//! # N-2 — a child's budget key follows its parent's re-point
//!
//! A managed child is admitted against the budget key its thread row carries,
//! copied from its parent at spawn. When the key runs out the operator
//! re-points the PARENT (`threads/{thread_id}/budget_assign`). Before N-2 the
//! children stayed parked on the spent key and the parent waited on them
//! forever. Now the re-point carries every descendant still on the parent's
//! old key, and the re-point re-runs admission, so the parked child goes out
//! under the new key with no restart.
//!
//! The guards are an OPPOSED pair inside one daemon:
//! * under parent `P` (re-pointed K1 → K2): child `C` and grandchild `G`, both
//!   parked on K1, are admitted under K2 and their turns run to the end;
//! * under parent `Q` (on the same spent K1, NOT re-pointed): child `D` stays
//!   on K1 and stays parked — and it is handed an explicit admission
//!   opportunity (an empty settings update re-runs `maybe_dispatch`) AFTER
//!   `C`/`G` have run, so "still parked" is the gate's answer, not the absence
//!   of a question. K1's window is the session window (hours); nothing in this
//!   test can roll it over, so K1 stays spent for the whole run.
//!
//! No real claude runs (`tests/fixtures/fake-claude`); every call is
//! in-process — the façade through `agents::call`, the WS methods through the
//! `ThreadsProducer` the chassis dispatches to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shapewire::server::Producer;
use shapewire::token::Claims;
use threads::chassis::ThreadsProducer;
use threads::config::Config;
use threads::event::ThreadEvent;
use threads::{build_manager, ThreadManager};

