//! Fixture frames for the pool board, and a fixture chassis server that
//! speaks the shapewire envelope on `127.0.0.1:0`, serves those frames on
//! subscribe, and RECORDS every request it is sent.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

pub fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-15T12:00:00Z").unwrap().with_timezone(&Utc)
}

fn at(d: Duration) -> String {
    (now() + d).to_rfc3339()
}

fn gauge(pct: Option<f64>, resets_in: Duration) -> Value {
    match pct {
        None => Value::Null,
        Some(p) => json!({"observed_pct": p, "resets_at": at(resets_in), "confidence": 1.0, "calibrated": true}),
    }
}

/// One `accounts` row, in the daemon's shape (`shapes/accounts.rs::to_json`).
pub fn row(label: &str, slot: &str, live_login: bool, disabled: bool, session: Option<f64>) -> Value {
    json!({
        "account": label,
        "registered": true,
        "login_dir": format!("/tmp/{label}"),
        "share": {"share": 0.5, "disabled": disabled, "source": "declared"},
        "disabled": {
            "disabled": disabled,
            "disabled_at": if disabled { json!(at(-Duration::hours(3))) } else { Value::Null },
            "note": if disabled { json!("vacation") } else { Value::Null },
        },
        "gauge": {
            "session": gauge(session, Duration::minutes(192)),
            "weekly-all": gauge(session.map(|_| 72.0), Duration::hours(80)),
            "weekly-fable": Value::Null,
        },
        "limit": null,
        "health": {"healthy": true, "dead_since": null, "dead_reason": null},
        "credential": {
            "slot_state": slot,
            "is_live_login": live_login,
            "refresh_expires_at_ms": (now() + Duration::days(20)).timestamp_millis(),
        },
        "pressure": {"ranked": true, "pressure": 1.4, "share": 0.5, "disabled": disabled},
        "moves": [],
        "machine_default": null,
    })
}

pub fn export(shape: &str, key: &str, row: Value) -> Value {
    json!({
        "type": "shape", "shape": shape, "key": key, "op": "read",
        "as_of": now().to_rfc3339(),
        "epoch": {"init_at": now().to_rfc3339(), "reinit": 0, "lost": 0, "state": "known"},
        "cells": {}, "row": row,
        "shared": if shape == "accounts" { json!({"warn_below_days": 7, "folds_into": {}, "machine_login": {"label": "default", "hosted_by": "alpha", "unhosted": false}}) } else { json!({}) },
    })
}

pub fn live_row(shape: &str, key: &str, row: Value) -> Value {
    json!({"type": "shape", "shape": shape, "key": key, "op": "live", "at": now().to_rfc3339(),
           "epoch": {"reinit": 0, "lost": 0, "state": "known"}, "event": {"kind": "row", "row": row}})
}

/// The four-account pool every fixture test draws: one of each state.
pub fn accounts() -> Vec<(String, Value)> {
    vec![
        ("alpha".into(), row("alpha", "live", true, false, Some(41.0))),
        ("beta".into(), row("beta", "wiped", false, false, Some(95.0))),
        ("delta".into(), row("delta", "no-login", false, false, None)),
        ("gamma".into(), row("gamma", "live", false, true, Some(10.0))),
    ]
}

pub fn fleet() -> Value {
    json!({"pool": {}, "threads": [
        {"id": "th-aaaaaaaa1", "account": "alpha", "state": "active", "executing": true, "queued": 2, "since": null,
         "parent_id": "th-parent00"},
        {"id": "th-bbbbbbbb2", "account": "beta", "state": "parked", "executing": false, "queued": 0,
         "since": "2026-09-25T10:00:00Z", "parent_id": null},
    ]})
}

/// `threads/summary` titles, by thread id — the roster's two and the parent.
pub fn titles() -> Vec<(&'static str, &'static str)> {
    vec![
        ("th-aaaaaaaa1", "carve region R7"),
        ("th-bbbbbbbb2", "old test thread"),
        ("th-parent00", "Mishmash: v5 region carve"),
    ]
}

/// The keyed `threads` rows the board holds — parked entries only.
pub fn thread_rows() -> Vec<(&'static str, Value)> {
    vec![(
        "th-bbbbbbbb2",
        json!({"status": {"state": "parked", "executing": false,
                          "detail": "no budget config: owner-gmail"}}),
    )]
}

