//! # R10.H1 — a finished subagent is recorded finished, whatever its wake mode,
//! # and a periodic sweep reconciles any whose finish was missed (TC-13)
//!
//! an internal report (substrate `interface-layer`, c0e216b),
//! findings F1 and F2. Each guard is forced red in an internal report
//! with its opposing arm:
//!
//! * `a_wake_never_child_is_recorded_finished_and_wakes_nobody_n2` — two
//!   `notify: hold` (`wake: never`) children end: each gets `child{finished}`
//!   on the parent's ledger (`children.runs[].finished` non-null), and the
//!   parent gets no wake and no report words. Forced: `report()`'s early
//!   return restored (finished stays null).
//! * `a_wake_end_child_is_recorded_finished_and_wakes_its_parent_n2` — the
//!   opposing arm: two default (`wake: end`) children end: finished AND the
//!   parent is woken naming them. A constant "never wake" fails it.
//! * `a_wake_never_childs_later_run_posts_no_reply_but_a_wake_end_childs_does`
//!   — `reply_owed` is false for `never` (forced: drop the explicit gate, the
//!   `never` child's re-task posts a reply), true for `end`.
//! * `the_sweep_finishes_a_missed_child_within_one_injected_period_*` — a
//!   completion LOST (not remembered by the R9.N3 retry: `Halt::LostCompletion`)
//!   for one `end` and one `never` child stays lost past 1.5 s with the sweep
//!   at its real 10-minute idle period; with the period injected at 500 ms the
//!   FIRST pass records both, wakes the parent for the `end` child only, and
//!   leaves a child that is still running untouched. Forced: the sweep pass a
//!   no-op (red); the sweep closing every unfinished child regardless of state
//!   (the running-child arm red).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shapewire::token::Claims;
use threads::config::Config;
use threads::event::ThreadEvent;
use threads::ledger::Kind;
use threads::managed::{Halt, SWEEP_IDLE, SWEEP_SETTLE};
use threads::shapes::threads as tshape;
use threads::{build_manager, ThreadManager};

fn env() -> &'static Path {
    static ENV: OnceLock<tempfile::TempDir> = OnceLock::new();
    ENV.get_or_init(|| {
        let tmp = tempfile::tempdir().unwrap();
        let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"));
        std::env::set_var("CAULDRON_HOME", tmp.path().join("cauldron"));
        std::env::set_var("THREADS_CLAUDE_BIN", fake);
        std::env::set_var("THREADS_TAIL_POLL_MS", "25");
        // The longest dispatch period: every wake below is the post's own
        // (Immediate) dispatch, and every "no wake" is asserted on the POST
        // (a `user_message` carrying a `wake_id`), which is written inside the
        // report call itself — not on a turn this tick might start.
        std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
        std::env::set_var("FAKE_STATE_DIR", tmp.path().join("fake-state"));
        std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
        std::fs::create_dir_all(tmp.path().join("fake-state")).unwrap();
        tmp
    })
    .path()
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

async fn open(dir: &Path) -> ThreadManager {
    env();
    threads::clock::testing::ticking();
    let m = build_manager(config_at(dir)).unwrap();
    m.recover().await;
    m
}

fn root(m: &ThreadManager, cwd: &Path) -> String {
    let settings = json!({"cwd": cwd.to_string_lossy(), "model": "sonnet", "account": {"named": "aa-owned"}});
    m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
}

fn claims(id: &str) -> Claims {
    Claims { thread_id: id.to_string(), run_id: format!("{id}/run-1"), root: id.to_string(), depth: 0, turn: "test".to_string() }
}

async fn call(m: &ThreadManager, caller: &str, name: &str, args: Value) -> Value {
    threads::agents::call(m, &claims(caller), name, args)
        .await
        .unwrap_or_else(|r| panic!("{name} from {caller} refused: {} {}", r.code, r.message))
}

