//! # Pool membership — `detail: keys`, and the MAX contract that bounds it
//!
//! The operator rejected a `pool-membership` SHAPE: *"this would just admit that
//! our shapes concept is deficient."* Membership is the same shape, over the
//! same keys, out of the same fold, at a COARSER LEVEL OF DETAIL — so it is a
//! parameter on `shapes.subscribe`, and under proposed rule 2.8 it has a ceiling
//! like every other parameter: `rows` is the ceiling, `keys` is less.
//!
//! **Every test here was forced red by the mutation named in its own docstring,
//! and the red was checked for `error[E` before it was believed** — this repo
//! has shipped a "forced red" that was really a compile error the mutation
//! introduced, and eleven tests that passed against broken code.
//!
//! The consumer-position test (§1) is deliberately over a REAL WEBSOCKET rather
//! than against the hub, because the thing being proved is a gate that lives on
//! the other side of the transport: `drive`'s `shape_members` set is what turns
//! a membership export into a standing subscription, and a hub-level test cannot
//! see it at all.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::event::Push;
use threads::shape::{
    CellState, ConnFrames, Detail, Hub, Hydrate, Input, Op, Outbox, Root, Shape, ShapeError,
    Stamped, Timestamp, Tracking, Tx,
};
use threads::{build_manager, ThreadManager, ThreadSettings};
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

fn thread_on(m: &ThreadManager, tmp: &std::path::Path) -> String {
    let mut s = ThreadSettings::new(tmp.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    m.create_thread(None, s).unwrap()
}

fn drain(f: &mut ConnFrames) -> Vec<Arc<threads::shape::ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

// ─────────────────────────────────────────────────────────────────────────────
// test shapes — small ceilings, so the paging and the short answer are reachable
// ─────────────────────────────────────────────────────────────────────────────

/// A keyed root whose key set a test sets directly, like `nesting.rs`'. It uses
/// the real `Map` cell here rather than a `BTreeSet`, because the membership set
/// being BESIDE the rows in `Map` is half of what is under test.
#[derive(Default)]
struct Pool {
    rows: threads::shape::Map<String, Cell>,
}

#[derive(Default)]
struct Cell {
    n: threads::shape::Register<u64>,
}

impl Root for Pool {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }
    fn members(&self) -> threads::shape::Membership {
        match self.rows.members_state() {
            CellState::Known { complete, .. } => threads::shape::Membership::Known {
                keys: self.rows.members().cloned().collect(),
                complete: *complete,
            },
            _ => threads::shape::Membership::NotHydrated {
                reason: "not read".to_string(),
            },
        }
    }
    fn row(&self, k: &str) -> Option<Value> {
        self.rows.get(&k.to_string()).map(|c| json!({"n": c.n.now()}))
    }
    fn cells(&self, k: &str) -> Option<Value> {
        self.rows
            .get(&k.to_string())
            .map(|c| json!({"n": c.n.state()}))
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

/// **FIVE MEMBERS, NO ROWS, AND A PAGE OF TWO.** A `PerKey` pool whose
/// membership read answers five ids and whose `hydrates_key` mints one row at a
/// time — the `threads` shape in miniature, with the ceilings small enough that
/// paging and the short answer are reachable in a test.
struct Tiny;

// The cells this file's test roots emit, declared — `Shape::CELLS` is required
// (see `shapewire::cell`), and a test shape declares what its `Root::cells`
// actually spells.
shapewire::declare_cells! {
    N_CELLS = [("n", shapewire::operation::DType::Scalar(shapewire::operation::Scalar::Int), shapewire::operation::Presence::Required, [])]
}

impl Shape<Input> for Tiny {
    const NAME: &'static str = "tiny";
    const CELLS: Cells = N_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const HYDRATE: Hydrate = Hydrate::PerKey;
    const MAX_KEYS_PER_FRAME: usize = 2;
    type Producer = ThreadManager;
    type Root = Pool;
    fn root(&self) -> Pool {
        Pool::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, _: &mut Pool, _: Timestamp) {}
    fn hydrates_members(&mut self, _: &ThreadManager, root: &mut Pool, as_of: Timestamp) -> bool {
        for k in ["a", "b", "c", "d", "e"] {
            root.rows.member(k.to_string());
        }
        root.rows.mark_members_read(as_of, true);
        let _ = root.drain();
        true
    }
    fn hydrates_key(
        &mut self,
        _: &ThreadManager,
        root: &mut Pool,
        key: &str,
        as_of: Timestamp,
    ) -> bool {
        if !root.rows.is_member(&key.to_string()) {
            return false;
        }
        root.rows.key(key.to_string()).n.read(as_of, 1);
        let _ = root.drain();
        true
    }
    /// A `Push::Info` whose reason is `join:<k>` is this pool's "a key arrived",
    /// and `part:<k>` is "a key left". **Neither mints a row** — `member` and
    /// `remove`, exactly as `threads`' fold does it.
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, Pool>) {
        let Input::Push(Push::Info { reason, .. }) = &input.value else {
            return;
        };
        if let Some(k) = reason.strip_prefix("join:") {
            tx.rows.member(k.to_string());
        }
        if let Some(k) = reason.strip_prefix("part:") {
            tx.rows.remove(&k.to_string());
        }
    }
}

/// **A `Whole` POOL WITH MORE KEYS THAN ITS ROW CEILING.** Five keys, ceiling
/// two — so a keyless `detail: rows` here is the case rule 2.8 says must be
/// answered SHORT and must say so, rather than silently truncated.
struct Wide;
impl Shape<Input> for Wide {
    const NAME: &'static str = "wide";
    const CELLS: Cells = N_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const MAX_ROWS_PER_SUBSCRIBE: usize = 2;
    type Producer = ThreadManager;
    type Root = Pool;
    fn root(&self) -> Pool {
        Pool::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut Pool, as_of: Timestamp) {
        for k in ["a", "b", "c", "d", "e"] {
            root.rows.key(k.to_string()).n.read(as_of, 1);
        }
        root.rows.mark_read(as_of, true);
        root.rows.mark_members_read(as_of, true);
        let _ = root.drain();
    }
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, Pool>) {}
}

