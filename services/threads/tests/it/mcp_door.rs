//! # The MCP door and the `agents` façade, proved on the real wire — R4.S1
//!
//! `docs/managed-subagents.md` §2 (the surface), §5.1 (the run token), §7 and
//! §8.1 (the carrier). Every test here was **forced red before it was made
//! green**, and the mutation plus the red's text is in the test's docstring.
//!
//! ## Addresses, because the dead-port discipline asks for them
//!
//! Every daemon in this file binds `127.0.0.1:0` through
//! `threads::chassis::serve`; every dial is to the address that bind returned.
//! `THREADS_CHASSIS_PORT` is never read — these tests do not go through
//! `run_daemon`. The one process that dials from OUTSIDE the test binary is the
//! `fake-claude` fixture in `mcpprobe` mode, and it dials the URL the test puts
//! in its prompt, which is the same ephemeral address. Nothing here names
//! 3651, 3652 or 3653.
//!
//! ## A probe must not witness itself
//!
//! The handshake test is the only one that uses a client this file did not
//! write: `fake-claude mcpprobe` performs the sequence the installed `claude`
//! 2.1.280 performs (read out of its bundle; cited in the fixture), with `curl`,
//! from a process the DAEMON spawned as a turn — so the token it presents is
//! for a turn that genuinely exists in `running_turns`. The test mints that
//! token with the daemon's own key, which is what the spawn path will do in
//! R4.S2; nothing on this branch injects it yet. Every other test dials with a
//! hand-rolled HTTP/1.1 client, which is fine for GUARDS — a forged signature
//! does not care who presents it — and would not be fine for the handshake.

#![allow(dead_code)]

#[path = "support/bus.rs"]
mod bus;

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use shapewire::envelope::codes;
use shapewire::token::{Claims, Key};
use threads::agents::{self, Kind, Tool, CALLER, SURFACE};
use threads::config::Config;
use threads::server::WS_METHODS;
use threads::{build_manager, ThreadManager};

// ═════════════════════════════════════════════════════════════════════════════
// fixture
// ═════════════════════════════════════════════════════════════════════════════

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env(tmp: &Path) {
    std::env::set_var("CAULDRON_HOME", tmp.join("cauldron"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var("FAKE_STATE_DIR", tmp.join("fake-state"));
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::fs::create_dir_all(tmp.join("fake-state")).unwrap();
}

fn config_at(dir: &Path, accounts: &[&str]) -> Config {
    let mut map = BTreeMap::new();
    for a in accounts {
        std::fs::create_dir_all(dir.join(a)).unwrap();
        map.insert(a.to_string(), dir.join(a).to_string_lossy().to_string());
    }
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
    tmp: tempfile::TempDir,
}

impl Daemon {
    async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        set_env(tmp.path());
        threads::clock::testing::ticking();
        let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
        let (addr, h) = threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        // The daemon lives as long as the test; dropping the handle would close
        // the socket the client is about to open.
        std::mem::forget(h);
        Self { m, addr, tmp }
    }

    fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    fn thread(&self) -> String {
        let settings = json!({
            "cwd": self.tmp.path().to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
        });
        self.m
            .create_thread(None, serde_json::from_value(settings).unwrap())
            .unwrap()
    }

    /// Send `prompt` and wait until the turn is RUNNING — a `running_turns`
    /// row exists — returning its spawn cookie.
    async fn running(&self, id: &str, prompt: &str) -> String {
        self.m
            .send(id, prompt.to_string(), threads::SendIntent::Immediate, Value::Null, false)
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(row) = self.m.store.get_running_turn(id).unwrap() {
                return row.spawn_cookie;
            }
            assert!(tokio::time::Instant::now() < deadline, "the turn never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Wait until no turn is running on `id`.
    async fn ended(&self, id: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while self.m.store.get_running_turn(id).unwrap().is_some() {
            assert!(tokio::time::Instant::now() < deadline, "the turn never ended");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn claims(&self, id: &str, cookie: &str) -> Claims {
        Claims {
            thread_id: id.to_string(),
            run_id: format!("{id}/run-1"),
            root: id.to_string(),
            depth: 0,
            turn: cookie.to_string(),
        }
    }

    fn mint(&self, id: &str, cookie: &str) -> String {
        self.m.run_token_key.mint(&self.claims(id, cookie))
    }
}

// ── a hand-rolled HTTP/1.1 client, for the guards ────────────────────────────

struct Res {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!("body is not JSON ({e}): {:?}", String::from_utf8_lossy(&self.body))
        })
    }
}

async fn raw(addr: SocketAddr, method: &str, target: &str, extra: &[(&str, &str)], body: &[u8]) -> Res {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut head = format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
    s.flush().await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut buf))
        .await
        .expect("the response did not finish")
        .unwrap();
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").expect("headers terminate");
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let body = buf[split + 4..].to_vec();
    let mut lines = head.lines();
    let status: u16 = lines.next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Res { status, headers, body }
}

/// One JSON-RPC POST the way the SDK's transport sends it, with `token` as the
/// bearer (or no `Authorization` at all).
async fn mcp(addr: SocketAddr, token: Option<&str>, body: Value) -> Res {
    let auth = token.map(|t| format!("Bearer {t}"));
    let mut extra: Vec<(&str, &str)> = vec![
        ("Content-Type", "application/json"),
        ("Accept", "application/json, text/event-stream"),
    ];
    if let Some(a) = auth.as_deref() {
        extra.push(("Authorization", a));
    }
    raw(addr, "POST", "/mcp", &extra, body.to_string().as_bytes()).await
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}

