//! **R10.M1 — THE TWO RECORDS** (`docs/MESSAGING-ORCHESTRATION.md` §5, §6.2,
//! §10.2; L-8 / E21-4: *"you need to separately track sent and whether to
//! send"*).
//!
//! Three groups, one per claim the step makes:
//!
//! 1. **The migration keeps every row.** A database built from the schema as it
//!    stood before this step (`tests/fixtures/schema-pre-r10m1.sql`, verbatim
//!    from 7ef22e73) with a row in every old state, opened by this build.
//! 2. **The two records are separate.** Withdraw, resend, attempt and
//!    delivered-derived-from-`turn_started`, at the store, each with the
//!    forcing that opposes it.
//! 3. **A kill at every instant between append and `turn_started` leaves the
//!    message recoverable.** The durable residue of a kill at each instant is
//!    left by a first manager, and a SECOND manager built fresh on the same
//!    data directory is the next boot. N = 3 rows per batch.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use threads::event::{delivery, Push, SendIntent, StoredEvent, ThreadEvent};
use threads::settings::ThreadSettings;
use threads::store::Store;
use threads::{build_manager, Config, ThreadManager};

// ── 1. the migration ─────────────────────────────────────────────────────

const OLD_SCHEMA: &str = include_str!("../fixtures/schema-pre-r10m1.sql");

/// One old row: (thread, message_id, text, intent, state).
const OLD_ROWS: &[(&str, u64, &str, &str, &str)] = &[
    ("t-a", 1, "waiting immediate", "immediate", "waiting"),
    ("t-a", 2, "waiting turn_end", "turn_end", "waiting"),
    ("t-a", 3, "delivered", "immediate", "delivered"),
    ("t-a", 4, "failed", "immediate", "failed"),
    ("t-a", 5, "cancelled one", "immediate", "cancelled"),
    ("t-a", 6, "cancelled two", "immediate_bare", "cancelled"),
    ("t-b", 1, "cancelled on b", "turn_end", "cancelled"),
    ("t-b", 2, "waiting bare", "immediate_bare", "waiting"),
];

/// A database exactly as a pre-R10.M1 daemon left it: the old DDL, a thread
/// row for `t-a`, a few ledger rows, and one queue row in every old state.
fn old_db(path: &Path) {
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch(OLD_SCHEMA).unwrap();
    let cols: Vec<String> = c
        .prepare("SELECT name FROM pragma_table_info('queue')")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    // SENTINEL: the fixture really is the old shape. Without this, a fixture
    // regenerated from the new schema would make every assertion below a
    // check of a fresh database, which needs no migration at all.
    assert!(
        !cols.iter().any(|c| c == "dispatch" || c == "attempts"),
        "the fixture must be the PRE-R10.M1 queue: {cols:?}"
    );
    c.execute(
        "INSERT INTO threads (id, settings_json, last_seq, created_at, updated_at)
         VALUES ('t-a', '{}', 3, '2026-10-01T00:00:00Z', '2026-10-01T00:00:00Z')",
        [],
    )
    .unwrap();
    for seq in 1..=3i64 {
        c.execute(
            "INSERT INTO events (thread_id, seq, message_id, kind, payload_json, ts)
             VALUES ('t-a', ?1, NULL, 'turn_started', ?2, '2026-10-01T00:00:00Z')",
            rusqlite::params![
                seq,
                json!({"kind": "turn_started", "attempt": 1, "message_ids": [3]}).to_string()
            ],
        )
        .unwrap();
    }
    for (t, mid, text, intent, state) in OLD_ROWS {
        c.execute(
            "INSERT INTO queue (thread_id, message_id, text, intent, state, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, '2026-10-01T00:00:00Z')",
            rusqlite::params![t, *mid as i64, text, intent, state],
        )
        .unwrap();
    }
}

/// Every queue row as (id, thread, message_id, text, intent, state, dispatch,
/// attempts), read with plain SQL so the reader is not the code under test.
#[allow(clippy::type_complexity)]
fn rows(path: &Path) -> Vec<(i64, String, u64, String, String, String, String, i64)> {
    let c = rusqlite::Connection::open(path).unwrap();
    let mut s = c
        .prepare(
            "SELECT id, thread_id, message_id, text, intent, state, dispatch, attempts
             FROM queue ORDER BY id",
        )
        .unwrap();
    let out = s
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, i64>(2)? as u64,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    out
}

fn withdrawn_events(store: &Store, thread: &str) -> Vec<(u64, Value)> {
    store
        .delivery_events(thread)
        .unwrap()
        .into_iter()
        .filter(|(_, t, _)| t == delivery::WITHDRAWN)
        .map(|(seq, _, d)| (seq, d))
        .collect()
}

