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
use tokio::sync::watch;
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
    /// REL-6: on a `threads/{id}/send`, push the daemon's own record of it —
    /// a `threads/log` transition carrying a `user_message` with the sent
    /// text — after the ack, as the real daemon logs an accepted message.
    pub echo_sends: bool,
    /// REL-6: leaves this connection never answers (a body the daemon is
    /// slow to serve), while everything else is answered.
    pub hang: Vec<String>,
    /// V1.6: frames pushed right after this connection answers a leaf —
    /// `("disable", <the row with disabled: true>)` plays a daemon that
    /// publishes the fact its command caused. `(leaf, frame)`, in order.
    pub after: Vec<(String, String)>,
}

impl Default for Conn {
    fn default() -> Self {
        Conn {
            boot_id: "boot-1".into(),
            answers: true,
            responses: HashMap::new(),
            refusals: HashMap::new(),
            push: Vec::new(),
            echo_sends: false,
            hang: Vec::new(),
            after: Vec::new(),
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

    /// Echo every accepted send as a `threads/log` event. See [`Conn::echo_sends`].
    pub fn echo_sends(mut self) -> Self {
        self.echo_sends = true;
        self
    }

    /// After answering `leaf`, push `frame`. See [`Conn::after`].
    pub fn then(mut self, leaf: &str, frame: Value) -> Self {
        self.after.push((leaf.to_string(), frame.to_string()));
        self
    }

    /// Never answer `leaf`. See [`Conn::hang`].
    pub fn hang(mut self, leaf: &str) -> Self {
        self.hang.push(leaf.to_string());
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
    /// The next seq an echoed send is logged at.
    echo_seq: u64,
    scripts: Vec<Conn>,
    conns: usize,
    log: Vec<Req>,
    /// Connections that have asked `shapes/list` and been answered.
    learned: usize,
    /// Connections the FACE ended (its socket closed under the fake's read) —
    /// never the fake's own `stop`/`close_current` (REL-6e F1).
    peer_closed: usize,
    /// REL-6f G2: connections a [`Gate`] let through, and connections it
    /// dropped because it did not open in time.
    gate_opened: usize,
    gate_timed_out: usize,
}

/// **REL-6f G2: AN EVENT ANOTHER FAKE WAITS ON**, not a clock it compares.
/// `(accepted, closed by the face)` for one fake, published on every change.
type Closes = Arc<watch::Sender<(usize, usize)>>;

/// **A GATE ON A FAKE'S CONNECTIONS** ([`Fake::start_gated`]): each incoming
/// connection is held, before its handshake, until EVERY connection the
/// watched fake accepted has been closed by the face. It waits on that event
/// itself. `most` is a failure bound, never a measurement: a connection the
/// event has not freed by then is counted (`gate_timed_out`) and dropped.
#[derive(Clone)]
pub struct Gate {
    closes: watch::Receiver<(usize, usize)>,
    most: Duration,
}

pub struct Fake {
    pub url: String,
    /// The chassis PORT, which is how a face is pointed at this rather than at
    /// anything real. `tests` set `TUI_THREADS_CHASSIS_PORT` to it.
    pub port: u16,
    shared: Arc<Mutex<Shared>>,
    closes: Closes,
    /// REL-6f: a close GENERATION, not a `Notify`: `notify_waiters` is lost
    /// on a connection task that is not parked in `notified()` at that instant
    /// (mid-send), so a `stop` could leave the socket open. A watch change is
    /// seen whenever the task next looks.
    close_now: Arc<watch::Sender<u64>>,
    accept: tokio::task::JoinHandle<()>,
}

impl Fake {
    pub async fn start(scripts: Vec<Conn>) -> Fake {
        Self::start_on(0, scripts).await
    }

    /// The same, on `port` (0: the kernel's choice) — REL-6's restart of a
    /// daemon on the port a face is already dialling.
    pub async fn start_on(port: u16, scripts: Vec<Conn>) -> Fake {
        Self::start_with(port, scripts, None).await
    }

    /// The same, with every incoming connection held at `gate` (REL-6f G2).
    pub async fn start_gated(scripts: Vec<Conn>, gate: Gate) -> Fake {
        Self::start_with(0, scripts, Some(gate)).await
    }

    /// **A GATE THAT OPENS WHEN THE FACE HAS CLOSED EVERY CONNECTION TO THIS
    /// FAKE** (and it accepted at least one), for another fake to hold its
    /// connections at. See [`Gate`].
    pub fn all_closed_gate(&self, most: Duration) -> Gate {
        Gate { closes: self.closes.subscribe(), most }
    }

    async fn start_with(port: u16, scripts: Vec<Conn>, gate: Option<Gate>) -> Fake {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.expect("bind");
        let port = listener.local_addr().unwrap().port();
        // REL-6a6: an isolated dial reaches only a port it was given.
        env::egress::grant_addr(listener.local_addr().unwrap()).expect("grant the fake's port");
        let shared = Arc::new(Mutex::new(Shared {
            scripts,
            ..Default::default()
        }));
        let close_now = Arc::new(watch::channel(0u64).0);
        let closes: Closes = Arc::new(watch::channel((0, 0)).0);

        let s = shared.clone();
        let c = close_now.clone();
        let w = closes.clone();
        let accept = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let n = {
                    let mut g = s.lock().unwrap();
                    g.conns += 1;
                    g.conns
                };
                w.send_modify(|(accepted, _)| *accepted += 1);
                let script = {
                    let g = s.lock().unwrap();
                    g.scripts
                        .get(n - 1)
                        .or_else(|| g.scripts.last())
                        .cloned()
                        .unwrap_or_default()
                };
                tokio::spawn(serve(sock, n, script, s.clone(), c.subscribe(), w.clone(), gate.clone()));
            }
        });

        Fake {
            url: format!("ws://127.0.0.1:{port}/ws"),
            port,
            shared,
            closes,
            close_now,
            accept,
        }
    }