fn refused(res: &Res) -> (String, String) {
    let r = &res.json()["result"];
    assert_eq!(r["isError"], json!(true), "{}", res.json());
    (
        r["structuredContent"]["refused"]["code"].as_str().unwrap().to_string(),
        r["structuredContent"]["refused"]["message"].as_str().unwrap().to_string(),
    )
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. THE HANDSHAKE, BY THE INSTALLED CLIENT'S SEQUENCE, FROM A REAL TURN
// ═════════════════════════════════════════════════════════════════════════════

/// **A REAL MCP CLIENT COMPLETES THE HANDSHAKE AGAINST `/mcp` WITH A MINTED
/// TOKEN AND LISTS EXACTLY THE §2 TOOLS** — and the token dies with the turn.
///
/// The client is `fake-claude mcpprobe`, spawned BY THE DAEMON as this
/// thread's turn, performing the 2.1.280 sequence with `curl`. It proposes
/// `2025-11-25`, is answered `2025-06-18` (the newest this door speaks; in the
/// client's accepted set), sends `notifications/initialized` (202), pings,
/// lists, calls the one live tool on ITSELF and a refused one, and sends a
/// `budget_key` to see it refused rather than ignored.
///
/// **Forced red** three ways, separately: (1) `mcp::http::post` answering
/// `202` to every request (the `initialize` line reads `STEP initialize 202`
/// and the version assertion fails on an empty body); (2) the `/mcp` route
/// removed from `into_router` — every step reads `404`; (3) the
/// `running_turns` check in `verify` inverted — the post-turn call below is
/// `200` instead of `401`.
#[tokio::test]
async fn the_installed_clients_handshake_completes_and_lists_exactly_the_surface() {
    let d = Daemon::start().await;
    let id = d.thread();
    let tokenfile = d.tmp.path().join("run.token");
    let out = d.tmp.path().join("mcp.out");
    let prompt = format!(
        "mcpprobe mcpurl={} tokenfile={} mcpout={} agentid={}",
        d.url(),
        tokenfile.display(),
        out.display(),
        id
    );
    let cookie = d.running(&id, &prompt).await;
    let token = d.mint(&id, &cookie);
    // The turn is running and nothing on disk names a token yet: hand it over.
    std::fs::write(&tokenfile, &token).unwrap();
    d.ended(&id).await;

    let text = std::fs::read_to_string(&out).expect("the probe wrote its record");
    // The wire, as the probe saw it — printed so a `--nocapture` run shows the
    // real handshake rather than this file's reading of it.
    println!("── fake-claude mcpprobe record ──\n{text}");
    let mut steps: BTreeMap<String, (u16, String)> = BTreeMap::new();
    let mut bodies: BTreeMap<String, Value> = BTreeMap::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, ' ');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("STEP"), Some(name), Some(rest)) => {
                let (code, session) = rest.split_once(" session=").unwrap();
                steps.insert(name.into(), (code.parse().unwrap(), session.into()));
            }
            (Some("BODY"), Some(name), Some(rest)) => {
                let v = if rest.trim().is_empty() { Value::Null } else { serde_json::from_str(rest).unwrap_or_else(|e| panic!("{name}: {e}: {rest}")) };
                bodies.insert(name.into(), v);
            }
            _ => panic!("unparsed probe line: {line}"),
        }
    }
    let step = |n: &str| steps.get(n).unwrap_or_else(|| panic!("no step {n}: {text}")).clone();

    // initialize: 200, a session, the negotiated version, tools capability.
    let (code, session) = step("initialize");
    assert_eq!(code, 200, "{text}");
    assert_ne!(session, "none", "initialize mints a session id: {text}");
    let init = &bodies["initialize"]["result"];
    assert_eq!(init["protocolVersion"], json!("2025-06-18"), "{init}");
    assert_eq!(init["capabilities"], json!({"tools": {}}));
    assert_eq!(init["serverInfo"]["name"], json!("threads"));
    // notifications/initialized: 202, no body — sent with the session and
    // version headers the client learned.
    assert_eq!(step("initialized").0, 202, "{text}");
    assert_eq!(bodies["initialized"], Value::Null);
    assert_eq!(step("ping").0, 200);
    assert_eq!(bodies["ping"]["result"], json!({}));
    // tools/list: exactly §2's twelve, in surface order, each with a schema.
    assert_eq!(step("tools_list").0, 200);
    let tools = bodies["tools_list"]["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        SURFACE.iter().map(|t| t.name).collect::<Vec<_>>(),
        "{text}"
    );
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], json!("object"), "{t}");
        assert!(t["description"].as_str().unwrap().len() > 20, "{t}");
    }
    // agents_log on itself: live, answers rows of its own log.
    assert_eq!(step("call_log").0, 200);
    let log = &bodies["call_log"]["result"];
    assert_eq!(log["isError"], json!(false), "{log}");
    assert_eq!(log["structuredContent"]["agent"], json!(id));
    let kinds: Vec<&str> = log["structuredContent"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"user_message"), "the prompt is in the log: {kinds:?}");
    assert!(kinds.contains(&"turn_started"), "{kinds:?}");
    // agents_spawn: LIVE since R4.S2 — over the installed client's own wire it
    // ACKNOWLEDGES (rule 2.4) with the two addresses the act minted, and the
    // child is a thread this daemon owns.
    assert_eq!(step("call_spawn").0, 200);
    let spawn = &bodies["call_spawn"]["result"];
    assert_eq!(spawn["isError"], json!(false), "{spawn}");
    let ack = &spawn["structuredContent"];
    let mut keys: Vec<&str> = ack.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["changed", "promise_id", "run_id", "thread_id", "visible"], "{spawn}");
    let child = ack["thread_id"].as_str().unwrap();
    assert!(d.m.store.get_thread(child).unwrap().is_some(), "the spawned child is a thread the daemon owns");
    // budget_key: refused `unknown_param`, not ignored — before the act is even reached.
    let budget = &bodies["call_spawn_budget"]["result"];
    assert_eq!(budget["structuredContent"]["refused"]["code"], json!(codes::UNKNOWN_PARAM), "{budget}");

    // ── AND THE TOKEN IS VOID THE MOMENT ITS TURN ENDS ──────────────────
    let after = mcp(d.addr, Some(&token), json!({"jsonrpc":"2.0","id":9,"method":"ping"})).await;
    assert_eq!(after.status, 401, "{}", String::from_utf8_lossy(&after.body));
    let msg = after.json()["error"]["message"].as_str().unwrap().to_string();
    assert!(msg.contains("no turn is running on thread"), "{msg}");
    assert!(after.headers.contains_key("www-authenticate"), "{:?}", after.headers);
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. THE TOKEN: forged, tampered, other-thread, absent
// ═════════════════════════════════════════════════════════════════════════════

