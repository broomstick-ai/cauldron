//! # Subscriber-declared retention, under a producer ceiling — rule 2.8's fourth knob
//!
//! The operator: *"these sequence shapes, if they are lazy, can have a parameter
//! for how many items the subscriber wants the producer to hold for them. Like
//! it could be ALL, COUNT."* Then, on the hazard of an unbounded `ALL`: *"we can
//! set a max size on the shape on the producer side. so instead of ALL it would
//! be MAX."*
//!
//! **The hazard these tests exist to settle** is who pays when two subscribers
//! ask for different amounts. The answer built here is that the CEILING BOUNDS
//! TOTAL EXPOSURE and retention is not per-subscriber at all — a shape has one
//! root shared by every subscriber, so `retain` is a projection at delivery and
//! not a request to hold. `a_small_ask_costs_the_producer_nothing_and_the_other_
//! subscriber_nothing` is the test that can tell the two designs apart: under
//! per-subscriber retention the large asker's window would be what the small
//! asker's frame is cut from, and under this one the cell is untouched.
//!
//! **Every test here was forced red by the mutation named in its own docstring,
//! and the red was checked for `error[E` before it was believed.**
//!
//! Hub-driven, not socket-driven: a retain is a claim about a FRAME, the hub is
//! where frames are made, and a test that goes through a socket and a timer can
//! pass or fail on scheduling.
//!
//! **Every assertion is on a frame a CONNECTION RECEIVED**, never on hub-internal
//! state. That is this repo's standing trap in its general form: proving that a
//! mechanism ran is not proving that anything reached a consumer.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use threads::config::Config;
use threads::event::Push;
use threads::shape::{
    ConnFrames, Derived, Hub, Input, Outbox, Retain, Root, Sequence, Shape, ShapeError,
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
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "first".to_string(),
        tmp.join("first").to_string_lossy().to_string(),
    );
    Config {
        data_dir: Some(tmp.to_string_lossy().to_string()),
        accounts,
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
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
}

fn drain(f: &mut ConnFrames) -> Vec<Arc<ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

fn a_line(s: &str) -> Push {
    Push::Info {
        domain: "test".into(),
        reason: s.to_string(),
        account: None,
        detail: None,
        at: chrono::Utc::now().to_rfc3339(),
    }
}

/// The `lines` array off the EXPORT a connection received — the frame, never the
/// cell.
fn exported_lines(frames: &[Arc<ShapeFrame>]) -> Vec<String> {
    let f = frames
        .iter()
        .find(|f| f.wire.row.is_some())
        .expect("an export carrying a row");
    lines_of(f.wire.row.as_ref().unwrap())
}

/// The `lines` array off a LIVE `row` derived event a connection received.
fn live_lines(frames: &[Arc<ShapeFrame>]) -> Vec<String> {
    let f = frames
        .iter()
        .find_map(|f| match &f.wire.event {
            Some(Derived::Row { row }) => Some(row.clone()),
            _ => None,
        })
        .expect("a live row frame");
    lines_of(&f)
}

fn lines_of(row: &Value) -> Vec<String> {
    row["lines"]
        .as_array()
        .expect("`lines` is an array")
        .iter()
        .map(|v| v["line"].as_str().unwrap().to_string())
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// test shapes
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq)]
struct Line {
    line: String,
}

struct Chan {
    lines: Sequence<Line>,
}

impl Default for Chan {
    fn default() -> Self {
        Self {
            lines: Sequence::new(CAPACITY, chrono::Duration::hours(1)),
        }
    }
}

/// The cell's own bound, and the shape's declared ceiling. Small enough that
/// eviction and a short ask are both reachable in a test.
const CAPACITY: usize = 4;

#[derive(Default)]
struct LogRoot {
    chans: threads::shape::Map<String, Chan>,
}

