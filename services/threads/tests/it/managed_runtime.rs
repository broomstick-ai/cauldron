//! **THE MANAGED SUBAGENT RUNTIME, ACROSS A DAEMON DEATH** (R4.S2).
//!
//! The release sentence: *"Kill the daemon mid-spawn, restart it, and the child
//! thread resumes from its ledger; no `Liveness::busy()` poll exists."*
//!
//! Read precisely (R4.S2b, the R4.S2 inspector's F3): the subagent WAIT poll is
//! deleted and **no `busy()` read decides a wait**. `busy()` is still read on
//! timers to REPORT — the heartbeat frame, the gauge's busy-rate ticker — and
//! `no_busy_read_decides_a_wait_even_through_a_call` checks the precise
//! sentence through calls, which the lexical `busy_polls` scan cannot.
//!
//! ## How a daemon is killed here, and why that is a kill
//!
//! Each daemon life is its own tokio runtime, and the kill is
//! `Runtime::shutdown_background()`: every task the daemon owns — turn
//! tailers, the placement loop, heartbeats — stops at its next await, exactly
//! as a `SIGKILL` stops them, and nothing the daemon would have done next
//! happens. What survives is what survives a real kill: the data dir on disk
//! and any `claude` process (each in its own process group, writing to its own
//! durable output file). The next life is a fresh `build_manager` over the same
//! data dir, then `recover()` — the boot path `run_daemon` takes.
//!
//! "Mid-spawn" is a halt point inside `agents/spawn` (`managed::Halt`): the
//! spawn stops right after one durable write, which leaves on disk exactly what
//! a kill at that instant leaves, because every write before it was `fsync`ed
//! and nothing after it ran. A probe must not witness itself: the test reads
//! the child back out of the STORE the second daemon wrote (its `threads` row,
//! its `user_message`, its `result_turn`) — never out of the ledger the halted
//! spawn wrote, which is the input, not the evidence.
//!
//! No real claude is spawned: `THREADS_CLAUDE_BIN` is `tests/fixtures/fake-claude`.
//! No socket is opened: the façade is called in-process (`agents::call`), the
//! same function the `/mcp` door calls after it has verified the token.

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
use threads::{build_manager, ThreadEvent, ThreadManager};

/// The env is process-wide and these tests each point it at their own tmp dir,
/// so they run one at a time.
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

/// One daemon life: its own runtime, a manager over `dir`, `recover()` run.
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

    /// **THE KILL.** Every task stops; nothing after this instant runs.
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

    /// Wait (in this life's runtime) until `f` holds, or `secs` pass.
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
}

fn events(m: &ThreadManager, id: &str) -> Vec<ThreadEvent> {
    m.store.tail_events(id, 1000).unwrap().into_iter().map(|e| e.event).collect()
}

fn user_texts(m: &ThreadManager, id: &str) -> Vec<(String, Value)> {
    events(m, id)
        .into_iter()
        .filter_map(|e| match e {
            ThreadEvent::UserMessage { text, metadata, .. } => Some((text, metadata)),
            _ => None,
        })
        .collect()
}

fn turn_ended(m: &ThreadManager, id: &str) -> bool {
    events(m, id).iter().any(|e| matches!(e, ThreadEvent::ResultTurn { .. }))
        && m.store.get_running_turn(id).unwrap().is_none()
}

/// The managed children the parent's ledger names, in spawn order — read off
/// the ledger FILE, the way recovery reads it.
fn children_named(dir: &Path, parent: &str) -> Vec<String> {
    let p = dir.join("ledgers").join(format!("{parent}.jsonl"));
    let Ok(l) = threads::ledger::Ledger::read_jsonl(&p) else { return Vec::new() };
    l.events()
        .iter()
        .filter(|e| e.kind == threads::ledger::Kind::Child && e.body["phase"] == "started")
        .map(|e| e.body["child_id"].as_str().unwrap().to_string())
        .collect()
}

fn wakes(m: &ThreadManager, parent: &str) -> Vec<String> {
    user_texts(m, parent)
        .into_iter()
        .filter(|(_, md)| md["origin"] == "agents")
        .map(|(t, _)| t)
        .collect()
}

/// **KILL THE DAEMON MID-SPAWN, RESTART IT, AND THE CHILD RESUMES FROM ITS
/// LEDGER** — at both kill points past the commit, and the parent is woken by
/// the child's report afterwards.
///
/// * After the COMMIT (the parent's ledger names the child; no thread row, no
///   turn): the first life is shown to have created nothing in the store, so
///   everything the second life shows is recovery's work.
/// * After the THREAD ROW (the child exists; its turn was never queued): the
///   turn is queued by recovery from the child's ledger, exactly once.
///
/// **THE MUTATION.** Delete `crate::managed::recover(self);` from
/// `ThreadManager::recover`.
///
/// Forced red: see the report — the child never exists / never runs.
#[test]
fn a_daemon_killed_mid_spawn_resumes_the_child_from_its_ledger_after_restart() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for halt in [Halt::AfterCommit, Halt::AfterThreadRow] {
        let tmp = tempfile::tempdir().unwrap();
        set_env(tmp.path());
        let a = Life::boot(tmp.path());
        let parent = a.thread(tmp.path());
        a.m.managed().halt_after(Some(halt));
        let err = a
            .call(&parent, "agents_spawn", json!({"task": "the child's task", "request_id": "req-1"}))
            .expect_err("the spawn stops at the kill point");
        assert!(err.message.contains("halted"), "{halt:?}: {}", err.message);
        let named = children_named(tmp.path(), &parent);
        assert_eq!(named.len(), 1, "{halt:?}: the commit point was reached: the parent's ledger names the child");
        let child = named[0].clone();
        // What the first life left in the STORE — the sentinel that the second
        // life's evidence is recovery's, not the first life's.
        match halt {
            Halt::AfterCommit => assert!(
                a.m.store.get_thread(&child).unwrap().is_none(),
                "killed at the commit point: no thread row yet"
            ),
            _ => {
                assert!(a.m.store.get_thread(&child).unwrap().is_some(), "killed after the row");
                assert!(user_texts(&a.m, &child).is_empty(), "…and before the turn was queued");
            }
        }
        a.kill();

        let b = Life::boot(tmp.path());
        assert!(
            b.until(15, |m| turn_ended(m, &child)),
            "{halt:?}: THE CHILD RESUMED FROM ITS LEDGER — its thread exists and its turn ran \
             after the restart. events: {:?}",
            events(&b.m, &child)
        );
        let sent = user_texts(&b.m, &child);
        assert_eq!(sent.len(), 1, "{halt:?}: the task was queued exactly once: {sent:?}");
        assert_eq!(sent[0].0, "the child's task", "the text came back out of the child's ledger");
        assert_eq!(sent[0].1["parent"], json!(parent));
        // And the wait closes: the child's report wakes the parent, once.
        assert!(
            b.until(15, |m| !wakes(m, &parent).is_empty()),
            "{halt:?}: the parent is woken by its child's report"
        );
        let w = wakes(&b.m, &parent);
        assert_eq!(w.len(), 1, "{halt:?}: woken once: {w:?}");
        assert!(w[0].contains(&child) && w[0].contains("Hello from fake claude."), "{}", w[0]);
        // A retried spawn after the restart is the same child, not a second.
        let again = b
            .call(&parent, "agents_spawn", json!({"task": "the child's task", "request_id": "req-1"}))
            .unwrap();
        assert_eq!(again["thread_id"], json!(child));
        assert_eq!(again["changed"], json!(false));
        assert_eq!(children_named(tmp.path(), &parent).len(), 1);
        assert_eq!(
            user_texts(&b.m, &child).len(),
            1,
            "{halt:?}: the replay re-ran materialize and queued nothing twice"
        );
        b.kill();
    }
}

/// **A KILL BEFORE THE COMMIT POINT IS A SPAWN THAT DID NOT HAPPEN.** The child's
/// ledger was written; the parent's never named it. Recovery must NOT resurrect
/// it (the opposed case to the test above: recovery is keyed on the parent's
/// commit, not on a child file existing), and a retry with the same
/// `request_id` then creates it, once.
#[test]
fn a_kill_before_the_commit_point_leaves_no_child_and_a_retry_makes_one() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    a.m.managed().halt_after(Some(Halt::AfterChildLedger));
    a.call(&parent, "agents_spawn", json!({"task": "t", "request_id": "req-1"}))
        .expect_err("halted");
    let orphans: Vec<_> = std::fs::read_dir(tmp.path().join("ledgers"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(orphans.len(), 1, "the child's ledger alone was written: {orphans:?}");
    a.kill();

    let b = Life::boot(tmp.path());
    let threads_after = b.m.store.list_threads(100).unwrap().len();
    assert_eq!(threads_after, 1, "recovery made no child out of an uncommitted spawn");
    let ok = b
        .call(&parent, "agents_spawn", json!({"task": "t", "request_id": "req-1"}))
        .unwrap();
    let child = ok["thread_id"].as_str().unwrap().to_string();
    assert_eq!(ok["changed"], json!(true));
    assert!(b.until(15, |m| turn_ended(m, &child)), "the retried spawn runs");
    assert_eq!(b.m.store.list_threads(100).unwrap().len(), 2);
    b.kill();
}

/// **A CHILD WHOSE TURN OUTLIVES THE DAEMON STILL REPORTS.** The daemon dies
/// while the child's `claude` is running; the next life re-adopts the turn
/// (`running_turns`), and when it ends the report reaches the parent from the
/// re-adopted path. Nothing in memory crossed the restart.
#[test]
fn a_child_running_across_a_restart_reports_to_its_parent() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let release = tmp.path().join("release");
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let ok = a
        .call(
            &parent,
            "agents_spawn",
            json!({"task": format!("holdturn release={}", release.display()), "request_id": "r"}),
        )
        .unwrap();
    let child = ok["thread_id"].as_str().unwrap().to_string();
    assert!(
        a.until(10, |m| m.store.get_running_turn(&child).unwrap().is_some()),
        "the child's turn is running"
    );
    a.kill();

    let b = Life::boot(tmp.path());
    assert!(wakes(&b.m, &parent).is_empty(), "not woken while the child still runs");
    std::fs::write(&release, b"go").unwrap();
    assert!(
        b.until(15, |m| wakes(m, &parent).len() == 1),
        "the re-adopted child's report wakes the parent. child events: {:?}",
        events(&b.m, &child)
    );
    b.kill();
}

