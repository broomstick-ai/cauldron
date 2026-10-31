//! **SH-A4 — `threads/{thread_id}/detach` and the attached-session pid reaper**
//! (`docs/SHIMS.md` §A.1 step 4, §A.3, §A.4 row SH-A4).
//!
//! Each test names its forced red (SH-A4 report,
//! an internal report in substrate-interface-layer):
//!
//! * `detach_deletes_the_row_and_the_token_is_refused_after` — over the real
//!   `/ws` door and the real `/mcp` door: 200 while attached, detach, then the
//!   SAME request is 401; no row, no lock, no files, the ledger's turn closed
//!   and clean. Forced red by SOFT-ENDING (not deleting) the row: `verify()`
//!   still sees it and answers 200.
//! * `detach_of_an_unknown_cookie_answers_not_found_and_changes_nothing` — a
//!   bogus cookie, and the right cookie on the WRONG thread, are refused
//!   wire code `not_found` and leave the row and the lock; the real detach then
//!   succeeds and a second detach of it is not_found (opposing).
//! * `two_attached_sessions_detaching_one_leaves_the_other` — N=2: detach A,
//!   B's row, lock, lease and token all survive.
//! * `the_reaper_detaches_only_sessions_whose_every_pid_is_dead` — a dead
//!   wrapper pid is reaped; a live wrapper pid, and a dead wrapper with a live
//!   claude pid, are not; the reaper signals nothing (the live child is still
//!   running after). Forced red by treating every row as dead.
//! * `the_reaper_task_reaps_on_its_period_and_keeps_a_live_session_past_ten_periods`
//!   — the spawned task at a 100 ms period: the dead row goes, the live row
//!   outlasts 10 periods.
//! * `detach_re_dispatches_the_queue_inside_the_backstop_tick` — a message
//!   sent while attached waits; detach dispatches it in well under the 60 s
//!   backstop tick the fixture sets. Forced red by removing the re-dispatch.
//! * `a_detach_racing_the_reaper_has_one_winner_and_the_loser_is_not_found` —
//!   SH-A2c: both held at a rendezvous after their reads; exactly one wins and
//!   the loser answers not_found, never `released: false`. Forced red by
//!   restoring the unserialized delete-then-remove (an internal report).
//!
//! Nothing launches a real claude: `THREADS_CLAUDE_BIN` does not exist.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::ledger::turn::ledger_path;
use threads::ledger::{Kind, Ledger};
use threads::runtime::attach::{AttachRequest, DetachRefused};
use threads::settings::{Projection, ThreadSettings};
use threads::{build_manager, SendIntent, ThreadManager};

const ACCOUNT: &str = "aa-detach";

