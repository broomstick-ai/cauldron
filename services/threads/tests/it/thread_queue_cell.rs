//! # The `queue` cell on `threads` — the two RPCs that had nowhere to go
//!
//! `docs/consumer-seam.md` §4 recorded `threads/{id}/queue` and
//! `threads/{id}/queue_failed` as the pair that had to STAY RPC, because *"no
//! shape carries queue contents"*. This is the cell that carries them, and these
//! are the properties that license deleting the two methods once the faces move.
//!
//! The two RPCs projected the SAME four fields out of `store::QueueRow` and
//! differed in one `WHERE state=`, so they are one cell with a declared `state`
//! word — see `shapes::threads::QueueEntry`.
//!
//! **Every test here was forced red before it was believed**, and the mutation is
//! in the test's own docstring. The rig drives the HUB directly rather than a
//! websocket, which is `thread_shapes.rs`' convention and for its reasons.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use threads::config::Config;
use threads::shape::{ConnFrames, Derived, Hub, Op};
use threads::shapes::threads as tshape;
use threads::{build_manager, ThreadManager, ThreadSettings};

// ─────────────────────────────────────────────────────────────────────────────
// fixture — `thread_shapes.rs`', narrowed to the one shape under test
// ─────────────────────────────────────────────────────────────────────────────

async fn start() -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "first".to_string(),
        tmp.path().join("first").to_string_lossy().to_string(),
    );
    // The clock seam: two emits inside one microsecond mean the second is
    // DROPPED by the cell's `folded_at` guard, which is what an optimised build
    // does to a tight emit loop. See `src/clock.rs`.
    threads::clock::testing::ticking();
    let manager = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();
    let hub: Arc<Hub> = manager.hub.clone();
    hub.register(tshape::Threads, manager.clone());
    (manager, tmp)
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
}

