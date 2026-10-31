//! # `threads.children` and `threads/non-idle`'s `parent_id`
//!
//! `docs/TUI-SUBAGENTS.md` §4.1–4.2 (M1, M4, M6): the facts a face needs to
//! say "waiting on N subagents" and to draw a child under its parent. Red
//! without the change: the cell and the row field do not exist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shapewire::token::Claims;
use threads::agents;
use threads::config::Config;
use threads::shape::{ConnFrames, Op, Shape};
use threads::shapes::{nonidle, threads as tshape};
use threads::event::Push;
use threads::shape::Derived;
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


/// The newest whole row this connection holds for `key` — export or live.
fn newest_row(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(key))
        .filter_map(|f| match (&f.wire.op, &f.wire.event, &f.wire.row) {
            (Op::Live, Some(Derived::Row { row }), _) => Some(row.clone()),
            (Op::Read, _, Some(row)) => Some(row.clone()),
            _ => None,
        })
        .last()
        .unwrap_or_else(|| panic!("no row for `{key}` among {} frames", frames.len()))
}

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
    fn spawn(&self, caller: &str, rid: &str, extra: Value) -> String {
        let mut args = json!({"task": format!("task-{rid}"), "request_id": rid});
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        call(&self.rt, &self.m, caller, "agents_spawn", args)["thread_id"]
            .as_str()
            .unwrap()
            .to_string()
    }
    fn wait_ended(&self, ids: &[&str]) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ids.iter().all(|id| turn_ended(&self.m, id)) && Instant::now() < deadline {
            self.rt.block_on(async { tokio::time::sleep(Duration::from_millis(25)).await });
        }
        assert!(ids.iter().all(|id| turn_ended(&self.m, id)), "every spawned run ended");
    }
    fn settle(&self, ms: u64) {
        self.rt.block_on(async { tokio::time::sleep(Duration::from_millis(ms)).await });
    }

    /// **WAIT FOR THE FRAME, NOT FOR A NUMBER OF MILLISECONDS.** (R9.F2)
    ///
    /// These tests used to `settle(200..400)` and then read whatever had
    /// arrived. A frame reaches a connection after the shape's debounce
    /// (shapewire `DEBOUNCE_MS`, 100 ms) PLUS however long this fixture's
    /// two-worker runtime waits for a core — and in a loaded `-p threads` run
    /// that second term is unbounded, so the fixed sleep was a bet on the
    /// machine's load (red in the full suite, green alone). This drains `f`
    /// into `acc` until `done(acc)` holds, polling every 25 ms, and gives up at
    /// [`FRAME_DEADLINE`] — the same budget `wait_ended` gives a whole spawned
    /// run. On a deadline it returns what it has, so the caller's own assertion
    /// reports the real shortfall.
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
}

/// How long a frame may take to arrive before a test calls it missing: the
/// 100 ms debounce is the only DECLARED period on the path; the rest is
/// scheduling, so this is sized like `wait_ended`'s 20 s, not like the debounce.
const FRAME_DEADLINE: Duration = Duration::from_secs(20);

/// The newest row for `key`, if any frame carried one yet (non-panicking
/// [`newest_row`], for `until` predicates).
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

fn has_export(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> bool {
    frames
        .iter()
        .any(|f| f.wire.key.as_deref() == Some(key) && matches!(f.wire.op, Op::Read))
}

fn run<'a>(children: &'a Value, id: &str) -> &'a Value {
    children["runs"]
        .as_array()
        .unwrap_or_else(|| panic!("runs is a list: {children}"))
        .iter()
        .find(|r| r["id"] == json!(id))
        .unwrap_or_else(|| panic!("a run for {id}: {children}"))
}