async fn wait_for(what: &str, bound: Duration, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + bound;
    while !f() {
        assert!(Instant::now() < deadline, "timed out ({bound:?}) waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn turns(m: &ThreadManager, id: &str) -> usize {
    m.store
        .events_from(id, 0, 100_000)
        .unwrap()
        .iter()
        .filter(|e| matches!(e.event, ThreadEvent::TurnStarted { .. }))
        .count()
}

fn ended(m: &ThreadManager, id: &str) -> bool {
    m.store.tail_events(id, 50).unwrap().iter().any(|e| matches!(e.event, ThreadEvent::TurnEnded { .. }))
        && m.is_idle(id).unwrap()
}

/// Spawn without waiting; returns the child's id.
async fn spawn(m: &ThreadManager, parent: &str, task: &str, extra: Value) -> String {
    let mut args = json!({"task": task});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    call(m, parent, "agents_spawn", args).await["thread_id"].as_str().unwrap().to_string()
}

/// The parent's `threads.children` cell, from its ledger by the shape's own builder.
fn children(m: &ThreadManager, p: &str) -> Value {
    let l = m.managed().load(p).unwrap().expect("the parent has a ledger");
    serde_json::to_value(tshape::children_row(&l)).unwrap()
}

fn finished(m: &ThreadManager, p: &str, c: &str) -> Value {
    let row = children(m, p);
    row["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(c))
        .unwrap_or_else(|| panic!("no run for {c}: {row}"))["finished"]
        .clone()
}

/// Every wake POSTED on `p` (a `user_message` carrying a `wake_id`): its `children`.
fn wakes(m: &ThreadManager, p: &str) -> Vec<Value> {
    m.store
        .events_from(p, 0, 100_000)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            ThreadEvent::UserMessage { metadata, .. } if metadata["wake_id"].is_string() => Some(metadata["children"].clone()),
            _ => None,
        })
        .collect()
}

/// Whether `c`'s run-end REPLY (R10.W1, a later run) was posted on `p`.
fn reply_from(m: &ThreadManager, p: &str, c: &str) -> bool {
    m.store.events_from(p, 0, 100_000).unwrap().into_iter().any(|e| match e.event {
        ThreadEvent::UserMessage { metadata, .. } => {
            metadata["reply_id"].as_str().is_some_and(|r| r.starts_with(&format!("reply:{c}:")))
        }
        _ => false,
    })
}

/// `context` events on the parent's ledger — the report WORDS a parent's model reads.
fn context_events(m: &ThreadManager, p: &str) -> usize {
    m.managed().load(p).unwrap().unwrap().events().iter().filter(|e| e.kind == Kind::Context).count()
}

/// `run_ended` completion records on the child's own ledger.
fn records(m: &ThreadManager, c: &str) -> usize {
    m.managed()
        .load(c)
        .unwrap()
        .unwrap()
        .events()
        .iter()
        .filter(|e| e.kind == Kind::System && e.body["fact"] == "run_ended")
        .count()
}

const BOUND: Duration = Duration::from_secs(20);

/// **F2 — A `wake: never` CHILD'S END IS RECORDED, AND WAKES NOBODY. N = 2.**
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wake_never_child_is_recorded_finished_and_wakes_nobody_n2() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let c1 = spawn(&m, &p, "child task one: say done", json!({"notify": "hold"})).await;
    let c2 = spawn(&m, &p, "child task two: say done", json!({"notify": "hold"})).await;
    wait_for("both never-children ended", BOUND, || ended(&m, &c1) && ended(&m, &c2)).await;
    wait_for("both never-children recorded finished on the parent", BOUND, || {
        !finished(&m, &p, &c1).is_null() && !finished(&m, &p, &c2).is_null()
    })
    .await;
    for c in [&c1, &c2] {
        assert_eq!(finished(&m, &p, c)["outcome"], json!("ok"), "{}", children(&m, &p));
        let runs = children(&m, &p);
        let r = runs["runs"].as_array().unwrap().iter().find(|r| r["id"] == json!(c)).unwrap().clone();
        assert_eq!(r["wake"], json!("never"), "{r}");
    }
    assert_eq!(children(&m, &p)["outstanding"], json!([]));
    // The wake decision stays on `outstanding`: a wake would have been POSTED
    // inside the same report call that wrote the finish. 1.5 s past it.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(wakes(&m, &p), Vec::<Value>::new(), "a wake:never child woke its parent");
    assert_eq!(turns(&m, &p), 0, "the parent ran a turn for a wake:never child");
    assert_eq!(context_events(&m, &p), 0, "a wake:never child's words reached the parent's ledger");
    // Exactly one finish each — a second record pass is a no-op.
    let n_fin = m.managed().load(&p).unwrap().unwrap().events().iter().filter(|e| e.kind == Kind::Child && e.body["phase"] == "finished").count();
    assert_eq!(n_fin, 2);
}

/// **OPPOSING ARM — A `wake: end` CHILD IS RECORDED AND ITS PARENT WOKEN. N = 2.**
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wake_end_child_is_recorded_finished_and_wakes_its_parent_n2() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let c1 = spawn(&m, &p, "child task one: say done", json!({})).await;
    let c2 = spawn(&m, &p, "child task two: say done", json!({})).await;
    wait_for("both end-children recorded finished", BOUND, || {
        !finished(&m, &p, &c1).is_null() && !finished(&m, &p, &c2).is_null()
    })
    .await;
    wait_for("the parent is woken naming both children", BOUND, || {
        let named: Vec<Value> = wakes(&m, &p).into_iter().flat_map(|w| w.as_array().cloned().unwrap_or_default()).collect();
        named.contains(&json!(c1)) && named.contains(&json!(c2))
    })
    .await;
    assert_eq!(finished(&m, &p, &c1)["outcome"], json!("ok"));
    assert_eq!(finished(&m, &p, &c2)["outcome"], json!("ok"));
}

/// **`reply_owed` IS FALSE FOR `never` — EXPLICITLY — AND TRUE FOR `end`.**
/// Both children are recorded finished first (so the old gate, "no finish ⇒ no
/// reply", is gone for both); each is then re-tasked by mail. The `end` child's
/// later run posts its reply; the `never` child's, re-tasked FIRST, posts none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wake_never_childs_later_run_posts_no_reply_but_a_wake_end_childs_does() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let n = spawn(&m, &p, "child task: say done", json!({"notify": "hold"})).await;
    let e = spawn(&m, &p, "child task: say done", json!({})).await;
    wait_for("both recorded finished", BOUND, || !finished(&m, &p, &n).is_null() && !finished(&m, &p, &e).is_null()).await;
    wait_for("the parent's wake turn for e is over", BOUND, || turns(&m, &p) >= 1 && m.is_idle(&p).unwrap()).await;
    for c in [&n, &e] {
        let t0 = turns(&m, c);
        call(&m, &p, "agents_message", json!({"agent": c, "body": "second task: say again"})).await;
        wait_for(&format!("{c}'s mail-started run ends"), BOUND, || turns(&m, c) > t0 && m.is_idle(c).unwrap()).await;
    }
    wait_for("e's later run posts its reply on p", BOUND, || reply_from(&m, &p, &e)).await;
    assert_eq!(records(&m, &n), 2, "n's later run ended with its own record (positive sentinel)");
    assert!(!reply_from(&m, &p, &n), "a wake:never child's later run posted a reply to its parent");
}

