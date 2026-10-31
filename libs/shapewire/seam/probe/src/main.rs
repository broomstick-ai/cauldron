//! **THE RE-SUBSCRIBE IDENTITY PROOF, AGAINST THE LIVE `threads` DAEMON.**
//!
//! `tests/resubscribe_identity.rs` in the vendored chassis proves rule 2.5 over
//! SYNTHETIC frames. That proof is necessary and it is not sufficient: the
//! frames are ones I wrote, so it cannot catch a producer that exports something
//! other than what I assumed. This binary runs the same property over frames the
//! real daemon actually sends, for the shape the whole R2.S2 adoption turns on —
//! `threads/log`, a bounded `Sequence` of exactly 4,000 rows, matching
//! `apps/tui/src/store.rs`'s own `OPEN_CAP`.
//!
//! **The case that justifies the design, and only production has it.** The
//! subject thread holds ~175,858 ledger events. A 4,000-row window over it is
//! 2.3% of the tail. The claim being tested is that a face whose buffer is 4,000
//! deep can DELETE that buffer and read the mirror — and the thing that would
//! sink it is if a re-subscribe over a 175k tail answered something different
//! from what a continuously-connected consumer holds.
//!
//! **READ-ONLY, AND STRUCTURALLY SO.** It sends exactly three method names:
//! `shapes/list`, `shapes/threads/log/subscribe` and
//! `shapes/threads/log/unsubscribe`. There is no command verb in this file, no
//! write, no restart, and the vendored chassis is linked at
//! `default-features = false` so the producer half is not even compiled in.
//!
//! What it prints is the comparison, and it exits non-zero if the two mirrors
//! disagree — so it is a test, not a report.

use std::collections::BTreeMap;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use shapewire::mirror::Mirror;
use tokio_tungstenite::tungstenite::Message;

const SHAPE: &str = "threads/log";

/// The shapes the daemon must serve for this proof to mean anything. Seven as of
/// `290113e`; `threads/log` is the one that arrived with it.
const EXPECTED_SHAPES: &[&str] = &[
    "accounts",
    "heartbeat",
    "narration",
    "threads",
    "threads/log",
    "threads/recent-updates",
    "threads/summary",
];

