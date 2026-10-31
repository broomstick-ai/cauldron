//! # The heartbeat, and the first RATE ceiling — rule 2.8's fifth knob
//!
//! Operator: *"heartbeat wins then."* And on the cadence: *"the producer should
//! offer a max contract to the consumer and the consumer can choose to utilize
//! up to that max. So again the answer is to set a MAX frequency."*
//!
//! **The claim these tests exist to settle** is that a heartbeat is not a
//! websocket ping with extra steps. It earns its place only if it TRAVELS THE
//! FRAME PATH — emitted by a producer, through the shape's FIFO, through the
//! fold, through the drain, out of the connection's own channel — because then
//! receiving one proves the thing a ping cannot: that frames are flowing.
//! `a_wedged_fold_stops_the_beat_while_the_shape_still_looks_alive` is the test
//! that can tell the two designs apart. It blocks the fold and shows the beats
//! stop while nothing is dead, nothing is refused, and no death export is
//! emitted — *"a frozen screen that looks calm"*, caught.
//!
//! **What a rate ceiling changes, tested rather than asserted in prose.** Every
//! other ceiling on this chassis bounds a QUANTITY.
//! `a_beat_faster_than_the_producer_is_refused_naming_the_floor_and_says_MORE`
//! pins the one that reaches into the code: *beyond the ceiling* is the SMALLER
//! number when the parameter is a period, so the refusal cannot be `AboveMax`
//! and its remedy cannot be "or less".
//!
//! **Every test here was forced red by the mutation named in its own docstring,
//! and the red was checked for `error[E` before it was believed.** Where a claim
//! has no production mutation that can flip it, the docstring says so in place
//! rather than naming a mutation that does not exist.
//!
//! Hub-driven, not socket-driven, with one deliberate exception
//! (`the_registered_heartbeat_shape_beats_with_nobody_emitting`, which is about
//! the production wiring and therefore has to use it). **Every assertion is on a
//! frame a CONNECTION RECEIVED**, never on hub-internal state: proving that a
//! mechanism ran is not proving that anything reached a consumer, and a promise
//! book that is empty proves only that a resolve happened.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use threads::config::Config;
use threads::event::{Fact, Push};
use threads::shape::{
    Beat, ConnFrames, Derived, Hub, Hydrate, Input, Map, Outbox, Register, Root, Shape, ShapeError,
    ShapeFrame, Stamped, Timestamp, Tracking, Tx,
};
use threads::{build_manager, ThreadManager};
use shapewire::cell::Cells;

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

