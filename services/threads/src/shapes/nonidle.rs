//! # `threads/non-idle` — the index of threads that are doing something
//!
//! **THE ROSTER, KEYED BY THE POOL INSTEAD OF BY ACCOUNT.** It answers one
//! question — *which threads are not idle right now, and where is each one
//! placed* — for the price of ONE keyless subscribe, which is exactly what
//! `accounts`' `threads` cell answered, and was the only reason that cell
//! existed. The cell is gone (R2.S8; `accounts` is version 2) and this roster
//! is the one place that answer lives.
//!
//! ## Why it is a shape and not a cell on `accounts`
//!
//! `accounts.threads` is the one violation of the nesting law on this producer:
//! a `Map<thread_id, {state, executing, queued, unread, detail, since}>` on a
//! shape whose subject is ACCOUNTS. Six fields of another shape's row, under a
//! key that is not that row's key. The file already admits it — *"They are
//! `crate::shapes::threads`'"* — and the operator's ruling is that the fix may
//! cost a second shape:
//!
//! > *"Note that **shapes are consumer items**. Whatever is best for the
//! > consumer, such that it's still the same efficiency, that's what we should
//! > do, **even if it means a second shape**. The shapes are lazy anyways."*
//!
//! **The alternative was ids-plus-a-count on `accounts`, and it fails the
//! efficiency half of that sentence.** A list of thread ids per account tells a
//! consumer WHICH threads are placed, and nothing about which are executing — so
//! the one keyless subscribe that answers *"which threads are executing,
//! fleet-wide"* becomes one subscribe plus N keyed subscribes to `threads`, which
//! is [`Hydrate::PerKey`] and refuses a keyless one. A `threads` row exports at
//! ~3.7 KB against ~120 bytes for an index entry (`crate::shapes::recent`'s
//! arithmetic), and the predicate a consumer actually applies —
//! `executing || state ∈ {active, retrying}` — needs `state` and `executing`,
//! which are the two fields ids-plus-count throws away. Keeping them ON
//! `accounts` to preserve the one-subscribe property is the violation, restated.
//!
//! So the choice was not *fix it* versus *second shape*. It was *second shape*
//! versus *do not fix it*.
//!
//! ## What this carries, and what it drops
//!
//! Six fields per entry, against `accounts.threads`' six — but not the same six:
//!
//! | field | why it is here |
//! |---|---|
//! | `id` | the membership, and a DECLARED `Ref("thread")` rather than a map key a consumer has to be told about |
//! | `account` | the placement — the thing the account key used to carry implicitly |
//! | `state` | half the predicate, and a closed vocabulary |
//! | `executing` | the other half, and NOT derivable from `state`: a thread locked by another daemon is `executing` with no local turn |
//! | `queued` | how many messages wait on it — the roll-up the account board SUMS per card (`queued_messages`), which without it would be N keyed `threads` subscribes |
//! | `since` | when it entered this set, which is what makes the roster sortable; the ONE datum not already on `threads` |
//! | `parent_id`, `ancestors` | lineage (TUI-SUBAGENTS M6; R9.UI1): `ancestors` is what lets a face roll a parent's status up from its whole subtree off this roster alone (`shapewire::subtree`) |
//!
//! **Dropped: `unread`, `detail`.** Both are per-thread detail that
//! `crate::shapes::threads` already carries — `unread` as a cell, `detail` as
//! `status.detail` — and no consumer reads either off the roster. `detail` is
//! the clearest case: it is a park reason in free text, sometimes a paragraph,
//! and it rode every export of every account key.
//!
//! **`queued` was dropped too, on a claim that was FALSE, and is back.** The
//! first cut of this file said no consumer read `queued` off `accounts.threads`
//! and that `lib/shapewire/seam/view.rs`'s `running()` was the cell's only
//! reader. The Python board read it: `threads_tui/model.py`'s activity block
//! summed `queued` per account into `queued_messages`. A roll-up a consumer
//! draws for every card is exactly the "minimum roll-up that makes it
//! answerable without the second call" the next paragraph allows, so it rides
//! here and the board derives the sum off this roster.
//!
//! **This is the line the nesting law actually draws**, and it is worth stating
//! because this shape also holds entries about threads. An INDEX carries a
//! membership or an order plus the minimum roll-up that makes it answerable
//! without the second call; it does not carry a ROW. `threads/recent-updates`
//! already re-serves `unread` and `seen_seq`, which the pool also holds, with
//! the argument written down. The violation on `accounts` was never "a map of
//! structs" — it was six fields of thread detail on a shape about accounts.
//!
//! ## What it costs the fold, which is less than nothing
//!
//! Keying the roster by account forced a cross-key invariant: a thread may
//! appear under exactly one account, so every `Push::Status` had to SCAN every
//! account row looking for the thread somewhere else and remove it
//! (`crate::shapes::accounts`' `elsewhere` walk). That is why that shape carries
//! `BURST: 8` — one status legitimately touches several keys.
//!
//! **Here a thread appears exactly once, so a placement change is a field
//! write.** No scan, no cross-key invariant, and `BURST: 2` — one input touches
//! the one pool.
//!
//! ## It INFORMS; it does not subscribe
//!
//! `crate::shapes::recent`'s rule, and it binds this shape identically: read the
//! ids off the row, pick the ones you want, and subscribe to `threads` with
//! them. Nothing here expands a reference and nothing here opens a subscription
//! on anybody's behalf. The second call is the consumer's.
//!
//! ## Keys: one, and it is `fleet`
//!
//! A pool with one fixed key, for `recent`'s reason: the question is about the
//! FLEET, so keying it by account would be the nesting this shape exists to
//! undo, and keying it by thread id would make the roster a thing you can only
//! read if you already know what is in it.
//!
//! ## Paging
//!
//! [`Paging::Whole`], declared rather than defaulted. The shape has exactly ONE
//! key, so a page of 100 rows and the whole pool are the same answer, and
//! `Paging::Default` would publish a 100-row page ceiling that can never bind —
//! a number a consumer reads off `shapes.list` and reasons about for nothing.
//! The LIST inside the row is not what `page` bounds (rows per subscribe are),
//! and its own bound is [`crate::runtime::MAX_NON_IDLE_HYDRATE_ROWS`], which the
//! hydrate reports as `Partial` rather than silently clipping — rule 2.8's
//! declare-refuse-never-clamp, from the producing end.
//!
//! ## Tracking
//!
//! `Lazy`, for `accounts`' reason rather than `recent`'s: it folds
//! `Push::Status`, which is edge-triggered and cheap, but an unwatched daemon
//! should still pay nothing. This is the laziness the operator's ruling leans
//! on — *"the shapes are lazy anyways"* — and it is what makes a second shape
//! free to an existing consumer that does not subscribe to it.
//!
//! ## Version
//!
//! [`NonIdle::VERSION`] is `1`, published on `shared` as `shape_version`. This
//! shape is NEW, so `1` is a claim about today and not a guess; see
//! [`crate::shapes::threads`]' module doc.

