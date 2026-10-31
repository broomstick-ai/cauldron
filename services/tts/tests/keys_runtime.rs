//! Keys are data, managed from outside, at runtime — and nothing asks for a
//! restart. "Why are you restarting the service at all? And definitely, like,
//! why are you asking me to do it?" — the operator

mod common;

use common::{built_manager, daemon, items, manager, manager_with, Client, TWO_KEYS};
use serde_json::{json, Value};
use tts::queue::Lane;

fn count_key(n: u64, extra: Value) -> Value {
    let mut v = json!({"mode": "lru_count", "max_entries": n, "queueing": "fifo", "module": "system",
        "priority": 100, "min_age_secs": 0, "module_config": {"voice": "Samantha"}});
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    v
}

/// A third key is one stanza; a different voice is a different fingerprint
/// and so different hashes — a voice swap needs no invalidation code.
#[tokio::test]
async fn a_third_key_needs_no_code() {
    let three = format!(
        "{TWO_KEYS}\n[keys.thread-names]\nmode = \"lru\"\nmax_bytes = \"64MiB\"\nqueueing = \"lifo\"\nmodule = \"system\"\n\
         priority = 75\nmin_age_secs = 0\n[keys.thread-names.module_config]\nvoice = \"Alex\"\n"
    );
    let (m, _t) = manager_with(&three);
    assert_eq!(m.key_names(), ["content", "interface", "thread-names"]);
    for key in ["interface", "content", "thread-names"] {
        assert_eq!(m.request(key, None, Some("a thread name".into()), 5000, "path").await.unwrap()["state"], "warm");
    }
    let (a, c) = (m.key("content").unwrap(), m.key("thread-names").unwrap());
    assert_eq!((a.fingerprint.as_str(), c.fingerprint.as_str(), c.cfg.queueing), ("Samantha", "Alex", tts::Queueing::Lifo));
}

/// A key gone from the file is DISABLED, not purged: refused for service,
/// every byte and row where it was.
#[tokio::test]
async fn reload_disables_a_removed_key_and_keeps_every_byte() {
    let three = format!(
        "{TWO_KEYS}\n[keys.scratch]\nmode = \"lru\"\nmax_bytes = \"8MiB\"\nqueueing = \"fifo\"\nmodule = \"system\"\n\
         min_age_secs = 0\n[keys.scratch.module_config]\nvoice = \"Samantha\"\n"
    );
    let (m, tmp) = manager_with(&three);
    let r = m.request("scratch", None, Some("keep me".into()), 5000, "path").await.unwrap();
    let (path, hash) = (r["path"].as_str().unwrap().to_string(), r["hash"].as_str().unwrap().to_string());
    std::fs::write(tmp.path().join("tts.toml"), TWO_KEYS).unwrap();
    assert!(m.reload().unwrap()["changed"].as_array().unwrap().iter().any(|c| c == "scratch"));
    assert_eq!(m.key("scratch").err().and_then(|e| e.code()), Some("unknown_key"));
    assert!(std::path::Path::new(&path).exists() && m.store.has_entry("scratch", &hash).unwrap());
    let rows = m.store.list_key_rows().unwrap();
    assert_eq!(rows.iter().find(|r| r["key"] == "scratch").unwrap()["enabled"], false);
}

/// The gate width is resized live in both directions; no reply says
/// `restart_required`.
#[tokio::test]
async fn a_concurrency_change_is_applied_live_in_both_directions() {
    let (m, tmp) = manager();
    for (want, was) in [(2, 4), (6, 2)] {
        std::fs::write(tmp.path().join("tts.toml"), TWO_KEYS.replace("max_concurrent   = 4", &format!("max_concurrent   = {want}"))).unwrap();
        let out = m.reload().unwrap();
        assert!(out.get("restart_required").is_none(), "{out}");
        assert_eq!((out["max_concurrent"].as_u64(), out["max_concurrent_was"].as_u64()), (Some(want), Some(was)));
        assert_eq!(m.globals().max_concurrent, want as usize);
    }
}