fn config(tmp: &std::path::Path) -> Config {
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    Config {
        data_dir: Some(tmp.to_string_lossy().to_string()),
        accounts: Default::default(),
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

async fn start() -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    // THE CLOCK SEAM. Every `Hub::emit` stamps with the clock and every cell
    // refuses an input at or before its `folded_at`, so two emits in one
    // microsecond mean the second is DROPPED — which is what an optimized
    // build does to a tight emit loop. See `src/clock.rs`.
    threads::clock::testing::ticking();
    let manager = build_manager(config(tmp.path())).unwrap();
    (manager, tmp)
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
}

fn drain(f: &mut ConnFrames) -> Vec<Arc<ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

/// The `seq` off every LIVE `row` frame a connection received, in arrival order.
/// The live path, never the export: a beat is a live occurrence, and an export
/// answering a subscribe is not one.
fn beats(frames: &[Arc<ShapeFrame>]) -> Vec<u64> {
    frames
        .iter()
        .filter_map(|f| match &f.wire.event {
            Some(Derived::Row { row }) => row["seq"].as_u64(),
            _ => None,
        })
        .collect()
}

/// The `seq` off the EXPORT that answered a subscribe.
fn exported_seq(frames: &[Arc<ShapeFrame>]) -> Option<u64> {
    frames
        .iter()
        .find(|f| f.wire.row.is_some())
        .and_then(|f| f.wire.row.as_ref().unwrap()["seq"].as_u64())
}

// ─────────────────────────────────────────────────────────────────────────────
// the test shapes
// ─────────────────────────────────────────────────────────────────────────────

/// The producer's period for the test shape. Small enough that a handful of
/// beats fit in a test, and comfortably above `DEBOUNCE_MS` below so that two
/// beats are two frames rather than one coalesced one.
const TEST_BEAT_MS: u64 = 50;

/// How long the wedged fold blocks for. Long enough that the test can observe
/// the silence and still finish.
const WEDGE_MS: u64 = 1_200;

#[derive(Debug, Default)]
struct BeatLink {
    seq: Register<u64>,
}

#[derive(Default)]
struct BeatRoot {
    links: Map<String, BeatLink>,
}

impl Root for BeatRoot {
    fn keys(&self) -> Vec<String> {
        self.links.keys().cloned().collect()
    }
    fn row(&self, k: &str) -> Option<Value> {
        self.links
            .get(&k.to_string())
            .map(|l| json!({"link": k, "seq": l.seq.now()}))
    }
    fn cells(&self, k: &str) -> Option<Value> {
        self.links
            .get(&k.to_string())
            .map(|l| json!({"seq": l.seq.state()}))
    }
    fn shared(&self) -> Value {
        json!({})
    }
    fn shared_cells(&self) -> Value {
        json!({})
    }
    fn drain(&mut self) -> Outbox {
        Outbox {
            rows: self.links.take_dirty_keys().into_iter().collect(),
            gone: self.links.take_removed(),
            added: self.links.take_added(),
            transitions: Vec::new(),
            shared: false,
        }
    }
    fn mark_dead(&mut self, _: &str) {}
}

/// How many times the wedging fold has actually blocked. A positive control: a
/// test that asserts silence must prove the mechanism it is silencing engaged.
static WEDGED: AtomicU64 = AtomicU64::new(0);

/// **A SHAPE THAT BEATS, WITH ONE KEY AND A DELIBERATE WEDGE INPUT.**
///
/// The wedge is `Push::Info { reason: "wedge" }`, which blocks the fold for
/// [`WEDGE_MS`] — a fold that is stuck, not one that died. The distinction is the
/// whole point: a dead task emits a death export and refuses new subscribes, so a
/// consumer IS told; a wedged one emits nothing at all and every control still
/// says the shape is fine.
struct Beater;

// The cells this file's test roots emit, declared — `Shape::CELLS` is required
// (see `shapewire::cell`), and a test shape declares what its `Root::cells`
// actually spells.
shapewire::declare_cells! {
    SEQ_CELLS = [("seq", shapewire::operation::DType::Scalar(shapewire::operation::Scalar::Int), shapewire::operation::Presence::Required, [])]
}

// The cells this file's test roots emit, declared — `Shape::CELLS` is required
// (see `shapewire::cell`), and a test shape declares what its `Root::cells`
// actually spells.
shapewire::declare_cells! {
    N_CELLS = [("n", shapewire::operation::DType::Scalar(shapewire::operation::Scalar::Int), shapewire::operation::Presence::Required, [])]
}

impl Shape<Input> for Beater {
    const NAME: &'static str = "beater";
    const CELLS: Cells = SEQ_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const HYDRATE: Hydrate = Hydrate::Whole;
    const MIN_BEAT_INTERVAL_MS: u64 = TEST_BEAT_MS;
    /// Well under [`TEST_BEAT_MS`] so the row debounce cannot coalesce two beats
    /// into one frame and make the gate look like it worked.
    const DEBOUNCE_MS: u64 = 5;
    type Producer = ThreadManager;
    type Root = BeatRoot;
    fn root(&self) -> BeatRoot {
        BeatRoot::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut BeatRoot, as_of: Timestamp) {
        root.links.key("link".to_string()).seq.read(as_of, 0);
        root.links.mark_read(as_of, true);
        let _ = root.drain();
    }
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, BeatRoot>) {
        let at = tx.at();
        match &input.value {
            Input::Fact(Fact::Beat { seq }) => {
                tx.links.key("link".to_string()).seq.emit(at, *seq);
            }
            Input::Push(Push::Info { reason, .. }) if reason == "wedge" => {
                WEDGED.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(WEDGE_MS));
            }
            _ => {}
        }
    }
}

/// **A SHAPE THAT DOES NOT BEAT** — it declares nothing, so it keeps the chassis
/// default of `MIN_BEAT_INTERVAL_MS = 0`, which is what every shape written
/// before `beat` existed has.
#[derive(Default)]
struct SilentRoot {
    rows: Map<String, Register<u64>>,
}
impl Root for SilentRoot {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }
    fn row(&self, k: &str) -> Option<Value> {
        self.rows.get(&k.to_string()).map(|n| json!({"n": n.now()}))
    }
    fn cells(&self, k: &str) -> Option<Value> {
        self.rows
            .get(&k.to_string())
            .map(|n| json!({"n": n.state()}))
    }
    fn shared(&self) -> Value {
        json!({})
    }
    fn shared_cells(&self) -> Value {
        json!({})
    }
    fn drain(&mut self) -> Outbox {
        Outbox {
            rows: self.rows.take_dirty_keys().into_iter().collect(),
            gone: self.rows.take_removed(),
            added: self.rows.take_added(),
            transitions: Vec::new(),
            shared: false,
        }
    }
    fn mark_dead(&mut self, _: &str) {}
}

