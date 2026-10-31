//! THE FACE'S SESSION, REPLAYED. No recorded tts session exists to replay, so
//! this is the next best thing: the frames the face actually sends — copied in
//! shape from harness `apps/aui/src/{tts_client.rs,synth.rs}` at 5a913c40 —
//! sent to the rewritten daemon, and every field the face actually reads off
//! the answers asserted. If the move had changed the wire, the face would read
//! a missing field as a failure; this is where that would show.

mod common;

use common::{daemon, Client, TWO_KEYS};
use serde_json::{json, Value};

/// `tts_client.rs` SUBSCRIBE_FRAME, verbatim.
const SUBSCRIBE_FRAME: &str = r#"{"type":"subscribe","keys":null}"#;

/// `synth.rs` `volatile_count_config`.
fn volatile(max_entries: u64) -> Value {
    json!({"mode": "lru_count", "max_entries": max_entries, "queueing": "fifo", "module": "system",
        "priority": 1, "min_age_secs": 0, "module_config": {"voice": "Samantha"}})
}

#[tokio::test]
async fn the_faces_session_reads_every_field_it_needs() {
    let (_m, addr, _tmp) = daemon(TWO_KEYS, None).await;
    let (mut c, welcome) = Client::connect(addr).await;

    // connection_task: `welcome` → boot_id, auth_required; then the subscribe.
    assert!(welcome["boot_id"].as_str().is_some_and(|b| !b.is_empty()));
    assert_eq!(welcome["auth_required"].as_bool(), Some(false));
    c.send_raw(serde_json::from_str(SUBSCRIBE_FRAME).unwrap()).await;
    assert_eq!(c.next_frame().await["type"], "sub_ack");

    // ensure_keys: one upsert per DECLARED key → `created`.
    let declared = [
        ("interface", json!({"mode": "keep_warm", "queueing": "fifo", "module": "system", "priority": 1,
            "min_age_secs": 120, "warn_bytes": 134217728u64, "refuse_bytes": 268435456u64,
            "module_config": {"voice": "Samantha"}})),
        ("content", json!({"mode": "lru", "max_bytes": 1073741824u64, "queueing": "fifo", "module": "system",
            "priority": 0, "min_age_secs": 120, "module_config": {"voice": "Samantha"}})),
        ("pinned-count", volatile(1)),
        ("active-count", volatile(10)),
        ("turn-activity", volatile(24)),
    ];
    for (key, config) in &declared {
        let v = c.ok("tts.upsert_key", json!({"key": key, "config": config})).await;
        assert!(v.get("created").and_then(Value::as_bool).is_some(), "{key}: {v}");
    }

    // refresh_key_facts: `tts.keys` → keys[].{key, mode, max_bytes|max_entries,
    // priority, module_fingerprint, enabled}.
    let keys = c.ok("tts.keys", json!({})).await;
    for (key, _) in &declared {
        let row = keys["keys"].as_array().unwrap().iter().find(|r| r["key"] == *key).unwrap_or_else(|| panic!("{key} missing"));
        for field in ["mode", "priority", "module_fingerprint", "enabled", "max_bytes", "max_entries"] {
            assert!(row.get(field).is_some(), "{key}: no `{field}` in {row}");
        }
    }

    // warm: `{key, items: [{text, priority?}], lane: "warm"}` → warm, queued.
    let reg = c.ok("tts.register", json!({"key": "content", "lane": "warm",
        "items": [{"text": "the face warms this"}, {"text": "and this", "priority": 6}]})).await;
    assert!(reg["warm"].as_u64().is_some() && reg["queued"].as_u64().is_some(), "{reg}");

    // render: `{key, text, wait_ms, mode: "path"}` (no hash) → path, from_cache.
    let first = c.ok("tts.request", json!({"key": "interface", "text": "Options", "wait_ms": 12000, "mode": "path"})).await;
    assert!(first["path"].as_str().is_some_and(|p| !p.is_empty()) && first["from_cache"] == false, "{first}");
    // …and with the hash the face computed itself.
    let hash = tts::hash::hash_for("Options", "Samantha");
    let again = c.ok("tts.request", json!({"key": "interface", "hash": hash, "text": "Options", "wait_ms": 12000, "mode": "path"})).await;
    assert_eq!((again["path"].as_str(), again["from_cache"].as_bool()), (first["path"].as_str(), Some(true)));

    // cancel: `{key, hashes}` → any success payload.
    c.ok("tts.cancel", json!({"key": "content", "hashes": [tts::hash::hash_for("never asked", "Samantha")]})).await;

    // An error outcome: the face matches on `code`, never on the text.
    let e = c.err("tts.request", json!({"key": "no-such-key", "text": "x", "wait_ms": 10, "mode": "path"})).await;
    assert_eq!((e["type"].as_str(), e["code"].as_str()), (Some("error"), Some("unknown_key")));
    assert!(e["error"].as_str().unwrap().starts_with("unknown_key: "));
}