impl Root for LogRoot {
    fn keys(&self) -> Vec<String> {
        self.chans.keys().cloned().collect()
    }
    fn row(&self, k: &str) -> Option<Value> {
        self.row_at(k, Retain::Max)
    }
    /// The override `MAX_RETAINED_ITEMS` obliges. A shape that declares the
    /// ceiling and skips this accepts an ask it then ignores.
    fn row_at(&self, k: &str, retain: Retain) -> Option<Value> {
        self.chans
            .get(&k.to_string())
            .map(|c| json!({"chan": k, "lines": c.lines.to_json_retained(retain)}))
    }
    fn cells(&self, k: &str) -> Option<Value> {
        self.chans
            .get(&k.to_string())
            .map(|c| json!({"lines": c.lines.state()}))
    }
    fn shared(&self) -> Value {
        json!({})
    }
    fn shared_cells(&self) -> Value {
        json!({})
    }
    fn drain(&mut self) -> Outbox {
        let dirty = self.chans.take_dirty_keys();
        let mut rows = Vec::new();
        let mut transitions = Vec::new();
        for k in dirty {
            let Some(c) = self.chans.get_mut(&k) else {
                continue;
            };
            for item in c.lines.take_pending() {
                transitions.push((k.clone(), "lines", item));
            }
            rows.push(k);
        }
        Outbox {
            rows,
            gone: self.chans.take_removed(),
            added: self.chans.take_added(),
            transitions,
            shared: false,
        }
    }
    fn mark_dead(&mut self, _: &str) {}
}

/// **ONE CHANNEL, A FOUR-ITEM WINDOW, AND A DECLARED CEILING OF FOUR.**
struct Logged;

// The cells this file's test roots emit, declared — `Shape::CELLS` is required
// (see `shapewire::cell`), and a test shape declares what its `Root::cells`
// actually spells.
shapewire::declare_cells! {
    LINES_CELLS = [("lines", shapewire::operation::DType::Scalar(shapewire::operation::Scalar::Int), shapewire::operation::Presence::Required, [])]
}

// The cells this file's test roots emit, declared — `Shape::CELLS` is required
// (see `shapewire::cell`), and a test shape declares what its `Root::cells`
// actually spells.
shapewire::declare_cells! {
    N_CELLS = [("n", shapewire::operation::DType::Scalar(shapewire::operation::Scalar::Int), shapewire::operation::Presence::Required, [])]
}

impl Shape<Input> for Logged {
    const NAME: &'static str = "logged";
    const CELLS: Cells = LINES_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const MAX_RETAINED_ITEMS: usize = CAPACITY;
    type Producer = ThreadManager;
    type Root = LogRoot;
    fn root(&self) -> LogRoot {
        LogRoot::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut LogRoot, as_of: Timestamp) {
        root.chans.key("log".to_string()).lines.mark_since_init(as_of);
        root.chans.mark_read(as_of, true);
        let _ = root.drain();
    }
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, LogRoot>) {
        let at = tx.at();
        let Input::Push(Push::Info { reason, .. }) = &input.value else {
            return;
        };
        tx.chans.key("log".to_string()).lines.emit(
            at,
            Line {
                line: reason.clone(),
            },
        );
    }
}

/// **A SHAPE WITH NO WINDOWED CELL AT ALL** — it declares nothing, so it keeps
/// the chassis default of `MAX_RETAINED_ITEMS = 0`, which is what every shape
/// written before `retain` existed has.
#[derive(Default)]
struct FlatRoot {
    rows: threads::shape::Map<String, threads::shape::Register<u64>>,
}

