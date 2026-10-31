//! # The three thread shapes, proved
//!
//! `threads` (general, `PerKey`), `threads/summary` (application-specific,
//! nested, `Whole`) and `narration` (the daemon talking, `Whole`).
//!
//! **Every test here was forced red by a named mutation before it was believed,
//! and the mutation is in the test's own docstring.** The mutations were applied
//! by a script that also checks the compiler output for `error[E` before it
//! believes a red, because this codebase has shipped a "forced red" that was
//! really a compile error the mutation introduced.
//!
//! Almost every test drives the HUB directly rather than a websocket — `nesting.rs`'
//! style. That is deliberate: a shape's contract is a frame, the hub is where
//! frames are made, and a test that goes through a socket and a timer is a test
//! that can pass or fail on scheduling. The two exceptions say why they are
//! exceptions in their own docstrings: the `settings` byte-shape test must compare
//! against what `threads.get` ACTUALLY puts on the wire, and the write-site tests
//! must run a real (fake-claude) turn because nothing else reaches those sites.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use threads::config::Config;
use threads::event::Push;
use threads::shape::{ConnFrames, Derived, Hub, Op, ShapeError};
use threads::shapes::{narration, summary, threads as tshape};
use threads::{build_manager, ThreadManager, ThreadSettings};

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

fn set_env(tmp: &std::path::Path) {
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
}

/// A manager with the three shapes registered — **parents before children**,
/// which `Hub::register` enforces with a panic.
async fn start() -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "first".to_string(),
        tmp.path().join("first").to_string_lossy().to_string(),
    );
    // THE CLOCK SEAM. Every `Hub::emit` stamps with the clock and every cell
    // refuses an input at or before its `folded_at`, so two emits in one
    // microsecond mean the second is DROPPED — which is what an optimized
    // build does to a tight emit loop. See `src/clock.rs`.
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
    hub.register(summary::Summary, manager.clone());
    hub.register(narration::Narration, manager.clone());
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

/// Every frame buffered for this connection.
fn drain(f: &mut ConnFrames) -> Vec<Arc<threads::shape::ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

/// The LAST export for `key` — a re-export supersedes an earlier one, so the
/// newest is the one a mirror would be holding.
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

fn export_cells(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    frames
        .iter()
        .filter(|f| f.wire.op == Op::Read && f.wire.key.as_deref() == Some(key))
        .last()
        .expect("an export")
        .wire
        .cells
        .clone()
        .unwrap_or(Value::Null)
}

fn export_shared(frames: &[Arc<threads::shape::ShapeFrame>]) -> Value {
    frames
        .iter()
        .filter(|f| f.wire.op == Op::Read)
        .last()
        .expect("an export")
        .wire
        .shared
        .clone()
        .unwrap_or(Value::Null)
}

