//! # The overflow repair, from the seat of the consumer that overflowed
//!
//! A connection's shape channel is a bounded `mpsc`. When it drops frames,
//! `drive` repairs the gap the only way rule 2.5 allows — a re-subscribe, which
//! re-exports. Two things were wrong with that repair, and both of them are
//! invisible from anywhere but the consumer's seat:
//!
//! 1. **IT MADE THE LAGGING CONSUMER WORSE.** The repair went through
//!    `Hub::subscribe`, whose whole contract is *asking nothing means the
//!    ceiling* (rule 2.8), so it arrived at `do_subscribe` carrying
//!    `Retain::Max` and `Beat::Max` — which `do_subscribe` then wrote over the
//!    connection's negotiated ask, deliberately, because for a CONSUMER'S second
//!    subscribe the last ask is the current ask. A consumer that had negotiated
//!    `retain: 1` and then fell behind was silently upgraded to the largest
//!    frames and the fastest cadence the contract allows. The repair for falling
//!    behind was more load.
//!
//! 2. **IT NEVER REPAIRED A ROSTER.** The loop walked `shape_subs` — this
//!    connection's `(shape, key)` gate set. A `detail: keys` subscriber HOLDS NO
//!    KEY (that is the whole point of the parameter), so it is in no entry of
//!    that set and got nothing at all: no roster, no `added`, not one frame,
//!    while its membership mirror silently diverged. `reinit_now` had already
//!    learned this lesson and repairs `member_subs` explicitly; the lag path did
//!    not.
//!
//! **Both tests are from the consumer's position over a REAL WEBSOCKET, and the
//! overflow is a REAL overflow** — the consumer stops reading its socket, the
//! producer emits past the connection channel's capacity, and the consumer then
//! reads what survived. That is deliberate and it is why the existing tests
//! missed this: `retention.rs` proves `retain` on a frame the hub built, and a
//! hub-level test cannot reach `drive`'s repair loop at all. The gate set and
//! the repair loop are both on the other side of the transport.
//!
//! **Neither test asserts that a book is empty.** Each asserts a FRAME THAT
//! ARRIVED at a consumer holding the subject, and what that frame said.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use threads::config::Config;
use threads::event::Push;
use threads::shape::{
    Hub, Input, Outbox, Retain, Root, Sequence, Shape, Stamped, Timestamp, Tracking, Tx,
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

/// **ENOUGH FRAMES TO REALLY OVERFLOW.** The per-connection channel is
/// `CONN_FRAME_CAPACITY` (1,024) and the socket buffers hold some hundreds more
/// before `drive`'s send blocks, so the burst has to clear both. Each emit here
/// produces two frames (a transition and a debounced row), and the margin is
/// deliberately generous: a burst that merely ALMOST overflows makes a vacuous
/// green, which is why both tests below also assert that the repair fired.
const BURST: usize = 4_000;

/// Emitted in chunks with a yield between them, so the SHAPE's own FIFO never
/// overflows. A FIFO overflow triggers `reinit_now`, which repairs rosters and
/// re-exports held keys for its own reasons — it would make both tests below
/// pass for the wrong reason. Each asserts `epoch.lost == 0` as the guard.
/// `per` and `pause` are per CALLER, not a constant, and the reason is the whole
/// difficulty of writing this test: a `line:` fold appends to one existing
/// window, while a `join:` fold MINTS A KEY — a new `Chan`, a new `Sequence`, an
/// `added` and a row — so the task's cost per item differs by an order of
/// magnitude and one pacing does not clear both. Minting at the `line:` pace
/// overflows the FIFO (measured: 1,014 lost), which re-inits the shape and makes
/// the roster test pass for `reinit_now`'s reasons instead of the repair's.
///
/// **PACED BY THE FOLD, NOT BY THE CLOCK** (R9.F2). The pause used to be the
/// whole pacing: `per` emits, sleep `pause` ms, repeat — a bet that the
/// shape's task folds `per` inputs in `pause` ms. In a loaded `-p threads` run
/// it does not (the two tests here burst on 4+4 workers at once, beside every
/// other test binary's CPU), the FIFO overflowed, and
/// `the_repair_reaches_a_membership_subscriber_too` went red on its `lost == 0`
/// guard in the full suite and green alone. Now each chunk waits until the
/// shape has FOLDED everything emitted before it ([`Noisy::folded`] counts
/// fold calls), so the FIFO never holds more than two chunks — 2 × `per` ≤ 400,
/// against `FIFO_CAPACITY` 1,024 — whatever the machine's load. The
/// per-CONNECTION channel still overflows, because nothing reads the socket;
/// that overflow is the condition under test and it is untouched. `pause`
/// stays as the minimum yield between chunks.
async fn burst(
    m: &ThreadManager,
    folded: &AtomicUsize,
    per: usize,
    pause: u64,
    mut mint: impl FnMut(usize) -> Push,
) {
    let started = std::time::Instant::now();
    let caught_up = |want: usize| async move {
        while folded.load(Ordering::SeqCst) < want {
            assert!(
                started.elapsed() < BURST_DEADLINE,
                "the shape folded {} of {want} inputs in {BURST_DEADLINE:?}",
                folded.load(Ordering::SeqCst)
            );
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    };
    for chunk in 0..(BURST / per) {
        caught_up(chunk * per).await;
        for i in 0..per {
            m.hub.emit(mint(chunk * per + i));
        }
        tokio::time::sleep(std::time::Duration::from_millis(pause)).await;
    }
    caught_up(BURST).await;
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
}

/// A whole burst, folded, on a loaded machine. Measured alone it takes a few
/// seconds; this is the point past which the shape task is not merely slow.
const BURST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(180);

fn an_info(reason: String) -> Push {
    Push::Info {
        domain: "test".into(),
        reason,
        account: None,
        detail: None,
        at: chrono::Utc::now().to_rfc3339(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// the test shape — one windowed cell, and keys minted by the fold
// ─────────────────────────────────────────────────────────────────────────────

/// The window, and the shape's declared ceiling. Big enough that `retain: 1` is
/// visibly narrower than `Retain::Max`, which is the whole observable.
const CAPACITY: usize = 8;

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

#[derive(Default)]
struct NoisyRoot {
    chans: threads::shape::Map<String, Chan>,
}

impl Root for NoisyRoot {
    fn keys(&self) -> Vec<String> {
        self.chans.keys().cloned().collect()
    }
    fn row(&self, k: &str) -> Option<Value> {
        self.row_at(k, Retain::Max)
    }
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

/// **ONE SHAPE THAT CAN BOTH FLOOD A KEY AND MINT KEYS.**
///
/// `line:<text>` appends to the `log` channel's window; `join:<k>` mints the key
/// `k`. The first is what makes a KEYED subscriber lag, the second is what makes
/// a MEMBERSHIP subscriber lag, and both tests need a shape that can do its own
/// half without a second fixture.
struct Noisy {
    /// Every fold call, counted, so [`burst`] can pace on the shape's real
    /// progress instead of on a sleep.
    folded: Arc<AtomicUsize>,
}

// The cells this file's test roots emit, declared — `Shape::CELLS` is required
// (see `shapewire::cell`), and a test shape declares what its `Root::cells`
// actually spells.
shapewire::declare_cells! {
    LINES_CELLS = [("lines", shapewire::operation::DType::Scalar(shapewire::operation::Scalar::Int), shapewire::operation::Presence::Required, [])]
}

impl Shape<Input> for Noisy {
    const NAME: &'static str = "noisy";
    const CELLS: Cells = LINES_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    const MAX_RETAINED_ITEMS: usize = CAPACITY;
    type Producer = ThreadManager;
    type Root = NoisyRoot;
    fn root(&self) -> NoisyRoot {
        NoisyRoot::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut NoisyRoot, as_of: Timestamp) {
        root.chans
            .key("log".to_string())
            .lines
            .mark_since_init(as_of);
        root.chans.mark_read(as_of, true);
        root.chans.mark_members_read(as_of, true);
        let _ = root.drain();
    }
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, NoisyRoot>) {
        self.folded.fetch_add(1, Ordering::SeqCst);
        let at = tx.at();
        let Input::Push(Push::Info { reason, .. }) = &input.value else {
            return;
        };
        if let Some(text) = reason.strip_prefix("line:") {
            tx.chans.key("log".to_string()).lines.emit(
                at,
                Line {
                    line: text.to_string(),
                },
            );
        }
        if let Some(k) = reason.strip_prefix("join:") {
            tx.chans.key(k.to_string()).lines.mark_since_init(at);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// the socket
// ─────────────────────────────────────────────────────────────────────────────

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

async fn serve(m: &ThreadManager) -> SocketAddr {
    let chassis = threads::chassis::chassis(Arc::new(m.clone()));
    let listener = shapewire::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (addr, h) = chassis
        .serve_on_with_shutdown(listener, std::future::pending::<()>())
        .unwrap();
    std::mem::forget(h);
    addr
}

/// `epoch.lost` for one shape, off `shapes.list`. The guard every test here
/// needs: a FIFO overflow re-inits the shape, and `reinit_now` re-exports held
/// keys and repairs rosters for its OWN reasons — so a nonzero `lost` would mean
/// the repair under test was never the thing that fired.
async fn lost(c: &mut Client, shape: &str) -> u64 {
    let r = c.request("lost", "shapes/list", json!({})).await;
    r["outcome"]["payload"]["shapes"]
        .as_array()
        .expect("shapes")
        .iter()
        .find(|s| s["name"] == json!(shape))
        .expect("the shape")["epoch"]["lost"]
        .as_u64()
        .expect("lost is a number")
}

fn lines_of(frame: &Value) -> Vec<String> {
    frame["row"]["lines"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v["line"].as_str().map(str::to_string))
        .collect()
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. THE REPAIR KEEPS THE ASK THE CONSUMER NEGOTIATED
// ═════════════════════════════════════════════════════════════════════════════

/// **A CONSUMER THAT ASKED FOR ONE ITEM STILL GETS ONE ITEM AFTER IT LAGS.**
///
/// The consumer subscribes with `retain: 1`, stops reading, is flooded past its
/// channel's capacity, and then reads. The repair's re-export must still be cut
/// to one item — because `retain: 1` is what this connection asked for and
/// nothing since has unasked it.
///
/// Under the defect the re-export carries the whole eight-item window: the
/// repair went through `Hub::subscribe`, which means *the ceiling*, and
/// `do_subscribe` wrote that ceiling over the connection's recorded ask. Eight
/// times the bytes, for the consumer that had just failed to keep up with one.
///
/// **THE MUTATION.** In `shapewire/src/server.rs`'s overflow arm, put the
/// repair back through `Hub::subscribe`:
/// ```text
/// let _ = producer.hub().subscribe(shape, Some(key), conn_id, None);
/// ```
///
/// Forced red:
/// ```text
/// thread 'the_repair_for_a_lagging_consumer_keeps_its_negotiated_retain' panicked:
/// the re-export after an overflow must still honour `retain: 1` — this
/// connection asked for one item and the repair is not an ask that unasks it.
/// got 8 items: ["l3992", "l3993", "l3994", "l3995", "l3996", "l3997", "l3998",
/// "l3999"]
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_repair_for_a_lagging_consumer_keeps_its_negotiated_retain() {
    let tmp = tempfile::tempdir().unwrap();
    // THE CLOCK SEAM. Every `Hub::emit` stamps with the clock and every cell
    // refuses an input at or before its `folded_at`, so two emits in one
    // microsecond mean the second is DROPPED — which is what an optimized
    // build does to a tight emit loop. See `src/clock.rs`.
    threads::clock::testing::ticking();
    let m = build_manager(config(tmp.path())).unwrap();
    let hub: Arc<Hub> = m.hub.clone();
    let folded = Arc::new(AtomicUsize::new(0));
    hub.register(Noisy { folded: folded.clone() }, m.clone());
    let addr = serve(&m).await;

    let mut c = Client::connect(addr).await;
    let r = c
        .request(
            "1",
            "shapes/noisy/subscribe",
            json!({"key":"log","retain":1}),
        )
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");

    // The export that answers the subscribe, already cut to the ask.
    let first = c.drain().await;
    let exports: Vec<&Value> = first
        .iter()
        .filter(|f| f["type"] == "shape" && f["op"] == json!("read") && f.get("row").is_some())
        .collect();
    assert_eq!(
        exports.len(),
        1,
        "exactly one export answers one keyed subscribe: {first:#?}"
    );

    // ── NOW STOP READING, AND FLOOD ──────────────────────────────────────────
    //
    // Nothing below calls `c.next()`, so the socket's buffers fill, `drive`
    // blocks on its send, and the bounded per-connection channel behind it
    // overflows and counts the drops. That is the real condition — not a
    // simulated one — and it is the only condition under which `drive` repairs.
    burst(&m, &folded, 200, 3, |i| an_info(format!("line:l{i}"))).await;

    // ── AND READ WHAT SURVIVED ───────────────────────────────────────────────
    let after = c.drain().await;
    let repairs: Vec<&Value> = after
        .iter()
        .filter(|f| f["type"] == "shape" && f["op"] == json!("read") && f.get("row").is_some())
        .collect();
    assert!(
        !repairs.is_empty(),
        "the overflow must have been real and the repair must have fired — a \
         re-export is the only thing that sends an `op: read` here, and without \
         one this test proves nothing. frames seen: {}",
        after.len()
    );
    assert_eq!(
        lost(&mut c, "noisy").await,
        0,
        "the SHAPE's FIFO must not have overflowed: `reinit_now` re-exports held \
         keys for its own reasons, and it would make this test pass without the \
         repair under test ever running"
    );

    let got = lines_of(repairs.last().unwrap());
    assert_eq!(
        got.len(),
        1,
        "the re-export after an overflow must still honour `retain: 1` — this \
         connection asked for one item and the repair is not an ask that unasks \
         it. got {} items: {got:?}",
        got.len()
    );
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. AND IT REPAIRS A ROSTER, WHICH IT NEVER DID
// ═════════════════════════════════════════════════════════════════════════════

/// **A MEMBERSHIP SUBSCRIBER THAT LAGS GETS ITS ROSTER BACK.**
///
/// A `detail: keys` consumer holds no key, so it appears in no `(shape, key)`
/// entry of `drive`'s gate set — and the repair loop walked exactly that set. It
/// therefore received NOTHING from an overflow: not a roster, not an `added`,
/// not one frame carrying a fresh `epoch`, while the keys it was missing had
/// already been minted and their `added` frames dropped. The roster it went on
/// believing was a roster from before the gap, with nothing to tell it
/// otherwise.
///
/// The assertion is that the repaired roster NAMES THE KEYS THAT ARRIVED DURING
/// THE GAP. "A members frame arrived" alone would pass against a roster that
/// re-states the pre-gap membership, which is the stale mirror this closes.
///
/// **THE MUTATION.** In `shapewire/src/server.rs`'s overflow arm, delete the
/// `shape_members` loop, leaving only the `shape_subs` one.
///
/// Forced red:
/// ```text
/// thread 'the_repair_reaches_a_membership_subscriber_too' panicked:
/// a membership subscriber that overflowed must be handed a fresh roster — it
/// holds no key, so the `shape_subs` loop reaches it with nothing and its mirror
/// silently keeps a roster from before the gap. members frames after the burst: 0
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_repair_reaches_a_membership_subscriber_too() {
    let tmp = tempfile::tempdir().unwrap();
    threads::clock::testing::ticking();
    let m = build_manager(config(tmp.path())).unwrap();
    let hub: Arc<Hub> = m.hub.clone();
    let folded = Arc::new(AtomicUsize::new(0));
    hub.register(Noisy { folded: folded.clone() }, m.clone());
    let addr = serve(&m).await;

    let mut c = Client::connect(addr).await;
    let r = c
        .request("1", "shapes/noisy/subscribe", json!({"detail":"keys"}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");

    // The roster of what existed when it asked: just `log`.
    let first = c.drain().await;
    assert!(
        first
            .iter()
            .any(|f| f["type"] == "shape" && f.get("members").is_some()),
        "a `detail: keys` subscribe is answered with a membership frame: {first:#?}"
    );

    // ── STOP READING, AND MINT KEYS UNTIL THE CHANNEL OVERFLOWS ──────────────
    burst(&m, &folded, 25, 8, |i| an_info(format!("join:k{i:05}"))).await;

    // ── THE REPAIRED ROSTER ──────────────────────────────────────────────────
    let after = c.drain().await;
    let pages: Vec<&Value> = after
        .iter()
        .filter(|f| f["type"] == "shape" && f.get("members").is_some())
        .collect();
    assert!(
        !pages.is_empty(),
        "a membership subscriber that overflowed must be handed a fresh roster — \
         it holds no key, so the `shape_subs` loop reaches it with nothing and \
         its mirror silently keeps a roster from before the gap. members frames \
         after the burst: {}",
        pages.len()
    );
    assert_eq!(
        lost(&mut c, "noisy").await,
        0,
        "the SHAPE's FIFO must not have overflowed: `reinit_now` repairs \
         `member_subs` for its own reasons and would make this pass without the \
         repair under test"
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
    let last = format!("k{:05}", BURST - 1);
    assert!(
        roster.contains(&last),
        "the repaired roster must name the keys minted DURING the gap — a roster \
         that only re-states what the consumer already had is the stale mirror \
         this repair exists to close. looking for {last}, roster has {} keys",
        roster.len()
    );
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
