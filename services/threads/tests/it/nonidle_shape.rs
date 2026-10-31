//! # `threads/non-idle` — the roster, keyed by the pool
//!
//! The index that exists so `accounts` does not have to hold six fields of
//! `threads`' rows under an account key. Its module doc carries the argument;
//! this file proves the four properties the argument rests on:
//!
//! 1. **ONE KEYLESS SUBSCRIBE ANSWERS IT.** That is the efficiency bar the
//!    operator set — *"whatever is best for the consumer, such that it's still
//!    the same efficiency"* — and the reason ids-plus-a-count on `accounts` was
//!    rejected.
//! 2. **THE PREDICATE IS CARRIED, not left to a second call** — `state` and
//!    `executing`, which are the two fields `view::running` reads.
//! 3. **ONLY `idle` REMOVES.** Parked, retrying and queued stay.
//! 4. **A MOVE IS A FIELD WRITE.** On `accounts` a placement change meant
//!    scanning every account row for the thread somewhere else; here a thread
//!    appears exactly once.
//!
//! **Every test was forced red by a named mutation before it was believed**, and
//! the mutation is in the test's own docstring.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use threads::config::Config;
use threads::event::Push;
use threads::shape::{ConnFrames, Derived, Hub, Op, Shape};
use threads::shapes::{nonidle, threads as tshape};
use threads::{build_manager, ThreadManager, ThreadSettings};

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

/// **PARENTS BEFORE CHILDREN**, which `Hub::register` enforces with a panic:
/// `threads/non-idle` declares `PARENT = Some("threads")`.
async fn start() -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    // ONE account, so `create_thread`'s pool choice is deterministic and a
    // placement assertion is about the roster rather than about which login the
    // pool happened to pick. The move test names its target account on the push
    // itself, which is what the fold reads.
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
    hub.register(nonidle::NonIdle, manager.clone());
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

/// **A KEYLESS SUBSCRIBE.** `None` for the key is the whole efficiency claim —
/// the roster arrives without the consumer naming anything.
async fn watch_roster(m: &ThreadManager, conn: u64) -> ConnFrames {
    let f = m.hub.connect(conn);
    m.hub
        .subscribe(nonidle::NonIdle::NAME, None, conn, None)
        .expect("a keyless subscribe to the roster is the point of the shape");
    settle().await;
    f
}

/// The newest roster this connection holds — the export, or a later live row.
fn roster(frames: &[Arc<threads::shape::ShapeFrame>]) -> Vec<Value> {
    let newest = frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(nonidle::FLEET))
        .filter_map(|f| match (&f.wire.op, &f.wire.event, &f.wire.row) {
            (Op::Live, Some(Derived::Row { row }), _) => Some(row.clone()),
            (Op::Read, _, Some(row)) => Some(row.clone()),
            _ => None,
        })
        .last()
        .unwrap_or_else(|| {
            panic!(
                "no roster frame for `{}` among {} frames",
                nonidle::FLEET,
                frames.len()
            )
        });
    newest["threads"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("the roster cell is a list: {newest}"))
}

fn entry<'a>(roster: &'a [Value], id: &str) -> Option<&'a Value> {
    roster.iter().find(|e| e["id"] == json!(id))
}

/// Make a thread non-idle by parking a message in its queue — `state: queued`,
/// which `thread_rows_non_idle` admits through `waiting_queue_counts`.
fn enqueue(m: &ThreadManager, id: &str, message_id: u64) {
    m.store
        .enqueue(id, message_id, "parked", "turn_end", &Value::Null, &[])
        .unwrap();
}

