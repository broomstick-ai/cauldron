//! # Ack-then-silence, closed in five more places
//!
//! One disease, five doors. Rule 2.7 splits an act into the moments it
//! completes so that a producer never says *"accepted"* and then goes quiet;
//! every test in this file is a case where it said something stronger than
//! `accepted` — `visible: true`, `consumers: 1`, `ok: true` — and then the
//! frame, the row or the timer never came.
//!
//! **Two of these are written from the CONSUMER'S POSITION, over a real
//! socket, and that is not a stylistic choice.** Both were found by a probe
//! against a running daemon and MISSED by the unit tests around them: finding 1
//! because the promise book is empty on both the correct and the broken path
//! (the book closes the id at the emit either way, so only a consumer counting
//! frames can tell), and finding 2 because the hub's `subs` set is pruned
//! correctly and the defect is entirely in `drive`'s per-connection gate, which
//! no unit test can reach. A test that cannot observe what the consumer
//! observes cannot see either bug.
//!
//! Every test here was **forced red by a named mutation before it was
//! believed**, and the mutation is in the test's own docstring. Where a
//! mutation came back GREEN it is said so out loud and the test was rewritten
//! against observable behaviour instead.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::event::Fact;
use threads::shape::{
    CellState, ConnFrames, Hub, Input, Op, Outbox, Root, Shape, Stamped, Timestamp, Tracking, Tx,
};
use threads::{build_manager, ThreadManager};
use shapewire::cell::Cells;

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

