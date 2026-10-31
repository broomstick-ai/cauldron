//! **ONE SLOW READ MUST NOT TAKE THE WHOLE STORE OUT OF SERVICE.**
//!
//! This is the structural half of the 2026-09-11 hang. `Store` was one SQLite
//! connection behind one `std::sync::Mutex`, and every method held that guard
//! for its whole query — so a single read that got slow stopped being a slow
//! read and became a process-wide outage. A live `sample(1)` of daemon pid 1337
//! caught four of ten tokio workers parked in `Store::conn`'s `Mutex::lock` and
//! a fifth holding it inside one `SELECT DISTINCT`. `/health`, which returns a
//! fixed struct, took over fifteen seconds to answer. Commit 6ac979d fixed THAT
//! query; this file is about the structure that made one query enough.
//!
//! SQLite in WAL mode does not have this constraint: readers do not block the
//! writer and the writer does not block readers. The mutex was imposing a limit
//! the database itself does not have, and `Store::read` — a pool of read-only
//! connections — removes it.
//!
//! **PROVEN AT N >= 2, because a lock is exactly the thing whose empty and
//! single cases cannot exercise what it arbitrates.** Every case here runs at
//! least two readers against each other:
//!
//! * one slow reader against FOUR fast ones, which is the daemon's shape —
//!   `/health` and the fleet derivation must answer while `ledger.query` scans;
//! * FOUR slow readers at once, which is the pool's own contention: they must
//!   overlap rather than queue, or the pool is a mutex with extra steps;
//! * a WRITER against a slow reader, which is the case the single connection
//!   could never allow and is what actually wedged the daemon — the meter was
//!   trying to record a charge while the scan held the lock.
//!
//! The slow read is `spend_summary`, chosen because it is REAL: it is what
//! `ledger.query` runs, it is a `GROUP BY` over the whole of `usage_ledger`
//! (1,496,710 rows on the operator's daemon, never pruned), and it is
//! therefore a genuine candidate to be the next slow query rather than a
//! synthetic stand-in for one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use threads::store::Store;

/// Rows in the synthetic ledger. Large enough that the scan is hundreds of
/// milliseconds — long enough for "did anything else get through" to be a real
/// question rather than a timer artefact.
const LEDGER_ROWS: usize = 400_000;

/// Readers in the fan-out cases. Two would satisfy the N >= 2 bar; four is
/// closer to the ten tokio workers the daemon actually runs and keeps the pool
/// (8 connections) comfortably unexhausted, so what is being measured is
/// overlap and not queueing.
const FANOUT: usize = 4;

/// Concurrent SCANS in case 2 — the pool's own ceiling (`READ_POOL_MAX`, 8), so
/// none of them can queue on a checkout. Eight rather than `FANOUT` because the
/// statistic case 2 reads is the shortest scan as a fraction of the window, and
/// queueing pins that at `1/N`: the larger N, the further a serialized pool sits
/// below the bar. See case 2 for the measurements that set it (R9.F2).
const SCAN_FANOUT: usize = 8;

fn seeded_store(dir: &std::path::Path) -> Store {
    let store = Store::open(dir.join("threads.db")).unwrap();
    // ONE statement, so seeding costs one transaction rather than 400,000.
    store
        .execute_sql(&format!(
            "INSERT INTO usage_ledger
                (thread_id, account, model, input_tokens, output_tokens,
                 cache_read_tokens, cache_creation_tokens, cost_usd, ts,
                 started_at, accounting_rev)
             WITH RECURSIVE n(i) AS (
                 SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {LEDGER_ROWS}
             )
             SELECT 'thread-' || (i % 512), 'acct-' || (i % 3), 'claude-fable-5',
                    i % 997, i % 331, i % 71, i % 13, (i % 100) / 1000.0,
                    '2026-09-11T00:00:00Z', '2026-09-11T00:00:00Z', 1
             FROM n",
        ))
        .unwrap();
    store
}

/// How long ONE uncontended `spend_summary` takes on this machine. Everything
/// else is expressed as a multiple of it, so the test encodes no absolute
/// wall-clock number and cannot be tuned to one machine's disk.
fn slow_read_baseline(store: &Store) -> Duration {
    // Warm first — the first scan pays for the page cache, and charging that
    // to the baseline would make the fix look better than it is.
    let _ = store.spend_summary(false).unwrap();
    (0..3)
        .map(|_| {
            let t = Instant::now();
            let rows = store.spend_summary(false).unwrap();
            assert!(!rows.is_empty());
            t.elapsed()
        })
        .min()
        .unwrap()
}