/// **A ROOT THAT OVERRIDES NOTHING**, like `accounts`, `narration` and
/// `threads/summary`. It is what makes the test below about the chassis DEFAULT
/// rather than about a fixture: `Pool` above answers `members` out of the
/// `Map`'s membership set, and a `Whole` shape has no reason to.
#[derive(Default)]
struct Bare {
    rows: threads::shape::Map<String, Cell>,
}

impl Root for Bare {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }
    fn row(&self, k: &str) -> Option<Value> {
        self.rows
            .get(&k.to_string())
            .map(|c| json!({"n": c.n.now()}))
    }
    fn cells(&self, k: &str) -> Option<Value> {
        self.rows
            .get(&k.to_string())
            .map(|c| json!({"n": c.n.state()}))
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

/// `Whole`, declaring nothing else — the shape every producer in this repo is.
struct Fleet;
impl Shape<Input> for Fleet {
    const NAME: &'static str = "fleet";
    const CELLS: Cells = N_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    type Producer = ThreadManager;
    type Root = Bare;
    fn root(&self) -> Bare {
        Bare::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut Bare, as_of: Timestamp) {
        for k in ["a", "b", "c", "d", "e"] {
            root.rows.key(k.to_string()).n.read(as_of, 1);
        }
        root.rows.mark_read(as_of, true);
        let _ = root.drain();
    }
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, Bare>) {}
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

fn members_of(f: &threads::shape::ShapeFrame) -> Option<&threads::shape::MembersPage> {
    f.wire.members.as_ref()
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. FROM THE CONSUMER'S SEAT, OVER A REAL CONNECTION
// ═════════════════════════════════════════════════════════════════════════════

struct Client {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

/// **RULE 2.1'S CONSUMER IDENTITY, STATED IN ONE PLACE FOR EVERY SUBSCRIBE THIS
/// FILE SENDS.**
///
/// Identity is COMPULSORY at subscribe time since the operator's ruling of
/// 2026-10-08 (*"consumers to be forced to say who they are so we can track
/// consumers"*), so a subscribe carrying no `consumer` is refused `malformed`.
/// The tests in this file are about hydration, unsubscribe, retention, paging
/// and the rest — NOT about identity — so they state a well-formed one here,
/// once, exactly as a real consumer does: `seam/link.rs`'s `subscribe_params`
/// builds its identity in one place for the same reason, and the board's
/// `api.py` now does too.
///
/// **IT NEVER OVERWRITES AN IDENTITY THE CALLER STATED.** A test that sends a
/// deliberately blank or absent one to prove the refusal still can, by saying
/// `consumer` itself. That is how the rule is actually proven, and it is proven
/// elsewhere on purpose — `shapes.rs`'s
/// `an_absent_identity_is_refused_at_the_wire_and_so_is_a_blank_one` at the wire
/// and `shapewire::ledger`'s `an_absent_identity_is_refused_and_so_is_a_blank_one`
/// at the parser — never by this helper, which exists to keep unrelated tests
/// from asserting identity by accident.
fn identified(method: &str, params: Value) -> Value {
    if !method.ends_with("/subscribe") {
        return params;
    }
    let mut params = params;
    if params.get("consumer").is_some() {
        return params;
    }
    if let Some(o) = params.as_object_mut() {
        o.insert(
            "consumer".into(),
            json!({"name": "threads-tests", "shape_version": "shapes.md@rev4"}),
        );
    }
    params
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        let mut c = Self { ws };
        c.next().await.expect("a welcome");
        c
    }
    async fn next(&mut self) -> Option<Value> {
        use tokio_tungstenite::tungstenite::Message;
        loop {
            let got =
                tokio::time::timeout(std::time::Duration::from_millis(900), self.ws.next()).await;
            match got {
                Err(_) | Ok(None) => return None,
                Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).ok(),
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) => return None,
            }
        }
    }
    async fn drain(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        while let Some(v) = self.next().await {
            out.push(v);
        }
        out
    }
    async fn request(&mut self, id: &str, method: &str, params: Value) -> Value {
        let params = identified(method, params);
        use tokio_tungstenite::tungstenite::Message;
        self.ws
            .send(Message::Text(
                json!({"type":"request","id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        loop {
            let Some(f) = self.next().await else {
                panic!("no response to {method}");
            };
            if f["type"] == "response" && f["correlate"] == id {
                return f;
            }
        }
    }
}

/// **THE WHOLE POINT, FROM THE SEAT THAT PAYS FOR IT.**
///
/// A consumer subscribes to `threads` at `detail: keys` over a real socket. It
/// must get: the roster of what exists, NO ROWS AT ALL, and then — for a thread
/// created AFTER it subscribed — an `added` frame naming that thread.
///
/// That last clause is `docs/shapes.md` §7.2, the oldest open item in
/// `consumer-migration-ledger.md`: a keyless subscribe enumerated the keys of
/// that moment and was blind to every later one. It is also the reason a
/// consumer could not use this pool at all until now — `threads` is `PerKey`, a
/// keyless `detail: rows` subscribe is refused, and there was no way to ask what
/// existed without already naming a key you did not have.
///
/// **THE MUTATION.** In `drive`'s third arm (`shapewire/src/server.rs`),
/// delete the `shape_members.contains(&f.shape)` half of the undirected-frame
/// gate, leaving only `shape_subs.contains(&(shape, k))`. The roster still
/// arrives; the `added` never does, because a membership subscriber holds no
/// key.
///
/// Forced red:
/// ```text
/// a thread created AFTER the membership subscribe must ARRIVE as `added` —
/// that is `shapes.md` §7.2, and without it `detail: keys` is a snapshot
/// wearing a subscription's name. saw: []
/// ```
#[tokio::test]
async fn a_membership_subscriber_gets_the_roster_no_rows_and_later_arrivals() {
    let tmp = tempfile::tempdir().unwrap();
    threads::clock::testing::ticking();
    let manager = build_manager(config(tmp.path())).unwrap();
    let before_a = thread_on(&manager, tmp.path());
    let before_b = thread_on(&manager, tmp.path());

    let chassis = threads::chassis::chassis(Arc::new(manager.clone()));
    let listener = shapewire::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (addr, h) = chassis
        .serve_on_with_shutdown(listener, std::future::pending::<()>())
        .unwrap();
    std::mem::forget(h);

    let mut c = Client::connect(addr).await;
    let r = c
        .request(
            "1",
            &format!("shapes/{}/subscribe", "threads"),
            json!({"detail":"keys"}),
        )
        .await;
    assert_eq!(
        r["outcome"]["type"],
        json!("success"),
        "a keyless subscribe is refused for ROWS on this shape and ANSWERED for \
         membership — that is the whole change: {r}"
    );

    // ── THE ROSTER ───────────────────────────────────────────────────────────
    let first = c.drain().await;
    let pages: Vec<&Value> = first
        .iter()
        .filter(|f| f["type"] == "shape" && f.get("members").is_some())
        .collect();
    assert!(
        !pages.is_empty(),
        "a `detail: keys` subscribe must be answered with a membership frame: {first:#?}"
    );
    let roster: BTreeSet<String> = pages
        .iter()
        .flat_map(|p| {
            p["members"]["keys"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| v.as_str().map(str::to_string))
        })
        .collect();
    assert!(
        roster.contains(&before_a) && roster.contains(&before_b),
        "both threads that exist are in the roster: {roster:?}"
    );
    assert_eq!(
        pages.last().unwrap()["complete"],
        json!(true),
        "the LAST frame of a subscribe says the answer is whole — otherwise a \
         consumer can only detect the end with a deadline, which is the policy \
         rule 2.5 exists to keep out of clients: {pages:#?}"
    );

    // ── AND NOT ONE ROW ──────────────────────────────────────────────────────
    //
    // The arithmetic this parameter exists for: 5,417 rows is ~20 MB, 5,417 ids
    // is ~200 KB. A membership frame that carried rows would be the dump
    // arriving through the door built to avoid it.
    for f in first.iter().filter(|f| f["type"] == "shape") {
        assert!(
            f.get("row").is_none() && f.get("cells").is_none(),
            "`detail: keys` is membership and NOTHING else: {f}"
        );
    }

    // ── A THREAD CREATED AFTERWARDS ARRIVES ──────────────────────────────────
    let after = thread_on(&manager, tmp.path());
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let live = c.drain().await;
    let saw: Vec<&Value> = live
        .iter()
        .filter(|f| f["type"] == "shape" && f["event"]["kind"] == json!("added"))
        .collect();
    assert!(
        saw.iter().any(|f| f["key"] == json!(after.as_str())),
        "a thread created AFTER the membership subscribe must ARRIVE as `added` — \
         that is `shapes.md` §7.2, and without it `detail: keys` is a snapshot \
         wearing a subscription's name. saw: {saw:#?}"
    );
    // And still no rows: an arrival is an EXISTENCE claim. A consumer that wants
    // the row names the key, which is an ordinary keyed subscribe.
    for f in live.iter().filter(|f| f["type"] == "shape") {
        assert!(
            f.get("row").is_none(),
            "an `added` carries no row — `docs/access-patterns.md` 3.3: the same \
             bytes meaning two things is the collapsed-event class: {f}"
        );
    }
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. THE MECHANISM, AT THE HUB
// ═════════════════════════════════════════════════════════════════════════════

/// **A MEMBER IS NOT A ROW, AND THE SCOPE GUARD IS UNTOUCHED.**
///
/// `threads`' fold mints no key — every arm asks `holds` first — and that is
/// what keeps a 2.46 M-event ledger from growing the root to the whole fleet.
/// Membership had to be recorded WITHOUT weakening it, so the fold calls
/// `Map::member` and never `Map::key`.
///
/// Observable from outside: after a membership subscribe and five arrivals, the
/// shape's `keys` on `shapes.list` — which is the HELD set — is still empty,
/// while the roster has grown.
///
/// **THE MUTATION**, in the PRODUCT and not in this file's fixture: in
/// `fold.rs`, make `Map::member` mint the row — `pub fn member(&mut self, k: K)
/// { self.key(k); }` — which is the "membership is just a key" collapse this
/// whole mechanism exists instead of.
///
/// Forced red:
/// ```text
/// a membership arrival must NOT mint a row — that is the guard keeping an
/// unbounded pool's root bounded: ["f"]
/// ```
#[tokio::test]
async fn a_membership_arrival_does_not_mint_a_row() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Tiny, m.clone());

    let mut c = hub.connect(61);
    hub.subscribe_detail("tiny", None, 61, None, Detail::Keys)
        .expect("membership is answerable on a per-key pool");
    settle().await;
    drain(&mut c);

    m.hub.emit(an_info("join:f"));
    settle().await;

    let held: Vec<String> = hub
        .list()
        .into_iter()
        .find(|i| i.name == "tiny")
        .expect("the shape")
        .keys;
    assert!(
        held.is_empty(),
        "a membership arrival must NOT mint a row — that is the guard keeping an \
         unbounded pool's root bounded: {held:?}"
    );

    let frames = drain(&mut c);
    let added: Vec<String> = frames
        .iter()
        .filter(|f| matches!(f.wire.event, Some(threads::shape::Derived::Added)))
        .filter_map(|f| f.wire.key.clone())
        .collect();
    assert_eq!(
        added,
        vec!["f".to_string()],
        "and the arrival is still ANNOUNCED — recording it without telling anybody \
         would be the same silence with more bookkeeping: {frames:?}"
    );
    drop(tmp);
}

/// **THE MEMBERSHIP PAGE IS BOUNDED, AND THE LAST PAGE SAYS SO.**
///
/// `Tiny` declares `MAX_KEYS_PER_FRAME = 2` over five members, so the roster is
/// three frames: two, two, one. The chassis pages — there is no cursor the
/// consumer has to drive, because a page it was told about and cannot ask for
/// would be ack-then-silence wearing a cursor.
///
/// **THE MUTATION.** In `ShapeTask::export_members`, replace
/// `let per = S::MAX_KEYS_PER_FRAME.max(1);` with `let per = usize::MAX;` — the
/// "one frame is simpler" version, which is exactly the unbounded
/// `ShapeInfo.keys` this ceiling exists to replace.
///
/// Forced red:
/// ```text
/// MAX_KEYS_PER_FRAME = 2 over five members is three frames: 1
/// ```
#[tokio::test]
async fn the_membership_roster_is_paged_at_the_declared_ceiling() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Tiny, m.clone());

    let mut c = hub.connect(62);
    hub.subscribe_detail("tiny", None, 62, None, Detail::Keys)
        .unwrap();
    settle().await;
    let frames = drain(&mut c);
    let pages: Vec<&threads::shape::ShapeFrame> = frames
        .iter()
        .map(Arc::as_ref)
        .filter(|f| f.wire.members.is_some())
        .collect();
    assert_eq!(
        pages.len(),
        3,
        "MAX_KEYS_PER_FRAME = 2 over five members is three frames: {}",
        pages.len()
    );
    let sizes: Vec<usize> = pages
        .iter()
        .map(|f| match members_of(f).unwrap() {
            threads::shape::MembersPage::Known { keys, .. } => keys.len(),
            other => panic!("a readable pool must answer Known: {other:?}"),
        })
        .collect();
    assert_eq!(sizes, vec![2, 2, 1], "no frame over the ceiling");

    // **ONLY THE LAST PAGE CLAIMS THE ANSWER IS WHOLE.** A `complete: true` on
    // page one would tell a consumer it had the roster while two thirds of it
    // was still in flight.
    let completes: Vec<Option<bool>> = pages.iter().map(|f| f.wire.complete).collect();
    assert_eq!(
        completes,
        vec![None, None, Some(true)],
        "rule 2.8's fourth clause: the frame that ENDS the answer is the one that \
         says the answer ended: {completes:?}"
    );
    // And the resume hint points at the page that follows, so a mirror that lost
    // a frame knows where the hole starts.
    match members_of(pages[0]).unwrap() {
        threads::shape::MembersPage::Known { next, complete, .. } => {
            assert_eq!(next.as_deref(), Some("c"));
            assert!(!complete, "page one is not the whole roster");
        }
        other => panic!("{other:?}"),
    }
    drop(tmp);
}

