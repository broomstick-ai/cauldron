//! **`/health` SAYS 503 WHEN THE STORE CANNOT BE WRITTEN** (R9.C6, gap C-6).
//!
//! Against the REAL binary on a throwaway data dir and a kernel port. The
//! wedge is made from outside, the way it looks from inside: another
//! connection takes SQLite's write lock on this daemon's `threads.db` and
//! holds it, so every write the daemon attempts waits out its `busy_timeout`
//! and fails — the heartbeat's `Store::ping` after its own short
//! `PING_BUSY_MS` (R9.C6b), so that it does not hold the write mutex for 5 s.
//!
//! Claims, each opposed:
//!
//! 1. HEALTHY ⇒ 200, held for three whole bounds: a heartbeat that never beat,
//!    or a layer that always said 503, is red here;
//! 2. WRITE LOCK HELD ⇒ 503 within the declared bound (+ one heartbeat period
//!    + the store's 5 s `busy_timeout` + slack), with the stale body — while a
//!    plain TCP connect to the same port still SUCCEEDS, which is exactly why
//!    `probe = "connect"` could never see this;
//! 3. LOCK RELEASED ⇒ 200 again: the 503 is a reading of the store, not a
//!    latch.
//!
//! Nothing here touches `:3653`, `~/.cauldron` or a real claude.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The bound the daemon under test runs with (`THREADS_HEALTH_STALE_SECS`).
const BOUND_SECS: u64 = 3;
/// `liveness::HEARTBEAT_PERIOD`.
const PERIOD_SECS: u64 = 2;
/// The store connection's `busy_timeout`. Since R9.C6b a ping blocked by the
/// lock fails after `PING_BUSY_MS` (250 ms) instead; this stays the slack for
/// any other write the daemon has in flight when the lock is taken.
const BUSY_SECS: u64 = 5;

fn fake_claude() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// One `GET /health`: `Some((status, body))`, or `None` if nothing answered
/// within 1.5 s (cauldron's own `PROBE_TIMEOUT`).
fn health(port: u16) -> Option<(u16, String)> {
    let sa: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut s = TcpStream::connect_timeout(&sa, Duration::from_millis(1500)).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(1500))).ok()?;
    s.write_all(format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes())
        .ok()?;
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).to_string();
    let code = text.split_whitespace().nth(1)?.parse().ok()?;
    Some((code, text))
}

fn connects(port: u16) -> bool {
    let sa: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    TcpStream::connect_timeout(&sa, Duration::from_millis(1500)).is_ok()
}

struct Daemon {
    child: Child,
    port: u16,
    log: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("/bin/kill").args(["-KILL", &self.child.id().to_string()]).status();
        let _ = self.child.wait();
    }
}