/// **A TOKEN MINTED WITH THE WRONG KEY IS REFUSED, AND SO ARE TAMPERED
/// CLAIMS** — before anything about turns is consulted.
///
/// **Forced red** by `Key::verify` skipping its compare: the forged call
/// answers `200` with a `ping` result instead of `401`.
#[tokio::test]
async fn a_token_minted_with_another_key_is_refused_and_so_is_a_tampered_one() {
    let d = Daemon::start().await;
    let id = d.thread();
    let claims = d.claims(&id, "any-cookie");
    let forged = Key::from_bytes([7; 32]).mint(&claims);
    let res = mcp(d.addr, Some(&forged), json!({"jsonrpc":"2.0","id":1,"method":"ping"})).await;
    assert_eq!(res.status, 401, "{}", String::from_utf8_lossy(&res.body));
    let body = res.json();
    let msg = body["error"]["message"].as_str().unwrap().to_string();
    assert_eq!(
        msg,
        "unauthorized: run token refused: bad signature: the token was not minted by this \
         daemon's key, or its claims were altered after minting"
    );
    assert_eq!(body["error"]["data"]["code"], json!(codes::UNAUTHORIZED));
    assert_eq!(body["error"]["code"], json!(-32001));
    assert_eq!(res.headers.get("www-authenticate").map(String::as_str), Some("Bearer realm=\"threads\""));

    // Tampered: a genuine token with its claims rewritten to name another thread.
    let genuine = d.mint(&id, "any-cookie");
    let mut other = claims.clone();
    other.thread_id = "someone-else".into();
    let parts: Vec<&str> = genuine.split('.').collect();
    let tampered = format!(
        "{}.{}.{}",
        parts[0],
        shapewire::token::hex_encode(&serde_json::to_vec(&other).unwrap()),
        parts[2]
    );
    let res = mcp(d.addr, Some(&tampered), json!({"jsonrpc":"2.0","id":2,"method":"ping"})).await;
    assert_eq!(res.status, 401);
    assert!(res.json()["error"]["message"].as_str().unwrap().contains("bad signature"));

    // Absent, and not a bearer.
    let res = mcp(d.addr, None, json!({"jsonrpc":"2.0","id":3,"method":"ping"})).await;
    assert_eq!(res.status, 401);
    assert!(res.json()["error"]["message"].as_str().unwrap().contains("missing Authorization"));
    let res = raw(
        d.addr,
        "POST",
        "/mcp",
        &[("Content-Type", "application/json"), ("Accept", "application/json, text/event-stream"), ("Authorization", "Basic abc")],
        br#"{"jsonrpc":"2.0","id":4,"method":"ping"}"#,
    )
    .await;
    assert_eq!(res.status, 401);
    assert!(res.json()["error"]["message"].as_str().unwrap().contains("must be `Bearer"));

    // And a genuine token for a thread with no running turn: the signature is
    // fine and the WINDOW refuses it.
    let res = mcp(d.addr, Some(&genuine), json!({"jsonrpc":"2.0","id":5,"method":"ping"})).await;
    assert_eq!(res.status, 401);
    let msg = res.json()["error"]["message"].as_str().unwrap().to_string();
    assert!(msg.contains(&format!("no turn is running on thread {id}")), "{msg}");
}

/// **A TOKEN FOR THREAD A CANNOT ACT AS THREAD B.** The caller is bound from
/// the token, so `agents_log {agent: B}` under A's token is A asking about B
/// — refused `no_route` (since R4.S3: A does not SEE B in any network, so
/// READS(A, B) is false) — and never B's rows. The positive sentinel is A
/// asking about A on the same token: rows.
///
/// **Forced red** by `agents::call` binding `caller` from the ARGUMENTS when
/// present instead of from the claims: the `{agent: B, caller: B}` call below
/// is answered with B's rows instead of refused (`malformed`), and the
/// `{agent: B}` call reads `no_route` naming A as the caller either way —
/// which is why both are asserted.
#[tokio::test]
async fn a_token_for_thread_a_cannot_act_as_thread_b() {
    let d = Daemon::start().await;
    let a = d.thread();
    let b = d.thread();
    let release = d.tmp.path().join("release-a");
    let cookie = d.running(&a, &format!("holdturn release={}", release.display())).await;
    let token = d.mint(&a, &cookie);

    // A about B: refused, typed, naming both ids.
    let res = mcp(d.addr, Some(&token), call(1, "agents_log", json!({"agent": b}))).await;
    assert_eq!(res.status, 200);
    let (code, msg) = refused(&res);
    assert_eq!(code, threads::networks::refusal::NO_ROUTE);
    assert!(msg.contains(&format!("you (`{a}`) may not read `{b}`'s log")), "{msg}");

    // A about A: answered.
    let res = mcp(d.addr, Some(&token), call(2, "agents_log", json!({"agent": a, "limit": 10}))).await;
    let r = res.json()["result"].clone();
    assert_eq!(r["isError"], json!(false), "{r}");
    assert_eq!(r["structuredContent"]["agent"], json!(a));
    assert!(r["structuredContent"]["rows"].as_array().unwrap().len() >= 2, "{r}");
    assert_eq!(r["structuredContent"]["window"], json!(threads::shapes::log::WINDOW));

    // A claiming to be B in the arguments: refused before anything is read.
    let res = mcp(d.addr, Some(&token), call(3, "agents_log", json!({"agent": b, CALLER: b}))).await;
    let (code, msg) = refused(&res);
    assert_eq!(code, codes::MALFORMED);
    assert!(msg.contains("the caller is the run token's thread"), "{msg}");

    std::fs::write(&release, b"go").unwrap();
    d.ended(&a).await;
}

