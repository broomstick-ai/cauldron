//! # The golden session — the one door, pinned as bytes
//!
//! **RE-CUT AT R2.S4 (2026-10-10).** This file drove ONE scripted session
//! through BOTH servers — the legacy `:3651` router and the chassis — over the
//! same `ThreadManager` and compared the wire as bytes, because the API's move
//! into the chassis relocated a loop whose correctness lived in arm ordering and
//! comments. The legacy router is retired, so there is no second side to
//! compare against, and a comparison with one side is not a test.
//!
//! What the comparison PROTECTED is still owed, and is pinned here against
//! literals instead: the welcome's exact bytes (its key order is what the old
//! forcing broke), the auth handshake and its bearer, the request/response
//! envelope and the typed refusal, a shape subscribe and its export — and the
//! two things R2.S4 changed: a legacy `{"type":"subscribe"}` /
//! `{"type":"unsubscribe"}` is now refused `malformed` with ONE frame (no
//! `sub_ack`, no boot-note `lifecycle` replay, though `on_boot` has run), and a
//! push on the daemon's bus reaches no connection as a legacy frame.
//! `/health`'s body, which `core/tools/promote` parses, is pinned as exact bytes.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use shapewire::envelope::codes;
use threads::config::Config;
use threads::{build_manager, ThreadManager};

const SECRET: &str = "a-secret-the-session-must-satisfy";

// ─────────────────────────────────────────────────────────────────────────────
// fixture — one manager, the one server
// ─────────────────────────────────────────────────────────────────────────────

async fn start() -> (ThreadManager, SocketAddr, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "first".to_string(),
        tmp.path().join("first").to_string_lossy().to_string(),
    );
    let config = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        secret: Some(SECRET.to_string()),
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    };
    // THE CLOCK SEAM. Every `Hub::emit` stamps with the clock and every cell
    // refuses an input at or before its `folded_at`, so two emits in one
    // microsecond mean the second is DROPPED. See `src/clock.rs`.
    threads::clock::testing::ticking();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let manager = build_manager(config).unwrap();
    // **THE BOOT NOTE MUST EXIST**, or "no replay on subscribe" below would be
    // true for the trivial reason that there was nothing to replay.
    manager.update.on_boot().await;
    assert!(
        manager.update.boot_note().is_some(),
        "the session must run against a daemon that HAS a boot note"
    );
    let (addr, h) = threads::chassis::serve(Arc::new(manager.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    std::mem::forget(h);
    (manager, addr, tmp)
}

/// A client that keeps every frame as the TEXT it arrived as. Nothing is parsed
/// on the way in, because a parse is exactly the step that would hide a key
/// order difference.
struct Raw {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    log: Vec<String>,
}

impl Raw {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        let mut c = Self {
            ws,
            log: Vec::new(),
        };
        c.take(1).await; // welcome
        c
    }

    async fn send(&mut self, v: Value) {
        use tokio_tungstenite::tungstenite::Message;
        self.ws
            .send(Message::Text(v.to_string().into()))
            .await
            .unwrap();
    }

    /// Take exactly `n` text frames, in order.
    async fn take(&mut self, n: usize) -> Vec<String> {
        use tokio_tungstenite::tungstenite::Message;
        let mut out = Vec::new();
        while out.len() < n {
            let got =
                tokio::time::timeout(std::time::Duration::from_secs(5), self.ws.next()).await;
            match got {
                Err(_) => panic!("timed out waiting for frame {} of {n}", out.len() + 1),
                Ok(None) => panic!("socket closed after {} of {n} frames", out.len()),
                Ok(Some(Ok(Message::Text(t)))) => {
                    let t = t.to_string();
                    self.log.push(t.clone());
                    out.push(t);
                }
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(e))) => panic!("socket error: {e}"),
            }
        }
        out
    }

    /// Everything that arrives within a quiet window — for the steps whose frame
    /// count is what is under test.
    async fn drain(&mut self) -> Vec<String> {
        use tokio_tungstenite::tungstenite::Message;
        let mut out = Vec::new();
        loop {
            let got =
                tokio::time::timeout(std::time::Duration::from_millis(900), self.ws.next()).await;
            match got {
                Err(_) | Ok(None) => return out,
                Ok(Some(Ok(Message::Text(t)))) => {
                    let t = t.to_string();
                    self.log.push(t.clone());
                    out.push(t);
                }
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) => return out,
            }
        }
    }
}


