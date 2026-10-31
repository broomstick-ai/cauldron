//! **THE `threads` SHAPE'S PER-KEY HYDRATE COSTS ELEVEN STATEMENTS, WHATEVER
//! THE FLEET HOLDS AND WHATEVER THE THREAD HAS DONE.**
//!
//! `docs/shapes.md` 4.3 makes a bounded read a producer's obligation, and the
//! `accounts` shape is why it needs a test rather than a promise: its
//! `thread_rows_non_idle` did `list_threads(100_000)` plus one `get_thread` per
//! surviving row, and that was invisible for weeks because the tables it walked
//! were empty on the operator's machine. This is the shape where the same mistake
//! would be 5,417 times worse — so the bound is measured, on both axes that could
//! move it.
//!
//! **This file is its own test binary on purpose**, for the same reason
//! `hydrate_cost.rs` and `derivation_cost.rs` are: `store::db_trips()` is a
//! process-global counter, so a test running beside this one would land its own
//! statements inside the measurement window. Nothing else belongs in here.
//!
//! **What eleven is, and it is stated rather than asserted as a magic number:**
//!
//! | request | statements | what |
//! |---|---|---|
//! | `ThreadRowRead` | 2 | one primary-key `get_thread`, plus the last `compacted` event for the marker's range (`idx_events_thread_kind`, tail 1) |
//! | `ThreadDerivedState` | 6 | `thread_states`' FIVE whole-table reads of one-row-per-*interesting*-thread tables — the daemon's ONE derivation, not a second copy — plus one `unread_count` |
//! | `ThreadNewestEvent` | 1 | one backwards seek on `idx_events_thread_seq` |
//! | `ThreadMessageHead` | 1 | one `MAX(message_id)` on `idx_events_thread_msg` |
//! | `ThreadAppState` | 1 | newest custom event per `event_type`, grouped — bounded by the APPLICATION's vocabulary (7 types in the production store) |
//! | `ThreadLiveness` | 0 | one mutex. The beat has no durable store |
//!
//! Plus ONE `COUNT(*)` for `fleet_size` per EPOCH — not per key — which is
//! asserted separately below because "once per epoch" is the claim that would
//! quietly become "once per subscribe" if somebody moved it.
//!
//! **The honest caveat, stated where the number is.** A flat STATEMENT count is
//! not a flat row count, and one of the eleven is not O(1) in rows:
//! `unread_count` counts addressable events above the seen cursor, so a thread
//! that has never been marked seen pays a scan of its own addressable tail. That
//! is the same statement `threads.status` has always answered with, on the same
//! index, and making it O(1) needs a `seen_message_id` column — a migration, and
//! deliberately out of this dispatch's scope. The test varies the log length by
//! 100× precisely so that this is visible as a row cost and not hidden as a
//! statement one.

use threads::shape::Handles;
use threads::shapes::threads as tshape;
use threads::{build_manager, Config, ThreadManager, ThreadSettings};

fn set_env() {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-thread-hydrate-home"),
    );
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
}

/// The statements ONE `hydrates_key` costs. See the table in the module doc.
const EXPECTED_TRIPS: u64 = 11;

fn manager(dir: &std::path::Path) -> ThreadManager {
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        dir.join("aa-owned").to_string_lossy().to_string(),
    );
    // No `serve()` and no `recover()`: both start watchers that would touch the
    // database inside the measurement window.
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

/// **EVERY READ ONE `hydrates_key` MAKES, in the order it makes them.**
///
/// The handlers are called directly rather than through a `shapes.subscribe`,
/// because a statement count needs a synchronous window: the shape task's init
/// runs on another task and a settle would let the manager's own work land inside
/// the measurement. What is measured is therefore exactly the producer's side of
/// the hydrate contract, which is what the bound is about.
fn hydrate_one_key(m: &ThreadManager, id: &str) {
    let _ = m.hydrate(tshape::ThreadRowRead {
        thread_id: id.to_string(),
    });
    let _ = m.hydrate(tshape::ThreadDerivedState {
        thread_id: id.to_string(),
    });
    let _ = m.hydrate(tshape::ThreadNewestEvent {
        thread_id: id.to_string(),
    });
    let _ = m.hydrate(tshape::ThreadMessageHead {
        thread_id: id.to_string(),
    });
    let _ = m.hydrate(tshape::ThreadAppState {
        thread_id: id.to_string(),
    });
    let _ = m.hydrate(tshape::ThreadLiveness {
        thread_id: id.to_string(),
    });
}

