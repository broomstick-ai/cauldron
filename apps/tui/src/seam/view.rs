//! `view.rs` — **THE READ VOCABULARY, AND THE ONLY THING A FACE READS.**
//!
//! Every function takes `&Held` (and a `now` where an age is derived) and
//! answers a [`Fact`]. It is a PROJECTION, not a cache: nothing in this file has
//! a field, so there is no second copy of anything and nothing here can be
//! stale relative to the mirror it reads.
//!
//! **No function here can issue a request.** None of them holds a `Link`, so
//! "the render path makes no RPCs" is a type-level fact rather than a rule
//! somebody has to remember. That is the structural half of the operator's
//! ruling: *we should remove the ability to request data* — above this file
//! there is no door to request it through.
//!
//! **It answers only about keys the book holds.** For a thread nobody asked
//! about, the honest answer is [`Holding::NotAsked`] and not `idle`; the old
//! `StatusTable` could only say `fresh: false` for the whole fleet at once,
//! which is why a daemon a face had not finished talking to looked exactly
//! like a daemon with nothing running.
//!
//! Divergence budget: **zero fenced lines in this file.** If a face needs a
//! different answer it reads a different function; it does not fork the
//! vocabulary.

use std::collections::BTreeMap;

use serde_json::Value;
use shapewire::mirror::{ContentError, Declaration, Fetch, Mirror};
use shapewire::surface::Carried;
use shapewire::wire::{CellState, Timestamp};

// `LOG`, `NARRATION`, `RECENT` and `SUMMARY` are no longer imported here: a cell
// token carries its own shape, so the four reads that named one of those consts
// beside a cell name now name the cell alone. The consts stay in `held.rs`, where
// `Book::standing` and the face's `hold`/`release` calls use them.
use super::held::{Fact, Held, Holding, LinkState, Settled, ACCOUNTS, HEARTBEAT, LINK, THREADS};
// **THE PRODUCER'S SURFACE, AS A BUILD INPUT.** Every cell this file reads, every
// field it gets out of one and every word it compares against is a const of this
// generated module. A name the producer does not declare has no const, so the
// failure is rustc's — `E0425: cannot find value ... in module` — at the read.
use super::inventory;

// ──────────────────────────────────────────────────────────────────────────
//  Small typed answers
// ──────────────────────────────────────────────────────────────────────────

/// The `status` cell's level: the daemon's own three words about one thread.
///
/// **There is no `pending()` on this type any more, and there is no `pending`
/// word on the wire.** The daemon's vocabulary is `active | queued | parked |
/// retrying | idle` — declared on `shapes.list` as `threads.status.state`'s
/// words and pinned to the derivation by the producer's own tests — and
/// `pending` was never in it. The predicate that compared against it answered
/// `false` for every status ever folded, for months; `seamdiff --inventory` now
/// refuses a word this file compares against that the producer does not
/// declare, so the next one is a red test and not a silent veto.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadStatus {
    pub state: String,
    /// **The daemon's own boolean, and the authority** — it is exactly "the
    /// execution lock is held". Declared `Required`: a status row without it
    /// is `NotHydrated`, never `false`.
    pub executing: bool,
    pub detail: Option<String>,
}

impl ThreadStatus {
    /// Is this status reporting an execution? **Byte-for-byte the predicate the
    /// firehose path used** (`status_frame_executing`): the boolean wins, and
    /// `state` is consulted only when it is absent, so a daemon that adds a
    /// state word later still lands somewhere sane instead of reading as idle.
    /// Both words are in the producer's declared vocabulary.
    pub fn running(&self) -> bool {
        // The words are the PRODUCER'S, as consts of the generated inventory, so a
        // word it does not declare has no name to write here. `pending` had one
        // for months — as a quoted literal that no producer has ever sent.
        //
        // The path is spelled in full rather than aliased, deliberately: rustc's
        // refusal is `cannot find ... constant `X` in module `<path>``, and the
        // path is where the authority on the vocabulary is. An alias would make the
        // error say `in module `word``, which names the alias and not the producer.
        self.executing
            || matches!(
                self.state.as_str(),
                inventory::threads::status::state::ACTIVE
                    | inventory::threads::status::state::RETRYING
            )
    }

    // **THERE IS NO `pending()` HERE ANY MORE, AND THAT IS THE POINT.**
    //
    // `pending(&self) -> bool { self.state == <that word> }` stood here. The
    // word is deliberately NOT quoted into that comparison, and the reason is
    // the other half of this merge: `seamdiff --inventory` now scans this
    // file's RAW TEXT for every `==` that is followed by a quoted word, and it
    // is not
    // comment-aware — by design, because a word a face compares against is a
    // word a face compares against wherever it is written. A tombstone that
    // quoted the dead comparison verbatim would therefore re-seed, out of its
    // own epitaph, the exact `WordNotDeclared` finding the deletion exists to
    // remove, and `the_cell_guard_reads_the_real_reference_and_it_routes_clean`
    // says so in as many words. This is the same self-witnessing class as
    // `marker_open`'s in the engine, and it takes the same structural fix:
    // prose names the word, code does not compare against it. It was
    // the last surviving read of a status word NO PRODUCER ON THIS WIRE HAS
    // EVER SENT: the daemon's state vocabulary is
    // `active | queued | parked | retrying | idle`, so this predicate answered
    // `false` on every frame any producer ever exported, in the whole of its
    // life. Its only caller was the AUI's `daemon_pending`, which fed a veto
    // that silenced a heartbeat and placed an index-zero row, and which
    // therefore also never once fired.
    //
    // It is deleted rather than kept, because it existed to serve two verbs —
    // `thread.replace` and `thread.unpend` — that the operator killed
    // ("remove them"), and nothing else asked it anything. A predicate over a
    // word the wire does not carry is not a feature waiting for a producer: it
    // is a silent `false` that reads, to anyone above it, like an answer.
    //
    // `state` is still here and still the producer's own word, so a face that
    // one day has a real new state matches on `state` like any other, and the
    // `running()` above shows the shape. No face re-grows a predicate for a
    // word the inventory cannot route.
}

