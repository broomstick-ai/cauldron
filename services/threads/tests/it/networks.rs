//! # Messaging networks — R4.S3, `docs/agent-networks.md`
//!
//! Two parts.
//!
//! 1. **The end-to-end val** — two threads in one network exchange mail
//!    through the real `/mcp` door (hand-rolled HTTP/1.1, a run token minted
//!    for a turn that is genuinely running), and the network and the mail
//!    survive a daemon restart: a SECOND `ThreadManager` opened over the same
//!    data dir, serving a SECOND chassis on a new ephemeral port, routes the
//!    next exchange over the network the first daemon created.
//! 2. **The rules**, each as an opposed pair (a refusal beside the acceptance
//!    that a constant implementation would also have to produce), called
//!    through the same façade (`threads::agents::call`) the door uses.
//!
//! Every daemon binds `127.0.0.1:0`; nothing here names a live port, a live
//! data dir, or `~/.cauldron` (CAULDRON_HOME is a tempdir).
//!
//! Forced reds are recorded in the R4.S3 report
//! (an internal report in the interface-layer repo), each with its
//! mutation and its red output.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use shapewire::token::Claims;
use threads::config::Config;
use threads::event::ThreadEvent;
use threads::{build_manager, networks, ThreadManager};

// ═════════════════════════════════════════════════════════════════════════════
// fixture
// ═════════════════════════════════════════════════════════════════════════════

/// One process-wide environment: env vars are process-global and the tests in
/// this binary run in parallel, so they all share ONE tempdir for the fixture's
/// state and each daemon gets its own data dir under its own tempdir.
fn env() -> &'static Path {
    static ENV: OnceLock<tempfile::TempDir> = OnceLock::new();
    ENV.get_or_init(|| {
        let tmp = tempfile::tempdir().unwrap();
        let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"));
        std::env::set_var("CAULDRON_HOME", tmp.path().join("cauldron"));
        std::env::set_var("THREADS_CLAUDE_BIN", fake);
        std::env::set_var("THREADS_TAIL_POLL_MS", "25");
        std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
        std::env::set_var("FAKE_STATE_DIR", tmp.path().join("fake-state"));
        std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
        std::fs::create_dir_all(tmp.path().join("fake-state")).unwrap();
        tmp
    })
    .path()
}

fn config_at(dir: &Path) -> Config {
    let mut map = BTreeMap::new();
    std::fs::create_dir_all(dir.join("aa-owned")).unwrap();
    map.insert("aa-owned".to_string(), dir.join("aa-owned").to_string_lossy().to_string());
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts: map,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        probe_interval_busy_secs: 0,
        ..Default::default()
    }
}

struct Daemon {
    m: ThreadManager,
    addr: SocketAddr,
    server: tokio::task::JoinHandle<()>,
}

impl Daemon {
    async fn open(dir: &Path) -> Self {
        env();
        threads::clock::testing::ticking();
        let m = build_manager(config_at(dir)).unwrap();
        m.recover().await;
        let (addr, server) = threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        Self { m, addr, server }
    }

    /// Stop serving and drop the manager: the next `open` over the same dir is
    /// a fresh daemon that knows only what `threads.db` holds.
    fn kill(self) {
        self.server.abort();
        drop(self.m);
    }

    fn thread(&self, cwd: &Path) -> String {
        let settings = json!({"cwd": cwd.to_string_lossy(), "model": "sonnet", "account": {"named": "aa-owned"}});
        self.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    }

