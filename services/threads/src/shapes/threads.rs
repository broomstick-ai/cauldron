//! # The `threads` shape — one thread in depth, and NOTHING an application means
//!
//! The GENERAL shape of a thread system. It carries what any consumer of
//! threads wants — the header, the settings blob, the derived status, the
//! placement, the durable positions, the compaction marker, the beat's level,
//! and the application's own events HELD OPAQUELY — and it interprets none of
//! it. Operator ruling, recorded in `docs/consumer-migration-ledger.md`:
//!
//! > *"Application logic belongs in application-specific shapes. The general
//! > `threads` shape carries what any consumer of a thread system wants —
//! > header, status, placement, durable positions, and app event payloads held
//! > opaquely. Anything that INTERPRETS those payloads — the summary/title
//! > derivation being the example — is a separate application-specific shape.
//! > A general shape must not encode one app's meaning."*
//!
//! So [`ThreadCells::app`] is a `Map<event_type, Register<Value>>` and the
//! `Value` is never read by this file. The one thing everybody wants out of it
//! — a thread's NAME, which is the newest `summary_updated` — is derived by
//! [`crate::shapes::summary`], a separate shape, nested under this one.
//!
//! ## Keys, and why `KeyScope::OnDemand` is the only affordable answer
//!
//! **A key is a thread id, and the default key set is EMPTY.** A keyed
//! subscribe naming a thread the root does not hold calls
//! [`Threads::hydrates_key`]; a KEYLESS subscribe is refused on the request id
//! with `ShapeError::KeyRequired`.
//!
//! The measurement that decides it (`docs/thread-shapes-plan.md` §0, read off
//! the production store): **5,417 threads, and the key set is MONOTONE —
//! there is no thread delete path anywhere in this crate.** A `Fleet` key
//! scope would therefore make a keyless subscribe 5,417 exports at `accounts`'
//! measured 4.1 KB each: >20 MB of frames into a channel whose capacity is
//! 1,024 (`docs/shapes.md` §3.7), and the repair for the resulting lag is to
//! re-export every key held, which is the same 20 MB again. `shapes.list`
//! would serialise 5,417 × 36-char ids on a method the board polls.
//!
//! `Active` (only non-idle threads are keys) is cheap and is what
//! `accounts`' nested `threads` sub-map ALREADY does — which is the second
//! reason this shape is not `Active`: the fleet's live set has a home already,
//! and a second shape answering the same question would be the second source
//! S3 spent a stage deleting. What has NO home today is one thread in depth,
//! and that thread is idle between turns, which is most of the time — so
//! `Active` is exactly the scope that cannot serve it.
//!
//! **Three things fall out of `OnDemand` that are worth naming:**
//!
//! 1. **`docs/shapes.md` §7.2's keyless-enumeration gap cannot be reached
//!    here.** That defect is "a key created after a keyless subscribe produces
//!    `row` frames the connection's gate drops, because it never had an export
//!    for that key". This shape has no keyless subscribe, and — see
//!    [`Threads::folds`] — its fold NEVER MINTS A KEY. Every key this root
//!    holds was created by a `shapes.subscribe` whose export opened the gate
//!    for it. The plan called §7.2 a hard prerequisite for any thread-keyed
//!    shape; it is a prerequisite for the `Fleet` and `Active` scopes only.
//! 2. **The fold's scope guard is what keeps the root bounded.** 2,463,981
//!    ledger events exist; if `Push::Event` for an unheld thread created its
//!    key, the root would grow to the whole fleet in minutes. So every arm
//!    starts by asking whether the root already holds the key and returns if
//!    not. How much of the input that discards is on `shapes.list` as
//!    `events_ignored`.
//! 3. **The root grows only inside one connection's lifetime.** `Tracking::Lazy`
//!    drops the root on the last unsubscribe with no grace period
//!    (`docs/shapes.md` §3.9), so the demand-hydrated set cannot outlive the
//!    consumers that asked for it. `docs/chassis-resources.md` 6.1's
//!    per-subscription scope exit is not built and is not needed for that
//!    bound; it would only shrink the set *within* a long-lived connection.
//!
//! ## Tracking
//!
//! `Lazy`, and **not for `accounts`' reason.** Nothing here spawns a process or
//! reads a keychain; every hydrate is SQLite. What `Lazy` buys is the FOLD:
//! `Push::Event` is the highest-volume input this daemon has, `Hub::emit_at`
//! cannot be gated on `any_tracking()` (the firehose must always get it), so a
//! tracking shape costs one FIFO push plus one `folds` call per ledger event at
//! a measured ~50 events/s peak. `Lazy` is what makes an unwatched daemon pay
//! none of it, and the fold's first act on the three highest-volume kinds
//! (`stream_event` 674,493 rows, `message_usage` 431,018, `unknown` 333,422 —
//! 58% of the ledger) is to return.
//!
//! ## The hydrate bound
//!
//! **ELEVEN statements per key, flat in the fleet size and flat in the activity
//! level, and STILL eleven now that the newest message's body is summarised.**
//! Six requests: `ThreadRowRead` (1 — a primary-key `get_thread`),
//! `ThreadNewestEvent` (1 — one seek on `idx_events_thread_seq`, reading ONE
//! MORE COLUMN of the row it already read so that [`ThreadCells::body`] costs
//! no statement; a `newest_body` query of its own would have been a twelfth,
//! on a path `tests/recent_hydrate_cost.rs` counts), `ThreadMessageHead` (1),
//! `ThreadAppState` (1 — `idx_events_thread_kind` plus one PK join per event
//! type, and the app's whole vocabulary is 7 types), `ThreadLiveness` (0 — one
//! mutex, no statement), `ThreadDerivedState` (6 — `thread_states`' five
//! whole-table reads of one-row-per-*interesting*-thread tables, plus one
//! `unread_count`). Plus `ThreadCompactionRange` inside the row read, and
//! `FleetSize` ONCE per epoch, never per key.
//!
//! There is no `list_threads` and no per-id loop anywhere in this file. That is
//! deliberate: the `accounts` shape shipped a `list_threads(100_000)` plus an
//! N+1 `get_thread` that was invisible only because the tables it walked were
//! empty (`tests/hydrate_cost.rs` now pins the repair), and this is the shape
//! where the same mistake would be 5,417 times worse.
//!
//! ## What is NOT here
//!
//! * **No messages cell, and no `Sequence` of the log.** `Sequence` has **no
//!   `read`** (`shapewire::fold`), so a tail would be `since_init` from the
//!   instant of subscription — strictly worse than the `threads.history
//!   {after_seq}` cursor AUI already uses. VERIFIED against the code rather
//!   than taken from the plan: `Sequence` has `emit`, `mark_since_init`, `now`,
//!   `to_json`, `take_pending`, `len`, `is_empty`, `state`, `mark_dead` and
//!   nothing that accepts an `as_of`. So the division is: **the shape says
//!   WHERE the log is, the RPC says WHAT is in it.** `newest`,
//!   `last_message_id`, `seen_seq` and `unread` are the WHERE;
//!   `threads.history` keeps the WHAT.
//! * **No `next_message_id`.** `threads.get` returns it; it is
//!   `last_message_id + 1`, and `docs/shapes.md` §4.2's rule is that a position
//!   difference is derived at render. Carrying both would put two writers (the
//!   `threads.next_message_id` column, bumped by `alloc_message_id`, and the
//!   ledger) behind one number.
//! * **No `last_seq` column.** `threads.last_seq` is a CACHE of the ledger's
//!   head, and `import.rs` inserts events without `bump_last_seq` — so the
//!   column can lag its own log. `docs/thread-shapes-plan.md` §3.4 asks the
//!   shape to pick one source; this picks the LEDGER, because the number exists
//!   for a consumer to compare its cursor against and a stale head makes that
//!   comparison wrong. [`NewestRow::seq`] is that number, read from `events`.
//!   **This is a deliberate divergence from `threads.get`'s `last_seq`** and the
//!   only one in this file.
//! * **No `status` legacy word.** `DerivedState::status_word` is a projection of
//!   `state`; the cell carries `state`, `executing` and `detail`, and a consumer
//!   that needs the three-word vocabulary projects it itself.
//! * **No `Counter` cell.** `threads` retains its whole ledger, so every
//!   apparent counter is a difference of durable positions or a `count` over
//!   held rows — except [`ThreadCells::unread`], which is neither, and the cell
//!   carries the argument.
//! * **No clock.** Every `.emit()` takes the input's stamp.
//!
//! ## Version
//!
//! [`Threads::VERSION`] is `1`, and it rides on `shared` as `shape_version`.
//! `Shape` has no `VERSION`/`SERVES` const — rule 2.6 is entirely unbuilt
//! (`docs/thread-shapes-plan.md` §8.1 verified the trait carries neither) — and
//! adding one means editing `shapewire::hub` and `::wire`, which a sibling
//! dispatch owns. What IS declared here is the number itself, where a consumer
//! and the subscriber ledger's `shape_version` can both see it. **What is left
//! out: `SERVES`, the three-way refusal at link time, and the `superseded`
//! warning on `Epoch`.** Declaring the number is not implementing the rule.

use serde::Serialize;
use serde_json::{json, Value};

use crate::event::{Fact, Push};
use shapewire::cell::Cells;
use shapewire::declare_cells;
use shapewire::operation::{
    DType::{Cursor, Enum, List, Map as MapOf, Opaque, Path, Ref, Scalar as Sc, Struct, Timestamp as Ts},
    Presence::{Optional, Required},
    Scalar::{Bool, Int, Str},
    TimeEncoding::{Rfc3339},
};
use crate::shape::{
    Edge, Handles, Hydrate, HydrateRequest, Hydration, Input, Map, Membership, Register, Root,
    Shape, Stamped,
    Timestamp, Tracking, Tx,
};

/// The ledger kinds the fold discards before it does anything else — 58% of the
/// 2,463,981 rows in the production store. A constant so the fold and the
/// counter cannot disagree about which they are.
pub const BULK_KINDS: &[&str] = &["stream_event", "message_usage", "unknown"];

// ─────────────────────────────────────────────────────────────────────────────
// Row types — each is the ONE type both the fact and the hydrate response carry
// ─────────────────────────────────────────────────────────────────────────────

/// **The header, which moves by WHOLESALE REPLACEMENT.** `upsert_thread` writes
/// the row and `map_thread` reads it; there is no field-at-a-time write, so this
/// is one `Register` and not four.
///
/// `created_at`/`updated_at` are carried as the daemon stores them and nothing
/// derived from them is carried: no `age`, no `idle_for`. Cells carry the
/// timestamp and the consumer subtracts.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HeaderRow {
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// **The compaction marker.** The two durable fields are written together by
/// `update_thread_compaction`; `range` is the message-id span the last
/// compaction swallowed, which lives in the `Compacted` event's payload.
///
/// One row and ONE writer: the fold does not read `Push::Event(Compacted)`,
/// because the fact emitted beside that write already carries the range, and two
/// writers behind one cell is how a mirror learns to disagree with itself.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct CompactionRow {
    pub last_compaction_message_id: u64,
    pub needs_restate: bool,
    pub range: Option<[u64; 2]>,
}

/// **THE THREAD'S CURRENT CONTEXT ID — `<thread_id>.<last_compaction_message_id>`.**
///
/// The `context_id` cell (R2.S3d). A consumer that keys a cache on "the context
/// this thread is in now" — the harness's `mind-index-v5` delta reads, which
/// serve full content on an unknown context — used to ask `threads/{id}/get` for
/// it and got nothing, because `get` never carried one. The producer already
/// derives it, ONCE, in [`crate::context::context_id`], for the `context:` line
/// it injects into a thread's own prompt; this calls that function and nothing
/// else, so the id a consumer reads off the shape and the id the agent sees in
/// its prompt are one derivation and cannot disagree.
///
/// **It is computed at export off [`ThreadCells::compaction`], not held in a
/// register of its own.** `context.rs` gives the reason in its own words — *"the
/// id cannot disagree with the boundary, because it IS the boundary"* — and a
/// second register written beside the first would be the second place this
/// crate has three times learned to regret. Its cell state is therefore the
/// compaction marker's state.
///
/// Published whatever `inject_context_id` says: that flag decides whether the
/// AGENT is shown the line, and the context a thread is in exists either way.
pub fn context_id_of(thread_id: &str, compaction: &CompactionRow) -> String {
    crate::context::context_id(thread_id, compaction.last_compaction_message_id)
}

/// **The status LEVEL, and the thing `Edge` exists for.**
///
/// `docs/shapes.md` §4.2 step 4 — *"do you need to know that X CHANGED, not only
/// what it is?"* — and here two consumers need the two halves for two reasons: a
/// fleet view COUNTS the level, and a speaking interface narrates the
/// TRANSITION. `Edge` hydrates the level and reports its transitions
/// `since_init`, which is honest: the daemon keeps no status history to hydrate
/// from, and `schema.sql` deliberately has no `status` column at all.
///
/// **This is `Edge`'s first instance on any producer.** `docs/shapes.md` §5
/// records that it had none.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StateLevel {
    /// `active` | `queued` | `parked` | `retrying` | `idle` — the vocabulary the
    /// operator named, derived once by `ThreadManager::thread_states`.
    pub state: String,
    /// An execution holds this thread's durable lock right now.
    pub executing: bool,
    /// The park or retry reason, when there is one.
    pub detail: Option<String>,
}

/// **WHERE THE LOG'S HEAD IS.** `seq` is the durable position a consumer
/// compares its own cursor against; `kind` and `ts` say what the last thing to
/// happen was, so a consumer can tell "nothing has happened" from "I have not
/// been told".
///
/// Ordered on `seq` by the fold, NOT by `Register::emit_ordered`. The plan's D5
/// offered the event's `ts` or a new chassis `emit_ordered_by`; the code refuses
/// both. `emit_ordered` can only order on a `Timestamp`, and `ts` is a wall
/// clock that can move backwards — which would DISCARD a real event — while
/// `seq` is monotone per thread by construction. So the ordering key is `seq`,
/// compared in [`Threads::folds`] against the held row, and no chassis change is
/// needed.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NewestRow {
    pub seq: u64,
    pub message_id: Option<u64>,
    pub kind: String,
    pub ts: String,
}

/// **THE BEAT'S LEVEL — `Push::Liveness`' home.**
///
/// `accounts` refuses this input as transport noise and is right to: membership
/// on a card is `executing`, which comes from the `Status` EDGE, so a 1 Hz beat
/// repeating a level nobody derives from it adds nothing there. It is a
/// different question here, and the operator's ruling is that every `Push`
/// variant gets a home rather than keeping the firehose alive:
///
/// * `running_subagents` **exists nowhere else.** No RPC returns it, no other
///   frame carries it, and it is the only answer to "is this turn still doing
///   something, or is the parent just sitting there".
/// * It is a LEVEL, not a log, so it is a `Register` and not a `Sequence`. A
///   64-item narration window fed 1 Hz beats from three running threads holds
///   twenty-one seconds of beats and nothing else — which is why this is not a
///   `narration` line.
/// * **The fold drops a beat that repeats the held level** ([`Threads::folds`]),
///   so a steady turn costs one frame at its start and one at its end, not one
///   per second. A beat is only news when it says something new.
///
/// `busy` is THIS PROCESS's in-memory view of the turn and `status.executing` is
/// the durable lock. They are different facts and can legitimately differ: a
/// thread locked by another daemon reads `executing: true` with `busy: false`.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct LivenessRow {
    pub busy: bool,
    pub parent_running: bool,
    pub running_subagents: Vec<String>,
}

