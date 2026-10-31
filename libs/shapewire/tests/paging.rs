//! # Paging, proved — `page`, `from`, `next`, and the three-level chain
//!
//! The operator: *"we should make large list shapes page by built-in mechanism
//! so it's just a field you set … a global default of 100, a producer-set
//! default informed by the size of the thing being paginated, and then a
//! consumer can set their own page size since they know their context. null
//! being no pagination."*
//!
//! What this file holds to:
//!
//! * **Rule 2.5** — a page token is a KEY the consumer holds (`next`, sent
//!   back verbatim as `from`), not a
//!   position the producer remembers. The same ask yields the same keys, so
//!   re-asking IS recovery and the producer keeps nothing per subscriber.
//! * **Rule 2.8** — a `page` above `rows_per_subscribe` is refused NAMING the
//!   ceiling, never clamped; a short answer says `complete: false` and where it
//!   stopped.
//! * **Rule 2.6** — a consumer that sends nothing gets the published default;
//!   no frame of a complete answer grew a byte (`next` is absent, asserted).
//!
//! Every red was forced with its real text; the mutation is in the docstring.

use std::collections::{BTreeSet, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use serde_json::{json, Value};

use shapewire::cell::Cells;
use shapewire::fold::{Outbox, Root};
use shapewire::hub::{Hub, Kind, Shape, Tracking, Tx};
use shapewire::mirror::{Ask, Mirror, OverCeiling};
use shapewire::operation::{DType::Scalar, Presence::Required, Scalar::Str};
use shapewire::wire::{
    CellState, Op, Paging, ShapeError, ShapeFrame, Stamped, Timestamp, DEFAULT_PAGE_ROWS,
};

#[derive(Clone)]
struct Beat1;
impl Kind for Beat1 {
    fn kind(&self) -> &'static str {
        "beat"
    }
}

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

shapewire::declare_cells! {
    KEY_CELLS = [("key", Scalar(Str), Required, [])]
}

/// Zero-padded so key order is numeric order and the walk is checkable.
fn key(i: usize) -> String {
    format!("k{i:04}")
}

/// One shape per declaration under test; each hydrates `N` keys.
macro_rules! pool {
    ($name:ident, $wire:literal, $n:expr, $page:expr, $max:expr) => {
        struct $name;
        impl Shape<Beat1> for $name {
            const NAME: &'static str = $wire;
            const TRACKING: Tracking = Tracking::Lazy;
            const CELLS: Cells = KEY_CELLS;
            const SHARED: Cells = Cells::NONE;
            const PAGE: Paging = $page;
            const MAX_ROWS_PER_SUBSCRIBE: usize = $max;
            type Producer = ();
            type Root = Keys;
            fn root(&self) -> Keys {
                Keys::default()
            }
            fn hydrates(&mut self, _: &(), root: &mut Keys, _: Timestamp) {
                for i in 0..$n {
                    root.keys.insert(key(i));
                }
                let _ = root.drain();
            }
            fn folds(&mut self, _: &Stamped<Beat1>, _: &mut Tx<'_, Keys>) {}
        }
    };
}

// 250 keys, nothing declared: the chassis default of 100 applies.
pool!(Silent, "silent", 250, Paging::Default, 500);
// 250 keys, the producer says 10: "informed by the size of the thing".
pool!(Tens, "tens", 250, Paging::Rows(10), 500);
// 250 keys, the producer says no pagination: the ceiling, as before.
pool!(Whole, "whole", 250, Paging::Whole, 500);
// 40 keys, a ceiling BELOW the chassis default: the default is the ceiling.
pool!(Small, "small", 40, Paging::Default, 30);
// 7 keys and nothing declared: smaller than any page — the pre-paging case.
pool!(Tiny, "tiny", 7, Paging::Default, 500);
// Two declarations that contradict the shape's own contract.
pool!(ZeroPage, "zero-page", 7, Paging::Rows(0), 500);
pool!(OverPage, "over-page", 7, Paging::Rows(600), 500);

fn a_hub() -> Arc<Hub<Beat1>> {
    let hub = Arc::new(Hub::<Beat1>::new());
    hub.register(Silent, (), 64);
    hub.register(Tens, (), 64);
    hub.register(Whole, (), 64);
    hub.register(Small, (), 64);
    hub.register(Tiny, (), 64);
    hub
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
}

fn exports(f: &mut shapewire::hub::ConnFrames) -> Vec<Arc<ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        if x.wire.op == Op::Read {
            all.push(x);
        }
    }
    all
}

