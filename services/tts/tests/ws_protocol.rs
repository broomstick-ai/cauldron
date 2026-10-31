//! The websocket wire, against a real daemon on an ephemeral port with a real
//! client. THIS FILE IS WHAT SAYS THE MOVE DID NOT CHANGE THE WIRE: every
//! frame shape, error code and method name a face depends on is asserted
//! here as the substrate daemon served it.

mod common;

use std::io::{Read, Write};

use common::{daemon, items, Client, TWO_KEYS};
use serde_json::{json, Value};

/// The method inventory, read out of `src/server.rs`'s match arms and held
/// to `WS_METHODS` exactly.
#[test]
fn the_ws_method_inventory_is_pinned() {
    let src = include_str!("../src/server.rs");
    let body = &src[src.find("async fn handle(").expect("handle")..];
    let body = &body[..body.find("\nfn req_str").expect("end of handle")];
    let arms: Vec<&str> = body
        .lines()
        .filter_map(|l| l.trim().strip_prefix('"')?.split_once("\" =>").map(|(m, _)| m))
        .collect();
    let mut sorted = arms.clone();
    sorted.sort();
    assert_eq!(sorted, tts::server::WS_METHODS, "the arms and WS_METHODS disagree");
    assert!(body.contains("other => Err(Error::wire(\"unknown_method\""), "the catch-all is gone");
    // The substrate daemon's surface, all ten, unchanged.
    assert_eq!(tts::server::WS_METHODS.len(), 10);
    for m in ["tts.cancel", "tts.forget", "tts.import", "tts.keys", "tts.register", "tts.reload", "tts.request", "tts.status", "tts.touch", "tts.upsert_key"] {
        assert!(tts::server::WS_METHODS.contains(&m), "{m} left the surface");
    }
}

/// Every error this crate can put on the wire names a code from
/// `WS_ERROR_CODES` — the codes are the contract; the prose is not.
#[test]
fn the_ws_error_codes_are_pinned() {
    let mut used = Vec::new();
    for src in [
        include_str!("../src/server.rs"),
        include_str!("../src/manager.rs"),
        include_str!("../src/error.rs"),
        include_str!("../src/keys.rs"),
        include_str!("../src/import.rs"),
        include_str!("../src/synth/system.rs"),
        include_str!("../src/synth/kokoro.rs"),
    ] {
        for pat in ["Error::wire(", "Error::wire_with("] {
            for chunk in src.split(pat).skip(1) {
                if let Some(code) = chunk.trim_start().strip_prefix('"').and_then(|s| s.split('"').next()) {
                    used.push(code.to_string());
                }
            }
        }
    }
    assert!(used.len() > 10, "found only {used:?} — did wire() get renamed?");
    for code in &used {
        assert!(tts::WS_ERROR_CODES.contains(&code.as_str()), "{code:?} is not in WS_ERROR_CODES");
    }
    let mut sorted = tts::WS_ERROR_CODES.to_vec();
    sorted.sort();
    assert_eq!(sorted, tts::WS_ERROR_CODES, "WS_ERROR_CODES must stay sorted");
}

/// The frames, serialized, exactly as the face parses them.
#[test]
fn the_frame_shapes_are_the_substrate_daemons() {
    use tts::protocol::{Outcome, Push, ServerMsg};
    let welcome = ServerMsg::Welcome { proto: 1, daemon: "tts", daemon_version: "0.1.0", boot_id: "b".into(), auth_required: false };
    assert_eq!(
        serde_json::to_value(&welcome).unwrap(),
        json!({"type": "welcome", "proto": 1, "daemon": "tts", "daemon_version": "0.1.0", "boot_id": "b", "auth_required": false})
    );
    let e = tts::Error::wire_with("not_ready", "x", json!({"hash": "h"}));
    assert_eq!(
        serde_json::to_value(ServerMsg::Response { correlate: "r1".into(), outcome: Outcome::from_err(&e) }).unwrap(),
        json!({"type": "response", "correlate": "r1", "outcome": {"type": "error", "error": "not_ready: x", "code": "not_ready", "details": {"hash": "h"}}})
    );
    let push = Push::Evicted { key: "k".into(), hash: "h".into(), bytes: 3, reason: "lru".into() };
    assert_eq!(
        serde_json::to_value(ServerMsg::Push { push }).unwrap(),
        json!({"type": "push", "event": "evicted", "key": "k", "hash": "h", "bytes": 3, "reason": "lru"})
    );
    let health = Push::ModuleHealth { module: "system".into(), state: "ok".into(), reason: None };
    assert_eq!(serde_json::to_value(ServerMsg::Push { push: health }).unwrap(), json!({"type": "push", "event": "module_health", "module": "system", "state": "ok"}));
    assert_eq!(serde_json::to_value(ServerMsg::SubAck { firehose: true, keys: vec![] }).unwrap(), json!({"type": "sub_ack", "firehose": true, "keys": []}));
}