/// **EVERY ROW SURVIVES, AND EACH OLD STATE LANDS WHERE §10.2 SAYS.**
///
/// `waiting → eager` (a `turn_end` row keeps `turn_end`), `delivered`/`failed`
/// unchanged, `cancelled → never` + ONE `message_withdrawn` per thread. Ids,
/// text, message ids and intents are compared row for row against the
/// pre-migration read, so a migration that rebuilt the table would be caught.
#[test]
fn the_migration_keeps_every_row_in_every_old_state() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.db");
    old_db(&path);
    let before: Vec<(i64, String, u64, String, String, String)> = {
        let c = rusqlite::Connection::open(&path).unwrap();
        let mut s = c
            .prepare("SELECT id, thread_id, message_id, text, intent, state FROM queue ORDER BY id")
            .unwrap();
        let v = s
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get::<_, i64>(2)? as u64,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v
    };
    assert_eq!(before.len(), OLD_ROWS.len());

    let store = Store::open(&path).unwrap();
    let after = rows(&path);
    assert_eq!(after.len(), before.len(), "THE MIGRATION LOST ROWS: {after:?}");
    for (b, a) in before.iter().zip(after.iter()) {
        assert_eq!((b.0, &b.1, b.2, &b.3, &b.4), (a.0, &a.1, a.2, &a.3, &a.4), "row identity moved");
        let (want_state, want_dispatch, want_attempts) = match (b.5.as_str(), b.4.as_str()) {
            ("waiting", i) if i.starts_with("turn_end") => ("waiting", "turn_end", 0),
            ("waiting", _) => ("waiting", "eager", 0),
            ("delivered", _) => ("delivered", "eager", 1),
            ("failed", _) => ("failed", "eager", 1),
            ("cancelled", _) => ("waiting", "never", 0),
            (s, _) => panic!("unexpected old state {s}"),
        };
        assert_eq!(
            (a.5.as_str(), a.6.as_str(), a.7),
            (want_state, want_dispatch, want_attempts),
            "old row {b:?} migrated wrong"
        );
    }

    // One withdrawn event per thread, naming exactly that thread's cancelled
    // rows, at the next seq on that thread.
    let a = withdrawn_events(&store, "t-a");
    assert_eq!(a.len(), 1, "t-a: {a:?}");
    assert_eq!(a[0].0, 4, "the withdrawn event takes MAX(seq)+1 on t-a");
    assert_eq!(a[0].1["message_ids"], json!([5, 6]));
    assert_eq!(a[0].1["dispatch_now"], json!("never"));
    assert_eq!(a[0].1["by"], json!("migration"));
    let b = withdrawn_events(&store, "t-b");
    assert_eq!(b.len(), 1, "t-b: {b:?}");
    assert_eq!(b[0].0, 1, "t-b had no ledger rows, so its event is seq 1");
    assert_eq!(b[0].1["message_ids"], json!([1]));

    // The dispatcher's view: waiting and not withdrawn, in order.
    let wa: Vec<u64> = store.waiting_queue("t-a").unwrap().iter().map(|r| r.message_id).collect();
    assert_eq!(wa, vec![1, 2], "withdrawn rows must not be offered; live ones must");
    let wb: Vec<u64> = store.waiting_queue("t-b").unwrap().iter().map(|r| r.message_id).collect();
    assert_eq!(wb, vec![2]);
    let failed: Vec<u64> = store.failed_deliveries("t-a").unwrap().iter().map(|r| r.message_id).collect();
    assert_eq!(failed, vec![4], "a failed row stays failed and re-selectable");
    // The legacy `delivered` row with no `turn_started` naming it is still
    // what boot recovery restores — the migration did not hide it. Message 3
    // HAS a turn_started, so nothing is stranded here.
    assert!(store.stranded_deliveries().unwrap().is_empty());
}

/// **IDEMPOTENT, AND A LATE `cancelled` IS STILL CONVERTED.** A second open
/// writes nothing; a row an older binary cancels afterwards is converted on the
/// next open with exactly one more event.
#[test]
fn the_migration_is_idempotent_and_converts_a_late_cancelled_row() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.db");
    old_db(&path);
    drop(Store::open(&path).unwrap());
    let once = rows(&path);
    let store = Store::open(&path).unwrap();
    assert_eq!(rows(&path), once, "a second open moved rows");
    assert_eq!(withdrawn_events(&store, "t-a").len(), 1, "a second open wrote a second event");
    drop(store);

    // An older binary, run against the migrated file, cancels message 1.
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        let n = c
            .execute(
                "UPDATE queue SET state='cancelled' WHERE thread_id='t-a' AND message_id=1",
                [],
            )
            .unwrap();
        assert_eq!(n, 1);
    }
    let store = Store::open(&path).unwrap();
    let ev = withdrawn_events(&store, "t-a");
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[1].1["message_ids"], json!([1]));
    let r1 = rows(&path).into_iter().find(|r| r.1 == "t-a" && r.2 == 1).unwrap();
    assert_eq!((r1.5.as_str(), r1.6.as_str()), ("waiting", "never"));
    // Opposing: message 2, never cancelled, is untouched.
    let r2 = rows(&path).into_iter().find(|r| r.1 == "t-a" && r.2 == 2).unwrap();
    assert_eq!((r2.5.as_str(), r2.6.as_str()), ("waiting", "turn_end"));
}