    /// Start a turn that runs until `release` exists; return its cookie.
    async fn hold(&self, id: &str, release: &Path) -> String {
        self.m
            .send(id, format!("holdturn release={}", release.display()), threads::SendIntent::Immediate, Value::Null, false)
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(row) = self.m.store.get_running_turn(id).unwrap() {
                return row.spawn_cookie;
            }
            assert!(tokio::time::Instant::now() < deadline, "the hold turn never started on {id}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn idle(&self, id: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            // R4.S4b (HOLDS R19): the runtime's own predicate, which holds
            // across the dequeue → `running_turns` gap; reading the running
            // row and the queue here reported idle inside it.
            if self.m.is_idle(id).unwrap() {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "{id} never went idle");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// **A CHILD, THROUGH THE REAL SPAWN** (R4.S3-land). Lineage has one
    /// owner, `ThreadMeta.parent_id` in R4.S2's managed ledgers, written at the
    /// spawn's commit point — so a test that needs a parent edge makes one the
    /// only way production can: `agents_spawn` through the façade the MCP door
    /// uses. `wake: never` so the child's report does not open a turn on the
    /// parent behind the test's back. Returns once the child's task turn has
    /// run and it is dormant.
    async fn spawn_child(&self, parent: &str) -> String {
        let ack = threads::agents::call(
            &self.m,
            &claims(parent, "test"),
            "agents_spawn",
            json!({"task": "child task: say done", "notify": "hold"}),
        )
        .await
        .unwrap_or_else(|r| panic!("agents_spawn from {parent} refused: {} {}", r.code, r.message));
        let child = ack["thread_id"].as_str().expect("agents_spawn's ack carries thread_id").to_string();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let ended = self.m.store.max_message_id(&child).unwrap() > 0
                && self.m.store.get_running_turn(&child).unwrap().is_none()
                && self.m.store.waiting_queue(&child).unwrap().is_empty()
                && self.m.store.tail_events(&child, 50).unwrap().iter().any(|e| {
                    matches!(e.event, threads::ThreadEvent::TurnEnded { .. })
                });
            if ended {
                return child;
            }
            assert!(tokio::time::Instant::now() < deadline, "the spawned child {child} never finished its task turn");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn token(&self, id: &str, cookie: &str) -> String {
        self.m.run_token_key.mint(&claims(id, cookie))
    }
}

fn claims(id: &str, cookie: &str) -> Claims {
    Claims { thread_id: id.to_string(), run_id: format!("{id}/run-1"), root: id.to_string(), depth: 0, turn: cookie.to_string() }
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// ── the wire: one JSON-RPC POST to /mcp, hand-rolled ─────────────────────────

async fn mcp_call(addr: SocketAddr, token: &str, name: &str, arguments: Value) -> Value {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}).to_string();
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /mcp HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(body.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut buf)).await.expect("response").unwrap();
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").expect("headers");
    let status = String::from_utf8_lossy(&buf[..split]).lines().next().unwrap().to_string();
    assert!(status.contains(" 200 "), "{status}: {}", String::from_utf8_lossy(&buf[split + 4..]));
    let v: Value = serde_json::from_slice(&buf[split + 4..]).unwrap();
    v["result"].clone()
}

fn ok(result: &Value) -> &Value {
    assert_eq!(result["isError"], json!(false), "expected success: {result}");
    &result["structuredContent"]
}

fn refusal(result: &Value) -> String {
    assert_eq!(result["isError"], json!(true), "expected a refusal: {result}");
    result["structuredContent"]["refused"]["code"].as_str().unwrap().to_string()
}

/// Every `UserMessage` on `id`'s ledger that carries network mail, as
/// `(text, metadata.mail)`.
fn mail_on(m: &ThreadManager, id: &str) -> Vec<(String, Value)> {
    m.store
        .events_from(id, 0, 100_000)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            ThreadEvent::UserMessage { text, metadata, .. } if metadata.get("mail").is_some() => {
                Some((text, metadata["mail"].clone()))
            }
            _ => None,
        })
        .collect()
}

fn customs_on(m: &ThreadManager, id: &str, kind: &str) -> Vec<Value> {
    m.store
        .events_from(id, 0, 100_000)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            ThreadEvent::Custom { event_type, data } if event_type == kind => Some(data),
            _ => None,
        })
        .collect()
}

/// How many turns `id` has started — a baseline for "no NEW turn".
fn turns(m: &ThreadManager, id: &str) -> usize {
    m.store
        .events_from(id, 0, 100_000)
        .unwrap()
        .iter()
        .filter(|e| matches!(e.event, ThreadEvent::TurnStarted { .. }))
        .count()
}