/// **F1 — THE SWEEP FINISHES A MISSED CHILD WITHIN ONE INJECTED PERIOD, AND
/// LEAVES A RUNNING ONE ALONE.** The period outlasted by the negative is the
/// sweep's own: [`SWEEP_IDLE`] (600 s) before injection — so 1.5 s of "still
/// unfinished" proves only that nothing ELSE closes a lost completion (no
/// live retry was told) — then 500 ms injected, and the first pass decides.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sweep_finishes_a_missed_child_within_one_injected_period_and_leaves_a_running_one() {
    assert_eq!(SWEEP_IDLE, Duration::from_secs(600));
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p1 = root(&m, dir.path());
    let p2 = root(&m, dir.path());
    // Two lost completions on p1: one `end`, one `never`.
    m.managed().halt_after(Some(Halt::LostCompletion));
    let ce = spawn(&m, &p1, "child task: say done", json!({})).await;
    wait_for("ce ended and its completion lost", BOUND, || ended(&m, &ce) && m.managed().halt_armed().is_none()).await;
    m.managed().halt_after(Some(Halt::LostCompletion));
    let cn = spawn(&m, &p1, "child task: say done", json!({"notify": "hold"})).await;
    wait_for("cn ended and its completion lost", BOUND, || ended(&m, &cn) && m.managed().halt_armed().is_none()).await;
    // A child still running, on p2.
    let release = dir.path().join("release-h");
    let h = spawn(&m, &p2, &format!("holdturn release={}", release.display()), json!({})).await;
    wait_for("h is running", BOUND, || m.store.get_running_turn(&h).unwrap().is_some()).await;

    // The misses are real and nothing else closes them.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(finished(&m, &p1, &ce).is_null() && finished(&m, &p1, &cn).is_null(), "{}", children(&m, &p1));
    assert_eq!(records(&m, &ce), 0, "the lost completion wrote a record");
    assert_eq!(m.managed().retries_owed(), 0, "the live retry was told — not a lost completion");
    assert_eq!(m.managed().sweep_passes(), 0, "a sweep ran before the injected period");
    assert_eq!(wakes(&m, &p1), Vec::<Value>::new());

    // The live epilogue's window (SWEEP_SETTLE, 30 s) shrunk below the 1.5 s
    // these runs have been over; the period is the bound under test.
    m.managed().set_sweep_settle(Duration::from_millis(200));
    let period = Duration::from_millis(500);
    m.managed().set_sweep_period(period, period);
    let t0 = Instant::now();
    wait_for("the first sweep pass", period + Duration::from_secs(5), || m.managed().sweep_passes() >= 1).await;
    let took = t0.elapsed();
    // Right after the FIRST pass, while h still runs.
    assert!(m.store.get_running_turn(&h).unwrap().is_some(), "h stopped running before the assertion — the arm proves nothing");
    assert_eq!(finished(&m, &p1, &ce)["outcome"], json!("ok"), "the sweep did not finish the missed end-child: {}", children(&m, &p1));
    assert_eq!(finished(&m, &p1, &cn)["outcome"], json!("ok"), "the sweep did not finish the missed never-child: {}", children(&m, &p1));
    assert_eq!(wakes(&m, &p1), vec![json!([ce])], "the sweep must wake p1 for the end-child only");
    assert!(finished(&m, &p2, &h).is_null(), "the sweep finished a child that is still running: {}", children(&m, &p2));
    assert_eq!(records(&m, &h), 0, "the sweep wrote a record for a child that is still running");
    assert!(took >= period - Duration::from_millis(50), "the pass ran before one period ({took:?})");

    // h ends on its own; its finish is recorded (by the live path or the sweep).
    std::fs::write(&release, b"go").unwrap();
    wait_for("h recorded finished after it ends", BOUND, || !finished(&m, &p2, &h).is_null()).await;
}

/// Corrective turns (`kind: schema_retry`) queued on child `c`.
fn correctives(m: &ThreadManager, c: &str) -> usize {
    m.store
        .events_from(c, 0, 100_000)
        .unwrap()
        .into_iter()
        .filter(|e| matches!(&e.event, ThreadEvent::UserMessage { metadata, .. } if metadata["kind"] == "schema_retry"))
        .count()
}

/// Messages posted on `p` that carry `c`'s end — a wake naming it, or its reply.
fn messages_for(m: &ThreadManager, p: &str, c: &str) -> usize {
    m.store
        .events_from(p, 0, 100_000)
        .unwrap()
        .into_iter()
        .filter(|e| match &e.event {
            ThreadEvent::UserMessage { metadata, .. } => {
                metadata["children"].as_array().is_some_and(|a| a.contains(&json!(c)))
                    || metadata["reply_id"].as_str().is_some_and(|r| r.starts_with(&format!("reply:{c}:")))
            }
            _ => false,
        })
        .count()
}

