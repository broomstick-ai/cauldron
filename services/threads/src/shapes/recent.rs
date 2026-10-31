//! # `threads/recent-updates` — the index of recent updates
//!
//! Ruling 3's second thread shape: *"two shapes for threads — the pool, and ONE
//! index of recent updates."* The pool is [`crate::shapes::threads`]; this is
//! the index, and it does exactly one thing the pool cannot.
//!
//! ## It INFORMS; it does not subscribe
//!
//! Operator ruling, and the first constraint on every line below:
//!
//! > *"Those indices can inform the app which from the pool of threads to
//! > subscribe to. We don't need each of the indices to facilitate the actual
//! > subscription process."*
//!
//! So the whole protocol is: subscribe to this shape, read `threads` off the
//! row, pick the ids you want, and subscribe to `threads` with them. Nothing
//! here expands a reference, nothing here opens a subscription on your behalf,
//! and nothing here knows which of the ids you cared about. A prior round built
//! reference cells that expanded at subscribe time, capped the expansion and
//! tracked provenance so a consumer would not have to make that second call; it
//! was deleted in one sentence, because every piece of it was the ENVIRONMENT
//! deciding what the AGENT wanted. The second call is the point.
//!
//! ## It does not re-cache thread content
//!
//! Ids, an order, and a two-integer roll-up. No header, no settings, no title,
//! no status, no message. **Two indices over one pool must cost two orderings
//! and not two copies**, and the arithmetic is the reason: a row on
//! `threads` exports at ~3.7 KB, an entry here at ~120 bytes.
//!
//! ## What it adds beyond membership, stated, because membership already exists
//!
//! `detail: keys` on `threads` plus `Derived::Added` already tell a consumer
//! what exists and announce a brand-new thread (`crate::shapes::threads`'
//! `hydrates_members`). Against that, this shape adds **ordering and a
//! roll-up**, and nothing else:
//!
//! 1. **ORDER BY LAST USER INPUT.** Membership is existence in KEY order —
//!    `SELECT id FROM threads ORDER BY id`, which for opaque ids is arbitrary.
//!    A consumer that wants *the twenty threads worth looking at first* gets
//!    nothing from it. `docs/access-patterns.md` 3.4 is explicit: *"Membership
//!    is EXISTENCE, in key order. It is not RECENCY."* **Which recency is a
//!    RULING and is §"What the order is" below** — it was "activity" until
//!    2026-10-05 and that was a defect, not a wording.
//! 2. **BOUNDED BY RELEVANCE.** A membership roster is the whole fleet, paged
//!    (5,417 ids at measurement, up to `MAX_MEMBER_IDS` 10,000). This is
//!    [`RECENT_CAPACITY`] entries, and which entries is the answer.
//! 3. **THE UNREAD ROLL-UP**, which a membership frame cannot carry at all —
//!    `MembersPage::Known` is a `Vec<String>` and has nowhere to put a number.
//!
//! **It does NOT add existence**, and that is worth saying because it looks like
//! it does. A thread appears here when HE FIRST SPEAKS IN IT — but that is a
//! CONSEQUENCE of the order, not a second membership channel, and it is bounded
//! by the order: a thread he stops speaking in falls out of this window and still
//! exists, and a thread he has NEVER spoken in was never in it. *If you want to
//! know what exists, ask the pool at `detail: keys`. Ask this shape what to look
//! at first.*
//!
//! ## What the order is: LAST USER INPUT, and nothing else
//!
//! **Operator directive, verbatim** — the constraint that decides both halves of
//! this file:
//!
//! > *"the recents need to be ordered on last user input, not on when they were
//! > created or any other events, including renames."*
//!
//! Three exclusions, each of which the previous design failed:
//!
//! * **not on creation.** `upsert_thread` stamps `threads.updated_at`.
//! * **not on any other event.** `bump_last_seq` advances `threads.updated_at` on
//!   every one of 2.46 M ledger appends, and the fold called `Recency::touch`
//!   from a generic `Push::Event` arm — so a `stream_event`, 58% of the ledger,
//!   reordered the band.
//! * **not on renames.** The fold had an explicit `Fact::ThreadHeader` arm that
//!   called `touch`, and a header write is what a rename IS
//!   (`update_thread_settings`).
//!
//! So the order is `user_message` and nothing else, and **it is spelled twice,
//! deliberately, because an index has two halves that can disagree**:
//!
//! | half | where | what moves it |
//! |---|---|---|
//! | the live fold | [`Recent::folds`]' `Push::Event` arm | `Recency::touch`, only when `ThreadEvent::is_user_message` |
//! | the durable order | `threads.last_user_input_at`, via `Store::recent_threads` | `Store::bump_last_seq`, only when the append was a `user_message` |
//!
//! **Fixing one without the other is the trap.** With the fold fixed and the
//! hydrate still on `updated_at`, the live order and the order a RE-INITIALISED
//! subscriber receives disagree — and nothing is visibly wrong until a reconnect,
//! at which point the band silently re-sorts on last-any-event. Both halves ask
//! `ThreadEvent::is_user_message` (the column's writer) or `kind =
//! 'user_message'` (the backfill's filter), which are the same predicate in two
//! languages, and `tests/recent_updates.rs` compares the two orders across a real
//! re-init rather than trusting that.
//!
//! **A thread he has never spoken in holds no place here**, and that is the
//! directive and not an omission: an order on last user input has no position to
//! offer it, the fold cannot mint one (it moves a thread only on a user message),
//! and so the hydrate does not either — `recent_threads` filters `NULL`. Padding
//! the window with never-spoken threads would hand a reconnecting consumer entries
//! the live fold would never have produced.
//!
//! ## `recent-visited` is NOT here, and was dropped rather than deferred
//!
//! A sibling index of the threads a consumer had VISITED was considered and
//! deleted in design. A visit is a CONSUMER-SIDE FACT: the producer has no
//! business knowing it, recording it would make a shape that is by construction
//! shared by many consumers hold per-consumer policy state (one `visited` cell
//! cannot hold one value per viewer), and it is a second source for something
//! the consumer already knows exactly. The test is not *"should the environment
//! decide this"* but *"does the environment even KNOW this."*
//!
//! That ruling has a mechanical consequence inside this file, and it is the one
//! place a reader might expect otherwise: **`Fact::ThreadSeen` updates the
//! roll-up and DOES NOT REORDER.** Being read is something a consumer did, not
//! something the thread did. `Recency::set` exists for exactly that distinction —
//! in place, no move — and `Recency::touch` is for the one act that IS the order.
//!
//! **`set` is now the common case and not the exception.** Every ledger event
//! that is not a `user_message` goes through it too (see §"What the order is"),
//! which means *the roll-up and the order are separately fed from one site*: a
//! tool result on a thread in the window raises its `unread` and leaves it where
//! it sits. A test that only checked that a non-user event does not reorder would
//! pass against a fold that had simply stopped folding, so
//! `tests/recent_updates.rs` asserts the roll-up moved in the same breath.
//!
//! ## Keys: one, and it is `fleet`
//!
//! **A key is a POOL, and there is one pool.** `docs/shape-design-guide.md` step
//! 1: *"A single thing is a pool with one fixed key"* — which is what
//! `narration`'s three channels are, and what this shape's one channel is. It is
//! not keyed by thread id, and that is the design and not an omission: a shape
//! keyed by thread id exports one frame per key IN KEY ORDER, so the ORDER — the
//! only thing this shape carries — would be the one fact it could not express.
//! The order has to live *in* a cell, so it does, and the cell needs a key, so
//! there is one.
//!
//! `Hydrate::Whole` follows: one key, read at init, and there is never a second.
//!
//! ## The cell is a `Recency`, which is a sequence that hydrates
//!
//! `shapewire::fold::Recency` was written for this shape, and the operator
//! described it before it existed: *"a recency sequence across fleet, maybe that
//! aggregated by thread_id so it's just a reorder of recently active threads,
//! maybe a counter of unread on each one across the fleet."* Three things follow
//! from that sentence and each is why this is not a `Sequence`:
//!
//! * **aggregated by thread_id** — a hot thread appended once per event would
//!   fill a 32-item window with three distinct ids and evict every cold thread
//!   the index exists to surface. A touch MOVES.
//! * **a reorder** — so it emits no `transition`. A mirror handed appended items
//!   would accumulate moves and never hold the order.
//! * **it hydrates** — [`Recent::hydrates`] reads the durable order, so a COLD
//!   subscriber receives history instead of only what happens next. That is what
//!   `retain` is a parameter of, and `Sequence` has no `read` at all.
//!
//! ## The hydrate bound: `2 + RECENT_CAPACITY` statements, and why 32
//!
//! **Two shape-level statements plus one per entry**, for 34 at
//! [`RECENT_CAPACITY`] = 32:
//!
//! | statements | read | cost |
//! |---|---|---|
//! | 1 | `store::recent_threads(32)` — the order | one scan + sort of `threads`, two columns; there is no index on `last_user_input_at` |
//! | 1 | `store::list_thread_seen()` — every seen cursor | one scan of a small one-row-per-*interesting*-thread table |
//! | 32 | `store::unread_count(id, seen)` — the roll-up | one index range on `(thread_id, seq)` per entry |
//!
//! **Flat in the fleet size**: the first two are one statement each whether the
//! fleet is 5,417 or 500,000, and the third is `RECENT_CAPACITY` and not a
//! function of anything. `tests/recent_hydrate_cost.rs` pins both axes, in its
//! own binary because `store::db_trips()` is process-global.
//!
//! **Why 32 and not 500.** `RECENT_CAPACITY` is the only term in the bound that
//! grows, so it is the whole of the cost decision. 32 is the window size this
//! daemon already calls a window (`accounts.moves` 32 × 24 h, `threads.status`
//! 32 × 24 h) and it is comfortably above the operator's *"the twenty most
//! recently updated"*. At `MAX_ROWS_PER_SUBSCRIBE`'s 500 the cold subscribe
//! would be 502 statements, and the per-entry `unread_count` is an index range
//! over one thread's events above its seen cursor — 177,995 entries on the
//! largest thread in the production store. A cold hydrate that can walk a
//! hundred thousand index entries five hundred times is the unbounded read this
//! whole repair exists to prevent, and `list_threads(100_000)` is the instance
//! that was already found here.
//!
//! **No time window.** [`Recency`] is bounded by capacity alone, deliberately:
//! *"the most recently updated N"* must have an answer on an idle fleet, and a
//! 24 h window would empty this index after a quiet day — the one state in which
//! a consumer most wants to know where it left off.
//!
//! ## `unread` here and `unread` on the pool — the one rule-7 spend, stated
//!
//! [`crate::shapes::threads`]' `unread` cell carries the same number for a
//! thread a consumer HOLDS. Two shapes carrying one fact is what *"one way to
//! know a thing"* forbids, so the reason it is here anyway is on the record:
//!
//! * The pool's `unread` exists only for keys a consumer has subscribed to. This
//!   one exists for the 32 most recent threads whether or not any of them is
//!   held — which is the whole ask, *"a counter of unread on each one ACROSS THE
//!   FLEET"*, and the pool by construction cannot answer it.
//! * **The two cannot disagree except by window, and the disagreement is
//!   ATTRIBUTABLE.** Both are fed from the same two emit sites
//!   (`Push::Event` and `Fact::ThreadSeen`) with the same arithmetic — an
//!   ADDRESSABLE event above the seen cursor is one unread — and each export
//!   carries its own cell state, so a consumer holding both can say exactly
//!   which fold point each number is from. That is rule 8's stated exception:
//!   two statements of one act that fire from one site and cannot disagree.
//! * **Authority, if a consumer must pick one:** the pool. It hydrates per key
//!   from `thread_states`; this one hydrates in a batch and is a roll-up.
//!
//! `seen_seq` rides beside `unread` on each entry for the same reason
//! `threads` carries both positions: so a consumer can SEE that the count is not
//! a position difference rather than having to trust it.
//!
//! ## Tracking
//!
//! `Lazy`, and for `threads`' reason rather than `accounts`': this shape folds
//! `Push::Event`, which is 2,463,981 rows of ledger at ~50/s peak, and `Lazy` is
//! what makes an unwatched daemon pay none of it. **Unlike `threads` it does NOT
//! skip `BULK_KINDS`**, which is a deliberate divergence — but the REASON changed
//! when the order did. It used to be that a `stream_event` bumps
//! `threads.updated_at` through `bump_last_seq`, so a fold that ignored those
//! three kinds would drift out of agreement with the column its hydrate read;
//! the hydrate no longer reads that column, so that reason is gone. The reason now
//! is the ROLL-UP: an addressable event is one unread whatever its kind, the
//! pool's `unread` cell is fed from this same site with the same arithmetic, and a
//! fold that skipped 58% of the ledger would make the two numbers disagree by
//! construction. Those events reach `Recency::set` and never `touch`, so the cost
//! of folding them is an in-place field write on at most one of 32 entries, behind
//! the 100 ms row debounce — strictly less than the deque move it used to be.
//!
//! ## Version
//!
//! [`Recent::VERSION`] is `1`, published on `shared` as `shape_version`. See
//! [`crate::shapes::threads`]' module doc for what declaring the number does and
//! does not buy — `SERVES` and the link-time refusal are not built.

