//! # The `heartbeat` shape — is the LINK alive, not is the thread busy
//!
//! Operator ruling: *"heartbeat wins then."* His own doubt, which this module
//! exists to answer rather than to dodge:
//!
//! > *"I could see a heartbeat shape being something that we just want in
//! > general. Asking for heartbeat should probably come with a frequency. But
//! > idk how reliable a broken transport detection is in the websocket protocol.
//! > If it's a 100% reliable signal of connection being maintained then I don't
//! > see why we need a heartbeat."*
//!
//! ## The answer: ping/pong is not insufficient because it is unreliable
//!
//! It is insufficient because it answers a DIFFERENT QUESTION, and the
//! difference is the whole design rationale.
//!
//! A TCP connection can be silently dead for minutes with both ends believing it
//! is fine — that is why WebSocket has ping frames at all, and this chassis
//! already sends them: [`shapewire::server::PING_INTERVAL`], the fourth
//! `select!` arm of the connection loop. Those pings are good at what they do.
//!
//! But a ping proves the SOCKET is alive. It proves nothing about whether FRAMES
//! ARE FLOWING. The ping is written by the connection loop's own timer arm; it
//! never touches the hub, never enters a shape's FIFO, is never folded, and is
//! never drained out of a `Root`. So a wedged fold behind a healthy socket and
//! working pings looks calm and is broken — which is the failure the board's own
//! header code names:
//!
//! > *"A FROZEN SCREEN THAT LOOKS CALM IS THE FAILURE THIS EXISTS TO PREVENT."*
//!
//! ## Which makes the route the ONLY reason this shape may exist
//!
//! A heartbeat earns its place only if it travels the FRAME path. This one does,
//! end to end, and every link is a real one:
//!
//! ```text
//!   beat task (runtime.rs)  ── hub.emit(Fact::Beat{seq}) ──►  the shape's FIFO
//!        │                                                          │
//!        │                                            ShapeTask::run's select
//!        │                                                          ▼
//!        └────────────────────────────────────────────────  folds() → tx dirty
//!                                                                    │
//!                                    Root::drain() → flush_rows → emit_live_row
//!                                                                    │
//!                                        the beat gate, then the connection's
//!                                        own ConnFrames channel → the socket
//! ```
//!
//! Receiving one therefore proves every one of: something is still emitting;
//! the FIFO is accepting and has not overflowed past repair; the shape task is
//! scheduled and its fold returned; the drain and the flush ran; the
//! connection's frame channel is being read. A ping proves none of those, and
//! **a heartbeat that rode the socket instead would be a ping with extra steps
//! and should not have been built.**
//!
//! `tests/heartbeat.rs::a_wedged_fold_stops_the_beat_while_the_shape_still_looks_alive`
//! is the proof: it blocks the fold, keeps beating, and shows the frames stop
//! while nothing is dead, nothing is refused, and no death export is emitted.
//! That is the frozen screen, caught.
//!
//! ## What this does NOT answer, and where that answer lives
//!
//! Three questions get conflated the moment a reader sees the word "alive", so
//! they are named apart here:
//!
//! | question | who answers it |
//! |---|---|
//! | Is the SOCKET open? | the websocket ping, `server.rs`'s fourth `select!` arm. Nothing here. |
//! | Is this THREAD's turn still working? | [`crate::shapes::threads`]' `liveness` cell — a LEVEL on the thread the beat is about, fed by `Push::Liveness`. It is about a thread being BUSY, it exists only while something runs, and its silence means "nothing is running". |
//! | Are FRAMES still flowing to ME? | this shape, and only this shape. |
//!
//! **`Push::Liveness` is not this and this is not it.** That variant already has
//! a home and keeps it. The distinction in one line: `liveness` is about a THREAD
//! DOING WORK, and `heartbeat` is about a LINK CARRYING FRAMES. A thread can be
//! idle for a week with the link perfectly healthy, and the link can be wedged
//! while three threads beat 3×/s into a fold nobody drains. Neither is evidence
//! about the other.
//!
//! ## A key is: the link. There is one, and it is `link`
//!
//! `docs/shape-design-guide.md` step 1: *"a single thing is a pool with one fixed
//! key"* — `narration`'s three channels arriving at a smaller number. There is
//! nothing to key a beat BY. It is not per thread (that is `liveness`), not per
//! account, and not per connection: a shape has one root shared by every
//! subscriber, so a per-connection key would be a per-subscriber root, which this
//! chassis does not have and `crate::shape::Retain`'s doc explains at length.
//! What differs per connection is the CADENCE, and that is a delivery projection,
//! not a key.
//!
//! ## The cell is a `Register<u64>` of a monotone count, and it may not dedupe
//!
//! `crate::shapes::threads`' rule — *"a level fed by a beat drops a beat that
//! repeats the held level"* — is exactly inverted here, and the inversion is the
//! point. There, the beat is a carrier for a value and a repeat carries nothing
//! new. Here the ARRIVAL is the whole content, so a heartbeat that deduped would
//! emit one frame and then go silent forever: the perfect stub that pretends to
//! work. A monotone `seq` is what makes every beat a distinct value, and
//! `a_beat_that_repeats_a_value_would_never_arrive_so_the_seq_monotonically_moves`
//! is the test that fails if anyone makes this cell idempotent by analogy with
//! that rule.
//!
//! `seq` is the PRODUCER's count of beats made. The gate does not renumber it —
//! see [`crate::shape::Beat`] §6: under `beat: n` the gaps in what a consumer
//! sees are its own ask, not loss.
//!
//! ## The frequency is subscriber-declared under a producer MAX
//!
//! Operator: *"the producer should offer a max contract to the consumer and the
//! consumer can choose to utilize up to that max. So again the answer is to set a
//! MAX frequency."* Built exactly as `retain` was, which is the precedent: a
//! `const` on the shape ([`Heartbeat::MIN_BEAT_INTERVAL_MS`]), published on
//! `shapes.list` as `max.beat_interval_ms`, asked for on the subscribe as
//! `beat: max | <ms>`, refused naming the floor and never clamped.
//!
//! **It is the first rate ceiling on this chassis, where every existing one is a
//! quantity, and [`crate::shape::Beat`] states the six consequences.** The two
//! worth carrying in a shape author's head: the refusal could not reuse
//! `AboveMax` because *beyond the ceiling* is the SMALLER number when the
//! parameter is a period; and "asking nothing means the ceiling" stops being free
//! — on the quantity axis the default costs the producer nothing extra, and here
//! it is the most expensive delivery the contract allows, on every connection
//! that never named the knob.
//!
//! ## `Lazy`, and the beat costs nothing when nobody is listening
//!
//! `Lazy` means the FIFO accepts nothing with no subscribers and the root is
//! dropped on the last unsubscribe. The beat task checks `hub.tracked` before it
//! emits, so an unwatched daemon does no beat work at all — which matters more
//! here than for any other shape on this producer, because this is the one input
//! that arrives whether or not anything happened.
//!
//! ## Version
//!
//! [`Heartbeat::VERSION`] is `1`, published on `shared` as `shape_version`. See
//! [`crate::shapes::threads`]' module doc for what declaring the number does and
//! does not buy — `SERVES` and the link-time refusal are not built.