fn finishes(m: &ThreadManager, p: &str, c: &str) -> usize {
    m.managed()
        .load(p)
        .unwrap()
        .unwrap()
        .events()
        .iter()
        .filter(|e| e.kind == Kind::Child && e.body["phase"] == "finished" && e.body["child_id"] == json!(c))
        .count()
}

async fn sweep_now(m: &ThreadManager) {
    let m2 = m.clone();
    tokio::task::spawn_blocking(move || threads::managed::sweep(&m2)).await.unwrap();
}

/// **INSPECT F-1 — THE SWEEP AND THE LIVE EPILOGUE NEVER BOTH CLOSE A RUN.
/// N = 2 schema children, 2 sweep passes inside the window, 2 after.**
///
/// Each child's first answer is invalid (`fake:fix-on-retry`), so its live
/// epilogue owes a corrective turn before the run is over. The epilogues are
/// held at their very start — the window between the turn lock's release (the
/// child reads idle, its store holds a terminal) and `turn_finished` — and two
/// sweep passes run there at the REAL settle period ([`SWEEP_SETTLE`]). Then
/// the epilogues go on. Per child: exactly one corrective, exactly one
/// `child{finished}`, exactly one message to the parent — and two more passes
/// with the settle shrunk to zero change nothing.
///
/// FORCED: the sweep's settle skip removed — the in-window pass reports the
/// unmet run from the store (a wake), the epilogue still sends the corrective,
/// and the corrective run's end is a SECOND message (a reply) to the parent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sweep_never_closes_a_run_inside_its_live_epilogue_one_corrective_one_message_n2() {
    assert_eq!(SWEEP_SETTLE, Duration::from_secs(30));
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let schema = json!({"type": "object", "required": ["verdict"],
        "properties": {"verdict": {"enum": ["merge", "block"]}}, "description": "fake:fix-on-retry"});
    m.managed().pause_epilogues(2);
    let s1 = spawn(&m, &p, "schema child one", json!({"json_schema": schema})).await;
    let s2 = spawn(&m, &p, "schema child two", json!({"json_schema": schema})).await;
    let deadline = Instant::now() + BOUND;
    while m.managed().epilogues_paused() < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        m.managed().epilogues_paused(),
        2,
        "both epilogues held in the window; s1 ended={} s2 ended={} turns={},{} children={}",
        ended(&m, &s1), ended(&m, &s2), turns(&m, &s1), turns(&m, &s2), children(&m, &p)
    );
    for c in [&s1, &s2] {
        assert!(m.is_idle(c).unwrap(), "{c} must read idle in the window, or the race is not set up");
    }
    sweep_now(&m).await;
    sweep_now(&m).await;
    for c in [&s1, &s2] {
        assert!(finished(&m, &p, c).is_null(), "a sweep closed {c} inside its live epilogue: {}", children(&m, &p));
        assert_eq!(messages_for(&m, &p, c), 0, "a sweep messaged the parent about {c} inside its live epilogue");
    }
    m.managed().release_epilogues();
    wait_for("both recorded finished", BOUND, || !finished(&m, &p, &s1).is_null() && !finished(&m, &p, &s2).is_null()).await;
    wait_for("the parent told about both", BOUND, || messages_for(&m, &p, &s1) >= 1 && messages_for(&m, &p, &s2) >= 1).await;
    m.managed().set_sweep_settle(Duration::ZERO);
    sweep_now(&m).await;
    sweep_now(&m).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    for c in [&s1, &s2] {
        assert_eq!(correctives(&m, c), 1, "{c}: one corrective turn");
        assert_eq!(finishes(&m, &p, c), 1, "{c}: one child{{finished}}");
        assert_eq!(messages_for(&m, &p, c), 1, "{c}: one message to the parent");
        assert_eq!(finished(&m, &p, c)["outcome"], json!("ok"));
    }
}

