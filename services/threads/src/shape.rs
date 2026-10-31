//! # `threads`' binding of the shape chassis
//!
//! **The chassis itself now lives in `shapewire`.** This module is what is
//! left behind once it moved: the five things that were never generic, plus
//! re-exports so the rest of the crate keeps its `crate::shape::` paths.
//!
//! `docs/shapes.md` 6.2 names the five. Four of them are here:
//!
//! 1. [`Input`] — the fold's input type. The chassis is generic over it and
//!    only ever asks it for a coarse [`Kind::kind`] name, so `Push` and `Fact`
//!    stay `threads`' business.
//! 2. The **push bus** (`Hub::firehose`), on [`Hub`] beside the chassis rather
//!    than inside it. `emit_at` stamps ONCE, fans out to the shapes, and then
//!    sends the push — the same order and the same single stamp as before the
//!    move. It fed the legacy wire firehose until R2.S4 retired that; it now
//!    has no wire reader, only in-process ones (the update gate's tests and
//!    the integration tests that watch what the daemon did).
//! 3. The **`unplaced_status` counter**, now declared by the shape that owns it
//!    (`shapes::accounts`) through `Shape::counts`. It is still `unplaced_status`
//!    at the top level of `shapes.list`: the chassis flattens its counter map,
//!    so the board's wire did not change.
//! 4. [`FIFO_CAPACITY`] — 1,024, justified by this daemon's measured ledger
//!    peak and meaningless to another producer. Passed to `register` now.
//!
//! The fifth, `Shape::KEYED`, IS GONE — deleted 2026-09-28. It was a const
//! carried onto `shapes.list` and read by no code, and `shapes::narration`'s
//! module doc is the record of an author finding that out: a `KEYED = false`
//! shape would have been acked and handed nothing, because `do_subscribe`
//! iterates `Root::keys()`. Every shape is a keyed pool; a "keyless" one is a
//! pool with one fixed key, which is what narration's three channels are.
//!
//! ```text
//!  ThreadManager
//!    │
//!    ├─► hub.emit(Input::Push(p))  ← stamps `at` ONCE ─┬─► shape FIFOs ─► folds ─► frames
//!    │                                                 └─► firehose (in-process bus; no wire reader since R2.S4)
//!    └─► hub.emit(Input::Fact(f))  ← NO path to the firehose, by construction
//! ```
//!
//! That last line made byte-identity of the legacy wire a CONSTRUCTION and not
//! a discipline while there was one: a `Fact` has no expressible route to the
//! bus.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::event::{Fact, Push};

// The chassis is three modules by ROLE — what a consumer parses, what a
// producer folds with, and the runtime — and `threads` is a producer, so it
// re-exports all three onward under the one `crate::shape::` path the rest of
// this crate already uses.
pub use shapewire::fold::*;
pub use shapewire::hub::{
    ConnFrames, Fifo, Handles, Hydrate, HydrateRequest, Hydration, Item, Kind, Shape, SubscribeId,
    SubscribeItem, Tracking, Tx, CONN_FRAME_CAPACITY, MAX_ARMED_TIMERS_PER_CONN, MAX_FUTURE_AT,
    PROMISE_RETENTION, PROMISE_RETENTION_PER_KEY, REINIT_FLOOR_SECS,
};
pub use shapewire::wire::*;

/// **This daemon's FIFO capacity**, justified by the ledger's measured peak.
/// The chassis offers `DEFAULT_FIFO_CAPACITY`; it does not impose one, because
/// the number means nothing to a producer with a different arrival rate.
pub const FIFO_CAPACITY: usize = shapewire::hub::DEFAULT_FIFO_CAPACITY;

/// What a fold sees: an existing legacy push, or a producer FACT that exists
/// only for shapes.
#[derive(Debug, Clone)]
pub enum Input {
    Push(Push),
    Fact(Fact),
}

impl Input {
    /// The coarse name used in the burst warning, so the operator can tell
    /// which input made a fold re-emit keys it did not change.
    pub fn kind(&self) -> &'static str {
        match self {
            Input::Push(Push::Event(_)) => "event",
            Input::Push(Push::Liveness { .. }) => "liveness",
            Input::Push(Push::Status { .. }) => "status",
            Input::Push(Push::Progress { .. }) => "progress",
            Input::Push(Push::Lifecycle(_)) => "lifecycle",
            Input::Push(Push::Info { .. }) => "info",
            Input::Fact(f) => f.kind(),
        }
    }
}

impl Kind for Input {
    fn kind(&self) -> &'static str {
        Input::kind(self)
    }
}

/// The chassis hub, specialised to this daemon's input.
pub type CoreHub = shapewire::hub::Hub<Input>;