/// A fresh data dir seeds its launch keys, and a count key is as well formed
/// in the database as a byte key.
#[tokio::test]
async fn a_fresh_data_dir_seeds_the_launch_keys_into_the_store() {
    let (m, tmp) = built_manager();
    assert!(tmp.path().join("tts.toml").exists());
    assert_eq!(m.key_names(), ["active-count", "content", "interface", "pinned-count"]);
    assert_eq!((m.globals().max_concurrent, m.globals().max_bytes), (4, 1536 << 20));
    let rows = m.store.list_key_rows().unwrap();
    let pinned = rows.iter().find(|r| r["key"] == "pinned-count").expect("projected");
    assert_eq!((pinned["mode"].as_str(), pinned["max_entries"].as_u64(), pinned["max_bytes"].is_null()), (Some("lru_count"), Some(1), true));
    assert_eq!(m.status(Some("active-count")).unwrap()["keys"][0]["max_entries"], 10);
}

#[test]
fn an_unknown_module_is_refused_at_load_and_kokoro_by_name() {
    common::set_env();
    for (module, says) in [("espeak", "espeak"), ("kokoro", "no synthesis backend")] {
        let bad = TWO_KEYS.replace("module        = \"system\"", &format!("module        = \"{module}\""));
        let tmp = tempfile::tempdir().unwrap();
        let err = tts::TtsManager::new(
            tts::Store::open(tmp.path().join("tts.db")).unwrap(),
            tts::Config::default(),
            tmp.path().to_path_buf(),
            tts::KeysFile::from_toml(&bad).unwrap(),
        )
        .err()
        .expect("accepted at load");
        assert!(err.to_string().contains(says), "{module}: {err}");
    }
}

/// THE HEADLINE: a daemon that never heard of `pinned-count` is handed it by
/// a client and serves it on the next frame — same process, same boot id.
#[tokio::test]
async fn a_key_the_daemon_never_heard_of_is_created_by_a_client_and_served_at_once() {
    let (m, addr, _tmp) = daemon(TWO_KEYS, None).await;
    let (mut c, _) = Client::connect(addr).await;
    let boot = m.boot_id.clone();
    let reg = json!({"key": "pinned-count", "items": items(&["Pinned. none."])});
    assert_eq!(c.err("tts.register", reg.clone()).await["code"], "unknown_key");
    let r = c.ok("tts.upsert_key", json!({"key": "pinned-count", "config": count_key(1, json!({}))})).await;
    assert_eq!((r["created"].as_bool(), r["config"]["max_entries"].as_u64()), (Some(true), Some(1)));
    assert_eq!(c.ok("tts.register", reg).await["refused"], 0);
    let got = c.ok("tts.request", json!({"key": "pinned-count", "text": "Pinned. none.", "wait_ms": 8000})).await;
    assert_eq!(got["state"], "warm");
    assert_eq!(m.boot_id, boot, "the daemon restarted");
}

/// Declared on every client boot: the redeclaration is a no-op and never
/// clobbers what the operator tuned; `replace` reconfigures the live key.
#[tokio::test]
async fn redeclaring_is_idempotent_and_replace_reconfigures_live() {
    let (m, addr, _tmp) = daemon(TWO_KEYS, None).await;
    let (mut c, _) = Client::connect(addr).await;
    c.ok("tts.upsert_key", json!({"key": "active-count", "config": count_key(10, json!({}))})).await;
    c.ok("tts.upsert_key", json!({"key": "active-count", "replace": true,
        "config": count_key(25, json!({"queueing": "lifo", "priority": 42, "module_config": {"voice": "Alex"}}))})).await;
    let again = c.ok("tts.upsert_key", json!({"key": "active-count", "config": count_key(10, json!({}))})).await;
    assert_eq!((again["created"].as_bool(), again["updated"].as_bool(), again["config"]["max_entries"].as_u64()), (Some(false), Some(false), Some(25)));
    let rk = m.key("active-count").unwrap();
    assert_eq!((rk.cfg.max_entries, rk.cfg.priority, rk.cfg.queueing.as_str()), (Some(25), 42, "lifo"));
    assert_eq!(rk.module_config["voice"], "Alex");
}