/// **INSPECT F-2 — A RE-TASKED CHILD RUNNING WITH AN EARLIER RECORD IS NOT
/// CLOSED. N = 2 (one `end`, one `never`).**
///
/// Each child's first run writes its `run_ended` record and loses the report
/// (`Halt::LostReport`), so it is unfinished WITH a record. The parent
/// re-tasks it with a held turn; while that turn runs, two sweep passes (settle
/// shrunk below the first run's age, so only `reconcile`'s idle read stands
/// between the record and a close) must leave both unfinished and the parent
/// unwoken. Opposing: once the held turns end, the live path finishes both and
/// wakes the parent for the `end` child.
///
/// FORCED: `reconcile`'s `if !idle(m, id)? { return }` removed — the pass
/// reports both from their old records while they run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_running_child_with_an_earlier_record_is_not_closed_by_the_sweep_n2() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let mut kids = Vec::new();
    for extra in [json!({}), json!({"notify": "hold"})] {
        m.managed().halt_after(Some(Halt::LostReport));
        let c = spawn(&m, &p, "child task: say done", extra).await;
        // The halt is consumed just AFTER the record is written: wait for
        // both, or the next arm's halt is consumed by this child.
        wait_for("its first run ended with a record and the report lost", BOUND, || {
            ended(&m, &c) && records(&m, &c) == 1 && m.managed().halt_armed().is_none()
        })
        .await;
        assert!(finished(&m, &p, &c).is_null(), "the report was not lost: {}", children(&m, &p));
        kids.push(c);
    }
    let release = dir.path().join("release-retask");
    for c in &kids {
        call(&m, &p, "agents_message", json!({"agent": c, "body": format!("holdturn release={}", release.display())})).await;
    }
    wait_for("both re-tasked runs are running", BOUND, || kids.iter().all(|c| m.store.get_running_turn(c).unwrap().is_some())).await;
    m.managed().set_sweep_settle(Duration::from_millis(100));
    tokio::time::sleep(Duration::from_millis(300)).await;
    sweep_now(&m).await;
    sweep_now(&m).await;
    for c in &kids {
        assert!(m.store.get_running_turn(c).unwrap().is_some(), "{c} stopped running before the assertion — the arm proves nothing");
        assert!(finished(&m, &p, c).is_null(), "the sweep closed {c} while it ran: {}", children(&m, &p));
    }
    assert_eq!(wakes(&m, &p), Vec::<Value>::new(), "the sweep woke the parent for a running child");
    std::fs::write(&release, b"go").unwrap();
    wait_for("both finished once their runs end", BOUND, || kids.iter().all(|c| !finished(&m, &p, c).is_null())).await;
    wait_for("the parent woken for the end child", BOUND, || wakes(&m, &p).iter().any(|w| w.as_array().is_some_and(|a| a.contains(&json!(kids[0]))))).await;
    for c in &kids {
        assert_eq!(finishes(&m, &p, c), 1);
    }
}

#[path = "support/first_exec.rs"]
mod first_exec;

// ═════════════════════════════════════════════════════════════════════════════
//  R10.H2 (TC-13; HEALTH F3) — THE PERIODIC HEALTH INVARIANT, I1–I5
// ═════════════════════════════════════════════════════════════════════════════
//
// Each invariant: the violation is injected, the warning appears on the
// warnings path within ONE injected period (and not before it: the loop starts
// at the real 5-minute `PERIOD`, and nothing has run 1.5 s later), naming
// exactly the threads in violation; the violation is fixed and the invariant
// is cleared; a healthy counterpart in the same tree is never named.

use threads::health_check::{self as hc, Health, Limits, Proc};

/// Every record the daemon's warnings path wrote, from the disk half of
/// `threads::warnings::COUNTER` — the counter that feeds the `warnings` shape.
/// The tracing layer is installed once for this binary.
fn warnings_sink() -> &'static Path {
    static SINK: OnceLock<tempfile::TempDir> = OnceLock::new();
    SINK.get_or_init(|| {
        use tracing_subscriber::layer::SubscriberExt;
        let t = tempfile::tempdir().unwrap();
        threads::warnings::COUNTER.attach_sink(t.path().join("warnings.jsonl"), 64 << 20);
        let sub = tracing_subscriber::registry().with(threads::warnings::WarningCounter);
        tracing::subscriber::set_global_default(sub).expect("no other global subscriber in this binary");
        t
    })
    .path()
}

/// The health WARN records for `inv` naming `id`.
fn health_warnings(inv: &str, id: &str) -> usize {
    let p = warnings_sink().join("warnings.jsonl");
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|r| r["target"] == "threads::health_check" && r["fields"]["invariant"] == json!(inv))
        .filter(|r| r["message"].as_str().is_some_and(|s| s.contains(id)))
        .count()
}

fn named(h: &Health, inv: &str) -> Vec<String> {
    h.standing().get(inv).cloned().unwrap_or_default()
}

fn names(h: &Health, inv: &str, id: &str) -> bool {
    named(h, inv).iter().any(|s| s.contains(id))
}

/// The loop started at the REAL period: nothing runs for 1.5 s. Then the
/// injected period; returns once the first pass has run (and asserts it ran
/// no sooner than one period).
async fn first_injected_pass(m: &ThreadManager, h: &std::sync::Arc<Health>, l: Limits) {
    assert_eq!(hc::PERIOD, Duration::from_secs(300));
    let _task = hc::spawn(m.clone(), std::sync::Arc::clone(h));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(h.passes(), 0, "a health pass ran before its 5-minute period");
    assert!(h.standing().is_empty(), "{:?}", h.standing());
    let t0 = Instant::now();
    h.set_limits(l);
    wait_for("the first injected health pass", l.period + Duration::from_secs(10), || h.passes() >= 1).await;
    assert!(t0.elapsed() >= l.period - Duration::from_millis(50), "the pass ran before one period");
    std::mem::forget(_task); // the loop lives for the test's runtime
}

async fn next_pass(h: &Health, l: Limits) {
    let n = h.passes();
    wait_for("the next health pass", l.period + Duration::from_secs(10), || h.passes() > n).await;
}

fn quick(period_ms: u64) -> Limits {
    let ms = Duration::from_millis;
    Limits { period: ms(period_ms), stuck_wait: ms(300), idle_child: ms(300), roster_after_end: ms(300), grace: Duration::ZERO }
}

