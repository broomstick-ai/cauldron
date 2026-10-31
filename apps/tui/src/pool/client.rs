//! The board's one connection to the threads daemon: the chassis WebSocket
//! (`ws://host:port/ws`), speaking the shapewire envelope.
//!
//! It subscribes to two shapes — `accounts` (keyless) and `threads/non-idle`
//! (key `fleet`) — plus, on the board's ask, single keys of `threads` and
//! `threads/summary` (`Out::Hold`) — and forwards every `type: shape` frame to the board. Acts
//! go out as `type: request` frames and their outcome comes back on a
//! oneshot. It reads no files; the only thing it sends unprompted is the
//! subscribes. It reconnects with backoff (1/2/4/8/15/30 s, `api.py`).

use std::collections::HashMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use super::model::{ACCOUNTS, FLEET, NON_IDLE};

pub const CONSUMER: &str = "tui-pool";
pub const SHAPE_VERSION: &str = "shapes.md@rev4";

#[derive(Debug)]
pub enum In {
    Live,
    Offline(String),
    Frame(Value),
    /// A subscribe the daemon refused — drawn as a note, never swallowed.
    Refused(String),
}

pub enum Out {
    Request { method: String, params: Value, reply: oneshot::Sender<Result<Value, String>> },
    /// Ask for a fresh `accounts` export (after a write, as the Python board
    /// resyncs). The new state arrives on the shape, not on the write's reply.
    Resubscribe,
    /// Subscribe one KEY of a keyed shape (`threads`, `threads/summary`). Sent
    /// once per session; the board re-asks after a reconnect (`In::Live`).
    Hold { shape: &'static str, key: String },
}

#[derive(Clone)]
pub struct Handle {
    tx: mpsc::Sender<Out>,
}

impl Handle {
    /// Send one request; the outcome's `payload` on success, the daemon's
    /// error text on refusal.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Out::Request { method: method.to_string(), params, reply })
            .await
            .map_err(|_| "the connection task is gone".to_string())?;
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("the connection dropped before the daemon answered".into()),
            Err(_) => Err("the daemon did not answer within 30s".into()),
        }
    }

    pub async fn resubscribe(&self) {
        let _ = self.tx.send(Out::Resubscribe).await;
    }

    pub async fn hold(&self, shape: &'static str, key: &str) {
        let _ = self.tx.send(Out::Hold { shape, key: key.to_string() }).await;
    }
}

/// The secret, if one is set: `TUI_THREADS_SECRET`, then `THREADS_SECRET`
/// (the chat face's seam reads the same two).
fn secret() -> Option<String> {
    ["TUI_THREADS_SECRET", "THREADS_SECRET"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.trim().is_empty())
}

/// Start the connection task. Frames and link changes arrive on `events`.
///
/// An isolated process (`cauldron::live::isolation`: a test, a harness, a
/// drive, a dump) is REFUSED a connection to one of his live ports, loudly
/// (REL-6a2): the board's url names the operator's daemon unless pointed away.
pub fn spawn(url: String, events: mpsc::Sender<In>) -> Handle {
    if let Some(refusal) = crate::cauldron::live::refuse_isolated_url("pool", &url) {
        panic!("{refusal}");
    }
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(run(url, events, rx));
    Handle { tx }
}

/// The port a `ws://host:port/…` url names, if it names one.
pub fn url_port(url: &str) -> Option<u16> {
    crate::cauldron::live::url_port(url)
}

fn subscribe_params(key: Option<&str>) -> Value {
    let mut p = json!({"consumer": {"name": CONSUMER, "shape_version": SHAPE_VERSION, "pid": std::process::id()}});
    if let Some(k) = key {
        p["key"] = json!(k);
    }
    p
}

fn subscribe(
    n: &mut u64,
    subs: &mut HashMap<String, &'static str>,
    shape: &'static str,
    key: Option<&str>,
) -> Value {
    *n += 1;
    let id = format!("p{n}");
    subs.insert(id.clone(), shape);
    json!({"type": "request", "id": id, "method": format!("shapes/{shape}/subscribe"), "params": subscribe_params(key)})
}

