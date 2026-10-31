//! **A TURN IS NEVER DESTROYED UNDER A SENTENCE THAT CONTRADICTS ITSELF.**
//!
//! `claude` can end a run with a `result` line that says `subtype: "success"`
//! and `is_error: true` at the same time. The daemon classified that by
//! SUBTYPE — `classify_result`, whose doc comment asserted "`success` never
//! reaches here" — and the catch-all turned it into a fatal `claude_error`
//! whose whole message was `result subtype success`.
//!
//! It reached there 46 times in the operator's own ledger between
//! 2026-08-03 and 2026-08-18, twenty-one of them on the 18th alone, across at
//! least a dozen threads (`966825e8`, `fc41160e`, `929aa8d5`, `18646f20`,
//! `f651b713`, `381f8428`, `294b5efb`, `08b1426d`, `ade9ea22`, `d0f659e9`,
//! `20dae0f0`, `e0c099d0`). Every one was a real turn thrown away: `Fatal`
//! clears the retry row, so his prompt went with it.
//!
//! And the ledger says what those runs actually were. The last thing on the
//! stream before the flagged result was, in all 46 cases, a named failure:
//! `API Error: 529 Overloaded` (after the CLI exhausted its own ten internal
//! `api_retry` attempts), `Unable to connect to API (ENOTFOUND)`,
//! `Connection closed mid-response`, `500 Internal server error`, or
//! `Prompt is too long`. The subtype knew none of that. So the fix reads the
//! cause off the stream and hands it to the same classifier a non-zero exit
//! goes through — and when nothing is recognizable, it retries within bounds
//! rather than destroying the turn.
//!
//! Every test drives the manager in-process against the FAKE claude fixture.
//! No real claude turn is ever spawned; the manager gets an explicit
//! `data_dir` under a tempdir and `THREADS_CLAUDE_HOME` points transcript
//! storage at a scratch dir, so nothing here touches the operator's own
//! `~/.claude`, the running daemon, or the keychain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use threads::event::{Push, StoredEvent, ThreadEvent};
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env() {
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "200");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &Path) -> Config {
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        dir.join("aa-owned").to_string_lossy().to_string(),
    );
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

fn watch(m: &ThreadManager) -> Arc<Mutex<Vec<StoredEvent>>> {
    let mut rx = m.subscribe();
    let seen = Arc::new(Mutex::new(Vec::<StoredEvent>::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Event(ev) = p {
                out.lock().unwrap().push(ev);
            }
        }
    });
    seen
}

fn saw(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> bool {
    seen.lock().unwrap().iter().any(|e| e.event.kind() == kind)
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..400 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

/// **The turn survives, and the operator's words survive with it.**
///
/// The fixture reproduces thread `fc41160e` at 2026-08-18T17:45:00Z exactly:
/// an overload printed as assistant text, then a result calling itself a
/// success and flagging itself an error, then exit 1.
///
/// Before the fix this produced `turn_failed{error_kind: "claude_error",
/// message: "result subtype success"}`, no retry row, and the prompt gone.
/// After it, the 529 is classified for what it is and the work is held.
#[tokio::test]
async fn a_success_result_flagged_as_an_error_does_not_discard_the_turn() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    m.send(
        &id,
        "errorresult please — this is the operator's real turn".into(),
        Default::default(),
        json!({}),
        false,
    )
    .unwrap();

    assert!(
        eventually(|| saw(&seen, "retry_scheduled")).await,
        "a result that contradicts itself must be RETRIED, never discarded: {:?}",
        seen.lock()
            .unwrap()
            .iter()
            .map(|e| e.event.kind())
            .collect::<Vec<_>>()
    );

    // THE PROMPT IS STILL HELD. This is the whole of it — `Fatal` clears the
    // retry row, and 46 turns went that way.
    let row = m
        .store
        .get_retry(&id)
        .unwrap()
        .expect("the operator's turn is held on the retry row, not thrown away");
    assert!(
        row.prompt.contains("the operator's real turn"),
        "his words must be exactly what is re-delivered, got {:?}",
        row.prompt
    );

    // …and it is held as the failure it actually was: a 5xx overload, read off
    // the stream, not a fatal classification invented from the subtype.
    assert_eq!(
        row.kind, "transient",
        "a 529 after the CLI burned its own retries is an overload"
    );

    // The tautology is gone from the ledger entirely.
    let bad: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            ThreadEvent::TurnFailed { message, .. } => Some(message.clone()),
            _ => None,
        })
        .filter(|msg| msg.contains("result subtype success"))
        .collect();
    assert!(
        bad.is_empty(),
        "no turn may ever be failed under the words 'result subtype success': {bad:?}"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
