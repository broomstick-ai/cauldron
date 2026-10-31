//! THE THREAD EXECUTION LOCK — operator msg #1213, intent item 45.
//!
//! > "You need a locking mechanism inside of threads. And the lock needs to be
//! >  built in. It is impossible to run the same thread two times. Impossible.
//! >  ... If we ever want to have the same thread run twice, we can create a
//! >  forking behavior later. But right now, it should never be possible."
//!
//! WHAT WENT WRONG (verified live, 2026-07-31 19:0x–19:41). One session had up
//! to FOUR live `claude --resume` processes at once on ONE thread, and the
//! operator heard the same answer twice, repeatedly. The daemon had done
//! nothing wrong: `threads.send` was accepted, the turn was QUEUED behind a
//! running one, and no `turn_started` arrived within the client's 15s accept
//! deadline — so AUI called it a transport failure and spawned the turn
//! DIRECTLY. The message then ran twice: once on the direct path immediately,
//! once from the daemon's queue when the thread freed.
//!
//! The client half of that is fixed in `apps/aui/src/threads_client.rs` (an
//! accepted send is never a fallback trigger). But client politeness is not
//! what the operator asked for. These tests hold the SERVICE-SIDE line: with
//! the lock in place, no client — polite, buggy, or malicious — can obtain a
//! second concurrent execution of one thread.
//!
//! Every test drives the manager in-process against the FAKE claude fixture:
//! no real claude turn, no `~/.threads` (every manager gets an explicit
//! `data_dir` under a tempdir), no login, no keychain, no AUI.

#[path = "support/keychain.rs"]
mod keychain;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::event::{Push, SendIntent, ThreadEvent};
use threads::runtime::LockRefusal;
use threads::settings::ThreadSettings;
use threads::store::{ParkedTurnRow, RunningTurnRow};
use threads::{build_manager, Config, ThreadManager};

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env() {
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
    // the tempdir. Nothing here is about accounts — these tests are about the
    // lock — but every test creates a thread, and a thread has to be PLACED.
    // The machine `default` that `ensure_default` registers on every boot is
    // the operator's own `~/.claude.json` login and is no longer a placement
    // target, so a config with no accounts of its own can no longer create a
    // thread at all. `aa-owned` sorts first, so it takes the alphabetical rank
    // `default` used to hold and the placement these tests get is unchanged.
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

/// Every ledger event, in firehose order — the only place `turn_started` /
/// `turn_ended` are visible (they carry no message id, so the ledger reader
/// filters them out).
type Log = Arc<Mutex<Vec<ThreadEvent>>>;

fn watch(m: &ThreadManager) -> Log {
    let mut rx = m.subscribe();
    let seen: Log = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Event(ev) = p {
                out.lock().unwrap().push(ev.event);
            }
        }
    });
    seen
}

/// Every `Push::Status` frame, in order.
type Statuses = Arc<Mutex<Vec<(String, String, u64)>>>;

fn watch_status(m: &ThreadManager) -> Statuses {
    let mut rx = m.subscribe();
    let seen: Statuses = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Status {
                thread_id,
                state,
                queued,
                ..
            } = p
            {
                out.lock().unwrap().push((thread_id, state, queued));
            }
        }
    });
    seen
}

fn starts(log: &Log) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, ThreadEvent::TurnStarted { .. }))
        .count()
}

/// THE INVARIANT, read straight off the ordered event stream: a `turn_started`
/// must never appear while a previous turn has not yet ended. Returns the
/// deepest concurrency observed — 1 means one execution at a time, 2 means the
/// bug is back.
fn max_overlap(log: &Log) -> i32 {
    let mut depth = 0i32;
    let mut worst = 0i32;
    for e in log.lock().unwrap().iter() {
        match e {
            ThreadEvent::TurnStarted { .. } => {
                depth += 1;
                worst = worst.max(depth);
            }
            ThreadEvent::TurnEnded { .. } | ThreadEvent::TurnInterrupted { .. } => {
                depth = (depth - 1).max(0);
            }
            _ => {}
        }
    }
    worst
}

/// Every message id that reached a prompt, in delivery order.
fn delivered(log: &Log) -> Vec<u64> {
    let mut out = Vec::new();
    for e in log.lock().unwrap().iter() {
        if let ThreadEvent::TurnStarted { message_ids, .. } = e {
            out.extend(message_ids.iter().copied());
        }
    }
    out
}