fn captured(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. THE VAL: an exchange over the MCP door that survives a daemon restart
// ═════════════════════════════════════════════════════════════════════════════

/// **TWO THREADS IN ONE NETWORK EXCHANGE MAIL THROUGH `/mcp`, AND THE NETWORK
/// AND THE MAIL SURVIVE A DAEMON RESTART.**
///
/// p is the parent, c its child — spawned through `agents_spawn` (R4.S2's
/// managed runtime; the parent edge is its `ThreadMeta.parent_id`). Under the default home network c sees nobody — it cannot address
/// p (asserted). Daemon ONE: p, in a running turn, creates `pair` over the
/// door with c admitted (p holds authority over c through `home(p)`); c, in a
/// running turn, mails p over the door; p is dormant and `wakeable` to c, so
/// the mail opens p's next turn and the fixture captures the prompt p's model
/// received. Daemon ONE is then killed and daemon TWO opened over the same
/// data dir. On TWO: the mail is on p's ledger with its sender and network;
/// c mails p again WITHOUT naming the network — the only route c has to p is
/// `pair`, so this succeeds only if `pair`'s membership came back from disk;
/// and p answers c over `pair`, which c's model receives.
///
/// Forced red in the report: (A) membership wiped on every open
/// (`DELETE FROM network_members` in `apply_schema`'s batch) — the post-restart
/// send is refused `no_route`; (B) `deliver` skips `manager.send` — p's turn
/// never opens and the first wait times out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_threads_exchange_mail_over_the_mcp_door_and_it_survives_a_daemon_restart() {
    let dir = tempfile::tempdir().unwrap();
    let cap_p = dir.path().join("p.prompts");
    let cap_c = dir.path().join("c.prompts");

    // ── daemon ONE ───────────────────────────────────────────────────────
    let d1 = Daemon::open(dir.path()).await;
    let p = d1.thread(dir.path());
    let c = d1.spawn_child(&p).await;

    // c in a running turn: before any network, c has no route to p.
    let rel_c1 = dir.path().join("c1.release");
    let cookie_c = d1.hold(&c, &rel_c1).await;
    let tok_c = d1.token(&c, &cookie_c);
    let r = mcp_call(d1.addr, &tok_c, "agents_message", json!({"agent": p, "body": "too early"})).await;
    assert_eq!(refusal(&r), "no_route", "a child does not see its parent by default (N3): {r}");

    // p in a running turn creates `pair` over the door.
    let rel_p1 = dir.path().join("p1.release");
    let cookie_p = d1.hold(&p, &rel_p1).await;
    let tok_p = d1.token(&p, &cookie_p);
    let r = mcp_call(
        d1.addr,
        &tok_p,
        "agents_network_create",
        json!({"name": "pair", "members": {c.clone(): {"view": true, "visible": true, "wakeable": true}}}),
    )
    .await;
    let net = ok(&r)["network"].as_str().expect("create acknowledges with the minted network id").to_string();
    let created = customs_on(&d1.m, &p, "network_created");
    assert_eq!(created.len(), 1, "the fact is on the caller's own ledger");
    assert_eq!(created[0]["admitted"], json!([c]), "admitted under authority, not offered: {}", created[0]);
    std::fs::write(&rel_p1, "").unwrap();
    d1.idle(&p).await;

    // c mails p over the door; p is dormant and wakeable → p's turn opens.
    let body1 = format!("hello parent, from the child promptfile={}", cap_p.display());
    let r = mcp_call(d1.addr, &tok_c, "agents_message", json!({"agent": p, "body": body1, "network": net})).await;
    ok(&r);
    wait_for("p's model receives c's mail", 20, || captured(&cap_p).contains("hello parent, from the child")).await;
    assert!(
        captured(&cap_p).contains(&format!("[mail from agent {c} over network \"pair\" ({net})]")),
        "the mail is marked as mail, with its sender and network: {}",
        captured(&cap_p)
    );
    std::fs::write(&rel_c1, "").unwrap();
    d1.idle(&p).await;
    d1.idle(&c).await;

    // ── the restart ──────────────────────────────────────────────────────
    let addr1 = d1.addr;
    d1.kill();
    let d2 = Daemon::open(dir.path()).await;
    assert_ne!(d2.addr, addr1, "a second daemon on a second port");

    // The mail is on p's ledger, read back by the NEW daemon.
    let mail = mail_on(&d2.m, &p);
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert!(mail[0].0.contains("hello parent, from the child"));
    assert_eq!(mail[0].1["from"], json!(c));
    assert_eq!(mail[0].1["network"], json!(net));

    // c mails p again, NOT naming the network: `pair` is c's only route to p.
    let rel_c2 = dir.path().join("c2.release");
    let cookie_c2 = d2.hold(&c, &rel_c2).await;
    let tok_c2 = d2.token(&c, &cookie_c2);
    let r = mcp_call(d2.addr, &tok_c2, "agents_message", json!({"agent": p, "body": "still here after the restart"})).await;
    ok(&r);
    wait_for("p receives the post-restart mail", 20, || mail_on(&d2.m, &p).len() == 2).await;
    assert_eq!(mail_on(&d2.m, &p)[1].1["network"], json!(net), "it rode `pair`");
    std::fs::write(&rel_c2, "").unwrap();
    d2.idle(&c).await;
    d2.idle(&p).await;

    // p answers c over `pair`; c's model receives it.
    let rel_p2 = dir.path().join("p2.release");
    let cookie_p2 = d2.hold(&p, &rel_p2).await;
    let tok_p2 = d2.token(&p, &cookie_p2);
    let reply = format!("reply from the parent promptfile={}", cap_c.display());
    let r = mcp_call(d2.addr, &tok_p2, "agents_message", json!({"agent": c, "body": reply, "network": net})).await;
    ok(&r);
    wait_for("c's model receives p's reply", 20, || captured(&cap_c).contains("reply from the parent")).await;
    std::fs::write(&rel_p2, "").unwrap();
    d2.idle(&p).await;
    d2.idle(&c).await;

    // And c's reach, read over the door, shows p as a peer in `pair`.
    let rel_c3 = dir.path().join("c3.release");
    let cookie_c3 = d2.hold(&c, &rel_c3).await;
    let r = mcp_call(d2.addr, &d2.token(&c, &cookie_c3), "agents_reach", json!({})).await;
    let reach = ok(&r).clone();
    let pair = reach["networks"].as_array().unwrap().iter().find(|n| n["id"] == json!(net)).cloned();
    let pair = pair.unwrap_or_else(|| panic!("`pair` is in c's reach: {reach}"));
    assert_eq!(pair["peers"][0]["id"], json!(p), "{reach}");
    std::fs::write(&rel_c3, "").unwrap();
    d2.idle(&c).await;
    d2.kill();
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. THE RULES — each an opposed pair, through the façade the door uses
// ═════════════════════════════════════════════════════════════════════════════

async fn call(m: &ThreadManager, caller: &str, name: &str, args: Value) -> Result<Value, String> {
    threads::agents::call(m, &claims(caller, "test"), name, args)
        .await
        .map_err(|r| r.code.to_string())
}

/// A family: root r with children a and b, and a's child g.
async fn family(d: &Daemon, dir: &Path) -> (String, String, String, String) {
    let r = d.thread(dir);
    let a = d.spawn_child(&r).await;
    let b = d.spawn_child(&r).await;
    let g = d.spawn_child(&a).await;
    (r, a, b, g)
}

/// **HOME: DOWNWARD IS UNCONDITIONAL, NOTHING ELSE IS (N3), AND DELIVERY
/// GRANTS A ROUTE BACK (§1.5).** The root mails a grandchild (allowed, through
/// `home(root)`); the grandchild could not mail the root before that and can
/// after; a sibling still cannot.
///
/// Forced red in the report: `sees_in` returning `true` for every pair — the
/// first `no_route` assertion fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn home_sight_runs_downward_and_delivery_grants_a_route_back() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let (r, a, b, g) = family(&d, dir.path()).await;

    assert_eq!(call(&d.m, &g, "agents_message", json!({"agent": r, "body": "up"})).await.unwrap_err(), "no_route");
    assert_eq!(call(&d.m, &a, "agents_message", json!({"agent": b, "body": "sideways"})).await.unwrap_err(), "no_route");
    // r → g: through home(r), transitively.
    call(&d.m, &r, "agents_message", json!({"agent": g, "body": "down"})).await.unwrap();
    assert_eq!(mail_on(&d.m, &g).len(), 1);
    // g now holds a route back to r; a sibling still has none.
    call(&d.m, &g, "agents_message", json!({"agent": r, "body": "reply"})).await.unwrap();
    assert_eq!(call(&d.m, &b, "agents_message", json!({"agent": g, "body": "sideways"})).await.unwrap_err(), "no_route");
    d.kill();
}

