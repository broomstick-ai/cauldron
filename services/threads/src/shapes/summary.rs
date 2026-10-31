//! # The `threads/summary` shape — an APPLICATION-SPECIFIC shape, and the line
//!
//! **This file interprets one application's vocabulary, and that is the only
//! reason it is a separate shape.** Operator ruling, recorded in
//! `docs/consumer-migration-ledger.md` before the work that implements it:
//!
//! > *"Application logic belongs in application-specific shapes. The general
//! > `threads` shape carries what any consumer of a thread system wants —
//! > header, status, placement, durable positions, and app event payloads held
//! > opaquely. Anything that INTERPRETS those payloads — the summary/title
//! > derivation being the example — is a separate application-specific shape. A
//! > general shape must not encode one app's meaning."*
//!
//! ## Where the line falls, stated once
//!
//! | side | what is on it |
//! |---|---|
//! | GENERAL (`threads`) | `app: Map<event_type, Register<Value>>` — every custom event's newest payload, stored and never read. It knows that the application writes events and that they have types. It knows nothing about what any type MEANS |
//! | APPLICATION (this file) | `summary_updated` is a thread's NAME; its payload's `summary` field is the title and its `idx` field is the app's own coordinate |
//!
//! Every judgement in the right-hand column is one an application made, and the
//! test of whether the line held is mechanical: **the string `"summary_updated"`
//! appears in this crate's EXECUTABLE code only in this file** (as
//! [`SUMMARY_EVENT_TYPE`] and the two comparisons against it). Every other
//! occurrence under `lib/threads/src` — in `event.rs`, in `import.rs`, in
//! `shapes/threads.rs`, in `shapes/mod.rs` — is a doc comment naming it as an
//! example. Grep it before adding a sixth.
//!
//! **A thread has no title column.** `schema.sql`'s `threads` table is
//! `{id, account, cwd, model, settings_json, last_seq, next_message_id,
//! last_compaction_message_id, needs_restate, last_meta_date, created_at,
//! updated_at}` — no title, no label, no tags. The human-readable name of a
//! thread is the newest `custom` event of type `summary_updated`, written by the
//! application through `threads.event`, and any consumer that wants to list
//! threads by name today must walk a thread's ledger to find it. That is what
//! this shape buys, and it is app logic, not a header field.
//!
//! ## Nested under `threads`
//!
//! `PARENT = Some("threads")` and `NAME = "threads/summary"`, which `Hub::
//! register` enforces (a name that does not spell its parent, or a parent that is
//! not registered yet, is a panic in the producer's build function — a daemon
//! that does not boot rather than a consumer that subscribes to something
//! incoherent).
//!
//! Nesting is the right relationship and it is cheap: a shape nobody subscribes
//! to costs nothing under `Tracking::Lazy`, so a consumer that wants threads
//! without app meaning pays for none of this, and a consumer that wants only
//! titles subscribes to the child and never the parent. The two are INDEPENDENT
//! for liveness and for re-init; `Epoch.parent_epoch` on every frame this shape
//! emits carries the parent's era as of this child's last init, so a consumer
//! holding both mirrors can tell whether it may read them together.
//!
//! **The child's keys are not a subset of the parent's.** The parent holds keys
//! on demand (usually none); this shape holds the named threads. Both spell a key
//! as a bare thread id, which is what makes them joinable at all —
//! `docs/chassis-resources.md` 1.1 leaves how a child spells `(parent_key,
//! own_key)` to the producer, and here the pair collapses because a summary is
//! about exactly one thread.
//!
//! ## Keys and the hydrate bound
//!
//! **A key is a thread that HAS a name.** `Hydrate::Whole`: the key set is read
//! at init, in one statement. That the set is the 500 most recently named
//! threads rather than every thread that was ever named is a COMPLETENESS fact,
//! not a scope axis, and it is stated three times where it can change per
//! epoch — `Hydration::Partial`, `complete: false` on the row set's cell state,
//! and `shared.truncated`. It used to be stated a fourth time as
//! `KeyScope::Active`, a const that could not change when the world did; see
//! the declaration on `impl Shape for Summary`. 366 `summary_updated` events
//! exist across 179 threads in the production store, so today the cap is not
//! near.
//!
//! **The bound is [`MAX_SUMMARY_ROWS`] = 500 rows from ONE statement, and the
//! read reports `Partial` when the cap bites** — never `Complete`, which is the
//! short-read-that-claims-to-be-whole failure `tests/hydrate_bound.rs` exists
//! for. 500 is `server::QUERY_MAX_LIMIT`, the number this daemon already calls
//! one page of an archive read, and at ~250 bytes per title row it is ~125 KB
//! and 500 frames against a per-connection channel of 1,024 — the only bound
//! that matters here is the FRAME count, because `docs/thread-shapes-plan.md`
//! §4 measured that for a thread-keyed shape it is the exports and not the SQL
//! that kills a keyless subscribe.
//!
//! The statement itself is a group-by over `events` filtered on `kind='custom'`
//! and the payload's `event_type`; `idx_events_thread_kind` is `(thread_id,
//! kind)`, so a fleet-wide read on `kind` alone cannot use it and this scans.
//! ONCE per shape epoch, with no N+1 anywhere. **An index on `(kind, id)` would
//! turn it into a seek and is deliberately not added**: an index migration on a
//! 2.46 GB table is exactly the ledger-migration work the operator put out of
//! scope for this dispatch.
//!
//! **"ONE statement" WAS FALSE UNTIL 2026-10-02, AND THE COUNTER COULD NOT SEE
//! IT.** `Store::newest_app_events` ran the group-by TWICE — once wrapped in
//! `SELECT COUNT(*)` for the total and once for the page — so this hydrate was
//! two full scans of `events`, measured at ~3.5 s each cold on the production
//! store rather than the 0.85–0.94 s recorded here, and ~7 s in total. Both
//! statements rode ONE connection acquisition, and `store::db_trips()` counts
//! acquisitions, so every cost test in the repo agreed with the wrong number.
//! The read is now one statement — `COUNT(*) OVER ()` carries the total on the
//! page's own rows — and `tests/db_trips_metric.rs` pins that structurally,
//! because the runtime counter still cannot.
//!
//! ## One known gap, named because it is reachable here
//!
//! `docs/shapes.md` §7.2 — a keyless subscribe enumerates the keys the root
//! holds AT THAT MOMENT, and a key created later produces `row` frames that
//! `drive`'s gate correctly drops. The parent shape cannot reach that gap
//! (`OnDemand`, and its fold mints nothing); **this shape can**, because a thread
//! being named for the first time mints its key. The consequence is bounded and
//! is the pre-existing one: a keyless subscriber does not hear about a
//! newly-named thread until it re-subscribes. Closing it is a change to
//! `shapewire::hub`, which a sibling dispatch owns, and it is the one thing
//! in this shape that a chassis fix would improve.

