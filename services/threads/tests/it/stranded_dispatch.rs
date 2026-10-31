//! THE STRANDED DISPATCH — a restart must not be able to eat what the
//! operator said.
//!
//! WHAT WENT WRONG (verified live, 2026-08-03 03:57:4x, thread
//! `495bd3cd…` — the operator's "Minecraft" thread). The daemon self-updates
//! with `execv`, which keeps the pid and leaves the port unowned for a moment.
//! Something noticed the gap and started a SECOND daemon. Both ran the same
//! boot sequence, and that sequence was `recover()` FIRST, bind the port
//! SECOND. So both daemons re-dispatched real turns before either found out it
//! had lost. The loser took the thread lock, marked three of the operator's
//! messages `delivered`, built their prompt, handed it to a detached task —
//! and then failed the bind, at which point `main.rs` called
//! `std::process::exit(1)` and took that task with it.
//!
//! What survived was the worst possible residue: three `queue` rows reading
//! `delivered`, no `running_turns` row, no turn, no output file, no tokens
//! spent. `waiting_queue` only ever offers `state='waiting'`, so nothing would
//! have handed those messages to anyone ever again. The operator asked a
//! question and the system quietly decided it had already been answered.
//!
//! Two fixes, one per failure:
//!
//!   1. `recover()` returns stranded deliveries to the queue, so a death
//!      anywhere in the dispatch window is survivable however it happens.
//!   2. `run_daemon` takes the port BEFORE recovering, so a daemon that is
//!      not supposed to exist dies having consumed nothing.
//!
//! The counterweight in every test here is single execution: a message that
//! provably DID reach a turn must never be handed out a second time, no matter
//! how that turn ended. A duplicate turn is a real harm.

#[path = "support/keychain.rs"]
mod keychain;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::event::{Push, SendIntent, ThreadEvent};
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env() {
    // THE CONSUMER LEDGER IS WIRED BY `run_daemon`, so a test that starts one
    // must be pointed away from the operator's real `~/.cauldron` — otherwise
    // every suite run appends to the file he uses to see who is subscribed, and
    // parallel test binaries interleave into it. See `threads::consumers`.
    std::env::set_var(
        "CAULDRON_HOME",
        std::env::temp_dir().join("threads-tests-cauldron"),
    );
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "200");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &std::path::Path) -> Config {
    // CON-27: one account threads OWNS, i.e. one with its own config dir under
    // the tempdir. These tests care about what recovery does to the QUEUE, not
    // about who serves the turn, but every one of them creates a thread and a
    // thread has to be PLACED somewhere. The machine `default` that
    // `ensure_default` registers on every boot is the operator's own
    // `~/.claude.json` login and is no longer a placement target, so a config
    // registering no accounts of its own can no longer create a thread at all.
    // `aa-owned` sorts first, so it plays exactly the role `default` used to.
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        dir.join("aa-owned").to_string_lossy().to_string(),
    );
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn plain(cwd: &std::path::Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..400 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

/// Every ledger event WITH the thread it belongs to. The thread id is not
/// decoration: message ids are per-thread ordinals, so two threads both call
/// their first message `1`, and a log that drops the thread id cannot tell a
/// duplicate execution apart from two threads each running once.
type Log = Arc<Mutex<Vec<(String, ThreadEvent)>>>;

fn watch(m: &ThreadManager) -> Log {
    let mut rx = m.subscribe();
    let seen: Log = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Event(ev) = p {
                out.lock().unwrap().push((ev.thread_id.clone(), ev.event));
            }
        }
    });
    seen
}

/// Every message id that reached a prompt ON ONE THREAD, in delivery order.
fn delivered(log: &Log, thread_id: &str) -> Vec<u64> {
    let mut out = Vec::new();
    for (tid, e) in log.lock().unwrap().iter() {
        if tid != thread_id {
            continue;
        }
        if let ThreadEvent::TurnStarted { message_ids, .. } = e {
            out.extend(message_ids.iter().copied());
        }
    }
    out
}

