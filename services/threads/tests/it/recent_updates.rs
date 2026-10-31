//! # `threads/recent-updates` — the index, proved
//!
//! Ruling 3's second thread shape. The constraints these tests exist to hold
//! are the ones that decided the design, and each has a test that can tell the
//! design apart from the one it replaced:
//!
//! | constraint | test |
//! |---|---|
//! | it HYDRATES, so a cold subscriber gets history | `the_index_hydrates_so_a_cold_subscriber_gets_the_order_it_missed` |
//! | it is a REORDER, aggregated by thread_id, not a log | `activity_reorders_the_index_and_does_not_append_to_it` |
//! | a VISIT is the consumer's fact — `recent-visited` stays dropped | `a_seen_moves_the_roll_up_and_never_the_order` |
//! | it carries NO thread content | `the_index_carries_ids_an_order_and_a_roll_up_and_no_row` |
//! | it INFORMS; the app subscribes | `the_index_hands_out_pool_keys_and_opens_nothing` |
//! | `retain` bounds it, which is Part 1 meeting Part 2 | `a_retain_count_on_the_index_shows_the_newest_n` |
//! | a RENAME does not reorder — the operator named this one | `a_rename_does_not_reorder_the_index` |
//! | a non-user event does not reorder but DOES move the roll-up | `a_non_user_event_does_not_reorder_but_still_moves_the_roll_up` |
//! | a user message DOES reorder, and is the only door in | `a_user_message_moves_the_thread_to_the_front` |
//! | the live order and the hydrate order AGREE across a re-init | `the_hydrate_order_and_the_live_order_agree_across_a_reinit` |
//! | a thread he has never spoken in is in NEITHER half | `a_thread_he_has_never_spoken_in_holds_no_place_in_the_index` |
//! | a bulk event that changes nothing costs no frame | `a_bulk_event_that_changes_nothing_costs_no_frame` |
//!
//! **The bottom four are one directive**, quoted verbatim: *"the recents need to
//! be ordered on last user input, not on when they were created or any other
//! events, including renames."* The last of them exists because an index has TWO
//! halves — the live fold and the durable read it re-hydrates from — and fixing
//! only the fold leaves them disagreeing at every re-initialisation, which is
//! invisible until a reconnect.
//!
//! **Every test here was forced red by the mutation named in its own docstring,
//! and the red was checked for `error[E` before it was believed — and the
//! RESTORE was checked for green, because a red nobody can undo is not
//! attributable to the mutation.**
//!
//! **Every assertion is on a frame a CONNECTION RECEIVED.** This repo's standing
//! trap is asserting that a mechanism ran rather than that anything arrived at a
//! consumer holding the key; an index is exactly the shape where that mistake is
//! easy, because its whole state is one row on one key and a test could read it
//! off `shapes.list` and prove nothing about delivery.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use threads::config::Config;
use threads::event::{Fact, Push, StoredEvent, ThreadEvent};
use threads::shape::{ConnFrames, Derived, Hub, Op, Retain, ShapeFrame};
use threads::shapes::recent::{FLEET, RECENT_CAPACITY};
use threads::{build_manager, ThreadManager, ThreadSettings};

const SHAPE: &str = "threads/recent-updates";

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