/// **A PARENT FANNED OUT OVER THREE CHILDREN IS WOKEN ONCE, BY THE LAST.** At
/// the daemon level: three real child threads, three turns, ONE wake turn on
/// the parent carrying all three reports. And the idempotence pair: the same
/// `request_id` is the same child (`changed: false`); a different one is a new
/// child.
#[test]
fn three_children_wake_their_parent_once_and_a_request_id_is_one_child() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let release = tmp.path().join("release");
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let task = format!("holdturn release={}", release.display());
    let mut kids = Vec::new();
    for r in ["r1", "r2", "r3"] {
        let ok = a.call(&parent, "agents_spawn", json!({"task": task, "request_id": r})).unwrap();
        assert_eq!(ok["changed"], json!(true), "{ok}");
        kids.push(ok["thread_id"].as_str().unwrap().to_string());
    }
    let replay = a.call(&parent, "agents_spawn", json!({"task": task, "request_id": "r2"})).unwrap();
    assert_eq!(replay["thread_id"], json!(kids[1]), "the same request id is the same child");
    assert_eq!(replay["changed"], json!(false));
    let mut uniq = kids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 3, "three request ids are three children");
    assert_eq!(a.m.store.list_threads(100).unwrap().len(), 4);

    std::fs::write(&release, b"go").unwrap();
    assert!(a.until(20, |m| kids.iter().all(|k| turn_ended(m, k))), "all three ran");
    assert!(a.until(10, |m| !wakes(m, &parent).is_empty()), "the parent was woken");
    // Outlast the longest period in the wake path — the 25 ms tail poll and the
    // turn epilogue — many times over before asserting there is no second wake.
    a.rt.block_on(async { tokio::time::sleep(Duration::from_millis(1500)).await });
    let w = wakes(&a.m, &parent);
    assert_eq!(w.len(), 1, "woken ONCE, by the last child: {w:?}");
    for k in &kids {
        assert!(w[0].contains(k.as_str()), "the one wake carries every report: {}", w[0]);
    }
    a.kill();
}

/// **WHAT THE SPAWN REFUSES, IT REFUSES TYPED AND BEFORE ANYTHING IS WRITTEN.**
/// A parameter the daemon cannot honour, and a depth past the cap.
#[test]
fn a_spawn_the_daemon_cannot_honour_is_refused_typed_and_writes_nothing() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let e = a
        .call(&parent, "agents_spawn", json!({"task": "t", "max_thinking_tokens": 5}))
        .unwrap_err();
    assert_eq!(e.code, codes::NOT_AVAILABLE_YET, "{}", e.message);
    // `agent` went live at R4.S8 (tests/spawn_params.rs) and `json_schema` at
    // R11.J3 (tests/spawn_params.rs section 5); `max_thinking_tokens` is still
    // a parameter the daemon cannot honour, so it carries this probe.
    assert!(e.message.contains("`max_thinking_tokens`"), "{}", e.message);
    assert!(!tmp.path().join("ledgers").exists() || children_named(tmp.path(), &parent).is_empty());
    assert_eq!(a.m.store.list_threads(100).unwrap().len(), 1);

    // Depth: root 0 → 1 → 2 → 3 allowed; a spawn from depth 3 is refused.
    let mut cur = parent.clone();
    for d in 1..=3 {
        let ok = a.call(&cur, "agents_spawn", json!({"task": "t", "request_id": format!("d{d}")})).unwrap();
        cur = ok["thread_id"].as_str().unwrap().to_string();
    }
    let e = a.call(&cur, "agents_spawn", json!({"task": "t", "request_id": "d4"})).unwrap_err();
    assert_eq!(e.code, codes::FORBIDDEN, "{}", e.message);
    assert!(e.message.contains("depth 3"), "{}", e.message);
    a.kill();
}

/// Every `loop`/`while` body in `src` that both SLEEPS and reads `.busy()` —
/// the shape of a `Liveness::busy()` poll. Whole-line comments are dropped
/// first so a doc comment that names the old poll cannot satisfy or fail it.
fn busy_polls(src: &str) -> Vec<String> {
    let code: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(|l| format!("{l}\n"))
        .collect();
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let rest = &code[i..];
        let at_word = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if at_word && (rest.starts_with("loop {") || rest.starts_with("while ")) {
            let Some(open) = rest.find('{') else { break };
            let mut depth = 0usize;
            let mut end = None;
            for (j, c) in rest[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(open + j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(end) = end {
                let body = &rest[..=end];
                if body.contains("sleep(") && body.contains(".busy()") {
                    out.push(body.lines().next().unwrap_or("").trim().to_string());
                }
            }
        }
        i += rest.chars().next().map(char::len_utf8).unwrap_or(1);
    }
    out
}

/// **NO `Liveness::busy()` POLL EXISTS.** The poller (`poll_subagents_until_idle`)
/// and the mtime sweep it drove (`sweep_subagents`) are deleted; the heartbeat
/// no longer loops on `busy()`; the turn-end wait is the ledger fold.
///
/// The scan is shown able to find a poll first — a planted one, in the exact
/// shape the deleted poller had — so a clean result is a measurement.
///
/// **THE MUTATION.** Restore `poll_subagents_until_idle` (or put
/// `let _ = st.live.lock().unwrap().busy();` back into the heartbeat loop).
#[test]
fn no_liveness_busy_poll_remains_in_the_runtime() {
    let planted = "async fn p(&self) {\n    loop {\n        tokio::time::sleep(self.tunables.heartbeat).await;\n        let busy = state.live.lock().unwrap().busy();\n        if !busy { break; }\n    }\n}\n";
    assert_eq!(busy_polls(planted), vec!["loop {".to_string()], "the scan finds a planted poll");
    let commented = "// loop { sleep(x); s.busy() }\n";
    assert!(busy_polls(commented).is_empty(), "…and is not satisfied by a comment naming one");
    // EVERY source file of the crate (R4.S5, the R-5 inspector's F2): the
    // managed path lives in `managed.rs` and `networks.rs`, and a scan of
    // `runtime.rs` alone let a poll planted there pass.
    let files = src_files();
    let mut polls = Vec::new();
    for (name, src) in &files {
        for p in busy_polls(src) {
            polls.push(format!("{name}: {p}"));
        }
        for gone in ["fn poll_subagents_until_idle", "fn sweep_subagents", "subagent_idle"] {
            let hits: Vec<&str> = src
                .lines()
                .filter(|l| !l.trim_start().starts_with("//") && l.contains(gone))
                .collect();
            assert!(hits.is_empty(), "`{gone}` is gone from {name}: {hits:?}");
        }
    }
    assert_eq!(polls, Vec::<String>::new(), "no loop in src/ sleeps and reads busy()");
}

/// **EVERY `.rs` FILE UNDER `src/`**, recursively, as `(path relative to
/// src/, text)`, sorted. Asserted able to see what it must: the runtime (by
/// size), the managed path (`managed.rs`, `networks.rs`) and a nested module,
/// so a scan over this set is a scan over the crate and not over whatever a
/// wrong path happened to find.
fn src_files() -> Vec<(String, String)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, root, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
                out.push((rel, std::fs::read_to_string(&p).unwrap()));
            }
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort();
    let get = |n: &str| out.iter().find(|(k, _)| k == n).map(|(_, v)| v.len()).unwrap_or(0);
    assert!(get("runtime.rs") > 100_000, "the real runtime.rs was read");
    assert!(get("managed.rs") > 10_000, "managed.rs was read: {:?}", out.iter().map(|x| &x.0).collect::<Vec<_>>());
    assert!(get("networks.rs") > 10_000, "networks.rs was read");
    assert!(out.iter().any(|(k, _)| k.contains('/')), "nested modules were walked");
    out
}

// ── R4.S2b F1/F2: the three read paths R4.S2 made live, by behaviour ────────

fn spawn_ok(l: &Life, caller: &str, task: &str, rid: &str) -> String {
    let ok = l
        .call(caller, "agents_spawn", json!({"task": task, "request_id": rid}))
        .unwrap_or_else(|e| panic!("spawn from {caller}: {} {}", e.code, e.message));
    ok["thread_id"].as_str().unwrap().to_string()
}

/// **A PARENT READS ITS CHILD'S AND ITS GRANDCHILD'S LOG; A SIBLING, A CHILD
/// LOOKING UP, AND A COUSIN ARE REFUSED.** `agents_log`'s widening from "self
/// only" to "self or a managed descendant" is an authorization rule, so both
/// sides are asserted: the grant (which a constant `false` fails) and the
/// refusal (which a constant `true` fails). The grant's evidence is the
/// child's own task text, read back out of the STORE through the log rows —
/// not the ledger the spawn wrote.
///
/// **THE MUTATIONS.** The gate is `networks::reads` since the networks merge
/// (R4.S3b): it returns `false` for every non-self agent → the grant fails;
/// returns `true` for every agent → the refusals fail.
#[test]
fn a_parent_reads_its_descendants_logs_and_a_sibling_is_refused() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let c1 = spawn_ok(&a, &parent, "task-of-c1", "c1");
    let c2 = spawn_ok(&a, &parent, "task-of-c2", "c2");
    let g = spawn_ok(&a, &c1, "task-of-g", "g");
    assert!(a.until(20, |m| [&c1, &c2, &g].iter().all(|k| turn_ended(m, k))), "all three ran");

    let log = |caller: &str, agent: &str| a.call(caller, "agents_log", json!({"agent": agent}));
    for (who, task) in [(&c1, "task-of-c1"), (&g, "task-of-g")] {
        let page = log(&parent, who).unwrap_or_else(|e| panic!("parent reads {who}'s log: {} {}", e.code, e.message));
        assert_eq!(page["agent"], json!(who));
        let rows = page["rows"].as_array().unwrap();
        assert!(!rows.is_empty(), "the descendant's log has rows");
        assert!(page.to_string().contains(task), "the rows are {who}'s own events (its task text): {page}");
        if task == "task-of-g" {
            assert!(!page.to_string().contains("task-of-c1"), "…and only its own: {page}");
        }
    }
    // the child reads its own child too, and itself
    assert!(log(&c1, &g).is_ok(), "c1 reads its child's log");
    assert!(log(&c2, &c2).is_ok(), "a thread reads its own log");
    for (caller, agent, why) in [
        (&c2, &c1, "a sibling"),
        (&c2, &g, "a sibling's child"),
        (&c1, &parent, "a child reading its parent"),
        (&g, &c2, "a grandchild reading its uncle"),
    ] {
        // R4.S3b: since the networks merge (2616a35) the gate is
        // `networks::reads` (N9), which refuses `no_route` — one answer for
        // "absent" and "not yours to see" (N2) — not R4.S2's pre-networks
        // `not_available_yet`.
        let e = log(caller, agent).expect_err(why);
        assert_eq!(e.code, threads::networks::refusal::NO_ROUTE, "{why}: {}", e.message);
        assert!(e.message.contains("may not read"), "{why}: {}", e.message);
    }
    a.kill();
}

