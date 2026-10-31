//! WS round-trip tests (CCW's fake-claude pattern): each starts the threads
//! daemon in-process on an ephemeral port with the FAKE `claude` fixture
//! (canned stream-json — NO real invocation), connects a real WebSocket
//! client, and asserts the locked contract: boot-id welcome, optional auth,
//! full-text user messages with metadata + spoken ids, timestamped history,
//! liveness-never-persisted, queue-while-busy delivery, persisted retries
//! (schedule + restart re-arm), and terminal failure events.
//!
//! NOTE: a REAL-claude burn-in remains the blocking phase-0 gate (design doc
//! §9.2) — these tests prove the daemon's contract, not claude's behavior.


#[path = "support/bus.rs"]
mod bus;
#[path = "support/keychain.rs"]
mod keychain;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use futures::{stream::SplitSink, stream::SplitStream, SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use threads::{build_manager, server, Config};

/// **THE UNDELIVERED QUEUE, READ OFF THE `queue` CELL AND FILTERED.**
///
/// This is what `threads/{id}/queue` and `threads/{id}/queue_failed` used to
/// answer, and both methods are gone. They projected exactly the same four
/// fields out of `store::QueueRow` and differed in ONE `WHERE state=`, so the
/// discriminator their NAMES encoded is now a declared field with a closed
/// vocabulary — `shapes::threads::QueueEntry` and `QUEUE_STATE_WORDS` — and a
/// reader FILTERS instead of choosing a method per panel.
///
/// **A FRESH CONNECTION PER READ, deliberately.** The cell arrives as a shape
/// frame, and this file's client sits on the firehose with a `pending` queue
/// that the push assertions walk in order; draining shape frames out of that
/// queue would couple every queue read to whatever push happened to be in
/// flight.
///
/// **A KNOWN-EMPTY QUEUE IS `[]`, NEVER A MISSING KEY.** The cell is `Required`
/// and `shapes::threads`' hydrate maps `Hydration::Empty` onto an empty list,
/// so the panic below fires only if the shape stopped carrying the cell at all
/// — which is the thing worth failing on, and which is why this reads the key
/// rather than defaulting it away.
async fn queue_rows(addr: SocketAddr, id: &str, state: &str) -> Vec<Value> {
    let (ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
    let (mut wr, mut rd) = ws.split();
    let _welcome = rd.next().await;
    wr.send(Message::Text(
        json!({
            "type": "request",
            "id": "queue-cell",
            "method": "shapes/threads/subscribe",
            "params": {
                "key": id,
                // **COMPULSORY SINCE 2026-10-08.** An absent or half-filled
                // identity is REFUSED by this door, and the refusal emits no
                // export — so a helper that omitted it would wait out the full
                // timeout below for what is a missing parameter.
                "consumer": {
                    "name": "threads-tests",
                    "shape_version": "shapes.md@rev4",
                },
            },
        })
        .to_string(),
    ))
    .await
    .unwrap();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(20), rd.next())
            .await
            .expect("timed out waiting for the `threads` export")
            .expect("socket closed before the export")
            .expect("socket error");
        if let Message::Text(t) = msg {
            let f: Value = serde_json::from_str(&t).unwrap();
            // **A REFUSAL IS NAMED RATHER THAN WAITED OUT.** This door answers a
            // subscribe it refuses with an error outcome and then emits nothing,
            // so a reader that watched only for shape frames would report a
            // twenty-second timeout for what is a one-line parameter mistake.
            if f["type"] == "response"
                && f["correlate"] == json!("queue-cell")
                && f["outcome"]["type"] != json!("success")
            {
                panic!("the `threads` subscribe was refused: {}", f["outcome"]);
            }
            if f["type"] == "shape" && f["op"] == "read" && f["key"] == json!(id) {
                return f["row"]["queue"]
                    .as_array()
                    .unwrap_or_else(|| {
                        panic!("the `threads` shape must carry the `queue` cell: {f}")
                    })
                    .iter()
                    .filter(|e| e["state"] == json!(state))
                    .cloned()
                    .collect();
            }
        }
    }
}


type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

/// Set the process-global test env ONCE to fixed values (identical across
/// every test so parallel tests never race). Turn MODE is chosen from the
/// prompt, not env, precisely so this can stay shared.
fn set_env() {
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_HEARTBEAT_MS", "100");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "300");
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    // The machine-default account's transcript store is `<HOME>/.claude` — the
    // OPERATOR'S OWN login. Point the transcript home at a scratch dir so no
    // test can read or write inside it (account moves now port transcripts).
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// Start the daemon on 127.0.0.1:0; returns the manager (for direct store /
/// recover access), the bound address, and the live temp data dir.
async fn start_with(secret: Option<&str>) -> (threads::ThreadManager, SocketAddr, tempfile::TempDir) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // CON-27: `default` is the operator's MACHINE login and is never a place a
    // thread may be PUT — `ensure_default` registers it on every boot, but it
    // has no config dir of its own, so a spawn there would land in the
    // operator's `~/.claude`. A daemon whose registry holds nothing else has no
    // placeable account at all, and `threads.create` refuses outright. So every
    // daemon here registers exactly one account threads OWNS. `aa-owned` sorts
    // first, so it takes the ranking slot `default` used to occupy by accident
    // — and, being the only PLACEABLE one, it also keeps the "single account ⇒
    // no failover" shape the retry tests below depend on.
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        tmp.path().join("aa-owned").to_string_lossy().to_string(),
    );
    let config = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        secret: secret.map(str::to_string),
        ..Default::default()
    };
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let manager = build_manager(config).unwrap();
    let (addr, _handle) = threads::chassis::serve(std::sync::Arc::new(manager.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (manager, addr, tmp)
}

/// A minimal WS test client: request/response + a buffered push stream.
struct Client {
    wr: SplitSink<Ws, Message>,
    rd: SplitStream<Ws>,
    pending: VecDeque<Value>,
    next_id: u64,
    /// The push bus, once `subscribe_all` has opened it.
    bus: Option<bus::Bus>,
    /// How long `next_frame` waits for one frame. 10 s by default; a request
    /// whose answer is known to be slow under load raises it for itself
    /// (R4.S4b, HOLDS R25: `daemon/image` hashes the running image and took
    /// ~20 s on a loaded machine).
    frame_timeout: Duration,
}

impl Client {
    async fn connect(addr: SocketAddr) -> (Self, Value) {
        let (ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
        let (wr, rd) = ws.split();
        let mut c = Self {
            wr,
            rd,
            pending: VecDeque::new(),
            next_id: 0,
            bus: None,
            frame_timeout: Duration::from_secs(10),
        };
        let welcome = c.next_frame().await;
        (c, welcome)
    }

    async fn send_raw(&mut self, v: Value) {
        self.wr
            .send(Message::Text(v.to_string()))
            .await
            .unwrap();
    }

    async fn next_frame(&mut self) -> Value {
        loop {
            let msg = tokio::time::timeout(self.frame_timeout, self.rd.next())
                .await
                .expect("timed out waiting for frame")
                .expect("socket closed")
                .expect("socket error");
            match msg {
                Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Close(_) => panic!("unexpected close"),
                other => panic!("unexpected frame {other:?}"),
            }
        }
    }

    /// Send a request; buffer pushes until the response arrives.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = format!("r{}", self.next_id);
        self.send_raw(json!({"type": "request", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let f = self.next_frame().await;
            if f["type"] == "response" {
                assert_eq!(f["correlate"], id.as_str());
                return f["outcome"].clone();
            }
            self.pending.push_back(f);
        }
    }

    async fn request_ok(&mut self, method: &str, params: Value) -> Value {
        let outcome = self.request(method, params).await;
        assert_eq!(outcome["type"], "success", "outcome: {outcome}");
        outcome["payload"].clone()
    }

    /// **THE DAEMON'S PUSHES, FROM ITS OWN BUS** (R2.S4). This sent
    /// `{"type":"subscribe"}` and read the legacy firehose off the socket; that
    /// door is retired. The bus is opened here, where the subscribe was, so the
    /// ordering the tests rely on is unchanged — see `support/bus.rs`.
    async fn subscribe_all(&mut self, m: &threads::ThreadManager) {
        self.bus = Some(bus::Bus::open(m));
    }

    /// The next push frame matching `pred` (drains the buffer first).
    async fn wait_push(&mut self, pred: impl FnMut(&Value) -> bool) -> Value {
        self.bus
            .as_mut()
            .expect("subscribe_all opens the bus before anything waits on it")
            .wait(pred)
            .await
    }

    /// The next EVENT frame of the given kind.
    async fn wait_event(&mut self, kind: &str) -> Value {
        self.wait_push(|f| f["type"] == "event" && f["event"]["kind"] == kind)
            .await
    }
}

fn settings(cwd: &std::path::Path) -> Value {
    json!({"cwd": cwd.to_string_lossy(), "model": "sonnet"})
}

// ── R2.S3c (R4.S9, HOLDS R9): the retired reads' derivations ─────────────────
//
// `accounts/pool_share_get`, `accounts/pressure` and `threads/status` were
// retired from the door: no consumer sent them (the board reads the
// `accounts` and `threads` shapes). The tests below used them to OBSERVE what
// a write did, so they now ask the SAME derivation each arm answered with —
// the arm bodies, verbatim — and the retirement itself is pinned in
// `chassis_api.rs::the_retired_reads_answer_unknown_method_and_the_kept_ones_still_answer`.

fn share_get(m: &threads::ThreadManager, account: Option<&str>) -> Value {
    let accounts = match account {
        Some(a) => vec![a.to_string()],
        None => m.account_pool(),
    };
    m.budgets.pool_share_rows(&accounts)
}

fn pressure_get(m: &threads::ThreadManager) -> Value {
    m.budgets.pressure_rows(&m.account_pool(), chrono::Utc::now())
}

fn statuses_get(m: &threads::ThreadManager) -> Value {
    let ids: Vec<String> = m.store.list_threads(200).unwrap().into_iter().map(|r| r.id).collect();
    json!({ "statuses": m.thread_statuses(&ids).unwrap() })
}

#[tokio::test]
async fn welcome_carries_boot_id_and_proto() {
    let (manager, addr, _tmp) = start_with(None).await;
    let (_c, welcome) = Client::connect(addr).await;
    assert_eq!(welcome["type"], "welcome");
    assert_eq!(welcome["daemon"], "threads");
    assert_eq!(welcome["proto"], 1);
    assert_eq!(welcome["auth_required"], false);
    // §9.3: the boot id distinguishes a daemon restart from a socket blip.
    assert_eq!(welcome["boot_id"], manager.boot_id.as_str());
}

/// `daemon.image` answers over the real wire, and `daemon.status` no longer
/// carries the field that misled three investigations.
///
/// Under the test harness the "running image" is the test binary, which is not
/// the daemon and has no boot identity — so this also proves the endpoint
/// stays honest in the case where most of what it is asked is unanswerable.
#[tokio::test]
async fn daemon_image_reports_the_running_image_and_status_no_longer_says_binary() {
    let (_m, addr, _tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;

    // R4.S4b (HOLDS R25): this answer hashes the running image; under load it
    // took ~20 s against the 10 s frame timeout and flaked 1/3. 120 s is six
    // times the slowest observed answer.
    c.frame_timeout = Duration::from_secs(120);
    let out = c.request("daemon/image", json!({})).await;
    assert_eq!(out["type"], "success", "daemon.image must be on the wire: {out}");
    let p = &out["payload"];

    // The kernel answers these about the process that is executing, whatever
    // is or is not at any path.
    assert_eq!(
        p["running"]["code_signature"]["cdhash"]["known"], true,
        "the running image's cdhash must come from the kernel: {}",
        p["running"]["code_signature"]["cdhash"]
    );
    assert_eq!(
        p["running"]["code_signature"]["ad_hoc"]["known"], true,
        "whether the RUNNING image is ad-hoc must be answerable"
    );
    assert_eq!(
        p["running"]["text_inode"]["known"], true,
        "the executing text inode must be answerable"
    );
    assert_eq!(p["running"]["pid"], std::process::id());

    // Every section the operator asked for is present.
    for k in ["running", "launch_path", "divergence", "absorbable", "boot"] {
        assert!(p.get(k).is_some(), "the payload must carry `{k}`");
    }

    // The two boot values are distinguished, and the payload says which one is
    // greppable — a prior report cited boot identifiers that were nowhere on
    // disk because they were the in-memory kind.
    assert!(p["boot"]["boot_id"]["recorded_where"]
        .as_str()
        .unwrap()
        .contains("IN MEMORY ONLY"));
    assert!(p["boot"]["boot_identity"]["recorded_where"]
        .as_str()
        .unwrap()
        .contains("restarts.jsonl"));

    // (`daemon/status`'s corrected field was asserted here; R2.S3c — R4.S9,
    // HOLDS R9 — retired that read, so `daemon/image` above is the one answer.)
}

#[tokio::test]
async fn auth_gate_when_secret_configured() {
    let (_m, addr, tmp) = start_with(Some("hunter2")).await;
    let (mut c, welcome) = Client::connect(addr).await;
    assert_eq!(welcome["auth_required"], true);

    // Unauthenticated requests are refused.
    let outcome = c.request("threads/list", json!({})).await;
    assert_eq!(outcome["type"], "error");
    assert!(outcome["error"].as_str().unwrap().contains("unauthorized"));

    // Correct secret unlocks the connection.
    c.send_raw(json!({"type": "auth", "secret": "hunter2"})).await;
    let ack = c.next_frame().await;
    assert_eq!(ack["type"], "auth_ack", "{ack}");
    assert_eq!(ack["ok"], true);
    let payload = c.request_ok("threads/list", json!({})).await;
    assert!(payload["threads"].is_array());
    drop(tmp);
}

#[tokio::test]
async fn full_turn_roundtrip_with_ids_metadata_and_history() {
    let (_m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    // Q2: the caller supplies the thread id (session adoption).
    let my_id = "cccccccc-0000-0000-0000-000000000001";
    let payload = c
        .request_ok(
            "threads/create",
            json!({"thread_id": my_id, "settings": settings(tmp.path())}),
        )
        .await;
    assert_eq!(payload["thread_id"], my_id);

    // Q3 + G18: full text + arbitrary metadata, spoken id returned.
    let payload = c
        .request_ok(
            &format!("threads/{}/send", my_id),
            json!({
                                "text": "hello agent, this text must be stored whole",
                "metadata": {"audio_start": "10:00:00", "audio_end": "10:00:07"},
            }),
        )
        .await;
    let msg_id = payload["message_id"].as_u64().unwrap();
    assert_eq!(msg_id, 1);

    let um = c.wait_event("user_message").await;
    assert_eq!(um["message_id"], 1);
    assert_eq!(um["event"]["text"], "hello agent, this text must be stored whole");
    assert_eq!(um["event"]["metadata"]["audio_end"], "10:00:07");

    let ts = c.wait_event("turn_started").await;
    assert_eq!(ts["event"]["message_ids"], json!([1]));

    // The injected first-turn preamble is itself on the record.
    let ci = c.wait_event("context_injected").await;
    assert!(ci["event"]["text"].as_str().unwrap().contains(my_id));

    let at = c.wait_event("assistant_text").await;
    assert_eq!(at["event"]["text"], "Hello from fake claude.");
    assert_eq!(at["event"]["api_message_id"], "msg_fake_01"); // G4
    assert!(at["message_id"].as_u64().unwrap() >= 2); // spoken id assigned

    c.wait_event("result_turn").await;
    let te = c.wait_event("turn_ended").await;
    assert_eq!(te["event"]["exit_code"], 0);

    // Liveness arrived as TRANSIENT frames…
    let lv = c.wait_push(|f| f["type"] == "liveness").await;
    assert_eq!(lv["thread_id"], my_id);

    // …and never as ledger rows (G6): replayed history has no liveness kind,
    // and every row carries ts + seq (G5).
    let hist = c
        .request_ok(&format!("threads/{}/history", my_id), json!({"after_seq": 0}))
        .await;
    let events = hist["events"].as_array().unwrap();
    assert!(!events.is_empty());
    for e in events {
        assert_ne!(e["event"]["kind"], "liveness");
        assert!(e["ts"].is_string());
        assert!(e["seq"].is_u64());
    }

    // G20: retrieval by spoken id. Read off the store, in-process, since
    // R2.S3a retired `threads/{id}/messages` from the wire (no caller).
    let msgs = _m
        .store
        .events_by_range(&my_id, Some(1), Some(1), None, None)
        .unwrap();
    assert_eq!(serde_json::to_value(&msgs[0].event).unwrap()["kind"], "user_message");

    // Q11: spend landed in the ledger. Read off the store for the same reason:
    // `ledger/query` was retired at R2.S3a.
    let spend = _m.store.spend_summary(false).unwrap();
    assert_eq!(spend[0].key, my_id);
    assert!(spend[0].total_tokens > 0);
}

#[tokio::test]
async fn queue_while_busy_delivers_after_turn(){
    let (_m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();

    // First send holds the child for ~700ms; the second must QUEUE (G7 —
    // CCW rejected it) and deliver when the parent finishes.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "slowturn please"}))
        .await;
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "queued while busy"}))
        .await;

    let t1 = c.wait_event("turn_started").await;
    assert_eq!(t1["event"]["message_ids"], json!([1]));
    c.wait_event("turn_ended").await;

    let t2 = c.wait_event("turn_started").await;
    assert_eq!(t2["event"]["message_ids"], json!([2]));
    c.wait_event("turn_ended").await;
}

#[tokio::test]
async fn ratelimit_schedules_a_persisted_retry() {
    let (manager, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&manager).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "ratelimit me"}))
        .await;

    // The synthetic 429 streams through as limit_hit…
    let lh = c.wait_event("limit_hit").await;
    assert_eq!(lh["event"]["status"], 429);

    // …and (single account → no failover) becomes a PERSISTED deadline (Q5).
    let rs = c.wait_event("retry_scheduled").await;
    assert_eq!(rs["event"]["error_kind"], "usage_limit");
    assert_eq!(rs["event"]["attempt"], 2);
    assert!(rs["event"]["retry_at"].is_string());

    let row = manager.store.get_retry(&id).unwrap().expect("retry row persisted");
    assert_eq!(row.message_ids, vec![1]);
    assert!(row.prompt.contains("ratelimit me"));

    // §10 scaffolding: the account's limit window was recorded. The window is
    // recorded against the account the turn was SERVED on, and after CON-27
    // that is never the machine `default` — it is the one account threads owns
    // here, which sorts first in the listing.
    // `accounts/list` until R2.S3c (R4.S9, HOLDS R9) retired it; its
    // `available` was `AccountState::available(now)` over this same store row.
    let now = threads::store::now_rfc3339();
    let serving = manager
        .store
        .list_account_states()
        .unwrap()
        .into_iter()
        .find(|s| s.name == "aa-owned")
        .expect("the window is recorded against the account that served the turn");
    assert!(!serving.available(&now), "aa-owned is inside its limit window");
}

