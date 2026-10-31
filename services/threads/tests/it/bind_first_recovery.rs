//! **THREADS ANSWERS `/health` BEFORE IT RESUMES ITS CHILDREN** (R9.C7g,
//! implements C-7).
//!
//! The incident (2026-10-20, DEPLOY-9g): a fresh threads replayed 252 "child
//! resumed from its ledger" records before it served its port, and for ~60 s
//! nothing could connect. These tests build that boot on purpose — a data dir
//! whose ledgers commit [`CHILDREN`] children the store has never seen, so
//! boot recovery must materialize every one — and then:
//!
//! 1. `bind_and_health_answer_within_2s_with_500_children_to_resume` — the
//!    REAL binary answers `/health` 200 within [`BIND_BOUND`] of being
//!    spawned, while recovery is still running (`status: "recovering"`,
//!    `recovery.children_pending` named), and later reports `ok` with every
//!    child resumed. Its sentinel is the recovery's own measured length: it
//!    must be LONGER than the bound, or the fixture could not have told
//!    bind-first from resume-first and the test would prove nothing.
//!    Forced red: move `manager.recover().await` back ahead of the serve in
//!    `run_daemon` (report, item 5).
//! 2. `a_request_needing_recovery_waits_or_is_refused_busy_never_answered_wrong`
//!    — during recovery, `threads/list` is either refused `busy` (bound 0)
//!    or answered only once recovery is done, with ALL the children in it
//!    (bound long); and `daemon/image`, which recovery cannot change, is
//!    answered at once during recovery (the opposite arm: not everything
//!    waits).
//!
//! Nothing here touches `:3653`, `~/.cauldron` or a real claude.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shapewire::envelope::codes;
use shapewire::mcp::McpTools;
use shapewire::server::Producer;
use shapewire::token::Claims;
use threads::config::Config;
use threads::managed::Halt;
use threads::{agents, build_manager, ThreadManager};

/// Children committed in the fixture's ledgers with no thread row.
const CHILDREN: usize = 500;
/// The bound the spec names: spawn → `/health` answered 200.
const BIND_BOUND: Duration = Duration::from_secs(2);

static SERIAL: Mutex<()> = Mutex::new(());

/// The pre-chassis `/health` status (R9.C7l): alive, opening the store.
const STARTING: &str = "starting";

fn fake_claude() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"))
}

fn set_env(tmp: &Path) {
    std::env::set_var("CAULDRON_HOME", tmp.join("cauldron"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var("FAKE_STATE_DIR", tmp.join("fake-state"));
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::remove_var("THREADS_CHASSIS_PORT");
    std::fs::create_dir_all(tmp.join("fake-state")).unwrap();
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

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

/// **THE FIXTURE.** One parent and [`CHILDREN`] spawns, each killed at the
/// commit point (`Halt::AfterCommit`): the parent's ledger names the child and
/// the child's ledger exists, and the store has no row for it. That is exactly
/// what boot recovery's first managed pass (`managed::recover` →
/// `materialize`) exists to finish. Returns `(parent, children)`.
fn fixture(dir: &Path) -> (String, Vec<String>) {
    let rt = runtime();
    let (parent, kids) = rt.block_on(async {
        threads::clock::testing::ticking();
        let m = build_manager(config_at(dir)).unwrap();
        let settings = json!({"cwd": dir.to_string_lossy(), "model": "sonnet", "account": {"named": "aa-owned"}});
        let parent = m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap();
        let claims = Claims {
            thread_id: parent.clone(),
            run_id: format!("{parent}/run-1"),
            root: parent.clone(),
            depth: 0,
            turn: "cookie".into(),
        };
        for i in 0..CHILDREN {
            m.managed().halt_after(Some(Halt::AfterCommit));
            let err = agents::call(&m, &claims, "agents_spawn", json!({"task": format!("child {i}"), "request_id": format!("req-{i}")}))
                .await
                .expect_err("the spawn stops at the commit point");
            assert!(err.message.contains("halted"), "{}", err.message);
        }
        let kids = children_named(dir, &parent);
        (parent, kids)
    });
    rt.shutdown_background();
    assert_eq!(kids.len(), CHILDREN, "the parent's ledger commits every child");
    (parent, kids)
}

/// The children the parent's ledger names, read off the file.
fn children_named(dir: &Path, parent: &str) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join("ledgers").join(format!("{parent}.jsonl"))).unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["body"]["phase"] == "started")
        .filter_map(|v| v["body"]["child_id"].as_str().map(str::to_string))
        .collect()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// One `GET /health`: `Some((status, json body))`, `None` if nothing answered
/// within `timeout`.
fn health(port: u16, timeout: Duration) -> Option<(u16, Value)> {
    let sa: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut s = TcpStream::connect_timeout(&sa, timeout).ok()?;
    s.set_read_timeout(Some(timeout)).ok()?;
    s.write_all(format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes())
        .ok()?;
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).to_string();
    let code = text.split_whitespace().nth(1)?.parse().ok()?;
    let body = text.split("\r\n\r\n").nth(1).and_then(|b| serde_json::from_str(b).ok()).unwrap_or(Value::Null);
    Some((code, body))
}