/// The last `row` a LIVE frame carried for `key`, which is what a mirror holds
/// after applying everything since the export.
fn last_live_row(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Value {
    frames
        .iter()
        .filter(|f| f.wire.op == Op::Live && f.wire.key.as_deref() == Some(key))
        .filter_map(|f| match &f.wire.event {
            Some(Derived::Row { row }) => Some(row.clone()),
            _ => None,
        })
        .last()
        .unwrap_or_else(|| panic!("no live row for `{key}` among {} frames", frames.len()))
}

fn transitions(frames: &[Arc<threads::shape::ShapeFrame>], cell: &str) -> Vec<Value> {
    frames
        .iter()
        .filter_map(|f| match &f.wire.event {
            Some(Derived::Transition { cell: c, item }) if c == cell => Some(item.clone()),
            _ => None,
        })
        .collect()
}

/// Subscribe to one thread and return everything that came back.
async fn watch(
    m: &ThreadManager,
    conn: u64,
    id: &str,
) -> (ConnFrames, Vec<Arc<threads::shape::ShapeFrame>>) {
    let mut f = m.hub.connect(conn);
    m.hub.subscribe("threads", Some(id), conn, None).unwrap();
    settle().await;
    let frames = drain(&mut f);
    (f, frames)
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. THE KEY SCOPE — what a keyless subscribe means here
// ─────────────────────────────────────────────────────────────────────────────

/// **A KEYLESS SUBSCRIBE TO `threads` IS REFUSED ON THE REQUEST ID.**
///
/// The whole reason this shape is `KeyScope::OnDemand`: there are 5,417 threads
/// in the production store and no delete path anywhere in the crate, so "every
/// key I know about" is an inventory nobody can afford and the honest answer is
/// to make the consumer name the thread it wants. Acking `{queued: true}` and
/// then exporting nothing is the failure class the refusal exists to close.
///
/// **THE MUTATION.** `Threads::HYDRATE = Hydrate::Whole`. The subscribe is then
/// accepted, and the assertion below fires on an `Ok`.
///
/// Forced red:
/// ```text
/// a shape whose keyless subscribe would be thousands of exports must REFUSE
/// the request, not ack it: Ok(())
/// ```
#[tokio::test]
async fn a_keyless_subscribe_to_the_threads_shape_is_refused_on_the_request_id() {
    let (m, tmp) = start().await;
    let mut f = m.hub.connect(7);
    match m.hub.subscribe("threads", None, 7, None) {
        Err(ShapeError::KeyRequired { shape, max_rows }) => {
            assert_eq!(shape, "threads");
            assert_eq!(
                max_rows, 500,
                "rule 2.8: the refusal NAMES the ceiling, so a consumer learns \
                 what to ask instead"
            );
        }
        other => panic!(
            "a shape whose keyless subscribe would be thousands of exports must REFUSE \
             the request, not ack it: {other:?}"
        ),
    }
    settle().await;
    assert!(
        drain(&mut f).is_empty(),
        "a refused subscribe creates no subscription, so no frame follows it"
    );
    assert!(
        !m.hub.tracked("threads"),
        "and a refused subscribe must not have started the fold — the whole point \
         of `Lazy` on the daemon's highest-volume input"
    );
    drop(tmp);
}

/// **THE SHARED BLOCK SAYS WHAT THE KEY SET IS, AND WHICH VERSION SERVES IT.**
///
/// Under `OnDemand` the keys are neither the fleet nor the live subset, and a
/// consumer must not have to infer that from an absent value — `machine_login.
/// unhosted`'s lesson. `fleet_size` is how it tells "I hold one key" from "there
/// is one thread"; `limits` is the daemon's own constants so nobody hardcodes
/// them; `shape_version` is rule 2.6's number, declared where a consumer can see
/// it.
///
/// **THE MUTATION.** Delete the `root.set_shared_constants(as_of)` call from
/// `Threads::hydrates`.
///
/// Forced red:
/// ```text
/// a consumer must be able to read what a key MEANS off the export: Null
/// ```
#[tokio::test]
async fn the_shared_block_says_what_the_key_set_is_and_which_version_serves_it() {
    let (m, tmp) = start().await;
    let a = thread_on(&m, tmp.path());
    let b = thread_on(&m, tmp.path());
    let (_f, frames) = watch(&m, 8, &a).await;
    let shared = export_shared(&frames);

    assert_eq!(
        shared["key_scope"]["mode"],
        json!("per_key"),
        "a consumer must be able to read what a key MEANS off the export: {shared}"
    );
    assert_eq!(
        shared["key_scope"]["includes_idle"],
        json!(true),
        "a per-key key is hydrated because somebody named it, whatever state \
         the thread is in"
    );
    // **TWO ASKS, TWO ANSWERS.** This was one word, `"refused"`, and it stopped
    // being true the moment `detail: keys` was served: a keyless subscribe is
    // refused for rows and ANSWERED for membership.
    assert_eq!(shared["key_scope"]["keyless_rows"], json!("refused"));
    assert_eq!(shared["key_scope"]["keyless_keys"], json!("membership"));
    assert_eq!(
        shared["fleet_size"],
        json!(2),
        "two threads exist and this connection holds one key: {shared}"
    );
    assert_eq!(
        shared["limits"]["query_max_limit"],
        json!(500),
        "the daemon's own constant, so no consumer hardcodes it"
    );
    assert_eq!(shared["shape_version"], json!(1));
    let _ = b;
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. THE HYDRATE — every cell, from the store, for one named thread
// ─────────────────────────────────────────────────────────────────────────────

/// **NAMING A THREAD HYDRATES EVERY CELL OF IT.**
///
/// Twelve cells, and the test asserts the VALUE of each one rather than its
/// presence, because a cell that hydrates to nothing is exactly the defect the
/// three-valued state exists to expose: `accounts` shipped "4 accounts rendered
/// as 0" behind a `Complete(vec![])`.
///
/// **THE MUTATION.** Make `Handles<ThreadRowRead>::hydrate` answer
/// `Hydration::Empty { reason: "x" }` — a handler that lies about a row it could
/// perfectly well have read.
///
/// Forced red, re-measured 2026-09-26:
/// ```text
/// assertion `left == right` failed
///   left: Null
///  right: String("fable")
/// ```
///
/// **The parenthetical that used to follow this — "the refusal path mints nothing,
/// so there is no row to assert on at all" — was false, and the recorded red said
/// `no export for `<id>` among 0 frames`, which this mutation cannot produce.** A
/// refused `hydrates_key` makes the chassis export a ROW-LESS frame on purpose —
/// *"so the consumer is told, rather than nothing"* — and the sibling test
/// `naming_a_thread_that_does_not_exist_mints_nothing_and_still_answers` asserts
/// exactly that. So the export arrives, `last_export` finds it and returns
/// `Value::Null` for its absent row, and the test dies on the first cell it reads
/// rather than on a frame count. The contract the old note appealed to is the OPPOSITE of the one the
/// chassis holds.
#[tokio::test]
async fn naming_a_thread_hydrates_every_cell_of_it() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());
    // An addressable application event, so `last_message_id` and `unread` have
    // something real to hold, and the `app` cell has a payload.
    m.append_custom(
        &id,
        "summary_updated".into(),
        json!({"idx": 3, "summary": "the garbage-post CLI"}),
        true,
    )
    .unwrap();
    m.mark_seen(&id, 1).unwrap();

    let (_f, frames) = watch(&m, 9, &id).await;
    let row = last_export(&frames, &id);
    let cells = export_cells(&frames, &id);

    // ── the header, the blob, the placement ──────────────────────────────
    assert_eq!(row["header"]["model"], json!("fable"));
    assert_eq!(
        row["header"]["cwd"],
        json!(tmp.path().to_string_lossy().to_string())
    );
    assert!(
        row["header"]["created_at"].is_string() && row["header"]["updated_at"].is_string(),
        "the header carries the stamps and nothing derived from them: {row}"
    );
    assert_eq!(row["settings"]["model"], json!("fable"));
    assert_eq!(row["account"], json!("first"));

    // ── the derived state ────────────────────────────────────────────────
    assert_eq!(row["status"]["state"], json!("idle"));
    assert_eq!(row["status"]["executing"], json!(false));
    assert_eq!(row["queued"], json!(0));
    assert_eq!(
        row["queue"], json!([]),
        "an empty queue is a KNOWN empty list, never `null` and never `?` — \
         `Hydration::Empty` is a statement. `queue_holds_both_states_...` in \
         `thread_queue_cell.rs` is where its contents are proved: {row}"
    );

    // ── the positions, and the count that is not their difference ────────
    assert_eq!(
        row["newest"]["seq"], json!(2),
        "`thread_started` is seq 1 and the custom event is seq 2, read out of the \
         LEDGER rather than the cached column: {row}"
    );
    assert_eq!(row["newest"]["kind"], json!("custom"));
    assert_eq!(row["last_message_id"], json!(1));
    assert_eq!(row["seen_seq"], json!(1));
    assert_eq!(
        row["unread"], json!(1),
        "one addressable event sits above the seen cursor. `newest.seq - seen_seq` \
         would say 1 here by luck; the next test is where the two part company"
    );

    // ── the compaction marker, the beat, the app payload ─────────────────
    assert_eq!(row["compaction"]["last_compaction_message_id"], json!(0));
    assert_eq!(row["compaction"]["needs_restate"], json!(false));
    assert_eq!(row["liveness"]["busy"], json!(false));
    assert_eq!(row["liveness"]["running_subagents"], json!([]));
    assert_eq!(
        row["app"]["summary_updated"]["summary"],
        json!("the garbage-post CLI"),
        "the application's payload, HELD OPAQUELY — stored verbatim and never read \
         by the general shape: {row}"
    );

    // ── and every one of them says `known`, not `?` ──────────────────────
    for cell in [
        "header",
        "settings",
        "account",
        "status",
        "queued",
        "queue",
        "last_message_id",
        "seen_seq",
        "unread",
        "newest",
        "compaction",
        "liveness",
        "app",
    ] {
        assert_eq!(
            cells[cell]["state"], json!("known"),
            "cell `{cell}` did not hydrate; a `?` a consumer cannot explain is \
             worse than a missing cell: {cells}"
        );
    }
    drop(tmp);
}

/// **A THREAD THAT DOES NOT EXIST MINTS NO KEY, AND THE CONSUMER IS STILL TOLD.**
///
/// The honest half of `OnDemand`: `hydrates_key` answers `false`, the chassis
/// warns and exports a row-less frame, and NOTHING is created — because a row of
/// `?`s for a thread that does not exist cannot be told apart from a thread whose
/// hydrate failed.
///
/// **THE MUTATION.** Make `Threads::hydrates_key` return `true` on the
/// `Empty` arm (i.e. delete the early `return false`), so a key is minted for a
/// thread that does not exist.
///
/// Forced red:
/// ```text
/// a thread that does not exist must not become a key: ["no-such-thread"]
/// ```
#[tokio::test]
async fn naming_a_thread_that_does_not_exist_mints_nothing_and_still_answers() {
    let (m, tmp) = start().await;
    let mut f = m.hub.connect(10);
    m.hub
        .subscribe("threads", Some("no-such-thread"), 10, None)
        .unwrap();
    settle().await;
    let frames = drain(&mut f);

    assert!(
        !frames.is_empty(),
        "an ack followed by silence is the class the refusal exists to close — the \
         consumer must get a frame saying there is nothing here"
    );
    let e = frames
        .iter()
        .find(|f| f.wire.op == Op::Read)
        .expect("an export");
    assert!(
        e.wire.row.is_none(),
        "and the frame must carry no row: {:?}",
        e.wire.row
    );
    let keys: Vec<String> = m
        .hub
        .list()
        .into_iter()
        .find(|s| s.name == "threads")
        .expect("the shape is registered")
        .keys;
    assert!(
        keys.is_empty(),
        "a thread that does not exist must not become a key: {keys:?}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. THE FOLD
// ─────────────────────────────────────────────────────────────────────────────

/// **THE FOLD MINTS NO KEY FOR A THREAD NOBODY NAMED.**
///
/// The bound that keeps an `OnDemand` root from becoming the whole fleet:
/// 2,463,981 ledger events exist, and if `Push::Event` for an unheld thread
/// created its key the root would grow to every thread in minutes. It is also
/// what makes `docs/shapes.md` §7.2's export-before-row gap unreachable in this
/// shape — a key exists only because a subscribe created it, and that subscribe's
/// export is what opened the connection's gate for it.
///
/// **THE MUTATION.** Delete the `if !holds(tx, &se.thread_id) { return; }` guard
/// from the `Push::Event` arm of `Threads::folds`.
///
/// Forced red:
/// ```text
/// an event for a thread nobody asked about must not create a key: ["<a>", "<b>"]
/// ```
#[tokio::test]
async fn the_fold_mints_no_key_for_a_thread_nobody_named() {
    let (m, tmp) = start().await;
    let watched = thread_on(&m, tmp.path());
    let (_f, _) = watch(&m, 11, &watched).await;

    // A second thread, busy, that this connection never named.
    let other = thread_on(&m, tmp.path());
    m.append_custom(&other, "failed_input".into(), json!({"text": "x"}), true)
        .unwrap();
    m.mark_seen(&other, 1).unwrap();
    settle().await;

    let keys: Vec<String> = m
        .hub
        .list()
        .into_iter()
        .find(|s| s.name == "threads")
        .unwrap()
        .keys;
    assert_eq!(
        keys,
        vec![watched.clone()],
        "an event for a thread nobody asked about must not create a key: {keys:?}"
    );
    drop(tmp);
}

/// **THE POSITIONS MOVE, AND THE UNREAD COUNT IS EXACT WHERE THEIR DIFFERENCE
/// IS NOT.**
///
/// This is the operator's question about `unread` answered as behaviour. Two
/// events arrive: one NON-addressable (no `message_id`) and one addressable. Both
/// move the log's head; only the second is unread. So after them
/// `newest.seq - seen_seq` is 2 and the true count is 1, and the cell carries 1.
///
/// **THE MUTATION.** In the `Push::Event` arm of `Threads::folds`, move the
/// `unread` increment out of the `if let Some(mid)` block so every event counts.
///
/// Forced red:
/// ```text
/// `newest.seq - seen_seq` OVER-COUNTS — only addressable events are unread.
/// seq 3, seen 1, unread 2
/// ```
#[tokio::test]
async fn the_unread_count_is_exact_where_the_position_difference_is_not() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());
    m.mark_seen(&id, 1).unwrap();
    let (mut f, _) = watch(&m, 12, &id).await;

    // seq 2: machinery. The application wrote it; nobody can be read it back.
    m.append_custom(&id, "transcript_ported".into(), json!({"n": 1}), false)
        .unwrap();
    // seq 3: addressable. THIS is unread.
    m.append_custom(&id, "summary_updated".into(), json!({"summary": "hi"}), true)
        .unwrap();
    settle().await;

    let frames = drain(&mut f);
    let row = last_live_row(&frames, &id);
    assert_eq!(row["newest"]["seq"], json!(3), "{row}");
    assert_eq!(row["seen_seq"], json!(1));
    assert_eq!(
        row["unread"],
        json!(1),
        "`newest.seq - seen_seq` OVER-COUNTS — only addressable events are unread. \
         seq {}, seen {}, unread {}",
        row["newest"]["seq"],
        row["seen_seq"],
        row["unread"]
    );
    assert_eq!(
        row["last_message_id"],
        json!(1),
        "the non-addressable event allocated no message id, so the addressable \
         position is the one the summary took: {row}"
    );
    drop(tmp);
}

/// **MARKING A THREAD SEEN MOVES THE POSITION AND RESETS THE COUNT.**
///
/// The third writer of the `unread` cell, and the one the fold cannot do alone: a
/// cursor JUMP swallows an unknown number of addressable events, so the write
/// site recomputes the count and carries it on `Fact::ThreadSeen`. `Push::Status`
/// cannot do this job — it carries `unread` but not `seen_seq`, and adding a field
/// to it would be changing an existing endpoint.
///
/// **THE MUTATION.** Delete the `self.hub.emit_fact(Fact::ThreadSeen {..})` block
/// from `ThreadManager::mark_seen`.
///
/// Forced red:
/// ```text
/// the seen cursor moved and the shape was never told: seen 1, unread 2
/// ```
#[tokio::test]
async fn marking_a_thread_seen_moves_the_position_and_resets_the_count() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());
    m.mark_seen(&id, 1).unwrap();
    let (mut f, _) = watch(&m, 13, &id).await;

    for i in 0..2 {
        m.append_custom(&id, "failed_input".into(), json!({"i": i}), true)
            .unwrap();
    }
    settle().await;
    let mid = last_live_row(&drain(&mut f), &id);
    assert_eq!(mid["unread"], json!(2), "two addressable events arrived");

    // The operator has now heard them.
    let head = m.store.max_seq(&id).unwrap();
    m.mark_seen(&id, head).unwrap();
    settle().await;
    let row = last_live_row(&drain(&mut f), &id);
    assert_eq!(
        (row["seen_seq"].clone(), row["unread"].clone()),
        (json!(head), json!(0)),
        "the seen cursor moved and the shape was never told: seen {}, unread {}",
        row["seen_seq"],
        row["unread"]
    );
    drop(tmp);
}