use serde_json::{json, Value};

use crate::event::{Fact, Push};
use crate::shape::{
    Handles, Hydrate, Input, Map, Min, Register, Root, Semilattice, Shape, Stamped,
    Timestamp, Tracking, Tx,
};
use shapewire::cell::Cells;
use shapewire::declare_cells;
use shapewire::operation::{
    DType::{Enum, List, Ref, Scalar as Sc, Struct, Timestamp as Ts},
    Presence::{Optional, Required},
    Scalar::{Bool, Int},
    TimeEncoding::Rfc3339,
};
use shapewire::wire::Paging;

/// **THE ONE KEY** — `crate::shapes::recent::FLEET`'s reason: a single thing is
/// a pool with one fixed key.
pub const FLEET: &str = "fleet";

/// One non-idle thread, and where it is placed.
///
/// `id` is not a field here: it rides from the cell's own key in
/// [`FleetRow::to_json`], so an entry cannot disagree with the key that holds
/// it. `crate::shapes::recent::RecentEntry` carries `id` and `at` the same way
/// and for the same reason.
#[derive(Debug, Clone, Default)]
pub struct PlacedRow {
    /// **WHERE IT IS PLACED.** On `accounts.threads` this was the KEY the row
    /// lived under, which is why that row has no account field; here the roster
    /// is keyed by the pool, so the placement becomes a declared
    /// `Ref("account")` and a move is a field write rather than a re-key.
    pub account: String,
    pub state: String,
    pub executing: bool,
    /// Messages waiting on this thread — `Push::Status::queued` live,
    /// `ThreadEntry::queued` (the store's `waiting_queue_counts`) on hydrate.
    pub queued: u64,
    /// First-seen, joined by `Min` so a replay, a duplicate and a re-hydrate all
    /// converge on the same instant — the rule the `accounts.threads` cell's `since` followed,
    /// carried over unchanged because it is the only field here whose value
    /// depends on when it was first observed rather than on what is true now.
    pub since: Semilattice<Min<Timestamp>>,
    /// **THE THREAD'S PARENT, `None` FOR A ROOT** (`docs/TUI-SUBAGENTS.md` M6)
    /// — so a board can draw a child under its parent and tell an orphan from a
    /// subagent. Immutable lineage, read from [`NonIdleRoot::lineage`].
    pub parent_id: Option<String>,
    /// **EVERY ANCESTOR, NEAREST FIRST, `[]` FOR A ROOT** (R9.UI1, operator
    /// ruling UI-b: a parent's status rolls up from its whole subtree). The
    /// walk up [`NonIdleRoot::lineage`], cycle-safe, through
    /// `shapewire::subtree::ancestors`. It is what lets a face apply
    /// `shapewire::subtree::rollup_in` to THIS roster alone: only an active or
    /// parked descendant can move a parent's status, every such thread is on
    /// this roster, and its `ancestors` name every thread whose status it
    /// moves — including one that is idle and therefore not here.
    pub ancestors: Vec<String>,
}