/// **`agents_list` IS THE CALLER'S SUBTREE WITH STATES, AND ITS `filter` IS
/// HONOURED.** Parent → {c1 (held running), c2 (ended)}, c1 → g (held
/// running). The parent sees all three with their states, depths and parents;
/// c1 sees only g; c2 sees nothing. Each filter key narrows to exactly the
/// rows it names, and a key or value off the schema is refused `malformed`.
///
/// **THE MUTATIONS.** `rows` always `[]` (the inspector's M6) → red. The
/// filter ignored (`keeps` → `true`) → red. A constant non-empty answer fails
/// the c2 and filter cases.
#[test]
fn agents_list_is_the_callers_subtree_with_states_and_filter_is_honoured() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let release = tmp.path().join("release");
    let hold = format!("holdturn release={}", release.display());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let c1 = spawn_ok(&a, &parent, &hold, "c1");
    let c2 = spawn_ok(&a, &parent, "task-of-c2", "c2");
    let g = spawn_ok(&a, &c1, &hold, "g");
    // R9.F2i: c2's barrier is what `agents_list`'s `run_state` reads — the
    // `turn_ended` event on the store and the thread idle — not
    // `turn_ended(..)`, which is `result_turn` + the running row cleared. Both
    // of those land BEFORE `emit(TurnEnded)` and before the thread reads idle,
    // so the old barrier let the list run inside that window and c2 came back
    // `running` (tip 63f87994, :630, `left: "running" right: "ended"`).
    assert!(
        a.until(20, |m| turn_ended(m, &c2)
            && m.store.last_seq_of_kind(&c2, "turn_ended").unwrap().is_some()
            && m.is_idle(&c2).unwrap()
            && m.store.get_running_turn(&c1).unwrap().is_some()
            && m.store.get_running_turn(&g).unwrap().is_some()),
        "c2 ended, c1 and g held running"
    );
    let list = |caller: &str, args: Value| a.call(caller, "agents_list", args);
    let ids = |v: &Value| -> Vec<String> {
        let mut v: Vec<String> = v["rows"].as_array().unwrap().iter().map(|r| r["thread_id"].as_str().unwrap().to_string()).collect();
        v.sort();
        v
    };
    let sorted = |mut v: Vec<&String>| -> Vec<String> {
        v.sort();
        v.into_iter().cloned().collect()
    };

    let all = list(&parent, json!({})).unwrap();
    assert_eq!(ids(&all), sorted(vec![&c1, &c2, &g]), "the parent's whole subtree: {all}");
    let row = |id: &str| all["rows"].as_array().unwrap().iter().find(|r| r["thread_id"] == json!(id)).unwrap().clone();
    assert_eq!(row(&c1)["state"], json!("running"));
    assert_eq!(row(&c1)["depth"], json!(1));
    assert_eq!(row(&c1)["parent_id"], json!(parent));
    assert_eq!(row(&c1)["outcome"], Value::Null, "an open run has no outcome");
    assert_eq!(row(&c2)["state"], json!("ended"));
    assert_eq!(row(&c2)["outcome"], json!("completed"));
    assert_eq!(row(&g)["state"], json!("running"));
    assert_eq!(row(&g)["depth"], json!(2));
    assert_eq!(row(&g)["parent_id"], json!(c1));

    assert_eq!(ids(&list(&c1, json!({})).unwrap()), vec![g.clone()], "c1 sees its own subtree only");
    assert!(ids(&list(&c2, json!({})).unwrap()).is_empty(), "c2 has no descendants");
    // depth is below the CALLER: g is c1's depth-1 row.
    assert_eq!(list(&c1, json!({})).unwrap()["rows"][0]["depth"], json!(1));

    // F2: the filter is honoured.
    let f = |filter: Value| ids(&list(&parent, json!({"filter": filter})).unwrap());
    assert_eq!(f(json!({"state": "ended"})), vec![c2.clone()]);
    assert_eq!(f(json!({"state": "running"})), sorted(vec![&c1, &g]));
    assert_eq!(f(json!({"depth": 2})), vec![g.clone()]);
    assert_eq!(f(json!({"depth": 1})), sorted(vec![&c1, &c2]));
    assert_eq!(f(json!({"outcome": "completed"})), vec![c2.clone()]);
    assert!(f(json!({"outcome": "crashed"})).is_empty());
    assert_eq!(f(json!({"state": "running", "depth": 1})), vec![c1.clone()], "keys AND together");
    assert_eq!(f(json!({})), sorted(vec![&c1, &c2, &g]), "an empty filter keeps everything");
    for bad in [json!({"colour": "red"}), json!({"state": "asleep"}), json!({"depth": -1}), json!("ended")] {
        let e = list(&parent, json!({"filter": bad})).expect_err("an off-schema filter is refused");
        assert_eq!(e.code, codes::MALFORMED, "{bad}: {}", e.message);
    }
    std::fs::write(&release, b"go").unwrap();
    a.kill();
}

/// **`agents_result` IS `busy` UNTIL THE RUN ENDS, THEN THE TEXT; IT IS
/// FORBIDDEN TO A NON-ANCESTOR AND TO ONESELF; `run` IS REFUSED, NOT
/// IGNORED.** One held child, read before and after its release.
///
/// **THE MUTATIONS.** Drop the `run_ended` check → the `busy` assertion fails.
/// Drop the descendant check → the sibling's `forbidden` fails. Drop the
/// `run` refusal → `run: 0` answers instead of refusing.
#[test]
fn agents_result_is_busy_before_the_end_then_the_text_and_run_is_refused() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let release = tmp.path().join("release");
    let hold = format!("holdturn release={}", release.display());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let c = spawn_ok(&a, &parent, &hold, "c");
    let sib = spawn_ok(&a, &parent, &hold, "sib");
    assert!(a.until(10, |m| m.store.get_running_turn(&c).unwrap().is_some()), "c is running");
    let result = |caller: &str, args: Value| a.call(caller, "agents_result", args);

    let e = result(&parent, json!({"agent": c})).expect_err("open run");
    assert_eq!(e.code, codes::BUSY, "{}", e.message);
    assert!(e.message.contains("not_ended"), "{}", e.message);
    for (caller, agent, why) in [(&sib, &c, "a sibling"), (&c, &parent, "a child asking for its parent"), (&parent, &parent, "oneself")] {
        let e = result(caller, json!({"agent": agent})).expect_err(why);
        assert_eq!(e.code, codes::FORBIDDEN, "{why}: {}", e.message);
    }

    std::fs::write(&release, b"go").unwrap();
    assert!(
        a.until(20, |_| result(&parent, json!({"agent": c})).is_ok()),
        "the result is served once the run has ended"
    );
    let ok = result(&parent, json!({"agent": c})).unwrap();
    assert_eq!(ok["agent"], json!(c));
    assert_eq!(ok["outcome"], json!("completed"), "{ok}");
    assert!(ok["text"].as_str().unwrap().contains("Hello from fake claude."), "{ok}");
    // still forbidden after the end — the refusal is not a timing artefact
    let e = result(&sib, json!({"agent": c})).expect_err("sibling after the end");
    assert_eq!(e.code, codes::FORBIDDEN, "{}", e.message);
    // F2: `run` refused, never silently answered with the latest.
    let e = result(&parent, json!({"agent": c, "run": 0})).expect_err("run is not honoured yet");
    assert_eq!(e.code, codes::NOT_AVAILABLE_YET, "{}", e.message);
    assert!(e.message.contains("`run`"), "{}", e.message);
    assert!(result(&parent, json!({"agent": c, "run": null})).is_ok(), "a null `run` is an omitted one");
    a.kill();
}

// ── R4.S2b F3: "no `busy()` read decides a wait", THROUGH CALLS ─────────────
//
// `busy_polls` above is lexical and one level deep: it sees a `.busy()` typed
// inside a loop body and nothing reached through a call. HEAD's own heartbeat
// is the counter-example the R4.S2 inspector named (F3): the 1 Hz beat reaches
// `l.busy()` one hop down, in `emit_liveness`, and the busy-rate probe ticker
// reaches it two hops down, in `accounts_with_work`. Both REPORT; neither
// decides whether a wait goes on. So the true sentence is narrower than "no
// loop reads busy()", and this is the instrument for it: a call graph over
// `runtime.rs` (the only file that can call `Liveness::busy()` — the type is
// private to it), every timer loop's transitive reach, and the control-flow
// decisions inside each loop.

/// `src` with whole-line comments dropped and every string / char literal
/// blanked to spaces (same length, so indices still line up), so neither a
/// comment nor a format string can satisfy, fail, or unbalance the scan.
fn code_only(src: &str) -> String {
    let kept: String = src
        .lines()
        .map(|l| if l.trim_start().starts_with("//") { String::new() } else { l.to_string() })
        .map(|l| format!("{l}\n"))
        .collect();
    let b: Vec<char> = kept.chars().collect();
    let mut out = String::with_capacity(kept.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // raw string r"…" / r#"…"#
        if c == 'r' && (i == 0 || !(b[i - 1].is_alphanumeric() || b[i - 1] == '_')) {
            let mut j = i + 1;
            let mut hashes = 0;
            while j < b.len() && b[j] == '#' {
                hashes += 1;
                j += 1;
            }
            if j < b.len() && b[j] == '"' {
                let close: String = std::iter::once('"').chain(std::iter::repeat('#').take(hashes)).collect();
                let rest: String = b[j + 1..].iter().collect();
                let end = rest.find(&close).map(|k| j + 1 + rest[..k].chars().count() + close.len()).unwrap_or(b.len());
                for k in i..end {
                    out.push(if b[k] == '\n' { '\n' } else { ' ' });
                }
                i = end;
                continue;
            }
        }
        if c == '"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != '"' {
                if b[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            for k in i..=j.min(b.len() - 1) {
                out.push(if b[k] == '\n' { '\n' } else { ' ' });
            }
            i = j + 1;
            continue;
        }
        if c == '\'' {
            // a char literal ('x', '\n', '\''), never a lifetime
            let lit = if i + 2 < b.len() && b[i + 1] != '\\' && b[i + 2] == '\'' {
                Some(3)
            } else if i + 3 < b.len() && b[i + 1] == '\\' {
                (i + 2..b.len().min(i + 12)).find(|&k| b[k] == '\'').map(|k| k - i + 1)
            } else {
                None
            };
            if let Some(n) = lit {
                for _ in 0..n {
                    out.push(' ');
                }
                i += n;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Index of the `}` matching the `{` at `open`.
fn close_of(code: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (j, c) in code[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + j);
                }
            }
            _ => {}
        }
    }
    None
}

/// Every word-boundary occurrence of `kw` in `code`.
fn word_hits(code: &str, kw: &str) -> Vec<usize> {
    let b = code.as_bytes();
    code.match_indices(kw)
        .map(|(i, _)| i)
        .filter(|&i| i == 0 || !is_ident(b[i - 1]))
        .filter(|&i| {
            let e = i + kw.len();
            e >= b.len() || !is_ident(b[e]) || !kw.chars().last().map(|c| c.is_alphanumeric()).unwrap_or(false)
        })
        .collect()
}