    /// **THE DAEMON GOES DOWN**: no new connection is accepted (the listener
    /// is dropped, so the port is free to be bound again) and the current
    /// one is closed.
    pub fn stop(&self) {
        self.accept.abort();
        self.close_now.send_modify(|g| *g += 1);
    }

    pub fn close_current(&self) {
        self.close_now.send_modify(|g| *g += 1);
    }

    pub fn log(&self) -> Vec<Req> {
        self.shared.lock().unwrap().log.clone()
    }

    pub fn connections(&self) -> usize {
        self.shared.lock().unwrap().conns
    }

    /// How many connections the face closed (REL-6e F1).
    pub fn peer_closed(&self) -> usize {
        self.shared.lock().unwrap().peer_closed
    }

    /// Connections its [`Gate`] let through (REL-6f G2).
    pub fn gate_opened(&self) -> usize {
        self.shared.lock().unwrap().gate_opened
    }

    /// Connections its [`Gate`] dropped because the event it waits on did not
    /// happen within its bound (REL-6f G2).
    pub fn gate_timed_out(&self) -> usize {
        self.shared.lock().unwrap().gate_timed_out
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
    mut close_now: watch::Receiver<u64>,
    closes: Closes,
    gate: Option<Gate>,
) {
    if let Some(Gate { mut closes, most }) = gate {
        let freed = tokio::time::timeout(most, closes.wait_for(|&(accepted, closed)| accepted >= 1 && closed >= accepted)).await;
        let mut g = shared.lock().unwrap();
        if matches!(freed, Ok(Ok(_))) {
            g.gate_opened += 1;
        } else {
            g.gate_timed_out += 1;
            return;
        }
    }
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
            _ = close_now.changed() => return,
            frame = stream.next() => {
                let Some(Ok(Message::Text(t))) = frame else {
                    shared.lock().unwrap().peer_closed += 1;
                    closes.send_modify(|(_, closed)| *closed += 1);
                    return;
                };
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
                } else if script.hang.iter().any(|h| h == leaf) {
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
                if script.echo_sends && leaf == "send" && !doubled {
                    let echo = echo_of(subject.unwrap_or_default(), &params, &shared);
                    if sink.send(Message::Text(echo.to_string())).await.is_err() {
                        return;
                    }
                }
                for (l, f) in &script.after {
                    if l == leaf && sink.send(Message::Text(f.clone())).await.is_err() {
                        return;
                    }
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

/// **THE DAEMON'S RECORD OF A SEND** — one `threads/log` live transition, a
/// `user_message` carrying the sent text, at the next seq from 1000.
fn echo_of(thread: &str, params: &Value, shared: &Arc<Mutex<Shared>>) -> Value {
    let seq = {
        let mut g = shared.lock().unwrap();
        g.echo_seq = g.echo_seq.max(1000) + 1;
        g.echo_seq
    };
    let text = params.get("text").and_then(Value::as_str).unwrap_or_default();
    json!({
        "type": "shape", "shape": "threads/log", "key": thread, "op": "live",
        "at": "2026-10-30T12:00:00Z",
        "epoch": {"reinit": 0, "lost": 0, "state": "known"},
        "event": {"kind": "transition", "cell": "events", "item": {
            "seq": seq, "kind": "user_message", "ts": "2026-10-30T12:00:00Z",
            "payload": {"here": {"kind": "user_message", "text": text}},
        }},
    })
}
