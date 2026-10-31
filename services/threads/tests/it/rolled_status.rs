//! # `threads.rolled` — every thread's status rolled up over its whole subtree (R9.TS1)
//!
//! Operator ruling (round 42): a thread's status is `active`, `idle` or an
//! error state, nothing else. `active` if the thread OR ANY DESCENDANT, at any
//! depth, is running; `active_descendants` counts descendants running
//! THEMSELVES; the display is `active (+X)` / `active` (never `(+0)`) /
//! `idle` / `error: parked`; the children summary is `N active, M idle`.
//!
//! These run the real daemon: a real spawn tree (`agents_spawn` from the
//! root, then from the CHILD), a grandchild whose turn is genuinely executing
//! (`holdturn`, released by the test), and the `threads` shape read off a
//! connection — the hydrate AND the live fold. The pure rule is pinned in
//! `shapewire::rolled`'s own unit tests (depth ≥ 3 with several running).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shapewire::token::Claims;
use threads::agents;
use threads::config::Config;
use threads::shape::{ConnFrames, Derived, Op, Shape};
use threads::shapes::threads as tshape;
use threads::{build_manager, ThreadEvent, ThreadManager};

static SERIAL: Mutex<()> = Mutex::new(());

fn set_env(tmp: &Path) {
    std::env::set_var("CAULDRON_HOME", tmp.join("cauldron"));
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
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

fn call(rt: &tokio::runtime::Runtime, m: &ThreadManager, caller: &str, tool: &str, args: Value) -> Value {
    let claims = Claims {
        thread_id: caller.to_string(),
        run_id: format!("{caller}/run-1"),
        root: caller.to_string(),
        depth: 0,
        turn: "cookie".into(),
    };
    rt.block_on(agents::call(m, &claims, tool, args))
        .unwrap_or_else(|e| panic!("{tool} from {caller}: {} {}", e.code, e.message))
}

fn turn_ended(m: &ThreadManager, id: &str) -> bool {
    m.store
        .tail_events(id, 1000)
        .unwrap()
        .iter()
        .any(|e| matches!(e.event, ThreadEvent::ResultTurn { .. }))
        && m.store.get_running_turn(id).unwrap().is_none()
}

fn drain(f: &mut ConnFrames) -> Vec<Arc<threads::shape::ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

/// The newest whole row this connection holds for `key` — export or live.
fn try_newest_row(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Option<Value> {
    frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(key))
        .filter_map(|f| match (&f.wire.op, &f.wire.event, &f.wire.row) {
            (Op::Live, Some(Derived::Row { row }), _) => Some(row.clone()),
            (Op::Read, _, Some(row)) => Some(row.clone()),
            _ => None,
        })
        .last()
}

/// The `cells` block of the newest export for `key`.
fn export_cells(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(key) && matches!(f.wire.op, Op::Read))
        .last()
        .and_then(|f| f.wire.cells.clone())
        .unwrap_or(Value::Null)
}

fn display(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    try_newest_row(frames, key).map(|r| r["rolled"]["display"].clone()).unwrap_or(Value::Null)
}

const FRAME_DEADLINE: Duration = Duration::from_secs(20);

struct Fx {
    rt: tokio::runtime::Runtime,
    m: ThreadManager,
    tmp: tempfile::TempDir,
}

fn start() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let m = rt.block_on(async {
        threads::clock::testing::ticking();
        let m = build_manager(config_at(tmp.path())).unwrap();
        m.recover().await;
        m
    });
    Fx { rt, m, tmp }
}