use serde::Serialize;
use serde_json::{json, Value};

use crate::event::{Fact, Push};
use shapewire::cell::Cells;
use shapewire::declare_cells;
use shapewire::operation::{
    DType::{Cursor, List, Ref, Scalar as Sc, Struct, Timestamp as Ts},
    Presence::{Required},
    Scalar::{Int},
    TimeEncoding::{Rfc3339},
};
use crate::shape::{
    Handles, Hydrate, HydrateRequest, Hydration, Input, Map, Recency, Register, Retain, Root,
    Shape, Stamped, Timestamp, Tracking, Tx,
};

/// **THE ONE KEY.** A pool with one fixed key is what a single thing is; see the
/// module doc for why the order cannot be spelled as a key set.
pub const FLEET: &str = "fleet";

/// **HOW MANY THREADS THE INDEX ORDERS**, and the producer's ceiling for
/// `retain`. The module doc carries the arithmetic: it is the only term in the
/// hydrate bound that grows.
pub const RECENT_CAPACITY: usize = 32;

/// One entry: an id, a position, and a roll-up. **Never a row.**
///
/// `id` and `at` are added by `Recency::to_json_retained` from the cell's own
/// key and stamp rather than being fields here, so the order cannot disagree
/// with the entry that claims it.
///
/// **`at` is the ORDER KEY, and it is WHEN HE LAST SPOKE IN THIS THREAD** — not
/// a re-serving of `threads.header.updated_at`, which moves on every append and
/// on every rename and is what this index wrongly ordered by until 2026-10-05.
/// At hydrate it is the `threads.last_user_input_at` column; afterwards it is the
/// fold's own stamp at the `user_message` that moved the entry. A consumer that
/// wants the authoritative header stamp subscribes to `threads/{id}` and reads it
/// there; this one exists so the order is checkable instead of merely asserted.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RecentEntry {
    /// Addressable events above [`Self::seen_seq`]. See the module doc for why
    /// this is carried at all when the pool has the same number.
    pub unread: u64,
    /// The seen cursor the count is above — carried so the consumer can see the
    /// count is not a position difference, exactly as `threads` carries both.
    pub seen_seq: u64,
}