impl Root for FlatRoot {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }
    fn row(&self, k: &str) -> Option<Value> {
        self.rows.get(&k.to_string()).map(|n| json!({"n": n.now()}))
    }
    fn cells(&self, k: &str) -> Option<Value> {
        self.rows.get(&k.to_string()).map(|n| json!({"n": n.state()}))
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

struct Flat;
impl Shape<Input> for Flat {
    const NAME: &'static str = "flat";
    const CELLS: Cells = N_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    type Producer = ThreadManager;
    type Root = FlatRoot;
    fn root(&self) -> FlatRoot {
        FlatRoot::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut FlatRoot, as_of: Timestamp) {
        root.rows.key("a".to_string()).read(as_of, 1);
        root.rows.mark_read(as_of, true);
        let _ = root.drain();
    }
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, FlatRoot>) {}
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. MAX is what is retained, and asking nothing means MAX
// ─────────────────────────────────────────────────────────────────────────────

/// **`MAX` IS EVERYTHING CURRENTLY RETAINED, NEVER EVERYTHING THAT HAPPENED.**
///
/// Six lines are emitted into a four-item window. `Retain::Max` answers FOUR —
/// the four the producer still holds — and the two that were evicted are gone
/// from the world, not merely unasked-for. That is the whole of why the name is
/// `MAX` and not `ALL`: an `ALL` that meant "everything that ever happened"
/// would be a durable read with no bound, which is the class
/// `list_threads(100_000)` already was here.
///
/// A subscriber that asks for NOTHING gets the same four — rule 2.8's second
/// clause, which is what lets a knob be added without a version bump.
///
/// **THE MUTATION.** In `fold.rs`, delete `Sequence::evict`'s
/// `while self.items.len() > self.capacity { self.items.pop_front(); }` — the
/// "MAX means everything" reading made real, which is the unbounded `ALL` the
/// operator replaced.
///
/// Forced red:
/// ```text
/// a four-item window holds FOUR after six emits, and `max` is that and not the
/// six: ["l0", "l1", "l2", "l3", "l4", "l5"]
/// ```
#[tokio::test]
async fn max_is_everything_currently_retained_and_asking_nothing_means_max() {
    // **ON THE CLOCK SEAM, for the same reason as
    // `recent_updates::the_order_is_bounded_by_the_capacity_the_shape_declares`:**
    // `Hub::emit` stamps with the clock and the cell's stamp rule refuses an
    // input at or before its `folded_at`, so six emits in a tight loop under an
    // optimized build collide and some are dropped. The window then held
    // `["l1", "l2", "l4", "l5"]` — not the newest four, not even four
    // CONSECUTIVE ones, which is the tell that this was never about capacity.
    // The seam is installed by `start()`, for every test in this file, and it
    // gives every emit a distinct stamp. See `src/clock.rs`.
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Logged, m.clone());

    let mut asked = hub.connect(81);
    hub.subscribe_ask(
        "logged",
        Some("log"),
        81,
        None,
        threads::shape::Detail::Rows,
        Retain::Max,
        threads::shape::Beat::Max,
    )
    .unwrap();
    settle().await;
    drain(&mut asked);

    // A second connection that names NO retain at all — the pre-parameter
    // caller, which must keep getting exactly what it got.
    for i in 0..6 {
        m.hub.emit(a_line(&format!("l{i}")));
    }
    settle().await;
    let mut silent = hub.connect(82);
    hub.subscribe("logged", Some("log"), 82, None).unwrap();
    settle().await;

    let explicit_max = live_lines(&drain(&mut asked));
    assert_eq!(
        explicit_max,
        vec!["l2", "l3", "l4", "l5"],
        "a four-item window holds FOUR after six emits, and `max` is that and not \
         the six: {explicit_max:?}"
    );
    let no_ask = exported_lines(&drain(&mut silent));
    assert_eq!(
        no_ask, explicit_max,
        "asking nothing means the ceiling — a consumer written before `retain` \
         existed keeps getting exactly what it got: {no_ask:?}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. COUNT(n), and who pays for a large ask — N = 2, because that is the
//    question
// ─────────────────────────────────────────────────────────────────────────────

/// **A SMALL ASK COSTS THE PRODUCER NOTHING AND THE OTHER SUBSCRIBER NOTHING.**
///
/// This is the test the hazard came down to. Two connections hold the same key
/// of the same shape over the SAME ROOT; one asks `retain: 2`, the other asks
/// for everything. Under PER-SUBSCRIBER RETENTION the producer would hold the
/// larger window and the small asker's answer would be cut from it — which is
/// observably the same answer, so the thing that distinguishes the designs is
/// what the OTHER subscriber sees and what the cell still holds afterwards.
///
/// So all three are asserted at once, on frames each connection actually
/// received: the narrow subscriber gets the newest two, the wide one gets all
/// four, and neither ask moved the cell.
///
/// **N = 2 deliberately.** Anything that arbitrates between two or more of
/// something is proven at N ≥ 2 or not proven, and a single-subscriber test
/// cannot exercise the contention this parameter exists to be safe under.
///
/// **TWO MUTATIONS, both forced, both this same red.** (a) In
/// `ShapeTask::export`, collapse the per-retain grouping back to one frame —
/// `let targets = vec![(Retain::Max, self.audience())]` — the "one body, many
/// connections" version the broadcast path had before. (b) In `fold.rs`, make
/// `Sequence::to_json_retained` ignore its argument (`let skip = 0;`) — the
/// projection as a no-op, which is the shape that declares a ceiling and then
/// serves everybody the ceiling.
///
/// Forced red (both):
/// ```text
/// `retain: 2` is the NEWEST two of what is retained: ["l0", "l1", "l2", "l3"]
/// ```
#[tokio::test]
async fn a_small_ask_costs_the_producer_nothing_and_the_other_subscriber_nothing() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Logged, m.clone());

    let mut wide = hub.connect(83);
    hub.subscribe("logged", Some("log"), 83, None).unwrap();
    settle().await;
    for i in 0..4 {
        m.hub.emit(a_line(&format!("l{i}")));
    }
    settle().await;
    drain(&mut wide);

    let mut narrow = hub.connect(84);
    hub.subscribe_ask(
        "logged",
        Some("log"),
        84,
        None,
        threads::shape::Detail::Rows,
        Retain::Count(2),
        threads::shape::Beat::Max,
    )
    .unwrap();
    settle().await;

    let got = exported_lines(&drain(&mut narrow));
    assert_eq!(
        got,
        vec!["l2", "l3"],
        "`retain: 2` is the NEWEST two of what is retained: {got:?}"
    );

    // ── AND THE CELL DID NOT MOVE ────────────────────────────────────────
    //
    // One more line reaches BOTH connections. The wide one must still see four,
    // which is only true if the narrow subscriber's ask evicted nothing — the
    // whole of the who-pays answer, asserted at the consumer that would have
    // paid.
    m.hub.emit(a_line("l4"));
    settle().await;
    let wide_got = live_lines(&drain(&mut wide));
    assert_eq!(
        wide_got,
        vec!["l1", "l2", "l3", "l4"],
        "a narrow subscriber's ask must not shorten anybody else's window — it is \
         a projection at delivery, not a request to hold: {wide_got:?}"
    );
    drop(tmp);
}