#[tokio::test]
async fn transient_failure_retries_with_backoff_then_succeeds() {
    let (_m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": format!("failonce {id}")}),
    )
    .await;

    c.wait_event("turn_started").await;
    // The first spawn dies (exit 1) and schedules a persisted deadline.
    let te1 = c.wait_event("turn_ended").await;
    assert_eq!(te1["event"]["exit_code"], 1);
    let rs = c.wait_event("retry_scheduled").await;
    assert_eq!(rs["event"]["error_kind"], "transient");

    // The armed timer fires (backoff attempt 2 = 2s) and the retry succeeds.
    let t2 = c.wait_event("turn_started").await;
    assert_eq!(t2["event"]["attempt"], 2);
    c.wait_event("assistant_text").await;
    let te2 = c.wait_event("turn_ended").await;
    assert_eq!(te2["event"]["exit_code"], 0);
}

/// **Credit exhaustion bars the ACCOUNT and retries the TURN.**
///
/// This test used to be `credit_balance_fails_terminally` and asserted the exact
/// opposite: *"G8+G9: a terminal marker, correctly classified, never retried."*
/// That was a faithful test of a decision the operator has since reversed, and
/// it is rewritten rather than deleted so the reversal is visible in the history
/// of the thing that pinned it.
///
/// > *"credit limit exhaustion means that that account is no longer open for
/// > placement, but that means that another account might be open for placement.
/// > So it does deserve a retry."*
///
/// What survives from the old test is everything G9 was actually right about:
/// the classification is still specific (`credit_balance`, not an anonymous
/// error), and the account really is out of action. What changes is the scope —
/// the account is barred, the delivery is not destroyed.
#[tokio::test]
async fn credit_balance_bars_the_account_and_retries_the_turn() {
    let (m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    let account = m.serving_account(&id).unwrap();
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "creditfail"}))
        .await;

    // THE TURN IS RETRIED, still under its own specific classification. The old
    // assertion looked for `turn_failed` here; a `retry_scheduled` arriving
    // instead is the whole of the ruling.
    let rs = c.wait_event("retry_scheduled").await;
    assert_eq!(
        rs["event"]["error_kind"], "credit_balance",
        "an exhausted balance keeps its own error class — it does not become an \
         anonymous transient just because it is now retried"
    );

    // THE ACCOUNT IS BARRED, through the one existing bar (`accounts.limit_until`,
    // surfaced by `unavailable_accounts`), not through a parallel mechanism.
    let (unavailable, unhealthy) = m.barred_accounts();
    assert!(
        unavailable.contains(&account),
        "the account that reported no credit must leave the placement pool: {unavailable:?}"
    );
    // …and it is NOT marked a dead login. An empty wallet is a billing state,
    // not a broken OAuth session, and conflating them would need a human `/login`
    // that fixes nothing.
    assert!(
        unhealthy.is_empty(),
        "a credit balance must not flag the login dead: {unhealthy:?}"
    );
}

#[tokio::test]
async fn persisted_retry_rearms_across_daemon_restart() {
    // G10 end-to-end: a parked prompt laid down by a "previous daemon run" fires
    // after recover() — the boot half of "we can restart AUI (or threads) and
    // never experience a thread crash".
    //
    // WHAT RE-ARMS IT MOVED. `recover` used to spawn one timer per retry row,
    // staggered so N past-due floors did not stampede. The operator removed the
    // per-thread timestamp those timers slept on, so boot now rings the POOL'S
    // cadence once (`cadence_rearm`) and the placement task computes a rung from the
    // rows it finds. This row is `transient` at attempt 2, so that rung is
    // `transient_backoff_secs(2)` — five seconds — and the pass that fires then is a
    // DEADLINE, which is why it takes the lock and runs rather than asking a question
    // the gate would answer for free.
    let (manager, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&manager).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();

    manager
        .store
        .upsert_retry(&threads::store::RetryRow {
            thread_id: id.clone(),
            prompt: "hello from a previous life".into(),
            message_ids: vec![1],
            attempt: 2,
            kind: "transient".into(),
            reason: "prior overload".into(),
            account: None,
            created_at: String::new(),
        })
        .unwrap();

    manager.recover().await;

    let ts = c.wait_event("turn_started").await;
    assert_eq!(ts["event"]["attempt"], 2);
    assert_eq!(ts["event"]["message_ids"], json!([1]));
    c.wait_event("turn_ended").await;
    // Success clears the persisted deadline.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(manager.store.get_retry(&id).unwrap().is_none());
}

