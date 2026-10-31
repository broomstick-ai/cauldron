//! **PL-3: THE PROTOLEDGER SHADOW, FLAG ON** (`THREADS_PL_SHADOW=1`), against a
//! daemon on `127.0.0.1:0` and the fake claude's `projected` mode, which
//! appends to the thread's native transcript exactly as real claude does.
//! Every test in this binary turns the flag on BEFORE its manager is built;
//! the flag-off proof is its own binary (`tests/pl_shadow_off.rs`), so the two
//! never race on one process's environment.
//!
//! What the ledger holds is read back from the LEDGER FILE with protoledger's
//! plain reader (`threads::pl::shadow::read_active_path`), never through the
//! daemon's store handle or its own comparison (doctrine 3), and each thread's
//! assistant records are matched to the uuids in THAT thread's native
//! transcript, so two threads with identical reply texts cannot pass for each
//! other.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use threads::pl::shadow::{read_active_path, read_all, StatsSnapshot};
use threads::pl::{Event, Kind};
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

#[path = "support/first_exec.rs"]
mod first_exec;

const ACCOUNT: &str = "aa-owned";

fn set_env() {
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake);
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "200");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var("THREADS_CLAUDE_HOME", std::env::temp_dir().join("threads-tests-claude-home"));
    std::env::remove_var("THREADS_CHASSIS_PORT");
    std::env::remove_var("CAULDRON_DATA_ROOT");
    std::env::set_var("THREADS_PL_SHADOW", "1");
}

fn manager_at(dir: &Path) -> ThreadManager {
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(ACCOUNT.to_string(), dir.join(ACCOUNT).to_string_lossy().to_string());
    build_manager(Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap()
}

async fn start(dir: &Path) -> ThreadManager {
    let m = manager_at(dir);
    let (_addr, h) = threads::chassis::serve(std::sync::Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap()).await.unwrap();
    std::mem::forget(h);
    m
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..800 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

fn count_kind(m: &ThreadManager, id: &str, kind: &str) -> usize {
    m.store.list_events(id, 0, 100_000, None).unwrap().iter().filter(|e| e.event.kind() == kind).count()
}

fn stats(m: &ThreadManager) -> StatsSnapshot {
    m.pl_shadow_stats().expect("the flag is on: the manager holds a shadow")
}

fn new_thread(m: &ThreadManager, dir: &Path, name: &str) -> String {
    let cwd = dir.join("work").join(name);
    std::fs::create_dir_all(&cwd).unwrap();
    m.create_thread(None, ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string())).unwrap()
}

fn find(root: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

fn ledger_file(dir: &Path, id: &str) -> PathBuf {
    find(&dir.join("protoledger").join("ledgers"), &format!("{id}.jsonl")).unwrap_or_else(|| panic!("no ledger file for {id}"))
}

/// The native transcript claude wrote for `id` (under the account's dir).
fn native_file(dir: &Path, id: &str) -> PathBuf {
    find(&dir.join(ACCOUNT).join("projects"), &format!("{id}.jsonl")).unwrap_or_else(|| panic!("no native transcript for {id}"))
}

fn native_assistant_uuids(native: &Path) -> Vec<String> {
    std::fs::read_to_string(native)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["type"] == "assistant")
        .map(|v| v["uuid"].as_str().unwrap().to_string())
        .collect()
}

/// `events`' top-level assistant texts, in order — this test's own reading.
fn events_assistant_texts(m: &ThreadManager, id: &str) -> Vec<String> {
    m.store
        .list_events(id, 0, 100_000, None)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            threads::ThreadEvent::AssistantText { text, parent_tool_use_id: None, .. } => Some(text),
            _ => None,
        })
        .collect()
}

/// The ledger's active-path assistant `text` events, as (native uuid, text).
fn ledger_assistant(path: &[Event]) -> Vec<(String, String)> {
    path.iter()
        .filter(|e| e.kind == Kind::Text && e.str("role") == Some("assistant"))
        .map(|e| (e.id.split('#').next().unwrap().to_string(), e.str("text").unwrap_or_default().to_string()))
        .collect()
}