/// **THE RETAIN BOUND HOLDS ON THE LIVE `row` FRAME, NOT ONLY ON THE EXPORT.**
///
/// A knob that held only on the first frame would be the stub that pretends to
/// work: the consumer asks for two, receives two, and is handed four on the next
/// thing that happens. One emit, two connections with different asks, two
/// different bodies — measured on the LIVE frames each received.
///
/// **THE MUTATION.** In `ShapeTask::flush_rows`, call the old
/// `emit_live_with(Some(k), at, Derived::Row { row }, ids)` with a row built from
/// `root.row(&k)` instead of `emit_live_row` — one body for the whole audience,
/// which is what the path did before.
///
/// Forced red:
/// ```text
/// a live `row` frame is bounded by the retain its receiver declared: ["l0", "l1", "l2"]
/// ```
#[tokio::test]
async fn the_retain_bound_holds_on_the_live_row_frame_too() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Logged, m.clone());

    let mut wide = hub.connect(85);
    hub.subscribe("logged", Some("log"), 85, None).unwrap();
    let mut narrow = hub.connect(86);
    hub.subscribe_ask(
        "logged",
        Some("log"),
        86,
        None,
        threads::shape::Detail::Rows,
        Retain::Count(1),
        threads::shape::Beat::Max,
    )
    .unwrap();
    settle().await;
    drain(&mut wide);
    drain(&mut narrow);

    for i in 0..3 {
        m.hub.emit(a_line(&format!("l{i}")));
    }
    settle().await;

    let narrow_got = live_lines(&drain(&mut narrow));
    assert_eq!(
        narrow_got,
        vec!["l2"],
        "a live `row` frame is bounded by the retain its receiver declared: \
         {narrow_got:?}"
    );
    let wide_got = live_lines(&drain(&mut wide));
    assert_eq!(
        wide_got,
        vec!["l0", "l1", "l2"],
        "and the connection that asked for nothing still receives the ceiling from \
         the SAME emit: {wide_got:?}"
    );
    drop(tmp);
}

