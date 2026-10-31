//! # A parent causes a child thread to exist — and waits for it by ending its turn
//!
//! `docs/proto-ledger.md` §5 is the design; `docs/managed-subagents.md` §1 is
//! the model. This file is the runtime of the thing those two describe, and it
//! is **four pure functions over ledgers**. Nothing here launches a process,
//! opens a socket, touches `threads.db` or reaches the running daemon —
//! `docs/ASSUMPTIONS-LOG.md` A5, the same stance as the rest of the module.
//!
//! ## There is no "launch a subagent", because `-p` is not a mode
//!
//! The operator: *"I don't see why we need a `-p` at all if we are supporting
//! sending in standard input."* He is right, and the consequence is the shape
//! of this file. A one-shot `-p` process cannot be resumed, cannot be sent a
//! message mid-flight and has no address, so it cannot be the thing a managed
//! child *is*. What a child is, is **a thread with a turn waiting in it**:
//!
//! 1. [`spawn`] mints the child's [`ThreadMeta`] from the parent's, appends the
//!    request text to the child's ledger as one `user` event, and records
//!    `child{started}` in the parent's.
//! 2. Placement runs it, on exactly the path any other thread's queued turn
//!    takes (`ThreadManager::maybe_dispatch`).
//!
//! stdin is not a mode; it is how a turn's text arrives. So **spawning and
//! sending are the same act**, and this file has no third verb for it.
//!
//! ## How a parent waits: it ends its turn, and the ledger remembers why
//!
//! The ruling was *"stop and get resumed"* — `managed-subagents.md` §9 Q1's
//! third option, over holding a request open and over re-polling. A parent
//! that is waiting therefore does nothing special: **its turn ends, normally,
//! with outcome `ok`, and it goes dormant.** Dormancy is free; it is what every
//! thread does between turns.
//!
//! What makes the wait a wait is that the parent's *next* turn does not arrive
//! until a child reports, and the parent's own immutable ledger is what says
//! whether it is still owed one. [`outstanding`] is that answer, and it is a
//! **fold, not a column**: the `child_id`s with a `child{started}` and no
//! `child{finished}`. Exactly as depth is a walk over `parent_id` rather than a
//! cached column (§5), being-owed-a-child is a fold over `child` events rather
//! than a stored state.
//!
//! When a child finishes, [`report`] appends `child{finished}` and the child's
//! words to the parent, and — **only when that empties the outstanding set** —
//! returns a [`Wake`]. The daemon turns a `Wake` into one call,
//! `ThreadManager::placement_wake(`[`WAKE_REASON`]`)`, and the ordinary
//! placement pass does the rest: `resume_sweep_placing` already puts every
//! thread with a waiting queue row through `maybe_dispatch`. The parent resumes
//! as an ordinary `--resume` turn.
//!
//! ### Why this adds no `ParkCause` variant, and what that gives up
//!
//! `ParkCause` (`budgets.rs:614`) looked like the place for "waiting on a
//! child", and it is not. Three measured reasons:
//!
//! * **Every one of its ten variants is an admission verdict about accounts or
//!   budget** — a threshold, a meter, a scope, a clamp, a disabled login. None
//!   is a fact about another thread.
//! * **`resume_sweep_placing` (`runtime.rs:6200`) re-asks the gate for every
//!   parked row on every pass**, with no predicate on the row itself. A
//!   child-wait cause would therefore make `Budgets::admit` the function that
//!   has to answer a *lineage* question, on every pass, for every parked
//!   thread. That is a coupling with nothing to recommend it.
//! * **`parked_turns` has no cause column at all** (`schema.sql:644`). The
//!   machine tag survives only on the `TurnParked` event, as a bare string, so
//!   a variant would not even be what the durable row remembers.
//!
//! And a park is the wrong *shape* regardless: a parked turn is a turn whose
//! **text already exists** and is being held (`parked_turns.prompt`, verbatim,
//! CON-4). A parent waiting on a child has no held text — its next turn's text
//! *is* the child's report, which does not exist yet. There is nothing to put
//! in the column.
//!
//! So the wait needs no new state, and the precedent for that is already in the
//! tree: `"login"` is a first-class park whose cause is not a `ParkCause`
//! variant (`runtime.rs:8395`). **Given up:** a parent cannot wait on something
//! that is not a child — a timer, an operator approval, an arbitrary predicate
//! — because the only wait this file can express is "a child I started has not
//! finished". `agents_watch`'s general `until` predicate
//! (`managed-subagents.md` §2.4) would need more than a fold. And a parent's
//! wait is not visible in SQL: answering "what is this thread waiting for"
//! means reading its ledger, where answering it from a column would have been
//! one query. Depth made that same trade first, deliberately.
//!
//! ### What this replaces
//!
//! `turn_end_waits_for_subagents` (`runtime.rs:3826`) is today's version of
//! this wait: `Liveness::busy()` — `parent_running || !outstanding.is_empty()`
//! — read off an **in-memory, per-process** registry, which is why
//! `managed-subagents.md` §8.12 says *"'My turn is not over until my children
//! are done' becomes a suspension, not a poll."* [`outstanding`] is that
//! suspension: the same question, answered from the immutable ledger, so it
//! survives a restart and a re-adoption. This file does not delete the config
//! flag — nothing here is wired into `runtime.rs` yet (§7) — it provides the
//! derivation that makes deleting it correct.
//!
//! ## General over both subagent models, because the `child` event already is
//!
//! `proto-ledger.md` §5 keeps two cases alive deliberately, and the operator
//! asked to *"stay general enough for both"*:
//!
//! | | a child **inside** the parent's thread | a child **as its own thread** |
//! |---|---|---|
//! | its events | the parent's ledger, `within = Some(tool_use_id)` | its own ledger, `within = None` |
//! | `child_id` means | an agent id | a thread id |
//! | `ThreadMeta.parent_id` | not involved | set |
//!
//! Nothing in this file has to branch on which. [`outstanding`] folds `child`
//! events by `child_id` and never reads `within`, and the adapter already
//! writes exactly those events for the in-thread case — `task_started` →
//! `child{started}` and `task_notification` → `child{finished}`
//! (`claude_code.rs:148`, `:157`). **What is tested, and what is not:** the
//! fold is tested on events produced BY that adapter (`claude_code::translate`
//! over wire lines in the CLI's own `task_started` / `task_notification`
//! shapes — `the_fold_is_the_same_for_a_child_inside_and_a_child_as_a_thread`).
//! The archive census COUNTED 8,471 such `child` events (`proto-ledger.md`
//! §6.1); no test folds that archive, so that number is the size of the
//! in-thread model's history, not a measurement of this function against it
//! (R4.S2 corrected an earlier sentence here that said it was). The migration
//! to threads-of-their-own changes what `child_id` denotes and not one line
//! here. [`spawn`] is the half that only
//! the second model uses; [`outstanding`] and [`report`] serve both.
//!
//! ## Budget: one key, and the sharing was already built
//!
//! *"A child always inherits its parent's key. No spawn-time override."* If the
//! budget is exhausted the child hangs and the parent hangs too.
//!
//! This file does one thing for that — [`Spawned::budget_key`] is the parent's,
//! copied, and a request that names its own is **refused**
//! ([`SpawnRefused::BudgetKeyNamed`]) rather than ignored. Everything else
//! already exists and needed no change, which is worth stating precisely
//! because it is easy to mistake for work:
//!
//! * `ThreadSettings::budget_key` is per-thread with mandatory default
//!   inheritance (`settings.rs:252`), and its own doc says *"several threads
//!   may share one key (that is the point of keying)"*.
//! * The meter **aggregates by key and never filters by thread**
//!   (`store.rs:4038`: `WHERE budget_key=?1 AND axis=?2 AND account=?3 AND ts >= ?4`).
//!   So two threads on one key already sum into one `Meter.consumed`, and one
//!   `check_trip` answer already serves both.
//! * "Hang" is spelled `Admission::Park { cause: ParkCause::Budget }`
//!   (`budgets.rs:5440`) — CON-4, *park, never drop*.
//!
//! So "both hang on exhaustion" is a **consequence of key equality**, not a
//! mechanism, and the only thing this file can get wrong is the copy. That is
//! what [`spawn`] is tested on.
//!
//! **The copy is a snapshot, so the RE-POINT has to follow it (N-2).** The
//! operator's move when a key runs out is to re-point the parent
//! (`threads/{thread_id}/budget_assign`). Re-pointing only the parent left
//! every child parked on the spent key and the parent waiting on them
//! forever. `crate::managed::repoint_descendants` closes that: the re-point
//! walks the parent's committed descendants and moves every one still on the
//! parent's OLD key, re-running admission, so a parked child goes out under
//! the new key with no restart (`tests/child_key_follows_parent.rs`). A
//! descendant someone pointed elsewhere on purpose keeps its key, and its
//! subtree follows it.
//!
//! **Two asymmetries the sharing does not remove, named so they are not
//! discovered later.** (1) A shared key makes parent and child park together on
//! all six *key-level* causes — `Disabled`, `UnknownKey`, `Scope`, `Budget` and
//! the rest — not only on an exhausted meter. (2) The mid-turn kill latch
//! (`runtime.rs:198`, `budget_kill_latch`) is **per-`ThreadState`, not
//! per-key**: a kill that lands on the parent latches only the parent, which
//! then stays parked until the operator moves the limit (`runtime.rs:4498`),
//! while the child parks at the gate instead and un-parks by itself on the next
//! window rollover. So on the kill path the two do *not* hang alike, and
//! nothing in this file can make them — the latch would have to become
//! per-key.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use super::event::{CycleFound, Draft, Kind, Ledger, ThreadMeta};