fn config_in(tmp: &std::path::Path) -> Config {
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    let mut accounts = BTreeMap::new();
    for a in ["first", "second"] {
        accounts.insert(a.to_string(), tmp.join(a).to_string_lossy().to_string());
    }
    Config {
        data_dir: Some(tmp.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

/// The CHASSIS server — the daemon's one door since R2.S4. (`start_live`, the
/// legacy `:3651` loop, stood beside it until then; every test that used it now
/// runs here.)
async fn start_chassis() -> (ThreadManager, SocketAddr, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let manager = build_manager(config_in(tmp.path())).unwrap();
    let chassis = threads::chassis::chassis(Arc::new(manager.clone()));
    let listener = shapewire::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (addr, h) = chassis
        .serve_on_with_shutdown(listener, std::future::pending::<()>())
        .unwrap();
    std::mem::forget(h);
    (manager, addr, tmp)
}

/// A minimal websocket consumer. Both loops speak the same request/response
/// envelope for these three methods, so one client drives both ports.
struct Client {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

/// **RULE 2.1'S CONSUMER IDENTITY, STATED IN ONE PLACE FOR EVERY SUBSCRIBE THIS
/// FILE SENDS.**
///
/// Identity is COMPULSORY at subscribe time since the operator's ruling of
/// 2026-10-08 (*"consumers to be forced to say who they are so we can track
/// consumers"*), so a subscribe carrying no `consumer` is refused `malformed`.
/// The tests in this file are about hydration, unsubscribe, retention, paging
/// and the rest — NOT about identity — so they state a well-formed one here,
/// once, exactly as a real consumer does: `seam/link.rs`'s `subscribe_params`
/// builds its identity in one place for the same reason, and the board's
/// `api.py` now does too.
///
/// **IT NEVER OVERWRITES AN IDENTITY THE CALLER STATED.** A test that sends a
/// deliberately blank or absent one to prove the refusal still can, by saying
/// `consumer` itself. That is how the rule is actually proven, and it is proven
/// elsewhere on purpose — `shapes.rs`'s
/// `an_absent_identity_is_refused_at_the_wire_and_so_is_a_blank_one` at the wire
/// and `shapewire::ledger`'s `an_absent_identity_is_refused_and_so_is_a_blank_one`
/// at the parser — never by this helper, which exists to keep unrelated tests
/// from asserting identity by accident.
fn identified(method: &str, params: Value) -> Value {
    if !method.ends_with("/subscribe") {
        return params;
    }
    let mut params = params;
    if params.get("consumer").is_some() {
        return params;
    }
    if let Some(o) = params.as_object_mut() {
        o.insert(
            "consumer".into(),
            json!({"name": "threads-tests", "shape_version": "shapes.md@rev4"}),
        );
    }
    params
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        let mut c = Self { ws };
        c.next_within(900).await; // the welcome
        c
    }

    async fn send(&mut self, v: Value) {
        use tokio_tungstenite::tungstenite::Message;
        self.ws
            .send(Message::Text(v.to_string().into()))
            .await
            .unwrap();
    }

    async fn next_within(&mut self, ms: u64) -> Option<Value> {
        use tokio_tungstenite::tungstenite::Message;
        loop {
            let got =
                tokio::time::timeout(std::time::Duration::from_millis(ms), self.ws.next()).await;
            match got {
                Err(_) | Ok(None) => return None,
                Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).ok(),
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) => return None,
            }
        }
    }

    /// Everything that arrives before `ms` of silence.
    async fn drain_for(&mut self, ms: u64) -> Vec<Value> {
        let mut out = Vec::new();
        while let Some(v) = self.next_within(ms).await {
            out.push(v);
        }
        out
    }

    /// Send a request and return its response, KEEPING every frame that arrived
    /// while waiting — an export can overtake a reply and a test that threw
    /// those away would be measuring its own client.
    async fn request(&mut self, id: &str, method: &str, params: Value) -> (Value, Vec<Value>) {
        let params = identified(method, params);
        self.send(json!({"type":"request","id":id,"method":method,"params":params}))
            .await;
        let mut other = Vec::new();
        loop {
            let Some(f) = self.next_within(2_000).await else {
                panic!("no response to {method}");
            };
            if f["type"] == "response" && f["correlate"] == id {
                return (f, other);
            }
            other.push(f);
        }
    }
}

/// The payload of a response, whichever envelope it came in. The live loop
/// answers `{"outcome":{"type":"success","payload":…}}` and so does the chassis
/// loop; a refusal is `{"outcome":{"type":…,"…"}}` with no payload.
fn payload(resp: &Value) -> &Value {
    &resp["outcome"]["payload"]
}

/// Every `promise_id` any `shape` frame in `frames` carries.
fn promise_ids(frames: &[Value]) -> BTreeSet<u64> {
    frames
        .iter()
        .filter(|f| f["type"] == "shape")
        .filter_map(|f| f["promises"].as_array())
        .flatten()
        .filter_map(Value::as_u64)
        .collect()
}

/// Every `(shape, key)` a `shape` frame in `frames` is about.
fn keys_seen(frames: &[Value]) -> Vec<String> {
    frames
        .iter()
        .filter(|f| f["type"] == "shape")
        .filter_map(|f| f["key"].as_str().map(str::to_string))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// A SHAPE WITH A HYDRATE YOU CAN LAND AN EVENT INSIDE
// ─────────────────────────────────────────────────────────────────────────────

/// How long `Slow::hydrates` blocks. Long enough that a command issued right
/// after the subscribe lands inside the window on a loaded machine, short
/// enough that the test is not a sleep.
const HYDRATE_MS: u64 = 700;

/// A keyed root over account names that folds `Fact::AccountDisabled`.
#[derive(Default)]
struct Accounts {
    rows: BTreeMap<String, bool>,
    dirty: BTreeSet<String>,
}

impl Root for Accounts {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }
    fn row(&self, k: &str) -> Option<Value> {
        self.rows.get(k).map(|d| json!({ "disabled": d }))
    }
    fn cells(&self, k: &str) -> Option<Value> {
        self.rows.get(k).map(|_| {
            json!({ "disabled": CellState::Known {
                complete: true,
                folded_at: chrono::Utc::now(),
                since_init: false,
                covers_since: None,
            }})
        })
    }
    fn shared(&self) -> Value {
        json!({})
    }
    fn shared_cells(&self) -> Value {
        json!({})
    }
    fn drain(&mut self) -> Outbox {
        Outbox {
            rows: std::mem::take(&mut self.dirty).into_iter().collect(),
            ..Default::default()
        }
    }
    fn mark_dead(&mut self, _: &str) {}
}

/// **THE HYDRATE WINDOW, MADE OBSERVABLE.**
///
/// `hydrates` blocks for [`HYDRATE_MS`], which is what the real `accounts`
/// shape's ~0.8 s of `rusqlite` and keychain work does on a cold subscribe. It
/// must run on a MULTI-THREAD runtime for a command to be issuable during it —
/// `init_blocking` calls inline on a current-thread runtime, which is exactly
/// why every existing `#[tokio::test]` in this crate could never see this bug.
struct Slow;


// The cells this file's test roots emit, declared — `Shape::CELLS` is required
// (see `shapewire::cell`), and a test shape declares what its `Root::cells`
// actually spells.
shapewire::declare_cells! {
    DISABLED_CELLS = [("disabled", shapewire::operation::DType::Scalar(shapewire::operation::Scalar::Int), shapewire::operation::Presence::Required, [])]
}

impl Shape<Input> for Slow {
    const NAME: &'static str = "slowaccounts";
    const CELLS: Cells = DISABLED_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    type Producer = ThreadManager;
    type Root = Accounts;
    fn root(&self) -> Accounts {
        Accounts::default()
    }
    fn hydrates(&mut self, p: &ThreadManager, root: &mut Accounts, _: Timestamp) {
        std::thread::sleep(std::time::Duration::from_millis(HYDRATE_MS));
        for a in p.config.accounts.keys() {
            root.rows.insert(a.clone(), false);
        }
        let _ = root.drain(); // a hydrate publishes nothing
    }
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, Accounts>) {
        if let Input::Fact(Fact::AccountDisabled {
            account, disabled, ..
        }) = &input.value
        {
            tx.rows.insert(account.clone(), *disabled);
            tx.dirty.insert(account.clone());
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. `visible: true` FOR A PROMISE NOTHING WILL EVER CARRY
// ═════════════════════════════════════════════════════════════════════════════

/// **A COMMAND ISSUED DURING A COLD HYDRATE IS TOLD `visible: true`, SO A FRAME
/// MUST CARRY ITS ID.**
///
/// `Hub::subscribe` flips `tracking` synchronously, before the shape task has
/// reached `init_blocking`, so for the whole hydrate window every producer gate
/// reads `any_tracking() == true` and answers `Visible::Expected`. The quiet
/// drain then folded each input that had arrived during the read and threw its
/// `Outbox` away — and never touched `st.promises`. The id was in no live frame
/// (discarded), no export (`pending_promises` was empty), and no re-subscribe
/// (`carried` was never written). `visible: true` and nothing, forever: the
/// not-tracking bug in a different window.
///
/// Asserted from the CONSUMER'S SIDE and not from the book: the promise book is
/// empty on both the correct and the broken path, because the emit closes the id
/// either way. Only a consumer counting frames can tell them apart, which is why
/// the unit tests around this code could not.
///
/// **Forced red** by deleting `self.remember_pending(st.promises.clone());` from
/// the quiet drain in `ShapeTask::init_blocking`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_promise_made_during_a_cold_hydrate_still_reaches_the_consumer() {
    let (m, addr, tmp) = start_chassis().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Slow, m.clone());

    let mut c = Client::connect(addr).await;
    // COLD: nothing has ever subscribed, so this subscribe pays the hydrate.
    let (sub, _) = c
        .request(
            "1",
            &format!("shapes/{}/subscribe", "slowaccounts"),
            json!({"key":"first"}),
        )
        .await;
    assert_eq!(
        payload(&sub)["queued"],
        json!(true),
        "the subscribe must be accepted: {sub}"
    );

    // INSIDE THE WINDOW. The task is blocked in `hydrates` right now, so this
    // fact lands in the FIFO and is drained quietly.
    assert!(
        m.hub.any_tracking(),
        "tracking is on BEFORE the hydrate finishes — that is the whole premise \
         of this test, and without it the command below takes the not-tracking \
         path and proves nothing"
    );
    let (reply, early) = c
        .request("2", &format!("accounts/{}/disable", "first"), json!({}))
        .await;
    let body = payload(&reply);
    let id = body["promise_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("a command replies with a promise id: {reply}"));
    assert_eq!(
        body["visible"],
        json!(true),
        "a fold IS tracking, so the producer promised a frame would carry id \
         {id}: {reply}"
    );

    // Everything the consumer is ever told. The hydrate has to finish first, so
    // the window is generous.
    let mut frames = early;
    frames.extend(c.drain_for(HYDRATE_MS + 900).await);
    let seen = promise_ids(&frames);
    assert!(
        !frames.iter().any(|f| f["type"] == "shape") == false,
        "the consumer must receive SOME shape frame, or the silence below is the \
         test's own bug: {frames:#?}"
    );
    assert!(
        seen.contains(&id),
        "the producer said `visible: true` for promise {id} and then no frame \
         carried it — ids on the wire were {seen:?}: {frames:#?}"
    );
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. `shapes.unsubscribe {key}` MUST STOP THAT KEY'S FRAMES
// ═════════════════════════════════════════════════════════════════════════════

/// The body of the two tests below: one connection subscribes to `first` and
/// `second`, unsubscribes `first`, and then both accounts move.
///
/// The positive control is the POINT of the shape of this assertion: `second`
/// must still arrive. A test that only asserted silence for `first` would pass
/// against a daemon that had stopped delivering anything at all.
async fn a_keyed_unsubscribe_is_honoured_on(addr: SocketAddr, m: &ThreadManager) {
    let mut c = Client::connect(addr).await;
    for (i, k) in ["first", "second"].iter().enumerate() {
        let (r, _) = c
            .request(
                &format!("s{i}"),
                &format!("shapes/{}/subscribe", "accounts"),
                json!({"key":k}),
            )
            .await;
        assert_eq!(payload(&r)["queued"], json!(true), "{r}");
    }
    // A SECOND connection keeps the lazy shape tracking after the unsubscribe,
    // so the silence below is a gate decision and not an idle shape.
    let mut keeper = Client::connect(addr).await;
    keeper
        .request("k", &format!("shapes/{}/subscribe", "accounts"), json!({}))
        .await;
    c.drain_for(700).await;
    keeper.drain_for(700).await;

    let (un, _) = c
        .request(
            "u",
            &format!("shapes/{}/unsubscribe", "accounts"),
            json!({"key":"first"}),
        )
        .await;
    assert_eq!(payload(&un)["ok"], json!(true), "{un}");

    // BOTH accounts move. `first` is unsubscribed; `second` is not.
    m.emit_health_fact("first");
    m.emit_health_fact("second");
    let frames = c.drain_for(900).await;
    let keys = keys_seen(&frames);

    assert!(
        keys.iter().any(|k| k == "second"),
        "THE POSITIVE CONTROL: `second` is still subscribed and must still \
         arrive, or the absence of `first` proves nothing about the \
         unsubscribe: {frames:#?}"
    );
    assert!(
        !keys.iter().any(|k| k == "first"),
        "`shapes.unsubscribe {{key: \"first\"}}` was answered `ok: true` and \
         `first`'s frames kept arriving: {frames:#?}"
    );
}

// `a_keyed_unsubscribe_stops_that_keys_frames_on_the_live_port` stood here and
// was DELETED at R2.S4: it ran `a_keyed_unsubscribe_is_honoured_on` against the
// legacy `:3651` loop (`server::handle_connection_method`'s prune), which is
// retired. The same assertion against the chassis loop is the next test, and
// it is now the only loop there is.


/// **THE CHASSIS PORT.** Same defect, separate loop, separate `shape_subs`, and
/// it was verified over a real socket on both — so it is proved on both.
///
/// **Forced red** by deleting the `match key { … }` block that prunes
/// `shape_subs` from the `shapes.unsubscribe` arm of
/// `shapewire::server::chassis_method`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keyed_unsubscribe_stops_that_keys_frames_on_the_chassis_port() {
    let (m, addr, tmp) = start_chassis().await;
    a_keyed_unsubscribe_is_honoured_on(addr, &m).await;
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. A KEYED UNSUBSCRIBE OF ONE KEY MUST NOT WIPE THE LEDGER ENTRY
// ═════════════════════════════════════════════════════════════════════════════

/// **`subscribers` AND `consumers` ANSWER ONE QUESTION AND MUST NOT DISAGREE.**
///
/// `Hub::unsubscribe` called `ConsumerLedger::disconnected` on every unsubscribe,
/// and that function takes no key: it removes the whole `(conn, shape)` entry. So
/// dropping ONE of two keys emptied the ledger while the task still held the
/// other pair and still delivered its frames — `shapes.list` answering
/// `subscribers: 1, consumers: 0`. `wire::ShapeInfo::consumers` had just been
/// corrected to say the two agree; this was the one call in which they did not.
///
/// Asserted as the DISAGREEMENT and not as `consumers.len() == 1`, because that
/// is the claim the wire doc makes and the only one a future change can break in
/// a way this test should catch.
///
/// **Forced red** by replacing the `Some(k) => self.consumers.narrowed(…)` arm of
/// `Hub::unsubscribe` with the old unconditional
/// `self.consumers.disconnected(shape, conn, Unsubscribed)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keyed_unsubscribe_narrows_the_ledger_entry_it_does_not_wipe_it() {
    let (m, addr, tmp) = start_chassis().await;
    let mut c = Client::connect(addr).await;
    for (i, k) in ["first", "second"].iter().enumerate() {
        c.request(
            &format!("s{i}"),
            &format!("shapes/{}/subscribe", "accounts"),
            json!({"key":k,
                   "consumer":{"name":"prober","shape_version":"rev4"}}),
        )
        .await;
    }
    c.drain_for(700).await;

    c.request(
        "u",
        &format!("shapes/{}/unsubscribe", "accounts"),
        json!({"key":"first"}),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let info = m
        .hub
        .list()
        .into_iter()
        .find(|i| i.name == "accounts")
        .expect("the accounts shape");
    assert_eq!(
        info.subscribers, 1,
        "one connection still holds `second`, so the task counts it"
    );
    assert_eq!(
        info.consumers.len(),
        info.subscribers,
        "one question, two numbers: `subscribers` = {} and `consumers` = {} — \
         the disagreement `wire::ShapeInfo::consumers` says cannot happen. \
         consumers: {:#?}",
        info.subscribers,
        info.consumers.len(),
        info.consumers
    );
    // AND THE ENTRY KEPT THE KEY IT STILL HOLDS, rather than surviving as an
    // entry that names nothing — which would make the count agree and the answer
    // useless.
    let keys = &info.consumers[0].keys;
    assert!(
        keys.iter().any(|k| k == "second") && !keys.iter().any(|k| k == "first"),
        "the entry must NARROW to exactly what is still held: {keys:?}"
    );
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 4. A KEYED UNSUBSCRIBE MUST NOT CANCEL ANOTHER KEY'S ARMED TIMER
// ═════════════════════════════════════════════════════════════════════════════

/// **AN ARMED FUTURE-`at` SUBSCRIBE ON `a` SURVIVES AN UNSUBSCRIBE OF `b`.**
///
/// Both arms of `Item::Unsubscribe` fell through to `armed.retain(|s| s.conn !=
/// conn)`, which drops every armed subscribe this connection holds whatever key
/// it named. The consumer had been answered `{queued: true, subscribe_id}` —
/// rule 2.7's `accepted` — and then no export, no error and no death notice
/// followed. Ack-then-silence, which the `accepted`/`visible` split exists to
/// prevent.
///
/// Driven through the hub rather than a socket because the thing under test is
/// the armed queue, and what a consumer observes — "the export for `a` arrives
/// at its `at`" — is exactly what `ConnFrames` shows.
///
/// **Forced red** by replacing the `Some(k)` arm's key-scoped `armed.retain`
/// with the old `self.armed.retain(|(_, s)| s.conn != conn)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsubscribing_one_key_leaves_another_keys_armed_timer_armed() {
    let (m, _addr, tmp) = start_chassis().await;
    let hub: Arc<Hub> = m.hub.clone();
    let mut frames: ConnFrames = hub.connect(77);

    // A LIVE subscribe on `second`, so the shape is tracking and the connection
    // is a real subscriber.
    hub.subscribe("accounts", Some("second"), 77, None).unwrap();
    // And an ARMED one on `first`, a second in the future.
    let at = chrono::Utc::now() + chrono::Duration::milliseconds(1_200);
    let armed_id = hub
        .subscribe("accounts", Some("first"), 77, Some(at))
        .expect("a future `at` inside the bound is accepted");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    while frames.try_recv().is_ok() {}

    // Drop `second`. `first`'s timer must still be armed.
    hub.unsubscribe("accounts", Some("second"), 77);

    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    let mut got: Vec<Arc<threads::shape::ShapeFrame>> = Vec::new();
    while let Ok(f) = frames.try_recv() {
        got.push(f);
    }
    let satisfied = got.iter().any(|f| {
        f.wire.op == Op::Read
            && f.wire.key.as_deref() == Some("first")
            && f.wire.subscribes.contains(&armed_id)
    });
    assert!(
        satisfied,
        "subscribe {armed_id} was accepted for a future `at` on `first`, and an \
         unsubscribe of `second` silently cancelled it: {:#?}",
        got.iter()
            .map(|f| (f.wire.op, f.wire.key.clone(), f.wire.subscribes.clone()))
            .collect::<Vec<_>>()
    );
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 5. `pending_promises` IS BOUNDED, AND MORTAL
// ═════════════════════════════════════════════════════════════════════════════

/// A shape that folds every `AccountDisabled` fact and NEVER dirties a row — so
/// the ids it is handed can never be taken by a flush, which is the condition
/// under which `pending_promises` grew without limit.
struct NeverFlushes;

impl Shape<Input> for NeverFlushes {
    const NAME: &'static str = "neverflushes";
    const CELLS: Cells = DISABLED_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    type Producer = ThreadManager;
    type Root = Accounts;
    fn root(&self) -> Accounts {
        Accounts::default()
    }
    fn hydrates(&mut self, p: &ThreadManager, root: &mut Accounts, _: Timestamp) {
        for a in p.config.accounts.keys() {
            root.rows.insert(a.clone(), false);
        }
        let _ = root.drain();
    }
    /// Deliberately empty: the fact folds into nothing and dirties nothing.
    fn folds(&mut self, _: &Stamped<Input>, _: &mut Tx<'_, Accounts>) {}
}

/// **AN EXPORT NEVER RE-OFFERS MORE IDS THAN THE RETENTION CAP.**
///
/// `carried` is bounded twice and `prune_carried` is a free function so that
/// *"the write path and the read path cannot apply two different policies"*. Its
/// sibling `pending_promises` had no policy at all: merged into, emptied only by
/// a `row` that actually flushed, and COPIED by every export — so a subject whose
/// row never flushes accumulated past the documented
/// `PROMISE_RETENTION_PER_KEY` of 64 and rode every export for that subject
/// forever.
///
/// Asserted against the number on the WIRE rather than against the map, because
/// the cap's whole purpose is a bound on what a reconnecting consumer is handed.
///
/// **Forced red** by deleting the `cap_per_subject` call from
/// `ShapeTask::remember_pending`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_export_never_re_offers_more_ids_than_the_retention_cap() {
    let (m, _addr, tmp) = start_chassis().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(NeverFlushes, m.clone());
    let mut frames: ConnFrames = hub.connect(88);
    hub.subscribe("neverflushes", Some("first"), 88, None)
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    while frames.try_recv().is_ok() {}

    // `cap + 6` OPEN promises for ONE subject, resolved together — which is what
    // a coalescing producer does — and folded by a shape that dirties no row, so
    // no flush can ever take them.
    let cap = threads::shape::PROMISE_RETENTION_PER_KEY;
    for _ in 0..(cap + 6) {
        let _ = m
            .promises
            .open::<threads::commands::AccountsDisable>(Some("first".to_string()));
    }
    m.emit_disabled_fact("first");
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    while frames.try_recv().is_ok() {}

    // A re-subscribe: the export is the carrier of last resort, and this is the
    // number it offers.
    hub.subscribe("neverflushes", Some("first"), 88, None)
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let mut worst = 0usize;
    while let Ok(f) = frames.try_recv() {
        if f.wire.op == Op::Read {
            worst = worst.max(f.wire.promises.len());
        }
    }
    assert!(
        worst > 0,
        "THE POSITIVE CONTROL: the export must be offering ids at all, or the \
         bound below is satisfied by a wire that carries nothing"
    );
    assert!(
        worst <= cap,
        "an export re-offered {worst} ids for one subject; the documented cap is \
         {cap} and it was advisory"
    );
    drop(tmp);
}

/// **AND WHAT AN IDLE SHAPE HANDS FORWARD IS BOUNDED.**
///
/// # This test was rewritten, and the rewrite is the finding
///
/// It first asserted that `stop_if_idle` DROPPED the pending ids, and that fix
/// turned `a_promise_survives_the_connection_and_rides_the_re_export` red — a
/// pre-existing test of rule 2.5's own repair, whose phase 1 drops a socket INSIDE
/// the row debounce, where the pending id is the only record the producer still
/// has. 2.5 says so in as many words: *"the last consumer leaving is exactly the
/// moment those ids become the only record that a command was answered."* The
/// dropping fix was wrong, the suite caught it, and the code won.
///
/// What was actually broken was never the SURVIVAL. It was that nothing bounded
/// what survived: `pending_promises` had no cap and no age, so its ids rode every
/// export of every later epoch forever, beside freshly minted ones, on a shape
/// that had re-hydrated in between. So the ids now MOVE into `carried` at the
/// idle — the record that means *a frame is owed this*, bounded by
/// `PROMISE_RETENTION` and `PROMISE_RETENTION_PER_KEY` and deliberately outliving
/// an idle shape — and the assertion is the pair: still offered, and no longer
/// unbounded.
///
/// **THE MUTATION.** Delete the `remember_carried` loop from
/// `ShapeTask::stop_if_idle`, leaving the `pending_promises = Resolving::none()`.
///
/// Forced red:
/// ```text
/// an id pending when the last consumer left is the ONLY record that the command
/// was answered (rule 2.5), and the next epoch's export offered none of them
/// ```
///
/// # What this test does NOT pin, said rather than implied
///
/// A second mutation was tried — restore the ORIGINAL `stop_if_idle`, which left
/// `pending_promises` untouched — and it came back **GREEN**. That is not a hole
/// in this test, it is where the bound actually lives: `remember_pending` caps
/// each subject at merge time, so the `cap + 6` ids above are already 64 before
/// the shape goes idle, and `an_export_never_re_offers_more_ids_than_the_retention_cap`
/// is the test that reds on that cap.
///
/// What the handoff adds over leaving the set alone is therefore the AGE bound:
/// a pending id had none at all, and a carried one expires at
/// `PROMISE_RETENTION` (120 s). That is deliberately NOT asserted here, because
/// asserting it means sleeping two minutes or making a production constant
/// test-settable, and a two-minute test in this suite would be skipped inside a
/// month. Named as a residual instead of covered by an assertion that would pass
/// either way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_shape_hands_its_pending_ids_to_the_bounded_record() {
    let (m, _addr, tmp) = start_chassis().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(NeverFlushes, m.clone());
    let mut frames: ConnFrames = hub.connect(99);
    hub.subscribe("neverflushes", Some("first"), 99, None)
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    while frames.try_recv().is_ok() {}

    // `cap + 6` promises no frame of this epoch will carry: `NeverFlushes` dirties
    // nothing, so `pending_promises` is the only place they can live.
    let cap = threads::shape::PROMISE_RETENTION_PER_KEY;
    for _ in 0..(cap + 6) {
        let _ = m
            .promises
            .open::<threads::commands::AccountsDisable>(Some("first".to_string()));
    }
    m.emit_disabled_fact("first");
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    while frames.try_recv().is_ok() {}

    // The last consumer leaves. A Lazy shape drops its root and its pending rows,
    // so nothing will ever flush those ids.
    hub.unsubscribe("neverflushes", Some("first"), 99);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    // A NEW epoch: fresh hydrate, fresh `init_at`.
    hub.subscribe("neverflushes", Some("first"), 99, None)
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let mut worst = 0usize;
    let mut any = false;
    while let Ok(f) = frames.try_recv() {
        if f.wire.op == Op::Read {
            any |= !f.wire.promises.is_empty();
            worst = worst.max(f.wire.promises.len());
        }
    }
    assert!(
        any,
        "an id pending when the last consumer left is the ONLY record that the \
         command was answered (rule 2.5), and the next epoch's export offered \
         none of them"
    );
    assert!(
        worst <= cap,
        "and it is still inside the documented cap: {worst} ids for one subject \
         against {cap}. NOTE: this holds by `remember_pending`'s cap upstream, \
         not by the handoff — see the docstring"
    );
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 6. A BAD `THREADS_CHASSIS_PORT` IS LOUD
// ═════════════════════════════════════════════════════════════════════════════

/// **A MISCONFIGURATION THAT WOULD MISDIRECT THE CHASSIS SERVER IS REFUSED, NOT
/// IGNORED.**
///
/// `effective_chassis_port` was `s.parse().unwrap_or(self.chassis_port)`, so
/// `THREADS_CHASSIS_PORT=garbage` silently took the file value and the daemon
/// came up on a port nobody asked for with nothing said. Same class as
/// `chassis_port: 0` taking `Default` while *"the whole API move was promoted and
/// the port it moved to was dark"*.
///
/// Three cases, because absent, empty and WRONG are three states and only one of
/// them is an error — a refusal that also fired on "unset" would be a different
/// bug with the same test.
///
/// **Forced red** by restoring `s.parse().unwrap_or(self.chassis_port)`.
///
/// This test is `#[serial]`-shaped by hand: it mutates a process-wide env var, so
/// it saves and restores it and asserts nothing that another test's config could
/// move.
#[test]
fn a_chassis_port_that_is_not_a_port_is_refused_and_named() {
    let prior = std::env::var("THREADS_CHASSIS_PORT").ok();
    let cfg = Config {
        chassis_port: 3653,
        ..Default::default()
    };

    // UNSET: the file value, silently and correctly.
    std::env::remove_var("THREADS_CHASSIS_PORT");
    assert_eq!(cfg.effective_chassis_port().unwrap(), 3653);

    // EMPTY: "I did not set this". Still the file value.
    std::env::set_var("THREADS_CHASSIS_PORT", "");
    assert_eq!(
        cfg.effective_chassis_port().unwrap(),
        3653,
        "an empty variable is absent, not wrong"
    );

    // A REAL OVERRIDE still wins.
    std::env::set_var("THREADS_CHASSIS_PORT", "4111");
    assert_eq!(cfg.effective_chassis_port().unwrap(), 4111);

    // GARBAGE: refused, and the message names the variable and the value so an
    // operator reading one line knows which of their shells is wrong.
    for bad in ["garbage", "-1", "70000", "3653x", " "] {
        std::env::set_var("THREADS_CHASSIS_PORT", bad);
        let err = cfg
            .effective_chassis_port()
            .expect_err(&format!("{bad:?} is not a port and must be refused"));
        let text = err.to_string();
        assert!(
            text.contains("THREADS_CHASSIS_PORT") && text.contains(bad),
            "the refusal must name the variable and the value it refused: {text}"
        );
    }

    match prior {
        Some(v) => std::env::set_var("THREADS_CHASSIS_PORT", v),
        None => std::env::remove_var("THREADS_CHASSIS_PORT"),
    }
}