/// **A PARENT'S MANAGED CHILDREN — the lineage facts a face needs to say
/// "waiting on N subagents"** (`docs/TUI-SUBAGENTS.md` §4.1, M1/M4).
///
/// Read off the parent's OWN managed ledger and nothing else: `outstanding` is
/// [`crate::ledger::spawn::outstanding`] verbatim (one owner of the wake rule),
/// `runs` is one entry per top-level `child{started}`, with its matching
/// `child{finished}` folded in. Lineage only — a child's live state stays on the
/// child's own row, so this never duplicates `status`.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct ChildrenRow {
    pub outstanding: Vec<String>,
    pub runs: Vec<ChildRun>,
}

/// One committed spawn, as the parent's ledger records it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChildRun {
    pub id: String,
    pub agent: Option<String>,
    pub name: Option<String>,
    /// `end` or `never` — `agents_spawn`'s `wake`.
    pub wake: String,
    pub started_at: String,
    pub request_id: String,
    pub finished: Option<ChildFinished>,
    /// **THE CHILD'S LAST ACTIVITY** (TC-44's recency key), RFC3339 UTC: the
    /// time of the newest event on the CHILD's own log — a turn start or end,
    /// a message in or out, a tool call. `null` for a child with no event yet.
    /// Hydrated by [`children_row_at`] (one statement for every run), then
    /// moved live by [`Fact::ThreadChildActivity`], at most once per
    /// wall-clock second per child. ABSENT (not `null`) until the child has an
    /// event, so the pure ledger builder's output is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
}

/// [`children_row`] with each run's `last_activity_at` filled from the store —
/// ONE statement for all the runs (`Store::newest_event_ts_many`). A failed
/// read leaves the field `null` and says so in the log; the lineage itself is
/// still the ledger's complete answer.
pub fn children_row_at(l: &crate::ledger::Ledger, store: &crate::store::Store) -> ChildrenRow {
    let mut row = children_row(l);
    if row.runs.is_empty() {
        return row;
    }
    let ids: Vec<String> = row.runs.iter().map(|r| r.id.clone()).collect();
    match store.newest_event_ts_many(&ids) {
        Ok(ts) => {
            for r in &mut row.runs {
                r.last_activity_at = ts.get(&r.id).cloned();
            }
        }
        Err(e) => tracing::warn!(parent = %l.thread_id(), error = %e, "children: last activity could not be read"),
    }
    row
}

/// A child's report: the ledger's own outcome vocabulary (`ok|failed|interrupted`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChildFinished {
    pub outcome: String,
    pub at: String,
}

