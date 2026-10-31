//! # `threads/recent-visits` — the index of recent visits, proved
//!
//! The third thread shape (#H78, Q-9 item 8): a sibling of
//! `threads/recent-updates` ordered on VISIT time. Every assertion is on a frame
//! a CONNECTION RECEIVED, as in `tests/recent_updates.rs`, whose fixture this
//! file copies.
//!
//! | constraint | test |
//! |---|---|
//! | it HYDRATES the visit order; a never-visited thread holds no place | `a_cold_subscriber_receives_the_visit_order_it_missed` |
//! | a visit MOVES a thread to the front; it does not append | `a_visit_moves_the_thread_to_the_front_and_does_not_append` |
//! | an UPDATE — a `user_message` included — does not move this order | `an_update_does_not_move_the_visit_order` |
//! | a visit does NOT move `recent-updates` (the flagged converse) | `a_visit_does_not_reorder_recent_updates` |
//! | the live order and the hydrate order AGREE across a re-init | `the_hydrate_order_and_the_live_order_agree_across_a_reinit` |
//! | the entry is an id, a visit stamp and a cursor — no row, no roll-up | `the_entry_is_an_id_a_stamp_and_a_cursor` |
//!
//! Every test names the mutation that forced it red; the red and green runs are
//! pasted in an internal report.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use threads::config::Config;
use threads::event::{Push, StoredEvent, ThreadEvent};
use threads::shape::{ConnFrames, Derived, Hub, ShapeFrame};
use threads::shapes::recent_visits::FLEET;
use threads::{build_manager, ThreadManager, ThreadSettings};

const SHAPE: &str = "threads/recent-visits";
const UPDATES: &str = "threads/recent-updates";

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

/// Parents first, as the daemon registers: the pool, the updates index, then
/// this one. `recent-updates` is registered so the converse can be asserted.
async fn start() -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    threads::clock::testing::ticking();
    let m = build_manager(config(tmp.path())).unwrap();
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(threads::shapes::threads::Threads, m.clone());
    hub.register(threads::shapes::recent::Recent, m.clone());
    hub.register(threads::shapes::recent_visits::RecentVisits, m.clone());
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