/// **THE DEPTH CAP** — `managed-subagents.md` §1.3, `max_agent_depth = 3`, root
/// at 0. A spawn whose child would land past it is refused.
///
/// It is a **policy, not a cycle guard**, and the distinction is the whole
/// reason [`ThreadMeta::depth`] no longer takes a `limit`. A cycle is
/// unconstructible (see [`ThreadMeta`]), so nothing here is defending against
/// one; this number exists because of the operator's *"strictly just to prevent
/// a thousand depth subagents."* Conflating the two would have produced a guard
/// that looks like it prevents cycles and actually only truncates counting.
pub const MAX_DEPTH: u32 = 3;

/// **THE ONE WAKE REASON THIS FILE ADDS.** Handed to
/// `ThreadManager::placement_wake` (`runtime.rs:10445`), which rings the
/// placement bell without running a pass inline. It joins the sixteen reasons
/// that exist (`"limit_expired"`, `"usage_observed"`, `"pool_reloaded"`, …) and
/// is the only name this design needs in that vocabulary.
pub const WAKE_REASON: &str = "child_reported";

/// How a child's run came out. The three words are the `outcome` enum the event
/// manifest already declares (`event.rs`'s `LEDGER_EVENT_CELLS`), not a new
/// vocabulary — `managed-subagents.md` §1.2's *"machine status"* only, with no
/// word an agent asserts about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Failed,
    /// Cut off by something nobody asked for (a daemon restart, a compaction
    /// stop): the run ended without its own result.
    Interrupted,
    /// The operator (or an `interrupt` mail) cancelled the run (H179,
    /// `docs/SUBAGENT-WAKE.md`): not a failure, and the parent is told so.
    Cancelled,
}

impl Outcome {
    pub fn word(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Failed => "failed",
            Outcome::Interrupted => "interrupted",
            Outcome::Cancelled => "cancelled",
        }
    }

    /// The inverse of [`Outcome::word`]; `None` for a word it never wrote.
    pub fn from_word(w: &str) -> Option<Outcome> {
        Some(match w {
            "ok" => Outcome::Ok,
            "failed" => Outcome::Failed,
            "interrupted" => Outcome::Interrupted,
            "cancelled" => Outcome::Cancelled,
            _ => return None,
        })
    }
}

/// **WHAT A PARENT ASKS FOR.** Deliberately not a launch: there is no model, no
/// `--agent`, no process shape here. `agent` is an opaque passthrough
/// (`managed-subagents.md` §1.5 — threads never resolves or validates it) and
/// rides in `detail`.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// **THE SPAWN'S IDENTITY**, and what makes it idempotent. Claude's own
    /// per-call `toolUseId` in practice. It becomes the id of the parent's
    /// `child{started}` event, so a retried spawn is a replay the ledger
    /// already refuses by id.
    pub request_id: String,
    /// The child thread's id, minted by the caller.
    pub child_id: String,
    /// The turn's text, as it will be delivered. This is the whole of "sending
    /// in standard input".
    pub text: String,
    /// The parent's `tool_use` id when the parent called a tool to get here.
    /// Carried through to the `child` event so the in-thread model's events and
    /// this one's are the same shape.
    pub tool_use_id: Option<String>,
    /// Opaque passthrough: description, agent type, whatever the caller sends.
    pub detail: Value,
    /// **MUST BE `None`.** Present only so that naming a key is *refused*
    /// rather than silently ignored — `managed-subagents.md` §1.4: *"Ignoring a
    /// parameter the caller meant is the silent-divergence class this codebase
    /// spent a stage deleting."*
    pub budget_key: Option<String>,
}

