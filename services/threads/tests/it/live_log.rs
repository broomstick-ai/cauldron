//! # `threads/log` — a thread's live event log, proved
//!
//! **Every test here was forced red by a named mutation, and the mutation plus the
//! red's actual text is in the test's own docstring.** The mutations were applied
//! and reverted by `scripts/force-red-live-log.sh`, which also refuses to believe
//! a red whose output contains `error[E` — a compile error is not a forced red, and
//! this codebase has shipped one before.
//!
//! **The forcings OPPOSE each other**, deliberately, so that no degenerate
//! implementation satisfies the suite:
//!
//! | mutation | reddens | stays green |
//! |---|---|---|
//! | `hydrates_key` always returns `false` | §1, §2 (headline) | §1b |
//! | `hydrates_key` always mints a row | §1b | §1 |
//! | the fold appends every event unconditionally | §6 | §3, §4 |
//! | the fold appends nothing (`seq` guard inverted) | §2, §3, §4 | §6 |
//! | `BULK_KINDS` return added to the fold | §3 | §7 |
//! | the `holds` scope guard deleted | §7 | §3 |
//! | the key cap never fires | §9 | §10 |
//! | the key cap fires on a re-subscribe | §10 | §9 |
//! | `MAX_INLINE_PAYLOAD` raised past every body | §5 | §4 |
//! | `MAX_INLINE_PAYLOAD` lowered to 0 | §4 | §5 |
//!
//! The suite drives the HUB directly, `thread_shapes.rs`' way: a shape's contract
//! is a frame, the hub is where frames are made, and a test that goes through a
//! socket and a timer can pass or fail on scheduling. The one exception — §8, the
//! resource door — says why in its own docstring.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use threads::config::Config;
use threads::event::{Push, StoredEvent, ThreadEvent};
use threads::shape::{Beat, ConnFrames, Derived, Detail, Hub, Op, Retain, ShapeError, ShapeFrame};
use threads::shapes::log;
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

/// A manager with the pool and the log registered — **parents before children**,
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
    hub.register(threads::shapes::threads::Threads, manager.clone());
    hub.register(log::Log, manager.clone());
    (manager, tmp)
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
}

fn a_thread(m: &ThreadManager, tmp: &std::path::Path) -> String {
    let mut s = ThreadSettings::new(tmp.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    m.create_thread(None, s).unwrap()
}

/// **EVERY THREAD ALREADY HAS ONE EVENT, AND THE FIXTURE MUST NOT PRETEND
/// OTHERWISE.**
///
/// `ThreadManager::create_thread` appends `thread_started` at `seq` 1. A fixture
/// that numbered its own events from 1 would collide with it, and the collision is
/// SILENT at both ends: `append_event` is `ON CONFLICT(thread_id, seq) DO NOTHING`,
/// so the store keeps `thread_started`, and the fold's `seq` guard refuses the push
/// as at-or-below the head. The test would then be green about an event that never
/// reached anything. So every window below starts with `seq: 1` — the creation — and
/// the fixture allocates from the store's own head.
const CREATED: u64 = 1;

/// **PERSIST, THEN BROADCAST — the daemon's own order, and the test's.**
///
/// `docs/shapes.md` §2.7: the real emit site appends to the store and then calls
/// `hub.emit`, which is why a hydrate at `as_of` cannot miss an event whose push is
/// still in flight. A fixture that only emitted would exercise the live fold
/// against a store that disagrees with it, and §2's whole claim is that the two
/// agree.
///
/// Answers the `seq` it allocated, so a test can replay it.
fn happened(m: &ThreadManager, mut ev: StoredEvent) -> u64 {
    ev.seq = m.store.max_seq(&ev.thread_id).unwrap() + 1;
    let seq = ev.seq;
    m.store.append_event(&ev).unwrap();
    m.hub.emit(Push::Event(ev));
    seq
}

/// The same event again, at the `seq` it already has — a replay, which the store
/// makes a no-op and which the fold must also refuse.
fn replayed(m: &ThreadManager, ev: StoredEvent) {
    m.store.append_event(&ev).unwrap();
    m.hub.emit(Push::Event(ev));
}

fn delta(id: &str, text: &str) -> StoredEvent {
    StoredEvent {
        thread_id: id.to_string(),
        seq: 0,
        message_id: None,
        ts: chrono::Utc::now().to_rfc3339(),
        event: ThreadEvent::StreamEvent {
            raw: json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": text}
            }),
        },
    }
}

fn said(id: &str, message_id: u64, text: &str) -> StoredEvent {
    StoredEvent {
        thread_id: id.to_string(),
        seq: 0,
        message_id: Some(message_id),
        ts: chrono::Utc::now().to_rfc3339(),
        event: ThreadEvent::UserMessage {
            text: text.to_string(),
            intent: Default::default(),
            metadata: json!({"from": "the test"}),
        },
    }
}

fn drain(f: &mut ConnFrames) -> Vec<Arc<ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

/// The LAST export for `key` — a re-export supersedes an earlier one, so the
/// newest is what a mirror holds.
fn last_export(frames: &[Arc<ShapeFrame>], key: &str) -> Option<Value> {
    frames
        .iter()
        .filter(|f| f.wire.op == Op::Read && f.wire.key.as_deref() == Some(key))
        .last()
        .map(|f| f.wire.row.clone().unwrap_or(Value::Null))
}

