//! **`threads cutover <thread_id>` — the cutover verb's caller** (R4.S4b,
//! HOLDS R17). Drives the real binary against a throwaway data dir named by
//! an explicit `--config`; never the operator's store, never a daemon.
//!
//! **THE MUTATION.** In `main.rs`, make the `cutover` arm `exit(0)` instead
//! of `exit(2)`, or pass a constant account in `cutover_thread`.
//!
//! Forced red:
//! ```text
//! a thread with no staged copy is refused, exit 2, naming its own account's copy
//! ```

use std::path::Path;
use std::process::Command;

fn run(cfg: &Path, id: &str) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_threads"))
        .args(["cutover", id, "--config"])
        .arg(cfg)
        .env_remove("THREADS_CONFIG")
        .output()
        .unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).to_string())
}

#[test]
fn the_cutover_cli_reads_the_store_and_refuses_with_exit_2() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    {
        let store = threads::Store::open(data.join("threads.db")).unwrap();
        let now = threads::store::now_rfc3339();
        store
            .upsert_thread(&threads::store::ThreadRow {
                id: "t-cli".into(),
                account: Some("cc2".into()),
                cwd: Some("/w".into()),
                model: Some("fable".into()),
                settings_json: "{}".into(),
                last_seq: 0,
                next_message_id: 1,
                last_compaction_message_id: 0,
                needs_restate: false,
                created_at: now.clone(),
                updated_at: now,
            })
            .unwrap();
    }
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, format!("data_dir = {:?}\n", data.display().to_string())).unwrap();

    let want = data.join("raw-copies").join("cc2").join("t-cli.jsonl");
    let (rc, err) = run(&cfg, "t-cli");
    assert_eq!(rc, 2, "a thread with no staged copy is refused, exit 2, naming its own account's copy: {err}");
    assert!(err.contains(&want.display().to_string()), "the refusal names the path: {err}");

    // Staged: the verb is reached and names the unbuilt act — still exit 2.
    std::fs::create_dir_all(want.parent().unwrap()).unwrap();
    std::fs::write(&want, "{\"type\":\"user\"}\n").unwrap();
    let (rc, err) = run(&cfg, "t-cli");
    assert_eq!(rc, 2, "{err}");
    assert!(err.contains("is not built") && err.contains(&want.display().to_string()), "{err}");

    // An unknown thread is refused as unknown, not as a missing copy.
    let (rc, err) = run(&cfg, "t-nobody");
    assert_eq!(rc, 2, "{err}");
    assert!(err.contains("no thread t-nobody"), "{err}");
}

// ---------------------------------------------------------------------------
// R4.S4b.fix (HOLDS R25): `threads cutover` cannot create or migrate a store.
//
// THE MUTATION, for every guard below: in `main.rs`'s `cutover` arm, drop the
// `is_file()` refusal and open with `threads::Store::open` instead of
// `threads::Store::open_read_only`. Guard (d) is opposed the other way: make
// `open_read_only` always use `immutable=1`.
// ---------------------------------------------------------------------------

fn seed_thread(store: &threads::Store, id: &str, account: &str) {
    let now = threads::store::now_rfc3339();
    store
        .upsert_thread(&threads::store::ThreadRow {
            id: id.into(),
            account: Some(account.into()),
            cwd: Some("/w".into()),
            model: Some("fable".into()),
            settings_json: "{}".into(),
            last_seq: 0,
            next_message_id: 1,
            last_compaction_message_id: 0,
            needs_restate: false,
            created_at: now.clone(),
            updated_at: now,
        })
        .unwrap();
}

fn write_cfg(root: &Path, data: &Path) -> std::path::PathBuf {
    let cfg = root.join("config.toml");
    std::fs::write(&cfg, format!("data_dir = {:?}\n", data.display().to_string())).unwrap();
    cfg
}

/// The store's three files as (name, exists) — the db and its WAL pair.
fn side_files(data: &Path) -> Vec<(&'static str, bool)> {
    ["threads.db", "threads.db-wal", "threads.db-shm"]
        .into_iter()
        .map(|n| (n, data.join(n).exists()))
        .collect()
}