fn set_env(tmp: &Path) {
    std::env::set_var("CAULDRON_HOME", tmp.join("cauldron"));
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    // The resume tick is stretched out of the test's reach: every admission
    // below is the one `update_settings` / `send` asks for, never a timer's.
    std::env::set_var("THREADS_RESUME_TICK_MS", "600000");
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

fn events(m: &ThreadManager, id: &str) -> Vec<ThreadEvent> {
    m.store.tail_events(id, 1000).unwrap().into_iter().map(|e| e.event).collect()
}

fn count(m: &ThreadManager, id: &str, pred: impl Fn(&ThreadEvent) -> bool) -> usize {
    events(m, id).iter().filter(|e| pred(e)).count()
}

fn ended(e: &ThreadEvent) -> bool {
    matches!(e, ThreadEvent::TurnEnded { .. })
}

fn started(e: &ThreadEvent) -> bool {
    matches!(e, ThreadEvent::TurnStarted { .. })
}

fn parked_on(key: &'static str) -> impl Fn(&ThreadEvent) -> bool {
    move |e| matches!(e, ThreadEvent::TurnParked { cause, budget_key, .. } if cause == "budget" && budget_key.as_deref() == Some(key))
}

async fn until(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

async fn ws(p: &ThreadsProducer, method: &str, params: Value) -> Value {
    p.handle(method, params.clone())
        .await
        .unwrap_or_else(|e| panic!("{method} {params}: {} {}", e.code, e.message))
}

async fn spawn(m: &ThreadManager, caller: &str, task: &str) -> String {
    let claims = Claims {
        thread_id: caller.to_string(),
        run_id: format!("{caller}/run-1"),
        root: caller.to_string(),
        depth: 0,
        turn: "cookie".into(),
    };
    let r = threads::agents::call(m, &claims, "agents_spawn", json!({"task": task, "notify": "hold"}))
        .await
        .unwrap_or_else(|e| panic!("spawn: {} {}", e.code, e.message));
    r["thread_id"].as_str().unwrap().to_string()
}

fn thread_on(m: &ThreadManager, cwd: &Path, key: &str) -> String {
    let s = json!({"cwd": cwd.to_string_lossy(), "model": "sonnet", "account": {"named": "aa-owned"}, "budget_key": key});
    m.create_thread(None, serde_json::from_value(s).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn re_pointing_a_parent_re_admits_its_parked_descendants_and_only_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    threads::clock::testing::ticking();
    let m = build_manager(config_at(tmp.path())).unwrap();
    m.recover().await;
    let p = ThreadsProducer::new(Arc::new(m.clone()));
    let cwd = tmp.path();

    // K1 spends out on any turn at all (priced on the prior, uncalibrated);
    // K2 is generous.
    ws(&p, "budgets/{key}/set", json!({"key": "k1", "session_pct": 0.000_001, "accounts": ["aa-owned"]})).await;
    ws(&p, "budgets/{key}/set", json!({"key": "k2", "session_pct": 100.0, "accounts": ["aa-owned"]})).await;

    // Spend K1 out with one turn on a bystander thread.
    let burn = thread_on(&m, cwd, "k1");
    ws(&p, "threads/{thread_id}/send", json!({"thread_id": burn, "text": "hello"})).await;
    assert!(until(30, || count(&m, &burn, ended) >= 1).await, "the burner turn never ended");
    let k1 = ws(&p, "budgets/{key}/get", json!({"key": "k1"})).await;
    assert_eq!(k1["tripped"], true, "K1 must be spent before the children spawn: {k1}");

    // P → C → G, and Q → D, all on K1, all parked on it.
    let parent_p = thread_on(&m, cwd, "k1");
    let parent_q = thread_on(&m, cwd, "k1");
    let c = spawn(&m, &parent_p, "child of P").await;
    let g = spawn(&m, &c, "grandchild of P").await;
    let d = spawn(&m, &parent_q, "child of Q").await;
    for (name, id) in [("C", &c), ("G", &g), ("D", &d)] {
        assert_eq!(m.thread_budget_key(id).unwrap().as_deref(), Some("k1"), "{name} inherits K1");
        assert!(
            until(30, || count(&m, id, parked_on("k1")) >= 1).await,
            "{name} never parked on K1: {:?}",
            events(&m, id)
        );
        assert_eq!(count(&m, id, started), 0, "{name} must not run on a spent key");
    }

    // The operator re-points P only.
    ws(&p, "threads/{thread_id}/budget_assign", json!({"thread_id": parent_p, "key": "k2"})).await;

    // P's descendants follow and RUN — no restart, no timer (the resume tick
    // is 600 s).
    for (name, id) in [("C", &c), ("G", &g)] {
        assert!(
            until(30, || count(&m, id, ended) >= 1).await,
            "{name} was not re-admitted under K2 after its parent was re-pointed: key {:?}, events {:?}",
            m.thread_budget_key(id).unwrap(),
            events(&m, id)
        );
        assert_eq!(m.thread_budget_key(id).unwrap().as_deref(), Some("k2"), "{name} is on K2 now");
    }

    // Q was not re-pointed: D stays on spent K1 and stays parked, even when
    // handed an explicit admission opportunity after C and G have run.
    let empty: threads::settings::SettingsPatch = serde_json::from_value(json!({})).unwrap();
    m.update_settings(&d, empty).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(m.thread_budget_key(&d).unwrap().as_deref(), Some("k1"), "D was not re-pointed");
    assert_eq!(count(&m, &d, started), 0, "D ran on a spent key: {:?}", events(&m, &d));
    assert_eq!(m.thread_budget_key(&parent_q).unwrap().as_deref(), Some("k1"));
}

fn key(m: &ThreadManager, id: &str) -> Option<String> {
    m.thread_budget_key(id).unwrap()
}

/// One daemon with generous keys `ka`, `kb`, `kc` (nothing here parks).
async fn generous() -> (tempfile::TempDir, ThreadManager, ThreadsProducer) {
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    threads::clock::testing::ticking();
    let m = build_manager(config_at(tmp.path())).unwrap();
    m.recover().await;
    let p = ThreadsProducer::new(Arc::new(m.clone()));
    for k in ["ka", "kb", "kc"] {
        ws(&p, "budgets/{key}/set", json!({"key": k, "session_pct": 100.0, "accounts": ["aa-owned"]})).await;
    }
    (tmp, m, p)
}

/// R9.N2b guard 1 — **a descendant pointed ELSEWHERE keeps its key, and so
/// does its subtree.** P → C (on P's key) and P → C2 → G2, with C2 then
/// re-pointed directly to `kc` (G2 follows C2). Re-pointing P `ka → kb` moves
/// C (the opposed half: the walk does move what is on the old key) and leaves
/// C2 and G2 on `kc`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_descendant_on_another_key_keeps_it_and_so_does_its_subtree() {
    let (tmp, m, p) = generous().await;
    let parent = thread_on(&m, tmp.path(), "ka");
    let c = spawn(&m, &parent, "child on the parent's key").await;
    let c2 = spawn(&m, &parent, "child pointed elsewhere").await;
    let g2 = spawn(&m, &c2, "grandchild under the elsewhere child").await;
    ws(&p, "threads/{thread_id}/budget_assign", json!({"thread_id": c2, "key": "kc"})).await;
    assert_eq!(key(&m, &c2).as_deref(), Some("kc"), "C2 pointed at kc directly");
    assert_eq!(key(&m, &g2).as_deref(), Some("kc"), "G2 followed C2 to kc");
    assert_eq!(key(&m, &c).as_deref(), Some("ka"));

    let answer = ws(&p, "threads/{thread_id}/budget_assign", json!({"thread_id": parent, "key": "kb"})).await;
    assert_eq!(answer["changed"], true, "{answer}");
    assert_eq!(key(&m, &c).as_deref(), Some("kb"), "C was on the parent's old key and follows it");
    assert_eq!(key(&m, &c2).as_deref(), Some("kc"), "C2 was pointed at kc on purpose and keeps it");
    assert_eq!(key(&m, &g2).as_deref(), Some("kc"), "G2 follows C2, not the root");
}

/// R9.N2b guard 2 — **a spawn racing `budget_assign` lands on the NEW key.**
/// The seam runs the real `threads/{thread_id}/budget_assign` arm INSIDE the
/// child's `materialize`, after it read the parent's row (key `ka`) and before
/// it creates the child's row — so the re-point's walk cannot see the child.
/// The child must still end up on `kb`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spawn_racing_budget_assign_lands_on_the_new_key() {
    let (tmp, m, _p) = generous().await;
    let parent = thread_on(&m, tmp.path(), "ka");
    let ran = Arc::new(std::sync::Mutex::new(None::<Value>));
    {
        let ran = ran.clone();
        let racer = ThreadsProducer::new(Arc::new(m.clone()));
        let pid = parent.clone();
        let rt = tokio::runtime::Handle::current();
        // The seam is synchronous (it runs inside `materialize`); the WS arm
        // is async, so it is driven to completion on a fresh OS thread while
        // the spawn waits — the spawn cannot proceed until the re-point is done.
        threads::managed::testing::in_materialize_window(&parent, move || {
            let answer = std::thread::spawn(move || {
                rt.block_on(ws(&racer, "threads/{thread_id}/budget_assign", json!({"thread_id": pid, "key": "kb"})))
            })
            .join()
            .expect("the racing budget_assign panicked");
            *ran.lock().unwrap() = Some(answer);
        });
    }
    let child = spawn(&m, &parent, "spawned while the parent is re-pointed").await;
    // Sentinel: the re-point really ran inside the window (else this test
    // proves nothing about the race).
    let answer = ran.lock().unwrap().clone().expect("the budget_assign never ran inside materialize's window");
    assert_eq!(answer["changed"], true, "{answer}");
    assert_eq!(key(&m, &parent).as_deref(), Some("kb"));
    assert_eq!(key(&m, &child).as_deref(), Some("kb"), "the racing child was stranded on the parent's old key");
}

/// R9.N2b guard 3 — **a failed descendant re-point is named in the answer.**
/// P → C1, P → C2 → G2, all on `ka`. C2's re-point is made to fail (seam).
/// Re-pointing P to `kb` moves C1 and answers `left_on_old_key` naming C2 and
/// its subtree (G2), which stay on `ka`. Paired: a clean re-point of Q answers
/// exactly `{promise_id, visible, changed}`, as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_descendant_re_point_is_named_and_a_clean_one_answers_as_before() {
    let (tmp, m, p) = generous().await;
    let parent = thread_on(&m, tmp.path(), "ka");
    let c1 = spawn(&m, &parent, "child that moves").await;
    let c2 = spawn(&m, &parent, "child whose re-point fails").await;
    let g2 = spawn(&m, &c2, "grandchild under the failed child").await;
    threads::managed::testing::fail_repoint_of(&c2);
    let answer = ws(&p, "threads/{thread_id}/budget_assign", json!({"thread_id": parent, "key": "kb"})).await;
    assert_eq!(key(&m, &c1).as_deref(), Some("kb"));
    assert_eq!(key(&m, &c2).as_deref(), Some("ka"), "the injected failure left C2 on ka");
    assert_eq!(key(&m, &g2).as_deref(), Some("ka"));
    let left: Vec<&str> = answer["left_on_old_key"]
        .as_array()
        .unwrap_or_else(|| panic!("the answer does not name the descendants left on the old key: {answer}"))
        .iter()
        .map(|e| e["thread_id"].as_str().unwrap())
        .collect();
    let mut left_sorted = left.clone();
    left_sorted.sort();
    let mut want = vec![c2.as_str(), g2.as_str()];
    want.sort();
    assert_eq!(left_sorted, want, "{answer}");
    assert!(answer["left_on_old_key"].as_array().unwrap().iter().all(|e| e["budget_key"] == "ka"), "{answer}");

    let q = thread_on(&m, tmp.path(), "ka");
    let d = spawn(&m, &q, "child of a clean re-point").await;
    let clean = ws(&p, "threads/{thread_id}/budget_assign", json!({"thread_id": q, "key": "kb"})).await;
    assert_eq!(key(&m, &d).as_deref(), Some("kb"));
    let mut fields: Vec<&String> = clean.as_object().unwrap().keys().collect();
    fields.sort();
    assert_eq!(fields, ["changed", "promise_id", "visible"], "a clean re-point answers as before: {clean}");
    assert_eq!(clean["changed"], true);
}

