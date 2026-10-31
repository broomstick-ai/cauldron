//! `tts.import` — adoption by hash, by hardlink. Every source directory here
//! is a fixture in a tempdir; nothing touches a real cache.

mod common;

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use common::{manager, Client};
use serde_json::json;
use tts::import::{import, ImportMode};

/// `texts` as correctly named files, plus the junk a real directory holds:
/// an in-flight temp, a zero-byte file, and a name that is not a hash.
fn fake_cache(dir: &Path, texts: &[&str], voice: &str) -> u64 {
    std::fs::create_dir_all(dir).unwrap();
    for t in texts {
        std::fs::write(dir.join(format!("{}.aiff", tts::hash::hash_for(t, voice))), [7u8; 2048]).unwrap();
    }
    std::fs::write(dir.join(format!("{}.warm.aiff", "a".repeat(64))), b"xx").unwrap();
    std::fs::write(dir.join(format!("{}.aiff", "b".repeat(64))), b"").unwrap();
    std::fs::write(dir.join("notes.txt"), b"hello").unwrap();
    2048 * texts.len() as u64
}

#[tokio::test]
async fn a_dry_run_reports_the_real_numbers_and_writes_nothing() {
    let (m, _t) = manager();
    let src = tempfile::tempdir().unwrap();
    let bytes = fake_cache(src.path(), &["one", "two", "three"], "Samantha");
    let rep = import(&m, "content", src.path(), None, ImportMode::Hardlink, true).unwrap();
    for (k, v) in [("adopted", json!(3)), ("bytes", json!(bytes)), ("scanned", json!(6)), ("skipped", json!(3)), ("would_delete", json!(0)), ("fingerprint", json!("Samantha"))] {
        assert_eq!(rep[k], v, "{k}: {rep}");
    }
    assert_eq!(m.store.key_stats("content").unwrap().entries, 0);
    assert_eq!(std::fs::read_dir(m.audio_dir()).unwrap().count(), 0, "a dry run wrote audio");
}

/// Hardlinked (one inode, two links), warm, and the source left exactly as
/// it was.
#[tokio::test]
async fn a_real_run_hardlinks_and_never_touches_the_source() {
    let (m, _t) = manager();
    let src = tempfile::tempdir().unwrap();
    fake_cache(src.path(), &["alpha", "beta"], "Samantha");
    let before: Vec<_> = std::fs::read_dir(src.path()).unwrap().flatten().map(|e| e.file_name()).collect();
    let rep = import(&m, "content", src.path(), None, ImportMode::Hardlink, false).unwrap();
    assert_eq!(rep["adopted"], 2);
    for t in ["alpha", "beta"] {
        let h = tts::hash::hash_for(t, "Samantha");
        let r = m.store.get_render(&h).unwrap().expect("adopted");
        assert_eq!(r.state, "warm");
        let dest = m.abs_path(&r.rel_path.unwrap());
        let (a, b) = (std::fs::metadata(src.path().join(format!("{h}.aiff"))).unwrap(), std::fs::metadata(&dest).unwrap());
        assert_eq!((a.ino(), b.nlink()), (b.ino(), 2), "the importer copied instead of linking");
    }
    let after: Vec<_> = std::fs::read_dir(src.path()).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(before, after, "the source directory changed");
}

/// The whole point: an adopted file is served from cache with no synthesis.
#[tokio::test]
async fn an_adopted_entry_is_served_from_cache_with_no_synthesis() {
    let (m, tmp) = manager();
    let src = tempfile::tempdir().unwrap();
    fake_cache(src.path(), &["already spoken"], "Samantha");
    import(&m, "content", src.path(), None, ImportMode::Hardlink, false).unwrap();
    let r = m.request("content", None, Some("Already   Spoken".into()), 1000, "path").await.unwrap();
    assert_eq!(r["from_cache"], true);
    assert!(common::say_calls(tmp.path()).is_empty(), "an adopted entry was synthesized");
}

