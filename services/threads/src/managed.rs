//! # The managed subagent runtime — `ledger::spawn` wired into the daemon (R4.S2)
//!
//! `docs/managed-subagents.md` §1 is the model and `ledger/spawn.rs` is its
//! four pure functions. This file is the daemon half: it gives those functions
//! a DURABLE ledger to fold over, turns a `Spawned::Created` into a thread the
//! daemon owns with a turn queued in it, turns a child's ended run into
//! `report`, turns a `Wake` into one ordinary turn on the parent, and — the
//! release sentence — finishes every one of those acts after a daemon death.
//!
//! ## Where the ledgers live, and the commit point
//!
//! One JSONL file per managed thread under `<data_dir>/ledgers/`, in the file
//! form `ledger::Ledger` already defines (line 1 the meta, then one event per
//! line). Only threads that took part in a spawn have one: a parent gets its
//! file at its first spawn (as a root), a child at birth. Every write is
//! `fsync`ed before the next act starts, so a kill at any instant leaves a
//! prefix of the sequence below on disk and nothing else:
//!
//! 1. the child's ledger (meta naming its parent + one `user` event: the task)
//!    — written whole to a temporary name and renamed, so it is all or nothing;
//! 2. **THE COMMIT POINT:** `child{started}` appended to the parent's ledger
//!    under the spawn's `request_id`;
//! 3. the child's `threads` row (settings copied from the parent's, budget key
//!    included — `managed-subagents.md` §1.4);
//! 4. the child's turn: `send` — a `user_message` event and a `queue` row,
//!    which is the durable turn every restart already re-dispatches.
//!
//! A kill before (2) leaves an orphan child ledger and no thread: the spawn did
//! not happen, and a retry with the same `request_id` does it. A kill after (2)
//! is finished by [`recover`] at the next boot, which reads the parent's
//! ledger, finds the `child{started}`, and performs (3) and (4) idempotently
//! from the CHILD'S OWN LEDGER — the task text is read back from it, not from
//! anything in memory. That is "the child thread resumes from its ledger".
//! From (4) on, the child is an ordinary thread: its turn survives a restart by
//! the existing durable-turn contract (`running_turns`, re-adoption).
//!
//! ## How the parent waits — P1, the `af77` reading
//!
//! The parent ends its turn. Nothing polls. When a child's run ends
//! (`ThreadManager::finish_turn` calls [`turn_finished`] with the turn's own
//! [`TurnTerminal`] — H179, `docs/SUBAGENT-WAKE.md`), the child's completion
//! record is written to its own ledger and [`report`] appends
//! `child{finished}` and the child's words to the parent's ledger, and ONLY
//! when that empties the parent's outstanding set (a fold over its ledger, not
//! a column and not an in-memory registry) is the parent woken: one queued
//! turn carrying every report since the last wake, plus
//! `placement_wake("child_reported")`. A parent over five children is woken
//! once, by the fifth.
//!
//! That replaces the in-memory `Liveness::busy()` poll
//! (`poll_subagents_until_idle` / `sweep_subagents`, deleted in the same
//! change): "my turn is not over until my children are done" is now a durable
//! derivation that survives a restart.
//!
//! ## The operator's questions, at their recommended readings (acted on)
//!
//! * **Q1 (how a parent waits)** — the parent ends its turn and is woken by the
//!   last report (`wake: end`, the default). `agents_gather`'s bounded block,
//!   the design's (c), is [`gather`] (R4.S8): a read that waits at most
//!   `poll_s`, woken by every turn end, and answers `{satisfied: false,
//!   pending}` when the interval runs out first.
//! * **Q2 (`immediate`)** — latency of routing: the wake is queued
//!   `SendIntent::Immediate` and starts at the parent's next turn boundary.
//! * **Q4 / P3 (an infrastructure failure)** — refused to the caller, typed,
//!   when it happens BEFORE the commit point (no parent thread, depth cap, a
//!   corrupt store, a parameter the daemon cannot honour). AFTER the commit
//!   point a failure is never refused: the child is a thread with a queued
//!   turn, so admission parks it (CON-4, park never drop) and recovery
//!   finishes it.
//! * **Q5 / P4 (what a child's ledger holds)** — a child is exactly its parent:
//!   its events go to `events` in full like any thread's, and `threads/log`
//!   already puts large bodies behind the resource door. The JSONL ledger here
//!   carries STRUCTURE only — the task, `child` edges, reports, wake markers.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use shapewire::command::{Accepted, Visible};
use shapewire::envelope::codes;

use crate::error::{Error, Result};
use crate::event::{SendIntent, ThreadEvent};
use crate::ledger::claude_code::RUNTIME;
use crate::ledger::spawn::{self as lspawn, Outcome, SpawnRefused, SpawnRequest, Spawned};
use crate::ledger::{Draft, Kind, Ledger, ThreadMeta};
use crate::runtime::ThreadManager;
use crate::settings::ThreadSettings;

/// The directory under the data dir that holds the managed ledgers.
pub const LEDGER_DIR: &str = "ledgers";

/// The `system` fact recording that a wake was queued on the parent.
const WAKE_QUEUED: &str = "wake_queued";

/// The `system` fact recording that the operator cancelled this thread while
/// it was owed `children`: the wake that any of THOSE children's reports
/// would queue is suppressed (R9.N1, gap N-1). `{fact, children, reason}`.
/// An existing event kind (`system`), like `run_ended` and `wake_queued` —
/// no ledger storage change.
const WAKE_SUPPRESSED: &str = "wake_suppressed";

/// **WHERE A TEST MAY STOP A SPAWN, AS A KILL WOULD.** Each stage is the
/// instant just after one durable write: halting there leaves on disk exactly
/// what a `SIGKILL` at that instant leaves, because every write before it was
/// `fsync`ed and nothing after it happened. Never set outside a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Halt {
    /// After (1): the child's ledger exists, the parent's does not name it.
    AfterChildLedger,
    /// After (2), the commit point: the parent's ledger names the child; no
    /// thread row, no turn.
    AfterCommit,
    /// After (3): the child's thread row exists; its turn was never queued.
    AfterThreadRow,
    /// Not a spawn stage: the next managed child whose run ends stops before
    /// its completion record is written — a death between the child's
    /// `turn_ended` and its report (H179). Boot reconciliation must finish it.
    BeforeCompletion,
    /// Not a spawn stage (R9.N3): the next wake [`deliver_due_wakes`] would
    /// post FAILS before the post — the network delivery errored, nothing
    /// reached the parent's store. The live retry must finish it.
    BeforeWakePost,
    /// Not a spawn stage (R9.N3): the next wake's post LANDS (the parent's
    /// `user_message` is in the store) and then delivery fails before the
    /// `wake_queued` marker is written — a half-landed wake. The retry must
    /// find it by `wake_id` and only mark it.
    AfterWakePost,
    /// Not a spawn stage (R11.J4): the next corrective turn an `unmet` answer
    /// is owed stops before its message is recorded — a death between the
    /// `structured_output` fact and the retry. The next life must send it.
    BeforeSchemaRetry,
    /// Not a spawn stage (R10.W1b): the next reply for a later run
    /// ([`reply`]) FAILS before its post — nothing reached the parent's store.
    /// The live retry, or the next life's boot, must post it exactly once.
    BeforeReplyPost,
    /// Not a spawn stage (R10.H1, F1): the next managed child whose run ends
    /// drops its completion AND the live retry is not told — the in-memory
    /// reminder lost, as a daemon whose retry task died would lose it. Only
    /// the periodic [`sweep`] (or the next boot) can finish that child.
    LostCompletion,
    /// Not a spawn stage (R10.H1 inspect F-2): the next managed child whose
    /// run ends writes its completion record (`run_ended`) and then loses the
    /// report — not remembered. The child is left unfinished WITH a record,
    /// which is the case only `reconcile`'s idle read keeps a re-tasked,
    /// running child from being closed.
    LostReport,
}

/// **THE LIVE RETRY'S BACKOFF** (R9.N3, gap N-3): the first retry runs this
/// long after the failure, each further one twice as long, never longer than
/// [`RETRY_CAP`]. A parent owed a failed delivery waits at most about one
/// rung — not until the next daemon restart.
pub const RETRY_BASE: std::time::Duration = std::time::Duration::from_millis(500);
/// The longest wait between two retries of owed deliveries.
pub const RETRY_CAP: std::time::Duration = std::time::Duration::from_secs(30);

/// **THE RECONCILIATION SWEEP'S PERIOD WHILE ANY CHILD IS UNFINISHED**
/// (R10.H1, F1; an internal report). A child whose end was missed by
/// both the live path and the R9.N3 retry (a lost in-memory reminder, a
/// panic, a write that raced a restart) is recorded finished — and its parent
/// woken, for `wake: end` — within about one of these, not at the next boot.
pub const SWEEP_OWED: std::time::Duration = std::time::Duration::from_secs(60);
/// The sweep's period when the last pass found no unfinished child at all.
pub const SWEEP_IDLE: std::time::Duration = std::time::Duration::from_secs(600);
/// **THE SWEEP LEAVES A RUN THAT ENDED LESS THAN THIS AGO TO THE LIVE PATH**
/// (R10.H1 inspect F-1). Between a child's turn lock being released (it reads
/// idle) and its epilogue (`turn_finished`: the structured verdict, a
/// corrective turn, the record and the report) there is a window in which the
/// run is over in the store but not yet handled. A sweep that closed it there
/// would report the run from the store AND let the epilogue send a corrective
/// turn whose end becomes a second message to the parent. The epilogue takes
/// milliseconds; a run still unhandled this long after its terminal event was
/// genuinely missed.
pub const SWEEP_SETTLE: std::time::Duration = std::time::Duration::from_secs(30);

/// **DELIVERIES THAT FAILED WHILE RUNNING, OWED A RETRY** (R9.N3). In memory
/// on purpose: the durable state (the ledgers, the store) is what the retry
/// re-reads, and a restart loses only the reminder — boot [`recover`] scans
/// every ledger anyway and stays the backstop.
#[derive(Default)]
struct Owed {
    /// Children whose completion did not reach their parent: re-run
    /// [`reconcile`], then their parent's due wakes.
    children: std::collections::BTreeSet<String>,
    /// Parents whose due wake did not land: re-run [`deliver_due_wakes`].
    parents: std::collections::BTreeSet<String>,
    /// A retry task is scheduled (at most one per daemon).
    running: bool,
    /// Retry passes run so far — read by tests as the positive sentinel that
    /// the retry DID run when the asserted outcome is "nothing woke".
    passes: u64,
}

/// The per-daemon handle: the ledger directory and the one lock every
/// read-fold-append of a managed ledger is taken under.
pub struct Managed {
    dir: PathBuf,
    /// Shared with every projected turn (`ledger::turn::ProjectedTurn`),
    /// which writes the same `ledgers/<thread>.jsonl` — see [`Managed::write_lock`].
    write: Arc<Mutex<()>>,
    halt: Mutex<Option<Halt>>,
    /// **EVERY TURN END, AS A WAKE-UP** (R4.S8). [`turn_finished`] pings it;
    /// a waiting [`gather`] re-evaluates its predicate on the ping instead of
    /// on a timer, so a run that ends mid-wait is answered at once.
    turn_ended: Arc<tokio::sync::Notify>,
    /// When each still-unsatisfied gather (caller, predicate, runs) was first
    /// asked, so `timeout_s` counts ACROSS polls. In memory on purpose: a
    /// daemon restart resets it, which costs at most a longer wait, never a
    /// lost one (the predicate is re-read from durable state on every call).
    gathers: Mutex<std::collections::HashMap<String, std::time::Instant>>,
    /// Failed deliveries owed a live retry — see [`Owed`], [`retry_owed`].
    owed: Mutex<Owed>,
    /// `(first wait, cap)` of the retry backoff; [`RETRY_BASE`] / [`RETRY_CAP`]
    /// unless a test shrank them.
    backoff: Mutex<(std::time::Duration, std::time::Duration)>,
    /// `(while owed, while idle)` periods of the reconciliation [`sweep`];
    /// [`SWEEP_OWED`] / [`SWEEP_IDLE`] unless a test shrank them (R10.H1).
    sweep_period: Mutex<(std::time::Duration, std::time::Duration)>,
    /// Pinged when a test changes `sweep_period`, so the waiting sweep task
    /// re-reads it instead of sleeping out the old (10 min) period.
    sweep_kick: Arc<tokio::sync::Notify>,
    /// `(task started, passes run, unfinished children the last pass saw)`.
    sweep_state: Mutex<(bool, u64, usize)>,
    /// [`SWEEP_SETTLE`] unless a test shrank it.
    sweep_settle: Mutex<std::time::Duration>,
    /// **ONE CLOSER AT A TIME** (R10.H1 inspect F-1): a child's live epilogue
    /// ([`turn_finished`]) and the sweep's [`reconcile`] of a child are
    /// serialized, so whichever runs second re-reads what the first wrote
    /// (the record, the finish, a queued corrective) and does nothing twice.
    close: Mutex<()>,
    /// TEST ONLY: `(epilogues still to pause, epilogues paused now, released)`.
    pause: Mutex<(usize, usize, bool)>,
}

impl Managed {
    pub fn new(data_dir: &Path) -> Self {
        Managed {
            dir: data_dir.join(LEDGER_DIR),
            write: Arc::new(Mutex::new(())),
            halt: Mutex::new(None),
            turn_ended: Arc::new(tokio::sync::Notify::new()),
            gathers: Mutex::new(std::collections::HashMap::new()),
            owed: Mutex::new(Owed::default()),
            backoff: Mutex::new((RETRY_BASE, RETRY_CAP)),
            sweep_period: Mutex::new((SWEEP_OWED, SWEEP_IDLE)),
            sweep_kick: Arc::new(tokio::sync::Notify::new()),
            sweep_state: Mutex::new((false, 0, 0)),
            sweep_settle: Mutex::new(SWEEP_SETTLE),
            close: Mutex::new(()),
            pause: Mutex::new((0, 0, false)),
        }
    }

    /// **TEST ONLY.** Shrink (or restore) [`SWEEP_SETTLE`].
    #[doc(hidden)]
    pub fn set_sweep_settle(&self, settle: std::time::Duration) {
        *self.sweep_settle.lock().unwrap() = settle;
    }

    /// **TEST ONLY.** The next `n` turn epilogues stop at their
    /// very start — the window between the turn lock's release and
    /// `turn_finished` — until [`Managed::release_epilogues`] (20 s ceiling).
    #[doc(hidden)]
    pub fn pause_epilogues(&self, n: usize) {
        *self.pause.lock().unwrap() = (n, 0, false);
    }

    /// **TEST ONLY.** How many epilogues are paused right now.
    #[doc(hidden)]
    pub fn epilogues_paused(&self) -> usize {
        self.pause.lock().unwrap().1
    }

    /// **TEST ONLY.** Let every paused epilogue go on.
    #[doc(hidden)]
    pub fn release_epilogues(&self) {
        self.pause.lock().unwrap().2 = true;
    }

