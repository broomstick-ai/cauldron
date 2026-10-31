//! # Wake on demand, and at most one awake — R5.S2 (DEC-34)
//!
//! Every guard here is measured against the OPERATING SYSTEM, not against the
//! host's opinion of itself: a spawn is evidenced by the fixture's own pid
//! file (written by the child, not by the host), and "asleep" is `kill -0`
//! on that pid failing. A host that merely forgot a process would still pass a
//! check that asked the host; it fails these.
//!
//! Forcings (each run red, then restored; the red text is in the R5.S2
//! report):
//! * wake-on-demand — `call` refusing instead of waking → red; the opposite
//!   forcing, `new` waking every process server eagerly → red on
//!   "nothing is spawned before the first call".
//! * at-most-one — `wake` displacing the previous server WITHOUT killing it
//!   (`std::mem::forget` of the old slot, which also keeps its stdin open so
//!   the fixture cannot exit by itself) → red; the opposite forcing, `wake`
//!   killing the server it just spawned → red on "the second is awake".

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mcp_host::decl::ToolDecl;
use mcp_host::host::{Bounds, CallError, ToolHost};
use mcp_host::wire;
use serde_json::{json, Value};

fn fixture() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-mcp-tool").display().to_string()
}

fn process_decl(name: &str, tool: &str, pid_dir: &Path) -> ToolDecl {
    process_decl_env(name, tool, pid_dir, &[])
}

/// A process server with extra fixture env (`FAKE_MCP_HANG`, `FAKE_MCP_SLOW`).
fn process_decl_env(name: &str, tool: &str, pid_dir: &Path, env: &[(&str, &str)]) -> ToolDecl {
    let extra: String = env.iter().map(|(k, v)| format!("{k} = \"{v}\"\n")).collect();
    let text = format!(
        "name = \"{name}\"\nresidency = \"process\"\n[process]\ncommand = \"{}\"\n\
         [process.env]\nFAKE_MCP_NAME = \"{name}\"\nFAKE_MCP_PID_DIR = \"{}\"\n{extra}\
         [[tools]]\nname = \"{tool}\"\ndescription = \"test tool {tool}\"\n",
        fixture(),
        pid_dir.display()
    );
    ToolDecl::parse(&text, &PathBuf::from(format!("/t/{name}.toml"))).unwrap()
}

fn in_process_decl() -> ToolDecl {
    let mut d = ToolDecl::parse(
        "name = \"agents\"\nresidency = \"in-process\"\nprovider = \"threads\"\n[[tools]]\nname = \"agents_spawn\"\n",
        &PathBuf::from("/t/agents.toml"),
    )
    .unwrap();
    d.description = "ours".into();
    d
}

