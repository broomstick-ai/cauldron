//! # A turn the daemon runs can reach managed execution — R4.S5
//!
//! The release sentence (R-5): *"a subagent is a thread the daemon owns, run by
//! managed execution over the network layer"*. Until R4.S5 no production code
//! minted a run token or handed `claude` an MCP config (the R-5 inspector's
//! F1), so every `agents_*` call a real turn could make was `UNAUTHORIZED`.
//!
//! These tests prove the spawn side: for every turn, the daemon puts a run
//! token in the child's environment (`THREADS_RUN_TOKEN`) and `--mcp-config
//! <file>` on its argv, the file naming THIS daemon's `/mcp` on the address the
//! chassis actually bound, with the same token as its bearer.
//!
//! ## A probe must not witness itself
//!
//! **Nothing in this file mints a token.** The token is read back out of the
//! environment of a process the DAEMON spawned (`fake-claude-capture` records
//! it), and the URL out of the config file the daemon wrote — then the test
//! dials that URL with that token. The door's acceptance is therefore evidence
//! about what the spawn path produced, not about what the test made.
//!
//! ## Addresses
//!
//! Every daemon here binds `127.0.0.1:0` through `threads::chassis::serve`, so
//! the port the turn is handed must be the ephemeral one — a configured or
//! hard-coded port (3653 is the default) fails the URL assertion.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use threads::config::Config;
use threads::{build_manager, ThreadManager};

/// The env is process-wide and each test points it at its own tmp dir.
static SERIAL: Mutex<()> = Mutex::new(());

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude-capture"))
        .to_string_lossy()
        .to_string()
}

