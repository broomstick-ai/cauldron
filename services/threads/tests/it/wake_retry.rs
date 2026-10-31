//! **A CHILD'S COMPLETION OR WAKE THAT FAILED TO LAND IS RETRIED WITHOUT A
//! DAEMON RESTART** (R9.N3, GAP-AUDIT N-3).
//!
//! Before: `turn_finished`, `report` and the network `post` inside
//! `deliver_due_wakes` logged a failure as "boot reconciliation retries it",
//! and the only retry WAS boot — the parent waited for the next restart. Now
//! the failed child / parent is remembered in memory and a bounded backoff
//! timer (`managed::RETRY_BASE`, doubling, capped at `RETRY_CAP`) re-runs the
//! same per-child `reconcile` and per-parent `deliver_due_wakes` boot runs.
//!
//! Every test here runs in ONE daemon life: nothing is restarted, so a wake
//! that lands was delivered by the live retry. Failures are injected with
//! `managed::Halt` (single-shot): `BeforeCompletion` (the completion fails
//! before its record), `BeforeWakePost` (the network post fails), and
//! `AfterWakePost` (the post landed, the `wake_queued` marker did not).
//!
//! No real claude: `THREADS_CLAUDE_BIN` is `tests/fixtures/fake-claude`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shapewire::token::Claims;
use threads::agents;
use threads::config::Config;
use threads::managed::Halt;
use threads::{build_manager, ThreadEvent, ThreadManager};

static SERIAL: Mutex<()> = Mutex::new(());

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

struct Life {
    rt: tokio::runtime::Runtime,
    m: ThreadManager,
}

impl Life {
    fn boot(dir: &Path) -> Life {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
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
        let settings = json!({"cwd": dir.to_string_lossy(), "model": "sonnet", "account": {"named": "aa-owned"}});
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

    fn until(&self, secs: f64, mut f: impl FnMut(&ThreadManager) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs_f64(secs);
        while Instant::now() < deadline {
            if f(&self.m) {
                return true;
            }
            self.rt.block_on(async { tokio::time::sleep(Duration::from_millis(25)).await });
        }
        f(&self.m)
    }

    fn sleep(&self, d: Duration) {
        self.rt.block_on(async { tokio::time::sleep(d).await });
    }
}

fn spawn_ok(l: &Life, caller: &str, task: &str, rid: &str) -> String {
    let ok = l
        .call(caller, "agents_spawn", json!({"task": task, "request_id": rid}))
        .unwrap_or_else(|e| panic!("spawn from {caller}: {} {}", e.code, e.message));
    ok["thread_id"].as_str().unwrap().to_string()
}

/// The parent's wake messages, read out of the STORE (never the ledger the
/// retry writes its marker to).
fn wake_events(m: &ThreadManager, parent: &str) -> Vec<threads::StoredEvent> {
    m.store
        .tail_events(parent, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| matches!(&e.event, ThreadEvent::UserMessage { metadata, .. } if metadata["origin"] == "agents"))
        .collect()
}

fn child_ended(m: &ThreadManager, child: &str) -> bool {
    m.store.last_seq_of_kind(child, "turn_ended").unwrap().is_some() && m.store.get_running_turn(child).unwrap().is_none()
}

fn ledger(dir: &Path, id: &str) -> threads::ledger::Ledger {
    threads::ledger::Ledger::read_jsonl(&dir.join("ledgers").join(format!("{id}.jsonl"))).unwrap()
}

fn facts(dir: &Path, id: &str, fact: &str) -> usize {
    ledger(dir, id)
        .events()
        .iter()
        .filter(|e| e.kind == threads::ledger::Kind::System && e.body["fact"] == fact)
        .count()
}

fn finished_on_parent(dir: &Path, parent: &str, child: &str) -> bool {
    ledger(dir, parent)
        .events()
        .iter()
        .any(|e| e.kind == threads::ledger::Kind::Child && e.body["phase"] == "finished" && e.body["child_id"] == child)
}

/// The bound the live retry is held to with its PRODUCTION backoff: the first
/// retry runs `RETRY_BASE` (500 ms) after the failure; 5 s leaves room for a
/// loaded machine and is still nothing like "until the next restart".
const BOUND_S: f64 = 5.0;

/// **GUARD (a), COMPLETION: THE CHILD'S COMPLETION FAILS ONCE → THE PARENT IS
/// WOKEN WITHIN THE BOUND, NO RESTART.** `Halt::BeforeCompletion` fails
/// `complete` before the record is written — what used to wait for boot.
/// Production backoff (not shrunk), so the bound measured is the real one.
///
/// **THE MUTATION.** Make `managed::remember` return at its top (no retry
/// is ever scheduled) → no wake within the bound.
#[test]
fn a_failed_completion_wakes_the_parent_within_the_bound_without_a_restart() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    assert_eq!(threads::managed::RETRY_BASE, Duration::from_millis(500), "the bound below assumes the production base");
    let parent = a.thread(tmp.path());
    a.m.managed().halt_after(Some(Halt::BeforeCompletion));
    let child = spawn_ok(&a, &parent, "plain child", "a1");
    assert!(a.until(60.0, |m| child_ended(m, &child)), "the child's turn ends");
    let ended = Instant::now();
    assert!(
        a.until(BOUND_S, |m| wake_events(m, &parent).len() == 1),
        "the live retry wakes the parent within {BOUND_S} s of the failed completion (waited {:?}, passes {}, owed {})",
        ended.elapsed(),
        a.m.managed().retry_passes(),
        a.m.managed().retries_owed()
    );
    assert!(a.m.managed().retry_passes() >= 1, "the wake came from a retry pass");
    assert_eq!(facts(tmp.path(), &child, "run_ended"), 1, "the retry wrote the child's completion record once");
    assert!(a.until(BOUND_S, |m| m.managed().retries_owed() == 0), "nothing is left owed");
    a.sleep(Duration::from_millis(1200));
    assert_eq!(wake_events(&a.m, &parent).len(), 1, "exactly one wake");
    a.kill();
}

