//! **THE `accounts` HYDRATE IS BOUNDED, AND SAYS SO WHEN THE BOUND BITES.**
//!
//! The companion to `hydrate_cost.rs`, which measures the read's COST. This file
//! is about its HONESTY: `MAX_NON_IDLE_HYDRATE_ROWS` caps what one hydrate
//! carries, and `docs/shapes.md` 4.3 says a short read must present itself as
//! `Partial` — a statement about the read — never as `Complete`, which is a
//! statement about the world and would be a handler that lied.
//!
//! Kept OUT of `hydrate_cost.rs` deliberately, and that separation is itself a
//! finding: `store::db_trips()` is a process-global counter, tests in one binary
//! run in parallel by default, and these tests' own several hundred statements
//! landed inside that file's measurement window and moved its floor. A
//! trip-count measurement cannot share a binary with anything at all.

use threads::store::RetryRow;
use threads::{build_manager, Config, ThreadManager, ThreadSettings};

fn set_env() {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-hydrate-home"),
    );
}

fn manager(dir: &std::path::Path) -> ThreadManager {
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        dir.join("aa-owned").to_string_lossy().to_string(),
    );
    // No `serve()` and no `recover()`: both start background watchers that would
    // touch the database inside the measurement window.
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

fn make_threads(m: &ThreadManager, dir: &std::path::Path, n: usize) -> Vec<String> {
    (0..n)
        .map(|_| {
            let mut s = ThreadSettings::new(dir.to_string_lossy().to_string(), "fable".to_string());
            s.injection = threads::settings::Injection::NOTHING;
            m.create_thread(None, s).unwrap()
        })
        .collect()
}

/// Arm a retry, which is one of the four things that make a thread non-idle —
/// and the cheapest to fabricate without running a turn.
fn arm_retry(m: &ThreadManager, id: &str) {
    m.store
        .upsert_retry(&RetryRow {
            thread_id: id.to_string(),
            prompt: "try me again".into(),
            message_ids: vec![1],
            attempt: 1,
            kind: "rate_limit".into(),
            reason: "the account said no".into(),
            // Far future, so nothing can decide the floor elapsed mid-test.
            account: None,
            created_at: String::new(),
        })
        .unwrap();
}

/// **THE CAP IS REPORTED AS `Partial`, NOT SWALLOWED AS `Complete`.**
///
/// `docs/shapes.md` 4.3: "A `Complete(vec![])` for a populated set is a handler
/// that lied, and only a test can catch it." The same is true of a
/// `Complete(vec![…512 of 900])`. The cap is 512, so this drives the boundary by
/// arming more retries than that and asserting the verdict, not the length.
#[tokio::test]
async fn a_truncated_hydrate_is_partial_and_says_how_much_is_missing() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path());

    let cap = threads::runtime::MAX_NON_IDLE_HYDRATE_ROWS;
    let ids = make_threads(&m, tmp.path(), cap + 3);
    for id in &ids {
        arm_retry(&m, id);
    }

    let out = m.thread_rows_non_idle().unwrap();
    assert_eq!(out.rows.len(), cap, "the read carries exactly the cap");
    let why = out
        .truncated
        .expect("over the cap, so the read must say it is not a whole answer");
    assert!(
        why.contains(&format!("{}", cap + 3)) && why.contains('3'),
        "the reason must name how many there were and how many are missing: {why}"
    );

    // ── AND THE SHAPE'S HANDLER ANSWERS `Partial`, NOT `Complete`. ──────────
    //
    // The struct's flag is not the contract; what the fold sees is. A
    // `Complete(512 of 515)` would mark every key `complete: true` on the wire
    // and no consumer could tell it had been short-changed.
    use threads::shape::{Handles, Hydration};
    match Handles::<threads::shapes::accounts::ThreadRows>::hydrate(
        &m,
        threads::shapes::accounts::ThreadRows,
    ) {
        Hydration::Partial { got, reason } => {
            assert_eq!(got.len(), cap);
            assert!(reason.contains(&format!("{}", cap + 3)), "{reason}");
        }
        other => panic!(
            "a truncated read must be Partial — a statement about the READ, \
             docs/shapes.md 3.8. Got: {}",
            match other {
                Hydration::Complete(v) => format!("Complete({} rows)", v.len()),
                Hydration::Empty { reason } => format!("Empty({reason})"),
                Hydration::Partial { .. } => unreachable!(),
            }
        ),
    }

    drop(tmp);
}

/// The other side of the boundary: UNDER the cap is a whole answer, so the flag
/// is not something a busy fleet sets by being busy.
#[tokio::test]
async fn a_bounded_hydrate_under_the_cap_is_a_whole_answer() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path());
    let ids = make_threads(&m, tmp.path(), 4);
    for id in &ids {
        arm_retry(&m, id);
    }
    let out = m.thread_rows_non_idle().unwrap();
    assert_eq!(out.rows.len(), 4);
    assert!(
        out.truncated.is_none(),
        "under the cap is Complete, not Partial: {:?}",
        out.truncated
    );
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