/// **A COPY OF THE LIVE DATABASE** — run by hand, never by the suite. Make
/// the copy WAL-consistently (the source opened read-only), then point the
/// test at the copy:
/// `sqlite3 'file:<live threads.db>?mode=ro' ".backup /tmp/copy.db"`, then
/// `R10M1_DB_COPY=/tmp/copy.db cargo test -p threads --test it -- --ignored two_records::a_copy_of_a_real`.
/// Opens the copy, and asserts every pre-existing row is still there with its
/// identity intact and its old state mapped. R10.M1b: it REFUSES a live store
/// (see [`refuse_live_db`]) before anything is opened, because it migrates
/// the file it is given IN PLACE and forward-only.
#[test]
#[ignore]
fn a_copy_of_a_real_database_migrates_without_losing_a_row() {
    let Ok(p) = std::env::var("R10M1_DB_COPY") else {
        panic!("set R10M1_DB_COPY to a COPY of a threads.db (never the live file)");
    };
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    match migrate_copy_in_place(Path::new(&p), &home) {
        Ok(summary) => eprintln!("{summary}"),
        Err(refusal) => panic!("{refusal}"),
    }
}

/// The body of the hand-run test. Refuses a live store FIRST — nothing below
/// the guard runs, so nothing is opened, for read or for write — then
/// migrates the copy in place and checks every row.
fn migrate_copy_in_place(path: &Path, home: &Path) -> Result<String, String> {
    refuse_live_db(path, home)?;
    let before: Vec<(i64, String, u64, String, String)> = {
        let c = rusqlite::Connection::open(path).unwrap();
        let mut s = c
            .prepare("SELECT id, thread_id, message_id, text, state FROM queue ORDER BY id")
            .unwrap();
        let v = s
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64, r.get(3)?, r.get(4)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v
    };
    let store = Store::open(path).unwrap();
    let after = rows(path);
    assert_eq!(after.len(), before.len());
    let mut by_state = std::collections::BTreeMap::<String, usize>::new();
    for (b, a) in before.iter().zip(after.iter()) {
        assert_eq!((b.0, &b.1, b.2, &b.3), (a.0, &a.1, a.2, &a.3));
        let ok = match b.4.as_str() {
            "cancelled" => a.5 == "waiting" && a.6 == "never",
            s => a.5 == s && a.6 != "never",
        };
        assert!(ok, "{b:?} -> {a:?}");
        *by_state.entry(format!("{}->{}/{}", b.4, a.5, a.6)).or_default() += 1;
    }
    let cancelled_threads: std::collections::BTreeSet<&String> =
        before.iter().filter(|b| b.4 == "cancelled").map(|b| &b.1).collect();
    for t in cancelled_threads {
        assert_eq!(withdrawn_events(&store, t).len(), 1, "thread {t}");
    }
    Ok(format!("R10M1 live-copy migration: {} rows, {by_state:?}", after.len()))
}

/// **R10.M1b — REFUSE A LIVE STORE.** `Err` with a message naming why, for a
/// `path` that is, or resolves to (symlink or hard link), a live threads
/// store, or that any process holds open. Uses only `stat`/`realpath`, a
/// directory listing of `<home>/.cauldron/data` and `lsof`: the candidate is
/// never opened, and neither is any live file.
///
/// Live means: anything under `<home>/.cauldron` or `<home>/.threads` (the
/// path as given, or its canonical form, under the root as given or its
/// canonical form); the same inode as `<home>/.cauldron/data/*/threads/store/threads.db`
/// or `<home>/.threads/threads.db`; or the db, its `-wal` or its `-shm` open in
/// another process. A `sqlite3 .backup` copy anywhere else, closed, passes.
/// Fails closed on the holder check: a file counts as NOT held only when
/// `lsof -w -t` exits 1 with empty stdout AND empty stderr (or lists no pid
/// but this process's). Any other answer — another exit code, anything on
/// stderr (lsof's own errors exit 1 too, with a message there), lsof missing —
/// refuses the path.
fn refuse_live_db(path: &Path, home: &Path) -> Result<(), String> {
    refuse_live_db_with(path, home, Path::new("lsof"))
}

