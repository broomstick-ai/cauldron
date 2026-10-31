//! **R9.L5 (GAP-AUDIT L-5): A MANAGED CHILD WITH A FOLLOW-UP TURN STARTING IS
//! NEVER REPORTED FINISHED WITH ITS OLD RESULT.**
//!
//! `managed::idle` decides "this child's run has ended" for `turn_finished`
//! (the report), `ended` (`agents/result`, `agents/list`) and boot `reconcile`.
//! It used to read four store rows — `running_turns`, the waiting queue,
//! `retries`, `parked_turns` — and not the execution LOCK. A dispatch takes
//! the lock and marks its queue rows delivered BEFORE the `running_turns` row
//! is written (the prologue: plugin snapshot, spawn guard, the projected
//! render), so in that gap all four rows read empty while the next turn is
//! starting. `finish_turn` calls `turn_finished` right after `maybe_dispatch`
//! has done exactly that for a queued follow-up — so a child with a follow-up
//! was reported to its parent with its PREVIOUS turn's result.
//!
//! The gap is HELD, not raced: the follow-up turn is projected, and its render
//! needs the `ledgers/` write lock, which the test holds
//! (`ThreadManager::ledger_write_lock_for_test`). The first turn is
//! native-owned, so its end needs no ledger write lock and runs through.
//!
//! No real claude: `THREADS_CLAUDE_BIN` is `tests/fixtures/fake-claude`. A
//! native turn says "Hello from fake claude."; a projected turn whose prompt
//! says `projected` takes the fixture's projected branch and says
//! "projected reply N" — so the old result and the new one are different words.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shapewire::envelope::codes;
use shapewire::token::Claims;
use threads::agents;
use threads::config::Config;
use threads::managed::Halt;
use threads::settings::{Projection, SettingsPatch};
use threads::{build_manager, ThreadEvent, ThreadManager};

/// The env is process-wide and these tests each point it at their own tmp dir.
static SERIAL: Mutex<()> = Mutex::new(());

const OLD_WORDS: &str = "Hello from fake claude.";
const NEW_WORDS: &str = "projected reply";

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

/// One daemon life. FOUR workers, not two: with the defect present the
/// epilogue's report blocks on the held ledger lock in one worker and the
/// follow-up's render blocks in another, and the daemon must still have
/// workers left to serve the test's calls.
struct Life {
    rt: tokio::runtime::Runtime,
    m: ThreadManager,
}

