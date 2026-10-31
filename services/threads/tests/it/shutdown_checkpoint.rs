//! **A WEDGED STORE MUST DEGRADE THE CLEAN SHUTDOWN, NOT DEFEAT IT.**
//!
//! `599bff3` gave the daemon a real clean shutdown on SIGTERM. On 2026-09-11 it
//! did not hold, and not because of a bug in it: the last thing a clean exit
//! does is fold the WAL back, that ran `Store::checkpoint`, and `checkpoint`
//! wanted the one mutex that was exactly what had failed. The supervisor's own
//! ledger: SIGTERM at 17:24:56Z with 60s to drain, `rung2_failed`, a second
//! SIGTERM, `rung3_failed`, SIGKILL at 17:26:45Z. 108 seconds, two SIGTERMs, no
//! drain, and an 87 MB WAL that survived into the next boot.
//!
//! The read pool removed the INSTANCE — the lock-holder that day was a READ,
//! and reads no longer take that mutex. This file is about the COUPLING, which
//! is what remains: a long WRITE can still hold the write mutex, and
//! `wal_keeper.abort()` cancels the keeper's task without cancelling the
//! `spawn_blocking` closure inside it, so shutdown can still arrive while the
//! keeper holds the connection. Any future long lock-hold would re-break
//! shutdown in exactly the same way.
//!
//! **N >= 2 by construction**: every case here has a second thread holding the
//! store against the shutdown path, because a shutdown checkpoint measured
//! against an idle store cannot exercise the thing it exists to survive.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use threads::event::{SendIntent, StoredEvent, ThreadEvent};
use threads::store::{ShutdownCheckpoint, Store, ThreadRow};

/// The shutdown path's whole budget in these tests. Deliberately small — the
/// daemon uses 5s — so the test says something about the BOUND rather than
/// about this machine's disk.
const BUDGET: Duration = Duration::from_millis(600);

fn thread_row(id: &str) -> ThreadRow {
    ThreadRow {
        id: id.into(),
        account: None,
        cwd: Some("/tmp".into()),
        model: Some("sonnet".into()),
        settings_json: "{}".into(),
        last_seq: 0,
        next_message_id: 1,
        last_compaction_message_id: 0,
        needs_restate: false,
        created_at: "2026-09-11T00:00:00Z".into(),
        updated_at: "2026-09-11T00:00:00Z".into(),
    }
}

fn fat(thread: &str, seq: u64) -> StoredEvent {
    StoredEvent {
        thread_id: thread.into(),
        seq,
        message_id: None,
        ts: "2026-09-11T00:00:00Z".into(),
        event: ThreadEvent::UserMessage {
            text: "x".repeat(16 * 1024),
            intent: SendIntent::Immediate,
            metadata: json!({}),
        },
    }
}

/// A store with a real WAL on disk, so a checkpoint has something to fold.
fn grown(dir: &std::path::Path) -> Store {
    let store = Store::open(dir.join("threads.db")).unwrap();
    store.upsert_thread(&thread_row("t-1")).unwrap();
    for seq in 1..400 {
        store.append_event(&fat("t-1", seq)).unwrap();
    }
    assert!(
        store.wal_bytes() > 1_000_000,
        "the fixture must actually grow a WAL, or every assertion below is vacuous"
    );
    store
}

/// Hold the WRITE mutex for at least `at_least` by giving it one long
/// statement. Returns once the holder is provably inside the statement.
fn hold_the_write_mutex(store: &Store, rows: usize) -> (thread::JoinHandle<()>, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel();
    let s = store.clone();
    let h = thread::spawn(move || {
        // One statement, one transaction: the store mutex is held for the
        // whole insert and so is SQLite's own write lock.
        tx.send(()).unwrap();
        s.execute_sql(&format!(
            "INSERT INTO usage_ledger
                (thread_id, account, model, input_tokens, output_tokens,
                 cache_read_tokens, cache_creation_tokens, cost_usd, ts,
                 started_at, accounting_rev)
             WITH RECURSIVE n(i) AS (
                 SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {rows}
             )
             SELECT 'thread-' || (i % 512), 'acct', 'm', i, i, i, i, 0.0,
                    '2026-09-11T00:00:00Z', '2026-09-11T00:00:00Z', 1
             FROM n",
        ))
        .unwrap();
    });
    (h, rx)
}