struct Silent;
impl Shape<Input> for Silent {
    const NAME: &'static str = "silent";
    const CELLS: Cells = N_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    type Producer = ThreadManager;
    type Root = SilentRoot;
    fn root(&self) -> SilentRoot {
        SilentRoot::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut SilentRoot, as_of: Timestamp) {
        root.rows.key("a".to_string()).read(as_of, 1);
        root.rows.mark_read(as_of, true);
        let _ = root.drain();
    }
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, SilentRoot>) {}
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. The beat travels the frame path
// ─────────────────────────────────────────────────────────────────────────────

/// **A BEAT ARRIVES AT A CONSUMER HOLDING THE KEY, AND THE SEQ MOVES EVERY TIME.**
///
/// The arrival half of the frame-path claim, and the assertion is on frames a
/// connection RECEIVED. Three beats, three frames, `[1, 2, 3]` — which also pins
/// the design decision this shape's fold inverts: `crate::shapes::threads` drops
/// a beat that repeats the held level, and a heartbeat that did the same would
/// emit one frame and then be silent forever. A monotone `seq` is what makes
/// every beat a distinct value.
///
/// It runs against the PRODUCTION shape, not the test one, so that the mutation
/// below is a mutation of the thing that ships.
///
/// **THE MUTATION.** In `lib/threads/src/shapes/heartbeat.rs`, give the fold's
/// `Beat` arm the read-before-reach guard every other fold on this producer has:
/// `if tx.links.get(&KEY_LINK.to_string()).is_some() { return; }` — the
/// idempotent-level rule applied where it inverts.
///
/// Forced red:
/// ```text
/// three beats emitted through the FIFO are three live row frames at a consumer
/// holding the key, and the seq moves on every one: []
/// ```
#[tokio::test]
async fn a_beat_arrives_at_a_consumer_holding_the_key_and_the_seq_advances() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(threads::shapes::heartbeat::Heartbeat, m.clone());

    let mut rx = hub.connect(101);
    hub.subscribe("heartbeat", Some("link"), 101, None).unwrap();
    settle().await;
    assert_eq!(
        exported_seq(&drain(&mut rx)),
        Some(0),
        "the export that answers the subscribe carries the hydrate's zero"
    );

    // **SPACED PAST THE PRODUCTION SHAPE'S OWN CEILING**, which is the first
    // thing a rate knob does to a test: this connection asked for nothing, so it
    // asked for `Beat::Max`, which IS `BEAT_INTERVAL_MS` — emit three beats 160 ms
    // apart and the gate correctly shows one. A quantity ceiling has no such
    // effect on a test, because it does not depend on when anything happened.
    for seq in 1..=3u64 {
        m.hub.emit_fact(Fact::Beat { seq });
        tokio::time::sleep(std::time::Duration::from_millis(
            threads::shapes::heartbeat::BEAT_INTERVAL_MS + 100,
        ))
        .await;
    }
    settle().await;

    let got = beats(&drain(&mut rx));
    assert_eq!(
        got,
        vec![1, 2, 3],
        "three beats emitted through the FIFO are three live row frames at a \
         consumer holding the key, and the seq moves on every one: {got:?}"
    );
    drop(tmp);
}