/// [`refuse_live_db`] with the `lsof` binary injectable, so its failure
/// modes can be forced in a test.
fn refuse_live_db_with(path: &Path, home: &Path, lsof: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let refuse = |why: String| {
        Err(format!(
            "REFUSED {}: {why}. This test migrates the file IN PLACE and forward-only; \
             give it a `sqlite3 .backup` COPY outside ~/.cauldron that nothing holds open",
            path.display()
        ))
    };
    let canon = match std::fs::canonicalize(path) {
        Ok(c) => c,
        Err(e) => return refuse(format!("cannot resolve it ({e})")),
    };
    let roots = [home.join(".cauldron"), home.join(".threads")];
    for root in &roots {
        let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        for p in [path, canon.as_path()] {
            for r in [root.as_path(), root_canon.as_path()] {
                // `Path::starts_with` compares whole components, so
                // `.cauldron` never matches `.cauldron2`.
                if p.starts_with(r) {
                    return refuse(format!("{} is under the live root {}", p.display(), r.display()));
                }
            }
        }
    }
    let meta = std::fs::metadata(&canon).map_err(|e| format!("REFUSED {}: stat: {e}", path.display()))?;
    let mut live_files = vec![home.join(".threads").join("threads.db")];
    if let Ok(rd) = std::fs::read_dir(home.join(".cauldron").join("data")) {
        for env in rd.flatten() {
            live_files.push(env.path().join("threads").join("store").join("threads.db"));
        }
    }
    for lf in &live_files {
        if let Ok(m) = std::fs::metadata(lf) {
            if (m.dev(), m.ino()) == (meta.dev(), meta.ino()) {
                return refuse(format!("it is the same file as the live store {}", lf.display()));
            }
        }
    }
    let me = std::process::id().to_string();
    for suffix in ["", "-wal", "-shm"] {
        let f = PathBuf::from(format!("{}{suffix}", canon.display()));
        if !suffix.is_empty() && !f.exists() {
            continue;
        }
        let out = match std::process::Command::new(lsof).arg("-w").arg("-t").arg("--").arg(&f).output() {
            Ok(o) => o,
            Err(e) => return refuse(format!("cannot run lsof to see who holds it ({e})")),
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let pids: Vec<&str> = stdout.split_whitespace().collect();
        let holders: Vec<&str> = pids.iter().copied().filter(|pid| *pid != me).collect();
        if !holders.is_empty() {
            return refuse(format!("{} is held open by pid(s) {}", f.display(), holders.join(",")));
        }
        let clean_none = out.status.code() == Some(1) && pids.is_empty() && stderr.trim().is_empty();
        let only_me = out.status.code() == Some(0) && !pids.is_empty() && stderr.trim().is_empty();
        if !(clean_none || only_me) {
            return refuse(format!(
                "lsof gave no clean answer on whether {} is held ({}, stderr: {:?})",
                f.display(),
                out.status,
                stderr.trim()
            ));
        }
    }
    Ok(())
}

/// A temp `home` with a pre-M1 database laid out exactly where the live one
/// lives (`<home>/.cauldron/data/prod/threads/store/threads.db`).
fn fake_live_home() -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let store = home.path().join(".cauldron/data/prod/threads/store");
    std::fs::create_dir_all(&store).unwrap();
    let db = store.join("threads.db");
    old_db(&db);
    (home, db)
}

/// Still the pre-M1 shape: the guard's refusal left the file unmigrated.
/// Read with a plain read-only connection, after the fact.
fn still_unmigrated(path: &Path) -> bool {
    let c = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let cols: Vec<String> = c
        .prepare("SELECT name FROM pragma_table_info('queue')")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    !cols.iter().any(|c| c == "dispatch")
}

/// **THE HAND-RUN MIGRATION REFUSES A PATH THAT LOOKS LIVE** — the path as
/// given, a symlink to it from outside, and a hard link to it from outside;
/// each refusal leaves the file unmigrated. Opposing forcing: a `sqlite3
/// .backup` copy of that same database, outside the live root, IS migrated.
#[test]
fn the_hand_run_migration_refuses_a_live_layout_path_and_takes_a_backup_copy() {
    let (home, live) = fake_live_home();
    let outside = tempfile::tempdir().unwrap();

    let e = migrate_copy_in_place(&live, home.path()).unwrap_err();
    assert!(e.starts_with("REFUSED") && e.contains("live root"), "{e}");
    assert!(still_unmigrated(&live), "refused, yet the live-layout file was migrated");

    let link = outside.path().join("innocent.db");
    std::os::unix::fs::symlink(&live, &link).unwrap();
    let e = migrate_copy_in_place(&link, home.path()).unwrap_err();
    assert!(e.contains("live root"), "a symlink resolving into the live root: {e}");
    assert!(still_unmigrated(&live));

    let hard = outside.path().join("hard.db");
    std::fs::hard_link(&live, &hard).unwrap();
    let e = migrate_copy_in_place(&hard, home.path()).unwrap_err();
    assert!(e.contains("same file as the live store"), "a hard link to the live file: {e}");
    assert!(still_unmigrated(&live));

    // Opposing: the backup the operator is told to make passes and migrates.
    let copy = outside.path().join("copy.db");
    let st = std::process::Command::new("sqlite3")
        .arg(format!("file:{}?mode=ro", live.display()))
        .arg(format!(".backup {}", copy.display()))
        .status()
        .unwrap();
    assert!(st.success(), "sqlite3 .backup: {st}");
    assert!(still_unmigrated(&copy), "SENTINEL: the copy starts in the old shape");
    let ok = migrate_copy_in_place(&copy, home.path()).unwrap();
    assert!(ok.contains(&format!("{} rows", OLD_ROWS.len())), "{ok}");
    assert!(!still_unmigrated(&copy), "the accepted copy really was migrated");
    assert!(still_unmigrated(&live), "migrating the copy never touched the original");
}