// ── 1. the storm: many sends, one execution ──────────────────────────────

/// A CONCURRENT SEND STORM on one thread yields exactly one execution at a
/// time, and every message is delivered, in order, exactly once.
///
/// This is the live failure reproduced as a fixture: eight sends land while a
/// slow turn runs. Before the lock, the ceiling on concurrent executions was
/// whatever the client's retry behavior produced (four, live). After it, the
/// ceiling is structural.
#[tokio::test]
async fn a_send_storm_on_one_thread_never_produces_two_executions() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let log = watch(&m);
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    let mut sent = Vec::new();
    for i in 0..8 {
        // `slowturn` holds the thread for 0.7s, so every send after the first
        // lands while an execution is live — the exact window the duplicate
        // came through.
        let mid = m
            .send(
                &id,
                format!("slowturn storm {i}"),
                SendIntent::Immediate,
                serde_json::Value::Null,
                false,
            )
            .unwrap();
        sent.push(mid);
    }

    // Everything drains eventually.
    assert!(
        eventually(|| m.store.waiting_queue(&id).unwrap().is_empty() && !m.is_locked(&id)).await,
        "the queue never drained: {:?}",
        m.store.waiting_queue(&id).unwrap().len()
    );
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        max_overlap(&log),
        1,
        "TWO EXECUTIONS OVERLAPPED on one thread — the lock did not hold"
    );
    let got = delivered(&log);
    assert_eq!(
        got,
        sent,
        "messages must be delivered in order, exactly once each (never dropped, never merged \
         away, never re-run)"
    );
    assert!(starts(&log) >= 1);
}

// ── 2. the refusal, per entry point ──────────────────────────────────────

/// While the lock is held, `threads.send` QUEUES — it does not start a second
/// execution, and it does not drop the message.
#[tokio::test]
async fn send_queues_behind_the_lock_instead_of_starting_a_second_execution() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let log = watch(&m);
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    // Hold the thread by hand — standing in for an execution in flight.
    let held = m.try_lock_thread(&id, "test").unwrap();
    assert!(m.is_locked(&id));

    let mid = m
        .send(
            &id,
            "hello".into(),
            SendIntent::Immediate,
            serde_json::Value::Null,
            false,
        )
        .unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(starts(&log), 0, "a locked thread must not start a turn");
    assert_eq!(
        m.store.waiting_queue(&id).unwrap().len(),
        1,
        "the message must still be WAITING — never dropped, never merged"
    );
    // What AUI is told: the thread is WORKING and a message of the operator's
    // is waiting behind it. Both facts, on one frame — that is the whole of
    // "the operator must be able to tell, hands-free, that a thread is still
    // working and that a queued message is waiting".
    let s = m.thread_status(&id).unwrap();
    assert_eq!(s["state"], "active");
    assert_eq!(s["queued"], 1);

    // Free it: the queued message runs, on its own, with nobody poking it.
    drop(held);
    m.resume_sweep();
    assert!(
        eventually(|| starts(&log) == 1).await,
        "the queued message must run once the lock frees"
    );
    assert_eq!(delivered(&log), vec![mid]);
    assert_eq!(max_overlap(&log), 1);
}

/// PARKED-TURN REDELIVERY is locked. A budget-killed prompt waiting in
/// `parked_turns` is re-dispatched by the resume sweep; that path takes the
/// lock like any other, so it cannot run alongside a live execution.
#[tokio::test]
async fn parked_turn_redelivery_is_locked() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let log = watch(&m);
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    m.store
        .upsert_parked_turn(&ParkedTurnRow {
            thread_id: id.clone(),
            prompt: "parked work".into(),
            message_ids: vec![1],
            attempt: 1,
            reason: "test".into(),
            budget_key: None,
            parked_at: threads::store::now_rfc3339(),
        })
        .unwrap();

    let held = m.try_lock_thread(&id, "test").unwrap();
    for _ in 0..6 {
        m.resume_sweep();
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert_eq!(
        starts(&log),
        0,
        "the parked prompt must not be redelivered into a locked thread"
    );
    assert!(
        m.store.get_parked_turn(&id).unwrap().is_some(),
        "and it must still be parked — a refusal never consumes work"
    );

    drop(held);
    m.resume_sweep();
    assert!(eventually(|| starts(&log) == 1).await);
    assert_eq!(max_overlap(&log), 1);
}

