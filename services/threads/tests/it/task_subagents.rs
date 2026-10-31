//! # `threads.subagents` — each in-process (Task / Agent tool) subagent's own
//! status, keyed by its launching `tool_use_id` (R9.TS2, TC-10)
//!
//! The operator: *"on each one of the individual agents… It's idle or
//! active… And then their title"* and *"all I care about is where the active
//! agents are."* The face used to relabel the transcript; the daemon now
//! serves the answer.
//!
//! These run the REAL daemon on a REAL turn: the `claude` it spawns is
//! `task_subagents_claude.sh`, which writes the stream-json lines a test
//! scripts — the same `Agent` tool calls, `tool_use_result`s and
//! `system/task_*` lines the CLI writes (shapes from
//! `tests/fixtures/task_lifecycle.jsonl`) — and blocks on files the test
//! creates, so each step is observed with the turn genuinely running. Every
//! assertion reads the `threads` shape off a connection: the hydrate (export)
//! and the live fold (`Op::Live` row frames, no re-subscribe).

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

const FRAME_DEADLINE: Duration = Duration::from_secs(20);

fn script_claude() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/it/task_subagents_claude.sh")
}

/// Pay the fresh file's first exec OUTSIDE every timed window
/// (`support/first_exec.rs` says why), once per process.
fn warm() {
    static DONE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    DONE.get_or_init(|| {
        let st = std::process::Command::new(script_claude())
            .arg("/usage")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status()
            .expect("exec the scripted claude");
        assert!(st.success(), "the scripted claude answers /usage");
    });
}

fn set_env(tmp: &Path) {
    warm();
    std::env::set_var("CAULDRON_HOME", tmp.join("cauldron"));
    std::env::set_var("THREADS_CLAUDE_BIN", script_claude());
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

fn drain(f: &mut ConnFrames) -> Vec<Arc<threads::shape::ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

/// The newest whole row this connection holds for `key` — export or live.
fn newest_row(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Option<Value> {
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

/// The `subagents` cell of the newest row for `key` (`null` if none yet),
/// AS SERVED — `last_activity_at` included.
fn subs_raw(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    newest_row(frames, key).map(|r| r["subagents"].clone()).unwrap_or(Value::Null)
}

/// [`subs_raw`] with each entry's `last_activity_at` checked (an RFC3339
/// instant) and removed, so status / title / ids compare exactly.
fn subs(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    let mut v = subs_raw(frames, key);
    if let Some(m) = v.as_object_mut() {
        for (k, e) in m.iter_mut() {
            let at = e.as_object_mut().and_then(|o| o.remove("last_activity_at"));
            let ok = at
                .as_ref()
                .and_then(Value::as_str)
                .is_some_and(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok());
            assert!(ok, "{k}: last_activity_at is an RFC3339 instant: {at:?}");
        }
    }
    v
}

fn activity(frames: &[Arc<threads::shape::ShapeFrame>], key: &str, tu: &str) -> String {
    subs_raw(frames, key)[tu]["last_activity_at"].as_str().unwrap_or_default().to_string()
}

/// The `subagents` values carried by LIVE row frames only, in order.
#[allow(dead_code)]
fn live_subs(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Vec<Value> {
    frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(key) && matches!(f.wire.op, Op::Live))
        .filter_map(|f| match &f.wire.event {
            Some(Derived::Row { row }) => Some(row["subagents"].clone()),
            _ => None,
        })
        .collect()
}

fn export_cells(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(key) && matches!(f.wire.op, Op::Read))
        .last()
        .and_then(|f| f.wire.cells.clone())
        .unwrap_or(Value::Null)
}

fn row(status: &str, error: Option<&str>, display: &str, task_id: Option<&str>, parent: Option<&str>) -> Value {
    json!({"status": status, "error": error, "display": display, "task_id": task_id,
           "parent_tool_use_id": parent})
}

// ── the stream-json lines, in the CLI's own shapes ─────────────────────────

fn spawn_line(tu: &str, description: &str, background: bool, parent: Option<&str>) -> String {
    let mut v = json!({"type": "assistant", "message": {"id": format!("msg_{tu}"), "model": "claude-fable-5",
        "content": [{"type": "tool_use", "id": tu, "name": "Agent",
                     "input": {"description": description, "subagent_type": "general-purpose",
                               "run_in_background": background, "prompt": "go"}}]}});
    if let Some(p) = parent {
        v["parent_tool_use_id"] = json!(p);
    }
    v.to_string()
}
fn task_started(task_id: &str, tu: &str, description: &str) -> String {
    json!({"type": "system", "subtype": "task_started", "task_id": task_id, "tool_use_id": tu,
           "description": description, "subagent_type": "general-purpose", "task_type": "local_agent",
           "session_id": "fake-sess"})
    .to_string()
}
/// A FOREGROUND call's result: the CLI's `tool_use_result` (sync, completed)
/// beside the `tool_result` block.
fn fg_result(tu: &str, agent: &str, is_error: bool) -> String {
    json!({"type": "user", "tool_use_result": {"isAsync": false, "status": "completed", "agentId": agent},
           "message": {"content": [{"type": "tool_result", "tool_use_id": tu, "content": "done",
                                    "is_error": is_error}]}})
    .to_string()
}
/// A BACKGROUND call's launch ack: `isAsync`, and a `tool_result` that is NOT
/// the subagent's end.
fn bg_ack(tu: &str, agent: &str) -> String {
    json!({"type": "user", "tool_use_result": {"isAsync": true, "status": "async_launched", "agentId": agent,
                                              "outputFile": "/dev/null"},
           "message": {"content": [{"type": "tool_result", "tool_use_id": tu, "content": "launched"}]}})
    .to_string()
}
fn task_updated_failed(task_id: &str) -> String {
    json!({"type": "system", "subtype": "task_updated", "task_id": task_id,
           "patch": {"status": "failed", "end_time": 1785622319569i64, "error": "Agent failed: boom"},
           "session_id": "fake-sess"})
    .to_string()
}
fn task_notification(task_id: &str, tu: &str, status: &str) -> String {
    json!({"type": "system", "subtype": "task_notification", "task_id": task_id, "tool_use_id": tu,
           "status": status, "output_file": "", "summary": "over", "session_id": "fake-sess"})
    .to_string()
}
const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"fake-sess","cwd":"/tmp","model":"claude-fable-5","tools":["Agent"],"mcp_servers":[],"permissionMode":"bypassPermissions","slash_commands":[],"apiKeySource":"none","agents":[]}"#;
const RESULT: &str = r#"{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.01,"num_turns":1,"duration_ms":50,"stop_reason":"end_turn","modelUsage":{},"session_id":"fake-sess"}"#;

struct Fx {
    rt: tokio::runtime::Runtime,
    m: ThreadManager,
    tmp: tempfile::TempDir,
}

fn start() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let m = rt.block_on(async {
        threads::clock::testing::ticking();
        let m = build_manager(config_at(tmp.path())).unwrap();
        m.recover().await;
        m
    });
    Fx { rt, m, tmp }
}

