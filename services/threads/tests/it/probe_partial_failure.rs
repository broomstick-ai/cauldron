//! **ONE ACCOUNT FAILING ITS `/usage` POLL IS NEWS EVEN WHEN ITS SIBLINGS
//! SUCCEED.**
//!
//! `probe_accounts_once` collected a reason per failed account and then
//! returned the collection ONLY on the `n == 0` branch — every account failed,
//! or nobody hears about any of it. So a login that had stopped answering was
//! indistinguishable, at every log level and through every API, from one
//! polling cleanly, for as long as one sibling kept working. That is exactly
//! how the free `alice-example-com` account stayed invisible
//! (`reports/0093-stability-threads.md`, F6).
//!
//! The fix is observability only: the per-account reason is logged at WARN as
//! it happens and kept on `last_probe_failures()`, and `gauge.probe` carries it
//! on the success arm. Nothing about WHICH accounts poll, or what a poll does,
//! changes — which is what the second assertion here is for.

use std::collections::BTreeMap;

use threads::{build_manager, Config};

fn set_env() {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

#[tokio::test]
async fn a_failing_account_is_reported_even_when_a_sibling_polls_fine() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();

    // TWO accounts — the minimum that can show the defect at all. With one
    // account a failure IS `n == 0` and the old code reported it; the bug only
    // exists from two upwards, so one account could never have caught it.
    let mut accounts = BTreeMap::new();
    for name in ["aa-healthy", "zz-broken"] {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        accounts.insert(name.to_string(), dir.to_string_lossy().to_string());
    }
    // The fixture answers NOTHING for an account whose own config dir holds
    // this marker, which is what a login that cannot be polled looks like.
    std::fs::write(tmp.path().join("zz-broken/usage-fail"), b"").unwrap();

    let m = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();

    let n = m.probe_accounts_once(true).await.unwrap();

    // The healthy sibling still polls and still records all three percentage
    // axes: the failure must not cost the sweep anything.
    assert_eq!(
        n, 3,
        "the healthy account's three axes must still land — a partial failure \
         is not allowed to abort the sweep"
    );

    let failures = m.last_probe_failures();
    assert_eq!(
        failures.len(),
        1,
        "exactly one account failed and its reason must survive the sweep, not \
         be discarded because a sibling succeeded; got {failures:?}"
    );
    assert!(
        failures[0].starts_with("zz-broken:"),
        "the reason must NAME the account that failed, or it cannot be acted \
         on; got {:?}",
        failures[0]
    );

    // And a clean sweep clears it, so the record is the CURRENT state of the
    // fleet rather than a graveyard that never empties.
    std::fs::remove_file(tmp.path().join("zz-broken/usage-fail")).unwrap();
    let n = m.probe_accounts_once(true).await.unwrap();
    assert_eq!(n, 6, "both accounts poll once the marker is gone");
    assert!(
        m.last_probe_failures().is_empty(),
        "a recovered account must clear itself from the failure record"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