/// Recency comes from the SOURCE file's mtime, so the face's LRU order
/// survives the move.
#[tokio::test]
async fn recency_is_seeded_from_the_source_mtime() {
    let (m, _t) = manager();
    let src = tempfile::tempdir().unwrap();
    let (old_h, new_h) = (tts::hash::hash_for("older one", "Samantha"), tts::hash::hash_for("newer one", "Samantha"));
    std::fs::write(src.path().join(format!("{old_h}.aiff")), [7u8; 2048]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(src.path().join(format!("{new_h}.aiff")), [7u8; 2048]).unwrap();
    let cutoff = tts::store::now_rfc3339();
    import(&m, "content", src.path(), None, ImportMode::Hardlink, false).unwrap();
    let (older, newer) = (m.store.entry("content", &old_h).unwrap().unwrap(), m.store.entry("content", &new_h).unwrap().unwrap());
    assert!(older.last_used_at < newer.last_used_at, "the order was lost: {older:?} {newer:?}");
    assert!(newer.last_used_at < cutoff, "recency is import time, not the file's");
}

/// THE TRIPWIRE: naming another voice is refused, with the reason.
#[tokio::test]
async fn a_voice_mismatch_is_refused_rather_than_orphaning_everything() {
    let (m, _t) = manager();
    let src = tempfile::tempdir().unwrap();
    fake_cache(src.path(), &["one"], "Samantha");
    let e = import(&m, "content", src.path(), Some(json!({"voice": "Alex"})), ImportMode::Hardlink, true).unwrap_err();
    assert_eq!(e.code(), Some("bad_params"));
    assert!(e.to_string().contains("Alex"), "{e}");
    assert!(import(&m, "content", src.path(), Some(json!({"voice": "Samantha"})), ImportMode::Hardlink, true).is_ok());
}

/// Twice is `already_present`; two overlapping caches adopt the overlap once.
#[tokio::test]
async fn importing_twice_or_overlapping_adopts_each_hash_once() {
    let (m, _t) = manager();
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    fake_cache(a.path(), &["shared beat", "only in a"], "Samantha");
    fake_cache(b.path(), &["shared beat", "only in b"], "Samantha");
    assert_eq!(import(&m, "content", a.path(), None, ImportMode::Hardlink, false).unwrap()["adopted"], 2);
    let again = import(&m, "content", a.path(), None, ImportMode::Hardlink, false).unwrap();
    assert_eq!((again["adopted"].as_u64(), again["already_present"].as_u64()), (Some(0), Some(2)));
    let rb = import(&m, "content", b.path(), None, ImportMode::Hardlink, false).unwrap();
    assert_eq!((rb["adopted"].as_u64(), rb["already_present"].as_u64()), (Some(1), Some(1)));
    assert_eq!(m.store.key_stats("content").unwrap().entries, 3);
}

/// The wire method: a dry run unless asked otherwise, `~/` expanded.
#[tokio::test]
async fn tts_import_on_the_wire_is_a_dry_run_by_default() {
    let (m, addr, _t) = common::daemon(common::TWO_KEYS, None).await;
    let src = tempfile::tempdir().unwrap();
    fake_cache(src.path(), &["over the wire"], "Samantha");
    let (mut c, _) = Client::connect(addr).await;
    let rep = c.ok("tts.import", json!({"key": "content", "from_dir": src.path()})).await;
    assert_eq!((rep["dry_run"].as_bool(), rep["adopted"].as_u64()), (Some(true), Some(1)));
    assert_eq!(m.store.key_stats("content").unwrap().entries, 0);
    assert_eq!(c.err("tts.import", json!({"key": "content", "from_dir": src.path(), "mode": "symlink"})).await["code"], "bad_params");
    let real = c.ok("tts.import", json!({"key": "content", "from_dir": src.path(), "dry_run": false})).await;
    assert_eq!(real["adopted"], 1);
    assert_eq!(tts::import::expand_home("~/x"), Path::new(&std::env::var("HOME").unwrap()).join("x"));
}
