//! # The write sites — the five that were silent, and the two that were not
//!
//! `docs/shapes.md` §2.5 declines to cover one failure with a timer: *"a producer
//! whose fold silently stops emitting a row it should have"*. This file is that
//! case turned into tests. Each one drives a REAL write — a turn against the
//! fake-claude fixture, a real failover through the admission gate, a real
//! compaction — and asserts the cell moved WITHOUT a re-subscribe, because a cell
//! that is only right after a re-hydrate is a cell the fold has stopped feeding.
//!
//! **No real claude is ever spawned.** `THREADS_CLAUDE_BIN` is the fixture, and
//! the turn MODE is chosen from the prompt text, so parallel tests cannot race on
//! an env var.
//!
//! **Two of the plan's five silent writers are NOT here, because the code refutes
//! them**, and the refutations are recorded at the sites themselves:
//!
//! * `interrupt_thread` writes `thread_interrupts`, which is not an input to
//!   `thread_states` at all — so no level moves across that write and a `Status`
//!   there would announce nothing.
//! * `maybe_dispatch`'s `clear_parked_turn` and `schedule_retry` both run with the
//!   `TurnLock` held, and `executing` outranks `parked` and `retrying` in
//!   `thread_states`' precedence — so the level at those instants is `active` both
//!   before and after the write. Their real edge is the lock release, which
//!   already emits.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use threads::config::Config;
use threads::event::Push;
use threads::shape::{ConnFrames, Derived, Hub, Op};
use threads::shapes::threads as tshape;
use threads::{build_manager, ThreadManager, ThreadSettings};

fn set_env(tmp: &std::path::Path) {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("THREADS_HEARTBEAT_MS", "100");
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
}

async fn start(names: &[&str]) -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let mut accounts = BTreeMap::new();
    for n in names {
        let dir = tmp.path().join(n);
        std::fs::create_dir_all(&dir).unwrap();
        accounts.insert(n.to_string(), dir.to_string_lossy().to_string());
    }
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