// ═════════════════════════════════════════════════════════════════════════════
// 2b. THE ATTACHED WINDOW (SH-A3, docs/SHIMS.md §SH-A3)
// ═════════════════════════════════════════════════════════════════════════════

/// An `attached_sessions` row as SH-A2's attach verb writes it. Seeded
/// directly through SH-A1's store CRUD: SH-A2 (the minting verb) floats.
fn attached(cookie: &str, thread: &str) -> threads::store::AttachedSessionRow {
    threads::store::AttachedSessionRow {
        cookie: cookie.into(),
        thread_id: thread.into(),
        account: "aa-owned".into(),
        wrapper_pid: 4100,
        claude_pid: None,
        transcript_path: None,
        byte_offset: 0,
        started_at: "2026-10-21T00:00:00Z".into(),
    }
}

async fn ping(d: &Daemon, token: &str, id: u64) -> Res {
    mcp(d.addr, Some(token), json!({"jsonrpc":"2.0","id":id,"method":"ping"})).await
}

fn refusal_message(res: &Res) -> String {
    res.json()["error"]["message"].as_str().unwrap_or_default().to_string()
}

/// **A TOKEN WHOSE COOKIE MATCHES A LIVE ATTACHED ROW IS ACCEPTED, AND ONLY
/// WHILE THE ROW LIVES.** No turn runs on the thread at any point, so the
/// `running_turns` window cannot be what answers. Opposed: a token for the
/// same thread under a cookie no row holds is refused while the row is live
/// (so "accept any genuine token" fails), and the live token itself is
/// answered (so "refuse everything" fails). Detach deletes the row; the same
/// token is then refused — there is no `ended` column, a gone row IS ended.
///
/// **Forced red** by removing the `get_attached_session` lookup from
/// `verify`: the live-row ping reads `401` "no turn is running" instead of
/// `200`.
#[tokio::test]
async fn an_attached_sessions_cookie_opens_the_door_while_its_row_lives_and_not_after() {
    let d = Daemon::start().await;
    let a = d.thread();
    assert!(d.m.store.get_running_turn(&a).unwrap().is_none(), "no managed turn on the thread");
    d.m.store.insert_attached_session(&attached("att-live", &a)).unwrap();
    let token = d.mint(&a, "att-live");

    let res = ping(&d, &token, 1).await;
    assert_eq!(res.status, 200, "live attached row: {}", String::from_utf8_lossy(&res.body));
    assert_eq!(res.json()["result"], json!({}));
    // A real tool, bound to the token's thread, answers too.
    let res = mcp(d.addr, Some(&token), call(2, "agents_log", json!({"agent": a, "limit": 10}))).await;
    assert_eq!(res.json()["result"]["isError"], json!(false), "{}", res.json());
    assert_eq!(res.json()["result"]["structuredContent"]["agent"], json!(a));

    // Opposing: a genuine token on the same thread whose cookie no row holds.
    let stray = d.mint(&a, "att-nobody");
    let res = ping(&d, &stray, 3).await;
    assert_eq!(res.status, 401, "{}", String::from_utf8_lossy(&res.body));
    assert!(refusal_message(&res).contains(&format!("no turn is running on thread {a}")), "{}", refusal_message(&res));

    // Detach: the row is gone, and so is the window.
    assert!(d.m.store.delete_attached_session("att-live").unwrap());
    let res = ping(&d, &token, 4).await;
    assert_eq!(res.status, 401, "deleted row: {}", String::from_utf8_lossy(&res.body));
    assert!(refusal_message(&res).contains(&format!("no turn is running on thread {a}")), "{}", refusal_message(&res));
}

/// **THREAD A'S ATTACHED COOKIE PRESENTED FOR THREAD B IS REFUSED.** The
/// token is genuine (this daemon's key) and its cookie names a LIVE row — on
/// thread A. Minted for B, it must not open B's door. Positive sentinel on the
/// same row: the token minted for A is answered, so the refusal is about the
/// thread, not the cookie.
///
/// **Forced red** by dropping the `row.thread_id == claims.thread_id` guard
/// (accepting any live row by cookie): the B-token ping reads `200`.
#[tokio::test]
async fn an_attached_cookie_of_thread_a_does_not_open_thread_b() {
    let d = Daemon::start().await;
    let a = d.thread();
    let b = d.thread();
    d.m.store.insert_attached_session(&attached("att-a", &a)).unwrap();

    let res = ping(&d, &d.mint(&a, "att-a"), 1).await;
    assert_eq!(res.status, 200, "sentinel, A's own token: {}", String::from_utf8_lossy(&res.body));

    let res = ping(&d, &d.mint(&b, "att-a"), 2).await;
    assert_eq!(res.status, 401, "A's cookie for B: {}", String::from_utf8_lossy(&res.body));
    let msg = refusal_message(&res);
    assert!(msg.contains(&format!("attached session on another thread, not thread {b}")), "{msg}");
    assert_eq!(res.json()["error"]["data"]["code"], json!(codes::UNAUTHORIZED));
}