/// **INTRODUCTION IS NON-TRANSITIVE (N4); AUTHORITY ADMITS, ELSE IT IS AN
/// OFFER (§1.3).** a cannot add its sibling b (a does not see b); r can, and b
/// is admitted at once (r holds authority over b through `home(r)`). a adds
/// its own child g: admitted. A thread a sees but holds no authority over is
/// offered, and joins; `join` without an offer is `no_offer`.
///
/// Forced red in the report: `plan_admits` skipping the `sees` check —
/// a's add of b is accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introduction_is_non_transitive_and_non_authority_adds_are_offers() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let (r, a, b, g) = family(&d, dir.path()).await;

    let e = call(&d.m, &a, "agents_network_create", json!({"name": "x", "members": {b.clone(): {}}})).await;
    assert_eq!(e.unwrap_err(), "no_route", "a does not see its sibling");
    let ack = call(&d.m, &r, "agents_network_create", json!({"name": "rb", "members": {b.clone(): {"view": true}}})).await.unwrap();
    let rb = ack["network"].as_str().unwrap().to_string();
    assert_eq!(customs_on(&d.m, &r, "network_created")[0]["admitted"], json!([b]));
    assert!(customs_on(&d.m, &b, "network_created").is_empty(), "N7: membership facts land only on the actor's ledger");

    // a creates a network with g (its child: authority) — admitted.
    let ack = call(&d.m, &a, "agents_network_create", json!({"name": "ag", "members": {g.clone(): {}}})).await.unwrap();
    let ag = ack["network"].as_str().unwrap().to_string();
    assert_eq!(customs_on(&d.m, &a, "network_created")[0]["admitted"], json!([g]));

    // In `rb`, b has `view`, so b SEES r there — but r is not imposable to b,
    // so b adding r to a network of its own is an OFFER.
    let ack = call(&d.m, &b, "agents_network_create", json!({"name": "br", "members": {r.clone(): {}}})).await.unwrap();
    let br = ack["network"].as_str().unwrap().to_string();
    let created = &customs_on(&d.m, &b, "network_created")[0];
    assert_eq!(created["offered"], json!([r]), "{created}");
    assert_eq!(created["admitted"], json!([]), "{created}");
    assert_eq!(call(&d.m, &r, "agents_network_join", json!({"network": ag})).await.unwrap_err(), "no_offer");
    call(&d.m, &r, "agents_network_join", json!({"network": br})).await.unwrap();
    let reach = call(&d.m, &r, "agents_reach", json!({})).await.unwrap();
    assert!(reach["networks"].as_array().unwrap().iter().any(|n| n["id"] == json!(br)), "{reach}");
    let _ = rb;
    d.kill();
}

/// **REFUSE, NEVER CLAMP (N8): `max_chars` BITES AT ONE OVER, NOT AT THE
/// LIMIT.** A network with `max_chars: 10`: ten characters are delivered,
/// eleven are refused `too_large` and nothing reaches the recipient. A rule
/// over the producer ceiling is refused at create.
///
/// Forced red in the report: `check_size` using `>=` — the ten-char message is
/// refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_chars_refuses_one_over_and_accepts_exactly_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let (r, a, _b, _g) = family(&d, dir.path()).await;
    let e = call(&d.m, &r, "agents_network_create", json!({"name": "big", "rules": {"max_chars": networks::MAX_MESSAGE_CHARS + 1}})).await;
    assert_eq!(e.unwrap_err(), "too_large");
    let ack = call(&d.m, &r, "agents_network_create", json!({"name": "tiny", "rules": {"max_chars": 10}, "members": {a.clone(): {"wakeable": true}}})).await.unwrap();
    let n = ack["network"].as_str().unwrap().to_string();
    assert_eq!(
        call(&d.m, &r, "agents_message", json!({"agent": a, "body": "ééééééééééé", "network": n})).await.unwrap_err(),
        "too_large",
        "eleven chars (22 bytes) over a ten-char limit"
    );
    assert!(mail_on(&d.m, &a).is_empty(), "a refused message reaches nobody");
    call(&d.m, &r, "agents_message", json!({"agent": a, "body": "éééééééééé", "network": n})).await.unwrap();
    assert_eq!(mail_on(&d.m, &a).len(), 1, "exactly ten chars is delivered — the unit is chars, not bytes");
    d.idle(&a).await;
    d.kill();
}