/// **N = 2 THREADS, TWO TURNS EACH: the conversation rows of `events` equal
/// the ledger's active path, each thread in its own ledger.**
///
/// Forced red (report §5): drop the carry's last line in `shadow::carry`, so
/// the ledger never receives the assistant record → the active path lacks
/// `projected reply 2`.
#[tokio::test]
async fn two_threads_each_turn_lands_in_its_own_ledger_equal_to_events() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = start(tmp.path()).await;
    let a = new_thread(&m, tmp.path(), "a");
    let b = new_thread(&m, tmp.path(), "b");
    for id in [&a, &b] {
        m.send(id, "projected one".into(), Default::default(), Value::Null, false).unwrap();
    }
    assert!(eventually(|| stats(&m).turns == 2).await, "both first turns shadowed: {:?}", stats(&m));
    for id in [&a, &b] {
        m.send(id, "projected two".into(), Default::default(), Value::Null, false).unwrap();
    }
    assert!(eventually(|| stats(&m).turns == 4).await, "all four turns shadowed: {:?}", stats(&m));
    // The ledger files first, read independently of the shadow's own
    // comparison; the shadow's counters last.
    for id in [&a, &b] {
        assert_eq!(count_kind(&m, id, "turn_ended"), 2);
        let ev = events_assistant_texts(&m, id);
        assert_eq!(ev, vec!["projected reply 1".to_string(), "projected reply 2".to_string()], "events, {id}");
        let file = ledger_file(tmp.path(), id);
        let path = read_active_path(&file).unwrap();
        let got = ledger_assistant(&path);
        let texts: Vec<String> = got.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(texts, ev, "the ledger's active path equals events' conversation rows, {id}");
        // …and they are THIS thread's records: the uuids in its own transcript.
        let uuids: Vec<String> = got.iter().map(|(u, _)| u.clone()).collect();
        assert_eq!(uuids, native_assistant_uuids(&native_file(tmp.path(), id)), "{id}'s own records");
        // cauldron's own facts went in through PL-1's writers: the composed
        // prompt of each turn and a FULL settings snapshot.
        let all = read_all(&file).unwrap();
        let prompts: Vec<&str> = all
            .iter()
            .filter(|e| e.kind == Kind::Text && e.id.starts_with("ct:prompt:"))
            .filter_map(|e| e.str("text"))
            .collect();
        assert_eq!(prompts.len(), 2, "{id}: {prompts:?}");
        assert!(prompts[0].contains("projected one") && prompts[1].contains("projected two"), "{prompts:?}");
        assert!(all.iter().any(|e| e.kind == Kind::Settings && e.id.starts_with("ct:settings:")), "{id}: a settings snapshot");
    }
    let (la, lb) = (ledger_file(tmp.path(), &a), ledger_file(tmp.path(), &b));
    assert_ne!(la, lb, "sentinel: two threads, two ledgers");
    assert_eq!(stats(&m), StatsSnapshot { turns: 4, failures: 0, diverged: 0, drift: 0 });
}

/// **A SHADOW FAILURE IS COUNTED AND THE TURN STILL SUCCEEDS.** The store
/// cannot open (a FILE sits where its directory goes), so both steps fail.
///
/// Forced red (report §5): make the before-step's failure fatal to the turn.
/// Opposition: the N = 2 test above, where a healthy store counts 0 failures.
#[tokio::test]
async fn a_shadow_failure_is_counted_and_the_turn_still_succeeds() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("protoledger"), b"not a directory").unwrap();
    let m = start(tmp.path()).await;
    let id = new_thread(&m, tmp.path(), "f");
    m.send(&id, "projected one".into(), Default::default(), Value::Null, false).unwrap();
    assert!(eventually(|| stats(&m).failures == 2).await, "before and after both failed and were counted: {:?}", stats(&m));
    assert!(eventually(|| count_kind(&m, &id, "turn_ended") == 1).await);
    assert_eq!(count_kind(&m, &id, "turn_failed"), 0, "the turn did not fail");
    assert_eq!(events_assistant_texts(&m, &id), vec!["projected reply 1".to_string()], "the turn ran and answered");
    assert_eq!(stats(&m), StatsSnapshot { turns: 0, failures: 2, diverged: 0, drift: 0 });
}

/// **A DIVERGED BINDING IS COUNTED, NEVER FATAL, AND THE THREAD IS PROJECTED
/// AGAIN.** After turn 1 the bound location loses its assistant record (an
/// edit that is not an append); turn 2 finds it diverged before re-projecting.
///
/// Forced red (report §5): drop the before-step's diverged count.
/// Opposition: the N = 2 test, where untouched locations count 0.
#[tokio::test]
async fn a_diverged_binding_is_counted_and_the_turn_still_succeeds() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = start(tmp.path()).await;
    let id = new_thread(&m, tmp.path(), "d");
    m.send(&id, "projected one".into(), Default::default(), Value::Null, false).unwrap();
    assert!(eventually(|| stats(&m).turns == 1).await, "{:?}", stats(&m));
    let loc = find(&tmp.path().join("protoledger").join("projections").join(&id).join("g0"), &format!("{id}.jsonl"))
        .expect("the first projection");
    let text = std::fs::read_to_string(&loc).unwrap();
    assert!(text.contains("projected reply 1"), "sentinel: the record to remove is there");
    let cut: String = text.split_inclusive('\n').filter(|l| !l.contains("projected reply 1")).collect();
    std::fs::write(&loc, cut).unwrap();

    m.send(&id, "projected two".into(), Default::default(), Value::Null, false).unwrap();
    assert!(eventually(|| stats(&m).turns == 2).await, "{:?}", stats(&m));
    assert_eq!(stats(&m), StatsSnapshot { turns: 2, failures: 1, diverged: 1, drift: 0 });
    assert_eq!(count_kind(&m, &id, "turn_failed"), 0);
    assert_eq!(count_kind(&m, &id, "turn_ended"), 2);
    assert!(
        find(&tmp.path().join("protoledger").join("projections").join(&id).join("g1"), &format!("{id}.jsonl")).is_some(),
        "projected again, into a new generation"
    );
    let path = read_active_path(&ledger_file(tmp.path(), &id)).unwrap();
    let texts: Vec<String> = ledger_assistant(&path).into_iter().map(|(_, t)| t).collect();
    assert_eq!(texts, events_assistant_texts(&m, &id), "the ledger still equals events");
}
