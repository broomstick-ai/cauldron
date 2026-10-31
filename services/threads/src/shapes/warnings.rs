//! # The `warnings` shape — the producer reporting on ITSELF
//!
//! The operator's framing, which is the requirement and not a nice-to-have:
//!
//! > *"reporting itself means it needs to send a warning to its consumer until we
//! > have centralized cauldron warning aggregation."*
//!
//! [`crate::warnings`] holds the counter and the disk record and argues the
//! mechanism. This file is the half that reaches a consumer, and it exists
//! because a count in a producer's memory that no consumer can read is the 47 MB
//! log file again with extra steps.
//!
//! ## A key is: THE PRODUCER. There is one, and it is `daemon`
//!
//! `docs/shape-design-guide.md` step 1 says to write the sentence *"a key is …"*
//! first, because every later choice depends on it. Here it is the awkward one:
//! this is a fact about **the producer itself**, which no shape on this daemon had
//! as a subject before.
//!
//! | shape | a key is |
//! |---|---|
//! | `accounts` | a pool account |
//! | `threads` | a thread |
//! | `threads/summary` | a thread that has a name |
//! | `narration` | a channel the daemon talks on |
//! | `threads/recent-updates` | the one pool, `fleet` |
//! | `heartbeat` | the one link, `link` |
//! | **`warnings`** | **the one producer, `daemon`** |
//!
//! The nearest precedents are `heartbeat`'s `link` and `recent`'s `fleet` — the
//! guide's *"a single thing is a pool with one fixed key"*. There is nothing to
//! key this by: a warning is not per account (most are not about one), not per
//! thread (the 29,644 named a thread each and the FACT is about the daemon that
//! kept emitting them), and not per connection (a shape has one root shared by
//! every subscriber).
//!
//! ## WHY A NEW SHAPE, and the three candidates that were rejected
//!
//! **`heartbeat`, rejected, and it was the closest call.** It already has one
//! fixed key, is already about the producer rather than any account or thread, is
//! already `Lazy`, and already runs a 1 Hz task — so riding its beat would have
//! cost no new machinery at all. Three things killed it:
//!
//! 1. Its module doc spends a page keeping *"is the socket open"*, *"is this
//!    thread busy"* and *"are frames reaching me"* apart. A fourth, unrelated
//!    question on the same shape is the collapse rule 8 names.
//! 2. **The two folds need opposite rules.** `heartbeat.seq` MUST NOT dedupe — the
//!    arrival is the whole content, and a heartbeat that dropped a repeat would
//!    emit one frame and then go silent forever. A warning count MUST dedupe, or
//!    it is a frame per second forever saying nothing changed. Both rules in one
//!    `folds` is how one of them gets "fixed" by analogy later.
//! 3. **Charging.** A consumer subscribed to `heartbeat` receives one frame per
//!    second for the life of the link, by design. This shape is SILENT while
//!    nothing is wrong. Folding them would price the quiet answer at the noisy
//!    one's rate, and the quiet is the feature.
//!
//! **`narration`, rejected, and it is the best *semantic* fit.** It is literally
//! *"the daemon talking while it works"*. But its cells are `Sequence`s — bounded
//! windows of 64 lines over 1 hour — and a count of 29,644 cannot live in a
//! 64-item window. Under rule 2.2 a count is a LEVEL and a narration line is a
//! TRANSITION; they route down different paths with no dial, and putting both in
//! one shape mixes the two delivery modes the rule exists to separate. A consumer
//! wanting only the number would also be charged for three 64-line windows.
//!
//! **A cell on `accounts`, rejected, and it was the CHEAPEST.** The Python board
//! already mirrors exactly one shape and already reads `accounts.shared`, so a
//! number there would have reached the face with no new subscription and a
//! one-line board diff. It is still wrong: the AUI does not care about accounts,
//! and a design where *"is the daemon healthy"* is only answerable by subscribing
//! to the account board is a shape encoding one application's layout. The board
//! pays a second mirror instead; that cost is real and it is the right one.
//!
//! ## `Lazy`, and what that does NOT reset
//!
//! `Lazy` for the usual reason — no subscriber, no FIFO, no root — plus a sharper
//! one: the sampler checks `hub.tracked` before it emits, so an unwatched daemon
//! does no sampling work at all, and this is an input that arrives whether or not
//! anything happened.
//!
//! **The COUNTERS are not in the root and do not reset with it.** They are
//! process-global in [`crate::warnings::COUNTER`]. So a `Lazy` drop and re-init
//! loses nothing: the next hydrate reads the live totals and the first export is
//! correct. This is deliberately NOT a `Counter` cell —
//! `docs/shape-design-guide.md` step 5 is explicit that a `Lazy` shape holding a
//! `Counter` *"compiles, registers and resets its count on every idle without a
//! warning"*, and a counter that silently resets is the exact lie about 29,644
//! this feature exists to stop telling. The honest form the guide names is a
//! `Register` with a NAMED WRITER SET — hydrate reads the global, the fold
//! re-reads it — which is what this is, and the producer genuinely retains the
//! facts, so the `Counter` escape clause does not apply.
//!
//! Because the epoch resets and the counts do not, `shared.counts_since` is NOT
//! optional: it carries the DAEMON's start, so a consumer never reads
//! `epoch.init_at` as the counting window and under-reports by everything that
//! happened while nothing was watching.
//!
//! ## The fold DEDUPES, and that is what makes zero frames the quiet answer
//!
//! `Map::key` dirties on reach, so every arm reads with `get` first and reaches
//! only when a number actually moved (`docs/shape-design-guide.md` step 6,
//! *"read before reaching"*). At the sample interval that means:
//!
//! * nothing warned → **no frame at all**, forever, at zero cost;
//! * something warned → one frame, carrying the new totals.
//!
//! `a_sample_that_repeats_the_held_tally_costs_no_frame` is the test, and it is
//! forced red by reaching for the key before comparing.
//!
//! ## Zero is `Known(0)`, and absence is the FACE's job
//!
//! The export always carries `warn` and `error`, and at boot both are a
//! three-valued `Known` zero — not absent, not `NotHydrated`. *"Prove zero reads
//! as zero"* is a producer obligation, and a shape that omitted the cell until
//! something went wrong could not distinguish *"nothing has gone wrong"* from
//! *"this daemon does not count"*.
//!
//! Hiding a zero is the FACE's decision and both faces take it: the board's
//! header segment and the TUI's header part are absent at zero and appear only
//! when the number is real. A number permanently on screen becomes wallpaper,
//! which is how the log failed; a number that appears is the one he looks at.
//!
//! ## Version
//!
//! [`Warnings::VERSION`] is `1`, published on `shared` as `shape_version`
//! identically to the other six. See [`crate::shapes::threads`]' module doc for
//! what declaring the number does and does not buy on this producer.