    fn maybe_pause(&self, id: &str) {
        {
            let mut p = self.pause.lock().unwrap();
            // Any thread's epilogue: the tests that arm this run no other
            // turn in the window (a ledger read here could race the turn's
            // own projected writes, so the child is not looked up).
            let _ = id;
            if p.0 == 0 {
                return;
            }
            p.0 -= 1;
            p.1 += 1;
        }
        let wait = || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while !self.pause.lock().unwrap().2 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        // Called on a runtime worker: hand its queued tasks off first, or a
        // task parked in this worker's LIFO slot (another child's turn) stalls
        // behind the pause.
        match tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()) {
            Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(wait),
            _ => wait(),
        }
        self.pause.lock().unwrap().1 -= 1;
    }

    /// **TEST ONLY.** Shrink (or restore) the reconciliation sweep's periods
    /// (doctrine 4: the negative "nothing closed it yet" outlasts the real
    /// period only if the period is injectable). The waiting sweep task
    /// restarts its wait with the new period; no pass runs early.
    #[doc(hidden)]
    pub fn set_sweep_period(&self, owed: std::time::Duration, idle: std::time::Duration) {
        *self.sweep_period.lock().unwrap() = (owed, idle);
        self.sweep_kick.notify_waiters();
    }

    /// How many reconciliation sweep passes have run in this daemon's life.
    pub fn sweep_passes(&self) -> u64 {
        self.sweep_state.lock().unwrap().1
    }

    /// **TEST ONLY.** Shrink (or restore) the live retry's backoff.
    #[doc(hidden)]
    pub fn set_retry_backoff(&self, base: std::time::Duration, cap: std::time::Duration) {
        *self.backoff.lock().unwrap() = (base, cap);
    }

    /// How many deliveries are owed a live retry right now (children + parents).
    pub fn retries_owed(&self) -> usize {
        let o = self.owed.lock().unwrap();
        o.children.len() + o.parents.len()
    }

    /// How many live retry passes have run in this daemon's life.
    pub fn retry_passes(&self) -> u64 {
        self.owed.lock().unwrap().passes
    }

    /// The retry task's exit test, under the same lock [`remember`] takes, so
    /// an item remembered while the last pass ran is never stranded: either
    /// this sees it and the task keeps going, or the task has cleared
    /// `running` and `remember` starts a new one.
    fn retry_done(&self) -> bool {
        let mut o = self.owed.lock().unwrap();
        if o.children.is_empty() && o.parents.is_empty() {
            o.running = false;
            return true;
        }
        false
    }

    /// The ledger directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// **THE ONE WRITE LOCK FOR `ledgers/`.** A projected turn
    /// (`ledger::turn::ProjectedTurn`) writes the same file a spawn, report
    /// or wake appends to — a parent's spawn is a tool call made DURING its
    /// turn — so it takes this lock for each write and re-reads the tail when
    /// the file moved under it.
    pub fn write_lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.write)
    }

    /// **TEST ONLY.** The next spawn stops after `stage`, as a kill would.
    #[doc(hidden)]
    pub fn halt_after(&self, stage: Option<Halt>) {
        *self.halt.lock().unwrap() = stage;
    }

    /// **TEST ONLY.** The stage still armed, if the next act has not reached it.
    #[doc(hidden)]
    pub fn halt_armed(&self) -> Option<Halt> {
        *self.halt.lock().unwrap()
    }

    fn halted(&self, stage: Halt) -> Result<()> {
        let mut h = self.halt.lock().unwrap();
        if *h == Some(stage) {
            *h = None;
            return Err(Error::Config(format!("spawn halted after {stage:?} (test kill point)")));
        }
        Ok(())
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.jsonl"))
    }

    /// Read a thread's ledger, or `None` when it took part in no spawn.
    ///
    /// A kill mid-append can leave a final line with no newline; that line is
    /// an append that did not happen, so it is cut before the read rather than
    /// failing the whole ledger. Any OTHER malformation is refused by name.
    pub fn load(&self, id: &str) -> Result<Option<Ledger>> {
        let p = self.path(id);
        if !p.exists() {
            return Ok(None);
        }
        crate::ledger::repair_torn_tail(&p)?;
        Ledger::read_jsonl(&p)
            .map(Some)
            .map_err(|e| Error::Config(format!("managed ledger {}: {e:?}", p.display())))
    }

    /// Write a whole ledger, atomically: a temporary file, `fsync`, rename.
    fn create(&self, l: &Ledger) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let p = self.path(l.thread_id());
        let tmp = self.dir.join(format!("{}.jsonl.tmp", l.thread_id()));
        l.write_jsonl(&tmp)?;
        std::fs::File::open(&tmp)?.sync_all()?;
        std::fs::rename(&tmp, &p)?;
        sync_dir(&self.dir);
        Ok(())
    }

    /// Append the ledger's last `n` events to its file, in one write, `fsync`ed.
    fn append_last(&self, l: &Ledger, n: usize) -> Result<()> {
        let p = self.path(l.thread_id());
        if !p.exists() {
            return self.create(l);
        }
        let evs = l.events();
        let mut buf = Vec::new();
        for e in &evs[evs.len() - n..] {
            serde_json::to_writer(&mut buf, e)?;
            buf.push(b'\n');
        }
        let mut f = std::fs::OpenOptions::new().append(true).open(&p)?;
        f.write_all(&buf)?;
        f.sync_data()?;
        Ok(())
    }

    /// Every ledger in the directory, by thread id.
    fn all(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut ids: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.strip_suffix(".jsonl").map(str::to_string)
            })
            .collect();
        ids.sort();
        ids
    }

    fn meta_of(&self, id: &str) -> Option<ThreadMeta> {
        self.load(id).ok().flatten().map(|l| l.meta().clone())
    }

    /// `id`'s parent: `ThreadMeta.parent_id`, the ONE owner of lineage
    /// (`docs/agent-networks.md` §1.1; `crate::networks` reads it, never
    /// writes it). `None` for a root or a thread that took part in no spawn.
    pub fn parent_of(&self, id: &str) -> Option<String> {
        self.meta_of(id).and_then(|m| m.parent_id().map(str::to_string))
    }

    /// `id`'s immediate children: the COMMITTED spawns, i.e. the
    /// `child{started}` events on its own ledger (the commit point). A child
    /// ledger written before a kill at the commit point is not a child.
    pub fn children_of(&self, id: &str) -> Vec<String> {
        self.load(id)
            .ok()
            .flatten()
            .map(|l| {
                l.events()
                    .iter()
                    .filter(|e| e.kind == Kind::Child && e.body["phase"] == "started" && e.within.is_none())
                    .filter_map(|e| e.body["child_id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every `(child, parent)` pair on disk — each ledger's meta read once.
    /// `threads/non-idle`'s hydrate seeds its lineage map from it; lineage is
    /// immutable, so later spawns reach that shape through `Fact::ThreadChildren`.
    pub fn lineage(&self) -> Vec<(String, String)> {
        self.all()
            .into_iter()
            .filter_map(|id| self.parent_of(&id).map(|p| (id, p)))
            .collect()
    }

    /// Is `agent` the caller itself or one of its descendants? A walk up the
    /// child's `parent_id` chain, cycle-safe through [`ThreadMeta::depth`]'s
    /// own guard.
    pub fn is_descendant_or_self(&self, caller: &str, agent: &str) -> bool {
        if caller == agent {
            return true;
        }
        let mut seen = std::collections::HashSet::new();
        let mut cur = self.meta_of(agent).and_then(|m| m.parent_id().map(str::to_string));
        while let Some(p) = cur {
            if p == caller {
                return true;
            }
            if !seen.insert(p.clone()) {
                return false;
            }
            cur = self.meta_of(&p).and_then(|m| m.parent_id().map(str::to_string));
        }
        false
    }

    /// The parent is owed a child — the durable fold that replaces
    /// `Liveness::busy()` on the turn-end path.
    pub fn is_waiting(&self, id: &str) -> bool {
        self.load(id)
            .ok()
            .flatten()
            .map(|l| lspawn::is_waiting(&l))
            .unwrap_or(false)
    }
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

fn refused(code: &'static str, message: impl Into<String>) -> Error {
    Error::Refused {
        code,
        message: message.into(),
    }
}

fn opt_str(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Parameters on the §2.2 surface the daemon cannot honour yet. Refused, never
/// ignored: a parameter the caller meant and the daemon dropped is the silent
/// divergence the surface exists to delete.
///
/// `agent`, `effort`, `networks` and `network` left this list at R4.S8: each is
/// checked by [`spawn_params`] before the commit point and honoured after it.
/// `json_schema` left it at R11.J3: checked by [`check_json_schema`] before the
/// commit point, carried on the task message, and passed as `--json-schema` on
/// the turn that delivers it ([`turn_json_schema`]).
const NOT_YET: &[(&str, &str)] = &[
    ("max_thinking_tokens", "no thinking budget exists on `ThreadSettings`"),
];

// ═════════════════════════════════════════════════════════════════════════════
// STRUCTURED OUTPUT, PHASE A (R11.J3, `docs/STRUCTURED-OUTPUT.md` §3.1, §3.2)
// ═════════════════════════════════════════════════════════════════════════════

/// A `json_schema` that is not a JSON Schema 2020-12 document (§3.1).
pub const BAD_SCHEMA: &str = "bad_schema";
/// A `json_schema` above [`MAX_SCHEMA_BYTES`] serialized (§3.1).
pub const SCHEMA_TOO_LARGE: &str = "schema_too_large";
/// The ceiling on a schema, serialized compact: 16 KiB (§3.1). It rides argv
/// and the task message's metadata, so it is bounded.
pub const MAX_SCHEMA_BYTES: usize = 16 * 1024;
/// The daemon's own key in a message's metadata: `meta["cauldron.threads"]`.
/// The schema is recorded there, immutably, so the ledger shows what was asked
/// (§3.1). Not a declared path to the model (`crate::context`), so it never
/// reaches the prompt.
pub const META_KEY: &str = "cauldron.threads";

/// **A SCHEMA IS CHECKED BEFORE THE COMMIT POINT.** Returns the compact
/// serialization that rides argv. Refused `bad_schema` when the value is not
/// an object, fails the 2020-12 meta-schema, or does not compile (an external
/// `$ref` the daemon will not fetch, an invalid `pattern`); `schema_too_large`
/// above [`MAX_SCHEMA_BYTES`]. Size first: it is the cheap check, and an
/// oversized document is never handed to the validator.
pub fn check_json_schema(schema: &Value) -> Result<String> {
    let bad = |why: String| {
        refused(BAD_SCHEMA, format!("bad_schema: `json_schema` {why}. Refused rather than ignored; nothing was spawned"))
    };
    if !schema.is_object() {
        return Err(bad(format!("must be a JSON Schema object, got {schema}")));
    }
    let text = schema.to_string();
    if text.len() > MAX_SCHEMA_BYTES {
        return Err(refused(
            SCHEMA_TOO_LARGE,
            format!(
                "schema_too_large: `json_schema` is {} bytes serialized; the ceiling is {MAX_SCHEMA_BYTES}. Refused rather than ignored; nothing was spawned",
                text.len()
            ),
        ));
    }
    if let Err(e) = jsonschema::draft202012::meta::validate(schema) {
        return Err(bad(format!("is not a JSON Schema 2020-12 document: {e} (at `{}`)", e.instance_path())));
    }
    if let Err(e) = jsonschema::draft202012::new(schema) {
        return Err(bad(format!("does not compile: {e}")));
    }
    Ok(text)
}

/// **THE BATCH-LEVEL DEFAULT (§3.1).** `agents_spawn_batch` may name one
/// `json_schema` for the whole batch; an item that names none inherits it, and
/// an item's own schema wins. Returns the item's spawn parameters with the
/// default applied. (`agents_spawn_batch` itself is still refused
/// `not_available_yet` on this tree; this is the rule it spawns each item by.)
pub fn batch_item_params(batch: &Value, item: &Value) -> Value {
    let mut out = item.clone();
    let own = item.get("json_schema").is_some_and(|v| !v.is_null());
    if let (false, Some(d)) = (own, batch.get("json_schema").filter(|v| !v.is_null())) {
        out["json_schema"] = d.clone();
    }
    out
}

/// **THE SCHEMA OF THE TURN THAT DELIVERS `message_ids`, IF ANY (§3.2).**
/// `--json-schema` is a per-process flag and a turn is one process, so the
/// schema rides the turn whose prompt carries the schema-bearing message, and
/// only that turn: the next turn's messages carry none, and it gets none. Read
/// from the delivered `user_message` events' metadata — the same record a
/// retry or a re-adopted delivery reads, so every attempt of the turn gets it.
/// Phase A has one schema-bearing message per child (the spawn's task); if a
/// batch ever held two, the first wins here and §3.2's batch boundary (R11.J6)
/// is what keeps that from happening.
pub fn turn_json_schema(m: &ThreadManager, thread_id: &str, message_ids: &[u64]) -> Option<String> {
    if message_ids.is_empty() {
        return None;
    }
    let kinds = ["user_message".to_string()];
    let q = crate::store::MessageQuery {
        ids: Some(message_ids),
        from: None,
        to: None,
        tail: None,
        kinds: Some(&kinds),
        cursor: None,
        limit: message_ids.len(),
    };
    let page = match m.store.query_messages(thread_id, &q) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(thread = %thread_id, error = %e, "reading the turn's messages for a json_schema failed; the turn runs without one");
            return None;
        }
    };
    page.events.iter().find_map(|e| match &e.event {
        ThreadEvent::UserMessage { metadata, .. } => metadata
            .get(META_KEY)
            .and_then(|t| t.get("json_schema"))
            .filter(|s| s.is_object())
            .map(Value::to_string),
        _ => None,
    })
}

// ═════════════════════════════════════════════════════════════════════════════
// THE CORRECTIVE TURN (R11.J4, `docs/STRUCTURED-OUTPUT.md` §3.3)
// ═════════════════════════════════════════════════════════════════════════════
//
// A schema-bearing turn whose answer the daemon judged `unmet` gets ONE more
// turn by default (Q-2's recommendation, acted on per R11-shared): a message
// from the daemon to the same thread carrying the validation errors and the
// SAME schema, so `turn_json_schema` puts `--json-schema` on that turn too.
// Its id is deterministic, `schema-retry:<message>:<n>`, and the decision is
// read from durable state alone — the newest `user_message` and the
// `structured_output` facts after it — so a daemon that dies anywhere around
// it decides the same thing on its next life, and never sends a second one:
// once the corrective message is recorded it IS the newest message, and no
// fact follows it until its own turn has run. The child's completion record
// waits, because a queued corrective message leaves the child not idle.
// An `unmet` never changes the run's outcome or anyone's routing (D3).

/// The corrective turns an `unmet` answer gets when the asker names none
/// (Q-2: "one by default; the asker may set 0–3").
pub const SCHEMA_RETRIES_DEFAULT: u64 = 1;
/// The most corrective turns an asker may ask for (§3.3, Option 3.3).
pub const SCHEMA_RETRIES_MAX: u64 = 3;
/// The daemon's verdict on one schema-bearing turn (§3.4): R11.J2's event,
/// written at the turn's end by [`record_verdict`].
pub use crate::event::STRUCTURED_OUTPUT_EVENT;

/// `schema_retries` on a spawn: an integer `0..=SCHEMA_RETRIES_MAX`, and only
/// beside a `json_schema` — it counts corrective turns for that schema's
/// answer, so alone it would be a parameter the daemon silently ignores.
/// Checked before the commit point, like the schema itself.
fn check_schema_retries(params: &Value) -> Result<Option<u64>> {
    let Some(v) = params.get("schema_retries").filter(|v| !v.is_null()) else { return Ok(None) };
    let n = v.as_u64().filter(|n| *n <= SCHEMA_RETRIES_MAX).ok_or_else(|| {
        refused(
            codes::MALFORMED,
            format!("`agents/spawn` `schema_retries` must be an integer 0..={SCHEMA_RETRIES_MAX}, got {v}. Nothing was spawned"),
        )
    })?;
    if params.get("json_schema").map_or(true, Value::is_null) {
        return Err(refused(
            codes::MALFORMED,
            "`agents/spawn` `schema_retries` needs `json_schema`: it counts corrective turns for a schema's answer. Refused rather than ignored; nothing was spawned",
        ));
    }
    Ok(Some(n))
}

/// The deterministic id of the `n`th corrective turn for message `root`.
pub fn schema_retry_id(root: u64, n: u64) -> String {
    format!("schema-retry:{root}:{n}")
}

/// The corrective message's words (§3.3), with the validation errors shown.
fn schema_retry_text(errors: &[String]) -> String {
    let shown = if errors.is_empty() { "no structured output was given".to_string() } else { errors.join("; ") };
    format!("Your last answer did not match the requested schema: {shown}. Answer again with the StructuredOutput tool.")
}

/// The newest `structured_output` fact on `id` after `after_seq`: its seq and
/// its data. Paged forward, so a turn of any length answers the same.
fn newest_structured_after(m: &ThreadManager, id: &str, after_seq: u64) -> Result<Option<(u64, Value)>> {
    let mut from = after_seq + 1;
    let mut found = None;
    loop {
        let page = m.store.events_from(id, from, 500)?;
        let Some(last) = page.last() else { return Ok(found) };
        for e in &page {
            if let ThreadEvent::Custom { event_type, data } = &e.event {
                if event_type == STRUCTURED_OUTPUT_EVENT {
                    found = Some((e.seq, data.clone()));
                }
            }
        }
        from = last.seq + 1;
    }
}

/// **THE VERDICT ON THIS TURN** (R11.J4, wiring R11.J2's judge into the live
/// turn end): when the turn delivered a schema-bearing message, append ONE
/// `structured_output` fact judged by [`TurnTerminal::structured_record`] —
/// `for_message` is the asker's message (a corrective message names it) and
/// `attempt` counts from 1. Nothing when the turn bore no schema, or did not
/// end `ok` (no answer to judge). Idempotent: a verdict already on the ledger
/// after that message (a second writer, or a replayed epilogue) is not doubled.
fn record_verdict(m: &ThreadManager, id: &str, turn: &TurnTerminal) -> Result<()> {
    // A turn that straddled a restart (re-adopted in this life) never showed
    // this life its `turn_started`: replay the store from the newest one.
    let replayed;
    let turn = if turn.message_ids.is_empty() {
        let Some(start) = m.store.newest_of_kind(id, "turn_started", false)? else { return Ok(()) };
        let mut t = TurnTerminal::default();
        let mut from = start.seq;
        loop {
            let page = m.store.events_from(id, from, 500)?;
            let Some(last) = page.last() else { break };
            for e in &page {
                t.observe(&e.event);
            }
            from = last.seq + 1;
        }
        replayed = t;
        &replayed
    } else {
        turn
    };
    if turn.message_ids.is_empty() {
        return Ok(());
    }
    let kinds = ["user_message".to_string()];
    let q = crate::store::MessageQuery {
        ids: Some(&turn.message_ids),
        from: None,
        to: None,
        tail: None,
        kinds: Some(&kinds),
        cursor: None,
        limit: turn.message_ids.len(),
    };
    let page = m.store.query_messages(id, &q)?;
    let found = page.events.iter().find_map(|e| match (&e.event, e.message_id) {
        (ThreadEvent::UserMessage { metadata, .. }, Some(mid)) => {
            let own = &metadata[META_KEY];
            own.get("json_schema").filter(|s| s.is_object()).map(|s| (e.seq, mid, s.clone(), own.clone()))
        }
        _ => None,
    });
    let Some((seq, mid, schema, own)) = found else { return Ok(()) };
    if newest_structured_after(m, id, seq)?.is_some() {
        return Ok(());
    }
    let (root, n) = match own.get("schema_retry") {
        Some(r) => (r["of"].as_u64().unwrap_or(mid), r["n"].as_u64().unwrap_or(0)),
        None => (mid, 0),
    };
    let Some(rec) = turn.structured_record(&schema, None, Some(root), (n + 1) as u32) else { return Ok(()) };
    let crate::event::ThreadEvent::Custom { event_type, data } = rec.to_event() else { return Ok(()) };
    m.append_custom(id, event_type, data, false)?;
    Ok(())
}

/// **IS A CORRECTIVE TURN OWED, AND IF SO QUEUE IT** (R11.J4). `Ok(true)` when
/// one is queued now; `Ok(false)` when none is owed — the newest message bore
/// no schema, its turn has not been judged (or was judged `ok`), or its
/// retries are spent. Durable state only, so it is idempotent across lives.
fn schema_retry(m: &ThreadManager, id: &str) -> Result<bool> {
    let Some(u) = m.store.newest_of_kind(id, "user_message", false)? else { return Ok(false) };
    let (ThreadEvent::UserMessage { metadata, .. }, Some(mid)) = (&u.event, u.message_id) else { return Ok(false) };
    let own = &metadata[META_KEY];
    let Some(schema) = own.get("json_schema").filter(|s| s.is_object()) else { return Ok(false) };
    // The ROOT is the message the asker sent; a corrective message names it.
    let (root, n) = match own.get("schema_retry") {
        Some(r) => (r["of"].as_u64().unwrap_or(mid), r["n"].as_u64().unwrap_or(0)),
        None => (mid, 0),
    };
    let Some((_, verdict)) = newest_structured_after(m, id, u.seq)? else { return Ok(false) };
    if verdict["status"] != "unmet" {
        return Ok(false);
    }
    let limit = own
        .get("schema_retries")
        .and_then(Value::as_u64)
        .unwrap_or(SCHEMA_RETRIES_DEFAULT)
        .min(SCHEMA_RETRIES_MAX);
    if n >= limit {
        return Ok(false);
    }
    m.managed().halted(Halt::BeforeSchemaRetry)?;
    let next = n + 1;
    let rid = schema_retry_id(root, next);
    let errors: Vec<String> = verdict["errors"]
        .as_array()
        .map(|a| a.iter().map(|e| e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string())).collect())
        .unwrap_or_default();
    let mut carried = json!({ "json_schema": schema, "schema_retry": { "id": rid, "of": root, "n": next } });
    if let Some(l) = own.get("schema_retries") {
        carried["schema_retries"] = l.clone();
    }
    let mut meta = json!({ "origin": "daemon", "kind": "schema_retry" });
    meta[META_KEY] = carried;
    m.send(id, schema_retry_text(&errors), SendIntent::Immediate, meta, false)?;
    tracing::info!(thread_id = %id, retry = %rid, "structured output unmet: corrective turn queued");
    Ok(true)
}

/// The flags a child gets in a network a spawn names when the caller does not
/// say (`networks` as a list, or `network` without `child`): it sees and is
/// seen, and mail over the network may wake it. R4.S8's recommendation, acted
/// on — the design names no default (`docs/agent-networks.md` §4.2).
fn child_default_flags() -> crate::networks::Flags {
    crate::networks::Flags { view: true, visible: true, wakeable: true, ..Default::default() }
}

/// A present parameter must be a string; a non-string is refused, never read
/// as absent (`opt_str` would drop it silently).
fn strict_str(params: &Value, key: &str) -> Result<Option<String>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v.clone())),
        Some(other) => Err(refused(codes::MALFORMED, format!("`agents/spawn` `{key}` must be a string, got {other}"))),
    }
}