#[tokio::test]
async fn subagent_liveness_outlasts_parent_result() {
    let (_m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    c.request_ok(&format!("threads/{}/send", id), json!({"text": format!("subagent {id}")}))
        .await;

    c.wait_event("agent_launched").await;
    c.wait_event("result_turn").await;
    // Busy-while-subagent-outstanding: a liveness beat with busy=true even
    // though the parent result arrived (the idle-illusion guard).
    let lv = c
        .wait_push(|f| f["type"] == "liveness" && f["busy"] == true && !f["running_subagents"].as_array().unwrap().is_empty())
        .await;
    assert_eq!(lv["running_subagents"][0], "agent-abc");
    // R4.S2: the mtime sweep is deleted. The parent's PROCESS EXITING is what
    // completes an agent still listed — a native agent cannot outlive it.
    c.wait_event("agent_completed").await;
}

#[tokio::test]
async fn settings_update_mid_thread_changes_model() {
    let (_m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();

    // Q9/Q10: live-editable settings blob.
    // **AN ACKNOWLEDGEMENT, NOT THE SETTINGS** (rule 2.4). The blob arrives on
    // the `settings_changed` ledger event and on the `threads` shape's
    // `settings` cell; the reply says only that the write took.
    let payload = c
        .request_ok(
            &format!("threads/{}/update", id),
            json!({"patch": {"model": "fable"}}),
        )
        .await;
    assert!(payload["promise_id"].is_u64(), "{payload}");
    assert_eq!(payload["changed"], true, "{payload}");
    assert!(payload.get("applied").is_none(), "{payload}");
    let sc = c.wait_event("settings_changed").await;
    assert_eq!(sc["event"]["patch"]["model"], "fable");

    // `threads/{id}/get` until R2.S3c (R4.S9, HOLDS R9) retired it. Its row
    // was `thread_json`, which is exactly what each `threads/list` row is (the
    // kept read `threads-stress` sends), so the same row is read off the list.
    let list = c.request_ok("threads/list", json!({"limit": 1000})).await;
    let got = list["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["thread_id"] == id.as_str())
        .cloned()
        .unwrap_or_else(|| panic!("{id} missing from threads/list: {list}"));
    assert_eq!(got["model"], "fable");
    assert_eq!(got["settings"]["model"], "fable");

    // ── THE COMPACTION MARKER IS ON THE RPC ROW, NOT ONLY ON THE SHAPE ──────
    //
    // Another process needs to know where the last compaction ended. The
    // `threads` SHAPE has carried this all along (its `compaction` cell holds
    // both durable fields), but this row rendered twelve fields and not these
    // two — so a consumer on the RPC path could read `last_seq` and
    // `next_message_id` and have no way to ask the question, even though
    // `ThreadRow` has read the column since it was written.
    //
    // ABSENT, not zero, is what the test pins. A missing key and a `0` are
    // different answers: `0` is a real boundary meaning "nothing has been
    // compacted yet", so a consumer that cannot tell them apart would read a
    // fresh thread and a daemon that does not report as the same thing.
    assert_eq!(
        got["last_compaction_message_id"], 0,
        "a thread that has never compacted reports the boundary as 0, not as absent: {got}"
    );
    assert!(
        got.get("last_compaction_message_id").is_some(),
        "the boundary must be PRESENT — a consumer cannot distinguish a missing \
         key from a daemon that does not answer the question: {got}"
    );
    assert_eq!(
        got["needs_restate"], false,
        "and the flag beside it, because `update_thread_compaction` writes the \
         two TOGETHER: a boundary without the flag does not say whether the \
         producer still owes a restatement: {got}"
    );
}

#[tokio::test]
async fn custom_events_land_in_the_ledger() {
    let (_m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();

    // §11: application-defined thread events, addressable on request.
    // **THE REPLY IS AN ACKNOWLEDGEMENT** (rule 2.4, 2026-09-28) — it used to
    // carry `message_id`, the allocated addressable position, which is a fact
    // the `threads` shape holds as `last_message_id`. The position arrives on
    // the ledger event, which is where it always arrived; the reply is now only
    // the promise.
    let payload = c
        .request_ok(
            &format!("threads/{}/event", id),
            json!({"event_type": "summary_updated",
                   "data": {"summary": "the operator's summary"}, "addressable": true}),
        )
        .await;
    assert!(payload["promise_id"].is_u64(), "{payload}");
    assert!(
        payload.get("message_id").is_none(),
        "the position is not a reply field: {payload}"
    );

    let ev = c.wait_event("custom").await;
    assert_eq!(ev["event"]["event_type"], "summary_updated");
    let mid = ev["message_id"].as_u64().unwrap();

    // Read off the store, in-process: `threads/{id}/messages` was retired from
    // the wire at R2.S3a (no caller).
    let msgs = _m
        .store
        .events_by_range(&id, Some(mid), Some(mid), None, None)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&msgs[0].event).unwrap()["data"]["summary"],
        "the operator's summary"
    );
}

// ── accounts.status (§10) ────────────────────────────────────────────────────

/// A daemon on its own temp data dir with a TWO-account registry, plus a
/// `config.toml` written into that dir so the `threads` CLI can be pointed at
/// the very same store. Nothing here resolves — let alone creates — `~/.threads`.
async fn start_two_account() -> (
    threads::ThreadManager,
    SocketAddr,
    tempfile::TempDir,
    PathBuf,
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let mut accounts = std::collections::BTreeMap::new();
    // A config dir that does not exist is fine: nothing in this test spawns
    // claude, and the fake fixture ignores CLAUDE_CONFIG_DIR anyway. It only
    // has to be a second, distinct registry entry.
    // TWO POOL ACCOUNTS, both named. The second one used to be the machine
    // default, supplied for free by `ensure_default` — which meant this
    // fixture's "two accounts" were one real account and the operator's own
    // login. The daemon no longer registers that row (2026-08-27), so both are
    // stated.
    accounts.insert(
        "first".to_string(),
        tmp.path().join("first").to_string_lossy().to_string(),
    );
    accounts.insert(
        "second".to_string(),
        tmp.path().join("second").to_string_lossy().to_string(),
    );
    let config = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        ..Default::default()
    };
    // `build_manager` persists the merged registry to `<data_dir>/accounts.toml`
    // — the file the CLI reads — so both surfaces see the same two accounts.
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let manager = build_manager(config).unwrap();
    let cfg_path = tmp.path().join("config.toml");
    std::fs::write(&cfg_path, format!("data_dir = \"{}\"\n", tmp.path().display())).unwrap();
    let (addr, _handle) = threads::chassis::serve(std::sync::Arc::new(manager.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (manager, addr, tmp, cfg_path)
}

#[tokio::test]
async fn accounts_status_cli_counts_what_is_true() {
    // The per-account activity roll-up had two surfaces — the `accounts/status`
    // WS method and `threads accounts status --json` — pinned byte-identical
    // here. R2.S3c (R4.S9, HOLDS R9) retired the WS method (no consumer sent
    // it), so the CLI is the one surface left and the payload facts below are
    // asserted on ITS output, over the same seeded store.
    use threads::store::{now_rfc3339, ParkedTurnRow, RetryRow, RunningTurnRow, ThreadRow};

    let (m, addr, tmp, cfg) = start_two_account().await;
    let now = now_rfc3339();

    // Threads are seeded DIRECTLY into the store: this surface is a pure read
    // of durable state, so it needs no turn — and no claude — to have something
    // to report. `account` in the settings blob is what the thread ASKED for;
    // the column is where it is being served.
    let mk = |id: &str, account: &str, choice: &str| ThreadRow {
        id: id.to_string(),
        account: Some(account.to_string()),
        cwd: Some("/tmp".into()),
        model: Some("haiku".into()),
        settings_json: format!("{{\"cwd\":\"/tmp\",\"model\":\"haiku\",\"account\":{choice}}}"),
        last_seq: 0,
        next_message_id: 1,
        last_compaction_message_id: 0,
        needs_restate: false,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    for row in [
        // There is no status word to seed any more: a thread row records
        // configuration, and what each thread is DOING is established below by
        // the durable rows that actually mean it — a `running_turns` row for
        // `t-exec`, a `parked_turns` row for `t-park`, queue rows for
        // `t-queued`, a `retries` row for `t-retry`.
        mk("t-exec", "first", "\"auto\""),
        mk("t-park", "first", "\"auto\""),
        mk("t-queued", "second", "{\"named\":\"second\"}"),
        mk("t-retry", "second", "{\"named\":\"second\"}"),
    ] {
        m.store.upsert_thread(&row).unwrap();
    }
    m.store
        .upsert_running_turn(&RunningTurnRow {
            thread_id: "t-exec".into(),
            pid: 424_242,
            spawn_cookie: "c".into(),
            output_path: "/tmp/out".into(),
            byte_offset: 0,
            started_at: now.clone(),
            prompt: None,
            message_ids: vec![],
            attempt: 1,
        })
        .unwrap();
    m.store
        .upsert_parked_turn(&ParkedTurnRow {
            thread_id: "t-park".into(),
            prompt: "held".into(),
            message_ids: vec![1],
            attempt: 1,
            reason: "budget:demo session 100%".into(),
            budget_key: Some("demo".into()),
            parked_at: now.clone(),
        })
        .unwrap();
    m.store.enqueue("t-queued", 1, "hello", "turn_end", &serde_json::Value::Null, &[]).unwrap();
    m.store.enqueue("t-queued", 2, "again", "turn_end", &serde_json::Value::Null, &[]).unwrap();
    // Pin the fixture: a pending RETRY row makes `maybe_dispatch` return before
    // it can touch anything, so the background resume sweep cannot deliver the
    // queue or un-park the prompt between the two reads below.
    //
    // `t-retry` is in this list because a retry row is now the ONLY thing that
    // makes a thread count as retrying — the `retrying` counter is derived from
    // the `retries` table, not read off a status word a fixture could simply
    // assert into existence. So the arithmetic below is: `default` has one
    // (t-park), `second` has two (t-queued, t-retry).
    for id in ["t-park", "t-queued", "t-retry"] {
        m.store
            .upsert_retry(&RetryRow {
                thread_id: id.into(),
                prompt: "held".into(),
                message_ids: vec![1],
                attempt: 1,
                kind: "rate_limit".into(),
                reason: "fixture: holds the resume sweep off this thread".into(),
                // Unattributed on purpose: an unattributed floor never lifts
                // early, so this fixture still pins the sweep off the thread.
                account: None,
                created_at: String::new(),
            })
            .unwrap();
    }
    // A tracked limit window on one account...
    m.store
        .upsert_account_state(&threads::accounts::AccountState {
            name: "second".into(),
            config_dir: None,
            limit_until: Some("2099-01-01T00:00:00+00:00".into()),
            last_limit_prose: Some("resets in 2099".into()),
            ..Default::default()
        })
        .unwrap();
    // ...and one ALREADY-RECORDED gauge reading on the other. Both surfaces
    // must carry it, and neither may poll `/usage` to get it.
    m.budgets
        .observe(
            "first",
            threads::gauge::Axis::Session,
            42.0,
            None,
            "usage_poll",
        )
        .unwrap();

    let _ = addr;
    // `--config` points the CLI at this temp data dir, so `~/.threads` is never
    // resolved.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_threads"))
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "accounts",
            "status",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "threads accounts status --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ws: Value = serde_json::from_str(String::from_utf8(out.stdout).unwrap().trim_end()).unwrap();

    let by = |name: &str| -> Value {
        ws["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["account"] == name)
            .cloned()
            .expect("every registered account reports, busy or not")
    };

    let d = by("first");
    assert_eq!(d["registered"], json!(true));
    assert_eq!(d["threads"], json!(2));
    assert_eq!(d["executing"], json!(1));
    assert_eq!(d["executing_thread_ids"], json!(["t-exec"]));
    assert_eq!(d["parked"], json!(1));
    assert_eq!(d["parked_thread_ids"], json!(["t-park"]));
    assert_eq!(d["park_reasons"]["budget:demo session 100%"], json!(1));
    // t-park carries an armed retry (see the pin above) — DERIVED from the
    // `retries` table, which is the same place `thread_states` reads it.
    assert_eq!(d["retrying"], json!(1));
    assert_eq!(d["available"], json!(true));
    assert_eq!(d["gauge"]["session"]["observed_pct"], json!(42.0));
    assert_eq!(d["gauge"]["session"]["source"], json!("usage_poll"));
    assert_eq!(d["gauge"]["session"]["calibrated"], json!(false));

    let s = by("second");
    assert_eq!(s["threads"], json!(2));
    assert_eq!(s["queued_threads"], json!(1));
    assert_eq!(s["queued_messages"], json!(2));
    // Both of `second`'s threads have an armed retry row.
    assert_eq!(s["retrying"], json!(2));
    // Inside an observed limit window ⇒ not available, with the prose.
    assert_eq!(s["available"], json!(false));
    assert_eq!(s["last_limit_prose"], json!("resets in 2099"));
    assert_eq!(s["gauge"]["session"]["observed_pct"], Value::Null);

    assert_eq!(
        ws["totals"],
        json!({"executing": 1, "parked": 1, "queued_messages": 2, "threads": 4, "busy": true})
    );

    drop(tmp);
}

/// **`accounts health --json` is the credential-fleet surface, and its builder
/// is a pure function of the instant.**
///
/// This was `accounts_health_is_on_the_wire_and_byte_identical_to_the_cli`:
/// the `accounts/health` WS arm and the CLI pinned against each other. R2.S3c
/// (R4.S9, HOLDS R9) retired the WS arm — the board reads the `accounts` shape
/// — so the CLI is the one surface; its payload shape and the purity of
/// `accounts::health_json` (which made the old comparison honest) stay pinned.
///
/// Forced (historical, still applies to the purity half): with the builder
/// reading the clock internally the `a == b` assertion below goes red.
#[tokio::test]
async fn accounts_health_cli_payload_and_builder_purity() {
    let (_m, _addr, tmp, cfg) = start_two_account().await;
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_threads"))
        .args(["--config", cfg.to_str().unwrap(), "accounts", "health", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "threads accounts health --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ws: Value = serde_json::from_str(String::from_utf8(out.stdout).unwrap().trim_end()).unwrap();

    // The shape the interface is coding against.
    assert!(ws["slots"].is_array(), "slots must be an array: {ws}");
    assert!(ws["now_ms"].is_i64());
    assert_eq!(
        ws["warn_below_days"],
        json!(threads::accounts::HEALTH_WARN_BELOW_DAYS),
        "the warning horizon is part of the payload so a reader never hardcodes it"
    );
    assert!(
        ws["accounts_dir"]
            .as_str()
            .unwrap()
            .ends_with("accounts"),
        "accounts_dir names the tree the slots live in: {ws}"
    );

    // The instant is the ONLY nondeterminism. Same data dir, same `now_ms`,
    // twice — byte-identical, including every `days_left`. This is what makes
    // blanking those fields above safe rather than convenient: if the builder
    // read the clock internally (the shape `main.rs` had before this change,
    // and the shape that made a WS arm impossible to compare) this goes red.
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
    let dir = cfg.parent().unwrap();
    let a = threads::accounts::health_json(dir, &home, 1_700_000_000_000).unwrap();
    let b = threads::accounts::health_json(dir, &home, 1_700_000_000_000).unwrap();
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap(),
        "health_json must be a pure function of (data_dir, home, now_ms)"
    );
    let c2 = threads::accounts::health_json(dir, &home, 1_700_000_086_400_00).unwrap();
    assert_ne!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&c2).unwrap(),
        "…and it must actually USE now_ms — a builder that ignored it would make \
         the comparison above vacuous"
    );
    drop(tmp);
}

/// **`accounts.pool_share.set` is reachable over the wire, and what it writes is
/// what placement reads.**
///
/// `Store::set_pool_share` shipped with NO caller anywhere except a test, so
/// "lend the pool 60% of this account" was unreachable from the CLI, from the
/// WS and from every interface — the number could only be changed by editing
/// SQLite by hand. This is the round trip that proves the hole is closed at the
/// protocol level; `tests/accounts_pool_share_api.rs` proves placement obeys it.
///
/// **THE MUTATION.** Have the `.set` arm return the value it was SENT instead of
/// re-reading through `pool_share_rows`, and the out-of-range assertion below
/// still passes while `share` reports 1.5 — the exact failure the store's
/// refuse-don't-clamp rule exists to prevent. Have it skip the store write and
/// the read-back reports the old value. Both red here.
#[tokio::test]
async fn pool_share_is_settable_over_the_wire_and_out_of_range_is_refused() {
    let (_m, addr, tmp, _cfg) = start_two_account().await;
    let (mut c, _w) = Client::connect(addr).await;

    // Absent configuration reads as the whole account, and says so.
    let before = share_get(&_m, Some("first"));
    assert_eq!(before["shares"][0]["share"], json!(1.0));
    assert_eq!(before["shares"][0]["override"], Value::Null);
    assert_eq!(before["shares"][0]["source"], json!("unset"));

    let set = c
        .request_ok(
            &format!("accounts/{}/pool_share_set", "first"),
            json!({"share": 0.6, "note": "he keeps 40% to spend by hand"}),
        )
        .await;
    // The command acknowledges; the QUERY answers (docs/shapes.md 2.4).
    assert!(set.get("promise_id").is_some(), "acknowledged: {set}");
    let set = share_get(&_m, Some("first"));
    assert_eq!(set["shares"][0]["share"], json!(0.6));
    assert_eq!(set["shares"][0]["override"], json!(0.6));
    assert_eq!(set["shares"][0]["source"], json!("override"));
    assert_eq!(
        set["shares"][0]["note"],
        json!("he keeps 40% to spend by hand")
    );

    // It persisted, and a bare `.get` lists every pool member.
    let all = share_get(&_m, None);
    let rows = all["shares"].as_array().unwrap();
    assert!(rows.len() >= 2, "a bare get lists the pool: {all}");
    let first = rows.iter().find(|r| r["account"] == "first").unwrap();
    assert_eq!(first["share"], json!(0.6));

    // REFUSED, not clamped. Silently storing 1.0 for a 1.5 would tell the
    // operator his edit landed when it did not.
    //
    // 0.0 IS BACK ON THIS LIST (2026-09-20). It left on 2026-09-08 because a
    // true zero was the disable; the operator has separated the two questions
    // — *"I want disabled to be independent of share"* — so the state moved to
    // `accounts.disable` and this field is a quantity again. The refusal is
    // asserted to NAME that door, because the defect being fixed is a capability
    // nobody could find.
    for bad in [1.5, -0.2, 0.0] {
        let outcome = c
            .request(&format!("accounts/{}/pool_share_set", "first"), json!({"share": bad}))
            .await;
        assert_eq!(
            outcome["type"], "error",
            "share {bad} must be refused, not clamped: {outcome}"
        );
    }
    // And the refusals left the good value alone.
    let after = share_get(&_m, Some("first"));
    assert_eq!(after["shares"][0]["share"], json!(0.6));

    // ── THE DISABLE IS NOT REACHABLE FROM THIS ARM, AND THE REFUSAL SAYS SO ──
    //
    // This block used to send `share: 0` and assert a disable came back. That
    // equivalence is the defect: the capability shipped 2026-09-08 behind a
    // magic value in a number field and the operator never found it — his own
    // `pool_shares` row still read `1.0e-05` four days later. The error message
    // is asserted, not just the error, because the message is the only thing
    // standing where somebody types the old spelling.
    let refused = c
        .request(
            &format!("accounts/{}/pool_share_set", "first"),
            json!({"share": 0.0, "note": "no longer a paid account"}),
        )
        .await;
    assert_eq!(refused["type"], "error", "{refused}");
    let msg = refused["error"].as_str().unwrap_or_default();
    assert!(
        msg.contains(&format!("accounts/{}/disable", "first")),
        "the refusal must name the door the act moved to: {refused}"
    );

    // ── AND THE REAL DOOR LEAVES THE SHARE ALONE ──
    //
    // `first` is on 0.6 from the block above. It is STILL on 0.6 while it sits
    // out of the pool, which is the whole separation in three assertions.
    let off = c
        .request_ok(
            &format!("accounts/{}/disable", "first"),
            json!({"note": "no longer a paid account"}),
        )
        .await;
    // Same correction: the command acknowledges, the QUERY answers.
    assert!(off.get("promise_id").is_some(), "acknowledged: {off}");
    let off = share_get(&_m, Some("first"));
    assert_eq!(off["shares"][0]["disabled"], json!(true));
    assert_eq!(
        off["shares"][0]["share"],
        json!(0.6),
        "a disable is not a re-weighting: the share he lent is untouched"
    );
    assert_eq!(off["shares"][0]["disabled_note"], json!("no longer a paid account"));
    assert!(off["shares"][0]["disabled_at"].is_string(), "{off}");
    assert_eq!(
        off["shares"][0]["source"], json!("override"),
        "and the 0.6 override he set is still his: taking the account out of \
         the pool does not unsay what he lent it"
    );

    // It is STILL LISTED. An account that vanishes from the payload is how he
    // loses track of one; disabling takes an account out of placement, never out
    // of the interface.
    let all = share_get(&_m, None);
    let rows = all["shares"].as_array().unwrap();
    let first = rows.iter().find(|r| r["account"] == "first").unwrap();
    assert_eq!(first["disabled"], json!(true));
    assert_eq!(first["share"], json!(0.6));

    // And the pressure payload agrees, with `pressure: null` and a reason a
    // human can act on — never `0`, which is the BUSIEST possible reading. The
    // SHARE on that payload is the real 0.6 too.
    let pressure = pressure_get(&_m);
    let pr = pressure["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account"] == "first")
        .expect("a disabled account is still in the pressure payload");
    assert_eq!(pr["disabled"], json!(true));
    assert_eq!(pr["ranked"], json!(false));
    assert!(pr["pressure"].is_null());
    assert_eq!(pr["unranked_reason"], json!("disabled"));
    assert_eq!(pr["share"], json!(0.6));

    // REVERSIBLE, AND NOBODY SUPPLIES A NUMBER.
    let back = c.request_ok(&format!("accounts/{}/enable", "first"), json!({})).await;
    assert!(back.get("promise_id").is_some(), "acknowledged: {back}");
    let back = share_get(&_m, Some("first"));
    assert_eq!(back["shares"][0]["disabled"], json!(false));
    assert_eq!(back["shares"][0]["share"], json!(0.6));
    assert!(back["shares"][0]["disabled_at"].is_null());
    drop(tmp);
}

/// **`accounts.pressure` puts the ranker's own number on the wire, and an
/// UNRANKED account carries `null` — never `0`.**
///
/// The interface must not recompute pressure in Python; that would fork the
/// arithmetic into a third implementation that disagrees silently. This is the
/// protocol half of that; `tests/accounts_pressure.rs` is the arithmetic half.
///
/// **THE MUTATION.** Serialise the unranked case as `0.0` instead of `null` —
/// which is exactly what `unwrap_or_default()` on the `Option<f64>` would do,
/// and it is one keystroke — and the `Value::Null` assertion goes red. Zero
/// pressure means "exactly out of share with a full window left", the busiest
/// possible reading; unranked means "we have never measured this". Rendering
/// the second as the first inverts the account's meaning.
/// **P10 — THE CLI SHARE WRITE GOES THROUGH THE DAEMON, AND REFUSES WHEN IT
/// IS DOWN.**
///
/// `threads accounts pool-share <a> <s>` used to write `pool_shares` directly
/// with `Store::set_pool_share`. That is one writer too many: the daemon holds
/// the pool cell placement reads shares off, it is what rings
/// `info{accounts, pool_share}`, and it is now what emits `Fact::PoolShare`.
/// A share written around it decides nothing until something reloads and no
/// interface ever hears about it.
///
/// **Refuse, do not fall back.** The operator's ruling. The refusal has to name
/// the daemon and the URL, because "it did not work" with no address is a
/// message that sends somebody reading source.
///
/// **Forced red** by restoring the direct `budgets.store_ref().set_pool_share`
/// call: the command then exits 0 and writes the row.
#[tokio::test]
async fn the_cli_share_write_refuses_when_the_daemon_is_down() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // A port nothing is listening on. `port = 0` would be ambiguous, so this
    // picks a real one and closes it.
    let dead_port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    // **`chassis_port`, NOT `port` (R2.S4).** The CLI dials the chassis door
    // now; `port` named the retired legacy door and is ignored. A config that
    // still set only `port` left the CLI on the DEFAULT chassis port — the
    // operator's live daemon — and this test's write went there (R2.S4 run,
    // 2026-10-10: `pool_shares` row `aa-owned = 0.5` in the live store). The
    // env override is cleared for the same reason.
    let cfg = tmp.path().join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "data_dir = \"{}\"\nchassis_port = {dead_port}\n",
            tmp.path().display()
        ),
    )
    .unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_threads"))
        .env_remove("THREADS_CHASSIS_PORT")
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "accounts",
            "pool-share",
            "aa-owned",
            "0.5",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a share the daemon never saw must not be reported as written"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(&format!("{dead_port}")),
        "the refusal must name the URL it could not reach: {err}"
    );
    assert!(
        err.contains("daemon"),
        "the refusal must name the daemon: {err}"
    );
    assert!(
        err.contains("NOTHING WAS WRITTEN"),
        "the refusal must say that nothing changed: {err}"
    );

    // And it really did not write: the store has no row for it.
    let store = threads::Store::open(tmp.path().join("threads.db")).unwrap();
    assert_eq!(
        store.pool_share("aa-owned").unwrap(),
        None,
        "a refused write must leave the store untouched"
    );
    drop(tmp);
}

