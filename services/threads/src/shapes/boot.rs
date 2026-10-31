//! # The `boot` shape — why THIS process is the one you reached
//!
//! `server.rs` and `chassis.rs` both carried the same block until R2.S4
//! (2026-10-10) retired the legacy firehose and deleted both copies:
//!
//! ```text
//! if !sent_boot_note && firehose { ... ServerMsg::Lifecycle { .. } ... }
//! ```
//!
//! once per connection, **to firehose subscribers only**. Its own comment gave
//! the reason it had to be a replay rather than a broadcast:
//!
//! > *"`on_boot` computes the reason this process came up, and it runs BEFORE
//! > this server is listening — so at the instant the fact exists there is
//! > nobody to broadcast it to."*
//!
//! **That is the definition of a fact that predates every subscription**, and it
//! is why no shape on this producer could hold it before one was built for it:
//! every other shape's content either arrives as an input after init
//! ([`crate::shapes::narration`], [`crate::shapes::heartbeat`]) or is re-read
//! from the store at init ([`crate::shapes::accounts`],
//! [`crate::shapes::threads`]). This fact is neither. It is computed once, from
//! evidence that exists for one instant, and then held.
//!
//! So: retire the firehose with this block still in it and the capability
//! vanishes silently — a consumer on shapes alone would never learn whether the
//! daemon it is talking to fell back onto a previous binary. This shape is that
//! block's home, and it is the only thing it is for. Since R2.S4 it is the ONLY
//! carrier: the block is gone, and [`crate::update::UpdateGate::boot_note`]'s
//! one reader is this shape.
//!
//! ## A key is: THE PRODUCER. There is one, and it is `daemon`
//!
//! `docs/shape-design-guide.md` step 1 says to write the sentence *"a key is …"*
//! first. Here it is the same awkward one [`crate::shapes::warnings`] answered:
//! the subject is **the producer itself**.
//!
//! | shape | a key is |
//! |---|---|
//! | `accounts` | a pool account |
//! | `threads` | a thread |
//! | `threads/summary` | a thread that has a name |
//! | `narration` | a channel the daemon talks on |
//! | `threads/recent-updates` | the one pool, `fleet` |
//! | `threads/non-idle` | the one pool, `fleet` |
//! | `heartbeat` | the one link, `link` |
//! | `warnings` | the one producer, `daemon` |
//! | **`boot`** | **the one producer, `daemon`** |
//!
//! The precedents are the guide's *"a single thing is a pool with one fixed
//! key"*: `heartbeat`'s `link` and `recent`'s `fleet`, with `warnings`' `daemon`
//! as the one that already made the producer a subject.
//!
//! **IT IS NOT KEYED BY `boot_id`, and that was the live alternative.** The
//! SUBJECT of the fact is a boot, so keying by the boot's own id looks like the
//! honest choice. It is not, for a reason that decides it rather than merely
//! leaning:
//!
//! * **A consumer cannot name the key before it has the answer.** `boot_id` is
//!   minted by the process it identifies. A face that wants standing interest in
//!   *"why is the daemon I am attached to the one I am attached to"* has to
//!   express that BEFORE it knows which boot it reached — and under
//!   [`Hydrate::Whole`] a keyless subscribe would still work, so this is not
//!   fatal, but a KEYED subscribe would be unexpressible, which makes half the
//!   shape's surface useless.
//! * **The key would change on every restart, and a root holds one era anyway.**
//!   A process has exactly one boot. Keying by `boot_id` would produce a pool
//!   whose key set is always `{one}` and always a different one, so `gone`/`added`
//!   would fire on every restart to say something the epoch already said.
//! * **There is no history to key.** This shape serves THIS boot. The durable
//!   record of past boots is `restarts.jsonl` (`crate::update::RESTART_LOG`), it
//!   is not a shape, and making it one is a different piece of work with a
//!   different subject (see "What this is not", below).
//!
//! So `boot_id` is a CELL on the one stable key, which is what lets a consumer
//! correlate this row against the `boot_id` on its own `welcome` frame and
//! against [`crate::shapes::narration`]'s `lifecycle` lines.
//!
//! ## WHY A NEW SHAPE, and the three candidates that were rejected
//!
//! **[`crate::shapes::narration`], rejected, and it is the best *semantic* fit —
//! it already has a `lifecycle` channel carrying this exact note type.** Two
//! reasons, and the second is the one that decides it:
//!
//! 1. **Rule 2.2 routes them differently.** A narration line is a TRANSITION: an
//!    occurrence, serial, where two in the wrong order are two different
//!    stories. This is a LEVEL — *"the current value of why this process is
//!    running"* — and it never moves for the life of the process. Rule 2.2 has
//!    no dial: a `Sequence` item goes down the serial path and a `Register` goes
//!    down the coalescing one, and putting a level in a window is choosing the
//!    wrong path by hand.
//! 2. **THE WINDOW WOULD EVICT IT.** `narration`'s cells are
//!    `Sequence::new(LINE_CAPACITY, Duration::hours(1))` — **both** bounds, and
//!    the time bound is the problem. A daemon that has been up for more than an
//!    hour would hand a fresh subscriber a `lifecycle` channel with the boot note
//!    aged out of it. The fact that must survive for exactly as long as the
//!    process does would expire from the only cell that held it, and this daemon's
//!    uptime is measured in days. That is not a policy disagreement; it is the
//!    shape being unable to carry the value.
//!
//!    Its module doc's own reason — *"There is no hydrate request in this file at
//!    all … these frames are transient by construction"* — is true and points the
//!    same way, but it is the weaker statement: one could in principle add a
//!    hydrate to `narration`. One cannot add a hydrate that outlives the window
//!    the same cell enforces.
//!
//! **[`crate::shapes::warnings`], rejected, and it was the closest call.** It is
//! already `Whole`, already keyed `daemon`, already `Lazy`, already about the
//! producer rather than any account or thread, and its hydrate already reads
//! process memory rather than a store — which is the one property this fact
//! needs and no other shape has. Riding its `daemon` row would have cost no new
//! shape, no registration, no inventory entry. Three things killed it:
//!
//! 1. **The two have OPPOSITE silence semantics, and both are load-bearing.**
//!    `warnings` exists to be silent — *"nothing warned → no frame at all,
//!    forever, at zero cost … the quiet is the feature"*, which is why its fold
//!    reads before it reaches. This fact must be delivered on EVERY subscribe,
//!    unconditionally, because being unlearnable-after-the-fact is its entire
//!    problem. One row cannot be both the shape that says nothing and the shape
//!    that always says something.
//! 2. **It would price a once-per-epoch fact at the warning rate.** A `row` frame
//!    carries the whole row, so four immutable strings would ride every sample in
//!    which a warning count moved — and a consumer that wants only the boot note
//!    would be subscribed to a 1 Hz sampler to get it.
//! 3. **Rule 8, which is what `warnings` used to reject `heartbeat`.** *"How many
//!    warnings has this daemon emitted"* and *"why is this daemon running"* are
//!    two unrelated questions, and a second question on one shape is the collapse
//!    that rule names. The argument does not get weaker for being reused.
//!
//! **A cell on [`crate::shapes::threads`] or [`crate::shapes::accounts`],
//! rejected, and not closely.** Neither has a key this fact could hang on: it is
//! not about a thread and not about an account. `accounts` was the cheapest route
//! to the board's eye (it is the one shape the board mirrors) and is wrong for
//! `warnings`' reason — a design where *"why did the daemon restart"* is only
//! answerable by subscribing to the account board is a general shape encoding one
//! application's layout.
//!
//! ## `Lazy`, and `Whole`
//!
//! **`Lazy`** for the usual reason — no subscriber, no root — and here it costs
//! strictly nothing to be wrong about: there is no sampler to keep off an
//! unwatched machine ([`crate::shapes::warnings`]' reason), no FIFO to keep a
//! ledger out of ([`crate::shapes::threads`]' reason), and no beat. The hydrate
//! is two `Mutex` locks and four `String` clones. `Lazy` is chosen because
//! `Eager` is only required by a shape holding a [`crate::shape::Counter`] and
//! this holds none, so the default discipline applies.
//!
//! **A `Lazy` re-init loses nothing, and that is the same property `warnings`
//! argued.** The value is not in the root: it lives in
//! [`crate::update::UpdateGate`], outside the shape, for the life of the process.
//! So a drop on the last unsubscribe and a re-hydrate on the next subscribe read
//! the *same* note and produce the *same* export. There is no epoch in which
//! this shape has a different answer.
//!
//! **`Whole`.** One key, minted at init, and there is never a second — so *"every
//! key I know about"* and *"every key there is"* are the same key, which is what
//! `Whole` claims. A keyless subscribe here is an inventory of one.
//!
//! ## THE HYDRATE'S SOURCE, AND WHAT "DURABLE" HAS TO MEAN HERE
//!
//! This is the precondition the whole design rests on, so it is stated exactly
//! rather than gestured at. The claim it is usually given as — *"`boot_note()` is
//! computed at `on_boot` from a durable record, so a hydrate has a source"* — is
//! **half wrong**, and the half that is wrong matters.
//!
//! What `on_boot` actually does ([`crate::update::UpdateGate::on_boot`]):
//!
//! 1. reads the durable pending record, `pending-restart.json`;
//! 2. reconciles it against the identity this process actually booted with;
//! 3. **DELETES IT** — on the satisfied path `clear_pending` calls
//!    `write_pending(None)`, which is `fs::remove_file`;
//! 4. computes the note from that reconciliation, plus `current_exe` and the
//!    last-good path;
//! 5. stores the note in a `StdMutex<Option<LifecycleNote>>` on the gate.
//!
//! So **the disk record is destroyed by the very step that reads it**, and it is
//! destroyed before the server is listening. A hydrate that went back to
//! `pending-restart.json` would find nothing and would therefore compute a
//! DIFFERENT and WRONG answer — `unknown` instead of `new_binary`. Disk
//! durability is not merely unnecessary here; re-reading disk would be the bug.
//!
//! Nor does `restarts.jsonl` hold it. [`crate::update::LifecycleNote`]'s own doc
//! says *"The durable record of the same events is `restarts.jsonl`, which … remains
//! the source of truth; this is the doorbell, not the ledger"* — but the line
//! `log_event("boot", ..)` writes carries `at`, `event`, `pid`, `binary`,
//! `identity` and `pending`, and **not** the derived `reason` or `detail`. The
//! ledger holds the evidence; it does not hold the conclusion.
//!
//! **What IS true, and is the right bar.** The source is
//! [`crate::update::UpdateGate::boot_note`] — process memory — and it is
//! *process-durable*: written once, never mutated (one write, `update.rs`'s
//! `on_boot`), and readable for the entire remaining life of the process. That is
//! exactly the lifetime a shape needs, because **a shape's root cannot outlive its
//! process either.** A restart is a new process with a new note and a new epoch;
//! there is no reachable moment at which a hydrate could be asked for this fact
//! and the memory not be there.
//!
//! That is the same argument [`crate::shapes::warnings`] makes for
//! `crate::warnings::COUNTER` — *"The COUNTERS are not in the root and do not
//! reset with it … a `Lazy` drop and re-init loses nothing: the next hydrate reads
//! the live totals"* — and it clears `emit_thread_queue`'s bar, which is the bar
//! that matters: *"a fact invented here would be a value with no source."* This
//! value has a source. It is read, never computed here, and this file contains no
//! expression that could produce a reason word.
//!
//! ## AND `None` IS NOT `unknown` — the one thing that could have been got wrong
//!
//! [`crate::update::UpdateGate::boot_note`] returns `Option`. It is `None` in a
//! process where `on_boot` has not run: before it on the real daemon, and for the
//! whole life of a test or tool that builds a manager and never calls it.
//!
//! `CAUSE_UNKNOWN` is **a real, positive answer** — [`crate::update::UpdateGate::compute_boot_note`]
//! is explicit that it is *"a real answer and the one this daemon owes when it
//! cannot account for itself"*, reached after three measurements that all came
//! back negative. *"There is no note yet"* is a different statement entirely, and
//! exporting the first as the second would be a producer substituting a value for
//! its own silence.
//!
//! So an absent note exports [`crate::shape::CellState::NotHydrated`] **with a
//! reason**, on all four cells, while the key `daemon` is still minted and the key
//! set is still complete — because the producer exists and is named even when its
//! account of itself is not yet composed. The three-valued cell state exists
//! precisely so a consumer never has to guess which of the two it is holding.
//!
//! This is `warnings`' *"prove zero reads as zero"* obligation in its other
//! direction: there, a known zero must not read as absent; here, an absence must
//! not read as a known word.
//!
//! ## Rule 2.2: A LEVEL, AND THE FOLD HAS NO LIVE ARM
//!
//! A boot note is a level about the current boot, so this shape's cells are
//! `Register`s and rule 2.2 routes them down the coalescing path.
//!
//! **And nothing folds into them. [`Boot::folds`] has no arm that writes.** That
//! is not an unfinished shape; it is what *"a fact that predates every
//! subscription"* means expressed as code. The note is computed once and never
//! mutated, so there is no input whose arrival could move a cell, and an arm that
//! wrote one would be writing a value with no source. The entire product of this
//! shape is its EXPORT.
//!
//! Three consequences worth stating, because a reader will reach for each:
//!
//! * **`BURST` is `0`, deliberately.** Every other shape sets it just above its
//!   legitimate fan-out. This shape's legitimate fan-out is nothing, so `0` is
//!   that same rule applied — and it makes the backstop useful rather than
//!   decorative: the first derived event this fold ever produces trips the burst
//!   warning, which is exactly the review an author adding a live arm here should
//!   be made to pass.
//! * **It needs no `Fact` variant, and therefore changes no other shape.** Every
//!   fold on this producer matches exhaustively with no `_` arm, so a new input
//!   variant is a compile error in nine files. This shape adds none. That is a
//!   property of the fact, not a convenience: a `Fact` would be a live input for a
//!   value that has no live source.
//! * **The going-down note is NOT here, and must not be.**
//!   `Push::Lifecycle(phase: going_down)` is emitted while the socket is still
//!   open, so it is reachable by broadcast and already has a home —
//!   [`crate::shapes::narration`]'s `lifecycle` channel, where it is a transition
//!   among transitions. Folding it here would be rule 7's *"one way to know a
//!   thing"* broken, and incoherent besides: the cell would be a level whose value
//!   is erased by the next boot's hydrate, i.e. erased by the completion of the
//!   very event it announced.
//!
//! ## `phase` and `service` ARE CONSTANTS, NOT CELLS
//!
//! [`crate::update::LifecycleNote`] has six fields and this shape declares four
//! cells. The two left out are left out on purpose:
//!
//! * **`service`** is [`crate::update::SERVICE_ID`], a compile-time `&str`. It
//!   has no `CellState` and nothing can make it unknown, which is `warnings`'
//!   test for what belongs on `shared` as a constant rather than as a cell.
//! * **`phase`** is always [`crate::update::PHASE_BACK_UP`] —
//!   `compute_boot_note` passes that literal on every path. A cell for it would be
//!   a cell that cannot move, which invites the next author to believe it can and
//!   to fold `going_down` into it (see above). Published as a constant, it is
//!   instead a positive declaration of this shape's scope: **everything here is
//!   about coming up**, and the other phase lives on `narration`.
//!
//! ## What a consumer holds after subscribing, and what it may not conclude
//!
//! `docs/shape-design-guide.md` step 9, stated rather than left to be inferred.
//!
//! * **Keyless subscribe** — an inventory of one: the `daemon` row, plus `shared`.
//! * **Keyed subscribe on `daemon`** — the same one row. The key is minted at
//!   init, so it is never standing interest in a name that does not exist.
//! * **Keyed subscribe on any other key** — a row-less export. There is one
//!   producer and `daemon` is its name.
//! * **`gone`** — never. The key is minted at init and removed by nothing; there
//!   is no code path in this file that calls `remove`.
//! * **No windowed cell**, so no `transitions` frame can exist on this shape,
//!   `MAX_RETAINED_ITEMS` stays `0`, and a `retain: n` is refused `NoRetention`.
//! * **No further frames.** After the export, a consumer will receive nothing
//!   from this shape for the life of the epoch. Silence here means *"this is
//!   still true"*, which is the opposite of what silence means on
//!   [`crate::shapes::heartbeat`]. A consumer must not treat it as a stall.
//!
//! **And the thing a consumer may NOT conclude, which the firehose comment is
//! equally emphatic about:** this is *not* an assertion that a restart just
//! happened. A cold start exports one too. Only the consumer knows whether it saw
//! a different `boot_id` before, so only the consumer can decide this is news —
//! and the `boot_id` cell is what lets it decide.
//!
//! ## What this is NOT
//!
//! * **Not a restart history.** One key, this boot. Past boots are
//!   `restarts.jsonl`, which is not a shape and is not made one here.
//! * **Not liveness.** *"Is the daemon there"* is [`crate::shapes::heartbeat`]'s
//!   question — are frames reaching me — and *"is the socket open"* is the
//!   websocket ping's. This shape answers neither and is silent by design, so
//!   reading its silence as health would be reading the wrong instrument.
//! * **Not the image report.** What commit the running binary was built from is
//!   `daemon.image`, an RPC, and this shape does not duplicate it. `reason` says
//!   *why* this image is the one running, not *what* it is.
//!
//! ## Version
//!
//! [`Boot::VERSION`] is `1`, published on `shared` as `shape_version` identically
//! to the other nine. A NEW shape is additive under rule 2.6, so nothing bumps —
//! see [`crate::shapes::threads`]' module doc for what declaring the number does
//! and does not buy on this producer.