fn config(tmp: &std::path::Path) -> Config {
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "first".to_string(),
        tmp.join("first").to_string_lossy().to_string(),
    );
    Config {
        data_dir: Some(tmp.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

/// The index is nested under `threads`, and `register` demands parents-first —
/// so the pool is registered here too, exactly as the daemon does it.
async fn start() -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    // THE CLOCK SEAM. Every `Hub::emit` stamps with the clock and every cell
    // refuses an input at or before its `folded_at`, so two emits in one
    // microsecond mean the second is DROPPED — which is what an optimized
    // build does to a tight emit loop. See `src/clock.rs`.
    threads::clock::testing::ticking();
    let m = build_manager(config(tmp.path())).unwrap();
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(threads::shapes::threads::Threads, m.clone());
    hub.register(threads::shapes::recent::Recent, m.clone());
    (m, tmp)
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}

fn drain(f: &mut ConnFrames) -> Vec<Arc<ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

/// **A THREAD HE HAS SPOKEN IN** — which is the only kind that holds a place in
/// this index.
///
/// Operator directive, verbatim: *"the recents need to be ordered on last user
/// input, not on when they were created or any other events, including
/// renames."* A thread with no last user input has no position in such an order,
/// so `Store::recent_threads` excludes it and the fold never admits it. Creating
/// a thread is therefore NOT enough to put it in the index, and a fixture that
/// only created would be measuring the empty window in every test below.
///
/// The stamp goes in through `Store::bump_last_seq` with a `Some(..)` — the same
/// statement, with the same argument, that `ThreadManager::emit_resolving` runs
/// for a real `user_message` append. `a_bare_thread` below is the other half:
/// created and never spoken in.
fn a_thread(m: &ThreadManager, tmp: &std::path::Path) -> String {
    let id = a_bare_thread(m, tmp);
    let spoke_at = chrono::Utc::now().to_rfc3339();
    m.store.bump_last_seq(&id, 0, Some(&spoke_at)).unwrap();
    id
}

/// A thread that EXISTS and that he has never spoken in.
fn a_bare_thread(m: &ThreadManager, tmp: &std::path::Path) -> String {
    let mut s = ThreadSettings::new(tmp.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    m.create_thread(None, s).unwrap()
}

/// **A `user_message` ON THE WIRE** — the one event that moves this order.
///
/// Addressable, like the real one: `ThreadManager::send_declaring` allocates a
/// message id for it, so a fixture that left it `None` would be testing a
/// `user_message` nobody can address.
fn he_spoke(id: &str, seq: u64, message_id: u64) -> Push {
    Push::Event(StoredEvent {
        thread_id: id.to_string(),
        seq,
        message_id: Some(message_id),
        ts: chrono::Utc::now().to_rfc3339(),
        event: ThreadEvent::UserMessage {
            text: "he said something".to_string(),
            intent: Default::default(),
            metadata: Value::Null,
        },
    })
}

/// One ledger event for `id` that is NOT a user message — through the hub on the
/// same path the daemon's own append emits it. A `custom` event, which is what
/// most of the 2.46 M-row ledger is made of.
fn an_event(id: &str, seq: u64, message_id: Option<u64>) -> Push {
    Push::Event(StoredEvent {
        thread_id: id.to_string(),
        seq,
        message_id,
        ts: chrono::Utc::now().to_rfc3339(),
        event: ThreadEvent::Custom {
            event_type: "assistant_suggestions".to_string(),
            data: json!({"seq": seq}),
        },
    })
}

/// The `threads` array off a frame a connection RECEIVED — export or live row,
/// whichever this frame is. Never off the root.
fn order_in(f: &ShapeFrame) -> Option<Vec<Value>> {
    let row = match (&f.wire.row, &f.wire.event) {
        (Some(r), _) => r.clone(),
        (None, Some(Derived::Row { row })) => row.clone(),
        _ => return None,
    };
    Some(row["threads"].as_array()?.clone())
}

/// The ids, in the order the frame carries them.
fn ids_in(f: &ShapeFrame) -> Vec<String> {
    order_in(f)
        .unwrap_or_default()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect()
}

/// The LAST frame this connection received that carries the order — the index's
/// current answer as delivered, not as held.
fn latest(frames: &[Arc<ShapeFrame>]) -> Arc<ShapeFrame> {
    frames
        .iter()
        .filter(|f| order_in(f).is_some())
        .next_back()
        .cloned()
        .unwrap_or_else(|| panic!("no frame carrying the order reached this connection"))
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. It hydrates — which is the whole reason it is a `Recency` and not a
//    `Sequence`
// ─────────────────────────────────────────────────────────────────────────────

/// **A COLD SUBSCRIBER RECEIVES THE ORDER IT MISSED, NOT AN EMPTY WINDOW.**
///
/// Three threads are created BEFORE anybody subscribes, so nothing this shape
/// could have folded has happened while it was tracking — a `Sequence` here
/// would be `since_init` from the instant of the subscribe and would export an
/// empty array, which is exactly why `threads` carries no messages cell. The
/// index reads the durable `updated_at DESC` order instead.
///
/// The order is asserted as NEWEST FIRST against the creation order, which is
/// the claim `shared.order` makes out loud.
///
/// **THE MUTATION.** In `shapes/recent.rs`, replace the `hydrates` body's
/// `threads.read(as_of, entries, complete)` with
/// `threads.mark_since_init(as_of)` — i.e. make it a `Sequence`-shaped shape
/// that starts empty, which is the design this one exists instead of.
///
/// Forced red:
/// ```text
/// a cold subscriber receives the durable order, newest first — an index that
/// hydrates to nothing is a `Sequence` with extra steps: []
/// ```
#[tokio::test]
async fn the_index_hydrates_so_a_cold_subscriber_gets_the_order_it_missed() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let a = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let c = a_thread(&m, tmp.path());

    // COLD: the first subscriber this shape has ever had.
    let mut f = hub.connect(91);
    hub.subscribe(SHAPE, Some(FLEET), 91, None).unwrap();
    settle().await;
    let frames = drain(&mut f);
    let got = ids_in(&latest(&frames));
    assert_eq!(
        got,
        vec![c.clone(), b.clone(), a.clone()],
        "a cold subscriber receives the durable order, newest first — an index \
         that hydrates to nothing is a `Sequence` with extra steps: {got:?}"
    );

    // And each entry carries the position it was ordered on, so the order is
    // CHECKABLE and not merely asserted.
    let entries = order_in(&latest(&frames)).unwrap();
    let stamps: Vec<&str> = entries.iter().map(|e| e["at"].as_str().unwrap()).collect();
    assert!(
        stamps.windows(2).all(|w| w[0] >= w[1]),
        "every entry carries the stamp it was ordered on, descending: {stamps:?}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. A reorder, aggregated by thread_id — not a log
// ─────────────────────────────────────────────────────────────────────────────

/// **ACTIVITY MOVES A THREAD TO THE FRONT; IT DOES NOT APPEND A SECOND ENTRY.**
///
/// The operator's own words for this shape were *"a recency sequence across
/// fleet, maybe that AGGREGATED BY THREAD_ID so it's just a REORDER of recently
/// active threads"*, and the aggregation is load-bearing rather than tidy: a
/// `Sequence` fed one item per event would fill its window with the busiest
/// thread and evict every cold one the index exists to surface. Three events on
/// the OLDEST thread is the case that tells the two apart — under a log it is
/// three new entries and a length of six, under a reorder it is one entry at the
/// front and a length of three.
///
/// **THE MUTATION.** In `fold.rs`, delete the dedupe from `Recency::touch` —
/// remove the `if let Some(pos) = self.items.iter().position(..) {
/// self.items.remove(pos); }` — which makes it an append-only log wearing a
/// recency name.
///
/// Forced red:
/// ```text
/// a touch MOVES a member; three events on one thread is one entry at the front
/// and not three more: 3
/// ```
#[tokio::test]
async fn activity_reorders_the_index_and_does_not_append_to_it() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let a = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let c = a_thread(&m, tmp.path());

    let mut f = hub.connect(92);
    hub.subscribe(SHAPE, Some(FLEET), 92, None).unwrap();
    settle().await;
    let start_order = ids_in(&latest(&drain(&mut f)));
    assert_eq!(start_order, vec![c.clone(), b.clone(), a.clone()]);

    // The OLDEST thread is spoken in three times. **IT HAS TO BE A
    // `user_message`**, because that is the only event that moves this order at
    // all — see `he_spoke`'s own docstring and the module doc's directive.
    for seq in 1..=3u64 {
        m.hub.emit(he_spoke(&a, seq, seq));
    }
    settle().await;

    let frames = drain(&mut f);
    let after = latest(&frames);
    let ids = ids_in(&after);
    assert_eq!(
        ids.len(),
        3,
        "a touch MOVES a member; three events on one thread is one entry at the \
         front and not three more: {}",
        ids.len()
    );
    assert_eq!(
        ids,
        vec![a.clone(), c.clone(), b.clone()],
        "and the member that moved is at the FRONT, with the rest in their old \
         relative order: {ids:?}"
    );

    // **AND IT ARRIVED AS A `row`, NOT AS A `transition`.** A reorder spelled as
    // an appended item would hand a mirror moves to accumulate and never the
    // order itself — which is the whole reason `Recency` emits none.
    let transitions: Vec<&Arc<ShapeFrame>> = frames
        .iter()
        .filter(|x| matches!(x.wire.event, Some(Derived::Transition { .. })))
        .collect();
    assert!(
        transitions.is_empty(),
        "a reorder is not an appended item, so this shape emits no transition: \
         {transitions:?}"
    );
    drop(tmp);
}

/// **THE ORDER IS BOUNDED BY CAPACITY, AND THE OLDEST MEMBER IS WHAT LEAVES.**
///
/// `RECENT_CAPACITY + 4` distinct threads are touched. The index holds
/// `RECENT_CAPACITY` — which is the number a consumer read off `shapes.list` as
/// `max.retained_items` before it asked, so the bound it sees and the bound the
/// producer keeps are one const read once at `register`.
///
/// **THE MUTATION.** In `fold.rs`, delete `Recency::touch`'s
/// `while self.items.len() > self.capacity { self.items.pop_back(); }` — the
/// unbounded index, which is `Retention::All` arriving through a different door.
///
/// Forced red:
/// ```text
/// the index holds its declared capacity and no more: 36
/// ```
#[tokio::test]
async fn the_order_is_bounded_by_the_capacity_the_shape_declares() {
    // **ON THE CLOCK SEAM, because the STAMP RULE makes a tie a DROP.** Every
    // `Hub::emit` stamps with the clock, and `Recent::folds` refuses an input
    // stamped at or before the cell's `folded_at` — correctly, since that is how
    // an event already in flight when the hydrate ran is kept from moving the
    // order backwards. Under an optimized build `RECENT_CAPACITY + 4` emits in a
    // tight loop collided on the microsecond, so the refusal swallowed half of
    // them and the index held 17, 18, 19 or 20 of the 32 it was asked for —
    // a different number almost every run. The seam is installed by `start()`,
    // for every test in this file: real time that may not repeat, so every emit
    // carries a distinct stamp and the loop is as deterministic as it reads.
    // See `src/clock.rs`.
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let mut f = hub.connect(93);
    hub.subscribe(SHAPE, Some(FLEET), 93, None).unwrap();
    settle().await;
    drain(&mut f);

    let ids: Vec<String> = (0..RECENT_CAPACITY + 4)
        .map(|_| a_bare_thread(&m, tmp.path()))
        .collect();
    for (i, id) in ids.iter().enumerate() {
        m.hub.emit(he_spoke(id, (i + 1) as u64, (i + 1) as u64));
    }
    settle().await;

    let held = ids_in(&latest(&drain(&mut f)));
    assert_eq!(
        held.len(),
        RECENT_CAPACITY,
        "the index holds its declared capacity and no more: {}",
        held.len()
    );
    assert_eq!(
        held[0], ids[ids.len() - 1],
        "and the newest touch is at the front: {:?}",
        &held[..3]
    );
    assert!(
        !held.contains(&ids[0]),
        "the OLDEST member is the one that left: {:?}",
        ids[0]
    );

    let published = hub
        .list()
        .into_iter()
        .find(|i| i.name == SHAPE)
        .expect("the shape")
        .max
        .retained_items;
    assert_eq!(
        published, RECENT_CAPACITY,
        "and the bound a consumer reads off `shapes.list` is the same const: \
         {published}"
    );
    drop(tmp);
}

/// **AN EVENT THE STAMP RULE REFUSES COSTS NO FRAME.**
///
/// The guide's *"a beat that repeats the held level produces no frame"*, arriving
/// on this shape as the hydration edge: an event already in flight when the
/// hydrate read ran is stamped before the cell's `folded_at`, `Recency::touch`
/// refuses it, and the order does not move. **That refusal has to be decided
/// through `get`, before `Map::key`** — `key` dirties on reach, so a guard
/// placed after it has already cost the `row` frame it exists to avoid, and the
/// consumer would receive a frame carrying an order identical to the one it
/// already holds.
///
/// A frame that says nothing is not harmless on this shape: it is the whole
/// 32-entry order re-sent, and the fold sees every one of a 2.46 M-row ledger's
/// events.
///
/// **THE MUTATION.** In `shapes/recent.rs`'s `Push::Event` arm, move the
/// `folded_at` guard to AFTER `tx.pools.key(FLEET.to_string())` — the guard that
/// is correct about the cell and too late about the row.
///
/// Forced red:
/// ```text
/// an event the stamp rule refuses must cost NO frame — `key` dirties on reach,
/// so the guard has to run before it: 1
/// ```
#[tokio::test]
async fn an_event_the_stamp_rule_refuses_costs_no_frame() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    let a = a_thread(&m, tmp.path());

    let mut f = hub.connect(100);
    hub.subscribe(SHAPE, Some(FLEET), 100, None).unwrap();
    settle().await;
    let first = ids_in(&latest(&drain(&mut f)));
    assert_eq!(first, vec![a.clone()], "the hydrate landed");

    // Stamped in 2020: an event that was already in flight when the hydrate read
    // ran. `Hub::emit_at` takes the stamp from the caller, which is the same
    // door a genuinely late event arrives through.
    // A `user_message` deliberately: it is the one event that WOULD move this
    // order, so the only thing that can stop it here is the stamp rule, and a
    // green cannot come from the fold having declined it for some other reason.
    m.hub.emit_at(
        "2020-01-01T00:00:00Z".parse().unwrap(),
        he_spoke(&a, 1, 1),
    );
    settle().await;

    let after: Vec<Arc<ShapeFrame>> = drain(&mut f)
        .into_iter()
        .filter(|x| order_in(x).is_some())
        .collect();
    assert_eq!(
        after.len(),
        0,
        "an event the stamp rule refuses must cost NO frame — `key` dirties on \
         reach, so the guard has to run before it: {}",
        after.len()
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. A VISIT is the consumer's fact — `recent-visited` stays dropped
// ─────────────────────────────────────────────────────────────────────────────

/// **BEING READ CHANGES THE ROLL-UP AND NEVER THE ORDER.**
///
/// `recent-visited` — an index of the threads a consumer had VISITED — was
/// designed and DROPPED: a visit is a consumer-side fact, the producer has no
/// business knowing it, and a shape shared by many consumers cannot hold one
/// `visited` value per viewer. `Fact::ThreadSeen` is the nearest thing this
/// producer has to a visit, so it is the place that ruling becomes mechanical: a
/// `seen` on the LAST thread in the order must leave it last.
///
/// The roll-up moving is asserted in the same breath, because "it changed
/// nothing" would pass a test that proved the fold had simply stopped.
///
/// **TWO MUTATIONS, both forced.** (a) In `shapes/recent.rs`, make the
/// `Fact::ThreadSeen` arm use `threads.touch(..)` instead of `threads.set(..)` —
/// the "a seen is activity" reading, which is `recent-visited` re-introduced one
/// field at a time. Red: *"a `seen` is something a CONSUMER did — it must not
/// move the order"*. (b) In `fold.rs`, make `Recency::set` leave the cell state
/// alone — the reading that says *"nothing about the order moved, so nothing
/// about the cell moved"*, which is half right and hands a consumer a changed row
/// beside a stale `folded_at`. Red: *"the cell FOLDED an input, so its
/// `folded_at` advanced to that input"*.
///
/// **(b) CAME BACK GREEN THE FIRST TIME**, against a weaker version of this test
/// that asserted `folded_at` was merely later than the member's own position
/// stamp — which the earlier `touch` in this same test had already made true. The
/// assertion is pinned to the instant the seen was emitted now, which is the only
/// stamp that separates this act from the one before it.
#[tokio::test]
async fn a_seen_moves_the_roll_up_and_never_the_order() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let a = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let c = a_thread(&m, tmp.path());

    let mut f = hub.connect(94);
    hub.subscribe(SHAPE, Some(FLEET), 94, None).unwrap();
    settle().await;
    let before = ids_in(&latest(&drain(&mut f)));
    assert_eq!(before, vec![c.clone(), b.clone(), a.clone()]);

    // HE SPEAKS in the OLDEST thread: it moves to the front and its unread goes
    // to one. That is the one act this order is made of.
    m.hub.emit(he_spoke(&a, 1, 1));
    settle().await;
    let mid = latest(&drain(&mut f));
    assert_eq!(ids_in(&mid), vec![a.clone(), c.clone(), b.clone()]);
    let unread_a = order_in(&mid).unwrap()[0]["unread"].as_u64().unwrap();
    assert_eq!(unread_a, 1, "an addressable event above the cursor is unread");

    // Now a SEEN on the thread at the BACK. If a seen were activity it would
    // come to the front; it must not.
    //
    // The instant BEFORE the fact is emitted, so the cell-state assertion below
    // is against this act and not against the earlier event's stamp — an
    // assertion that merely said `folded_at` was "recent" would pass on the
    // touch that came before it, which is a green that proves nothing.
    let before_the_seen = chrono::Utc::now();
    m.hub.emit_fact(Fact::ThreadSeen {
        thread_id: b.clone(),
        seen_seq: 7,
        unread: 0,
    });
    settle().await;
    let after = latest(&drain(&mut f));
    let order = ids_in(&after);
    assert_eq!(
        order,
        vec![a.clone(), c.clone(), b.clone()],
        "a `seen` is something a CONSUMER did — it must not move the order: \
         {order:?}"
    );

    // …and it DID land, so this is not a test that the fold stopped.
    let entry = order_in(&after)
        .unwrap()
        .into_iter()
        .find(|e| e["id"] == json!(b))
        .expect("the thread that was seen is still in the order");
    assert_eq!(
        entry["seen_seq"],
        json!(7),
        "the roll-up moved in place: {entry}"
    );

    // ── AND THE MEMBER'S `at` DID NOT MOVE WHILE THE CELL'S `folded_at` DID ──
    //
    // Two stamps answering two different questions, and this act is the one
    // place they come apart. The member's `at` is its POSITION, so advancing it
    // would claim a reorder that did not happen and leave the printed order
    // disagreeing with the stamps printed beside it. The cell's `folded_at` is
    // WHEN THIS CELL LAST FOLDED AN INPUT, and it just folded one — leaving it
    // behind would hand a consumer a changed row beside an unmoved cell state
    // and let it conclude the row is older than it is.
    let at_before = order_in(&mid)
        .unwrap()
        .into_iter()
        .find(|e| e["id"] == json!(b))
        .expect("held before the seen")["at"]
        .clone();
    assert_eq!(
        entry["at"], at_before,
        "a consumer's act must not move the member's POSITION: {entry}"
    );

    // `cells` rides only on an export, so re-subscribe to read the cell state
    // the way a consumer would — off a frame, not off the root.
    let mut g = hub.connect(101);
    hub.subscribe(SHAPE, Some(FLEET), 101, None).unwrap();
    settle().await;
    let cells = drain(&mut g)
        .iter()
        .find_map(|x| x.wire.cells.clone())
        .expect("an export carries per-cell provenance");
    let folded: chrono::DateTime<chrono::Utc> = cells["threads"]["folded_at"]
        .as_str()
        .expect("the order's cell state is known")
        .parse()
        .expect("a stamp");
    // `>=`, not `>` (R9.F2). `Hub::emit_fact` stamps with the ticking seam
    // since R9.F2b, but `before_the_seen` is a RAW read taken immediately
    // before the emit, and the ticking clock hands out the real instant when
    // it has not been handed out before — so the two can still be equal. Two
    // reads on this machine's microsecond clock land in the SAME microsecond whenever the
    // emit path is quick enough, and then `folded_at == before_the_seen` with
    // the fold entirely correct: measured twice (`…077902 UTC` both sides in
    // R9.A2's inspect; `…674895` in `src/clock.rs`'s history). A strict `>`
    // made the test a race on the scheduler. What the assertion has to tell
    // apart is "folded THIS input" from "still holding the previous fold", and
    // the previous fold is the `he_spoke` emit, a full `settle()` (300 ms)
    // BEFORE `before_the_seen` — so `>=` keeps the whole discrimination.
    assert!(
        folded >= before_the_seen,
        "the cell FOLDED an input, so its `folded_at` advanced to that input — a \
         changed row beside an unmoved cell state lets a consumer conclude the \
         row is older than it is: folded_at={folded}, the seen was emitted at \
         {before_the_seen}"
    );
    drop(tmp);
}

/// **`Hub::emit_fact` STAMPS THROUGH THE CLOCK SEAM** (R9.F2b, ledger R9.F2-2).
///
/// `src/clock.rs` clause 2 puts every shape stamp inside the seam, because a
/// stamp tied with a cell's `folded_at` is a DROPPED input. `emit`,
/// `emit_fact_resolving` and `emit_push_resolving` all stamp with
/// `crate::clock::now`; `emit_fact` alone went to the chassis hub's default
/// clock, raw `chrono::Utc::now`, so neither `testing::ticking` nor a freeze
/// reached it.
///
/// The probe is a FREEZE: under `freeze_at(t)` the only clock that answers `t`
/// is the seam, so the cell's `folded_at` equals `t` exactly when the stamp came
/// from the seam, and is the real wall time otherwise. Two different frozen
/// instants, each read back, so a constant stamp cannot pass. Both are an hour
/// ahead so each fold is strictly after the one before it.
///
/// **Forced red** at aa8832a1 (`emit_fact` → `self.core.emit(..)`, the raw hub
/// clock): *"emit_fact must stamp with crate::clock::now (clause 2): under
/// freeze_at(2026-10-17 10:59:18 UTC) the cell folded at 2026-10-17
/// 09:59:18.360043 UTC"*.
#[tokio::test]
async fn emit_fact_is_stamped_by_the_clock_seam() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    let b = a_thread(&m, tmp.path());

    let mut f = hub.connect(95);
    hub.subscribe(SHAPE, Some(FLEET), 95, None).unwrap();
    settle().await;
    assert_eq!(ids_in(&latest(&drain(&mut f))), vec![b.clone()]);

    let base = chrono::Utc::now().timestamp() + 3600;
    for (i, at) in [base, base + 7].into_iter().enumerate() {
        let at = chrono::DateTime::from_timestamp(at, 0).unwrap();
        {
            let _frozen = threads::clock::testing::freeze_at(at);
            m.hub.emit_fact(Fact::ThreadSeen {
                thread_id: b.clone(),
                seen_seq: 10 + i as u64,
                unread: 0,
            });
        }
        // `cells` rides only on an export: re-subscribe and wait for one whose
        // roll-up shows THIS seen, so the read is of the fold and not of a
        // frame that arrived before it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut conn = 200 + 100 * i as u64;
        let folded = loop {
            conn += 1;
            let mut g = hub.connect(conn);
            hub.subscribe(SHAPE, Some(FLEET), conn, None).unwrap();
            settle().await;
            let frames = drain(&mut g);
            let seen_now = frames
                .iter()
                .filter_map(|x| order_in(x))
                .flatten()
                .any(|e| e["id"] == json!(b) && e["seen_seq"] == json!(10 + i as u64));
            let cells = frames.iter().find_map(|x| x.wire.cells.clone());
            if let (true, Some(cells)) = (seen_now, cells) {
                break cells["threads"]["folded_at"]
                    .as_str()
                    .expect("the order's cell state is known")
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .expect("a stamp");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "seen #{i} never folded into an export within 20 s"
            );
        };
        assert_eq!(
            folded, at,
            "emit_fact must stamp with crate::clock::now (clause 2): under \
             freeze_at({at}) the cell folded at {folded}"
        );
    }
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. It carries no thread content, and it opens no subscription
// ─────────────────────────────────────────────────────────────────────────────

/// **IDS, AN ORDER, AND A TWO-INTEGER ROLL-UP — AND NOTHING THAT IS A ROW.**
///
/// *"Two indices over one pool must cost two orderings, not two copies."* The
/// entry's field set is asserted EXACTLY rather than by absence of a blacklist,
/// because a blacklist passes the moment somebody adds a field nobody thought to
/// forbid — and the thing that would be added is always the convenient one (a
/// title, a status, the settings blob at ~6.8 KB a row).
///
/// **THE MUTATION.** In `shapes/recent.rs`, add `"settings": p.settings` — or
/// any field carrying content — to `RecentEntry`. The equivalent applied here
/// was adding `title: Option<String>` to `RecentEntry` with `None`.
///
/// Forced red:
/// ```text
/// an index entry is an id, a position and a roll-up: ["at", "id", "seen_seq", "title", "unread"]
/// ```
#[tokio::test]
async fn the_index_carries_ids_an_order_and_a_roll_up_and_no_row() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    let a = a_thread(&m, tmp.path());

    let mut f = hub.connect(95);
    hub.subscribe(SHAPE, Some(FLEET), 95, None).unwrap();
    settle().await;
    let frame = latest(&drain(&mut f));

    let row = frame.wire.row.clone().expect("an export carrying the row");
    let mut row_fields: Vec<&str> = row.as_object().unwrap().keys().map(String::as_str).collect();
    row_fields.sort_unstable();
    assert_eq!(
        row_fields,
        vec!["pool", "threads"],
        "the index's row is the pool it is over and the order — nothing else: \
         {row_fields:?}"
    );

    let entries = order_in(&frame).unwrap();
    assert_eq!(entries.len(), 1, "one thread exists");
    let mut fields: Vec<&str> = entries[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        vec!["at", "id", "seen_seq", "unread"],
        "an index entry is an id, a position and a roll-up: {fields:?}"
    );
    assert_eq!(entries[0]["id"], json!(a));
    drop(tmp);
}

/// **IT INFORMS; IT DOES NOT SUBSCRIBE.**
///
/// > *"Those indices can inform the app which from the pool of threads to
/// > subscribe to. We don't need each of the indices to facilitate the actual
/// > subscription process."*
///
/// So the test is the protocol, end to end: subscribing to the INDEX must open
/// nothing on the POOL, and the ids it hands out must be pool keys the app can
/// then subscribe to itself. Reference expansion — the producer opening the
/// second subscription for you — was built and deleted for exactly this, so the
/// absence is asserted rather than assumed.
///
/// **THE MUTATION.** In `Hub::subscribe_ask`, after the item is pushed, follow
/// the shape's declared `PARENT` and enqueue the same subscribe on it —
/// reference expansion in miniature, and the "save the consumer a round trip"
/// version of an index. It is the environment executing the agent's policy,
/// which is what rule 4 is a test for.
///
/// Forced red:
/// ```text
/// subscribing to the INDEX must open nothing on the POOL — the app makes the
/// second call, and that is the point: 1
/// ```
#[tokio::test]
async fn the_index_hands_out_pool_keys_and_opens_nothing() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    let a = a_thread(&m, tmp.path());

    let mut f = hub.connect(96);
    hub.subscribe(SHAPE, Some(FLEET), 96, None).unwrap();
    settle().await;
    let ids = ids_in(&latest(&drain(&mut f)));
    assert_eq!(ids, vec![a.clone()]);

    let pool_subscribers = hub
        .list()
        .into_iter()
        .find(|i| i.name == "threads")
        .expect("the pool")
        .subscribers;
    assert_eq!(
        pool_subscribers, 0,
        "subscribing to the INDEX must open nothing on the POOL — the app makes \
         the second call, and that is the point: {pool_subscribers}"
    );

    // **AND THE IDS IT HANDS OUT ARE POOL KEYS.** An index whose ids the pool
    // does not recognise would inform nobody of anything.
    let mut p = hub.connect(97);
    hub.subscribe("threads", Some(&ids[0]), 97, None)
        .expect("an id off the index is a key the pool serves");
    settle().await;
    let got = drain(&mut p);
    let row = got
        .iter()
        .find(|x| x.wire.op == Op::Read && x.wire.key.as_deref() == Some(a.as_str()))
        .and_then(|x| x.wire.row.clone())
        .expect("the pool answers that key with a row");
    assert_eq!(
        row["thread_id"],
        json!(a),
        "and the row is the thread the index named: {row}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. Part 1 meeting Part 2
// ─────────────────────────────────────────────────────────────────────────────

/// **`retain: n` ON THE INDEX IS "THE n MOST RECENT", WHICH IS THE ASK IT
/// EXISTS FOR.**
///
/// *"the twenty most recently updated"* is a `retain` on this shape, and it is
/// why the index hydrating matters: without a hydrate the answer to a cold
/// `retain: 1` would be nothing rather than the one thread. The wide subscriber
/// on the same root still sees all three from the same emit — the who-pays claim,
/// asserted here on the real shape rather than only on a fixture.
///
/// **THE MUTATION.** In `shapes/recent.rs`, delete the `Root::row_at` override
/// so the shape falls back to the default that ignores `retain` — the shape that
/// declares a ceiling and then serves everybody the ceiling, which is the pairing
/// the chassis cannot check.
///
/// Forced red:
/// ```text
/// `retain: 1` on the index is the single most recently updated thread: 3
/// ```
#[tokio::test]
async fn a_retain_count_on_the_index_shows_the_newest_n() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let _a = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let _b = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let c = a_thread(&m, tmp.path());

    let mut wide = hub.connect(98);
    hub.subscribe(SHAPE, Some(FLEET), 98, None).unwrap();
    let mut narrow = hub.connect(99);
    hub.subscribe_ask(
        SHAPE,
        Some(FLEET),
        99,
        None,
        threads::shape::Detail::Rows,
        Retain::Count(1),
        threads::shape::Beat::Max,
    )
    .unwrap();
    settle().await;

    let got = ids_in(&latest(&drain(&mut narrow)));
    assert_eq!(
        got.len(),
        1,
        "`retain: 1` on the index is the single most recently updated thread: {}",
        got.len()
    );
    assert_eq!(got[0], c, "and it is the newest one: {got:?}");

    let wide_got = ids_in(&latest(&drain(&mut wide)));
    assert_eq!(
        wide_got.len(),
        3,
        "and the subscriber that asked for nothing still receives the ceiling — \
         a narrow ask is paid for by nobody: {wide_got:?}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. WHAT THE ORDER IS: LAST USER INPUT, AND NOTHING ELSE
//
//    Operator directive, verbatim: *"the recents need to be ordered on last
//    user input, not on when they were created or any other events, including
//    renames."* The four tests below are that sentence's four claims — a rename
//    does not reorder, a non-user event does not reorder but DOES still update
//    the roll-up, a user message DOES reorder, and the two halves of the order
//    (the live fold and the durable hydrate) agree across a re-initialisation.
//
//    **Every order here is asserted POSITIVELY and EXACTLY** — `assert_eq!` on
//    the whole vector, never "x is not before y". A negative assertion about an
//    order goes silent the moment its subject is renamed out from under it, and
//    the subject of these tests is an id the fixture mints.
// ─────────────────────────────────────────────────────────────────────────────

/// **THE ORDER THIS CONSUMER HOLDS**, which is not the same as "the order on the
/// last frame it received".
///
/// A fold that correctly does NOTHING sends nothing, so a drain that brings no
/// order-carrying frame means *the order did not change* — and `latest` panics on
/// that case rather than answering it. Reading the held order through here is
/// what lets the assertions below stay positive and exact (`the order is
/// [c,b,a]`) in the case where the right behaviour is silence.
fn held(prev: &[String], frames: &[Arc<ShapeFrame>]) -> Vec<String> {
    frames
        .iter()
        .filter(|f| order_in(f).is_some())
        .next_back()
        .map(|f| ids_in(f))
        .unwrap_or_else(|| prev.to_vec())
}

/// The `unread` this frame carries for one thread, or `None` if the frame does
/// not hold that thread at all.
fn unread_of(f: &ShapeFrame, id: &str) -> Option<u64> {
    order_in(f)?
        .into_iter()
        .find(|e| e["id"] == json!(id))?["unread"]
        .as_u64()
}

/// **A RENAME DOES NOT REORDER THE INDEX** — the regression the operator named
/// by name.
///
/// > *"the recents need to be ordered on last user input, not on when they were
/// > created or any other events, **including renames**."*
///
/// A rename in this daemon is a HEADER WRITE: `ThreadManager::update_settings`
/// runs `store::update_thread_settings` (which advances `threads.updated_at`) and
/// then `emit_thread_facts`, which puts `Fact::ThreadHeader` on the wire. The fold
/// had an arm for exactly that fact and it called `Recency::touch`, on the
/// reasoning that a header write advances `updated_at` and so was "activity" —
/// correct about the column, wrong about the question.
///
/// The rename is done to the OLDEST thread in the order, because that is the only
/// position from which a wrong answer is visible: a rename of the newest thread
/// leaves it newest either way.
///
/// **IT IS ASSERTED THAT THE RENAME LANDED**, through the POOL's `header` cell on
/// a frame a second connection received. Without that half this test would pass
/// against a daemon that had stopped emitting `Fact::ThreadHeader` altogether —
/// a green from a deleted emit, which this repo has shipped before. The pool's
/// `header` cell is fed by that fact and by nothing else, so it goes red if the
/// emit goes away.
///
/// **THE MUTATION.** In `shapes/recent.rs`, restore the arm this change deleted:
/// move `Input::Fact(Fact::ThreadHeader { .. })` out of the homed-elsewhere list
/// and give it back its `touch` on the thread's held entry.
///
/// Forced red, and it is red TWICE over — a real rename puts a header fact AND a
/// `settings_changed` ledger event on the wire, so the unconditional-`touch` fold
/// (mutation B1 on `a_non_user_event_…`) moves the thread here as well:
/// ```text
/// assertion `left == right` failed: a RENAME must not reorder the index —
/// operator directive, verbatim: "not any other events, including renames":
///   left:  ["d33ebfdc…", "6fb53525…", "d1c07899…"]
///   right: ["6fb53525…", "d1c07899…", "d33ebfdc…"]
/// ```
/// — the thread that was LAST is now first, which is the whole regression.
#[tokio::test]
async fn a_rename_does_not_reorder_the_index() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let a = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let c = a_thread(&m, tmp.path());

    let mut f = hub.connect(110);
    hub.subscribe(SHAPE, Some(FLEET), 110, None).unwrap();
    // The POOL, on the renamed thread's key — the witness that the header fact
    // actually went out.
    let mut pool = hub.connect(111);
    hub.subscribe("threads", Some(&a), 111, None).unwrap();
    settle().await;
    let before = ids_in(&latest(&drain(&mut f)));
    assert_eq!(
        before,
        vec![c.clone(), b.clone(), a.clone()],
        "the fixture requires the renamed thread to start LAST — a rename of the \
         newest thread leaves it newest either way: {before:?}"
    );
    drain(&mut pool);

    // ── THE RENAME ───────────────────────────────────────────────────────────
    let (_blob, visible) = m
        .update_settings(
            &a,
            threads::settings::SettingsPatch {
                model: Some("opus".to_string()),
                ..Default::default()
            },
        )
        .expect("the rename takes");
    settle().await;

    let order = held(&before, &drain(&mut f));
    assert_eq!(
        order,
        vec![c.clone(), b.clone(), a.clone()],
        "a RENAME must not reorder the index — operator directive, verbatim: \
         \"not any other events, including renames\": got {order:?}"
    );

    // ── …AND THE RENAME LANDED, so this is not a test of a deleted emit ──────
    assert!(
        visible.is_expected(),
        "the fixture requires a tracking fold, or the header fact was never put \
         on the wire at all: {visible:?}"
    );
    let header_model = drain(&mut pool)
        .iter()
        .filter_map(|x| order_in_pool(x))
        .filter_map(|row| row["header"]["model"].as_str().map(str::to_string))
        .next_back();
    assert_eq!(
        header_model.as_deref(),
        Some("opus"),
        "the POOL's header cell must carry the rename — it is fed by \
         `Fact::ThreadHeader` and by nothing else, so this is what makes the \
         assertion above a statement about the INDEX rather than about a fact \
         nobody emitted: {header_model:?}"
    );
    drop(tmp);
}

/// The whole row off a POOL frame a connection received — the pool's rows are
/// shaped differently from the index's, so this is a second reader and not
/// `order_in`.
fn order_in_pool(f: &ShapeFrame) -> Option<Value> {
    match (&f.wire.row, &f.wire.event) {
        (Some(r), _) => Some(r.clone()),
        (None, Some(Derived::Row { row })) => Some(row.clone()),
        _ => None,
    }
}

/// **A NON-USER EVENT DOES NOT REORDER, AND STILL MOVES THE ROLL-UP.**
///
/// This is the half of the fix that is easy to overshoot. The fold called
/// `Recency::touch` from a generic `Push::Event` arm, so ANY ledger event
/// reordered the band — a `stream_event` (58% of a 2.46 M-row ledger), a tool
/// result, a turn ending. The repair is to stop those events MOVING the entry, not
/// to stop folding them: an addressable event above the seen cursor is still one
/// unread, the pool's own `unread` cell is fed from this same site with the same
/// arithmetic, and a consumer watching a busy thread it has not read still wants
/// the number.
///
/// So both claims are asserted in one pass, and the roll-up half is why: *"it
/// changed nothing"* would pass a test against a fold that had simply stopped.
///
/// The second half of the test is the OTHER edge of `Recency::set` — a thread the
/// window does not hold. He has never spoken in it, so it has no position in a
/// last-user-input order; `set` answers `false` for it and the order must be
/// untouched, not extended.
///
/// **TWO MUTATIONS, both forced.**
///
/// (a) In `shapes/recent.rs`'s `Push::Event` arm, call `cell.touch(..)`
/// unconditionally instead of branching on `se.event.is_user_message()` — the fold
/// as it was. Five of the fourteen tests here go red under it:
/// ```text
/// assertion `left == right` failed: a non-user event must not reorder — the order
/// is last USER input, and a `custom` event is not him speaking:
///   left:  ["3de19573…", "1f2d473e…", "69e8aeab…"]
///   right: ["1f2d473e…", "69e8aeab…", "3de19573…"]
/// ```
///
/// (b) In the same arm, `return` for every non-user event so none reaches a cell
/// at all — the overshoot, an index that stops counting unreads. It is red here
/// and in `a_bulk_event_that_changes_nothing_costs_no_frame`'s second half, and
/// nowhere else:
/// ```text
/// assertion `left == right` failed: and it DID land, so this is not a test that
/// the fold stopped: 0
///   left: 0
///   right: 1
/// ```
#[tokio::test]
async fn a_non_user_event_does_not_reorder_but_still_moves_the_roll_up() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let a = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let c = a_thread(&m, tmp.path());

    let mut f = hub.connect(112);
    hub.subscribe(SHAPE, Some(FLEET), 112, None).unwrap();
    settle().await;
    let before = ids_in(&latest(&drain(&mut f)));
    assert_eq!(before, vec![c.clone(), b.clone(), a.clone()]);

    // ── AN ADDRESSABLE, NON-USER EVENT ON THE OLDEST THREAD ─────────────────
    m.hub.emit(an_event(&a, 1, Some(1)));
    settle().await;
    let frames = drain(&mut f);
    let order = held(&before, &frames);
    assert_eq!(
        order,
        vec![c.clone(), b.clone(), a.clone()],
        "a non-user event must not reorder — the order is last USER input, and a \
         `custom` event is not him speaking: {order:?}"
    );

    let unread = frames
        .iter()
        .filter(|x| order_in(x).is_some())
        .next_back()
        .and_then(|x| unread_of(x, &a))
        .unwrap_or(0);
    assert_eq!(
        unread, 1,
        "and it DID land, so this is not a test that the fold stopped: {unread}"
    );

    // ── AND A THREAD THE WINDOW DOES NOT HOLD IS NOT ADMITTED BY ONE ─────────
    //
    // He has never spoken in `d`, so it has no position in a last-user-input
    // order. `Recency::set` answers `false` for a member it does not hold and the
    // fold declines to reach for the key at all.
    let d = a_bare_thread(&m, tmp.path());
    m.hub.emit(an_event(&d, 1, Some(1)));
    settle().await;
    let order = held(&order, &drain(&mut f));
    assert_eq!(
        order,
        vec![c, b, a],
        "a non-user event on a thread he has never spoken in must not put it in \
         the order — there is no position to give it: {order:?}"
    );
    drop(tmp);
}

/// **A USER MESSAGE DOES REORDER** — the one act this index is an order of.
///
/// Both directions of it, because the fold has two outcomes and a test of one is
/// not a test of the other:
///
/// * a thread ALREADY in the window moves to the front (`touch` lifts a held
///   member), and its `at` — the position stamp the export carries beside it —
///   advances, so the printed order and the printed stamps still agree;
/// * a thread NOT in the window is ADMITTED by it (`touch` inserts), which is the
///   only door into this index and the reason a thread appears here when he first
///   speaks in it rather than when it is created.
///
/// **THE MUTATION.** In `shapes/recent.rs`'s `Push::Event` arm, invert the branch
/// — `if spoke { cell.set(..) } else { cell.touch(..) }` — the fix applied
/// backwards. It is a LOUD mutation: eight of the fourteen tests in this file go
/// red under it, which is the right outcome and is noted so a reader does not
/// expect a surgical one.
///
/// Forced red:
/// ```text
/// assertion `left == right` failed: a `user_message` moves the thread he spoke in
/// to the FRONT:
///   left:  ["9a8527eb…", "a0dd6da6…", "1f772ffd…"]
///   right: ["1f772ffd…", "9a8527eb…", "a0dd6da6…"]
/// ```
#[tokio::test]
async fn a_user_message_moves_the_thread_to_the_front() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let a = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b = a_thread(&m, tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let c = a_thread(&m, tmp.path());

    let mut f = hub.connect(113);
    hub.subscribe(SHAPE, Some(FLEET), 113, None).unwrap();
    settle().await;
    let first = latest(&drain(&mut f));
    let before = ids_in(&first);
    assert_eq!(before, vec![c.clone(), b.clone(), a.clone()]);
    let at_before = order_in(&first)
        .unwrap()
        .into_iter()
        .find(|e| e["id"] == json!(a))
        .expect("held before he spoke")["at"]
        .clone();

    // ── HE SPEAKS IN THE OLDEST THREAD ───────────────────────────────────────
    m.hub.emit(he_spoke(&a, 1, 1));
    settle().await;
    let moved = latest(&drain(&mut f));
    let order = ids_in(&moved);
    assert_eq!(
        order,
        vec![a.clone(), c.clone(), b.clone()],
        "a `user_message` moves the thread he spoke in to the FRONT: {order:?}"
    );

    let entry = order_in(&moved)
        .unwrap()
        .into_iter()
        .find(|e| e["id"] == json!(a))
        .expect("still held");
    assert_ne!(
        entry["at"], at_before,
        "and its POSITION STAMP advanced with it — the export's `at` pairs are \
         what make the order checkable rather than asserted, so a move that left \
         the stamp behind would print an order its own stamps contradict: {entry}"
    );

    // ── AND A USER MESSAGE IS THE DOOR IN ───────────────────────────────────
    let d = a_bare_thread(&m, tmp.path());
    m.hub.emit(he_spoke(&d, 1, 1));
    settle().await;
    let order = ids_in(&latest(&drain(&mut f)));
    assert_eq!(
        order,
        vec![d, a, c, b],
        "a thread he has never spoken in is ADMITTED by his first message, and \
         that is the only door into this index: {order:?}"
    );
    drop(tmp);
}

/// The index with a TWO-DEEP input FIFO, so a burst overflows it and the shape
/// task re-initialises — the same deterministic technique `reinit_shrink.rs`,
/// `nesting.rs` and `pool_membership.rs` use. `Hub::register` hard-codes
/// `FIFO_CAPACITY`, so this goes through the core hub to name its own.
async fn start_with_a_tiny_fifo() -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    threads::clock::testing::ticking();
    let m = build_manager(config(tmp.path())).unwrap();
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(threads::shapes::threads::Threads, m.clone());
    hub.core()
        .register(threads::shapes::recent::Recent, m.clone(), 2);
    (m, tmp)
}

/// This shape's epoch as the hub reports it — `reinit` is the counter that says a
/// re-initialisation really happened.
fn epoch_of(hub: &Arc<Hub>) -> threads::shape::Epoch {
    hub.list()
        .into_iter()
        .find(|i| i.name == SHAPE)
        .expect("the shape is registered")
        .epoch
}

/// Something for the FIFO to overflow with that this shape folds as a NO-OP, so
/// the flood itself cannot change the order it is being used to re-read.
fn an_info() -> Push {
    Push::Info {
        domain: "test".into(),
        reason: "flood".into(),
        account: None,
        detail: None,
        at: chrono::Utc::now().to_rfc3339(),
    }
}

/// **THE HYDRATE ORDER AND THE LIVE FOLD ORDER AGREE ACROSS A RE-INITIALISATION**
/// — the half-fix trap, tested directly.
///
/// An index has two halves that can disagree: the live fold, which reorders on an
/// event, and the durable read the shape hydrates from, which it runs again on
/// every re-init. **Fixing only the fold leaves them disagreeing at every
/// re-initialisation**, and nothing is visibly wrong until a reconnect — at which
/// point the band silently re-sorts on last-any-event. That is a divergence no
/// test of the fold alone can see, so this one re-initialises for real and
/// compares.
///
/// The fixture is built so the two possible answers are DIFFERENT:
///
/// * he speaks in `a`, then `c`, then `b` — through `ThreadManager::send`, the real
///   path, so one act writes `threads.last_user_input_at` AND puts the
///   `user_message` on the wire. Last-user-input order is `[b, c, a]`.
/// * then `a` gets a pile of NON-user events, which is what a working thread
///   mostly produces. Every one of them advances `threads.updated_at` through
///   `bump_last_seq`, so `updated_at` order has `a` at the FRONT.
///
/// So `[b, c, a]` is the right answer and `a`-first is the wrong one, and the two
/// are told apart by one assertion.
///
/// **It is `send` and not a hand-written pair of writes**, deliberately. If
/// `emit_resolving` stopped passing the recency stamp to `bump_last_seq`, a
/// fixture that wrote the column itself would stay green while production broke;
/// this one goes red, because the post-re-init order is read back out of the very
/// column that call is the only writer of.
///
/// **THE RE-INIT IS ASSERTED TO HAVE HAPPENED** (`epoch.reinit` moved), because
/// without that this test is two reads of one live cell and proves nothing at all.
///
/// **THE MUTATION.** In `store::recent_threads`, put the read back the way it was
/// — `SELECT id, updated_at FROM threads ORDER BY updated_at DESC LIMIT ?1`, with
/// no `IS NOT NULL` filter — i.e. fix the fold and leave the hydrate. **Twelve of
/// the fourteen tests in this file stay GREEN under it**, which IS the half-fix
/// trap: the only two that move are this one and
/// `a_thread_he_has_never_spoken_in_holds_no_place_in_the_index`.
///
/// Forced red:
/// ```text
/// assertion `left == right` failed: the order a RE-INITIALISED subscriber
/// receives must be the order the live fold held — a hydrate on `updated_at`
/// re-sorts the band on last-any-event at every reconnect:
///   live  ["ffd7cd47…", "5d3453a9…", "09fa0aa3…"]
///   after ["09fa0aa3…", "ffd7cd47…", "5d3453a9…"]
/// ```
/// — `09fa0aa3…` is the thread that got the pile of non-user events, and the
/// re-init put it at the FRONT.
///
/// **A SECOND MUTATION, also forced**, because this test's other job is to notice
/// if the column stops being written at all: in `ThreadManager::emit_resolving`,
/// replace `ev.event.is_user_message().then(|| ev.ts.clone())` with `None`. It goes
/// red on the LIVE assertion — the re-init that the sends themselves provoke
/// re-reads an empty column and empties the cell, so the band loses threads he
/// spoke in:
/// ```text
/// assertion `left == right` failed: the LIVE fold orders on last user input, so
/// the busiest thread is still last:
///   left:  ["255522c2…", "6470dfa6…"]
///   right: ["255522c2…", "6470dfa6…", "13beb2b9…"]
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hydrate_order_and_the_live_order_agree_across_a_reinit() {
    let (m, tmp) = start_with_a_tiny_fifo().await;
    let hub: Arc<Hub> = m.hub.clone();

    let a = a_bare_thread(&m, tmp.path());
    let b = a_bare_thread(&m, tmp.path());
    let c = a_bare_thread(&m, tmp.path());

    let mut f = hub.connect(114);
    hub.subscribe(SHAPE, Some(FLEET), 114, None).unwrap();
    settle().await;
    // Whatever the cold hydrate answered is NOT asserted here — that claim is
    // `a_thread_he_has_never_spoken_in_holds_no_place_in_the_index`'s, and making
    // it a PRECONDITION of this test would short-circuit the one comparison this
    // test exists for: a hydrate reading the wrong column would trip the
    // precondition and never reach the order.
    let cold = ids_in(&latest(&drain(&mut f)));

    // ── HE SPEAKS, THROUGH THE REAL PATH, IN a THEN c THEN b ────────────────
    for id in [&a, &c, &b] {
        m.send(
            id,
            "something he said".to_string(),
            threads::SendIntent::Immediate,
            json!({}),
            false,
        )
        .expect("the send takes");
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
    settle().await;

    // ── AND `a` IS THE BUSY ONE, so `updated_at` and `last_user_input_at`
    //    DISAGREE and the test can tell which one answered ────────────────────
    for i in 0..12 {
        m.append_custom(&a, "assistant_suggestions".into(), json!({"i": i}), i % 2 == 0)
            .expect("a non-user append");
    }
    settle().await;

    let live = held(&cold, &drain(&mut f));
    assert_eq!(
        live,
        vec![b.clone(), c.clone(), a.clone()],
        "the LIVE fold orders on last user input, so the busiest thread is still \
         last: {live:?}"
    );

    // ── THE RE-INIT ─────────────────────────────────────────────────────────
    //
    // **`REINIT_FLOOR_SECS` IS WHY THIS LOOPS.** `absorb_lost` refuses to re-init
    // within five seconds of the last one — the floor that closes the re-init
    // livelock under sustained overload — and the sends above drive a real turn
    // through a two-deep FIFO, so the fixture has usually spent one re-init before
    // this point. Flooding once and asserting would then be asserting against the
    // floor rather than against the shape. So: flood, wait past the floor, and try
    // again, up to four times.
    let mut epoch = epoch_of(&hub);
    let reinits_before = epoch.reinit;
    for _ in 0..4 {
        for _ in 0..200 {
            m.hub.emit(an_info());
        }
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        epoch = epoch_of(&hub);
        if epoch.reinit > reinits_before {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5200)).await;
    }
    assert!(
        epoch.reinit > reinits_before,
        "the scenario REQUIRES a real re-initialisation — without one this test is \
         two reads of one live cell and says nothing about the durable order: \
         {epoch:?}"
    );

    // ── AND THE ORDER THE RE-INITIALISED SUBSCRIBER NOW HOLDS ───────────────
    let after = held(&live, &drain(&mut f));
    assert_eq!(
        after, live,
        "the order a RE-INITIALISED subscriber receives must be the order the live \
         fold held — a hydrate on `updated_at` re-sorts the band on last-any-event \
         at every reconnect:\n  live  {live:?}\n  after {after:?}"
    );
    // Stated absolutely as well as relatively: two halves that agreed on the
    // WRONG order would satisfy the comparison above and nothing else here.
    assert_eq!(
        after,
        vec![b, c, a],
        "…and the order both halves agree on is last user input: {after:?}"
    );
    drop(tmp);
}

/// **A THREAD HE HAS NEVER SPOKEN IN HOLDS NO PLACE IN THE INDEX** — the
/// directive's consequence for `NULL`, on both halves of the order.
///
/// An order on last user input has no position to offer a thread with no last
/// user input. Any position invented for it — bottom, top, by `created_at` —
/// would be an order on something the directive excludes. So
/// `Store::recent_threads` filters `last_user_input_at IS NULL`, and the fold
/// cannot admit one either because it moves a thread only on a `user_message`.
///
/// Both halves are checked here, because this is exactly the claim where they
/// could drift apart cheaply: the HYDRATE, read by a cold subscriber, and then
/// the same subscriber's live view after a non-user event on the same thread.
///
/// **IT IS ASSERTED THAT THE THREADS EXIST**, through the POOL's membership —
/// otherwise this test would pass against a `create_thread` that silently failed,
/// which is an empty index for a reason that has nothing to do with the order.
///
/// **THE MUTATION.** In `store::recent_threads`, drop the
/// `WHERE last_user_input_at IS NOT NULL` clause — so `NULL` sorts wherever
/// SQLite puts it and never-spoken threads pad the window.
///
/// Forced red:
/// ```text
/// assertion `left == right` failed: a cold subscriber is handed only the threads
/// he has actually spoken in — a never-spoken thread has no position in an order
/// on last user input: 3 entries
///   left:  ["1c701d27…", "654fcad5…", "d12a904a…"]
///   right: []
/// ```
#[tokio::test]
async fn a_thread_he_has_never_spoken_in_holds_no_place_in_the_index() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let x = a_bare_thread(&m, tmp.path());
    let y = a_bare_thread(&m, tmp.path());
    let z = a_bare_thread(&m, tmp.path());

    let mut f = hub.connect(115);
    hub.subscribe(SHAPE, Some(FLEET), 115, None).unwrap();
    settle().await;
    let cold = ids_in(&latest(&drain(&mut f)));
    assert_eq!(
        cold,
        Vec::<String>::new(),
        "a cold subscriber is handed only the threads he has actually spoken in — \
         a never-spoken thread has no position in an order on last user input: \
         {} entries",
        cold.len()
    );

    // ── AND THEY DO EXIST, so this is not an empty index for the wrong reason ─
    let existing = m.store.thread_ids(100).expect("the pool's membership");
    let mut want = vec![x.clone(), y.clone(), z.clone()];
    want.sort();
    let mut got: Vec<String> = existing.iter().filter(|i| want.contains(i)).cloned().collect();
    got.sort();
    assert_eq!(
        got, want,
        "the fixture requires three threads that EXIST and that he has not spoken \
         in — an index empty because nothing was created proves nothing: {got:?}"
    );

    // ── NOR DOES A NON-USER EVENT LET ONE IN ────────────────────────────────
    for seq in 1..=3u64 {
        m.hub.emit(an_event(&x, seq, Some(seq)));
    }
    m.hub.emit_fact(threads::event::Fact::ThreadSeen {
        thread_id: y.clone(),
        seen_seq: 1,
        unread: 0,
    });
    settle().await;
    let live = held(&cold, &drain(&mut f));
    assert_eq!(
        live,
        Vec::<String>::new(),
        "and the live fold admits nothing either — `Recency::set` answers `false` \
         for a member the window does not hold, and a `seen` was never a way in: \
         {live:?}"
    );
    drop(tmp);
}