/// **A SETTINGS UPDATE MOVES THE BLOB AND THE HEADER — AND THE PATCH IS NOT THE
/// BLOB.**
///
/// The trap this cell exists to avoid: `ThreadEvent::SettingsChanged` carries a
/// PATCH, so a shape folding it would hold `{"model": "opus"}` as the whole of a
/// thread's settings. The fact carries the stored blob, and `updated_at`/`model`
/// move on the header at the same instant.
///
/// **THE MUTATION.** Delete the `self.emit_thread_facts(id)` call from
/// `ThreadManager::update_settings`.
///
/// Forced red, re-measured 2026-09-26:
/// ```text
/// the cell must hold the stored BLOB, not the patch: … "settings":{…"model":"fable"…}
///   left: String("fable")
///  right: String("opus")
/// ```
///
/// **NOT "no live row among 0 frames", which is what this said and which the
/// mutation cannot produce.** `update_settings` still emits
/// `ThreadEvent::SettingsChanged` on the line after the deleted one, and
/// `settings_changed` is not a `BULK_KIND`, so the `Push::Event` arm moves
/// `newest` and a live `row` DOES arrive — carrying the settings the fold last
/// knew about. The defect this pins is therefore a STALE cell on a live frame,
/// which is strictly worse than no frame and reads nothing like it.
#[tokio::test]
async fn a_settings_update_moves_the_blob_and_the_header() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());
    let (mut f, frames) = watch(&m, 14, &id).await;
    let before = last_export(&frames, &id);
    assert_eq!(before["settings"]["model"], json!("fable"));

    let mut patch = threads::settings::SettingsPatch::default();
    patch.model = Some("opus".to_string());
    m.update_settings(&id, patch).unwrap();
    settle().await;

    let row = last_live_row(&drain(&mut f), &id);
    assert_eq!(
        row["settings"]["model"],
        json!("opus"),
        "the cell must hold the stored BLOB, not the patch: {row}"
    );
    assert!(
        row["settings"]["cwd"].is_string(),
        "…which is why it is the blob and not the patch: a patch would have \
         replaced every other field with nothing: {row}"
    );
    assert_eq!(
        row["header"]["model"],
        json!("opus"),
        "the header moved in the same write: {row}"
    );
    drop(tmp);
}

