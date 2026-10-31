//! # `Hydrate` and nesting, proved
//!
//! Two chassis additions, one file, because the second is built on the first: a
//! nested shape's key space is `(parent_key, own_key)` and cannot be hydrated
//! whole, which is exactly what `Hydrate::PerKey` exists to express.
//!
//! Every test here was forced red by a named mutation before it was believed, and
//! the mutation is in the test's own docstring. The two things this file is most
//! careful about:
//!
//! 1. **The DEFAULTS.** Both additions are consts with defaults, and the whole
//!    claim that "nothing regresses" rests on a shape that declares neither
//!    behaving exactly as it did. So a shape that declares neither is tested, not
//!    assumed.
//! 2. **The EPOCH RULING**, which is the decision most likely to be wrong later
//!    (`shapewire::wire::Epoch::parent_epoch` carries the argument). It is
//!    asserted as behaviour — a parent re-init does not make a child re-init, and
//!    a consumer can still tell the two eras apart — rather than as an
//!    implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Value};
use threads::config::Config;
use threads::event::Push;
use threads::shape::{
    CellState, ConnFrames, Hub, Hydrate, Input, Op, Outbox, Root, Shape, ShapeError, Stamped,
    Timestamp, Tracking, Tx,
};
use threads::{build_manager, ThreadManager};
use shapewire::cell::Cells;

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

async fn start() -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "first".to_string(),
        tmp.path().join("first").to_string_lossy().to_string(),
    );
    accounts.insert(
        "second".to_string(),
        tmp.path().join("second").to_string_lossy().to_string(),
    );
    // THE CLOCK SEAM. Every `Hub::emit` stamps with the clock and every cell
    // refuses an input at or before its `folded_at`, so two emits in one
    // microsecond mean the second is DROPPED — which is what an optimized
    // build does to a tight emit loop. See `src/clock.rs`.
    threads::clock::testing::ticking();
    let manager = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();
    (manager, tmp)
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
}

/// Every frame buffered for this connection, split by `op`.
fn drain(f: &mut ConnFrames) -> (Vec<Arc<threads::shape::ShapeFrame>>, usize, usize) {
    let mut all = Vec::new();
    let (mut reads, mut lives) = (0, 0);
    while let Ok(x) = f.try_recv() {
        match x.wire.op {
            Op::Read => reads += 1,
            Op::Live => lives += 1,
        }
        all.push(x);
    }
    (all, reads, lives)
}

// ─────────────────────────────────────────────────────────────────────────────
// test shapes — ONE root type, many declarations
// ─────────────────────────────────────────────────────────────────────────────

/// A keyed root that holds whatever keys somebody put in it. Deliberately not
/// built on the `Map` cell: what is under test is the chassis's key handling, and
/// a root whose key set a test can set directly is the shortest way to drive it.
#[derive(Default)]
struct Keys {
    keys: BTreeSet<String>,
    dirty: BTreeSet<String>,
}

impl Root for Keys {
    fn keys(&self) -> Vec<String> {
        self.keys.iter().cloned().collect()
    }
    fn row(&self, k: &str) -> Option<Value> {
        self.keys.contains(k).then(|| json!({ "key": k }))
    }
    fn cells(&self, k: &str) -> Option<Value> {
        self.keys.contains(k).then(|| {
            json!({ "key": CellState::Known {
                complete: true,
                folded_at: chrono::Utc::now(),
                since_init: false,
                covers_since: None,
            }})
        })
    }
    fn shared(&self) -> Value {
        json!({})
    }
    fn shared_cells(&self) -> Value {
        json!({})
    }
    fn drain(&mut self) -> Outbox {
        Outbox {
            rows: std::mem::take(&mut self.dirty).into_iter().collect(),
            ..Default::default()
        }
    }
    fn mark_dead(&mut self, _: &str) {}
}

/// **A SHAPE THAT DECLARES NEITHER ADDITION.** The control: no `HYDRATE`, no
/// `PARENT`, no `hydrates_key`. Whatever this one does is what every shape
/// written before these consts existed must still do.
struct Plain;

