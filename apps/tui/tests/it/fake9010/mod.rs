//! **A FAKE AUI, on a real socket — and the ONLY 9010 anything in this suite
//! ever talks to.**
//!
//! # Why this exists at all, and it is not the usual reason
//!
//! The daemon fake next door (`tests/it/fake/mod.rs`) exists because three
//! transport behaviours fail SILENTLY. This one exists for a blunter reason:
//! **every act on the other end of 9010 is destructive and irreversible in the
//! operator's actual life.** `SetThreadClosed` closes a real thread on his real
//! board. `SetThreadSummary` overwrites a real title. `MovePin` renumbers every
//! pin he has. `RemoveProject` unlinks `~/minds/<name>`. There is no read-only
//! way to "just check the shape" of any of them, and a single stray frame at
//! the live listener is not a failed test — it is damage.
//!
//! So the whole send path is proven here, against a socket this file opened
//! itself on an **ephemeral loopback port**, and the assertions are on the
//! **RAW BYTES** the client wrote. Nothing in this file can reach 198.51.100.77
//! and nothing in it knows the number 9010.
//!
//! # Why the bytes and not the types
//!
//! The client is built against `aui_wire`'s own `Mutation` and `ClientMsg`, so
//! a test that asserted on those types would be asserting that serde round-trips
//! — true, and not the question. The question is what the LIVE SERVER would
//! read, and the live server reads `serde_json::from_str::<ClientMsg>(&text)`
//! (`apps/aui/src/v2/ws.rs:112`) followed by `Mutation::from_envelope`
//! (`:159`). So this fake keeps the text frames verbatim, and the tests parse
//! them the way that server parses them.
//!
//! One `Script` per incoming connection, popped in order; the last repeats, so
//! a reconnect ladder does not need a rung apiece.

#![allow(dead_code)] // each test uses a different corner of this harness

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

/// How one connection behaves.
#[derive(Clone, Debug)]
pub struct Script {
    /// Answer `hello` with `server_info`. `false` leaves the client connected
    /// and NOT usable — the state in which a mutation must be refused rather
    /// than queued.
    pub server_info: bool,
    /// Every `mutate` is acked `Ok` unless this says otherwise. `Some(msg)`
    /// answers every mutation with that refusal, which is how the "relay the
    /// AUI's own sentence verbatim" rule is proven.
    pub refuse_with: Option<String>,
    /// Do not answer `mutate` at all. The client must then report NOT PERFORMED
    /// rather than assume, and must never report success.
    pub swallow_mutations: bool,
    /// Raw frames pushed straight after `server_info`, in order.
    pub push: Vec<String>,
    /// Close the socket once this many mutations have arrived.
    pub close_after_mutations: Option<usize>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            server_info: true,
            refuse_with: None,
            swallow_mutations: false,
            push: Vec::new(),
            close_after_mutations: None,
        }
    }
}

impl Script {
    pub fn refuses(msg: &str) -> Self {
        Self {
            refuse_with: Some(msg.into()),
            ..Default::default()
        }
    }
    pub fn silent() -> Self {
        Self {
            server_info: false,
            ..Default::default()
        }
    }
    pub fn swallows() -> Self {
        Self {
            swallow_mutations: true,
            ..Default::default()
        }
    }
    pub fn push(mut self, frame: Value) -> Self {
        self.push.push(frame.to_string());
        self
    }
    pub fn close_after(mut self, n: usize) -> Self {
        self.close_after_mutations = Some(n);
        self
    }
}

/// One frame as the server saw it: the TEXT, before anything parsed it.
#[derive(Clone, Debug)]
pub struct Frame {
    /// 1-based connection number.
    pub conn: usize,
    /// The raw bytes the client wrote.
    pub text: String,
}

