//! # A re-init has to be able to SHRINK a mirror
//!
//! `reinit_now` throws the root away, re-hydrates, and repairs every mirror. It
//! repaired them in one direction only: it re-exported the keys the NEW root
//! holds and emitted no `gone` at all. And `mirror::Roster::page` UNIONS — it
//! must, because `export_members` emits a walk as N frames and only the last can
//! claim `complete`, so a page that replaced would leave a consumer holding page
//! N alone.
//!
//! Those two together are a hole: **a key that existed before a re-init and not
//! after it survived in the consumer's roster, inside a page marked
//! `complete: true`, with its row still held and every cell still `Known`.** The
//! mirror stated, with full confidence, that a key exists and that a row is
//! current, and nothing on the wire could ever correct it. The consumer's only
//! recourse was to notice and re-subscribe — detect-and-retry, which rule 2.5
//! exists to keep out of clients.
//!
//! `pool_membership.rs::a_reinit_repairs_a_membership_mirror` is the only
//! existing re-init test and it floods ONE key set: `Tiny::hydrates_members`
//! answers `a b c d e` on every init, so the before-set and the after-set are
//! equal by construction and that test cannot see this no matter what it
//! asserts. The shape here answers a DIFFERENT key set on its second init, which
//! is the whole fixture.
//!
//! ## Gone is not the same as not-held, and that is half the fix
//!
//! Emitting `gone` is not enough on its own, because the mirror's `gone` handler
//! merely REMOVED the row — after which `Mirror::cell` answered
//! `NotHydrated { reason: "unseen" }`, *nobody has told me*, about a key the
//! producer had just explicitly unsaid. Conflating a removal with an absence is
//! this repo's house disease (`CellState`'s three values, `Enumeration`'s three,
//! `MembersPage`'s three) arriving one level down. `Mirror::key_state` is the
//! third value: `Held | Gone | Unknown`, and the test below asserts all three in
//! one pass, because a fix that answered `Gone` for everything it is not holding
//! would pass any test that only looked at the vanished key.
//!
//! Consumer position, over a real websocket, into a real `Mirror`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use shapewire::mirror::{Enumeration, KeyState, Mirror};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::event::Push;
use threads::shape::{
    Hub, Input, Outbox, Root, Shape, Stamped, Timestamp, Tracking, Tx,
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

/// **WHAT THE NEXT INIT WILL READ.** The test flips it between the first init
/// and the re-init, which is the one thing the existing re-init test cannot do:
/// a shape whose hydrate is a constant cannot shrink.
type Source = Arc<Mutex<Vec<&'static str>>>;

#[derive(Default)]
struct PoolRoot {
    rows: threads::shape::Map<String, Cell>,
}

#[derive(Default)]
struct Cell {
    n: threads::shape::Register<u64>,
}

impl Root for PoolRoot {
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

/// **A `Whole` POOL WHOSE KEY SET CAN CHANGE BETWEEN EPOCHS.**
///
/// `hydrates` reads [`Source`] — so the test decides what the next init finds.
/// The fold deliberately touches no key: every key change in this test comes
/// from a RE-HYDRATE, which is the door that had no removal on it.
struct Shrinking(Source);

// The cells this file's test roots emit, declared — `Shape::CELLS` is required
// (see `shapewire::cell`), and a test shape declares what its `Root::cells`
// actually spells.
shapewire::declare_cells! {
    N_CELLS = [("n", shapewire::operation::DType::Scalar(shapewire::operation::Scalar::Int), shapewire::operation::Presence::Required, [])]
}

impl Shape<Input> for Shrinking {
    const NAME: &'static str = "shrinking";
    const CELLS: Cells = N_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    type Producer = ThreadManager;
    type Root = PoolRoot;
    fn root(&self) -> PoolRoot {
        PoolRoot::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, root: &mut PoolRoot, as_of: Timestamp) {
        for k in self.0.lock().unwrap().iter() {
            root.rows.key(k.to_string()).n.read(as_of, 1);
        }
        root.rows.mark_read(as_of, true);
        root.rows.mark_members_read(as_of, true);
        let _ = root.drain();
    }
    /// Nothing. The flood below exists only to overflow the FIFO and force the
    /// re-init; if it changed the root, the test would be about the fold.
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, PoolRoot>) {}
}

fn an_info(reason: &str) -> Push {
    Push::Info {
        domain: "test".into(),
        reason: reason.into(),
        account: None,
        detail: None,
        at: chrono::Utc::now().to_rfc3339(),
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
    /// Reads until the socket goes quiet, applying every `shape` frame to the
    /// mirror. The mirror is the assertion surface: this test is about what a
    /// consumer BELIEVES after the epoch changed under it.
    async fn drain_into(&mut self, m: &mut Mirror) -> usize {
        let mut n = 0;
        while let Some(v) = self.next().await {
            if v["type"] == "shape" {
                m.accept(&v);
                n += 1;
            }
        }
        n
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

// ═════════════════════════════════════════════════════════════════════════════

/// **A KEY THAT DOES NOT COME BACK FROM A RE-INIT LEAVES THE MIRROR, AND SAYS
/// WHY.**
///
/// The consumer holds two rows and a `complete: true` roster. The producer
/// re-inits onto a root that holds only one of the two keys. Afterwards the
/// consumer must believe exactly three things, and the third is the one that
/// makes the fix a distinction rather than a deletion:
///
/// * `a` is `Held` and in the roster — it survived.
/// * `b` is `Gone` and NOT in the roster, which is still `complete` — it was
///   unsaid, and the consumer knows it was unsaid.
/// * `c`, which nobody has ever mentioned, is `Unknown` — *not* `Gone`. A fix
///   that answered `Gone` for everything it does not hold would satisfy the
///   second bullet and be a worse lie than the bug.
///
/// **THE MUTATION.** Delete the `if !before.is_empty() { … }` block (the
/// `departed` loop) from `ShapeTask::reinit_now` in `shapewire/src/hub.rs`.
///
/// Forced red:
/// ```text
/// thread 'a_reinit_that_drops_a_key_shrinks_the_consumers_mirror' panicked:
/// a key the re-init did not bring back must leave the mirror as GONE — the
/// roster UNIONS, so withholding it from the new page cannot remove it and
/// nothing else on the wire can. key_state(b) = Held, roster = ["a", "b"]
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reinit_that_drops_a_key_shrinks_the_consumers_mirror() {
    let tmp = tempfile::tempdir().unwrap();
    // THE CLOCK SEAM. Every `Hub::emit` stamps with the clock and every cell
    // refuses an input at or before its `folded_at`, so two emits in one
    // microsecond mean the second is DROPPED — which is what an optimized
    // build does to a tight emit loop. See `src/clock.rs`.
    threads::clock::testing::ticking();
    let m = build_manager(config(tmp.path())).unwrap();
    let source: Source = Arc::new(Mutex::new(vec!["a", "b"]));
    let hub: Arc<Hub> = m.hub.clone();
    // FIFO capacity 2, so a burst issued without awaiting overflows it and the
    // task re-inits — the same deterministic technique `nesting.rs` and
    // `pool_membership.rs` use.
    hub.core()
        .register(Shrinking(source.clone()), m.clone(), 2);

    let chassis = threads::chassis::chassis(Arc::new(m.clone()));
    let listener = shapewire::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (addr, h) = chassis
        .serve_on_with_shutdown(listener, std::future::pending::<()>())
        .unwrap();
    std::mem::forget(h);

    let mut c = Client::connect(addr).await;
    let mut mirror = Mirror::new("shrinking");

    // ── THE ROSTER, AND BOTH ROWS ────────────────────────────────────────────
    //
    // Two subscribes on one connection: `detail: keys` for the roster (which is
    // the only thing that can be `complete: true`) and a keyless `detail: rows`
    // for the rows. A membership subscriber holds no key and a row subscriber has
    // no complete roster, and the defect needs both halves to be visible.
    for (id, params) in [
        ("1", json!({"detail":"keys"})),
        ("2", json!({})),
    ] {
        let r = c
            .request(id, "shapes/shrinking/subscribe", params)
            .await;
        assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
        c.drain_into(&mut mirror).await;
    }
    assert_eq!(
        mirror.key_state("a"),
        KeyState::Held,
        "the fixture requires both rows mirrored first"
    );
    assert_eq!(mirror.key_state("b"), KeyState::Held);
    assert_eq!(
        mirror.members().enumeration(),
        Enumeration::Known { complete: true },
        "and a roster the producer called WHOLE — which is what makes the stale \
         key a confident lie rather than a gap: {:?}",
        mirror.members()
    );

    // ── THE WORLD CHANGES, AND THE SHAPE RE-INITS ONTO IT ────────────────────
    source.lock().unwrap().retain(|k| *k != "b");
    for _ in 0..40 {
        m.hub.emit(an_info("flood"));
    }
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    let info = hub
        .list()
        .into_iter()
        .find(|i| i.name == "shrinking")
        .expect("the shape");
    assert!(
        info.epoch.lost > 0 && info.epoch.reinit == 1,
        "the scenario requires a real overflow and exactly one re-init: {:?}",
        info.epoch
    );

    let n = c.drain_into(&mut mirror).await;
    assert!(
        n > 0,
        "a re-init must reach a mirror at all — that is \
         `pool_membership.rs::a_reinit_repairs_a_membership_mirror`'s ground, and \
         this test stands on it"
    );

    // ── WHAT THE CONSUMER BELIEVES NOW ───────────────────────────────────────
    assert_eq!(
        mirror.key_state("b"),
        KeyState::Gone,
        "a key the re-init did not bring back must leave the mirror as GONE — the \
         roster UNIONS, so withholding it from the new page cannot remove it and \
         nothing else on the wire can. key_state(b) = {:?}, roster = {:?}",
        mirror.key_state("b"),
        mirror.members().keys()
    );
    assert_eq!(
        mirror.key_state("a"),
        KeyState::Held,
        "and the key that DID come back is untouched"
    );
    assert_eq!(
        mirror.key_state("c"),
        KeyState::Unknown,
        "**GONE IS NOT NOT-HELD.** A key nobody has ever mentioned is Unknown, \
         not Gone: if this answered Gone the mirror would be claiming the producer \
         removed something it never spoke of, which is a worse lie than the stale \
         row this fix deletes"
    );
    assert_eq!(
        mirror.vanished(),
        vec!["b".to_string()],
        "and exactly one key was unsaid"
    );

    // The roster agrees, and still claims to be whole — a shrunk roster that went
    // `complete: false` would have traded one wrong answer for a useless one.
    assert_eq!(
        mirror.members().keys(),
        vec!["a".to_string()],
        "the roster shrank too: {:?}",
        mirror.members()
    );
    assert!(
        mirror.members().complete(),
        "and it is still a WHOLE answer: {:?}",
        mirror.members()
    );

    // And the row really is gone, not merely unlisted.
    assert_eq!(mirror.keys(), vec!["a".to_string()]);
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