/// The idle store's bound on reaching the mutex. An idle store has nothing to
/// wait for: the only work before the first `try_lock` is one `stat` of the
/// `-wal` file. 25 ms is tight enough that a regression which sleeps or polls
/// ~50 ms before its first `try_lock` goes red (R9.F2l forced it with 50 ms
/// and 100 ms sleeps), and loose enough for a machine with every build slot
/// busy. It bounds `waited`, which since R9.F2l is the LOCK WAIT only — read
/// when the lock is won, before the TRUNCATE fold — so this does not time the
/// disk. (Before R9.F2l rung 1 read it after the fold, and a 50 ms bound went
/// red at 123–225 ms under load with the mutex free the whole time.)
const IDLE_REACH_BOUND: Duration = Duration::from_millis(25);

#[test]
fn an_idle_store_reaches_the_primary_mutex_inside_25ms_and_folds_the_wal_back() {
    let tmp = tempfile::tempdir().unwrap();
    let store = grown(tmp.path());
    let before = store.wal_bytes();

    let out = store.checkpoint_for_shutdown(BUDGET);
    match &out {
        ShutdownCheckpoint::OnPrimary { waited, report } => {
            assert!(
                *waited < IDLE_REACH_BOUND,
                "an idle store must be reached immediately: the lock wait was {waited:?} \
                 against a {IDLE_REACH_BOUND:?} bound, with nothing holding the mutex"
            );
            assert!(
                report.fully_reclaimed(),
                "an idle store's checkpoint must fold everything back: {report:?}"
            );
            assert!(
                report.wal_bytes_after < before / 4,
                "TRUNCATE must move the file: {} -> {}",
                report.wal_bytes_before,
                report.wal_bytes_after
            );
        }
        other => panic!("an idle store must go through the front door, got {other:?}"),
    }
}

/// The opposing case for the bound above (doctrine 2): `waited` must MEASURE
/// the lock wait, not merely be small. The write mutex is held for seconds and
/// then released inside rung 1's half of a generous budget, so rung 1 wins
/// after a real wait, and `waited` must cover that hold — a constant `ZERO`
/// fails here. The slack outlasts rung 1's 10 ms poll period.
#[test]
fn a_mutex_released_inside_the_mutex_half_is_won_on_rung_1_and_the_hold_is_waited() {
    const SLACK: Duration = Duration::from_millis(100);
    let tmp = tempfile::tempdir().unwrap();
    let store = grown(tmp.path());

    let (released_tx, released_rx) = mpsc::channel();
    let (h, started) = hold_the_write_mutex(&store, 1_200_000);
    // Stamp the release: `execute_sql` drops the guard as it returns, so the
    // stamp is at or after the release.
    let holder = thread::spawn(move || {
        h.join().unwrap();
        released_tx.send(Instant::now()).unwrap();
    });
    started.recv().unwrap();
    thread::sleep(Duration::from_millis(150));

    let t_call = Instant::now();
    let out = store.checkpoint_for_shutdown(Duration::from_secs(120));
    holder.join().unwrap();
    let hold = released_rx.recv().unwrap().saturating_duration_since(t_call);
    assert!(
        hold > Duration::from_millis(300),
        "premise: the mutex must still be held well after the call ({hold:?}), or \
         this measures no wait at all"
    );
    match &out {
        ShutdownCheckpoint::OnPrimary { waited, .. } => {
            assert!(
                *waited + SLACK >= hold && *waited <= hold + SLACK,
                "`waited` must be the hold: waited {waited:?}, mutex held {hold:?} after the call"
            );
            eprintln!("released door 1: waited {waited:?}, hold {hold:?}");
        }
        other => panic!("a mutex released inside the mutex half must be won on rung 1, got {other:?}"),
    }
}