impl Daemon {
    fn boot(scratch: &Path, data_dir: &Path) -> Daemon {
        let port = free_port();
        let cfg = scratch.join("config.toml");
        std::fs::write(
            &cfg,
            format!(
                "host = \"127.0.0.1\"\nchassis_port = {port}\ndata_dir = \"{}\"\nclaude_bin = \"{}\"\n\
                 calibrate = false\nedge_poll = false\nprobe_interval_secs = 0\nprobe_interval_busy_secs = 0\n",
                data_dir.display(),
                fake_claude().display()
            ),
        )
        .unwrap();
        let log = scratch.join(format!("daemon-{port}.log"));
        let child = Command::new(env!("CARGO_BIN_EXE_threads"))
            .arg("--config")
            .arg(&cfg)
            .env_remove("THREADS_CHASSIS_PORT")
            .env("THREADS_HEALTH_STALE_SECS", BOUND_SECS.to_string())
            .env("THREADS_UPDATE_WATCH", "off")
            .env("CAULDRON_HOME", scratch.join("cauldron-home"))
            .env("THREADS_CLAUDE_BIN", fake_claude())
            .env("THREADS_CLAUDE_HOME", scratch.join("claude-home"))
            .env("HOME", scratch.join("home"))
            .env("FAKE_STATE_DIR", scratch)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .expect("spawn the threads binary");
        let d = Daemon { child, port, log };
        let deadline = Instant::now() + Duration::from_secs(60);
        // R9.C7g r37: a 200 can now arrive while boot recovery still runs
        // behind the served port; up means the daemon says recovery is done.
        while !health(port)
            .map(|(c, b)| c == 200 && b.contains(r#""recovery":{"#) && b.contains(r#""state":"done""#))
            .unwrap_or(false)
        {
            assert!(Instant::now() < deadline, "the daemon never answered /health 200 with recovery done; log:\n{}", d.log());
            std::thread::sleep(Duration::from_millis(100));
        }
        d
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

#[test]
fn health_is_200_while_the_store_is_writable_and_503_within_its_bound_once_the_write_lock_is_held() {
    let scratch = tempfile::tempdir().unwrap();
    let data_dir = scratch.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let d = Daemon::boot(scratch.path(), &data_dir);

    // ── 1. HEALTHY, held past three whole bounds (opposes "always 503" and
    //       "a heartbeat that never beats": either would be 503 by now).
    let held = Duration::from_secs(3 * BOUND_SECS + PERIOD_SECS);
    let t0 = Instant::now();
    while t0.elapsed() < held {
        let got = health(d.port);
        assert_eq!(got.as_ref().map(|(c, _)| *c), Some(200), "a healthy daemon's /health: {got:?}\n{}", d.log());
        std::thread::sleep(Duration::from_millis(250));
    }

    // ── 2. WEDGE: another connection holds the write lock.
    let db = data_dir.join("threads.db");
    assert!(db.is_file(), "premise: the daemon's store is at {}", db.display());
    let locker = rusqlite::Connection::open(&db).unwrap();
    locker.busy_timeout(Duration::from_secs(10)).unwrap();
    locker.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let locked_at = Instant::now();
    let limit = Duration::from_secs(BOUND_SECS + PERIOD_SECS + BUSY_SECS + 10);
    let wedged = loop {
        if let Some((503, body)) = health(d.port) {
            break body;
        }
        assert!(
            locked_at.elapsed() < limit,
            "/health never answered 503 within {limit:?} of the write lock being taken (last: {:?})\n{}",
            health(d.port),
            d.log()
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let took = locked_at.elapsed();
    println!("R9.C6: /health went 503 {took:?} after the write lock was taken (bound {BOUND_SECS}s)");
    // Not before the bound: the 503 is the AGE of the last good beat passing
    // the bound, and that beat was at most one period before the lock.
    assert!(
        took + Duration::from_secs(PERIOD_SECS) + Duration::from_millis(500) >= Duration::from_secs(BOUND_SECS),
        "503 after {took:?}, sooner than the {BOUND_SECS}s bound allows"
    );
    assert!(wedged.contains("\"wedged\"") && wedged.contains("heartbeat_age_ms"), "{wedged}");
    // R9.C7d: the 503 still carries the restart gate — turns running and the
    // drain door (`draining_since`, null when not draining) — so the
    // supervisor can drain a wedged daemon before relaunching it rather than
    // killing its turns. Without the key it reads "no door" and kills them.
    let json: serde_json::Value = serde_json::from_str(wedged.split_once("\r\n\r\n").map(|x| x.1).unwrap_or(""))
        .unwrap_or_else(|e| panic!("the 503 body is not JSON ({e}): {wedged}"));
    assert!(
        json.pointer("/restart_gate/draining_since").is_some_and(|v| v.is_null())
            && json.pointer("/restart_gate/turns").is_some_and(|v| v.as_u64() == Some(0)),
        "R9.C7d: a 503 /health without the restart gate hides the drain door: {wedged}"
    );
    // What `probe = "connect"` would have read at this very moment.
    assert!(connects(d.port), "premise of C-6: the kernel still completes the handshake");

    // ── 3. RELEASED ⇒ 200 again.
    locker.execute_batch("ROLLBACK;").unwrap();
    drop(locker);
    let released = Instant::now();
    while health(d.port).map(|(c, _)| c) != Some(200) {
        assert!(
            released.elapsed() < Duration::from_secs(PERIOD_SECS + BUSY_SECS + 10),
            "/health stayed unhealthy after the lock was released: {:?}\n{}",
            health(d.port),
            d.log()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// **R9.C6b: THE HEARTBEAT DOES NOT HOLD THE DAEMON'S READS HOSTAGE TO A
/// FOREIGN WRITER.** Another process holds SQLite's write lock for 10 s (a long
/// `threads import`, say). The heartbeat keeps pinging — it must, that is how
/// `/health` learns the store cannot be written — but each ping holds the write
/// mutex only for its own short wait, so an in-process statement that needs
/// the write connection completes in well under a second the whole time.
///
/// The read is `Store::synchronous` (`PRAGMA synchronous` on the write
/// connection): it needs the write MUTEX and nothing from SQLite's write lock,
/// so the only thing that can make it slow here is a ping holding the mutex.
/// Pool reads never queued behind the ping and would measure nothing.
///
/// Opposed: the heartbeat is the real `liveness::spawn` over the real
/// `Store::ping`, and the test asserts it RAN (>= 2 attempts in 10 s) and
/// FAILED throughout (no beat during the lock) — so a mutex-only or
/// always-`Ok` ping, which would also keep reads fast, is red here, as it is in
/// the 503 test above. The 10 s outlasts the heartbeat period (2 s) five times
/// and the connection's 5 s `busy_timeout` twice (doctrine 4).
#[test]
fn an_in_process_read_stays_under_1s_while_another_process_holds_the_write_lock_for_10s() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use threads::liveness;
    use threads::store::Store;

    let scratch = tempfile::tempdir().unwrap();
    let db = scratch.path().join("threads.db");
    let store = Store::open(&db).unwrap();
    store.ping().expect("premise: an unlocked store pings");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let hb = liveness::Heartbeat::new(Duration::from_secs(60));
    let attempts = Arc::new(AtomicUsize::new(0));
    let (pinged, counted) = (store.clone(), Arc::clone(&attempts));
    let _beat = {
        let _g = rt.enter();
        liveness::spawn(hb.clone(), liveness::HEARTBEAT_PERIOD, move || {
            counted.fetch_add(1, Ordering::SeqCst);
            pinged.ping().map_err(|e| e.to_string())
        })
    };
    // Let it beat once while the store is free, so "no beat during the lock"
    // below is a change and not a heartbeat that never ran.
    let t = Instant::now();
    while attempts.load(Ordering::SeqCst) == 0 || hb.age() > Duration::from_secs(PERIOD_SECS + 1) {
        assert!(t.elapsed() < Duration::from_secs(10), "the heartbeat never beat on a free store");
        std::thread::sleep(Duration::from_millis(50));
    }

    let locker = rusqlite::Connection::open(&db).unwrap();
    locker.busy_timeout(Duration::from_secs(10)).unwrap();
    locker.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let locked_at = Instant::now();
    let before = attempts.load(Ordering::SeqCst);
    let held = Duration::from_secs(10);
    let (mut worst, mut reads) = (Duration::ZERO, 0u32);
    while locked_at.elapsed() < held {
        let t = Instant::now();
        store.synchronous().expect("an in-process read on the write connection");
        worst = worst.max(t.elapsed());
        reads += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
    let during = attempts.load(Ordering::SeqCst) - before;
    let age = hb.age();
    locker.execute_batch("ROLLBACK;").unwrap();
    drop(locker);
    println!("R9.C6b: {reads} reads over {held:?} of a foreign write lock, worst {worst:?}; {during} pings, heartbeat age {age:?}");

    assert!(
        worst < Duration::from_secs(1),
        "an in-process read waited {worst:?} behind the heartbeat while another process held the write lock"
    );
    // At least two: a 5 s ping wait leaves room for two attempts in 10 s, a
    // short one for four or more.
    assert!(during >= 2, "premise: the heartbeat kept pinging through the lock ({during} attempts in {held:?})");
    assert!(
        // Less a hair: a ping already inside its BEGIN when the lock was
        // asked for finishes first and may stamp just after `locked_at`.
        age + Duration::from_millis(500) >= held,
        "the heartbeat BEAT while another process held the write lock (age {age:?}): the ping no longer sees a store it cannot write"
    );

    // Released: the very next ping beats again.
    let released = Instant::now();
    while hb.age() > released.elapsed() {
        assert!(released.elapsed() < Duration::from_secs(3 * PERIOD_SECS), "no beat after the lock was released");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// **R9.C6b (inspect F-1): THE SUPERVISOR'S START WINDOW IS NEVER SHORTER THAN
/// THE DAEMON'S OWN ALLOWANCE FOR THE SAME BOOT.** Between the bind and the
/// first `/health` 200, `update.on_boot` → `keep_last_good` pays the running
/// image's first exec under `FIRST_EXEC_BUDGET_MS` and then a `--self-check`
/// under `SELF_CHECK_TIMEOUT`. Cauldron waits for that 200 within the
/// declaration's `start_timeout_ms` (`cli/src/main.rs` `wait_healthy`). A
/// window shorter than the sum kills, as a failed start, a boot the daemon
/// would have finished — 60 s did, on paper, under a storm (~65 s).
///
/// Reads the SHIPPED declaration (`services/threads.toml`), parsed, not
/// grepped, so a comment cannot satisfy it.
#[test]
fn the_declared_start_timeout_covers_first_exec_budget_plus_self_check() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("threads.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let decl: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(decl.get("name").and_then(|v| v.as_str()), Some("threads"), "premise: {} is threads' declaration", path.display());
    let declared = decl
        .get("start_timeout_ms")
        .and_then(|v| v.as_integer())
        .expect("threads.toml declares start_timeout_ms") as u128;
    let floor = threads::update::FIRST_EXEC_BUDGET_MS as u128 + threads::update::SELF_CHECK_TIMEOUT.as_millis();
    assert!(floor >= 140_000, "premise: the daemon's own allowance is first exec 120 s + self-check 20 s, got {floor} ms");
    assert!(
        declared >= floor,
        "start_timeout_ms = {declared} is shorter than FIRST_EXEC_BUDGET_MS + SELF_CHECK_TIMEOUT = {floor} ms: \
         cauldron would kill a boot that threads' own keep_last_good is still allowed to be in"
    );
}

/// **R9.C6b (inspect finding 3): A PING RESTORES THE CONNECTION'S 5 s WAIT.**
/// The ping lowers `busy_timeout` to `PING_BUSY_MS` for itself only. Every
/// other write must keep waiting out a foreign writer's brief lock as before:
/// here another connection holds the write lock for 1.5 s, a ping fails fast
/// during it, and an ordinary store write issued right after that failed ping
/// WAITS for the lock and succeeds. With the 250 ms left behind it fails BUSY.
#[test]
fn a_failed_ping_leaves_the_write_connection_waiting_its_full_5s() {
    let scratch = tempfile::tempdir().unwrap();
    let db = scratch.path().join("threads.db");
    let store = threads::store::Store::open(&db).unwrap();
    store.ping().expect("premise: an unlocked store pings");

    let locker = rusqlite::Connection::open(&db).unwrap();
    locker.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let hold = Duration::from_millis(1500);
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(hold);
        locker.execute_batch("ROLLBACK;").unwrap();
    });
    let locked_at = Instant::now();

    let t = Instant::now();
    let pinged = store.ping();
    let ping_took = t.elapsed();
    assert!(pinged.is_err(), "premise: a ping under a foreign write lock fails");
    assert!(ping_took < Duration::from_secs(1), "the ping waited {ping_took:?}, not its short wait");
    assert!(locked_at.elapsed() < hold, "premise: the lock is still held after the ping");

    let t = Instant::now();
    let wrote = store.record_thread_account("c6b-thread", "c6b-account");
    let write_took = t.elapsed();
    releaser.join().unwrap();
    println!("R9.C6b: ping failed in {ping_took:?}; the next write took {write_took:?} ({wrote:?})");
    assert!(
        wrote.is_ok(),
        "a write right after a failed ping gave up after {write_took:?} ({wrote:?}): the ping left its short busy_timeout behind"
    );
    // It really waited for the lock — the write was contended, not lucky.
    assert!(write_took >= Duration::from_millis(500), "premise: the write waited for the lock ({write_took:?})");
}

// R9.F2b: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