/// Arm `arm` (one of the `threads::managed::testing` window seams) for
/// `parent`'s next spawn so that it drives the REAL
/// `threads/{thread_id}/budget_assign` arm for `target` -> `key` to completion
/// inside that window. The seams are synchronous (they run inside
/// `materialize`) and the WS arm is async, so it runs on a fresh OS thread
/// while the spawn waits. Returns the slot the answer lands in: `None` after
/// the spawn means the window never fired.
fn race_in(
    arm: fn(&str, Box<dyn FnOnce() + Send + 'static>),
    m: &ThreadManager,
    parent: &str,
    target: &str,
    key: &'static str,
) -> Arc<std::sync::Mutex<Option<Value>>> {
    let ran = Arc::new(std::sync::Mutex::new(None::<Value>));
    let slot = ran.clone();
    let racer = ThreadsProducer::new(Arc::new(m.clone()));
    let target = target.to_string();
    let rt = tokio::runtime::Handle::current();
    arm(
        parent,
        Box::new(move || {
            let answer = std::thread::spawn(move || {
                rt.block_on(ws(&racer, "threads/{thread_id}/budget_assign", json!({"thread_id": target, "key": key})))
            })
            .join()
            .expect("the racing budget_assign panicked");
            *slot.lock().unwrap() = Some(answer);
        }),
    );
    ran
}

fn materialize_window(parent: &str, f: Box<dyn FnOnce() + Send + 'static>) {
    threads::managed::testing::in_materialize_window(parent, f);
}

fn reread_window(parent: &str, f: Box<dyn FnOnce() + Send + 'static>) {
    threads::managed::testing::in_reread_window(parent, f);
}

fn landed(slot: &Arc<std::sync::Mutex<Option<Value>>>, which: &str) -> Value {
    slot.lock().unwrap().clone().unwrap_or_else(|| panic!("the {which} budget_assign never ran inside its window"))
}

/// R9.N2c guard (inspect F2) — **TWO re-points of one parent during one
/// spawn never leave the child on a stale key.** Re-point #1 (`ka -> kb`)
/// lands after `materialize` read the parent's row (the child has no row, so
/// its walk skips it); re-point #2 (`kb -> kc`) lands after `materialize`
/// re-read the parent's key as `kb` and before it re-points the child — so #2's
/// walk finds the child still on `ka`, not on its old key `kb`, and skips it
/// too. The child must still end on the parent's FINAL key, `kc`, not on the
/// `kb` the re-read saw. Both re-points are deterministic (each runs to
/// completion inside its window while the spawn waits), so there is no
/// period to outlast.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_re_points_during_one_spawn_leave_the_child_on_the_parents_final_key() {
    let (tmp, m, _p) = generous().await;
    let parent = thread_on(&m, tmp.path(), "ka");
    let first = race_in(materialize_window, &m, &parent, &parent, "kb");
    let second = race_in(reread_window, &m, &parent, &parent, "kc");
    let child = spawn(&m, &parent, "spawned while the parent is re-pointed twice").await;
    // Sentinels: both re-points really ran inside their windows and moved
    // the parent (else this proves nothing about the race).
    assert_eq!(landed(&first, "first")["changed"], true);
    assert_eq!(landed(&second, "second")["changed"], true);
    assert_eq!(key(&m, &parent).as_deref(), Some("kc"));
    assert_eq!(
        key(&m, &child).as_deref(),
        Some("kc"),
        "the child was left on a stale key while its parent is on kc"
    );
}

