//! The websocket daemon: `GET /ws` and an unauthenticated `GET /health`, off
//! one listener.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::import::{self, ImportMode};
use crate::manager::{RegisterItem, TtsManager};
use crate::protocol::{ClientMsg, Outcome, ServerMsg, PROTO};
use crate::queue::Lane;

const DAEMON_NAME: &str = "tts";
const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Every request method [`handle`] answers. `tests/ws_protocol.rs` reads the
/// match arms out of this file and requires exactly this list — the surface
/// `threads` once recorded as 22 methods had quietly become 24 because nothing
/// pinned it. Every arm is written `"tts.x" => ...` on one line.
pub const WS_METHODS: &[&str] = &[
    "tts.cancel",
    "tts.forget",
    "tts.import",
    "tts.keys",
    "tts.register",
    "tts.reload",
    "tts.request",
    "tts.status",
    "tts.touch",
    "tts.upsert_key",
];

pub fn router(manager: TtsManager) -> Router {
    Router::new().route("/ws", get(ws_handler)).route("/health", get(health)).with_state(Arc::new(manager))
}

/// Bind the websocket address. Separate from [`serve`] so the daemon can hold
/// its port BEFORE it opens the store: a second daemon pointed at an address
/// that is already held fails here, before it has swept anything.
pub async fn bind(addr: SocketAddr) -> Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(addr).await.map_err(|e| Error::Config(format!("binding {addr}: {e}")))
}

/// Serve on a bound listener; returns the bound address and the server task.
pub fn serve(manager: TtsManager, listener: tokio::net::TcpListener) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let bound = listener.local_addr().map_err(|e| Error::Config(format!("local_addr: {e}")))?;
    manager.start();
    let app = router(manager);
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "tts server error");
        }
    });
    Ok((bound, handle))
}

async fn health(State(m): State<Arc<TtsManager>>) -> impl IntoResponse {
    Json(json!({"status": "ok", "daemon": DAEMON_NAME, "version": DAEMON_VERSION, "proto": PROTO, "boot_id": m.boot_id}))
}

async fn ws_handler(ws: WebSocketUpgrade, State(m): State<Arc<TtsManager>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| drive(socket, m))
}

type Tx = SplitSink<WebSocket, Message>;

async fn send(tx: &mut Tx, msg: &ServerMsg) -> std::result::Result<(), ()> {
    let text = serde_json::to_string(msg).map_err(|_| ())?;
    tx.send(Message::Text(text)).await.map_err(|_| ())
}