fn a_thread(m: &ThreadManager, tmp: &std::path::Path) -> String {
    let mut s = ThreadSettings::new(tmp.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    m.create_thread(None, s).unwrap()
}

/// A thread he has SPOKEN in, so it holds a place in `recent-updates` — the
/// same column write `ThreadManager::emit_resolving` makes for a real
/// `user_message`.
fn a_spoken_thread(m: &ThreadManager, tmp: &std::path::Path) -> String {
    let id = a_thread(m, tmp);
    let spoke_at = threads::clock::now().to_rfc3339();
    m.store.bump_last_seq(&id, 0, Some(&spoke_at)).unwrap();
    id
}

/// **A VISIT, THROUGH THE REAL PATH** — `ThreadManager::mark_seen`, the one
/// site that both stamps `thread_seen.updated_at` and emits `Fact::ThreadSeen`.
fn visit(m: &ThreadManager, id: &str, seq: u64) {
    m.mark_seen(id, seq).expect("mark_seen takes");
}

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

fn order_in(f: &ShapeFrame) -> Option<Vec<Value>> {
    let row = match (&f.wire.row, &f.wire.event) {
        (Some(r), _) => r.clone(),
        (None, Some(Derived::Row { row })) => row.clone(),
        _ => return None,
    };
    Some(row["threads"].as_array()?.clone())
}

fn ids_in(f: &ShapeFrame) -> Vec<String> {
    order_in(f)
        .unwrap_or_default()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect()
}

/// The LAST frame of `shape` this connection received that carries an order.
fn latest_of(shape: &str, frames: &[Arc<ShapeFrame>]) -> Option<Arc<ShapeFrame>> {
    frames
        .iter()
        .filter(|f| f.shape == shape && order_in(f).is_some())
        .next_back()
        .cloned()
}

fn latest(frames: &[Arc<ShapeFrame>]) -> Arc<ShapeFrame> {
    latest_of(SHAPE, frames)
        .unwrap_or_else(|| panic!("no `{SHAPE}` frame carrying the order reached this connection"))
}

fn held(prev: &[String], frames: &[Arc<ShapeFrame>]) -> Vec<String> {
    latest_of(SHAPE, frames)
        .map(|f| ids_in(&f))
        .unwrap_or_else(|| prev.to_vec())
}

// ─────────────────────────────────────────────────────────────────────────────

/// **A COLD SUBSCRIBER RECEIVES THE VISIT ORDER, NOT THE CREATION ORDER, AND
/// NOT AN EMPTY WINDOW.**
///
/// Four threads are created `a, b, c, d`; three are visited `b, a, c` BEFORE
/// anybody subscribes, and `d` never is. Creation order would answer
/// `[d, c, b, a]`; the visit order is `[c, a, b]`, and `d` holds no place.
///
/// **MUTATIONS.** (1) In `recent_visits.rs` `hydrates`, replace
/// `threads.read(as_of, entries, complete)` with `threads.mark_since_init(as_of)`
/// → the cold order is `[]`. (2) In `Store::recent_visits`, `ORDER BY
/// s.updated_at ASC` → `[b, a, c]`.
#[tokio::test]
async fn a_cold_subscriber_receives_the_visit_order_it_missed() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let a = a_thread(&m, tmp.path());
    let b = a_thread(&m, tmp.path());
    let c = a_thread(&m, tmp.path());
    let d = a_thread(&m, tmp.path());
    for id in [&b, &a, &c] {
        visit(&m, id, 0);
    }

    let mut f = hub.connect(201);
    hub.subscribe(SHAPE, Some(FLEET), 201, None).unwrap();
    settle().await;
    let frames = drain(&mut f);
    let got = ids_in(&latest(&frames));
    assert_eq!(
        got,
        vec![c.clone(), a.clone(), b.clone()],
        "a cold subscriber receives the durable VISIT order, most recent first, \
         and a never-visited thread holds no place: {got:?}"
    );
    assert!(!got.contains(&d), "never visited, never in the order");

    let entries = order_in(&latest(&frames)).unwrap();
    let stamps: Vec<&str> = entries.iter().map(|e| e["at"].as_str().unwrap()).collect();
    assert!(
        stamps.windows(2).all(|w| w[0] > w[1]),
        "every entry carries the visit stamp it was ordered on, strictly descending: \
         {stamps:?}"
    );
    drop(tmp);
}

/// **A VISIT MOVES A THREAD TO THE FRONT; IT DOES NOT APPEND A SECOND ENTRY.**
///
/// The oldest-visited thread is visited twice more, live. A reorder gives one
/// entry at the front and a length of three; a log would give five.
///
/// **MUTATION.** In `recent_visits.rs` `folds`, replace `touch(at, thread_id.clone(), ..)`
/// with `set(at, thread_id, ..)` → the order does not move.
#[tokio::test]
async fn a_visit_moves_the_thread_to_the_front_and_does_not_append() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let a = a_thread(&m, tmp.path());
    let b = a_thread(&m, tmp.path());
    let c = a_thread(&m, tmp.path());
    for id in [&a, &b, &c] {
        visit(&m, id, 0);
    }
    let mut f = hub.connect(202);
    hub.subscribe(SHAPE, Some(FLEET), 202, None).unwrap();
    settle().await;
    let before = ids_in(&latest(&drain(&mut f)));
    assert_eq!(before, vec![c.clone(), b.clone(), a.clone()]);

    visit(&m, &a, 0);
    visit(&m, &a, 0);
    settle().await;
    let after_frame = latest(&drain(&mut f));
    let after = ids_in(&after_frame);
    assert_eq!(
        after,
        vec![a.clone(), c.clone(), b.clone()],
        "a visit moves the thread to the front, once, and appends nothing: {after:?}"
    );
    drop(tmp);
}