/// Ask one page; return `(keys in order, complete, next)` off the frames that
/// answered THIS subscribe id and no other.
async fn page(
    hub: &Hub<Beat1>,
    frames: &mut shapewire::hub::ConnFrames,
    shape: &str,
    conn: u64,
    ask: Paging,
    from: Option<&str>,
) -> (Vec<String>, Option<bool>, Option<String>) {
    let id = hub.subscribe_page(shape, conn, ask, from).expect("inside the ceiling");
    settle().await;
    let got: Vec<_> = exports(frames)
        .into_iter()
        .filter(|f| f.wire.subscribes.contains(&id))
        .collect();
    let keys = got.iter().filter_map(|f| f.wire.key.clone()).collect();
    let last = got.last().expect("a page answers with at least one frame");
    assert!(
        got.iter().take(got.len() - 1).all(|f| f.wire.complete.is_none() && f.wire.next.is_none()),
        "only the LAST frame of a page says whether it is complete and where it stopped"
    );
    (keys, last.wire.complete, last.wire.next.clone())
}

// ─────────────────────────────────────────────────────────────────────────────
// The chain
// ─────────────────────────────────────────────────────────────────────────────

/// **THE CHAIN, RESOLVED** — pure, so every arm is one line.
///
/// **THE MUTATION.** In `Paging::resolve`, make the `Default`/`Default` arm
/// return `ceiling`.
///
/// Forced red:
/// ```text
/// nothing declared, nothing asked: the chassis default
/// ```
#[test]
fn the_chain_resolves_consumer_then_producer_then_chassis() {
    use Paging::*;
    assert_eq!(
        Paging::resolve(Default, Default, 500),
        DEFAULT_PAGE_ROWS,
        "nothing declared, nothing asked: the chassis default"
    );
    assert_eq!(Paging::resolve(Default, Rows(10), 500), 10, "the producer's default");
    assert_eq!(Paging::resolve(Default, Whole, 500), 500, "the producer said no pagination");
    assert_eq!(Paging::resolve(Rows(25), Rows(10), 500), 25, "the consumer wins over the producer");
    assert_eq!(Paging::resolve(Whole, Rows(10), 500), 500, "null is the ceiling by name");
    assert_eq!(
        Paging::resolve(Default, Default, 30),
        30,
        "the chassis default against a smaller ceiling is the ceiling — nobody's ask was cut"
    );
    // The wire spellings, including the two refusals.
    assert_eq!(Paging::parse(None), Some(Default));
    assert_eq!(Paging::parse(Some(&json!(null))), Some(Whole));
    assert_eq!(Paging::parse(Some(&json!(25))), Some(Rows(25)));
    assert_eq!(Paging::parse(Some(&json!(0))), None, "a page of nothing is not an ask");
    assert_eq!(Paging::parse(Some(&json!("lots"))), None);
}

/// **`shapes.list` PUBLISHES THE PRODUCER'S TWO LINKS, RESOLVED.**
///
/// **THE MUTATION.** In `ShapeInfo::declared_by`, publish `max` as the
/// default for every declaration.
///
/// Forced red:
/// ```text
/// a silent shape publishes the chassis default
/// ```
#[tokio::test]
async fn shapes_list_publishes_the_page_contract_resolved() {
    let hub = a_hub();
    let list = hub.list();
    let page = |n: &str| list.iter().find(|s| s.name == n).unwrap().page.unwrap();
    assert_eq!(page("silent").default, Some(100), "a silent shape publishes the chassis default");
    assert_eq!(page("silent").max, 500);
    assert_eq!(page("tens").default, Some(10));
    assert_eq!(page("whole").default, None, "`None` is *no pagination*, stated");
    assert_eq!(page("small").default, Some(30), "the default cannot exceed the ceiling");
    // And the mirror reads it before it asks.
    let mut m = Mirror::new("silent");
    assert_eq!(m.page(), None, "a mirror never handed a `ShapeInfo` knows nothing");
    m.learn(list.iter().find(|s| s.name == "silent").unwrap());
    assert_eq!(m.page().unwrap().default, Some(100));
    m.reset();
    assert_eq!(m.page().unwrap().max, 500, "a re-init keeps the contract");
}

// ─────────────────────────────────────────────────────────────────────────────
// The walk
// ─────────────────────────────────────────────────────────────────────────────