impl Fx {
    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }
    /// Write the script and start a turn that runs it: a child of a fresh
    /// root, `notify: hold` so its end wakes nobody. Returns the child id.
    fn run_script(&self, lines: &[String]) -> String {
        let root = self.root();
        self.spawn_script(&root, "script.txt", "r1", lines)
    }
    fn root(&self) -> String {
        let _ctx = self.rt.enter();
        let settings = json!({"cwd": self.tmp.path().to_string_lossy(), "model": "sonnet",
                              "account": {"named": "aa-owned"}});
        self.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    }
    /// A child of `root` whose turn runs `lines` (`notify: hold`).
    fn spawn_script(&self, root: &str, name: &str, rid: &str, lines: &[String]) -> String {
        let script = self.path(name);
        std::fs::write(&script, lines.join("\n") + "\n").unwrap();
        let root = root.to_string();
        let claims = Claims {
            thread_id: root.clone(),
            run_id: format!("{root}/run-1"),
            root: root.clone(),
            depth: 0,
            turn: "cookie".into(),
        };
        let args = json!({"task": format!("script={}", script.display()), "request_id": rid, "notify": "hold"});
        let out = self
            .rt
            .block_on(agents::call(&self.m, &claims, "agents_spawn", args))
            .unwrap_or_else(|e| panic!("agents_spawn: {} {}", e.code, e.message));
        out["thread_id"].as_str().unwrap().to_string()
    }
    fn go(&self, name: &str) {
        std::fs::write(self.path(name), b"go").unwrap();
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
    /// Accumulate frames until `done` or the deadline; the caller asserts.
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
    fn subscribe(&self, conn: u64, key: &str) -> ConnFrames {
        let f = self.m.hub.connect(conn);
        self.m.hub.subscribe(tshape::Threads::NAME, Some(&key.to_string()), conn, None).unwrap();
        f
    }
    fn executing(&self, id: &str) -> bool {
        self.m.thread_state(id).map(|d| d.executing).unwrap_or(false)
    }
}

