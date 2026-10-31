//! **A FAKE THREADS CHASSIS, ON A REAL SOCKET.**
//!
//! # What this was, and what changed
//!
//! It was a fake of the LEGACY daemon wire — `welcome` / `auth` /
//! `subscribe{threads:null}` / id-correlated RPC — written so that three things
//! that fail SILENTLY could be forced to happen: `boot_id`-discriminated
//! reconnect, `Lagged` recovery and the outbound drain. All three are gone from
//! this face, with the hand-rolled transport that owned them, so the harness
//! that scripted them is gone too and `tests/transport.rs` with it.
//!
//! What is left is the SHAPE of a fake this suite still needs: a real socket,
//! on an ephemeral loopback port it opened itself, that speaks the CHASSIS door
//! well enough for [`tui::seam::link`] to reach `LinkState::Up` and for a
//! command to be answered or deliberately left hanging.
//!
//! **It is emphatically not the real daemon and never talks to one.** The
//! operator's real threads are on 3651 and 3653 and nothing in this file can
//! reach either: the port is whatever the kernel handed this process, and the
//! face under test is pointed at it through `TUI_THREADS_CHASSIS_PORT`.
//!
//! # The chassis protocol, as this answers it
//!
//! 1. on connect → `{"type":"welcome","boot_id","auth_required":false}`
//! 2. the client asks `shapes/list` → answered from
//!    `libs/shapewire/inventory.json`, the SAME committed artifact
//!    `tests/it/seam_reference.rs` routes the seam's surface against, so a fake
//!    that served a surface the producer does not have would be caught by that
//!    test rather than by a reader
//! 3. the client subscribes each standing entry → `shapes/{shape}/subscribe`,
//!    answered `success` with an empty payload. **An empty success is not an
//!    export**: no mirror hydrates, so every `view::*` answer stays
//!    `NotHydrated` and these tests see the honest *I have not been told*
//!    state. A test that needs a hydrated mirror folds frames through
//!    `Held::fold` directly; that is what `tests/it/seam_conformance.rs` does, and
//!    it needs no socket at all.
//! 4. anything else → the `Conn`'s own scripted answer, or a bare success
//!
//! One `Conn` script per incoming connection, popped in order; the last repeats.

#![allow(dead_code)] // each test uses a different corner of this harness

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

/// How one connection behaves.
#[derive(Clone, Debug)]
pub struct Conn {
    pub boot_id: String,
    /// Whether this connection answers anything but `shapes/list`.
    ///
    /// **The name is kept from the legacy harness and so is its purpose.** It
    /// used to withhold the `sub_ack` so a request sat in the outbound channel;
    /// it now withholds every answer but the learn, which leaves a command
    /// PENDING for the whole test. That is the window a double `enter` falls
    /// into, and it is the only thing the one test that uses it needs.
    pub answers: bool,
    /// Per-leaf scripted payloads, e.g. `("send", {"accepted": true})`.
    pub responses: HashMap<String, Value>,
    /// Per-leaf scripted REFUSALS, `(code, message)`; a leaf here is answered
    /// with an `error` outcome instead of a success (TUI-P1f).
    pub refusals: HashMap<String, (String, String)>,
    /// Frames pushed unprompted the moment the client has learned the surface.
    pub push: Vec<String>,
}

impl Default for Conn {
    fn default() -> Self {
        Conn {
            boot_id: "boot-1".into(),
            answers: true,
            responses: HashMap::new(),
            refusals: HashMap::new(),
            push: Vec::new(),
        }
    }
}

impl Conn {
    pub fn boot(id: &str) -> Self {
        Conn {
            boot_id: id.into(),
            ..Default::default()
        }
    }

    /// Answer nothing but `shapes/list`. See [`Conn::answers`].
    pub fn no_suback(mut self) -> Self {
        self.answers = false;
        self
    }

    /// Script one leaf's answer. The leaf, not the path: `"send"`, never
    /// `"threads/{id}/send"`, so a test says what it means and the path parse
    /// stays this file's business.
    pub fn respond(mut self, leaf: &str, payload: Value) -> Self {
        self.responses.insert(leaf.to_string(), payload);
        self
    }

    /// Script one leaf's REFUSAL, by leaf as [`Conn::respond`] does.
    pub fn refuse(mut self, leaf: &str, code: &str, message: &str) -> Self {
        self.refusals
            .insert(leaf.to_string(), (code.to_string(), message.to_string()));
        self
    }