/// **A MANAGED TURN'S TOKEN IS ACCEPTED EXACTLY AS BEFORE SH-A3**, with
/// attached rows present in the table (one on another thread, one on this
/// thread under another cookie), and refused once its turn ends — the
/// `running_turns` window is unchanged in both directions.
///
/// **Forced red** by making `verify` consult ONLY `attached_sessions`
/// (the `running_turns` arm skipped): the running-turn ping reads `401`.
#[tokio::test]
async fn a_managed_turns_token_is_still_accepted_with_attached_rows_present() {
    let d = Daemon::start().await;
    let a = d.thread();
    let b = d.thread();
    d.m.store.insert_attached_session(&attached("att-b", &b)).unwrap();
    d.m.store.insert_attached_session(&attached("att-a-other", &a)).unwrap();
    let release = d.tmp.path().join("release-managed");
    let cookie = d.running(&a, &format!("holdturn release={}", release.display())).await;
    assert_ne!(cookie, "att-a-other");
    let token = d.mint(&a, &cookie);

    let res = ping(&d, &token, 1).await;
    assert_eq!(res.status, 200, "running turn: {}", String::from_utf8_lossy(&res.body));
    assert_eq!(res.json()["result"], json!({}));

    std::fs::write(&release, b"go").unwrap();
    d.ended(&a).await;
    let res = ping(&d, &token, 2).await;
    assert_eq!(res.status, 401, "ended turn: {}", String::from_utf8_lossy(&res.body));
    assert!(refusal_message(&res).contains(&format!("no turn is running on thread {a}")), "{}", refusal_message(&res));
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. THE FAÇADE RULE: the subject is spelled once, on both carriers
// ═════════════════════════════════════════════════════════════════════════════

/// **THE SUBJECT RIDES THE PATH, ONCE.** Over `/ws`, `agents/<a>/log` with
/// `agent` also in the params is refused by the chassis's `bind_subject`,
/// unchanged. Over `/mcp`, the agent sends `agent` as a parameter and the
/// façade moves it into the path — so the same request that is two spellings
/// on `/ws` is the only spelling on `/mcp`, and `bind_subject` sees one.
///
/// **Forced red** by `agents::bind_path` reading the subject without
/// `remove`-ing it (`args.get(subject)`): the `/mcp` call is refused
/// `malformed` with *"`agent` is the SUBJECT of `agents/{agent}/log` and rides
/// the path, not the params"* — the chassis's own refusal firing on the
/// façade, which is exactly the hazard the rule exists to make impossible and
/// exactly how it would be caught.
#[tokio::test]
async fn the_subject_is_spelled_once_on_both_carriers() {
    let d = Daemon::start().await;
    let a = d.thread();
    let release = d.tmp.path().join("release-a");
    let cookie = d.running(&a, &format!("holdturn release={}", release.display())).await;
    let token = d.mint(&a, &cookie);

    // /ws: two spellings, refused by the chassis.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws", d.addr)).await.unwrap();
    let _welcome = ws.next().await.unwrap().unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({"type":"request","id":"1","method":format!("agents/{a}/log"),"params":{"agent": a, CALLER: a}}).to_string(),
    ))
    .await
    .unwrap();
    let reply: Value = serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(reply["outcome"]["code"], json!(codes::MALFORMED), "{reply}");
    assert!(
        reply["outcome"]["error"].as_str().unwrap().contains("is the SUBJECT of `agents/{agent}/log`"),
        "{reply}"
    );
    // /ws: one spelling (the path), with the caller stated — the operator's
    // door answers.
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({"type":"request","id":"2","method":format!("agents/{a}/log"),"params":{CALLER: a, "limit": 5}}).to_string(),
    ))
    .await
    .unwrap();
    let reply: Value = serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(reply["outcome"]["type"], json!("success"), "{reply}");
    assert_eq!(reply["outcome"]["payload"]["agent"], json!(a));

    // /mcp: the agent sends `agent` as a param; the façade is the sole writer
    // of the path, and the chassis sees one spelling.
    let res = mcp(d.addr, Some(&token), call(1, "agents_log", json!({"agent": a, "limit": 5}))).await;
    assert_eq!(res.json()["result"]["isError"], json!(false), "{}", res.json());

    // The façade's naming rule is mechanical, and every template is in the inventory.
    for t in SURFACE {
        assert_eq!(t.name, Tool::mcp_name_of(t.template), "{}", t.template);
        assert!(WS_METHODS.contains(&t.template), "{} is not pinned", t.template);
    }

    std::fs::write(&release, b"go").unwrap();
    d.ended(&a).await;
}

// ═════════════════════════════════════════════════════════════════════════════
// 4. COMMANDS ACKNOWLEDGE — the guard
// ═════════════════════════════════════════════════════════════════════════════

/// **EVERY COMMAND ANSWERS `Accepted{promise_id, visible, changed?}` AND
/// NOTHING ELSE — and the façade refuses a command that answered a body.**
///
/// The guard is exercised directly, for every command whether or not its arm
/// is live on this tree: a command tool handed an acknowledgement passes, handed
/// a body is refused with the stray keys named, and `agents_spawn`'s two
/// minted addresses are the only extras allowed. A read passes anything.
///
/// **Forced red** by `acknowledgement_only` returning `Ok(reply)` for every
/// kind: the `{"rows": []}` assertion fails with `Ok(Object {"rows": …})`.
/// And the guard sits on the live path — `agents::call` runs it after
/// `handle` — which `the_guard_is_on_the_live_path` below pins by driving a
/// command through the real façade and reading the refusal's text.
#[test]
fn every_command_tool_acknowledges_and_the_guard_refuses_a_body() {
    let ack = json!({"promise_id": 7, "visible": true});
    let ack_changed = json!({"promise_id": 7, "visible": false, "changed": true});
    let body = json!({"rows": [], "count": 3});
    let mut commands = 0;
    for t in SURFACE {
        match t.kind {
            Kind::Command => {
                commands += 1;
                assert_eq!(agents::acknowledgement_only(t, ack.clone()).unwrap(), ack, "{}", t.name);
                assert_eq!(agents::acknowledgement_only(t, ack_changed.clone()).unwrap(), ack_changed);
                let e = agents::acknowledgement_only(t, body.clone()).unwrap_err();
                assert_eq!(e.code, codes::ERROR);
                assert!(
                    e.message.contains(&format!("façade guard: `{}` is a COMMAND and its handler answered keys [\"count\", \"rows\"]", t.name)),
                    "{}",
                    e.message
                );
                let e = agents::acknowledgement_only(t, json!({"visible": true})).unwrap_err();
                assert!(e.message.contains("no `promise_id`/`visible`"), "{}", e.message);
                let e = agents::acknowledgement_only(t, json!("ok")).unwrap_err();
                assert!(e.message.contains("a non-object"), "{}", e.message);
                // The minted addresses: spawn's two, and network_create's one
                // (R4.S3), each on its own tool only.
                let with = json!({"promise_id": 1, "visible": true, "thread_id": "t", "run_id": "r"});
                if t.name == "agents_spawn" {
                    assert!(agents::acknowledgement_only(t, with.clone()).is_ok());
                } else {
                    assert!(agents::acknowledgement_only(t, with.clone()).is_err(), "{}", t.name);
                }
                // …and `network` on spawn too since R4.S8 (the network a spawn mints).
                let net = json!({"promise_id": 1, "visible": true, "network": "n"});
                if t.name == "agents_network_create" || t.name == "agents_spawn" {
                    assert!(agents::acknowledgement_only(t, net.clone()).is_ok());
                } else {
                    assert!(agents::acknowledgement_only(t, net.clone()).is_err(), "{}", t.name);
                }
            }
            Kind::Read => {
                assert_eq!(agents::acknowledgement_only(t, body.clone()).unwrap(), body, "{}", t.name);
            }
        }
    }
    assert_eq!(commands, 13, "§2.2's eight commands, less `network_join`'s old meaning, plus agent-networks §4.2's admit, join, leave, remove, configure, plus R10.M3's `agents_policy`");
}