fn turn_ended(m: &ThreadManager, id: &str) -> bool {
    m.store.tail_events(id, 1000).unwrap().iter().any(|e| matches!(e.event, ThreadEvent::ResultTurn { .. }))
        && m.store.get_running_turn(id).unwrap().is_none()
}

/// **RUNNING → `active`, FINISHED → `idle`, and the live move on finish.**
///
/// Two foreground Task calls in one running turn; B's result lands, A's does
/// not. Subscribed while the turn is held: A reads `active`, B `idle`, each
/// with its own title and the CLI's task id — on the hydrate (a second
/// connection subscribed afterwards) as on the live fold. Then A's result
/// lands and A moves to `idle` LIVE. Then C is launched and the `claude`
/// process ends with C still running: C moves, live, to `error: interrupted`.
#[test]
fn a_running_task_is_active_a_finished_one_idle_and_the_end_moves_it_live() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let s = |n: &str| fx.path(n).display().to_string();
    let id = fx.run_script(&[
        INIT.to_string(),
        format!("WAIT {}", s("s1")),
        spawn_line("toolu_A", "Read the whitepaper", false, None),
        spawn_line("toolu_B", "Survey the tests", false, None),
        task_started("agA", "toolu_A", "Read the whitepaper"),
        task_started("agB", "toolu_B", "Survey the tests"),
        fg_result("toolu_B", "agB", false),
        format!("WAIT {}", s("s2")),
        fg_result("toolu_A", "agA", false),
        format!("WAIT {}", s("s3")),
        spawn_line("toolu_C", "Never told it ended", false, None),
        format!("WAIT {}", s("s4")),
        RESULT.to_string(),
    ]);
    fx.wait("the scripted turn is executing", |_| fx.executing(&id));
    let mut f = fx.subscribe(71, &id);
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| newest_row(fr, &id).is_some());
    assert_eq!(subs(&frames, &id), json!({}), "nothing launched yet: a KNOWN empty map, not `?`");
    assert_eq!(export_cells(&frames, &id)["subagents"]["state"], json!("known"));

    fx.go("s1");
    let want1 = json!({
        "toolu_A": row("active", None, "Read the whitepaper", Some("agA"), None),
        "toolu_B": row("idle", None, "Survey the tests", Some("agB"), None),
    });
    fx.until(&mut f, &mut frames, |fr| subs(fr, &id) == want1);
    assert_eq!(subs(&frames, &id), want1, "running A is active, finished B is idle (live)");
    assert!(!live_subs(&frames, &id).is_empty(), "it arrived on LIVE row frames");

    // The HYDRATE says the same thing. A second subscribe while the root still
    // held the key would only re-export the live-updated cell (a probe that
    // witnesses itself), so the connection is dropped and the lazy root waited
    // out first (sentinel: the shape stops tracking); the read below is a
    // hydrate out of the daemon's roster. The rest of the test is live on it.
    drop(f);
    fx.m.hub.dropped(71);
    fx.wait("the lazy threads root was dropped", |m| !m.hub.tracked(tshape::Threads::NAME));
    let mut f = fx.subscribe(72, &id);
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| newest_row(fr, &id).is_some());
    assert_eq!(subs(&frames, &id), want1, "the HYDRATE carries the same roster");
    assert_eq!(export_cells(&frames, &id)["subagents"]["state"], json!("known"));

    fx.go("s2");
    let want2 = json!({
        "toolu_A": row("idle", None, "Read the whitepaper", Some("agA"), None),
        "toolu_B": row("idle", None, "Survey the tests", Some("agB"), None),
    });
    fx.until(&mut f, &mut frames, |fr| subs(fr, &id) == want2);
    assert_eq!(subs(&frames, &id), want2, "A's end moved it to idle LIVE, no re-subscribe");

    fx.go("s3");
    fx.until(&mut f, &mut frames, |fr| subs(fr, &id)["toolu_C"]["status"] == json!("active"));
    assert_eq!(subs(&frames, &id)["toolu_C"], row("active", None, "Never told it ended", None, None));
    fx.go("s4");
    fx.wait("the scripted turn ended", |m| turn_ended(m, &id));
    fx.until(&mut f, &mut frames, |fr| subs(fr, &id)["toolu_C"]["status"] == json!("error"));
    assert_eq!(
        subs(&frames, &id)["toolu_C"],
        row("error", Some("interrupted"), "Never told it ended", None, None),
        "the process ended with C still running: interrupted, live"
    );
    fx.rt.shutdown_background();
}

