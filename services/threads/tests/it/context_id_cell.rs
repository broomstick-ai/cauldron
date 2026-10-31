//! # `threads.context_id` — the carrier for the read `threads/{id}/get` never answered
//!
//! R2.S3d, item A. The harness's `mind-index-v5` keys its delta ledger on a
//! thread's CURRENT context id and asked `threads/{id}/get` for it — which never
//! carried one, so the tool served full content every time. The id is
//! `<thread_id>.<last_compaction_message_id>` and the producer already derives it
//! once (`threads::context::context_id`) for the prompt's `context:` line. This
//! cell publishes that derivation on the `threads` shape.
//!
//! Three properties, each forced red by a named mutation of
//! `shapes/threads.rs`'s `context_id_of` (recorded in each docstring):
//!
//! 1. the cold subscriber reads it off the HYDRATE;
//! 2. a compaction moves it on the LIVE fold;
//! 3. it is the CONTEXT id and not the message id: the fixture holds a
//!    `last_message_id` that differs from the compaction boundary, so a cell
//!    that published either message id instead is red.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use threads::config::Config;
use threads::event::Fact;
use threads::shape::{ConnFrames, Derived, Hub, Op, Shape};
use threads::shapes::threads as tshape;
use threads::{build_manager, ThreadManager, ThreadSettings};

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

/// The newest row this connection holds for `key` — the export, or a later live row.
fn newest_row(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(key))
        .filter_map(|f| match (&f.wire.op, &f.wire.event, &f.wire.row) {
            (Op::Live, Some(Derived::Row { row }), _) => Some(row.clone()),
            (Op::Read, _, Some(row)) => Some(row.clone()),
            _ => None,
        })
        .last()
        .unwrap_or_else(|| panic!("no row for `{key}` among {} frames", frames.len()))
}

/// The export's per-cell provenance for `key`.
fn export_cells(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(key) && matches!(f.wire.op, Op::Read))
        .filter_map(|f| f.wire.cells.clone())
        .last()
        .unwrap_or_else(|| panic!("no export carried a cells block for `{key}`"))
}

/// Give the thread an addressable message so its `last_message_id` is a real,
/// DIFFERENT number from the compaction boundary the test sets.
fn one_message(m: &ThreadManager, id: &str) -> u64 {
    let (mid, _) = m
        .append_custom(id, "note".into(), json!({"n": 1}), true)
        .unwrap();
    mid.expect("an addressable event allocates a message id")
}

/// **THE COLD SUBSCRIBER READS THE CONTEXT ID OFF THE EXPORT.**
///
/// The boundary is written to the store BEFORE anything subscribes, so the
/// only path that can put it on the row is the hydrate.
///
/// **MUTATIONS.** (a) `context_id_of` returns a constant `"ctx"`: red, the
/// export says `ctx` where it must say `<id>.42`. (b) `context_id_of` returns
/// `last_compaction_message_id.to_string()` (the bare message id): red, `42`
/// is not `<id>.42`.
#[tokio::test]
async fn the_export_carries_the_context_id_the_boundary_names() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());
    let last = one_message(&m, &id);
    m.store.update_thread_compaction(&id, 42, false).unwrap();
    assert_ne!(last, 42, "the fixture needs two distinct ids or (b) cannot be red");

    let mut f = m.hub.connect(31);
    m.hub
        .subscribe(tshape::Threads::NAME, Some(&id), 31, None)
        .expect("a keyed subscribe to threads");
    settle().await;
    let frames = drain(&mut f);
    let row = newest_row(&frames, &id);

    assert_eq!(
        row["context_id"],
        json!(format!("{id}.42")),
        "the context id is `<thread>.<last_compaction_message_id>`: {row}"
    );
    assert_eq!(
        row["last_message_id"],
        json!(last),
        "the message id rides its own cell and is not the context id: {row}"
    );
    assert_eq!(
        row["context_id"],
        json!(threads::context::context_id(&id, 42)),
        "ONE derivation: the cell and the prompt's `context:` line agree"
    );
    let cells = export_cells(&frames, &id);
    assert_eq!(
        cells["context_id"], cells["compaction"],
        "one register behind both, so one provenance: {cells}"
    );
    drop(tmp);
}

/// **A COMPACTION MOVES IT, LIVE.**
///
/// Subscribed at the first context (`<id>.0` — a fresh thread's boundary is 0),
/// then the producer's own fact for a compaction that ended at 97. The live row
/// must carry `<id>.97`.
///
/// **MUTATIONS.** (a) the constant `"ctx"`: red on the first assert (and on
/// the second). (b) `context_id_of` publishes `<id>.<last_message_id>`, the
/// message id instead of the boundary: red, the fixture's `last_message_id` is
/// not 97 — the two-values property the brief asks the fixture to hold.
#[tokio::test]
async fn a_compaction_moves_the_context_id_on_the_live_fold() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());
    let last = one_message(&m, &id);

    let mut f = m.hub.connect(32);
    m.hub
        .subscribe(tshape::Threads::NAME, Some(&id), 32, None)
        .expect("a keyed subscribe to threads");
    settle().await;
    let before = newest_row(&drain(&mut f), &id);
    assert_eq!(
        before["context_id"],
        json!(format!("{id}.0")),
        "the first context, from thread initialisation: {before}"
    );

    m.store.update_thread_compaction(&id, 97, true).unwrap();
    let stored = m.store.get_thread(&id).unwrap().expect("the thread row");
    m.hub.emit_fact(Fact::ThreadCompaction {
        thread_id: id.clone(),
        row: tshape::compaction_of(&stored, Some([1, 97])),
    });
    settle().await;
    let after = newest_row(&drain(&mut f), &id);

    assert_ne!(last, 97, "the fixture needs two distinct ids or (b) cannot be red");
    assert_eq!(
        after["context_id"],
        json!(format!("{id}.97")),
        "a compaction is a new context, and the row says which: {after}"
    );
    assert_eq!(after["compaction"]["last_compaction_message_id"], json!(97));
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