/// **A SILENT CONSUMER WALKS 250 KEYS IN THREE PAGES OF 100, BY `next`**, and
/// the union is the pool with no overlap. This is the positive sentinel for
/// every claim about `next`: a walk that could not fail would not visit 250.
///
/// **THE MUTATION.** In `do_subscribe`, compute `next` as `rest[cap - 1]`
/// (the last key exported, not the first not exported).
///
/// Forced red — on the first page's `next`, before the walk even starts; the
/// partition assertion would catch the same mutation one page later:
/// ```text
/// `next` is the first key NOT exported
/// ```
#[tokio::test]
async fn a_silent_consumer_pages_the_pool_by_next_and_the_pages_partition_it() {
    let hub = a_hub();
    let mut frames = hub.connect(1);
    let mut seen: Vec<String> = Vec::new();
    let mut from: Option<String> = None;
    let mut pages = 0;
    loop {
        let (keys, complete, next) =
            page(&hub, &mut frames, "silent", 1, Paging::Default, from.as_deref()).await;
        pages += 1;
        if pages == 1 {
            assert_eq!(keys.len(), 100, "nothing asked: the published default of 100");
            assert_eq!(keys[0], key(0));
            assert_eq!(complete, Some(false));
            assert_eq!(next.as_deref(), Some(key(100).as_str()), "`next` is the first key NOT exported");
        }
        seen.extend(keys);
        match (complete, next) {
            (Some(true), None) => break,
            (Some(false), Some(n)) => from = Some(n),
            other => panic!("a page is complete with no next, or short with one: {other:?}"),
        }
        assert!(pages < 10, "the walk must terminate");
    }
    assert_eq!(pages, 3, "250 keys at 100 a page is three pages");
    let distinct: HashSet<&String> = seen.iter().collect();
    assert_eq!(distinct.len(), 250, "the pages partition the pool: no key twice, none missing");
    assert_eq!(seen, (0..250).map(key).collect::<Vec<_>>(), "and in key order");
    // Rule 2.5: the same ask yields the same keys. Re-asking page two IS
    // recovery, and the producer held nothing to make it so.
    let (again, c, n) = page(&hub, &mut frames, "silent", 1, Paging::Default, Some(&key(100))).await;
    assert_eq!(again, (100..200).map(key).collect::<Vec<_>>());
    assert_eq!((c, n.as_deref()), (Some(false), Some(key(200).as_str())));
}

/// **THE PRODUCER'S DEFAULT, THE CONSUMER'S OVERRIDE, AND `null`.**
///
/// **THE MUTATION.** In `enqueue_subscribe`, resolve with `Paging::Default`
/// in place of the consumer's `page`.
///
/// Forced red:
/// ```text
/// the consumer's 25 wins over the producer's 10
/// ```
#[tokio::test]
async fn the_producer_default_the_consumer_override_and_null() {
    let hub = a_hub();
    let mut frames = hub.connect(1);
    let (keys, complete, next) = page(&hub, &mut frames, "tens", 1, Paging::Default, None).await;
    assert_eq!(keys.len(), 10, "the producer declared 10");
    assert_eq!((complete, next.as_deref()), (Some(false), Some(key(10).as_str())));
    let (keys, _, _) = page(&hub, &mut frames, "tens", 1, Paging::Rows(25), None).await;
    assert_eq!(keys.len(), 25, "the consumer's 25 wins over the producer's 10");
    let (keys, complete, next) = page(&hub, &mut frames, "tens", 1, Paging::Whole, None).await;
    assert_eq!(keys.len(), 250, "`null` is the ceiling by name: the whole pool, under 500");
    assert_eq!((complete, next), (Some(true), None));
    // A producer that declared `Whole` serves the ceiling to a silent consumer —
    // the rule-2.6 lever for a shape whose faces already read more than 100.
    let (keys, complete, _) = page(&hub, &mut frames, "whole", 1, Paging::Default, None).await;
    assert_eq!((keys.len(), complete), (250, Some(true)));
    // A ceiling below the chassis default: the default IS the ceiling.
    let (keys, complete, next) = page(&hub, &mut frames, "small", 1, Paging::Default, None).await;
    assert_eq!((keys.len(), complete, next.as_deref()), (30, Some(false), Some(key(30).as_str())));
}