    pub fn push(mut self, frame: Value) -> Self {
        self.push.push(frame.to_string());
        self
    }
}

/// One request this fake saw.
#[derive(Clone, Debug)]
pub struct Req {
    pub conn: usize,
    pub id: String,
    pub method: String,
    pub params: Value,
}

impl Req {
    /// The last path segment — `send` out of `threads/{id}/send`.
    pub fn leaf(&self) -> &str {
        split_path(&self.method).2
    }

    /// The `{subject}` segment, when the method has one.
    pub fn subject(&self) -> Option<&str> {
        split_path(&self.method).1
    }
}

/// **`family[/{subject}]/leaf`, PARSED RIGHT-TO-LEFT**, exactly as
/// `shapewire::server::route` does: the leaf is last, the family is first, and
/// whatever is between them is the subject.
pub fn split_path(method: &str) -> (&str, Option<&str>, &str) {
    let mut segs = method.split('/').collect::<Vec<_>>();
    let leaf = segs.pop().unwrap_or("");
    let family = if segs.is_empty() { "" } else { segs.remove(0) };
    let subject = if segs.is_empty() {
        None
    } else {
        Some(segs[0])
    };
    (family, subject, leaf)
}

#[derive(Default)]
struct Shared {
    scripts: Vec<Conn>,
    conns: usize,
    log: Vec<Req>,
    /// Connections that have asked `shapes/list` and been answered.
    learned: usize,
}

pub struct Fake {
    pub url: String,
    /// The chassis PORT, which is how a face is pointed at this rather than at
    /// anything real. `tests` set `TUI_THREADS_CHASSIS_PORT` to it.
    pub port: u16,
    shared: Arc<Mutex<Shared>>,
    close_now: Arc<Notify>,
}