struct Daemon {
    child: Child,
    log: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("/bin/kill").args(["-KILL", &self.child.id().to_string()]).status();
        let _ = self.child.wait();
    }
}

impl Daemon {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn spawn_daemon(scratch: &Path, data_dir: &Path, port: u16, extra: &[(&str, &str)]) -> Daemon {
    let cfg = scratch.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "host = \"127.0.0.1\"\nchassis_port = {port}\ndata_dir = \"{dd}\"\nclaude_bin = \"{fc}\"\n\
             calibrate = false\nedge_poll = false\nprobe_interval_secs = 0\nprobe_interval_busy_secs = 0\n\
             [accounts]\naa-owned = \"{dd}/aa-owned\"\n",
            dd = data_dir.display(),
            fc = fake_claude().display()
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
        .env("CAULDRON_HOME", scratch.join("cauldron-home"))
        .env("THREADS_CLAUDE_BIN", fake_claude())
        .env("THREADS_CLAUDE_HOME", scratch.join("claude-home"))
        .env("HOME", scratch.join("home"))
        .env("FAKE_STATE_DIR", scratch.join("fake-state"))
        .env("RUST_LOG", "info")
        .envs(extra.iter().copied())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .expect("spawn the threads binary");
    Daemon { child, log }
}

/// **BIND + `/health` IN < 2 s WITH 500 CHILDREN TO RESUME.**
///
/// The clock starts at `spawn()` of the real binary (its first exec is paid
/// beforehand, untimed — `--build-stamp` reads and writes nothing) and stops at
/// the first `/health` that answers 200.
///
/// Three things are asserted, each opposed:
/// * the first 200 lands inside [`BIND_BOUND`] — a daemon that resumes before
///   it serves is red here (measured: report item 5);
/// * that first 200 says `status: "recovering"` with a `recovery` object — so
///   it really was answered BEFORE recovery finished, not after a fast one;
/// * recovery then finishes (`status: "ok"`, every child resumed) and its own
///   measured length is LONGER than the bound — the sentinel that this fixture
///   could have told the two orders apart. Without it, a fixture that recovers
///   in 300 ms would pass a resume-first daemon too.
#[test]
fn bind_and_health_answer_within_2s_with_500_children_to_resume() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let data_dir = scratch.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(scratch.path().join("fake-state")).unwrap();
    set_env(scratch.path());
    let (_parent, kids) = fixture(&data_dir);

    // Pay the binary's first exec outside the timed window.
    let warm = Command::new(env!("CARGO_BIN_EXE_threads")).arg("--build-stamp").output().unwrap();
    assert!(warm.status.success(), "warming the binary: {warm:?}");

