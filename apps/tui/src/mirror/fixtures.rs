//! **FRAMES, NOT STATE.** The test-only frame builders every seam test seeds
//! through.
//!
//! The point of this file is that a test may not poke a `Mirror`'s insides. It
//! builds the bytes a socket would deliver and hands them to
//! [`crate::seam::held::Held::fold`], so
//! what a test proves is what the real delivery path does — and the frame shapes
//! here are the producer's, transcribed from `lib/threads/src/shapes/*.rs`'s own
//! `to_json` and verified against the live daemon's `shapes/list` on 2026-10-07.
//!
//! **A SECOND COPY, DELIBERATELY**, beside `apps/aui/src/mirror/fixtures.rs`.
//! The operator's own framing of this design is *"the same backend and just
//! different skins... with a copy in each one instead of a real shared
//! backend"*, and the fixtures are part of what is copied: a shared
//! `seam-fixtures` crate both faces linked would be the shared backend the
//! whole arrangement refused, and `tests/it/seam_reference.rs` is what keeps the
//! copying honest for the files that matter.
//!
//! A hand-written row that drifted from the producer's would make every test
//! here a tautology, which is why the shapes this face reads are listed cell by
//! cell below with the file they came from.

use serde_json::{json, Value};

use crate::seam::held::{
    ACCOUNTS, FLEET, HEARTBEAT, LINK, LOG, NARRATION, NON_IDLE, RECENT, SUMMARY, THREADS,
};

/// **AN EXPORT: a labelled read of current state.** `op: read`. The one-line
/// rule's first half — *hold the export* — and it REPLACES the whole row.
pub fn export(shape: &str, key: Option<&str>, row: Value, cells: Value) -> Value {
    let mut v = json!({
        "shape": shape,
        "op": "read",
        "as_of": "2026-10-07T12:00:00Z",
        "epoch": {"init_at": "2026-10-07T11:59:59Z", "reinit": 0, "lost": 0, "state": "known"},
        "cells": cells,
        "row": row,
    });
    if let Some(k) = key {
        v["key"] = json!(k);
    }
    v
}

/// The same, carrying the `subscribe_id` and `complete` that rule 2.7's stages
/// are read off.
pub fn export_for(
    shape: &str,
    key: Option<&str>,
    row: Value,
    cells: Value,
    subscribe_id: u64,
    complete: bool,
) -> Value {
    let mut v = export(shape, key, row, cells);
    v["subscribes"] = json!([subscribe_id]);
    v["complete"] = json!(complete);
    v
}

/// **A DERIVED EVENT: an occurrence.** `op: live`. The one-line rule's second
/// half — *apply everything after it*.
pub fn live(shape: &str, key: Option<&str>, at: &str, event: Value) -> Value {
    let mut v = json!({
        "shape": shape,
        "op": "live",
        "at": at,
        "epoch": {"reinit": 0, "lost": 0, "state": "known"},
        "event": event,
    });
    if let Some(k) = key {
        v["key"] = json!(k);
    }
    v
}

/// A `Known` cell state, which is what `fact_of` branches on.
pub fn known(at: &str) -> Value {
    json!({"state": "known", "complete": true, "folded_at": at})
}

const AT: &str = "2026-10-07T12:00:00Z";

// ──────────────────────────────────────────────────────────────────────────
//  accounts — `lib/threads/src/shapes/accounts.rs::AccountRow::to_json`
// ──────────────────────────────────────────────────────────────────────────

/// **One account row, `accounts` v2** — since R2.S8 it carries NO threads: the
/// roster that rode it as `accounts.*.threads` moved to `threads/non-idle`
/// ([`non_idle_export`]). `registered` is the one cell a fleet test needs to make
/// the account board KNOWN.
pub fn accounts_export(account: &str) -> Value {
    export(
        ACCOUNTS,
        Some(account),
        json!({"account": account, "registered": true}),
        json!({"registered": known(AT)}),
    )
}

/// **THE RETIRED SHAPE, KEPT ONLY AS THE OLD FRAME A TEST FEEDS TO PROVE IT IS
/// NOT READ.** `accounts` v1 carried `threads: {thread_id: {state, executing,
/// queued, unread, detail, since}}` on each login's row; R2.S8 deleted that cell.
/// A face that still read it would answer from this frame, so a test that folds
/// it and then asserts the fleet is still UNKNOWN is the guard that a face did
/// not keep the old read. Never use it to seed a fleet.
pub fn legacy_accounts_export_with_threads(account: &str, threads: &[(&str, bool)]) -> Value {
    let placed: serde_json::Map<String, Value> = threads
        .iter()
        .map(|(id, executing)| {
            (
                (*id).to_string(),
                json!({
                    "state": if *executing { "active" } else { "parked" },
                    "executing": executing,
                    "queued": 0,
                    "unread": 0,
                    "detail": Value::Null,
                    "since": AT,
                }),
            )
        })
        .collect();
    export(
        ACCOUNTS,
        Some(account),
        json!({
            "account": account,
            "registered": true,
            "threads": Value::Object(placed),
        }),
        json!({"threads": known(AT), "registered": known(AT)}),
    )
}