/// **R4.S8: THE FOUR SPAWN PARAMETERS, CHECKED BEFORE THE COMMIT POINT.**
/// Returns the fragment the spawn records in its `child{started}` detail, from
/// which [`materialize`] — at spawn time or in recovery — honours them. Every
/// refusal here happens before anything is written (R4.S2's Q4: refused typed
/// before the commit point, never after it).
///
/// * `agent` — opaque, non-empty, not flag-shaped (`--agent -x` would be read
///   as a flag by the CLI). Never resolved (§1.5).
/// * `effort` — one of [`crate::settings::EFFORT_LEVELS`].
/// * `networks` — `{id: flags}` (`docs/agent-networks.md` §4.2) or `[id]`
///   (`managed-subagents.md` §2.2; each gets [`child_default_flags`]): the
///   child is ADMITTED to each, which needs the caller to hold `members` there.
///   A parent holds authority over its unborn child (it is `imposable` in the
///   parent's home by construction), so it is admitted at once, never offered.
///   Refused `no_route` (not a network the caller is in — the same answer for
///   one that does not exist), `home`, `no_right`, `above_ceiling`.
/// * `network` — `{self?, child?, rules?, name?}`: mint a two-member network,
///   caller and child, in the same act (§4.2). Its id is minted HERE and
///   recorded, so recovery creates the same network and never a second.
///   Refused `malformed` (bad flags/rules), `too_large`, `above_ceiling`.
fn spawn_params(m: &ThreadManager, caller: &str, params: &Value, detail: &mut Value) -> Result<()> {
    use crate::networks::{self as nets, refusal, Flags, Rules};
    let bad = |why: String| refused(codes::MALFORMED, format!("`agents/spawn` {why}. Refused rather than ignored; nothing was spawned"));
    if let Some(agent) = strict_str(params, "agent")? {
        if agent.trim().is_empty() || agent.starts_with('-') {
            return Err(bad(format!("`agent` must be a non-empty agent name not starting with `-`, got {agent:?}")));
        }
        detail["agent"] = json!(agent);
    }
    if let Some(effort) = strict_str(params, "effort")? {
        if !crate::settings::EFFORT_LEVELS.contains(&effort.as_str()) {
            return Err(bad(format!(
                "`effort` must be one of {}, got {effort:?}",
                crate::settings::EFFORT_LEVELS.join(" | ")
            )));
        }
        detail["effort"] = json!(effort);
    }
    let parse_flags = |what: &str, v: &Value| -> Result<Flags> {
        serde_json::from_value(v.clone()).map_err(|e| bad(format!("`{what}`: {e}")))
    };
    // `networks`: admit the child into networks the caller holds `members` in.
    let mut joins = serde_json::Map::new();
    match params.get("networks") {
        None | Some(Value::Null) => {}
        Some(Value::Array(ids)) => {
            for id in ids {
                let id = id.as_str().ok_or_else(|| bad(format!("`networks` entries must be network ids, got {id}")))?;
                joins.insert(id.to_string(), serde_json::to_value(child_default_flags())?);
            }
        }
        Some(Value::Object(map)) => {
            for (id, f) in map {
                let f = parse_flags(&format!("networks.{id}"), f)?;
                joins.insert(id.clone(), serde_json::to_value(f)?);
            }
        }
        Some(other) => return Err(bad(format!("`networks` must be {{id: flags}} or [id], got {other}"))),
    }
    for id in joins.keys() {
        let no_route = || refused(refusal::NO_ROUTE, format!("no_route: `{caller}` admitting a child to `{id}`. No network you are in has that id. This answer is the same whether or not it exists"));
        let net = nets::load(m, id)?.ok_or_else(no_route)?;
        if net.is_home() {
            return Err(refused(refusal::HOME, format!("home: `{id}` is a home network; a child is in every ancestor's home by lineage, never by an admit")));
        }
        let roster = nets::members(m, &net)?;
        let mine = roster.get(caller).ok_or_else(no_route)?;
        if !mine.members {
            return Err(refused(refusal::NO_RIGHT, format!("no_right: you lack `members` in `{id}`, so you cannot admit a child to it")));
        }
        if roster.len() + 1 > nets::MAX_MEMBERS {
            return Err(refused(refusal::ABOVE_CEILING, format!("above_ceiling: `{id}` is at {} members (MAX_MEMBERS)", nets::MAX_MEMBERS)));
        }
    }
    if !joins.is_empty() {
        detail["networks"] = Value::Object(joins);
    }
    // `network`: mint a two-member network with the child.
    match params.get("network") {
        None | Some(Value::Null) => {}
        Some(Value::Object(o)) => {
            for k in o.keys() {
                if !["self", "child", "rules", "name"].contains(&k.as_str()) {
                    return Err(bad(format!("`network` takes self, child, rules, name; not `{k}`")));
                }
            }
            let me = match o.get("self").filter(|v| !v.is_null()) {
                Some(v) => parse_flags("network.self", v)?,
                None => Flags::creator(),
            };
            let child = match o.get("child").filter(|v| !v.is_null()) {
                Some(v) => parse_flags("network.child", v)?,
                None => child_default_flags(),
            };
            let rules: Rules = match o.get("rules").filter(|v| !v.is_null()) {
                Some(v) => serde_json::from_value(v.clone()).map_err(|e| bad(format!("`network.rules`: {e}")))?,
                None => Rules::default(),
            };
            if rules.max_chars == 0 || rules.max_chars > nets::MAX_MESSAGE_CHARS {
                return Err(refused(
                    refusal::TOO_LARGE,
                    format!("too_large: max_chars {} is outside 1..={} (MAX_MESSAGE_CHARS). Refused, not clamped", rules.max_chars, nets::MAX_MESSAGE_CHARS),
                ));
            }
            let name = match o.get("name").filter(|v| !v.is_null()) {
                Some(Value::String(n)) if !n.trim().is_empty() => n.clone(),
                Some(other) => return Err(bad(format!("`network.name` must be a non-empty string, got {other}"))),
                None => "spawn".to_string(),
            };
            if m.store.count_networks_created_by(caller)? >= nets::MAX_NETWORKS_PER_THREAD {
                return Err(refused(
                    refusal::ABOVE_CEILING,
                    format!("above_ceiling: a thread may create at most {} networks (MAX_NETWORKS_PER_THREAD)", nets::MAX_NETWORKS_PER_THREAD),
                ));
            }
            detail["network"] = json!({
                "id": uuid::Uuid::new_v4().to_string(),
                "name": name,
                "self": me,
                "child": child,
                "rules": rules,
            });
        }
        Some(other) => return Err(bad(format!("`network` must be an object {{self?, child?, rules?, name?}}, got {other}"))),
    }
    Ok(())
}

/// **MARK A THREAD'S SETTINGS AS AN AGENT'S** — `metadata.agent = who`
/// (TUI-P1a). The rest of the blob is kept: a `null` becomes an object, and a
/// non-object blob (never written by any client today) is kept under
/// `metadata.client` rather than discarded.
pub(crate) fn tag_agent(metadata: &mut Value, who: &str) {
    if metadata.is_null() {
        *metadata = json!({});
    }
    if !metadata.is_object() {
        *metadata = json!({ "client": metadata.take() });
    }
    metadata["agent"] = json!(who);
}

/// **R4.S8: HONOUR `networks` / `network` FOR A COMMITTED CHILD.** Called by
/// [`materialize`] BEFORE the child's thread row exists, so the row is the
/// marker that this ran: a kill before the row re-runs it (every write here
/// is an upsert of the same row, so it lands once), and a kill after it never
/// runs it again — which is what keeps recovery from re-admitting a child that
/// has since LEFT a network. The child has not run yet when this happens, so
/// its first turn already sees every network it was spawned into.
fn apply_spawn_networks(m: &ThreadManager, parent_id: &str, child_id: &str, detail: &Value) -> Result<()> {
    use crate::store::networks::{MemberRow, NetworkRow};
    let now = crate::store::now_rfc3339();
    let member = |net: &str, who: &str, flags: &Value| -> Result<()> {
        m.store.upsert_member(&MemberRow {
            network_id: net.to_string(),
            thread_id: who.to_string(),
            flags_json: flags.to_string(),
            state: "member".into(),
            joined_by: parent_id.to_string(),
            joined_at: now.clone(),
        })
    };
    if let Some(n) = detail.get("network").filter(|v| v.is_object()) {
        let id = n["id"].as_str().ok_or_else(|| refused(codes::INTEGRITY, "spawn detail `network` has no id"))?;
        if m.store.get_network(id)?.is_none() {
            m.store.upsert_network(&NetworkRow {
                id: id.to_string(),
                name: n["name"].as_str().unwrap_or("spawn").to_string(),
                created_by: parent_id.to_string(),
                rules_json: n["rules"].to_string(),
                created_at: now.clone(),
            })?;
            member(id, parent_id, &n["self"])?;
        }
        member(id, child_id, &n["child"])?;
    }
    if let Some(joins) = detail.get("networks").and_then(Value::as_object) {
        for (id, flags) in joins {
            member(id, child_id, flags)?;
        }
    }
    // R10.M3: the spawn's `notify` sets every link it creates between parent
    // and child — `home:<parent>`, the minted network, each admitted one — so
    // the child's answer is routed by the parent's word whichever it rides.
    // Absent, no row is written and the links read `direct` (the default).
    // Inside the same run-once-before-the-row window as the memberships, so a
    // recovery never overwrites a policy the parent changed since.
    if let Some(n) = detail.get("notify").and_then(Value::as_str) {
        use crate::networks::policy::{self, Notify};
        let n = if n == "hold" { Notify::Hold } else { Notify::Direct };
        let mut nets = vec![format!("{}{parent_id}", crate::networks::HOME_PREFIX)];
        if let Some(id) = detail["network"]["id"].as_str() {
            nets.push(id.to_string());
        }
        if let Some(joins) = detail.get("networks").and_then(Value::as_object) {
            nets.extend(joins.keys().cloned());
        }
        for id in nets {
            policy::set(m, parent_id, child_id, &id, n, json!({"act": "agents_spawn"}))?;
        }
    }
    Ok(())
}