use serde::Serialize;
use serde_json::{json, Value};

use crate::event::{Fact, Push};
use shapewire::cell::Cells;
use shapewire::declare_cells;
use shapewire::operation::{
    DType::{Cursor, Scalar as Sc, Struct, Timestamp as Ts},
    Presence::{Optional, Required},
    Scalar::{Bool, Int, Str},
    TimeEncoding::{Rfc3339},
};
use crate::shape::{
    Handles, Hydrate, HydrateRequest, Hydration, Input, Map, Register, Root, Shape, Stamped,
    Timestamp, Tracking, Tx,
};

/// **THE APPLICATION'S EVENT TYPE.** The one string in this crate that encodes an
/// application's meaning, and it is in an application-specific shape.
pub const SUMMARY_EVENT_TYPE: &str = "summary_updated";

/// **THE HYDRATE BOUND.** See the module doc: 500 = `server::QUERY_MAX_LIMIT`,
/// chosen for the FRAME count of a keyless subscribe and not for the SQL.
pub const MAX_SUMMARY_ROWS: usize = crate::server::QUERY_MAX_LIMIT;

/// **A THREAD'S NAME, as the application means it.**
///
/// `title` is the payload's `summary`; `idx` is the application's own coordinate
/// (`{"idx":24,"summary":"…"}` is the shape of the 366 rows in the production
/// store). `seq` is the ledger position the name came from, which is both the
/// ordering key and the thing a consumer can go read for itself.
///
/// `title` is `Option` and `None` is a real answer: an application that wrote a
/// `summary_updated` with no `summary` field said something, and inventing a
/// title for it would be this shape guessing on the app's behalf.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TitleRow {
    pub title: Option<String>,
    pub idx: Option<i64>,
    pub seq: u64,
    pub at: String,
}