// The cells this file's test roots emit, declared — `Shape::CELLS` is required
// (see `shapewire::cell`), and a test shape declares what its `Root::cells`
// actually spells.
shapewire::declare_cells! {
    KEY_CELLS = [("key", shapewire::operation::DType::Scalar(shapewire::operation::Scalar::Int), shapewire::operation::Presence::Required, [])]
}

impl Shape<Input> for Plain {
    const NAME: &'static str = "plain";
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    type Producer = ThreadManager;
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut Keys, _: Timestamp) {
        root.keys.insert("aa".into());
        root.keys.insert("bb".into());
        let _ = root.drain();
    }
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, Keys>) {}
}

/// **`Whole`, DECLARED OUT LOUD** — the shape `threads/summary` became when
/// `KeyScope::Active` was deleted (2026-09-28). It exists so that "an explicit
/// `Whole` behaves exactly like declaring nothing" is a test and not a claim:
/// the one way to get the collapse wrong is to treat "declared something" as
/// "not the default" and route it down the refusal path.
struct Explicit;
impl Shape<Input> for Explicit {
    const NAME: &'static str = "explicit";
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const HYDRATE: Hydrate = Hydrate::Whole;
    type Producer = ThreadManager;
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut Keys, _: Timestamp) {
        root.keys.insert("live-1".into());
        root.keys.insert("live-2".into());
        let _ = root.drain();
    }
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, Keys>) {}
}

/// `PerKey` with a real `hydrates_key`: it holds NOTHING at init and mints any
/// key whose name starts with `ok-`.
struct OnDemandShape;
impl Shape<Input> for OnDemandShape {
    const NAME: &'static str = "ondemand";
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const HYDRATE: Hydrate = Hydrate::PerKey;
    type Producer = ThreadManager;
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, _: &mut Keys, _: Timestamp) {}
    fn hydrates_key(
        &mut self,
        _: &ThreadManager,
        root: &mut Keys,
        key: &str,
        _: Timestamp,
    ) -> bool {
        if !key.starts_with("ok-") {
            return false;
        }
        root.keys.insert(key.to_string());
        let _ = root.drain(); // a hydrate publishes nothing
        true
    }
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, Keys>) {}
}

/// `PerKey` that does NOT override `hydrates_key` — so the chassis default
/// refuses, which is what keeps the trait method additive.
struct Refuser;
impl Shape<Input> for Refuser {
    const NAME: &'static str = "refuser";
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const HYDRATE: Hydrate = Hydrate::PerKey;
    type Producer = ThreadManager;
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, _: &mut Keys, _: Timestamp) {}
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, Keys>) {}
}

/// The PARENT of the nesting tests. Its fold dirties its keys on every input, so
/// a burst is enough to overflow a small FIFO and force a re-init.
struct Parent;
impl Shape<Input> for Parent {
    const NAME: &'static str = "tree";
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    type Producer = ThreadManager;
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut Keys, _: Timestamp) {
        root.keys.insert("p1".into());
        let _ = root.drain();
    }
    fn folds(&mut self, _: &Stamped<Input>, tx: &mut Tx<'_, Keys>) {
        tx.dirty.insert("p1".into());
    }
}

/// The CHILD, addressed by a path-shaped name and nothing else.
struct Child;
impl Shape<Input> for Child {
    const NAME: &'static str = "tree/leaves";
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const PARENT: Option<&'static str> = Some("tree");
    type Producer = ThreadManager;
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut Keys, _: Timestamp) {
        root.keys.insert("p1:c1".into());
        let _ = root.drain();
    }
    fn folds(&mut self, _: &Stamped<Input>, tx: &mut Tx<'_, Keys>) {
        tx.dirty.insert("p1:c1".into());
    }
}