use serde_json::{json, Value};

use crate::event::{Fact, Push};
use shapewire::cell::Cells;
use shapewire::declare_cells;
use shapewire::operation::{
    DType::{Enum, Scalar as Sc, Timestamp as Ts},
    Presence::Required,
    Scalar::{Int, Str},
    TimeEncoding::Rfc3339,
};

use crate::shape::{Hydrate, Input, Map, Register, Root, Shape, Stamped, Timestamp, Tracking, Tx};
use crate::update::LifecycleNote;

/// **THE ONE KEY.** There is one producer to report on, so there is one key.
///
/// Deliberately the same WORD as [`crate::shapes::warnings::KEY_DAEMON`], because
/// it is the same subject — this daemon — and the design guide's step 1 requires
/// two shapes a consumer may join to *"spell the key identically"*. It is this
/// shape's own `const` rather than a re-export, which is the precedent
/// `recent::FLEET` and `nonidle::FLEET` already set: a key constant belongs to
/// the shape that keys by it, so neither shape can move the other's wire.
pub const KEY_DAEMON: &str = "daemon";

/// **WHY THE FOUR CELLS ARE `NotHydrated` RATHER THAN `unknown`** when this
/// process has no note — see the module doc's section on it. Carried as a `const`
/// so the export's reason is one string a test can assert against rather than a
/// sentence duplicated at four call sites.
pub const NO_NOTE_YET: &str =
    "this process has not run `on_boot`, so it has not yet composed an account of its own boot; \
     this is not the same statement as `reason: unknown`, which is a conclusion reached from \
     three measurements";