/// The `liveness` cell. All three fields are declared `Required` on the
/// producer, so a row missing any of them is `NotHydrated`, never a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Liveness {
    pub busy: bool,
    pub parent_running: bool,
    /// **THE IDS, as the producer carries them — not a count.** The producer's
    /// `LivenessRow::running_subagents` is `Vec<String>` and always was; this
    /// read it as a `u64` until 2026-10-08, got `None` from `as_u64`, and
    /// defaulted to zero — so every subagent count any face ever drew off this
    /// seam was 0. Found by declaring the cell (`List(Ref("thread"))`) and
    /// reading the declaration against the read.
    pub running_subagents: Vec<String>,
}

/// **ONE UNDELIVERED QUEUE ROW**, read off `threads`' `queue` cell.
///
/// It replaces TWO RPC calls, and the pair is the last one a face still had to
/// make for a thread it was already subscribed to: `threads/{id}/queue` and
/// `threads/{id}/queue_failed`. They projected the same four fields and differed
/// only in the state they filtered on, so the producer declares ONE cell with
/// `state` as a field — and a face filters instead of calling twice.
///
/// Every field is declared `Required`, so a row missing any of them is
/// `NotHydrated` and never a default. `metadata` and `headers` are on the
/// producer's stored row and are deliberately not declared, so there is no const
/// to read them with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedMessage {
    pub message_id: u64,
    /// **VERBATIM WHAT HE SAID.** The re-select surface exists so a prompt
    /// survives a retry ladder running out; a face that re-renders this from
    /// anything else defeats it.
    pub text: String,
    pub intent: String,
    pub created_at: String,
    /// `waiting` or `failed` — the producer's closed vocabulary, matched through
    /// `inventory::threads::queue::state`. A face shows the failed ones behind
    /// *"that didn't get through — send it again?"* and acts on them with
    /// `commands`' re-select verb, which takes the `message_id` beside it.
    pub state: String,
}

/// Where a thread's log head is, read off `threads`. No cursor: these are
/// positions a face RENDERS, never positions it asks from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Positions {
    pub newest_seq: u64,
    pub last_message_id: Option<u64>,
    pub seen_seq: u64,
    pub unread: u64,
    pub queued: u64,
}

/// One row of `threads/recent-updates`, in the order the producer exported it.
#[derive(Debug, Clone, PartialEq)]
pub struct Recent {
    pub id: String,
    pub at: Option<Timestamp>,
    pub unread: u64,
    pub seen_seq: u64,
}

/// One row of `threads/non-idle`'s roster: where a non-idle thread is placed
/// and what it is doing. Keyed by thread id in [`roster`].
#[derive(Debug, Clone, PartialEq)]
pub struct RosterRow {
    pub account: String,
    /// `active | parked | retrying | queued | idle` — the producer's closed
    /// vocabulary, `inventory::threads_non_idle::threads::state`.
    pub state: String,
    pub executing: bool,
    pub queued: u64,
    pub since: Option<Timestamp>,
    /// Every ancestor (`threads` keys), nearest first, `[]` for a root — what
    /// `shapewire::subtree::rollup_in` reads to roll a parent's status up from
    /// its subtree (R9.UI1). Read TOLERANTLY: a producer older than the field
    /// sends none, and that reads as `[]` (no subtree to roll up) rather than
    /// costing the whole roster.
    pub ancestors: Vec<String>,
}

/// One page of a thread's message history: a RESOURCE id and the message-id
/// range it covers (R2.S3d). Read off `threads.pages`; the bytes are read
/// through the resource door by the LINK — a view function cannot fetch.
#[derive(Debug, Clone, PartialEq)]
pub struct MessagePage {
    pub id: String,
    pub from: u64,
    pub to: u64,
}

/// One recorded gauge reading off `accounts/gauge-history` (R2.S3d).
#[derive(Debug, Clone, PartialEq)]
pub struct GaugeReading {
    /// `session | weekly_all | weekly_fable` —
    /// `inventory::accounts_gauge_history::readings::axis`.
    pub axis: String,
    pub pct: f64,
    /// When it was OBSERVED — the item's own stamp.
    pub at: Timestamp,
    pub resets_at: Option<Timestamp>,
    pub source: String,
}