/// **A KEYLESS `detail: rows` OVER THE CEILING IS ANSWERED SHORT AND SAYS SO.**
///
/// Rule 2.8's fourth clause. `Wide` holds five keys with
/// `MAX_ROWS_PER_SUBSCRIBE = 2`; the consumer asked for nothing, and nothing
/// means the ceiling, so it gets two rows and a `complete: false` that tells it
/// the pool is bigger than what it was handed. **Silent truncation is the lie
/// the rule forbids; a stated short answer is not.**
///
/// **THE MUTATION.** In `do_subscribe`'s keyless arm, replace
/// `let complete = all.len() <= cap;` with `let complete = true;` — the frame
/// still carries a field, and it lies.
///
/// Forced red:
/// ```text
/// five keys through a ceiling of two is a SHORT answer and the last frame must
/// say so: Some(true)
/// ```
#[tokio::test]
async fn a_keyless_rows_subscribe_over_the_ceiling_says_it_is_short() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Wide, m.clone());

    let mut c = hub.connect(63);
    hub.subscribe("wide", None, 63, None)
        .expect("asking for nothing is asking for the ceiling, never a refusal");
    settle().await;
    let frames = drain(&mut c);
    let reads: Vec<&threads::shape::ShapeFrame> = frames
        .iter()
        .map(Arc::as_ref)
        .filter(|f| f.wire.op == Op::Read)
        .collect();
    assert_eq!(
        reads.len(),
        2,
        "the ceiling is two rows, and it is not a suggestion: {}",
        reads.len()
    );
    assert_eq!(
        reads.last().unwrap().wire.complete,
        Some(false),
        "five keys through a ceiling of two is a SHORT answer and the last frame \
         must say so: {:?}",
        reads.last().unwrap().wire.complete
    );
    drop(tmp);
}