/// **THE ONE BUILDER** — the hydrate and the fact both go through it, so the
/// cell a subscriber hydrates and the level a later write emits cannot differ.
pub fn children_row(l: &crate::ledger::Ledger) -> ChildrenRow {
    use crate::ledger::Kind;
    let mut runs: Vec<ChildRun> = Vec::new();
    for e in l.events() {
        if e.kind != Kind::Child || e.within.is_some() {
            continue;
        }
        let Some(cid) = e.body["child_id"].as_str() else { continue };
        match e.body["phase"].as_str() {
            Some("started") => {
                let d = &e.body["detail"];
                runs.push(ChildRun {
                    id: cid.to_string(),
                    agent: d["agent"].as_str().map(str::to_string),
                    name: d["name"].as_str().map(str::to_string),
                    wake: d["wake"].as_str().unwrap_or("end").to_string(),
                    started_at: e.ts.clone(),
                    request_id: e.id.clone(),
                    finished: None,
                    last_activity_at: None,
                });
            }
            Some("finished") => {
                if let Some(r) = runs.iter_mut().find(|r| r.id == cid && r.finished.is_none()) {
                    r.finished = Some(ChildFinished {
                        outcome: e.body["outcome"].as_str().unwrap_or("").to_string(),
                        at: e.ts.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    ChildrenRow {
        outstanding: crate::ledger::spawn::outstanding(l).into_iter().collect(),
        runs,
    }
}

/// **A THREAD'S STATUS, ROLLED UP OVER ITS WHOLE SUBTREE** (R9.TS1).
///
/// Operator ruling (round 42): a thread's status is `active`, `idle` or an
/// error state, and nothing else. Every field here is computed by
/// `shapewire::rolled` — the ONE implementation every Rust face links — and
/// the strings a face shows are published here already rendered, so a face
/// that is not Rust (the Python board) renders `display` and
/// `children_summary` verbatim and needs no logic at all.
///
/// * `status` — `active` (the thread or ANY descendant, at any depth, is
///   running), else `error` (the thread ITSELF is in an error state — an error
///   does NOT roll up to a parent, which counts it in `children_error`), else
///   `idle`. `active` is `shapewire::subtree::rollup`'s rules 1–2 (R9.UI1's
///   UI-b), applied here to EVERY thread — a subagent's row carries its own
///   subtree's answer exactly as a root's does.
/// * `error` — the thread's own error label when `status` is `error`: `parked`
///   (the derivation's park), `failed` or `interrupted` (the outcome its LAST
///   managed run reported, read off its parent's ledger).
/// * `active_descendants` — descendants at every depth that are RUNNING
///   THEMSELVES (a descendant active only by rollup is not counted: root →
///   idle child → running grandchild reads `active (+1)` at the root AND at
///   the child).
/// * `display` — `shapewire::rolled::display`: `active (+X)`, `active` (never
///   `(+0)`), `idle`, `error: parked|failed|interrupted`.
/// * `children_*` — over DIRECT children, each by its own rolled status;
///   `null` for a thread with no children. `children_summary` is
///   `N active, M idle`, plus `, K error` only when K > 0.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RolledRow {
    pub status: String,
    pub error: Option<String>,
    pub active_descendants: u64,
    pub display: String,
    pub children_active: Option<u64>,
    pub children_idle: Option<u64>,
    pub children_error: Option<u64>,
    pub children_summary: Option<String>,
}

impl From<&shapewire::rolled::Rolled> for RolledRow {
    fn from(r: &shapewire::rolled::Rolled) -> Self {
        RolledRow {
            status: r.status.to_string(),
            error: r.error.map(str::to_string),
            active_descendants: r.active_descendants,
            display: r.display(),
            children_active: r.children.map(|c| c.active),
            children_idle: r.children.map(|c| c.idle),
            children_error: r.children.map(|c| c.error),
            children_summary: r.children_summary(),
        }
    }
}

/// **ONE IN-PROCESS (Task / Agent tool) SUBAGENT, AS THE DAEMON KNOWS IT**
/// (R9.TS2, TC-10) — one value of the `subagents` cell, keyed by the
/// `tool_use_id` of the `Agent`/`Task` call that launched it.
///
/// * `status` — `active` (launched and not yet over), `idle` (over, cleanly),
///   or `error`. One of [`shapewire::rolled::STATUS_WORDS`].
/// * `error` — when `status` is `error`: `failed` (the CLI said `failed`, or
///   the launching tool call came back `is_error`) or `interrupted` (the CLI
///   said `stopped`/`killed`, or the `claude` process exited with the subagent
///   still running). One of [`shapewire::rolled::ERROR_LABELS`].
/// * `display` — the title the model gave the call (`description`), else its
///   `subagent_type`, else `subagent`.
/// * `task_id` — the CLI's task id (`agentId` / `task_started.task_id`) once
///   the CLI has assigned one; the join key to `liveness.running_subagents`.
/// * `parent_tool_use_id` — the `tool_use_id` of the subagent that made this
///   call, `null` at the top level: a face draws depth 2+ from it.
/// * `last_activity_at` — RFC3339 UTC: when the daemon last saw this subagent
///   act (its call, its own messages / thinking / tool calls / tool results —
///   every stream line whose `parent_tool_use_id` is its key — a
///   `task_progress` beat naming it, and every status move). The recency key
///   for TC-44. Moves at most once per wall-clock SECOND (a status move always
///   moves it), so a busy subagent costs at most one frame a second; it is the
///   time of the first activity seen in that second.
///
/// Read off the daemon's own stream accounting ([`TaskRoster`]), never derived
/// from transcript text by a face.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SubagentRow {
    pub status: String,
    pub error: Option<String>,
    pub display: String,
    pub task_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
    pub last_activity_at: String,
}

/// How many Task subagents one thread's roster holds. The oldest FINISHED rows
/// are dropped first; an `active` row is never dropped to make room.
pub const MAX_TASK_ROWS: usize = 64;

#[derive(Debug, Clone)]
struct TaskEntry {
    row: SubagentRow,
    /// Launched in the background (`run_in_background`, or the CLI answered
    /// `isAsync`): its `tool_result` is the launch ack, not its end.
    background: bool,
    order: u64,
}

/// **THE DAEMON'S PER-THREAD ROSTER OF IN-PROCESS SUBAGENTS** (R9.TS2).
///
/// Held in memory beside `Liveness::outstanding` (the source
/// `liveness.running_subagents` comes from) and fed by the same stream arms in
/// `ThreadManager::interpret`, plus the process-exit edge. Every method
/// answers whether the roster CHANGED, so the caller emits
/// `Fact::ThreadSubagents` only on news.
///
/// In memory, like `outstanding`: a daemon restart starts every thread's
/// roster empty. A subagent cannot outlive the `claude` process it runs in.
#[derive(Debug, Default, Clone)]
pub struct TaskRoster {
    rows: std::collections::BTreeMap<String, TaskEntry>,
    next: u64,
}

/// The CLI's terminal word → `(status, error)`. `completed` (and a word we
/// cannot read) is `idle`; `failed` is `error: failed`; `stopped` / `killed`
/// are `error: interrupted`.
fn terminal_of(word: Option<&str>) -> (&'static str, Option<&'static str>) {
    use shapewire::rolled::{ERROR, ERROR_FAILED, ERROR_INTERRUPTED, IDLE};
    match word {
        Some("failed") => (ERROR, Some(ERROR_FAILED)),
        Some("stopped") | Some("killed") => (ERROR, Some(ERROR_INTERRUPTED)),
        _ => (IDLE, None),
    }
}

impl TaskRoster {
    /// The cell's value: every held row, keyed by launching `tool_use_id`.
    pub fn rows(&self) -> std::collections::BTreeMap<String, SubagentRow> {
        self.rows.iter().map(|(k, e)| (k.clone(), e.row.clone())).collect()
    }

    /// An `Agent`/`Task` tool call: the subagent is `active` from here.
    pub fn spawn(
        &mut self,
        tool_use_id: &str,
        display: Option<&str>,
        background: bool,
        parent_tool_use_id: Option<&str>,
        now: &str,
    ) -> bool {
        if tool_use_id.is_empty() {
            return false;
        }
        let order = self.next;
        self.next += 1;
        let row = SubagentRow {
            status: shapewire::rolled::ACTIVE.to_string(),
            error: None,
            display: display.filter(|s| !s.trim().is_empty()).unwrap_or("subagent").to_string(),
            task_id: None,
            parent_tool_use_id: parent_tool_use_id.map(str::to_string),
            last_activity_at: now.to_string(),
        };
        let prev = self.rows.insert(
            tool_use_id.to_string(),
            TaskEntry { row, background, order },
        );
        self.trim();
        prev.map(|p| p.row) != self.rows.get(tool_use_id).map(|e| e.row.clone())
    }

    /// The CLI's `tool_use_result` naming the agent: the task id, and whether
    /// it was launched in the background. A synchronous result whose status is
    /// already `completed` ends it.
    pub fn launched(&mut self, tool_use_id: &str, agent_id: &str, is_async: bool, status: &str, now: &str) -> bool {
        let Some(e) = self.rows.get_mut(tool_use_id) else {
            return false;
        };
        let before = e.row.clone();
        if e.row.task_id.is_none() && !agent_id.is_empty() {
            e.row.task_id = Some(agent_id.to_string());
        }
        if is_async {
            e.background = true;
        } else if status == "completed" {
            Self::end(e, Some("completed"));
        }
        Self::moved(e, &before, now)
    }

    /// `system/task_started`: joins the task id onto the call. A `local_agent`
    /// whose call this roster never saw (it started before the daemon read the
    /// stream) is added, `active`, titled by its own description.
    pub fn started(
        &mut self,
        task_id: &str,
        tool_use_id: Option<&str>,
        description: Option<&str>,
        task_type: Option<&str>,
        now: &str,
    ) -> bool {
        let Some(tu) = tool_use_id.filter(|s| !s.is_empty()) else {
            return false;
        };
        if !self.rows.contains_key(tu) {
            if task_type != Some("local_agent") {
                return false;
            }
            self.spawn(tu, description, false, None, now);
        }
        let e = self.rows.get_mut(tu).expect("present");
        let before = e.row.clone();
        if e.row.task_id.is_none() {
            e.row.task_id = Some(task_id.to_string());
        }
        Self::moved(e, &before, now) | Self::touched(e, now)
    }

    /// `system/task_updated` / `task_notification`: the CLI's own word that a
    /// task is over (`status` refines how). Found by `tool_use_id` when the
    /// line carries one, else by `task_id`. `None` status with no end is a
    /// patch (`is_backgrounded`) and ends nothing.
    pub fn ended(&mut self, task_id: &str, tool_use_id: Option<&str>, status: Option<&str>, now: &str) -> bool {
        let key = tool_use_id
            .filter(|t| self.rows.contains_key(*t))
            .map(str::to_string)
            .or_else(|| {
                self.rows
                    .iter()
                    .find(|(_, e)| e.row.task_id.as_deref() == Some(task_id))
                    .map(|(k, _)| k.clone())
            });
        let Some(key) = key else { return false };
        let e = self.rows.get_mut(&key).expect("present");
        let before = e.row.clone();
        if e.row.task_id.is_none() && !task_id.is_empty() {
            e.row.task_id = Some(task_id.to_string());
        }
        Self::end(e, status);
        Self::moved(e, &before, now)
    }

    /// `is_backgrounded` on a `task_updated` patch: its `tool_result` will be
    /// a launch ack from here on.
    pub fn backgrounded(&mut self, task_id: &str) {
        if let Some(e) = self.rows.values_mut().find(|e| e.row.task_id.as_deref() == Some(task_id)) {
            e.background = true;
        }
    }

    /// The `tool_result` of the launching call. For a foreground subagent it IS
    /// the end (`is_error` → `error: failed`); for a background one it is the
    /// launch ack and ends nothing.
    pub fn tool_result(&mut self, tool_use_id: &str, is_error: bool, now: &str) -> bool {
        let Some(e) = self.rows.get_mut(tool_use_id) else {
            return false;
        };
        if e.background {
            return Self::touched(e, now);
        }
        let before = e.row.clone();
        Self::end(e, Some(if is_error { "failed" } else { "completed" }));
        Self::moved(e, &before, now) | Self::touched(e, now)
    }

    /// The subagent keyed `tool_use_id` ACTED (a line it wrote, a progress
    /// beat naming it): moves `last_activity_at` if the second has turned.
    pub fn touch(&mut self, tool_use_id: &str, now: &str) -> bool {
        match self.rows.get_mut(tool_use_id) {
            Some(e) => Self::touched(e, now),
            None => false,
        }
    }

    /// The `claude` process exited: a subagent still `active` cannot be
    /// running any more, and nothing said it ended — `error: interrupted`.
    pub fn process_exited(&mut self, now: &str) -> bool {
        let mut changed = false;
        for e in self.rows.values_mut() {
            if e.row.status == shapewire::rolled::ACTIVE {
                Self::end(e, Some("stopped"));
                e.row.last_activity_at = now.to_string();
                changed = true;
            }
        }
        changed
    }

    /// A status / task-id move always stamps the activity time.
    fn moved(e: &mut TaskEntry, before: &SubagentRow, now: &str) -> bool {
        if e.row != *before {
            e.row.last_activity_at = now.to_string();
            true
        } else {
            false
        }
    }

    /// Activity with no status move: stamps only when the wall-clock SECOND
    /// has turned (RFC3339's first 19 characters), so the cell moves at most
    /// once a second per subagent.
    fn touched(e: &mut TaskEntry, now: &str) -> bool {
        let sec = |s: &str| s.get(..19).map(str::to_string).unwrap_or_else(|| s.to_string());
        if sec(now) > sec(&e.row.last_activity_at) {
            e.row.last_activity_at = now.to_string();
            true
        } else {
            false
        }
    }

    /// An end applies to a running subagent; a LATER error may also correct an
    /// earlier clean end (a `tool_result` then a `failed` notification), but a
    /// clean word never clears an error.
    fn end(e: &mut TaskEntry, word: Option<&str>) {
        let (status, error) = terminal_of(word);
        let running = e.row.status == shapewire::rolled::ACTIVE;
        let correcting = e.row.status == shapewire::rolled::IDLE && status == shapewire::rolled::ERROR;
        if running || correcting {
            e.row.status = status.to_string();
            e.row.error = error.map(str::to_string);
        }
    }

    fn trim(&mut self) {
        while self.rows.len() > MAX_TASK_ROWS {
            let victim = self
                .rows
                .iter()
                .filter(|(_, e)| e.row.status != shapewire::rolled::ACTIVE)
                .min_by_key(|(_, e)| e.order)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    self.rows.remove(&k);
                }
                None => break,
            }
        }
    }
}

/// **THE ONE BUILDER** for [`RolledRow`] — the hydrate and the live fact both
/// go through it. Answers a row for every id in `ids` that is in `top`'s
/// subtree (`top` itself included).
///
/// Cost: the subtree walk reads each thread's MANAGED ledger once (files, not
/// SQL) — its children and each child's last run outcome — and the states are
/// ONE call to `ThreadManager::thread_states`: its five whole-table reads, flat
/// in the subtree size and in the fleet.
pub fn rolled_rows(
    m: &crate::runtime::ThreadManager,
    top: &str,
    ids: &[String],
) -> crate::error::Result<Vec<(String, RolledRow)>> {
    let mg = m.managed();
    // ONE load of each managed ledger in the subtree: its children (the
    // committed spawns, as `children_row` reads them) and the outcome each
    // child's LAST run reported.
    let mut kids: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut outcome: std::collections::BTreeMap<String, String> = Default::default();
    // `top`'s OWN outcome lives in ITS parent's ledger, which the walk down
    // never reads — seed it, or a failed thread's own row (the per-key
    // hydrate, where `top` is the key) would read `idle` (R9.TS1 inspect F1).
    if let Some(parent) = mg.parent_of(top) {
        if let Some(l) = mg.load(&parent).map_err(|e| crate::error::Error::Internal(format!("{e}")))? {
            for run in children_row(&l).runs.iter().filter(|r| r.id == top) {
                match &run.finished {
                    Some(f) => outcome.insert(run.id.clone(), f.outcome.clone()),
                    None => outcome.remove(&run.id),
                };
            }
        }
    }
    let mut stack = vec![top.to_string()];
    let mut seen: std::collections::BTreeSet<String> = [top.to_string()].into_iter().collect();
    while let Some(id) = stack.pop() {
        let row = match mg.load(&id).map_err(|e| crate::error::Error::Internal(format!("{e}")))? {
            Some(l) => children_row(&l),
            None => ChildrenRow::default(),
        };
        let mut mine = Vec::new();
        for run in &row.runs {
            // The LAST run of a child decides: an unfinished one clears an
            // older outcome (it is running, or owed).
            match &run.finished {
                Some(f) => outcome.insert(run.id.clone(), f.outcome.clone()),
                None => outcome.remove(&run.id),
            };
            if !mine.contains(&run.id) {
                mine.push(run.id.clone());
            }
            if seen.insert(run.id.clone()) {
                stack.push(run.id.clone());
            }
        }
        kids.insert(id, mine);
    }
    let all: Vec<String> = kids.keys().cloned().collect();
    let states = m.thread_states(&all)?;
    let rolled = shapewire::rolled::rolled_subtree(
        top,
        |id| kids.get(id).cloned().unwrap_or_default(),
        |id| {
            let (state, executing) = match states.get(id) {
                Some(d) => (d.state, d.executing),
                None => ("idle", false),
            };
            // THE FAILURE STATES THE PRODUCER ALREADY HAS: a thread the
            // derivation calls idle whose last managed run reported `failed`
            // or `interrupted` is in that error state (`cancelled` and `ok`
            // are not errors). Anything else keeps the derivation's word.
            let word = match (state, outcome.get(id).map(String::as_str)) {
                ("idle", Some(o @ ("failed" | "interrupted"))) => o,
                _ => state,
            };
            (word.to_string(), executing)
        },
    );
    Ok(ids
        .iter()
        .filter_map(|id| rolled.get(id).map(|r| (id.clone(), RolledRow::from(r))))
        .collect())
}

/// **ONE UNDELIVERED QUEUE ROW — the thing no shape carried.**
///
/// The two RPCs this replaces, `threads/{id}/queue` and
/// `threads/{id}/queue_failed`, projected exactly these four fields out of
/// `store::QueueRow` and differed in ONE WAY ONLY: a `WHERE state='waiting'`
/// against a `WHERE state='failed'`. `QueueRow` already carries `state` as a
/// column, so the discriminator the two calls encoded in their NAMES is a field
/// — and `docs/consumer-seam.md` §4 recorded them as the pair that had to stay
/// RPC because *"no shape carries queue contents"*. That is what this is.
///
/// **One cell, not two.** Two spellings for one act is the collapse the server's
/// own re-select comment argues against, and a consumer rendering either list
/// wants the same four fields in the same order; it filters on `state`. A
/// `queue_failed` cell beside a `queue` cell would also make "is this row
/// waiting or failed" answerable two ways, which is how a mirror learns to
/// disagree with itself.
///
/// **It carries no `metadata` and no `headers`**, which `QueueRow` does hold.
/// Neither RPC exposed them and neither should: `metadata` is the caller's
/// opaque per-message blob whose single declared path to the model lives in
/// `crate::context` (DEC-55 category (a)), and `headers` is post-validation
/// render state. A cell is a declaration that a consumer MAY read what is in it,
/// so putting them here would publish both.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct QueueEntry {
    /// The allocated addressable position — the same id `threads.queue_cancel`
    /// and `threads.queue_reselect` take, which is why a consumer can act on
    /// this list without a second call.
    pub message_id: u64,
    pub text: String,
    pub intent: String,
    pub created_at: String,
    /// `waiting` | `failed`, straight off the stored column. The CLOSED
    /// vocabulary is the point: `delivered`, `cancelled` and any future word are
    /// not in this cell at all, because a row in one of those states is not
    /// undelivered and is not what either RPC answered.
    pub state: String,
}

/// The two queue states this cell carries, and the only two.
///
/// Declared as a const so the manifest's `Enum` and the producer's filter cannot
/// drift — `crate::runtime::ThreadManager::thread_queue` reads rows for exactly
/// these words.
pub const QUEUE_STATE_WORDS: &[&str] = &["waiting", "failed"];

// ─────────────────────────────────────────────────────────────────────────────
// The row and the root
// ─────────────────────────────────────────────────────────────────────────────

/// Thirteen cells: `Register` × 10, `Edge` × 1 (its first instance anywhere),
/// `Map` × 1 (the opaque app payloads), `Sequence` only inside the `Edge`.
/// `Semilattice` and `Counter` have no instance and the module doc says why.
#[derive(Debug)]
pub struct ThreadCells {
    pub header: Register<HeaderRow>,
    /// **THE RAW STORED BLOB, REPARSED AND NEVER ROUND-TRIPPED.**
    ///
    /// This cell is one keystroke from an infinite patch loop in AUI, and the
    /// mechanism belongs where the cell is. `thread_json` emits
    /// `serde_json::from_str::<Value>(&r.settings_json)` — the stored string
    /// reparsed, never passed through `ThreadSettings`. AUI hashes its desired
    /// blob, and on a mismatch it `threads.get`s, diffs six fields as literal
    /// `Value` comparisons, and patches. A daemon that normalised, reordered,
    /// defaulted-in or re-typed any of those fields on read-back would put AUI
    /// into the same `threads.update` on every single turn.
    ///
    /// So both paths into this cell go through [`settings_of`], which is that
    /// one expression and nothing else: `Fact::ThreadSettings` carries what it
    /// returns and `ThreadRowRead` answers with what it returns.
    /// `tests/thread_shapes.rs::the_settings_cell_is_byte_identical_to_threads_get`
    /// asserts the export against the live `threads.get` reply rather than
    /// against this file's own idea of the blob.
    ///
    /// **`Push::Event(SettingsChanged)` is NOT folded into it**, and that is the
    /// other half of the same trap: that event carries a `patch`, not the blob
    /// (`event.rs`'s `SettingsChanged { patch: Value }`). A `Register<Value>` fed
    /// a patch would hold a patch, and the live path and the read path would not
    /// agree about what this cell even is.
    pub settings: Register<Value>,
    /// **THE PLACEMENT, AS A LEVEL AND NOT AN EDGE.** Its transition log already
    /// exists in two places — the `AccountSwitched` ledger event and `accounts`'
    /// `moves: Sequence` — and two shapes must not both own one transition log.
    ///
    /// `Option`, and `None` means the daemon does not know, exactly as
    /// `Push::Status.account` already reasons. A `Status` whose `account` is
    /// absent therefore does NOT write this cell: "we do not know" must not
    /// overwrite a placement we do know.
    pub account: Register<Option<String>>,
    pub status: Edge<StateLevel>,
    /// Waiting queue rows. A live count of rows the producer holds, not an
    /// unbounded total, so `docs/shapes.md` §4.2 step 6 does not apply and
    /// `Counter` would be wrong: `waiting_queue_counts` hydrates it in one
    /// covered read.
    ///
    /// **IT STAYS, ALONGSIDE [`Self::queue`], AND THE REDUNDANCY IS THE CHEAPER
    /// OF THE TWO ARRANGEMENTS.** `queue.iter().filter(waiting).count()` would
    /// derive this number, so carrying both looks like duplication — but the two
    /// are fed by reads of very different cost, and collapsing them would move
    /// the expensive one onto the fleet:
    ///
    /// * this cell hydrates from `Store::waiting_queue_counts`, ONE statement
    ///   answered entirely from `idx_queue_thread_state` without touching a row,
    ///   for the WHOLE fleet — which is what `accounts` needs, per account, on
    ///   every board export;
    /// * [`Self::queue`] hydrates from `waiting_queue` + `failed_deliveries`,
    ///   which read ROWS (including every message's text) for ONE thread.
    ///
    /// `threads` is [`Hydrate::PerKey`], so the row read happens only for a
    /// thread somebody is actually looking at. Deriving the count from the
    /// contents instead would make the fleet-wide number cost a text-bearing row
    /// scan per thread. *Minimal redundancy is good; no dependency is not
    /// necessarily better* — and here the dependency is the cheap direction.
    pub queued: Register<u64>,
    /// **THE UNDELIVERED QUEUE'S CONTENTS** — see [`QueueEntry`] for what one row
    /// is and why `waiting` and `failed` share one cell.
    ///
    /// A `Register<Vec<_>>` and NOT a `Sequence`: this is a LEVEL, not a log.
    /// The queue is a set of rows that exist right now, and a row leaves it by
    /// being delivered, cancelled or re-selected — none of which is an append.
    /// A `Sequence` would retain rows that are no longer queued and make
    /// "what is waiting" unanswerable from the cell that claims to hold it.
    ///
    /// **Its bound is the queue's own depth and is not a declared ceiling.** A
    /// thread's undelivered rows are what a human parked plus what one failed
    /// delivery ladder left behind; there is no retention window to declare and
    /// `MAX_RETAINED_ITEMS` would be describing something this cell does not do.
    /// Unlike `accounts.threads` — which rode every export of every account key
    /// and therefore needed `MAX_NON_IDLE_HYDRATE_ROWS` — this rides the export
    /// of the ONE thread a consumer asked for.
    pub queue: Register<Vec<QueueEntry>>,
    /// The highest ADDRESSABLE position in the log — the unit a human can be
    /// read back. `None` means nothing addressable has ever been written.
    pub last_message_id: Register<Option<u64>>,
    /// The durable seen cursor, straight off `thread_seen.last_seen_seq`.
    pub seen_seq: Register<u64>,
    /// **THE EXACT COUNT OF UNREAD ADDRESSABLE EVENTS, MAINTAINED BY THE FOLD.**
    ///
    /// The operator asked for the argument for carrying this at all, and it is
    /// that the obvious derivation is WRONG: `newest.seq - seen_seq`
    /// over-counts, because `addressable` is a property of an appended event
    /// (`append_custom`'s `addressable` flag is what allocates a message id) and
    /// only 47,556 of the store's 2,463,981 events have one. `thread_seen`
    /// stores ONLY `last_seen_seq` — a position, with no message id — so the
    /// exact answer is not a difference of the two positions the daemon keeps.
    ///
    /// It is not a `Counter` either: a `Counter` cannot hydrate and is
    /// `since_init` by construction, and this number must be right on the first
    /// export.
    ///
    /// So it is a `Register` with three writers and no query in the fold:
    ///
    /// 1. the hydrate reads `unread_count(id, seen)` — the same statement
    ///    `threads.status` already answers with;
    /// 2. the fold INCREMENTS it when an event arrives that is addressable and
    ///    sits above the held seen position — the fold sees every event and can
    ///    test `message_id.is_some()` itself;
    /// 3. `Fact::ThreadSeen` RESETS it, carrying the count recomputed at the
    ///    `mark_seen` write site, because when the cursor jumps the fold cannot
    ///    know how many addressable events the jump swallowed.
    ///
    /// Both positions are carried too, so a consumer can see for itself that the
    /// difference is not this number.
    ///
    /// **WHERE IT IS NOT EXACT, stated because it is a real edge and not a
    /// rounding.** The increment is skipped for an event whose `seq` is not past
    /// the held head, because the cell cannot tell a DUPLICATE `seq` from an
    /// out-of-order arrival of a new one — and over-counting a replay is the worse
    /// of the two errors, since a count that drifts up never comes back down on
    /// its own. So a genuinely out-of-order addressable event is not counted, and
    /// the correction is the next `Fact::ThreadSeen` (which carries a real read) or
    /// a re-init. It takes two emits on ONE thread interleaving to happen at all:
    /// the FIFO preserves arrival order, and `seq` is allocated by a `fetch_add`
    /// before the append.
    ///
    /// **`Push::Status.unread` is deliberately NOT folded into it** — the one
    /// field of `Status` this shape ignores. `emit_status` takes its stamp BEFORE
    /// its five reads (deliberately, so a level cannot be set from pre-hydrate
    /// content), which means the `unread` it carries was read AFTER its own
    /// stamp; `Register::emit` is last-arrival-wins, so accepting it would let a
    /// status frame overwrite a fresher increment with an older count.
    pub unread: Register<u64>,
    pub newest: Register<NewestRow>,
    /// **THE NEWEST MESSAGE'S BODY, AS A RESOURCE — the first resource on any
    /// producer in this workspace.**
    ///
    /// `docs/chassis-resources.md` 1.2: *a shape says what exists, a resource
    /// fetch says what it is.* [`NewestRow`] says WHERE the log is; this says
    /// what the thing at the head IS, by id and by size, and the CONTENT is
    /// fetched by [`crate::shapes::body`]'s door. The division is
    /// `docs/thread-shapes-plan.md`'s and the module doc of `body.rs` records
    /// the re-verification against today's `fold` and `Store::list_events`.
    ///
    /// **A SUMMARY, never an enumeration.** One id. A thread with 5,000
    /// messages names one here and names none anywhere else — the older bodies
    /// are reached through `threads/{id}/history`, whose rows carry `seq`, and
    /// `seq` is what the PRODUCER mints an id from. A consumer cannot spell one.
    ///
    /// ## Why a cell of its own and not a field on `newest`
    ///
    /// Because its STATE moves independently, which is the test
    /// `docs/shapes.md` 4.2 sets for a cell. `newest` is `Known` from one index
    /// seek that reads `seq`, `message_id`, `kind` and `ts`; the body is read
    /// out of the same row's `payload_json`, and a payload the shape cannot
    /// parse leaves `newest` perfectly known and this `NotHydrated` with the
    /// reason. One cell carrying both would have to report the whole head as
    /// unknown because one column would not parse.
    ///
    /// `Known(None)` is the honest value for an event kind that HAS no body (a
    /// turn boundary, a tool result): a complete answer about the world, which
    /// renders as empty. `NotHydrated` is *the store holds a row I could not
    /// read*. Those are the two statements the three-valued unknown exists to
    /// keep apart, and collapsing them would make an unreadable payload look
    /// like a message with nothing in it.
    pub body: Register<Option<shapewire::wire::ResourceSummary>>,
    pub compaction: Register<CompactionRow>,
    pub liveness: Register<LivenessRow>,
    /// **THE APPLICATION'S OWN EVENTS, HELD OPAQUELY — newest per
    /// `event_type`.**
    ///
    /// Structurally identical to `accounts`' `gauge: Map<axis, Register>`, and
    /// chosen by the same two steps of `docs/shapes.md` §4.2: a set of things
    /// addressed by name, each holding a current value.
    ///
    /// **Nothing in this crate reads the `Value`.** The vocabulary is the app's
    /// (7 types in the production store: `summary_updated`,
    /// `assistant_suggestions`, `transcript_ported`, `failed_input`,
    /// `thread_compacted`, `transcript_reconciled`,
    /// `thread_compaction_started`), the producer cannot classify it, and the
    /// moment this shape branched on an `event_type` it would be encoding one
    /// application's meaning into the general shape. The interpretation lives in
    /// [`crate::shapes::summary`].
    ///
    /// `docs/chassis-resources.md` 3.4 (D-app) notes the latent cost: a large
    /// custom payload multiplies every `row` frame for that thread. Its
    /// recommended fix is an `inline: bool` on `threads.event`, which is a
    /// change to an existing endpoint and therefore out of scope here. The cost
    /// is bounded by what the app writes, and it is stated rather than hidden.
    pub app: Map<String, Register<Value>>,
    /// **THE THREAD'S PARENT — `null` FOR A ROOT** (R4.S12, DEC-32).
    ///
    /// Read off the ONE owner of lineage, `ThreadMeta.parent_id` in the
    /// thread's managed ledger — the same fact `agents/list` answers its
    /// `parent_id` from — so a face can draw a subagent under its parent
    /// without a second call. A thread with no managed ledger took part in no
    /// spawn as a child and is a root: `Known(None)`, never `?`.
    ///
    /// **HYDRATE-ONLY, AND THAT IS COMPLETE.** Lineage is written once, in the
    /// child's ledger meta, BEFORE the child's `threads` row exists
    /// (`crate::managed`'s spawn order: ledger, commit point, row), and it is
    /// never rewritten. `hydrates_key` answers `false` for an id with no row,
    /// so no key can be hydrated before its parent is on disk, and no fold arm
    /// needs to feed this cell.
    ///
    /// LAST in the row, so every cell before it keeps its position; additive,
    /// so `Threads::VERSION` does not move (rule 2.6).
    pub parent_id: Register<Option<String>>,
    /// **THE THREAD'S MANAGED CHILDREN** — see [`ChildrenRow`]. Hydrated off
    /// the thread's managed ledger (no ledger is a COMPLETE empty value, never
    /// `?`), folded from [`Fact::ThreadChildren`], which `crate::managed` emits
    /// after every spawn commit and every report. ADDITIVE and LAST, so
    /// `Threads::VERSION` does not move.
    pub children: Register<ChildrenRow>,
    /// **THE STATUS ROLLED UP OVER THE WHOLE SUBTREE** — see [`RolledRow`].
    /// Hydrated by [`rolled_rows`], folded from [`Fact::ThreadRolled`], which
    /// `ThreadManager::emit_rolled` emits for a thread and every ancestor on
    /// each status edge and each children move. ADDITIVE and LAST, so
    /// `Threads::VERSION` does not move.
    pub rolled: Register<RolledRow>,
    /// **THE THREAD'S IN-PROCESS (Task / Agent tool) SUBAGENTS, KEYED BY THE
    /// LAUNCHING `tool_use_id`** (R9.TS2, TC-10) — see [`SubagentRow`].
    /// Hydrated out of MEMORY (the thread's [`TaskRoster`], beside the
    /// `outstanding` map `liveness.running_subagents` reads), folded from
    /// [`Fact::ThreadSubagents`], which `ThreadManager` emits whenever a Task
    /// subagent starts, ends or errors. A `Register` of the whole map: it is a
    /// LEVEL, bounded by [`MAX_TASK_ROWS`]. ADDITIVE and LAST, so
    /// `Threads::VERSION` does not move.
    pub subagents: Register<std::collections::BTreeMap<String, SubagentRow>>,
}

impl Default for ThreadCells {
    fn default() -> Self {
        Self {
            header: Register::new(),
            settings: Register::new(),
            account: Register::new(),
            // 32 transitions within 24 h — `accounts.moves`' bounds, for the same
            // reason: a history nobody can count past is worse than one that says
            // what it covers, and `covers_since` rides the export.
            status: Edge::new(32, chrono::Duration::hours(24)),
            queued: Register::new(),
            queue: Register::new(),
            last_message_id: Register::new(),
            seen_seq: Register::new(),
            unread: Register::new(),
            newest: Register::new(),
            body: Register::new(),
            compaction: Register::new(),
            liveness: Register::new(),
            app: Map::new(),
            parent_id: Register::new(),
            children: Register::new(),
            rolled: Register::new(),
            subagents: Register::new(),
        }
    }
}

impl ThreadCells {
    fn to_json(&self, key: &str) -> Value {
        json!({
            "thread_id": key,
            "header": self.header.now(),
            "settings": self.settings.now(),
            "account": self.account.now().cloned().flatten(),
            "status": self.status.now(),
            "status_transitions": self.status.transitions().to_json(),
            // **`null`, NEVER A FABRICATED `0`, FOR A CELL THAT IS NOT KNOWN.**
            // These three read `.unwrap_or(0)` until 2026-10-08, so the row
            // carried a number under a slot whose `cells` block said
            // `not_hydrated`; a consumer that read the row without consulting
            // the block read a seen position of zero that nobody had stated.
            // The `cells` block is the truth and the row must not contradict
            // it. (`header`, `newest` and the rest already emitted `null`.)
            "queued": self.queued.now(),
            "queue": self.queue.now(),
            "last_message_id": self.last_message_id.now().cloned().flatten(),
            "seen_seq": self.seen_seq.now(),
            "unread": self.unread.now(),
            "newest": self.newest.now(),
            "body": self.body.now().cloned().flatten(),
            "compaction": self.compaction.now(),
            // **DERIVED HERE, HELD NOWHERE.** See [`context_id_of`]: the value
            // is a function of the key and the compaction marker, so it is
            // computed off the one register at export and cannot drift from it.
            "context_id": self.compaction.now().map(|c| context_id_of(key, c)),
            // **THE MESSAGE PAGES, DERIVED HERE, HELD NOWHERE** — the resource
            // ids a historical range is read through (`crate::shapes::pages`).
            // A function of the key and `last_message_id`, so `null` exactly
            // when that cell is unknown and `[]` for a thread with no message.
            "pages": self.last_message_id.now().map(|last| match last {
                Some(l) => Value::Array(crate::shapes::pages::pages_of(key, *l)),
                None => Value::Array(Vec::new()),
            }),
            "liveness": self.liveness.now(),
            "app": Value::Object(
                self.app
                    .iter()
                    .map(|(t, c)| (t.clone(), c.now().cloned().unwrap_or(Value::Null)))
                    .collect(),
            ),
            "parent_id": self.parent_id.now().cloned().flatten(),
            "children": self.children.now(),
            "rolled": self.rolled.now(),
            "subagents": self.subagents.now(),
        })
    }

    fn cells_json(&self) -> Value {
        json!({
            "header": self.header.state(),
            "settings": self.settings.state(),
            "account": self.account.state(),
            "status": self.status.state(),
            "status_transitions": self.status.transitions().state(),
            "queued": self.queued.state(),
            "queue": self.queue.state(),
            "last_message_id": self.last_message_id.state(),
            "seen_seq": self.seen_seq.state(),
            "unread": self.unread.state(),
            "newest": self.newest.state(),
            "body": self.body.state(),
            "compaction": self.compaction.state(),
            // The context id's provenance IS the compaction marker's: one
            // register behind both, so one state.
            "context_id": self.compaction.state(),
            // One register behind `pages` and `last_message_id`, so one state.
            "pages": self.last_message_id.state(),
            "liveness": self.liveness.state(),
            "app": self.app.state(),
            "parent_id": self.parent_id.state(),
            "children": self.children.state(),
            "rolled": self.rolled.state(),
            "subagents": self.subagents.state(),
        })
    }

    fn mark_dead(&mut self, reason: &str) {
        self.header.mark_dead(reason);
        self.settings.mark_dead(reason);
        self.account.mark_dead(reason);
        self.status.mark_dead(reason);
        self.queued.mark_dead(reason);
        self.queue.mark_dead(reason);
        self.last_message_id.mark_dead(reason);
        self.seen_seq.mark_dead(reason);
        self.unread.mark_dead(reason);
        self.newest.mark_dead(reason);
        self.body.mark_dead(reason);
        self.compaction.mark_dead(reason);
        self.liveness.mark_dead(reason);
        self.app.mark_dead(reason);
        for (_, c) in self.app.entries_mut() {
            c.mark_dead(reason);
        }
        self.parent_id.mark_dead(reason);
        self.children.mark_dead(reason);
        self.rolled.mark_dead(reason);
        self.subagents.mark_dead(reason);
    }
}

/// The held state: the keyed `Map` plus the shape-level cells.
pub struct ThreadsRoot {
    pub rows: Map<String, ThreadCells>,
    /// **HOW MANY THREADS EXIST AT ALL**, so a consumer holding three keys knows
    /// whether that is the fleet or a window onto it. One `SELECT COUNT(*)`,
    /// once per epoch.
    pub fleet_size: Register<u64>,
    /// **WHAT THE KEY SET *IS*, SAID STRUCTURALLY.**
    ///
    /// Under `OnDemand` the keys are neither the fleet nor the live subset, and a
    /// consumer must not have to infer that from an absent value. This is
    /// `machine_login.unhosted`'s lesson applied: the discriminator is a typed
    /// field, never a reason string and never "the map was empty".
    pub key_scope: Register<Value>,
    /// The daemon's own constants, so no consumer hardcodes them — exactly
    /// `warn_below_days`' pattern on the `accounts` shape.
    pub limits: Register<Value>,
    /// This shape's declared version. See the module doc for what declaring it
    /// does and does not buy.
    pub shape_version: Register<u32>,
    shared_dirty: bool,
}

impl Default for ThreadsRoot {
    fn default() -> Self {
        Self {
            rows: Map::new(),
            fleet_size: Register::new(),
            key_scope: Register::new(),
            limits: Register::new(),
            shape_version: Register::new(),
            shared_dirty: false,
        }
    }
}

impl ThreadsRoot {
    /// Idempotent like `accounts`' siblings: a re-read that finds the same value
    /// must not mark `shared` dirty, or every hydrate would emit a `shared`
    /// event that changed nothing.
    fn set_fleet_size(&mut self, at: Timestamp, n: u64) {
        if self.fleet_size.now() == Some(&n) {
            return;
        }
        self.fleet_size.emit(at, n);
        self.shared_dirty = true;
    }

    fn set_shared_constants(&mut self, at: Timestamp) {
        // **THIS BLOCK IS A HAND-ROLLED COPY OF A CHASSIS FACT** and
        // `docs/access-patterns.md` 7.4 has it slated for deletion into the
        // `contract` block on `shapes.list`. It carries no CEILING for exactly
        // that reason: `ShapeInfo.max` is where a ceiling is published, and two
        // homes for one number is the drift rule 2.6 exists to refuse.
        let scope = json!({
            "mode": <Threads as Shape<Input>>::HYDRATE.as_str(),
            // A per-key key is hydrated because somebody named it, whatever
            // state the thread is in — which is why this shape never claimed to
            // hold "the active set".
            "includes_idle": true,
            // **TWO ASKS, TWO ANSWERS, AND IT USED TO BE ONE WORD.** This said
            // `"refused"` flat, which stopped being true the moment `detail:
            // keys` was served: a keyless subscribe is refused for ROWS and
            // ANSWERED for membership, and a consumer reading one word would
            // have concluded the pool cannot be enumerated at all — which is
            // the opposite of what this change did.
            "keyless_rows": "refused",
            "keyless_keys": "membership",
        });
        let limits = json!({
            "query_default_limit": crate::server::QUERY_DEFAULT_LIMIT,
            "query_max_limit": crate::server::QUERY_MAX_LIMIT,
            "message_kinds": crate::server::MESSAGE_KINDS,
        });
        if self.key_scope.now() != Some(&scope) {
            self.key_scope.emit(at, scope);
            self.shared_dirty = true;
        }
        if self.limits.now() != Some(&limits) {
            self.limits.emit(at, limits);
            self.shared_dirty = true;
        }
        if self.shape_version.now() != Some(&Threads::VERSION) {
            self.shape_version.emit(at, Threads::VERSION);
            self.shared_dirty = true;
        }
    }
}

impl Root for ThreadsRoot {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }

    /// **WHAT EXISTS, WHICH ON THIS SHAPE IS NOT WHAT IS HELD.** The default
    /// `Root::members` answers the held keys, which is right for a root that
    /// reads its whole key set at init; here the two come apart by design, so
    /// this reads the `Map`'s membership set and its own state word.
    fn members(&self) -> Membership {
        match self.rows.members_state() {
            crate::shape::CellState::NotHydrated { reason } => Membership::NotHydrated {
                reason: reason
                    .clone()
                    .unwrap_or_else(|| "the pool's membership has not been read".to_string()),
            },
            crate::shape::CellState::Dead { reason } => Membership::NotHydrated {
                reason: reason.clone(),
            },
            crate::shape::CellState::Known { complete, .. } => Membership::Known {
                keys: self.rows.members().cloned().collect(),
                complete: *complete,
            },
        }
    }

    fn row(&self, key: &str) -> Option<Value> {
        self.rows.get(&key.to_string()).map(|r| r.to_json(key))
    }

    fn cells(&self, key: &str) -> Option<Value> {
        self.rows.get(&key.to_string()).map(|r| r.cells_json())
    }

    fn shared(&self) -> Value {
        json!({
            "fleet_size": self.fleet_size.now(),
            "key_scope": self.key_scope.now(),
            "limits": self.limits.now(),
            "shape_version": self.shape_version.now(),
        })
    }

    fn shared_cells(&self) -> Value {
        json!({
            "fleet_size": self.fleet_size.state(),
            "key_scope": self.key_scope.state(),
            "limits": self.limits.state(),
            "shape_version": self.shape_version.state(),
        })
    }

    fn drain(&mut self) -> crate::shape::Outbox {
        let dirty = self.rows.take_dirty_keys();
        let gone = self.rows.take_removed();
        // **THE ARRIVALS, AND THIS IS THE ONE THAT MATTERS.** This fold mints
        // no row — every arm asks `holds` first — so `added` here is almost
        // always a key that became a MEMBER without becoming a row: a thread
        // this daemon saw an event about, recorded the existence of, and did
        // not hydrate. That is what lets a `detail: keys` subscriber learn about
        // a thread created after it subscribed while the scope guard stays
        // exactly as strict as it was.
        let added = self.rows.take_added();
        let mut transitions = Vec::new();
        let mut rows = Vec::new();
        for k in dirty {
            let Some(r) = self.rows.get_mut(&k) else {
                continue;
            };
            for item in r.status.transitions_mut().take_pending() {
                transitions.push((k.clone(), "status", item));
            }
            rows.push(k);
        }
        crate::shape::Outbox {
            rows,
            gone,
            added,
            transitions,
            shared: std::mem::take(&mut self.shared_dirty),
        }
    }

    fn mark_dead(&mut self, reason: &str) {
        self.fleet_size.mark_dead(reason);
        self.key_scope.mark_dead(reason);
        self.limits.mark_dead(reason);
        self.shape_version.mark_dead(reason);
        self.rows.mark_dead(reason);
        for (_, r) in self.rows.entries_mut() {
            r.mark_dead(reason);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The ONE builders — shared by the live fact and the hydrate read
// ─────────────────────────────────────────────────────────────────────────────

/// **THE SETTINGS BLOB, AS `threads.get` PRODUCES IT.** One expression, two
/// paths — see [`ThreadCells::settings`] for why this function existing is the
/// whole defence against an AUI patch loop.
pub fn settings_of(row: &crate::store::ThreadRow) -> Value {
    serde_json::from_str::<Value>(&row.settings_json).unwrap_or(Value::Null)
}

/// The header, from the durable row.
pub fn header_of(row: &crate::store::ThreadRow) -> HeaderRow {
    HeaderRow {
        cwd: row.cwd.clone(),
        model: row.model.clone(),
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}

/// The compaction marker's two durable fields. `range` is filled by the caller
/// that knows it — the emit site has it in hand, and the hydrate reads it.
pub fn compaction_of(row: &crate::store::ThreadRow, range: Option<[u64; 2]>) -> CompactionRow {
    CompactionRow {
        last_compaction_message_id: row.last_compaction_message_id,
        needs_restate: row.needs_restate,
        range,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The hydrate requests — six per key, one shape-level, each ONE read
// ─────────────────────────────────────────────────────────────────────────────

macro_rules! keyed_request {
    ($name:ident, $wire:literal, $resp:ty) => {
        /// A per-key hydrate request. `thread_id` is filled by
        /// `Threads::hydrates_key`; `Default`'s empty id is the chassis's bound
        /// and is never the value that gets used.
        #[derive(Default)]
        pub struct $name {
            pub thread_id: String,
        }
        impl HydrateRequest for $name {
            const NAME: &'static str = $wire;
            type Response = $resp;
        }
    };
}

/// Everything one `get_thread` answers.
#[derive(Debug, Clone)]
pub struct ThreadRowSnapshot {
    pub header: HeaderRow,
    pub settings: Value,
    pub account: Option<String>,
    pub compaction: CompactionRow,
}

/// What `thread_states` plus one `unread_count` answer.
#[derive(Debug, Clone)]
pub struct DerivedSnapshot {
    pub level: StateLevel,
    pub queued: u64,
    pub seen_seq: u64,
    pub unread: u64,
}

/// One application event type's newest payload — held OPAQUELY.
#[derive(Debug, Clone)]
pub struct AppEntry {
    pub event_type: String,
    pub data: Value,
}

keyed_request!(ThreadRowRead, "thread_row", ThreadRowSnapshot);
keyed_request!(ThreadDerivedState, "thread_derived_state", DerivedSnapshot);
keyed_request!(ThreadNewestEvent, "thread_newest_event", NewestSnapshot);

/// **ONE ROW OF `events`, FEEDING TWO CELLS.**
///
/// One hydrate request, one statement, two cells — [`ThreadCells::newest`] and
/// [`ThreadCells::body`] — because both are facts about the same row and
/// splitting the REQUEST would split the read. `body` is `Result` rather than
/// `Option` so the two outcomes a payload can have stay apart all the way to
/// the cell: `Ok(None)` is a kind with no body (complete, renders empty),
/// `Err(reason)` is a row this shape could not read (`NotHydrated`, renders
/// `?`).
#[derive(Debug, Clone)]
pub struct NewestSnapshot {
    pub newest: NewestRow,
    pub body: Result<Option<shapewire::wire::ResourceSummary>, String>,
}
keyed_request!(ThreadMessageHead, "thread_message_head", u64);
keyed_request!(ThreadAppState, "thread_app_state", AppEntry);
keyed_request!(ThreadLiveness, "thread_liveness", LivenessRow);
// The thread's parent, off its managed ledger's meta — `None` for a root.
keyed_request!(ThreadParent, "thread_parent", Option<String>);
// The thread's managed children, off its own managed ledger — empty for none.
keyed_request!(ThreadChildren, "thread_children", ChildrenRow);
// The thread's status rolled up over its whole subtree (R9.TS1) — `rolled_rows`.
keyed_request!(ThreadRolledRead, "thread_rolled", RolledRow);
// The thread's in-process Task subagents (R9.TS2) — out of memory, one mutex.
keyed_request!(
    ThreadSubagentsRead,
    "thread_subagents",
    std::collections::BTreeMap<String, SubagentRow>
);
// The undelivered queue for one thread — `waiting` then `failed`, each ordered
// by insertion. TWO statements, both on `idx_queue_thread_state`, and they are
// two because the two states are two `WHERE`s on one index, not because the cell
// is two things. (A `///` here is an unused doc comment: `keyed_request!` writes
// the struct's own docs.)
keyed_request!(ThreadQueueRows, "thread_queue_rows", QueueEntry);

/// **THE POOL'S MEMBERSHIP** — every thread id, shape-level, read once per
/// epoch and never per key. See [`crate::store::Store::thread_ids`].
#[derive(Default)]
pub struct ThreadIds;
impl HydrateRequest for ThreadIds {
    const NAME: &'static str = "thread_ids";
    type Response = String;
}

/// **HOW MANY IDS ONE MEMBERSHIP READ WILL TAKE.**
///
/// A producer ceiling, declared rather than left to `SELECT id FROM threads`
/// being small in practice. 10,000 against a measured fleet of 5,417 is ~400 KB
/// of ids and cannot bite today; the point is that when it does, the read
/// answers `Partial`, the membership export says `complete: false`, and nobody
/// gets a short roster that claims to be the pool. The remedy for a consumer
/// that sees it — `range: [lo, hi)` paging over membership — is the next
/// dispatch's, and stating the bound is what makes it askable for.
pub const MAX_MEMBER_IDS: usize = 10_000;

/// The fleet count — shape-level, read ONCE per epoch and never per key.
#[derive(Default)]
pub struct FleetSize;
impl HydrateRequest for FleetSize {
    const NAME: &'static str = "thread_fleet_size";
    type Response = u64;
}

// ─────────────────────────────────────────────────────────────────────────────
// The shape
// ─────────────────────────────────────────────────────────────────────────────

pub struct Threads;

impl Threads {
    /// **THIS SHAPE'S VERSION.** See the module doc: the number is declared and
    /// published; the refusal is not built.
    ///
    /// **STILL `1` AFTER `context_id` (R2.S3d), deliberately.** Rule 2.6 moves the
    /// number on REMOVAL or SEMANTIC CHANGE, never on an addition: the cell is
    /// new, no consumer built against version 1 reads it, every cell version 1
    /// declared is still here meaning what it meant, and the consumer chassis
    /// ignores a cell it does not know (`shapewire::mirror`). A bump would refuse
    /// every v1 link — the AUI, the TUI, the harness — over a cell none of them
    /// can be missing.
    pub const VERSION: u32 = 1;
}

// ─────────────────────────────────────────────────────────────────────────────
// THE CELL MANIFEST — what this shape carries, declared (`shapewire::cell`)
// ─────────────────────────────────────────────────────────────────────────────
//
// One entry per key of `ThreadCells::cells_json`, with what the row slot holds.
// `tests/cell_manifest.rs` asserts the two key sets are equal, so the manifest
// cannot drift from the row builder; what it says about each VALUE is read off
// the row types above. Two declarations carry a closed vocabulary — `status.state`
// and `newest.kind` — and those are the two words a consumer may compare against.
// `settings` and `app` are `Opaque` by ruling: nothing in this crate reads
// inside either, and a consumer may not either.
declare_cells! {
    /// Per-key cells of `threads`.
    pub THREADS_CELLS = [
        ("header", Struct("HeaderRow"), Required, [
            ("cwd", Path, Optional),
            ("model", Sc(Str), Optional),
            ("created_at", Ts(Rfc3339), Required),
            ("updated_at", Ts(Rfc3339), Required),
        ]),
        // The raw stored blob, reparsed and never round-tripped: a consumer
        // compares it whole and reads nothing inside it.
        ("settings", Opaque, Required, []),
        // `None` is the daemon saying it has not placed this thread.
        ("account", Ref("account"), Optional, []),
        ("status", Struct("StateLevel"), Required, [
            ("state", Enum(crate::runtime::STATE_WORDS), Required),
            ("executing", Sc(Bool), Required),
            ("detail", Sc(Str), Optional),
        ]),
        // The `Edge`'s log: `from` is absent on the first transition the shape
        // saw, `at` is the chassis's stamp on every retained item.
        ("status_transitions", List(&Struct("Transition")), Required, [
            ("from", Struct("StateLevel"), Optional),
            ("to", Struct("StateLevel"), Required),
            ("at", Ts(Rfc3339), Required),
        ]),
        ("queued", Sc(Int), Required, []),
        // The undelivered rows themselves. `state` is a CLOSED vocabulary and is
        // the field that replaces the two RPCs' difference — the pair differed
        // only in a `WHERE state=` and the names encoded the discriminator.
        // `metadata` and `headers` are on the stored row and deliberately NOT
        // declared here: a cell is permission to read what is in it.
        ("queue", List(&Struct("QueueEntry")), Required, [
            ("message_id", Cursor("message"), Required),
            ("text", Sc(Str), Required),
            ("intent", Sc(Str), Required),
            ("created_at", Ts(Rfc3339), Required),
            ("state", Enum(QUEUE_STATE_WORDS), Required),
        ]),
        ("last_message_id", Cursor("message"), Optional, []),
        ("seen_seq", Cursor("event"), Required, []),
        ("unread", Sc(Int), Required, []),
        ("newest", Struct("NewestRow"), Required, [
            ("seq", Cursor("event"), Required),
            ("message_id", Cursor("message"), Optional),
            ("kind", Enum(crate::event::KINDS), Required),
            ("ts", Ts(Rfc3339), Required),
        ]),
        // `Known(None)` is an event kind with no body.
        ("body", Struct("ResourceSummary"), Optional, [
            ("id", Ref("resource"), Required),
            ("bytes", Sc(Int), Required),
            ("transports", List(&Enum(&["ws", "get"])), Required),
        ]),
        ("compaction", Struct("CompactionRow"), Required, [
            ("last_compaction_message_id", Cursor("message"), Required),
            ("needs_restate", Sc(Bool), Required),
            ("range", List(&Cursor("message")), Optional),
        ]),
        // `<thread_id>.<last_compaction_message_id>` — the id of the context
        // the thread is in NOW, derived at export off `compaction` by the one
        // function the prompt's `context:` line uses (`context_id_of`). Opaque
        // to a consumer: compare it whole, never parse it. ADDITIVE, so
        // `Threads::VERSION` does not move (rule 2.6's cheapening property).
        ("context_id", Sc(Str), Required, []),
        // The thread's message history as RESOURCE ids, one per page of
        // `pages::PAGE_MESSAGES` message ids, oldest first, cut at
        // `last_message_id`. Read each through `resource/{id}/read` (or `GET
        // /resource/{id}`); pick pages by `from`/`to`, never build an id.
        // ADDITIVE, so `Threads::VERSION` does not move.
        ("pages", List(&Struct("MessagePage")), Required, [
            ("id", Ref("resource"), Required),
            ("from", Cursor("message"), Required),
            ("to", Cursor("message"), Required),
        ]),
        ("liveness", Struct("LivenessRow"), Required, [
            ("busy", Sc(Bool), Required),
            ("parent_running", Sc(Bool), Required),
            // A LIST of ids, not a count — a consumer that read it as a number
            // got `None` and defaulted to zero for as long as it existed.
            // CLI task ids of IN-PROCESS subagents (`runtime::Liveness`), NOT
            // thread ids: no `threads` key answers one, so no `Ref` is declared.
            ("running_subagents", List(&Sc(Str)), Required),
        ]),
        // The application's own events, held opaquely: newest per event type.
        ("app", MapOf(&Opaque), Required, []),
        // The thread's parent (a key of THIS shape), `null` for a root — read
        // off `ThreadMeta.parent_id`, the fact `agents/list` answers from.
        // ADDITIVE and LAST, so `Threads::VERSION` does not move.
        ("parent_id", Ref("thread"), Optional, []),
        // The thread's managed children (`ChildrenRow`): the ids it is still
        // owed (the daemon's own wake fold) and one entry per committed spawn.
        // ADDITIVE and LAST, so `Threads::VERSION` does not move.
        ("children", Struct("ChildrenRow"), Required, [
            ("outstanding", List(&Ref("thread")), Required),
            ("runs", List(&Struct("ChildRun")), Required),
        ]),
        // The status rolled up over the WHOLE subtree (R9.TS1, operator ruling
        // r42: `active`, `idle` or an error state, nothing else) — the rule is
        // `shapewire::subtree::rollup`, and `display` / `children_summary` are
        // `shapewire::rolled`'s strings, published rendered so a face needs no
        // logic. ADDITIVE and LAST, so `Threads::VERSION` does not move.
        ("rolled", Struct("RolledRow"), Required, [
            ("status", Enum(shapewire::rolled::STATUS_WORDS), Required),
            ("error", Enum(shapewire::rolled::ERROR_LABELS), Optional),
            ("active_descendants", Sc(Int), Required),
            ("display", Sc(Str), Required),
            ("children_active", Sc(Int), Optional),
            ("children_idle", Sc(Int), Optional),
            ("children_error", Sc(Int), Optional),
            ("children_summary", Sc(Str), Optional),
        ]),
        // The thread's in-process (Task / Agent tool) subagents, keyed by the
        // launching `tool_use_id` (R9.TS2, TC-10): the daemon's own status for
        // each — `active`, `idle` or `error` with its label — and the title
        // the model gave it. `task_id` joins to `liveness.running_subagents`;
        // `parent_tool_use_id` is the key of the subagent that made the call.
        // ADDITIVE and LAST, so `Threads::VERSION` does not move.
        ("subagents", MapOf(&Struct("SubagentRow")), Required, [
            ("status", Enum(shapewire::rolled::STATUS_WORDS), Required),
            ("error", Enum(shapewire::rolled::ERROR_LABELS), Optional),
            ("display", Sc(Str), Required),
            ("task_id", Sc(Str), Optional),
            ("parent_tool_use_id", Sc(Str), Optional),
            ("last_activity_at", Ts(Rfc3339), Required),
        ]),
    ]
}

declare_cells! {
    /// Shape-level cells of `threads` — the `shared_cells` block.
    pub THREADS_SHARED = [
        ("fleet_size", Sc(Int), Required, []),
        ("key_scope", Struct("KeyScope"), Required, [
            ("mode", Enum(&["whole", "per_key"]), Required),
            ("includes_idle", Sc(Bool), Required),
            ("keyless_rows", Enum(&["refused"]), Required),
            ("keyless_keys", Enum(&["membership"]), Required),
        ]),
        ("limits", Struct("Limits"), Required, [
            ("query_default_limit", Sc(Int), Required),
            ("query_max_limit", Sc(Int), Required),
            ("message_kinds", List(&Enum(crate::server::MESSAGE_KINDS)), Required),
        ]),
        ("shape_version", Sc(Int), Required, []),
    ]
}

impl Shape<Input> for Threads {
    const NAME: &'static str = "threads";
    const CELLS: Cells = THREADS_CELLS;
    const SHARED: Cells = THREADS_SHARED;
    /// The key-level nesting edges, declared: `parent_id` and
    /// `children.outstanding` hold keys of THIS shape (a thread's children are
    /// threads — "a graph traversal on nodes in a flat list"), `account` holds a key of
    /// `accounts`. `body.id` is `Ref("resource")` and is deliberately NOT here:
    /// a resource id is answered by the resource door, not by a shape.
    const REFERS: &'static [(&'static str, &'static str)] =
        &[("thread", "threads"), ("account", "accounts")];
    const TRACKING: Tracking = Tracking::Lazy;
    /// **PER-KEY, and the module doc carries the whole argument**: 5,417
    /// monotone keys make `Whole` a 20 MB first export, `accounts` already holds
    /// the live subset, and the thing with no home is one thread in depth —
    /// which is idle most of the time and therefore outside any active set.
    ///
    /// **What that used to cost, and no longer does.** A keyless subscribe here
    /// is refused, which meant there was no way to ask this pool WHAT EXISTS
    /// without already naming a key you did not have. `detail: keys` is that
    /// ask, and [`Self::hydrates_members`] is what answers it — one statement
    /// over one column, never a row scan.
    const HYDRATE: Hydrate = Hydrate::PerKey;
    /// One `Push::Status` or one thread fact touches exactly one key, so any
    /// burst bigger than a handful means the fold has started reaching for keys
    /// it does not change. `accounts` sits at 8 because a `pool_reloaded`
    /// legitimately touches every key at once; nothing here ever does.
    const BURST: usize = 4;
    /// Declared so it is seeded at zero and is ALWAYS on `shapes.list` — a number
    /// that appears only once something goes wrong reads as "this daemon has no
    /// such counter", which is a different claim.
    const COUNTERS: &'static [&'static str] = &["events_ignored"];

    type Producer = crate::runtime::ThreadManager;
    type Root = ThreadsRoot;

    fn root(&self) -> ThreadsRoot {
        ThreadsRoot::default()
    }

    /// **HOW MUCH OF ITS INPUT THIS FOLD THROWS AWAY.**
    ///
    /// 58% of the production ledger is the three [`BULK_KINDS`]. The count is the
    /// operator's answer to "is this fold worth its FIFO", and it is computed
    /// from the input alone — `counts` cannot see the root, so it counts the
    /// kinds it can name and not the scope misses.
    fn counts(&self, input: &Stamped<Input>) -> Option<&'static str> {
        match &input.value {
            Input::Push(Push::Event(se)) if BULK_KINDS.contains(&se.event.kind()) => {
                Some("events_ignored")
            }
            _ => None,
        }
    }

    /// **INIT READS NO THREAD.** The default key set of an `OnDemand` shape is
    /// empty, so this hydrates the shape-level cells and nothing else: one
    /// `COUNT(*)` and three constants. Every thread is read by
    /// [`Self::hydrates_key`], because somebody named it.
    fn hydrates(
        &mut self,
        p: &crate::runtime::ThreadManager,
        root: &mut ThreadsRoot,
        as_of: Timestamp,
    ) {
        root.set_shared_constants(as_of);
        let fleet = p.hydrate(FleetSize);
        match fleet.rows().first() {
            Some(n) => root.set_fleet_size(as_of, *n),
            None => root.fleet_size.not_hydrated(
                fleet
                    .reason()
                    .unwrap_or_else(|| "the thread count could not be read".into()),
            ),
        }
        // ZERO KEYS IS THE COMPLETE DEFAULT SCOPE for this shape, and
        // `shared.key_scope` is what says so structurally. `Known` with no rows
        // renders as empty; `NotHydrated` renders `?`, and a consumer that has
        // not yet named a thread is not looking at a failure.
        root.rows.mark_read(as_of, true);
        // A hydrate publishes nothing — the export IS the publication.
        let _ = root.drain();
    }

    /// **WHAT IS IN THE POOL — ONE STATEMENT, ONE COLUMN, NO ROW.**
    ///
    /// `SELECT id FROM threads ORDER BY id LIMIT n`, covered by the primary-key
    /// index. This is the whole of `detail: keys` on this shape, and it is
    /// affordable for exactly the reason [`Self::hydrates`] is not allowed to
    /// read a thread: ids are ~36 bytes and rows are ~3.7 KB, so the fleet is
    /// 200 KB here and 20 MB there.
    ///
    /// It runs when a `detail: keys` subscribe arrives and not at init — the
    /// module doc's INIT READS NO THREAD is unchanged, and a consumer that names
    /// a key still pays one primary-key read and nothing else.
    ///
    /// **A SHORT READ SAYS IT IS SHORT.** Exactly [`MAX_MEMBER_IDS`] ids back
    /// means the statement hit its own limit, so the membership is marked
    /// `complete: false` and the export carries it — never a truncated roster
    /// claiming to be the pool, which is the failure `tests/hydrate_bound.rs`
    /// exists for on the sibling shape.
    fn hydrates_members(
        &mut self,
        p: &crate::runtime::ThreadManager,
        root: &mut ThreadsRoot,
        as_of: Timestamp,
    ) -> bool {
        let got = p.hydrate(ThreadIds);
        if let Some(reason) = match &got {
            Hydration::Empty { reason } => Some((*reason).to_string()),
            // A `Partial` with no rows is a READ THAT FAILED, not a pool with no
            // members: `Handles<ThreadIds>` answers `Partial{got: []}` on a
            // store error. An empty pool comes back `Complete(vec![])`.
            Hydration::Partial { got, reason } if got.is_empty() => Some(reason.clone()),
            _ => None,
        } {
            root.rows.mark_members_not_hydrated(reason);
            return false;
        }
        let ids = got.rows();
        // The read hit its own ceiling, so the roster is a floor and not the set.
        let complete = got.is_complete() && ids.len() < MAX_MEMBER_IDS;
        for id in ids {
            // `member`, never `key`: a member is not a row. Minting rows here
            // would be the 20 MB export arriving through the membership door.
            root.rows.member(id.clone());
        }
        root.rows.mark_members_read(as_of, complete);
        // A hydrate publishes nothing — the export IS the publication.
        let _ = root.drain();
        true
    }

    /// **ONE THREAD, BECAUSE SOMEBODY NAMED IT.** Eleven statements, flat in the
    /// fleet size and in the activity level; see the module doc's bound.
    ///
    /// Answers `false` for an id that names no thread, which is the honest
    /// refusal: the chassis logs it and exports a row-less frame, so the consumer
    /// is told rather than left on an ack. **Nothing is minted on that path** — a
    /// row of `?`s for a thread that does not exist cannot be told apart from a
    /// thread whose hydrate failed.
    fn hydrates_key(
        &mut self,
        p: &crate::runtime::ThreadManager,
        root: &mut ThreadsRoot,
        key: &str,
        as_of: Timestamp,
    ) -> bool {
        let id = key.to_string();
        let snap = p.hydrate(ThreadRowRead {
            thread_id: id.clone(),
        });
        let snap_complete = snap.is_complete();
        let Some(s) = snap.rows().first().cloned() else {
            return false;
        };
        {
            let row = root.rows.key(id.clone());
            if snap_complete {
                row.header.read(as_of, s.header.clone());
                row.settings.read(as_of, s.settings.clone());
                row.account.read(as_of, s.account.clone());
            } else {
                row.header.read_partial(as_of, s.header.clone());
                row.settings.read_partial(as_of, s.settings.clone());
                row.account.read_partial(as_of, s.account.clone());
            }
            row.compaction.read(as_of, s.compaction.clone());
        }

        // ── the ONE derivation, and the two positions it carries ──────────
        let derived = p.hydrate(ThreadDerivedState {
            thread_id: id.clone(),
        });
        let derived_reason = derived.reason();
        match derived.rows().first().cloned() {
            Some(d) => {
                let row = root.rows.key(id.clone());
                row.status.read(as_of, d.level);
                row.queued.read(as_of, d.queued);
                row.seen_seq.read(as_of, d.seen_seq);
                row.unread.read(as_of, d.unread);
            }
            None => {
                let why = derived_reason
                    .unwrap_or_else(|| "this thread's state could not be derived".into());
                let row = root.rows.key(id.clone());
                row.status.not_hydrated(why.clone());
                row.queued.not_hydrated(why.clone());
                row.seen_seq.not_hydrated(why.clone());
                row.unread.not_hydrated(why);
            }
        }

        // ── where the log's head is ───────────────────────────────────────
        let newest = p.hydrate(ThreadNewestEvent {
            thread_id: id.clone(),
        });
        let newest_complete = newest.is_complete();
        let newest_reason = newest.reason();
        match newest.rows().first().cloned() {
            Some(n) => {
                let row = root.rows.key(id.clone());
                row.newest.read(as_of, n.newest);
                // **TWO OUTCOMES, TWO CELL STATES.** A payload this shape could
                // not read leaves `newest` known and says `?` about the body
                // alone — which is the whole reason `body` is its own cell.
                match n.body {
                    Ok(summary) => row.body.read(as_of, summary),
                    Err(why) => row.body.not_hydrated(why),
                }
            }
            // A thread with no events is a COMPLETE answer and renders as empty;
            // only a failed read renders `?`.
            None if !newest_complete => {
                let why = newest_reason.unwrap_or_default();
                let row = root.rows.key(id.clone());
                row.newest.not_hydrated(why.clone());
                row.body.not_hydrated(why);
            }
            None => {
                let row = root.rows.key(id.clone());
                row.newest.read(
                    as_of,
                    NewestRow {
                        seq: 0,
                        message_id: None,
                        kind: String::new(),
                        ts: String::new(),
                    },
                );
                // No events at all is a complete answer about the body too:
                // there is none, and that is known rather than unhydrated.
                row.body.read(as_of, None);
            }
        }

        let head = p.hydrate(ThreadMessageHead {
            thread_id: id.clone(),
        });
        // `max_message_id` answers 0 for a thread with no addressable event, and
        // 0 is not a message id — the honest cell value is `None`.
        let head_value = head.rows().first().copied().filter(|n| *n > 0);
        root.rows
            .key(id.clone())
            .last_message_id
            .read(as_of, head_value);

        // ── the application's events, OPAQUE ──────────────────────────────
        let app = p.hydrate(ThreadAppState {
            thread_id: id.clone(),
        });
        let app_complete = app.is_complete();
        let app_reason = app.reason();
        for e in app.rows() {
            root.rows
                .key(id.clone())
                .app
                .key(e.event_type.clone())
                .read(as_of, e.data.clone());
        }
        {
            let row = root.rows.key(id.clone());
            match (&app_reason, app_complete) {
                (Some(why), false) => row.app.mark_not_hydrated(why.clone()),
                _ => row.app.mark_read(as_of, true),
            }
        }

        // ── the beat's level, out of MEMORY and not out of a table ────────
        let live = p.hydrate(ThreadLiveness {
            thread_id: id.clone(),
        });
        let live_reason = live.reason();
        match live.rows().first().cloned() {
            Some(l) => root.rows.key(id.clone()).liveness.read(as_of, l),
            None => root.rows.key(id.clone()).liveness.not_hydrated(
                live_reason.unwrap_or_else(|| "no beat has been read for this thread".into()),
            ),
        }

        // ── lineage: the parent, off the managed ledger ───────────────────
        let parent = p.hydrate(ThreadParent {
            thread_id: id.clone(),
        });
        let parent_reason = parent.reason();
        match parent.rows().first().cloned() {
            Some(pid) if parent.is_complete() => {
                root.rows.key(id.clone()).parent_id.read(as_of, pid)
            }
            _ => root.rows.key(id.clone()).parent_id.not_hydrated(
                parent_reason.unwrap_or_else(|| "this thread's lineage could not be read".into()),
            ),
        }

        // ── lineage: the children, off the same managed ledger ───────────
        let kids = p.hydrate(ThreadChildren {
            thread_id: id.clone(),
        });
        let kids_reason = kids.reason();
        match kids.rows().first().cloned() {
            Some(c) if kids.is_complete() => root.rows.key(id.clone()).children.read(as_of, c),
            _ => root.rows.key(id.clone()).children.not_hydrated(
                kids_reason.unwrap_or_else(|| "this thread's children could not be read".into()),
            ),
        }

        // ── the status, rolled up over the whole subtree (R9.TS1) ────────
        let rolled = p.hydrate(ThreadRolledRead {
            thread_id: id.clone(),
        });
        let rolled_reason = rolled.reason();
        match rolled.rows().first().cloned() {
            Some(r) if rolled.is_complete() => root.rows.key(id.clone()).rolled.read(as_of, r),
            _ => root.rows.key(id.clone()).rolled.not_hydrated(
                rolled_reason
                    .unwrap_or_else(|| "this thread's subtree status could not be read".into()),
            ),
        }

        // ── the in-process Task subagents (R9.TS2), out of MEMORY ─────────
        let subs = p.hydrate(ThreadSubagentsRead {
            thread_id: id.clone(),
        });
        let subs_reason = subs.reason();
        match subs.rows().first().cloned() {
            Some(m) if subs.is_complete() => root.rows.key(id.clone()).subagents.read(as_of, m),
            _ => root.rows.key(id.clone()).subagents.not_hydrated(
                subs_reason.unwrap_or_else(|| "this thread's subagents could not be read".into()),
            ),
        }

        // ── the undelivered queue ─────────────────────────────────────────
        //
        // `Empty` is a KNOWN empty queue, so it reads as an empty list and not
        // as `?`: a thread with nothing parked is the ordinary case, and
        // rendering it unhydrated would put a `?` on almost every thread.
        let q = p.hydrate(ThreadQueueRows {
            thread_id: id.clone(),
        });
        let q_complete = q.is_complete();
        let q_reason = q.reason();
        let q_rows = q.rows().to_vec();
        {
            let row = root.rows.key(id.clone());
            if q_complete {
                row.queue.read(as_of, q_rows);
            } else {
                row.queue.not_hydrated(
                    q_reason.unwrap_or_else(|| "this thread's queue could not be read".into()),
                );
            }
        }

        // A hydrate publishes nothing.
        let _ = root.drain();
        true
    }

    /// **EXHAUSTIVE, WITH NO `_` ARM**, so a new `Push` or `Fact` variant is a
    /// compile error here rather than a silent drop.
    ///
    /// Two rules bind every arm:
    ///
    /// * **The bulk kinds return FIRST.** 58% of the ledger is `stream_event` /
    ///   `message_usage` / `unknown` and no cell here reads one.
    /// * **NO ARM MINTS A KEY.** Every arm asks [`holds`] first. That is what
    ///   keeps an `OnDemand` root from growing to the whole fleet as the ledger
    ///   flows, and it is what makes `docs/shapes.md` §7.2's export-before-row
    ///   gap unreachable here: a key exists only because a subscribe created it,
    ///   and that subscribe's export is what opened the connection's gate for it.
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, ThreadsRoot>) {
        let at = tx.at();
        // ── MEMBERSHIP FIRST, AND WITHOUT MINTING A ROW ──────────────────────
        //
        // Every input below that names a thread PROVES that thread exists, and
        // that is a fact the pool owes a `detail: keys` subscriber whether or
        // not anybody is holding the key. `member` records it in a `BTreeSet`
        // and yields ONE `added` event the first time; a repeat is silent, so a
        // 50 events/s ledger does not become 50 frames/s.
        //
        // **It runs BEFORE the bulk-kind return and before every `holds` guard,
        // deliberately.** 58% of the ledger is the three `BULK_KINDS` and this
        // fold reads none of them — but a `stream_event` is still proof that its
        // thread exists, and a thread whose only traffic is bulk would otherwise
        // be invisible to the pool. The cost is one `BTreeSet` lookup per input.
        //
        // **And it does not weaken the scope guard.** `holds` still gates every
        // row-touching arm, `Map::key` is still never reached for an unheld
        // thread, and the root still grows by ~36 bytes per thread instead of by
        // a row. That guard is what keeps a 2.46 M-event ledger from growing
        // this root to the fleet, and it is untouched.
        if let Some(id) = subject(&input.value) {
            tx.rows.member(id.to_string());
        }
        match &input.value {
            // ── the ledger envelope, and the app's opaque payloads ────────
            Input::Push(Push::Event(se)) => {
                if BULK_KINDS.contains(&se.event.kind()) {
                    return;
                }
                if !holds(tx, &se.thread_id) {
                    return;
                }
                // **READ BEFORE REACHING.** `Map::key` marks the key dirty on
                // reach, deliberately (a caller that reaches for a key is about
                // to change it), so a guard that runs AFTER `key(..)` has already
                // cost a `row` frame. Every decision this arm makes is therefore
                // taken through `get` first, and `key` is called only once
                // something is actually going to move. `BURST: 4` is the backstop
                // and this is the discipline it backs.
                let held_seq = tx
                    .rows
                    .get(&se.thread_id)
                    .and_then(|r| r.newest.now().map(|n| n.seq));
                // WHERE THE HEAD IS, ordered on `seq` — see `NewestRow`.
                let stale = held_seq.is_some_and(|s| se.seq <= s);
                let custom = matches!(&se.event, crate::event::ThreadEvent::Custom { .. });
                if stale && se.message_id.is_none() && !custom {
                    // A replayed or out-of-order event that moves no cell.
                    return;
                }
                let row = tx.rows.key(se.thread_id.clone());
                if !stale {
                    row.newest.emit(
                        at,
                        NewestRow {
                            seq: se.seq,
                            message_id: se.message_id,
                            kind: se.event.kind().to_string(),
                            ts: se.ts.clone(),
                        },
                    );
                    // **THE BODY SUMMARY MOVES WITH THE HEAD, AND ONLY WITH
                    // IT.** It is emitted on EVERY non-stale event, including
                    // one with no body, so the summary always describes the
                    // event `newest` names. Emitting only when there IS a body
                    // would leave the previous message's id beside a newer
                    // `newest` — a summary naming a resource that is no longer
                    // what the row is about, which is worse than no summary.
                    //
                    // The fold reads the text off the event in hand. It does
                    // not read the store (rule: a fold does no I/O) and it does
                    // not read a clock.
                    row.body.emit(
                        at,
                        crate::shapes::body::body_text(&se.event)
                            .and_then(|t| {
                                crate::shapes::body::summary(&se.thread_id, se.seq, t)
                            }),
                    );
                }
                if let Some(mid) = se.message_id {
                    let ahead = row
                        .last_message_id
                        .now()
                        .cloned()
                        .flatten()
                        .is_none_or(|held| mid > held);
                    if ahead {
                        row.last_message_id.emit(at, Some(mid));
                    }
                    // THE EXACT UNREAD COUNT: addressable, and above the seen
                    // position. See `ThreadCells::unread`.
                    let seen = row.seen_seq.now().copied().unwrap_or(0);
                    if se.seq > seen && !stale && row.unread.state().is_known() {
                        let held = row.unread.now().copied().unwrap_or(0);
                        row.unread.emit(at, held + 1);
                    }
                }
                if let crate::event::ThreadEvent::Custom { event_type, data } = &se.event {
                    // OPAQUE. The `Value` is stored and never read.
                    row.app.key(event_type.clone()).emit(at, data.clone());
                }
            }

            // ── the undelivered queue, as a LEVEL ────────────────────────
            //
            // The whole set, replacing whatever was held: see
            // `Fact::ThreadQueue` for why the input is a level and not a delta.
            // `queued` is NOT written here even though these rows contain the
            // answer — that cell has one writer, `Push::Status`, which is
            // emitted on every one of these paths, and two writers behind one
            // cell is how a mirror learns to disagree with itself.
            Input::Fact(Fact::ThreadQueue { thread_id, rows }) => {
                if !holds(tx, thread_id) {
                    return;
                }
                tx.rows.key(thread_id.clone()).queue.emit(at, rows.clone());
            }

            // ── the managed children, as a LEVEL ─────────────────────────
            Input::Fact(Fact::ThreadChildren { thread_id, row }) => {
                if !holds(tx, thread_id) {
                    return;
                }
                tx.rows.key(thread_id.clone()).children.emit(at, row.clone());
            }

            // ── the subtree's status, as a LEVEL that only speaks on change ──
            Input::Fact(Fact::ThreadRolled { thread_id, row }) => {
                if !holds(tx, thread_id) {
                    return;
                }
                // Compared through `get`, never `key` (which dirties on reach):
                // every status edge re-states the whole ancestor chain, and an
                // unchanged level must not cost a `row` frame.
                if tx
                    .rows
                    .get(thread_id)
                    .is_some_and(|r| r.rolled.now() == Some(row))
                {
                    return;
                }
                tx.rows.key(thread_id.clone()).rolled.emit(at, row.clone());
            }

            // ── a child ACTED: move its run's `last_activity_at` (TC-44) ──
            //
            // Applied to the held `children` level in place — the lineage is
            // the ledger's and is not re-read for an activity tick. A run this
            // row does not hold yet (the spawn fact still in flight), or an
            // older stamp than the held one, moves nothing.
            Input::Fact(Fact::ThreadChildActivity { thread_id, child_id, at: when }) => {
                if !holds(tx, thread_id) {
                    return;
                }
                let Some(held) = tx.rows.get(thread_id).and_then(|r| r.children.now().cloned()) else {
                    return;
                };
                let mut next = held.clone();
                let Some(run) = next.runs.iter_mut().rev().find(|r| &r.id == child_id) else {
                    return;
                };
                if run.last_activity_at.as_deref().is_some_and(|h| h >= when.as_str()) {
                    return;
                }
                run.last_activity_at = Some(when.clone());
                tx.rows.key(thread_id.clone()).children.emit(at, next);
            }

            // ── the in-process Task subagents, as a LEVEL ────────────────
            Input::Fact(Fact::ThreadSubagents { thread_id, rows }) => {
                if !holds(tx, thread_id) {
                    return;
                }
                if tx
                    .rows
                    .get(thread_id)
                    .is_some_and(|r| r.subagents.now() == Some(rows))
                {
                    return;
                }
                tx.rows.key(thread_id.clone()).subagents.emit(at, rows.clone());
            }

            // ── the status EDGE ──────────────────────────────────────────
            Input::Push(Push::Status {
                thread_id,
                state,
                executing,
                queued,
                unread: _,
                detail,
                account,
            }) => {
                if !holds(tx, thread_id) {
                    return;
                }
                let row = tx.rows.key(thread_id.clone());
                row.status.emit(
                    at,
                    StateLevel {
                        state: state.clone(),
                        executing: *executing,
                        detail: detail.clone(),
                    },
                );
                row.queued.emit(at, *queued);
                // `None` means the daemon does not know, so it writes nothing —
                // an honest absence must not overwrite a placement we hold.
                if let Some(a) = account {
                    row.account.emit(at, Some(a.clone()));
                }
            }

            // ── the beat, as a LEVEL that only speaks when it changes ─────
            Input::Push(Push::Liveness {
                thread_id,
                busy,
                parent_running,
                running_subagents,
            }) => {
                if !holds(tx, thread_id) {
                    return;
                }
                let next = LivenessRow {
                    busy: *busy,
                    parent_running: *parent_running,
                    running_subagents: running_subagents.clone(),
                };
                // A 1 Hz beat repeating the held level is not news. **Compared
                // through `get`, never through `key`** — `key` dirties on reach, so
                // the same comparison one line later still costs the `row` frame it
                // exists to avoid. That is not a theory: the test below caught it.
                if tx
                    .rows
                    .get(thread_id)
                    .is_some_and(|r| r.liveness.now() == Some(&next))
                {
                    return;
                }
                tx.rows.key(thread_id.clone()).liveness.emit(at, next);
            }

            // ── the produced thread facts ────────────────────────────────
            Input::Fact(Fact::ThreadHeader { thread_id, row }) => {
                if !holds(tx, thread_id) {
                    return;
                }
                tx.rows.key(thread_id.clone()).header.emit(at, row.clone());
            }
            Input::Fact(Fact::ThreadSettings {
                thread_id,
                settings,
            }) => {
                if !holds(tx, thread_id) {
                    return;
                }
                tx.rows
                    .key(thread_id.clone())
                    .settings
                    .emit(at, settings.clone());
            }
            Input::Fact(Fact::ThreadPlacement { thread_id, account }) => {
                if !holds(tx, thread_id) {
                    return;
                }
                tx.rows
                    .key(thread_id.clone())
                    .account
                    .emit(at, account.clone());
            }
            Input::Fact(Fact::ThreadSeen {
                thread_id,
                seen_seq,
                unread,
            }) => {
                if !holds(tx, thread_id) {
                    return;
                }
                let row = tx.rows.key(thread_id.clone());
                row.seen_seq.emit(at, *seen_seq);
                // The RESET half of the exact count: when the cursor jumps, the
                // fold cannot know what the jump swallowed, so the write site
                // recomputed it.
                row.unread.emit(at, *unread);
            }
            Input::Fact(Fact::ThreadCompaction { thread_id, row }) => {
                if !holds(tx, thread_id) {
                    return;
                }
                tx.rows
                    .key(thread_id.clone())
                    .compaction
                    .emit(at, row.clone());
            }

            // ── narration's, and the `accounts` shape's ───────────────────
            //
            // EXHAUSTIVE AND DELIBERATE. `Progress`/`Lifecycle`/`Info` are
            // `crate::shapes::narration`'s three channels; every `Fact` below is
            // about an ACCOUNT and belongs to `crate::shapes::accounts`.
            Input::Push(Push::Progress { .. })
            | Input::Push(Push::Lifecycle(_))
            | Input::Push(Push::Info { .. })
            | Input::Fact(Fact::GaugeObserved { .. })
            | Input::Fact(Fact::LimitRecorded { .. })
            | Input::Fact(Fact::LimitCleared { .. })
            | Input::Fact(Fact::PoolShare { .. })
            | Input::Fact(Fact::AccountDisabled { .. })
            | Input::Fact(Fact::LoginHealth { .. })
            | Input::Fact(Fact::CredentialChanged { .. })
            | Input::Fact(Fact::AccountRegistered { .. })
            | Input::Fact(Fact::AccountRemoved { .. })
            | Input::Fact(Fact::Pressure { .. })
            // **AND THE BEAT, WHICH IS THE ONE TO READ TWICE.** This shape's
            // `liveness` cell is a level about a THREAD BEING BUSY, and a reader
            // who sees "heartbeat" here will reach for it. They are different
            // facts: `liveness` goes silent when nothing is running and says
            // nothing about whether frames reach anybody, and
            // `crate::shapes::heartbeat` beats whether or not any thread exists.
            // Folding a link beat into a thread's row would make an idle thread
            // look busy and a wedged link look fine.
            | Input::Fact(Fact::Beat { .. })
            // The producer's own warning count is `crate::shapes::warnings`'
            // business and nothing else's.
            | Input::Fact(Fact::WarningsSampled { .. }) => {}
        }
    }
}

/// **THE SCOPE GUARD.** Does this root already hold that key? A `PerKey`
/// shape's fold may never mint one — see [`Threads::folds`].
fn holds(tx: &Tx<'_, ThreadsRoot>, thread_id: &str) -> bool {
    tx.rows.contains(&thread_id.to_string())
}

/// **WHICH THREAD THIS INPUT IS ABOUT, OR NONE** — the membership statement,
/// and nothing else reads it.
///
/// **EXHAUSTIVE, WITH NO `_` ARM**, for the same reason [`Threads::folds`] is:
/// a new `Push` or `Fact` variant must be a compile error here, so that an
/// input that names a thread cannot be added and silently leave the pool blind
/// to it. Membership is a claim about what exists; a claim that quietly stops
/// being made is worse than one that was never made.
fn subject(input: &Input) -> Option<&str> {
    match input {
        Input::Push(Push::Event(se)) => Some(&se.thread_id),
        Input::Push(Push::Status { thread_id, .. })
        | Input::Push(Push::Liveness { thread_id, .. }) => Some(thread_id),
        Input::Fact(Fact::ThreadHeader { thread_id, .. })
        | Input::Fact(Fact::ThreadSettings { thread_id, .. })
        | Input::Fact(Fact::ThreadPlacement { thread_id, .. })
        | Input::Fact(Fact::ThreadSeen { thread_id, .. })
        | Input::Fact(Fact::ThreadCompaction { thread_id, .. })
        | Input::Fact(Fact::ThreadQueue { thread_id, .. })
        | Input::Fact(Fact::ThreadChildren { thread_id, .. })
        | Input::Fact(Fact::ThreadRolled { thread_id, .. })
        | Input::Fact(Fact::ThreadSubagents { thread_id, .. })
        | Input::Fact(Fact::ThreadChildActivity { thread_id, .. }) => Some(thread_id),
        // ABOUT AN ACCOUNT, OR ABOUT THE DAEMON — no thread is named, so no
        // membership is claimed. `Progress` says so in its own doc: *"It
        // carries no `thread_id` because it belongs to no thread."*
        Input::Push(Push::Progress { .. })
        | Input::Push(Push::Lifecycle(_))
        | Input::Push(Push::Info { .. })
        | Input::Fact(Fact::GaugeObserved { .. })
        | Input::Fact(Fact::LimitRecorded { .. })
        | Input::Fact(Fact::LimitCleared { .. })
        | Input::Fact(Fact::PoolShare { .. })
        | Input::Fact(Fact::AccountDisabled { .. })
        | Input::Fact(Fact::LoginHealth { .. })
        | Input::Fact(Fact::CredentialChanged { .. })
        | Input::Fact(Fact::AccountRegistered { .. })
        | Input::Fact(Fact::AccountRemoved { .. })
        | Input::Fact(Fact::Pressure { .. })
        // A beat has no subject at all — it is not about a thread, which
        // is the distinction `crate::shapes::heartbeat`'s module doc draws
        // against this shape's own `liveness` cell.
        | Input::Fact(Fact::Beat { .. })
        // A warning count is about the PRODUCER, so it names no thread.
        | Input::Fact(Fact::WarningsSampled { .. }) => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The producer's answers — one read each, through the SAME builder the live
// fact goes through
// ─────────────────────────────────────────────────────────────────────────────

use crate::runtime::ThreadManager;

impl Handles<ThreadIds> for ThreadManager {
    /// **ONE `SELECT id FROM threads ORDER BY id LIMIT n`.** Never `Empty`: a
    /// store with no threads is a COMPLETE answer of zero members, and a `?` on
    /// "what is in this pool" would be read as a broken daemon — the same
    /// argument `FleetSize` makes one line down.
    ///
    /// A store error is `Partial{got: []}`, which the shape turns into
    /// `NotHydrated{reason}` on the membership rather than into an empty roster.
    /// Those are two different claims and a consumer must be able to tell them
    /// apart: "there are no threads" and "I could not look".
    fn hydrate(&self, _: ThreadIds) -> Hydration<String> {
        match self.store.thread_ids(MAX_MEMBER_IDS) {
            Ok(ids) => Hydration::Complete(ids),
            Err(e) => Hydration::Partial {
                got: Vec::new(),
                reason: format!("{e}"),
            },
        }
    }
}

impl Handles<FleetSize> for ThreadManager {
    /// One `SELECT COUNT(*)`, once per epoch. Never `Empty`: a store with no
    /// threads is a complete answer of zero, and a `?` on "how many threads
    /// exist" would be read as a broken daemon.
    fn hydrate(&self, _: FleetSize) -> Hydration<u64> {
        match self.store.count_threads() {
            Ok(n) => Hydration::Complete(vec![n]),
            Err(e) => Hydration::Partial {
                got: Vec::new(),
                reason: format!("{e}"),
            },
        }
    }
}

impl Handles<ThreadRowRead> for ThreadManager {
    /// ONE primary-key read, and the `settings` blob comes out of
    /// [`settings_of`] — the same expression `thread_json` uses, which is the
    /// whole point of the function existing.
    ///
    /// `Empty` for a thread id that names no thread. That is a statement about
    /// the world, and [`Threads::hydrates_key`] turns it into a refusal rather
    /// than into a row of `?`s.
    ///
    /// The compaction RANGE costs a second read (`events_by_range` on
    /// `idx_events_thread_kind`, tail 1), and it is here rather than in a
    /// request of its own because the row it belongs to is written and hydrated
    /// as one value.
    fn hydrate(&self, r: ThreadRowRead) -> Hydration<ThreadRowSnapshot> {
        let row = match self.store.get_thread(&r.thread_id) {
            Ok(Some(row)) => row,
            Ok(None) => {
                return Hydration::Empty {
                    reason: "no thread has this id",
                }
            }
            Err(e) => {
                return Hydration::Partial {
                    got: Vec::new(),
                    reason: format!("{e}"),
                }
            }
        };
        let range = self.last_compaction_span(&r.thread_id);
        Hydration::Complete(vec![ThreadRowSnapshot {
            header: header_of(&row),
            settings: settings_of(&row),
            account: row.account.clone(),
            compaction: compaction_of(&row, range),
        }])
    }
}

impl Handles<ThreadDerivedState> for ThreadManager {
    /// **THE ONE DERIVATION, and no second copy.** `thread_states` is where
    /// every answer to "is this thread running" comes from — `threads.status`,
    /// `threads.list`, `threads.get`, the `status` push frame — so this shape
    /// asks it rather than deriving anything of its own. Five whole-table reads
    /// of one-row-per-*interesting*-thread tables, plus one `unread_count`.
    ///
    /// Never `Empty`: a thread nothing knows anything about is genuinely `idle`,
    /// and `DerivedState::idle` says so. That is the answer, not a default.
    fn hydrate(&self, r: ThreadDerivedState) -> Hydration<DerivedSnapshot> {
        let d = match self.thread_state(&r.thread_id) {
            Ok(d) => d,
            Err(e) => {
                return Hydration::Partial {
                    got: Vec::new(),
                    reason: format!("{e}"),
                }
            }
        };
        // The one read `thread_states` deliberately leaves out, because
        // `threads.list` does not want it: the EXACT unread count above the seen
        // cursor. It is the same statement `threads.status` answers with.
        let unread = match self.store.unread_count(&r.thread_id, d.last_seen_seq) {
            Ok(n) => n,
            Err(e) => {
                return Hydration::Partial {
                    got: Vec::new(),
                    reason: format!("{e}"),
                }
            }
        };
        Hydration::Complete(vec![DerivedSnapshot {
            level: StateLevel {
                state: d.state.to_string(),
                executing: d.executing,
                detail: d.detail.clone(),
            },
            queued: d.queued,
            seen_seq: d.last_seen_seq,
            unread,
        }])
    }
}

impl Handles<ThreadNewestEvent> for ThreadManager {
    /// ONE seek on `idx_events_thread_seq`. `Empty` when the thread has no
    /// events at all, which is a complete answer about the world and renders as
    /// empty rather than `?`.
    fn hydrate(&self, r: ThreadNewestEvent) -> Hydration<NewestSnapshot> {
        match self.store.newest_event(&r.thread_id) {
            Ok(Some(row)) => Hydration::Complete(vec![NewestSnapshot {
                body: crate::shapes::body::body_text_of_payload(&row.kind, &row.payload_json)
                    .map(|text| {
                        text.and_then(|t| {
                            crate::shapes::body::summary(&r.thread_id, row.seq, &t)
                        })
                    }),
                newest: NewestRow {
                    seq: row.seq,
                    message_id: row.message_id,
                    kind: row.kind,
                    ts: row.ts,
                },
            }]),
            Ok(None) => Hydration::Empty {
                reason: "this thread has no events",
            },
            Err(e) => Hydration::Partial {
                got: Vec::new(),
                reason: format!("{e}"),
            },
        }
    }
}

impl Handles<ThreadMessageHead> for ThreadManager {
    /// ONE `MAX(message_id)` on `idx_events_thread_msg`. `0` means nothing
    /// addressable was ever written, and the shape turns that into `None`.
    fn hydrate(&self, r: ThreadMessageHead) -> Hydration<u64> {
        match self.store.max_message_id(&r.thread_id) {
            Ok(n) => Hydration::Complete(vec![n]),
            Err(e) => Hydration::Partial {
                got: Vec::new(),
                reason: format!("{e}"),
            },
        }
    }
}

impl Handles<ThreadAppState> for ThreadManager {
    /// ONE statement for every application event type this thread has ever
    /// carried — `newest_custom_events`, whose group-by is served by
    /// `idx_events_thread_kind` and whose result is bounded by the app's
    /// vocabulary (7 types in the production store).
    ///
    /// `Empty` for a thread the application has written nothing about. 745 of
    /// the store's 924 custom events are on 179 threads, so "no app state" is
    /// the ordinary case and it must render as empty, not as `?`.
    fn hydrate(&self, r: ThreadAppState) -> Hydration<AppEntry> {
        match self.store.newest_custom_events(&r.thread_id) {
            Ok(v) if v.is_empty() => Hydration::Empty {
                reason: "no application event has been written to this thread",
            },
            Ok(v) => Hydration::Complete(
                v.into_iter()
                    .map(|(event_type, data)| AppEntry { event_type, data })
                    .collect(),
            ),
            Err(e) => Hydration::Partial {
                got: Vec::new(),
                reason: format!("{e}"),
            },
        }
    }
}

impl Handles<ThreadLiveness> for ThreadManager {
    /// **NO STATEMENT AT ALL — one mutex.** The beat has no durable store: it is
    /// read out of the same `ThreadState::live` that `emit_liveness` broadcasts
    /// from, so the read path and the live path cannot drift.
    ///
    /// A thread this process has not adopted answers `Complete` with the default
    /// row rather than `Empty`, because "no in-memory state" is not an absence of
    /// information: a thread with no state in this process has no turn running in
    /// this process, and `busy: false` is that fact.
    fn hydrate(&self, r: ThreadLiveness) -> Hydration<LivenessRow> {
        Hydration::Complete(vec![self.liveness_row(&r.thread_id)])
    }
}

impl Handles<ThreadParent> for ThreadManager {
    /// **THE SAME FACT `agents/list` READS** — `ThreadMeta.parent_id` in the
    /// thread's managed ledger (`Managed::parent_of` is that read with its
    /// error swallowed; this keeps the error, so an unreadable ledger renders
    /// `?` rather than a root). No ledger at all is a thread that was never
    /// spawned as a child: a COMPLETE `None`.
    fn hydrate(&self, r: ThreadParent) -> Hydration<Option<String>> {
        match self.managed().load(&r.thread_id) {
            Ok(Some(l)) => Hydration::Complete(vec![l.meta().parent_id().map(str::to_string)]),
            Ok(None) => Hydration::Complete(vec![None]),
            Err(e) => Hydration::Partial {
                got: Vec::new(),
                reason: format!("{e}"),
            },
        }
    }
}

impl Handles<ThreadChildren> for ThreadManager {
    /// The thread's managed children, through [`children_row`] — the SAME
    /// builder `Fact::ThreadChildren` is built with. No ledger is a thread that
    /// never spawned: a COMPLETE empty value. An unreadable ledger is `?`.
    fn hydrate(&self, r: ThreadChildren) -> Hydration<ChildrenRow> {
        match self.managed().load(&r.thread_id) {
            Ok(Some(l)) => Hydration::Complete(vec![children_row_at(&l, &self.store)]),
            Ok(None) => Hydration::Complete(vec![ChildrenRow::default()]),
            Err(e) => Hydration::Partial {
                got: Vec::new(),
                reason: format!("{e}"),
            },
        }
    }
}

impl Handles<ThreadRolledRead> for ThreadManager {
    /// The thread's own subtree, through [`rolled_rows`] — the SAME builder
    /// `Fact::ThreadRolled` is built with. A thread with no managed ledger is a
    /// leaf, which is a complete answer. A store or ledger failure is `?`.
    fn hydrate(&self, r: ThreadRolledRead) -> Hydration<RolledRow> {
        match rolled_rows(self, &r.thread_id, std::slice::from_ref(&r.thread_id)) {
            Ok(mut rows) if !rows.is_empty() => Hydration::Complete(vec![rows.remove(0).1]),
            Ok(_) => Hydration::Partial {
                got: Vec::new(),
                reason: "the thread is not in its own subtree".into(),
            },
            Err(e) => Hydration::Partial {
                got: Vec::new(),
                reason: format!("{e}"),
            },
        }
    }
}

impl Handles<ThreadSubagentsRead> for ThreadManager {
    /// **NO STATEMENT — one mutex**, exactly like `ThreadLiveness`: read out of
    /// the same in-memory roster `Fact::ThreadSubagents` is built from. A
    /// thread this process has not adopted has launched no subagent in this
    /// process: a COMPLETE empty map.
    fn hydrate(&self, r: ThreadSubagentsRead) -> Hydration<std::collections::BTreeMap<String, SubagentRow>> {
        Hydration::Complete(vec![self.subagent_rows(&r.thread_id)])
    }
}

impl ThreadManager {
    /// **RE-STATE THE ROLLED STATUS OF `thread_id` AND EVERY ANCESTOR**
    /// (R9.TS1) — `Fact::ThreadRolled`, one per thread on the chain, all off
    /// ONE walk of the topmost ancestor's subtree and ONE state derivation.
    ///
    /// Called where a subtree answer can move: every status edge
    /// (`emit_status_answering`) and every children move (`emit_children_fact`).
    /// It ALSO re-states `thread_id`'s direct children: a child's report lands
    /// on its PARENT's ledger (that is the children move), and the outcome it
    /// carries is the child's OWN error state (R9.TS1 inspect F1).
    /// Gated on the `threads` shape tracking, so an unwatched daemon pays
    /// nothing; the fold drops a level that did not change.
    pub fn emit_rolled(&self, thread_id: &str) {
        if !self.hub.tracked(Threads::NAME) {
            return;
        }
        let at = crate::clock::now();
        let mg = self.managed();
        let mut chain = vec![thread_id.to_string()];
        chain.extend(shapewire::subtree::ancestors(thread_id, |c| mg.parent_of(c)));
        let top = chain.last().cloned().unwrap_or_else(|| thread_id.to_string());
        let mut ids = chain.clone();
        for kid in mg.children_of(thread_id) {
            if !ids.contains(&kid) {
                ids.push(kid);
            }
        }
        match rolled_rows(self, &top, &ids) {
            Ok(rows) => {
                for (id, row) in rows {
                    self.hub.emit_fact_at(at, Fact::ThreadRolled { thread_id: id, row });
                }
            }
            Err(e) => {
                tracing::warn!(thread = %thread_id, error = %e, "rolled status: the subtree could not be read");
            }
        }
    }
}

impl Handles<ThreadQueueRows> for ThreadManager {
    /// Two statements on `idx_queue_thread_state`, which is `(thread_id, state)`
    /// — so each is an index seek on this thread's rows in that state, not a
    /// scan of the queue.
    ///
    /// **`Empty` for a thread with nothing undelivered, and that is the ordinary
    /// case.** An empty queue is a COMPLETE answer that renders as an empty
    /// panel; only a failed read may render `?`. Getting that backwards is the
    /// defect `docs/shapes.md` 4.3 names, and `queued` is zero on almost every
    /// thread almost always.
    fn hydrate(&self, r: ThreadQueueRows) -> Hydration<QueueEntry> {
        match self.thread_queue(&r.thread_id) {
            Ok(v) if v.is_empty() => Hydration::Empty {
                reason: "this thread holds no waiting or failed queue row",
            },
            Ok(v) => Hydration::Complete(v),
            Err(e) => Hydration::Partial {
                got: Vec::new(),
                reason: format!("{e}"),
            },
        }
    }
}