use serde_json::{json, Value};

use crate::event::{Fact, Push};
use shapewire::cell::Cells;
use shapewire::declare_cells;
use shapewire::operation::{
    DType::{Scalar as Sc},
    Presence::{Required},
    Scalar::{Int},
};
use crate::shape::{Hydrate, Input, Map, Register, Root, Shape, Stamped, Timestamp, Tracking, Tx};

/// **THE ONE KEY.** There is one link to be alive or not, so there is one key.
pub const KEY_LINK: &str = "link";

/// **THE PRODUCER'S OWN PERIOD, AND THEREFORE THE MAX FREQUENCY** — one beat per
/// second. It is the rate the beat task emits at AND the ceiling a `beat` ask is
/// refused against, deliberately ONE number: a ceiling that named a rate the
/// producer did not actually emit at would be a contract nothing honours, and the
/// pairing between the two is not something the chassis can check.
///
/// One second rather than [`shapewire::server::PING_INTERVAL`]'s thirty: the
/// ping is a keepalive for an idle socket and 30 s is the right cost for that;
/// this is a liveness signal a consumer may want to act on, and a 30 s beat means
/// a wedged fold is invisible for half a minute. It is also the rate
/// `Push::Liveness` already beats at, so the daemon's fastest existing periodic
/// input is not made faster by this shape existing.
pub const BEAT_INTERVAL_MS: u64 = 1_000;