/// **`accounts.pressure` ANSWERS BYTE-IDENTICALLY ON BOTH SURFACES.**
///
/// Its two siblings (`accounts.status`, `accounts.health`) have had this test
/// since they landed; `accounts.pressure` never did. That gap became
/// load-bearing when pressure became PRODUCED rather than derived: P15 lifted
/// `pressure_rows`' hand-built `json!` into a typed `PressureTable` so the WS
/// arm, the CLI and `Fact::Pressure` are three projections of ONE value — and
/// the claim that the lift changed no bytes was a claim this suite could not
/// have caught breaking.
///
/// **The rule the lift has to obey, stated so it cannot be forgotten:** a typed
/// row reaches the wire through `serde_json::to_value` on BOTH surfaces.
/// `serde_json` is built without `preserve_order`, so a `Value` object sorts
/// its keys while a struct serialised directly keeps declaration order; one
/// surface doing `to_string(&typed)` while the other does `to_value` would
/// differ in byte order alone.
///
/// `now` is blanked on both sides before comparison — it is minted inside each
/// arm from its own `Utc::now()` and can never match — and the purity of the
/// rest is asserted separately, so the blanking cannot hide a real drift.
///
/// **Forced red** by serialising the table directly
/// (`serde_json::to_string(&self.pressure_table(..))` in `pressure_rows`):
/// declaration order versus sorted order, and this fails on the first row.
#[tokio::test]
async fn pressure_is_on_the_wire_and_byte_identical_to_the_cli() {
    let (m, _addr, tmp, cfg) = start_two_account().await;

    m.budgets
        .observe(
            "first",
            threads::gauge::Axis::WeeklyAll,
            40.0,
            Some("2099-01-01T00:00:00Z".to_string()),
            "usage_poll",
        )
        .unwrap();
    m.store.set_pool_share("second", 0.25, Some("a note")).unwrap();

    let ws = pressure_get(&m);

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_threads"))
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "accounts",
            "pressure",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "threads accounts pressure --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // **RAW STDOUT BYTES on one side.** Parsing the CLI into a `Value` and
    // re-serialising it would re-sort its keys and silently normalise away the
    // one thing this test exists to catch — a surface that serialises the
    // typed table DIRECTLY (declaration order) while the other goes through
    // `to_value` (sorted). The first cut of this test did exactly that and
    // stayed green under the mutation it was written for.
    //
    // `now` is minted inside each arm from its own `Utc::now()` and can never
    // match, so the CLI's value is GRAFTED onto the WS side rather than both
    // being blanked — one field substituted, every other byte compared.
    let raw = String::from_utf8(out.stdout).unwrap().trim_end().to_string();
    let cli: Value = serde_json::from_str(&raw).unwrap();
    let mut ws_cmp = ws.clone();
    assert!(
        cli["now"].is_string() && ws_cmp["now"].is_string(),
        "both surfaces carry the instant they were computed at: {cli} / {ws_cmp}"
    );
    ws_cmp["now"] = cli["now"].clone();
    assert_eq!(
        raw,
        serde_json::to_string(&ws_cmp).unwrap(),
        "the WS `accounts.pressure` arm and `accounts pressure --json` must not drift"
    );

    // …and the blanked field is the ONLY thing that differs, which is what
    // makes the blanking honest rather than a way to hide a drift: the table is
    // otherwise a pure function of (store, pool, now).
    let a = m.budgets.pressure_table(&m.account_pool(), chrono::Utc::now());
    let b = m.budgets.pressure_table(&m.account_pool(), chrono::Utc::now());
    let strip = |t: &threads::budgets::PressureTable| {
        let mut v = serde_json::to_value(t).unwrap();
        v["now"] = json!("");
        serde_json::to_string(&v).unwrap()
    };
    assert_eq!(
        strip(&a),
        strip(&b),
        "pressure_table must be a pure function of (store, pool, now)"
    );

    // The typed lift must not have changed the SHAPE either.
    assert_eq!(ws["axis"], json!("weekly_all"));
    assert!(ws["deadband"].is_f64());
    assert!(ws["ranked_count"].is_u64());
    let row = ws["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account"] == "first")
        .unwrap();
    assert_eq!(row["axis"], json!("weekly_all"), "axis rides on the row too: {row}");
    assert!(row["rank"].is_u64(), "rank is a bare number, not an object: {row}");
    drop(tmp);
}

#[tokio::test]
async fn pressure_is_on_the_wire_and_unranked_is_null_not_zero() {
    let (m, _addr, tmp, _cfg) = start_two_account().await;

    // `first` has a reading in a live window; `second` has never been measured.
    m.budgets
        .observe(
            "first",
            threads::gauge::Axis::WeeklyAll,
            40.0,
            Some("2099-01-01T00:00:00Z".to_string()),
            "usage_poll",
        )
        .unwrap();

    let p = pressure_get(&m);
    assert_eq!(p["axis"], json!("weekly_all"));
    let rows = p["accounts"].as_array().unwrap();

    let first = rows.iter().find(|r| r["account"] == "first").unwrap();
    assert_eq!(first["ranked"], json!(true));
    assert!(first["pressure"].as_f64().unwrap() > 0.0, "measured: {first}");
    assert_eq!(first["rank"], json!(1));
    assert_eq!(first["unranked_reason"], Value::Null);
    assert_eq!(first["share"], json!(1.0), "the denominator travels with it");
    assert_eq!(first["observed_pct"], json!(40.0));
    assert_eq!(first["resets_at"], json!("2099-01-01T00:00:00+00:00"));
    assert_eq!(first["reset_basis"], json!("observed"));

    let second = rows.iter().find(|r| r["account"] == "second").unwrap();
    assert_eq!(second["ranked"], json!(false));
    assert_eq!(
        second["pressure"],
        Value::Null,
        "UNRANKED IS NOT ZERO — it is the absence of evidence, and zero is the \
         busiest possible reading: {second}"
    );
    assert!(
        second["pressure"].as_f64().is_none(),
        "and it must not be renderable as a number at all"
    );
    assert_eq!(second["rank"], Value::Null, "unranked has no rank, not last place");
    assert_eq!(second["unranked_reason"], json!("no_reading"));
    assert_eq!(p["ranked_count"], json!(1));
    drop(tmp);
}