fn config(tmp: &Path) -> Config {
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
    // The backstop placement tick: 60 s, so a dispatch inside a few seconds is
    // the detach's own doing, never the tick's.
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var("THREADS_CLAUDE_BIN", "/nonexistent/fake-claude-for-detach");
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

fn projected_thread(m: &ThreadManager, tmp: &Path, name: &str) -> String {
    let cwd = tmp.join("work").join(name);
    std::fs::create_dir_all(&cwd).unwrap();
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.projection = Projection::Projected;
    m.create_thread(None, s).unwrap()
}

fn turn_files(tmp: &Path, id: &str) -> Vec<PathBuf> {
    match std::fs::read_dir(tmp.join("turns").join(id)) {
        Ok(rd) => rd.map(|e| e.unwrap().path()).collect(),
        Err(_) => vec![],
    }
}

/// A pid that is certainly dead: a child we spawned and reaped ourselves.
fn dead_pid() -> i64 {
    let mut c = std::process::Command::new("/usr/bin/true").spawn().unwrap();
    let pid = c.id() as i64;
    c.wait().unwrap();
    pid
}

/// A live pid that is NOT this test process: a sleeping child. The reaper
/// must leave it running (it never signals).
fn live_child() -> std::process::Child {
    std::process::Command::new("/bin/sleep").arg("60").spawn().unwrap()
}

fn still_running(c: &mut std::process::Child) -> bool {
    matches!(c.try_wait(), Ok(None))
}

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

/// POST one JSON-RPC `ping` to `url` with exactly this `Authorization`.
async fn ping(url: &str, authorization: &str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.strip_prefix("http://").expect("an http URL");
    let (hostport, path) = rest.split_once('/').expect("a path");
    let addr: SocketAddr = hostport.parse().expect("host:port");
    let body = json!({"jsonrpc":"2.0","id":1,"method":"ping"}).to_string();
    let mut s = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr))
        .await
        .expect("connect did not hang")
        .unwrap();
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
        .expect("the response finished")
        .unwrap();
    let head = String::from_utf8_lossy(&buf).to_string();
    head.lines().next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detach_deletes_the_row_and_the_token_is_refused_after() {
    let (m, addr, tmp) = start().await;
    let id = projected_thread(&m, tmp.path(), "a");
    let mut c = Client::connect(addr).await;
    let r = c
        .request("a1", &format!("threads/{id}/attach"), json!({"wrapper_pid": std::process::id()}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "attach answered: {r}");
    let p = &r["outcome"]["payload"];
    let cookie = p["cookie"].as_str().unwrap().to_string();
    let args: Vec<String> = serde_json::from_value(p["args"].clone()).unwrap();
    let mcp = PathBuf::from(arg_after(&args, "--mcp-config").unwrap());
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&mcp).unwrap()).unwrap();
    let url = cfg["mcpServers"]["threads"]["url"].as_str().unwrap().to_string();
    let auth = cfg["mcpServers"]["threads"]["headers"]["Authorization"].as_str().unwrap().to_string();

    // SENTINEL: attached, the token opens the door — so the 401 below can fail.
    assert_eq!(ping(&url, &auth).await, 200, "the attached token opens /mcp");
    assert!(m.is_locked(&id));
    assert!(!turn_files(tmp.path(), &id).is_empty(), "the lease's files exist while attached");

    // ── DETACH over the real door ──
    let d = c
        .request("d1", &format!("threads/{id}/detach"), json!({"cookie": cookie, "exit": 0}))
        .await;
    assert_eq!(d["outcome"]["type"], json!("success"), "detach answered: {d}");
    assert_eq!(d["outcome"]["payload"]["released"], json!(true), "{d}");

    // ── the row is GONE (deleted, not soft-ended), so verify refuses ──
    assert!(m.store.get_attached_session(&cookie).unwrap().is_none(), "the row is deleted");
    assert!(m.store.list_attached_sessions(&id).unwrap().is_empty());
    assert_eq!(ping(&url, &auth).await, 401, "a detached session's token is refused");
    // ── the lease is released ──
    assert!(!m.is_locked(&id), "the lock is released");
    assert!(m.attached_cookies(&id).is_empty(), "the lease left the map");
    assert!(turn_files(tmp.path(), &id).is_empty(), "both files removed: {:?}", turn_files(tmp.path(), &id));
    // ── the final reconcile and turn_ended ──
    let l = Ledger::read_jsonl(&ledger_path(tmp.path(), &id)).unwrap();
    let ended: Vec<_> = l
        .events()
        .iter()
        .filter(|e| e.kind == Kind::TurnEnded && e.turn.as_deref() == Some(cookie.as_str()))
        .collect();
    assert_eq!(ended.len(), 1, "exactly one turn_ended for the attach's turn: {ended:?}");
    assert_eq!(ended[0].body["detail"]["exit_code"], json!(0), "the wrapper's exit is recorded");
    assert!(threads::ledger::turn::dirty_mark(&l).is_none(), "the ledger is clean after the reconcile");
    // OPPOSING: the thread can be attached again.
    m.attach_thread(&id, &AttachRequest { wrapper_pid: std::process::id() as i64, account: None })
        .expect("a detached thread attaches again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detach_of_an_unknown_cookie_answers_not_found_and_changes_nothing() {
    let (m, addr, tmp) = start().await;
    let a = projected_thread(&m, tmp.path(), "a");
    let b = projected_thread(&m, tmp.path(), "b");
    let me = std::process::id() as i64;
    let ra = m.attach_thread(&a, &AttachRequest { wrapper_pid: me, account: None }).unwrap();

    let mut c = Client::connect(addr).await;
    let bogus = c
        .request("x1", &format!("threads/{a}/detach"), json!({"cookie": "no-such-cookie"}))
        .await;
    assert_ne!(bogus["outcome"]["type"], json!("success"), "{bogus}");
    // The CODE a client branches on is `not_found` (SH-A2c, DEPLOY-9k finding
    // 2) — not `unknown` with `not_found` only in the prose.
    assert_eq!(bogus["outcome"]["code"], json!("not_found"), "typed not-found: {bogus}");

    // The right cookie on the WRONG thread is not found either — never re-homed.
    match m.detach_thread(&b, &ra.cookie, None) {
        Err(DetachRefused::NotFound { .. }) => {}
        other => panic!("a cookie on another thread is not found, got {other:?}"),
    }
    // Nothing changed.
    assert!(m.store.get_attached_session(&ra.cookie).unwrap().is_some(), "the row survives");
    assert!(m.is_locked(&a), "the lock survives");
    assert_eq!(m.attached_cookies(&a), vec![ra.cookie.clone()]);

    // OPPOSING: the real detach succeeds; a second detach of it is not found.
    let ok = c
        .request("x2", &format!("threads/{a}/detach"), json!({"cookie": ra.cookie}))
        .await;
    assert_eq!(ok["outcome"]["type"], json!("success"), "{ok}");
    // …and a second detach of the released cookie — the race loser's answer —
    // is the same `not_found` code on the wire (SH-A2c).
    let again = c
        .request("x3", &format!("threads/{a}/detach"), json!({"cookie": ra.cookie}))
        .await;
    assert_eq!(again["outcome"]["code"], json!("not_found"), "a second detach is not found: {again}");
    match m.detach_thread(&a, &ra.cookie, None) {
        Err(DetachRefused::NotFound { .. }) => {}
        other => panic!("a second detach is not found, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_attached_sessions_detaching_one_leaves_the_other() {
    let (m, _addr, tmp) = start().await;
    let a = projected_thread(&m, tmp.path(), "a");
    let b = projected_thread(&m, tmp.path(), "b");
    let me = std::process::id() as i64;
    let ra = m.attach_thread(&a, &AttachRequest { wrapper_pid: me, account: None }).unwrap();
    let rb = m.attach_thread(&b, &AttachRequest { wrapper_pid: me, account: None }).unwrap();
    assert_eq!(m.store.list_all_attached_sessions().unwrap().len(), 2, "N=2 attached");

    let d = m.detach_thread(&a, &ra.cookie, None).expect("detach A");
    assert!(d.released);
    // A is gone…
    assert!(m.store.get_attached_session(&ra.cookie).unwrap().is_none());
    assert!(!m.is_locked(&a));
    // …B is untouched.
    let rows = m.store.list_all_attached_sessions().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].cookie, rb.cookie);
    assert!(m.is_locked(&b), "B still holds its thread");
    assert_eq!(m.attached_cookies(&b), vec![rb.cookie.clone()]);
    assert!(!turn_files(tmp.path(), &b).is_empty(), "B's files are untouched");
    // And B detaches normally afterwards.
    m.detach_thread(&b, &rb.cookie, None).expect("detach B");
    assert!(m.store.list_all_attached_sessions().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reaper_detaches_only_sessions_whose_every_pid_is_dead() {
    let (m, _addr, tmp) = start().await;
    let dead = projected_thread(&m, tmp.path(), "dead");
    let live = projected_thread(&m, tmp.path(), "live");
    let mixed = projected_thread(&m, tmp.path(), "mixed");
    let mut child = live_child();
    let child_pid = child.id() as i64;

    let rd = m.attach_thread(&dead, &AttachRequest { wrapper_pid: dead_pid(), account: None }).unwrap();
    let rl = m.attach_thread(&live, &AttachRequest { wrapper_pid: child_pid, account: None }).unwrap();
    // A dead wrapper with a LIVE claude pid (SessionStart reported it).
    let rm = m.attach_thread(&mixed, &AttachRequest { wrapper_pid: dead_pid(), account: None }).unwrap();
    let wrapper = m.store.get_attached_session(&rm.cookie).unwrap().unwrap().wrapper_pid;
    assert!(m.store.update_attached_pids(&rm.cookie, wrapper, Some(child_pid)).unwrap());

    let reaped = m.reap_attached_sessions();
    assert_eq!(reaped, vec![rd.cookie.clone()], "only the all-dead session is reaped");
    assert!(m.store.get_attached_session(&rd.cookie).unwrap().is_none(), "the dead row is deleted");
    assert!(!m.is_locked(&dead), "the dead session's lock is released");
    assert!(m.store.get_attached_session(&rl.cookie).unwrap().is_some(), "a live wrapper keeps its row");
    assert!(m.is_locked(&live));
    assert!(m.store.get_attached_session(&rm.cookie).unwrap().is_some(), "a live claude pid keeps its row");
    assert!(m.is_locked(&mixed));
    // The reaper never signals: the live process is still running.
    assert!(still_running(&mut child), "the reaper must never kill a process");
    // A second sweep is idempotent.
    assert!(m.reap_attached_sessions().is_empty());

    // OPPOSING: the same live session IS reaped once its process is gone.
    child.kill().unwrap();
    child.wait().unwrap();
    let mut again = m.reap_attached_sessions();
    again.sort();
    let mut want = vec![rl.cookie.clone(), rm.cookie.clone()];
    want.sort();
    assert_eq!(again, want, "both go once the live pid is dead");
    assert!(m.store.list_all_attached_sessions().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reaper_task_reaps_on_its_period_and_keeps_a_live_session_past_ten_periods() {
    let (m, _addr, tmp) = start().await;
    let dead = projected_thread(&m, tmp.path(), "dead");
    let live = projected_thread(&m, tmp.path(), "live");
    let mut child = live_child();
    let rd = m.attach_thread(&dead, &AttachRequest { wrapper_pid: dead_pid(), account: None }).unwrap();
    let rl = m.attach_thread(&live, &AttachRequest { wrapper_pid: child.id() as i64, account: None }).unwrap();

    let period = Duration::from_millis(100);
    let task = threads::liveness::spawn_attach_reaper(m.clone(), period);
    let t0 = std::time::Instant::now();
    while m.store.get_attached_session(&rd.cookie).unwrap().is_some() {
        assert!(t0.elapsed() < Duration::from_secs(5), "the reaper task did not reap the dead row");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Ten periods past the reap: the live row is still there.
    tokio::time::sleep(period * 10).await;
    assert!(m.store.get_attached_session(&rl.cookie).unwrap().is_some(), "live row outlasts 10 periods");
    assert!(m.is_locked(&live));
    assert!(still_running(&mut child), "never killed");
    task.abort();
    child.kill().unwrap();
    child.wait().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detach_re_dispatches_the_queue_inside_the_backstop_tick() {
    let (m, _addr, tmp) = start().await;
    let id = projected_thread(&m, tmp.path(), "q");
    let r = m
        .attach_thread(&id, &AttachRequest { wrapper_pid: std::process::id() as i64, account: None })
        .unwrap();
    m.send(&id, "mail while attached".into(), SendIntent::Immediate, Value::Null, false).unwrap();
    // While attached it waits (the lock is the attach's).
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(m.store.waiting_queue(&id).unwrap().len(), 1, "queued behind the attach");

    m.detach_thread(&id, &r.cookie, None).unwrap();
    // Dispatched by the detach itself: 5 s is far inside the 60 s backstop.
    let t0 = std::time::Instant::now();
    while !m.store.waiting_queue(&id).unwrap().is_empty() {
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "the detach did not re-dispatch the queue (the backstop tick is 60 s)"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// SH-A2c (SH-A4 inspect, finding 2): an explicit detach RACING the reaper on
/// one cookie. Both are held at a rendezvous right after their reads, so BOTH
/// have seen the row before either deletes it — the interleaving that used to
/// let the loser answer `Ok(released: false)` (or put a cookie the explicit
/// detach released into the reaped list). Exactly one wins: either the
/// explicit detach answers `released: true` and the reaper reaps nothing, or
/// the reaper reaps the cookie and the explicit detach answers NOT FOUND.
/// Never both, never neither, never `released: false`. N = 12 rounds, each a
/// fresh attach. Forced red by restoring the unserialized delete-then-remove
/// (the SH-A4 code), rendezvous kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_detach_racing_the_reaper_has_one_winner_and_the_loser_is_not_found() {
    let (m, _addr, tmp) = start().await;
    let id = projected_thread(&m, tmp.path(), "race");
    let (mut explicit_won, mut reaper_won) = (0, 0);
    for round in 0..12 {
        let r = m.attach_thread(&id, &AttachRequest { wrapper_pid: dead_pid(), account: None }).unwrap();
        threads::runtime::attach::rendezvous_detaches_after_read_for_test(&r.cookie, 2);
        let (m1, m2, id1, c1) = (m.clone(), m.clone(), id.clone(), r.cookie.clone());
        let explicit = tokio::task::spawn_blocking(move || m1.detach_thread(&id1, &c1, None));
        let reaper = tokio::task::spawn_blocking(move || m2.reap_attached_sessions());
        let (explicit, reaped) = (explicit.await.unwrap(), reaper.await.unwrap());
        match (&explicit, reaped.as_slice()) {
            (Ok(d), []) if d.released => explicit_won += 1,
            (Err(DetachRefused::NotFound { .. }), [c]) if *c == r.cookie => reaper_won += 1,
            _ => panic!("round {round}: one winner, the loser not-found; got explicit {explicit:?}, reaped {reaped:?}"),
        }
        assert!(m.store.get_attached_session(&r.cookie).unwrap().is_none(), "round {round}: row gone");
        assert!(!m.is_locked(&id), "round {round}: lock released");
        assert!(m.attached_cookies(&id).is_empty(), "round {round}: lease left the map");
    }
    eprintln!("detach/reaper race: explicit won {explicit_won}, reaper won {reaper_won}");
    assert_eq!(explicit_won + reaper_won, 12);
}