/// One link: the beat count, and nothing else.
#[derive(Debug, Default)]
pub struct Link {
    /// **A MONOTONE COUNT OF BEATS THE PRODUCER HAS MADE.**
    ///
    /// Not a clock and not an age — `docs/shape-design-guide.md` §6: anything
    /// with `now` in it is derived at render. The frame already carries its own
    /// `at`; a consumer that wants an age subtracts.
    pub seq: Register<u64>,
}

pub struct HeartbeatRoot {
    pub links: Map<String, Link>,
    pub shape_version: Register<u32>,
    shared_dirty: bool,
}

impl Default for HeartbeatRoot {
    fn default() -> Self {
        Self {
            links: Map::new(),
            shape_version: Register::new(),
            shared_dirty: false,
        }
    }
}

impl Root for HeartbeatRoot {
    fn keys(&self) -> Vec<String> {
        self.links.keys().cloned().collect()
    }

    fn row(&self, key: &str) -> Option<Value> {
        self.links
            .get(&key.to_string())
            .map(|l| json!({ "link": key, "seq": l.seq.now() }))
    }

    fn cells(&self, key: &str) -> Option<Value> {
        self.links
            .get(&key.to_string())
            .map(|l| json!({ "seq": l.seq.state() }))
    }

    /// **THE CEILING IS PUBLISHED TWICE AND THAT IS DELIBERATE.**
    ///
    /// `max.beat_interval_ms` on `shapes.list` is the chassis's home for it and
    /// is what a refusal names. This copy is on the shape's own `shared`, beside
    /// `shape_version`, for the consumer that is holding a mirror of this shape
    /// and has not called `shapes.list` — the same hand-published pattern the
    /// other four shapes use for `shape_version`, and it is read off the same
    /// `const`, so the two cannot drift.
    fn shared(&self) -> Value {
        json!({
            "shape_version": self.shape_version.now(),
            "link": KEY_LINK,
            "beat_interval_ms": BEAT_INTERVAL_MS,
        })
    }

    fn shared_cells(&self) -> Value {
        json!({ "shape_version": self.shape_version.state() })
    }

    fn drain(&mut self) -> crate::shape::Outbox {
        crate::shape::Outbox {
            rows: self.links.take_dirty_keys().into_iter().collect(),
            gone: self.links.take_removed(),
            added: self.links.take_added(),
            // No windowed cell here, so no appended item can exist. Which is also
            // why `MAX_RETAINED_ITEMS` stays 0 and a `retain: n` on this shape is
            // refused `NoRetention` — two knobs, two independent declarations.
            transitions: Vec::new(),
            shared: std::mem::take(&mut self.shared_dirty),
        }
    }

    fn mark_dead(&mut self, reason: &str) {
        self.shape_version.mark_dead(reason);
        self.links.mark_dead(reason);
        for (_, l) in self.links.entries_mut() {
            l.seq.mark_dead(reason);
        }
    }
}

pub struct Heartbeat;

impl Heartbeat {
    /// See [`crate::shapes::threads`]' module doc for what a declared version is
    /// and is not.
    pub const VERSION: u32 = 1;
}

declare_cells! {
    /// Per-key cells of `heartbeat`: the beat count. A monotone count, not a
    /// cursor — nobody echoes it back.
    pub HEARTBEAT_CELLS = [
        ("seq", Sc(Int), Required, []),
    ]
}

declare_cells! {
    /// Shape-level cells of `heartbeat`.
    pub HEARTBEAT_SHARED = [
        ("shape_version", Sc(Int), Required, []),
    ]
}