/// A child whose fold panics on command, for the death-independence test.
struct BoomChild;
impl Shape<Input> for BoomChild {
    const NAME: &'static str = "tree/boom";
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const PARENT: Option<&'static str> = Some("tree");
    type Producer = ThreadManager;
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut Keys, _: Timestamp) {
        root.keys.insert("bk".into());
        let _ = root.drain();
    }
    fn folds(&mut self, input: &Stamped<Input>, _: &mut Tx<'_, Keys>) {
        if let Input::Push(Push::Info { reason, .. }) = &input.value {
            if reason == "boom" {
                panic!("the child fold panicked, as this test asked it to");
            }
        }
    }
}

fn info<'a>(list: &'a [threads::shape::ShapeInfo], name: &str) -> &'a threads::shape::ShapeInfo {
    list.iter().find(|i| i.name == name).unwrap_or_else(|| {
        let have: Vec<&String> = list.iter().map(|i| &i.name).collect();
        panic!("no shape named {name}; the hub holds {have:?}")
    })
}

fn an_info(reason: &str) -> Push {
    Push::Info {
        domain: "x".into(),
        reason: reason.into(),
        account: None,
        detail: None,
        at: "now".into(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. Hydrate
// ─────────────────────────────────────────────────────────────────────────────

/// **THE DEFAULT IS WHAT `accounts` DOES, AND A SHAPE THAT DECLARES NOTHING GETS
/// IT.**
///
/// The whole "nothing regresses" claim for both consts rests on this, so it is
/// tested rather than asserted: `Plain` declares no `HYDRATE`, no `PARENT` and
/// no `hydrates_key`, and a keyless subscribe to it still means "every key I know
/// about" — N >= 2 keys, one export each — and is not refused.
///
/// **THE MUTATION.** Change `Shape::HYDRATE`'s default to `Hydrate::PerKey`.
/// The keyless subscribe is then refused and `reads` is 0.
#[tokio::test]
async fn a_shape_that_declares_no_key_scope_keeps_the_fleet_behaviour() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Plain, m.clone());

    let mut c = hub.connect(41);
    hub.subscribe("plain", None, 41, None)
        .expect("a keyless subscribe to a fleet shape is not refused");
    settle().await;

    let (frames, reads, _) = drain(&mut c);
    assert_eq!(
        reads, 2,
        "one export per key it holds, and it holds two: {:?}",
        frames.iter().map(|f| f.key.clone()).collect::<Vec<_>>()
    );
    let keys: BTreeSet<String> = frames.iter().filter_map(|f| f.key.clone()).collect();
    assert_eq!(
        keys,
        ["aa".to_string(), "bb".to_string()].into_iter().collect(),
        "and they are the shape's own keys"
    );

    // The default `hydrates_key` is never reached on a `Whole` shape at all: a key
    // it does not hold still gets the frame it always got — a row-less export the
    // mirror holds as `NotHydrated` — and not a refusal.
    hub.subscribe("plain", Some("never-heard-of-it"), 41, None)
        .expect("a `Whole` shape does not refuse an unknown key; it may arrive later");
    settle().await;
    let (frames, reads, _) = drain(&mut c);
    assert_eq!(reads, 1, "one frame, and it is a frame rather than silence");
    assert!(
        frames[0].wire.row.is_none(),
        "with no row, which is exactly what it did before any of this existed"
    );
    drop(tmp);
}

/// **A SHAPE THAT DECLARES `Whole` OUT LOUD IS NOT A `PerKey` SHAPE.**
///
/// This test used to be `an_active_shape_exports_its_active_set_and_is_not_
/// refused`, and it existed because `KeyScope::Active` had no distinct
/// mechanism of its own — so the one way to get it wrong was to route it down
/// the refusal path because it was "not `Fleet`". `Active` is DELETED
/// (2026-09-28) and `threads/summary`, its only declarer, now says `Whole`; the
/// failure mode survives the deletion exactly, because "declared something" is
/// still not "not the default".
///
/// **THE MUTATION.** In `Hub::subscribe_detail`, change the guard from
/// `slot.hydrate == Hydrate::PerKey` to `slot.hydrate != Hydrate::Whole`.
/// Nothing moves — which is the point of the second half of this test, where
/// the mutation that DOES bite is inverting the comparison.
#[tokio::test]
async fn an_explicitly_whole_shape_exports_its_set_and_is_not_refused() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Explicit, m.clone());

    let mut c = hub.connect(42);
    hub.subscribe("explicit", None, 42, None)
        .expect("a `Whole` shape reads its set at init, so a keyless subscribe is legal");
    settle().await;
    let (frames, reads, _) = drain(&mut c);
    assert_eq!(reads, 2, "the set it read at init, one export each");
    let keys: BTreeSet<String> = frames.iter().filter_map(|f| f.key.clone()).collect();
    assert_eq!(
        keys,
        ["live-1".to_string(), "live-2".to_string()]
            .into_iter()
            .collect()
    );
    drop(tmp);
}