/// The real process table, narrowed to the pids this test owns plus every
/// `running_turns` pid of this test's own store — so another test binary's
/// leaked orphan cannot reach this test, and a real running turn can.
fn owned_procs(m: &ThreadManager, own: std::sync::Arc<std::sync::Mutex<Vec<i64>>>) -> hc::ProcSource {
    let store = m.store.clone();
    std::sync::Arc::new(move || {
        let mut keep: Vec<i64> = own.lock().unwrap().clone();
        keep.extend(store.list_running_turns().map_err(|e| std::io::Error::other(e.to_string()))?.iter().map(|t| t.pid));
        Ok(hc::ps_table()?.into_iter().filter(|p| keep.contains(&p.pid)).collect())
    })
}

fn sleeper() -> std::process::Child {
    std::process::Command::new("/bin/sleep").arg("120").spawn().unwrap()
}

/// **I2 + I3 — A STUCK WAIT AND AN UNFINISHED IDLE CHILD. N = 2 children on
/// p1 (one `end`, one `never`), one healthy child on p2.**
///
/// Both p1 children run to an end whose REPORT is lost (`Halt::LostReport`:
/// the record is written, `child{finished}` is not). The `end` child stays in
/// p1's `outstanding` — I2 — and both stay unfinished — I3. The `never` child
/// is the opposing arm for I2 (not owed, so not a stuck wait); p2's normally
/// finished child is the opposing arm for both. The R10.H1 sweep is held at
/// 10 min so it cannot repair the tree under the check. Fixed by one sweep
/// pass: both clear.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn health_i2_stuck_wait_and_i3_unfinished_child_warn_within_one_period_and_clear_n2() {
    warnings_sink();
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    m.managed().set_sweep_period(Duration::from_secs(600), Duration::from_secs(600));
    let p1 = root(&m, dir.path());
    let p2 = root(&m, dir.path());
    let mut kids = Vec::new();
    for extra in [json!({}), json!({"notify": "hold"})] {
        m.managed().halt_after(Some(Halt::LostReport));
        let c = spawn(&m, &p1, "child task: say done", extra).await;
        wait_for("ended with a record and the report lost", BOUND, || {
            ended(&m, &c) && records(&m, &c) == 1 && m.managed().halt_armed().is_none()
        })
        .await;
        kids.push(c);
    }
    let (ce, cn) = (kids[0].clone(), kids[1].clone());
    let ok = spawn(&m, &p2, "child task: say done", json!({})).await;
    wait_for("the healthy child recorded finished", BOUND, || !finished(&m, &p2, &ok).is_null()).await;
    assert!(finished(&m, &p1, &ce).is_null() && finished(&m, &p1, &cn).is_null(), "{}", children(&m, &p1));

    let l = quick(400);
    tokio::time::sleep(Duration::from_millis(400)).await; // past stuck_wait / idle_child
    let h = Health::with(Limits::default(), std::sync::Arc::new(|| Ok(Vec::<Proc>::new())));
    first_injected_pass(&m, &h, l).await;
    // Doctrine 10: exactly these threads are in violation.
    assert_eq!(named(&h, "I2").len(), 1, "I2: {:?}", h.standing());
    assert!(names(&h, "I2", &format!("parent={p1} child={ce}")), "I2 must name p1/ce: {:?}", h.standing());
    assert!(!names(&h, "I2", &cn), "a wake:never child is no stuck wait: {:?}", h.standing());
    assert_eq!(named(&h, "I3").len(), 2, "I3: {:?}", h.standing());
    assert!(names(&h, "I3", &ce) && names(&h, "I3", &cn), "I3 must name ce and cn: {:?}", h.standing());
    for s in h.standing().values().flatten() {
        assert!(!s.contains(&p2) && !s.contains(&ok), "the healthy p2/ok was named: {s}");
    }
    assert_eq!(health_warnings("I2", &ce), 1, "the I2 warning naming ce did not reach the warnings path");
    assert!(health_warnings("I3", &cn) >= 1, "the I3 warning naming cn did not reach the warnings path");
    // A standing violation is not re-warned every period — and its ids are
    // stable: the next pass runs over a second later, so an id carrying an
    // age in seconds would read as new.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    next_pass(&h, l).await;
    assert_eq!(health_warnings("I2", &ce), 1, "the same standing I2 was warned twice");

    // Fix: one reconciliation pass records both finishes (its live-epilogue
    // settle shrunk below these runs' age).
    m.managed().set_sweep_settle(Duration::ZERO);
    sweep_now(&m).await;
    assert!(!finished(&m, &p1, &ce).is_null() && !finished(&m, &p1, &cn).is_null());
    next_pass(&h, l).await;
    assert!(named(&h, "I2").is_empty() && named(&h, "I3").is_empty(), "not cleared: {:?}", h.standing());
}