fn set_env(tmp: &Path) {
    std::env::set_var("CAULDRON_HOME", tmp.join("cauldron"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    // A token in the TEST process's own environment must never reach a turn.
    std::env::set_var("THREADS_RUN_TOKEN", "inherited-from-the-daemons-env");
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
    tmp: tempfile::TempDir,
}

impl Daemon {
    async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        set_env(tmp.path());
        threads::clock::testing::ticking();
        let m = build_manager(config_at(tmp.path())).unwrap();
        let (addr, h) = threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        std::mem::forget(h);
        Self { m, addr, tmp }
    }

    fn thread(&self) -> String {
        let settings = json!({
            "cwd": self.tmp.path().to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
        });
        self.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    }

    /// Start a capturing turn on `id` and wait until the spawned process has
    /// recorded what it was handed. Returns the capture and the release path.
    async fn capturing_turn(&self, id: &str, label: &str) -> (Capture, PathBuf) {
        let cap = self.tmp.path().join(format!("cap-{label}"));
        let rel = self.tmp.path().join(format!("release-{label}"));
        self.m
            .send(
                id,
                format!("capture={} release={}", cap.display(), rel.display()),
                threads::SendIntent::Immediate,
                Value::Null,
                false,
            )
            .unwrap();
        let done = PathBuf::from(format!("{}.done", cap.display()));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !done.exists() {
            assert!(tokio::time::Instant::now() < deadline, "the turn never ran the fixture");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        (Capture::read(&cap), rel)
    }

    async fn ended(&self, id: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while self.m.store.get_running_turn(id).unwrap().is_some() {
            assert!(tokio::time::Instant::now() < deadline, "the turn never ended");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// What the daemon handed one spawned turn, as that process recorded it.
struct Capture {
    argv: Vec<String>,
    /// `THREADS_RUN_TOKEN` as the child saw it; `None` when unset.
    token: Option<String>,
    /// The path after `--mcp-config`, if any.
    mcp_path: Option<String>,
    /// That file's contents, copied while the turn ran.
    mcp: Option<Value>,
}

impl Capture {
    fn read(cap: &Path) -> Self {
        let p = |ext: &str| PathBuf::from(format!("{}.{ext}", cap.display()));
        let argv: Vec<String> = std::fs::read_to_string(p("argv"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        let token = std::fs::read_to_string(p("token")).ok();
        let mcp_path = argv
            .iter()
            .position(|a| a == "--mcp-config")
            .and_then(|i| argv.get(i + 1).cloned());
        let mcp = std::fs::read_to_string(p("mcp"))
            .ok()
            .map(|s| serde_json::from_str(&s).expect("the --mcp-config file is JSON"));
        Self { argv, token, mcp_path, mcp }
    }

    /// The token the child holds, asserted present and not the one in the
    /// daemon's own environment.
    fn token(&self) -> String {
        let t = self.token.clone().expect("THREADS_RUN_TOKEN reached the spawned turn");
        assert_ne!(t, "inherited-from-the-daemons-env", "the daemon's own env leaked into the turn");
        assert!(t.starts_with("rt1."), "a run token: {t}");
        t
    }

    /// `(url, Authorization header)` of the `threads` server in the config.
    fn server(&self) -> (String, String) {
        let path = self.mcp_path.as_ref().unwrap_or_else(|| {
            panic!("--mcp-config is on the spawned turn's argv: {:?}", self.argv)
        });
        let cfg = self.mcp.as_ref().unwrap_or_else(|| panic!("the file {path} existed while the turn ran"));
        let s = &cfg["mcpServers"]["threads"];
        assert_eq!(s["type"], json!("http"), "{cfg}");
        (
            s["url"].as_str().unwrap_or_else(|| panic!("a url: {cfg}")).to_string(),
            s["headers"]["Authorization"].as_str().unwrap_or_else(|| panic!("a bearer: {cfg}")).to_string(),
        )
    }
}

// ── a hand-rolled HTTP/1.1 client (the one `tests/mcp_door.rs` uses) ───────

struct Res {
    status: u16,
    body: Vec<u8>,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!("body is not JSON ({e}): {:?}", String::from_utf8_lossy(&self.body))
        })
    }
}

/// POST one JSON-RPC body to `url` (which must be `http://host:port/path`)
/// with `Authorization: Bearer <token>`.
async fn mcp(url: &str, token: &str, body: Value) -> Res {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.strip_prefix("http://").expect("an http URL");
    let (hostport, path) = rest.split_once('/').expect("a path");
    let addr: SocketAddr = hostport.parse().expect("host:port");
    let body = body.to_string();
    let mut s = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr))
        .await
        .expect("connect did not hang")
        .unwrap_or_else(|e| panic!("connecting to {addr}: {e}"));
    let head = format!(
        "POST /{path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nAuthorization: Bearer {token}\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(body.as_bytes()).await.unwrap();
    s.flush().await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut buf))
        .await
        .expect("the response did not finish")
        .unwrap();
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").expect("headers terminate");
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let status: u16 = head.lines().next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
    Res { status, body: buf[split + 4..].to_vec() }
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}

// ═════════════════════════════════════════════════════════════════════════════

/// **A TURN THE DAEMON SPAWNS HOLDS A TOKEN `/mcp` ACCEPTS, AND ITS ARGV NAMES
/// THE DAEMON'S BOUND `/mcp`** — and through that door the turn spawns a
/// managed child that the daemon owns under it.
///
/// Paired negative (doctrine 2): the SAME token, once its turn has ended, is
/// refused `401`; and a token from turn 1 is refused while turn 2 runs on the
/// same thread, whose own daemon-minted token is accepted. So neither "the
/// door accepts everything" nor "the door refuses everything" passes.
///
/// **Forced red** (pasted in the R4.S5 report): (1) `env()` no longer pushes
/// `THREADS_RUN_TOKEN` → `token()` panics (the inherited test-env value, or
/// none); (2) `build_args` no longer pushes `--mcp-config` → `server()` panics;
/// (3) `mcp_url` hands out `port + 1` → the URL assertion fails; (4)
/// `ThreadsProducer::verify` skips the `running_turns` cookie check → the
/// finished turn's token is answered `200`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spawned_turn_carries_a_token_and_the_bound_mcp_url_and_spawns_a_managed_child() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let d = Daemon::start().await;
    let id = d.thread();
    let (cap, release) = d.capturing_turn(&id, "one").await;

    // The token is in the child's environment, and is the config's bearer.
    let token = cap.token();
    let (url, auth) = cap.server();
    assert_eq!(url, format!("http://{}/mcp", d.addr), "the turn is handed the chassis's BOUND /mcp address");
    assert_eq!(auth, format!("Bearer {token}"), "the config's bearer is the env's token");
    // The config file lives under the daemon's own data dir, per turn.
    let mcp_path = cap.mcp_path.clone().unwrap();
    let turns_dir = d.tmp.path().join("turns").join(&id);
    assert!(
        Path::new(&mcp_path).starts_with(&turns_dir),
        "{mcp_path} is under {}",
        turns_dir.display()
    );

    // The door accepts it, while the turn runs.
    let list = mcp(&url, &token, json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}})).await;
    assert_eq!(list.status, 200, "{}", String::from_utf8_lossy(&list.body));
    let names: Vec<String> = list.json()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.iter().any(|n| n == "agents_spawn"), "{names:?}");

    // …and through it the turn spawns a managed child, owned by this thread.
    let spawned = mcp(&url, &token, call(2, "agents_spawn", json!({"task": "a child's task", "request_id": "r4s5-1"}))).await;
    assert_eq!(spawned.status, 200);
    let sc = &spawned.json()["result"];
    assert_eq!(sc["isError"], json!(false), "{sc}");
    let child = sc["structuredContent"]["thread_id"].as_str().unwrap_or_else(|| panic!("{sc}")).to_string();
    assert_ne!(child, id);
    let listed = mcp(&url, &token, call(3, "agents_list", json!({}))).await;
    let rows = listed.json()["result"]["structuredContent"].to_string();
    assert!(rows.contains(&child), "the child is in the caller's subtree: {rows}");
    // The daemon wrote the child's managed ledger, naming the parent.
    let ledger = std::fs::read_to_string(d.tmp.path().join("ledgers").join(format!("{child}.jsonl")))
        .expect("the daemon wrote the child's managed ledger");
    assert!(ledger.contains(&id), "the child's ledger names its parent: {ledger}");

    // Turn ends: the config file goes, and the token is refused.
    std::fs::write(&release, b"").unwrap();
    d.ended(&id).await;
    // R9.F2: `ended` watches the `running_turns` row, and `run_once` clears
    // that row BEFORE `process_exited`, the `turn_ended` emit and the reconcile
    // — the config file goes when `_mcp_file` drops at the END of `run_once`.
    // Under suite load that gap is long enough for an immediate `exists()` to
    // see the file (red in the full run, green alone). The contract is "gone
    // once the turn has ended", so wait for it with the same 30 s `ended` gives.
    let gone_by = tokio::time::Instant::now() + Duration::from_secs(30);
    while Path::new(&mcp_path).exists() && tokio::time::Instant::now() < gone_by {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!Path::new(&mcp_path).exists(), "the per-turn --mcp-config is removed at turn end");
    let after = mcp(&url, &token, json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{}})).await;
    assert_eq!(after.status, 401, "a finished turn's token is refused: {}", String::from_utf8_lossy(&after.body));

    // Turn 2 on the same thread: a NEW token, accepted; turn 1's still refused.
    let (cap2, release2) = d.capturing_turn(&id, "two").await;
    let token2 = cap2.token();
    assert_ne!(token2, token, "every turn is minted its own token");
    let ok2 = mcp(&url, &token2, json!({"jsonrpc":"2.0","id":5,"method":"tools/list","params":{}})).await;
    assert_eq!(ok2.status, 200, "turn 2's token is accepted");
    let stale = mcp(&url, &token, json!({"jsonrpc":"2.0","id":6,"method":"tools/list","params":{}})).await;
    assert_eq!(stale.status, 401, "turn 1's token is refused while turn 2 runs");
    std::fs::write(&release2, b"").unwrap();
    d.ended(&id).await;
}