/// **`agents/spawn`.** `caller` is the verified parent thread.
pub fn spawn(m: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = opt_str(params, crate::agents::CALLER)
        .ok_or_else(|| Error::Config("missing required param: caller".into()))?;
    let task = opt_str(params, "task")
        .ok_or_else(|| refused(codes::MALFORMED, "`agents/spawn` needs `task`"))?;
    for (k, why) in NOT_YET {
        if params.get(*k).is_some_and(|v| !v.is_null()) {
            return Err(refused(
                codes::NOT_AVAILABLE_YET,
                format!("`agents/spawn` parameter `{k}` cannot be honoured on this daemon yet: {why}. Refused rather than ignored; nothing was spawned"),
            ));
        }
    }
    // DEPRECATED(wake, remove after DEPLOY-9n) — R10.M3b (R10.M3, Q-23
    // option B). For ONE release `wake` is accepted again on both doors with
    // its pre-R10.M3 meaning: `never` = no spawn-report / run-end wake of the
    // parent (`detail.wake = "never"`, the projection `outstanding` and
    // `reply_owed` read) and NO policy row, so the child's own mail to the
    // parent stays `direct` and wakes it — never mapped to `hold`, which
    // would also hold that mail (the 55-minute sleep R10.W1 fixed). `end` is
    // the default. Beside `notify` it is refused: two words for one link.
    // Removing it: delete this block (and the param in `agents.rs`); the
    // refusal it replaced is `agents::REMOVED`'s.
    let deprecated_wake = match params.get("wake") {
        None | Some(Value::Null) => None,
        Some(Value::String(w)) if w == "end" || w == "never" => Some(w.clone()),
        Some(other) => {
            return Err(refused(
                codes::MALFORMED,
                format!("`agents/spawn` `wake` (deprecated) is \"end\" | \"never\", got {other}. Refused rather than ignored; nothing was spawned"),
            ))
        }
    };
    if deprecated_wake.is_some() && params.get("notify").is_some_and(|v| !v.is_null()) {
        return Err(refused(
            codes::MALFORMED,
            "`agents/spawn` was given both `wake` and `notify`: they are two words for one link. `wake` is deprecated (R10.M3b) and will be refused next release; pass only `notify` (or neither, to be woken by receipts). Refused rather than ignored; nothing was spawned",
        ));
    }
    let notify = crate::networks::policy::parse(params, "agents_spawn")?;
    // `detail.wake` is the PROJECTION the spawn-report fold still reads
    // (`ledger/spawn.rs` `outstanding`, the `threads.children` cell) until
    // R10.M4 deletes that fold: a `hold` link is not owed a wake, exactly as
    // `wake: never` was. The policy itself is the link's row, written by
    // `apply_spawn_networks`.
    let wake = match deprecated_wake.as_deref() {
        Some("never") => "never",
        Some(_) => "end",
        None if notify == Some(crate::networks::policy::Notify::Hold) => "never",
        None => "end",
    };
    // R11.J3: the schema is judged HERE, before anything is written; a bad
    // one refuses the whole act and nothing is queued (§3.1).
    let json_schema = match params.get("json_schema") {
        None | Some(Value::Null) => None,
        Some(s) => {
            check_json_schema(s)?;
            Some(s.clone())
        }
    };
    let request_id = opt_str(params, "request_id").unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let prow = m
        .store
        .get_thread(&caller)?
        .ok_or_else(|| refused(codes::UNKNOWN, format!("no such thread: {caller}")))?;
    let psettings = ThreadSettings::from_stored(&prow.settings_json)?;
    let mut detail = json!({ "wake": wake });
    if let Some(n) = notify {
        detail["notify"] = json!(n.word());
    }
    for k in ["model", "cwd", "append_system_prompt", "name"] {
        if let Some(v) = opt_str(params, k) {
            detail[k] = json!(v);
        }
    }
    spawn_params(m, &caller, params, &mut detail)?;
    // Recorded in `child{started}`, from which [`materialize`] — at spawn time
    // or in recovery — puts it on the task message.
    if let Some(s) = json_schema {
        detail["json_schema"] = s;
    }
    // R11.J4: how many corrective turns an `unmet` answer gets (§3.3, Q-2).
    // Recorded only when the caller named it; absent reads as the default.
    if let Some(n) = check_schema_retries(params)? {
        detail["schema_retries"] = json!(n);
    }
    let mut req = SpawnRequest::new(request_id.clone(), uuid::Uuid::new_v4().to_string(), task).detail(detail);
    req.budget_key = opt_str(params, "budget_key");

    let mg = m.managed();
    let _w = mg.write.lock().unwrap();
    let now = crate::store::now_rfc3339();
    let mut parent = match mg.load(&caller)? {
        Some(l) => l,
        None => Ledger::new(ThreadMeta::root(caller.clone(), RUNTIME, now.clone())),
    };
    let out = lspawn::spawn(
        &mut parent,
        psettings.budget_key.as_deref(),
        |id| mg.meta_of(id),
        &req,
        &now,
    )
    .map_err(|e| match e {
        SpawnRefused::BudgetKeyNamed { .. } => refused(codes::UNKNOWN_PARAM, e.to_string()),
        SpawnRefused::DepthCap { .. } => refused(codes::FORBIDDEN, e.to_string()),
        SpawnRefused::ParentCycle(_) => refused(codes::INTEGRITY, e.to_string()),
        SpawnRefused::RequestIdTaken { .. } => refused(codes::MALFORMED, e.to_string()),
    })?;
    let (child_id, created) = match &out {
        Spawned::Created { child, .. } => {
            mg.create(child)?;
            mg.halted(Halt::AfterChildLedger)?;
            mg.append_last(&parent, 1)?;
            m.emit_children_fact(&parent);
            mg.halted(Halt::AfterCommit)?;
            (child.thread_id().to_string(), true)
        }
        Spawned::Already { child_id } => (child_id.clone(), false),
    };
    let acc = materialize(m, &caller, &child_id)?;
    let acc = match acc {
        Some(a) => a,
        None => {
            // A replay of a spawn whose thread already exists: nothing moved,
            // and nothing will carry this promise.
            let p = m.promises.open::<crate::commands::ThreadsCreate>(Some(child_id.clone()));
            let _ = m
                .promises
                .abandon_all::<crate::commands::ThreadExistsFact>(Some(&child_id));
            p.accepted(Visible::Never).changed(false)
        }
    };
    let acc: Accepted = if created { acc } else { acc.changed(false) };
    let mut reply = acc.json();
    reply["thread_id"] = json!(child_id);
    reply["run_id"] = json!(request_id);
    // The network a spawn minted is an ADDRESS the act minted (R4.S8), read
    // back from the committed `child{started}` so a replay answers the same id.
    let minted = mg
        .load(&caller)?
        .and_then(|p| p.events().iter().find(|e| e.kind == Kind::Child && e.body["phase"] == "started" && e.body["child_id"] == child_id.as_str()).cloned())
        .and_then(|e| e.body["detail"]["network"]["id"].as_str().map(str::to_string));
    if let Some(id) = minted {
        reply["network"] = json!(id);
    }
    // DEPRECATED(wake, remove after DEPLOY-9n) — R10.M3b: told on the reply,
    // and counted on the caller's own ledger so a census can name callers.
    if let Some(w) = deprecated_wake {
        reply["warning"] = json!(crate::agents::WAKE_WARNING);
        m.append_custom(
            &caller,
            DEPRECATED_PARAM.to_string(),
            json!({"act": "agents_spawn", "param": "wake", "value": w, "child_id": child_id, "run_id": request_id, "remove_after": "DEPLOY-9n"}),
            false,
        )?;
    }
    Ok(reply)
}

/// The ledger event a deprecated parameter's use appends on the CALLER's
/// ledger (R10.M3b): `{act, param, value, child_id, run_id, remove_after}`.
/// DEPRECATED(wake, remove after DEPLOY-9n).
pub const DEPRECATED_PARAM: &str = "deprecated_param";

/// **A CHILD'S KEY FOLLOWS ITS PARENT'S (N-2).** A child is admitted against
/// the key its thread row carries, which [`materialize`] copied from the
/// parent at spawn. When the operator re-points the parent
/// (`threads/{thread_id}/budget_assign`) — the usual move when a key runs
/// out — the children used to stay parked on the old key while the parent
/// waited on them forever.
///
/// So the re-point walks the parent's committed descendants
/// ([`Managed::children_of`], recursively, cycle-safe) and re-points every
/// one still on the parent's OLD key, through the same
/// [`ThreadManager::update_settings`] the parent went through — which re-runs
/// admission (`maybe_dispatch`), so a child parked on the spent key goes out
/// under the new one with no restart. A descendant on a DIFFERENT key was
/// pointed there on purpose (a direct `budget_assign` on it); it is left
/// alone, and so is its subtree, which follows it rather than the root.
///
/// A committed child with no thread row yet is skipped: [`materialize`] will
/// copy the parent's settings when it creates the row, and re-reads the
/// parent's key once that row exists, so a spawn racing this walk still lands
/// on the new key (R9.N2b). A descendant whose re-point fails does not fail
/// the parent's re-point, which has already landed — but it is NOT silent:
/// it, and every member of its subtree still on the old key (which followed
/// it rather than the root), comes back in [`Repointed::left`], and the
/// `budget_assign` answer names them (R9.N2b).
pub fn repoint_descendants(m: &ThreadManager, parent_id: &str, old: Option<&str>, new: Option<&str>) -> Repointed {
    let mg = m.managed();
    let mut seen = std::collections::HashSet::new();
    seen.insert(parent_id.to_string());
    let mut frontier = mg.children_of(parent_id);
    let mut out = Repointed::default();
    // Subtrees below a failed re-point: their members stay where they are,
    // and those on the old key are named as left there.
    let mut stranded: Vec<(String, String)> = Vec::new();
    while let Some(id) = frontier.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let step = || -> Result<bool> {
            if m.store.get_thread(&id)?.is_none() || m.thread_budget_key(&id)?.as_deref() != old {
                return Ok(false);
            }
            if testing::repoint_fails(&id) {
                return Err(Error::Config(format!("injected re-point failure for {id} (test seam)")));
            }
            let patch: crate::settings::SettingsPatch = serde_json::from_value(json!({ "budget_key": new }))
                .map_err(|e| Error::Config(format!("bad budget key: {e}")))?;
            m.update_settings(&id, patch)?;
            Ok(true)
        };
        match step() {
            Ok(true) => {
                frontier.extend(mg.children_of(&id));
                out.moved.push(id);
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(parent = %parent_id, descendant = %id, error = %e, "budget_assign: re-pointing a descendant failed; it stays on the old key");
                stranded.extend(mg.children_of(&id).into_iter().map(|c| (c, id.clone())));
                out.left.push((id, e.to_string()));
            }
        }
    }
    while let Some((id, failed)) = stranded.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if m.store.get_thread(&id).ok().flatten().is_none() || m.thread_budget_key(&id).ok().flatten().as_deref() != old {
            continue;
        }
        stranded.extend(mg.children_of(&id).into_iter().map(|c| (c, failed.clone())));
        out.left.push((id, format!("its ancestor {failed} could not be re-pointed, and its subtree follows it")));
    }
    out
}

/// What a parent's re-point did to its descendants (R9.N2b).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Repointed {
    /// Descendants now on the new key.
    pub moved: Vec<String>,
    /// Descendants LEFT ON THE OLD KEY because their re-point failed (or an
    /// ancestor's did): `(thread id, why)`.
    pub left: Vec<(String, String)>,
}

/// **TEST SEAMS for the re-point (R9.N2b).** Process-global and keyed by
/// thread id, so concurrent tests in one binary cannot cross. Nothing in the
/// daemon arms them; unarmed, each is one uncontended lock and a miss.
///
/// **COMPILED ONLY FOR TESTS (R9.N2c, inspect F1).** Gated on `cfg(test)` or
/// the `test-seams` feature, which only this crate's own dev-dependency on
/// itself turns on (`Cargo.toml`), so its integration tests reach the seam
/// and a release build of the daemon — or of any crate depending on
/// `threads` — does not contain it: a call to `fail_repoint_of` from
/// production code is a compile error, not a silently stranded subtree. The
/// call sites in this file see the inert twin below instead.
#[cfg(any(test, feature = "test-seams"))]
#[doc(hidden)]
pub mod testing {
    use std::sync::Mutex;

    type Hook = Box<dyn FnOnce() + Send>;
    static WINDOW: Mutex<Vec<(String, Hook)>> = Mutex::new(Vec::new());
    static FAIL: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static REREAD: Mutex<Vec<(String, Hook)>> = Mutex::new(Vec::new());

    /// Run `f` once, inside the next [`super::materialize`] of a child of
    /// `parent_id`, AFTER it has read the parent's row and BEFORE it creates
    /// the child's — the window a racing `budget_assign` lands in.
    pub fn in_materialize_window(parent_id: &str, f: impl FnOnce() + Send + 'static) {
        WINDOW.lock().unwrap().push((parent_id.to_string(), Box::new(f)));
    }

    /// Run `f` once, inside the next [`super::materialize`] of a child of
    /// `parent_id`, AFTER it has created the child's row and re-read the
    /// parent's key and BEFORE it re-points the child to that key — the
    /// window a SECOND racing `budget_assign` lands in (R9.N2c).
    pub fn in_reread_window(parent_id: &str, f: impl FnOnce() + Send + 'static) {
        REREAD.lock().unwrap().push((parent_id.to_string(), Box::new(f)));
    }

    /// The next re-point of descendant `id` fails (once).
    pub fn fail_repoint_of(id: &str) {
        FAIL.lock().unwrap().push(id.to_string());
    }

    pub(super) fn take_window(parent_id: &str) -> Option<Hook> {
        let mut w = WINDOW.lock().unwrap();
        let i = w.iter().position(|(p, _)| p == parent_id)?;
        Some(w.remove(i).1)
    }

    pub(super) fn take_reread(parent_id: &str) -> Option<Hook> {
        let mut w = REREAD.lock().unwrap();
        let i = w.iter().position(|(p, _)| p == parent_id)?;
        Some(w.remove(i).1)
    }

    pub(super) fn repoint_fails(id: &str) -> bool {
        let mut f = FAIL.lock().unwrap();
        match f.iter().position(|x| x == id) {
            Some(i) => {
                f.remove(i);
                true
            }
            None => false,
        }
    }
}

/// The seams' inert twin for every build without `test-seams`: private, never
/// armed, so each call site folds to nothing.
#[cfg(not(any(test, feature = "test-seams")))]
mod testing {
    pub(super) fn take_window(_: &str) -> Option<fn()> {
        None
    }

    pub(super) fn take_reread(_: &str) -> Option<fn()> {
        None
    }

    pub(super) fn repoint_fails(_: &str) -> bool {
        false
    }
}

/// **THE OPERATOR CANCELLED `root`: SUPPRESS ITS SUBTREE'S WAKES, AND NAME
/// THE SUBTREE** (R9.N1, gap N-1). Called by `ThreadManager::cancel` BEFORE
/// any member is cancelled.
///
/// Walks the managed subtree below `root` (committed spawns, the same
/// `children_of` edge `agents_list` reads; cycle-safe by `seen`). On every
/// member — `root` included — that is still owed children, appends one
/// `system{fact: wake_suppressed, children}` naming exactly the children owed
/// at this instant, under the ledger write lock, so a report cannot slip in
/// between the read of the owed set and the marker. [`deliver_due_wakes`]
/// then withholds any wake whose emptying report is from one of those
/// children — at run time and at boot alike, because the marker is durable.
///
/// A child already reported is not in the set, so its report stands as it
/// was. Returns the descendants (not `root`), parents before children.
pub fn begin_subtree_cancel(m: &ThreadManager, root: &str) -> Vec<String> {
    let mg = m.managed();
    let mut seen = std::collections::HashSet::new();
    let mut order = Vec::new();
    let mut frontier = std::collections::VecDeque::from([root.to_string()]);
    while let Some(id) = frontier.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        frontier.extend(mg.children_of(&id));
        order.push(id);
    }
    let _w = mg.write.lock().unwrap();
    for id in &order {
        let Ok(Some(mut l)) = mg.load(id) else { continue };
        let owed = lspawn::outstanding(&l);
        if owed.is_empty() {
            continue;
        }
        let marked = l
            .append(Draft::new(
                Kind::System,
                crate::store::now_rfc3339(),
                json!({"fact": WAKE_SUPPRESSED, "children": owed, "reason": "the operator cancelled this thread"}),
            ))
            .map_err(|e| Error::Config(format!("{e:?}")))
            .and_then(|_| mg.append_last(&l, 1));
        match marked {
            Ok(()) => tracing::info!(thread = %id, owed = owed.len(), "cancel: the wake its owed children would queue is suppressed"),
            Err(e) => tracing::error!(thread = %id, error = %e, "cancel: COULD NOT RECORD THE WAKE SUPPRESSION"),
        }
    }
    order.into_iter().skip(1).collect()
}

/// **AN OPERATOR CANCEL DEFERS A MEMBER'S QUEUED MESSAGES — IT NEVER DROPS
/// THEM** (R10.M2, L-8, E21-4: "cancel their delivery but keep them ready to
/// deliver on the next turn … I don't want to lose the messages, above all").
///
/// R9.N1b's walk (`withdraw_queued`, reverted on the release) with the verb
/// changed: every row still waiting to be sent — the operator's own
/// follow-ups, network mail, a child-report wake queued before the cancel —
/// is set `ride` instead of `cancelled`. A rider never starts a turn (the
/// daemon does not argue with the person who said stop, R9.L3) and goes out,
/// oldest first, ahead of whatever starts the next one. Rows already `ride`
/// are left as they are. Returns how many rows were set `ride`.
pub fn ride_queued(m: &ThreadManager, id: &str) -> usize {
    let rows = match m.store.waiting_queue(id) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(thread = %id, error = %e, "cancel: COULD NOT READ THE QUEUE TO DEFER IT");
            return 0;
        }
    };
    let ids: Vec<u64> = rows.iter().filter(|r| r.dispatch != crate::event::delivery::RIDE).map(|r| r.message_id).collect();
    if ids.is_empty() {
        return 0;
    }
    let ridden = m.defer_to_ride(id, &ids, false, "operator_cancel");
    if ridden > 0 {
        tracing::info!(thread = %id, ridden, "cancel: queued messages deferred to ride the next turn");
    }
    ridden
}

/// Every child named by a `wake_suppressed` fact on `parent`.
fn suppressed_children(parent: &Ledger) -> std::collections::BTreeSet<String> {
    parent
        .events()
        .iter()
        .filter(|e| e.kind == Kind::System && e.body["fact"] == WAKE_SUPPRESSED)
        .filter_map(|e| e.body["children"].as_array())
        .flatten()
        .filter_map(|c| c.as_str().map(str::to_string))
        .collect()
}