/// `GET /health` over plain HTTP/1.0 — the probe a face runs before it dials.
fn health(addr: std::net::SocketAddr) -> Value {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\n\r\n").unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    serde_json::from_str(out.split("\r\n\r\n").nth(1).expect("a body")).unwrap()
}

#[tokio::test]
async fn welcome_and_health_agree_on_the_boot() {
    let (m, addr, _t) = daemon(TWO_KEYS, None).await;
    let (_c, welcome) = Client::connect(addr).await;
    assert_eq!((welcome["type"].as_str(), welcome["daemon"].as_str(), welcome["proto"].as_u64()), (Some("welcome"), Some("tts"), Some(1)));
    assert_eq!((welcome["auth_required"].as_bool(), welcome["boot_id"].as_str()), (Some(false), Some(m.boot_id.as_str())));
    let h = tokio::task::spawn_blocking(move || health(addr)).await.unwrap();
    assert_eq!((h["status"].as_str(), h["daemon"].as_str(), h["boot_id"].as_str()), (Some("ok"), Some("tts"), Some(m.boot_id.as_str())));
}

#[tokio::test]
async fn a_secret_gates_every_frame_until_auth() {
    let (_m, addr, _t) = daemon(TWO_KEYS, Some("s3cret")).await;
    let (mut c, welcome) = Client::connect(addr).await;
    assert_eq!(welcome["auth_required"], true);
    let refused = c.request("tts.keys", json!({})).await;
    assert_eq!((refused["type"].as_str(), refused["code"].as_str()), (Some("error"), Some("bad_params")));
    c.send_raw(json!({"type": "auth", "secret": "s3cret"})).await;
    assert_eq!(c.next_frame().await, json!({"type": "auth_ack", "ok": true}));
    assert_eq!(c.ok("tts.keys", json!({})).await["keys"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn coded_errors_on_the_wire() {
    let (_m, addr, _t) = daemon(TWO_KEYS, None).await;
    let (mut c, _) = Client::connect(addr).await;
    let e = c.err("tts.nope", json!({})).await;
    assert!(e["code"] == "unknown_method" && e["error"].as_str().unwrap().starts_with("unknown_method: "));
    let h = common::hash_of("something else");
    assert_eq!(c.err("tts.request", json!({"key": "content", "hash": h, "text": "not that"})).await["code"], "hash_mismatch");
    assert_eq!(c.err("tts.request", json!({"key": "content", "hash": h})).await["code"], "unknown_hash");
    assert_eq!(c.err("tts.request", json!({"key": "content", "text": "x", "mode": "bytes"})).await["code"], "bad_params");
    assert_eq!(c.err("tts.status", json!({"key": "nope"})).await["code"], "unknown_key");
    assert_eq!(c.err("tts.register", json!({"key": "content", "items": [{"text": "x", "priority": "high"}]})).await["code"], "bad_params");
    c.send_raw(json!({"type": "gibberish"})).await;
    let f = c.next_frame().await;
    assert_eq!((f["type"].as_str(), f["outcome"]["code"].as_str()), (Some("response"), Some("bad_params")));
}

#[tokio::test]
async fn register_then_request_is_a_path_and_then_a_cache_hit() {
    let (_m, addr, tmp) = daemon(TWO_KEYS, None).await;
    let (mut c, _) = Client::connect(addr).await;
    let reg = c.ok("tts.register", json!({"key": "content", "items": items(&["hello there"])})).await;
    assert_eq!(reg["queued"], 1);
    let hash = reg["items"][0]["hash"].as_str().unwrap().to_string();
    let got = c.ok("tts.request", json!({"key": "content", "text": "hello there", "wait_ms": 8000})).await;
    assert_eq!((got["state"].as_str(), got["format"].as_str(), got["hash"].as_str()), (Some("warm"), Some("aiff"), Some(hash.as_str())));
    let path = got["path"].as_str().unwrap();
    assert!(std::path::Path::new(path).exists() && path.contains(&format!("/audio/{}/{}/", &hash[0..2], &hash[2..4])));
    assert_eq!(c.ok("tts.request", json!({"key": "content", "hash": hash})).await["from_cache"], true);
    assert_eq!(common::say_calls(tmp.path()).len(), 1);
}

#[tokio::test]
async fn not_ready_is_an_error_with_its_details() {
    let (_m, addr, tmp) = daemon(TWO_KEYS, None).await;
    common::say_takes_ms(tmp.path(), 3000);
    let (mut c, _) = Client::connect(addr).await;
    let e = c.err("tts.request", json!({"key": "content", "text": "slow", "wait_ms": 50})).await;
    assert_eq!(e["code"], "not_ready");
    assert!(e["details"]["hash"].is_string() && e["details"]["state"].is_string() && e["details"].get("queue_position").is_some(), "{e}");
}

#[tokio::test]
async fn touch_status_keys_and_forget() {
    let (_m, addr, _t) = daemon(TWO_KEYS, None).await;
    let (mut c, _) = Client::connect(addr).await;
    let got = c.ok("tts.request", json!({"key": "content", "text": "touch me", "wait_ms": 8000})).await;
    let (hash, path) = (got["hash"].as_str().unwrap().to_string(), got["path"].as_str().unwrap().to_string());
    let t = c.ok("tts.touch", json!({"key": "content", "hashes": [hash, common::hash_of("never")]})).await;
    assert_eq!((t["touched"].as_u64(), t["unknown"].as_array().map(Vec::len)), (Some(1), Some(1)));
    let keys = c.ok("tts.keys", json!({})).await;
    let content = keys["keys"].as_array().unwrap().iter().find(|k| k["key"] == "content").unwrap().clone();
    assert_eq!((content["mode"].as_str(), content["module_fingerprint"].as_str()), (Some("lru"), Some("Samantha")));
    let st = c.ok("tts.status", json!({})).await;
    assert_eq!((st["global"]["max_concurrent"].as_u64(), st["global"]["module_health"]["system"]["state"].as_str()), (Some(4), Some("ok")));
    assert_eq!(st["keys"].as_array().unwrap().len(), 2);
    let out = c.ok("tts.forget", json!({"key": "content", "hashes": [hash]})).await;
    assert_eq!(out["forgotten"], 1);
    assert!(out["freed_bytes"].as_u64().unwrap() > 0 && !std::path::Path::new(&path).exists());
}

/// Pushes are filtered by key; none are persisted.
#[tokio::test]
async fn pushes_are_transient_and_filtered_by_key() {
    let (_m, addr, _t) = daemon(TWO_KEYS, None).await;
    let (mut c, _) = Client::connect(addr).await;
    c.send_raw(json!({"type": "subscribe", "keys": ["interface"]})).await;
    assert_eq!(c.next_frame().await["type"], "sub_ack");
    c.ok("tts.register", json!({"key": "content", "items": items(&["not my key"])})).await;
    c.ok("tts.register", json!({"key": "interface", "items": items(&["my key"])})).await;
    let pushes = c.collect_pushes(1500).await;
    let warmed: Vec<&Value> = pushes.iter().filter(|p| p["event"] == "warmed").collect();
    assert!(!warmed.is_empty(), "no warmed push: {pushes:?}");
    assert!(warmed.iter().all(|w| w["key"] == "interface"), "a push leaked across the filter");
}
