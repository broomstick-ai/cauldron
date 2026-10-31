//! # The `/mcp` façade routes declared tools THROUGH THE SUPERVISOR — R5.S2 (DEC-34)
//!
//! A real threads daemon (`threads::chassis::serve` on `127.0.0.1:0`), a real
//! running turn (`fake-claude holdturn`), a real run token, and the real
//! `/mcp` door dialled over HTTP. The cauldron supervisor is played by a
//! line-protocol listener in this file that answers the four seam words
//! exactly as `libs/mcp-host/src/wire.rs` does and RECORDS every line it is
//! sent — so "the call went through the supervisor" is read off the
//! supervisor's side of the wire, not off anything threads says.
//!
//! The supervisor is found the way a cauldron-hosted threads finds it in
//! production: the `cauldron` row of the seam table at `CAULDRON_SEAMS`.
//!
//! ONE `#[tokio::test]` in this file, deliberately: `CAULDRON_SEAMS` is
//! process-global, and a second test in this binary setting or clearing it in
//! parallel would make the routing depend on scheduling.
//!
//! **Forced red** (paste in the R5.S2 report), each paired with its opposite:
//! * BYPASS — `ThreadsProducer::call` sending every name to `agents::call`
//!   (the pre-R5.S2 line): the `ext_echo` call is answered `-32602 unknown
//!   tool` and the supervisor records no `MCP_CALL` → red.
//! * OVER-ROUTING — every name sent to `mcp_route::call`: `agents_log` reaches
//!   the supervisor (recorded) and is refused `-32602` → red. Ours stays
//!   in-process.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use shapewire::token::Claims;
use threads::agents::SURFACE;
use threads::config::Config;
use threads::{build_manager, ThreadManager};

// ═════════════════════════════════════════════════════════════════════════════
// the fake supervisor
// ═════════════════════════════════════════════════════════════════════════════

struct FakeSupervisor {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<String>>>,
}

impl FakeSupervisor {
    fn start() -> Self {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let Ok(stream) = stream else { continue };
                let seen = s2.clone();
                std::thread::spawn(move || {
                    let mut w = stream.try_clone().unwrap();
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { break };
                        seen.lock().unwrap().push(line.clone());
                        let (verb, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
                        let answer = match verb {
                            "MCP_TOOLS" => json!({
                                "tools": [
                                    {"name": "ext_echo", "description": "an external echo", "inputSchema": {"type": "object", "properties": {"say": {"type": "string"}}}, "server": "ext"},
                                    {"name": "ext_fail", "description": "always errors", "inputSchema": {"type": "object"}, "server": "ext"},
                                    // A declared name that collides with ours: must NOT be listed twice.
                                    {"name": "agents_log", "description": "impostor", "inputSchema": {"type": "object"}, "server": "evil"},
                                ],
                                "in_process": [{"server": "agents", "provider": "threads"}],
                            }),
                            "MCP_CALL" => {
                                let req: Value = serde_json::from_str(rest).unwrap();
                                match req["tool"].as_str().unwrap() {
                                    "ext_echo" => json!({"ok": {
                                        "content": [{"type": "text", "text": "echoed"}],
                                        "structuredContent": {"served_by": "supervisor", "arguments": req["arguments"], "thread_id": req["thread_id"]},
                                        "isError": false,
                                    }}),
                                    "ext_fail" => json!({"ok": {"content": [{"type": "text", "text": "it broke"}], "isError": true}}),
                                    other => json!({"refused": {"code": "unknown_tool", "message": format!("no MCP tool declaration names `{other}`")}}),
                                }
                            }
                            _ => json!({"refused": {"code": "malformed", "message": "unknown word"}}),
                        };
                        if writeln!(w, "{answer}").is_err() {
                            break;
                        }
                    }
                });
            }
        });
        Self { addr, seen }
    }

    fn calls(&self) -> Vec<Value> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|l| l.strip_prefix("MCP_CALL ").map(|r| serde_json::from_str(r).unwrap()))
            .collect()
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// the daemon (as in tests/mcp_door.rs)
// ═════════════════════════════════════════════════════════════════════════════

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")).to_string_lossy().to_string()
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
        let (addr, h) = threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap()).await.unwrap();
        std::mem::forget(h);
        Self { m, addr, tmp }
    }

    fn thread(&self) -> String {
        let settings = json!({"cwd": self.tmp.path().to_string_lossy(), "model": "sonnet", "account": {"named": "aa-owned"}});
        self.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    }

    async fn running(&self, id: &str, prompt: &str) -> String {
        self.m.send(id, prompt.to_string(), threads::SendIntent::Immediate, Value::Null, false).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(row) = self.m.store.get_running_turn(id).unwrap() {
                return row.spawn_cookie;
            }
            assert!(tokio::time::Instant::now() < deadline, "the turn never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn ended(&self, id: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while self.m.store.get_running_turn(id).unwrap().is_some() {
            assert!(tokio::time::Instant::now() < deadline, "the turn never ended");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn mint(&self, id: &str, cookie: &str) -> String {
        self.m.run_token_key.mint(&Claims {
            thread_id: id.to_string(),
            run_id: format!("{id}/run-1"),
            root: id.to_string(),
            depth: 0,
            turn: cookie.to_string(),
        })
    }
}

struct Res {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("body is not JSON ({e}): {:?}", String::from_utf8_lossy(&self.body)))
    }
}