/// One narration line.
///
/// **No `text` field.** There was one, read off the line and defaulted to `""`,
/// and no producer has ever carried a `text` on a narration line: a line is its
/// `Push` frame's fields verbatim, tagged on `kind` (`progress | lifecycle |
/// info`), and the SENTENCE is the face's to compose (the audio law). The seam
/// was fabricating an empty one. `kind` is the declared tag; `raw` is the line.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub at: Option<Timestamp>,
    pub kind: String,
    pub raw: Value,
}

// ──────────────────────────────────────────────────────────────────────────
//  Plumbing
// ──────────────────────────────────────────────────────────────────────────

/// Turn a cell's three-valued state plus its value into a [`Fact`]. **The one
/// place the three unknowns are read**, so no reader can collapse two of them by
/// accident.
///
/// A `Known` cell whose value the reader could not parse as declared — a
/// `Required` field missing or mistyped — is `NotHydrated` naming the cell, and
/// never a defaulted value. `.unwrap_or(false)` on a required field was how
/// `executing` would have read as *not executing* on a malformed row, which is
/// the class this whole kit is built against.
fn fact_of<T>(state: &CellState, read: impl FnOnce() -> Option<T>, what: &str) -> Fact<T> {
    match state {
        CellState::Known { .. } => match read() {
            Some(v) => Fact::Known(v),
            None => Fact::not_hydrated(format!(
                "`{what}` is known but does not carry what the producer declares for it \
                 (a required field is missing or mistyped)"
            )),
        },
        CellState::NotHydrated { reason } => Fact::NotHydrated {
            reason: reason
                .clone()
                .unwrap_or_else(|| format!("no export has carried `{what}`")),
        },
        CellState::Dead { reason } => Fact::Dead {
            reason: reason.clone(),
        },
    }
}

/// **A CELL THE PRODUCER DOES NOT DECLARE IS `Undeclared`, BEFORE ANY STATE IS
/// READ.** Asked of the mirror's learned manifest; `Unstated` (no `shapes/list`
/// yet) judges nothing, so a read before the handshake answers as it always did.
///
/// **THIS IS NOW THE BACKSTOP AND NOT THE MECHANISM**, and it is kept for exactly
/// one case the compiler cannot reach: a RUNNING producer whose manifest has moved
/// since the inventory this build compiled against was taken. Time skew is not
/// visible to any consumer's build, by definition, so the runtime answer still has
/// to exist — and it still says *never* rather than *not yet*.
fn undeclared(m: &Mirror, c: Carried) -> Option<String> {
    let (shape, name) = (c.shape(), c.name());
    let d = if c.is_shared() {
        m.shared_declared(name)
    } else {
        m.declared(name)
    };
    match d {
        Declaration::Undeclared { declares } => Some(format!(
            "`{name}` is not a cell `{shape}` declares, so no export will ever carry it; \
             this build reads a cell the producer does not serve. It declares: {}",
            if declares.is_empty() {
                "nothing at this level".to_string()
            } else {
                declares.join(", ")
            }
        )),
        _ => None,
    }
}

/// A cell of a keyed row, three-valued — four, counting [`Fact::Undeclared`].
///
/// **IT TAKES A `Carried`, NOT A NAME.** `shapewire::surface::Carried` is sealed:
/// the only thing that mints one is the generated `seam/inventory.rs`, which has
/// one const per cell the producer declares and cannot have one for a cell it
/// does not. So a read of a cell no export will ever carry does not fail a test —
/// it does not build. That is the `pending` hole closed at the cell layer: the
/// AUI read a `pending` field off `threads` rows for months, the mirror answered
/// *not yet* for the life of every process, and the veto built on it never fired
/// once.
///
/// The token carries the SHAPE as well as the name, because a cell name alone is
/// not a fact: `limits` is declared on two shapes and `threads` is a cell on one
/// shape and the name of another.
fn cell<T>(h: &Held, c: Carried, key: &str, f: impl FnOnce(&Value) -> Option<T>) -> Fact<T> {
    let (shape, name) = (c.shape(), c.name());
    let Some(m) = h.mirror(shape) else {
        return Fact::not_hydrated(format!("`{shape}` is not in this face's book"));
    };
    if let Some(r) = m.refusal() {
        return Fact::Dead {
            reason: r.to_string(),
        };
    }
    if let Some(reason) = undeclared(m, c) {
        return Fact::Undeclared { reason };
    }
    let state = m.cell(key, name);
    let row = m.row(key).clone();
    fact_of(&state, || f(row.get(name).unwrap_or(&Value::Null)), name)
}

fn shared_cell<T>(h: &Held, c: Carried, f: impl FnOnce(&Value) -> Option<T>) -> Fact<T> {
    let (shape, name) = (c.shape(), c.name());
    let Some(m) = h.mirror(shape) else {
        return Fact::not_hydrated(format!("`{shape}` is not in this face's book"));
    };
    if let Some(reason) = undeclared(m, c) {
        return Fact::Undeclared { reason };
    }
    let state = m.shared_cell(name);
    let shared = m.shared().clone();
    fact_of(&state, || f(shared.get(name).unwrap_or(&Value::Null)), name)
}