/// **THE ONE BUILDER**, shared by the fold and the hydrate — `docs/shapes.md`
/// §4.3 rule 1. The live path reads the payload off `Push::Event`; the read path
/// reads the same payload out of `events.payload_json`. One function, so the two
/// cannot drift about what a title is.
pub fn title_of(seq: u64, at: &str, data: &Value) -> TitleRow {
    TitleRow {
        title: data
            .get("summary")
            .and_then(Value::as_str)
            .map(str::to_string),
        idx: data.get("idx").and_then(Value::as_i64),
        seq,
        at: at.to_string(),
    }
}

#[derive(Debug, Default)]
pub struct SummaryCells {
    pub title: Register<TitleRow>,
}

impl SummaryCells {
    fn to_json(&self, key: &str) -> Value {
        json!({ "thread_id": key, "title": self.title.now() })
    }
    fn cells_json(&self) -> Value {
        json!({ "title": self.title.state() })
    }
}

pub struct SummaryRoot {
    pub rows: Map<String, SummaryCells>,
    /// Whether the last read hit [`MAX_SUMMARY_ROWS`], and what that means —
    /// said on `shared` because it is a fact about the SHAPE's key set and not
    /// about any one row. The per-cell `complete: false` says the same thing
    /// per key; this says it once, where a consumer deciding whether to fall
    /// back to `threads.list` will look.
    pub truncated: Register<Value>,
    pub shape_version: Register<u32>,
    shared_dirty: bool,
}

impl Default for SummaryRoot {
    fn default() -> Self {
        Self {
            rows: Map::new(),
            truncated: Register::new(),
            shape_version: Register::new(),
            shared_dirty: false,
        }
    }
}

impl Root for SummaryRoot {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }

    fn row(&self, key: &str) -> Option<Value> {
        self.rows.get(&key.to_string()).map(|r| r.to_json(key))
    }

    fn cells(&self, key: &str) -> Option<Value> {
        self.rows.get(&key.to_string()).map(|r| r.cells_json())
    }

    fn shared(&self) -> Value {
        json!({
            "shape_version": self.shape_version.now(),
            "event_type": SUMMARY_EVENT_TYPE,
            "max_rows": MAX_SUMMARY_ROWS,
            "truncated": self.truncated.now(),
        })
    }

    fn shared_cells(&self) -> Value {
        json!({
            "shape_version": self.shape_version.state(),
            "truncated": self.truncated.state(),
        })
    }

    fn drain(&mut self) -> crate::shape::Outbox {
        crate::shape::Outbox {
            rows: self.rows.take_dirty_keys().into_iter().collect(),
            gone: self.rows.take_removed(),
            // A thread named for the first time JOINS this pool, and that is an
            // arrival a membership subscriber is owed.
            added: self.rows.take_added(),
            transitions: Vec::new(),
            shared: std::mem::take(&mut self.shared_dirty),
        }
    }

    fn mark_dead(&mut self, reason: &str) {
        self.shape_version.mark_dead(reason);
        self.truncated.mark_dead(reason);
        self.rows.mark_dead(reason);
        for (_, r) in self.rows.entries_mut() {
            r.title.mark_dead(reason);
        }
    }
}

/// The one request: the most recently named threads, capped.
#[derive(Default)]
pub struct NamedThreads;

/// One named thread, as the read answers it.
#[derive(Debug, Clone)]
pub struct NamedThread {
    pub thread_id: String,
    pub row: TitleRow,
}

impl HydrateRequest for NamedThreads {
    const NAME: &'static str = "named_threads";
    type Response = NamedThread;
}

pub struct Summary;

impl Summary {
    /// See [`crate::shapes::threads`]' module doc for what a declared version is
    /// and is not. This shape's version moves for a different reason than the
    /// parent's: it moves when the APPLICATION's vocabulary changes meaning.
    pub const VERSION: u32 = 1;
}

declare_cells! {
    /// Per-key cells of `threads/summary`: the thread's name, as the newest
    /// `summary_updated` said it. `title: None` inside is a row with no title.
    pub SUMMARY_CELLS = [
        ("title", Struct("TitleRow"), Required, [
            ("title", Sc(Str), Optional),
            ("idx", Sc(Int), Optional),
            ("seq", Cursor("event"), Required),
            ("at", Ts(Rfc3339), Required),
        ]),
    ]
}

declare_cells! {
    /// Shape-level cells of `threads/summary`.
    pub SUMMARY_SHARED = [
        ("shape_version", Sc(Int), Required, []),
        ("truncated", Struct("Truncation"), Required, [
            ("truncated", Sc(Bool), Required),
            ("reason", Sc(Str), Required),
        ]),
    ]
}

