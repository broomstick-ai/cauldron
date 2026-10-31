//! **R9.C7c (gap C-7) — A PIN WHILE TURNS EXECUTE: THREADS DRAINS, THEN IS
//! REPLACED, AND NO TURN IS LOST OR RE-BOUGHT.** The threads half, against the
//! REAL binary.
//!
//! A re-pin restart that waits for a quiet moment never happens in prod:
//! threads had no idle window from 23:35Z to 03:38Z (2026-10-16/17), because
//! long turns hand off back to back (an internal report §C). So
//! the supervisor asks threads to DRAIN (`SIGUSR1`): it starts no new turn,
//! queues every new prompt, and lets the running turns finish. `SIGUSR2`
//! cancels a drain. What the drain is doing is read back off `/health`
//! (`restart_gate.draining_since`, `restart_gate.turns`).
//!
//! Claims, each paired with its opposite:
//!
//! (handoff — the brief's step 3, MEASURED before it was relied on) a live
//!     `claude` child SURVIVES its daemon being stopped the way a supervisor
//!     stops it (SIGTERM, wait for exit), and the NEXT daemon on the same data
//!     dir re-adopts it: the turn ends once, with its own result, the child
//!     was started once, nothing was killed or retried. OPPOSED by the hazard
//!     test: a child that writes stderr after its daemon died dies of SIGPIPE
//!     and its turn is lost — which is why cauldron's at-cap default is
//!     `extend` (wait, never kill), and `handoff` is opt-in.
//! (a) a drain does not touch a running turn: it finishes with its own verdict
//!     (no `turn_killed`, no `retry_scheduled`), and only then does the gate
//!     read idle;
//! (b) a prompt sent mid-drain is QUEUED, not started — asserted over four
//!     resume ticks, the backstop that would otherwise start it — and runs on
//!     the far side of the restart, once. Opposed: with no drain the same
//!     prompt starts at once;
//! (d) a cancelled drain starts the queued prompt without any restart.
//!
//! Throwaway everything: a temp data dir, a port taken from the kernel, the
//! stub `claude`, a temp `CAULDRON_HOME` and `HOME`. Nothing here can reach the
//! operator's `:3653` or `~/.cauldron`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const ACCOUNT: &str = "alice-example-com";
/// The resume tick, shrunk so that "not started" can outlast it (doctrine 4).
const RESUME_TICK_MS: u64 = 500;
/// Four resume ticks: every way a queued prompt is re-asked has come round.
const OUTLAST: Duration = Duration::from_millis(4 * RESUME_TICK_MS);
const BUDGET: Duration = Duration::from_secs(60);