/// **I1 — EXECUTING WITH NO LIVE PID, AND A LIVE CLAUDE NO EXECUTING THREAD
/// OWNS. N = 2 executing threads (one dead, one healthy), one stray claude.**
///
/// t1 and t2 are each held by a foreign daemon whose pid is alive (a real
/// `sleep`), so both read `executing`. t2's running turn names a live pid
/// (another `sleep`) — healthy; t1 has no running turn — I1. t3 is not
/// executing while a live claude-like process (`argv[0] = claude`) holds
/// `--session-id t3` — I1. Fixed: t1's lock released, the stray killed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn health_i1_executing_without_a_pid_and_a_stray_claude_warn_within_one_period_and_clear_n2() {
    use std::os::unix::process::CommandExt;
    warnings_sink();
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let (t1, t2, t3) = (root(&m, dir.path()), root(&m, dir.path()), root(&m, dir.path()));
    let mut holder = sleeper();
    let mut turn = sleeper();
    let mut stray = std::process::Command::new("/bin/sh")
        .arg0("claude")
        .args(["-c", "sleep 120; exit 0", "claude", "--session-id", &t3])
        .spawn()
        .unwrap();
    let own = std::sync::Arc::new(std::sync::Mutex::new(vec![holder.id() as i64, turn.id() as i64, stray.id() as i64]));
    for t in [&t1, &t2] {
        assert_eq!(m.store.try_claim_thread(t, "other-boot", holder.id() as i64, "dispatch").unwrap(), threads::store::Claim::Taken);
    }
    m.store
        .upsert_running_turn(&threads::store::RunningTurnRow {
            thread_id: t2.clone(),
            pid: turn.id() as i64,
            spawn_cookie: "c".into(),
            output_path: "/dev/null".into(),
            byte_offset: 0,
            started_at: threads::store::now_rfc3339(),
            prompt: None,
            message_ids: vec![],
            attempt: 1,
        })
        .unwrap();
    assert!(m.thread_state(&t1).unwrap().executing && m.thread_state(&t2).unwrap().executing);
    assert!(!m.thread_state(&t3).unwrap().executing);
    // The stray's command line is what `ps` must classify (a positive sentinel).
    wait_for("the stray is in the table as a claude holding t3", BOUND, || {
        hc::ps_table().unwrap().iter().any(|p| p.pid == stray.id() as i64 && p.claude_like() && p.session().as_deref() == Some(t3.as_str()))
    })
    .await;

    let l = quick(400);
    let h = Health::with(Limits::default(), owned_procs(&m, own));
    first_injected_pass(&m, &h, l).await;
    assert_eq!(named(&h, "I1").len(), 2, "I1: {:?}", h.standing());
    assert!(names(&h, "I1", &format!("thread={t1} executing")), "I1 must name t1: {:?}", h.standing());
    assert!(names(&h, "I1", &format!("thread={t3} pid={}", stray.id())), "I1 must name t3's stray: {:?}", h.standing());
    assert!(!names(&h, "I1", &t2), "the healthy executing t2 was named: {:?}", h.standing());
    assert_eq!(health_warnings("I1", &t1), 1);
    assert_eq!(health_warnings("I1", &t3), 1);
    assert_eq!(health_warnings("I1", &t2), 0);

    m.store.release_thread_lock(&t1, "other-boot").unwrap();
    stray.kill().unwrap();
    stray.wait().unwrap();
    next_pass(&h, l).await;
    assert!(named(&h, "I1").is_empty(), "not cleared: {:?}", h.standing());
    for c in [&mut holder, &mut turn] {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn append(m: &ThreadManager, t: &str, ev: Value) {
    let seq = m.store.max_seq(t).unwrap() + 1;
    let event: ThreadEvent = serde_json::from_value(ev).unwrap();
    m.store
        .append_event(&threads::StoredEvent { thread_id: t.to_string(), seq, message_id: None, ts: threads::store::now_rfc3339(), event })
        .unwrap();
}

/// **I4 — A ROSTER STILL LISTING A DIRECT AGENT AFTER THE TURN ENDED. N = 2
/// listed agents on t1; t2 the opposing arm (its newest roster is empty),
/// t3 the second (only a backgrounded shell listed).** Fixed: one agent's
/// `task_notification` lands — that agent clears and the other stays; the
/// second lands — I4 clears.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn health_i4_a_roster_listing_an_agent_after_the_turn_ended_warns_within_one_period_and_clears_n2() {
    warnings_sink();
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let (t1, t2, t3) = (root(&m, dir.path()), root(&m, dir.path()), root(&m, dir.path()));
    let agent = |id: &str| json!({"task_id": id, "task_type": "local_agent"});
    for (t, a, b) in [(&t1, "ta-1", "ta-2"), (&t2, "tb-1", "tb-2")] {
        for x in [a, b] {
            append(&m, t, json!({"kind": "task_started", "task_id": x, "tool_use_id": format!("tu-{x}"), "task_type": "local_agent"}));
        }
        append(&m, t, json!({"kind": "background_tasks_changed", "tasks": [agent(a), agent(b)]}));
    }
    append(&m, &t2, json!({"kind": "background_tasks_changed", "tasks": []}));
    append(&m, &t3, json!({"kind": "background_tasks_changed", "tasks": [{"task_id": "sh-1", "task_type": "local_bash"}]}));
    for t in [&t1, &t2, &t3] {
        append(&m, t, json!({"kind": "turn_ended", "exit_code": 0}));
    }
    tokio::time::sleep(Duration::from_millis(400)).await; // past roster_after_end

    let l = quick(400);
    let h = Health::with(Limits::default(), std::sync::Arc::new(|| Ok(Vec::<Proc>::new())));
    first_injected_pass(&m, &h, l).await;
    assert_eq!(named(&h, "I4"), vec![
        format!("thread={t1} task=ta-1 listed by the newest roster, no task_notification"),
        format!("thread={t1} task=ta-2 listed by the newest roster, no task_notification"),
    ], "I4 must name exactly t1's two agents");
    assert_eq!(health_warnings("I4", &t1), 1);
    assert_eq!(health_warnings("I4", &t2) + health_warnings("I4", &t3), 0);

    append(&m, &t1, json!({"kind": "task_notification", "task_id": "ta-1", "status": "completed"}));
    next_pass(&h, l).await;
    assert_eq!(named(&h, "I4").len(), 1, "{:?}", h.standing());
    assert!(names(&h, "I4", "ta-2"));
    append(&m, &t1, json!({"kind": "task_notification", "task_id": "ta-2", "status": "completed"}));
    next_pass(&h, l).await;
    assert!(named(&h, "I4").is_empty(), "not cleared: {:?}", h.standing());
}

