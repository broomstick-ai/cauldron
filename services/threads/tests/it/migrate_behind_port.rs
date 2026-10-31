//! **A STORE MIGRATION RUNS BEHIND THE SERVED PORT** (R9.C7h, implements C-7).
//!
//! The incident (2026-10-21, DEPLOY-9h): a fresh threads held its port but
//! answered nothing for 75.4 s, because R10.M1's one-time queue migration ran
//! inside `build_manager`, before the chassis was served. These tests boot
//! the REAL binary with a migration standing in for that one
//! (`THREADS_MIGRATION_FAULT`, a `test-seams` hook inside `Store::open` that
//! is announced exactly like a real migration) and assert:
//!
//! * (a) `/health` answers within [`BIND_BOUND`] with **503 `migrating`**
//!   while a 5 s migration runs, and the boot then reaches `ok`;
//! * (b) a store that needs NO migration never answers `migrating`, over its
//!   whole boot;
//! * (c) a migration that never finishes reads **503 `recovery_stuck`** past
//!   the shrunk stuck bound — and a migration that does finish under a bound
//!   it fits in never reads stuck; time spent migrating counts against the
//!   recovery bound;
//! * the REAL notice path: `Store::open_noticing` announces the R10.M1 queue
//!   migration on an old-format store, and announces nothing on a current one.
//!
//! (d), the holder's side, is `libs/env/tests/hosting.rs::
//! the_holder_reads_503_migrating_as_alive_not_ready_and_recovery_stuck_as_unhealthy`.
//!
//! Nothing here touches `:3653`, `~/.cauldron` or a real claude.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use threads::Store;

/// The bound the spec names: spawn → `/health` answered.
const BIND_BOUND: Duration = Duration::from_secs(2);

static SERIAL: Mutex<()> = Mutex::new(());

fn fake_claude() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"))
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
        .env_remove("THREADS_MIGRATION_FAULT")
        .env_remove("THREADS_RECOVERY_FAULT")
        .env_remove("THREADS_RECOVERY_STUCK_MS")
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

/// A scratch data dir whose store already exists at the current schema (so
/// the daemon's own open needs no migration of its own), and a warmed binary.
fn scratch_store() -> (tempfile::TempDir, PathBuf) {
    let scratch = tempfile::tempdir().unwrap();
    let data_dir = scratch.path().join("data");
    std::fs::create_dir_all(data_dir.join("aa-owned")).unwrap();
    std::fs::create_dir_all(scratch.path().join("fake-state")).unwrap();
    drop(Store::open(data_dir.join("threads.db")).unwrap());
    let warm = Command::new(env!("CARGO_BIN_EXE_threads")).arg("--build-stamp").output().unwrap();
    assert!(warm.status.success(), "warming the binary: {warm:?}");
    (scratch, data_dir)
}

/// R9.C7l: the pre-chassis `/health` answer while the store opens with no
/// migration announced (yet) — 200 `starting`, the door's own word.
fn is_starting(h: &(u16, Value)) -> bool {
    h.0 == 200 && h.1["status"] == "starting"
}