/// **AN UPDATE — A `user_message` INCLUDED — DOES NOT MOVE THE VISIT ORDER.**
/// The brief's "order on update time instead of visit time → red", on the LIVE
/// half. He speaks in the back thread; `recent-updates` moves it (asserted, so
/// the event did land and this is not a stopped fold); this index does not.
///
/// **MUTATION.** In `recent_visits.rs` `folds`, move `Input::Push(Push::Event(_))`
/// out of the homed-elsewhere list into an arm that `touch`es `se.thread_id`.
#[tokio::test]
async fn an_update_does_not_move_the_visit_order() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let a = a_spoken_thread(&m, tmp.path());
    let b = a_spoken_thread(&m, tmp.path());
    let c = a_spoken_thread(&m, tmp.path());
    for id in [&a, &b, &c] {
        visit(&m, id, 0);
    }
    let mut f = hub.connect(203);
    hub.subscribe(SHAPE, Some(FLEET), 203, None).unwrap();
    hub.subscribe(UPDATES, Some(FLEET), 203, None).unwrap();
    settle().await;
    let cold = drain(&mut f);
    assert_eq!(ids_in(&latest(&cold)), vec![c.clone(), b.clone(), a.clone()]);

    m.hub.emit(he_spoke(&a, 1, 1));
    settle().await;
    let frames = drain(&mut f);
    let updates = latest_of(UPDATES, &frames)
        .expect("the update reached recent-updates — positive control");
    assert_eq!(
        ids_in(&updates)[0],
        a,
        "positive control: the update DID move recent-updates"
    );
    let visits = held(&[c.clone(), b.clone(), a.clone()], &frames);
    assert_eq!(
        visits,
        vec![c.clone(), b.clone(), a.clone()],
        "an update is not a visit — the visit order must not move: {visits:?}"
    );
    drop(tmp);
}

/// **A VISIT DOES NOT REORDER `recent-updates`** — the converse the spec asked
/// to be flagged rather than resolved silently. Both shapes on one connection:
/// visiting the thread at the BACK of the updates order moves it to the front
/// of THIS order (positive control) and leaves the updates order unchanged.
///
/// **MUTATION.** In `shapes/recent.rs`'s `Fact::ThreadSeen` arm, replace
/// `.set(at, thread_id, RecentEntry{..})` with
/// `.touch(at, thread_id.clone(), RecentEntry{..})`.
#[tokio::test]
async fn a_visit_does_not_reorder_recent_updates() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    let a = a_spoken_thread(&m, tmp.path());
    let b = a_spoken_thread(&m, tmp.path());
    let c = a_spoken_thread(&m, tmp.path());
    let mut f = hub.connect(204);
    hub.subscribe(SHAPE, Some(FLEET), 204, None).unwrap();
    hub.subscribe(UPDATES, Some(FLEET), 204, None).unwrap();
    settle().await;
    let cold = drain(&mut f);
    let updates_before = ids_in(&latest_of(UPDATES, &cold).expect("updates exported"));
    assert_eq!(updates_before, vec![c.clone(), b.clone(), a.clone()]);

    visit(&m, &a, 0);
    settle().await;
    let frames = drain(&mut f);
    let visits = ids_in(&latest(&frames));
    assert_eq!(visits[0], a, "positive control: the visit landed here: {visits:?}");
    let updates_after = latest_of(UPDATES, &frames)
        .map(|x| ids_in(&x))
        .unwrap_or_else(|| updates_before.clone());
    assert_eq!(
        updates_after, updates_before,
        "a VISIT must not move recent-updates' order — being seen is not an update"
    );
    drop(tmp);
}