impl Fake {
    pub async fn start(scripts: Vec<Conn>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().unwrap().port();
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
                    let mut g = s.lock().unwrap();
                    g.conns += 1;
                    g.conns
                };
                let script = {
                    let g = s.lock().unwrap();
                    g.scripts
                        .get(n - 1)
                        .or_else(|| g.scripts.last())
                        .cloned()
                        .unwrap_or_default()
                };
                tokio::spawn(serve(sock, n, script, s.clone(), c.clone()));
            }
        });

        Fake {
            url: format!("ws://127.0.0.1:{port}/ws"),
            port,
            shared,
            close_now,
        }
    }

    pub fn close_current(&self) {
        self.close_now.notify_waiters();
    }

    pub fn log(&self) -> Vec<Req> {
        self.shared.lock().unwrap().log.clone()
    }

    pub fn connections(&self) -> usize {
        self.shared.lock().unwrap().conns
    }

    pub async fn until(&self, what: &str, f: impl Fn(&Fake) -> bool) {
        let t0 = Instant::now();
        while !f(self) {
            assert!(
                t0.elapsed() < Duration::from_secs(5),
                "timed out waiting for {what}; the fake saw {:#?}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// **`n` CONNECTIONS HAVE LEARNED THE SURFACE.** The chassis equivalent of
    /// the legacy `sub_ack`: a face that has been answered `shapes/list` has a
    /// book it can subscribe from and a link that is UP.
    pub async fn until_subscribed(&self, n: usize) {
        self.until("the surface to be learned", |f| {
            f.shared.lock().unwrap().learned >= n
        })
        .await;
    }

    pub async fn until_connections(&self, n: usize) {
        self.until("connections", |f| f.connections() >= n).await;
    }
}

/// **THE SURFACE, FROM THE COMMITTED INVENTORY.** Not hand-written: a fake that
/// served shapes the producer does not have would make every test over it a
/// belief about the wire rather than a record of one, and this is the same file
/// `tests/it/seam_reference.rs` routes the seam's own surface against.
fn shapes_list() -> Value {
    let inv: Value =
        serde_json::from_str(include_str!("../../../../../libs/shapewire/seam/inventory.json"))
            .expect("the vendored inventory parses");
    let shapes: Vec<Value> = inv["shapes"]
        .as_array()
        .expect("the inventory lists shapes")
        .iter()
        .map(|s| {
            let mut v = json!({
                "name": s["name"],
                "keys": [],
                "tracking": s["tracking"],
                "tracked": false,
                "subscribers": 0,
                "epoch": {"reinit": 0, "lost": 0, "state": "not_hydrated"},
                "fifo_high_water": 1,
                "bursts": 0,
                "consumers": [],
                "max": s["max"],
                "contract": {"version": s["version"], "serves": s["serves"]},
                // **`cells` AND `shared_cells` ARE REQUIRED, NOT DEFAULTED.**
                // `wire::ShapeInfo` deliberately gives neither a
                // `#[serde(default)]`: an empty list would read as "this
                // producer carries nothing", which is a lie a face would then
                // serve as `Fact::Undeclared` on every cell. They come from the
                // same committed inventory as everything else here, so this
                // fixture cannot disagree with the producer about its manifest.
                "cells": s["cells"],
                "shared_cells": s["shared_cells"],
            });
            if let Some(p) = s["parent"].as_str() {
                v["parent"] = json!(p);
            }
            v
        })
        .collect();
    json!({ "shapes": shapes })
}

async fn serve(
    sock: tokio::net::TcpStream,
    n: usize,
    script: Conn,
    shared: Arc<Mutex<Shared>>,
    close_now: Arc<Notify>,
) {
    let Ok(ws) = tokio_tungstenite::accept_async(sock).await else {
        return;
    };
    let (mut sink, mut stream) = ws.split();
    // **THE WELCOME FRAME IS PARSED AS A TYPE NOW, AND A MISS DROPS THE
    // SOCKET.** `seam::link` used to read `boot_id` off an untyped value and
    // ignore the rest; it now deserialises `envelope::ChassisServerMsg` and, on
    // failure, publishes `LinkState::Down { "the daemon's welcome frame did not
    // parse" }` and breaks the session — so a fake missing a required field
    // never reaches `shapes/list` at all. `proto` and `daemon` are the two this
    // fake was not sending, and their absence masked every other finding.
    let welcome = json!({
        "type": "welcome",
        "proto": 1,
        "daemon": "threads",
        "boot_id": script.boot_id,
        "auth_required": false,
        "daemon_version": "fake",
    });
    if sink.send(Message::Text(welcome.to_string())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            _ = close_now.notified() => return,
            frame = stream.next() => {
                let Some(Ok(Message::Text(t))) = frame else { return };
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                if v.get("type").and_then(Value::as_str) != Some("request") {
                    continue;
                }
                let id = v["id"].as_str().unwrap_or_default().to_string();
                let method = v["method"].as_str().unwrap_or_default().to_string();
                let params = v["params"].clone();
                let (_, subject, leaf) = split_path(&method);
                // **THE REAL DAEMON'S REFUSAL, REPRODUCED.** A subject spelled
                // in the path AND in the params is refused — the same refusal
                // whether the two agree or disagree, because a precedence rule
                // would be a place for them to disagree. A fake that quietly
                // accepted both would hide exactly the regression the
                // path-migration was about.
                let doubled = subject.is_some()
                    && params.get("thread_id").and_then(Value::as_str).is_some();
                {
                    let mut g = shared.lock().unwrap();
                    g.log.push(Req {
                        conn: n,
                        id: id.clone(),
                        method: method.clone(),
                        params: params.clone(),
                    });
                }
                let outcome = if doubled {
                    Some(json!({
                        "type": "error",
                        "code": "bad_request",
                        "error": "`thread_id` is the SUBJECT of this method and rides the \
                                  path, not the params",
                    }))
                } else if method == "shapes/list" {
                    Some(json!({"type": "success", "payload": shapes_list()}))
                } else if !script.answers {
                    // Everything but the learn is left hanging, deliberately.
                    None
                } else if let Some((code, message)) = script.refusals.get(leaf) {
                    Some(json!({"type": "error", "code": code, "error": message}))
                } else if leaf == "subscribe" || leaf == "unsubscribe" {
                    Some(json!({"type": "success", "payload": {}}))
                } else {
                    Some(json!({
                        "type": "success",
                        "payload": script
                            .responses
                            .get(leaf)
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                    }))
                };
                let Some(outcome) = outcome else { continue };
                let reply = json!({"type": "response", "correlate": id, "outcome": outcome});
                if sink.send(Message::Text(reply.to_string())).await.is_err() {
                    return;
                }
                if method == "shapes/list" {
                    shared.lock().unwrap().learned += 1;
                    for f in &script.push {
                        if sink.send(Message::Text(f.clone())).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}