/// **A DATABASE ANOTHER PROCESS HOLDS OPEN IS REFUSED**, wherever it lives —
/// a `sqlite3` child with the file open and a query answered. The SAME file,
/// once that process has exited, is accepted (opposing forcing), so the
/// refusal is about the holder and not the path.
#[test]
fn the_hand_run_migration_refuses_a_database_another_process_holds_open() {
    use std::io::{BufRead, BufReader, Write};
    let home = tempfile::tempdir().unwrap(); // an empty home: no live root to trip on
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("held.db");
    old_db(&db);

    let mut holder = std::process::Command::new("sqlite3")
        .arg(&db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = holder.stdin.take().unwrap();
    writeln!(stdin, "SELECT 'open', count(*) FROM queue;").unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    BufReader::new(holder.stdout.take().unwrap()).read_line(&mut line).unwrap();
    // SENTINEL: the holder really has the database open (it answered from it).
    assert_eq!(line.trim(), format!("open|{}", OLD_ROWS.len()), "holder did not open the db");

    let e = migrate_copy_in_place(&db, home.path()).unwrap_err();
    assert!(e.contains(&format!("held open by pid(s) {}", holder.id())), "{e}");
    assert!(still_unmigrated(&db), "refused, yet the held file was migrated");

    drop(stdin); // EOF: the holder exits and closes the file
    assert!(holder.wait().unwrap().success());
    let ok = migrate_copy_in_place(&db, home.path()).unwrap();
    assert!(ok.contains(&format!("{} rows", OLD_ROWS.len())), "{ok}");
}

/// **AN LSOF THAT ERRORS IS NOT "NOBODY HOLDS IT"** (inspect finding, 4e99488).
/// Fake `lsof` scripts stand in for the real one on a closed temp db: exit 1
/// with a message on stderr (what lsof's own errors look like), exit 2 with
/// nothing, and exit 0 with nothing are each refused, and the file is left
/// unmigrated. Opposing forcing: a fake that exits 1 with empty stdout and
/// stderr — the real lsof's answer for a file nobody holds — is accepted.
#[test]
fn an_lsof_that_errors_refuses_and_only_a_clean_empty_answer_accepts() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fake = |name: &str, body: &str| {
        let p = dir.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    let db = dir.path().join("closed.db");
    old_db(&db);
    for (name, body) in [
        ("lsof-stderr", "echo 'lsof: status error on it: Permission denied' >&2; exit 1"),
        ("lsof-rc2", "exit 2"),
        ("lsof-rc0-empty", "exit 0"),
    ] {
        let e = refuse_live_db_with(&db, home.path(), &fake(name, body)).unwrap_err();
        assert!(e.contains("lsof gave no clean answer"), "{name}: {e}");
    }
    let e = refuse_live_db_with(&db, home.path(), &dir.path().join("no-such-lsof")).unwrap_err();
    assert!(e.contains("cannot run lsof"), "{e}");
    assert!(still_unmigrated(&db));
    // Opposing: the clean "nobody holds it" answer passes.
    refuse_live_db_with(&db, home.path(), &fake("lsof-clean", "exit 1")).unwrap();
    // And the real lsof gives exactly that answer for this closed file.
    refuse_live_db(&db, home.path()).unwrap();
}

// ── 2. the two records, at the store ─────────────────────────────────────

fn custom(thread: &str, seq: u64, event_type: &str, data: Value) -> StoredEvent {
    StoredEvent {
        thread_id: thread.to_string(),
        seq,
        message_id: None,
        ts: threads::store::now_rfc3339(),
        event: ThreadEvent::Custom { event_type: event_type.to_string(), data },
    }
}

fn enqueue(store: &Store, thread: &str, mid: u64, intent: &str) -> i64 {
    store.enqueue(thread, mid, &format!("words {mid}"), intent, &Value::Null, &[]).unwrap()
}

/// **WITHDRAW TAKES A ROW OUT OF THE DISPATCHER'S VIEW AND KEEPS ITS TEXT.**
/// Opposing forcings: a sibling row is still offered; a row with an open
/// attempt is not withdrawable; a second withdraw moves nothing.
#[test]
fn withdraw_hides_one_row_keeps_its_text_and_resend_brings_it_back() {
    let store = Store::open_in_memory().unwrap();
    enqueue(&store, "t", 1, "immediate");
    enqueue(&store, "t", 2, "turn_end");
    let r3 = enqueue(&store, "t", 3, "immediate");
    store.mark_attempted(&[r3], None).unwrap();

    let ev = custom("t", 10, delivery::WITHDRAWN, json!({"message_ids": [1]}));
    assert!(store.queue_withdraw("t", 1, Some(&ev)).unwrap());
    let w: Vec<(u64, String)> =
        store.waiting_queue("t").unwrap().into_iter().map(|r| (r.message_id, r.dispatch)).collect();
    assert_eq!(w, vec![(2, "turn_end".to_string())], "only the live row is offered");
    assert_eq!(store.waiting_queue_counts().unwrap().get("t"), Some(&1));
    assert!(store.threads_with_waiting_queue().unwrap().contains(&"t".to_string()));
    assert_eq!(withdrawn_events(&store, "t").len(), 1, "the withdraw is on the ledger");
    assert!(!store.queue_withdraw("t", 1, None).unwrap(), "a second withdraw moves nothing");
    assert!(!store.queue_withdraw("t", 3, None).unwrap(), "an attempted row is not withdrawable");

    // The text never left: resend brings the row back verbatim, eager.
    assert_eq!(store.queue_resend("t", &[1]).unwrap(), vec![1]);
    let w: Vec<(u64, String, String)> = store
        .waiting_queue("t")
        .unwrap()
        .into_iter()
        .map(|r| (r.message_id, r.text, r.dispatch))
        .collect();
    assert_eq!(
        w,
        vec![(1, "words 1".into(), "eager".into()), (2, "words 2".into(), "turn_end".into())]
    );
    assert!(store.queue_resend("t", &[]).unwrap().is_empty(), "nothing else to resend");
}

/// **AN ATTEMPT IS THE ROW MOVE AND THE EVENT, OR NEITHER.** The event is
/// forced to fail (its seq is taken), and no row may have moved. Opposing: with
/// a free seq both land.
#[test]
fn an_attempt_that_cannot_be_recorded_moves_no_row() {
    let store = Store::open_in_memory().unwrap();
    let a = enqueue(&store, "t", 1, "immediate");
    let b = enqueue(&store, "t", 2, "immediate");
    store.append_event(&custom("t", 5, "x/occupier", Value::Null)).unwrap();

    let clash = custom("t", 5, delivery::ATTEMPTED, json!({"attempt": "att:5", "message_ids": [1, 2]}));
    assert!(store.mark_attempted(&[a, b], Some(&clash)).is_err(), "the clash must fail the attempt");
    let w = store.waiting_queue("t").unwrap();
    assert_eq!(w.len(), 2, "a failed attempt record moved rows: {w:?}");
    assert!(w.iter().all(|r| r.attempts == 0 && r.state == "waiting"));

    let ok = custom("t", 6, delivery::ATTEMPTED, json!({"attempt": "att:6", "message_ids": [1, 2]}));
    store.mark_attempted(&[a, b], Some(&ok)).unwrap();
    assert!(store.waiting_queue("t").unwrap().is_empty());
    assert_eq!(store.last_attempt_ids("t", &[1, 2]).unwrap().len(), 2);
}

/// **THE APPEND AND THE QUEUE ROW ARE ONE WRITE.** A `user_message` that
/// cannot be appended leaves no row, and a row never exists without its words
/// on the ledger. Opposing: a free seq writes both.
#[test]
fn an_append_that_cannot_be_queued_writes_neither() {
    let store = Store::open_in_memory().unwrap();
    store.append_event(&custom("t", 1, "x/occupier", Value::Null)).unwrap();
    let um = |seq| StoredEvent {
        thread_id: "t".into(),
        seq,
        message_id: Some(1),
        ts: threads::store::now_rfc3339(),
        event: ThreadEvent::UserMessage {
            text: "words".into(),
            intent: SendIntent::Immediate,
            metadata: Value::Null,
        },
    };
    assert!(store
        .enqueue_recorded("t", 1, "words", "immediate", &Value::Null, &[], Some(&um(1)))
        .is_err());
    assert!(store.waiting_queue("t").unwrap().is_empty(), "a row without its append");
    store
        .enqueue_recorded("t", 1, "words", "immediate", &Value::Null, &[], Some(&um(2)))
        .unwrap();
    assert_eq!(store.waiting_queue("t").unwrap().len(), 1);
    assert_eq!(store.max_seq("t").unwrap(), 2);
}

/// **DELIVERED IS DERIVED FROM `turn_started`**, and an attempt alone is not a
/// delivery. Opposing: the id a `turn_started` names IS delivered; the one it
/// does not name is not.
#[test]
fn delivered_is_derived_from_turn_started_and_an_attempt_is_not_a_delivery() {
    let store = Store::open_in_memory().unwrap();
    let a = enqueue(&store, "t", 1, "immediate");
    let b = enqueue(&store, "t", 2, "immediate");
    store.mark_attempted(&[a, b], None).unwrap();
    assert!(store.delivered_message_ids("t").unwrap().is_empty(), "attempted is not delivered");
    assert_eq!(store.stranded_deliveries().unwrap().len(), 2, "both are recoverable");
    store
        .append_event(&StoredEvent {
            thread_id: "t".into(),
            seq: 1,
            message_id: None,
            ts: threads::store::now_rfc3339(),
            event: ThreadEvent::TurnStarted { attempt: 1, message_ids: vec![1] },
        })
        .unwrap();
    let d = store.delivered_message_ids("t").unwrap();
    assert!(d.contains(&1) && !d.contains(&2), "{d:?}");
    let s: Vec<u64> = store.stranded_deliveries().unwrap().iter().map(|r| r.message_id).collect();
    assert_eq!(s, vec![2]);
}

// ── 3. a kill at every instant between append and turn_started ───────────

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env() {
    std::env::set_var("CAULDRON_HOME", std::env::temp_dir().join("threads-tests-cauldron"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "200");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &Path) -> Config {
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert("aa-owned".to_string(), dir.join("aa-owned").to_string_lossy().to_string());
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn plain(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..400 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

type Log = Arc<Mutex<Vec<ThreadEvent>>>;

fn watch(m: &ThreadManager, thread: &str) -> Log {
    let mut rx = m.subscribe();
    let seen: Log = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    let thread = thread.to_string();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Event(ev) = p {
                if ev.thread_id == thread {
                    out.lock().unwrap().push(ev.event);
                }
            }
        }
    });
    seen
}

fn started(log: &Log) -> Vec<u64> {
    let mut out = Vec::new();
    for e in log.lock().unwrap().iter() {
        if let ThreadEvent::TurnStarted { message_ids, .. } = e {
            out.extend(message_ids.iter().copied());
        }
    }
    out
}

/// The life before the kill: a thread, three messages queued while the
/// thread is held (so the test owns the timing), then `stop` decides how far
/// the dispatch got. The first manager is DROPPED, and a second one built on
/// the same directory is returned — the next boot, reading only what is on disk.
enum Stop {
    /// After the append + queue row, before any attempt.
    BeforeAttempt,
    /// After the attempt committed, before `turn_started`.
    AfterAttempt,
    /// After `turn_started` named the batch.
    AfterTurnStarted,
}

fn previous_life(dir: &Path, stop: Stop) -> (String, Vec<u64>, Option<String>, ThreadManager) {
    let (id, sent, attempt) = {
        let m1 = build_manager(config_at(dir)).unwrap();
        let id = m1.create_thread(None, plain(dir)).unwrap();
        let held = m1.try_lock_thread(&id, "test").unwrap();
        let mut sent = Vec::new();
        for i in 0..3 {
            sent.push(
                m1.send(&id, format!("kill-instant {i}"), SendIntent::Immediate, Value::Null, false)
                    .unwrap(),
            );
        }
        let rows = m1.store.waiting_queue(&id).unwrap();
        assert_eq!(rows.len(), 3, "precondition: three queued rows");
        let mut attempt = None;
        match stop {
            Stop::BeforeAttempt => {}
            Stop::AfterAttempt | Stop::AfterTurnStarted => {
                attempt = Some(m1.open_attempt_for_rows(&id, &rows).unwrap());
            }
        }
        if matches!(stop, Stop::AfterTurnStarted) {
            let seq = m1.store.max_seq(&id).unwrap() + 1;
            m1.store
                .append_event(&StoredEvent {
                    thread_id: id.clone(),
                    seq,
                    message_id: None,
                    ts: threads::store::now_rfc3339(),
                    event: ThreadEvent::TurnStarted { attempt: 1, message_ids: sent.clone() },
                })
                .unwrap();
        }
        held.release();
        (id, sent, attempt)
    };
    let m2 = build_manager(config_at(dir)).unwrap();
    (id, sent, attempt, m2)
}

fn ledger(m: &ThreadManager, id: &str) -> Vec<(String, Value)> {
    m.store
        .delivery_events(id)
        .unwrap()
        .into_iter()
        .map(|(_, t, d)| (t, d))
        .collect()
}

/// **KILLED BEFORE THE ATTEMPT**: the rows are `waiting`, so the next boot
/// sends all three, in order, once, through one first attempt.
#[tokio::test(flavor = "multi_thread")]
async fn killed_after_append_before_attempt_all_three_are_sent_once() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let (id, sent, _, m) = previous_life(tmp.path(), Stop::BeforeAttempt);
    assert!(ledger(&m, &id).is_empty(), "no attempt was made before the kill");
    let log = watch(&m, &id);
    m.recover().await;
    assert!(eventually(|| started(&log).len() == 3).await, "not delivered: {:?}", started(&log));
    assert_eq!(started(&log), sent);
    let l = ledger(&m, &id);
    assert_eq!(l.len(), 1, "{l:?}");
    assert_eq!(l[0].0, delivery::ATTEMPTED);
    assert_eq!(l[0].1["message_ids"], json!(sent));
    assert_eq!(l[0].1["resend_of"], json!({}), "a first attempt resends nothing");
}