/// The RETRY entry point takes the same lock. `fire_retry` claims with
/// `entry = "retry"`; this asserts that claim is refused while the thread is
/// held, which is precisely why a re-armed retry re-arms itself for later
/// instead of becoming the second instance.
#[tokio::test]
async fn the_retry_entry_point_is_refused_while_the_thread_is_locked() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    let held = m.try_lock_thread(&id, "dispatch").unwrap();
    assert_eq!(
        m.try_lock_thread(&id, "retry").err(),
        Some(LockRefusal::InProcess),
        "a retry must not be able to claim a thread an execution already holds"
    );
    drop(held);
    assert!(
        m.try_lock_thread(&id, "retry").is_ok(),
        "and it must succeed once the thread frees"
    );
}

/// BOOT RE-ADOPTION holds the lock for the orphan it adopts.
///
/// A daemon restarted mid-turn re-adopts the still-running child. Without a
/// lock, the very first send after that restart would dispatch a SECOND
/// execution alongside the orphan — which is the same duplicate by a different
/// door.
#[tokio::test]
async fn boot_re_adoption_holds_the_lock_for_the_orphan() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let log = watch(&m);
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    // A live "child": a real process we own, that outlives the assertions.
    let mut child = std::process::Command::new("sleep")
        .arg("5")
        .spawn()
        .expect("spawn sleep");
    let out_path = tmp.path().join("orphan.jsonl");
    std::fs::write(&out_path, "").unwrap();
    m.store
        .upsert_running_turn(&RunningTurnRow {
            thread_id: id.clone(),
            pid: child.id() as i64,
            spawn_cookie: "cookie".into(),
            output_path: out_path.to_string_lossy().to_string(),
            byte_offset: 0,
            started_at: threads::store::now_rfc3339(),
            prompt: None,
            message_ids: vec![],
            attempt: 1,
        })
        .unwrap();

    m.recover().await;
    assert!(
        eventually(|| m.is_locked(&id)).await,
        "the re-adopted orphan must hold this thread's lock"
    );
    assert_eq!(m.thread_status(&id).unwrap()["state"], "active");

    // A send now must NOT start a turn next to the orphan.
    m.send(
        &id,
        "while adopted".into(),
        SendIntent::Immediate,
        serde_json::Value::Null,
        false,
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        starts(&log),
        0,
        "a send during re-adoption must queue, not start a second execution"
    );

    // When the orphan exits, the thread frees and the queued send runs — once.
    child.kill().ok();
    child.wait().ok();
    assert!(eventually(|| starts(&log) == 1).await);
    assert_eq!(max_overlap(&log), 1);
}

// ── 3. the reclaim rule ──────────────────────────────────────────────────

/// A DEAD holder's claim is reclaimed. A daemon that crashed holding a lock
/// must not strand the thread forever — the operator would simply find one
/// thread permanently mute.
#[tokio::test]
async fn a_dead_holders_claim_is_reclaimed() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let log = watch(&m);
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    // A claim from a daemon that no longer exists: a pid that has certainly
    // exited (we start it and reap it), under a foreign boot id.
    let mut corpse = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = corpse.id() as i64;
    corpse.wait().unwrap();
    m.store
        .try_claim_thread(&id, "boot-of-a-crashed-daemon", dead_pid, "dispatch")
        .unwrap();
    assert!(m.store.get_thread_lock(&id).unwrap().is_some());
    assert!(
        !m.is_locked(&id),
        "a dead holder is not a live execution, so status must not claim it is"
    );

    m.send(
        &id,
        "after the crash".into(),
        SendIntent::Immediate,
        serde_json::Value::Null,
        false,
    )
    .unwrap();
    assert!(
        eventually(|| starts(&log) == 1).await,
        "a thread held by a DEAD daemon must be reclaimable, not deadlocked forever"
    );

    let row = m.store.get_thread_lock(&id).unwrap();
    assert!(
        row.is_none() || row.unwrap().boot_id != "boot-of-a-crashed-daemon",
        "the stale claim must be gone"
    );
}