/// **THE BUILDER, ON A LEDGER WITH TWO CHILDREN — ONE REPORTED, ONE OWED.**
/// `outstanding` is the daemon's own wake fold and `runs` carries the label,
/// the wake mode, the request id and the report.
#[test]
fn a_parent_with_two_children_one_reported_names_both_and_owes_one() {
    use threads::ledger::spawn::{self as lspawn, Outcome, SpawnRequest};
    use threads::ledger::{Ledger, ThreadMeta};
    let mut p = Ledger::new(ThreadMeta::root("p", threads::ledger::claude_code::RUNTIME, "2026-10-16T00:00:00Z"));
    let a = SpawnRequest::new("rid-a", "c-a", "carve").detail(json!({"wake": "end", "agent": "core:curator-v5", "name": "carve R7"}));
    let b = SpawnRequest::new("rid-b", "c-b", "scout").detail(json!({"wake": "end"}));
    lspawn::spawn(&mut p, None, |_| None, &a, "2026-10-16T00:00:01Z").unwrap();
    lspawn::spawn(&mut p, None, |_| None, &b, "2026-10-16T00:00:02Z").unwrap();
    lspawn::report(&mut p, "c-a", Outcome::Ok, "done", "2026-10-16T00:00:09Z");
    let row = serde_json::to_value(tshape::children_row(&p)).unwrap();
    assert_eq!(row["outstanding"], json!(["c-b"]), "only the unreported child is owed: {row}");
    assert_eq!(
        run(&row, "c-a"),
        &json!({"id": "c-a", "agent": "core:curator-v5", "name": "carve R7", "wake": "end",
                "started_at": "2026-10-16T00:00:01Z", "request_id": "rid-a",
                "finished": {"outcome": "ok", "at": "2026-10-16T00:00:09Z"}}),
        "{row}"
    );
    assert_eq!(run(&row, "c-b")["finished"], Value::Null, "{row}");
    assert_eq!(run(&row, "c-b")["agent"], Value::Null, "{row}");
}

/// **HYDRATE.** A cold keyed subscribe to a parent whose two children already
/// ran: one `wake: end` (reported, `finished.outcome = ok`), one `wake: never`
/// (not owed, never reported — but its end recorded, R10.H1). A thread that never spawned is a KNOWN empty.
#[test]
fn the_children_cell_hydrates_off_the_parents_ledger() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let root = fx.thread();
    let lone = fx.thread();
    let a = fx.spawn(&root, "a", json!({"name": "carve R7"}));
    let b = fx.spawn(&root, "b", json!({"notify": "hold"}));
    fx.wait_ended(&[&a, &b]);
    fx.settle(300);
    // R10.H1: the never-child's finish is recorded by its own run end — wait
    // for it on the ledger, so the export below is read after it.
    let deadline = Instant::now() + FRAME_DEADLINE;
    while Instant::now() < deadline {
        let l = fx.m.managed().load(&root).unwrap().unwrap();
        let row = serde_json::to_value(tshape::children_row(&l)).unwrap();
        if !run(&row, &b)["finished"].is_null() {
            break;
        }
        fx.settle(25);
    }

    let mut f = fx.m.hub.connect(51);
    for k in [&root, &lone] {
        fx.m.hub.subscribe(tshape::Threads::NAME, Some(k), 51, None).unwrap();
    }
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| has_export(fr, &root) && has_export(fr, &lone));
    let (r_row, r_cells) = export(&frames, &root);
    let (l_row, l_cells) = export(&frames, &lone);
    assert_eq!(l_row["children"], json!({"outstanding": [], "runs": []}), "{l_row}");
    assert_eq!(l_cells["children"]["state"], json!("known"), "no ledger is known-empty: {l_cells}");
    assert_eq!(r_cells["children"]["state"], json!("known"), "{r_cells}");
    let c = &r_row["children"];
    assert_eq!(c["runs"].as_array().map(Vec::len), Some(2), "{c}");
    assert_eq!(run(c, &a)["name"], json!("carve R7"), "{c}");
    assert_eq!(run(c, &a)["wake"], json!("end"), "{c}");
    assert_eq!(run(c, &a)["request_id"], json!("a"), "{c}");
    assert_eq!(run(c, &a)["finished"]["outcome"], json!("ok"), "{c}");
    assert_eq!(run(c, &b)["wake"], json!("never"), "{c}");
    // R10.H1 (TC-13): a wake:never child is not REPORTED (no wake, no words)
    // but its end IS recorded — `finished` null forever was defect D1.
    assert_eq!(run(c, &b)["finished"]["outcome"], json!("ok"), "a wake:never child's end is recorded: {c}");
    assert_eq!(c["outstanding"], json!([]), "{c}");
    fx.rt.shutdown_background();
}