/// `fn name` → (span of its body). Declarations without a body are skipped.
fn fn_bodies(code: &str) -> Vec<(String, usize, usize)> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for i in word_hits(code, "fn ") {
        let s = i + 3;
        let e = s + code[s..].bytes().take_while(|c| is_ident(*c)).count();
        if e == s {
            continue;
        }
        let name = code[s..e].to_string();
        let Some(rel) = code[e..].find(|c| c == '{' || c == ';') else { continue };
        if b[e + rel] == b';' {
            continue;
        }
        if let Some(close) = close_of(code, e + rel) {
            out.push((name, e + rel, close));
        }
    }
    out
}

/// The `pub` / `pub(crate)` functions of `code` among `names` — what another
/// file of the crate can call (R4.S5).
fn exported(code: &str, names: &std::collections::BTreeSet<String>) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for kw in ["pub fn ", "pub(crate) fn ", "pub async fn ", "pub(crate) async fn "] {
        for i in word_hits(code, kw) {
            let s = i + kw.len();
            let n: String = code[s..].bytes().take_while(|c| is_ident(*c)).map(|c| c as char).collect();
            if names.contains(&n) {
                out.insert(n);
            }
        }
    }
    out
}

/// Does `text` call `name` — `name(`, `.name(`, `::name(`, also `.name::<…>(`?
fn calls(text: &str, name: &str) -> bool {
    let b = text.as_bytes();
    text.match_indices(name).any(|(i, _)| {
        let e = i + name.len();
        (i == 0 || !is_ident(b[i - 1])) && (text[e..].starts_with('(') || text[e..].starts_with("::<"))
    })
}

/// The functions in `code` that reach `.busy()` through any chain of calls
/// (a fixed point over the name-level call graph — an over-approximation, the
/// safe direction for a ban).
fn busy_reachers(code: &str) -> std::collections::BTreeSet<String> {
    busy_reachers_seeded(code, &Default::default())
}

/// [`busy_reachers`], with `seeds` — functions DEFINED IN ANOTHER FILE that
/// reach `busy()` there — counted as tainted callees here (R4.S5). This is how
/// the scan crosses files without merging the crate into one name graph: one
/// merged graph ties every `now()` / `is_empty()` / `load()` in the crate to
/// every other, and flags the std calls of thirty unrelated loops.
fn busy_reachers_seeded(
    code: &str,
    seeds: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<String> {
    let fns = fn_bodies(code);
    let mut tainted: std::collections::BTreeSet<String> = fns
        .iter()
        .filter(|(_, s, e)| code[*s..=*e].contains(".busy()"))
        .map(|(n, _, _)| n.clone())
        .collect();
    tainted.extend(seeds.iter().cloned());
    loop {
        let before = tainted.len();
        for (n, s, e) in &fns {
            if !tainted.contains(n) && tainted.iter().any(|t| calls(&code[*s..=*e], t)) {
                tainted.insert(n.clone());
            }
        }
        if tainted.len() == before {
            return tainted;
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TimerLoop {
    /// The named `fn` the loop is written in.
    within: String,
    /// The loop reaches `.busy()`, directly or through a call.
    reaches_busy: bool,
    /// The busy-reaching functions the loop body calls directly.
    via: Vec<String>,
    /// Each control-flow decision in the loop (an `if` / `while` / `match`
    /// whose block can `break`, `continue` or `return`, or a `break` /
    /// `return` carrying a value) whose condition depends on a `busy()` read.
    decisions: Vec<String>,
}

/// Every TIMER loop in `code` — a `loop` / `while` whose body sleeps or ticks —
/// with what it reaches and what it decides.
fn timer_loops(code: &str) -> Vec<TimerLoop> {
    timer_loops_seeded(code, &Default::default())
}

/// [`timer_loops`] over one file, with `seeds` from another (see
/// [`busy_reachers_seeded`]).
fn timer_loops_seeded(code: &str, seeds: &std::collections::BTreeSet<String>) -> Vec<TimerLoop> {
    let reachers = busy_reachers_seeded(code, seeds);
    let fns = fn_bodies(code);
    let mut out = Vec::new();
    let mut starts: Vec<usize> = word_hits(code, "loop {");
    starts.extend(word_hits(code, "while "));
    starts.sort();
    for st in starts {
        let Some(rel) = code[st..].find('{') else { continue };
        let Some(close) = close_of(code, st + rel) else { continue };
        let body = &code[st..=close];
        if !(body.contains("sleep(") || body.contains(".tick().await")) {
            continue;
        }
        let tainted_text = |t: &str, vars: &std::collections::BTreeSet<String>| {
            t.contains(".busy()")
                || reachers.iter().any(|r| calls(t, r))
                || vars.iter().any(|v| !word_hits(t, v).is_empty())
        };
        // variables bound from a busy-derived expression, to a fixed point
        let mut vars: std::collections::BTreeSet<String> = Default::default();
        loop {
            let before = vars.len();
            for li in word_hits(body, "let ") {
                let stmt_end = body[li..].find(';').map(|k| li + k).unwrap_or(body.len());
                let stmt = &body[li..stmt_end];
                let Some(eq) = stmt.find('=') else { continue };
                if tainted_text(&stmt[eq..], &vars) {
                    for w in stmt[4..eq].split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                        if !w.is_empty() && w.chars().next().unwrap().is_lowercase() && w != "mut" && w != "ref" {
                            vars.insert(w.to_string());
                        }
                    }
                }
            }
            if vars.len() == before {
                break;
            }
        }
        let mut decisions = Vec::new();
        // the loop's own condition
        if body.starts_with("while ") && tainted_text(&body[..rel], &vars) {
            decisions.push(body[..rel].trim().to_string());
        }
        for kw in ["if ", "match ", "while "] {
            for h in word_hits(body, kw) {
                if h == 0 {
                    continue;
                }
                let Some(o) = body[h..].find('{') else { continue };
                let Some(c) = close_of(body, h + o) else { continue };
                let header = &body[h..h + o];
                let block = &body[h + o..=c];
                let exits = ["break", "continue", "return"].iter().any(|x| !word_hits(block, x).is_empty());
                if exits && tainted_text(header, &vars) {
                    decisions.push(header.trim().to_string());
                }
            }
        }
        for kw in ["break ", "return "] {
            for h in word_hits(body, kw) {
                let end = body[h..].find(|c| c == ';' || c == '}' || c == ',').map(|k| h + k).unwrap_or(body.len());
                if tainted_text(&body[h..end], &vars) {
                    decisions.push(body[h..end].trim().to_string());
                }
            }
        }
        let within = fns
            .iter()
            .filter(|(_, s, e)| *s < st && st < *e)
            .min_by_key(|(_, s, e)| e - s)
            .map(|(n, _, _)| n.clone())
            .unwrap_or_default();
        out.push(TimerLoop {
            within,
            reaches_busy: body.contains(".busy()") || reachers.iter().any(|r| calls(body, r)),
            via: reachers.iter().filter(|r| calls(body, r)).cloned().collect(),
            decisions,
        });
    }
    out
}

/// **NO `busy()` READ DECIDES A WAIT — FOLLOWED THROUGH CALLS.** The precise
/// sentence R4.S2's claim reduces to (its "none is in a loop" was false: F3).
///
/// Two halves, each shown able to fail first:
///
/// * **No timer loop decides on `busy()`**, however many calls down the read
///   is. Sentinels: a direct decision, a decision through a helper that the
///   old lexical `busy_polls` cannot see, and a report-only loop that must NOT
///   count as a decision (the opposed case).
/// * **The scan reaches the reads R4.S2's lexical scan could not** — the
///   liveness heartbeat (`spawn_heartbeat` → `emit_liveness`) and the
///   busy-rate probe ticker (→ `probe_busy_accounts` → `busy_poll_targets` →
///   `accounts_with_work`) — so "no decision" is measured over loops that DO
///   read `busy()`. Reaching is allowed: a timer may REPORT busy(). Deciding
///   is not.
///
/// **THE MUTATIONS.** In `spawn_heartbeat`, end the loop on a helper
/// `fn liveness_busy(&self, s: &ThreadState) -> bool { s.live.lock().unwrap().busy() }`
/// (`if !st.turn_running.load(..) && !mgr.liveness_busy(&st) { break; }`):
/// this test goes red, `no_liveness_busy_poll_remains_in_the_runtime` stays
/// green.
#[test]
fn no_busy_read_decides_a_wait_even_through_a_call() {
    // Sentinel 1: a direct decision.
    let direct = "fn a(&self) {\n    loop {\n        tokio::time::sleep(t).await;\n        if self.s.live.lock().unwrap().busy() { continue; }\n        break;\n    }\n}\n";
    let d = timer_loops(&code_only(direct));
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].reaches_busy && d[0].decisions.len() == 1, "a direct busy() decision is found: {d:?}");

    // Sentinel 2: the same decision one call down — what the lexical scan misses.
    let through = "fn liveness_busy(&self, s: &S) -> bool {\n    s.live.lock().unwrap().busy()\n}\nfn h(&self) {\n    loop {\n        tokio::time::sleep(t).await;\n        let still = mgr.liveness_busy(&st);\n        if !still { break; }\n    }\n}\n";
    assert!(busy_polls(through).is_empty(), "the lexical scan is blind to a decision through a call");
    let t = timer_loops(&code_only(through));
    assert_eq!(t.len(), 1, "{t:?}");
    assert_eq!(t[0].within, "h");
    assert!(t[0].reaches_busy, "the call graph follows the helper");
    assert_eq!(t[0].decisions, vec!["if !still".to_string()], "…and the bound variable carries the taint");

    // Opposed: a loop that REPORTS busy() and exits on something else decides nothing.
    let report = "fn beat(&self) {\n    let b = self.live.lock().unwrap().busy();\n    self.hub.emit(b);\n}\nfn h(&self) {\n    loop {\n        tokio::time::sleep(t).await;\n        mgr.beat();\n        if !st.turn_running.load(SeqCst) { break; }\n    }\n}\n";
    let r = timer_loops(&code_only(report));
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].reaches_busy, "a reporter is reached…");
    assert!(r[0].decisions.is_empty(), "…and is not a decision: {r:?}");

    // A string or a comment is not code.
    let fake = "fn h(&self) {\n    // if s.busy() { break; }\n    loop {\n        tokio::time::sleep(t).await;\n        log(\"if x.busy() { break }\");\n        if done { break; }\n    }\n}\n";
    let f = timer_loops(&code_only(fake));
    assert!(f.len() == 1 && !f[0].reaches_busy && f[0].decisions.is_empty(), "{f:?}");

    // Sentinel 3 (R4.S5): the helper in one FILE, the deciding loop in
    // another — the shape a poll in `managed.rs` through a `runtime.rs` helper
    // takes. The crate is scanned as one text, so the call graph crosses files.
    let helper_file = "pub fn liveness_busy(&self, s: &S) -> bool {\n    s.live.lock().unwrap().busy()\n}\nfn private_busy(&self) -> bool {\n    self.l.busy()\n}\n";
    let loop_file = "pub async fn wait_children(m: &M) {\n    loop {\n        tokio::time::sleep(t).await;\n        if !m.liveness_busy(&st) { break; }\n    }\n}\n";
    let helper = code_only(helper_file);
    let seeds = exported(&helper, &busy_reachers(&helper));
    assert_eq!(seeds.iter().cloned().collect::<Vec<_>>(), vec!["liveness_busy".to_string()], "only the pub reacher crosses files");
    let x = timer_loops_seeded(&code_only(loop_file), &seeds);
    assert!(x.len() == 1 && x[0].within == "wait_children" && x[0].decisions.len() == 1, "{x:?}");
    // Opposed: the same loop file, unseeded, finds the loop but no decision —
    // so the red above is the cross-file seed and nothing else.
    let y = timer_loops(&code_only(loop_file));
    assert!(y.len() == 1 && y[0].decisions.is_empty(), "{y:?}");

    // The real crate: EVERY src/ file (R4.S5, the R-5 inspector's F2), so the
    // managed path (`managed.rs`, `networks.rs`) is under the ban too. The
    // runtime is measured as before; every other file is scanned on its own
    // call graph, seeded with the runtime's exported busy() reachers.
    let files = src_files();
    let code: String = code_only(&files.iter().find(|(n, _)| n == "runtime.rs").unwrap().1);
    let reachers = busy_reachers(&code);
    let seeds = exported(&code, &reachers);
    assert!(seeds.contains("busy_poll_targets"), "the runtime's pub busy() reachers seed the other files: {seeds:?}");
    for must in ["emit_liveness", "accounts_with_work", "busy_poll_targets", "probe_busy_accounts"] {
        assert!(reachers.contains(must), "the call graph reaches busy() from `{must}`: {reachers:?}");
    }
    let mut loops = timer_loops(&code);
    assert!(loops.len() >= 3, "the scan finds the runtime's timer loops: {loops:?}");
    let mut scanned = Vec::new();
    for (name, src) in files.iter().filter(|(n, _)| n != "runtime.rs") {
        let found = timer_loops_seeded(&code_only(src), &seeds);
        scanned.push(format!("{name}:{}", found.len()));
        for mut l in found {
            l.within = format!("{name}::{}", l.within);
            loops.push(l);
        }
    }
    assert!(scanned.iter().any(|s| s.starts_with("managed.rs:")) && scanned.iter().any(|s| s.starts_with("networks.rs:")), "{scanned:?}");
    let deciding: Vec<&TimerLoop> = loops.iter().filter(|l| !l.decisions.is_empty()).collect();
    assert!(deciding.is_empty(), "NO TIMER LOOP DECIDES ON A busy() READ: {deciding:#?}");
    // Positive control on the REAL file: the scan does see the two reporters
    // R4.S2's lexical scan could not — the heartbeat (one hop, `emit_liveness`)
    // and the busy-rate probe ticker (three hops, `accounts_with_work`) — so
    // the empty `deciding` above is a measurement over loops that DO read
    // busy(), not a search that could not reach them. (Measured at R4.S2b:
    // seven timer loops reach busy(), all reporting — the heartbeat, four
    // `start_watchers` tickers (the probe ticker among them) whose callees
    // emit liveness or count in-flight work, and the two tail loops via
    // `process_line`. Reaching is not deciding.)
    for (within, via) in PROBE_AND_HEARTBEAT {
        assert!(
            loops.iter().any(|l| l.within == within && l.reaches_busy && l.via.iter().any(|v| v == via)),
            "the scan reaches busy() from the `{within}` timer loop through `{via}`: {loops:#?}"
        );
    }
}