impl Life {
    fn boot(dir: &Path) -> Life {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
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

    fn kill(self) {
        let Life { rt, m } = self;
        rt.shutdown_background();
        drop(m);
    }

    fn thread(&self, dir: &Path) -> String {
        let settings = json!({
            "cwd": dir.to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
        });
        let _ctx = self.rt.enter();
        self.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    }

    fn call(&self, caller: &str, tool: &str, args: Value) -> Result<Value, shapewire::envelope::Refusal> {
        let claims = Claims {
            thread_id: caller.to_string(),
            run_id: format!("{caller}/run-1"),
            root: caller.to_string(),
            depth: 0,
            turn: "cookie".into(),
        };
        self.rt.block_on(agents::call(&self.m, &claims, tool, args))
    }

    fn spawn(&self, caller: &str, task: &str, rid: &str) -> String {
        let ok = self
            .call(caller, "agents_spawn", json!({"task": task, "request_id": rid}))
            .unwrap_or_else(|e| panic!("spawn from {caller}: {} {}", e.code, e.message));
        ok["thread_id"].as_str().unwrap().to_string()
    }

    /// Wait until `f` holds, or `secs` pass. A plain thread sleep, not the
    /// runtime's timer: the test thread must not depend on a worker being free.
    fn until(&self, secs: u64, mut f: impl FnMut(&ThreadManager) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if f(&self.m) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        f(&self.m)
    }
}

fn events(m: &ThreadManager, id: &str) -> Vec<ThreadEvent> {
    m.store.tail_events(id, 1000).unwrap().into_iter().map(|e| e.event).collect()
}

fn count_kind(m: &ThreadManager, id: &str, kind: &str) -> usize {
    m.store
        .list_events(id, 0, 100_000, None)
        .unwrap()
        .iter()
        .filter(|e| e.event.kind() == kind)
        .count()
}

/// The parent's wake messages (the reports its children delivered).
fn wakes(m: &ThreadManager, parent: &str) -> Vec<String> {
    events(m, parent)
        .into_iter()
        .filter_map(|e| match e {
            ThreadEvent::UserMessage { text, metadata, .. } if metadata["origin"] == "agents" => Some(text),
            _ => None,
        })
        .collect()
}

/// The child's completion records (`run_ended` facts on its own ledger).
fn records(dir: &Path, child: &str) -> Vec<Value> {
    let p = dir.join("ledgers").join(format!("{child}.jsonl"));
    let Ok(l) = threads::ledger::Ledger::read_jsonl(&p) else { return Vec::new() };
    l.events()
        .iter()
        .filter(|e| e.kind == threads::ledger::Kind::System && e.body["fact"] == "run_ended")
        .map(|e| e.body.clone())
        .collect()
}

/// The predicate `managed::idle` used before R9.L5: four store rows, no lock.
fn four_row_idle(m: &ThreadManager, id: &str) -> bool {
    m.store.get_running_turn(id).unwrap().is_none()
        && m.store.waiting_queue(id).unwrap().is_empty()
        && m.store.get_retry(id).unwrap().is_none()
        && m.store.get_parked_turn(id).unwrap().is_none()
}

/// **A CHILD WHOSE FOLLOW-UP TURN IS STARTING IS NOT REPORTED; WHEN THAT TURN
/// ENDS IT IS REPORTED ONCE, WITH THE NEW TURN'S RESULT.**
///
/// The child's first turn (native, `holdturn`) is held open while a follow-up
/// is queued and the child is switched to projected. The test then takes the
/// `ledgers/` write lock and releases the first turn. Its epilogue dispatches
/// the follow-up — lock taken, queue row delivered — and the follow-up stops
/// in its render, before any `running_turns` row: THE GAP, held open.
///
/// Sentinels (doctrine 9), inside the held gap:
/// * the first turn has ended (`turn_ended` × 1), the thread lock row exists,
///   and the follow-up has been dequeued;
/// * the pre-fix four-row predicate reads IDLE — so the gap is reached and an
///   answer of "not ended" is the lock's doing, not some other row's.
///
/// Guards:
/// * in the gap, `agents/result` answers `busy`/`not_ended` — not the first
///   turn's words (with the defect: `Ok`, "Hello from fake claude.");
/// * released, the parent is woken exactly once, and the wake carries the
///   FOLLOW-UP's words ("projected reply"), never the first turn's. The
///   child's ledger holds ONE completion record, the new one. (With the
///   defect: the epilogue's `complete` passed `idle`, blocked on the held
///   ledger lock in `write_record`, and on release wrote and reported the
///   OLD result; the follow-up's own end then found the parent no longer
///   owed anything.)
///
/// Periods outlasted while held: the 25 ms tail poll, ×20 (500 ms) — the
/// report decision is synchronous in `finish_turn`, right after the dispatch.
///
/// **THE MUTATION.** Restore `managed::idle` to the four-row read.
#[test]
fn a_child_whose_follow_up_is_starting_is_not_reported_and_then_reports_the_new_result_once() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let release = tmp.path().join("release-first-turn");
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let child = a.spawn(&parent, &format!("holdturn release={}", release.display()), "l5");
    assert!(
        a.until(20, |m| m.store.get_running_turn(&child).unwrap().is_some()),
        "the child's first turn is running"
    );
    {
        let _ctx = a.rt.enter();
        a.m.update_settings(&child, SettingsPatch { projection: Some(Projection::Projected), ..Default::default() })
            .unwrap();
        a.m.send(&child, "projected follow-up".into(), threads::SendIntent::Immediate, json!({}), false)
            .unwrap();
    }
    assert!(
        a.until(5, |m| !m.store.waiting_queue(&child).unwrap().is_empty()),
        "the follow-up waits behind the running turn"
    );