/// One client connection.
async fn drive(socket: WebSocket, m: Arc<TtsManager>) {
    let (mut tx, mut rx) = socket.split();
    let mut events = m.subscribe();
    let secret = m.config.effective_secret();
    let mut authed = secret.is_none();
    let (mut firehose, mut keys) = (false, HashSet::<String>::new());
    let welcome = ServerMsg::Welcome {
        proto: PROTO,
        daemon: DAEMON_NAME,
        daemon_version: DAEMON_VERSION,
        boot_id: m.boot_id.clone(),
        auth_required: !authed,
    };
    if send(&mut tx, &welcome).await.is_err() {
        return;
    }
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;
    loop {
        let reply = tokio::select! {
            incoming = rx.next() => {
                let text = match incoming {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => continue,
                };
                match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(ClientMsg::Auth { secret: given }) => {
                        let ok = secret.as_ref().map_or(true, |s| constant_time_eq(s.as_bytes(), given.as_bytes()));
                        authed |= ok;
                        if send(&mut tx, &ServerMsg::AuthAck { ok }).await.is_err() || !ok {
                            break;
                        }
                        continue;
                    }
                    Ok(other) if !authed => {
                        let correlate = match other {
                            ClientMsg::Request { id, .. } => id,
                            _ => String::new(),
                        };
                        let e = Error::bad_params("unauthorized: send an auth frame first");
                        ServerMsg::Response { correlate, outcome: Outcome::from_err(&e) }
                    }
                    Ok(ClientMsg::Request { id, method, params }) => {
                        let outcome = match handle(&m, &method, params).await {
                            Ok(payload) => Outcome::ok(payload),
                            Err(e) => Outcome::from_err(&e),
                        };
                        ServerMsg::Response { correlate: id, outcome }
                    }
                    Ok(ClientMsg::Subscribe { keys: k }) => {
                        match k {
                            None => firehose = true,
                            Some(list) => keys.extend(list),
                        }
                        ServerMsg::SubAck { firehose, keys: keys.iter().cloned().collect() }
                    }
                    Ok(ClientMsg::Unsubscribe { keys: k }) => {
                        match k {
                            None => (firehose, keys) = (false, HashSet::new()),
                            Some(list) => list.iter().for_each(|id| { keys.remove(id); }),
                        }
                        ServerMsg::SubAck { firehose, keys: keys.iter().cloned().collect() }
                    }
                    Err(e) => ServerMsg::Response {
                        correlate: String::new(),
                        outcome: Outcome::from_err(&Error::bad_params(format!("invalid frame: {e}"))),
                    },
                }
            }
            ev = events.recv() => match ev {
                Ok(push) if authed && push.key().map_or(true, |k| firehose || keys.contains(k)) => ServerMsg::Push { push },
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            },
            _ = ping.tick() => {
                if tx.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
                continue;
            }
        };
        if send(&mut tx, &reply).await.is_err() {
            break;
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn handle(m: &TtsManager, method: &str, params: Value) -> Result<Value> {
    match method {
        // { key, items: [{text, priority?}], lane?: "warm"|"interactive" }
        // -> { items: [{hash, state, action, text_used, bytes?, priority?}],
        //      warm, queued, refused, enqueued, joined, touched, discarded }
        // A PUT per item; priority is per ITEM. A priority that is present and
        // not an integer is REFUSED — a dropped priority is a silent reorder.
        "tts.register" => {
            let key = req_str(&params, "key")?;
            let items = params.get("items").and_then(Value::as_array).ok_or_else(|| Error::bad_params("items must be an array of {text}"))?;
            let mut batch = Vec::with_capacity(items.len());
            for it in items {
                let text = it.get("text").and_then(Value::as_str).ok_or_else(|| Error::bad_params("every item needs a string `text`"))?;
                let priority = match it.get("priority") {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(v.as_i64().ok_or_else(|| Error::bad_params(format!("priority must be an integer, got {v}")))?),
                };
                batch.push(RegisterItem { text: text.to_string(), priority });
            }
            let lane = match params.get("lane").and_then(Value::as_str) {
                None => Lane::Warm,
                Some(s) => Lane::parse(s).ok_or_else(|| Error::bad_params(format!("unknown lane: {s}")))?,
            };
            m.register(&key, batch, lane).await
        }
        // { key, hash?, text?, wait_ms?=30000, mode?="path" }
        // -> { hash, state:"warm", path, bytes, format, from_cache, synth_ms, age_ms }
        // `not_ready` is an ERROR, not an empty success: a client's fallback
        // depends on telling "no" from "not yet".
        "tts.request" => {
            let key = req_str(&params, "key")?;
            let hash = params.get("hash").and_then(Value::as_str).map(str::to_string);
            let text = params.get("text").and_then(Value::as_str).map(str::to_string);
            let wait_ms = params.get("wait_ms").and_then(Value::as_u64).unwrap_or(30_000);
            let mode = params.get("mode").and_then(Value::as_str).unwrap_or("path");
            m.request(&key, hash, text, wait_ms, mode).await
        }
        "tts.touch" => m.touch(&req_str(&params, "key")?, &req_list(&params, "hashes")?),
        "tts.status" => m.status(params.get("key").and_then(Value::as_str)),
        "tts.keys" => m.keys_payload(),
        // { key, hashes } -> { dequeued, aborted, unknown }. Never touches warm
        // audio; that is `tts.forget`.
        "tts.cancel" => m.cancel(&req_str(&params, "key")?, &req_list(&params, "hashes")?),
        // The only deletion path.
        "tts.forget" => m.forget(&req_str(&params, "key")?, &req_list(&params, "hashes")?),
        "tts.reload" => m.reload(),
        // { key, config, replace?=false } -> { key, created, updated, config }
        "tts.upsert_key" => {
            let key = req_str(&params, "key")?;
            let config = params.get("config").cloned().ok_or_else(|| Error::bad_params("missing required param: config"))?;
            m.upsert_key(&key, &config, params.get("replace").and_then(Value::as_bool).unwrap_or(false))
        }
        // { key, from_dir, module_config?, mode?="hardlink", dry_run?=true }
        // Adopt a cache directory by hash. Never deletes from `from_dir`, and
        // writes nothing unless `dry_run` is false.
        "tts.import" => {
            let key = req_str(&params, "key")?;
            let from_dir = import::expand_home(&req_str(&params, "from_dir")?);
            let mode = match params.get("mode").and_then(Value::as_str) {
                None => ImportMode::Hardlink,
                Some(s) => ImportMode::parse(s).ok_or_else(|| Error::bad_params(format!("unknown import mode: {s}")))?,
            };
            let dry_run = params.get("dry_run").and_then(Value::as_bool).unwrap_or(true);
            import::import(m, &key, &from_dir, params.get("module_config").cloned(), mode, dry_run)
        }
        other => Err(Error::wire("unknown_method", format!("unknown method: {other}"))),
    }
}

fn req_str(params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::bad_params(format!("missing required param: {key}")))
}

fn req_list(params: &Value, key: &str) -> Result<Vec<String>> {
    params
        .get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .ok_or_else(|| Error::bad_params(format!("{key} must be an array of strings")))
}