/// **A POOL THAT FITS INSIDE ITS CEILING SAYS `complete: true`.**
///
/// The other half of the clause above, and the one that makes the field worth
/// reading: if `complete` were `false` whenever it appeared, a consumer would
/// learn nothing from it. `Tiny` at `detail: keys` is five members under a
/// membership read that answered whole.
///
/// **THE MUTATION.** In `export_members`, change `complete: i + 1 == n &&
/// complete` to `complete: false`.
///
/// Forced red:
/// ```text
/// a read that WAS whole must say so, or the field carries no information:
/// Some(false)
/// ```
#[tokio::test]
async fn a_whole_answer_says_complete_true() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Tiny, m.clone());

    let mut c = hub.connect(64);
    hub.subscribe_detail("tiny", None, 64, None, Detail::Keys)
        .unwrap();
    settle().await;
    let frames = drain(&mut c);
    let last = frames
        .iter()
        .filter(|f| f.wire.members.is_some())
        .next_back()
        .expect("a membership frame");
    assert_eq!(
        last.wire.complete,
        Some(true),
        "a read that WAS whole must say so, or the field carries no information: {:?}",
        last.wire.complete
    );
    drop(tmp);
}

/// **A MEMBERSHIP FRAME IS NOT A ROW-LESS EXPORT.**
///
/// `export` has always emitted `row: None, cells: None` for a key the root does
/// not hold. *"I do not hold this key"* and *"this key exists, and you asked for
/// membership"* are two claims, and the same bytes meaning both is the
/// collapsed-event class that `docs/access-patterns.md` 3.3 counts four bugs of
/// in this path. So a membership frame carries `members` and a row frame never
/// does — a consumer discriminates on a field, never on an absence.
///
/// **THE MUTATION.** In `export_members`, set `members: None` and carry the keys
/// as `row: Some(json!(keys))` instead — the "one frame type is simpler"
/// version. The subscribe is still answered; the discriminator is gone.
///
/// Forced red:
/// ```text
/// membership is discriminated by a FIELD and never by an absence: None
/// ```
#[tokio::test]
async fn a_membership_frame_is_discriminated_by_a_field_not_by_an_absence() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Tiny, m.clone());

    // A ROW-LESS EXPORT, for the comparison: a key this pool does not know.
    let mut a = hub.connect(65);
    hub.subscribe("tiny", Some("not-a-member"), 65, None)
        .unwrap();
    settle().await;
    let rowless = drain(&mut a);
    let rowless = rowless
        .iter()
        .find(|f| f.wire.op == Op::Read)
        .expect("a frame, rather than silence, for a key that is not there");
    assert!(rowless.wire.row.is_none(), "it is row-less");
    assert!(
        rowless.wire.members.is_none(),
        "and it is NOT a membership frame, which is the whole distinction: {:?}",
        rowless.wire.members
    );

    let mut b = hub.connect(66);
    hub.subscribe_detail("tiny", None, 66, None, Detail::Keys)
        .unwrap();
    settle().await;
    let frames = drain(&mut b);
    let page = frames
        .iter()
        .find(|f| f.wire.op == Op::Read)
        .expect("a membership frame");
    assert!(
        page.wire.members.is_some(),
        "membership is discriminated by a FIELD and never by an absence: {:?}",
        page.wire.members
    );
    drop(tmp);
}