/// **A FAILED TASK IS `error` WITH ITS LABEL.** D runs in the background: its
/// launch ack is a `tool_result` that must NOT end it. Then the CLI patches D
/// `failed`, a foreground call F comes back `is_error`, and a background G is
/// `stopped`: D and F read `error: failed`, G `error: interrupted`.
///
/// The negative (the ack ends nothing) is ordered, not timed: E is launched
/// AFTER the ack on the same stream, the fold is FIFO, so once E is on the
/// shape every effect the ack could have had is on it too — no tick or
/// debounce in the path can hold the ack's effect back past E's.
#[test]
fn a_failed_task_is_error_with_its_label_and_a_launch_ack_is_not_an_end() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let s = |n: &str| fx.path(n).display().to_string();
    let id = fx.run_script(&[
        INIT.to_string(),
        format!("WAIT {}", s("s1")),
        spawn_line("toolu_D", "Background audit", true, None),
        task_started("agD", "toolu_D", "Background audit"),
        bg_ack("toolu_D", "agD"),
        spawn_line("toolu_E", "Ordering sentinel", false, None),
        format!("WAIT {}", s("s2")),
        task_updated_failed("agD"),
        spawn_line("toolu_F", "Doomed call", false, None),
        fg_result("toolu_F", "agF", true),
        spawn_line("toolu_G", "Stopped by the CLI", true, None),
        bg_ack("toolu_G", "agG"),
        task_notification("agG", "toolu_G", "stopped"),
        fg_result("toolu_E", "agE", false),
        format!("WAIT {}", s("s3")),
        RESULT.to_string(),
    ]);
    fx.wait("the scripted turn is executing", |_| fx.executing(&id));
    let mut f = fx.subscribe(73, &id);
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| newest_row(fr, &id).is_some());
    fx.go("s1");
    fx.until(&mut f, &mut frames, |fr| subs(fr, &id).get("toolu_E").is_some());
    assert!(subs(&frames, &id).get("toolu_E").is_some(), "the sentinel landed");
    assert_eq!(
        subs(&frames, &id)["toolu_D"],
        row("active", None, "Background audit", Some("agD"), None),
        "a background launch ack is not the subagent's end"
    );

    fx.go("s2");
    fx.until(&mut f, &mut frames, |fr| subs(fr, &id)["toolu_E"]["status"] == json!("idle"));
    let got = subs(&frames, &id);
    assert_eq!(got["toolu_D"], row("error", Some("failed"), "Background audit", Some("agD"), None), "{got}");
    assert_eq!(got["toolu_F"], row("error", Some("failed"), "Doomed call", Some("agF"), None), "{got}");
    assert_eq!(got["toolu_G"], row("error", Some("interrupted"), "Stopped by the CLI", Some("agG"), None), "{got}");
    // Opposed in the same roster: a clean end is `idle`, no label.
    assert_eq!(got["toolu_E"], row("idle", None, "Ordering sentinel", Some("agE"), None), "{got}");
    fx.go("s3");
    fx.wait("the scripted turn ended", |m| turn_ended(m, &id));
    fx.rt.shutdown_background();
}