/// The daemon's one row: this boot's account of itself.
///
/// Every cell is a `Register` — a LEVEL under rule 2.2 — and every one of them is
/// written exactly once per epoch, by the hydrate. There is no second writer; see
/// the module doc on why the fold has no live arm.
#[derive(Debug, Default)]
pub struct BootRow {
    /// Which of [`crate::update::CAUSES`] this boot was. Declared a closed `Enum`,
    /// which is the one declaration here that buys something a reader can lose: a
    /// face that paints differently for a `fallback` compares against the WORD,
    /// and `seamdiff` can only check that comparison against a vocabulary the
    /// producer declares.
    pub reason: Register<String>,
    /// One human sentence. The consumer switches on [`BootRow::reason`] and never
    /// parses this — the same contract as `ServerMsg::Progress::detail` and
    /// `narration`'s lines.
    pub detail: Register<String>,
    /// The boot this row is about — the same id the consumer sees on `welcome`
    /// and on [`crate::shapes::narration`]'s `lifecycle` lines, so it can
    /// correlate all three. It is also the ONLY thing that lets a consumer decide
    /// whether this export is news; see the module doc.
    pub boot_id: Register<String>,
    /// When the note was composed, which is `on_boot`'s instant and therefore
    /// this boot's own start — NOT the export's `as_of`. A consumer reading uptime
    /// off this subtracts from its own clock and should know the two are different
    /// machines' notions of now.
    pub at: Register<String>,
}

