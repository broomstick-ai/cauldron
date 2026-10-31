//! # The chassis server's own contract
//!
//! `docs/chassis-writes.md` §10.4 step 0: *"Each of the five invariants above
//! becomes a test against TODAY's `drive` … A behaviour that has no failing test
//! before the move has no way to report that it did not survive it."* Every
//! invariant here was a COMMENT in `server.rs::drive` and is now a test against
//! the CHASSIS loop; `golden_session.rs` is what says the two loops agree.
//!
//! Every test in this file was **forced red before it was made green**, and the
//! exact mutation is named in its own docstring. A green that has never failed
//! is not evidence — this crate has shipped three tests that passed against
//! broken code because they described the implementation rather than the
//! contract.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use shapewire::envelope::codes;
use shapewire::server::{Bearers, CHASSIS_METHODS};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::event::{Fact, Push};
use threads::server::WS_METHODS;
use threads::shape::{
    CellState, Hub, Input, Outbox, Root, Shape, Stamped, Timestamp, Tracking, Tx,
};
use threads::{build_manager, ThreadManager};
use shapewire::cell::Cells;

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

fn config(tmp: &std::path::Path, secret: Option<&str>) -> Config {
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    let mut accounts = BTreeMap::new();
    for a in ["first", "second"] {
        accounts.insert(
            a.to_string(),
            tmp.join(a).to_string_lossy().to_string(),
        );
    }
    Config {
        data_dir: Some(tmp.to_string_lossy().to_string()),
        accounts,
        secret: secret.map(str::to_string),
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

/// The chassis server alone, plus the [`Bearers`] it issues against.
async fn start_with(
    secret: Option<&str>,
) -> (
    ThreadManager,
    SocketAddr,
    Arc<Bearers>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().unwrap();
    // THE CLOCK SEAM. Every `Hub::emit` stamps with the clock and every cell
    // refuses an input at or before its `folded_at`, so two emits in one
    // microsecond mean the second is DROPPED — which is what an optimized
    // build does to a tight emit loop. See `src/clock.rs`.
    threads::clock::testing::ticking();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let manager = build_manager(config(tmp.path(), secret)).unwrap();
    let chassis = threads::chassis::chassis(Arc::new(manager.clone()));
    let bearers = Arc::clone(chassis.bearers());
    let listener = shapewire::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (addr, h) = chassis
        .serve_on_with_shutdown(listener, std::future::pending::<()>())
        .unwrap();
    std::mem::forget(h);
    (manager, addr, bearers, tmp)
}

async fn start() -> (ThreadManager, SocketAddr, Arc<Bearers>, tempfile::TempDir) {
    start_with(None).await
}

struct Client {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    welcome: Value,
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
        let mut c = Self {
            ws,
            welcome: Value::Null,
        };
        c.welcome = c.next().await.expect("a welcome");
        c
    }

    async fn send(&mut self, v: Value) {
        use tokio_tungstenite::tungstenite::Message;
        self.ws
            .send(Message::Text(v.to_string().into()))
            .await
            .unwrap();
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
        self.send(json!({"type":"request","id":id,"method":method,"params":params}))
            .await;
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

/// Moves every fact the `accounts` shape folds.
fn shake(m: &ThreadManager) {
    m.store.set_pool_share("first", 0.5, Some("t")).unwrap();
    m.emit_share_fact("first");
    m.emit_limit_fact("first");
    m.emit_health_fact("second");
    m.produce_pressure();
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. THE CONNECTION LIFECYCLE — five invariants that were comments
// ═════════════════════════════════════════════════════════════════════════════

/// **A LEGACY `subscribe` ON THE CHASSIS IS REFUSED, AND OPENS NOTHING.**
///
/// RE-CUT AT R2.S4. Before (`a_legacy_connection_is_handed_no_shape_frames`):
/// a connection that sent `{"type":"subscribe","threads":null}` was ACCEPTED
/// onto the firehose and was then handed no SHAPE frames while a mirror
/// connection was. Now the firehose is retired: the same frame is answered with
/// ONE refusal carrying the chassis's code for a client frame nobody owns —
/// `codes::MALFORMED` (`invalid frame: unknown type \`subscribe\``), which is
/// what `shapewire::server` answers when `ProducerControl::client_frame`
/// disowns a type. (The brief named `unknown_method`; that is the code for a
/// REQUEST whose method nothing answers, and `subscribe` is not a request.)
/// The connection is then handed neither shape frames nor a legacy push,
/// while a mirror on the same daemon IS receiving frames — the positive half
/// that makes the silence evidence.
///
/// **Forced red** (R2.S4) by restoring the chassis's legacy control half
/// (a `ProducerControl` answering `subscribe` with a `sub_ack`): the one frame
/// is the ack, and this fails with
/// `assertion left == right failed: {"firehose":true,"threads":[],"type":"sub_ack"}`
/// (`left: String("sub_ack")`, `right: "response"`).
#[tokio::test]
async fn a_legacy_subscribe_is_refused_and_opens_nothing() {
    let (m, addr, _b, tmp) = start().await;
    let mut legacy = Client::connect(addr).await;
    legacy.send(json!({"type":"subscribe","threads":null})).await;
    let answer = legacy.drain().await;
    assert_eq!(
        answer.len(),
        1,
        "a legacy subscribe earns one refusal and nothing else (no sub_ack, no boot \
         note): {answer:#?}"
    );
    let r = &answer[0];
    assert_eq!(r["type"], json!("response"), "{r}");
    assert_eq!(r["correlate"], json!(""), "nothing to correlate: {r}");
    assert_eq!(r["outcome"]["type"], json!("error"), "{r}");
    assert_eq!(
        r["outcome"]["code"],
        json!(codes::MALFORMED),
        "the chassis's code for a frame type nobody owns: {r}"
    );
    assert_eq!(
        r["outcome"]["error"],
        json!("invalid frame: unknown type `subscribe`"),
        "{r}"
    );

    // A SECOND connection subscribes to a shape, so frames really are flowing.
    let mut mirror = Client::connect(addr).await;
    let r = mirror
        .request("1", &format!("shapes/{}/subscribe", "accounts"), json!({}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
    mirror.drain().await;
    shake(&m);
    m.hub.emit(Push::Status {
        thread_id: "t".into(),
        state: "idle".into(),
        executing: false,
        queued: 0,
        unread: 0,
        detail: None,
        account: Some("first".into()),
    });
    let seen = mirror.drain().await;
    assert!(
        seen.iter().any(|f| f["type"] == "shape"),
        "the shape channel must really be carrying frames, or the silence below \
         proves nothing: {seen:#?}"
    );

    let legacy_frames = legacy.drain().await;
    assert!(
        legacy_frames.is_empty(),
        "a refused legacy subscribe must open nothing — no shape frames, no \
         `status` push: {legacy_frames:#?}"
    );
    drop(tmp);
}

/// **THE RECEIVER IS CREATED BEFORE THE ITEM IS ENQUEUED.**
///
/// `drive` creates this connection's shape receiver in the `Request` arm, ahead
/// of dispatch, so the export cannot be emitted into a channel the connection is
/// not yet listening on. Moving it after the dispatch loses the first export of
/// every COLD subscribe, silently — the connection stays healthy and never
/// learns it is holding nothing.
///
/// **Forced red** by deleting the receiver's creation from the `Request` arm
/// entirely: the cold subscribe answers `{queued:true}` and then no export
/// arrives, and this fails with `a cold subscribe must deliver its own export`.
///
/// **What this test does NOT guard, said plainly rather than implied.** Moving
/// that line to AFTER `dispatch` leaves this test GREEN, and I checked: the
/// enqueue only pushes a control item onto the shape's FIFO, and the export is
/// emitted by the shape task milliseconds later on another task — so the window
/// between "enqueued" and "emitted" is wider than the window between "dispatch
/// returned" and "receiver created", and a client cannot observe the difference.
/// The BEFORE-the-enqueue ordering is therefore held by structure and by the
/// comment on that line, not by this assertion; what this assertion holds is the
/// observable half, that a cold subscribe's export is delivered at all. Named as
/// a residual in the report rather than papered over with a sleep.
#[tokio::test]
async fn a_cold_subscribes_first_export_is_not_lost() {
    let (_m, addr, _b, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    // COLD: nothing has subscribed to `accounts` on this daemon yet.
    let r = c
        .request(
            "1",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({"key":"first"}),
        )
        .await;
    assert_eq!(r["outcome"]["payload"]["queued"], json!(true), "{r}");
    let frames = c.drain().await;
    let export = frames
        .iter()
        .find(|f| f["type"] == "shape" && f["op"] == "read");
    assert!(
        export.is_some(),
        "a cold subscribe must deliver its own export: {frames:#?}"
    );
    drop(tmp);
}

/// **THE CONNECTION'S OWN LIFETIME IS WHAT CLOSES ITS SUBSCRIPTIONS.**
///
/// `hub.dropped(conn)` lives in a `Drop` and not as the last statement of
/// `drive`, because a panic unwinds past a last statement and an abort never
/// reaches one. Both leave the shape task counting a subscriber that is gone.
///
/// **Forced red** by deleting `self.0.hub().dropped(self.2)` from
/// `ConnectionGuard::drop`: `subscribers` stays at 1 after the socket closes and
/// this fails with `a dropped connection must stop being counted: 1`.
#[tokio::test]
async fn a_dropped_connection_stops_being_counted() {
    let (m, addr, _b, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.request(
        "1",
        &format!("shapes/{}/subscribe", "accounts"),
        json!({"key":"first"}),
    )
    .await;
    c.drain().await;

    let subscribers = |m: &ThreadManager| -> usize {
        m.hub
            .list()
            .into_iter()
            .find(|i| i.name == "accounts")
            .map(|i| i.subscribers)
            .unwrap_or(0)
    };
    assert_eq!(subscribers(&m), 1, "the subscribe must have been counted");

    drop(c);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert_eq!(
        subscribers(&m),
        0,
        "a dropped connection must stop being counted: {}",
        subscribers(&m)
    );
    // AND THE LEDGER, which is the other half of `hub.dropped` and the half that
    // reports a consumer that is gone — the confidently-wrong state it exists to
    // prevent.
    let rows = m
        .hub
        .list()
        .into_iter()
        .find(|i| i.name == "accounts")
        .map(|i| i.consumers)
        .unwrap_or_default();
    assert!(
        rows.is_empty(),
        "the consumer ledger must stop reporting a consumer that is gone: {rows:#?}"
    );
    drop(tmp);
}

/// **THE `authed` GATE IS ON EVERY ARM.**
///
/// Three halves, because each is a different way for the gate to be missing: a
/// request is refused on its own id; a client frame is refused before anything
/// looks at its type; a shape frame is not delivered either.
///
/// **Forced red** by deleting `ref other if !authed =>` from `drive`'s incoming
/// match: the `threads.list` below succeeds and this fails with
/// `an unauthenticated request must be refused`.
///
/// **Forced red** (R2.S4, the re-cut half) by the same restored control half:
/// `authenticated, the legacy frame is refused for what it IS:
/// {"firehose":true,"threads":[],"type":"sub_ack"}`.
///
/// RE-CUT AT R2.S4 (the last half). Before: after `auth`, a legacy subscribe
/// was honoured and the same `status` push ARRIVED — the positive half proving
/// the earlier silence was the gate. There is no firehose to honour it now, so
/// the positive half is: after `auth` the same `threads.list` SUCCEEDS, and the
/// same legacy frame is refused `malformed` instead of `unauthorized` — the
/// gate opened, and what is left is the frame's own refusal.
#[tokio::test]
async fn an_unauthenticated_connection_is_refused_and_pushed_nothing() {
    let (m, addr, _b, tmp) = start_with(Some("shibboleth")).await;
    let mut c = Client::connect(addr).await;
    assert_eq!(c.welcome["auth_required"], json!(true));

    // 1. A request is refused ON ITS OWN ID, so a caller can correlate it.
    let r = c.request("r1", "threads/list", json!({})).await;
    assert_eq!(r["correlate"], json!("r1"), "correlated: {r}");
    assert_eq!(r["outcome"]["type"], json!("error"), "{r}");
    assert!(
        r["outcome"]["error"]
            .as_str()
            .unwrap()
            .contains("unauthorized"),
        "an unauthenticated request must be refused: {r}"
    );
    assert_eq!(
        r["outcome"]["code"],
        json!(codes::UNAUTHORIZED),
        "and typed, so a consumer need not read the sentence: {r}"
    );

    // 2. A producer frame is refused too, with no id to correlate.
    c.send(json!({"type":"subscribe","threads":null})).await;
    let r = c.next().await.expect("a refusal");
    assert_eq!(r["outcome"]["code"], json!(codes::UNAUTHORIZED), "{r}");

    // 3. And nothing is PUSHED. Emitted after the (refused) subscribe, so a
    //    daemon that had honoured the subscribe would deliver it.
    m.hub.emit(Push::Status {
        thread_id: "t".into(),
        state: "idle".into(),
        executing: false,
        queued: 0,
        unread: 0,
        detail: None,
        account: None,
    });
    let seen = c.drain().await;
    assert!(
        seen.is_empty(),
        "an unauthenticated connection must receive nothing: {seen:#?}"
    );

    // And the gate really opens, so the refusals above were the gate and not a
    // broken fixture.
    c.send(json!({"type":"auth","secret":"shibboleth"})).await;
    let ack = c.next().await.expect("an auth_ack");
    assert_eq!(ack["ok"], json!(true), "{ack}");
    let r = c.request("r2", "threads/list", json!({})).await;
    assert_eq!(r["outcome"]["type"], json!("success"), "after auth the request is served: {r}");
    c.send(json!({"type":"subscribe","threads":null})).await;
    let r = c.next().await.expect("a refusal");
    assert_eq!(
        r["outcome"]["code"],
        json!(codes::MALFORMED),
        "authenticated, the legacy frame is refused for what it IS: {r}"
    );
    drop(tmp);
}

/// **NO BOOT NOTE RIDES A LEGACY SUBSCRIBE; THE `boot` SHAPE CARRIES IT.**
///
/// RE-CUT AT R2.S4. Before (`the_boot_note_is_replayed_once_per_connection`):
/// the first firehose subscribe on a connection replayed one `lifecycle` frame,
/// a second subscribe replayed none, and a fresh connection heard it again.
/// That replay — `ThreadsControl`'s `if ty == "subscribe" && !sent_boot_note &&
/// firehose` — is deleted with the firehose. Now: with a note held, a legacy
/// subscribe yields no `lifecycle` frame, and a `boot` shape subscribe on the
/// SAME connection exports this boot's `reason` and `boot_id` — the fact moved
/// carriers and is not lost.
///
/// **Forced red** (R2.S4) by restoring the replay (a control half pushing the
/// boot note as a `lifecycle` frame after the ack): `no lifecycle frame rides
/// a legacy subscribe: [ … ]`, `left: 1`, `right: 0`.
#[tokio::test]
async fn no_boot_note_rides_a_legacy_subscribe_and_the_boot_shape_carries_it() {
    let (m, addr, _b, tmp) = start().await;
    // `on_boot` is what computes the reason this process came up. A test that
    // skipped it would assert "no replay" against a `None` and pass for no
    // reason.
    m.update.on_boot().await;
    let note = m.update.boot_note().expect("the fixture must actually have a note");
    let mut c = Client::connect(addr).await;

    c.send(json!({"type":"subscribe","threads":null})).await;
    let first = c.drain().await;
    let notes = first.iter().filter(|f| f["type"] == "lifecycle").count();
    assert_eq!(notes, 0, "no lifecycle frame rides a legacy subscribe: {first:#?}");

    let r = c
        .request("1", "shapes/boot/subscribe", json!({"key": "daemon"}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
    let frames = c.drain().await;
    let export = frames
        .iter()
        .find(|f| f["type"] == "shape" && f["op"] == "read" && f["row"].is_object())
        .unwrap_or_else(|| panic!("the boot shape exports its row: {frames:#?}"));
    assert_eq!(export["row"]["reason"], json!(note.reason), "{export}");
    assert_eq!(export["row"]["boot_id"], json!(note.boot_id), "{export}");
    drop(tmp);
}

/// **A CONNECTION RECEIVES A SHAPE FRAME IFF IT HAS HAD ITS OWN EXPORT FOR THAT
/// `(shape, key)`.** `shape_subs` is populated by nothing except an export
/// addressed to this connection.
///
/// **Forced red** by changing the `Some(_) => false` arm (another connection's
/// export) to `true`: the `first` mirror is handed the `second` mirror's export
/// and this fails with `a mirror must not see another connection's export`.
#[tokio::test]
async fn a_mirror_sees_only_the_keys_it_holds() {
    let (m, addr, _b, tmp) = start().await;
    let mut a = Client::connect(addr).await;
    let mut b = Client::connect(addr).await;
    a.request(
        "1",
        &format!("shapes/{}/subscribe", "accounts"),
        json!({"key":"first"}),
    )
    .await;
    a.drain().await;
    b.request(
        "1",
        &format!("shapes/{}/subscribe", "accounts"),
        json!({"key":"second"}),
    )
    .await;
    b.drain().await;

    // A's export must not have reached B, and vice versa. Both drained above,
    // so anything from here is a LIVE frame for a held key.
    m.emit_health_fact("second");
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let a_frames = a.drain().await;
    let b_frames = b.drain().await;

    for f in &a_frames {
        assert_ne!(
            f["key"],
            json!("second"),
            "a mirror must not see another connection's export or key: {f}"
        );
    }
    assert!(
        b_frames
            .iter()
            .all(|f| f["type"] != "shape" || f["key"] != json!("first")),
        "and not the other way round either: {b_frames:#?}"
    );
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. R1 — THE TYPED ERROR ENVELOPE
// ═════════════════════════════════════════════════════════════════════════════

/// A shape whose fold panics on command — the only way to exercise the death
/// path without waiting for a real bug. Lifted from `shapes.rs`.
struct Boom;

#[derive(Default)]
struct BoomRoot {
    n: u64,
    dirty: bool,
}

impl Root for BoomRoot {
    fn keys(&self) -> Vec<String> {
        vec!["k".to_string()]
    }
    fn row(&self, _: &str) -> Option<Value> {
        Some(json!({ "n": self.n }))
    }
    fn cells(&self, _: &str) -> Option<Value> {
        Some(json!({ "n": CellState::Known {
            complete: true,
            folded_at: chrono::Utc::now(),
            since_init: false,
            covers_since: None,
        }}))
    }
    fn shared(&self) -> Value {
        json!({})
    }
    fn shared_cells(&self) -> Value {
        json!({})
    }
    fn drain(&mut self) -> Outbox {
        Outbox {
            rows: if std::mem::take(&mut self.dirty) {
                vec!["k".to_string()]
            } else {
                vec![]
            },
            ..Default::default()
        }
    }
    fn mark_dead(&mut self, _: &str) {}
}


// The cells this file's test roots emit, declared — `Shape::CELLS` is required
// (see `shapewire::cell`), and a test shape declares what its `Root::cells`
// actually spells.
shapewire::declare_cells! {
    N_CELLS = [("n", shapewire::operation::DType::Scalar(shapewire::operation::Scalar::Int), shapewire::operation::Presence::Required, [])]
}

impl Shape<Input> for Boom {
    const NAME: &'static str = "boom";
    const CELLS: Cells = N_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    type Producer = ThreadManager;
    type Root = BoomRoot;

    fn root(&self) -> BoomRoot {
        BoomRoot::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, _: &mut BoomRoot, _: Timestamp) {}
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, BoomRoot>) {
        if let Input::Push(Push::Info { reason, .. }) = &input.value {
            if reason == "boom" {
                panic!("the fold panicked, as this test asked it to");
            }
        }
        tx.n += 1;
        tx.dirty = true;
    }
}

/// **RULE 2.6's REFUSAL CARRIES ITS OWN CODE ON THE CHASSIS DOOR (R2.S8).**
///
/// `accounts` is version 2 (its `threads` cell is gone) and serves only 2, so a
/// consumer stating version 1 is refused — and the CODE is what a consuming
/// chassis goes terminal on (`seam/held.rs::Held::refuse` ->
/// `Mirror::refused_version`), so the code is asserted, never the prose.
///
/// Opposed, so a door that stamped one code on everything cannot pass: a plain
/// unknown method on the same connection carries `unknown_method`, and the
/// version-2 subscribe of the same shape is SERVED.
///
/// **THE MUTATION.** `.map_err(Refusal::from)` on the subscribe arm of
/// `shapewire/src/server.rs` replaced by `.map_err(|e| Refusal::untyped(e.to_string()))`:
/// the first assertion reads `"error"`.
#[tokio::test]
async fn a_version_refusal_carries_version_refused_and_an_unknown_method_its_own() {
    let (_m, addr, _b, tmp) = start().await;
    let mut c = Client::connect(addr).await;

    let r = c
        .request("v1", "shapes/accounts/subscribe", json!({"version": 1}))
        .await;
    assert_eq!(r["outcome"]["code"], json!(codes::VERSION_REFUSED), "{r}");

    let r = c.request("um", "threads/not-a-verb/at-all", json!({})).await;
    assert_eq!(r["outcome"]["code"], json!(codes::UNKNOWN_METHOD), "{r}");

    let r = c
        .request("v2", "shapes/accounts/subscribe", json!({"version": 2}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "version 2 is served: {r}");
    drop(tmp);
}

/// **THE READS NOBODY MADE ARE GONE FROM THE DOOR (R2.S3a).** The operator:
/// *"We should not have new paths to request data… We should remove the ability
/// to request data."* `budgets/list`, `ledger/query` and
/// `threads/{thread_id}/messages` had no caller in the seam, the board, either
/// face or the harness tools (census in `docs/consumer-migration-ledger.md`), so
/// the producer stopped serving them: each now answers `unknown_method`, with
/// well-formed params and — for the keyed one — a thread that EXISTS, so the
/// refusal can only be the method's absence and not a bad subject or param.
///
/// Opposed on the SAME connection, so a door that refused everything cannot
/// pass: `threads/list` (and, until R2.S3c, `threads/{id}/get`) on that same
/// thread answer `success`, and so does `threads/{id}/budget_get` — which was on the brief's
/// list of five and was NOT retired, because `src/bin/threads-stress.rs` still
/// calls it (as it does `budgets/{key}/get`). A producer does not remove a read
/// a consumer still makes; that one is pinned served here so its removal has to
/// be deliberate.
///
/// **THE MUTATION.** Put `"ledger/query"` back in `WS_METHODS` together with
/// its arm (both halves: an entry without its arm is refused by the same
/// router and would prove nothing). Forced red:
/// ```text
/// ledger/query must be refused unknown_method, got: {"correlate":"r2",...,"outcome":{"type":"success",...}}
/// ```
///
/// # R2.S3c (R4.S9, HOLDS R9) — ten more
///
/// The reads R2.S3b/R2.S3d moved every consumer off — `accounts/{health,list,
/// pool_share_get,pressure,status}`, `daemon/status`, `gauge/history`,
/// `threads/status`, `threads/{thread_id}/get`, `threads/{thread_id}/query` —
/// are retired once the faces running that code were relaunched (r23). Census
/// (R4.S9 report): no running face, board, harness tool or script sends them.
/// The KEPT set grows by what `threads-stress` and the `threads image` CLI still
/// send: `gauge/status`, `threads/{id}/history`, `budgets/{key}/get`,
/// `daemon/image`. **Forced red** by restoring the `threads/{thread_id}/get`
/// arm and its `WS_METHODS` entry; the red is pasted in the R4.S9 report.
#[tokio::test]
async fn the_retired_reads_answer_unknown_method_and_the_kept_ones_still_answer() {
    let (_m, addr, _b, tmp) = start().await;
    let mut c = Client::connect(addr).await;

    let created = c
        .request(
            "mk",
            "threads/create",
            json!({"settings": {"cwd": tmp.path().display().to_string(), "model": "sonnet"}}),
        )
        .await;
    assert_eq!(created["outcome"]["type"], json!("success"), "{created}");
    let id = created["outcome"]["payload"]["thread_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no thread_id: {created}"))
        .to_string();

    let retired = [
        ("budgets/list".to_string(), json!({})),
        ("ledger/query".to_string(), json!({"group_by": "thread"})),
        (format!("threads/{id}/messages"), json!({"ids": [1]})),
        // R2.S3c (R4.S9):
        ("accounts/health".to_string(), json!({})),
        ("accounts/list".to_string(), json!({})),
        ("accounts/pool_share_get".to_string(), json!({})),
        ("accounts/pressure".to_string(), json!({})),
        ("accounts/status".to_string(), json!({})),
        ("daemon/status".to_string(), json!({})),
        ("gauge/history".to_string(), json!({})),
        ("threads/status".to_string(), json!({})),
        (format!("threads/{id}/get"), json!({})),
        (format!("threads/{id}/query"), json!({"ids": [1]})),
    ];
    for (n, (method, params)) in retired.iter().enumerate() {
        let r = c.request(&format!("r{n}"), method, params.clone()).await;
        assert_eq!(
            r["outcome"]["code"],
            json!(codes::UNKNOWN_METHOD),
            "{method} must be refused unknown_method, got: {r}"
        );
    }
    for template in [
        "budgets/list",
        "ledger/query",
        "threads/{thread_id}/messages",
        "accounts/health",
        "accounts/list",
        "accounts/pool_share_get",
        "accounts/pressure",
        "accounts/status",
        "daemon/status",
        "gauge/history",
        "threads/status",
        "threads/{thread_id}/get",
        "threads/{thread_id}/query",
    ] {
        assert!(!WS_METHODS.contains(&template), "{template} is still pinned");
    }

    let kept = [
        "threads/list".to_string(),
        format!("threads/{id}/budget_get"),
        format!("threads/{id}/history"),
        "gauge/status".to_string(),
        "daemon/image".to_string(),
    ];
    for (n, method) in kept.iter().enumerate() {
        let r = c.request(&format!("k{n}"), method, json!({})).await;
        assert_eq!(
            r["outcome"]["type"],
            json!("success"),
            "{method} is kept and must still answer on the same connection: {r}"
        );
    }
    // `budgets/{key}/get` is kept (threads-stress sends it) but has no key in
    // this fixture, so its PRODUCER refuses it — with `error`, which is a routed
    // method answering, never `unknown_method`.
    let r = c.request("kb", "budgets/stress-key/get", json!({})).await;
    assert_eq!(
        r["outcome"]["code"],
        json!(codes::ERROR),
        "budgets/{{key}}/get is kept: routed, refused by the producer, not unknown: {r}"
    );
    drop(tmp);
}

/// **A CONSUMER CAN TELL `unknown` FROM `dead` FROM AN ABSURD `at` WITHOUT
/// PARSING ENGLISH.** That is R1, and it is the whole reason it folds into this
/// move: before it, all three arrived as `Error::Config(e.to_string())` — the
/// `Display` prose of `ShapeError`, which `wire.rs` documented reaching no wire.
///
/// **Forced red** by giving `impl Coded for ShapeError` the default body (delete
/// the match, leave `impl Coded for ShapeError {}`): every code below becomes
/// `"error"` and this fails three times over, starting with
/// `unknown shape must be distinguishable: "error"`.
#[tokio::test]
async fn every_shape_refusal_carries_its_own_code() {
    let (m, addr, _b, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Boom, m.clone());
    let mut c = Client::connect(addr).await;

    // 1. UNKNOWN — no such shape on this producer.
    let r = c
        .request("1", &format!("shapes/{}/subscribe", "not-a-shape"), json!({}))
        .await;
    assert_eq!(
        r["outcome"]["code"],
        json!(codes::UNKNOWN),
        "unknown shape must be distinguishable: {}",
        r["outcome"]["code"]
    );

    // 2. MALFORMED — an `at` that is not RFC3339, and a missing `shape`.
    let r = c
        .request(
            "2",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({"at":"tuesday"}),
        )
        .await;
    assert_eq!(r["outcome"]["code"], json!(codes::MALFORMED), "{r}");
    // **AND A SUBSCRIBE WITH NO SHAPE IS NO LONGER `malformed` — IT IS NOT
    //   EXPRESSIBLE.** The shape is the SUBJECT and rides the path, so
    //   `shapes/subscribe` is not a method at all and the inventory says so.
    //   That is the stronger answer: `malformed` said *I could not read your
    //   request*, and this says *there is no such door*, which is what a
    //   consumer that omitted the shape actually needs to hear.
    let r = c.request("3", "shapes/subscribe", json!({})).await;
    assert_eq!(r["outcome"]["code"], json!(codes::UNKNOWN_METHOD), "{r}");
    // And naming the shape in the PARAMS as well is refused rather than
    // resolved by precedence — the one place the subject may be written down
    // is the path.
    let r = c
        .request(
            "3b",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({"shape": "accounts"}),
        )
        .await;
    assert_eq!(r["outcome"]["code"], json!(codes::MALFORMED), "{r}");

    // 3. MALFORMED again — an absurd future `at`, which is a bound and not a
    //    parse failure, and must not be confused with a dead shape.
    let far = (chrono::Utc::now() + chrono::Duration::days(3650)).to_rfc3339();
    let r = c
        .request(
            "4",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({"at":far}),
        )
        .await;
    assert_eq!(r["outcome"]["code"], json!(codes::MALFORMED), "{r}");
    assert!(
        r["outcome"]["error"].as_str().unwrap().contains("24h"),
        "the prose still names the bound: {r}"
    );

    // 4. DEAD — the terminal state a consumer must draw as terminal.
    c.request("5", &format!("shapes/{}/subscribe", "boom"), json!({"key":"k"}))
        .await;
    c.drain().await;
    hub.emit(Push::Info {
        domain: "x".into(),
        reason: "boom".into(),
        account: None,
        detail: None,
        at: "now".into(),
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let r = c
        .request("6", &format!("shapes/{}/subscribe", "boom"), json!({"key":"k"}))
        .await;
    assert_eq!(
        r["outcome"]["code"],
        json!(codes::DEAD),
        "a dead fold is its own code, not prose: {r}"
    );

    // 5. And the four are FOUR — a code set where two refusals share a code is
    //    a code set that has not typed them.
    let codes_seen = [codes::UNKNOWN, codes::MALFORMED, codes::DEAD];
    assert_eq!(
        codes_seen.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
    drop(tmp);
}

/// **AN UNTYPED PRODUCER ERROR IS SPELLED `error`, NOT LEFT ABSENT.**
///
/// `threads::error::Error` takes `Coded`'s default deliberately (its `Config`
/// variant is three codes wearing one name), so every one of the 50 producer
/// methods refuses with `code: "error"` — exactly as informative as before R1,
/// and said out loud so a consumer can tell "this producer has not typed this
/// path" from "this producer is old".
///
/// **Forced red** by making `Outcome::Error`'s `code` an
/// `Option<String>` with `skip_serializing_if`: the assertion that the field is
/// PRESENT fails with `code must never be absent`.
#[tokio::test]
async fn an_untyped_producer_error_still_carries_a_code() {
    let (_m, addr, _b, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    // A real producer method, refused by the producer's own error type.
    let r = c
        // `threads/{id}/get` until R2.S3c retired it (R4.S9); `budget_get` is a
        // kept read that refuses an unknown thread through the same `Config`.
        .request("1", &format!("threads/{}/budget_get", "no-such-thread"), json!({}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("error"), "{r}");
    assert!(
        r["outcome"].get("code").is_some(),
        "code must never be absent: {r}"
    );
    assert_eq!(r["outcome"]["code"], json!(codes::ERROR), "{r}");
    drop(tmp);
}

/// **AN UNKNOWN METHOD IS ANSWERED FROM THE INVENTORY, NOT FROM PROSE.**
///
/// The producer's catch-all arm answers the same sentence as an `Error`
/// indistinguishable from every other bad request, so typing it there would have
/// meant matching on English. The chassis holds the inventory, so it answers
/// this itself — same sentence, new code.
///
/// **Forced red** by deleting the `None if !producer.names()…` arm from
/// `dispatch`: the answer falls through to the producer's catch-all, the code
/// becomes `"error"`, and this fails with `an unknown method is structurally
/// knowable: "error"`.
#[tokio::test]
async fn an_unknown_method_is_typed_structurally() {
    let (_m, addr, _b, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let r = c.request("1", "threads.nope", json!({})).await;
    assert_eq!(
        r["outcome"]["code"],
        json!(codes::UNKNOWN_METHOD),
        "an unknown method is structurally knowable: {}",
        r["outcome"]["code"]
    );
    assert_eq!(
        r["outcome"]["error"],
        json!("unknown method: threads.nope"),
        "and the sentence is unchanged: {r}"
    );
    drop(tmp);
}

/// The one client-frame difference the golden session deliberately does not
/// exercise, pinned here so it is a decision and not a discovery.
///
/// **Forced red** (pre-R2.S4) by having `classify` bind the body to a
/// `ChassisMsg` before reading the `type` tag. Since R2.S4 no `threads` frame
/// reaches the producer, so the second half asserts the refusal instead.
#[tokio::test]
async fn an_unknown_frame_type_is_refused_differently_and_deliberately() {
    let (_m, addr, _b, tmp) = start().await;
    let mut c = Client::connect(addr).await;

    c.send(json!({"type":"not-a-frame-kind"})).await;
    let r = c.next().await.expect("a refusal");
    assert_eq!(r["correlate"], json!(""), "nothing to correlate: {r}");
    assert!(
        r["outcome"]["error"]
            .as_str()
            .unwrap()
            .contains("unknown type `not-a-frame-kind`"),
        "the chassis names the tag rather than listing a producer's vocabulary: {r}"
    );
    assert_eq!(r["outcome"]["code"], json!(codes::MALFORMED), "{r}");

    // RE-CUT AT R2.S4: `subscribe` WAS a producer frame — it reached
    // `ThreadsControl` and was answered with a `sub_ack`. `threads` owns no
    // client frame now (`NoControl`), so `subscribe` is refused by exactly the
    // same rule, sentence and code as the made-up type above.
    c.send(json!({"type":"subscribe","threads":null})).await;
    let r = c.next().await.expect("a refusal");
    assert_eq!(
        r["outcome"]["error"],
        json!("invalid frame: unknown type `subscribe`"),
        "a frame type no producer owns is refused like any other: {r}"
    );
    assert_eq!(r["outcome"]["code"], json!(codes::MALFORMED), "{r}");
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. THE METHOD SURFACE
// ═════════════════════════════════════════════════════════════════════════════

/// **THE SURFACE IS THE CHASSIS'S PLUS THE PRODUCER'S — AND THE TWO SETS ARE NOT
/// NESTED.**
///
/// `threads`' `WS_METHODS` is pinned against its own match arms by
/// `ws_protocol.rs`. This is the other half: the chassis's methods are in the
/// daemon's surface and the chassis really answers each one.
///
/// # The containment assertion that used to be here, and why it is per-method now
///
/// It asserted `CHASSIS_METHODS ⊆ WS_METHODS` for ALL of them, and that held for
/// exactly as long as every chassis method was one that had MOVED off the
/// producer: the legacy server still serves `shapes.list` and the two
/// subscribes out of its own match, so each name was in both inventories
/// honestly.
///
/// `durable/barrier` (2026-10-02, rule 2.7's third stage) is the first chassis
/// method that was never the producer's. Adding it to `WS_METHODS` would make
/// the LEGACY server claim a method it has no arm for — and
/// `the_ws_method_inventory_is_pinned` would fail, correctly, because that
/// inventory is pinned against `src/server.rs`'s arms and there is no arm to
/// find. So the rule is per method: a chassis method that moved off the producer
/// stays in both, and one that is new chassis surface is in `CHASSIS_METHODS`
/// alone. The half that actually guards anything — *it is ANSWERED, not merely
/// listed* — is below and covers every method either way.
///
/// **Forced red** by removing `"shapes/list"` from `CHASSIS_METHODS`: the
/// containment assertion fails with `a chassis method that moved off the
/// producer is missing from the daemon's surface`. And forced red the other way
/// by adding `"durable/barrier"` to `WS_METHODS`, which fails
/// `the_ws_method_inventory_is_pinned` in `ws_protocol.rs` — the two tests
/// together are what make the split a decision rather than an omission.
/// **A CHASSIS METHOD THE PRODUCER DOES NOT SERVE IS ANSWERED ON THE ONE DOOR,
/// AND THE PRODUCER'S INVENTORY DOES NOT CLAIM IT.**
///
/// The structural half lives in
/// `ws_protocol.rs::the_ws_method_inventory_is_pinned`.
///
/// RE-CUT AT R2.S4. Before: both doors served one manager, and
/// `resource/{id}/read` was answered by the chassis and refused `unknown
/// method` by the legacy door — the live proof that the legacy door's own
/// inventory must not claim it. There is no legacy door now. What stays true
/// and is asserted: the chassis door SERVES it (not `unknown_method`), a name
/// in NEITHER inventory on the same door IS `unknown_method` (so the code check
/// discriminates), and `WS_METHODS` does not list it.
///
/// **THE MUTATION.** Put `"resource/{id}/read"` back into `WS_METHODS`: the
/// last assertion fails — forced (R2.S4): `the producer's inventory must not
/// claim a method only the chassis answers`.
#[tokio::test]
async fn a_chassis_method_the_producer_does_not_serve_is_claimed_by_neither_inventory() {
    let (_m, chassis_addr, _b, tmp) = start().await;

    const M: &str = "resource/no-such-resource/read";

    let mut c = Client::connect(chassis_addr).await;
    let on_chassis = c.request("1", M, json!({})).await;
    assert_ne!(
        on_chassis["outcome"]["code"],
        json!(codes::UNKNOWN_METHOD),
        "the chassis door SERVES this method — it is in CHASSIS_METHODS and \
         `dispatch` routes it before `Producer::handle` is reached. A \
         not-found for the id is the right refusal; unknown_method is not: {on_chassis}"
    );
    let nowhere = c.request("2", "resource-nope/read", json!({})).await;
    assert_eq!(
        nowhere["outcome"]["code"],
        json!(codes::UNKNOWN_METHOD),
        "the sentinel: a name nothing serves IS unknown_method on this door: {nowhere}"
    );
    assert!(
        !WS_METHODS.contains(&"resource/{id}/read"),
        "the producer's inventory must not claim a method only the chassis answers"
    );
    drop(tmp);
}

#[tokio::test]
async fn the_surface_is_the_chassis_plus_the_producer() {
    let (_m, addr, _b, tmp) = start().await;

    // 1. Every chassis method the producer ALSO serves is in the daemon's
    //    pinned inventory, so it is a method `ws_protocol.rs` pins.
    const MOVED_OFF_THE_PRODUCER: &[&str] = &[
        "shapes/list",
        "shapes/{shape}/subscribe",
        "shapes/{shape}/unsubscribe",
    ];
    for m in MOVED_OFF_THE_PRODUCER {
        assert!(
            WS_METHODS.contains(m),
            "a chassis method that moved off the producer is missing from the              daemon's surface: {m}"
        );
        assert!(CHASSIS_METHODS.contains(m), "{m}");
    }
    assert_eq!(
        CHASSIS_METHODS,
        &[
            "durable/barrier",
            "shapes/list",
            "shapes/{shape}/subscribe",
            "shapes/{shape}/unsubscribe",
            // The FOURTH, 2026-10-02: the inline resource door. It is the
            // chassis's for the same reason the HTTP one is — the subject is an
            // opaque, shape-issued id and the gate that mints it is
            // `resource::opaque_id`'s single constructor. A producer routing it
            // itself would be a second place for the no-query rule to be
            // forgotten.
            "resource/{id}/read",
        ],
        "the three that moved, plus the barrier and the inline resource door — \
         neither of which was ever the producer's"
    );
    // ── AND THE OTHER DIRECTION, WHICH IS WHERE THE RULE WAS BROKEN ─────────
    //
    // A chassis method that `threads` does not serve must NOT be in
    // `WS_METHODS`, because both doors route against that list and only one of
    // them can answer: `:3651` matched `resource/{id}/read`, found no arm, and
    // answered `unknown method: resource/{id}/read` on a method its own
    // inventory claimed. Advertising a method you refuse is worse than not
    // advertising it — `caps.py::is_absence` reads an unknown-method refusal as
    // "this daemon does not have it", which is TRUE here and contradicted by the
    // inventory the same daemon publishes.
    //
    // `:3653` is unaffected: the chassis routes `CHASSIS_METHODS` first and
    // `producer.names()` second, so a chassis method needs no entry in the
    // producer's list to be served.
    for m in ["durable/barrier", "resource/{id}/read"] {
        assert!(
            !WS_METHODS.contains(&m),
            "`threads` has no arm for `{m}` and must not claim it — \
             `ws_protocol.rs::the_ws_method_inventory_is_pinned` is the structural \
             half of this"
        );
    }

    // 2. And each is really ANSWERED by the chassis, not merely listed. An
    //    `unknown_method` code here would mean the name was pinned and the arm
    //    was not.
    let mut c = Client::connect(addr).await;
    for (i, m) in CHASSIS_METHODS.iter().enumerate() {
        let r = c
            .request(
                &i.to_string(),
                m,
                json!({"key":"first"}),
            )
            .await;
        assert_ne!(
            r["outcome"]["code"],
            json!(codes::UNKNOWN_METHOD),
            "the chassis must ANSWER {m}, not list it: {r}"
        );
    }
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 4. TWO SERVERS, ONE HUB
// ═════════════════════════════════════════════════════════════════════════════

/// **TWO SERVERS OVER ONE HUB NEVER MINT THE SAME CONNECTION ID.**
///
/// A `ConnId` is the address a shape export is delivered to. `server.rs` minted
/// them from a `static AtomicU64` in its own file, which is correct for exactly
/// as long as there is one server; the second daemon made two, each starting at
/// 1, so both first connections would have been id 1 and a frame addressed
/// `to: Some(1)` would have reached whichever asked. The id space is the hub's.
///
/// RE-CUT AT R2.S4. Before: the two servers were the legacy router and the
/// chassis. The legacy router is gone; the invariant is the hub's and holds for
/// ANY two servers over it, so the two are now two chassis servers over one
/// manager — which is still a real configuration (a test, or a dev daemon
/// beside a probe).
///
/// **Forced red** (pre-R2.S4) by restoring `static NEXT_CONN` in
/// `server.rs::drive`. **Forced red (R2.S4)** by making
/// `shapewire::hub::Hub::next_conn` answer `1` every time: the two
/// subscriptions collapse onto one id and this fails with
/// `both servers' subscribes are here: [1]` (`left: 1`, `right: 2`).
#[tokio::test]
async fn two_servers_over_one_hub_mint_distinct_connection_ids() {
    let tmp = tempfile::tempdir().unwrap();
    threads::clock::testing::ticking();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config(tmp.path(), None)).unwrap();
    let (one, h1) = threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (two, h2) =
        threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
    std::mem::forget((h1, h2));

    let mut a = Client::connect(one).await;
    let mut b = Client::connect(two).await;
    // WITH an identity, so both land in the hub's registry and `shapes.list`
    // reports the conn id each server minted.
    let ra = a
        .request(
            "1",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({"key":"first",
                   "consumer":{"name":"probe-one","shape_version":"1"}}),
        )
        .await;
    let rb = b
        .request(
            "1",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({"key":"second",
                   "consumer":{"name":"probe-two","shape_version":"1"}}),
        )
        .await;
    assert_eq!(ra["outcome"]["type"], json!("success"), "{ra}");
    assert_eq!(rb["outcome"]["type"], json!("success"), "{rb}");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let info = m
        .hub
        .list()
        .into_iter()
        .find(|i| i.name == "accounts")
        .expect("the accounts shape");
    let conns: Vec<u64> = info.consumers.iter().map(|e| e.conn).collect();
    assert_eq!(conns.len(), 2, "both servers' subscribes are here: {conns:?}");
    assert_ne!(
        conns[0], conns[1],
        "two servers minted the same connection id: {conns:?}"
    );
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 5. THE BEARER — the grant the resource door will read
// ═════════════════════════════════════════════════════════════════════════════

/// **ISSUED ON CONNECT, GATED BY THE HANDSHAKE, REVOKED WITH THE SOCKET.**
///
/// Operator ruling 2026-09-26. An open socket has completed its handshake by
/// arriving, so the grant rides the `Welcome`; a socket with a secret to satisfy
/// is issued nothing until its `AuthAck`.
///
/// **Forced red** by deleting `self.1.revoke(self.2)` from
/// `ConnectionGuard::drop`: the grant outlives the socket and this fails with
/// `a grant must not outlive its socket: 1 still live`.
#[tokio::test]
async fn a_bearer_is_issued_on_connect_and_dies_with_the_socket() {
    let (_m, addr, bearers, tmp) = start().await;
    assert_eq!(bearers.live(), 0, "nothing granted before anyone connects");

    let c = Client::connect(addr).await;
    assert_eq!(c.welcome["auth_required"], json!(false));
    let token = c.welcome["bearer"]
        .as_str()
        .expect("an open socket is issued its grant on the welcome")
        .to_string();
    assert_eq!(bearers.live(), 1);

    // It authenticates the second door — by HEADER.
    let mut h = axum::http::HeaderMap::new();
    h.insert(
        "authorization",
        format!("Bearer {token}").parse().unwrap(),
    );
    assert!(
        bearers.verify(&h).is_ok(),
        "the grant the handshake issued must authenticate"
    );

    drop(c);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert_eq!(
        bearers.live(),
        0,
        "a grant must not outlive its socket: {} still live",
        bearers.live()
    );
    assert!(
        bearers.verify(&h).is_err(),
        "and the revoked token must stop authenticating"
    );
    drop(tmp);
}

/// **THE SECRET IS WHAT GATES THE GRANT.**
///
/// **Forced red** by issuing the bearer unconditionally on the welcome
/// (`bearer: Some(bearers.issue(conn_id))`): a connection that has not
/// authenticated holds a grant, and this fails with `a socket that has not
/// authenticated must hold nothing`.
#[tokio::test]
async fn the_handshake_gates_the_grant() {
    let (_m, addr, bearers, tmp) = start_with(Some("shibboleth")).await;
    let mut c = Client::connect(addr).await;
    assert_eq!(c.welcome["auth_required"], json!(true));
    assert!(
        c.welcome.get("bearer").is_none(),
        "a socket that has not authenticated must hold nothing: {}",
        c.welcome
    );
    assert_eq!(bearers.live(), 0);

    // A WRONG secret is issued nothing either.
    let mut bad = Client::connect(addr).await;
    bad.send(json!({"type":"auth","secret":"wrong"})).await;
    let ack = bad.next().await.expect("an auth_ack");
    assert_eq!(ack["ok"], json!(false));
    assert!(
        ack.get("bearer").is_none(),
        "a refused handshake issues nothing: {ack}"
    );

    // The right one is.
    c.send(json!({"type":"auth","secret":"shibboleth"})).await;
    let ack = c.next().await.expect("an auth_ack");
    assert_eq!(ack["ok"], json!(true));
    let token = ack["bearer"].as_str().expect("the grant").to_string();
    assert_eq!(bearers.live(), 1);

    // **IDEMPOTENT PER CONNECTION.** A second `auth` frame must not orphan the
    // first token by handing out a second.
    c.send(json!({"type":"auth","secret":"shibboleth"})).await;
    let again = c.next().await.expect("an auth_ack");
    assert_eq!(
        again["bearer"].as_str(),
        Some(token.as_str()),
        "a second auth returns the SAME grant, or the client holds a token this \
         registry does not know it handed out twice"
    );
    assert_eq!(bearers.live(), 1);
    drop(tmp);
}

/// **A BEARER IS READ FROM A HEADER AND FROM NOWHERE ELSE.**
///
/// The no-query rule is not about the transport: the resource endpoint takes one
/// opaque shape-issued id and never a query, because the drift risk was a
/// PARALLEL NAMESPACE. [`Bearers::verify`] takes a `HeaderMap` and has no access
/// to a URI, so a bearer in a query string is not refused — it is unreachable.
/// This test pins the refusals that remain expressible.
///
/// **Forced red** by having `verify` fall back to `headers.get("x-bearer")`
/// when `authorization` is absent: the "a bearer under any other name is not a
/// bearer" assertion fails.
#[tokio::test]
async fn only_an_authorization_header_carries_a_bearer() {
    let (_m, addr, bearers, tmp) = start().await;
    let c = Client::connect(addr).await;
    let token = c.welcome["bearer"].as_str().unwrap().to_string();

    let with = |k: &str, v: String| {
        let mut h = axum::http::HeaderMap::new();
        h.insert(
            axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            v.parse().unwrap(),
        );
        h
    };

    // No header at all.
    let r = bearers.verify(&axum::http::HeaderMap::new()).unwrap_err();
    assert_eq!(r.code, codes::UNAUTHORIZED);
    assert!(r.message.contains("Authorization: Bearer"), "{r}");

    // The right header, the right scheme.
    assert!(bearers
        .verify(&with("authorization", format!("Bearer {token}")))
        .is_ok());
    assert!(
        bearers
            .verify(&with("authorization", format!("bearer {token}")))
            .is_ok(),
        "the scheme is case-insensitive in HTTP"
    );

    // The right token, the wrong scheme.
    assert!(bearers
        .verify(&with("authorization", format!("Basic {token}")))
        .is_err());

    // A bearer under any other name is not a bearer.
    assert!(
        bearers.verify(&with("x-bearer", token.clone())).is_err(),
        "a bearer under any other name is not a bearer"
    );

    // An unknown token.
    let e = bearers
        .verify(&with("authorization", "Bearer 0123456789abcdef".into()))
        .unwrap_err();
    assert_eq!(e.code, codes::UNAUTHORIZED);
    assert!(
        !e.message.contains("0123456789abcdef"),
        "a refusal must not echo the token into whatever logs it: {e}"
    );
    drop(tmp);
}

/// **THE BEARER IS REDACTED IN `Debug` AND `Display`.** The way a secret escapes
/// is a `tracing` field or a `{:?}` on a struct that happens to contain one.
///
/// **Forced red** by `#[derive(Debug)]` on `Bearer`: both assertions fail with
/// the token in the output.
#[test]
fn a_bearer_cannot_be_logged_by_accident() {
    let b = shapewire::envelope::Bearer::from_token("s3cr3t-token");
    assert_eq!(format!("{b:?}"), "Bearer(<redacted>)");
    assert_eq!(format!("{b}"), "<redacted>");
    assert!(
        !format!("{b:?}{b}").contains("s3cr3t"),
        "a token must not reach a log through a format string"
    );
    // And it is still reachable, conspicuously.
    assert_eq!(b.expose(), "s3cr3t-token");
}

/// **THE GRANT IS PER CONNECTION, NOT PER PROCESS.** Two connections hold two
/// grants, and one closing does not revoke the other's — the failure that would
/// make the resource door unusable for a consumer that reconnects while another
/// is live.
///
/// **Forced red** by keying `Bearers` on the daemon rather than the connection
/// (issue once, ignore `conn`): the two tokens come back equal and this fails
/// with `two connections must hold two grants`.
#[tokio::test]
async fn two_connections_hold_two_grants() {
    let (_m, addr, bearers, tmp) = start().await;
    let a = Client::connect(addr).await;
    let b = Client::connect(addr).await;
    let ta = a.welcome["bearer"].as_str().unwrap().to_string();
    let tb = b.welcome["bearer"].as_str().unwrap().to_string();
    assert_ne!(ta, tb, "two connections must hold two grants");
    assert_eq!(bearers.live(), 2);

    let with = |v: String| {
        let mut h = axum::http::HeaderMap::new();
        h.insert("authorization", v.parse().unwrap());
        h
    };
    let ca = bearers.verify(&with(format!("Bearer {ta}"))).unwrap();
    let cb = bearers.verify(&with(format!("Bearer {tb}"))).unwrap();
    assert_ne!(ca, cb, "and they name two connections");

    drop(a);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert!(
        bearers.verify(&with(format!("Bearer {tb}"))).is_ok(),
        "one socket closing must not revoke another's grant"
    );
    assert!(bearers.verify(&with(format!("Bearer {ta}"))).is_err());
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 6. THE RESOURCE SEAM
// ═════════════════════════════════════════════════════════════════════════════

/// **THE RESOURCE DOOR IS BUILT, AND `threads` NOW SERVES THROUGH IT.**
///
/// This test has been rewritten twice by the thing it describes landing. Its
/// first version asserted `/resource/anything` was a 404 because no route
/// existed; its second asserted 501 `unsupported` because the route existed and
/// `threads` declared no `Resources`. **Both are superseded as of 2026-10-02:**
/// `threads` implements `Resources` for message bodies
/// (`shapes::body`, `chassis::ThreadsProducer`), so the 501 is gone from this
/// producer and an unknown id is a 404 `unknown`.
///
/// **The 501 claim is not deleted, it MOVED to the only place that can still
/// make it** — `resource_endpoint.rs::a_producer_with_no_resources_refuses_honestly_rather_than_404`,
/// which stands up a fake producer with `resources() == None`. A claim about a
/// producer that serves no resources cannot be tested against one that does, and
/// weakening it to fit would have been worse than moving it.
///
/// What it pins HERE now:
///
/// 1. The route is authenticated BEFORE it says anything about resources, so an
///    anonymous caller learns nothing about this producer's capability. This is
///    the assertion that survived both rewrites unchanged.
/// 2. An id this producer did not mint is `unknown` and 404, not `gone` and not
///    a 501 — the honest answer for a store that cannot tell *never written*
///    from *deleted*.
/// 3. There is still no collection endpoint. `/resource` is where a parallel
///    namespace starts and it does not exist.
///
/// **Forced red** by deleting the bearer check from the door: `the door
/// authenticates before it says anything about resources: HTTP/1.1 404 Not
/// Found`. And by `ThreadsProducer::resources` returning `None` again: `left:
/// 501  right: 404` on assertion 2 — the producer silently losing its door is
/// exactly what this now guards.
#[tokio::test]
async fn the_resource_door_is_mounted_and_threads_serves_message_bodies_through_it() {
    let (_m, addr, _b, tmp) = start().await;
    let get = |path: &'static str| async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).to_string()
    };
    assert!(get("/health").await.starts_with("HTTP/1.1 200"));

    // ── WITHOUT A BEARER: 401, BEFORE ANYTHING ABOUT RESOURCES IS SAID ─────
    //
    // Auth is the first gate, so an anonymous caller does not learn whether this
    // producer serves resources at all.
    let anon = get("/resource/anything").await;
    assert!(
        anon.starts_with("HTTP/1.1 401"),
        "the door authenticates before it says anything about resources: {}",
        anon.lines().next().unwrap_or_default()
    );
    assert!(anon.contains("\"code\":\"unauthorized\""), "{anon}");

    // ── WITH A BEARER: 501 `unsupported`, NOT 404 ─────────────────────────
    let c = Client::connect(addr).await;
    let bearer = c.welcome["bearer"].as_str().expect("a bearer").to_string();
    let authed = {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(
            format!(
                "GET /resource/anything HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\
                 Authorization: Bearer {bearer}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).to_string()
    };
    let status: u16 = authed
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        status, 404,
        "`threads` serves message bodies now, so an id it did not mint is an \
         UNKNOWN ID and not a producer with no door: {}",
        authed.lines().next().unwrap_or_default()
    );
    assert!(
        authed.contains(&format!("\"code\":\"{}\"", codes::UNKNOWN)),
        "and it says so with a typed code a consumer can branch on — `unknown` \
         and not `gone`, which this store cannot honestly claim: {authed}"
    );
    assert!(
        !authed.contains(&format!("\"code\":\"{}\"", codes::UNSUPPORTED)),
        "a producer that has a door must never answer `unsupported`: {authed}"
    );
    // A collection endpoint is what a parallel namespace starts as, and it does
    // not exist here either.
    assert!(get("/resource").await.starts_with("HTTP/1.1 404"));
    drop(c);
    drop(tmp);
}

/// **A BUS PUSH REACHES NO CONNECTION; THE SHAPE ARM STILL DELIVERS.**
///
/// RE-CUT AT R2.S4. Before (`the_extension_arm_and_the_shape_arm_both_run`): a
/// firehose subscriber was handed BOTH the legacy `status` push (the extension
/// arm, `ThreadsStream`) and the shape frame (the shape arm), proving neither
/// starved the other. `threads` has no extension arm now (`NoStream`), so the
/// same interleaving — one bus push, one fact — must deliver the shape frame
/// and no `status` frame.
///
/// **Forced red** by restoring `ThreadsStream` with an unconditional filter
/// pass: a `status` frame arrives and `no legacy push reaches the wire` fails.
#[tokio::test]
async fn a_bus_push_reaches_no_connection_and_the_shape_arm_still_runs() {
    let (m, addr, _b, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.request(
        "1",
        &format!("shapes/{}/subscribe", "accounts"),
        json!({"key":"first"}),
    )
    .await;
    c.drain().await;

    // One bus push and one shape fact, in that order.
    m.hub.emit(Push::Status {
        thread_id: "t".into(),
        state: "idle".into(),
        executing: false,
        queued: 0,
        unread: 0,
        detail: None,
        account: Some("first".into()),
    });
    m.hub.emit_fact(Fact::LimitCleared {
        account: "first".into(),
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let seen = c.drain().await;
    assert!(
        !seen.iter().any(|f| f["type"] == "status"),
        "no legacy push reaches the wire: {seen:#?}"
    );
    assert!(
        seen.iter().any(|f| f["type"] == "shape"),
        "and the shape arm delivers: {seen:#?}"
    );
    drop(tmp);
}

/// `shapes.list` moved into the chassis, so the counter the board reads must
/// still be at the TOP LEVEL of the payload: `ShapeInfo.counters` is
/// `#[serde(flatten)]` and a chassis that nested it would blank a live number on
/// the operator's board.
///
/// **Forced red** by having `chassis_method` answer
/// `json!({"shapes": …, "wrapped": true})` under a `list` key instead: the
/// top-level assertion fails.
#[tokio::test]
async fn shapes_list_from_the_chassis_keeps_the_boards_wire() {
    let (_m, addr, _b, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let r = c.request("1", "shapes/list", json!({})).await;
    let shapes = r["outcome"]["payload"]["shapes"]
        .as_array()
        .unwrap_or_else(|| panic!("shapes.list answers a list: {r}"));
    let accounts = shapes
        .iter()
        .find(|s| s["name"] == "accounts")
        .unwrap_or_else(|| panic!("the accounts shape: {r}"));
    assert!(
        accounts.get("unplaced_status").is_some(),
        "the board reads `unplaced_status` at the top level of each shape: {accounts}"
    );
    // `keyed` is GONE (2026-09-28). It was `Shape::KEYED`, a const read by no
    // code: `do_subscribe` iterates `Root::keys()`, so a `KEYED = false` shape
    // would have been acked and handed nothing. Every consumer that read it read
    // `true`.
    assert!(
        accounts.get("keyed").is_none(),
        "`keyed` was a constant, not information: {accounts}"
    );
    for k in ["name", "tracking", "tracked", "subscribers", "epoch", "max"] {
        assert!(accounts.get(k).is_some(), "missing `{k}`: {accounts}");
    }
    // ── RULE 2.8's LAST CLAUSE, ON THE LIVE WIRE ─────────────────────────────
    //
    // *"Every ceiling rides `shapes.list`, so a consumer can read the contract
    // it is allowed to use before it uses it."* A ceiling a consumer can only
    // discover by being refused is a contract it has to probe for.
    let max = &accounts["max"];
    assert_eq!(
        max["rows_per_subscribe"],
        json!(500),
        "the row ceiling a keyless `detail: rows` refusal names: {accounts}"
    );
    assert_eq!(max["keys_per_frame"], json!(500), "the membership page size");
    assert_eq!(
        max["future_at_secs"],
        json!(86400),
        "`MAX_FUTURE_AT`, which `BadAt` has refused against since before the rule \
         existed — the evidence that 2.8 is a rule and not a preference"
    );
    assert_eq!(max["armed_timers_per_conn"], json!(4));
    drop(tmp);
}