#[test]
fn a_slow_read_does_not_stop_the_store_answering_anything_else() {
    let tmp = tempfile::tempdir().unwrap();
    let store = seeded_store(tmp.path());
    let baseline = slow_read_baseline(&store);
    assert!(
        baseline >= Duration::from_millis(30),
        "the seeded scan took {baseline:?}, too fast for this test to mean \
         anything; raise LEDGER_ROWS"
    );

    // ── CASE 1: one slow reader, four fast readers ───────────────────────
    //
    // The fast read is `get_thread` — a primary-key lookup on an empty table,
    // the cheapest thing the store does, and structurally what `/health` and
    // the fleet derivation are made of.
    let stop = Arc::new(AtomicBool::new(false));
    let mut fast = Vec::new();
    for _ in 0..FANOUT {
        let s = store.clone();
        let stop = stop.clone();
        fast.push(thread::spawn(move || {
            let mut worst = Duration::ZERO;
            let mut calls = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let t = Instant::now();
                let _ = s.get_thread("no-such-thread").unwrap();
                worst = worst.max(t.elapsed());
                calls += 1;
            }
            (worst, calls)
        }));
    }
    // Let the fast readers get going, so a result of "zero calls" can only
    // mean blocked.
    thread::sleep(Duration::from_millis(20));

    let s = store.clone();
    let scanner = thread::spawn(move || {
        let t = Instant::now();
        let _ = s.spend_summary(false).unwrap();
        t.elapsed()
    });
    let scan = scanner.join().unwrap();
    stop.store(true, Ordering::Relaxed);
    let (worst, calls): (Duration, u64) = fast
        .into_iter()
        .map(|h| h.join().unwrap())
        .fold((Duration::ZERO, 0), |(w, c), (w2, c2)| (w.max(w2), c + c2));

    assert!(
        calls > 0,
        "no fast read completed at all while the scan ran"
    );
    // THE PROPERTY. A key lookup must not wait for a full-table scan. The bar
    // is a QUARTER of the scan, which is enormously generous — the pre-fix
    // behaviour is a fast read waiting the ENTIRE remaining scan, i.e. worst
    // ~= scan — and being generous is deliberate: this asserts "not
    // serialized", not a latency SLO.
    assert!(
        worst < scan / 4,
        "a primary-key lookup waited {worst:?} while a {scan:?} ledger scan \
         ran ({calls} lookups completed). One slow read is not allowed to be \
         the whole store's availability: this is the shape that parked four of \
         ten tokio workers on 2026-09-11."
    );

    // ── CASE 2: FOUR slow readers at once ────────────────────────────────
    //
    // The pool's own contention case, and the measurement here is OVERLAP
    // rather than throughput — deliberately, because throughput would be
    // measuring the machine. Four concurrent scans on this box take about 4x
    // one scan's wall time even when they are run against four ENTIRELY
    // SEPARATE databases with no shared lock of any kind (measured: 478 ms
    // alone, 1.99-2.02 s each when four run together on four files). That is
    // memory bandwidth in a debug build, and no store design can fix it.
    //
    // What a lock changes is not the total, it is the SHAPE. Serialized, the
    // first scan finishes at ~1x and the last at ~4x, so the SHORTEST
    // individual scan is a quarter of the window. Overlapped, every scan is in
    // flight for almost the whole window, so the shortest is nearly all of it.
    //
    // **THE BAR IS DERIVED FROM THE QUEUEING MODEL, NOT FROM A QUIET MACHINE**
    // (R9.F2). It was a flat 0.6 with N = 4, "comfortably below the ~0.97 the
    // pool produces" — on an idle box. The scans are CPU-bound, so under the
    // suite's real load (other steps building, load 20–70 on 10 cores, two of
    // them efficiency cores) the scheduler does not give four threads equal
    // shares and one scan finishes early. Measured on this code (R9.F2,
    // 2026-10-16, load 20–100): pool, N = 4: shortest/window 0.52–1.00 over 65
    // windows, 5 of them ≤ 0.6 — the full-suite red that is green alone. What
    // queueing actually predicts is shortest ≈ window / N: a test-side mutex
    // around the same calls (the serialized model) measured 0.19–0.30 at N = 4
    // and 0.08–0.17 at N = 8 (18 windows), while the pool at N = 8 measured
    // 0.41–0.99 (18 windows, 12 of them beside 16 busy-loop hogs at load ~90).
    // So: N = 8, and the bar is TWICE the queueing prediction, 2/N = 0.25 —
    // above every serialized window measured, below every overlapped one.
    let bar = 2.0 / SCAN_FANOUT as f64;
    let t = Instant::now();
    let handles: Vec<_> = (0..SCAN_FANOUT)
        .map(|_| {
            let s = store.clone();
            thread::spawn(move || {
                let t = Instant::now();
                let rows = s.spend_summary(false).unwrap();
                assert!(!rows.is_empty());
                t.elapsed()
            })
        })
        .collect();
    let each: Vec<Duration> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let together = t.elapsed();
    let shortest = *each.iter().min().unwrap();
    assert!(
        shortest.as_secs_f64() > together.as_secs_f64() * bar,
        "{SCAN_FANOUT} concurrent ledger scans finished a window of {together:?}, and \
         the shortest of them ran for only {shortest:?} — they are queueing, not \
         overlapping. A read pool that serializes is a mutex with extra steps. \
         (single-scan baseline {baseline:?}, all: {each:?})"
    );

    // ── CASE 3: a WRITER against a slow reader ───────────────────────────
    //
    // The case the single connection could not allow at all, and the one that
    // actually wedged the daemon: the budget meter was trying to RECORD a
    // charge while the scan held the mutex. WAL gives a writer and a reader
    // full concurrency; the mutex took it away.
    let s = store.clone();
    let scanner = thread::spawn(move || {
        let t = Instant::now();
        let _ = s.spend_summary(false).unwrap();
        t.elapsed()
    });
    thread::sleep(Duration::from_millis(5));
    let t = Instant::now();
    store
        .record_charge(&threads::store::ChargeRow {
            budget_key: "default".into(),
            thread_id: "t-1".into(),
            account: "acct-0".into(),
            axis: "session".into(),
            tokens: 10,
            cost_usd: 0.01,
            ts: "2026-09-11T00:00:00Z".into(),
        })
        .unwrap();
    let write = t.elapsed();
    let scan = scanner.join().unwrap();
    assert!(
        write < scan / 4,
        "recording one charge waited {write:?} behind a {scan:?} read. This is \
         `charge_budget` on every streamed token-usage line, and it is exactly \
         what the live sample caught blocked."
    );
}