/// **A KEYLESS `detail: rows` SUBSCRIBE TO A `PerKey` SHAPE IS REFUSED ON THE
/// REQUEST ID, AND THE REFUSAL NAMES THE CEILING.**
///
/// "Every key I know about" is "nothing" there, and answering `{queued: true}` and
/// then exporting nothing is the ack-then-silence class the armed-timer cap was
/// moved out of the shape task to close. The refusal is decidable from the const
/// alone, which is why it can be answered where the caller still sees it.
///
/// **THE MUTATION.** Delete the `KeyRequired` guard in `Hub::subscribe_detail`. The
/// subscribe then returns `Ok` and no frame ever arrives — which is what the
/// assertion on `reads` below catches, so this test proves both halves.
#[tokio::test]
async fn a_keyless_subscribe_to_an_on_demand_shape_is_refused_not_acked() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(OnDemandShape, m.clone());

    let mut c = hub.connect(43);
    match hub.subscribe("ondemand", None, 43, None) {
        Err(ShapeError::KeyRequired { shape, max_rows }) => {
            assert_eq!(shape, "ondemand");
            // **RULE 2.8: A REFUSAL NAMES THE CEILING IT IS AGAINST**, so the
            // consumer learns what to ask instead rather than only that it
            // asked wrongly.
            assert_eq!(max_rows, 500, "the shape's declared MAX_ROWS_PER_SUBSCRIBE");
        }
        other => panic!(
            "a shape with no inventory must REFUSE a request for its inventory, \
             not ack it and then say nothing: {other:?}"
        ),
    }
    settle().await;
    let (_, reads, lives) = drain(&mut c);
    assert_eq!(
        (reads, lives),
        (0, 0),
        "and a refused subscribe creates no subscription, so no frame follows it"
    );

    // The shape is also not now TRACKING on the strength of a refused subscribe —
    // a refusal that woke a lazy shape would be a refusal that cost something.
    assert!(
        !hub.tracked("ondemand"),
        "a refused subscribe must not have started the shape"
    );
    drop(tmp);
}

/// **A KEYED SUBSCRIBE ASKS THE PRODUCER TO HYDRATE THAT KEY, AND ONLY THAT
/// KEY.**
///
/// The mechanism `docs/chassis-resources.md` promotes from an option to a
/// requirement, because a VFS root cannot be hydrated whole. The shape holds
/// NOTHING at init; naming `ok-1` makes `ok-1` exist and exports it with a real
/// row; naming a second key makes a second key, and the first is still there.
///
/// **THE MUTATION.** Make `ShapeTask::do_subscribe` skip the `hydrate_one_key`
/// call. The export then carries `row: None` for a key the producer was perfectly
/// able to provide.
#[tokio::test]
async fn an_on_demand_shape_hydrates_the_one_key_a_consumer_names() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(OnDemandShape, m.clone());

    let mut c = hub.connect(44);
    hub.subscribe("ondemand", Some("ok-1"), 44, None).unwrap();
    settle().await;
    let (frames, reads, _) = drain(&mut c);
    assert_eq!(reads, 1, "one export, for the one key asked for");
    assert_eq!(frames[0].key.as_deref(), Some("ok-1"));
    assert_eq!(
        frames[0].wire.row.as_ref().and_then(|r| r.get("key")),
        Some(&json!("ok-1")),
        "and it carries a REAL row, which is the whole difference between a \
         hydrated key and a key-shaped hole: {:?}",
        frames[0].wire
    );

    // A SECOND key, so the on-demand set is proved to grow rather than to be
    // replaced — N >= 2, because a key set that arbitrates between keys cannot be
    // proved with one.
    hub.subscribe("ondemand", Some("ok-2"), 44, None).unwrap();
    settle().await;
    let (frames, reads, _) = drain(&mut c);
    assert_eq!(reads, 1);
    assert_eq!(frames[0].key.as_deref(), Some("ok-2"));
    let keys: BTreeSet<String> = info(&hub.list(), "ondemand").keys.iter().cloned().collect();
    assert_eq!(
        keys,
        ["ok-1".to_string(), "ok-2".to_string()].into_iter().collect(),
        "both keys are held, so hydrating one did not evict the other"
    );
    drop(tmp);
}