/// **GUARD (a), POST: THE NETWORK POST OF THE WAKE FAILS ONCE → THE PARENT IS
/// WOKEN WITHIN THE BOUND, NO RESTART.** The report reached the parent's
/// ledger (`child{finished}`), so `reconcile` would no-op; only the
/// remembered PARENT re-runs `deliver_due_wakes`.
///
/// **THE MUTATION.** As above (`remember` returns at its top) → no wake.
#[test]
fn a_failed_wake_post_wakes_the_parent_within_the_bound_without_a_restart() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    a.m.managed().halt_after(Some(Halt::BeforeWakePost));
    let child = spawn_ok(&a, &parent, "plain child", "a2");
    assert!(a.until(60.0, |m| child_ended(m, &child)), "the child's turn ends");
    assert!(a.until(BOUND_S, |_| finished_on_parent(tmp.path(), &parent, &child)), "the report itself landed");
    let reported = Instant::now();
    assert!(
        a.until(BOUND_S, |m| wake_events(m, &parent).len() == 1),
        "the live retry posts the failed wake within {BOUND_S} s (waited {:?}, passes {})",
        reported.elapsed(),
        a.m.managed().retry_passes()
    );
    assert!(a.m.managed().retry_passes() >= 1, "the wake came from a retry pass");
    assert!(a.until(BOUND_S, |_| facts(tmp.path(), &parent, "wake_queued") == 1), "the wake is marked queued once");
    a.kill();
}

/// **GUARD (b): A HALF-LANDED WAKE — MESSAGE STORED, MARKER NOT — IS EXACTLY
/// ONE WAKE.** `Halt::AfterWakePost` lets the post land in the parent's store
/// and fails before `wake_queued`. The retry must find the message by
/// `wake_id` and only mark it. Backoff shrunk to 50 ms / cap 200 ms and the
/// test waits 2 s afterwards: past ten retry periods, so a duplicate the
/// retry would post has had every chance to land.
///
/// **THE MUTATION.** In `deliver_due_wakes_once`, treat `already_sent` as
/// `None` (always post) → two wakes.
#[test]
fn a_half_landed_wake_is_marked_not_resent_past_ten_retry_periods() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    a.m.managed().set_retry_backoff(Duration::from_millis(50), Duration::from_millis(200));
    let parent = a.thread(tmp.path());
    a.m.managed().halt_after(Some(Halt::AfterWakePost));
    let child = spawn_ok(&a, &parent, "plain child", "b1");
    assert!(a.until(60.0, |m| child_ended(m, &child)), "the child's turn ends");
    assert!(a.until(BOUND_S, |m| wake_events(m, &parent).len() == 1), "the first post landed");
    // R9.F2i: wait for the retry's OUTCOME, not its counters. `retry_owed`
    // bumps `passes` and TAKES the owed sets at the TOP of a pass, so
    // `passes >= 1 && owed == 0` is already true while that pass is still
    // inside `deliver_due_wakes` — the marker read straight after it raced the
    // pass and came back 0 (base 63f87994, `left: 0 right: 1`).
    assert!(
        a.until(BOUND_S, |_| facts(tmp.path(), &parent, "wake_queued") == 1),
        "the retry marked the half-landed wake (passes {}, owed {})",
        a.m.managed().retry_passes(),
        a.m.managed().retries_owed()
    );
    assert!(a.m.managed().retry_passes() >= 1, "the marker came from a retry pass");
    assert!(a.until(BOUND_S, |m| m.managed().retries_owed() == 0), "nothing is left owed");
    a.sleep(Duration::from_secs(2));
    assert_eq!(wake_events(&a.m, &parent).len(), 1, "exactly one wake after the retry");
    assert_eq!(facts(tmp.path(), &parent, "wake_queued"), 1, "and one marker");
    a.kill();
}