impl Fx {
    fn thread(&self) -> String {
        let _ctx = self.rt.enter();
        let settings = json!({
            "cwd": self.tmp.path().to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
        });
        self.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    }
    /// A child of `caller`, `notify: hold` so its end never wakes the parent
    /// (a woken parent would run a turn of its own and move the answer).
    fn spawn(&self, caller: &str, rid: &str, task: &str) -> String {
        let args = json!({"task": task, "request_id": rid, "notify": "hold"});
        call(&self.rt, &self.m, caller, "agents_spawn", args)["thread_id"]
            .as_str()
            .unwrap()
            .to_string()
    }
    /// A child that REPORTS to its parent (`wake: end`, the default) — a
    /// `notify: hold` child is never reported, so it carries no outcome.
    fn spawn_reporting(&self, caller: &str, rid: &str, task: &str) -> String {
        let args = json!({"task": task, "request_id": rid});
        call(&self.rt, &self.m, caller, "agents_spawn", args)["thread_id"]
            .as_str()
            .unwrap()
            .to_string()
    }
    fn settle(&self, ms: u64) {
        self.rt.block_on(async { tokio::time::sleep(Duration::from_millis(ms)).await });
    }
    fn wait(&self, what: &str, ok: impl Fn(&ThreadManager) -> bool) {
        let deadline = Instant::now() + FRAME_DEADLINE;
        while !ok(&self.m) && Instant::now() < deadline {
            self.settle(25);
        }
        assert!(ok(&self.m), "{what}");
    }
    fn until(
        &self,
        f: &mut ConnFrames,
        acc: &mut Vec<Arc<threads::shape::ShapeFrame>>,
        done: impl Fn(&[Arc<threads::shape::ShapeFrame>]) -> bool,
    ) {
        let deadline = Instant::now() + FRAME_DEADLINE;
        loop {
            acc.extend(drain(f));
            if done(acc) || Instant::now() >= deadline {
                return;
            }
            self.settle(25);
        }
    }
    fn subscribe(&self, conn: u64, keys: &[&String]) -> ConnFrames {
        let f = self.m.hub.connect(conn);
        for k in keys {
            self.m.hub.subscribe(tshape::Threads::NAME, Some(k), conn, None).unwrap();
        }
        f
    }
}

/// root → c (idle, ran and ended) → g (its turn is EXECUTING, held open).
struct Tree {
    root: String,
    c: String,
    g: String,
    release: PathBuf,
}

fn build_tree(fx: &Fx) -> Tree {
    let root = fx.thread();
    let c = fx.spawn(&root, "c", "an ordinary child task");
    fx.wait("the child's turn ended", |m| turn_ended(m, &c));
    let release = fx.tmp.path().join("release-g");
    let g = fx.spawn(&c, "g", &format!("holdturn release={}", release.display()));
    fx.wait("the grandchild is executing", |m| m.thread_state(&g).map(|d| d.executing).unwrap_or(false));
    // SENTINEL (doctrine 9): the three are three different threads, in the
    // lineage the test means, and only g is running — so a right answer at the
    // root and at c can only come from the rollup.
    assert_eq!(fx.m.managed().parent_of(&g), Some(c.clone()));
    assert_eq!(fx.m.managed().parent_of(&c), Some(root.clone()));
    assert_eq!(fx.m.managed().parent_of(&root), None);
    assert!(!fx.m.thread_state(&c).unwrap().executing && fx.m.thread_state(&c).unwrap().state == "idle");
    assert!(!fx.m.thread_state(&root).unwrap().executing && fx.m.thread_state(&root).unwrap().state == "idle");
    Tree { root, c, g, release }
}

/// **HIS TEST, ON THE HYDRATE.** Subscribed after the tree is running: root →
/// idle child → running grandchild reads `active (+1)` at the root AND at the
/// child (a subagent gets the rollup), `active` at the grandchild, and the
/// summaries say `1 active, 0 idle`. All four fields ride every row, `known`.
#[test]
fn his_test_root_idle_child_running_grandchild_hydrates_active_plus_one() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let t = build_tree(&fx);
    let mut f = fx.subscribe(61, &[&t.root, &t.c, &t.g]);
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| {
        [&t.root, &t.c, &t.g].iter().all(|k| try_newest_row(fr, k).is_some())
    });
    let row = |k: &str| try_newest_row(&frames, k).unwrap_or(Value::Null)["rolled"].clone();
    assert_eq!(
        row(&t.root),
        json!({"status": "active", "error": null, "active_descendants": 1, "display": "active (+1)",
               "children_active": 1, "children_idle": 0, "children_error": 0,
               "children_summary": "1 active, 0 idle"}),
        "the root: one running descendant, counted once"
    );
    assert_eq!(
        row(&t.c),
        json!({"status": "active", "error": null, "active_descendants": 1, "display": "active (+1)",
               "children_active": 1, "children_idle": 0, "children_error": 0,
               "children_summary": "1 active, 0 idle"}),
        "the SUBAGENT gets the rollup too — it is not idle"
    );
    assert_eq!(
        row(&t.g),
        json!({"status": "active", "error": null, "active_descendants": 0, "display": "active",
               "children_active": null, "children_idle": null, "children_error": null,
               "children_summary": null}),
        "a running leaf: plain `active`, never `(+0)`, and no summary"
    );
    for k in [&t.root, &t.c, &t.g] {
        assert_eq!(export_cells(&frames, k)["rolled"]["state"], json!("known"), "{k}");
    }
    std::fs::write(&t.release, b"go").unwrap();
    fx.wait("the grandchild ended", |m| turn_ended(m, &t.g));
    fx.rt.shutdown_background();
}