impl Shape<Input> for Heartbeat {
    const NAME: &'static str = "heartbeat";
    const CELLS: Cells = HEARTBEAT_CELLS;
    const SHARED: Cells = HEARTBEAT_SHARED;
    /// **`Lazy`, and here it is load-bearing rather than merely right.** Every
    /// other shape on this producer is `Lazy` to keep a hydrate or a firehose out
    /// of an undrained FIFO; this one is `Lazy` so that an input which arrives
    /// WHETHER OR NOT ANYTHING HAPPENED costs an unwatched daemon nothing.
    const TRACKING: Tracking = Tracking::Lazy;
    /// **`Whole`.** One key, minted at init, and there is never a second — so
    /// "every key I know about" and "every key there is" are the same one key,
    /// which is exactly what `Whole` claims. A keyless subscribe here is an
    /// inventory of one.
    const HYDRATE: Hydrate = Hydrate::Whole;
    /// One beat touches one key: one `row` and nothing else.
    const BURST: usize = 2;
    /// **THE RATE CEILING** — see [`BEAT_INTERVAL_MS`] and [`crate::shape::Beat`].
    /// The first `const` on this producer that bounds work per unit time.
    const MIN_BEAT_INTERVAL_MS: u64 = BEAT_INTERVAL_MS;

    type Producer = crate::runtime::ThreadManager;
    type Root = HeartbeatRoot;

    fn root(&self) -> HeartbeatRoot {
        HeartbeatRoot::default()
    }

    /// **NO HYDRATE REQUEST, AND FOR A SHARPER REASON THAN `narration`'S.**
    ///
    /// There, the inputs are transient and nothing durable exists to read. Here
    /// there is nothing to read even in principle: the count of beats a PREVIOUS
    /// epoch made is not a fact about this link, and exporting it would be the
    /// last connection's liveness claimed as this one's. So the counter starts at
    /// zero every init, and the export's own `epoch` is what says which era the
    /// number belongs to.
    fn hydrates(
        &mut self,
        _p: &crate::runtime::ThreadManager,
        root: &mut HeartbeatRoot,
        as_of: Timestamp,
    ) {
        if root.shape_version.now() != Some(&Heartbeat::VERSION) {
            root.shape_version.emit(as_of, Heartbeat::VERSION);
            root.shared_dirty = true;
        }
        root.links.key(KEY_LINK.to_string()).seq.read(as_of, 0);
        root.links.mark_read(as_of, true);
        // A hydrate publishes nothing — the export IS the publication.
        let _ = root.drain();
    }

    /// **EXHAUSTIVE, WITH NO `_` ARM**, like the other four: a new input variant
    /// must be a compile error here rather than a silent non-fold.
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, HeartbeatRoot>) {
        let at = tx.at();
        match &input.value {
            // **NO `get`-BEFORE-`key` GUARD, AND THAT IS THE DESIGN.** Every other
            // fold on this producer reads before it reaches, because `Map::key`
            // dirties on reach and a repeat must not cost a frame. Here a repeat
            // is impossible by construction (`seq` is monotone) and a frame per
            // beat is the entire product. Dropping a repeat would be the
            // idempotent-level rule applied where it inverts — see the module doc.
            Input::Fact(Fact::Beat { seq }) => {
                tx.links.key(KEY_LINK.to_string()).seq.emit(at, *seq);
            }

            // ── homed elsewhere, every one of them ───────────────────────
            //
            // A beat is about the LINK, so nothing else belongs here — not even
            // `Push::Liveness`, which is about a THREAD being busy and lives on
            // `crate::shapes::threads` as a level. The module doc's table names
            // the three questions apart.
            Input::Push(Push::Event(_))
            | Input::Push(Push::Status { .. })
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
            // A queue's contents are about a THREAD; this shape is about the
            // LINK. Nothing about what is waiting to be delivered says whether
            // frames are still reaching a consumer.
            | Input::Fact(Fact::ThreadQueue { .. })
            | Input::Fact(Fact::ThreadChildren { .. })
            | Input::Fact(Fact::ThreadRolled { .. })
            | Input::Fact(Fact::ThreadSubagents { .. })
            | Input::Fact(Fact::ThreadChildActivity { .. })
            // The producer's own warning count is `crate::shapes::warnings`'
            // business and nothing else's.
            | Input::Fact(Fact::WarningsSampled { .. }) => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// NO `Handles` IMPL EXISTS FOR THIS SHAPE, DELIBERATELY
// ─────────────────────────────────────────────────────────────────────────────
//
// The shape makes no hydrate request, so `docs/shapes.md` §4.4's first
// compile-time guarantee — "the producer answers every request the shape makes" —
// is vacuous here, exactly as it is for `narration`. Stated rather than left to be
// noticed: the guarantee is not weakened, there is nothing for it to check, and
// this shape's hydrate cost is ZERO statements.