/// One connection's whole life: connect, handshake, learn, subscribe, fold until
/// the export is complete, then drop the socket.
///
/// Returns the mirror and the raw export frames, so the caller can compare both
/// the folded state and the bytes.
async fn session(url: &str, key: &str, note: &str) -> Result<(Mirror, Vec<Value>), String> {
    let (ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| format!("{note}: connect: {e}"))?;
    let (mut tx, mut rx) = ws.split();

    // ── handshake ───────────────────────────────────────────────────────────
    let welcome = next_json(&mut rx, note).await?;
    if welcome["type"] != "welcome" {
        return Err(format!("{note}: first frame was not a welcome: {welcome}"));
    }
    if welcome["auth_required"].as_bool().unwrap_or(false) {
        return Err(format!(
            "{note}: this daemon wants a secret; the probe deliberately holds none"
        ));
    }
    let boot_id = welcome["boot_id"].as_str().unwrap_or("?").to_string();

    // ── learn: one `shapes/list`, which is the ONLY way to read the ceilings ─
    send(&mut tx, "1", "shapes/list", json!({}), note).await?;
    let mut info_for_shape: Option<Value> = None;
    let mut served: Vec<String> = Vec::new();
    loop {
        let v = next_json(&mut rx, note).await?;
        if v["type"] == "response" && v["correlate"] == json!("1") {
            let shapes = v["outcome"]["payload"]["shapes"]
                .as_array()
                .cloned()
                .ok_or_else(|| format!("{note}: shapes/list had no shapes: {v}"))?;
            for s in &shapes {
                if let Some(n) = s["name"].as_str() {
                    served.push(n.to_string());
                    if n == SHAPE {
                        info_for_shape = Some(s.clone());
                    }
                }
            }
            break;
        }
    }
    served.sort();
    println!("  [{note}] boot_id={boot_id}  shapes served: {}", served.len());
    for want in EXPECTED_SHAPES {
        if !served.iter().any(|s| s == want) {
            return Err(format!(
                "{note}: the daemon does not serve `{want}` — this proof needs it"
            ));
        }
    }
    let info = info_for_shape.ok_or_else(|| format!("{note}: no ShapeInfo for {SHAPE}"))?;
    let mut m = Mirror::new(SHAPE);
    // `Mirror::learn` takes the typed `ShapeInfo`; going through the wire type is
    // the point — a field the consumer cannot parse is a finding here, not later.
    match serde_json::from_value::<shapewire::wire::ShapeInfo>(info.clone()) {
        Ok(parsed) => {
            println!(
                "  [{note}] learned ceilings: retained_items={} rows_per_subscribe={}",
                parsed.max.retained_items, parsed.max.rows_per_subscribe
            );
            m.learn(&parsed);
        }
        Err(e) => return Err(format!("{note}: ShapeInfo did not parse: {e}\n{info}")),
    }

    // ── subscribe: ONE ask, no position, no range, no `after_seq` ────────────
    //
    // This is the whole of recovery. If a cursor were needed, it would have to
    // appear in these params, and there is nowhere in them to put one.
    // `shape_version` is FREE TEXT and the producer refuses a non-string
    // (`ledger.rs::ConsumerIdentity::parse`: *"required and must be a non-empty
    // string: say what you were built against, even if it is a date"*). Note
    // that `docs/shapes.md` rule 2.1 calls it *"`version` an integer"* — the
    // producer is the authority and the doc is stale; sending the integer the
    // doc describes is refused `malformed`, which is how this was found.
    let params = json!({
        "key": key,
        "consumer": {
            "name": "tui",
            "shape_version": "290113e",
            "pid": std::process::id(),
            "build": "liveprobe/R2.S2",
        },
    });
    println!(
        "  [{note}] subscribe params: {}",
        serde_json::to_string(&params).unwrap()
    );
    send(&mut tx, "2", &format!("shapes/{SHAPE}/subscribe"), params, note).await?;

    // ── fold until the export says it is complete ────────────────────────────
    let mut exports: Vec<Value> = Vec::new();
    let mut frames = 0usize;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(format!("{note}: no complete export within 30s"));
        }
        let v = match tokio::time::timeout(left, next_json(&mut rx, note)).await {
            Ok(r) => r?,
            Err(_) => return Err(format!("{note}: no complete export within 30s")),
        };
        if v["type"] == "response" && v["correlate"] == json!("2") {
            let out = &v["outcome"];
            println!(
                "  [{note}] subscribe reply: {}",
                serde_json::to_string(out).unwrap_or_default()
            );
            // A refusal is `{"type":"error","code":...,"error":...}` — branch on
            // the CODE, never on the prose (rule: the chassis door types its
            // refusals so a consumer never string-matches).
            if out["type"] == json!("error") || out.get("code").is_some() {
                return Err(format!(
                    "{note}: subscribe refused, code={} — {}",
                    out["code"], out["error"]
                ));
            }
            continue;
        }
        if v["type"] != "shape" {
            println!("  [{note}] other frame: type={}", v["type"]);
            continue;
        }
        if frames < 3 || v["op"] == json!("read") {
            let cells: Vec<String> = v["cells"]
                .as_object()
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
            println!(
                "  [{note}] shape frame #{}: op={} key={} complete={} subscribes={} cells={:?}",
                frames + 1,
                v["op"],
                v["key"],
                v["complete"],
                v["subscribes"],
                cells
            );
        }
        frames += 1;
        let r = m.accept(&v);
        if v["op"] == json!("read") {
            exports.push(v.clone());
        }
        if !r.rejected.is_empty() {
            for rj in &r.rejected {
                println!("  [{note}] REJECTED {} {:?}", rj.reason, rj.detail);
            }
        }
        if v["complete"] == json!(true) {
            println!("  [{note}] export complete after {frames} frame(s)");
            break;
        }
    }

    // ── THE FORCING, and it is in the probe rather than in the daemon ───────
    //
    // `core:writing-tools`: a green nobody has forced to fail is not evidence,
    // and this probe's green is the headline claim of R2.S2 — so it has to be
    // shown capable of going red. The honest mutation is the DIVERGENCE the
    // probe exists to detect: session A folds one more item than the producer
    // ever sent, as a consumer with a leaked buffer across the reconnect would.
    // Nothing is sent to the daemon and nothing about the daemon changes; the
    // lie is told to A's mirror only.
    if std::env::var("FORCE_RED").is_ok() && note == "A" {
        let fake = json!({
            "shape": SHAPE, "key": key, "op": "live",
            "at": "2026-10-07T18:00:00Z",
            "epoch": {"reinit": 0, "lost": 0, "state": "known"},
            "event": {
                "kind": "transition", "cell": "events",
                "item": {"seq": 999_999_999u64, "kind": "forced_red_phantom"},
            },
        });
        let r = m.accept(&fake);
        println!(
            "  [{note}] FORCE_RED: injected one phantom item (applied={})",
            r.applied()
        );
    }

    // Narrow the subscription before dropping, so the producer is not left
    // thinking this connection still cares. (Dropping the socket would do it;
    // saying so is politer and is what the seam's `Book::release` does.)
    let _ = send(
        &mut tx,
        "3",
        &format!("shapes/{SHAPE}/unsubscribe"),
        json!({"key": key}),
        note,
    )
    .await;
    let _ = tx.send(Message::Close(None)).await;
    Ok((m, exports))
}