/// **A WEDGED FOLD STOPS THE BEAT WHILE EVERY OTHER CONTROL SAYS THE SHAPE IS
/// FINE** — the whole reason a heartbeat is not a websocket ping.
///
/// The failure being reproduced is the one the board's header names: *"a frozen
/// screen that looks calm."* The socket is open, the chassis ping timer is
/// untouched (it lives in the connection loop's own `select!` arm and never
/// enters the hub), the shape is not dead, a fresh subscribe is not refused —
/// and frames have stopped. Nothing in this chassis says so except the absence
/// of beats.
///
/// **A WEDGE, NOT A DEATH, DELIBERATELY.** A panicking fold is already covered
/// (`shapes.rs::a_dead_shape_task_tells_every_mirror_once_and_then_refuses`) and
/// it is the EASY case: a death emits one final export, so the consumer is told.
/// A wedge tells nobody. `WEDGED` is the positive control — a test that asserts
/// silence must prove the mechanism it silenced engaged.
///
/// **THE MUTATION, for the first half.** In `hub.rs`'s `emit_live_row`, make the
/// gate's first-frame rule `None => false` instead of `None => true`. A
/// connection that has never been beaten then never becomes due, because
/// `last_beat` is only written when a frame passes — every beat is withheld,
/// forever, on every connection.
///
/// Forced red:
/// ```text
/// beats flow before the wedge, or this test proves nothing about the wedge: []
/// ```
///
/// **THE SECOND HALF HAS NO PRODUCTION MUTATION THAT FLIPS IT, and that is
/// recorded rather than faked.** "No beat arrives while the fold is wedged" is a
/// claim about the ABSENCE of a second path from an emit to a frame. There is no
/// such path to delete: a `Fact` reaches a connection only by being folded, and
/// nothing in the chassis synthesises a row. The mutation that WOULD red it is
/// the design this test exists to reject — a beat emitted by the delivery layer
/// or by the connection loop — and writing it in order to delete it again would
/// be building the ping with extra steps to prove it is not being built.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wedged_fold_stops_the_beat_while_the_shape_still_looks_alive() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Beater, m.clone());

    let mut rx = hub.connect(102);
    hub.subscribe("beater", Some("link"), 102, None).unwrap();
    settle().await;
    drain(&mut rx);

    // Beats flow.
    for seq in 1..=2u64 {
        m.hub.emit_fact(Fact::Beat { seq });
        tokio::time::sleep(std::time::Duration::from_millis(TEST_BEAT_MS)).await;
    }
    settle().await;
    let before = beats(&drain(&mut rx));
    assert!(
        !before.is_empty(),
        "beats flow before the wedge, or this test proves nothing about the \
         wedge: {before:?}"
    );

    // Wedge the fold, then keep beating into it.
    let was = WEDGED.load(Ordering::SeqCst);
    m.hub.emit(Push::Info {
        domain: "x".into(),
        reason: "wedge".into(),
        account: None,
        detail: None,
        at: "now".into(),
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    for seq in 3..=8u64 {
        m.hub.emit_fact(Fact::Beat { seq });
        tokio::time::sleep(std::time::Duration::from_millis(TEST_BEAT_MS)).await;
    }

    let during = beats(&drain(&mut rx));
    assert_eq!(
        during,
        Vec::<u64>::new(),
        "a wedged fold carries no beat — that is the whole claim, and it is what \
         a ping cannot tell a consumer: {during:?}"
    );
    assert!(
        WEDGED.load(Ordering::SeqCst) > was,
        "the wedge never engaged, so the silence above proves nothing"
    );

    // …and everything else still says the shape is perfectly well.
    assert!(
        hub.is_dead("beater").is_none(),
        "a WEDGE is not a death: no death export, nothing marked dead — which is \
         exactly why the absence of beats is the only signal"
    );
    assert!(
        hub.subscribe("beater", Some("link"), 103, None).is_ok(),
        "a fresh subscribe is accepted, so a consumer checking liveness by \
         subscribing learns nothing"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. The cadence, under the producer's ceiling — N = 2, because that is the
//    question
// ─────────────────────────────────────────────────────────────────────────────

/// **A SLOW ASK COSTS THE PRODUCER NOTHING AND THE OTHER SUBSCRIBER NOTHING.**
///
/// The retain hazard, arriving on the rate axis. Two connections hold the same
/// key of the same shape over the SAME ROOT; one asks for the ceiling (by asking
/// nothing at all, which is rule 2.8's second clause) and the other asks
/// `beat: 500` against a 50 ms producer. Under a design where a subscriber's
/// cadence moved the PRODUCER, the slow asker would slow the fast one down, or
/// the fast one would speed the slow one up. Under this one the producer emits at
/// its own `const` rate regardless and the projection is DECIMATION at delivery.
///
/// So all three are asserted at once, on frames each connection received: the
/// fast subscriber gets every beat, the slow one gets strictly fewer, and the
/// slow one's beats are a SUBSET of the producer's own sequence — it was shown
/// some of the beats that were made, not a renumbered stream of its own.
///
/// **N = 2 deliberately.** Anything that arbitrates between two or more of
/// something is proven at N ≥ 2 or not proven.
///
/// **THE MUTATION.** In `hub.rs`'s `emit_live_row`, delete the gate — replace the
/// whole `let audience = if S::MIN_BEAT_INTERVAL_MS == 0 … ` block with
/// `let audience = self.audience();`, which is the shape that declares a rate
/// ceiling and then serves everybody the ceiling.
///
/// Forced red:
/// ```text
/// a `beat: 500` subscriber is shown strictly fewer beats than the ceiling
/// subscriber: 8 vs 8
/// ```
/// **THE LAG REPAIR DOES NOT SPEED UP THE CONSUMER THAT LAGGED.**
///
/// A connection's frame channel is bounded, and when it drops frames `drive`
/// repairs the gap by re-subscribing every key the connection holds — rule 2.5,
/// the only recovery there is. That repair went through `Hub::subscribe`, whose
/// whole contract is *asking nothing means the ceiling*, so it arrived at
/// `do_subscribe` carrying `Beat::Max` and that function wrote it over the
/// connection's recorded cadence. Deliberately, and correctly, FOR A CONSUMER'S
/// SECOND SUBSCRIBE: the last ask is the current ask. For the chassis's own
/// repair it meant a consumer that had asked to be beaten every 500 ms, and had
/// then fallen behind, was silently moved to the shape's `MIN_BEAT_INTERVAL_MS`
/// — the most expensive delivery the contract allows, handed to the one
/// subscriber that had just proved it could not keep up.
///
/// **This axis bites harder than `retain`'s.** A `retain` that was widened costs
/// bytes per frame; a `beat` that was widened costs FRAMES, and frames are what
/// overflowed the channel in the first place. Nothing refuses and nothing logs,
/// so from the consumer's side the repair for falling behind is more load.
///
/// `Hub::resubscribe_lagged` is the chassis's own spelling and carries the
/// negotiated ask through. This drives exactly the call `drive` makes.
///
/// **HUB-DRIVEN ON PURPOSE**, like every other beat test in this file: a cadence
/// is a property of the interval between two frames, so a test that put a socket
/// and its scheduler between the emit and the assertion would be measuring the
/// weather. `tests/lag_repair.rs` proves the same repair's `retain` half over a
/// real websocket, which is the axis a socket CAN settle. Both assert on frames a
/// CONNECTION RECEIVED.
///
/// **THE MUTATION.** In `shapewire/src/hub.rs`, make `resubscribe_lagged`
/// pass the ceiling instead of "no ask":
/// ```text
/// self.enqueue_subscribe(shape, key, conn, None, detail, Some(Retain::Max), Some(Beat::Max))
/// ```
/// which is what routing the repair through `Hub::subscribe` did.
///
/// Forced red:
/// ```text
/// thread 'the_lag_repair_does_not_speed_up_a_slow_subscribers_beat' panicked:
/// the repair must not widen a negotiated cadence: a `beat: 500` subscriber was
/// shown 12 of 12 beats after the repair, which is the ceiling it did not ask
/// for — and frames are what overflowed its channel
/// ```
#[tokio::test]
async fn the_lag_repair_does_not_speed_up_a_slow_subscribers_beat() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Beater, m.clone());

    let mut slow = hub.connect(131);
    hub.subscribe_ask(
        "beater",
        Some("link"),
        131,
        None,
        threads::shape::Detail::Rows,
        threads::shape::Retain::Max,
        Beat::Every(500),
    )
    .unwrap();
    settle().await;
    drain(&mut slow);

    // ── THE REPAIR, EXACTLY AS `drive` ISSUES IT ─────────────────────────────
    hub.resubscribe_lagged("beater", Some("link"), 131, threads::shape::Detail::Rows)
        .expect("the repair is accepted on a live shape");
    settle().await;
    let repair = drain(&mut slow);
    assert!(
        repair.iter().any(|f| f.wire.op == threads::shape::Op::Read),
        "the repair must have produced its re-export, or there is nothing under \
         test: {:?}",
        repair.iter().map(|f| f.wire.op).collect::<Vec<_>>()
    );

    // Twelve beats at the producer's own 50 ms period — 600 ms, so a 500 ms
    // cadence can admit two or three of them and nothing like twelve.
    let mut made = Vec::new();
    for seq in 1..=12u64 {
        m.hub.emit_fact(Fact::Beat { seq });
        made.push(seq);
        tokio::time::sleep(std::time::Duration::from_millis(TEST_BEAT_MS)).await;
    }
    settle().await;

    let got = beats(&drain(&mut slow));
    assert!(
        got.len() < made.len(),
        "the repair must not widen a negotiated cadence: a `beat: 500` subscriber \
         was shown {} of {} beats after the repair, which is the ceiling it did \
         not ask for — and frames are what overflowed its channel. got: {got:?}",
        got.len(),
        made.len()
    );
    assert!(
        got.iter().all(|s| made.contains(s)),
        "and what it was shown is a SUBSET of the beats the producer made, not a \
         stream renumbered for it: {got:?} vs {made:?}"
    );
    drop(tmp);
}

#[tokio::test]
async fn a_slow_ask_costs_the_producer_nothing_and_the_other_subscriber_nothing() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Beater, m.clone());

    // The pre-parameter caller: it names no beat at all and must keep getting
    // exactly what it got.
    let mut fast = hub.connect(111);
    hub.subscribe("beater", Some("link"), 111, None).unwrap();
    let mut slow = hub.connect(112);
    hub.subscribe_ask(
        "beater",
        Some("link"),
        112,
        None,
        threads::shape::Detail::Rows,
        threads::shape::Retain::Max,
        Beat::Every(500),
    )
    .unwrap();
    settle().await;
    drain(&mut fast);
    drain(&mut slow);

    let mut made = Vec::new();
    for seq in 1..=12u64 {
        m.hub.emit_fact(Fact::Beat { seq });
        made.push(seq);
        tokio::time::sleep(std::time::Duration::from_millis(TEST_BEAT_MS)).await;
    }
    settle().await;

    let fast_got = beats(&drain(&mut fast));
    let slow_got = beats(&drain(&mut slow));

    assert_eq!(
        fast_got, made,
        "asking nothing means the ceiling — a consumer written before `beat` \
         existed keeps getting every beat: {fast_got:?}"
    );
    assert!(
        slow_got.len() < fast_got.len(),
        "a `beat: 500` subscriber is shown strictly fewer beats than the ceiling \
         subscriber: {} vs {}",
        slow_got.len(),
        fast_got.len()
    );
    assert!(
        !slow_got.is_empty(),
        "…and more than none of them, or the gate is a mute and not a cadence: \
         {slow_got:?}"
    );
    assert!(
        slow_got.iter().all(|s| made.contains(s)),
        "the gate does NOT renumber: what a slow subscriber sees is a subset of \
         the beats the producer made, gaps and all — `Beat` §6: a gap is its own \
         ask, not loss. {slow_got:?}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. The ceiling: refused, naming it, never clamped — and pointing the OTHER WAY
// ─────────────────────────────────────────────────────────────────────────────

/// **TOO FAST IS REFUSED NAMING THE FLOOR, AND THE REMEDY SAYS *MORE*.**
///
/// This is the single place a rate ceiling reaches into the code differently from
/// a quantity ceiling. `ShapeError::AboveMax` says *"asked {asked}, the maximum is
/// {max}, ask for {max} or less"*. On this axis the ask that is beyond the
/// contract is the SMALLER number — `beat: 10` against a 50 ms producer — so that
/// sentence is false as arithmetic and its remedy is advice to ask again. Hence a
/// variant of its own.
///
/// Also asserted: the ask is not CLAMPED. A clamped rate would be a second claim
/// about a cadence the contract states outright, and the consumer would have to
/// decide which to believe.
///
/// **THE MUTATION.** In `Hub::subscribe_ask`, clamp instead of refusing —
/// `let beat = Beat::Every(ms.max(slot.max.beat_interval_ms));` and drop the
/// `TooFast` return.
///
/// Forced red:
/// ```text
/// a beat faster than the producer's period is REFUSED, never clamped: Ok(3)
/// ```
#[tokio::test]
async fn a_beat_faster_than_the_producer_is_refused_naming_the_floor_and_says_more() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Beater, m.clone());
    settle().await;

    let asked = hub.subscribe_ask(
        "beater",
        Some("link"),
        121,
        None,
        threads::shape::Detail::Rows,
        threads::shape::Retain::Max,
        Beat::Every(10),
    );
    match asked {
        Err(ShapeError::TooFast {
            shape,
            asked_ms,
            min_ms,
        }) => {
            assert_eq!(shape, "beater");
            assert_eq!(asked_ms, 10);
            assert_eq!(
                min_ms, TEST_BEAT_MS,
                "the refusal names the producer's own period, so the consumer \
                 knows what to ask instead"
            );
            let text = ShapeError::TooFast {
                shape,
                asked_ms,
                min_ms,
            }
            .to_string();
            assert!(
                text.contains("or MORE"),
                "a rate ceiling is a FLOOR on the interval, so the remedy is to \
                 ask for a BIGGER number — `or less` would be advice to ask for \
                 the same failure again: {text}"
            );
        }
        other => panic!(
            "a beat faster than the producer's period is REFUSED, never clamped: \
             {other:?}"
        ),
    }

    // And the ceiling itself is servable: exactly the producer's period is fine.
    assert!(
        hub.subscribe_ask(
            "beater",
            Some("link"),
            122,
            None,
            threads::shape::Detail::Rows,
            threads::shape::Retain::Max,
            Beat::Every(TEST_BEAT_MS),
        )
        .is_ok(),
        "the ceiling is an inclusive bound — asking for exactly it is the ask \
         `max` means"
    );
    drop(tmp);
}