/// **A DORMANT MEMBER THAT IS NOT `wakeable` IS NOT WOKEN — THE MAIL IS HELD,
/// DURABLY, AND RELEASED AHEAD OF THE NEXT MAIL THAT DOES WAKE IT.** Outlasts
/// the longest period in the path: the fake turn runs in well under a second
/// and the daemon's tail poll is 25 ms; we wait 1.5 s for a turn that must not
/// start, after watching the wakeable counterpart start one within that bound.
///
/// Forced red in the report: `deliver` ignoring `wakeable` — the held count is
/// 0 and a turn starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_wakeable_dormant_member_is_held_not_woken_and_released_by_the_next_waking_mail() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let (r, a, b, _g) = family(&d, dir.path()).await;
    // `quiet`: a is a member that is NOT wakeable; b in `loud` is.
    let quiet = call(&d.m, &r, "agents_network_create", json!({"name": "quiet", "members": {a.clone(): {}}})).await.unwrap()["network"]
        .as_str().unwrap().to_string();
    let loud = call(&d.m, &r, "agents_network_create", json!({"name": "loud", "members": {b.clone(): {"wakeable": true}}})).await.unwrap()["network"]
        .as_str().unwrap().to_string();

    // Both children already ran their spawn task turn: count turns FROM HERE.
    let (a0, b0) = (turns(&d.m, &a), turns(&d.m, &b));
    call(&d.m, &r, "agents_message", json!({"agent": b, "body": "wake up", "network": loud})).await.unwrap();
    let started = tokio::time::Instant::now();
    wait_for("the wakeable member's turn", 10, || turns(&d.m, &b) > b0).await;
    let bound = started.elapsed();

    call(&d.m, &r, "agents_message", json!({"agent": a, "body": "do not wake", "network": quiet})).await.unwrap();
    assert_eq!(d.m.store.count_held_mail(&a).unwrap(), 1, "held");
    assert_eq!(customs_on(&d.m, &a, "mail_held").len(), 1, "marked on the recipient's ledger");
    // The longest period in the dispatch path is the resume tick
    // (THREADS_RESUME_TICK_MS, 60 s in production), which dispatches any thread
    // with a WAITING queue row. A held message has no queue row at all, so no
    // tick of any length can start a turn from it — asserted structurally here,
    // and then observed over the bounded wait below.
    assert!(d.m.store.waiting_queue(&a).unwrap().is_empty(), "held mail must not be a queue row");
    tokio::time::sleep(Duration::from_millis(1500).max(bound * 3)).await;
    assert_eq!(turns(&d.m, &a), a0, "a non-wakeable dormant member was woken");
    // Mail over home(r), where a IS wakeable, wakes a — and the held message
    // is released first.
    call(&d.m, &r, "agents_message", json!({"agent": a, "body": "now wake", "network": format!("home:{r}")})).await.unwrap();
    assert_eq!(d.m.store.count_held_mail(&a).unwrap(), 0);
    let mail = mail_on(&d.m, &a);
    assert_eq!(mail.len(), 2, "{mail:?}");
    assert!(mail[0].0.contains("do not wake") && mail[1].0.contains("now wake"), "held first, in order: {mail:?}");
    d.idle(&a).await;
    d.idle(&b).await;
    d.kill();
}

/// **NO UPWARD LEDGER READ (N9), WHATEVER THE FLAGS SAY; DOWNWARD IS THE
/// DEFAULT.** r reads its grandchild's log; g cannot read r's even after r
/// makes itself `visible` and `inspectable` in a network g is in.
///
/// Forced red in the report: `reads` without the ancestor clause — g's read
/// of r is accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_thread_reads_an_ancestors_log_whatever_the_flags_say() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let (r, _a, b, g) = family(&d, dir.path()).await;
    call(&d.m, &r, "agents_log", json!({"agent": g})).await.unwrap();
    call(&d.m, &r, "agents_network_create", json!({
        "name": "open", "self": {"view": true, "visible": true, "inspectable": true},
        "members": {g.clone(): {"view": true}, b.clone(): {"view": true, "inspectable": true}}
    }))
    .await
    .unwrap();
    assert_eq!(call(&d.m, &g, "agents_log", json!({"agent": r})).await.unwrap_err(), "no_route");
    // The same flags on a non-ancestor DO grant the read: g reads b.
    call(&d.m, &g, "agents_log", json!({"agent": b})).await.unwrap();
    d.kill();
}

/// **`interrupt` IS NETWORK-GATED AND NEVER SILENTLY DOWNGRADED.** To a member
/// that is not `interruptible`: `not_interruptible`, and nothing is delivered.
/// To one that is (every descendant in its parent's home): accepted since
/// R4.S8 — the cut itself is driven in `tests/spawn_params.rs`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_is_gated_by_the_flag_and_never_downgraded() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let (r, a, _b, _g) = family(&d, dir.path()).await;
    let n = call(&d.m, &r, "agents_network_create", json!({"name": "calm", "members": {a.clone(): {}}})).await.unwrap()["network"]
        .as_str().unwrap().to_string();
    let e = call(&d.m, &r, "agents_message", json!({"agent": a, "body": "stop", "intent": "interrupt", "network": n})).await;
    assert_eq!(e.unwrap_err(), "not_interruptible");
    assert!(mail_on(&d.m, &a).is_empty(), "nothing was delivered under a lower intent");
    call(&d.m, &r, "agents_message", json!({"agent": a, "body": "stop", "intent": "interrupt", "network": format!("home:{r}")}))
        .await
        .expect("interruptible over the home network");
    assert_eq!(mail_on(&d.m, &a).len(), 1, "delivered once, as interrupt mail");
    d.kill();
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. THE SEAMS R4.S2 OPENED — wired at R4.S3-land
// ═════════════════════════════════════════════════════════════════════════════