/// **A MANAGED CHILD'S OWN TURN IS HANDED A TOKEN TOO, CARRYING ITS TREE** —
/// so a subagent can itself reach the door. The child's token names the child
/// as caller, the parent as root, at depth 1; read off the token the child's
/// process received (claims are inspected without the key: the door, not this
/// test, is what verifies them).
///
/// **Forced red**: `mint_turn_token` minting every token as a root
/// (`root = thread_id`, `depth = 0`) → the root/depth assertions fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_managed_childs_turn_is_handed_a_token_naming_its_root_and_depth() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let d = Daemon::start().await;
    let parent = d.thread();
    let (cap, release) = d.capturing_turn(&parent, "p").await;
    let token = cap.token();
    let (url, _) = cap.server();
    let childcap = d.tmp.path().join("cap-child");
    let childrel = d.tmp.path().join("release-child");
    let task = format!("capture={} release={}", childcap.display(), childrel.display());
    let spawned = mcp(&url, &token, call(1, "agents_spawn", json!({"task": task, "request_id": "r4s5-2"}))).await;
    let child = spawned.json()["result"]["structuredContent"]["thread_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{}", spawned.json()))
        .to_string();
    let done = PathBuf::from(format!("{}.done", childcap.display()));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !done.exists() {
        assert!(tokio::time::Instant::now() < deadline, "the child's turn never ran");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let cc = Capture::read(&childcap);
    let ctoken = cc.token();
    let claims = shapewire::token::peek_unverified(&ctoken).unwrap();
    assert_eq!(claims.thread_id, child);
    assert_eq!(claims.root, parent, "the child's tree is rooted at its parent");
    assert_eq!(claims.depth, 1);
    // And the door accepts the child's own token while its turn runs.
    let (curl, _) = cc.server();
    let ok = mcp(&curl, &ctoken, json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})).await;
    assert_eq!(ok.status, 200);
    // The parent, at depth 0.
    let pc = shapewire::token::peek_unverified(&token).unwrap();
    assert_eq!((pc.root.as_str(), pc.depth), (parent.as_str(), 0));
    std::fs::write(&childrel, b"").unwrap();
    std::fs::write(&release, b"").unwrap();
    d.ended(&child).await;
    d.ended(&parent).await;
}