fn fake_claude() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn write_config(dir: &Path, data_dir: &Path, port: u16) -> PathBuf {
    let slot = dir.join("slots").join(ACCOUNT);
    std::fs::create_dir_all(&slot).unwrap();
    let cfg = dir.join(format!("config-{port}.toml"));
    std::fs::write(
        &cfg,
        format!(
            "host = \"127.0.0.1\"\n\
             chassis_port = {port}\n\
             data_dir = \"{}\"\n\
             claude_bin = \"{}\"\n\
             calibrate = false\n\
             edge_poll = false\n\
             probe_interval_secs = 0\n\
             probe_interval_busy_secs = 0\n\
             [accounts]\n\
             \"{ACCOUNT}\" = \"{}\"\n",
            data_dir.display(),
            fake_claude().display(),
            slot.display(),
        ),
    )
    .unwrap();
    cfg
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

async fn eventually(what: &str, budget: Duration, mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(t0.elapsed() < budget, "timed out after {budget:?} waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct Daemon {
    child: Child,
    port: u16,
    log: PathBuf,
}

impl Daemon {
    async fn boot(scratch: &Path, data_dir: &Path) -> Daemon {
        let port = free_port();
        let cfg = write_config(scratch, data_dir, port);
        let log = scratch.join(format!("daemon-{port}.log"));
        let child = Command::new(env!("CARGO_BIN_EXE_threads"))
            .arg("--config")
            .arg(&cfg)
            .env_remove("THREADS_CHASSIS_PORT")
            .env("CAULDRON_HOME", scratch.join("cauldron-home"))
            .env("THREADS_CLAUDE_BIN", fake_claude())
            .env("THREADS_CLAUDE_HOME", scratch.join("claude-home"))
            .env("THREADS_RESUME_TICK_MS", RESUME_TICK_MS.to_string())
            .env("THREADS_TAIL_POLL_MS", "50")
            .env("HOME", scratch.join("home"))
            .env("FAKE_STATE_DIR", scratch)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .expect("spawn the threads binary");
        let d = Daemon { child, port, log };
        // UP MEANS RECOVERED, read from the process (R9.C7g r37). Since C7g a
        // `/health` answer arrives while boot recovery is still running behind
        // the served port, so "it answered" no longer implies "it re-adopted
        // its turns" — the premise the restart tests below rest on. Wait for
        // the daemon's own `recovery.state == "done"`, never for a timer.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let h = d.health_raw();
            if h.as_ref().map(|h| h["recovery"]["state"] == "done").unwrap_or(false) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the daemon never reported recovery done (last /health {h:?}); log: {}",
                d.log()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        d
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    fn signal(&self, sig: i32) {
        unsafe {
            libc::kill(self.pid(), sig);
        }
    }

    fn health_raw(&self) -> Option<Value> {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        s.write_all(b"GET /health HTTP/1.0\r\nHost: x\r\n\r\n").ok()?;
        let mut buf = String::new();
        s.read_to_string(&mut buf).ok()?;
        serde_json::from_str(buf.split_once("\r\n\r\n")?.1).ok()
    }

    /// `restart_gate` off this process's own `/health`.
    fn gate(&self) -> Value {
        self.health_raw().map(|h| h["restart_gate"].clone()).unwrap_or(Value::Null)
    }

    async fn request(&self, method: &str, params: Value) -> Value {
        let (ws, _) = connect_async(format!("ws://127.0.0.1:{}/ws", self.port)).await.unwrap();
        let (mut wr, mut rd) = ws.split();
        wr.send(Message::Text(
            json!({"type": "request", "id": "c7c", "method": method, "params": params}).to_string(),
        ))
        .await
        .unwrap();
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(20), rd.next())
                .await
                .expect("timed out waiting for the response")
                .expect("socket closed")
                .expect("socket error");
            if let Message::Text(t) = msg {
                let f: Value = serde_json::from_str(&t).unwrap();
                if f["type"] == "response" && f["correlate"] == json!("c7c") {
                    let out = f["outcome"].clone();
                    assert_eq!(out["type"], "success", "{method}: {out}");
                    return out["payload"].clone();
                }
            }
        }
    }

    async fn new_thread(&self, cwd: &Path) -> String {
        let p = self
            .request("threads/create", json!({"settings": {"cwd": cwd.to_string_lossy(), "model": "sonnet"}}))
            .await;
        p["thread_id"].as_str().expect("a thread id").to_string()
    }

    async fn send(&self, id: &str, text: &str) {
        self.request(&format!("threads/{id}/send"), json!({"text": text})).await;
    }

    /// What a supervisor does: SIGTERM, then wait for the process to be gone.
    fn stop(mut self) {
        self.signal(libc::SIGTERM);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("the throwaway daemon ignored SIGTERM for 60 s");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A turn the test holds open: it runs until `release` exists, and appends
/// its pid to `starts` the moment it is started, so "started once" is counted
/// by the child itself, not inferred from the ledger.
struct Held {
    release: PathBuf,
    starts: PathBuf,
}

impl Held {
    fn new(scratch: &Path, tag: &str) -> Held {
        Held { release: scratch.join(format!("release-{tag}")), starts: scratch.join(format!("starts-{tag}")) }
    }
    fn prompt(&self, extra: &str) -> String {
        format!("handoffturn {extra} release={} starts={}", self.release.display(), self.starts.display())
    }
    fn release(&self) {
        std::fs::write(&self.release, b"go").unwrap();
    }
    fn pids(&self) -> Vec<i32> {
        std::fs::read_to_string(&self.starts)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        // Never leave a fixture child behind: release it, then make sure.
        let _ = std::fs::write(&self.release, b"go");
        for p in self.pids() {
            if alive(p) {
                std::thread::sleep(Duration::from_millis(300));
                unsafe {
                    libc::kill(p, libc::SIGKILL);
                }
            }
        }
    }
}

/// Every ledger kind for `id`, in order, read off the store with no daemon up.
fn kinds(data_dir: &Path, id: &str) -> Vec<String> {
    let store = threads::Store::open_read_only(data_dir.join("threads.db")).unwrap();
    store
        .events_from(id, 0, 100_000)
        .unwrap()
        .into_iter()
        .map(|e| e.event.kind().to_string())
        .collect()
}

fn count(k: &[String], kind: &str) -> usize {
    k.iter().filter(|x| *x == kind).count()
}

fn scratch() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("threads-home");
    let cwd = tmp.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    (tmp, data, cwd)
}

// ══════════════════════════════════════════════════════════ handoff ═════

/// **THE MEASUREMENT THE CAP'S FALLBACK RESTS ON.** A turn executing when its
/// daemon is stopped keeps running, and the next daemon re-adopts it and lands
/// it with its own verdict, once.
///
/// Forced: with `recover()`'s adoption skipped (the new daemon never tails the
/// orphan), the turn is never ended by anyone and the wait for `turn_ended`
/// times out — so the green below is the new process doing the landing, not
/// the old one having done it before it died (the child is still held open
/// across the whole restart, and released only once B is up).
#[tokio::test(flavor = "multi_thread")]
async fn c7c_handoff_a_live_turn_survives_its_daemon_and_the_next_one_lands_it_once() {
    let (tmp, data, cwd) = scratch();
    let held = Held::new(tmp.path(), "h");
    let a = Daemon::boot(tmp.path(), &data).await;
    let id = a.new_thread(&cwd).await;
    a.send(&id, &held.prompt("")).await;
    eventually("the held turn started", BUDGET, || held.pids().len() == 1).await;
    eventually("A counts it running", BUDGET, || a.gate()["turns"] == 1).await;
    let child = held.pids()[0];
    let a_pid = a.pid();

    a.stop();
    assert!(!alive(a_pid), "sentinel: daemon A is gone");
    assert!(alive(child), "the turn's child DIED WITH ITS DAEMON (pid {child})");

    let b = Daemon::boot(tmp.path(), &data).await;
    eventually("B re-adopted the turn", BUDGET, || b.gate()["turns"] == 1).await;
    assert!(b.log().contains("re-adopting running turn"), "{}", b.log());
    held.release();
    eventually("the adopted turn ended under B", BUDGET, || b.gate()["turns"] == 0).await;
    eventually("the child exited", BUDGET, || !alive(child)).await;
    b.stop();

    let k = kinds(&data, &id);
    assert_eq!(held.pids().len(), 1, "the child was started more than once: {:?}", held.pids());
    assert_eq!(count(&k, "turn_started"), 1, "{k:?}");
    assert_eq!(count(&k, "turn_ended"), 1, "{k:?}");
    assert_eq!(count(&k, "result_turn"), 1, "the turn's own verdict landed once: {k:?}");
    for bad in ["turn_killed", "retry_scheduled", "turn_interrupted", "turn_failed"] {
        assert_eq!(count(&k, bad), 0, "{bad} on a handed-off turn: {k:?}");
    }
}

// ════════════════════════════════════════════════════════ (a) + (b) ═════

/// (a) and (b) on one daemon pair: a drain while T1 executes leaves T1 alone,
/// and T2's prompt, sent mid-drain, waits for the restart.
#[tokio::test(flavor = "multi_thread")]
async fn c7c_b_a_prompt_mid_drain_is_queued_outlasting_4_resume_ticks_and_runs_after_the_restart() {
    let (tmp, data, cwd) = scratch();
    let t1 = Held::new(tmp.path(), "t1");
    let t2 = Held::new(tmp.path(), "t2");
    t2.release(); // T2 finishes the moment it is started.
    let a = Daemon::boot(tmp.path(), &data).await;
    assert_eq!(a.gate()["draining_since"], Value::Null, "the door is served, not draining: {}", a.gate());
    let id1 = a.new_thread(&cwd).await;
    let id2 = a.new_thread(&cwd).await;
    a.send(&id1, &t1.prompt("")).await;
    eventually("T1 started", BUDGET, || t1.pids().len() == 1).await;

    a.signal(libc::SIGUSR1);
    eventually("A says it is draining", BUDGET, || a.gate()["draining_since"].is_u64()).await;
    assert_eq!(a.gate()["draining"], true);
    a.send(&id2, &t2.prompt("")).await; // accepted, not refused
    tokio::time::sleep(OUTLAST).await;
    assert!(t2.pids().is_empty(), "a prompt STARTED mid-drain: {:?}\n{}", t2.pids(), a.log());

    // (a): the running turn is untouched and finishes with its own verdict.
    assert_eq!(a.gate()["idle"], false, "T1 is still running: {}", a.gate());
    t1.release();
    eventually("T1 finished and A reads idle", BUDGET, || a.gate()["idle"] == true).await;
    tokio::time::sleep(OUTLAST).await;
    assert!(t2.pids().is_empty(), "T2 started on the draining daemon after T1 ended\n{}", a.log());
    assert!(a.gate()["draining_since"].is_u64(), "the drain held until the restart");
    a.stop();
    let k1 = kinds(&data, &id1);
    assert_eq!(count(&k1, "result_turn"), 1, "{k1:?}");
    for bad in ["turn_killed", "retry_scheduled", "turn_interrupted", "turn_failed"] {
        assert_eq!(count(&k1, bad), 0, "{bad} on the turn that ran through the drain: {k1:?}");
    }

    // (b): the queued prompt runs on the far side of the restart, once.
    let b = Daemon::boot(tmp.path(), &data).await;
    assert_eq!(b.gate()["draining_since"], Value::Null, "a drain must not outlive its process");
    eventually("T2 ran under B", BUDGET, || t2.pids().len() == 1).await;
    eventually("B idle", BUDGET, || b.gate()["idle"] == true).await;
    b.stop();
    let k2 = kinds(&data, &id2);
    assert_eq!(count(&k2, "turn_started"), 1, "{k2:?}");
    assert_eq!(count(&k2, "result_turn"), 1, "{k2:?}");
    assert_eq!(t1.pids().len(), 1, "T1 was re-run: {:?}", t1.pids());
}

/// Opposes (b): with no drain the same prompt starts at once, inside the
/// window (b) holds it out for — so (b)'s hold is the drain, not a slow
/// fixture.
#[tokio::test(flavor = "multi_thread")]
async fn c7c_b_without_a_drain_the_same_prompt_starts_at_once() {
    let (tmp, data, cwd) = scratch();
    let t2 = Held::new(tmp.path(), "t2");
    t2.release();
    let a = Daemon::boot(tmp.path(), &data).await;
    let id2 = a.new_thread(&cwd).await;
    a.send(&id2, &t2.prompt("")).await;
    eventually("T2 started with no drain", OUTLAST, || t2.pids().len() == 1).await;
    a.stop();
}

// ═══════════════════════════════════════════════════════════════ (d) ════

/// (d) a cancelled drain resumes normal starts: the prompt queued during the
/// drain starts on the SAME process, with no restart, and new prompts start.
#[tokio::test(flavor = "multi_thread")]
async fn c7c_d_a_cancelled_drain_starts_the_queued_prompt_without_a_restart() {
    let (tmp, data, cwd) = scratch();
    let t2 = Held::new(tmp.path(), "t2");
    let t3 = Held::new(tmp.path(), "t3");
    t2.release();
    t3.release();
    let a = Daemon::boot(tmp.path(), &data).await;
    let id2 = a.new_thread(&cwd).await;
    a.signal(libc::SIGUSR1);
    eventually("draining", BUDGET, || a.gate()["draining_since"].is_u64()).await;
    a.send(&id2, &t2.prompt("")).await;
    tokio::time::sleep(OUTLAST).await;
    assert!(t2.pids().is_empty(), "sentinel: queued while draining\n{}", a.log());

    a.signal(libc::SIGUSR2);
    eventually("the drain is over", BUDGET, || a.gate()["draining_since"].is_null()).await;
    eventually("the queued prompt started on the same process", BUDGET, || t2.pids().len() == 1).await;
    let id3 = a.new_thread(&cwd).await;
    a.send(&id3, &t3.prompt("")).await;
    eventually("a new prompt starts normally", BUDGET, || t3.pids().len() == 1).await;
    let a_pid = a.pid();
    assert!(alive(a_pid), "no restart happened");
    a.stop();
    assert_eq!(count(&kinds(&data, &id2), "result_turn"), 1);
}

/// **THE SIGPIPE'D HANDOFF IS RETRIED, ONCE** (measured R9.C7c, flipped by
/// R9.L2, ruling E18-7). The same handoff, but the child writes one line to
/// STDERR after its daemon is gone. Its stderr is a pipe to the dead daemon,
/// so it dies of SIGPIPE before its result. Before L-2 the next daemon closed
/// the turn with `turn_ended` and NO result, NO retry, NO failure — the prompt
/// was lost (this test pinned that state, and was red here only once L-2
/// landed; the inverted assertion is red at the pre-L2 tip 36584166). With L-2
/// the adopting daemon's verdict schedules a retry: exactly one
/// `retry_scheduled`, after exactly one `turn_started` / `turn_ended` — so the
/// turn is neither lost nor silently re-run before the retry. Whether
/// `at_cap = "handoff"` should then become the default is NOT decided here
/// (it goes with R9.C7b).
/// Opposed by `c7c_handoff_…_lands_it_once`: same path, no stderr write, lands.
#[tokio::test(flavor = "multi_thread")]
async fn c7c_handoff_a_child_killed_by_sigpipe_after_its_daemon_died_is_retried() {
    let (tmp, data, cwd) = scratch();
    let held = Held::new(tmp.path(), "s");
    let a = Daemon::boot(tmp.path(), &data).await;
    let id = a.new_thread(&cwd).await;
    a.send(&id, &held.prompt("stderrafter")).await;
    eventually("started", BUDGET, || held.pids().len() == 1).await;
    eventually("running", BUDGET, || a.gate()["turns"] == 1).await;
    let child = held.pids()[0];
    a.stop();
    assert!(alive(child), "sentinel: the child outlived its daemon");
    let b = Daemon::boot(tmp.path(), &data).await;
    eventually("adopted", BUDGET, || b.gate()["turns"] == 1).await;
    held.release();
    eventually("ended under B", BUDGET, || b.gate()["turns"] == 0).await;
    eventually("the child is gone", BUDGET, || !alive(child)).await;
    eventually("the retry is scheduled", BUDGET, || {
        count(&kinds(&data, &id), "retry_scheduled") >= 1
    })
    .await;
    b.stop();
    let k = kinds(&data, &id);
    assert_eq!(count(&k, "retry_scheduled"), 1, "exactly one retry: {k:?}");
    let at = k.iter().position(|x| x == "retry_scheduled").unwrap();
    let before = &k[..at];
    assert_eq!(count(before, "turn_started"), 1, "no second turn_started before the retry: {k:?}");
    assert_eq!(count(before, "turn_ended"), 1, "the SIGPIPE'd turn ended once: {k:?}");
    assert_eq!(count(before, "result_turn"), 0, "sentinel: the child died before its result: {k:?}");
    for none in ["turn_failed", "turn_parked"] {
        assert_eq!(count(&k, none), 0, "{none}: the turn is retried, not failed or parked: {k:?}");
    }
}

// R9.F2b: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;

// ═══════════════════════════════════════ R9.C7b (E18-15): handoff at the cap ═══

/// **WHAT `at_cap = "handoff"` DOES TO THREADS, END TO END ON THE REAL BINARY**
/// (R9.C7b, ledger E18-15). Threads now declares `handoff` with a 600 s cap, so
/// at the cap cauldron stops a draining threads that still has a turn running
/// and starts the new build. Here T1 NEVER finishes while A lives; A is
/// draining, and T2 arrives mid-drain (queued, outlasting four resume ticks).
/// Then the supervisor's at-cap act: SIGTERM, wait, boot B on the same data.
/// B re-adopts T1 and T2 RUNS while T1 is still running (intake resumed: it
/// does not wait for the handed-off turn), a new prompt T3 starts normally,
/// and T1 — released only once B is up — lands its OWN verdict, once, with no
/// kill, retry, interruption or failure. The gate names the turn it waits for
/// (`restart_gate.turn_threads`), which is what `cauldron status` prints.
///
/// Forced: with `recover()`'s adoption skipped, T1's verdict never lands (see
/// the report); with `admit_turn_for` reverted to `admit_turn`, `turn_threads`
/// is empty and the first assertion on it is red.
#[tokio::test(flavor = "multi_thread")]
async fn c7b_handoff_at_the_cap_readopts_a_never_ending_turn_lands_its_verdict_and_intake_resumes() {
    let (tmp, data, cwd) = scratch();
    let t1 = Held::new(tmp.path(), "t1");
    let t2 = Held::new(tmp.path(), "t2");
    let t3 = Held::new(tmp.path(), "t3");
    t2.release();
    t3.release();
    let a = Daemon::boot(tmp.path(), &data).await;
    let id1 = a.new_thread(&cwd).await;
    let id2 = a.new_thread(&cwd).await;
    a.send(&id1, &t1.prompt("")).await;
    eventually("T1 started", BUDGET, || t1.pids().len() == 1).await;
    eventually("A counts T1", BUDGET, || a.gate()["turns"] == 1).await;
    assert_eq!(a.gate()["turn_threads"], json!([id1]), "the gate does not name the running turn");

    a.signal(libc::SIGUSR1);
    eventually("A is draining", BUDGET, || a.gate()["draining_since"].is_u64()).await;
    a.send(&id2, &t2.prompt("")).await;
    tokio::time::sleep(OUTLAST).await;
    assert!(t2.pids().is_empty(), "sentinel: T2 is held by the drain\n{}", a.log());

    // The cap: cauldron hands off — stops A with T1 still running.
    let child = t1.pids()[0];
    let a_pid = a.pid();
    a.stop();
    assert!(!alive(a_pid), "sentinel: A is gone");
    assert!(alive(child), "T1's child died with its daemon");

    let b = Daemon::boot(tmp.path(), &data).await;
    eventually("T2 runs under B (intake resumed)", BUDGET, || t2.pids().len() == 1).await;
    assert!(alive(child), "intake resumed only after the handed-off turn ended");
    eventually("B names the re-adopted T1 alone", BUDGET, || b.gate()["turn_threads"] == json!([id1])).await;
    assert!(b.log().contains("re-adopting running turn"), "{}", b.log());
    assert_eq!(b.gate()["draining_since"], Value::Null, "a drain must not outlive its process");
    let id3 = b.new_thread(&cwd).await;
    b.send(&id3, &t3.prompt("")).await;
    eventually("a new prompt starts while T1 still runs", BUDGET, || t3.pids().len() == 1).await;
    assert!(alive(child), "sentinel: T1 is still running");

    t1.release();
    eventually("T1 ended under B", BUDGET, || b.gate()["turns"] == 0).await;
    eventually("T1's child exited", BUDGET, || !alive(child)).await;
    b.stop();

    let k1 = kinds(&data, &id1);
    assert_eq!(t1.pids().len(), 1, "T1 was started more than once: {:?}", t1.pids());
    assert_eq!(count(&k1, "turn_started"), 1, "{k1:?}");
    assert_eq!(count(&k1, "turn_ended"), 1, "{k1:?}");
    assert_eq!(count(&k1, "result_turn"), 1, "T1's own verdict did not land once: {k1:?}");
    for bad in ["turn_killed", "retry_scheduled", "turn_interrupted", "turn_failed"] {
        assert_eq!(count(&k1, bad), 0, "{bad} on the handed-off turn: {k1:?}");
    }
    for (id, what) in [(&id2, "T2"), (&id3, "T3")] {
        let k = kinds(&data, id);
        assert_eq!(count(&k, "turn_started"), 1, "{what}: {k:?}");
        assert_eq!(count(&k, "result_turn"), 1, "{what}: {k:?}");
    }
}