/// The pids the fixture recorded for `name`, in spawn order.
fn spawned(pid_dir: &Path, name: &str) -> Vec<u32> {
    std::fs::read_to_string(pid_dir.join(format!("{name}.pid")))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Wait for the fixture's pid file — the child writes it, so it may land a
/// beat after the handshake returns (it is written BEFORE the read loop, so
/// in practice it is there by the time `initialize` was answered).
fn wait_spawned(pid_dir: &Path, name: &str, n: usize) -> Vec<u32> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let got = spawned(pid_dir, name);
        if got.len() >= n || Instant::now() > deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `kill -0` on a pid we just reaped can race the kernel for a moment; wait
/// up to 2 s for a death, so a "still alive" verdict is not a scheduling blip.
fn dead_within(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. WAKE ON DEMAND
// ═════════════════════════════════════════════════════════════════════════════

/// **NOTHING IS SPAWNED AT LOAD; THE FIRST CALL WAKES THE SERVER AND IS SERVED
/// BY IT.** Both halves: a host that wakes eagerly fails the first assertion,
/// a host that never wakes fails the second.
#[test]
fn a_tool_is_woken_on_demand_by_the_first_call_that_names_it() {
    let pids = tempfile::tempdir().unwrap();
    let h = ToolHost::new(vec![process_decl("alpha", "alpha_echo", pids.path())]).unwrap();

    // Outlast the spawn path: a server woken at load would have written its
    // pid file well inside this window.
    std::thread::sleep(Duration::from_millis(300));
    assert!(spawned(pids.path(), "alpha").is_empty(), "a server was spawned before anything asked for it");
    assert_eq!(h.awake(), None);
    assert_eq!(h.wakes(), 0);

    let r = h.call("alpha_echo", json!({"id": 99, "x": 1})).expect("the call is served");
    let got = wait_spawned(pids.path(), "alpha", 1);
    assert_eq!(got.len(), 1, "exactly one spawn for the first call: {got:?}");
    assert_eq!(r["structuredContent"]["served_by"], "alpha", "{r}");
    assert_eq!(r["structuredContent"]["pid"], json!(got[0]), "served by the process the host spawned: {r}");
    assert_eq!(h.awake(), Some(("alpha".to_string(), got[0])));

    // A second call to an awake server does NOT respawn it.
    h.call("alpha_echo", json!({})).unwrap();
    assert_eq!(spawned(pids.path(), "alpha").len(), 1, "an awake server was respawned");
    assert_eq!(h.wakes(), 1);
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. AT MOST ONE AWAKE
// ═════════════════════════════════════════════════════════════════════════════

/// **WAKING A SECOND SERVER PUTS THE FIRST TO SLEEP** — measured by the first
/// server's PROCESS being gone, not by the host saying so. Paired: the second
/// must be alive and serving, so a host that kills everything fails too.
#[test]
fn waking_a_second_server_puts_the_first_to_sleep() {
    let pids = tempfile::tempdir().unwrap();
    let h = ToolHost::new(vec![
        process_decl("alpha", "alpha_echo", pids.path()),
        process_decl("beta", "beta_echo", pids.path()),
    ])
    .unwrap();

    h.call("alpha_echo", json!({})).unwrap();
    let a = wait_spawned(pids.path(), "alpha", 1)[0];
    assert!(alive(a), "alpha is awake after its call");

    let r = h.call("beta_echo", json!({})).unwrap();
    let b = wait_spawned(pids.path(), "beta", 1)[0];
    assert_eq!(r["structuredContent"]["served_by"], "beta");

    assert!(dead_within(a), "alpha (pid {a}) is STILL RUNNING after beta was woken: two tool processes awake");
    assert!(alive(b), "beta (pid {b}) is not running after it was woken");
    assert_eq!(h.awake(), Some(("beta".to_string(), b)));

    // And back: alpha is woken again (a NEW process), beta goes to sleep.
    h.call("alpha_echo", json!({})).unwrap();
    let a2 = wait_spawned(pids.path(), "alpha", 2);
    assert_eq!(a2.len(), 2, "alpha was re-woken as a new process: {a2:?}");
    assert!(dead_within(b), "beta (pid {b}) still running after alpha was re-woken");
    assert!(alive(a2[1]));

    // Dropping the host takes the awake server with it.
    drop(h);
    assert!(dead_within(a2[1]), "the host was dropped and its awake tool process survived it");
}

/// The explicit `wake` reports what it put to sleep, and `sleep` empties the slot.
#[test]
fn wake_reports_the_server_it_displaced_and_sleep_empties_the_slot() {
    let pids = tempfile::tempdir().unwrap();
    let h = ToolHost::new(vec![
        process_decl("alpha", "alpha_echo", pids.path()),
        process_decl("beta", "beta_echo", pids.path()),
    ])
    .unwrap();
    let w1 = h.wake("alpha").unwrap();
    assert!(w1.spawned && w1.slept.is_none());
    let again = h.wake("alpha").unwrap();
    assert!(!again.spawned, "re-waking the awake server must not spawn");
    let w2 = h.wake("beta").unwrap();
    assert_eq!(w2.slept.as_deref(), Some("alpha"));
    assert_eq!(h.sleep().as_deref(), Some("beta"));
    assert!(dead_within(w2.pid));
    assert_eq!(h.awake(), None);
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. RESIDENCY, REFUSALS, AND THE WIRE
// ═════════════════════════════════════════════════════════════════════════════

/// An in-process tool is refused by the host (its provider serves it), and an
/// undeclared tool is refused as unknown — neither spawns anything.
#[test]
fn in_process_and_undeclared_tools_are_refused_without_a_spawn() {
    let pids = tempfile::tempdir().unwrap();
    let h = ToolHost::new(vec![in_process_decl(), process_decl("alpha", "alpha_echo", pids.path())]).unwrap();
    match h.call("agents_spawn", json!({})) {
        Err(CallError::InProcess { provider, .. }) => assert_eq!(provider, "threads"),
        other => panic!("expected in_process, got {other:?}"),
    }
    assert!(matches!(h.call("nope", json!({})), Err(CallError::UnknownTool(_))));
    assert_eq!(h.wakes(), 0);
    assert!(spawned(pids.path(), "alpha").is_empty());
}

/// A server that dies is reaped and woken fresh on the next call.
#[test]
fn a_server_that_dies_is_woken_fresh_on_the_next_call() {
    let pids = tempfile::tempdir().unwrap();
    let h = ToolHost::new(vec![process_decl("alpha", "alpha_echo", pids.path())]).unwrap();
    h.call("alpha_echo", json!({})).unwrap();
    let p = wait_spawned(pids.path(), "alpha", 1)[0];
    std::process::Command::new("kill").args(["-9", &p.to_string()]).status().unwrap();
    // Reaping is lazy: the host's next `awake()` notices and reaps (until
    // then the killed child is a zombie, which `kill -0` still answers for).
    let deadline = Instant::now() + Duration::from_secs(2);
    while h.awake().is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(h.awake(), None);
    assert!(dead_within(p));
    let r = h.call("alpha_echo", json!({})).unwrap();
    let all = wait_spawned(pids.path(), "alpha", 2);
    assert_eq!(all.len(), 2);
    assert_eq!(r["structuredContent"]["pid"], json!(all[1]));
}

/// The seam words: `MCP_TOOLS` answers from the declarations WITHOUT waking,
/// `MCP_CALL` serves through the host, and a foreign word is not ours.
#[test]
fn the_seam_words_list_without_waking_and_call_through_the_host() {
    let pids = tempfile::tempdir().unwrap();
    let host = ToolHost::new(vec![in_process_decl(), process_decl("alpha", "alpha_echo", pids.path())]).unwrap();
    let tools = wire::answer(&host, "MCP_TOOLS", "").unwrap();
    let names: Vec<&str> = tools["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["alpha_echo"], "{tools}");
    assert_eq!(tools["tools"][0]["inputSchema"], json!({"type": "object"}));
    assert_eq!(tools["in_process"][0]["provider"], "threads");
    assert!(spawned(pids.path(), "alpha").is_empty(), "MCP_TOOLS woke a server");

    let ok = wire::answer(&host, "MCP_CALL", r#"{"tool":"alpha_echo","arguments":{},"thread_id":"t1"}"#).unwrap();
    assert_eq!(ok["ok"]["structuredContent"]["served_by"], "alpha", "{ok}");
    let st = wire::answer(&host, "MCP_STATUS", "").unwrap();
    assert_eq!(st["awake"]["server"], "alpha", "{st}");

    let r: Value = wire::answer(&host, "MCP_CALL", r#"{"tool":"agents_spawn"}"#).unwrap();
    assert_eq!(r["refused"]["code"], "in_process");
    let r = wire::answer(&host, "MCP_CALL", "not json").unwrap();
    assert_eq!(r["refused"]["code"], "malformed");
    assert_eq!(wire::answer(&host, "MCP_SLEEP", "").unwrap()["slept"], "alpha");
    assert!(wire::answer(&host, "STATUS", "").is_none());
}

// ═════════════════════════════════════════════════════════════════════════════
// 4. ONE STUCK TOOL CANNOT STALL THE REST (R5.S2b)
// ═════════════════════════════════════════════════════════════════════════════
//
// Every request here goes through `wire::answer`, the path the daemon's seam
// connections take, so the forcing "revert to the single lock" (one global
// mutex held for the whole of `answer`, which is what R5.S2 shipped) is a
// forcing of the code under test and not of a test-only path.
//
// The wedged server's own bounds (handshake, call) are 60 s, far past every
// watchdog here: a test that went green by the wedged server TIMING OUT would
// need a minute, and the watchdogs are seconds. The bound these tests outlast
// is the host's PREEMPT bound, shrunk to 400 ms (doctrine 4) and named in each
// test's name.
//
// Forcings (red text in the R5.S2b report):
// * single lock (a global mutex around `wire::answer`) → the hung-on-wake and
//   hung-on-call tests time out on LISTING (watchdog 1 s).
// * preempt never → listing stays green (it takes no lock), the other tool's
//   CALL times out (watchdog 3 s).
// * preempt immediately (no wait for a busy server) → the healthy-busy test
//   goes red: the slow tool's in-flight call is killed.
// * the opposite of all three: the fast tool alone is served well inside the
//   same 3 s watchdog, so the watchdog is not what makes the reds red.

const PREEMPT: Duration = Duration::from_millis(400);
const LIST_WATCHDOG: Duration = Duration::from_secs(1);
const CALL_WATCHDOG: Duration = Duration::from_secs(3);

fn wedge_bounds() -> Bounds {
    Bounds { handshake: Duration::from_secs(60), call: Duration::from_secs(60), preempt: PREEMPT }
}

/// Run `word rest` on the wire in its own thread; the answer and how long it
/// took, or `Err(limit)` if it did not answer within `limit` (the thread is
/// left behind — it is stuck by definition).
fn ask_within(h: &Arc<ToolHost>, word: &'static str, rest: &str, limit: Duration) -> Result<(Value, Duration), Duration> {
    let (tx, rx) = mpsc::channel();
    let (h, rest) = (h.clone(), rest.to_string());
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let v = wire::answer(&h, word, &rest).expect("an MCP word");
        let _ = tx.send((v, t0.elapsed()));
    });
    rx.recv_timeout(limit).map_err(|_| limit)
}

/// Start `MCP_CALL tool` in the background; its answer arrives on the channel.
fn call_in_background(h: &Arc<ToolHost>, tool: &str) -> mpsc::Receiver<Value> {
    let (tx, rx) = mpsc::channel();
    let (h, line) = (h.clone(), format!(r#"{{"tool":"{tool}","arguments":{{}}}}"#));
    std::thread::spawn(move || {
        let _ = tx.send(wire::answer(&h, "MCP_CALL", &line).expect("an MCP word"));
    });
    rx
}

/// The shared body of the two wedge tests: `stuck` is awake and wedged on
/// `hang`; listing and `fast`'s call must complete, the stuck call must come
/// back `tool_failed`, and at most one process is left awake.
fn a_wedged_server_does_not_stall_the_rest(hang: &str) {
    let pids = tempfile::tempdir().unwrap();
    let h = Arc::new(
        ToolHost::with_bounds(
            vec![
                process_decl_env("stuck", "stuck_echo", pids.path(), &[("FAKE_MCP_HANG", hang)]),
                process_decl("fast", "fast_echo", pids.path()),
            ],
            wedge_bounds(),
        )
        .unwrap(),
    );
    let stuck_answer = call_in_background(&h, "stuck_echo");
    let stuck_pid = wait_spawned(pids.path(), "stuck", 1)[0];
    // It is awake and inside its wedge: the pid file is written before the
    // read loop, and give the request a beat to be written to it.
    std::thread::sleep(Duration::from_millis(100));
    // LISTING: no lock, answered at once.
    let (tools, took) = ask_within(&h, "MCP_TOOLS", "", LIST_WATCHDOG)
        .unwrap_or_else(|l| panic!("MCP_TOOLS did not answer within {l:?} while `stuck` is wedged on `{hang}`"));
    let names: Vec<&str> = tools["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["fast_echo", "stuck_echo"], "{tools}");
    assert!(took < PREEMPT, "listing waited {took:?}, i.e. on the wedged tool");

    // The precondition, read AFTER listing so a stalled host reds on the
    // listing: the wedged server is the awake one, and it is busy.
    let st = ask_within(&h, "MCP_STATUS", "", LIST_WATCHDOG).expect("MCP_STATUS answered while a tool is wedged").0;
    assert_eq!(st["awake"]["server"], "stuck", "the wedged server is the awake one: {st}");
    assert!(st["awake"]["busy"].as_u64().unwrap() >= 1, "and it is busy: {st}");

    // ANOTHER TOOL'S CALL: waits out the preempt bound, then is served.
    let (r, took) = ask_within(&h, "MCP_CALL", r#"{"tool":"fast_echo","arguments":{}}"#, CALL_WATCHDOG)
        .unwrap_or_else(|l| panic!("fast_echo's call did not complete within {l:?} while `stuck` is wedged on `{hang}`"));
    assert_eq!(r["ok"]["structuredContent"]["served_by"], "fast", "{r}");
    assert!(took >= PREEMPT, "fast was served in {took:?}, before the {PREEMPT:?} preempt bound: a busy server was killed without its grace");

    // THE WEDGED CALL: an error for that tool only, promptly.
    let s = stuck_answer.recv_timeout(Duration::from_secs(2)).expect("the wedged call was never answered");
    assert_eq!(s["refused"]["code"], "tool_failed", "{s}");
    assert!(s["refused"]["message"].as_str().unwrap().contains("put to sleep"), "{s}");

    // AT MOST ONE AWAKE, measured on the OS.
    assert!(dead_within(stuck_pid), "stuck (pid {stuck_pid}) still running after fast was woken: two awake");
    let fast_pid = wait_spawned(pids.path(), "fast", 1)[0];
    assert!(alive(fast_pid));
    assert_eq!(h.awake(), Some(("fast".to_string(), fast_pid)));
}

/// A server that never answers `initialize`.
#[test]
fn a_tool_hung_on_wake_stalls_neither_listing_nor_another_tools_call_past_the_400ms_preempt_bound() {
    a_wedged_server_does_not_stall_the_rest("initialize");
}

/// A server that answers `initialize` and never answers `tools/call`.
#[test]
fn a_tool_hung_on_call_stalls_neither_listing_nor_another_tools_call_past_the_400ms_preempt_bound() {
    a_wedged_server_does_not_stall_the_rest("tools/call");
}

/// **THE OPPOSITE FORCING**: the fast tool alone, through the same wire and
/// watchdogs. Green under every forcing above, so their reds come from the
/// wedged server, not from a watchdog too tight for a bash fixture's spawn.
#[test]
fn the_fast_tool_alone_lists_and_is_served_inside_the_same_3s_watchdog() {
    let pids = tempfile::tempdir().unwrap();
    let h = Arc::new(ToolHost::with_bounds(vec![process_decl("fast", "fast_echo", pids.path())], wedge_bounds()).unwrap());
    ask_within(&h, "MCP_TOOLS", "", LIST_WATCHDOG).expect("listing");
    let (r, took) = ask_within(&h, "MCP_CALL", r#"{"tool":"fast_echo","arguments":{}}"#, CALL_WATCHDOG).expect("the call");
    assert_eq!(r["ok"]["structuredContent"]["served_by"], "fast", "{r}");
    assert!(took < PREEMPT, "nothing to wait out, yet the call took {took:?}");
}

/// **A HEALTHY BUSY TOOL KEEPS ITS GRACE.** `slow` takes 1 s per call, well
/// inside a 3 s preempt bound: a call for `fast` arriving mid-call waits for
/// it rather than killing it, and both are served. A host that preempts
/// without waiting fails here (slow's call comes back `tool_failed`).
#[test]
fn a_busy_tool_is_not_put_to_sleep_before_the_3s_preempt_bound() {
    let pids = tempfile::tempdir().unwrap();
    let bounds = Bounds { preempt: Duration::from_secs(3), ..wedge_bounds() };
    let h = Arc::new(
        ToolHost::with_bounds(
            vec![
                process_decl_env("slow", "slow_echo", pids.path(), &[("FAKE_MCP_SLOW", "1")]),
                process_decl("fast", "fast_echo", pids.path()),
            ],
            bounds,
        )
        .unwrap(),
    );
    let slow_answer = call_in_background(&h, "slow_echo");
    wait_spawned(pids.path(), "slow", 1);
    std::thread::sleep(Duration::from_millis(200));
    let (r, took) = ask_within(&h, "MCP_CALL", r#"{"tool":"fast_echo","arguments":{}}"#, Duration::from_secs(6)).expect("fast's call");
    let s = slow_answer.recv_timeout(Duration::from_secs(2)).expect("slow's call");
    assert_eq!(s["ok"]["structuredContent"]["served_by"], "slow", "the busy tool was preempted inside its grace: {s}");
    assert_eq!(r["ok"]["structuredContent"]["served_by"], "fast", "{r}");
    assert!(took < Duration::from_secs(3), "fast waited {took:?}: the full bound, though slow went idle at ~1 s");
}

/// **TWO CALLERS QUEUED BEHIND A STUCK TOOL** (R5.S2b inspect finding 1). `bee`
/// and `cee` both wait out `stuck`'s 400 ms preempt bound. The first to act
/// preempts `stuck` and wakes its own server; the second must then give THAT
/// fresh spawn its full grace, because the preempt clock restarts when the
/// occupant changes. Both healthy calls are served. When the clock was kept
/// per waiter, the second waiter's already-expired clock killed the fresh
/// spawn mid-handshake, and a healthy tool answered `tool_failed` (3/3).
#[test]
fn two_callers_queued_behind_a_stuck_tool_are_both_served_after_the_400ms_preempt_bound() {
    let pids = tempfile::tempdir().unwrap();
    let h = Arc::new(
        ToolHost::with_bounds(
            vec![
                process_decl_env("stuck", "stuck_echo", pids.path(), &[("FAKE_MCP_HANG", "tools/call")]),
                process_decl("bee", "bee_echo", pids.path()),
                process_decl("cee", "cee_echo", pids.path()),
            ],
            wedge_bounds(),
        )
        .unwrap(),
    );
    let stuck_answer = call_in_background(&h, "stuck_echo");
    let stuck_pid = wait_spawned(pids.path(), "stuck", 1)[0];
    std::thread::sleep(Duration::from_millis(100));
    // Two waiters, queued at the same moment behind the stuck occupant.
    let bee = call_in_background(&h, "bee_echo");
    let cee = call_in_background(&h, "cee_echo");
    let b = bee.recv_timeout(CALL_WATCHDOG).expect("bee's call did not complete within 3s");
    let c = cee.recv_timeout(CALL_WATCHDOG).expect("cee's call did not complete within 3s");
    assert_eq!(b["ok"]["structuredContent"]["served_by"], "bee", "a HEALTHY tool failed because another was stuck: {b}");
    assert_eq!(c["ok"]["structuredContent"]["served_by"], "cee", "a HEALTHY tool failed because another was stuck: {c}");
    let s = stuck_answer.recv_timeout(Duration::from_secs(2)).expect("the wedged call was never answered");
    assert_eq!(s["refused"]["code"], "tool_failed", "{s}");
    assert!(dead_within(stuck_pid), "stuck (pid {stuck_pid}) still running");
    // At most one awake, measured on the OS: of bee and cee, only the occupant lives.
    let (name, pid) = h.awake().expect("one of bee/cee is awake");
    let other = if name == "bee" { "cee" } else { "bee" };
    for p in spawned(pids.path(), other) {
        assert!(dead_within(p), "{other} (pid {p}) still running while {name} (pid {pid}) is awake");
    }
}
