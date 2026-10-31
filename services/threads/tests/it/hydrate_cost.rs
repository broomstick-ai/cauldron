//! **THE `accounts` HYDRATE COSTS A FIXED NUMBER OF STATEMENTS.**
//!
//! `docs/shapes.md` 4.3 makes an honest, bounded read a producer's obligation at
//! hydrate. `thread_rows_non_idle` — the `accounts` shape's read for its
//! `threads` cell — owed neither: it did `list_threads(100_000)`, an unbounded
//! scan of every thread that exists, and then one `get_thread` PER SURVIVING ROW
//! to reach a single `last_seq`. A fleet scan plus an N+1, inside the path a
//! re-subscribe runs (`docs/shapes.md` 2.5), which is the path that must be
//! cheap.
//!
//! It was invisible because `thread_locks`, `parked_turns` and `retries` are all
//! empty on the operator's machine, so N was zero and the scan hid behind a warm
//! page cache. So this test does not measure wall time — wall time is exactly
//! what hid it. It counts STATEMENTS, and it asserts the only property that
//! matters: **the count does not move when the fleet grows, and it does not move
//! when the number of busy threads grows.** Two independent axes, each varied,
//! because a fix for one is not a fix for the other — the scan was flat in
//! activity and the N+1 was flat in fleet size, and the old code had both.
//!
//! **This file is its own test binary on purpose**, for the same reason
//! `derivation_cost.rs` is: `store::db_trips()` is a process-global counter, so a
//! test running beside this one would land its own statements inside the
//! measurement window. Nothing else belongs in here.

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

/// The statements ONE `thread_rows_non_idle` costs, whatever the fleet holds.
///
/// Five whole-table reads for the candidate set and its `since` stamps
/// (`thread_locks`, `parked_turns`, `retries`, the grouped `queue` count, and
/// `delivery_clocks` — so a retry cannot reset `since`, an internal thread), the
/// five `thread_states` makes, and ONE `last_seqs` for every candidate at once.
/// Every one of the ten is a whole-table or grouped read of a table that holds
/// one row per INTERESTING thread; the eleventh is bounded by
/// `MAX_NON_IDLE_HYDRATE_ROWS`.
const EXPECTED_TRIPS: u64 = 11;

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

/// One measured call: **the FLOOR of five samples, which is the call's own
/// cost.**
///
/// `db_trips()` is process-global. `create_thread` announces the thread's status
/// and the manager's own sweeps read the store, so a statement that is not this
/// call's can land inside the window — and it can only ADD. The minimum over
/// several samples is therefore the honest measurement, and it is a measurement
/// and not a hope: a genuine N+1 raises every sample, so the floor moves with it.
/// A settle between samples lets the current-thread runtime drain whatever it has
/// spawned, since a spawned task only runs at an await point.
async fn cost(m: &ThreadManager) -> (u64, usize) {
    let mut floor = u64::MAX;
    let mut rows = 0;
    for _ in 0..5 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let before = threads::store::db_trips();
        let out = m.thread_rows_non_idle().unwrap();
        floor = floor.min(threads::store::db_trips() - before);
        rows = out.rows.len();
    }
    (floor, rows)
}

#[tokio::test]
async fn the_accounts_hydrate_is_flat_in_the_fleet_and_in_the_busy_count() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path());

    // ── A: 64 threads, NONE busy. The fleet is real; the answer is empty. ────
    let ids = make_threads(&m, tmp.path(), 64);
    let (cost_64_idle, rows) = cost(&m).await;
    assert_eq!(
        rows, 0,
        "no thread holds a lock, a park, a retry or a queue row"
    );

    // ── B: same fleet, EIGHT busy. Varying the ACTIVITY axis. ───────────────
    for id in &ids[..8] {
        arm_retry(&m, id);
    }
    let (cost_64_busy8, rows) = cost(&m).await;
    assert_eq!(rows, 8, "eight armed retries are eight non-idle rows");

    // ── C: same fleet, TWENTY-FOUR busy. Activity axis again, tripled. ──────
    for id in &ids[8..24] {
        arm_retry(&m, id);
    }
    let (cost_64_busy24, rows) = cost(&m).await;
    assert_eq!(rows, 24);

    // ── D: DOUBLE the fleet, busy count unchanged. The other axis. ──────────
    let _more = make_threads(&m, tmp.path(), 64);
    let (cost_128_busy24, rows) = cost(&m).await;
    assert_eq!(rows, 24, "the 64 new threads are idle and must not appear");

    assert_eq!(
        (cost_64_idle, cost_64_busy8, cost_64_busy24, cost_128_busy24),
        (
            // NOTHING is busy, so there is no candidate to ask about and
            // `last_seqs` acquires no connection at all — which is the one
            // statement of the eleven that is allowed to be absent, and asserting it
            // separately is what proves `last_seqs` is ONE trip and not one per
            // id.
            EXPECTED_TRIPS - 1,
            EXPECTED_TRIPS,
            EXPECTED_TRIPS,
            EXPECTED_TRIPS
        ),
        "a hydrate's read cost must not depend on the fleet size OR on how many \
         threads are busy. The shape this replaced was 9 + N: an unbounded \
         `list_threads(100_000)` plus one `get_thread` per non-idle row, so 24 \
         busy threads cost 33 statements and 128 idle ones were read and thrown \
         away. Measured now: {cost_64_idle} idle/64, {cost_64_busy8} busy8/64, \
         {cost_64_busy24} busy24/64, {cost_128_busy24} busy24/128."
    );

    // ── AND THE ROWS ARE THE SAME ROWS. ─────────────────────────────────────
    //
    // A cheap read that answers differently is not a fix. `unread` is the field
    // the deleted `get_thread` existed to feed, so it is the one that would go
    // silently wrong if `last_seqs` disagreed with it.
    let out = m.thread_rows_non_idle().unwrap();
    assert!(out.truncated.is_none(), "24 is far below the cap");
    for r in &out.rows {
        let row = m.store.get_thread(&r.thread_id).unwrap().unwrap();
        assert_eq!(
            r.unread,
            row.last_seq.saturating_sub(0),
            "`unread` must be the same watermark the per-thread read gave: {r:?}"
        );
        assert_eq!(r.state, "retrying");
    }
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