async fn start_with_a_tiny_fifo() -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    threads::clock::testing::ticking();
    let m = build_manager(config(tmp.path())).unwrap();
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(threads::shapes::threads::Threads, m.clone());
    hub.core()
        .register(threads::shapes::recent_visits::RecentVisits, m.clone(), 2);
    (m, tmp)
}

fn epoch_of(hub: &Arc<Hub>) -> threads::shape::Epoch {
    hub.list()
        .into_iter()
        .find(|i| i.name == SHAPE)
        .expect("the shape is registered")
        .epoch
}

fn an_info() -> Push {
    Push::Info {
        domain: "test".into(),
        reason: "flood".into(),
        account: None,
        detail: None,
        at: chrono::Utc::now().to_rfc3339(),
    }
}

/// **THE HYDRATE ORDER AND THE LIVE ORDER AGREE ACROSS A RE-INITIALISATION** —
/// the half-fix trap `recent-updates` paid for, tested directly.
///
/// Visits go `a, c, b` (live order `[b, c, a]`), then `a` gets a pile of
/// non-user appends, which advance `threads.updated_at` — so an order on UPDATE
/// time would put `a` first. The re-init is asserted to have happened.
///
/// **MUTATION.** In `Store::recent_visits`, order on the thread's update time:
/// `ORDER BY t.updated_at DESC` → after the re-init `a` is at the front.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hydrate_order_and_the_live_order_agree_across_a_reinit() {
    let (m, tmp) = start_with_a_tiny_fifo().await;
    let hub: Arc<Hub> = m.hub.clone();

    let a = a_thread(&m, tmp.path());
    let b = a_thread(&m, tmp.path());
    let c = a_thread(&m, tmp.path());

    let mut f = hub.connect(214);
    hub.subscribe(SHAPE, Some(FLEET), 214, None).unwrap();
    settle().await;
    let cold = held(&[], &drain(&mut f));

    for id in [&a, &c, &b] {
        visit(&m, id, 0);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    settle().await;
    for i in 0..12 {
        m.append_custom(&a, "assistant_suggestions".into(), json!({"i": i}), i % 2 == 0)
            .expect("a non-user append");
    }
    settle().await;

    let live = held(&cold, &drain(&mut f));
    assert_eq!(
        live,
        vec![b.clone(), c.clone(), a.clone()],
        "the LIVE fold orders on visit time: {live:?}"
    );

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
        "the scenario REQUIRES a real re-initialisation: {epoch:?}"
    );

    let after = held(&live, &drain(&mut f));
    assert_eq!(
        after, live,
        "the order a RE-INITIALISED subscriber receives must be the order the live \
         fold held:\n  live  {live:?}\n  after {after:?}"
    );
    assert_eq!(after, vec![b, c, a], "…and it is visit order: {after:?}");
    drop(tmp);
}

/// **AN ENTRY IS AN ID, A VISIT STAMP AND A CURSOR** — asserted exactly, so a
/// convenient field (a title, an `unread`) is a red, and the cursor is the one
/// the visit left.
///
/// **MUTATION.** Add `pub unread: u64` to `VisitEntry` (filled `0`).
#[tokio::test]
async fn the_entry_is_an_id_a_stamp_and_a_cursor() {
    let (m, tmp) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    let a = a_thread(&m, tmp.path());
    let mut f = hub.connect(205);
    hub.subscribe(SHAPE, Some(FLEET), 205, None).unwrap();
    settle().await;
    let _ = drain(&mut f);
    visit(&m, &a, 7);
    settle().await;
    let entries = order_in(&latest(&drain(&mut f))).unwrap();
    assert_eq!(entries.len(), 1);
    let mut keys: Vec<&str> = entries[0].as_object().unwrap().keys().map(|k| k.as_str()).collect();
    keys.sort();
    assert_eq!(keys, vec!["at", "id", "seen_seq"], "an entry is id, at, seen_seq: {keys:?}");
    assert_eq!(entries[0]["id"], json!(a));
    assert_eq!(entries[0]["seen_seq"], json!(7), "the cursor the visit left");
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