/// **LINEAGE HAS ONE OWNER, AND THE NETWORKS READ IT: a child made by
/// `agents_spawn` is in `home(parent)`; a thread created any other way is
/// not.** r spawns c through the managed runtime (the parent edge is c's
/// `ThreadMeta.parent_id`, committed by r's `child{started}`); x is an ordinary
/// thread. r reaches c (home, downward) and is refused x (`no_route`, no
/// network gives sight of it); `agents_reach` lists c under r's home and not
/// x; c still cannot address r (N3).
///
/// Forced red in the land report: `Managed::children_of` answering nothing —
/// r → c is refused `no_route`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spawned_child_is_in_its_parents_home_and_an_unspawned_thread_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    let c = d.spawn_child(&r).await;
    let x = d.thread(dir.path());
    assert_eq!(networks::ancestors(&d.m, &c).unwrap(), vec![r.clone()], "c's one ancestor is r, read from ThreadMeta");
    assert!(networks::ancestors(&d.m, &x).unwrap().is_empty());
    // Before any mail (delivery grants a route back, §1.5): c cannot address r.
    assert_eq!(call(&d.m, &c, "agents_message", json!({"agent": r, "body": "up"})).await.unwrap_err(), "no_route");
    call(&d.m, &r, "agents_message", json!({"agent": c, "body": "down"})).await.unwrap();
    assert_eq!(call(&d.m, &r, "agents_message", json!({"agent": x, "body": "stranger"})).await.unwrap_err(), "no_route");
    let reach = call(&d.m, &r, "agents_reach", json!({})).await.unwrap().to_string();
    assert!(reach.contains(&c), "r's reach lists its spawned child: {reach}");
    assert!(!reach.contains(&x), "r's reach does not list an unrelated thread: {reach}");
    d.idle(&c).await;
    d.kill();
}

/// **HELD MAIL RIDES THE NEXT TURN THAT STARTS FOR ANY REASON — the turn-start
/// seam (`networks::release_held`, called from `maybe_dispatch`).** a is a
/// dormant member of `quiet`, where it is not `wakeable`: r's mail is held and
/// no turn starts (opposed: held count 1, no queue row, no new turn over a
/// bounded wait). Then the OPERATOR sends a an ordinary message — not mail, so
/// `deliver`'s own release never runs. The turn that opens carries the held
/// mail: the fixture captures it in the prompt, the held row is gone, and the
/// mail is on a's ledger ahead of nothing it should not be.
///
/// Forced red in the land report: the `release_held` call deleted from
/// `maybe_dispatch` — the held count stays 1 and the prompt lacks the mail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_mail_is_released_into_the_next_turn_that_starts_for_any_reason() {
    let dir = tempfile::tempdir().unwrap();
    let cap = dir.path().join("a.prompts");
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    let a = d.spawn_child(&r).await;
    let quiet = call(&d.m, &r, "agents_network_create", json!({"name": "quiet", "members": {a.clone(): {}}})).await.unwrap()["network"]
        .as_str().unwrap().to_string();
    let a0 = turns(&d.m, &a);
    call(&d.m, &r, "agents_message", json!({"agent": a, "body": "held words for a", "network": quiet})).await.unwrap();
    assert_eq!(d.m.store.count_held_mail(&a).unwrap(), 1, "held");
    assert!(d.m.store.waiting_queue(&a).unwrap().is_empty(), "held mail is not a queue row");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(turns(&d.m, &a), a0, "held mail woke nobody");

    // An ordinary operator send — not network mail.
    d.m.send(&a, format!("operator nudge promptfile={}", cap.display()), threads::SendIntent::Immediate, Value::Null, false)
        .unwrap();
    wait_for("a's next turn carries the held mail", 20, || captured(&cap).contains("held words for a")).await;
    assert!(captured(&cap).contains("operator nudge"), "{}", captured(&cap));
    assert_eq!(d.m.store.count_held_mail(&a).unwrap(), 0, "released exactly once");
    let mail = mail_on(&d.m, &a);
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert!(mail[0].0.contains("held words for a"), "{mail:?}");
    d.idle(&a).await;
    d.kill();
}