/// **`beat` ON A SHAPE THAT DOES NOT BEAT IS `NoBeat`, NOT `TooFast`.**
///
/// Two different facts with two different repairs, exactly as `NoRetention` and
/// `AboveMax` are on the history axis. *"There is no beat here"* is repaired by
/// dropping the parameter; *"you asked faster than I go"* is repaired by asking
/// for a bigger number. Collapsing them would tell this consumer to "ask for 0ms
/// or more", which is advice to keep asking.
///
/// **THE MUTATION.** In `Hub::subscribe_ask`, delete the
/// `slot.max.beat_interval_ms == 0` arm so the second check answers it —
/// `10 < 0` is false, so the ask is ACCEPTED and then silently honoured by
/// nothing.
///
/// Forced red:
/// ```text
/// a `beat` ask on a shape that does not beat is refused, and it is its own
/// refusal: Ok(4)
/// ```
#[tokio::test]
async fn a_beat_asked_of_a_shape_that_does_not_beat_is_no_beat_and_not_too_fast() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Silent, m.clone());
    settle().await;

    match hub.subscribe_ask(
        "silent",
        Some("a"),
        131,
        None,
        threads::shape::Detail::Rows,
        threads::shape::Retain::Max,
        Beat::Every(10),
    ) {
        Err(ShapeError::NoBeat { shape }) => {
            assert_eq!(shape, "silent");
            let text = ShapeError::NoBeat { shape }.to_string();
            assert!(
                text.contains("omit it"),
                "the repair for `there is no beat here` is to DROP the parameter, \
                 and the refusal has to say so: {text}"
            );
        }
        other => panic!(
            "a `beat` ask on a shape that does not beat is refused, and it is its \
             own refusal: {other:?}"
        ),
    }

    // And a shape that does not beat still serves a subscriber that asked for
    // nothing — rule 2.8's second clause, which is what lets a knob be added
    // without a version bump.
    assert!(
        hub.subscribe("silent", Some("a"), 132, None).is_ok(),
        "every shape written before `beat` existed is unchanged by its existing"
    );
    drop(tmp);
}