pub struct BootRoot {
    pub rows: Map<String, BootRow>,
    pub shape_version: Register<u32>,
    shared_dirty: bool,
}

impl Default for BootRoot {
    fn default() -> Self {
        Self {
            rows: Map::new(),
            shape_version: Register::new(),
            shared_dirty: false,
        }
    }
}

impl BootRoot {
    /// Apply the note, or state that there is not one.
    ///
    /// **ONE FUNCTION FOR BOTH OUTCOMES**, so the present and absent cases cannot
    /// acquire two different ideas of which cells exist — the bug
    /// `shapewire::ledger`'s `carried`/`pending_promises` pair had, and the reason
    /// [`crate::shapes::warnings::DaemonWarningsRoot::apply`] is also one
    /// function.
    ///
    /// **THE KEY IS MINTED EITHER WAY.** The producer exists and is named even
    /// when its account of itself is not composed yet, so `daemon` is a key in
    /// both branches and only the cells differ. Minting it only on success would
    /// make *"no note"* indistinguishable from *"no producer"*, which is the
    /// collapse the three-valued cell state exists to prevent.
    ///
    /// There is no read-before-reach guard here, and that is correct rather than
    /// an omission: `Map::key` dirties on reach, and every other fold on this
    /// producer guards against paying for a repeat — but this runs ONCE per
    /// epoch, from the hydrate, and a hydrate publishes nothing. There is no
    /// frame to avoid.
    fn apply(&mut self, as_of: Timestamp, note: Option<&LifecycleNote>) {
        let row = self.rows.key(KEY_DAEMON.to_string());
        match note {
            Some(n) => {
                row.reason.read(as_of, n.reason.clone());
                row.detail.read(as_of, n.detail.clone());
                row.boot_id.read(as_of, n.boot_id.clone());
                row.at.read(as_of, n.at.clone());
            }
            // **NOT `unknown`.** See the module doc: `unknown` is a conclusion and
            // this is the absence of one.
            None => {
                row.reason.not_hydrated(NO_NOTE_YET);
                row.detail.not_hydrated(NO_NOTE_YET);
                row.boot_id.not_hydrated(NO_NOTE_YET);
                row.at.not_hydrated(NO_NOTE_YET);
            }
        }
    }