/// Reproduce the exact durable residue the dying daemon left behind: the queue
/// rows are `delivered`, and no `turn_started` names them.
///
/// This is what `maybe_dispatch` leaves on disk between `mark_delivered` and
/// `run_turn`'s first emit. Killing a real process inside that window is not
/// reproducible in-process, so the test asserts against the STATE that window
/// produces, which is what recovery actually has to read.
fn strand(m: &ThreadManager, thread_id: &str) -> Vec<i64> {
    let rows = m.store.waiting_queue(thread_id).unwrap();
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    m.store.mark_delivered(&ids).unwrap();
    ids
}

// ── 1. the loss, and its undoing ─────────────────────────────────────────

/// A DISPATCH THAT DIED BEFORE ITS TURN STARTED gives the messages back.
///
/// Without the recovery pass the rows sit `delivered` forever and the operator
/// never hears an answer — the live failure, exactly.
#[tokio::test]
async fn a_dispatch_that_died_before_its_turn_started_returns_its_messages() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    // Three messages, held back from dispatch so the test owns the timing.
    let held = m.try_lock_thread(&id, "test").unwrap();
    let mut sent = Vec::new();
    for i in 0..3 {
        sent.push(
            m.send(
                &id,
                format!("stranded {i}"),
                SendIntent::Immediate,
                serde_json::Value::Null,
                false,
            )
            .unwrap(),
        );
    }
    let row_ids = strand(&m, &id);
    assert_eq!(row_ids.len(), 3, "three messages should be waiting");
    held.release();

    // Everything the operator said is now invisible to the scheduler.
    assert!(
        m.store.waiting_queue(&id).unwrap().is_empty(),
        "precondition: a stranded batch is not in the waiting queue"
    );
    assert_eq!(
        m.store.stranded_deliveries().unwrap().len(),
        3,
        "recovery must SEE all three stranded rows"
    );

    let log = watch(&m);
    m.recover().await;

    assert!(
        eventually(|| delivered(&log, &id).len() == 3).await,
        "the stranded messages were never re-delivered: {:?}",
        delivered(&log, &id)
    );
    assert_eq!(
        delivered(&log, &id),
        sent,
        "every stranded message comes back, in order, exactly once"
    );
    assert!(
        m.store.stranded_deliveries().unwrap().is_empty(),
        "once re-delivered they are no longer stranded"
    );
}

// ── 2. the counterweight: single execution ───────────────────────────────

/// A MESSAGE THAT DID REACH A TURN IS NEVER HANDED OUT AGAIN — the guard that
/// makes recovery safe to run on every boot.
///
/// `turn_started` is emitted before the output file is created and before the
/// child is spawned, so its presence is a durable receipt that a child was
/// about to receive that message. Recovery treats that receipt as final.
#[tokio::test]
async fn a_message_that_reached_a_turn_is_never_re_delivered() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let log = watch(&m);
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    let mid = m
        .send(
            &id,
            "answered once".to_string(),
            SendIntent::Immediate,
            serde_json::Value::Null,
            false,
        )
        .unwrap();

    assert!(
        eventually(|| delivered(&log, &id) == vec![mid] && !m.is_locked(&id)).await,
        "the turn never ran"
    );
    assert!(
        m.store.stranded_deliveries().unwrap().is_empty(),
        "a delivery with a turn behind it is NOT stranded"
    );

    // Boot again, and again. Recovery must stay a no-op on it.
    m.recover().await;
    m.recover().await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        delivered(&log, &id),
        vec![mid],
        "DUPLICATE EXECUTION: a message that already ran was delivered a second time"
    );
}

/// A TURN THAT STARTED AND THEN DIED IS BURIED, NOT RE-RUN.
///
/// This is the deliberately conservative half. That turn burned tokens and may
/// have done real work — a database write, a git commit, a message sent. Only
/// turns that provably never began are recoverable; anything else risks doing
/// the work twice, which is worse than a turn that visibly stopped.
#[tokio::test]
async fn a_turn_that_started_and_died_is_never_re_run() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let log = watch(&m);
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    let mid = m
        .send(
            &id,
            "slowturn interrupted".to_string(),
            SendIntent::Immediate,
            serde_json::Value::Null,
            false,
        )
        .unwrap();
    assert!(
        eventually(|| delivered(&log, &id) == vec![mid]).await,
        "the turn never started"
    );
    assert!(eventually(|| !m.is_locked(&id)).await, "the turn never ended");

    // The row is delivered and a turn ran for it — even though that turn's
    // process is long gone.
    assert!(
        m.store.stranded_deliveries().unwrap().is_empty(),
        "a turn that STARTED is never treated as stranded, however it ended"
    );

    m.recover().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        delivered(&log, &id),
        vec![mid],
        "DUPLICATE EXECUTION: an interrupted turn was re-run instead of buried"
    );
}