/// **GUARD (c): A `wake_suppressed` PARENT STAYS UNWOKEN AFTER A
/// FAILED-THEN-RETRIED REPORT — AND THE REPORT IS NOT LOST: IT RIDES
/// (R10.M2, L-8).** The parent is cancel-marked (R9.N1's
/// `begin_subtree_cancel`) while it is owed a `holdturn` child; the child is
/// released, its completion fails (`BeforeCompletion`), and the live retry
/// re-reports it. Positive sentinels that the retry DID run: the report
/// (`child{finished}`) reaches the parent's ledger and nothing is left owed.
/// Backoff shrunk to 50 ms / cap 200 ms and the test waits 2 s after the
/// report — past ten retry periods.
///
/// **THE MUTATION.** Skip the `suppressed.contains` check in
/// `deliver_due_wakes_once` → the retry wakes the parent (a turn starts).
/// Opposed (R10.M2): drop the withheld wake instead of riding it → no wake
/// row is on the parent's queue.
#[test]
fn a_suppressed_parent_stays_unwoken_after_a_retried_report_past_ten_retry_periods() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    a.m.managed().set_retry_backoff(Duration::from_millis(50), Duration::from_millis(200));
    let parent = a.thread(tmp.path());
    let release = tmp.path().join("release-c1");
    let child = spawn_ok(&a, &parent, &format!("holdturn release={}", release.display()), "c1");
    a.m.managed().halt_after(Some(Halt::BeforeCompletion));
    threads::managed::begin_subtree_cancel(&a.m, &parent);
    assert_eq!(facts(tmp.path(), &parent, "wake_suppressed"), 1, "the parent is cancel-marked while owed the child");
    let t0 = a.m.store.last_seq_of_kind(&parent, "turn_started").unwrap();
    std::fs::write(&release, b"go").unwrap();
    assert!(a.until(60.0, |m| child_ended(m, &child)), "the released child's turn ends");
    assert!(
        a.until(BOUND_S, |_| finished_on_parent(tmp.path(), &parent, &child)),
        "the retry re-reported the child (passes {})",
        a.m.managed().retry_passes()
    );
    assert!(a.m.managed().retry_passes() >= 1, "the report came from a retry pass");
    assert!(a.until(BOUND_S, |m| m.managed().retries_owed() == 0), "nothing is left owed");
    // R9.F2i: `owed == 0` is true from the TOP of the pass (see guard (b)),
    // so the 2 s quiet window starts only once the pass's outcome — the
    // withheld wake, marked queued — is on the record.
    assert!(
        a.until(BOUND_S, |_| facts(tmp.path(), &parent, "wake_queued") == 1),
        "the retried report queued its (withheld) wake"
    );
    a.sleep(Duration::from_secs(2));
    let wakes = wake_events(&a.m, &parent);
    assert_eq!(wakes.len(), 1, "the retried report reached the parent once, as a withheld wake");
    let mid = wakes[0].message_id.expect("the wake is a queued message");
    let q: Vec<(u64, String)> = a.m.store.waiting_queue(&parent).unwrap().into_iter().map(|r| (r.message_id, r.dispatch)).collect();
    assert_eq!(q, vec![(mid, "ride".to_string())], "it rides the parent's next turn instead of being dropped");
    assert_eq!(a.m.store.last_seq_of_kind(&parent, "turn_started").unwrap(), t0, "the suppressed parent was not woken by the retry");
    assert_eq!(facts(tmp.path(), &parent, "wake_queued"), 1, "one wake was queued, and only one");
    a.kill();
}

#[path = "support/first_exec.rs"]
mod first_exec;