/// **A CONNECTION'S RETAIN IS ITS CURRENT ASK, AND A LATER SUBSCRIBE REPLACES
/// IT — WIDENING INCLUDED.**
///
/// `retain` is per CONNECTION per SHAPE, so a second subscribe from the same
/// connection is that connection changing its mind. The direction that is easy
/// to get wrong is WIDENING: an insert guarded by `if retain != Max` looks
/// harmless — "nothing asked means the ceiling, so there is nothing to record" —
/// and silently keeps a narrower bound from an earlier ask applied to frames
/// nobody asked to shorten. Absence and `Max` are the same frame only when the
/// map has no stale entry to fall back to.
///
/// **THIS TEST WAS ADDED AFTER A MUTATION CAME BACK GREEN.** The suite had no
/// case where one connection subscribed twice, so
/// `if s.retain != Retain::Max { self.retains.insert(..) }` passed everything.
///
/// **THE MUTATION.** In `ShapeTask::do_subscribe`, guard the insert:
/// `if s.retain != Retain::Max { self.retains.insert(s.conn, s.retain); }`.
///
/// Forced red:
/// ```text
/// a later subscribe replaces the retain, WIDENING included — absence and `max`
/// are the same ask only if no stale entry survives: ["l3"]
/// ```
#[tokio::test]
async fn a_later_subscribe_replaces_this_connections_retain_widening_included() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Logged, m.clone());

    let mut c = hub.connect(90);
    hub.subscribe_ask(
        "logged",
        Some("log"),
        90,
        None,
        threads::shape::Detail::Rows,
        Retain::Count(1),
        threads::shape::Beat::Max,
    )
    .unwrap();
    settle().await;
    for i in 0..4 {
        m.hub.emit(a_line(&format!("l{i}")));
    }
    settle().await;
    let narrow = live_lines(&drain(&mut c));
    assert_eq!(narrow, vec!["l3"], "the first ask is honoured: {narrow:?}");

    // The SAME connection asks again, naming nothing — which means the ceiling.
    hub.subscribe("logged", Some("log"), 90, None).unwrap();
    settle().await;
    let widened = exported_lines(&drain(&mut c));
    assert_eq!(
        widened,
        vec!["l0", "l1", "l2", "l3"],
        "a later subscribe replaces the retain, WIDENING included — absence and \
         `max` are the same ask only if no stale entry survives: {widened:?}"
    );

    // And it holds for the LIVE frames after it, not only for that one export.
    m.hub.emit(a_line("l4"));
    settle().await;
    let after = live_lines(&drain(&mut c));
    assert_eq!(
        after,
        vec!["l1", "l2", "l3", "l4"],
        "the replacement is the connection's standing ask, not a one-off: \
         {after:?}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. The refusals — named, never clamped, and not one refusal wearing two
//    meanings
// ─────────────────────────────────────────────────────────────────────────────

/// **AN ASK ABOVE THE CEILING IS REFUSED NAMING IT, AND IS NEVER CLAMPED.**
///
/// Rule 2.8's third clause. A clamped answer is a second claim about a number
/// the frame states outright, and the consumer must then decide which to
/// believe; a refusal that names the ceiling tells it exactly what to ask
/// instead. The refusal is taken at `Hub::subscribe_ask`, where the answer can
/// still reach the caller, rather than inside the task where it would be an ack
/// followed by a surprise.
///
/// **THE MUTATION.** In `Hub::subscribe_ask`, replace the `AboveMax` return with
/// a clamp — `let retain = Retain::Count(n.min(slot.max.retained_items));` — the
/// "be helpful" version rule 2.8 forbids by name.
///
/// Forced red:
/// ```text
/// an ask above the ceiling is REFUSED, never clamped: Ok(1)
/// ```
#[tokio::test]
async fn an_ask_above_the_ceiling_is_refused_naming_it_and_never_clamped() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Logged, m.clone());
    let _c = hub.connect(87);

    let got = hub.subscribe_ask(
        "logged",
        Some("log"),
        87,
        None,
        threads::shape::Detail::Rows,
        Retain::Count(CAPACITY + 1),
        threads::shape::Beat::Max,
    );
    match &got {
        Err(ShapeError::AboveMax {
            param, asked, max, ..
        }) => {
            assert_eq!(param, "retain");
            assert_eq!(asked, "5");
            assert_eq!(
                max, "4",
                "the refusal NAMES the ceiling — one that does not tells a consumer \
                 it asked wrongly and not what to ask instead"
            );
        }
        other => panic!("an ask above the ceiling is REFUSED, never clamped: {other:?}"),
    }
    // And the ceiling the refusal names is the one `shapes.list` publishes, so a
    // consumer can read the contract before it uses it rather than by being
    // refused once.
    let published = hub
        .list()
        .into_iter()
        .find(|i| i.name == "logged")
        .expect("the shape")
        .max
        .retained_items;
    assert_eq!(
        published, CAPACITY,
        "the ceiling on `shapes.list` and the ceiling in the refusal are the same \
         const read once at `register`: {published}"
    );
    drop(tmp);
}

