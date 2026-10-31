//! SH-A1 (docs/SHIMS.md §A.4): the `attached_sessions` table and its store
//! CRUD. No verbs and no runtime wiring here — that is SH-A2 onward.
//!
//! Three guards:
//! 1. a CRUD round trip on a FRESH database, with two threads so that a
//!    by-thread list that ignored its filter, an update that wrote nothing,
//!    or a delete that deleted nothing (or everything) each fail;
//! 2. a second insert under one cookie is refused AND leaves the first row
//!    untouched, while a different cookie on the same thread is accepted —
//!    so neither "refuse everything" nor "replace on conflict" passes;
//! 3. an EXISTING database that predates the table gains it on open, keeps
//!    its old rows, and the CRUD works on it. A sentinel first proves the
//!    "before" database really lacks the table.

use rusqlite::Connection;
use threads::store::{AttachedSessionRow, RunningTurnRow, Store};

fn row(cookie: &str, thread: &str, started_at: &str) -> AttachedSessionRow {
    AttachedSessionRow {
        cookie: cookie.into(),
        thread_id: thread.into(),
        account: "acct-a".into(),
        wrapper_pid: 4100,
        claude_pid: None,
        transcript_path: None,
        byte_offset: 0,
        started_at: started_at.into(),
    }
}

fn cookies(rows: &[AttachedSessionRow]) -> Vec<&str> {
    rows.iter().map(|r| r.cookie.as_str()).collect()
}

fn has_table(path: &std::path::Path) -> bool {
    let c = Connection::open(path).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='attached_sessions'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    n == 1
}

/// The full lifecycle against one store; shared by the fresh-DB test and the
/// migrated-DB test so both are held to the same contract.
fn exercise_crud(store: &Store) {
    let a1 = row("ck-a1", "thread-a", "2026-10-20T10:00:00Z");
    let a2 = AttachedSessionRow {
        account: "acct-b".into(),
        wrapper_pid: 4200,
        claude_pid: Some(4201),
        transcript_path: Some("/tmp/x/thread-a.jsonl".into()),
        byte_offset: 77,
        ..row("ck-a2", "thread-a", "2026-10-20T11:00:00Z")
    };
    let b1 = row("ck-b1", "thread-b", "2026-10-20T09:00:00Z");
    store.insert_attached_session(&a2).unwrap();
    store.insert_attached_session(&a1).unwrap();
    store.insert_attached_session(&b1).unwrap();

    // Get by cookie returns every field as written, including the NULLs.
    assert_eq!(store.get_attached_session("ck-a1").unwrap(), Some(a1.clone()));
    assert_eq!(store.get_attached_session("ck-a2").unwrap(), Some(a2.clone()));
    assert_eq!(store.get_attached_session("ck-none").unwrap(), None);

    // List by thread: only that thread's rows, oldest first.
    assert_eq!(cookies(&store.list_attached_sessions("thread-a").unwrap()), ["ck-a1", "ck-a2"]);
    assert_eq!(cookies(&store.list_attached_sessions("thread-b").unwrap()), ["ck-b1"]);
    assert!(store.list_attached_sessions("thread-c").unwrap().is_empty());

    // Offset update touches only its row.
    assert!(store.update_attached_offset("ck-a1", 12_345).unwrap());
    assert_eq!(store.get_attached_session("ck-a1").unwrap().unwrap().byte_offset, 12_345);
    assert_eq!(store.get_attached_session("ck-a2").unwrap().unwrap().byte_offset, 77);

    // Pids + transcript (the SessionStart write).
    assert!(store.update_attached_pids("ck-a1", 4300, Some(4301)).unwrap());
    assert!(store.update_attached_transcript("ck-a1", "/tmp/y/thread-a.jsonl").unwrap());
    let got = store.get_attached_session("ck-a1").unwrap().unwrap();
    assert_eq!(
        got,
        AttachedSessionRow {
            wrapper_pid: 4300,
            claude_pid: Some(4301),
            transcript_path: Some("/tmp/y/thread-a.jsonl".into()),
            byte_offset: 12_345,
            ..a1.clone()
        }
    );
    // b1 untouched by any of it.
    assert_eq!(store.get_attached_session("ck-b1").unwrap(), Some(b1.clone()));

    // Updates on a missing cookie report false and create nothing.
    assert!(!store.update_attached_offset("ck-none", 1).unwrap());
    assert!(!store.update_attached_pids("ck-none", 1, None).unwrap());
    assert!(!store.update_attached_transcript("ck-none", "/p").unwrap());
    assert_eq!(store.get_attached_session("ck-none").unwrap(), None);

    // Delete removes exactly that row.
    assert!(store.delete_attached_session("ck-a1").unwrap());
    assert_eq!(store.get_attached_session("ck-a1").unwrap(), None);
    assert_eq!(cookies(&store.list_attached_sessions("thread-a").unwrap()), ["ck-a2"]);
    assert_eq!(cookies(&store.list_attached_sessions("thread-b").unwrap()), ["ck-b1"]);
    assert!(!store.delete_attached_session("ck-a1").unwrap());
}

