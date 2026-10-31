//! The rewritten store against PRODUCTION's schema.
//!
//! `fixtures/prod-schema-2026-10-10.sql` is the `.schema` of a read-only
//! `.backup` copy of the live store — schema only. The first two tests build a
//! database from it and open it with this crate's `Store`, which is exactly
//! what the rewritten daemon will do to production's file. The third, ignored
//! by default, reads a full copy named by `TTS_LIVE_STORE_COPY` (never the live
//! file) and is run by hand when such a copy exists.

use rusqlite::{Connection, OpenFlags};
use tts::store::Store;

const PROD_SCHEMA: &str = include_str!("fixtures/prod-schema-2026-10-10.sql");
const TABLES: &[&str] = &["cache_keys", "renders", "entries", "synth_locks", "pending_unlink", "key_counters"];

/// `(name, type, notnull, default, pk)` per column, per table.
fn columns(c: &Connection) -> Vec<(String, Vec<(String, String, i64, Option<String>, i64)>)> {
    TABLES
        .iter()
        .map(|t| {
            let mut s = c.prepare(&format!("PRAGMA table_info({t})")).unwrap();
            let cols = s
                .query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (t.to_string(), cols)
        })
        .collect()
}

fn indexes(c: &Connection) -> Vec<(String, String)> {
    let mut s = c
        .prepare("SELECT name, tbl_name FROM sqlite_master WHERE type='index' ORDER BY name")
        .unwrap();
    s.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

/// A database carrying production's schema, at a temp path.
fn prod_shaped() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("tts.db");
    Connection::open(&p).unwrap().execute_batch(PROD_SCHEMA).unwrap();
    (dir, p)
}

#[test]
fn the_rewritten_store_matches_prods_schema_column_for_column() {
    let (_d, prod) = prod_shaped();
    let fresh_dir = tempfile::tempdir().unwrap();
    Store::open(fresh_dir.path().join("tts.db")).unwrap();
    // Opening production's shape must change nothing in it.
    let before = columns(&Connection::open(&prod).unwrap());
    Store::open(&prod).expect("the rewritten store opens production's schema");
    let prod_c = Connection::open(&prod).unwrap();
    assert_eq!(columns(&prod_c), before, "opening production's store altered its schema");
    let fresh_c = Connection::open(fresh_dir.path().join("tts.db")).unwrap();
    assert_eq!(indexes(&prod_c), indexes(&fresh_c));

    // Column for column, with exactly one known difference: production's
    // `cache_keys.priority` defaults to 50 (a retired scale) and a fresh store's
    // to 0. Every projection writes `priority`, so the default is never read.
    let (p, f) = (columns(&prod_c), columns(&fresh_c));
    let mut diffs = Vec::new();
    for ((t, pc), (_, fc)) in p.iter().zip(&f) {
        assert_eq!(pc.len(), fc.len(), "{t}: column count");
        for (a, b) in pc.iter().zip(fc) {
            if a != b {
                diffs.push(format!("{t}.{}: prod {:?} fresh {:?}", a.0, a.3, b.3));
                assert_eq!((&a.0, &a.1, a.2, a.4), (&b.0, &b.1, b.2, b.4), "{t}: more than a default differs");
            }
        }
    }
    assert_eq!(diffs, ["cache_keys.priority: prod Some(\"50\") fresh Some(\"0\")"]);
}

#[test]
fn the_rewritten_store_reads_and_writes_on_prods_schema() {
    let (_d, prod) = prod_shaped();
    let s = Store::open(&prod).unwrap();
    let file = tts::KeysFile::from_toml(tts::keys::DEFAULT_TTS_TOML).unwrap();
    let keys: Vec<_> = file.keys.values().map(|k| (k.clone(), "Samantha".to_string())).collect();
    let changed = s.project_keys(&keys).unwrap();
    assert_eq!(changed.len(), 4, "every launch key is new to an empty store");
    assert!(s.project_keys(&keys).unwrap().is_empty(), "a second projection changes nothing");
    let h = tts::hash::hash_for("hello", "Samantha");
    assert_eq!(s.ensure_render(&h, "system", "Samantha", 5, "aiff").unwrap(), "queued");
    s.register_entry("content", &h).unwrap();
    s.mark_warm(&h, &tts::hash::rel_path(&h, "aiff"), 2048, "aiff", 7).unwrap();
    let st = s.key_stats("content").unwrap();
    assert_eq!((st.warm, st.bytes), (1, 2048));
    assert_eq!(s.drop_entry("content", &h).unwrap(), 2048);
    assert_eq!(s.take_pending_unlink().unwrap().len(), 1);
}

/// `TTS_LIVE_STORE_COPY=<a .backup copy> cargo test -p tts --test store_schema -- --ignored`
#[test]
#[ignore = "needs TTS_LIVE_STORE_COPY: a read-only .backup copy of a live store"]
fn a_copy_of_the_live_store_has_the_fixtures_schema_and_opens_with_its_rows_intact() {
    let copy = std::env::var("TTS_LIVE_STORE_COPY").expect("TTS_LIVE_STORE_COPY");
    let ro = Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (_d, fixture) = prod_shaped();
    assert_eq!(columns(&ro), columns(&Connection::open(&fixture).unwrap()), "the copy drifted from the fixture");
    let warm: i64 = ro.query_row("SELECT COUNT(*) FROM renders WHERE state='warm'", [], |r| r.get(0)).unwrap();
    drop(ro);

    // A scratch copy OF the copy, so even the copy is never written.
    let dir = tempfile::tempdir().unwrap();
    let scratch = dir.path().join("tts.db");
    std::fs::copy(&copy, &scratch).unwrap();
    let s = Store::open(&scratch).unwrap();
    let rows = s.list_key_rows().unwrap();
    assert!(rows.iter().any(|r| r["key"] == "content" && r["mode"] == "lru"));
    let total: u64 = ["content", "interface"].iter().map(|k| s.key_stats(k).unwrap().warm).sum();
    assert!(total > 0 && total as i64 <= warm, "warm memberships {total} vs warm renders {warm}");
    eprintln!("live copy: {warm} warm renders, {} keys, {total} warm memberships in content+interface", rows.len());
}