/// Steps (3) and (4), idempotently, from the child's own ledger. Returns the
/// create acknowledgement when THIS call created the thread row.
fn materialize(m: &ThreadManager, parent_id: &str, child_id: &str) -> Result<Option<Accepted>> {
    let mg = m.managed();
    let child = mg
        .load(child_id)?
        .ok_or_else(|| refused(codes::INTEGRITY, format!("the parent {parent_id} names child {child_id}, whose ledger is missing")))?;
    let started = mg
        .load(parent_id)?
        .and_then(|p| {
            p.events()
                .iter()
                .find(|e| e.kind == Kind::Child && e.body["phase"] == "started" && e.body["child_id"] == child_id)
                .cloned()
        });
    let detail = started.as_ref().map(|e| e.body["detail"].clone()).unwrap_or(Value::Null);
    let mut acc = None;
    if m.store.get_thread(child_id)?.is_none() {
        let prow = m
            .store
            .get_thread(parent_id)?
            .ok_or_else(|| refused(codes::UNKNOWN, format!("no such thread: {parent_id}")))?;
        let mut s = ThreadSettings::from_stored(&prow.settings_json)?;
        if let Some(v) = detail.get("model").and_then(Value::as_str) {
            s.model = v.to_string();
        }
        if let Some(v) = detail.get("cwd").and_then(Value::as_str) {
            s.cwd = v.to_string();
        }
        if let Some(v) = detail.get("append_system_prompt").and_then(Value::as_str) {
            s.append_system_prompt = Some(v.to_string());
        }
        // R4.S8: a child runs as the agent and at the effort its spawn named;
        // absent, it inherits its parent's (the settings copy above).
        if let Some(v) = detail.get("agent").and_then(Value::as_str) {
            s.agent = Some(v.to_string());
        }
        if let Some(v) = detail.get("effort").and_then(Value::as_str) {
            s.effort = Some(v.to_string());
        }
        // **A SPAWNED CHILD IS AN AGENT'S THREAD, AND SAYS SO FROM BIRTH**
        // (TUI-P1a, TC-05/TC-06). The settings copy above carries the parent's
        // `app_id` — `aui` or `harness-tui` — so by app id alone a child looks
        // like a thread he started. `parent_id` hides it from his recents, but
        // only once a face holds that fact; the tag is in the settings the
        // child is CREATED with, so there is no window in which it is his.
        // `metadata` is the opaque client blob ("stored and returned, never
        // read" by this daemon); faces read `metadata.agent`.
        tag_agent(&mut s.metadata, &format!("agents_spawn:{parent_id}"));
        apply_spawn_networks(m, parent_id, child_id, &detail)?;
        if let Some(window) = testing::take_window(parent_id) {
            window();
        }
        let copied = s.budget_key.clone();
        let (_, a) = m.create_thread_acked(Some(child_id.to_string()), s)?;
        acc = Some(a);
        // **A SPAWN RACING `budget_assign` LANDS ON THE NEW KEY (R9.N2b).**
        // The parent's row was read above, before the child's row existed; a
        // re-point landing in between finds no row for this child and skips
        // it ([`repoint_descendants`]), so the copy just written carries the
        // spent key. Re-read the parent's key NOW that the row exists: either
        // the re-point's walk ran after this row was created (and moved it),
        // or the parent's new key was written before this read (and is seen
        // here) — the walk reads the child row only after writing the
        // parent's key. Chosen over taking `mg.write` in `budget_assign`,
        // which `update_settings`' admission would then run under, and which
        // the boot path (`recover`) does not hold around this call.
        //
        // **AND KEEPS FOLLOWING UNTIL IT IS STABLE (R9.N2c, inspect F2).** A
        // single re-read is not enough: a SECOND re-point landing between that
        // re-read and the child's write finds the child still on the copied
        // key — not on its own old key — so its walk skips the child, and the
        // write then lands the key the re-read saw, which is already stale.
        // So: after every write, read the parent again; while the child is
        // still on a key this spawn put there (the copy, or a key written by
        // this loop) and the parent has moved on, follow it. A child on any
        // OTHER key was pointed there on purpose (a direct `budget_assign` on
        // it) and is left alone, as [`repoint_descendants`] leaves it. Each
        // further pass needs a further re-point of the parent, so this ends
        // when the operator stops re-pointing; it is not a timed retry.
        let mut ours = vec![copied];
        loop {
            let parent_now = m.thread_budget_key(parent_id)?;
            if let Some(window) = testing::take_reread(parent_id) {
                window();
            }
            let child_now = m.thread_budget_key(child_id)?;
            if child_now == parent_now || !ours.contains(&child_now) {
                break;
            }
            let patch: crate::settings::SettingsPatch = serde_json::from_value(json!({ "budget_key": parent_now }))
                .map_err(|e| Error::Config(format!("bad budget key: {e}")))?;
            m.update_settings(child_id, patch)?;
            ours.push(parent_now);
        }
    }
    mg.halted(Halt::AfterThreadRow)?;
    if m.store.max_message_id(child_id)? == 0 {
        let text = child
            .events()
            .iter()
            .find(|e| e.kind == Kind::User)
            .and_then(|e| e.body["text"].as_str())
            .ok_or_else(|| refused(codes::INTEGRITY, format!("child {child_id}'s ledger holds no task")))?
            .to_string();
        let mut meta = json!({"origin": "agents_spawn", "parent": parent_id, "request_id": started.map(|e| e.id)});
        // R11.J3: the spawn's schema belongs to its TASK message (§3.1),
        // recorded immutably in the daemon's own key of that message's
        // metadata; [`turn_json_schema`] reads it back for the turn that
        // delivers it, and for no other.
        if let Some(s) = detail.get("json_schema").filter(|s| s.is_object()) {
            meta[META_KEY] = json!({ "json_schema": s });
            // R11.J4: the corrective-turn ceiling rides the same record, so
            // a retry (live, or after a restart) reads it off the message.
            if let Some(n) = detail.get("schema_retries").and_then(Value::as_u64) {
                meta[META_KEY]["schema_retries"] = json!(n);
            }
        }
        m.send(child_id, text, SendIntent::Immediate, meta, false)?;
    }
    Ok(acc)
}

// ═════════════════════════════════════════════════════════════════════════════
// A CHILD'S COMPLETION IS AN EVENT (H179, `docs/SUBAGENT-WAKE.md`)
// ═════════════════════════════════════════════════════════════════════════════
//
// The turn that ends a managed child's run hands its own terminal record to
// [`turn_finished`] — captured by [`TurnTerminal::observe`] as the turn's
// events were emitted, never read back out of the log. That record is written
// durably to the CHILD's ledger (`system{fact: run_ended}`), reported to the
// parent's ledger, and the parent is woken through the same delivery
// networks use (`crate::networks::post`). Nothing scans a tail to decide
// whether a child is done or how it came out.
//
// The ONE read of the store left is boot reconciliation (and a re-adopted
// turn whose first half ran in a dead daemon): [`terminal_from_store`], which
// reads the newest event of each terminal kind by index, flat in the length
// of the run.

/// The `system` fact recording how a managed child's run ended — the durable
/// completion record. `{fact, outcome, text}`.
const RUN_ENDED: &str = "run_ended";

/// **ONE TURN'S OWN TERMINAL RECORD**, captured as its events are emitted
/// (`ThreadManager::emit_resolving` calls [`TurnTerminal::observe`] on every
/// event) and taken whole by the turn's epilogue (`finish_turn`), so the next
/// turn starts empty. In memory on purpose: it is the fast path of a turn that
/// ended in THIS daemon; a turn that straddled a restart fills what it missed
/// from [`terminal_from_store`].
#[derive(Debug, Default, Clone)]
pub struct TurnTerminal {
    result_error: Option<bool>,
    failed: bool,
    cancelled: bool,
    interrupted: bool,
    words: Option<String>,
    /// R11.J2: the `result` line's `structured_output`, if any.
    structured_output: Option<Value>,
    /// R11.J2: the input of the LAST top-level `StructuredOutput` tool_use —
    /// the fallback candidate when the result line carries none (§3.3).
    last_structured_call: Option<Value>,
    /// R11.J4: the message ids this turn delivered (its `turn_started`), so
    /// the epilogue judges the answer against THIS turn's schema — never the
    /// next turn's, which `maybe_dispatch` may already have started.
    message_ids: Vec<u64>,
}

impl TurnTerminal {
    /// Fold one emitted event in.
    pub fn observe(&mut self, ev: &ThreadEvent) {
        match ev {
            ThreadEvent::ResultTurn { is_error, structured_output, .. } => {
                self.result_error = Some(*is_error);
                if structured_output.is_some() {
                    self.structured_output = structured_output.clone();
                }
            }
            ThreadEvent::ToolUse { name, input, parent_tool_use_id: None, .. }
                if name == crate::event::STRUCTURED_OUTPUT_TOOL =>
            {
                self.last_structured_call = Some(input.clone());
            }
            ThreadEvent::TurnStarted { message_ids, .. } => self.message_ids = message_ids.clone(),
            ThreadEvent::TurnFailed { .. } => self.failed = true,
            ThreadEvent::TurnInterrupted { trigger, .. } => {
                if trigger == crate::event::InterruptTrigger::Operator.as_str() {
                    self.cancelled = true;
                } else {
                    self.interrupted = true;
                }
            }
            ThreadEvent::AssistantText { text, parent_tool_use_id: None, .. } => self.words = Some(text.clone()),
            _ => {}
        }
    }

    /// R11.J2: the structured-output candidate this turn offers — the result
    /// line's value, else the last top-level `StructuredOutput` input. NOT a
    /// verdict; [`Self::structured_record`] judges it.
    pub fn structured_candidate(&self) -> Option<&Value> {
        crate::event::structured_candidate(self.structured_output.as_ref(), self.last_structured_call.as_ref())
    }

    /// R11.J2: the daemon's record for a turn that was asked for `schema`
    /// (§3.3). `None` unless the turn came out `ok` — a failed, cancelled or
    /// interrupted turn has no answer to judge, and its outcome speaks for it.
    /// Otherwise the candidate is validated, and only a valid value is `ok`.
    pub fn structured_record(
        &self,
        schema: &Value,
        turn: Option<String>,
        for_message: Option<u64>,
        attempt: u32,
    ) -> Option<crate::event::StructuredOutputRecord> {
        if self.outcome() != Some(Outcome::Ok) {
            return None;
        }
        Some(crate::event::StructuredOutputRecord::judge(
            schema,
            self.structured_candidate(),
            turn,
            for_message,
            attempt,
        ))
    }

    /// How the turn came out, when it said. Cancelled outranks everything (a
    /// cut turn may still have flushed a result), then a failure, then the
    /// CLI's own result line.
    fn outcome(&self) -> Option<Outcome> {
        Some(if self.cancelled {
            Outcome::Cancelled
        } else if self.failed || self.result_error == Some(true) {
            Outcome::Failed
        } else if self.result_error == Some(false) {
            Outcome::Ok
        } else if self.interrupted {
            Outcome::Interrupted
        } else {
            return None;
        })
    }
}

/// A child's completion record: machine status and its last words
/// (`managed-subagents.md` §1.2 — never anything the agent asserts).
#[derive(Debug, Clone)]
pub struct Terminal {
    pub outcome: Outcome,
    pub text: String,
    /// **THE RUN'S STRUCTURED RESULT** (R11.J5, §3.4): `{for_message, attempt,
    /// status, value?, errors?}` from the run's last schema-bearing turn —
    /// `None` exactly when the run carried no `json_schema`.
    pub structured: Option<Value>,
}

/// Nothing is in flight for `id`: no execution holds its lock, no turn
/// running, nothing queued, no retry armed and no park. Reads of CURRENT
/// state — what decides "terminal" for a turn that just ended is that its
/// epilogue found nothing left to do, not anything inferred from history.
///
/// **THE LOCK IS ASKED, through [`ThreadManager::is_idle`]** (R9.L5, GAP-AUDIT
/// L-5). A dispatch takes the execution lock and marks its queue rows
/// delivered BEFORE the `running_turns` row is written; `finish_turn` calls
/// `turn_finished` right after `maybe_dispatch` has done exactly that for a
/// queued follow-up. Four store rows alone read empty in that gap, and the
/// child was reported finished with its PREVIOUS turn's result. `is_idle` is
/// the one answer to "nothing executing, nothing waiting" (the lock, judged
/// live the way `thread_states` judges it, plus the queue and running row);
/// the retry and park reads stay here, because a child waiting on either is
/// not ended either. A lock left by a dead daemon is not live, so boot
/// `reconcile` still reports a child whose run ended with the old process.
fn idle(m: &ThreadManager, id: &str) -> Result<bool> {
    Ok(m.is_idle(id)? && m.store.get_retry(id)?.is_none() && m.store.get_parked_turn(id)?.is_none())
}

/// The newest completion record on a child's own ledger.
fn record_of(child: &Ledger) -> Option<Terminal> {
    child.events().iter().rev().find_map(|e| {
        (e.kind == Kind::System && e.body["fact"] == RUN_ENDED).then(|| Terminal {
            outcome: e.body["outcome"].as_str().and_then(Outcome::from_word).unwrap_or(Outcome::Failed),
            text: e.body["text"].as_str().unwrap_or("").to_string(),
            structured: e.body.get("structured").filter(|v| !v.is_null()).cloned(),
        })
    })
}

/// **THE CHILD'S TERMINAL STATE, READ FROM THE STORE BY KIND** — boot
/// reconciliation's source, and the fill for a turn that straddled a restart.
/// `None` when no turn has ended since the child's last message (it has not
/// run to an end). Each read is one search on `idx_events_thread_kind`; none
/// is a window, so a run of any length answers the same.
pub fn terminal_from_store(m: &ThreadManager, id: &str) -> Result<Option<Terminal>> {
    let after = m.store.last_seq_of_kind(id, "user_message")?.unwrap_or(0);
    let newest = |kind: &str| -> Result<Option<crate::event::StoredEvent>> {
        Ok(m.store.newest_of_kind(id, kind, false)?.filter(|e| e.seq > after))
    };
    let ended = newest("turn_ended")?;
    let failed = newest("turn_failed")?;
    let interrupted = newest("turn_interrupted")?;
    if ended.is_none() && failed.is_none() && interrupted.is_none() {
        return Ok(None);
    }
    let mut t = TurnTerminal::default();
    for e in [newest("result_turn")?, failed, interrupted].into_iter().flatten() {
        t.observe(&e.event);
    }
    let words = m.store.newest_of_kind(id, "assistant_text", true)?.filter(|e| e.seq > after);
    if let Some(e) = words {
        t.observe(&e.event);
    }
    Ok(Some(Terminal {
        outcome: t.outcome().unwrap_or(Outcome::Failed),
        text: t.words.unwrap_or_default(),
        structured: run_structured(m, id)?,
    }))
}

/// **THE RUN'S STRUCTURED RESULT, FROM DURABLE STATE** (R11.J5, §3.4): the
/// verdict on the run's LAST schema-bearing turn — the task, or the newest
/// corrective turn for it. `None` when no message of the run bore a schema;
/// when one did and no verdict followed its turn (the turn failed or was cut,
/// so there was no answer to judge), it is `unmet` with that said, because the
/// asker asked and did not get a value.
fn run_structured(m: &ThreadManager, id: &str) -> Result<Option<Value>> {
    let kinds = ["user_message".to_string()];
    let mut cursor = None;
    let mut last: Option<(u64, u64, Value)> = None; // (seq, message id, our metadata)
    loop {
        let q = crate::store::MessageQuery { ids: None, from: None, to: None, tail: None, kinds: Some(&kinds), cursor, limit: 200 };
        let page = m.store.query_messages(id, &q)?;
        let Some(tail) = page.events.last() else { break };
        cursor = tail.message_id;
        for e in &page.events {
            if let (ThreadEvent::UserMessage { metadata, .. }, Some(mid)) = (&e.event, e.message_id) {
                if metadata[META_KEY].get("json_schema").is_some_and(Value::is_object) {
                    last = Some((e.seq, mid, metadata[META_KEY].clone()));
                }
            }
        }
        if !page.has_more {
            break;
        }
    }
    let Some((seq, mid, own)) = last else { return Ok(None) };
    let (root, n) = match own.get("schema_retry") {
        Some(r) => (r["of"].as_u64().unwrap_or(mid), r["n"].as_u64().unwrap_or(0)),
        None => (mid, 0),
    };
    let mut s = json!({ "for_message": root, "attempt": n + 1 });
    match newest_structured_after(m, id, seq)? {
        Some((ev_seq, v)) => {
            let ok = v["status"] == "ok";
            s["status"] = json!(if ok { "ok" } else { "unmet" });
            s["event_seq"] = json!(ev_seq);
            if let Some(val) = v.get("value").filter(|x| !x.is_null()) {
                s["value"] = val.clone();
            }
            if !ok {
                s["errors"] = v.get("errors").filter(|x| x.is_array()).cloned().unwrap_or_else(|| json!([]));
            }
        }
        None => {
            s["status"] = json!("unmet");
            s["errors"] = json!(["the turn that carried the schema ended with no answer to judge"]);
        }
    }
    Ok(Some(s))
}

/// **`agents_result`'s structured keys, honestly** (R11.J5; `managed-subagents.md`
/// §2.3's `{outcome, text, structured_output?, schema_unmet?}`): no schema →
/// neither key; met → `structured_output`; not met → `schema_unmet: true` with
/// `errors`. An invalid value is NEVER served as `structured_output` — a caller
/// that keys on that field's presence must be able to trust it; the invalid
/// value stays on the `structured_output` fact and in the report's element.
fn result_structured(out: &mut Value, structured: Option<&Value>) {
    let Some(s) = structured else { return };
    if s["status"] == "ok" {
        out["structured_output"] = s.get("value").cloned().unwrap_or(Value::Null);
    } else {
        out["schema_unmet"] = json!(true);
        out["errors"] = s.get("errors").cloned().unwrap_or_else(|| json!([]));
    }
}

/// Is `id` ended, and how? Current state first (anything in flight is not
/// ended), then the child's own completion record, then — for a run that ended
/// before this build wrote records — the store's terminal events.
fn ended(m: &ThreadManager, id: &str) -> Result<Option<Terminal>> {
    if !idle(m, id)? {
        return Ok(None);
    }
    if let Some(t) = m.managed().load(id)?.as_ref().and_then(record_of) {
        return Ok(Some(t));
    }
    terminal_from_store(m, id)
}