/// The `fn`s holding the two timer loops that reach `busy()` and only report
/// it: the liveness heartbeat and the gauge's busy-rate probe ticker.
const PROBE_AND_HEARTBEAT: [(&str, &str); 2] =
    [("spawn_heartbeat", "emit_liveness"), ("start_watchers", "probe_busy_accounts")];

/// **A CHILD WHOSE RUN EMITS MORE THAN 400 EVENTS STILL REPORTS, AND ITS
/// PARENT IS WOKEN** (H179, 2026-10-16, an internal thread → child bd062063).
///
/// Production: the child's 74 s turn wrote 495 events. `run_ended` looked for
/// the child's last `user_message` inside a 400-event tail, did not find it,
/// and answered "not ended" forever — so the child was never reported, the
/// parent was never woken (DEC-32 stop-and-resume broken), and `agents/list`
/// said `running` with a null outcome for a thread that had been idle for an
/// hour.
///
/// **THE MUTATION.** Put `run_ended` back on `tail_events(id, 400)` for its
/// `user_message` / `turn_ended` positions → the child is never `ended` and
/// the parent's wake never arrives.
#[test]
fn a_child_with_a_long_run_reports_and_wakes_its_parent() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let child = spawn_ok(&a, &parent, "chattyturn", "long");
    assert!(a.until(20, |m| turn_ended(m, &child)), "the child's turn ends");
    let n = a.m.store.tail_events(&child, 100_000).unwrap().len();
    assert!(n > 400, "the child's run is longer than the old 400-event tail: {n} events");
    assert!(
        a.until(15, |m| wakes(m, &parent).len() == 1),
        "the long child's report wakes the parent once; child has {n} events"
    );
    assert!(wakes(&a.m, &parent)[0].contains(&child), "the wake carries the child's report");
    let rows = a.call(&parent, "agents_list", json!({})).unwrap();
    assert_eq!(rows["rows"][0]["thread_id"], json!(child));
    assert_eq!(rows["rows"][0]["state"], json!("ended"), "{rows}");
    assert_eq!(rows["rows"][0]["outcome"], json!("completed"), "{rows}");
    a.kill();
}

// ═════════════════════════════════════════════════════════════════════════════
// H179 — A CHILD'S COMPLETION IS AN EVENT (`docs/SUBAGENT-WAKE.md`)
// ═════════════════════════════════════════════════════════════════════════════
//
// The child's turn hands its own terminal record to `managed::turn_finished`;
// the record is written to the child's ledger, reported to the parent's, and
// the parent is woken through `networks::post`. Boot reconciles from the
// record, or from the child's terminal events read by kind. No tail.

/// The parent's wake messages as stored events (for their timestamps).
/// The message ids every turn on `id` was started with, in order (R10.M2).
fn delivered(m: &ThreadManager, id: &str) -> Vec<u64> {
    events(m, id)
        .into_iter()
        .filter_map(|e| match e {
            ThreadEvent::TurnStarted { message_ids, .. } => Some(message_ids),
            _ => None,
        })
        .flatten()
        .collect()
}

fn wake_events(m: &ThreadManager, parent: &str) -> Vec<threads::StoredEvent> {
    m.store
        .tail_events(parent, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| matches!(&e.event, ThreadEvent::UserMessage { metadata, .. } if metadata["origin"] == "agents"))
        .collect()
}

fn ts(s: &str) -> chrono::DateTime<chrono::FixedOffset> {
    chrono::DateTime::parse_from_rfc3339(s).unwrap_or_else(|e| panic!("ts {s}: {e}"))
}

/// Milliseconds from the child's last `turn_ended` (or `turn_interrupted`,
/// for a cancel with no execution) to the parent's wake message, both read off
/// the store the daemon wrote.
fn wake_latency_ms(m: &ThreadManager, child: &str, parent: &str) -> i64 {
    let end = ["turn_ended", "turn_interrupted", "turn_failed"]
        .iter()
        .filter_map(|k| m.store.newest_of_kind(child, k, false).unwrap())
        .max_by_key(|e| e.seq)
        .expect("the child's run has an end event");
    let wake = wake_events(m, parent).pop().expect("a wake");
    (ts(&wake.ts) - ts(&end.ts)).num_milliseconds()
}

/// The `outcome` attribute of the one report a wake carries.
fn reported_outcome(wake: &str) -> String {
    let at = wake.find("outcome=\"").expect("an outcome attribute") + 9;
    wake[at..].split('"').next().unwrap().to_string()
}

/// **A CHILD OF MORE THAN 10,000 EVENTS WAKES ITS PARENT, WITH ITS OWN WORDS,
/// INSIDE TWO SECONDS OF ITS END.** `floodturn` writes the child's final
/// top-level words, three native-subagent lines, then 10,200 system lines.
///
/// **RED WITHOUT THE FIX.** Before bb22b2fa no wake ever came (the
/// `user_message` was outside the 400-event tail `run_ended` read). On
/// bb22b2fa the wake came, but `outcome_of` still read the words out of a
/// 400-event tail: the report was empty, and this test fails at "carries the
/// child's own words".
#[test]
fn a_child_of_ten_thousand_events_wakes_its_parent_with_its_words() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let child = spawn_ok(&a, &parent, "floodturn", "flood");
    assert!(
        a.until(240, |m| wake_events(m, &parent).len() == 1),
        "the long child's completion wakes the parent"
    );
    let n = a.m.store.last_seq_of_kind(&child, "result_turn").unwrap().unwrap();
    assert!(n > 10_000, "the child's run is more than 10,000 events: result at seq {n}");
    let wake = wakes(&a.m, &parent).remove(0);
    assert!(wake.contains(&child), "{wake}");
    assert!(
        wake.contains("FLOOD-FINAL-WORDS"),
        "the wake carries the child's own words, not a native subagent's and not nothing: {wake}"
    );
    assert_eq!(reported_outcome(&wake), "ok");
    let lat = wake_latency_ms(&a.m, &child, &parent);
    assert!(lat < 2_000, "the wake lands within 2 s of the child's end: {lat} ms");
    let res = a.call(&parent, "agents_result", json!({"agent": child})).unwrap();
    assert_eq!(res["outcome"], json!("completed"));
    assert!(res["text"].as_str().unwrap().contains("FLOOD-FINAL-WORDS"), "{res}");
    a.kill();
}

/// **THE WAKE IS AN EVENT: BOUNDED LATENCY, NO POLL BEHIND IT.** The
/// placement backstop is switched OFF (`THREADS_RESUME_TICK_MS=0`), so the only
/// thing that can wake the parent is the child's own turn end; three children
/// in a row each wake their parent within 2 s of their end, and the parent's
/// wake turn RUNS (its `result_turn` lands) within 2 s more.
#[test]
fn the_wake_lands_within_two_seconds_of_the_childs_end() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    std::env::set_var("THREADS_RESUME_TICK_MS", "0");
    let a = Life::boot(tmp.path());
    for i in 0..3 {
        let parent = a.thread(tmp.path());
        let child = spawn_ok(&a, &parent, "basic", &format!("lat-{i}"));
        assert!(a.until(20, |m| wake_events(m, &parent).len() == 1), "child {i} wakes its parent");
        let lat = wake_latency_ms(&a.m, &child, &parent);
        assert!(lat < 2_000, "child {i}: wake {lat} ms after its end");
        assert!(
            a.until(2, |m| m.store.last_seq_of_kind(&parent, "result_turn").unwrap().is_some()),
            "child {i}: the woken parent's turn runs to its result within 2 s"
        );
    }
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    a.kill();
}