/// **A BULK EVENT COSTS NO FRAME AT ALL** — the hot-path consequence of the fix,
/// and a cost regression this change could easily have shipped.
///
/// `stream_event` and its two `BULK_KINDS` siblings are 58% of a 2.46 M-row ledger
/// at ~50/s peak, and they carry no `message_id`. Before this change they reordered
/// the index, which was wrong but at least changed something. Now they change
/// NOTHING: the order is last user input, and a non-addressable event moves no
/// `unread` either. So the fold must decide that BEFORE `Map::key`, which dirties
/// on reach — otherwise every one of them sends a consumer the whole 32-entry order
/// with not one field different from the copy it already holds.
///
/// This is `an_event_the_stamp_rule_refuses_costs_no_frame`'s rule, arriving at a
/// second gate, and the module doc's *"a frame that says nothing is not harmless on
/// this shape"* is why it is asserted rather than assumed.
///
/// **It is asserted in the same breath that an ADDRESSABLE one DOES cost a frame**,
/// because "no frames" would otherwise pass against a fold that had stopped
/// folding ledger events altogether — which is `a_non_user_event_…`'s mutation (b)
/// wearing a different disguise.
///
/// **THE MUTATION.** In `shapes/recent.rs`'s `Push::Event` arm, delete the
/// `will_change` guard, so a non-user event always reaches `cell.set(..)` through
/// `Map::key`.
///
/// Forced red (ONE frame and not three, because the row debounce collapses the
/// burst — which is exactly why the count is the wrong thing to reason about and
/// the guard has to be in the fold):
/// ```text
/// assertion `left == right` failed: a non-addressable, non-user event changes
/// nothing, so it must cost NO frame — `Map::key` dirties on reach, and a frame
/// here is the whole 32-entry order re-sent: 1
///   left: 1
///   right: 0
/// ```
#[tokio::test]
async fn a_bulk_event_that_changes_nothing_costs_no_frame() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    let a = a_thread(&m, tmp.path());

    let mut f = hub.connect(116);
    hub.subscribe(SHAPE, Some(FLEET), 116, None).unwrap();
    settle().await;
    assert_eq!(ids_in(&latest(&drain(&mut f))), vec![a.clone()], "the hydrate landed");

    // ── THREE NON-ADDRESSABLE, NON-USER EVENTS ──────────────────────────────
    for seq in 1..=3u64 {
        m.hub.emit(an_event(&a, seq, None));
    }
    settle().await;
    let quiet = drain(&mut f)
        .into_iter()
        .filter(|x| order_in(x).is_some())
        .count();
    assert_eq!(
        quiet, 0,
        "a non-addressable, non-user event changes nothing, so it must cost NO \
         frame — `Map::key` dirties on reach, and a frame here is the whole \
         32-entry order re-sent: {quiet}"
    );

    // ── AND AN ADDRESSABLE ONE STILL DOES ───────────────────────────────────
    m.hub.emit(an_event(&a, 4, Some(4)));
    settle().await;
    let frames = drain(&mut f);
    let loud = frames.iter().filter(|x| order_in(x).is_some()).count();
    assert_eq!(
        loud, 1,
        "an ADDRESSABLE non-user event moves the roll-up, so it DOES cost a frame \
         — without this half, a fold that had stopped folding ledger events would \
         pass the assertion above: {loud}"
    );
    let unread = frames
        .iter()
        .filter(|x| order_in(x).is_some())
        .next_back()
        .and_then(|x| unread_of(x, &a));
    assert_eq!(
        unread,
        Some(1),
        "…and the frame carries the number that made it worth sending: {unread:?}"
    );
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