/// A command driven through the real façade with a handler that answered a
/// body is refused at the façade — proven by the only lever this tree has: a
/// read tool's reply is a body, and classifying it as a command for one call
/// refuses it. The façade's guard is called with the tool's own kind, so this
/// is asserted on `acknowledgement_only` over a REAL reply from `handle`.
#[tokio::test]
async fn the_guard_is_on_the_live_path() {
    let d = Daemon::start().await;
    let a = d.thread();
    let claims = d.claims(&a, "c");
    // The live path, as the door runs it: a read answers.
    let reply = agents::call(&d.m, &claims, "agents_log", json!({"agent": a})).await.unwrap();
    assert_eq!(reply["agent"], json!(a));
    // The same reply, held to a command's rule, is what the guard refuses.
    let as_command = Tool { kind: Kind::Command, ..*agents::tool("agents_log").unwrap() };
    let e = agents::acknowledgement_only(&as_command, reply).unwrap_err();
    assert!(e.message.contains("answered keys"), "{}", e.message);
    // And every command on the surface whose act is not on this tree, through
    // the real façade, is refused typed BEFORE the guard — naming its path.
    // (The network commands are live since R4.S3; `tests/networks.rs` drives
    // them.)
    for t in SURFACE.iter().filter(|t| t.kind == Kind::Command && !t.live) {
        let mut args = serde_json::Map::new();
        for p in t.params.iter().filter(|p| p.required) {
            let v: Value = match p.name {
                "runs" | "members" => json!(["x"]),
                "agents" => json!([{"task": "t"}]),
                "scope" => json!("one"),
                "action" => json!("wake"),
                _ => json!("x"),
            };
            args.insert(p.name.into(), v);
        }
        let e = agents::call(&d.m, &claims, t.name, Value::Object(args)).await.unwrap_err();
        assert_eq!(e.code, codes::NOT_AVAILABLE_YET, "{}: {}", t.name, e.message);
        assert!(e.message.contains(&format!("`{}`", t.template)), "{}: {}", t.name, e.message);
    }
    // …and the LIVE command (R4.S2) passes the guard on the live path: its
    // real reply is an acknowledgement plus the two minted addresses.
    let ack = agents::call(&d.m, &claims, "agents_spawn", json!({"task": "t"})).await.unwrap();
    assert!(ack["promise_id"].is_u64() && ack["visible"].is_boolean(), "{ack}");
}

// ═════════════════════════════════════════════════════════════════════════════
// 5. THE SURFACE IS EXACTLY §2
// ═════════════════════════════════════════════════════════════════════════════