/// **`gone` REACHES A MEMBERSHIP SUBSCRIBER**, because `gone` means LEFT THE
/// POOL and a membership subscriber is exactly who is owed that.
///
/// It is the twin of `added` and it keeps its existing meaning for key holders;
/// what changed is the audience. `docs/access-patterns.md` 6.8: a key leaving
/// ONE CONNECTION's held set is not this event and must never be built as one.
///
/// **THE MUTATION.** In `fold.rs`'s `Map::remove`, delete the
/// `if self.members.remove(k) { … }` block — so a key that left the pool keeps
/// its membership for ever, and a roster only ever grows. This test drives the
/// HUB, where the audience is `subs`; `drive`'s half of the gate (which lets a
/// `gone` through to a connection holding no key) is mutated by §1's socket
/// test, where it is observable.
///
/// Forced red:
/// ```text
/// a departure is announced to whoever asked what exists: []
/// ```
/// — the FIRST assertion, because `c` was a member and never a row, so with the
/// membership half of `remove` gone there is nothing left for it to remove and
/// no `gone` is emitted at all.
#[tokio::test]
async fn a_key_that_leaves_the_pool_leaves_the_membership() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Tiny, m.clone());

    let mut c = hub.connect(67);
    hub.subscribe_detail("tiny", None, 67, None, Detail::Keys)
        .unwrap();
    settle().await;
    drain(&mut c);

    m.hub.emit(an_info("part:c"));
    settle().await;
    let frames = drain(&mut c);
    assert!(
        frames
            .iter()
            .any(|f| matches!(f.wire.event, Some(threads::shape::Derived::Gone))
                && f.wire.key.as_deref() == Some("c")),
        "a departure is announced to whoever asked what exists: {frames:?}"
    );

    // And a fresh membership subscribe no longer lists it.
    let mut d = hub.connect(68);
    hub.subscribe_detail("tiny", None, 68, None, Detail::Keys)
        .unwrap();
    settle().await;
    let roster: BTreeSet<String> = drain(&mut d)
        .iter()
        .filter_map(|f| match &f.wire.members {
            Some(threads::shape::MembersPage::Known { keys, .. }) => Some(keys.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(
        !roster.contains("c"),
        "a key that left the pool must leave the MEMBERSHIP, or a roster only \
         ever grows: {roster:?}"
    );
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. THE MAX CONTRACT'S REFUSALS — named, never clamped
// ═════════════════════════════════════════════════════════════════════════════

/// **`detail: keys` TAKES NO KEY**, and the refusal is decided where the request
/// is still a request.
///
/// Membership is a question about what EXISTS; a key is an answer to it.
/// Answering `{shape, key, detail: keys}` with a one-key roster would be a
/// second spelling of the keyed subscribe that already exists.
///
/// **THE MUTATION.** Delete the `Detail::Keys && key.is_some()` guard in
/// `Hub::subscribe_detail`. The subscribe is then accepted and the task exports
/// the whole roster to a consumer that named one key.
///
/// Forced red:
/// ```text
/// membership is a question about the POOL, so naming a key is a request
/// nobody can answer twice: Ok(1)
/// ```
#[tokio::test]
async fn detail_keys_with_a_key_is_refused_on_the_request_id() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Tiny, m.clone());

    let mut c = hub.connect(69);
    match hub.subscribe_detail("tiny", Some("a"), 69, None, Detail::Keys) {
        Err(ShapeError::BadDetail(msg)) => {
            assert!(msg.contains("tiny"), "the refusal names the shape: {msg}");
        }
        other => panic!(
            "membership is a question about the POOL, so naming a key is a request \
             nobody can answer twice: {other:?}"
        ),
    }
    settle().await;
    assert!(
        drain(&mut c).is_empty(),
        "a refused subscribe creates no subscription, so no frame follows it"
    );
    drop(tmp);
}

/// **A KEYLESS `detail: keys` IS ANSWERED WHERE A KEYLESS `detail: rows` IS
/// REFUSED**, on the same shape, in the same call.
///
/// This is the change in one assertion. `KEY_SCOPE::OnDemand` refused a keyless
/// subscribe outright, so there was no way to ask a pool what existed without
/// naming a key you did not have. The refusal survives where it is right — the
/// 20 MB dump cannot happen by accident — and the coarser ask is served.
///
/// **THE MUTATION.** In `Hub::subscribe_detail`, drop `&& detail == Detail::Rows`
/// from the `KeyRequired` guard, so membership is refused along with rows.
///
/// Forced red:
/// ```text
/// a pool that cannot afford to hand you its rows can still afford to tell you
/// what is in it: Err(KeyRequired { shape: "tiny", max_rows: 500 })
/// ```
#[tokio::test]
async fn keys_is_served_where_rows_is_refused() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Tiny, m.clone());

    match hub.subscribe("tiny", None, 70, None) {
        Err(ShapeError::KeyRequired { shape, max_rows }) => {
            assert_eq!(shape, "tiny");
            // Rule 2.8: the refusal NAMES the ceiling, so the consumer learns
            // what to ask instead rather than only that it asked wrongly.
            assert_eq!(max_rows, 500);
        }
        other => panic!("a keyless dump of a per-key pool is still refused: {other:?}"),
    }
    let got = hub.subscribe_detail("tiny", None, 70, None, Detail::Keys);
    assert!(
        got.is_ok(),
        "a pool that cannot afford to hand you its rows can still afford to tell \
         you what is in it: {got:?}"
    );
    drop(tmp);
}