/// **KEYED BY THE REAL `tool_use_id`, N = 2 CONCURRENT.** P and Q run at the
/// same time: two keys, two titles, both `active`. Q ends; P stays `active` —
/// an implementation keying on anything shared would have one row, or move
/// both. R is launched BY P (`parent_tool_use_id`): depth 2 names its parent.
#[test]
fn two_concurrent_tasks_are_keyed_by_their_own_tool_use_id() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let s = |n: &str| fx.path(n).display().to_string();
    let id = fx.run_script(&[
        INIT.to_string(),
        format!("WAIT {}", s("s1")),
        spawn_line("toolu_P", "First of two", false, None),
        spawn_line("toolu_Q", "Second of two", false, None),
        spawn_line("toolu_R", "Launched by P", false, Some("toolu_P")),
        format!("WAIT {}", s("s2")),
        fg_result("toolu_Q", "agQ", false),
        format!("WAIT {}", s("s3")),
        RESULT.to_string(),
    ]);
    fx.wait("the scripted turn is executing", |_| fx.executing(&id));
    let mut f = fx.subscribe(74, &id);
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| newest_row(fr, &id).is_some());
    fx.go("s1");
    let want1 = json!({
        "toolu_P": row("active", None, "First of two", None, None),
        "toolu_Q": row("active", None, "Second of two", None, None),
        "toolu_R": row("active", None, "Launched by P", None, Some("toolu_P")),
    });
    fx.until(&mut f, &mut frames, |fr| subs(fr, &id) == want1);
    assert_eq!(subs(&frames, &id), want1, "three keys, each its own call's");

    fx.go("s2");
    let want2 = json!({
        "toolu_P": row("active", None, "First of two", None, None),
        "toolu_Q": row("idle", None, "Second of two", Some("agQ"), None),
        "toolu_R": row("active", None, "Launched by P", None, Some("toolu_P")),
    });
    fx.until(&mut f, &mut frames, |fr| subs(fr, &id) == want2);
    assert_eq!(subs(&frames, &id), want2, "Q's end moved Q and only Q");
    fx.go("s3");
    fx.wait("the scripted turn ended", |m| turn_ended(m, &id));
    fx.rt.shutdown_background();
}

/// A line WRITTEN BY the subagent keyed `parent` (its own text).
fn sub_text(parent: &str, text: &str) -> String {
    json!({"type": "assistant", "parent_tool_use_id": parent,
           "message": {"id": format!("msg_sub_{parent}_{text}"), "model": "claude-fable-5",
                       "content": [{"type": "text", "text": text}]}})
    .to_string()
}

/// The `children.runs[]` entry for `child` in the newest row of `parent`.
fn run_of(frames: &[Arc<threads::shape::ShapeFrame>], parent: &str, child: &str) -> Value {
    newest_row(frames, parent)
        .and_then(|r| r["children"]["runs"].as_array().cloned())
        .and_then(|runs| runs.into_iter().find(|r| r["id"] == json!(child)))
        .unwrap_or(Value::Null)
}
fn run_activity(frames: &[Arc<threads::shape::ShapeFrame>], parent: &str, child: &str) -> String {
    run_of(frames, parent, child)["last_activity_at"].as_str().unwrap_or_default().to_string()
}

/// More than one wall-clock second: `last_activity_at` moves at most once per
/// second (the throttle is the longest period in the path), so every wait
/// between two activities the test compares outlasts it.
const PAST_THE_SECOND: u64 = 1_300;

/// **TC-44 ON THE `subagents` CELL: RECENCY IS LAST ACTIVITY, NOT SPAWN.**
/// X is launched first, Y second — so by spawn time Y is newer. Then, past the
/// one-second throttle, X writes a line of its own. X's `last_activity_at`
/// moves LIVE and is now later than Y's: the activity order is the OPPOSITE of
/// the spawn order, so a spawn-time key cannot pass. Y, which did not act,
/// keeps its stamp (opposed: an implementation stamping every row moves Y too).
#[test]
fn a_subagent_that_acts_is_more_recent_than_a_later_spawned_idle_sibling() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let s = |n: &str| fx.path(n).display().to_string();
    let id = fx.run_script(&[
        INIT.to_string(),
        format!("WAIT {}", s("s1")),
        spawn_line("toolu_X", "Spawned first", false, None),
        format!("WAIT {}", s("s2")),
        spawn_line("toolu_Y", "Spawned second", false, None),
        format!("WAIT {}", s("s3")),
        sub_text("toolu_X", "X is working"),
        format!("WAIT {}", s("s4")),
        RESULT.to_string(),
    ]);
    fx.wait("the scripted turn is executing", |_| fx.executing(&id));
    let mut f = fx.subscribe(75, &id);
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| newest_row(fr, &id).is_some());
    fx.go("s1");
    fx.until(&mut f, &mut frames, |fr| subs_raw(fr, &id).get("toolu_X").is_some());
    fx.settle(PAST_THE_SECOND);
    fx.go("s2");
    fx.until(&mut f, &mut frames, |fr| subs_raw(fr, &id).get("toolu_Y").is_some());
    let (x0, y0) = (activity(&frames, &id, "toolu_X"), activity(&frames, &id, "toolu_Y"));
    // SENTINEL (doctrine 9): spawn order really is X before Y, so the
    // comparison below is not satisfied by spawn time.
    assert!(x0 < y0, "spawn order: X {x0} before Y {y0}");

    fx.settle(PAST_THE_SECOND);
    fx.go("s3");
    fx.until(&mut f, &mut frames, |fr| activity(fr, &id, "toolu_X") > y0);
    let (x1, y1) = (activity(&frames, &id, "toolu_X"), activity(&frames, &id, "toolu_Y"));
    assert!(x1 > y1, "X acted after Y spawned: X {x1} must be later than Y {y1}");
    assert_eq!(y1, y0, "Y did not act: its stamp did not move");
    assert_eq!(subs(&frames, &id)["toolu_X"]["status"], json!("active"));
    fx.go("s4");
    fx.wait("the scripted turn ended", |m| turn_ended(m, &id));
    fx.rt.shutdown_background();
}