/// `id`'s ancestors through a child → parent map — the ONE walk this shape
/// uses for [`PlacedRow::ancestors`].
pub fn ancestors_of(lineage: &std::collections::BTreeMap<String, String>, id: &str) -> Vec<String> {
    shapewire::subtree::ancestors(id, |c| lineage.get(c).cloned())
}

impl PlacedRow {
    /// The whole row at once, for the reason the deleted `accounts.threads` cell gave: the
    /// derived event IS the whole row, so naming the consequence once at this
    /// level is as honest as naming it per field and far less noise.
    pub fn emit(
        &mut self,
        at: Timestamp,
        account: &str,
        state: &str,
        executing: bool,
        queued: u64,
    ) {
        self.account = account.to_string();
        self.state = state.to_string();
        self.executing = executing;
        self.queued = queued;
        self.since.emit(at, Min(at));
    }

    fn to_json(&self, id: &str) -> Value {
        json!({
            "id": id,
            "account": self.account,
            "state": self.state,
            "executing": self.executing,
            "queued": self.queued,
            "since": self.since.now().map(|m| m.0),
            "parent_id": self.parent_id,
            "ancestors": self.ancestors,
        })
    }
}

/// One pool's roster.
#[derive(Default)]
pub struct FleetRow {
    /// Keyed by thread id so the fold can write one entry without rebuilding the
    /// roster, and emitted as a LIST so the thread reference is DECLARED
    /// (`id: Ref("thread")`) instead of implied by a map key.
    pub threads: Map<String, PlacedRow>,
}

impl FleetRow {
    fn to_json(&self, key: &str) -> Value {
        json!({
            "pool": key,
            // Sorted by the map's own key order, which is `BTreeMap`'s: stable
            // across exports, so two consumers of one frame agree on the order
            // and a re-export does not reshuffle. The ORDER IS NOT MEANINGFUL
            // and this shape declares no order on `shared` — `recent` is the
            // shape that orders, and a consumer wanting longest-running-first
            // sorts on `since`, which is carried for exactly that.
            "threads": Value::Array(
                self.threads.iter().map(|(id, r)| r.to_json(id)).collect(),
            ),
        })
    }
}