/// **A PRODUCER THAT CANNOT ENUMERATE SAYS SO, AND IS NOT AN EMPTY POOL.**
///
/// `docs/access-patterns.md` 6.10: no `MEMBERS: bool` flag, because the
/// three-valued state already says it. `Tiny` is registered but its membership
/// is asked for on a shape whose `hydrates_members` is the chassis DEFAULT —
/// which refuses — so the roster comes back `NotHydrated`, not `[]`.
///
/// The distinction is the one `machine_login.unhosted` was a lesson about:
/// "there is nothing here" and "I could not look" render differently, and a
/// consumer must not have to infer which it got from an absence.
///
/// **THE MUTATION**, in the PRODUCT: in `ShapeTask::export_members`, answer a
/// `Membership::NotHydrated` with `vec![MembersPage::Known{keys: Vec::new(),
/// complete: true, next: None}]` — the "empty is empty" collapse.
///
/// Forced red:
/// ```text
/// a pool that could not be read is NOT an empty pool: Known { keys: [],
/// complete: true }
/// ```
#[tokio::test]
async fn a_pool_that_cannot_be_enumerated_answers_not_hydrated() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Mute, m.clone());

    let mut c = hub.connect(71);
    hub.subscribe_detail("mute", None, 71, None, Detail::Keys)
        .expect("the ask is legal; the answer is what says it could not be served");
    settle().await;
    let frames = drain(&mut c);
    let page = frames
        .iter()
        .find_map(|f| f.wire.members.clone())
        .expect("a membership frame, rather than silence");
    match page {
        threads::shape::MembersPage::NotHydrated { .. } => {}
        other => panic!("a pool that could not be read is NOT an empty pool: {other:?}"),
    }
    drop(tmp);
}