/// **THE DEFAULT `hydrates_key` REFUSES, AND A REFUSAL IS STILL A FRAME.**
///
/// `Refuser` declares `OnDemand` and overrides nothing, so the chassis default
/// runs — which is what makes the trait method additive for every shape that
/// already exists. The consumer is told: it gets the row-less export, not silence.
///
/// **THE MUTATION.** Make `ShapeTask::do_subscribe` `return` when the key is still
/// unheld after the on-demand attempt, instead of falling through to the export.
/// `reads` is then 0: an ack, and then nothing, forever.
///
/// **AND THE MUTATION THAT DOES NOT WORK, which is worth recording because I
/// tried it first and it went green.** Flipping the default body of
/// `Shape::hydrates_key` from `false` to `true` changes NOTHING observable: the
/// default's body mints no key either way, so `root.keys()` is empty under both
/// and both produce the row-less frame. The boolean is not a contract on its own —
/// what is observable is whether a key appears and whether a frame arrives — so
/// this test is written against those and not against the return value. A test
/// written against the boolean would have been a test that could not fail.
#[tokio::test]
async fn the_default_hydrates_key_refuses_and_says_so_with_a_frame() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Refuser, m.clone());

    let mut c = hub.connect(45);
    hub.subscribe("refuser", Some("anything"), 45, None).unwrap();
    settle().await;
    let (frames, reads, _) = drain(&mut c);
    assert_eq!(
        reads, 1,
        "a refusal to hydrate is still answered with a frame — an ack followed by \
         nothing is the one shape this chassis refuses to have"
    );
    assert!(
        frames[0].wire.row.is_none(),
        "and the frame says there is nothing here: {:?}",
        frames[0].wire
    );
    assert!(
        info(&hub.list(), "refuser").keys.is_empty(),
        "the key was NOT minted, because the shape refused"
    );

    // And the one that DOES implement it is unaffected by the other's refusal —
    // the default is per shape, not per chassis.
    hub.register(OnDemandShape, m.clone());
    hub.subscribe("ondemand", Some("ok-9"), 45, None).unwrap();
    settle().await;
    assert_eq!(info(&hub.list(), "ondemand").keys, vec!["ok-9".to_string()]);
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Nesting
// ─────────────────────────────────────────────────────────────────────────────

