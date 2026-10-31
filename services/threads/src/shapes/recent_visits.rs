//! # `threads/recent-visits` — the index of recent visits
//!
//! The third thread shape the operator asked for, verbatim: *"we should just
//! have 3 shapes: the pool of threads, an index of recent updates, an index of
//! recent visits"* (Q-9 item 8, #H78). The pool is [`crate::shapes::threads`];
//! [`crate::shapes::recent`] is the index of recent updates; this is the third.
//! It is a DIRECT SIBLING of `recent-updates` and copies its whole contract —
//! read that module doc first; only what differs is restated here.
//!
//! ## What the order is: VISIT TIME, and nothing else
//!
//! A visit is `ThreadManager::mark_seen` — a consumer being shown a thread —
//! and that one call is the only thing that moves this order. It does two
//! things in one act, and they are the two halves of this index:
//!
//! | half | where | what moves it |
//! |---|---|---|
//! | the live fold | [`RecentVisits::folds`]' `Fact::ThreadSeen` arm | `Recency::touch` |
//! | the durable order | `thread_seen.updated_at`, via `Store::recent_visits` | `Store::mark_seen`, every call |
//!
//! **The rule is spelled ONCE, at the one site that feeds both halves**:
//! `ThreadManager::mark_seen` writes the column and emits the fact, and nothing
//! else writes either. So "what is a visit" has one answer, not two that must be
//! kept in step — the lesson `recent-updates` paid for (its fold and its hydrate
//! once read two different columns). `tests/recent_visits.rs` still compares the
//! two orders across a REAL re-init rather than trusting that.
//!
//! **Not on updates.** A ledger event — a `user_message` included — moves
//! `recent-updates` and NOT this order. Being written to is something the
//! thread did; being visited is something a consumer did. The two indices are
//! the two answers, and each refuses the other's input.
//!
//! **And the converse, which is the operator/curator flag the spec asked for:
//! a visit does NOT touch `recent-updates`' order.** That shape folds the same
//! `Fact::ThreadSeen` through `Recency::set` (in place, no move) to keep its
//! roll-up current, and that is unchanged here (`set` vs `touch` is proven in
//! `tests/recent_updates.rs` and is not re-litigated). `tests/recent_visits.rs`
//! asserts it again from THIS side, holding both shapes on one connection.
//!
//! ## Why a producer may hold this at all (the §3.6 objection)
//!
//! `recent-updates`' module doc records the objection that dropped a visits
//! index in design: *a visit is a consumer-side fact, and one cell cannot hold
//! one value per viewer.* It does not apply to THIS design, because there is no
//! per-viewer state anywhere in it. `thread_seen` is already ONE ROW PER THREAD,
//! fleet-wide, with no viewer column — the daemon already holds exactly one
//! visit cursor per thread and already serves it on the pool's `seen_seq` cell.
//! This shape only exposes that cursor's RECENCY as a second index, parallel to
//! `recent-updates`. It adds no state the producer did not already keep.
//!
//! ## The entry: an id, a visit stamp, and the cursor — no roll-up
//!
//! `id` and `at` ride from the `Recency` cell (`at` is WHEN IT WAS LAST
//! VISITED). `seen_seq` is the cursor the visit left, and it is carried because
//! it arrives on the same fact and costs nothing. **There is no `unread` here,
//! deliberately**: it would make this fold `Push::Event` (2.46 M rows) to keep a
//! number current that `recent-updates` and the pool already carry — a third
//! copy of one fact, which rule 7 forbids without a reason, and there is none.
//! Ask the pool for content, ask `recent-updates` for the roll-up, ask this
//! shape what he looked at last.
//!
//! ## Bounds: capacity only, no time window, hydrate in ONE statement
//!
//! [`VISITS_CAPACITY`] is [`crate::shapes::recent::RECENT_CAPACITY`] — a
//! sibling index is the same window — and the reasons there carry over
//! unchanged, including why there is no time window. The hydrate is ONE
//! statement (`Store::recent_visits`), flat in the fleet size: there is no
//! per-entry roll-up to compute.
//!
//! ## Tracking
//!
//! `Lazy`, like its sibling. The fold is one arm on a rare fact (a visit is a
//! human act), so the cost of folding is negligible; `Lazy` is for consistency
//! and so an unwatched daemon builds no cell at all.

use serde::Serialize;
use serde_json::{json, Value};

use crate::event::{Fact, Push};
use shapewire::cell::Cells;
use shapewire::declare_cells;
use shapewire::operation::{
    DType::{Cursor, List, Ref, Scalar as Sc, Struct, Timestamp as Ts},
    Presence::Required,
    Scalar::Int,
    TimeEncoding::Rfc3339,
};
use crate::shape::{
    Handles, Hydrate, HydrateRequest, Hydration, Input, Map, Recency, Register, Retain, Root,
    Shape, Stamped, Timestamp, Tracking, Tx,
};