/// **A STATUS EDGE MOVES THE LEVEL AND APPENDS A TRANSITION.**
///
/// `Edge`'s first instance on any producer. The level hydrates; the transitions
/// are `since_init` and the export says so. Two consumers need the two halves for
/// two reasons — a fleet view counts the level, a speaking interface narrates the
/// change — which is why this is an `Edge` and not a `Register`.
///
/// **THE MUTATION.** Change `row.status.emit(at, ..)` to `row.status.read(at, ..)`
/// in the `Push::Status` arm of `Threads::folds` — a hydrate set, which moves the
/// level and appends nothing.
///
/// Forced red:
/// ```text
/// a level that moved must leave a transition behind: []
/// ```
#[tokio::test]
async fn a_status_edge_moves_the_level_and_appends_a_transition() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());
    let (mut f, _) = watch(&m, 15, &id).await;

    // The frames the daemon itself sends at a lock take and a queue accept.
    for (state, executing, queued) in [("active", true, 0u64), ("idle", false, 0)] {
        m.hub.emit(Push::Status {
            thread_id: id.clone(),
            state: state.into(),
            executing,
            queued,
            unread: 99, // DELIBERATELY WRONG — see the assertion below.
            detail: None,
            account: Some("first".into()),
        });
    }
    settle().await;
    let frames = drain(&mut f);

    let row = last_live_row(&frames, &id);
    assert_eq!(row["status"]["state"], json!("idle"), "{row}");
    let moves = transitions(&frames, "status");
    assert_eq!(
        moves.len(),
        2,
        "a level that moved must leave a transition behind: {moves:?}"
    );
    // The hydrate READ the level as `idle`, so the first live emit is a real move
    // off it — which is also the proof that `Edge::read` sets a level without
    // inventing a transition for it.
    assert_eq!(moves[0]["from"]["state"], json!("idle"));
    assert_eq!(moves[0]["to"]["state"], json!("active"));
    assert_eq!(moves[0]["to"]["executing"], json!(true));
    assert_eq!(moves[1]["from"]["state"], json!("active"));
    assert_eq!(moves[1]["to"]["state"], json!("idle"));
    assert_eq!(
        row["unread"],
        json!(0),
        "`Status.unread` is the ONE field of that frame this shape ignores: its \
         stamp is taken before its five reads, so accepting it would let a stale \
         count overwrite a fresher increment. 99 must not appear: {row}"
    );
    drop(tmp);
}