/// One pool's index.
pub struct FleetRow {
    /// The order. Most recent first.
    pub threads: Recency<RecentEntry>,
}

impl Default for FleetRow {
    fn default() -> Self {
        Self {
            threads: Recency::new(RECENT_CAPACITY),
        }
    }
}

pub struct RecentRoot {
    pub pools: Map<String, FleetRow>,
    pub shape_version: Register<u32>,
    /// **EVERY MANAGED CHILD THIS SHAPE KNOWS OF** (R9.UI1, operator ruling
    /// UI-a: recent threads hide subagents). Seeded at hydrate off the managed
    /// ledgers (`NonIdleLineage`, the same read `threads/non-idle` seeds its
    /// lineage from), grown by `Fact::ThreadChildren`, which a spawn emits at
    /// its commit point and so BEFORE the child's task `user_message` is
    /// appended. A `user_message` on a thread in this set never moves the
    /// order. Lineage is write-once, so a held id is never wrong.
    pub children: std::collections::BTreeSet<String>,
    shared_dirty: bool,
}

impl Default for RecentRoot {
    fn default() -> Self {
        Self {
            pools: Map::new(),
            shape_version: Register::new(),
            children: Default::default(),
            shared_dirty: false,
        }
    }
}

impl Root for RecentRoot {
    fn keys(&self) -> Vec<String> {
        self.pools.keys().cloned().collect()
    }