/// `threads.reconcile_transcript` over the wire — the exact protocol shape the
/// AUI recovery path calls (msg #1028 §3).
///
/// The client says "this thread's transcript advanced externally at `<path>`"
/// and the daemon merges it into the thread's CURRENT account store, newest
/// wins, before the next daemon turn resumes there.
#[tokio::test]
async fn reconcile_transcript_merges_an_external_transcript_newest_wins() {
    let (_m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;

    let p = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    // The transcript store of the thread's account. After CON-27 that account
    // is `aa-owned`, the one login threads owns here — so its store is its OWN
    // config dir under the tempdir (`<config_dir>/projects/<slug>/`), not the
    // machine default's `<home>/.claude/projects/`. That is the point of the
    // constraint made visible on disk: nothing threads does can write a
    // transcript into the operator's own login.
    let slug = format!("-tmp-ws-reconcile-{id}");
    let line = |n: usize| {
        (0..n)
            .map(|i| json!({"sessionId": id, "seq": i}).to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };

    // The daemon's copy: 3 events. The copy the AUI fallback advanced while the
    // daemon was down, in a store of its own: 8.
    let resident_dir = tmp.path().join("aa-owned/projects").join(&slug);
    std::fs::create_dir_all(&resident_dir).unwrap();
    let resident = resident_dir.join(format!("{id}.jsonl"));
    std::fs::write(&resident, line(3)).unwrap();

    let external_dir = tmp.path().join("fallback-store/projects").join(&slug);
    std::fs::create_dir_all(&external_dir).unwrap();
    let external = external_dir.join(format!("{id}.jsonl"));
    std::fs::write(&external, line(8)).unwrap();

    let out = c
        .request_ok(
            &format!("threads/{}/reconcile_transcript", id),
            json!({"path": external.to_string_lossy()}),
        )
        .await;
    // **AN ACKNOWLEDGEMENT, NOT THE OUTCOME** (rule 2.4, R2.S5). The positive
    // sentinel first — this IS a command reply — then every outcome field is
    // absent BY KEY.
    assert!(out["promise_id"].is_u64(), "a command reply carries its promise: {out}");
    assert_eq!(out["changed"], json!(true), "the transcript WAS ported: {out}");
    for gone in [
        "thread_id", "account", "source", "outcome", "reconciled", "destination",
        "destination_events", "source_events", "replaced_events",
    ] {
        assert!(
            out.get(gone).is_none(),
            "`{gone}` is the ledger event's payload, never a reply field: {out}"
        );
    }

    assert_eq!(
        std::fs::read_to_string(&resident).unwrap(),
        std::fs::read_to_string(&external).unwrap()
    );
    assert!(external.is_file(), "the external copy is never deleted");

    // It is on the thread's own ledger, so the move is answerable after the
    // fact from `threads.history` alone.
    let hist = c
        .request_ok(
            &format!("threads/{}/history", id),
            json!({"kinds": ["custom"], "limit": 50}),
        )
        .await;
    let events = hist["events"].as_array().unwrap();
    let rec = events
        .iter()
        .find(|e| e["event"]["event_type"] == "transcript_reconciled")
        .unwrap_or_else(|| panic!("the reconcile must be on the ledger: {events:?}"));
    // WHERE THE OUTCOME LIVES NOW: the ledger event's payload — the value
    // `ThreadCells::app["transcript_reconciled"]` holds.
    let data = &rec["event"]["data"];
    assert_eq!(data["thread_id"], id.as_str(), "{data}");
    assert_eq!(data["account"], "aa-owned");
    assert_eq!(data["outcome"], "ported");
    assert_eq!(data["reconciled"], true);
    assert_eq!(data["destination"], resident.to_string_lossy().as_ref());
    assert_eq!(data["destination_events"], 8);
    assert_eq!(data["source_events"], 8);
    assert_eq!(data["replaced_events"], 3);
    assert_eq!(data["source"], external.to_string_lossy().as_ref());

    // Idempotent, and never a rollback.
    let again = c
        .request_ok(
            &format!("threads/{}/reconcile_transcript", id),
            json!({"path": external.to_string_lossy()}),
        )
        .await;
    assert!(again["promise_id"].is_u64(), "{again}");
    assert_eq!(
        again["changed"],
        json!(false),
        "handing the same path back is `kept` — nothing moved: {again}"
    );
    assert!(again.get("outcome").is_none(), "{again}");

    // Refusals, not guesses.
    let bad = c
        .request(
            &format!("threads/{}/reconcile_transcript", id),
            json!({"path": tmp.path().join("nope.jsonl").to_string_lossy()}),
        )
        .await;
    assert_eq!(bad["type"], "error");
    assert!(bad["error"].as_str().unwrap().contains("nothing to reconcile"));

    // **A RECONCILE THAT NAMES NO THREAD IS NOT A MALFORMED REQUEST ANY
    // MORE — IT IS NOT A METHOD.** The subject rides the path, so the old
    // "missing required param: thread_id" cannot be reached: there is nothing
    // to leave out. The inventory answers instead, and it names what was sent.
    let bad = c
        .request(
            "threads/reconcile_transcript",
            json!({"path": external.to_string_lossy()}),
        )
        .await;
    assert_eq!(bad["type"], "error");
    assert!(
        bad["error"]
            .as_str()
            .unwrap()
            .contains("unknown method: threads/reconcile_transcript"),
        "{bad}"
    );
}

/// The daemon's request surface, pinned (F-3).
///
/// The surface was recorded as 22 methods and had quietly become 24 — the
/// `budgets.disable` / `budgets.enable` pair from DEC-33, plus `budgets.delete`
/// surviving as an explicit refusal rather than being removed. Twelve tests in
/// this file exercise individual methods; none of them noticed, because none of
/// them looked at the inventory.
///
/// So this reads the dispatch arms straight out of `src/server.rs` and asserts
/// they are EXACTLY `server::WS_METHODS`. Adding, renaming or removing a method
/// now fails here until the pinned list is edited on purpose.
#[test]
fn the_ws_method_inventory_is_pinned() {
    let src = include_str!("../../src/server.rs");
    // Only the dispatch matches — not `WS_METHODS` itself, which names the same
    // strings and would otherwise make this test tautological.
    //
    // ── ONE REGION AGAIN, AND THE CHASSIS ANSWERS THE REST (R2.S4) ─────────
    //
    // This scanned a SECOND region, `handle_connection_method`, because the
    // legacy door answered `shapes.subscribe` / `shapes.unsubscribe` itself,
    // before `dispatch`, and they had to be found somewhere. That function was
    // `server::drive`'s and went with the `:3651` listener. The one door left is
    // the chassis, which routes `CHASSIS_METHODS` FIRST and `producer.names()`
    // second (`shapewire/src/server.rs`), so a method pinned in `WS_METHODS`
    // that is also a chassis method is answered without an arm here. The law
    // below states that per method rather than by an exception list: every
    // pinned method has an arm in `handle` OR is answered by the chassis, and
    // every arm is pinned.
    //
    // It is anchored on the function's own SIGNATURE rather than on a generic
    // marker: a marker that also appears in prose makes this test find the
    // wrong region and pass for the wrong reason, which is exactly what
    // happened on the first cut of the shapes change.
    let region = |from: &str, to: &str| -> String {
        let start = src
            .find(from)
            // Skip the signature line itself so a doc comment above it cannot
            // contribute arms.
            .unwrap_or_else(|| panic!("{from} was renamed — re-point this test"));
        let body = &src[start + from.len()..];
        let end = body
            .find(to)
            .unwrap_or_else(|| panic!("the end marker {to:?} moved — re-point this test"));
        body[..end].to_string()
    };
    let regions = [
        // the dispatcher, ending at its unknown-method catch-all
        region("async fn handle(", "other => Err("),
    ];

    let mut arms = 0usize;
    let mut found: Vec<String> = Vec::new();
    for body in &regions {
        for line in body.lines() {
            let line = line.trim();
            let Some(head) = line.strip_suffix("=> {") else {
                continue;
            };
            let head = head.trim();
            if !head.starts_with('"') {
                continue;
            }
            arms += 1;
            for part in head.split('|') {
                let name = part.trim().trim_matches('"').to_string();
                assert!(
                    name.contains('/') && !name.contains(' '),
                    "unparsed dispatch arm: {line}"
                );
                found.push(name);
            }
        }
    }
    found.sort();

    let mut pinned: Vec<String> = server::WS_METHODS.iter().map(|s| s.to_string()).collect();
    pinned.sort();
    let mut deduped = pinned.clone();
    deduped.dedup();
    assert_eq!(deduped, pinned, "WS_METHODS lists a method twice");

    // ── NO EXCEPTION LIST, AND THAT IS THE FIX (2026-10-02) ─────────────────
    //
    // There was a `CHASSIS_ONLY: &[&str] = &["resource/{id}/read"]` carve-out
    // here, on the reasoning that *`WS_METHODS` is the UNION the daemon serves,
    // so it must list every chassis method*. That reasoning had already been
    // overturned: `chassis_api.rs::the_surface_is_the_chassis_plus_the_producer`
    // established that `CHASSIS_METHODS ⊆ WS_METHODS` is NOT a law and made
    // containment PER METHOD — a chassis method that moved off the producer
    // stays in both, one that was never the producer's is in `CHASSIS_METHODS`
    // alone. `durable/barrier` was kept out of `WS_METHODS` on exactly that
    // ground, and `resource/{id}/read` was put in on the ground that had just
    // been dropped.
    //
    // The consequence is what this test is for: `:3651` ADVERTISED a method it
    // has no arm for, so `route` matched it, `handle_connection_method` returned
    // `None`, `dispatch` fell through to `handle`'s catch-all and the consumer
    // got `unknown method: resource/{id}/read` — the inventory and the router
    // disagreeing about the daemon's own surface. The carve-out is what made
    // that invisible.
    //
    // So the law is now an EQUALITY with nothing subtracted: every method in
    // `WS_METHODS` has an arm on this door, and every arm is in `WS_METHODS`.
    // A chassis method `threads` does not serve belongs in `CHASSIS_METHODS`
    // alone, where the chassis door routes it (`shapewire/src/server.rs`:
    // `route(CHASSIS_METHODS, m).or_else(|| route(producer.names(), m))`, so
    // `:3653` serves it without `threads` listing it at all).
    // Every arm is pinned …
    for f in &found {
        assert!(
            pinned.contains(f),
            "src/server.rs dispatches `{f}` and server::WS_METHODS does not pin it"
        );
    }
    // … and every pinned method is ANSWERED on the one door: by an arm, or by
    // the chassis ahead of the producer.
    let mut answered: Vec<String> = found.clone();
    answered.extend(
        pinned
            .iter()
            .filter(|m| shapewire::server::CHASSIS_METHODS.contains(&m.as_str()))
            .cloned(),
    );
    answered.sort();
    answered.dedup();
    assert_eq!(
        answered, pinned,
        "the WS method inventory drifted: src/server.rs dispatches {found:?} and \
         the chassis answers its own CHASSIS_METHODS, but server::WS_METHODS pins \
         {pinned:?}. A method in the inventory that nothing answers is a method \
         this door ADVERTISES AND REFUSES; if it is the chassis's, it belongs in \
         CHASSIS_METHODS alone."
    );
    // 24 at the audit (F-3), 25 since `threads.reconcile_transcript` landed
    // with transcript porting (msg #1028), 27 since `threads.status` and
    // `threads.seen` landed with the thread lock (msg #1213: "status isn't
    // making it whatsoever here to AUI"), 28 since `threads.query` landed with
    // the citation surface (msg #1438: "I do not want you writing prose…
    // it's tripling the amount of information required").
    // …and 30 since `threads.queue_failed` / `threads.queue_reselect` landed
    // with retry closure (2026-08-25). A ladder that runs out now preserves the
    // delivery on `queue.state='failed'` instead of leaving it indistinguishable
    // from a success; these two calls are what make that preservation reachable,
    // and without them "the prompt is kept" would be true and useless.
    // …and 31 since `accounts.login_restored` landed (2026-08-27). It is the
    // hook the login-issuing flow calls the instant it sees a login succeed:
    // the account's login bar is lifted on every label at once and it is
    // placeable again immediately, instead of waiting out the recheck window.
    // The pin is doing exactly its job here — this number moved because a
    // method was added deliberately, and the test made that visible.
    // …and 35 since the ACCOUNTS-INTERFACE surface landed (2026-08-27), four
    // methods in one pass, each closing a way the interface was getting a fact
    // from somewhere other than this API:
    //
    // * `accounts.health` — was CLI-only, so the interface spawned a process
    //   and waited up to thirty seconds per board build.
    // * `accounts.pressure` — was PRIVATE to the ranker, so a client that
    //   wanted it had to recompute it and fork the arithmetic.
    // * `accounts.pool_share.get` / `.set` — `Store::set_pool_share` had no
    //   caller anywhere except a test, so a per-account usage limit was
    //   unreachable from EVERY surface, not just from the interface.
    //
    // The pin is doing its job again: this number moved because four methods
    // were added deliberately, and the test made that visible.
    // …and 43 once the LOGIN FLOW landed — `accounts.login.begin` /
    // `.complete` / `.cancel`. Logging an account in was the one account
    // operation the interface could not perform: `claude auth login` prints a
    // URL and then blocks reading a pasted `code#state` from stdin, so the app
    // had to suspend itself and hand the operator a bare terminal. It is two
    // calls now, driven over PIPES — measured, not assumed; a pty wraps the URL
    // in an OSC-8 hyperlink and buys nothing.
    //
    // …and 40 with the second half of the same pass: the operations the
    // interface was performing by SHELLING OUT, and the two facts it was
    // getting from outside the API altogether.
    //
    // * `gauge.history` — the history view and every sparkline were drawn by
    //   opening `threads.db` READ-ONLY from the client, against the daemon's
    //   own WAL, with the schema and the poll cadence duplicated client-side.
    // * `daemon.status` — the detail behind liveness. DOWN is still detected
    //   from the socket and `GET /health`, because a dead daemon answers
    //   nothing; this is what makes the UP page honest.
    // * `daemon.restart` — his ruling for the down case was "one page that says
    //   the daemon is down with a button to restart the daemon". This is the
    //   thing the button calls when the daemon is reachable. It QUEUES on the
    //   update protocol and fires at the first quiet moment; it does not kill a
    //   process and cannot interrupt a turn.
    // * `accounts.rename` / `accounts.delete` — were `subprocess` calls to a
    //   bash tool whose prose the client grepped for outcomes.
    // …and 44 with `accounts.rotate`, the last operation the interface was
    // performing by shelling out and reading a log file.
    // …and 45 with `daemon.image`: WHICH BINARY IS ACTUALLY RUNNING.
    //
    // > *"You need a way to be able to verify which binary is running. If you
    // > could just add an endpoint in the API surface with metadata, just
    // > anything we would want to know. Like whether it's signed even."*
    //
    // It reports the running image's code signature (cdhash, identifier,
    // authority, signed-or-ad-hoc), the text inode it is executing against the
    // inode currently at the launch path, its build, and whether the on-disk
    // image would be absorbed — every field describing THE IMAGE THE PROCESS IS
    // EXECUTING rather than a path, because a path can be replaced underneath a
    // running process and here it routinely is.
    //
    // It arrived together with the REMOVAL of `daemon.status`'s `binary` field,
    // which was a path presented as the running binary and which misled three
    // separate investigations in one week. Two fields answering the same
    // question differently is how the next one goes wrong, so there is now one.
    // …and 47 with `accounts.disable` and `accounts.enable` (msg #531).
    //
    // > *"Create the machinery on both sides of the contract to disable an
    // > account so that it doesn't get considered a placement, and then the text
    // > user interface just says, like, disabled. I don't want it to be deleted,
    // > though. I still want to see it there as one of my accounts."*
    //
    // TWO NAMES, ONE MECHANISM. They write the same single column
    // (`pool_shares.share`) through the same setter that
    // `accounts.pool_share.set` uses, and a zero sent to that method produces a
    // state indistinguishable from `accounts.disable`'s, because it IS that
    // state. What they add is a NAME for the act and the restore share that a
    // named act can carry — `Budgets::restore_share`, so re-enabling is one
    // keypress rather than a keypress plus a number he has to remember.
    //
    // They exist because the capability without the name did not reach him: the
    // 0%-share disable shipped 2026-09-08, was live in the running daemon, and
    // his own `pool_shares` row for `alice-example-com` still read `1.0e-05` on
    // 2026-09-11 — the 0.001% he had tried BEFORE it existed.
    // `resource/{id}/read` (2026-10-02, the inline resource door) was added here
    // and REMOVED AGAIN the same day, so the count stayed at 50. It is a CHASSIS
    // method with no `src/server.rs` arm behind it and never will have one, and
    // listing it made this door advertise a method it then refused as
    // `unknown method`. It lives in `CHASSIS_METHODS` alone, where the chassis
    // door routes it; `chassis_api.rs::
    // a_chassis_method_the_producer_does_not_serve_is_claimed_by_neither_inventory`
    // is the live proof that `:3653` answers it and `:3651` says it does not
    // have it.
    // …and 48 once the QUEUE READS were REMOVED, which is the first time this
    // number has gone DOWN because a shape absorbed a method rather than
    // because a method was a mistake.
    //
    // `threads/{id}/queue` and `threads/{id}/queue_failed` projected exactly the
    // same four fields out of `store::QueueRow` and differed in ONE `WHERE
    // state=`, so the discriminator their NAMES encoded is now a declared field
    // with a closed vocabulary — `shapes::threads::QueueEntry` and
    // `QUEUE_STATE_WORDS`. Both were calls a face made about a thread it was
    // ALREADY subscribed to, so the answer moved when the queue moved and
    // asking for it on a keypress was always a guess about when it had changed.
    // A face now reads the `queue` cell and FILTERS, and cannot show a waiting
    // panel and a failed panel disagreeing about which rows exist.
    //
    // The two queue WRITES stayed (`queue_cancel`, `queue_reselect`): a cell
    // publishes a fact and "move these rows back to `waiting`" is an act with
    // an effect, so rule 2.4 keeps it a method. The pin is doing its job in the
    // subtractive direction here — this number moved because a surface SHRANK
    // deliberately, and the test made that visible.
    // …and 45 at R2.S3a, when three READS that nothing called were RETIRED:
    // `budgets/list`, `ledger/query` and `threads/{thread_id}/messages`. The
    // operator: *"We should not have new paths to request data… We should
    // remove the ability to request data."* The census that found no caller —
    // the seam, the board, both faces and the harness tools — is in
    // `docs/consumer-migration-ledger.md`. Two more uncalled-by-faces reads,
    // `budgets/{key}/get` and `threads/{thread_id}/budget_get`, were NOT
    // retired with them: `src/bin/threads-stress.rs` still calls both, and a
    // producer does not remove a read a consumer still makes (rule 2.6).
    // …and 57 at R4.S1, when the `agents` FAMILY landed: twelve method paths
    // for `docs/managed-subagents.md` §2's eight commands and four reads
    // (`agents/spawn`, `spawn_batch`, `{agent}/message`, `{network}/broadcast`,
    // `{agent}/interrupt`, `watch`, `network_create`, `{network}/network_join`,
    // `list`, `{agent}/result`, `gather`, `{agent}/log`).
    //
    // **THEY ARE PINNED HERE BECAUSE THEY ARE THIS PRODUCER'S, NOT THE
    // CHASSIS'S** — the distinction `durable/barrier` and `resource/{id}/read`
    // are kept OUT of this list for. The MCP door (`shapewire::mcp`) is a
    // second CARRIER over the same arms, not a second producer: each tool is a
    // façade that binds its subject into the path below
    // (`threads::agents`, and `docs/shape-surface.md` §5.5). So the `/ws` door
    // answers all twelve by path, which is what makes this pin the right place
    // for them.
    //
    // Eleven of the twelve are refused `not_available_yet` on this tree because
    // their resolving fact is the managed runtime's (R4.S2); a refusal IS an
    // answer, and the arm exists. `agents/{agent}/log` is live. The number
    // going UP by a whole family at once is the pin doing its job: the surface
    // grew deliberately and the test made it say so.
    // …and 62 at R4.S3, when the network acts of `docs/agent-networks.md`
    // §4.2 landed LIVE: `agents/{network}/network_admit`, `network_leave`,
    // `network_remove`, `network_configure`, and the `agents/reach` read.
    // `message`, `broadcast`, `network_create` and `network_join` went live in
    // place (same paths), and `agents/{agent}/log` is gated by READS.
    // …and 52 at R4.S9 (R2.S3c, HOLDS R9): ten reads whose consumers had all
    // moved onto shapes and been relaunched (r23) were retired —
    // `accounts/{health,list,pool_share_get,pressure,status}`, `daemon/status`,
    // `gauge/history`, `threads/status`, `threads/{thread_id}/get`,
    // `threads/{thread_id}/query`. Six reads stay because `threads-stress` or
    // the `threads image` CLI still send them (`threads/list`,
    // `threads/{thread_id}/history`, `threads/{thread_id}/budget_get`,
    // `budgets/{key}/get`, `gauge/status`, `daemon/image`).
    // …and 53 at PL-2b (r33): `threads/{thread_id}/source_census`, a plain
    // read — transcript + `turns/` capture presence per thread, for PL-2's
    // census through the door.
    // …and 54 at SH-A2 (r38): `threads/{thread_id}/attach`, the interactive
    // TUI lease (`docs/SHIMS.md` §A).
    // …and 55 at SH-A4 (r40): `threads/{thread_id}/detach`, the lease's exit.
    // …and 56 at R12.10 (r41): `accounts/{account}/recheck`, the re-check a
    // verified `claude-accounts` login asks for (`threads accounts recheck`).
    // …and 57 at R10.M3 (r42): `agents/{agent}/policy`, a link's notify
    // policy changed without sending (`MESSAGING-ORCHESTRATION.md` §8).
    assert_eq!(pinned.len(), 57, "the surface is 57 methods");
    // 47 arms until R2.S4, when `handle_connection_method`'s two
    // (`shapes/{shape}/subscribe`, `shapes/{shape}/unsubscribe`) went with the
    // legacy door: the chassis answers both, ahead of the producer, so they are
    // pinned and answered without an arm here. `shapes/list` keeps its arm.
    //
    // …and 44 at R4.S1: twelve `agents` names on TWO arms, deliberately. Eleven
    // share one `|` arm because they share one answer (the typed refusal naming
    // the method path), and this scan reads arms a LINE at a time while
    // splitting names on `|` — so one line carrying eleven names is one arm and
    // eleven pinned names, which is exactly the shape of the thing. The twelfth,
    // `agents/{agent}/log`, has its own arm because it has its own answer.
    // …and 47 at R4.S2: `agents/spawn`, `agents/list` and `agents/{agent}/result`
    // left the one shared refusal arm for arms of their own, because they now
    // answer (the managed runtime, `crate::managed`). Names unchanged at 57.
    // …and 56 at R4.S3 landed over R4.S2: four more names left the shared
    // refusal arm (`message`, `broadcast`, `network_create`, `network_join` are
    // live now) and they and the five new paths each have their own arm in
    // `crate::networks` — nine arms added (47 + 9), the refusal arm kept with
    // its four remaining names (`spawn_batch`, `interrupt`, `watch`, `gather`).
    // …and 46 at R4.S9: the ten retired reads were ten arms.
    // …and 47 at R4.S8: `agents/gather` left the refusal arm for its own
    // (`crate::managed::gather`, the bounded block); names unchanged at 52.
    // …and 48 at PL-2b: `source_census` has its own arm; names 53.
    // …and 49 at SH-A2: `attach` has its own arm; names 54.
    // …and 50 at SH-A4: `detach` has its own arm; names 55.
    // …and 51 at R12.10: `recheck` has its own arm; names 56.
    // …and 52 at R10.M3: `policy` has its own arm; names 57.
    assert_eq!(arms, 52, "57 producer method names: 52 match arms, 2 chassis-answered");

    // The citation query (`threads/{thread_id}/query`) and the status query
    // (`threads/status`) LEFT deliberately at R4.S9 — the plugin tool reads
    // message pages through the resource door, and every face reads status off
    // the `threads` shape. Their absence is pinned, so a return is deliberate.
    for name in ["threads/{thread_id}/query", "threads/status"] {
        assert!(!pinned.iter().any(|m| m == name), "{name} came back");
    }
    // `seen` is a write, and stays.
    assert!(pinned.iter().any(|m| m == "threads/{thread_id}/seen"));

    // The three the drift was made of, named so a silent removal reads as a
    // deliberate one.
    for name in [
        "budgets/{key}/disable",
        "budgets/{key}/enable",
        "budgets/{key}/delete",
    ] {
        assert!(pinned.iter().any(|m| m == name), "{name} left the surface");
    }
    // And `budgets.delete` is a REFUSAL, not an implementation (DEC-33).
    let delete_arm = regions[0]
        .split("\"budgets/{key}/delete\" => {")
        .nth(1)
        .and_then(|rest| rest.split("\"threads/{thread_id}/budget_assign\"").next())
        .expect("budgets.delete arm");
    assert!(
        delete_arm.contains("Err(Error::Config"),
        "budgets/{{key}}/delete must stay an explicit refusal"
    );
}

// ── the status surface (operator msg #1213, second half) ─────────────────

/// "And status isn't making it whatsoever here to AUI."
///
/// The wire contract AUI stands on, end to end over a real socket: a `status`
/// PUSH frame on every transition, and a `threads.status` QUERY for a client
/// that just connected and cannot wait for an edge. Both carry the same
/// vocabulary — `active` / `queued` / `idle` — plus the queued count, which is
/// what tells the operator a message of his is waiting behind a working
/// thread.
#[tokio::test]
async fn status_reaches_a_client_by_both_push_and_query() {
    let (_m, addr, tmp) = start_with(None).await;
    let (mut c, _) = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    let created = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let tid = created["thread_id"].as_str().unwrap().to_string();

    // Idle before anything is said.
    let s = _m.thread_status(&tid).unwrap();
    assert_eq!(s["state"], "idle");
    assert_eq!(s["executing"], false);

    // A slow turn, then a second send that must QUEUE behind the lock.
    c.request_ok(
        &format!("threads/{}/send", tid),
        json!({"text": "slowturn first"}),
    )
    .await;
    let active = c
        .wait_push(|f| f["type"] == "status" && f["state"] == "active")
        .await;
    assert_eq!(active["thread_id"], tid.as_str());
    assert_eq!(active["executing"], true);

    c.request_ok(
        &format!("threads/{}/send", tid),
        json!({"text": "slowturn second"}),
    )
    .await;
    let queued = c
        .wait_push(|f| f["type"] == "status" && f["queued"].as_u64().unwrap_or(0) > 0)
        .await;
    assert_eq!(
        queued["state"], "active",
        "still working, with a message waiting behind it — both facts on one frame"
    );

    // The bulk form: one round trip paints every row after a reconnect.
    let all = statuses_get(&_m);
    let rows = all["statuses"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["thread_id"] == tid.as_str()));

    // Back to idle when the work is done, and the unread cursor is advanceable.
    let _ = c
        .wait_push(|f| f["type"] == "status" && f["state"] == "idle")
        .await;
    let before = _m.thread_status(&tid).unwrap();
    assert!(before["unread"].as_u64().unwrap() > 0);
    // **AN ACKNOWLEDGEMENT, NOT THE STATUS** (rule 2.4). It answered the whole
    // fresh status *"so a client never needs a second call"*; `unread` and
    // `seen_seq` are cells the `threads` shape folds from the fact this call
    // emits, so the second call is the one the shape removes. The QUERY still
    // answers it, and that is what is asserted.
    let ack = c.request_ok(&format!("threads/{}/seen", tid), json!({})).await;
    assert!(ack["promise_id"].is_u64(), "{ack}");
    assert!(ack.get("unread").is_none(), "{ack}");
    let after = _m.thread_status(&tid).unwrap();
    assert_eq!(
        after["unread"], 0,
        "`threads/{{thread_id}}/seen` with no seq means 'I have heard everything'"
    );
}

/// A second `threads.send` on a BUSY thread is accepted and QUEUED — it is
/// never refused, never dropped, and never runs a second execution.
///
/// This is the client-facing shape of the lock, and it is what makes AUI's
/// patient wait correct: `accepted: true` now means "the daemon owns this
/// turn", so a client that falls back on a missing `turn_started` is
/// manufacturing the duplicate, not recovering from one.
#[tokio::test]
async fn a_send_into_a_locked_thread_is_accepted_and_queued_never_duplicated() {
    let (manager, addr, tmp) = start_with(None).await;
    let (mut c, _) = Client::connect(addr).await;
    c.subscribe_all(&manager).await;

    let created = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let tid = created["thread_id"].as_str().unwrap().to_string();

    c.request_ok(
        &format!("threads/{}/send", tid),
        json!({"text": "slowturn one"}),
    )
    .await;
    let first = c.wait_event("turn_started").await;
    let first_ids = first["event"]["message_ids"].clone();

    // Three more sends land while the thread is locked. All accepted.
    for i in 0..3 {
        let ack = c
            .request_ok(
                &format!("threads/{}/send", tid),
                json!({"text": format!("slowturn queued {i}")}),
            )
            .await;
        assert_eq!(ack["accepted"], true);
    }
    let waiting = queue_rows(addr, &tid, "waiting").await;
    assert_eq!(
        waiting.len(),
        3,
        "every send is HELD, in order — never merged away and never dropped"
    );

    // The first turn ends before any second turn begins.
    c.wait_event("turn_ended").await;
    let second = c.wait_event("turn_started").await;
    assert_ne!(
        second["event"]["message_ids"], first_ids,
        "the second turn must be the QUEUED work, not a re-run of the first"
    );

    // And at no point were two executions live.
    assert!(
        !manager.is_locked(&tid) || manager.thread_status(&tid).unwrap()["executing"] == true,
        "status and the lock must agree"
    );
}

// ── the fan-out gap: RETIRED WITH THE FIREHOSE (R2.S4) ──────────────────────
//
// Five tests stood here and were DELETED, not re-cut, because what they pinned
// no longer exists anywhere in the producer: `a_dropped_range_tells_the_client_
// where_to_resume`, `a_lag_with_no_delivered_events_is_still_reported` and
// `the_lag_frame_serializes_as_a_tagged_lagged_frame` asserted the CONTENT of
// `protocol::lag_frame` / `ServerMsg::Lagged`; `the_lagged_arm_is_never_empty_
// again` and `the_replay_cursor_advances_only_behind_a_delivered_event` were
// source scans of `server::drive`'s firehose arm. The frame, its cursors and
// that arm are `docs/consumer-seam.md` §6 M7's producer half and went with the
// `:3651` listener. The gap they guarded — a consumer silently missing a range
// — is the SHAPE channel's now, and its repair (re-export every held key on
// overflow) is pinned by `lag_repair.rs` against the chassis.

/// **`accounts.disable` / `accounts.enable` — THE ACT, NAMED, OVER THE WIRE.**
///
/// The operator, msg #531: *"create the machinery on both sides of the contract
/// to disable an account so that it doesn't get considered a placement, and then
/// the text user interface just says, like, disabled. I don't want it to be
/// deleted, though. I still want to see it there as one of my accounts, because
/// if I turn it back on, I'm going to keep using the same login."*
///
/// # These two methods are NOT a second mechanism
///
/// `pool_share_is_settable_over_the_wire_and_out_of_range_is_refused` above
/// disables the same account by sending a literal `0` to
/// `accounts.pool_share.set`, and asserts the same state on the same fields.
/// **Both tests are kept, deliberately**, because the thing worth pinning is
/// that the two routes are INDISTINGUISHABLE afterwards: one column, one
/// predicate, one wire fact. If a future change gives `accounts.disable` a state
/// of its own, that test and this one will disagree about an account and one of
/// them will go red.
///
/// What these methods add is a NAME for the act. The cost of not having one is
/// not hypothetical: the
/// 0%-share disable shipped 2026-09-08 and the operator's own `pool_shares` row
/// for `alice-example-com` still read `1.0e-05` on 2026-09-11 — the 0.001% he
/// tried BEFORE the feature existed. The capability was live and he never
/// reached it.
///
/// **AND SINCE 2026-09-20 THEY ARE THE ONLY DOOR, over state of their own.**
/// *"I want disabled to be independent of share. Whether to include it in the
/// pool supersedes how much should be made available to the pool."* So the two
/// facts round-trip separately and this test asserts both on every leg: a 0.6
/// share is 0.6 before the disable, 0.6 while disabled, and 0.6 after the enable
/// — with nobody supplying the number at any point.
///
/// **THE MUTATION.** Make `accounts.disable` also write a share (any share, the
/// old `set_pool_share(.., 0.0, ..)` included) and every `share` assertion below
/// the disable goes red — which is the whole defect in one line: an act the
/// operator reads as "take this out" silently re-weighting his pool.
#[tokio::test]
async fn an_account_is_disabled_and_enabled_by_name_and_the_share_never_moves() {
    let (_m, addr, tmp, _cfg) = start_two_account().await;
    let (mut c, _w) = Client::connect(addr).await;

    // He lent the pool 60% of this account.
    c.request_ok(
        &format!("accounts/{}/pool_share_set", "first"),
        json!({"share": 0.6}),
    )
    .await;

    // ── DISABLE ──
    let off = c
        .request_ok(
            &format!("accounts/{}/disable", "first"),
            json!({"note": "not a paid account any more"}),
        )
        .await;
    // **THE REPLY IS AN ACKNOWLEDGEMENT** (docs/shapes.md 2.4) — a promise id
    // and nothing else. The row it used to carry was a second source for what
    // `accounts.pool_share.get` already answers, so the assertions below read
    // it from there. They are unchanged in substance; only their source moved.
    assert!(off.get("promise_id").is_some(), "acknowledged: {off}");
    assert!(off.get("shares").is_none(), "and carries no fact: {off}");
    let state = share_get(&_m, Some("first"));
    let row = &state["shares"][0];
    assert_eq!(row["disabled"], json!(true));
    assert_eq!(
        row["share"],
        json!(0.6),
        "THE HEADLINE. The state is its own row now; the share he lent is not \
         part of the transaction and is reported unchanged beside it"
    );
    assert_eq!(row["override"], json!(0.6), "and it is still HIS number");
    assert_eq!(
        row["source"],
        json!("override"),
        "a disable is the most deliberate thing he can say about an account"
    );
    // THE TWO NOTES ARE TWO NOTES. `note` explains the SHARE, `disabled_note`
    // explains the DISABLE; merging them would make "not a paid account any
    // more" read as an explanation of a 0.6 he set for other reasons.
    assert_eq!(row["disabled_note"], json!("not a paid account any more"));
    assert!(row["note"].is_null(), "the share carried no note: {row}");
    assert!(row["disabled_at"].is_string(), "{row}");
    // RETIRED WITH THE MEMO. There is no share to reconstruct, so there is no
    // second field answering the question `share` already answers.
    assert!(row["restore_to"].is_null(), "{row}");
    assert!(row["prior"].is_null(), "{row}");

    // STILL LISTED, and out of placement.
    let pressure = pressure_get(&_m);
    let pr = pressure["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account"] == "first")
        .expect("a disabled account is STILL on the pressure payload");
    assert_eq!(pr["disabled"], json!(true));
    assert_eq!(pr["ranked"], json!(false));
    assert!(pr["pressure"].is_null());
    assert_eq!(pr["unranked_reason"], json!("disabled"));
    assert_eq!(pr["share"], json!(0.6), "the quantity survives on this payload too");

    // DISABLING TWICE IS IDEMPOTENT AND DOES NOT RE-DATE. The old failure this
    // replaces was a second disable eating the `prior_share` memo; the same
    // class of mistake here would be a second press rewriting `disabled_at`,
    // which is the only record of WHEN he took the account out.
    let at = row["disabled_at"].clone();
    let again = c.request_ok(&format!("accounts/{}/disable", "first"), json!({})).await;
    assert!(again.get("promise_id").is_some(), "acknowledged: {again}");
    let again = share_get(&_m, Some("first"));
    assert_eq!(again["shares"][0]["disabled_at"], at);
    assert_eq!(
        again["shares"][0]["disabled_note"],
        json!("not a paid account any more"),
        "a second disable with nothing to say must not erase what he said"
    );
    assert_eq!(again["shares"][0]["share"], json!(0.6));

    // ── ENABLE, with no number supplied by anyone ──
    let on = c.request_ok(&format!("accounts/{}/enable", "first"), json!({})).await;
    assert!(on.get("promise_id").is_some(), "acknowledged: {on}");
    let on = share_get(&_m, Some("first"));
    let row = &on["shares"][0];
    assert_eq!(row["disabled"], json!(false));
    assert_eq!(
        row["share"],
        json!(0.6),
        "it comes back at the share it never stopped having"
    );
    assert!(row["disabled_at"].is_null(), "the state is DELETED, not flagged: {row}");
    assert!(row["disabled_note"].is_null(), "{row}");

    // ENABLE AT ZERO IS A CONTRADICTION and is refused by the share bound,
    // which now names `accounts.disable` rather than describing a zero.
    let bad = c
        .request(&format!("accounts/{}/enable", "first"), json!({"share": 0.0}))
        .await;
    assert_eq!(bad["type"], "error", "enable at 0 must be refused: {bad}");
    let still = share_get(&_m, Some("first"));
    assert_eq!(still["shares"][0]["share"], json!(0.6), "the refusal changed nothing");
    assert_eq!(still["shares"][0]["disabled"], json!(false));

    // ENABLE WITH A SHARE is two explicit acts in one call, and it is the only
    // way the share moves on this path.
    c.request_ok(&format!("accounts/{}/disable", "first"), json!({})).await;
    let at_new = c
        .request_ok(&format!("accounts/{}/enable", "first"), json!({"share": 0.25}))
        .await;
    assert!(at_new.get("promise_id").is_some(), "acknowledged: {at_new}");
    let at_new = share_get(&_m, Some("first"));
    assert_eq!(at_new["shares"][0]["disabled"], json!(false));
    assert_eq!(at_new["shares"][0]["share"], json!(0.25));

    // An account with NOTHING ever said about its share round-trips at the whole
    // account, and disabling it MINTS NO OVERRIDE — `source` stays `unset`, so
    // the interface never shows him a number he did not type.
    let ack = c
        .request_ok(&format!("accounts/{}/disable", "second"), json!({}))
        .await;
    assert!(ack.get("promise_id").is_some(), "acknowledged: {ack}");
    let second = share_get(&_m, Some("second"));
    assert_eq!(second["shares"][0]["disabled"], json!(true));
    assert_eq!(second["shares"][0]["share"], json!(1.0));
    assert_eq!(second["shares"][0]["source"], json!("unset"), "{second}");
    assert!(second["shares"][0]["override"].is_null());
    c.request_ok(&format!("accounts/{}/enable", "second"), json!({})).await;
    let back = share_get(&_m, Some("second"));
    assert_eq!(back["shares"][0]["share"], json!(1.0));
    assert_eq!(back["shares"][0]["disabled"], json!(false));
    assert_eq!(back["shares"][0]["source"], json!("unset"));

    drop(tmp);
}