/// **A DAEMON DEATH BETWEEN THE CHILD'S END AND ITS REPORT STILL WAKES THE
/// PARENT, AT BOOT.** `Halt::BeforeCompletion` stops the child's completion
/// exactly where a kill after `turn_ended` would: no record, no report, no
/// wake. The next life reconciles the waiting parent from the child's terminal
/// events (by kind) — the wake lands within 2 s of boot, with the child's
/// words, and the child's ledger now holds its completion record.
///
/// **THE MUTATION.** Delete pass 2 of `managed::recover` (the `reconcile`
/// loop) → the parent is never woken.
#[test]
fn a_restart_mid_wait_still_wakes_the_parent() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    a.m.managed().halt_after(Some(Halt::BeforeCompletion));
    let child = spawn_ok(&a, &parent, "floodturn", "restart");
    assert!(
        a.until(240, |m| m.store.last_seq_of_kind(&child, "turn_ended").unwrap().is_some()
            && m.store.get_running_turn(&child).unwrap().is_none()),
        "the child's turn ends"
    );
    a.rt.block_on(async { tokio::time::sleep(Duration::from_millis(300)).await });
    assert!(wake_events(&a.m, &parent).is_empty(), "the halted completion woke nobody");
    a.kill();

    let booted = Instant::now();
    let b = Life::boot(tmp.path());
    assert!(
        b.until(2, |m| wake_events(m, &parent).len() == 1),
        "boot reconciliation wakes the waiting parent within 2 s (took {:?})",
        booted.elapsed()
    );
    let wake = wakes(&b.m, &parent).remove(0);
    assert!(wake.contains("FLOOD-FINAL-WORDS"), "{wake}");
    assert_eq!(reported_outcome(&wake), "ok");
    let l = threads::ledger::Ledger::read_jsonl(&tmp.path().join("ledgers").join(format!("{child}.jsonl"))).unwrap();
    assert!(
        l.events().iter().any(|e| e.kind == threads::ledger::Kind::System && e.body["fact"] == "run_ended"),
        "reconciliation wrote the child's completion record"
    );
    b.kill();
    // A third life wakes nobody again.
    let c = Life::boot(tmp.path());
    c.rt.block_on(async { tokio::time::sleep(Duration::from_millis(300)).await });
    assert_eq!(wake_events(&c.m, &parent).len(), 1, "reconciliation is idempotent");
    c.kill();
}

/// **A FAILED CHILD WAKES ITS PARENT `failed`; A CANCELLED ONE `cancelled`.**
///
/// * `badmodel` fails deterministically; with a ladder of one it is given up
///   (`turn_failed`) and the parent is woken with `outcome="failed"`.
/// * `holdturn` runs until cancelled; the cancel is the run's end, and the
///   parent is woken with `outcome="cancelled"` — not `failed`, which is what
///   a tail read of a run with no `result` line said before.
#[test]
fn a_failed_or_cancelled_child_wakes_its_parent_with_that_status() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = {
        let settings = json!({
            "cwd": tmp.path().to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
            "retry_policy": {"max_deterministic_attempts": 1},
        });
        let _ctx = a.rt.enter();
        a.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    };
    let failed = spawn_ok(&a, &parent, "badmodel", "f");
    assert!(a.until(30, |m| wake_events(m, &parent).len() == 1), "the failed child wakes its parent");
    let wake = wakes(&a.m, &parent).remove(0);
    assert!(wake.contains(&failed), "{wake}");
    assert_eq!(reported_outcome(&wake), "failed", "{wake}");
    let lat = wake_latency_ms(&a.m, &failed, &parent);
    assert!(lat < 2_000, "failed: wake {lat} ms after its end");

    // Let the parent's wake turn finish before the second spawn.
    assert!(a.until(10, |m| m.store.get_running_turn(&parent).unwrap().is_none()
        && m.store.waiting_queue(&parent).unwrap().is_empty()));
    let release = tmp.path().join("never");
    let cancelled = spawn_ok(&a, &parent, &format!("holdturn release={}", release.display()), "c");
    assert!(a.until(10, |m| m.store.get_running_turn(&cancelled).unwrap().is_some()), "the child runs");
    a.m.cancel(&cancelled).unwrap();
    assert!(a.until(10, |m| wake_events(m, &parent).len() == 2), "the cancelled child wakes its parent");
    let wake = wakes(&a.m, &parent).remove(1);
    assert!(wake.contains(&cancelled), "{wake}");
    assert_eq!(reported_outcome(&wake), "cancelled", "{wake}");
    let lat = wake_latency_ms(&a.m, &cancelled, &parent);
    assert!(lat < 2_000, "cancelled: wake {lat} ms after its end");
    a.kill();
}

// ═════════════════════════════════════════════════════════════════════════
// R9.L3 (gap L-3): CANCEL REACHES A PARKED TURN
// ═════════════════════════════════════════════════════════════════════════

/// Park `prompt` for `id` exactly as a budget kill / login refusal does: a
/// durable `parked_turns` row, which `maybe_dispatch` redelivers first.
fn park_turn(m: &ThreadManager, id: &str, prompt: &str) {
    m.store
        .upsert_parked_turn(&threads::store::ParkedTurnRow {
            thread_id: id.to_string(),
            prompt: prompt.to_string(),
            message_ids: vec![1],
            attempt: 1,
            reason: "budget exhausted (test park)".into(),
            budget_key: None,
            parked_at: threads::store::now_rfc3339(),
        })
        .unwrap();
}

fn started(m: &ThreadManager, id: &str) -> usize {
    events(m, id).iter().filter(|e| matches!(e, ThreadEvent::TurnStarted { .. })).count()
}

fn operator_interrupts(m: &ThreadManager, id: &str) -> usize {
    events(m, id)
        .iter()
        .filter(|e| matches!(e, ThreadEvent::TurnInterrupted { trigger, .. } if trigger == "operator"))
        .count()
}

/// **A CANCELLED PARKED TURN NEVER RUNS — PAST THE RESUME TICK, AND PAST A
/// DAEMON RESTART — WHILE AN UN-CANCELLED ONE BESIDE IT IS REDELIVERED.**
///
/// Two idle threads each get a durable parked turn. One is cancelled. Then
/// every path that redelivers a parked turn is driven: `resume_sweep` called
/// directly (the redelivery itself, so no period can be out-waited), and a
/// SECOND daemon life over the same data dir booted with
/// `THREADS_RESUME_TICK_MS=100` (`recover()` plus ten resume ticks in the
/// 1 s waited — the longest period in the path, shrunk and outlasted, doctrine 4).
///
/// The pair rules out the constant implementations: "never redeliver
/// anything" fails the un-cancelled thread, "cancel is a no-op" fails the
/// cancelled one (red at 97614bfd: the row survives the cancel and the
/// sweep runs the prompt).
#[test]
fn a_cancelled_parked_turn_never_runs_past_ten_resume_ticks_and_a_restart() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let cancelled = a.thread(tmp.path());
    let kept = a.thread(tmp.path());
    park_turn(&a.m, &cancelled, "PARKED-PROMPT-THE-OPERATOR-CANCELLED");
    park_turn(&a.m, &kept, "PARKED-PROMPT-STILL-OWED");
    {
        let _ctx = a.rt.enter();
        a.m.cancel(&cancelled).unwrap();
    }
    assert!(
        a.m.store.get_parked_turn(&cancelled).unwrap().is_none(),
        "the cancel dropped the parked row"
    );
    assert_eq!(operator_interrupts(&a.m, &cancelled), 1, "the cancel is recorded as an operator interrupt");
    assert!(a.m.store.get_parked_turn(&kept).unwrap().is_some(), "the other thread's park is untouched");

    // The redelivery path itself, driven directly.
    for _ in 0..3 {
        a.rt.block_on(async { a.m.resume_sweep() });
    }
    assert!(a.until(10, |m| turn_ended(m, &kept)), "the UN-cancelled parked turn IS redelivered and runs");
    assert!(a.m.store.get_parked_turn(&kept).unwrap().is_none(), "redelivered once, then cleared");
    assert_eq!(started(&a.m, &cancelled), 0, "the cancelled parked prompt never ran");
    a.kill();

    // A second life, ticking every 100 ms: recovery and ten ticks.
    std::env::set_var("THREADS_RESUME_TICK_MS", "100");
    let b = Life::boot(tmp.path());
    b.rt.block_on(async { tokio::time::sleep(Duration::from_millis(1_000)).await });
    assert_eq!(started(&b.m, &cancelled), 0, "the cancelled parked prompt never ran, across a restart and ten ticks");
    assert!(b.m.store.get_parked_turn(&cancelled).unwrap().is_none());
    b.kill();
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
}

/// **A PARKED MANAGED CHILD, CANCELLED, WAKES ITS PARENT `cancelled`.**
///
/// The child's run ends into a park it cannot leave (its account's weekly
/// reading is over the default 95 % threshold, so `admit_or_park` refuses the
/// redelivery), so `managed::idle` is false and the parent is — correctly —
/// not woken. Then the operator cancels the child. Red at 97614bfd: the
/// cancel touched only a retry row, the parked row kept the child "not idle",
/// and the parent waited forever. The first half (no wake while parked) is the
/// opposing case: an implementation that reported on every turn end fails it.
#[test]
fn a_parked_managed_child_cancelled_wakes_its_parent_cancelled() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let release = tmp.path().join("release-parked-child");
    let child = spawn_ok(&a, &parent, &format!("holdturn release={}", release.display()), "p");
    assert!(a.until(10, |m| m.store.get_running_turn(&child).unwrap().is_some()), "the child runs");

    // The block, then the park the run ends into.
    a.m.budgets
        .observe("aa-owned", threads::gauge::Axis::WeeklyAll, 99.0, None, "test")
        .unwrap();
    park_turn(&a.m, &child, "PARKED-CHILD-PROMPT");
    std::fs::write(&release, b"go").unwrap();
    assert!(a.until(25, |m| m.store.get_running_turn(&child).unwrap().is_none()), "the child's turn ends");
    a.rt.block_on(async {
        a.m.resume_sweep();
        tokio::time::sleep(Duration::from_millis(500)).await;
    });
    assert!(a.m.store.get_parked_turn(&child).unwrap().is_some(), "the child is parked");
    assert_eq!(wake_events(&a.m, &parent).len(), 0, "a parked child has not ended: its parent is not woken");

    {
        let _ctx = a.rt.enter();
        a.m.cancel(&child).unwrap();
    }
    assert!(a.until(10, |m| wake_events(m, &parent).len() == 1), "the cancelled parked child wakes its parent");
    let wake = wakes(&a.m, &parent).remove(0);
    assert!(wake.contains(&child), "{wake}");
    assert_eq!(reported_outcome(&wake), "cancelled", "{wake}");
    assert!(a.m.store.get_parked_turn(&child).unwrap().is_none(), "the parked row is gone");
    a.kill();
}

// ═════════════════════════════════════════════════════════════════════════
// R9.L3b (L-3 inspect finding 1): THE IN-MEMORY HALF OF THE CANCEL
// ═════════════════════════════════════════════════════════════════════════

