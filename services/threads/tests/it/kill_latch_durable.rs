//! **R9.L4 (GAP-AUDIT L-4): A BUDGET KILL LATCHES DURABLY, PER BUDGET KEY.**
//!
//! The latch exists because a kill fires on the mid-turn ESTIMATE and the
//! epilogue then reconciles the meter back UNDER the limit (the `killcheap`
//! fixture reproduces exactly that), so the meter alone would redispatch the
//! parked prompt into the same kill — a billable livelock. Until R9.L4 the
//! latch was an in-memory field on ONE thread's state, which left three holes:
//!
//! - (a) a RESTART forgot it — one paid kill per restart;
//! - (b) only a limit edit released it — never the window rolling over;
//! - (c) a SIBLING thread on the same key was not held by it.
//!
//! Every test here first asserts the meter reads UNDER the limit after the
//! kill, so "stays parked" is a test of the latch and not of the meter.
//!
//! Each daemon life is its own tokio runtime; the restart is
//! `Runtime::shutdown_background()` then a fresh `build_manager` + `recover()`
//! over the same data dir (the shape `managed_runtime.rs` uses). No real
//! claude is spawned: `THREADS_CLAUDE_BIN` is `tests/fixtures/fake-claude`.
//!
//! The rollover is driven by a GAUGE OBSERVATION, not a frozen clock: the
//! crate's frozen clock is thread-local (`clock::testing::freeze_at`) and the
//! admission gate runs on the runtime's worker threads, so it cannot reach
//! them. A fresh cash-axis observation with no reset instant moves the cash
//! window's start to the observation (`Budgets::window_start`'s *assumed*
//! basis) — the same rollover the meter itself reads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::json;
use threads::config::Config;
use threads::{build_manager, ThreadEvent, ThreadManager};

#[path = "support/first_exec.rs"]
mod first_exec;

/// The env is process-wide and each test points it at its own tmp dir.
static SERIAL: Mutex<()> = Mutex::new(());