// ── R2.S4: THE DAEMON BINDS NO LEGACY PORT ──────────────────────────────────

/// **A REAL `run_daemon` SERVES THE CHASSIS DOOR AND BINDS NOTHING ON THE PORT
/// THE LEGACY DOOR WOULD HAVE TAKEN.**
///
/// The daemon is started the way `main` starts it: a `config.toml` on disk,
/// read by `Config::load`, handed to `threads::run_daemon`. The file names TWO
/// free ephemeral ports — `port`, the key that bound the legacy `:3651` door
/// until R2.S4, and `chassis_port`. Both were measured free by binding and
/// releasing them, so neither is the operator's live `:3651`/`:3653` (which are
/// held by the running daemon and would make a "refused" claim meaningless).
///
/// The claim is a connect to the `port` port being REFUSED (ECONNREFUSED)
/// while, in the same test and on the same daemon, the `chassis_port` door
/// answers `shapes/list` — the positive sentinel that the daemon under test is
/// up, so the refusal is the absence of a listener and not the absence of a
/// daemon.
///
/// **Forced red** (R2.S4) by restoring a legacy bind in `run_daemon` (the
/// `port` field back on `Config` and a `server::bind` of it held for the
/// daemon's life): `nothing listens on the legacy port, but 127.0.0.1:62123
/// accepted a connection`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_binds_no_legacy_port_and_serves_the_chassis_door() {
    set_env();
    // THE CONSUMER LEDGER IS WIRED BY `run_daemon`: point it away from the
    // operator's real `~/.cauldron`.
    std::env::set_var(
        "CAULDRON_HOME",
        std::env::temp_dir().join("threads-tests-cauldron"),
    );
    // An inherited `THREADS_CHASSIS_PORT` would override the file and could
    // name the operator's live port.
    std::env::remove_var("THREADS_CHASSIS_PORT");

    let free = || {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    let legacy_port = free();
    let chassis_port = free();
    assert_ne!(legacy_port, chassis_port);
    for p in [legacy_port, chassis_port] {
        assert!(
            ![3651u16, 3653].contains(&p),
            "an ephemeral port must never be the operator's: {p}"
        );
    }

    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let cfg_path = tmp.path().join("config.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "port = {legacy_port}\nchassis_port = {chassis_port}\nhost = \"127.0.0.1\"\n\
             data_dir = \"{}\"\ncalibrate = false\nedge_poll = false\n\
             probe_interval_secs = 0\nprobe_interval_busy_secs = 0\n",
            data.display()
        ),
    )
    .unwrap();
    let config = Config::load(&cfg_path).expect("a config carrying the retired `port` key loads");
    assert_eq!(config.chassis_port, chassis_port);
    let daemon = tokio::spawn(threads::run_daemon(config));

    // Wait for the chassis door — bounded.
    let chassis: SocketAddr = format!("127.0.0.1:{chassis_port}").parse().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if TcpStream::connect(chassis).await.is_ok() {
            break;
        }
        if daemon.is_finished() {
            panic!("run_daemon exited before serving: {:?}", daemon.await);
        }
        assert!(std::time::Instant::now() < deadline, "the chassis door never came up");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // SENTINEL: the daemon under test is up and answering on its one door.
    let (mut c, welcome) = Client::connect(chassis).await;
    assert_eq!(welcome["type"], "welcome", "{welcome}");
    let shapes = c.request_ok("shapes/list", json!({})).await;
    assert!(shapes["shapes"].is_array(), "shapes/list answers: {shapes}");

    // THE CLAIM: nothing listens where the legacy door would have.
    let legacy: SocketAddr = format!("127.0.0.1:{legacy_port}").parse().unwrap();
    match TcpStream::connect(legacy).await {
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::ConnectionRefused,
            "nothing listens on the legacy port: {e}"
        ),
        Ok(_) => panic!("nothing listens on the legacy port, but {legacy} accepted a connection"),
    }

    daemon.abort();
    drop(tmp);
}