/// Every `row` a live frame carried for this key, oldest first — the sequence a
/// mirror would have applied.
fn live_rows(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Vec<Value> {
    frames
        .iter()
        .filter(|f| f.wire.op == Op::Live && f.wire.key.as_deref() == Some(key))
        .filter_map(|f| match &f.wire.event {
            Some(Derived::Row { row }) => Some(row.clone()),
            _ => None,
        })
        .collect()
}

async fn watch(m: &ThreadManager, conn: u64, id: &str) -> ConnFrames {
    let mut f = m.hub.connect(conn);
    m.hub.subscribe("threads", Some(id), conn, None).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    drain(&mut f);
    f
}

/// Wait until `p` holds, or give up after ~8 s. Used instead of a fixed sleep so
/// a slow machine makes the test slower rather than red — a timing assertion that
/// passes by scheduling luck is worse than no assertion.
async fn until(mut p: impl FnMut() -> bool) -> bool {
    for _ in 0..160 {
        if p() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────────

/// **CREATING A THREAD ANNOUNCES ITS STATE.**
///
/// The first of the five, and the only one whose repair is a legacy frame rather
/// than a fact: `upsert_thread` wrote the row and `ThreadStarted` announced the
/// OCCURRENCE, and nothing said what the thread's state now IS. A client that
/// creates a thread and waits used to hear nothing at all until its first send.
///
/// It is asserted on the FIREHOSE, not on a shape, and that is the honest place:
/// under `KeyScope::OnDemand` no consumer can hold a key for a thread that does
/// not exist yet, so the shape cannot observe a creation and the emit that
/// matters is the legacy one. (Which is also why `create_thread` emits no FACTS —
/// an emit nothing could ever fold would be a stub that pretends to work.)
///
/// **THE MUTATION.** Delete the `self.emit_status(&id);` line from
/// `ThreadManager::create_thread`.
///
/// Forced red:
/// ```text
/// a brand-new thread must say what state it is in: no `status` frame among
/// ["event"]
/// ```
#[tokio::test]
async fn creating_a_thread_announces_its_state() {
    let (m, tmp) = start(&["first"]).await;
    let mut rx = m.hub.firehose.subscribe();
    let id = thread_on(&m, tmp.path());

    let mut kinds = Vec::new();
    let mut status = None;
    while let Ok(p) = rx.try_recv() {
        match p {
            Push::Status {
                ref thread_id,
                ref state,
                queued,
                ..
            } if thread_id == &id => {
                kinds.push("status");
                status = Some((state.clone(), queued));
            }
            Push::Event(_) => kinds.push("event"),
            _ => kinds.push("other"),
        }
    }
    let (state, queued) = status.unwrap_or_else(|| {
        panic!("a brand-new thread must say what state it is in: no `status` frame among {kinds:?}")
    });
    assert_eq!(
        (state.as_str(), queued),
        ("idle", 0),
        "a thread that has just been created is idle with nothing queued, and \
         saying so is the answer rather than a default"
    );
    drop(tmp);
}

/// **A DELIVERY MOVES THE QUEUED COUNT DOWN, WHILE THE TURN IS STILL RUNNING.**
///
/// The worst of the five, because it is the ordinary path. `lock_turn` emits a
/// `Status` BEFORE `mark_delivered` moves the rows out of `waiting`, so the frame
/// carried the PRE-decrement count; the next edge was `TurnLock::release` when the
/// turn ended. Every consumer therefore showed a queue depth that was too high for
/// the whole duration of the turn that had just drained it.
///
/// The assertion is the shape of the repair: **a frame exists in which the thread
/// is executing AND the queue is empty.** Without the emit, no such frame is ever
/// produced on this path — `executing: true` arrives with `queued: 1` from
/// `lock_turn`, and `queued: 0` only arrives with `executing: false` from the
/// release.
///
/// **THE DISPATCH IS DRIVEN BY A NUDGE, NOT BY A SEND, and that is the whole
/// reason the test looks like this.** `ThreadManager::send` ends with its own
/// `emit_status` AFTER `maybe_dispatch`, so on the send path the decrement was
/// already announced by accident. The hole is every OTHER caller of
/// `maybe_dispatch`: `nudge_thread` (the re-selection path behind
/// `threads.queue_reselect`), `update_settings` unblocking a parked thread, and
/// `clear_park`. All of them emit BEFORE the dispatch and nothing after it. So the
/// test enqueues a waiting row and nudges, which is exactly what a re-selected
/// message does.
///
/// **A note on what CANNOT be asserted here**, because it looks like a gap: the
/// intermediate `(executing: true, queued: 1)` is not required to appear. Per-key
/// `row` frames coalesce behind `DEBOUNCE_MS` (100 ms) and the lock take and the
/// delivery are microseconds apart, so rule 2.2's "a dropped intermediate is
/// invisible" applies — the mirror after the last frame equals the mirror after
/// every frame. Demanding to see it would be a test asserting the debounce did not
/// do its job.
///
/// **THE MUTATION.** Delete the `self.emit_status(&state.id);` line added after
/// `mark_delivered` in `maybe_dispatch`.
///
/// Forced red:
/// ```text
/// the queue drained while the turn ran and no frame said so. Pairs seen:
/// [(false, 1), (true, 1), (false, 0)]
/// ```
#[tokio::test]
async fn a_delivery_moves_the_queued_count_down_while_the_turn_runs() {
    let (m, tmp) = start(&["first"]).await;
    let id = thread_on(&m, tmp.path());
    let mut f = watch(&m, 30, &id).await;

    // A waiting row, put there without `send`'s trailing announcement.
    // `slowturn` holds the turn open for 0.7 s, so "executing" is a window a test
    // can observe rather than an instant it can miss.
    m.store
        .enqueue(&id, 1, "slowturn please", "immediate", &Value::Null, &[])
        .unwrap();
    let queued_now = || {
        m.store
            .waiting_queue_counts()
            .ok()
            .and_then(|c| c.get(&id).copied())
            .unwrap_or(0)
    };
    assert_eq!(queued_now(), 1, "the row is waiting before the nudge");

    m.nudge_thread(&id);
    let delivered = until(|| queued_now() == 0).await;
    assert!(delivered, "the nudge never dispatched the waiting row");
    // …and then wait for the turn to END, so the release's own frame is in the
    // window too and the assertions below are about a complete story.
    let finished = until(|| !m.is_locked(&id)).await;
    assert!(finished, "the fake turn never released the lock");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let frames = drain(&mut f);
    let pairs: Vec<(bool, u64)> = live_rows(&frames, &id)
        .iter()
        .map(|r| {
            (
                r["status"]["executing"].as_bool().unwrap_or(false),
                r["queued"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    assert!(
        pairs.contains(&(true, 0)),
        "the queue drained while the turn ran and no frame said so. Pairs seen: \
         {pairs:?}"
    );
    // NOT VACUOUS, and the guard is the STORE rather than a frame: the two
    // `queued_now()` assertions above prove the queue really held a row and
    // really drained, so `(true, 0)` is a fact about a queue that was full.
    assert_eq!(
        pairs.last().copied(),
        Some((false, 0)),
        "and the turn's end is in the window too, so these frames are a complete \
         story rather than a snapshot that happened to catch the right one: \
         {pairs:?}"
    );
    drop(tmp);
}

/// **A FAILOVER MOVES THE PLACEMENT CELL.**
///
/// The real failover: the thread starts where the cursor puts it, readings arrive
/// that put it over its own threshold, and the admission gate moves it on its first
/// send.
///
/// **THIS PATH IS COVERED TWICE, AND THE MUTATION SWEEP IS WHAT ESTABLISHED IT.**
/// `move_thread` calls `persist_thread_account` (which emits
/// `Fact::ThreadPlacement`) and THEN `emit_status` (whose `account` the fold also
/// reads). Deleting EITHER leaves this test green, because the other still moves
/// the cell. That is not a hole in the test — it is the design: the fact sits at
/// the write so the paths with no edge are covered (see the next test), and the
/// edge covers the paths where placement is decided.
///
/// So the mutation is a TWO-PART one, and it is the only way to make this contract
/// red. A test whose green survives deleting one of two redundant writers is worth
/// having precisely because it says the redundancy is real.
///
/// **THE MUTATION (both, together).** Delete the `if let Some(a) = account {
/// row.account.emit(at, Some(a.clone())); }` block from the `Push::Status` arm of
/// `Threads::folds`, AND the `self.emit_thread_facts(&state.id);` line from
/// `ThreadManager::persist_thread_account`.
///
/// Forced red:
/// ```text
/// thread 'a_failover_announces_the_placement' panicked at
/// lib/threads/tests/thread_shape_writes.rs:343:5:
/// the placement moved and the cell did not follow: ["first"] (started on
/// `first`, the column now says `second`)
/// ```
#[tokio::test]
async fn a_failover_announces_the_placement() {
    let (m, tmp) = start(&["first", "second"]).await;
    let mut s = ThreadSettings::new(tmp.path().to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s.thresholds.session = Some(50.0);
    let id = m.create_thread(None, s).unwrap();
    let started = m.store.get_thread(&id).unwrap().unwrap().account.unwrap();
    let elsewhere = if started == "first" { "second" } else { "first" };

    let mut f = watch(&m, 31, &id).await;
    // The readings that make the gate move it: its own account is over the
    // thread's threshold and the other one is clear.
    m.budgets
        .observe(&started, threads::gauge::Axis::Session, 90.0, None, "usage_poll")
        .unwrap();
    m.budgets
        .observe(elsewhere, threads::gauge::Axis::Session, 5.0, None, "usage_poll")
        .unwrap();

    m.send(
        &id,
        "hello".into(),
        threads::event::SendIntent::Immediate,
        Value::Null,
        false,
    )
    .unwrap();
    let moved = until(|| {
        m.store
            .get_thread(&id)
            .ok()
            .flatten()
            .and_then(|r| r.account)
            .map(|a| a != started)
            .unwrap_or(false)
    })
    .await;
    assert!(moved, "the admission gate never moved the thread");
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let frames = drain(&mut f);
    let placements: Vec<String> = live_rows(&frames, &id)
        .iter()
        .filter_map(|r| r["account"].as_str().map(str::to_string))
        .collect();
    assert!(
        placements.iter().any(|a| a == elsewhere),
        "the placement moved and the cell did not follow: {placements:?} \
         (started on `{started}`, the column now says `{}`)",
        m.store
            .get_thread(&id)
            .unwrap()
            .unwrap()
            .account
            .unwrap_or_default()
    );
    drop(tmp);
}

/// **A BOOT-TIME PLACEMENT CORRECTION ANNOUNCES ITSELF — THE PATH WITH NO EDGE.**
///
/// This is `Fact::ThreadPlacement`'s own test, and the path it covers is the one
/// the plan named and nothing else reaches: `get_state`'s drift correction. A
/// thread this process has not adopted is rehydrated from the store, its settings
/// re-resolve to a different login than the column records, and
/// `persist_thread_account` rewrites the column — at BOOT, with no `Status` edge
/// anywhere near it. Before the fact, that write was completely silent, and a
/// consumer holding the thread's key kept the old placement until it re-subscribed.
///
/// The drift is created the way the daemon itself creates it: the durable column is
/// written to disagree with the thread's PIN (a pin is absolute and is re-resolved
/// on every rehydration — "that IS the self-healing case"), and a SECOND manager
/// over the same store is what makes the thread un-adopted. The trigger is
/// `append_custom`, deliberately: it goes through `get_state` and emits no
/// `Status`, so the fact is the only thing that can carry the correction.
///
/// **THE MUTATION.** Delete the `self.emit_thread_facts(&state.id);` line from
/// `ThreadManager::persist_thread_account`.
///
/// Forced red, re-measured 2026-09-26:
/// ```text
/// the drift correction rewrote the placement column and the shape was never
/// told: held "first", column "second"
/// ```
///
/// **The two values used to be recorded the other way round.** They are the whole
/// content of this red: `held` is what the MIRROR has (the drifted `first`, from
/// the hydrate) and `column` is what the STORE now says (the healed `second`).
/// Printed in reverse it describes a mirror that ran AHEAD of the store, which is
/// not a failure this code can have, and a reader checking their own red against
/// it would conclude they had reproduced something else.
#[tokio::test]
async fn a_boot_time_placement_correction_announces_itself() {
    let (m, tmp) = start(&["first", "second"]).await;
    let mut s = ThreadSettings::new(tmp.path().to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s.account = threads::settings::AccountChoice::Named("second".into());
    let id = m.create_thread(None, s.clone()).unwrap();
    assert_eq!(
        m.store.get_thread(&id).unwrap().unwrap().account.as_deref(),
        Some("second"),
        "a pin places the thread where it points"
    );

    // The durable row now DISAGREES with the pin, which is exactly the state a
    // rehydration is there to heal. Written through the store so no fact is
    // emitted for it: the correction below is what must be announced.
    let row = m.store.get_thread(&id).unwrap().unwrap();
    m.store
        .update_thread_settings(
            &id,
            &row.settings_json,
            Some("first"),
            row.cwd.as_deref().unwrap_or_default(),
            row.model.as_deref().unwrap_or_default(),
        )
        .unwrap();

    // A SECOND manager over the same store: this one has never adopted the
    // thread, so `get_state` will rehydrate it and find the drift.
    let m2 = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts: BTreeMap::from([
            ("first".to_string(), tmp.path().join("first").to_string_lossy().to_string()),
            ("second".to_string(), tmp.path().join("second").to_string_lossy().to_string()),
        ]),
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();
    let hub: Arc<Hub> = m2.hub.clone();
    hub.register(tshape::Threads, m2.clone());

    let mut f = m2.hub.connect(33);
    m2.hub.subscribe("threads", Some(&id), 33, None).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let opening = drain(&mut f);
    let export = opening
        .iter()
        .find(|fr| fr.wire.op == Op::Read)
        .and_then(|fr| fr.wire.row.clone())
        .expect("an export");
    assert_eq!(
        export["account"],
        json!("first"),
        "the hydrate reads the column as it stands, drift and all: {export}"
    );

    // `append_custom` goes through `get_state` — which rehydrates, finds the
    // drift, and rewrites the column — and emits NO `Status`.
    m2.append_custom(&id, "failed_input".into(), json!({"x": 1}), false)
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    let column = m2
        .store
        .get_thread(&id)
        .unwrap()
        .unwrap()
        .account
        .unwrap_or_default();
    assert_eq!(
        column, "second",
        "the rehydration must have healed the column back to the pin, or this test \
         is not about a drift correction at all"
    );
    let held = live_rows(&drain(&mut f), &id)
        .iter()
        .filter_map(|r| r["account"].as_str().map(str::to_string))
        .last()
        .unwrap_or_else(|| "<no live row at all>".to_string());
    assert_eq!(
        held, column,
        "the drift correction rewrote the placement column and the shape was never \
         told: held {held:?}, column {column:?}"
    );
    drop(tmp);
}

/// **A COMPACTION MOVES THE MARKER, AND CARRIES THE RANGE IT SWALLOWED.**
///
/// Both durable compaction writes — `interpret`'s `Compacted` arm, which records
/// the boundary and raises `needs_restate`, and `build_prompt`, which consumes the
/// restatement and lowers it again — emitted nothing of their own. §12's whole
/// point is that an agent can recover state after an uncontrolled compaction, and
/// the range is the thing it needs; a consumer had to walk the ledger for it.
///
/// The `range` travels ON the fact rather than being folded out of
/// `ThreadEvent::Compacted`, which is what keeps the cell to one writer.
///
/// **THE MUTATION.** Delete the `self.emit_compaction_fact(..)` call from the
/// `ThreadEvent::Compacted` arm of `ThreadManager::interpret`.
///
/// Forced red:
/// ```text
/// a compaction moved the marker and the shape was never told: no live row
/// carried needs_restate
/// ```
#[tokio::test]
async fn a_compaction_moves_the_marker_and_carries_its_range() {
    let (m, tmp) = start(&["first"]).await;
    let id = thread_on(&m, tmp.path());
    let mut f = watch(&m, 32, &id).await;

    m.send(
        &id,
        "compactexit please".into(),
        threads::event::SendIntent::Immediate,
        Value::Null,
        false,
    )
    .unwrap();
    let compacted = until(|| {
        m.store
            .get_thread(&id)
            .ok()
            .flatten()
            .map(|r| r.needs_restate)
            .unwrap_or(false)
    })
    .await;
    assert!(compacted, "the fake turn never produced a compaction boundary");
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let frames = drain(&mut f);
    let marker = live_rows(&frames, &id)
        .into_iter()
        .map(|r| r["compaction"].clone())
        .find(|c| c["needs_restate"] == json!(true))
        .unwrap_or_else(|| {
            panic!(
                "a compaction moved the marker and the shape was never told: no live \
                 row carried needs_restate among {} frames",
                frames.len()
            )
        });
    assert_eq!(
        marker["last_compaction_message_id"],
        json!(m
            .store
            .get_thread(&id)
            .unwrap()
            .unwrap()
            .last_compaction_message_id),
        "the cell must carry the boundary the store recorded, not a guess: {marker}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// THE ONE TEST THAT NEEDS A SOCKET
// ─────────────────────────────────────────────────────────────────────────────

/// **THE `settings` CELL IS BYTE-IDENTICAL TO WHAT `threads.get` PUTS ON THE
/// WIRE.**
///
/// This test goes through a real websocket, and the reason is the whole point of
/// it: the contract is not "the cell equals `settings_of(row)`" — that would be a
/// test describing the implementation, which this codebase has shipped four times.
/// The contract is **the cell equals what the RPC says**, because AUI's
/// `sync_thread_settings` hashes its desired blob, and on a mismatch `threads.get`s
/// and diffs six fields as literal `serde_json::Value` comparisons. A daemon that
/// normalised, reordered, defaulted-in or re-typed any of them on read-back would
/// put AUI into the same `threads.update` on every single turn. So the assertion
/// has to be against the other answer, not against this shape's own idea of it.
///
/// It is asserted on the EXPORT and again after a LIVE update, because those are
/// different code paths — `Handles<ThreadRowRead>` and `Fact::ThreadSettings` —
/// and a drift between them would be invisible to a test that checked only one.
///
/// **THE THIRD ASSERTION IS THE ONE WITH TEETH, AND THE MUTATION SWEEP IS WHY IT
/// EXISTS.** The obvious mutation — make `settings_of` round-trip through the typed
/// struct, which is exactly the refactor a future reader would think was tidier —
/// leaves the first two assertions GREEN, and the reason is worth writing down:
/// `ThreadSettings` carries **no `skip_serializing_if` on any field**, and the
/// stored blob is itself `to_string(&ThreadSettings)`, so `from_str` then
/// `to_value` reproduces it byte for byte for every blob this daemon writes. The
/// round trip is provably identity *on a canonical blob*.
///
/// It is NOT identity on a blob containing something the struct does not know
/// about — a row written by a newer daemon, or `import.rs`'s AUI header — because
/// serde drops unknown fields silently. That is the real risk, so the test writes
/// such a blob and asserts on it. The RPC reparses and keeps the key; a
/// round-tripping shape would lose it, and the two answers a consumer diffs would
/// disagree about a field neither of them owns.
///
/// **THE MUTATION.** Make `shapes::threads::settings_of` round-trip through the
/// typed struct:
/// `serde_json::to_value(serde_json::from_str::<crate::settings::ThreadSettings>(&row.settings_json).unwrap()).unwrap_or(Value::Null)`
///
/// Forced red:
/// ```text
/// a blob this daemon cannot fully parse must reach the cell UNCHANGED, exactly
/// as `threads.get` reparses it — serde drops what it does not know, and a
/// consumer diffing the two would see a field vanish.
/// ```
#[tokio::test]
async fn the_settings_cell_is_byte_identical_to_threads_get() {
    let (m, tmp) = start(&["first"]).await;
    let id = thread_on(&m, tmp.path());

    // **THE OTHER ANSWER, SINCE R2.S3c (R4.S9, HOLDS R9).** This read the
    // `threads/{thread_id}/get` request, which is retired. Its `settings` was
    // exactly `serde_json::from_str::<Value>(&row.settings_json)` (server.rs
    // `thread_json`), so the reference is now that same reparse of the stored
    // string, read straight off the store — the claim (the cell is the STORED
    // blob, not a typed round trip of it) is unchanged.
    fn settings_over_the_wire(m: &threads::ThreadManager, id: &str) -> Value {
        let row = m.store.get_thread(id).unwrap().expect("the thread exists");
        serde_json::from_str::<Value>(&row.settings_json).unwrap_or(Value::Null)
    }

    let rpc = settings_over_the_wire(&m, &id);
    assert!(rpc.is_object(), "threads.get answered no settings blob: {rpc}");

    let mut f = m.hub.connect(40);
    m.hub.subscribe("threads", Some(&id), 40, None).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let frames = drain(&mut f);
    let export = frames
        .iter()
        .find(|fr| fr.wire.op == Op::Read)
        .and_then(|fr| fr.wire.row.clone())
        .expect("an export");

    assert_eq!(
        export["settings"], rpc,
        "the `settings` cell must be byte-identical to `threads.get`'s, or AUI \
         patches every turn.\n  shape: {}\n    rpc: {}",
        export["settings"], rpc
    );

    // ── AND AGAIN ON THE LIVE PATH, which is different code. ────────────────
    let mut patch = threads::settings::SettingsPatch::default();
    patch.model = Some("opus".to_string());
    m.update_settings(&id, patch).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    let rpc2 = settings_over_the_wire(&m, &id);
    let live = live_rows(&drain(&mut f), &id)
        .pop()
        .expect("a live row after the settings write");
    assert_eq!(rpc2["model"], json!("opus"), "the RPC moved: {rpc2}");
    assert_eq!(
        live["settings"], rpc2,
        "the LIVE path must agree with the RPC too — the fact carries the stored \
         blob, not the patch, and not a re-serialisation of it.\n  shape: {}\n    \
         rpc: {}",
        live["settings"], rpc2
    );

    // ── AND A BLOB THIS DAEMON CANNOT FULLY PARSE. ─────────────────────────
    //
    // `import.rs` writes `metadata.aui_import`; a rolled-back daemon reads rows a
    // newer one wrote. Both put keys in `settings_json` that `ThreadSettings` has
    // no field for, and serde drops those silently on a round trip.
    let row = m.store.get_thread(&id).unwrap().unwrap();
    let mut blob: Value = serde_json::from_str(&row.settings_json).unwrap();
    blob["a_field_from_a_newer_daemon"] = json!({"kept": true});
    m.store
        .update_thread_settings(
            &id,
            &serde_json::to_string(&blob).unwrap(),
            row.account.as_deref(),
            row.cwd.as_deref().unwrap_or_default(),
            row.model.as_deref().unwrap_or_default(),
        )
        .unwrap();

    let rpc3 = settings_over_the_wire(&m, &id);
    assert_eq!(
        rpc3["a_field_from_a_newer_daemon"],
        json!({"kept": true}),
        "`threads.get` reparses the stored string, so it keeps the key: {rpc3}"
    );
    // **THE SUBSCRIPTION IS DROPPED FIRST, AND THAT IS RULE 2.5 RATHER THAN A
    // TEST TRICK.** On a TRACKED shape a re-subscribe RE-EXPORTS what the cells
    // already hold; it does not re-hydrate, because `do_subscribe` calls
    // `hydrates_key` only for a key the root does not have. The write above went
    // straight to the store and emitted no fact, so the held value is correctly
    // still the old one. Dropping the last subscription clears the root
    // (`Tracking::Lazy`, no grace period), so the next subscribe is a genuine
    // re-read — which is what this assertion is about.
    m.hub.unsubscribe("threads", Some(&id), 40);
    m.hub.dropped(40);
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let mut f2 = m.hub.connect(41);
    m.hub.subscribe("threads", Some(&id), 41, None).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let export3 = drain(&mut f2)
        .iter()
        .find(|fr| fr.wire.op == Op::Read)
        .and_then(|fr| fr.wire.row.clone())
        .expect("an export");
    assert_eq!(
        export3["settings"], rpc3,
        "a blob this daemon cannot fully parse must reach the cell UNCHANGED, \
         exactly as `threads.get` reparses it — serde drops what it does not know, \
         and a consumer diffing the two would see a field vanish.\n  shape: {}\n \
         rpc: {}",
        export3["settings"], rpc3
    );
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
