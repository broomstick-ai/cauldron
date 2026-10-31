//! **A POOL-SHARE OVERRIDE CAN BE CLEARED OVER THE WIRE, NOT ONLY SET** (R2.S3g).
//!
//! `accounts/{account}/pool_share_set` took `share` as a required number and
//! no method removed an override, so a key minted by an override alone —
//! `aa-owned`, written into the operator's live daemon by a test that reached
//! it: no registration, no slot, no threads — could be created by one call and
//! removed by none. The operator (2026-10-10): *"The end state should clear
//! aa-owned from my pool because its not real."*
//!
//! The door: `share: null` (present, null) DELETES the override row. An ABSENT
//! `share` is still a malformed call. After the clear, an unregistered account
//! that holds no threads has NO row in the `accounts` export; a registered one
//! keeps its row, because registration holds it.
//!
//! Every daemon here binds `127.0.0.1:0` (an ephemeral port the kernel picks)
//! and lives in a tempdir. Nothing dials a fixed port.
//!
//! Every test names the mutation that turned it red.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::net::SocketAddr;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use threads::{build_manager, Config, ThreadManager};

fn set_env() {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        first_exec::warmed_abs(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake-claude")),
    );
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-pool-share-api-home"),
    );
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
}

/// One REGISTERED account, `first`. `ghost` is never registered anywhere — it
/// stands where `aa-owned` stands on the live daemon.
async fn start() -> (ThreadManager, SocketAddr, tempfile::TempDir) {
    set_env();
    threads::clock::testing::ticking();
    let tmp = tempfile::tempdir().unwrap();
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "first".to_string(),
        tmp.path().join("first").to_string_lossy().to_string(),
    );
    let config = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    };
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config).unwrap();
    let (addr, h) = threads::chassis::serve(
        std::sync::Arc::new(m.clone()),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    std::mem::forget(h);
    (m, addr, tmp)
}

