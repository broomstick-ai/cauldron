//! **SH-A5 — `recover()` handles attached sessions across a REAL daemon
//! restart** (`docs/SHIMS.md` §A.3 "Daemon restart while attached", §A.4 row
//! SH-A5).
//!
//! `an_attached_session_survives_a_restart_alive_kept_dead_released_none_signalled`
//! runs the real `threads` binary twice on one data dir. Daemon 1 attaches
//! TWO projected threads over `/ws` — X leased to a live wrapper pid (a
//! sleeping child of this test), Y to a wrapper pid this test then kills and
//! reaps. Daemon 1 is SIGKILLed (it never detaches anything). Daemon 2 boots on
//! the same data dir and its recovery must:
//!
//! * KEEP X: the row stays, X's thread lock is re-taken under daemon 2's pid
//!   and boot id, and a detach through daemon 2 answers `released: true` and
//!   frees the lock — the lease is really held in daemon 2, not just left on
//!   disk;
//! * RELEASE Y: the row is gone, Y's thread has no lock row, and Y's ledger
//!   closes the attach's turn (`turn_ended{interrupted}`) and is no longer
//!   dirty (reconciled);
//! * SIGNAL NOTHING: X's wrapper is still running after recovery and after the
//!   detach.
//!
//! Sentinels: before daemon 2 starts, BOTH rows exist and BOTH lock rows are
//! held by daemon 1's (dead) pid and both ledgers are dirty — so every
//! assertion after the restart is a change recovery made. The reaper is OFF
//! in both daemons (`THREADS_ATTACH_REAP_MS=0`), so recovery alone can have
//! deleted Y's row. Forcings: SH-A5 report, item 5.
//!
//! Nothing here touches `:3653`, `~/.cauldron` or a real claude.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::ledger::turn::{dirty_mark, ledger_path};
use threads::ledger::{Kind, Ledger};
use threads::settings::{Projection, ThreadSettings};
use threads::build_manager;

#[path = "support/keychain.rs"]
mod keychain;

const ACCOUNT: &str = "aa-owned";

fn config_at(dir: &Path) -> Config {
    let mut map = BTreeMap::new();
    std::fs::create_dir_all(dir.join(ACCOUNT)).unwrap();
    map.insert(ACCOUNT.to_string(), dir.join(ACCOUNT).to_string_lossy().to_string());
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

/// Two projected threads, created in process before any daemon runs.
fn fixture(scratch: &Path, data_dir: &Path) -> (String, String) {
    keychain::install();
    std::env::set_var("THREADS_CLAUDE_HOME", scratch.join("claude-home"));
    std::env::set_var("CAULDRON_HOME", scratch.join("cauldron-home"));
    std::env::remove_var("THREADS_CHASSIS_PORT");
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let ids = rt.block_on(async {
        threads::clock::testing::ticking();
        let m = build_manager(config_at(data_dir)).unwrap();
        let mut ids = vec![];
        for name in ["x", "y"] {
            let cwd = scratch.join("work").join(name);
            std::fs::create_dir_all(&cwd).unwrap();
            let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
            s.projection = Projection::Projected;
            ids.push(m.create_thread(None, s).unwrap());
        }
        ids
    });
    rt.shutdown_background();
    (ids[0].clone(), ids[1].clone())
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn health(port: u16) -> Option<Value> {
    let sa: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let t = Duration::from_millis(1500);
    let mut s = TcpStream::connect_timeout(&sa, t).ok()?;
    s.set_read_timeout(Some(t)).ok()?;
    s.write_all(format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes())
        .ok()?;
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).to_string();
    if text.split_whitespace().nth(1)? != "200" {
        return None;
    }
    text.split("\r\n\r\n").nth(1).and_then(|b| serde_json::from_str(b).ok())
}

struct Daemon {
    child: Child,
    log: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Daemon {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
    fn pid(&self) -> i64 {
        self.child.id() as i64
    }
    /// SIGKILL — the daemon gets no chance to detach anything.
    fn kill_hard(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

fn spawn_daemon(scratch: &Path, data_dir: &Path, port: u16) -> Daemon {
    let cfg = scratch.join(format!("config-{port}.toml"));
    std::fs::write(
        &cfg,
        format!(
            "host = \"127.0.0.1\"\nchassis_port = {port}\ndata_dir = \"{dd}\"\nclaude_bin = \"/nonexistent/fake-claude-sh-a5\"\n\
             calibrate = false\nedge_poll = false\nprobe_interval_secs = 0\nprobe_interval_busy_secs = 0\n\
             [accounts]\n{ACCOUNT} = \"{dd}/{ACCOUNT}\"\n",
            dd = data_dir.display(),
        ),
    )
    .unwrap();
    let log = scratch.join(format!("daemon-{port}.log"));
    let child = Command::new(env!("CARGO_BIN_EXE_threads"))
        .arg("--config")
        .arg(&cfg)
        .env_remove("THREADS_CHASSIS_PORT")
        .env("THREADS_UPDATE_WATCH", "off")
        .env("THREADS_RESUME_TICK_MS", "60000")
        // The reaper is OFF: only recovery may settle an attached row here.
        .env("THREADS_ATTACH_REAP_MS", "0")
        .env("CAULDRON_HOME", scratch.join("cauldron-home"))
        .env("THREADS_CLAUDE_BIN", "/nonexistent/fake-claude-sh-a5")
        .env("THREADS_CLAUDE_HOME", scratch.join("claude-home"))
        .env("HOME", scratch.join("home"))
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .expect("spawn the threads binary");
    let d = Daemon { child, log };
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(b) = health(port) {
            if b["status"] == "ok" && b["recovery"]["state"] == "done" {
                break;
            }
        }
        assert!(Instant::now() < deadline, "daemon on {port} never finished recovery; log:\n{}", d.log());
        std::thread::sleep(Duration::from_millis(50));
    }
    d
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(port: u16) -> Ws {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws")).await.unwrap();
    let _welcome = next(&mut ws).await.expect("a welcome");
    ws
}

async fn next(ws: &mut Ws) -> Option<Value> {
    use tokio_tungstenite::tungstenite::Message;
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
            Err(_) | Ok(None) => return None,
            Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).ok(),
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) => return None,
        }
    }
}