/// **`update_thread_settings` ON `threads/{id}/send`: THE TURN-START SETTINGS
/// WRITE** (R4.S7, IS-27 ask 1; the operator, `#H112`: *"the turn-start action
/// can change the thread settings. Maybe include an update_thread_settings={}
/// object"*). The patch is applied through the one settings writer before the
/// message is queued; `{}` is accepted and changes nothing; a patch that does
/// not parse refuses the WHOLE send, so nothing is queued under settings the
/// caller did not get.
///
/// **THE MUTATIONS.** (a) ignore the param in `server.rs` → "the patch took"
/// reds (model still `sonnet`). (b) apply it AFTER `send_declaring` → the
/// refused-patch half reds: the message was queued before the patch failed.
/// (c) report `settings_changed: true` unconditionally → the `{}` half reds.
/// The thread's `thread_json` row, read off `threads/list` (the kept read).
/// `threads/{id}/get` served this same row until R2.S3c (R4.S9, HOLDS R9)
/// retired it; Deploy-3 re-pointed R4.S7's test here when the two met.
async fn thread_row(c: &mut Client, id: &str) -> Value {
    let list = c.request_ok("threads/list", json!({"limit": 1000})).await;
    list["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["thread_id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("{id} missing from threads/list: {list}"))
}

#[tokio::test]
async fn send_carries_a_turn_start_settings_write() {
    let (_m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&_m).await;
    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();

    let r = c
        .request_ok(
            &format!("threads/{}/send", id),
            json!({"text": "hello", "update_thread_settings": {"model": "fable"}}),
        )
        .await;
    assert_eq!(r["settings_changed"], true, "{r}");
    let sc = c.wait_event("settings_changed").await;
    assert_eq!(sc["event"]["patch"]["model"], "fable");
    let got = thread_row(&mut c, &id).await;
    assert_eq!(got["settings"]["model"], "fable", "the patch took: {got}");
    c.wait_event("turn_ended").await;

    let r = c
        .request_ok(
            &format!("threads/{}/send", id),
            json!({"text": "again", "update_thread_settings": {}}),
        )
        .await;
    assert_eq!(r["settings_changed"], false, "an empty object changes nothing: {r}");
    c.wait_event("turn_ended").await;

    let before = thread_row(&mut c, &id).await;
    let refused = c
        .request(
            &format!("threads/{}/send", id),
            json!({"text": "never queued", "update_thread_settings": {"model": 5}}),
        )
        .await;
    assert_ne!(refused["type"], "success", "a patch that does not parse refuses the send: {refused}");
    let after = thread_row(&mut c, &id).await;
    assert_eq!(
        before["next_message_id"], after["next_message_id"],
        "nothing was queued under a refused patch: before {before} after {after}"
    );
    assert!(before["next_message_id"].is_u64(), "the instrument reads a real position: {before}");
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;

// ── R9.E1 (R9.F2-3): stderr is JOINED, bounded, before the exit is classified ──
//
// `run_once` classifies a non-zero exit with no structured cause from stderr,
// which a separate task reads. It used to read the buffer without joining that
// task: under load the reader had not run, the buffer was empty, the text became
// "claude exited with code 1", and a credit or login failure was an anonymous
// `claude_error` — the account was NOT barred. R9.F2 forced it by delaying the
// reader 500 ms per line (red every run). The fixture modes below force the same
// thing from the outside: the child exits AT ONCE and a descendant that
// inherited its stderr writes the line 500 ms later, so the text cannot be in
// the buffer when the exit is seen. 500 ms outlasts the reader's own lag after
// an exit (the tail polls every 25 ms here); only a join can see the line.

#[tokio::test]
async fn credit_balance_written_to_stderr_500ms_after_the_exit_still_bars_the_account() {
    let (m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    let account = m.serving_account(&id).unwrap();
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "creditlate"}))
        .await;

    let rs = c.wait_event("retry_scheduled").await;
    assert_eq!(
        rs["event"]["error_kind"], "credit_balance",
        "the credit line arrived on stderr 500 ms after the exit; classified \
         before stderr was joined, it reads as an anonymous claude_error: {rs}"
    );
    let (unavailable, unhealthy) = m.barred_accounts();
    assert!(
        unavailable.contains(&account),
        "the account that reported no credit must leave the placement pool: {unavailable:?}"
    );
    assert!(unhealthy.is_empty(), "a credit balance must not flag the login dead: {unhealthy:?}");
}

