//! **WARN-1 N-2 — A `/usage` PROBE THAT KEEPS FAILING IS ONE WARNING PER
//! EPISODE, NOT ONE PER SWEEP.**
//!
//! an internal report §3 K3: `runtime.rs`'s "usage probe FAILED for
//! this account" warned once per account per sweep, so an account that stayed
//! broken re-warned forever. Now: WARN on an account's FIRST failure, WARN once
//! on its recovery, INFO between. The per-sweep REASON is unchanged on
//! `last_probe_failures()` (asserted, so the observability
//! `probe_partial_failure.rs` guards is not traded away).
//!
//! N = 2 broken accounts plus one healthy sibling. Warnings are counted with a
//! thread-local capture (`support/capture.rs`); `#[tokio::test]` is a
//! current-thread runtime, so `probe_set`'s own `warn!` runs on this thread.

#[path = "support/capture.rs"]
mod capture;

use std::collections::BTreeMap;

use threads::{build_manager, Config};

fn set_env() {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    std::env::set_var("THREADS_CLAUDE_HOME", std::env::temp_dir().join("threads-tests-claude-home"));
}

fn count(recs: &std::sync::Mutex<Vec<capture::Rec>>, needle: &str, account: &str) -> usize {
    recs.lock()
        .unwrap()
        .iter()
        .filter(|r| r.target == "threads::runtime" && r.level == tracing::Level::WARN)
        .filter(|r| r.message.contains(needle))
        .filter(|r| r.fields.get("account").map(String::as_str) == Some(account))
        .count()
}

#[tokio::test]
async fn a_failing_account_warns_on_its_first_failure_and_on_recovery_only_n2() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = ["aa-healthy", "zz-broken-1", "zz-broken-2"];
    let mut accounts = BTreeMap::new();
    for name in names {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        accounts.insert(name.to_string(), dir.to_string_lossy().to_string());
    }
    let marker = |n: &str| tmp.path().join(n).join("usage-fail");
    for n in ["zz-broken-1", "zz-broken-2"] {
        std::fs::write(marker(n), b"").unwrap();
    }
    let m = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();
    let (recs, _g) = capture::install();
    const FAILED: &str = "usage probe FAILED";
    const RECOVERED: &str = "usage probe RECOVERED";

    // Three sweeps with both broken accounts failing every time.
    for sweep in 1..=3 {
        m.probe_accounts_once(true).await.unwrap();
        // The reason is still reported on EVERY sweep — only the WARN is deduped.
        let f = m.last_probe_failures();
        assert_eq!(f.len(), 2, "sweep {sweep}: both broken accounts' reasons must be kept: {f:?}");
    }
    for n in ["zz-broken-1", "zz-broken-2"] {
        assert_eq!(count(&recs, FAILED, n), 1, "{n}: a standing failure warned once per sweep");
    }
    assert_eq!(count(&recs, FAILED, "aa-healthy"), 0, "the healthy sibling was warned");

    // zz-broken-1 recovers: one recovery warning, then silence.
    std::fs::remove_file(marker("zz-broken-1")).unwrap();
    m.probe_accounts_once(true).await.unwrap();
    m.probe_accounts_once(true).await.unwrap();
    assert_eq!(count(&recs, RECOVERED, "zz-broken-1"), 1, "the recovery must be said exactly once");
    assert_eq!(count(&recs, RECOVERED, "zz-broken-2"), 0, "a still-failing account did not recover");
    assert_eq!(count(&recs, RECOVERED, "aa-healthy"), 0, "an account that never failed has nothing to recover from");
    assert_eq!(count(&recs, FAILED, "zz-broken-2"), 1, "still one warning for the account still failing");

    // And it breaks again: a NEW episode, so a new first-failure warning.
    std::fs::write(marker("zz-broken-1"), b"").unwrap();
    m.probe_accounts_once(true).await.unwrap();
    assert_eq!(count(&recs, FAILED, "zz-broken-1"), 2, "a second episode is news again");
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