/// **THE CEILING A REFUSAL NAMES IS THE CEILING `shapes.list` PUBLISHES.**
///
/// Rule 2.8's fourth clause: a consumer must be able to read the contract it is
/// allowed to use before it uses it. Both numbers come off the same `const`, read
/// once at `register`, so they cannot drift — and this asserts the wire carries
/// it, not that the const exists.
///
/// **THE MUTATION.** In `Hub::register`, hard-code `beat_interval_ms: 0` instead
/// of `S::MIN_BEAT_INTERVAL_MS` — a ceiling that is enforced and not published,
/// which is the drift this clause exists to prevent.
///
/// Forced red:
/// ```text
/// the ceiling rides `shapes.list` so a consumer can read it before it asks:
/// 0 vs 50
/// ```
#[tokio::test]
async fn the_rate_ceiling_rides_shapes_list_beside_every_quantity_ceiling() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Beater, m.clone());
    hub.register(Silent, m.clone());
    settle().await;

    let listed = hub.list();
    let beater = listed.iter().find(|i| i.name == "beater").unwrap();
    let silent = listed.iter().find(|i| i.name == "silent").unwrap();

    assert_eq!(
        beater.max.beat_interval_ms, TEST_BEAT_MS,
        "the ceiling rides `shapes.list` so a consumer can read it before it \
         asks: {} vs {}",
        beater.max.beat_interval_ms, TEST_BEAT_MS
    );
    assert_eq!(
        silent.max.beat_interval_ms, 0,
        "and `0` is the honest statement `this shape does not beat` — the same \
         spelling `retained_items: 0` uses for `no windowed cell`"
    );
    assert_eq!(
        beater.max.retained_items, 0,
        "the two knobs are independent declarations: a shape that beats holds no \
         windowed cell by virtue of beating, and a `retain: n` on it is still \
         `NoRetention`"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. The gate must not eat a promise
// ─────────────────────────────────────────────────────────────────────────────

struct BeatCommand;
/// The stand-in shape: this fixture's `Fact` is what its own fold emits.
struct ABeaterShape;
impl shapewire::command::Resolves<Fact> for ABeaterShape {}
impl shapewire::command::Command for BeatCommand {
    const NAME: &'static str = "beater.poke";
    type Resolves = Fact;
    type ResolvedBy = ABeaterShape;
}

/// **A FRAME CARRYING A PROMISE ID IS NEVER WITHHELD BY THE BEAT GATE.**
///
/// The hazard a rate projection creates and a quantity projection cannot: a
/// cadence gate DROPS frames, and a dropped frame takes its promise ids with it.
/// A consumer asking for a slower beat asked about its BEATS; it did not give the
/// producer permission to withhold the answer to its WRITE, and a promise whose
/// frame is dropped is a consumer waiting forever on an id nothing will ever
/// carry.
///
/// **THIS ASSERTS ARRIVAL, NOT RESOLUTION.** An empty promise book proves that a
/// resolve ran and never that a frame carried the id. The assertion here is on
/// the `promises` array of a frame the slow connection RECEIVED, while its
/// cadence says it should have been skipped.
///
/// **THE MUTATION.** In `emit_live_row`, drop the `!promises.is_empty()` term
/// from the gate's guard — `if S::MIN_BEAT_INTERVAL_MS == 0`.
///
/// Forced red:
/// ```text
/// the frame carrying the promise reached the slow connection: []
/// ```
#[tokio::test]
async fn a_frame_carrying_a_promise_is_never_withheld_by_the_beat_gate() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Beater, m.clone());

    let mut slow = hub.connect(141);
    hub.subscribe_ask(
        "beater",
        Some("link"),
        141,
        None,
        threads::shape::Detail::Rows,
        threads::shape::Retain::Max,
        // Slower than this whole test: nothing may pass the gate on cadence
        // alone after the first frame.
        Beat::Every(60_000),
    )
    .unwrap();
    settle().await;
    drain(&mut slow);

    // Burn the gate's first-frame allowance, which is always due.
    m.hub.emit_fact(Fact::Beat { seq: 1 });
    settle().await;
    drain(&mut slow);

    // A beat with a promise on it. Nothing about its cadence changed.
    let promise = m.promises.open::<BeatCommand>(Some("link".to_string()));
    let id = promise.id();
    let resolving = m.promises.resolve::<Fact>(Some("link"));
    m.hub
        .emit_fact_resolving(resolving, Fact::Beat { seq: 2 });
    settle().await;

    let carried: Vec<u64> = drain(&mut slow)
        .iter()
        .flat_map(|f| f.wire.promises.clone())
        .collect();
    assert!(
        carried.contains(&id.0),
        "the frame carrying the promise reached the slow connection: {carried:?} \
         (a promise is resolved by the arrival of its fact at the consumer \
         holding the subject — a cadence is not permission to withhold it)"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. The production wiring, and the question the beat does NOT answer
// ─────────────────────────────────────────────────────────────────────────────

/// **THE REGISTERED `heartbeat` SHAPE BEATS WITH NOBODY EMITTING.**
///
/// Every test above drives the beat by hand, which proves the mechanism and not
/// the wiring. This one subscribes to the real shape on a real manager with its
/// real watchers started, emits nothing at all, and waits — so what arrives came
/// from the beat task in `runtime.rs`, through the FIFO, through
/// `shapes::heartbeat`'s own fold.
///
/// **THE MUTATION.** In `runtime.rs`'s `start_watchers`, delete the
/// `hub.register(crate::shapes::heartbeat::Heartbeat, …)` line — a beat task with
/// no shape to beat, which is the half of the pairing the chassis cannot check.
///
/// Forced red:
/// ```text
/// the registered heartbeat shape beats on its own: Err(Unknown("heartbeat"))
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_registered_heartbeat_shape_beats_with_nobody_emitting() {
    let (m, tmp) = start().await;
    m.start_watchers();
    settle().await;

    let mut rx = m.hub.connect(151);
    m.hub
        .subscribe("heartbeat", Some("link"), 151, None)
        .expect("the registered heartbeat shape beats on its own");
    settle().await;
    drain(&mut rx);

    // Two producer periods plus slack. Nothing here emits anything.
    tokio::time::sleep(std::time::Duration::from_millis(
        threads::shapes::heartbeat::BEAT_INTERVAL_MS * 2 + 600,
    ))
    .await;

    let got = beats(&drain(&mut rx));
    assert!(
        got.len() >= 2,
        "the beat task drives the shape with no producer input at all, and two \
         periods is two beats: {got:?}"
    );
    assert!(
        got.windows(2).all(|w| w[1] > w[0]),
        "and the seq is monotone, which is what stops the cell deduping itself \
         into silence: {got:?}"
    );
    drop(tmp);
}

/// **A THREAD'S LIVENESS BEAT IS NOT A LINK BEAT, AND NEITHER ANSWERS THE OTHER.**
///
/// The conflation this shape's module doc exists to prevent, made mechanical.
/// `Push::Liveness` is a level on `crate::shapes::threads` about a THREAD BEING
/// BUSY; `Fact::Beat` is about the LINK carrying frames. A reader who sees the
/// word "heartbeat" reaches for the wrong one, and a fold that accepted both
/// would make an idle thread look busy and a wedged link look fine.
///
/// **THE MUTATION.** In `lib/threads/src/shapes/heartbeat.rs`, move
/// `Input::Push(Push::Liveness { .. })` out of the ignored arm and into the beat
/// arm — the conflation, written down.
///
/// Forced red:
/// ```text
/// a thread's liveness beat is not a link beat: [7]
/// ```
#[tokio::test]
async fn a_thread_liveness_beat_is_not_a_link_beat() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(threads::shapes::heartbeat::Heartbeat, m.clone());

    let mut rx = hub.connect(161);
    hub.subscribe("heartbeat", Some("link"), 161, None).unwrap();
    settle().await;
    drain(&mut rx);

    for _ in 0..3 {
        m.hub.emit(Push::Liveness {
            thread_id: "t-1".into(),
            busy: true,
            parent_running: true,
            running_subagents: Vec::new(),
        });
    }
    settle().await;

    let got = beats(&drain(&mut rx));
    assert_eq!(
        got,
        Vec::<u64>::new(),
        "a thread's liveness beat is not a link beat: {got:?}"
    );
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