pub fn title_row(t: &str) -> Value {
    json!({"title": {"title": t, "idx": 0, "seq": 1, "at": now().to_rfc3339()}})
}

/// Every frame a fresh subscriber is handed.
pub fn all_frames() -> Vec<Value> {
    let mut v: Vec<Value> = accounts().into_iter().map(|(k, r)| export("accounts", &k, r)).collect();
    v.push(export("threads/non-idle", "fleet", fleet()));
    for (k, t) in titles() {
        v.push(export("threads/summary", k, title_row(t)));
    }
    for (k, r) in thread_rows() {
        v.push(export("threads", k, r));
    }
    v
}

/// A fixture chassis. `requests` holds every `type: request` it was sent, in
/// order, as `(method, params)`.
pub struct Fixture {
    pub url: String,
    pub requests: Arc<Mutex<Vec<(String, Value)>>>,
}

pub async fn serve() -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // REL-6a6: an isolated dial reaches only a port it was given.
    env::egress::grant_addr(addr).expect("grant the fixture's port");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let rec = requests.clone();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let rec = rec.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else { return };
                let send = |v: Value| Message::Text(v.to_string().into());
                if ws.send(send(json!({"type": "welcome", "proto": 1, "daemon": "threads", "auth_required": false}))).await.is_err() { return; }
                // The fixture's own state: which accounts are disabled.
                let mut rows = accounts();
                while let Some(Ok(m)) = ws.next().await {
                    let Message::Text(t) = m else { continue };
                    let v: Value = serde_json::from_str(&t).unwrap();
                    if v["type"] != "request" {
                        continue;
                    }
                    let method = v["method"].as_str().unwrap().to_string();
                    rec.lock().unwrap().push((method.clone(), v["params"].clone()));
                    let ok = |p: Value| send(json!({"type": "response", "correlate": v["id"], "outcome": {"type": "success", "payload": p}}));
                    let parts: Vec<&str> = method.split('/').collect();
                    match parts.as_slice() {
                        ["shapes", "accounts", "subscribe"] => {
                            if ws.send(ok(json!({"queued": true}))).await.is_err() { return; }
                            for (k, r) in &rows {
                                if ws.send(send(export("accounts", k, r.clone()))).await.is_err() { return; }
                            }
                        }
                        ["shapes", "threads", "non-idle", "subscribe"] => {
                            if ws.send(ok(json!({"queued": true}))).await.is_err() { return; }
                            if ws.send(send(export("threads/non-idle", "fleet", fleet()))).await.is_err() { return; }
                        }
                        ["shapes", "threads", "summary", "subscribe"] => {
                            if ws.send(ok(json!({"queued": true}))).await.is_err() { return; }
                            let key = v["params"]["key"].as_str().unwrap_or("");
                            if let Some((_, t)) = titles().into_iter().find(|(k, _)| *k == key) {
                                if ws.send(send(export("threads/summary", key, title_row(t)))).await.is_err() { return; }
                            }
                        }
                        ["shapes", "threads", "subscribe"] => {
                            if ws.send(ok(json!({"queued": true}))).await.is_err() { return; }
                            let key = v["params"]["key"].as_str().unwrap_or("");
                            if let Some((_, r)) = thread_rows().into_iter().find(|(k, _)| *k == key) {
                                if ws.send(send(export("threads", key, r))).await.is_err() { return; }
                            }
                        }
                        ["accounts", label, verb @ ("disable" | "enable")] => {
                            let dis = *verb == "disable";
                            let mut changed = false;
                            for (k, r) in rows.iter_mut() {
                                if k == label {
                                    changed = r["disabled"]["disabled"] != json!(dis);
                                    r["disabled"]["disabled"] = json!(dis);
                                    r["share"]["disabled"] = json!(dis);
                                    r["pressure"]["disabled"] = json!(dis);
                                    if ws.send(send(live_row("accounts", k, r.clone()))).await.is_err() { return; }
                                }
                            }
                            if ws.send(ok(json!({"promise_id": "p1", "visible": true, "changed": changed}))).await.is_err() { return; }
                        }
                        _ => {
                            if ws.send(send(json!({"type": "response", "correlate": v["id"],
                                "outcome": {"type": "error", "error": format!("unknown method: {method}")}}))).await.is_err() { return; }
                        }
                    }
                }
            });
        }
    });
    Fixture { url: format!("ws://{addr}/ws"), requests }
}