/// Eighteen tools (R4.S1's twelve, with agent-networks §4.2's admit, leave,
/// remove, configure and reach added by R4.S3, and R10.M3's `agents_policy`), their kinds, their required params, their subjects, and a
/// schema that parses for each — pinned against the design's two tables.
#[test]
fn the_surface_is_exactly_section_2() {
    let expected: &[(&str, Kind, &[&str], Option<&str>, bool)] = &[
        ("agents_spawn", Kind::Command, &["task"], None, true),
        ("agents_spawn_batch", Kind::Command, &["agents"], None, false),
        ("agents_message", Kind::Command, &["agent", "body"], Some("agent"), true),
        ("agents_policy", Kind::Command, &["agent", "notify"], Some("agent"), true),
        ("agents_broadcast", Kind::Command, &["network", "body"], Some("network"), true),
        ("agents_interrupt", Kind::Command, &["agent", "scope"], Some("agent"), false),
        ("agents_watch", Kind::Command, &["runs", "until", "action"], None, false),
        ("agents_network_create", Kind::Command, &["name"], None, true),
        ("agents_network_admit", Kind::Command, &["network", "members"], Some("network"), true),
        ("agents_network_join", Kind::Command, &["network"], Some("network"), true),
        ("agents_network_leave", Kind::Command, &["network"], Some("network"), true),
        ("agents_network_remove", Kind::Command, &["network", "members"], Some("network"), true),
        ("agents_network_configure", Kind::Command, &["network"], Some("network"), true),
        ("agents_list", Kind::Read, &[], None, true),
        ("agents_reach", Kind::Read, &[], None, true),
        ("agents_result", Kind::Read, &["agent"], Some("agent"), true),
        ("agents_gather", Kind::Read, &["runs", "until"], None, true),
        ("agents_log", Kind::Read, &["agent"], Some("agent"), true),
    ];
    assert_eq!(SURFACE.len(), expected.len());
    for (t, (name, kind, required, subject, live)) in SURFACE.iter().zip(expected) {
        assert_eq!(t.name, *name);
        assert_eq!(t.kind, *kind, "{name}");
        assert_eq!(t.live, *live, "{name}");
        assert_eq!(t.subject(), *subject, "{name}");
        let got: Vec<&str> = t.params.iter().filter(|p| p.required).map(|p| p.name).collect();
        assert_eq!(got, *required, "{name}");
        let schema = t.input_schema();
        assert_eq!(schema["additionalProperties"], json!(false));
        let req: Vec<&str> = schema["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(req, *required, "{name}");
        for p in t.params {
            assert!(schema["properties"].get(p.name).is_some(), "{name}.{}", p.name);
        }
        assert_eq!(agents::tool(name).unwrap().template, t.template);
    }
    // The batch's items are spawn's inputs, byte for byte on the properties.
    let spawn = agents::tool("agents_spawn").unwrap().input_schema();
    let batch = agents::tool("agents_spawn_batch").unwrap().input_schema();
    assert_eq!(batch["properties"]["agents"]["items"]["properties"], spawn["properties"]);
    assert_eq!(batch["properties"]["agents"]["items"]["required"], spawn["required"]);
    // `budget_key` is on NO spawn input: §1.4, a child inherits its parent's key.
    assert!(spawn["properties"].get("budget_key").is_none());
    // R10.M3 (`MESSAGING-ORCHESTRATION.md` §8): `notify` is on spawn, the
    // batch's items, message and policy — one schema, so the four cannot
    // drift; the live words are direct | hold. R10.M3b (Q-23 option B):
    // `wake` is back on spawn and the batch's items ONLY, DEPRECATED for one
    // release (DEPRECATED(wake, remove after DEPLOY-9n)) — end | never, and
    // its description says it will be refused.
    let wake = &spawn["properties"]["wake"];
    assert_eq!(wake["enum"], json!(["end", "never"]), "{wake}");
    assert!(wake["description"].as_str().is_some_and(|d| d.contains("DEPRECATED") && d.contains("refused next release")), "{wake}");
    let notify = &spawn["properties"]["notify"];
    assert_eq!(notify["oneOf"][0]["enum"], json!(["direct", "hold"]), "{notify}");
    for t in ["agents_message", "agents_policy"] {
        assert_eq!(&agents::tool(t).unwrap().input_schema()["properties"]["notify"], notify, "{t}");
    }
    assert_eq!(
        SURFACE.iter().filter(|t| t.input_schema()["properties"].get("wake").is_some()).map(|t| t.name).collect::<Vec<_>>(),
        vec!["agents_spawn"],
        "`wake` (deprecated) is on agents_spawn alone"
    );
    // The live tools on this tree: R4.S1's log, R4.S2's managed runtime (spawn/list/result),
    // and R4.S3's network acts.
    assert_eq!(
        SURFACE.iter().filter(|t| t.live).map(|t| t.name).collect::<Vec<_>>(),
        vec![
            "agents_spawn",
            "agents_message",
            "agents_policy",
            "agents_broadcast",
            "agents_network_create",
            "agents_network_admit",
            "agents_network_join",
            "agents_network_leave",
            "agents_network_remove",
            "agents_network_configure",
            "agents_list",
            "agents_reach",
            "agents_result",
            "agents_gather",
            "agents_log",
        ]
    );
}

/// **AN UNKNOWN PARAMETER IS REFUSED, NEVER IGNORED** (§1.4), before the act
/// is reached — so a spawn carrying `budget_key` is `unknown_param` on a tree
/// where spawn itself is `not_available_yet`.
///
/// **Forced red** by `check_params` dropping its unknown-name loop: the
/// `budget_key` call reads `not_available_yet` (the act was reached).
#[tokio::test]
async fn an_unknown_param_is_refused_before_the_act_and_a_missing_one_is_named() {
    let d = Daemon::start().await;
    let a = d.thread();
    let claims = d.claims(&a, "c");
    let e = agents::call(&d.m, &claims, "agents_spawn", json!({"task": "t", "budget_key": "k"})).await.unwrap_err();
    assert_eq!(e.code, codes::UNKNOWN_PARAM);
    assert!(e.message.starts_with("`agents_spawn` does not take `budget_key`; it takes `task`*, `agent`"), "{}", e.message);
    let e = agents::call(&d.m, &claims, "agents_message", json!({"agent": "x"})).await.unwrap_err();
    assert_eq!(e.code, codes::MALFORMED);
    assert_eq!(e.message, "missing required param: body (`agents_message`)");
    let e = agents::call(&d.m, &claims, "agents_log", json!({})).await.unwrap_err();
    assert_eq!(e.code, codes::MALFORMED);
    assert!(e.message.contains("missing required param: agent"), "{}", e.message);
    let e = agents::call(&d.m, &claims, "agents_log", json!({"agent": "a/b"})).await.unwrap_err();
    assert_eq!(e.code, codes::MALFORMED, "{}", e.message);
    let e = agents::call(&d.m, &claims, "agents_log", json!({"agent": a, "limit": 0})).await.unwrap_err();
    assert_eq!(e.code, codes::MALFORMED);
    assert!(e.message.contains("1..=4000"), "{}", e.message);
    let e = agents::call(&d.m, &claims, "agents_log", json!({"agent": a, "limit": 4001})).await.unwrap_err();
    assert_eq!(e.code, codes::MALFORMED);
    let e = agents::call(&d.m, &claims, "nope", json!({})).await.unwrap_err();
    assert_eq!(e.code, codes::UNKNOWN_METHOD);
    let e = agents::call(&d.m, &claims, "agents_log", json!("not an object")).await.unwrap_err();
    assert_eq!(e.code, codes::MALFORMED);
    // Paging: from_seq and next_seq agree, and the page is the shape's rows.
    let page = agents::call(&d.m, &claims, "agents_log", json!({"agent": a, "limit": 1})).await.unwrap();
    assert_eq!(page["rows"].as_array().unwrap().len(), 1);
    assert_eq!(page["next_seq"], json!(page["rows"][0]["seq"].as_u64().unwrap() + 1));
    let next = agents::call(&d.m, &claims, "agents_log", json!({"agent": a, "from_seq": page["next_seq"], "limit": 1})).await.unwrap();
    assert_eq!(next["from_seq"], page["next_seq"]);
    // An unknown thread, from a caller that IS that thread (so the self-scope
    // passes): `unknown`.
    let ghost = d.claims("no-such-thread", "c");
    let e = agents::call(&d.m, &ghost, "agents_log", json!({"agent": "no-such-thread"})).await.unwrap_err();
    assert_eq!(e.code, codes::UNKNOWN);
}

// ═════════════════════════════════════════════════════════════════════════════
// 6. THE DOOR'S TRANSPORT RULES
// ═════════════════════════════════════════════════════════════════════════════

/// GET and DELETE are 405 with `Allow: POST`; a missing `Accept` is 406; a
/// non-JSON body type is 415; an unsupported protocol header is 400; a batch
/// is 400 — each before the token is read, and each a JSON-RPC error body.
#[tokio::test]
async fn the_door_refuses_what_the_reference_server_refuses_and_offers_no_stream() {
    let d = Daemon::start().await;
    for m in ["GET", "DELETE"] {
        let res = raw(d.addr, m, "/mcp", &[("Accept", "text/event-stream")], b"").await;
        assert_eq!(res.status, 405, "{m}");
        assert_eq!(res.headers.get("allow").map(String::as_str), Some("POST"));
        assert_eq!(res.json()["error"]["code"], json!(-32000));
    }
    let res = raw(d.addr, "POST", "/mcp", &[("Content-Type", "application/json")], b"{}").await;
    assert_eq!(res.status, 406);
    assert!(res.json()["error"]["message"].as_str().unwrap().contains("must accept both"));
    let res = raw(
        d.addr,
        "POST",
        "/mcp",
        &[("Content-Type", "text/plain"), ("Accept", "application/json, text/event-stream")],
        b"{}",
    )
    .await;
    assert_eq!(res.status, 415);
    let res = raw(
        d.addr,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("mcp-protocol-version", "1999-01-01"),
        ],
        b"{}",
    )
    .await;
    assert_eq!(res.status, 400);
    assert!(res.json()["error"]["message"].as_str().unwrap().contains("supported versions: 2025-06-18, 2025-03-26"));
    // A batch, with a valid token for a running turn, is still refused.
    let a = d.thread();
    let release = d.tmp.path().join("release-a");
    let cookie = d.running(&a, &format!("holdturn release={}", release.display())).await;
    let token = d.mint(&a, &cookie);
    let res = mcp(d.addr, Some(&token), json!([{"jsonrpc":"2.0","id":1,"method":"ping"}])).await;
    assert_eq!(res.status, 400, "{}", String::from_utf8_lossy(&res.body));
    assert!(res.json()["error"]["message"].as_str().unwrap().contains("batching"));
    // And a method this door does not answer is -32601, correlated.
    let res = mcp(d.addr, Some(&token), json!({"jsonrpc":"2.0","id":"x","method":"resources/list"})).await;
    assert_eq!(res.status, 200);
    assert_eq!(res.json()["error"]["code"], json!(-32601));
    assert_eq!(res.json()["id"], json!("x"));
    // The health route is untouched beside it.
    let res = raw(d.addr, "GET", "/health", &[], b"").await;
    assert_eq!(res.status, 200);
    std::fs::write(&release, b"go").unwrap();
    d.ended(&a).await;
}