    let port = free_port();
    let t0 = Instant::now();
    let d = spawn_daemon(scratch.path(), &data_dir, port, &[]);
    // R9.C7l: while the store opens, /health answers 200 `starting` at once;
    // the claim is about the first 200 that comes from the chassis.
    let mut first: Option<(Duration, Value)> = None;
    while t0.elapsed() < Duration::from_secs(120) {
        if let Some((200, body)) = health(port, Duration::from_millis(250)) {
            if body["status"] != STARTING {
                first = Some((t0.elapsed(), body));
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let (took, body) = first.unwrap_or_else(|| panic!("/health never answered 200; log:\n{}", d.log()));
    eprintln!("bind_first: spawn -> first /health 200 in {took:?}; body {body}");

    // Recovery then completes, and says how long it took.
    let deadline = Instant::now() + Duration::from_secs(300);
    let done = loop {
        if let Some((200, b)) = health(port, Duration::from_millis(1500)) {
            if b["status"] == "ok" && b["recovery"]["state"] == "done" {
                break b;
            }
        }
        assert!(Instant::now() < deadline, "recovery never finished; log tail:\n{}", d.log());
        std::thread::sleep(Duration::from_millis(100));
    };
    eprintln!("bind_first: recovery done: {}", done["recovery"]);
    let rec_ms = done["recovery"]["elapsed_ms"].as_u64().unwrap();
    assert_eq!(done["recovery"]["children_total"], json!(kids.len()), "recovery walked every committed child");
    assert_eq!(done["recovery"]["children_resumed"], json!(kids.len()));
    assert_eq!(done["recovery"]["children_pending"], json!(0));
    let resumed = d.log().matches("child resumed from its ledger").count();
    assert_eq!(resumed, kids.len(), "every child really materialized (log lines)");

    // SENTINEL (doctrine 9): the recovery this daemon did is longer than the
    // bound, so a daemon that did it BEFORE serving could not meet the bound.
    assert!(
        Duration::from_millis(rec_ms) > BIND_BOUND,
        "fixture too light to discriminate: recovery took {rec_ms} ms, not more than {BIND_BOUND:?}"
    );

    // THE CLAIM.
    assert!(
        took < BIND_BOUND,
        "spawn -> /health 200 took {took:?}, over the {BIND_BOUND:?} bound with {} children to resume \
         (first body {body}); log tail:\n{}",
        kids.len(),
        d.log().lines().rev().take(20).collect::<Vec<_>>().join("\n")
    );
    assert_eq!(
        body["status"], "recovering",
        "the first 200 was answered while recovery ran, and says so: {body}"
    );
    assert_eq!(body["recovery"]["state"], "recovering", "{body}");
    assert!(body["recovery"]["children_pending"].is_u64(), "names what is still resuming: {body}");
}

/// **A REQUEST THAT NEEDS RECOVERY STATE DURING RECOVERY: WAIT OR `busy`,
/// NEVER A WRONG ANSWER.** In process, over the same fixture, with recovery
/// started in the background exactly as `run_daemon` starts it.
///
/// * wait bound 0 → `threads/list` is refused `busy` at once, while recovery
///   is still running (asserted on both sides of the call — the sentinel that
///   the refusal is about recovery and not a fast one that already finished);
/// * `daemon/image` is answered during recovery, at once (not everything waits);
/// * wait bound 120 s → `threads/list` answers only after recovery is done,
///   and the answer lists every child. Forced red: drop the gate in
///   `ThreadsProducer::handle` — the list comes back early and short.
#[test]
fn a_request_needing_recovery_waits_or_is_refused_busy_never_answered_wrong() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("data");
    std::fs::create_dir_all(&dir).unwrap();
    set_env(scratch.path());
    let (parent, kids) = fixture(&dir);

    let rt = runtime();
    rt.block_on(async {
        threads::clock::testing::ticking();
        let m: Arc<ThreadManager> = Arc::new(build_manager(config_at(&dir)).unwrap());
        let p = threads::chassis::ThreadsProducer::new(Arc::clone(&m));
        let rec_mgr = Arc::clone(&m);
        m.update.recovery().begin();
        let task = tokio::spawn(async move {
            rec_mgr.recover().await;
            rec_mgr.update.recovery().finish();
        });

        // ── bound 0: refused busy, during recovery.
        m.update.recovery().set_wait(Duration::ZERO);
        assert!(m.update.recovery().is_recovering(), "premise: recovery is running");
        let r = p.handle("threads/list", json!({"limit": 5000})).await;
        let still = m.update.recovery().is_recovering();
        match &r {
            Err(e) => {
                assert_eq!(e.code, codes::BUSY, "{}", e.message);
                assert!(e.message.contains("recovering"), "{}", e.message);
            }
            Ok(v) => panic!("threads/list answered during recovery with a zero wait: {} rows", v["threads"].as_array().map(|a| a.len()).unwrap_or(0)),
        }
        assert!(still, "premise: recovery was still running when the refusal came back");

        // ── r37 (inspect d): THE MCP DOOR gates too. Same bound 0, the
        // parent's own run claims: `agents_list` is refused `busy` while
        // recovery runs. Forced red: drop the gate in `McpTools::call`.
        let claims = Claims {
            thread_id: parent.clone(),
            run_id: format!("{parent}/run-1"),
            root: parent.clone(),
            depth: 0,
            turn: "cookie".into(),
        };
        let r = McpTools::call(&p, &claims, "agents_list", json!({})).await;
        let still = m.update.recovery().is_recovering();
        match &r {
            Err(e) => assert_eq!(e.code, codes::BUSY, "{}", e.message),
            Ok(v) => panic!("agents_list answered through the MCP door during recovery with a zero wait: {v}"),
        }
        assert!(still, "premise: recovery was still running when the MCP refusal came back");

        // ── r37 (inspect a): the history of a child recovery has NOT yet
        // materialized is refused `busy`, not answered `[]`. Sentinel: the
        // store really has no row for it at the moment of the call.
        let late = kids.last().unwrap().clone();
        assert!(m.store.get_thread(&late).unwrap().is_none(), "premise: {late} is not materialized yet");
        let r = p.handle("threads/{thread_id}/history", json!({"thread_id": late})).await;
        let still = m.update.recovery().is_recovering();
        match &r {
            Err(e) => assert_eq!(e.code, codes::BUSY, "{}", e.message),
            Ok(v) => panic!("history of an unmaterialized child answered during recovery: {v}"),
        }
        assert!(still, "premise: recovery was still running when the history refusal came back");

        // ── the opposite arm: a read recovery cannot change is served now.
        // (With the 120 s bound set FIRST, so a daemon/image that waited would
        // come back only after recovery: "still recovering" afterwards is the
        // proof it did not wait.)
        m.update.recovery().set_wait(Duration::from_secs(120));
        let img = p.handle("daemon/image", json!({})).await;
        assert!(img.is_ok(), "daemon/image during recovery: {:?}", img.err().map(|e| e.message));
        assert!(m.update.recovery().is_recovering(), "daemon/image was answered DURING recovery, without waiting for it");
        // …and so is the history of a thread the store already has (the
        // opposite arm of the history gate: not every history waits).
        let h = p.handle("threads/{thread_id}/history", json!({"thread_id": parent})).await;
        assert!(h.is_ok(), "the parent's history during recovery: {:?}", h.err().map(|e| e.message));
        assert!(m.update.recovery().is_recovering(), "the parent's history was answered DURING recovery, without waiting");

        // ── long bound: answered only once done, and complete.
        let v = p.handle("threads/list", json!({"limit": 5000})).await.expect("threads/list after waiting");
        assert!(!m.update.recovery().is_recovering(), "answered only after recovery finished");
        let ids: std::collections::BTreeSet<String> = v["threads"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["id"].as_str().or_else(|| t["thread_id"].as_str()).map(str::to_string))
            .collect();
        assert!(ids.contains(&parent), "the parent is listed");
        let missing: Vec<&String> = kids.iter().filter(|k| !ids.contains(*k)).collect();
        assert!(missing.is_empty(), "{} of {} children missing from a post-recovery list", missing.len(), kids.len());
        // After recovery both doors answer (the gates are not "always busy").
        let h = p.handle("threads/{thread_id}/history", json!({"thread_id": late})).await;
        assert!(h.is_ok(), "the late child's history after recovery: {:?}", h.err().map(|e| e.message));
        let a = McpTools::call(&p, &claims, "agents_list", json!({})).await;
        assert!(a.is_ok(), "agents_list after recovery: {:?}", a.err().map(|e| e.message));
        task.await.unwrap();
    });
    rt.shutdown_background();
}

// ── r37 (doctrine 10): A RECOVERY THAT NEVER FINISHES IS NOT HEALTHY ──────

/// The shrunk stuck bound the next three tests run under (`THREADS_RECOVERY_STUCK_MS`).
const STUCK_BOUND_MS: u64 = 1500;

fn empty_boot(extra: &[(&str, &str)]) -> (tempfile::TempDir, Daemon, u16) {
    let scratch = tempfile::tempdir().unwrap();
    let data_dir = scratch.path().join("data");
    std::fs::create_dir_all(data_dir.join("aa-owned")).unwrap();
    std::fs::create_dir_all(scratch.path().join("fake-state")).unwrap();
    let port = free_port();
    let d = spawn_daemon(scratch.path(), &data_dir, port, extra);
    (scratch, d, port)
}

/// **A PANIC IN RECOVERY EXITS THE PROCESS NON-ZERO** (so the holder relaunches
/// it), instead of leaving `/health` at 200 `recovering` forever. Forced via
/// the `test-seams` fault `THREADS_RECOVERY_FAULT=panic`. Forced red: the
/// recovery watcher's panic arm without its `exit` — the process lives on,
/// answering 200 `recovering`. Opposed by
/// `a_normal_recovery_stays_200_past_the_1500ms_stuck_bound_then_reports_ok`,
/// whose process is still alive after recovery.
#[test]
fn a_panic_in_recovery_exits_the_daemon_non_zero() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (_s, mut d, port) = empty_boot(&[("THREADS_RECOVERY_FAULT", "panic")]);
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(st) = d.child.try_wait().unwrap() {
            break st;
        }
        assert!(
            Instant::now() < deadline,
            "a daemon whose recovery panicked is still running 60 s later (/health {:?}); log:\n{}",
            health(port, Duration::from_millis(500)),
            d.log()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    // Sentinel: it was the seam's panic, not some other early exit.
    assert!(d.log().contains("boot recovery panicked (test seam)"), "{}", d.log());
    assert_eq!(status.code(), Some(threads::recovery::PANIC_EXIT_CODE), "{status:?}; log:\n{}", d.log());
}

/// **A RECOVERY THAT HANGS PAST ITS BOUND READS 503 `recovery_stuck`.** The
/// period outlasted is the stuck bound itself, shrunk to [`STUCK_BOUND_MS`].
/// Sentinel: the first answer was 200 `recovering` — the daemon was serving
/// and recovery was running, so the 503 is the bound firing, not a boot that
/// never came up. Forced red: the liveness guard's stuck branch disabled —
/// `/health` stays 200 `recovering` forever.
#[test]
fn a_recovery_hung_past_its_1500ms_bound_answers_503_recovery_stuck() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let bound = STUCK_BOUND_MS.to_string();
    let (_s, d, port) = empty_boot(&[("THREADS_RECOVERY_FAULT", "hang"), ("THREADS_RECOVERY_STUCK_MS", &bound)]);
    let t0 = Instant::now();
    let first = loop {
        if let Some(x) = health(port, Duration::from_millis(500)).filter(|(c, b)| !(*c == 200 && b["status"] == STARTING)) {
            break x;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "never answered past `starting`; log:\n{}", d.log());
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(first.0, 200, "{first:?}");
    assert_eq!(first.1["status"], "recovering", "{first:?}");
    let deadline = Instant::now() + Duration::from_millis(STUCK_BOUND_MS * 10);
    let last = loop {
        let h = health(port, Duration::from_millis(1000));
        if let Some((503, b)) = &h {
            break b.clone();
        }
        assert!(Instant::now() < deadline, "/health never left 200 for a recovery hung past {STUCK_BOUND_MS} ms: {h:?}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(last["status"], "recovery_stuck", "{last}");
    assert_eq!(last["recovery"]["state"], "stuck", "{last}");
    assert!(
        last["recovery"]["elapsed_ms"].as_u64().unwrap() > STUCK_BOUND_MS,
        "stuck only past the bound: {last}"
    );
}

/// **THE OPPOSITE ARM: A NORMAL RECOVERY UNDER THE SAME SHRUNK BOUND NEVER
/// READS STUCK.** It answers 200 throughout, reports `ok` once done, and is
/// still 200 `ok` and still running for 3× the 1500 ms stuck bound after
/// that — so neither "always 503 after the bound" nor "exit on every
/// recovery" passes.
#[test]
fn a_normal_recovery_stays_200_past_the_1500ms_stuck_bound_then_reports_ok() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let bound = STUCK_BOUND_MS.to_string();
    let (_s, mut d, port) = empty_boot(&[("THREADS_RECOVERY_STUCK_MS", &bound)]);
    let t0 = Instant::now();
    let mut done_at: Option<Instant> = None;
    loop {
        if let Some((code, b)) = health(port, Duration::from_millis(1000)) {
            assert_eq!(code, 200, "a normal recovery's /health: {b}");
            assert_ne!(b["recovery"]["state"], "stuck", "{b}");
            if b["status"] == "ok" && b["recovery"]["state"] == "done" && done_at.is_none() {
                done_at = Some(Instant::now());
            }
        }
        if let Some(at) = done_at {
            if at.elapsed() > Duration::from_millis(STUCK_BOUND_MS * 3) {
                break;
            }
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "recovery never reported done; log:\n{}", d.log());
        assert!(d.child.try_wait().unwrap().is_none(), "the daemon exited after a normal recovery; log:\n{}", d.log());
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(d.child.try_wait().unwrap().is_none(), "the daemon is still running");
}

// ── R9.C7l: /health ANSWERS WHILE A QUIET STORE OPEN RUNS ─────────────────

/// One probe of the chassis: what came back, and when.
#[derive(Debug, Clone)]
enum Probe {
    /// Nothing listening (connect refused / failed).
    Refused,
    /// Connected, request sent, no status line within the timeout.
    Held,
    /// An HTTP answer: code and JSON body (Null if not JSON).
    Answer(u16, Value),
}

/// `GET <path>` on the chassis with `timeout` on connect and read.
fn probe(port: u16, path: &str, timeout: Duration) -> Probe {
    let sa: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let Ok(mut s) = TcpStream::connect_timeout(&sa, timeout) else { return Probe::Refused };
    s.set_read_timeout(Some(timeout)).unwrap();
    if s
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes())
        .is_err()
    {
        return Probe::Refused;
    }
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).to_string();
    let Some(code) = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()) else { return Probe::Held };
    let body = text.split("\r\n\r\n").nth(1).and_then(|b| serde_json::from_str(b).ok()).unwrap_or(Value::Null);
    Probe::Answer(code, body)
}

/// The quiet store open the seam holds (`THREADS_MIGRATION_FAULT=quiet-sleep:`).
const QUIET_OPEN_MS: u64 = 1500;

/// **A PLANNED HANDOFF KEEPS THE CHASSIS `/health` ANSWERING (R9.C7l).**
///
/// DEPLOY-9k: the new process bound its chassis and then held every `/health`
/// for the 1.46 s its store open took (no migration announced, so the old
/// pre-chassis router WAITED), and the chassis read down for 2.21 s. Here the
/// `test-seams` fault `quiet-sleep:1500` holds `Store::open` for 1.5 s with NO
/// notice; `/health` is polled every 50 ms (200 ms timeout) from spawn until
/// `ok`, and:
///
/// * SENTINEL: the chassis was not ready for ≥ 1.2 s after the bind (its
///   first non-`starting` answer) — the seam really held the open, so a
///   router that waits for the chassis could not have passed;
/// * the first probe that CONNECTED (= the bind) was answered, within 100 ms;
/// * the largest gap between consecutive answers, bind → `ok`, is < 200 ms;
/// * OPPOSED: a non-/health request sent while `starting` is NOT answered
///   (held past 400 ms, `/health` still `starting` after it), and the same
///   request IS answered once the chassis is ready — so not every route
///   answers early, and the probe could have seen an answer.
///
/// Forced red: delete the /health branch in `startup.rs::answer` (report).
#[test]
fn health_answers_starting_within_100ms_of_bind_while_a_quiet_1500ms_store_open_runs() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let quiet = format!("quiet-sleep:{QUIET_OPEN_MS}");
    let (_s, d, port) = empty_boot(&[("THREADS_MIGRATION_FAULT", &quiet)]);
    let t0 = Instant::now();
    // (sent at, answered at, probe)
    let mut seen: Vec<(Duration, Duration, Probe)> = Vec::new();
    let mut other: Option<std::thread::JoinHandle<(Duration, Probe, Duration)>> = None;
    loop {
        let sent = t0.elapsed();
        let p = probe(port, "/health", Duration::from_millis(200));
        let got = t0.elapsed();
        let is_starting = matches!(&p, Probe::Answer(200, b) if b["status"] == STARTING);
        let done = matches!(&p, Probe::Answer(200, b) if b["status"] == "ok");
        seen.push((sent, got, p));
        // The opposing arm, off this loop so it does not open a gap in it.
        if is_starting && other.is_none() {
            other = Some(std::thread::spawn(move || {
                let sent = t0.elapsed();
                let p = probe(port, "/no-such-route", Duration::from_millis(400));
                (sent, p, t0.elapsed())
            }));
        }
        if done {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "never reached `ok`: {seen:?}\nlog:\n{}", d.log());
        std::thread::sleep(Duration::from_millis(50));
    }
    let answered: Vec<(Duration, Duration, &Value)> = seen
        .iter()
        .filter_map(|(s, g, p)| match p {
            Probe::Answer(_, b) => Some((*s, *g, b)),
            _ => None,
        })
        .collect();
    let starting: Vec<Duration> =
        answered.iter().filter(|(_, _, b)| b["status"] == STARTING).map(|(_, g, _)| *g).collect();