/// **THE ONE KEY** — [`crate::shapes::recent::FLEET`]'s reason.
pub const FLEET: &str = "fleet";

/// How many threads the index orders — the sibling's window, see the module doc.
pub const VISITS_CAPACITY: usize = crate::shapes::recent::RECENT_CAPACITY;

/// One entry: an id, a visit stamp (both from the cell), and the cursor the
/// visit left. **Never a row, and no roll-up** — see the module doc.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VisitEntry {
    /// `thread_seen.last_seen_seq` after the visit's monotonic merge.
    pub seen_seq: u64,
}

pub struct FleetRow {
    /// The order. Most recently visited first.
    pub threads: Recency<VisitEntry>,
}

impl Default for FleetRow {
    fn default() -> Self {
        Self {
            threads: Recency::new(VISITS_CAPACITY),
        }
    }
}

pub struct VisitsRoot {
    pub pools: Map<String, FleetRow>,
    pub shape_version: Register<u32>,
    shared_dirty: bool,
}

impl Default for VisitsRoot {
    fn default() -> Self {
        Self {
            pools: Map::new(),
            shape_version: Register::new(),
            shared_dirty: false,
        }
    }
}

impl Root for VisitsRoot {
    fn keys(&self) -> Vec<String> {
        self.pools.keys().cloned().collect()
    }

    fn row(&self, key: &str) -> Option<Value> {
        self.row_at(key, Retain::Max)
    }

    /// The `retain` projection — the other half of `MAX_RETAINED_ITEMS`, as on
    /// the sibling.
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