    let lock = a.m.ledger_write_lock_for_test();
    let guard = lock.lock().unwrap();
    std::fs::write(&release, b"go").unwrap();
    assert!(
        a.until(20, |m| count_kind(m, &child, "turn_ended") == 1
            && m.store.get_thread_lock(&child).unwrap().is_some()
            && m.store.waiting_queue(&child).unwrap().is_empty()
            && m.store.get_running_turn(&child).unwrap().is_none()),
        "sentinel: the first turn ended and the follow-up took the lock and dequeued, with no running row: {:?}",
        events(&a.m, &child)
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        four_row_idle(&a.m, &child) && a.m.store.get_thread_lock(&child).unwrap().is_some(),
        "sentinel: still inside the held gap, and the pre-fix four-row predicate reads idle"
    );
    assert!(!a.m.is_idle(&child).unwrap(), "sentinel: `ThreadManager::is_idle` names the lock");
    let r = a.call(&parent, "agents_result", json!({"agent": child}));
    match &r {
        Err(e) => {
            assert_eq!(e.code, codes::BUSY, "{}", e.message);
            assert!(e.message.contains("not_ended"), "{}", e.message);
        }
        Ok(v) => panic!("a child whose follow-up turn is starting is not ended, but agents/result answered {v}"),
    }
    assert!(wakes(&a.m, &parent).is_empty(), "not reported while the follow-up is starting");

    drop(guard);
    assert!(
        a.until(30, |m| count_kind(m, &child, "turn_ended") == 2 && !wakes(m, &parent).is_empty()),
        "released, the follow-up runs and ends, and the parent is woken. child: {:?}",
        events(&a.m, &child)
    );
    std::thread::sleep(Duration::from_millis(500));
    let w = wakes(&a.m, &parent);
    assert_eq!(w.len(), 1, "reported exactly once: {w:?}");
    assert!(w[0].contains(&child), "{}", w[0]);
    assert!(
        w[0].contains(NEW_WORDS) && !w[0].contains(OLD_WORDS),
        "the report carries the follow-up's result, not the first turn's: {}\nchild: {:?}",
        w[0],
        events(&a.m, &child)
    );
    let recs = records(tmp.path(), &child);
    assert_eq!(recs.len(), 1, "one completion record: {recs:?}");
    assert!(recs[0]["text"].as_str().unwrap().contains(NEW_WORDS), "{recs:?}");
    let ok = a.call(&parent, "agents_result", json!({"agent": child})).unwrap();
    assert!(ok["text"].as_str().unwrap().contains(NEW_WORDS), "{ok}");
    a.kill();
}

/// **PAIRED: A TRULY IDLE CHILD IS REPORTED AT ONCE.** No follow-up, nothing
/// held: the child's one turn ends and the parent is woken within 2 s of
/// that end — the lock read adds no wait (the epilogue releases the lock
/// before it reports). Opposes a `managed::idle` that is constantly `false`.
#[test]
fn a_truly_idle_child_is_reported_at_once() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let child = a.spawn(&parent, "plain child", "idle");
    assert!(a.until(20, |m| wakes(m, &parent).len() == 1), "the ended child wakes its parent");
    let end = a.m.store.newest_of_kind(&child, "turn_ended", false).unwrap().expect("turn_ended");
    let wake = a
        .m
        .store
        .tail_events(&parent, 1000)
        .unwrap()
        .into_iter()
        .find(|e| matches!(&e.event, ThreadEvent::UserMessage { metadata, .. } if metadata["origin"] == "agents"))
        .unwrap();
    let ms = (chrono::DateTime::parse_from_rfc3339(&wake.ts).unwrap()
        - chrono::DateTime::parse_from_rfc3339(&end.ts).unwrap())
    .num_milliseconds();
    assert!(ms < 2_000, "reported within 2 s of the end: {ms} ms");
    assert!(wakes(&a.m, &parent)[0].contains(OLD_WORDS), "{:?}", wakes(&a.m, &parent));
    a.kill();
}