/// **THE POOL FAILING TO OPEN A CONNECTION MUST NOT FAIL THE READ.**
///
/// Opening a second connection is a way for a read to fail that did not exist
/// before the pool: a descriptor limit, a `-shm` that cannot be created, a
/// filesystem gone read-only. A hardening change is not allowed to turn a
/// working read into a failing one, so `Store::read` falls back to the write
/// connection and logs — degraded to exactly the behaviour this daemon ran on
/// for months, which is a state it survives.
///
/// Forced by unlinking the database out from under the pool. The writer still
/// holds the inode, so the data is all there; only a NEW read-only open can
/// fail, which is precisely the condition under test.
#[test]
fn a_read_whose_pooled_connection_cannot_be_opened_still_answers() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.db");
    let store = Store::open(&path).unwrap();
    store
        .execute_sql(
            "INSERT INTO threads (id, settings_json, last_seq, next_message_id,
                                  last_compaction_message_id, needs_restate,
                                  created_at, updated_at)
             VALUES ('t-1', '{}', 0, 1, 0, 0, '2026-09-11T00:00:00Z', '2026-09-11T00:00:00Z')",
        )
        .unwrap();
    // Prove the read works normally first, or the assertion below could pass
    // for the wrong reason.
    assert!(store.get_thread("t-1").unwrap().is_some());

    // Every pooled connection is returned to the pool after use, so drain the
    // pool's ability to open NEW ones by taking the file away. The one already
    // open is still valid, so ask on several threads at once to make sure at
    // least one checkout has to open.
    std::fs::remove_file(&path).unwrap();
    let handles: Vec<_> = (0..FANOUT)
        .map(|_| {
            let s = store.clone();
            thread::spawn(move || {
                for _ in 0..25 {
                    assert!(
                        s.get_thread("t-1").unwrap().is_some(),
                        "a read must still answer when the pool cannot open a connection"
                    );
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}