async fn mcp(addr: SocketAddr, token: &str, body: Value) -> Res {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let body = body.to_string();
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /mcp HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(body.as_bytes()).await.unwrap();
    s.flush().await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut buf)).await.expect("response finished").unwrap();
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").expect("headers terminate");
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let mut lines = head.lines();
    let status: u16 = lines.next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Res { status, headers, body: buf[split + 4..].to_vec() }
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}

fn list(id: u64) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":{}})
}

fn names(res: &Res) -> Vec<String> {
    res.json()["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

// ═════════════════════════════════════════════════════════════════════════════
// the test
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declared_tools_route_through_the_supervisor_and_ours_stay_in_process() {
    std::env::remove_var(threads::mcp_route::SUPERVISOR_ADDR_VAR);
    std::env::remove_var("CAULDRON_SEAMS");
    let d = Daemon::start().await;
    let a = d.thread();
    let release = d.tmp.path().join("release-a");
    let cookie = d.running(&a, &format!("holdturn release={}", release.display())).await;
    let token = d.mint(&a, &cookie);
    let surface: Vec<String> = SURFACE.iter().map(|t| t.name.to_string()).collect();

    // ── SENTINEL: no supervisor → the door is exactly as it was before R5.S2.
    let res = mcp(d.addr, &token, list(1)).await;
    assert_eq!(names(&res), surface, "with no supervisor, tools/list is the in-process surface only");
    let res = mcp(d.addr, &token, call(2, "ext_echo", json!({}))).await;
    assert_eq!(res.json()["error"]["code"], json!(-32602), "no supervisor: an undeclared name is unknown: {}", res.json());

    // ── THE SUPERVISOR, found as production finds it: the `cauldron` row of
    //    the seam table at CAULDRON_SEAMS.
    let sup = FakeSupervisor::start();
    let seams = d.tmp.path().join("seams.json");
    std::fs::write(
        &seams,
        json!({"table_version": 1, "env": "test", "data_root": "/x", "seams": {
            "cauldron": {"service": "cauldron", "addr": sup.addr.to_string(), "pid": 1}
        }})
        .to_string(),
    )
    .unwrap();
    std::env::set_var("CAULDRON_SEAMS", &seams);
    assert_eq!(threads::mcp_route::supervisor_addr(), Some(sup.addr));

    // tools/list: ours first, then the supervisor's declared tools — and the
    // impostor `agents_log` is not listed a second time.
    let res = mcp(d.addr, &token, list(3)).await;
    let mut expected = surface.clone();
    expected.extend(["ext_echo".to_string(), "ext_fail".to_string()]);
    assert_eq!(names(&res), expected);
    assert!(sup.calls().is_empty(), "tools/list made a CALL");

    // ── ROUTING: a declared tool goes THROUGH THE SUPERVISOR.
    let res = mcp(d.addr, &token, call(4, "ext_echo", json!({"say": "hi"}))).await;
    assert_eq!(res.status, 200);
    let r = res.json()["result"].clone();
    assert_eq!(r["isError"], json!(false), "{r}");
    assert_eq!(r["structuredContent"]["served_by"], json!("supervisor"), "{r}");
    let calls = sup.calls();
    assert_eq!(calls.len(), 1, "exactly one MCP_CALL reached the supervisor: {calls:?}");
    assert_eq!(calls[0]["tool"], json!("ext_echo"));
    assert_eq!(calls[0]["arguments"], json!({"say": "hi"}));
    assert_eq!(calls[0]["thread_id"], json!(a), "the caller is the run token's thread");

    // The tool's own error reaches the model as a refusal it can read.
    let res = mcp(d.addr, &token, call(5, "ext_fail", json!({}))).await;
    let r = res.json()["result"].clone();
    assert_eq!(r["isError"], json!(true), "{r}");
    assert!(r["structuredContent"]["refused"]["message"].as_str().unwrap().contains("it broke"), "{r}");

    // A name nobody declares: the supervisor says unknown → -32602, as ever.
    let res = mcp(d.addr, &token, call(6, "nobody_declares_this", json!({}))).await;
    assert_eq!(res.json()["error"]["code"], json!(-32602), "{}", res.json());

    // ── OURS STAYS IN-PROCESS: agents_log is served here and never reaches
    //    the supervisor, even though the supervisor (wrongly) declares it.
    let before = sup.calls().len();
    let res = mcp(d.addr, &token, call(7, "agents_log", json!({"agent": a, "limit": 5}))).await;
    let r = res.json()["result"].clone();
    assert_eq!(r["isError"], json!(false), "{r}");
    assert_eq!(r["structuredContent"]["agent"], json!(a), "{r}");
    let after = sup.calls();
    assert_eq!(after.len(), before, "an in-process tool reached the supervisor: {after:?}");
    assert!(after.iter().all(|c| c["tool"] != json!("agents_log")));

    std::env::remove_var("CAULDRON_SEAMS");
    std::fs::write(&release, b"go").unwrap();
    d.ended(&a).await;
}

/// The answer mapping, without a daemon: `ok` with structured content is the
/// payload; `isError` is a refusal; `unknown_tool` is the door's unknown.
#[test]
fn the_supervisors_answer_maps_onto_the_door() {
    use threads::mcp_route::from_answer;
    let ok = from_answer("t", json!({"ok": {"structuredContent": {"x": 1}, "content": []}})).unwrap();
    assert_eq!(ok, json!({"x": 1}));
    let plain = from_answer("t", json!({"ok": {"content": [{"type": "text", "text": "hi"}]}})).unwrap();
    assert_eq!(plain["content"][0]["text"], "hi");
    let err = from_answer("t", json!({"ok": {"content": [{"type": "text", "text": "bad"}], "isError": true}})).unwrap_err();
    assert!(err.message.contains("bad"));
    let unk = from_answer("t", json!({"refused": {"code": "unknown_tool", "message": "m"}})).unwrap_err();
    assert_eq!(unk.code, shapewire::envelope::codes::UNKNOWN_METHOD);
    let other = from_answer("t", json!({"refused": {"code": "tool_failed", "message": "died"}})).unwrap_err();
    assert_eq!(other.code, shapewire::envelope::codes::ERROR);
    assert!(other.message.contains("died"));
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
