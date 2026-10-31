//! The WAL keeper's contract: a checkpoint says what it did, and truncation is
//! the only thing that moves the `-wal` file's high-water mark.
//!
//! **A green you have not forced to fail is not evidence.** Every test here
//! first ASSERTS the state it needs into existence — that a WAL was actually
//! grown, that frames were actually present to reclaim — so that a change which
//! makes the checkpoint a no-op fails these rather than passing them vacuously.
//!
//! The defect being pinned (2026-09-11): `Store::checkpoint` ran only on the way
//! out of `run_daemon`, so the `-wal` file's high-water mark was permanent for
//! the daemon's uptime. SQLite's auto-checkpoint keeps the FRAME COUNT near
//! `wal_autocheckpoint` but resets the write cursor in place and never shrinks
//! the file. Three days of that reached 87 MB, a health endpoint at 2.1s against
//! tts's 0.03s, and finally a daemon too wedged to answer the SIGTERM whose
//! checkpoint would have fixed it.

use serde_json::json;
use threads::event::{SendIntent, StoredEvent, ThreadEvent};
use threads::store::{Store, ThreadRow};

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

/// One fat event. The payload is deliberately large so a bounded number of
/// appends moves the `-wal` file by megabytes rather than kilobytes.
fn fat(thread: &str, seq: u64) -> StoredEvent {
    StoredEvent {
        thread_id: thread.into(),
        seq,
        message_id: None,
        ts: "2026-09-11T00:00:00Z".into(),
        event: ThreadEvent::UserMessage {
            text: "x".repeat(16 * 1024),
            intent: SendIntent::Immediate,
            metadata: json!({ "wav_path": "/captures/a.wav" }),
        },
    }
}

fn grow(store: &Store, thread: &str, from: u64, count: u64) {
    for seq in from..from + count {
        store.append_event(&fat(thread, seq)).unwrap();
    }
}

#[test]
fn a_checkpoint_reports_its_frames_and_truncates_the_file() {
    // THE CONSUMER LEDGER IS WIRED BY `run_daemon`, so a test that starts one
    // must be pointed away from the operator's real `~/.cauldron` — otherwise
    // every suite run appends to the file he uses to see who is subscribed, and
    // parallel test binaries interleave into it. See `threads::consumers`.
    std::env::set_var(
        "CAULDRON_HOME",
        std::env::temp_dir().join("threads-tests-cauldron"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("t.db")).unwrap();
    store.upsert_thread(&thread_row("t")).unwrap();

    grow(&store, "t", 1, 400);

    // FORCE THE FAILURE: if the writes above stopped growing a WAL, everything
    // below would pass while proving nothing at all.
    let before = store.wal_bytes();
    assert!(
        before > 0,
        "no WAL was grown — this test proves nothing; check journal_mode=WAL in Store::open"
    );

    let r = store.checkpoint().unwrap();

    assert!(
        r.fully_reclaimed(),
        "a single-connection store had no competing reader, so this must fully \
         reclaim: {r:?}"
    );
    assert_eq!(r.busy, 0, "nothing should have been able to block this: {r:?}");
    assert!(
        r.wal_bytes_after < r.wal_bytes_before,
        "TRUNCATE did not shrink the file: {r:?}"
    );
    assert!(
        store.wal_bytes() < before,
        "the file on disk is still at its pre-checkpoint size"
    );
}

/// **The defect, in one assertion.**
///
/// This is the state the 87 MB WAL was actually in, and it is not the one you
/// would guess. The file is megabytes; the number of frames still waiting to be
/// folded into the main database is ZERO. Auto-checkpoint had done its job
/// perfectly and the file was pure slack — so "is the WAL backed up?" was the
/// wrong question, and any monitor built on frame counts would have reported
/// healthy for all three days.
///
/// Only TRUNCATE reclaims slack. That is the entire reason `spawn_wal_keeper`
/// exists rather than a comment saying "SQLite handles this".
#[test]
fn a_wal_can_hold_megabytes_of_slack_with_zero_live_frames() {
    // THE CONSUMER LEDGER IS WIRED BY `run_daemon`, so a test that starts one
    // must be pointed away from the operator's real `~/.cauldron` — otherwise
    // every suite run appends to the file he uses to see who is subscribed, and
    // parallel test binaries interleave into it. See `threads::consumers`.
    std::env::set_var(
        "CAULDRON_HOME",
        std::env::temp_dir().join("threads-tests-cauldron"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("t.db")).unwrap();
    store.upsert_thread(&thread_row("t")).unwrap();

    grow(&store, "t", 1, 400);

    let r = store.checkpoint().unwrap();

    assert_eq!(
        r.log_frames, 0,
        "expected auto-checkpoint to have already folded everything back, leaving \
         a file that is entirely slack: {r:?}"
    );
    assert!(
        r.wal_bytes_before > 1024 * 1024,
        "expected megabytes of slack behind those zero frames, got {r:?}"
    );
    assert_eq!(
        r.wal_bytes_after, 0,
        "TRUNCATE is the only thing that reclaims slack, and it did not: {r:?}"
    );
}

#[test]
fn the_wal_keeps_its_high_water_mark_until_a_truncate_moves_it() {
    // THE CONSUMER LEDGER IS WIRED BY `run_daemon`, so a test that starts one
    // must be pointed away from the operator's real `~/.cauldron` — otherwise
    // every suite run appends to the file he uses to see who is subscribed, and
    // parallel test binaries interleave into it. See `threads::consumers`.
    std::env::set_var(
        "CAULDRON_HOME",
        std::env::temp_dir().join("threads-tests-cauldron"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("t.db")).unwrap();
    store.upsert_thread(&thread_row("t")).unwrap();

    grow(&store, "t", 1, 400);
    let peak = store.wal_bytes();
    assert!(peak > 0, "no WAL was grown — this test proves nothing");

    // Keep writing. SQLite's auto-checkpoint reclaims frames throughout, but it
    // resets the write cursor IN PLACE: the file itself must not shrink. This
    // is the defect — the reason 87 MB survived for three days — and if it ever
    // stops being true, this assertion is the thing that should fail and make
    // somebody re-read the keeper.
    grow(&store, "t", 1_000, 400);
    assert!(
        store.wal_bytes() >= peak,
        "the -wal shrank without an explicit truncate; auto-checkpoint behaviour \
         has changed and spawn_wal_keeper's justification needs re-reading"
    );

    // And the fix: an explicit truncate is what moves it.
    let r = store.checkpoint().unwrap();
    assert!(r.fully_reclaimed(), "{r:?}");
    assert!(
        store.wal_bytes() < peak,
        "the explicit checkpoint did not move the high-water mark: {r:?}"
    );
}

#[test]
fn a_store_with_no_wal_on_disk_reports_zero_rather_than_failing() {
    // `wal_bytes` is best-effort by contract: the keeper calls it every tick to
    // decide whether the checkpoint is worth the store mutex, so it must never
    // be the thing that panics or errors a daemon.
    let store = Store::open_in_memory().unwrap();
    assert_eq!(
        store.wal_bytes(),
        0,
        "an in-memory store has no -wal file and must report 0, not a stale or \
         guessed size"
    );
}