async fn request(ws: &mut Ws, id: &str, method: &str, params: Value) -> Value {
    use tokio_tungstenite::tungstenite::Message;
    let frame = json!({"type":"request","id":id,"method":method,"params":params});
    ws.send(Message::Text(frame.to_string().into())).await.unwrap();
    loop {
        let f = next(ws).await.unwrap_or_else(|| panic!("no response to {method}"));
        if f["type"] == "response" && f["correlate"] == id {
            return f;
        }
    }
}

/// The durable state, read straight off the db (read-only; the daemon may be running).
fn db(data_dir: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(
        data_dir.join("threads.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}

fn attached_row(data_dir: &Path, cookie: &str) -> bool {
    db(data_dir)
        .query_row("SELECT COUNT(*) FROM attached_sessions WHERE cookie = ?1", [cookie], |r| r.get::<_, i64>(0))
        .unwrap()
        == 1
}

/// `(boot_id, daemon_pid)` of the thread's lock row, if any.
fn lock_row(data_dir: &Path, thread: &str) -> Option<(String, i64)> {
    use rusqlite::OptionalExtension;
    db(data_dir)
        .query_row(
            "SELECT boot_id, daemon_pid FROM thread_locks WHERE thread_id = ?1",
            [thread],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .unwrap()
}

fn ledger(data_dir: &Path, thread: &str) -> Ledger {
    Ledger::read_jsonl(&ledger_path(data_dir, thread)).unwrap()
}

fn turn_has(l: &Ledger, kind: Kind, cookie: &str) -> bool {
    l.events().iter().any(|e| e.kind == kind && e.turn.as_deref() == Some(cookie))
}

fn still_running(c: &mut Child) -> bool {
    matches!(c.try_wait(), Ok(None))
}

/// A wrapper stand-in that is killed and reaped when dropped, so a red run
/// does not leave a `sleep 600` behind.
struct Wrapper(Child);

impl Drop for Wrapper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn an_attached_session_survives_a_restart_alive_kept_dead_released_none_signalled() {
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path();
    let data_dir = scratch.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let (x, y) = fixture(scratch, &data_dir);
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();

    // The two wrappers: X's lives through the whole test, Y's dies with daemon 1.
    let mut wrapper_x = Wrapper(Command::new("/bin/sleep").arg("600").spawn().unwrap());
    let mut wrapper_y = Wrapper(Command::new("/bin/sleep").arg("600").spawn().unwrap());

    // ── DAEMON 1: attach both ──────────────────────────────────────────
    let p1 = free_port();
    let mut d1 = spawn_daemon(scratch, &data_dir, p1);
    let cookies = rt.block_on(async {
        let mut ws = connect(p1).await;
        let mut cookies = vec![];
        for (thread, wrapper) in [(&x, &wrapper_x.0), (&y, &wrapper_y.0)] {
            let r = request(&mut ws, thread, &format!("threads/{thread}/attach"), json!({"wrapper_pid": wrapper.id()})).await;
            assert_eq!(r["outcome"]["type"], json!("success"), "attach {thread} answered: {r}");
            cookies.push(r["outcome"]["payload"]["cookie"].as_str().unwrap().to_string());
        }
        cookies
    });
    let (cx, cy) = (cookies[0].clone(), cookies[1].clone());
    let d1_pid = d1.pid();
    d1.kill_hard();
    wrapper_y.0.kill().unwrap();
    wrapper_y.0.wait().unwrap();

    // SENTINELS: before the restart, both sessions are on disk, both threads
    // are locked by daemon 1's (now dead) pid, both ledgers are dirty.
    for (thread, cookie) in [(&x, &cx), (&y, &cy)] {
        assert!(attached_row(&data_dir, cookie), "sentinel: {thread}'s attached row exists before the restart");
        let (_, pid) = lock_row(&data_dir, thread).expect("sentinel: the attach locked the thread");
        assert_eq!(pid, d1_pid, "sentinel: {thread}'s lock is daemon 1's");
        let l = ledger(&data_dir, thread);
        assert!(turn_has(&l, Kind::TurnStarted, cookie), "sentinel: the attach opened a turn on {thread}");
        assert!(!turn_has(&l, Kind::TurnEnded, cookie), "sentinel: nothing closed {thread}'s turn");
        assert!(dirty_mark(&l).is_some(), "sentinel: {thread}'s ledger is dirty before the restart");
    }
    let d1_boot = lock_row(&data_dir, &x).unwrap().0;

    // ── DAEMON 2: the restart ─────────────────────────────────────────
    let p2 = free_port();
    let d2 = spawn_daemon(scratch, &data_dir, p2);
    let d2_pid = d2.pid();
    assert_ne!(d2_pid, d1_pid, "sentinel: two different daemon processes");

    // X — ALIVE: row kept, lock re-taken by daemon 2, never signalled.
    assert!(attached_row(&data_dir, &cx), "the live session's row survives recovery; log:\n{}", d2.log());
    let (boot, pid) = lock_row(&data_dir, &x)
        .unwrap_or_else(|| panic!("the live session's thread is LOCKED after recovery; log:\n{}", d2.log()));
    assert_eq!(pid, d2_pid, "the lock is re-taken by daemon 2");
    assert_ne!(boot, d1_boot, "under daemon 2's boot id");
    assert!(still_running(&mut wrapper_x.0), "recovery signalled the live wrapper");

    // Y — DEAD: row deleted, lock released, turn closed and reconciled.
    assert!(!attached_row(&data_dir, &cy), "the dead session's row is deleted; log:\n{}", d2.log());
    assert_eq!(lock_row(&data_dir, &y), None, "the dead session's thread is unlocked");
    let ly = ledger(&data_dir, &y);
    assert!(turn_has(&ly, Kind::TurnEnded, &cy), "the dead attach's turn is closed on the ledger");
    assert!(dirty_mark(&ly).is_none(), "the dead attach's turn is reconciled (ledger clean)");
    // Opposed: the LIVE session's ledger was left alone (still its open turn).
    let lx = ledger(&data_dir, &x);
    assert!(!turn_has(&lx, Kind::TurnEnded, &cx), "the live session's turn is not closed by recovery");

    // The kept lease is REAL in daemon 2: its detach releases it.
    let r = rt.block_on(async {
        let mut ws = connect(p2).await;
        request(&mut ws, "d", &format!("threads/{x}/detach"), json!({"cookie": cx})).await
    });
    assert_eq!(r["outcome"]["type"], json!("success"), "detach answered: {r}");
    assert_eq!(r["outcome"]["payload"]["released"], json!(true), "daemon 2 held the lease: {r}");
    assert_eq!(lock_row(&data_dir, &x), None, "the detach released the re-taken lock");
    assert!(still_running(&mut wrapper_x.0), "nothing signalled the wrapper");

    drop(wrapper_x);
    drop(d2);
}