/// **I5 — AN ORPHANED CLAUDE HOLDING A SESSION THAT IS NO THREAD. N = 2
/// orphans (ppid 1): one holds an unknown session (I5), one holds a real
/// thread's id (not I5).** Each is a real `stall-claude`-shaped script,
/// double-forked so launchd adopts it. Fixed: the leaked one is killed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn health_i5_an_orphaned_claude_with_no_thread_warns_within_one_period_and_clears_n2() {
    use std::os::unix::fs::PermissionsExt;
    warnings_sink();
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let real = root(&m, dir.path());
    let script = dir.path().join("stall-claude-h2");
    std::fs::write(&script, "#!/bin/sh\nsleep 120\nexit 0\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let orphan = |session: &str| -> i64 {
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", "\"$0\" --session-id \"$1\" >/dev/null 2>&1 & echo $!", script.to_str().unwrap(), session])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    };
    let ghost = format!("{}-ghost", uuid_like());
    let leaked = orphan(&ghost);
    let adopted = orphan(&real);
    let own = std::sync::Arc::new(std::sync::Mutex::new(vec![leaked, adopted]));
    wait_for("both orphans are adopted by launchd (ppid 1) and read as claude-like", BOUND, || {
        let t = hc::ps_table().unwrap();
        [leaked, adopted].iter().all(|pid| t.iter().any(|p| p.pid == *pid && p.ppid == 1 && p.claude_like()))
    })
    .await;

    let l = quick(400);
    let h = Health::with(Limits::default(), owned_procs(&m, own));
    first_injected_pass(&m, &h, l).await;
    assert_eq!(named(&h, "I5"), vec![format!("pid={leaked} session={ghost} orphaned claude, no such thread")], "{:?}", h.standing());
    assert_eq!(health_warnings("I5", &ghost), 1);
    assert_eq!(health_warnings("I5", &real), 0, "an orphan holding a REAL thread is not I5");

    unsafe { libc::kill(leaked as i32, libc::SIGKILL) };
    next_pass(&h, l).await;
    assert!(named(&h, "I5").is_empty(), "not cleared: {:?}", h.standing());
    unsafe { libc::kill(adopted as i32, libc::SIGKILL) };
}

fn uuid_like() -> String {
    format!("{:x}-{:x}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
}

/// **A HEALTHY TREE EMITS NOTHING, over three injected periods** — a parent
/// with two children that ended normally (one `end`, one `never`), a thread
/// whose roster emptied before its turn ended, and a child whose turn is
/// RUNNING right now (a real executing thread with a live pid in the table).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn health_a_healthy_tree_emits_nothing_over_three_injected_periods() {
    warnings_sink();
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let ce = spawn(&m, &p, "child task: say done", json!({})).await;
    let cn = spawn(&m, &p, "child task: say done", json!({"notify": "hold"})).await;
    wait_for("both recorded finished", BOUND, || !finished(&m, &p, &ce).is_null() && !finished(&m, &p, &cn).is_null()).await;
    let t = root(&m, dir.path());
    append(&m, &t, json!({"kind": "background_tasks_changed", "tasks": [{"task_id": "x", "task_type": "local_agent"}]}));
    append(&m, &t, json!({"kind": "task_notification", "task_id": "x", "status": "completed"}));
    append(&m, &t, json!({"kind": "background_tasks_changed", "tasks": []}));
    append(&m, &t, json!({"kind": "turn_ended", "exit_code": 0}));
    let release = dir.path().join("release-healthy");
    let run = spawn(&m, &p, &format!("holdturn release={}", release.display()), json!({})).await;
    wait_for("run is running", BOUND, || m.store.get_running_turn(&run).unwrap().is_some()).await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    let l = quick(300);
    let h = Health::with(Limits::default(), owned_procs(&m, std::sync::Arc::new(std::sync::Mutex::new(vec![]))));
    first_injected_pass(&m, &h, l).await;
    next_pass(&h, l).await;
    next_pass(&h, l).await;
    // The sentinel: the table this check saw holds the running turn's pid.
    let pid = m.store.get_running_turn(&run).unwrap().expect("still running").pid;
    assert!(hc::ps_table().unwrap().iter().any(|q| q.pid == pid), "the running turn's pid is not in the table — the arm proves nothing");
    assert!(m.thread_state(&run).unwrap().executing);
    assert!(h.standing().is_empty(), "a healthy tree was warned: {:?}", h.standing());
    for id in [&p, &ce, &cn, &t, &run] {
        for inv in ["I1", "I2", "I3", "I4", "I5"] {
            assert_eq!(health_warnings(inv, id), 0, "{inv} named healthy {id}");
        }
    }
    std::fs::write(&release, b"go").unwrap();
    wait_for("run recorded finished", BOUND, || !finished(&m, &p, &run).is_null()).await;
}