/// **PAIRED: BOOT `reconcile` STILL REPORTS A CHILD WHOSE LOCK DIED WITH THE
/// OLD PROCESS.** The child's run ends; its completion is halted
/// (`Halt::BeforeCompletion`, a death between `turn_ended` and the report);
/// the daemon dies; and a `thread_locks` row is planted for the child naming
/// a holder that is DEAD — a pid that has exited, under another boot id. The
/// next life must still report the child.
///
/// What the DEAD arm does NOT prove (R9.L5 inspect, R9.L6d): boot `recover`
/// releases every dead-holder lock BEFORE `managed::recover` runs, so by
/// reconcile time the planted row is gone and `managed::idle` never reads it —
/// an `idle` that treated ANY lock row as busy passes this arm too. That is
/// not a test gap but the product: at boot, `idle`'s dead-holder branch is
/// unreachable. The branch IS reachable at runtime (another daemon on the same
/// store dies holding a child's lock), and
/// `at_runtime_a_dead_daemons_lock_on_an_ended_child_is_not_an_execution_and_a_live_holders_is`
/// below reaches it.
///
/// Opposition: the same planted row naming a LIVE holder (a `sleep` this test
/// spawned and keeps alive) is NOT reported — so reconcile does consult the
/// lock, and the first half is not passing because the row is ignored.
#[test]
fn boot_reconcile_reports_a_child_whose_lock_died_and_not_one_whose_holder_lives() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // A pid that has exited, and one that stays alive for the test.
    let mut dead = std::process::Command::new("/usr/bin/true").spawn().unwrap();
    let dead_pid = dead.id() as i64;
    dead.wait().unwrap();
    let mut live = std::process::Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let live_pid = live.id() as i64;

    for (holder_pid, holder, reported) in [(dead_pid, "dead", true), (live_pid, "live", false)] {
        let tmp = tempfile::tempdir().unwrap();
        set_env(tmp.path());
        let a = Life::boot(tmp.path());
        let parent = a.thread(tmp.path());
        a.m.managed().halt_after(Some(Halt::BeforeCompletion));
        let child = a.spawn(&parent, "plain child", holder);
        assert!(
            a.until(20, |m| m.store.last_seq_of_kind(&child, "turn_ended").unwrap().is_some()
                && m.store.get_running_turn(&child).unwrap().is_none()
                && m.store.get_thread_lock(&child).unwrap().is_none()),
            "{holder}: the child's turn ends"
        );
        std::thread::sleep(Duration::from_millis(300));
        assert!(wakes(&a.m, &parent).is_empty(), "{holder}: the halted completion woke nobody");
        a.kill();

        // The planted row: another daemon's lock on the child.
        let store = threads::Store::open(&tmp.path().join("threads.db")).unwrap();
        store.try_claim_thread(&child, "boot-of-a-previous-daemon", holder_pid, "dispatch").unwrap();
        assert_eq!(store.get_thread_lock(&child).unwrap().unwrap().daemon_pid, holder_pid);
        drop(store);

        let b = Life::boot(tmp.path());
        if reported {
            assert!(
                b.until(5, |m| wakes(m, &parent).len() == 1),
                "{holder}: boot reconciliation reports the child whose lock died with its holder"
            );
            assert!(wakes(&b.m, &parent)[0].contains(OLD_WORDS), "{:?}", wakes(&b.m, &parent));
        } else {
            std::thread::sleep(Duration::from_millis(1_000));
            assert!(
                wakes(&b.m, &parent).is_empty(),
                "{holder}: a child whose lock a LIVE holder still has is not reported: {:?}",
                wakes(&b.m, &parent)
            );
        }
        b.kill();
    }
    let _ = live.kill();
    let _ = live.wait();
}