pub struct NonIdleRoot {
    pub pools: Map<String, FleetRow>,
    /// child → parent, every managed child this shape knows of: seeded by the
    /// hydrate off the managed ledgers, grown by `Fact::ThreadChildren`.
    /// Lineage never changes, so a held pair is never wrong.
    pub lineage: std::collections::BTreeMap<String, String>,
    pub shape_version: Register<u32>,
    shared_dirty: bool,
}

impl Default for NonIdleRoot {
    fn default() -> Self {
        Self {
            pools: Map::new(),
            lineage: Default::default(),
            shape_version: Register::new(),
            shared_dirty: false,
        }
    }
}

impl Root for NonIdleRoot {
    fn keys(&self) -> Vec<String> {
        self.pools.keys().cloned().collect()
    }

    fn row(&self, key: &str) -> Option<Value> {
        self.pools.get(&key.to_string()).map(|p| p.to_json(key))
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
            // **THE MEMBERSHIP PREDICATE, ON THE WIRE.** The one thing a
            // consumer must know before it can use this roster is what is IN it,
            // and a predicate that lives only in a doc comment is a predicate
            // the consumer guesses. `idle` is the excluded word; the other four
            // of `crate::runtime::STATE_WORDS` are the set.
            "membership": "every thread whose state is not `idle` — holding a lock, parked, retrying, or holding a queue row",
        })
    }

    fn shared_cells(&self) -> Value {
        json!({ "shape_version": self.shape_version.state() })
    }

    fn drain(&mut self) -> crate::shape::Outbox {
        crate::shape::Outbox {
            rows: self.pools.take_dirty_keys().into_iter().collect(),
            gone: self.pools.take_removed(),
            added: self.pools.take_added(),
            // A thread entering or leaving the roster is not an appended item:
            // the roster is a LEVEL. `recent`'s reason, and `accounts.threads`
            // reasons the same way — `accounts.moves` is where a placement's
            // transition log lives, and two shapes must not both own one log.
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

pub struct NonIdle;

impl NonIdle {
    /// See [`crate::shapes::threads`]' module doc for what a declared version is
    /// and is not.
    pub const VERSION: u32 = 1;
}

declare_cells! {
    /// Per-key cells of `threads/non-idle` — one pool, one cell: the roster.
    /// `id` rides each entry from the cell's own key.
    pub NON_IDLE_CELLS = [
        ("threads", List(&Struct("PlacedThread")), Required, [
            ("id", Ref("thread"), Required),
            ("account", Ref("account"), Required),
            ("state", Enum(crate::runtime::STATE_WORDS), Required),
            ("executing", Sc(Bool), Required),
            // The per-card SUM the account board draws; see the module doc for
            // why this one roll-up rides here and `unread`/`detail` do not.
            ("queued", Sc(Int), Required),
            // `None` until an entry's first-seen instant is known. It is
            // `Optional` for the reason `accounts.threads.since` is: a row
            // hydrated from a candidate with no parseable stamp is carried
            // WITHOUT one rather than given an invented instant.
            ("since", Ts(Rfc3339), Optional),
            // The thread's parent (a `threads` key), `null` for a root. ADDITIVE
            // and LAST, so `NonIdle::VERSION` does not move.
            ("parent_id", Ref("thread"), Optional),
            // Every ancestor (`threads` keys), nearest first, `[]` for a root
            // (R9.UI1). ADDITIVE and LAST, so `NonIdle::VERSION` does not move.
            ("ancestors", List(&Ref("thread")), Required),
        ]),
    ]
}

declare_cells! {
    /// Shape-level cells of `threads/non-idle`. `pool` and `membership` ride
    /// `shared` as constants WITHOUT a cell state and are therefore not here —
    /// `recent`'s rule: a value with no state is a value whose unknown-ness
    /// cannot be expressed, and the manifest declares cells, not constants.
    pub NON_IDLE_SHARED = [
        ("shape_version", Sc(Int), Required, []),
    ]
}

impl Shape<Input> for NonIdle {
    /// **NESTED UNDER `threads`**, because it is an index OVER that pool and
    /// nothing else — so a consumer reaches it from the pool's name, and
    /// `Epoch::parent_epoch` says whether the index and the pool it hands you
    /// keys for are from the same era. `recent`'s argument, unchanged.
    const NAME: &'static str = "threads/non-idle";
    const CELLS: Cells = NON_IDLE_CELLS;
    const SHARED: Cells = NON_IDLE_SHARED;
    /// The index's whole protocol, declared: an entry's `id` is a key of
    /// `threads`, its `account` a key of `accounts`, and the second call is the
    /// consumer's.
    const REFERS: &'static [(&'static str, &'static str)] =
        &[("thread", "threads"), ("account", "accounts")];
    const PARENT: Option<&'static str> = Some("threads");
    const TRACKING: Tracking = Tracking::Lazy;
    /// **`Whole`, and it is not a default.** One key, minted at init, and there
    /// is never a second — so a keyless subscribe is a real inventory of exactly
    /// one pool. Declared even though it is the default, because *"a const whose
    /// only instance is an omission is a const nobody can see"*.
    const HYDRATE: Hydrate = Hydrate::Whole;
    /// **DECLARED, NOT DEFAULTED** — see the module doc's §Paging. One key means
    /// a 100-row chassis page can never bind, and publishing a ceiling that
    /// cannot bind is a number a consumer reasons about for nothing.
    const PAGE: Paging = Paging::Whole;
    /// One `Push::Status` touches the one pool: one `row`, and nothing else.
    /// `accounts` sits at 8 because keying this roster by account made one status
    /// touch several keys — see the module doc's §What it costs the fold.
    const BURST: usize = 2;

    type Producer = crate::runtime::ThreadManager;
    type Root = NonIdleRoot;

    fn root(&self) -> NonIdleRoot {
        NonIdleRoot::default()
    }

    /// **THE COLD SUBSCRIBER'S ROSTER — the SAME read `accounts` uses.**
    ///
    /// `crate::shapes::accounts::ThreadRows` is `thread_rows_non_idle`, which is
    /// already the bounded candidate-set read behind `accounts.threads`. Sharing
    /// it is the point: two shapes over one roster must cost two projections and
    /// not two reads, and a second read would be a second chance to disagree
    /// about who is non-idle.
    fn hydrates(
        &mut self,
        p: &crate::runtime::ThreadManager,
        root: &mut NonIdleRoot,
        as_of: Timestamp,
    ) {
        if root.shape_version.now() != Some(&NonIdle::VERSION) {
            root.shape_version.emit(as_of, NonIdle::VERSION);
            root.shared_dirty = true;
        }
        // Lineage first, so every entry below is written with its parent.
        for (child, parent) in p.hydrate(NonIdleLineage).into_rows() {
            root.lineage.insert(child, parent);
        }
        let got = p.hydrate(crate::shapes::accounts::ThreadRows);
        let complete = got.is_complete();
        let reason = got.reason();
        let rows = got.into_rows();
        // **A FAILED READ IS NOT AN EMPTY FLEET.** `Partial{got: []}` is the
        // handler's answer to a store error; a fleet with nothing running comes
        // back `Empty`, which `is_complete` calls complete — a statement of
        // zero, which renders as an empty roster and not as `?`.
        if rows.is_empty() && !complete {
            let why = reason.unwrap_or_else(|| "the non-idle roster could not be read".into());
            root.pools
                .key(FLEET.to_string())
                .threads
                .mark_not_hydrated(why.clone());
            root.pools.mark_not_hydrated(why);
            root.pools.mark_read(as_of, false);
            let _ = root.drain();
            return;
        }
        {
            let pool = root.pools.key(FLEET.to_string());
            for t in rows {
                let e = pool.threads.key(t.thread_id.clone());
                e.account = t.account.clone();
                e.state = t.state.clone();
                e.executing = t.executing;
                e.queued = t.queued;
                e.parent_id = root.lineage.get(&t.thread_id).cloned();
                e.ancestors = ancestors_of(&root.lineage, &t.thread_id);
                if let Some(since) = t.since {
                    e.since.read(as_of, Min(since));
                }
            }
            pool.threads.mark_read(as_of, complete);
        }
        root.pools.mark_read(as_of, true);
        // A hydrate publishes nothing — the export IS the publication.
        let _ = root.drain();
    }

    /// **EXHAUSTIVE, WITH NO `_` ARM.** One arm moves the roster — `Push::Status`
    /// and nothing else — and everything else says where it lives. A new `Push`
    /// or `Fact` variant is a compile error here, so ignoring it is a conscious
    /// act rather than an accident.
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, NonIdleRoot>) {
        let at = tx.at();
        match &input.value {
            // ── membership, seen through the LEVEL ───────────────────────
            Input::Push(Push::Status {
                thread_id,
                state,
                executing,
                queued,
                unread: _,
                detail: _,
                account,
            }) => {
                // **PRE-INIT IS NOT A ROSTER OF ONE.** The pool key is minted by
                // the hydrate; a status folded before it would otherwise create
                // the pool and publish a roster containing exactly the one thread
                // that happened to move first, which a consumer cannot tell from
                // a fleet with one thread running.
                //
                // **DEFENCE IN DEPTH, AND UNPROVEN — SAID OUT LOUD BECAUSE AN
                // UNREACHABLE GUARD THAT LOOKS TESTED IS WORSE THAN NO GUARD.**
                // Under `Tracking::Lazy` the chassis routes no input to this
                // shape until something subscribes, and the subscribe runs
                // `hydrates` — which mints `FLEET` — before any fold. So there
                // is no reachable ordering in which this line fires, and a test
                // written against it PASSED WITH THE LINE DELETED (recorded, not
                // guessed: `tests/nonidle_shape.rs`'s first-subscribe test went
                // 6/6 green under exactly that mutation). It is kept because
                // `Lazy` is a declaration that could change and `recent`'s fold
                // reads its pool defensively for the same reason; it is NOT
                // kept on the strength of a test, and the sibling test's
                // docstring says so rather than claiming a red it cannot force.
                if tx.pools.get(&FLEET.to_string()).is_none() {
                    return;
                }
                let Some(account) = account else {
                    // `account: None` means the daemon genuinely does not know
                    // where this thread is. An entry whose `account` is declared
                    // `Required` may not be written without one, and inventing a
                    // `"?"` placement would publish a thread as placed somewhere
                    // that does not exist. `accounts` counts this case as
                    // `unplaced_status`; it is the same input and the same
                    // silence, and the number is already on `shapes.list`.
                    return;
                };
                let pool = tx.pools.key(FLEET.to_string());
                if state == "idle" {
                    // **ONLY `idle` REMOVES.** Parked, retrying and queued stay:
                    // they are not free, and a roster that hid them would answer
                    // "nothing is going on" while three threads waited. This is
                    // `accounts.threads`' rule and the membership predicate the
                    // `shared` block publishes.
                    pool.threads.remove(thread_id);
                    return;
                }
                let parent = tx.lineage.get(thread_id).cloned();
                let ancestors = ancestors_of(&tx.lineage, thread_id);
                let pool = tx.pools.key(FLEET.to_string());
                let e = pool.threads.key(thread_id.clone());
                e.emit(at, account, state, *executing, *queued);
                e.parent_id = parent;
                e.ancestors = ancestors;
            }

            // ── lineage, learned from a parent's children level ──────────
            Input::Fact(Fact::ThreadChildren { thread_id, row }) => {
                for r in &row.runs {
                    tx.lineage.insert(r.id.clone(), thread_id.clone());
                }
                // A child already on the roster before its parent's fact
                // arrived (hydrate raced the spawn) is corrected in place —
                // and so is every roster entry BELOW it, whose `ancestors`
                // walk passed through the edge just learned (R9.UI1). Read
                // before reaching: `Map::key` dirties on reach, so only an
                // entry whose lineage actually moved is touched.
                let stale: Vec<(String, Option<String>, Vec<String>)> =
                    match tx.pools.get(&FLEET.to_string()) {
                        Some(pool) => pool
                            .threads
                            .iter()
                            .filter_map(|(id, e)| {
                                let parent = tx.lineage.get(id).cloned();
                                let anc = ancestors_of(&tx.lineage, id);
                                (e.parent_id != parent || e.ancestors != anc)
                                    .then(|| (id.clone(), parent, anc))
                            })
                            .collect(),
                        None => Vec::new(),
                    };
                if !stale.is_empty() {
                    let pool = tx.pools.key(FLEET.to_string());
                    for (id, parent, anc) in stale {
                        let e = pool.threads.key(id);
                        e.parent_id = parent;
                        e.ancestors = anc;
                    }
                }
            }

            // ── everything else, and where it lives ──────────────────────
            //
            // **THE FIVE THREAD FACTS ARE `crate::shapes::threads`'.** This shape
            // holds a membership and a placement; a header, a settings blob, a
            // seen cursor, a compaction marker and the queue's contents are all
            // per-thread detail, which is the thing this shape exists in order
            // NOT to hold. `ThreadPlacement` is the nearest miss and is still
            // not this shape's: it is the durable placement COLUMN, and the
            // EDGE (`Push::Status`) is what decides membership here — folding
            // the column as well would be a second writer for one placement,
            // and a thread whose column moved while it was idle would be
            // admitted to a roster it does not belong in.
            Input::Push(Push::Event(_))
            | Input::Push(Push::Liveness { .. })
            | Input::Push(Push::Progress { .. })
            | Input::Push(Push::Lifecycle(_))
            | Input::Push(Push::Info { .. })
            | Input::Fact(Fact::ThreadHeader { .. })
            | Input::Fact(Fact::ThreadSettings { .. })
            | Input::Fact(Fact::ThreadPlacement { .. })
            | Input::Fact(Fact::ThreadSeen { .. })
            | Input::Fact(Fact::ThreadCompaction { .. })
            | Input::Fact(Fact::ThreadQueue { .. })
            // About an ACCOUNT, not about which threads are busy on it.
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
            // The link beat is `crate::shapes::heartbeat`'s and the warning
            // count is `crate::shapes::warnings`'.
            // R9.TS1: the subtree's rolled status is the `threads` shape's.
            | Input::Fact(Fact::ThreadRolled { .. })
            | Input::Fact(Fact::ThreadSubagents { .. })
            | Input::Fact(Fact::ThreadChildActivity { .. })
            | Input::Fact(Fact::Beat { .. })
            | Input::Fact(Fact::WarningsSampled { .. }) => {}
        }
    }
}

/// Re-exported so a test can name the roster read this shape SHARES with
/// `crate::shapes::accounts` without reaching through that module. One read,
/// two projections — see [`NonIdle::hydrates`].
pub use crate::shapes::accounts::ThreadRows;

/// **EVERY `(child, parent)` PAIR THE MANAGED LEDGERS RECORD** — the seed of
/// [`NonIdleRoot::lineage`]. Whole, read once per hydrate.
#[derive(Default)]
pub struct NonIdleLineage;
impl crate::shape::HydrateRequest for NonIdleLineage {
    const NAME: &'static str = "non_idle_lineage";
    type Response = (String, String);
}

impl Handles<NonIdleLineage> for crate::runtime::ThreadManager {
    fn hydrate(&self, _: NonIdleLineage) -> crate::shape::Hydration<(String, String)> {
        crate::shape::Hydration::Complete(self.managed().lineage())
    }
}