/// The single refusal a legacy control frame now earns: no correlate, code
/// `malformed`, and the chassis's sentence naming the frame type.
fn assert_refused_frame(frames: &[String], ty: &str) {
    assert_eq!(
        frames.len(),
        1,
        "a legacy `{ty}` earns exactly ONE frame — the refusal; no `sub_ack`, and \
         no `lifecycle` boot-note replay: {frames:#?}"
    );
    let v: Value = serde_json::from_str(&frames[0]).unwrap();
    assert_eq!(v["type"], "response", "{v}");
    assert_eq!(v["correlate"], "", "{v}");
    assert_eq!(v["outcome"]["type"], "error", "{v}");
    assert_eq!(v["outcome"]["code"], json!(codes::MALFORMED), "{v}");
    assert_eq!(
        v["outcome"]["error"],
        json!(format!("invalid frame: unknown type `{ty}`")),
        "{v}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// THE SESSION
// ─────────────────────────────────────────────────────────────────────────────

/// **CONNECT, AUTH, A LEGACY SUBSCRIBE, A REQUEST, A REFUSAL, A SHAPE SUBSCRIBE,
/// A PUSH, UNSUBSCRIBE — through the one door, against literals.**
///
/// Before R2.S4 (`the_same_session_is_byte_identical_through_both_servers`):
/// every step's frames were equal, as bytes, across the legacy and chassis
/// servers, and the `subscribe` step answered a `sub_ack` plus the boot-note
/// `lifecycle` on both. Now: the welcome is pinned byte-for-byte, and the
/// legacy `subscribe` / `unsubscribe` steps each answer ONE `malformed`
/// refusal.
///
/// **Forced red** (R2.S4) by restoring the chassis's legacy control half
/// (answering `subscribe` with a `sub_ack` and the boot note): the
/// `subscribe` step fails with ``a legacy `subscribe` earns exactly ONE frame —
/// the refusal; no `sub_ack`, and no `lifecycle` boot-note replay:
/// ["{\"firehose\":true,\"threads\":[],\"type\":\"sub_ack\"}", …]``.
#[tokio::test]
async fn the_session_through_the_one_door_is_pinned() {
    let (m, addr, _tmp) = start().await;
    let mut a = Raw::connect(addr).await;

    // ── 1. CONNECT ──────────────────────────────────────────────────────────
    // Exact bytes: key ORDER is the thing a field reshuffle breaks, and a
    // socket with a secret to satisfy is issued no bearer on the welcome.
    assert_eq!(
        a.log[0],
        format!(
            r#"{{"type":"welcome","proto":1,"daemon":"threads","daemon_version":"{}","boot_id":"{}","auth_required":true}}"#,
            env!("CARGO_PKG_VERSION"),
            m.boot_id
        ),
        "the welcome, as bytes"
    );

    // ── 2. AUTH ─────────────────────────────────────────────────────────────
    a.send(json!({"type":"auth","secret":SECRET})).await;
    let ack = a.take(1).await;
    assert!(
        ack[0].starts_with(r#"{"type":"auth_ack","ok":true,"bearer":""#),
        "the grant rides the auth_ack: {}",
        ack[0]
    );

    // ── 3. A LEGACY SUBSCRIBE — refused, and nothing replayed ───────────────
    a.send(json!({"type":"subscribe","threads":null})).await;
    assert_refused_frame(&a.drain().await, "subscribe");

    // ── 4. A REQUEST ────────────────────────────────────────────────────────
    a.send(json!({"type":"request","id":"r1","method":"threads/list","params":{}}))
        .await;
    let r = a.take(1).await;
    assert!(
        r[0].starts_with(r#"{"type":"response","correlate":"r1","outcome":{"type":"success","payload":"#),
        "{}",
        r[0]
    );

    // ── 4b. A REFUSED REQUEST — the sentence and the typed code ─────────────
    a.send(json!({"type":"request","id":"r2","method":"threads.nope","params":{}}))
        .await;
    let r = a.take(1).await;
    assert!(
        r[0].contains("unknown method: threads.nope") && r[0].contains(r#""code":"unknown_method""#),
        "{}",
        r[0]
    );

    // ── 5. A SHAPE SUBSCRIBE AND ITS EXPORT ─────────────────────────────────
    a.send(json!({"type":"request","id":"r3","method":"shapes/accounts/subscribe",
                  "params":{"key":"first",
                            "consumer":{"name":"threads-tests","shape_version":"shapes.md@rev4"}}}))
        .await;
    let la = a.drain().await;
    assert!(
        la.len() >= 2
            && la[0].contains(r#""correlate":"r3""#)
            && la.iter().any(|f| f.contains(r#""type":"shape""#) && f.contains(r#""op":"read""#)),
        "a cold subscribe answers {{queued}} and then exports; got {la:#?}"
    );

    // ── 6. A PUSH ON THE BUS — reaches no connection as a legacy frame ──────
    m.hub.emit(threads::event::Push::Status {
        thread_id: "t-golden".into(),
        state: "idle".into(),
        executing: false,
        queued: 0,
        unread: 0,
        detail: Some("the golden session".into()),
        account: Some("first".into()),
    });
    let la = a.drain().await;
    assert!(
        !la.iter().any(|f| f.contains(r#""type":"status""#)),
        "there is no firehose: a bus push must not reach the wire as a legacy frame: {la:#?}"
    );

    // ── 7. UNSUBSCRIBE (both kinds) ─────────────────────────────────────────
    a.send(json!({"type":"request","id":"r4","method":"shapes/accounts/unsubscribe",
                  "params":{"key":"first"}}))
        .await;
    let r = a.take(1).await;
    assert_eq!(
        r[0],
        r#"{"type":"response","correlate":"r4","outcome":{"type":"success","payload":{"ok":true}}}"#
    );
    a.send(json!({"type":"unsubscribe"})).await;
    assert_refused_frame(&a.drain().await, "unsubscribe");

    // ── 8. CLOSE ────────────────────────────────────────────────────────────
    use tokio_tungstenite::tungstenite::Message;
    a.ws.send(Message::Close(None)).await.unwrap();
}

/// **`/health` IS A DEPLOYMENT DEPENDENCY.** `core/tools/promote` reads this
/// body (it hardcodes `http://127.0.0.1:3651/health` today — owed a move to
/// `:3653` with R2.S4); breaking the path or a key does not fail a deploy, it
/// destroys the ability to confirm what is running.
///
/// Before R2.S4 (`health_is_byte_identical_and_carries_exactly_the_six_keys`):
/// the legacy and chassis bodies were equal as bytes, and the six keys were
/// named. Now: the chassis body equals a LITERAL, as bytes, so the key order the
/// old comparison protected is pinned without a second server.
///
/// **Forced red** (pre-R2.S4, and again at R2.S4) by declaring `HealthWire`'s
/// fields with `status` first: `left: "{\"status\":\"ok\",\"boot_id\":…}"`
/// against `right: "{\"boot_id\":…,\"status\":\"ok\",\"version\":…}"`.
#[tokio::test]
async fn health_is_pinned_as_bytes_and_carries_exactly_the_six_keys() {
    let (m, addr, _tmp) = start().await;

    // No http client in this crate's deps; one request by hand is cheaper.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8(buf).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").expect("a response with a body");
    assert!(head.starts_with("HTTP/1.1 200"), "/health must answer 200: {head}");

    assert_eq!(
        body,
        format!(
            // R9.C7: the six fixed keys, then ONE producer extra,
            // `restart_gate` — the update gate's counters, which cauldron reads
            // before a re-pin restart. All zero and both predicates true here:
            // nothing runs in this fixture, and reading `/health` takes no
            // request permit, so the read cannot make itself busy. R9.C7b:
            // `turn_threads`, the ids of the running turns (none here).
            // R9.C7g: and `recovery` — boot recovery's progress, `done` here
            // because this fixture never begins one (only `run_daemon` does).
            // R9.C7h: and `recovery.migrated_ms` — time the store spent
            // migrating behind the port before recovery began (0 here).
            r#"{{"boot_id":"{}","daemon":"threads","graceful_shutdown":true,"proto":1,"status":"ok","version":"{}","recovery":{{"children_pending":0,"children_resumed":0,"children_total":0,"elapsed_ms":0,"migrated_ms":0,"phase":"done","state":"done"}},"restart_gate":{{"draining":false,"draining_since":null,"idle":true,"quiet":true,"requests":0,"spawning":0,"turn_threads":[],"turns":0}}}}"#,
            m.boot_id,
            env!("CARGO_PKG_VERSION")
        ),
        "the body `promote` reads, as bytes"
    );
}