impl SpawnRequest {
    pub fn new(request_id: impl Into<String>, child_id: impl Into<String>, text: impl Into<String>) -> Self {
        SpawnRequest {
            request_id: request_id.into(),
            child_id: child_id.into(),
            text: text.into(),
            tool_use_id: None,
            detail: Value::Null,
            budget_key: None,
        }
    }
    pub fn tool_use(mut self, id: impl Into<String>) -> Self {
        self.tool_use_id = Some(id.into());
        self
    }
    pub fn detail(mut self, d: Value) -> Self {
        self.detail = d;
        self
    }
}

/// **WHY A SPAWN DID NOT HAPPEN.** Each one is a refusal, not a degradation:
/// nothing here half-creates a child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnRefused {
    /// The child would be deeper than [`MAX_DEPTH`].
    DepthCap {
        parent: String,
        parent_depth: u32,
        max: u32,
    },
    /// The request named a `budget_key`. A child always inherits
    /// (`managed-subagents.md` §1.4), so the parameter has no meaning and is
    /// refused by the name the spec gives it, `unknown_param`.
    BudgetKeyNamed { named: String },
    /// The parent's own depth could not be established because the store it was
    /// read from holds a cycle. Construction cannot build one, so this is a
    /// corrupt store, and a spawn onto it is refused rather than guessed at.
    ParentCycle(CycleFound),
    /// The `request_id` is already the id of an event in the parent's ledger
    /// that is NOT a `child{started}` — an operator prompt, a report, anything.
    /// It cannot be a replay of a spawn (no spawn wrote it), and it cannot be a
    /// new spawn (the ledger refuses a second event under one id). Refused by
    /// name; before R4.S2 this reached an `expect` and panicked the caller.
    RequestIdTaken { request_id: String, kind: &'static str },
}

impl std::fmt::Display for SpawnRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnRefused::DepthCap {
                parent,
                parent_depth,
                max,
            } => write!(
                f,
                "thread {parent} is at depth {parent_depth}; a child of it would be at depth {} \
                 and the cap is {max}",
                parent_depth + 1
            ),
            SpawnRefused::BudgetKeyNamed { named } => write!(
                f,
                "unknown_param: budget_key — a child always inherits its parent's key, so \
                 `{named}` cannot be honoured and is refused rather than ignored"
            ),
            SpawnRefused::ParentCycle(c) => write!(f, "{c}"),
            SpawnRefused::RequestIdTaken { request_id, kind } => write!(
                f,
                "request_id `{request_id}` is already the id of a `{kind}` event in the parent's \
                 ledger, not of a spawn; a spawn's request_id must be unique to it"
            ),
        }
    }
}

/// **WHAT A SPAWN DID.** `Created` carries the child's ledger with its turn
/// already in it; `Already` is the idempotent answer.
#[derive(Debug, Clone)]
pub enum Spawned {
    Created {
        /// The child's ledger: one `user` event, which is the turn placement
        /// will run. Not written to disk here — the caller owns storage.
        child: Ledger,
        /// The parent's key, copied. Equality with the parent's is the whole of
        /// budget sharing.
        budget_key: Option<String>,
    },
    /// This `request_id` already spawned; here is the child it made. Returned
    /// instead of minting a second thread, so a retried tool call is free.
    Already { child_id: String },
}

impl Spawned {
    pub fn child_id(&self) -> &str {
        match self {
            Spawned::Created { child, .. } => child.thread_id(),
            Spawned::Already { child_id } => child_id,
        }
    }
}

/// **A PARENT CAUSES A CHILD THREAD TO EXIST.** No process is launched, here or
/// anywhere this function reaches.
///
/// `parent_budget_key` is the parent's key as its settings hold it;
/// `parent_lookup` answers a `ThreadMeta` by id, for the depth walk. The acts,
/// in order, all of them appends:
///
/// 1. Refuse a request that named a `budget_key`.
/// 2. Establish the parent's depth; refuse past [`MAX_DEPTH`].
/// 3. If `request_id` is already a `child{started}` in the parent, answer
///    [`Spawned::Already`] with the child id that event names.
/// 4. Build the child's [`ThreadMeta`] with [`ThreadMeta::child_of`] — which is
///    why no cycle can result — and append the request text as one `user`
///    event.
/// 5. Append `child{started}` to the parent under `request_id`.
pub fn spawn(
    parent: &mut Ledger,
    parent_budget_key: Option<&str>,
    parent_lookup: impl Fn(&str) -> Option<ThreadMeta>,
    req: &SpawnRequest,
    now_ts: &str,
) -> Result<Spawned, SpawnRefused> {
    if let Some(named) = &req.budget_key {
        return Err(SpawnRefused::BudgetKeyNamed {
            named: named.clone(),
        });
    }
    let parent_depth = parent
        .meta()
        .depth(parent_lookup)
        .map_err(SpawnRefused::ParentCycle)?;
    if parent_depth + 1 > MAX_DEPTH {
        return Err(SpawnRefused::DepthCap {
            parent: parent.thread_id().to_string(),
            parent_depth,
            max: MAX_DEPTH,
        });
    }
    // Idempotence, straight off the immutable ledger: the first answer is
    // permanent, so there is nothing to derive and nothing to cache.
    //
    // ONLY a `child{started}` is a replay. Any other event under this id is a
    // collision, refused by name — the earlier `if let` fell through here and
    // the append below then met its own replay refusal inside an `expect`.
    if let Some(e) = parent.events().iter().find(|e| e.id == req.request_id) {
        let started = e.kind == Kind::Child
            && e.body.get("phase").and_then(Value::as_str) == Some("started");
        return match e.body.get("child_id").and_then(Value::as_str) {
            Some(id) if started => Ok(Spawned::Already {
                child_id: id.to_string(),
            }),
            _ => Err(SpawnRefused::RequestIdTaken {
                request_id: req.request_id.clone(),
                kind: e.kind.word(),
            }),
        };
    }
    let meta = ThreadMeta::child_of(parent.meta(), req.child_id.clone(), now_ts);
    let mut child = Ledger::new(meta);
    child
        .append(Draft::new(
            Kind::User,
            now_ts,
            json!({"text": req.text, "intent": "immediate"}),
        ))
        .expect("a minted id is never a replay");
    parent
        .append(
            Draft::new(
                Kind::Child,
                now_ts,
                json!({
                    "phase": "started",
                    "child_id": req.child_id,
                    "tool_use_id": req.tool_use_id,
                    "detail": req.detail,
                }),
            )
            .with_id(req.request_id.clone()),
        )
        .expect("every event already under this id was answered above");
    Ok(Spawned::Created {
        child,
        budget_key: parent_budget_key.map(str::to_string),
    })
}