async fn run(url: String, events: mpsc::Sender<In>, mut out: mpsc::Receiver<Out>) {
    const BACKOFF: [u64; 6] = [1, 2, 4, 8, 15, 30];
    let mut attempt = 0usize;
    loop {
        match session(&url, &events, &mut out).await {
            Ok(()) => return, // the board hung up
            Err(why) => {
                if events.send(In::Offline(why)).await.is_err() {
                    return;
                }
            }
        }
        let wait = BACKOFF[attempt.min(BACKOFF.len() - 1)];
        attempt += 1;
        // Requests made while offline are answered at once, not queued.
        let deadline = tokio::time::sleep(Duration::from_secs(wait));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                m = out.recv() => match m {
                    None => return,
                    Some(Out::Request { reply, .. }) => { let _ = reply.send(Err("offline — not sent".into())); }
                    Some(Out::Resubscribe) | Some(Out::Hold { .. }) => {}
                }
            }
        }
    }
}

/// One connection's life. `Ok` only when the board side closed.
async fn session(
    url: &str,
    events: &mpsc::Sender<In>,
    out: &mut mpsc::Receiver<Out>,
) -> Result<(), String> {
    let mut ws = crate::cauldron::live::dial_ws("pool", url).await.map_err(|e| format!("connect: {e}"))?;
    let mut n = 0u64;
    let mut pending: HashMap<String, oneshot::Sender<Result<Value, String>>> = HashMap::new();
    let mut subs: HashMap<String, &'static str> = HashMap::new();

    macro_rules! send {
        ($v:expr) => {
            ws.send(Message::Text($v.to_string().into())).await.map_err(|e| format!("send: {e}"))?
        };
    }

    // welcome, then auth if asked for
    let welcome = loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
            Err(_) => return Err("no welcome within 10s".into()),
            Ok(None) => return Err("closed before welcome".into()),
            Ok(Some(Err(e))) => return Err(format!("read: {e}")),
            Ok(Some(Ok(Message::Text(t)))) => break serde_json::from_str::<Value>(&t).unwrap_or(Value::Null),
            Ok(Some(Ok(_))) => continue,
        }
    };
    if welcome["auth_required"] == true {
        let Some(s) = secret() else { return Err("the daemon requires a secret and none is set".into()) };
        send!(json!({"type": "auth", "secret": s}));
    }

    let a = subscribe(&mut n, &mut subs, ACCOUNTS, None);
    let b = subscribe(&mut n, &mut subs, NON_IDLE, Some(FLEET));
    send!(a);
    send!(b);
    let _ = events.send(In::Live).await;

    loop {
        tokio::select! {
            m = out.recv() => match m {
                None => return Ok(()),
                Some(Out::Request { method, params, reply }) => {
                    n += 1;
                    let id = format!("p{n}");
                    pending.insert(id.clone(), reply);
                    send!(json!({"type": "request", "id": id, "method": method, "params": params}));
                }
                Some(Out::Resubscribe) => {
                    let a = subscribe(&mut n, &mut subs, ACCOUNTS, None);
                    send!(a);
                }
                Some(Out::Hold { shape, key }) => {
                    let a = subscribe(&mut n, &mut subs, shape, Some(&key));
                    send!(a);
                }
            },
            f = ws.next() => {
                let t = match f {
                    None => return Err("the daemon closed the connection".into()),
                    Some(Err(e)) => return Err(format!("read: {e}")),
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Ping(p))) => { let _ = ws.send(Message::Pong(p)).await; continue; }
                    Some(Ok(Message::Close(_))) => return Err("the daemon closed the connection".into()),
                    Some(Ok(_)) => continue,
                };
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                match v["type"].as_str() {
                    Some("shape") => { if events.send(In::Frame(v)).await.is_err() { return Ok(()); } }
                    Some("response") => {
                        let id = v["correlate"].as_str().unwrap_or("").to_string();
                        let o = &v["outcome"];
                        let r = if o["type"] == "error" {
                            Err(o["error"].as_str().unwrap_or("refused").to_string())
                        } else {
                            Ok(o["payload"].clone())
                        };
                        if let Some(reply) = pending.remove(&id) {
                            let _ = reply.send(r);
                        } else if let (Some(shape), Err(e)) = (subs.remove(&id), r) {
                            let _ = events.send(In::Refused(format!("subscribe {shape}: {e}"))).await;
                        }
                    }
                    Some("auth_ack") if v["ok"] != true => return Err("the daemon refused the secret".into()),
                    _ => {}
                }
            }
        }
    }
}