/// **`threads`' hub: the chassis, and the push bus beside it.**
///
/// The bus (still named `firehose`, after the legacy wire transport it fed
/// until R2.S4 retired it) is not a chassis concern, and a second producer
/// would have no use for it. Keeping it here rather than
/// in `shapewire` is `docs/shapes.md` 6.2's coupling 2, and the wrapper is
/// what makes "same order, same single stamp" true without the chassis knowing
/// the word.
pub struct Hub {
    core: Arc<CoreHub>,
    /// Same capacity 4,096 as when it lived on `ThreadManager`. Still `pub`:
    /// `runtime::ThreadManager::subscribe` and two tests read it.
    pub firehose: broadcast::Sender<Push>,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    pub fn new() -> Self {
        let (firehose, _rx) = broadcast::channel(4096);
        Self {
            core: Arc::new(CoreHub::new()),
            firehose,
        }
    }

    /// The chassis hub, for the few places that need it directly.
    pub fn core(&self) -> &Arc<CoreHub> {
        &self.core
    }

    /// The subscriber ledger. Lives on the chassis; reached through here.
    pub fn consumers(&self) -> &shapewire::ledger::ConsumerLedger {
        &self.core.consumers
    }

    /// **Stamp now, fan out, then the firehose.**
    ///
    /// The stamp comes from [`crate::clock::now`] and not `chrono::Utc::now()`
    /// because a stamp is COMPARED, not reported: every cell's stamp rule
    /// refuses an input at or before its `folded_at`, so two emits in the same
    /// microsecond mean the second one is silently DROPPED. Under an optimized
    /// build that is what a tight `for` loop of emits does, and it took
    /// `retention` and `recent_updates` red. See `src/clock.rs`.
    pub fn emit(&self, push: Push) {
        self.emit_at(crate::clock::now(), push);
    }

    /// **Stamp with an `at` the CALLER took before it read the store.**
    ///
    /// Ruling 3, from the critique's F2: `emit_status` does five store reads via
    /// `thread_status` and only THEN sends. If the hub stamped at send time, a
    /// `Status` whose content was read at t0 could arrive stamped t3 — after a
    /// hydrate's `as_of` — and set a LEVEL from pre-hydrate content. Taking the
    /// stamp before the read makes the stamp a lower bound on the content's
    /// age, which is what the stamp rule assumes. `Push` stays untouched.
    ///
    /// Fan-out FIRST, firehose second, off the one stamp — unchanged by the
    /// move into `shapewire`.
    pub fn emit_at(&self, at: Timestamp, push: Push) {
        self.core.emit_at(at, Input::Push(push.clone()));
        let _ = self.firehose.send(push);
    }

    /// **NEVER the firehose.** There is no expressible path from a `Fact` to a
    /// legacy frame — that is what makes byte-identity a construction rather
    /// than a discipline.
    /// Stamped by [`crate::clock::now`], like every other emit here: a stamp is
    /// compared against a cell's `folded_at`, so it is inside the seam
    /// (`clock.rs` clause 2). It used to call `self.core.emit`, which stamps with
    /// the chassis hub's raw `chrono::Utc::now`, out of reach of `ticking` and
    /// `freeze_at` (R9.F2-2; guard `recent_updates::emit_fact_is_stamped_by_the_clock_seam`).
    pub fn emit_fact(&self, fact: Fact) {
        self.core.emit_at(crate::clock::now(), Input::Fact(fact));
    }

    /// The read-then-emit form, for a producer that computes from the store
    /// before it emits (P15's `produce_pressure`). Same reason as
    /// [`Hub::emit_at`].
    pub fn emit_fact_at(&self, at: Timestamp, fact: Fact) {
        self.core.emit_at(at, Input::Fact(fact));
    }

    /// **A PUSH that RESOLVES commands** — the legacy envelope, with promise ids
    /// on the frames it causes.
    ///
    /// It exists because two of the five commands converted on 2026-09-28 are
    /// resolved by the LEDGER ENVELOPE and not by a `Fact`:
    /// `threads/{thread_id}/event` by the `Push::Event` that carries the
    /// addressable position, and `threads/create` by the `Push::Status` whose
    /// membership statement says the thread exists. Both cells already have
    /// exactly one writer — the push arm of `Threads::folds` — and inventing a
    /// `Fact` twin so the promise had something to hang on would have given each
    /// of them a second writer for no reason but the promise.
    ///
    /// **Same order, same single stamp, and the firehose still gets it.** A
    /// `Resolving` has no expressible route to a legacy frame: it rides the
    /// chassis `Stamped` and the firehose is sent the `Push` alone.
    pub fn emit_push_resolving(&self, at: Timestamp, promises: Resolving, push: Push) {
        self.core
            .emit_resolving(at, Input::Push(push.clone()), promises);
        let _ = self.firehose.send(push);
    }