/// **WHAT A PARENT IS STILL OWED** — the `child_id`s it started and has not
/// seen finish, in id order. A fold over the parent's own immutable ledger; no
/// column, no table, no in-memory registry.
///
/// **It never reads `within`**, which is what makes it serve both subagent
/// models: a child running inside the parent's thread and a child running as
/// its own thread produce the same two `child` events, differing only in what
/// `child_id` denotes.
///
/// A `child{started}` whose `detail.wake` is `"never"` is not owed: the
/// parent asked not to be woken for it (`agents_spawn`'s `wake: never`), so it
/// neither keeps the parent waiting nor counts toward the report that wakes it.
///
/// A `child{finished}` for an id that never started is ignored rather than
/// counted negative — the ledger is a record of observations, and an
/// unmatched finish is an observation about something this ledger did not see
/// start.
pub fn outstanding(parent: &Ledger) -> BTreeSet<String> {
    let mut started: BTreeSet<String> = BTreeSet::new();
    let mut finished: BTreeSet<String> = BTreeSet::new();
    for e in parent.events() {
        if e.kind != Kind::Child {
            continue;
        }
        let Some(id) = e.body.get("child_id").and_then(Value::as_str) else {
            continue;
        };
        match e.body.get("phase").and_then(Value::as_str) {
            Some("started") => {
                if e.body.pointer("/detail/wake").and_then(Value::as_str) != Some("never") {
                    started.insert(id.to_string());
                }
            }
            Some("finished") => {
                finished.insert(id.to_string());
            }
            _ => {}
        }
    }
    started.difference(&finished).cloned().collect()
}

/// **EVERY CHILD THE PARENT STARTED AND HAS NOT RECORDED FINISHED** (R10.H1,
/// TC-13), WHATEVER ITS WAKE MODE — the superset of [`outstanding`]. A
/// `wake: never` child is in this set and not in that one: the parent is not
/// WAITING for it, but its run still ends, and that end is recorded
/// ([`record_finished`]) so `children.runs[].finished` is never null forever.
/// "Is a wake owed" stays [`outstanding`]'s question alone.
pub fn unfinished(parent: &Ledger) -> BTreeSet<String> {
    let mut started: BTreeSet<String> = BTreeSet::new();
    let mut finished: BTreeSet<String> = BTreeSet::new();
    for e in parent.events() {
        if e.kind != Kind::Child {
            continue;
        }
        let Some(id) = e.body.get("child_id").and_then(Value::as_str) else {
            continue;
        };
        match e.body.get("phase").and_then(Value::as_str) {
            Some("started") => {
                started.insert(id.to_string());
            }
            Some("finished") => {
                finished.insert(id.to_string());
            }
            _ => {}
        }
    }
    started.difference(&finished).cloned().collect()
}

/// The `detail.wake` a `child{started}` for `child_id` carried (`"end"` when
/// it named none, as `children_row` reads it); `None` when this ledger never
/// started that child.
pub fn wake_of(parent: &Ledger, child_id: &str) -> Option<String> {
    parent.events().iter().find_map(|e| {
        (e.kind == Kind::Child
            && e.body.get("phase").and_then(Value::as_str) == Some("started")
            && e.body.get("child_id").and_then(Value::as_str) == Some(child_id))
        .then(|| e.body.pointer("/detail/wake").and_then(Value::as_str).unwrap_or("end").to_string())
    })
}

/// **RECORD A CHILD'S END WITHOUT TELLING THE PARENT** (R10.H1, F2): appends
/// `child{finished}` only — no `context` words, so nothing reaches the
/// parent's model, and no [`Wake`]. The path a `wake: never` child's end takes
/// ([`report`] is the one for a child the parent is owed). Idempotent: `false`
/// (nothing appended) when the child was never started here or is already
/// recorded finished.
pub fn record_finished(parent: &mut Ledger, child_id: &str, outcome: Outcome, now_ts: &str) -> bool {
    if !unfinished(parent).contains(child_id) {
        return false;
    }
    parent
        .append(Draft::new(
            Kind::Child,
            now_ts,
            json!({
                "phase": "finished",
                "child_id": child_id,
                "outcome": outcome.word(),
            }),
        ))
        .expect("a minted id is never a replay");
    true
}

/// **THE PARENT IS OWED NOTHING AND MAY END ITS TURN.** The predicate the
/// turn-end path asks, replacing `Liveness::busy()`'s `!outstanding.is_empty()`
/// (`runtime.rs:3826`) with the same question answered durably.
pub fn is_waiting(parent: &Ledger) -> bool {
    !outstanding(parent).is_empty()
}

/// **THE RING.** Returned by [`report`] only when a child's report emptied the
/// parent's outstanding set. The daemon's whole obligation is
/// `manager.placement_wake(wake.reason)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wake {
    /// The thread to wake — the parent.
    pub thread_id: String,
    /// Always [`WAKE_REASON`]; a field rather than a constant at the call site
    /// so a log line can carry it without re-deriving it.
    pub reason: &'static str,
    /// The child whose report emptied the set.
    pub last_child: String,
}

/// What a report did to the parent.
#[derive(Debug, Clone, PartialEq)]
pub struct Reported {
    /// What the parent is owed *after* this report.
    pub outstanding: BTreeSet<String>,
    /// `Some` exactly when `outstanding` is now empty — so a parent fanned out
    /// over five children is woken once, by the fifth, and not five times.
    pub wake: Option<Wake>,
    /// `true` when this `child_id` was not outstanding: a duplicate report, or
    /// a child this ledger never saw start. Recorded, not refused.
    pub unmatched: bool,
}

/// **A CHILD REPORTS, AND THE PARENT BECOMES RUNNABLE.** Appends two events to
/// the parent and answers whether to ring the bell.
///
/// The child's words land as **`context`**, not `user`, and that is load-bearing
/// rather than tidy. `managed-subagents.md` §8.8 names the hole it closes:
/// *"Mail is indistinguishable from the operator's own words"* — `queue` has no
/// `origin` and no `sender_thread_id`, so CON-4's *park, never drop*, which
/// exists for **his prompts**, would extend to agent chatter by accident. In
/// the ledger the two are already different kinds: `user` is *"words put into
/// the thread by the operator or a client"*, `context` is *"the daemon told the
/// model something that is not a user message"*. A child's report is the
/// second. So the distinction costs nothing and is a property of the record.
pub fn report(
    parent: &mut Ledger,
    child_id: &str,
    outcome: Outcome,
    text: &str,
    now_ts: &str,
) -> Reported {
    let unmatched = !outstanding(parent).contains(child_id);
    parent
        .append(Draft::new(
            Kind::Child,
            now_ts,
            json!({
                "phase": "finished",
                "child_id": child_id,
                "outcome": outcome.word(),
            }),
        ))
        .expect("a minted id is never a replay");
    parent
        .append(Draft::new(Kind::Context, now_ts, json!({"text": text})))
        .expect("a minted id is never a replay");
    let left = outstanding(parent);
    let wake = if left.is_empty() && !unmatched {
        Some(Wake {
            thread_id: parent.thread_id().to_string(),
            reason: WAKE_REASON,
            last_child: child_id.to_string(),
        })
    } else {
        None
    };
    Reported {
        outstanding: left,
        wake,
        unmatched,
    }
}