/// Recovery is SCOPED TO THE THREAD THAT LOST ITS TURN — a stranded batch on
/// one thread never disturbs a healthy neighbour.
#[tokio::test]
async fn recovery_touches_only_the_thread_that_was_stranded() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let log = watch(&m);

    let healthy = m.create_thread(None, plain(tmp.path())).unwrap();
    let stranded_id = m.create_thread(None, plain(tmp.path())).unwrap();

    let healthy_mid = m
        .send(
            &healthy,
            "ran fine".to_string(),
            SendIntent::Immediate,
            serde_json::Value::Null,
            false,
        )
        .unwrap();
    assert!(
        eventually(|| delivered(&log, &healthy).contains(&healthy_mid) && !m.is_locked(&healthy))
            .await,
        "the healthy thread never ran"
    );

    let held = m.try_lock_thread(&stranded_id, "test").unwrap();
    let lost_mid = m
        .send(
            &stranded_id,
            "lost to a restart".to_string(),
            SendIntent::Immediate,
            serde_json::Value::Null,
            false,
        )
        .unwrap();
    strand(&m, &stranded_id);
    held.release();

    m.recover().await;

    assert!(
        eventually(|| delivered(&log, &stranded_id) == vec![lost_mid]).await,
        "the stranded thread's message never came back"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        delivered(&log, &healthy),
        vec![healthy_mid],
        "DUPLICATE EXECUTION: a healthy thread's message was re-run by another thread's recovery"
    );
}

// ── 3. the corpse that held the lock ─────────────────────────────────────

/// Leave a real, unreaped zombie behind and hand back its pid.
///
/// `std::process::Child` does not reap on drop, so a child we never `wait()`
/// stays in the process table as a zombie — the same shape a daemon that died
/// mid-restart leaves under a parent that is not waiting on it.
fn unreaped_corpse() -> (std::process::Child, i64) {
    let corpse = std::process::Command::new("true").spawn().unwrap();
    let pid = corpse.id() as i64;
    // `true` exits at once; give the kernel a moment to mark it.
    for _ in 0..200 {
        if is_zombie(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    (corpse, pid)
}

fn is_zombie(pid: i64) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().starts_with('Z')
}

/// A LOCK HELD BY AN UNREAPED CORPSE IS RECLAIMED.
///
/// `kill(pid, 0)` succeeds on a zombie, so a liveness check built only on it
/// reads a corpse as a working daemon, declines to steal its lock, and refuses
/// every dispatch on that thread from then on. That is not a stall — it is
/// permanent, and no amount of queued work ever moves again.
#[tokio::test]
async fn a_lock_held_by_an_unreaped_corpse_is_reclaimed_at_boot() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    let (mut corpse, zombie_pid) = unreaped_corpse();
    assert!(
        is_zombie(zombie_pid),
        "precondition: the test needs a real unreaped zombie to be meaningful"
    );

    m.store
        .try_claim_thread(&id, "daemon-that-died-mid-restart", zombie_pid, "dispatch")
        .unwrap();

    m.recover().await;

    assert!(
        m.store.get_thread_lock(&id).unwrap().is_none(),
        "a lock held by a CORPSE must be reclaimed — `kill(pid, 0)` succeeding on a zombie is \
         not evidence of a working daemon, and believing it strands the thread forever"
    );

    corpse.wait().ok();
}