    /// Shape-level facts, with an IDEMPOTENT setter: a re-read that finds the same
    /// value must not dirty `shared` (`docs/shape-design-guide.md` step 6).
    fn set_shared(&mut self, at: Timestamp) {
        if self.shape_version.now() != Some(&Boot::VERSION) {
            self.shape_version.emit(at, Boot::VERSION);
            self.shared_dirty = true;
        }
    }
}

impl Root for BootRoot {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }

    fn row(&self, key: &str) -> Option<Value> {
        self.rows.get(&key.to_string()).map(|r| {
            json!({
                "producer": key,
                "reason": r.reason.now(),
                "detail": r.detail.now(),
                "boot_id": r.boot_id.now(),
                "at": r.at.now(),
            })
        })
    }

    fn cells(&self, key: &str) -> Option<Value> {
        self.rows.get(&key.to_string()).map(|r| {
            json!({
                "reason": r.reason.state(),
                "detail": r.detail.state(),
                "boot_id": r.boot_id.state(),
                "at": r.at.state(),
            })
        })
    }

    /// The daemon's constants, so no consumer hardcodes them.
    ///
    /// `phase` and `service` are HERE rather than on the row, and the module doc
    /// carries the argument: both are compile-time constants with no `CellState`,
    /// and `phase` in particular is a positive declaration that this shape serves
    /// only the coming-up half. `causes` publishes the vocabulary `reason` is an
    /// `Enum` over, so a face need not embed the four words.
    fn shared(&self) -> Value {
        json!({
            "shape_version": self.shape_version.now(),
            "key": KEY_DAEMON,
            "service": crate::update::SERVICE_ID,
            "phase": crate::update::PHASE_BACK_UP,
            "causes": crate::update::CAUSES,
        })
    }

    fn shared_cells(&self) -> Value {
        json!({ "shape_version": self.shape_version.state() })
    }

    fn drain(&mut self) -> crate::shape::Outbox {
        crate::shape::Outbox {
            rows: self.rows.take_dirty_keys().into_iter().collect(),
            gone: self.rows.take_removed(),
            added: self.rows.take_added(),
            // No windowed cell here, so no appended item can exist — which is
            // also why `MAX_RETAINED_ITEMS` stays 0 and a `retain: n` on this
            // shape is refused `NoRetention`.
            transitions: Vec::new(),
            shared: std::mem::take(&mut self.shared_dirty),
        }
    }

    fn mark_dead(&mut self, reason: &str) {
        self.shape_version.mark_dead(reason);
        self.rows.mark_dead(reason);
        for (_, r) in self.rows.entries_mut() {
            r.reason.mark_dead(reason);
            r.detail.mark_dead(reason);
            r.boot_id.mark_dead(reason);
            r.at.mark_dead(reason);
        }
    }
}