async fn send<S>(tx: &mut S, id: &str, method: &str, params: Value, note: &str) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
{
    let frame = json!({"type": "request", "id": id, "method": method, "params": params});
    tx.send(Message::Text(frame.to_string()))
        .await
        .map_err(|_| format!("{note}: send {method} failed"))
}

async fn next_json<S>(rx: &mut S, note: &str) -> Result<Value, String>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match rx.next().await {
            Some(Ok(Message::Text(t))) => {
                return serde_json::from_str(&t).map_err(|e| format!("{note}: bad json: {e}"))
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(format!("{note}: socket: {e}")),
            None => return Err(format!("{note}: socket closed")),
        }
    }
}

/// What a face renders off one `threads/log` key: the window's items, in order,
/// plus the cell's completeness. Deliberately NOT the stamps (see the synthetic
/// proof's module doc for why those must differ).
fn window(m: &Mirror, key: &str) -> (usize, Vec<(u64, String)>, bool, Option<String>) {
    let held = match m.held(key) {
        Some(h) => h,
        None => return (0, Vec::new(), false, None),
    };
    // The cell name is the shape's own; discover it rather than assume it, so a
    // rename on the producer is a loud failure here instead of an empty window.
    let cell = held
        .cells()
        .keys()
        .find(|k| !held.items(k).is_empty())
        .cloned()
        .unwrap_or_else(|| "tail".to_string());
    let items = held.items(&cell);
    let ids: Vec<(u64, String)> = items
        .iter()
        .map(|it| {
            (
                it.get("seq").and_then(Value::as_u64).unwrap_or(0),
                it.get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string(),
            )
        })
        .collect();
    let (complete, covers) = match held.cell(&cell) {
        shapewire::wire::CellState::Known {
            complete,
            covers_since,
            ..
        } => (complete, covers_since.map(|t| t.to_rfc3339())),
        _ => (false, None),
    };
    (items.len(), ids, complete, covers)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let key = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "57db2ec9-e6c0-4492-89d0-19f7070ee0db".to_string());
    let port = args.get(2).cloned().unwrap_or_else(|| "3653".to_string());
    let url = format!("ws://127.0.0.1:{port}/ws");

    println!("── rule 2.5 against the LIVE daemon ──────────────────────────────");
    println!("   shape : {SHAPE}   (window 4,000 — matches the TUI's OPEN_CAP)");
    println!("   key   : {key}");
    println!("   door  : {url}  (chassis door; typed refusals)");
    println!();

    // ── A: the consumer that never disconnected ─────────────────────────────
    println!("A. a consumer that subscribes once:");
    let (a, a_exports) = match session(&url, &key, "A").await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("FAILED: {e}");
            std::process::exit(2);
        }
    };

    // ── B: a WHOLE NEW CONNECTION. Socket death is total: new socket, new
    //      ConnId, new welcome, one subscribe. No cursor was carried between
    //      these two sessions because there is nowhere to carry one.
    println!();
    println!("B. the socket died; a NEW connection subscribes again:");
    let (b, b_exports) = match session(&url, &key, "B").await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("FAILED: {e}");
            std::process::exit(2);
        }
    };

    let (an, aids, ac, acov) = window(&a, &key);
    let (bn, bids, bc, bcov) = window(&b, &key);

    println!();
    println!("── the comparison ────────────────────────────────────────────────");
    println!("  A: {an:>5} items  complete={ac}  covers_since={acov:?}");
    println!("  B: {bn:>5} items  complete={bc}  covers_since={bcov:?}");
    if let (Some(fa), Some(la)) = (aids.first(), aids.last()) {
        println!("  A: seq {} … {}", fa.0, la.0);
    }
    if let (Some(fb), Some(lb)) = (bids.first(), bids.last()) {
        println!("  B: seq {} … {}", fb.0, lb.0);
    }

    // What the window is a window ONTO — the honest ratio, which is the whole
    // argument for the shape.
    println!();
    println!("  exports: A={} B={}", a_exports.len(), b_exports.len());

    let mut bad = 0;

    if an != bn {
        println!("  ✗ item COUNT differs ({an} vs {bn})");
        bad += 1;
    } else {
        println!("  ✓ item count identical ({an})");
    }
    if aids != bids {
        // Where, exactly. A silent "they differ" would be useless.
        match aids.iter().zip(bids.iter()).position(|(x, y)| x != y) {
            Some(first) => {
                println!("  ✗ item SEQUENCE differs, first at index {first}:");
                println!("      A[{first}] = {:?}", aids.get(first));
                println!("      B[{first}] = {:?}", bids.get(first));
            }
            // **NOT `unwrap_or(0)`.** Every overlapping index agrees and the
            // difference is in the TAIL, so naming index 0 — where the two are
            // identical — would be a diagnostic that lies about its own finding.
            // The first run of the forcing printed exactly that and it is fixed
            // here rather than tolerated.
            None => {
                let n = aids.len().min(bids.len());
                println!(
                    "  ✗ item sequences AGREE on all {n} overlapping indices; \
                     they differ only in LENGTH ({} vs {})",
                    aids.len(),
                    bids.len()
                );
                println!("      A tail beyond the overlap: {:?}", &aids[n..]);
                println!("      B tail beyond the overlap: {:?}", &bids[n..]);
            }
        }
        bad += 1;
    } else {
        println!("  ✓ every item identical, in order (seq + kind)");
    }
    if ac != bc {
        println!("  ✗ completeness differs ({ac} vs {bc})");
        bad += 1;
    } else {
        println!("  ✓ completeness identical ({ac})");
    }

    // The row (the key's level cells) must match too — `threads/log` carries a
    // row beside its sequence.
    if a.row(&key) != b.row(&key) {
        println!("  ✗ the ROW differs:");
        println!("      A = {}", a.row(&key));
        println!("      B = {}", b.row(&key));
        bad += 1;
    } else {
        println!("  ✓ the row is identical");
    }

    // And the cell-state KINDS, which is what makes a face draw a value rather
    // than a `?`.
    let kinds = |m: &Mirror| -> BTreeMap<String, bool> {
        m.held(&key)
            .map(|h| {
                h.cells()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.is_known()))
                    .collect()
            })
            .unwrap_or_default()
    };
    if kinds(&a) != kinds(&b) {
        println!("  ✗ cell-state kinds differ: {:?} vs {:?}", kinds(&a), kinds(&b));
        bad += 1;
    } else {
        println!("  ✓ cell-state kinds identical ({:?})", kinds(&a));
    }

    println!();
    if bad == 0 {
        println!("RESULT: IDENTICAL. Recovery was one `subscribe` — no cursor, no");
        println!("        `after_seq`, no re-emit verb, nothing carried between the");
        println!("        two connections. The repair machinery is deletable.");
    } else {
        println!("RESULT: {bad} DISAGREEMENT(S). Rule 2.5 does not hold here and the");
        println!("        repair machinery is load-bearing after all.");
        std::process::exit(1);
    }
}