/// How the child's run came out, and its last words. Refused-by-absence is
/// the caller's job: this answers `Failed` / empty for a run that has not
/// ended, so call it only on an ended run.
pub fn outcome_of(m: &ThreadManager, id: &str) -> Result<(Outcome, String)> {
    Ok(ended(m, id)?.map(|t| (t.outcome, t.text)).unwrap_or((Outcome::Failed, String::new())))
}

/// **A TURN ENDED — IF IT ENDED A MANAGED CHILD'S RUN, DELIVER THE COMPLETION
/// RECORD.** Called by `finish_turn` with the turn's own [`TurnTerminal`] on
/// every turn (and by `cancel` for a waiting delivery it dropped), so for any
/// thread that is not a managed child it is one file-existence check.
pub fn turn_finished(m: &ThreadManager, id: &str, turn: TurnTerminal) {
    m.managed().maybe_pause(id);
    // R10.H1 inspect F-1: one closer at a time — see `Managed::close`.
    let _close = m.managed().close.lock().unwrap_or_else(|e| e.into_inner());
    // Every waiting `gather` re-reads its predicate (R4.S8). After the report
    // below as well, so a waiter that re-checks before it lands is pinged again.
    m.managed().turn_ended.notify_waiters();
    // R11.J4: the daemon's verdict on a schema-bearing turn (§3.3/§3.4) is
    // written first, so the retry decision and the record both read it.
    if let Err(e) = record_verdict(m, id, &turn) {
        tracing::warn!(thread_id = %id, error = %e, "managed: recording the structured_output verdict failed; the run reads as unmet");
    }
    // R11.J4: an `unmet` answer with retries left is not the run's end — its
    // corrective turn is queued first, and the record waits for it.
    match schema_retry(m, id) {
        Ok(false) => {}
        Ok(true) => {
            m.managed().turn_ended.notify_waiters();
            return;
        }
        Err(e) => {
            tracing::warn!(thread_id = %id, error = %e, "managed: queueing a corrective turn failed; the live retry re-runs it (boot reconciliation is the backstop)");
            remember(m, Some(id), None);
            m.managed().turn_ended.notify_waiters();
            return;
        }
    }
    if m.managed().halted(Halt::LostCompletion).is_err() {
        // TEST ONLY (R10.H1): the completion is lost and NOT remembered.
        tracing::warn!(thread_id = %id, "managed: completion dropped at the LostCompletion test kill point");
        m.managed().turn_ended.notify_waiters();
        return;
    }
    let reported = complete(m, id, turn);
    m.managed().turn_ended.notify_waiters();
    if let Err(e) = reported {
        tracing::warn!(thread_id = %id, error = %e, "managed: delivering a child's completion failed; the live retry re-runs it (boot reconciliation is the backstop)");
        remember(m, Some(id), None);
    }
}

/// **REMEMBER A FAILED DELIVERY, AND MAKE SURE A RETRY IS SCHEDULED** (R9.N3).
///
/// The trigger is a bounded backoff TIMER, not `placement_wake`: the bell
/// rings only when some other input to placement moves, so a failure in a
/// quiet daemon would wait for an unrelated event (and the placement loop's
/// own cadence widens as passes come to nothing). A timer bounds the wait by
/// itself: [`RETRY_BASE`], doubling, capped at [`RETRY_CAP`], and the task
/// exists only while something is owed.
fn remember(m: &ThreadManager, child: Option<&str>, parent: Option<&str>) {
    let mg = m.managed();
    let start = {
        let mut o = mg.owed.lock().unwrap();
        if let Some(c) = child {
            o.children.insert(c.to_string());
        }
        if let Some(p) = parent {
            o.parents.insert(p.to_string());
        }
        !std::mem::replace(&mut o.running, true)
    };
    if !start {
        return;
    }
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        // No runtime to schedule on (a sync caller outside the daemon): boot
        // reconciliation is the only retry.
        mg.owed.lock().unwrap().running = false;
        tracing::warn!("managed: no runtime to schedule the delivery retry on; boot reconciliation retries it");
        return;
    };
    let m = m.clone();
    rt.spawn(async move { retry_loop(m).await });
}

/// The one retry task: wait a rung, retry everything owed, until nothing is.
async fn retry_loop(m: ThreadManager) {
    let (mut wait, cap) = *m.managed().backoff.lock().unwrap();
    loop {
        tokio::time::sleep(wait).await;
        let m2 = m.clone();
        // The retry does store and file I/O and takes the ledger write lock.
        let _ = tokio::task::spawn_blocking(move || retry_owed(&m2)).await;
        if m.managed().retry_done() {
            break;
        }
        wait = (wait * 2).min(cap);
    }
}

/// **RETRY EVERY OWED DELIVERY, ONCE** (R9.N3) — the per-child [`reconcile`]
/// and the per-parent [`deliver_due_wakes`] that boot [`recover`] runs, for
/// exactly the children and parents whose delivery failed while running.
/// Idempotent by the same paths boot uses: a child no longer owed is a no-op,
/// a wake already marked `wake_queued` is skipped, a wake whose message landed
/// unmarked is found by `wake_id` ([`already_sent`]) and only marked, and a
/// wake a `wake_suppressed` fact withholds stays withheld. What fails again is
/// remembered again. Returns how many are still owed.
pub fn retry_owed(m: &ThreadManager) -> usize {
    let mg = m.managed();
    let (children, parents) = {
        let mut o = mg.owed.lock().unwrap();
        o.passes += 1;
        (std::mem::take(&mut o.children), std::mem::take(&mut o.parents))
    };
    let mut parents: std::collections::BTreeSet<String> = parents;
    for c in &children {
        match reconcile(m, c) {
            Ok(()) => {
                if let Some(p) = mg.parent_of(c) {
                    parents.insert(p);
                }
            }
            Err(e) => {
                tracing::warn!(thread_id = %c, error = %e, "managed retry: reconciling a child failed again");
                mg.owed.lock().unwrap().children.insert(c.clone());
            }
        }
    }
    for p in &parents {
        let res = {
            let _w = mg.write.lock().unwrap();
            match mg.load(p) {
                Ok(Some(mut l)) => deliver_due_wakes(m, &mut l),
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            }
        };
        if let Err(e) = res {
            tracing::warn!(thread_id = %p, error = %e, "managed retry: delivering a wake failed again");
            mg.owed.lock().unwrap().parents.insert(p.clone());
        }
    }
    mg.retries_owed()
}

/// Write the child's completion record, then report it to the parent.
fn complete(m: &ThreadManager, id: &str, turn: TurnTerminal) -> Result<()> {
    let mg = m.managed();
    let Some(child) = mg.load(id)? else {
        return Ok(()); // not a managed thread
    };
    if child.meta().parent_id().is_none() || !idle(m, id)? {
        return Ok(()); // a root, or the run goes on (queued, retrying, parked)
    }
    mg.halted(Halt::BeforeCompletion)?;
    let mut outcome = turn.outcome();
    let mut text = turn.words.clone();
    if outcome.is_none() || text.is_none() {
        // A turn re-adopted after a restart saw only its second half here.
        if let Some(s) = terminal_from_store(m, id)? {
            outcome = outcome.or(Some(s.outcome));
            text = text.or(Some(s.text));
        }
    }
    let t = Terminal {
        outcome: outcome.unwrap_or(Outcome::Failed),
        text: text.unwrap_or_default(),
        structured: run_structured(m, id)?,
    };
    write_record(m, id, &t)?;
    tracing::info!(child = %id, outcome = t.outcome.word(), "managed: child run ended; completion record written");
    if mg.halted(Halt::LostReport).is_err() {
        tracing::warn!(child = %id, "managed: report dropped at the LostReport test kill point");
        return Ok(());
    }
    report(m, id, &t)
}

/// Append the completion record to the child's own ledger, `fsync`ed.
fn write_record(m: &ThreadManager, id: &str, t: &Terminal) -> Result<()> {
    let mg = m.managed();
    let _w = mg.write.lock().unwrap();
    let Some(mut child) = mg.load(id)? else { return Ok(()) };
    let mut body = json!({"fact": RUN_ENDED, "outcome": t.outcome.word(), "text": t.text});
    // R10.W1: the run's last turn (`reply_of_turn`, MESSAGING-ORCHESTRATION
    // §2.3) — the store seq of the child's newest `turn_started`. It is what
    // tells a LATER run's end (a reply owed to the parent) from a second
    // record of the run the spawn report already covered.
    if let Some(turn) = m.store.newest_of_kind(id, "turn_started", false)? {
        body["turn"] = json!(turn.seq);
    }
    // R10.W1b: the child's store position when the run ended — the next
    // run's window starts after it (see [`answered_in_run`]).
    body["end_seq"] = json!(m.store.max_seq(id)?);
    // R11.J5: `run_ended.structured`, present only when the run asked (§3.4).
    if let Some(s) = &t.structured {
        body["structured"] = s.clone();
    }
    child
        .append(Draft::new(Kind::System, crate::store::now_rfc3339(), body))
        .map_err(|e| Error::Config(format!("{e:?}")))?;
    mg.append_last(&child, 1)
}

/// Report a child's completion to its parent's ledger, and wake the parent
/// when it was the last child owed. Idempotent: a child the parent no longer
/// owes (already reported, or `wake: never`) is a no-op.
fn report(m: &ThreadManager, id: &str, t: &Terminal) -> Result<()> {
    let mg = m.managed();
    let Some(parent_id) = mg.parent_of(id) else { return Ok(()) };
    let _w = mg.write.lock().unwrap();
    let Some(mut parent) = mg.load(&parent_id)? else {
        return Ok(());
    };
    if !lspawn::outstanding(&parent).contains(id) {
        // R10.H1 (F2, D1): a `wake: never` child is not OWED — the parent is
        // not waiting and is not woken — but its run still ENDED, and that is
        // recorded: `child{finished}` alone (no words, no wake), so
        // `children.runs[].finished` is not null forever. The wake decision
        // stays on `outstanding`; this is only the record.
        if lspawn::unfinished(&parent).contains(id) {
            if lspawn::record_finished(&mut parent, id, t.outcome, &crate::store::now_rfc3339()) {
                if let Err(e) = mg.append_last(&parent, 1) {
                    remember(m, Some(id), None);
                    return Err(e);
                }
                m.emit_children_fact(&parent);
            }
            return Ok(());
        }
        // R10.W1: not owed by the SPAWN — but a later run (one a message
        // started) ending is a reply the parent is owed by the link's policy.
        return reply(m, id, &parent, t);
    }
    // R11.J5: the parent's model reads the value inside today's
    // `<agent_report>`, as a `<structured_output>` element after the words.
    let words = match &t.structured {
        Some(s) => format!("{}\n{}", t.text, lspawn::structured_element(s)),
        None => t.text.clone(),
    };
    let r = lspawn::report(&mut parent, id, t.outcome, &words, &crate::store::now_rfc3339());
    // R9.N3: a report that did not reach the parent's ledger leaves the child
    // owed — the retry re-runs `reconcile` for it.
    if let Err(e) = mg.append_last(&parent, 2) {
        remember(m, Some(id), None);
        return Err(e);
    }
    m.emit_children_fact(&parent);
    if r.wake.is_some() {
        // A failed wake remembers its parent inside `deliver_due_wakes`.
        deliver_due_wakes(m, &mut parent)?;
    }
    Ok(())
}

/// **BOOT: RECONCILE ONE CHILD THE PARENT STILL OWES.** From the child's own
/// completion record when it has one; otherwise — a death between the turn's
/// end and the record — from the store's terminal events, writing the record
/// first. A child still in flight is left to its own turn end.
fn reconcile(m: &ThreadManager, id: &str) -> Result<()> {
    let mg = m.managed();
    let Some(child) = mg.load(id)? else { return Ok(()) };
    let Some(parent_id) = child.meta().parent_id().map(str::to_string) else { return Ok(()) };
    // R11.J4: a death between an `unmet` verdict and its corrective message —
    // the run is not over; send the retry this life (once: see `schema_retry`).
    // Before the `owed` check, so a `wake: never` child is corrected as well.
    if record_of(&child).is_none() && idle(m, id)? && schema_retry(m, id)? {
        return Ok(());
    }
    // R10.H1: "owed a record", whatever the wake mode — a `wake: never` child
    // that ended unrecorded is finished here too ([`report`] keeps the wake
    // decision on `outstanding`).
    let owed = mg.load(&parent_id)?.is_some_and(|p| lspawn::unfinished(&p).contains(id));
    if !owed {
        // R10.W1: a reply owed for a later run, which a failure (live retry)
        // or a death (boot) left unposted. Idempotent by its reply id.
        if idle(m, id)? && reply_owed(&child, &parent_id, m)?.is_some() {
            if let Some(t) = record_of(&child) {
                let _w = mg.write.lock().unwrap();
                if let Some(parent) = mg.load(&parent_id)? {
                    return reply(m, id, &parent, &t);
                }
            }
        }
        return Ok(());
    }
    if !idle(m, id)? {
        return Ok(());
    }
    let t = match record_of(&child) {
        Some(t) => t,
        None => {
            let Some(t) = terminal_from_store(m, id)? else { return Ok(()) };
            write_record(m, id, &t)?;
            t
        }
    };
    tracing::info!(child = %id, parent = %parent_id, outcome = t.outcome.word(), "managed recovery: a child that ended while its parent waited is reported");
    report(m, id, &t)
}

/// **IS A REPLY OWED FOR THE CHILD'S NEWEST RUN, AND UNDER WHAT ID** (R10.W1,
/// `MESSAGING-ORCHESTRATION.md` §2.3: "a reply is produced when a child's run
/// ends"). The spawn's own run is reported by the `outstanding` fold
/// ([`report`]) — that path is unchanged. Every LATER run (one a message
/// started — the parent's `agents_message`, a sibling's mail, the operator)
/// used to end in silence: the child was no longer outstanding, so `report`
/// returned and the parent was never woken.
///
/// `Some("reply:<child>:<turn>")` exactly when: the parent's ledger committed
/// this child (`child{started}`) with a wake mode other than `never` (R10.H1:
/// that link's "do not tell me" is this explicit gate — a `wake: never` child
/// is recorded `child{finished}` too now), the spawn's run was already
/// reported (`child{finished}` on the parent), and the child's newest completion record
/// names a turn OTHER than the one its first record (the spawn's run) named.
/// The turn makes the id deterministic, so a retry, a duplicate epilogue or a
/// boot pass posts it at most once ([`sent_with`]).
fn reply_owed(child: &Ledger, parent_id: &str, m: &ThreadManager) -> Result<Option<String>> {
    let id = child.thread_id();
    let Some(parent) = m.managed().load(parent_id)? else { return Ok(None) };
    let (mut started, mut finished) = (false, false);
    for e in parent.events() {
        if e.kind != Kind::Child || e.body["child_id"] != id {
            continue;
        }
        match e.body["phase"].as_str() {
            // R10.H1: a `wake: never` link's "do not tell me" is kept HERE,
            // explicitly — since R10.H1 such a child DOES get its
            // `child{finished}`, so the missing finish no longer gates it.
            Some("started") if e.body["detail"]["wake"] == "never" => return Ok(None),
            Some("started") => started = true,
            Some("finished") => finished = true,
            _ => {}
        }
    }
    if !started || !finished {
        return Ok(None);
    }
    let turns: Vec<Option<u64>> = child
        .events()
        .iter()
        .filter(|e| e.kind == Kind::System && e.body["fact"] == RUN_ENDED)
        .map(|e| e.body["turn"].as_u64())
        .collect();
    let (Some(first), Some(Some(last))) = (turns.first(), turns.last()) else { return Ok(None) };
    if *first == Some(*last) {
        return Ok(None);
    }
    Ok(Some(format!("reply:{id}:{last}")))
}

/// **THE CHILD ALREADY ANSWERED ITS PARENT IN THIS RUN** (R10.W1b, the
/// R10.W1 inspector's F4; R13's rule — a delivered message is never sent
/// twice). A child re-tasked by mail that answers with `agents_message` and
/// then ends its run would otherwise give the parent TWO messages for one run:
/// its own receipt and the run-end reply. The receipt IS the run's reply, so
/// the automatic one is not posted. "This run" is the child's store after the
/// previous completion record's `end_seq` (its `turn` for a record written
/// before R10.W1b), read for a `mail_queued` fact addressed to the parent
/// whose `delivery` is `delivered`. A `held` receipt does not count.
fn answered_in_run(m: &ThreadManager, child: &Ledger, parent_id: &str) -> Result<bool> {
    let records: Vec<&Value> = child
        .events()
        .iter()
        .filter(|e| e.kind == Kind::System && e.body["fact"] == RUN_ENDED)
        .map(|e| &e.body)
        .collect();
    let Some(prev) = records.len().checked_sub(2).map(|i| records[i]) else { return Ok(false) };
    let Some(after) = prev["end_seq"].as_u64().or_else(|| prev["turn"].as_u64()) else { return Ok(false) };
    let id = child.thread_id();
    let mut from = after + 1;
    loop {
        let page = m.store.events_from(id, from, 500)?;
        let Some(last) = page.last() else { return Ok(false) };
        for e in &page {
            if let ThreadEvent::Custom { event_type, data } = &e.event {
                // Only a DELIVERED receipt answers the run (R10.W1b re-dispatch,
                // inspector H1): a HELD one woke nobody, so suppressing the
                // run-end reply too would leave the parent asleep — 8e4fca10.
                if event_type == "mail_queued" && data["to"] == parent_id && data["delivery"] == "delivered" {
                    return Ok(true);
                }
            }
        }
        from = last.seq + 1;
    }
}