/// **R9.L6d (L5 inspect): `managed::idle` JUDGES A FOREIGN LOCK'S LIVENESS,
/// reached where the product can reach it — at RUNTIME, not boot.** The
/// child's run ends and is reported; then, while this daemon keeps running
/// (no boot sweep in between), a `thread_locks` row is planted on the child
/// under another daemon's boot id. `agents_result` asks `managed::ended` ->
/// `idle` -> `is_idle` -> `lock_is_live`.
///   - DEAD holder (an exited pid): not an execution — the result is answered
///     (completed, the run's words), and the row is STILL THERE afterwards, so
///     the answer came from judging it, not from its absence.
///   - LIVE holder (a `sleep` this test owns): an execution — `not_ended`.
/// An `idle` ignoring the lock fails the live arm; one treating any lock row
/// as busy fails the dead arm.
#[test]
fn at_runtime_a_dead_daemons_lock_on_an_ended_child_is_not_an_execution_and_a_live_holders_is() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut dead = std::process::Command::new("/usr/bin/true").spawn().unwrap();
    let dead_pid = dead.id() as i64;
    dead.wait().unwrap();
    let mut live = std::process::Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let live_pid = live.id() as i64;

    for (holder_pid, holder, ended) in [(dead_pid, "dead", true), (live_pid, "live", false)] {
        let tmp = tempfile::tempdir().unwrap();
        set_env(tmp.path());
        let a = Life::boot(tmp.path());
        let parent = a.thread(tmp.path());
        let child = a.spawn(&parent, "plain child", holder);
        assert!(a.until(20, |m| wakes(m, &parent).len() == 1), "{holder}: the child ended and was reported");
        assert!(a.m.store.get_thread_lock(&child).unwrap().is_none(), "{holder}: sentinel: its own lock released");
        let ok = a.call(&parent, "agents_result", json!({"agent": child}));
        assert!(ok.is_ok(), "{holder}: sentinel: answered before the plant");

        // Another daemon on the same store takes the child's lock — this
        // daemon is NOT restarted, so nothing sweeps the row.
        let store = threads::Store::open(&tmp.path().join("threads.db")).unwrap();
        store.try_claim_thread(&child, "boot-of-another-daemon", holder_pid, "dispatch").unwrap();
        drop(store);
        assert_eq!(
            a.m.store.get_thread_lock(&child).unwrap().map(|r| r.daemon_pid),
            Some(holder_pid),
            "{holder}: sentinel: the running daemon sees the planted row"
        );

        let r = a.call(&parent, "agents_result", json!({"agent": child}));
        if ended {
            let v = r.unwrap_or_else(|e| panic!("{holder}: a dead holder's lock is not an execution: {} {}", e.code, e.message));
            assert_eq!(v["outcome"], "completed", "{holder}: {v}");
            assert!(v["text"].as_str().unwrap().contains(OLD_WORDS), "{holder}: {v}");
            assert!(
                a.m.store.get_thread_lock(&child).unwrap().is_some(),
                "{holder}: the row was still there when `idle` judged it"
            );
        } else {
            let e = r.expect_err("a live holder's lock is an execution");
            assert!(e.message.contains("not_ended"), "{holder}: {} {}", e.code, e.message);
        }
        a.kill();
    }
    let _ = live.kill();
    let _ = live.wait();
}