    fn row(&self, key: &str) -> Option<Value> {
        self.row_at(key, Retain::Max)
    }

    /// **THE `retain` PROJECTION, AND THE REASON THIS SHAPE DECLARES A
    /// CEILING.** `Shape::MAX_RETAINED_ITEMS` without this override would be a
    /// shape that accepts a `retain` ask and then ignores it — the two
    /// declarations move together, and the chassis cannot check the pairing
    /// because `Root` exposes no cell types.
    fn row_at(&self, key: &str, retain: Retain) -> Option<Value> {
        self.pools.get(&key.to_string()).map(|p| {
            json!({
                "pool": key,
                "threads": p.threads.to_json_retained(retain),
            })
        })
    }

    fn cells(&self, key: &str) -> Option<Value> {
        self.pools
            .get(&key.to_string())
            .map(|p| json!({ "threads": p.threads.state() }))
    }

    /// **NO `capacity` HERE.** It rides on `shapes.list` as
    /// `max.retained_items`, which is the chassis's own home for a declared
    /// ceiling, and publishing it twice would be two sources for one number.
    /// What IS here is the thing that has no chassis home: what the order IS.
    fn shared(&self) -> Value {
        json!({
            "shape_version": self.shape_version.now(),
            "pool": FLEET,
            // **WHAT THE ORDER IS, IN THE ONE PLACE A CONSUMER CAN READ IT
            // OFF THE WIRE.** It is the operator's own exclusion list
            // compressed to four words, and it is published rather than
            // documented because "activity" — which this said until the fold
            // was fixed — is exactly the word that hid the defect.
            "order": "last user input, most recent first",
            "orders": "threads",
        })
    }

    fn shared_cells(&self) -> Value {
        json!({ "shape_version": self.shape_version.state() })
    }

    fn drain(&mut self) -> crate::shape::Outbox {
        let rows: Vec<String> = self.pools.take_dirty_keys().into_iter().collect();
        crate::shape::Outbox {
            rows,
            gone: self.pools.take_removed(),
            // The one pool is minted at init and never again. Carried anyway,
            // because "never" is a property of this fold and not of the
            // mechanism.
            added: self.pools.take_added(),
            // **EMPTY, ALWAYS, AND THAT IS THE POINT.** A reorder is not an
            // appended item; see `Recency`.
            transitions: Vec::new(),
            shared: std::mem::take(&mut self.shared_dirty),
        }
    }