/// A LIVE holder's claim is NEVER stolen. This is the other half of the
/// reclaim rule, and the half that keeps the directive true: "alive → wait".
#[tokio::test]
async fn a_live_holders_claim_is_never_stolen() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let log = watch(&m);
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    // Another daemon, alive: a process we hold open for the whole test.
    let mut other = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let live_pid = other.id() as i64;
    m.store
        .try_claim_thread(&id, "boot-of-another-live-daemon", live_pid, "dispatch")
        .unwrap();
    assert!(m.is_locked(&id), "a live foreign holder IS a live execution");

    m.send(
        &id,
        "into a foreign lock".into(),
        SendIntent::Immediate,
        serde_json::Value::Null,
        false,
    )
    .unwrap();
    for _ in 0..6 {
        m.resume_sweep();
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert_eq!(
        starts(&log),
        0,
        "a thread held by a LIVE daemon must never be run by a second one"
    );
    assert_eq!(
        m.store.get_thread_lock(&id).unwrap().unwrap().boot_id,
        "boot-of-another-live-daemon",
        "and its claim must be untouched"
    );
    assert_eq!(
        m.store.waiting_queue(&id).unwrap().len(),
        1,
        "the message waits; it is never dropped"
    );

    other.kill().ok();
    other.wait().ok();
    // Now the holder is dead, so the same send proceeds — reclaimed, not lost.
    m.resume_sweep();
    assert!(eventually(|| starts(&log) == 1).await);

    // Cleanup so the tempdir teardown is quiet.
    let _ = eventually(|| !m.is_locked(&id)).await;
}

/// The boot sweep reclaims dead holders' rows so `threads.status` tells the
/// truth about every thread from the daemon's first second — not only about
/// the threads somebody has since sent to.
#[tokio::test]
async fn boot_reclaims_dead_locks_and_leaves_live_ones_alone() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let dead_thread = m.create_thread(None, plain(tmp.path())).unwrap();
    let live_thread = m.create_thread(None, plain(tmp.path())).unwrap();

    let mut corpse = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = corpse.id() as i64;
    corpse.wait().unwrap();
    let mut other = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let live_pid = other.id() as i64;

    m.store
        .try_claim_thread(&dead_thread, "crashed", dead_pid, "dispatch")
        .unwrap();
    m.store
        .try_claim_thread(&live_thread, "still-running", live_pid, "dispatch")
        .unwrap();

    m.recover().await;

    assert!(
        m.store.get_thread_lock(&dead_thread).unwrap().is_none(),
        "the dead daemon's claim must be swept at boot"
    );
    assert_eq!(
        m.store
            .get_thread_lock(&live_thread)
            .unwrap()
            .unwrap()
            .boot_id,
        "still-running",
        "a LIVE holder's claim must survive another daemon's boot untouched"
    );

    other.kill().ok();
    other.wait().ok();
}

// ── 4. status (the second half of the directive) ─────────────────────────

/// The status surface answers the two questions the operator asked to be able
/// to hear hands-free: is this thread still working, and is a message of mine
/// waiting behind it.
#[tokio::test]
async fn status_reports_active_then_queued_then_idle() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let frames = watch_status(&m);
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    assert_eq!(m.thread_status(&id).unwrap()["state"], "idle");

    m.send(
        &id,
        "slowturn one".into(),
        SendIntent::Immediate,
        serde_json::Value::Null,
        false,
    )
    .unwrap();
    assert!(
        eventually(|| m.thread_status(&id).unwrap()["state"] == "active").await,
        "a running thread must read ACTIVE"
    );

    m.send(
        &id,
        "slowturn two".into(),
        SendIntent::Immediate,
        serde_json::Value::Null,
        false,
    )
    .unwrap();
    let s = m.thread_status(&id).unwrap();
    assert_eq!(s["state"], "active");
    assert_eq!(s["queued"], 1, "and it must say a message is WAITING");

    assert!(
        eventually(|| m.thread_status(&id).unwrap()["state"] == "idle").await,
        "and IDLE when it is finally done"
    );

    // The third word in the operator's vocabulary: work accepted, nothing
    // running it yet (a `turn_end` message held behind still-live subagents,
    // or a thread whose holder has not freed it).
    m.store.enqueue(&id, 999, "held back", "turn_end", &serde_json::Value::Null, &[]).unwrap();
    assert_eq!(m.thread_status(&id).unwrap()["state"], "queued");
    assert!(m.store.cancel_queued(&id, 999).unwrap());

    let seen: Vec<String> = frames
        .lock()
        .unwrap()
        .iter()
        .map(|(_, s, _)| s.clone())
        .collect();
    assert!(
        seen.iter().any(|s| s == "active"),
        "an ACTIVE status frame must reach subscribers over the event stream, \
         not merely be queryable: {seen:?}"
    );
    assert!(
        seen.iter().any(|s| s == "idle"),
        "and so must the transition back to idle: {seen:?}"
    );
    assert!(
        frames.lock().unwrap().iter().any(|(_, _, q)| *q > 0),
        "and the queued COUNT must ride on the frame"
    );
}