/// **TC-44 ON THE MANAGED `children` CELL.** c1 is spawned first, c2 second;
/// both turns are held. Past the throttle, c1 writes a line; c2 does not. On
/// the PARENT's row, c1's run carries the later `last_activity_at` — LIVE, and
/// on a fresh hydrate (a second connection, read off the store). Spawn order
/// is the opposite, so a spawn-time key fails both.
#[test]
fn a_managed_child_that_acts_is_more_recent_than_a_later_spawned_sibling() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let s = |n: &str| fx.path(n).display().to_string();
    let root = fx.root();
    let mut f = fx.subscribe(76, &root);
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| newest_row(fr, &root).is_some());
    let c1 = fx.spawn_script(
        &root,
        "c1.txt",
        "rc1",
        &[INIT.to_string(), format!("WAIT {}", s("c1a")), sub_text_top("c1 acts"), format!("WAIT {}", s("end")), RESULT.to_string()],
    );
    fx.wait("c1 is executing", |_| fx.executing(&c1));
    fx.settle(PAST_THE_SECOND);
    let c2 = fx.spawn_script(
        &root,
        "c2.txt",
        "rc2",
        &[INIT.to_string(), format!("WAIT {}", s("end")), RESULT.to_string()],
    );
    fx.wait("c2 is executing", |_| fx.executing(&c2));
    fx.until(&mut f, &mut frames, |fr| {
        !run_activity(fr, &root, &c1).is_empty() && !run_activity(fr, &root, &c2).is_empty()
    });
    let (a1, a2) = (run_activity(&frames, &root, &c1), run_activity(&frames, &root, &c2));
    assert!(a1 < a2, "SENTINEL: spawn order, c1 {a1} before c2 {a2}");

    fx.settle(PAST_THE_SECOND);
    fx.go("c1a");
    fx.until(&mut f, &mut frames, |fr| run_activity(fr, &root, &c1) > a2);
    let (b1, b2) = (run_activity(&frames, &root, &c1), run_activity(&frames, &root, &c2));
    assert!(b1 > b2, "c1 acted last: LIVE c1 {b1} must be later than c2 {b2}");

    // The HYDRATE, off the store: the connection dropped and the lazy root
    // waited out first (sentinel: the shape stops tracking), so this is a read,
    // not a re-export of the live-updated cell.
    drop(f);
    fx.m.hub.dropped(76);
    fx.wait("the lazy threads root was dropped", |m| !m.hub.tracked(tshape::Threads::NAME));
    let mut f2 = fx.subscribe(77, &root);
    let mut frames2 = Vec::new();
    fx.until(&mut f2, &mut frames2, |fr| !run_activity(fr, &root, &c1).is_empty());
    let (h1, h2) = (run_activity(&frames2, &root, &c1), run_activity(&frames2, &root, &c2));
    assert!(h1 > h2, "c1 acted last: HYDRATED c1 {h1} must be later than c2 {h2}");
    fx.go("end");
    fx.wait("c1 ended", |m| turn_ended(m, &c1));
    fx.wait("c2 ended", |m| turn_ended(m, &c2));
    fx.rt.shutdown_background();
}

/// A top-level assistant line (the managed child's own words).
fn sub_text_top(text: &str) -> String {
    json!({"type": "assistant", "message": {"id": format!("msg_top_{}", text.len()), "model": "claude-fable-5",
                                            "content": [{"type": "text", "text": text}]}})
    .to_string()
}