/// **THE FOLD.** Subscribed while g runs; then g is released and its status
/// edge re-states the whole chain, live, with no re-subscribe: everything is
/// `idle`, the root's child counts move to `0 active, 1 idle`.
#[test]
fn a_status_edge_restates_the_whole_ancestor_chain_live() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let t = build_tree(&fx);
    let mut f = fx.subscribe(62, &[&t.root, &t.c, &t.g]);
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| display(fr, &t.root) == json!("active (+1)"));
    assert_eq!(display(&frames, &t.root), json!("active (+1)"), "the starting level");
    std::fs::write(&t.release, b"go").unwrap();
    fx.wait("the grandchild ended", |m| turn_ended(m, &t.g) && !m.thread_state(&t.g).unwrap().executing);
    fx.until(&mut f, &mut frames, |fr| {
        [&t.root, &t.c, &t.g].iter().all(|k| display(fr, k) == json!("idle"))
    });
    let live: Vec<Value> = frames
        .iter()
        .filter(|x| x.wire.key.as_deref() == Some(t.root.as_str()) && matches!(x.wire.op, Op::Live))
        .filter_map(|x| match &x.wire.event {
            Some(Derived::Row { row }) => Some(row["rolled"].clone()),
            _ => None,
        })
        .collect();
    assert!(!live.is_empty(), "a LIVE row frame moved the root's held cell");
    for k in [&t.root, &t.c, &t.g] {
        assert_eq!(display(&frames, k), json!("idle"), "{k} after the release");
    }
    let root = try_newest_row(&frames, &t.root).unwrap()["rolled"].clone();
    assert_eq!(root["children_summary"], json!("0 active, 1 idle"), "{root}");
    assert_eq!(root["active_descendants"], json!(0), "{root}");
    fx.rt.shutdown_background();
}