/// **FOLD.** Subscribed BEFORE the spawn: the cell starts known-empty and the
/// `Fact::ThreadChildren` levels emitted at the commit point and at the report
/// move it, with no re-subscribe.
#[test]
fn a_spawn_and_its_report_move_a_held_children_cell() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let root = fx.thread();
    let mut f = fx.m.hub.connect(52);
    fx.m.hub.subscribe(tshape::Threads::NAME, Some(&root), 52, None).unwrap();
    let mut first = Vec::new();
    fx.until(&mut f, &mut first, |fr| try_newest_row(fr, &root).is_some());
    assert_eq!(newest_row(&first, &root)["children"], json!({"outstanding": [], "runs": []}));

    let a = fx.spawn(&root, "a", json!({}));
    fx.wait_ended(&[&a]);
    // The report's level is the LAST one this run produces; wait for a live
    // row that carries it rather than for 400 ms.
    let mut later = Vec::new();
    fx.until(&mut f, &mut later, |fr| {
        fr.iter()
            .filter(|x| x.wire.key.as_deref() == Some(root.as_str()))
            .filter_map(|x| match &x.wire.event {
                Some(Derived::Row { row }) => Some(row["children"].clone()),
                _ => None,
            })
            .last()
            .is_some_and(|c| c["outstanding"] == json!([]) && !c["runs"].as_array().is_none_or(Vec::is_empty))
    });
    let rows: Vec<Value> = later
        .iter()
        .filter(|fr| fr.wire.key.as_deref() == Some(root.as_str()))
        .filter_map(|fr| match &fr.wire.event {
            Some(Derived::Row { row }) => Some(row["children"].clone()),
            _ => None,
        })
        .collect();
    // Row frames coalesce, so the commit point's level (`outstanding: [a]`)
    // may be folded into the report's; the held cell moving off empty with no
    // re-subscribe is the fold.
    assert!(!rows.is_empty(), "a live row frame moved the held cell");
    let c = rows.last().cloned().unwrap_or(Value::Null);
    assert_eq!(run(&c, &a)["finished"]["outcome"], json!("ok"), "the report's level: {c}");
    assert_eq!(c["outstanding"], json!([]), "{c}");
    fx.rt.shutdown_background();
}

/// **`threads/non-idle` ROWS NAME THEIR PARENT** — off the ledgers at hydrate
/// (child `a`, spawned before the subscribe) and off `Fact::ThreadChildren`
/// after it (child `b`). A root reads `null`.
#[test]
fn non_idle_rows_carry_parent_id() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let root = fx.thread();
    let a = fx.spawn(&root, "a", json!({}));
    fx.wait_ended(&[&a]);
    let mut f = fx.m.hub.connect(53);
    fx.m.hub.subscribe(nonidle::NonIdle::NAME, None, 53, None).unwrap();
    // The hydrate must be ON THIS CONNECTION before `b` exists, or `b` would
    // be hydrated too and the fold half of this test would be untested.
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| try_newest_row(fr, nonidle::FLEET).is_some());
    let b = fx.spawn(&root, "b", json!({}));
    fx.wait_ended(&[&b]);
    let st = |id: &str| Push::Status {
        thread_id: id.to_string(),
        state: "parked".to_string(),
        executing: false,
        queued: 0,
        unread: 0,
        detail: None,
        account: Some("aa-owned".to_string()),
    };
    // Wait until the roster names all three with a lineage answer for each
    // child; the assertions below then check WHICH answer.
    //
    // RE-STATED, NOT EMITTED ONCE (R9.F2, found by the after-run): `a` and `b`
    // were spawned `wake: end`, so each report WAKES `root`, and root's own
    // turn then moves its real status (active, then idle — which takes it OFF
    // `threads/non-idle`) after the `parked` level written here. A single emit
    // raced that wake: the old 300 ms read usually beat it, a deadline poll
    // waits it out and loses root (after-run 1: a and b on the roster, root
    // gone, 20 s). The level is re-stated every 150 ms (> the 100 ms debounce)
    // until the frame shows it, so the roster this test reads is the one it
    // asked for, whenever root's wake turns land.
    let named = |fr: &[Arc<threads::shape::ShapeFrame>]| {
        try_newest_row(fr, nonidle::FLEET)
            .and_then(|r| r["threads"].as_array().cloned())
            .is_some_and(|ts| {
                [&root, &a, &b].iter().all(|id| ts.iter().any(|e| e["id"] == json!(id)))
                    && [&a, &b].iter().all(|id| {
                        ts.iter().any(|e| e["id"] == json!(id) && !e["parent_id"].is_null())
                    })
            })
    };
    let deadline = Instant::now() + FRAME_DEADLINE;
    loop {
        for id in [&root, &a, &b] {
            fx.m.hub.emit(st(id));
        }
        fx.settle(150);
        frames.extend(drain(&mut f));
        if named(&frames) || Instant::now() >= deadline {
            break;
        }
    }
    let roster = newest_row(&frames, nonidle::FLEET)["threads"].as_array().cloned().unwrap();
    let pid = |id: &str| {
        roster
            .iter()
            .find(|e| e["id"] == json!(id))
            .unwrap_or_else(|| panic!("{id} on the roster: {roster:?}"))["parent_id"]
            .clone()
    };
    assert_eq!(pid(&a), json!(root), "hydrated lineage: {roster:?}");
    assert_eq!(pid(&b), json!(root), "folded lineage: {roster:?}");
    assert_eq!(pid(&root), Value::Null, "a root: {roster:?}");
    fx.rt.shutdown_background();
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
