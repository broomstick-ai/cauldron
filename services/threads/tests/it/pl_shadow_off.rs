//! **PL-3: FLAG OFF (the default) ⇒ NO STORE WRITES.** Its own binary, so no
//! flag-on test shares this process's environment.
//!
//! Negative test, so it outlasts the longest period in its path (doctrine 4):
//! the shadow's writes happen INSIDE `run_turn` (before the spawn, and after
//! the last attempt but before the turn releases the thread), so a SECOND
//! turn on the same thread cannot start until the first turn's shadow steps
//! would have run. The test waits for that second turn to end.
//!
//! Forced red (report §5): `Shadow::from_flag` ignores the flag (always on).
//! Opposition: `tests/pl_shadow.rs`, flag on, finds the store and its ledgers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

#[path = "support/first_exec.rs"]
mod first_exec;

#[tokio::test]
async fn flag_off_writes_no_store_across_two_full_turns() {
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake);
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var("THREADS_CLAUDE_HOME", std::env::temp_dir().join("threads-tests-claude-home"));
    std::env::remove_var("THREADS_CHASSIS_PORT");
    std::env::remove_var("THREADS_PL_SHADOW");
    let tmp = tempfile::tempdir().unwrap();
    let dir: &Path = tmp.path();
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert("aa-owned".to_string(), dir.join("aa-owned").to_string_lossy().to_string());
    let m: ThreadManager = build_manager(Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();
    let (_addr, h) = threads::chassis::serve(std::sync::Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap()).await.unwrap();
    std::mem::forget(h);
    let cwd = dir.join("work").join("x");
    std::fs::create_dir_all(&cwd).unwrap();
    let id = m.create_thread(None, ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string())).unwrap();
    let ended = |m: &ThreadManager| {
        m.store.list_events(&id, 0, 100_000, None).unwrap().iter().filter(|e| e.event.kind() == "turn_ended").count()
    };
    m.send(&id, "projected one".into(), Default::default(), Value::Null, false).unwrap();
    for _ in 0..800 {
        if ended(&m) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    m.send(&id, "projected two".into(), Default::default(), Value::Null, false).unwrap();
    for _ in 0..800 {
        if ended(&m) == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(ended(&m), 2, "sentinel: both turns ran (they wrote a native transcript)");
    assert!(!dir.join("protoledger").exists(), "flag off: no protoledger store was created under {}", dir.display());
    assert!(m.pl_shadow_stats().is_none(), "flag off: the manager holds no shadow");
}
