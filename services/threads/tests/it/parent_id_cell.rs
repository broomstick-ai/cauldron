//! # `threads.parent_id` — a face draws a subagent under its parent off ONE row
//!
//! R4.S12 (R-5, DEC-32). The R4.S5.live proof had to read the parent link
//! through `agents/list {caller: parent}` and the child's history metadata,
//! because the `threads` shape carried no parent. This cell carries it, read
//! from the SAME ledger fact `agents/list` answers from (`ThreadMeta.parent_id`).
//!
//! The fixture is a REAL spawn tree through `agents_spawn` (fake-claude, no
//! model): root → child → grandchild. Three distinct ids, so a cell wired to
//! the wrong id is red:
//!
//! * the root's cell is `null` and `known` (not `?`);
//! * the child's is the root;
//! * the grandchild's is the CHILD, not the root — a cell that published the
//!   tree's root, or the run's `root` claim, is red here;
//! * and every one equals `agents/list`'s `parent_id` for the same thread.
//!
//! **MUTATIONS** (recorded in the report): (a) drop the cell from
//! `ThreadCells::to_json` → red (`Null` where the root's id is due);
//! (b) `Handles<ThreadParent>` answers the thread's OWN id → red;
//! (c) it answers `None` always → red; (d) it answers the root-of-tree's id
//! (walk to the top) → red on the grandchild.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shapewire::token::Claims;
use threads::agents;
use threads::config::Config;
use threads::shape::{ConnFrames, Op, Shape};
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

/// The export (`Op::Read`) row and its cells block for `key`.
fn export(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> (Value, Value) {
    let f = frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(key) && matches!(f.wire.op, Op::Read))
        .last()
        .unwrap_or_else(|| panic!("no export for `{key}` among {} frames", frames.len()));
    (
        f.wire.row.clone().unwrap_or(Value::Null),
        f.wire.cells.clone().unwrap_or(Value::Null),
    )
}

/// **EACH ROW NAMES ITS PARENT; THE ROOT'S IS A KNOWN `null`; AND IT IS THE
/// FACT `agents/list` ANSWERS FROM.**
#[test]
fn each_threads_row_carries_its_parent_and_a_root_carries_null() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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
        // `recover` runs `start_watchers`, which registers the shapes.
        m.recover().await;
        m
    });

    let root = {
        let _ctx = rt.enter();
        let settings = json!({
            "cwd": tmp.path().to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
        });
        m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    };
    let spawn = |caller: &str, rid: &str| -> String {
        call(&rt, &m, caller, "agents_spawn", json!({"task": format!("task-{rid}"), "request_id": rid}))
            ["thread_id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    // A thread that never spawned and was never spawned: NO managed ledger at
    // all — the `Ok(None)` arm, which `root` (whose spawns gave it a ledger)
    // does not reach. R4.S12 inspector F-1.
    let lone = {
        let _ctx = rt.enter();
        let settings = json!({
            "cwd": tmp.path().to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
        });
        m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    };
    let child = spawn(&root, "child");
    let grand = spawn(&child, "grand");
    assert!(
        root != child && child != grand && root != grand,
        "three distinct ids, or a wrong-id wiring cannot be told from the right one"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while !(turn_ended(&m, &child) && turn_ended(&m, &grand)) && Instant::now() < deadline {
        rt.block_on(async { tokio::time::sleep(Duration::from_millis(25)).await });
    }
    assert!(turn_ended(&m, &child) && turn_ended(&m, &grand), "both spawned runs ended");

    // ── subscribe, keyed, one key per thread, cold ───────────────────────
    let mut f = m.hub.connect(41);
    for k in [&root, &child, &grand, &lone] {
        m.hub
            .subscribe(tshape::Threads::NAME, Some(k), 41, None)
            .expect("a keyed subscribe to threads");
    }
    rt.block_on(async { tokio::time::sleep(Duration::from_millis(300)).await });
    let frames = drain(&mut f);

    let (r_row, r_cells) = export(&frames, &root);
    let (c_row, c_cells) = export(&frames, &child);
    let (g_row, g_cells) = export(&frames, &grand);
    let (l_row, l_cells) = export(&frames, &lone);

    assert!(
        l_row.as_object().is_some_and(|o| o.contains_key("parent_id")) && l_row["parent_id"].is_null(),
        "a thread with no managed ledger is a root: null: {l_row}"
    );
    assert_eq!(
        l_cells["parent_id"]["state"],
        json!("known"),
        "no ledger is a COMPLETE answer (a root), never `?`: {l_cells}"
    );

    assert!(
        r_row.as_object().is_some_and(|o| o.contains_key("parent_id")),
        "the row CARRIES the cell; a root's absent key is not a root's null: {r_row}"
    );
    assert_eq!(r_row["parent_id"], Value::Null, "a root has no parent: {r_row}");
    assert_eq!(
        r_cells["parent_id"]["state"],
        json!("known"),
        "a root's null is a KNOWN answer, never `?`: {r_cells}"
    );
    assert_eq!(c_row["parent_id"], json!(root), "the child's parent is the root: {c_row}");
    assert_eq!(c_cells["parent_id"]["state"], json!("known"), "{c_cells}");
    assert_eq!(
        g_row["parent_id"],
        json!(child),
        "the grandchild's parent is the CHILD — not the tree's root, not itself: {g_row}"
    );
    assert_eq!(g_cells["parent_id"]["state"], json!("known"), "{g_cells}");

    // ── the same fact `agents/list` answers from ─────────────────────────
    let listed = call(&rt, &m, &root, "agents_list", json!({}));
    for (id, row) in [(&child, &c_row), (&grand, &g_row)] {
        let l = listed["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["thread_id"] == json!(id))
            .unwrap_or_else(|| panic!("agents/list names {id}: {listed}"));
        assert_eq!(
            row["parent_id"], l["parent_id"],
            "the shape and agents/list agree on {id}'s parent"
        );
    }

    rt.shutdown_background();
    drop(m);
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
