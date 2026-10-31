//! The thread runtime — registry, queue dispatch, turn runner, retries,
//! liveness, and boot recovery. Supersedes CCW's `session.rs`.
//!
//! What changed from CCW, and why (design doc §10–§12):
//!
//! - **Caller-supplied thread ids** (Q2): `create_thread` adopts an existing
//!   claude session uuid and only mints when absent — AUI's 177 existing
//!   threads keep their conversations.
//! - **Full user text + metadata persisted at accept time** (Q3/G3/G18): the
//!   `UserMessage` ledger event is written the moment a send is accepted; the
//!   queue only owns delivery timing.
//! - **A real queue with two intents** (Q4/G7): CCW rejected a second turn
//!   mid-flight; here sends enqueue and the dispatcher drains — `immediate`
//!   when no parent turn is in flight, `turn_end` when the thread is fully
//!   idle (parent AND async subagents done).
//! - **Retries owned entirely, with persisted deadlines** (Q5/G10): every
//!   scheduled retry is a `retries` row re-armed on boot; the exponential
//!   backoff actually receives the attempt number (§9.4's dead-code fix);
//!   usage limits wait for the parsed reset and retry indefinitely (AUI's
//!   ladder); account failover honors the per-thread policy (§10).
//! - **Durable turn output + pid persistence** (G1): the child's stdout goes
//!   to an append-only file the daemon TAILS, so a daemon restart can re-adopt
//!   a still-running child by tailing from the persisted offset instead of
//!   orphaning it to die on EPIPE.
//! - **Liveness never touches the ledger** (G6): beats go down the firehose
//!   as a distinct transient frame.
//! - **Cancel is level-triggered** (G14): an `AtomicBool` checked every tail
//!   poll — a cancel can no longer be dropped between selects.
//! - **Compactions are bookkept** (§12): the `Compacted` event carries the
//!   pre-compaction message-id range, and the next turn's prompt restates
//!   working path / plugin paths / thread id / date-time.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::broadcast;

use shapewire::command::Visible;

use crate::accounts::{config_dir_of, AccountState, Placeable, PortOutcome, Registry, UsageProbe};
use crate::budgets::{
    Admission, AdmissionRequest, Budgets, Demand, ParkCause, PlacementRequest, Thresholds, Trip,
    TripAction,
};
use crate::config::Config;
use crate::context;
use crate::error::{Error, Result};
use crate::event::{Fact, InterruptTrigger, Push, SendIntent, StoredEvent, ThreadEvent};
use crate::gauge::{self, Axis};
use crate::plugins;
use crate::pool::PoolCell;
use crate::retry::{
    classify_errored_result, classify_message,
    RetryDecision, TransientKind,
};
use crate::settings::{AccountChoice, Failover, SettingsPatch, ThreadSettings};
use crate::spawn::{claude_bin, SpawnConfig, SpawnLogin};
use crate::store::{
    Claim, ParkedTurnRow, QueueRow, RetryRow, RunningTurnRow, Store, ThreadLockRow, ThreadRow,
};
use crate::update::{TurnPermit, UpdateGate};

// PL-2b: the read-only source census (`threads/{thread_id}/source_census`).
// A child module so it can reach this file's private lookups without editing
// any function here.
mod source_census;
// SH-A2: `threads/{thread_id}/attach` — lease a thread to an interactive TUI.
pub mod attach;

/// How long a turn must have WAITED before DEC-57's queued notice is worth a
/// thread's attention.
///
/// The decision the operator did not make and someone had to: every message
/// technically queues, so an unconditional notice would prefix a
/// `<thread_system_notice>` onto every ordinary turn and teach the model to
/// skim past the element entirely — at which point the interruption notice it
/// shares a tag with stops being read either. Thirty seconds is the smallest
/// gap where "he asked me this a while ago" changes what a good answer is,
/// which is the gap he described.
const QUEUED_NOTICE_MIN_SECS: i64 = 30;

/// **A QUEUED MESSAGE'S WHETHER-TO-SEND CHANGED WITHOUT AN ATTEMPT ENDING**
/// (R10.M2): `{message_ids, reason, dispatch_now}`. An operator cancel sets a
/// row that was only waiting `ride`; there is no attempt for a
/// `delivery_cancelled` to name, and §5.1 asks that every change of the
/// dispatch also be a ledger event. Written by [`ThreadManager::defer_to_ride`]
/// in the same transaction as the move.
pub const DISPATCH_CHANGED: &str = "cauldron.threads/dispatch_changed";

/// What [`ThreadManager::delivery_evidence`] read off the ledger (R13.1).
#[derive(Debug, Clone, Default)]
pub(crate) struct DeliveryEvidence {
    /// The ids of the batch the model demonstrably saw within this delivery,
    /// in batch order.
    pub(crate) delivered: Vec<u64>,
    /// How many `turn_started` within this delivery named any of the ids
    /// (the current attempt's own included, when asked from `run_once`).
    pub(crate) prior_starts: usize,
    /// The newest event that cut this delivery: `{kind, seq, reason}`.
    pub(crate) why: Option<Value>,
    /// How many attempts of this delivery wrote its ORIGINAL text to a child
    /// that came up (`system_init` in the attempt, no continue note), R13.2.
    pub(crate) prompt_writes: usize,
    /// R13.5 (inspect finding 3): the ids whose turn ENDED — a clean `result`
    /// after the turn that consumed them opened. A re-entry for these tells
    /// the model its reply was delivered and a LATER step was cut, not that
    /// its turn on them was interrupted (which invites a second answer).
    pub(crate) replied: Vec<u64>,
    /// R13.6 (R13.5-inspect-2 F-a): the ids whose turn was taken as the
    /// operator's ONLY because it spoke while ANNOUNCED, with no later
    /// un-announced turn that spoke. That turn may be a speaking pre-prompt
    /// mini-turn (the model has not seen the message) or his own turn with a
    /// notification before it (the model answered it): the stream cannot
    /// tell. A re-entry for these gets the UNSURE note — check, then answer
    /// or finish — never the replied note's "do not answer it again".
    pub(crate) unsure: Vec<u64>,
}

/// Live liveness state for a thread (ported from CCW).
///
/// **REPORTING ONLY: NO `busy()` READ DECIDES A WAIT** (R4.S2; wording
/// corrected R4.S2b). It IS read on timers — the 1 Hz heartbeat
/// (`emit_liveness`) and the gauge's busy-rate ticker (`accounts_with_work`) —
/// but only to report and to choose which accounts to measure; no loop exits,
/// continues or waits on it. `tests/managed_runtime.rs`'s
/// `no_busy_read_decides_a_wait_even_through_a_call` follows calls to check
/// that. `outstanding` is the
/// native (in-process) async agents the CURRENT turn's `claude` launched; it is
/// cleared by the CLI's own `task_notification`, or — the machine fact that
/// replaced the deleted mtime sweep — by that process exiting, since a native
/// agent cannot outlive the process it runs in. Whether a parent is still owed
/// a MANAGED child is not here at all: it is a fold over the parent's durable
/// ledger (`crate::managed`), which survives a restart.
#[derive(Debug, Default)]
struct Liveness {
    parent_running: bool,
    /// agentId → its live-transcript output file.
    outstanding: HashMap<String, Option<String>>,
    /// **EVERY IN-PROCESS (Task / Agent tool) SUBAGENT THIS THREAD LAUNCHED,
    /// keyed by the launching `tool_use_id`, with the daemon's own status for
    /// it** (R9.TS2). Fed by the same stream arms as `outstanding`, plus the
    /// launching call's `tool_result` and the process-exit edge, so a
    /// foreground Task — which `outstanding` never lists — is here too. The
    /// `threads` shape's `subagents` cell is its export.
    tasks: crate::shapes::threads::TaskRoster,
}

impl Liveness {
    fn busy(&self) -> bool {
        self.parent_running || !self.outstanding.is_empty()
    }
    fn running(&self) -> Vec<String> {
        let mut v: Vec<String> = self.outstanding.keys().cloned().collect();
        v.sort();
        v
    }
}

/// Per-thread runtime state.
pub(crate) struct ThreadState {
    id: String,
    /// LIVE settings — mutable mid-thread (Q9). Every spawn snapshots it.
    settings: StdMutex<ThreadSettings>,
    account: StdMutex<String>,
    config_dir: StdMutex<Option<String>>,
    seq: AtomicU64,
    next_message_id: AtomicU64,
    resumable: AtomicBool,
    turn_running: AtomicBool,
    live: StdMutex<Liveness>,
    /// Level-triggered cancel (G14) — checked on every tail poll.
    cancel_requested: AtomicBool,
    /// **THE OPEN ATTEMPT (R10.M2, L-8): the message ids a queue drain has
    /// marked attempted and whose `turn_started` has not been emitted yet.**
    /// Set by `maybe_dispatch` under the turn lock, taken by the dispatch task
    /// the instant before `run_turn` — or by [`ThreadManager::cancel_turn`],
    /// which then ends the attempt `delivery_cancelled{operator_cancel}` and
    /// sets its rows `ride` instead of letting the turn start. Whoever takes
    /// it owns the attempt; the other finds `None`.
    open_attempt: StdMutex<Option<Vec<u64>>>,
    /// TEST SEAM (R10.M2): when set, a queue drain's dispatch task waits on it
    /// between opening the attempt and taking it back — the window a real
    /// `edge_poll_open` spends awaiting `/usage`. Never set outside a test.
    attempt_hold: StdMutex<Option<Arc<tokio::sync::Notify>>>,
    /// Plugin mtime snapshot from the last spawn (hot-reload detection).
    plugin_snapshot: StdMutex<plugins::PluginSnapshot>,
    /// Restate the plugin paths in the next prompt (set on observed change).
    plugins_dirty: AtomicBool,
    /// Tokens already charged to the budget for the CURRENT turn from live
    /// `MessageUsage` lines, PER MODEL. The `ResultTurn` aggregate is
    /// authoritative, so the turn epilogue charges only the difference — mid-
    /// turn enforcement without double-charging. It is keyed by model because
    /// the weekly-fable meter only accepts fable-model usage (round-7 item 4),
    /// so a single scalar could not be reconciled correctly.
    turn_charged: StdMutex<HashMap<String, u64>>,
    /// Dollars provisionally charged to the budget for the CURRENT turn from
    /// live `MessageUsage` lines, PER MODEL — the priced ESTIMATE that lets a
    /// cash budget bite mid-turn. Reconciled against the authoritative
    /// `costUSD` in the turn epilogue, exactly like `turn_charged`.
    turn_cost_charged: StdMutex<HashMap<String, f64>>,
    /// Anthropic `message.id`s whose usage has already been charged this turn.
    /// Claude repeats a message's `usage` payload on every content-block line
    /// of that message, so without this the meter reads ~2× high.
    charged_message_ids: StdMutex<HashSet<String>>,
    /// Set when a budgets-module failure has already been reported for the
    /// CURRENT turn, so a broken module cannot flood the ledger.
    budgets_failure_reported: AtomicBool,
    /// Set when a budget trip has ordered this turn's execution torn down
    /// (DEC-13). Level-triggered, like cancel.
    budget_kill: AtomicBool,
    /// The trip that ordered the kill (for the ledger + the park reason).
    kill_trip: StdMutex<Option<Trip>>,
    /// **Set when this thread's ACCOUNT was barred while this turn was still
    /// running** (`ThreadSettings::limit_kill_mid_turn`). Level-triggered, like
    /// `cancel_requested` and `budget_kill`, and read by the same tail loop.
    ///
    /// Distinct from `budget_kill` in where it LANDS, which is the whole point:
    /// a budget kill parks the prompt (`park_killed_turn`) and waits for the
    /// meter, while this one routes into `run_turn`'s existing 429 branch —
    /// `note_account_limit` → `admit_or_park` → `interrupt_thread` — so the
    /// thread is handed back to the balancer and may be running again on
    /// another account inside a second. It re-uses that branch verbatim rather
    /// than growing a second one: `interrupt_thread` still contains no
    /// placement logic (DEC-56 as amended).
    limit_kill: AtomicBool,
    /// **STOP THIS TURN: it has just been through an automatic compaction and
    /// this thread is not permitted to resume through one**
    /// (`ThreadSettings::resume_after_compaction`, false by default).
    ///
    /// Raised by `interpret`'s `Compacted` arm — the ONE place in the crate that
    /// knows a compaction happened — and consumed by the tail loop in
    /// `run_once`, which tears the execution down exactly as the three teardown
    /// flags beside it do. It is a flag rather than a direct signal from
    /// `interpret` for the same reason `budget_kill` and `limit_kill` are: the
    /// interpreter runs inside the line-reading loop and must not block,
    /// signal, or decide; it observes, and the supervisor acts.
    ///
    /// Unlike its three neighbours this one is not a fault and not a refusal.
    /// It ends the delivery (`retry::ends_the_delivery`) instead of laddering
    /// it, and it lands on the ledger as `TurnInterrupted` with trigger
    /// `compaction`.
    compaction_stop: AtomicBool,
    /// What was known at the moment the interrupt was ordered. Without it the
    /// interrupted turn would reach the 429 branch with no reset prose and no
    /// quota — and `note_account_limit` writes NOTHING when it cannot parse a
    /// reset, so the bar the poller just observed would be dropped on the floor
    /// by the very path meant to act on it.
    limit_kill_signal: StdMutex<Option<LimitKill>>,
    /// The last park reason emitted, so a parked thread reports once instead
    /// of once per resume tick.
    park_reason: StdMutex<Option<String>>,
    /// **A REFUSED PROJECTION IS NOT RE-ASKED UNTIL SOMETHING IT DEPENDS ON
    /// MOVES** (R4.S4a inspect finding 1). `(fingerprint, when)` of the
    /// refusal: the projection inputs `refuse_projection` saw (the switch, the
    /// allowed losses, the account, the ledger file's length and mtime). While
    /// the fingerprint is unchanged and `Tunables::projection_hold` has not
    /// elapsed, `maybe_dispatch` does not redeliver the parked turn. Without it
    /// a deterministic refusal (`NativeWithoutLedger`, which every unmigrated
    /// thread meets if flipped before the cutover) was redelivered by the
    /// refusal's own `finish_turn` → `maybe_dispatch`, synchronously, forever:
    /// ~82% of a core per thread and two events per lap.
    ///
    /// **The login refusal arms the same hold** (R4.S4b, HOLDS R17), with a
    /// `login|`-prefixed fingerprint of its own inputs
    /// (`ThreadManager::login_fingerprint`): one parked row per thread, so one
    /// hold, recomputed by whichever refusal armed it.
    projection_hold: StdMutex<Option<(String, std::time::Instant)>>,
    /// **THE CURRENT TURN'S OWN TERMINAL RECORD** (H179,
    /// `docs/SUBAGENT-WAKE.md`): every event `emit_resolving` writes is folded
    /// in as it is written, and `finish_turn` takes it whole and hands it to
    /// `managed::turn_finished`. That is how a managed child's completion
    /// reaches its parent as an event — the turn says how it ended — instead of
    /// being inferred by scanning the child's log afterwards.
    turn_terminal: StdMutex<crate::managed::TurnTerminal>,
}

/// Tunables (env-overridable so tests run fast).
#[derive(Debug, Clone)]
struct Tunables {
    heartbeat: Duration,
    tail_poll: Duration,
    /// **THE BACKSTOP'S PERIOD, AND IT IS NO LONGER THE MECHANISM.**
    ///
    /// Placement re-runs on EVENTS (see [`ThreadManager::placement_wake`]); this
    /// is the bounded fallback that catches an event set which turns out to be
    /// incomplete, because a thread silently parked forever is worse than a
    /// cheap poll. `THREADS_RESUME_TICK_MS=0` switches the fallback OFF
    /// entirely, leaving placement purely event-driven — which is how the
    /// headline test proves the events do the work rather than the clock.
    resume_tick: Duration,
    /// How long a torn-down execution gets between SIGTERM and SIGKILL.
    kill_grace: Duration,
    /// Gap between the two polls of an OPENING edge pair (§3.2). Two polls a
    /// few seconds apart is what gives `IdleMovement` something to compare.
    edge_pair_gap: Duration,
    /// **HOW LONG A TURN WAITS ON ITS OPENING `/usage` PAIR** (R10.M2c,
    /// TC-45). The pair runs concurrently with this wait; past it the turn
    /// starts anyway and the pair is abandoned, so a slow or timing-out probe
    /// (prod: 20-60 s) never holds a message back from the model. 5 s covers
    /// a healthy pair's own 3 s gap plus a quick probe; a reading that would
    /// land later is discarded, never recorded inside the window.
    edge_open_bound: Duration,
    /// **HOW OFTEN THE ROSTER IS RE-READ FROM DISK** — the noticer's second leg.
    ///
    /// Its own period, and deliberately not `resume_tick`: the roster moves when
    /// a human runs `claude-accounts` or `threads accounts adopt` in a terminal,
    /// which is minutes-scale, and two file reads every five seconds forever
    /// would be paying placement-latency prices for human-latency news.
    ///
    /// It used to ride the GAUGE tick (~120 s), and that is the defect this
    /// closes: the gauge task is spawned only when a gauge cadence exists, so on
    /// a daemon with `probe_interval_secs = 0` and `probe_interval_busy_secs = 0`
    /// — a supported configuration — the roster was never re-read at all and a
    /// new account was invisible until a restart. The roster is an input to
    /// PLACEMENT, not to the gauge, and it is now noticed on placement's terms.
    roster_reload: Duration,
    /// How long a refused projection waits before it is re-asked with
    /// UNCHANGED inputs (an `Io` refusal can heal by itself; a typed one
    /// cannot). Any change to its inputs re-asks at once. See
    /// `ThreadState::projection_hold`.
    projection_hold: Duration,
    /// **P6's period** ([`CREDENTIAL_SAMPLE_PERIOD`], five minutes).
    /// `THREADS_CREDENTIAL_SAMPLE_MS` shrinks it for tests, so a test that says
    /// "after one sample" outlasts a real period rather than a guess (A-1).
    credential_sample: Duration,
    /// **HOW LONG A RUNNING CHILD MAY SAY NOTHING BEFORE IT IS CALLED HUNG**
    /// (R9.L6, GAP-AUDIT L-6). `THREADS_OUTPUT_STALL_MS`, default 20 minutes;
    /// `0` switches the check off.
    ///
    /// The tail loop knows how far the child has written and, until this, never
    /// knew WHEN — so a child that wrote its init line and then hung read as an
    /// active turn forever and everything queued behind it. Past this much
    /// silence the child is torn down and the turn goes on the retry ladder.
    ///
    /// Why twenty minutes: the longest silences a HEALTHY child produces on its
    /// stream are a single tool call (claude's Bash tool caps itself at 10
    /// minutes) and one long generation that is not streamed until complete
    /// (several minutes at the top output budgets). Twice the longest known
    /// legitimate silence keeps a false teardown — which re-buys the turn's
    /// work — rarer than the hang it is guarding against. Native async agents
    /// are the one quiet-but-alive case longer than that, and they are not
    /// measured by this bound at all: the clock is held while any is
    /// outstanding (see the tail loop) — up to [`Tunables::async_hold`].
    output_stall: Duration,
    /// **HOW LONG AN OUTSTANDING ASYNC AGENT MAY HOLD THE STALL CHECK**
    /// (R9.L6b, ledger R9.L6-1). `THREADS_ASYNC_HOLD_MS`; the default is
    /// [`ASYNC_HOLD_FACTOR`] × `output_stall` (two hours at the 20-minute
    /// default); `0` removes the bound (the pre-R9.L6b unbounded hold).
    ///
    /// While a native async agent is outstanding the parent may be silent for
    /// as long as the agent works, so the ordinary bound is held. But
    /// `outstanding` is cleared only by the CLI's own `task_notification` (or
    /// the process exiting), so a notification that never arrives held the
    /// check FOREVER — a hung parent with a stale entry was never torn down.
    /// Past this much continuous parent silence the hold gives way and the
    /// child is torn down like any stalled one, with the agent ids still
    /// outstanding named in the log and on the ledger.
    async_hold: Duration,
    /// **HOW LONG `run_once` WAITS FOR ITS STDERR READER BEFORE CLASSIFYING**
    /// (R9.E1, finding R9.F2-3). See [`STDERR_DRAIN_BOUND`].
    stderr_drain: Duration,
}

/// **THE BOUND ON JOINING THE STDERR READER** (R9.E1 / R9.F2-3).
///
/// A non-zero exit with no structured cause is classified from stderr, and the
/// stderr reader is a separate task. It used to be read without joining that
/// task, so under load the reader had not run yet, the buffer was empty, and a
/// credit-balance or expired-login exit became an anonymous `claude_error` —
/// the account was not barred and placement kept sending it turns.
///
/// Stderr reaches EOF when the child exits, so the join normally returns at
/// once. The bound exists for a DESCENDANT that inherited stderr and outlives
/// the child: without it, that descendant would hold the turn open for as long
/// as it lives. Past the bound the turn is classified from what was read, and
/// the truncation is logged. `THREADS_STDERR_DRAIN_MS` overrides it.
pub const STDERR_DRAIN_BOUND: Duration = Duration::from_secs(5);

/// **THE ASYNC HOLD, AS A MULTIPLE OF THE STALL BOUND** (R9.L6b).
///
/// Why six: a native async agent running for an hour is ordinary (the
/// premise of the hold, ledger R9.L6-1); six stall bounds (two hours at the
/// default) is twice that, so a
/// false teardown of a working agent — which re-buys the whole turn — stays
/// rarer than the hang it guards. Derived from the stall bound rather than
/// fixed so a test that shrinks the one shrinks the other in proportion.
///
/// **WHO IS TORN DOWN AT THE HOLD (R9.L6d, H179-3 decision, kept).** The hold
/// measures PARENT-stream silence only. A live agent is safe as long as the
/// CLI's `system/task_progress` heartbeat for it reaches the parent's stream
/// (it does for a running task; any byte resets the clock). An agent that is
/// alive but puts NOTHING on the parent's stream for the whole hold — one
/// tool call longer than two hours with no progress line — IS torn down, and
/// the turn is retried. The agent's own `output_file` (kept in `outstanding`)
/// is not consulted. Both regimes are pinned in `tests/output_stall.rs`
/// (`…heartbeating_on_the_parent_stream…` and `…writing_only_its_own_file…`).
pub const ASYNC_HOLD_FACTOR: u64 = 6;

fn env_ms(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

impl Default for Tunables {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_millis(env_ms("THREADS_HEARTBEAT_MS", 1000)),
            tail_poll: Duration::from_millis(env_ms("THREADS_TAIL_POLL_MS", 150)),
            resume_tick: Duration::from_millis(env_ms("THREADS_RESUME_TICK_MS", 5_000)),
            kill_grace: Duration::from_millis(env_ms("THREADS_KILL_GRACE_MS", 3_000)),
            edge_pair_gap: Duration::from_millis(env_ms("THREADS_EDGE_PAIR_GAP_MS", 3_000)),
            edge_open_bound: Duration::from_millis(env_ms("THREADS_EDGE_OPEN_BOUND_MS", 5_000)),
            roster_reload: Duration::from_millis(env_ms("THREADS_ROSTER_RELOAD_MS", 30_000)),
            projection_hold: Duration::from_millis(env_ms("THREADS_PROJECTION_HOLD_MS", 30_000)),
            credential_sample: Duration::from_millis(env_ms(
                "THREADS_CREDENTIAL_SAMPLE_MS",
                CREDENTIAL_SAMPLE_PERIOD.as_millis() as u64,
            )),
            output_stall: Duration::from_millis(env_ms("THREADS_OUTPUT_STALL_MS", 20 * 60 * 1000)),
            async_hold: Duration::from_millis(env_ms(
                "THREADS_ASYNC_HOLD_MS",
                ASYNC_HOLD_FACTOR * env_ms("THREADS_OUTPUT_STALL_MS", 20 * 60 * 1000),
            )),
            stderr_drain: Duration::from_millis(env_ms(
                "THREADS_STDERR_DRAIN_MS",
                STDERR_DRAIN_BOUND.as_millis() as u64,
            )),
        }
    }
}

/// **WHAT WOKE ONE PLACEMENT PASS.** Three sources, and the distinctions between
/// them are load-bearing in two different directions.
///
/// It replaces a `by_event: bool`, which could only say "the mechanism" or "the
/// fallback". The pool's backoff is neither: it is a mechanism of its own, and
/// collapsing it into either of the other two would destroy a signal.
/// [`Self::Cadence`] folded into [`Self::Event`] would let the pool's own clock
/// masquerade as a world change; folded into [`Self::Backstop`] it would make
/// `backstop_placed` fire on healthy operation, which is the one thing the
/// instrumented backstop may not be allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassOrigin {
    /// **AN INPUT TO PLACEMENT MOVED.** Fourteen-plus reasons ring this bell. A
    /// pass woken this way asks every parked prompt a QUESTION.
    Event,
    /// **THE POOL'S OWN BACKOFF CAME DUE** ([`PoolCadence`]). The only origin
    /// that is a DEADLINE: it may re-run a prompt on the very account that
    /// refused it, and a refusal it collects hands the prompt to `parked_turns`.
    Cadence,
    /// **NOTHING HAPPENED AND THE POLL FIRED ANYWAY** — the instrumented
    /// fallback. It reconsiders everything exactly as an event does, and its
    /// retry leg is a QUESTION, never a deadline: if the backstop were allowed to
    /// be the deadline the cadence would be decorative.
    Backstop,
}

impl PassOrigin {
    /// Is this pass entitled to treat a parked prompt as a DEADLINE?
    ///
    /// Exactly one origin is, and this is the whole of the deadline-versus-question
    /// distinction after the per-thread floor was removed. It used to be
    /// `retry_floor_elapsed(row)` — arithmetic on a per-thread timestamp. It is now
    /// a property of WHY THE POOL WOKE, which is the operator's model: *"if any
    /// thread is parked then the pool needs a retry, not the thread."*
    pub fn is_deadline(self) -> bool {
        matches!(self, PassOrigin::Cadence)
    }
}

/// **THE POOL'S BACKOFF. ONE CADENCE FOR EVERY PARKED PROMPT.**
///
/// This is where the retry ladder lives now, and the operator's ruling is the
/// whole reason it exists here rather than on each row:
///
/// > *"if any thread is parked then the pool needs a retry, not the thread. Any
/// > parked threads get checked at any opportunity."*
///
/// # It is a LADDER, and TWO things advance it
///
/// The rung is `min` over everything parked of
/// `retry::ladder_secs(kind, attempt + barren)`:
///
/// * **`attempt`** is the row's own spent spawns, unchanged in meaning and still
///   per-thread. It advances only when a turn actually RAN and was refused, which
///   is the invariant that keeps a refused ask free.
/// * **`barren`** is the POOL's consecutive cadence firings that came to nothing.
///   It exists because `attempt` cannot advance when the gate keeps refusing, and
///   something has to: without it a parked `Overload` row sits at
///   `transient_backoff_secs(1) == 0` forever and the cadence is a spin loop.
///   Every cadence firing increments it; anything the pool actually places, and
///   any retry that reaches `run_turn`, resets it to zero.
///
/// **The numbers are `retry.rs`'s, untouched.** The ladder was RE-HOMED, not
/// retuned: the same six per-class functions, read through the same
/// [`crate::retry::ladder_secs`], at an index that is now a sum.
///
/// # Why `min` and not `max`
///
/// Because a pool cadence is a promise to everything in it. Taking the slowest
/// rung would make a connectivity blip (45s ceiling) wait out a login expiry
/// (600s ceiling) parked beside it. Taking the fastest means no prompt waits
/// longer than its own class's policy said it should — the cadence is never
/// slower than the slowest thing it serves is willing to tolerate.
///
/// # An empty pool has no cadence at all
///
/// `rung` is `None` when `retries` is empty: there is no timer, nothing to fire,
/// and nothing to compute. That is the pool-level analogue of "a thread with no
/// retry has no floor", and it is why this adds no steady-state cost.
#[derive(Debug, Default)]
pub struct PoolCadence {
    /// Consecutive cadence firings after which the pool was no better off.
    barren: AtomicU32,
}

impl PoolCadence {
    /// The pool made progress — reset the ladder to its first rung.
    fn progressed(&self) {
        self.barren.store(0, Ordering::SeqCst);
    }

    /// A cadence fired and nothing came of it — climb one rung.
    ///
    /// Saturating, so a daemon left refusing for a week cannot wrap the index
    /// back onto a fast rung. Every ladder in `retry.rs` is capped, so climbing
    /// forever simply parks the cadence on the shortest ceiling among the classes
    /// actually waiting.
    fn barren_tick(&self) {
        let _ = self
            .barren
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                Some(n.saturating_add(1))
            });
    }

    fn barren(&self) -> u32 {
        self.barren.load(Ordering::SeqCst)
    }
}

/// **WHAT WOKE PLACEMENT, COUNTED** — the whole answer to "is the event set
/// complete, or is the backstop quietly carrying it?".
///
/// The operator's model is that placement re-runs when the world changes, on
/// events, not on a clock. A backstop poll makes that model survivable when the
/// event set is incomplete — but a backstop that fires constantly also HIDES a
/// missing event, which is exactly how `resume_sweep` came to be on no event at
/// all. So the backstop is instrumented rather than trusted:
/// `backstop_placed` counting above zero names a defect, in the log, at `error`.
///
/// **THE POOL CADENCE DID NOT TOUCH THAT MEANING, AND THAT IS DELIBERATE.**
/// `backstop_placed` is scoped to [`ThreadManager::resume_sweep_placing`] — the
/// PARKED half — and the cadence is a THIRD, separately counted wake source that
/// never increments it. So "a backstop pass placed a parked thread" still means
/// exactly "an input to placement moved with no bell to ring", and the cadence
/// cannot quietly become the normal path for parked work without
/// `backstop_placed` staying at zero while `by_cadence` climbs, which is visible.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PlacementPasses {
    /// Every placement pass that ran, however it was woken.
    pub total: u64,
    /// Passes woken by an EVENT — the mechanism.
    pub by_event: u64,
    /// Passes woken by the BACKSTOP timer — the fallback.
    pub by_backstop: u64,
    /// Passes woken by the POOL'S OWN CADENCE — [`PoolCadence`]. Not a fallback
    /// and not a defect: this is the mechanism that replaced N per-thread retry
    /// floors, and it is the only origin entitled to treat a parked prompt as a
    /// DEADLINE rather than as a question.
    pub by_cadence: u64,
    /// **BACKSTOP PASSES THAT ACTUALLY PLACED A PARKED THREAD.** Each one is an
    /// input to placement that moved with no bell to ring. Zero is the only
    /// value that says the event set is complete.
    pub backstop_placed: u64,
    /// Parked / queued threads dispatched by placement passes, all origins.
    pub placed: u64,
    /// **PARKED PROMPTS RECONSIDERED, SUMMED OVER EVERY PASS** — the scoreboard
    /// for *"any parked threads get checked at any opportunity"*.
    ///
    /// It replaces `floor_superseded`, which counted the rows an event-driven pass
    /// re-asked that their own `retry_at` would have deferred. **That number is
    /// now structurally equal to this one**, because there is no `retry_at` left
    /// to defer anything: every pass, of every origin, reconsiders every row. The
    /// field was renamed rather than kept as an alias, because a counter named
    /// after a mechanism that no longer exists is the `threads.status` mistake —
    /// a second source of truth that only looks like an answer.
    ///
    /// Zero while `retries` is non-empty and passes are happening is the defect it
    /// detects: something has put a filter back on the candidate set.
    pub reconsidered: u64,
    /// **THE POOL'S CURRENT CADENCE, IN SECONDS** — `0` when nothing is parked and
    /// no cadence is armed at all. See [`PoolCadence`]; this is the one number
    /// that replaces N per-thread floors.
    pub cadence_secs: u64,
    /// **CONSECUTIVE CADENCE FIRINGS THAT CAME TO NOTHING** — the pool's own
    /// attempt counter, and the thing that walks the cadence up its ladder when
    /// no thread's `attempt` can (a refusal never spends a thread's ladder). This
    /// is what bounds a hot loop against a gate that keeps refusing.
    pub barren: u32,
}

/// The live counters behind [`PlacementPasses`], plus the reasons the most
/// recent pass was woken by (drained from `wake_pending` as the pass starts).
#[derive(Debug, Default)]
struct PlacementLedger {
    total: AtomicU64,
    by_event: AtomicU64,
    by_backstop: AtomicU64,
    by_cadence: AtomicU64,
    backstop_placed: AtomicU64,
    placed: AtomicU64,
    reconsidered: AtomicU64,
    /// The reason set the LAST pass consumed. Bounded by the event set's size.
    last_reasons: StdMutex<std::collections::BTreeSet<String>>,
    /// Thread ids the LAST pass dispatched. Bounded by the parked set.
    last_placed: StdMutex<Vec<String>>,
}

/// Per-account edge-poll bookkeeping (account-selection-design §3.2).
#[derive(Debug, Default, Clone)]
struct EdgeState {
    /// `/usage` polls recorded on this account since the current idle gap began.
    /// Two is what a bracket plus an `IdleMovement` witness needs, so an opening
    /// edge only costs a poll when the gap is short of that.
    polls_since_idle: usize,
    /// A closing settle ladder is walking this account right now.
    ladder_running: bool,
    /// The rungs the ladder currently walking has recorded.
    rungs: Vec<gauge::SettleReading>,
    /// The last completed settle profile — the MEASURED settle delay, or the
    /// statement that the ladder was too short to see one.
    profile: Option<gauge::SettleProfile>,
}

/// The settle ladder as durations, from `config.settle_ladder_secs`. Sorted,
/// non-positive rungs dropped. Per-manager, never from the process environment:
/// two daemons (or two tests) in one process must be able to walk different
/// ladders.
fn settle_ladder_of(config: &Config) -> Vec<Duration> {
    let mut l: Vec<Duration> = config
        .settle_ladder_secs
        .iter()
        .copied()
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(Duration::from_secs_f64)
        .collect();
    l.sort();
    l
}

/// How a budget kill tears an execution down (DEC-13 / DIR-19 / D-8).
///
/// The operator asked for strategies to be TRIED (M-9), so this is a seam,
/// not a hard-coded behavior: pick it with `THREADS_TEARDOWN` or
/// `teardown_strategy` in `config.toml`, and the chosen strategy is recorded
/// on every `TurnKilled` event so the ledger alone can compare them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeardownStrategy {
    /// DEFAULT. Every turn's child is spawned as the leader of its OWN
    /// process group, so `killpg` reaches claude AND everything claude
    /// spawned (Bash tool children, MCP helpers, nested clis) in one signal:
    /// SIGTERM, a grace window, then SIGKILL. This is the strongest teardown
    /// reachable without a per-child supervisor.
    ProcessGroup,
    /// Signal ONLY the claude process. Anything it spawned is reparented to
    /// init and keeps running — this is the strategy that ORPHANS children,
    /// kept as the comparison arm for M-9.
    ChildOnly,
}

impl TeardownStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            TeardownStrategy::ProcessGroup => "process_group",
            TeardownStrategy::ChildOnly => "child_only",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "process_group" | "pgroup" => Some(TeardownStrategy::ProcessGroup),
            "child_only" | "child" => Some(TeardownStrategy::ChildOnly),
            _ => None,
        }
    }
    fn from_env_or(config: &Config) -> Self {
        std::env::var("THREADS_TEARDOWN")
            .ok()
            .as_deref()
            .and_then(Self::parse)
            .or_else(|| config.teardown_strategy.as_deref().and_then(Self::parse))
            .unwrap_or(TeardownStrategy::ProcessGroup)
    }
}

/// Send `sig` to a turn's child. With [`TeardownStrategy::ProcessGroup`] the
/// child is its own group leader (`pgid == pid`), so `killpg` sweeps every
/// descendant; with `ChildOnly` only the claude process is signalled.
fn signal_turn(pid: i64, strategy: TeardownStrategy, sig: libc::c_int) {
    if pid <= 0 {
        return;
    }
    unsafe {
        match strategy {
            TeardownStrategy::ProcessGroup => {
                libc::killpg(pid as libc::pid_t, sig);
            }
            TeardownStrategy::ChildOnly => {
                libc::kill(pid as libc::pid_t, sig);
            }
        }
    }
}

/// Is `pid` a live process? (`kill(pid, 0)` — EPERM still means alive.)
///
/// A ZOMBIE IS NOT ALIVE, and saying otherwise strands a thread forever.
/// `kill(pid, 0)` succeeds on a zombie: the process has exited, but its entry
/// stays in the table until its parent reaps it, and a parent that never waits
/// leaves it there indefinitely. That is not a rare shape here — it is exactly
/// what a daemon that died mid-restart leaves behind.
///
/// Live case, 2026-08-03: the Minecraft thread's `thread_locks` row named pid
/// 33517, a daemon that had exited hours earlier and was still a zombie under
/// an unreaping parent. Both reclaim paths — `lock_turn`'s and `recover()`'s —
/// read that as "another LIVE daemon holds this thread's lock", declined to
/// steal it, and refused every dispatch. The thread could not run again, ever,
/// no matter what was queued on it. `kill(pid, 0)` alone cannot tell a working
/// daemon from a corpse, so it is asked the second question too.
pub(crate) fn pid_alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    let exists = r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    exists && !pid_is_zombie(pid)
}

/// Has `pid` exited and simply not been reaped yet?
///
/// Only ever consulted for a pid that already answered `kill(pid, 0)`. When the
/// answer cannot be determined it returns `false` — "not provably a corpse" —
/// so an unreadable process table can never cost us a lock we should have left
/// alone. Being wrong in that direction leaves a thread stuck until the next
/// boot; being wrong the other way could steal a lock from a working daemon and
/// run a thread twice, which is the far worse harm.
/// macOS answers this in two different ways depending on how far gone the
/// process is, and both have to be read:
///
/// - a process the kernel can still describe reports `pbi_status`, and `SZOMB`
///   is the plain answer;
/// - a process already stripped to a bare exit-status slot cannot be described
///   at all — `proc_pidinfo` fails with `ESRCH` even though `kill(pid, 0)`
///   still succeeds. Measured against the real corpse from the live incident,
///   pid 33517: `kill` said yes, `proc_pidinfo` said `ESRCH`.
///
/// `EPERM` is deliberately NOT in that set. It means the pid belongs to another
/// user and we are not allowed to look — a live process we cannot inspect, and
/// treating it as a corpse could steal a lock from a working daemon and run a
/// thread twice. Anything that is not positive evidence of death reads as
/// alive.
#[cfg(target_os = "macos")]
fn pid_is_zombie(pid: i64) -> bool {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n == size {
        return info.pbi_status == libc::SZOMB;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(target_os = "linux")]
fn pid_is_zombie(pid: i64) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `pid (comm) STATE ...` — `comm` may itself contain spaces and parens, so
    // the state is the first field after the LAST ')'.
    let Some((_, tail)) = stat.rsplit_once(')') else {
        return false;
    };
    tail.split_whitespace().next() == Some("Z")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn pid_is_zombie(_pid: i64) -> bool {
    false
}

// ── THE THREAD EXECUTION LOCK (operator msg #1213) ───────────────────────
//
// "You need a locking mechanism inside of threads. And the lock needs to be
//  built in. It is impossible to run the same thread two times. Impossible."
//
// The lock is TWO gates that must BOTH be passed, and one type — [`TurnLock`]
// — that is the only thing an execution can be started while holding:
//
//  1. `ThreadState::turn_running`, an `AtomicBool` compare-and-swap. Settles
//     races between concurrent tasks INSIDE one daemon, in nanoseconds, with
//     no I/O.
//  2. A row in the `thread_locks` table, keyed by `thread_id` PRIMARY KEY.
//     Settles everything else: a second daemon on the same store, a daemon
//     restarted while a child still runs, any future CLI verb. SQLite refuses
//     the second INSERT — this is not a check the caller could forget.
//
// Every path that starts an execution goes through [`ThreadManager::lock_turn`]
// and holds the returned [`TurnLock`] for the whole execution. There is no
// other way in: `run_turn` and `adopt_turn` REQUIRE the guard by value, so a
// path that forgot to lock does not compile.
//
// RECLAIM RULE (a crash must not deadlock a thread forever): a claim whose
// `daemon_pid` is DEAD is stolen with a boot_id-guarded compare-and-swap; a
// claim whose `daemon_pid` is ALIVE is never stolen — the second claimant
// refuses and the work stays queued. A pid that has been RECYCLED by this very
// process (`daemon_pid == our pid` under a different `boot_id`) counts as dead,
// because it is provably not the old daemon.

/// Why a lock attempt failed — for the log line and the refusal test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockRefusal {
    /// Another task in THIS daemon is already executing this thread.
    InProcess,
    /// A LIVE holder (another daemon process) owns the claim.
    HeldBy(ThreadLockRow),
    /// The store refused to answer. Fail CLOSED — an unverifiable lock is not
    /// a lock, and running anyway is exactly the duplicate the directive bans.
    StoreError(String),
    /// ADMISSION IS CLOSED: a restart window is open (intent item 64). The work
    /// stays exactly where it is — queue rows are still `waiting`, parked turns
    /// are still parked, retries are still armed — and the daemon that comes
    /// back in a moment re-dispatches all of it from `recover()`.
    RestartPending,
}

impl LockRefusal {
    pub fn as_str(&self) -> &'static str {
        match self {
            LockRefusal::InProcess => "in_process",
            LockRefusal::HeldBy(_) => "held_by_live_daemon",
            LockRefusal::StoreError(_) => "store_error",
            LockRefusal::RestartPending => "restart_pending",
        }
    }
}

/// A HELD thread execution lock. Constructing one is the only way to start an
/// execution; dropping one releases the thread.
///
/// Release order is deliberate: the durable row goes first, the in-process
/// `turn_running` flag second. The other order would leave a window in which a
/// dispatcher wins the atomic and then trips over our own not-yet-deleted row.
///
/// `Drop` is the backstop, not the mechanism — every normal path calls
/// [`TurnLock::release`] explicitly at the top of `finish_turn`, before the
/// queue is re-dispatched. But a panicking turn task must not strand a thread,
/// and `Drop` is what makes that structurally impossible.
pub struct TurnLock {
    mgr: ThreadManager,
    state: Arc<ThreadState>,
    released: AtomicBool,
    /// THE UPDATE-GATE ADMISSION PERMIT (intent item 64). Taken before either
    /// of the two gates below, because this is the one door every execution
    /// enters through and therefore the only place "nothing is being started"
    /// can be made true. Handed back in `release`, not merely at drop.
    ///
    /// **It is no longer what a restart waits for.** It counts
    /// `update::Blockers::turns`, which is REPORTED on every ledger line and is
    /// not in `Blockers::quiet` — a running turn survives an `execv` and is
    /// re-adopted at boot, and waiting for all of them to end at once starved a
    /// restart for 266 minutes. What a restart waits for is
    /// `update::SpawnPermit`, held by `run_once` across the spawn window alone.
    /// The two dispatch gates below and the durable `thread_locks` row are a
    /// DIFFERENT invariant (one execution per thread) and were not touched by
    /// that change.
    permit: StdMutex<Option<TurnPermit>>,
}

impl TurnLock {
    pub fn thread_id(&self) -> &str {
        &self.state.id
    }

    /// Release the thread. Idempotent.
    pub fn release(&self) {
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        // Give the update gate its permit back FIRST: from here on this
        // execution can no longer be a reason to defer a restart.
        drop(self.permit.lock().unwrap_or_else(|e| e.into_inner()).take());
        match self
            .mgr
            .store
            .release_thread_lock(&self.state.id, &self.mgr.boot_id)
        {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                thread = %self.state.id,
                boot = %self.mgr.boot_id,
                "released a thread lock we no longer owned — somebody reclaimed it"
            ),
            Err(e) => tracing::error!(
                thread = %self.state.id, error = %e,
                "RELEASING THE THREAD LOCK FAILED — the row is stale until this daemon exits"
            ),
        }
        self.state.turn_running.store(false, Ordering::SeqCst);
        self.mgr.emit_status(&self.state.id);
    }
}

impl Drop for TurnLock {
    fn drop(&mut self) {
        if !self.released.load(Ordering::SeqCst) {
            tracing::warn!(
                thread = %self.state.id,
                "thread lock released by Drop — a turn path exited without releasing"
            );
        }
        self.release();
    }
}

/// **How far out a parsed reset instant is still believed.**
///
/// It was THIRTY HOURS, and that number was sized for a session window while
/// the daemon was handed weekly ones. Measured live on 2026-08-06: the CLI said
/// *"You've hit your weekly limit · resets Aug 8 at 1pm (America/Chicago)"* —
/// about forty-six hours out — so every parsed reset was discarded as
/// out-of-range and `compute_retry_at` fell through to
/// `usage_limit_backoff_secs`, a flat five minutes. All 851 retry rows that day
/// carried `now + 300s` while `accounts.limit_until` held the true instant
/// correctly, two columns apart. The floor was not merely short; it was a
/// different quantity entirely.
///
/// Eight days covers the longest window that exists (the seven-day weekly pool)
/// with a day of slack for a clock skew or an unusual phrasing, and still
/// refuses prose that resolves to next month.
///
/// Widening it was safe because a long reset never delayed a thread that had
/// somewhere else to go, and it is now safe for a simpler reason: this bound is
/// read in exactly one place, `note_account_limit_for`'s `limit_until`, and that is
/// an ACCOUNT BAR. No per-thread retry instant is derived from a reset any more
/// (the floor it used to write is gone), so a long window stops the daemon
/// hammering one account and delays nothing else at all.
const MAX_PARSED_RESET: chrono::Duration = chrono::Duration::days(8);

/// `limit_axis` of a bar written because a `rate_limit_event` reported paid
/// overage (A-4). A completed turn does not clear it — see
/// `ThreadManager::note_rate_limit_push`.
pub const OVERAGE_AXIS: &str = "overage";

/// **WHERE `RetryOrigin` WAS** — a two-variant enum, `Timer` and `Sweep`, which
/// existed so the periodic sweep could not breed per-thread timers behind itself.
/// There are no per-thread timers left to breed. [`ThreadManager::fire_retry`]
/// takes a [`PassOrigin`] instead, which says what woke the POOL, and that is the
/// only distinction the retry path draws.
///
/// **THE BOUND ON ONE `accounts` HYDRATE'S `threads` CELL.**
///
/// `docs/shapes.md` 4.3 makes an honest, bounded read a producer's obligation at
/// hydrate, and [`ThreadManager::thread_rows_non_idle`] had neither: it scanned
/// every thread that exists and then re-read each survivor one at a time. The
/// scan is gone (the candidate set replaces it — see that function). This is
/// what is left, and it protects two different things from the one number:
///
/// 1. **The export.** Every row here becomes an entry in `threads/non-idle`'s
///    one roster (it was the `accounts` shape's `threads` cell, which rode
///    EVERY export of EVERY account key, until R2.S8). An
///    unbounded activity table is an unbounded frame on the wire, for every
///    subscriber, on every re-subscribe — and re-subscribe is load-bearing
///    (`docs/shapes.md` 2.5), so it is the path that must not be able to blow up.
/// 2. **The one statement.** [`crate::store::Store::last_seqs`] binds one SQLite
///    parameter per id, and the most conservative build of SQLite caps that at
///    999 (`SQLITE_MAX_VARIABLE_NUMBER`; modern builds allow 32,766, but the
///    bound must not depend on which one is linked).
///
/// **Why 512.** It is the largest power of two that leaves headroom under the
/// 999-parameter floor, and it is comfortably above anything this producer can
/// actually reach: the largest store measured holds 587 threads in total
/// (`tests/derivation_cost.rs`), of which 151 are idle on the machine default
/// alone, and the count that matters — threads holding a lock, a park, a retry
/// or a queue row — is ZERO on this machine today and bounded in practice by how
/// many turns one host can run at once. So it cannot truncate a real fleet even
/// if every thread in that store were busy at once.
///
/// **When it does bite, it is reported as `Partial`, never as `Complete`.** A
/// `Complete(vec![…512 of 900])` would be the handler that lied, which
/// `docs/shapes.md` 4.3 names as the one thing only a test can catch.
pub const MAX_NON_IDLE_HYDRATE_ROWS: usize = 512;

/// The result of [`ThreadManager::thread_rows_non_idle`]: the rows, and whether
/// the read is a whole answer.
///
/// A bare `Vec` could not say "there were more than I am allowed to carry", and
/// the caller ([`crate::shapes::accounts`]'s `Handles<ThreadRows>`) has to choose
/// between `Complete` and `Partial` — a distinction `docs/shapes.md` 3.8 makes
/// load-bearing and which `empty_and_partial_are_different_verdicts` pins.
#[derive(Debug, Clone)]
pub struct NonIdleThreads {
    pub rows: Vec<crate::shapes::accounts::ThreadEntry>,
    /// `Some(reason)` when [`MAX_NON_IDLE_HYDRATE_ROWS`] was hit. The read is a
    /// statement about the read, so it becomes `Hydration::Partial`.
    pub truncated: Option<String>,
}

/// **THE ANSWER TO "IS THIS THREAD RUNNING", AND THE ONLY ONE.**
///
/// Produced solely by [`ThreadManager::thread_states`]. Every surface that
/// reports a thread's activity — `threads.status`, `threads.list`,
/// `threads.get`, `threads.seen`, the `status` push frame — renders THIS, so
/// two callers asking the same question at the same instant are reading the
/// same computation, not two copies of it.
///
/// Named `DerivedState` and not `ThreadState` because `ThreadState` is already
/// the crate-private per-thread runtime struct (settings, atomics, latches).
/// Two types with one name in one module would be exactly the sort of ambiguity
/// this whole change exists to remove.
/// **THE STATE VOCABULARY, CLOSED.** The five words [`ThreadManager::thread_states`]
/// derives, in precedence order, and the words the `threads` and `accounts`
/// shapes DECLARE on `shapes.list` (`Shape::CELLS`, `status.state` and
/// `threads.*.state`) so a consumer can be checked against them rather than
/// against a doc comment. A word a consumer compares against that is not here
/// — `pending` was one, for months — is a word no producer will ever emit.
///
/// `tests/cell_manifest.rs::the_state_vocabulary_is_the_derivation` pins this
/// list against the literals in `thread_states`, so the two cannot drift.
pub const STATE_WORDS: &[&str] = &["active", "parked", "retrying", "queued", "idle"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedState {
    pub thread_id: String,
    /// `active` | `parked` | `retrying` | `queued` | `idle`. The vocabulary AUI
    /// reads off the `state` key (`status_frame_executing` accepts `active` and
    /// `retrying`, and its own tests pin that).
    pub state: &'static str,
    /// A live execution holds this thread's lock. AUI's `status_frame_executing`
    /// prefers this boolean over `state`, and it is the authority.
    pub executing: bool,
    /// Waiting (accepted, undelivered, uncancelled) queue rows.
    pub queued: u64,
    /// The ledger seq this thread has been read through (0 = nothing).
    pub last_seen_seq: u64,
    /// The park or retry reason, when there is one.
    pub detail: Option<String>,
}

impl DerivedState {
    /// The reading for a thread nothing knows anything about: no lock, no park,
    /// no retry, no queue. A thread that has never run is genuinely idle, and
    /// saying so is not a default — it is the answer.
    pub fn idle(thread_id: &str) -> Self {
        Self {
            thread_id: thread_id.to_string(),
            state: "idle",
            executing: false,
            queued: 0,
            last_seen_seq: 0,
            detail: None,
        }
    }

    /// **THE LEGACY WIRE PROJECTION** — `busy` | `retrying` | `idle`, the
    /// three-word vocabulary the deleted `threads.status` column carried.
    ///
    /// `threads.list` and `threads.get` still emit this under the `status` key,
    /// and MUST. AUI's `status_str_is_running` accepts exactly `busy` and
    /// `retrying` there, pinned by AUI's own tests, and its `threads.list`
    /// fallback path is what paints the fleet when `threads.status` is
    /// unavailable. Change or drop this spelling and every thread silently reads
    /// as idle in the operator's ear.
    ///
    /// The mapping is the deleted column's own, preserved exactly: `parked` and
    /// `queued` projected to `idle` there too (the column was written from
    /// liveness and a retry row and knew nothing about parks or queues). Richer
    /// news travels on `state`/`executing`, which ride the same payload.
    pub fn status_word(&self) -> &'static str {
        match self.state {
            "active" => "busy",
            "retrying" => "retrying",
            _ => "idle",
        }
    }
}

/// **P6's period.** Five minutes, and it is not `resume_tick`.
///
/// The operator, on this exact producer: *"Polling shouldn't be the default."*
/// It is not the default here — `accounts.login.complete`, `accounts.rotate`
/// and `note_login_restored` sample directly, because those are the paths the
/// daemon drives and knows the instant of. This period exists only for a write
/// another process made behind the daemon's back (`claude /login`,
/// `claude-accounts`, the rotation script), which has no event to hook.
///
/// Overridable for tests only (`THREADS_CREDENTIAL_SAMPLE_MS`, read into
/// `Tunables::credential_sample`).
pub(crate) const CREDENTIAL_SAMPLE_PERIOD: Duration = Duration::from_secs(300);

/// The daemon's thread registry + event firehose.
#[derive(Clone)]
pub struct ThreadManager {
    pub store: Store,
    /// The LOGIN SLOT registry (`accounts.toml`). It belongs to the
    /// `claude-accounts` CLI, and the daemon reads it for exactly one thing:
    /// deriving the pool when no `pool.json` exists. **Nothing on the
    /// placement, metering, gauge, probe or status path may read it** — those
    /// all read [`Self::pool`]. `tests/default_out_of_placement.rs` guards that.
    pub registry: Arc<Registry>,
    /// **THE POOL, RELOADABLE** — the flat list of typed resource entries the
    /// manager draws from (`pool.json`, else derived from the registry). This is
    /// the daemon's account list; the machine default is not in it, and not
    /// because it was filtered.
    ///
    /// **IT USED TO BE A PLAIN `Arc<Pool>` BUILT ONCE AT BOOT, AND THAT WAS A
    /// DEFECT WITH TEETH.** `lib.rs::build_manager` loaded it exactly once and
    /// nothing ever replaced it, so every account the daemon could see was
    /// frozen at the instant the process started. Three consequences, all real
    /// and all reachable from the account board:
    ///
    /// * **A NEW ACCOUNT WAS INVISIBLE UNTIL RESTART.** `accounts.login.begin`
    ///   on an unused label runs `scripts/add-account`, which creates the
    ///   directory and registers the slot with `threads accounts adopt` — in a
    ///   CHILD process, writing `accounts.toml` underneath us. The login
    ///   succeeded, the credential was real, and `accounts.list` / `.status` /
    ///   `.pressure` kept reporting the old roster. So "add an account from the
    ///   TUI" half-worked in the most confusing possible way.
    /// * **A RENAME LEFT PLACEMENT POINTING AT A DIRECTORY THAT NO LONGER
    ///   EXISTED.** `accounts.rename` moves the login directory on disk and
    ///   rewrites the registry; the frozen pool kept the OLD label with the OLD
    ///   `login_dir`, and [`Self::config_dir_for`] hands that path to the spawn
    ///   as `CLAUDE_CONFIG_DIR`. A turn placed there spawns against a path that
    ///   is gone.
    /// * **A DELETE LEFT A GHOST ROW** that `accounts.list` still drew and that
    ///   placement would still rank.
    ///
    /// The frame that says "re-read the accounts" was already being broadcast
    /// for all three (`renamed`, `deleted`, and the login's terminal progress),
    /// so a faithful client re-read and was handed the SAME STALE ANSWER. The
    /// push channel cannot be correct while the thing it points at cannot move.
    ///
    /// **Why `RwLock<Arc<Pool>>` and not `ArcSwap`.** It needs no new
    /// dependency, the read is `.read().unwrap().clone()` — one uncontended
    /// lock and one `Arc` bump, on paths that already do far more work (a
    /// `names()` clones every string) — and writes happen only on roster
    /// changes, which are operator acts measured in minutes apart. Every reader
    /// goes through [`Self::pool`], which hands back an `Arc` SNAPSHOT: a caller
    /// that iterates entries holds a consistent pool for the whole walk even if
    /// a reload lands mid-walk, which a `&Pool` borrow out of the lock could not
    /// have promised.
    ///
    /// The handle is SHARED with [`crate::budgets::Budgets`], which needs the
    /// same pool for a declared share — see [`crate::budgets::Budgets::with_pool_handle`].
    /// Two independently-reloaded copies would be two answers to one question.
    pool: PoolCell,
    pub config: Arc<Config>,
    /// **THE ONE EMIT POINT.** Was `firehose: broadcast::Sender<Push>`; the
    /// sender still exists and is still public, as [`crate::shape::Hub::firehose`].
    ///
    /// Every `firehose.send` in the crate became a `hub.emit`/`hub.emit_at` so
    /// that the producer's stamp is applied in exactly ONE place (the fold
    /// therefore never reads a clock) and every registered shape sees every
    /// input in arrival order. The firehose itself is untouched: same capacity,
    /// same `Push` values, same order, so the legacy wire is byte-identical.
    pub hub: Arc<crate::shape::Hub>,
    /// **THE OPEN PROMISES** (`docs/shapes.md` 2.4). A command replies with an
    /// id from here and never with the fact it caused; the fact arrives through
    /// the shape carrying that id. Empty at rest — `open_ids()` being non-empty
    /// after a deterministic drain is a handler that forgot to emit.
    ///
    /// `Arc`, not a value: `ThreadManager` is `Clone`, and a book that CLONED
    /// would mean a promise opened on one clone could never be resolved on
    /// another — every command would hang exactly when the handler and the
    /// producer happened to hold different clones.
    pub promises: std::sync::Arc<shapewire::command::PromiseBook>,
    /// Minted once per daemon process; carried on `Welcome` (§9.3).
    pub boot_id: String,
    /// **THE RUN-TOKEN KEY** (R4.S1, `docs/managed-subagents.md` §5.1): the
    /// machine key that signs `THREADS_RUN_TOKEN` and verifies it on every
    /// `/mcp` call. Read from `<data_dir>/run-token.key`, minted there on first
    /// use, `0600`. Every turn's token is minted with it in `run_once`
    /// ([`ThreadManager::mint_turn_token`], R4.S5); `pub` so a test can mint
    /// one too.
    pub run_token_key: Arc<shapewire::token::Key>,
    /// **WHERE THIS DAEMON'S `/mcp` FAÇADE ACTUALLY LISTENS** (R4.S5), as the
    /// chassis bound it. Set by [`crate::chassis::serve`] from the listener's
    /// own `local_addr`, so a test on port 0 hands its turns the ephemeral
    /// port and not a configured one. Unset, a turn is handed
    /// [`Config::effective_chassis_port`] on `host` — which is exactly what
    /// `run_daemon` binds (it refuses port 0, and reads `THREADS_CHASSIS_PORT`,
    /// so a second environment's port is the one named). See
    /// [`ThreadManager::mcp_url`].
    mcp_endpoint: Arc<StdMutex<Option<std::net::SocketAddr>>>,
    /// **THE MANAGED SUBAGENT RUNTIME** (R4.S2, `crate::managed`): the durable
    /// ledgers under `<data_dir>/ledgers/` and the lock their folds take.
    managed: Arc<crate::managed::Managed>,
    /// **LOGINS IN FLIGHT.** A login is a two-round-trip conversation with a
    /// human in the middle — the child prints a URL and then blocks reading a
    /// pasted code — so the child outlives the request that started it and has
    /// to be owned somewhere. See [`crate::login`].
    pub logins: crate::login::LoginSessions,
    /// When this process built its manager, RFC3339 UTC. Carried on
    /// `daemon.status` so a client can say how long the daemon has been up
    /// without keeping its own clock — and, more usefully, so a client that
    /// reconnects can tell "the same daemon, still up" from "a new daemon that
    /// replaced it" by reading a stamp rather than inferring it from `boot_id`
    /// changing, which only says THAT it changed.
    pub started_at: String,
    /// The budgets module (§15) — keyed budgets, thresholds, the layered
    /// gauge, and the continuous limit estimate. The single accounting
    /// authority (DEC-7).
    pub budgets: Budgets,
    /// The live per-account usage probe (DEC-15 layer 2). `NoProbe` unless
    /// probing is enabled in config.
    probe: Arc<dyn UsageProbe>,
    /// THE UPDATE PROTOCOL (intent item 64). Owns admission for turn starts and
    /// the count of in-flight WS requests, so it — and only it — can say when
    /// the daemon is quiet enough to replace itself.
    pub update: Arc<UpdateGate>,
    teardown: TeardownStrategy,
    data_dir: PathBuf,
    threads: Arc<StdMutex<HashMap<String, Arc<ThreadState>>>>,
    /// **TC-44: THE LAST ACTIVITY SECOND PUBLISHED PER MANAGED CHILD, and each
    /// thread's parent** (lineage is written once and never moves, so it is
    /// cached; `None` is a root). See [`ThreadManager::note_activity`].
    child_activity: Arc<StdMutex<HashMap<String, (Option<String>, String)>>>,
    tunables: Tunables,
    /// The resume/probe watchers are started once, from whichever of
    /// `serve()` / `recover()` runs first.
    watchers_started: Arc<AtomicBool>,
    /// Per-account edge-poll state (§3.2): how many polls sit in the current
    /// idle gap, whether a settle ladder is walking, and what the last ladder
    /// measured.
    edge: Arc<StdMutex<HashMap<String, EdgeState>>>,
    /// The settle ladder offsets, resolved once from config.
    settle_ladder: Arc<Vec<Duration>>,
    /// **THE ACCOUNT PICTURE CHANGED.** Rung whenever an input to placement
    /// moves — a limit window recorded or cleared, a login proven dead or
    /// revived, a fresh `/usage` observation. [`Self::retry_sweep`] waits on
    /// this, so a retrying thread re-asks placement on the CHANGE rather than
    /// on a clock. `notify_one` (not `notify_waiters`) on purpose: it stores a
    /// permit, so a change that lands while a sweep is already running is not
    /// lost.
    account_picture: Arc<tokio::sync::Notify>,
    /// **WHICH EVENTS HAVE RUNG SINCE THE LAST PLACEMENT PASS.**
    ///
    /// The bell is a `Notify` holding ONE permit, which is what makes a storm
    /// of simultaneous changes cost one pass — but a permit carries no name, so
    /// without this the pass could not say what woke it. Every ring inserts its
    /// reason here; [`Self::placement_pass`] takes the whole set and reports it.
    ///
    /// A `BTreeSet` and not a `Vec`: it is bounded by the number of distinct
    /// reasons in the event set (a few dozen), so it cannot grow without bound
    /// however long a pass is delayed, and a reason that rings five times in one
    /// coalescing window is one entry rather than five — which is the shape the
    /// coalescing proof asserts on.
    wake_pending: Arc<StdMutex<std::collections::BTreeSet<String>>>,
    /// **THE PLACEMENT PASS LEDGER** — see [`PlacementPasses`]. It exists so the
    /// BACKSTOP'S FIRING IS OBSERVABLE: a poll that finds work the events should
    /// have woken is a defect report, not a success.
    placement: Arc<PlacementLedger>,
    /// **THE POOL'S BACKOFF** — see [`PoolCadence`]. One cadence for every parked
    /// prompt, in place of one floor per thread.
    cadence: Arc<PoolCadence>,
    /// Rung the placement task to RECOMPUTE its cadence without running a pass.
    ///
    /// A new parked prompt shortens the pool's cadence (its `attempt` is 1), and a
    /// cleared one may lengthen it. Neither is a reason to reconsider placement —
    /// the first is a failure that has just happened and the second is a success —
    /// so this is a separate bell from `account_picture`, and ringing it must not
    /// show up as `by_event`.
    cadence_rearm: Arc<tokio::sync::Notify>,
    /// **WHICH LOGINS WERE BARRED WHEN THE NOTICER LAST LOOKED.**
    ///
    /// The one piece of state [`Self::notice_expired_limits`] needs to make a
    /// window ending into an EDGE rather than a condition. A `limit_until`
    /// column is a watermark nothing rewrites when the clock passes it, so
    /// "is it in the past" is true forever once it is true, and a noticer
    /// without this would ring `limit_expired` every tick until the next 429.
    ///
    /// It is a set of account LABELS, in memory, fleet-wide — deliberately not a
    /// per-account scheduled wake. A per-bar timer would be precise and would
    /// also be a per-thing timestamp, which is the pattern the operator objects
    /// to; this is one cheap "has any bar ended" question asked on a clock, which
    /// is the honest shape for noticing a non-event.
    barred_seen: Arc<StdMutex<std::collections::BTreeSet<String>>>,
    /// Threads whose sweep-driven retry fire is in flight. Without it every
    /// 5s tick would spawn another fire for the same row and each refusal
    /// would arm another timer — a slow amplification, not a deadlock, which
    /// is the kind that is noticed late.
    retry_firing: Arc<StdMutex<std::collections::HashSet<String>>>,
    /// **How many times in a row each login has failed to authenticate**, keyed
    /// by login identity (falling back to the account label when no fingerprint
    /// is known).
    ///
    /// Indexes [`crate::retry::login_bar_secs`], so the first recheck after a
    /// fresh failure is prompt (60s) and a login that keeps failing settles to
    /// the ten-minute ceiling instead of respawning a doomed process every
    /// minute forever.
    ///
    /// Cleared by [`Self::note_account_served`] and
    /// [`Self::note_login_restored`] — any good news restarts the ladder.
    ///
    /// In MEMORY, not in the schema, and the tradeoff is deliberate: a daemon
    /// restart forgets every streak and rechecks sooner, which is the safe
    /// direction (one extra local spawn, zero tokens) and needs no migration
    /// on a table another agent is actively reworking.
    login_bar_streak: Arc<StdMutex<HashMap<String, u32>>>,
    /// **WHAT THE LAST `/usage` SWEEP COULD NOT POLL**, one entry per account
    /// that failed, replaced wholesale on every sweep.
    ///
    /// It exists because [`Self::probe_accounts_once`] used to collect these
    /// and then return them ONLY when every account failed. One sibling
    /// succeeding was enough to discard every other account's reason, forever
    /// and at no log level — so an account that had not polled in weeks was
    /// indistinguishable from one polling cleanly, and the free
    /// `alice-example-com` login stayed invisible exactly that way.
    ///
    /// In memory, deliberately: this is the CURRENT state of the sweep, not a
    /// history, and the history is the log line each failure now writes.
    probe_failures: Arc<StdMutex<Vec<String>>>,
    /// **WARN-1 N-2: THE ACCOUNTS WHOSE `/usage` PROBE IS FAILING NOW**, with
    /// how many consecutive probes have failed. A failure WARNs only when the
    /// account enters this map, and a success WARNs once when it leaves; every
    /// probe between is INFO, so a login that stays broken is one warning per
    /// episode instead of one per sweep (census §3 K3).
    probe_failing: Arc<StdMutex<HashMap<String, u64>>>,
    /// **SLOTS WHOSE LOGIN IS WIPED** (`SlotHealth::Wiped`): the store exists
    /// and holds no refresh token, so nothing but `claude-accounts login
    /// <label>` can make the slot run a turn again. Keyed by LABEL — this is a
    /// fact about one slot's own store, not about the login it once held — with
    /// the instant it was last read (`None` = recheck due).
    ///
    /// It exists because the login bar is TIMED, and a timed bar on a wiped
    /// slot is a thrash: the bar expires, `notice_expired_limits` re-runs
    /// placement, the same threads land on the same slot, the turn fails on the
    /// credential and the bar is written again (observed live on
    /// `alice-example-com`, 2026-10-14). Filled only after a turn failed to
    /// authenticate; emptied by a recheck that finds the slot signed in, a
    /// reported login, or a completed turn. In memory: a restart costs one
    /// failed turn to re-learn it, and never a keychain read per placement.
    ///
    /// H179: the value now also carries the slot's reading word (`wiped`,
    /// `expired`, `stranded` — [`crate::accounts::SlotHealth::word`]), because
    /// it is one of the inputs of [`ThreadManager::account_unavailability`] and
    /// a reason has to say which.
    ///
    /// R9.A2c: the set is a [`SignInSet`], whose ONE writer is
    /// [`SignInSet::apply`] — reached only through
    /// [`ThreadManager::apply_sign_in`]. Every source (boot seed, auth failure,
    /// the credential sampler, the recheck, a reported login, a completed turn)
    /// hands it a FACT stamped with the instant its reading STARTED, and a fact
    /// older than the newest one already applied to that label is dropped. So a
    /// sampler tick whose keychain read began before a probe marked the slot
    /// can no longer remove the label the probe just set.
    sign_in_needed: Arc<StdMutex<SignInSet>>,
    /// How a slot's health is read: the keychain in production
    /// ([`crate::accounts::probe_slot_health`]), a stand-in under test.
    slot_probe: Arc<StdMutex<SlotProbe>>,
    /// [`SIGN_IN_RECHECK`] in milliseconds; injectable for tests only.
    sign_in_recheck_ms: Arc<std::sync::atomic::AtomicU64>,
    /// **R9.A3 (A-3):** per DEAD login (`dead_since` set), when the recheck
    /// last read its slot. A dead login is out of placement and the spawn
    /// guard refuses it, so no turn can ever clear it; the recheck reads its
    /// slot once per [`SIGN_IN_RECHECK`] instead, and a Live reading clears it.
    dead_rechecked: Arc<StdMutex<std::collections::BTreeMap<String, std::time::Instant>>>,
    /// **R9.L4 (L-4): the budget-kill LATCH, durable and PER BUDGET KEY.**
    ///
    /// A kill fires on the live mid-turn ESTIMATE; the turn epilogue then
    /// reconciles that estimate against the authoritative `costUSD`, and when
    /// the estimate over-shot, the meter drops back UNDER the limit. Without
    /// this latch the parked prompt is immediately admissible again, so it
    /// redispatches, over-shoots, and is killed again — a livelock that spawns
    /// a real, billable turn on every cycle. Observed live 2026-07-29: eight
    /// kill/park/resume cycles in 29 seconds before the daemon was stopped.
    ///
    /// It used to be a per-`ThreadState` field, which left three holes (L-4):
    /// a restart forgot it (one paid kill per restart), only a limit edit
    /// released it (never the window rolling over), and a SIBLING thread on
    /// the same key was not held by it (its meter read under the limit too).
    ///
    /// Now the truth is the LEDGER: the `budget_tripped` event with
    /// `action: kill` that every kill already writes (`check_trip_midturn`),
    /// plus the operator's `turn_interrupted{trigger: operator}` on the killed
    /// thread for a cancel. No new table, column or event kind — this map is a
    /// cache of that reading, seeded once per daemon life by
    /// [`Self::kill_latches`] and written on every kill. `None` = not seeded.
    ///
    /// Released (see `admit_or_park`) when the tripped axis no longer carries
    /// the limit that killed the turn (DEC-11, the operator's escape hatch), or
    /// when EVERY account the key meters has rolled into a window that began
    /// after the kill ([`Budgets::rolled_over_since`]) — the same rollover the
    /// meter uses, after which the killed spend is out of the meter entirely.
    kill_latches: Arc<StdMutex<Option<HashMap<String, KillLatch>>>>,
    /// **SH-A2: THE ATTACHED LEASES**, by cookie — the `TurnLock`, the open
    /// projected turn and the two files an interactive session holds until
    /// detach (SH-A4). See [`attach`].
    attached: Arc<StdMutex<HashMap<String, attach::AttachedHold>>>,
    /// TEST ONLY: fail the next attach right after its row is written.
    attach_fault_after_insert: Arc<AtomicBool>,
    /// **PL-3: the protoledger shadow**, present only when the daemon started
    /// with `THREADS_PL_SHADOW=1` (`crate::pl::shadow`). `None` — the default
    /// — means no turn writes anything to a protoledger store.
    pl_shadow: Option<Arc<crate::pl::shadow::Shadow>>,
}

/// One latched budget kill (R9.L4) — see `ThreadManager::kill_latches`.
#[derive(Debug, Clone)]
struct KillLatch {
    axis: Axis,
    /// The limit, in the axis's own unit, that the kill tripped at.
    limit: f64,
    /// When the kill fired: the instant a window must have opened after for
    /// the latch to release on rollover.
    killed_at: DateTime<Utc>,
    /// The thread whose turn was killed.
    thread_id: String,
    /// The operator cancelled the killed thread's park (R9.L3). That releases
    /// the latch for THAT thread only; every sibling on the key stays held.
    own_released: bool,
}

/// **THE ONE AVAILABILITY PREDICATE, ON THE ROW THE BOARD DRAWS (H179).**
///
/// Stamps a `pressure` row with [`ThreadManager::account_unavailability`]'s
/// answer: `available`, and `unavailable_reasons` in the predicate's own words.
/// An account the predicate bars is also `unavailable: true` and unranked —
/// the board renders exactly what placement acts on, and a dashboard that says
/// "wiped" can no longer sit beside a pressure row that says "ranked 1".
pub fn with_availability(
    mut row: serde_json::Value,
    account: &str,
    unavailable: &std::collections::BTreeMap<String, Vec<String>>,
) -> serde_json::Value {
    let reasons = unavailable.get(account).cloned().unwrap_or_default();
    if let Some(o) = row.as_object_mut() {
        o.insert("available".into(), serde_json::json!(reasons.is_empty()));
        o.insert("unavailable_reasons".into(), serde_json::json!(reasons));
        if !reasons.is_empty() {
            o.insert("unavailable".into(), serde_json::json!(true));
            o.insert("ranked".into(), serde_json::json!(false));
            o.insert("rank".into(), serde_json::Value::Null);
            if o.get("unranked_reason").map_or(true, |v| v.is_null()) {
                o.insert("unranked_reason".into(), serde_json::json!("unavailable"));
            }
        }
    }
    row
}

/// Reads one slot's [`crate::accounts::SlotHealth`] from `(label, login_dir)`.
pub type SlotProbe =
    Arc<dyn Fn(&str, Option<&str>) -> crate::accounts::SlotHealth + Send + Sync>;

/// Reads one credential store's facts by config dir (`None` = the machine
/// default): [`crate::accounts::read_store_facts`] in production.
pub type CredentialReader =
    Arc<dyn Fn(Option<&str>) -> crate::accounts::CredentialFacts + Send + Sync>;

/// **The slot probe, classifying against the whole pool (R9.A2, gap A-2).**
/// Every OTHER pool slot plus the machine default (`"the live login"`) is a
/// peer, so a slot whose secret is byte-identical to another store's reads
/// `CloneOf`, and a capture whose twin rotated away reads `Stranded` — the
/// classification the board draws, now also the one placement acts on.
///
/// R9.A2b (CON-28): a DISABLED slot is never a peer. Its store is not read to
/// decide anyone else's clone-ness — a disabled slot is not checked at all.
fn pool_slot_probe(
    pool: crate::pool::PoolCell,
    budgets: Budgets,
    read: CredentialReader,
) -> SlotProbe {
    Arc::new(move |label: &str, dir: Option<&str>| {
        let pool = crate::pool::snapshot(&pool);
        let mut peers: Vec<(String, Option<String>)> = pool
            .names()
            .into_iter()
            .filter(|n| n != label)
            .filter(|n| !budgets.is_disabled(n))
            .filter_map(|n| pool.login_dir(&n).map(|d| (n.clone(), Some(d.to_string()))))
            .collect();
        if dir.is_some() {
            peers.push(("the live login".to_string(), None));
        }
        crate::accounts::probe_slot_health_among(dir, &peers, &*read)
    })
}

/// How often a slot known to be wiped is re-read from the keychain. One
/// `security` spawn per wiped slot per interval, on the bar-expiry tick. The
/// production value; a test shrinks it with
/// [`ThreadManager::set_sign_in_recheck_for_test`] (R9.A2b, doctrine 4).
const SIGN_IN_RECHECK: Duration = Duration::from_secs(30);

/// What one source learned about one slot's login, handed to the ONE writer
/// of the sign-in set ([`SignInSet::apply`], R9.A2c).
pub(crate) enum SignInFact {
    /// A keychain reading of the slot ([`ThreadManager::probe_slot`]).
    Reading(crate::accounts::SlotHealth),
    /// The slot's account is DISABLED (CON-28): it carries no sign-in label,
    /// and nothing was read to learn that.
    Disabled,
    /// A login was reported, or a turn completed on the slot — either proves
    /// the store authenticates.
    SignedIn,
}

/// What [`SignInSet::apply`] did with a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignInChange {
    /// The fact was older than the newest one already applied to this label,
    /// and changed nothing. This is the interleave guard.
    Stale,
    /// Applied; membership and word unchanged.
    Unchanged,
    /// The label joined the set.
    Marked,
    /// Still in the set, under a different word.
    Reworded,
    /// The label left the set.
    Cleared,
}

impl SignInChange {
    fn changed(self) -> bool {
        matches!(self, Self::Marked | Self::Reworded | Self::Cleared)
    }
}

/// **THE SIGN-IN SET, WITH EXACTLY ONE WRITER (R9.A2c).**
///
/// `labels` is what placement and the board read: label → (last checked,
/// `None` = recheck due; the reading's word). `applied` is, per label, the
/// instant the newest APPLIED fact's reading started — kept after the label
/// leaves, so a stale fact can neither remove a fresh mark nor re-add a label
/// a newer fact cleared.
///
/// Before this, five sites wrote the map directly, and A1's sampler
/// (`reconcile_sign_in_needed`) applied a reading that could have STARTED
/// before the boot/auth-failure probe marked the slot: the probe's label was
/// removed by an older reading (MERGE-32 cp5, the suspected cause of
/// `slot_probe_clone_stranded`'s full-suite-only red).
#[derive(Default)]
pub(crate) struct SignInSet {
    labels: std::collections::BTreeMap<String, (Option<std::time::Instant>, &'static str)>,
    applied: std::collections::BTreeMap<String, std::time::Instant>,
}

impl SignInSet {
    /// **The one writer of the label.** `taken_at` is when the fact's reading
    /// STARTED (for an event, when it happened). `disabled` is the account's
    /// state, read by the caller before the lock: a disabled slot is never
    /// given a label, whatever the fact says.
    pub(crate) fn apply(
        &mut self,
        label: &str,
        fact: SignInFact,
        taken_at: std::time::Instant,
        disabled: bool,
    ) -> SignInChange {
        if self.applied.get(label).is_some_and(|newest| *newest > taken_at) {
            return SignInChange::Stale;
        }
        self.applied.insert(label.to_string(), taken_at);
        let word = match fact {
            SignInFact::Reading(h) if h.needs_sign_in() && !disabled => Some(h.word()),
            _ => None,
        };
        match word {
            Some(word) => {
                match self
                    .labels
                    .insert(label.to_string(), (Some(std::time::Instant::now()), word))
                {
                    None => SignInChange::Marked,
                    Some((_, was)) if was != word => SignInChange::Reworded,
                    Some(_) => SignInChange::Unchanged,
                }
            }
            None => {
                if self.labels.remove(label).is_some() {
                    SignInChange::Cleared
                } else {
                    SignInChange::Unchanged
                }
            }
        }
    }

    /// Every labelled slot whose last check is older than `period`.
    fn due(&self, period: Duration) -> Vec<String> {
        self.labels
            .iter()
            .filter(|(_, (at, _))| at.map_or(true, |t| t.elapsed() >= period))
            .map(|(l, _)| l.clone())
            .collect()
    }

    /// Make every labelled slot due for a recheck (TESTS: a new probe).
    /// Schedules only; never changes who is labelled.
    fn mark_all_due(&mut self) {
        for v in self.labels.values_mut() {
            v.0 = None;
        }
    }

    fn word(&self, label: &str) -> Option<&'static str> {
        self.labels.get(label).map(|(_, w)| *w)
    }

    fn labels(&self) -> Vec<String> {
        self.labels.keys().cloned().collect()
    }
}

impl ThreadManager {
    pub fn new(
        store: Store,
        registry: Registry,
        pool: crate::pool::Pool,
        config: Config,
        data_dir: PathBuf,
    ) -> Self {
        let hub = Arc::new(crate::shape::Hub::new());
        // The probe is ALWAYS the real one. `probe_interval_secs` schedules the
        // BACKGROUND sweep (see `start_watchers`) and nothing else — wiring
        // `NoProbe` when the interval was 0 also disabled the explicit
        // `gauge.probe` call, which then returned `observations: 0` with no
        // error and no reason. A "poll now" that silently does nothing is worse
        // than one that costs a `/usage` invocation.
        let probe: Arc<dyn UsageProbe> = Arc::new(crate::gauge::ClaudeUsageProbe::default());
        let teardown = TeardownStrategy::from_env_or(&config);
        let settle_ladder = Arc::new(settle_ladder_of(&config));
        // ONE cell, handed to both holders. `Budgets` reads the declared share
        // off the same pool placement reads its membership off, so a reload has
        // to reach both or the two disagree about what accounts exist.
        let pool: PoolCell = crate::pool::cell(pool);
        let pool_for_probe = pool.clone();
        let budgets = Budgets::new(store.clone()).with_pool_handle(pool.clone());
        // The key is read or minted under the same directory the store was
        // just opened in, so a data dir the store could write is one the key
        // can be written to; a failure here is the failure `Store::open`
        // would have reported one line earlier, and it is spelled rather than
        // defaulted — a process-local key would void every token across a
        // restart and say nothing.
        let key_path = data_dir.join("run-token.key");
        let run_token_key = Arc::new(
            shapewire::token::Key::load_or_mint(&key_path).unwrap_or_else(|e| {
                panic!("the run-token key at {}: {e}", key_path.display())
            }),
        );
        let data_dir_for_shadow = data_dir.clone();
        let mgr = Self {
            budgets: budgets.clone(),
            store,
            registry: Arc::new(registry),
            pool,
            config: Arc::new(config),
            hub,
            promises: std::sync::Arc::new(shapewire::command::PromiseBook::new()),
            boot_id: uuid::Uuid::new_v4().to_string(),
            run_token_key,
            mcp_endpoint: Arc::new(StdMutex::new(None)),
            managed: Arc::new(crate::managed::Managed::new(&data_dir)),
            started_at: crate::store::now_rfc3339(),
            logins: crate::login::LoginSessions::default(),
            probe,
            update: UpdateGate::new(data_dir.clone()),
            teardown,
            data_dir,
            threads: Arc::new(StdMutex::new(HashMap::new())),
            child_activity: Arc::new(StdMutex::new(HashMap::new())),
            tunables: Tunables::default(),
            watchers_started: Arc::new(AtomicBool::new(false)),
            edge: Arc::new(StdMutex::new(HashMap::new())),
            settle_ladder,
            account_picture: Arc::new(tokio::sync::Notify::new()),
            wake_pending: Arc::new(StdMutex::new(std::collections::BTreeSet::new())),
            placement: Arc::new(PlacementLedger::default()),
            cadence: Arc::new(PoolCadence::default()),
            cadence_rearm: Arc::new(tokio::sync::Notify::new()),
            barred_seen: Arc::new(StdMutex::new(std::collections::BTreeSet::new())),
            retry_firing: Arc::new(StdMutex::new(std::collections::HashSet::new())),
            login_bar_streak: Arc::new(StdMutex::new(HashMap::new())),
            probe_failures: Arc::new(StdMutex::new(Vec::new())),
            probe_failing: Arc::new(StdMutex::new(HashMap::new())),
            sign_in_needed: Arc::new(StdMutex::new(SignInSet::default())),
            slot_probe: Arc::new(StdMutex::new(pool_slot_probe(
                pool_for_probe,
                budgets,
                Arc::new(crate::accounts::read_store_facts),
            ))),
            sign_in_recheck_ms: Arc::new(std::sync::atomic::AtomicU64::new(
                SIGN_IN_RECHECK.as_millis() as u64,
            )),
            dead_rechecked: Arc::new(StdMutex::new(std::collections::BTreeMap::new())),
            kill_latches: Arc::new(StdMutex::new(None)),
            attached: Arc::new(StdMutex::new(HashMap::new())),
            attach_fault_after_insert: Arc::new(AtomicBool::new(false)),
            pl_shadow: crate::pl::shadow::Shadow::from_env(&data_dir_for_shadow).map(Arc::new),
        };
        mgr.refresh_identities();
        // Hand the gate the firehose and this boot's id, so a restart can
        // ANNOUNCE itself rather than only being inferred from a dropped
        // socket. The gate is built for a data directory and knows nothing
        // about the manager, so this is a wiring step rather than a
        // constructor argument — and a gate that never gets wired (a test, a
        // CLI verb) simply never announces.
        mgr.update.wire_announcer(mgr.hub.clone(), &mgr.boot_id);
        mgr
    }

    /// Replace the update gate (intent item 64) with one pinned to an explicit
    /// binary.
    ///
    /// For TESTS: a test harness's `current_exe()` is the harness itself, so the
    /// gate `new` builds is deliberately disarmed. This hands the manager a gate
    /// aimed at a fixture binary, so the tests drive the real mechanism —
    /// admission, quiet window, preflight — instead of a stand-in for it. Call
    /// it before the manager is cloned; every clone shares the `Arc`.
    pub fn with_update_gate(mut self, gate: Arc<UpdateGate>) -> Self {
        // The replacement gate inherits the announcement wiring, or the tests
        // that exercise the real restart path would be exercising a gate that
        // structurally cannot announce — which is the one thing they are least
        // able to notice.
        gate.wire_announcer(self.hub.clone(), &self.boot_id);
        self.update = gate;
        self
    }

    /// **THE POOL AS IT STANDS RIGHT NOW** — a snapshot, taken per call.
    ///
    /// Every reader of the roster goes through here. It replaced a `pub pool:
    /// Arc<Pool>` field that callers read directly, and the field went private
    /// deliberately: a direct read of a cell is a read of whatever the cell held
    /// when the borrow was taken, and the whole point of making the pool
    /// reloadable is that "whatever it held" is no longer a constant. One
    /// accessor is also the only way [`Self::reload_pool`] can be sure it has
    /// reached every consumer.
    ///
    /// Cheap: one uncontended read lock and one `Arc` clone. Call it per use
    /// rather than caching it in a struct — a cached copy is the defect this
    /// change exists to remove.
    pub fn pool(&self) -> Arc<crate::pool::Pool> {
        crate::pool::snapshot(&self.pool)
    }

    /// **RE-DERIVE THE POOL FROM DISK, AND SAY SO IF IT MOVED.**
    ///
    /// The roster changes underneath a running daemon in four ways, and only one
    /// of them is a write this process makes to its own memory:
    ///
    /// * `accounts.login.begin` on an unused label — `scripts/add-account`
    ///   creates the directory and `threads accounts adopt` registers the slot,
    ///   both in a CHILD process, writing `accounts.toml` from outside.
    /// * `accounts.rename` — moves the login directory and rewrites the
    ///   registry, so the entry's `login_dir` changes as well as its name.
    /// * `accounts.delete` — removes the slot entirely.
    /// * The operator running `claude-accounts` / `threads accounts adopt` in a
    ///   terminal, with no WS call involved at all.
    ///
    /// So the source of truth is the FILES, and this re-reads them exactly as
    /// `lib.rs::build_manager` does at boot — `pool.json` when it exists, else
    /// derived from `accounts.toml`. Deriving it any other way (patching the
    /// in-memory pool per operation) would give boot and reload two different
    /// definitions of the roster, and the fourth case above has no operation to
    /// patch from.
    ///
    /// **A BROKEN `pool.json` IS NOT A REASON TO LOSE THE POOL WE HAVE.**
    /// `Pool::load` treats an unparseable file as an error, which is right at
    /// boot — the daemon refuses to start on a config it cannot honour. Here the
    /// daemon is already serving on a pool that works, and replacing it with
    /// nothing because a hand edit is mid-save would take placement out of
    /// service. So a failure is logged loudly and the standing pool is kept.
    ///
    /// Returns `true` when the pool ACTUALLY CHANGED. Callers use that to decide
    /// whether to announce — a reload that finds the same roster must not emit
    /// an `Info`, because `Info` means "what you are showing changed" and a
    /// client that re-reads on a no-op is a client we taught to poll again.
    pub fn reload_pool(&self) -> bool {
        let registry = match Registry::load(&self.data_dir.join("accounts.toml")) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "re-reading accounts.toml failed — KEEPING the pool this daemon is \
                     already serving on rather than replacing it with nothing"
                );
                return false;
            }
        };
        // ONE DIFFERENCE FROM BOOT, stated because it is a real one: boot also
        // merges `config.accounts` inline seeds into the registry before
        // deriving. It does not need repeating here, because `build_manager`
        // SAVES that merge back to `accounts.toml` whenever it changed anything
        // — so by the time this runs, every seed is already a row in the file
        // being re-read. A seed that changes while the daemon runs would need a
        // config reload, which is a different feature and has no caller.
        let next = match crate::pool::Pool::load(&self.data_dir.join("pool.json"), &registry) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "re-deriving the pool failed — KEEPING the pool this daemon is already \
                     serving on; a config that cannot be parsed must not take placement out \
                     of service"
                );
                return false;
            }
        };
        // **P5 — THE ROSTER DIFF, TAKEN WHERE BOTH POOLS EXIST.**
        //
        // `reload_pool` already knows exactly when the roster changed — it
        // returns `bool` for that reason and callers announce on it — so the
        // roster producer is a diff here rather than a comparison somebody
        // re-derives later. It is captured INSIDE the write-lock block, because
        // that is the only instant the old and new pools are both in hand, and
        // EMITTED outside it: `hub.emit_fact` walks every shape's FIFO and must
        // never run under the pool's write guard.
        let mut gained: Vec<(String, Option<String>)> = Vec::new();
        let lost: Vec<String>;
        {
            let mut g = self.pool.write().unwrap_or_else(|e| e.into_inner());
            if **g == next {
                return false;
            }
            tracing::info!(
                before = ?g.names(),
                after = ?next.names(),
                "POOL RELOADED — the account roster moved while the daemon was running"
            );
            let before: std::collections::BTreeMap<String, Option<String>> = g
                .entries
                .iter()
                .map(|e| (e.name.clone(), e.login_dir().map(str::to_string)))
                .collect();
            for e in &next.entries {
                let dir = e.login_dir().map(str::to_string);
                // A login_dir change is a re-registration, not a no-op: a
                // rename moves the directory on disk and `config_dir_for`
                // hands that path to the spawn.
                if before.get(&e.name) != Some(&dir) {
                    gained.push((e.name.clone(), dir));
                }
            }
            let after: std::collections::BTreeSet<&str> =
                next.entries.iter().map(|e| e.name.as_str()).collect();
            lost = before
                .keys()
                .filter(|n| !after.contains(n.as_str()))
                .cloned()
                .collect();
            *g = Arc::new(next);
        }
        if self.hub.any_tracking() {
            for (name, login_dir) in gained {
                self.hub
                    .emit_fact(crate::event::Fact::AccountRegistered { name, login_dir });
            }
            for name in lost {
                self.hub.emit_fact(crate::event::Fact::AccountRemoved { name });
            }
        }
        // The identity fingerprints are keyed by pool name, so a roster that
        // grew a member has a member nothing has fingerprinted. Without this a
        // new account has no `identity` row, `selection_units` cannot collapse
        // it with an alias, and `poll_targets` would measure one login twice.
        self.refresh_identities();
        // ── AND THE FLEET-WIDE SCOPE, WHICH IS A FUNCTION OF THE POOL ──────
        //
        // **THIS LINE IS WHAT MAKES THE 2026-09-18 PLACEMENT FIX SURVIVE A
        // RELOADABLE POOL, AND WITHOUT IT THAT FIX SILENTLY REGRESSES.**
        //
        // `Budgets::ensure_default` reconciles `aui:default`'s account scope to
        // the pool, widen-only. It was called at boot and NOWHERE ELSE, and its
        // own docs said that was sufficient — with a reason that was true when
        // it was written: *"`self.pool` is built once at construction and is
        // read-only for the process's life, so boot is the only instant at
        // which the pool can have changed."*
        //
        // `PoolCell` is precisely the retirement of that premise. The pool now
        // changes four ways while the process runs (see this function's own
        // list), and every one of them can ADD a name. An account that joins
        // the pool at 14:00 would be in the roster, be fingerprinted, be
        // pressure-ranked and be polled by the busy scan — and still be removed
        // by `admit`'s FIRST filter, `scoped_accounts`, for every thread
        // carrying the fleet-wide key. Which is the reported defect, exactly:
        // *"if an account is added to the pool, there's a condition where it can
        // never be used."* The account would be invisible until the next daemon
        // restart, and the restart is the thing this whole wave exists to stop
        // needing.
        //
        // It belongs HERE rather than at the four call sites because the
        // invariant is "the scope follows the pool", and the pool has exactly
        // one writer. A per-call-site version would be four statements of one
        // rule, and the fourth case — the operator adopting a slot in a
        // terminal — has no call site to state it at.
        //
        // On the CHANGED path only. A reload that found the same roster changed
        // nothing the scope is derived from, and this runs once per watcher
        // tick; re-reading and re-writing a budget row to confirm a fact that
        // cannot have moved is the polling this wave is removing, one layer
        // down.
        //
        // Widen-only is unchanged and is why a DELETE is safe to route through
        // here: a name that left the pool is left in the scope, exactly as at
        // boot. Dropping a name is `purge_account_from_scopes`'s job and it is
        // told which name by a caller that knows what the name means.
        // P5's tail: every account's share and limit level re-asserted for the
        // new roster, then P15. `pool_reloaded` (`:4255`) is KEPT beside this
        // for old boards — the announcement and the facts are paired
        // everywhere, never substituted.
        for n in self.account_pool() {
            self.emit_share_fact(&n);
            // P16 beside P3: an account that just joined the roster has an
            // inclusion state as surely as it has a share, and a mirror that
            // learned the share and not the state would draw it as live.
            self.emit_disabled_fact(&n);
            self.emit_limit_fact(&n);
            self.emit_health_fact(&n);
        }
        self.produce_pressure();
        let names = self.account_pool();
        match self.budgets.ensure_default(&names) {
            // `ensure_default` logs the specific line — created, or widened and
            // with which names. Silence here means the scope already held every
            // name in the new roster, which is the ordinary case for a rename
            // that reused a scoped label or a delete.
            Ok(_) => {}
            Err(e) => tracing::warn!(
                error = %e,
                accounts = names.len(),
                "the pool reloaded but the fleet-wide budget scope could not be reconciled to \
                 it — an account that just joined the roster stays invisible to every thread \
                 carrying that key until this succeeds or the daemon restarts"
            ),
        }
        true
    }

    /// Fingerprint every registered account's LOGIN once, at daemon open, and
    /// say out loud when two labels turn out to be one login.
    ///
    /// This is the prerequisite the design puts first (§3.1): the registry had no
    /// notion of identity, so two entries pointing at the same underlying login
    /// counted as two accounts, and every pool count, exclusivity claim and
    /// "N accounts means N×100% capacity" statement over them was false.
    ///
    /// Reads one JSON file per account and hashes three of its fields. It never
    /// invokes `claude`, never touches the keychain, never reads a token, and
    /// never records anything but a hash.
    fn refresh_identities(&self) {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            tracing::warn!("HOME is not set; account identities cannot be fingerprinted");
            return;
        };
        for name in self.pool().names() {
            let cd = self.config_dir_for(&name);
            let fp = crate::accounts::identity_of(cd.as_deref(), &home);
            if fp.is_none() {
                tracing::debug!(
                    account = %name,
                    "no readable oauthAccount identity for this account; it is treated as \
                     its own login (absence never collapses two accounts into one)"
                );
            }
            if let Err(e) = self
                .store
                .set_account_identity(&name, cd.as_deref(), fp.as_deref())
            {
                tracing::warn!(account = %name, error = %e, "recording the account identity failed");
            }
        }
        let names = self.pool().names();
        let states = self.account_states();
        for unit in crate::accounts::selection_units(&names, &states) {
            if unit.is_aliased() {
                tracing::warn!(
                    labels = ?unit.members,
                    selection_unit = %unit.canonical,
                    "these registry labels are ONE login — counted as one account for \
                     selection, failover and capacity"
                );
            }
        }
        self.log_account_pool();
        // **P8 — an identity change can clear an effective limit.**
        //
        // A limit row is written with the LOGIN IDENTITY that earned it
        // (`set_account_limit(.., identity)`), and `effective_limit_until`
        // refuses to report a window whose identity no longer matches the
        // login behind the label — so re-fingerprinting can silently make a bar
        // stop existing. Nothing announced that, because nothing here writes.
        // Re-asserting the LEVEL for every account is the whole fix: the fold
        // sees the current answer, which is `LimitCleared` exactly when the
        // window stopped being effective.
        for n in names {
            self.emit_limit_fact(&n);
        }
    }

    /// **The account picture, in one line per account.**
    ///
    /// It used to be a single aggregate — `labels=3 logins=2` — which is a true
    /// statement that answers no question anyone has. On 2026-08-14 the useful
    /// sentence was "owner-account: weekly 100%, 6 threads on it", and
    /// nothing in the daemon could produce it at any level.
    ///
    /// Called on boot (identity refresh) and on every probe tick that actually
    /// polled, so the log carries the gauge's own trajectory account by account
    /// rather than only its endpoints.
    pub fn log_account_pool(&self) {
        let names = self.pool().names();
        let states = self.account_states();
        tracing::info!(
            labels = names.len(),
            logins = crate::accounts::distinct_healthy_logins(&names, &states),
            "account pool"
        );
        for name in &names {
            let threads = self.in_flight_count(name);
            let obs = |axis: Axis| {
                self.budgets
                    .observed_value(name, axis)
                    .ok()
                    .flatten()
                    .unwrap_or(-1.0)
            };
            // **THROUGH THE ACCESSOR, AND IT USED TO BE A HAND-INLINED COPY OF
            // IT.** `limit_until.is_some() && !limit_is_stale()` is, character
            // for character, the pre-2026-10-06 body of
            // `AccountState::effective_limit_until` — so when that function
            // learned that an elapsed window is no window, this line did not,
            // because it never called it.
            //
            // This is the line the operator was reading. `account usage … barred=true`
            // for `alice-example-com` on a window that ended on 2 October came
            // from here, not from the `accounts` shape: a copy of a predicate is
            // a copy of every bug the original is ever fixed for.
            let now_s = crate::store::now_rfc3339();
            let barred = states
                .get(name)
                .is_some_and(|s| s.effective_limit_until(&now_s).is_some());
            // `-1` for an axis the gauge has never reported: an absence must be
            // legible as an absence and never as a zero (CON-5's shape, in a
            // log line).
            tracing::info!(
                account = %name,
                session_pct = obs(Axis::Session),
                weekly_pct = obs(Axis::WeeklyAll),
                fable_pct = obs(Axis::WeeklyFable),
                %threads,
                %barred,
                "account gauge"
            );
        }
    }

    /// Every tracked account row, keyed by name (limit window, identity, health).
    fn account_states(&self) -> std::collections::BTreeMap<String, AccountState> {
        self.store
            .list_account_states()
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.name.clone(), s))
            .collect()
    }

    /// The accounts selection may actually choose from: ONE label per distinct
    /// login (aliases collapsed onto their canonical label), no dead login, and
    /// — CON-27 — never the machine default.
    ///
    /// Returns [`Placeable`], so this is not "the pool, filtered": it is the
    /// only expression in the crate that produces the type placement consumes.
    /// The machine default is dropped BEFORE aliases are collapsed, which is
    /// the part that was actually broken — see [`crate::accounts::placeable_pool`].
    pub fn selectable_pool(&self) -> Vec<Placeable> {
        let unavailable = self.account_unavailability();
        crate::accounts::selectable_pool(&self.pool(), &self.account_states())
            .into_iter()
            .filter(|p| !unavailable.contains_key(p.name()))
            .collect()
    }

    /// **THE ONE AVAILABILITY PREDICATE (H179).** Every pool label that may not
    /// be tried for a turn, on ANY path, with the machine words that say why.
    /// An account absent from this map is available.
    ///
    /// The operator, 2026-10-16: cc2's login was shown expired, frozen and
    /// wiped on the dashboard, and placement still put mishmash there first and
    /// fell back to cc1 only after the turn failed. *"That should not have
    /// happened."* Every signal below "should be interpreted as account not
    /// available":
    ///
    /// * `disabled` — his own switch (`accounts/<a>/disable`). Unpaid accounts
    ///   are disabled with a note saying so.
    /// * `login_wiped` / `login_expired` / `login_stranded` / `login_clone-of`
    ///   — the slot's keychain
    ///   reading ([`crate::accounts::SlotHealth::needs_sign_in`]), the same
    ///   classification the board draws as WIPED / EXPIRED / FROZEN. Read at
    ///   boot, on every auth failure, and rechecked on the bar-expiry tick.
    /// * `login_dead` — a login proven dead by an auth failure no retry fixes.
    /// * `auth_failed` — inside the timed bar a failed authentication wrote
    ///   (401 / `authentication_failed`, and 403).
    /// * `billing` — inside the bar an exhausted credit balance or a
    ///   payment/billing refusal wrote.
    ///
    /// A usage-limit window is NOT here: it is a timed throughput fact, not an
    /// account fault, and stays in [`Self::barred_accounts`] as before.
    ///
    /// Consumers, and there is no second copy: [`Self::selectable_pool`]
    /// (creation and every `Auto` placement), [`Self::barred_accounts`] (the
    /// admission gate — staying AND moving, so a thread sitting on an
    /// unavailable account is failed over BEFORE its turn, never after), the
    /// ungated escape hatch and the spawn guard in `run_once` (the last line),
    /// and the `accounts` shape's `pressure` row (`available` /
    /// `unavailable_reasons`), which is what the board renders.
    pub fn account_unavailability(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let names = self.pool().names();
        let states = self.account_states();
        let now = crate::store::now_rfc3339();
        let slots: std::collections::BTreeMap<String, &'static str> = {
            let set = self.sign_in_needed.lock().unwrap();
            names.iter().filter_map(|n| set.word(n).map(|w| (n.clone(), w))).collect()
        };
        let dead = crate::accounts::unhealthy_accounts(&names, &states);
        let mut out = std::collections::BTreeMap::new();
        for name in &names {
            let mut why: Vec<String> = Vec::new();
            if self.budgets.is_disabled(name) {
                why.push("disabled".into());
            }
            if let Some(word) = slots.get(name) {
                why.push(format!("login_{word}"));
            }
            if dead.contains(name) {
                why.push("login_dead".into());
            }
            if let Some(s) = states.get(name) {
                if s.effective_limit_until(&now).is_some() {
                    match s.limit_axis.as_deref() {
                        Some("login") => why.push("auth_failed".into()),
                        Some("credit") => why.push("billing".into()),
                        _ => {}
                    }
                }
            }
            if !why.is_empty() {
                out.insert(name.clone(), why);
            }
        }
        out
    }

    /// [`Self::account_unavailability`] for one label: `None` = available.
    pub fn account_unavailable_reasons(&self, name: &str) -> Option<Vec<String>> {
        self.account_unavailability().remove(name)
    }

    /// Every account threads may place on, WITHOUT alias-collapsing or the
    /// health filter — the pool the admission gate is handed, which applies
    /// health itself through `unhealthy` and needs to see every label.
    ///
    /// The same membership as [`Self::account_pool`], in the placement type.
    /// The two are the SAME LIST — as of 2026-08-27 there is no longer a
    /// "visible" pool that is wider than the placeable one, because the only
    /// thing the wider list ever carried was the machine default.
    pub fn placement_pool(&self) -> Vec<Placeable> {
        self.pool().placeable()
    }

    /// **The cause, on its way to placement.** The two exclusion sets
    /// `Budgets::admit` is handed on EVERY admission:
    ///
    /// * `unavailable` — accounts that cannot serve a turn at all (an observed
    ///   usage-limit window, and every other label for that login). This is how
    ///   the 429 a turn just took, recorded by [`Self::note_account_limit`],
    ///   reaches the one ranking instead of being re-decided in a private
    ///   selector.
    /// * `unhealthy` — logins proven dead: never a placement target. The thread
    ///   already on one does NOT keep running there: `login_dead` is in
    ///   [`Self::account_unavailability`], so it is also in `unavailable` above
    ///   and the thread is failed over before its turn (and the spawn guard
    ///   refuses it). A completed turn therefore cannot clear the flag; a probe
    ///   of the slot that reads Live does ([`Self::recheck_sign_in_needed`] and
    ///   the credential sample, R9.A3), as does a reported login.
    ///
    /// This answers "what is barred". It never answers "so go there".
    ///
    /// A slot whose login is WIPED is `unavailable` for as long as it stays
    /// wiped, whatever its timed login bar says: the bar exists so one turn can
    /// discover a re-authentication, and a wiped store has nothing to discover.
    /// Read from memory ([`Self::sign_in_needed`]), never from the keychain.
    pub fn barred_accounts(&self) -> (Vec<String>, Vec<String>) {
        let names = self.pool().names();
        let states = self.account_states();
        let mut unavailable =
            crate::accounts::unavailable_accounts(&names, &states, &crate::store::now_rfc3339());
        // H179: the ONE predicate bars staying as well as moving, so a thread
        // whose account became unavailable is failed over BEFORE its turn.
        for label in self.account_unavailability().into_keys() {
            if names.contains(&label) && !unavailable.contains(&label) {
                unavailable.push(label);
            }
        }
        unavailable.sort();
        (unavailable, crate::accounts::unhealthy_accounts(&names, &states))
    }

    /// Labels currently known to be wiped (sorted). Memory only.
    pub fn sign_in_needed_labels(&self) -> Vec<String> {
        self.sign_in_needed.lock().unwrap().labels()
    }

    /// Replace how slot health is read. For TESTS, which must never touch the
    /// keychain. Every known-wiped slot becomes due for a recheck, because a
    /// reading taken through the old probe says nothing about the new one.
    pub fn set_slot_probe_for_test(&self, probe: SlotProbe) {
        *self.slot_probe.lock().unwrap() = probe;
        self.sign_in_needed.lock().unwrap().mark_all_due();
    }

    /// Keep the PRODUCTION probe — peers, manifest mode, classification — and
    /// replace only the keychain read under it. For TESTS (R9.A2).
    pub fn set_credential_reader_for_test(&self, read: CredentialReader) {
        self.set_slot_probe_for_test(pool_slot_probe(
            self.pool.clone(),
            self.budgets.clone(),
            read,
        ));
    }

    /// Shrink [`SIGN_IN_RECHECK`] so a test can outlast it (doctrine 4). For
    /// TESTS (R9.A2b).
    pub fn set_sign_in_recheck_for_test(&self, period: Duration) {
        self.sign_in_recheck_ms
            .store(period.as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
    }

    fn sign_in_recheck(&self) -> Duration {
        Duration::from_millis(self.sign_in_recheck_ms.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// **The one gate in front of every slot read (R9.A2b, CON-28).** A
    /// DISABLED slot is not checked at all — no keychain read, no probe — and
    /// is reported `disabled` from config alone: `None`. Every caller (boot
    /// seed, auth failure, the credential sampler, the periodic recheck) comes
    /// through here.
    ///
    /// R9.A2c: the reading carries the instant it STARTED, which is what
    /// [`SignInSet::apply`] orders facts by.
    fn probe_slot(
        &self,
        label: &str,
    ) -> Option<(crate::accounts::SlotHealth, std::time::Instant)> {
        if self.budgets.is_disabled(label) {
            return None;
        }
        let probe = self.slot_probe.lock().unwrap().clone();
        let pool = self.pool();
        let started = std::time::Instant::now();
        Some((probe(label, pool.login_dir(label)), started))
    }

    /// **The only path to the sign-in set's writer (R9.A2c).** Reads whether
    /// `label` is disabled (a store read) BEFORE taking the set's lock, then
    /// hands the fact to [`SignInSet::apply`].
    fn apply_sign_in(
        &self,
        label: &str,
        fact: SignInFact,
        taken_at: std::time::Instant,
    ) -> SignInChange {
        let disabled = self.budgets.is_disabled(label);
        let live = matches!(fact, SignInFact::Reading(crate::accounts::SlotHealth::Live));
        let change = self
            .sign_in_needed
            .lock()
            .unwrap()
            .apply(label, fact, taken_at, disabled);
        // R9.A3 (A-3): a probe of the slot that reads LIVE is also the evidence
        // that lifts `login_dead`. Every probe reading — the sampler's and the
        // recheck's — reaches this one place, so there is no second site. A
        // stale reading (older than the newest fact) lifts nothing, and a
        // disabled slot is never read (`probe_slot`) and never revived here.
        if live
            && !disabled
            && change != SignInChange::Stale
            && self.revive_dead_login_on_probe(label, taken_at)
        {
            self.produce_pressure();
        }
        change
    }

    /// **A Live reading of `label`'s slot lifts its `dead_since` (R9.A3).**
    /// Only when the reading STARTED after the login was marked dead: a probe
    /// that began before the auth failure wrote the flag says nothing about
    /// the store the server just rejected. Returns whether it cleared.
    fn revive_dead_login_on_probe(&self, label: &str, started: std::time::Instant) -> bool {
        let Some(since) = self
            .account_states()
            .get(label)
            .and_then(|s| s.dead_since.clone())
        else {
            return false;
        };
        let elapsed = chrono::Duration::from_std(started.elapsed())
            .unwrap_or_else(|_| chrono::Duration::zero());
        let read_at = crate::clock::now() - elapsed;
        if DateTime::parse_from_rfc3339(&since).is_ok_and(|d| d.with_timezone(&Utc) > read_at) {
            return false;
        }
        self.revive_dead_login(label, "its slot reads Live")
    }

    /// **Lift `login_dead` from `account` and say so (R9.A3).** The clear of
    /// the dead flag on evidence the login authenticates — a completed turn
    /// ([`Self::note_login_health`]) or a Live slot reading
    /// ([`Self::revive_dead_login_on_probe`]). Returns whether it was dead.
    /// Not the only clear: a REPORTED login ([`Self::note_login_restored`])
    /// clears `dead_since` itself and rings `login_restored`, not `login_revived` (R9.A3b).
    fn revive_dead_login(&self, account: &str, evidence: &str) -> bool {
        let dead = self.account_states().get(account).is_some_and(|s| !s.healthy());
        if !dead {
            return false;
        }
        tracing::info!(%account, evidence, "this login authenticates again — back in the pool");
        if let Err(e) = self.store.set_account_health(account, None) {
            tracing::warn!(%account, error = %e, "clearing account health failed");
            return false;
        }
        // A REVIVED login is the case the clock could never see: a thread
        // asleep until 01:00 would have ignored it for hours.
        self.account_picture_changed_for("login_revived", Some(account));
        self.emit_health_fact(account); // P4
        true
    }

    /// **A turn on `label` just failed to authenticate: is its store wiped?**
    /// One keychain read, on the failure path only. A wiped slot leaves
    /// placement until its classification changes; anything else keeps the
    /// ordinary timed login bar and nothing more.
    fn note_slot_after_auth_failure(&self, label: &str) -> bool {
        let Some((health, started)) = self.probe_slot(label) else {
            return false;
        };
        if !health.needs_sign_in() {
            return false;
        }
        let word = health.word();
        if self.apply_sign_in(label, SignInFact::Reading(health), started) == SignInChange::Marked {
            tracing::error!(
                account = %label,
                slot_state = word,
                "LOGIN WIPED on this slot — its keychain store holds no refresh token, so it \
                 is out of placement until it is signed in again. RUN `claude-accounts login \
                 {label}`; no timed bar will put work back on it before that, and once signed \
                 in again it returns within one recheck/sample, with no turn needed"
            );
            // H179: the board's `pressure` row carries the availability verdict.
            self.produce_pressure();
        }
        true
    }

    /// **BOOT: READ EVERY POOL SLOT ONCE, SO A SLOT WIPED WHILE THE DAEMON WAS
    /// DOWN IS OUT OF PLACEMENT BEFORE THE FIRST TURN.** `sign_in_needed` is
    /// memory only and was filled only by an auth failure, so every restart
    /// placed the first turn it could onto each wiped slot to learn it was
    /// wiped (H179: the parent 60f0380d's wake was ported to the wiped
    /// `bob-example-com` and failed "OAuth session expired" at 20:40:46Z).
    /// One keychain read per slot, at boot only; the periodic recheck then
    /// owns the slots this marks, exactly as it owns ones a failure marked.
    ///
    /// R9.A2c: only needs-sign-in readings are applied here — boot ADDS, it
    /// never lifts — and only through [`Self::apply_sign_in`]. A disabled
    /// slot is never read (`probe_slot`) and never labelled (`apply`), so the
    /// "on a DISABLED account" branch that used to sit here was unreachable and
    /// is gone.
    pub fn seed_sign_in_needed(&self) {
        let mut any_fresh = false;
        for label in self.pool().names() {
            // R9.A2b (CON-28): a disabled slot is not read at boot either.
            let Some((health, started)) = self.probe_slot(&label) else {
                continue;
            };
            if !health.needs_sign_in() {
                continue;
            }
            let word = health.word();
            if self.apply_sign_in(&label, SignInFact::Reading(health), started)
                == SignInChange::Marked
            {
                any_fresh = true;
                tracing::warn!(
                    account = %label,
                    slot_state = word,
                    "LOGIN {} at boot — not available, out of placement until \
                     `claude-accounts login {label}`; signed in again, it returns within \
                     one recheck/sample",
                    word.to_uppercase()
                );
            }
        }
        if any_fresh {
            self.produce_pressure();
        }
    }

    /// **A-1 (R9.A1): READ EVERY POOL SLOT AND MAKE THE SIGN-IN SET AGREE.**
    /// Called from [`Self::sample_credentials`]. A slot whose reading needs a
    /// sign-in (WIPED / EXPIRED / STRANDED) joins the set; a member that reads
    /// anything else leaves it and rings placement, exactly as
    /// [`Self::recheck_sign_in_needed`] does. Any change re-stamps the
    /// `pressure` rows, so the board and placement read one verdict. Uses the
    /// same probe as boot, the auth-failure path and the recheck, so the four
    /// never disagree about a slot. Returns `(barred, restored)` labels.
    ///
    /// R9.A2c: every reading goes through [`Self::apply_sign_in`], stamped with
    /// the instant it started. A reading that began before a probe marked the
    /// slot is STALE and changes nothing — a sample tick no longer removes a
    /// label the probe just set.
    ///
    /// CON-28: a DISABLED account gets no sign-in advice — it is simply
    /// unavailable, and it is never read.
    pub fn reconcile_sign_in_needed(&self) -> (Vec<String>, Vec<String>) {
        let mut barred = Vec::new();
        let mut restored = Vec::new();
        let mut changed = false;
        for label in self.pool().names() {
            // R9.A2b-r (E17-16, CON-28): a DISABLED slot is not checked at
            // all, and never carries a sign-in label — that label reads as
            // "sign in to it". `None` = not read; any label is dropped unread,
            // exactly as `recheck_sign_in_needed` drops it. It shows only
            // `disabled`.
            let Some((health, started)) = self.probe_slot(&label) else {
                changed |= self
                    .apply_sign_in(&label, SignInFact::Disabled, std::time::Instant::now())
                    .changed();
                continue;
            };
            let word = health.word();
            let needs = health.needs_sign_in();
            let change = self.apply_sign_in(&label, SignInFact::Reading(health), started);
            changed |= change.changed();
            match change {
                SignInChange::Marked => {
                    barred.push(label.clone());
                    tracing::warn!(
                        account = %label,
                        slot_state = word,
                        "LOGIN {} (credential sample) — not available, out of placement \
                         until `claude-accounts login {label}`; signed in again, it returns \
                         within one recheck/sample",
                        word.to_uppercase()
                    );
                }
                SignInChange::Cleared if !needs => restored.push(label),
                _ => {}
            }
        }
        self.announce_restored(&restored, "credential sample: ");
        if changed {
            self.produce_pressure();
        }
        (barred, restored)
    }

    /// **The "back in placement" line, worded for what the account IS
    /// (R9.A2c, folding A1's inspect follow-up).** A slot that read usable
    /// again rings placement. When its account is DISABLED it is NOT back in
    /// placement — it is still unavailable as `disabled` — so it gets a line
    /// that says so, no placement bell, and no sign-in advice (CON-28). With
    /// the gate in `probe_slot` this arm is reached only when a disable races
    /// a reading already in flight.
    fn announce_restored(&self, restored: &[String], prefix: &str) {
        for label in restored {
            if self.budgets.is_disabled(label) {
                tracing::info!(
                    account = %label,
                    "{prefix}slot left the sign-in set; the account is DISABLED, so it stays \
                     unavailable"
                );
                continue;
            }
            tracing::info!(
                account = %label,
                "{prefix}slot reads usable again — back in placement, which re-runs now"
            );
            self.account_picture_changed_for("slot_signed_in", Some(label));
        }
    }

    /// Forget a slot's wiped reading — a login was reported or a turn completed
    /// on it, either of which proves the store authenticates.
    fn clear_sign_in_needed(&self, label: &str) -> bool {
        self.apply_sign_in(label, SignInFact::SignedIn, std::time::Instant::now())
            == SignInChange::Cleared
    }

    /// **Re-read every wiped slot whose last reading is older than
    /// [`SIGN_IN_RECHECK`].** A slot the operator has signed in leaves the set
    /// and rings the placement bell, so it is placeable again on this tick
    /// rather than on the next failure. Returns the labels that left.
    ///
    /// R9.A3: it also reads, on the same period, every DEAD login's slot
    /// (`dead_since`) not already in the set; a Live reading lifts the flag.
    fn recheck_sign_in_needed(&self) -> Vec<String> {
        let period = self.sign_in_recheck();
        let due = self.sign_in_needed.lock().unwrap().due(period);
        let mut restored = Vec::new();
        let mut dropped_disabled = false;
        for label in due {
            // R9.A2b (CON-28): a slot disabled after it was marked leaves the
            // set unread — it is reported `disabled` from config alone and is
            // never re-read, on this tick or any later one.
            let Some((health, started)) = self.probe_slot(&label) else {
                dropped_disabled |= self
                    .apply_sign_in(&label, SignInFact::Disabled, std::time::Instant::now())
                    .changed();
                continue;
            };
            let needs = health.needs_sign_in();
            if self.apply_sign_in(&label, SignInFact::Reading(health), started)
                == SignInChange::Cleared
                && !needs
            {
                restored.push(label);
            }
        }
        // R9.A3 (A-3): a DEAD login (`dead_since`) that is not in the sign-in
        // set is read here too, once per period. Nothing else would ever clear
        // it: it is unavailable, so no turn completes on it. The reading goes
        // through `apply_sign_in` like every other — a Live one lifts the dead
        // flag there (and rings `login_revived`); a needs-sign-in one joins the
        // set, which the loop above then owns. A disabled slot is not read.
        let mut dead_marked = false;
        {
            let states = self.account_states();
            let in_set = self.sign_in_needed_labels();
            let dead: Vec<String> = self
                .pool()
                .names()
                .into_iter()
                .filter(|n| states.get(n).is_some_and(|s| !s.healthy()) && !in_set.contains(n))
                .collect();
            let due_dead: Vec<String> = {
                let mut seen = self.dead_rechecked.lock().unwrap();
                // R9.A3b (A-3): an entry outlives the REVIVAL until its period
                // ends. Dropping it the moment the login reads healthy let a
                // re-killed login (Live store, server still rejecting) be read
                // again on the very next tick — 2 reads in one period, not 1.
                seen.retain(|l, t| dead.contains(l) || t.elapsed() < period);
                dead.into_iter()
                    .filter(|l| seen.get(l).map_or(true, |t| t.elapsed() >= period))
                    .collect()
            };
            for label in due_dead {
                let Some((health, started)) = self.probe_slot(&label) else {
                    continue;
                };
                self.dead_rechecked.lock().unwrap().insert(label.clone(), started);
                dead_marked |= self.apply_sign_in(&label, SignInFact::Reading(health), started)
                    == SignInChange::Marked;
            }
        }
        self.announce_restored(&restored, "");
        if !restored.is_empty() || dropped_disabled || dead_marked {
            self.produce_pressure();
        }
        restored
    }

    /// The clause appended to a park/failover reason while any pool slot is
    /// wiped, so the refusal names the one action that fixes it.
    ///
    /// R9.A3: the hint is TRUE for `login_dead` too — a Live reading of the
    /// slot lifts `dead_since` within one recheck/sample, no turn needed. `pub`
    /// so a test reads the exact clause a park reason carries.
    pub fn sign_in_note(&self) -> String {
        // H179: every unavailable account, with the predicate's own words. A
        // DISABLED account gets no "sign in" advice: the operator disables an
        // account he no longer pays for, and re-enabling or logging in to it is
        // never the suggested fix.
        self.account_unavailability()
            .into_iter()
            .map(|(l, why)| {
                let hint = if why.iter().any(|w| w == "disabled") {
                    String::new()
                } else if why.iter().any(|w| w.starts_with("login_")) {
                    format!(
                        " — out of placement until `claude-accounts login {l}`; signed in \
                         again, it returns within one recheck/sample"
                    )
                } else {
                    String::new()
                };
                format!("; {l}: not available ({}){hint}", why.join(", "))
            })
            .collect()
    }

    /// How many distinct logins we hold that still authenticate — the only
    /// honest account count (`registry.names().len()` counts labels).
    pub fn distinct_healthy_logins(&self) -> usize {
        crate::accounts::distinct_healthy_logins(&self.pool().names(), &self.account_states())
    }

    /// **The account POOL (DEC-5/DEC-10): every name in the pool.**
    ///
    /// Metering, gauge polling, status output, the activity roll-up and every
    /// WS surface read this. It used to be `registry.names()` — the VISIBLE
    /// list, which carried the machine default because the operator manages
    /// that slot with `claude-accounts`. The operator struck that split on
    /// 2026-08-27: *"I do not want default even in the purview of the system.
    /// Why does it even see that there is a default? … It's a completely
    /// separate system, not even visible to placement."*
    ///
    /// So this is the POOL now, and the machine default stops APPEARING on
    /// every one of those surfaces at once — rather than each of them growing
    /// its own filter, which is the thing that would have gone stale.
    pub fn account_pool(&self) -> Vec<String> {
        self.pool().names()
    }

    /// A thread's budget key, read from its live settings (`None` = no
    /// budget).
    pub fn thread_budget_key(&self, thread_id: &str) -> Result<Option<String>> {
        let state = self.get_state(thread_id)?;
        let key = state.settings.lock().unwrap().budget_key.clone();
        Ok(key)
    }

    /// A thread's park state, or `Value::Null` when it is not parked.
    ///
    /// Two shapes of park exist and both are reported here: a GATE park (the
    /// admission gate said no; the messages are still queued) and a KILLED
    /// park (`prompt_parked: true` — an in-flight execution was torn down and
    /// its prompt lives in a durable row).
    pub fn park_state(&self, thread_id: &str) -> Result<Value> {
        let live = self
            .threads
            .lock()
            .unwrap()
            .get(thread_id)
            .and_then(|s| s.park_reason.lock().unwrap().clone());
        let row = self.store.get_parked_turn(thread_id)?;
        if live.is_none() && row.is_none() {
            return Ok(Value::Null);
        }
        Ok(serde_json::json!({
            "reason": row.as_ref().map(|r| r.reason.clone()).or(live),
            "budget_key": row.as_ref().and_then(|r| r.budget_key.clone()),
            "message_ids": row.as_ref().map(|r| r.message_ids.clone()).unwrap_or_default(),
            "parked_at": row.as_ref().map(|r| r.parked_at.clone()),
            "prompt_parked": row.is_some(),
        }))
    }

    /// Subscribe to the firehose (subscribers filter by thread id).
    pub fn subscribe(&self) -> broadcast::Receiver<Push> {
        self.hub.firehose.subscribe()
    }

    // ── thread lifecycle ─────────────────────────────────────────────────

    /// **The `CLAUDE_CONFIG_DIR` a POOL account spawns with** — where its
    /// login is stored, read off the pool entry that names it.
    ///
    /// Read off the POOL, not the registry, and that is what closes the worst
    /// hole this wave found. Through the registry, an unknown name and the
    /// machine default both resolved to `None`, and `None` means "leave
    /// `CLAUDE_CONFIG_DIR` unset", which means `~/.claude` — the operator's own
    /// terminal login. So every path that could reach here with a name threads
    /// does not own would silently spawn AS HIM. Through the pool there is no
    /// such name: an entry either names its login directory or is not an entry.
    fn config_dir_for(&self, name: &str) -> Option<String> {
        self.pool().login_dir(name).map(str::to_string)
    }

    // ── THE TURN'S DOOR TO MANAGED EXECUTION (R4.S5) ───────────────────────

    /// Record where the chassis actually bound, so every turn from here on is
    /// handed that address. Called by [`crate::chassis::serve`]; the last bind
    /// wins.
    pub fn set_mcp_endpoint(&self, addr: std::net::SocketAddr) {
        *self.mcp_endpoint.lock().unwrap() = Some(addr);
    }

    /// **THE `/mcp` URL A TURN IS HANDED.** The address the chassis reported
    /// binding, else what `run_daemon` binds: `host` on
    /// [`Config::effective_chassis_port`] (which honours `THREADS_CHASSIS_PORT`,
    /// so a non-prod environment names its own port). An unspecified bind host
    /// (`0.0.0.0`, `::`) is dialled on loopback, since the child runs on this
    /// machine. `None` only when neither can be known.
    pub fn mcp_url(&self) -> Option<String> {
        let addr = match *self.mcp_endpoint.lock().unwrap() {
            Some(a) => a,
            None => {
                let port = self.config.effective_chassis_port().ok().filter(|p| *p != 0)?;
                format!("{}:{port}", self.config.host).parse().ok()?
            }
        };
        let ip = if addr.ip().is_unspecified() {
            match addr {
                std::net::SocketAddr::V4(_) => std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                std::net::SocketAddr::V6(_) => std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
            }
        } else {
            addr.ip()
        };
        Some(format!("http://{}/mcp", std::net::SocketAddr::new(ip, addr.port())))
    }

    /// **MINT THE RUN TOKEN FOR ONE TURN** (`docs/managed-subagents.md` §5.1).
    /// `turn` is the spawn cookie the `running_turns` row will carry, which is
    /// the token's whole validity window: `ThreadsProducer::verify` refuses it
    /// the moment that row is cleared or re-keyed by a later turn. `root` and
    /// `depth` are read off the managed lineage (`ThreadMeta.parent_id`), so a
    /// managed child's turn carries its tree; a thread that took part in no
    /// spawn is a root at depth 0.
    pub fn mint_turn_token(&self, thread_id: &str, turn: &str) -> String {
        let mut root = thread_id.to_string();
        let mut depth: u32 = 0;
        let mut seen = HashSet::new();
        seen.insert(root.clone());
        while let Some(p) = self.managed.parent_of(&root) {
            if !seen.insert(p.clone()) {
                break;
            }
            root = p;
            depth += 1;
        }
        self.run_token_key.mint(&shapewire::token::Claims {
            thread_id: thread_id.to_string(),
            run_id: format!("{thread_id}/{turn}"),
            root,
            depth,
            turn: turn.to_string(),
        })
    }

    /// Mint the turn's token and write its `--mcp-config` file, `0600`, at
    /// `<data_dir>/turns/<thread>/<cookie>.mcp.json` — beside the turn's
    /// durable stdout, never in the repo or the operator's home. Returns the
    /// token, and the file behind a guard that removes it when the turn ends.
    /// The file is `None` (and the turn still runs, with the token in its
    /// environment) when no `/mcp` URL can be named or the write fails; that is
    /// logged, never fatal — a turn that cannot reach the façade is still a
    /// turn.
    fn turn_mcp(&self, thread_id: &str, cookie: &str) -> (String, Option<TurnMcpConfig>) {
        let token = self.mint_turn_token(thread_id, cookie);
        let Some(url) = self.mcp_url() else {
            tracing::warn!(thread = %thread_id, "no /mcp address is known; this turn gets no --mcp-config");
            return (token, None);
        };
        let dir = self.data_dir.join("turns").join(thread_id);
        let path = dir.join(format!("{cookie}.mcp.json"));
        let body = crate::spawn::mcp_config_json(&url, &token).to_string();
        let written = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&dir)?;
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            use std::io::Write as _;
            o.open(&path)?.write_all(body.as_bytes())
        })();
        match written {
            Ok(()) => (token, Some(TurnMcpConfig(path))),
            Err(e) => {
                tracing::error!(
                    thread = %thread_id, path = %path.display(), error = %e,
                    "COULD NOT WRITE THIS TURN'S --mcp-config; the model cannot reach agents_* this turn"
                );
                (token, None)
            }
        }
    }

    /// **The login directory a turn spawns with: the POOL'S, as of now.**
    ///
    /// `ThreadState::config_dir` is resolved when the thread is placed or
    /// rehydrated, and a pool reload does not touch it. Claude Code keys its
    /// keychain item on the `CLAUDE_CONFIG_DIR` STRING, so when a re-sign-in
    /// re-registers a slot under a different string for the same directory
    /// (the live `accounts` symlink gives every slot two), the cached string
    /// names the DEAD credential while the pool — and the `/usage` probe —
    /// name the fresh one. 2026-10-15 19:18Z: three turns on `cc1` spawned
    /// with the stale string seconds after `LOGIN RESTORED` and all failed
    /// `authentication_failed`.
    ///
    /// So the pool's entry wins whenever the pool carries the account, and the
    /// cache is refreshed to match. An account the pool does not carry keeps
    /// the cached value, and [`Self::spawn_login`] judges it exactly as before.
    fn current_config_dir(&self, state: &ThreadState, account: &str) -> Option<String> {
        let mut cached = state.config_dir.lock().unwrap();
        if let Some(dir) = self.config_dir_for(account) {
            if cached.as_deref() != Some(dir.as_str()) {
                tracing::info!(
                    thread = %state.id,
                    account = %account,
                    "spawn login directory refreshed from the pool (it moved since placement)"
                );
                *cached = Some(dir);
            }
        }
        cached.clone()
    }

    /// **THE SPAWN GUARD: which login this turn is allowed to run as, or a
    /// refusal naming why not.** `Err` means NOTHING MAY BE SPAWNED.
    ///
    /// # The hazard this closes
    ///
    /// [`Self::config_dir_for`] answers `None` for any name the pool does not
    /// carry, `spawn.rs` turned `None` into `cmd.env_remove("CLAUDE_CONFIG_DIR")`,
    /// and an unset `CLAUDE_CONFIG_DIR` is `~/.claude` — **the operator's own
    /// terminal login**. So a turn that reached the spawn site holding a name
    /// the pool does not own ran AS HIM, successfully and silently: no error,
    /// no park, no ledger event, a turn that simply worked on the wrong
    /// account. It is the same door [`Self::probe_account`] slammed for
    /// `/usage`, one path over, and it is shut here for the same stated reason.
    ///
    /// `config_dir_for`'s docstring argues this cannot happen — *"through the
    /// pool there is no such name: an entry either names its login directory or
    /// is not an entry"* — and that argument is sound **only while the pool is
    /// immutable for the life of the process**, which it is today. It is an
    /// argument about PLACEMENT, and placement is not the spawn: a reloadable
    /// pool (the `PoolCell` work) breaks the implication "valid when placed" ⇒
    /// "valid when spawned", because a rename or a delete landing between
    /// admission and `cmd.spawn()` yields exactly the `None` above, and
    /// [`Self::resolve_account`] already takes several separate pool snapshots
    /// between choosing a name and resolving its directory. Nothing downstream
    /// catches it either: every AUI thread is UNGATED, and an ungated thread
    /// converts every gate refusal into "run anyway" (see [`Self::run_ungated`]
    /// and the `continuity` arm of `admit_or_park`).
    ///
    /// # THE PREDICATE, exactly
    ///
    /// > A turn may spawn with `CLAUDE_CONFIG_DIR` UNSET **if and only if its
    /// > account IS the machine default** — the name
    /// > [`crate::accounts::DEFAULT_ACCOUNT`], which is not a pool member.
    /// > Every other account must present a non-empty login directory, and a
    /// > missing one is REFUSED, never substituted.
    ///
    /// Note what it is NOT. It is not *"`None` is always wrong"*: `None` is the
    /// machine default's CORRECT answer — that account IS "the variable is
    /// unset" and it has no directory to name — and 151 of the 1,849 threads in
    /// the live store sit on it right now (`docs/spawn-config-dir-guard.md`
    /// carries the census). A guard that strict would park all 151 of them.
    /// [`Self::reresolve_account`] leaves them there deliberately rather than
    /// silently reassigning them, because DEC-56 forbids moving a thread's
    /// account without carrying its transcript; the admission gate evicts them
    /// BY PORTING on a later turn instead.
    ///
    /// Nor is it the looser *"a pool account must have a directory"*. That
    /// permits the hazard whole: a renamed or deleted account is no longer a
    /// pool member either, so the check would pass it straight through to the
    /// `None` arm — which is precisely the case the reloadable pool creates.
    /// The carve-out is therefore written as an IDENTITY (`name == default`),
    /// not as a category (`not placeable`).
    ///
    /// The second half of the carve-out — `!self.pool.contains(name)` — costs
    /// nothing today (no pool entry can be named `default`: an entry must name
    /// a login directory, and `accounts::rename_account` refuses the name in
    /// both directions) and keeps the guard honest if that ever changes. A
    /// `default` that IS a pool member is a pool member like any other and owes
    /// a directory; the machine default is the account that is *outside* the
    /// pool and names none. Both halves of that sentence are checked.
    ///
    /// # Why it is phrased as a REFUSAL and not a substitution
    ///
    /// There is no correct fallback available here, exactly as there is none in
    /// [`Self::probe_account`]. A turn needs a login to run as, and "the
    /// machine's" is the one answer that is forbidden. Substituting a healthy
    /// pool account instead would be a silent account move without its
    /// transcript — DEC-56's prohibition — so the turn PARKS with its prompt
    /// kept and the operator is told what happened.
    fn spawn_login(
        &self,
        account: &str,
        config_dir: Option<&str>,
    ) -> std::result::Result<SpawnLogin, String> {
        if let Some(login) = config_dir.and_then(SpawnLogin::pool_account) {
            return Ok(login);
        }
        // No directory. Exactly one account may spawn that way.
        // `pool()`, not `pool` — the pool became a `PoolCell` in the same merge
        // that brought this guard in, and the two landed on separate branches
        // that git reconciled textually without either one compiling against
        // the other. Reading a SNAPSHOT here is also the correct semantics: the
        // question is whether `default` is a pool member right now, and a
        // reload between this check and the spawn can only ever ADD names, not
        // make the machine default into one.
        if account == crate::accounts::DEFAULT_ACCOUNT && !self.pool().contains(account) {
            return Ok(SpawnLogin::machine_default());
        }
        Err(format!(
            "{account}: this thread's account resolves to no login directory, so there is \
             nothing to spawn it as. REFUSED rather than spawned without one — an unset \
             CLAUDE_CONFIG_DIR would run this turn against the operator's own terminal login \
             (~/.claude), which this daemon never does. Only the machine default may spawn \
             with none, and this thread is not on it. Either {account} left the pool (a \
             rename, a delete, a pool reload mid-turn) or it is a resource that is not a \
             Claude account at all."
        ))
    }

    /// The spawn guard's DECISION, reachable from an integration test.
    ///
    /// Public for the same reason [`Self::probe_account_for_test`] is, and the
    /// argument is verbatim that one's: the DECISION is what is worth testing,
    /// and proving it through the private path would require watching how
    /// `claude` was spawned — which, for the one case that matters, is the
    /// observation that is impossible to make safely. A test that could see the
    /// bad spawn would be a test that performed it.
    #[doc(hidden)]
    pub fn spawn_login_for_test(
        &self,
        account: &str,
        config_dir: Option<&str>,
    ) -> std::result::Result<Option<String>, String> {
        self.spawn_login(account, config_dir)
            .map(|l| l.config_dir().map(str::to_string))
    }

    /// Resolve an [`AccountChoice`] to `(name, config_dir)` for a thread that has
    /// no account yet — i.e. at CREATE time.
    ///
    /// `Auto` PLACES the thread. [`Self::selectable_pool`] — one label per
    /// distinct login, dead logins excluded — is the candidate set, and
    /// [`Budgets::place`] ranks it: the tightest weekly axis, then the session
    /// inside a deadband, then cash, then fewest threads placed / stay put /
    /// round-robin cursor / name. Every one of those is a real criterion; the
    /// arm used to be `.into_iter().next()`, i.e. **the alphabetically first**
    /// entry of that pool.
    ///
    /// That one line is why every thread on this machine lives on `default`.
    /// A thread's account is STICKY (see [`Self::reresolve_account`]), and
    /// stickiness holds whatever this function chose *forever* — so for an AUI
    /// thread, which carries no budget key and no thresholds and therefore
    /// never asks the gate to move it, this is the ONLY placement decision it
    /// will ever get. Stickiness over an arbitrary choice is just a durable
    /// arbitrary choice.
    ///
    /// Placement is NOT gating: it chooses among accounts and never refuses.
    /// So a placement that cannot be computed (the census read failed, say)
    /// degrades to the old alphabetical pick rather than failing thread
    /// creation, and the registry fallback below is untouched.
    ///
    /// **This is not re-resolution.** Once a thread has an account, use
    /// [`Self::reresolve_account`]: re-running this on rehydration or on a
    /// settings patch is what used to yank a failed-over thread back to the
    /// alphabetically first account and restart its conversation.
    fn resolve_account(
        &self,
        choice: &AccountChoice,
        thresholds: &Thresholds,
    ) -> Result<(String, Option<String>)> {
        let placed: Placeable = match choice {
            // A PIN is honoured only if it names somewhere threads may run.
            // Pinning the machine default is the one form of "placement" a
            // human can write by hand, so it is refused by hand here — and it
            // degrades to an ordinary auto placement rather than to an error,
            // because refusing to create the thread would turn a bad setting
            // into a dead daemon (CON-27).
            AccountChoice::Named(n) => match self.pool().get(n).map(|e| e.placeable()) {
                Some(p) => p,
                None => {
                    tracing::warn!(
                        account = %n,
                        "this thread is pinned to an account threads may not place on \
                         (the machine default is never placeable — CON-27); placing it \
                         automatically instead"
                    );
                    self.auto_place(thresholds)?
                }
            },
            AccountChoice::Auto => self.auto_place(thresholds)?,
        };
        let name = placed.into_name();
        let cd = self.config_dir_for(&name);
        Ok((name, cd))
    }

    /// Rank the placeable pool and answer with one account, or fail.
    ///
    /// **Every fallback in here is placeable too, and that is the point.** The
    /// last-ditch fallback used to be `self.registry.names().into_iter().next()`
    /// — the alphabetically first REGISTERED name, which on the operator's
    /// machine is literally `default`. A pool that went empty for any reason
    /// (every login dead, no readable identity) therefore handed threads
    /// straight onto his own terminal login. It now falls back to the
    /// alphabetically first PLACEABLE account, and when there is none it
    /// ERRORS rather than reaching for a name that is merely present.
    fn auto_place(&self, thresholds: &Thresholds) -> Result<Placeable> {
        let pool = self.selectable_pool();
        self.budgets
            .place(&PlacementRequest {
                thresholds,
                candidates: &pool,
                // A thread with no account yet has nothing to stay put
                // for, and this is exactly the "genuinely new
                // placement" the round-robin cursor exists to spread.
                prefer: None,
                // A thread being created holds no conversation anywhere, so
                // the continuity tie-break has nothing to weigh — every
                // candidate is equally cold and the criterion cancels out.
                sessions: &[],
                // A thread being created has no history to estimate from, so
                // the default demand is the honest answer: one thread, no
                // subagents, a nominal turn. It is identical across every
                // candidate, so it cancels out of the ordering entirely and the
                // reachable budget decides on its own.
                demand: Demand::default(),
            })
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "placement failed at create time; falling back to pool order"
                );
                None
            })
            .or_else(|| pool.first().cloned())
            // Every login is dead (or none has a readable identity): fall back
            // to the placeable registry rather than refusing to create the
            // thread. An unusable pool must not also become an unusable daemon;
            // admission and failover report the real problem.
            .or_else(|| self.pool().placeable().into_iter().next())
            .ok_or_else(|| {
                Error::Config(
                    "no placeable account is registered: threads runs on accounts that have \
                     their own config dir, never on the machine default (CON-27)"
                        .into(),
                )
            })
    }

    /// Re-resolve the account of a thread that ALREADY HAS ONE — on rehydration
    /// (every daemon restart) or on a settings patch (every `threads.update`).
    ///
    /// A thread's account is STICKY. `Auto` means "the daemon chose for me", not
    /// "choose again every time you look at me": the claude session for a thread
    /// id lives inside one account's config dir, so moving the thread restarts
    /// its conversation. Re-resolution was doing exactly that — a thread that had
    /// failed over to account B was pulled back to the alphabetically first
    /// account on the next daemon restart and on the next settings patch of any
    /// kind (a model change is a `threads.update`, and the audio interface makes
    /// those routinely). That contradicts DEC-26, park-for-continuity, which the
    /// admission gate implements correctly one layer away.
    ///
    /// So `Auto` keeps the established account for as long as it is still
    /// registered. Stickiness breaks on exactly one condition — the account is no
    /// longer in the registry, where staying would mean spawning with a config dir
    /// that does not exist. Notably it does NOT break on a dead or rate-limited
    /// login, and the reason is ownership rather than cost: deciding to move (or
    /// to park instead) belongs to the admission gate, which is the only place
    /// that knows whether the target has a session for this thread and can port
    /// the conversation to it. Re-resolution here would move the account column
    /// WITHOUT carrying the transcript, which is a different and worse thing than
    /// a move.
    ///
    /// (This used to say that moving a thread with history is "the more expensive
    /// mistake". That justification is DELETED, not softened — INTENT 83, idx
    /// `2951`: *"the justification you used that it's going to be more expensive
    /// to move it if it hits the session limit, that's not true because you're
    /// gonna move it beforehand anyway."* The move happens either way and the
    /// transcript ports either way; no extra cost accrues from the limit being
    /// hit. The mechanism below is unchanged — only the reason for it is, and it
    /// had to change because a placement built on a false premise is a placement
    /// nobody can reason about.)
    ///
    /// A PIN is still absolute (rank 1): `Named` always re-resolves to its name,
    /// which is the self-healing case the old comment described and the only one
    /// it was right about.
    fn reresolve_account(
        &self,
        choice: &AccountChoice,
        established: Option<&str>,
        thresholds: &Thresholds,
    ) -> Result<(String, Option<String>)> {
        if matches!(choice, AccountChoice::Auto) {
            if let Some(current) = established.filter(|a| !a.is_empty()) {
                // THE CONDITION USED TO BE `self.registry.get(current).is_some()`,
                // i.e. stickiness broke for an account that had left the
                // registry. When the machine default stopped being registered
                // (2026-08-27) that condition started firing on every thread
                // stranded on it — 119 of them on the operator's machine — and
                // moved their account column at boot WITHOUT carrying their
                // transcripts. That is precisely the silent reassignment DEC-56
                // forbids, and it would have been done in bulk, at boot, to
                // conversations he still has.
                //
                // So stickiness is now unconditional for `Auto`, and the
                // justification the old condition gave ("staying would mean
                // spawning with a config dir that does not exist") is answered
                // one layer up rather than here: the gate cannot admit a thread
                // on an account outside the pool, so it parks for continuity
                // and `failover_target` moves it BY PORTING. Nothing spawns on
                // an unresolvable account, because nothing spawns without
                // passing the gate first.
                {
                    // CON-27, and the distinction is the whole reason boot
                    // recovery is safe: STAYING somewhere is not being PLACED
                    // there. A thread already sitting on the machine default
                    // keeps sitting on it across this restart, because moving it
                    // here would move its account column WITHOUT carrying its
                    // transcript — silent reassignment, which DEC-56 forbids and
                    // which is how a conversation gets stranded. It is evicted
                    // one layer up instead: the admission gate cannot admit it
                    // where it is (the machine default is not in the gate's
                    // pool), so it parks for continuity and `failover_target`
                    // moves it by PORTING. Boot recovery therefore never assigns
                    // the machine default to anything — it only declines to
                    // strand a thread that is already there.
                    if !self.is_placeable(current) {
                        tracing::warn!(
                            account = %current,
                            "this thread is sitting on an account threads may not place on \
                             (CON-27); leaving it there for now — the admission gate moves it \
                             off by porting its conversation on its next turn"
                        );
                    }
                    return Ok((current.to_string(), self.config_dir_for(current)));
                }
            }
        }
        self.resolve_account(choice, thresholds)
    }

    /// May threads place a thread on this account name? (CON-27.)
    ///
    /// Reporting only — a `bool` cannot be handed to placement, which takes
    /// [`Placeable`]. Nothing may branch on this to DECIDE a destination; use
    /// [`Self::placement_pool`] for that, which cannot express a wrong answer.
    pub fn is_placeable(&self, name: &str) -> bool {
        self.pool().contains(name)
    }

    /// The account this thread is being served on right now (in-memory truth,
    /// mirrored durably in `threads.account`).
    pub fn serving_account(&self, id: &str) -> Result<String> {
        let state = self.get_state(id)?;
        let account = state.account.lock().unwrap().clone();
        Ok(account)
    }

    /// Create a thread. `thread_id` is ADOPTED when supplied (Q2 — this is
    /// what lets AUI's existing sessions keep working); minted when absent.
    /// Emits `ThreadStarted`. Does not run a turn.
    pub fn create_thread_acked(
        &self,
        thread_id: Option<String>,
        settings: ThreadSettings,
    ) -> Result<(String, shapewire::command::Accepted)> {
        let id = thread_id
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if self.store.get_thread(&id)?.is_some() {
            return Err(Error::Config(format!("thread already exists: {id}")));
        }
        // DOOR ONE used to refuse an incoherent injection combination here
        // (`ensure_context_id_coherent`). With one flag per injected thing and
        // the metadata block DERIVED from its lines, no combination means
        // nothing, so there is nothing to refuse and the check is gone rather
        // than relaxed. `crate::settings::Injection` records why.
        // R4.S4b (HOLDS R17): the same door as `update_settings`.
        crate::ledger::turn::validate_allow_loss(&settings.projection_allow_loss).map_err(|message| {
            Error::Refused {
                code: shapewire::envelope::codes::MALFORMED,
                message,
            }
        })?;
        let (account, config_dir) = self.resolve_account(&settings.account, &settings.thresholds)?;
        let now = crate::store::now_rfc3339();
        self.store.upsert_thread(&ThreadRow {
            id: id.clone(),
            account: Some(account.clone()),
            cwd: Some(settings.cwd.clone()),
            model: Some(settings.model.clone()),
            settings_json: serde_json::to_string(&settings)?,
            last_seq: 0,
            next_message_id: 1,
            last_compaction_message_id: 0,
            needs_restate: false,
            created_at: now.clone(),
            updated_at: now,
        })?;
        let state = Arc::new(ThreadState {
            id: id.clone(),
            settings: StdMutex::new(settings.clone()),
            account: StdMutex::new(account.clone()),
            config_dir: StdMutex::new(config_dir),
            seq: AtomicU64::new(0),
            next_message_id: AtomicU64::new(1),
            resumable: AtomicBool::new(false),
            turn_running: AtomicBool::new(false),
            live: StdMutex::new(Liveness::default()),
            cancel_requested: AtomicBool::new(false),
            open_attempt: StdMutex::new(None),
            attempt_hold: StdMutex::new(None),
            plugin_snapshot: StdMutex::new(Default::default()),
            plugins_dirty: AtomicBool::new(false),
            turn_charged: StdMutex::new(HashMap::new()),
            turn_cost_charged: StdMutex::new(HashMap::new()),
            charged_message_ids: StdMutex::new(HashSet::new()),
            budgets_failure_reported: AtomicBool::new(false),
            budget_kill: AtomicBool::new(false),
            kill_trip: StdMutex::new(None),
            limit_kill: AtomicBool::new(false),
            compaction_stop: AtomicBool::new(false),
            limit_kill_signal: StdMutex::new(None),
            park_reason: StdMutex::new(None),
            projection_hold: StdMutex::new(None),
            turn_terminal: StdMutex::new(Default::default()),
        });
        self.threads.lock().unwrap().insert(id.clone(), state.clone());
        // ── WHERE `threads/create` RESOLVES, AND WHY IT IS THIS LINE ─────────
        //
        // **The promise is opened HERE and not by the handler**, because the
        // subject is minted at the top of this function and the handler has no
        // way to name it before the act that names it. A promise bound to the
        // wrong subject is the bug this whole change deletes; binding it where
        // the subject is born is the one place it cannot be got wrong.
        //
        // **And it rides `ThreadStarted` and not the status edge below**, which
        // the first cut of this got wrong and the test caught. `Threads::folds`
        // records `tx.rows.member(id)` before every `holds` guard, and `added`
        // fires ONCE per key — on the FIRST input that names it. That input is
        // this ledger event, not the `emit_status` three statements down, so a
        // resolve attached to the status found a membership statement that had
        // already been made and silently rode nothing.
        let promise = self
            .promises
            .open::<crate::commands::ThreadsCreate>(Some(id.clone()));
        let started = ThreadEvent::ThreadStarted {
            thread_id: id.clone(),
            account,
            cwd: settings.cwd.clone(),
            model: settings.model.clone(),
        };
        let visible = if self.hub.any_tracking() {
            // The resolve is an ARGUMENT of the emit, never a statement above it.
            self.emit_resolving(
                &state,
                None,
                started,
                self.promises
                    .resolve::<crate::commands::ThreadExistsFact>(Some(&id)),
            );
            Visible::Expected
        } else {
            // No fold, so no `added`, so no carrier. The legacy frame still goes
            // out — it always did — and the id is ABANDONED rather than closed
            // in silence.
            self.emit(&state, None, started);
            let _ = self
                .promises
                .abandon_all::<crate::commands::ThreadExistsFact>(Some(&id));
            Visible::Never
        };
        // ── SILENT WRITER 1: A BRAND-NEW THREAD PRODUCED NO STATUS EDGE ──
        //
        // `upsert_thread` above wrote the row and `ThreadStarted` announced the
        // OCCURRENCE; nothing said what the thread's state now IS. A client that
        // creates a thread and then waits hears nothing at all until its first
        // send, which is why `threads.create` callers poll.
        //
        // Legacy-visible and ADDITIVE, in exactly the way `move_thread`'s own
        // `emit_status` already is (`docs/shapes.md` §5 records that as "the one
        // legacy-visible change, additive"): one extra `status` frame per thread
        // creation, of a type every consumer already renders.
        //
        // **AND NO `emit_thread_facts` HERE, WHICH IS A FINDING RATHER THAN AN
        // OMISSION.** `shapes::threads` is `KeyScope::OnDemand` and its fold
        // mints no key, so a key exists only because a `shapes.subscribe` asked
        // for it — and a subscribe naming a thread that does not exist yet is
        // REFUSED. So at this instant no consumer can hold this thread's key, no
        // fold can be holding a cell for it, and the first consumer to name it
        // reads these three values out of the store in `hydrates_key`. A fact
        // emitted here could never be folded by anything: it would be an emit
        // that looks like it works, which `core:stub-doctrine` and
        // `docs/shapes.md` both bar. The write is announced by the edge and
        // hydrated by the subscribe.
        //
        // ── THAT PARAGRAPH IS NOW HALF WRONG (2026-09-28) ───────────────
        //
        // The half that changed is MEMBERSHIP. "A fact emitted here could never
        // be folded by anything" is still true of the KEY — nobody can hold a
        // key that did not exist a moment ago. It is no longer true of the POOL:
        // `Threads::folds` records `tx.rows.member(id)` BEFORE every `holds`
        // guard, so the ledger event above yields one `Derived::Added`, the pool
        // telling a `detail: keys` subscriber that this thread exists. That is
        // exactly the fact `threads/create` causes, and it is the ONLY frame
        // this key produces while nobody holds it — which is why `hub.rs`'s
        // `added` loop carries promise ids now, as `gone` always has.
        //
        // What is still NOT emitted here is `emit_thread_facts`, and for the
        // unchanged reason: those are ROW cells, the fold's `holds` guard drops
        // them for a key nobody holds, and the first consumer to name the key
        // reads them out of the store in `hydrates_key`.
        self.emit_status(&id);
        // `changed` is unconditionally true: this function REFUSES an id that
        // already exists, so there is no re-assertion path to report.
        Ok((id, promise.accepted(visible).changed(true)))
    }

    /// [`Self::create_thread_acked`] for the many callers that want the id and
    /// nothing else — the CLI, the importer, and every test that builds a
    /// fixture. It drops the acknowledgement on the floor, which is correct for
    /// a caller that is not a consumer: nobody is waiting on a promise it never
    /// saw, and `emit_thread_exists` closes it either way.
    pub fn create_thread(
        &self,
        thread_id: Option<String>,
        settings: ThreadSettings,
    ) -> Result<String> {
        Ok(self.create_thread_acked(thread_id, settings)?.0)
    }

    /// Look up (or rehydrate from the ledger) a thread by id.
    fn get_state(&self, id: &str) -> Result<Arc<ThreadState>> {
        if let Some(s) = self.threads.lock().unwrap().get(id) {
            return Ok(s.clone());
        }
        let row = self
            .store
            .get_thread(id)?
            .ok_or_else(|| Error::Config(format!("no such thread: {id}")))?;
        let settings = ThreadSettings::from_stored(&row.settings_json)?;
        let max_seq = self.store.max_seq(id)?;
        // Self-heal the message counter from the ledger (imports may have
        // written rows directly).
        let next_msg = row.next_message_id.max(self.store.max_message_id(id)? + 1);
        let (account, config_dir) = self
            .reresolve_account(
                &settings.account,
                row.account.as_deref(),
                &settings.thresholds,
            )
            .unwrap_or((row.account.clone().unwrap_or_default(), None));
        // IS-A, second half: rehydration re-resolves the thread's account from
        // its settings, so a restart can legitimately move a thread off the
        // account the column recorded — but ONLY for a reason that outranks
        // continuity: a pinned thread goes back to its pin, and a thread whose
        // account has been un-registered has to go somewhere. An `Auto` thread
        // STAYS PUT (see `reresolve_account`). Write the result back either way,
        // or the column would go stale in the other direction and the durable
        // answer would stop mirroring `ThreadState`.
        let account_drifted = row.account.as_deref() != Some(account.as_str());
        let state = Arc::new(ThreadState {
            id: id.to_string(),
            settings: StdMutex::new(settings),
            account: StdMutex::new(account),
            config_dir: StdMutex::new(config_dir),
            seq: AtomicU64::new(max_seq),
            next_message_id: AtomicU64::new(next_msg),
            resumable: AtomicBool::new(max_seq > 0),
            turn_running: AtomicBool::new(false),
            live: StdMutex::new(Liveness::default()),
            cancel_requested: AtomicBool::new(false),
            open_attempt: StdMutex::new(None),
            attempt_hold: StdMutex::new(None),
            plugin_snapshot: StdMutex::new(Default::default()),
            plugins_dirty: AtomicBool::new(false),
            turn_charged: StdMutex::new(HashMap::new()),
            turn_cost_charged: StdMutex::new(HashMap::new()),
            charged_message_ids: StdMutex::new(HashSet::new()),
            budgets_failure_reported: AtomicBool::new(false),
            budget_kill: AtomicBool::new(false),
            kill_trip: StdMutex::new(None),
            limit_kill: AtomicBool::new(false),
            compaction_stop: AtomicBool::new(false),
            limit_kill_signal: StdMutex::new(None),
            park_reason: StdMutex::new(None),
            projection_hold: StdMutex::new(None),
            turn_terminal: StdMutex::new(Default::default()),
        });
        if account_drifted {
            let a = state.account.lock().unwrap().clone();
            self.persist_thread_account(&state, &a);
        }
        self.threads
            .lock()
            .unwrap()
            .insert(id.to_string(), state.clone());
        Ok(state)
    }

    /// Apply a partial settings update mid-thread (Q9/Q10). Takes effect on
    /// the next spawn. Emits `SettingsChanged` with the applied patch.
    /// **AND THE FACTS BELOW ARE WHERE `threads/{thread_id}/update` RESOLVES**
    /// (rule 2.4, 2026-09-28). The reply used to be `{thread_id, applied}` —
    /// the applied settings, which is the same blob `Fact::ThreadSettings`
    /// carries and `ThreadCells::settings` holds. `applied` is still returned to
    /// the crate's own callers; what changed is that it no longer rides the wire.
    pub fn update_settings(&self, id: &str, patch: SettingsPatch) -> Result<(Value, Visible)> {
        let state = self.get_state(id)?;
        // Did this patch actually say anything about the account? A patch that
        // did not is not permission to move the thread — see
        // `reresolve_account`. AUI patches settings for routine things (a model
        // change is a `threads.update`), and re-picking the account on each of
        // them silently restarted the conversation of every failed-over thread.
        let account_patched = patch.account.is_some();
        // R4.S4b (HOLDS R17): an allowed-loss name no render can meet allows
        // nothing — refused here, to whoever typed it, before anything moves.
        if let Some(names) = &patch.projection_allow_loss {
            crate::ledger::turn::validate_allow_loss(names).map_err(|message| Error::Refused {
                code: shapewire::envelope::codes::MALFORMED,
                message,
            })?;
        }
        let (applied, snapshot) = {
            let mut s = state.settings.lock().unwrap();
            // DOOR TWO used to build a PROSPECTIVE blob here and refuse it if
            // the injection settings composed to something meaningless. Flat
            // flags have no such composition, so the probe and the refusal are
            // both gone — see `crate::settings::Injection`.
            let applied = s.apply(patch);
            (applied, s.clone())
        };
        let established = state.account.lock().unwrap().clone();
        let resolved = if account_patched {
            self.resolve_account(&snapshot.account, &snapshot.thresholds)
        } else {
            self.reresolve_account(
                &snapshot.account,
                Some(established.as_str()),
                &snapshot.thresholds,
            )
        };
        if let Ok((account, cd)) = resolved {
            *state.account.lock().unwrap() = account;
            *state.config_dir.lock().unwrap() = cd;
        }
        // F4, third site: `update_settings` WRITES `state.account` and emitted
        // only `SettingsChanged`, so a settings-driven placement change was
        // invisible on the status stream. Emitted only when the account
        // actually moved — a settings change that does not touch placement is
        // not a placement edge and must not look like one.
        let account_moved = *state.account.lock().unwrap() != established;
        self.store.update_thread_settings(
            id,
            &serde_json::to_string(&snapshot)?,
            Some(state.account.lock().unwrap().as_str()),
            &snapshot.cwd,
            &snapshot.model,
        )?;
        if account_moved {
            self.emit_status(id);
        }
        // ── SILENT WRITER 2: THE BLOB MOVED AND ONLY THE PATCH WAS SAID ──
        //
        // `update_thread_settings` above replaced `settings_json`, `cwd`, `model`
        // and (when placement moved) `account`. The only announcement was the
        // `SettingsChanged` event below, which carries the PATCH — what changed,
        // not what the settings ARE. A `Register<Value>` fed that would hold a
        // patch. So the facts go out here, carrying the blob as
        // `thread_json` renders it.
        let visible = self.emit_thread_facts(id);
        self.emit(
            &state,
            None,
            ThreadEvent::SettingsChanged {
                patch: applied.clone(),
            },
        );
        // A settings change can retire the very block that parked this thread
        // (a raised budget, a cleared threshold, a re-pointed key) — re-check
        // immediately rather than waiting for the resume tick.
        self.maybe_dispatch(&state);
        Ok((applied, visible))
    }

    /// **THE OPERATOR'S CANCEL: `id` AND ITS WHOLE MANAGED SUBTREE (R9.N1,
    /// gap N-1).**
    ///
    /// A parent he cancels is usually a parent WAITING on its children, and
    /// cancelling it alone left them running on its budget key — and when the
    /// last one ended, its report woke the parent into a turn he had just
    /// told it not to take. So:
    ///
    /// 1. [`crate::managed::begin_subtree_cancel`] walks the subtree and, on
    ///    every member that is owed children, durably records that the wake
    ///    those children's reports would queue is SUPPRESSED (a `system` fact
    ///    on that member's own ledger, which `deliver_due_wakes` honours at
    ///    run time and at boot). It is written BEFORE any descendant is
    ///    cancelled, so no cascade report can race ahead of it.
    /// 2. `id`, then every descendant, is cancelled through the one
    ///    single-thread path ([`ThreadManager::cancel_turn`]) — a running turn
    ///    is cut, a retry ladder or a parked turn is dropped (R9.L3), and each
    ///    child's run ends `cancelled` and is reported to its parent as usual.
    ///    Only the WAKE is withheld; the report itself is written, so
    ///    `agents_result` / `agents_list` read the truth.
    ///
    /// 3. **Queued work is DEFERRED, never dropped (R10.M2, L-8, E21-4).**
    ///    Before each member's turn is cut, every message still waiting on its
    ///    queue — the operator's own follow-ups, mail, a wake queued before
    ///    the cancel — is set `ride` through [`crate::managed::ride_queued`]
    ///    (R9.N1b's walk, with `ride` in place of withdraw): it no longer
    ///    starts a turn, and it goes out, oldest first, ahead of whatever
    ///    starts the next one. Riding first means a running member's epilogue
    ///    finds nothing to dispatch. A member whose whole run was that queue —
    ///    parked at the admission gate, nothing executing, no retry or parked
    ///    row for `cancel_turn` to defer — gets `cancel_turn`'s parked-turn
    ///    end (R9.L3) here: the in-memory park cleared, one operator
    ///    `turn_interrupted`, and its run ends `cancelled` to its parent.
    ///
    /// A child that ended BEFORE the cancel was reported then and is not
    /// touched. Only the operator's explicit `queue_cancel` on one message
    /// withdraws it; a cancel never does.
    pub fn cancel(&self, id: &str) -> Result<()> {
        self.get_state(id)?;
        let descendants = crate::managed::begin_subtree_cancel(self, id);
        self.cancel_member(id)?;
        for d in &descendants {
            if let Err(e) = self.cancel_member(d) {
                tracing::warn!(thread = %id, descendant = %d, error = %e, "cancel: a managed descendant could not be cancelled");
            }
        }
        if !descendants.is_empty() {
            tracing::info!(thread = %id, descendants = descendants.len(), "the operator cancelled a thread and its managed subtree");
        }
        Ok(())
    }

    /// **DEFER, NEVER DROP (R10.M2, L-8, E21-4): set `message_ids`' rows on
    /// `id` back on the queue `ride`, and record it on the ledger.**
    ///
    /// The one door every operator-cancel path takes — an open attempt, a
    /// retry ladder's or a parked turn's rows, and the cascade's waiting rows
    /// ([`crate::managed::ride_queued`]). A row moves only from a state that
    /// still owes the message (`attempted`/`delivered` that a turn did not
    /// finish, or `waiting` `eager`/`turn_end`); `failed` and a withdrawn row
    /// (`never`) are left exactly as they are, because only the operator's
    /// explicit re-select or withdraw moves those.
    ///
    /// The ledger fact is written in the SAME transaction as the move:
    /// - rows that were in an attempt end it — `delivery_cancelled{attempt,
    ///   message_ids, reason, dispatch_now: ride}`, naming the attempt the
    ///   rows were last in (`null` for rows attempted before R10.M1);
    /// - rows that were only waiting have no attempt to end; their
    ///   whether-to-send changed, so `dispatch_changed{message_ids, reason,
    ///   dispatch_now: ride}` (§5.1: every change of it is a ledger event).
    ///
    /// `in_attempt`: the rows were attempted (an open attempt, or the rows a
    /// retry ladder or parked turn holds) rather than only waiting. Returns
    /// how many rows moved.
    pub(crate) fn defer_to_ride(&self, id: &str, message_ids: &[u64], in_attempt: bool, reason: &str) -> usize {
        use crate::event::delivery;
        if message_ids.is_empty() {
            return 0;
        }
        let Ok(state) = self.get_state(id) else {
            return 0;
        };
        // Name only the rows that will move, so the event never claims a
        // message that was already delivered or withdrawn.
        let owed = match self.store.rideable(id, message_ids) {
            Ok(owed) => owed,
            Err(e) => {
                tracing::error!(thread = %id, error = %e, "cancel: could not read which rows are still owed; deferring all named");
                message_ids.to_vec()
            }
        };
        if owed.is_empty() {
            tracing::info!(thread = %id, ?message_ids, "cancel: no row left to defer — the messages were already delivered or withdrawn");
            return 0;
        }
        let message_ids = &owed[..];
        let (event_type, data) = if in_attempt {
            let atts = self.store.last_attempt_ids(id, message_ids).unwrap_or_default();
            let attempt = message_ids.iter().find_map(|m| atts.get(m).cloned());
            (
                delivery::CANCELLED,
                json!({"attempt": attempt, "message_ids": message_ids, "reason": reason, "dispatch_now": delivery::RIDE}),
            )
        } else {
            (
                DISPATCH_CHANGED,
                json!({"message_ids": message_ids, "reason": reason, "dispatch_now": delivery::RIDE}),
            )
        };
        let ev = Some(self.sequenced(&state, event_type, data));
        match self.store.queue_ride(id, message_ids, ev.as_ref()) {
            Ok(moved) => {
                if moved > 0 {
                    if let Some(ev) = ev {
                        self.published(&state, ev);
                    }
                    self.emit_thread_queue(crate::clock::now(), id, false);
                }
                moved
            }
            Err(e) => {
                tracing::error!(thread = %id, error = %e, ?message_ids, "CANCEL COULD NOT DEFER ITS MESSAGES TO RIDE — they stay where they were");
                0
            }
        }
    }

    /// One member of an operator cancel (see [`Self::cancel`], item 3): ride
    /// its queue, cut its turn, and end a run that was only queued work.
    fn cancel_member(&self, t: &str) -> Result<()> {
        let state = self.get_state(t)?;
        let delivery_waiting = self.store.get_retry(t).ok().flatten().is_some()
            || self.store.get_parked_turn(t).ok().flatten().is_some();
        let ridden = crate::managed::ride_queued(self, t);
        self.cancel_turn(t)?;
        if ridden == 0
            || delivery_waiting
            || state.turn_running.load(Ordering::SeqCst)
            || !self.is_idle(t).unwrap_or(false)
        {
            return Ok(());
        }
        // Nothing executes and nothing waits on a ladder or a park: the riders
        // were this member's whole run. Only a member the gate HELD had a run
        // in flight to end; an idle member with mail queued behind nothing
        // owes no interrupt.
        if state.park_reason.lock().unwrap().take().is_none() {
            return Ok(());
        }
        self.release_own_kill_latch(t);
        state.projection_hold.lock().unwrap().take();
        self.emit(
            &state,
            None,
            ThreadEvent::TurnInterrupted {
                reason: "the operator interrupted this turn".into(),
                trigger: InterruptTrigger::Operator.as_str().into(),
            },
        );
        let terminal = std::mem::take(&mut *state.turn_terminal.lock().unwrap());
        crate::managed::turn_finished(self, t, terminal);
        self.emit_status(t);
        Ok(())
    }

    /// **TEST SEAM (R10.M2): hold the next queue drain on `id` with its
    /// attempt OPEN** — rows marked attempted, `turn_started` not yet emitted —
    /// until [`Self::release_attempt_for_test`]. The window it widens is real:
    /// the dispatch task awaits `edge_poll_open` there.
    #[doc(hidden)]
    pub fn hold_attempts_for_test(&self, id: &str) -> Result<()> {
        let state = self.get_state(id)?;
        *state.attempt_hold.lock().unwrap() = Some(Arc::new(tokio::sync::Notify::new()));
        Ok(())
    }

    /// Release a [`Self::hold_attempts_for_test`] hold (the waiting drain, or
    /// the next one, proceeds) and remove it.
    #[doc(hidden)]
    pub fn release_attempt_for_test(&self, id: &str) -> Result<()> {
        let state = self.get_state(id)?;
        if let Some(h) = state.attempt_hold.lock().unwrap().take() {
            h.notify_one();
        }
        Ok(())
    }

    /// The message ids of `id`'s open attempt, if a drain holds one.
    #[doc(hidden)]
    pub fn open_attempt_for_test(&self, id: &str) -> Result<Option<Vec<u64>>> {
        Ok(self.get_state(id)?.open_attempt.lock().unwrap().clone())
    }

    /// Request cancellation of ONE thread's turn (level-triggered, G14) —
    /// running, retrying or parked — and nothing below it. The operator's door
    /// is [`ThreadManager::cancel`], which runs this over a whole subtree; an
    /// `interrupt` mail (`networks::deliver`) cuts only its recipient's turn,
    /// because the recipient goes straight on to read the mail and may still
    /// want its own children's reports.
    pub fn cancel_turn(&self, id: &str) -> Result<()> {
        let reason = "operator_cancel";
        let state = self.get_state(id)?;
        state.cancel_requested.store(true, Ordering::SeqCst);
        // ── A CANCEL ENDS THE OPEN ATTEMPT UNDELIVERED (R10.M2, L-8) ────────
        //
        // A queue drain marked its rows attempted and handed them to a
        // dispatch task that has not yet emitted `turn_started` (it is still
        // awaiting the opening `/usage` edge). Nothing has reached the
        // process, so nothing was delivered: the attempt ends
        // `delivery_cancelled{operator_cancel}` and its rows go back on the
        // queue `ride`, to go out with the next turn. Taking the attempt is
        // what stops the turn — the dispatch task finds it gone and ends
        // through `finish_turn` without spawning. No `run_once` will run to
        // record the interrupt, so it is recorded here.
        let open = state.open_attempt.lock().unwrap().take();
        if let Some(ids) = open {
            self.defer_to_ride(id, &ids, true, reason);
            self.emit(
                &state,
                None,
                ThreadEvent::TurnInterrupted {
                    reason: "the operator interrupted this turn".into(),
                    trigger: InterruptTrigger::Operator.as_str().into(),
                },
            );
            tracing::info!(thread = %id, messages = ids.len(), "the operator cancelled an OPEN delivery attempt — its messages ride the next turn");
        }
        // ── A CANCEL ALSO ENDS A DELIVERY THAT IS ONLY *WAITING* ─────────────
        //
        // **Found by mutation testing on 2026-08-25, and it is a real gap that
        // this wave makes considerably worse.**
        //
        // `cancel_requested` is a flag the TAIL LOOP polls, so it only reaches a
        // delivery that is executing right now. A delivery sitting on a retry
        // ladder is not executing — it is an armed timer and a `retries` row —
        // and the flag never reached it. Worse, `run_once` clears
        // `cancel_requested` on entry, so when the timer did fire, the pending
        // cancellation was consumed and discarded and the turn ran anyway.
        //
        // That was survivable when almost nothing retried. Under this wave's
        // rule almost everything does: a deterministic failure now sits on a
        // ladder for eighteen minutes where it used to end immediately. Leaving
        // this would mean the operator cancels a thread, watches it stop, and
        // sees it spawn again minutes later — the daemon arguing with the person
        // who told it to stop, which is the exact inversion of the rule.
        //
        // So the LADDER is dropped here. `thread_status` derives `retrying`
        // purely from that row's existence, so the thread leaves `retrying` at
        // once and the armed timer finds nothing to fire (`fire_retry` returns
        // early when the row is gone — it is already written to tolerate being
        // superseded).
        //
        // **The MESSAGES are not dropped (R10.M2, L-8, E21-4 — this reverses
        // the rule that stood here).** The ladder's message rows go back on
        // the queue `ride` BEFORE the row is cleared (a kill between the two
        // leaves a second copy, never none): not re-fired — he said stop — but
        // carried, oldest first, by whatever starts the next turn.
        let mut dropped_waiting = false;
        if let Some(retry) = self.store.get_retry(id).ok().flatten() {
            // R13.1 (closes R10.M2-1): only what never reached the model rides.
            // A ladder row lives on through its re-fired run, so a cancel that
            // lands on a RUNNING retried attempt — or on a ladder whose earlier
            // attempt was answered — used to ride messages the model already
            // had: re-sent on the next turn, under a `delivery_cancelled` that
            // claimed they were never delivered.
            let owed = self.undelivered_of(id, &retry.message_ids);
            self.defer_to_ride(id, &owed, true, reason);
            self.store.clear_retry(id).ok();
            tracing::info!(
                thread = %id,
                "the operator cancelled a WAITING delivery — its armed retry ladder is dropped \
                 rather than left to fire minutes later; its messages ride the next turn"
            );
            dropped_waiting = true;
        }
        // ── …AND A DELIVERY THAT IS *PARKED* (R9.L3, gap L-3) ────────────────
        //
        // A parked turn is the same shape as a retry: not executing, so the
        // flag never reaches it, and a durable `parked_turns` row that
        // `maybe_dispatch` redelivers the moment the block lifts — a budget
        // raised, a limit window lapsing — hours after the operator said stop,
        // and at his cost. So the row goes, and with it everything that kept
        // the thread parked in memory: the park reason (or every screen keeps
        // showing `parked`), the budget-kill latch (or a later message is
        // refused by a kill he already cancelled) and the projection hold.
        //
        // Removing the row is also what lets a parked MANAGED child end: its
        // run was not idle while the row existed (`managed::idle`), so its
        // parent was never told and waited on it forever. The report below now
        // finds it idle and delivers `cancelled`.
        if let Some(parked) = self.store.get_parked_turn(id).ok().flatten() {
            // R10.M2: its messages ride the next turn (as the ladder's do) —
            // those the model never saw (R13.1, as the ladder's arm above).
            let owed = self.undelivered_of(id, &parked.message_ids);
            self.defer_to_ride(id, &owed, true, reason);
            if let Err(e) = self.store.clear_parked_turn(id) {
                tracing::error!(thread = %id, error = %e, "CANCEL COULD NOT DROP THE PARKED TURN");
            } else {
                tracing::info!(
                    thread = %id,
                    "the operator cancelled a PARKED delivery — it is not redelivered when the \
                     block lifts; its messages ride the next turn"
                );
            }
            state.park_reason.lock().unwrap().take();
            // R9.L4: the latch is per KEY now, so the cancel releases it for
            // THIS thread's own park only. A sibling held by the same kill
            // stays held — the operator cancelled one prompt, not the budget.
            self.release_own_kill_latch(id);
            state.projection_hold.lock().unwrap().take();
            dropped_waiting = true;
        }
        if dropped_waiting {
            // Only when no execution is live: a running turn emits its own
            // `turn_interrupted` from `run_once`, and two would be a lie about
            // how many things were interrupted.
            if !state.turn_running.load(Ordering::SeqCst) {
                self.emit(
                    &state,
                    None,
                    ThreadEvent::TurnInterrupted {
                        reason: "the operator interrupted this turn".into(),
                        trigger: InterruptTrigger::Operator.as_str().into(),
                    },
                );
                // No execution is live, so no `finish_turn` will run: this
                // cancel IS the run's end. A managed child's parent is told
                // now, `cancelled` (H179, `docs/SUBAGENT-WAKE.md`).
                let terminal = std::mem::take(&mut *state.turn_terminal.lock().unwrap());
                crate::managed::turn_finished(self, id, terminal);
            }
            self.emit_status(id);
        }
        Ok(())
    }

    // ── THE LOCK: the one door into an execution (msg #1213) ─────────────

    /// Claim `thread_id` for an execution, or refuse.
    ///
    /// This is the ONLY constructor of [`TurnLock`], and `run_turn` /
    /// `adopt_turn` take a `TurnLock` by value — so "start an execution
    /// without the lock" is not a mistake the code can express.
    ///
    /// `entry` names the caller (`dispatch` | `parked` | `retry` | `adopt`) and
    /// is recorded on the row, so a refusal log says which two paths collided.
    pub(crate) fn lock_turn(
        &self,
        state: &Arc<ThreadState>,
        entry: &str,
    ) -> std::result::Result<TurnLock, LockRefusal> {
        // GATE 0 — UPDATE ADMISSION (intent item 64). Before anything else,
        // because this is the one place every execution in the daemon starts,
        // and therefore the only place where "nothing is being started" can be
        // made TRUE rather than merely likely. A restart window closes
        // admission while holding the same mutex this call needs, so a turn
        // cannot slip in between the window's quiet check and its `execv`.
        //
        // The permit lives inside the returned `TurnLock` for the whole
        // execution, where it is the REPORTED `turns` count. It is no longer
        // what "quiet" means — see the doc on `TurnLock::permit` and the
        // `update` module header. The refusal below is unchanged and is still
        // the admission gate.
        let Some(permit) = self.update.admit_turn_for(&state.id) else {
            tracing::info!(
                thread = %state.id, %entry,
                "a restart window is open — this execution is refused and the work stays \
                 queued for the daemon that comes back"
            );
            return Err(LockRefusal::RestartPending);
        };
        let permit = StdMutex::new(Some(permit));
        // Gate 1 — the in-process CAS. Cheap, and the only thing fast enough to
        // settle two tasks that reached here in the same microsecond.
        if state.turn_running.swap(true, Ordering::SeqCst) {
            return Err(LockRefusal::InProcess);
        }
        // Gate 2 — the durable claim. From here every exit MUST clear the
        // atomic, or the thread is deadlocked in this process.
        let undo = || state.turn_running.store(false, Ordering::SeqCst);
        let me = std::process::id() as i64;
        // One constructor for the three success paths below, so the admission
        // permit can never be forgotten on one of them.
        let grant = || TurnLock {
            mgr: self.clone(),
            state: state.clone(),
            released: AtomicBool::new(false),
            permit: StdMutex::new(permit.lock().unwrap_or_else(|e| e.into_inner()).take()),
        };
        // Two passes at most: a holder that vanishes between our INSERT and our
        // read (or that we successfully steal) deserves exactly one retry, and
        // a bounded retry cannot spin.
        for _ in 0..2 {
            match self
                .store
                .try_claim_thread(&state.id, &self.boot_id, me, entry)
            {
                Ok(Claim::Taken) => {
                    let lock = grant();
                    self.emit_status(&state.id);
                    return Ok(lock);
                }
                Ok(Claim::Vanished) => continue,
                Ok(Claim::Held(row)) => {
                    // THE SHARED RULE (`lock_is_live`), asked with
                    // `turn_running: false` — and that argument is a statement
                    // of fact, not a convenience. Gate 1 above swapped the
                    // atomic and got `false` back, proving no execution was
                    // running in this process when we arrived; the `true` now
                    // sitting in it is OURS, put there a dozen lines up. So the
                    // honest input to "does this process have a turn running on
                    // this thread" is `false`, and a row bearing our own boot id
                    // is therefore a leaked row (a failed release), not a live
                    // execution — take it back and say so.
                    //
                    // `is_locked` and `thread_states` ask the identical
                    // question with their own honest `turn_running`. That is the
                    // fix for the inverted rule: the observer and the actor can
                    // no longer disagree about one row.
                    let ours = row.boot_id == self.boot_id;
                    let recycled = !ours && row.daemon_pid == me;
                    if !self.lock_is_live(&row, false) {
                        tracing::warn!(
                            thread = %state.id, holder = %row.boot_id,
                            holder_pid = row.daemon_pid, %entry,
                            reason = if ours { "own stale row" }
                                else if recycled { "recycled pid" }
                                else { "holder pid is dead" },
                            "reclaiming a stale thread lock"
                        );
                        match self.store.steal_thread_lock(
                            &state.id,
                            &row.boot_id,
                            &self.boot_id,
                            me,
                            entry,
                        ) {
                            Ok(true) => {
                                let lock = grant();
                                self.emit_status(&state.id);
                                return Ok(lock);
                            }
                            // Lost the reclaim race to another daemon: whoever
                            // won it is now the live holder. Re-read once.
                            Ok(false) => continue,
                            Err(e) => {
                                undo();
                                return Err(LockRefusal::StoreError(e.to_string()));
                            }
                        }
                    }
                    // A LIVE holder. This is the directive: it is not possible
                    // to run this thread a second time. The work stays queued.
                    tracing::info!(
                        thread = %state.id, holder = %row.boot_id,
                        holder_pid = row.daemon_pid, held_since = %row.acquired_at,
                        holder_entry = %row.entry, %entry,
                        "thread is LOCKED by a live execution — the send stays queued"
                    );
                    undo();
                    return Err(LockRefusal::HeldBy(row));
                }
                Err(e) => {
                    undo();
                    return Err(LockRefusal::StoreError(e.to_string()));
                }
            }
        }
        undo();
        Err(LockRefusal::StoreError(
            "the thread lock changed hands twice while claiming it".into(),
        ))
    }

    /// Claim a thread by id — the public face of [`Self::lock_turn`], for
    /// callers outside the runtime module (the entry-point tests, and any
    /// future verb that needs to run a thread by hand).
    ///
    /// Holding the returned guard is what makes a concurrent execution
    /// impossible; dropping it frees the thread.
    pub fn try_lock_thread(
        &self,
        thread_id: &str,
        entry: &str,
    ) -> std::result::Result<TurnLock, LockRefusal> {
        let state = self
            .get_state(thread_id)
            .map_err(|e| LockRefusal::StoreError(e.to_string()))?;
        self.lock_turn(&state, entry)
    }

    /// **IS THIS LOCK ROW A LIVE EXECUTION?** The one rule, in one place.
    ///
    /// It used to exist twice, with opposite answers for the same row.
    /// [`Self::is_locked`] called a row bearing OUR OWN `boot_id` live
    /// unconditionally; `lock_turn`'s `Claim::Held` arm called the identical row
    /// STALE and stole it. So a failed `release_thread_lock` left
    /// `thread_status` reporting `active` forever for the life of the process
    /// while the dispatcher cheerfully kept running turns on that very thread —
    /// the observer and the actor disagreeing about the same row, which is the
    /// operator's rule violated inside a single process.
    ///
    /// `lock_turn`'s rule wins, because it is the one the daemon ACTS on and
    /// because it is the one that is right: our own `boot_id` on a row while
    /// this process has no turn running is a LEAKED row (a release that failed),
    /// not an execution.
    ///
    /// `turn_running` is the caller's honest answer to "does THIS process have a
    /// turn running on this thread right now". It is only consulted for a row we
    /// own; another daemon's row is judged by its pid, which is the only thing
    /// we can observe about it.
    ///
    /// Three callers, one rule: [`Self::is_locked`], [`Self::thread_states`],
    /// and `lock_turn`. `recover`'s boot sweep is the fourth.
    pub(crate) fn lock_is_live(&self, row: &crate::store::ThreadLockRow, turn_running: bool) -> bool {
        if row.boot_id == self.boot_id {
            // OUR row. Live iff we really are running a turn on it.
            return turn_running;
        }
        // A pid equal to OUR pid under a DIFFERENT boot id is a recycled pid
        // from a previous daemon, not a live holder.
        if row.daemon_pid == std::process::id() as i64 {
            return false;
        }
        pid_alive(row.daemon_pid)
    }

    /// Does THIS process have a turn running on `thread_id`?
    ///
    /// Reads the in-memory registry WITHOUT rehydrating: a thread nobody has
    /// touched this boot has no `ThreadState`, and therefore no turn running,
    /// and building one to discover that would turn a status read into a ledger
    /// read per thread. Absent means `false`, which is the truth.
    fn turn_running(&self, thread_id: &str) -> bool {
        self.threads
            .lock()
            .unwrap()
            .get(thread_id)
            .map(|s| s.turn_running.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    /// Is an execution holding this thread right now — by a holder that is
    /// actually ALIVE? (A dead holder's row is reclaimable, so it is not a live
    /// execution and must not be reported as one.)
    ///
    /// A thin read over [`Self::lock_is_live`]; it has no rule of its own.
    pub fn is_locked(&self, thread_id: &str) -> bool {
        match self.store.get_thread_lock(thread_id) {
            Ok(Some(row)) => self.lock_is_live(&row, self.turn_running(thread_id)),
            Ok(None) => false,
            // Unreadable is reported as LOCKED: the safe answer to "is
            // something already running this thread" is always yes.
            Err(_) => true,
        }
    }

    // ── STATUS: the second half of msg #1213 ─────────────────────────────
    //
    // THE ONE DERIVATION. Everything that reports whether a thread is running
    // comes through `thread_states` below — `threads.status`, `threads.list`,
    // `threads.get`, `threads.seen`, the `status` push frame. There is no
    // second copy and no cached column to disagree with it.
    //
    // > "We shouldn't have the ability to get two different answers when two
    // >  different people ask."
    //
    // That is the whole reason this function is shaped the way it is. It was
    // nine derivations, four vocabularies and one stale column; the column is
    // deleted (`store::DROPPED_COLUMNS`) and the derivations collapsed to this.

    /// **DERIVE the state of many threads at once — five reads, not seven per
    /// thread.**
    ///
    /// Every input is a whole-table read of a table that holds one row per
    /// *interesting* thread (a held lock, a parked turn, an armed retry, a
    /// waiting message, a seen cursor), never one row per thread that exists.
    /// The old per-thread shape cost 7 queries × N through ONE serialized
    /// `Mutex<Connection>`; AUI asks for `limit: 1000` and this database has 587
    /// threads, so a single reconnect cost ~4110 trips. This costs five, for any
    /// N, and the query-count test in `tests/one_derivation.rs` holds that.
    ///
    /// The park latch is read from MEMORY, not from a sixth query, and that is
    /// also a correctness fix: only `park_killed_turn` and `park_retried_turn`
    /// write the durable `parked_turns` row, so an ADMISSION-GATE park existed
    /// solely in `state.park_reason`. `park_state` read it and `thread_status`
    /// did not — which is why `emit_park` had to call `emit_status` to announce
    /// a park whose own frame then said `queued`. Both shapes are read here now,
    /// so both are one answer.
    ///
    /// Ids that name no thread are simply absent from the result.
    pub fn thread_states(&self, ids: &[String]) -> Result<HashMap<String, DerivedState>> {
        // ── the five reads ────────────────────────────────────────────────
        // 1. Who holds an execution lock (filtered below by the SAME liveness
        //    predicate `lock_turn` acts on — see `lock_is_live`).
        let locks: HashMap<String, crate::store::ThreadLockRow> = self
            .store
            .list_thread_locks()?
            .into_iter()
            .map(|r| (r.thread_id.clone(), r))
            .collect();
        // 2. Durable parks (a killed or retried turn whose prompt is held).
        let parked: HashMap<String, String> = self
            .store
            .list_parked_turns()?
            .into_iter()
            .map(|p| (p.thread_id, p.reason))
            .collect();
        // 3. Armed retries.
        let retries: HashMap<String, String> = self
            .store
            .list_retries()?
            .into_iter()
            .map(|r| (r.thread_id, r.reason))
            .collect();
        // 4. Waiting queue depth, off `idx_queue_thread_state`.
        let queued = self.store.waiting_queue_counts()?;
        // 5. Seen cursors, so `threads.status` needs no per-thread lookup.
        let seen = self.store.list_thread_seen()?;

        // ── the two in-memory reads: free, one mutex, no query ────────────
        let (running, gate_parks): (HashSet<String>, HashMap<String, String>) = {
            let g = self.threads.lock().unwrap();
            let running = g
                .iter()
                .filter(|(_, s)| s.turn_running.load(Ordering::SeqCst))
                .map(|(id, _)| id.clone())
                .collect();
            let parks = g
                .iter()
                .filter_map(|(id, s)| {
                    s.park_reason
                        .lock()
                        .unwrap()
                        .clone()
                        .map(|r| (id.clone(), r))
                })
                .collect();
            (running, parks)
        };

        let mut out = HashMap::with_capacity(ids.len());
        for id in ids {
            let executing = locks
                .get(id)
                .map(|row| self.lock_is_live(row, running.contains(id)))
                .unwrap_or(false);
            // A durable park row's reason wins over the in-memory latch, the
            // same precedence `park_state` uses: the row is the one that
            // survives a restart.
            let park = parked.get(id).or_else(|| gate_parks.get(id));
            let retry = retries.get(id);
            let queued = queued.get(id).copied().unwrap_or(0);
            // Precedence, unchanged from the per-thread version it replaces: an
            // execution outranks everything, then a held prompt, then an armed
            // retry, then accepted-but-undelivered work, then nothing.
            let (state, detail) = if executing {
                ("active", None)
            } else if let Some(r) = park {
                ("parked", Some(r.clone()))
            } else if let Some(r) = retry {
                ("retrying", Some(r.clone()))
            } else if queued > 0 {
                ("queued", None)
            } else {
                ("idle", None)
            };
            out.insert(
                id.clone(),
                DerivedState {
                    thread_id: id.clone(),
                    state,
                    executing,
                    queued,
                    last_seen_seq: seen.get(id).copied().unwrap_or(0),
                    detail,
                },
            );
        }
        Ok(out)
    }

    /// One thread's derived state, or the `idle` reading for a thread nothing
    /// knows anything about (which is exactly what an untouched thread is).
    pub fn thread_state(&self, thread_id: &str) -> Result<DerivedState> {
        let id = thread_id.to_string();
        Ok(self
            .thread_states(std::slice::from_ref(&id))?
            .remove(thread_id)
            .unwrap_or_else(|| DerivedState::idle(thread_id)))
    }

    /// **IS THIS THREAD IDLE — nothing executing, nothing waiting?** (R4.S4b,
    /// HOLDS R19.)
    ///
    /// Asking `running_turns` alone is wrong in a window every dispatch opens:
    /// the queue rows are marked delivered right after the execution LOCK is
    /// taken, and the `running_turns` row is written only once `claude` has
    /// spawned — after the plugin snapshot, the spawn guard and, for a
    /// projected thread, the render. In between, "no running row and no
    /// waiting row" reads idle while a turn is starting. The lock
    /// (`thread_locks`, the same liveness `thread_state` reports as
    /// `executing`) spans that window, so it is asked first; the running row is
    /// still asked, for the tail after the lock is released.
    pub fn is_idle(&self, thread_id: &str) -> Result<bool> {
        let d = self.thread_state(thread_id)?;
        // R10.M2: a RIDER is not pending work — it never starts a turn, it
        // only goes out with the next one — so a thread whose queue holds
        // nothing but riders is idle (and a cancelled managed child whose
        // messages ride still ends `cancelled` to its parent).
        let pending = d.queued > 0
            && self
                .store
                .waiting_queue(thread_id)?
                .iter()
                .any(|q| q.dispatch != crate::event::delivery::RIDE);
        Ok(!d.executing && !pending && self.store.get_running_turn(thread_id)?.is_none())
    }

    /// **TEST ONLY.** The `ledgers/` write lock, so a test can hold a projected
    /// turn inside its prologue (the dispatch gap `is_idle` covers).
    #[doc(hidden)]
    pub fn ledger_write_lock_for_test(&self) -> Arc<std::sync::Mutex<()>> {
        self.managed.write_lock()
    }

    /// Dress a derived state as the `threads.status` payload: the derivation
    /// plus the two COUNTS, which are per-thread `events` reads and are the
    /// reason this is separate from [`Self::thread_states`].
    ///
    /// `threads.list` deliberately does NOT call this — the `unread`/`last_seq`
    /// fan-out measures ~0.26s of SQLite over this database, and the list
    /// neither returns those numbers nor needs them.
    fn status_json(&self, d: &DerivedState) -> Result<Value> {
        let unread = self.store.unread_count(&d.thread_id, d.last_seen_seq)?;
        let last_seq = self.store.max_seq(&d.thread_id).unwrap_or(0);
        Ok(json!({
            "thread_id": d.thread_id,
            "state": d.state,
            "executing": d.executing,
            "queued": d.queued,
            "unread": unread,
            "last_seq": last_seq,
            "last_seen_seq": d.last_seen_seq,
            "detail": d.detail,
        }))
    }

    /// The per-thread status AUI both queries and receives: `active` (an
    /// execution holds the lock), `queued` (messages accepted and waiting),
    /// `parked` / `retrying`, or `idle` — plus the queued and unread counts.
    ///
    /// A thin wrapper over [`Self::thread_states`]. It holds NO rule of its own,
    /// which is the point: `threads.status` and `threads.list` cannot disagree
    /// because there is only one place where the answer is worked out.
    pub fn thread_status(&self, thread_id: &str) -> Result<Value> {
        self.status_json(&self.thread_state(thread_id)?)
    }

    /// The whole-fleet `threads.status`: ONE derivation for every thread, then
    /// the two count reads per thread.
    ///
    /// The counts are the remaining per-thread cost on this path (2N trips) and
    /// are known: they are what `threads.status` is FOR. `threads.list` — the
    /// call AUI makes on every reconnect — pays none of it.
    pub fn thread_statuses(&self, ids: &[String]) -> Result<Vec<Value>> {
        let derived = self.thread_states(ids)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let d = derived
                .get(id)
                .cloned()
                .unwrap_or_else(|| DerivedState::idle(id));
            out.push(self.status_json(&d)?);
        }
        Ok(out)
    }

    // ── THE SHAPE PRODUCERS (P1-P15) ─────────────────────────────────────
    //
    // Each of these sits beside a `account_picture_changed_for` call that was
    // already there. That call ANNOUNCES ("the account picture moved, go
    // re-read"); these carry the VALUE. The two are deliberately paired at
    // every site rather than one replacing the other: the announcement is what
    // an old board and the harness faces still use, and the fact is what a fold
    // needs. Because both fire from the same place, they cannot disagree about
    // whether something happened.
    //
    // **Every one is gated on `hub.any_tracking()`.** A producer that read the
    // store to build a fact nobody folds would be pure cost on a machine with
    // no board open — which is the same argument that makes the `accounts`
    // shape `Lazy` in the first place.

    /// P1 — a gauge observation was RECORDED (never merely attempted).
    ///
    /// Returns rule 2.7's `visible` for the promises about THIS account — see
    /// [`Self::emit_disabled_fact`], which carries the whole argument.
    pub fn emit_gauge_fact(&self, account: &str, axis: crate::gauge::Axis) -> Visible {
        if !self.hub.any_tracking() {
            // See `emit_disabled_fact`: nothing will carry the ids, so ABANDON them
            // here — which is a named act with a reply of its own, not a silent
            // close.
            let _ = self
                .promises
                .abandon_all::<crate::commands::GaugeObservedFact>(Some(account));
            return Visible::Never;
        }
        match self.budgets.gauge_axis_row(account, axis) {
            // **THE SUBJECT IS THE ACCOUNT THIS OBSERVATION IS ABOUT**, and it is
            // an argument of the emit for the same reason the resolve is: there is
            // no line above this one for a later edit to leave behind.
            Ok(row) => self.hub.emit_fact_resolving(
                self.promises
                    .resolve::<crate::commands::GaugeObservedFact>(Some(account)),
                crate::event::Fact::GaugeObserved {
                account: account.to_string(),
                axis: axis.as_str().to_string(),
                row,
            }),
            Err(e) => tracing::debug!(%account, error = %e, "gauge fact not emitted"),
        }
        self.produce_pressure();
        Visible::Expected
    }

    /// P2 / P8 / P9 — the CURRENT limit window for one login, as
    /// `accounts.list` would report it. Emitting the level rather than the
    /// event means a recorded, a cleared and a re-recorded window all converge
    /// on the same cell value, so a dropped fact cannot leave a stale bar on a
    /// card.
    pub fn emit_limit_fact(&self, account: &str) {
        if !self.hub.any_tracking() {
            return;
        }
        let state = self
            .store
            .list_account_states()
            .ok()
            .and_then(|v| v.into_iter().find(|s| s.name == account));
        // The EFFECTIVE window, at THIS instant. A window that has ended
        // publishes `LimitCleared`, not a `LimitRecorded` carrying a watermark
        // four days in the past — the live fold is the other half of the
        // `accounts` shape that was republishing one (`shapes::accounts`).
        let now = crate::store::now_rfc3339();
        let fact = match state.as_ref().and_then(|s| s.effective_limit_until(&now)) {
            Some(until) => crate::event::Fact::LimitRecorded {
                account: account.to_string(),
                until: until.to_string(),
                axis: state.as_ref().and_then(|s| s.limit_axis.clone()),
                prose: state.as_ref().and_then(|s| s.last_limit_prose.clone()),
            },
            None => crate::event::Fact::LimitCleared {
                account: account.to_string(),
            },
        };
        self.hub.emit_fact(fact);
    }

    /// P4 — login health, as a level for the same reason as the limit.
    pub fn emit_health_fact(&self, account: &str) {
        if !self.hub.any_tracking() {
            return;
        }
        let state = self
            .store
            .list_account_states()
            .ok()
            .and_then(|v| v.into_iter().find(|s| s.name == account));
        self.hub.emit_fact(crate::event::Fact::LoginHealth {
            account: account.to_string(),
            healthy: state.as_ref().map(|s| s.healthy()).unwrap_or(true),
            dead_since: state.as_ref().and_then(|s| s.dead_since.clone()),
            dead_reason: state.as_ref().and_then(|s| s.dead_reason.clone()),
        });
    }

    /// P3 / P10 — a pool share moved. Built by `pool_share_row`, the one
    /// builder `accounts.pool_share.get` and the CLI also answer through.
    pub fn emit_share_fact(&self, account: &str) -> Visible {
        if !self.hub.any_tracking() {
            // See `emit_disabled_fact`: no frame will carry them, so ABANDON them
            // here — a named act, reported on the reply, rather than a close in
            // silence.
            let _ = self
                .promises
                .abandon_all::<crate::commands::PoolShareFact>(Some(account));
            return Visible::Never;
        }
        // The resolve is an ARGUMENT of the emit, never a statement above it —
        // and so is the SUBJECT it resolves for, which is this account and not
        // whichever account's row happens to flush first.
        self.hub.emit_fact_resolving(
            self.promises
                .resolve::<crate::commands::PoolShareFact>(Some(account)),
            crate::event::Fact::PoolShare {
                account: account.to_string(),
                row: self.budgets.pool_share_row(account),
            },
        );
        self.produce_pressure();
        Visible::Expected
    }

    /// **P16 — IS THIS ACCOUNT IN THE POOL AT ALL.** The producer for
    /// [`crate::event::Fact::AccountDisabled`], and the reason "disabled" reaches
    /// a consumer as a fact instead of as a number it has to interpret.
    ///
    /// Read straight off `account_disabled`, which is the state, so this can
    /// never disagree with what `Budgets::is_disabled` tells placement half a
    /// millisecond later. That symmetry is the whole point of a producer: the
    /// board and the ranker are looking at one row.
    ///
    /// Emitted by `accounts.disable`, `accounts.enable` and the roster re-assert
    /// in [`Self::reload_pool`] — the same three places the share fact is
    /// emitted, because the two facts are independent in meaning and not in
    /// timing: every moment one of them can move, the other can too.
    /// **P16, and the point at which `accounts.disable`/`enable` RESOLVE.**
    ///
    /// The resolve happens HERE, at the emit, and deliberately not in the
    /// handler. A handler that took the ids out of the book before calling this
    /// would empty the book whether or not the emit followed — which makes the
    /// liveness property untestable, because a forgotten emit leaves no trace.
    /// The book must be closed BY the emit or the assertion "no promise left
    /// open" proves nothing. (It was written the wrong way round first, and the
    /// forced-red pass caught it: deleting the emit left every test green.)
    /// **AND IT RETURNS RULE 2.7's `visible`, WHICH IS THE 2026-09-26 REPAIR.**
    ///
    /// The not-tracking arm below closes a promise with no emit. The reasoning was
    /// right for the BOOK and the COMMENT made an assumption the wire then broke:
    /// *"a consumer that is not subscribed cannot be waiting"* — which is false for
    /// the very next consumer in line. The AUI issues commands from a process that
    /// may not yet be subscribed to the shape they move, and the reply it got here
    /// was byte-identical to the reply on the path where a frame WILL follow. So a
    /// writer could hold an id that resolves nowhere and could not tell that from a
    /// slow fold. Every other collapse in this area degrades to *slow* or
    /// *ambiguous*; this one degrades to *forever*.
    ///
    /// The answer is returned rather than re-derived by the handler, because a
    /// handler asking `any_tracking()` for itself reads the same predicate at a
    /// second instant and the two reads can disagree — which is the disease, one
    /// layer down. The caller turns this into [`Accepted::visible`].
    pub fn emit_disabled_fact(&self, account: &str) -> Visible {
        if !self.hub.any_tracking() {
            // No shape is live, so no frame will ever carry these ids. ABANDON
            // them — the command DID take effect, the store write happened before
            // this call, and what is missing is only the chassis's ability to say
            // so. `abandon_all` is deliberately not `resolve`: `resolve`'s routing
            // is now RETAINED for a re-export (`ShapeTask::carried`), so calling it
            // with nothing to emit would record ids against a frame that never
            // exists.
            let _ = self
                .promises
                .abandon_all::<crate::commands::AccountDisabledFact>(Some(account));
            return Visible::Never;
        }
        // ONE read decides, and the prose is fetched only when the state says
        // there is prose to fetch. An enable DELETES the row, so a `note` read
        // afterwards is `None` by construction rather than by being cleared —
        // there is no window in which the last disable's words ride an enable.
        let disabled_at = self.store.account_disabled_at(account).unwrap_or(None);
        let note = match disabled_at {
            Some(_) => self.store.account_disabled_note(account).unwrap_or(None),
            None => None,
        };
        // **The resolve is an ARGUMENT of the emit, not a statement above it.**
        // On its own line it would survive the emit's deletion and empty the
        // book anyway, which is how the first two versions of this passed a
        // forced-red pass they should have failed. Inlined, there is no way to
        // remove the emit and keep the resolve.
        //
        // **AND IT RESOLVES FOR ONE ACCOUNT**, the one this fact is about. Before
        // the subject existed this closed every open `AccountDisabled` promise
        // whatever account it was about, so two operators disabling two accounts
        // each closed the other's promise and each id rode a frame the other
        // consumer never receives.
        self.hub.emit_fact_resolving(
            self.promises
                .resolve::<crate::commands::AccountDisabledFact>(Some(account)),
            crate::event::Fact::AccountDisabled {
                account: account.to_string(),
                disabled: disabled_at.is_some(),
                disabled_at,
                note,
            },
        );
        Visible::Expected
    }

    /// **P15 — PRESSURE IS PRODUCED, NOT DERIVED.**
    ///
    /// The operator reversed rev 2 on this: the producer computes pressure,
    /// stamps it and emits it; no consumer holds the formula. A formula change
    /// then ships in one binary and reaches every consumer without a redeploy,
    /// and `caps.py`'s "a second implementation would be a second answer" stays
    /// true with zero implementations outside `budgets.rs`.
    ///
    /// Called after P1, P3 and P5 — the three things pressure is a function of
    /// (an observation, a share, the roster). Between those it AGES, and
    /// `as_of` says so on screen; the gauge poll (120 s busy / 900 s idle) and
    /// `R` bound the staleness exactly as they do today.
    ///
    /// **The stamp is taken before the read** (ruling 3): `pressure_table`
    /// reads the store to compute, so a stamp applied at emit time would be
    /// newer than the content it describes.
    pub fn produce_pressure(&self) {
        if !self.hub.any_tracking() {
            return;
        }
        let at = crate::clock::now();
        let table = self.budgets.pressure_table(&self.account_pool(), at);
        let as_of = table.now.clone();
        let unavailable = self.account_unavailability();
        for r in &table.accounts {
            let Ok(row) = serde_json::to_value(r) else {
                continue;
            };
            let row = with_availability(row, &r.account, &unavailable);
            self.hub.emit_fact_at(
                at,
                crate::event::Fact::Pressure {
                    account: r.account.clone(),
                    row,
                    as_of: as_of.clone(),
                },
            );
        }
    }

    /// **P6 — THE CREDENTIAL SAMPLER, AND WHY IT IS A FALLBACK.**
    ///
    /// Operator ruling: *"Polling shouldn't be the default."* The keychain and
    /// `accounts.toml` are written by OTHER processes (`claude-accounts`,
    /// `claude /login`, the rotation script), so there is genuinely no event
    /// this daemon can hook for an out-of-band write — which is exactly the
    /// case §6 names as "the edge of what a sampler should be asked to do".
    ///
    /// So the sampler is the FALLBACK, not the mechanism. The mechanism is that
    /// `accounts.login.complete` and `accounts.rotate` call this directly on
    /// success, because those are the two paths the daemon itself drives and it
    /// knows the instant they finish. The periodic call exists only to catch
    /// what another process did behind the daemon's back.
    ///
    /// **Five minutes, and NOT `resume_tick`.** Rev 3 hung this on
    /// `start_watchers`' tick, which `runtime.rs:228` defines as 5 s — four
    /// `security` invocations every five seconds while a board is open, a ~10%
    /// duty cycle and ~70,000 keychain reads a day, on the same task as
    /// `resume_sweep` and `retry_sweep`. The critique's F4 caught it. This has
    /// its own loop, its own period, and its own `spawn_blocking`, and it is
    /// gated on the shape being TRACKED so a machine nobody is watching pays
    /// nothing at all.
    ///
    /// **The gauge poll keeps its own cadence** (120 s busy / 900 s idle) and
    /// is NOT touched: that is the operator's liveness-critical path and
    /// credentials are not.
    ///
    /// **A-1 (R9.A1): THE SAMPLE ALSO FEEDS PLACEMENT.** It used to only emit
    /// `CredentialChanged`, so the board drew a mid-day EXPIRED within five
    /// minutes while the sign-in set — written only at boot and after an auth
    /// failure — kept the account placeable until a turn failed on it. Every
    /// call now first reconciles the sign-in set
    /// ([`Self::reconcile_sign_in_needed`]), tracked or not; only the board
    /// emission below is gated on the shape being tracked.
    pub fn sample_credentials(&self) {
        self.reconcile_sign_in_needed();
        if !self.hub.tracked("accounts") {
            return;
        }
        use crate::shape::{Handles, Hydration};
        let slots = match Handles::<crate::shapes::accounts::LoginSlots>::hydrate(
            self,
            crate::shapes::accounts::LoginSlots,
        ) {
            Hydration::Complete(v) => v,
            Hydration::Partial { got, reason } => {
                tracing::debug!(%reason, "credential sample was partial");
                got
            }
            Hydration::Empty { .. } => Vec::new(),
        };
        for slot in slots {
            self.hub
                .emit_fact(crate::event::Fact::CredentialChanged {
                    account: slot.label.clone(),
                    slot: Box::new(slot),
                });
        }
    }

    /// **P13 — every NON-IDLE thread, with where it is and since when.**
    ///
    /// `threads/non-idle`'s hydrate for its roster, and the `accounts` shape's
    /// for which keys hold non-idle work (its `threads` cell, which this used
    /// to fill, is gone since R2.S8). It is
    /// `thread_states`' fleet-wide reads (NOT `threads.status`' per-thread
    /// walk, which is the 265 ms N+1) restricted to the threads that are
    /// actually doing something, plus the placement out of
    /// `account_of` and a `since` out of the three row tables that carry an
    /// instant.
    ///
    /// **Idle threads are excluded by construction, not filtered on the
    /// consumer.** 151 idle threads name the machine default in this store
    /// today; every one of them would be a row nobody draws and a key nobody
    /// wants. "Non-idle" is the whole selection rule of this shape's `threads`
    /// cell, live and hydrated alike — an `idle` `Status` REMOVES a row.
    ///
    /// `since` is best-effort by table: a held lock knows when it was acquired,
    /// a parked turn when it parked, a retry when its floor was set. A queued
    /// thread has no single instant here (the queue holds many rows), so it
    /// reports `None` rather than guessing — and the shape's `Semilattice<Min>`
    /// then takes the first LIVE stamp instead, which is honest about being a
    /// since-init observation.
    ///
    /// ── WHAT THIS USED TO DO, AND WHY IT WAS A DEFECT ─────────────────────
    ///
    /// It read `list_threads(100_000)` — every thread that exists, eleven
    /// columns each — and then called `get_thread` ONCE PER SURVIVING ROW to
    /// reach a single `last_seq`. That is an unbounded fleet scan plus an N+1,
    /// inside a hydrate path, and `docs/shapes.md` 4.3 says a producer owes ONE
    /// read per request. It was invisible because `thread_locks`, `parked_turns`
    /// and `retries` are all empty on this machine, so N was zero and the scan's
    /// cost hid behind a warm page cache.
    ///
    /// **The bound is not a number picked for the scan; it is the CANDIDATE
    /// SET.** A thread can be reported anything but `idle` only if it appears in
    /// one of four places — a held execution lock, a durable parked turn, an
    /// armed retry, or a waiting queue row — plus the in-memory admission-gate
    /// park latch, which costs no query at all. That is exactly the precedence
    /// [`Self::thread_states`] applies, so the union of those keys is a SUPERSET
    /// of the non-idle set by construction, and every id outside it would have
    /// been read, derived and then thrown away. The fleet scan is therefore not
    /// bounded, it is DELETED: each of those tables holds one row per
    /// INTERESTING thread, never one row per thread that exists.
    ///
    /// The residual cap is [`MAX_NON_IDLE_HYDRATE_ROWS`] and what it protects is
    /// no longer the scan — it is the export and the `IN (…)` statement. See its
    /// own docs for the number and the reason.
    ///
    /// Read cost is now FLAT: five whole-table reads for the candidates and
    /// their `since` stamps (the fifth is `delivery_clocks`), `thread_states`'
    /// five, and ONE [`crate::store::Store::last_seqs`] for every candidate at
    /// once — eleven statements, whatever the fleet size and whatever the
    /// activity level.
    /// Pinned by `tests/hydrate_cost.rs`.
    pub fn thread_rows_non_idle(&self) -> Result<NonIdleThreads> {
        let parse = |s: &str| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.with_timezone(&Utc))
        };
        // ── THE CANDIDATE SET AND THE `since` STAMPS, OUT OF THE SAME READS ──
        //
        // A `BTreeSet` rather than a `HashSet` so that if the cap below ever
        // bites, WHICH rows survive is deterministic (lowest thread id first)
        // rather than a function of hash order — a truncation nobody can
        // reproduce is a truncation nobody can debug.
        let mut candidates: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        let mut since: HashMap<String, chrono::DateTime<Utc>> = HashMap::new();
        for r in self.store.list_thread_locks()? {
            if let Some(t) = parse(&r.acquired_at) {
                since.insert(r.thread_id.clone(), t);
            }
            candidates.insert(r.thread_id);
        }
        for r in self.store.list_parked_turns()? {
            if let Some(t) = parse(&r.parked_at) {
                since.entry(r.thread_id.clone()).or_insert(t);
            }
            candidates.insert(r.thread_id);
        }
        for r in self.store.list_retries()? {
            // BIRTH, NOT A DEADLINE. This read `retry_at`, which answered "when
            // will it next try" to a question that asked "since when has it been
            // waiting" — and moved on every attempt, so a thread four hundred
            // attempts deep looked freshly arrived. `created_at` survives every
            // re-arm, which is what this column is for.
            if let Some(t) = parse(&r.created_at) {
                since.entry(r.thread_id.clone()).or_insert(t);
            }
            candidates.insert(r.thread_id);
        }
        // Threads with a delivery IN HAND (locked, retrying or parked) — the
        // only ones a delivery clock may speak for. A thread with merely a
        // waiting queue has not started its next delivery yet.
        let in_delivery = candidates.clone();
        for (id, n) in self.store.waiting_queue_counts()? {
            if n > 0 {
                candidates.insert(id);
            }
        }
        // ── A RETRY MUST NOT RESET `since` (an internal thread) ──────────────
        //
        // A retried attempt re-takes the execution lock, and the lock's
        // `acquired_at` was the first thing read above — so every retry made a
        // thread forty attempts deep look freshly started, and 2½ days of
        // looping read as "busy", never as "stuck". The delivery's own clock
        // records when its FIRST attempt began and is not moved by a retry;
        // for a thread that is a candidate already, the EARLIER of the two
        // wins. It never adds a candidate: a clock alone is not activity.
        for (id, started) in self.store.list_delivery_starts()? {
            if !in_delivery.contains(&id) {
                continue;
            }
            if let Some(t) = parse(&started) {
                since
                    .entry(id)
                    .and_modify(|cur| {
                        if t < *cur {
                            *cur = t;
                        }
                    })
                    .or_insert(t);
            }
        }
        // The ADMISSION-GATE park lives only in memory (`state.park_reason`) and
        // writes no durable row, but `thread_states` reads it as a park — so it
        // is a candidate. One mutex, no query.
        {
            let g = self.threads.lock().unwrap();
            for (id, s) in g.iter() {
                if s.park_reason.lock().unwrap().is_some() {
                    candidates.insert(id.clone());
                }
            }
        }

        let total = candidates.len();
        let over = total.saturating_sub(MAX_NON_IDLE_HYDRATE_ROWS);
        let truncated = (over > 0).then(|| {
            format!(
                "{total} threads are non-idle; this read carries the first \
                 {MAX_NON_IDLE_HYDRATE_ROWS} in thread-id order and {over} are missing"
            )
        });
        let ids: Vec<String> = candidates
            .into_iter()
            .take(MAX_NON_IDLE_HYDRATE_ROWS)
            .collect();

        let derived = self.thread_states(&ids)?;
        // ONE statement for every candidate's watermark, instead of one
        // `get_thread` per row.
        let last_seqs = self.store.last_seqs(&ids)?;

        let mut rows = Vec::new();
        for id in &ids {
            let Some(d) = derived.get(id) else { continue };
            if d.state == "idle" {
                continue;
            }
            let Some(account) = self.account_of(id) else {
                continue;
            };
            rows.push(crate::shapes::accounts::ThreadEntry {
                thread_id: id.clone(),
                account,
                state: d.state.to_string(),
                executing: d.executing,
                queued: d.queued,
                unread: last_seqs
                    .get(id)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(d.last_seen_seq),
                detail: d.detail.clone(),
                since: since.get(id).copied(),
            });
        }
        Ok(NonIdleThreads { rows, truncated })
    }

    /// Broadcast a status transition. Transient like liveness — never in the
    /// ledger — and EDGE-triggered: emitted where the answer can actually have
    /// changed (lock taken/released, message accepted or cancelled, park), not
    /// on a timer.
    pub(crate) fn emit_status(&self, thread_id: &str) {
        let _ = self.emit_status_answering(thread_id, false);
    }

    /// [`Self::emit_status`], and — when `answers` — the queue fact it carries
    /// RESOLVES the open `ThreadQueueFact` promises about this thread.
    ///
    /// `answers` is `true` on exactly two paths, the two queue commands
    /// (`threads/{thread_id}/queue_cancel`, `.../queue_reselect`). Every other
    /// status edge passes `false`: one landing between such a promise's `open`
    /// and its write would otherwise close it on a level that does not yet
    /// contain the write.
    ///
    /// Returns the emit path's own [`Visible`]. Every early return ABANDONS
    /// when it answers, so no promise outlives a status that could not be read.
    pub(crate) fn emit_status_answering(&self, thread_id: &str, answers: bool) -> Visible {
        // **THE STAMP IS TAKEN BEFORE THE READ, DELIBERATELY** (the critique's
        // F2, and the operator's ruling on it). `thread_status` is five store
        // reads; if the hub stamped at SEND time, a status whose content was
        // read at t0 could arrive stamped t3 — after a shape's hydrate `as_of`
        // — and set a LEVEL out of pre-hydrate content. Two tasks racing
        // `emit_status` for one thread is all it takes: the slower read wins
        // the stamp and the mirror shows a thread not executing while the lock
        // is held. Stamping before the read makes the stamp a LOWER BOUND on
        // the content's age, which is what the shape's stamp rule assumes.
        //
        // The write-then-emit sites (`emit`, `move_thread`) do not need this:
        // their content is committed before the emit by construction.
        let at = crate::clock::now();
        let Ok(s) = self.thread_status(thread_id) else {
            if answers {
                let _ = self
                    .promises
                    .abandon_all::<crate::commands::ThreadQueueFact>(Some(thread_id));
            }
            return Visible::Never;
        };
        self.hub.emit_at(at, Push::Status {
            thread_id: thread_id.to_string(),
            state: s["state"].as_str().unwrap_or("idle").to_string(),
            executing: s["executing"].as_bool().unwrap_or(false),
            queued: s["queued"].as_u64().unwrap_or(0),
            unread: s["unread"].as_u64().unwrap_or(0),
            detail: s["detail"].as_str().map(str::to_string),
            // WHERE, on the frame that says WHETHER. See
            // `Push::Status::account`: without this a client learns a thread
            // started and has to ask `threads.get` to learn which account's
            // running count just moved — which is the follow-up the account
            // board skipped, which is why its counts only moved on refresh.
            account: self.account_of(thread_id),
        });
        // R9.TS1: a status edge can move this thread's and every ancestor's
        // subtree answer — re-state the chain (`shapes::threads::RolledRow`).
        self.emit_rolled(thread_id);
        self.emit_thread_queue(at, thread_id, answers)
    }

    /// **ONE THREAD'S UNDELIVERED QUEUE — `waiting` then `failed`.**
    ///
    /// The read behind `crate::shapes::threads`' `queue` cell, and behind the two
    /// RPCs that cell replaced: `threads/{id}/queue` projected the `waiting`
    /// half and `threads/{id}/queue_failed` the `failed` half, out of the same
    /// `store::QueueRow` and with the same four fields. They are one list here
    /// because the only thing that differed was the state they filtered on, and
    /// `QueueRow` carries that state as a column.
    ///
    /// `metadata` and `headers` are on the stored row and are dropped here: see
    /// `crate::shapes::threads::QueueEntry` for why neither may be published.
    pub(crate) fn thread_queue(
        &self,
        thread_id: &str,
    ) -> Result<Vec<crate::shapes::threads::QueueEntry>> {
        let entry = |q: crate::store::QueueRow| crate::shapes::threads::QueueEntry {
            message_id: q.message_id,
            text: q.text,
            intent: q.intent,
            created_at: q.created_at,
            state: q.state,
        };
        let mut out: Vec<_> = self
            .store
            .waiting_queue(thread_id)?
            .into_iter()
            .map(entry)
            .collect();
        out.extend(
            self.store
                .failed_deliveries(thread_id)?
                .into_iter()
                .map(entry),
        );
        Ok(out)
    }

    /// **THE QUEUE FACT, EMITTED WHEREVER THE STATUS IS.**
    ///
    /// It rides [`Self::emit_status`] rather than sitting at each of the six
    /// writes that can move the queue (enqueue, deliver, restore, fail,
    /// re-select, cancel), and that is not convenience — it INHERITS AN
    /// INVARIANT THE SYSTEM ALREADY MAINTAINS AND TESTS. Every one of those six
    /// changes how many rows are `waiting`, `Push::Status` carries `queued`, and
    /// `crate::shapes::threads`' `queued` cell folds it — so a mutation site that
    /// did not reach `emit_status` would ALREADY be leaving that count stale. A
    /// seventh write would have to break the existing cell before it could break
    /// this one, which is a far better guarantee than six call sites nothing
    /// checks.
    ///
    /// **Gated on `any_tracking`, like every other fact**, so a daemon nobody is
    /// watching pays nothing for it. When something IS watching, the cost is two
    /// index seeks on `idx_queue_thread_state` per status change — not per
    /// second, and not a scan. Stated rather than hidden: this is the one place
    /// `emit_status` touches the queue TABLE rather than the derived status it
    /// already read.
    ///
    /// **`answers`** — whether this emit closes the `ThreadQueueFact` promises
    /// about `thread_id` (see [`Self::emit_status_answering`]). When it does,
    /// the resolve is an ARGUMENT of the emit, and both early returns ABANDON
    /// rather than fall through, so a promise is never closed in silence.
    fn emit_thread_queue(
        &self,
        at: shapewire::wire::Timestamp,
        thread_id: &str,
        answers: bool,
    ) -> Visible {
        let abandon = || {
            if answers {
                let _ = self
                    .promises
                    .abandon_all::<crate::commands::ThreadQueueFact>(Some(thread_id));
            }
            Visible::Never
        };
        if !self.hub.any_tracking() {
            return abandon();
        }
        // A queue we cannot read is not a queue we may describe. The hydrate's
        // own `Partial` is where that becomes visible; a fact invented here
        // would be a value with no source.
        let Ok(rows) = self.thread_queue(thread_id) else {
            return abandon();
        };
        // **The resolve is an ARGUMENT of the emit, never a statement above it.**
        self.hub.core().emit_resolving(
            at,
            crate::shape::Input::Fact(Fact::ThreadQueue {
                thread_id: thread_id.to_string(),
                rows,
            }),
            if answers {
                self.promises
                    .resolve::<crate::commands::ThreadQueueFact>(Some(thread_id))
            } else {
                crate::shape::Resolving::none()
            },
        );
        Visible::Expected
    }

    // ── THE THREAD-SHAPE PRODUCERS ───────────────────────────────────────
    //
    // The five thread facts, each emitted AFTER its durable write and gated on
    // `hub.any_tracking()` so a machine nobody is watching pays nothing.
    //
    // **Every one of these sites was SILENT before.** `Push::Status` announces
    // the derived state and nothing else, so the header, the settings blob, the
    // placement column, the seen cursor and the compaction marker were written
    // and then announced by nothing — or, in `SettingsChanged`'s case, announced
    // as a PATCH, which is not a value a level can hold. A fold built on what
    // existed would have held cells that quietly stopped moving.

    /// **THE ONE FUNNEL FOR A DURABLE THREAD-ROW WRITE.** One read, three facts.
    ///
    /// `upsert_thread` and `update_thread_settings` are the only two writers of
    /// the `threads` row's mutable columns, and both write `settings_json`,
    /// `account`, `cwd`, `model` and `updated_at` together — so the honest unit is
    /// the row, read once and published as the three cells it feeds. Splitting it
    /// into "emit only what I think I changed" is how a level goes stale: the
    /// caller that forgets one field leaves a cell behind, and the three cells
    /// coalesce into ONE `row` frame per key anyway (levels absorb a
    /// re-assertion for free — `docs/shapes.md` §2.4).
    ///
    /// **The stamp is taken BEFORE the read**, `emit_status`' reason exactly: a
    /// value read at t0 must not arrive stamped t3, after a hydrate's `as_of`,
    /// and set a level out of pre-hydrate content.
    ///
    /// **AND IT IS WHERE `threads/{thread_id}/update` RESOLVES** (rule 2.4,
    /// 2026-09-28). `ThreadSettings` is the one of the three that carries what
    /// that command moved, so it is the one the resolve rides. The other two go
    /// out unresolved and always did: a settings write that also moved placement
    /// is two facts, and only one of them is the answer to this call.
    ///
    /// Both early returns ABANDON rather than fall through. The `get_thread`
    /// failure is the one that matters: without it a promise opened by a command
    /// whose row then became unreadable stayed open forever, which is the leak
    /// `PromiseBook::open_ids` reported on `gauge.probe`'s `Err` arm and no test
    /// triggered.
    pub(crate) fn emit_thread_facts(&self, thread_id: &str) -> Visible {
        if !self.hub.any_tracking() {
            let _ = self
                .promises
                .abandon_all::<crate::commands::ThreadSettingsFact>(Some(thread_id));
            return Visible::Never;
        }
        let at = crate::clock::now();
        let Ok(Some(row)) = self.store.get_thread(thread_id) else {
            // A row we cannot read is not a row we may describe. The hydrate's
            // own `Partial` is where an unreadable thread becomes visible; a
            // fact invented here would be a value with no source.
            let _ = self
                .promises
                .abandon_all::<crate::commands::ThreadSettingsFact>(Some(thread_id));
            return Visible::Never;
        };
        self.hub.emit_fact_at(
            at,
            Fact::ThreadHeader {
                thread_id: thread_id.to_string(),
                row: crate::shapes::threads::header_of(&row),
            },
        );
        // **The resolve is an ARGUMENT of the emit, never a statement above it.**
        self.hub.core().emit_resolving(
            at,
            crate::shape::Input::Fact(Fact::ThreadSettings {
                thread_id: thread_id.to_string(),
                // THE SAME EXPRESSION `thread_json` PUTS ON THE WIRE. See
                // `shapes::threads::ThreadCells::settings`: a re-serialised blob
                // would put AUI into a patch loop on every turn.
                settings: crate::shapes::threads::settings_of(&row),
            }),
            self.promises
                .resolve::<crate::commands::ThreadSettingsFact>(Some(thread_id)),
        );
        self.hub.emit_fact_at(
            at,
            Fact::ThreadPlacement {
                thread_id: thread_id.to_string(),
                account: row.account.clone(),
            },
        );
        Visible::Expected
    }

    /// **THE COMPACTION MARKER, AFTER `update_thread_compaction`.**
    ///
    /// `range` is passed in rather than re-read, because both call sites already
    /// hold it: the `Compacted` arm has just computed it and `build_prompt` reads
    /// it two lines above for the restatement. That is also what keeps the cell to
    /// ONE writer — the fold does not read `ThreadEvent::Compacted`.
    /// **A PARENT'S MANAGED CHILDREN, AS A LEVEL** — `Fact::ThreadChildren`,
    /// after the durable ledger write that moved them (a spawn's commit point, a
    /// child's report). Built by the hydrate's own builder, off the ledger the
    /// caller just wrote, so no re-read.
    pub(crate) fn emit_children_fact(&self, parent: &crate::ledger::Ledger) {
        if !self.hub.any_tracking() {
            return;
        }
        self.hub.emit_fact_at(
            crate::clock::now(),
            Fact::ThreadChildren {
                thread_id: parent.thread_id().to_string(),
                row: crate::shapes::threads::children_row_at(parent, &self.store),
            },
        );
        // R9.TS1: a spawn or a report moves the parent's subtree.
        self.emit_rolled(parent.thread_id());
    }

    pub(crate) fn emit_compaction_fact(&self, thread_id: &str, range: Option<[u64; 2]>) {
        if !self.hub.any_tracking() {
            return;
        }
        let at = crate::clock::now();
        let Ok(Some(row)) = self.store.get_thread(thread_id) else {
            return;
        };
        self.hub.emit_fact_at(
            at,
            Fact::ThreadCompaction {
                thread_id: thread_id.to_string(),
                row: crate::shapes::threads::compaction_of(&row, range),
            },
        );
    }

    /// **ADVANCE THE SEEN CURSOR, AND SAY SO.**
    ///
    /// The store write plus its fact, in one place, because a cursor that moves
    /// without an emit is the defect this method exists to remove: `threads.seen`
    /// called `store.mark_seen` directly and then `emit_status`, and `Status`
    /// carries `unread` but not `seen_seq` — so the position moved in silence and
    /// only its consequence was announced.
    ///
    /// `unread` is recomputed HERE and carried on the fact. It is the one number
    /// the fold cannot maintain across this write: incrementing per addressable
    /// event is exact going forward, but a cursor JUMP swallows an unknown number
    /// of them, and the answer is a question about the log. `threads.seen` reads
    /// it anyway on the very next line to build its reply.
    ///
    /// Returns what the store returns — the cursor AFTER the monotonic merge, so a
    /// stale client cannot un-read what the operator has already heard.
    /// **AND IT IS WHERE `threads/{thread_id}/seen` RESOLVES** (rule 2.4,
    /// 2026-09-28). The reply used to be `thread_status(&id)` — the whole fresh
    /// status, justified as *"so a client never needs a second call"* — which is
    /// a second source for `unread` and `seen_seq`, two values this very fact
    /// carries and `ThreadCells` folds. The second call it saved is the one the
    /// shape makes unnecessary.
    pub fn mark_seen(&self, thread_id: &str, seq: u64) -> Result<(u64, Visible)> {
        let seen = self.store.mark_seen(thread_id, seq)?;
        if !self.hub.any_tracking() {
            // No fold is running, so no frame will carry these ids. ABANDON them
            // — a named act with a name on the reply — rather than closing them
            // in silence. See `emit_disabled_fact` for the whole argument.
            let _ = self
                .promises
                .abandon_all::<crate::commands::ThreadSeenFact>(Some(thread_id));
            return Ok((seen, Visible::Never));
        }
        // AFTER the write, and the unread count is read against the cursor
        // the store actually settled on rather than the one we asked for.
        let unread = self.store.unread_count(thread_id, seen).unwrap_or(0);
        // **The resolve is an ARGUMENT of the emit.** On its own line it would
        // survive the emit's deletion and empty the book anyway — which is how
        // the first two versions of the liveness test passed a forced-red pass
        // they should have failed.
        self.hub.emit_fact_resolving(
            self.promises
                .resolve::<crate::commands::ThreadSeenFact>(Some(thread_id)),
            Fact::ThreadSeen {
                thread_id: thread_id.to_string(),
                seen_seq: seen,
                unread,
            },
        );
        Ok((seen, Visible::Expected))
    }

    /// The message-id span the last compaction swallowed, in the shape the
    /// compaction cell carries. A thin `pub(crate)` face on
    /// [`Self::last_compaction_range`] so the shape's hydrate handler reads it
    /// through the same path `build_prompt` does.
    pub(crate) fn last_compaction_span(&self, thread_id: &str) -> Option<[u64; 2]> {
        self.last_compaction_range(thread_id)
            .map(|(from, to)| [from, to])
    }

    /// **THE BEAT'S LEVEL, OUT OF MEMORY.** The same `ThreadState::live` that
    /// `emit_liveness` broadcasts from, so the shape's read path and its live path
    /// cannot drift.
    ///
    /// A thread this process has not adopted answers the default row rather than
    /// nothing: no in-memory state means no turn running IN THIS PROCESS, and
    /// `busy: false` is that fact. (The durable lock is a different question and
    /// `status.executing` is where it is answered — a thread held by another
    /// daemon reads `executing: true` with `busy: false`.)
    /// **THE `subagents` CELL'S VALUE** (R9.TS2): the thread's in-memory Task
    /// roster. A thread this process has not adopted launched nothing here — an
    /// empty map, a complete answer (`liveness_row`'s reasoning).
    pub(crate) fn subagent_rows(
        &self,
        thread_id: &str,
    ) -> std::collections::BTreeMap<String, crate::shapes::threads::SubagentRow> {
        let g = self.threads.lock().unwrap();
        match g.get(thread_id) {
            Some(s) => s.live.lock().unwrap().tasks.rows(),
            None => Default::default(),
        }
    }

    /// **TC-44: A THREAD APPENDED AN EVENT AT `ts`** — if it is a managed
    /// child, tell its parent's `children` cell (`Fact::ThreadChildActivity`),
    /// at most once per wall-clock second per child. Gated on the `threads`
    /// shape tracking. The parent is read once per thread (`Managed::parent_of`)
    /// and cached: lineage never moves.
    fn note_activity(&self, thread_id: &str, ts: &str) {
        if !self.hub.tracked(
            <crate::shapes::threads::Threads as crate::shape::Shape<crate::shape::Input>>::NAME,
        ) {
            return;
        }
        let sec = ts.get(..19).unwrap_or(ts).to_string();
        let cached = self.child_activity.lock().unwrap().get(thread_id).cloned();
        let parent = match cached {
            Some((None, _)) => return,
            Some((_, held)) if held >= sec => return,
            Some((p, _)) => p,
            None => self.managed().parent_of(thread_id),
        };
        self.child_activity
            .lock()
            .unwrap()
            .insert(thread_id.to_string(), (parent.clone(), sec));
        if let Some(p) = parent {
            self.hub.emit_fact_at(
                crate::clock::now(),
                Fact::ThreadChildActivity {
                    thread_id: p,
                    child_id: thread_id.to_string(),
                    at: ts.to_string(),
                },
            );
        }
    }

    /// `Fact::ThreadSubagents` — the whole roster, after it changed. Gated on
    /// the `threads` shape tracking, like `emit_rolled`; the fold drops a map
    /// that did not change.
    fn emit_subagents(&self, state: &ThreadState) {
        if !self.hub.tracked(
            <crate::shapes::threads::Threads as crate::shape::Shape<crate::shape::Input>>::NAME,
        ) {
            return;
        }
        let rows = state.live.lock().unwrap().tasks.rows();
        self.hub.emit_fact_at(
            crate::clock::now(),
            Fact::ThreadSubagents {
                thread_id: state.id.clone(),
                rows,
            },
        );
    }

    pub(crate) fn liveness_row(&self, thread_id: &str) -> crate::shapes::threads::LivenessRow {
        let g = self.threads.lock().unwrap();
        match g.get(thread_id) {
            Some(s) => {
                let l = s.live.lock().unwrap();
                crate::shapes::threads::LivenessRow {
                    busy: l.busy(),
                    parent_running: l.parent_running,
                    running_subagents: l.running(),
                }
            }
            None => crate::shapes::threads::LivenessRow::default(),
        }
    }

    /// **The account a thread is on, for a frame that is about to say the
    /// thread moved.** `None` means genuinely unknown, never a guess.
    ///
    /// Two sources, in this order, and the order is the point:
    ///
    /// 1. **The in-memory serving account** (`ThreadState::account`) — the live
    ///    truth, and the one `switch_account` and a failover update first.
    /// 2. **The durable `threads.account` column**, for a thread this process
    ///    has not hydrated into its map (a boot that has not re-adopted it yet,
    ///    or a thread nobody has touched since the restart).
    ///
    /// An empty in-memory string is treated as ABSENT and falls through to the
    /// durable row: a thread constructed before placement resolved carries `""`,
    /// and `""` is not an account name — reporting it as one would put an
    /// unnamed bucket on the operator's board.
    ///
    /// Best-effort by construction. This is called on the liveness/status
    /// broadcast path, which must never fail a turn: a store read that errors
    /// yields `None` and the frame goes out without the field, exactly as it did
    /// before the field existed.
    pub(crate) fn account_of(&self, thread_id: &str) -> Option<String> {
        let live = self
            .threads
            .lock()
            .unwrap()
            .get(thread_id)
            .map(|s| s.account.lock().unwrap().clone())
            .filter(|a| !a.is_empty());
        if live.is_some() {
            return live;
        }
        self.store
            .get_thread(thread_id)
            .ok()
            .flatten()
            .and_then(|r| r.account)
            .filter(|a| !a.is_empty())
    }

    // ── send + queue (Q4) ────────────────────────────────────────────────

    /// Accept a user message: persist `UserMessage` NOW (with its spoken
    /// message id + the caller's metadata), enqueue it with its intent, and
    /// dispatch if the thread can take a turn. Returns the message id.
    ///
    /// `bare`: deliver this message verbatim with no injected headers or
    /// preamble (slash-command turns like `/compact` — AUI's `bare_prompt`).
    pub fn send(
        &self,
        id: &str,
        text: String,
        intent: SendIntent,
        metadata: Value,
        bare: bool,
    ) -> Result<u64> {
        Ok(self
            .send_declaring(id, text, intent, metadata, bare, &Value::Null)?
            .0)
    }

    /// **THE DECLARATION ENTRY POINT** (INTENT 66b). Same as [`Self::send`],
    /// plus the header declarations the application wants threads to compose
    /// for this message. Returns the message id and any warnings about
    /// declarations that were REFUSED.
    ///
    /// The message is accepted either way. A malformed declaration is an
    /// application's bug and the operator must never pay for it with a lost
    /// turn, so refusals come back as warnings on an otherwise successful send
    /// rather than as an error. Validation happens HERE, once, and only the
    /// accepted result is stored — so the delivery path renders what was
    /// already judged and cannot reach a different verdict later.
    pub fn send_declaring(
        &self,
        id: &str,
        text: String,
        intent: SendIntent,
        metadata: Value,
        bare: bool,
        headers: &Value,
    ) -> Result<(u64, Vec<String>)> {
        let (declared, warnings) = context::validate_declarations(headers);
        for w in &warnings {
            tracing::warn!(thread_id = %id, warning = %w, "header declaration refused");
        }
        let state = self.get_state(id)?;
        let message_id = self.record_and_enqueue(&state, text, intent, metadata, bare, &declared)?;
        self.maybe_dispatch(&state);
        // Edge-triggered status: after dispatch, so the frame says `active`
        // when this send started a turn and `queued` when it did not — which
        // is the whole thing the operator asked to be able to hear.
        self.emit_status(id);
        Ok((message_id, warnings))
    }

    /// The `UserMessage` event and its `queue` row — [`Self::send_declaring`]
    /// minus the dispatch.
    fn record_and_enqueue(
        &self,
        state: &Arc<ThreadState>,
        text: String,
        intent: SendIntent,
        metadata: Value,
        bare: bool,
        declared: &[context::HeaderDeclaration],
    ) -> Result<u64> {
        let message_id = self.alloc_message_id(state)?;
        // R10.M1: THE APPEND AND THE QUEUE ROW ARE ONE TRANSACTION. They were
        // two writes, and a kill between them left the words on the ledger
        // with no row any dispatcher would ever select — the one instant
        // between append and `turn_started` at which a message was lost.
        let ev = self.sequenced_event(
            state,
            Some(message_id),
            ThreadEvent::UserMessage {
                text: text.clone(),
                intent,
                metadata: metadata.clone(),
            },
        );
        let intent_tag = if bare {
            format!("{}_bare", intent.as_str())
        } else {
            intent.as_str().to_string()
        };
        // The blob rides the QUEUE ROW too, not just the ledger event: delivery
        // happens later (possibly after a restart), and the metadata block is
        // composed at delivery from the row in hand.
        self.store.enqueue_recorded(
            &state.id,
            message_id,
            &text,
            &intent_tag,
            &metadata,
            declared,
            Some(&ev),
        )?;
        self.published(state, ev);
        Ok(message_id)
    }

    /// **ENQUEUE WITHOUT DISPATCHING** (R4.S3-land): the durable half of
    /// [`Self::send`] only — the event and the queue row. For a caller that is
    /// itself on the dispatch path (`crate::networks::release_held`, called
    /// from `maybe_dispatch`), where a dispatch would re-enter it.
    pub(crate) fn enqueue_undispatched(
        &self,
        id: &str,
        text: String,
        intent: SendIntent,
        metadata: Value,
    ) -> Result<u64> {
        let state = self.get_state(id)?;
        self.record_and_enqueue(&state, text, intent, metadata, false, &[])
    }

    /// Append an application-defined event (§11). `addressable` allocates a
    /// spoken message id.
    ///
    /// **THE MESSAGE ID NO LONGER RIDES THE REPLY** (rule 2.4, 2026-09-28).
    /// `threads/{thread_id}/event` answered `{thread_id, message_id}`, and
    /// `message_id` is the allocated addressable POSITION — a fact the `threads`
    /// shape holds as `ThreadCells::last_message_id`, folded out of the very
    /// `Push::Event` this function emits. Two sources for one number, and the
    /// reply was the one that could be stale by the time it was read. It is
    /// still returned HERE, to the crate's own callers, because the fold needs
    /// it on the envelope and the queue path needs it to enqueue; what changed
    /// is that the WS handler no longer puts it on the wire.
    pub fn append_custom(
        &self,
        id: &str,
        event_type: String,
        data: Value,
        addressable: bool,
    ) -> Result<(Option<u64>, Visible)> {
        let state = self.get_state(id)?;
        let message_id = if addressable {
            Some(self.alloc_message_id(&state)?)
        } else {
            None
        };
        if !self.hub.any_tracking() {
            let _ = self
                .promises
                .abandon_all::<crate::commands::ThreadMessageFact>(Some(id));
            self.emit(&state, message_id, ThreadEvent::Custom { event_type, data });
            return Ok((message_id, Visible::Never));
        }
        // **THE RESOLVE IS AN ARGUMENT OF THE EMIT.** `emit_resolving` is the
        // same store-append-then-publish as `emit`, with the routed ids handed
        // to the publish rather than to a statement beside it — so deleting the
        // emit takes the resolve with it and the liveness test goes red.
        self.emit_resolving(
            &state,
            message_id,
            ThreadEvent::Custom { event_type, data },
            self.promises
                .resolve::<crate::commands::ThreadMessageFact>(Some(id)),
        );
        Ok((message_id, Visible::Expected))
    }

    /// Cancel a still-waiting queued message by its message id.
    ///
    /// **AND IT IS WHERE `threads/{thread_id}/queue_cancel` RESOLVES** (rule
    /// 2.4, R2.S5). The reply used to carry `cancelled`; whether a row moved is
    /// now the reply's `changed`, and the queue itself is `ThreadCells::queue`.
    ///
    /// A cancel that found no waiting row still emits the queue level — a
    /// no-op that emits nothing cannot be told from one that never ran — but
    /// not a STATUS edge, because the status did not move.
    ///
    /// **R10.M1: THIS IS A WITHDRAW** ([`crate::store::Store::queue_withdraw`]).
    /// The row's dispatch becomes `never` and a `message_withdrawn{by:
    /// operator}` event lands in the same transaction; the text stays.
    pub fn cancel_queued(&self, id: &str, message_id: u64) -> Result<(bool, Visible)> {
        let out = match self.get_state(id) {
            Ok(state) => {
                let ev = self.sequenced(
                    &state,
                    crate::event::delivery::WITHDRAWN,
                    serde_json::json!({
                        "message_ids": [message_id],
                        "by": "operator",
                        "dispatch_now": crate::event::delivery::NEVER,
                    }),
                );
                let moved = self.store.queue_withdraw(id, message_id, Some(&ev))?;
                if moved {
                    self.published(&state, ev);
                }
                moved
            }
            // No live thread state (unknown id): there is nothing to sequence
            // an event against, and nothing for a dispatcher to select either.
            Err(_) => self.store.queue_withdraw(id, message_id, None)?,
        };
        let visible = if out {
            self.emit_status_answering(id, true)
        } else {
            self.emit_thread_queue(crate::clock::now(), id, true)
        };
        Ok((out, visible))
    }

    /// **Put `failed` rows back on `waiting`, and say so.** The write behind
    /// `threads/{thread_id}/queue_reselect`: the store move, then — when a row
    /// moved — the ordinary dispatcher nudge, with the status edge it emits
    /// carrying the resolving queue fact.
    ///
    /// Returns how many rows moved, which the WS handler turns into `changed`
    /// and does not put on the wire. A re-select that moved nothing emits the
    /// queue level anyway, for the reason [`Self::cancel_queued`] does.
    pub fn reselect_failed(&self, id: &str, message_ids: &[u64]) -> Result<(usize, Visible)> {
        let moved = self.store.reselect_failed(id, message_ids)?;
        let visible = if moved > 0 {
            self.nudge_thread_answering(id, true)
        } else {
            self.emit_thread_queue(crate::clock::now(), id, true)
        };
        Ok((moved, visible))
    }

    /// **RESEND withdrawn or failed rows** (R10.M1): the store move
    /// ([`crate::store::Store::queue_resend`]), the `message_resent` event that
    /// records the dispatch change, and the ordinary dispatcher nudge. Returns
    /// the message ids that moved.
    pub fn queue_resend(&self, id: &str, message_ids: &[u64]) -> Result<Vec<u64>> {
        let moved = self.store.queue_resend(id, message_ids)?;
        if moved.is_empty() {
            return Ok(moved);
        }
        if let Ok(state) = self.get_state(id) {
            self.emit(
                &state,
                None,
                ThreadEvent::Custom {
                    event_type: crate::event::delivery::RESENT.to_string(),
                    data: serde_json::json!({ "message_ids": moved, "by": "operator" }),
                },
            );
        }
        self.nudge_thread(id);
        Ok(moved)
    }

    /// A ledger envelope for a `cauldron.threads/*` delivery event, with its
    /// seq ALLOCATED but not yet written — for a store call that writes it in
    /// the same transaction as the queue move it records. Hand it to
    /// [`Self::published`] once that transaction has committed.
    fn sequenced(&self, state: &ThreadState, event_type: &str, data: Value) -> StoredEvent {
        self.sequenced_event(
            state,
            None,
            ThreadEvent::Custom {
                event_type: event_type.to_string(),
                data,
            },
        )
    }

    /// [`Self::sequenced`] for any event.
    fn sequenced_event(
        &self,
        state: &ThreadState,
        message_id: Option<u64>,
        event: ThreadEvent,
    ) -> StoredEvent {
        let seq = state.seq.fetch_add(1, Ordering::SeqCst) + 1;
        StoredEvent {
            thread_id: state.id.clone(),
            seq,
            message_id,
            ts: crate::store::now_rfc3339(),
            event,
        }
    }

    /// The rest of [`Self::emit_resolving`] for an envelope a store
    /// transaction already wrote: the watermark, the terminal observer, and
    /// the live push. Nothing here persists the event a second time.
    fn published(&self, state: &ThreadState, ev: StoredEvent) {
        // The recency stamp exactly as `emit_resolving` derives it.
        let user_input_at = ev.event.is_user_message().then(|| ev.ts.clone());
        let _ = self
            .store
            .bump_last_seq(&state.id, ev.seq, user_input_at.as_deref());
        state.turn_terminal.lock().unwrap().observe(&ev.event);
        self.note_activity(&state.id, &ev.ts);
        self.hub.emit_push_resolving(
            crate::clock::now(),
            crate::shape::Resolving::none(),
            Push::Event(ev),
        );
    }

    /// **OPEN AN ATTEMPT on a dispatch batch** (R10.M1, §5.3 rule 4). The
    /// `delivery_attempted{attempt, message_ids, transport: "stdin",
    /// resend_of}` event and the rows' move to `attempted` are ONE transaction,
    /// committed (and, at the store's `synchronous`, fsynced) before the turn
    /// task exists — so before any byte can reach a process. `attempt` is
    /// `att:<seq>`, unique per thread. `resend_of` maps each message that an
    /// earlier attempt already named to that attempt.
    ///
    /// Public (doc-hidden) so a test can stop a dispatch at exactly this
    /// instant: the residue it leaves is what a kill right after the commit
    /// leaves.
    #[doc(hidden)]
    pub fn open_attempt_for_rows(&self, id: &str, batch: &[QueueRow]) -> Result<String> {
        let state = self.get_state(id)?;
        self.open_attempt(&state, batch)
    }

    fn open_attempt(&self, state: &ThreadState, batch: &[QueueRow]) -> Result<String> {
        let message_ids: Vec<u64> = batch.iter().map(|q| q.message_id).collect();
        let resend_of: serde_json::Map<String, Value> = if batch.iter().any(|q| q.attempts > 0) {
            self.store
                .last_attempt_ids(&state.id, &message_ids)?
                .into_iter()
                .map(|(mid, att)| (mid.to_string(), Value::String(att)))
                .collect()
        } else {
            serde_json::Map::new()
        };
        let mut ev = self.sequenced(state, crate::event::delivery::ATTEMPTED, Value::Null);
        let attempt = format!("att:{}", ev.seq);
        ev.event = ThreadEvent::Custom {
            event_type: crate::event::delivery::ATTEMPTED.to_string(),
            data: serde_json::json!({
                "attempt": attempt,
                "message_ids": message_ids,
                "transport": "stdin",
                "resend_of": resend_of,
            }),
        };
        let row_ids: Vec<i64> = batch.iter().map(|q| q.id).collect();
        self.store.mark_attempted(&row_ids, Some(&ev))?;
        self.published(state, ev);
        Ok(attempt)
    }

    /// **Ask the dispatcher to look at this thread now.**
    ///
    /// The entry point a RE-SELECTED delivery uses. It deliberately does not run
    /// a turn: it re-enters `maybe_dispatch`, so a message put back in the queue
    /// after its ladder ran out goes out on exactly the path a fresh message
    /// would, through the same admission gate, with no second dispatch route to
    /// keep in step.
    pub fn nudge_thread(&self, id: &str) {
        let _ = self.nudge_thread_answering(id, false);
    }

    /// [`Self::nudge_thread`], with the status edge resolving the queue
    /// promises when `answers` — see [`Self::emit_status_answering`].
    fn nudge_thread_answering(&self, id: &str, answers: bool) -> Visible {
        let Ok(state) = self.get_state(id) else {
            if answers {
                let _ = self
                    .promises
                    .abandon_all::<crate::commands::ThreadQueueFact>(Some(id));
            }
            return Visible::Never;
        };
        let visible = self.emit_status_answering(id, answers);
        self.maybe_dispatch(&state);
        visible
    }

    /// Dispatch waiting queue rows if the thread can take a turn.
    ///
    /// Intent semantics (Q4): `immediate` delivers when no PARENT turn is in
    /// flight (a new turn may start while async subagents still run — the
    /// natural Claude Code behavior); `turn_end` waits for full idleness.
    /// A pending scheduled retry owns the next spawn — new sends queue behind
    /// it rather than double-driving the session.
    ///
    /// THE ADMISSION SEAM (CON-4/DEC-8): budget + threshold enforcement lives
    /// here, and a blocked turn is PARKED with a queryable reason — the queue
    /// rows stay `waiting` and the ledger records a `turn_parked` event.
    /// CCW's `admit()` silently discarded the blocked turn; that failure mode
    /// is structurally impossible here because nothing is dequeued until the
    /// gate has already said yes.
    ///
    /// **Returns whether it ACTUALLY STARTED A TURN** — not whether it looked.
    /// Every caller but one ignores it; [`Self::resume_sweep_placing`] is the
    /// one, and it needs the distinction because "the backstop poll ran" is
    /// noise while "the backstop poll placed a thread no event woke it for" is a
    /// defect report. `true` only after the lock is taken and the run task is
    /// spawned, so a park, a refusal and an empty queue are all `false`.
    fn maybe_dispatch(&self, state: &Arc<ThreadState>) -> bool {
        // A COURTESY, NOT THE GATE. When a restart window is open this saves a
        // pointless admission evaluation (and the `turn_resumed` event it would
        // emit) for work `lock_turn` is about to refuse anyway. Deleting this
        // line would not create a race: the permit inside `lock_turn` is what
        // makes "nothing can start" true, and it is taken under the same mutex
        // the window holds across its `execv`. See `update.rs`.
        if !self.update.admitting() {
            return false;
        }
        if state.turn_running.load(Ordering::SeqCst) {
            return false;
        }
        // **AN ORDERING RULE, NOT A FLOOR — AND IT IS WORTH SAYING SO, BECAUSE
        // THIS LINE HAS BEEN READ AS THE FLOOR GATING THE EVENT SET.**
        //
        // It does not read `retry_at`. It reads whether a retry row EXISTS, and a
        // row exists for exactly as long as a prompt is waiting in `retries` — so
        // a floor that elapsed a second ago defers the queue exactly as hard as
        // one a day out. What it enforces is the documented ordering above: a
        // pending retry owns the next spawn, and messages sent while a thread is
        // retrying queue BEHIND the prompt that is already waiting rather than
        // overtaking it. Deleting it would reorder the operator's own messages,
        // and worse: `placement_pass` runs the parked sweep BEFORE the retry
        // sweep, so the queue row would take the lock and the retry — a `Sweep`
        // origin, which deliberately does not re-arm on a lost lock — would be
        // stranded until the next bell.
        //
        // The thing that WAS the floor gating the event set is the retry sweep's
        // selection, and it is fixed there: see `retries_to_reask_on_event`. With
        // that in place this line strands nothing, because the retry whose row is
        // deferring the queue is itself re-asked in the same pass.
        if matches!(self.store.get_retry(&state.id), Ok(Some(_))) {
            return false;
        }
        // A turn killed mid-flight by a budget trip owns the next dispatch:
        // its prompt was parked verbatim and must go back out first.
        if let Ok(Some(parked)) = self.store.get_parked_turn(&state.id) {
            // A REFUSED PROJECTION WAITS FOR ITS INPUTS TO MOVE (or the hold
            // to lapse) — a cause gate, ahead of the admission gate, so a
            // held turn emits nothing at all. See `ThreadState::projection_hold`.
            {
                let mut hold = state.projection_hold.lock().unwrap();
                if let Some((fp, at)) = hold.as_ref() {
                    if *fp == self.hold_fingerprint(state, fp) && at.elapsed() < self.tunables.projection_hold {
                        return false;
                    }
                    *hold = None;
                }
            }
            if !self.admit_or_park(state, &parked.message_ids) {
                return false;
            }
            // ENTRY POINT: parked-turn redelivery. Locked like every other.
            let Ok(lock) = self.lock_turn(state, "parked") else {
                return false;
            };
            let _ = self.store.clear_parked_turn(&state.id);
            // THE REDELIVERY'S ATTEMPT IS OPEN (R10.M2c fix 2, TC-45): the row
            // is gone, so from here until the task takes this back a cancel
            // finds it in `cancel_turn`'s open-attempt arm — the redelivery
            // ends undelivered and its messages RIDE the operator's next send.
            // It names what that cancel may ride: only the messages the model
            // never saw (R13.1, as the parked arm of `cancel_turn`). Nothing
            // can be delivered before the task takes it back, so reading it
            // here is reading it at the cancel.
            *state.open_attempt.lock().unwrap() =
                Some(self.undelivered_of(&state.id, &parked.message_ids));
            // `admit_or_park` already emitted the `turn_resumed` event.
            let mgr = self.clone();
            let st = state.clone();
            tokio::spawn(async move {
                let account = st.account.lock().unwrap().clone();
                mgr.edge_poll_open(&account, Some(st.as_ref())).await;
                if st.open_attempt.lock().unwrap().take().is_none() {
                    tracing::info!(thread = %st.id, "the operator cancelled a parked redelivery before it reached the process — its messages ride the next turn");
                    mgr.finish_turn(st, lock).await;
                    return;
                }
                mgr.run_turn(st, parked.prompt, parked.message_ids, parked.attempt, lock)
                    .await;
            });
            // PLACED. The lock is taken and the turn task is spawned, which is
            // the only point in this function where the answer to "did the sweep
            // actually move anything" becomes yes for a PARKED turn.
            return true;
        }
        let mut waiting = match self.store.waiting_queue(&state.id) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(error = %e, "queue read failed");
                return false;
            }
        };
        // Only riders (R10.M2): nothing here is a reason to start a turn —
        // and releasing held mail below would manufacture one.
        if waiting.iter().all(|q| q.dispatch == crate::event::delivery::RIDE) {
            return false;
        }
        // ── R4.S3-land: HELD NETWORK MAIL RIDES THE TURN THAT IS STARTING ──
        //
        // Mail to a dormant member that is not `wakeable` to its sender is held
        // (`network_held_mail`), deliberately NOT a queue row, so nothing wakes
        // on it. A turn is about to start here for some other reason — so the
        // held mail is released into the queue now, behind what is already
        // waiting, and the queue is read again. `release_held` enqueues WITHOUT
        // dispatching, so this cannot re-enter `maybe_dispatch`.
        match crate::networks::release_held(self, &state.id) {
            Ok(0) => {}
            Ok(_) => match self.store.waiting_queue(&state.id) {
                Ok(w) => waiting = w,
                Err(e) => {
                    tracing::warn!(error = %e, "queue re-read after releasing held mail failed");
                    return false;
                }
            },
            Err(e) => tracing::warn!(thread_id = %state.id, error = %e, "releasing held network mail failed"),
        }
        // ── R4.S2: THE WAIT IS THE LEDGER FOLD, NOT `busy()` ─────────────
        //
        // Under the flag, "every subagent has gone quiet" is now asked of the
        // parent's DURABLE ledger — `managed::Managed::is_waiting`, the
        // `child{started}`-without-`child{finished}` fold — which survives a
        // restart, instead of `Liveness::busy()`, an in-memory registry kept
        // true by a poll (deleted). Native in-process agents need no term: they
        // cannot outlive the parent's process, so `parent_running` covers them.
        //
        // The history below is kept for the reasoning; its last paragraph is
        // what this replaced.
        //
        // THE ONE LINE THE LIVENESS REPAIR COULD MOVE. `busy()` is
        // `parent_running || !outstanding.is_empty()`, and `outstanding` was
        // dead — never populated, because the parser read `toolUseResult` while
        // the CLI emits `tool_use_result`. So `turn_end` and `immediate` have
        // always meant the same thing in practice.
        //
        // Now that `outstanding` is real, this is the ONLY place where that
        // reality would change the daemon's behaviour rather than its
        // reporting: `finish_turn`, `spawn_heartbeat` and
        // `poll_subagents_until_idle` all read the truthful `busy()` and all of
        // them only observe — they sweep and they broadcast. (`live_status` was
        // the fourth, and it SET a status string; it is deleted along with the
        // column it wrote — `busy()` is liveness, not execution, and reporting
        // it as execution is precisely how a finished turn stayed `busy`.) This
        // line decides whether a queued message goes out.
        //
        // So the guard is exactly here, and it is a guard on DISPATCH TIMING,
        // not on the accounting. Default off = today's behaviour, and the
        // config field says why.
        let parent_running = state.live.lock().unwrap().parent_running;
        let fully_idle = if self.config.turn_end_waits_for_subagents {
            !parent_running && !self.managed.is_waiting(&state.id)
        } else {
            !parent_running
        };
        // ── RIDERS NEVER START A TURN; THEY GO FIRST IN ONE (R10.M2, L-8) ──
        //
        // A `ride` row is a message an operator cancel deferred: it is not a
        // reason to run, so it is set aside here and only the remaining rows
        // decide whether a turn starts. When one does, every rider goes out
        // ahead of the rows that caused it, oldest first (`waiting_queue` is
        // in queue order) — `build_prompt` keeps the order it is handed.
        let (riders, waiting): (Vec<QueueRow>, Vec<QueueRow>) =
            waiting.into_iter().partition(|q| q.dispatch == crate::event::delivery::RIDE);
        // A bare message must be delivered ALONE (slash commands lead the
        // prompt); deliver everything before it, then it, in separate turns.
        let mut batch: Vec<QueueRow> = Vec::new();
        let mut bare_batch = false;
        for q in waiting {
            let deliverable = q.intent.starts_with("immediate") || fully_idle;
            if !deliverable {
                break; // preserve order: don't deliver around a held message
            }
            let is_bare = q.intent.ends_with("_bare");
            if is_bare {
                if batch.is_empty() {
                    batch.push(q);
                    bare_batch = true;
                }
                break;
            }
            batch.push(q);
        }
        if batch.is_empty() {
            return false;
        }
        // A bare message still travels alone: riders wait for the next
        // non-bare turn.
        if !bare_batch && !riders.is_empty() {
            let mut carried = riders;
            carried.append(&mut batch);
            batch = carried;
        }
        // The gate runs BEFORE anything is dequeued: a park leaves every row
        // exactly where it was (CON-4).
        let pending_ids: Vec<u64> = batch.iter().map(|q| q.message_id).collect();
        if !self.admit_or_park(state, &pending_ids) {
            return false;
        }
        // ENTRY POINT: the queue drain. The lock is taken BEFORE a single row
        // is marked delivered, so a refusal leaves every message exactly where
        // it was — queued, in order, for whenever the thread frees. This is the
        // path the duplicate-instance bug came in on (msg #1199/#1213): the
        // client is told "accepted", and accepted now means "queued behind the
        // lock", never "running twice".
        let Ok(lock) = self.lock_turn(state, "dispatch") else {
            return false;
        };
        // R10.M1: `mark_delivered` became `open_attempt` — the attempt event
        // and the row move in one transaction, before the turn task exists.
        // DELIVERED is not written anywhere: it is the `turn_started` that
        // `run_turn` emits naming these ids.
        //
        // A failed attempt write leaves the rows `waiting` and must not start
        // a turn the ledger never saw attempted: release the lock and stop.
        if let Err(e) = self.open_attempt(state, &batch) {
            tracing::warn!(error = %e, "opening a delivery attempt failed; rows stay queued");
            drop(lock);
            return false;
        }
        // ── SILENT WRITER 4: `queued` DECREMENTED ON EVERY DELIVERY ──────
        //
        // The worst of the five, because it is the ordinary path rather than an
        // edge case. `lock_turn` above emitted a `Status` BEFORE these rows left
        // `waiting`, so the frame carries the PRE-decrement count, and the next
        // edge is `TurnLock::release` when the turn ends — which can be minutes.
        // Every consumer therefore showed a queue depth that was too high for the
        // whole duration of the turn that had just drained it.
        //
        // `state` does not move here (`executing` already outranks everything), so
        // this frame exists for the COUNT. That is why it is a second
        // `emit_status` and not a rearrangement of the first.
        self.emit_status(&state.id);
        let (prompt, message_ids) = self.build_prompt(state, &batch, bare_batch);
        // THE ATTEMPT IS OPEN (R10.M2): from here until the dispatch task
        // takes it back, a cancel ends it undelivered instead of racing it.
        *state.open_attempt.lock().unwrap() = Some(message_ids.clone());
        let mgr = self.clone();
        let st = state.clone();
        tokio::spawn(async move {
            // The OPENING edge (§3.2) runs inside the dispatch task, before the
            // turn: `turn_running` is already set, so nothing races it. It is a
            // no-op whenever the leading idle gap already holds a pair, which
            // the previous closing ladder normally left there. R10.M2c: the
            // wait on it is BOUNDED (`edge_open_bound`) and a cancel ends it at
            // once — the attempt is watched, not only the probe.
            let account = st.account.lock().unwrap().clone();
            mgr.edge_poll_open(&account, Some(st.as_ref())).await;
            let hold = st.attempt_hold.lock().unwrap().clone();
            if let Some(hold) = hold {
                hold.notified().await;
            }
            // TAKE THE ATTEMPT BACK, or find that a cancel took it first
            // (R10.M2, L-8). A cancel that won ended the attempt
            // `delivery_cancelled`, set its rows `ride` and recorded the
            // operator's interrupt: no turn starts, and this execution ends
            // here through the one epilogue every execution ends through.
            if st.open_attempt.lock().unwrap().take().is_none() {
                tracing::info!(thread = %st.id, "the operator cancelled this delivery before it reached the process — its messages ride the next turn");
                mgr.finish_turn(st, lock).await;
                return;
            }
            mgr.run_turn(st, prompt, message_ids, 1, lock).await;
        });
        // PLACED — the queue-drain half of the same answer.
        true
    }

    /// Assemble the outgoing prompt: context preambles (first-turn identity,
    /// post-compaction restatement §12, plugin-change notice) + per-message
    /// `[#id date time]` headers (Q7). Bare messages skip everything.
    fn build_prompt(
        &self,
        state: &Arc<ThreadState>,
        batch: &[QueueRow],
        bare: bool,
    ) -> (String, Vec<u64>) {
        let message_ids: Vec<u64> = batch.iter().map(|q| q.message_id).collect();
        if bare {
            return (batch[0].text.clone(), message_ids);
        }
        let (inj, cwd, app_id) = {
            let s = state.settings.lock().unwrap();
            (s.injection, s.cwd.clone(), s.app_id.clone())
        };
        let sections: Vec<context::PromptSection> = batch
            .iter()
            .map(|q| {
                let ts = DateTime::parse_from_rfc3339(&q.created_at)
                    .map(|t| t.with_timezone(&Utc))
                    .unwrap_or_else(|_| crate::clock::now());
                context::PromptSection {
                    message_id: q.message_id,
                    ts,
                    text: q.text.clone(),
                    // AUI-supplied, system-computed (INTENT 65). Absent when
                    // the caller sent no audio start — the line is then simply
                    // omitted; threads never invents this figure.
                    duration: context::duration_from_metadata(&q.metadata, ts),
                    // The application's own coordinate (INTENT 66c), read from
                    // the one declared key. Absent → no `idx:` line.
                    idx: context::app_idx_from_metadata(&q.metadata),
                    headers: q.headers.clone(),
                }
            })
            .collect();
        // THERE IS NO `if !inject` BRANCH ANY MORE, and deleting it is part of
        // the flattening rather than a tidy-up. It existed because
        // `inject_context: false` was a MASTER switch that had to synthesise a
        // whole second policy — level `none`, headers stripped, no preambles, no
        // context id — in a place that could drift from the real one. With one
        // flag per injected thing, "tell the model nothing extra" is just
        // `Injection::NOTHING` flowing down the SAME path, and every gate below
        // is the flag for the one thing it gates. Body scrutiny still fires
        // either way, which is why both paths always had to end in
        // `build_prompt_body`.
        let plugin_dirs: Vec<String> = {
            let s = state.settings.lock().unwrap();
            plugins::collect_plugin_dirs(&s)
                .into_iter()
                .map(|p| p.display().to_string())
                .collect()
        };
        let mut preambles: Vec<String> = Vec::new();
        let first_turn = !state.resumable.load(Ordering::SeqCst);
        if first_turn && inj.first_turn_preamble {
            preambles.push(context::first_turn_preamble(&state.id, &plugin_dirs));
        }
        let mut restated = false;
        // WHERE THE CURRENT CONTEXT STARTED, read off the same row the restate
        // check reads and nowhere else. The restate block below writes
        // `row.last_compaction_message_id` straight back unchanged, so this
        // value is the live boundary either way. `None` means the row could
        // not be read at all — in which case the `context:` line is omitted
        // rather than asserted as `.0`, because a wrong epoch is worse than a
        // missing one for something whose only job is to be a trustworthy
        // cache key.
        let mut boundary: Option<u64> = None;
        if let Ok(Some(row)) = self.store.get_thread(&state.id) {
            boundary = Some(row.last_compaction_message_id);
            // The flag gates the WHOLE block, consumption included, so a thread
            // with the restatement off keeps `needs_restate` up and will deliver
            // one if the flag is later turned on. That is exactly what
            // `inject_context: false` already did — the marker was never reached
            // on that path either — so it is parity, not a new rule.
            if row.needs_restate && inj.compaction_restatement {
                let pre_range = self.last_compaction_range(&state.id);
                preambles.push(context::compaction_restatement(
                    &state.id,
                    &cwd,
                    &plugin_dirs,
                    pre_range,
                    crate::clock::now(),
                ));
                let _ = self.store.update_thread_compaction(
                    &state.id,
                    row.last_compaction_message_id,
                    false,
                );
                // The restatement has been consumed, so `needs_restate` is down —
                // a durable change to the compaction marker with no announcement
                // of its own. `pre_range` is the range this very block just read,
                // so the fact costs no extra statement.
                self.emit_compaction_fact(
                    &state.id,
                    pre_range.map(|(from, to)| [from, to]),
                );
                restated = true;
            }
        }
        // The flag is tested FIRST so that short-circuiting also skips the
        // `swap`: a thread that does not announce plugin changes must not
        // silently consume the dirty marker either, which is again what
        // `inject_context: false` did by never reaching this line.
        if inj.plugin_changes && state.plugins_dirty.swap(false, Ordering::SeqCst) && !first_turn {
            preambles.push(context::plugins_changed_preamble(&plugin_dirs));
        }
        // DEC-55/56/57: threads' own system events, rendered LAST so the
        // envelope sits immediately above the operator's words it prefixes.
        //
        // `pending_system_notices` MINTS and PERSISTS the notices and clears the
        // interrupted flag, so the flag has to gate the call and not just the
        // push: a notice nobody was shown must not clear the state it reports.
        let now = crate::clock::now();
        if inj.system_notices {
            let notices = self.pending_system_notices(state, batch, now);
            if let Some(block) = context::assemble_model_notices(&notices, now) {
                preambles.push(block);
            }
        }
        // INTENT 65's payload policy. The thread id rides the first post of the
        // thread and the first post after a compaction — and BOTH conditions
        // key off state that already exists (`resumable` and the `needs_restate`
        // flag this build just consumed), so the policy adds no third source of
        // truth for "where am I in this conversation".
        // THE CONTEXT ID, derived, nowhere stored. It is computed here and not
        // inside `build_prompt_body` because the boundary is durable state and
        // `context.rs` is a renderer with no store access — the same split
        // every other durable input to the policy already obeys.
        let ctx_id = match (inj.context_id, boundary) {
            (true, Some(b)) => Some(context::context_id(&state.id, b)),
            _ => None,
        };
        // ── THE MID-CONTEXT CATCH-UP. INTERIM — DELETE ME, DO NOT PRESERVE ──
        //
        // The `context:` line rides the boundary posts, so a thread that turns
        // `inject_context_id` on in the MIDDLE of a context would not see the
        // line until its next compaction. The operator found that: a flag that
        // silently does nothing until an unpredictable future event is worse
        // than no flag.
        //
        // The RIGHT fix is not here. It is to rebuild the metadata for each user
        // message at turn start, so metadata becomes a PROJECTION rather than a
        // stored fact and every flag change applies retroactively and
        // immediately — and, with thread settings becoming an input to turn
        // start, a flag enabled at turn start would take effect on that same
        // turn with no catch-up logic at all. That work is being designed
        // separately (see `docs/`, the adapter/projection piece). **When it
        // lands, this block and `threads.last_context_id` are ITS to delete, not
        // its to keep.**
        //
        // Until then, the narrow version: if the id has not been stated in the
        // CURRENT context, state it on the next post. `threads.last_context_id`
        // is the id last stated, so "not yet carried" is a comparison and not a
        // scan, and it cannot fire twice in one context because stating it
        // records it. The boundary triggers are kept alongside rather than
        // replaced by it, so a daemon restart mid-thread still restates exactly
        // as it did before.
        let stated_ctx_id = self.store.get_thread_context_id(&state.id).ok().flatten();
        let catch_up = ctx_id.is_some() && stated_ctx_id.as_deref() != ctx_id.as_deref();
        let policy = context::MetadataPolicy {
            inject: inj,
            thread_id: &state.id,
            state_thread_id: first_turn || restated,
            context_id: ctx_id.as_deref(),
            state_context_id: first_turn || restated || catch_up,
            last_date: self.store.get_thread_meta_date(&state.id).ok().flatten(),
            now,
            app: app_id.as_deref(),
        };
        let (body, stated_date) = context::build_prompt_body(&sections, &policy);
        if stated_date != policy.last_date {
            if let Err(e) = self
                .store
                .set_thread_meta_date(&state.id, stated_date.as_deref())
            {
                tracing::warn!(error = %e, "persisting the stated metadata date failed");
            }
        }
        // Record the id this delivery actually stated, so the catch-up above
        // fires exactly once per context. Only when it was stated: a delivery
        // that said nothing must not claim to have.
        if policy.state_context_id && ctx_id.is_some() && ctx_id != stated_ctx_id {
            if let Err(e) = self
                .store
                .set_thread_context_id(&state.id, ctx_id.as_deref())
            {
                tracing::warn!(error = %e, "persisting the stated context id failed");
            }
        }
        if preambles.is_empty() {
            (body, message_ids)
        } else {
            // CON-22, enforced WHERE THE CONTEXT IS ASSEMBLED rather than at
            // each renderer: every line of machinery the daemon injects — a
            // preamble, a restatement, a notice, and anything a future kind
            // adds — passes through the guard, so no new event kind can leak a
            // reset instant by being written somewhere nobody thought to
            // filter.
            let injected = context::guard_injection(&preambles.join("\n\n"), now);
            self.emit(state, None, ThreadEvent::ContextInjected {
                text: injected.clone(),
            });
            (format!("{injected}\n\n{body}"), message_ids)
        }
    }

    /// The category-(b) events this turn owes the thread: the resume notice
    /// for a pending interruption (DEC-56) and the queued-turn report
    /// (DEC-57). Each is PERSISTED as it is minted, so the ledger shows
    /// exactly what the thread was told, and the interrupted flag is cleared
    /// only once the notice has actually been built.
    fn pending_system_notices(
        &self,
        state: &Arc<ThreadState>,
        batch: &[QueueRow],
        now: DateTime<Utc>,
    ) -> Vec<ThreadEvent> {
        let mut out: Vec<ThreadEvent> = Vec::new();

        // ── DEC-56 as superseded: WHEN, and how long ago. NOT why. ──────
        // The event still CARRIES `trigger` and `detail` (the ledger needs
        // them); `context::render_model_notice` is what refuses to render
        // them. Minting them here is not a leak and removing them here would
        // not be a fix — see that function's header.
        if let Ok(Some(row)) = self.store.get_thread_interrupt(&state.id) {
            let interrupted_at = DateTime::parse_from_rfc3339(&row.interrupted_at)
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or(now);
            let elapsed_seconds = (now - interrupted_at).num_seconds().max(0);
            let ev = ThreadEvent::ThreadSystemResumed {
                trigger: row.trigger.clone(),
                detail: row.detail.clone(),
                interrupted_at: row.interrupted_at.clone(),
                resumed_at: now.to_rfc3339(),
                elapsed_seconds,
                // Computed by the SYSTEM and handed over already computed —
                // "it resuming after one second is gonna be different than it
                // resuming after four days", and a model cannot reliably
                // subtract two timestamps.
                elapsed: context::format_elapsed(elapsed_seconds),
                switched: row.switched,
                from_account: row.from_account.clone(),
                to_account: row.to_account.clone(),
            };
            if let Err(e) = self.store.clear_thread_interrupt(&state.id) {
                tracing::warn!(thread = %state.id, error = %e, "clearing the interrupted flag failed");
            }
            self.emit(state, None, ev.clone());
            out.push(ev);
        }

        // ── DEC-57: the gap between intent and delivery ─────────────────
        // The interrupted flag is NOT set here and is reported FALSE: nothing
        // was interrupted, this turn never started. Two situations that both
        // produce a time gap need two different reports.
        if let Some(oldest) = batch.first() {
            if let Ok(submitted) = DateTime::parse_from_rfc3339(&oldest.created_at) {
                let submitted = submitted.with_timezone(&Utc);
                let queued_for_seconds = (now - submitted).num_seconds();
                if queued_for_seconds >= QUEUED_NOTICE_MIN_SECS {
                    let ev = ThreadEvent::ThreadSystemQueued {
                        submitted_at: oldest.created_at.clone(),
                        queued_until: now.to_rfc3339(),
                        queued_for_seconds,
                        queued_for: context::format_elapsed(queued_for_seconds),
                        interrupted: false,
                        message_ids: batch.iter().map(|q| q.message_id).collect(),
                    };
                    self.emit(state, None, ev.clone());
                    out.push(ev);
                }
            }
        }
        out
    }

    /// The message-id range recorded on the most recent `Compacted` event.
    fn last_compaction_range(&self, thread_id: &str) -> Option<(u64, u64)> {
        let kinds = vec!["compacted".to_string()];
        let evs = self
            .store
            .events_by_range(thread_id, None, None, Some(&kinds), Some(1))
            .ok()?;
        match evs.last()?.event {
            ThreadEvent::Compacted {
                message_id_range, ..
            } => message_id_range,
            _ => None,
        }
    }

    // ── budgets: the admission gate + park/resume (§15, CON-4) ───────────

    /// **Can this thread's budget key refuse anything?** The other half of
    /// `ungated` (INTENT 72b) — see [`crate::budgets::BudgetConfig::enforces`].
    ///
    /// **The governing rule: the INHERITED DEFAULT is invisible to the gate,
    /// and every key the operator set behaves exactly as it did before.**
    ///
    /// That is what makes this change one to accounting rather than to
    /// behaviour, and it decides all three arms below.
    ///
    /// * A key with a config refuses when it carries a ceiling, or is disabled.
    /// * A key with NO config refuses — a thread must never run unmetered on a
    ///   key that does not exist (a typo, or a client that never called
    ///   `budgets.set`). The one exception is `aui:default`, which nobody typed:
    ///   it is what this crate puts on a thread that named none, and a thread
    ///   holding only that must run exactly as an unkeyed thread ran.
    /// * A config that cannot be READ refuses, on the same split. For an
    ///   operator-set key that is the pre-existing fail-closed rule, untouched.
    ///   For the inherited default it must not, because such a thread did no
    ///   config read at all before this change and simply ran; turning a read it
    ///   never used to do into a new way to STOP it is precisely the behaviour
    ///   change this must not cause. Either way the fault is reported loudly.
    fn budget_can_refuse(&self, budget_key: &Option<String>) -> bool {
        let Some(key) = budget_key.as_deref() else {
            return false;
        };
        let inherited_default = key == crate::budgets::DEFAULT_BUDGET_KEY;
        match self.budgets.get_config(key) {
            Ok(Some(cfg)) => cfg.enforces(),
            Ok(None) => !inherited_default,
            Err(e) => {
                tracing::warn!(
                    key = %key, error = %e,
                    inherited_default,
                    "could not read a budget config while deciding whether anything is enforced"
                );
                !inherited_default
            }
        }
    }

    /// **Can these thresholds refuse anything RIGHT NOW?** — the threshold half
    /// of the `ungated` test, and the sibling of [`Self::budget_can_refuse`].
    ///
    /// A configured ceiling and an ENFORCEABLE ceiling are different things.
    /// `admit`'s threshold loop skips any axis `observed_value` cannot read
    /// (CON-5: never gate on a gauge you cannot read), so a threshold on an
    /// axis nothing has ever reported is inert by construction — it cannot
    /// refuse, cannot park, and cannot move a thread. Reporting such a thread
    /// as GATED would change its behaviour on the one branch where the two
    /// differ (the ungated escape hatch under `ParkCause::Unavailable`) on the
    /// strength of a ceiling that will never fire.
    ///
    /// The pool is scanned rather than just the thread's current account,
    /// because a threshold that can refuse ANYWHERE is a live gate — the gate
    /// compares candidates, not one account.
    fn thresholds_can_refuse(&self, thresholds: &Thresholds) -> bool {
        let configured = thresholds.configured();
        if configured.is_empty() {
            return false;
        }
        let pool = self.account_pool();
        for (axis, _) in &configured {
            for account in &pool {
                match self.budgets.observed_value(account, *axis) {
                    Ok(Some(_)) => return true,
                    // A store FAULT is not an absence (audit F2). It is
                    // reported as gated so the fail-closed machinery in
                    // `admit` — which will see the same error — decides,
                    // rather than this helper quietly routing around it.
                    Err(_) => return true,
                    Ok(None) => {}
                }
            }
        }
        false
    }

    /// Run the admission gate. Returns `true` when the thread may take a turn
    /// now; `false` means it has been PARKED (the work stays exactly where it
    /// was and a `turn_parked` event carries the queryable reason).
    /// **WOULD THE GATE HAVE ANYTHING TO SAY ABOUT AN EARLY ASK? — asked
    /// WITHOUT the execution lock, and committing nothing.**
    ///
    /// **A QUESTION IS NOT A TURN, and this is the function that makes that
    /// true.** Until 2026-10-06 [`Self::fire_retry`] took the execution lock
    /// merely to ask whether a retry was ready. Taking it emits
    /// `Push::Status { executing: true }`; the no-progress guard then returned
    /// without running anything; the [`TurnLock`] went out of scope, logged
    /// *"released by Drop — a turn path exited without releasing"* and emitted a
    /// second `Push::Status { executing: false }`. Two frames per thread per tick
    /// on three threads whose floor was a day out — 27,705 warnings since
    /// 2026-08-28, a board that flickered `1→2→3 … 3→2→1→0` on a five-second
    /// loop, and a gauge that counted three phantom locks as running turns, since
    /// [`Self::in_flight_count`] reads the `turn_running` bit `lock_turn` sets.
    ///
    /// **WHY THE GUARD IN `fire_retry` COULD NOT SIMPLY BE HOISTED.** Of its
    /// conditions only the cheap one is readable without the lock — the origin,
    /// which is an argument. (There was a second: `retry_floor_elapsed`, a pure
    /// function of the row's per-thread `retry_at`. Both are gone, and the origin
    /// now carries what they decided.) The other — *did the account change* — is
    /// not a condition at all. It is a POST-CONDITION of [`Self::admit_or_park`],
    /// which is the thing that moves the thread. Hoisting the cheap clause alone
    /// would reject every question and delete the feature; hoisting
    /// `admit_or_park` itself
    /// would port a conversation out from under whatever execution holds the
    /// lock, which is the race the lock exists for. So the lock IS taken first
    /// for a reason, and the honest shape is this one: ask read-only, lock only
    /// when the answer is yes, and let the existing guard RE-CHECK under the lock
    /// before committing.
    ///
    /// **IT LIVES HERE, BESIDE THE GATE, AND NOT IN THE RETRY PATH.** DEC-56's
    /// rule is that the re-ask may decide WHETHER to ask again while
    /// `admit_or_park` decides WHERE a thread goes — so the retry path is barred
    /// by test (`retry_replacement::the_retry_reask_path_chooses_nothing`) from
    /// naming a placement at all. This function is the admission module answering
    /// a yes/no question about its own decision; `fire_retry` names nothing but
    /// this predicate, and chooses nothing.
    ///
    /// **IT REJECTS ONLY ON AN ADMIT THAT MOVES NOTHING, and that narrowness is
    /// the whole of its safety.** A PARK is information the operator can act on —
    /// a disabled key, an exhausted meter, a threshold — and `admit_or_park` is
    /// the only thing that RECORDS one, with its queryable reason. So a park
    /// proceeds to the lock and is announced honestly; only the case where the
    /// gate admits and placement leaves the thread exactly where it is (which is
    /// every ungated AUI thread on a settled account — all three of the churning
    /// ones) is answered here for free. That case is the churn, and it is the
    /// only case in which the old code did literally nothing but emit two frames.
    ///
    /// **IT FAILS OPEN, ALWAYS.** Every uncertainty — unreadable history, a
    /// faulting budgets module, a park of any kind — answers `true`, which costs
    /// one lock and two honest frames and lets the locked path decide with its
    /// own fail-closed rules. So this can only skip a lock it is CERTAIN nothing
    /// would do anything with; it can never suppress an ask that had an answer.
    /// That direction is deliberate: a wrong `true` costs two frames, while a
    /// wrong `false` is a thread that silently stops asking.
    fn early_ask_would_move(&self, state: &Arc<ThreadState>) -> bool {
        let (budget_key, thresholds, pinned) = {
            let s = state.settings.lock().unwrap();
            let pinned = matches!(s.retry_policy.failover, Failover::PinnedAccount { .. })
                || matches!(s.account, AccountChoice::Named(_));
            (s.budget_key.clone(), s.thresholds.clone(), pinned)
        };
        // The same inputs `admit_or_park` builds, read the same way — including
        // `placement_pool()`, so CON-27's edge holds here too: nothing this
        // function can see as a destination includes the machine default.
        let accounts = self.placement_pool();
        let current = state.account.lock().unwrap().clone();
        let Ok(history) = self.store.thread_accounts(&state.id) else {
            // `admit_or_park` has a considered answer for an unreadable history
            // (report-and-run when ungated, fail closed otherwise). Defer to it
            // rather than duplicating it.
            return true;
        };
        let (unavailable, unhealthy) = self.barred_accounts();
        let live_subagents = state.live.lock().unwrap().running().len();
        let demand = self
            .budgets
            .estimate_demand(&state.id)
            .with_live_subagents(live_subagents);
        let req = AdmissionRequest {
            budget_key: budget_key.as_deref(),
            thresholds: &thresholds,
            accounts: &accounts,
            current_account: &current,
            pinned,
            history_accounts: &history,
            unavailable: &unavailable,
            unhealthy: &unhealthy,
            demand,
        };
        match self.budgets.admit(&req) {
            // The ONE case worth skipping a lock for: admitted, and placement
            // leaves the thread where it is. `switch_to` is compared against the
            // current account rather than merely tested for `Some`, because the
            // guard this stands in for asks whether the account CHANGED.
            Ok(Admission::Admit { switch_to }) => switch_to.is_some_and(|a| a.name() != current),
            // A park must be recorded by the gate, with its reason. Proceed.
            Ok(Admission::Park { .. }) => true,
            // The module faulted. Let the locked path fail closed and say why.
            Err(_) => true,
        }
    }

    fn admit_or_park(&self, state: &Arc<ThreadState>, message_ids: &[u64]) -> bool {
        let (budget_key, thresholds, pinned, budgeting_off) = {
            let s = state.settings.lock().unwrap();
            let pinned = matches!(s.retry_policy.failover, Failover::PinnedAccount { .. })
                || matches!(s.account, AccountChoice::Named(_));
            (
                s.budget_key.clone(),
                s.thresholds.clone(),
                pinned,
                s.budgeting_off,
            )
        };
        // ROUND-7 item 1, the remedy path: budgeting explicitly OFF on this
        // thread bypasses the module entirely — no gate, no meter, no trip.
        // This is what lets the operator keep working while a broken module is
        // being fixed, and it is the ONLY thing that runs when the module is
        // broken, because everything else fails closed below.
        if budgeting_off {
            self.clear_park(state, message_ids);
            return true;
        }
        // No key ⇒ no budget; no thresholds ⇒ no gate. Nothing to ENFORCE —
        // which is not the same as nothing to DECIDE. This used to
        // `return true` right here, and that early return is why placement was
        // invisible: it fires for every AUI thread, on every turn, so the one
        // code path that can move a thread onto a better account was never
        // reached by the threads that make up the entire deployment.
        //
        // So an ungated thread now runs the gate too, purely for its PLACEMENT
        // — and the gate is not allowed to stop it. `ungated` is threaded
        // through every refusal below, and each one turns into "run anyway":
        // placement is not gating, and a thread with nothing to enforce must
        // always run. What it can do is come back with `switch_to`, which is
        // the whole point of removing the short-circuit.
        // NOTHING TO ENFORCE — which is not the same as NO KEY (INTENT 72b).
        //
        // This used to read `budget_key.is_none() && thresholds.is_empty()`.
        // That was correct only for as long as a key implied a ceiling. Now
        // every thread inherits `aui:default`, so "has a key" stopped being
        // evidence of anything being gated, and reading it that way would have
        // flipped all 129 live threads from the ungated branch to the gated one
        // in a change that is supposed to alter accounting and nothing else.
        //
        // The question the branch actually wants to ask is whether ANY refusal
        // is possible: no thresholds, and no budget carrying a ceiling. A live
        // key with every axis unset meters and attributes, and can never stop a
        // turn — so a thread holding one runs exactly the path it ran before.
        //
        // ── AND A THRESHOLD NOBODY CAN READ REFUSES NOTHING ──
        //
        // `thresholds.is_empty()` answers "did the operator configure a
        // ceiling", which stopped being the same question the moment
        // `Thresholds::weekly` acquired a fleet-wide default
        // (`DEFAULT_WEEKLY_THRESHOLD_PCT`). CON-5 already says an unobserved
        // gauge never gates; this says the same thing one level up, so a
        // configured-but-unreadable ceiling does not silently flip a thread out
        // of the ungated branch.
        //
        // It is not a nicety. The ungated branch is what makes a WRONG
        // `limit_until` falsifiable: an ungated thread runs anyway when every
        // login is barred, the turn completes, and `note_account_served` retires
        // the window that was never true (the whole of the 2026-08-02 fix).
        // Reading the new default as "gated" would park every AUI thread behind
        // a bad window with nothing left in the daemon able to disprove it —
        // trading a thirty-three-minute outage for an open-ended one.
        //
        // The engagement is therefore automatic and self-limiting: once the
        // `/usage` poller has a weekly reading the gate is live and Fix 4 bites;
        // if the gauge goes dark the fleet degrades to yesterday's behaviour
        // instead of to starvation.
        let ungated =
            !self.thresholds_can_refuse(&thresholds) && !self.budget_can_refuse(&budget_key);
        // A budget KILL latches until the operator moves the limit or the
        // killed window rolls over (R9.L4). The meter alone cannot be trusted
        // to hold this thread: the kill fired on a mid-turn estimate, and
        // reconciling that estimate away can drop the meter back under the
        // limit for exactly as long as it takes to redispatch the same prompt
        // into the same kill. The latch is per KEY, so it holds every thread
        // charging the key that was killed, not only the killed one, and it is
        // read back from the ledger, so a restart does not forget it.
        //
        // An UNGATED thread is filtered out of this entirely, exactly as the
        // old short-circuit did by returning above it. A latch on a thread that
        // now carries no key at all is the residue of a budget it no longer
        // has, and unkeying a thread is itself the operator removing the limit
        // that killed it.
        let latch = match budget_key.as_deref().filter(|_| !ungated) {
            Some(k) => match self.kill_latch_for(k, &state.id) {
                Ok(l) => l.map(|l| (k.to_string(), l)),
                // The ledger read is part of the admission decision: a fault
                // fails CLOSED rather than releasing the latch.
                Err(e) => {
                    return self.fail_closed(state, "admission", &e.to_string(), budget_key.clone(), message_ids)
                }
            },
            None => None,
        };
        if let Some((latched_key, l)) = latch {
            let (latched_axis, latched_limit) = (l.axis, l.limit);
            let cfg = match self.budgets.get_config(&latched_key) {
                Ok(c) => c,
                // A budgets fault fails CLOSED like every other admission
                // error rather than releasing the latch.
                Err(e) => {
                    return self.fail_closed(
                        state,
                        "admission",
                        &e.to_string(),
                        Some(latched_key),
                        message_ids,
                    )
                }
            };
            // Released when the tripped axis no longer carries the limit that
            // killed the turn — i.e. the operator moved it (DEC-11, the
            // existing escape hatch) — or the key has no config at all (which
            // now parks on its own account, so the latch is moot).
            let limit_moved = match &cfg {
                Some(c) => c
                    .limit(latched_axis)
                    .map(|l| (l - latched_limit).abs() > f64::EPSILON)
                    .unwrap_or(true),
                None => true,
            };
            // …or when the killed window has ROLLED OVER (R9.L4 (b)): every
            // account the key meters is now in a window that opened after the
            // kill, so the killed spend is out of the meter and the meter can
            // be trusted with the next trip again.
            let rolled = if limit_moved {
                false
            } else {
                match self.budgets.rolled_over_since(
                    &latched_key,
                    latched_axis,
                    l.killed_at,
                    &self.placement_pool(),
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        return self.fail_closed(
                            state,
                            "admission",
                            &e.to_string(),
                            Some(latched_key),
                            message_ids,
                        )
                    }
                }
            };
            // Q-34: a DISABLED key denies for a louder and more actionable
            // reason than a stale kill latch, so when the key is disabled this
            // whole branch stands aside and lets `admit` park with cause
            // `disabled` — leaving the latch exactly as it is, so re-enabling
            // the key cannot silently release a kill either.
            if !cfg.is_some_and(|c| c.disabled) {
                if rolled {
                    // A rollover never un-happens, so the latch is dropped.
                    // A moved limit is NOT dropped: it is re-read every time,
                    // exactly as a restart re-reads it from the ledger, so
                    // putting the killing limit back re-engages it either way.
                    self.drop_kill_latch(&latched_key, l.killed_at);
                } else if !limit_moved {
                    let whose = if l.thread_id == state.id {
                        "this turn".to_string()
                    } else {
                        format!("a turn on thread {}", l.thread_id)
                    };
                    self.note_park(
                        state,
                        ParkCause::Budget.as_str(),
                        &format!(
                            "budget {latched_key} killed {whose} on its {} meter at the \
                             {latched_limit} limit; it stays parked until that limit is raised \
                             or the window rolls over",
                            latched_axis.as_str()
                        ),
                        Some(latched_key),
                        message_ids,
                    );
                    return false;
                }
            }
        }
        // CON-27: the gate is handed the PLACEABLE pool, not the visible one.
        // Every destination it can name is derived from this by filtering, so
        // the machine default cannot be admitted onto, failed over to, or
        // switched to — on this branch or any branch added later.
        let accounts = self.placement_pool();
        let current = state.account.lock().unwrap().clone();
        // Round-7 item 7: where this thread's conversation already lives.
        let history = match self.store.thread_accounts(&state.id) {
            Ok(h) => h,
            Err(e) => {
                // Reading history is part of the admission decision, so a
                // failure here fails closed like any other — unless there is no
                // decision to fail, in which case the thread runs where it is
                // and the fault is reported instead of enforced.
                if ungated {
                    self.report_budgets_failure(state, "history", &e.to_string(), None);
                    return self.run_ungated(state, "history", &e.to_string(), message_ids);
                }
                return self.fail_closed(state, "admission", &e.to_string(), budget_key, message_ids);
            }
        };
        // The observed facts no gauge measures: limit windows and dead logins,
        // plus every alias of one. This is how a 429 the turn just took becomes
        // an INPUT to placement instead of a private selector beside it.
        let (unavailable, unhealthy) = self.barred_accounts();
        // WHAT THIS THREAD IS ABOUT TO SPEND AND HOW MANY THREADS IT IS
        // (INTENT 83). Read fresh on every admission, off this thread's own
        // ledger history, so a thread that has just started fanning out is
        // placed as the thing it has become rather than as the thing it was.
        // Both of the operator's variables (idx `2928`): the moving average of
        // subagents, and the number running RIGHT NOW.
        let live_subagents = state.live.lock().unwrap().running().len();
        let demand = self
            .budgets
            .estimate_demand(&state.id)
            .with_live_subagents(live_subagents);
        let req = AdmissionRequest {
            budget_key: budget_key.as_deref(),
            thresholds: &thresholds,
            accounts: &accounts,
            current_account: &current,
            pinned,
            history_accounts: &history,
            unavailable: &unavailable,
            unhealthy: &unhealthy,
            demand,
        };
        match self.budgets.admit(&req) {
            Ok(Admission::Admit { switch_to }) => {
                if let Some(next) = switch_to {
                    self.switch_account(
                        state,
                        &next,
                        "moving to an admissible in-scope account (DEC-10)",
                    );
                }
                self.clear_park(state, message_ids);
                true
            }
            Ok(Admission::Park {
                cause,
                reason,
                budget_key,
            }) => {
                // A wiped slot is the one refusal a timed wait cannot clear, so
                // the reason names the sign-in that does.
                let reason = format!("{reason}{}", self.sign_in_note());
                // THE FAILOVER (msg #1028): a trip is exactly the moment
                // stickiness yields. If some other account admits this thread,
                // the conversation ports there and the turn runs, instead of
                // parking until a reset.
                //
                // Tried BEFORE the ungated escape hatch below, and that order is
                // load-bearing now that the 429 path comes through here: an AUI
                // thread carries no key and no thresholds, so "run where you
                // are" would send it straight back into the rate limit it just
                // took. Moving is strictly better than running in place
                // whenever a sibling admits, and it is never a refusal — a
                // failover that finds nothing falls through and the thread runs
                // exactly as it did before.
                if let Some(next) = self.failover_target(&req, cause) {
                    let why = format!(
                        "{} on {current}: {reason} — failing over to {next} \
                         and carrying the conversation (msg #1028)",
                        cause.as_str()
                    );
                    if self.move_thread(state, &next, &why, true) {
                        tracing::info!(thread = %state.id, from = %current, to = %next, "budget/threshold failover");
                        self.clear_park(state, message_ids);
                        return true;
                    }
                    // The conversation could not be carried. Parking is the
                    // safe half of the trade: the work waits, and it waits
                    // WHOLE. Moving anyway would fork it across two accounts.
                    tracing::warn!(
                        thread = %state.id, from = %current, to = %next,
                        "failover target admits this thread but the conversation could not \
                         be carried — parking instead of forking it"
                    );
                }
                // The invariant that makes removing the short-circuit safe: an
                // ungated thread is never GATED. It came through here to be
                // PLACED, and a placement pass may not invent a refusal for a
                // thread that carries nothing to refuse.
                //
                // Three causes can genuinely reach here ungated, and all used
                // to be impossible because the early return ran first: SCOPE,
                // when the registry is empty; CONTINUITY, when the thread sits
                // on an account that is no longer registered; and UNAVAILABLE,
                // when every login is inside a limit window and there is
                // nowhere at all to fail over to. Today's behaviour in all
                // three is "run" — the retry ladder, not this gate, is what
                // makes an ungated thread wait for a reset.
                // ── THE SHARE CLAMP IS NOT DOWNGRADED ──
                //
                // The escape hatch below exists because "an ungated thread is
                // never GATED": it carries no key and no threshold, so a
                // placement pass may not invent a refusal for it. The share is
                // the one refusal that is NOT invented here — the operator set
                // it, per account, and he set it precisely so that the account
                // stops taking work:
                //
                //   "if all the other accounts are blocked, we don't want that
                //    account to even be available for placement, right? ... it
                //    needs to be like hard clamped."
                //
                // Letting `run_ungated` override it would make the clamp
                // identical to the negative pressure it was ruled insufficient
                // to replace — every AUI thread is ungated, so the clamp would
                // never once bite in the real deployment. `failover_target` has
                // already run above, so this is reached only when there is no
                // unclamped account to move to, which is the case he named.
                // ── …BUT "RUN ANYWAY" STILL NEEDS SOMETHING TO RUN AS ──
                //
                // The SAME predicate as the spawn guard (`spawn_login`), asked
                // one layer earlier, and it is here for two distinct reasons.
                //
                // **Correctness.** The escape hatch is the door the hazard
                // actually walks through. Every AUI thread is ungated, so a
                // thread stranded on an account that has left the pool reaches
                // `continuity`/`scope` here and is admitted with the words "an
                // ungated thread is never GATED" — and then spawns with
                // `CLAUDE_CONFIG_DIR` unset, i.e. as the operator. Note the
                // ORDER: `failover_target` has already run above, so a thread
                // that could have been carried to a healthy account has been.
                // This is reached only when there was nowhere to port it to,
                // and at that point "run where you are" is not available
                // either, because where it is cannot be run as.
                //
                // **Liveness.** The spawn-site guard alone parks the turn and
                // `resume_sweep` then re-asks this gate every tick; an ungated
                // admit would re-dispatch into the same refusal forever — a hot
                // loop emitting `turn_parked`/`turn_resumed` pairs. Parking
                // HERE latches (`note_park` fires once) and the thread stays
                // quietly parked until the account is back in the pool, which
                // is the same steady state every other park has.
                //
                // This does NOT make the spawn-site guard redundant. That one
                // is the last line of defence and reads the value that actually
                // reaches `env_remove`, after this gate has already passed: the
                // window this cannot cover is a pool that changes BETWEEN
                // admission and `cmd.spawn()`, which is exactly what a
                // reloadable pool introduces. One predicate, one function, two
                // call sites — deliberately.
                // ── …AND NEVER ONTO A SLOT WHOSE LOGIN IS WIPED ──
                //
                // H179, observed live on `alice-example-com`: its timed login
                // bar ended, the resume sweep re-asked this gate, the threads
                // whose conversation lived there could not be carried anywhere
                // — and this hatch then RAN them where they were, on a slot
                // whose store is wiped. Each turn failed on the credential, the
                // bar was written again, and ten minutes later the same threads
                // did it again. Like the share clamp above, this is not a
                // refusal this pass invents: a wiped slot cannot authenticate
                // whatever we do. Parking latches, and `slot_signed_in` rings the
                // bell that re-asks.
                //
                // (Formerly: "a DISABLE alone is deliberately NOT refused here:
                // work already on a disabled account finishes there". Superseded
                // by H179 below.)
                //
                // H179 SUPERSEDES the disable carve-out above: the operator ruled
                // that an unavailable account — disabled included — is never
                // tried, on any path. A turn ALREADY EXECUTING there is not torn
                // down (nothing here reaches it); the NEXT turn is not started.
                if ungated {
                    if let Some(why) = self.account_unavailable_reasons(&current) {
                        let why = format!(
                            "{reason} — not running it on {current}: that account is not \
                             available ({})",
                            why.join(", ")
                        );
                        self.note_park(state, cause.as_str(), &why, budget_key, message_ids);
                        return false;
                    }
                }
                if ungated && cause != ParkCause::Share {
                    let cd = self.current_config_dir(state, &current);
                    if let Err(why) = self.spawn_login(&current, cd.as_deref()) {
                        tracing::error!(
                            thread = %state.id,
                            account = %current,
                            gate_cause = %cause.as_str(),
                            reason = %why,
                            "REFUSING TO RUN THIS UNGATED THREAD: the gate would have run it \
                             where it is, but where it is cannot be spawned as without \
                             falling back to the operator's own terminal login"
                        );
                        self.note_park(state, "login", &why, budget_key, message_ids);
                        return false;
                    }
                    return self.run_ungated(state, cause.as_str(), &reason, message_ids);
                }
                self.note_park(state, cause.as_str(), &reason, budget_key, message_ids);
                false
            }
            Err(e) => {
                // Fail CLOSED protects the operator from UNMETERED spend. An
                // ungated thread is unmetered by construction — there is no
                // budget it could overrun and no threshold it could cross — so
                // failing it closed protects nothing and would freeze every AUI
                // thread on any budgets fault, which is precisely what the old
                // short-circuit made impossible. So it is LOUD — an error log
                // and a `budgets_failed` ledger event, once per turn — and it
                // runs.
                if ungated {
                    self.report_budgets_failure(state, "admission", &e.to_string(), None);
                    return self.run_ungated(state, "admission", &e.to_string(), message_ids);
                }
                self.fail_closed(state, "admission", &e.to_string(), budget_key, message_ids)
            }
        }
    }

    /// **THE FAILOVER RULE.** Where a thread the gate just parked can go
    /// instead of parking — `None` means park.
    ///
    /// The operator superseded "a thread with history never moves" (msg #1028,
    /// intent 39): *"if we're running a thread on one account and then it hits
    /// the budget for that account, we should continue running the thread on the
    /// other account… I just care about continuity and it staying within its
    /// threshold and budget."* Continuity is the CONVERSATION, and the
    /// conversation is a file that can be ported, so a park that a DIFFERENT
    /// ACCOUNT could clear is no longer a park.
    ///
    /// Exactly three causes qualify, because exactly three are properties of
    /// the ACCOUNT the thread sits on:
    ///
    /// * `threshold` — this account is over a per-account gauge ceiling.
    /// * `unavailable` — this account is inside an observed usage-limit window
    ///   (or is another label for a login that is). This is the 429 path's
    ///   cause, and routing it here is what replaced `try_switch_account`: the
    ///   retry no longer picks a sibling itself, it says "this one cannot serve
    ///   a turn" and the gate answers.
    /// * `continuity` — an admissible account exists; the thread just has no
    ///   session there. That is the cause this rule was written to retire.
    ///
    /// Every other cause is pool-wide or key-wide, and no account change can
    /// clear it:
    ///
    /// * `budget` — **a budget meters the POOL and follows the thread across
    ///   accounts by construction** (`check_trip` reads the whole account list,
    ///   and a cash budget is dollars, not a per-account gauge). Moving buys
    ///   nothing, and failover must never look like a way out of a budget.
    /// * `disabled` / `unknown_key` — facts about the KEY, not the account.
    /// * `scope` — no registered account is in the budget's scope at all.
    /// * `internal` — the module is broken and failing closed is the point.
    ///
    /// A PINNED thread never fails over: a pin is a pin (§10), and the pinned
    /// branch of the gate is checked before anything here can apply.
    ///
    /// The target is chosen by **asking the gate again with `history_accounts`
    /// emptied** — the one thing that changed is that the conversation can now
    /// follow. That is deliberately the entire implementation: every rule the
    /// gate enforces (key state, scope, thresholds, the pooled budget trip, dead
    /// and duplicate logins, placement ranking) is re-applied verbatim on the
    /// second ask, so failover cannot become a hole in a gate it does not know
    /// about. In particular the budget trip is re-checked AFTER the target is
    /// picked, so a tripped budget parks the retry exactly as it parked the
    /// first ask.
    ///
    /// Placement stickiness survives as the mild preference it was: this runs
    /// only on a park, so a thread that may run where it is is never moved.
    fn failover_target(&self, req: &AdmissionRequest<'_>, cause: ParkCause) -> Option<Placeable> {
        if req.pinned {
            return None;
        }
        if !matches!(
            cause,
            ParkCause::Threshold | ParkCause::Continuity | ParkCause::Unavailable
        ) {
            return None;
        }
        let mut retry = req.clone();
        retry.history_accounts = &[];
        match self.budgets.admit(&retry) {
            Ok(Admission::Admit { switch_to }) => {
                switch_to.filter(|a| a.name() != req.current_account)
            }
            // Still parked with the conversation free to move, or the module
            // errored: either way there is nowhere to go.
            _ => None,
        }
    }

    /// An ungated thread's refusal, downgraded to a note. Always returns `true`.
    ///
    /// A thread with no budget key and no thresholds runs the admission gate
    /// only so that it gets PLACED (see the top of [`Self::admit_or_park`]).
    /// When the gate answers with a park or an error instead, the placement is
    /// simply unavailable this turn — the thread stays exactly where it is and
    /// takes its turn, which is what it did before it ever entered this path.
    fn run_ungated(
        &self,
        state: &Arc<ThreadState>,
        phase: &str,
        detail: &str,
        message_ids: &[u64],
    ) -> bool {
        // ── "RUNNING WHERE IT IS" IS NOT ALWAYS BENIGN ──
        //
        // At `debug` this line was invisible on a daemon whose default level is
        // `info`, and two of the phases that reach it are the daemon reporting
        // that it has NO GOOD OPTION and is proceeding anyway:
        //
        //  * `unavailable` — every login is inside an observed limit window and
        //    there is nowhere to fail over to, so the thread is about to run
        //    straight back into the refusal it just took. That is exactly the
        //    2026-08-14 shape, and it was logged at a level nobody sees.
        //  * `admission` — the budgets module itself errored. An ungated thread
        //    is unmetered by construction so failing it closed protects
        //    nothing, but the module being broken is not routine.
        //
        // Every other phase (`scope`, `continuity`, …) is ordinary placement
        // bookkeeping and stays at `debug`.
        if phase == "unavailable" || phase == "admission" {
            tracing::warn!(
                thread = %state.id,
                %phase,
                %detail,
                account = %state.account.lock().unwrap(),
                "RUNNING ANYWAY: this thread is ungated and the gate had nowhere better to \
                 put it — a turn is about to start on an account the gate could not clear"
            );
        } else {
            tracing::debug!(
                thread = %state.id,
                %phase,
                %detail,
                "ungated thread: no placement this turn, running where it is"
            );
        }
        self.clear_park(state, message_ids);
        true
    }

    /// ROUND-7 item 1: **fail CLOSED**, loudly.
    ///
    /// The build this supersedes admitted the turn on an internal budgets
    /// error. The operator rejected that outright: on an internal failure
    /// NOTHING runs except threads whose budgeting is explicitly turned off.
    /// CON-4 is still honored — the work PARKS, it is never dropped, and it
    /// resumes by itself the moment the module works again (the resume sweep
    /// re-runs this gate) or the moment `budgeting_off` is set on the thread.
    ///
    /// Always returns `false` (do not admit).
    fn fail_closed(
        &self,
        state: &Arc<ThreadState>,
        phase: &str,
        error: &str,
        budget_key: Option<String>,
        message_ids: &[u64],
    ) -> bool {
        let reason = format!(
            "budgets module error ({phase}): {error} — failing CLOSED; \
             set `budgeting_off` on this thread to bypass the module while it is fixed"
        );
        if self.park_changed(state, &reason) {
            tracing::error!(thread = %state.id, %phase, %error, "BUDGETS MODULE FAILED — failing closed");
            self.emit(
                state,
                None,
                ThreadEvent::BudgetsFailed {
                    phase: phase.to_string(),
                    error: error.to_string(),
                    outcome: "parked".into(),
                    budget_key: budget_key.clone(),
                },
            );
            self.emit_park(state, "internal", &reason, budget_key, message_ids);
        }
        false
    }

    /// Latch a park reason. Returns true when it CHANGED — re-checking every
    /// resume tick must not spam the ledger, so every park-side event fires
    /// only on a change.
    fn park_changed(&self, state: &Arc<ThreadState>, reason: &str) -> bool {
        let mut p = state.park_reason.lock().unwrap();
        if p.as_deref() == Some(reason) {
            return false;
        }
        *p = Some(reason.to_string());
        true
    }

    fn emit_park(
        &self,
        state: &Arc<ThreadState>,
        cause: &str,
        reason: &str,
        budget_key: Option<String>,
        message_ids: &[u64],
    ) {
        tracing::info!(thread = %state.id, %cause, %reason, "turn parked");
        // Parked is neither working nor free — say so on the status stream, or
        // AUI hears silence and assumes idle.
        self.emit_status(&state.id);
        self.emit(
            state,
            None,
            ThreadEvent::TurnParked {
                cause: cause.to_string(),
                reason: reason.to_string(),
                budget_key,
                message_ids: message_ids.to_vec(),
            },
        );
    }

    /// Record a park once (see [`Self::park_changed`]).
    fn note_park(
        &self,
        state: &Arc<ThreadState>,
        cause: &str,
        reason: &str,
        budget_key: Option<String>,
        message_ids: &[u64],
    ) {
        if self.park_changed(state, reason) {
            self.emit_park(state, cause, reason, budget_key, message_ids);
        }
    }

    /// Clear a park and announce the resume (only if it was actually parked).
    fn clear_park(&self, state: &Arc<ThreadState>, message_ids: &[u64]) {
        let was = state.park_reason.lock().unwrap().take();
        if let Some(reason) = was {
            let budget_key = state.settings.lock().unwrap().budget_key.clone();
            self.emit(
                state,
                None,
                ThreadEvent::TurnResumed {
                    reason: format!("no longer blocked ({reason})"),
                    budget_key,
                    message_ids: message_ids.to_vec(),
                },
            );
            // F4, second site. The asymmetry was the defect: `emit_park`
            // (`:3397`) says "parked" on the status stream and this, its
            // mirror, said nothing at all — so a thread that stopped being
            // parked stayed parked on every screen until some other edge fired.
            self.emit_status(&state.id);
        }
    }

    // ── DEC-56: the interruption protocol ────────────────────────────────

    /// **THE INTERRUPT PATH, ENTIRE.** Set the interrupted flag with its
    /// timestamp, emit the category-(b) system event, return. Nothing else.
    ///
    /// The operator amended DEC-56 in msg #1412 precisely to keep this
    /// function this small: *"you don't directly attempt to put it in another
    /// account. You put it back to the balancer. That's what it means when an
    /// interrupt happens."* The balancer is `Budgets::place` / `Budgets::admit`
    /// reached through `ThreadManager::resolve_account` and
    /// `ThreadManager::admit_or_park`, with `parked_turns` making the wait
    /// durable and `resume_sweep` making the resumption automatic. All of it
    /// is already built and already tested.
    ///
    /// So: **this function contains no placement logic and must never grow
    /// any.** No account ranking, no candidate filtering, no health or limit
    /// checks, no "try the other account" branch — not even a one-line "pick
    /// any other selectable account", which is exactly the shape the rule
    /// exists to forbid. A second placement implementation does not fail
    /// loudly, it DRIFTS: it bypasses the admission gate, the selectable pool,
    /// the round-robin cursor and the placement deadbands, and it splits
    /// ownership of "where does this thread run" across two files.
    /// `tests/system_events.rs::the_interrupt_path_contains_no_placement_logic`
    /// reads this body and fails on sight of any of it.
    ///
    /// If placement must behave differently on an interrupt, that difference
    /// is an INPUT to `Budgets::place` / `Budgets::admit` and is implemented
    /// there.
    ///
    /// `switched` is DEC-55's flag — whether a switchover actually happened —
    /// and is passed IN by the caller, which is the only party that knows;
    /// asking here would be this function making a placement observation.
    #[allow(clippy::too_many_arguments)]
    fn interrupt_thread(
        &self,
        state: &Arc<ThreadState>,
        trigger: InterruptTrigger,
        detail: &str,
        switched: bool,
        from_account: Option<String>,
        to_account: Option<String>,
    ) {
        let at = crate::store::now_rfc3339();
        let row = crate::store::ThreadInterruptRow {
            thread_id: state.id.clone(),
            trigger: trigger.as_str().to_string(),
            detail: detail.to_string(),
            switched,
            from_account: from_account.clone(),
            to_account: to_account.clone(),
            interrupted_at: at.clone(),
        };
        if let Err(e) = self.store.set_thread_interrupt(&row) {
            // A lost flag costs the thread its resume notice, never its work —
            // the hand-back below is unaffected either way.
            tracing::error!(thread = %state.id, error = %e, "recording the interrupted flag failed");
        }
        tracing::info!(
            thread = %state.id, trigger = trigger.as_str(), %switched,
            "HARD BREAK — thread flagged interrupted and handed back to placement/admission"
        );
        self.emit(
            state,
            None,
            ThreadEvent::ThreadSystemInterrupted {
                trigger: trigger.as_str().to_string(),
                switched,
                detail: detail.to_string(),
                at,
                from_account,
                to_account,
            },
        );
    }

    /// Which gate fired, from the failure text — `session_limit`,
    /// `weekly_limit` or `fable_limit`. **A LEDGER CLASSIFICATION. It no longer
    /// reaches the model at all.**
    ///
    /// This comment used to justify itself with DEC-56's permission to name the
    /// gate ("maybe specify like you hit the 40% account threshold for the
    /// session limit"). **That permission was superseded on 2026-08-17** — see
    /// [`crate::event::InterruptTrigger`] for the operator's words and
    /// [`crate::context::render_model_notice`] for where it is now enforced.
    /// The classification stays because the OPERATOR needs it (it is what tells
    /// him which window actually refused, and it is written to
    /// `thread_system_interrupted`), not because a model is owed it.
    ///
    /// What it must NOT carry, then as now, is the vendor text itself: the
    /// verbatim limit prose is the thing that leaked ("the usage limit for the
    /// week was hit… resets Sunday"), so the trigger is CLASSIFIED out of it
    /// and the prose stays in the ledger.
    /// **THE QUOTA THAT ACTUALLY REFUSED, from the quota the stream named.**
    ///
    /// `pool` is `gauge::Axis`, lifted straight off the `limit_hit` event the
    /// parser already produced (`TurnSignals::pool`) — the same value that is
    /// written to `accounts.limit_axis` and observed into `usage_observations`.
    /// It is used FIRST because it is a fact, where the prose is a guess.
    ///
    /// **It was not used at all, and the guess was structurally wrong.** The
    /// `reason` this is called with on the 429 path is `RetryDecision`'s summary
    /// string, which for a rate limit is the literal `"rate limit (429)"` —
    /// it contains no axis word, ever, so the fallback chain could only ever
    /// reach its `else` arm. Measured live on 2026-08-06: 456 consecutive
    /// `thread_system_interrupted` events reported `session_limit` while the
    /// account's session axis read 0% and its WEEKLY axis was pegged at 100%.
    /// The operator, and everyone diagnosing for him, was reasoning about a
    /// session reset that was hours away when the real window was two days out.
    ///
    /// The prose fallback stays for the case `pool` is absent (a 429 whose
    /// stream line named no quota), and it now reads the RESET PROSE rather than
    /// a classification of it, which is where an axis word can genuinely appear.
    fn classify_limit_trigger(pool: Option<&str>, reason: &str) -> InterruptTrigger {
        if let Some(axis) = pool.and_then(Axis::parse) {
            return match axis {
                Axis::WeeklyFable => InterruptTrigger::FableLimit,
                Axis::WeeklyAll => InterruptTrigger::WeeklyLimit,
                Axis::Session => InterruptTrigger::SessionLimit,
                // The cash axis is not a quota window and never produces a 429.
                Axis::Cash => InterruptTrigger::HardBudget,
            };
        }
        let r = reason.to_lowercase();
        if r.contains("opus") || r.contains("fable") {
            InterruptTrigger::FableLimit
        } else if r.contains("week") {
            InterruptTrigger::WeeklyLimit
        } else {
            InterruptTrigger::SessionLimit
        }
    }

    /// The one short factual clause RECORDED about a limit break. SYNTHESIZED,
    /// never the raw prose (CON-22).
    ///
    /// It used to say "the one short factual clause the model is allowed to
    /// read". **It is not read by the model any more** — the 2026-08-17
    /// supersession of DEC-56 dropped `detail` from
    /// [`crate::context::render_model_notice`] entirely. It survives as the
    /// LEDGER's `detail`, and every producer of that field must come through
    /// here (that is why `park_killed_turn` no longer passes `Trip::reason`):
    /// a model-audience field holding only service-synthesized clauses is what
    /// makes a future renderer change safe to reason about.
    fn limit_detail(trigger: InterruptTrigger) -> &'static str {
        match trigger {
            InterruptTrigger::SessionLimit => {
                "the session usage limit was reached on the account this thread was running on"
            }
            InterruptTrigger::WeeklyLimit => {
                "the weekly usage limit was reached on the account this thread was running on"
            }
            InterruptTrigger::FableLimit => {
                "the weekly Fable-model usage limit was reached on the account this thread was \
                 running on"
            }
            InterruptTrigger::Threshold => "an account usage threshold was crossed",
            InterruptTrigger::HardBudget => "a hard budget limit was reached",
            // None of these three is a limit break, so none of them reaches
            // `interrupt_thread` — they are carried on `turn_interrupted`,
            // which has no `detail` field. The clauses are written out anyway
            // (rather than left as a panic or a placeholder) because this match
            // is exhaustive on purpose: a future caller that DOES route one of
            // them through here gets a true sentence, not a lie and not a
            // crash.
            InterruptTrigger::Operator => "the operator interrupted this thread",
            InterruptTrigger::DaemonRestart => {
                "the service restarted while this thread was mid-turn"
            }
            InterruptTrigger::Compaction => {
                "this thread was compacted, and it does not resume after a compaction"
            }
        }
    }

    /// Charge a thread's budget key, if it has one (DEC-4: no key ⇒ no
    /// budget), then re-check the meters. ALL usage inside the turn charges
    /// the parent thread's key, including Claude Code's own subagent spawns —
    /// they already land in `modelUsage` (DEC-18).
    /// `tokens` and `cost_usd` are SIGNED — a negative pair is the turn
    /// epilogue correcting a provisional over-charge (see [`Budgets::charge`]).
    fn charge_budget(&self, state: &Arc<ThreadState>, model: &str, tokens: i64, cost_usd: f64) {
        if tokens == 0 && cost_usd == 0.0 {
            return;
        }
        // Round-7 item 1: a bypassed thread is not metered either.
        let (key, budgeting_off) = {
            let s = state.settings.lock().unwrap();
            (s.budget_key.clone(), s.budgeting_off)
        };
        if budgeting_off {
            return;
        }
        let Some(key) = key else { return };
        let account = state.account.lock().unwrap().clone();
        if let Err(e) = self
            .budgets
            .charge(&key, &state.id, &account, model, tokens, cost_usd)
        {
            // A charge that cannot be written means the meter is now WRONG.
            // The execution is already in flight so there is nothing to gate
            // here, but the next admission will fail closed on the same fault
            // — and this says so out loud rather than under-charging silently.
            self.report_budgets_failure(state, "charge", &e.to_string(), Some(key.clone()));
            return;
        }
        self.check_trip_midturn(state, &key);
    }

    /// Announce a budgets-module failure ONCE per turn (a broken module must
    /// not flood the ledger). The daemon keeps running the in-flight execution
    /// — killing a turn because our own bookkeeping broke would be a worse
    /// outcome — but the next admission gate fails closed.
    fn report_budgets_failure(
        &self,
        state: &Arc<ThreadState>,
        phase: &str,
        error: &str,
        budget_key: Option<String>,
    ) {
        tracing::error!(thread = %state.id, %phase, %error, "BUDGETS MODULE FAILED mid-turn");
        if state
            .budgets_failure_reported
            .swap(true, Ordering::SeqCst)
        {
            return;
        }
        self.emit(
            state,
            None,
            ThreadEvent::BudgetsFailed {
                phase: phase.to_string(),
                error: error.to_string(),
                outcome: "logged".into(),
                budget_key,
            },
        );
    }

    // ── R9.L4: the durable, per-key budget-kill latch ─────────────────────

    /// The seeded latch map. The first call in a daemon life reads it back
    /// from the ledger ([`Store::budget_kill_latches`]); every later kill is
    /// written into it by [`Self::set_kill_latch`].
    fn kill_latches(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<HashMap<String, KillLatch>>>> {
        let mut g = self.kill_latches.lock().unwrap();
        if g.is_none() {
            let mut map: HashMap<String, KillLatch> = HashMap::new();
            for row in self.store.budget_kill_latches()? {
                let Some(axis) = Axis::parse(&row.axis) else {
                    continue;
                };
                let Some(killed_at) = DateTime::parse_from_rfc3339(&row.killed_at)
                    .ok()
                    .map(|t| t.with_timezone(&Utc))
                else {
                    continue;
                };
                let newer = map.get(&row.budget_key).is_none_or(|l| killed_at >= l.killed_at);
                if newer {
                    map.insert(
                        row.budget_key.clone(),
                        KillLatch {
                            axis,
                            limit: row.limit,
                            killed_at,
                            thread_id: row.thread_id.clone(),
                            own_released: row.cancelled_after,
                        },
                    );
                }
            }
            *g = Some(map);
        }
        Ok(g)
    }

    /// Latch a kill on `key` (the newest kill on a key replaces any older one).
    fn set_kill_latch(&self, key: &str, axis: Axis, limit: f64, thread_id: &str) {
        let latch = KillLatch {
            axis,
            limit,
            killed_at: crate::clock::now(),
            thread_id: thread_id.to_string(),
            own_released: false,
        };
        match self.kill_latches() {
            Ok(mut g) => {
                g.get_or_insert_with(HashMap::new).insert(key.to_string(), latch);
            }
            // The seed read failed: latch this kill anyway (in memory) and
            // leave the map unseeded, so the next read retries the ledger —
            // which already holds this kill's `budget_tripped` event.
            Err(e) => {
                tracing::error!(budget_key = %key, error = %e, "READING THE BUDGET-KILL LATCHES FAILED");
            }
        }
    }

    /// The latch that holds `thread_id`, charging `key`, if any. A latch the
    /// operator released for its own thread by cancelling (R9.L3) does not hold
    /// that thread, and still holds every sibling.
    fn kill_latch_for(&self, key: &str, thread_id: &str) -> Result<Option<KillLatch>> {
        let g = self.kill_latches()?;
        Ok(g.as_ref()
            .and_then(|m| m.get(key))
            .filter(|l| !(l.own_released && l.thread_id == thread_id))
            .cloned())
    }

    /// Drop `key`'s latch — only the one killed at `killed_at`, so a newer
    /// kill that landed in between is never released by an older rollover.
    fn drop_kill_latch(&self, key: &str, killed_at: DateTime<Utc>) {
        if let Ok(mut g) = self.kill_latches() {
            if let Some(m) = g.as_mut() {
                if m.get(key).is_some_and(|l| l.killed_at == killed_at) {
                    m.remove(key);
                }
            }
        }
    }

    /// The cancel's half (R9.L3, narrowed by R9.L4): release the latch for the
    /// killed thread's OWN park, never a sibling's gate.
    fn release_own_kill_latch(&self, thread_id: &str) {
        if let Ok(mut g) = self.kill_latches() {
            if let Some(m) = g.as_mut() {
                for l in m.values_mut().filter(|l| l.thread_id == thread_id) {
                    l.own_released = true;
                }
            }
        }
    }

    /// Mid-turn enforcement. Announces the trip once per turn, and orders a
    /// teardown ONLY when the key's own config says `kill` — round-7 item 3
    /// makes `finish` the default, so the ordinary trip lets the in-flight
    /// execution complete and gates the NEXT turn instead.
    fn check_trip_midturn(&self, state: &Arc<ThreadState>, key: &str) {
        if state.kill_trip.lock().unwrap().is_some() {
            return; // already announced for this turn
        }
        let accounts = self.account_pool();
        let trip = match self.budgets.check_trip(key, &accounts) {
            Ok(Some(t)) => t,
            Ok(None) => return,
            Err(e) => {
                self.report_budgets_failure(
                    state,
                    "trip_check",
                    &e.to_string(),
                    Some(key.to_string()),
                );
                return;
            }
        };
        self.emit(
            state,
            None,
            ThreadEvent::BudgetTripped {
                budget_key: trip.budget_key.clone(),
                axis: trip.axis.as_str().to_string(),
                consumed_pct: trip.consumed,
                limit_pct: trip.limit,
                action: trip.action.as_str().to_string(),
            },
        );
        let kill = trip.action == TripAction::Kill;
        if kill {
            // Latch BEFORE the teardown. The turn epilogue is about to
            // reconcile this trip's estimate against the authoritative cost,
            // and if the estimate over-shot, the meter will fall back under
            // the limit — at which point the parked prompt would look
            // admissible again and be redispatched into the same kill. See
            // `ThreadManager::kill_latches`. The `budget_tripped` event just
            // emitted is the durable copy a restart reads it back from.
            self.set_kill_latch(&trip.budget_key, trip.axis, trip.limit, &state.id);
        }
        *state.kill_trip.lock().unwrap() = Some(trip);
        if kill {
            // The tail loop picks this up on its next poll and tears the
            // execution down; the prompt is parked, never dropped (CON-4).
            state.budget_kill.store(true, Ordering::SeqCst);
        }
    }

    /// Feed one gauge observation from a stream event (DEC-15 layers 1 & 3).
    ///
    /// **AND SAY SO ON THE WIRE.** This is the highest-frequency information
    /// update the daemon makes about the board — `rate_limit_event` arrives on
    /// EVERY turn, so the usage percentages a client is displaying genuinely
    /// move minute by minute — and until this broadcast existed it was also the
    /// quietest. That combination is exactly what the operator was looking at:
    /// numbers that were changing constantly in the daemon and never once on
    /// his screen between refreshes.
    ///
    /// It announces WITHOUT ringing `account_picture_changed`, deliberately.
    /// That function wakes the retry sweep, and a fresh reading is not on its
    /// own a reason to re-ask placement for every armed retry — the events that
    /// ARE (a limit recorded, a login dying or reviving) call it themselves and
    /// are unaffected. Broadcasting a display fact must not change placement
    /// behaviour as a side effect.
    fn observe_gauge(
        &self,
        state: &Arc<ThreadState>,
        axis: Axis,
        pct: f64,
        resets_at: Option<String>,
        source: &str,
    ) {
        let account = state.account.lock().unwrap().clone();
        if let Err(e) = self
            .budgets
            .observe(&account, axis, pct, resets_at, source)
        {
            tracing::warn!(error = %e, "recording gauge observation failed");
            return;
        }
        // P1. After the record, never before: a shape that folded an
        // observation the store refused would hold a number the daemon does
        // not believe.
        self.emit_gauge_fact(&account, axis);
        // Only after it is RECORDED. Announcing a write that failed would send
        // every client to re-read a number that did not change.
        self.emit_info(
            "gauge",
            source,
            Some(&account),
            Some(format!(
                "{account}: {} is now {pct:.0}%",
                axis.as_str()
            )),
        );
    }

    /// Remember that this thread now holds a conversation on its current
    /// account (round-7 item 7). Best-effort: a failure here can only cost the
    /// gate a continuity opportunity, never correctness.
    fn note_thread_account(&self, state: &Arc<ThreadState>) {
        let account = state.account.lock().unwrap().clone();
        if let Err(e) = self.store.record_thread_account(&state.id, &account) {
            tracing::warn!(thread = %state.id, error = %e, "recording thread account history failed");
        }
    }

    /// Persist `threads.account` so the durable row mirrors the account this
    /// thread is actually being served on (IS-A).
    ///
    /// Before this, the column was written once at CREATE time and never
    /// again, so it was wrong for every thread that had failed over and every
    /// durable consumer of "which account is this thread on" inherited that.
    /// With this write the column is the authoritative durable answer, which is
    /// what lets [`crate::accounts::attribute_account`] rank it above the
    /// `thread_accounts` history it previously had to fall back on.
    ///
    /// Goes through the existing `update_thread_settings` "persist the live
    /// row" write rather than adding a store method: the other columns are
    /// written back from the same in-memory settings they were read from, so
    /// they are unchanged by construction.
    ///
    /// Best-effort by design — a failure here can only cost a durable record,
    /// and must never abort a switch that `ThreadState` already reflects.
    fn persist_thread_account(&self, state: &Arc<ThreadState>, account: &str) {
        let snapshot = state.settings.lock().unwrap().clone();
        let json = match serde_json::to_string(&snapshot) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(thread = %state.id, error = %e, "serializing settings to persist the account column failed");
                return;
            }
        };
        if let Err(e) = self.store.update_thread_settings(
            &state.id,
            &json,
            Some(account),
            &snapshot.cwd,
            &snapshot.model,
        ) {
            tracing::warn!(thread = %state.id, %account, error = %e, "persisting the thread account column failed");
            return;
        }
        // ── SILENT WRITER 3: THE PLACEMENT COLUMN MOVED WITH NO EMIT AT ALL ──
        //
        // This is the ONE funnel for the `threads.account` write and it announced
        // nothing. Both callers were silent in different ways: `move_thread` emits
        // a `Status` (so the placement reached a fold as an attribute of the
        // edge), while `get_state`'s drift correction — a rehydrated thread whose
        // settings re-resolve to another login — writes the column at BOOT, with
        // no edge anywhere near it. The fact goes at the write, so both callers
        // are covered by construction and a third one cannot forget.
        self.emit_thread_facts(&state.id);
    }

    /// The `CLAUDE_CONFIG_DIR` of a REGISTERED account, refusing an unregistered
    /// one rather than answering `None`.
    ///
    /// [`Self::config_dir_for`] cannot make this distinction — an unregistered
    /// name and the machine default both come back `None` — and for the
    /// transcript store that difference is the whole thing: `None` means
    /// `$HOME/.claude`, i.e. the operator's own login. Porting a conversation
    /// into it by accident is exactly the mistake this refuses.
    ///
    /// **THE LIVE POOL FIRST, THE BOOT REGISTRY SECOND (H179).** `self.registry`
    /// is the roster as it stood at BOOT and is never re-read; the pool is
    /// re-derived on every reload (`reload_pool`). Reading only the registry made
    /// every account that joined after boot "not registered" here, so a parked
    /// turn's failover onto it failed the port and the turn parked for
    /// CONTINUITY under its old account forever — exactly a parked turn bound to
    /// its last home. A pool entry always names its own login directory, so it
    /// never resolves to `$HOME/.claude`; the registry is kept as the fallback
    /// for a SOURCE account that has since left the pool, whose transcript must
    /// still be found to be carried out of it.
    fn transcript_store_of(&self, account: &str) -> Result<Option<String>> {
        if let Some(dir) = self.config_dir_for(account) {
            return Ok(Some(dir));
        }
        match self.registry.get(account) {
            Some(a) => Ok(config_dir_of(a).map(|p| p.to_string_lossy().to_string())),
            None => Err(Error::Config(format!(
                "account {account} is not registered: its transcript store is unknown"
            ))),
        }
    }

    /// Carry this thread's CONVERSATION from one account's store to another's.
    ///
    /// The transcript is the conversation (see the `accounts` transcript-porting
    /// banner); porting it is what makes an account move continuous rather than
    /// a restart. Idempotent, and it never deletes the source.
    /// Is this transcript tail worth nothing, and how many records is it?
    ///
    /// `Some(n)` only when EVERY record in the tail is something a failed
    /// attempt leaves behind — the echoed prompt, bookkeeping, an API-error
    /// record. `None` the moment anything worth keeping appears, and `None` for
    /// an unparseable line, because we do not delete what we cannot read.
    ///
    /// Split out from [`ThreadManager::rollback_failed_attempt`] so the rule that
    /// decides whether the operator's conversation gets truncated is testable
    /// on its own, without standing up a runtime.
    ///
    /// # THIS IS NOT A LEAK CONTROL, AND MUST NOT BE TURNED INTO ONE
    ///
    /// The `!errored` clause below means a tail is trimmable only while it
    /// holds NO real assistant record. So whenever a turn produced real output
    /// and THEN hit a 429, this returns `None` and the synthetic API-error
    /// record survives on the transcript — permanently. That is correct: the
    /// alternative is deleting the operator's actual conversation to hide an
    /// error string, and "we do not delete what we cannot read" generalizes to
    /// "we do not delete what is worth reading".
    ///
    /// The surviving record is covered instead by [`crate::scrub`], which runs
    /// before every spawn and replaces that record's text with `<scrubbed>`
    /// while leaving the record — and its `isApiErrorMessage` flag, which is
    /// what makes Claude Code's own normalizer drop it from the request — in
    /// place. Two mechanisms, two jobs: this one bounds context GROWTH, the
    /// scrub bounds what the record SAYS. Do not widen this function's trimming
    /// rule in the name of the leak; the leak is already handled and widening
    /// it would cost conversation.
    fn trimmable_tail(tail: &[u8]) -> Option<usize> {
        let mut records = 0usize;
        for line in tail.split(|b| *b == b'\n') {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let v: Value = serde_json::from_slice(line).ok()?;
            if v.get("subtype").and_then(Value::as_str) == Some("compact_boundary") {
                return None;
            }
            let errored = v
                .get("isApiErrorMessage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if v.get("type").and_then(Value::as_str) == Some("assistant") && !errored {
                return None;
            }
            records += 1;
        }
        (records > 0).then_some(records)
    }

    /// **Where this thread's conversation is, and how long it is right now.**
    ///
    /// Taken immediately before an attempt so that a FAILED attempt can be
    /// rolled back off the transcript ([`Self::rollback_failed_attempt`]). The
    /// path is absolute and keeps pointing at the same file even if the gate
    /// moves the thread to another account afterwards.
    ///
    /// `None` whenever the transcript cannot be located — no mark, no rollback,
    /// which is the safe direction.
    fn transcript_mark(&self, state: &Arc<ThreadState>) -> Option<(PathBuf, u64)> {
        let home = crate::accounts::transcript_home()?;
        let account = state.account.lock().unwrap().clone();
        let store = self.transcript_store_of(&account).ok()?;
        let path = crate::accounts::locate_transcript(store.as_deref(), &home, &state.id)?;
        let len = std::fs::metadata(&path).ok()?.len();
        Some((path, len))
    }

    /// **Roll a failed attempt's tail off the conversation.**
    ///
    /// Every attempt is a fresh `claude` invocation, and Claude Code writes the
    /// prompt it was handed into the session transcript EVERY time — then, on a
    /// refusal, a synthetic assistant record carrying the error. A retry ladder
    /// therefore grows the conversation without ever advancing it.
    ///
    /// On 2026-08-06 five threads sat on that ladder for thirteen hours. Each
    /// attempt appended its prompt (a 3.1 KB preamble, in the worst case) plus a
    /// synthetic 429, ~2,000 records per thread, 36–44% of the active context.
    /// That is what pushed three of them over the auto-compaction line the
    /// moment they were unblocked — the retries did not merely fail to make
    /// progress, they destroyed context while failing.
    ///
    /// TRIMS ONLY A PROVABLY WORTHLESS TAIL. Anything the attempt might have
    /// genuinely produced — a real assistant message, a compaction boundary —
    /// aborts the rollback and the tail stays. So the failure mode is "we kept
    /// some junk", never "we dropped the operator's conversation".
    fn rollback_failed_attempt(&self, state: &Arc<ThreadState>, mark: &Option<(PathBuf, u64)>) {
        let Some((path, mark_len)) = mark else { return };
        let Ok(bytes) = std::fs::read(path) else { return };
        let mark_len = *mark_len as usize;
        if bytes.len() <= mark_len {
            return;
        }
        let Some(records) = Self::trimmable_tail(&bytes[mark_len..]) else {
            return;
        };
        let trimmed = bytes.len() - mark_len;
        match std::fs::OpenOptions::new().write(true).open(path).and_then(|f| {
            f.set_len(mark_len as u64)?;
            f.sync_all()
        }) {
            Ok(()) => tracing::info!(
                thread = %state.id, records, bytes = trimmed, path = %path.display(),
                "rolled a failed attempt off the transcript: a retry must not grow the \
                 conversation it could not advance"
            ),
            Err(e) => tracing::warn!(
                thread = %state.id, error = %e, path = %path.display(),
                "could not roll the failed attempt off the transcript"
            ),
        }
    }

    fn port_conversation(
        &self,
        state: &Arc<ThreadState>,
        from: &str,
        to: &str,
    ) -> Result<PortOutcome> {
        let home = crate::accounts::transcript_home()
            .ok_or_else(|| Error::Config("HOME is not set: no transcript store to port".into()))?;
        let src = self.transcript_store_of(from)?;
        let dst = self.transcript_store_of(to)?;
        crate::accounts::port_transcript(src.as_deref(), dst.as_deref(), &home, &state.id)
    }

    /// **THE PLACEMENT CHECK FOR A PROJECTED THREAD** — `crate::ledger::turn::check_placement`
    /// over this thread's ledger and the native file under its CURRENT account.
    fn placement_check(
        &self,
        state: &Arc<ThreadState>,
        to_account: &str,
    ) -> std::result::Result<(), crate::ledger::turn::PlacementRefused> {
        let Some(home) = crate::accounts::transcript_home() else {
            return Ok(());
        };
        let cwd = state.settings.lock().unwrap().cwd.clone();
        let config_dir = state.config_dir.lock().unwrap().clone();
        crate::ledger::turn::check_placement(
            &state.id,
            &crate::ledger::turn::ledger_path(&self.data_dir, &state.id),
            &crate::ledger::turn::native_path(config_dir.as_deref(), &home, &cwd, &state.id),
            to_account,
        )
    }

    /// The placement check, callable by a test: exactly what `move_thread`
    /// asks before it moves a projected thread, with the same inputs.
    pub fn placement_check_for_test(
        &self,
        id: &str,
        to_account: &str,
    ) -> std::result::Result<(), crate::ledger::turn::PlacementRefused> {
        let state = self.get_state(id).map_err(|e| crate::ledger::turn::PlacementRefused::LedgerUnreadable {
            path: std::path::PathBuf::from(id),
            why: e.to_string(),
        })?;
        self.placement_check(&state, to_account)
    }

    /// Switch a thread's serving account, emitting the audit event.
    ///
    /// Best-effort port: a conversation that cannot be carried still moves the
    /// thread. Used where the gate said the thread MAY run where it is and is
    /// merely being placed better (`Admission::Admit { switch_to }`), so a
    /// failed port costs the conversation but never forks it across two
    /// accounts that both may serve turns. The path that moves a thread
    /// *because* it may not stay — failover — calls [`Self::move_thread`]
    /// directly with `port_must_succeed`.
    fn switch_account(&self, state: &Arc<ThreadState>, next: &str, reason: &str) {
        self.move_thread(state, next, reason, false);
    }

    /// Move a thread onto `next`, CARRYING ITS CONVERSATION.
    ///
    /// `port_must_succeed` is the failover contract: when the caller is moving a
    /// thread *because* it may not run where it is, a port that ERRORS must
    /// abort the move (returns `false`) rather than fork the conversation onto a
    /// second account. Parking is recoverable; a forked conversation is not.
    ///
    /// A source that simply is not there ([`PortOutcome::NoSource`]) is NOT an
    /// error and does not abort: no transcript means the conversation is already
    /// unrecoverable on the source account too (`--resume` would fail there),
    /// so there is nothing left to protect and running beats parking forever.
    ///
    /// Ordering matters at every step and each one is load-bearing:
    ///
    /// 1. PORT first. Nothing about the thread has changed yet, so a failure
    ///    here is a no-op.
    /// 2. Record the destination in `thread_accounts` BEFORE the account column
    ///    moves. A crash between the two leaves a true statement on disk (the
    ///    conversation IS on both) rather than a false one.
    /// 3. Flip the in-memory account + config dir, then persist the column
    ///    (IS-A), then set `resumable`, then emit — so anything reacting to
    ///    `AccountSwitched` by reading the store sees the new account.
    fn move_thread(
        &self,
        state: &Arc<ThreadState>,
        next: &str,
        reason: &str,
        port_must_succeed: bool,
    ) -> bool {
        let current = state.account.lock().unwrap().clone();
        if current == next {
            return true;
        }
        // ── A PROJECTED THREAD IS NOT PORTED; IT IS RE-RENDERED, AND ONLY IF
        // ITS FILE HOLDS NOTHING THE LEDGER LACKS (R4.S4a). The next turn on
        // `next` renders the ledger into that account's store, so there is no
        // copy to carry (`docs/thread-adapters.md` §5.5). What there IS, and
        // this is the ONE enforcement point `docs/proto-ledger.md` §4 names, is
        // the dirty check: a turn whose reconcile never ran, or a native file
        // whose digest moved since the ledger last looked, refuses the move —
        // moving would leave those lines behind in the store the thread left.
        let projected = state.settings.lock().unwrap().projection.is_projected();
        if projected {
            if let Err(e) = self.placement_check(state, next) {
                tracing::warn!(
                    thread = %state.id, from = %current, to = %next, refusal = %e,
                    "PLACEMENT REFUSED: the thread's native file holds what its ledger does not; \
                     it stays where it is until a turn reconciles it"
                );
                return false;
            }
        }
        let port = if projected {
            Ok(PortOutcome::NoSource)
        } else {
            self.port_conversation(state, &current, next)
        };
        let carried = match &port {
            Ok(o) => o.holds_conversation(),
            Err(e) => {
                tracing::warn!(
                    thread = %state.id,
                    from = %current, to = %next, error = %e,
                    "could not carry this thread's conversation to the target account"
                );
                if port_must_succeed {
                    return false;
                }
                false
            }
        };
        if carried {
            // The target now holds a session for this thread id, which is
            // exactly what `thread_accounts` records — and what the continuity
            // gate will read on the next turn.
            if let Err(e) = self.store.record_thread_account(&state.id, next) {
                tracing::warn!(thread = %state.id, error = %e, "recording the ported account failed");
            }
        }
        // The LIVE pool first (H179): an account that joined after boot is in
        // the pool and not in the boot-time registry, and leaving `config_dir`
        // as `None` for it would mean spawning without `CLAUDE_CONFIG_DIR`.
        let cd = self.config_dir_for(next).or_else(|| {
            self.registry
                .get(next)
                .and_then(config_dir_of)
                .map(|p| p.to_string_lossy().to_string())
        });
        *state.account.lock().unwrap() = next.to_string();
        *state.config_dir.lock().unwrap() = cd;
        self.persist_thread_account(state, next);
        // Does a session for this thread id exist on the target? A successful
        // port has just made one; otherwise the thread may still have run there
        // before. Either way this decides `--resume` vs `--session-id`.
        // A projected thread with a ledger resumes anywhere: the render makes
        // the session on the target before the spawn (`run_once` recomputes
        // this from the render itself; the value set here only covers the gap).
        let ledger_there = projected
            && crate::ledger::turn::ledger_path(&self.data_dir, &state.id).exists();
        if ledger_there {
            if let Err(e) = self.store.record_thread_account(&state.id, next) {
                tracing::warn!(thread = %state.id, error = %e, "recording the projected account failed");
            }
        }
        let resumable_there = carried
            || ledger_there
            || self
                .store
                .thread_accounts(&state.id)
                .map(|h| h.iter().any(|a| a == next))
                .unwrap_or(false);
        state.resumable.store(resumable_there, Ordering::SeqCst);
        // The port is recorded on the thread's own ledger, so "when did this
        // conversation move stores, and how big was it" is answerable after the
        // fact from `threads.history` alone.
        if let Ok(outcome) = &port {
            if outcome.holds_conversation() {
                let mut data = outcome.to_json();
                if let Some(obj) = data.as_object_mut() {
                    obj.insert("from".into(), Value::String(current.clone()));
                    obj.insert("to".into(), Value::String(next.to_string()));
                }
                self.emit(
                    state,
                    None,
                    ThreadEvent::Custom {
                        event_type: "transcript_ported".to_string(),
                        data,
                    },
                );
            }
        }
        let reason = match (&port, carried) {
            (Ok(o), true) => format!("{reason} (conversation {} to {next})", o.as_str()),
            _ => reason.to_string(),
        };
        self.emit(
            state,
            None,
            ThreadEvent::AccountSwitched {
                from: current,
                to: next.to_string(),
                reason,
            },
        );
        // **F4 — A PLACEMENT IS A `Status` EDGE.** The ledger's
        // `AccountSwitched` is the OCCURRENCE and was the only live signal that
        // a thread had moved; nothing said what the thread's state now IS under
        // its new account. A client therefore had to chase `threads.get` per
        // thread to learn a placement — which is precisely the follow-up the
        // account board skipped, which is why its per-account counts only moved
        // on refresh. `emit_park` (`:3397`) already did this for parking; these
        // three sites did not.
        //
        // Legacy-visible and ADDITIVE: one extra `status` frame per move on the
        // firehose. `live.py:201` already folds any `status`, and the harness
        // terminal face renders a status it already renders.
        self.emit_status(&state.id);
        true
    }

    /// §3: a client hands back a transcript that advanced OUTSIDE the daemon.
    ///
    /// The AUI in-process fallback runs turns on whichever login holds the
    /// conversation while the daemon is down, so by the time the daemon is back
    /// the authoritative transcript may be a copy the daemon has never seen. On
    /// recovery the client says "this thread's transcript advanced externally at
    /// `<path>`" and this merges it back into the thread's CURRENT account
    /// store, newest wins, before the next daemon turn resumes there.
    ///
    /// Newest-wins is the whole merge rule: transcripts are append-only, so the
    /// copy with more events is the copy that has the other's history plus more.
    /// Nothing is ever interleaved, and nothing is ever deleted.
    ///
    /// **AND IT IS WHERE `threads/{thread_id}/reconcile_transcript` RESOLVES**
    /// (rule 2.4, R2.S5): the `transcript_reconciled` ledger event below is the
    /// resolver, and its payload is `ThreadCells::app["transcript_reconciled"]`.
    /// The outcome is still returned HERE, to the crate's own callers; what
    /// changed is that the WS handler no longer puts it on the wire.
    pub fn reconcile_transcript(&self, thread_id: &str, path: &str) -> Result<(Value, Visible)> {
        let state = self.get_state(thread_id)?;
        let source = PathBuf::from(path);
        if !source.is_file() {
            return Err(Error::Config(format!(
                "no transcript at {path}: nothing to reconcile"
            )));
        }
        let account = state.account.lock().unwrap().clone();
        let home = crate::accounts::transcript_home()
            .ok_or_else(|| Error::Config("HOME is not set: no transcript store".into()))?;
        let dest_store = self.transcript_store_of(&account)?;
        let outcome =
            crate::accounts::port_transcript_file(&source, dest_store.as_deref(), &home, &state.id)?;
        if outcome.holds_conversation() {
            // The conversation is provably in this account's store now, so the
            // next turn RESUMES it instead of minting a new session over it.
            state.resumable.store(true, Ordering::SeqCst);
            if let Err(e) = self.store.record_thread_account(&state.id, &account) {
                tracing::warn!(thread = %state.id, error = %e, "recording the reconciled account failed");
            }
        }
        let mut data = outcome.to_json();
        if let Some(obj) = data.as_object_mut() {
            obj.insert("thread_id".into(), Value::String(state.id.clone()));
            obj.insert("account".into(), Value::String(account.clone()));
            obj.insert("source".into(), Value::String(path.to_string()));
            obj.insert(
                "reconciled".into(),
                Value::Bool(matches!(outcome, PortOutcome::Ported { .. })),
            );
        }
        let event = ThreadEvent::Custom {
            event_type: "transcript_reconciled".to_string(),
            data: data.clone(),
        };
        let visible = if !self.hub.any_tracking() {
            // No fold is running, so no frame will carry these ids: ABANDON
            // them, as `append_custom` does, and still write the ledger.
            let _ = self
                .promises
                .abandon_all::<crate::commands::TranscriptReconciledFact>(Some(&state.id));
            self.emit(&state, None, event);
            Visible::Never
        } else {
            // **THE RESOLVE IS AN ARGUMENT OF THE EMIT.**
            self.emit_resolving(
                &state,
                None,
                event,
                self.promises
                    .resolve::<crate::commands::TranscriptReconciledFact>(Some(&state.id)),
            );
            Visible::Expected
        };
        tracing::info!(
            thread = %state.id, %account, %path, outcome = outcome.as_str(),
            "external transcript reconciled"
        );
        Ok((data, visible))
    }

    // ── watchers: resume sweep + `/usage` poll ───────────────────────────

    /// Start the background watchers exactly once. Called from `serve()` and
    /// `recover()`, whichever runs first.
    pub fn start_watchers(&self) {
        if self.watchers_started.swap(true, Ordering::SeqCst) {
            return;
        }
        // The self-update watcher (intent item 64). Arms only for a binary
        // literally named `threads`, so it can never fire inside a test process.
        self.update.spawn_watcher();

        // ── THE SHAPES ───────────────────────────────────────────────────
        //
        // Registered here, not in `build_manager`, for two reasons: a shape
        // task needs a tokio runtime to be spawned into, and `start_watchers`
        // is already the idempotent once-per-process hook
        // (`watchers_started.swap`) that both `recover()` and
        // `serve_on_with_shutdown` go through.
        //
        // `Accounts` is `Lazy`, so registering it costs one task that is
        // immediately idle: it holds no state and its FIFO accepts nothing
        // until the first `shapes.subscribe`.
        self.hub
            .register(crate::shapes::accounts::Accounts, self.clone());
        // An account's gauge series — `PARENT = Some("accounts")`, so after the
        // pool it is a series of (R2.S3d).
        self.hub
            .register(crate::shapes::gauge_history::GaugeHistory, self.clone());
        // **PARENTS FIRST, AND `register` ENFORCES IT.** `threads/summary`
        // declares `PARENT = Some("threads")`, and a parent that is not
        // registered yet is a panic here — in the producer's build function, so a
        // mis-declared nesting tree is a daemon that does not boot rather than a
        // consumer that subscribes to something incoherent.
        //
        // All three are `Lazy`: registering one costs a task that is immediately
        // idle, holds no state, and whose FIFO accepts nothing until somebody
        // subscribes. `threads` in particular must stay that way — it is the only
        // shape on this producer that would fold `Push::Event`, which is 2.46M
        // rows of ledger.
        self.hub
            .register(crate::shapes::threads::Threads, self.clone());
        self.hub
            .register(crate::shapes::summary::Summary, self.clone());
        // The live event log — `PARENT = Some("threads")`, so after the pool for
        // the same parents-first reason. `Lazy`, and it matters more here than
        // anywhere: this is the ONLY fold on this producer that reads every
        // `Push::Event` including the three `BULK_KINDS` its parent discards, so an
        // unwatched daemon must pay none of it.
        self.hub
            .register(crate::shapes::log::Log, self.clone());
        // The index of recent updates — also `PARENT = Some("threads")`, so it
        // is registered after the pool for the same parents-first reason.
        self.hub
            .register(crate::shapes::recent::Recent, self.clone());
        // The index of recent VISITS — `recent`'s sibling, same parent.
        self.hub
            .register(crate::shapes::recent_visits::RecentVisits, self.clone());
        // The non-idle roster — the THIRD index under `threads`, so after the
        // pool for the same parents-first reason. It hydrates from the SAME
        // `ThreadRows` read `accounts` uses, so it must not be registered before
        // `accounts`: nothing enforces that ordering mechanically (the request is
        // the producer's, not a shape's), but reading one roster twice from two
        // places is the thing sharing the request exists to prevent, and keeping
        // the two registrations in one order keeps that visible.
        self.hub
            .register(crate::shapes::nonidle::NonIdle, self.clone());
        self.hub
            .register(crate::shapes::narration::Narration, self.clone());
        self.hub
            .register(crate::shapes::heartbeat::Heartbeat, self.clone());
        self.hub
            .register(crate::shapes::warnings::Warnings, self.clone());
        // **THE BOOT NOTE'S HOME.** No parent, keyed `daemon` like `warnings`,
        // and it needs nothing emitted to it ever — its whole content is read
        // from `self.update` by its own hydrate, because the fact was computed in
        // `on_boot` before any server was listening. That is also why its
        // registration order does not matter: it reads no other shape's roster
        // and nothing reads it.
        self.hub.register(crate::shapes::boot::Boot, self.clone());

        // ── THE BEAT, AND WHY IT IS EMITTED FROM OUT HERE ────────────────
        //
        // `crate::shapes::heartbeat` is worth having only if the beat travels the
        // FRAME path, because a beat that rides the socket is a websocket ping
        // with extra steps and the socket is already pinged
        // (`shapewire::server::PING_INTERVAL`, the connection loop's fourth
        // `select!` arm). So it is emitted HERE, by an ordinary producer emit
        // through `hub.emit`, exactly like every durable write on this daemon —
        // and not by the shape task's own loop, which would skip the FIFO and
        // prove one link less.
        //
        // What a delivered beat therefore witnesses, in order: this task is
        // scheduled; the FIFO accepted the item; `ShapeTask::run` came back round
        // its `select!`; `folds` returned; the drain and the flush ran; the
        // connection's frame channel was not full. A ping witnesses none of them.
        //
        // **`tracked` FIRST, EVERY TICK.** The shape is `Lazy`, so with no
        // subscriber its FIFO accepts nothing and this emit would be a no-op —
        // but it would still be an allocation and a lock per second forever on an
        // unwatched daemon, and this is the one input that arrives whether or not
        // anything happened. The counter does not advance while nobody is
        // listening either: a `seq` that had climbed for a week in silence would
        // be describing an era no subscriber shares, and the export's `epoch`
        // already says which era it is.
        let beat_mgr = self.clone();
        tokio::spawn(async move {
            let period = std::time::Duration::from_millis(
                crate::shapes::heartbeat::BEAT_INTERVAL_MS,
            );
            let mut seq: u64 = 0;
            loop {
                tokio::time::sleep(period).await;
                if !beat_mgr.hub.tracked("heartbeat") {
                    continue;
                }
                seq += 1;
                beat_mgr.hub.emit_fact(crate::event::Fact::Beat { seq });
            }
        });

        // ── THE WARNING SAMPLER, AND WHY IT IS A SECOND TASK ─────────────
        //
        // `crate::warnings::COUNTER` is incremented by a `tracing` layer, INSIDE
        // `warn!`. It cannot emit the fact itself: `hub.emit_fact` allocates,
        // takes locks and may itself log, and a producer emit on the warn path
        // would be a re-entrancy hazard on every warning in the daemon. So the
        // counter is passive and this task is the one that publishes it — the same
        // separation the beat uses, for a sharper reason.
        //
        // **IT IS NOT FOLDED INTO THE BEAT TASK ABOVE**, although one timer would
        // have done both. Two reasons, and the second is the one that matters: the
        // beat gates on `tracked("heartbeat")`, so a consumer subscribed to
        // `warnings` and not to `heartbeat` would have been served nothing, which
        // is a silent dependency between two shapes that are deliberately
        // independent; and a shared timer makes the two shapes' cadences one knob,
        // where `heartbeat`'s is a published MAX contract and this one's is not.
        //
        // **`tracked` FIRST, EVERY TICK**, as the beat does: the shape is `Lazy`,
        // so an unwatched daemon does no sampling at all. Unlike the beat, nothing
        // is lost by the silence — the counters live outside the root, keep
        // climbing while nobody listens, and the next hydrate reads the true
        // totals. `shared.counts_since` is what says the window is the DAEMON's
        // life and not this epoch's.
        //
        // **THE FOLD DEDUPES**, so a quiet second costs no frame even though this
        // emits every second: `DaemonWarningsRoot::apply` reads before it reaches
        // the key. A daemon that is not warning is silent on this shape.
        let warn_mgr = self.clone();
        tokio::spawn(async move {
            let period = std::time::Duration::from_millis(
                crate::shapes::warnings::SAMPLE_INTERVAL_MS,
            );
            loop {
                tokio::time::sleep(period).await;
                if !warn_mgr.hub.tracked(
                    <crate::shapes::warnings::Warnings as crate::shape::Shape<
                        crate::shape::Input,
                    >>::NAME,
                ) {
                    continue;
                }
                warn_mgr
                    .hub
                    .emit_fact(crate::event::Fact::WarningsSampled {
                        tally: crate::warnings::COUNTER.tally(),
                    });
            }
        });

        // ── P6: THE CREDENTIAL SAMPLER, ON ITS OWN CLOCK ─────────────────
        //
        // FIVE MINUTES, deliberately not `resume_tick` (5 s) and deliberately
        // not on the resume/retry watcher's task. See
        // `ThreadManager::sample_credentials` for the whole argument; in short,
        // four `security` invocations every five seconds is a ~10% duty cycle
        // on a subprocess, it would stall `resume_sweep` for half a second per
        // tick, and polling is not supposed to be the default. The login and
        // rotation paths emit directly; this catches only out-of-band writes by
        // other processes.
        //
        // `spawn_blocking`, because the sample is subprocess + filesystem work
        // and must not sit on a tokio worker.
        //
        // A-1 (R9.A1): the tick runs whether or not a board is open, because
        // the sample now also feeds PLACEMENT (the sign-in set) — what is
        // barred must not depend on whether someone is watching. Untracked, it
        // costs one keychain read per pool slot per period and emits nothing;
        // the board emission inside `sample_credentials` stays gated.
        let mgr = self.clone();
        tokio::spawn(async move {
            let every = mgr.tunables.credential_sample;
            loop {
                tokio::time::sleep(every).await;
                let m = mgr.clone();
                let _ = tokio::task::spawn_blocking(move || m.sample_credentials()).await;
            }
        });

        // ── PLACEMENT RE-RUNS ON EVENTS. ONE BELL, ONE TASK ──────────────
        //
        // **THIS USED TO BE TWO TASKS AND THEY WERE WIRED THE WRONG WAY ROUND.**
        // One slept `resume_tick` and called `resume_sweep` — the sweep for
        // PARKED threads, which is the state "there is a pending turn and it has
        // not been placed". The other woke on `account_picture` and called
        // `retry_sweep`. So all fourteen events woke the retry mechanism and NOT
        // ONE of them woke placement: the event-driven path served a per-thread
        // clock, and the genuinely placement-shaped state was polled every five
        // seconds and nothing else.
        //
        // Both sweeps now hang off the ONE bell, inside one task, so one
        // coalescing window covers both and a pool reload that enables an
        // account and clears a bar costs one pass rather than five. They stay two
        // SWEEPS because they serve two states — `parked_turns` versus `retries`,
        // `maybe_dispatch` versus `fire_retry`; see `placement_pass`.
        //
        // **The tick is DEMOTED FROM MECHANISM TO BACKSTOP, and instrumented.**
        // It is kept, because an event set can always turn out to be incomplete
        // and a thread silently parked forever is a worse failure than a cheap
        // poll — but a backstop that quietly carries the work is exactly how the
        // wiring above survived, so `placement_pass` reports at `error` when a
        // backstop pass places anything. `THREADS_RESUME_TICK_MS=0` removes the
        // fallback entirely, which is how a test proves the events do the work.
        // **AND THE POOL'S OWN BACKOFF IS THE THIRD SOURCE.** It is not the
        // backstop and it must never be confused with it. The backstop is a flat
        // 5s poll that exists only because an event set can be incomplete; the
        // cadence is the RETRY LADDER, re-homed off N per-thread `retry_at`
        // floors onto one pool-level timer, on the operator's ruling that *"if any
        // thread is parked then the pool needs a retry, not the thread."*
        //
        // Counting them apart is what keeps `backstop_placed` meaningful: if the
        // cadence were spelled as another `sleep(tick)` arm, every pool-driven
        // placement would be reported as a missing event and the one detector of
        // an incomplete event set would drown in its own noise.
        //
        // `cadence_rearm` is a `continue`, not a pass: a new parked prompt changes
        // how long the pool should sleep (`PoolCadence::rung` reads the live rows)
        // without itself being a reason to reconsider anything.
        let mgr = self.clone();
        let tick = self.tunables.resume_tick;
        tokio::spawn(async move {
            // **THE CADENCE IS A DEADLINE, NOT A FRESH SLEEP EACH TIME ROUND.**
            // Spelled as `sleep(rung)` inside the select, a 300-second rung would
            // never come due at all: the 5-second backstop wins the race, the loop
            // re-enters, and the rung starts over. So the pool's next cadence is
            // held as an INSTANT across iterations — one in-memory deadline for the
            // whole pool, which is precisely what the operator asked for in place of
            // one persisted deadline per thread.
            //
            // `None` means nothing is parked: no cadence, no timer, no wake-up.
            let mut due: Option<tokio::time::Instant> = mgr
                .cadence_rung()
                .map(|r| tokio::time::Instant::now() + r);
            loop {
                let due_at = due;
                let woken = tokio::select! {
                    _ = mgr.account_picture.notified() => PassOrigin::Event,
                    // NOT A PASS. The parked set changed, so the pool should sleep
                    // for a different length — which is news to the clock and not to
                    // placement, whose inputs have not moved.
                    _ = mgr.cadence_rearm.notified() => {
                        due = mgr.cadence_rung().map(|r| tokio::time::Instant::now() + r);
                        continue;
                    }
                    _ = async {
                        match due_at {
                            Some(at) => tokio::time::sleep_until(at).await,
                            None => std::future::pending::<()>().await,
                        }
                    } => PassOrigin::Cadence,
                    _ = async {
                        if tick.is_zero() {
                            std::future::pending::<()>().await
                        } else {
                            tokio::time::sleep(tick).await
                        }
                    } => PassOrigin::Backstop,
                };
                // **A DEADLINE SUBSUMES A QUESTION, AND THIS LINE IS WHY THERE IS
                // ONLY EVER ONE PASS.** If the pool's cadence is due, THIS pass serves
                // it whatever woke it — an event, the poll, or the cadence itself.
                //
                // Written as two passes, it was a real bug and a flaky test found it.
                // The 5-second backstop and a 5-second rung come due in the same
                // millisecond; `select!` picks one at random; the loop re-enters and
                // immediately runs the other. The question goes first about half the
                // time, and `retry_firing` — the per-thread coalescer that stops passes
                // stacking asks — then DEDUPES THE DEADLINE AWAY. The pool climbed a
                // rung for an ask that never happened, and the prompt waited another
                // fifteen seconds. `ws_protocol::transient_failure_retries_with_backoff_then_succeeds`
                // is what caught it.
                //
                // Subsuming is sound rather than merely convenient: both routes ask
                // about the SAME rows (there is one candidate set), and the deadline is
                // the strictly wider route. A pass that serves the cadence has done
                // everything a question would have done and more. So a coincidence
                // costs nothing, and the pool's deadline is honoured at the first
                // opportunity instead of at a second pass that may be deduped.
                let cadence_due = due_at
                    .map(|at| at <= tokio::time::Instant::now())
                    .unwrap_or(false);
                let origin = if cadence_due {
                    PassOrigin::Cadence
                } else {
                    woken
                };
                mgr.placement_pass(origin);
                // **RE-ARM ONLY WHEN THE DEADLINE WAS CONSUMED, OR WHEN THERE WAS
                // NONE.** Re-arming after EVERY pass is a bug, and it was written and
                // caught: an event or a backstop tick would push the deadline out by a
                // fresh rung, so with the 5-second poll running and a 5-second rung the
                // cadence could be starved forever. `budgets_e2e`'s
                // `a_retry_goes_through_the_admission_gate_and_parks_when_it_is_refused`
                // is what found it — the hand-over that a cadence owes never came.
                //
                // A pass that did NOT consume the deadline leaves it alone. The inputs
                // to the rung change in exactly two places and both ring
                // `cadence_rearm`: `schedule_retry` (a new or re-armed prompt, whose
                // attempt is the rung's other index) and `park_retried_turn` (a prompt
                // that left). A row cleared by a success rings nothing, which costs one
                // cadence pass over an empty set and self-corrects on the line below.
                if origin == PassOrigin::Cadence || due.is_none() {
                    due = mgr
                        .cadence_rung()
                        .map(|r| tokio::time::Instant::now() + r);
                }
            }
        });
        // ── THE NOTICER: THE TWO NON-EVENTS, TURNED INTO EVENTS ──────────
        //
        // A clock here is not the thing being removed. Two inputs to placement
        // move without anything in this process happening at the instant they
        // move, so SOMEBODY HAS TO NOTICE THEM — a limit window reaching its end,
        // and the account roster being rewritten by another process. This task's
        // whole job is to convert those two non-events into ordinary events and
        // ring the bell, which keeps the sweep above purely event-driven and
        // keeps the backstop's firing meaningful.
        //
        // It is deliberately NOT the sweep's own task: if the noticing and the
        // sweeping shared a tick, every expiry would be swept by the same timer
        // that noticed it and `backstop_placed` could never distinguish "the
        // event set is incomplete" from "a window ended".
        let mgr = self.clone();
        tokio::spawn(async move {
            // The bar check rides `resume_tick` when there is one, because that
            // is the latency the daemon already promised for a resuming thread.
            // With the backstop switched off it still runs, on its own floor:
            // noticing an expiry is not the fallback, it is an event source.
            let every = if tick.is_zero() {
                Duration::from_secs(1)
            } else {
                tick
            };
            let roster_every = mgr.tunables.roster_reload;
            let mut last_roster = std::time::Instant::now();
            loop {
                tokio::time::sleep(every).await;
                mgr.notice_expired_limits();
                if last_roster.elapsed() >= roster_every {
                    last_roster = std::time::Instant::now();
                    mgr.notice_roster_change();
                }
            }
        });
        // ── THE GAUGE SWEEP, AT TWO RATES ────────────────────────────────
        //
        // **IT USED TO BE ONE FLAT TIMER OVER THE WHOLE POOL**, and
        // that single number had to serve two jobs it cannot serve at once. As
        // the interval that bounds "how long can the daemon keep placing threads
        // onto an account that is already exhausted" it is far too slow — that
        // gap was thirteen minutes and five threads on 2026-08-14, and
        // `gauge_bar_pct` turns the poll into the mechanism that closes it. As
        // the interval at which every login in the pool is polled whether or not
        // anything is happening on it, it is already as fast as it should be:
        // an idle account's reading does not move, so re-reading it costs a
        // `claude` process to learn nothing.
        //
        // The operator named the split himself: *"If we can add a periodic scan
        // to threads at the higher rate anthropic allows, while threads are
        // running only, then that would be useful in many ways and can start the
        // emit."*
        //
        // So there are two legs on one timer:
        //
        // * **The BUSY leg** (`probe_interval_busy_secs`, 120s) polls ONLY the
        //   logins that have a turn or a subagent in flight right now. Usually
        //   one account; frequently none, in which case it does nothing at all
        //   and costs one in-memory walk. This is what makes the board's usage
        //   numbers move on their own while work is running, which is the whole
        //   ask — every recorded reading announces itself through
        //   `probe_account`'s `emit_info`.
        // * **The IDLE leg** (`probe_interval_secs`, 3600s since 2026-09-20,
        //   900s before it) is the whole-pool sweep, including accounts nothing
        //   is running on, which is the only thing that keeps an idle account's
        //   reading from going stale forever.
        //
        //   **IT SWEEPS REGARDLESS OF WHETHER ANYTHING IS RUNNING, and that is
        //   deliberate.** It is gated only on `gauge_needed()` — does anything
        //   on this machine want a gauge reading at all — and never on whether
        //   there is a placement; `probe_accounts_once` polls `poll_targets()`,
        //   which is the pool. Adding an "is anything running" skip here was
        //   considered on 2026-09-20 and REJECTED: this leg is the staleness
        //   floor, and a floor that switches itself off exactly when nothing is
        //   happening is not a floor, it is the 900-second sweep with extra
        //   steps. The operator's ask — *"if there isn't any placement you can
        //   move it to 1x/hour so we don't get rate limited"* — is a CADENCE
        //   change, and it is the default that moved.
        //
        // ONE timer, ticking at the shorter cadence, with each leg checking its
        // own elapsed time. Two timers would drift into phase with each other
        // and double-poll a busy account on the overlap, which is exactly the
        // process cost this design is being careful about.
        let busy_every = self.busy_probe_interval();
        let slow_every = (self.config.probe_interval_secs > 0)
            .then(|| Duration::from_secs(self.config.probe_interval_secs.max(60)));
        let tick = match (busy_every, slow_every) {
            (Some(b), Some(s)) => Some(b.min(s)),
            (Some(b), None) => Some(b),
            (None, Some(s)) => Some(s),
            (None, None) => None,
        };
        if let Some(tick) = tick {
            let mgr = self.clone();
            tokio::spawn(async move {
                let mut last_busy = std::time::Instant::now();
                let mut last_slow = std::time::Instant::now();
                loop {
                    tokio::time::sleep(tick).await;
                    // ── THE ROSTER RE-READ USED TO BE HERE, AND THAT WAS THE
                    //    BUG ─────────────────────────────────────────────
                    //
                    // `reload_pool()` was called on this tick, with the correct
                    // argument that it belongs to neither gauge leg. But it
                    // belongs to neither TASK either, and this task does not
                    // always exist: it is spawned only `if let Some(tick)`, so on
                    // a daemon with `probe_interval_secs = 0` and
                    // `probe_interval_busy_secs = 0` — both legs off, which is a
                    // supported configuration — the roster was never re-read and
                    // an account the operator adopted in a terminal was invisible
                    // until a restart. A placement input whose noticing is
                    // conditional on the GAUGE being switched on is a placement
                    // input that is sometimes not noticed at all.
                    //
                    // It now lives on the noticer task (`notice_roster_change`,
                    // `Tunables::roster_reload`), which is unconditional and which
                    // also distinguishes an account GAINED (`new_account`) from a
                    // roster that merely moved (`pool_reloaded`).
                    if let Some(every) = busy_every {
                        if last_busy.elapsed() >= every {
                            last_busy = std::time::Instant::now();
                            match mgr.probe_busy_accounts().await {
                                // DEBUG, not info, and the asymmetry with the
                                // slow leg below is deliberate: "nothing is
                                // running, so nothing was polled" is the
                                // ordinary state of an idle daemon and is not
                                // news. The slow leg going quiet IS news,
                                // because it means the gauge has no floor left
                                // at all.
                                Err(e) => tracing::debug!(
                                    reason = %e,
                                    "busy-rate gauge scan did not poll this tick"
                                ),
                                Ok(n) => tracing::debug!(
                                    observations = n,
                                    "busy-rate gauge scan recorded readings on the accounts \
                                     that have work on them"
                                ),
                            }
                        }
                    }
                    if let Some(every) = slow_every {
                        if last_slow.elapsed() >= every {
                            last_slow = std::time::Instant::now();
                            match mgr.probe_accounts_once(false).await {
                                // THE POLLER GOING QUIET IS NEWS. At `debug` this was
                                // the daemon losing its only measurement of account
                                // capacity without saying so at any level the operator
                                // ever sees — and once the gauge can BAR, a sweep that
                                // does not poll is a safety mechanism that is not
                                // running.
                                Err(e) => tracing::info!(
                                    reason = %e,
                                    "background gauge sweep did NOT poll — no fresh account usage \
                                     reading this tick"
                                ),
                                Ok(_) => mgr.log_account_pool(),
                            }
                        }
                    }
                }
            });
        }
    }

    /// **The BUSY cadence as a duration, floored** — `None` when the busy scan
    /// is switched off (`probe_interval_busy_secs = 0`), which also switches off
    /// the placement reading that shares the number.
    ///
    /// The floor ([`crate::config::PROBE_BUSY_FLOOR_SECS`]) is applied HERE, in
    /// one place, so the timer and [`Self::placement_poll`]'s staleness bound
    /// can never be derived from two different readings of the same config
    /// field. A hand-written `5` in `config.toml` is clamped rather than
    /// honoured; see the field's docs for why.
    pub fn busy_probe_interval(&self) -> Option<Duration> {
        let secs = self.config.probe_interval_busy_secs;
        (secs > 0).then(|| Duration::from_secs(secs.max(crate::config::PROBE_BUSY_FLOOR_SECS)))
    }

    /// **THE DRAIN, AS THE SUPERVISOR ASKS FOR IT** (R9.C7c, gap C-7).
    ///
    /// `true` begins one ([`UpdateGate::begin_drain`]: no new turn starts, new
    /// prompts stay queued, running turns finish). `false` cancels one — the
    /// pin went back to the build that is running — and then RE-ASKS THE QUEUE
    /// at once, because the prompts that arrived during the drain were refused
    /// admission and nothing else rings for them until the next event or the
    /// placement poll. Returns whether the state changed.
    pub fn set_draining(&self, on: bool) -> bool {
        if on {
            self.update.begin_drain()
        } else {
            let ended = self.update.end_drain();
            if ended {
                let placed = self.resume_sweep_placing();
                tracing::info!(?placed, "drain cancelled: the queue was re-asked");
            }
            ended
        }
    }

    /// Re-check every parked / queued thread. This is what makes "when usage
    /// resets or config changes make a parked thread admissible, it resumes"
    /// true without anyone poking the daemon.
    pub fn resume_sweep(&self) {
        let _ = self.resume_sweep_placing();
    }

    /// The same sweep, SAYING WHICH THREADS IT PLACED.
    ///
    /// The return value is what makes the backstop observable
    /// ([`Self::placement_pass`]): "the poll ran" is not news, "the poll started
    /// a turn nobody rang a bell for" is. It is a list rather than a count
    /// because a test asserting on placement has to be able to name the thread,
    /// and because the defect report is only actionable if it says which thread
    /// was stranded.
    pub fn resume_sweep_placing(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .store
            .list_parked_turns()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.thread_id)
            .collect();
        for t in self.store.threads_with_waiting_queue().unwrap_or_default() {
            if !ids.contains(&t) {
                ids.push(t);
            }
        }
        let mut placed = Vec::new();
        for id in ids {
            if let Ok(state) = self.get_state(&id) {
                if self.maybe_dispatch(&state) {
                    placed.push(id);
                }
            }
        }
        placed
    }

    /// **A LIMIT WINDOW REACHING ITS END, TURNED INTO AN EVENT.**
    ///
    /// This is the one input to placement that has no event to ring, and the
    /// reason is that NOTHING HAPPENS at the instant it moves: a bar ends because
    /// a clock passed a number, in no process, so somebody has to look. The old
    /// code knew this and admitted it in a comment — *"the one input that has no
    /// event to ring, a limit window quietly expiring"* — and answered it by
    /// polling the sweep. That answer wires a clock into the MECHANISM.
    ///
    /// This answers it by wiring the clock into an EVENT SOURCE instead: look,
    /// and when a window has ended since the last look, ring `limit_expired` like
    /// any other cause. Placement stays event-driven and the backstop stays a
    /// backstop, which is what makes `backstop_placed` mean anything.
    ///
    /// # Which of the three retractions this is, and why it is not the others
    ///
    /// A bar stops holding three ways and each already has its own event:
    /// `note_account_served` CLEARS the column (`limit_cleared`), a gauge reading
    /// under the ceiling FALSIFIES it (`limit_falsified_by_gauge`), and
    /// re-fingerprinting makes it STALE (`refresh_identities` re-asserts the
    /// level — the precedent this is shaped after). Only expiry was missing.
    ///
    /// So the test here is narrow on purpose: the row still CARRIES a window, it
    /// is not stale, and the window's end is at or before now. A cleared column
    /// fails the first clause and a falsified identity the second, so this never
    /// double-rings a retraction another event already announced.
    ///
    /// The comparison is the string compare `effective_limit_until` uses, for the
    /// reason that accessor gives: both instants are written by `to_rfc3339()` on
    /// a `Utc` value, and the gate and the noticer must not disagree about expiry
    /// because one parses and the other compares bytes.
    ///
    /// # It covers the CREDIT bar too, which had no event at all
    ///
    /// A credit top-up is a human action on Anthropic's side that this daemon
    /// cannot observe. `note_account_credit_exhausted` handles that honestly by
    /// writing a `limit_until` of `now + CREDIT_BAR_SECS` — *"honestly a RE-PROBE
    /// INTERVAL rather than a prediction"* — so "the credit bar ended, let one
    /// turn through to find out" is the same expiry edge as a quota window's, on
    /// the same column, and it needs no second mechanism.
    ///
    /// Returns the labels it rang for, so a test asserts on names rather than on
    /// a side effect.
    pub fn notice_expired_limits(&self) -> Vec<String> {
        // Wiped slots first: one that has been signed in leaves the set and
        // rings for itself. One that is still wiped stays barred, so its timed
        // login bar ending is NOT an edge — ringing it would re-run placement
        // onto a slot nothing can run on.
        self.recheck_sign_in_needed();
        let wiped = self.sign_in_needed_labels();
        let now = crate::store::now_rfc3339();
        let states = self.store.list_account_states().unwrap_or_default();
        let mut barred_now = std::collections::BTreeSet::new();
        let mut ended_now = std::collections::BTreeSet::new();
        for s in &states {
            if s.limit_is_stale() {
                continue;
            }
            match s.limit_until.as_deref() {
                Some(t) if t > now.as_str() => {
                    barred_now.insert(s.name.clone());
                }
                Some(_) => {
                    ended_now.insert(s.name.clone());
                }
                None => {}
            }
        }
        // THE EDGE, taken against the last look. A label that was barred then and
        // whose window has ended now is the transition; everything else is a
        // condition, and a condition rung every tick is the poll this replaces.
        let expired: Vec<String> = {
            let mut seen = self.barred_seen.lock().unwrap();
            let out = seen
                .iter()
                .filter(|n| ended_now.contains(*n) && !wiped.contains(*n))
                .cloned()
                .collect();
            *seen = barred_now;
            out
        };
        for name in &expired {
            tracing::info!(
                account = %name,
                "LIMIT WINDOW ENDED — placement re-runs now rather than on the next poll"
            );
            // The full bell: this one DOES announce, because no other producer
            // said anything. A window ending is invisible to every client until
            // somebody emits, which is why the board kept drawing `barred=true`.
            self.account_picture_changed_for("limit_expired", Some(name));
            self.emit_limit_fact(name);
        }
        expired
    }

    /// **THE ROSTER WAS REWRITTEN BY ANOTHER PROCESS, TURNED INTO AN EVENT.**
    ///
    /// The second non-event. `reload_pool` has always been the re-read; what it
    /// did not have was an unconditional caller (it rode the gauge tick, which
    /// does not exist when both probe cadences are zero) or a reason tag that
    /// distinguished an account JOINING from the roster merely moving.
    ///
    /// `new_account` versus `pool_reloaded` is computed here, from the pool before
    /// and after, rather than inside `reload_pool` — deliberately, because
    /// `accounts.rename` also calls `reload_pool` and deliberately does NOT ring
    /// (*"nothing about PLACEMENT moved: the same logins are selectable, under one
    /// new name"*). Moving the ring into the re-read would make a rename ring
    /// `new_account`, which is both wrong and the opposite of what that call site
    /// argues for.
    ///
    /// Returns the reason it rang, or `None` when the roster had not moved — a
    /// reload that found the same roster must not announce, for the reason
    /// `reload_pool`'s own docs give: a client that re-reads on a no-op is a
    /// client taught to poll again.
    pub fn notice_roster_change(&self) -> Option<&'static str> {
        let before: std::collections::BTreeSet<String> =
            self.account_pool().into_iter().collect();
        if !self.reload_pool() {
            return None;
        }
        let after: std::collections::BTreeSet<String> =
            self.account_pool().into_iter().collect();
        let gained: Vec<String> = after.difference(&before).cloned().collect();
        if gained.is_empty() {
            self.account_picture_changed_for("pool_reloaded", None);
            return Some("pool_reloaded");
        }
        tracing::info!(
            accounts = ?gained,
            "NEW ACCOUNT IN THE POOL — placement re-runs now rather than at the next restart"
        );
        // Named when exactly one account joined, which is the real case (a human
        // adopts one slot at a time). Two at once is honestly fleet-wide, for the
        // same reason a rotation pass is.
        //
        // Both reasons are written as LITERALS at their own ring rather than
        // computed into a variable, so the guard that reads the event set out of
        // `src/` can see them. A reason handed through a binding is invisible to
        // that scan, and an event set nothing can enumerate is one that decays.
        let named = (gained.len() == 1).then(|| gained[0].clone());
        self.account_picture_changed_for("new_account", named.as_deref());
        Some("new_account")
    }

    /// Does anything actually need the gauge?
    ///
    /// Three reasons, any one of which is enough: a budget key exists, some
    /// thread carries a key or a threshold, or CALIBRATION still wants a reading.
    ///
    /// That third clause is the fix for the defect that dominated everything else
    /// in this area. Enforcement was the only reason to poll, the audio interface
    /// sets neither a budget key nor a threshold, and so in the real deployment
    /// **no `/usage` observation was ever recorded** — which means zero brackets,
    /// zero samples, and `tokens_per_percent` absent forever however the threads
    /// were arranged. Percentage budgets are blocked on calibration and
    /// calibration is blocked on a reading, so "we want to calibrate" has to be a
    /// reason to poll in its own right.
    /// Polling asks what is ENFORCED, not what EXISTS (INTENT 72b).
    ///
    /// Both tests here used to key off mere existence — `any_configured`, and
    /// `budget_key.is_some()` on any thread. Once every thread inherits
    /// `aui:default` those are true forever, and this would have switched
    /// `/usage` polling on permanently across the whole fleet. Polling costs
    /// real requests against the operator's own accounts, so turning it on is a
    /// behaviour change, and this change is supposed to be invisible to him.
    /// A budget that cannot refuse anything needs no gauge to enforce against.
    ///
    /// ── AND THE FOURTH CLAUSE, WHICH SUPERSEDES THE PARAGRAPH ABOVE ──
    ///
    /// "A budget that cannot refuse anything needs no gauge to enforce against"
    /// was true only while the gauge could not refuse anything either. It can
    /// now: `probe_account` writes `accounts.limit_until` on a measured
    /// exhausted reading (`Config::gauge_bar_pct`), which makes the poll itself
    /// an ENFORCEMENT mechanism and not merely an input to one.
    ///
    /// Without this clause the whole of that fix is a no-op on the exact
    /// deployment it was written for. Nothing there enforces and no thread
    /// carried a threshold, so the ONLY reason the sweep ran at all was
    /// incomplete calibration — and the moment the last axis of the last
    /// account calibrates, `probe_accounts_once(false)` refuses, the poller
    /// stops, and the daemon goes blind again with no log above `debug` to say
    /// so. The cost argument that produced the old wording still stands and is
    /// answered by the two probe intervals — `probe_interval_busy_secs` (120s)
    /// while work is running, which is when an exhausted account can actually be
    /// reached, and `probe_interval_secs` (3600s since 2026-09-20) for the idle
    /// pool — not by switching the meter off.
    fn gauge_needed(&self) -> bool {
        let bar = self.config.gauge_bar_pct;
        if bar > 0.0 && bar <= 100.0 {
            return true;
        }
        if self.budgets.any_enforcing().unwrap_or(false) {
            return true;
        }
        if self.calibration_wanted() {
            return true;
        }
        self.store
            .list_threads(500)
            .unwrap_or_default()
            .iter()
            .any(|r| {
                ThreadSettings::from_stored(&r.settings_json)
                    .map(|s| !s.thresholds.is_empty() || self.budget_can_refuse(&s.budget_key))
                    .unwrap_or(false)
            })
    }

    /// Is there a (selectable account, axis) pair we still have no calibration
    /// for? Dead logins and alias labels are not calibration targets — measuring
    /// a login twice under two names produces two numbers for one limit.
    fn calibration_wanted(&self) -> bool {
        if !self.config.calibrate {
            return false;
        }
        self.selectable_pool()
            .iter()
            .any(|a| self.account_uncalibrated(a))
    }

    /// Does this account still have an axis with no limit estimate at all?
    fn account_uncalibrated(&self, account: &str) -> bool {
        Axis::ALL.iter().any(|axis| {
            matches!(self.store.get_estimate(account, axis.as_str()), Ok(None))
        })
    }

    /// **The labels to poll `/usage` on: ONE PER LOGIN, never one per label.**
    ///
    /// `calibration_wanted` already states the rule this enforces — *"Dead
    /// logins and alias labels are not calibration targets — measuring a login
    /// twice under two names produces two numbers for one limit"* — but it
    /// applied the rule only to the question "does anything still want a
    /// reading". The POLL itself asked `registry.names()`, the raw visible list,
    /// so the rule was stated in one function and broken in the next.
    ///
    /// **Measured on the operator's own daemon, 2026-08-06.** `default` and
    /// `alice-example-com` carry one identity fingerprint — the machine login
    /// had been signed into the new account, which is exactly how a label comes
    /// to alias a real slot. Both were polled. `usage_observations` therefore
    /// holds two independent streams for one pool, and they disagree: `default`
    /// last read 100% on all three axes while `alice-example-com` read 12% on
    /// the weekly one. `limit_estimates` is keyed the same way, so the same
    /// login also calibrates twice, under two names, from interleaved samples.
    ///
    /// Two properties, and the second is why this is not simply
    /// `selectable_pool()`:
    ///
    /// * **One measurement per login.** Labels collapse by identity, and a
    ///   label whose identity cannot be read is never collapsed with anything
    ///   (absence of evidence must not merge two real accounts).
    /// * **Measure it under the name PLACEMENT will read.** Within a login the
    ///   canonical label is a pool name, because the gauge, the budget meters
    ///   and the calibration are all keyed by label: a reading recorded against
    ///   a name placement never consults is a reading nothing will ever use.
    ///
    /// # This function used to SPAWN `claude` AS THE OPERATOR
    ///
    /// It collapsed `registry.names()` — the slot list — and per login preferred
    /// a placeable member, falling back to `u.canonical` when the login had
    /// none. `default` is a slot with no readable-alias guarantee, so whenever
    /// the machine login was nobody's alias that fallback returned `"default"`.
    /// `probe_account("default")` then resolved a config dir of `None`, and
    /// `None` means leave `CLAUDE_CONFIG_DIR` UNSET — so the daemon ran
    /// `claude -p "/usage"` against `~/.claude`, the operator's own terminal
    /// login, on a timer, and wrote the result back under `"default"` where it
    /// was drawn as a bar row. The old doc comment described this as intended
    /// ("keeps the machine login polled while it is nobody's alias").
    ///
    /// It is not intended: *"threads should never access my default account."*
    /// The comment above is kept as a TOMBSTONE — it records a behaviour that
    /// ran against his personal login on a timer, and a record of that is worth
    /// more than a clean file — but nothing in it blesses anything.
    ///
    /// This function's earlier repair said "the fallback still exists and is
    /// still correct, it simply has no non-pool name left to fall back to."
    /// That sentence was wrong, and it is struck. Iterating the pool removed
    /// the only NAME that resolved to no config dir; it did not remove the
    /// RESOLUTION, and [`Self::probe_account`] would still have polled without
    /// a config dir — which is to say against `~/.claude` — for any name that
    /// is not a pool member or any pool entry that is not a Claude account.
    /// The refusal now lives there, at the access, and there is no fallback of
    /// this shape left anywhere on the polling path.
    ///
    /// Public for the same reason [`Self::retries_to_reask`] is: the DECISION is
    /// what is worth testing, and testing it must not require shelling out to
    /// `claude` and watching how many times it was called.
    ///
    /// R9.A2c (CON-28): a DISABLED account is not a poll target. Its `/usage`
    /// is not asked — a disabled slot is not checked at all — and a login
    /// shared with an enabled label is still polled once, under that label.
    pub fn poll_targets(&self) -> Vec<String> {
        let names = self.enabled_pool_names();
        let states = self.account_states();
        crate::accounts::selection_units(&names, &states)
            .into_iter()
            .map(|u| u.canonical)
            .collect()
    }

    /// Every pool label whose account is NOT disabled — the gauge poll's
    /// population (R9.A2c, CON-28).
    fn enabled_pool_names(&self) -> Vec<String> {
        self.pool()
            .names()
            .into_iter()
            .filter(|n| !self.budgets.is_disabled(n))
            .collect()
    }

    /// Poll `/usage` once per LOGIN and fold the readings into
    /// the gauge (DEC-15 layer 2 — the only source covering all three axes).
    ///
    /// `force` distinguishes the two callers. The BACKGROUND sweep passes
    /// `false` and skips the poll when nothing on this machine could use a
    /// gauge reading — a machine with no budgets and no thresholds should never
    /// shell out to `claude`. An EXPLICIT `gauge.probe` passes `true`: the
    /// operator asked for a reading, so they get one, and the reading is useful
    /// on its own (it is what makes calibration possible in the first place).
    ///
    /// Returns `Ok(rows_recorded)`, or `Err(reason)` when the poll did not run
    /// — never a silent zero, which is how "poll now" used to report doing
    /// nothing at all.
    pub async fn probe_accounts_once(&self, force: bool) -> std::result::Result<usize, String> {
        if !force && !self.gauge_needed() {
            return Err(
                "no budget key, no threshold, `gauge_bar_pct` disabled, and every axis of \
                 every selectable account is already calibrated (or `calibrate` is off), so \
                 the background sweep does not poll — call gauge/probe explicitly to poll \
                 anyway"
                    .to_string(),
            );
        }
        let names = self.poll_targets();
        if names.is_empty() {
            return Err("no accounts are registered, so there is nothing to poll".to_string());
        }
        let (n, failures) = self.probe_set(&names).await;
        // The whole sweep's failures, kept where a caller can read them —
        // `gauge.probe` reports them on a partial success, and
        // `last_probe_failures` is what a test can assert on. Replaced
        // wholesale, so a recovered account clears itself.
        //
        // **THE BUSY LEG DELIBERATELY DOES NOT WRITE HERE** (see
        // [`Self::probe_busy_accounts`]): this slot means "what the last
        // WHOLE-POOL sweep could not poll", and letting a scan that only looks
        // at one busy account replace it wholesale would silently narrow that
        // sentence to "what the last one account could not poll" while every
        // reader kept the old meaning.
        *self.probe_failures.lock().unwrap() = failures.clone();
        if n > 0 {
            // A fresh reading can unblock a parked thread immediately.
            self.resume_sweep();
        }
        if n == 0 && !failures.is_empty() {
            return Err(failures.join("; "));
        }
        Ok(n)
    }

    /// **POLL ONLY THE ACCOUNTS THAT HAVE WORK ON THEM** — the busy leg of the
    /// two-rate sweep (`probe_interval_busy_secs`).
    ///
    /// The operator's constraint on this was explicit and is the reason this is
    /// a separate function rather than a faster `probe_accounts_once`: *"do not
    /// let the busy-rate scan poll accounts that are not involved. Prefer
    /// polling the accounts that actually have work on them."* An idle account's
    /// reading does not move, so re-reading it at the busy rate would spend a
    /// `claude` process to learn nothing — and would do it once every two
    /// minutes, per idle login, forever.
    ///
    /// Same gate as the slow sweep ([`Self::gauge_needed`]), because the
    /// question "does anything on this machine want a gauge reading" is not a
    /// function of how fast we are asking. Same per-login collapsing as
    /// [`Self::poll_targets`], for the same reason: measuring one login twice
    /// under two labels produces two disagreeing streams for one limit.
    ///
    /// Returns `Err` with a REASON rather than `Ok(0)` when it does not poll —
    /// "nothing is running" and "the probe failed on every busy account" are
    /// different sentences and the caller logs them differently.
    pub async fn probe_busy_accounts(&self) -> std::result::Result<usize, String> {
        if !self.gauge_needed() {
            return Err(
                "nothing on this machine wants a gauge reading, so the busy-rate scan does \
                 not poll either — the rate is about FRESHNESS, never about whether to \
                 measure at all"
                    .to_string(),
            );
        }
        let names = self.busy_poll_targets();
        if names.is_empty() {
            return Err(
                "no account has a turn or a subagent in flight, so there is nothing the \
                 busy-rate scan should poll — an idle account's reading does not move"
                    .to_string(),
            );
        }
        let (n, failures) = self.probe_set(&names).await;
        if n > 0 {
            // Identical to the slow sweep's reason: a fresh reading can lift a
            // threshold park or a measured bar, and a parked thread must not
            // wait for the 900s tick to find out.
            self.resume_sweep();
        }
        if n == 0 && !failures.is_empty() {
            return Err(failures.join("; "));
        }
        Ok(n)
    }

    /// **THE LOGINS WITH OUR WORK ON THEM RIGHT NOW**, one label per login, in
    /// the name placement and the gauge are keyed by.
    ///
    /// Public because it is the DECISION the busy scan is built on, and a
    /// decision worth making is worth testing without shelling out to `claude`
    /// and counting invocations.
    ///
    /// Aliases are collapsed the same way [`Self::poll_targets`] collapses them,
    /// and a login is "busy" when ANY of its labels has work: a thread running
    /// under one label of a login is spending the same quota the canonical label
    /// reports, so polling the canonical label is the correct measurement and
    /// polling both would be the double-count `poll_targets` already forbids.
    pub fn busy_poll_targets(&self) -> Vec<String> {
        let busy = self.accounts_with_work();
        if busy.is_empty() {
            return Vec::new();
        }
        // R9.A2c (CON-28): never a disabled account, as in `poll_targets`.
        let names = self.enabled_pool_names();
        let states = self.account_states();
        crate::accounts::selection_units(&names, &states)
            .into_iter()
            .filter(|u| u.members.iter().any(|m| busy.contains(m)))
            .map(|u| u.canonical)
            .collect()
    }

    /// Every account name something of ours is in flight on, as raw LABELS.
    ///
    /// The same two sources [`Self::in_flight_count`] walks — the in-memory
    /// thread map, plus the durable `running_turns` rows for a turn re-adopted
    /// after a restart that is not in the map yet — but walked ONCE for the
    /// whole pool instead of once per account. That matters because this runs on
    /// a timer: `in_flight_count` issues a `list_running_turns` read per call,
    /// so asking it per pool member would turn an idle tick into N database
    /// reads to discover that the answer is "nothing".
    fn accounts_with_work(&self) -> HashSet<String> {
        let states: Vec<Arc<ThreadState>> =
            self.threads.lock().unwrap().values().cloned().collect();
        let mut out: HashSet<String> = HashSet::new();
        for s in &states {
            if s.turn_running.load(Ordering::SeqCst) || s.live.lock().unwrap().busy() {
                let a = s.account.lock().unwrap().clone();
                if !a.is_empty() {
                    out.insert(a);
                }
            }
        }
        for r in self.store.list_running_turns().unwrap_or_default() {
            if states.iter().any(|s| s.id == r.thread_id) {
                continue;
            }
            if let Ok(Some(row)) = self.store.get_thread(&r.thread_id) {
                if let Some(a) = row.account.filter(|a| !a.is_empty()) {
                    out.insert(a);
                }
            }
        }
        out
    }

    /// Poll a named set of logins, folding every reading into the gauge.
    ///
    /// The shared body of the slow sweep and the busy scan. Split out so the two
    /// cannot drift in how they treat a per-account failure — which is the exact
    /// drift this function's own history warns about: `probe_accounts_once` used
    /// to discard every failure but the last, and one sibling succeeding hid an
    /// account that had not polled in weeks.
    ///
    /// Returns `(observations recorded, per-account failure reasons)`.
    async fn probe_set(&self, names: &[String]) -> (usize, Vec<String>) {
        let mut n = 0;
        let mut failures: Vec<String> = Vec::new();
        for name in names {
            match self.probe_account(name).await {
                Ok(rows) => {
                    n += rows;
                    // WARN-1 N-2: the recovery is said once, at the level the
                    // failure was, so the episode has both ends on the shape.
                    let was = self.probe_failing.lock().unwrap().remove(name);
                    if let Some(failed) = was {
                        tracing::warn!(
                            account = %name,
                            failed_probes = failed,
                            "usage probe RECOVERED for this account"
                        );
                    }
                }
                Err(e) => {
                    // **SAID OUT LOUD, WHATEVER THE SIBLINGS DID.** This used
                    // to be a silent `push` whose only reader was the `n == 0`
                    // branch below, so one account polling successfully
                    // discarded every other account's reason at no log level
                    // at all. A login that has not polled in weeks then looks
                    // exactly like a healthy one.
                    //
                    // **ONCE PER EPISODE (WARN-1 N-2).** WARN on the account's
                    // FIRST failure; a probe that fails again while it is
                    // still failing is INFO with the running count, so a
                    // login that stays broken is not a warning per sweep. The
                    // reason stays on `last_probe_failures()` every time.
                    let failed = {
                        let mut f = self.probe_failing.lock().unwrap();
                        let c = f.entry(name.clone()).or_insert(0);
                        *c += 1;
                        *c
                    };
                    if failed == 1 {
                        tracing::warn!(
                            account = %name,
                            reason = %e,
                            "usage probe FAILED for this account; the sweep continues with the rest"
                        );
                    } else {
                        tracing::info!(
                            account = %name,
                            reason = %e,
                            failed_probes = failed,
                            "usage probe still failing for this account"
                        );
                    }
                    failures.push(e);
                }
            }
        }
        (n, failures)
    }

    /// What the last `/usage` sweep could not poll — empty when every account
    /// answered. See [`Self::probe_accounts_once`] for why a partial failure
    /// needs somewhere to live.
    pub fn last_probe_failures(&self) -> Vec<String> {
        self.probe_failures.lock().unwrap().clone()
    }

    /// Poll `/usage` for ONE account and fold every axis it reports into the
    /// gauge. The single place a poll happens: the timed sweep, an explicit
    /// `gauge.probe` and both edge polls all come through here, so the
    /// per-account poll bookkeeping ([`EdgeState::polls_since_idle`]) can never
    /// miss a poll.
    ///
    /// A poll is free and inert by construction: `/usage` is a `num_turns: 0`
    /// slash command that writes no ledger row and no `running_turns` row, so it
    /// forms no overlap window and cannot dirty the bracket it is taken for.
    /// [`Self::probe_account`], reachable from an integration test.
    ///
    /// Public for the same reason [`Self::poll_targets`] is: the DECISION is
    /// what is worth testing, and the decision that matters here is the REFUSAL
    /// — that a name with no login directory is never polled against the
    /// operator's own `~/.claude`. Proving that through the private path would
    /// require watching how `claude` was spawned, which is exactly the
    /// observation that is impossible to make safely.
    #[doc(hidden)]
    pub async fn probe_account_for_test(&self, name: &str) -> std::result::Result<usize, String> {
        self.probe_account(name).await
    }

    async fn probe_account(&self, name: &str) -> std::result::Result<usize, String> {
        // **NO CONFIG DIR IS A REFUSAL, NOT A DEFAULT.**
        //
        // This is the door the usage-polling fallback actually walked through,
        // and closing the NAME supply did not close it. `probe.probe(None)`
        // means "leave `CLAUDE_CONFIG_DIR` unset", and an unset config dir means
        // `~/.claude` — the operator's own terminal login. That is how the
        // daemon came to run `claude -p "/usage"` against his personal session
        // on a timer, and the comment that used to sit above `poll_targets`
        // described it as intended.
        //
        // > *"I'm really disappointed that you found a comment that said that
        // > because I've been trying to purge this behavior for a long time."*
        //
        // Removing `default` from the pool removed the only name that RESOLVED
        // to `None` in practice; it did not remove the resolution. A name that
        // is not a pool member, or a pool entry that is not a Claude account,
        // still resolves to `None` here, and every one of those would spawn
        // against his login exactly as before. So the refusal is placed at the
        // ACCESS, where it holds for any caller and any future resource type,
        // rather than at whichever list happens to feed it today.
        //
        // There is no correct fallback available at this point. A poll needs a
        // login to poll, and "the machine's" is precisely the answer that is
        // forbidden.
        //
        // R9.A2c (CON-28): a DISABLED account is not polled either — by the
        // timed sweep (`poll_targets` already leaves it out) or by an edge poll
        // or an explicit probe naming it. The refusal is here, at the access,
        // for the same reason the one above is.
        if self.budgets.is_disabled(name) {
            return Err(format!(
                "{name}: the account is disabled, so its /usage is not polled — a disabled \
                 account is not checked at all"
            ));
        }
        let Some(cd) = self.config_dir_for(name) else {
            return Err(format!(
                "{name}: no login directory, so there is nothing to poll. REFUSED rather than \
                 polled without one — an unset CLAUDE_CONFIG_DIR would run /usage against the \
                 operator's own terminal login, which this daemon never does."
            ));
        };
        let cd = Some(cd);
        let probe = self.probe.clone();
        let rows = tokio::task::spawn_blocking(move || probe.probe(cd.as_deref()))
            .await
            .ok()
            .flatten();
        let Some(rows) = rows else {
            return Err(format!(
                "{name}: the /usage probe returned nothing (claude not runnable, \
                 timed out, or its output did not parse)"
            ));
        };
        let mut n = 0;
        for (pool, pct, reset_prose) in rows {
            let Some(axis) = Axis::from_pool_label(&pool) else {
                // A model-scoped pool we have no axis for. Recorded as a miss
                // rather than mapped onto the wrong axis.
                tracing::debug!(account = %name, %pool, "unmapped /usage pool label");
                continue;
            };
            let resets_at = reset_prose.as_deref().and_then(gauge::prose_to_rfc3339);
            if let Err(e) = self
                .budgets
                .observe(name, axis, pct, resets_at, "usage_poll")
            {
                tracing::warn!(error = %e, "recording /usage observation failed");
            } else {
                n += 1;
                // THE POLL IS THE REFRESH. This is the reading whose age the
                // board reports, so the instant it lands is the instant a
                // client's "last refresh" clock should move — whether the poll
                // was the 900s background sweep or a `gauge.probe` he asked
                // for. Announced per reading rather than per account: clients
                // coalesce (see `Push::Info`), and a per-account roll-up would
                // have to wait for a slow axis before saying anything at all.
                self.emit_info(
                    "gauge",
                    "usage_poll",
                    Some(name),
                    Some(format!("{name}: {} is now {pct:.0}%", axis.as_str())),
                );
                // P1, second record site. The `/usage` poller keeps its
                // existing cadence (120 s busy / 900 s idle) — that is the
                // operator's liveness-critical path and nothing here changes
                // it. This only carries what it already announced.
                self.emit_gauge_fact(name, axis);
                // ── AND IT WAKES PLACEMENT, WHICH IT DID NOT ─────────────
                //
                // **A THRESHOLD CROSSING RANG NOTHING, AND A THRESHOLD IS WHAT
                // PARKS MOST THREADS.** This function already rings the bell on
                // the two ends of the range — it writes a bar
                // (`note_account_limit_for` → `limit_recorded`) or retracts one
                // (`limit_falsified_by_gauge`) — and said nothing in between. But
                // a thread's `Thresholds` are its own number, well below any bar:
                // a reading that takes an account from 96% back to 91% crosses a
                // 95% threshold, unparks every thread carrying it, and under the
                // old wiring moved placement SILENTLY. The whole window was
                // invisible because the two endpoints were wired and the interior
                // was not.
                //
                // `placement_wake` and not the full bell, because this reading has
                // already announced itself three lines up, on the `gauge` domain,
                // with the number in it. Routing it through
                // `account_picture_changed_for` as well would publish one fact
                // twice on two domains; the wake is the half that was missing.
                //
                // Per READING rather than per account, matching the `emit_info`
                // above — and it costs nothing extra, because the bell holds one
                // permit: four axes on one account is four stores and one pass.
                self.placement_wake("usage_observed");
            }

            // ── THE GAUGE SPEAKS UP ──────────────────────────────────────
            //
            // Every reading is now SAID, at a level that matches what it
            // means, because before this the daemon's own poller read
            // `weekly_all = 100.0` on 2026-08-14T19:26:13 and the operator's
            // only clue was thirteen minutes of nothing. The one precedent in
            // this crate for logging a POLICY condition rather than a
            // bookkeeping failure is `budgets`' "parked with usable accounts it
            // is not allowed to see" warn; this is the second.
            let in_flight = self.in_flight_count(name);
            if pct >= 100.0 && in_flight > 0 {
                tracing::error!(
                    account = %name, axis = axis.as_str(), %pct, threads = in_flight,
                    resets = reset_prose.as_deref().unwrap_or("<none>"),
                    "ACCOUNT EXHAUSTED WITH OUR WORK ON IT — every turn and every subagent \
                     running here is going to be refused"
                );
            } else {
                // ── THERE IS NO `account usage is high` WARN, AND ITS ABSENCE
                //    IS THE OPERATOR'S OWN INSTRUCTION ──────────────────────
                //
                // > *"the 'account usage is high' alert is stupid"*
                //
                // `else if pct >= 80.0 { tracing::warn!(..., "account usage is
                // high") }` STOOD HERE. He is right, and the reason it was wrong
                // is worth writing down rather than just deleting:
                //
                //   * IT WAS A LEVEL TEST, NOT A CROSSING. It fired on every
                //     reading above the line — once per AXIS, per ACCOUNT, per
                //     SWEEP, with no hysteresis and no edge detection. Four axes
                //     on four accounts at `probe_interval_busy_secs = 120` is up
                //     to 2,880 WARN lines a day saying the same unchanged thing,
                //     which is the warning-volume complaint from the other end.
                //   * IT WAS NOT A FAULT. 80% of a quota is the gauge doing its
                //     job. The precedent cited above — `budgets`' "parked with
                //     usable accounts it is not allowed to see" — is a POLICY
                //     CONTRADICTION the daemon cannot resolve by itself. A high
                //     reading is a measurement, and nothing is wrong.
                //   * THE CASE IT WAS REACHING FOR IS ALREADY COVERED, one arm
                //     up and louder: `pct >= 100.0 && in_flight > 0` is
                //     `error!`, because THAT is a fault — work is placed on an
                //     account that will refuse it. That arm is untouched.
                //
                // THE READING IS NOT LOST. It falls through to the `info!` below,
                // which carries the same account, axis and percent it always
                // did, so the series he can reconstruct from the log is
                // unbroken. What is gone is the ALERT, which is what he asked
                // for — not the observation.
                //
                // `resets` is the one field that does not survive, because it
                // was only ever there to make the warn actionable and the `info!`
                // arm never carried it. The reset instant is on
                // `accounts.limit_until` and on the `accounts` shape's `limit`
                // cell, which is where a consumer reads it.
                tracing::info!(
                    account = %name, axis = axis.as_str(), %pct, threads = in_flight,
                    "account usage"
                );
            }

            // ── AND THE GAUGE CAN NOW BAR ────────────────────────────────
            //
            // A MEASURED exhausted percent, written to `accounts.limit_until`
            // exactly as a 429 would write it. This is the fix: the poller had
            // the fact thirteen minutes before the 429 did and no way to say it.
            //
            // CON-5 forbids ENFORCING on a fabricated percent before
            // calibration. This is not fabricated and it is not an estimate —
            // it is the number `/usage` reported, and `budgets`' own module doc
            // says observed percents "bite from the very first observation, with
            // no estimate at all". A wrong bar also self-heals from two
            // directions: `note_account_served` clears the window on any
            // completed turn, and `limit_is_stale` retires one whose login
            // identity has moved.
            //
            // `axis.as_str()`, NOT `pool` — the raw `/usage` label. The 429 path
            // writes `limit_axis` from `classify_limit_pool`, which is an axis
            // string, and a column written in two vocabularies is a column that
            // means nothing.
            let ceiling = self.config.gauge_bar_pct;
            if ceiling > 0.0 && ceiling <= 100.0 {
                if pct >= ceiling {
                    self.note_account_limit_for(name, reset_prose.as_deref(), Some(axis.as_str()));
                } else {
                    // ── AND IT CAN NOW UN-BAR, WHICH IS THE OTHER HALF ───────
                    //
                    // The comment above claimed "a wrong bar also self-heals
                    // from two directions: `note_account_served` clears the
                    // window on any completed turn". That is the bar's own
                    // alibi and it is FALSE, because the bar is what stops a
                    // turn from starting. On 2026-10-06 this poller read
                    // `session = 8%` on `owner-gmail` at 03:44:43Z while
                    // `accounts.limit_until` held `2026-10-07T03:19:00Z`, said
                    // "account usage pct=8" into the log, and left the bar
                    // exactly where it was. There was no path back at all: a
                    // completed turn was impossible and `note_login_restored`
                    // only clears the `login` axis.
                    //
                    // So the poll falsifies as symmetrically as it asserts. A
                    // measured percent below the ceiling is the SAME KIND of
                    // evidence as a measured percent above it, out of the same
                    // reading of the same surface — refusing to act on it in
                    // one direction only is what made a misparse permanent.
                    self.note_account_quota_measured_open(name, axis, pct);
                }
            }
        }
        if n > 0 {
            let mut m = self.edge.lock().unwrap();
            m.entry(name.to_string()).or_default().polls_since_idle += 1;
        }
        Ok(n)
    }

    // ── EDGE POLLING (account-selection-design §3.2) ─────────────────────
    //
    // "Did the 15-minute sweep happen to land in the adjacent idle gap?" becomes
    // "a poll lands in the adjacent idle gap by construction". That retires
    // `Unbracketed` — the dominant rejection — because `evaluate_window` needs
    // the `before` poll to sit in `[prev_end, window.start]` and the `after` poll
    // in `[window.end, next_start]`, and those are precisely the two instants
    // edge polling aims at.
    //
    // Two polls per gap rather than one is what turns `IdleMovement` from silence
    // into evidence: a pair in one idle stretch either agrees (the account really
    // was quiet, and the sample carries proof of it) or disagrees (someone else
    // is spending, or our own meter has not settled — either way the sample is
    // refused rather than recorded wrong).

    /// Is edge polling wanted for this account right now?
    ///
    /// Off when disabled in config, and — the part that keeps it cheap — off for
    /// an account nothing needs a reading for. In steady state, once every axis
    /// of an account is calibrated and no budget names it, its edges stop being
    /// polled at all.
    fn edge_poll_wanted(&self, account: &str) -> bool {
        if !self.config.edge_poll {
            return false;
        }
        // ENFORCING, not merely configured — the fleet-wide default key exists
        // on every thread and gates nothing, so its existence must not by itself
        // start polling the operator's accounts (INTENT 72b).
        if self.budgets.any_enforcing().unwrap_or(false) {
            return true;
        }
        self.config.calibrate && self.account_uncalibrated(account)
    }

    /// Is anything of OURS in flight on `account`? Only in-flight work matters:
    /// a parked or queued turn is not spending, and a poll taken while nothing
    /// runs is exactly the reading a bracket needs.
    fn account_in_flight(&self, account: &str) -> bool {
        self.in_flight_count(account) > 0
    }

    /// **HOW MANY** of ours are in flight on `account` — the same walk
    /// [`Self::account_in_flight`] does, counting instead of short-circuiting.
    ///
    /// It exists because "the account is at 100%" and "the account is at 100%
    /// and six of our threads are on it" are different sentences, and only the
    /// second one is an incident. It is what the probe's escalation ladder
    /// reads to decide between `warn` and `error`.
    fn in_flight_count(&self, account: &str) -> usize {
        let states: Vec<Arc<ThreadState>> =
            self.threads.lock().unwrap().values().cloned().collect();
        let mut n = 0;
        for s in &states {
            if *s.account.lock().unwrap() != account {
                continue;
            }
            if s.turn_running.load(Ordering::SeqCst) || s.live.lock().unwrap().busy() {
                n += 1;
            }
        }
        // A turn re-adopted after a daemon restart has a durable row but may not
        // be in the in-memory map yet.
        for r in self.store.list_running_turns().unwrap_or_default() {
            if states.iter().any(|s| s.id == r.thread_id) {
                continue;
            }
            if let Ok(Some(row)) = self.store.get_thread(&r.thread_id) {
                if row.account.as_deref() == Some(account) {
                    n += 1;
                }
            }
        }
        n
    }

    /// **THE OPENING EDGE: take a reading before the turn starts, by exactly ONE
    /// of two mechanisms.** The counter is zeroed afterwards either way, because
    /// the idle gap is over the moment the turn starts.
    async fn edge_poll_open(&self, account: &str, attempt: Option<&ThreadState>) {
        // ── THE TWO WAYS A DISPATCH CAN TAKE A READING, AND WHY THEY ARE
        //    MUTUALLY EXCLUSIVE ──────────────────────────────────────────
        //
        // Both branches below exist to put a `/usage` reading next to a
        // dispatch. They are ALTERNATIVES, never both, and the `return` in the
        // first arm is what makes that structural rather than a convention:
        //
        // * **The edge PAIR** (the rest of this function) runs when
        //   `edge_poll_wanted` — a budget is enforcing, or calibration still
        //   wants a bracket for this account. It takes up to two REAL, recorded,
        //   announced readings INLINE, awaited, before the window opens. When it
        //   runs, the board already gets a fresh number at placement; there is
        //   nothing left for a second mechanism to add.
        // * **The placement reading** ([`Self::placement_poll`]) runs only when
        //   the pair does NOT. That is the case the feature was built for: on a
        //   fully-calibrated machine with no enforcing budget — the operator's
        //   own daemon — `edge_poll_wanted` is false forever, so before this
        //   existed NO reading was ever taken at placement at all.
        //
        // **THEY USED TO BOTH RUN, AND IT WAS A RACE THAT MADE THE TEST SUITE
        // FLAKY.** `placement_poll` is fire-and-forget, so its `claude -p
        // /usage` child ran CONCURRENTLY with the pair's first one. Measured on
        // `selection_prereqs::edge_polls_bracket_a_window_and_measure_the_settle_delay`,
        // which scripts the meter as `41,41,45,45,45` through a position counter
        // in the account's config dir: the account was polled with that counter
        // read-modify-written by two overlapping processes, the update was lost,
        // and the trace shows **41 read three times from a two-`41` sequence**.
        // The opening pair therefore never landed on the values the bracket
        // needs, `IdleMovement` was never witnessed, the window was never
        // evaluated, and `get_estimate` stayed `None` — the empty `[]` in the
        // failure. It failed roughly one run in four under parallel load and
        // passed serially, which is the worst possible shape: often enough green
        // to look green.
        //
        // The duplicate was never wanted on its own merits either. Two `/usage`
        // spawns at one instant for one account is one wasted process, and the
        // pair's readings are strictly better — inline, ordered, and taken
        // before the window rather than across it.
        if self.edge_poll_wanted(account) {
            self.edge_poll_pair_bounded(account, attempt).await;
            self.begin_window(account);
            return;
        }
        self.placement_poll(account);
        self.begin_window(account);
    }

    /// **THE OPENING PAIR, WAITED ON FOR AT MOST `edge_open_bound`** (R10.M2c,
    /// TC-45, ledger r45). It used to be awaited outright: on 2026-10-27 prod's
    /// `/usage` probes ran 20-60 s, so every cold-gap turn sat 53-62 s with its
    /// attempt open and nothing at the process, and the thread read as
    /// uninterruptible.
    ///
    /// The pair now runs as its own task. This waits for whichever comes first:
    /// - the pair finishing — the bracket is exactly what it always was (fast
    ///   probes change nothing);
    /// - the bound — the pair is ABORTED and the turn starts. A probe's result
    ///   is recorded synchronously after its one `.await`, so an abort either
    ///   lands a whole reading (taken before the window opens) or none: a late
    ///   reading never enters the window as a false `before`. The window then
    ///   lacks its `before` (or its `IdleMovement` witness) and
    ///   `gauge::evaluate_window` rejects it `Unbracketed` — that, plus the
    ///   `warn` below, is how the turn is marked uncalibrated;
    /// - a cancel taking `attempt`'s open attempt — the pair is aborted and the
    ///   dispatch task goes straight to its cancelled epilogue.
    async fn edge_poll_pair_bounded(&self, account: &str, attempt: Option<&ThreadState>) {
        let mgr = self.clone();
        let acct = account.to_string();
        let mut pair = tokio::spawn(async move { mgr.edge_poll_pair(&acct).await });
        let bound = tokio::time::sleep(self.tunables.edge_open_bound);
        tokio::pin!(bound);
        let cancelled = || attempt.is_some_and(|st| st.open_attempt.lock().unwrap().is_none());
        loop {
            tokio::select! {
                _ = &mut pair => return,
                _ = &mut bound => {
                    pair.abort();
                    tracing::warn!(
                        %account, bound_ms = self.tunables.edge_open_bound.as_millis() as u64,
                        "opening /usage pair not done within its bound — the turn starts now and \
                         this window is UNCALIBRATED (no reading is recorded late)"
                    );
                    return;
                }
                _ = tokio::time::sleep(Duration::from_millis(25)), if attempt.is_some() => {
                    if cancelled() {
                        pair.abort();
                        return;
                    }
                }
            }
        }
    }

    /// The opening PAIR: make sure the leading idle gap holds two polls before
    /// the window opens.
    ///
    /// Split out of [`Self::edge_poll_open`] so that function reads as the
    /// either/or it now is. The body is unchanged.
    ///
    /// Normally the gap holds one already — the previous CLOSING ladder left its
    /// rungs in this very gap — and then this costs nothing at all. It only pays
    /// when the gap is cold: a daemon that just started, a ladder cut short by
    /// new work, or an account whose first window this is.
    async fn edge_poll_pair(&self, account: &str) {
        let have = self
            .edge
            .lock()
            .unwrap()
            .get(account)
            .map(|e| e.polls_since_idle)
            .unwrap_or(0);
        let need = 2usize.saturating_sub(have);
        for i in 0..need {
            if i > 0 {
                tokio::time::sleep(self.tunables.edge_pair_gap).await;
            }
            if let Err(e) = self.probe_account(account).await {
                tracing::debug!(reason = %e, "opening edge poll did not record a reading");
                break;
            }
        }
        if need > 0 {
            tracing::debug!(
                %account,
                polls = need,
                "opening edge polls taken before dispatch"
            );
        }
        // `begin_window` is NOT called here: [`Self::edge_poll_open`] owns it on
        // both of its branches, so the window opens exactly once however the
        // reading was taken.
    }

    /// **TAKE A USAGE READING FOR THE ACCOUNT A TURN IS ABOUT TO RUN ON**, if
    /// the one we hold is older than the busy cadence.
    ///
    /// The operator, on the usage numbers: *"It should be happening at least at
    /// placement time so should be emitting."* Placement is the one instant at
    /// which a stale reading is guaranteed to matter — a thread is about to
    /// start spending on this account, the board is about to show a running
    /// count against it, and the number beside that count may be from an hour
    /// ago.
    ///
    /// **FIRE-AND-FORGET, AND THAT IS A DECISION, NOT AN OVERSIGHT.** The
    /// obvious shape is to await the poll before running the turn, which is what
    /// the edge-poll pair below does. It is wrong here. The edge pair is paid
    /// for by CALIBRATION, which needs its `before` reading to sit strictly
    /// before the window opens; this reading is paid for by the BOARD, which
    /// needs a fresh number and does not care whether it lands a second before
    /// or a second after the turn starts. Awaiting it would add a `claude`
    /// process spawn — seconds — to the latency of every turn dispatched onto a
    /// stale account, which is a real cost charged to the operator's own typing
    /// so that a display can be marginally more punctual. It is spawned instead.
    ///
    /// It cannot dirty anything it would have protected: `/usage` is a
    /// `num_turns: 0` slash command that writes no ledger row and no
    /// `running_turns` row (see [`Self::probe_account`]), so a reading taken
    /// alongside a running window forms no window of its own, and the 900s sweep
    /// has always recorded observations mid-window anyway.
    ///
    /// **The staleness bound is the busy cadence itself**, not a knob of its
    /// own. That is deliberate: `probe_interval_busy_secs` is already the
    /// statement "while work is running, a reading older than this is too old",
    /// and placement is the moment work starts running. Two numbers here would
    /// be two places to state one policy, and they would disagree.
    ///
    /// Polls the CANONICAL label of the login, never the raw label the thread
    /// happens to be placed under — same rule as [`Self::poll_targets`], and for
    /// the same reason: a reading recorded against a name placement never
    /// consults is a reading nothing will ever use.
    ///
    /// # Two switches, and why it is under BOTH
    ///
    /// * **`config.edge_poll`** — this IS an edge poll by that field's own
    ///   definition: *"poll at the boundaries of our own activity rather than
    ///   only on a timer"*, and a dispatch is precisely such a boundary. A
    ///   deployment that has said "do not poll at our activity boundaries" has
    ///   already answered this question, and answering it a second way under a
    ///   different name is how a system ends up with two switches for one
    ///   behaviour and no rule for which wins. What it does NOT obey is
    ///   [`Self::edge_poll_wanted`], the PER-ACCOUNT gate — that one asks
    ///   whether CALIBRATION still wants a bracket, which is false forever on a
    ///   calibrated machine and is the reason no reading was ever taken at
    ///   placement before this existed.
    /// * **`probe_interval_busy_secs`** — supplies the staleness bound, and
    ///   switching the busy scan off switches this off with it. The number
    ///   already means "while work is running, a reading older than this is too
    ///   old"; placement is when work starts running.
    fn placement_poll(&self, account: &str) {
        if !self.config.edge_poll {
            return;
        }
        let Some(max_age) = self.busy_probe_interval() else {
            return; // the busy scan is switched off; so is this
        };
        if !self.gauge_needed() {
            return;
        }
        let target = self.canonical_label(account);
        if let Some(age) = self.last_observation_age_secs(&target) {
            if age < max_age.as_secs() as i64 {
                // Fresh enough. The busy scan took it, or the last placement
                // did — either way the board is not showing a stale number and
                // a second `claude` spawn would buy nothing.
                return;
            }
        }
        let mgr = self.clone();
        tokio::spawn(async move {
            match mgr.probe_account(&target).await {
                // `probe_account` RECORDS and only then announces (its
                // `emit_info` sits inside the `Ok` arm of `budgets.observe`), so
                // there is nothing to emit here and emitting anyway would
                // announce a write that may have failed.
                Ok(rows) => tracing::debug!(
                    account = %target, %rows,
                    "placement reading taken — a turn is starting on this account and its \
                     last usage reading was older than the busy cadence"
                ),
                Err(e) => tracing::debug!(
                    account = %target, reason = %e,
                    "placement reading did not record"
                ),
            }
        });
    }

    /// How many seconds old the newest usage observation for this account is,
    /// across every axis. `None` when it has never been observed, or when the
    /// timestamp cannot be parsed — both of which read as "poll it", which is
    /// the safe direction: the cost of a needless poll is one process, and the
    /// cost of skipping a needed one is a number on the board that is silently
    /// wrong.
    fn last_observation_age_secs(&self, account: &str) -> Option<i64> {
        let newest = self.budgets.last_observed_at(account)?;
        let at = gauge::parse_instant(&newest)?;
        Some((crate::clock::now() - at).num_seconds())
    }

    /// The label this login is measured and placed under — its selection unit's
    /// canonical member, or the name itself when it belongs to no unit (an
    /// account with no readable identity is its own login, and absence of
    /// evidence never collapses two real accounts).
    fn canonical_label(&self, account: &str) -> String {
        let names = self.pool().names();
        let states = self.account_states();
        crate::accounts::selection_units(&names, &states)
            .into_iter()
            .find(|u| u.members.iter().any(|m| m == account))
            .map(|u| u.canonical)
            .unwrap_or_else(|| account.to_string())
    }

    /// A window is opening on this account: the idle gap (and any ladder walking
    /// in it) is over.
    fn begin_window(&self, account: &str) {
        let mut m = self.edge.lock().unwrap();
        let e = m.entry(account.to_string()).or_default();
        e.polls_since_idle = 0;
        e.ladder_running = false;
    }

    /// The CLOSING edge: this account just went idle for us. Walk the settle
    /// ladder, recording every rung, and read the settle profile off it.
    ///
    /// Fire-and-forget: nothing waits on a ladder, and any real work reopening
    /// the account abandons it mid-walk (rank 6 — real work never waits on
    /// calibration).
    fn edge_poll_close(&self, account: &str) {
        if !self.edge_poll_wanted(account) || self.settle_ladder.is_empty() {
            return;
        }
        if self.account_in_flight(account) {
            return;
        }
        {
            let mut m = self.edge.lock().unwrap();
            let e = m.entry(account.to_string()).or_default();
            if e.ladder_running {
                return;
            }
            e.ladder_running = true;
            e.polls_since_idle = 0;
            e.rungs.clear();
        }
        let mgr = self.clone();
        let account = account.to_string();
        tokio::spawn(async move { mgr.walk_settle_ladder(account).await });
    }

    /// Walk the settle ladder, then close the bracket — but only with a reading
    /// that has stopped moving.
    ///
    /// Two phases, and the split between them is the whole point:
    ///
    /// 1. **Measure.** Read the meter at each ladder offset WITHOUT recording
    ///    anything, until two consecutive rungs agree. That instant is the
    ///    measured settle delay — the residual the verification pass refused to
    ///    invent a constant for, because the error runs in the dangerous
    ///    direction: a closing reading taken while the meter is still catching up
    ///    understates the delta, an understated delta divides into an overstated
    ///    tokens-per-percent, and an overstated limit is one that never trips.
    ///
    /// 2. **Close.** Only once the reading is settled, take the recorded closing
    ///    PAIR — two real `/usage` observations a few seconds apart. They are what
    ///    `evaluate_window` uses as the `after` bracket and as the `IdleMovement`
    ///    witness that the account really was quiet.
    ///
    /// The unrecorded phase is what makes this safe rather than merely informed.
    /// Recording every rung would have handed the bracket an unsettled `after`
    /// poll — and calibration is evaluated as each observation lands, so the
    /// under-estimate would be written before the later rungs could contradict
    /// it. A raw read costs a free `num_turns: 0` slash command and touches
    /// nothing.
    ///
    /// If the ladder runs out while the meter is still moving, NOTHING is
    /// recorded: the window stays unbracketed until the next poll, which is the
    /// honest outcome. A refused sample costs one free poll; a wrong one
    /// silently disables a budget.
    async fn walk_settle_ladder(&self, account: String) {
        let started = std::time::Instant::now();

        // Already measured on this account this run? Then USE the measurement:
        // wait the settle delay out and go straight to the recorded pair. That is
        // what makes the ladder a one-off cost per daemon run rather than a
        // standing tax on every idle transition. (Held in memory, so a restart
        // re-measures — the number is cheap and a stale one is not worth
        // persisting.)
        let known = self
            .edge
            .lock()
            .unwrap()
            .get(&account)
            .and_then(|e| e.profile.clone())
            .filter(|p| p.determinate());
        if let Some(p) = known {
            let wait = Duration::from_millis(p.settled_after_ms.unwrap_or(0));
            tokio::time::sleep(wait).await;
            self.close_bracket_pair(&account).await;
            self.edge
                .lock()
                .unwrap()
                .entry(account)
                .or_default()
                .ladder_running = false;
            return;
        }

        let mut rungs: Vec<gauge::SettleReading> = Vec::new();
        for offset in self.settle_ladder.iter().copied() {
            let due = started + offset;
            let now = std::time::Instant::now();
            if due > now {
                tokio::time::sleep(due - now).await;
            }
            // Real work reopened the account: abandon the ladder. A reading taken
            // inside a window measures nothing, and real work never waits on
            // calibration (rank 6).
            if self.account_in_flight(&account) {
                tracing::debug!(%account, "settle ladder abandoned — the account is busy again");
                break;
            }
            {
                let m = self.edge.lock().unwrap();
                match m.get(&account) {
                    Some(e) if e.ladder_running => {}
                    // `begin_window` cancelled us.
                    _ => break,
                }
            }
            // The SESSION axis is the rung we read: it is the fastest-moving of
            // the three, so settle lag shows up there first.
            let Some(pct) = self.read_axis_pct(&account, Axis::Session).await else {
                tracing::debug!(%account, "settle ladder read returned nothing");
                break;
            };
            rungs.push(gauge::SettleReading {
                offset_ms: offset.as_millis() as u64,
                pct,
            });
            {
                let mut m = self.edge.lock().unwrap();
                m.entry(account.clone()).or_default().rungs = rungs.clone();
            }
            // Two consecutive rungs agreeing IS the settle: stop paying for
            // readings that can only confirm it.
            if rungs.len() >= 2 && (rungs[rungs.len() - 1].pct - rungs[rungs.len() - 2].pct).abs()
                <= f64::EPSILON
            {
                break;
            }
        }

        let profile = gauge::settle_profile(&rungs);
        if !profile.rungs.is_empty() {
            tracing::info!(
                %account,
                settle = %profile.describe(),
                "edge poll settle ladder"
            );
        }
        {
            let mut m = self.edge.lock().unwrap();
            let e = m.entry(account.clone()).or_default();
            if !profile.rungs.is_empty() {
                e.profile = Some(profile.clone());
            }
        }

        // Phase 2: the recorded closing pair, and only from a settled reading.
        if profile.determinate() {
            self.close_bracket_pair(&account).await;
        } else if !profile.rungs.is_empty() {
            tracing::warn!(
                %account,
                settle = %profile.describe(),
                "the meter had not settled by the last ladder rung, so NO closing poll was \
                 recorded — this window stays unbracketed rather than calibrating from a \
                 reading that is still moving (lengthen `settle_ladder_secs`)"
            );
        }

        let mut m = self.edge.lock().unwrap();
        m.entry(account).or_default().ladder_running = false;
    }

    /// The recorded CLOSING PAIR: two `/usage` observations a few seconds apart
    /// in the trailing idle gap. The first is the `after` bracket; the two
    /// together are the `IdleMovement` witness that the account really was quiet
    /// across the gap. Abandoned the moment real work reopens the account.
    async fn close_bracket_pair(&self, account: &str) {
        for i in 0..2 {
            if i > 0 {
                tokio::time::sleep(self.tunables.edge_pair_gap).await;
            }
            if self.account_in_flight(account) {
                return;
            }
            if let Err(e) = self.probe_account(account).await {
                tracing::debug!(%account, reason = %e, "closing edge poll recorded nothing");
                return;
            }
        }
    }

    /// READ one axis of the meter without recording anything.
    ///
    /// Distinct from [`Self::probe_account`] on purpose: this is the settle
    /// ladder's measurement instrument, and a measurement that is still moving
    /// must not become bracket material. Same free `/usage` invocation, no
    /// observation row, no calibration evaluation, no side effect at all.
    async fn read_axis_pct(&self, account: &str, want: Axis) -> Option<f64> {
        let cd = self.config_dir_for(account);
        let probe = self.probe.clone();
        let rows = tokio::task::spawn_blocking(move || probe.probe(cd.as_deref()))
            .await
            .ok()
            .flatten()?;
        rows.into_iter()
            .find(|(pool, _, _)| Axis::from_pool_label(pool) == Some(want))
            .map(|(_, pct, _)| pct)
    }

    /// What the last completed settle ladder on `account` measured. `None` until
    /// one has run. This is the answer to "how long after a turn lands does the
    /// meter finish moving?" — measured, not assumed.
    pub fn settle_profile(&self, account: &str) -> Option<gauge::SettleProfile> {
        self.edge.lock().unwrap().get(account)?.profile.clone()
    }

    /// How many `/usage` polls sit in this account's current idle gap.
    pub fn polls_in_idle_gap(&self, account: &str) -> usize {
        self.edge
            .lock()
            .unwrap()
            .get(account)
            .map(|e| e.polls_since_idle)
            .unwrap_or(0)
    }

    // ── event emission ───────────────────────────────────────────────────

    fn alloc_message_id(&self, state: &ThreadState) -> Result<u64> {
        let id = state.next_message_id.fetch_add(1, Ordering::SeqCst);
        self.store.update_thread_counters(&state.id, id + 1)?;
        Ok(id)
    }

    /// Persist-then-broadcast (the CCW invariant, kept): bump seq, append to
    /// the ledger, advance the row's watermark, push to subscribers.
    ///
    /// It used to write a STATUS word here too, and that write is where the
    /// twenty-five-hour `busy` came from: on the terminal path the sequence is
    /// `clear_running_turn` → `parent_running = false` → `emit_liveness` →
    /// `emit(TurnEnded)`, and all of it runs while the `TurnLock` is still held
    /// and `turn_running` is still true — so the status computed here read the
    /// still-true atomic and persisted `busy` on a turn that had ENDED.
    /// `finish_turn`'s compensating write was skipped entirely on the
    /// `still_busy` branch, and a panicking turn wrote nothing at all.
    ///
    /// The write-ordering bug is not fixed here; it is GONE, because there is
    /// nothing left to write. The answer is derived when it is asked, from rows
    /// (`thread_locks`) that `TurnLock::release` and `TurnLock::drop` both
    /// clear — which is also why the panic path is correct now for free.
    fn emit(&self, state: &ThreadState, message_id: Option<u64>, event: ThreadEvent) {
        self.emit_resolving(
            state,
            message_id,
            event,
            crate::shape::Resolving::none(),
        );
    }

    /// [`Self::emit`] with promise ids routed onto the frames this event causes.
    ///
    /// **The ledger envelope is a RESOLVER** (rule 2.4, 2026-09-28), because
    /// `ThreadCells::last_message_id`, `newest` and `app` are folded out of
    /// `Input::Push(Push::Event(_))` and out of nothing else. Giving those cells
    /// a `Fact` twin so a promise had something to hang on would be a second
    /// writer created for the promise's convenience, which is the collapse rule
    /// 2.4 exists to prevent wearing 2.4's own clothes.
    fn emit_resolving(
        &self,
        state: &ThreadState,
        message_id: Option<u64>,
        event: ThreadEvent,
        promises: crate::shape::Resolving,
    ) {
        let seq = state.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let ev = StoredEvent {
            thread_id: state.id.clone(),
            seq,
            message_id,
            ts: crate::store::now_rfc3339(),
            event,
        };
        if let Err(e) = self.store.append_event(&ev) {
            tracing::warn!(error = %e, "failed to persist event");
        }
        // **THE RECENCY STAMP RIDES THE SAME WRITE AS THE WATERMARK**, and it is
        // derived from the event this call is already holding rather than from a
        // flag a caller passes. `ThreadEvent::is_user_message` is the one
        // classifier the `threads/recent-updates` fold uses too, so the column
        // and the live order cannot be fed by two readings of "he spoke" — see
        // `Store::bump_last_seq`.
        let user_input_at = ev.event.is_user_message().then(|| ev.ts.clone());
        let _ = self
            .store
            .bump_last_seq(&state.id, seq, user_input_at.as_deref());
        state.turn_terminal.lock().unwrap().observe(&ev.event);
        self.note_activity(&state.id, &ev.ts);
        self.hub
            .emit_push_resolving(crate::clock::now(), promises, Push::Event(ev));
    }

    /// Broadcast a liveness beat — transient only, NEVER persisted (G6).
    ///
    /// It persisted a status word until this change, which made "never
    /// persisted" false for the one field that mattered. It is true again: this
    /// function now touches no table at all.
    fn emit_liveness(&self, state: &ThreadState) {
        let (busy, parent, running) = {
            let l = state.live.lock().unwrap();
            (l.busy(), l.parent_running, l.running())
        };
        // **THE BEAT NO LONGER RESOLVES A PLACEMENT (S3).** It read the
        // thread's own `state.account` under a lock and put it on every frame,
        // at ~1 Hz per busy thread, for one consumer that no longer exists —
        // the account board's `live.py::_note_account`. The board learns
        // placement from the `accounts` shape now, where a thread's row lives
        // under the account it was placed on, and the shape folds `Status`
        // (the EDGE at which a placement is decided) rather than this beat.
        // See `event::Push::Liveness` for the consumer sweep that preceded the
        // removal, and `emit_status`, which still resolves it and must.
        self.hub.emit(Push::Liveness {
            thread_id: state.id.clone(),
            busy,
            parent_running: parent,
            running_subagents: running,
        });
    }

    // ── the turn runner ──────────────────────────────────────────────────

    /// Run one delivery to completion (retries included).
    ///
    /// `lock` is taken BY VALUE and never optional: a caller that has not
    /// claimed the thread cannot call this function at all. It is released in
    /// `finish_turn`, before the queue is re-dispatched.
    async fn run_turn(
        &self,
        state: Arc<ThreadState>,
        prompt: String,
        message_ids: Vec<u64>,
        attempt: u32,
        lock: TurnLock,
    ) {
        let mut attempt = attempt;
        // ── HOW MANY TIMES THIS DELIVERY HAS RESTARTED FRESH ────────────────
        //
        // A LOCAL, and that is load-bearing. The obvious place to count this is
        // `state.resumable`, and doing so is WRONG in a way that is invisible
        // until it burns: `run_once` sets `resumable = true` after every spawn
        // it makes, unconditionally, whatever the exit code (see the "first
        // successful spawn" comment there, which is what the code MEANS but not
        // what it CHECKS). So a flag-based "have we already restarted fresh?"
        // is cleared by the very next attempt and answers "no" forever.
        //
        // Measured, when this was a flag: a thread whose CLI reports "no
        // conversation found" every time restarted fresh **1283 times** in about
        // thirty seconds — an unbounded wipe loop with no backoff at all, which
        // is precisely the token furnace this wave exists to prevent. A local
        // counter cannot be reset by anything outside this loop, so the bound
        // holds by construction rather than by the good behaviour of a flag
        // three thousand lines away.
        let mut fresh_starts: u32 = 0;
        // ── PL-3: THE PROTOLEDGER SHADOW, BEFORE THE SPAWN ──────────────────
        //
        // Nothing at all unless the daemon started with `THREADS_PL_SHADOW=1`.
        // Never fatal: `run_before` logs and counts its own failures, and
        // nothing below reads what it did. `events` stays the truth.
        let pl_mark = self.pl_shadow_before(&state, &prompt, &message_ids).await;
        loop {
            // Per-turn budget bookkeeping resets with the turn.
            state.turn_charged.lock().unwrap().clear();
            state.turn_cost_charged.lock().unwrap().clear();
            state.charged_message_ids.lock().unwrap().clear();
            // …and so does the CUMULATIVE-USAGE baseline: a new attempt is a
            // new claude process whose `modelUsage` counters start at zero
            // again, and diffing against the previous process's high-water mark
            // would swallow this turn's spend entirely. The epoch check in
            // `record_turn_usage` already covers the normal case; this covers
            // the one it cannot — two consecutive executions with no
            // running-turn row to distinguish them. Not done on the re-adopt
            // path (`adopt_turn`), which resumes an execution rather than
            // starting one, and needs its baseline intact.
            let _ = self.store.clear_turn_usage_watermarks(&state.id);
            state.budgets_failure_reported.store(false, Ordering::SeqCst);
            state.budget_kill.store(false, Ordering::SeqCst);
            *state.kill_trip.lock().unwrap() = None;
            // A LEVEL FLAG MUST NOT OUTLIVE THE TURN IT WAS RAISED FOR. The
            // account picture is re-asked by `admit_or_park` on the way in, so
            // a flag raised against the previous attempt's account would tear
            // down an attempt that has already been placed somewhere healthy —
            // once per attempt, forever.
            state.limit_kill.store(false, Ordering::SeqCst);
            *state.limit_kill_signal.lock().unwrap() = None;
            // ── THE DELIVERY'S CLOCK: STARTED ONCE, NEVER BY A RETRY ─────────
            //
            // Idempotent for the same delivery, so a retry, a resumed park and
            // a re-fired retry after a daemon restart all keep the FIRST
            // attempt's `turn_started_at` and the running failure streak. See
            // `delivery_clocks` in schema.sql and an internal thread.
            if let Err(e) = self.store.begin_delivery_attempt(&state.id, &message_ids) {
                tracing::warn!(thread = %state.id, error = %e, "recording the delivery clock failed");
            }
            self.emit(
                &state,
                None,
                ThreadEvent::TurnStarted {
                    attempt,
                    message_ids: message_ids.clone(),
                },
            );

            // Where the conversation is, and how long it is, BEFORE this
            // attempt writes to it — see `rollback_failed_attempt`.
            let tx_mark = self.transcript_mark(&state);

            let mut outcome = self.run_once(&state, &prompt, &message_ids, attempt).await;

            // ── THE MID-TURN INTERRUPT LANDS IN THE 429 BRANCH ──
            //
            // Not in a branch of its own, and above all not in
            // `park_killed_turn`. A usage-limit interrupt and a usage-limit 429
            // are the same event observed at two different moments, so they get
            // the same handling: record the bar where placement reads it, ask
            // the ONE admission authority, flag the thread (DEC-56), and either
            // continue on the account the balancer chose or let the retry
            // ladder own the wait. Synthesizing the decision here is what makes
            // that reuse literal — there is exactly one copy of the 429 logic
            // in this function, and it is the copy that was already there.
            //
            // No `decision.is_none()` conjunct (R9.L6d, deleted): it was a
            // condition that could not fail. `run_once`'s ONLY `limit_killed`
            // return carries `decision: None` by construction (THE
            // INTERRUPT'S FACTS OUTRANK THE STREAM'S, there), so a streamed
            // 429 before the SIGTERM never reaches this line as a decision.
            // Its reset and pool already ride on `outcome`.
            if outcome.limit_killed {
                outcome.decision = Some(RetryDecision::Retry {
                    kind: TransientKind::RateLimit,
                    reason: "usage limit (mid-turn interrupt)".into(),
                });
            }

            // ── THE SPAWN GUARD REFUSED A LOGIN: NOTHING RAN ────────────────
            //
            // First, above every other arm, because it is the only outcome here
            // that describes a turn which never reached `cmd.spawn()`. There is
            // no child to classify, no exit code to ladder and no work to
            // preserve beyond the prompt — which `refuse_unresolved_login` has
            // already put in a durable `parked_turns` row, along with the park,
            // the `error!` and the ledger event.
            //
            // A `break` and NOT a ladder, deliberately. Retrying would re-ask a
            // question whose answer is a config fact (this account is not in
            // the pool), burn the deterministic ladder against it, and end in
            // `give_up_and_keep_the_prompt` — which would move the queue rows
            // to `failed` while the parked row still holds the same delivery.
            // One delivery, two live copies, and he hears it twice.
            if outcome.login_refused {
                break;
            }
            // The projection refused — a config/data fact about this thread,
            // not a transient. Same `break` as the login refusal above, for the
            // same reason: laddering would re-ask a question with a fixed
            // answer and end in a second live copy of the parked delivery.
            if outcome.projection_refused {
                break;
            }

            if outcome.budget_killed {
                self.park_killed_turn(&state, &prompt, &message_ids, attempt);
                break;
            }
            // ── THE ONE THING THAT ENDS A DELIVERY ───────────────────────────
            //
            // **A user interruption. Nothing else.**
            //
            // `cancelled` is raised by `ThreadManager::cancel` — `threads.cancel`,
            // the operator asking for it — and it is the single trigger for which
            // `retry::ends_the_delivery` answers true. It is checked HERE, before
            // the verdict match, so that the rule reads in execution order: the
            // operator's intention short-circuits everything, and every other
            // outcome below it lands on a ladder.
            //
            // This is also the only `break` in this function not preceded by a
            // recorded failure, and that asymmetry is correct rather than an
            // oversight: he already knows, because he is the one who did it.
            // Every OTHER exit owes him a `TurnFailed` and his prompt back.
            //
            // `clear_retry` here is a plain drop, deliberately. A cancelled turn
            // does not preserve its prompt for re-selection, because the operator
            // withdrawing a delivery is not a delivery that failed to reach him —
            // re-offering what he just cancelled would be the module arguing with
            // him.
            if outcome.cancelled {
                self.store.clear_retry(&state.id).ok();
                self.store.end_delivery(&state.id).ok();
                tracing::info!(
                    thread = %state.id, attempt,
                    "the operator interrupted this delivery — it ENDS here and is not retried"
                );
                break;
            }
            // ── THE SECOND THING THAT ENDS A DELIVERY ────────────────────────
            //
            // **An automatic compaction on a thread that does not resume
            // through one** — his standing instruction, given once and applied
            // whenever it becomes relevant, which is why it sits beside the
            // operator's own interruption rather than below with the gates.
            //
            // Like the arm above it, this is DEFENCE IN DEPTH: `run_once`
            // already short-circuited to `decision: None`, so the success arm
            // below would end the delivery anyway. It is written out regardless
            // for the two things the success arm cannot do — say in the daemon
            // log WHY this delivery stopped, and make the policy visible at the
            // place a reader looks for "what ends a turn".
            //
            // `clear_retry` is the same plain drop the cancel arm does, and for
            // a closely related reason: this delivery did not FAIL, so there is
            // nothing to hold for re-selection. Its prompt was delivered, it was
            // worked on, and the work is on the ledger above the compaction
            // boundary. Re-offering it would replay work already done.
            //
            // What is NOT dropped is anything still queued. Those messages were
            // never claimed by this turn, so they are untouched here and
            // `finish_turn`'s re-drain sends them out on the next turn — which
            // is the turn that receives the post-compaction restatement.
            if outcome.compaction_stopped {
                self.store.clear_retry(&state.id).ok();
                self.store.end_delivery(&state.id).ok();
                tracing::info!(
                    thread = %state.id, attempt,
                    "an automatic compaction ended this delivery (resume_after_compaction=false) \
                     — it ENDS here and is not retried; queued messages are untouched"
                );
                break;
            }
            match outcome.decision {
                None => {
                    self.land_clean_verdict(&state);
                    break;
                }
                // Every failure verdict lands on a ladder — see
                // `land_failed_verdict`, shared with `adopt_turn` (L-2).
                Some(decision) => match self.land_failed_verdict(
                    &state,
                    &prompt,
                    &message_ids,
                    attempt,
                    &mut fresh_starts,
                    decision,
                    outcome.reset.as_deref(),
                    outcome.pool.as_deref(),
                    &tx_mark,
                ) {
                    VerdictLanding::RunAgain => {
                        attempt += 1;
                        continue;
                    }
                    VerdictLanding::Done => break,
                },
            }
        }

        // ── PL-3: …AND AFTER IT: carry, follow, compare (never fatal) ────────
        if let Some(seq) = pl_mark {
            self.pl_shadow_after(&state, seq).await;
        }
        self.finish_turn(state, lock).await;
    }

    /// PL-3: the shadow's counters, `None` when the flag is off.
    pub fn pl_shadow_stats(&self) -> Option<crate::pl::shadow::StatsSnapshot> {
        self.pl_shadow.as_ref().map(|s| s.stats())
    }

    /// PL-3: the thread's native transcript as `transcript_mark` finds it.
    fn pl_native_path(&self, state: &Arc<ThreadState>) -> Option<PathBuf> {
        let home = crate::accounts::transcript_home()?;
        let account = state.account.lock().unwrap().clone();
        let store = self.transcript_store_of(&account).ok()?;
        crate::accounts::locate_transcript(store.as_deref(), &home, &state.id)
    }

    /// PL-3 step 1 (`crate::pl::shadow::Shadow::run_before`), off the async
    /// workers. Returns the `events` seq the turn starts after, or `None`
    /// when the shadow is off.
    async fn pl_shadow_before(&self, state: &Arc<ThreadState>, prompt: &str, message_ids: &[u64]) -> Option<u64> {
        let shadow = self.pl_shadow.clone()?;
        let settings = {
            let s = state.settings.lock().unwrap();
            crate::pl::SettingsSnapshot::of(&s, s.append_system_prompt.clone())
        };
        let kinds = vec!["user_message".to_string()];
        let q = crate::store::MessageQuery {
            ids: Some(message_ids),
            from: None,
            to: None,
            tail: None,
            kinds: Some(&kinds),
            cursor: None,
            limit: message_ids.len().max(1),
        };
        let mut raw = Vec::new();
        let mut headers = serde_json::Map::new();
        let mut bare = false;
        if !message_ids.is_empty() {
            for r in self.store.query_messages(&state.id, &q).map(|p| p.events).unwrap_or_default() {
                if let ThreadEvent::UserMessage { text, metadata, .. } = &r.event {
                    raw.push(text.clone());
                    if let Some(h) = metadata.get("headers").and_then(Value::as_object) {
                        headers.extend(h.clone());
                    }
                    bare |= metadata.get("_bare").and_then(Value::as_bool) == Some(true);
                }
            }
        }
        let start = crate::pl::shadow::TurnStart {
            thread_id: state.id.clone(),
            ts: crate::store::now_rfc3339(),
            prompt: crate::pl::Prompt {
                composed: prompt.to_string(),
                raw_text: raw.join("\n\n"),
                message_id: (message_ids.len() == 1).then(|| message_ids[0] as i64),
                queue_id: None,
                headers,
                bare,
                turn: None,
            },
            settings,
            native: self.pl_native_path(state),
        };
        let seq = self.store.max_seq(&state.id).unwrap_or(0);
        let _ = tokio::task::spawn_blocking(move || shadow.run_before(&start)).await;
        Some(seq)
    }

    /// PL-3 step 3 (`crate::pl::shadow::Shadow::run_after`): the turn's
    /// conversation rows in `events` (top level only; a native subagent's
    /// rows are its own agent in the ledger) handed over for the comparison.
    async fn pl_shadow_after(&self, state: &Arc<ThreadState>, after_seq: u64) {
        let Some(shadow) = self.pl_shadow.clone() else { return };
        let kinds: Vec<String> =
            ["assistant_text", "thinking", "tool_use", "tool_result"].iter().map(|k| k.to_string()).collect();
        let mut conv = Vec::new();
        let mut after = after_seq;
        loop {
            let page = self.store.list_events(&state.id, after, 1000, Some(&kinds)).unwrap_or_default();
            let Some(last) = page.last().map(|e| e.seq) else { break };
            for e in page {
                use crate::pl::shadow::Conv;
                match e.event {
                    ThreadEvent::AssistantText { text, parent_tool_use_id: None, .. } if !text.is_empty() => {
                        conv.push(Conv::AssistantText(text))
                    }
                    ThreadEvent::Thinking { text, parent_tool_use_id: None } if !text.is_empty() => {
                        conv.push(Conv::Reasoning(text))
                    }
                    ThreadEvent::ToolUse { id, name, parent_tool_use_id: None, .. } => {
                        conv.push(Conv::ToolCall { id, name })
                    }
                    ThreadEvent::ToolResult { tool_use_id, parent_tool_use_id: None, .. } => {
                        conv.push(Conv::ToolResult { id: tool_use_id })
                    }
                    _ => {}
                }
            }
            after = last;
        }
        let native = self.pl_native_path(state);
        let id = state.id.clone();
        let _ = tokio::task::spawn_blocking(move || shadow.run_after(&id, native.as_deref(), &conv)).await;
    }

    /// **THE SUCCESS VERDICT, landed.** The one place a delivery that ended
    /// with no failure verdict is closed: `run_turn` on a clean ending, and
    /// `adopt_turn` on a re-adopted one (L-2) — so a successful adopted retry
    /// clears the retry row that fired it instead of leaving it to run the
    /// same paid prompt again.
    fn land_clean_verdict(&self, state: &Arc<ThreadState>) {
        // Success: this delivery is done; drop any pending retry.
        self.store.clear_retry(&state.id).ok();
        self.store.end_delivery(&state.id).ok();
        // R12.5c: a completed turn proves the slot's credential authenticates
        // as SOMEONE — not that it is this account's. Every login flag it would
        // lift below goes through R12.10's re-check first; fail keeps.
        if !self.completed_turn_may_lift_flags(state) {
            return;
        }
        // A turn that completed is proof the login works — if it was
        // flagged dead, it is not dead any more.
        self.note_login_health(state, None);
        // …and it is proof the login is not RATE LIMITED, which
        // nothing used to be able to say. A recorded window that a
        // completed turn has just falsified is cleared here rather
        // than left to expire on its own clock.
        self.note_account_served(state);
    }

    /// **R12.5c: MAY THIS COMPLETED TURN LIFT ITS ACCOUNT'S FLAGS?**
    ///
    /// `note_login_health` (revives `dead_since`) and `note_account_served`
    /// (drops the slot from the sign-in set, clears the login bar and every
    /// non-overage window) used to run on every clean ending with no identity
    /// read — and a slot holding b@'s grant under a@'s label completes turns AS
    /// b@, so a@'s flags were lifted on b@'s success (R12.5b inspect, finding 1).
    ///
    /// When the account carries a LOGIN flag (`login_*`, or the login bar
    /// `auth_failed`) this runs [`crate::accounts::recheck_flags`] with
    /// [`crate::accounts::RecheckTrigger::Turn`] — the same trusted read and
    /// slot probe as a login or his enable — and only `verified` lets the
    /// completion's clears run. Anything else (crossed, unreadable, a disabled
    /// account that is not read) keeps every flag, and the re-check's own
    /// `flags_kept` says why. With no login flag nothing is read: the common
    /// clean ending makes no network call.
    fn completed_turn_may_lift_flags(&self, state: &Arc<ThreadState>) -> bool {
        let account = state.account.lock().unwrap().clone();
        let words = self.account_unavailable_reasons(&account).unwrap_or_default();
        if !words.iter().any(|w| w.starts_with("login_") || w == "auth_failed") {
            return true;
        }
        let check =
            crate::accounts::recheck_flags(self, &account, crate::accounts::RecheckTrigger::Turn);
        let verified = check.identity_state == Some(crate::accounts::IdentityState::Verified);
        if !verified {
            tracing::warn!(
                %account,
                identity = ?check.identity_state,
                reason = check.identity_reason.as_deref().or(check.skipped.as_deref()).unwrap_or(""),
                kept = ?check.kept,
                "a turn completed on this account, but the re-check did not verify whose \
                 credential served it — NOTHING was cleared"
            );
        }
        verified
    }

    /// **A FAILURE VERDICT, landed on its ladder** — the verdict-to-outcome
    /// match, shared by `run_turn` and `adopt_turn` (L-2) so a re-adopted
    /// turn's ending is classified exactly as a spawned one's: a transient
    /// goes on its ladder, a credit or login error bars the account and hands
    /// the turn back to admission, a usage limit lands on the usage-limit
    /// ladder. `RunAgain` means "re-run now at `attempt + 1`" (placement moved
    /// the turn to another account, or a fresh restart) — the caller owns the
    /// re-run; `Done` means the outcome has been recorded and the turn ends.
    ///
    /// ── EVERY VERDICT LANDS ON A LADDER ──────────────────────────
    ///
    /// **THE RULE (2026-08-25): a user interruption ends the
    /// delivery. Nothing else does.**
    ///
    /// > *"I want to retry after all events that are not user
    /// > interruptions."*
    ///
    /// This arm used to be three: a `RestartFresh` arm, a `Retry`
    /// arm, and a `Some(other)` catch-all that dropped the delivery
    /// for `Fatal`, `CreditBalance` and an exhausted `RestartFresh`.
    /// Those three verdicts did not end turns because anyone had
    /// decided they should — they ended turns because the catch-all
    /// was where unhandled variants fell. That is the emergent
    /// property the rule replaces.
    ///
    /// The rule now has exactly one home: `retry::ends_the_delivery`
    /// decides what ends a delivery (and answers `true` for
    /// `InterruptTrigger::Operator` alone), and `retry::ladder_for`
    /// is TOTAL — there is no `None` arm to fall through, so a new
    /// `RetryDecision` variant cannot quietly acquire terminal
    /// behaviour by being forgotten here.
    ///
    /// The operator interruption itself is handled by the callers, at
    /// `outcome.cancelled` (and `adopt_turn`'s `cancelled`), before this is
    /// reached. Every `Done` returned here is preceded by a recorded outcome.
    #[allow(clippy::too_many_arguments)]
    fn land_failed_verdict(
        &self,
        state: &Arc<ThreadState>,
        prompt: &str,
        message_ids: &[u64],
        attempt: u32,
        fresh_starts: &mut u32,
        decision: RetryDecision,
        reset: Option<&str>,
        pool: Option<&str>,
        tx_mark: &Option<(PathBuf, u64)>,
    ) -> VerdictLanding {
        // ── ONE VERDICT CARRIES A RECOVERY, NOT JUST A LADDER ──
        //
        // `RestartFresh` means the CLI could not find the
        // conversation to resume. Starting a new one is the only
        // action that gets his words delivered at all — but it is
        // also destructive in a way no other retry is, so it is
        // decided, and defended, in `may_restart_fresh` rather than
        // folded into the general ladder. When that says no, the
        // verdict falls through and is retried like anything else.
        if let RetryDecision::RestartFresh { reason } = &decision {
            if self.may_restart_fresh(state, attempt, *fresh_starts, reason) {
                *fresh_starts += 1;
                state.resumable.store(false, Ordering::SeqCst);
                return VerdictLanding::RunAgain;
            }
        }
        let (kind, reason) = crate::retry::ladder_for(&decision);
        // ── HOW LONG THIS CLASS OF FAILURE HAS BEEN RUNNING ──────
        //
        // Wall-clock since the first failure of the current
        // same-class run, read off the durable delivery clock. This
        // — not `attempt` — is what the connectivity and
        // server-error budgets are enforced against below.
        let streak_secs = self.failure_streak_secs(&state, &kind);
        // ── A FAILED ATTEMPT LEAVES NOTHING BEHIND ──
        //
        // Done FIRST, while the thread is still on the account
        // whose transcript was marked and before the gate below can
        // port it anywhere, so what moves is already clean.
        self.rollback_failed_attempt(state, tx_mark);
        if kind == TransientKind::RateLimit {
            // ── THE 429 PATH HAS NO SELECTOR OF ITS OWN ──
            //
            // It used to: `try_switch_account` ranked the registry
            // through `accounts::select_failover` and moved the
            // thread itself, which made it a SECOND placement
            // implementation — one that never saw the budget's
            // scope, the pooled trip, the placement deadbands or
            // the round-robin cursor, and that ran the next attempt
            // without passing the admission gate at all.
            //
            // Now it does the two things only it can do, and asks:
            //
            // 1. RECORD THE CAUSE where placement reads it. The
            //    account that just answered 429 is stamped with its
            //    limit window, so `unavailable_accounts` — and
            //    therefore `AdmissionRequest::unavailable` — knows
            //    it is unusable, along with every other label for
            //    the same login.
            self.note_account_limit(
                &state,
                reset,
                pool,
            );
            let before = state.account.lock().unwrap().clone();
            // Kept for the retry row: the login that actually
            // answered 429, not wherever the gate leaves the thread a
            // few lines below. There is no floor to attribute any more,
            // but a starving delivery still has to be able to name what
            // it is piling up behind.
            let floored = before.clone();
            // 2. ASK THE ONE AUTHORITY. `admit_or_park` is the same
            //    gate the queue, the parked-turn redelivery and the
            //    retry timer go through. It places via
            //    `Budgets::place`, it moves a thread with history
            //    only by PORTING its transcript (`move_thread` with
            //    `port_must_succeed`), and it parks when the answer
            //    is no. Nothing here filters candidates or ranks
            //    accounts.
            let admitted = self.admit_or_park(&state, &message_ids);
            let after = state.account.lock().unwrap().clone();
            let switched = after != before;
            // DEC-56: a usage limit IS the hard break. Flag the
            // thread, name the gate, and report whether the gate's
            // answer moved it — `interrupt_thread` is told, it does
            // not look.
            // The QUOTA the stream named, not a guess read out of a
            // string that never carries one.
            let trigger =
                Self::classify_limit_trigger(pool, &reason);
            self.interrupt_thread(
                &state,
                trigger,
                Self::limit_detail(trigger),
                switched,
                Some(before),
                switched.then_some(after),
            );
            if admitted && switched {
                // Placement put this thread somewhere else AND the
                // gate said run. That is the only case that skips
                // the ladder, and it skips nothing else: the gate
                // it would have re-entered is the gate that just
                // answered.
                return VerdictLanding::RunAgain;
            }
            // Otherwise the LADDER owns the wait — including when
            // the gate parked us, because the retry row is the
            // prompt's durable home and `fire_retry` re-asks the
            // same gate (audit F1) and hands a still-refused prompt
            // to `parked_turns`.
            //
            // ── AND THE LADDER NOW HAS A TOP ──
            //
            // This line used to end "Usage limits retry
            // indefinitely", and it was the only ladder in the crate
            // with no ceiling. The exemption was written about
            // WAITING — you should not ration a thread's patience
            // for a weekly reset by a counter — but `attempt` has
            // never measured waiting. It is incremented here, after
            // `run_once` returned, so every unit of it is a real
            // `claude` process that was spawned and refused.
            // "Indefinitely" therefore bought unbounded SPENDING,
            // and on 2026-08-06 it bought 924 spawns on one
            // delivery.
            //
            // Waiting stays unbounded: the floor is still whatever
            // the reset prose says (up to `MAX_PARSED_RESET`), and a
            // retry that sleeps a week and succeeds costs one
            // attempt. Only spending is bounded, and the bound is
            // sized so far above the legitimate case
            // (`DEFAULT_MAX_USAGE_LIMIT_ATTEMPTS`) that reaching it
            // is itself the diagnosis.
            //
            // Three outcomes, and the middle one is the point: a
            // retry that has failed hundreds of times must not be
            // able to reach the ceiling in silence.
            self.land_on_the_usage_limit_ladder(
                &state,
                &prompt,
                &message_ids,
                attempt + 1,
                &kind,
                &reason,
                reset,
                &floored,
            );
            return VerdictLanding::Done;
        }
        // ── CREDIT EXHAUSTION BARS THE ACCOUNT, NOT THE TURN ─────
        //
        // > *"credit limit exhaustion means that that account is no
        // > longer open for placement, but that means that another
        // > account might be open for placement. So it does deserve
        // > a retry."*
        //
        // Structurally this is the 429 path's twin, and deliberately
        // so: both are "THIS account cannot serve, another might",
        // and that shape already has a correct implementation.
        // Record the cause where placement reads it, then ask the
        // ONE admission authority. Nothing here ranks accounts.
        //
        // It is kept as its OWN branch rather than merged into the
        // 429 branch above, because three things there are wrong for
        // credit: `classify_limit_trigger` would name a usage-limit
        // gate that did not fire, `interrupt_thread` would report a
        // quota break to the thread, and the usage-limit ceiling is
        // sized for reset windows that a credit balance does not
        // have. Sharing the two steps that ARE the same, and not the
        // three that are not, is the whole of the reuse.
        if kind == TransientKind::CreditBalance {
            // 1. RECORD THE CAUSE. The account leaves the placement
            //    pool via `accounts.limit_until` — the existing bar,
            //    on every label of the login, exactly like a 429.
            self.note_account_credit_exhausted(&state, &reason);
            let before = state.account.lock().unwrap().clone();
            let floored = before.clone();
            // 2. ASK THE ONE AUTHORITY.
            let admitted = self.admit_or_park(&state, &message_ids);
            let after = state.account.lock().unwrap().clone();
            let switched = after != before;
            if admitted && switched {
                // Money somewhere else. Go, immediately — the wait
                // exists to find another account, and one was found.
                tracing::info!(
                    thread = %state.id, from = %before, to = %after,
                    "credit exhausted on {before}; placement moved this turn to \
                     {after} rather than ending it"
                );
                return VerdictLanding::RunAgain;
            }
            let ceiling = self.max_credit_attempts(&state);
            if attempt < ceiling {
                self.schedule_retry(
                    &state, &prompt, &message_ids, attempt + 1, &kind, &reason,
                    None, Some(&floored),
                );
            } else {
                self.give_up_and_keep_the_prompt(
                    &state,
                    &message_ids,
                    &kind,
                    format!(
                        "every account this turn could reach reported an exhausted \
                         credit balance, across {attempt} attempts, so it was given \
                         up. Top up an account and re-send this message — its text \
                         has been kept. Reason: {reason}"
                    ),
                );
            }
            return VerdictLanding::Done;
        }
        // ── AN EXPIRED LOGIN BARS THE ACCOUNT, NOT THE TURN ──────
        //
        // > *"The threads module just outright refused to place my
        // > thread and just bombed me with a few errors indicating
        // > that my login expired. So we need to fold that in as one
        // > of the failure and retry mechanisms that bars an account
        // > for a while."* — the operator, 2026-08-26
        //
        // The credit branch above is the template and this is its
        // twin, because the two facts have the same SCOPE: "this
        // login cannot serve, another might". What made login expiry
        // worse than credit was not the policy but the absence of
        // one — it had no class, so it inherited whichever generic
        // ladder the door it came through happened to land on, and
        // both doors landed badly. Through stderr it became
        // `Deterministic` (`unclassified error: …`); through the
        // errored-result door — the one the real CLI uses — it
        // became `ErrorDuringExecution`, tagged `"transient"`, with
        // `max_attempts` = 3. That is three spawns in eleven seconds
        // against a credential that will not be valid again until a
        // human acts, no bar, no failover, and the raw credential
        // error read back to him three times.
        //
        // Kept as its OWN branch rather than merged with credit for
        // the same reason credit is kept apart from the 429: the two
        // steps that are the same are shared, and the ones that are
        // not are not. The bar carries a different axis, the ceiling
        // is its own, and the give-up says the one thing only he can
        // do — which credit's message must not say and this one
        // must.
        if kind == TransientKind::LoginExpired {
            // 1. RECORD THE CAUSE, on every label of the login, with
            //    `limit_axis = "login"` so the status surface can say
            //    WHY rather than implying a quota window.
            self.note_account_login_expired(&state, &reason);
            let before = state.account.lock().unwrap().clone();
            let floored = before.clone();
            // 2. ASK THE ONE AUTHORITY. Nothing here ranks accounts.
            let admitted = self.admit_or_park(&state, &message_ids);
            let after = state.account.lock().unwrap().clone();
            let switched = after != before;
            if admitted && switched {
                // A login that still authenticates. Go immediately —
                // the wait exists to find one, and one was found.
                tracing::info!(
                    thread = %state.id, from = %before, to = %after,
                    "login expired on {before}; placement moved this turn to \
                     {after} rather than ending it"
                );
                return VerdictLanding::RunAgain;
            }
            let ceiling = self.max_login_attempts(&state);
            if attempt < ceiling {
                self.schedule_retry(
                    &state, &prompt, &message_ids, attempt + 1, &kind, &reason,
                    None, Some(&floored),
                );
            } else {
                // THE ONE GIVE-UP IN THIS CRATE THAT NAMES A HUMAN
                // ACTION. Every other exhausted ladder ends by
                // quoting the failure, because for every other class
                // the failure is the whole story and there is nothing
                // he can do but re-send. Here there is exactly one
                // thing he can do, and it is the only thing that
                // works — so the message is the ask, and the vendor's
                // credential string is deliberately NOT interpolated
                // into it. He does not need to read "OAuth session
                // expired and could not be refreshed" a seventh time;
                // he needs to know which command to run. The verbatim
                // text is preserved where diagnosis belongs — on the
                // bar's `last_limit_prose` and in the ERROR log line
                // — not read aloud to him mid-turn.
                self.give_up_and_keep_the_prompt(
                    &state,
                    &message_ids,
                    &kind,
                    format!(
                        "every account this turn could reach needs re-authenticating, \
                         across {attempt} attempts, so it was given up. Run `claude \
                         /login` for the barred accounts — `threads accounts status` \
                         names them — then re-send this message; its text has been \
                         kept."
                    ),
                );
            }
            return VerdictLanding::Done;
        }
        // WHICH CEILING APPLIES. A lost network gets its own, far
        // higher one (item 73): `max_attempts` is 3, which on the
        // transient ladder is about seven seconds of patience, and
        // seven seconds is not an outage policy — it is what turned
        // "my wifi dropped" into "the thread is dead and I find out
        // hours later". A connectivity failure must not spend the
        // transient budget, and a transient failure must not spend
        // this one.
        //
        // No failover hint is passed here, and for connectivity
        // that is load-bearing rather than incidental: the network
        // is not account-specific, so moving the thread would spend
        // a placement (and a transcript port) to arrive at the same
        // unreachable socket. It retries where it is.
        // ── DID THIS FAILURE PROVE THE LOGIN IS DEAD? ────────────
        //
        // This call used to live in the terminal `Some(other)` arm
        // that no longer exists, and losing it would have been a
        // silent regression: an OAuth session that cannot be
        // refreshed classifies as `Fatal` (nothing in
        // `classify_message` recognizes it), so the deterministic
        // ladder is now the ONLY path it takes. Without this, a dead
        // login would never again be flagged and would stay in the
        // selection pool forever.
        //
        // It moved INWARD as well as across — it now runs on every
        // deterministic attempt rather than only on the last one, so
        // a dead login leaves the pool on the first refusal instead
        // of after the whole ladder. `is_dead_login_error` is
        // narrow (a rate limit, an overload and an exhausted credit
        // balance all deliberately return false), so this cannot
        // flag a login for a failure that is not the login's fault.
        if kind == TransientKind::Deterministic {
            self.note_login_health(&state, Some(&reason));
        }
        // WHICH CEILING APPLIES. Every class has its own, and no
        // class may spend another's — a connectivity failure must
        // not consume the transient budget, and a deterministic one
        // must not consume either.
        //
        // **Two of them are WALL-CLOCK, not counts (2026-10-14).**
        // Connectivity is bounded by `CONNECTIVITY_BUDGET_SECS` and
        // nothing else; a server error by `MAX_SERVER_ERROR_ATTEMPTS`
        // OR `SERVER_ERROR_BUDGET_SECS`, whichever ends first. A
        // count bounds spawns, not time, and in an internal thread each
        // spawn ran ~3 minutes of real work before failing: forty
        // "connectivity" attempts were 2½ days and $1,212.
        let exhausted = if kind.is_connectivity() {
            crate::retry::connectivity_exhausted(streak_secs)
        } else if kind == TransientKind::ServerError {
            crate::retry::server_error_exhausted(attempt, streak_secs)
        } else {
            attempt >= self.attempt_ceiling(&state, &kind)
        };
        if !exhausted {
            self.schedule_retry(&state, &prompt, &message_ids, attempt + 1, &kind, &reason, None, None);
        } else {
            // NEVER SILENTLY, AND NEVER EMPTY-HANDED. The retry row
            // is cleared and a TurnFailed goes on the ledger, so the
            // thread ends in a state he can SEE and act on — and
            // `finish_turn` below releases the lock, so it reads
            // idle-and-failed rather than "Active" forever. The
            // audit's top-ranked hole is a turn that looks active
            // with nothing behind it; this path must not become a
            // second way to produce one.
            let message = if kind.is_connectivity() {
                // Named as a NETWORK failure, not a generic
                // exhaustion, because the action he can take is
                // different: this one says "check your connection
                // and send again", not "something broke in the
                // service". The minutes are MEASURED now, not summed
                // off the backoff ladder.
                format!(
                    "could not reach the service for {} minutes across {attempt} \
                     attempts, so this turn was given up — its text has been kept \
                     and can be re-sent: {reason}",
                    streak_secs / 60
                )
            } else if kind == TransientKind::ServerError {
                format!(
                    "the API kept answering with a server error ({attempt} attempts \
                     over {} minutes), so this turn was given up rather than re-run \
                     again — its text has been kept and can be re-sent: {reason}",
                    streak_secs / 60
                )
            } else {
                format!(
                    "retries exhausted after {attempt} attempts — this turn's text \
                     has been kept and can be re-sent: {reason}"
                )
            };
            self.give_up_and_keep_the_prompt(&state, &message_ids, &kind, message);
        }
        return VerdictLanding::Done;
    }


    /// **The attempt ceiling for a COUNT-bounded class.** Connectivity and
    /// server errors are wall-clock bounded and never reach here.
    fn attempt_ceiling(&self, state: &Arc<ThreadState>, kind: &TransientKind) -> u32 {
        if kind.is_connectivity() {
            // Unreachable on the retry path (connectivity is time-bounded);
            // answered with the historical sizing rather than a panic.
            crate::retry::MAX_CONNECTIVITY_ATTEMPTS
        } else if *kind == TransientKind::ServerError {
            crate::retry::MAX_SERVER_ERROR_ATTEMPTS
        } else if *kind == TransientKind::Deterministic {
            // The class that had no ladder at all until 2026-08-25,
            // because it used to be terminal. Short and steeply
            // spaced: see `TransientKind::Deterministic`.
            self.max_deterministic_attempts(state)
        } else if matches!(
            kind,
            TransientKind::Overload | TransientKind::ErrorDuringExecution
        ) {
            // **THE OTHER HALF OF THE TRANSIENT FIX (2026-08-27).**
            // A ladder is only as good as the number of rungs you
            // are allowed to climb, and these two were ceilinged at
            // `max_attempts` — three — purely by falling off the end
            // of this chain. Three attempts is seven seconds on the
            // old ladder and about twenty on the new one; neither is
            // a policy for an overloaded service.
            //
            // Its own constant, because no class may spend another's
            // budget: the same rule that keeps a connectivity
            // failure off the transient ceiling keeps a transient
            // failure off the connectivity one.
            crate::retry::DEFAULT_MAX_TRANSIENT_ATTEMPTS
        } else {
            self.max_attempts(state)
        }
    }

    /// **Seconds since the current run of `kind` failures began**, recording
    /// this failure on the delivery's durable clock as it goes.
    ///
    /// The run survives retries, parks and daemon restarts (it lives in
    /// `delivery_clocks`, not on the retry row), and a failure of a different
    /// class starts a new run — so no class spends another's time budget. A
    /// store error answers ZERO, which fails in the patient direction: the
    /// budget is not spent by a read that did not happen.
    fn failure_streak_secs(&self, state: &Arc<ThreadState>, kind: &TransientKind) -> u64 {
        let since = match self.store.note_delivery_failure(&state.id, kind.kind_str()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(thread = %state.id, error = %e, "recording the failure streak failed");
                return 0;
            }
        };
        match chrono::DateTime::parse_from_rfc3339(&since) {
            Ok(t) => (crate::clock::now() - t.with_timezone(&Utc))
                .num_seconds()
                .max(0) as u64,
            Err(_) => 0,
        }
    }

    /// **The durable landing of a usage limit: the retry ladder, with its top.**
    ///
    /// Extracted from `run_turn`'s 429 branch, unchanged, so that `adopt_turn`
    /// can reach the SAME landing rather than growing a second copy of it. The
    /// duplication that copy would create is the exact hazard the ceiling exists
    /// to bound — a ladder with no top bought 924 spawns on one delivery on
    /// 2026-08-06 — and a second copy is a second place for the top to be
    /// forgotten.
    ///
    /// `next` is the attempt number this landing is scheduling, i.e. already
    /// incremented. `floored` is the account that actually REFUSED — never the
    /// thread, and never wherever the gate has since moved it. It used to be the
    /// subject of a per-thread floor; with the floor gone it is the login a starving
    /// delivery names as what it is piling up behind.
    ///
    /// Three outcomes, and the middle one is the point: a retry that has failed
    /// hundreds of times must not be able to reach the ceiling in silence.
    #[allow(clippy::too_many_arguments)]
    fn land_on_the_usage_limit_ladder(
        &self,
        state: &Arc<ThreadState>,
        prompt: &str,
        message_ids: &[u64],
        next: u32,
        kind: &TransientKind,
        reason: &str,
        reset: Option<&str>,
        floored: &str,
    ) {
        let ceiling = self.max_usage_limit_attempts(state);
        match crate::retry::usage_limit_verdict(next, ceiling) {
            crate::retry::UsageLimitVerdict::GiveUp => {
                self.give_up_on_usage_limit(state, message_ids, kind, reason, next, floored);
            }
            verdict => {
                self.schedule_retry(
                    state,
                    prompt,
                    message_ids,
                    next,
                    kind,
                    reason,
                    reset,
                    Some(floored),
                );
                // AFTER the row is written, deliberately: the escalation reports
                // the floor and the age off the persisted row rather than
                // recomputing them, so there is exactly one source of truth for
                // when this retry fires and how long it has been going.
                if verdict == crate::retry::UsageLimitVerdict::Starving {
                    self.note_retry_starving(state, kind, reason, next, ceiling, floored);
                }
            }
        }
    }

    /// A budget kill ended the execution — PARK the operator's prompt
    /// verbatim so it can be re-delivered the moment the budget allows it.
    ///
    /// This is the exact failure CCW shipped and CON-4 forbids: `admit()`
    /// dropped a blocked turn on the floor. Here the prompt lands in a
    /// durable `parked_turns` row (it survives a daemon restart, like a
    /// retry) and the resume sweep re-dispatches it.
    fn park_killed_turn(
        &self,
        state: &Arc<ThreadState>,
        prompt: &str,
        message_ids: &[u64],
        attempt: u32,
    ) {
        let trip = state.kill_trip.lock().unwrap().clone();
        let budget_key = trip
            .as_ref()
            .map(|t| t.budget_key.clone())
            .or_else(|| state.settings.lock().unwrap().budget_key.clone());
        let reason = trip
            .as_ref()
            .map(Trip::reason)
            .unwrap_or_else(|| "budget exhausted".to_string());
        if let Err(e) = self.store.upsert_parked_turn(&ParkedTurnRow {
            thread_id: state.id.clone(),
            prompt: prompt.to_string(),
            message_ids: message_ids.to_vec(),
            attempt,
            reason: reason.clone(),
            budget_key: budget_key.clone(),
            parked_at: crate::store::now_rfc3339(),
        }) {
            tracing::error!(thread = %state.id, error = %e, "PARKING A KILLED TURN FAILED");
        }
        self.store.clear_retry(&state.id).ok();
        self.note_park(state, "budget", &reason, budget_key, message_ids);
        // DEC-56: a budget kill tore an execution down mid-flight, so this is
        // a HARD break and the thread is owed an account of it. (A
        // `TripAction::Finish` trip is the SOFT break — the turn is allowed to
        // end on its own and nothing is emitted: "we don't even have to notify
        // the thread if there was a soft break.")
        //
        // No switchover happened and none is attempted here: the prompt is in
        // a durable `parked_turns` row and `resume_sweep` re-enters
        // `admit_or_park`, which is the balancer deciding run-now versus wait.
        let trigger = match trip.as_ref().map(|t| t.axis) {
            Some(Axis::WeeklyFable) => InterruptTrigger::FableLimit,
            _ => InterruptTrigger::HardBudget,
        };
        // **THE DETAIL IS SYNTHESIZED, NEVER THE TRIP'S FREE TEXT.**
        //
        // This used to pass `reason` — `Trip::reason`'s own string, e.g.
        // *"budget aui:default exhausted: weekly_all meter at 100% of its 95%
        // limit"* — straight into `ThreadSystemInterrupted::detail`, which is
        // an `Audience::Model` field. That bypassed the synthesize-only
        // discipline every other producer on this path obeys (`limit_detail`),
        // and it carried a budget key, a meter name, a percentage and a
        // threshold into a field a model may read.
        //
        // Nothing is lost by fixing it: the verbatim trip reason is already on
        // the ledger, twice — `note_park(state, "budget", &reason, …)` three
        // lines above, and the `parked_turns` row's own `reason` column. Those
        // are ledger-only and are where the operator reads it.
        //
        // Since the DEC-56 supersession the renderer no longer emits `detail`
        // at all, so this is belt AND braces on purpose: the renderer decides
        // what a model is SHOWN, this decides what a model-audience field is
        // ALLOWED TO HOLD, and a future renderer change must not be able to
        // resurrect a leak that was only ever hidden.
        self.interrupt_thread(
            state,
            trigger,
            Self::limit_detail(trigger),
            false,
            Some(state.account.lock().unwrap().clone()),
            None,
        );
    }

    /// **THE SPAWN GUARD'S REFUSAL: no login, no turn.** Nothing has been
    /// spawned and nothing will be; this parks the thread and says so.
    ///
    /// Reached only from [`Self::spawn_login`]'s `Err` arm — the thread's
    /// account resolved to no login directory and it is not the machine
    /// default, so running it would export nothing and land the turn on
    /// `~/.claude`, the operator's own terminal login.
    ///
    /// # LOUD, and never a substitution
    ///
    /// Modelled on the `/usage` refusal in [`Self::probe_account`], which is
    /// the same door one path over, and it matches its severity deliberately:
    /// that one was found already open and running against his personal login
    /// on a timer, and his answer to the comment that called it intended was
    /// *"I'm really disappointed that you found a comment that said that
    /// because I've been trying to purge this behavior for a long time."* So
    /// this one is not a `warn!` and not a fallback:
    ///
    /// * **`error!` in the daemon log**, naming the thread and the account, so
    ///   a fleet stuck this way is visible without reading the store.
    /// * **A durable `parked_turns` row**, which is what keeps his words. CON-4
    ///   — a blocked turn is parked, never dropped — and `resume_sweep`
    ///   re-dispatches it the moment it can run, which for this park means the
    ///   moment the account is back in the pool (or the operator moves the
    ///   thread himself).
    /// * **`turn_parked` on the ledger and the firehose**, cause `login`, with
    ///   the reason prose, so a client can show WHY a thread went quiet instead
    ///   of inferring idleness. A turn that silently succeeded on the wrong
    ///   account is the outcome this whole guard exists to convert into this
    ///   event.
    ///
    /// No account is substituted. A healthy pool account is always available
    /// and picking one would be a silent account move WITHOUT the transcript —
    /// DEC-56 forbids exactly that, and 119 threads were once moved at boot
    /// without theirs. Moving a thread is the admission gate's act, by PORTING,
    /// and this is not the admission gate.
    ///
    /// `interrupt_thread` is NOT called (unlike [`Self::park_killed_turn`]):
    /// nothing was torn down, no work was lost, and the thread is owed no
    /// account of an interruption that did not happen.
    fn refuse_unresolved_login(
        &self,
        state: &Arc<ThreadState>,
        prompt: &str,
        message_ids: &[u64],
        attempt: u32,
        reason: &str,
    ) -> TurnOutcome {
        let account = state.account.lock().unwrap().clone();
        tracing::error!(
            thread = %state.id,
            account = %account,
            attempt,
            %reason,
            "REFUSED TO SPAWN THIS TURN: its account resolves to no login directory, and an \
             unset CLAUDE_CONFIG_DIR would run it against the operator's own terminal login. \
             The turn is parked and his prompt is kept."
        );
        let budget_key = state.settings.lock().unwrap().budget_key.clone();
        if let Err(e) = self.store.upsert_parked_turn(&ParkedTurnRow {
            thread_id: state.id.clone(),
            prompt: prompt.to_string(),
            message_ids: message_ids.to_vec(),
            attempt,
            reason: reason.to_string(),
            budget_key: budget_key.clone(),
            parked_at: crate::store::now_rfc3339(),
        }) {
            tracing::error!(
                thread = %state.id, error = %e,
                "PARKING A LOGIN-REFUSED TURN FAILED — the turn is still refused, but his \
                 prompt may not be re-deliverable from the parked row"
            );
        }
        self.store.clear_retry(&state.id).ok();
        self.note_park(state, "login", reason, budget_key, message_ids);
        // R4.S4b (HOLDS R17): the same cause gate the projection refusal
        // arms, keyed on what THIS refusal depends on. Without it a thread that
        // reaches the spawn guard past the gate (`budgeting_off`, an ungated
        // budgets error) is redelivered by its own `finish_turn` and refused
        // again, forever. See `ThreadState::projection_hold`.
        *state.projection_hold.lock().unwrap() =
            Some((self.login_fingerprint(state), std::time::Instant::now()));
        TurnOutcome {
            decision: None,
            reset: None,
            pool: None,
            cancelled: false,
            budget_killed: false,
            limit_killed: false,
            compaction_stopped: false,
            login_refused: true,
            projection_refused: false,
        }
    }

    /// What a LOGIN refusal depends on (R4.S4b, HOLDS R17): the account, the
    /// directory cached at placement, the pool's directory for that name and
    /// whether the pool carries it — every input `spawn_login` is asked with.
    /// Prefixed `login|` so the hold knows which fingerprint to recompute.
    fn login_fingerprint(&self, state: &Arc<ThreadState>) -> String {
        let account = state.account.lock().unwrap().clone();
        let cached = state.config_dir.lock().unwrap().clone();
        let pooled = self.config_dir_for(&account);
        let member = self.pool().contains(&account);
        format!("login|{account}|{cached:?}|{pooled:?}|{member}")
    }

    /// Recompute the fingerprint of whichever refusal armed the hold.
    fn hold_fingerprint(&self, state: &Arc<ThreadState>, held: &str) -> String {
        if held.starts_with("login|") {
            self.login_fingerprint(state)
        } else {
            self.projection_fingerprint(state)
        }
    }

    /// What a projection refusal depends on, as one comparable string: the
    /// switch, the allowed losses, the account (placement moves the native
    /// path), and the ledger file's length and mtime (a cutover writes it).
    fn projection_fingerprint(&self, state: &Arc<ThreadState>) -> String {
        let (projection, allow) = {
            let s = state.settings.lock().unwrap();
            (s.projection.is_projected(), s.projection_allow_loss.clone())
        };
        let account = state.account.lock().unwrap().clone();
        let ledger = std::fs::metadata(crate::ledger::turn::ledger_path(&self.data_dir, &state.id))
            .ok()
            .map(|m| (m.len(), m.modified().ok()));
        format!("{projection}|{allow:?}|{account}|{ledger:?}")
    }

    /// **THE PROJECTED TURN COULD NOT BEGIN** — park it, cause `projection`,
    /// prompt kept. The twin of [`Self::refuse_unresolved_login`]: nothing was
    /// spawned, so there is nothing to classify and nothing to ladder; the
    /// typed `ProjectionRefused` is already rendered into `reason`, which names
    /// the loss, the path, or the native conversation the render would have
    /// overwritten.
    fn refuse_projection(
        &self,
        state: &Arc<ThreadState>,
        prompt: &str,
        message_ids: &[u64],
        attempt: u32,
        reason: &str,
    ) -> TurnOutcome {
        tracing::error!(
            thread = %state.id,
            attempt,
            %reason,
            "REFUSED TO SPAWN THIS PROJECTED TURN: the ledger could not be rendered into the \
             native file. The turn is parked and his prompt is kept."
        );
        let budget_key = state.settings.lock().unwrap().budget_key.clone();
        if let Err(e) = self.store.upsert_parked_turn(&ParkedTurnRow {
            thread_id: state.id.clone(),
            prompt: prompt.to_string(),
            message_ids: message_ids.to_vec(),
            attempt,
            reason: reason.to_string(),
            budget_key: budget_key.clone(),
            parked_at: crate::store::now_rfc3339(),
        }) {
            tracing::error!(
                thread = %state.id, error = %e,
                "PARKING A PROJECTION-REFUSED TURN FAILED — the turn is still refused, but his \
                 prompt may not be re-deliverable from the parked row"
            );
        }
        self.store.clear_retry(&state.id).ok();
        self.note_park(state, "projection", reason, budget_key, message_ids);
        *state.projection_hold.lock().unwrap() =
            Some((self.projection_fingerprint(state), std::time::Instant::now()));
        TurnOutcome {
            decision: None,
            reset: None,
            pool: None,
            cancelled: false,
            budget_killed: false,
            limit_killed: false,
            compaction_stopped: false,
            login_refused: false,
            projection_refused: true,
        }
    }

    /// Common turn epilogue: release the slot, keep polling detached
    /// subagents, and drain the queue.
    async fn finish_turn(&self, state: Arc<ThreadState>, lock: TurnLock) {
        // THIS TURN'S TERMINAL RECORD, taken before anything below can start
        // the next turn (whose events must fold into a fresh one). H179.
        let terminal = std::mem::take(&mut *state.turn_terminal.lock().unwrap());
        // FREE THE THREAD FIRST. Everything below can dispatch the next turn,
        // and the next turn has to be able to take the lock we are holding.
        // `release` clears the durable row and then `turn_running`, and `Drop`
        // at the end of this scope is a no-op after it.
        lock.release();
        // R4.S2: THE SUBAGENT POLLER THAT STOOD HERE IS DELETED. It spawned
        // `poll_subagents_until_idle` — a `Liveness::busy()` loop over an mtime
        // sweep — whenever a native agent was still listed. A native agent
        // cannot outlive the process it runs in, and that process has exited
        // by now (`process_exited`); a MANAGED child is a thread of its own,
        // whose end reaches this parent through `managed::turn_finished` below,
        // on the child's own epilogue. Nothing is left to poll.
        //
        // Immediate-intent messages (and turn_end ones if fully idle) go now.
        self.maybe_dispatch(&state);
        // ── A TURN COMPLETING IS A PLACEMENT EVENT, FOR THE WHOLE POOL (H179) ──
        //
        // The operator: *"parking is a pool-level state, not an account one …
        // any placement event (… a turn completing, etc.) triggers re-placing ALL
        // parked threads, each onto whichever account admission picks."* The line
        // above re-asks only THIS thread's queue; every other parked turn used to
        // wait for some other bell. Wake-only (the turn's end is already on the
        // status stream), and coalesced by `placement_wake`'s single permit, so a
        // burst of completions costs one pass. It is not the live loop the
        // "a park does not ring" rule guards against: a turn ENDING is real
        // progress, never the gate's own refusal echoing back.
        self.placement_wake("turn_completed");
        // If this turn ended a MANAGED CHILD's run, report it to the parent's
        // ledger — and wake the parent when it was the last one owed. After
        // `maybe_dispatch`, so a child with more queued work is not reported.
        crate::managed::turn_finished(self, &state.id, terminal);
        // The CLOSING edge (§3.2): if that was the last of our work on this
        // account, walk the settle ladder in its trailing idle gap. Checked AFTER
        // `maybe_dispatch`, so a queue that immediately re-dispatches never
        // starts a ladder it would only have to abandon.
        let account = state.account.lock().unwrap().clone();
        if !self.account_in_flight(&account) {
            self.edge_poll_close(&account);
        }
    }

    fn max_attempts(&self, state: &ThreadState) -> u32 {
        state.settings.lock().unwrap().retry_policy.max_attempts
    }

    fn max_usage_limit_attempts(&self, state: &ThreadState) -> u32 {
        state
            .settings
            .lock()
            .unwrap()
            .retry_policy
            .max_usage_limit_attempts
    }

    fn max_deterministic_attempts(&self, state: &ThreadState) -> u32 {
        state
            .settings
            .lock()
            .unwrap()
            .retry_policy
            .max_deterministic_attempts
    }

    fn max_credit_attempts(&self, state: &ThreadState) -> u32 {
        state.settings.lock().unwrap().retry_policy.max_credit_attempts
    }

    fn max_login_attempts(&self, state: &ThreadState) -> u32 {
        state.settings.lock().unwrap().retry_policy.max_login_attempts
    }

    /// **THE END OF EVERY LADDER — and the one thing that must survive it.**
    ///
    /// > *"I would rather have the entire threads module burned to the ground
    /// > than violate this principle"* — his voice input is never lost and never
    /// > silently fails to reach the thread.
    ///
    /// Every give-up in this crate goes through here, so that "the prompt
    /// survives" is a property of the EXIT rather than something three separate
    /// call sites each have to remember.
    ///
    /// The order is deliberate and is the whole of the correctness argument:
    ///
    /// 1. **`fail_deliveries` FIRST.** It moves the queue rows from `delivered`
    ///    to `failed`, which is what makes his words re-selectable. It must
    ///    happen before `clear_retry`, because `clear_retry` is a plain `DELETE`
    ///    that destroys the retry row — and until these two lines existed in this
    ///    order, that `DELETE` was the last copy of the prompt any dispatcher
    ///    could read. `queue.text` is untouched by delivery and by failure alike,
    ///    so what is preserved is verbatim what he said, not a reconstruction.
    /// 2. **`clear_retry` second**, which is what makes the give-up VISIBLE:
    ///    `thread_status` derives `retrying` purely from the existence of that
    ///    row, so the thread leaves `retrying` here and nowhere else.
    /// 3. **`TurnFailed` last**, on the ledger, which is what AUI hears.
    ///
    /// A failure to preserve is logged at ERROR and does NOT abort the give-up:
    /// a thread stuck in `retrying` forever would be a worse outcome than a lost
    /// re-selection, and the `TurnFailed` still names what happened either way.
    fn give_up_and_keep_the_prompt(
        &self,
        state: &Arc<ThreadState>,
        message_ids: &[u64],
        kind: &TransientKind,
        message: String,
    ) {
        match self.store.fail_deliveries(&state.id, message_ids) {
            Ok(kept) => tracing::warn!(
                thread = %state.id, kept, ?message_ids,
                "retry ladder exhausted — {kept} of this delivery's messages were moved to \
                 `failed` and can be re-sent verbatim; nothing he said has been dropped"
            ),
            Err(e) => tracing::error!(
                thread = %state.id, error = %e, ?message_ids,
                "PRESERVING A FAILED DELIVERY FAILED — the turn is still being failed visibly, \
                 but these messages may not be re-selectable from the queue"
            ),
        }
        self.store.clear_retry(&state.id).ok();
        // The delivery is over, so is its clock.
        self.store.end_delivery(&state.id).ok();
        self.emit(
            state,
            self.alloc_message_id(state).ok(),
            ThreadEvent::TurnFailed {
                error_kind: kind.kind_str().into(),
                message,
            },
        );
    }

    /// **May this delivery recover by starting a NEW conversation?**
    ///
    /// `RestartFresh` means the CLI reported *"no conversation found"* for the
    /// session it was asked to resume. The obvious response — start fresh and
    /// re-send — is what the code did unconditionally, and it deserves its own
    /// thought rather than being folded into the general retry, because it is
    /// the only "retry" in this crate that can DESTROY something.
    ///
    /// # Why it is dangerous
    ///
    /// A fresh start does not mint a new id. `spawn.rs::build_args` passes
    /// `--session-id <thread_id>` on a first turn and `--resume <thread_id>`
    /// otherwise — **the same id either way.** So "restart fresh" means: open a
    /// brand-new conversation at the exact id the old one occupies. If real
    /// history exists there, it is orphaned under an id that now points at
    /// something else, silently, with the operator told nothing.
    ///
    /// # Why it cannot simply be refused
    ///
    /// Because refusing it loses his words, and that is the one thing this module
    /// may not do. If the conversation genuinely is gone, a fresh start is the
    /// ONLY action that delivers what he said. Declining to restart would end the
    /// turn — which is exactly the emergent terminal behaviour this wave exists
    /// to remove.
    ///
    /// # The decision
    ///
    /// Restart fresh **only when there is provably nothing to lose, and never
    /// more than once per delivery.**
    ///
    /// * **Nothing to lose** is checked, not assumed: `transcript_mark` locates
    ///   this thread's transcript in the CURRENT account's store and reports its
    ///   length. No file, or an empty one, means the CLI told the truth and there
    ///   is no history to strand. A non-empty transcript means "no conversation
    ///   found" is misdirection — most plausibly the conversation lives in
    ///   ANOTHER account's store after a port, which is a real and reachable
    ///   state in this crate — and wiping on that reading would create two
    ///   divergent conversations under one id. The turn is retried instead, on
    ///   the deterministic ladder, where a genuine transient resume failure gets
    ///   several more chances and a real problem ends visibly with the prompt
    ///   intact.
    /// * **Once** because a SECOND `RestartFresh` in the same delivery cannot
    ///   mean "the session expired" — we just made that session. It means
    ///   something structural is wrong (the wrong config dir, an unwritable
    ///   store), and repeating the wipe buys nothing while risking it again.
    ///
    /// Never silent, in either direction: both the restart and the refusal are
    /// logged at WARN naming the transcript and its size.
    fn may_restart_fresh(
        &self,
        state: &Arc<ThreadState>,
        attempt: u32,
        fresh_starts: u32,
        reason: &str,
    ) -> bool {
        // ONCE PER DELIVERY. Counted by `run_turn`'s own local, never by
        // `state.resumable` — see the comment on `fresh_starts`. A flag here
        // bought an unbounded wipe loop (1283 restarts in thirty seconds),
        // because `run_once` re-raises `resumable` after every spawn it makes,
        // successful or not.
        if fresh_starts > 0 {
            tracing::warn!(
                thread = %state.id, attempt, fresh_starts, %reason,
                "REFUSING a second fresh start on this delivery: the conversation this attempt \
                 could not find is the one the previous attempt just created, so this is not an \
                 expired session — it is a configuration that cannot hold a conversation. \
                 Retrying on the deterministic ladder instead of wiping again"
            );
            return false;
        }
        // NOTHING TO LOSE — checked, not assumed.
        match self.transcript_mark(state) {
            Some((path, len)) if len > 0 => {
                tracing::warn!(
                    thread = %state.id, attempt, %reason, transcript = %path.display(), bytes = len,
                    "REFUSING to restart fresh: the CLI reported no conversation to resume, but a \
                     non-empty transcript for this thread exists. A fresh start re-mints the SAME \
                     session id, so it would strand that history under an id pointing at a new \
                     conversation. Retrying instead"
                );
                false
            }
            other => {
                tracing::warn!(
                    thread = %state.id, attempt, %reason,
                    transcript = ?other.map(|(p, _)| p.display().to_string()),
                    "restarting fresh: no conversation to resume and no transcript to lose, so a \
                     new conversation is opened at this thread's id and the prompt is re-sent"
                );
                true
            }
        }
    }

    /// **An account reported an exhausted credit balance: bar the LOGIN.**
    ///
    /// The sibling of [`Self::note_account_limit_for`], and deliberately the same
    /// mechanism rather than a parallel one — it writes `accounts.limit_until`
    /// through the same `set_account_limit`, on every label of the login, stamped
    /// with the identity it was observed on. That column is the crate's ONE bar:
    /// `AccountState::available` reads it, `unavailable_accounts` widens it
    /// across aliases, and `AdmissionRequest::unavailable` is how placement sees
    /// it. Nothing new is invented and no third exclusion set is added.
    ///
    /// # What could NOT be reused, and why
    ///
    /// `note_account_limit_for` derives its instant by PARSING reset prose and
    /// refuses to write when there is none — correctly, since writing `None`
    /// through would clear a live window. A credit exhaustion carries no reset
    /// prose and has no natural reset time at all: it ends when a human tops the
    /// account up, which the daemon cannot observe. So the instant is computed
    /// here instead ([`crate::retry::CREDIT_BAR_SECS`]), and it is honestly a
    /// RE-PROBE INTERVAL rather than a prediction.
    ///
    /// That makes the recovery story sound rather than lucky: the bar expires,
    /// one turn is allowed through to find out, and if it succeeds
    /// `note_account_served` clears the window on every label at once. The
    /// account's own success is what proves the account is back — the same
    /// argument the connectivity ladder makes for a short ceiling over a
    /// reachability probe.
    ///
    /// `limit_axis` is recorded as `credit` so `accounts status` says WHY this
    /// login is barred rather than implying a quota window that does not exist.
    ///
    /// # It deliberately does NOT raise the mid-turn kill
    ///
    /// `note_account_limit_for` ends by calling `raise_limit_kill`, tearing down
    /// every turn running on the barred login. This does not, for two reasons.
    /// The signal it would raise is `LimitKill`, which `run_turn` synthesizes
    /// into a `RateLimit` verdict — so every torn-down turn would be mislabelled
    /// as a usage limit and would spend the usage-limit ladder for a credit
    /// failure. And the turns in question do not need it: each either completes
    /// (which proves credit is available and CLEARS this bar) or takes its own
    /// credit error and arrives here by the same path. Not killing them is both
    /// the smaller blast radius and the more honest reading.
    fn note_account_credit_exhausted(&self, state: &Arc<ThreadState>, reason: &str) {
        let account = state.account.lock().unwrap().clone();
        let until = (crate::clock::now() + chrono::Duration::seconds(crate::retry::CREDIT_BAR_SECS))
            .to_rfc3339();
        let (labels, identity) = self.login_labels(&account);
        for label in &labels {
            if let Err(e) = self.store.set_account_limit(
                label,
                Some((&until, Some(reason), Some("credit"), identity.as_deref())),
            ) {
                tracing::warn!(%label, error = %e, "recording a credit-balance bar failed");
            }
        }
        tracing::warn!(
            %account, labels = ?labels, until = %until, bar_secs = crate::retry::CREDIT_BAR_SECS,
            "CREDIT BALANCE EXHAUSTED on this LOGIN — barred from placement on every label of \
             it. The turn is NOT ended: it is handed back to admission, which may place it on \
             an account that still has credit. The bar expires so one turn can discover a \
             top-up, and a completed turn clears it immediately"
        );
        // An input to placement just moved — every retrying thread re-asks now.
        // NAMED, so the board can say which login ran out of credit rather than
        // only that something changed.
        self.account_picture_changed_for("credit_exhausted", Some(&account));
        // P9, the credit half.
        for label in &labels {
            self.emit_limit_fact(label);
        }
    }

    /// **A login failed to authenticate: bar the LOGIN, and say the one thing
    /// only he can act on.**
    ///
    /// The sibling of [`Self::note_account_credit_exhausted`], and deliberately
    /// the same mechanism down to the line: `set_account_limit` on every label of
    /// the login, stamped with the identity it was observed on, with
    /// `limit_axis` naming the reason. That column is the crate's ONE bar —
    /// `AccountState::available` reads it, `unavailable_accounts` widens it
    /// across aliases, `AdmissionRequest::unavailable` is how placement sees it.
    /// No third exclusion set is invented, and in particular this does NOT
    /// invent one by reaching for `dead_since`; see below.
    ///
    /// `limit_axis` is `"login"`, the second non-quota axis after wave one's
    /// `"credit"`. It matters more here than there, because `accounts status`
    /// renders a bar as a limit window, and rendering an expired credential as a
    /// usage window would tell him to wait for a reset that is never coming —
    /// the exact opposite of the action he needs to take. `main.rs` reads this
    /// axis and prints the ask instead.
    ///
    /// # Why NOT `dead_since`, which exists and looks like it was made for this
    ///
    /// Because it and this are mutually exclusive, not because it is broken.
    /// `dead_since` bars the login as a placement TARGET and deliberately leaves
    /// `unavailable` empty, so the thread already on it STAYS (DEC-26) and keeps
    /// taking its turns there — and that thread's later success is the only
    /// thing that clears the flag (`note_login_health`'s success branch, and
    /// `selection_prereqs::a_dead_login_leaves_the_pool_and_stays_visible` pins
    /// exactly that).
    ///
    /// The operator asked for the turn to be RETRIED ELSEWHERE. The moment it
    /// moves, `dead_since` has no clearer left: nothing is on the account to
    /// succeed there, and `placeable_pool` will not send anything new. Setting
    /// BOTH would therefore trade "the turn dies" for "the account dies", which
    /// is the worse bargain. So this path owns the concern alone, with a bar that
    /// expires and that `note_account_served` can lift.
    ///
    /// The consequence is that `note_login_health`'s FAILURE branch is now
    /// unreachable for auth text — `classify_message` names it `LoginExpired`
    /// before it can ever become `Deterministic`. Its success branch still runs
    /// and still clears a `dead_since` written by an older daemon. That
    /// vestigial half is called out in the wave report rather than left to be
    /// discovered.
    ///
    /// So the bar expires ([`crate::retry::LOGIN_BAR_SECS`]) and the existing
    /// `note_account_served` clears it on the first completed turn, which is the
    /// only observation the daemon can actually make about a human having
    /// re-authenticated. That function needs no change: it already clears every
    /// label of a login at once.
    ///
    /// # It deliberately does NOT raise the mid-turn kill
    ///
    /// For the same two reasons `note_account_credit_exhausted` gives, and the
    /// first one applies with more force: `raise_limit_kill` raises `LimitKill`,
    /// which `run_turn` synthesizes into a `RateLimit` verdict, so every torn-
    /// down turn would be relabelled a usage limit and would spend the
    /// usage-limit ladder for a credential failure — re-burying the class this
    /// function exists to dig out. And the turns in question do not need it:
    /// each either completes (proving the login authenticates, which CLEARS this
    /// bar) or takes its own auth failure and arrives here by this same path.
    fn note_account_login_expired(&self, state: &Arc<ThreadState>, reason: &str) {
        let account = state.account.lock().unwrap().clone();
        let (labels, identity) = self.login_labels(&account);
        // THE BAR IS NOW A LADDER, not a constant (2026-08-27). The recheck is
        // a local `claude` spawn that the CLI refuses on the credential before a
        // model is reached — zero tokens, sub-second — so the operator ruled it
        // can happen far more often than every thirty minutes. See
        // `retry::login_bar_secs` for the local-versus-API determination.
        // Is the slot's own store WIPED? If so it leaves placement until it is
        // signed in, rather than until the timed bar below expires — which is
        // the thrash this read exists to stop. One keychain read per failure.
        self.note_slot_after_auth_failure(&account);
        let streak_key = identity.clone().unwrap_or_else(|| account.clone());
        let streak = {
            let mut m = self.login_bar_streak.lock().unwrap();
            let e = m.entry(streak_key).or_insert(0);
            let seen = *e;
            *e = e.saturating_add(1);
            seen
        };
        let bar_secs = crate::retry::login_bar_secs(streak);
        let until = (crate::clock::now() + chrono::Duration::seconds(bar_secs)).to_rfc3339();
        for label in &labels {
            if let Err(e) = self.store.set_account_limit(
                label,
                Some((&until, Some(reason), Some("login"), identity.as_deref())),
            ) {
                tracing::warn!(%label, error = %e, "recording a login-expiry bar failed");
            }
        }
        // ERROR, not WARN, and it is the one line in this path written to be
        // READ BY A HUMAN. Every other failure class in this crate is something
        // the daemon can wait out; this one cannot be fixed by anything the
        // daemon is able to do, so the log has to name the human action. It is
        // the same escalation `note_retry_starving` makes and for the same
        // reason: a state only he can leave must not be reported at the volume
        // of a state that clears itself.
        tracing::error!(
            %account, labels = ?labels, until = %until, bar_secs, streak,
            "LOGIN EXPIRED on this login — barred from placement on every label of it. RUN \
             `claude /login` UNDER THIS ACCOUNT'S CONFIG DIR; nothing else fixes this and no \
             amount of retrying will. The turn is NOT ended: it is handed back to admission, \
             which may place it on a login that still authenticates. The bar expires so one \
             turn can discover a re-authentication, and a completed turn clears it immediately"
        );

        // **THE ONE NOTICE A DAY.** Not the alarm the operator struck:
        //
        // > *"I don't really need an alarm... Sounds like it's just going to
        // > nag me, and I don't want to be nagged. Maybe just, like, if there's
        // > an attempt to place on a thread, you can notify me in the audio
        // > user interface, but I don't want to be notified more than once per
        // > day."*
        //
        // "An attempt to place on a thread" is exactly where this sits: a turn
        // just tried to run on this login and was refused on the credential.
        // Nothing here fires on a timer and nothing files a ticket.
        //
        // Keyed on the LOGIN, not the label, so five labels of one credential
        // are one notice — and two genuinely different expired accounts are
        // two, because they are two things for him to fix.
        //
        // The gate is DURABLE (`store::claim_notice`) rather than a field on
        // this manager, because this daemon restarts itself whenever a new
        // binary lands and an expired login is a state only he can leave: an
        // in-memory window would re-open on every boot, and the daemon would
        // nag him precisely because it was trying not to.
        let notice_subject = identity.clone().unwrap_or_else(|| account.clone());
        match self.store.claim_notice(
            crate::event::LOGIN_EXPIRED_NOTICE,
            &notice_subject,
            chrono::Duration::days(1),
            crate::clock::now(),
        ) {
            Ok(Some(suppressed_since)) => {
                let also = match suppressed_since {
                    0 => String::new(),
                    1 => " It has also blocked one other attempt since I last mentioned it."
                        .to_string(),
                    n => format!(
                        " It has also blocked {n} other attempts since I last mentioned it."
                    ),
                };
                let speakable = format!(
                    "The login for {} has expired, so work cannot run on it. Sign it in again \
                     under that account's config directory.{}",
                    labels.first().map(String::as_str).unwrap_or(&account),
                    also
                );
                self.emit(
                    state,
                    None,
                    ThreadEvent::AccountLoginExpiredNotice {
                        account: account.clone(),
                        labels: labels.clone(),
                        bar_until: until.clone(),
                        suppressed_since,
                        speakable,
                    },
                );
            }
            Ok(None) => tracing::debug!(
                %account,
                "login-expiry notice suppressed — already spoken within the last day"
            ),
            // Best-effort. The BAR above is not best-effort and is already
            // recorded; failing to say something out loud must never cost the
            // placement decision.
            Err(e) => {
                tracing::warn!(%account, error = %e, "could not claim the login-expiry notice")
            }
        }

        // An input to placement just moved — every retrying thread re-asks now.
        self.account_picture_changed_for("login_expired", Some(&account));
        // P9 — a login bar IS a limit window as far as every card is
        // concerned, and `accounts.list` reports it through the same columns.
        for label in &labels {
            self.emit_limit_fact(label);
        }
    }

    /// **THE STARVATION BOUND, for retries.** A usage-limit retry past
    /// [`crate::retry::usage_limit_starving_from`] is not waiting for a reset
    /// any more, it is starving, and it says so at ERROR and on the ledger,
    /// repeating every [`crate::retry::usage_limit_escalate_every`] attempts.
    ///
    /// Modelled on `update.rs::check_starvation` deliberately, down to the
    /// repeat: that module records that deduplicating its own deferral line
    /// forever is exactly why "an hour of starvation left ONE line in the ledger
    /// and looked like seven quiet deferrals." A single warning at attempt 12
    /// would have described attempt 924 identically.
    ///
    /// The one deviation from that precedent is WHERE it lands. `update.rs`
    /// owns a private JSONL ledger (`restarts.jsonl`) because a restart belongs
    /// to the daemon, not to any thread. A starving retry belongs to a specific
    /// thread and a specific delivery, so its durable home is that thread's
    /// event ledger — the same place `retry_scheduled` already lives, with the
    /// same `LedgerOnly` audience, so it reaches the operator and never the
    /// model.
    fn note_retry_starving(
        &self,
        state: &Arc<ThreadState>,
        kind: &TransientKind,
        reason: &str,
        attempt: u32,
        ceiling: u32,
        account: &str,
    ) {
        // `since` is read back rather than recomputed: it survives every re-arm
        // and this call may be the four hundredth. `retry_at` is no longer a
        // per-thread instant at all — see `next_cadence_at` — so the report names
        // when the POOL will next reconsider this prompt, which is the question a
        // reader of a starvation line is actually asking.
        let since = self
            .store
            .get_retry(&state.id)
            .ok()
            .flatten()
            .map(|r| r.created_at)
            .filter(|c| !c.is_empty());
        let retry_at = self.next_cadence_at();
        tracing::error!(
            thread = %state.id,
            attempt,
            give_up_at = ceiling,
            account = %account,
            retry_at = %retry_at,
            since = ?since,
            "retry: THIS DELIVERY HAS BEEN REFUSED {attempt} TIMES AND IS STILL RETRYING. \
             Every one of those attempts spawned a real turn on {account} and was refused. \
             A usage limit does not refuse {attempt} times; this is a loop, not a backoff. \
             Giving up at {ceiling}. Reason: {reason}",
        );
        self.emit(
            state,
            None,
            ThreadEvent::RetryStarving {
                error_kind: kind.kind_str().into(),
                reason: reason.to_string(),
                attempt,
                give_up_at: ceiling,
                account: account.to_string(),
                retry_at,
                since,
            },
        );
    }

    /// **The end of the usage-limit ladder.** Same shape as the transient and
    /// connectivity give-ups a few lines above in `run_turn` — clear the row,
    /// emit a `TurnFailed` — because "NEVER SILENTLY" is a property of every
    /// ladder here, and this one had no end at which to honour it.
    ///
    /// Clearing the row is what makes it visible everywhere at once, with no
    /// second mechanism: `thread_status` derives `retrying` purely from the
    /// existence of a `retries` row, so the thread leaves `retrying` the instant
    /// this runs, and `accounts status`'s RETRYING column drops it in the same
    /// breath. The `TurnFailed` is what AUI hears — §7.5 of the data contract
    /// has AUI disarm its own retry scheduler on the strength of the daemon
    /// owning retries, so a give-up that stayed silent on the wire would leave
    /// the turn simply never spoken about again.
    fn give_up_on_usage_limit(
        &self,
        state: &Arc<ThreadState>,
        message_ids: &[u64],
        kind: &TransientKind,
        reason: &str,
        attempt: u32,
        account: &str,
    ) {
        let since = self.store.retry_created_at(&state.id).ok().flatten();
        tracing::error!(
            thread = %state.id,
            attempt,
            account = %account,
            since = ?since,
            "retry: GIVING UP on this delivery after {attempt} refused attempts on {account}",
        );
        // Named as a LIMIT that never cleared, not as generic exhaustion,
        // because the action he can take is different and specific: the account
        // is named, so he can look at that login rather than at the thread.
        let message = format!(
            "the usage limit on {account} never cleared: {attempt} attempts were spent on \
             this turn and every one was refused, so it was given up. Its text has been kept \
             and can be re-sent. Reason: {reason}"
        );
        // Through the SHARED exit, which is what clears the retry row and keeps
        // his words — this used to `clear_retry` inline, which meant the longest
        // ladder in the crate was also the one that destroyed the prompt.
        self.give_up_and_keep_the_prompt(state, message_ids, kind, message);
    }

    /// **THE OUTPUT-STALL CHECK, ONE COPY FOR BOTH TAIL LOOPS** (R9.L6,
    /// R9.L6b). Called once per poll by `run_once` and by `adopt_turn`;
    /// returns `true` on the poll it tears the child down (SIGTERM sent), so
    /// the caller arms its SIGKILL escalation.
    ///
    /// The fifth teardown, the twin of the other four in mechanism (same
    /// `signal_turn`, same grace, same SIGKILL escalation) and of the limit
    /// kill in destination: it does not END the delivery, it ladders it. A
    /// hang is a failure, not an intention, and the rule is that only an
    /// intention ends a delivery (`retry::ends_the_delivery`).
    ///
    /// HELD while a native async agent is outstanding. Such an agent runs
    /// inside this process and writes nothing to the parent's stream, so the
    /// parent can be legitimately silent for as long as the agent works — an
    /// hour is ordinary. The silence bound is measured from the later of the
    /// last output and the last poll that saw an agent outstanding, so it
    /// counts silence AFTER the last agent completes, never through it.
    ///
    /// THE HOLD IS BOUNDED (R9.L6b, ledger R9.L6-1): `outstanding` is cleared
    /// only by the CLI's `task_notification` or the process exiting, so a
    /// notification that never arrives would hold forever. Past
    /// `Tunables::async_hold` of parent silence with agents still outstanding
    /// the child is torn down anyway, the agent ids named.
    ///
    /// `armed` is the caller's "no other teardown in flight": a turn already
    /// being cancelled, killed or stopped must not be re-attributed to a stall
    /// during its grace window.
    ///
    /// R9.L6c / R13.5: a child whose LAST word was a clean `result` has
    /// DELIVERED; it is reaped and the turn lands on that verdict
    /// ([`StallClock::retry_decision`] answers `None` for it), and output after
    /// that result is recorded as `post_result_output_truncated`, never
    /// silently lost.
    fn check_output_stall(
        &self,
        state: &Arc<ThreadState>,
        pid: i64,
        clock: &mut StallClock,
        sig: &TurnSignals,
        armed: bool,
    ) -> bool {
        let bound = self.tunables.output_stall;
        if bound.is_zero() || clock.stalled.is_some() || !armed {
            return false;
        }
        let now = std::time::Instant::now();
        let outstanding: Vec<String> = state.live.lock().unwrap().running();
        let silent = clock.last_output.elapsed();
        let (fire, held_agents) = if !outstanding.is_empty() {
            clock.held_at = Some(now);
            let hold = self.tunables.async_hold;
            (!hold.is_zero() && silent >= hold, Some(outstanding))
        } else {
            let since = match clock.held_at {
                Some(h) if h > clock.last_output => h,
                _ => clock.last_output,
            };
            (since.elapsed() >= bound, None)
        };
        if !fire {
            return false;
        }
        clock.stalled = Some(silent);
        // R13.5: "delivered" is the LAST word (`StallClock::retry_decision`
        // reads the same field), so the fate named here is the fate given.
        let delivered = sig.last_word_result;
        let fate = if delivered {
            "after delivering its result — the child is reaped and the turn lands on the \
             verdict it already sent"
        } else {
            "— the child is treated as hung, torn down, and the turn handed to the retry ladder"
        };
        let reason = match &held_agents {
            None => {
                tracing::warn!(
                    thread = %state.id, pid, silent_ms = silent.as_millis() as u64, delivered,
                    "OUTPUT STALL — the child has written nothing for longer than the \
                     stall bound; tearing it down ({})",
                    if delivered {
                        "its result already arrived: the turn lands, no retry"
                    } else {
                        "laddering the turn"
                    }
                );
                clock.retry_reason = Some(format!(
                    "claude wrote no output for {}s and was torn down as hung",
                    silent.as_secs()
                ));
                format!(
                    "no output from claude for {}s (stall bound {}s) {fate}",
                    silent.as_secs(),
                    bound.as_secs()
                )
            }
            Some(agents) => {
                tracing::warn!(
                    thread = %state.id, pid, silent_ms = silent.as_millis() as u64, delivered,
                    hold_ms = self.tunables.async_hold.as_millis() as u64,
                    outstanding = ?agents,
                    "ASYNC HOLD EXPIRED — the parent has written nothing for longer than the \
                     async-agent hold while these agents stayed outstanding with no \
                     task_notification; tearing it down"
                );
                clock.retry_reason = Some(format!(
                    "claude wrote no output for {}s while async agent(s) {} never reported \
                     completion, and was torn down as hung",
                    silent.as_secs(),
                    agents.join(", ")
                ));
                format!(
                    "no output from claude for {}s while async agent(s) {} stayed outstanding \
                     past the async hold ({}s) {fate}",
                    silent.as_secs(),
                    agents.join(", "),
                    self.tunables.async_hold.as_secs()
                )
            }
        };
        self.emit(
            state,
            None,
            ThreadEvent::TurnKilled {
                reason,
                // Not a budget trip.
                budget_key: None,
                strategy: self.teardown.as_str().to_string(),
            },
        );
        // R9.L6c: output after the verdict (trailing task lines — anything
        // that opened a turn re-armed the ladder, R13.5) that the stall cut
        // off is not re-run, so it is RECORDED — never silently lost. An existing kind
        // (`custom`, ledger-only); no storage change.
        if delivered && sig.events_after_result > 0 {
            tracing::warn!(
                thread = %state.id, pid, events = sig.events_after_result,
                "post_result_output_truncated — the child wrote more after its \
                 result and then hung; the turn lands on that result and the \
                 later output is not retried"
            );
            self.emit(
                state,
                None,
                ThreadEvent::Custom {
                    event_type: "post_result_output_truncated".into(),
                    data: json!({
                        "events_after_result": sig.events_after_result,
                        "silent_ms": silent.as_millis() as u64,
                        "stall_bound_ms": bound.as_millis() as u64,
                        "reason": "the child wrote more after its clean result and \
                                   then went silent past the stall bound; the turn \
                                   landed on that result and the later output was \
                                   cut off, not retried",
                    }),
                },
            );
        }
        signal_turn(pid, self.teardown, libc::SIGTERM);
        true
    }

    /// One spawn+tail+wait cycle. The child's stdout is a DURABLE append-only
    /// file the daemon tails (G1): a daemon restart mid-turn re-adopts the
    /// child from the persisted `(pid, cookie, path, offset)` instead of
    /// killing it.
    ///
    /// `message_ids` and `attempt` are carried in for one reason only: they are
    /// written onto the `running_turns` row beside the prompt, so that a daemon
    /// which restarts mid-turn can tear this execution down and land it
    /// durably. `run_once` itself does nothing else with them.
    // ── R13.1: A DELIVERED MESSAGE IS NEVER RE-SENT ──────────────────────
    //
    // Every re-entry of a delivery — the in-turn ladder (`run_turn`'s loop),
    // a fired retry (`fire_retry`), a retry scheduled by restart recovery
    // (L-1, `recover`), a resumed park (`maybe_dispatch`), an adopted turn
    // handed back to the ladder — reaches the child through ONE door, the
    // stdin write in `run_once`. Before R13.1 that door wrote the stored
    // prompt unconditionally into a `--resume`d conversation that already
    // held it and hours of work on it (DIAG-duplicate-resume: an internal thread
    // got message 62 four times). The question is now asked there, of the
    // ledger, once per attempt.

    /// **WHAT THE LEDGER SAYS ABOUT THIS DELIVERY** (R13.1 A, C).
    ///
    /// The DELIVERY is bounded below by the newest `delivery_attempted`
    /// (R10.M1) naming any of these ids: an operator's explicit re-send opens
    /// a new attempt, and what an earlier delivery of the same id did must not
    /// suppress the one he asked for. A pre-R10.M1 delivery has no such event
    /// and is bounded by the thread's start.
    ///
    /// Within it, an id is DELIVERED when a `turn_started` naming it was
    /// followed — before the next `turn_started` on the thread — by output the
    /// MODEL authored (`assistant_text`, `thinking`, `tool_use`). That is
    /// deliberately stronger than "`turn_started` names it": a `turn_started`
    /// is written before the spawn, so an attempt refused at the spawn guard,
    /// at the projection, or by a failed spawn has one too and delivered
    /// nothing — calling that delivered would tell the model to continue a
    /// message it never saw, which is a LOST message, the one failure worse
    /// than a duplicate. Model output after the prompt is the proof the
    /// conversation holds it. An API error before any output (a 429) is not
    /// proof and the prompt goes out again on the next attempt, as before.
    pub(crate) fn delivery_evidence(&self, thread_id: &str, message_ids: &[u64]) -> DeliveryEvidence {
        let mut out = DeliveryEvidence::default();
        if message_ids.is_empty() {
            return out;
        }
        let window_start: u64 = self
            .store
            .last_attempt_ids(thread_id, message_ids)
            .unwrap_or_default()
            .values()
            .filter_map(|a| a.strip_prefix("att:").and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        // Every `turn_started` in the window, oldest first, paged.
        let started_kind = vec!["turn_started".to_string()];
        let mut starts: Vec<(u64, Vec<u64>)> = Vec::new();
        let mut after = window_start;
        loop {
            let page = match self.store.list_events(thread_id, after, 1000, Some(&started_kind)) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(thread = %thread_id, error = %e, "R13.1: could not read turn_started; treating the delivery as undelivered");
                    return out;
                }
            };
            let Some(last) = page.last().map(|e| e.seq) else { break };
            for e in page {
                if let ThreadEvent::TurnStarted { message_ids: ids, .. } = &e.event {
                    starts.push((e.seq, ids.clone()));
                }
            }
            after = last;
        }
        let output_kinds = vec!["assistant_text".to_string(), "thinking".to_string(), "tool_use".to_string()];
        let turn_kinds = vec![
            "system_init".to_string(),
            "result_turn".to_string(),
            "task_notification".to_string(),
            "assistant_text".to_string(),
            "thinking".to_string(),
            "tool_use".to_string(),
        ];
        let mut delivered: Vec<u64> = Vec::new();
        let mut replied: Vec<u64> = Vec::new();
        let mut unsure: Vec<u64> = Vec::new();
        for (i, (seq, ids)) in starts.iter().enumerate() {
            if !ids.iter().any(|m| message_ids.contains(m)) {
                continue;
            }
            out.prior_starts += 1;
            let next = starts.get(i + 1).map(|(s, _)| *s);
            let inside = |x: u64| next.map_or(true, |n| x < n);
            // ── R13.5: WHICH CLI TURN CONSUMED THE OPERATOR'S PROMPT ──────
            //
            // One `-p --resume` process can stream SEVERAL CLI turns, each
            // opened by a `system_init`. Before the prompt, the CLI runs every
            // `<task-notification>` queued from the previous session as its
            // own mini-turn and ANNOUNCES it with a `task_notification` line
            // before that turn's `init` (DEPLOY-9m seq 54 → init 55 → result
            // 56 → init 57; DEPLOY-9l seq 91 → 92 → 93 → 94). In the prod
            // store (R13.5-inspect Q2: 121 such windows since 2026-08-13)
            // every one of those mini-turns is SILENT — it ends in a result
            // with no model output — and the operator's turn is the next,
            // un-announced `init`.
            //
            // So a turn is skipped as a pre-prompt mini-turn only while it is
            // BOTH announced (a `task_notification` between the end of the
            // turn before it — its `result`, else its `init`; for the first
            // turn, `turn_started` — and its own `init`) AND silent (no
            // top-level model output before the next `init`). The operator's
            // turn is the first turn that is un-announced OR speaks, and model
            // output from its `init` on is the evidence.
            //
            // Which way each ambiguity falls is chosen (ruling R13.5-4): never
            // a duplicate. An announced turn that SPEAKS is taken as his — it
            // may be a speaking mini-turn (the model never saw the message)
            // but it may equally be his own turn with a notification folded in
            // or re-announced before it (R13.5-inspect q1d), where skipping it
            // re-sent an answered message. Unless a later un-announced turn
            // spoke, such a delivery is UNSURE (R13.6) and gets a note that
            // asks the model to check and to say so if it cannot see the
            // message: a possible loss is loud, never a duplicate. When every turn is announced and
            // silent, the first is taken. A silent announced mini-turn
            // followed by an un-announced turn that wrote nothing is the one
            // shape that re-sends: the model has not answered anything since
            // the prompt could have been read.
            let mut floor = *seq;
            let mut op_init: Option<u64> = None;
            let mut replied_here = false;
            let mut unsure_here = false;
            {
                let mut evs: Vec<(u64, &'static str)> = Vec::new();
                let mut after = *seq;
                'page: loop {
                    let page = match self.store.list_events(thread_id, after, 1000, Some(&turn_kinds)) {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    let Some(last) = page.last().map(|e| e.seq) else { break };
                    for e in &page {
                        if !inside(e.seq) {
                            break 'page;
                        }
                        let k = match &e.event {
                            ThreadEvent::SystemInit { .. } => "init",
                            ThreadEvent::ResultTurn { is_error: false, .. } => "result",
                            ThreadEvent::ResultTurn { .. } => "errored_result",
                            ThreadEvent::TaskNotification { .. } => "notification",
                            ThreadEvent::AssistantText { parent_tool_use_id: None, .. }
                            | ThreadEvent::Thinking { parent_tool_use_id: None, .. }
                            | ThreadEvent::ToolUse { parent_tool_use_id: None, .. } => "output",
                            _ => continue,
                        };
                        evs.push((e.seq, k));
                    }
                    after = last;
                }
                // Turns, in order: (init seq, announced, spoke).
                let mut turns: Vec<(u64, bool, bool)> = Vec::new();
                let mut announced = false;
                for (s, k) in &evs {
                    match *k {
                        "notification" => announced = true,
                        "result" | "errored_result" => announced = false,
                        "init" => {
                            turns.push((*s, announced, false));
                            announced = false;
                        }
                        _ => {
                            if let Some(t) = turns.last_mut() {
                                t.2 = true;
                            }
                        }
                    }
                }
                let op_idx = turns
                    .iter()
                    .position(|(_, announced, spoke)| !announced || *spoke)
                    .or(if turns.is_empty() { None } else { Some(0) });
                op_init = op_idx.map(|i| turns[i].0);
                if let Some(i) = op_idx {
                    let (init, announced, spoke) = turns[i];
                    floor = init;
                    // R13.6 (R13.5-inspect-2 F-a): taken only because it
                    // SPOKE while announced, and no later un-announced turn
                    // spoke — it may be a speaking mini-turn the prompt has
                    // not reached yet. Its `result` is then no proof of a
                    // reply, so the message is UNSURE, never REPLIED.
                    //
                    // R13.7 (R13.6-inspect F-b, IS-55): when a later
                    // un-announced turn DID speak, that turn decides whether
                    // the reply was delivered — the speaking announced turn's
                    // own `result` is no proof of it. A clean `result` after
                    // that later turn opened is; without one (it was cut
                    // mid-work: G3-C, or a kill mid-tool) the message is
                    // UNSURE, never told "Do not answer it again".
                    let later_spoke = if announced && spoke {
                        turns[i + 1..].iter().find(|(_, a, s)| !*a && *s).map(|t| t.0)
                    } else {
                        None
                    };
                    unsure_here = announced && spoke && later_spoke.is_none();
                    // R13.5-inspect finding 3: a clean `result` after his turn
                    // opened means his turn ENDED — the reply is delivered,
                    // and whatever was cut later was another step.
                    let reply_floor = later_spoke.unwrap_or(init);
                    replied_here = !unsure_here && evs.iter().any(|(s, k)| *s > reply_floor && *k == "result");
                    if later_spoke.is_some() && !replied_here {
                        unsure_here = true;
                    }
                }
            }
            let answered = match self.store.list_events(thread_id, floor, 1, Some(&output_kinds)) {
                Ok(first) => first.first().is_some_and(|o| inside(o.seq)),
                Err(_) => false,
            };
            let _ = op_init;
            if answered {
                for m in ids.iter().filter(|m| message_ids.contains(m)) {
                    if !delivered.contains(m) {
                        delivered.push(*m);
                    }
                    if replied_here && !replied.contains(m) {
                        replied.push(*m);
                    }
                    if unsure_here && !unsure.contains(m) {
                        unsure.push(*m);
                    }
                }
            }
        }
        out.replied = message_ids.iter().copied().filter(|m| replied.contains(m)).collect();
        out.unsure = message_ids.iter().copied().filter(|m| unsure.contains(m)).collect();
        out.delivered = message_ids.iter().copied().filter(|m| delivered.contains(m)).collect();
        // ── R13.2: HOW OFTEN THE ORIGINAL ALREADY WENT IN ────────────────────
        //
        // An attempt WROTE the original when its child came up — `system_init`
        // before the next `turn_started` — and it was not an attempt that got
        // a continue note instead (`delivery_resumed{sent: "continue"}` in its
        // window). The daemon writes stdin right after the spawn and the CLI
        // reads all of it before `init`, so an attempt that died before its
        // write (a refused or failed spawn, a kill in the spawn window) shows
        // no `system_init` and counts zero. Read off events every turn already
        // writes, so no turn's event sequence changes.
        let custom_kind = vec!["custom".to_string()];
        let mut continued: Vec<u64> = Vec::new();
        let mut after = window_start;
        loop {
            let page = match self.store.list_events(thread_id, after, 1000, Some(&custom_kind)) {
                Ok(p) => p,
                Err(_) => break,
            };
            let Some(last) = page.last().map(|e| e.seq) else { break };
            for e in &page {
                if let ThreadEvent::Custom { event_type, data } = &e.event {
                    if event_type == crate::event::delivery::RESUMED && data.get("sent").and_then(|v| v.as_str()) == Some("continue") {
                        continued.push(e.seq);
                    }
                }
            }
            after = last;
        }
        let init_kind = vec!["system_init".to_string()];
        for (i, (seq, ids)) in starts.iter().enumerate() {
            if !ids.iter().any(|m| message_ids.contains(m)) {
                continue;
            }
            let next = starts.get(i + 1).map(|(s, _)| *s);
            let inside = |x: u64| x > *seq && next.map_or(true, |n| x < n);
            if continued.iter().any(|c| inside(*c)) {
                continue;
            }
            let came_up = match self.store.list_events(thread_id, *seq, 1, Some(&init_kind)) {
                Ok(first) => first.first().is_some_and(|o| inside(o.seq)),
                Err(_) => false,
            };
            if came_up {
                out.prompt_writes += 1;
            }
        }
        // WHY: the newest event in the window that cut this delivery.
        let cut_kinds: Vec<String> = [
            "turn_interrupted",
            "turn_killed",
            "retry_scheduled",
            "turn_parked",
            "turn_resumed",
            "turn_failed",
        ]
        .iter()
        .map(|k| k.to_string())
        .collect();
        let mut after = window_start;
        loop {
            let page = match self.store.list_events(thread_id, after, 1000, Some(&cut_kinds)) {
                Ok(p) => p,
                Err(_) => break,
            };
            let Some(last) = page.last() else { break };
            after = last.seq;
            let v = serde_json::to_value(&last.event).unwrap_or(Value::Null);
            let reason = v
                .get("reason")
                .or_else(|| v.get("error_kind"))
                .or_else(|| v.get("cause"))
                .cloned()
                .unwrap_or(Value::Null);
            out.why = Some(json!({"kind": last.event.kind(), "seq": last.seq, "reason": reason}));
        }
        out
    }

    /// The ids of `message_ids` this delivery has NOT yet put in front of the
    /// model (R13.1) — what a cancel may still owe the operator.
    pub(crate) fn undelivered_of(&self, thread_id: &str, message_ids: &[u64]) -> Vec<u64> {
        let ev = self.delivery_evidence(thread_id, message_ids);
        message_ids.iter().copied().filter(|m| !ev.delivered.contains(m)).collect()
    }

    /// The words a RESUMED session gets instead of a message it already has
    /// (R13.1 A). Never the original text, and never the cause: the cause is
    /// an operator-account fact (a limit, a login, a restart) that the replay
    /// audit (`ThreadEvent::audience`, DEC-55) keeps off the model's context —
    /// it lives on the ledger in `delivery_resumed.why`.
    /// The note for an AMBIGUOUS delivery (R13.2): its text reached this
    /// session's stdin at least twice and the model never answered, so it is
    /// not sent a third time. Because "written to stdin" is not proof the
    /// conversation holds it, the note asks the model to SAY so if it does not
    /// see the message — the duplicate is bounded, and a loss is never silent.
    pub(crate) fn ambiguous_note(message_ids: &[u64]) -> String {
        let ids: Vec<String> = message_ids.iter().map(|m| format!("#{m}")).collect();
        context::system_notice(
            context::NOTICE_KIND_RESUMED,
            &format!(
                "Your turn on message {ids} was interrupted before you replied, and this session has \
                 been resumed.\nThat message was already delivered to this session; it is not being \
                 sent again.\nIf you cannot see message {ids} above, say so plainly so it can be \
                 re-sent. Otherwise, answer it now.",
                ids = ids.join(", ")
            ),
        )
    }

    /// R13.5 (inspect finding 3): the note for a re-entry whose reply was
    /// already DELIVERED — a clean `result` ended the turn on these ids, and
    /// what the cut interrupted was a later step in the same process (a
    /// wake by a task or agent notification). Telling the model its turn on
    /// the message was interrupted invites a second answer to it.
    pub(crate) fn replied_note(message_ids: &[u64]) -> String {
        let ids: Vec<String> = message_ids.iter().map(|m| format!("#{m}")).collect();
        context::system_notice(
            context::NOTICE_KIND_RESUMED,
            &format!(
                "Your reply to message {ids} was delivered; a later step in this session was \
                 interrupted and the session has been resumed.\n\
                 Message {ids} is in this conversation above and is not being sent again. Do not \
                 answer it again: finish only what that later step left open, or end your turn if \
                 nothing is left.",
                ids = ids.join(", ")
            ),
        )
    }

    /// R13.6 (R13.5-inspect-2 F-a): the note for a re-entry whose delivery
    /// is UNSURE — the turn taken as the operator's spoke while announced,
    /// so it may have been a pre-prompt mini-turn (the model never saw the
    /// message) or his own turn (the model answered it). It says the message
    /// may or may not have been answered and asks the model to CHECK before
    /// answering, and to say so if it cannot see the message: a loss is loud,
    /// and an answered message is not invited a second answer.
    pub(crate) fn unsure_note(message_ids: &[u64]) -> String {
        let ids: Vec<String> = message_ids.iter().map(|m| format!("#{m}")).collect();
        context::system_notice(
            context::NOTICE_KIND_RESUMED,
            &format!(
                "This session was interrupted and has been resumed. Message {ids} was already \
                 delivered to this session and is not being sent again, but it may or may not have \
                 been answered.\nCheck the conversation above before answering. If you already \
                 replied to message {ids}, finish only what is still open, or end your turn if \
                 nothing is. If you have not replied to it yet, answer it now.\nIf you cannot see \
                 message {ids} above, say so plainly so it can be re-sent.",
                ids = ids.join(", ")
            ),
        )
    }

    pub(crate) fn continue_note(message_ids: &[u64]) -> String {
        let ids: Vec<String> = message_ids.iter().map(|m| format!("#{m}")).collect();
        context::system_notice(
            context::NOTICE_KIND_RESUMED,
            &format!(
                "Your turn on message {} was interrupted and this session has been resumed.\n\
                 That message was already delivered and is in this conversation above; it is not \
                 being sent again.\nContinue the work where you left off.",
                ids.join(", ")
            ),
        )
    }

    async fn run_once(
        &self,
        state: &Arc<ThreadState>,
        prompt: &str,
        message_ids: &[u64],
        attempt: u32,
    ) -> TurnOutcome {
        state.cancel_requested.store(false, Ordering::SeqCst);
        let first_turn = !state.resumable.load(Ordering::SeqCst);

        // Snapshot LIVE settings + rescan plugins (Q9/Q10 hot paths).
        let (cfg, snapshot) = {
            let s = state.settings.lock().unwrap();
            let dirs = plugins::collect_plugin_dirs(&s);
            let snapshot = plugins::snapshot(&dirs);
            let dir_strs: Vec<String> = dirs.iter().map(|p| p.display().to_string()).collect();
            (SpawnConfig::from_settings(&state.id, &s, dir_strs), snapshot)
        };
        // Between-turn plugin change? Emit the observation + mark for restate.
        {
            let mut prev = state.plugin_snapshot.lock().unwrap();
            if !prev.is_empty() {
                let (added, removed, modified) = plugins::diff_snapshots(&prev, &snapshot);
                if !(added.is_empty() && removed.is_empty() && modified.is_empty()) {
                    state.plugins_dirty.store(true, Ordering::SeqCst);
                    self.emit(
                        state,
                        None,
                        ThreadEvent::PluginsChanged {
                            added,
                            removed,
                            modified,
                        },
                    );
                }
            }
            *prev = snapshot;
        }

        // ── THE SPAWN GUARD: WHOSE LOGIN IS THIS TURN ABOUT TO USE? ──────
        //
        // The LAST line of defence, and the only one that sees the value that
        // actually reaches `cmd.env_remove`. Everything upstream reasons about
        // NAMES — placement, admission, failover, re-resolution — and this is
        // the one place that reasons about the DIRECTORY those names resolved
        // to, at the instant it is used. The distance between the two is the
        // whole hazard: `config_dir` was resolved when the thread was placed or
        // rehydrated, and `None` here means `CLAUDE_CONFIG_DIR` gets REMOVED,
        // which means `~/.claude` — the operator's own terminal login.
        //
        // See `ThreadManager::spawn_login` for the predicate and for why the
        // machine default legitimately spawns with no directory at all. A
        // refusal PARKS the turn with his prompt kept; it never substitutes an
        // account, because substituting one would move a thread off its
        // conversation without carrying it (DEC-56).
        let account = state.account.lock().unwrap().clone();
        let config_dir = self.current_config_dir(state, &account);
        let login = match self.spawn_login(&account, config_dir.as_deref()) {
            Ok(login) => login,
            Err(reason) => {
                return self.refuse_unresolved_login(state, prompt, message_ids, attempt, &reason)
            }
        };
        // ── …AND NEVER AS AN ACCOUNT THAT IS NOT AVAILABLE (H179) ──
        //
        // The same last line, for the ONE availability predicate
        // (`account_unavailability`). Admission has already failed the thread
        // over or parked it, so this fires only when the picture moved between
        // admission and here (an auth failure on another thread, a disable, a
        // slot reading). It parks with his prompt kept; it never substitutes an
        // account.
        if let Some(why) = self.account_unavailable_reasons(&account) {
            tracing::warn!(
                thread = %state.id, %account, reasons = ?why,
                "REFUSED TO SPAWN THIS TURN: its account is not available — parked, never tried"
            );
            let reason = format!("{account} is not available ({})", why.join(", "));
            return self.refuse_unresolved_login(state, prompt, message_ids, attempt, &reason);
        }

        // ── THE PROJECTED TURN (R4.S4a, `crate::ledger::turn`) ───────────────
        //
        // Only for a thread whose settings say `projection: projected`; the
        // default is `native_owned` and skips every line of this block, so an
        // unflipped thread's path is today's byte for byte. For a flipped
        // thread the native file the runtime is about to `--resume` is RENDERED
        // from the thread's ledger here, in full, before the spawn, and
        // `system{projected}` lands on the ledger beside this turn. Whether the
        // runtime resumes or starts a session is then a fact about the render
        // — a render with no conversation lines means `--session-id` — so
        // `first_turn` is recomputed from it rather than from `resumable`.
        //
        // A refusal (`ProjectionRefused`, typed) PARKS the turn with cause
        // `projection` and his prompt kept — the mechanism login refusal uses,
        // one arm up — and never substitutes a render it could not vouch for.
        // The cookie is minted here because the ledger's events of this turn
        // carry it, exactly as the durable stdout file below is named by it.
        let cookie = uuid::Uuid::new_v4().to_string();
        let (projected, allow_loss) = {
            let s = state.settings.lock().unwrap();
            (s.projection.is_projected(), s.projection_allow_loss.clone())
        };
        let mut proj: Option<crate::ledger::turn::ProjectedTurn> = None;
        let mut first_turn = first_turn;
        if projected {
            let Some(home) = crate::accounts::transcript_home() else {
                return self.refuse_projection(
                    state,
                    prompt,
                    message_ids,
                    attempt,
                    "HOME is not set: no transcript store to render the thread into",
                );
            };
            let now = crate::store::now_rfc3339();
            let begun = crate::ledger::turn::ProjectedTurn::begin(crate::ledger::turn::TurnInputs {
                data_dir: &self.data_dir,
                thread_id: &state.id,
                config_dir: login.config_dir(),
                home: &home,
                cwd: &cfg.cwd,
                prompt,
                message_ids,
                attempt,
                cookie: &cookie,
                allow_loss: &allow_loss,
                now: &now,
                write_lock: self.managed.write_lock(),
            });
            match begun {
                Ok(p) => {
                    first_turn = !p.resumable();
                    state.resumable.store(!first_turn, Ordering::SeqCst);
                    tracing::info!(
                        thread = %state.id,
                        lines = p.rendered.lines,
                        bytes = p.rendered.bytes,
                        digest = %p.rendered.digest,
                        losses = ?p.rendered.losses,
                        recovered = ?p.recovered.as_ref().map(|r| (r.prompts_confirmed, r.ingested)),
                        native = %p.native_path().display(),
                        "rendered the thread's native file from its ledger before the spawn"
                    );
                    proj = Some(p);
                }
                Err(e) => {
                    return self.refuse_projection(state, prompt, message_ids, attempt, &e.to_string())
                }
            }
        }
        // ── R13.1: SEND IT, OR TELL THE RESUMED SESSION TO CONTINUE ──────────
        //
        // Asked HERE, after the projection (which settles whether this spawn
        // resumes a conversation at all) and before the stdin write below,
        // which is the only door the prompt has into a process. Every re-entry
        // path arrives at this line: see `delivery_evidence`.
        //
        // CONTINUE only when BOTH hold: the spawn RESUMES a conversation
        // (`!first_turn` — a fresh session, e.g. after the CLI lost the
        // conversation, has never seen the message and gets it verbatim), and
        // the conversation already HOLDS every id of the batch — the model
        // answered it on an earlier attempt of this delivery, or (projected
        // thread) the ledger's `user` line for this batch is in the render.
        // Anything less sends the original, exactly once per attempt: a
        // duplicate is recoverable, a message the model never saw is not.
        let evidence = self.delivery_evidence(&state.id, message_ids);
        let on_ledger = proj.as_ref().is_some_and(|p| p.prompt_on_ledger);
        let all_delivered = !message_ids.is_empty()
            && (on_ledger || message_ids.iter().all(|m| evidence.delivered.contains(m)));
        // ── R13.2: AN AMBIGUOUS DELIVERY IS RE-SENT ONCE, NOT PER RETRY ──────
        //
        // Written to stdin, never answered, process gone: R13.1 cannot tell
        // whether the conversation holds it, so it re-sends — and before this
        // it did so on EVERY retry, up to the ladder's ceiling (48). After the
        // original has gone in TWICE (the first send and one re-send), a
        // resumed session gets the ambiguous note instead. A fresh session
        // (`first_turn`) still gets the original: it provably has nothing.
        let ambiguous = !all_delivered && !message_ids.is_empty() && evidence.prompt_writes >= 2;
        let continue_only = !first_turn && (all_delivered || ambiguous);
        let reentry = evidence.prior_starts > 1 || on_ledger || !evidence.delivered.is_empty();
        let stdin_text: String = if continue_only && !all_delivered {
            Self::ambiguous_note(message_ids)
        } else if continue_only && !on_ledger && message_ids.iter().any(|m| evidence.unsure.contains(m)) {
            // R13.6: never "do not answer it again" to a message whose turn
            // may have been a speaking mini-turn — check, then answer.
            Self::unsure_note(message_ids)
        } else if continue_only
            && !on_ledger
            && message_ids.iter().all(|m| evidence.replied.contains(m))
        {
            Self::replied_note(message_ids)
        } else if continue_only {
            Self::continue_note(message_ids)
        } else {
            prompt.to_string()
        };

        // ── THE RUN TOKEN AND THE DOOR (R4.S5) ───────────────────────────────
        //
        // Every turn the daemon runs is handed its own run token (env
        // `THREADS_RUN_TOKEN`) and an MCP config naming this daemon's `/mcp`
        // façade with that token as its bearer — which is how a model turn
        // reaches `agents_*` and managed execution at all. The token is minted
        // for THIS cookie, so it verifies only while this turn's
        // `running_turns` row stands; the config file is removed when
        // `_mcp_file` drops at the end of this function.
        let (run_token, _mcp_file) = self.turn_mcp(&state.id, &cookie);
        let mut cfg = cfg;
        cfg.run_token = Some(run_token);
        cfg.mcp_config = _mcp_file.as_ref().map(TurnMcpConfig::path);
        // R11.J3: `--json-schema` rides the turn that delivers the
        // schema-bearing message, and only that turn (STRUCTURED-OUTPUT §3.2).
        cfg.json_schema = crate::managed::turn_json_schema(self, &state.id, message_ids);
        let args = cfg.build_args(first_turn);

        // ── THE API-ERROR SCRUB (`crate::scrub`) ─────────────────────────
        //
        // BEFORE THE CHILD STARTS, and on EVERY spawn — the operator's words:
        // *"before ever starting an agent backup, regardless of if we just
        // switched accounts or not."* Not on the failover path, not gated on
        // `switched`: a usage limit recorded six turns ago is still sitting in
        // the transcript that turn seven `--resume`s from, and the model that
        // read it out of its own history on 2026-08-14 downgraded itself to a
        // cheaper model to conserve the operator's quota without asking.
        //
        // Placed here, between building argv and `cmd.spawn()`, because this is
        // the last instant at which the transcript is quiescent: the child owns
        // that file from `spawn()` onward and a rename under a running claude
        // would be a race. `first_turn` is not consulted deliberately — a
        // `--session-id` spawn normally has no transcript yet, `scrub_session`
        // then finds nothing and costs one failed directory scan, and the case
        // where a transcript DOES already exist (a port, an import, a
        // re-adoption) is exactly the one an optimisation here would miss.
        //
        // NEVER FATAL. A transcript that cannot be scrubbed must not cost the
        // operator his turn; the failure is logged loudly and the turn runs.
        // The residual is honest and stated rather than hidden: if this errors,
        // that turn resumes unscrubbed.
        if state.settings.lock().unwrap().scrub_api_error_details {
            if let Some(home) = crate::accounts::transcript_home() {
                match crate::scrub::scrub_session(login.config_dir(), &home, &state.id) {
                    Ok(s) if s.records_changed > 0 => tracing::info!(
                        thread = %state.id,
                        files = s.files_scanned,
                        rewritten = s.files_rewritten,
                        records = s.records_changed,
                        "scrubbed API-error details off the transcript before spawn"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::error!(
                        thread = %state.id, error = %e,
                        "THE API-ERROR SCRUB FAILED — this turn resumes from a transcript that \
                         may still carry vendor error prose"
                    ),
                }
            }
        }

        // Durable stdout file (G1). The cookie was minted above, where the
        // projected turn needed it first.
        let out_dir = self.data_dir.join("turns").join(&state.id);
        if let Err(e) = std::fs::create_dir_all(&out_dir) {
            return TurnOutcome::fatal(format!("creating {}: {e}", out_dir.display()));
        }
        let output_path = out_dir.join(format!("{cookie}.jsonl"));
        let out_file = match std::fs::File::create(&output_path) {
            Ok(f) => f,
            Err(e) => return TurnOutcome::fatal(format!("creating turn output file: {e}")),
        };

        let program = claude_bin();
        let env_overrides = cfg.env(&login);
        let mut cmd = Command::new(&program);
        cmd.args(&args);
        cmd.current_dir(&cfg.cwd);
        for (k, v) in &env_overrides {
            match v {
                Some(val) => {
                    cmd.env(k, val);
                }
                None => {
                    cmd.env_remove(k);
                }
            }
        }
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::from(out_file));
        cmd.stderr(std::process::Stdio::piped());
        // The turn's child leads its OWN process group, so a teardown can
        // reach everything claude spawned in one signal (D-8) rather than
        // orphaning its children. It also keeps the child insulated from
        // signals aimed at the daemon's group, which the durable-output
        // design (G1) already assumes.
        #[cfg(unix)]
        cmd.process_group(0);

        // THE CHILD OWNS ITS OWN TCC IDENTITY. Without this, macOS attributes
        // every consent prompt this `claude` provokes to the daemon's own
        // RESPONSIBLE process — the AUI ui-server — whose identity is a bare
        // Mach-O at a path Cargo recreates on every build, so nothing a consent
        // decision could attach to survives. Disclaiming here moves the consent
        // subject onto `claude` itself, which carries a Developer ID from
        // Anthropic PBC and a designated requirement that is not path-keyed.
        // The full argument, the measurement, and why it is done this way are in
        // `mod disclaim` at the bottom of this file. Arming is best-effort by
        // design: if it cannot be armed the turn spawns exactly as before.
        #[cfg(target_os = "macos")]
        if !disclaim::arm_on(&mut cmd, &program, &args, &env_overrides) && disclaim::enabled() {
            tracing::warn!(
                thread = %state.id,
                "COULD NOT DISCLAIM RESPONSIBILITY FOR THIS TURN'S claude — it will inherit \
                 the daemon's TCC identity, so any consent prompt it provokes is attributed \
                 to whatever started the daemon rather than to claude itself"
            );
        }

        // ── THE SPAWN WINDOW: THE ONLY THING A RESTART WAITS FOR ─────────
        //
        // From here until `upsert_running_turn` returns there can be a live
        // `claude` process that NOTHING ON DISK NAMES. `recover()` re-adopts
        // from `running_turns`, so a child in this gap can be neither re-adopted
        // nor buried: an `execv` here orphans it permanently.
        //
        // Everything on either side of this permit survives a restart by
        // construction — own process group, file-backed stdout, durable row,
        // `adopt_turn` tailing from `byte_offset` — which is exactly why the
        // quiet condition is this window and NOT "no turn is executing". See
        // `update::Blockers::quiet`.
        let mut spawn_permit = self.update.enter_spawn();

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return TurnOutcome::fatal(format!("spawning claude: {e}")),
        };

        // Persist the turn's process identity (G1).
        let pid = child.id().map(|p| p as i64).unwrap_or(-1);
        let recorded = self.store.upsert_running_turn(&RunningTurnRow {
            thread_id: state.id.clone(),
            pid,
            spawn_cookie: cookie.clone(),
            output_path: output_path.display().to_string(),
            byte_offset: 0,
            started_at: crate::store::now_rfc3339(),
            // What `adopt_turn` needs to honour a teardown on the other side of
            // a restart, instead of declining because it does not know what the
            // operator said (part 2 of this change).
            prompt: Some(prompt.to_string()),
            message_ids: message_ids.to_vec(),
            attempt,
        });
        // CLOSE THE WINDOW HERE, and close it even on failure.
        //
        // A failed write means this child is already unrecoverable, and holding
        // the daemon non-quiet forever does not make it recoverable — it only
        // converts one stranded child into a daemon that can never absorb a new
        // binary. So: shout, and let go. The residual is stated rather than
        // hidden.
        if let Err(e) = recorded {
            tracing::error!(
                thread = %state.id, pid, error = %e,
                "PERSISTING THE RUNNING-TURN ROW FAILED — this child is not recoverable across a \
                 daemon restart; nothing on disk names it"
            );
        }
        spawn_permit.release();

        // Feed the prompt on stdin, then close it — or, for a delivery the
        // resumed session already holds, the continue note INSTEAD of it
        // (R13.1 A/B; decided above). Never both, never the original twice.
        let mut write_error: Option<String> = None;
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(stdin_text.as_bytes()).await {
                write_error = Some(e.to_string());
            }
            let _ = stdin.write_all(b"\n").await;
            drop(stdin);
        } else {
            write_error = Some("the child had no stdin".into());
        }
        // ── R13.1 C: THE RE-ENTRY IS A LEDGER FACT ───────────────────────────
        //
        // What was resumed, why, and which ids were or were not re-sent. Only
        // for a re-entry (an earlier attempt of this delivery exists): a first
        // send is already `delivery_attempted` + `turn_started`.
        if reentry {
            let resent: Vec<u64> = if continue_only { Vec::new() } else { message_ids.to_vec() };
            let withheld: Vec<u64> = if continue_only { message_ids.to_vec() } else { Vec::new() };
            let evidence_word = if ambiguous && continue_only {
                json!("stdin_written_no_output")
            } else if !all_delivered {
                Value::Null
            } else if on_ledger {
                json!("projected_ledger")
            } else {
                json!("model_output")
            };
            self.emit(
                state,
                None,
                ThreadEvent::Custom {
                    event_type: crate::event::delivery::RESUMED.to_string(),
                    data: json!({
                        "attempt": attempt,
                        "message_ids": message_ids,
                        "why": evidence.why,
                        "delivered_ids": evidence.delivered,
                        "resent_ids": resent,
                        "withheld_ids": withheld,
                        "sent": if continue_only { "continue" } else { "prompt" },
                        "resumed_session": !first_turn,
                        "evidence": evidence_word,
                        // R13.2: written to stdin, never answered — the
                        // conversation may or may not hold it.
                        "ambiguous": ambiguous,
                        "prompt_writes": evidence.prompt_writes,
                        // R13.5: the reply to these ids was already
                        // delivered; the note says a later step was cut.
                        "replied_ids": evidence.replied,
                        // R13.6: the turn taken as his spoke while announced;
                        // the note asks the model to check before answering.
                        "unsure_ids": evidence.unsure,
                        "write_error": write_error,
                    }),
                },
            );
            tracing::info!(
                thread = %state.id, attempt, ?message_ids, continue_only,
                "R13.1: a delivery re-entered its process — {}",
                if continue_only { "already delivered, the resumed session got a continue note only" }
                else { "not delivered (or not resumable), the prompt was sent once" }
            );
        }

        // Drain stderr concurrently (flag/parse errors land here).
        let stderr_buf = Arc::new(StdMutex::new(String::new()));
        // The reader's handle is KEPT (R9.E1): the classification below joins
        // it, bounded, before it reads `stderr_buf`.
        let mut stderr_reader: Option<tokio::task::JoinHandle<()>> = None;
        if let Some(stderr) = child.stderr.take() {
            let buf = stderr_buf.clone();
            stderr_reader = Some(tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let mut b = buf.lock().unwrap();
                    b.push_str(&l);
                    b.push('\n');
                }
            }));
        }

        {
            let mut l = state.live.lock().unwrap();
            l.parent_running = true;
        }
        self.emit_liveness(state);
        let heartbeat = self.spawn_heartbeat(state);

        // Tail the durable output file.
        let mut sig = TurnSignals::default();
        let mut offset: u64 = 0;
        let mut cancelled = false;
        let mut budget_killed = false;
        let mut limit_killed: Option<LimitKill> = None;
        // Set when this turn was ended at a compaction boundary because the
        // thread does not resume through one.
        let mut compaction_stopped = false;
        // Set once a teardown has been signalled: the instant SIGTERM
        // escalates to SIGKILL.
        let mut kill_deadline: Option<std::time::Instant> = None;
        let mut exit_status: Option<std::process::ExitStatus> = None;
        // ── THE LAST-OUTPUT STAMP (R9.L6) ── see `StallClock`. Set when the
        // stall teardown fires; consumed after the loop to put the turn on a
        // ladder.
        let mut stall = StallClock::new();
        loop {
            let (lines, new_off) = read_new_lines(&output_path, offset);
            for line in &lines {
                self.process_line(state, line, &mut sig, proj.as_mut());
            }
            if new_off != offset {
                offset = new_off;
                stall.saw_output();
                let _ = self.store.update_turn_offset(&state.id, offset);
            }
            // ── THE OUTPUT STALL (R9.L6, GAP-AUDIT L-6; hold bounded R9.L6b) ──
            //
            // One copy, shared with `adopt_turn`: `check_output_stall`.
            let armed = kill_deadline.is_none()
                && !cancelled
                && !budget_killed
                && limit_killed.is_none()
                && !compaction_stopped;
            if self.check_output_stall(state, pid, &mut stall, &sig, armed) {
                kill_deadline = Some(std::time::Instant::now() + self.tunables.kill_grace);
            }
            if state.cancel_requested.swap(false, Ordering::SeqCst) {
                cancelled = true;
                // Cancel uses the SAME teardown as a budget kill, so a
                // cancelled turn no longer leaves claude's children running.
                signal_turn(pid, self.teardown, libc::SIGTERM);
                kill_deadline = Some(std::time::Instant::now() + self.tunables.kill_grace);
            }
            // A budget trip whose `on_trip` is `kill` ends the execution here.
            // (DEC-13 made KILL the default; round-7 item 3 superseded that —
            // `TripAction::Finish` is the default now, see `budgets.rs:95-108`
            // — but the teardown seam it specified is exactly this one.)
            if state.budget_kill.swap(false, Ordering::SeqCst) {
                budget_killed = true;
                let trip = state.kill_trip.lock().unwrap().clone();
                let reason = trip
                    .as_ref()
                    .map(Trip::reason)
                    .unwrap_or_else(|| "budget exhausted".to_string());
                self.emit(
                    state,
                    None,
                    ThreadEvent::TurnKilled {
                        reason,
                        budget_key: trip.as_ref().map(|t| t.budget_key.clone()),
                        strategy: self.teardown.as_str().to_string(),
                    },
                );
                signal_turn(pid, self.teardown, libc::SIGTERM);
                kill_deadline = Some(std::time::Instant::now() + self.tunables.kill_grace);
            }
            // ── THE MID-TURN INTERRUPT (`ThreadSettings::limit_kill_mid_turn`)
            //
            // The third teardown flag, and structurally the twin of the two
            // above: same swap, same `signal_turn`, same grace window, same
            // escalation to SIGKILL below. Only the DESTINATION differs — this
            // one does not park, it routes into `run_turn`'s 429 branch, which
            // hands the thread back to `admit_or_park`.
            //
            // No placement, no account selection and no failover happens here
            // or anywhere on the interrupt path (DEC-56 as amended). This block
            // tears an execution down; the balancer decides where it goes.
            if state.limit_kill.swap(false, Ordering::SeqCst) {
                let sigk = state.limit_kill_signal.lock().unwrap().clone();
                let account = sigk
                    .as_ref()
                    .map(|k| k.account.clone())
                    .unwrap_or_else(|| state.account.lock().unwrap().clone());
                let quota = sigk
                    .as_ref()
                    .and_then(|k| k.pool.clone())
                    .unwrap_or_else(|| "<unnamed>".to_string());
                limit_killed = Some(sigk.unwrap_or(LimitKill {
                    reset: None,
                    pool: None,
                    account: account.clone(),
                }));
                self.emit(
                    state,
                    None,
                    ThreadEvent::TurnKilled {
                        reason: format!(
                            "usage limit on {account} ({quota}) — interrupted mid-turn and \
                             handed back to admission"
                        ),
                        // Not a budget trip. Naming one here would attribute
                        // the teardown to a meter that never moved.
                        budget_key: None,
                        strategy: self.teardown.as_str().to_string(),
                    },
                );
                signal_turn(pid, self.teardown, libc::SIGTERM);
                kill_deadline = Some(std::time::Instant::now() + self.tunables.kill_grace);
            }
            // ── THE POST-COMPACTION ENDING (`resume_after_compaction`) ───────
            //
            // The fourth teardown flag, and mechanically the twin of the three
            // above: same swap, same `signal_turn`, same grace window, same
            // escalation to SIGKILL below. It is written to look like them on
            // purpose — a reader who understands one understands all four.
            //
            // What differs is the MEANING, and the ledger says so. The other
            // three record a `TurnKilled`: something was refused, or the
            // operator pulled the plug, and a kill is the honest word. This one
            // records a `TurnInterrupted` with trigger `compaction`, because
            // nothing failed and nothing refused — the turn worked, the
            // conversation was compacted, and the thread is being stopped there
            // on the operator's standing instruction. `TurnEnded` still follows
            // it below, as it does for every execution, so the turn's terminal
            // record is a clean pair rather than an absence.
            //
            // The delivery ENDS here rather than laddering
            // (`retry::ends_the_delivery`). Nothing is re-spawned, nothing is
            // parked, and the prompt is not re-offered: it was delivered, it was
            // worked on, and that work is already on the ledger above the
            // boundary. Messages still QUEUED behind this turn were never
            // claimed by it and are untouched — `finish_turn` re-drains the
            // queue as it always does, so they go out on the next turn, which
            // picks up the post-compaction restatement `interpret` just armed.
            if state.compaction_stop.swap(false, Ordering::SeqCst) {
                compaction_stopped = true;
                self.emit(
                    state,
                    None,
                    ThreadEvent::TurnInterrupted {
                        reason: "an automatic compaction happened and this thread does not \
                                 resume after one (resume_after_compaction=false) — the turn \
                                 ends here; the next turn is told what was compacted away"
                            .to_string(),
                        trigger: InterruptTrigger::Compaction.as_str().into(),
                    },
                );
                signal_turn(pid, self.teardown, libc::SIGTERM);
                kill_deadline = Some(std::time::Instant::now() + self.tunables.kill_grace);
            }
            if let Some(dl) = kill_deadline {
                if std::time::Instant::now() >= dl {
                    signal_turn(pid, self.teardown, libc::SIGKILL);
                    kill_deadline = None;
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    // Final drain after exit.
                    let (lines, new_off) = read_new_lines(&output_path, offset);
                    for line in &lines {
                        self.process_line(state, line, &mut sig, proj.as_mut());
                    }
                    offset = new_off;
                    let _ = self.store.update_turn_offset(&state.id, offset);
                    exit_status = Some(status);
                    break;
                }
                Ok(None) => {}
                Err(_) => break,
            }
            tokio::time::sleep(self.tunables.tail_poll).await;
        }
        // ── THE FLAG IS NEVER LEFT RAISED ACROSS A TURN BOUNDARY ─────────────
        //
        // The FINAL DRAIN above reads the last lines of the stream after the
        // child has already exited, and then breaks — so a compaction that
        // arrives in the same poll as the exit is interpreted, raises
        // `compaction_stop`, and never meets the swap inside the loop. Because
        // the flag lives on the THREAD and not on the execution, leaving it
        // raised there would arm the NEXT turn to stop on its first poll,
        // having never compacted. That is a stale flag ending a turn for
        // something that happened during a previous one, and it is the kind of
        // defect that would look, from outside, like the daemon stopping
        // threads at random.
        //
        // Draining it here closes that window. The turn is already over, so
        // there is nothing to tear down and nothing to record — the compaction
        // is on the ledger and `needs_restate` is armed, both by `interpret`.
        // What it DOES still do is end the delivery rather than let the child's
        // exit be laddered into a re-spawn, which is the same policy the
        // in-loop branch applies, for the same reason.
        //
        // **Honest about its own coverage:** in every ordering a fixture can
        // construct, the in-loop check above has already consumed the flag and
        // this is a no-op — `compaction_stop.rs` cannot force it to fail on its
        // own, and its docstring says so. It is kept because the two breaks it
        // guards (the final drain, and the `Err(_)` arm) are real exits that
        // skip the loop body, and because the cost of being wrong here is a
        // thread that stops turns nobody can explain.
        if state.compaction_stop.swap(false, Ordering::SeqCst) {
            compaction_stopped = true;
        }
        heartbeat.abort();
        let _ = self.store.clear_running_turn(&state.id);

        self.process_exited(state);
        self.emit_liveness(state);

        let exit_code = exit_status.and_then(|s| s.code());
        self.emit(state, None, ThreadEvent::TurnEnded { exit_code });

        // ── RECONCILE (R4.S4a) — the runtime's tail, read once, now that the
        // file is quiescent. The prompt as the runtime wrote it is confirmed by
        // text, an exchange the daemon never saw is ingested under the
        // runtime's own uuids, attachments and meta lines are named losses,
        // and `system{reconciled}` clears the derived dirty mark. A failure
        // here leaves the mark SET: placement refuses the thread until the next
        // turn's crash-recovery reconcile, which is the safe direction.
        if let Some(p) = proj.as_mut() {
            match p.end(exit_code, &crate::store::now_rfc3339()) {
                Ok(r) => tracing::info!(
                    thread = %state.id,
                    status = ?r.status,
                    prompts_confirmed = r.prompts_confirmed,
                    ingested = r.ingested,
                    already = r.already,
                    losses = ?r.losses,
                    tail_bytes = r.tail_bytes,
                    "reconciled the runtime's tail into the thread's ledger"
                ),
                Err(e) => tracing::error!(
                    thread = %state.id, error = %e,
                    "RECONCILE FAILED — the thread stays dirty and placement will refuse it \
                     until a later turn reconciles it"
                ),
            }
        }

        // First successful spawn → future turns resume, and this thread now
        // has conversation HISTORY on this account (round-7 item 7: the
        // admission gate parks rather than abandoning it).
        state.resumable.store(true, Ordering::SeqCst);
        self.note_thread_account(state);

        // ── A CANCEL OUTRANKS EVERY OTHER TEARDOWN (R9.L6d, ledger R9.L6b-1) ──
        //
        // Checked FIRST, above the budget and limit returns. A cancel that
        // races a limit kill or a budget kill (both flags set before the child
        // exits) is still the operator's word: the delivery ends. Below them,
        // the limit return won and the turn went back to admission and ran
        // again — the daemon re-running what the operator stopped. The adopted
        // path (`adopt_turn`) already checks cancel first; this matches it.
        // ── THE RULE, AT THE SITE THAT ACTUALLY ENFORCES IT ──────────────────
        //
        // **This is where a user interruption ends the delivery.** Not in
        // `run_turn`'s `outcome.cancelled` arm — that arm is defence in depth,
        // and mutation testing proved it: disabling it entirely leaves every
        // test green, because the `decision: None` returned below already ends
        // the turn through the success arm.
        //
        // The load-bearing act is the SHORT-CIRCUIT here, which returns before
        // the classifier can look at the non-zero exit a killed child leaves
        // behind. Remove it and the operator's own cancellation is classified
        // like any other failure and lands on a retry ladder — the daemon
        // arguing with the person who told it to stop.
        //
        // So the rule is consulted HERE, where it decides something. Written as
        // a call rather than as an implicit truth so that
        // `retry::ends_the_delivery` is load-bearing rather than decorative: if
        // it ever stops answering `true` for `Operator`, this stops
        // short-circuiting and the integration test goes red immediately.
        if cancelled && crate::retry::ends_the_delivery(InterruptTrigger::Operator) {
            // ── R9.L4b: THE CANCEL THAT BEATS A BUDGET KILL RELEASES ITS LATCH ──
            //
            // `check_trip_midturn` latched this thread's kill before the
            // teardown, and since R9.L6d/L6e the operator's cancel outranks
            // the park — so no `parked_turns` row is written and
            // `cancel_turn`'s parked-row branch (the only other release) never
            // runs. Without this the thread stayed held by a kill he had
            // already cancelled until the limit moved, the window rolled or
            // the daemon restarted (the ledger seed already reads the
            // `turn_interrupted{operator}` below as `cancelled_after`). Own
            // latch only: a sibling on the same key stays held.
            self.release_own_kill_latch(&state.id);
            // ── SAY SO, DO NOT LEAVE IT TO BE INFERRED ──
            //
            // A cancelled turn used to emit `turn_ended` and nothing else, so
            // the only thing a client could do was wait out its
            // no-result grace window (AUI's is 90 seconds) and then guess. The
            // operator pressed a button and heard nothing for a minute and a
            // half. The ledger knows exactly what happened at this line, so it
            // records it: interrupted, by the OPERATOR.
            //
            // Emitted AFTER `turn_ended` so the exit code is on the record
            // first and this reads as the reason that turn ended, and the same
            // event is emitted by `adopt_turn` on the same condition — a
            // re-adopted orphan must not behave differently.
            self.emit(
                state,
                None,
                ThreadEvent::TurnInterrupted {
                    reason: "the operator interrupted this turn".into(),
                    trigger: InterruptTrigger::Operator.as_str().into(),
                },
            );
            return TurnOutcome {
                decision: None,
                reset: sig.reset,
                pool: sig.pool,
                cancelled: true,
                budget_killed: false,
                limit_killed: false,
                compaction_stopped: false,
            login_refused: false,
            projection_refused: false,
            };
        }

        if budget_killed {
            // The prompt is parked by `run_turn`, never dropped (CON-4).
            return TurnOutcome {
                decision: None,
                reset: sig.reset,
                pool: sig.pool,
                cancelled: false,
                budget_killed: true,
                limit_killed: false,
                compaction_stopped: false,
            login_refused: false,
            projection_refused: false,
            };
        }
        if let Some(k) = limit_killed {
            // THE INTERRUPT'S FACTS OUTRANK THE STREAM'S. A turn killed at
            // 19:26 because the poller saw 100% has, by construction, no 429 of
            // its own in `sig` — that refusal is thirteen minutes in the future.
            // Preferring `sig` here would hand the 429 branch a `None` reset,
            // and `note_account_limit` writes nothing at all without one.
            return TurnOutcome {
                decision: None,
                reset: k.reset.or(sig.reset),
                pool: k.pool.or(sig.pool),
                cancelled: false,
                budget_killed: false,
                limit_killed: true,
                compaction_stopped: false,
            login_refused: false,
            projection_refused: false,
            };
        }
        // ── AND THE SAME SHORT-CIRCUIT FOR THE POST-COMPACTION ENDING ────────
        //
        // Placed here, beside the operator's, for the reason the block above
        // spells out: **this is the site that actually enforces the rule.** We
        // just sent SIGTERM to the child, so it is about to exit non-zero. If
        // that exit reaches the classifier five lines below, a deliberate
        // ending is read as a crash, lands on a transient ladder, and the turn
        // is re-spawned — recreating the exact post-compaction momentum the
        // setting exists to prevent, while telling the operator it had failed.
        //
        // Written as a call to `retry::ends_the_delivery` rather than as an
        // implicit `true` so the policy stays in one place and stays
        // load-bearing: if that function ever stops answering `true` for
        // `Compaction`, this stops short-circuiting and `compaction_stop.rs`
        // goes red immediately rather than quietly.
        //
        // The `TurnInterrupted` record is NOT emitted here — the tail loop
        // already wrote it at the moment of the decision, so the ledger reads
        // in causal order: `compacted`, then `turn_interrupted`, then
        // `turn_ended`. Emitting again here would double it.
        if compaction_stopped && crate::retry::ends_the_delivery(InterruptTrigger::Compaction) {
            return TurnOutcome {
                decision: None,
                reset: sig.reset,
                pool: sig.pool,
                cancelled: false,
                budget_killed: false,
                limit_killed: false,
                compaction_stopped: true,
                login_refused: false,
            projection_refused: false,
            };
        }

        // Classify a non-zero exit with no in-stream retry signal via stderr.
        let mut decision = sig.decision.take();
        // ── …UNLESS IT HAD ALREADY DELIVERED (R9.L6c, inspect F1) ──
        //
        // A child whose last word was a clean `result` finished the turn; only
        // the process lingered. Its teardown is OURS, so neither the stall
        // ladder below nor the exit-code door (a CLI that exits 143 on our
        // SIGTERM) may turn that into a failure: the turn lands with the
        // verdict it sent. Re-running it would duplicate a finished, paid
        // turn's work under `--resume`.
        // R13.5: the LAST word (`last_word_result`), not the sticky first
        // result — the same field the no-verdict guard below reads.
        let reaped_after_verdict = stall.stalled.is_some() && sig.last_word_result;
        // ── A STALLED CHILD GOES ON A LADDER (R9.L6) ──
        //
        // It was killed by a signal, so it has no exit code and the classifier
        // below would see nothing at all: the turn would read as a SUCCESS and
        // the operator's words would be dropped. A verdict the stream already
        // carried (a 429 just before the hang, say) is more specific and wins.
        if decision.is_none() {
            decision = stall.retry_decision(&sig);
        }
        if decision.is_none() && !reaped_after_verdict {
            if let Some(code) = exit_code {
                if code != 0 && !sig.schema_unmet_exit {
                    // THE STRUCTURED CAUSE FIRST (2026-08-27). A turn that
                    // streamed a synthetic API error and then exited non-zero
                    // has already told us what went wrong, in fields. stderr is
                    // the fallback, not the authority — and on a crash it is
                    // frequently empty, which is how a real cause became
                    // "claude exited with code 1".
                    // STDERR IS JOINED BEFORE IT IS READ (R9.E1, R9.F2-3). The
                    // reader is a separate task; read before it has run, the
                    // buffer is empty and a credit or login failure is
                    // classified as an anonymous `claude_error`. Bounded by
                    // `stderr_drain`: a descendant holding stderr open past it
                    // gets the turn classified from what was read, and a log.
                    if sig.api_error.is_none() {
                        if let Some(mut reader) = stderr_reader.take() {
                            let bound = self.tunables.stderr_drain;
                            if tokio::time::timeout(bound, &mut reader).await.is_err() {
                                reader.abort();
                                tracing::warn!(
                                    thread = %state.id,
                                    exit_code = code,
                                    bound_ms = bound.as_millis() as u64,
                                    "stderr not at EOF within the drain bound; \
                                     classifying this turn from TRUNCATED stderr"
                                );
                            }
                        }
                    }
                    decision = Some(sig.api_error.clone().unwrap_or_else(|| {
                        let err = stderr_buf.lock().unwrap().clone();
                        let text = if err.trim().is_empty() {
                            format!("claude exited with code {code}")
                        } else {
                            err
                        };
                        classify_message(&text)
                    }));
                }
            }
        }
        // ── R13.3: A CHILD THAT NEVER ENDED ITS TURN DID NOT FINISH IT ───────
        //
        // Everything above reads a verdict off the stream, a stall, or a
        // NON-ZERO exit code. A child killed by a signal while this daemon is
        // UP has no exit code at all, and one that exits 0 without a `result`
        // has a code that says nothing — both fell through to `decision: None`,
        // which is the CLEAN verdict: the delivery ended, nothing retried, no
        // `turn_failed`, the operator's words gone with no record (found by
        // R13.2). The turn-end marker is a clean `result`
        // (`sig.result_delivered`); with none, the turn is not done. It lands
        // on the ladder as the unclassified mid-execution death it is — the
        // SAME verdict `adopt_turn` (`adopted_verdict`) and the stall arm give
        // that death — so R13's rule decides what the retry sends (continue
        // note, the original, or after two unanswered writes the ambiguous
        // note), and the ladder's ceiling ends it LOUDLY (`turn_failed`, the
        // text kept) rather than never.
        //
        // R13.4: "a clean `result`" means the LAST word, not the first. One
        // process can stream several CLI turns (a queued `<task-notification>`
        // mini-turn ahead of the operator's), so a result followed by a newly
        // opened turn ended that earlier turn, not this delivery
        // (`TurnSignals::last_word_result`; DEPLOY-9l attempt 2).
        if decision.is_none() && !reaped_after_verdict && !sig.last_word_result {
            decision = Some(sig.api_error.clone().unwrap_or_else(|| RetryDecision::Retry {
                kind: TransientKind::ErrorDuringExecution,
                reason: match exit_code {
                    Some(code) => format!("claude exited with code {code} without ending its turn (no result)"),
                    None => "claude was killed before it ended its turn (no exit code, no result)".into(),
                },
            }));
        }
        TurnOutcome {
            decision,
            reset: sig.reset,
            pool: sig.pool,
            cancelled: false,
            budget_killed: false,
            limit_killed: false,
            compaction_stopped: false,
            login_refused: false,
            projection_refused: false,
        }
    }

    /// Parse one raw stream line into events; interpret + emit each.
    ///
    /// `proj` is `Some` for a projected turn (`crate::ledger::turn`): the same
    /// line is then also translated into the thread's ledger, through the one
    /// `translate` the archive parser uses. The `events` table is written
    /// either way, exactly as before — the ledger is beside it, not instead.
    fn process_line(
        &self,
        state: &Arc<ThreadState>,
        line: &str,
        sig: &mut TurnSignals,
        proj: Option<&mut crate::ledger::turn::ProjectedTurn>,
    ) {
        for mut ev in crate::stream::parse_raw_line(line) {
            self.interpret(state, &mut ev, sig);
            let message_id = self.assign_message_id(state, &ev);
            self.emit(state, message_id, ev);
        }
        if let Some(p) = proj {
            if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                p.observe(&v, &crate::store::now_rfc3339());
            }
        }
    }

    /// Which streamed events get a spoken message id: top-level assistant
    /// text (the addressable reply) and compaction boundaries (AUI stamps
    /// both today; sidechain text stays unaddressed).
    fn assign_message_id(&self, state: &Arc<ThreadState>, ev: &ThreadEvent) -> Option<u64> {
        match ev {
            ThreadEvent::AssistantText {
                parent_tool_use_id: None,
                ..
            }
            | ThreadEvent::Compacted { .. } => self.alloc_message_id(state).ok(),
            _ => None,
        }
    }

    /// Stateful interpretation before emit: liveness accounting, retry
    /// capture, compaction bookkeeping (§12), spend recording (Q11).
    fn interpret(&self, state: &Arc<ThreadState>, ev: &mut ThreadEvent, sig: &mut TurnSignals) {
        // R9.L6c / E17-8: has a clean verdict arrived, and how much was
        // written after it? (`TurnSignals::result_delivered`)
        if sig.result_delivered {
            sig.events_after_result += 1;
        } else if matches!(ev, ThreadEvent::ResultTurn { is_error: false, .. }) {
            sig.result_delivered = true;
        }
        // R13.4: is the LAST word still a clean result? (`last_word_result`)
        note_last_word(sig, ev);
        // R9.TS2 / TC-44: a line WRITTEN BY an in-process subagent (its
        // `parent_tool_use_id` is that subagent's launching call), or a
        // progress beat naming one, is that subagent acting.
        let now = crate::store::now_rfc3339();
        let actor = match &*ev {
            ThreadEvent::AssistantText { parent_tool_use_id, .. }
            | ThreadEvent::Thinking { parent_tool_use_id, .. }
            | ThreadEvent::ToolUse { parent_tool_use_id, .. }
            | ThreadEvent::ToolResult { parent_tool_use_id, .. } => parent_tool_use_id.clone(),
            ThreadEvent::TaskProgress { tool_use_id, .. } => tool_use_id.clone(),
            _ => None,
        };
        if let Some(actor) = actor {
            if state.live.lock().unwrap().tasks.touch(&actor, &now) {
                self.emit_subagents(state);
            }
        }
        if let ThreadEvent::ResultTurn { subtype, is_error: false, .. } = ev {
            sig.schema_unmet_exit |= crate::retry::is_schema_unmet_result(subtype);
        }
        match ev {
            // THE STREAM'S LAST WORDS, kept for the one case that has no other
            // diagnosis: a `result` flagged `is_error` whose subtype is
            // `success` (`TurnSignals::last_text`). Nothing else reads it, and
            // it is overwritten every block, so it costs one String per turn.
            ThreadEvent::AssistantText {
                text,
                parent_tool_use_id: None,
                ..
            } => {
                if !text.trim().is_empty() {
                    sig.last_text = Some(text.clone());
                }
            }
            // R9.TS2: the Task call itself — the subagent is `active` from here.
            ThreadEvent::AgentSpawn {
                tool_use_id,
                subagent_type,
                description,
                run_in_background,
                parent_tool_use_id,
            } => {
                let changed = state.live.lock().unwrap().tasks.spawn(
                    tool_use_id,
                    description.as_deref().or(subagent_type.as_deref()),
                    *run_in_background,
                    parent_tool_use_id.as_deref(),
                    &now,
                );
                if changed {
                    self.emit_subagents(state);
                }
            }
            ThreadEvent::AgentLaunched {
                tool_use_id,
                agent_id,
                is_async,
                status,
                output_file,
            } => {
                let changed = {
                    let mut l = state.live.lock().unwrap();
                    if *is_async && status != "completed" {
                        l.outstanding.insert(agent_id.clone(), output_file.clone());
                    }
                    l.tasks.launched(tool_use_id, agent_id, *is_async, status, &now)
                };
                if changed {
                    self.emit_subagents(state);
                }
            }
            ThreadEvent::AgentCompleted { agent_id } => {
                let mut l = state.live.lock().unwrap();
                l.outstanding.remove(agent_id);
            }
            // R9.TS2: the CLI names the task the call became.
            ThreadEvent::TaskStarted {
                task_id,
                tool_use_id,
                description,
                task_type,
                ..
            } => {
                let changed = state.live.lock().unwrap().tasks.started(
                    task_id,
                    tool_use_id.as_deref(),
                    description.as_deref(),
                    task_type.as_deref(),
                    &now,
                );
                if changed {
                    self.emit_subagents(state);
                }
            }
            // R9.TS2: the launching call's result. For a FOREGROUND Task this
            // is its end (`is_error` → `error: failed`); for a background one
            // it is the launch ack and ends nothing (`TaskRoster::tool_result`).
            ThreadEvent::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => {
                let changed = state.live.lock().unwrap().tasks.tool_result(tool_use_id, *is_error, &now);
                if changed {
                    self.emit_subagents(state);
                }
            }
            // THE TRUTHFUL COMPLETION. `system/task_notification` is the CLI's
            // own word that a task is over, and its `task_id` is the same
            // identifier `AgentLaunched` calls `agent_id` — verified, all 836
            // captured `agentId`s appear as a `task_started.task_id`.
            //
            // Until now the ONLY thing that could clear `outstanding` was
            // `sweep_subagents`, which infers completion from an output file's
            // mtime going quiet — measured at ~9 spurious completions per real
            // one, and misfiring on 95.6% of agents. This arm means the common
            // case never reaches that heuristic: 802 of 805 captured async
            // launches were notified inside the SAME turn, so the entry is
            // gone from `outstanding` before the turn even ends.
            //
            // Accounting only. No `AgentCompleted` is synthesized here — that
            // kind is documented as the mtime-derived signal and conflating the
            // two would destroy the ledger's ability to tell a real completion
            // from a guessed one. The `task_notification` row IS the record,
            // and it is strictly richer (status, summary, spend, output file).
            //
            // Unconditional on arrival: a notification is only ever emitted at
            // the end of a task. `status` refines HOW it ended (`completed` /
            // `failed` / `stopped`), never WHETHER — so gating on a status
            // string would strand a task whose status we simply failed to read.
            ThreadEvent::TaskNotification {
                task_id,
                tool_use_id,
                status,
                ..
            } => {
                let changed = {
                    let mut l = state.live.lock().unwrap();
                    l.outstanding.remove(task_id);
                    // R9.TS2: the same word ends the roster row, with HOW.
                    l.tasks.ended(task_id, tool_use_id.as_deref(), status.as_deref(), &now)
                };
                if changed {
                    self.emit_subagents(state);
                }
            }
            // ── A SUBAGENT'S USAGE LIMIT IS THIS ACCOUNT'S USAGE LIMIT ──
            //
            // `parse_task_updated` has always lifted the subagent's error text
            // off the wire, and `grep -n TaskUpdated runtime.rs` returned ZERO
            // hits: six records each carrying "You've hit your weekly limit ·
            // resets 1pm (America/Chicago)" were persisted between 19:33 and
            // 19:36 on 2026-08-14 and never looked at. The parent turn kept
            // running for another three and a half minutes.
            //
            // ATTRIBUTION IS EXACT, not inferred. A subagent is spawned by the
            // parent `claude` process and inherits its `CLAUDE_CONFIG_DIR`
            // (`spawn_turn` sets it per account), so a subagent's 429 is
            // necessarily a 429 on `*state.account.lock()` — the same account
            // the parent is on. There is no join to get wrong.
            ThreadEvent::TaskUpdated {
                task_id,
                status,
                error,
                is_backgrounded,
                ..
            } => {
                // R9.TS2: a terminal patch ends the roster row (`failed` →
                // `error: failed`); an `is_backgrounded` patch marks its
                // `tool_result` as a launch ack. Before the limit check, which
                // returns early on every patch that is not a 429.
                let changed = {
                    let mut l = state.live.lock().unwrap();
                    if *is_backgrounded == Some(true) {
                        l.tasks.backgrounded(task_id);
                    }
                    match status.as_deref() {
                        Some(_) => l.tasks.ended(task_id, None, status.as_deref(), &now),
                        None => false,
                    }
                };
                if changed {
                    self.emit_subagents(state);
                }
                let Some((axis, reset_prose)) =
                    subagent_limit_signal(status.as_deref(), error.as_deref(), *is_backgrounded)
                else {
                    return;
                };
                let account = state.account.lock().unwrap().clone();
                // CORROBORATION, one-directional. An observation that ACTIVELY
                // CONTRADICTS the claim vetoes it; a missing observation does
                // not. Requiring corroboration outright would make this fix
                // depend on the poller having run, which is precisely the
                // dependency that made the incident possible — and a subagent's
                // 429 is a direct refusal from the API, which is stronger
                // evidence than a fifteen-minute-old poll, not weaker. So the
                // veto fires only on a reading that cannot be reconciled with
                // exhaustion at all.
                match self.budgets.observed_value(&account, axis) {
                    Ok(Some(v)) if v < SUBAGENT_LIMIT_CONTRADICTION_FLOOR => {
                        tracing::warn!(
                            %account, axis = axis.as_str(), observed = v,
                            "a subagent reported a usage limit but the gauge last read this \
                             axis far below exhaustion — REFUSING to bar the account on the \
                             subagent's word alone"
                        );
                        return;
                    }
                    _ => {}
                }
                tracing::warn!(
                    thread = %state.id, %account, axis = axis.as_str(),
                    resets = %reset_prose,
                    "SUBAGENT DIED ON A USAGE LIMIT — barring the account now rather than \
                     waiting for the parent turn to take its own 429"
                );
                // (a) BAR. Same writer, same subject (the login), same stamp.
                self.note_account_limit(state, Some(&reset_prose), Some(axis.as_str()));
                // (b) FEED THE GAUGE, mirroring the `LimitHit` arm below. A
                // distinct source string, because this is a subagent's refusal
                // and the ledger should be able to tell the two apart — and it
                // is added to `Budgets::observe`'s no-calibrate guard beside
                // `limit_hit`, because it is the same cliff and pairing it would
                // poison `tokens_per_percent` identically.
                let resets_at = gauge::prose_to_rfc3339(&reset_prose);
                self.observe_gauge(state, axis, 100.0, resets_at, "subagent_limit_hit");
                // (c) MAKE THE TURN'S ENDING A 429. Without this the parent
                // stream may never produce one — the parent can finish
                // perfectly normally while every subagent under it died — and
                // the existing retry+failover branch would never fire.
                sig.reset = Some(reset_prose);
                sig.pool = Some(axis.as_str().to_string());
                sig.decision = Some(RetryDecision::Retry {
                    kind: TransientKind::RateLimit,
                    reason: "rate limit (429 on a subagent)".into(),
                });
            }
            ThreadEvent::LimitHit { reset, pool, .. } => {
                sig.reset = reset.clone();
                // Carry the QUOTA forward, not just the clock. The 429 branch
                // records the bar, and a bar that cannot name what refused it
                // cannot ever be scoped to it.
                sig.pool = pool.clone();
                sig.decision = Some(RetryDecision::Retry {
                    kind: TransientKind::RateLimit,
                    reason: "rate limit (429)".into(),
                });
                // DEC-15 layer 3, the backstop: a refusal we can attribute to
                // a pool IS an observation — that axis is exhausted on this
                // account. It never feeds calibration (a 429 is a cliff, not
                // a measured delta); `Budgets::observe` refuses to pair it.
                if let Some(axis) = pool.as_deref().and_then(Axis::parse) {
                    let resets_at = reset.as_deref().and_then(gauge::prose_to_rfc3339);
                    self.observe_gauge(state, axis, 100.0, resets_at, "limit_hit");
                }
            }
            // **THE API ERROR THAT IS NOT A LIMIT.** The other half of the
            // 2026-08-27 split (see `stream::is_rate_limit_api_error`).
            //
            // Note what this arm does NOT do: it does not write `sig.decision`.
            // The arm above does, because a 429 is unambiguous — the turn is
            // over and the only question is when to try again. This one is not:
            // the same record shape covers a mid-response drop the CLI then
            // recovers from. See `TurnSignals::api_error`.
            ThreadEvent::ApiError {
                error,
                status,
                text,
            } => {
                let decision = crate::retry::classify_api_error(error.as_deref(), *status, text);
                tracing::warn!(
                    thread = %state.id, error = ?error, status = ?status,
                    verdict = %decision.kind_str(),
                    "the CLI reported an API error that is NOT a usage limit — classified on \
                     its own axis rather than as a rate limit"
                );
                // FIRST one wins. A turn that takes an auth failure and then a
                // connectivity blip should be diagnosed by the thing that
                // actually stopped it, which is the first refusal, not the
                // wreckage after it.
                if sig.api_error.is_none() {
                    sig.api_error = Some(decision);
                }
            }
            ThreadEvent::RateLimitEvent {
                rate_limit_type,
                utilization,
                resets_at,
                resets_at_rfc3339,
                status,
                is_using_overage,
            } => {
                // DEC-15 layer 1: the free push gauge. `utilization` is 0-1;
                // every other gauge is 0-100, so normalize here, once.
                match Axis::from_rate_limit_type(rate_limit_type) {
                    Some(axis) => self.observe_gauge(
                        state,
                        axis,
                        *utilization * 100.0,
                        resets_at_rfc3339.clone(),
                        "rate_limit_event",
                    ),
                    None => tracing::debug!(
                        rate_limit_type = %rate_limit_type,
                        "unmapped rateLimitType — recorded on the ledger, not on an axis"
                    ),
                }
                // A-4: the two fields this arm used to drop with `..`.
                self.note_rate_limit_push(
                    state,
                    &crate::stream::RateLimitPush {
                        rate_limit_type: rate_limit_type.clone(),
                        status: status.clone(),
                        is_using_overage: *is_using_overage,
                        resets_at: *resets_at,
                        windows: Vec::new(),
                    },
                );
            }
            // A-4: the CURRENT `rate_limit_event` shape — `unifiedWindows` and
            // no top-level `utilization` — which the parser keeps as `Unknown`
            // rather than fabricate a single reading (and which the TUI renders
            // from the raw line, so it stays `Unknown` on the ledger). Read here
            // so its windows reach the gauge and its overage / `rejected`
            // reaches placement.
            ThreadEvent::Unknown { raw } => {
                if let Some(push) = crate::stream::RateLimitPush::from_line(raw) {
                    for w in &push.windows {
                        if let Some(axis) = Axis::from_rate_limit_type(&w.name) {
                            self.observe_gauge(
                                state,
                                axis,
                                w.utilization * 100.0,
                                w.resets_at.and_then(gauge::epoch_to_rfc3339),
                                "rate_limit_event",
                            );
                        }
                    }
                    self.note_rate_limit_push(state, &push);
                }
            }
            ThreadEvent::MessageUsage {
                model,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                api_message_id,
            } => {
                // Charge PROVISIONALLY as the turn streams, so a budget can
                // bite mid-turn (a `kill` toggle needs something to kill). The
                // `result_turn` aggregate reconciles below, per model — the
                // fable meter accepts fable usage only (round-7 item 4).
                //
                // ONCE PER MESSAGE. Claude emits one `assistant` line per
                // content block and repeats the message's `usage` payload on
                // every one, so charging per line double-counted the turn
                // (22,462 true tokens metered as 44,862, observed 2026-07-29).
                // A line with no `message.id` cannot be deduped and is charged
                // — under-charging is the dangerous direction, and the epilogue
                // reconciles either way.
                // The repeat is still EMITTED — the ledger keeps every line
                // claude sent; it is only the charge that happens once.
                if let Some(id) = api_message_id {
                    if !state.charged_message_ids.lock().unwrap().insert(id.clone()) {
                        return;
                    }
                }
                let tokens =
                    *input_tokens + *output_tokens + *cache_read_tokens + *cache_creation_tokens;
                // COST is not on this line, so it is PRICED from the token
                // counts that are (per-model rates, `gauge::estimate_cost_usd`).
                // Without this the cash meter could not move until the turn was
                // already over, which is precisely why a mid-turn kill was
                // unreachable. The estimate is provisional and the authoritative
                // `costUSD` replaces it below — it never becomes the record.
                let cost = crate::gauge::estimate_cost_usd(
                    model,
                    *input_tokens,
                    *output_tokens,
                    *cache_read_tokens,
                    *cache_creation_tokens,
                );
                *state
                    .turn_charged
                    .lock()
                    .unwrap()
                    .entry(model.clone())
                    .or_insert(0) += tokens;
                *state
                    .turn_cost_charged
                    .lock()
                    .unwrap()
                    .entry(model.clone())
                    .or_insert(0.0) += cost;
                let model = model.clone();
                self.charge_budget(state, &model, tokens as i64, cost);
            }
            ThreadEvent::Compacted {
                message_id_range, ..
            } => {
                // §12: stamp the pre-compaction message-id range and flag the
                // next turn for a context restatement.
                if let Ok(Some(row)) = self.store.get_thread(&state.id) {
                    let current_max = state.next_message_id.load(Ordering::SeqCst).saturating_sub(1);
                    let from = row.last_compaction_message_id + 1;
                    if current_max >= from {
                        *message_id_range = Some((from, current_max));
                    }
                    let _ = self
                        .store
                        .update_thread_compaction(&state.id, current_max, true);
                    // The boundary was just recorded and `needs_restate` raised.
                    // The range is the one stamped onto the event above, so the
                    // cell and the ledger cannot disagree about what the last
                    // compaction swallowed.
                    self.emit_compaction_fact(
                        &state.id,
                        message_id_range.map(|(from, to)| [from, to]),
                    );
                }
                // ── AND THEN, THE BRANCH (`resume_after_compaction`) ──────────
                //
                // **ONE mechanism, one branch — deliberately not two.**
                //
                // Everything above this line runs either way and is untouched:
                // the boundary is recorded, the swallowed id range is stamped,
                // and `needs_restate` is raised so that whichever turn comes
                // next is told plainly that it has just been through a
                // compaction and is given the thread id, the working path and
                // the range to go read. That is the standing ruling that an
                // automatic compaction is a HARD INTERRUPT rather than a
                // silent event, and it is not weakened here.
                //
                // The flag decides only what "next" means. With it TRUE the
                // interrupt is delivered to a thread that carries on, exactly as
                // before. With it FALSE — the default — the interrupt becomes an
                // ENDING: the turn stops at the boundary, and the restatement is
                // read by the turn the operator himself starts afterwards, with
                // whatever framing he wants to give it.
                //
                // Because both outcomes come off the same flag in the same arm,
                // they cannot disagree about whether a compaction happened or
                // about what the next turn gets told.
                let resume = state
                    .settings
                    .lock()
                    .map(|s| s.resume_after_compaction)
                    // A thread whose settings are unreadable STOPS. The
                    // conservative direction here is the one that ends a turn,
                    // because the failure this guards against is a thread
                    // ploughing on through a compaction unsupervised, and an
                    // ending is always recoverable by sending again.
                    .unwrap_or(false);
                if !resume {
                    state.compaction_stop.store(true, Ordering::SeqCst);
                }
            }
            ThreadEvent::ResultTurn {
                subtype,
                is_error,
                model_usage,
                ..
            } => {
                // Spend tracking (Q11): every result's modelUsage → ledger, AS
                // A DELTA against this execution's previous snapshot.
                //
                // `modelUsage` is the CUMULATIVE per-model total for the whole
                // claude process — parent and every subagent — as of the moment
                // that `result` line was written, not a figure belonging to the
                // result that carries it. A turn with N subagents therefore
                // emits N+1 results all quoting the same running total, and
                // storing each one verbatim recorded the turn N+1 times over
                // (measured live 2026-07-31: 17 identical rows for one turn, a
                // thread reading $2,845 against a true ~$199). Recording only
                // the LAST result would be wrong too — the total grows between
                // bursts inside a single execution, so the earlier bursts'
                // spend would go unbooked and then reappear inside the final
                // figure. `Store::record_turn_usage` owns the arithmetic and is
                // documented there.
                //
                // The running-turn row is still present here (it is cleared
                // only after this tail loop sees the child exit), so its
                // `started_at` is this execution's own start — carried onto the
                // ledger rows for the overlap-window calibrator (msg #620), and
                // used as the watermark EPOCH so a fresh process starts from a
                // fresh baseline.
                let account = state.account.lock().unwrap().clone();
                let started_at = self
                    .store
                    .get_running_turn(&state.id)
                    .ok()
                    .flatten()
                    .map(|r| r.started_at);
                let deltas = match self.store.record_turn_usage(
                    &state.id,
                    Some(&account),
                    model_usage,
                    started_at.as_deref(),
                ) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(error = %e, "usage ledger write failed");
                        Vec::new()
                    }
                };
                // Reconcile the budget against the SAME deltas the ledger just
                // booked, PER MODEL, by charging the SIGNED DIFFERENCE between
                // what this result actually added and what the provisional
                // stream charges already booked since the last reconciliation.
                // The per-model split is what keeps the weekly-fable meter
                // honest and what carries `costUSD` onto the cash meter
                // (round-7 items 4/5).
                //
                // Charging from the ledger's own return value is deliberate:
                // the meter and the ledger cannot drift apart, because there is
                // only one number. It is also what makes the meter settle
                // EXACTLY ONCE per result no matter how many results arrive.
                // The old code reconciled against the cumulative total every
                // time, while `mem::take` emptied the provisional on the first
                // result — so results two through seventeen charged the entire
                // running total again with nothing to subtract, and a budget
                // tripped at roughly a tenth of its stated amount.
                //
                // The difference is signed on BOTH axes, and that is the whole
                // point: the token side was previously `saturating_sub`, which
                // clamped to zero whenever the provisional charge ran ahead —
                // so an over-charge could never be repaired and the meter simply
                // stayed wrong (the 2× reading of 2026-07-29). The dollar side
                // needs the same property now that `MessageUsage` is priced from
                // an estimate: reconciliation must be able to hand dollars BACK.
                // After this runs the meter equals the authoritative total
                // exactly, whichever way the estimate erred.
                let provisional = std::mem::take(&mut *state.turn_charged.lock().unwrap());
                let provisional_cost =
                    std::mem::take(&mut *state.turn_cost_charged.lock().unwrap());
                let mut rows: Vec<(String, u64, f64)> = deltas
                    .into_iter()
                    .map(|d| (d.model, d.tokens, d.cost_usd))
                    .collect();
                // A model the stream charged but this result's delta never
                // mentions would otherwise keep its provisional charge forever;
                // give it a zero-delta row so the correction is written.
                for model in provisional.keys().chain(provisional_cost.keys()) {
                    if !rows.iter().any(|(m, _, _)| m == model) {
                        rows.push((model.clone(), 0, 0.0));
                    }
                }
                for (model, tokens, cost) in rows {
                    let already = provisional.get(&model).copied().unwrap_or(0);
                    let already_cost = provisional_cost.get(&model).copied().unwrap_or(0.0);
                    self.charge_budget(
                        state,
                        &model,
                        tokens as i64 - already as i64,
                        cost - already_cost,
                    );
                }
                if *is_error && sig.decision.is_none() {
                    // The SUBTYPE is not always the diagnosis. A result can
                    // arrive saying `subtype: "success"` and `is_error: true`
                    // at once — 46 times in this ledger between 2026-08-03 and
                    // 2026-08-18 — and classifying that by subtype produced a
                    // fatal `claude_error` reading "result subtype success",
                    // which destroyed the operator's turn under a sentence
                    // that contradicts itself. What failed is in the stream's
                    // last words, so they are handed over with it.
                    //
                    // THE STRUCTURED CAUSE OUTRANKS THE PROSE (2026-08-27). If
                    // the CLI already named what went wrong in a synthetic API
                    // error, use that: `classify_errored_result` is reading
                    // `last_text` and hoping, and the hope is not always well
                    // placed. The real corpus contains turns whose last
                    // assistant text says "safeguards flagged this message"
                    // while the result prose says "You've hit your session
                    // limit" — two different diagnoses of one turn, and the
                    // guess picks whichever landed last.
                    sig.decision = Some(sig.api_error.clone().unwrap_or_else(|| {
                        classify_errored_result(subtype, sig.last_text.as_deref())
                    }));
                }
                self.emit_liveness(state);
            }
            _ => {}
        }
    }

    // ── retries (Q5/G10) ─────────────────────────────────────────────────

    /// **WHERE `compute_retry_at` WAS.**
    ///
    /// It took a kind, an attempt and a reset prose and wrote a per-thread instant
    /// into `retries.retry_at`. The operator removed that instant:
    ///
    /// > *"There should not be a per-thread retry timestamp."*
    ///
    /// Its three jobs were re-homed, none of them lost:
    ///
    /// 1. **The ladders** — the six per-class `retry::*_backoff_secs` functions it
    ///    selected between — are now read through [`crate::retry::ladder_secs`] by
    ///    [`PoolCadence`], at `attempt + barren`. The NUMBERS are untouched.
    /// 2. **The parsed reset instant** for a usage limit is already the ACCOUNT
    ///    BAR's: `note_account_limit_for` writes `accounts.limit_until` from the
    ///    same prose plus the same [`crate::reset::RESET_SLACK`], and
    ///    `notice_expired_limits` turns its expiry into the `limit_expired` EVENT.
    ///    The bar was always the thing actually doing the waiting — the ladder
    ///    comments on `CreditBalance` and `LoginExpired` said so in as many words —
    ///    and the floor was a second derivation of one fact, which is the pattern
    ///    this crate bans.
    /// 3. **The `+20s` grace on top of the slack** has nothing left to order. It
    ///    existed so a floor could not come due before the bar it was waiting on
    ///    lifted; with no floor, the bar's lift IS the event, and the gate reads
    ///    `limit_until` on every ask. `RESET_SLACK` (60s) is unchanged and is the
    ///    surviving policy. This is the one number the move retires rather than
    ///    re-homes, and it is retired because its subject is gone.
    ///
    /// Do not reintroduce a function shaped like it.

    /// Persist the parked prompt, emit `RetryScheduled`, and re-arm the POOL's
    /// cadence.
    ///
    /// It used to compute and persist a deadline and arm a per-thread timer. It now
    /// writes a row that says only *what* is waiting and *why*, and rings
    /// `cadence_rearm` — because a fresh failure is at `attempt` 1, which is the
    /// shortest rung its class has, so the pool's cadence may need to be shorter
    /// than whatever it is currently sleeping.
    #[allow(clippy::too_many_arguments)]
    fn schedule_retry(
        &self,
        state: &Arc<ThreadState>,
        prompt: &str,
        message_ids: &[u64],
        attempt: u32,
        kind: &TransientKind,
        reason: &str,
        reset: Option<&str>,
        on_account: Option<&str>,
    ) {
        // WHO REFUSED. It was recorded so a floor could be attributed to a login
        // rather than to the thread; the floor is gone and the attribution is still
        // worth having, because `note_retry_starving` has to be able to name the
        // login a delivery is piling up behind. `on_account` is passed explicitly
        // by the 429 path because the gate may have moved the thread between the
        // failure and here; everything else records where it sits.
        let account = on_account
            .map(str::to_string)
            .or_else(|| Some(state.account.lock().unwrap().clone()));
        let row = RetryRow {
            thread_id: state.id.clone(),
            prompt: prompt.to_string(),
            message_ids: message_ids.to_vec(),
            attempt,
            kind: kind.kind_str().into(),
            reason: reason.to_string(),
            account,
            // READ-ONLY on this struct: the store stamps the real birth instant on
            // insert and preserves it across every re-arm.
            created_at: String::new(),
        };
        if let Err(e) = self.store.upsert_retry(&row) {
            tracing::warn!(error = %e, "persisting retry failed");
        }
        // A NEW PARKED PROMPT MAY SHORTEN THE POOL'S CADENCE. Rung, not rung-less:
        // this is a `continue` in the placement task, so it recomputes the sleep
        // without counting as a reason to reconsider placement. A failure that has
        // just happened is not news to placement — the gate has not changed its
        // mind — it is news to the CLOCK.
        self.cadence_rearm.notify_one();
        // ── NO `emit_status` HERE, AND THE PLAN SAID THERE SHOULD BE ──────
        //
        // `docs/thread-shapes-plan.md` §6.3 lists this site as a silent writer:
        // "a thread entering `retrying` produces no edge". **The precedence rule
        // refutes it.** `thread_states` ranks `executing` above everything, every
        // caller of this function is inside `run_turn` with the `TurnLock` still
        // held, and `turn_running` is still true — so the derived level at this
        // instant is `active`, not `retrying`, and a `Status` emitted here would
        // carry `active`. It would announce nothing and cost five store reads per
        // retry.
        //
        // The `retrying` level's real edge is the LOCK RELEASE, which happens
        // after this row is written and which already emits (`TurnLock::release`).
        // The ordering is what makes it correct: the row exists before the release
        // reads it, so the frame the release sends says `retrying`.
        self.emit(
            state,
            None,
            ThreadEvent::RetryScheduled {
                error_kind: kind.kind_str().into(),
                reason: reason.to_string(),
                attempt,
                // THE WIRE FIELD SURVIVES AND IS STILL HONEST. No consumer is
                // migrated by this change (`retry_at` is parsed by the AUI), and
                // the sentence it was always answering — "when will this retry next
                // be considered" — now has a POOL-level answer instead of a
                // per-thread one. What it no longer is, is a floor: nothing reads
                // it back, and nothing may.
                retry_at: self.next_cadence_at(),
                reset_prose: reset.map(str::to_string),
            },
        );
    }

    /// **WHEN THE POOL WILL NEXT RECONSIDER EVERYTHING**, as RFC3339 — the honest
    /// replacement for a per-thread `retry_at` on the wire and in the log.
    ///
    /// It is derived on demand and stored nowhere. That is the point: a stored
    /// answer would be a timestamp, and a timestamp is what was removed. An empty
    /// pool answers `now`, because with nothing parked the next reconsideration is
    /// whenever the next event rings, which is not a time this can know.
    pub fn next_cadence_at(&self) -> String {
        match self.cadence_rung() {
            Some(rung) => (crate::clock::now()
                + chrono::Duration::from_std(rung).unwrap_or_else(|_| chrono::Duration::zero()))
            .to_rfc3339(),
            None => crate::store::now_rfc3339(),
        }
    }

    /// **WHERE `arm_retry_timer` WAS — one tokio task per retrying thread,
    /// sleeping until that thread's own `retry_at`.**
    ///
    /// It was the per-thread clock in its purest form, and it is gone with the
    /// timestamp it slept on. The pool's single cadence replaced every one of
    /// those timers ([`PoolCadence`]), which also deletes two bugs the fleet of
    /// them carried: a lost lock bred one more timer per refusal, and `recover`
    /// had to re-arm and STAGGER N of them at boot to stop a stampede. One timer
    /// needs no staggering.

    /// **THE POOL'S CADENCE, AS A DURATION — or `None` when nothing is parked.**
    ///
    /// The one number that replaced N per-thread `retry_at` floors. See
    /// [`PoolCadence`] for the ruling and for why the index is a sum; this is the
    /// arithmetic.
    ///
    /// Read fresh off the live rows on every arm, deliberately: a cached rung is a
    /// per-pool timestamp, and a timestamp is the thing being removed. It costs
    /// one `list_retries` per cadence firing, and the firing itself is the thing
    /// the ladder is rationing, so the read can never be hotter than the policy.
    ///
    /// **An unreadable `kind` gets the most PATIENT ladder, not the fastest.** A
    /// row whose tag this build does not recognise must not be the row that holds
    /// the whole pool at a tight rung, so it is read as `Deterministic` — the
    /// slowest-climbing class — rather than guessed at.
    pub fn cadence_rung(&self) -> Option<Duration> {
        let rows = self.store.list_retries().unwrap_or_default();
        if rows.is_empty() {
            return None;
        }
        let barren = self.cadence.barren();
        let secs = rows
            .iter()
            .map(|r| {
                let kind = crate::retry::kind_from_str(&r.kind)
                    .unwrap_or(TransientKind::Deterministic);
                crate::retry::ladder_secs(&kind, r.attempt.saturating_add(barren))
            })
            .min()
            .unwrap_or(0);
        Some(Duration::from_secs(secs))
    }

    /// **EVERY PARKED PROMPT. THERE IS NO FILTER AND THERE IS NO SECOND
    /// SELECTION.**
    ///
    /// This function is the operator's ruling, and it is one line long because the
    /// ruling is:
    ///
    /// > *"if any thread is parked then the pool needs a retry, not the thread.
    /// > Any parked threads get checked at any opportunity."*
    ///
    /// # What was here before, and why both halves of it are gone
    ///
    /// There were TWO selections. `retries_to_reask` — the backstop's — kept rows
    /// whose `retry_at` had elapsed OR whose account had a free sibling login
    /// (`retry_unblocked_early`). `retries_to_reask_on_event` had no filter. The
    /// split was the previous round's repair, and it left the sharpest defect
    /// half-fixed: a row whose `account` column is `None` failed BOTH clauses
    /// permanently, so on the backstop leg no number of bells could ever re-ask it.
    /// It waited only for its clock.
    ///
    /// Under this ruling that row is simply parked, like every other, and it is
    /// reconsidered on every pass of every origin. Both the clock and the
    /// cheap pre-guess at the account picture are deleted rather than merged: the
    /// clock because the operator removed it, and the pre-guess because
    /// [`Self::early_ask_would_move`] asks the GATE the real question, read-only,
    /// and a second cheaper answer beside it is exactly the "two people asking the
    /// same question must not get two different answers" failure.
    ///
    /// # What stops this being a hot loop
    ///
    /// Not a filter here. Five mechanisms, and the first is the new one:
    ///
    /// 1. **The pool's cadence is a ladder** ([`PoolCadence`]). Clock-driven
    ///    reconsideration slows down as the pool keeps coming to nothing, on
    ///    `retry.rs`'s own numbers. This is what replaced the per-thread floors,
    ///    and it does the job they were doing badly.
    /// 2. **The permit coalesces the event burst.** `placement_wake` is
    ///    `Notify::notify_one`, which stores ONE permit: fourteen reasons ringing
    ///    between two passes are fourteen cheap stores and ONE pass. The unit of
    ///    cost is a PASS, not an event.
    /// 3. **`retry_firing` dedupes per thread across passes.** A row whose re-ask
    ///    is still in flight is skipped, so passes cannot stack asks on one thread.
    /// 4. **`early_ask_would_move` answers read-only, before the lock.** A
    ///    question-origin ask that the gate would neither satisfy nor refuse costs
    ///    ZERO lock acquisitions. That is `5f2b981` and this change WIDENS it: it
    ///    used to be scoped to rows whose floor had not elapsed, and it now covers
    ///    every ask that is not the pool's own deadline.
    /// 5. **The ladder is only ever spent by a turn that RAN.** Every refusal path
    ///    in [`Self::fire_retry`] returns before `run_turn`, leaving `attempt`,
    ///    `account` and the prompt exactly as they were.
    ///
    /// A refusal is still RECORDED: a gate that parks goes on to the lock and
    /// `admit_or_park` writes the park with its queryable reason, because a refused
    /// ask is information the operator can act on. Only the case where the gate
    /// admits and moves nothing is answered for free.
    pub fn retries_to_reconsider(&self) -> Vec<RetryRow> {
        self.store.list_retries().unwrap_or_default()
    }

    /// **RECONSIDER EVERY PARKED PROMPT**, whatever woke the pass.
    ///
    /// One sweep, one candidate set. The origin does not change WHO is asked — it
    /// changes what asking is allowed to cost and whether the answer is a deadline,
    /// and both of those live in [`Self::fire_retry`]. That is the surviving shape
    /// of `24ab8f9`'s rule, generalised: *"an event buys a wider set of threads to
    /// ask about, never a more permissive route for the answer."* The set is now as
    /// wide as it can get for every origin, so what is left to distinguish is only
    /// the route — and exactly one origin, the pool's own cadence, gets the
    /// deadline route.
    pub fn retry_sweep(&self, origin: PassOrigin) {
        let rows = self.retries_to_reconsider();
        if !rows.is_empty() {
            self.placement
                .reconsidered
                .fetch_add(rows.len() as u64, Ordering::SeqCst);
        }
        self.fire_each_retry(rows, origin);
    }

    /// Spawn one re-ask per selected row, at most one in flight per thread.
    ///
    /// `retry_firing` is the per-thread coalescer and it is what keeps a burst of
    /// passes from stacking asks on one thread — see
    /// [`Self::retries_to_reask_on_event`]'s bound 2.
    fn fire_each_retry(&self, rows: Vec<RetryRow>, origin: PassOrigin) {
        for row in rows {
            let id = row.thread_id;
            if !self.retry_firing.lock().unwrap().insert(id.clone()) {
                continue; // already being re-asked; do not stack another
            }
            let mgr = self.clone();
            tokio::spawn(async move {
                mgr.fire_retry(&id, origin).await;
                mgr.retry_firing.lock().unwrap().remove(&id);
            });
        }
    }

    /// Ring the bell: an input to placement moved (a limit recorded or
    /// cleared, a login died or revived, a fresh usage observation).
    ///
    /// **IT NOW RINGS OUTWARD AS WELL AS INWARD, AND THAT IS THE WHOLE FIX FOR
    /// "THE BOARD ONLY UPDATES WHEN I PRESS R".** This function already was the
    /// daemon's own chokepoint for "the account picture changed" — its doc
    /// comment named the three causes and every one of them called it — but the
    /// only thing it woke was the retry sweep, INSIDE this process. The fact
    /// never left the daemon, so a client watching the board had no way to hear
    /// it and no choice but to poll.
    ///
    /// Hanging the broadcast here rather than at the nine call sites is
    /// deliberate: a future tenth cause gets the broadcast by calling the
    /// function it already has to call, instead of by someone remembering. That
    /// is the difference between a channel that stays complete and one that
    /// decays into "the few that happen to be wired".
    ///
    /// `reason` is the machine tag that reaches the client on the frame.
    pub fn account_picture_changed(&self, reason: &str) {
        self.account_picture_changed_for(reason, None);
    }

    /// **THE SAME BELL, NAMING THE ACCOUNT IT IS ABOUT.**
    ///
    /// [`Self::account_picture_changed`] has always broadcast `account: None`,
    /// because it was written as a chokepoint for "some input to placement
    /// moved" and at the chokepoint that is all it knew. But every caller
    /// EXCEPT the rotation pass knows exactly which login moved — a limit was
    /// recorded against one account, a login died, a share was set — and threw
    /// that away on the way through.
    ///
    /// What it cost is not correctness: the client's contract is "on `info`,
    /// re-read `domain`", and re-reading the accounts answers correctly either
    /// way. It cost the NARRATION, which the operator values explicitly — the
    /// difference between a board that can say *"alice-example-com hit its
    /// weekly limit"* and one that can only say *"something about the accounts
    /// changed"*. A client cannot recover the name by diffing, either: by the
    /// time it re-reads, the new picture is the only one it has.
    ///
    /// `None` still means *fleet-wide, or we genuinely do not know*, and a
    /// rotation pass — which re-credentials every slot at once — is honestly
    /// that. Guessing a single account for it would be worse than saying
    /// nothing.
    ///
    /// The frame stays ADDITIVE: `account` is an existing optional field on
    /// [`Push::Info`] that has always been populated on the `gauge` domain, so
    /// this populates a field clients already parse rather than inventing one.
    pub fn account_picture_changed_for(&self, reason: &str, account: Option<&str>) {
        self.placement_wake(reason);
        self.emit_info("accounts", reason, account, None);
    }

    /// **RE-RUN PLACEMENT: AN INPUT TO IT MOVED.** The bell, with the
    /// announcement taken off it.
    ///
    /// # Why this is a function of its own
    ///
    /// [`Self::account_picture_changed_for`] does two things that only looked
    /// like one: it WAKES the daemon's sweeps and it ANNOUNCES on the `accounts`
    /// domain so a client re-reads. Most placement inputs want both. Two do not,
    /// and before this they had to choose:
    ///
    /// * A `/usage` reading already announces itself, per reading, on the
    ///   `gauge` domain (`probe_account`'s `emit_info`). Routing it through the
    ///   combined bell would publish every reading twice, on two domains, for
    ///   one fact.
    /// * A budget edit has no shape to announce on at all (see
    ///   `budgets/{key}/set`'s reply: `Visible::Never`), but it very much moves
    ///   placement — a raised limit can unpark threads immediately.
    ///
    /// Both of those used to reach placement by some other route or not at all:
    /// the reading by no route (a reading that takes an account back under a
    /// thread's thresholds moved placement silently), the budget edit by calling
    /// `resume_sweep()` directly — which reached the PARKED threads and left
    /// every RETRYING one waiting for a tick.
    ///
    /// # What a ring costs, and why a storm is free
    ///
    /// `notify_one` stores ONE permit. Twenty rings between two passes are
    /// therefore twenty cheap stores and ONE pass — the coalescing is a property
    /// of the primitive rather than a debounce someone has to tune. The reason
    /// is recorded in `wake_pending` so the pass can still say what woke it,
    /// which is the only thing the permit throws away.
    ///
    /// # Three things that deliberately do NOT ring, each for its own reason
    ///
    /// * **`accounts.rename`** — it reloads the pool and announces `renamed` on
    ///   the `accounts` domain without ringing, and that is correct and is left
    ///   alone: the same logins are selectable under one new name, so no input to
    ///   placement moved. Its own call site states the argument.
    /// * **A prompt landing in `parked_turns`.** All three writers —
    ///   `park_killed_turn`, `park_retried_turn` and the adopted-turn landing —
    ///   write the row because THE GATE HAS JUST SAID NO. Ringing there would ask
    ///   the same gate the same question in the same instant and get the same
    ///   answer, then park again and ring again: a live loop with no new
    ///   information in it. The arriving prompt is not what is waiting — `send`
    ///   calls `maybe_dispatch` directly on the way in, and `finish_turn` calls it
    ///   again when the lock frees — so the park is the END of a dispatch attempt,
    ///   not the start of one.
    /// * **A per-thread `Thresholds` or settings edit.** `apply_settings` already
    ///   calls `maybe_dispatch` for the thread it edited, and a threshold belongs
    ///   to one thread, so the global pass would be a sweep of every parked thread
    ///   to re-ask a question only one of them's answer changed.
    pub fn placement_wake(&self, reason: &str) {
        self.wake_pending
            .lock()
            .unwrap()
            .insert(reason.to_string());
        self.account_picture.notify_one();
    }

    /// The placement-pass ledger — see [`PlacementPasses`].
    pub fn placement_passes(&self) -> PlacementPasses {
        PlacementPasses {
            total: self.placement.total.load(Ordering::SeqCst),
            by_event: self.placement.by_event.load(Ordering::SeqCst),
            by_backstop: self.placement.by_backstop.load(Ordering::SeqCst),
            backstop_placed: self.placement.backstop_placed.load(Ordering::SeqCst),
            by_cadence: self.placement.by_cadence.load(Ordering::SeqCst),
            placed: self.placement.placed.load(Ordering::SeqCst),
            reconsidered: self.placement.reconsidered.load(Ordering::SeqCst),
            cadence_secs: self.cadence_rung().map(|d| d.as_secs()).unwrap_or(0),
            barren: self.cadence.barren(),
        }
    }

    /// The reasons the MOST RECENT placement pass was woken by — the coalescing
    /// window's contents. Empty for a pass the backstop woke, which is the
    /// point: the backstop has no reason to give.
    pub fn last_placement_reasons(&self) -> Vec<String> {
        self.placement
            .last_reasons
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect()
    }

    /// The threads the MOST RECENT placement pass dispatched.
    pub fn last_placed_threads(&self) -> Vec<String> {
        self.placement.last_placed.lock().unwrap().clone()
    }

    /// **ONE PASS OVER EVERYTHING WAITING ON PLACEMENT**, and the only thing the
    /// bell and the backstop both call.
    ///
    /// # Two sweeps, one bell, and why they are not merged
    ///
    /// [`Self::resume_sweep`] and [`Self::retry_sweep`] serve DIFFERENT STATES
    /// and are deliberately left separate:
    ///
    /// * `resume_sweep` serves `parked_turns` and waiting queues — the operator's
    ///   own definition of parked, *"there is a pending turn and it has not been
    ///   placed"* — and it re-enters through `maybe_dispatch`, which is the
    ///   ordinary dispatch path a fresh message takes.
    /// * `retry_sweep` serves `retries` rows, whose prompts live in a different
    ///   table and go out through `fire_retry`, which asks the gate before it
    ///   takes the lock.
    ///
    /// # The retry half has ONE selection now, and the origin changes only the ROUTE
    ///
    /// There used to be two candidate sets, and the floor picked between them. The
    /// floor is gone (*"There should not be a per-thread retry timestamp"*), so
    /// [`Self::retry_sweep`] reconsiders EVERY parked prompt on every pass of every
    /// origin — see [`Self::retries_to_reconsider`]. `reconsidered` counts them.
    ///
    /// What the origin still decides is whether the ask is a QUESTION or a
    /// DEADLINE, and that lives in [`Self::fire_retry`]. Exactly one origin —
    /// [`PassOrigin::Cadence`], the pool's own backoff — is a deadline. An event
    /// and the backstop both ask questions, which cost zero locks when the gate
    /// would move nothing.
    ///
    /// Merging the sweeps would mean one function with two row sets and two
    /// entry points, i.e. the same two sweeps with a shared name. What SHOULD be
    /// one is the WAKE, and that is what this is: one bell, one task, one
    /// coalescing window, both sweeps. Before it, all fourteen events woke
    /// `retry_sweep` and NONE woke `resume_sweep` — the event-driven path served
    /// the timer mechanism and the genuinely placement-shaped state was polled.
    ///
    /// `origin` says what woke this pass. The backstop is a fallback, so a
    /// backstop pass that PLACES something is a missing event, and it says so at
    /// `error` rather than quietly succeeding.
    ///
    /// **`backstop_placed` STILL MEANS "AN EVENT WAS MISSING", and the pool cadence
    /// was given its own origin precisely so that stays true.** The counter is
    /// scoped to the PARKED half ([`Self::resume_sweep_placing`]) and to
    /// [`PassOrigin::Backstop`] alone. The cadence is counted as `by_cadence` and
    /// never touches it, so the mechanism that replaced the per-thread floors
    /// cannot quietly become the thing that carries parked work while the only
    /// detector of an incomplete event set reads zero.
    pub fn placement_pass(&self, origin: PassOrigin) -> Vec<String> {
        let reasons: std::collections::BTreeSet<String> =
            std::mem::take(&mut *self.wake_pending.lock().unwrap());
        *self.placement.last_reasons.lock().unwrap() = reasons.clone();
        let placed = self.resume_sweep_placing();
        // **EVERY PARKED PROMPT IS RECONSIDERED, WHATEVER WOKE THIS PASS.** There
        // is no candidate filter left, because there is no per-thread floor left to
        // filter with: *"if any thread is parked then the pool needs a retry, not
        // the thread. Any parked threads get checked at any opportunity."*
        //
        // The origin rides along because it still decides the ROUTE — a question
        // (an event, the backstop) or a deadline (the pool's cadence). The gate
        // still decides where the thread goes, a refusal is still recorded with its
        // reason, and the attempt ladder is still only ever spent by a turn that
        // ran. See `retries_to_reconsider` for the five things that stop this
        // becoming a hot loop.
        self.retry_sweep(origin);
        *self.placement.last_placed.lock().unwrap() = placed.clone();
        self.placement.total.fetch_add(1, Ordering::SeqCst);
        self.placement
            .placed
            .fetch_add(placed.len() as u64, Ordering::SeqCst);
        // **THE POOL'S LADDER, ADVANCED OR RESET.** Anything placed at all means
        // the pool got somewhere, so the cadence drops back to its first rung; a
        // cadence that fired and placed nothing climbs one. `fire_retry` also
        // resets it just before a retry turn actually runs, which is the other
        // shape "the pool got somewhere" takes and which this pass cannot see
        // because the asks are spawned.
        if !placed.is_empty() {
            self.cadence.progressed();
        } else if origin.is_deadline() {
            self.cadence.barren_tick();
        }
        match origin {
            PassOrigin::Event => {
                self.placement.by_event.fetch_add(1, Ordering::SeqCst);
            }
            PassOrigin::Cadence => {
                self.placement.by_cadence.fetch_add(1, Ordering::SeqCst);
            }
            PassOrigin::Backstop => {
            self.placement.by_backstop.fetch_add(1, Ordering::SeqCst);
            if !placed.is_empty() {
                self.placement
                    .backstop_placed
                    .fetch_add(1, Ordering::SeqCst);
                // **A DEFECT REPORT, NOT A SUCCESS.** The backstop exists so a
                // parked thread is never stranded by an incomplete event set. It
                // placing anything means something moved that rang no bell, and
                // the whole reason gap 1 survived is that a poll which carries
                // the work quietly looks exactly like a system that works.
                tracing::error!(
                    threads = ?placed,
                    "THE BACKSTOP POLL PLACED WORK NO EVENT WOKE IT FOR — an input to \
                     placement moved with no bell to ring. The poll is a fallback; it \
                     carrying the mechanism is a MISSING EVENT, and this line is the \
                     report of one"
                );
            }
            }
        }
        if !reasons.is_empty() {
            tracing::debug!(
                reasons = ?reasons, placed = ?placed,
                "placement re-ran because an input to it moved"
            );
        }
        placed
    }

    /// Broadcast one INFORMATION UPDATE — see [`Push::Info`] for the contract
    /// and for why this announces rather than carries.
    ///
    /// Best-effort and never logged on failure: a send error means nobody is
    /// subscribed, which is the normal state of a daemon with no client
    /// attached, and it is the same policy `emit_progress` and `emit_liveness`
    /// already follow.
    pub fn emit_info(
        &self,
        domain: &str,
        reason: &str,
        account: Option<&str>,
        detail: Option<String>,
    ) {
        self.hub.emit(Push::Info {
            domain: domain.to_string(),
            reason: reason.to_string(),
            account: account.map(str::to_string),
            detail,
            at: crate::store::now_rfc3339(),
        });
    }

    /// **ASK PLACEMENT ABOUT ONE PARKED PROMPT.**
    ///
    /// `origin` is the only thing that distinguishes a QUESTION from a DEADLINE,
    /// and that distinction used to be `retry_floor_elapsed(&row)` — arithmetic on
    /// a per-thread timestamp. It is now a property of why the POOL woke:
    ///
    /// | origin | route | a refusal |
    /// |---|---|---|
    /// | [`PassOrigin::Event`] | question — read-only ask first, zero locks when the gate would move nothing | recorded with its reason; the prompt keeps its place in `retries` |
    /// | [`PassOrigin::Backstop`] | question, identically | recorded; prompt stays |
    /// | [`PassOrigin::Cadence`] | **deadline** — straight to the lock, may re-run on the account that refused | recorded, and the prompt is handed to `parked_turns` ([`Self::park_retried_turn`]) |
    ///
    /// **Why the question-first path was not collapsed.** The ruling says every
    /// parked thread is checked at every opportunity, which could be read as "so
    /// always go to the lock". It must not be: `5f2b981` made the ask read-only and
    /// pre-lock because taking the execution lock merely to ASK emitted two
    /// `Push::Status` frames, logged a "released by Drop" warning and counted as a
    /// running turn in the gauge — 27,705 warnings and a board that flickered on a
    /// five-second loop. Reconsidering MORE often makes that path more
    /// load-bearing, not less. So the question survives and WIDENS: it used to
    /// cover only rows whose floor had not elapsed, and it now covers every ask
    /// that is not the pool's own deadline.
    ///
    /// **Why the deadline was not collapsed either.** Something has to be allowed
    /// to re-run a prompt on the same account it was refused by — a usage-limit
    /// window that simply expires has no other outcome. That licence used to be
    /// "your floor elapsed". If nothing had it, a thread whose limit window ended
    /// with no event behind it would be re-asked forever and never run. The pool's
    /// cadence is what holds it now, which is the operator's model exactly: *"the
    /// pool needs a retry, not the thread."*
    async fn fire_retry(&self, thread_id: &str, origin: PassOrigin) {
        let Ok(Some(row)) = self.store.get_retry(thread_id) else {
            return; // superseded or cleared
        };
        let Ok(state) = self.get_state(thread_id) else {
            return;
        };
        // ── ASK BEFORE YOU LOCK: A QUESTION IS NOT A TURN ────────────────
        //
        // **`5f2b981`, WIDENED — NOT UNDONE.** Asking used to cost the execution
        // lock, a `Push::Status { executing: true }`, a "released by Drop" warning
        // and a second frame, every tick, forever, for the overwhelmingly common
        // answer of "nothing to do". It costs a read-only question to the gate;
        // only an answer worth a turn or worth RECORDING goes on to lock.
        //
        // Its scope used to be "a sweep ask whose floor has not elapsed". With the
        // floor gone the scope is simply "not the pool's own deadline", which is
        // STRICTLY MORE of the asks than before — exactly the right direction, since
        // the pool now reconsiders everything at every opportunity and a free
        // refusal is what makes that affordable.
        //
        // This decides WHETHER to ask again and names no placement: see
        // `early_ask_would_move`, which is the admission module's own function, for
        // why the guard below could not simply be hoisted and why this fails open.
        // It returns `true` on a PARK, so a refusal still reaches the lock and still
        // gets recorded with its reason. No retry POLICY changes here: not the
        // ladder, not the attempt, not where a thread may be placed. Only what
        // asking costs.
        if !origin.is_deadline() && !self.early_ask_would_move(&state) {
            tracing::debug!(
                thread = %thread_id, ?origin,
                "the pool was reconsidered but the gate would neither move this thread nor \
                 refuse it; the prompt keeps its place — asked without taking the lock"
            );
            return;
        }
        // ENTRY POINT: the retry timer. A retry is a next execution, so it
        // takes the same lock — a retry that fires while the thread is running
        // (a re-armed boot retry racing a fresh send, say) waits its turn
        // instead of becoming the second instance.
        let lock = match self.lock_turn(&state, "retry") {
            Ok(l) => l,
            Err(refusal) => {
                tracing::info!(
                    thread = %thread_id, reason = refusal.as_str(),
                    "retry deferred: the thread is locked by a live execution"
                );
                // NOTHING RE-ARMS, BECAUSE THERE IS NOTHING PER-THREAD TO ARM.
                // The old code armed a fresh 30s timer here for a `Timer` origin,
                // which bred one more timer per refusal. The pool's cadence comes
                // round again on its own, and so does the next event; the prompt is
                // in `retries` and `retries_to_reconsider` has no filter, so losing
                // a lock costs this prompt one pass and nothing else.
                return;
            }
        };
        // A RETRY IS A NEXT EXECUTION, so it goes through the same gate as
        // every other turn (audit F1). This path used to call `run_turn`
        // directly: `admit_or_park` was reachable only from parked-turn
        // redelivery and the queue dequeue, so a retry armed hours earlier ran
        // even when the key had since been DISABLED, a FINISH meter was
        // exhausted, a threshold was over, or the budgets module was faulting —
        // fail-closed simply did not cover it. Worst on the rate-limit case,
        // which is exactly the failure that both arms a long retry AND records
        // the 100% observation the gate would refuse on.
        let account_before = state.account.lock().unwrap().clone();
        if !self.admit_or_park(&state, &row.message_ids) {
            // The gate said no and has already emitted `turn_parked` with the
            // queryable reason. Release the slot and hand the prompt over.
            //
            // `release()`, not `drop()`. This line MEANT to release and said so,
            // but `TurnLock::release` is the only spelling that does it without
            // the "released by Drop" warning — `Drop` warns first and releases
            // second, by design, because an unreleased drop is a turn path that
            // forgot. This one has not forgotten, so it says so. The warning
            // itself is untouched: a path that still trips it is still a bug
            // worth finding, which is exactly how this one was found.
            lock.release();
            // A QUESTION IS NOT A DEADLINE, AND THIS IS THE RE-GROUNDED TEST FOR
            // WHICH ONE THIS WAS. It read `!retry_floor_elapsed(&row)` — the
            // per-thread clock. It now reads the pass's origin: only the POOL's own
            // cadence coming due is a deadline, and only a deadline's refusal hands
            // the prompt over.
            //
            // A "no" to a question is information and it has already been RECORDED
            // by `admit_or_park` with its queryable reason. The prompt stays in
            // `retries`, which is its durable home and which carries the attempt, so
            // the next reconsideration asks again. Handing it to `parked_turns` on
            // every refusal would erase the ladder on the first event that happened
            // to arrive while the gate was saying no.
            if !origin.is_deadline() {
                tracing::debug!(
                    thread = %thread_id, ?origin,
                    "the pool was reconsidered and the gate still says no; the prompt keeps \
                     its place and its attempt"
                );
                return;
            }
            self.park_retried_turn(&state, &row);
            return;
        }
        // ── AN EARLY ASK THAT CHANGED NOTHING IS STILL A NO ──
        //
        // **This is the hole the 2026-08-06 strand came through, and it is the
        // whole of the fix.** The paragraph above already says an early ask is a
        // question rather than a deadline — but it can only act on that when the
        // gate REFUSES, and for an UNGATED thread the gate never refuses:
        // `admit_or_park` downgrades every park to run-anyway (`run_ungated`),
        // and every AUI thread is ungated. So the early ask fell straight
        // through to `run_turn`, which spawned a real claude process into the
        // account that had just refused, took the same 429 about three seconds
        // later, rescheduled, and was re-asked again by the next sweep.
        //
        // Measured live: attempts 298 -> 852 in forty-five minutes, 459 claude
        // spawns, every one `is_error: true`. It is a LIVE-LOCK, not a dropped
        // wake-up — the thread never stopped asking and never once got anywhere.
        // (The sweep was firing honestly: a genuinely free login existed the
        // whole time. It could not be reached because the failover's transcript
        // port into a half-registered account kept failing, and a failed port
        // correctly declines to move the thread.)
        //
        // So progress is now REQUIRED before an early ask may spend a turn: the
        // gate must have actually MOVED the thread. If it did not, the account
        // picture moved in a way this thread cannot use, the retry row keeps its
        // floor and its attempt, and its armed timer is still the wake-up. It
        // stays visibly `retrying` — never none of running, armed, or visible.
        //
        // Deliberately not a restructuring of the retry state machine ("don't
        // make a change that is adding more risk", idx `2952`). Nothing changes
        // for a retry whose floor has elapsed, and nothing changes for an early
        // ask that genuinely relocates the thread.
        //
        // ── AND IT IS NOW THE RE-CHECK, NOT THE ONLY CHECK ──
        //
        // `early_ask_would_move` answers the cheap half of this question
        // read-only before the lock is taken, so reaching here on a question
        // means either the gate had something to say (a park, which it has now
        // recorded) or the picture CHANGED between the ask and the lock. That
        // second case is a genuine TOCTOU window and closing it is what this
        // guard is for: it is the authority, and the pre-lock question is only an
        // optimisation that keeps the common "nothing to do" from costing a lock.
        //
        // **ITS SCOPE IS NOW THE ORIGIN, AND THAT IS THE HOT-LOOP BOUND.** It used
        // to be `origin == Sweep && !retry_floor_elapsed(&row)`. The pool's cadence
        // is the only thing entitled to spend a turn on the same account it was
        // refused by, because the cadence is the thing that waited; an event-driven
        // or backstop-driven ask that admits and relocates nothing releases and
        // declines, however many bells ring.
        //
        // **IT RELEASES EXPLICITLY.** Returning with the lock still live is what
        // earned 27,705 "released by Drop" warnings: `Drop` warns and THEN
        // releases, because a turn path that exits without releasing is a defect
        // worth shouting about. That warning is not silenced and must not be — it
        // is how this bug was found. The fix is to stop EARNING it, and a path
        // that deliberately declines to run says so by releasing.
        if !origin.is_deadline() {
            let account_now = state.account.lock().unwrap().clone();
            if account_now == account_before {
                tracing::info!(
                    thread = %thread_id, account = %account_now, ?origin,
                    "the account picture moved between the ask and the lock but this thread \
                     could not be moved with it; declining instead of re-running into the \
                     same refusal"
                );
                lock.release();
                return;
            }
        }
        // THE POOL IS ABOUT TO SPEND A REAL TURN, so it is not barren: reset the
        // cadence to its first rung. This is the half of "the pool got somewhere"
        // that `placement_pass` cannot see, because asks are spawned and this
        // decision is made after that pass has already returned.
        self.cadence.progressed();
        // The row stays until the retried turn SUCCEEDS (crash-safe); the
        // next failure upserts a fresh row over it.
        self.run_turn(
            state,
            row.prompt.clone(),
            row.message_ids.clone(),
            row.attempt,
            lock,
        )
        .await;
    }

    /// A retry the admission gate refused **at the pool's own deadline**: move its
    /// prompt from the retry row into the durable `parked_turns` row, EXACTLY ONCE.
    ///
    /// # Its trigger, re-grounded
    ///
    /// It used to fire on an ELAPSED PER-THREAD FLOOR plus a refusal. The floor is
    /// gone, so it fires on [`PassOrigin::Cadence`] plus a refusal — the pool's own
    /// backoff came due, the gate was asked in earnest, and it still said no. That
    /// is the same event in the new vocabulary: the waiting is over and the answer
    /// is still no, so the prompt moves to the one place a gate-refused prompt is
    /// re-dispatched from.
    ///
    /// It is also what keeps the cadence from spinning on a row the gate will never
    /// admit: the hand-over REMOVES the row from `retries`, so it stops being an
    /// input to the pool's rung and becomes an ordinary parked turn.
    ///
    /// The retry row is the prompt's only home while a retry is armed, and
    /// `parked_turns` is the only place a gate-refused prompt is re-dispatched
    /// from (`maybe_dispatch` checks it before the queue, and `resume_sweep`
    /// re-checks it every tick). So the hand-over is ordered write-then-clear
    /// and it never overwrites: **on a write failure the retry row is left exactly
    /// where it is, so the prompt is never dropped**; when a parked turn somehow
    /// already exists, that one is redelivered first and this prompt stays in
    /// `retries` behind it, so no prompt is ever duplicated either.
    ///
    /// # `requeue` no longer arms anything, and that is a repair
    ///
    /// Every one of the three failure exits below used to arm a fresh 30-second
    /// per-thread timer, which bred one timer per refusal. Leaving the row in place
    /// is now sufficient ON ITS OWN: the prompt is in `retries`,
    /// [`Self::retries_to_reconsider`] has no filter, and the pool reconsiders
    /// everything on its own cadence and on every event. Nothing is needed to come
    /// back for it, which is why the recovery path got shorter rather than more
    /// careful.
    fn park_retried_turn(&self, state: &Arc<ThreadState>, row: &RetryRow) {
        // The prompt STAYS in `retries`; the pool will reconsider it. Ringing the
        // cadence only makes sure the pool's sleep reflects a set that did not
        // actually shrink.
        let requeue = || self.cadence_rearm.notify_one();
        match self.store.get_parked_turn(&state.id) {
            Ok(Some(_)) => {
                tracing::warn!(
                    thread = %state.id,
                    "a parked turn already holds this thread's slot; the refused prompt stays \
                     in `retries` behind it"
                );
                return requeue();
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(thread = %state.id, error = %e, "reading the parked turn failed; keeping the retry row");
                return requeue();
            }
        }
        // THE PARK IS RECORDED WITH ITS REASON, and the gate's own words are
        // preferred over this fallback. A refusal is information.
        let reason = state
            .park_reason
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| format!("the admission gate refused this retry: {}", row.reason));
        let budget_key = state.settings.lock().unwrap().budget_key.clone();
        if let Err(e) = self.store.upsert_parked_turn(&ParkedTurnRow {
            thread_id: state.id.clone(),
            prompt: row.prompt.clone(),
            message_ids: row.message_ids.clone(),
            attempt: row.attempt,
            reason,
            budget_key,
            parked_at: crate::store::now_rfc3339(),
        }) {
            // The prompt still lives in the retry row, so it is not lost —
            // come back to it rather than clearing anything.
            tracing::error!(thread = %state.id, error = %e, "PARKING A REFUSED RETRY FAILED");
            return requeue();
        }
        // Handed over: clear the retry row so `maybe_dispatch` stops deferring
        // to it and the parked prompt becomes the next thing out.
        if let Err(e) = self.store.clear_retry(&state.id) {
            tracing::warn!(thread = %state.id, error = %e, "clearing the handed-over retry row failed");
        }
        // The retry set just shrank, which may LENGTHEN the pool's cadence. Same
        // bell as a fresh failure rings, for the mirror-image reason.
        self.cadence_rearm.notify_one();
    }

    /// Every registry label that names the same LOGIN as `account`, including
    /// `account` itself, and the fingerprint they share.
    ///
    /// A label whose identity could not be read is a unit of one: absence of
    /// evidence never merges two logins, because merging wrongly would bar a
    /// real account nobody has any evidence against.
    fn login_labels(&self, account: &str) -> (Vec<String>, Option<String>) {
        let states = self.account_states();
        let identity = states.get(account).and_then(|s| s.identity.clone());
        let Some(fp) = identity else {
            return (vec![account.to_string()], None);
        };
        let mut labels: Vec<String> = self
            .registry
            .names()
            .into_iter()
            .filter(|n| {
                states
                    .get(n)
                    .and_then(|s| s.identity.as_deref())
                    .is_some_and(|id| id == fp)
            })
            .collect();
        if !labels.iter().any(|n| n == account) {
            labels.push(account.to_string());
        }
        labels.sort();
        (labels, Some(fp))
    }

    /// **Record an observed usage limit against the LOGIN that refused.**
    ///
    /// It used to be recorded against the LABEL the failing thread happened to
    /// be spawned under, and that is the whole of the 2026-08-02 defect. A label
    /// is a name for a login, not a login: `default` has no config dir, so it is
    /// whatever `~/.claude.json` currently holds, and a human `/login` re-points
    /// it without the daemon being told. A limit written against the label
    /// therefore outlives the login it describes, and
    /// [`crate::accounts::with_aliases`] then promotes that orphan into a bar on
    /// every label of whatever login the label points at NOW — which on that
    /// night was a completely different, healthy account serving turns
    /// continuously.
    ///
    /// Two things follow, and both are here rather than at the read side so the
    /// stored rows are coherent on their own:
    ///
    /// * The window is written to **every label of the login**, so two names for
    ///   one account can no longer disagree about whether it is limited.
    /// * Each row is stamped with the identity it was OBSERVED on
    ///   ([`crate::accounts::AccountState::limit_is_stale`]), so re-pointing a
    ///   label at a different login retires its limit instead of exporting it.
    ///
    /// `pool` is the quota the refusal named. It is recorded, not yet enforced:
    /// the bar is still account-wide.
    fn note_account_limit(&self, state: &Arc<ThreadState>, reset: Option<&str>, pool: Option<&str>) {
        let account = state.account.lock().unwrap().clone();
        self.note_account_limit_for(&account, reset, pool);
    }

    /// [`Self::note_account_limit`] addressed by ACCOUNT NAME rather than by a
    /// thread that happens to be sitting on it.
    ///
    /// The whole of the split. `note_account_limit` used the `Arc<ThreadState>`
    /// for exactly one thing — reading `state.account` — and that single
    /// dereference was the reason the `/usage` poller could not bar anything: a
    /// poller has an account name and no thread at all, so the only writer of
    /// `limit_until` in the daemon was unreachable from the one place that
    /// MEASURES exhaustion. On 2026-08-14 the poller read `weekly_all = 100.0`
    /// and had nowhere to put it; the bar was written thirteen minutes later by
    /// a 429 the daemon had already been told about.
    ///
    /// It is synchronous and takes no locks it does not immediately release, so
    /// it is safe to call from the poller's post-`spawn_blocking` path and from
    /// the stream-event interpreter alike.
    fn note_account_limit_for(&self, account: &str, reset: Option<&str>, pool: Option<&str>) {
        let account = account.to_string();
        // ── THE WINDOW HAS TO BE PLAUSIBLE BEFORE IT CAN BAR ANYTHING ────────
        //
        // `MAX_PARSED_RESET` guarded the retry ladder and nothing else. This
        // write — the one that actually takes an account out of service — put
        // whatever the prose resolved to straight into `accounts.limit_until`
        // with no bound at all, and on 2026-08-15 that was
        // `2027-08-17T22:49Z`: a YEAR-long bar on `owner-account`,
        // written from `Aug 17 at 5:49pm (America/Chicago)` by a resolver that
        // rolled a passed date forward by a year (`reset::ResetTime::resolve`,
        // now fixed). Five days of a healthy account — `session_pct=0.0`,
        // `weekly_pct=17.0` — refusing to serve a single turn, and nothing
        // could take it back: the only thing that clears a window early is a
        // completed turn on the account, and the bar is what prevents one.
        //
        // The producer is fixed; this is the second line of defence for the
        // whole CLASS, because the asymmetry here is brutal. A window that is
        // too SHORT costs one extra 429. A window that is too LONG costs the
        // account until the clock runs out, with no path back. Eight days
        // already covers the longest pool that exists (the seven-day weekly)
        // with a day of slack, so anything beyond it is not a limit we failed
        // to understand — it is a limit that cannot be real, and we refuse it
        // LOUDLY rather than acting on it.
        let now_local = crate::clock::now_local().naive_local();
        let resolved = reset
            .and_then(crate::reset::parse_reset)
            .and_then(|r| r.resolve(now_local));
        if let Some(t) = resolved {
            if t - now_local > MAX_PARSED_RESET {
                tracing::error!(
                    %account,
                    prose = reset.unwrap_or("<none>"),
                    resolved = %t,
                    max_days = MAX_PARSED_RESET.num_days(),
                    "REFUSING an implausible usage-limit window — this prose resolves \
                     further out than the longest pool that exists, so it is not a limit \
                     we can act on. The recorded window is left alone and the retry ladder \
                     owns the wait; barring an account on this would take it out of service \
                     with nothing able to bring it back"
                );
                return;
            }
        }
        // ONE MINUTE OF SLACK PAST THE INSTANT THE PROSE NAMED
        // ([`crate::reset::RESET_SLACK`]). The vendor does not lift a quota at the
        // instant it names: at 2026-10-06 03:19:17Z — seventeen seconds after the
        // reset this very prose named — `/usage` was still answering `session =
        // 100%`. Writing the named instant as the window meant the account
        // re-entered the pool at reset + 0s, took the stale exhausted reading, and
        // handed `reset::resolve` the one input it got wrong: a reset it had just
        // crossed. `804cffd` fixed that resolution; this removes the occasion for
        // it.
        let limit_until = resolved.map(|t| {
            let delta = t - now_local;
            (crate::clock::now() + delta + crate::reset::RESET_SLACK).to_rfc3339()
        });
        let Some(until) = limit_until else {
            // Unparseable (or absent) reset prose. Writing the `None` through
            // would CLEAR whatever live window this login already had and
            // un-bar it instantly — the retry ladder's backoff owns this case,
            // not the accounts table.
            tracing::warn!(
                %account,
                prose = reset.unwrap_or("<none>"),
                "a 429 gave no parseable reset time — leaving the recorded window alone \
                 and letting the retry ladder back off"
            );
            return;
        };
        let labels = self.record_account_limit(&account, &until, reset, pool);
        // An input to placement just moved. Every retrying thread re-asks now
        // rather than at whatever instant it happened to write down.
        self.account_picture_changed_for("limit_recorded", Some(&account));
        // P2. The LEVEL, not the event: a recorded, cleared and re-recorded
        // window all converge on one cell value, so a dropped fact cannot leave
        // a stale bar on a card. Emitted per LABEL, because a limit is written
        // per label and two labels can be one login.
        for label in &labels {
            self.emit_limit_fact(label);
        }
        // …and every turn STILL RUNNING on this login is running on an account
        // that has already refused. DEC-56's hard break, applied to the threads
        // that have not taken their own 429 yet (see
        // `ThreadSettings::limit_kill_mid_turn`).
        self.raise_limit_kill(&labels, reset, pool);
    }

    /// **Write a resolved bar on every label of the login** — the write half of
    /// [`Self::note_account_limit_for`], split out (A-4) so a bar whose instant
    /// is NOT reset prose can use the same write without the mid-turn kill.
    /// Returns the labels written. The CALLER rings the bell (with a literal
    /// reason — `placement_rerun_events` reads the reason set off the source)
    /// and emits the per-label limit facts.
    fn record_account_limit(
        &self,
        account: &str,
        until: &str,
        reset: Option<&str>,
        pool: Option<&str>,
    ) -> Vec<String> {
        let (labels, identity) = self.login_labels(account);
        for label in &labels {
            if let Err(e) = self.store.set_account_limit(
                label,
                Some((until, reset, pool, identity.as_deref())),
            ) {
                tracing::warn!(%label, error = %e, "recording an account limit failed");
            }
        }
        tracing::info!(
            %account, labels = ?labels, quota = pool.unwrap_or("<unnamed>"), %until,
            "usage limit recorded against this LOGIN (every label of it), stamped with the \
             identity it was observed on"
        );
        labels
    }

    /// **A `rate_limit_event` said this account is on PAID OVERAGE, or that
    /// the vendor REJECTED the request: bar the login for placement** (gap A-4).
    ///
    /// Before this, the runtime read one window's utilization off the event
    /// and dropped `status` and `isUsingOverage` (`..`), so a turn could run
    /// into paid overage — the TUI transcript even said "on overage" — while
    /// placement kept loading new work onto the account. Money is the reason
    /// this exists: an account on overage must not receive the NEXT placement.
    ///
    /// # It bars; it does NOT tear the running turn down
    ///
    /// Same asymmetry as the over-share clamp (`budgets.rs`, the operator's
    /// 2026-08-27 ruling): work already running finishes, NEW placement stops.
    /// So this writes through [`Self::record_account_limit`] and never reaches
    /// `raise_limit_kill`, which [`Self::note_account_limit_for`] ends with.
    ///
    /// # The window
    ///
    /// [`crate::stream::RateLimitPush::bar_reset`] — the latest reset among the
    /// windows reported exhausted, else the top-level `resetsAt` — plus
    /// [`crate::reset::RESET_SLACK`], because it IS a vendor-named reset. An
    /// absent, elapsed or implausible (> `MAX_PARSED_RESET`) instant falls back
    /// to [`crate::retry::CREDIT_BAR_SECS`], the crate's existing re-probe
    /// interval for a bar with no usable reset: unlike the 429 path, NOT
    /// writing would leave the account taking paid work.
    ///
    /// # The axis
    ///
    /// Overage is recorded as `limit_axis = "overage"`, and
    /// [`Self::note_account_served`] does not clear that axis: the turn that
    /// was running when the event arrived completes ON THE OVERAGE, so its
    /// completion proves the account serves, not that it serves for free.
    /// A `rejected` without overage is recorded on the window's own axis, where
    /// the existing falsifiers (a completed turn, a `/usage` reading under the
    /// ceiling) apply unchanged.
    ///
    /// Re-recording an identical-or-shorter bar of the same axis is skipped:
    /// the event arrives on every API call of a turn, and each write rings the
    /// placement bell.
    fn note_rate_limit_push(&self, state: &Arc<ThreadState>, push: &crate::stream::RateLimitPush) {
        if !push.bars() {
            return;
        }
        let account = state.account.lock().unwrap().clone();
        let now = crate::clock::now();
        let named = push
            .bar_reset()
            .and_then(|e| chrono::DateTime::from_timestamp(e, 0))
            .filter(|t| *t > now && *t - now <= MAX_PARSED_RESET);
        let until_t = match named {
            Some(t) => t + crate::reset::RESET_SLACK,
            None => now + chrono::Duration::seconds(crate::retry::CREDIT_BAR_SECS),
        };
        let axis: Option<String> = if push.is_using_overage {
            Some(OVERAGE_AXIS.to_string())
        } else {
            Axis::from_rate_limit_type(&push.rate_limit_type).map(|a| a.as_str().to_string())
        };
        // Already barred at least this long on this axis? Nothing new to say.
        let states = self.account_states();
        let covered = states.get(&account).is_some_and(|s| {
            s.limit_axis == axis
                && s
                    .limit_until
                    .as_deref()
                    .and_then(|u| chrono::DateTime::parse_from_rfc3339(u).ok())
                    .is_some_and(|u| u >= until_t)
        });
        if covered {
            return;
        }
        let reason = format!(
            "rate_limit_event: status={} isUsingOverage={} rateLimitType={}",
            push.status.as_deref().unwrap_or("<none>"),
            push.is_using_overage,
            push.rate_limit_type
        );
        tracing::warn!(
            thread = %state.id, %account, overage = push.is_using_overage,
            status = push.status.as_deref().unwrap_or("<none>"),
            reset_named = named.is_some(),
            "the CLI's rate_limit_event says this login is on paid overage or was refused — \
             barred for placement; the running turn is NOT torn down"
        );
        let labels =
            self.record_account_limit(&account, &until_t.to_rfc3339(), Some(&reason), axis.as_deref());
        // The same bell and the same facts as every other recorded bar.
        self.account_picture_changed_for("limit_recorded", Some(&account));
        for label in &labels {
            self.emit_limit_fact(label);
        }
    }

    /// **Order the mid-turn interrupt on every thread running on a barred
    /// login** — the WHO half of `ThreadSettings::limit_kill_mid_turn`.
    ///
    /// Level-triggered, exactly like `budget_kill` and `cancel_requested`: this
    /// raises a flag the tail loop is already polling every `tail_poll` tick,
    /// and does no teardown, no placement and no signalling of its own. The
    /// reset prose and the quota ride along on `limit_kill_signal` so the
    /// interrupted turn reaches `run_turn`'s 429 branch carrying the same two
    /// facts a real 429 would have carried.
    ///
    /// Threads with the flag turned off are skipped and NOT logged per-call —
    /// opting out is a configuration, not an event.
    fn raise_limit_kill(&self, labels: &[String], reset: Option<&str>, pool: Option<&str>) {
        let states: Vec<Arc<ThreadState>> =
            self.threads.lock().unwrap().values().cloned().collect();
        for s in &states {
            if !s.turn_running.load(Ordering::SeqCst) {
                continue;
            }
            let on = s.account.lock().unwrap().clone();
            if !labels.iter().any(|l| *l == on) {
                continue;
            }
            if !self.limit_kill_enabled(&s.id) {
                continue;
            }
            *s.limit_kill_signal.lock().unwrap() = Some(LimitKill {
                reset: reset.map(str::to_string),
                pool: pool.map(str::to_string),
                account: on.clone(),
            });
            if !s.limit_kill.swap(true, Ordering::SeqCst) {
                tracing::warn!(
                    thread = %s.id, account = %on, quota = pool.unwrap_or("<unnamed>"),
                    "MID-TURN INTERRUPT ordered: this turn is running on a login that has \
                     just been barred, so it is torn down and handed back to admission \
                     rather than left to discover the limit for itself"
                );
            }
        }
    }

    /// Is the mid-turn interrupt enabled for this thread? Defaults to TRUE —
    /// including when the settings blob cannot be read, because a thread whose
    /// configuration is unreadable is exactly the one that must not be left
    /// running on an exhausted account.
    fn limit_kill_enabled(&self, thread_id: &str) -> bool {
        match self.store.get_thread(thread_id) {
            Ok(Some(row)) => ThreadSettings::from_stored(&row.settings_json)
                .map(|s| s.limit_kill_mid_turn)
                .unwrap_or(true),
            _ => true,
        }
    }

    /// **The `/usage` poll MEASURED this quota below its ceiling, so a bar
    /// recorded on that same quota is falsified.** The clearing half of the
    /// poller's bar.
    ///
    /// # Why this had to exist
    ///
    /// Before it, `accounts.limit_until` had exactly two early clear paths and a
    /// false bar could reach neither. `note_account_served` needs a COMPLETED
    /// TURN, and the bar is precisely what prevents one — the circularity is
    /// named in that method's own doc comment and was treated as covered.
    /// `note_login_restored` clears only `limit_axis == "login"`. So a quota bar
    /// written in error had no exit but its own clock, and on 2026-10-06 that
    /// clock was a full day long: `reset::ResetTime::resolve` rolled a session
    /// reset that had passed by twenty-three seconds forward by 24 hours (now
    /// fixed at the producer), and this poller then watched the account report
    /// `session = 8%` of a fresh window and had no way to say so. The operator
    /// lost his working account for the evening.
    ///
    /// # What makes it safe to act on
    ///
    /// It is the SAME READING that writes the bar, from the same `/usage`
    /// response, compared against the same ceiling. If a measured `pct >=
    /// ceiling` is good enough to take an account out of service — and
    /// `note_account_limit_for` says it is, because the percent is reported
    /// rather than estimated — then a measured `pct < ceiling` is good enough to
    /// put it back. Believing the number in one direction only is not caution,
    /// it is a ratchet.
    ///
    /// # What it will NOT touch
    ///
    /// Only a bar recorded on THIS axis. `limit_axis` is matched exactly, so:
    ///
    /// * a `login` bar survives — an expired credential is not falsified by a
    ///   quota reading, and only a successful login lifts it
    ///   ([`Self::note_login_restored`]);
    /// * a `credit` bar survives — an empty balance is not a quota window and
    ///   ends when a human tops the account up
    ///   ([`Self::note_account_credit_exhausted`]);
    /// * a bar recorded on a DIFFERENT quota survives — `session` measuring 8%
    ///   says nothing about `weekly_all`, and clearing across axes would let a
    ///   fresh session unlock a genuinely exhausted week.
    /// * a bar with no `limit_axis` at all survives. Those are rows written
    ///   before the column existed, or 429s whose prose named no quota; we
    ///   cannot tell what they are about, and a bar we cannot attribute keeps
    ///   barring — the same direction [`crate::accounts::AccountState::limit_is_stale`]
    ///   takes, and for the same reason.
    ///
    /// Written per LABEL across the login, exactly as the bar is, so a clear is
    /// never narrower than the bar it removes.
    ///
    /// # There are no per-thread retry floors left to poke
    ///
    /// A 429 used to write TWO things out of one piece of evidence — this bar, and a
    /// RETRY FLOOR on `retries.retry_at` derived from the same reset prose. Clearing
    /// the first and not the second is why three threads stayed parked on
    /// 2026-10-06 after this path had already worked, and the obvious repair was to
    /// re-arm those floors from here.
    ///
    /// **That repair was written, proved, and then deleted on the operator's
    /// ruling** — and the ruling has since been carried out: the second derivation
    /// does not exist any more, so there is nothing left to fall out of step with
    /// this bar. The reasoning is kept because it is the reasoning that removed it:
    ///
    /// > *"note that the parked status means that there is a pending turn and it
    /// > has not been placed. There should not be a per-thread retry timestamp.
    /// > And there are certain events which tell placement to rerun the full
    /// > placement algorithm — e.g. new_account, limit_expired, and others. We
    /// > just need to make sure that those events are complete."*
    ///
    /// So a falsified bar owes placement an EVENT, not a sweep over per-thread
    /// clocks — and it already emits one: `account_picture_changed_for` below is
    /// that event. **The other side of it is now closed too**: the durable answer
    /// named here — *"for the floor to stop existing rather than for one more
    /// writer to reach in and adjust it"* — is what happened. There is no
    /// per-thread floor and no cheap pre-guess at the account picture left to
    /// refuse this event; the bell reconsiders every parked prompt and the gate,
    /// reading this cleared bar, decides.
    fn note_account_quota_measured_open(&self, account: &str, axis: gauge::Axis, pct: f64) {
        let states = self.account_states();
        let (labels, _identity) = self.login_labels(account);
        // Only act when there is something to falsify — this runs on every
        // reading of every axis, and a no-op must not ring the placement bell.
        let falsified: Vec<String> = labels
            .iter()
            .filter(|l| {
                states.get(*l).is_some_and(|s| {
                    s.limit_until.is_some() && s.limit_axis.as_deref() == Some(axis.as_str())
                })
            })
            .cloned()
            .collect();
        if falsified.is_empty() {
            return;
        }
        for label in &falsified {
            if let Err(e) = self.store.set_account_limit(label, None) {
                tracing::warn!(
                    %label, error = %e,
                    "clearing a quota bar the /usage poll just falsified failed"
                );
            }
        }
        tracing::info!(
            %account, labels = ?falsified, axis = axis.as_str(), %pct,
            ceiling = self.config.gauge_bar_pct,
            "the /usage poll MEASURED this quota below its ceiling while a bar was recorded \
             on it — the recorded window was wrong, so it is cleared on every label of this \
             login rather than left to expire on a clock nothing could contradict"
        );
        // Capacity just appeared where placement believed there was none.
        self.account_picture_changed_for("limit_falsified_by_gauge", Some(account));
        for label in &falsified {
            self.emit_limit_fact(label);
        }
    }

    /// **A turn completed on this account, so the login is not limited.**
    ///
    /// The counterpart to [`Self::note_login_health`]'s success branch, and the
    /// reason the 2026-08-02 incident cannot last five hours again. Before this,
    /// `note_account_limit` was the ONLY writer of `limit_until` in the daemon:
    /// a window went in when a 429 arrived and came out only when its own clock
    /// ran out. Nothing could contradict it. The `/usage` poll watched the very
    /// same login report 29% every fifteen minutes for five hours and had no way
    /// to say so, and turns kept completing on it the whole time.
    ///
    /// A completed turn is the strongest evidence the daemon ever gets that an
    /// account can serve, and it is evidence about the LOGIN, so it clears the
    /// window on every label of it. The clear is exactly as wide as the bar it
    /// removes: while `limit_axis` is recorded, the bar itself is still
    /// account-wide, so a success falsifies all of it. If a narrower quota is
    /// genuinely exhausted the next refusal on it re-records the bar — one 429,
    /// against hours of a healthy login being believed dead.
    fn note_account_served(&self, state: &Arc<ThreadState>) {
        let account = state.account.lock().unwrap().clone();
        let states = self.account_states();
        let (labels, identity) = self.login_labels(&account);
        // A COMPLETED TURN PROVES THE CREDENTIAL, so the login-bar ladder goes
        // back to its bottom rung. Done BEFORE the early return below: the
        // streak is about the login, not about whether a bar happens to be
        // recorded right now, and a login that recovers and later fails again
        // must get the prompt 60-second recheck rather than resuming at ten
        // minutes.
        self.reset_login_bar_streak(&account, identity.as_deref());
        // A completed turn proves the store authenticates, so a wiped reading
        // on this slot is stale.
        if self.clear_sign_in_needed(&account) {
            self.account_picture_changed_for("slot_signed_in", Some(&account));
        }
        // Only act when there is something to falsify — this runs after every
        // successful turn, and a no-op must not ring the placement bell.
        //
        // An OVERAGE bar is not falsified by a completed turn (A-4): the turn
        // that completes is typically the very one that ran onto the overage,
        // and it served on paid usage. See `note_rate_limit_push`.
        let clearable: Vec<String> = labels
            .iter()
            .filter(|l| {
                states.get(*l).is_some_and(|s| {
                    s.limit_until.is_some() && s.limit_axis.as_deref() != Some(OVERAGE_AXIS)
                })
            })
            .cloned()
            .collect();
        if clearable.is_empty() {
            return;
        }
        for label in &clearable {
            if let Err(e) = self.store.set_account_limit(label, None) {
                tracing::warn!(%label, error = %e, "clearing a falsified account limit failed");
            }
        }
        tracing::info!(
            %account, labels = ?labels,
            "a turn COMPLETED on this login, so the recorded usage-limit window was wrong — \
             cleared on every label of it rather than left to expire on a stale clock"
        );
        // Capacity just appeared where placement believed there was none.
        self.account_picture_changed_for("limit_cleared", Some(&account));
        // P2, the clearing half.
        for label in &labels {
            self.emit_limit_fact(label);
        }
    }

    /// Drop a login's consecutive-auth-failure streak. See
    /// [`Self::login_bar_streak`].
    fn reset_login_bar_streak(&self, account: &str, identity: Option<&str>) {
        let key = identity.map(str::to_string).unwrap_or_else(|| account.to_string());
        self.login_bar_streak.lock().unwrap().remove(&key);
    }

    /// **A SUCCESSFUL LOGIN REOPENS THE ACCOUNT, IMMEDIATELY. This is the hook
    /// the login flow calls.**
    ///
    /// # The ruling
    ///
    /// > *"if we're asking for the login codes from threads by using that
    /// > [force] function I just mentioned, then we'll know when the login has
    /// > succeeded and you can drop that flag and immediately add that account
    /// > to the pool as available."*
    ///
    /// The login is going to be issued THROUGH threads, so threads observes the
    /// success directly. There is nothing to wait for and nothing to confirm:
    /// no timer, no self-attestation, no probe to schedule. The flow that saw
    /// the login succeed calls this, and the account is selectable on the next
    /// placement decision.
    ///
    /// # What calls it
    ///
    /// The login-issuing flow is a separate piece of work and is NOT built
    /// here. This is the hook it will call, and it is deliberately complete and
    /// callable today:
    ///
    /// - in-process: this method;
    /// - from the CLI: `threads accounts login-restored <account>`;
    /// - over the wire: the `login_restored` WS request (`server.rs`).
    ///
    /// # It clears the LOGIN bar and nothing else
    ///
    /// The discipline this whole audit is about. A successful `/login` is
    /// evidence about the CREDENTIAL. It says nothing whatever about quota, so
    /// a bar whose `limit_axis` is `usage`/`weekly`/`session`/`credit` is left
    /// exactly where it is. Clearing those would be the same class-loss in
    /// reverse: treating one axis's good news as another's.
    ///
    /// `note_account_served` may clear a quota bar because a COMPLETED TURN
    /// genuinely falsifies one — the account served, so it was not out of
    /// quota. A login does not.
    ///
    /// # The automatic retries are kept
    ///
    /// > *"we should still do the automatic tries, you know, just as a backup."*
    ///
    /// Nothing here removes the expiring bar or the ladder. This is a fast path
    /// that lands in front of them; if it is never called, the account still
    /// comes back on its own exactly as before.
    ///
    /// Returns the labels it reopened — empty when the login had no bar to lift,
    /// which is a normal, quiet outcome.
    /// **Emit a PROGRESS frame** for a long account operation.
    ///
    /// Transient and firehose-only, like liveness. It exists so a login or a
    /// rotation can say something while it runs WITHOUT the caller polling and
    /// without the client tailing a log file — which is literally what the
    /// rotation view did: it captured a byte offset into `accounts/rotation.log`
    /// and regex-parsed another program's prose every 150 milliseconds.
    pub fn emit_progress(
        &self,
        op: &str,
        id: &str,
        account: Option<&str>,
        phase: &str,
        detail: Option<String>,
        done: bool,
        ok: Option<bool>,
    ) {
        // A send failure means nobody is subscribed, which is normal and not
        // worth a log line — the same policy every other push here follows.
        self.hub.emit(Push::Progress {
            op: op.to_string(),
            id: id.to_string(),
            account: account.map(str::to_string),
            phase: phase.to_string(),
            detail,
            done,
            ok,
        });
    }

    /// Where `scripts/add-account` lives, relative to the data dir.
    ///
    /// The login flow DRIVES that script rather than reimplementing it: its
    /// credential guards (it refuses when `CLAUDE_CODE_OAUTH_TOKEN` is set,
    /// because `claude` then deletes the machine-default keychain item on exit),
    /// its distinct-account verdicts and its conformance vectors are all
    /// load-bearing, and none of them are restated in Rust.
    pub fn add_account_script(&self) -> std::path::PathBuf {
        self.data_dir.join("scripts").join("add-account")
    }

    /// The daemon's data dir. Exposed so WS arms that answer questions about
    /// the ACCOUNTS DIRECTORY — health, rename, delete, rotation — can reach the
    /// same tree the CLI reads, rather than each re-deriving it from config and
    /// drifting.
    /// The managed subagent runtime's handle (`crate::managed`).
    pub fn managed(&self) -> &crate::managed::Managed {
        &self.managed
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    pub fn note_login_restored(&self, account: &str) -> Vec<String> {
        self.note_login_restored_checked(account).1
    }

    /// **[`Self::note_login_restored`], GATED ON THE RE-CHECK (R12.5b).**
    ///
    /// A reported login used to clear the sign-in set, the login bar and
    /// `dead_since` on the caller's word — and both doors that report one (the
    /// daemon's own `accounts/login/{id}/complete` and the
    /// `accounts/{account}/login_restored` WS arm, open to any client) cleared
    /// with no identity read at all (R12.10 inspect (e)). Now the report is a
    /// TRIGGER, not evidence: R12.10's [`crate::accounts::recheck_flags`] runs
    /// first — the trusted server read and the slot probe — and only a
    /// `verified` identity lets anything below clear. Any other verdict
    /// (crossed, unreadable, a disabled account that is not read) clears
    /// nothing: the flags are kept with their reason, and the re-check's own
    /// `flags_kept` says why. Returns the re-check and the labels reopened.
    pub fn note_login_restored_checked(
        &self,
        account: &str,
    ) -> (crate::accounts::FlagRecheck, Vec<String>) {
        let states = self.account_states();
        let sign_in_before = self
            .account_unavailable_reasons(account)
            .unwrap_or_default()
            .iter()
            .any(|w| w.starts_with("login_") && w != "login_dead");
        let check =
            crate::accounts::recheck_flags(self, account, crate::accounts::RecheckTrigger::Login);
        if check.identity_state != Some(crate::accounts::IdentityState::Verified) {
            tracing::warn!(
                %account,
                identity = ?check.identity_state,
                reason = check.identity_reason.as_deref().or(check.skipped.as_deref()).unwrap_or(""),
                "a login was reported, but the re-check did not verify it — NOTHING was cleared"
            );
            return (check, Vec::new());
        }
        (check, self.lift_login_flags_verified(account, states, sign_in_before))
    }

    /// The clears a VERIFIED login earns (the body `note_login_restored` always
    /// had). `states` / `sign_in_before` are read before the re-check, so what
    /// it already lifted is still reported as reopened.
    fn lift_login_flags_verified(
        &self,
        account: &str,
        states: std::collections::BTreeMap<String, AccountState>,
        sign_in_before: bool,
    ) -> Vec<String> {
        let (labels, identity) = self.login_labels(account);
        // The ladder goes back to its bottom rung whether or not a bar was
        // recorded — this login just proved it authenticates.
        self.reset_login_bar_streak(account, identity.as_deref());
        // A wiped slot that was just signed in is placeable again NOW.
        let was_wiped = self.clear_sign_in_needed(account) || sign_in_before;

        let mut reopened = Vec::new();
        for label in &labels {
            // ONLY a login bar. `limit_axis` is the axis the bar was written
            // on, and it is the whole reason this can be safe.
            let is_login_bar = states
                .get(label)
                .is_some_and(|s| s.limit_until.is_some() && s.limit_axis.as_deref() == Some("login"));
            if !is_login_bar {
                continue;
            }
            match self.store.set_account_limit(label, None) {
                Ok(()) => reopened.push(label.clone()),
                Err(e) => {
                    tracing::warn!(%label, error = %e, "clearing a login bar after a successful login failed")
                }
            }
        }

        // `dead_since` is the permanent form of the same fact, and a successful
        // login falsifies it too. Older daemons could write it for auth text;
        // clearing it here is what makes this hook a complete answer to "this
        // account is locked out" rather than a partial one.
        let was_dead = states.get(account).map(|s| !s.healthy()).unwrap_or(false);
        if was_dead {
            if let Err(e) = self.store.set_account_health(account, None) {
                tracing::warn!(%account, error = %e, "clearing account health after a successful login failed");
            }
        }

        if reopened.is_empty() && !was_dead && !was_wiped {
            tracing::info!(
                %account,
                "a successful login was reported for an account that was not barred for its \
                 login — nothing to reopen"
            );
            // ── THIS PATH USED TO RETURN IN SILENCE, AND THAT WAS A GAP ──
            //
            // The operator's ask was *"Login authenticates — emit"*, and this
            // is the ordinary shape of a successful login: he re-authenticates
            // an account BEFORE it expires, so nothing was barred and there is
            // nothing to reopen. Every announcement below was inside the "we
            // actually lifted a bar" branch, so the common case — the one where
            // he is being diligent — announced nothing at all, and the board
            // kept showing the old credential expiry until he pressed refresh.
            //
            // Something DID change and it is on screen: the login's expiry, its
            // health row, and, for a label that had no directory a moment ago,
            // its very existence. So it announces.
            //
            // A DIFFERENT REASON on purpose. `login_restored` means "a bar was
            // lifted and this account is placeable again NOW"; claiming that
            // here would tell a client capacity appeared when none did. This is
            // `login_refreshed` — the credential moved, nothing about placement
            // did — and it rings [`Self::account_picture_changed_for`]'s bell
            // for the retry sweep for the same reason: nothing about placement
            // moved, so there is nothing to re-ask.
            self.emit_info(
                "accounts",
                "login_refreshed",
                Some(account),
                Some(format!("{account} signed in again")),
            );
            // P4 — the EARLY exit too. Rev 3 named only the `login_restored`
            // tail; a login that signs in again without a bar to lift still
            // changed its credential, and a shape that heard nothing here would
            // keep showing the old expiry.
            self.emit_health_fact(account);
            self.sample_credentials();
            return reopened;
        }

        tracing::info!(
            %account, reopened = ?reopened, cleared_dead_flag = was_dead,
            "LOGIN RESTORED — the login bar was lifted on every label of this login and the \
             account is available for placement NOW, without waiting for the recheck window. \
             Any quota bar on it is deliberately untouched: a login proves the credential, \
             not the quota"
        );
        // Capacity just appeared where placement believed there was none — every
        // retrying thread re-asks immediately rather than on the next tick.
        self.account_picture_changed_for("login_restored", Some(account));
        // P4 / P6.
        self.emit_health_fact(account);
        for label in &labels {
            self.emit_limit_fact(label);
        }
        self.sample_credentials();
        reopened
    }

    /// Fold a turn outcome into the account's LOGIN HEALTH.
    ///
    /// `failure` is the terminal error text, or `None` for a turn that
    /// completed. Only a failure that PROVES the login is dead
    /// ([`crate::accounts::is_dead_login_error`] — an OAuth session that cannot
    /// be refreshed needs a human `/login`, not a retry) sets the flag; a rate
    /// limit, an overload and an exhausted credit balance deliberately do not.
    /// A successful turn clears it — but a dead login is unavailable, so no turn
    /// is spawned there; what clears it in practice is a Live reading of its
    /// slot ([`Self::revive_dead_login_on_probe`], R9.A3) or a reported login.
    ///
    /// While the flag is set the account is not a selection candidate and not a
    /// failover candidate, and it stays fully visible in `accounts status` with
    /// the instant and the verbatim reason — a dead account the operator cannot
    /// see is worse than a dead account.
    fn note_login_health(&self, state: &Arc<ThreadState>, failure: Option<&str>) {
        let account = state.account.lock().unwrap().clone();
        let dead_now = self
            .account_states()
            .get(&account)
            .map(|s| !s.healthy())
            .unwrap_or(false);
        match failure {
            Some(text) if crate::accounts::is_dead_login_error(text) => {
                if dead_now {
                    return;
                }
                tracing::error!(
                    %account,
                    "this login is DEAD (authentication cannot be refreshed) — not available: \
                     out of placement, and no turn runs on it, until `claude-accounts login \
                     {account}`; signed in again, it returns within one recheck/sample"
                );
                if let Err(e) = self
                    .store
                    .set_account_health(&account, Some((&crate::store::now_rfc3339(), text)))
                {
                    tracing::warn!(%account, error = %e, "recording account health failed");
                }
                // A login left the pool: every retry floored on it can stop
                // waiting for it, and every other retry has one fewer place to
                // go. Either way placement's inputs moved.
                self.account_picture_changed_for("login_dead", Some(&account));
                self.emit_health_fact(&account); // P4
            }
            None if dead_now => {
                self.revive_dead_login(&account, "a turn completed on it");
            }
            _ => {}
        }
    }

    // ── liveness heartbeat (ported from CCW; broadcast-only) ─────────────

    /// A liveness beat every `heartbeat` while the turn's process runs; it is
    /// aborted when the process exits. It REPORTS and decides nothing: it no
    /// longer sweeps agents and no longer outlives the turn on `busy()` — the
    /// `Liveness::busy()` poll, and the mtime sweep it drove, were deleted in
    /// R4.S2. It still READS `busy()` every beat, one call down in
    /// `emit_liveness`, to put it on the frame; its exit is `turn_running`,
    /// never `busy()` (`tests/managed_runtime.rs` follows the call to check).
    fn spawn_heartbeat(&self, state: &Arc<ThreadState>) -> tokio::task::JoinHandle<()> {
        let mgr = self.clone();
        let sid = state.id.clone();
        let interval = self.tunables.heartbeat;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let st = match mgr.threads.lock().unwrap().get(&sid).cloned() {
                    Some(s) => s,
                    None => break,
                };
                mgr.emit_liveness(&st);
                if !st.turn_running.load(Ordering::SeqCst) {
                    break;
                }
            }
        })
    }

    /// **THE TURN'S PROCESS HAS EXITED** — a machine fact, and the one that
    /// replaced the mtime sweep (R4.S2). A native async agent runs INSIDE the
    /// `claude` process, so once that process is gone none of its agents can
    /// still be running: each one still listed is completed here, with an
    /// `AgentCompleted` the ledger can tell apart from the CLI's own
    /// `task_notification`. Before this, the listing was cleared by
    /// `sweep_subagents` guessing from an output file's mtime (~9 spurious
    /// completions per real one, `TaskNotification`'s arm records), polled by
    /// `poll_subagents_until_idle` for as long as `busy()` stayed true.
    fn process_exited(&self, state: &Arc<ThreadState>) {
        let (gone, tasks_changed) = {
            let mut l = state.live.lock().unwrap();
            l.parent_running = false;
            let mut v: Vec<String> = l.outstanding.drain().map(|(k, _)| k).collect();
            v.sort();
            // R9.TS2: a subagent still `active` when its process is gone was
            // ended by nothing that said so — `error: interrupted`.
            (v, l.tasks.process_exited(&crate::store::now_rfc3339()))
        };
        if tasks_changed {
            self.emit_subagents(state);
        }
        for agent_id in gone {
            self.emit(state, None, ThreadEvent::AgentCompleted { agent_id });
        }
    }

    // ── boot recovery (G1/G10/G15) ───────────────────────────────────────

    /// Run once at daemon start: sweep stale statuses, re-adopt or bury
    /// in-flight turns, re-arm persisted retries, re-dispatch waiting queues.
    pub async fn recover(&self) {
        // Before anything can place a turn (the watchers, the managed wakes
        // below): which slots are ALREADY wiped. See `seed_sign_in_needed`.
        self.seed_sign_in_needed();
        self.start_watchers();

        // The fleet-wide default budget every thread inherits (INTENT 72b).
        // Created here rather than on first use so that "what did this thread
        // cost" is answerable from the daemon's first second, and so the row
        // exists before any turn is charged against it. It never overwrites a
        // ceiling, so a limit the operator has since put on it survives every
        // restart.
        //
        // IT ALSO RECONCILES THAT KEY'S SCOPE TO THE POOL. An account the
        // operator added since the last boot joins the fleet-wide scope here or
        // it is invisible to every keyed thread until it does — which is exactly
        // what happened to `bob-example-com` (see `Budgets::ensure_default`).
        //
        // **THIS IS NO LONGER THE ONLY MOMENT IT HAPPENS, AND IT USED TO SAY IT
        // WAS.** The reason given here was that `self.pool` is built once at
        // construction and read-only for the process's life, so boot is the only
        // instant the pool can have changed. `PoolCell` made that false:
        // `reload_pool` re-derives the roster from disk on a rename, a delete, a
        // completed login and once per watcher tick, and calls this same
        // function on the changed path for the same reason. Boot is now the
        // FIRST reconciliation rather than the only one — still needed, because
        // the roster can also have moved while the daemon was DOWN, which no
        // reload will ever observe.
        let pool = self.account_pool();
        match self.budgets.ensure_default(&pool) {
            // `ensure_default` logs the specific line — created, or widened and
            // with which names — so there is nothing accurate to say here that
            // it has not already said more precisely.
            Ok(_) => {}
            Err(e) => tracing::warn!(
                error = %e,
                "could not seed the default budget; threads keyed to it run ungated until it exists"
            ),
        }

        // THE MIGRATION `ensure_default`'s never-overwrite rule made necessary.
        // That rule is why the `aui:default` row on this machine still listed
        // the literal account `"default"` in its scope long after the concept
        // left placement: the row was written when the machine default was a
        // registered account, and boot has been correctly declining to touch it
        // ever since. Placement did not care; `budgets.get` showed it to the
        // operator, which is the part that mattered.
        //
        // The NAME is supplied here, not inside `budgets`. That module is
        // barred by source guard from naming the machine default in executable
        // code, and correctly so — a module that names it to remove it still
        // knows it. `budgets` knows how to drop a name from a scope; this is
        // the one place that knows which name, and it is the same place that
        // already knows the concept exists in order to keep it out of the pool.
        match self
            .budgets
            .purge_account_from_scopes(crate::accounts::DEFAULT_ACCOUNT, &pool)
        {
            Ok(keys) if !keys.is_empty() => tracing::info!(
                ?keys,
                "migrated the machine default out of these budgets' account scopes"
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!(
                error = %e,
                "could not migrate the machine default out of the budget scopes; it will still \
                 appear in `budgets/<key>/get` until this succeeds"
            ),
        }

        // G15's `sweep_stale_busy` used to run here, resetting every `busy`
        // thread row to `idle` at boot. There is no status column to sweep any
        // more — see `store::DROPPED_COLUMNS`. A derived answer cannot go stale
        // across a restart, so there is nothing for a boot-time repair to do.

        // THREAD LOCKS left by a daemon that is gone. The lazy reclaim in
        // `lock_turn` would handle each of these on its next dispatch, but
        // sweeping them here means `threads.status` tells the truth about
        // every thread from the first second of the process, not just the ones
        // somebody has since sent to. Locks whose holder is ALIVE are left
        // strictly alone — that is the whole reclaim rule.
        //
        // Judged by `lock_is_live`, the SAME predicate `lock_turn` and
        // `thread_states` use, asked with `turn_running: false` because this
        // process has not started a turn yet. (Its `boot_id` arm cannot fire
        // here at all: our boot id is a uuid minted seconds ago, so no row on
        // disk can bear it.)
        for row in self.store.list_thread_locks().unwrap_or_default() {
            if !self.lock_is_live(&row, false) {
                match self.store.release_thread_lock(&row.thread_id, &row.boot_id) {
                    Ok(true) => tracing::info!(
                        thread = %row.thread_id, holder = %row.boot_id,
                        holder_pid = row.daemon_pid,
                        "reclaimed a thread lock left by a dead daemon"
                    ),
                    Ok(false) => {}
                    Err(e) => tracing::warn!(error = %e, "sweeping a stale thread lock failed"),
                }
            } else {
                tracing::warn!(
                    thread = %row.thread_id, holder = %row.boot_id,
                    holder_pid = row.daemon_pid,
                    "another LIVE daemon holds this thread's lock — leaving it alone"
                );
            }
        }

        // ── SH-A5: ATTACHED SESSIONS THAT OUTLIVED THE PREVIOUS DAEMON ──────
        //
        // An `attached_sessions` row (SH-A1) is an interactive claude the
        // operator is typing in, leased the thread by `attach` (SH-A2). The
        // sweep just above released the dead daemon's lock rows — including
        // the ones those leases held — so this pass decides each row by its
        // OWN pids (`attach::session_alive`, the reaper's predicate):
        //
        // * alive (or no pid recorded, so nothing to judge it by): the thread's
        //   lock is TAKEN BACK under this daemon and the lease parked in the
        //   `attached` map, so a detach or the reaper later releases it
        //   exactly as if this daemon had attached it;
        // * every recorded pid dead: the row is DELETED (absence = gone, as
        //   SH-A4's detach), the attach's turn closed and reconciled into the
        //   ledger, its files removed, and a lock no live holder owns released.
        //
        // NEVER KILLS: the operator is typing in that claude. This pass reads
        // pids (`kill(pid, 0)`) and sends no signal in any branch.
        //
        // AFTER the sweep, deliberately: the sweep asks `lock_is_live` with
        // `turn_running: false`, which would release a lock this pass had just
        // taken under OUR boot id. BEFORE the `running_turns` pass and the
        // re-dispatches below, so nothing queued on an attached thread can be
        // started alongside the operator's session. Its store and ledger reads
        // run on the blocking pool, not on this async worker (R9.C7i).
        {
            let mgr = self.clone();
            match tokio::task::spawn_blocking(move || mgr.recover_attached_sessions()).await {
                Ok(_) => {}
                Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                Err(e) => tracing::error!(error = %e, "the attached-session recovery pass did not run"),
            }
        }

        // In-flight turns from the previous daemon run (G1).
        for row in self.store.list_running_turns().unwrap_or_default() {
            let Ok(state) = self.get_state(&row.thread_id) else {
                let _ = self.store.clear_running_turn(&row.thread_id);
                continue;
            };
            if pid_alive(row.pid) {
                // ENTRY POINT: boot re-adoption. The child from the previous
                // daemon is STILL RUNNING this thread, so the lock has to be
                // held for it — otherwise the first send after a restart would
                // dispatch a second execution alongside the orphan.
                let Ok(lock) = self.lock_turn(&state, "adopt") else {
                    tracing::error!(
                        thread = %row.thread_id, pid = row.pid,
                        "cannot re-adopt: another live daemon holds this thread's lock"
                    );
                    continue;
                };
                tracing::info!(thread = %row.thread_id, pid = row.pid, "re-adopting running turn");
                {
                    let mut l = state.live.lock().unwrap();
                    l.parent_running = true;
                }
                let mgr = self.clone();
                tokio::spawn(async move {
                    mgr.adopt_turn(state, row, lock).await;
                });
            } else {
                self.emit(
                    &state,
                    None,
                    ThreadEvent::TurnInterrupted {
                        reason: format!(
                            "daemon restarted mid-turn; child pid {} is gone (spawn cookie {})",
                            row.pid, row.spawn_cookie
                        ),
                        trigger: InterruptTrigger::DaemonRestart.as_str().into(),
                    },
                );
                let _ = self.store.clear_running_turn(&row.thread_id);
                // ── L-1: THE INTERRUPTED DELIVERY GOES ON THE LADDER ──────────
                //
                // `retry::ends_the_delivery(DaemonRestart)` is `false`: a restart
                // is a GATE, not an intention, and a gate must never end the
                // delivery it cut off. Until this block existed the rule was
                // stated and not obeyed here — the row was deleted, the queue
                // rows stayed `delivered` (the stranded sweep skips them because
                // a `turn_started` names them), the thread read idle, and nobody
                // answered him.
                //
                // Continues the ladder the dead execution was on (`attempt + 1`)
                // under the SAME count ceiling an unclassified mid-execution
                // death gets in `run_turn` (`ErrorDuringExecution`), so a turn
                // that keeps dying across restarts cannot loop forever; at the
                // ceiling it ends through `give_up_and_keep_the_prompt` —
                // `failed` queue rows plus a visible `TurnFailed`.
                //
                // A pre-column row (`prompt: None`) has nothing to hand the
                // ladder; it keeps the old behaviour and says so.
                match row.prompt.as_deref() {
                    Some(prompt) => {
                        let kind = TransientKind::ErrorDuringExecution;
                        let reason = format!(
                            "the service restarted while this turn was running (child pid {} \
                             is gone); its delivery resumes — the prompt is re-sent only if \
                             it never reached the model (R13.1)",
                            row.pid
                        );
                        if row.attempt >= self.attempt_ceiling(&state, &kind) {
                            self.give_up_and_keep_the_prompt(
                                &state,
                                &row.message_ids,
                                &kind,
                                format!(
                                    "retries exhausted after {} attempts — the last one was cut \
                                     off by a service restart; this turn's text has been kept \
                                     and can be re-sent",
                                    row.attempt
                                ),
                            );
                        } else {
                            self.schedule_retry(
                                &state,
                                prompt,
                                &row.message_ids,
                                row.attempt + 1,
                                &kind,
                                &reason,
                                None,
                                None,
                            );
                        }
                    }
                    None => tracing::warn!(
                        thread = %row.thread_id, pid = row.pid,
                        "a turn cut off by a restart left no stored prompt (pre-column row); \
                         it cannot be re-delivered from here"
                    ),
                }
            }
        }

        // ── PERSISTED PARKED PROMPTS (G10) — NOTHING IS RE-ARMED HERE ANY MORE ──
        //
        // This used to spawn one timer per retry row, staggering the past-due ones
        // (10s + 5s each) so a boot with N floors did not stampede N spawns. Both
        // the fleet of timers and the stagger are gone with the per-thread
        // timestamp: the POOL has one cadence, `cadence_rung` reads it off the live
        // rows, and the placement task arms it on its very first loop iteration.
        // One timer cannot stampede, so there is nothing to stagger.
        //
        // What boot still does is teach an UNATTRIBUTED row which login refused it.
        // A row written before the `account` column existed does not know, and the
        // answer is knowable here and only here: a retry row exists precisely
        // because the gate did NOT move the thread, so the account it is still
        // sitting on is the account that refused it. It is no longer eligibility
        // (nothing gates on it) — it is the name a starving delivery reports.
        let mut adopted = 0usize;
        for row in self.store.list_retries().unwrap_or_default() {
            if row.account.is_some() {
                continue;
            }
            let sitting_on = self
                .get_state(&row.thread_id)
                .ok()
                .map(|s| s.account.lock().unwrap().clone());
            if let Some(account) = sitting_on {
                tracing::info!(
                    thread = %row.thread_id, %account,
                    "adopting a parked prompt written without its account; that is the login \
                     it was refused by"
                );
                let _ = self.store.upsert_retry(&RetryRow {
                    account: Some(account),
                    ..row
                });
                adopted += 1;
            }
        }
        if adopted > 0 {
            tracing::info!(adopted, "parked prompts taught which login refused them");
        }
        // **BOOT IS WHERE THE POOL LEARNS WHAT IS PARKED.** This is the one re-arm
        // that replaced the fleet of per-thread timers `recover` used to spawn: ring
        // the cadence so the placement task computes a rung from the rows that were
        // on disk, instead of waiting for the next thing to happen to it.
        self.cadence_rearm.notify_one();

        // STRANDED DELIVERIES (Q4's last hole). A dispatch marks its queue rows
        // `delivered` and only then — across a `tokio::spawn` and several
        // `.await`s — starts the turn. A death anywhere in that window leaves
        // the rows `delivered` with no turn behind them, and since
        // `waiting_queue` only ever offers `state='waiting'`, nothing would
        // hand them to anyone again: what the operator said is gone.
        //
        // A restart is the likeliest way to die in that window, and it does not
        // even need the daemon to be killed. `execv` keeps the pid, so a fresh
        // image boots while whatever else noticed the gap may have started a
        // SECOND daemon; both run `recover()`, and the one that loses the port
        // bind exits — after it has already consumed a batch. This pass is what
        // makes that survivable, and `serve_on` (see `run_daemon`) is what stops
        // the loser from consuming anything in the first place.
        //
        // Runs AFTER the `running_turns` pass, so a turn that really is in
        // flight — adopted just above, or buried as interrupted — already owns
        // its messages and is never second-guessed here. Runs BEFORE
        // `resume_sweep`, so the restored rows go out on the ordinary dispatch
        // path in the same boot, carrying the ordinary queued-turn notice.
        let stranded = self.store.stranded_deliveries().unwrap_or_default();
        if !stranded.is_empty() {
            let mut by_thread: HashMap<String, Vec<QueueRow>> = HashMap::new();
            for row in stranded {
                by_thread.entry(row.thread_id.clone()).or_default().push(row);
            }
            for (thread_id, rows) in by_thread {
                let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
                let message_ids: Vec<u64> = rows.iter().map(|r| r.message_id).collect();
                // R10.M1 (§5.3 rule 5): the open attempt ends on the ledger as
                // `delivery_cancelled{reason: restart}`, in the same
                // transaction that puts the rows back, and their dispatch is
                // left as it was. One event per attempt the rows were in; a
                // legacy `delivered` row has no attempt id (`attempt: null`).
                let cancelled = self.get_state(&thread_id).ok().map(|state| {
                    let last = self
                        .store
                        .last_attempt_ids(&thread_id, &message_ids)
                        .unwrap_or_default();
                    let attempts: Vec<Value> = {
                        let mut a: Vec<Option<String>> =
                            message_ids.iter().map(|m| last.get(m).cloned()).collect();
                        a.sort();
                        a.dedup();
                        a.into_iter()
                            .map(|x| x.map(Value::String).unwrap_or(Value::Null))
                            .collect()
                    };
                    let attempt = if attempts.len() == 1 {
                        attempts[0].clone()
                    } else {
                        Value::Array(attempts)
                    };
                    let dispatch_now: Vec<&str> =
                        rows.iter().map(|r| r.dispatch.as_str()).collect();
                    (
                        state.clone(),
                        self.sequenced(
                            &state,
                            crate::event::delivery::CANCELLED,
                            serde_json::json!({
                                "attempt": attempt,
                                "message_ids": message_ids,
                                "reason": "restart",
                                "dispatch_now": dispatch_now,
                            }),
                        ),
                    )
                });
                let restored = self
                    .store
                    .restore_deliveries_recorded(&ids, cancelled.as_ref().map(|(_, ev)| ev));
                if let (Ok(n), Some((state, ev))) = (&restored, cancelled) {
                    if *n > 0 {
                        self.published(&state, ev);
                    }
                }
                match restored {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::warn!(
                            thread = %thread_id, restored = n, ?message_ids,
                            "a dispatch consumed these messages and died before its turn \
                             started; returning them to the queue"
                        );
                        // Say so in the ledger. The turn never ran, so nothing
                        // was cut off — but the record has to show why messages
                        // that read as delivered are about to be sent again.
                        if let Ok(state) = self.get_state(&thread_id) {
                            self.emit(
                                &state,
                                None,
                                ThreadEvent::TurnInterrupted {
                                    reason: format!(
                                        "a dispatch claimed message(s) {message_ids:?} and died \
                                         before the turn started; no turn ran and nothing was \
                                         cut off, so they are being re-queued"
                                    ),
                                    // The dispatch died with the daemon, in the
                                    // same boot-recovery pass as the orphan
                                    // sweep above — not something anyone asked
                                    // for.
                                    trigger: InterruptTrigger::DaemonRestart.as_str().into(),
                                },
                            );
                        }
                    }
                    Err(e) => tracing::warn!(
                        error = %e, thread = %thread_id,
                        "restoring a stranded delivery failed"
                    ),
                }
            }
        }

        // ── R4.S2: FINISH EVERY MANAGED ACT A DEATH INTERRUPTED ─────────────
        //
        // A spawn the parent's ledger committed but the store never saw, a
        // child's run that ended unreported, a wake that was due and never
        // queued — each is finished here from the ledgers on disk
        // (`crate::managed::recover`). After the `running_turns` pass, so a
        // child whose turn was re-adopted above is not mistaken for ended.
        crate::managed::recover(self);

        // Waiting queue entries (Q4) and parked turns (CON-4) survive
        // restarts too — a budget-killed prompt is never lost to a reboot.
        self.resume_sweep();
    }

    /// **SH-A5 — THE ATTACHED-SESSION PASS OF [`Self::recover`].** See the
    /// comment at its call. Returns `(kept, released)` cookies. Synchronous:
    /// it is run on the blocking pool.
    fn recover_attached_sessions(&self) -> (Vec<String>, Vec<String>) {
        let rows = match self.store.list_all_attached_sessions() {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "boot could not read attached_sessions — attached threads are unlocked until \
                     a detach or the reaper settles them"
                );
                return (vec![], vec![]);
            }
        };
        let (mut kept, mut released) = (vec![], vec![]);
        for row in rows {
            match attach::session_alive(&row) {
                Some(false) => {
                    if self.release_dead_attached(&row) {
                        released.push(row.cookie);
                    }
                }
                judged => {
                    if self.keep_live_attached(&row, judged.is_none()) {
                        kept.push(row.cookie);
                    }
                }
            }
        }
        if !kept.is_empty() || !released.is_empty() {
            tracing::info!(?kept, ?released, "boot recovery settled the attached sessions");
        }
        (kept, released)
    }

    /// A session still alive (or unjudgeable): take the thread's lock back
    /// under THIS daemon and park the lease, so a detach or the reaper
    /// releases it later. Signals nothing.
    fn keep_live_attached(&self, row: &crate::store::AttachedSessionRow, unjudged: bool) -> bool {
        if self
            .attached
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&row.cookie)
        {
            return true; // this daemon already holds it
        }
        let state = match self.get_state(&row.thread_id) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(
                    thread = %row.thread_id, cookie = %row.cookie, error = %e,
                    "an attached session names a thread this daemon cannot load; its row is left \
                     for a detach"
                );
                return false;
            }
        };
        let lock = match self.lock_turn(&state, "attach_recover") {
            Ok(l) => l,
            Err(LockRefusal::HeldBy(h)) => {
                tracing::warn!(
                    thread = %row.thread_id, cookie = %row.cookie, holder = %h.boot_id,
                    holder_pid = h.daemon_pid,
                    "an attached session's thread is held by another LIVE daemon — leaving it alone"
                );
                return false;
            }
            Err(r) => {
                tracing::error!(
                    thread = %row.thread_id, cookie = %row.cookie, refusal = r.as_str(),
                    "COULD NOT RE-TAKE A LIVE ATTACHED SESSION'S THREAD LOCK — the thread is \
                     unlocked while the operator's session runs"
                );
                return false;
            }
        };
        let dir = self.data_dir.join("turns").join(&row.thread_id);
        let mcp = dir.join(format!("{}.mcp.json", row.cookie));
        let mcp = mcp.exists().then(|| TurnMcpConfig(mcp));
        let settings = TurnMcpConfig(dir.join(format!("{}.settings.json", row.cookie)));
        let hold = attach::AttachedHold::recovered(lock, mcp, settings);
        self.attached
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(row.cookie.clone(), hold);
        tracing::info!(
            thread = %row.thread_id, cookie = %row.cookie, wrapper_pid = row.wrapper_pid,
            claude_pid = ?row.claude_pid, unjudged,
            "kept an attached session across the restart: its thread stays locked"
        );
        true
    }

    /// A session whose every recorded pid is dead: delete its row FIRST
    /// (absence = gone; `verify()` refuses its token from here), remove its
    /// files, close and reconcile its turn, and release a lock no live holder
    /// owns. `false` when someone else (the reaper, a detach) took the row.
    fn release_dead_attached(&self, row: &crate::store::AttachedSessionRow) -> bool {
        match self.store.delete_attached_session(&row.cookie) {
            Ok(true) => {}
            Ok(false) => return false,
            Err(e) => {
                tracing::error!(
                    thread = %row.thread_id, cookie = %row.cookie, error = %e,
                    "could not delete a dead attached session's row"
                );
                return false;
            }
        }
        let dir = self.data_dir.join("turns").join(&row.thread_id);
        for ext in ["mcp.json", "settings.json"] {
            let _ = std::fs::remove_file(dir.join(format!("{}.{ext}", row.cookie)));
        }
        match self.close_dead_attached_turn(row) {
            Ok(r) => tracing::info!(
                thread = %row.thread_id, cookie = %row.cookie, reconciled = ?r,
                "closed a dead attached session's turn"
            ),
            Err(e) => tracing::error!(
                thread = %row.thread_id, cookie = %row.cookie, error = %e,
                "A DEAD ATTACHED SESSION'S FINAL RECONCILE FAILED — the ledger stays dirty and the \
                 next turn's begin reconciles it"
            ),
        }
        // The lock: the sweep above normally released it already (it was the
        // dead daemon's). Whatever is left is released only if no live holder
        // owns it — the same rule, `lock_is_live`.
        if let Ok(Some(l)) = self.store.get_thread_lock(&row.thread_id) {
            if !self.lock_is_live(&l, self.turn_running(&row.thread_id)) {
                let _ = self.store.release_thread_lock(&row.thread_id, &l.boot_id);
            }
        }
        if let Ok(state) = self.get_state(&row.thread_id) {
            self.emit_status(&state.id);
        }
        tracing::info!(
            thread = %row.thread_id, cookie = %row.cookie, wrapper_pid = row.wrapper_pid,
            claude_pid = ?row.claude_pid,
            "released a dead attached session at boot: row deleted, thread unlocked"
        );
        true
    }

    /// The final reconcile a detach would have run, for a session whose lease
    /// died with the previous daemon: a `turn_ended{interrupted}` for the
    /// attach's turn if nothing closed it, then the dirty thread reconciled
    /// against its native file — under the one `ledgers/` write lock.
    fn close_dead_attached_turn(
        &self,
        row: &crate::store::AttachedSessionRow,
    ) -> std::io::Result<Option<crate::ledger::dirty::Reconciled>> {
        use crate::ledger::{dirty, turn, Draft, Kind, Ledger};
        let path = turn::ledger_path(&self.data_dir, &row.thread_id);
        if !path.exists() {
            return Ok(None);
        }
        let state = self.get_state(&row.thread_id).map_err(|e| std::io::Error::other(e.to_string()))?;
        let cwd = state.settings.lock().unwrap().cwd.clone();
        let config_dir = self.current_config_dir(&state, &row.account);
        let login = self.spawn_login(&row.account, config_dir.as_deref()).map_err(std::io::Error::other)?;
        let home = crate::accounts::transcript_home()
            .ok_or_else(|| std::io::Error::other("HOME is not set: no transcript store to reconcile from"))?;
        let native = turn::native_path(login.config_dir(), &home, &cwd, &row.thread_id);
        let write = self.managed.write_lock();
        let _w = write.lock().unwrap_or_else(|e| e.into_inner());
        crate::ledger::repair_torn_tail(&path)?;
        let mut ledger = Ledger::read_jsonl(&path).map_err(|e| std::io::Error::other(e.to_string()))?;
        let on_disk = ledger.len();
        let now = crate::store::now_rfc3339();
        let of_this_turn = |l: &Ledger, k: Kind| {
            l.events()
                .iter()
                .any(|e| e.kind == k && e.turn.as_deref() == Some(row.cookie.as_str()))
        };
        if of_this_turn(&ledger, Kind::TurnStarted) && !of_this_turn(&ledger, Kind::TurnEnded) {
            let _ = ledger.append(
                Draft::new(
                    Kind::TurnEnded,
                    now.as_str(),
                    json!({"outcome": "interrupted", "detail": {
                        "exit_code": null,
                        "reason": "the attached session and the daemon that leased it both died",
                    }}),
                )
                .in_turn(Some(&row.cookie)),
            );
        }
        let reconciled = match turn::dirty_mark(&ledger) {
            Some(m) => {
                let mut marks = dirty::DirtyMarks::default();
                marks.mark(&row.thread_id, &m.since_turn, m.rendered_bytes);
                dirty::reconcile(&mut ledger, &mut marks, &native, &now)?
            }
            None => None,
        };
        for e in &ledger.events()[on_disk..] {
            Ledger::append_to_file(&path, e)?;
        }
        Ok(reconciled)
    }

    /// Tail a re-adopted orphan's durable output from the persisted offset
    /// until the pid exits. The exit CODE is unobservable (the child was
    /// re-parented away from us), so the turn closes with `exit_code: None`
    /// and no stderr-based retry classification — best-effort by design;
    /// see the report's G1 notes.
    ///
    /// **This loop duplicates `run_once`'s tail loop, and that is on purpose.**
    /// The operator was shown the duplication and ruled against removing it:
    /// *"To me, that sounds like there's an error in the architecture of the
    /// threads module… I don't actually want to fix this, because this is just a
    /// symptom of poor software architect work done by large language models in
    /// general."* Do not merge them. What the two loops MUST share is the
    /// durable landing of a teardown, and they do — `park_killed_turn` and
    /// `land_failed_verdict` are called from both — and, since R9.L6b, the
    /// output-stall check: one helper, `check_output_stall`, called from both
    /// loops, so a re-adopted child that hangs is torn down exactly as a fresh
    /// one is.
    async fn adopt_turn(&self, state: Arc<ThreadState>, row: RunningTurnRow, lock: TurnLock) {
        let output_path = PathBuf::from(&row.output_path);
        let mut offset = row.byte_offset;
        let mut sig = TurnSignals::default();
        // ── L-2: WHAT THE PREVIOUS DAEMON ALREADY READ ──────────────────
        //
        // The tail resumes at `byte_offset`, so everything before it was
        // read by the daemon that was restarted — including, often, the
        // verdict itself (a child that sent its `result` and then lingered in
        // a hook is exactly what a restart catches). The verdict-bearing part
        // of that prefix is re-read, WITHOUT re-emitting or re-charging
        // anything, so the adopted ending is judged on the whole execution's
        // stream as `run_turn` would judge it, not on its tail alone.
        seed_verdict_from_prefix(&output_path, row.byte_offset, &mut sig);
        // ── R9.L6b: THE ASYNC AGENTS THE PREVIOUS DAEMON SAW LAUNCH ──────
        //
        // `outstanding` lives in memory, so a restart forgets an async agent
        // the child launched before it — and the stall check below would then
        // read the parent's legitimate silence while that agent works as a
        // hang. Rebuilt from the same prefix, without emitting anything; the
        // tail clears entries as their `task_notification`s arrive, and
        // `process_exited` clears the rest.
        let held = outstanding_in_prefix(&output_path, row.byte_offset);
        if !held.is_empty() {
            state.live.lock().unwrap().outstanding.extend(held);
        }
        // ── WHAT THE ADOPTER NOW KNOWS ───────────────────────────────────
        //
        // `None` means the row predates the `prompt` column, NOT that the
        // prompt was lost. It was never at risk: the operator's words are on
        // disk twice from the instant a turn goes running — the `events` ledger
        // (`kind = 'user_message'`, written by `send_declaring` before dispatch)
        // and `queue.text` (`mark_delivered` only flips `state`, it does not
        // clear the text) — and this crate never deletes a row from either. The
        // gap this closes was a MISSING SELECT, not a data-loss risk.
        //
        // A `None` row therefore behaves exactly as this function did before the
        // column existed, which is what makes the limitation a ONE-RESTART
        // transient rather than a permanent contract.
        let recovered = row.prompt.clone();
        let attempt = row.attempt;
        let message_ids = row.message_ids.clone();
        // Set when a teardown fires and we have a prompt to land it with. Acted
        // on after the child is gone, never inside the loop, so the landing
        // happens once and after the ledger has closed the turn.
        let mut budget_killed = false;
        let mut limit_killed: Option<LimitKill> = None;
        // The ASYMMETRY THIS FIXES: this loop used to read `cancel_requested`
        // and act on it — SIGTERM the process group — without recording that it
        // had. `run_once` returned a `cancelled` outcome its caller could see;
        // an adopted orphan signalled and then closed with a bare `turn_ended`,
        // so the identical operator action produced two different ledgers
        // depending on whether the daemon happened to have restarted. It is
        // recorded here now, on the same condition and with the same event.
        let mut cancelled = false;
        let mut compaction_stopped = false;
        // ── R9.L6b: THE OUTPUT-STALL CLOCK, ON THIS PATH TOO ──
        //
        // Starts at ADOPTION: when the child last wrote before the restart is
        // not known, so a re-adopted child gets one whole bound from here — a
        // late teardown rather than a false one. `stall_kill_at` is the
        // SIGKILL escalation `run_once` applies to every teardown; here it is
        // armed by the stall teardown only (the target IS hung, and a hung
        // child may not honour SIGTERM).
        let mut stall = StallClock::new();
        let mut stall_kill_at: Option<std::time::Instant> = None;
        self.emit_liveness(&state);
        let heartbeat = self.spawn_heartbeat(&state);
        loop {
            let (lines, new_off) = read_new_lines(&output_path, offset);
            for line in &lines {
                self.process_line(&state, line, &mut sig, None);
            }
            if new_off != offset {
                offset = new_off;
                stall.saw_output();
                let _ = self.store.update_turn_offset(&state.id, offset);
            }
            // ── THE OUTPUT STALL (R9.L6b) — the same check as `run_once`'s ──
            //
            // Armed only with a recovered prompt, for the reason the limit
            // kill below declines without one: a row that predates the
            // `prompt` column cannot be laddered from here, and tearing it
            // down anyway would drop the turn (CON-4).
            let armed = recovered.is_some()
                && !cancelled
                && !budget_killed
                && limit_killed.is_none()
                && !compaction_stopped;
            if self.check_output_stall(&state, row.pid, &mut stall, &sig, armed) {
                stall_kill_at = Some(std::time::Instant::now() + self.tunables.kill_grace);
            }
            if let Some(dl) = stall_kill_at {
                if std::time::Instant::now() >= dl {
                    signal_turn(row.pid, self.teardown, libc::SIGKILL);
                    stall_kill_at = None;
                }
            }
            // The `||`'s short-circuit is preserved exactly as it was:
            // `budget_kill` is still only swapped when the cancel flag was not
            // set. Only the binding is new, so the two can be told apart —
            // a cancel discards the prompt by definition (the operator asked for
            // it to stop), a budget kill must PARK it (CON-4). The same binding
            // is what lets a cancel be RECORDED and not merely acted on.
            let cancel_now = state.cancel_requested.swap(false, Ordering::SeqCst);
            let budget_now = !cancel_now && state.budget_kill.swap(false, Ordering::SeqCst);
            if cancel_now || budget_now {
                cancelled |= cancel_now;
                // A re-adopted child is still its own process-group leader
                // (we set that at spawn), so the same teardown reaches its
                // descendants across a daemon restart.
                signal_turn(row.pid, self.teardown, libc::SIGTERM);
            }
            if budget_now {
                // ── CON-4 ON THE ADOPTED PATH ──
                //
                // *"A budget or threshold trip must PARK the turn and surface
                // it, never discard it… killing ends the execution, it does not
                // lose the operator's prompt."* This branch used to SIGTERM and
                // then fall straight through to `finish_turn` with no park at
                // all, so an adopted turn's prompt really was dropped on a
                // budget kill — the same defect `park_killed_turn` was written
                // to end, surviving on the one path that could not call it.
                budget_killed = recovered.is_some();
                // The same event `run_once` emits on this condition, with the
                // same fields — a client must not be able to tell which loop
                // tore the turn down. It is emitted even when the prompt cannot
                // be parked: the teardown HAPPENED either way, and a silent
                // teardown is the asymmetry this path keeps being caught in.
                let trip = state.kill_trip.lock().unwrap().clone();
                self.emit(
                    &state,
                    None,
                    ThreadEvent::TurnKilled {
                        reason: trip
                            .as_ref()
                            .map(Trip::reason)
                            .unwrap_or_else(|| "budget exhausted".to_string()),
                        budget_key: trip.as_ref().map(|t| t.budget_key.clone()),
                        strategy: self.teardown.as_str().to_string(),
                    },
                );
                if !budget_killed {
                    tracing::error!(
                        thread = %state.id, pid = row.pid,
                        "BUDGET KILL ON AN ADOPTED TURN WHOSE ROW PREDATES THE `prompt` COLUMN — \
                         the execution is torn down and the prompt cannot be parked from here. \
                         It is still on the ledger (`user_message`) and in `queue.text`. This \
                         resolves itself on the next daemon restart."
                    );
                }
            }
            // ── AND THE POST-COMPACTION ENDING, ON THIS PATH TOO ──
            //
            // **This branch is not optional, and leaving it out is worse than a
            // missing feature — it is a leak.** `adopt_turn` runs the same
            // `process_line`, so the same `Compacted` arm raises
            // `ThreadState::compaction_stop` here exactly as it does under
            // `run_once`. The flag lives on the thread, not on the execution.
            // With no consumer on this loop it would stay raised after the
            // adopted turn closed, and the NEXT turn's tail loop would swap it
            // on its first poll and end a turn that had never compacted at all.
            //
            // So the flag is consumed on whichever loop is running, and the
            // ledger says the same thing either way: a client must not be able
            // to tell which loop tore the turn down. That is the rule the two
            // teardowns above already follow.
            //
            // Nothing is parked and no ladder is armed, matching `run_once`:
            // the delivery ENDS. Unlike the budget branch there is no
            // `recovered.is_some()` precondition, because there is nothing to
            // land — an ending does not need the prompt back.
            if state.compaction_stop.swap(false, Ordering::SeqCst) {
                compaction_stopped = true;
                self.emit(
                    &state,
                    None,
                    ThreadEvent::TurnInterrupted {
                        reason: "an automatic compaction happened and this thread does not \
                                 resume after one (resume_after_compaction=false) — the turn \
                                 ends here; the next turn is told what was compacted away"
                            .to_string(),
                        trigger: InterruptTrigger::Compaction.as_str().into(),
                    },
                );
                signal_turn(row.pid, self.teardown, libc::SIGTERM);
            }
            // ── AND `limit_kill`, WHICH THIS PATH NOW HONOURS ──
            //
            // The comment that used to sit here said a usage-limit teardown was
            // impossible on an adopted turn because *"the operator's message
            // would be gone, with no durable home to be redelivered from."*
            // **That sentence was false** and it has been deleted rather than
            // left standing beside the fix. The message is on the `events`
            // ledger and in `queue.text` from before dispatch, and it is now on
            // the `running_turns` row this function is holding. What was missing
            // was a SELECT.
            //
            // Structurally the twin of the two flags above: same swap, same
            // `signal_turn`, same process group. The landing differs, and
            // matches `run_once`'s exactly — not a park, but the retry ladder,
            // because a usage-limit interrupt and a usage-limit 429 are the same
            // event seen at two moments.
            if state.limit_kill.swap(false, Ordering::SeqCst) {
                let sigk = state.limit_kill_signal.lock().unwrap().clone();
                let account = sigk
                    .as_ref()
                    .map(|k| k.account.clone())
                    .unwrap_or_else(|| state.account.lock().unwrap().clone());
                if recovered.is_none() {
                    // THE PRE-MIGRATION FALLBACK, and it is exactly today's
                    // behaviour: decline, and say so once. A row with no prompt
                    // cannot be landed durably from here, and tearing it down
                    // anyway is the CON-4 violation the old comment feared.
                    tracing::warn!(
                        thread = %state.id, pid = row.pid, %account,
                        "declining a mid-turn usage-limit interrupt on an adopted turn whose row \
                         predates the `prompt` column — it will discover the limit by taking the \
                         429 itself, as it did before this column existed"
                    );
                } else {
                    let quota = sigk
                        .as_ref()
                        .and_then(|k| k.pool.clone())
                        .unwrap_or_else(|| "<unnamed>".to_string());
                    limit_killed = Some(sigk.unwrap_or(LimitKill {
                        reset: None,
                        pool: None,
                        account: account.clone(),
                    }));
                    // The same event, with the same fields, as `run_once` emits
                    // on this condition: a client must not be able to tell which
                    // loop tore the turn down.
                    self.emit(
                        &state,
                        None,
                        ThreadEvent::TurnKilled {
                            reason: format!(
                                "usage limit on {account} ({quota}) — interrupted mid-turn and \
                                 handed back to admission"
                            ),
                            // Not a budget trip.
                            budget_key: None,
                            strategy: self.teardown.as_str().to_string(),
                        },
                    );
                    signal_turn(row.pid, self.teardown, libc::SIGTERM);
                }
            }
            if !pid_alive(row.pid) {
                let (lines, _) = read_new_lines(&output_path, offset);
                for line in &lines {
                    self.process_line(&state, line, &mut sig, None);
                }
                break;
            }
            tokio::time::sleep(self.tunables.tail_poll).await;
        }
        heartbeat.abort();
        let _ = self.store.clear_running_turn(&state.id);
        self.process_exited(&state);
        self.emit_liveness(&state);
        self.emit(&state, None, ThreadEvent::TurnEnded { exit_code: None });
        if cancelled {
            // Identical event, identical trigger, identical position relative
            // to `turn_ended` as the `run_once` path — a client cannot tell
            // which loop tore the turn down, and must not have to.
            self.emit(
                &state,
                None,
                ThreadEvent::TurnInterrupted {
                    reason: "the operator interrupted this turn".into(),
                    trigger: InterruptTrigger::Operator.as_str().into(),
                },
            );
        }
        state.resumable.store(true, Ordering::SeqCst);
        self.note_thread_account(&state);

        // ── THE DURABLE LANDING ──────────────────────────────────────────
        //
        // After `turn_ended`, so the ledger closes the execution before it
        // records where the work went — the same order `run_turn` uses.
        //
        // THE ACCEPTED TRADE, stated because it is real: re-delivering an
        // adopted turn re-runs a turn that has already burned tokens. That is
        // not new and it is not this change's invention — `park_killed_turn`
        // does exactly this on every budget kill, and has since CON-4. An
        // execution that was torn down mid-flight has no partial result to
        // resume from, so the only two options are re-run it or lose the
        // operator's message, and CON-4 settles which of those is acceptable.
        // ── R9.L6e: THE OPERATOR'S CANCEL OUTRANKS THE BUDGET PARK ──
        //
        // As on the fresh path since R9.L6d (`run_once` checks `cancelled`
        // before `budget_killed`): a cancel that lands while a budget kill is
        // pending ends the delivery. Without `!cancelled` this path recorded
        // `turn_interrupted` (operator) AND parked the prompt the operator had
        // just stopped. Pinned by `output_stall.rs`'s
        // `a_cancel_after_an_adopted_turns_budget_kill_lands_cancelled_not_parked_for_4500ms`.
        if budget_killed && !cancelled {
            if let Some(prompt) = recovered.as_ref() {
                // Identical landing to `run_turn`'s: a durable `parked_turns`
                // row plus the DEC-56 hard-break flag, re-dispatched by
                // `resume_sweep` the moment the budget allows it.
                self.park_killed_turn(&state, prompt, &message_ids, attempt);
            }
            self.finish_turn(state, lock).await;
            return;
        }

        // ── L-2: THE ADOPTED ENDING LANDS WITH `run_turn`'S VERDICT ─────
        //
        // Until L-2 the adopted path ended here for every verdict but the two
        // kills: a 529 in the adopted tail was dropped (no retry, no
        // `turn_failed`, the message unanswered), a successful adopted RETRY
        // left the retry row that fired it in place (so the same paid prompt
        // ran again), and a credit or login error barred nothing. It now goes
        // through the SAME landings `run_turn` uses, in the same order
        // `run_turn` checks them: the budget park above, then the operator's
        // interrupt, then the verdict.
        //
        // ── THE OPERATOR'S INTERRUPT BEFORE THE LIMIT KILL (R9.L6b) ──
        //
        // This check used to sit AFTER the limit-kill landing, so a cancel
        // that raced a limit kill (two flags, one poll) put the turn the
        // operator had just stopped on the usage-limit ladder. A user
        // interruption ends the delivery; nothing outranks it (R9.L6e: not even
        // the budget park above, which yields to it).
        if cancelled || (compaction_stopped
            && crate::retry::ends_the_delivery(InterruptTrigger::Compaction))
        {
            // R9.L4b: as in `run_once` — a cancel that beat this adopted
            // turn's budget kill releases the thread's OWN kill latch.
            if cancelled {
                self.release_own_kill_latch(&state.id);
            }
            // The operator's interrupt (and a post-compaction stop) ENDS the
            // delivery, exactly as in `run_turn`.
            self.store.clear_retry(&state.id).ok();
            self.store.end_delivery(&state.id).ok();
            self.finish_turn(state, lock).await;
            return;
        }
        // ── THE LIMIT KILL GOES THROUGH `land_failed_verdict` (R9.L2-3) ──
        //
        // It used to land straight on the usage-limit ladder, skipping the
        // admission gate `run_turn`'s 429 branch asks — so an adopted turn
        // interrupted on a barred login waited out the ladder even when
        // placement had another account open. Now it is `run_turn`'s
        // handling, literally: a synthesized `RateLimit` verdict (a real 429
        // the stream carried is more specific and stands), the interrupt's
        // reset and quota outranking the stream's (a turn killed because the
        // poller saw 100% has, by construction, no 429 of its own), and the
        // same landing — `note_account_limit`, `admit_or_park`, DEC-56's
        // `interrupt_thread`, then either "placement moved it, run now"
        // (`RunAgain`) or the ladder at `attempt + 1`.
        let (verdict, reset, pool) = match limit_killed {
            Some(k) => (
                Some(sig.decision.clone().unwrap_or(RetryDecision::Retry {
                    kind: TransientKind::RateLimit,
                    reason: "usage limit (mid-turn interrupt, re-adopted turn)".into(),
                })),
                k.reset.or(sig.reset.clone()),
                k.pool.or(sig.pool.clone()),
            ),
            // A stalled child's verdict (R9.L6b) is `run_once`'s: the ladder,
            // unless the stream carried a verdict or its LAST word was a clean
            // result (R13.5; `adopted_verdict` reads the same field).
            None => (
                match stall.retry_decision(&sig) {
                    Some(d) => Some(d),
                    None => Self::adopted_verdict(&sig),
                },
                sig.reset.clone(),
                sig.pool.clone(),
            ),
        };
        match verdict {
            None => self.land_clean_verdict(&state),
            Some(decision) => match recovered {
                Some(prompt) => {
                    let mut fresh_starts = 0;
                    match self.land_failed_verdict(
                        &state,
                        &prompt,
                        &message_ids,
                        attempt,
                        &mut fresh_starts,
                        decision,
                        reset.as_deref(),
                        pool.as_deref(),
                        // No transcript mark: the attempt began in the previous
                        // daemon, which is where the mark was taken.
                        &None,
                    ) {
                        VerdictLanding::Done => {}
                        // Placement moved the turn, or it restarts fresh:
                        // `run_turn` would re-run it now at `attempt + 1`, so
                        // the adopted turn is handed to `run_turn`, lock and
                        // all, to do exactly that.
                        VerdictLanding::RunAgain => {
                            Box::pin(self.run_turn(state, prompt, message_ids, attempt + 1, lock))
                                .await;
                            return;
                        }
                    }
                }
                None => tracing::error!(
                    thread = %state.id, pid = row.pid, verdict = %decision.kind_str(),
                    "AN ADOPTED TURN WHOSE ROW PREDATES THE `prompt` COLUMN ended on a failure \
                     verdict — it cannot be put on a ladder from here. The words are still on \
                     the ledger (`user_message`) and in `queue.text`."
                ),
            },
        }
        self.finish_turn(state, lock).await;
    }

    /// **The verdict of a re-adopted execution** (L-2), from the stream alone:
    /// the adopted child is not ours, so there is no exit code and no stderr.
    ///
    /// * A verdict the stream carried (`sig.decision`: a 429, an errored
    ///   `result`, a subagent's limit) is the verdict, as in `run_turn`.
    /// * Otherwise a clean `result` as the process's LAST word means the turn
    ///   DELIVERED (`None`) — R13.5, overriding E17-8's sticky latch (executor
    ///   ruling H179-3, round 42): one process can stream several CLI turns,
    ///   and a queued notification mini-turn's result followed by a newly
    ///   opened turn ended that mini-turn, not this delivery. A re-entry after
    ///   an answered turn is safe because R13's rule sends the continue note,
    ///   not the operator's text.
    /// * Otherwise the child ended with no verdict at all — it died
    ///   mid-execution. `run_turn` reads that off the non-zero exit, with the
    ///   stream's structured API error first (`sig.api_error`); with no exit
    ///   code to read, the fallback is the class an unclassified mid-execution
    ///   death gets, the same one L-1 uses for a child a restart killed.
    fn adopted_verdict(sig: &TurnSignals) -> Option<RetryDecision> {
        if sig.decision.is_some() {
            return sig.decision.clone();
        }
        if sig.last_word_result {
            return None;
        }
        Some(sig.api_error.clone().unwrap_or_else(|| RetryDecision::Retry {
            kind: TransientKind::ErrorDuringExecution,
            reason: "a re-adopted turn's child ended without a result".into(),
        }))
    }
}

/// **ONE EXECUTION'S OUTPUT-STALL CLOCK** (R9.L6, R9.L6b) — the state
/// `ThreadManager::check_output_stall` keeps between polls, owned by whichever
/// tail loop is running (`run_once` or `adopt_turn`).
struct StallClock {
    /// The instant the child last wrote to its stream. Starts at spawn (or at
    /// adoption), so a child that never writes at all is as hung as one that
    /// wrote once and stopped.
    last_output: std::time::Instant,
    /// The last poll at which a native async agent was outstanding.
    held_at: Option<std::time::Instant>,
    /// Set when the teardown fires: the silence that tripped it.
    stalled: Option<Duration>,
    /// The ladder's reason for this teardown (plain silence, or a hold that
    /// expired with named agents outstanding).
    retry_reason: Option<String>,
}

impl StallClock {
    fn new() -> Self {
        Self {
            last_output: std::time::Instant::now(),
            held_at: None,
            stalled: None,
            retry_reason: None,
        }
    }
    /// The child wrote something.
    fn saw_output(&mut self) {
        self.last_output = std::time::Instant::now();
    }
    /// **A STALLED CHILD GOES ON A LADDER** — unless the stream already
    /// carried a verdict (more specific, it wins) or the process's LAST word
    /// was a clean `result` (R9.L6c: the turn DELIVERED; only the process
    /// lingered, and re-running it would duplicate a finished, paid turn under
    /// `--resume`). R13.5 (overriding E17-8's sticky latch, H179-3): a result
    /// followed by a newly opened turn — a notification mini-turn ahead of
    /// the operator's, or a parent woken after its answer — does not count;
    /// that stall is laddered, and R13's rule sends the continue note for a
    /// message the model already answered.
    fn retry_decision(&self, sig: &TurnSignals) -> Option<RetryDecision> {
        self.stalled?;
        if sig.decision.is_some() || sig.last_word_result {
            return None;
        }
        Some(RetryDecision::Retry {
            kind: TransientKind::ErrorDuringExecution,
            reason: self
                .retry_reason
                .clone()
                .unwrap_or_else(|| "claude wrote no output and was torn down as hung".into()),
        })
    }
}

/// **THE ASYNC AGENTS STILL OUTSTANDING AT `upto`** (R9.L6b) — the same fold
/// `process_line` does over `outstanding` (launched async and not already
/// completed; cleared by the CLI's `task_notification`), run over
/// the prefix a restarted daemon already read, emitting nothing. What
/// `adopt_turn` seeds the stall check's hold with.
fn outstanding_in_prefix(path: &std::path::Path, upto: u64) -> HashMap<String, Option<String>> {
    let mut out = HashMap::new();
    if upto == 0 {
        return out;
    }
    let Ok(f) = std::fs::File::open(path) else {
        return out;
    };
    let mut buf = Vec::new();
    if f.take(upto).read_to_end(&mut buf).is_err() {
        return out;
    }
    for line in String::from_utf8_lossy(&buf).lines() {
        if line.trim().is_empty() {
            continue;
        }
        for ev in crate::stream::parse_raw_line(line) {
            match ev {
                ThreadEvent::AgentLaunched {
                    agent_id,
                    is_async,
                    status,
                    output_file,
                    ..
                } if is_async && status != "completed" => {
                    out.insert(agent_id, output_file);
                }
                // The ONLY clearing arm, and guarded (R9.L6d: an agent
                // notified inside the prefix must not hold the adopted parent).
                // No `AgentCompleted` arm: that event is synthesized by
                // `process_exited` and written to the ledger, never parsed off
                // the CLI's stream (`stream::parse_line` has no producer for
                // it), so an arm for it here could never run.
                ThreadEvent::TaskNotification { task_id, .. } => {
                    out.remove(&task_id);
                }
                _ => {}
            }
        }
    }
    out
}

/// In-stream signals captured during one spawn (retry decision + reset prose).
#[derive(Default)]
struct TurnSignals {
    decision: Option<RetryDecision>,
    reset: Option<String>,
    /// The last TOP-LEVEL assistant text this turn streamed.
    ///
    /// Kept for one reader: a `result` line flagged `is_error` whose subtype
    /// is `success` and therefore names no cause
    /// (`retry::classify_errored_result`). The CLI puts the cause in this text
    /// — `API Error: 529 Overloaded`, `…(ENOTFOUND)`, `Connection closed
    /// mid-response`, `Prompt is too long` — and without it the daemon has
    /// nothing to classify but a contradiction.
    ///
    /// Top-level only (`parent_tool_use_id: None`): a subagent's chatter is
    /// not the parent turn's verdict.
    last_text: Option<String>,
    /// The quota the refusal named (`gauge::Axis::as_str`), when it named one.
    /// `interpret` used to consume this into a `usage_observations` row and drop
    /// it, so the 429 branch five hundred lines later had no idea WHICH limit
    /// had been hit and recorded an un-scoped bar on the whole account.
    pool: Option<String>,
    /// **The verdict a synthetic API error named, held back until something
    /// else agrees the turn failed.**
    ///
    /// Deliberately NOT `decision`, and the distinction is the whole design.
    ///
    /// `decision` is a commitment: `run_turn` retries on any `Some`, whatever
    /// the result line went on to say. A `ThreadEvent::ApiError` is not that
    /// strong on its own. In the operator's turn logs, 27 API-error records
    /// arrive on turns whose `result` then reports `is_error: false` — the CLI
    /// hit "Connection closed mid-response", recovered, and delivered. Writing
    /// `decision` from the API error would re-run every one of those completed
    /// turns and hand him the work twice.
    ///
    /// So it is held here as the BEST AVAILABLE DIAGNOSIS and consumed only by
    /// the two places that have already established the turn failed:
    /// the `ResultTurn` arm (`is_error: true`) and the non-zero-exit door. In
    /// both it outranks what they would otherwise use, because it is the
    /// producer's own structured verdict and their fallbacks are guesses at
    /// prose.
    ///
    /// The event itself is emitted either way, so the 168 records that used to
    /// leave no trace at all are now on the ledger even when the turn recovers.
    api_error: Option<RetryDecision>,
    /// **A clean verdict has arrived this execution** (R9.L6c, inspect F1):
    /// a `result` with `is_error: false` was parsed off the stream.
    ///
    /// STICKY for the execution. Since R13.5 it decides NOTHING: it only
    /// starts the [`Self::events_after_result`] count. Every verdict — the
    /// stall teardown (`StallClock::retry_decision`, `reaped_after_verdict`),
    /// R13.3's no-verdict guard, and the adopt path (`adopted_verdict`) —
    /// reads [`Self::last_word_result`]. E17-8 (an executor ruling that kept
    /// the stall arm on this sticky latch) was overridden by the executor in
    /// round 42 (H179-3): a parent woken after its result that then hangs is
    /// laddered, and R13's continue note keeps the re-entry from re-sending
    /// the operator's text.
    result_delivered: bool,
    /// **R13.4: the process's LAST word was a clean `result`** — a clean
    /// `result` has arrived and NO TURN HAS OPENED SINCE.
    ///
    /// Not [`Self::result_delivered`], and the difference is a silent loss
    /// observed in prod (DEPLOY-9l attempt 2, an internal thread):
    /// one `-p --resume` process can run SEVERAL CLI turns. On resume the CLI
    /// first runs a queued `<task-notification>` as its own mini-turn —
    /// `system/init`, clean `result` — and only then opens the operator's
    /// turn with a SECOND `system/init`. The sticky latch fired on that first
    /// result, so a signal death during the real work read as clean and the
    /// message was dropped with no `turn_failed`.
    ///
    /// Re-opened (set false) by what opens or continues a CLI turn after the
    /// result: a `system/init`, or TOP-LEVEL model output (`assistant` text,
    /// `thinking`, `tool_use` with no `parent_tool_use_id`). Deliberately NOT
    /// re-opened by everything else: a finished turn's process goes on to
    /// stream `background_tasks_changed`, `task_updated{killed}` and
    /// `task_notification` for its background tasks after its final
    /// `result` (DEPLOY-9l attempt 1, seq 83→86), and counting those would
    /// re-run a delivered, paid turn.
    ///
    /// Read by every verdict that asks "did this turn end?": R13.3's
    /// no-verdict guard in `run_once`, the stall teardown
    /// (`StallClock::retry_decision`, `reaped_after_verdict`,
    /// `check_output_stall`'s fate), and the adopt path (`adopted_verdict`,
    /// seeded by `seed_verdict_from_prefix`) — R13.5. Maintained by ONE
    /// function, [`note_last_word`].
    last_word_result: bool,
    /// How many stream events were parsed after the first clean `result`.
    /// Non-zero at a stall teardown means output the stall cut off, and the
    /// ledger says so rather than letting it vanish.
    events_after_result: u64,
    /// R11.J2: the `result` was `error_max_structured_output_retries`, which
    /// the CLI follows with exit 1. The turn delivered (only its JSON is
    /// missing, `retry::is_schema_unmet_result`), so that exit code is not a
    /// failure to classify.
    schema_unmet_exit: bool,
}

/// What [`ThreadManager::land_failed_verdict`] asks its caller to do next.
enum VerdictLanding {
    /// The outcome is recorded (a retry scheduled, a park, a visible failure);
    /// the turn ends here.
    Done,
    /// Re-run the turn NOW at `attempt + 1`: placement moved it to another
    /// account, or the conversation is being restarted fresh.
    RunAgain,
}

/// The outcome of one spawn cycle.
struct TurnOutcome {
    decision: Option<RetryDecision>,
    reset: Option<String>,
    /// See [`TurnSignals::pool`].
    pool: Option<String>,
    cancelled: bool,
    /// A budget trip tore this execution down (DEC-13). The caller PARKS the
    /// prompt — a kill ends the execution, it does not lose what was said.
    budget_killed: bool,
    /// A USAGE LIMIT on this thread's account tore this execution down
    /// (`ThreadState::limit_kill`). Deliberately NOT `budget_killed`: the
    /// caller must route this into the 429 branch, which hands the thread back
    /// to `admit_or_park` and may have it running elsewhere immediately, where
    /// a budget kill parks and waits for a meter that has not moved.
    limit_killed: bool,
    /// **An automatic compaction ended this execution**
    /// (`ThreadSettings::resume_after_compaction`, false by default).
    ///
    /// Deliberately its own field rather than a reuse of `cancelled`. The two
    /// share a policy — both END the delivery instead of laddering it — but they
    /// do not share a cause, and the caller logs them apart so the ledger and
    /// the daemon log agree with each other about which happened.
    compaction_stopped: bool,
    /// **THE SPAWN GUARD REFUSED THIS TURN A LOGIN — nothing was spawned.**
    ///
    /// Set only by [`ThreadManager::refuse_unresolved_login`], which has
    /// already parked the thread, logged at ERROR and put the refusal on the
    /// ledger. `run_turn` reads it as an END, not as a failure to classify, for
    /// two reasons and both matter:
    ///
    /// * **There is no child to diagnose.** `decision` is a verdict about
    ///   something that RAN; this turn never reached `cmd.spawn()`.
    /// * **A ladder would double-handle the prompt.** The refusal already put
    ///   his words in a durable `parked_turns` row, so letting the retry ladder
    ///   run it out to `give_up_and_keep_the_prompt` would ALSO move the queue
    ///   rows to `failed` — two live copies of one delivery, and a
    ///   re-dispatch that hands him the same message twice.
    ///
    /// Its own field rather than a reuse of `budget_killed` (which parks in
    /// exactly this shape) because the two do not share a cause and the daemon
    /// log has to agree with the ledger about which happened — the same
    /// argument `compaction_stopped` makes for not being `cancelled`.
    login_refused: bool,
    /// **The projected turn could not begin** (`crate::ledger::turn::ProjectionRefused`):
    /// nothing was spawned, the prompt is parked with cause `projection`. Same
    /// shape as `login_refused`, for the same reason — a turn that never reached
    /// `cmd.spawn()` is ended, not laddered.
    projection_refused: bool,
}

impl TurnOutcome {
    fn fatal(reason: String) -> Self {
        Self {
            decision: Some(RetryDecision::Fatal { reason }),
            reset: None,
            pool: None,
            cancelled: false,
            budget_killed: false,
            limit_killed: false,
            compaction_stopped: false,
            login_refused: false,
            projection_refused: false,
        }
    }
}

/// Below this observed percent, a gauge reading is taken to CONTRADICT a
/// subagent's claim of exhaustion rather than merely fail to confirm it.
///
/// 50 is deliberately far from the cliff. The question this number answers is
/// not "is the account nearly full" — the subagent already told us it is full —
/// but "is this reading so incompatible with exhaustion that the message is
/// more likely to be text a subagent READ than a refusal it RECEIVED". A poll
/// can legitimately be fifteen minutes stale and a weekly axis can move a long
/// way in fifteen minutes, so anything nearer the cliff would start vetoing
/// true reports.
const SUBAGENT_LIMIT_CONTRADICTION_FLOOR: f64 = 50.0;

/// **Is this `system/task_updated` patch a subagent that died on a USAGE
/// LIMIT?** Returns the axis that refused and the reset prose, in the shape
/// `note_account_limit` wants (what follows `"resets "`, not the whole
/// message).
///
/// ── THE FALSE-POSITIVE GUARD, WHICH IS THE POINT OF THIS FUNCTION ──
///
/// The obvious implementation is `classify_message(&error)` and a check for
/// `TransientKind::RateLimit`. It must not be used here. `retry::classify_message`'s
/// rate-limit branch matches the bare substring `"resets"`, which is fine for
/// the PARENT stream — there the text is a synthetic 429 the API produced — and
/// catastrophic for a subagent's error field, where the text is whatever the
/// subagent was doing when it died. On this deployment subagents read
/// rate-limiting code as routine work. One of them failing while quoting
/// `runtime.rs` back at us would bar a perfectly healthy account, and the bar
/// would then be inherited by every label of that login.
///
/// So three independent conditions, all required:
///
/// 1. **A terminal failure.** `status` is `failed` or `killed`. 183 of 2,407
///    captured patches carry only `is_backgrounded` and no status at all; a
///    backgrounding patch is not a death, and `completed` certainly is not.
/// 2. **A classifiable QUOTA.** `gauge::classify_limit_pool` returns a concrete
///    axis — the same classifier the parent's 429 goes through, so the two
///    agree on what `limit_axis` means.
/// 3. **A PARSEABLE RESET.** `reset::parse_limit_reset` returns `Some`, which
///    requires the literal `"resets "` followed by prose that resolves to a
///    real instant. Prose alone is not enough; it has to be a time.
///
/// A corroboration check against the gauge is applied by the CALLER, which has
/// the account and the store — see the `TaskUpdated` arm.
pub fn subagent_limit_signal(
    status: Option<&str>,
    error: Option<&str>,
    is_backgrounded: Option<bool>,
) -> Option<(Axis, String)> {
    // (1) A terminal failure, and not merely a backgrounding patch. The
    // `is_backgrounded` test is redundant given the status test today — such a
    // patch carries no status — and it is kept because the redundancy is free
    // and the CLI's patch shapes are not a closed set.
    if is_backgrounded.is_some() && status.is_none() {
        return None;
    }
    if !matches!(status, Some("failed") | Some("killed")) {
        return None;
    }
    let error = error?;
    if error.trim().is_empty() {
        return None;
    }
    // (2) A quota we can name.
    let axis = crate::gauge::classify_limit_pool(error)?;
    // (3) A reset that is a TIME, not a word.
    let reset = crate::reset::parse_limit_reset(error)?;
    Some((axis, reset.raw))
}

/// What was known when a mid-turn interrupt was ordered — carried on
/// [`ThreadState::limit_kill_signal`] from whichever of the three detectors
/// fired (the `/usage` poller, a subagent's death, or a sibling thread's 429)
/// to the 429 branch that acts on it.
#[derive(Debug, Clone)]
struct LimitKill {
    /// Reset prose in the shape `crate::reset::parse_reset` wants — i.e. what
    /// follows `"resets "`, NOT the whole message.
    reset: Option<String>,
    /// `gauge::Axis::as_str`, never a raw `/usage` pool label: this ends up in
    /// `accounts.limit_axis`, which the 429 path also writes, and the column
    /// only means anything if both writers speak the same vocabulary.
    pool: Option<String>,
    /// The account the bar was observed on, for the ledger line.
    account: String,
}

/// **R13.4 / R13.5: THE LAST-WORD RULE**, in one place — `interpret` (a live
/// tail) and `seed_verdict_from_prefix` (the prefix an adopting daemon
/// re-reads) both fold every parsed event through it, so the stall arm, the
/// no-verdict guard and the adopt path judge the SAME field by the SAME rule.
/// A clean `result` sets [`TurnSignals::last_word_result`]; an errored one
/// clears it; anything that opens or continues a CLI turn (`system/init`, or
/// TOP-LEVEL model output) clears it; every other event — a finished turn's
/// trailing `background_tasks_changed` / `task_updated` / `task_notification`
/// lines among them — leaves it alone.
fn note_last_word(sig: &mut TurnSignals, ev: &ThreadEvent) {
    match ev {
        ThreadEvent::ResultTurn { is_error, .. } => sig.last_word_result = !*is_error,
        ThreadEvent::SystemInit { .. }
        | ThreadEvent::AssistantText { parent_tool_use_id: None, .. }
        | ThreadEvent::Thinking { parent_tool_use_id: None, .. }
        | ThreadEvent::ToolUse { parent_tool_use_id: None, .. } => sig.last_word_result = false,
        _ => {}
    }
}

/// **L-2: re-read the verdict-bearing part of what the previous daemon
/// already consumed** — bytes `[0, upto)` of a re-adopted turn's output file —
/// into `sig`, and nothing else. No event is emitted, nothing is charged,
/// nothing is barred: that daemon already did all of it. Only the fields the
/// adopted ending is judged on are filled, by the same rules
/// `ThreadManager::interpret` applies to them, so the verdict reflects the
/// whole execution and not only the tail after the restart.
fn seed_verdict_from_prefix(path: &std::path::Path, upto: u64, sig: &mut TurnSignals) {
    if upto == 0 {
        return;
    }
    let Ok(f) = std::fs::File::open(path) else {
        return;
    };
    let mut buf = Vec::new();
    if f.take(upto).read_to_end(&mut buf).is_err() {
        return;
    }
    for line in String::from_utf8_lossy(&buf).lines() {
        if line.trim().is_empty() {
            continue;
        }
        for ev in crate::stream::parse_raw_line(line) {
            if sig.result_delivered {
                sig.events_after_result += 1;
            }
            // R13.5: the adopt path judges on the LAST word (`adopted_verdict`),
            // so the prefix keeps that field by `interpret`'s own rule.
            note_last_word(sig, &ev);
            match ev {
                ThreadEvent::AssistantText {
                    text,
                    parent_tool_use_id: None,
                    ..
                } if !text.trim().is_empty() => sig.last_text = Some(text),
                ThreadEvent::LimitHit { reset, pool, .. } => {
                    sig.reset = reset;
                    sig.pool = pool;
                    sig.decision = Some(RetryDecision::Retry {
                        kind: TransientKind::RateLimit,
                        reason: "rate limit (429)".into(),
                    });
                }
                ThreadEvent::ApiError {
                    error,
                    status,
                    text,
                } => {
                    if sig.api_error.is_none() {
                        sig.api_error = Some(crate::retry::classify_api_error(
                            error.as_deref(),
                            status,
                            &text,
                        ));
                    }
                }
                ThreadEvent::ResultTurn {
                    subtype, is_error, ..
                } => {
                    if !is_error {
                        sig.result_delivered = true;
                    } else if sig.decision.is_none() {
                        sig.decision = Some(sig.api_error.clone().unwrap_or_else(|| {
                            classify_errored_result(&subtype, sig.last_text.as_deref())
                        }));
                    }
                }
                _ => {}
            }
        }
    }
}

/// Read the COMPLETE lines appended to `path` since `offset`. Returns the
/// lines and the new offset (past the last complete newline — a partial
/// trailing line stays unconsumed for the next poll).
fn read_new_lines(path: &std::path::Path, offset: u64) -> (Vec<String>, u64) {
    let Ok(mut f) = std::fs::File::open(path) else {
        return (Vec::new(), offset);
    };
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return (Vec::new(), offset);
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return (Vec::new(), offset);
    }
    // Only consume through the last newline.
    let consumed = match buf.iter().rposition(|&b| b == b'\n') {
        Some(pos) => pos + 1,
        None => return (Vec::new(), offset),
    };
    let text = String::from_utf8_lossy(&buf[..consumed]);
    let lines: Vec<String> = text
        .lines()
        .map(str::to_string)
        .filter(|l| !l.trim().is_empty())
        .collect();
    (lines, offset + consumed as u64)
}

// ── RESPONSIBILITY DISCLAIM AT THE `claude` SPAWN SEAM ───────────────────────
//
// macOS does not attribute a TCC consent prompt to the process that touched the
// protected thing. It attributes it to that process's RESPONSIBLE process — the
// ancestor the system considers "the app". Every `claude` this daemon spawns
// therefore inherited whatever identity started the daemon, which since
// 2026-08-20 has been the AUI ui-server. MEASURED on this machine, tccd's own
// `AUTHREQ_ATTRIBUTION` record for a child spawned the ordinary way reads:
//
//     responsible={identifier=com.harness.ui-server,
//                  responsible_path=…/harness/target/debug/ui-server,
//                  binary_path=…/the-child}
//
// so every prompt an agent provokes lands on the operator's audio interface —
// whose identity is a bare Mach-O with no bundle, no team identifier and no
// sealed resources, living at a Cargo path that is deleted and recreated on
// every build. tccd identifies it BY PATH because there is nothing else to
// identify it by, which is precisely why a consent decision has nothing durable
// to attach to.
//
// `responsibility_spawnattrs_setdisclaim` makes the spawned child its OWN
// responsible process. With it applied, the same record — measured, same run,
// alternating with the control — becomes:
//
//     attribution={accessing={identifier=the-child, …}}     // NO responsible=
//     AUTHREQ_SUBJECT: subject=/path/to/the-child
//
// The consent SUBJECT moves off ui-server and onto the spawned binary. Here
// that binary is `claude`: Developer ID Application: Anthropic PBC
// (Q6L2SF6YDW), hardened runtime, designated requirement keyed to identifier +
// team OU and NOT to a path, so it survives cask updates. That is strictly more
// durable than the identity it replaces. The visible cost is that the prompt
// now names `claude` instead of the operator's own interface. That is the
// honest trade, not a defect — and it does NOT on its own prove that app-data
// consent then persists, which remains open.
//
// It is a PRIVATE libquarantine SPI. It is resolved through `dlsym` rather than
// linked, so that if a future macOS drops it the daemon still launches and
// simply stops disclaiming, instead of failing to start.
//
// WHY THIS GOES THROUGH `pre_exec` AND `POSIX_SPAWN_SETEXEC`, which looks odd.
// The disclaim is a posix_spawn ATTRIBUTE, and `std::process::Command` offers no
// way to reach the `posix_spawnattr_t` it builds internally. Installing a
// `pre_exec` closure forces Rust onto its fork/exec path; from inside the forked
// child we then call `posix_spawnp` with `POSIX_SPAWN_SETEXEC`, which means "do
// not fork — replace THIS image": an exec that carries our attribute. Everything
// Rust does BEFORE the callbacks has already happened and is preserved — dup2 of
// the three stdio handles, chdir to the turn's cwd, and the `setpgid` that gives
// the turn its own process group for the D-8 teardown. What Rust does AFTER the
// callbacks is apply the environment, so this module builds the child's
// environment itself: the daemon's own environment plus the caller's overrides,
// which is exactly what `Command::env` would have produced.
//
// FAILURE IS NON-FATAL BY DESIGN, AND THE RESIDUAL IS STATED RATHER THAN HIDDEN.
// Every check that CAN be made in the parent is made there, where it can be
// logged: the SPI's presence, and every attribute call. If any of that fails the
// closure is never installed and the turn spawns exactly as it did before. If
// the in-child `posix_spawnp` itself fails, the callback returns `Ok` and Rust's
// own `execvp` runs — the operator keeps his turn, and the only thing lost is
// the disclaim. That last case cannot be logged (post-fork, in a threaded
// process), so the honest statement is: a turn can run without the disclaim and
// nothing in this process will say so. The observable that settles it is tccd's
// own attribution record, not this code.
//
// ESCAPE HATCH: `THREADS_DISCLAIM_RESPONSIBILITY=0` (or `false`/`no`/`off`) in
// the daemon's environment turns this off with no rebuild and no code change.
#[cfg(target_os = "macos")]
pub(crate) mod disclaim {
    use std::ffi::{CString, OsString};
    use std::os::unix::ffi::OsStrExt;

    /// `dlsym`'s "search every loaded image" handle. `libc` does not define it
    /// for Apple targets; the value is from `<dlfcn.h>`.
    const RTLD_DEFAULT: *mut libc::c_void = -2isize as *mut libc::c_void;

    const SPI: &[u8] = b"responsibility_spawnattrs_setdisclaim\0";

    type SetDisclaim =
        unsafe extern "C" fn(*mut libc::posix_spawnattr_t, libc::c_int) -> libc::c_int;

    /// `false` only when the operator has explicitly switched it off.
    pub(crate) fn enabled() -> bool {
        match std::env::var("THREADS_DISCLAIM_RESPONSIBILITY") {
            Ok(v) => !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            ),
            Err(_) => true,
        }
    }

    fn setdisclaim_spi() -> Option<SetDisclaim> {
        let p = unsafe { libc::dlsym(RTLD_DEFAULT, SPI.as_ptr() as *const libc::c_char) };
        if p.is_null() {
            None
        } else {
            // SAFETY: the symbol, when present, has exactly this signature —
            // `int(posix_spawnattr_t *, int)`. Verified on this machine.
            Some(unsafe { std::mem::transmute::<*mut libc::c_void, SetDisclaim>(p) })
        }
    }

    /// Everything the forked child needs, allocated ENTIRELY IN THE PARENT.
    ///
    /// Nothing here may allocate after the fork: `malloc` in a forked child of a
    /// threaded process can deadlock on a lock another thread held at fork time.
    /// The child's only job is to call `posix_spawnp` with these prebuilt
    /// pointers.
    pub(crate) struct DisclaimedExec {
        attr: libc::posix_spawnattr_t,
        program: CString,
        argv: Vec<*mut libc::c_char>,
        envp: Vec<*mut libc::c_char>,
        // Owners of the storage `argv`/`envp` point into. Never read directly;
        // dropping them would dangle those pointers.
        _argv_owned: Vec<CString>,
        _envp_owned: Vec<CString>,
    }

    // SAFETY: the raw pointers are owned by this struct and only ever
    // dereferenced by the thread (or forked child) that holds it. `pre_exec`
    // requires `Send + Sync`.
    unsafe impl Send for DisclaimedExec {}
    unsafe impl Sync for DisclaimedExec {}

    impl Drop for DisclaimedExec {
        fn drop(&mut self) {
            unsafe {
                libc::posix_spawnattr_destroy(&mut self.attr);
            }
        }
    }

    impl DisclaimedExec {
        /// Build the disclaiming exec for one spawn, or `None` if it cannot be
        /// armed — in which case the caller must spawn normally.
        ///
        /// `program` and `args` are the same values handed to
        /// `Command::new`/`Command::args`; `env_overrides` is the same list
        /// handed to `Command::env`/`Command::env_remove`, where `None` means
        /// remove.
        pub(crate) fn arm(
            program: &str,
            args: &[String],
            env_overrides: &[(String, Option<String>)],
        ) -> Option<Self> {
            let setdisclaim = setdisclaim_spi()?;

            let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
            // SAFETY: `attr` is a valid out-parameter; every call below is
            // checked and the attr is destroyed on any failure path.
            unsafe {
                if libc::posix_spawnattr_init(&mut attr) != 0 {
                    return None;
                }
                if libc::posix_spawnattr_setflags(
                    &mut attr,
                    libc::POSIX_SPAWN_SETEXEC as libc::c_short,
                ) != 0
                    || setdisclaim(&mut attr, 1) != 0
                {
                    libc::posix_spawnattr_destroy(&mut attr);
                    return None;
                }
            }

            let program_c = match CString::new(program.as_bytes()) {
                Ok(c) => c,
                Err(_) => {
                    unsafe { libc::posix_spawnattr_destroy(&mut attr) };
                    return None;
                }
            };

            // argv[0] is the program as written, matching what `Command` passes
            // to `execvp`.
            let mut argv_owned = vec![program_c.clone()];
            for a in args {
                match CString::new(a.as_bytes()) {
                    Ok(c) => argv_owned.push(c),
                    Err(_) => {
                        unsafe { libc::posix_spawnattr_destroy(&mut attr) };
                        return None;
                    }
                }
            }

            let envp_owned = build_envp(std::env::vars_os(), env_overrides);

            let mut argv: Vec<*mut libc::c_char> = argv_owned
                .iter()
                .map(|c| c.as_ptr() as *mut libc::c_char)
                .collect();
            argv.push(std::ptr::null_mut());
            let mut envp: Vec<*mut libc::c_char> = envp_owned
                .iter()
                .map(|c| c.as_ptr() as *mut libc::c_char)
                .collect();
            envp.push(std::ptr::null_mut());

            Some(Self {
                attr,
                program: program_c,
                argv,
                envp,
                _argv_owned: argv_owned,
                _envp_owned: envp_owned,
            })
        }

        /// Runs INSIDE THE FORKED CHILD, from `pre_exec`. On success this never
        /// returns — the image is replaced. On failure it returns `Ok(())` so
        /// that Rust's own `execvp` still runs and the operator keeps his turn.
        ///
        /// Allocation-free by construction: every pointer was built in `arm`.
        pub(crate) fn exec(&self) -> std::io::Result<()> {
            let mut pid: libc::pid_t = 0;
            // SAFETY: all pointers were built in the parent and outlive this
            // call; `POSIX_SPAWN_SETEXEC` means a successful call does not
            // return.
            unsafe {
                libc::posix_spawnp(
                    &mut pid,
                    self.program.as_ptr(),
                    std::ptr::null(),
                    &self.attr,
                    self.argv.as_ptr(),
                    self.envp.as_ptr(),
                );
            }
            Ok(())
        }
    }

    /// Install the disclaim on `cmd`, or leave `cmd` untouched and return
    /// `false`. `program`, `args` and `env_overrides` must be the SAME values
    /// already given to `Command::new`/`args`/`env`.
    ///
    /// THE SPAWN SEAM CALLS THIS AND SO DOES THE ATTRIBUTION PROBE IN THE TESTS
    /// BELOW. That is the whole reason it exists as a function: the probe is the
    /// only thing that can show the attribution actually moved, and it is worth
    /// nothing if it exercises a copy of the wiring rather than the wiring.
    pub(crate) fn arm_on(
        cmd: &mut tokio::process::Command,
        program: &str,
        args: &[String],
        env_overrides: &[(String, Option<String>)],
    ) -> bool {
        if !enabled() {
            return false;
        }
        match DisclaimedExec::arm(program, args, env_overrides) {
            Some(armed) => {
                // SAFETY: the closure allocates nothing and calls only
                // `posix_spawnp` on pointers built before the fork.
                unsafe {
                    cmd.pre_exec(move || armed.exec());
                }
                true
            }
            None => false,
        }
    }

    /// The environment `Command` would have handed to the child: the inherited
    /// environment, with each override set (or removed, for `None`).
    ///
    /// Split out from `arm` so it is testable without spawning anything. Later
    /// entries win, matching `Command::env`.
    pub(crate) fn build_envp<I>(
        inherited: I,
        overrides: &[(String, Option<String>)],
    ) -> Vec<CString>
    where
        I: IntoIterator<Item = (OsString, OsString)>,
    {
        let mut env: Vec<(Vec<u8>, Vec<u8>)> = inherited
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
            .collect();
        for (k, v) in overrides {
            let key = k.as_bytes().to_vec();
            env.retain(|(ek, _)| ek != &key);
            if let Some(val) = v {
                env.push((key, val.as_bytes().to_vec()));
            }
        }
        env.into_iter()
            .filter_map(|(k, v)| {
                let mut b = k;
                b.push(b'=');
                b.extend_from_slice(&v);
                CString::new(b).ok()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape the retry ladder appended ~2,000 times per thread on
    /// 2026-08-06: the echoed prompt, then a synthetic assistant record whose
    /// only content is the refusal.
    const PROMPT_ECHO: &str = r#"{"type":"user","uuid":"u1","parentUuid":"p0","message":{"role":"user","content":"The plugin set attached to this thread changed. Current plugins:"}}"#;
    const SYNTHETIC_429: &str = r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","isApiErrorMessage":true,"apiErrorStatus":429,"message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"You've hit your weekly limit"}]}}"#;
    const REAL_ANSWER: &str = r#"{"type":"assistant","uuid":"a2","parentUuid":"u1","message":{"model":"claude-opus-5","role":"assistant","content":[{"type":"text","text":"Here is the answer."}]}}"#;
    const COMPACTED: &str = r#"{"type":"system","subtype":"compact_boundary","uuid":"s1","compactMetadata":{"trigger":"auto"}}"#;

    fn tail(lines: &[&str]) -> Vec<u8> {
        let mut s = String::new();
        for l in lines {
            s.push_str(l);
            s.push('\n');
        }
        s.into_bytes()
    }

    #[test]
    fn a_refused_attempt_leaves_a_trimmable_tail() {
        let t = tail(&[PROMPT_ECHO, SYNTHETIC_429]);
        assert_eq!(ThreadManager::trimmable_tail(&t), Some(2));
    }

    #[test]
    fn a_tail_holding_a_real_answer_is_never_trimmed() {
        // The attempt produced something before it died. Keeping junk is the
        // acceptable failure; dropping the operator's conversation is not.
        let t = tail(&[PROMPT_ECHO, REAL_ANSWER, SYNTHETIC_429]);
        assert_eq!(ThreadManager::trimmable_tail(&t), None);
    }

    #[test]
    fn a_tail_holding_a_compaction_boundary_is_never_trimmed() {
        // Truncating across a compaction would strand the transcript on a
        // summary whose boundary no longer exists.
        let t = tail(&[PROMPT_ECHO, COMPACTED, SYNTHETIC_429]);
        assert_eq!(ThreadManager::trimmable_tail(&t), None);
    }

    #[test]
    fn an_unreadable_line_is_never_trimmed() {
        // We do not delete what we cannot read — a half-written last line is
        // exactly the case where guessing is worst.
        let t = tail(&[PROMPT_ECHO, r#"{"type":"assist"#]);
        assert_eq!(ThreadManager::trimmable_tail(&t), None);
    }

    #[test]
    fn an_empty_tail_is_nothing_to_do() {
        assert_eq!(ThreadManager::trimmable_tail(b""), None);
        assert_eq!(ThreadManager::trimmable_tail(b"\n\n  \n"), None);
    }

    // ── THE RESPONSIBILITY DISCLAIM ──────────────────────────────────────
    //
    // What these can prove in-process is that the seam is ARMABLE on this
    // machine and that the environment it hands the child is the one
    // `Command` would have handed it. What they CANNOT prove is the thing
    // that matters — that macOS then attributes the child to itself — because
    // that is observable only in tccd's own `AUTHREQ_ATTRIBUTION` record for a
    // real spawned process. That evidence is in the report, not here, and no
    // green below should be read as standing in for it.

    #[cfg(target_os = "macos")]
    #[test]
    fn the_disclaim_spi_is_present_and_the_seam_arms() {
        // If this fails on some future macOS, the daemon still runs — it just
        // stops disclaiming, and the WARN at the spawn seam says so. This test
        // is how we find out that happened.
        let armed = super::disclaim::DisclaimedExec::arm(
            "/bin/echo",
            &["hello".to_string()],
            &[("SOME_KEY".to_string(), Some("v".to_string()))],
        );
        assert!(
            armed.is_some(),
            "responsibility_spawnattrs_setdisclaim could not be resolved or applied — every \
             claude this daemon spawns will inherit the TCC identity of whatever started it"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_child_environment_is_the_one_command_would_have_built() {
        use std::ffi::OsString;
        let inherited = vec![
            (OsString::from("KEEP"), OsString::from("1")),
            (OsString::from("ANTHROPIC_API_KEY"), OsString::from("sk-x")),
            (OsString::from("CLAUDE_CONFIG_DIR"), OsString::from("/old")),
        ];
        let overrides = vec![
            // `None` is `env_remove` — the API key must not reach the child,
            // which is what makes it use subscription auth.
            ("ANTHROPIC_API_KEY".to_string(), None),
            ("CLAUDE_CONFIG_DIR".to_string(), Some("/new".to_string())),
            ("CLAUDE_CODE_DISABLE_AUTO_MEMORY".to_string(), Some("1".to_string())),
        ];
        let envp = super::disclaim::build_envp(inherited, &overrides);
        let got: Vec<String> = envp
            .iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect();

        assert!(got.contains(&"KEEP=1".to_string()), "inherited vars survive");
        assert!(
            !got.iter().any(|e| e.starts_with("ANTHROPIC_API_KEY=")),
            "a None override REMOVES the variable; leaking the API key here would silently \
             change which account every turn bills to. got: {got:?}"
        );
        assert!(
            got.contains(&"CLAUDE_CONFIG_DIR=/new".to_string()),
            "an override REPLACES rather than duplicates. got: {got:?}"
        );
        assert_eq!(
            got.iter()
                .filter(|e| e.starts_with("CLAUDE_CONFIG_DIR="))
                .count(),
            1,
            "exactly one entry per key — a duplicate would leave which value wins to libc"
        );
        assert!(got.contains(&"CLAUDE_CODE_DISABLE_AUTO_MEMORY=1".to_string()));
    }

    /// MANUAL EVIDENCE, NOT A CI GREEN — and it is ignored because the thing it
    /// proves is not observable from inside this process. Whether macOS moved
    /// the consent attribution is visible ONLY in tccd's own record, so this
    /// test spawns a binary through the REAL seam (`disclaim::arm_on`, the same
    /// call `run_turn` makes) and prints the pid for correlation:
    ///
    ///   DISCLAIM_PROBE_BIN=<a binary macOS has never seen> \
    ///   DISCLAIM_PROBE_MODE=plain|disclaim \
    ///     cargo test -p threads --lib disclaim_attribution_probe -- --ignored --nocapture
    ///
    ///   /usr/bin/log show --style compact --predicate 'subsystem == "com.apple.TCC"' \
    ///     --last 5m | grep <the binary>
    ///
    /// Use `/usr/bin/log` with the ABSOLUTE path: a bare `log show` in zsh hits
    /// a shell builtin and returns empty without erroring, so a broken query and
    /// a quiet system look identical. The binary must be one syspolicyd has not
    /// already evaluated — it emits the attribution record on FIRST exec only,
    /// so a reused binary yields silence that proves nothing.
    ///
    /// PLAIN yields `responsible={… responsible_path=…/ui-server …}`.
    /// DISCLAIM yields no `responsible=` block at all and an `AUTHREQ_SUBJECT`
    /// naming the spawned binary itself. That difference IS the change.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manual: the attribution it proves is observable only in tccd's log"]
    fn disclaim_attribution_probe() {
        let bin = std::env::var("DISCLAIM_PROBE_BIN")
            .expect("set DISCLAIM_PROBE_BIN to a binary macOS has never executed");
        let plain = std::env::var("DISCLAIM_PROBE_MODE").as_deref() == Ok("plain");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // The seam's own shape: same Command type, same process_group, same
            // arming call.
            let mut cmd = Command::new(&bin);
            cmd.process_group(0);
            let armed = if plain {
                false
            } else {
                super::disclaim::arm_on(&mut cmd, &bin, &[], &[])
            };
            assert_eq!(armed, !plain, "the seam did not arm as asked");
            let mut child = cmd.spawn().expect("spawn");
            println!(
                "PROBE mode={} bin={bin} pid={:?}",
                if plain { "plain" } else { "disclaim" },
                child.id()
            );
            let st = child.wait().await.expect("wait");
            assert!(st.success(), "probe binary exited {st:?}");
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_escape_hatch_is_off_only_when_explicitly_set() {
        // The operator's one-line revert. Default MUST be on, or landing this
        // change would do nothing and the report would be wrong.
        std::env::remove_var("THREADS_DISCLAIM_RESPONSIBILITY");
        assert!(super::disclaim::enabled(), "unset means ON");
        for off in ["0", "false", "no", "off", "OFF", " false "] {
            std::env::set_var("THREADS_DISCLAIM_RESPONSIBILITY", off);
            assert!(!super::disclaim::enabled(), "{off:?} must disable");
        }
        for on in ["1", "true", "yes", ""] {
            std::env::set_var("THREADS_DISCLAIM_RESPONSIBILITY", on);
            assert!(super::disclaim::enabled(), "{on:?} must not disable");
        }
        std::env::remove_var("THREADS_DISCLAIM_RESPONSIBILITY");
    }

    #[test]
    fn bookkeeping_records_do_not_block_a_trim() {
        // The daemon's own sidebands (queue-operation, last-prompt, ai-title,
        // mode) carry no conversation and must not pin a worthless tail.
        let t = tail(&[
            r#"{"type":"queue-operation","op":"deliver"}"#,
            PROMPT_ECHO,
            r#"{"type":"last-prompt","text":"x"}"#,
            SYNTHETIC_429,
        ]);
        assert_eq!(ThreadManager::trimmable_tail(&t), Some(4));
    }
}

/// **A TURN'S `--mcp-config` FILE**, removed when the turn ends (R4.S5). It
/// holds the run token, so it does not outlive the window the token is valid
/// in. A daemon restart mid-turn leaves it behind for the re-adopted child
/// (claude read it at start); the token in it dies with the turn's
/// `running_turns` row either way.
struct TurnMcpConfig(PathBuf);

impl TurnMcpConfig {
    fn path(&self) -> String {
        self.0.display().to_string()
    }
}

impl Drop for TurnMcpConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