/// **A NESTED SHAPE IS ADDRESSED BY ITS PATH-SHAPED NAME, WITH NO NEW SUBSCRIBE
/// PARAMETER, AND IS TRACKED INDEPENDENTLY.**
///
/// Three claims in one test because they are one mechanism: the child is a shape,
/// so `{shape, key}` reaches it, `Tracking::Lazy` makes an unsubscribed one free,
/// and `shapes.list` shows the declared edge.
///
/// **THE MUTATION.** Make `ShapeInfo.parent` always `None` in `Hub::list` (or
/// delete the `info.parent = ...` line) and the edge assertions go red; register
/// the child EAGER and the costs-nothing assertion goes red.
#[tokio::test]
async fn a_nested_shape_is_addressed_by_path_and_costs_nothing_unsubscribed() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Parent, m.clone());
    hub.register(Child, m.clone());

    // ── NOBODY HAS SUBSCRIBED: BOTH ARE FREE ────────────────────────────────
    settle().await;
    assert!(!hub.tracked("tree"), "an unsubscribed parent holds nothing");
    assert!(
        !hub.tracked("tree/leaves"),
        "and a nested shape nobody subscribes to costs nothing — the property the \
         whole design is for"
    );

    // ── THE CHILD ALONE, BY NAME ────────────────────────────────────────────
    let mut c = hub.connect(51);
    hub.subscribe("tree/leaves", Some("p1:c1"), 51, None)
        .expect("a path-shaped name is a shape name; nothing else changed");
    settle().await;
    let (frames, reads, _) = drain(&mut c);
    assert_eq!(reads, 1);
    assert_eq!(frames[0].shape, "tree/leaves");
    assert_eq!(frames[0].key.as_deref(), Some("p1:c1"));

    assert!(hub.tracked("tree/leaves"), "the child is running");
    assert!(
        !hub.tracked("tree"),
        "and the PARENT is not: subscribing to a child does not start its parent, \
         which is what independent tracking means"
    );

    // ── THE DECLARED EDGE, ON `shapes.list` ─────────────────────────────────
    let list = hub.list();
    assert_eq!(
        info(&list, "tree/leaves").parent.as_deref(),
        Some("tree"),
        "the edge is declared and reported, not recovered by splitting a string"
    );
    assert_eq!(
        info(&list, "tree").parent, None,
        "a root shape has no parent, and the field is absent rather than empty"
    );
    assert_eq!(hub.core().children("tree"), vec!["tree/leaves"]);
    assert!(hub.core().children("tree/leaves").is_empty());
    drop(tmp);
}