/// **POST THE REPLY FOR A LATER RUN, BY THE LINK'S POLICY** (R10.W1; the
/// policy READ is R10.M3's). The link is (parent ← child over
/// `home:<parent>`), read by `networks::post` (`networks::policy`): `direct`
/// when no act set it (§2.2) — the reply is its own message on the parent's
/// queue, `Immediate`, so a dormant parent is woken exactly as a spawned run's
/// end wakes it. It RIDES instead (queued, never the cause of a turn) when the
/// link is `hold`, when the parent lowered its own `wakeable` in its home
/// network (the network's cap, §4 item 3), or when the run was cut by an
/// operator cancel (§7.2: the cascade's replies are riders). No-op when no
/// reply is owed or it was already posted.
fn reply(m: &ThreadManager, id: &str, parent: &Ledger, t: &Terminal) -> Result<()> {
    let mg = m.managed();
    let Some(child) = mg.load(id)? else { return Ok(()) };
    let pid = parent.thread_id().to_string();
    let Some(reply_id) = reply_owed(&child, &pid, m)? else { return Ok(()) };
    if sent_with(m, &pid, "reply_id", &reply_id)?.is_some() {
        return Ok(());
    }
    if answered_in_run(m, &child, &pid)? {
        tracing::info!(child = %id, parent = %pid, reply_id = %reply_id, "managed: the child mailed its parent in this run; that mail is the run's reply, none is posted");
        return Ok(());
    }
    let home = format!("{}{pid}", crate::networks::HOME_PREFIX);
    let words = match &t.structured {
        Some(s) => format!("{}\n{}", t.text, lspawn::structured_element(s)),
        None => t.text.clone(),
    };
    let text = format!(
        "<agent_reports>\n<agent_report thread_id=\"{id}\" outcome=\"{}\">\n{words}\n</agent_report>\n</agent_reports>",
        t.outcome.word()
    );
    let metadata = json!({
        "origin": "agents", "kind": lspawn::WAKE_REASON, "reply_id": reply_id, "children": [id],
        "mail": {"kind": "agent_reply", "from": id, "network": home},
    });
    let res = mg.halted(Halt::BeforeReplyPost).and_then(|()| {
        if t.outcome == Outcome::Cancelled {
            m.enqueue_undispatched(&pid, text, SendIntent::Immediate, metadata).map(|mid| {
                m.defer_to_ride(&pid, &[mid], false, "operator_cancel");
                (mid, Some("operator_cancel"))
            })
        } else {
            // R10.M3: the LINK's policy (parent ← child over home) and the
            // home cap are read by `post` — `direct` (no row) wakes, `hold`
            // or a lowered home `wakeable` rides.
            crate::networks::post_routed(m, &pid, text, SendIntent::Immediate, metadata)
        }
    });
    match res {
        Ok((mid, rides)) => {
            tracing::info!(child = %id, parent = %pid, reply_id = %reply_id, message_id = mid, rides = ?rides, "managed: a later run ended; its reply is posted to the parent");
            m.placement_wake(lspawn::WAKE_REASON);
            Ok(())
        }
        Err(e) => {
            remember(m, Some(id), None);
            Err(e)
        }
    }
}

/// The ids of the `child{finished}` events that emptied the parent's
/// outstanding set — each one a wake that is due — paired with the reports
/// that wake carries.
fn due_wakes(parent: &Ledger) -> Vec<(String, Vec<(String, String, String)>)> {
    let mut out = Vec::new();
    let mut started = std::collections::BTreeSet::new();
    let mut finished = std::collections::BTreeSet::new();
    let mut pending: Vec<(String, String, String)> = Vec::new();
    let evs = parent.events();
    for (i, e) in evs.iter().enumerate() {
        if e.kind != Kind::Child {
            continue;
        }
        let Some(cid) = e.body["child_id"].as_str() else { continue };
        match e.body["phase"].as_str() {
            Some("started") if e.body["detail"]["wake"] != "never" => {
                started.insert(cid.to_string());
            }
            Some("finished") => {
                let matched = started.contains(cid) && !finished.contains(cid);
                finished.insert(cid.to_string());
                if !matched {
                    continue;
                }
                let words = evs
                    .get(i + 1)
                    .filter(|n| n.kind == Kind::Context)
                    .and_then(|n| n.body["text"].as_str())
                    .unwrap_or("")
                    .to_string();
                pending.push((cid.to_string(), e.body["outcome"].as_str().unwrap_or("").to_string(), words));
                if started.difference(&finished).next().is_none() {
                    out.push((e.id.clone(), std::mem::take(&mut pending)));
                }
            }
            _ => {}
        }
    }
    out
}

fn wake_queued(parent: &Ledger, wake_id: &str) -> bool {
    parent
        .events()
        .iter()
        .any(|e| e.kind == Kind::System && e.body["fact"] == WAKE_QUEUED && e.body["wake_id"] == wake_id)
}

/// Queue every due wake the parent's ledger does not record as queued. A wake
/// whose `user_message` reached the store but whose marker did not (a kill
/// between the two) is found by its `wake_id` and only marked.
fn deliver_due_wakes(m: &ThreadManager, parent: &mut Ledger) -> Result<()> {
    let r = deliver_due_wakes_once(m, parent);
    if r.is_err() {
        // R9.N3: the wake is still due on the parent's ledger; the live retry
        // re-runs this for the parent (idempotent by `wake_id`).
        remember(m, None, Some(parent.thread_id()));
    }
    r
}

fn deliver_due_wakes_once(m: &ThreadManager, parent: &mut Ledger) -> Result<()> {
    let mg = m.managed();
    let pid = parent.thread_id().to_string();
    let suppressed = suppressed_children(parent);
    for (wake_id, reports) in due_wakes(parent) {
        if wake_queued(parent, &wake_id) {
            continue;
        }
        // R9.N1: the operator cancelled this parent while it was owed the
        // child whose report made this wake due — the cascade's report. The
        // parent is not RUN for it. R10.M2 (L-8, §7.2): it is not dropped
        // either — the wake is queued `ride`, so the reports go out with the
        // parent's next turn instead of never.
        let rides = reports.last().filter(|r| suppressed.contains(&r.0)).map(|r| r.0.clone());
        if let Some(last) = &rides {
            tracing::info!(parent = %pid, child = %last, wake_id = %wake_id, "managed: wake withheld — the parent was cancelled while it was owed this child; the reports ride its next turn");
        }
        let message_id = match already_sent(m, &pid, &wake_id)? {
            Some(mid) => mid,
            None => {
                mg.halted(Halt::BeforeWakePost)?;
                let mut text = String::from("<agent_reports>\n");
                for (cid, outcome, words) in &reports {
                    text.push_str(&format!(
                        "<agent_report thread_id=\"{cid}\" outcome=\"{outcome}\">\n{words}\n</agent_report>\n"
                    ));
                }
                text.push_str("</agent_reports>");
                let children: Vec<&str> = reports.iter().map(|r| r.0.as_str()).collect();
                let last = children.last().copied().unwrap_or_default();
                let metadata = json!({
                    "origin": "agents", "kind": lspawn::WAKE_REASON, "wake_id": wake_id, "children": children,
                    "mail": {"kind": "agent_report", "from": last, "network": format!("{}{pid}", crate::networks::HOME_PREFIX)},
                });
                if rides.is_some() {
                    // Queued WITHOUT a dispatch, then set `ride` — it starts no
                    // turn on the parent the operator cancelled. (A kill
                    // between the two leaves an eager wake: the pre-R9.N1
                    // behaviour, one extra turn, never a lost report.)
                    let mid = m.enqueue_undispatched(&pid, text, SendIntent::Immediate, metadata)?;
                    m.defer_to_ride(&pid, &[mid], false, "operator_cancel");
                    mid
                } else {
                    // THE NETWORK DELIVERY (H179): the completion reaches the
                    // parent as mail over its home network — `networks::post`,
                    // the same door `agents_message` mail takes — `Immediate`,
                    // so a dormant parent is dispatched in the same call.
                    crate::networks::post(m, &pid, text, SendIntent::Immediate, metadata)?
                }
            }
        };
        mg.halted(Halt::AfterWakePost)?;
        parent
            .append(Draft::new(
                Kind::System,
                crate::store::now_rfc3339(),
                json!({"fact": WAKE_QUEUED, "wake_id": wake_id, "message_id": message_id}),
            ))
            .map_err(|e| Error::Config(format!("{e:?}")))?;
        mg.append_last(parent, 1)?;
        m.placement_wake(lspawn::WAKE_REASON);
    }
    Ok(())
}

fn already_sent(m: &ThreadManager, pid: &str, wake_id: &str) -> Result<Option<u64>> {
    sent_with(m, pid, "wake_id", wake_id)
}

/// The message id of the `user_message` on `pid` whose metadata `key` is
/// `value`, if one landed.
fn sent_with(m: &ThreadManager, pid: &str, key: &str, value: &str) -> Result<Option<u64>> {
    let mut from = 0;
    loop {
        let page = m.store.events_from(pid, from, 500)?;
        let Some(last) = page.last() else { return Ok(None) };
        for e in &page {
            if let ThreadEvent::UserMessage { metadata, .. } = &e.event {
                if metadata[key] == value {
                    return Ok(e.message_id);
                }
            }
        }
        from = last.seq + 1;
    }
}

/// **BOOT: FINISH EVERY MANAGED ACT A DEATH INTERRUPTED.** Run by
/// `ThreadManager::recover`. Three passes, each idempotent:
///
/// 1. every `child{started}` in any ledger is materialized — thread row and
///    queued turn — from the child's own ledger;
/// 2. every child its parent still owes whose run is already TERMINAL is
///    reported — from the child's own completion record (`run_ended`), or,
///    for a death between the turn's end and that record, from its terminal
///    events read by kind ([`terminal_from_store`]); never from a tail;
/// 3. every due wake not marked queued is queued.
pub fn recover(m: &ThreadManager) {
    let mg = m.managed();
    let ids = mg.all();
    // R9.C7g: collected first so `/health` can say how many are still to
    // resume (`recovery.children_pending`) while this pass runs behind the
    // served port. Same walk, same order, same `materialize` per child.
    let rec = m.update.recovery();
    rec.phase("managed children");
    let mut started: Vec<(String, String)> = Vec::new();
    for pid in &ids {
        let Ok(Some(p)) = mg.load(pid) else { continue };
        for e in p.events() {
            if e.kind != Kind::Child || e.body["phase"] != "started" || e.within.is_some() {
                continue;
            }
            let Some(cid) = e.body["child_id"].as_str() else { continue };
            if mg.path(cid).exists() {
                started.push((pid.clone(), cid.to_string()));
            }
        }
    }
    rec.children(started.len() as u64);
    for (pid, cid) in &started {
        if let Err(err) = materialize(m, pid, cid) {
            tracing::warn!(parent = %pid, child = %cid, error = %err, "managed recovery: materializing a child failed");
        } else {
            tracing::info!(parent = %pid, child = %cid, "managed recovery: child resumed from its ledger");
        }
        rec.child_resumed();
    }
    rec.phase("managed reconcile");
    // 2. Every child its parent still owes that is already terminal — from
    //    the child's completion record, or its terminal events by kind.
    for id in &ids {
        if let Err(e) = reconcile(m, id) {
            tracing::warn!(thread_id = %id, error = %e, "managed recovery: reconciling a child failed");
        }
    }
    for pid in &ids {
        let _w = mg.write.lock().unwrap();
        let Ok(Some(mut p)) = mg.load(pid) else { continue };
        if let Err(e) = deliver_due_wakes(m, &mut p) {
            tracing::warn!(thread_id = %pid, error = %e, "managed recovery: delivering a wake failed");
        }
    }
    // R10.H1 (F1): what boot just did once, from now on periodically.
    start_sweep(m, count_unfinished(mg, &ids));
}

/// Every child some parent started and has not recorded finished, across all
/// ledgers — what decides the sweep's next period.
fn count_unfinished(mg: &Managed, ids: &[String]) -> usize {
    ids.iter()
        .filter_map(|pid| mg.load(pid).ok().flatten())
        .map(|p| lspawn::unfinished(&p).len())
        .sum()
}

/// **THE PERIODIC RECONCILIATION SWEEP** (R10.H1, F1; TC-13). Boot
/// [`recover`]'s passes 2 and 3 and the R9.N3 retry, run on a timer for EVERY
/// ledger, not only for the deliveries a failure remembered: for each
/// `child{started}` with no `child{finished}` whose child is idle with a
/// completion record (`run_ended`) or a terminal event in the store,
/// [`reconcile`] writes the record if missing and [`report`]s it —
/// `child{finished}` whatever the wake mode, the parent woken only when it was
/// owed (`wake: end`, via `outstanding`), `emit_children_fact` either way.
/// Then every due wake not marked queued is queued. A child still running is
/// left alone ([`reconcile`]'s `idle` read; a run with no terminal event has
/// nothing to report). Idempotent by the same paths boot uses. Returns how
/// many children are still unfinished after the pass.
pub fn sweep(m: &ThreadManager) -> usize {
    let mg = m.managed();
    let ids = mg.all();
    let settle = *mg.sweep_settle.lock().unwrap();
    for id in &ids {
        // R10.H1 inspect F-1: a run that ended within SWEEP_SETTLE belongs to
        // its live epilogue, not to the sweep.
        match settling(m, id, settle) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(thread_id = %id, error = %e, "managed sweep: reading a child's last turn end failed; skipped this pass");
                continue;
            }
        }
        let _close = mg.close.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = reconcile(m, id) {
            tracing::warn!(thread_id = %id, error = %e, "managed sweep: reconciling a child failed; the next pass retries it");
        }
    }
    for pid in &ids {
        let _w = mg.write.lock().unwrap();
        let Ok(Some(mut p)) = mg.load(pid) else { continue };
        if let Err(e) = deliver_due_wakes(m, &mut p) {
            tracing::warn!(thread_id = %pid, error = %e, "managed sweep: delivering a wake failed");
        }
    }
    let left = count_unfinished(mg, &ids);
    let mut st = mg.sweep_state.lock().unwrap();
    st.1 += 1;
    st.2 = left;
    left
}

/// Did `id`'s newest turn end (`turn_ended` / `turn_failed` /
/// `turn_interrupted`) land less than `settle` ago? A thread with none never
/// ran a turn to an end, so there is nothing to settle.
fn settling(m: &ThreadManager, id: &str, settle: std::time::Duration) -> Result<bool> {
    let mut newest: Option<crate::event::StoredEvent> = None;
    for kind in ["turn_ended", "turn_failed", "turn_interrupted"] {
        if let Some(e) = m.store.newest_of_kind(id, kind, false)? {
            if newest.as_ref().is_none_or(|n| e.seq > n.seq) {
                newest = Some(e);
            }
        }
    }
    let Some(e) = newest else { return Ok(false) };
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(&e.ts) else { return Ok(false) };
    let age = crate::clock::now().signed_duration_since(at.with_timezone(&chrono::Utc));
    Ok(age.to_std().map(|a| a < settle).unwrap_or(true))
}

/// Start the one sweep task (once per daemon; a second `recover` is a no-op).
fn start_sweep(m: &ThreadManager, unfinished: usize) {
    let mg = m.managed();
    {
        let mut st = mg.sweep_state.lock().unwrap();
        if st.0 {
            return;
        }
        st.0 = true;
        st.2 = unfinished;
    }
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        mg.sweep_state.lock().unwrap().0 = false;
        tracing::warn!("managed: no runtime to run the reconciliation sweep on; boot reconciliation is the only pass");
        return;
    };
    let m = m.clone();
    rt.spawn(async move { sweep_loop(m).await });
}

/// Wait the period the last pass chose ([`SWEEP_OWED`] while anything is
/// unfinished, [`SWEEP_IDLE`] otherwise), run a pass, repeat. A changed period
/// (tests) restarts the wait without running a pass.
async fn sweep_loop(m: ThreadManager) {
    loop {
        let kick = Arc::clone(&m.managed().sweep_kick);
        let notified = kick.notified();
        let wait = {
            let (owed, idle) = *m.managed().sweep_period.lock().unwrap();
            if m.managed().sweep_state.lock().unwrap().2 > 0 { owed } else { idle }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = notified => continue,
        }
        let m2 = m.clone();
        let _ = tokio::task::spawn_blocking(move || sweep(&m2)).await;
    }
}