/// **HELD MAIL SURVIVES A DEATH INSIDE ITS OWN RELEASE, AND IS DELIVERED
/// EXACTLY ONCE** (R4.S3b — the R4.S3-land inspector's F3). a holds two pieces
/// of mail. `release_held` is stopped as a kill would stop it
/// (`networks::halt_release_before`) — before its first enqueue, and (second
/// pass) between the first enqueue and the second — and the daemon is dropped.
/// A SECOND daemon over the same data dir then gets an ordinary operator send
/// for a. Both messages must reach a's ledger, each exactly once, nothing may
/// stay held, and the mail still held at the kill must ride the turn that
/// opens (the fixture captures that prompt).
///
/// The evidence is read from the second daemon's store, after a turn it ran —
/// not from anything the halted release wrote.
///
/// Forced red in the R4.S3b report: the pre-fix `release_held`
/// (`take_held_mail` — SELECT + DELETE committed — then the enqueues) loses
/// every message not yet enqueued, so the wait times out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_mail_survives_a_kill_inside_its_release_and_is_delivered_exactly_once() {
    const HELD: [&str; 2] = ["held mail one", "held mail two"];
    for nth in [0usize, 1] {
        let dir = tempfile::tempdir().unwrap();
        let cap = dir.path().join("a.prompts");
        let d = Daemon::open(dir.path()).await;
        let r = d.thread(dir.path());
        let a = d.spawn_child(&r).await;
        let quiet = call(&d.m, &r, "agents_network_create", json!({"name": "quiet", "members": {a.clone(): {}}})).await.unwrap()["network"]
            .as_str().unwrap().to_string();
        for body in HELD {
            call(&d.m, &r, "agents_message", json!({"agent": a, "body": body, "network": quiet})).await.unwrap();
        }
        assert_eq!(d.m.store.count_held_mail(&a).unwrap(), 2, "nth={nth}: both held");

        networks::halt_release_before(&a, nth);
        let err = networks::release_held(&d.m, &a).expect_err("the release stops at the kill point");
        assert!(err.to_string().contains("halted"), "nth={nth}: {err}");
        d.kill();

        let d2 = Daemon::open(dir.path()).await;
        d2.m.send(&a, format!("operator nudge promptfile={}", cap.display()), threads::SendIntent::Immediate, Value::Null, false)
            .unwrap();
        wait_for(&format!("nth={nth}: every held message reaches a after the restart"), 30, || {
            let mail = mail_on(&d2.m, &a);
            HELD.iter().all(|t| mail.iter().any(|(x, _)| x.contains(t)))
        })
        .await;
        // The dispatch is asynchronous: wait for the turn the nudge opened to
        // hand its prompt to the model, then for a to go quiet.
        // The dispatch is asynchronous: wait until every held message has been
        // handed to a turn — its message id in a `TurnStarted` — not merely
        // written to the ledger.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let ridden = held_ids_in_turns(&d2.m, &a, &HELD);
            if ridden.iter().all(|r| *r) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "nth={nth}: a held message never rode a turn after the restart: {ridden:?}\nmail: {:?}\nwaiting: {:?}\nheld: {}",
                mail_on(&d2.m, &a),
                d2.m.store.waiting_queue(&a).unwrap().iter().map(|q| q.text.clone()).collect::<Vec<_>>(),
                d2.m.store.count_held_mail(&a).unwrap(),
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        d2.idle(&a).await;
        assert_eq!(d2.m.store.count_held_mail(&a).unwrap(), 0, "nth={nth}: nothing left held");
        let mail = mail_on(&d2.m, &a);
        for t in HELD {
            let n = mail.iter().filter(|(x, _)| x.contains(t)).count();
            assert_eq!(n, 1, "nth={nth}: `{t}` delivered exactly once: {mail:?}");
        }
        if nth == 0 {
            // Nothing was enqueued before the kill, so both ride the turn the
            // nudge opens — what the model received, captured by the fixture.
            // (At nth=1 the first is a `turn_end` queue row from before the
            // kill and is batched apart from the `immediate` nudge; the
            // TurnStarted evidence above covers it.)
            wait_for(&format!("nth={nth}: both held messages reached the model"), 30, || {
                HELD.iter().all(|t| captured(&cap).contains(t))
            })
            .await;
        }
        d2.kill();
    }
}