/// THE WHOLE FAILURE, END TO END — the operator's Minecraft thread, rebuilt.
///
/// Both halves of the live incident at once: the messages are marked delivered
/// with no turn behind them, AND the thread's lock is held by the pid of the
/// daemon that died holding it, which is still a zombie. Either one alone is
/// enough to silence the thread forever. Recovery has to clear both, and then
/// the thread has to actually answer.
#[tokio::test]
async fn a_thread_stranded_by_a_restart_runs_again_after_one_boot() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    let held = m.try_lock_thread(&id, "test").unwrap();
    let mut sent = Vec::new();
    for i in 0..3 {
        sent.push(
            m.send(
                &id,
                format!("said before the restart {i}"),
                SendIntent::Immediate,
                serde_json::Value::Null,
                false,
            )
            .unwrap(),
        );
    }
    strand(&m, &id);
    held.release();

    // ...and the dead daemon's claim is still on the thread.
    let (mut corpse, zombie_pid) = unreaped_corpse();
    assert!(is_zombie(zombie_pid), "precondition: a real zombie");
    m.store
        .try_claim_thread(&id, "daemon-that-died-mid-restart", zombie_pid, "dispatch")
        .unwrap();

    let log = watch(&m);
    m.recover().await;

    assert!(
        eventually(|| delivered(&log, &id).len() == 3).await,
        "the thread never spoke again: delivered {:?}, lock {:?}",
        delivered(&log, &id),
        m.store.get_thread_lock(&id).unwrap().map(|r| r.boot_id)
    );
    assert_eq!(
        delivered(&log, &id),
        sent,
        "every message the operator sent before the restart is answered, in order, exactly once"
    );

    corpse.wait().ok();
}

// ── 4. the race that caused it ───────────────────────────────────────────

/// THE PORT IS THE ONLY MUTUAL EXCLUSION BETWEEN TWO DAEMONS, so it is taken
/// before recovery runs.
///
/// There is no pidfile, no advisory lock on `threads.db`, no `daemons` table —
/// the bind is all there is. Before this change `run_daemon` recovered first
/// and bound second, so the loser of a two-daemon race consumed the operator's
/// messages and only then discovered it should not have existed.
#[tokio::test]
async fn a_second_daemon_loses_the_port_before_it_can_consume_anything() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    // A message waiting for the boot sweep — what the loser would have eaten.
    let held = m.try_lock_thread(&id, "test").unwrap();
    let mid = m
        .send(
            &id,
            "must survive the losing daemon".to_string(),
            SendIntent::Immediate,
            serde_json::Value::Null,
            false,
        )
        .unwrap();
    held.release();

    // The winner owns the port.
    let winner = threads::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let addr = winner.local_addr().unwrap();

    // The loser's boot: the bind is attempted BEFORE `recover()`, so it fails
    // here and the queue is never touched.
    let loser = threads::server::bind(addr).await;
    assert!(
        loser.is_err(),
        "two daemons must not both own the port — the bind is the only thing that stops them"
    );

    let waiting = m.store.waiting_queue(&id).unwrap();
    assert_eq!(
        waiting.len(),
        1,
        "a daemon that lost the port must not have consumed the queue"
    );
    assert_eq!(waiting[0].message_id, mid);
    assert!(
        m.store.stranded_deliveries().unwrap().is_empty(),
        "nothing should have been marked delivered by the losing daemon"
    );

    drop(winner);
}

/// SOURCE GUARD: `run_daemon` binds before it recovers.
///
/// The ordering is the whole fix and it is invisible at runtime in-process, so
/// it is pinned in the source. If someone moves `recover()` back above the
/// bind, this fails and says why.
#[test]
fn run_daemon_takes_the_port_before_recovery_dispatches_anything() {
    let src = include_str!("../../src/lib.rs");
    let body = src
        .split_once("pub async fn run_daemon")
        .expect("run_daemon must exist")
        .1;
    let bind_at = body.find("server::bind").expect("run_daemon must bind the port");
    let recover_at = body.find(".recover()").expect("run_daemon must recover");
    assert!(
        bind_at < recover_at,
        "run_daemon must BIND before it RECOVERS: recovery dispatches real turns and marks real \
         queue rows delivered, and the bind is the only mutual exclusion between two daemons. \
         Recovering first means the loser of a restart race destroys the operator's messages \
         before it learns it should not exist."
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