/// **AN EDGE-SHAPED CELL'S ITEMS** — what the export retained, then what has
/// been appended since (`Held::items`) — under the cell's own state. The third
/// read that takes a token.
fn items(h: &Held, c: Carried, key: &str) -> Fact<Vec<Value>> {
    let (shape, name) = (c.shape(), c.name());
    let Some(m) = h.mirror(shape) else {
        return Fact::not_hydrated(format!("`{shape}` is not in this face's book"));
    };
    if let Some(r) = m.refusal() {
        return Fact::Dead {
            reason: r.to_string(),
        };
    }
    if let Some(reason) = undeclared(m, c) {
        return Fact::Undeclared { reason };
    }
    let state = m.cell(key, name);
    let items = m.held(key).map(|held| held.items(name)).unwrap_or_default();
    fact_of(&state, || Some(items), name)
}

fn ts(v: &Value) -> Option<Timestamp> {
    v.as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
}

// ──────────────────────────────────────────────────────────────────────────
//  The link
// ──────────────────────────────────────────────────────────────────────────

pub fn link(h: &Held) -> &LinkState {
    &h.link
}

/// **ARE FRAMES STILL REACHING ME?** Not *is the socket open* (a TCP connection
/// survives a daemon that has stopped folding) and not *is a thread busy*. The
/// `heartbeat` shape is a beat that travels the FIFO, the fold and this
/// connection's channel, so a gap here means one of those three stopped.
///
/// `None` while the link is down — there is nothing to be late, and the face
/// already knows the link is down.
pub fn frames_stalled_for(h: &Held, now: Timestamp) -> Option<chrono::Duration> {
    if !h.link.is_up() {
        return None;
    }
    let m = h.mirror(HEARTBEAT)?;
    let at = m.held(LINK).and_then(|held| held.at())?;
    Some(now.signed_duration_since(at))
}