/// **KILLED BETWEEN THE ATTEMPT AND `turn_started`** — the instant §5.3 rule 5
/// is about. The next boot ends the attempt as `delivery_cancelled{restart}`,
/// re-attempts with `resend_of` naming it, and delivers all three, once each.
#[tokio::test(flavor = "multi_thread")]
async fn killed_after_attempt_before_turn_started_the_ledger_shows_it_and_all_three_are_sent() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let (id, sent, attempt, m) = previous_life(tmp.path(), Stop::AfterAttempt);
    let attempt = attempt.unwrap();
    assert!(m.store.waiting_queue(&id).unwrap().is_empty(), "precondition: rows are attempted");
    assert!(m.store.delivered_message_ids(&id).unwrap().is_empty(), "precondition: not delivered");
    let log = watch(&m, &id);
    m.recover().await;
    assert!(eventually(|| started(&log).len() == 3).await, "LOST: {:?}", started(&log));
    assert_eq!(started(&log), sent, "each once, in order");

    let l = ledger(&m, &id);
    let kinds: Vec<&str> = l.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(kinds, vec![delivery::ATTEMPTED, delivery::CANCELLED, delivery::ATTEMPTED], "{l:?}");
    assert_eq!(l[0].1["attempt"], json!(attempt));
    assert_eq!(l[1].1["attempt"], json!(attempt), "the cancel names the open attempt");
    assert_eq!(l[1].1["reason"], json!("restart"));
    assert_eq!(l[1].1["message_ids"], json!(sent));
    assert_eq!(l[1].1["dispatch_now"], json!(["eager", "eager", "eager"]), "dispatch unchanged");
    let resend: Value = sent.iter().map(|m| (m.to_string(), json!(attempt))).collect::<serde_json::Map<_, _>>().into();
    assert_eq!(l[2].1["resend_of"], resend, "the second attempt names the first");
    assert_ne!(l[2].1["attempt"], json!(attempt));
    let r: Vec<u32> = {
        let c = rusqlite::Connection::open(tmp.path().join("threads.db")).unwrap();
        let mut s = c.prepare("SELECT attempts FROM queue ORDER BY id").unwrap();
        let v = s.query_map([], |r| r.get::<_, i64>(0)).unwrap().map(|x| x.unwrap() as u32).collect();
        v
    };
    assert_eq!(r, vec![2, 2, 2], "two attempts per row");

    // A further boot finds nothing open: the cancel is not written twice.
    m.recover().await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let cancels = ledger(&m, &id).iter().filter(|(t, _)| t == delivery::CANCELLED).count();
    assert_eq!(cancels, 1);
    assert_eq!(started(&log), sent, "DUPLICATE: a delivered message went out again");
}

/// **KILLED AFTER `turn_started`**: delivered, derived. The next boot sends
/// nothing again and cancels nothing — the opposing forcing for the restore.
#[tokio::test(flavor = "multi_thread")]
async fn killed_after_turn_started_nothing_is_sent_again_past_six_150ms_resume_ticks() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let (id, sent, _, m) = previous_life(tmp.path(), Stop::AfterTurnStarted);
    let d = m.store.delivered_message_ids(&id).unwrap();
    assert!(sent.iter().all(|s| d.contains(s)), "delivered derives from turn_started: {d:?}");
    let log = watch(&m, &id);
    m.recover().await;
    // Outlast the resume tick (THREADS_RESUME_TICK_MS = 150) several times.
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(started(&log).is_empty(), "re-delivered a delivered batch: {:?}", started(&log));
    assert!(
        !ledger(&m, &id).iter().any(|(t, _)| t == delivery::CANCELLED),
        "a delivered attempt must not be cancelled"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