fn export_cells(frames: &[Arc<ShapeFrame>], key: &str) -> Value {
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

/// The `events` array an export carries.
fn window(frames: &[Arc<ShapeFrame>], key: &str) -> Vec<Value> {
    last_export(frames, key)
        .and_then(|r| r["events"].as_array().cloned())
        .unwrap_or_default()
}

/// **A MIRROR, BUILT THE WAY RULE 2.3 SAYS A CONSUMER BUILDS ONE** — the export's
/// retained items, then every `transition` after it appended in arrival order.
/// Nothing else: no cursor, no gap query, no re-emit.
fn mirror(frames: &[Arc<ShapeFrame>], key: &str) -> Vec<Value> {
    let mut held: Vec<Value> = Vec::new();
    for f in frames {
        if f.wire.key.as_deref() != Some(key) {
            continue;
        }
        match (&f.wire.op, &f.wire.event) {
            (Op::Read, _) => {
                held = f.wire.row.as_ref().and_then(|r| r["events"].as_array().cloned())
                    .unwrap_or_default();
            }
            (
                Op::Live,
                Some(Derived::Transition {
                    cell,
                    item,
                }),
            ) if cell == "events" => held.push(item.clone()),
            _ => {}
        }
    }
    held
}

fn seqs(rows: &[Value]) -> Vec<u64> {
    rows.iter().map(|r| r["seq"].as_u64().unwrap()).collect()
}

/// **A CONNECTION THAT ACCUMULATES ITS FRAMES.**
///
/// Not a bare `ConnFrames`, because a mirror is built from the export PLUS every
/// transition after it and a bare `try_recv` loop consumes the export the first
/// time anybody looks. That is not a theory: it is the mistake this type exists to
/// make unmakeable, and it produced an empty mirror that looked like a fold that
/// had stopped.
struct Watch {
    f: ConnFrames,
    seen: Vec<Arc<ShapeFrame>>,
}

impl Watch {
    async fn open(m: &ThreadManager, conn: u64, id: &str) -> Self {
        let mut w = Watch {
            f: m.hub.connect(conn),
            seen: Vec::new(),
        };
        m.hub.subscribe("threads/log", Some(id), conn, None).unwrap();
        w.pull().await;
        w
    }

    async fn pull(&mut self) -> &[Arc<ShapeFrame>] {
        settle().await;
        self.seen.extend(drain(&mut self.f));
        &self.seen
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. IT HYDRATES — which is the whole reason `Sequence::read` had to exist
// ─────────────────────────────────────────────────────────────────────────────

/// **A COLD SUBSCRIBER RECEIVES THE TRANSCRIPT IT MISSED, NOT AN EMPTY WINDOW.**
///
/// Four files in this repo recorded the same measured absence — *"`Sequence` has
/// no `read`, so a tail would be `since_init` from the instant of
/// subscription"* — and all four concluded a thread's log could not be a shape.
/// This is the sentence that stops being true.
///
/// `since_init: false` on the cell state is the structural half of it: a window
/// that says `since_init: true` is telling a consumer it covers only what arrived
/// after it subscribed, which is the thing being refuted.
///
/// **THE MUTATION.** `Log::hydrates_key` returns `false` without reading.
///
/// Forced red:
/// ```text
/// a cold subscriber must receive the window as it stands, not an empty cell:
/// []
/// ```
#[tokio::test]
async fn a_cold_subscriber_receives_the_window_it_missed() {
    let (m, tmp) = start().await;
    let id = a_thread(&m, tmp.path());
    // Three events BEFORE anybody subscribes: nothing is folded, the store holds
    // them, and a `Sequence` without a `read` would show none of them.
    happened(&m, said(&id, 1, "first"));
    happened(&m, delta(&id, "he"));
    happened(&m, delta(&id, "llo"));

    let mut w = Watch::open(&m, 1, &id).await;
    let frames = w.pull().await.to_vec();
    let rows = window(&frames, &id);
    assert_eq!(
        seqs(&rows),
        vec![CREATED, 2, 3, 4],
        "a cold subscriber must receive the window as it stands, not an empty cell: \
         {rows:?}"
    );
    let state = export_cells(&frames, &id)["events"].clone();
    assert_eq!(state["state"], "known");
    assert_eq!(
        state["since_init"],
        Value::Null,
        "`since_init` is skipped when false, and false is what a hydrated window \
         must say: {state}"
    );
    assert_eq!(
        state["complete"], true,
        "a thread shorter than the window is a COMPLETE answer: {state}"
    );
    drop(tmp);
}

/// **A THREAD WITH NO LOG GETS NO ROW — never a window of `?`s.**
///
/// The subject is an id that names no thread, because every thread this daemon
/// creates has a `thread_started` event and therefore a log. That is the honest
/// form of the question: the pool is what says whether a thread EXISTS, and this
/// shape says what is in its log — for a thread that does not exist and for a
/// thread with an empty log the answer is the same, which the shape's own
/// `hydrates_key` doc states rather than hides.
///
/// `crate::shapes::threads`' own rule, restated: *"nothing is minted on that
/// path — a row of `?`s for a thread that does not exist cannot be told apart
/// from a thread whose hydrate failed."* This is the OPPOSING forcing to the test
/// above: a `hydrates_key` that always refuses reddens that one, and a
/// `hydrates_key` that always mints reddens this one, so neither degenerate
/// answer survives the pair.
///
/// **THE MUTATION.** `Log::hydrates_key` mints the row before the emptiness
/// check:
/// `root.rows.key(id.clone()); if events.is_empty() { return false; }`
///
/// Forced red:
/// ```text
/// a thread with no events must produce NO row, so that an empty log and a
/// failed read stay apart: Some(Object {"events": Array [], "retained": Number(0),
/// "thread_id": String("...")})
/// ```
#[tokio::test]
async fn a_thread_with_no_log_produces_no_row_at_all() {
    let (m, tmp) = start().await;
    let id = "a-thread-that-was-never-created".to_string();
    let mut w = Watch::open(&m, 1, &id).await;
    let frames = w.pull().await.to_vec();
    assert_eq!(
        last_export(&frames, &id),
        Some(Value::Null),
        "a thread with no events must produce NO row, so that an empty log and a \
         failed read stay apart: {:?}",
        last_export(&frames, &id)
    );
    assert_eq!(
        window(&frames, &id),
        Vec::<Value>::new(),
        "and no window either — nothing is minted on that path"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. THE HEADLINE — rule 2.5, and the whole point of the shape
// ─────────────────────────────────────────────────────────────────────────────

/// **A CONSUMER THAT LOSES ITS CONNECTION AND RE-SUBSCRIBES ENDS WITH A MIRROR
/// IDENTICAL TO ONE THAT NEVER DISCONNECTED — with no cursor, no gap query and no
/// re-emit verb.**
///
/// Rule 2.5, and it is the reason this shape exists: today a consumer of a
/// thread's log keeps a `delivered_seq`, notices a gap, pages `history` from it
/// and decides when it has waited long enough. Every one of those is a
/// distributed-systems policy in a client, and this test is the statement that
/// none of them is needed.
///
/// **TWO PHASES, because there are two ways to lose a connection and they take
/// different paths through the producer.**
///
/// * **Phase 1 — the root SURVIVES.** Another consumer is still holding the key,
///   so a re-subscribe is a re-export of a window that kept folding. The
///   reconnecting mirror must equal the one that was there the whole time.
/// * **Phase 2 — the root is GONE.** `Tracking::Lazy` drops it on the last
///   unsubscribe, so a re-subscribe re-HYDRATES from the store. The cold mirror
///   must still equal the live one, which is the claim `Sequence::read` is for.
///
/// The mirrors are compared as JSON arrays, item for item, `at` stamps included.
/// That is why the item stamp is the event's own `ts` on both paths — see
/// `log::stamp_of`.
///
/// **THE MUTATION.** Drop one append: in `Log::folds`, `if se.seq % 3 == 0 {
/// return; }` before the append.
///
/// Forced red:
/// ```text
/// phase 1: a consumer that lost its connection and re-subscribed must hold
/// exactly what one that never disconnected holds — no cursor, no gap query, no
/// re-emit: left [1, 2, 4, 5] right [1, 2, 4, 5]
/// ```
/// — and before that, on the `never` side:
/// ```text
/// assertion `left == right` failed: the live mirror must hold every event that
/// happened
///   left: [1, 2, 4, 5]
///  right: [1, 2, 3, 4, 5]
/// ```
#[tokio::test]
async fn a_resubscribed_mirror_is_identical_to_one_that_never_disconnected() {
    let (m, tmp) = start().await;
    let id = a_thread(&m, tmp.path());

    // ── phase 1: the root survives, because `never` holds the key ────────────
    let mut never = Watch::open(&m, 1, &id).await;
    let flaky = Watch::open(&m, 2, &id).await;

    happened(&m, said(&id, 1, "hello"));
    happened(&m, delta(&id, "hi"));
    settle().await;

    // THE CONNECTION IS LOST. Not an unsubscribe the consumer chose — a drop.
    m.hub.dropped(2);
    drop(flaky);

    // Three more events happen while it is away. `never` sees them live.
    happened(&m, delta(&id, " th"));
    happened(&m, delta(&id, "ere"));
    happened(&m, said(&id, 2, "bye"));
    let never_frames = never.pull().await.to_vec();
    let live = mirror(&never_frames, &id);
    assert_eq!(
        seqs(&live),
        vec![CREATED, 2, 3, 4, 5, 6],
        "the live mirror must hold every event that happened"
    );

    // IT RE-SUBSCRIBES. One call. No cursor was kept, no `after_seq` is sent, no
    // re-emit verb exists to call.
    let mut again = Watch::open(&m, 3, &id).await;
    let back = mirror(again.pull().await, &id);
    assert_eq!(
        back, live,
        "phase 1: a consumer that lost its connection and re-subscribed must hold \
         exactly what one that never disconnected holds — no cursor, no gap query, \
         no re-emit: left {:?} right {:?}",
        seqs(&back),
        seqs(&live)
    );

    // ── phase 2: every subscriber leaves, so `Lazy` drops the root ───────────
    m.hub.dropped(1);
    m.hub.dropped(3);
    drop(never);
    drop(again);
    settle().await;
    assert!(
        !m.hub.tracked("threads/log"),
        "`Tracking::Lazy` must drop the root on the last unsubscribe, or phase 2 \
         re-exports instead of re-hydrating and proves nothing"
    );

    let mut cold = Watch::open(&m, 4, &id).await;
    let hydrated = mirror(cold.pull().await, &id);
    assert_eq!(
        hydrated, live,
        "phase 2: a mirror rebuilt from a COLD hydrate must equal one built by \
         following every live frame — that is what makes re-subscribe a complete \
         recovery: left {:?} right {:?}",
        seqs(&hydrated),
        seqs(&live)
    );
    drop(tmp);
}

/// **THE HYDRATE AND THE LIVE FOLD BUILD THE SAME ROW, FIELD FOR FIELD.**
///
/// A sibling measured the trap on 2026-10-06: twelve of fourteen tests stayed
/// GREEN under a hydrate-only mutation. The structural defence here is that there
/// is only one builder (`log::row_of`) and both paths call it; this is the test
/// that notices when that stops being true.
///
/// It is stronger than the headline above, which compares WINDOWS: this compares
/// the SAME event reached two ways, so a mutation that changed one field on one
/// path and nothing else is red here.
///
/// **THE MUTATION.** In `Log::hydrates_key`, build the row by hand instead of
/// calling `row_of`: `LogRow { seq: e.seq, message_id: None, .. }`.
///
/// Forced red:
/// ```text
/// one event reached two ways must be one row: the hydrate and the live fold
/// have ONE builder and this is what notices when they stop
///   left: Object {"kind": String("user_message"), "message_id": Number(1), ...}
///  right: Object {"kind": String("user_message"), ...}
/// ```
#[tokio::test]
async fn the_hydrate_and_the_live_fold_build_the_same_row() {
    let (m, tmp) = start().await;
    let id = a_thread(&m, tmp.path());

    // LIVE: subscribe first, then the event arrives.
    let mut live_conn = Watch::open(&m, 1, &id).await;
    happened(&m, said(&id, 1, "the one event"));
    let folded = mirror(live_conn.pull().await, &id);

    // COLD: every subscriber leaves, the root is dropped, a new one hydrates the
    // same events out of the store.
    m.hub.dropped(1);
    drop(live_conn);
    settle().await;
    let mut cold = Watch::open(&m, 2, &id).await;
    let read = window(cold.pull().await, &id);

    assert_eq!(
        seqs(&folded),
        vec![CREATED, 2],
        "the creation and the one event: {folded:?}"
    );
    assert_eq!(
        folded, read,
        "one event reached two ways must be one row: the hydrate and the live fold \
         have ONE builder and this is what notices when they stop"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. THE BULK KINDS ARE THE POINT
// ─────────────────────────────────────────────────────────────────────────────

/// **A `stream_event` IS IN THE WINDOW, WITH ITS PAYLOAD ON THE FRAME.**
///
/// 58% of the production ledger is the three `BULK_KINDS` and
/// `crate::shapes::threads`' fold returns on all of them, which is exactly why a
/// consumer that wanted the transcript had to keep the firehose. A window that
/// filtered them would leave the firehose load-bearing and this shape pointless.
///
/// The payload must be ON THE FRAME (`Payload::Here`), not behind the door: a
/// fetch per delta is 125 round trips a message, which is worse than the thing
/// being retired.
///
/// **THE MUTATION.** Add `crate::shapes::threads::BULK_KINDS.contains(..)` and a
/// `return` to the top of the fold's `Push::Event` arm — the sibling shape's own
/// first line.
///
/// Forced red:
/// ```text
/// the kinds the pool discards are the kinds this shape exists to carry: []
/// ```
#[tokio::test]
async fn the_bulk_kinds_are_in_the_window_with_their_payloads_on_the_frame() {
    let (m, tmp) = start().await;
    let id = a_thread(&m, tmp.path());
    let mut w = Watch::open(&m, 1, &id).await;
    happened(&m, delta(&id, "Hel"));
    happened(&m, delta(&id, "lo"));
    let held = mirror(w.pull().await, &id).to_vec();
    let kinds: Vec<&str> = held.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        vec!["thread_started", "stream_event", "stream_event"],
        "the kinds the pool discards are the kinds this shape exists to carry: \
         {held:?}"
    );
    assert_eq!(
        held[1]["payload"]["here"]["raw"]["delta"]["text"], "Hel",
        "a delta's payload rides the frame — a fetch per delta is 125 round trips \
         a message: {}",
        held[1]
    );
    drop(tmp);
}

/// **NOTHING BUT A LEDGER EVENT REACHES THE WINDOW.**
///
/// The opposing half of the test above: a fold that appended everything would be
/// green there and red here. `Push::Status` is a DERIVED level and
/// `Fact::ThreadSeen` is a CONSUMER'S OWN ACT — appending either would put one
/// consumer's cursor into every other consumer's transcript, and neither is a row
/// of the `events` table.
///
/// **THE MUTATION.** Give the fold's catch-all arm the append.
///
/// Forced red:
/// ```text
/// the window holds the LEDGER and nothing else — a derived level and a
/// consumer's own cursor are not things that happened in the transcript: [1, 1]
/// ```
#[tokio::test]
async fn nothing_but_a_ledger_event_reaches_the_window() {
    let (m, tmp) = start().await;
    let id = a_thread(&m, tmp.path());
    let mut w = Watch::open(&m, 1, &id).await;
    happened(&m, said(&id, 1, "one real event"));
    m.hub.emit(Push::Status {
        thread_id: id.clone(),
        state: "active".into(),
        executing: true,
        queued: 0,
        unread: 0,
        detail: None,
        account: None,
    });
    m.hub.emit(Push::Liveness {
        thread_id: id.clone(),
        busy: true,
        parent_running: false,
        running_subagents: vec![],
    });
    m.hub.emit_fact(threads::event::Fact::ThreadSeen {
        thread_id: id.clone(),
        seen_seq: 1,
        unread: 0,
    });
    let held = mirror(w.pull().await, &id);
    assert_eq!(
        seqs(&held),
        vec![CREATED, 2],
        "the window holds the LEDGER and nothing else — a derived level and a \
         consumer's own cursor are not things that happened in the transcript: {:?}",
        seqs(&held)
    );
    drop(tmp);
}

/// **A REPLAYED `seq` IS NOT APPENDED TWICE.**
///
/// The guard is `seq` and not the stamp, because `Sequence::append` skips the
/// stamp rule on purpose — that function's doc carries the argument. The cost of
/// skipping it is that identity has to do the work, and this is the test that it
/// does: two appends of the same item with different stamps both pass a stamp
/// comparison and only an identity can refuse them.
///
/// **THE MUTATION.** Delete the `if held.is_some_and(|h| se.seq <= h) { return; }`
/// guard.
///
/// Forced red:
/// ```text
/// `seq` is monotone per thread by construction, so at-or-below the head is
/// already-seen and appending it would double-count a row for ever: [1, 2, 2, 1]
/// ```
#[tokio::test]
async fn a_replayed_seq_is_not_appended_twice() {
    let (m, tmp) = start().await;
    let id = a_thread(&m, tmp.path());
    let mut w = Watch::open(&m, 1, &id).await;
    let mut one = said(&id, 1, "once");
    let mut two = delta(&id, "twice");
    one.seq = happened(&m, one.clone());
    two.seq = happened(&m, two.clone());
    // The same two, again — a replay, which `ON CONFLICT DO NOTHING` makes a
    // no-op in the store and which the fold must also refuse. Newest first, so an
    // implementation that compared only against the IMMEDIATELY preceding item
    // would also be red.
    replayed(&m, two);
    replayed(&m, one);
    let held = mirror(w.pull().await, &id);
    assert_eq!(
        seqs(&held),
        vec![CREATED, 2, 3],
        "`seq` is monotone per thread by construction, so at-or-below the head is \
         already-seen and appending it would double-count a row for ever: {:?}",
        seqs(&held)
    );
    drop(tmp);
}

/// **THE FOLD MINTS NO KEY FOR A THREAD NOBODY OPENED.**
///
/// 2,463,981 ledger events exist. Without this guard the first minute of traffic
/// grows the root to the whole fleet, each key carrying a 4,000-row window. This
/// is `crate::shapes::threads`' `holds` guard, and it is load-bearing here for the
/// same reason squared: its window is bigger.
///
/// **THE MUTATION.** Delete the `if !holds(tx, &se.thread_id) { return; }` guard.
///
/// Forced red:
/// ```text
/// an `OnDemand` root may only hold what somebody named — a fold that minted
/// keys would hold the whole fleet within minutes: ["<other>"]
/// ```
#[tokio::test]
async fn the_fold_mints_no_key_for_a_thread_nobody_opened() {
    let (m, tmp) = start().await;
    let watched = a_thread(&m, tmp.path());
    let other = a_thread(&m, tmp.path());
    let mut w = Watch::open(&m, 1, &watched).await;
    // Traffic on a thread nobody opened a window on.
    for _ in 1..=5 {
        happened(&m, delta(&other, "noise"));
    }
    let frames = w.pull().await.to_vec();
    let touched: Vec<String> = frames
        .iter()
        .filter_map(|x| x.wire.key.clone())
        .filter(|k| k != &watched)
        .collect();
    assert!(
        touched.is_empty(),
        "an `OnDemand` root may only hold what somebody named — a fold that minted \
         keys would hold the whole fleet within minutes: {touched:?}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. WHERE THE BYTES GO
// ─────────────────────────────────────────────────────────────────────────────

/// **A SMALL EVENT RIDES THE FRAME WHOLE.**
///
/// The opposing half of the next test. A `MAX_INLINE_PAYLOAD` of `0` would send
/// every row through the door and redden this; a bound past every body would send
/// none and redden the next.
///
/// **THE MUTATION.** `MAX_INLINE_PAYLOAD = 0`.
///
/// Forced red:
/// ```text
/// an event that fits rides the frame whole, held opaquely: Object {"there":
/// Object {"bytes": Number(3), ...}}
/// ```
#[tokio::test]
async fn a_small_event_rides_the_frame_whole() {
    let (m, tmp) = start().await;
    let id = a_thread(&m, tmp.path());
    happened(&m, said(&id, 1, "hi"));
    let mut w = Watch::open(&m, 1, &id).await;
    let rows = window(w.pull().await, &id);
    assert_eq!(
        rows[1]["payload"]["here"]["text"], "hi",
        "an event that fits rides the frame whole, held opaquely: {}",
        rows[1]["payload"]
    );
    assert_eq!(
        rows[1]["payload"]["here"]["metadata"]["from"], "the test",
        "held OPAQUELY means the whole payload, not a field this crate chose: {}",
        rows[1]["payload"]
    );
    drop(tmp);
}

/// **A LARGE MESSAGE PUTS ITS TEXT BEHIND THE DOOR AND KEEPS ITS STRUCTURE ON THE
/// FRAME — and the `bytes` it states is the `bytes` the door delivers.**
///
/// The operator's constraint: *"do not put large content in the sequence."* The
/// reason it is `Payload::Split` and not `Payload::There` is that `intent`,
/// `metadata` and `parent_tool_use_id` are what a consumer nesting subagent output
/// reads, and they are small — withholding them with the paragraph would make a
/// consumer fetch 40 KB to learn one id.
///
/// The `text` KEY IS ABSENT rather than `null`: the tag already says the text was
/// lifted out, and a `null` is a second spelling of that which a consumer could
/// read as *the text is empty*.
///
/// **THE MUTATION.** `MAX_INLINE_PAYLOAD = usize::MAX`.
///
/// Forced red:
/// ```text
/// a message past the bound carries its structure and not its paragraph: Object
/// {"here": Object {"intent": ..., "metadata": ..., "text": String("aaaa...")}}
/// ```
#[tokio::test]
async fn a_large_message_splits_its_text_out_and_the_door_serves_exactly_that() {
    let (m, tmp) = start().await;
    let id = a_thread(&m, tmp.path());
    let long = "a".repeat(4096);
    happened(&m, said(&id, 1, &long));
    let mut w = Watch::open(&m, 1, &id).await;
    let rows = window(w.pull().await, &id).to_vec();
    let split = &rows[1]["payload"]["split"];
    assert!(
        !split.is_null(),
        "a message past the bound carries its structure and not its paragraph: {}",
        rows[1]["payload"]
    );
    assert_eq!(
        split["event"]["text"],
        Value::Null,
        "the text key is REMOVED, not nulled — the tag already says it was lifted \
         out: {split}"
    );
    assert!(
        split["event"].get("text").is_none(),
        "removed means absent: {split}"
    );
    assert_eq!(
        split["event"]["metadata"]["from"], "the test",
        "the structure stays on the frame, or a consumer fetches 4 KB to learn one \
         id: {split}"
    );
    assert_eq!(
        split["text"]["bytes"],
        long.len() as u64,
        "the summary's `bytes` is what the door will deliver, measured by the one \
         function that serves it: {split}"
    );
    assert_eq!(
        split["text"]["transports"],
        json!(["ws", "get"]),
        "both doors, whatever the size — pruning by size would be the producer \
         making the consuming side's call: {split}"
    );
    drop(tmp);
}

/// **A SUMMARY'S `bytes` IS WHAT THE DOOR WILL DELIVER, FOR EVERY KIND — measured
/// by the one function that serves it.**
///
/// The hazard this closes: a summary states a size, a door serves bytes, and if
/// the two are produced by two expressions they can disagree. `bytes` here is the
/// length of `body::served_of`'s own answer, and the door calls the same function
/// on the same `ThreadEvent`, so the agreement is by construction rather than by
/// coincidence. This is the test that notices when it stops being by construction.
///
/// It also pins the KIND dispatch, which is the widening this shape needed: before
/// it, `Resources::fetch` refused any id whose event was not one of the three
/// message kinds, on the stated grounds that *"the shape publishes no id for a
/// bodiless event"*. This shape publishes them, so a refusal would be a summary
/// naming something that cannot be read. The END-TO-END proof of the widened door
/// is `thread_bodies.rs::a_miss_is_the_same_typed_refusal_on_both_doors`, which
/// drives both real doors; this is the decision, where the decision lives.
///
/// **THE MUTATION.** In `body::served_summary`, `payload_json.len()` instead of
/// `served.len()` — the obvious-looking size, and a different number.
///
/// Forced red:
/// ```text
/// a summary's `bytes` must be the length of what the door serves: 4119 vs 4096
/// ```
#[test]
fn a_summarys_bytes_is_what_the_door_will_deliver_for_every_kind() {
    use threads::shapes::body;
    let long = "b".repeat(4096);
    // A MESSAGE: the door serves its TEXT, under text/plain.
    let msg = ThreadEvent::UserMessage {
        text: long.clone(),
        intent: Default::default(),
        metadata: json!({"from": "the test"}),
    };
    let (served, ct) = body::served_of(&msg);
    assert_eq!(ct, "text/plain; charset=utf-8");
    assert_eq!(served, long, "a message's body is its text and nothing else");
    let s = body::served_summary("t", 7, &msg).expect("spellable");
    assert_eq!(
        s.bytes,
        served.len() as u64,
        "a summary's `bytes` must be the length of what the door serves: {} vs {}",
        s.bytes,
        served.len()
    );

    // ANYTHING ELSE: the door serves the whole EVENT, under JSON.
    let other = ThreadEvent::Unknown {
        raw: json!({"tool": "read", "out": long}),
    };
    let (served, ct) = body::served_of(&other);
    assert_eq!(
        ct, "application/json; charset=utf-8",
        "a bodiless event's resource is its PAYLOAD, and the media type says so"
    );
    assert_eq!(
        served,
        serde_json::to_string(&other).unwrap(),
        "the whole event, re-serialised through the same expression the door uses"
    );
    let s = body::served_summary("t", 8, &other).expect("spellable");
    assert_eq!(s.bytes, served.len() as u64);
    assert_eq!(
        serde_json::to_value(&s).unwrap()["transports"],
        json!(["ws", "get"]),
        "both doors for every row, whatever the size"
    );

    // AND NO SUMMARY IS PUBLISHED FOR AN ID THE DOOR COULD NOT ACCEPT.
    assert_eq!(
        body::served_summary("a/b", 1, &other),
        None,
        "a summary naming an unfetchable resource is a shape claiming something \
         exists that cannot be read"
    );
}

/// **A BODILESS EVENT PAST THE BOUND IS CARRIED WHOLE BEHIND THE DOOR, not split
/// and not dropped.**
///
/// `Payload::There`'s only instance, and the thing that makes a 2 MB tool result
/// reachable without an RPC: there is no text to lift out of it, so the whole
/// event is the resource.
///
/// **THE MUTATION.** Make `payload_of` return `Payload::Here(whole)` for every
/// non-message event.
///
/// Forced red:
/// ```text
/// a payload past the bound with no text to lift is carried whole behind the
/// door: Object {"here": Object {"raw": ...}}
/// ```
#[tokio::test]
async fn a_bodiless_event_past_the_bound_is_carried_whole_behind_the_door() {
    let (m, tmp) = start().await;
    let id = a_thread(&m, tmp.path());
    let long = "b".repeat(4096);
    let seq = happened(
        &m,
        StoredEvent {
            thread_id: id.clone(),
            seq: 0,
            message_id: None,
            ts: chrono::Utc::now().to_rfc3339(),
            event: ThreadEvent::Unknown {
                raw: json!({"tool": "read", "out": long}),
            },
        },
    );
    let mut w = Watch::open(&m, 1, &id).await;
    let rows = window(w.pull().await, &id).to_vec();
    let there = rows[1]["payload"]["there"].clone();
    assert!(
        !there.is_null(),
        "a payload past the bound with no text to lift is carried whole behind the \
         door: {}",
        rows[1]["payload"]
    );
    assert_eq!(
        there["id"],
        format!("{id}.{seq}"),
        "the id is `<thread_id>.<seq>`, minted by the producer and never spellable \
         by a consumer: {there}"
    );
    assert!(
        there["bytes"].as_u64().unwrap() > 4096,
        "and `bytes` is the whole event, which is bigger than the text inside it: \
         {there}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. THE CEILINGS — rule 2.8, refused and never clamped
// ─────────────────────────────────────────────────────────────────────────────

/// **A `retain` ABOVE THE CEILING IS REFUSED, NAMING IT.**
///
/// And the ceiling is the shape's declared window, published on `shapes.list`, so
/// the number in the refusal and the number in the contract cannot drift.
///
/// **THE MUTATION.** Replace the refusal in `Hub::subscribe` with a clamp:
/// `let retain = Some(Retain::Count(n.min(slot.max.retained_items)));`
///
/// Forced red:
/// ```text
/// rule 2.8 REFUSES and never clamps — a clamped answer is a second claim about
/// a number the frame states outright: Ok(SubscribeId(1))
/// ```
#[tokio::test]
async fn a_retain_above_the_window_is_refused_naming_the_ceiling() {
    let (m, tmp) = start().await;
    let id = a_thread(&m, tmp.path());
    let _f = m.hub.connect(1);
    let asked = log::WINDOW + 1;
    match m.hub.subscribe_ask(
        "threads/log",
        Some(&id),
        1,
        None,
        Detail::Rows,
        Retain::Count(asked),
        Beat::Max,
    ) {
        Err(ShapeError::AboveMax {
            shape,
            param,
            asked: a,
            max,
        }) => {
            assert_eq!(shape, "threads/log");
            assert_eq!(param, "retain");
            assert_eq!(a, asked.to_string());
            assert_eq!(
                max,
                log::WINDOW.to_string(),
                "the refusal names the DECLARED window, read off the slot's \
                 published max and not off a literal"
            );
        }
        other => panic!(
            "rule 2.8 REFUSES and never clamps — a clamped answer is a second claim \
             about a number the frame states outright: {other:?}"
        ),
    }
    drop(tmp);
}

/// **A `retain` AT OR UNDER THE CEILING IS A PROJECTION, AND IT REACHES EVERY
/// FRAME.**
///
/// `Root::row_at`'s pairing with `MAX_RETAINED_ITEMS` is the one the chassis
/// cannot check — *"the direction that bites is declaring the ceiling without
/// overriding this: the ask would be accepted and silently ignored"*. This is the
/// test that job describes. It also checks the LIVE re-export, because a knob that
/// held only on the first frame is a stub that pretends to work.
///
/// **THE MUTATION.** Delete `LogRoot::row_at`, so the default ignores `retain`.
///
/// Forced red:
/// ```text
/// a declared ceiling without a `row_at` override accepts an ask and silently
/// ignores it: 5
/// ```
#[tokio::test]
async fn a_retain_under_the_ceiling_shows_the_newest_n_on_every_frame() {
    let (m, tmp) = start().await;
    let id = a_thread(&m, tmp.path());
    for _ in 1..=5 {
        happened(&m, delta(&id, "x"));
    }
    let mut f = m.hub.connect(1);
    m.hub
        .subscribe_ask(
            "threads/log",
            Some(&id),
            1,
            None,
            Detail::Rows,
            Retain::Count(2),
            Beat::Max,
        )
        .unwrap();
    settle().await;
    let rows = window(&drain(&mut f), &id);
    assert_eq!(
        seqs(&rows),
        vec![5, 6],
        "a declared ceiling without a `row_at` override accepts an ask and silently \
         ignores it: {}",
        rows.len()
    );
    // And on the LIVE re-export after it.
    happened(&m, delta(&id, "y"));
    settle().await;
    let frames = drain(&mut f);
    let live = frames
        .iter()
        .filter_map(|x| match &x.wire.event {
            Some(Derived::Row { row }) => row["events"].as_array().cloned(),
            _ => None,
        })
        .last()
        .expect("a live row");
    assert_eq!(
        seqs(&live),
        vec![6, 7],
        "`retain` bounds every `row` frame the connection receives, not only the \
         first: {live:?}"
    );
    drop(tmp);
}

/// **THE NINTH OPEN WINDOW ON ONE CONNECTION IS REFUSED, NAMING THE CEILING, WITH
/// NOTHING QUEUED.**
///
/// Rule 2.8 for the KEY parameter — the one knob a `PerKey` shape had no ceiling
/// for, and the one that decides a root's size when every key is a 4,000-row
/// window. A fast navigator opens one key per thread it passes and closes none,
/// and no `Lazy` drop catches that, because the connection never ends.
///
/// It is refused where the request is still a REQUEST, which is the armed-timer
/// cap's own repair: a cap discovered inside the task runs against an item the RPC
/// already answered `{queued: true}`, and the consumer then waits forever on a
/// subscription it was told it had.
///
/// **THE MUTATION.** `Log::MAX_OPEN_KEYS_PER_CONN = None`.
///
/// Forced red:
/// ```text
/// rule 2.8: a `PerKey` shape whose key costs a whole window must ceiling the
/// key, refused and never clamped: Ok(SubscribeId(9))
/// ```
#[tokio::test]
async fn the_ninth_open_window_on_one_connection_is_refused_naming_the_ceiling() {
    let (m, tmp) = start().await;
    let ids: Vec<String> = (0..=log::OPEN_WINDOWS)
        .map(|_| a_thread(&m, tmp.path()))
        .collect();
    let mut f = m.hub.connect(1);
    for id in ids.iter().take(log::OPEN_WINDOWS) {
        m.hub
            .subscribe("threads/log", Some(id), 1, None)
            .expect("inside the ceiling");
    }
    settle().await;
    let before = drain(&mut f).len();
    match m
        .hub
        .subscribe("threads/log", Some(&ids[log::OPEN_WINDOWS]), 1, None)
    {
        Err(ShapeError::TooManyKeys { shape, max }) => {
            assert_eq!(shape, "threads/log");
            assert_eq!(
                max,
                log::OPEN_WINDOWS,
                "the refusal names the ceiling, read off the slot's published max"
            );
            let msg = ShapeError::TooManyKeys {
                shape: shape.clone(),
                max,
            }
            .to_string();
            assert!(
                msg.contains("shapes.unsubscribe") && msg.contains("not clamped"),
                "the remedy is to RELEASE a key, not to ask smaller — there is no \
                 smaller ask than one key: {msg}"
            );
        }
        other => panic!(
            "rule 2.8: a `PerKey` shape whose key costs a whole window must ceiling \
             the key, refused and never clamped: {other:?}"
        ),
    }
    settle().await;
    assert_eq!(
        drain(&mut f).len(),
        0,
        "a refused subscribe creates no subscription, so no frame follows it \
         (before: {before})"
    );
    drop(tmp);
}

/// **A RE-SUBSCRIBE TO A KEY ALREADY OPEN COSTS NO SLOT, AND RELEASING ONE GIVES
/// IT BACK.**
///
/// The opposing half of the test above: a cap that never fires reddens that one,
/// and a cap that fires on a re-subscribe reddens this one. The second is not
/// hypothetical — rule 2.5 makes re-subscribe the remedy for every doubt a
/// consumer can have about its mirror, so a ceiling counting re-subscribes would
/// refuse recovery after exactly eight reconnects.
///
/// **THE MUTATION.** Make the reservation a count instead of a set:
/// `if *n >= max { return Err(..) } *n += 1;`
///
/// Forced red:
/// ```text
/// rule 2.5 makes re-subscribe the remedy for every doubt — a ceiling that
/// counted them would refuse recovery: Err(TooManyKeys { shape: "threads/log",
/// max: 8 })
/// ```
#[tokio::test]
async fn a_resubscribe_to_an_open_key_costs_no_slot_and_a_release_gives_one_back() {
    let (m, tmp) = start().await;
    let ids: Vec<String> = (0..=log::OPEN_WINDOWS)
        .map(|_| a_thread(&m, tmp.path()))
        .collect();
    let _f = m.hub.connect(1);
    for id in ids.iter().take(log::OPEN_WINDOWS) {
        m.hub.subscribe("threads/log", Some(id), 1, None).unwrap();
    }
    settle().await;
    // Re-subscribe to every key it already holds, twice over. None of these is a
    // new window.
    for _ in 0..2 {
        for id in ids.iter().take(log::OPEN_WINDOWS) {
            m.hub.subscribe("threads/log", Some(id), 1, None).unwrap_or_else(|e| {
                panic!(
                    "rule 2.5 makes re-subscribe the remedy for every doubt — a \
                     ceiling that counted them would refuse recovery: {e:?}"
                )
            });
        }
    }
    // Release one, and the ninth key becomes legal.
    m.hub.unsubscribe("threads/log", Some(&ids[0]), 1);
    settle().await;
    m.hub
        .subscribe("threads/log", Some(&ids[log::OPEN_WINDOWS]), 1, None)
        .expect("a released slot is a slot a different key may take");
    drop(tmp);
}

/// **A KEYLESS `detail: rows` SUBSCRIBE IS REFUSED, NAMING THE ROW CEILING.**
///
/// "Every key I know about" on a shape that holds a 4,000-row window per key is
/// 2,463,981 events and 3.8 GB. Acking it and then exporting nothing is the
/// failure class the refusal exists to close.
///
/// **THE MUTATION.** `Log::HYDRATE = Hydrate::Whole`.
///
/// Forced red:
/// ```text
/// a keyless subscribe to a per-key window shape must be refused on the request
/// id: Ok(SubscribeId(1))
/// ```
#[tokio::test]
async fn a_keyless_rows_subscribe_is_refused_naming_the_row_ceiling() {
    let (m, tmp) = start().await;
    let _f = m.hub.connect(1);
    match m.hub.subscribe("threads/log", None, 1, None) {
        Err(ShapeError::KeyRequired { shape, max_rows }) => {
            assert_eq!(shape, "threads/log");
            assert_eq!(max_rows, 500);
        }
        other => panic!(
            "a keyless subscribe to a per-key window shape must be refused on the \
             request id: {other:?}"
        ),
    }
    assert!(
        !m.hub.tracked("threads/log"),
        "and a refused subscribe must not have started the fold — the whole point \
         of `Lazy` on a fold that reads every ledger event"
    );
    drop(tmp);
}

/// **THE CEILINGS RIDE `shapes.list`, SO A CONSUMER READS THE CONTRACT BEFORE IT
/// ASKS** — rule 2.8's last clause.
///
/// And `open_keys_per_conn` is ABSENT on every shape that declares none, which is
/// what kept `GOLD_INFO` from moving when this field was added: the field exists
/// so one shape can publish a ceiling, and it must not move a byte of
/// `shapes.list` for the shapes that have none.
///
/// **THE MUTATION.** Drop the `skip_serializing_if` from
/// `ShapeMax::open_keys_per_conn`.
///
/// Forced red:
/// ```text
/// a shape that declares no key ceiling must not publish one — that absence is
/// what keeps every other shape's `shapes.list` byte-identical: Object {...
/// "open_keys_per_conn": Null ...}
/// ```
#[tokio::test]
async fn the_window_and_key_ceilings_ride_shapes_list_and_nothing_else_moves() {
    let (m, tmp) = start().await;
    let list = m.hub.list();
    let mine = list
        .iter()
        .find(|i| i.name == "threads/log")
        .expect("registered");
    let max = serde_json::to_value(&mine.max).unwrap();
    assert_eq!(max["retained_items"], log::WINDOW);
    assert_eq!(max["open_keys_per_conn"], log::OPEN_WINDOWS);
    assert_eq!(
        serde_json::to_value(&mine.contract).unwrap(),
        json!({"version": 1, "serves": [1]}),
        "`serves` is the WHOLE set a consumer may ask for, version included, so \
         nobody has to concatenate two fields; `SERVES` is empty, which is \
         exact-match-only — what a shape gets for free"
    );

    let pool = list.iter().find(|i| i.name == "threads").expect("registered");
    let pool_max = serde_json::to_value(&pool.max).unwrap();
    assert!(
        pool_max.get("open_keys_per_conn").is_none(),
        "a shape that declares no key ceiling must not publish one — that absence \
         is what keeps every other shape's `shapes.list` byte-identical: {pool_max}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. THE WINDOW IS A WINDOW
// ─────────────────────────────────────────────────────────────────────────────

/// **PAST THE CAPACITY THE OLDEST GOES, AND THE EXPORT SAYS WHAT IT COVERS.**
///
/// Rule 2.8's fourth clause — *"a short answer says structurally that it is
/// short, never a silent truncation"* — applied to a window rather than to a
/// page. `covers_since` is the oldest retained stamp, so a consumer can see that
/// its transcript starts somewhere and can start the paged walk there.
///
/// Driven at a small capacity through the cell directly, because filling 4,000
/// rows through a hub with a 100 ms debounce is a test that measures the debounce.
/// The capacity under test is the cell's, which is `WINDOW` in the shape.
///
/// **THE MUTATION.** In `Sequence::read`, `complete` instead of
/// `complete && skip == 0`.
///
/// Forced red:
/// ```text
/// a window handed more than it declares is a FLOOR of the log and must say so:
/// true
/// ```
#[test]
fn past_the_capacity_the_oldest_goes_and_the_export_says_what_it_covers() {
    use shapewire::fold::Sequence;
    let t0 = chrono::Utc::now();
    let mut cell: Sequence<u64> = Sequence::new_capacity_only(3);
    let entries: Vec<(chrono::DateTime<chrono::Utc>, u64)> = (1..=5)
        .map(|n| (t0 + chrono::Duration::seconds(n as i64), n))
        .collect();
    cell.read(t0 + chrono::Duration::seconds(10), entries, true);
    assert_eq!(
        cell.to_json(),
        json!([3, 4, 5]),
        "a read keeps the NEWEST `capacity` items, which is the end anybody asking \
         for less of a window wants"
    );
    let state = serde_json::to_value(cell.state()).unwrap();
    assert_eq!(
        state["complete"], false,
        "a window handed more than it declares is a FLOOR of the log and must say \
         so: {state}"
    );
    assert_eq!(
        state["covers_since"],
        serde_json::to_value(t0 + chrono::Duration::seconds(3)).unwrap(),
        "and `covers_since` says where it starts, so the paged walk has a place to \
         begin: {state}"
    );

    // A live append evicts the front and carries the flags rather than inventing
    // them — the line that would otherwise erase every `read`.
    cell.append(t0 + chrono::Duration::seconds(11), 6);
    let state = serde_json::to_value(cell.state()).unwrap();
    assert_eq!(cell.to_json(), json!([4, 5, 6]));
    assert_eq!(
        state["complete"], false,
        "a live append tells you nothing about whether the hydrate was whole: \
         {state}"
    );
    assert_eq!(
        state["since_init"],
        Value::Null,
        "and a cell that WAS hydrated does not become `since_init` because \
         something happened in it: {state}"
    );
}

/// **A WINDOW WITH NO TIME BOUND DOES NOT EMPTY ITSELF OVERNIGHT.**
///
/// `Sequence::new_capacity_only`'s whole reason, which is `Recency`'s: the thread
/// a consumer has open may have been quiet for a week, and a 24 h window would
/// hand it an empty transcript in exactly the state where it most wants the last
/// thing that was said.
///
/// **THE MUTATION.** `LogCells::default` uses
/// `Sequence::new(WINDOW, chrono::Duration::hours(24))`.
///
/// Forced red:
/// ```text
/// the transcript of a thread nobody has touched for a week is still the
/// transcript: []
/// ```
#[test]
fn a_count_bounded_window_does_not_empty_itself_overnight() {
    use shapewire::fold::Sequence;
    let week_ago = chrono::Utc::now() - chrono::Duration::days(7);
    let mut cell: Sequence<u64> = Sequence::new_capacity_only(8);
    cell.read(chrono::Utc::now(), vec![(week_ago, 1), (week_ago, 2)], true);
    assert_eq!(
        cell.to_json(),
        json!([1, 2]),
        "the transcript of a thread nobody has touched for a week is still the \
         transcript: {}",
        cell.to_json()
    );
    // And one live append at `now` must not sweep them out either.
    cell.append(chrono::Utc::now(), 3);
    assert_eq!(
        cell.to_json(),
        json!([1, 2, 3]),
        "a count bound is a count bound on the live path too: {}",
        cell.to_json()
    );
}


/// **THIS SHAPE MAKES NO ACTIVITY CLAIM, AND ITS `shared` BLOCK IS WHERE ONE WOULD
/// HAVE TO LIVE.**
///
/// A sibling landed `recent-updates`' ordering on last user input only (`37187d6`)
/// because *"any event is activity"* was a real defect: only 0.36% of 2,463,981
/// events are `user_message`, so a `stream_event` reordering the fleet moved a
/// thread he had not spoken in. This shape folds EVERY event, which makes it the
/// obvious place for that defect to come back — a shape-level recency over these
/// appends would be exactly it, built out of the kind that caused it.
///
/// So the guard is structural rather than behavioural: `shared` holds two cells and
/// the row holds one, and a cross-key order has nowhere to be. The per-key rows are
/// not comparable to each other — there is no `at` on a key, no `last_event_at`, no
/// count — and this is the test that stays red if one appears.
///
/// The order across threads exists and is `threads/recent-updates`, which orders on
/// `user_message` alone. One way to know a thing.
///
/// **THE MUTATION.** Add a `last_event_at: Register<Timestamp>` to `LogRoot`, set it
/// in the fold, and publish it on `shared`.
///
/// Forced red:
/// ```text
/// a shape-level cell on the log is where "any event is activity" comes back —
/// the order across threads is `threads/recent-updates`, which orders on
/// `user_message` alone: ["last_event_at", "limits", "shape_version"]
/// ```
#[tokio::test]
async fn the_log_makes_no_activity_claim_and_has_nowhere_to_put_one() {
    let (m, tmp) = start().await;
    let id = a_thread(&m, tmp.path());
    let mut w = Watch::open(&m, 1, &id).await;
    for _ in 0..20 {
        happened(&m, delta(&id, "noise"));
    }
    let frames = w.pull().await.to_vec();
    let shared = frames
        .iter()
        .filter(|f| f.wire.op == Op::Read)
        .last()
        .expect("an export")
        .wire
        .shared
        .clone()
        .unwrap_or(Value::Null);
    let keys: Vec<String> = shared
        .as_object()
        .expect("a shared block")
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        keys,
        vec!["limits".to_string(), "shape_version".to_string()],
        "a shape-level cell on the log is where \"any event is activity\" comes back \
         — the order across threads is `threads/recent-updates`, which orders on \
         `user_message` alone: {keys:?}"
    );

    let row = last_export(&frames, &id).unwrap();
    let row_keys: Vec<String> = row.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        row_keys,
        vec![
            "events".to_string(),
            "retained".to_string(),
            "thread_id".to_string()
        ],
        "and the ROW carries no per-key activity stamp either — nothing here is \
         comparable across keys, which is what keeps a fleet order out of a shape \
         that folds every event: {row_keys:?}"
    );
    drop(tmp);
}

/// **THE SHAPE'S OWN WINDOW HAS NO CLOCK — a week-old transcript survives a live
/// append.**
///
/// `a_count_bounded_window_does_not_empty_itself_overnight` above proves the CELL;
/// this proves the SHAPE uses that cell. The two are apart because the mutation
/// anybody reaches for — `Sequence::new(WINDOW, Duration::hours(24))` in
/// `LogCells::default` — leaves the cell test green, and a guard that cannot see the
/// one mutation it exists for is a decoration.
///
/// **IT TOOK TWO GOES, AND THE FIRST FAILURE IS THE INTERESTING ONE.** The first
/// version seeded the week-old events on a thread it had just CREATED, and came
/// back NO RED: `thread_started` is stamped NOW, it sits at the FRONT of the deque
/// (the order is `seq`, not the stamp), and `Sequence::evict` only pops from the
/// front — so a fresh row at the head shields every stale row behind it and a 24 h
/// window evicts nothing. That is a real property of the cell and not a quirk of
/// the fixture: time eviction on a log assumes arrival order matches stamp order,
/// and on a log hydrated out of a store it only mostly does. It is one more reason
/// this shape's window is bounded by COUNT, where there is nothing to assume.
///
/// So the subject here is a thread whose whole log is old: the events are written
/// under an id `create_thread` never minted, which is exactly what `hydrates_key`
/// reads — the store, by `thread_id`, whether or not the pool holds a row for it.
///
/// **THE MUTATION.** `LogCells::default` uses
/// `Sequence::new(WINDOW, chrono::Duration::hours(24))`.
///
/// Forced red:
/// ```text
/// one live append must not sweep out a transcript the hydrate just read — a
/// window with a clock empties itself overnight, which is the one state a
/// consumer most wants it full: [3]
/// ```
#[tokio::test]
async fn the_shapes_window_has_no_clock_so_a_week_old_transcript_survives_an_append() {
    let (m, tmp) = start().await;
    // An id the pool never minted, so there is no `thread_started` stamped NOW at
    // the front of the window — see the docstring for why that matters.
    let id = "a-thread-whose-whole-log-is-old".to_string();
    let week_ago = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();
    for seq in 1..=2u64 {
        m.store
            .append_event(&StoredEvent {
                thread_id: id.clone(),
                seq,
                message_id: None,
                ts: week_ago.clone(),
                event: ThreadEvent::StreamEvent {
                    raw: json!({"type": "content_block_delta", "index": 0,
                                "delta": {"type": "text_delta", "text": "old"}}),
                },
            })
            .unwrap();
    }
    let mut w = Watch::open(&m, 1, &id).await;
    assert_eq!(
        seqs(&window(w.pull().await, &id)),
        vec![1, 2],
        "the hydrate reads what the log holds, whatever its age — `Sequence::read` \
         does not sweep by time"
    );
    // ONE live append, stamped now. This is where a clock would bite.
    //
    // **THE ASSERTION IS ON THE PRODUCER'S OWN RE-EXPORTED ROW, not on the
    // accumulated mirror**, and the difference is the third thing this test had to
    // learn: a consumer's mirror only ever GROWS from transitions — eviction
    // reaches it on the next `row` frame, never as a transition — so a mirror
    // assembled from an export taken before the append plus the append itself holds
    // the stale rows whatever the producer did with them. The `row` frame the
    // debounce flushes after the append is what the producer actually holds.
    happened(&m, delta(&id, "new"));
    let frames = w.pull().await.to_vec();
    let row = frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(id.as_str()))
        .filter_map(|f| match &f.wire.event {
            Some(Derived::Row { row }) => row["events"].as_array().cloned(),
            _ => None,
        })
        .last()
        .expect("the append flushes a `row` frame carrying the window");
    assert_eq!(
        seqs(&row),
        vec![1, 2, 3],
        "one live append must not sweep out a transcript the hydrate just read — a \
         window with a clock empties itself overnight, which is the one state a \
         consumer most wants it full: {:?}",
        seqs(&row)
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// TLL-1b.2. THE PER-HYDRATE LINE IS DEBUG, NOT INFO
// ─────────────────────────────────────────────────────────────────────────────

/// Every event from `hydrates_key`'s "hydrated one key" site that a subscriber
/// filtered at `max` sees, recorded by level. Installed thread-locally; a
/// `#[tokio::test]` is a current-thread runtime and `hydrate_one_key` calls
/// `hydrates_key` inline on it, so the hydrate runs under this subscriber.
fn hydrate_line_capture(
    max: tracing_subscriber::filter::LevelFilter,
) -> (Arc<std::sync::Mutex<Vec<tracing::Level>>>, tracing::subscriber::DefaultGuard) {
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
    struct Cap(Arc<std::sync::Mutex<Vec<tracing::Level>>>);
    struct Msg(String);
    impl tracing::field::Visit for Msg {
        fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
            if f.name() == "message" {
                self.0 = format!("{v:?}");
            }
        }
    }
    impl<S: tracing::Subscriber> Layer<S> for Cap {
        fn on_event(&self, e: &tracing::Event<'_>, _c: Context<'_, S>) {
            let md = e.metadata();
            if md.target() != "threads::shapes::log" {
                return;
            }
            let mut m = Msg(String::new());
            e.record(&mut m);
            if m.0.contains("threads/log hydrated one key") {
                self.0.lock().unwrap().push(*md.level());
            }
        }
    }
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sub = tracing_subscriber::registry().with(Cap(Arc::clone(&seen)).with_filter(max));
    (seen, tracing::subscriber::set_default(sub))
}

/// **TLL-1b.2: ONE `threads/log` HYDRATE SAYS NOTHING AT INFO, AND ITS LINE IS
/// THERE AT DEBUG.** The perf branch added a per-hydrate `tracing::info!` in
/// `Log::hydrates_key`; a hydrate is per subscribe, so at INFO it is prod log
/// volume. It is `debug!` now, same fields.
///
/// Two hydrates of two different threads (a key already held re-exports and
/// does not re-hydrate), one under a capture filtered at INFO, one at DEBUG.
/// Opposed (doctrine 2): the DEBUG half proves the capture sees this site at
/// all, so a capture that sees nothing cannot pass the INFO half; and the INFO
/// half fails if the line goes back to `info!`. Both halves also check the
/// hydrate really happened (the window arrived).
///
/// **THE MUTATIONS.** (a) `tracing::debug!` → `tracing::info!` reddens the INFO
/// half; (b) deleting the event reddens the DEBUG half. Reds pasted in
/// an internal report §5.
#[tokio::test]
async fn a_log_hydrate_is_silent_at_info_and_logged_at_debug() {
    use tracing_subscriber::filter::LevelFilter;
    let (m, tmp) = start().await;
    let quiet = a_thread(&m, tmp.path());
    let loud = a_thread(&m, tmp.path());
    happened(&m, said(&quiet, 1, "first"));
    happened(&m, said(&loud, 1, "first"));

    let (at_info, g) = hydrate_line_capture(LevelFilter::INFO);
    let mut w = Watch::open(&m, 1, &quiet).await;
    assert_eq!(seqs(&window(w.pull().await, &quiet)), vec![CREATED, 2], "the INFO hydrate happened");
    drop(g);
    assert_eq!(
        at_info.lock().unwrap().clone(),
        Vec::<tracing::Level>::new(),
        "a `threads/log` hydrate must log nothing at INFO from its per-key line"
    );

    let (at_debug, g) = hydrate_line_capture(LevelFilter::DEBUG);
    let mut w2 = Watch::open(&m, 2, &loud).await;
    assert_eq!(seqs(&window(w2.pull().await, &loud)), vec![CREATED, 2], "the DEBUG hydrate happened");
    drop(g);
    assert_eq!(
        at_debug.lock().unwrap().clone(),
        vec![tracing::Level::DEBUG],
        "the per-hydrate line must still be emitted, once, at DEBUG"
    );
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