/// For each of `texts`: whether the `UserMessage` carrying it on `id`'s ledger
/// has its message id in some `TurnStarted.message_ids` — i.e. a turn took it.
fn held_ids_in_turns(m: &ThreadManager, id: &str, texts: &[&str]) -> Vec<bool> {
    let evs = m.store.events_from(id, 0, 100_000).unwrap();
    let started: Vec<u64> = evs
        .iter()
        .filter_map(|e| match &e.event {
            ThreadEvent::TurnStarted { message_ids, .. } => Some(message_ids.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    texts
        .iter()
        .map(|t| {
            evs.iter().any(|e| match &e.event {
                ThreadEvent::UserMessage { text, .. } if text.contains(t) => {
                    e.message_id.map(|mid| started.contains(&mid)).unwrap_or(false)
                }
                _ => false,
            })
        })
        .collect()
}

/// **CONCURRENT RELEASES ENQUEUE EACH HELD MESSAGE ONCE** (R4.S3b). Once the
/// delete moved after the enqueue (F3), two releases racing on one thread —
/// `deliver` and `maybe_dispatch` both call it — could each read the same rows
/// and each enqueue them. Four OS threads release a's 24 held messages at once;
/// every message must land on a's ledger exactly once and none stay held.
///
/// Forced red in the R4.S3b report: the `RELEASE` lock in `release_held`
/// removed — messages are enqueued more than once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_releases_enqueue_each_held_message_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    let a = d.spawn_child(&r).await;
    let quiet = call(&d.m, &r, "agents_network_create", json!({"name": "quiet", "members": {a.clone(): {}}})).await.unwrap()["network"]
        .as_str().unwrap().to_string();
    let texts: Vec<String> = (0..24).map(|i| format!("racing held mail #{i:02}#")).collect();
    for t in &texts {
        call(&d.m, &r, "agents_message", json!({"agent": a, "body": t, "network": quiet})).await.unwrap();
    }
    assert_eq!(d.m.store.count_held_mail(&a).unwrap(), texts.len(), "all held");

    let go = Arc::new(std::sync::Barrier::new(4));
    let released: usize = std::thread::scope(|s| {
        let hs: Vec<_> = (0..4)
            .map(|_| {
                let (m, a, go) = (&d.m, &a, go.clone());
                s.spawn(move || {
                    go.wait();
                    networks::release_held(m, a).unwrap()
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).sum()
    });
    assert_eq!(d.m.store.count_held_mail(&a).unwrap(), 0, "nothing left held");
    let mail = mail_on(&d.m, &a);
    for t in &texts {
        let n = mail.iter().filter(|(x, _)| x.contains(t.as_str())).count();
        assert_eq!(n, 1, "`{t}` enqueued exactly once (released total {released}): {n}");
    }
    assert_eq!(released, texts.len(), "the four releases between them released each row once");
    // Let the queue drain into a turn before the daemon goes.
    d.m.send(&a, "drain".into(), threads::SendIntent::Immediate, Value::Null, false).unwrap();
    d.idle(&a).await;
    d.kill();
}

// ═════════════════════════════════════════════════════════════════════════════
// R10.M2 (L-8, E21-4): RIDERS AND THE NETWORK SEAMS
// ═════════════════════════════════════════════════════════════════════════════

/// The message ids of every `turn_started` on `id`, one Vec per turn.
fn turn_ids(m: &ThreadManager, id: &str) -> Vec<Vec<u64>> {
    m.store
        .events_from(id, 0, 100_000)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            ThreadEvent::TurnStarted { message_ids, .. } => Some(message_ids),
            _ => None,
        })
        .collect()
}

/// Hold `a`'s next drain with its attempt open, send it `text`, and cut that
/// attempt with `cut`; returns the message id, now riding.
async fn ride_one(d: &Daemon, a: &str, text: &str, cut: impl FnOnce()) -> u64 {
    d.m.hold_attempts_for_test(a).unwrap();
    let mid = d.m.send(a, text.into(), threads::SendIntent::Immediate, Value::Null, false).unwrap();
    wait_for("the drain holds its attempt open", 10, || d.m.open_attempt_for_test(a).unwrap() == Some(vec![mid])).await;
    cut();
    let q: Vec<(u64, String)> = d.m.store.waiting_queue(a).unwrap().into_iter().map(|r| (r.message_id, r.dispatch)).collect();
    assert_eq!(q, vec![(mid, "ride".to_string())], "the cut attempt's message rides");
    mid
}

/// **A THREAD HOLDING ONLY RIDERS IS NOT A TURN ABOUT TO START: ITS HELD MAIL
/// STAYS HELD AND NOTHING RUNS — UNTIL A REAL MESSAGE STARTS A TURN, WHICH
/// CARRIES THE RIDER FIRST AND RELEASES THE HELD MAIL.** `maybe_dispatch`
/// releases held mail only when a turn is about to start (R4.S3-land); a
/// rider is a queue row that never starts one. a, dormant, gets a rider (an
/// operator cut of its open attempt) and a held letter over `quiet`; then the
/// dispatcher is poked directly (`nudge_thread`, the whole path) and 1.5 s
/// pass (sixty 25 ms tail polls). Red with the riders-only early return
/// removed: the poke releases the letter as an eager row, which starts the
/// turn the rider must never cause.
///
/// Opposed: the operator's ordinary message then starts a turn naming the
/// rider first, and the held letter is released into it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn riders_alone_release_no_held_mail_and_start_no_turn() {
    let dir = tempfile::tempdir().unwrap();
    let cap = dir.path().join("a.prompts");
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    let a = d.spawn_child(&r).await;
    let quiet = call(&d.m, &r, "agents_network_create", json!({"name": "quiet", "members": {a.clone(): {}}})).await.unwrap()["network"]
        .as_str().unwrap().to_string();
    let rider = ride_one(&d, &a, "RIDER-FOR-A", || d.m.cancel_turn(&a).unwrap()).await;
    d.m.release_attempt_for_test(&a).unwrap();
    d.idle(&a).await;
    let t0 = turn_ids(&d.m, &a);
    call(&d.m, &r, "agents_message", json!({"agent": a, "body": "held words for a", "network": quiet})).await.unwrap();
    assert_eq!(d.m.store.count_held_mail(&a).unwrap(), 1, "held");

    d.m.nudge_thread(&a);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(turn_ids(&d.m, &a), t0, "a rider and held mail start no turn");
    assert_eq!(d.m.store.count_held_mail(&a).unwrap(), 1, "the held mail stays held");

    let nudge = d
        .m
        .send(&a, format!("operator nudge promptfile={}", cap.display()), threads::SendIntent::Immediate, Value::Null, false)
        .unwrap();
    wait_for("a's next turn starts", 20, || turn_ids(&d.m, &a).len() == t0.len() + 1).await;
    let ids = turn_ids(&d.m, &a).pop().unwrap();
    assert_eq!(&ids[..2], &[rider, nudge], "the rider goes first, then the message that started the turn: {ids:?}");
    wait_for("the prompt is captured", 20, || captured(&cap).contains("held words for a")).await;
    let p = captured(&cap);
    assert!(p.find("RIDER-FOR-A").unwrap() < p.find("operator nudge").unwrap(), "{p}");
    assert_eq!(d.m.store.count_held_mail(&a).unwrap(), 0, "released into that turn");
    d.idle(&a).await;
    d.kill();
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