    fn mark_dead(&mut self, reason: &str) {
        self.shape_version.mark_dead(reason);
        self.pools.mark_dead(reason);
        for (_, p) in self.pools.entries_mut() {
            p.threads.mark_dead(reason);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The hydrate request — ONE, shape-level, never per key
// ─────────────────────────────────────────────────────────────────────────────

/// What one entry's durable state is, before the cell holds it.
#[derive(Debug, Clone)]
pub struct RecentRow {
    pub thread_id: String,
    /// The `threads.last_user_input_at` column, PARSED — when he last spoke in
    /// this thread. A row whose stamp will not parse is dropped by the handler and
    /// makes the read `Partial` with the reason — never given an invented stamp,
    /// which would put it in an order it did not earn. A row whose column is NULL
    /// never reaches here at all: `Store::recent_threads` filters it, because he
    /// has never spoken there and the order has no position to offer it.
    pub at: Timestamp,
    pub unread: u64,
    pub seen_seq: u64,
}

/// **THE DURABLE ORDER, READ ONCE PER EPOCH.** `2 + RECENT_CAPACITY`
/// statements; the module doc carries the table and the argument for the number.
#[derive(Default)]
pub struct RecentOrder;
impl HydrateRequest for RecentOrder {
    const NAME: &'static str = "thread_recent_order";
    type Response = RecentRow;
}

// ─────────────────────────────────────────────────────────────────────────────
// The shape
// ─────────────────────────────────────────────────────────────────────────────

pub struct Recent;

impl Recent {
    /// See [`crate::shapes::threads`]' module doc for what a declared version is
    /// and is not.
    pub const VERSION: u32 = 1;
}

declare_cells! {
    /// Per-key cells of `threads/recent-updates` — one pool, one cell: the order.
    /// `id` and `at` ride each entry from the chassis's `Recency` cell.
    pub RECENT_CELLS = [
        ("threads", List(&Struct("RecentEntry")), Required, [
            ("id", Ref("thread"), Required),
            ("at", Ts(Rfc3339), Required),
            ("unread", Sc(Int), Required),
            ("seen_seq", Cursor("event"), Required),
        ]),
    ]
}

declare_cells! {
    /// Shape-level cells of `threads/recent-updates`. `pool`, `order` and
    /// `orders` ride `shared` as constants WITHOUT a cell state and are therefore
    /// not here: a value with no state is a value whose unknown-ness cannot be
    /// expressed, and the manifest declares cells, not constants.
    pub RECENT_SHARED = [
        ("shape_version", Sc(Int), Required, []),
    ]
}

impl Shape<Input> for Recent {
    /// **THE LEAF IS THE OPERATOR'S OWN WORD.** Nested under `threads` because
    /// it is an index OVER that pool and nothing else — so a consumer reaches it
    /// from the pool's name, and `Epoch::parent_epoch` says whether the index and
    /// the pool it hands you keys for are from the same era.
    const NAME: &'static str = "threads/recent-updates";
    const CELLS: Cells = RECENT_CELLS;
    const SHARED: Cells = RECENT_SHARED;
    /// The index's whole protocol, declared: an entry's `id` is a key of
    /// `threads`, and the second call is the consumer's.
    const REFERS: &'static [(&'static str, &'static str)] = &[("thread", "threads")];
    const PARENT: Option<&'static str> = Some("threads");
    /// See the module doc: this shape folds `Push::Event`, and `Lazy` is what
    /// makes an unwatched daemon pay nothing for 2.46 M ledger rows.
    const TRACKING: Tracking = Tracking::Lazy;
    /// **`Whole`, and it is not a default.** One key, read at init, and there is
    /// never a second — so a keyless subscribe is a real inventory of exactly one
    /// pool. Declared even though it is the default, because *"a const whose only
    /// instance is an omission is a const nobody can see"*.
    const HYDRATE: Hydrate = Hydrate::Whole;
    /// **THE `retain` CEILING** — [`RECENT_CAPACITY`], because that is what the
    /// cell holds. A consumer reads it off `shapes.list` as
    /// `max.retained_items`, asks for that or less, and is refused naming it
    /// above. `Root::row_at` above is the other half of this declaration.
    const MAX_RETAINED_ITEMS: usize = RECENT_CAPACITY;
    /// One input touches the one key: one `row`, and nothing else. Anything above
    /// two means the fold started reaching for something it does not write.
    const BURST: usize = 2;

    type Producer = crate::runtime::ThreadManager;
    type Root = RecentRoot;

    fn root(&self) -> RecentRoot {
        RecentRoot::default()
    }

    /// **THE COLD SUBSCRIBER'S HISTORY.** One read, through the same entry type
    /// the live fold writes — one type, two paths, one cell.
    fn hydrates(
        &mut self,
        p: &crate::runtime::ThreadManager,
        root: &mut RecentRoot,
        as_of: Timestamp,
    ) {
        if root.shape_version.now() != Some(&Recent::VERSION) {
            root.shape_version.emit(as_of, Recent::VERSION);
            root.shared_dirty = true;
        }
        // UI-a: the children first, so the live fold that follows this hydrate
        // already knows which `user_message`s are not his.
        for (child, _parent) in p.hydrate(crate::shapes::nonidle::NonIdleLineage).into_rows() {
            root.children.insert(child);
        }
        let got = p.hydrate(RecentOrder);
        let complete = got.is_complete();
        let reason = got.reason();
        let rows = got.into_rows();
        // **A FAILED READ IS NOT AN EMPTY POOL.** `Partial{got: []}` is the
        // handler's answer to a store error; an empty fleet comes back
        // `Complete(vec![])`, which is a complete answer of zero.
        if rows.is_empty() && !complete {
            let why = reason.unwrap_or_else(|| "the recent order could not be read".into());
            root.pools.key(FLEET.to_string()).threads.mark_not_hydrated(why.clone());
            root.pools.mark_not_hydrated(why);
            root.pools.mark_read(as_of, false);
            let _ = root.drain();
            return;
        }
        let entries: Vec<(String, Timestamp, RecentEntry)> = rows
            .into_iter()
            .map(|r| {
                (
                    r.thread_id,
                    r.at,
                    RecentEntry {
                        unread: r.unread,
                        seen_seq: r.seen_seq,
                    },
                )
            })
            .collect();
        root.pools
            .key(FLEET.to_string())
            .threads
            .read(as_of, entries, complete);
        root.pools.mark_read(as_of, true);
        // A hydrate publishes nothing — the export IS the publication.
        let _ = root.drain();
    }

    /// **EXHAUSTIVE, WITH NO `_` ARM.** ONE arm moves the order — a
    /// `user_message` and nothing else — one arm changes a roll-up WITHOUT moving
    /// it, and everything else says where it lives.
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, RecentRoot>) {
        let at = tx.at();
        match &input.value {
            // ── A LEDGER EVENT: THE ROLL-UP ALWAYS, THE ORDER ONLY IF HE SPOKE ──
            //
            // **Operator directive, verbatim:** *"the recents need to be ordered
            // on last user input, not on when they were created or any other
            // events, including renames."*
            //
            // This arm called `touch` for EVERY event until this change, which is
            // the defect: a `stream_event` (58% of the ledger), a tool result, a
            // turn ending — any of them reordered the index. So the split here is
            // between the two things an event can do, and they are NOT the same
            // thing:
            //
            //   * **it reorders** — only `user_message`, through `touch`.
            //   * **it updates the roll-up** — every event, through `set`.
            //
            // `set` is the right half of that pair for exactly the reason the
            // `ThreadSeen` arm below uses it: in place, no move. An unread count
            // climbing is not the thread moving in a last-user-input order, and a
            // consumer watching a busy thread it has not read still gets the
            // number it came for.
            //
            // **`BULK_KINDS` are still folded**, unlike on `crate::shapes::threads`
            // — the old reason (keeping the live order in step with
            // `threads.updated_at`, which `bump_last_seq` advances on every
            // append) is gone with the column, but the new reason is the roll-up:
            // an addressable event is an unread whatever its kind, and skipping
            // 58% of the ledger would make this shape's `unread` disagree with the
            // pool's, which is fed from this same site.
            Input::Push(Push::Event(se)) => {
                // **READ BEFORE REACHING.** `Map::key` dirties on reach — a
                // caller that reaches for a key is about to change it — so a
                // guard placed after `key(..)` has already cost the `row` frame
                // it exists to avoid. Three decisions this arm makes are taken
                // through `get`:
                //
                //   1. WOULD THE STAMP RULE ACCEPT THIS AT ALL? An input at or
                //      before the cell's `folded_at` moves nothing, and that is
                //      reachable here rather than theoretical: an event already
                //      in flight when the hydrate read ran arrives stamped
                //      before it. `Recency::touch` refuses it, so without this
                //      check the refusal would still cost a frame carrying the
                //      order unchanged.
                //   2. WHAT WAS HELD? The previous entry is carried forward;
                //      inventing a fresh one would reset `unread` on every
                //      event.
                //   3. WOULD A NON-USER EVENT CHANGE ANYTHING? For a
                //      `user_message` the answer is always yes — it moves the
                //      order. For everything else the only thing that can move is
                //      the roll-up, and TWO WAYS IT CANNOT must both be caught
                //      here rather than inside the cell:
                //
                //        * the window does not hold the thread. `Recency::set`
                //          answers `false` and changes nothing; a thread he has
                //          never spoken in is NOT in this window, by construction.
                //        * the event is not ADDRESSABLE, or is at or below the
                //          seen cursor, so the `unread` arithmetic below does not
                //          move. **This is the hot one.** `stream_event` and its
                //          two `BULK_KINDS` siblings are 58% of a 2.46 M-row
                //          ledger at ~50/s peak and carry no `message_id`, so
                //          without this guard every one of them would reach
                //          `Map::key`, dirty the row, and send a consumer the
                //          whole 32-entry order with not one field different from
                //          the copy it already holds.
                //
                //      Both are `return`s and not a cell call, because `Map::key`
                //      DIRTIES ON REACH: by the time a cell could answer "nothing
                //      changed" the frame has already been bought.
                //      `an_event_the_stamp_rule_refuses_costs_no_frame` is the same
                //      rule for the stamp guard above, and this shape's module doc
                //      is why it matters here more than anywhere: a frame that says
                //      nothing is the whole order re-sent.
                // **UI-a: A SUBAGENT IS NEVER IN THE RECENTS.** A managed
                // child's `user_message`s are its task, its parent's mail and
                // its wakes — none of them moves this order, and the child
                // never enters it. Its unread roll-up has no entry to land on.
                if tx.children.contains(&se.thread_id) {
                    return;
                }
                let spoke = se.event.is_user_message();
                let held = tx.pools.get(&FLEET.to_string());
                if held
                    .map(|p| p.threads.state())
                    .and_then(|st| st.folded_at())
                    .is_some_and(|f| at <= f)
                {
                    return;
                }
                let known = held.and_then(|p| p.threads.get(&se.thread_id));
                let mut entry = known.cloned().unwrap_or(RecentEntry {
                    unread: 0,
                    seen_seq: 0,
                });
                // THE SAME ARITHMETIC THE POOL USES: addressable, and above the
                // seen position. `crate::shapes::threads`' `unread` cell is the
                // other writer of this number and it is fed from this same site.
                let unread_moved = se.message_id.is_some() && se.seq > entry.seen_seq;
                if unread_moved {
                    entry.unread += 1;
                }
                // **REACH FOR THE KEY ONLY IF SOMETHING WILL CHANGE.** Stated
                // positively because that is the rule: `Map::key` dirties on
                // reach, so this is the last point at which a frame can still be
                // declined.
                let will_change = spoke || (unread_moved && known.is_some());
                if !will_change {
                    return;
                }
                let cell = &mut tx.pools.key(FLEET.to_string()).threads;
                if spoke {
                    cell.touch(at, se.thread_id.clone(), entry);
                } else {
                    cell.set(at, &se.thread_id, entry);
                }
            }
            // ── THE ROLL-UP MOVED, AND THE ORDER DID NOT ─────────────────
            //
            // **THE ONE PLACE THE `recent-visited` RULING IS MECHANICAL.** Being
            // marked seen is something a CONSUMER did; a producer that let it
            // reorder would be keeping a visit index, which was dropped in design
            // precisely because a visit is not the producer's fact. So this is
            // `set` — in place, no move — and if the thread is not in the
            // window `set` answers `false` and changes nothing. It DOES advance
            // the cell's `folded_at`, because the cell folded an input; what it
            // must not advance is the MEMBER's own `at`, which is its position
            // in the order and is not a thing a consumer's act may move.
            Input::Fact(Fact::ThreadSeen {
                thread_id,
                seen_seq,
                unread,
            }) => {
                let known = tx
                    .pools
                    .get(&FLEET.to_string())
                    .and_then(|p| p.threads.get(thread_id))
                    .is_some();
                if !known {
                    return;
                }
                tx.pools.key(FLEET.to_string()).threads.set(
                    at,
                    thread_id,
                    RecentEntry {
                        unread: *unread,
                        seen_seq: *seen_seq,
                    },
                );
            }

            // ── UI-a: lineage, learned from a parent's children level ────
            //
            // Not a move in the order: it only teaches the fold which ids are
            // subagents. The spawn emits it at its commit point, before the
            // child's task is appended, so the child's first `user_message`
            // already meets the guard above.
            Input::Fact(Fact::ThreadChildren { row, .. }) => {
                for r in &row.runs {
                    tx.children.insert(r.id.clone());
                }
            }

            // ── homed elsewhere, every one of them ───────────────────────
            //
            // **`Fact::ThreadHeader` IS A RENAME, AND A RENAME MUST NOT
            // REORDER.** It is the one entry in this list that was once an
            // ordering arm, and it is named first because the operator named it:
            // *"ordered on last user input... not any other events, INCLUDING
            // RENAMES."* The arm that used to be here called `touch` on every
            // header write — `upsert_thread` or `update_thread_settings`, which is
            // the write a rename IS — on the reasoning that a header write
            // advances `threads.updated_at` and so was "activity". That reasoning
            // was correct about the column and wrong about the question, and both
            // halves of it are now gone: the order no longer reads `updated_at`
            // at all.
            //
            // It is a no-op and NOT a `set` either. A header write changes no
            // number this shape holds — the entry is an `unread` and a `seen_seq`
            // — so there is nothing in place to update, and the header's CONTENT
            // belongs to `crate::shapes::threads`, which folds this same fact.
            //
            // `Status` and `Liveness` are LEVELS on a thread and belong to
            // `crate::shapes::threads`; a level moving is not the thread being
            // updated, and folding one here would put a 1 Hz beat at the front of
            // a recency order. `Progress`/`Lifecycle`/`Info` belong to no thread
            // at all and are `crate::shapes::narration`'s. The settings blob, the
            // placement column and the compaction marker are the pool's content:
            // this shape carries no content, so it carries no reason to fold
            // them. Every remaining `Fact` is an account fact and is
            // `crate::shapes::accounts`'.
            Input::Fact(Fact::ThreadHeader { .. })
            | Input::Push(Push::Status { .. })
            | Input::Push(Push::Liveness { .. })
            | Input::Push(Push::Progress { .. })
            | Input::Push(Push::Lifecycle(_))
            | Input::Push(Push::Info { .. })
            | Input::Fact(Fact::ThreadSettings { .. })
            | Input::Fact(Fact::ThreadPlacement { .. })
            | Input::Fact(Fact::ThreadCompaction { .. })
            // A queue row is not a move in the pool's ORDER. `at` on this index
            // is when he last SPOKE in a thread, and enqueuing is the daemon
            // holding something he already said — not a new utterance.
            | Input::Fact(Fact::ThreadQueue { .. })
            // The link beat is `crate::shapes::heartbeat`'s alone — it is about
            // the LINK carrying frames, not about anything this shape holds.
            // R9.TS1: the subtree's rolled status is the `threads` shape's.
            | Input::Fact(Fact::ThreadRolled { .. })
            | Input::Fact(Fact::ThreadSubagents { .. })
            | Input::Fact(Fact::ThreadChildActivity { .. })
            | Input::Fact(Fact::Beat { .. })
            // The producer's own warning count is `crate::shapes::warnings`'
            // business and nothing else's.
            | Input::Fact(Fact::WarningsSampled { .. })
            | Input::Fact(Fact::GaugeObserved { .. })
            | Input::Fact(Fact::LimitRecorded { .. })
            | Input::Fact(Fact::LimitCleared { .. })
            | Input::Fact(Fact::PoolShare { .. })
            | Input::Fact(Fact::AccountDisabled { .. })
            | Input::Fact(Fact::LoginHealth { .. })
            | Input::Fact(Fact::CredentialChanged { .. })
            | Input::Fact(Fact::AccountRegistered { .. })
            | Input::Fact(Fact::AccountRemoved { .. })
            | Input::Fact(Fact::Pressure { .. }) => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The producer's answer
// ─────────────────────────────────────────────────────────────────────────────

impl Handles<RecentOrder> for crate::runtime::ThreadManager {
    /// `2 + RECENT_CAPACITY` statements — see the module doc's table.
    ///
    /// **`Complete(vec![])` for a fleet nobody has spoken in**, which is a
    /// complete answer of zero and not a failure; `Partial{got: []}` for a store
    /// error, which is the read failing and says so. A row whose
    /// `last_user_input_at` will not parse is DROPPED and makes the answer
    /// `Partial` with the reason, because an entry with an invented stamp would
    /// sit in an order it did not earn. A row whose column is NULL is not seen
    /// here at all — `recent_threads` excludes it, because he has never spoken
    /// there; see that function for why exclusion and not a bottom slot.
    fn hydrate(&self, _r: RecentOrder) -> Hydration<RecentRow> {
        // **UI-a: SUBAGENTS ARE LEFT OUT, AND THE WINDOW STILL FILLS.** A thread
        // with a `parent_id` (`ThreadMeta.parent_id`, the one owner of lineage)
        // holds no place here. The read starts at exactly `RECENT_CAPACITY` —
        // so a fleet with no subagents among its newest threads costs the same
        // ONE statement it always did — and widens only when filtering left the
        // window short and the column has more rows to give.
        let mg = self.managed();
        let mut limit = RECENT_CAPACITY;
        let order: Vec<(String, String)> = loop {
            let got = match self.store.recent_threads(limit) {
                Ok(v) => v,
                Err(e) => {
                    return Hydration::Partial {
                        got: Vec::new(),
                        reason: format!("{e}"),
                    }
                }
            };
            let exhausted = got.len() < limit;
            let kept: Vec<(String, String)> = got
                .into_iter()
                .filter(|(id, _)| !shapewire::subtree::hidden_from_recents(mg.parent_of(id).as_deref()))
                .take(RECENT_CAPACITY)
                .collect();
            if kept.len() >= RECENT_CAPACITY || exhausted {
                break kept;
            }
            limit = limit.saturating_mul(4);
        };
        // ONE statement for every seen cursor, rather than one per entry: the
        // same trade `thread_states` already makes, and the reason the bound is
        // `2 + N` and not `1 + 2N`.
        let seen = match self.store.list_thread_seen() {
            Ok(m) => m,
            Err(e) => {
                return Hydration::Partial {
                    got: Vec::new(),
                    reason: format!("{e}"),
                }
            }
        };
        let mut rows = Vec::with_capacity(order.len());
        let mut unparsed = 0usize;
        for (thread_id, last_user_input_at) in order {
            let Ok(at) = chrono::DateTime::parse_from_rfc3339(&last_user_input_at) else {
                unparsed += 1;
                continue;
            };
            let seen_seq = seen.get(&thread_id).copied().unwrap_or(0);
            let unread = match self.store.unread_count(&thread_id, seen_seq) {
                Ok(n) => n,
                Err(e) => {
                    return Hydration::Partial {
                        got: rows,
                        reason: format!("{e}"),
                    }
                }
            };
            rows.push(RecentRow {
                thread_id,
                at: at.with_timezone(&chrono::Utc),
                unread,
                seen_seq,
            });
        }
        if unparsed > 0 {
            return Hydration::Partial {
                got: rows,
                reason: format!(
                    "{unparsed} thread row(s) carry a `last_user_input_at` this build \
                     cannot parse as RFC3339, so they hold no place in the order"
                ),
            };
        }
        Hydration::Complete(rows)
    }
}