/// The other half of what R9.F2l fixed: on rung 1 `waited` must STOP AT THE
/// LOCK and not include the fold. The mutex is free, but a reader on a
/// connection of its own holds a WAL snapshot for `STALL`, so TRUNCATE (which
/// must wait for readers to clear before it can reset the file) spends that
/// long inside the fold, on the primary connection's 5 s `busy_timeout`. The
/// pre-R9.F2l shape — `waited` read after `truncate` — reports ~`STALL` here
/// and goes red; lock-wait-only reports ~0. `STALL` is the longest period in
/// the path and the premise asserts the fold really outlasted it.
#[test]
fn a_fold_stalled_500ms_by_a_reader_is_not_counted_as_waited_on_rung_1() {
    const STALL: Duration = Duration::from_millis(500);
    let tmp = tempfile::tempdir().unwrap();
    let store = grown(tmp.path());

    let db = tmp.path().join("threads.db");
    let (snap_tx, snap_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch("BEGIN").unwrap();
        let _: i64 = c
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
            .unwrap();
        snap_tx.send(()).unwrap();
        thread::sleep(STALL);
        c.execute_batch("COMMIT").unwrap();
    });
    snap_rx.recv().unwrap();

    let t = Instant::now();
    let out = store.checkpoint_for_shutdown(BUDGET * 10);
    let elapsed = t.elapsed();
    reader.join().unwrap();
    assert!(
        elapsed >= STALL * 4 / 5,
        "premise: the reader must stall the fold ({elapsed:?} < {STALL:?}), or this \
         cannot tell a lock wait from a fold"
    );
    match &out {
        ShutdownCheckpoint::OnPrimary { waited, report } => {
            assert!(
                *waited < STALL / 4,
                "`waited` must stop at the lock: it was {waited:?} while the fold took \
                 {elapsed:?} with the mutex free throughout"
            );
            eprintln!("stalled fold: waited {waited:?}, elapsed {elapsed:?}, report {report:?}");
        }
        other => panic!("a free mutex must be won on rung 1, got {other:?}"),
    }
}

#[test]
fn a_held_store_answers_inside_the_budget_instead_of_waiting_forever() {
    let tmp = tempfile::tempdir().unwrap();
    let store = grown(tmp.path());

    // 1.2 million rows in one statement — seconds of work, all of it with the
    // store mutex and SQLite's write lock held.
    let (holder, started) = hold_the_write_mutex(&store, 1_200_000);
    started.recv().unwrap();
    thread::sleep(Duration::from_millis(150));

    // ── THE DEFECT, DEMONSTRATED IN PLACE ────────────────────────────────
    //
    // `checkpoint()` is what the shutdown path used to call. Under this exact
    // hold it does not come back, and there is no argument it can be given
    // that would make it come back — it waits on `Mutex::lock`, which has no
    // timeout. This is not a claim in a comment: the assertion below is that
    // it is STILL RUNNING several budgets later.
    let s = store.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let old_path = thread::spawn(move || {
        let t = Instant::now();
        let r = s.checkpoint();
        let _ = done_tx.send(t.elapsed());
        r
    });

    // ── THE FIX ──────────────────────────────────────────────────────────
    let t = Instant::now();
    let out = store.checkpoint_for_shutdown(BUDGET);
    let elapsed = t.elapsed();

    // The ceiling is on WAITING: half the budget on the mutex, the rest as the
    // second connection's `busy_timeout`. The slack above it is the fold's own
    // work, which is deliberately NOT bounded — once SQLite is writing frames
    // back, interrupting it produces the recovery case, and sparing the next
    // boot that recovery is the entire purpose of the checkpoint. Measured
    // here: 1.60s against an unbounded `checkpoint()` that sat for 6.03s and
    // would have sat for as long as the write took, without limit.
    assert!(
        elapsed < BUDGET * 5,
        "the shutdown checkpoint took {elapsed:?} against a {BUDGET:?} budget. \
         An unbounded wait here is what turned every hang into a SIGKILL."
    );
    match &out {
        ShutdownCheckpoint::OnItsOwnConnection { waited, report } => {
            assert!(
                *waited >= BUDGET / 2 && *waited < BUDGET,
                "it must spend its mutex half and then stop waiting: {waited:?}"
            );
            // A REPORTED outcome, whichever way it went. Busy means SQLite
            // would not hand over the write lock inside the budget, which is
            // the honest answer for a store that is genuinely mid-write; it is
            // still infinitely better than not returning.
            eprintln!(
                "second door: busy={} log_frames={} checkpointed={} wal {} -> {}",
                report.busy,
                report.log_frames,
                report.checkpointed_frames,
                report.wal_bytes_before,
                report.wal_bytes_after
            );
        }
        other => panic!(
            "a store whose write mutex is held for seconds must fall through to \
             the second door, got {other:?}"
        ),
    }

    // The old path, meanwhile, is still waiting.
    assert!(
        done_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "`checkpoint()` returned while the write mutex was still held — the \
         fixture is not holding it and this test proves nothing"
    );

    holder.join().unwrap();
    // And once the holder lets go, the old path completes: nothing here is
    // broken, it was only ever unbounded.
    let old_took = done_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    let _ = old_path.join().unwrap().unwrap();
    assert!(
        old_took > BUDGET * 3,
        "the unbounded path finished in {old_took:?}, which is inside the \
         budget — the hold was too short for this comparison to mean anything"
    );
    eprintln!("unbounded checkpoint() waited {old_took:?}; bounded path returned in {elapsed:?}");
}