/// The largest structured value printed inline in a report (§3.5): above it
/// the element carries `ref="body:<event seq>"` and the parent fetches the
/// value with `agents_result`.
pub const STRUCTURED_INLINE_MAX: usize = 8 * 1024;

/// **THE `<structured_output>` ELEMENT** (R11.J5, `docs/STRUCTURED-OUTPUT.md`
/// §3.5) that a report's words carry inside today's `<agent_report>`, from a
/// run's structured result `{for_message, status, value?, errors?, event_seq?}`:
///
/// * `ok` — `<structured_output for_message="3" status="ok">{compact}</structured_output>`
/// * `unmet` — `status="unmet" errors="…"`, self-closing, or wrapping the
///   invalid value when there was one;
/// * a value above [`STRUCTURED_INLINE_MAX`] is replaced by `ref="body:<seq>"`.
///
/// Attribute text is XML-escaped. The JSON body is compact, with any `<` or
/// `&` written as a JSON `\u` escape ([`esc_body`]), so a value can never close
/// the element or the `<agent_report>` around it and is still valid JSON.
pub fn structured_element(s: &serde_json::Value) -> String {
    fn esc(t: &str) -> String {
        t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }
    let for_message = match &s["for_message"] {
        serde_json::Value::Null => String::new(),
        v => format!(" for_message=\"{}\"", esc(&v.to_string().trim_matches('"').to_string())),
    };
    let status = s["status"].as_str().unwrap_or("unmet");
    let mut attrs = format!("{for_message} status=\"{}\"", esc(status));
    if status != "ok" {
        let errs: Vec<String> = s["errors"]
            .as_array()
            .map(|a| a.iter().map(|e| e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string())).collect())
            .unwrap_or_default();
        attrs.push_str(&format!(" errors=\"{}\"", esc(&errs.join("; "))));
    }
    let Some(value) = s.get("value").filter(|v| !v.is_null()) else {
        return format!("<structured_output{attrs}/>");
    };
    let compact = value.to_string();
    if compact.len() > STRUCTURED_INLINE_MAX {
        let at = s["event_seq"].as_u64().map(|q| q.to_string()).unwrap_or_default();
        return format!("<structured_output{attrs} ref=\"body:{at}\"/>");
    }
    format!("<structured_output{attrs}>{}</structured_output>", esc_body(&compact))
}

/// A compact JSON body inside an element: only `<` and `&` can end or confuse
/// the element, and JSON lets both be written as `\u` escapes inside strings
/// (the only place they can occur), so the body stays valid JSON.
fn esc_body(t: &str) -> String {
    t.replace('&', "\\u0026").replace('<', "\\u003c")
}