// ──────────────────────────────────────────────────────────────────────────
//  threads/non-idle — `lib/threads/src/shapes/non_idle.rs`, as recorded in
//  `libs/shapewire/seam/conformance/fleet_non_idle.jsonl`
// ──────────────────────────────────────────────────────────────────────────

/// **THE ROSTER: one key, `fleet`, every non-idle thread with its placement.**
/// Each entry is `(id, account, state, executing)`; `queued` is 0 and `since`
/// is fixed. An export REPLACES the whole roster — a thread that stops being
/// named stops being non-idle — which is the property the per-login `accounts`
/// rows used to give per login.
pub fn non_idle_export(placed: &[(&str, &str, &str, bool)]) -> Value {
    let threads: Vec<Value> = placed
        .iter()
        .map(|(id, account, state, executing)| {
            json!({
                "id": id,
                "account": account,
                "state": state,
                "executing": executing,
                "queued": 0,
                "since": AT,
            })
        })
        .collect();
    export(
        NON_IDLE,
        Some(FLEET),
        json!({"pool": FLEET, "threads": threads}),
        json!({"threads": known(AT)}),
    )
}

/// `threads/non-idle` whose entries carry `ancestors` (R9.UI1): `(id, account,
/// state, executing, ancestors nearest first)`.
pub fn non_idle_export_with_ancestors(placed: &[(&str, &str, &str, bool, &[&str])]) -> Value {
    let threads: Vec<Value> = placed
        .iter()
        .map(|(id, account, state, executing, anc)| {
            json!({
                "id": id,
                "account": account,
                "state": state,
                "executing": executing,
                "queued": 0,
                "since": AT,
                "parent_id": anc.first(),
                "ancestors": anc,
            })
        })
        .collect();
    export(
        NON_IDLE,
        Some(FLEET),
        json!({"pool": FLEET, "threads": threads}),
        json!({"threads": known(AT)}),
    )
}

// ──────────────────────────────────────────────────────────────────────────
//  threads — `shapes/threads.rs::ThreadCells::to_json`
// ──────────────────────────────────────────────────────────────────────────

/// One thread's row. `status` is the `Edge`'s LEVEL — `{state, executing,
/// detail}` — and the transitions ride beside it under `status_transitions`,
/// which is the spelling the vendored mirror holds apart and the board's own
/// `ShapeMirror` got wrong.
#[allow(clippy::too_many_arguments)]
pub fn threads_export(
    id: &str,
    state: &str,
    executing: bool,
    seen_seq: u64,
    newest_seq: u64,
    settings: Value,
    account: Option<&str>,
) -> Value {
    export(
        THREADS,
        Some(id),
        json!({
            "thread_id": id,
            "header": {"cwd": "/tmp", "model": "sonnet", "created_at": AT, "updated_at": AT},
            "settings": settings,
            "account": account,
            "status": {"state": state, "executing": executing, "detail": Value::Null},
            "status_transitions": [],
            "queued": 0,
            "last_message_id": Value::Null,
            "seen_seq": seen_seq,
            "unread": 0,
            "newest": {"seq": newest_seq, "message_id": Value::Null, "kind": "assistant_text", "ts": AT},
            "body": Value::Null,
            "compaction": Value::Null,
            "liveness": {"busy": executing, "parent_running": false, "running_subagents": []},
            "app": {},
        }),
        json!({
            "header": known(AT),
            "settings": known(AT),
            "account": known(AT),
            "status": known(AT),
            "status_transitions": known(AT),
            // **EVERY CELL `view::positions` READS, DECLARED.** It used to gate
            // on `newest` alone and read the other four off the raw row with
            // `.unwrap_or(0)`; it now reads each one as its OWN cell and answers
            // `NotHydrated` for the whole of `Positions` if any is not declared
            // known. So a fixture that carried a value in the row but left its
            // cell out was asserting a default, not a fact — and `last_seq`
            // silently became `0`. The row below already carries all five; this
            // says the producer told us so, which a real export always does.
            "queued": known(AT),
            "last_message_id": known(AT),
            "seen_seq": known(AT),
            "unread": known(AT),
            "newest": known(AT),
            "liveness": known(AT),
            "app": known(AT),
        }),
    )
}