/// R9.N2c guard, the opposed half — **a child pointed ELSEWHERE during its
/// own spawn keeps that key.** Same two windows, but the second
/// `budget_assign` targets the CHILD (`kc`, a deliberate per-child choice)
/// while the parent stays on `kb`. Following the parent must not undo it:
/// "always copy the parent's key into the child" fails here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_pointed_elsewhere_during_its_spawn_keeps_that_key() {
    let (tmp, m, _p) = generous().await;
    let parent = thread_on(&m, tmp.path(), "ka");
    let first = race_in(materialize_window, &m, &parent, &parent, "kb");
    // The child's id is minted by the spawn; the second window reads the
    // child off the parent's committed children at the moment it fires.
    let ran = Arc::new(std::sync::Mutex::new(None::<Value>));
    {
        let slot = ran.clone();
        let racer = ThreadsProducer::new(Arc::new(m.clone()));
        let mm = m.clone();
        let pid = parent.clone();
        let rt = tokio::runtime::Handle::current();
        threads::managed::testing::in_reread_window(&parent, move || {
            let kids = mm.managed().children_of(&pid);
            assert_eq!(kids.len(), 1, "one child in flight: {kids:?}");
            let target = kids[0].clone();
            let answer = std::thread::spawn(move || {
                rt.block_on(ws(&racer, "threads/{thread_id}/budget_assign", json!({"thread_id": target, "key": "kc"})))
            })
            .join()
            .expect("the racing budget_assign panicked");
            *slot.lock().unwrap() = Some(answer);
        });
    }
    let child = spawn(&m, &parent, "spawned, then pointed elsewhere").await;
    assert_eq!(landed(&first, "first")["changed"], true);
    assert_eq!(landed(&ran, "child's")["changed"], true);
    assert_eq!(key(&m, &parent).as_deref(), Some("kb"));
    assert_eq!(key(&m, &child).as_deref(), Some("kc"), "the child's own key was overwritten by its parent's");
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