/// **A DAEMON STOOD UP THE WAY `run_daemon` STANDS IT UP HANDS ITS TURNS ITS
/// ENVIRONMENT'S PORT** — not the configured `chassis_port` (3653 by default),
/// and not a hard-coded one. `run_daemon` binds the listener itself and serves
/// it through `chassis::chassis`, never through `chassis::serve`, so the bound
/// address is never reported to the manager; the URL a turn is handed is then
/// `host` on `Config::effective_chassis_port`, which reads
/// `THREADS_CHASSIS_PORT` — the variable a second environment's declaration
/// sets. This test binds an ephemeral port, names it ONLY through that
/// variable (the config still says 3653), and asserts the turn's config names
/// it, and that the token works on it.
///
/// **Forced red**: `mcp_url`'s fallback reading `self.config.chassis_port`
/// instead of `effective_chassis_port()` → the URL names `:3653`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_daemon_served_like_run_daemon_hands_its_turns_the_environments_port() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    threads::clock::testing::ticking();
    let listener = threads::server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    assert_ne!(port, 3653);
    std::env::set_var("THREADS_CHASSIS_PORT", port.to_string());
    // The config names the prod default port (what a declaration-less
    // `config.toml` deserializes to); only the env names ours.
    let mut cfg = config_at(tmp.path());
    cfg.chassis_port = 3653;
    let m = build_manager(cfg).unwrap();
    let (bound, h) = threads::chassis::chassis(Arc::new(m.clone()))
        .serve_on_with_shutdown(listener, std::future::pending::<()>())
        .unwrap();
    std::mem::forget(h);
    let d = Daemon { m, addr: bound, tmp };
    let id = d.thread();
    let (cap, release) = d.capturing_turn(&id, "rd").await;
    std::env::remove_var("THREADS_CHASSIS_PORT");
    let token = cap.token();
    let (url, _) = cap.server();
    assert_eq!(url, format!("http://127.0.0.1:{port}/mcp"), "the turn is handed THIS environment's port");
    let ok = mcp(&url, &token, json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}})).await;
    assert_eq!(ok.status, 200, "{}", String::from_utf8_lossy(&ok.body));
    std::fs::write(&release, b"").unwrap();
    d.ended(&id).await;
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