/// One measured call: **the FLOOR of five samples, which is the call's own cost.**
///
/// `db_trips()` is process-global and a stray statement can only ADD, so the
/// minimum over several samples is the honest measurement — and it is a
/// measurement rather than a hope, because a genuine N+1 raises every sample and
/// the floor moves with it.
async fn cost(m: &ThreadManager, id: &str) -> u64 {
    let mut floor = u64::MAX;
    for _ in 0..5 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let before = threads::store::db_trips();
        hydrate_one_key(m, id);
        floor = floor.min(threads::store::db_trips() - before);
    }
    floor
}

#[tokio::test]
async fn the_per_key_hydrate_is_flat_in_the_fleet_and_in_the_thread() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path());

    // ── A: a 64-thread fleet, and one thread with a two-event log. ──────────
    let ids = make_threads(&m, tmp.path(), 64);
    let subject = ids[0].clone();
    m.append_custom(
        &subject,
        "summary_updated".into(),
        serde_json::json!({"summary": "one"}),
        true,
    )
    .unwrap();
    let cost_64_short = cost(&m, &subject).await;

    // ── B: the SAME thread, with a log 100× longer and four event types. ────
    //
    // The activity axis. A read that looped the log — or that walked event types
    // one at a time — would move here and not in the fleet axis below, which is
    // why both are varied: a fix for one is not a fix for the other, and the
    // `accounts` defect had one of each.
    for i in 0..200 {
        let t = ["failed_input", "assistant_suggestions", "thread_compacted"][i % 3];
        m.append_custom(
            &subject,
            t.into(),
            serde_json::json!({"i": i}),
            i % 2 == 0,
        )
        .unwrap();
    }
    let cost_64_long = cost(&m, &subject).await;

    // ── C: DOUBLE the fleet, same thread, same log. The other axis. ─────────
    let _more = make_threads(&m, tmp.path(), 64);
    let cost_128_long = cost(&m, &subject).await;

    assert_eq!(
        (cost_64_short, cost_64_long, cost_128_long),
        (EXPECTED_TRIPS, EXPECTED_TRIPS, EXPECTED_TRIPS),
        "a per-key hydrate must not depend on the fleet size OR on what the \
         thread has done. Eleven statements: 2 for the row and its compaction \
         range, 6 for the one derivation plus the unread count, and one each for \
         the log head, the addressable head and the app state. Measured: \
         {cost_64_short} short/64, {cost_64_long} long/64, {cost_128_long} long/128."
    );

    // ── AND THE READ ANSWERS THE RIGHT THING. ──────────────────────────────
    //
    // A cheap read that answers differently is not a fix. These are the two
    // values that would go silently wrong if a cheaper statement had been
    // substituted for the honest one.
    let newest = m.hydrate(tshape::ThreadNewestEvent {
        thread_id: subject.clone(),
    });
    let head = newest.rows().first().cloned().expect("a newest event");
    assert_eq!(
        head.newest.seq,
        m.store.max_seq(&subject).unwrap(),
        "the log head is read out of the LEDGER, not out of the cached \
         `threads.last_seq` column that `import.rs` can leave behind"
    );
    // AND THE BODY SUMMARY RODE THE SAME STATEMENT. The counts above did not
    // move when `body` was added, which is only true because `newest_event`
    // reads one more column of a row it already reads — so the thing that
    // proves the cost claim is that this value arrived, from this one request.
    assert!(
        head.body.is_ok(),
        "the newest event's body summary is answered by the same read as its head"
    );
    let app = m.hydrate(tshape::ThreadAppState {
        thread_id: subject.clone(),
    });
    assert_eq!(
        app.rows().len(),
        4,
        "one row per application event type, newest of each — and 201 events of \
         four types must not answer 201 rows or 1: {:?}",
        app.rows().iter().map(|r| &r.event_type).collect::<Vec<_>>()
    );

    // ── THE FLEET COUNT IS PER EPOCH, NOT PER KEY. ─────────────────────────
    //
    // It is not in the eleven, and this is what says so: the whole-shape read is
    // ONE statement, and it is made by `hydrates` (once, at init) rather than by
    // `hydrates_key`.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let before = threads::store::db_trips();
    let fleet = m.hydrate(tshape::FleetSize);
    let trips = threads::store::db_trips() - before;
    assert_eq!(trips, 1, "one `COUNT(*)`, and it is not per key");
    assert_eq!(
        fleet.rows().first().copied(),
        Some(128),
        "…and it counts the whole fleet, which is the point of carrying it: a \
         consumer holding one key must be able to tell that from a fleet of one"
    );
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