declare_cells! {
    /// **THE PER-KEY CELLS OF `boot`** — [`crate::update::LifecycleNote`]'s four
    /// non-constant fields, each a LEVEL.
    ///
    /// `reason` is a closed `Enum` over [`crate::update::CAUSES`] rather than a
    /// string, and that is the one declaration here that buys something a reader
    /// can lose — the same argument `warnings`' `kinds.level` makes. A face that
    /// paints a `fallback` differently compares against the WORD, and `seamdiff`
    /// can only check that comparison against a vocabulary the producer declares.
    /// Undeclared, `== "fallback"` is a predicate that may be false for ever and
    /// reads, to anyone above it, like an answer.
    ///
    /// `at` is a `Timestamp` and not a string because what it is for is
    /// arithmetic: it is this boot's start, and a reader that cannot parse it
    /// cannot tell a daemon that came up four seconds ago from one that came up
    /// four days ago.
    ///
    /// All four are `Required` — *never null once known*. They are known together
    /// or unknown together, because they come from one `Option` read in one
    /// statement; there is no reachable state in which three are known and one is
    /// not.
    pub BOOT_CELLS = [
        ("reason", Enum(crate::update::CAUSES), Required, []),
        ("detail", Sc(Str), Required, []),
        ("boot_id", Sc(Str), Required, []),
        ("at", Ts(Rfc3339), Required, []),
    ]
}