    // The bind: the first probe that found something listening.
    let (bind_sent, bind_got, bind_probe) =
        seen.iter().find(|(_, _, p)| !matches!(p, Probe::Refused)).cloned().expect("something connected");
    let span = match (starting.first(), starting.last()) {
        (Some(a), Some(b)) => *b - *a,
        _ => Duration::ZERO,
    };

    // SENTINEL (doctrine 9), independent of the fix: the chassis itself was
    // not ready for ≥ 1.2 s after the bind (first answer that is not
    // `starting`), so a router that waits for it would have left a hole.
    let chassis_at = answered
        .iter()
        .find(|(_, _, b)| b["status"] != STARTING)
        .map(|(_, g, _)| *g)
        .expect("the chassis answered at the end");
    assert!(
        chassis_at - bind_sent >= Duration::from_millis(1200),
        "fixture too light: the chassis was ready {:?} after the bind (need ≥ 1.2 s); probes {seen:?}",
        chassis_at - bind_sent
    );

    // THE CLAIM 1: no hole, bind → ok.
    // Anchored at the bind: a probe held from the bind is a hole from there.
    let times: Vec<Duration> = std::iter::once(bind_sent).chain(answered.iter().map(|(_, g, _)| *g)).collect();
    let gap = times.windows(2).map(|w| w[1] - w[0]).max().unwrap_or_default();
    eprintln!("C7l: {} answers, largest gap {gap:?}", answered.len());
    assert!(
        gap < Duration::from_millis(200),
        "a {gap:?} hole between /health answers across the boot; probes {seen:?}"
    );
    // THE CLAIM 2: the bind is answered at once.
    let took = bind_got - bind_sent;
    eprintln!("C7l: first connected probe at {bind_sent:?} answered in {took:?}: {bind_probe:?}");
    eprintln!("C7l: chassis ready {:?} after the bind; `starting` across {span:?}", chassis_at - bind_sent);
    assert!(
        matches!(&bind_probe, Probe::Answer(200, b) if b["status"] == STARTING),
        "the first probe that reached the bound chassis was not answered 200 `starting`: {bind_probe:?}"
    );
    assert!(took < Duration::from_millis(100), "the bind was answered after {took:?}, not within 100 ms");

    for (s, _, p) in seen.iter().skip_while(|(_, _, p)| matches!(p, Probe::Refused)) {
        assert!(matches!(p, Probe::Answer(200, _)), "after the bind, a probe at {s:?} got {p:?}");
    }

    // OPPOSED: a non-/health request is held, not answered as ready.
    let (o_sent, o_probe, o_done) = other.expect("the opposing request was sent").join().unwrap();
    eprintln!("C7l: /no-such-route sent at {o_sent:?} pre-ready -> {o_probe:?} at {o_done:?}");
    assert!(matches!(o_probe, Probe::Held), "a non-/health request was answered before the chassis: {o_probe:?}");
    assert!(
        starting.last().is_some_and(|l| *l >= o_done),
        "the hold must be bracketed by `starting` on both sides (last starting {:?}, request ended {o_done:?})",
        starting.last()
    );
    // ...and answered once ready: the probe could have seen an answer.
    let after = probe(port, "/no-such-route", Duration::from_millis(1000));
    assert!(matches!(after, Probe::Answer(..)), "the chassis never answers that route at all: {after:?}");
}

// R9.F2b: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