fn sha_and_mtime(p: &Path) -> (String, std::time::SystemTime) {
    use sha2::Digest;
    let bytes = std::fs::read(p).unwrap();
    let sha = format!("{:x}", sha2::Sha256::digest(&bytes));
    (sha, std::fs::metadata(p).unwrap().modified().unwrap())
}

fn columns(db: &Path, table: &str) -> Vec<String> {
    // Probe a COPY of the db and whatever `-wal`/`-shm` sit beside it, opened
    // normally so the log is replayed: a run killed by `process::exit` leaves
    // its writes in the `-wal`, and a probe of the main file alone (an
    // `immutable=1` open) is blind to them — measured: it passed the old code.
    // The copy also means the probe cannot touch the store it is measuring.
    let probe = tempfile::tempdir().unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let mut src = db.as_os_str().to_os_string();
        src.push(suffix);
        let src = std::path::PathBuf::from(src);
        if src.exists() {
            std::fs::copy(&src, probe.path().join(format!("p.db{suffix}"))).unwrap();
        }
    }
    let c = rusqlite::Connection::open(probe.path().join("p.db")).unwrap();
    let mut st = c.prepare(&format!("PRAGMA table_info({table})")).unwrap();
    let cols = st
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    cols
}

/// **(a) A MISSING STORE IS REFUSED, EXIT 2, AND NOTHING IS CREATED.** A data
/// dir with no `threads.db` (and one that does not exist at all) — the run
/// names the path it refused and leaves no `threads.db`/`-wal`/`-shm` behind.
/// Opposed: the existing-store test above still reaches the verb.
///
/// Forced red (open with `Store::open`, no `is_file()` refusal):
/// ```text
/// a missing store is not created by cutover
/// ```
#[test]
fn a_missing_store_is_refused_with_exit_2_and_never_created() {
    let tmp = tempfile::tempdir().unwrap();
    for (label, data) in [
        ("empty data dir", tmp.path().join("empty")),
        ("absent data dir", tmp.path().join("absent").join("deeper")),
    ] {
        if label == "empty data dir" {
            std::fs::create_dir_all(&data).unwrap();
        }
        let cfg = write_cfg(tmp.path(), &data);
        let (rc, err) = run(&cfg, "t-any");
        assert_eq!(
            side_files(&data),
            vec![("threads.db", false), ("threads.db-wal", false), ("threads.db-shm", false)],
            "a missing store is not created by cutover ({label}): {err}"
        );
        assert_eq!(rc, 2, "({label}) {err}");
        let want = data.join("threads.db").display().to_string();
        assert!(err.contains(&want) && err.contains("no store"), "names the path ({label}): {err}");
        if label == "absent data dir" {
            assert!(!data.exists(), "the data dir itself is not created: {err}");
        }
    }
}

/// **(b) AN EXISTING STORE IS NOT WRITTEN.** A cleanly-closed store (no
/// `-wal`/`-shm`, asserted as the precondition) is read by cutover — the verb
/// is reached and names the thread's own account's copy — and afterwards the
/// db's sha256 and mtime are unchanged and no `-wal`/`-shm` exists.
///
/// Forced red (open with `Store::open`):
/// ```text
/// cutover leaves no -wal/-shm where none existed
/// ```
#[test]
fn an_existing_store_is_read_without_a_byte_or_side_file_changing() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    {
        let store = threads::Store::open(data.join("threads.db")).unwrap();
        seed_thread(&store, "t-ro", "cc3");
    }
    assert_eq!(
        side_files(&data),
        vec![("threads.db", true), ("threads.db-wal", false), ("threads.db-shm", false)],
        "precondition: a cleanly closed store carries no WAL pair"
    );
    let db = data.join("threads.db");
    let before = sha_and_mtime(&db);
    // Outlast filesystem mtime granularity, so an in-place write the run made
    // could not land on the same timestamp.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let cfg = write_cfg(tmp.path(), &data);
    let (rc, err) = run(&cfg, "t-ro");
    assert_eq!(
        side_files(&data),
        vec![("threads.db", true), ("threads.db-wal", false), ("threads.db-shm", false)],
        "cutover leaves no -wal/-shm where none existed: {err}"
    );
    assert_eq!(sha_and_mtime(&db), before, "the db's bytes and mtime are unchanged: {err}");
    assert_eq!(rc, 2, "{err}");
    let want = data.join("raw-copies").join("cc3").join("t-ro.jsonl");
    assert!(err.contains(&want.display().to_string()), "the verb read the store: {err}");
}