/// Unread is counted against a durable seen cursor, and the cursor only ever
/// moves forward.
#[tokio::test]
async fn unread_counts_above_the_seen_cursor_and_the_cursor_is_monotonic() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    m.send(
        &id,
        "one".into(),
        SendIntent::Immediate,
        serde_json::Value::Null,
        false,
    )
    .unwrap();
    assert!(eventually(|| m.thread_status(&id).unwrap()["state"] == "idle").await);

    let before = m.thread_status(&id).unwrap()["unread"].as_u64().unwrap();
    assert!(before > 0, "a fresh thread's messages are unread");

    let head = m.store.max_seq(&id).unwrap();
    m.store.mark_seen(&id, head).unwrap();
    assert_eq!(
        m.thread_status(&id).unwrap()["unread"],
        0,
        "reading to the head clears unread"
    );

    // A stale client reporting an OLD cursor must not un-read what the
    // operator has already heard.
    m.store.mark_seen(&id, 1).unwrap();
    assert_eq!(m.store.last_seen_seq(&id).unwrap(), head);
    assert_eq!(m.thread_status(&id).unwrap()["unread"], 0);
}

// ── 5. the structural guarantee ──────────────────────────────────────────

/// EVERY ENTRY POINT IS COVERED — asserted against the source, so a new one
/// cannot be added silently.
///
/// Two facts together make "start an execution without the lock" unwritable:
///
///  1. `run_turn` and `adopt_turn` take a `TurnLock` BY VALUE, so the compiler
///     refuses any caller that has not claimed the thread. That is checked by
///     the build, not by a test.
///  2. There are exactly four claim sites, one per entry point, and this test
///     pins their names. A fifth path that needs to execute a thread has to
///     add its `lock_turn` call — and then update this list on purpose.
#[test]
fn every_execution_entry_point_claims_the_lock() {
    let src = include_str!("../../src/runtime.rs");

    for entry in ["dispatch", "parked", "retry", "adopt"] {
        let needle = format!("lock_turn(&state, \"{entry}\")");
        assert!(
            src.contains(&needle) || src.contains(&format!("lock_turn(state, \"{entry}\")")),
            "the `{entry}` entry point no longer claims the thread lock"
        );
    }

    // No path may set `turn_running` by hand any more: the flag is the lock's
    // internal business, and reaching around it is how the second execution
    // used to become possible.
    let hand_sets = src.matches("turn_running.store(true").count()
        + src.matches("turn_running.swap(true").count();
    assert_eq!(
        hand_sets, 1,
        "`turn_running` must be set in exactly ONE place — inside `lock_turn`. \
         A path that sets it by hand has bypassed the durable claim, which is \
         the whole lock as far as a second daemon is concerned."
    );

    // And every start of an execution goes through the two locked runners.
    for runner in ["fn run_turn(", "fn adopt_turn("] {
        let at = src.find(runner).expect("runner renamed — re-point this test");
        let sig_end = src[at..].find(')').unwrap();
        let sig = &src[at..at + sig_end + 1];
        assert!(
            sig.contains("lock: TurnLock") || src[at..at + 400].contains("lock: TurnLock"),
            "`{runner}` must take the lock BY VALUE so an unlocked caller cannot compile"
        );
    }
}

/// FORKING IS OUT OF SCOPE and must stay impossible (the operator: "If we ever
/// want to have the same thread run twice, we can create a forking behavior
/// later. But right now, it should never be possible.").
///
/// The lock has no bypass — no force flag, no "run anyway", no second claim
/// under any entry name. This test is the guard on that: it tries every entry
/// name against a held thread and requires all of them to be refused.
#[tokio::test]
async fn there_is_no_bypass_under_any_entry_name() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, plain(tmp.path())).unwrap();

    let held = m.try_lock_thread(&id, "dispatch").unwrap();
    for entry in ["dispatch", "parked", "retry", "adopt", "cli", "anything"] {
        assert!(
            m.try_lock_thread(&id, entry).is_err(),
            "`{entry}` obtained a SECOND concurrent claim on one thread"
        );
    }
    drop(held);
    assert!(m.try_lock_thread(&id, "dispatch").is_ok());
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