declare_cells! {
    /// **THE SHAPE-LEVEL CELLS OF `boot`.**
    ///
    /// The set is exactly [`BootRoot::shared_cells`]' keys, both directions, which
    /// `tests/cell_manifest.rs` asserts. The rest of what [`BootRoot::shared`]
    /// publishes — `key`, `service`, `phase`, `causes` — are the daemon's
    /// CONSTANTS, not cells: they have no `CellState` and nothing can make them
    /// unknown.
    pub BOOT_SHARED = [("shape_version", Sc(Int), Required, [])]
}

pub struct Boot;

impl Boot {
    /// See [`crate::shapes::threads`]' module doc for what a declared version is
    /// and is not on this producer.
    pub const VERSION: u32 = 1;
}

impl Shape<Input> for Boot {
    const NAME: &'static str = "boot";
    const CELLS: Cells = BOOT_CELLS;
    const SHARED: Cells = BOOT_SHARED;
    /// **`Lazy`.** No subscriber, no root — and the note lives outside the root,
    /// so the idle costs nothing and loses nothing. See the module doc; the reason
    /// differs from every other shape's and is stated there rather than assumed.
    const TRACKING: Tracking = Tracking::Lazy;
    /// **`Whole`.** One key, minted at init, and there is never a second — so
    /// "every key I know about" and "every key there is" are the same key, which
    /// is what `Whole` claims. A keyless subscribe here is an inventory of one.
    const HYDRATE: Hydrate = Hydrate::Whole;
    /// **`0`, AND IT IS A DECLARATION RATHER THAN A DEFAULT.** The fold has no arm
    /// that writes, so this shape's legitimate fan-out from one input is nothing —
    /// which makes the backstop useful instead of decorative: the first derived
    /// event this fold ever produces trips the burst warning. See the module doc.
    const BURST: usize = 0;

    type Producer = crate::runtime::ThreadManager;
    type Root = BootRoot;

    fn root(&self) -> BootRoot {
        BootRoot::default()
    }