struct Client {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    n: u64,
    /// Pushes that arrived while a request waited for its response — kept,
    /// not dropped, so a removal frame is never eaten by the next call.
    pushes: Vec<Value>,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        let mut c = Self {
            ws,
            n: 0,
            pushes: Vec::new(),
        };
        c.next().await.expect("a welcome");
        c
    }

    async fn next(&mut self) -> Option<Value> {
        loop {
            let got =
                tokio::time::timeout(std::time::Duration::from_millis(900), self.ws.next()).await;
            match got {
                Err(_) | Ok(None) => return None,
                Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).ok(),
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) => return None,
            }
        }
    }

    async fn drain(&mut self) -> Vec<Value> {
        let mut out = std::mem::take(&mut self.pushes);
        while let Some(v) = self.next().await {
            out.push(v);
        }
        out
    }

    /// The OUTCOME of one request (`{type: success|error, ...}`).
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.n += 1;
        let id = format!("r{}", self.n);
        self.ws
            .send(Message::Text(
                json!({"type":"request","id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        loop {
            let f = self
                .next()
                .await
                .unwrap_or_else(|| panic!("no response to {method}"));
            if f["type"] == "response" && f["correlate"] == json!(id) {
                return f["outcome"].clone();
            }
            self.pushes.push(f);
        }
    }
}

async fn set(c: &mut Client, account: &str, params: Value) -> Value {
    c.request(&format!("accounts/{account}/pool_share_set"), params)
        .await
}

/// The share row as the daemon derives it. This read the `accounts/pool_share_get`
/// request until R2.S3c (R4.S9, HOLDS R9) retired it; the row is now read off
/// the SAME derivation (`Budgets::pool_share_rows`) the retired arm called.
fn share_row(m: &ThreadManager, account: &str) -> Value {
    m.budgets.pool_share_rows(&[account.to_string()])["shares"][0].clone()
}

/// The keys of a FRESH whole-shape `accounts` export — what a board that
/// connects right now is handed.
async fn export_keys(addr: SocketAddr) -> Vec<String> {
    subscribed(addr).await.1
}

/// A connection subscribed to the whole `accounts` shape NOW, plus the keys its
/// export handed it. A whole-shape subscriber is sent live frames for the keys
/// it was exported — so a mirror that must witness a key going `gone` is opened
/// AFTER that key exists.
async fn subscribed(addr: SocketAddr) -> (Client, Vec<String>) {
    let mut c = Client::connect(addr).await;
    let r = c
        .request(
            "shapes/accounts/subscribe",
            json!({"consumer": {"name": "threads-tests", "shape_version": "shapes.md@rev4"}}),
        )
        .await;
    assert_eq!(r["type"], "success", "{r}");
    let frames = c.drain().await;
    let mut keys: Vec<String> = frames
        .iter()
        .filter(|f| f["type"] == "shape" && f["op"] == "read")
        .flat_map(|f| {
            let mut ks = Vec::new();
            if let Some(k) = f["key"].as_str() {
                ks.push(k.to_string());
            }
            if let Some(rows) = f["rows"].as_object() {
                ks.extend(rows.keys().cloned());
            }
            ks
        })
        .collect();
    assert!(
        !frames.iter().all(|f| f["op"] != "read"),
        "a fresh subscribe must be handed an export: {frames:#?}"
    );
    keys.sort();
    keys.dedup();
    (c, keys)
}

/// The keys a stream of frames said went `gone`.
fn gone_keys(frames: &[Value]) -> Vec<String> {
    frames
        .iter()
        .filter(|f| f["type"] == "shape" && f["event"]["kind"] == json!("gone"))
        .filter_map(|f| f["key"].as_str().map(str::to_string))
        .collect()
}

/// **(1) set 0.5 on an unregistered account, then clear: no override, and the
/// row is GONE from the `accounts` export.** The operator's end state for
/// `aa-owned`.
///
/// A mirror holds the shape hot throughout, as the live board does, so the
/// removal is the FOLD's and not merely a cold re-hydrate's.
///
/// **Forced red, both halves.** (a) `Store::clear_pool_share` made a no-op
/// returning `Ok(false)`: `a row existed and went` fails, `left: Bool(false)
/// right: Bool(true)`. (b) The store DELETE kept but the `accounts` fold made to
/// keep the key (`overridden = true`): `an unregistered account holding no
/// threads has NO row once its override is cleared: ["first", "ghost"]`.
#[tokio::test]
async fn a_cleared_override_leaves_no_row_for_an_unregistered_account() {
    let (m, addr, _tmp) = start().await;
    let mut mirror = Client::connect(addr).await;
    let r = mirror
        .request(
            "shapes/accounts/subscribe",
            json!({"consumer": {"name": "threads-tests", "shape_version": "shapes.md@rev4"}}),
        )
        .await;
    assert_eq!(r["type"], "success", "{r}");
    mirror.drain().await;

    let out = set(&mut mirror, "ghost", json!({"share": 0.5})).await;
    assert_eq!(out["type"], "success", "{out}");
    assert_eq!(
        share_row(&m, "ghost")["override"],
        json!(0.5)
    );
    mirror.drain().await;
    let before = export_keys(addr).await;
    assert!(
        before.contains(&"ghost".to_string()),
        "precondition — an override alone mints the key (the `aa-owned` defect): {before:?}"
    );

    let out = set(&mut mirror, "ghost", json!({"share": null})).await;
    assert_eq!(
        out["type"], "success",
        "a null share is a clear, not an error: {out}"
    );
    assert_eq!(
        out["payload"]["changed"],
        json!(true),
        "a row existed and went: {out}"
    );
    let row = share_row(&m, "ghost");
    assert_eq!(
        row["override"],
        Value::Null,
        "no override after a clear: {row}"
    );
    assert_eq!(row["source"], json!("unset"), "{row}");
    assert_eq!(
        m.store.pool_share("ghost").unwrap(),
        None,
        "the row is deleted, not zeroed"
    );

    mirror.drain().await;
    let after = export_keys(addr).await;
    assert!(
        !after.contains(&"ghost".to_string()),
        "an unregistered account holding no threads has NO row once its override is cleared: \
         {after:?}"
    );
    assert!(
        after.contains(&"first".to_string()),
        "the registered account is untouched: {after:?}"
    );
}

/// **(2) clear on an account with no override: `changed: false`, no error —
/// and no key minted.**
///
/// **Forced red** by making `clear_pool_share` return an error when no row
/// existed: `clearing nothing is idempotent: {"code":"error","error":"no pool
/// share override for first","type":"error"}`. And by letting a non-override
/// fact MINT a key the shape does not hold: `a clear must not mint the key it
/// exists to remove: ["first", "nobody"]`.
#[tokio::test]
async fn clearing_nothing_is_not_an_error_and_changes_nothing() {
    let (_m, addr, _tmp) = start().await;
    let mut c = Client::connect(addr).await;
    // HOT, as the live board holds it: with nobody tracking, the share fact is
    // never emitted and the no-mint assertion below would hold vacuously.
    let r = c
        .request(
            "shapes/accounts/subscribe",
            json!({"consumer": {"name": "threads-tests", "shape_version": "shapes.md@rev4"}}),
        )
        .await;
    assert_eq!(r["type"], "success", "{r}");
    c.drain().await;
    for account in ["first", "nobody"] {
        let out = set(&mut c, account, json!({"share": null})).await;
        assert_eq!(
            out["type"], "success",
            "clearing nothing is idempotent: {out}"
        );
        assert_eq!(out["payload"]["changed"], json!(false), "{account}: {out}");
        assert_eq!(
            out["payload"]["visible"],
            json!(true),
            "the fact was emitted: {out}"
        );
    }
    c.drain().await;
    let keys = export_keys(addr).await;
    assert!(
        !keys.contains(&"nobody".to_string()),
        "a clear must not mint the key it exists to remove: {keys:?}"
    );
}

/// **(3) ABSENT `share` is still `missing required param` — absent ≠ null.**
/// The positive sentinel that the two are told apart: an absent share must not
/// clear the row.
///
/// **Forced red** by treating absent as null in the handler (`None |
/// Some(Value::Null) => None`): `absent is a malformed call:
/// {"payload":{"changed":true,...},"type":"success"}`.
#[tokio::test]
async fn an_absent_share_is_still_malformed_and_clears_nothing() {
    let (m, addr, _tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let out = set(&mut c, "first", json!({"share": 0.4})).await;
    assert_eq!(out["type"], "success", "{out}");

    let out = set(&mut c, "first", json!({"note": "forgot the number"})).await;
    assert_eq!(out["type"], "error", "absent is a malformed call: {out}");
    assert!(
        out["error"]
            .as_str()
            .unwrap_or_default()
            .contains("missing required param: share"),
        "{out}"
    );
    assert_eq!(
        m.store.pool_share("first").unwrap(),
        Some(0.4),
        "an absent share must not be read as a clear"
    );
}

/// **(4) a REGISTERED account's row survives its override being cleared** —
/// the key is held by registration, not by the override.
///
/// **Forced red** by dropping `registered` from the fold's `held` test, so a
/// cleared override drops registered keys too: `a registered account's key is
/// held by registration — clearing its override must not drop it: [...]` (the
/// mirror is handed `gone` for `first`). The same stream's sentinel is
/// `ghost` going `gone`. A fresh export alone stayed GREEN
/// under that mutation — the subscribe re-hydrates the roster and re-adds the
/// key — which is why this test reads the live stream.
#[tokio::test]
async fn a_registered_accounts_row_survives_its_override_being_cleared() {
    let (m, addr, _tmp) = start().await;
    // Hot first, so the fold mints `ghost` from its override.
    let (mut warm, _) = subscribed(addr).await;
    for account in ["first", "ghost"] {
        let out = set(&mut warm, account, json!({"share": 0.5})).await;
        assert_eq!(out["type"], "success", "{out}");
    }
    warm.drain().await;
    // The witness, opened after both keys exist so it is sent frames for both.
    let (mut mirror, held) = subscribed(addr).await;
    assert!(
        held.contains(&"first".to_string()) && held.contains(&"ghost".to_string()),
        "precondition — the witness holds both keys: {held:?}"
    );

    let out = set(&mut mirror, "first", json!({"share": null})).await;
    assert_eq!(out["payload"]["changed"], json!(true), "{out}");
    let row = share_row(&m, "first");
    assert_eq!(row["source"], json!("unset"), "{row}");
    let out = set(&mut mirror, "ghost", json!({"share": null})).await;
    assert_eq!(out["payload"]["changed"], json!(true), "{out}");

    // The LIVE stream, not only a fresh export: a fresh subscribe re-hydrates
    // the roster and would re-add a registered key the fold had wrongly
    // dropped, so the export alone cannot see this defect. The mirror can — it
    // would be handed `gone` for `first`. And in the SAME stream, `ghost`
    // (override-only) DOES go: the positive sentinel that this stream carries
    // `gone` at all.
    let gone = gone_keys(&mirror.drain().await);
    assert!(
        gone.contains(&"ghost".to_string()),
        "sentinel — the override-only key goes `gone` in this stream: {gone:?}"
    );
    assert!(
        !gone.contains(&"first".to_string()),
        "a registered account's key is held by registration — clearing its override must \
         not drop it: {gone:?}"
    );
    let keys = export_keys(addr).await;
    assert!(
        keys.contains(&"first".to_string()),
        "a registered account keeps its row when its override is cleared: {keys:?}"
    );
}

/// **(5) an UNREGISTERED account with a non-idle thread placed on it keeps its
/// row after `share: null`** — the thread holds the key, not the override. In
/// the same stream `ghost` (override-only, no thread) goes `gone`.
///
/// **Forced red, opposed.** (a) The fold's placed hold made `|| false`: `held`
/// goes `gone`. (b) Every key held (`held = true`): `ghost` never goes `gone`
/// and the sentinel fails.
#[tokio::test]
async fn an_unregistered_account_holding_a_thread_keeps_its_row_after_a_clear() {
    let (m, addr, _tmp) = start().await;
    let (mut warm, _) = subscribed(addr).await;
    // A non-idle thread placed on `held`, which is registered nowhere — the
    // same input the daemon's own `Status` emit gives the fold.
    m.hub.emit(threads::event::Push::Status {
        thread_id: "t-held".into(),
        state: "active".into(),
        executing: true,
        queued: 0,
        unread: 0,
        detail: None,
        account: Some("held".into()),
    });
    for account in ["held", "ghost"] {
        let out = set(&mut warm, account, json!({"share": 0.5})).await;
        assert_eq!(out["type"], "success", "{out}");
    }
    warm.drain().await;
    let (mut mirror, keys) = subscribed(addr).await;
    assert!(
        keys.contains(&"held".to_string()) && keys.contains(&"ghost".to_string()),
        "precondition — the witness holds both keys: {keys:?}"
    );

    for account in ["held", "ghost"] {
        let out = set(&mut mirror, account, json!({"share": null})).await;
        assert_eq!(out["payload"]["changed"], json!(true), "{account}: {out}");
    }
    let gone = gone_keys(&mirror.drain().await);
    assert!(
        gone.contains(&"ghost".to_string()),
        "sentinel — the override-only key goes `gone` in this stream: {gone:?}"
    );
    assert!(
        !gone.contains(&"held".to_string()),
        "a key a non-idle thread is placed on is held by that thread — clearing its \
         override must not drop it: {gone:?}"
    );
    let keys = export_keys(addr).await;
    assert!(keys.contains(&"held".to_string()), "{keys:?}");
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