/// **A BEAT THAT REPEATS THE HELD LEVEL PRODUCES NO FRAME.**
///
/// `Push::Liveness`' home is a cell on the thread it is about — the variant
/// `accounts` refuses as transport noise, given a home so the firehose is not the
/// only place it lives. `running_subagents` exists nowhere else in the daemon's
/// whole surface.
///
/// It is a LEVEL, so the fold drops a beat that says nothing new: without that
/// one comparison an open thread costs a `row` frame every second for as long as
/// it runs, which is precisely the "transport noise" objection made real.
///
/// **THE MUTATION.** Delete the
/// `if tx.rows.get(thread_id).is_some_and(|r| r.liveness.now() == Some(&next))`
/// guard from the `Push::Liveness` arm of `Threads::folds`, leaving the `emit`.
///
/// Forced red, re-measured 2026-09-26:
/// ```text
/// a 1 Hz beat that repeats the held level must cost nothing: 1 frames
/// ```
///
/// **Both halves of this block were wrong before that re-measurement, and the way
/// they were wrong is worth keeping.** It named the guard as
/// `if row.liveness.now() == Some(&next)` — a `row` binding that does not exist,
/// spelled the one way the source comment three lines above the real guard
/// explicitly rejects, because `Map::key` dirties on reach and a guard written
/// after `let row = tx.rows.key(..)` still costs the frame it exists to suppress.
/// A reader applying the named mutation would not have found the code to mutate.
/// And it recorded `4 frames`, which this test cannot produce: the three repeat
/// beats land inside one `settle()` and `row` frames coalesce per key behind
/// `DEBOUNCE_MS`, so the drain holds ONE. A recorded red nobody can reproduce is
/// not evidence, and it reads exactly like evidence.
#[tokio::test]
async fn a_beat_that_repeats_the_held_level_produces_no_frame() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());
    let (mut f, _) = watch(&m, 16, &id).await;

    let beat = |busy: bool, subs: Vec<String>| Push::Liveness {
        thread_id: id.clone(),
        busy,
        parent_running: busy,
        running_subagents: subs,
    };
    // One real change, then three beats that say exactly the same thing.
    m.hub.emit(beat(true, vec!["agent-1".into()]));
    settle().await;
    let first = drain(&mut f);
    let row = last_live_row(&first, &id);
    assert_eq!(row["liveness"]["busy"], json!(true));
    assert_eq!(
        row["liveness"]["running_subagents"],
        json!(["agent-1"]),
        "the only place in this daemon's surface where a subagent roster appears: {row}"
    );

    for _ in 0..3 {
        m.hub.emit(beat(true, vec!["agent-1".into()]));
    }
    settle().await;
    let repeats = drain(&mut f);
    assert_eq!(
        repeats.len(),
        0,
        "a 1 Hz beat that repeats the held level must cost nothing: {} frames",
        repeats.len()
    );

    // …and a beat that DOES say something new still gets through, so the
    // assertion above is a gate and not a silencer.
    m.hub.emit(beat(false, vec![]));
    settle().await;
    let after = drain(&mut f);
    assert_eq!(
        last_live_row(&after, &id)["liveness"]["busy"],
        json!(false),
        "the beat is still folded when it changes: {after:?}"
    );
    drop(tmp);
}