#[test]
fn crud_round_trip_on_a_fresh_database() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.db");
    let store = Store::open(&path).unwrap();
    assert!(has_table(&path), "a fresh database must be created with attached_sessions");
    exercise_crud(&store);
}

#[test]
fn second_insert_under_one_cookie_is_refused_and_the_first_row_survives() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("t.db")).unwrap();
    let first = row("ck-dup", "thread-a", "2026-10-20T10:00:00Z");
    store.insert_attached_session(&first).unwrap();

    let clash = AttachedSessionRow {
        account: "acct-other".into(),
        wrapper_pid: 9999,
        ..row("ck-dup", "thread-z", "2026-10-20T12:00:00Z")
    };
    let err = store
        .insert_attached_session(&clash)
        .expect_err("a second insert under the same cookie must be refused");
    assert!(
        err.to_string().contains("ck-dup"),
        "the refusal must name the cookie: {err}"
    );
    // Refused, not merged: the first row is exactly as written.
    assert_eq!(store.get_attached_session("ck-dup").unwrap(), Some(first.clone()));
    assert!(store.list_attached_sessions("thread-z").unwrap().is_empty());

    // Opposing case: a DIFFERENT cookie on the same thread is accepted.
    let other = row("ck-other", "thread-a", "2026-10-20T10:30:00Z");
    store.insert_attached_session(&other).unwrap();
    assert_eq!(
        cookies(&store.list_attached_sessions("thread-a").unwrap()),
        ["ck-dup", "ck-other"]
    );
}

#[test]
fn existing_database_without_the_table_gains_it_on_open() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("old.db");

    // Build an "existing" database: a full store with a live row in an older
    // table, then the new table removed, which is the on-disk shape of every
    // database created before SH-A1.
    {
        let store = Store::open(&path).unwrap();
        store
            .upsert_running_turn(&RunningTurnRow {
                thread_id: "thread-old".into(),
                pid: 777,
                spawn_cookie: "old-cookie".into(),
                output_path: "/tmp/old.out".into(),
                byte_offset: 5,
                started_at: "2026-10-01T00:00:00Z".into(),
                prompt: Some("old prompt".into()),
                message_ids: vec![1, 2],
                attempt: 1,
            })
            .unwrap();
    }
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch("DROP INDEX IF EXISTS idx_attached_sessions_thread; DROP TABLE attached_sessions;")
            .unwrap();
    }
    // Sentinel: the "before" database really lacks the table, so the assertion
    // after the reopen can come out differently.
    assert!(!has_table(&path), "fixture must start without attached_sessions");

    let store = Store::open(&path).unwrap();
    assert!(has_table(&path), "opening an existing database must add attached_sessions");

    // The old data survived the migration.
    let old = store.get_running_turn("thread-old").unwrap().expect("old row kept");
    assert_eq!(old.pid, 777);
    assert_eq!(old.prompt.as_deref(), Some("old prompt"));

    exercise_crud(&store);
}