/// **A PAGE ABOVE THE CEILING IS REFUSED NAMING THE CEILING, ON THE REQUEST,
/// WITH NOTHING QUEUED** — rule 2.8, and A4: never clamped. The ceiling itself
/// is served.
///
/// **THE MUTATION.** In `enqueue_subscribe`, change `n > rows_per_subscribe`
/// to `n >= rows_per_subscribe`.
///
/// Forced red:
/// ```text
/// a page equal to the ceiling is inside it and is served
/// ```
#[tokio::test]
async fn a_page_above_the_ceiling_is_refused_naming_it_and_the_ceiling_is_served() {
    let hub = a_hub();
    let mut frames = hub.connect(1);
    let err = hub.subscribe_page("silent", 1, Paging::Rows(501), None).unwrap_err();
    match err {
        ShapeError::AboveMax { shape, param, asked, max } => {
            assert_eq!((shape.as_str(), param.as_str(), asked.as_str(), max.as_str()), ("silent", "page", "501", "500"));
        }
        other => panic!("refused as AboveMax naming the ceiling, not {other:?}"),
    }
    settle().await;
    assert!(exports(&mut frames).is_empty(), "a refused ask queues nothing");
    hub.subscribe_page("silent", 1, Paging::Rows(500), None)
        .expect("a page equal to the ceiling is inside it and is served");
    // And the consuming half refuses the same ask in-process, before it is sent.
    let mut m = Mirror::new("silent");
    m.learn(hub.list().iter().find(|s| s.name == "silent").unwrap());
    let ask = Ask { rows: None, keys_per_frame: None, future_at_secs: None, armed_timers: None, page: Some(501) };
    assert_eq!(m.check(&ask), Err(OverCeiling::Page { asked: 501, ceiling: 500 }));
    assert_eq!(m.check(&Ask { page: Some(500), ..ask }), Ok(()));
}

/// **A POOL SMALLER THAN ANY PAGE IS THE PRE-PAGING CASE, BYTE FOR BYTE.** The
/// answer is complete and no frame carries `next` — asserted on the JSON, with
/// the short-page frame as the positive control that the key CAN appear.
///
/// **THE MUTATION.** In `do_subscribe`, compute `complete` as `true`
/// unconditionally. The claim (no `next` on a complete answer) cannot go red
/// on its own — a 7-key pool has no key to name — so the red lands on the
/// control, which is why the control is there:
/// ```text
/// the control: a short page's last frame DOES carry `next` on the wire
/// ```
#[tokio::test]
async fn a_pool_under_the_page_is_complete_and_grows_no_byte() {
    let hub = a_hub();
    let mut frames = hub.connect(1);
    let id = hub.subscribe("tiny", None, 1, None).unwrap();
    settle().await;
    let got: Vec<_> = exports(&mut frames).into_iter().filter(|f| f.wire.subscribes.contains(&id)).collect();
    assert_eq!(got.len(), 7);
    assert_eq!(got.last().unwrap().wire.complete, Some(true));
    for f in &got {
        let v = serde_json::to_value(&f.wire).unwrap();
        assert!(v.get("next").is_none(), "no frame of a complete answer carries `next`: {v}");
    }
    // The control.
    let id = hub.subscribe("silent", None, 1, None).unwrap();
    settle().await;
    let got: Vec<_> = exports(&mut frames).into_iter().filter(|f| f.wire.subscribes.contains(&id)).collect();
    let v = serde_json::to_value(&got.last().unwrap().wire).unwrap();
    assert_eq!(
        v.get("next"),
        Some(&json!(key(100))),
        "the control: a short page's last frame DOES carry `next` on the wire"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The declaration
// ─────────────────────────────────────────────────────────────────────────────

/// **A PRODUCER DEFAULT OF ZERO, OR ABOVE THE CEILING, IS REFUSED AT BOOT.**
///
/// **THE MUTATION.** In `Hub::register`, make the page `assert!`s
/// unconditionally true.
///
/// Forced red:
/// ```text
/// a PAGE of Rows(0) must refuse at register
/// ```
#[tokio::test]
async fn a_page_declaration_that_contradicts_the_contract_is_refused() {
    let hub = Arc::new(Hub::<Beat1>::new());
    let zero = catch_unwind(AssertUnwindSafe(|| hub.register(ZeroPage, (), 64)))
        .err()
        .and_then(|e| e.downcast::<String>().ok())
        .expect("a PAGE of Rows(0) must refuse at register");
    assert!(zero.contains("declares PAGE `Rows(0)`"), "{zero}");
    let over = catch_unwind(AssertUnwindSafe(|| hub.register(OverPage, (), 64)))
        .err()
        .and_then(|e| e.downcast::<String>().ok())
        .expect("a PAGE above the ceiling must refuse at register");
    assert!(over.contains("`Rows(600)` above its own MAX_ROWS_PER_SUBSCRIBE of 500"), "{over}");
    // The positive control: the well-formed declarations register.
    hub.register(Tens, (), 64);
    assert!(hub.list().iter().any(|s| s.name == "tens"));
}