use serde_json::{json, Value};

use crate::event::{Fact, Push};
use shapewire::cell::Cells;
use shapewire::declare_cells;
use shapewire::operation::{
    DType::{Enum, List, Opaque, Scalar as Sc, Struct, Timestamp as Ts},
    Presence::Required,
    Scalar::{Bool, Int, Str},
    TimeEncoding::Rfc3339,
};

use crate::shape::{Hydrate, Input, Map, Register, Root, Shape, Stamped, Timestamp, Tracking, Tx};
use crate::warnings::Tally;

/// **THE ONE KEY.** There is one producer to report on, so there is one key.
pub const KEY_DAEMON: &str = "daemon";

/// **HOW OFTEN THE COUNTERS ARE SAMPLED** — once per second.
///
/// The same period as [`crate::shapes::heartbeat::BEAT_INTERVAL_MS`], and for a
/// cheaper reason than symmetry: it is already the daemon's fastest periodic
/// input, so this shape existing does not make anything faster. The sample costs
/// two atomic loads and a `BTreeMap` walk bounded by the number of `warn!`
/// statements in the crate, and because the fold dedupes, a quiet second produces
/// **no frame**. The rate a consumer actually sees is the rate the daemon is
/// warning at, which is the honest thing for it to be.
pub const SAMPLE_INTERVAL_MS: u64 = 1_000;