impl Shape<Input> for Summary {
    const NAME: &'static str = "threads/summary";
    const CELLS: Cells = SUMMARY_CELLS;
    const SHARED: Cells = SUMMARY_SHARED;
    /// **NO PAGINATION BY DEFAULT, DELIBERATELY — the rule-2.6 lever.** Both
    /// faces subscribe this shape keyless (`seam/held.rs` `standing()`) and
    /// read up to `MAX_SUMMARY_ROWS` names; the chassis default of 100 would
    /// shrink that answer under them, which is a semantic change. The hydrate
    /// is already capped at `MAX_SUMMARY_ROWS` (= the ceiling), so serving the
    /// whole of it costs what it always cost. A consumer that wants a page
    /// asks for one.
    const PAGE: shapewire::wire::Paging = shapewire::wire::Paging::Whole;
    const PARENT: Option<&'static str> = Some("threads");
    const TRACKING: Tracking = Tracking::Lazy;
    /// **`Whole`: the key set is read at init, in ONE statement.** This shape
    /// declared `KeyScope::Active` until 2026-09-28 and it was the only shape in
    /// the repo that did.
    ///
    /// **What `Active` was claiming, and where that claim lives now.** It said
    /// *the key set is the live subset and not the universe* — the keys are the
    /// [`MAX_SUMMARY_ROWS`] most recently named threads plus whatever has been
    /// named since this epoch began. That is a COMPLETENESS claim, and this
    /// shape was already making it three other ways: `hydrates` answers
    /// `Hydration::Partial` when the cap bites, `Map::mark_read(as_of, false)`
    /// puts `complete: false` on the row set's `CellState`, and
    /// `shared.truncated` states it as a shape-level scalar the board reads. A
    /// fourth spelling, as a const, was the only one of the four that could not
    /// change when the world did: it read `active` on an epoch that hit the cap
    /// and `active` on an epoch that read nine rows completely.
    ///
    /// **So nothing is lost by the collapse.** `Active` never routed anywhere —
    /// its own doc said *"today the chassis behaves identically to `Fleet`"* —
    /// so no branch changes; and the claim it carried is still made, per epoch,
    /// where it can be true on one and false on the next. What the axis becomes
    /// is the question it was really answering all along: HOW DOES A KEY ENTER
    /// THIS ROOT. It enters by the init read, which is `Whole`.
    const HYDRATE: Hydrate = Hydrate::Whole;
    /// One `summary_updated` touches exactly one key.
    const BURST: usize = 4;
    /// **HOW MANY APPLICATION EVENTS THIS SHAPE DISCARDS.** The app's vocabulary
    /// is seven types and this shape reads one; the number on `shapes.list` is
    /// what tells an operator that the other six are flowing and unread — which
    /// is the question "should there be another application shape" in its
    /// measurable form.
    const COUNTERS: &'static [&'static str] = &["app_events_ignored"];

    type Producer = crate::runtime::ThreadManager;
    type Root = SummaryRoot;

    fn root(&self) -> SummaryRoot {
        SummaryRoot::default()
    }

    fn counts(&self, input: &Stamped<Input>) -> Option<&'static str> {
        match &input.value {
            Input::Push(Push::Event(se)) => match &se.event {
                crate::event::ThreadEvent::Custom { event_type, .. }
                    if event_type != SUMMARY_EVENT_TYPE =>
                {
                    Some("app_events_ignored")
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn hydrates(
        &mut self,
        p: &crate::runtime::ThreadManager,
        root: &mut SummaryRoot,
        as_of: Timestamp,
    ) {
        if root.shape_version.now() != Some(&Summary::VERSION) {
            root.shape_version.emit(as_of, Summary::VERSION);
            root.shared_dirty = true;
        }
        let named = p.hydrate(NamedThreads);
        let complete = named.is_complete();
        let reason = named.reason();
        for n in named.rows() {
            let cell = &mut root.rows.key(n.thread_id.clone()).title;
            if complete {
                cell.read(as_of, n.row.clone());
            } else {
                // The value is real; the PICTURE behind it is not whole.
                cell.read_partial(as_of, n.row.clone());
            }
        }
        root.rows.mark_read(as_of, complete);
        let truncation = json!({
            "truncated": !complete,
            "reason": reason,
        });
        if root.truncated.now() != Some(&truncation) {
            root.truncated.emit(as_of, truncation);
            root.shared_dirty = true;
        }
        // A hydrate publishes nothing — the export IS the publication.
        let _ = root.drain();
    }

    /// **EXHAUSTIVE, WITH NO `_` ARM.** One arm reads one event type.
    ///
    /// This fold MINTS KEYS, unlike its parent's: a thread being named for the
    /// first time becomes a key. That is what makes the shape useful and it is
    /// what makes `docs/shapes.md` §7.2 reachable here — see the module doc.
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, SummaryRoot>) {
        let at = tx.at();
        match &input.value {
            Input::Push(Push::Event(se)) => {
                let crate::event::ThreadEvent::Custom { event_type, data } = &se.event else {
                    return;
                };
                if event_type != SUMMARY_EVENT_TYPE {
                    return;
                }
                // Ordered on `seq`, for [`crate::shapes::threads::NewestRow`]'s
                // reason: a wall clock can move backwards and a per-thread `seq`
                // cannot. A replayed or out-of-order name is DISCARDED rather
                // than applied.
                //
                // Compared through `get`, never `key`: `Map::key` marks a key
                // dirty on reach, so the same test one line later still emits the
                // `row` frame it exists to suppress.
                if tx
                    .rows
                    .get(&se.thread_id)
                    .is_some_and(|r| r.title.now().is_some_and(|held| se.seq <= held.seq))
                {
                    return;
                }
                tx.rows
                    .key(se.thread_id.clone())
                    .title
                    .emit(at, title_of(se.seq, &se.ts, data));
            }

            // ── everything else, homed ───────────────────────────────────
            Input::Push(Push::Status { .. })
            | Input::Push(Push::Liveness { .. })
            | Input::Push(Push::Progress { .. })
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
            | Input::Fact(Fact::ThreadHeader { .. })
            | Input::Fact(Fact::ThreadSettings { .. })
            | Input::Fact(Fact::ThreadPlacement { .. })
            | Input::Fact(Fact::ThreadSeen { .. })
            | Input::Fact(Fact::ThreadCompaction { .. })
            // A queue row carries no summary. This shape reads one event type
            // and a parked prompt is not it.
            | Input::Fact(Fact::ThreadQueue { .. })
            | Input::Fact(Fact::ThreadChildren { .. })
            | Input::Fact(Fact::ThreadRolled { .. })
            | Input::Fact(Fact::ThreadSubagents { .. })
            | Input::Fact(Fact::ThreadChildActivity { .. })
            // The link beat is `crate::shapes::heartbeat`'s alone — it is about
            // the LINK carrying frames, not about anything this shape holds.
            | Input::Fact(Fact::Beat { .. })
            // The producer's own warning count is `crate::shapes::warnings`'
            // business and nothing else's.
            | Input::Fact(Fact::WarningsSampled { .. }) => {}
        }
    }
}

impl Handles<NamedThreads> for crate::runtime::ThreadManager {
    /// ONE statement, capped at [`MAX_SUMMARY_ROWS`], through the same
    /// [`title_of`] builder the fold uses.
    ///
    /// **`Partial` when the cap bit**, with the count in the reason — a short
    /// read that claimed `Complete` is the one failure types cannot see, and it
    /// is the failure that puts a consumer's "I have every named thread" on a
    /// window.
    ///
    /// `Empty` when the application has named nothing. That is a statement about
    /// the world and renders as empty, not as `?`.
    fn hydrate(&self, _: NamedThreads) -> Hydration<NamedThread> {
        let (rows, total) = match self
            .store
            .newest_app_events(SUMMARY_EVENT_TYPE, MAX_SUMMARY_ROWS)
        {
            Ok(v) => v,
            Err(e) => {
                return Hydration::Partial {
                    got: Vec::new(),
                    reason: format!("{e}"),
                }
            }
        };
        let got: Vec<NamedThread> = rows
            .into_iter()
            .map(|(thread_id, seq, at, data)| NamedThread {
                row: title_of(seq, &at, &data),
                thread_id,
            })
            .collect();
        if total > got.len() {
            let missing = total - got.len();
            return Hydration::Partial {
                reason: format!(
                    "{total} threads have a name; this read carries the {} most recently \
                     named and {missing} are missing",
                    got.len()
                ),
                got,
            };
        }
        if got.is_empty() {
            return Hydration::Empty {
                reason: "the application has not named any thread",
            };
        }
        Hydration::Complete(got)
    }
}