const LIMIT: f64 = 0.01;

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env(tmp: &Path) {
    std::env::set_var("CAULDRON_HOME", tmp.join("cauldron"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    // The periodic sweep is pushed out past every test; only the explicit
    // `resume_sweep` calls below redeliver anything.
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

/// One daemon life.
struct Life {
    rt: tokio::runtime::Runtime,
    m: ThreadManager,
}

impl Life {
    fn boot(dir: &Path) -> Life {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let m = rt.block_on(async {
            threads::clock::testing::ticking();
            let m = build_manager(config_at(dir)).unwrap();
            m.recover().await;
            m
        });
        Life { rt, m }
    }

    /// Every task stops; nothing after this instant runs.
    fn kill(self) {
        let Life { rt, m } = self;
        rt.shutdown_background();
        drop(m);
    }

    fn until(&self, secs: u64, mut f: impl FnMut(&ThreadManager) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if f(&self.m) {
                return true;
            }
            self.rt.block_on(async { tokio::time::sleep(Duration::from_millis(25)).await });
        }
        f(&self.m)
    }

    /// Sweep every parked and queued thread, then give anything it started
    /// time to reach `turn_started`. 1 s outlasts the 25 ms tail poll and the
    /// dispatch spawn by a wide margin; the 60 s periodic tick is disabled.
    fn sweep(&self) {
        self.rt.block_on(async {
            self.m.resume_sweep();
            tokio::time::sleep(Duration::from_millis(1_000)).await;
        });
    }

    fn set_key(&self, key: &str, cash: f64) {
        let cfg: threads::budgets::BudgetConfig = serde_json::from_value(json!({
            "key": key, "cash_usd": cash, "on_trip": "kill", "accounts": ["aa-owned"],
        }))
        .unwrap();
        self.m.budgets.set_config(&cfg).unwrap();
    }

    fn thread_on(&self, dir: &Path, key: &str) -> String {
        let settings = json!({
            "cwd": dir.to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
            "budget_key": key,
        });
        let _ctx = self.rt.enter();
        self.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    }

    fn send(&self, id: &str, text: &str) {
        let _ctx = self.rt.enter();
        self.m
            .send(id, text.into(), threads::SendIntent::Immediate, json!({}), false)
            .unwrap();
    }

    fn cancel(&self, id: &str) {
        let _ctx = self.rt.enter();
        self.m.cancel(id).unwrap();
    }

    fn consumed(&self, key: &str) -> f64 {
        let snap = self.m.budgets.snapshot(key, &self.m.account_pool()).unwrap().unwrap().to_json();
        snap["cash"]["consumed_usd"].as_f64().unwrap()
    }

    /// Kill a `killcheap` turn on `id` and wait until it is parked, idle, and
    /// its epilogue has reconciled the meter back UNDER the limit — the state
    /// in which only the latch can hold anything.
    fn kill_turn(&self, id: &str, key: &str) {
        self.send(id, "killcheap: overshoot then flush a cheap result");
        assert!(
            self.until(30, |m| m.store.get_parked_turn(id).unwrap().is_some()
                && m.store.get_running_turn(id).unwrap().is_none()),
            "the budget kill parks {id}"
        );
        self.sweep();
        let consumed = self.consumed(key);
        assert!(
            consumed < LIMIT,
            "{key}: reconciliation must leave the meter under the limit (got {consumed}), \
             or the meter alone would hold the thread and this proves nothing about the latch"
        );
    }
}

fn events(m: &ThreadManager, id: &str) -> Vec<ThreadEvent> {
    m.store.tail_events(id, 1000).unwrap().into_iter().map(|e| e.event).collect()
}

fn started(m: &ThreadManager, id: &str) -> usize {
    events(m, id).iter().filter(|e| matches!(e, ThreadEvent::TurnStarted { .. })).count()
}

/// A fresh cash-axis gauge reading on `aa-owned` with no reset instant: the
/// cash window now opens at this observation (the *assumed* basis), i.e. it
/// has ROLLED OVER past everything charged before it.
fn roll_cash_window(m: &ThreadManager) {
    m.store
        .record_observation(&threads::store::ObservationRow {
            id: 0,
            account: "aa-owned".into(),
            axis: "cash".into(),
            pct: 0.0,
            resets_at: None,
            source: "test-rollover".into(),
            observed_at: threads::store::now_rfc3339(),
        })
        .unwrap();
}

/// **(a) THE LATCH SURVIVES A RESTART.** Kill → kill the daemon → boot a new
/// manager on the same data dir → sweep: the parked prompt stays parked.
/// Red with a memory-only latch: the new life's meter reads under the limit
/// and redelivers the prompt (a second `turn_started`).
///
/// Paired (oppose the forcing): in the same new life, raising the limit
/// releases it and the prompt runs — so "never redeliver after a restart"
/// fails here too.
#[test]
fn a_kill_latch_survives_a_restart_and_the_limit_still_releases_it() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let id = {
        let a = Life::boot(tmp.path());
        a.set_key("l4-restart", LIMIT);
        let id = a.thread_on(tmp.path(), "l4-restart");
        a.kill_turn(&id, "l4-restart");
        assert_eq!(started(&a.m, &id), 1, "only the killed turn ran before the restart");
        a.kill();
        id
    };

    let b = Life::boot(tmp.path());
    assert!(b.consumed("l4-restart") < LIMIT, "the new life's meter reads under the limit too");
    b.sweep();
    b.sweep();
    assert_eq!(
        started(&b.m, &id),
        1,
        "after a restart the killed prompt must stay parked — the latch is durable"
    );
    assert!(b.m.store.get_parked_turn(&id).unwrap().is_some(), "and its row is still there");

    b.set_key("l4-restart", 500.0);
    b.sweep();
    assert!(
        b.until(15, |m| started(m, &id) == 2),
        "raising the limit releases the restored latch (turns started: {})",
        started(&b.m, &id)
    );
    b.kill();
}

/// **(b) THE LATCH RELEASES WHEN THE KILLED WINDOW ROLLS OVER.** Kill → sweep
/// (held) → the cash window rolls → sweep: the prompt runs exactly once more.
/// Red without the rollover release: it stays parked forever at an unchanged
/// limit. The opposing case is the first sweep, BEFORE the roll, which must
/// hold — so "always release" fails.
///
/// The redelivered prompt is `killcheap` again, so in the new window it trips
/// and is killed again, and the NEW latch holds it: "runs once", not twice.
#[test]
fn a_kill_latch_releases_when_the_window_rolls_over_and_the_prompt_runs_once() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    a.set_key("l4-roll", LIMIT);
    let id = a.thread_on(tmp.path(), "l4-roll");
    a.kill_turn(&id, "l4-roll");
    a.sweep();
    assert_eq!(started(&a.m, &id), 1, "before the rollover the latch holds");

    roll_cash_window(&a.m);
    a.sweep();
    assert!(
        a.until(15, |m| started(m, &id) == 2),
        "the window rolled over, so the latch released and the prompt ran (turns started: {})",
        started(&a.m, &id)
    );
    assert!(
        a.until(30, |m| m.store.get_parked_turn(&id).unwrap().is_some()
            && m.store.get_running_turn(&id).unwrap().is_none()),
        "the rerun tripped the same budget in the new window and parked again"
    );
    a.sweep();
    a.sweep();
    assert_eq!(started(&a.m, &id), 2, "the prompt ran exactly once after the rollover");
    a.kill();
}