/// The daemon's own row: the counts, the breakdown, and how the disk half is
/// faring.
#[derive(Debug, Default)]
pub struct DaemonRow {
    /// `WARN` events since [`DaemonWarningsRoot::counts_since`].
    pub warn: Register<u64>,
    /// `ERROR` events since the same instant. Apart from `warn`, never summed —
    /// see [`crate::warnings::Tally::error`].
    pub error: Register<u64>,
    /// The top [`crate::warnings::TOP_KINDS`] call sites, descending by count,
    /// plus every `error` kind whatever its rank (WARN-1 D-F).
    ///
    /// A `Register<Value>` and NOT a `Map<String, Register<u64>>`, which is the
    /// form `docs/shape-design-guide.md` step 3 names for a small fixed
    /// vocabulary. Two reasons it is not that here: the value is a **ranking**,
    /// which is a derived ORDER over the whole set that a per-key map cannot
    /// express without the consumer re-sorting; and a `Map` would export all
    /// hundred-odd call sites once each had fired, where the whole point is the
    /// few that dominate. The producer knows the counts, so the producer ranks.
    pub kinds: Register<Value>,
    /// How many distinct call sites have fired — the denominator [`DaemonRow::kinds`]
    /// is a truncated numerator of. See [`crate::warnings::Tally::kinds_total`] for
    /// why a shape about dropped information may not drop information silently.
    pub kinds_total: Register<u64>,
    /// Failed writes to the disk record. Carried to the consumer BECAUSE it
    /// cannot be logged — see [`crate::warnings`]'s reentrancy section.
    pub sink_errors: Register<u64>,
    /// **THE LAST [`crate::warnings::LAST_ERRORS`] ERROR RECORDS (WARN-1 D-F)**,
    /// oldest first: `at`, `target`, `line`, `message`, `event_fields`. So a face or a
    /// deploy monitor can NAME an error without reading a file — on 2026-10-28
    /// prod counted 17 errors and no surface could say what any of them was.
    pub last_errors: Register<Value>,
}

pub struct DaemonWarningsRoot {
    pub rows: Map<String, DaemonRow>,
    pub shape_version: Register<u32>,
    /// **WHEN THE COUNTING STARTED** — the daemon's `started_at`, never the
    /// shape's epoch. See the module doc; this is the field that keeps a `Lazy`
    /// re-init from reading as a reset.
    pub counts_since: Register<String>,
    /// Whether a disk record is attached. A different statement from
    /// `sink_errors == 0`.
    pub sink_attached: Register<bool>,
    shared_dirty: bool,
}

impl Default for DaemonWarningsRoot {
    fn default() -> Self {
        Self {
            rows: Map::new(),
            shape_version: Register::new(),
            counts_since: Register::new(),
            sink_attached: Register::new(),
            shared_dirty: false,
        }
    }
}

impl DaemonWarningsRoot {
    /// Apply a tally, reaching for the key ONLY if something moved.
    ///
    /// One function so the hydrate and the fold cannot apply two policies to the
    /// same numbers — the bug `shapewire::ledger`'s `carried`/`pending_promises`
    /// pair had, where a record's write path and read path each had their own idea
    /// of the rule.
    ///
    /// Returns whether anything changed, so a caller can tell a real sample from a
    /// repeat without re-deriving it.
    fn apply(&mut self, at: Timestamp, t: &Tally, hydrating: bool) -> bool {
        let kinds = serde_json::to_value(&t.kinds).unwrap_or(Value::Null);
        let last_errors = serde_json::to_value(&t.last_errors).unwrap_or(Value::Null);
        // **READ BEFORE REACHING.** `Map::key` dirties on reach, so a guard after
        // it has already cost the frame it exists to avoid.
        let held = self.rows.get(&KEY_DAEMON.to_string());
        let moved = match held {
            None => true,
            Some(r) => {
                r.warn.now() != Some(&t.warn)
                    || r.error.now() != Some(&t.error)
                    || r.kinds.now() != Some(&kinds)
                    || r.kinds_total.now() != Some(&t.kinds_total)
                    || r.sink_errors.now() != Some(&t.sink_errors)
                    || r.last_errors.now() != Some(&last_errors)
            }
        };
        if !moved {
            return false;
        }
        let row = self.rows.key(KEY_DAEMON.to_string());
        if hydrating {
            row.warn.read(at, t.warn);
            row.error.read(at, t.error);
            row.kinds.read(at, kinds);
            row.kinds_total.read(at, t.kinds_total);
            row.sink_errors.read(at, t.sink_errors);
            row.last_errors.read(at, last_errors);
        } else {
            row.warn.emit(at, t.warn);
            row.error.emit(at, t.error);
            row.kinds.emit(at, kinds);
            row.kinds_total.emit(at, t.kinds_total);
            row.sink_errors.emit(at, t.sink_errors);
            row.last_errors.emit(at, last_errors);
        }
        true
    }

    /// Shape-level facts, with IDEMPOTENT setters: a re-read that finds the same
    /// value must not dirty `shared` (`docs/shape-design-guide.md` step 6).
    fn set_shared(&mut self, at: Timestamp, counts_since: &str, attached: bool) {
        if self.shape_version.now() != Some(&Warnings::VERSION) {
            self.shape_version.emit(at, Warnings::VERSION);
            self.shared_dirty = true;
        }
        if self.counts_since.now().map(String::as_str) != Some(counts_since) {
            self.counts_since.emit(at, counts_since.to_string());
            self.shared_dirty = true;
        }
        if self.sink_attached.now() != Some(&attached) {
            self.sink_attached.emit(at, attached);
            self.shared_dirty = true;
        }
    }
}