/// **THE SUBTREE, FOR THE ONE SUBSCRIPTION A PARENT NEEDS.**
/// `managed-subagents.md` §8.9: *"a parent needs **one** subscription carrying
/// its whole subtree's run states"*, and the same section's other half is that
/// `threads/list` must filter to **roots at the producer**. Both questions are
/// the same walk, so it is here once: ids keyed by parent, depth-first from
/// `root`, and it terminates because the graph is a forest.
///
/// `children_of` answers a thread's immediate children. A store that holds a
/// cycle (deserialized, never constructed) is reported rather than looped on.
pub fn subtree(
    root: &str,
    children_of: impl Fn(&str) -> Vec<String>,
) -> Result<BTreeMap<String, Vec<String>>, CycleFound> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut walked: Vec<String> = Vec::new();
    let mut stack = vec![root.to_string()];
    seen.insert(root.to_string());
    while let Some(id) = stack.pop() {
        walked.push(id.clone());
        let kids = children_of(&id);
        for k in &kids {
            if !seen.insert(k.clone()) {
                return Err(CycleFound {
                    from: root.to_string(),
                    at: k.clone(),
                    walked,
                });
            }
            stack.push(k.clone());
        }
        out.insert(id, kids);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::claude_code::RUNTIME;

    fn root(id: &str) -> ThreadMeta {
        ThreadMeta::root(id, RUNTIME, "2026-10-09T00:00:00Z")
    }

    /// A parent ledger with one operator prompt in it.
    fn parent(id: &str) -> Ledger {
        let mut l = Ledger::new(root(id));
        l.append(Draft::new(Kind::User, "2026-10-09T00:00:01Z", json!({"text": "fan out"})))
            .unwrap();
        l
    }

    const NOW: &str = "2026-10-09T00:00:02Z";

    /// **A SPAWN CREATES A THREAD WITH A TURN IN IT, SETS `parent_id`, AND
    /// LAUNCHES NOTHING.** The child's ledger holds exactly one `user` event —
    /// the text, which is what placement will run — and its meta names the
    /// parent. The parent's ledger holds `child{started}` under the request id.
    ///
    /// **THE MUTATION.** In `spawn`, drop the child's `user` append.
    ///
    /// Forced red:
    /// ```text
    /// the child's turn is already in its ledger, waiting for placement
    /// ```
    #[test]
    fn a_spawn_is_a_thread_with_a_turn_appended_and_no_process() {
        let mut p = parent("p");
        let req = SpawnRequest::new("toolu_01", "c1", "do the thing").tool_use("toolu_01");
        let out = spawn(&mut p, Some("aui:default"), |_| None, &req, NOW).unwrap();
        let Spawned::Created { child, budget_key } = &out else {
            panic!("a first spawn creates; got {out:?}")
        };
        assert_eq!(child.thread_id(), "c1");
        assert_eq!(child.meta().parent_id(), Some("p"), "the child names its parent");
        assert_eq!(child.meta().runtime, RUNTIME, "a child is exactly its parent but for the parent id");
        assert_eq!(
            child.events().len(),
            1,
            "the child's turn is already in its ledger, waiting for placement"
        );
        assert_eq!(child.events()[0].kind, Kind::User);
        assert_eq!(child.events()[0].body["text"], "do the thing");
        assert_eq!(child.events()[0].message_id, Some(1), "the turn is addressable");
        assert_eq!(budget_key.as_deref(), Some("aui:default"));

        let started = p.events().last().unwrap();
        assert_eq!(started.kind, Kind::Child);
        assert_eq!(started.id, "toolu_01", "the request id IS the event id");
        assert_eq!(started.body["phase"], "started");
        assert_eq!(started.body["child_id"], "c1");
        assert_eq!(outstanding(&p), BTreeSet::from(["c1".to_string()]));
        assert!(is_waiting(&p));
    }

    /// **A RETRIED SPAWN IS NOT A SECOND THREAD.** Idempotence comes off the
    /// immutable ledger — the `request_id` is the `child{started}` event's id,
    /// so the second call finds the first answer and returns it. No uuid5, no
    /// derivation, no second child.
    ///
    /// **THE MUTATION.** In `spawn`, delete the `find(|e| e.id == req.request_id)`
    /// early return.
    ///
    /// Forced red:
    /// ```text
    /// a retried spawn answers with the child that already exists
    /// ```
    #[test]
    fn a_retried_spawn_answers_the_child_that_already_exists() {
        let mut p = parent("p");
        let req = SpawnRequest::new("toolu_01", "c1", "do the thing");
        let first = spawn(&mut p, None, |_| None, &req, NOW).unwrap();
        assert!(matches!(first, Spawned::Created { .. }));
        let before = p.events().len();
        // The same call again — a different child_id even, as a retry that
        // minted a fresh one would send.
        let retry = SpawnRequest::new("toolu_01", "c2-minted-again", "do the thing");
        let second = spawn(&mut p, None, |_| None, &retry, NOW).unwrap();
        assert_eq!(
            second.child_id(),
            "c1",
            "a retried spawn answers with the child that already exists"
        );
        assert!(matches!(second, Spawned::Already { .. }));
        assert_eq!(p.events().len(), before, "and appends nothing");
        assert_eq!(outstanding(&p).len(), 1, "and does not double the debt");

        // THE OPPOSED CASE (R4.S2): a constant "always Already" passes every
        // assertion above. A DIFFERENT request id is a second child.
        let other = SpawnRequest::new("toolu_02", "c2", "another thing");
        let third = spawn(&mut p, None, |_| None, &other, NOW).unwrap();
        assert!(
            matches!(third, Spawned::Created { .. }),
            "a different request id spawns a second child: {third:?}"
        );
        assert_eq!(outstanding(&p), BTreeSet::from(["c1".to_string(), "c2".to_string()]));

        // And the answer survives the FILE FORM — the ledger the daemon keeps
        // (`crate::managed`) — read back into a fresh value: the retry finds
        // the first child in what was read, not in what was held.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.jsonl");
        p.write_jsonl(&path).unwrap();
        let mut back = Ledger::read_jsonl(&path).unwrap();
        let again = spawn(&mut back, None, |_| None, &retry, NOW).unwrap();
        assert_eq!(again.child_id(), "c1", "the read-back ledger answers the first child");
    }

    /// **A REQUEST ID THAT NAMES A NON-SPAWN EVENT IS REFUSED, NOT A PANIC.**
    /// The `af77` inspection's finding: `spawn` looked the id up, found an
    /// event with no `child_id` (here: the operator's own prompt), fell
    /// through, and the `child{started}` append then hit the ledger's replay
    /// refusal inside an `expect` — a panic in the daemon on a caller-supplied
    /// string. A `child{finished}` (which DOES carry a `child_id`) under the id
    /// is the second shape: the old code answered it as `Already`, naming a
    /// child no spawn under that id ever made.
    ///
    /// **THE MUTATION.** Restore the old lookup (`if let Some(id) =
    /// e.body.get("child_id")… { return Ok(Already) }`, fall through
    /// otherwise).
    ///
    /// Forced red: see the R4.S2 report — the first case panics with
    /// `every event already under this id was answered above: Replay`.
    #[test]
    fn a_request_id_naming_a_non_spawn_event_is_refused_not_a_panic() {
        let mut p = parent("p");
        let prompt_id = p.events()[0].id.clone();
        let req = SpawnRequest::new(prompt_id.clone(), "c1", "x");
        let err = spawn(&mut p, None, |_| None, &req, NOW)
            .expect_err("an id that is the operator's prompt is not a spawn");
        assert_eq!(
            err,
            SpawnRefused::RequestIdTaken { request_id: prompt_id, kind: "user" }
        );
        assert_eq!(p.events().len(), 1, "a refused spawn appends nothing");

        // A `child{finished}` under the id carries a child_id and is still not
        // a spawn.
        let mut q = parent("q");
        q.append(
            Draft::new(Kind::Child, NOW, json!({"phase": "finished", "child_id": "ghost"}))
                .with_id("fin-1"),
        )
        .unwrap();
        let err = spawn(&mut q, None, |_| None, &SpawnRequest::new("fin-1", "c1", "x"), NOW)
            .expect_err("a finish is not a spawn");
        assert!(matches!(err, SpawnRefused::RequestIdTaken { kind: "child", .. }), "{err:?}");
        // Opposed: the spawn's own id still answers Already.
        spawn(&mut q, None, |_| None, &SpawnRequest::new("s-1", "c9", "x"), NOW).unwrap();
        let again = spawn(&mut q, None, |_| None, &SpawnRequest::new("s-1", "c10", "x"), NOW).unwrap();
        assert_eq!(again.child_id(), "c9");
    }

    /// **A SPAWN ONTO A STORE THAT HOLDS A CYCLE IS REFUSED, NAMING IT.** The
    /// `ParentCycle` arm was untested (`af77` inspection). Construction cannot
    /// build a cycle, so the only door is the one the type does not own —
    /// deserialization — and that is exactly how this reaches it: two metas
    /// read from JSON, each naming the other as parent.
    ///
    /// **THE MUTATION.** In `spawn`, `.depth(parent_lookup).unwrap_or(0)`
    /// instead of `.map_err(SpawnRefused::ParentCycle)?`.
    ///
    /// Forced red: the spawn is CREATED on the corrupt store.
    #[test]
    fn a_spawn_onto_a_cyclic_store_is_refused_naming_the_cycle() {
        let a: ThreadMeta = serde_json::from_value(
            json!({"id": "a", "parent_id": "b", "runtime": RUNTIME, "created_at": ""}),
        )
        .unwrap();
        let b: ThreadMeta = serde_json::from_value(
            json!({"id": "b", "parent_id": "a", "runtime": RUNTIME, "created_at": ""}),
        )
        .unwrap();
        let store = vec![a.clone(), b];
        let lookup = |id: &str| store.iter().find(|m| m.id == id).cloned();
        let mut p = Ledger::new(a);
        let err = spawn(&mut p, None, lookup, &SpawnRequest::new("t", "c", "x"), NOW)
            .expect_err("a spawn onto a cyclic store is refused");
        let SpawnRefused::ParentCycle(c) = &err else { panic!("ParentCycle, got {err:?}") };
        assert_eq!((c.from.as_str(), c.at.as_str()), ("a", "a"));
        assert_eq!(c.walked, ["a", "b"]);
        assert!(err.to_string().contains("a -> b"), "the refusal names the cycle: {err}");
        assert!(p.is_empty(), "a refused spawn appends nothing");

        // Opposed: the same shape WITHOUT the back edge is a legal chain and
        // spawns.
        let a2: ThreadMeta = serde_json::from_value(
            json!({"id": "a", "parent_id": "b", "runtime": RUNTIME, "created_at": ""}),
        )
        .unwrap();
        let b2 = root("b");
        let store2 = vec![a2.clone(), b2];
        let lookup2 = |id: &str| store2.iter().find(|m| m.id == id).cloned();
        let mut p2 = Ledger::new(a2);
        assert!(spawn(&mut p2, None, lookup2, &SpawnRequest::new("t", "c", "x"), NOW).is_ok());
    }

    /// **`wake: never` IS NOT OWED.** A child the parent asked not to be woken
    /// for neither keeps it waiting nor counts toward the wake.
    ///
    /// **THE MUTATION.** Drop the `detail.wake` check in `outstanding`.
    #[test]
    fn a_child_spawned_with_wake_never_is_not_owed() {
        let mut p = parent("p");
        spawn(&mut p, None, |_| None, &SpawnRequest::new("t1", "quiet", "x").detail(json!({"wake": "never"})), NOW).unwrap();
        assert!(outstanding(&p).is_empty(), "a wake:never child is not owed");
        spawn(&mut p, None, |_| None, &SpawnRequest::new("t2", "loud", "x").detail(json!({"wake": "end"})), NOW).unwrap();
        assert_eq!(outstanding(&p), BTreeSet::from(["loud".to_string()]), "a wake:end child is");
        let r = report(&mut p, "loud", Outcome::Ok, "done", NOW);
        assert!(r.wake.is_some(), "the parent is woken by its last OWED child");
    }

    /// **NAMING A BUDGET KEY IS REFUSED, NOT IGNORED.** A child always inherits
    /// (`managed-subagents.md` §1.4) and the refusal says `unknown_param`.
    ///
    /// **THE MUTATION.** In `spawn`, replace the refusal with
    /// `let _ = &req.budget_key;`.
    ///
    /// Forced red:
    /// ```text
    /// a spawn that names a budget key is refused
    /// ```
    #[test]
    fn a_spawn_may_not_name_its_own_budget_key() {
        let mut p = parent("p");
        let mut req = SpawnRequest::new("toolu_01", "c1", "x");
        req.budget_key = Some("someone:else".into());
        let err = spawn(&mut p, Some("aui:default"), |_| None, &req, NOW)
            .expect_err("a spawn that names a budget key is refused");
        assert_eq!(err, SpawnRefused::BudgetKeyNamed { named: "someone:else".into() });
        assert!(err.to_string().starts_with("unknown_param: budget_key"), "{err}");
        assert_eq!(p.events().len(), 1, "a refused spawn appends nothing");
    }

    /// **A CHILD SHARES ITS PARENT'S KEY, WHICH IS THE WHOLE OF "BOTH HANG".**
    /// Key equality is the mechanism: the meter aggregates by key and never
    /// filters by thread (`store.rs:4038`), so one `check_trip` answer parks
    /// both. An unkeyed parent makes an unkeyed child — inheritance copies
    /// `None` too, rather than reaching for the fleet default, because reaching
    /// would silently re-key a thread the operator deliberately unkeyed
    /// (`settings.rs:236`).
    ///
    /// **THE MUTATION.** In `spawn`, return
    /// `budget_key: Some(crate::budgets::DEFAULT_BUDGET_KEY.to_string())`.
    ///
    /// Forced red:
    /// ```text
    /// a deliberately unkeyed parent makes an unkeyed child
    /// ```
    #[test]
    fn a_child_shares_its_parents_budget_key_including_none() {
        let mut p = parent("p");
        for (parent_key, want) in [(Some("team:alpha"), Some("team:alpha")), (None, None)] {
            let req = SpawnRequest::new(format!("toolu_{:?}", parent_key), "c", "x");
            let out = spawn(&mut p, parent_key, |_| None, &req, NOW).unwrap();
            let Spawned::Created { budget_key, .. } = out else { panic!("created") };
            assert_eq!(
                budget_key.as_deref(),
                want,
                "a deliberately unkeyed parent makes an unkeyed child"
            );
        }
    }

    /// **THE DEPTH CAP REFUSES THE FOURTH LEVEL.** Root 0, child 1, grandchild
    /// 2, great-grandchild 3 — and a spawn from depth 3 is refused, naming both
    /// depths. The cap is policy; it is not what stops a cycle.
    ///
    /// **THE MUTATION.** In `spawn`, change the test to
    /// `parent_depth + 1 > MAX_DEPTH + 1`.
    ///
    /// Forced red:
    /// ```text
    /// a spawn from the deepest allowed thread is refused
    /// ```
    #[test]
    fn the_depth_cap_refuses_a_child_past_it() {
        // A real chain, built the only way it can be built.
        let r = root("r");
        let c1 = ThreadMeta::child_of(&r, "c1", "");
        let c2 = ThreadMeta::child_of(&c1, "c2", "");
        let c3 = ThreadMeta::child_of(&c2, "c3", "");
        let store = vec![r, c1, c2, c3];
        let lookup = |id: &str| store.iter().find(|m| m.id == id).cloned();

        // From depth 2, a child at 3 is allowed.
        let mut at2 = Ledger::new(store[2].clone());
        let ok = spawn(&mut at2, None, lookup, &SpawnRequest::new("t1", "x", "go"), NOW);
        assert!(matches!(ok, Ok(Spawned::Created { .. })), "depth 3 is inside the cap: {ok:?}");

        // From depth 3, a child at 4 is refused.
        let mut at3 = Ledger::new(store[3].clone());
        let err = spawn(&mut at3, None, lookup, &SpawnRequest::new("t2", "y", "go"), NOW)
            .expect_err("a spawn from the deepest allowed thread is refused");
        assert_eq!(
            err,
            SpawnRefused::DepthCap { parent: "c3".into(), parent_depth: 3, max: 3 }
        );
        assert_eq!(
            err.to_string(),
            "thread c3 is at depth 3; a child of it would be at depth 4 and the cap is 3"
        );
        assert_eq!(at3.events().len(), 0, "a refused spawn appends nothing");
    }

    /// **A PARENT FANNED OUT OVER THREE CHILDREN IS WOKEN ONCE, BY THE LAST.**
    /// This is how a parent waits, whole: it ends its turn; each report appends
    /// to its ledger; only the report that empties the outstanding set returns a
    /// `Wake`, and that `Wake` is one `placement_wake(WAKE_REASON)` call.
    ///
    /// **THE MUTATION.** In `report`, return the `Wake` unconditionally.
    ///
    /// Forced red:
    /// ```text
    /// a parent still owed two children is not woken
    /// ```
    #[test]
    fn only_the_last_child_to_report_wakes_the_parent() {
        let mut p = parent("p");
        for (i, c) in ["c1", "c2", "c3"].iter().enumerate() {
            let req = SpawnRequest::new(format!("toolu_{i}"), *c, "go");
            spawn(&mut p, Some("k"), |_| None, &req, NOW).unwrap();
        }
        assert_eq!(outstanding(&p).len(), 3);

        let r1 = report(&mut p, "c1", Outcome::Ok, "c1 done", NOW);
        assert_eq!(r1.wake, None, "a parent still owed two children is not woken");
        assert_eq!(r1.outstanding, BTreeSet::from(["c2".to_string(), "c3".to_string()]));
        assert!(!r1.unmatched);

        let r2 = report(&mut p, "c2", Outcome::Failed, "c2 blew up", NOW);
        assert_eq!(r2.wake, None, "nor one still owed one");

        let r3 = report(&mut p, "c3", Outcome::Interrupted, "c3 cut short", NOW);
        assert_eq!(
            r3.wake,
            Some(Wake {
                thread_id: "p".into(),
                reason: "child_reported",
                last_child: "c3".into()
            }),
            "the last report wakes the parent, exactly once"
        );
        assert!(r3.outstanding.is_empty());
        assert!(!is_waiting(&p));

        // A duplicate report is recorded and wakes nothing.
        let dup = report(&mut p, "c3", Outcome::Ok, "c3 again", NOW);
        assert!(dup.unmatched, "a second report for the same child is unmatched");
        assert_eq!(dup.wake, None, "and does not re-wake a parent that is owed nothing");

        // Every outcome word is the manifest's, and a child's words are
        // `context`, never `user` — §8.8.
        let kinds: Vec<Kind> = p.events().iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds.iter().filter(|k| **k == Kind::User).count(),
            1,
            "only the operator's prompt is a `user` event; a child's report is `context`"
        );
        let outcomes: Vec<&str> = p
            .events()
            .iter()
            .filter(|e| e.kind == Kind::Child && e.body["phase"] == "finished")
            .map(|e| e.body["outcome"].as_str().unwrap())
            .collect();
        assert_eq!(outcomes, ["ok", "failed", "interrupted", "ok"]);
        let ctx: Vec<&str> = p
            .events()
            .iter()
            .filter(|e| e.kind == Kind::Context)
            .map(|e| e.body["text"].as_str().unwrap())
            .collect();
        assert_eq!(ctx, ["c1 done", "c2 blew up", "c3 cut short", "c3 again"]);
    }

    /// **THE WAIT IS THE SAME FOLD FOR A CHILD INSIDE THE PARENT'S THREAD.**
    /// The in-thread model's events — the ones the adapter already writes from
    /// `task_started` / `task_notification`, and the ones the archive census
    /// counted 8,471 of — carry `within = Some(tool_use_id)` and are folded
    /// identically. `outstanding` never reads `within`, so the migration to
    /// subagents-as-their-own-threads changes nothing here.
    ///
    /// **THE MUTATION.** In `outstanding`, add
    /// `if e.within.is_some() { continue; }`.
    ///
    /// Forced red:
    /// ```text
    /// an in-thread child is outstanding on exactly the same fold
    /// ```
    #[test]
    fn the_fold_is_the_same_for_a_child_inside_and_a_child_as_a_thread() {
        let mut p = parent("p");
        // What the ADAPTER writes for the CLI's own `task_started` line — run
        // through `claude_code::translate`, not written by hand (R4.S2: the
        // hand-written event agreed with the fold by construction).
        let adapt = |p: &mut Ledger, line: Value| {
            let crate::ledger::claude_code::Outcome::Events(ds) = crate::ledger::claude_code::translate(&line, Some("cookie")) else {
                panic!("the adapter translates {line}")
            };
            for d in ds {
                p.append(d).unwrap();
            }
        };
        adapt(&mut p, json!({"type": "system", "subtype": "task_started", "uuid": "u-1",
            "task_id": "agent-aa8b9248b", "tool_use_id": "toolu_9", "parent_tool_use_id": "toolu_9",
            "description": "d", "subagent_type": "general-purpose"}));
        assert_eq!(p.events().last().unwrap().within.as_deref(), Some("toolu_9"), "an in-thread child");
        assert_eq!(
            outstanding(&p),
            BTreeSet::from(["agent-aa8b9248b".to_string()]),
            "an in-thread child is outstanding on exactly the same fold"
        );
        // …and one as its own thread, side by side in the same parent.
        spawn(&mut p, None, |_| None, &SpawnRequest::new("toolu_10", "c1", "go"), NOW).unwrap();
        assert_eq!(outstanding(&p).len(), 2, "both models coexist in one parent");

        // The CLI's own `task_notification`, through the adapter, closes it.
        adapt(&mut p, json!({"type": "system", "subtype": "task_notification", "uuid": "u-2",
            "task_id": "agent-aa8b9248b", "status": "completed", "summary": "s", "parent_tool_use_id": "toolu_9"}));
        assert_eq!(outstanding(&p), BTreeSet::from(["c1".to_string()]), "the adapter's finish closes the in-thread child");
        let r = report(&mut p, "c1", Outcome::Ok, "thread done", NOW);
        assert!(r.wake.is_some(), "the last of either kind wakes the parent");

        // A finish for a child this ledger never saw start is recorded, not counted.
        let mut q = parent("q");
        let r = report(&mut q, "never-started", Outcome::Ok, "?", NOW);
        assert!(r.unmatched);
        assert_eq!(r.wake, None, "an unmatched finish never wakes anyone");
        assert!(outstanding(&q).is_empty());
    }

    /// **THE SUBTREE WALK TERMINATES AND SERVES BOTH §8.9 QUESTIONS.**
    ///
    /// **THE MUTATION.** In `subtree`, drop the `seen.insert` cycle check.
    ///
    /// Forced red:
    /// ```text
    /// a forged cyclic store is reported, not looped on
    /// ```
    #[test]
    fn the_subtree_walk_is_one_walk_and_terminates() {
        let kids: BTreeMap<&str, Vec<String>> = BTreeMap::from([
            ("r", vec!["a".to_string(), "b".to_string()]),
            ("a", vec!["a1".to_string()]),
            ("b", vec![]),
            ("a1", vec![]),
        ]);
        let got = subtree("r", |id| kids.get(id).cloned().unwrap_or_default()).unwrap();
        assert_eq!(got.keys().collect::<Vec<_>>(), ["a", "a1", "b", "r"]);
        assert_eq!(got["r"], ["a", "b"]);
        assert_eq!(got["a"], ["a1"]);

        // Only a deserialized store can present this; it is reported.
        let cyc: BTreeMap<&str, Vec<String>> =
            BTreeMap::from([("x", vec!["y".to_string()]), ("y", vec!["x".to_string()])]);
        let err = subtree("x", |id| cyc.get(id).cloned().unwrap_or_default())
            .expect_err("a forged cyclic store is reported, not looped on");
        assert_eq!(err.at, "x");
    }
}
