//! **A STANDALONE FAKE THREADS CHASSIS, so the composer can be TYPED IN.**
//!
//! # Why this exists
//!
//! The one thing that could not be felt by hand was the SEND. `threads/{id}/send`
//! puts real words in his real threads, so pressing `enter` against the live
//! daemon is forbidden — which left the most important key in the face as the
//! one nobody had actually pressed. This is the answer: a daemon-shaped thing
//! on loopback that speaks enough of the protocol for the face to connect, and
//! that prints every send it is asked for so the wire is visible while a human
//! drives.
//!
//! **It is not the real daemon and cannot reach one.** It binds an ephemeral
//! port (or one given on the command line) and answers out of memory. The
//! operator's threads are on 3651/3653 and nothing here knows that address.
//!
//! # RE-TARGETED (R7.S3, HOLDS R22) — it spoke the wrong wire entirely
//!
//! **Measured, not suspected.** This file used to speak the LEGACY wire:
//! `welcome` with no `proto`/`daemon`, a keyless `{"type":"subscribe",
//! "threads":null}`, and id-correlated RPC with no negotiation — and its own
//! usage banner told a human to set `$TUI_THREADS_URL`. Both are gone from
//! this face. `main.rs` refuses to start with that variable set at all
//! (`"tui: --server is gone, and so is $TUI_THREADS_URL."`), and `seam::link`
//! parses the welcome frame as a TYPED struct
//! (`shapewire::envelope::ChassisServerMsg::Welcome`) — a frame missing
//! `proto` or `daemon` fails to parse, and the connection is dropped before
//! anything resembling `shapes/list` is ever asked for. The old fake would
//! have looked, to a human running it, exactly like a face that could not
//! connect — because against the real `seam::link` it could not.
//!
//! The real door is the CHASSIS (`seam::link::DEFAULT_CHASSIS_PORT`, env
//! `TUI_THREADS_CHASSIS_PORT`), and it negotiates before anything else:
//!
//!   1. `welcome`, with every field `seam::link` requires.
//!   2. the client asks `shapes/list` — answered from the real, committed
//!      `libs/shapewire/seam/inventory.json`, the same file
//!      `tests/seam_reference.rs` routes the seam's own surface against, so
//!      this fake cannot claim a shape the producer does not have.
//!   3. the client subscribes each standing entry, `shapes/{shape}/subscribe`
//!      — answered `success` with an EMPTY payload.
//!
//! **What step 3 honestly does not do: export.** An empty success is not an
//! export — no mirror hydrates, and every `view::*` answer in the face stays
//! `NotHydrated`. Building a real export means driving `shapewire::mirror`'s
//! own wire encoding, which this file does not attempt: a hand-rolled second
//! copy of that encoder, for one example binary, is exactly the kind of
//! drifting duplicate the chassis migration deleted one of (see
//! `src/transport.rs`'s own header). So a face pointed at this binary reaches
//! `LinkState::Up` and shows an honestly empty, `NotHydrated` fleet rather
//! than a face that cannot connect at all — strictly more than the pre-chassis
//! version of this file could offer once `seam::link` landed, and nothing
//! invented in its place. **Revisit** if a worker wants a hydrated fleet by
//! hand: `tests/it/fake/mod.rs` (this file's test-only sibling, `#[cfg(test)]`,
//! unreachable from an example) is the harder-edged scripted version of the
//! same protocol and is the reference to extend, with `Conn::push` driving
//! real export frames through the committed inventory.
//!
//! What a human DOES get, which is the one thing this file was for: every
//! `threads/{thread_id}/send` this process receives is acknowledged the way
//! [`crate::acts`]'s [`crate::seam::commands::Ack`] shape expects
//! (`promise_id`, `visible: false` — "accepted, applied, no frame will ever
//! carry this id", so this fake never has to promise an export it cannot
//! produce — plus the `message_id`/`header_warnings` addresses a real `send`
//! mints) and printed in full to this process's own stdout, which is still
//! the wire made visible by eye that the rest of this header is about.
//!
//! Every other verb (`cancel`, `update`, `seen`, `event`, `create`) gets the
//! same ack shape with `visible: false`, which is enough for the face to read
//! "accepted" without this binary pretending a fact will ever arrive for it.
//!
//! # Running it
//!
//! ```text
//!   cargo run -p tui --example fake_daemon -- 8799
//!   TUI_THREADS_CHASSIS_PORT=8799 cargo run -p tui
//!   cargo run -p tui --example fake_daemon -- 0 --demo     # two made-up threads
//! ```
//!
//! # `--demo` (REL-6): two made-up threads, exported
//!
//! With `--demo` the fake pushes real export frames the moment a face has
//! learned the surface — the producer's own frame shapes, as
//! `src/mirror/fixtures.rs` records them: `threads/recent-updates` naming two
//! threads, their `threads` rows (named, his app's, idle), and one thread's
//! `threads/log`. So the cauldron face's Threads tab can be proved headless
//! against rows that are visibly not his (`tui --cauldron --dump --tab
//! threads`). The threads are invented here and nowhere else; REL-3's demo
//! dataset is where the teaser's will come from (release question Q-T2).
//!
//! (`TUI_THREADS_PORT`, the legacy `/health` door, is unrelated and is left
//! alone — setting only `TUI_THREADS_CHASSIS_PORT` is not refused; see
//! `mirror::check_ports`.)

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let demo = std::env::args().any(|a| a == "--demo");
    let listener = TcpListener::bind(("127.0.0.1", port)).await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    println!("fake threads chassis on ws://127.0.0.1:{port}/ws");
    println!("  TUI_THREADS_CHASSIS_PORT={port} cargo run -p tui");
    println!();

    // One promise id per command this process acknowledges. Never offered on
    // a frame — see the header: every ack answers `visible: false`.
    let seq = Arc::new(AtomicU64::new(100));
    while let Ok((sock, _)) = listener.accept().await {
        let seq = seq.clone();
        tokio::spawn(async move {
            let Ok(ws) = tokio_tungstenite::accept_async(sock).await else {
                return;
            };
            let (mut tx, mut rx) = ws.split();
            // **EVERY FIELD `seam::link` REQUIRES.** A welcome missing `proto`
            // or `daemon` fails to parse as `ChassisServerMsg::Welcome` and the
            // connection is dropped before `shapes/list` is ever asked for —
            // measured against the pre-rewrite version of this file, which
            // sent neither.
            let welcome = json!({
                "type": "welcome", "proto": 1, "daemon": "threads",
                "daemon_version": "fake", "boot_id": "boot-fake",
                "auth_required": false,
            });
            if tx.send(Message::Text(welcome.to_string())).await.is_err() {
                return;
            }
            println!("· a face connected");
            while let Some(Ok(Message::Text(text))) = rx.next().await {
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if v.get("type").and_then(Value::as_str) != Some("request") {
                    continue;
                }
                let id = v.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                let method = v.get("method").and_then(Value::as_str).unwrap_or("").to_string();
                let params = v.get("params").cloned().unwrap_or(Value::Null);
                let leaf = leaf(&method);

                if leaf == "send" {
                    println!("\n── {method} ──────────────────────────────");
                    println!("{}", serde_json::to_string_pretty(&params).unwrap());
                    println!("──────────────────────────────────────────────\n");
                } else {
                    println!("· {method}");
                }

                let payload = answer(&method, leaf, &seq);
                let resp = json!({
                    "type": "response", "correlate": id,
                    "outcome": {"type": "success", "payload": payload},
                });
                if tx.send(Message::Text(resp.to_string())).await.is_err() {
                    return;
                }
                if demo && method == "shapes/list" {
                    for f in demo_frames() {
                        if tx.send(Message::Text(f.to_string())).await.is_err() {
                            return;
                        }
                    }
                }
            }
            println!("· the face left");
        });
    }
}