/// **R9.L6e (R9.L6d inspect F2): AT BOOT, `managed::idle`'s DEAD-HOLDER ARM IS
/// REACHED ON THE PATH WHERE IT IS REACHABLE — THE SWEEP'S RELEASE FAILS.**
/// Boot `recover` releases every dead-holder lock before `managed::recover`,
/// so on the healthy path reconcile never reads one (see
/// `boot_reconcile_reports_a_child_whose_lock_died_and_not_one_whose_holder_lives`).
/// But `release_thread_lock` -> `Err` only warns (runtime.rs boot sweep) and
/// leaves the row in place. This test makes that release fail: a SQLite
/// trigger, planted in the store before the second boot, aborts the DELETE of
/// exactly the planted row (no other lock row is affected).
///   - DEAD holder (an exited pid): the boot reports the child with its run's
///     words, and the planted row is STILL THERE afterwards — so the sweep did
///     not clear it and the report came from `idle` judging it dead, not from
///     its absence.
///   - LIVE holder (a `sleep` this test owns): not reported, row still there.
/// Sentinels (doctrine 9): before the boot, a direct `release_thread_lock` of
/// the planted row is `Err` and the row survives it — the fault really fires.
/// An `idle` treating any lock row as busy fails the dead arm; one ignoring the
/// lock fails the live arm.
#[test]
fn boot_reconcile_judges_a_dead_holders_lock_the_sweep_failed_to_release_and_not_a_live_ones() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut dead = std::process::Command::new("/usr/bin/true").spawn().unwrap();
    let dead_pid = dead.id() as i64;
    dead.wait().unwrap();
    let mut live = std::process::Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let live_pid = live.id() as i64;
    const HOLDER: &str = "boot-of-a-previous-daemon-l6e";

    for (holder_pid, holder, reported) in [(dead_pid, "dead", true), (live_pid, "live", false)] {
        let tmp = tempfile::tempdir().unwrap();
        set_env(tmp.path());
        let a = Life::boot(tmp.path());
        let parent = a.thread(tmp.path());
        a.m.managed().halt_after(Some(Halt::BeforeCompletion));
        let child = a.spawn(&parent, "plain child", holder);
        assert!(
            a.until(20, |m| m.store.last_seq_of_kind(&child, "turn_ended").unwrap().is_some()
                && m.store.get_running_turn(&child).unwrap().is_none()
                && m.store.get_thread_lock(&child).unwrap().is_none()),
            "{holder}: the child's turn ends"
        );
        std::thread::sleep(Duration::from_millis(300));
        assert!(wakes(&a.m, &parent).is_empty(), "{holder}: the halted completion woke nobody");
        a.kill();

        // The planted row, and the fault: its release is refused by the store.
        let db = tmp.path().join("threads.db");
        let store = threads::Store::open(&db).unwrap();
        store.try_claim_thread(&child, HOLDER, holder_pid, "dispatch").unwrap();
        drop(store);
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(&format!(
            "CREATE TRIGGER l6e_refuse_release BEFORE DELETE ON thread_locks
             WHEN OLD.boot_id = '{HOLDER}'
             BEGIN SELECT RAISE(ABORT, 'l6e: release refused'); END;"
        ))
        .unwrap();
        drop(conn);
        let store = threads::Store::open(&db).unwrap();
        assert!(
            store.release_thread_lock(&child, HOLDER).is_err(),
            "{holder}: sentinel: the planted row's release fails"
        );
        assert_eq!(
            store.get_thread_lock(&child).unwrap().map(|r| r.daemon_pid),
            Some(holder_pid),
            "{holder}: sentinel: the row survived the failed release"
        );
        drop(store);

        let b = Life::boot(tmp.path());
        if reported {
            assert!(
                b.until(5, |m| wakes(m, &parent).len() == 1),
                "{holder}: boot reports the child whose dead holder's lock the sweep could not release"
            );
            assert!(wakes(&b.m, &parent)[0].contains(OLD_WORDS), "{:?}", wakes(&b.m, &parent));
        } else {
            std::thread::sleep(Duration::from_millis(1_000));
            assert!(
                wakes(&b.m, &parent).is_empty(),
                "{holder}: a child whose lock a LIVE holder has is not reported: {:?}",
                wakes(&b.m, &parent)
            );
        }
        assert_eq!(
            b.m.store.get_thread_lock(&child).unwrap().map(|r| r.daemon_pid),
            Some(holder_pid),
            "{holder}: the row was still there through boot, so `idle` judged it"
        );
        b.kill();
    }
    let _ = live.kill();
    let _ = live.wait();
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