/// **A PARENT RE-INIT DOES NOT INVALIDATE ITS CHILDREN — AND A CONSUMER CAN
/// STILL TELL WHETHER ITS TWO MIRRORS ARE FROM THE SAME ERA.**
///
/// The epoch ruling, asserted as behaviour. `shapewire::wire::Epoch::
/// parent_epoch` carries the argument; the short form is that the only thing that
/// causes a re-init is the shape's OWN FIFO overflowing, which is evidence about
/// one queue and about nothing else, and that coupling turns one overloaded parent
/// into N hydrates per five seconds.
///
/// The parent is registered at FIFO capacity 2 so a burst overflows it; the child
/// keeps the default, so the same burst does not overflow the child. That
/// asymmetry IS the scenario: one shape lost inputs, the other did not.
///
/// **THE MUTATIONS, both of which this test catches:**
/// * COUPLE them — call the child's `reinit_now` when the parent re-inits — and
///   the child's `reads` stops being 0.
/// * Stop stamping: delete the `parent_epoch` assignment in `init_blocking` and
///   the era assertions go red, because both mirrors then claim the same (absent)
///   era forever.
#[tokio::test]
async fn a_parent_reinit_leaves_its_children_alone_but_is_visible_as_an_era() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    // Capacity 2 on the parent: small enough that a burst issued without ever
    // awaiting fills it, because on a current-thread runtime the task cannot run
    // until we yield.
    hub.core().register(Parent, m.clone(), 2);
    hub.register(Child, m.clone());

    let mut p = hub.connect(61);
    let mut c = hub.connect(62);
    hub.subscribe("tree", Some("p1"), 61, None).unwrap();
    hub.subscribe("tree/leaves", Some("p1:c1"), 62, None).unwrap();
    settle().await;
    let (_, p_reads, _) = drain(&mut p);
    let (_, c_reads, _) = drain(&mut c);
    assert_eq!((p_reads, c_reads), (1, 1), "each got its own export");

    let list = hub.list();
    assert_eq!(
        info(&list, "tree").epoch.reinit, 0,
        "the parent has not re-inited yet"
    );
    assert_eq!(
        info(&list, "tree/leaves").epoch.parent_epoch,
        Some(0),
        "and the child's state was built under parent era 0 — SAME ERA, which is \
         the question a consumer holding both mirrors has to be able to answer"
    );
    assert_eq!(
        info(&list, "tree").epoch.parent_epoch, None,
        "a root shape stamps no parent era, so no frame it emits gains a field"
    );

    // ── OVERFLOW THE PARENT, AND ONLY THE PARENT ────────────────────────────
    for _ in 0..40 {
        hub.emit(an_info("flood"));
    }
    settle().await;
    settle().await;

    let (_, p_reads, _) = drain(&mut p);
    let (_, c_reads, c_lives) = drain(&mut c);
    let list = hub.list();

    assert!(
        info(&list, "tree").epoch.lost > 0,
        "the scenario requires the parent to have actually lost inputs, or there \
         is nothing to re-init from: {:?}",
        info(&list, "tree").epoch
    );
    assert_eq!(
        info(&list, "tree").epoch.reinit, 1,
        "so the parent re-inited exactly once (the 5s floor caps it): {:?}",
        info(&list, "tree").epoch
    );
    assert!(
        p_reads >= 1,
        "and repaired its own mirror with a fresh export"
    );

    assert_eq!(
        info(&list, "tree/leaves").epoch.lost, 0,
        "the child's own queue never overflowed — which is the point: a parent's \
         overflow is evidence about the parent's queue"
    );
    assert_eq!(
        info(&list, "tree/leaves").epoch.reinit, 0,
        "SO THE CHILD DID NOT RE-INIT. Coupling here would re-hydrate every open \
         child of every overloaded parent, once per floor period"
    );
    assert_eq!(
        c_reads, 0,
        "and the child's consumer was sent no re-export, because nothing about its \
         mirror became wrong"
    );
    assert!(
        c_lives > 0,
        "while the child kept FOLDING and kept emitting — independent, not frozen"
    );

    // ── THE ERA IS NOW DETECTABLY DIFFERENT ─────────────────────────────────
    assert_eq!(
        info(&list, "tree/leaves").epoch.parent_epoch,
        Some(0),
        "the child's state is still from parent era 0…"
    );
    assert_eq!(
        info(&list, "tree").epoch.reinit, 1,
        "…and the parent is on era 1, so a consumer comparing the two learns that \
         anything it inferred FROM THE PARENT about this child predates the \
         parent's rebuild. That comparison is the whole thing independence costs, \
         and it is one integer"
    );

    // ── AND RE-SUBSCRIBING THE CHILD PUTS THEM BACK IN STEP ─────────────────
    //
    // Which is rule 2.5 doing its job: recovery is re-subscribe, and there is no
    // second verb for "sync my eras".
    hub.unsubscribe("tree/leaves", Some("p1:c1"), 62);
    settle().await;
    let mut c2 = hub.connect(63);
    hub.subscribe("tree/leaves", Some("p1:c1"), 63, None).unwrap();
    settle().await;
    let (_, reads, _) = drain(&mut c2);
    assert_eq!(reads, 1);
    assert_eq!(
        info(&hub.list(), "tree/leaves").epoch.parent_epoch,
        Some(1),
        "a child that re-inits picks up the parent's current era"
    );
    drop(tmp);
}