/// **THE ERROR STATE, AND ITS LABEL.** A parked child (a durable park row —
/// the derivation's `parked`) under an idle root, nothing running: the child
/// reads `error: parked`; the root does NOT inherit it (errors do not roll up)
/// — it reads `idle` and counts the child as `1 error` in its summary.
/// Opposed: the park cleared, the child reads `idle` and the summary
/// `0 active, 1 idle`.
#[test]
fn a_parked_child_is_the_error_state_with_its_label() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let root = fx.thread();
    let c = fx.spawn(&root, "c", "an ordinary child task");
    fx.wait("the child's turn ended", |m| turn_ended(m, &c) && !m.thread_state(&c).unwrap().executing);
    // HOLD THE PARK STILL: a durable park is re-offered to placement by
    // `resume_sweep` on any placement event, and a dispatched park is no longer
    // a park. A drain refuses every new turn (`maybe_dispatch`), so the row
    // written below is the only thing that moves `c` until the test clears it.
    fx.m.update.begin_drain();
    fx.m.store
        .upsert_parked_turn(&threads::store::ParkedTurnRow {
            thread_id: c.clone(),
            prompt: "held".into(),
            message_ids: vec![],
            attempt: 0,
            reason: "test park".into(),
            budget_key: None,
            parked_at: "2026-10-25T00:00:00Z".into(),
        })
        .unwrap();
    assert_eq!(fx.m.thread_state(&c).unwrap().state, "parked", "sentinel: the derivation sees the park");
    let mut f = fx.subscribe(63, &[&root, &c]);
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| try_newest_row(fr, &root).is_some() && try_newest_row(fr, &c).is_some());
    let rolled = |fr: &[Arc<threads::shape::ShapeFrame>], k: &str| try_newest_row(fr, k).unwrap()["rolled"].clone();
    assert_eq!(rolled(&frames, &c)["display"], json!("error: parked"));
    assert_eq!(rolled(&frames, &c)["status"], json!("error"));
    assert_eq!(rolled(&frames, &c)["error"], json!("parked"));
    assert_eq!(rolled(&frames, &root)["display"], json!("idle"), "an error does not roll up");
    assert_eq!(rolled(&frames, &root)["children_summary"], json!("0 active, 0 idle, 1 error"));
    assert_eq!(rolled(&frames, &root)["children_error"], json!(1));

    // SENTINEL: the park held for the whole read (nothing dispatched it).
    assert_eq!(fx.m.thread_state(&c).unwrap().state, "parked", "the park is still the derivation's answer");
    assert_eq!(fx.m.thread_state(&root).unwrap().state, "idle", "and nothing ran the root");
    fx.m.store.clear_parked_turn(&c).unwrap();
    fx.m.emit_rolled(&c);
    fx.until(&mut f, &mut frames, |fr| display(fr, &c) == json!("idle"));
    assert_eq!(rolled(&frames, &c)["display"], json!("idle"));
    assert_eq!(rolled(&frames, &root)["children_summary"], json!("0 active, 1 idle"));
    fx.rt.shutdown_background();
}

/// **A FAILED RUN IS AN ERROR STATE.** A child whose managed run reported
/// `failed` (`badmodel` fails deterministically; a ladder of one gives it up)
/// reads `error: failed`; paired with an `ok` sibling, which reads `idle`.
#[test]
fn a_child_whose_run_failed_reads_error_failed() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let root = {
        let _ctx = fx.rt.enter();
        let settings = json!({
            "cwd": fx.tmp.path().to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
            "retry_policy": {"max_deterministic_attempts": 1},
        });
        fx.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    };
    let bad = fx.spawn_reporting(&root, "bad", "badmodel");
    let good = fx.spawn_reporting(&root, "good", "an ordinary child task");
    let reported = |m: &ThreadManager, id: &str| {
        let l = m.managed().load(&root).unwrap().unwrap();
        tshape::children_row(&l).runs.iter().any(|r| r.id == id && r.finished.is_some())
    };
    fx.wait("both children reported", |m| reported(m, &bad) && reported(m, &good));
    let l = fx.m.managed().load(&root).unwrap().unwrap();
    let outcome = |id: &str| {
        tshape::children_row(&l).runs.iter().find(|r| r.id == id).unwrap().finished.clone().unwrap().outcome
    };
    // SENTINEL: the two runs really did come out differently.
    assert_eq!((outcome(&bad).as_str(), outcome(&good).as_str()), ("failed", "ok"));
    let rows = tshape::rolled_rows(&fx.m, &root, &[root.clone(), bad.clone(), good.clone()]).unwrap();
    let get = |id: &str| rows.iter().find(|(k, _)| k == id).unwrap().1.clone();
    assert_eq!(get(&bad).display, "error: failed");
    assert_eq!(get(&good).display, "idle");
    assert_eq!(get(&root).children_summary.as_deref(), Some("0 active, 1 idle, 1 error"));
    fx.rt.shutdown_background();
}