/// **(c) AN OLDER-SCHEMA STORE IS NOT MIGRATED.** A store missing a column
/// `MIGRATIONS` adds (`budgets.fable_pct`) and carrying one `DROPPED_COLUMNS`
/// removes (`threads.status`) is exactly as old after cutover as before.
/// Sentinel, same instrument: `Store::open` on a copy DOES migrate it, so the
/// column probe can see a migration when one happens.
///
/// Forced red (open with `Store::open`):
/// ```text
/// cutover did not migrate the older schema
/// ```
#[test]
fn an_older_schema_store_is_not_migrated_by_cutover() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let db = data.join("threads.db");
    {
        let store = threads::Store::open(&db).unwrap();
        seed_thread(&store, "t-old", "cc4");
    }
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            "ALTER TABLE budgets DROP COLUMN fable_pct;
             ALTER TABLE threads ADD COLUMN status TEXT;",
        )
        .unwrap();
    }
    let old_budgets = columns(&db, "budgets");
    let old_threads = columns(&db, "threads");
    assert!(!old_budgets.contains(&"fable_pct".to_string()), "precondition: older schema");
    assert!(old_threads.contains(&"status".to_string()), "precondition: older schema");

    // Sentinel: the same probe sees a migration on a copy opened read-write.
    let copy_dir = tmp.path().join("copy");
    std::fs::create_dir_all(&copy_dir).unwrap();
    std::fs::copy(&db, copy_dir.join("threads.db")).unwrap();
    drop(threads::Store::open(copy_dir.join("threads.db")).unwrap());
    assert!(
        columns(&copy_dir.join("threads.db"), "budgets").contains(&"fable_pct".to_string())
            && !columns(&copy_dir.join("threads.db"), "threads").contains(&"status".to_string()),
        "sentinel: Store::open migrates this store, so the probe can see a migration"
    );

    let cfg = write_cfg(tmp.path(), &data);
    let (rc, err) = run(&cfg, "t-old");
    assert_eq!(
        (columns(&db, "budgets"), columns(&db, "threads")),
        (old_budgets, old_threads),
        "cutover did not migrate the older schema: {err}"
    );
    assert_eq!(rc, 2, "{err}");
    let want = data.join("raw-copies").join("cc4").join("t-old.jsonl");
    assert!(err.contains(&want.display().to_string()), "the verb read the store: {err}");
}

/// **(d) A LIVE WRITER'S UNCHECKPOINTED ROW IS SEEN.** The opposing case for
/// the read-only open: with a store held open (a `-wal` on disk) and the
/// thread's row only in the log, cutover still finds the thread — so the
/// read-only open is not a snapshot of the main file alone.
///
/// Forced red (`open_read_only` always `immutable=1`):
/// ```text
/// a row a live writer holds only in the WAL is seen
/// ```
#[test]
fn a_live_writers_uncheckpointed_row_is_seen_read_only() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let store = threads::Store::open(data.join("threads.db")).unwrap();
    seed_thread(&store, "t-live", "cc5");
    assert!(data.join("threads.db-wal").exists(), "precondition: a live WAL");
    let cfg = write_cfg(tmp.path(), &data);
    let (rc, err) = run(&cfg, "t-live");
    let want = data.join("raw-copies").join("cc5").join("t-live.jsonl");
    assert!(
        err.contains(&want.display().to_string()),
        "a row a live writer holds only in the WAL is seen: {err}"
    );
    assert_eq!(rc, 2, "{err}");
    drop(store);
}