/// A thread keyed to its own `kill`-on-trip cash budget of $0.01 on `aa-owned`.
fn keyed_thread(a: &Life, dir: &Path, key: &str) -> String {
    let cfg: threads::budgets::BudgetConfig = serde_json::from_value(json!({
        "key": key, "cash_usd": 0.01, "on_trip": "kill", "accounts": ["aa-owned"],
    }))
    .unwrap();
    a.m.budgets.set_config(&cfg).unwrap();
    let settings = json!({
        "cwd": dir.to_string_lossy(),
        "model": "sonnet",
        "account": {"named": "aa-owned"},
        "budget_key": key,
    });
    let _ctx = a.rt.enter();
    a.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
}

fn send_text(a: &Life, id: &str, text: &str) {
    let _ctx = a.rt.enter();
    a.m.send(id, text.into(), threads::SendIntent::Immediate, json!({}), false).unwrap();
}

/// **A CANCEL CLEARS THE PARK REASON AND THE BUDGET-KILL LATCH, SO THE
/// OPERATOR'S NEXT MESSAGE RUNS — WHILE AN UN-CANCELLED KILLED THREAD BESIDE
/// IT STAYS PARKED AND KEEPS ITS REASON.**
///
/// Both threads are parked the real way: a `killcheap` turn trips its own
/// `kill` cash budget on the live estimate, `park_killed_turn` writes the row
/// and the in-memory `park_reason`, and the trip sets `budget_kill_latch`.
/// The authoritative cost then reconciles the meter back UNDER the limit, so
/// from that point the latch ALONE refuses the next admission (the shape
/// `budgets_e2e::a_killed_turn_stays_parked_instead_of_relaunching_itself`
/// pins) — which is what makes "the next message runs" a test of the latch
/// clear rather than of the meter.
///
/// Guards (inspect finding 1 of R9.L3, 2f40860):
/// - `park_state(cancelled)` is null right after the cancel — red if the
///   `park_reason` clear in `cancel` is removed (the live reason survives);
/// - a message sent after the cancel starts a turn — red if the latch clear
///   is removed (the stale latch parks it).
/// The un-cancelled thread is the opposing case for both: it keeps its reason,
/// and the same send to it is refused by its latch — so neither "always null"
/// nor "always admit" passes.
#[test]
fn a_cancel_clears_the_park_reason_and_the_kill_latch_so_the_next_message_runs() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let cancelled = keyed_thread(&a, tmp.path(), "l3b-cancel");
    let kept = keyed_thread(&a, tmp.path(), "l3b-keep");

    for id in [&cancelled, &kept] {
        send_text(&a, id, "killcheap: overshoot then flush a cheap result");
        assert!(
            a.until(30, |m| m.store.get_parked_turn(id).unwrap().is_some()
                && m.store.get_running_turn(id).unwrap().is_none()),
            "the budget kill parks {id}"
        );
    }
    // Let both epilogues reconcile and any redelivery attempt hit the latch.
    a.rt.block_on(async {
        a.m.resume_sweep();
        tokio::time::sleep(Duration::from_millis(500)).await;
    });
    for (id, key) in [(&cancelled, "l3b-cancel"), (&kept, "l3b-keep")] {
        // The meter alone would now admit: only the latch holds the thread.
        let snap = a.m.budgets.snapshot(key, &a.m.account_pool()).unwrap().unwrap().to_json();
        let consumed = snap["cash"]["consumed_usd"].as_f64().unwrap();
        assert!(consumed < 0.01, "{key}: reconciliation left the meter under the limit (got {consumed})");
        let ps = a.m.park_state(id).unwrap();
        assert!(!ps.is_null(), "{id} reports parked before the cancel: {ps}");
        assert_eq!(started(&a.m, id), 1, "only the killed turn has run on {id}");
    }

    {
        let _ctx = a.rt.enter();
        a.m.cancel(&cancelled).unwrap();
    }
    assert!(a.m.store.get_parked_turn(&cancelled).unwrap().is_none(), "the cancel dropped the row");
    let ps = a.m.park_state(&cancelled).unwrap();
    assert!(ps.is_null(), "the cancel cleared the park reason, so the thread no longer reports parked: {ps}");
    let kept_ps = a.m.park_state(&kept).unwrap();
    assert!(
        kept_ps["reason"].as_str().is_some_and(|r| !r.is_empty()),
        "the UN-cancelled parked thread keeps its reason: {kept_ps}"
    );

    // The operator's next message, to each.
    send_text(&a, &cancelled, "after the cancel");
    send_text(&a, &kept, "while still killed");
    assert!(
        a.until(15, |m| started(m, &cancelled) == 2),
        "a message after the cancel runs — no stale latch refuses it (turns started: {})",
        started(&a.m, &cancelled)
    );
    a.rt.block_on(async {
        a.m.resume_sweep();
        tokio::time::sleep(Duration::from_millis(1_000)).await;
    });
    assert_eq!(started(&a.m, &kept), 1, "the un-cancelled thread's latch still refuses it");
    assert!(!a.m.park_state(&kept).unwrap().is_null(), "and it still reports parked");
    a.kill();
}

fn parks(m: &ThreadManager, id: &str) -> usize {
    events(m, id).iter().filter(|e| matches!(e, ThreadEvent::TurnParked { .. })).count()
}

/// A thread flipped to `projected` whose ledger holds a prompt with no text:
/// its next turn is refused by `refuse_projection`, which parks the prompt
/// AND arms `projection_hold` (the one place, with the login refusal, that
/// arms it). Same shape as `projected_turn::a_render_the_thread_does_not_allow_parks_the_turn`.
fn projection_refused_thread(a: &Life, dir: &Path, text: &str) -> String {
    use threads::ledger::{Draft, Kind, Ledger, ThreadMeta};
    use threads::settings::{Projection, SettingsPatch};
    let id = a.thread(dir);
    a.m.update_settings(&id, SettingsPatch { projection: Some(Projection::Projected), ..Default::default() })
        .unwrap();
    let lp = threads::ledger::turn::ledger_path(dir, &id);
    std::fs::create_dir_all(lp.parent().unwrap()).unwrap();
    let mut seeded = Ledger::new(ThreadMeta::root(id.clone(), "claude_code", "2026-10-10T00:00:00Z"));
    seeded
        .append(Draft::new(Kind::User, "2026-10-10T00:00:01Z", json!({"text": null})).with_id("lost-prompt"))
        .unwrap();
    seeded.write_jsonl(&lp).unwrap();
    send_text(a, &id, text);
    assert!(
        a.until(15, |m| parks(m, &id) == 1 && m.store.get_parked_turn(&id).unwrap().is_some()),
        "the projection refusal parks {id}"
    );
    id
}

/// **A CANCEL CLEARS THE PROJECTION HOLD, SO A PROMPT PARKED ON THE THREAD
/// AFTERWARDS IS REDELIVERED AT ONCE — WHILE AN UN-CANCELLED THREAD'S HOLD
/// STILL KEEPS ITS PARKED PROMPT FROM BEING RE-ASKED.**
///
/// Both threads are refused by the projection render, which parks the prompt
/// and arms `projection_hold` (30 s default `THREADS_PROJECTION_HOLD_MS`,
/// longer than this test; the resume tick is 60 s here, so only the direct
/// `resume_sweep` calls below redeliver anything). One is cancelled. Then a
/// parked row is written on it again (`park_turn`, nothing else changed — no
/// fingerprint input moves) and both are swept.
///
/// - cancelled: the redelivery is attempted, refused by the render again, and
///   parks a SECOND time — red if the `projection_hold` clear in `cancel` is
///   removed (the stale hold, its fingerprint unchanged, swallows the
///   redelivery: one park);
/// - kept (the opposing case): its hold is intact, so the same sweep does not
///   re-ask it — still one park. A cancel that dropped every thread's hold, or
///   a sweep that ignored holds, fails here.
///
/// Reachability (doctrine 5), stated plainly: the re-park is written by the
/// test. In production every path that writes a parked row after a cancel
/// either re-arms the hold itself (projection / login refusal) or runs a turn
/// first, which grows the ledger and so moves the fingerprint; the stale hold
/// is then discarded by the gate on its own. So this guards the clear as
/// written; it does not demonstrate a live-reachable stranding.
#[test]
fn a_cancel_clears_the_projection_hold_so_a_later_park_is_redelivered() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    std::env::remove_var("THREADS_PROJECTION_HOLD_MS");
    let a = Life::boot(tmp.path());
    let cancelled = projection_refused_thread(&a, tmp.path(), "projected, then cancelled");
    let kept = projection_refused_thread(&a, tmp.path(), "projected, still held");

    {
        let _ctx = a.rt.enter();
        a.m.cancel(&cancelled).unwrap();
    }
    assert!(a.m.store.get_parked_turn(&cancelled).unwrap().is_none(), "the cancel dropped the row");
    park_turn(&a.m, &cancelled, "PARKED-AGAIN-AFTER-THE-CANCEL");

    for _ in 0..3 {
        a.rt.block_on(async {
            a.m.resume_sweep();
            tokio::time::sleep(Duration::from_millis(300)).await;
        });
    }
    assert!(
        a.until(10, |m| parks(m, &cancelled) == 2),
        "after the cancel no hold remains: the new parked prompt is redelivered (and refused by the \
         render again) — parks: {}",
        parks(&a.m, &cancelled)
    );
    assert_eq!(parks(&a.m, &kept), 1, "the UN-cancelled thread's hold still keeps its parked prompt from being re-asked");
    assert!(a.m.store.get_parked_turn(&kept).unwrap().is_some(), "and its prompt is still parked");
    a.kill();
}

// ═════════════════════════════════════════════════════════════════════════
// R9.N1 (gap N-1): CANCELLING A PARENT CANCELS ITS MANAGED SUBTREE, AND THE
// CASCADE'S REPORTS DO NOT WAKE THE PARENT THAT WAS CANCELLED
// ═════════════════════════════════════════════════════════════════════════

/// Is `pid` a live process? A zombie (exited, not yet reaped) is not one.
fn pid_alive(pid: i64) -> bool {
    if unsafe { libc::kill(pid as i32, 0) } != 0 {
        return false;
    }
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout).trim().to_string();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// **THE DECLARED BOUND on the gap between a cut turn's running row clearing
/// and its `turn_interrupted` event landing.** Both are written by the same
/// epilogue in `run_once`, a few store writes apart, with no timer between
/// them; 5 s is the 25 ms tail poll (`THREADS_TAIL_POLL_MS`) times 200, room
/// for a loaded machine. A wait, not a sleep: it returns as soon as the event
/// is there.
const INTERRUPT_EMIT_BOUND_S: u64 = 5;

/// The pid of `id`'s executing `claude`, read off its `running_turns` row.
fn running_pid(m: &ThreadManager, id: &str) -> i64 {
    m.store.get_running_turn(id).unwrap().expect("a running turn").pid
}

