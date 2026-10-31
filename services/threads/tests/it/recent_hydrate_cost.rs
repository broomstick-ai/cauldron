//! **THE `threads/recent-updates` HYDRATE COSTS `2 + RECENT_CAPACITY`
//! STATEMENTS, WHATEVER THE FLEET HOLDS.**
//!
//! **This file is its own test binary on purpose**, for the same reason
//! `hydrate_cost.rs`, `derivation_cost.rs` and `thread_hydrate_cost.rs` are:
//! `store::db_trips()` is a process-global counter, so a test running beside
//! this one would land its own statements inside the measurement window.
//! Nothing else belongs in here.
//!
//! **What `2 + N` is, stated rather than asserted as a magic number:**
//!
//! | statements | read | what |
//! |---|---|---|
//! | 1 | `recent_threads(N)` | `SELECT id, last_user_input_at FROM threads WHERE last_user_input_at IS NOT NULL ORDER BY last_user_input_at DESC LIMIT N` — two columns, never a row |
//! | 1 | `list_thread_seen()` | every seen cursor in one scan of a small one-row-per-*interesting*-thread table |
//! | N | `unread_count(id, seen)` | one index range on `(thread_id, seq)` per ENTRY |
//!
//! **Why the middle one is a batch and the last one is not.** The seen cursors
//! are one small table and a whole-table read is cheaper than N primary-key
//! lookups through one serialized connection — the same trade `thread_states`
//! already makes. The unread counts are N index ranges over a 2.46 M-row table
//! and there is no batch form of them that is not a scan of the whole ledger, so
//! the bound is `2 + N` and the DESIGN LEVER IS N.
//!
//! **Which is why N is 32 and not 500.** `RECENT_CAPACITY` is the only term here
//! that grows. At `MAX_ROWS_PER_SUBSCRIBE`'s 500 a cold subscribe would be 502
//! statements, and the honest caveat below makes each of those 500 potentially
//! large. 32 is the window size this daemon already calls a window
//! (`accounts.moves`, `threads.status`) and is comfortably above the operator's
//! *"the twenty most recently updated"*.
//!
//! **The honest caveat, stated where the number is.** A flat STATEMENT count is
//! not a flat row count, and one of the `2 + N` is not O(1) in rows:
//! `unread_count` counts addressable events above the seen cursor, so a thread
//! never marked seen pays a scan of its own addressable tail — 177,995 index
//! entries on the largest thread in the production store. It is the same
//! statement `threads.status` and `threads`' own per-key hydrate have always
//! answered with, on the same index; making it O(1) needs a `seen_message_id`
//! column, which is a migration and out of scope. The test varies the log length
//! by 100× precisely so this is visible as a row cost and not hidden as a
//! statement one.
//!
//! **Both forced red.** The mutations are in each test's docstring.

use threads::shape::Handles;
use threads::shapes::recent::{RecentOrder, RECENT_CAPACITY};
use threads::{build_manager, Config, ThreadManager, ThreadSettings};

// ── THE SERIAL GATE ──────────────────────────────────────────────────────────
//
// **`store::db_trips()` is a PROCESS-GLOBAL counter**, and both tests in this
// binary measure statement counts as deltas across it. `cargo test` runs a
// binary's tests on parallel threads, so each reads the other's queries as its
// own and the measurement is decided by scheduling: this file passed alone,
// passed with `--test-threads=1`, and failed in the default parallel run.
//
// The dispatch that wrote these tests NAMED this hazard — "db_trips() is
// process-global, so a statement-count test cannot share a binary with anything
// that queries" — and then put two of them in one binary. The rule is right; the
// second test is what broke it.
//
// This is the same fix `self_update` carries, for the same reason. Other test
// binaries stay parallel; this file loses concurrency it never truly had.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn set_env() {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-recent-hydrate-home"),
    );
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
}

fn manager(dir: &std::path::Path) -> ThreadManager {
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        dir.join("aa-owned").to_string_lossy().to_string(),
    );
    // No `serve()` and no `recover()`: both start watchers that would touch the
    // database inside the measurement window.
    // THE CLOCK SEAM. Every `Hub::emit` stamps with the clock and every cell
    // refuses an input at or before its `folded_at`, so two emits in one
    // microsecond mean the second is DROPPED — which is what an optimized
    // build does to a tight emit loop. See `src/clock.rs`.
    threads::clock::testing::ticking();
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

/// **EVERY FIXTURE THREAD HAS BEEN SPOKEN IN**, because the read being measured
/// is `SELECT ... WHERE last_user_input_at IS NOT NULL` — the recents order is
/// *"last user input, not... any other events, including renames"*, so a thread
/// nobody has spoken in holds no place in it and would contribute no
/// `unread_count` statement to measure. A fixture that only CREATED threads would
/// quietly measure the empty case and the `2 + N` claim would be `2 + 0`.
///
/// The stamp is written through `Store::bump_last_seq` with a `Some(..)` — the
/// SAME statement, with the same argument, that `ThreadManager::emit_resolving`
/// runs for a real `user_message` append. It is outside every `cost()` window.
fn make_threads(m: &ThreadManager, dir: &std::path::Path, n: usize) -> Vec<String> {
    (0..n)
        .map(|_| {
            let mut s = ThreadSettings::new(dir.to_string_lossy().to_string(), "fable".to_string());
            s.injection = threads::settings::Injection::NOTHING;
            let id = m.create_thread(None, s).unwrap();
            let spoke_at = chrono::Utc::now().to_rfc3339();
            m.store.bump_last_seq(&id, 0, Some(&spoke_at)).unwrap();
            id
        })
        .collect()
}