/// **`agents/list`.** The caller's managed descendants, with machine state.
///
/// Each row: `thread_id`, `parent_id`, `state` (`running` | `parked` |
/// `ended`; `held` is on the schema and never produced yet), `outcome`
/// (`completed` | `crashed`, ended rows only, else null) and `depth` — BELOW
/// THE CALLER, so a child is 1 and a grandchild 2.
///
/// **`filter` is honoured, never ignored** (R4.S2b, the R4.S2 inspector's F2):
/// `state`, `outcome` and `depth` each keep only the rows equal to it, and they
/// AND together. A key the schema does not declare, or a value of the wrong
/// type, is refused `malformed` — a filter the daemon dropped would answer a
/// question the caller did not ask.
pub fn list(m: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = opt_str(params, crate::agents::CALLER)
        .ok_or_else(|| Error::Config("missing required param: caller".into()))?;
    let filter = ListFilter::parse(params.get("filter"))?;
    let mg = m.managed();
    let kids = |id: &str| mg.children_of(id);
    let tree = lspawn::subtree(&caller, kids).map_err(|c| refused(codes::INTEGRITY, c.to_string()))?;
    // Depth below the caller: a walk down the same tree the subtree fold built.
    let mut depth: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    depth.insert(caller.as_str(), 0);
    let mut stack = vec![caller.as_str()];
    while let Some(id) = stack.pop() {
        let d = depth[id];
        for k in tree.get(id).map(Vec::as_slice).unwrap_or(&[]) {
            depth.insert(k.as_str(), d + 1);
            stack.push(k.as_str());
        }
    }
    let mut rows = Vec::new();
    for (id, _) in tree.iter().filter(|(id, _)| **id != caller) {
        let meta = mg.meta_of(id);
        let state = run_state(m, id)?;
        let outcome = if state == "ended" {
            Some(match outcome_of(m, id)?.0 {
                Outcome::Ok => "completed",
                _ => "crashed",
            })
        } else {
            None
        };
        let d = depth.get(id.as_str()).copied().unwrap_or(0);
        if !filter.keeps(state, outcome, d) {
            continue;
        }
        rows.push(json!({
            "thread_id": id,
            "parent_id": meta.as_ref().and_then(|x| x.parent_id().map(str::to_string)),
            "state": state,
            "outcome": outcome,
            "depth": d,
        }));
    }
    Ok(json!({ "rows": rows }))
}

/// `agents_list`'s `filter`, parsed against exactly the schema it declares.
#[derive(Debug, Default)]
struct ListFilter {
    state: Option<String>,
    outcome: Option<String>,
    depth: Option<u64>,
}

impl ListFilter {
    fn parse(v: Option<&Value>) -> Result<ListFilter> {
        let bad = |why: String| refused(codes::MALFORMED, format!("`agents/list` `filter`: {why}. Refused rather than ignored"));
        let Some(v) = v.filter(|v| !v.is_null()) else {
            return Ok(ListFilter::default());
        };
        let obj = v.as_object().ok_or_else(|| bad(format!("must be an object, got {v}")))?;
        let mut f = ListFilter::default();
        for (k, x) in obj {
            if x.is_null() {
                continue;
            }
            match k.as_str() {
                "state" => {
                    let s = x.as_str().filter(|s| ["running", "held", "parked", "ended"].contains(s));
                    f.state = Some(s.ok_or_else(|| bad(format!("`state` must be running | held | parked | ended, got {x}")))?.to_string());
                }
                "outcome" => {
                    let s = x.as_str().filter(|s| ["completed", "crashed"].contains(s));
                    f.outcome = Some(s.ok_or_else(|| bad(format!("`outcome` must be completed | crashed, got {x}")))?.to_string());
                }
                "depth" => {
                    f.depth = Some(x.as_u64().ok_or_else(|| bad(format!("`depth` must be an integer >= 0, got {x}")))?);
                }
                other => return Err(bad(format!("`{other}` is not a filter key (state, outcome, depth)"))),
            }
        }
        Ok(f)
    }

    fn keeps(&self, state: &str, outcome: Option<&str>, depth: u64) -> bool {
        self.state.as_deref().map_or(true, |s| s == state)
            && self.outcome.as_deref().map_or(true, |o| Some(o) == outcome)
            && self.depth.map_or(true, |d| d == depth)
    }
}

/// Machine run state: `running` | `parked` | `ended`.
fn run_state(m: &ThreadManager, id: &str) -> Result<&'static str> {
    Ok(if ended(m, id)?.is_some() {
        "ended"
    } else if m.store.get_parked_turn(id)?.is_some() {
        "parked"
    } else {
        "running"
    })
}

/// **`agents/{agent}/result`.** Refused `busy` until the run has ended.
pub fn result(m: &ThreadManager, params: &Value) -> Result<Value> {
    let caller = opt_str(params, crate::agents::CALLER)
        .ok_or_else(|| Error::Config("missing required param: caller".into()))?;
    let agent = opt_str(params, "agent").ok_or_else(|| refused(codes::MALFORMED, "`agent` is required"))?;
    // `run` is on the schema; runs are not addressable yet — a child is one
    // run. Refused, never ignored (R4.S2b, the R4.S2 inspector's F2): answering
    // the latest run to a caller who named another would be a silent swap.
    if params.get("run").is_some_and(|v| !v.is_null()) {
        return Err(refused(
            codes::NOT_AVAILABLE_YET,
            "`agents/{agent}/result` parameter `run` cannot be honoured on this daemon yet: a managed \
             child is a single run and no run sequence is recorded to select by. Refused rather than \
             ignored; omit `run` for the latest (only) run",
        ));
    }
    if !m.managed().is_descendant_or_self(&caller, &agent) || caller == agent {
        return Err(refused(
            codes::FORBIDDEN,
            format!("`{agent}` is not a managed descendant of `{caller}`"),
        ));
    }
    let Some(Terminal { outcome, text, structured }) = ended(m, &agent)? else {
        return Err(refused(codes::BUSY, format!("not_ended: `{agent}`'s run has not ended; wait for the report or call again")));
    };
    let outcome = match outcome {
        Outcome::Ok => "completed",
        _ => "crashed",
    };
    let mut out = json!({ "agent": agent, "outcome": outcome, "text": text });
    result_structured(&mut out, structured.as_ref());
    Ok(out)
}

// ═════════════════════════════════════════════════════════════════════════════
// `agents/gather` — §2.4's spelling (c), the bounded block (R4.S8)
// ═════════════════════════════════════════════════════════════════════════════

/// `poll_s` when the caller does not say (§2.4: "default 60").
pub const GATHER_POLL_DEFAULT_S: u64 = 60;
/// The ceiling on one call's `poll_s`. Refused above it, never clamped: the
/// whole point of (c) is that no request outlives one short interval, because
/// the daemon's restart gate counts open requests (`chassis.rs`, `update.rs`).
pub const GATHER_POLL_MAX_S: u64 = 300;
/// How long a waiting gather sleeps between re-reads when no turn end pings
/// it — a backstop for a state change that is not a turn end (a park, a
/// retry clearing). The ping is the fast path.
/// `THREADS_GATHER_RECHECK_MS` overrides it, so a test can stretch it past its
/// own bound and prove the PING wakes a gather — with a 1 s backstop a missing
/// ping is invisible (R4.S8's forcing F10 measured exactly that).
fn gather_recheck() -> std::time::Duration {
    std::time::Duration::from_millis(
        std::env::var("THREADS_GATHER_RECHECK_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1000),
    )
}
/// An unsatisfied gather's first-asked time is forgotten after this long
/// without being asked again.
const GATHER_FORGET: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// The predicate of `until` (§2.4): `all` | `any` | `count:<n>` |
/// `any_state:[<state>,…]`.
#[derive(Debug, Clone, PartialEq)]
enum Until {
    All,
    Any,
    Count(usize),
    AnyState(Vec<String>),
}

impl Until {
    fn parse(s: &str) -> Result<Until> {
        let bad = || refused(codes::MALFORMED, format!("`agents/gather` `until` must be all | any | count:<n> | any_state:[<state>,…], got {s:?}"));
        Ok(match s.trim() {
            "all" => Until::All,
            "any" => Until::Any,
            t if t.starts_with("count:") => Until::Count(t["count:".len()..].trim().parse().map_err(|_| bad())?),
            t if t.starts_with("any_state:") => {
                let list = t["any_state:".len()..].trim().trim_start_matches('[').trim_end_matches(']');
                let states: Vec<String> = list.split(',').map(|x| x.trim().trim_matches('"').to_string()).filter(|x| !x.is_empty()).collect();
                if states.is_empty() || states.iter().any(|x| !["running", "held", "parked", "ended"].contains(&x.as_str())) {
                    return Err(bad());
                }
                Until::AnyState(states)
            }
            _ => return Err(bad()),
        })
    }

    fn holds(&self, states: &[&str]) -> bool {
        let ended = states.iter().filter(|s| **s == "ended").count();
        match self {
            Until::All => ended == states.len(),
            Until::Any => ended >= 1,
            Until::Count(n) => ended >= *n,
            Until::AnyState(set) => states.iter().any(|s| set.iter().any(|x| x == s)),
        }
    }
}

/// One gathered run: the child thread and the run id its spawn minted (the
/// `child{started}` event's id, which is the spawn's `request_id`).
struct Run {
    thread_id: String,
    run_id: String,
}

/// Resolve `runs` against the caller's managed subtree. `"children"` is every
/// direct child; otherwise each entry is a run id (a spawn's `run_id`) or a
/// descendant's thread id. Anything else is refused `forbidden` — the
/// `agents_result` answer for a thread that is not yours.
fn gather_runs(m: &ThreadManager, caller: &str, runs: &Value) -> Result<Vec<Run>> {
    let mg = m.managed();
    // Every committed spawn in the caller's subtree: (parent, child, run id).
    let tree = lspawn::subtree(caller, |id| mg.children_of(id)).map_err(|c| refused(codes::INTEGRITY, c.to_string()))?;
    let mut spawns: Vec<(String, Run)> = Vec::new();
    for pid in tree.keys() {
        let Some(l) = mg.load(pid)? else { continue };
        for e in l.events() {
            if e.kind == Kind::Child && e.body["phase"] == "started" && e.within.is_none() {
                if let Some(cid) = e.body["child_id"].as_str() {
                    spawns.push((pid.clone(), Run { thread_id: cid.to_string(), run_id: e.id.clone() }));
                }
            }
        }
    }
    let take = |r: &Run| Run { thread_id: r.thread_id.clone(), run_id: r.run_id.clone() };
    match runs {
        Value::String(s) if s == "children" => Ok(spawns.iter().filter(|(p, _)| p == caller).map(|(_, r)| take(r)).collect()),
        Value::Array(ids) => {
            let mut out: Vec<Run> = Vec::new();
            for id in ids {
                let id = id
                    .as_str()
                    .ok_or_else(|| refused(codes::MALFORMED, format!("`agents/gather` `runs` entries must be run or thread ids, got {id}")))?;
                let found = spawns.iter().find(|(_, r)| r.run_id == id || r.thread_id == id).map(|(_, r)| take(r));
                let Some(r) = found else {
                    return Err(refused(codes::FORBIDDEN, format!("`{id}` is not a run of a managed descendant of `{caller}`")));
                };
                if !out.iter().any(|x| x.thread_id == r.thread_id) {
                    out.push(r);
                }
            }
            Ok(out)
        }
        other => Err(refused(codes::MALFORMED, format!("`agents/gather` `runs` must be [id] or \"children\", got {other}"))),
    }
}

/// One evaluation of the predicate over durable state: the rows, and whether
/// it holds.
fn gather_snapshot(m: &ThreadManager, caller: &str, runs: &[Run], until: &Until) -> Result<(bool, Vec<Value>, Vec<String>)> {
    let mut states = Vec::new();
    let mut rows = Vec::new();
    let mut pending = Vec::new();
    for r in runs {
        let state = run_state(m, &r.thread_id)?;
        let mut row = json!({
            "thread_id": r.thread_id,
            "run_id": r.run_id,
            "parent_id": m.managed().parent_of(&r.thread_id),
            "state": state,
            "outcome": Value::Null,
            "depth": depth_below(m, caller, &r.thread_id),
        });
        if state == "ended" {
            let t = ended(m, &r.thread_id)?;
            let (outcome, text) = t.as_ref().map(|t| (t.outcome, t.text.clone())).unwrap_or((Outcome::Failed, String::new()));
            let outcome = if outcome == Outcome::Ok { "completed" } else { "crashed" };
            row["outcome"] = json!(outcome);
            // R11.J5: the same honest keys as `agents_result` — the
            // R4.S2b-inspect constant `schema_unmet: true` is gone here too.
            let mut res = json!({ "outcome": outcome, "text": text });
            result_structured(&mut res, t.as_ref().and_then(|t| t.structured.as_ref()));
            row["result"] = res;
        } else {
            pending.push(r.thread_id.clone());
        }
        states.push(state);
        rows.push(row);
    }
    Ok((until.holds(&states), rows, pending))
}

fn depth_below(m: &ThreadManager, caller: &str, id: &str) -> u64 {
    let mg = m.managed();
    let mut d = 0;
    let mut cur = Some(id.to_string());
    while let Some(c) = cur {
        if c == caller || d > 10_000 {
            break;
        }
        d += 1;
        cur = mg.parent_of(&c);
    }
    d
}

/// **`agents/gather`** — a READ that waits in place, boundedly (§2.4 (c)).
///
/// Answers as soon as `until` holds over `runs`: `{satisfied: true, rows}`,
/// each row `agents_list`'s plus `run_id` and, for an ended run, its `result`.
/// When the predicate does not hold within `poll_s` (default
/// [`GATHER_POLL_DEFAULT_S`], at most [`GATHER_POLL_MAX_S`]) it answers
/// `{satisfied: false, pending}` and the agent calls again; when `timeout_s`
/// has passed since this same gather was FIRST asked, it answers
/// `{satisfied: false, timed_out: true}`. Nothing is held across calls but
/// that first-asked instant, so a daemon restart costs one retried call.
///
/// The wait is woken by every turn end ([`turn_finished`]), with a
/// [`gather_recheck`] backstop; it never reads `Liveness::busy()`.
pub async fn gather(m: &ThreadManager, params: &Value) -> Result<Value> {
    use std::time::{Duration, Instant};
    let caller = opt_str(params, crate::agents::CALLER)
        .ok_or_else(|| Error::Config("missing required param: caller".into()))?;
    let runs_v = params.get("runs").ok_or_else(|| refused(codes::MALFORMED, "`agents/gather` needs `runs`"))?;
    let until_s = opt_str(params, "until").ok_or_else(|| refused(codes::MALFORMED, "`agents/gather` needs `until` (a string)"))?;
    let until = Until::parse(&until_s)?;
    let int = |k: &str| -> Result<Option<u64>> {
        match params.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_u64().map(Some).ok_or_else(|| refused(codes::MALFORMED, format!("`agents/gather` `{k}` must be an integer >= 0, got {v}"))),
        }
    };
    let poll_s = int("poll_s")?.unwrap_or(GATHER_POLL_DEFAULT_S);
    if poll_s == 0 || poll_s > GATHER_POLL_MAX_S {
        return Err(refused(
            codes::MALFORMED,
            format!("`agents/gather` `poll_s` must be 1..={GATHER_POLL_MAX_S}, got {poll_s}. Refused rather than clamped: no call may outlive one short interval"),
        ));
    }
    let timeout_s = int("timeout_s")?;
    let runs = gather_runs(m, &caller, runs_v)?;
    if runs.is_empty() {
        return Err(refused(codes::MALFORMED, format!("`agents/gather`: `{caller}` has no runs to gather (`runs` resolved to none)")));
    }
    if let Until::Count(n) = until {
        if n == 0 || n > runs.len() {
            return Err(refused(codes::MALFORMED, format!("`agents/gather` `until: count:{n}` can never be met over {} runs", runs.len())));
        }
    }
    // `timeout_s` counts from the first time THIS gather was asked.
    let mut ids: Vec<&str> = runs.iter().map(|r| r.thread_id.as_str()).collect();
    ids.sort();
    let key = format!("{caller}|{until_s}|{}", ids.join(","));
    let now = Instant::now();
    let first = {
        let mut g = m.managed().gathers.lock().unwrap();
        g.retain(|_, t| now.duration_since(*t) < GATHER_FORGET);
        *g.entry(key.clone()).or_insert(now)
    };
    let call_end = now + Duration::from_secs(poll_s);
    let give_up = timeout_s.map(|t| first + Duration::from_secs(t));
    let deadline = give_up.map_or(call_end, |g| g.min(call_end));
    let notify = Arc::clone(&m.managed().turn_ended);
    loop {
        // Registered BEFORE the read, so a turn end between the read and the
        // wait is not lost.
        let pinged = notify.notified();
        tokio::pin!(pinged);
        pinged.as_mut().enable();
        let (satisfied, rows, pending) = gather_snapshot(m, &caller, &runs, &until)?;
        let now = Instant::now();
        let timed_out = !satisfied && give_up.is_some_and(|g| now >= g);
        if satisfied || timed_out || now >= deadline {
            if satisfied || timed_out {
                m.managed().gathers.lock().unwrap().remove(&key);
            }
            return Ok(json!({
                "satisfied": satisfied,
                "timed_out": timed_out,
                "until": until_s,
                "rows": rows,
                "pending": pending,
                "poll_s": poll_s,
            }));
        }
        let wake = deadline.min(now + gather_recheck());
        tokio::select! {
            _ = &mut pinged => {}
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(wake)) => {}
        }
    }
}