/// **WHEN THIS FACE STARTED SEEING THIS CONNECTION'S FRAMES.** The replacement
/// for `StatusTable::watching_since`, and it is the mirror's own stamp rather
/// than an `Instant` taken when a query returned: a silence may never be
/// reported as longer than the span frames have actually been arriving over.
pub fn watching_since(h: &Held) -> Option<Timestamp> {
    match &h.link {
        LinkState::Up { since, .. } => Some(*since),
        _ => None,
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  The fleet
// ──────────────────────────────────────────────────────────────────────────

/// **EVERY THREAD THE DAEMON IS EXECUTING RIGHT NOW, AND WHERE IT IS PLACED** —
/// off `threads/non-idle`'s one roster, which is the fleet-wide answer for the
/// price of ONE keyless subscription and one row.
///
/// This is what replaces `threads/status {limit: 1000}` on every connect,
/// reconnect, lag and five-second stale tick. It is not a query: it is a cell
/// that moves when the fleet moves.
///
/// **IT READ `accounts.*.threads` UNTIL R2.S8**, and that cell is gone: it was
/// six fields of `threads`' row nested under an account key, the one violation
/// of the nesting law on the producer. The roster is keyed by the POOL and each
/// entry DECLARES its placement as `account`, so the map below is the same map
/// it always was — thread id to the account it is placed on — read off a field
/// rather than off which row the entry happened to sit in.
///
/// **One difference, stated rather than smoothed over.** `accounts` filed a
/// thread placed on the machine default under the account `folds_into` names,
/// and dropped it when nothing claimed the default. The roster carries the
/// placement the daemon reported, so such a thread answers here with the
/// account spelled `default`. No face reads the value side of this map today
/// (both faces read its KEYS and its length); a face that grows one and wants
/// the card a thread is drawn on resolves through `accounts`' shared
/// `folds_into`, which is where that fold lives.
pub fn running(h: &Held) -> Fact<BTreeMap<String, String>> {
    use inventory::threads_non_idle::threads as placed;
    cell(h, inventory::threads_non_idle::THREADS, super::held::FLEET, |v| {
        let mut out = BTreeMap::new();
        for e in v.as_array()? {
            // `id`, `account`, `state` and `executing` are all declared
            // `Required`; an entry without one is a producer defect and makes
            // the whole fact `NotHydrated` (the `?`s), never an entry read as
            // idle — deliver-or-refuse, from the consuming end.
            let id = e.get(placed::ID).and_then(Value::as_str)?;
            let account = e.get(placed::ACCOUNT).and_then(Value::as_str)?;
            let state = e.get(placed::STATE).and_then(Value::as_str)?;
            let executing = e.get(placed::EXECUTING).and_then(Value::as_bool)?;
            // Spelled in full, for the reason `ThreadStatus::running` gives: the
            // module path in rustc's refusal is where the vocabulary's authority is.
            if executing
                || matches!(
                    state,
                    inventory::threads_non_idle::threads::state::ACTIVE
                        | inventory::threads_non_idle::threads::state::RETRYING
                )
            {
                out.insert(id.to_string(), account.to_string());
            }
        }
        Some(out)
    })
}

/// **EVERY NON-IDLE THREAD, EXECUTING OR NOT** — the whole of
/// `threads/non-idle`'s one roster, thread id to [`RosterRow`]. [`running`] is
/// the executing subset; THIS is what a face draws as its fleet, because a
/// `parked` or `queued` thread is exactly the one the operator needs to see.
/// `accounts.*.threads` carried all of them until R2.S8, and a face that moved
/// to `running` lost the parked ones off its screen.
///
/// Same three-valued answer as [`running`]: not hydrated, dead or undeclared is
/// not a roster, and an entry missing a `Required` field (`id`, `account`,
/// `state`, `executing`, `queued`) makes the whole fact `NotHydrated` rather
/// than a row with a guess in it. `since` is declared optional and reads `None`.
pub fn roster(h: &Held) -> Fact<BTreeMap<String, RosterRow>> {
    use inventory::threads_non_idle::threads as placed;
    cell(h, inventory::threads_non_idle::THREADS, super::held::FLEET, |v| {
        let mut out = BTreeMap::new();
        for e in v.as_array()? {
            let id = e.get(placed::ID).and_then(Value::as_str)?;
            out.insert(
                id.to_string(),
                RosterRow {
                    account: e.get(placed::ACCOUNT).and_then(Value::as_str)?.to_string(),
                    state: e.get(placed::STATE).and_then(Value::as_str)?.to_string(),
                    executing: e.get(placed::EXECUTING).and_then(Value::as_bool)?,
                    queued: e.get(placed::QUEUED).and_then(Value::as_u64)?,
                    since: e.get(placed::SINCE).and_then(ts),
                    ancestors: e
                        .get(placed::ANCESTORS)
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                        .unwrap_or_default(),
                },
            );
        }
        Some(out)
    })
}

pub fn running_count(h: &Held) -> Fact<usize> {
    running(h).map(|m| m.len())
}

/// The order to look at first, plus the unread roll-up over it.
pub fn recent(h: &Held) -> Fact<Vec<Recent>> {
    cell(h, inventory::threads_recent_updates::THREADS, super::held::FLEET, |v| {
        let a = v.as_array()?;
        Some(
            a.iter()
                .map(|e| {
                    // Every field is declared `Required`; an entry missing one
                    // makes the whole fact `NotHydrated` (the `?` below), never
                    // an entry with a zero nobody stated.
                    Some(Recent {
                        id: e.get(inventory::threads_recent_updates::threads::ID).and_then(Value::as_str)?.to_string(),
                        at: e.get(inventory::threads_recent_updates::threads::AT).and_then(ts),
                        unread: e.get(inventory::threads_recent_updates::threads::UNREAD).and_then(Value::as_u64)?,
                        seen_seq: e.get(inventory::threads_recent_updates::threads::SEEN_SEQ).and_then(Value::as_u64)?,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
        )
    })
}

/// Every account the daemon knows, off the `accounts` mirror's key set.
pub fn accounts(h: &Held) -> Fact<Vec<String>> {
    let Some(m) = h.mirror(ACCOUNTS) else {
        return Fact::not_hydrated("`accounts` is not in this face's book");
    };
    if !m.exported() {
        return Fact::not_hydrated("no `accounts` export has arrived on this connection");
    }
    Fact::Known(m.keys())
}

/// One account's whole row, held opaquely. The Options menu already reads a
/// `Value` of this shape off `accounts/status`; it reads the same object here and
/// nothing above the seam changes.
pub fn account_row(h: &Held, name: &str) -> Fact<Value> {
    let Some(m) = h.mirror(ACCOUNTS) else {
        return Fact::not_hydrated("`accounts` is not in this face's book");
    };
    match m.key_state(name) {
        shapewire::mirror::KeyState::Held => Fact::Known(m.row(name).clone()),
        shapewire::mirror::KeyState::Gone => Fact::not_hydrated(format!(
            "the daemon says account `{name}` is gone"
        )),
        shapewire::mirror::KeyState::Absent => Fact::not_hydrated(format!(
            "the daemon answered account `{name}` with no row: it does not hold it"
        )),
        shapewire::mirror::KeyState::Unknown => {
            Fact::not_hydrated(format!("no export has carried account `{name}`"))
        }
    }
}

/// Whether the `threads/summary` key set hit its 500-row cap, so a face knows
/// whether a missing title means *unnamed* or *outside the window*.
pub fn summary_truncated(h: &Held) -> Fact<bool> {
    shared_cell(h, inventory::threads_summary::TRUNCATED, Value::as_bool)
}

// ──────────────────────────────────────────────────────────────────────────
//  One thread
// ──────────────────────────────────────────────────────────────────────────

/// The KEY-level question. Ask this before believing any `NotHydrated` below:
/// `NotAsked` means the book does not hold the key, which says nothing at all
/// about the thread.
pub fn holding(h: &Held, id: &str) -> Holding {
    h.holding(THREADS, id)
}

pub fn status(h: &Held, id: &str) -> Fact<ThreadStatus> {
    cell(h, inventory::threads::STATUS, id, |v| {
        Some(ThreadStatus {
            state: v.get(inventory::threads::status::STATE).and_then(Value::as_str)?.to_string(),
            // Declared `Required`: absent is a malformed row, not `false`.
            executing: v.get(inventory::threads::status::EXECUTING).and_then(Value::as_bool)?,
            detail: v
                .get(inventory::threads::status::DETAIL)
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    })
}

/// Every status transition this shape has retained for the thread, oldest
/// first. `since_init` on the cell state says it covers only what the shape saw
/// since it initialised, which is why this is never counted as history.
///
/// Read under the `status_transitions` cell's OWN state — it is its own cell on
/// the row, with its own `state` word — rather than under `status`'s, which it
/// was until 2026-10-08: two cells, two states, and one was being read for the
/// other.
pub fn status_transitions(h: &Held, id: &str) -> Fact<Vec<Value>> {
    items(h, inventory::threads::STATUS_TRANSITIONS, id)
}

/// **THE UNDELIVERED QUEUE — BOTH HALVES, ONE READ, NO RPC.**
///
/// This is what retires `threads/{id}/queue` and `threads/{id}/queue_failed`.
/// Both were calls a face made about a thread it was ALREADY subscribed to,
/// which is the shape of every read this seam exists to remove: the answer moves
/// when the queue moves, so asking for it on a timer or on a keypress was
/// always a guess about when it had changed.
///
/// The whole list, in the producer's order (`waiting` by insertion, then
/// `failed` by insertion). A face that drew one of the two panels filters on
/// [`QueuedMessage::state`]; a face that drew both now draws both from one cell
/// and cannot show them disagreeing about which rows exist.
///
/// **A KNOWN EMPTY QUEUE IS `Known(vec![])`, NOT `NotHydrated`** — the producer
/// answers `Hydration::Empty` for a thread with nothing parked, which is the
/// ordinary case. A face that renders `?` for it would put a `?` on almost every
/// thread, almost always.
pub fn queue(h: &Held, id: &str) -> Fact<Vec<QueuedMessage>> {
    cell(h, inventory::threads::QUEUE, id, |v| {
        v.as_array()?
            .iter()
            .map(|e| {
                Some(QueuedMessage {
                    message_id: e
                        .get(inventory::threads::queue::MESSAGE_ID)
                        .and_then(Value::as_u64)?,
                    text: e
                        .get(inventory::threads::queue::TEXT)
                        .and_then(Value::as_str)?
                        .to_string(),
                    intent: e
                        .get(inventory::threads::queue::INTENT)
                        .and_then(Value::as_str)?
                        .to_string(),
                    created_at: e
                        .get(inventory::threads::queue::CREATED_AT)
                        .and_then(Value::as_str)?
                        .to_string(),
                    state: e
                        .get(inventory::threads::queue::STATE)
                        .and_then(Value::as_str)?
                        .to_string(),
                })
            })
            .collect::<Option<Vec<_>>>()
    })
}

/// The failed half alone, for the re-select prompt — *"that didn't get through
/// — send it again?"*. A convenience over [`queue`] and NOT a second read: one
/// cell, one state, filtered here rather than by a second call to the producer.
///
/// Spelled in full against `inventory::threads::queue::state`, for the reason
/// `running()` gives: the module path in rustc's refusal is where the
/// vocabulary's authority is.
pub fn queue_failed(h: &Held, id: &str) -> Fact<Vec<QueuedMessage>> {
    queue(h, id).map(|q| {
        q.into_iter()
            .filter(|e| e.state == inventory::threads::queue::state::FAILED)
            .collect()
    })
}

pub fn liveness(h: &Held, id: &str) -> Fact<Liveness> {
    cell(h, inventory::threads::LIVENESS, id, |v| {
        Some(Liveness {
            busy: v.get(inventory::threads::liveness::BUSY).and_then(Value::as_bool)?,
            parent_running: v.get(inventory::threads::liveness::PARENT_RUNNING).and_then(Value::as_bool)?,
            running_subagents: v
                .get(inventory::threads::liveness::RUNNING_SUBAGENTS)
                .and_then(Value::as_array)?
                .iter()
                .map(|x| x.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()?,
        })
    })
}

/// **FIVE CELLS, FIVE STATES.** `newest`, `last_message_id`, `seen_seq`,
/// `unread` and `queued` are each their own cell on the `threads` row, and this
/// read used to gate all five on `newest`'s state alone — so a `seen_seq` the
/// producer had not hydrated (and, until the same day, filled with a `0`) was
/// read as the operator's seen position under another cell's `Known`. Each
/// cell is now read under its own state, and the first one that is not `Known`
/// is the answer, naming itself.
pub fn positions(h: &Held, id: &str) -> Fact<Positions> {
    let newest_seq = match cell(h, inventory::threads::NEWEST, id, |n| n.get(inventory::threads::newest::SEQ).and_then(Value::as_u64)) {
        Fact::Known(v) => v,
        other => return other.map(|_| Positions::default()),
    };
    let last_message_id = match cell(h, inventory::threads::LAST_MESSAGE_ID, id, |v| Some(v.as_u64())) {
        Fact::Known(v) => v,
        other => return other.map(|_| Positions::default()),
    };
    let seen_seq = match cell(h, inventory::threads::SEEN_SEQ, id, Value::as_u64) {
        Fact::Known(v) => v,
        other => return other.map(|_| Positions::default()),
    };
    let unread = match cell(h, inventory::threads::UNREAD, id, Value::as_u64) {
        Fact::Known(v) => v,
        other => return other.map(|_| Positions::default()),
    };
    let queued = match cell(h, inventory::threads::QUEUED, id, Value::as_u64) {
        Fact::Known(v) => v,
        other => return other.map(|_| Positions::default()),
    };
    Fact::Known(Positions {
        newest_seq,
        last_message_id,
        seen_seq,
        unread,
        queued,
    })
}

/// **The thread's settings, as the daemon holds them.** The one answer
/// `sync_thread_settings` used to get from `threads/{id}/get` before every
/// send. `docs/consumer-migration-ledger.md` pins that what `get` returned under
/// `settings` is byte-identical to what the shape carries, so the comparison a
/// face makes before a send is unchanged — only the door it came through is
/// gone.
pub fn settings(h: &Held, id: &str) -> Fact<Value> {
    cell(h, inventory::threads::SETTINGS, id, |v| Some(v.clone()))
}

/// Which login the daemon placed this thread on. `None` inside `Known` is the
/// daemon saying it has not placed it, which is a different answer from *I have
/// not been told*.
pub fn account(h: &Held, id: &str) -> Fact<Option<String>> {
    cell(h, inventory::threads::ACCOUNT, id, |v| {
        Some(v.as_str().map(str::to_string))
    })
}

/// **THIS THREAD'S PARENT**, when it is a managed subagent. `None` inside
/// `Known` is a root — a different answer from *not told*.
pub fn parent_id(h: &Held, id: &str) -> Fact<Option<String>> {
    cell(h, inventory::threads::PARENT_ID, id, |v| {
        Some(v.as_str().map(str::to_string))
    })
}

/// **THE MANAGED CHILDREN THIS THREAD SPAWNED** — `{outstanding, runs}`, the
/// value as the producer declares it. Lineage only; each child's live state is
/// on the child's own row.
pub fn children(h: &Held, id: &str) -> Fact<Value> {
    cell(h, inventory::threads::CHILDREN, id, |v| {
        v.get(inventory::threads::children::OUTSTANDING)?.as_array()?;
        v.get(inventory::threads::children::RUNS)?.as_array()?;
        Some(v.clone())
    })
}

/// **THE THREAD'S STATUS, ROLLED UP BY THE PRODUCER** (R9.TS1) — `status`
/// (`active` | `idle` | `error`), `display` (the one string a face draws:
/// `active (+X)`, `active`, `idle`, `error: <label>`), `children_summary`
/// (`N active, M idle[, K error]`, `null` with no children) and the counts
/// behind them, as the producer declares the cell. A face draws `display` and
/// `children_summary` verbatim; it never re-derives the rule. A value missing
/// its required `status` or `display` is not an answer.
pub fn rolled(h: &Held, id: &str) -> Fact<Value> {
    cell(h, inventory::threads::ROLLED, id, |v| {
        v.get(inventory::threads::rolled::STATUS)?.as_str()?;
        v.get(inventory::threads::rolled::DISPLAY)?.as_str()?;
        Some(v.clone())
    })
}

pub fn header(h: &Held, id: &str) -> Fact<Value> {
    cell(h, inventory::threads::HEADER, id, |v| Some(v.clone()))
}

/// The thread's NAME, off `threads/summary`. `None` inside `Known` means the
/// row exists with no title; a `NotHydrated` for a thread outside the 500 is
/// what [`summary_truncated`] explains, and the FACE falls back to its own
/// index there.
pub fn title(h: &Held, id: &str) -> Fact<Option<String>> {
    cell(h, inventory::threads_summary::TITLE, id, |v| {
        Some(v.get(inventory::threads_summary::title::TITLE).and_then(Value::as_str).map(str::to_string))
    })
}

/// The app's own newest custom event of one type, held opaquely by the producer.
/// `Known(Null)` is a type the app has never written: the cell is a map and an
/// absent key in it is a statement, not a missing cell.
pub fn app_payload(h: &Held, id: &str, event_type: &str) -> Fact<Value> {
    cell(h, inventory::threads::APP, id, |a| Some(a.get(event_type).cloned().unwrap_or(Value::Null)))
}

/// **THE THREAD'S CURRENT CONTEXT ID** (R2.S3d) — `threads.context_id`,
/// `<thread_id>.<last_compaction_message_id>`, the producer's one derivation.
/// OPAQUE: compare it whole, never parse it. A new value is a new context — the
/// cache key the harness's delta reads were asking `threads/{id}/get` for.
pub fn context_id(h: &Held, id: &str) -> Fact<String> {
    cell(h, inventory::threads::CONTEXT_ID, id, |v| v.as_str().map(str::to_string))
}

/// **THE THREAD'S MESSAGE HISTORY, AS RESOURCE PAGES** (R2.S3d) —
/// `threads.pages`, oldest first. Pick the pages whose `[from, to]` covers the
/// message ids wanted and read each through the resource door; never build a
/// page id. An entry missing a `Required` field makes the whole fact
/// `NotHydrated`, never a page with a guess in it.
pub fn message_pages(h: &Held, id: &str) -> Fact<Vec<MessagePage>> {
    use inventory::threads::pages as page;
    cell(h, inventory::threads::PAGES, id, |v| {
        v.as_array()?
            .iter()
            .map(|p| {
                Some(MessagePage {
                    id: p.get(page::ID).and_then(Value::as_str)?.to_string(),
                    from: p.get(page::FROM).and_then(Value::as_u64)?,
                    to: p.get(page::TO).and_then(Value::as_u64)?,
                })
            })
            .collect()
    })
}

/// **AN ACCOUNT'S WEEK OF GAUGE READINGS** (R2.S3d) — `accounts/gauge-history`'s
/// `readings` for `account`, oldest first, exported window plus every reading
/// appended since. `Known(empty)` is a week with no readings; a key the face
/// has not opened is `NotHydrated`. An item missing a `Required` field makes
/// the whole fact `NotHydrated`.
pub fn gauge_series(h: &Held, account: &str) -> Fact<Vec<GaugeReading>> {
    use inventory::accounts_gauge_history::readings as r;
    match items(h, inventory::accounts_gauge_history::READINGS, account) {
        Fact::Known(items) => {
            let parsed: Option<Vec<GaugeReading>> = items
                .iter()
                .map(|e| {
                    Some(GaugeReading {
                        axis: e.get(r::AXIS).and_then(Value::as_str)?.to_string(),
                        pct: e.get(r::PCT).and_then(Value::as_f64)?,
                        at: e.get(r::AT).and_then(ts)?,
                        resets_at: e.get(r::RESETS_AT).and_then(ts),
                        source: e.get(r::SOURCE).and_then(Value::as_str)?.to_string(),
                    })
                })
                .collect();
            match parsed {
                Some(v) => Fact::Known(v),
                None => Fact::not_hydrated(
                    "a `readings` item is missing a field the producer declares required",
                ),
            }
        }
        Fact::NotHydrated { reason } => Fact::NotHydrated { reason },
        Fact::Dead { reason } => Fact::Dead { reason },
        Fact::Undeclared { reason } => Fact::Undeclared { reason },
    }
}

/// Where this thread's newest message BODY is. The plan is executed by the
/// LINK — a view function cannot fetch, by construction.
pub fn body_plan(h: &Held, id: &str) -> Result<Fetch, ContentError> {
    match h.mirror(THREADS) {
        Some(m) => m.plan(id, inventory::threads::BODY.name()),
        None => Err(ContentError::NotKnown {
            reason: "`threads` is not in this face's book".into(),
        }),
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  The log window
// ──────────────────────────────────────────────────────────────────────────

/// **THE THREAD'S LIVE EVENT LOG, AS THE MIRROR HOLDS IT.** Oldest first, every
/// kind, exactly the rows `threads/{id}/history` would have paged — and never
/// paged, because the export carried the window and every transition since.
///
/// This is the function that let `backfill`, `fold_catch_up` and the
/// `Reconnected` arm of the turn watcher be deleted: there is no gap to fill,
/// so there is no filler.
pub fn log_rows(h: &Held, id: &str) -> Fact<Vec<Value>> {
    items(h, inventory::threads_log::EVENTS, id)
}

/// The oldest `seq` the producer still retains for this thread — the place a
/// deep-scrollback walk starts, derived from a row the consumer already holds
/// rather than from a cell nobody needed.
pub fn log_covers_from(h: &Held, id: &str) -> Option<u64> {
    log_rows(h, id)
        .known()?
        .first()
        .and_then(|r| r.get(inventory::threads_log::events::SEQ).and_then(Value::as_u64))
}

/// **WHEN THIS THREAD'S TURN LAST PRODUCED SOMETHING**, from the log window's
/// own `task_progress` rows.
///
/// The firehose path kept a `HashMap<session, Instant>` updated by a
/// `note_progress` call inside the status task. The window already holds those
/// rows, in order, with the producer's own `ts` on each — so the mark is
/// derived at read time and there is nothing to keep, nothing to clear when a
/// turn ends, and no way for a new turn to inherit the previous turn's proof of
/// life (the window is the ledger's order and the caller bounds it by the turn's
/// own age).
pub fn last_progress_at(h: &Held, id: &str) -> Option<Timestamp> {
    log_rows(h, id)
        .known()?
        .iter()
        .rev()
        .find(|r| {
            r.get(inventory::threads_log::events::KIND).and_then(Value::as_str)
                == Some(inventory::threads_log::events::kind::TASK_PROGRESS)
        })
        .and_then(|r| r.get(inventory::threads_log::events::TS).and_then(ts))
}

// ──────────────────────────────────────────────────────────────────────────
//  Narration
// ──────────────────────────────────────────────────────────────────────────

pub fn narration(h: &Held, channel: &str, since: Option<Timestamp>) -> Fact<Vec<Line>> {
    items(h, inventory::narration::LINES, channel).map(|items| {
        items
            .into_iter()
            .filter_map(|raw| {
                let at = raw.get(inventory::narration::lines::AT).and_then(ts);
                if let (Some(since), Some(at)) = (since, at) {
                    if at <= since {
                        return None;
                    }
                }
                // `kind` is the declared tag and is `Required`; a line without
                // one is not a line this build can name, and is left out rather
                // than given an empty kind.
                let kind = raw.get(inventory::narration::lines::KIND).and_then(Value::as_str)?.to_string();
                Some(Line { at, kind, raw })
            })
            .collect()
    })
}

// ──────────────────────────────────────────────────────────────────────────
//  Commands
// ──────────────────────────────────────────────────────────────────────────

/// Where one command stands. Rule 2.4's edge: the reply never carried the fact,
/// so this is the only honest answer and it comes off the frame that did.
pub fn settled(h: &Held, promise: u64) -> Settled {
    h.promises.status(promise)
}