/// **A CHILD'S DEATH DOES NOT TOUCH ITS PARENT.**
///
/// The other half of the liveness ruling. The child's fold panics; the supervisor
/// marks it dead, refuses new subscribes to it, and tells its mirrors — and the
/// parent goes on folding, exporting and accepting subscribes, because its fold is
/// independent and its summary is its own cell.
///
/// **THE MUTATION.** In the supervisor, mark every shape dead instead of the one
/// whose handle resolved (`for s in slots { *s.dead = ... }`). The parent
/// assertions then go red.
#[tokio::test]
async fn a_childs_death_leaves_its_parent_alive() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Parent, m.clone());
    hub.register(BoomChild, m.clone());

    let mut p = hub.connect(71);
    let mut c = hub.connect(72);
    hub.subscribe("tree", Some("p1"), 71, None).unwrap();
    hub.subscribe("tree/boom", Some("bk"), 72, None).unwrap();
    settle().await;
    let _ = drain(&mut p);
    let _ = drain(&mut c);

    hub.emit(an_info("boom"));
    settle().await;
    settle().await;

    // The child is dead, said once, and refuses.
    let (frames, _, _) = drain(&mut c);
    assert_eq!(
        frames
            .iter()
            .filter(|f| f.wire.epoch.state == threads::shape::ShapeState::Dead)
            .count(),
        1,
        "exactly one death export for the child"
    );
    assert!(hub.is_dead("tree/boom").is_some());
    assert!(matches!(
        hub.subscribe("tree/boom", Some("bk"), 73, None),
        Err(ShapeError::Dead { .. })
    ));

    // THE PARENT IS UNAFFECTED, and not merely "not marked dead" — it still works.
    assert!(
        hub.is_dead("tree").is_none(),
        "a child's fold panicking says nothing about its parent's"
    );
    hub.emit(an_info("after"));
    settle().await;
    let (_, _, p_lives) = drain(&mut p);
    assert!(
        p_lives > 0,
        "the parent kept folding and kept emitting after its child died"
    );
    let mut p2 = hub.connect(74);
    hub.subscribe("tree", Some("p1"), 74, None)
        .expect("and it still accepts subscribes");
    settle().await;
    assert_eq!(drain(&mut p2).1, 1);
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. The declaration-time checks
// ─────────────────────────────────────────────────────────────────────────────
//
// Each is a panic in `register`, which runs in the producer's BUILD function — so
// a mis-declared tree is a daemon that does not boot rather than a consumer that
// subscribes to something incoherent. That is why they are panics and not
// `Result`s: there is nobody to hand an error to at boot.

struct SlashNoParent;
impl Shape<Input> for SlashNoParent {
    const NAME: &'static str = "a/b";
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    type Producer = ThreadManager;
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, _: &mut Keys, _: Timestamp) {}
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, Keys>) {}
}

struct ParentNotRegistered;
impl Shape<Input> for ParentNotRegistered {
    const NAME: &'static str = "ghost/x";
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const PARENT: Option<&'static str> = Some("ghost");
    type Producer = ThreadManager;
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, _: &mut Keys, _: Timestamp) {}
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, Keys>) {}
}

struct NameDoesNotSpellParent;
impl Shape<Input> for NameDoesNotSpellParent {
    const NAME: &'static str = "elsewhere";
    const CELLS: Cells = KEY_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const PARENT: Option<&'static str> = Some("tree");
    type Producer = ThreadManager;
    type Root = Keys;
    fn root(&self) -> Keys {
        Keys::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, _: &mut Keys, _: Timestamp) {}
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, Keys>) {}
}

/// A path-shaped name with no declared parent is a missing declaration, not a
/// root shape with a slash in it. **MUTATION:** delete the `!S::NAME.contains('/')`
/// assert.
#[tokio::test]
#[should_panic(expected = "declares no PARENT")]
async fn a_path_shaped_name_without_a_declared_parent_is_refused() {
    let (m, _tmp) = start().await;
    m.hub.register(SlashNoParent, m.clone());
}

/// An edge to a shape that does not exist. **MUTATION:** replace the
/// `find(|s| s.name == p)` panic with `.map(...)` returning `None`.
#[tokio::test]
#[should_panic(expected = "is not registered")]
async fn a_parent_that_is_not_registered_is_refused() {
    let (m, _tmp) = start().await;
    m.hub.register(ParentNotRegistered, m.clone());
}

/// A declared parent the name does not spell — so a consumer could not find the
/// child from the parent. **MUTATION:** drop the `strip_prefix` panic.
#[tokio::test]
#[should_panic(expected = "must be")]
async fn a_name_that_does_not_spell_its_parent_is_refused() {
    let (m, _tmp) = start().await;
    m.hub.register(Parent, m.clone());
    m.hub.register(NameDoesNotSpellParent, m.clone());
}