/// **(c) A SIBLING ON THE SAME KEY IS HELD; A THREAD ON ANOTHER KEY IS NOT;
/// CANCELLING THE KILLED THREAD DOES NOT RELEASE THE SIBLING.**
///
/// - sibling: a message sent after the kill is not admitted (red with a
///   per-thread latch: its meter reads under the limit, so it runs);
/// - other key (opposing case): the same message on a thread keyed to an
///   untouched budget runs — so "refuse everyone" fails;
/// - cancel: the operator cancels the KILLED thread, which releases that
///   thread's own park (its next message runs, R9.L3) — and the sibling stays
///   held (red if the cancel cleared the key's latch).
/// - finally the limit is raised and the sibling runs, so the hold is the
///   latch and not something else stranding it.
#[test]
fn a_kill_latch_holds_siblings_on_its_key_and_a_cancel_does_not_release_them() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    a.set_key("l4-sib", LIMIT);
    a.set_key("l4-other", LIMIT);
    let killed = a.thread_on(tmp.path(), "l4-sib");
    let sibling = a.thread_on(tmp.path(), "l4-sib");
    let other = a.thread_on(tmp.path(), "l4-other");
    a.kill_turn(&killed, "l4-sib");

    a.send(&sibling, "hello from the sibling");
    a.send(&other, "hello from another key");
    assert!(
        a.until(15, |m| started(m, &other) == 1),
        "a thread on another key is not held by this key's kill"
    );
    a.sweep();
    assert_eq!(started(&a.m, &sibling), 0, "the sibling on the killed key is held by the latch");
    let ps = a.m.park_state(&sibling).unwrap();
    assert!(
        ps["reason"].as_str().is_some_and(|r| r.contains("l4-sib") && r.contains(&killed)),
        "the sibling's park names the key and the killed thread: {ps}"
    );

    a.cancel(&killed);
    assert!(a.m.store.get_parked_turn(&killed).unwrap().is_none(), "the cancel dropped the killed row");
    a.send(&killed, "after the cancel");
    assert!(
        a.until(15, |m| started(m, &killed) == 2),
        "the cancel released the killed thread's OWN park (turns started: {})",
        started(&a.m, &killed)
    );
    a.sweep();
    a.sweep();
    assert_eq!(started(&a.m, &sibling), 0, "cancelling the killed thread must not release its sibling");

    a.set_key("l4-sib", 500.0);
    a.sweep();
    assert!(
        a.until(15, |m| started(m, &sibling) == 1),
        "raising the limit releases the sibling (turns started: {})",
        started(&a.m, &sibling)
    );
    a.kill();
}

/// **(a)+(c) together: the sibling gate survives a restart, and so does the
/// cancel's narrowing** — after the restart the cancelled killed thread is
/// still released and the sibling is still held. Red if the cancel is not
/// read back (the killed thread re-latches) or the latch is not (the sibling
/// runs).
#[test]
fn after_a_restart_the_cancelled_thread_stays_released_and_the_sibling_stays_held() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let (killed, sibling) = {
        let a = Life::boot(tmp.path());
        a.set_key("l4-both", LIMIT);
        let killed = a.thread_on(tmp.path(), "l4-both");
        let sibling = a.thread_on(tmp.path(), "l4-both");
        a.kill_turn(&killed, "l4-both");
        a.cancel(&killed);
        a.kill();
        (killed, sibling)
    };
    let b = Life::boot(tmp.path());
    b.send(&sibling, "hello after the restart");
    b.send(&killed, "the cancelled thread speaks again");
    assert!(
        b.until(15, |m| started(m, &killed) == 2),
        "the cancel's own-park release survived the restart (turns started: {})",
        started(&b.m, &killed)
    );
    b.sweep();
    assert_eq!(started(&b.m, &sibling), 0, "the sibling is still held after the restart");
    b.kill();
}