/// Releases every `holdturn` it was handed when the test ends — green or red —
/// so no fake `claude` outlives the test by its 20 s ceiling.
struct Releases(Vec<PathBuf>);
impl Drop for Releases {
    fn drop(&mut self) {
        for r in &self.0 {
            let _ = std::fs::write(r, b"go");
        }
        // The tmp dir (and these files) goes right after this guard; give the
        // fixture's 20 ms poll time to see them first.
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// The `child{finished}` outcomes on `parent`'s ledger FILE, by child id.
fn finished_outcomes(dir: &Path, parent: &str) -> BTreeMap<String, String> {
    let p = dir.join("ledgers").join(format!("{parent}.jsonl"));
    let Ok(l) = threads::ledger::Ledger::read_jsonl(&p) else { return BTreeMap::new() };
    l.events()
        .iter()
        .filter(|e| e.kind == threads::ledger::Kind::Child && e.body["phase"] == "finished")
        .map(|e| {
            (
                e.body["child_id"].as_str().unwrap().to_string(),
                e.body["outcome"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// **CANCELLING A PARENT STOPS ITS CHILD AND ITS GRANDCHILD — ALL THREE
/// `claude` PROCESSES GONE, BY EXACT PID — AND NOTHING OUTSIDE ITS SUBTREE.**
///
/// Parent, child (spawned by the parent) and grandchild (spawned by the child)
/// each run a `holdturn` turn that only the test can release. The operator
/// cancels the PARENT. Red at ed7e868b: `cancel` set the parent's flag alone,
/// so the child and grandchild ran on (to their 20 s ceiling) on the parent's
/// key.
///
/// Opposed: an unrelated root with its own running child — a second subtree —
/// is still running, with live pids, after the cancel. A cancel that stopped
/// every thread fails it.
#[test]
fn cancelling_a_parent_stops_its_child_and_grandchild_and_their_claude_processes() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let rel = |n: &str| tmp.path().join(format!("release-{n}"));
    let _releases = Releases(["p", "c", "g", "op", "oc"].iter().map(|n| rel(n)).collect());
    let hold = |n: &str| format!("holdturn release={}", rel(n).display());
    let a = Life::boot(tmp.path());

    let parent = a.thread(tmp.path());
    send_text(&a, &parent, &hold("p"));
    assert!(a.until(10, |m| m.store.get_running_turn(&parent).unwrap().is_some()), "the parent runs");
    let child = spawn_ok(&a, &parent, &hold("c"), "c");
    assert!(a.until(10, |m| m.store.get_running_turn(&child).unwrap().is_some()), "the child runs");
    let grandchild = spawn_ok(&a, &child, &hold("g"), "g");
    assert!(a.until(10, |m| m.store.get_running_turn(&grandchild).unwrap().is_some()), "the grandchild runs");

    // The other subtree: a root and its child, both running.
    let other = a.thread(tmp.path());
    send_text(&a, &other, &hold("op"));
    assert!(a.until(10, |m| m.store.get_running_turn(&other).unwrap().is_some()), "the other root runs");
    let other_child = spawn_ok(&a, &other, &hold("oc"), "oc");
    assert!(a.until(10, |m| m.store.get_running_turn(&other_child).unwrap().is_some()), "its child runs");

    let subtree = [parent.clone(), child.clone(), grandchild.clone()];
    let pids: Vec<i64> = subtree.iter().map(|t| running_pid(&a.m, t)).collect();
    let other_pids: Vec<i64> = [&other, &other_child].iter().map(|t| running_pid(&a.m, t)).collect();
    for p in pids.iter().chain(&other_pids) {
        assert!(pid_alive(*p), "sentinel: pid {p} is a live process before the cancel");
    }

    {
        let _ctx = a.rt.enter();
        a.m.cancel(&parent).unwrap();
    }
    assert!(
        a.until(10, |m| subtree.iter().all(|t| m.store.get_running_turn(t).unwrap().is_none())),
        "the cancel stopped the whole subtree; still running: {:?}",
        subtree.iter().filter(|t| a.m.store.get_running_turn(t).unwrap().is_some()).collect::<Vec<_>>()
    );
    assert!(
        a.until(5, |_| pids.iter().all(|p| !pid_alive(*p))),
        "no orphan claude: pids {:?} alive {:?}",
        pids,
        pids.iter().map(|p| pid_alive(*p)).collect::<Vec<_>>()
    );
    for t in &subtree {
        // The runtime clears the running row BEFORE it emits `turn_interrupted`
        // (R9.N1 inspect F1, beba56c), so the event is waited for, bounded by
        // INTERRUPT_EMIT_BOUND_S, then counted exactly.
        a.until(INTERRUPT_EMIT_BOUND_S, |m| operator_interrupts(m, t) >= 1);
        assert_eq!(operator_interrupts(&a.m, t), 1, "{t}: its turn ended as an operator interrupt");
        assert!(
            !events(&a.m, t).iter().any(|e| matches!(e, ThreadEvent::ResultTurn { .. })),
            "{t}: cut off, not run to its result"
        );
    }

    // The other subtree is untouched.
    for (t, p) in [&other, &other_child].iter().zip(&other_pids) {
        assert!(a.m.store.get_running_turn(t).unwrap().is_some(), "{t}: outside the subtree, still running");
        assert!(pid_alive(*p), "{t}: its claude pid {p} is still alive");
        assert_eq!(operator_interrupts(&a.m, t), 0, "{t}: not interrupted");
    }
    std::fs::write(rel("op"), b"go").unwrap();
    std::fs::write(rel("oc"), b"go").unwrap();
    assert!(a.until(15, |m| turn_ended(m, &other) && turn_ended(m, &other_child)), "the other subtree runs to its end");
    a.kill();
}

/// **A CHILD THE CASCADE CANCELLED NEVER WAKES ITS CANCELLED PARENT — PAST
/// 1.5 s, A DIRECT RESUME SWEEP AND A DAEMON RESTART — WHILE A CHILD THAT
/// FINISHED BEFORE THE CANCEL KEEPS ITS REPORT, AND AN UN-CANCELLED PARENT IS
/// WOKEN BY ITS CANCELLED CHILD.**
///
/// Parent P has two children: `early` ran to its result before the cancel
/// (its report is on P's ledger, `ok`), `late` is still running. The operator
/// cancels P; the cascade cancels `late`, whose report empties P's
/// outstanding set — the exact event that queued a wake at ed7e868b. The
/// wake path has no timer of its own: the report queues the wake inline,
/// and the only re-delivery is boot recovery's `deliver_due_wakes`. So the
/// test outlasts the inline path (1.5 s against a 25 ms tail poll), then
/// drives a second daemon life over the same data dir.
///
/// Paired: parent Q is NOT cancelled; the operator cancels Q's child. Q IS
/// woken, once, `cancelled` — so "never wake on a cancelled report" fails.
#[test]
fn a_cascade_cancelled_child_never_wakes_its_cancelled_parent_past_one_and_a_half_seconds_and_a_restart() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let rel = |n: &str| tmp.path().join(format!("release-{n}"));
    let _releases = Releases(["late", "qc"].iter().map(|n| rel(n)).collect());
    let hold = |n: &str| format!("holdturn release={}", rel(n).display());
    let a = Life::boot(tmp.path());

    let p = a.thread(tmp.path());
    let late = spawn_ok(&a, &p, &hold("late"), "late");
    assert!(a.until(10, |m| m.store.get_running_turn(&late).unwrap().is_some()), "the late child runs");
    let early = spawn_ok(&a, &p, "the early child's task", "early");
    assert!(a.until(15, |_| finished_outcomes(tmp.path(), &p).contains_key(&early)), "the early child reported");
    assert_eq!(wake_events(&a.m, &p).len(), 0, "sentinel: P still owes `late`, so it is not woken yet");

    let q = a.thread(tmp.path());
    let qc = spawn_ok(&a, &q, &hold("qc"), "qc");
    assert!(a.until(10, |m| m.store.get_running_turn(&qc).unwrap().is_some()), "Q's child runs");

    {
        let _ctx = a.rt.enter();
        a.m.cancel(&p).unwrap();
        a.m.cancel(&qc).unwrap();
    }
    // The report that empties P's outstanding set lands — the wake's trigger.
    assert!(
        a.until(10, |_| finished_outcomes(tmp.path(), &p).get(&late).map(String::as_str) == Some("cancelled")),
        "the cascade cancelled `late` and its report reached P's ledger: {:?}",
        finished_outcomes(tmp.path(), &p)
    );
    // Paired: Q is woken by its cancelled child.
    assert!(a.until(10, |m| wake_events(m, &q).len() == 1), "the UN-cancelled parent IS woken by its child");
    let wake = wakes(&a.m, &q).remove(0);
    assert!(wake.contains(&qc), "{wake}");
    assert_eq!(reported_outcome(&wake), "cancelled", "{wake}");

    a.rt.block_on(async {
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        a.m.resume_sweep();
        tokio::time::sleep(Duration::from_millis(300)).await;
    });
    // R10.M2 (L-8, §7.2): the report is not dropped — its wake is queued on
    // P `ride`: recorded, carried by P's next turn, and the cause of none.
    let p_wakes = wake_events(&a.m, &p);
    assert_eq!(p_wakes.len(), 1, "the cascade's report lands on the cancelled parent as one wake");
    let p_wake = p_wakes[0].message_id.expect("the wake is addressable");
    assert_eq!(
        a.m.store.waiting_queue(&p).unwrap().iter().map(|r| (r.message_id, r.dispatch.clone())).collect::<Vec<_>>(),
        vec![(p_wake, "ride".to_string())],
        "and it rides — it does not wake P"
    );
    assert_eq!(started(&a.m, &p), 0, "P runs no turn for it");
    // The child that finished before the cancel still reported, normally.
    assert_eq!(
        finished_outcomes(tmp.path(), &p).get(&early).map(String::as_str),
        Some("ok"),
        "the early child's report stands"
    );
    a.kill();

    // Boot recovery re-delivers every due wake it does not find queued.
    let b = Life::boot(tmp.path());
    b.rt.block_on(async { tokio::time::sleep(Duration::from_millis(1_000)).await });
    assert_eq!(wake_events(&b.m, &p).len(), 1, "nor after a restart: the withheld wake is not re-sent as a waking one");
    assert_eq!(started(&b.m, &p), 0);
    assert_eq!(wake_events(&b.m, &q).len(), 1, "Q's one wake is not re-sent");
    // The operator's next message to P carries the cascade's report first.
    let d = {
        let _ctx = b.rt.enter();
        b.m.send(&p, "OPERATOR-NEXT-TO-P".into(), threads::SendIntent::Immediate, json!({}), false).unwrap()
    };
    assert!(b.until(10, |m| started(m, &p) == 1), "P's next message starts a turn");
    assert_eq!(delivered(&b.m, &p), vec![p_wake, d], "carrying the withheld report ahead of it");
    b.kill();
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