impl Root for DaemonWarningsRoot {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }

    fn row(&self, key: &str) -> Option<Value> {
        self.rows.get(&key.to_string()).map(|r| {
            json!({
                "producer": key,
                "warn": r.warn.now(),
                "error": r.error.now(),
                "kinds": r.kinds.now(),
                "kinds_total": r.kinds_total.now(),
                "sink_errors": r.sink_errors.now(),
                "last_errors": r.last_errors.now(),
            })
        })
    }

    fn cells(&self, key: &str) -> Option<Value> {
        self.rows.get(&key.to_string()).map(|r| {
            json!({
                "warn": r.warn.state(),
                "error": r.error.state(),
                "kinds": r.kinds.state(),
                "kinds_total": r.kinds_total.state(),
                "sink_errors": r.sink_errors.state(),
                "last_errors": r.last_errors.state(),
            })
        })
    }

    /// The daemon's constants, so no consumer hardcodes them, plus the two facts
    /// that stop the numbers being misread: `counts_since` and `sink_attached`.
    fn shared(&self) -> Value {
        json!({
            "shape_version": self.shape_version.now(),
            "key": KEY_DAEMON,
            "counts_since": self.counts_since.now(),
            "sample_interval_ms": SAMPLE_INTERVAL_MS,
            "top_kinds": crate::warnings::TOP_KINDS,
            "sink_attached": self.sink_attached.now(),
            "sink_dir": crate::warnings::WARNINGS_DIR,
            "sink_file": crate::warnings::WARNINGS_FILE,
            "errors_file": crate::warnings::ERRORS_FILE,
            "last_errors_max": crate::warnings::LAST_ERRORS,
        })
    }

    fn shared_cells(&self) -> Value {
        json!({
            "shape_version": self.shape_version.state(),
            "counts_since": self.counts_since.state(),
            "sink_attached": self.sink_attached.state(),
        })
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
        self.counts_since.mark_dead(reason);
        self.sink_attached.mark_dead(reason);
        self.rows.mark_dead(reason);
        for (_, r) in self.rows.entries_mut() {
            r.warn.mark_dead(reason);
            r.error.mark_dead(reason);
            r.kinds.mark_dead(reason);
            r.kinds_total.mark_dead(reason);
            r.sink_errors.mark_dead(reason);
            r.last_errors.mark_dead(reason);
        }
    }
}

declare_cells! {
    /// **THE PER-KEY CELLS OF `warnings`** — the two counts, the breakdown, and
    /// how the disk half is faring.
    ///
    /// `warn` and `error` are declared APART, because they are apart on the row
    /// and summing them is the face's choice and not the producer's
    /// ([`crate::warnings::Tally::error`]).
    ///
    /// `level` is a closed `Enum` rather than a string, and that is the one
    /// declaration here that buys something a reader can lose: a face that paints
    /// red once anything was an `error` compares against the WORD, and `seamdiff`
    /// can only check that comparison against a vocabulary the producer declares.
    /// Undeclared, `== "error"` is a predicate that is false for ever and reads,
    /// to anyone above it, like an answer.
    pub WARNINGS_CELLS = [
        ("warn", Sc(Int), Required, []),
        ("error", Sc(Int), Required, []),
        ("kinds", List(&Struct("Kind")), Required, [
            ("target", Sc(Str), Required),
            ("line", Sc(Int), Required),
            ("level", Enum(crate::warnings::LEVELS), Required),
            ("count", Sc(Int), Required),
            ("message", Sc(Str), Required),
        ]),
        ("kinds_total", Sc(Int), Required, []),
        ("sink_errors", Sc(Int), Required, []),
        // WARN-1 D-F. `event_fields` is the event's own key/value set, which differs
        // per call site: stored and returned, never branched on.
        ("last_errors", List(&Struct("ErrorRecord")), Required, [
            ("at", Ts(Rfc3339), Required),
            ("target", Sc(Str), Required),
            ("line", Sc(Int), Required),
            ("message", Sc(Str), Required),
            ("event_fields", Opaque, Required),
        ]),
    ]
}