/// **R9.L4b (R9.L6e inspect finding 1): A CANCEL THAT BEATS A BUDGET KILL
/// RELEASES THE KILLED THREAD'S OWN LATCH — in this daemon life, not only
/// after a restart.**
///
/// The race, staged deterministically with a 1000 ms tail poll: the poll that
/// reads `killcheap`'s expensive line trips the budget (the latch is set),
/// swaps the kill flag and SIGTERMs the child, which flushes a cheap result
/// and exits; the test sees the budget `turn_killed` and cancels before the
/// NEXT poll, which then reads both flags. The cancel wins (R9.L6d): the turn
/// lands as the operator's interrupt and no prompt is parked — so the only
/// release that existed (`cancel_turn`'s parked-row branch) never ran.
///
/// Red before R9.L4b: the meter reads under the limit, yet the thread's next
/// message is refused by the kill he already cancelled (turns started stays
/// 1). Opposed by the test below (a kill with no cancel still holds) and by
/// `a_kill_latch_holds_siblings_on_its_key_and_a_cancel_does_not_release_them`
/// (the cancel never releases a sibling).
#[test]
fn a_cancel_that_beats_a_budget_kill_releases_the_killed_threads_own_latch() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    std::env::set_var("THREADS_TAIL_POLL_MS", "1000");
    let a = Life::boot(tmp.path());
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    a.set_key("l4b-race", LIMIT);
    let id = a.thread_on(tmp.path(), "l4b-race");
    let sibling = a.thread_on(tmp.path(), "l4b-race");
    a.send(&id, "killcheap: overshoot then flush a cheap result");

    let killed = |m: &ThreadManager| {
        events(m, &id).iter().any(|e| {
            matches!(e, ThreadEvent::TurnKilled { budget_key: Some(k), .. } if k == "l4b-race")
        })
    };
    let ended = |m: &ThreadManager| {
        events(m, &id).iter().filter(|e| matches!(e, ThreadEvent::TurnEnded { .. })).count()
    };
    assert!(a.until(30, killed), "sentinel: the budget kill really fired");
    assert_eq!(ended(&a.m), 0, "sentinel: the cancel lands before the turn ends");
    a.cancel(&id);
    assert!(
        a.until(30, |m| ended(m) >= 1 && m.store.get_running_turn(&id).unwrap().is_none()),
        "the turn ends"
    );
    let ev = events(&a.m, &id);
    assert!(
        ev.iter().any(|e| matches!(e, ThreadEvent::TurnInterrupted { trigger, .. } if trigger == "operator")),
        "sentinel: the cancel won the race (operator interrupt recorded)"
    );
    assert!(
        !ev.iter().any(|e| matches!(e, ThreadEvent::TurnParked { .. })),
        "sentinel: the cancel won the race (nothing parked)"
    );
    assert!(a.m.store.get_parked_turn(&id).unwrap().is_none(), "sentinel: no parked row");
    assert!(
        a.until(10, |m| {
            let snap = m.budgets.snapshot("l4b-race", &m.account_pool()).unwrap().unwrap().to_json();
            snap["cash"]["consumed_usd"].as_f64().unwrap() < LIMIT
        }),
        "the meter reconciles under the limit, so only the latch could hold the thread"
    );

    a.send(&sibling, "hello from the sibling");
    a.send(&id, "after the cancel that beat the kill");
    assert!(
        a.until(15, |m| started(m, &id) == 2),
        "the cancel released the killed thread's own latch — its next message runs \
         (turns started: {})",
        started(&a.m, &id)
    );
    // r37 F-2 (doctrine 10, the population): the release is the killed
    // thread's OWN latch. A sibling on the same key, sent after the cancel
    // with the meter under the limit, stays held across two sweeps (2 x 1 s,
    // past the 25 ms tail poll; the 60 s tick is disabled). Red if this arm
    // released every latch.
    a.sweep();
    a.sweep();
    assert_eq!(
        started(&a.m, &sibling),
        0,
        "the cancel that beat the kill must not release a sibling on the same key"
    );
    let ps = a.m.park_state(&sibling).unwrap();
    assert!(
        ps["reason"].as_str().is_some_and(|r| r.contains("l4b-race") && r.contains(&id)),
        "the sibling is held by THIS kill's latch: {ps}"
    );
    a.kill();
}

/// **R9.L4b's opposing guard: a genuine budget kill (no cancel) still holds
/// the killed thread's own latch** — a later message on that thread does not
/// run and the parked prompt is not redelivered, with the meter under the
/// limit. Red if the latch were released whenever a killed turn ends. Then the
/// limit is raised and it runs, so the hold is the latch.
#[test]
fn a_budget_kill_with_no_cancel_still_holds_the_killed_threads_own_latch() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    a.set_key("l4b-hold", LIMIT);
    let id = a.thread_on(tmp.path(), "l4b-hold");
    a.kill_turn(&id, "l4b-hold");
    a.send(&id, "a later message with no cancel");
    a.sweep();
    a.sweep();
    assert_eq!(started(&a.m, &id), 1, "a budget kill with no cancel holds the killed thread");
    assert!(a.m.store.get_parked_turn(&id).unwrap().is_some(), "its parked row is still there");

    a.set_key("l4b-hold", 500.0);
    a.sweep();
    assert!(
        a.until(15, |m| started(m, &id) >= 2),
        "raising the limit releases it (turns started: {})",
        started(&a.m, &id)
    );
    a.kill();
}