/// The LEAF of a method path — `threads/t1/send` → `send`, `shapes/list` →
/// `list`. Matches `shapewire::server::route`'s own right-to-left parse.
fn leaf(method: &str) -> &str {
    method.rsplit_once('/').map_or(method, |(_, l)| l)
}

/// **THE REAL SURFACE, FROM THE COMMITTED INVENTORY — not hand-written.** A
/// fake that served shapes the producer does not have would make every face
/// that connects to it believe something the real daemon cannot back up, and
/// this is the same file `tests/seam_reference.rs` routes the seam's own
/// surface against.
fn shapes_list() -> Value {
    let inv: Value = serde_json::from_str(include_str!(
        "../../../libs/shapewire/seam/inventory.json"
    ))
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
                // `cells`/`shared_cells` are REQUIRED on `wire::ShapeInfo`
                // (no `#[serde(default)]`) — an empty list would read as "this
                // producer carries nothing", a lie this fixture must not tell
                // since it is lifted from the same committed manifest.
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

/// What this fake answers a request with, by leaf.
fn answer(method: &str, leaf: &str, seq: &AtomicU64) -> Value {
    match leaf {
        "list" if method == "shapes/list" => shapes_list(),
        // No export follows — see the module header's "what step 3 honestly
        // does not do". The mirror stays `NotHydrated`, truthfully.
        "subscribe" | "unsubscribe" => json!({}),
        // **THE ACK SHAPE `seam::commands::Ack::of` ACTUALLY READS.**
        // `visible: false` means "accepted, applied, and no frame will ever
        // carry this id" — the honest answer, since this fixture issues no
        // export. `promise_id` is still minted so a face that logs it has
        // something to show.
        "send" => {
            let id = seq.fetch_add(1, Ordering::SeqCst);
            json!({
                "accepted": true, "promise_id": id, "visible": false,
                "message_id": id, "header_warnings": [],
            })
        }
        "create" => {
            let id = seq.fetch_add(1, Ordering::SeqCst);
            json!({"promise_id": id, "visible": false, "thread_id": format!("fake-{id}")})
        }
        "cancel" | "update" | "seen" | "event" => {
            let id = seq.fetch_add(1, Ordering::SeqCst);
            json!({"promise_id": id, "visible": false})
        }
        _ => json!({}),
    }
}

/// **`--demo`'s frames** — two invented threads, exported in the producer's
/// frame shapes (`src/mirror/fixtures.rs`), with the wire's `type: shape`.
fn demo_frames() -> Vec<Value> {
    const AT: &str = "2026-10-30T11:00:00Z";
    let known = || json!({"state": "known", "complete": true, "folded_at": AT});
    let export = |shape: &str, key: &str, row: Value, cells: Value| {
        json!({"type": "shape", "shape": shape, "key": key, "op": "read", "as_of": AT,
            "epoch": {"init_at": AT, "reinit": 0, "lost": 0, "state": "known"}, "cells": cells, "row": row})
    };
    let thread = |id: &str, name: &str| {
        let cells: serde_json::Map<String, Value> = ["header", "settings", "account", "status", "status_transitions", "queued", "last_message_id", "seen_seq", "unread", "newest", "liveness", "app"]
            .iter()
            .map(|c| (c.to_string(), known()))
            .collect();
        export("threads", id, json!({
            "thread_id": id,
            "header": {"cwd": "/Users/demo/projects/potion", "model": "sonnet", "created_at": AT, "updated_at": AT},
            "settings": {"app_id": "tui"}, "account": null,
            "status": {"state": "idle", "executing": false, "detail": null}, "status_transitions": [],
            "queued": 0, "last_message_id": null, "seen_seq": 0, "unread": 0,
            "newest": {"seq": 2, "message_id": null, "kind": "assistant_text", "ts": AT},
            "body": null, "compaction": null,
            "liveness": {"busy": false, "parent_running": false, "running_subagents": []},
            "app": {"summary_updated": {"summary": name, "by": "tui", "ts": AT}},
        }), Value::Object(cells))
    };
    let item = |seq: u64, kind: &str, text: &str| json!({"seq": seq, "kind": kind, "ts": AT, "payload": {"here": {"kind": kind, "text": text}}});
    vec![
        export("threads/recent-updates", "fleet", json!({"pool": "fleet", "threads": [
            {"id": "demo-thread-1", "at": AT, "unread": 0, "seen_seq": 0},
            {"id": "demo-thread-2", "at": AT, "unread": 0, "seen_seq": 0}]}), json!({"threads": known()})),
        thread("demo-thread-1", "Demo: brew the first potion"),
        thread("demo-thread-2", "Demo: label the cauldron shelf"),
        export("threads/log", "demo-thread-1", json!({"thread_id": "demo-thread-1", "events": [
            item(1, "user_message", "what goes in first?"),
            item(2, "assistant_text", "Water, then the herbs, then patience.")], "retained": 0}), json!({"events": known()})),
    ]
}