fn status(id: &str, state: &str, executing: bool, account: Option<&str>) -> Push {
    Push::Status {
        thread_id: id.to_string(),
        state: state.to_string(),
        executing,
        queued: 0,
        unread: 0,
        detail: None,
        account: account.map(str::to_string),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. ONE KEYLESS SUBSCRIBE, AND THE PREDICATE IS IN IT
// ─────────────────────────────────────────────────────────────────────────────

/// **THE ROSTER ARRIVES WHOLE ON ONE KEYLESS SUBSCRIBE, CARRYING THE PREDICATE.**
///
/// This is the test that decides the design. `view::running` answers *"which
/// threads are executing, fleet-wide"* from ONE subscribe today, off
/// `accounts.*.threads`; an ids-plus-count `accounts` would have made it one
/// subscribe plus N keyed subscribes to `threads`, which is `PerKey` and refuses
/// a keyless one. So the bar is: one subscribe, and `state` + `executing` in the
/// answer.
///
/// **THE MUTATION.** `NonIdle::HYDRATE = Hydrate::PerKey`. The keyless subscribe
/// is then refused and `watch_roster`'s `expect` fires — which is the honest red
/// for this property, because the property IS that no key is needed.
#[tokio::test]
async fn one_keyless_subscribe_answers_the_whole_roster_with_the_predicate() {
    let (m, tmp) = start().await;
    let a = thread_on(&m, tmp.path());
    let b = thread_on(&m, tmp.path());
    enqueue(&m, &a, 1);
    enqueue(&m, &b, 1);

    let mut f = watch_roster(&m, 21).await;
    let r = roster(&drain(&mut f));

    assert_eq!(r.len(), 2, "both non-idle threads, one subscribe: {r:?}");
    for id in [&a, &b] {
        let e = entry(&r, id).unwrap_or_else(|| panic!("`{id}` is missing: {r:?}"));
        assert_eq!(
            e["account"], json!("first"),
            "the PLACEMENT, which the account key used to carry implicitly: {e}"
        );
        assert_eq!(
            e["state"], json!("queued"),
            "half the predicate, and a closed vocabulary: {e}"
        );
        assert_eq!(
            e["executing"], json!(false),
            "the other half, carried rather than left to a second call: {e}"
        );
        assert!(
            e.get("id").is_some(),
            "`id` is a DECLARED `Ref(\"thread\")`, not an implied map key: {e}"
        );
    }
    drop(tmp);
}

/// **IT CARRIES THE PREDICATE AND NOT THE ROW.** The two fields dropped from
/// `accounts.threads` — `unread`, `detail` — are per-thread detail the pool
/// already holds, and `detail` is free text that rode every export of every
/// account key. `queued` is NOT dropped: the account board sums it per card
/// (R2.S8), so it is the one roll-up that rides — see
/// `queued_rides_the_roster_from_the_hydrate_and_from_the_fold`.
///
/// **THE MUTATION.** A `("detail", Sc(Str), Optional)` field added to the
/// manifest and filled in `PlacedRow::to_json`. Red on the key being present.
#[tokio::test]
async fn the_roster_carries_the_predicate_and_not_the_row() {
    let (m, tmp) = start().await;
    let a = thread_on(&m, tmp.path());
    enqueue(&m, &a, 1);

    let mut f = watch_roster(&m, 22).await;
    let r = roster(&drain(&mut f));
    let e = entry(&r, &a).expect("the thread is on the roster");

    for dropped in ["unread", "detail"] {
        assert!(
            e.get(dropped).is_none(),
            "`{dropped}` is `crate::shapes::threads`' and must not be re-served \
             here — that re-serving is what made `accounts.threads` a row \
             fragment: {e}"
        );
    }
    let mut keys: Vec<&String> = e.as_object().expect("an entry object").keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["account", "ancestors", "executing", "id", "parent_id", "queued", "since", "state"],
        "exactly the eight declared fields (`parent_id`: TUI-SUBAGENTS M6; `ancestors`: R9.UI1): {e}"
    );
    drop(tmp);
}

/// **A QUEUED MESSAGE APPEARS ON THE ROW — off the hydrate AND off the fold.**
///
/// `threads_tui/model.py` sums `queued` per account into the card's
/// `queued_messages`. It read that off `accounts.threads`; when that cell goes,
/// this roster is the only keyless place the number lives, so both of its
/// writers are pinned with DIFFERENT values (2 and 1 off the store, 5 off a
/// live status) — a writer that emitted any one constant cannot satisfy all
/// three.
///
/// **THE MUTATIONS.** (1) the fold's `.emit(.., *queued)` changed to
/// `.emit(.., 0)`: red on the live `5`. (2) the hydrate's `e.queued = t.queued`
/// changed to `e.queued = 0`: red on the hydrated `2`.
#[tokio::test]
async fn queued_rides_the_roster_from_the_hydrate_and_from_the_fold() {
    let (m, tmp) = start().await;
    let a = thread_on(&m, tmp.path());
    let b = thread_on(&m, tmp.path());
    enqueue(&m, &a, 1);
    enqueue(&m, &a, 2);
    enqueue(&m, &b, 1);

    let mut f = watch_roster(&m, 26).await;
    let r = roster(&drain(&mut f));
    let ea = entry(&r, &a).expect("`a` is on the hydrated roster");
    let eb = entry(&r, &b).expect("`b` is on the hydrated roster");
    assert_eq!(ea["queued"], json!(2), "two messages wait on `a`, off the store: {ea}");
    assert_eq!(eb["queued"], json!(1), "one message waits on `b`, off the store: {eb}");

    let mut live = status(&a, "queued", false, Some("first"));
    if let Push::Status { queued, .. } = &mut live {
        *queued = 5;
    }
    m.hub.emit(live);
    settle().await;
    let r = roster(&drain(&mut f));
    let ea = entry(&r, &a).expect("`a` is still on the roster");
    assert_eq!(ea["queued"], json!(5), "the live status's count is the row's count: {ea}");
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. MEMBERSHIP: ONLY `idle` REMOVES
// ─────────────────────────────────────────────────────────────────────────────

/// **ONLY `idle` REMOVES; PARKED, RETRYING AND QUEUED STAY.**
///
/// `accounts.threads`' rule, carried over, and it is the membership predicate
/// the `shared` block publishes. A roster that dropped a parked thread would
/// answer "nothing is going on" while a thread waited.
///
/// **THE MUTATION.** The fold's `if state == "idle"` changed to
/// `if state != "active"`. The parked assertion then fires, because parked would
/// have been removed too.
#[tokio::test]
async fn only_idle_leaves_the_roster() {
    let (m, tmp) = start().await;
    let a = thread_on(&m, tmp.path());
    enqueue(&m, &a, 1);

    let mut f = watch_roster(&m, 23).await;
    assert!(
        entry(&roster(&drain(&mut f)), &a).is_some(),
        "POSITIVE SENTINEL: it must be on the roster before anything is removed"
    );

    // Each of the three non-idle words keeps it, and the state word follows.
    for state in ["parked", "retrying", "active"] {
        m.hub.emit(status(&a, state, state == "active", Some("first")));
        settle().await;
        let r = roster(&drain(&mut f));
        let e = entry(&r, &a)
            .unwrap_or_else(|| panic!("`{state}` must NOT remove the thread: {r:?}"));
        assert_eq!(e["state"], json!(state), "the state word follows: {e}");
    }

    // And `idle` does remove it.
    m.hub.emit(status(&a, "idle", false, Some("first")));
    settle().await;
    let r = roster(&drain(&mut f));
    assert!(
        entry(&r, &a).is_none(),
        "`idle` is the ONE word that removes: {r:?}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. A MOVE IS A FIELD WRITE
// ─────────────────────────────────────────────────────────────────────────────

/// **A PLACEMENT MOVE IS ONE FIELD WRITE, AND THE THREAD APPEARS ONCE.**
///
/// This is the structural win over the nested form. Keying the roster by account
/// forced a cross-key invariant — a thread may sit under exactly one account —
/// so `accounts`' fold SCANS every account row on every `Push::Status` looking
/// for the thread elsewhere and removes it, which is why that shape carries
/// `BURST: 8`. Here a thread has one entry and a move writes `account`.
///
/// **THE MUTATION.** `PlacedRow::emit` with the `self.account = …` assignment
/// deleted. The thread stays on the roster under its OLD account and the second
/// assertion fires — the exact defect the `elsewhere` scan exists to prevent, and
/// which cannot arise here.
#[tokio::test]
async fn a_move_rewrites_the_account_and_never_duplicates_the_thread() {
    let (m, tmp) = start().await;
    let a = thread_on(&m, tmp.path());
    enqueue(&m, &a, 1);

    let mut f = watch_roster(&m, 24).await;
    let before = roster(&drain(&mut f));
    assert_eq!(
        entry(&before, &a).map(|e| e["account"].clone()),
        Some(json!("first")),
        "POSITIVE SENTINEL: it must start on `first` or the move is unobservable: {before:?}"
    );

    m.hub.emit(status(&a, "active", true, Some("second")));
    settle().await;
    let after = roster(&drain(&mut f));

    assert_eq!(
        after.iter().filter(|e| e["id"] == json!(a)).count(),
        1,
        "ONE entry, always — a move is not an insert: {after:?}"
    );
    assert_eq!(
        entry(&after, &a).map(|e| e["account"].clone()),
        Some(json!("second")),
        "the placement moved: {after:?}"
    );
    drop(tmp);
}

/// **A STATUS THE DAEMON CANNOT PLACE WRITES NOTHING** — no entry, and no `"?"`
/// account. `account` is declared `Required` on an entry, so writing one without
/// a placement would publish a thread as placed somewhere that does not exist.
/// `accounts` counts this case as `unplaced_status` and the number is already on
/// `shapes.list`.
///
/// **THE MUTATION.** The fold's `let Some(account) = account else { return }`
/// replaced by `let account = account.clone().unwrap_or_else(|| "?".into())`.
/// The roster then grows an entry and the assertion fires.
#[tokio::test]
async fn an_unplaced_status_mints_no_entry() {
    let (m, tmp) = start().await;
    let a = thread_on(&m, tmp.path());
    enqueue(&m, &a, 1);
    // A thread id that was never created: the property is about the FOLD's
    // treatment of `account: None` and nothing else, so the fixture does not
    // also depend on a second thread's lifecycle.
    let unplaced = "00000000-0000-4000-8000-000000000000";

    let mut f = watch_roster(&m, 25).await;
    let before = roster(&drain(&mut f));
    assert_eq!(
        before.len(), 1,
        "POSITIVE SENTINEL: exactly the one placed thread to begin with: {before:?}"
    );
    assert!(
        entry(&before, unplaced).is_none(),
        "POSITIVE SENTINEL: the unplaced id is not on the roster yet: {before:?}"
    );

    m.hub.emit(status(unplaced, "active", true, None));
    settle().await;

    // **THE ASSERTION IS THAT NOTHING WAS PUBLISHED AT ALL**, which is stronger
    // than "no entry appeared" and is the honest shape of this property: the
    // fold returns before touching the pool, so the roster is not re-exported
    // and the consumer is told nothing — there is no frame to inspect.
    //
    // (Asserting it by re-reading a roster is what the first version of this
    // test did, and it failed `no roster frame among 0 frames` — reading the
    // absence of a frame as a broken rig when it was the property itself.)
    let frames = drain(&mut f);
    assert!(
        frames.is_empty(),
        "a status the daemon cannot place must publish NOTHING — inventing a \
         `\"?\"` placement would publish a thread as placed somewhere that does \
         not exist, and `account` is declared Required on an entry. Got {} \
         frame(s): {:?}",
        frames.len(),
        frames
            .iter()
            .map(|f| (f.wire.op.clone(), f.wire.key.clone()))
            .collect::<Vec<_>>()
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. THE PRE-INIT GUARD
// ─────────────────────────────────────────────────────────────────────────────

/// **THE FIRST SUBSCRIBER RECEIVES THE HYDRATE'S WHOLE ROSTER**, not whatever
/// the fold happened to have seen.
///
/// **AND IT DOES NOT PROVE THE FOLD'S PRE-INIT GUARD, WHICH IS WHY IT IS NAMED
/// FOR WHAT IT DOES PROVE.** The guard
/// (`if tx.pools.get(&FLEET).is_none() { return }`) exists so a status folded
/// before the pool is minted cannot publish a roster of exactly one thread. It
/// is UNREACHABLE: under `Tracking::Lazy` the chassis routes no input to this
/// shape until something subscribes, and the subscribe runs `hydrates` — which
/// mints the key — before any fold.
///
/// This was found by forcing it: an earlier version of this test claimed the
/// guard as its mutation, and **deleting the guard left all six tests green**.
/// A test that cannot fail for the reason it states is worse than no test, so
/// the claim is retracted here rather than left standing. The guard stays as
/// defence in depth and says as much at its own site.
///
/// **THE MUTATION this test DOES force.** `hydrates` with the `for t in rows`
/// loop deleted: the pool is still minted and the export still arrives, so the
/// roster comes back EMPTY and the length assertion fires.
#[tokio::test]
async fn the_first_subscriber_receives_the_hydrates_whole_roster() {

    let (m, tmp) = start().await;
    let a = thread_on(&m, tmp.path());
    let b = thread_on(&m, tmp.path());
    enqueue(&m, &a, 1);
    enqueue(&m, &b, 1);

    // Nobody has subscribed, so nothing is initialised. This status must not
    // create the pool.
    m.hub.emit(status(&a, "active", true, Some("first")));
    settle().await;

    let mut f = watch_roster(&m, 26).await;
    let r = roster(&drain(&mut f));

    assert_eq!(
        r.len(), 2,
        "the roster is the HYDRATE's, carrying both non-idle threads — not the \
         one entry a pre-init fold would have minted: {r:?}"
    );
    assert!(entry(&r, &a).is_some() && entry(&r, &b).is_some(), "{r:?}");
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