/// **A PRODUCER WITH NO TOOLS ANSWERS `unsupported` ON THE DOOR** — mounted on
/// every chassis, like the resource door, so the answer is reachable.
#[tokio::test]
async fn a_producer_with_no_tools_answers_unsupported_on_the_door() {
    use shapewire::envelope::Identity;
    use shapewire::hub::{Hub, Kind as HubKind};
    use shapewire::server::{NoControl, NoStream, Producer};
    #[derive(Clone)]
    struct Nothing;
    impl HubKind for Nothing {
        fn kind(&self) -> &'static str {
            "nothing"
        }
    }
    struct Bare {
        identity: Identity,
        hub: Hub<Nothing>,
    }
    impl Producer for Bare {
        type Input = Nothing;
        type Stream = NoStream;
        type Control = NoControl;
        fn identity(&self) -> &Identity {
            &self.identity
        }
        fn hub(&self) -> &Hub<Nothing> {
            &self.hub
        }
        fn names(&self) -> &'static [&'static str] {
            &[]
        }
        fn handle<'a>(
            &'a self,
            _m: &'a str,
            _p: Value,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, shapewire::envelope::Refusal>> + Send + 'a>> {
            Box::pin(async { Err(shapewire::envelope::Refusal::untyped("none")) })
        }
        fn connect(&self, _c: shapewire::wire::ConnId) -> (NoStream, NoControl) {
            (NoStream, NoControl)
        }
    }
    let bare = Arc::new(Bare {
        identity: Identity { daemon: "bare".into(), version: "0".into(), proto: 1, boot_id: "b".into() },
        hub: Hub::new(),
    });
    let (addr, h) = shapewire::server::serve(bare, "127.0.0.1:0".parse().unwrap()).await.unwrap();
    std::mem::forget(h);
    let res = mcp(addr, Some("rt1.aa.bb"), json!({"jsonrpc":"2.0","id":1,"method":"ping"})).await;
    assert_eq!(res.status, 501, "{}", String::from_utf8_lossy(&res.body));
    assert!(res.json()["error"]["message"].as_str().unwrap().starts_with("unsupported: this producer exposes no tools on /mcp"));
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