impl Frame {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.text).expect("the client wrote something that is not JSON")
    }
    /// The `t` tag: `hello` | `mutate` | …
    pub fn kind(&self) -> String {
        self.json()
            .get("t")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

#[derive(Default)]
struct Shared {
    scripts: Vec<Script>,
    frames: Vec<Frame>,
    conns: usize,
}

pub struct Fake9010 {
    pub url: String,
    shared: Arc<Mutex<Shared>>,
    close_now: Arc<Notify>,
}

impl Fake9010 {
    pub async fn start(scripts: Vec<Script>) -> Fake9010 {
        // **EPHEMERAL LOOPBACK, CHOSEN BY THE OS.** Not 9010, not the tailnet
        // address, and not a port any test could accidentally share with the
        // live listener.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        // REL-6a6: an isolated dial reaches only a port it was given.
        env::egress::grant_addr(listener.local_addr().expect("addr")).expect("grant the fake's port");
        let shared = Arc::new(Mutex::new(Shared {
            scripts,
            ..Default::default()
        }));
        let close_now = Arc::new(Notify::new());

        let s = shared.clone();
        let c = close_now.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let n = {
                    let mut g = s.lock().expect("lock");
                    g.conns += 1;
                    g.conns
                };
                let script = {
                    let g = s.lock().expect("lock");
                    g.scripts
                        .get(n - 1)
                        .or_else(|| g.scripts.last())
                        .cloned()
                        .unwrap_or_default()
                };
                tokio::spawn(serve(sock, n, script, s.clone(), c.clone()));
            }
        });

        Fake9010 {
            url: format!("ws://127.0.0.1:{port}/"),
            shared,
            close_now,
        }
    }

    pub fn close_current(&self) {
        self.close_now.notify_waiters();
    }

    pub fn frames(&self) -> Vec<Frame> {
        self.shared.lock().expect("lock").frames.clone()
    }

    /// **EVERY MUTATION FRAME, IN ORDER** — the assertion surface of this whole
    /// suite.
    pub fn mutations(&self) -> Vec<Value> {
        self.frames()
            .into_iter()
            .filter(|f| f.kind() == "mutate")
            .map(|f| f.json())
            .collect()
    }

    /// The envelope of the n-th mutation (0-based), as `{kind, payload}`.
    pub fn envelope(&self, n: usize) -> Value {
        self.mutations()
            .get(n)
            .and_then(|m| m.get("m").cloned())
            .unwrap_or_else(|| panic!("no mutation #{n}; saw {:#?}", self.mutations()))
    }

    pub fn connections(&self) -> usize {
        self.shared.lock().expect("lock").conns
    }

    /// Wait for a CONDITION, not for a duration. A `sleep` long enough to be
    /// reliable is a `sleep` long enough to hide a bug; this fails loudly with
    /// the state it was waiting on.
    pub async fn until(&self, what: &str, f: impl Fn(&Fake9010) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if f(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!(
            "timed out waiting for {what}\n  connections: {}\n  frames: {:#?}",
            self.connections(),
            self.frames()
        );
    }

    pub async fn until_mutations(&self, n: usize) {
        self.until(&format!("{n} mutation(s)"), |f| f.mutations().len() >= n)
            .await;
    }

    pub async fn until_connections(&self, n: usize) {
        self.until(&format!("{n} connection(s)"), |f| f.connections() >= n)
            .await;
    }
}

async fn serve(
    sock: tokio::net::TcpStream,
    n: usize,
    script: Script,
    shared: Arc<Mutex<Shared>>,
    close_now: Arc<Notify>,
) {
    let Ok(ws) = tokio_tungstenite::accept_async(sock).await else {
        return;
    };
    let (mut tx, mut rx) = ws.split();
    let mut mutations = 0usize;

    loop {
        let incoming = tokio::select! {
            _ = close_now.notified() => return,
            m = rx.next() => m,
        };
        let Some(Ok(Message::Text(text))) = incoming else {
            return;
        };
        // **RECORDED BEFORE IT IS PARSED**, so a frame the server could not
        // read is still evidence rather than a silent drop.
        shared.lock().expect("lock").frames.push(Frame {
            conn: n,
            text: text.clone(),
        });
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        match v.get("t").and_then(Value::as_str) {
            Some("hello") => {
                if !script.server_info {
                    continue;
                }
                // The shape `V2Server::server_info` answers with, trimmed to
                // the fields this client reads. Every other field is defaulted
                // on the wire, which is the tolerance this frame is testing.
                let info = json!({
                    "t": "server_info",
                    "voice": "fake",
                    "version": "0.0.0-fake",
                    "proto_version": 2,
                });
                if tx.send(Message::Text(info.to_string())).await.is_err() {
                    return;
                }
                for frame in &script.push {
                    if tx.send(Message::Text(frame.clone())).await.is_err() {
                        return;
                    }
                }
            }
            Some("mutate") => {
                mutations += 1;
                if script.swallow_mutations {
                    if Some(mutations) == script.close_after_mutations {
                        return;
                    }
                    continue;
                }
                let id = v.get("id").cloned().unwrap_or(Value::Null);
                let result = match script.refuse_with.as_ref() {
                    Some(msg) => json!({"ok": "err", "message": msg}),
                    None => json!({"ok": "ok"}),
                };
                let ack = json!({"t": "mut_ack", "id": id, "result": result});
                if tx.send(Message::Text(ack.to_string())).await.is_err() {
                    return;
                }
                if Some(mutations) == script.close_after_mutations {
                    return;
                }
            }
            _ => {}
        }
    }
}

/// An `Invalidate` frame for one node kind, as `apps/aui/src/v2/mod.rs`
/// broadcasts it after a successful mutation.
pub fn invalidate(kind: &str) -> Value {
    json!({
        "t": "invalidate",
        "node": {"kind": kind, "params": {}},
        "version": 1,
    })
}
