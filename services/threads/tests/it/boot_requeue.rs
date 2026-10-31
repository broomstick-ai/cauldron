//! **L-1: A TURN CUT OFF BY A REBOOT IS RETRIED, NOT SILENTLY DROPPED.**
//!
//! GAP-AUDIT L-1. The machine reboots mid-turn. At boot, `recover` finds the
//! `running_turns` row, sees the child's pid is dead, logs `daemon_restart` and
//! deletes the row. Before this fix it did nothing else: the queue rows stayed
//! `delivered` (the stranded-delivery sweep skips them, because a
//! `turn_started` names them), the thread read idle, and nobody answered.
//! That broke `retry::ends_the_delivery`, which says `DaemonRestart` is a GATE
//! and must never end the delivery it cut off.
//!
//! The two tests are a pair across the ceiling, so neither "always retry" nor
//! "always fail" passes both:
//! * one rung below the ceiling → a retry row at `attempt + 1`, thread
//!   `retrying`, no `turn_failed`;
//! * at the ceiling → no retry row, a visible `turn_failed`, and the queue
//!   row moved to `failed` (re-selectable verbatim).
//!
//! The restart is a REAL one as far as the manager can tell: the rows are
//! written by one manager, it is dropped, and a second manager is built on the
//! same data dir and runs `recover`. The dead child is a real process this
//! test spawned and reaped, so `pid_alive` answers from the OS.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use threads::event::{StoredEvent, ThreadEvent};
use threads::settings::ThreadSettings;
use threads::store::RunningTurnRow;
use threads::{build_manager, Config, ThreadManager};

const WORDS: &str = "the operator's words, cut off by a reboot";
const MSG: u64 = 7;

fn set_env() {
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake);
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &Path) -> Config {
    let mut map = BTreeMap::new();
    let cd = dir.join("solo");
    std::fs::create_dir_all(&cd).unwrap();
    map.insert("solo".to_string(), cd.to_string_lossy().to_string());
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts: map,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s.account = threads::settings::AccountChoice::Named("solo".into());
    s
}

/// A pid that WAS a real child and is now gone (spawned, exited, reaped).
fn dead_pid() -> i64 {
    let mut c = std::process::Command::new("true").spawn().unwrap();
    c.wait().unwrap();
    c.id() as i64
}

/// The previous daemon's state at the instant the machine went down: message
/// `MSG` queued and delivered, a `turn_started` naming it on the ledger, and a
/// `running_turns` row for a child that the reboot killed. Returns the thread
/// id and a manager built FRESH on the same data dir — the "next boot".
fn previous_daemon_died_mid_turn(dir: &Path, attempt: u32) -> (String, ThreadManager) {
    let id = {
        let m1 = build_manager(config_at(dir)).unwrap();
        let id = m1.create_thread(None, settings(dir)).unwrap();
        let qid = m1
            .store
            .enqueue(&id, MSG, WORDS, "immediate", &serde_json::Value::Null, &[])
            .unwrap();
        m1.store.mark_delivered(&[qid]).unwrap();
        let seq = m1.store.max_seq(&id).unwrap() + 1;
        m1.store
            .append_event(&StoredEvent {
                thread_id: id.clone(),
                seq,
                message_id: None,
                ts: threads::store::now_rfc3339(),
                event: ThreadEvent::TurnStarted {
                    attempt,
                    message_ids: vec![MSG],
                },
            })
            .unwrap();
        m1.store
            .upsert_running_turn(&RunningTurnRow {
                thread_id: id.clone(),
                pid: dead_pid(),
                spawn_cookie: "cookie".into(),
                output_path: dir.join("orphan.jsonl").to_string_lossy().to_string(),
                byte_offset: 0,
                started_at: threads::store::now_rfc3339(),
                prompt: Some(WORDS.to_string()),
                message_ids: vec![MSG],
                attempt,
            })
            .unwrap();
        id
    };
    let m2 = build_manager(config_at(dir)).unwrap();
    (id, m2)
}

fn ledger_kinds(m: &ThreadManager, id: &str) -> Vec<String> {
    m.store
        .list_events(id, 0, 10_000, None)
        .unwrap()
        .iter()
        .map(|e| e.event.kind().to_string())
        .collect()
}

/// **One rung below the ceiling: the reboot-killed turn goes back on the
/// ladder at `attempt + 1`.** Red at 97614bfd: no retry row, thread `idle`.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_killed_by_a_reboot_is_scheduled_for_retry_at_the_next_attempt() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let below = threads::retry::DEFAULT_MAX_TRANSIENT_ATTEMPTS - 1;
    let (id, m) = previous_daemon_died_mid_turn(tmp.path(), below);

    m.recover().await;

    assert!(
        m.store.get_running_turn(&id).unwrap().is_none(),
        "the dead row is cleared"
    );
    let kinds = ledger_kinds(&m, &id);
    assert!(
        kinds.iter().any(|k| k == "turn_interrupted"),
        "daemon_restart recorded: {kinds:?}"
    );
    let retry = m.store.get_retry(&id).unwrap().unwrap_or_else(|| {
        panic!(
            "L-1: a turn cut off by a reboot must land on the retry ladder; no retry row. \
             thread state = {:?}, ledger = {kinds:?}",
            m.thread_state(&id).map(|s| s.state)
        )
    });
    assert_eq!(retry.prompt, WORDS, "the prompt is carried verbatim");
    assert_eq!(retry.message_ids, vec![MSG], "with the delivery it answers");
    assert_eq!(
        retry.attempt,
        below + 1,
        "continuing the dead execution's ladder"
    );
    assert!(
        kinds.iter().any(|k| k == "retry_scheduled"),
        "announced: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|k| k == "turn_failed"),
        "below the ceiling nothing fails: {kinds:?}"
    );
    let st = m.thread_state(&id).unwrap();
    assert_eq!(st.state, "retrying", "the thread reads retrying, not idle");
    assert!(
        m.store.failed_deliveries(&id).unwrap().is_empty(),
        "the delivery is still live, not given up"
    );
}

/// **At the ceiling: no retry, a visible `turn_failed`, and the prompt kept.**
/// The pair to the test above (doctrine 2): "always retry" fails here.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_killed_by_a_reboot_at_the_ceiling_fails_visibly_and_keeps_the_prompt() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let at = threads::retry::DEFAULT_MAX_TRANSIENT_ATTEMPTS;
    let (id, m) = previous_daemon_died_mid_turn(tmp.path(), at);

    m.recover().await;

    let kinds = ledger_kinds(&m, &id);
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "at the ceiling there is no further retry: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| k == "turn_failed"),
        "L-1: an exhausted delivery fails VISIBLY: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|k| k == "retry_scheduled"),
        "and is not retried: {kinds:?}"
    );
    let failed = m.store.failed_deliveries(&id).unwrap();
    assert_eq!(
        failed
            .iter()
            .map(|r| (r.message_id, r.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(MSG, WORDS)],
        "the queue row moved delivered → failed, verbatim and re-selectable"
    );
    assert_ne!(m.thread_state(&id).unwrap().state, "retrying");
}

// R9.F2b: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
