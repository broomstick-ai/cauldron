//! **THE DERIVATION COSTS FIVE TRIPS, WHATEVER N IS.**
//!
//! The reason "just derive it" was not obviously right is that the per-thread
//! derivation was genuinely expensive: `thread_status` made SEVEN queries per
//! thread, and `threads.status` with no id ran it in a loop. AUI asks with
//! `limit: 1000`; the operator's database holds 587 threads; that is ~4110
//! statements through ONE serialized `Mutex<Connection>`, on every reconnect,
//! every `Lagged` and every five-second stale tick. It was AUI's single most
//! expensive call, and it is why a cache looked attractive.
//!
//! `thread_states` makes FIVE reads for the whole fleet — `thread_locks`,
//! `parked_turns`, `retries`, a grouped `queue` count off
//! `idx_queue_thread_state`, and `thread_seen` — plus two in-memory lookups
//! that cost nothing. So the cache has no argument left, and this test is what
//! keeps it that way: it asserts the COUNT, not the wall time, because wall
//! time on a warm page cache hides N round trips completely.
//!
//! **This file is its own test binary on purpose.** `store::db_trips()` is a
//! process-global counter, so a test running beside this one in the same binary
//! would land its own statements inside the measurement window. Nothing else
//! belongs in here.

use threads::{build_manager, Config, ThreadSettings};

fn set_env() {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// How many statements ONE `thread_states` call over `ids` costs. Five, always.
///
/// Deliberately a whole-table read per input rather than a per-thread lookup:
/// every one of the five tables holds one row per INTERESTING thread (a held
/// lock, a parked turn, an armed retry, a waiting message, a seen cursor),
/// never one row per thread that exists.
const EXPECTED_TRIPS: u64 = 5;

#[tokio::test]
async fn the_fleet_derivation_costs_five_queries_whatever_the_thread_count() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        tmp.path().join("aa-owned").to_string_lossy().to_string(),
    );
    // No `serve()` and no `recover()`: neither is needed to derive, and both
    // start background watchers that would touch the database inside the
    // measurement window.
    let m = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();

    let mut ids = Vec::new();
    for _ in 0..64 {
        let mut s = ThreadSettings::new(tmp.path().to_string_lossy().to_string(), "fable".to_string());
        s.injection = threads::settings::Injection::NOTHING;
        ids.push(m.create_thread(None, s).unwrap());
    }

    // ONE thread.
    let before = threads::store::db_trips();
    let one = m.thread_states(&ids[..1].to_vec()).unwrap();
    let cost_1 = threads::store::db_trips() - before;
    assert_eq!(one.len(), 1);

    // SIXTY-FOUR threads — the same call, the same cost.
    let before = threads::store::db_trips();
    let all = m.thread_states(&ids).unwrap();
    let cost_64 = threads::store::db_trips() - before;
    assert_eq!(all.len(), 64);

    assert_eq!(
        (cost_1, cost_64),
        (EXPECTED_TRIPS, EXPECTED_TRIPS),
        "the derivation must be FLAT in the number of threads. The old shape \
         was 7 queries per thread — {} would have cost {} statements through \
         one serialized connection.",
        ids.len(),
        ids.len() * 7,
    );

    // And the single-thread convenience wrapper is the same derivation plus the
    // two COUNT reads it exists to add (`unread`, `last_seq`) — not a second
    // copy of the derivation with a different rule.
    let before = threads::store::db_trips();
    m.thread_status(&ids[0]).unwrap();
    assert_eq!(
        threads::store::db_trips() - before,
        EXPECTED_TRIPS + 2,
        "thread_status is thread_states plus the two counts, and nothing else"
    );

    // THE WHOLE-FLEET `threads.status` — one derivation, then the two counts
    // per thread. The counts are what this call is FOR, and `threads.list`
    // (which AUI actually hammers on reconnect) pays none of them.
    let before = threads::store::db_trips();
    let rows = m.thread_statuses(&ids).unwrap();
    assert_eq!(rows.len(), 64);
    assert_eq!(
        threads::store::db_trips() - before,
        EXPECTED_TRIPS + 2 * ids.len() as u64,
        "the fleet status is 5 + 2N, not 7N"
    );

    // ── THE SHAPE THIS REPLACED, MEASURED ─────────────────────────────────
    //
    // Not a claim: the seven store calls the deleted per-thread `thread_status`
    // made, in its own order, driven here so the improvement is a number.
    // Nothing in the daemon calls them this way any more.
    let before = threads::store::db_trips();
    for id in &ids {
        let _ = m.store.get_thread_lock(id).unwrap(); //        1. is_locked
        let _ = m.store.waiting_queue(id).unwrap().len(); //    2. queued
        let _ = m.store.get_parked_turn(id).unwrap(); //        3. parked
        let _ = m.store.get_retry(id).unwrap(); //              4. retrying
        let seen = m.store.last_seen_seq(id).unwrap(); //       5. seen cursor
        let _ = m.store.unread_count(id, seen).unwrap(); //     6. unread
        let _ = m.store.max_seq(id).unwrap(); //                7. last_seq
    }
    let old = threads::store::db_trips() - before;
    assert_eq!(
        old,
        7 * ids.len() as u64,
        "the old per-thread shape was seven statements per thread"
    );
    assert!(
        old > 3 * (EXPECTED_TRIPS + 2 * ids.len() as u64),
        "and the fleet call it fed was several times more expensive: \
         {old} statements against {}",
        EXPECTED_TRIPS + 2 * ids.len() as u64
    );
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