fn thread_on(m: &ThreadManager, tmp: &std::path::Path) -> String {
    let mut s = ThreadSettings::new(tmp.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    m.create_thread(None, s).unwrap()
}

fn drain(f: &mut ConnFrames) -> Vec<Arc<threads::shape::ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

/// The LAST export for `key` — a re-export supersedes an earlier one.
fn last_export(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    frames
        .iter()
        .filter(|f| f.wire.op == Op::Read && f.wire.key.as_deref() == Some(key))
        .last()
        .unwrap_or_else(|| panic!("no export for `{key}` among {} frames", frames.len()))
        .wire
        .row
        .clone()
        .unwrap_or(Value::Null)
}

/// The newest `queue` the fold PUBLISHED LIVE — off `wire.event`'s
/// `Derived::Row`, which is where a live row arrives. Reading `wire.row` here
/// would silently find the hydrate's export instead and make the live tests pass
/// without a live frame ever being sent.
fn queue_now(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    frames
        .iter()
        .filter(|f| f.wire.op == Op::Live && f.wire.key.as_deref() == Some(key))
        .filter_map(|f| match &f.wire.event {
            Some(Derived::Row { row }) => Some(row["queue"].clone()),
            _ => None,
        })
        .filter(|q| !q.is_null())
        .last()
        .unwrap_or(Value::Null)
}

async fn watch(m: &ThreadManager, conn: u64, id: &str) -> ConnFrames {
    let f = m.hub.connect(conn);
    m.hub.subscribe("threads", Some(id), conn, None).unwrap();
    settle().await;
    f
}

/// Put one WAITING row on the thread. Answers the queue row id.
fn enqueue(m: &ThreadManager, id: &str, message_id: u64, text: &str) -> i64 {
    m.store
        .enqueue(id, message_id, text, "turn_end", &Value::Null, &[])
        .unwrap()
}

/// Put one FAILED row on the thread. `fail_deliveries` is guarded on
/// `state='delivered'`, so the row has to go out before it can come back failed
/// — which is also the only way it happens in production.
fn fail(m: &ThreadManager, id: &str, message_id: u64, text: &str) {
    let row = enqueue(m, id, message_id, text);
    m.store.mark_delivered(&[row]).unwrap();
    assert_eq!(
        m.store.fail_deliveries(id, &[message_id]).unwrap(),
        1,
        "the fixture must actually move a row to `failed`"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. ONE CELL, BOTH STATES
// ─────────────────────────────────────────────────────────────────────────────

/// **THE CELL CARRIES WAITING AND FAILED ROWS, DISCRIMINATED BY `state`.**
///
/// This is the whole claim that collapses two RPCs into one cell: a consumer
/// that rendered `threads/{id}/queue` reads the `waiting` entries and one that
/// rendered `threads/{id}/queue_failed` reads the `failed` ones, out of one
/// subscribe.
///
/// **THE MUTATION.** `ThreadManager::thread_queue` with the `failed_deliveries`
/// half deleted. Red on the `failed` entry's absence, which is the half that
/// used to need the second call.
#[tokio::test]
async fn the_queue_cell_holds_both_states_discriminated_by_the_state_word() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());

    enqueue(&m, &id, 1, "the waiting one");
    fail(&m, &id, 2, "the failed one");

    let mut f = watch(&m, 11, &id).await;
    let row = last_export(&drain(&mut f), &id);
    let q = row["queue"].as_array().expect("the queue cell is a list");

    assert_eq!(q.len(), 2, "both rows, one cell: {row}");

    let waiting: Vec<&Value> = q.iter().filter(|e| e["state"] == json!("waiting")).collect();
    let failed: Vec<&Value> = q.iter().filter(|e| e["state"] == json!("failed")).collect();
    assert_eq!(waiting.len(), 1, "one waiting row: {q:?}");
    assert_eq!(failed.len(), 1, "one failed row: {q:?}");

    assert_eq!(waiting[0]["message_id"], json!(1));
    assert_eq!(waiting[0]["text"], json!("the waiting one"));
    assert_eq!(waiting[0]["intent"], json!("turn_end"));
    assert!(
        waiting[0]["created_at"].is_string(),
        "the stamp the RPC carried: {q:?}"
    );

    assert_eq!(failed[0]["message_id"], json!(2));
    assert_eq!(
        failed[0]["text"], json!("the failed one"),
        "**VERBATIM.** The re-select surface exists so what he said survives a \
         ladder running out; a cell that re-typed it would defeat the point"
    );
    drop(tmp);
}

/// **`metadata` AND `headers` ARE ON THE STORED ROW AND ARE NOT ON THE WIRE.**
///
/// A cell is permission to read what is in it. `QueueRow::metadata` is the
/// caller's opaque blob whose single declared path to the model lives in
/// `crate::context` (DEC-55 category (a)) and `headers` is post-validation
/// render state; neither RPC exposed either, and declaring them here would
/// publish both.
///
/// **THE MUTATION.** `QueueEntry` with a `metadata: Value` field and
/// `thread_queue` filling it. Red on the key being present.
#[tokio::test]
async fn the_queue_cell_publishes_neither_metadata_nor_headers() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());

    // A row WITH both, so their absence on the wire is a projection and not an
    // accident of an empty fixture.
    m.store
        .enqueue(
            &id,
            1,
            "carries a blob",
            "turn_end",
            &json!({"wav_path": "/captures/a.wav"}),
            &[],
        )
        .unwrap();

    let mut f = watch(&m, 12, &id).await;
    let row = last_export(&drain(&mut f), &id);
    let e = &row["queue"][0];

    assert_eq!(e["text"], json!("carries a blob"), "the fixture landed: {row}");
    for hidden in ["metadata", "metadata_json", "headers", "headers_json"] {
        assert!(
            e.get(hidden).is_none(),
            "`{hidden}` is on the stored row and may not be on the wire: {e}"
        );
    }
    // And the entry carries EXACTLY the five declared fields, so a sixth cannot
    // arrive unnoticed.
    let mut keys: Vec<&String> = e.as_object().expect("an entry object").keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["created_at", "intent", "message_id", "state", "text"],
        "the declared field set, and nothing beside it: {e}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. THE EQUIVALENCE THAT LICENSES DELETING THE TWO METHODS
// ─────────────────────────────────────────────────────────────────────────────