/// **`retain` ON A SHAPE WITH NO WINDOWED CELL IS ITS OWN REFUSAL.**
///
/// *"There is no history here"* and *"you asked for more history than I serve"*
/// are two different facts with two different repairs — drop the parameter, or
/// ask for less — and one spelling for two claims is the collapsed-event class
/// every repair in this chassis is against. `AboveMax` with a ceiling of zero
/// would formally be true and would tell a consumer to "ask for 0 or less",
/// which is advice to do a thing that is not the fix.
///
/// **THE MUTATION.** In `Hub::subscribe_ask`, delete the `retained_items == 0`
/// branch so the zero ceiling falls through to `AboveMax` — the collapse.
///
/// Forced red:
/// ```text
/// a shape with no windowed cell refuses `retain` as NoRetention and not as an
/// ask that was too large: AboveMax { shape: "flat", param: "retain", asked: "1", max: "0" }
/// ```
#[tokio::test]
async fn a_retain_ask_on_a_shape_with_no_windowed_cell_is_its_own_refusal() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Flat, m.clone());
    let _c = hub.connect(88);

    let got = hub.subscribe_ask(
        "flat",
        Some("a"),
        88,
        None,
        threads::shape::Detail::Rows,
        Retain::Count(1),
        threads::shape::Beat::Max,
    );
    match &got {
        Err(ShapeError::NoRetention { shape }) => assert_eq!(shape, "flat"),
        other => panic!(
            "a shape with no windowed cell refuses `retain` as NoRetention and not as \
             an ask that was too large: {other:?}"
        ),
    }
    let text = got.unwrap_err().to_string();
    assert!(
        text.contains("omit it"),
        "and the prose carries the REPAIR, which is to drop the parameter: {text}"
    );

    // **AND `max` IS STILL SERVED ON THAT SHAPE.** A consumer that asks for
    // nothing, or says `max` out loud, is not asking for a windowed cell — it is
    // asking for the ceiling, and the ceiling of nothing is nothing.
    let mut c = hub.connect(89);
    hub.subscribe_ask(
        "flat",
        Some("a"),
        89,
        None,
        threads::shape::Detail::Rows,
        Retain::Max,
        threads::shape::Beat::Max,
    )
    .expect("`max` on a shape with no windowed cell is not a refusal");
    settle().await;
    let frames = drain(&mut c);
    assert!(
        frames.iter().any(|f| f.wire.row.is_some()),
        "and it is answered with the row it always was: {frames:?}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. The wire spelling
// ─────────────────────────────────────────────────────────────────────────────

/// **ABSENT AND `"max"` ARE THE SAME ASK; AN UNKNOWN SPELLING IS NOT AN ASK.**
///
/// The parse is where "asking nothing means the ceiling" is implemented, and
/// where a consumer that asked for something this producer does not serve is
/// told rather than quietly given the ceiling. `retain: 0` is a REAL ask — *the
/// row without its log* — and must not be read as absence.
///
/// **THE MUTATION.** In `wire::Retain::parse`, make the fallthrough
/// `Some(_) => Some(Retain::Max)` — the "be lenient" version, which turns a
/// typo into a silent full window.
///
/// Forced red:
/// ```text
/// an unknown `retain` spelling is refused, not defaulted: Some(Max)
/// ```
#[test]
fn the_retain_spelling_defaults_to_the_ceiling_and_refuses_what_it_does_not_know() {
    assert_eq!(Retain::parse(None), Some(Retain::Max));
    assert_eq!(Retain::parse(Some(&json!("max"))), Some(Retain::Max));
    assert_eq!(Retain::parse(Some(&json!(3))), Some(Retain::Count(3)));
    assert_eq!(
        Retain::parse(Some(&json!(0))),
        Some(Retain::Count(0)),
        "`retain: 0` is the real ask `the row without its log`, not an absence"
    );
    assert_eq!(
        Retain::parse(Some(&json!("all"))),
        None,
        "an unknown `retain` spelling is refused, not defaulted: {:?}",
        Retain::parse(Some(&json!("all")))
    );
    assert_eq!(
        Retain::parse(Some(&json!(-1))),
        None,
        "a negative count is not an ask"
    );

    // `take` is a floor and not a clamp: the ASK was already checked against the
    // declared ceiling, and a window holding fewer items than the ask is the
    // world being small.
    assert_eq!(Retain::Max.take(4), 4);
    assert_eq!(Retain::Count(2).take(4), 2);
    assert_eq!(Retain::Count(9).take(4), 4);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