/// Every `/health` answer from spawn until `status: "ok"` (or `limit`), as
/// `(since spawn, code, body)`; `None` entries are probes nothing answered.
fn watch_boot(d: &Daemon, port: u16, t0: Instant, limit: Duration) -> Vec<(Duration, Option<(u16, Value)>)> {
    let mut seen = Vec::new();
    while t0.elapsed() < limit {
        let h = health(port, Duration::from_millis(250));
        let done = matches!(&h, Some((200, b)) if b["status"] == "ok");
        seen.push((t0.elapsed(), h));
        if done {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the boot never reached `ok` within {limit:?}; log:\n{}", d.log());
}

/// **(a) A 5 s MIGRATION: `/health` ANSWERS 503 `migrating` WITHIN 2 s.**
///
/// The clock starts at `spawn()` (first exec paid beforehand) and stops at the
/// first ANSWER of any code. Asserted:
/// * that first answer is inside [`BIND_BOUND`], and is 503 `migrating` with a
///   `migration` object — distinct from `recovering` (200) and
///   `recovery_stuck`;
/// * SENTINEL (doctrine 9): `migrating` is still being answered PAST the bound
///   — the migration really outlasted it, so a daemon that migrated before it
///   served could not have met it;
/// * the boot then goes on to `ok` through the existing recovery path.
///
/// Forced red: open the store before the serve in `run_daemon` (the C7g
/// order) — nothing answers until the 5 s migration is over.
#[test]
fn a_5s_store_migration_answers_503_migrating_within_2s_then_reaches_ok() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (scratch, data_dir) = scratch_store();
    let port = free_port();
    let t0 = Instant::now();
    let d = spawn_daemon(scratch.path(), &data_dir, port, &[("THREADS_MIGRATION_FAULT", "sleep:5000")]);
    let seen = watch_boot(&d, port, t0, Duration::from_secs(120));
    // R9.C7l: before the seam's notice, /health answers 200 `starting` at
    // once (the store is opening, nothing is announced yet). The claim here is
    // about the first answer that is NOT that one.
    let (first_at, first) = seen
        .iter()
        .find_map(|(t, h)| h.as_ref().filter(|h| !is_starting(h)).map(|h| (*t, h.clone())))
        .unwrap_or_else(|| panic!("/health never answered past `starting`; log:\n{}", d.log()));
    eprintln!("migrate_behind_port: spawn -> first /health answer in {first_at:?}: {first:?}");
    let last_migrating = seen
        .iter()
        .filter(|(_, h)| matches!(h, Some((503, b)) if b["status"] == "migrating"))
        .map(|(t, _)| *t)
        .last()
        .unwrap_or_default();
    // SENTINEL first: the fixture's migration outlasted the bound.
    assert!(
        last_migrating > BIND_BOUND,
        "fixture too light to discriminate: `migrating` last seen at {last_migrating:?}, not past {BIND_BOUND:?}"
    );
    // THE CLAIM.
    assert!(first_at < BIND_BOUND, "spawn -> first /health took {first_at:?}, over {BIND_BOUND:?}; log:\n{}", d.log());
    assert_eq!(first.0, 503, "{first:?}");
    assert_eq!(first.1["status"], "migrating", "{first:?}");
    assert_eq!(first.1["migration"]["state"], "migrating", "{first:?}");
    assert_eq!(first.1["migration"]["step"], "test seam (sleep)", "{first:?}");
    // Then the existing path: no answer is `recovery_stuck`, and it ends `ok`.
    assert!(
        seen.iter().all(|(_, h)| !matches!(h, Some((_, b)) if b["status"] == "recovery_stuck")),
        "a migration inside its bound never reads stuck"
    );
    let (_, last) = seen.last().unwrap();
    let last = last.as_ref().unwrap();
    assert_eq!(last.0, 200);
    assert!(last.1["recovery"]["migrated_ms"].as_u64().unwrap() >= 5000, "{}", last.1);
    assert!(d.log().contains("store migration done"), "{}", d.log());
}

/// **(b) THE OPPOSING ARM: A STORE THAT NEEDS NO MIGRATION NEVER ANSWERS
/// `migrating`**, over its whole boot — every probe from spawn until `ok`
/// (the longest period in this path is the boot itself, so it is waited out
/// in full) — and nothing logs a migration.
#[test]
fn a_store_needing_no_migration_never_answers_migrating_over_its_whole_boot() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (scratch, data_dir) = scratch_store();
    let port = free_port();
    let t0 = Instant::now();
    let d = spawn_daemon(scratch.path(), &data_dir, port, &[]);
    let seen = watch_boot(&d, port, t0, Duration::from_secs(120));
    let answered = seen.iter().filter(|(_, h)| h.is_some()).count();
    assert!(answered >= 1, "the instrument saw at least one answer");
    for (t, h) in &seen {
        if let Some((code, b)) = h {
            assert_ne!(b["status"], "migrating", "at {t:?}: {code} {b}");
            assert_eq!(*code, 200, "at {t:?}: {b}");
        }
    }
    assert!(!d.log().contains("store migration running"), "{}", d.log());
    let (_, last) = seen.last().unwrap();
    assert_eq!(last.as_ref().unwrap().1["recovery"]["migrated_ms"], 0);
}

/// The shrunk stuck bound (`THREADS_RECOVERY_STUCK_MS`) of the hang test.
const STUCK_BOUND_MS: u64 = 1500;