declare_cells! {
    /// **THE SHAPE-LEVEL CELLS OF `warnings`.**
    ///
    /// The set is exactly [`DaemonWarningsRoot::shared_cells`]' keys, both
    /// directions, which `tests/cell_manifest.rs` asserts. The rest of what
    /// [`DaemonWarningsRoot::shared`] publishes — `key`, `sample_interval_ms`,
    /// `top_kinds`, `sink_dir`, `sink_file` — are the daemon's CONSTANTS, not
    /// cells: they have no `CellState` and nothing can make them unknown.
    ///
    /// `counts_since` is a `Timestamp` and not a string, because what it is for is
    /// arithmetic: it is the only thing that lets a consumer tell a counter that
    /// RESET from a count that fell to zero, and a reader that cannot parse it
    /// cannot do that.
    pub WARNINGS_SHARED = [
        ("shape_version", Sc(Int), Required, []),
        ("counts_since", Ts(Rfc3339), Required, []),
        ("sink_attached", Sc(Bool), Required, []),
    ]
}

pub struct Warnings;

impl Warnings {
    /// See [`crate::shapes::threads`]' module doc for what a declared version is
    /// and is not on this producer.
    pub const VERSION: u32 = 1;
}

impl Shape<Input> for Warnings {
    const NAME: &'static str = "warnings";
    const CELLS: Cells = WARNINGS_CELLS;
    const SHARED: Cells = WARNINGS_SHARED;
    /// **`Lazy`.** No subscriber, no sampling — and the counters live outside the
    /// root, so the idle costs nothing and loses nothing. See the module doc.
    const TRACKING: Tracking = Tracking::Lazy;
    /// **`Whole`.** One key, minted at init, and there is never a second — so
    /// "every key I know about" and "every key there is" are the same key, which
    /// is what `Whole` claims. A keyless subscribe here is an inventory of one.
    const HYDRATE: Hydrate = Hydrate::Whole;
    /// One sample touches one key: one `row`, plus `shared` on the first.
    const BURST: usize = 2;

    type Producer = crate::runtime::ThreadManager;
    type Root = DaemonWarningsRoot;

    fn root(&self) -> DaemonWarningsRoot {
        DaemonWarningsRoot::default()
    }

    /// **NO HYDRATE REQUEST, for `heartbeat`'s reason rather than `narration`'s.**
    ///
    /// There is nothing durable to READ: the counters are this process's own
    /// memory, reachable directly, and a request routed through
    /// `Producer::hydrate` would be ceremony around a static. So, like
    /// `heartbeat`, this shape has no `Handles` impl and `docs/shapes.md` §4.4's
    /// *"the producer answers every request the shape makes"* is vacuous here —
    /// stated rather than left to be noticed.
    ///
    /// What IS read off the producer is `started_at`, for `counts_since`.
    fn hydrates(
        &mut self,
        p: &crate::runtime::ThreadManager,
        root: &mut DaemonWarningsRoot,
        as_of: Timestamp,
    ) {
        let t = crate::warnings::COUNTER.tally();
        root.set_shared(as_of, &p.started_at, t.sink_attached);
        // **ZERO IS HYDRATED AS A KNOWN ZERO.** `apply` returns false for an
        // unchanged tally, and on a cold root there is no held value, so the key
        // is always minted here — a daemon that has warned zero times still
        // exports `warn: 0` in state `Known` rather than omitting the cell. See
        // the module doc.
        root.apply(as_of, &t, true);
        root.rows.mark_read(as_of, true);
        // A hydrate publishes nothing — the export IS the publication.
        let _ = root.drain();
    }

    /// **EXHAUSTIVE, WITH NO `_` ARM**, like the other six: a new input variant
    /// must be a compile error here rather than a silent non-fold.
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, DaemonWarningsRoot>) {
        let at = tx.at();
        match &input.value {
            Input::Fact(Fact::WarningsSampled { tally }) => {
                // `apply` does the read-before-reach, so a sample that repeats the
                // held tally costs no frame. That is what makes a quiet daemon
                // silent on this shape rather than 1 Hz forever.
                tx.apply(at, tally, false);
            }

            // ── homed elsewhere, every one of them ───────────────────────
            //
            // This shape is about the PRODUCER. Nothing about an account, a thread
            // or a channel belongs here, and a beat least of all: `heartbeat`
            // answers whether frames are flowing, which is a different question
            // with a different key.
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
            // A queue movement is not a warning. This shape counts what the
            // producer COMPLAINED about, and a parked or failed delivery already
            // has its own voice: `ThreadEvent::RetryStarving` and the narration
            // channels. Counting it here would double-report it.
            | Input::Fact(Fact::ThreadQueue { .. })
            | Input::Fact(Fact::ThreadChildren { .. })
            | Input::Fact(Fact::ThreadRolled { .. })
            | Input::Fact(Fact::ThreadSubagents { .. })
            | Input::Fact(Fact::ThreadChildActivity { .. }) => {}
        }
    }
}