/// **THE `app` CELL HOLDS EVERY EVENT TYPE, OPAQUELY, AND THE GENERAL SHAPE
/// INTERPRETS NONE OF THEM.**
///
/// The operator's line, made testable. Four of the application's seven event
/// types arrive; all four are held, newest-per-type, byte-for-byte; and the
/// general shape's row has no `title` field anywhere in it, because a thread's
/// NAME is an application's judgement about one of these payloads and lives in
/// `threads/summary`.
///
/// **THE MUTATION.** Make the `app` arm skip `summary_updated` (as it would if
/// the general shape "knew" that type meant something).
///
/// Forced red:
/// ```text
/// every application event type is held, whatever it means: Null
/// ```
#[tokio::test]
async fn the_app_cell_holds_every_event_type_opaquely() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());
    let (mut f, _) = watch(&m, 17, &id).await;

    for (t, d) in [
        ("summary_updated", json!({"idx": 1, "summary": "first name"})),
        ("assistant_suggestions", json!({"items": ["a", "b"]})),
        ("failed_input", json!({"error_kind": "usage_limit"})),
        ("thread_compacted", json!({"trigger": "manual"})),
        // …and a SECOND name, which must supersede the first.
        ("summary_updated", json!({"idx": 2, "summary": "second name"})),
    ] {
        m.append_custom(&id, t.into(), d, false).unwrap();
    }
    settle().await;
    let row = last_live_row(&drain(&mut f), &id);

    assert_eq!(
        row["app"]["summary_updated"]["summary"],
        json!("second name"),
        "every application event type is held, whatever it means: {}",
        row["app"]["summary_updated"]
    );
    assert_eq!(
        row["app"]["assistant_suggestions"]["items"],
        json!(["a", "b"]),
        "held VERBATIM — the producer cannot classify the app's vocabulary: {row}"
    );
    assert_eq!(row["app"]["failed_input"]["error_kind"], json!("usage_limit"));
    assert_eq!(row["app"]["thread_compacted"]["trigger"], json!("manual"));
    assert!(
        row.get("title").is_none() && row.get("name").is_none(),
        "the GENERAL shape must not encode one application's meaning — a thread's \
         name is `threads/summary`'s judgement: {row}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. THE APPLICATION-SPECIFIC SHAPE
// ─────────────────────────────────────────────────────────────────────────────

/// **THE APPLICATION SHAPE DERIVES A THREAD'S NAME; THE GENERAL SHAPE DOES NOT.**
///
/// Both shapes see the same `summary_updated` event and do different things with
/// it: `threads` stores the payload and reads nothing out of it, `threads/summary`
/// says "this is the thread's title, and `idx` is the app's own coordinate". That
/// division IS the operator's ruling, and this test is where it is observable.
///
/// It also proves the NESTING: the child is reachable by name, its keys are bare
/// thread ids like its parent's, and `Epoch.parent_epoch` is on its frames so a
/// consumer holding both mirrors can tell whether they are from one era.
///
/// **THE MUTATION.** Delete the `if event_type != SUMMARY_EVENT_TYPE { return; }`
/// guard in `Summary::folds`, so the app shape titles a thread from whatever the
/// application last wrote.
///
/// Forced red:
/// ```text
/// only the app's NAMING event names a thread: "a suggestion, not a name"
/// ```
#[tokio::test]
async fn the_application_shape_derives_a_thread_name_and_the_general_shape_does_not() {
    let (m, tmp) = start().await;
    let id = thread_on(&m, tmp.path());
    m.append_custom(
        &id,
        "summary_updated".into(),
        json!({"idx": 7, "summary": "OAuth login, coordinator pattern"}),
        false,
    )
    .unwrap();

    // The CHILD, subscribed keylessly: its key set is the named threads, so a
    // keyless subscribe is the inventory a "list threads by name" view wants.
    let mut f = m.hub.connect(18);
    m.hub.subscribe("threads/summary", None, 18, None).unwrap();
    settle().await;
    let frames = drain(&mut f);
    let row = last_export(&frames, &id);
    assert_eq!(
        row["title"]["title"],
        json!("OAuth login, coordinator pattern"),
        "the newest `summary_updated` IS the thread's name — there is no title \
         column anywhere in `schema.sql`: {row}"
    );
    assert_eq!(row["title"]["idx"], json!(7));
    assert_eq!(row["title"]["seq"], json!(2));
    assert_eq!(export_shared(&frames)["event_type"], json!("summary_updated"));

    // NESTING is declared and on the wire.
    let child = m
        .hub
        .list()
        .into_iter()
        .find(|s| s.name == "threads/summary")
        .expect("the child is registered under its path-shaped name");
    assert_eq!(child.parent.as_deref(), Some("threads"));
    assert!(
        frames
            .iter()
            .any(|f| f.wire.epoch.parent_epoch.is_some()),
        "every frame a nested shape emits carries the parent's era: {frames:?}"
    );

    // LIVE: a new name supersedes, and a DIFFERENT app event does not name it.
    m.append_custom(
        &id,
        "assistant_suggestions".into(),
        json!({"summary": "a suggestion, not a name"}),
        false,
    )
    .unwrap();
    settle().await;
    let after = drain(&mut f);
    let held = after
        .iter()
        .filter_map(|f| match &f.wire.event {
            Some(Derived::Row { row }) => Some(row.clone()),
            _ => None,
        })
        .last()
        .map(|r| r["title"]["title"].clone())
        .unwrap_or(row["title"]["title"].clone());
    assert_eq!(
        held,
        json!("OAuth login, coordinator pattern"),
        "only the app's NAMING event names a thread: {held}"
    );
    drop(tmp);
}

/// **THE LIVE ARM OF THE SUMMARY FOLD, ASSERTED POSITIVELY** — the title that
/// arrives while a consumer is watching, not the one the hydrate read.
///
/// **THE GAP THIS FILLS, found by an adversarial pass and verified.** Both
/// existing tests of `threads/summary` append their `summary_updated` BEFORE the
/// subscribe, and the shape is `Tracking::Lazy`, so its FIFO is not even
/// accepting until then: every title they assert comes from `Summary::hydrates`
/// via `Handles<NamedThreads>`. The test above then asserts an ABSENCE — a
/// non-naming event does not rename — through an `unwrap_or` that falls back to
/// the hydrated value, so it holds with the fold's whole live arm deleted. The
/// emit, the seq-ordering guard and the key MINTING that is the shape's stated
/// reason for existing were uncovered by anything.
///
/// Three things, in one test because they are one arm:
///
/// 1. A name that arrives LIVE reaches the consumer as a `row`.
/// 2. An OUT-OF-ORDER name is discarded — ordered on `seq`, because a wall clock
///    can move backwards and a per-thread `seq` cannot.
/// 3. The fold MINTS a key: a thread that was not named at init becomes a key of
///    this shape without a re-hydrate.
///
/// **THE MUTATION.** `return` at the top of the `Input::Push(Push::Event(se))`
/// arm of `Summary::folds`, i.e. the whole live arm deleted.
///
/// Forced red, and it is the HELPER that panics rather than an assertion, because
/// with the arm gone there is no live frame to read a title out of at all:
/// ```text
/// no live row for `0891dc98-…` among 0 frames
/// ```
///
/// **Under that same mutation the test above stays GREEN**, which is the whole
/// reason this one exists. Recorded here rather than in a report, because the next
/// person to change this fold will read these two docstrings and not the report.
#[tokio::test]
async fn a_name_that_arrives_live_reaches_the_consumer_and_a_stale_one_does_not() {
    let (m, tmp) = start().await;
    // NAMED AT INIT, so the shape has a key to hydrate and the subscribe below is
    // not the empty-inventory case.
    let seeded = thread_on(&m, tmp.path());
    m.append_custom(
        &seeded,
        "summary_updated".into(),
        json!({"idx": 1, "summary": "seeded at init"}),
        false,
    )
    .unwrap();
    // NOT named at init: this one's key does not exist yet.
    let fresh = thread_on(&m, tmp.path());

    let mut f = m.hub.connect(21);
    m.hub.subscribe("threads/summary", None, 21, None).unwrap();
    settle().await;
    let at_init = drain(&mut f);
    assert!(
        at_init
            .iter()
            .all(|x| x.wire.key.as_deref() != Some(fresh.as_str())),
        "`{fresh}` has no name yet, so it must not be a key of this shape at init"
    );

    // ── 1. A NAME THAT ARRIVES LIVE ──────────────────────────────────────────
    m.append_custom(
        &fresh,
        "summary_updated".into(),
        json!({"idx": 4, "summary": "named while watched"}),
        false,
    )
    .unwrap();
    settle().await;
    let live = drain(&mut f);
    let row = last_live_row(&live, &fresh);
    assert_eq!(
        row["title"]["title"],
        json!("named while watched"),
        "a name that arrives while a consumer is watching must reach it live: \
         {row}"
    );
    assert_eq!(row["title"]["idx"], json!(4), "and it carries the app's own \
         coordinate, which is the half the general shape refuses to read: {row}");

    // ── 3. THE FOLD MINTED THE KEY ───────────────────────────────────────────
    let keys = m
        .hub
        .list()
        .into_iter()
        .find(|s| s.name == "threads/summary")
        .expect("the child shape")
        .keys;
    assert!(
        keys.contains(&fresh),
        "the fold MINTS keys — that is what makes this shape useful — and \
         `{fresh}` was not a key at init: {keys:?}"
    );

    // ── 2. AN OUT-OF-ORDER NAME IS DISCARDED ─────────────────────────────────
    //
    // Written to the store with a LOWER `seq` than the one now held, which is
    // what a replayed or reordered event looks like. The store stamps `seq`
    // monotonically, so the stale event is injected through the hub on the same
    // path the fold reads it from.
    let stale = threads::event::StoredEvent {
        thread_id: fresh.clone(),
        seq: 1,
        message_id: None,
        ts: "2020-01-01T00:00:00Z".to_string(),
        event: threads::event::ThreadEvent::Custom {
            event_type: "summary_updated".to_string(),
            data: json!({"idx": 99, "summary": "a replay from the past"}),
        },
    };
    m.hub.emit(Push::Event(stale));
    settle().await;
    let after_stale = drain(&mut f);
    let held = after_stale
        .iter()
        .filter(|x| x.wire.op == Op::Live && x.wire.key.as_deref() == Some(fresh.as_str()))
        .filter_map(|x| match &x.wire.event {
            Some(Derived::Row { row }) => Some(row["title"]["title"].clone()),
            _ => None,
        })
        .last()
        .unwrap_or_else(|| row["title"]["title"].clone());
    assert_eq!(
        held,
        json!("named while watched"),
        "a name with a lower `seq` than the one held is a replay and must be \
         DISCARDED, not applied: {held}"
    );
    drop(tmp);
}

/// **THE APPLICATION SHAPE'S READ IS BOUNDED, AND IT SAYS SO WHEN THE BOUND
/// BITES.**
///
/// The bound is `MAX_SUMMARY_ROWS` = 500 = `server::QUERY_MAX_LIMIT`, chosen for
/// the FRAME count of a keyless subscribe rather than for the SQL: one export per
/// key, against a per-connection channel of 1,024. This test names 501 threads —
/// one more than the cap — and asserts the read reports `Partial` with the number
/// missing, never a short `Complete`. A short read that claims to be whole is the
/// one failure types cannot see.
///
/// **THE MUTATION.** Change the `total > got.len()` branch in
/// `Handles<NamedThreads>` to `Hydration::Complete(got)`.
///
/// Forced red:
/// ```text
/// a read that hit its cap must say so: complete=true, rows=500 of 501
/// ```
#[tokio::test]
async fn the_application_shapes_read_is_bounded_and_says_so_when_the_bound_bites() {
    let (m, tmp) = start().await;
    let over = summary::MAX_SUMMARY_ROWS + 1;
    for i in 0..over {
        let id = thread_on(&m, tmp.path());
        m.append_custom(
            &id,
            "summary_updated".into(),
            json!({"idx": i, "summary": format!("thread {i}")}),
            false,
        )
        .unwrap();
    }
    // The store read's own contract first: the page AND the total, so the caller
    // can tell a full answer from a page of one.
    let (rows, total) = m
        .store
        .newest_app_events("summary_updated", summary::MAX_SUMMARY_ROWS)
        .unwrap();
    assert_eq!(total, over, "every named thread is counted");
    assert_eq!(rows.len(), summary::MAX_SUMMARY_ROWS, "and the page is capped");

    let mut f = m.hub.connect(19);
    m.hub.subscribe("threads/summary", None, 19, None).unwrap();
    settle().await;
    let frames = drain(&mut f);
    let exports: Vec<_> = frames.iter().filter(|f| f.wire.op == Op::Read).collect();
    assert_eq!(
        exports.len(),
        summary::MAX_SUMMARY_ROWS,
        "one export per key, capped — the bound is on FRAMES: {} exports",
        exports.len()
    );
    let cells = exports[0].wire.cells.clone().unwrap();
    let shared = exports[0].wire.shared.clone().unwrap();
    assert_eq!(
        cells["title"]["complete"],
        json!(false),
        "a read that hit its cap must say so: complete={}, rows={} of {total}",
        cells["title"]["complete"],
        exports.len()
    );
    assert_eq!(shared["truncated"]["truncated"], json!(true));
    assert!(
        shared["truncated"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains(&format!("{over} threads have a name")),
        "and it must say how many are missing, where a consumer deciding whether \
         to fall back to `threads.list` will look: {shared}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. NARRATION — the last thing keeping a consumer on the firehose
// ─────────────────────────────────────────────────────────────────────────────

/// **THE THREE CHANNELS CARRY THE THREE VARIANTS THAT HAD NO HOME.**
///
/// `Push::Progress`, `Push::Lifecycle` and `Push::Info` belong to no thread and
/// no account, no RPC returns them, and both other shapes decline them by name —
/// so before this shape a consumer could not stop subscribing to the legacy
/// firehose. Each is a `Sequence` append, delivered serial and un-coalesced
/// because narration is transitions by definition.
///
/// **THE MUTATION.** Delete the `Push::Info` arm's body in `Narration::folds`
/// (fold it into the ignored arm).
///
/// Forced red:
/// ```text
/// the `info` channel carried nothing: []
/// ```
#[tokio::test]
async fn narration_carries_the_three_variants_that_had_no_other_home() {
    let (m, tmp) = start().await;
    let mut f = m.hub.connect(20);
    m.hub.subscribe("narration", None, 20, None).unwrap();
    settle().await;
    let opening = drain(&mut f);
    assert_eq!(
        opening.iter().filter(|f| f.wire.op == Op::Read).count(),
        3,
        "a keyless subscribe is an inventory of the three channels: {opening:?}"
    );

    m.hub.emit(Push::Progress {
        op: "login".into(),
        id: "op-1".into(),
        account: Some("first".into()),
        phase: "browser".into(),
        detail: Some("waiting for the code".into()),
        done: false,
        ok: None,
    });
    m.hub.emit(Push::Lifecycle(threads::update::LifecycleNote {
        service: threads::update::SERVICE_ID.to_string(),
        phase: threads::update::PHASE_GOING_DOWN.to_string(),
        reason: threads::update::CAUSE_NEW_BINARY.to_string(),
        detail: "a newer build was absorbed".into(),
        boot_id: "boot-7".into(),
        at: "2026-09-26T00:00:00Z".into(),
    }));
    m.emit_info("accounts", "pool_reloaded", None, None);
    settle().await;
    let frames = drain(&mut f);

    let lines = |channel: &str| -> Vec<Value> {
        frames
            .iter()
            .filter(|f| f.wire.key.as_deref() == Some(channel))
            .filter_map(|f| match &f.wire.event {
                Some(Derived::Transition { cell, item }) if cell == "lines" => Some(item.clone()),
                _ => None,
            })
            .collect()
    };

    let progress = lines(narration::CHANNEL_PROGRESS);
    assert_eq!(progress.len(), 1, "the `progress` channel: {progress:?}");
    assert_eq!(progress[0]["kind"], json!("progress"));
    assert_eq!(progress[0]["op"], json!("login"));
    assert_eq!(progress[0]["detail"], json!("waiting for the code"));

    let lifecycle = lines(narration::CHANNEL_LIFECYCLE);
    assert_eq!(lifecycle.len(), 1, "the `lifecycle` channel: {lifecycle:?}");
    assert_eq!(lifecycle[0]["phase"], json!("going_down"));
    assert_eq!(
        lifecycle[0]["boot_id"],
        json!("boot-7"),
        "the boot a consumer correlates against its own welcome frame: {lifecycle:?}"
    );

    let info = lines(narration::CHANNEL_INFO);
    assert_eq!(info.len(), 1, "the `info` channel carried nothing: {info:?}");
    assert_eq!(info[0]["domain"], json!("accounts"));
    assert_eq!(info[0]["reason"], json!("pool_reloaded"));

    // The three channels are INDEPENDENT windows, which is why this shape is
    // keyed: a burst on one cannot evict another's news.
    let held = frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(narration::CHANNEL_INFO))
        .filter_map(|f| match &f.wire.event {
            Some(Derived::Row { row }) => Some(row.clone()),
            _ => None,
        })
        .last()
        .expect("a row for the info channel");
    assert_eq!(held["lines"].as_array().map(Vec::len), Some(1));
    drop(tmp);
}

/// **`Push::Liveness` IS NOT A NARRATION LINE, AND THE REASON IS MEASURABLE.**
///
/// The beat has a home — the thread's `liveness` cell — and this asserts it is not
/// ALSO here, because a 1 Hz beat in a 64-item window would evict the restart
/// notice this shape exists to carry. Ten beats, and the `lifecycle` line that
/// arrived first is still the only thing in its channel.
///
/// **THE MUTATION.** Add a `Push::Liveness` arm to `Narration::folds` that appends
/// to the `progress` channel.
///
/// Forced red:
/// ```text
/// a beat is not narration — it is a level on the thread it is about: 10
/// ```
#[tokio::test]
async fn a_beat_is_not_a_narration_line() {
    let (m, tmp) = start().await;
    let mut f = m.hub.connect(21);
    m.hub.subscribe("narration", None, 21, None).unwrap();
    settle().await;
    drain(&mut f);

    for i in 0..10 {
        m.hub.emit(Push::Liveness {
            thread_id: format!("t-{i}"),
            busy: true,
            parent_running: true,
            running_subagents: vec![],
        });
    }
    settle().await;
    let frames = drain(&mut f);
    assert_eq!(
        frames.len(),
        0,
        "a beat is not narration — it is a level on the thread it is about: {}",
        frames.len()
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. THE HYDRATE BOUND, IN STATEMENTS
// ─────────────────────────────────────────────────────────────────────────────
//
// `tests/thread_hydrate_cost.rs` is its own binary and holds it: `db_trips()` is
// process-global, so a statement count cannot share a binary with anything that
// queries. This file deliberately holds no statement-count test.

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