/// **A `Whole` POOL ANSWERS MEMBERSHIP OUT OF ITS HELD KEYS, WITH NO EXTRA
/// READ.**
///
/// `Root::members`' default is *"the keys I hold, complete"*, and that is
/// exactly right for a root that reads its whole key set at init: there is
/// nothing to ask the producer for, because holding IS membership there. It is
/// why `accounts`, `narration` and `threads/summary` gained `detail: keys`
/// without implementing anything, and why `hydrates_members` is reached only on
/// a `PerKey` shape.
///
/// **THE MUTATION.** In `fold.rs`, change `Root::members`' default body to
/// `Membership::NotHydrated { reason: "unread".to_string() }` — the "a producer
/// must opt in" version, which would make every `Whole` shape answer `?` to a
/// question it can answer exactly.
///
/// Forced red:
/// ```text
/// a `Whole` root's members ARE its held keys, with no producer read at all:
/// NotHydrated { reason: "unread" }
/// ```
#[tokio::test]
async fn a_whole_pool_answers_membership_from_its_held_keys() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Fleet, m.clone());

    let mut c = hub.connect(72);
    hub.subscribe_detail("fleet", None, 72, None, Detail::Keys)
        .expect("membership is answerable on every shape, not only per-key ones");
    settle().await;
    let frames = drain(&mut c);
    let page = frames
        .iter()
        .find_map(|f| f.wire.members.clone())
        .expect("a membership frame");
    match page {
        threads::shape::MembersPage::Known { keys, complete, .. } => {
            assert_eq!(keys.len(), 5, "every key it read at init");
            assert!(complete, "and it read them all");
        }
        other => panic!(
            "a `Whole` root's members ARE its held keys, with no producer read \
             at all: {other:?}"
        ),
    }
    // **AND STILL NO ROWS**, even though this pool holds every one of them —
    // `detail` is what decides, not what the root happens to have.
    assert!(
        frames.iter().all(|f| f.wire.row.is_none()),
        "`detail: keys` is membership on every shape: {frames:?}"
    );
    drop(tmp);
}