/// **THE CELL ANSWERS WHAT THE TWO RPCs ANSWERED.**
///
/// Asserted against the SAME two store reads the two method arms make
/// (`server.rs`' `"threads/{thread_id}/queue"` calls `waiting_queue`, and
/// `"threads/{thread_id}/queue_failed"` calls `failed_deliveries`), so this is
/// the migration claim and not a restatement of the fixture: whatever those arms
/// would have put on the wire is in this cell, with the same ids, the same text
/// and the same order.
///
/// **THE MUTATION.** `thread_queue` reading `waiting_queue` twice instead of
/// `waiting_queue` then `failed_deliveries`. Red on the failed id set.
#[tokio::test]
async fn the_cell_answers_what_both_methods_answered() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());

    enqueue(&m, &id, 1, "first waiting");
    enqueue(&m, &id, 2, "second waiting");
    fail(&m, &id, 3, "a failed delivery");

    let mut f = watch(&m, 13, &id).await;
    let row = last_export(&drain(&mut f), &id);
    let q = row["queue"].as_array().unwrap();

    // What `threads/{id}/queue` would have answered.
    let want_waiting: Vec<(u64, String)> = m
        .store
        .waiting_queue(&id)
        .unwrap()
        .into_iter()
        .map(|r| (r.message_id, r.text))
        .collect();
    // What `threads/{id}/queue_failed` would have answered.
    let want_failed: Vec<(u64, String)> = m
        .store
        .failed_deliveries(&id)
        .unwrap()
        .into_iter()
        .map(|r| (r.message_id, r.text))
        .collect();

    assert!(
        !want_waiting.is_empty() && !want_failed.is_empty(),
        "POSITIVE SENTINEL: both halves must be non-empty or this test proves \
         nothing — waiting={want_waiting:?} failed={want_failed:?}"
    );

    let got = |state: &str| -> Vec<(u64, String)> {
        q.iter()
            .filter(|e| e["state"] == json!(state))
            .map(|e| {
                (
                    e["message_id"].as_u64().unwrap(),
                    e["text"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };

    assert_eq!(
        got("waiting"), want_waiting,
        "the cell's waiting half must equal `waiting_queue`, in order"
    );
    assert_eq!(
        got("failed"), want_failed,
        "the cell's failed half must equal `failed_deliveries`, in order"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. IT IS A LEVEL, AND IT MOVES
// ─────────────────────────────────────────────────────────────────────────────

/// **CANCELLING A WAITING ROW TAKES IT OUT OF THE CELL, LIVE.**
///
/// The cell is a `Register<Vec<_>>` and not a `Sequence` because the queue is a
/// LEVEL: a row leaves it by being delivered, cancelled or re-selected, none of
/// which is an append. A `Sequence` would retain the cancelled row and make
/// *what is waiting* unanswerable from the cell that claims to hold it.
///
/// This also proves the emit site. `Fact::ThreadQueue` rides `emit_status`
/// rather than sitting at each of the six writes that move the queue — see
/// `ThreadManager::emit_thread_queue` for why that inherits an invariant the
/// `queued` cell already depends on — and `cancel_queued` is one of those six.
///
/// **THE MUTATION.** `emit_status`' `self.emit_thread_queue(at, thread_id)` line
/// deleted. The hydrate still fills the cell, so the subscribe is unaffected and
/// the test goes red only on the LIVE update — which is the property at issue.
#[tokio::test]
async fn cancelling_a_waiting_row_removes_it_from_the_cell_live() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());

    enqueue(&m, &id, 1, "keep me");
    enqueue(&m, &id, 2, "cancel me");

    let mut f = watch(&m, 14, &id).await;
    let before = last_export(&drain(&mut f), &id);
    assert_eq!(
        before["queue"].as_array().map(Vec::len),
        Some(2),
        "POSITIVE SENTINEL: both rows must be in the cell before the cancel, or \
         the assertion below passes for the wrong reason: {before}"
    );

    assert!(
        m.cancel_queued(&id, 2).unwrap().0,
        "the fixture must actually cancel a row"
    );
    settle().await;

    let after = queue_now(&drain(&mut f), &id);
    let q = after
        .as_array()
        .unwrap_or_else(|| panic!("no live `queue` reached the consumer after the cancel: {after}"));
    assert_eq!(q.len(), 1, "the cancelled row must LEAVE the level: {after}");
    assert_eq!(q[0]["message_id"], json!(1));
    assert_eq!(q[0]["text"], json!("keep me"));
    drop(tmp);
}

/// **A RE-SELECT MOVES A ROW FROM `failed` TO `waiting` — ONE ROW, ONE CELL,
/// ONE WORD CHANGED.**
///
/// The two halves being one cell is what makes "send that again" observable as a
/// `state` word moving, rather than as a row vanishing from one list and an
/// unrelated row appearing in another.
///
/// **AND THE FACT CARRIES THE WHOLE UNDELIVERED SET, NOT A DELTA** — which is
/// what makes the move observable at all. The emit here is triggered by an
/// UNRELATED row being cancelled, and the re-selected row's new `state` arrives
/// with it, because `Fact::ThreadQueue` re-reads the level rather than
/// describing one row's change.
///
/// **WHY THE EMIT IS AN UNRELATED ACT.** `queue_reselect`'s own public path is
/// `nudge_thread`, which re-enters `maybe_dispatch` BY DESIGN — a re-selected
/// message goes out exactly as a fresh one would — so the row is delivered
/// almost at once and an assertion on `waiting` would race the dispatcher.
/// (Written that way first, it failed `left: 0`: the queue was empty because the
/// message had already gone out, which is correct behaviour and a wrong test.)
/// A fresh SUBSCRIBE does not help either: `hydrates_key` runs once per KEY, not
/// per connection, so a second connection is served the root's cached cell.
/// (Written that way second, it failed `left: "failed"`.) Both dead ends are
/// recorded because each looks like the obvious test.
///
/// **THE MUTATION.** `QueueEntry::state` filled from a literal `"waiting"`
/// instead of `q.state`. The first sentinel then reads `waiting` and fires
/// before the move is ever made.
#[tokio::test]
async fn reselecting_a_failed_row_moves_its_state_word_in_the_cell() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());

    fail(&m, &id, 7, "that didn't get through");
    enqueue(&m, &id, 8, "an unrelated waiting row");

    let mut f = watch(&m, 15, &id).await;
    let before = last_export(&drain(&mut f), &id);
    let qb = before["queue"].as_array().expect("a list");
    assert_eq!(qb.len(), 2, "POSITIVE SENTINEL: both rows before: {before}");
    assert_eq!(
        qb.iter().find(|e| e["message_id"] == json!(7)).unwrap()["state"],
        json!("failed"),
        "POSITIVE SENTINEL: row 7 must START failed, or the move below is \
         unobservable and this test proves nothing: {before}"
    );

    assert_eq!(
        m.store.reselect_failed(&id, &[7]).unwrap(),
        1,
        "the fixture must actually re-select a row"
    );
    // The UNRELATED act that carries the emit. `cancel_queued` is one of the six
    // writes that reach `emit_status`, and it does not dispatch.
    assert!(m.cancel_queued(&id, 8).unwrap().0, "the cancel must land");
    settle().await;

    let after = queue_now(&drain(&mut f), &id);
    let q = after
        .as_array()
        .unwrap_or_else(|| panic!("no live `queue` after the cancel: {after}"));

    assert_eq!(
        q.len(), 1,
        "the cancelled row left and the re-selected one stayed — ONE row, and \
         the whole level was re-read: {after}"
    );
    assert_eq!(q[0]["message_id"], json!(7), "the same row, by id: {after}");
    assert_eq!(
        q[0]["state"], json!("waiting"),
        "**THE MOVE.** Row 7 was `failed` at the subscribe and is `waiting` now, \
         and nothing about row 7 triggered this frame: {after}"
    );
    assert_eq!(
        q[0]["text"], json!("that didn't get through"),
        "verbatim across the move — `reselect_failed` leaves `text` untouched, \
         which is the whole reason the re-select surface exists"
    );
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