/// Written into tts.toml with the operator's comments intact, and found
/// again by a reload.
#[tokio::test]
async fn an_upserted_key_lands_in_tts_toml_with_his_comments_intact() {
    let annotated = format!("# HIS OWN NOTE: one gigabyte, msg #620.\n{TWO_KEYS}");
    let (_m, addr, tmp) = daemon(&annotated, None).await;
    let (mut c, _) = Client::connect(addr).await;
    c.ok("tts.upsert_key", json!({"key": "pinned-count", "config": count_key(1, json!({}))})).await;
    let text = std::fs::read_to_string(tmp.path().join("tts.toml")).unwrap();
    assert!(text.contains("HIS OWN NOTE") && text.contains("[keys.pinned-count]") && text.contains("upsert_key"), "{text}");
    let reloaded = c.ok("tts.reload", json!({})).await;
    assert!(reloaded["keys"].as_array().unwrap().iter().any(|k| k["key"] == "pinned-count"));
}

/// A malformed declaration is a coded refusal and the file is untouched: a
/// tts.toml the daemon cannot read is the one failure that needs a human.
#[tokio::test]
async fn an_invalid_declaration_is_refused_and_the_file_untouched() {
    let (m, addr, tmp) = daemon(TWO_KEYS, None).await;
    let before = std::fs::read_to_string(tmp.path().join("tts.toml")).unwrap();
    let (mut c, _) = Client::connect(addr).await;
    for config in [
        json!({"mode": "lru", "queueing": "fifo", "module": "system"}),
        json!({"mode": "lru_count", "queueing": "fifo", "module": "system"}),
        json!({"mode": "lru_count", "max_entries": 1, "max_bytes": 16, "queueing": "fifo", "module": "system"}),
        json!({"mode": "lru_count", "max_entries": 0, "queueing": "fifo", "module": "system"}),
        json!({"mode": "keep_warm", "queueing": "fifo", "module": "telepathy"}),
        json!({"mode": "vibes", "queueing": "fifo", "module": "system"}),
    ] {
        assert_eq!(c.err("tts.upsert_key", json!({"key": "bad", "config": config})).await["code"], "bad_params", "{config}");
    }
    assert_eq!(c.err("tts.upsert_key", json!({"key": "not a slug", "config": count_key(1, json!({}))})).await["code"], "bad_params");
    assert_eq!(std::fs::read_to_string(tmp.path().join("tts.toml")).unwrap(), before);
    assert!(!m.key_names().contains(&"bad".to_string()));
}

/// No reply this crate can produce asks for a restart — checked in the
/// source, because a payload test would miss a method it does not call.
#[test]
fn nothing_in_this_crate_can_ask_for_a_restart() {
    for (file, src) in [
        ("manager.rs", include_str!("../src/manager.rs")),
        ("server.rs", include_str!("../src/server.rs")),
        ("keys.rs", include_str!("../src/keys.rs")),
        ("store.rs", include_str!("../src/store.rs")),
        ("import.rs", include_str!("../src/import.rs")),
    ] {
        for (i, line) in src.lines().enumerate() {
            assert!(!line.split("//").next().unwrap_or("").contains("restart_required"), "{file}:{}: {line}", i + 1);
        }
    }
}

/// `enabled = false` in the file is a disabled key, refused as such; a key
/// the file no longer names is simply unknown. Both are `unknown_key`.
#[tokio::test]
async fn a_disabled_key_and_a_removed_key_are_both_refused() {
    let (m, tmp) = manager();
    let disabled = TWO_KEYS.replace("[keys.interface.module_config]", "enabled       = false\n[keys.interface.module_config]");
    std::fs::write(tmp.path().join("tts.toml"), &disabled).unwrap();
    m.reload().unwrap();
    let e = m.register("interface", vec!["x".into()], Lane::Warm).await.unwrap_err();
    assert!(e.code() == Some("unknown_key") && e.to_string().contains("disabled"), "{e}");
    let removed = TWO_KEYS.replace("[keys.interface]", "[keys.chrome]").replace("[keys.interface.module_config]", "[keys.chrome.module_config]");
    std::fs::write(tmp.path().join("tts.toml"), &removed).unwrap();
    m.reload().unwrap();
    let e = m.register("interface", vec!["x".into()], Lane::Warm).await.unwrap_err();
    assert!(e.code() == Some("unknown_key") && e.to_string().contains("no such cache key"), "{e}");
}