/// The common case: a thread whose status is all the test cares about.
pub fn thread_status(id: &str, state: &str, executing: bool) -> Value {
    threads_export(id, state, executing, 0, 0, Value::Null, None)
}

/// A status EDGE after the export — the daemon took or released the lock.
pub fn thread_status_edge(id: &str, state: &str, executing: bool) -> Value {
    live(
        THREADS,
        Some(id),
        AT,
        json!({
            "kind": "row",
            "row": {
                "thread_id": id,
                "status": {"state": state, "executing": executing, "detail": Value::Null},
                "liveness": {"busy": executing, "parent_running": false, "running_subagents": []},
            }
        }),
    )
}

// ──────────────────────────────────────────────────────────────────────────
//  threads/log — `shapes/log.rs::LogCells::to_json`
// ──────────────────────────────────────────────────────────────────────────

/// One window item. `payload` is externally tagged — `{"here": {...}}` — so the
/// discriminator is a key and not a field whose absence has to be interpreted.
pub fn log_item(seq: u64, kind: &str, ts: &str, event: Value) -> Value {
    let mut ev = event;
    if let Value::Object(m) = &mut ev {
        m.insert("kind".into(), json!(kind));
    }
    json!({
        "seq": seq,
        "kind": kind,
        "ts": ts,
        "payload": {"here": ev},
    })
}

/// A `Payload::Split` item: a MESSAGE whose `text` the producer lifted out.
pub fn log_item_split(seq: u64, kind: &str, ts: &str, event: Value, id: &str, bytes: u64) -> Value {
    let mut ev = event;
    if let Value::Object(m) = &mut ev {
        m.insert("kind".into(), json!(kind));
        m.remove("text");
    }
    json!({
        "seq": seq,
        "kind": kind,
        "ts": ts,
        "payload": {"split": {"event": ev, "text": {"id": id, "bytes": bytes, "transports": ["ws"]}}},
    })
}

/// The window, as an export. **Oldest first**, which is `Sequence`'s order and
/// the opposite of `Recency`'s.
pub fn log_export(id: &str, items: Vec<Value>) -> Value {
    export(
        LOG,
        Some(id),
        json!({"thread_id": id, "events": items, "retained": 0}),
        json!({"events": known(AT)}),
    )
}

/// One item appended after the export — the live half.
pub fn log_transition(id: &str, item: Value) -> Value {
    live(
        LOG,
        Some(id),
        AT,
        json!({"kind": "transition", "cell": "events", "item": item}),
    )
}

// ──────────────────────────────────────────────────────────────────────────
//  the small shapes
// ──────────────────────────────────────────────────────────────────────────

/// `threads/recent-updates` — `Recency`, NEWEST FIRST, `id` and `at` on each
/// entry so the order is checkable rather than merely asserted.
pub fn recent_export(entries: &[(&str, u64, u64)]) -> Value {
    let rows: Vec<Value> = entries
        .iter()
        .map(|(id, unread, seen_seq)| json!({"id": id, "at": AT, "unread": unread, "seen_seq": seen_seq}))
        .collect();
    export(
        RECENT,
        Some(FLEET),
        json!({"pool": FLEET, "threads": rows}),
        json!({"threads": known(AT)}),
    )
}

/// **A SHAPE-LEVEL SCALAR, WHICH RIDES `shared` AND NOT `row`.**
///
/// `threads/summary`'s `truncated` is the producer's own statement that its key
/// set hit the 500-row cap — so a face knows whether a missing title means
/// *unnamed* or *outside the window*. It is read through
/// `view::summary_truncated`, which goes to `Mirror::shared_cell`, so a fixture
/// that put it under `row`/`cells` like a keyed cell would be silently ignored
/// and the test over it would pass on `false` forever.
pub fn summary_truncated_export(truncated: bool) -> Value {
    json!({
        "shape": SUMMARY,
        "op": "read",
        "as_of": AT,
        "epoch": {"init_at": AT, "reinit": 0, "lost": 0, "state": "known"},
        "shared": {"truncated": {"truncated": truncated}},
        "shared_cells": {"truncated": known(AT)},
    })
}

pub fn summary_export(id: &str, title: &str) -> Value {
    export(
        SUMMARY,
        Some(id),
        json!({"thread_id": id, "title": {"title": title, "idx": 1, "seq": 1, "at": AT}}),
        json!({"title": known(AT)}),
    )
}