/// The pair: a GENUINELY generic failure, delivered the same late way, is still
/// `claude_error` and bars nothing — so "classify everything as credit" cannot
/// pass the test above.
#[tokio::test]
async fn a_generic_error_written_to_stderr_500ms_after_the_exit_is_still_claude_error() {
    let (m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    let account = m.serving_account(&id).unwrap();
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "genericlate"}))
        .await;

    let rs = c.wait_event("retry_scheduled").await;
    assert_eq!(rs["event"]["error_kind"], "claude_error", "a generic failure stays generic: {rs}");
    let (unavailable, unhealthy) = m.barred_accounts();
    assert!(
        !unavailable.contains(&account) && unhealthy.is_empty(),
        "a generic failure bars nothing: {unavailable:?} {unhealthy:?}"
    );
}

/// THE BOUND. A descendant that holds stderr open for 20 s must not hold the
/// turn for 20 s: past `STDERR_DRAIN_BOUND` (5 s) the turn is classified from
/// what was read — here the credit line, written before the exit — and the
/// truncation is logged. The gap from `turn_ended` to `retry_scheduled` is
/// measured both ways: at least ~the bound (the join really waited on the open
/// pipe), and well under the holder's 20 s (the bound really ended it).
#[tokio::test]
async fn stderr_held_open_past_the_5s_drain_bound_is_classified_from_what_was_read() {
    let (m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    let holdpid = tmp.path().join("holdpid");
    // Kill only what this test started — every holder the fixture recorded —
    // on the way out, panic included: a red run must not leak a 20 s holder.
    struct KillHolders(std::path::PathBuf);
    impl Drop for KillHolders {
        fn drop(&mut self) {
            if let Ok(pids) = std::fs::read_to_string(&self.0) {
                for pid in pids.split_whitespace().filter_map(|p| p.parse::<i32>().ok()) {
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
    }
    let _holders = KillHolders(holdpid.clone());
    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": format!("creditheld holdpid={}", holdpid.display())}),
    )
    .await;

    c.wait_event("turn_ended").await;
    let ended = std::time::Instant::now();
    let rs = c.wait_event("retry_scheduled").await;
    let gap = ended.elapsed();

    assert_eq!(
        rs["event"]["error_kind"], "credit_balance",
        "the line read before the bound is what classifies the turn: {rs}"
    );
    assert!(
        gap >= Duration::from_secs(4),
        "the join must wait on a stderr that is still open (bound 5 s); waited {gap:?}"
    );
    assert!(
        gap < Duration::from_secs(15),
        "the holder lives 20 s; the bound must end the wait first; waited {gap:?}"
    );
}

/// R11.J2 (`docs/STRUCTURED-OUTPUT.md` §3.3): a turn whose `result` is
/// `error_max_structured_output_retries` — `is_error: true`, then exit 1, as
/// the live CLI does it — DELIVERED. Only its JSON is missing. It must land
/// `ok`, never on the retry ladder: re-running it would re-do paid work.
///
/// The instrument is the order of three frames: after the first turn ends a
/// plain message is sent, and the first of {`retry_scheduled`, `turn_failed`,
/// the SECOND `turn_started`} to arrive is the verdict. Its opposite
/// (`turnsexhaust`, the same line and exit with a real ceiling subtype) proves
/// the instrument sees a laddered turn when there is one.
async fn first_after_exhaust(mode: &str) -> (Value, Value) {
    let (m, addr, tmp) = start_with(None).await;
    let (mut c, _w) = Client::connect(addr).await;
    c.subscribe_all(&m).await;
    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    c.request_ok(&format!("threads/{}/send", id), json!({"text": mode})).await;
    // The bus keeps frames a wait skipped, so the FIRST turn's own
    // `turn_started` is consumed here, by name, before anything else waits:
    // otherwise it would be the "first" frame below (this happened — every
    // mode read `turn_started`, R11.J2's first run of this instrument).
    let ts1 = c.wait_event("turn_started").await;
    let first_ids = ts1["event"]["message_ids"].clone();
    let rt = c.wait_event("result_turn").await;
    c.wait_event("turn_ended").await;
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "plain hello"}))
        .await;
    let first = c
        .wait_push(|f| {
            f["type"] == "event"
                && match f["event"]["kind"].as_str() {
                    Some("retry_scheduled" | "turn_failed") => true,
                    // a turn carrying the SECOND message (a retry of the first
                    // would carry the first message's ids again).
                    Some("turn_started") => f["event"]["message_ids"] != first_ids,
                    _ => false,
                }
        })
        .await;
    (rt, first)
}

#[tokio::test]
async fn a_schema_retries_exhausted_result_with_exit_1_lands_ok_and_is_not_laddered() {
    let (rt, first) = first_after_exhaust("schemaexhaust").await;
    assert_eq!(rt["event"]["subtype"], "error_max_structured_output_retries", "{rt}");
    assert_eq!(rt["event"]["is_error"], false, "re-classified as an ok turn: {rt}");
    assert_eq!(first["event"]["kind"], "turn_started", "no retry, no failure: {first}");
}

#[tokio::test]
async fn a_real_ceiling_result_with_exit_1_is_still_laddered() {
    let (rt, first) = first_after_exhaust("turnsexhaust").await;
    assert_eq!(rt["event"]["subtype"], "error_max_turns", "{rt}");
    assert_eq!(rt["event"]["is_error"], true, "{rt}");
    assert_eq!(first["event"]["kind"], "retry_scheduled", "{first}");
}

/// The exemption is the schema subtype's alone: a CLEAN `success` result
/// followed by the same exit 1 is laddered by the exit-code door exactly as
/// before R11.J2. (This is the forcing that opposes "skip the door for every
/// delivered turn".)
#[tokio::test]
async fn a_clean_result_then_exit_1_is_still_laddered_by_the_exit_code_door() {
    let (rt, first) = first_after_exhaust("okthenexit").await;
    assert_eq!(rt["event"]["subtype"], "success", "{rt}");
    assert_eq!(rt["event"]["is_error"], false, "{rt}");
    assert_eq!(first["event"]["kind"], "retry_scheduled", "{first}");
}