/// One measured call: **the FLOOR of five samples, which is the call's own
/// cost.** `db_trips()` is process-global and a stray statement can only ADD, so
/// the minimum over several samples is the honest measurement — and a genuine
/// N+1 raises every sample, so the floor moves with it.
fn cost(m: &ThreadManager) -> u64 {
    let mut floor = u64::MAX;
    for _ in 0..5 {
        let before = threads::store::db_trips();
        let _ = m.hydrate(RecentOrder);
        floor = floor.min(threads::store::db_trips() - before);
    }
    floor
}

/// **THE BOUND, AND IT IS `2 + ENTRIES` RATHER THAN `2 + CAPACITY`.**
///
/// A fleet SMALLER than the capacity pays one `unread_count` per thread that
/// exists, not per slot — which is stated because "N" in the module doc's table
/// is the number of ENTRIES the read returns, and a reader could take it for the
/// const.
///
/// **THE MUTATION.** In `Handles<RecentOrder>`, replace the one
/// `list_thread_seen()` with a per-entry `last_seen_seq(&thread_id)` — the N+1
/// that `thread_states` was batched to avoid and that the `accounts` shape
/// actually shipped once. It turns `2 + N` into `1 + 2N`.
///
/// Forced red:
/// ```text
/// the hydrate is 2 statements plus one per ENTRY: 13 for 6 threads
/// ```
#[test]
fn the_index_hydrate_is_two_statements_plus_one_per_entry() {
    let _serial = serial();
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path());

    let small = 6;
    make_threads(&m, tmp.path(), small);
    let got = cost(&m);
    assert_eq!(
        got,
        2 + small as u64,
        "the hydrate is 2 statements plus one per ENTRY: {got} for {small} threads"
    );
}

/// **FLAT IN THE FLEET, AND FLAT IN THE ACTIVITY LEVEL.**
///
/// Two axes, varied separately, because a fix for one is not a fix for the other
/// and the `accounts` defect had one of each.
///
/// * **Fleet.** 8 threads and then 80. The read is `ORDER BY updated_at DESC
///   LIMIT 32`, so past the capacity the statement count STOPS MOVING — that is
///   what makes this affordable at 5,417 and at 500,000, and it is the property
///   `list_threads(100_000)` did not have.
/// * **Activity.** The same fleet with a log 100× longer. The statement count
///   must not move at all; the ROW cost of `unread_count` does, and the module
///   doc says so where the number is rather than leaving it to be discovered.
///
/// **THE MUTATION.** In `store::recent_threads`, drop the `LIMIT ?1` — the
/// unbounded read, which is the whole class this test exists for. The row count is then
/// the fleet and the per-entry `unread_count` runs once per THREAD.
///
/// Forced red:
/// ```text
/// past the capacity the hydrate STOPS growing with the fleet: 34 at 8 threads,
/// 82 at 80
/// ```
#[test]
fn the_index_hydrate_is_flat_in_the_fleet_and_in_the_activity_level() {
    let _serial = serial();
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path());

    // ── A: a fleet at the capacity, short logs. ─────────────────────────────
    let ids = make_threads(&m, tmp.path(), RECENT_CAPACITY);
    let at_capacity = cost(&m);
    assert_eq!(
        at_capacity,
        2 + RECENT_CAPACITY as u64,
        "a fleet exactly at the capacity is the full bound: {at_capacity}"
    );

    // ── B: the fleet 2.5× bigger. The bound must not move. ──────────────────
    make_threads(&m, tmp.path(), RECENT_CAPACITY * 3 / 2);
    let bigger_fleet = cost(&m);
    assert_eq!(
        bigger_fleet, at_capacity,
        "past the capacity the hydrate STOPS growing with the fleet: \
         {at_capacity} at {} threads, {bigger_fleet} at {}",
        RECENT_CAPACITY,
        RECENT_CAPACITY * 5 / 2
    );

    // ── C: the ACTIVITY axis — the same fleet, a log 100× longer. ───────────
    //
    // A read that walked the log, or that asked per event, would move here and
    // not in B.
    for i in 0..200 {
        m.append_custom(
            &ids[0],
            "assistant_suggestions".into(),
            serde_json::json!({"i": i}),
            i % 2 == 0,
        )
        .unwrap();
    }
    let busy = cost(&m);
    assert_eq!(
        busy, at_capacity,
        "and flat in the activity level — a 100× longer log is the same \
         STATEMENT count: {busy}"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