/// **(c) A MIGRATION THAT NEVER FINISHES READS 503 `recovery_stuck`** past the
/// stuck bound, shrunk to [`STUCK_BOUND_MS`]. Sentinel: it first answered 503
/// `migrating` — so the flip is the bound firing on a running migration, not a
/// boot that never served. Forced red: `Startup::migrating_answer` ignoring
/// the bound — it stays `migrating` forever.
#[test]
fn a_store_migration_hung_past_its_1500ms_bound_answers_503_recovery_stuck() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (scratch, data_dir) = scratch_store();
    let port = free_port();
    let bound = STUCK_BOUND_MS.to_string();
    let d = spawn_daemon(
        scratch.path(),
        &data_dir,
        port,
        &[("THREADS_MIGRATION_FAULT", "hang"), ("THREADS_RECOVERY_STUCK_MS", &bound)],
    );
    let t0 = Instant::now();
    let first = loop {
        if let Some(x) = health(port, Duration::from_millis(500)).filter(|h| !is_starting(h)) {
            break x;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "never answered past `starting`; log:\n{}", d.log());
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(first.0, 503, "{first:?}");
    assert_eq!(first.1["status"], "migrating", "{first:?}");
    let deadline = Instant::now() + Duration::from_millis(STUCK_BOUND_MS * 10);
    let last = loop {
        let h = health(port, Duration::from_millis(1000));
        if let Some((503, b)) = &h {
            if b["status"] == "recovery_stuck" {
                break b.clone();
            }
        }
        assert!(Instant::now() < deadline, "/health never read stuck for a migration hung past {STUCK_BOUND_MS} ms: {h:?}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(last["migration"]["state"], "stuck", "{last}");
    assert!(last["migration"]["elapsed_ms"].as_u64().unwrap() > STUCK_BOUND_MS, "stuck only past the bound: {last}");
}

/// **(c) OPPOSED: A 2500 ms MIGRATION UNDER A 6000 ms BOUND NEVER READS
/// STUCK** and reaches `ok` — so neither "stuck after any migration" nor a
/// bound that ignores its value passes the test above.
#[test]
fn a_2500ms_store_migration_under_a_6000ms_bound_never_reads_stuck() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (scratch, data_dir) = scratch_store();
    let port = free_port();
    let t0 = Instant::now();
    let d = spawn_daemon(
        scratch.path(),
        &data_dir,
        port,
        &[("THREADS_MIGRATION_FAULT", "sleep:2500"), ("THREADS_RECOVERY_STUCK_MS", "6000")],
    );
    let seen = watch_boot(&d, port, t0, Duration::from_secs(60));
    assert!(
        seen.iter().any(|(_, h)| matches!(h, Some((503, b)) if b["status"] == "migrating")),
        "sentinel: the migration was seen running"
    );
    for (t, h) in &seen {
        if let Some((_, b)) = h {
            assert_ne!(b["status"], "recovery_stuck", "at {t:?}: {b}");
        }
    }
}

/// **THE STUCK BOUND COVERS MIGRATE + RECOVER TOGETHER.** A 2000 ms migration,
/// then a recovery that hangs, under a 3000 ms bound: `recovery_stuck` arrives
/// when RECOVERY has run only ~1000 ms of it (`recovery.elapsed_ms` < the
/// bound), because the migration's 2000 ms is carried. The C7g test
/// `a_recovery_hung_past_its_1500ms_bound_answers_503_recovery_stuck` is the
/// opposing arm (no migration: stuck only once recovery ALONE passes the
/// bound). Forced red: `recovery().begin()` instead of `begin_after`.
#[test]
fn time_spent_migrating_counts_against_the_3000ms_recovery_stuck_bound() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (scratch, data_dir) = scratch_store();
    let port = free_port();
    let d = spawn_daemon(
        scratch.path(),
        &data_dir,
        port,
        &[
            ("THREADS_MIGRATION_FAULT", "sleep:2000"),
            ("THREADS_RECOVERY_FAULT", "hang"),
            ("THREADS_RECOVERY_STUCK_MS", "3000"),
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let stuck = loop {
        if let Some((503, b)) = health(port, Duration::from_millis(1000)) {
            if b["status"] == "recovery_stuck" && b["recovery"].is_object() {
                break b;
            }
        }
        assert!(Instant::now() < deadline, "never read recovery_stuck; log:\n{}", d.log());
        std::thread::sleep(Duration::from_millis(50));
    };
    let rec = &stuck["recovery"];
    assert!(rec["migrated_ms"].as_u64().unwrap() >= 2000, "{stuck}");
    assert!(rec["elapsed_ms"].as_u64().unwrap() < 3000, "stuck before recovery alone used the bound: {stuck}");
}

/// **THE REAL NOTICE PATH.** `Store::open_noticing` announces R10.M1's queue
/// migration on an old-format store (the `dispatch`/`attempts` columns
/// absent — the queue table rebuilt in its pre-M1 shape) BEFORE it runs, and announces nothing on a
/// store at the current schema. Opposed both ways in one test.
#[test]
fn opening_an_old_format_store_announces_the_queue_migration_and_a_current_one_announces_nothing() {
    let scratch = tempfile::tempdir().unwrap();
    let db = scratch.path().join("threads.db");
    drop(Store::open(&db).unwrap());
    let heard = std::cell::RefCell::new(Vec::<&'static str>::new());
    drop(Store::open_noticing(&db, &|s| heard.borrow_mut().push(s)).unwrap());
    assert!(heard.borrow().is_empty(), "a current store announced {:?}", heard.borrow());

    Store::open(&db)
        .unwrap()
        .execute_sql(
            "DROP TABLE queue;
             CREATE TABLE queue (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, thread_id TEXT NOT NULL,
                 message_id INTEGER NOT NULL, text TEXT NOT NULL, intent TEXT NOT NULL,
                 state TEXT NOT NULL DEFAULT 'waiting', created_at TEXT NOT NULL,
                 delivered_at TEXT, metadata_json TEXT, headers_json TEXT);",
        )
        .unwrap();
    drop(Store::open_noticing(&db, &|s| heard.borrow_mut().push(s)).unwrap());
    assert_eq!(*heard.borrow(), vec!["queue two records"]);

    heard.borrow_mut().clear();
    drop(Store::open_noticing(&db, &|s| heard.borrow_mut().push(s)).unwrap());
    assert!(heard.borrow().is_empty(), "migrated once, announced again: {:?}", heard.borrow());
}

// R9.F2b: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