    fn shared(&self) -> Value {
        json!({
            "shape_version": self.shape_version.now(),
            "pool": FLEET,
            // What the order IS, on the wire, as the sibling publishes its own.
            "order": "last visit, most recent first",
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
            added: self.pools.take_added(),
            // A reorder is not an appended item; see `Recency`.
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

/// One entry's durable state, before the cell holds it.
#[derive(Debug, Clone)]
pub struct VisitRow {
    pub thread_id: String,
    /// `thread_seen.updated_at`, PARSED. A row whose stamp will not parse is
    /// dropped and makes the read `Partial`, never given an invented stamp.
    pub at: Timestamp,
    pub seen_seq: u64,
}

/// **THE DURABLE VISIT ORDER, READ ONCE PER EPOCH** — one statement.
#[derive(Default)]
pub struct VisitOrder;
impl HydrateRequest for VisitOrder {
    const NAME: &'static str = "thread_visit_order";
    type Response = VisitRow;
}

pub struct RecentVisits;

impl RecentVisits {
    pub const VERSION: u32 = 1;
}

declare_cells! {
    /// Per-key cells of `threads/recent-visits` — one pool, one cell: the order.
    pub VISITS_CELLS = [
        ("threads", List(&Struct("VisitEntry")), Required, [
            ("id", Ref("thread"), Required),
            ("at", Ts(Rfc3339), Required),
            ("seen_seq", Cursor("event"), Required),
        ]),
    ]
}

declare_cells! {
    /// Shape-level cells of `threads/recent-visits`.
    pub VISITS_SHARED = [
        ("shape_version", Sc(Int), Required, []),
    ]
}

impl Shape<Input> for RecentVisits {
    /// The operator's own word, nested under the pool it indexes — exactly as
    /// `threads/recent-updates`.
    const NAME: &'static str = "threads/recent-visits";
    const CELLS: Cells = VISITS_CELLS;
    const SHARED: Cells = VISITS_SHARED;
    const REFERS: &'static [(&'static str, &'static str)] = &[("thread", "threads")];
    const PARENT: Option<&'static str> = Some("threads");
    const TRACKING: Tracking = Tracking::Lazy;
    const HYDRATE: Hydrate = Hydrate::Whole;
    const MAX_RETAINED_ITEMS: usize = VISITS_CAPACITY;
    /// One input touches the one key.
    const BURST: usize = 2;

    type Producer = crate::runtime::ThreadManager;
    type Root = VisitsRoot;

    fn root(&self) -> VisitsRoot {
        VisitsRoot::default()
    }

    fn hydrates(
        &mut self,
        p: &crate::runtime::ThreadManager,
        root: &mut VisitsRoot,
        as_of: Timestamp,
    ) {
        if root.shape_version.now() != Some(&RecentVisits::VERSION) {
            root.shape_version.emit(as_of, RecentVisits::VERSION);
            root.shared_dirty = true;
        }
        let got = p.hydrate(VisitOrder);
        let complete = got.is_complete();
        let reason = got.reason();
        let rows = got.into_rows();
        // A FAILED READ IS NOT AN EMPTY POOL — the sibling's rule.
        if rows.is_empty() && !complete {
            let why = reason.unwrap_or_else(|| "the visit order could not be read".into());
            root.pools.key(FLEET.to_string()).threads.mark_not_hydrated(why.clone());
            root.pools.mark_not_hydrated(why);
            root.pools.mark_read(as_of, false);
            let _ = root.drain();
            return;
        }
        let entries: Vec<(String, Timestamp, VisitEntry)> = rows
            .into_iter()
            .map(|r| (r.thread_id, r.at, VisitEntry { seen_seq: r.seen_seq }))
            .collect();
        root.pools
            .key(FLEET.to_string())
            .threads
            .read(as_of, entries, complete);
        root.pools.mark_read(as_of, true);
        let _ = root.drain();
    }

    /// **EXHAUSTIVE, WITH NO `_` ARM.** ONE arm moves the order — a visit — and
    /// everything else says where it lives.
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, VisitsRoot>) {
        let at = tx.at();
        match &input.value {
            // ── A VISIT: THE ONE ACT THIS ORDER IS MADE OF ────────────────
            //
            // `touch`, not `set`: here the visit IS the order. Read before
            // reaching (`Map::key` dirties on reach), so an input the stamp
            // rule would refuse costs no frame — the sibling's rule.
            Input::Fact(Fact::ThreadSeen {
                thread_id,
                seen_seq,
                ..
            }) => {
                if tx
                    .pools
                    .get(&FLEET.to_string())
                    .and_then(|p| p.threads.state().folded_at())
                    .is_some_and(|f| at <= f)
                {
                    return;
                }
                tx.pools.key(FLEET.to_string()).threads.touch(
                    at,
                    thread_id.clone(),
                    VisitEntry {
                        seen_seq: *seen_seq,
                    },
                );
            }

            // ── homed elsewhere, every one of them ───────────────────────
            //
            // **`Push::Event` — a `user_message` INCLUDED — is an UPDATE, not a
            // visit**, and is `crate::shapes::recent`'s order. Folding it here
            // would make this index a second copy of that one. Everything else
            // is listed on the sibling with its home.
            Input::Push(Push::Event(_))
            | Input::Fact(Fact::ThreadHeader { .. })
            | Input::Push(Push::Status { .. })
            | Input::Push(Push::Liveness { .. })
            | Input::Push(Push::Progress { .. })
            | Input::Push(Push::Lifecycle(_))
            | Input::Push(Push::Info { .. })
            | Input::Fact(Fact::ThreadSettings { .. })
            | Input::Fact(Fact::ThreadPlacement { .. })
            | Input::Fact(Fact::ThreadCompaction { .. })
            | Input::Fact(Fact::ThreadQueue { .. })
            | Input::Fact(Fact::ThreadChildren { .. })
            | Input::Fact(Fact::ThreadRolled { .. })
            | Input::Fact(Fact::ThreadSubagents { .. })
            | Input::Fact(Fact::ThreadChildActivity { .. })
            | Input::Fact(Fact::Beat { .. })
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

impl Handles<VisitOrder> for crate::runtime::ThreadManager {
    /// ONE statement. `Complete(vec![])` for a fleet nobody has visited;
    /// `Partial{got: []}` for a store error; an unparsable stamp is dropped and
    /// makes the answer `Partial` with the reason.
    fn hydrate(&self, _r: VisitOrder) -> Hydration<VisitRow> {
        let order = match self.store.recent_visits(VISITS_CAPACITY) {
            Ok(v) => v,
            Err(e) => {
                return Hydration::Partial {
                    got: Vec::new(),
                    reason: format!("{e}"),
                }
            }
        };
        let mut rows = Vec::with_capacity(order.len());
        let mut unparsed = 0usize;
        for (thread_id, visited_at, seen_seq) in order {
            let Ok(at) = chrono::DateTime::parse_from_rfc3339(&visited_at) else {
                unparsed += 1;
                continue;
            };
            rows.push(VisitRow {
                thread_id,
                at: at.with_timezone(&chrono::Utc),
                seen_seq,
            });
        }
        if unparsed > 0 {
            return Hydration::Partial {
                got: rows,
                reason: format!(
                    "{unparsed} `thread_seen` row(s) carry an `updated_at` this build \
                     cannot parse as RFC3339, so they hold no place in the order"
                ),
            };
        }
        Hydration::Complete(rows)
    }
}