/// **A FAILED CHILD'S OWN ROW SAYS `error: failed` — LIVE AND ON A FRESH
/// HYDRATE** (R9.TS1 inspect F1). Read through the SHAPE the faces read, never
/// through `rolled_rows` from the root.
///
/// The children are held by a subscriber BEFORE they run: a drain holds their
/// first turns back (the spawn queues them), the subscribe exports them, and
/// only then does the drain end and `resume_sweep` place them — so the error
/// reaches the held row by the LIVE fold. Opposed: the `ok` sibling's row is
/// `idle` on both paths, and the parent counts `1 idle, 1 error`.
#[test]
fn a_failed_childs_own_row_reads_error_failed_live_and_on_hydrate() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let root = {
        let _ctx = fx.rt.enter();
        let settings = json!({
            "cwd": fx.tmp.path().to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
            "retry_policy": {"max_deterministic_attempts": 1},
        });
        fx.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    };
    fx.m.update.begin_drain();
    let bad = fx.spawn_reporting(&root, "bad", "badmodel");
    let good = fx.spawn_reporting(&root, "good", "an ordinary child task");
    let mut f = fx.subscribe(64, &[&bad, &good]);
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| try_newest_row(fr, &bad).is_some() && try_newest_row(fr, &good).is_some());
    // SENTINEL: neither child has run yet, so whatever the rows say next can
    // only have arrived live.
    assert!(!turn_ended(&fx.m, &bad) && !turn_ended(&fx.m, &good), "the drain held both first turns");
    assert_eq!(display(&frames, &bad), json!("idle"), "the held row before the run");
    let exported = frames.len();
    fx.m.update.end_drain();
    {
        // Placement spawns the turns: it runs inside the daemon's runtime.
        let _ctx = fx.rt.enter();
        fx.m.resume_sweep();
    }
    let reported = |m: &ThreadManager, id: &str| {
        let l = m.managed().load(&root).unwrap().unwrap();
        tshape::children_row(&l).runs.iter().any(|r| r.id == id && r.finished.is_some())
    };
    fx.wait("both children reported", |m| reported(m, &bad) && reported(m, &good));
    let l = fx.m.managed().load(&root).unwrap().unwrap();
    let outcome = |id: &str| {
        tshape::children_row(&l).runs.iter().find(|r| r.id == id).unwrap().finished.clone().unwrap().outcome
    };
    assert_eq!((outcome(&bad).as_str(), outcome(&good).as_str()), ("failed", "ok"), "sentinel: different outcomes");
    fx.until(&mut f, &mut frames, |fr| display(fr, &bad) == json!("error: failed"));
    let live_bad: Vec<Value> = frames[exported..]
        .iter()
        .filter(|x| x.wire.key.as_deref() == Some(bad.as_str()) && matches!(x.wire.op, Op::Live))
        .filter_map(|x| match &x.wire.event {
            Some(Derived::Row { row }) => Some(row["rolled"]["display"].clone()),
            _ => None,
        })
        .collect();
    assert_eq!(live_bad.last(), Some(&json!("error: failed")), "the failed child's LIVE row: {live_bad:?}");
    assert_eq!(display(&frames, &good), json!("idle"), "the ok sibling's live row");

    // A FRESH HYDRATE, keyed by the child. A second subscribe while the root
    // still holds the key would only re-export the LIVE-updated cell, so the
    // first connection is dropped and the lazy root is waited out first
    // (sentinel: the shape stops tracking), and the read below is a hydrate.
    drop(f);
    fx.m.hub.dropped(64);
    fx.wait("the lazy threads root was dropped", |m| !m.hub.tracked(tshape::Threads::NAME));
    let mut f2 = fx.subscribe(65, &[&bad, &good]);
    let mut fresh = Vec::new();
    fx.until(&mut f2, &mut fresh, |fr| try_newest_row(fr, &bad).is_some() && try_newest_row(fr, &good).is_some());
    let row = |k: &str| try_newest_row(&fresh, k).unwrap()["rolled"].clone();
    assert_eq!(row(&bad)["display"], json!("error: failed"), "hydrate: {}", row(&bad));
    assert_eq!(row(&bad)["error"], json!("failed"));
    assert_eq!(row(&good)["display"], json!("idle"), "hydrate: {}", row(&good));
    let rows = tshape::rolled_rows(&fx.m, &root, std::slice::from_ref(&root)).unwrap();
    assert_eq!(rows[0].1.children_summary.as_deref(), Some("0 active, 1 idle, 1 error"));
    fx.rt.shutdown_background();
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