    /// **A fact that RESOLVES commands** (`docs/shapes.md` 2.4). The ids ride on
    /// the frames this fact causes, each one on the frame for the SUBJECT it is
    /// about — see [`Resolving`], and `PromiseBook::resolve`, which routes them so
    /// a producer cannot hand them over unrouted. Still never the firehose.
    pub fn emit_fact_resolving(&self, promises: Resolving, fact: Fact) {
        self.core
            .emit_resolving(crate::clock::now(), Input::Fact(fact), promises);
    }

    /// Register a shape, at this daemon's FIFO capacity.
    pub fn register<S>(&self, shape: S, producer: S::Producer)
    where
        S: Shape<Input>,
    {
        self.core.register(shape, producer, FIFO_CAPACITY);
    }

    /// **THIS CONNECTION'S OWN FRAME CHANNEL.** Was `subscribe_frames()`, a
    /// receiver on one shared `broadcast` — see [`ConnFrames`] for why a directed
    /// frame must not be charged to every other connection's buffer.
    pub fn connect(&self, conn: ConnId) -> ConnFrames {
        self.core.connect(conn)
    }
    /// **ONE CONNECTION-ID SPACE, AND IT IS THE HUB'S.** Two servers serve this
    /// daemon during the API's move into the chassis; a counter per server would
    /// mint the same id twice over one hub. See `shapewire::hub::Hub`.
    pub fn next_conn(&self) -> ConnId {
        self.core.next_conn()
    }
    pub fn tracked(&self, shape: &str) -> bool {
        self.core.tracked(shape)
    }
    pub fn any_tracking(&self) -> bool {
        self.core.any_tracking()
    }
    pub fn names(&self) -> Vec<&'static str> {
        self.core.names()
    }
    pub fn list(&self) -> Vec<ShapeInfo> {
        self.core.list()
    }
    pub fn dropped(&self, conn: ConnId) {
        self.core.dropped(conn)
    }
    pub fn subscribe(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        at: Option<Timestamp>,
    ) -> std::result::Result<SubscribeId, ShapeError> {
        self.core.subscribe(shape, key, conn, at)
    }
    /// `version` is rule 2.6's link: what the consumer states it was built
    /// against, checked once against the shape's declared contract and REFUSED on
    /// the request id if this producer does not serve it. `None` states nothing and
    /// is served — see `shapewire::hub::Hub::subscribe_as`.
    ///
    /// Forwarded rather than defaulted here, because a wrapper that quietly passed
    /// `None` would make this producer unable to refuse a link no matter what a
    /// consumer stated, which is the rule's whole content removed by a
    /// convenience.
    #[allow(clippy::too_many_arguments)]
    pub fn subscribe_as(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        at: Option<Timestamp>,
        identity: Option<crate::consumers::ConsumerIdentity>,
        detail: Detail,
        retain: Retain,
        beat: Beat,
        version: Option<u32>,
        page: shapewire::wire::Paging,
        from: Option<&str>,
    ) -> std::result::Result<SubscribeId, ShapeError> {
        self.core.subscribe_as(
            shape, key, conn, at, identity, detail, retain, beat, version, page, from,
        )
    }

    /// [`Self::subscribe`] with the `detail` parameter stated — see
    /// [`Detail`]. Omitting it means [`Detail::Rows`], the ceiling.
    pub fn subscribe_detail(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        at: Option<Timestamp>,
        detail: Detail,
    ) -> std::result::Result<SubscribeId, ShapeError> {
        self.core.subscribe_detail(shape, key, conn, at, detail)
    }
    /// [`Self::subscribe`] with EVERY subscriber parameter stated — see
    /// [`Retain`] for the history axis and who pays for a large ask, and
    /// [`Beat`] for the RATE axis and the six ways a rate ceiling is not a
    /// quantity ceiling.
    pub fn subscribe_ask(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        at: Option<Timestamp>,
        detail: Detail,
        retain: Retain,
        beat: Beat,
    ) -> std::result::Result<SubscribeId, ShapeError> {
        self.core
            .subscribe_ask(shape, key, conn, at, detail, retain, beat)
    }
    /// **THE CHASSIS'S LAG REPAIR** — see
    /// [`shapewire::hub::Hub::resubscribe_lagged`]. It is NOT
    /// [`Self::subscribe`] with the defaults, and the difference is the point:
    /// `subscribe` means *asking nothing means the ceiling*, so repairing a
    /// lagging connection through it upgraded that connection to `Retain::Max`
    /// and `Beat::Max` — more load, for the one consumer that had just proved it
    /// could not keep up.
    pub fn resubscribe_lagged(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        detail: Detail,
    ) -> std::result::Result<SubscribeId, ShapeError> {
        self.core.resubscribe_lagged(shape, key, conn, detail)
    }
    pub fn unsubscribe(&self, shape: &str, key: Option<&str>, conn: ConnId) {
        self.core.unsubscribe(shape, key, conn)
    }
    pub fn is_dead(&self, shape: &str) -> Option<String> {
        self.core.is_dead(shape)
    }
}
