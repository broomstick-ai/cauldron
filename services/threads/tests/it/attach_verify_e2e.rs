//! **SH-A2b — attach → verify, end to end** (`docs/SHIMS.md` §SH-A2, §SH-A3).
//!
//! SH-A2 mints the token an attached session carries; SH-A3's `verify()`
//! (`src/chassis.rs`, the ATTACHED window) accepts it while the session's
//! `attached_sessions` row exists. Each half has its own tests; this file pins
//! the seam between them, so a drift in either (the claims SH-A2 mints, the
//! row SH-A3 matches) turns red here instead of as a 401 at the TUI's MCP door.
//!
//! The probe is the SH-A2 inspector's (an internal report, finding
//! 1): attach over the real `/ws` door, read the recipe's `--mcp-config` file,
//! POST a JSON-RPC `ping` to ITS `url` with ITS `Authorization` header →
//! **200**; delete the attached row → the very same request is **401**.
//!
//! Forced red (SH-A2b report, an internal report): (a) skipping the
//! row delete fails the 401 arm; (b) deleting the row before the first ping
//! fails the 200 arm. A token that is not this session's is refused while the
//! row exists (sentinel: the 200 is the token's doing, not an open door).
//!
//! Nothing launches claude: `THREADS_CLAUDE_BIN` is a path that does not exist.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::settings::{Projection, ThreadSettings};
use threads::{build_manager, ThreadManager};

const ACCOUNT: &str = "aa-attach-e2e";

fn config(tmp: &Path) -> Config {
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var("THREADS_CLAUDE_BIN", "/nonexistent/fake-claude-for-attach-e2e");
    std::env::remove_var("THREADS_CHASSIS_PORT");
    let mut accounts = BTreeMap::new();
    accounts.insert(ACCOUNT.to_string(), tmp.join(ACCOUNT).to_string_lossy().to_string());
    Config {
        data_dir: Some(tmp.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

async fn start() -> (ThreadManager, SocketAddr, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    threads::clock::testing::ticking();
    let m = build_manager(config(tmp.path())).unwrap();
    let (addr, h) = threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    std::mem::forget(h);
    (m, addr, tmp)
}

fn projected_thread(m: &ThreadManager, tmp: &Path) -> String {
    let cwd = tmp.join("work").join("e2e");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.projection = Projection::Projected;
    m.create_thread(None, s).unwrap()
}

// ── the /ws client (as `tests/attach_verb.rs`) ─────────────────────────────

struct Client {
    ws: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await.unwrap();
        let mut c = Self { ws };
        let _welcome = c.next().await.expect("a welcome");
        c
    }
    async fn next(&mut self) -> Option<Value> {
        use tokio_tungstenite::tungstenite::Message;
        loop {
            match tokio::time::timeout(Duration::from_secs(5), self.ws.next()).await {
                Err(_) | Ok(None) => return None,
                Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).ok(),
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) => return None,
            }
        }
    }
    async fn request(&mut self, id: &str, method: &str, params: Value) -> Value {
        use tokio_tungstenite::tungstenite::Message;
        let frame = json!({"type":"request","id":id,"method":method,"params":params});
        self.ws.send(Message::Text(frame.to_string().into())).await.unwrap();
        loop {
            let f = self.next().await.unwrap_or_else(|| panic!("no response to {method}"));
            if f["type"] == "response" && f["correlate"] == id {
                return f;
            }
        }
    }
}

// ── a hand-rolled HTTP/1.1 client (as `tests/turn_mcp_door.rs`) ────────────

struct Res {
    status: u16,
    body: Vec<u8>,
}

impl Res {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// POST one JSON-RPC body to `url` (`http://host:port/path`) with exactly the
/// `Authorization` header value given.
async fn post(url: &str, authorization: &str, body: Value) -> Res {
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
         Accept: application/json, text/event-stream\r\nAuthorization: {authorization}\r\n\
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

fn ping(id: u64) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"ping"})
}

fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_token_an_attach_returns_opens_mcp_and_is_refused_once_its_row_is_deleted() {
    let (m, addr, tmp) = start().await;
    let id = projected_thread(&m, tmp.path());

    // ── attach over the real /ws door ──
    let mut c = Client::connect(addr).await;
    let r = c.request("e1", &format!("threads/{id}/attach"), json!({"wrapper_pid": 31337})).await;
    assert_eq!(r["outcome"]["type"], json!("success"), "attach answered: {r}");
    let p = &r["outcome"]["payload"];
    let cookie = p["cookie"].as_str().expect("a cookie").to_string();
    let args: Vec<String> = serde_json::from_value(p["args"].clone()).unwrap();
    let env_token = p["env"]["THREADS_RUN_TOKEN"].as_str().expect("THREADS_RUN_TOKEN in the recipe env").to_string();

    // ── the door the recipe hands claude: url + Authorization from the file ──
    let mcp_path = PathBuf::from(arg_after(&args, "--mcp-config").expect("--mcp-config on the recipe argv"));
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
    let server = &cfg["mcpServers"]["threads"];
    let url = server["url"].as_str().unwrap_or_else(|| panic!("a url: {cfg}")).to_string();
    let auth = server["headers"]["Authorization"].as_str().unwrap_or_else(|| panic!("a bearer: {cfg}")).to_string();
    assert_eq!(url, format!("http://{addr}/mcp"), "the file names THIS daemon's /mcp");
    assert_eq!(auth, format!("Bearer {env_token}"), "the file's bearer is the recipe env's token");

    // The row the token's claims must match exists, on this thread.
    let row = m.store.get_attached_session(&cookie).unwrap().expect("the attached row");
    assert_eq!(row.thread_id, id);
    // No turn is running: the 200 below can only come from the ATTACHED window.
    assert!(m.store.get_running_turn(&id).unwrap().is_none(), "no running_turns row for an attach");

    // ── row present → 200 ──
    let ok = post(&url, &auth, ping(1)).await;
    assert_eq!(ok.status, 200, "the attach's own token opens /mcp: {}", ok.text());

    // SENTINEL (opposing the 200): while the row exists, a token that is not
    // this session's is refused — the door is not simply open.
    let forged = post(&url, "Bearer rt1.not-a-token-this-daemon-minted", ping(2)).await;
    assert_eq!(forged.status, 401, "a foreign token is refused while the row exists: {}", forged.text());

    // ── row deleted → 401, same url, same header ──
    assert!(m.store.delete_attached_session(&cookie).unwrap(), "the row was there to delete");
    assert!(m.store.get_attached_session(&cookie).unwrap().is_none());
    let gone = post(&url, &auth, ping(3)).await;
    assert_eq!(gone.status, 401, "a deleted attached row voids its token: {}", gone.text());
}

// ── the attach verb is in the GENERATED inventory ──────────────────────────
//
// `libs/shapewire/seam/inventory.json` is written by
// `libs/shapewire/seam/tools/make-inventory.py` (never by hand); a face routes
// against it, so a verb the daemon serves but the inventory omits cannot be
// dispatched by any face. Forced red by deleting `attach` from the generated
// file; opposed by planting a name the daemon does not serve.

#[test]
fn the_attach_verb_is_in_the_generated_inventory() {
    const ATTACH: &str = "threads/{thread_id}/attach";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../libs/shapewire/seam/inventory.json");
    let inv: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .unwrap_or_else(|e| panic!("{} is JSON: {e}", path.display()));
    let names = |k: &str| -> Vec<String> {
        serde_json::from_value(inv[k].clone()).unwrap_or_else(|e| panic!("inventory `{k}` is a name list: {e}"))
    };
    let ws = names("ws_methods");
    let all = names("methods");
    // SENTINEL: the daemon serves the verb, and the lists read are not empty.
    assert!(threads::server::WS_METHODS.contains(&ATTACH), "the daemon serves {ATTACH}");
    assert!(ws.len() > 1 && all.len() >= ws.len(), "real lists: ws={} all={}", ws.len(), all.len());
    // ── the verb is in the inventory, in both lists ──
    assert!(ws.iter().any(|n| n == ATTACH), "inventory ws_methods names {ATTACH}");
    assert!(all.iter().any(|n| n == ATTACH), "inventory methods names {ATTACH}");
    // OPPOSING: the inventory names no ws method the daemon does not serve, so
    // the membership above is a property of the list, not of any name.
    let phantom: Vec<&String> = ws.iter().filter(|n| !threads::server::WS_METHODS.contains(&n.as_str())).collect();
    assert!(phantom.is_empty(), "inventory ws_methods the daemon does not serve: {phantom:?}");
}