    /// **NO HYDRATE REQUEST, for `warnings`' reason rather than `narration`'s.**
    ///
    /// There is nothing to route a request for: the note is this process's own
    /// memory, reachable directly through
    /// [`crate::update::UpdateGate::boot_note`], and a request through
    /// `Producer::hydrate` would be ceremony around a field read. So, like
    /// `warnings` and `heartbeat`, this shape has no `Handles` impl and
    /// `docs/shapes.md` §4.4's *"the producer answers every request the shape
    /// makes"* is vacuous here — stated rather than left to be noticed.
    ///
    /// **This is the ONLY writer of this shape's cells, in either direction.**
    fn hydrates(
        &mut self,
        p: &crate::runtime::ThreadManager,
        root: &mut BootRoot,
        as_of: Timestamp,
    ) {
        root.set_shared(as_of);
        // ONE read of the gate. `apply` mints the key in both arms, so the
        // producer is named whether or not it can yet account for itself.
        let note = p.update.boot_note();
        root.apply(as_of, note.as_ref());
        // **COMPLETE EITHER WAY.** The key set is complete even when the cells are
        // `NotHydrated`: there is one producer and we know its name. Completeness
        // is a claim about the KEYS, and `Map::mark_not_hydrated` would be the
        // claim that we could not enumerate them, which is never true here.
        root.rows.mark_read(as_of, true);
        // A hydrate publishes nothing — the export IS the publication.
        let _ = root.drain();
    }

    /// **EXHAUSTIVE, WITH NO `_` ARM**, like the other nine: a new input variant
    /// must be a compile error here rather than a silent non-fold.
    ///
    /// **AND EVERY ARM IS EMPTY.** This is the only fold on this producer that
    /// writes nothing, and the module doc carries the argument: the note is
    /// computed once, before anything could be listening, and never mutated — so
    /// there is no input whose arrival could move a cell, and an arm that wrote
    /// one would be writing a value with no source.
    ///
    /// **It reads no clock and takes no stamp**, which is the no-clock rule
    /// satisfied in the strongest available way: a body that reads no instant at
    /// all cannot read the wrong one. Note that `tx.at()` is therefore absent
    /// here, so the source-scan sentinel other folds use (*"folds must take the
    /// stamp from the input"*) does not apply and
    /// `tests/boot_shape.rs::the_fold_writes_nothing_and_reads_no_clock` carries
    /// its own.
    fn folds(&mut self, input: &Stamped<Input>, _tx: &mut Tx<'_, BootRoot>) {
        match &input.value {
            // ── homed elsewhere, every one of them, INCLUDING THE ONE THAT
            //    LOOKS LIKE THIS SHAPE'S OWN ──────────────────────────────
            //
            // `Push::Lifecycle` is `crate::shapes::narration`'s `lifecycle`
            // channel and must stay there. It carries the GOING-DOWN note, which
            // is a transition emitted while the socket is still open — so it is
            // reachable by broadcast and never needed a replay. Folding it here
            // would break rule 7 ("one way to know a thing") and would be
            // incoherent besides: the cell would be a level whose value is erased
            // by the next boot's hydrate, which is to say erased by the
            // completion of the very event it announced.
            //
            // Everything else is somebody's: `Event`/`Status`/`Liveness` are
            // `threads`', the account facts are `accounts`', the beat is
            // `heartbeat`'s, the warning sample is `warnings`'.
            Input::Push(Push::Event(_))
            | Input::Push(Push::Status { .. })
            | Input::Push(Push::Liveness { .. })
            | Input::Push(Push::Progress { .. })
            | Input::Push(Push::Lifecycle(_))
            | Input::Push(Push::Info { .. })
            | Input::Fact(Fact::Beat { .. })
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
            | Input::Fact(Fact::ThreadHeader { .. })
            | Input::Fact(Fact::ThreadSettings { .. })
            | Input::Fact(Fact::ThreadPlacement { .. })
            | Input::Fact(Fact::ThreadSeen { .. })
            | Input::Fact(Fact::ThreadCompaction { .. })
            | Input::Fact(Fact::ThreadQueue { .. })
            | Input::Fact(Fact::ThreadChildren { .. })
            | Input::Fact(Fact::ThreadRolled { .. })
            | Input::Fact(Fact::ThreadSubagents { .. })
            | Input::Fact(Fact::ThreadChildActivity { .. })
            | Input::Fact(Fact::WarningsSampled { .. }) => {}
        }
    }
}