pub fn heartbeat_export(seq: u64, at: &str) -> Value {
    let mut v = export(
        HEARTBEAT,
        Some(LINK),
        json!({"link": LINK, "seq": seq}),
        json!({"seq": known(at)}),
    );
    // The chassis stamps every retained item; `Held::at()` is what
    // `frames_stalled_for` reads, and it comes off `at` on the frame.
    v["at"] = json!(at);
    v
}

/// **THE PRODUCER'S OWN LINE SHAPE, AND IT HAS NO `text` FIELD.**
///
/// `lib/threads/src/shapes/narration.rs`'s `NARRATION_CELLS` declares `lines`
/// as a list of `Line` whose cells are exactly:
///
/// ```text
///   kind (enum progress|lifecycle|info, required)   at (rfc3339, required)
///   op  id  account  phase  detail  done  ok  service  reason  boot_id  domain
/// ```
///
/// **THE FICTION THIS FILE REFUSED IS GONE FROM THE CHASSIS.** What stood here
/// said that `apps/aui/src/mirror/fixtures.rs` built `{at, text}`, that no
/// producer on this wire has ever sent a `text` on a narration line, and that
/// *"the fix belongs upstream in the chassis's own `view.rs`, and until it lands
/// this face reads `Line::raw` and says why."*
///
/// It landed: `Line::text` is gone and `Line::kind` replaced it, read off the
/// producer's own `Required` tag, and `view::narration` DROPS a line with no
/// `kind` rather than carrying an empty one. The AUI's builder has been corrected
/// to carry `kind` in the same release. This file still takes the RAW line, which
/// is now simply the honest way to write a fixture rather than a protest against
/// the kit — a test here can only assert over fields the producer really sends.
///
/// `fixtures.rs` is not one of the SEVEN files `tests/it/seam_reference.rs` diffs,
/// so a difference between the two faces' builders is legal here.
pub fn narration_export(channel: &str, lines: &[Value]) -> Value {
    export(
        NARRATION,
        Some(channel),
        json!({"channel": channel, "lines": lines}),
        json!({"lines": known(AT)}),
    )
}

/// One `progress` line: the firehose's `ServerMsg::Progress` fields, under the
/// producer's own names.
pub fn progress_line(at: &str, op: &str, phase: &str, detail: &str, done: bool, ok: bool) -> Value {
    json!({
        "kind": "progress", "at": at, "op": op, "id": "op-1", "account": "a",
        "phase": phase, "detail": detail, "done": done, "ok": ok,
    })
}

/// One `lifecycle` line: `ServerMsg::Lifecycle`'s fields.
pub fn lifecycle_line(at: &str, service: &str, phase: &str, reason: &str) -> Value {
    json!({
        "kind": "lifecycle", "at": at, "service": service, "phase": phase,
        "reason": reason, "boot_id": "boot-2",
    })
}

/// One `info` line: `ServerMsg::Info`'s fields. *What you are showing just
/// changed — re-read `domain`.* It carries no value, deliberately.
pub fn info_line(at: &str, domain: &str, reason: &str) -> Value {
    json!({"kind": "info", "at": at, "domain": domain, "reason": reason, "account": "a"})
}

/// **THE PRODUCER'S SURFACE, FROM THE ONE PLACE IT IS RECORDED.**
///
/// `libs/shapewire/inventory.json` — a COMMITTED artifact of the chassis repo,
/// written by `make-inventory.py` against a running daemon and COPIED by
/// `vendor-chassis.sh`, so a reviewer saw it land in a diff rather than having a
/// script regenerate it during a copy. `tests/it/seam_reference.rs` routes the
/// seam's method table and shape names against the same file.
///
/// **This used to be a second capture of `shapes/list` taken by this step, and
/// that was a second source for one fact.** The ceilings two tests here depend on
/// — `threads/log`'s `retained_items: 4000` and `open_keys_per_conn: 8`, and
/// `threads/recent-updates`' `retained_items: 32` — are in the inventory, so the
/// capture is deleted and there is one record of what the producer serves.
///
/// What is SYNTHESISED, named so nobody reads it as the producer's: the
/// per-connection bookkeeping a `shapes/list` reply carries and an inventory does
/// not (`keys`, `subscribers`, `epoch`, `fifo_high_water`, `bursts`, `consumers`).
/// None of it is a contract — it is this moment's state of one daemon — and
/// nothing in this face reads any of it.
pub fn shapes_list() -> Value {
    let inv: Value = serde_json::from_str(include_str!(
        "../../../../libs/shapewire/seam/inventory.json"
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

/// The same, as the typed inventory `Held::learn` takes.
pub fn shape_infos() -> Vec<shapewire::wire::ShapeInfo> {
    serde_json::from_value(shapes_list()["shapes"].clone())
        .expect("the vendored inventory is what this build parses")
}