/// **A RE-INIT REPAIRS A MEMBERSHIP MIRROR TOO, AND IT HOLDS NO KEY.**
///
/// `reinit_now` throws the root away, re-hydrates, and repairs every mirror by
/// re-exporting each key to `holders_of(k)`. A `detail: keys` subscriber is in
/// no `holders_of` set at all, so without a second audience it would receive
/// NOTHING from a re-init — no roster, no `added`, not even a frame carrying the
/// new `epoch.reinit` — while the state behind its roster was discarded and
/// rebuilt. That is the stale-mirror class the death export and `epoch.reinit`
/// exist to close, arriving through the one door with no repair on it.
///
/// **THE MUTATION.** Delete the `if !self.member_subs.is_empty() { … }` block
/// from `ShapeTask::reinit_now`.
///
/// Forced red:
/// ```text
/// a re-init must repair a membership mirror, which holds no key and is
/// therefore in nobody's `holders_of`: []
/// ```
#[tokio::test]
async fn a_reinit_repairs_a_membership_mirror() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    // Capacity 2, so a burst issued without awaiting overflows it — the same
    // technique `nesting.rs` uses to force a re-init deterministically.
    hub.core().register(Tiny, m.clone(), 2);

    let mut c = hub.connect(73);
    hub.subscribe_detail("tiny", None, 73, None, Detail::Keys)
        .unwrap();
    settle().await;
    drain(&mut c);

    for _ in 0..40 {
        m.hub.emit(an_info("flood"));
    }
    settle().await;
    settle().await;

    let info = hub
        .list()
        .into_iter()
        .find(|i| i.name == "tiny")
        .expect("the shape");
    assert!(
        info.epoch.lost > 0 && info.epoch.reinit == 1,
        "the scenario requires a real overflow and exactly one re-init: {:?}",
        info.epoch
    );

    let frames = drain(&mut c);
    let pages: Vec<&Arc<threads::shape::ShapeFrame>> =
        frames.iter().filter(|f| f.wire.members.is_some()).collect();
    assert!(
        !pages.is_empty(),
        "a re-init must repair a membership mirror, which holds no key and is \
         therefore in nobody's `holders_of`: {:?}",
        frames.iter().map(|f| f.wire.op).collect::<Vec<_>>()
    );
    assert_eq!(
        pages[0].wire.epoch.reinit, 1,
        "and the repair carries the NEW era, which is what tells the mirror the \
         roster it held belonged to a previous one"
    );
    assert!(
        pages.iter().all(|f| f.wire.subscribes.is_empty()),
        "a re-init's re-export answers no `shapes.subscribe` — naming one would \
         tell a consumer its own call had been answered twice: {:?}",
        pages.iter().map(|f| f.wire.subscribes.clone()).collect::<Vec<_>>()
    );
    drop(tmp);
}

/// `PerKey`, and no `hydrates_members` at all — so the chassis default refuses,
/// which is what keeps the trait method additive for every shape written before
/// it existed.
struct Mute;
impl Shape<Input> for Mute {
    const NAME: &'static str = "mute";
    const CELLS: Cells = N_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const HYDRATE: Hydrate = Hydrate::PerKey;
    type Producer = ThreadManager;
    type Root = Pool;
    fn root(&self) -> Pool {
        Pool::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, _: &mut Pool, _: Timestamp) {}
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, Pool>) {}
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
