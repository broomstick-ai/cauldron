//! # The shape chassis, proved
//!
//! Every test here was **forced red before it was made green** — a green that
//! has never failed is not evidence. Where forcing red needed a code change
//! rather than a fixture change, the exact mutation is named in the test's own
//! docstring so the next person can repeat it.
//!
//! The four invariants this file exists for:
//!
//! 1. **A connection that subscribed to no shape is handed no shape frame.**
//!    Until R2.S4 this was "the legacy wire is byte-identical": a connection
//!    that sent only `{"type":"subscribe","threads":null}` received ZERO
//!    `shape` frames. The firehose is retired, so that frame is now REFUSED
//!    (`malformed`) and the refused connection receives nothing at all across a
//!    full shape-changing scenario. No `Fact` can reach the daemon's push bus.
//! 2. **The unknown state has THREE values**, distinguishable on the wire:
//!    `known`, `not_hydrated`, `dead`.
//! 3. **A dead shape task produces EXACTLY ONE final export** and then refuses
//!    `shapes.subscribe` with an error, rather than answering `{queued: true}`
//!    to a consumer that would wait forever.
//! 4. **The fold never reads a clock**, and the roster count is the pool's.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::event::{Fact, Push};
use threads::shape::{
    CellState, Hub, Hydration, Input, Op, Outbox, Root, Shape, ShapeError, Stamped, Timestamp,
    Tracking, Tx,
};
use threads::{build_manager, ThreadManager};
use shapewire::cell::Cells;

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

fn set_env(tmp: &std::path::Path) {
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
}

async fn start() -> (ThreadManager, SocketAddr, tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "first".to_string(),
        tmp.path().join("first").to_string_lossy().to_string(),
    );
    accounts.insert(
        "second".to_string(),
        tmp.path().join("second").to_string_lossy().to_string(),
    );
    let config = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    };
    // THE CLOCK SEAM. Every `Hub::emit` stamps with the clock and every cell
    // refuses an input at or before its `folded_at`, so two emits in one
    // microsecond mean the second is DROPPED — which is what an optimized
    // build does to a tight emit loop. See `src/clock.rs`.
    threads::clock::testing::ticking();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let manager = build_manager(config).unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(
        &cfg,
        format!("data_dir = \"{}\"\n", tmp.path().display()),
    )
    .unwrap();
    let (addr, _h) = threads::chassis::serve(std::sync::Arc::new(manager.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (manager, addr, tmp, cfg)
}

/// A minimal WS client: every frame, in order, with nothing filtered.
struct Client {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    seen: Vec<Value>,
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
        let mut c = Self {
            ws,
            seen: Vec::new(),
        };
        c.next().await; // welcome
        c
    }

    async fn send(&mut self, v: Value) {
        use tokio_tungstenite::tungstenite::Message;
        self.ws
            .send(Message::Text(v.to_string().into()))
            .await
            .unwrap();
    }

    /// The next JSON frame, or `None` after 700 ms of silence.
    async fn next(&mut self) -> Option<Value> {
        use tokio_tungstenite::tungstenite::Message;
        loop {
            let got = tokio::time::timeout(std::time::Duration::from_millis(700), self.ws.next())
                .await;
            match got {
                Err(_) | Ok(None) => return None,
                Ok(Some(Ok(Message::Text(t)))) => {
                    let v: Value = serde_json::from_str(&t).ok()?;
                    self.seen.push(v.clone());
                    return Some(v);
                }
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) => return None,
            }
        }
    }

    /// Drain everything that arrives within the quiet window.
    async fn drain(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        while let Some(v) = self.next().await {
            out.push(v);
        }
        out
    }

    async fn request(&mut self, id: &str, method: &str, params: Value) -> Value {
        let params = identified(method, params);
        self.request_verbatim(id, method, params).await
    }

    /// **SEND THE PARAMS EXACTLY AS GIVEN**, with no identity added.
    ///
    /// The ONE way to send a subscribe that states no `consumer`, which
    /// `request` cannot do because `identified` fills one in. Exactly one test
    /// needs it — `an_absent_identity_is_refused_at_the_wire_and_so_is_a_blank_one`,
    /// whose whole subject is the ABSENT case — and it needs it precisely
    /// because the convenience helper would otherwise defeat it.
    ///
    /// **THIS IS NOT A HYPOTHETICAL.** That test first went green through
    /// `request` and had to be made to fail: the helper injected an identity,
    /// the door accepted the subscribe, and the assertion that absence is
    /// refused was being satisfied by a request that was not absent. A test for
    /// a refusal that silently stops sending the thing being refused is the
    /// false green this whole file's mutation notes exist to prevent.
    async fn request_verbatim(&mut self, id: &str, method: &str, params: Value) -> Value {
        self.send(json!({"type":"request","id":id,"method":method,"params":params}))
            .await;
        loop {
            let Some(f) = self.next().await else {
                panic!("no response to {method}");
            };
            if f["type"] == "response" && f["correlate"] == id {
                return f;
            }
        }
    }
}

/// A scenario that moves every produced fact this shape folds.
fn shake(m: &ThreadManager) {
    m.store.set_pool_share("first", 0.5, Some("t")).unwrap();
    m.emit_share_fact("first");
    m.budgets
        .observe(
            "first",
            threads::gauge::Axis::WeeklyAll,
            40.0,
            Some("2099-01-01T00:00:00Z".to_string()),
            "usage_poll",
        )
        .unwrap();
    m.emit_gauge_fact("first", threads::gauge::Axis::WeeklyAll);
    m.emit_limit_fact("first");
    m.emit_health_fact("second");
    m.produce_pressure();
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. THE LEGACY WIRE
// ─────────────────────────────────────────────────────────────────────────────

/// **`Fact` CANNOT REACH THE FIREHOSE.**
///
/// This is the byte-identity argument at its root: `Fact` is a separate enum
/// from `Push`, so `Hub::emit_fact` has no expressible path to
/// `firehose.send`. The test emits every variant and asserts the firehose
/// stayed silent — then emits one real `Push` through the same hub to prove the
/// receiver was working and the silence was not the test's own bug.
///
/// **Forced red** by making `emit_fact` call `emit` (one line in
/// `shape.rs::Hub::emit_fact`): the assertion below fires on the first fact.
#[tokio::test]
async fn a_fact_never_reaches_the_firehose() {
    let (m, _addr, tmp, _cfg) = start().await;
    let mut rx = m.hub.firehose.subscribe();

    for f in [
        Fact::LimitCleared {
            account: "first".into(),
        },
        Fact::PoolShare {
            account: "first".into(),
            row: json!({}),
        },
        Fact::AccountRemoved {
            name: "first".into(),
        },
        Fact::GaugeObserved {
            account: "first".into(),
            axis: "weekly_all".into(),
            row: json!({}),
        },
        // ── THE THREAD FACTS (2026-09-26) ────────────────────────────────
        //
        // The first facts on this producer that are NOT about an account, which
        // is why they are listed here rather than left to the account four
        // above: the byte-identity argument is about the ENUM, so a new variant
        // of a new KIND is exactly the case where "it cannot reach the firehose"
        // should be re-asserted rather than assumed to still hold.
        Fact::ThreadHeader {
            thread_id: "t-1".into(),
            row: threads::shapes::threads::HeaderRow {
                cwd: None,
                model: None,
                created_at: "2026-09-26T00:00:00Z".into(),
                updated_at: "2026-09-26T00:00:00Z".into(),
            },
        },
        Fact::ThreadSettings {
            thread_id: "t-1".into(),
            settings: json!({"model": "fable"}),
        },
        Fact::ThreadPlacement {
            thread_id: "t-1".into(),
            account: Some("first".into()),
        },
        Fact::ThreadSeen {
            thread_id: "t-1".into(),
            seen_seq: 3,
            unread: 1,
        },
        Fact::ThreadCompaction {
            thread_id: "t-1".into(),
            row: threads::shapes::threads::CompactionRow::default(),
        },
    ] {
        m.hub.emit_fact(f);
    }
    assert!(
        rx.try_recv().is_err(),
        "a Fact reached the firehose — the legacy wire is no longer byte-identical"
    );

    // …and the receiver is alive, so the silence above means something.
    m.hub.emit(Push::Info {
        domain: "accounts".into(),
        reason: "test".into(),
        account: None,
        detail: None,
        at: "now".into(),
    });
    assert!(
        rx.try_recv().is_ok(),
        "the firehose receiver was dead, so the assertion above proved nothing"
    );
    drop(tmp);
}

/// **A LEGACY SUBSCRIBE IS REFUSED, AND THE REFUSED CONNECTION IS HANDED
/// NOTHING WHILE A SHAPE IS LIVE.**
///
/// RE-CUT AT R2.S4. Before: the legacy client's `{"type":"subscribe",
/// "threads":null}` was acked (`sub_ack`) and then, while a SECOND connection
/// held a live shape subscription and a shape-changing scenario ran, it was
/// handed only frame types that existed before shapes — never `shape`. Now the
/// same frame is answered with ONE `malformed` refusal, and across the same
/// scenario the refused connection is handed NOTHING — no `shape` frame and no
/// legacy push — while the board (the positive half) is handed shape frames.
///
/// **Forced red** (R2.S4) by restoring the chassis's legacy control half
/// (`subscribe` acked with a `sub_ack`): `a legacy subscribe is refused, not
/// acked: {"firehose":true,"threads":[],"type":"sub_ack"}`.
#[tokio::test]
async fn a_legacy_connection_sees_no_shape_frame_while_a_shape_is_live() {
    let (m, addr, tmp, _cfg) = start().await;

    // The legacy consumer.
    let mut legacy = Client::connect(addr).await;
    legacy.send(json!({"type":"subscribe","threads":null})).await;
    let refusal = legacy.next().await.unwrap();
    assert_eq!(
        refusal["outcome"]["code"],
        json!(shapewire::envelope::codes::MALFORMED),
        "a legacy subscribe is refused, not acked: {refusal}"
    );

    // A shape consumer on its own connection.
    let mut board = Client::connect(addr).await;
    let r = board
        .request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;
    assert_eq!(r["outcome"]["payload"]["queued"], json!(true), "{r}");
    board.drain().await;

    shake(&m);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let board_frames = board.drain().await;
    assert!(
        board_frames.iter().any(|f| f["type"] == "shape"),
        "the shape consumer got no frames at all, so the legacy assertion below is vacuous: \
         {board_frames:?}"
    );

    let legacy_frames = legacy.drain().await;
    assert!(
        legacy_frames.is_empty(),
        "a refused legacy connection was handed frames: {legacy_frames:?}"
    );
    drop(tmp);
}

/// **ZERO SHAPE FRAMES WHILE NOBODY HAS SUBSCRIBED** — the `Lazy` half of the
/// same invariant. With no `shapes.subscribe` anywhere in the process, the
/// shape is not even tracking, so the producers do no work at all.
///
/// RE-CUT AT R2.S4. Before: the idle connection had sent a legacy firehose
/// subscribe (acked), and was handed no shape frames. Now no legacy subscribe
/// exists to send; the idle connection sends nothing, is handed nothing across
/// the scenario, and the shape is STILL untracked after it — the "costs
/// nothing" half asserted after the scenario, not only before it.
#[tokio::test]
async fn an_untracked_shape_costs_nothing_and_emits_nothing() {
    let (m, addr, tmp, _cfg) = start().await;
    assert!(
        !m.hub.tracked("accounts"),
        "a Lazy shape must not track before its first subscriber"
    );
    let mut idle = Client::connect(addr).await;
    shake(&m);
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let frames = idle.drain().await;
    assert!(frames.is_empty(), "{frames:?}");
    assert!(
        !m.hub.tracked("accounts"),
        "a scenario with no subscriber must not start the shape tracking"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. THE EXPORT, THE COUNT, AND THE THREE-VALUED UNKNOWN
// ─────────────────────────────────────────────────────────────────────────────

/// **THE EXPORT'S SHAPE, THE COUNT, AND `default` NOT BEING A KEY.**
///
/// `shapes.list` must report exactly the pool's account names — "4 accounts
/// rendered as 0" is the board defect the `PoolRoster` handler's `Complete`
/// contract exists to make impossible — and `default` must not be among them,
/// because the machine login is a NOTE on the account it folds into, never a
/// row of its own.
#[tokio::test]
async fn the_export_carries_the_row_the_shared_scalars_and_the_pools_own_count() {
    let (m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;
    c.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;

    let mut export = None;
    for _ in 0..12 {
        let Some(f) = c.next().await else { break };
        if f["type"] == "shape" && f["op"] == "read" {
            export = Some(f);
            break;
        }
    }
    let e = export.expect("no export arrived");
    assert_eq!(e["shape"], json!("accounts"));
    assert_eq!(e["key"], json!("first"));
    assert_eq!(e["row"]["account"], json!("first"));
    assert!(e["as_of"].is_string(), "an export carries the REAL fold point: {e}");
    assert!(e["at"].is_null(), "an export is not an occurrence: {e}");
    assert_eq!(e["epoch"]["state"], json!("known"));

    // RULING 6 — the shape-level scalars have a home, and the threshold is on
    // the wire so no consumer hardcodes it.
    assert_eq!(
        e["shared"]["warn_below_days"],
        json!(threads::accounts::HEALTH_WARN_BELOW_DAYS),
        "warn_below_days must ride on the export: {e}"
    );
    assert!(
        e["shared"].get("folds_into").is_some(),
        "the folds_into relationship is shape-level state: {e}"
    );

    let list = c.request("2", "shapes/list", json!({})).await;
    let s = &list["outcome"]["payload"]["shapes"][0];
    assert_eq!(s["name"], json!("accounts"));
    assert_eq!(s["tracking"], json!("lazy"));
    assert_eq!(s["tracked"], json!(true));
    let keys: Vec<String> = serde_json::from_value(s["keys"].clone()).unwrap();
    let mut pool = m.account_pool();
    pool.sort();
    let mut got = keys.clone();
    got.sort();
    assert_eq!(
        got, pool,
        "the shape's keys ARE the pool's account list — a handler that answered \
         Complete(vec![]) for a populated pool would show here"
    );
    assert!(
        !keys.iter().any(|k| k == "default"),
        "RULING 5: `default` is never a key — it is a note on the account it folds into: {keys:?}"
    );
    drop(tmp);
}

/// **`accounts` IS VERSION 2: THE `threads` CELL IS GONE, AND A VERSION-1 LINK
/// IS REFUSED AT SUBSCRIBE (R2.S8).**
///
/// The cell was six fields of `threads`' row nested under an account key — the
/// one violation of the nesting law on this producer — and its roster is
/// `threads/non-idle` now. Removing a cell a consumer read is a contract change,
/// so the shape's `VERSION` moved to 2 with `SERVES` left empty, and rule 2.6's
/// third state is what a consumer built against version 1 gets: REFUSED, on
/// its request id, with `version_refused`, naming `[2]`.
///
/// Opposed, so no constant producer passes: the version-2 subscribe of the SAME
/// key must be SERVED with a real row (the positive sentinel), the version-1 one
/// must be refused, and the served row must carry `account` but not `threads`.
///
/// **THE MUTATIONS.** (1) `Accounts::VERSION` back to 1: the version-1 subscribe
/// is served and the first assertion fires. (2) `"threads": {}` put back into
/// `AccountRow::to_json`: the served row carries it and the row assertion fires.
#[tokio::test]
async fn accounts_is_version_2_without_threads_and_a_version_1_link_is_refused() {
    let (_m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;

    let refused = c
        .request("1", "shapes/accounts/subscribe", json!({"key": "first", "version": 1}))
        .await;
    // This file's fixture served the LEGACY door until R2.S4, which carried no
    // `code` on any error; it serves the chassis now (the only door), whose
    // code for this refusal is pinned by `chassis_api.rs`'s
    // `a_version_refusal_carries_version_refused_and_an_unknown_method_its_own`.
    // What is pinned here is that the link is refused at all, and its prose.
    assert_eq!(
        refused["outcome"]["type"],
        json!("error"),
        "a consumer built against accounts v1 read the `threads` cell, which this \
         producer no longer serves — rule 2.6 REFUSES that link: {refused}"
    );
    let text = refused["outcome"]["error"].as_str().unwrap_or_default().to_string();
    assert!(
        text.contains("built against v1") && text.contains("[2]"),
        "the refusal names the version wanted and the versions served: {text}"
    );

    let served = c
        .request("2", "shapes/accounts/subscribe", json!({"key": "first", "version": 2}))
        .await;
    assert_eq!(
        served["outcome"]["type"],
        json!("success"),
        "POSITIVE SENTINEL: version 2 is the current contract and is served: {served}"
    );
    let mut export = None;
    for _ in 0..12 {
        let Some(f) = c.next().await else { break };
        if f["type"] == "shape" && f["op"] == "read" && f["shape"] == "accounts" {
            export = Some(f);
            break;
        }
    }
    let e = export.expect("the version-2 subscribe is answered by an export");
    assert_eq!(e["row"]["account"], json!("first"), "a real row: {e}");
    assert!(
        e["row"].get("threads").is_none() && e["cells"].get("threads").is_none(),
        "`threads` is gone from the row AND from its cells block: {e}"
    );
    drop(tmp);
}

/// **THE UNKNOWN STATE HAS THREE VALUES, NOT TWO.**
///
/// Operator, verbatim: *"we need ? handled by subscribers and there should be
/// values associated with it: not hydrated vs error."*
///
/// In this fixture the two accounts are in the registry with no login on disk, so
/// `credential` was READ and found nothing — which is `known` with a value of
/// `no-login`, because empty is a statement and not an absence. The cell nothing
/// reached is `machine_default`: the machine-default store is not signed in as
/// this account, so the note was never populated and says so with a reason. The
/// third value, `dead`, is proved by the task-death test below; this one asserts
/// the first two are distinguishable and that `not_hydrated` is not silently
/// spelled the same way as an empty `known`.
///
/// **THE MUTATION, corrected 2026-09-26.** Make `CellState::NotHydrated`
/// serialise under the `known` tag (`#[serde(rename = "known")]` on the variant)
/// — the collapse this rule exists to forbid, in one line.
///
/// Forced red:
/// ```text
/// the machine default does not fold into this account, so the note cell was
/// never populated and must say so rather than render as an empty known:
/// left: String("known")  right: String("not_hydrated")
/// ```
///
/// **What this block said before, and why it was worse than vague.** It claimed
/// the fixture made every `credential` cell `not_hydrated` — which the test four
/// assertions down contradicts in as many words — and it named a two-part
/// mutation, half of which is provably INERT here: no assertion in this test
/// mentions `dead`, as the paragraph above says itself, so deleting `Dead` cannot
/// turn it red. It then claimed "every assertion below fails", where the true
/// blast radius is three of nine. A forced-red note that overstates its reach is
/// read as proof that more is pinned than is pinned.
#[tokio::test]
async fn a_cell_says_which_kind_of_unknown_it_is() {
    let (_m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;
    c.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;

    let mut export = None;
    for _ in 0..12 {
        let Some(f) = c.next().await else { break };
        if f["type"] == "shape" && f["op"] == "read" {
            export = Some(f);
            break;
        }
    }
    let e = export.expect("no export arrived");
    let cells = &e["cells"];

    // A cell a handler REACHED.
    assert_eq!(
        cells["registered"]["state"],
        json!("known"),
        "the roster hydrated, so `registered` is known: {cells}"
    );
    assert!(
        cells["registered"]["folded_at"].is_string(),
        "a known cell carries the stamp that set it: {cells}"
    );

    // A cell that WAS read and found nothing is still `known` — empty renders
    // as empty. This fixture's slots exist in the registry with no login on
    // disk, so `credential` is a real answer ("no-login"), not a `?`.
    assert_eq!(
        cells["credential"]["state"],
        json!("known"),
        "a slot that was read and has no login is KNOWN — empty is a statement, \
         not an absence: {cells}"
    );
    assert_eq!(
        e["row"]["credential"]["slot_state"],
        json!("no-login"),
        "…and the known value says exactly what was found: {e}"
    );

    // A cell NOTHING reached — and it says why, which is the whole point.
    assert_eq!(
        cells["machine_default"]["state"],
        json!("not_hydrated"),
        "the machine default does not fold into this account, so the note cell \
         was never populated and must say so rather than render as an empty \
         known: {cells}"
    );
    assert!(
        cells["machine_default"]["reason"].is_string(),
        "`not hydrated` without a reason is the `?` the operator asked us to \
         stop rendering: {cells}"
    );

    // The two unknowns present on one export are distinguishable from `known`
    // and from each other's spelling; `dead` — the third — is proved by
    // `a_dead_shape_task_tells_every_mirror_once_and_then_refuses`.
    assert_ne!(cells["registered"]["state"], cells["machine_default"]["state"]);
    for state in ["known", "not_hydrated"] {
        assert!(
            cells.as_object().unwrap().values().any(|c| c["state"] == state),
            "the export must be able to say `{state}`: {cells}"
        );
    }
    drop(tmp);
}

/// A live fact reaches a subscribed mirror as a `row` upsert carrying one `at`
/// and no `cells` block — per-cell provenance rides on the export only.
#[tokio::test]
async fn a_produced_fact_reaches_a_subscribed_mirror_as_a_row() {
    let (m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;
    c.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;
    c.drain().await;

    m.store.set_pool_share("first", 0.25, Some("note")).unwrap();
    m.emit_share_fact("first");
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;

    let frames = c.drain().await;
    let row = frames
        .iter()
        .find(|f| f["type"] == "shape" && f["event"]["kind"] == "row")
        .unwrap_or_else(|| panic!("no row event: {frames:?}"));
    assert_eq!(row["op"], json!("live"));
    assert!(row["at"].is_string(), "a live frame carries one `at`: {row}");
    assert!(
        row["cells"].is_null(),
        "per-cell provenance rides on the EXPORT only: {row}"
    );
    assert_eq!(row["event"]["row"]["share"]["share"], json!(0.25), "{row}");
    drop(tmp);
}

/// **A PROMISE ID REACHES THE CONSUMER THAT ISSUED THE COMMAND** —
/// `docs/shapes.md` 2.4, written from the consumer's position rather than from
/// the book's.
///
/// # The defect this test exists for
///
/// Three pieces composed into one silent failure:
///
/// 1. `PromiseBook::resolve::<E>()` matched on TYPE ALONE and closed every open
///    promise of that type, whatever account it was about.
/// 2. `ShapeTask::flush_rows` took the whole accumulated id bag and put it on
///    the FIRST key's row frame (`if first { resolving.clone() }`).
/// 3. `server::drive` delivers a keyed frame only to connections in
///    `shape_subs` for that exact `(shape, key)`.
///
/// So a consumer subscribed to ONE key, issuing a command about THAT key, got
/// an acknowledgement and then nothing: the ids rode whichever key flushed
/// first — `first` sorts before `second` — and the book had already discarded
/// the promise, so no later frame could carry it either. It waited forever on a
/// promise nobody still held.
///
/// It was invisible because every existing test subscribes KEYLESSLY, and a
/// keyless subscribe on a keyed shape expands to one subscription per key: such
/// a connection holds `first` too and does receive the frame the ids rode.
///
/// # Why this fixture forces it
///
/// `accounts.disable` calls `emit_disabled_fact` (which dirties `second` and
/// carries the id) and then `emit_share_fact`, whose `produce_pressure` emits a
/// `Pressure` fact for EVERY pool account — so `first` is dirty inside the same
/// 100 ms debounce window and is the first key the flush walks. The connection
/// holds `second` alone, which is exactly the consumer the defect strands.
#[tokio::test]
async fn a_promise_id_rides_the_frame_for_the_key_its_command_was_about() {
    let (m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;

    // ONE key, and deliberately not the first one the flush would walk.
    c.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"second"}))
        .await;
    c.drain().await;

    let ack = c
        .request("2", &format!("accounts/{}/disable", "second"), json!({}))
        .await;
    let id = ack["outcome"]["payload"]["promise_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("a command acknowledges with a promise id: {ack}"));

    // Past the row debounce, and read EVERY frame this connection has ever been
    // sent (`seen`), not just what is still queued: the flush can land while
    // `request` is still waiting for its response, and a frame that arrives then
    // is delivered — which is the only thing this test is about — but is not in
    // `drain`'s window any more.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    c.drain().await;
    let delivered: Vec<(Value, Value)> = c
        .seen
        .iter()
        .filter(|f| f["type"] == "shape")
        .map(|f| (f["key"].clone(), f["promises"].clone()))
        .collect();

    let carriers: Vec<&Value> = c
        .seen
        .iter()
        .filter(|f| {
            f["type"] == "shape"
                && f["promises"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(id)))
        })
        .collect();
    assert!(
        !carriers.is_empty(),
        "promise {id} never reached the consumer that issued the command. The \
         ids rode a key this connection does not hold, and the book has already \
         closed the promise, so nothing later can carry it. \
         (key, promises) delivered here: {delivered:?}"
    );
    for f in &carriers {
        assert_eq!(
            f["key"],
            json!("second"),
            "the resolving fact is a fact about the subject the command named: {f}"
        );
    }
    // The liveness half, on the same run: the book closed it.
    assert!(
        m.promises.is_empty(),
        "no promise left open: {:?}",
        m.promises.open_ids()
    );

    // ── AND AGAIN, ON THE NO-OP ──────────────────────────────────────────
    //
    // `second` is already disabled, so nothing moves in the store. The fact is
    // re-asserted and the new id must arrive on the same key's frame — a
    // re-assertion is the only way a consumer can tell "it took" from "it was
    // never emitted", and a level absorbs it for free. It is here because the
    // routing depends on the subject's key being DIRTY: a cell that skipped an
    // identical write would leave the id with no frame to ride.
    let before = c.seen.len();
    let ack = c
        .request("3", &format!("accounts/{}/disable", "second"), json!({}))
        .await;
    let again = ack["outcome"]["payload"]["promise_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("acknowledged: {ack}"));
    assert_ne!(again, id, "a second command is a second promise");
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    c.drain().await;
    assert!(
        c.seen[before..].iter().any(|f| {
            f["type"] == "shape"
                && f["key"] == json!("second")
                && f["promises"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(again)))
        }),
        "the re-assertion carried promise {again} on `second`'s own frame: {:?}",
        &c.seen[before..]
    );
    drop(tmp);
}

/// **TWO WRITERS, TWO KEYS, TWO PROMISES — AND NEITHER CLOSES THE OTHER'S.**
///
/// **THE PROMISE-RECOVERY HOLE, FROM THE CONSUMER'S CHAIR** — rule 2.5 applied to
/// promises rather than to cells.
///
/// # The composed failure
///
/// A consumer issues a command and gets `{promise_id}`. Its socket dies before the
/// frame carrying that id arrives — a lag, a reconnect, a laptop lid. Rule 2.5's
/// remedy for both is *re-subscribe*, and it does. And then:
///
/// * the re-export hard-coded `promises: Vec::new()`, so it carried nothing;
/// * the book had already closed the id at emit, so no LATER frame could carry it
///   either.
///
/// The promise was gone, permanently, with nothing on the producer that still knew
/// it had been owed. The consumer's only recourse was the policy rule 2.5 exists to
/// forbid: *notice a promise did not resolve, decide it has been too long, ask
/// again* — a timeout and a retry in every client.
///
/// # What this test does, and why both halves are here
///
/// **Phase 1** drops the socket INSIDE the 100 ms row debounce, so the resolving
/// fact folded and no frame ever went out. The id survives in the task's
/// `pending_promises`, and the re-export must spend it.
///
/// **Phase 2** waits PAST the debounce first, so the frame really was emitted — into
/// a channel whose reader is gone. Nothing pending is left; only
/// `ShapeTask::carried` can answer, which is the retention this fix added.
///
/// Phase 1 alone would pass against a fix that only carried `pending_promises`, and
/// that is the weaker half: a lagged consumer's frames are frames that WERE sent.
///
/// **Forced red** by restoring `promises: Vec::new()` in `ShapeTask::export`.
#[tokio::test]
async fn a_promise_survives_the_connection_and_rides_the_re_export() {
    let (m, addr, tmp, _cfg) = start().await;

    // ── PHASE 1: the frame never went out ────────────────────────────────────
    let mut c = Client::connect(addr).await;
    c.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"second"}))
        .await;
    c.drain().await;
    assert!(
        m.hub.any_tracking(),
        "if nothing tracks, `accounts.disable` takes the not-tracking path and \
         this test asserts against code that did not run"
    );

    let ack = c
        .request("2", &format!("accounts/{}/disable", "second"), json!({}))
        .await;
    let id = ack["outcome"]["payload"]["promise_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("acknowledged: {ack}"));
    assert_eq!(
        ack["outcome"]["payload"]["visible"],
        json!(true),
        "a frame IS coming — this consumer is subscribed: {ack}"
    );

    // Gone before the debounce expires. `DEBOUNCE_MS` is 100; 30 ms is inside it.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    drop(c);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // The consumer comes back — a NEW socket, a new `ConnId`, and the one remedy
    // rule 2.5 gives it.
    let mut again = Client::connect(addr).await;
    again
        .request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"second"}))
        .await;
    let frames = again.drain().await;
    assert!(
        frames.iter().any(|f| {
            f["type"] == "shape"
                && f["op"] == json!("read")
                && f["promises"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(id)))
        }),
        "the re-export must carry promise {id}. It does not, so the consumer can \
         never learn that its command took — the book closed the promise at emit \
         and no frame will ever carry it again. Frames: {frames:#?}"
    );

    // ── PHASE 2: the frame DID go out, to a socket that had gone ─────────────
    let ack = again
        .request("2", &format!("accounts/{}/disable", "first"), json!({}))
        .await;
    let id2 = ack["outcome"]["payload"]["promise_id"].as_u64().unwrap();
    // Past the debounce: the row frame is emitted for real…
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // …and this connection is subscribed to `second`, not `first`, so it never
    // received it — exactly as a lagged or dead connection would not have.
    assert!(
        !again.seen.iter().any(|f| {
            f["promises"]
                .as_array()
                .is_some_and(|a| a.contains(&json!(id2)))
        }),
        "the fixture is wrong if this connection already saw {id2}"
    );
    let mut third = Client::connect(addr).await;
    third
        .request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;
    let frames = third.drain().await;
    assert!(
        frames.iter().any(|f| {
            f["type"] == "shape"
                && f["op"] == json!("read")
                && f["promises"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(id2)))
        }),
        "an export must RE-OFFER an id a lost frame already carried — that is the \
         half `pending_promises` alone cannot answer. Frames: {frames:#?}"
    );
    drop(tmp);
}

/// **A SUBSCRIBE IS MATCHABLE TO THE EXPORT THAT SATISFIES IT** — rule 2.7's
/// `accepted` / `exported` for `shapes.subscribe`.
///
/// `docs/shapes.md` 2.4 calls `{queued: true}` *"an acknowledgement in spirit and
/// anonymous in practice — the export that resolves it cannot be matched to it"*,
/// and names it open. The consequence is in the board: `resync()` waits on an
/// `exports` COUNTER with a deadline, which is a timeout and a retry policy in a
/// client, which is what rule 2.5 exists to keep out of clients.
///
/// **Forced red** by returning `Vec::new()` for `subscribes` in `ShapeTask::export`.
#[tokio::test]
async fn a_subscribe_is_answered_by_an_export_that_names_it() {
    let (_m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;

    let ack = c
        .request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;
    let sub = ack["outcome"]["payload"]["subscribe_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("a subscribe is acknowledged by NAME: {ack}"));
    assert_eq!(ack["outcome"]["payload"]["queued"], json!(true), "still: {ack}");
    let frames = c.drain().await;
    let mine: Vec<&Value> = frames
        .iter()
        .filter(|f| {
            f["type"] == "shape"
                && f["subscribes"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(sub)))
        })
        .collect();
    assert_eq!(
        mine.len(),
        1,
        "exactly the one export this call asked for names it: {frames:#?}"
    );
    assert_eq!(mine[0]["key"], json!("first"));
    assert_eq!(mine[0]["op"], json!("read"));

    // A SECOND subscribe is a second id, and its own export names only it — which
    // is the whole point: two asks in flight are told apart.
    let ack = c
        .request("2", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"second"}))
        .await;
    let sub2 = ack["outcome"]["payload"]["subscribe_id"].as_u64().unwrap();
    assert_ne!(sub, sub2, "a second ask is a second id");
    let frames = c.drain().await;
    let mine: Vec<&Value> = frames
        .iter()
        .filter(|f| {
            f["type"] == "shape"
                && f["subscribes"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(sub2)))
        })
        .collect();
    assert_eq!(mine.len(), 1, "{frames:#?}");
    assert_eq!(mine[0]["key"], json!("second"));
    assert!(
        !frames.iter().any(|f| f["subscribes"]
            .as_array()
            .is_some_and(|a| a.contains(&json!(sub)))),
        "and the FIRST ask is not answered a second time: {frames:#?}"
    );
    drop(tmp);
}

/// The book's own unit tests
/// (`shapewire::command::tests::a_fact_about_one_subject_leaves_another_subjects_promise_open`)
/// pin the rule; this pins it through the real producer, where the subject comes
/// off `emit_disabled_fact`'s own argument rather than out of a test's hand.
///
/// Before the fix this was the concurrent form of the same defect: `first`'s
/// fact closed `second`'s promise, so the consumer waiting on `second` was told
/// nothing and the daemon had no record that it was still owed anything.
#[tokio::test]
async fn one_accounts_fact_leaves_another_accounts_promise_open() {
    let (m, addr, tmp, _cfg) = start().await;
    // Tracking must be on, or `emit_disabled_fact` takes the not-tracking early
    // return — the one path where a promise closes without an emit — and this
    // test would pass against code that does nothing.
    let mut c = Client::connect(addr).await;
    c.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;
    c.drain().await;
    assert!(m.hub.any_tracking(), "the assertion below would be vacuous");

    // A promise about `second`, opened by hand because a handler emits before it
    // returns: there is no other way to hold one open across another account's
    // fact.
    let waiting = m
        .promises
        .open::<threads::commands::AccountsDisable>(Some("second".to_string()));

    // A fact about `first`. Same TYPE, different SUBJECT.
    m.store.set_account_disabled("first", true, None).unwrap();
    m.emit_disabled_fact("first");

    assert_eq!(
        m.promises.open_ids(),
        vec![(
            waiting.id(),
            "accounts/{account}/disable",
            Some("second".to_string())
        )],
        "a fact about `first` must not answer a command about `second`"
    );

    // And `second`'s own fact closes it.
    m.store.set_account_disabled("second", true, None).unwrap();
    m.emit_disabled_fact("second");
    assert!(
        m.promises.is_empty(),
        "the subject's own fact closes it: {:?}",
        m.promises.open_ids()
    );
    drop(tmp);
}

/// **§7.1 CLOSED: THE UNHOSTED MACHINE LOGIN IS SAYABLE WITHOUT PARSING PROSE.**
///
/// `SHAPES-TUI.md` §7.1 shipped a known gap at S2. The board draws *"this is
/// your machine login, and no named account carries it yet — press `n` to give
/// it a slot"*, and under this shape it could not be drawn:
///
/// * `default` is never a key (ruling 5), so there is no card;
/// * `shared.folds_into` is `{}` BOTH when there is no machine-default store
///   and when there is one nothing hosts;
/// * the only discriminator was the free-text `reason` on each row's
///   `machine_default` cell, and the client correctly refused to branch on it.
///
/// This fixture IS the unhosted case: two accounts are registered with config
/// dirs that do not exist, so no pool login matches the machine-default store's
/// identity and nothing hosts it. The affordance's condition has to be readable
/// from a typed field, on the keyless `shared` block, with `default` still not
/// a key.
///
/// **THE MUTATION.** Delete the `set_shared_machine_login` call from
/// `Accounts::hydrates` and every assertion below goes red — including, and this
/// is the point, the one that distinguishes "unhosted" from "no machine login
/// at all", which `folds_into` alone never could.
#[tokio::test]
async fn the_unhosted_machine_login_is_a_typed_field_on_the_shared_block() {
    let (m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;
    c.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;

    let mut export = None;
    for _ in 0..12 {
        let Some(f) = c.next().await else { break };
        if f["type"] == "shape" && f["op"] == "read" {
            export = Some(f);
            break;
        }
    }
    let e = export.expect("no export arrived");

    // THE FIELD EXISTS, STRUCTURALLY, and is not a string to be parsed.
    let ml = &e["shared"]["machine_login"];
    assert!(
        !ml.is_null(),
        "a machine-default store WAS read in this fixture, so `machine_login` is \
         a statement and not an absence: {e}"
    );
    assert_eq!(
        ml["unhosted"],
        json!(true),
        "no pool account carries this machine login — this ONE boolean is the \
         affordance's condition: {e}"
    );
    assert!(ml["hosted_by"].is_null(), "{e}");
    assert!(ml["label"].is_string(), "{e}");

    // AND IT IS THE THING `folds_into` COULD NOT SAY. The empty map below is
    // exactly what a HOSTED-nothing and a NO-MACHINE-LOGIN machine both produce,
    // which is why §7.1 could not branch on it.
    assert_eq!(
        e["shared"]["folds_into"],
        json!({}),
        "the gap's premise, asserted so this test fails loudly if it changes: {e}"
    );

    // `default` IS STILL NOT A KEY. The affordance is a fact about the MACHINE,
    // which is exactly why it belongs in `shared` and not in a row.
    let list = c.request("2", "shapes/list", json!({})).await;
    let keys: Vec<String> =
        serde_json::from_value(list["outcome"]["payload"]["shapes"][0]["keys"].clone()).unwrap();
    assert!(
        !keys.iter().any(|k| k == "default"),
        "ruling 5 is not what was relaxed to close §7.1: {keys:?}"
    );

    // The cell's own provenance is on the export too, so a consumer can still
    // tell "read, and nothing hosts it" from "never read".
    assert_eq!(e["shared_cells"]["machine_login"]["state"], json!("known"), "{e}");
    let _ = m;
    drop(tmp);
}

/// **INCLUSION IS ITS OWN CELL, AND IT ROUND-TRIPS BESIDE A NON-ZERO SHARE.**
///
/// The operator, 2026-09-20: *"I want disabled to be independent of share.
/// Whether to include it in the pool supersedes how much should be made
/// available to the pool."* On the shape that means own fact, own cell, own
/// producer — not a field inside the `share` blob — and the discriminating
/// assertion is a row carrying `disabled: true` and `share: 0.6` at once.
///
/// **THE MUTATION.** Delete the `Fact::AccountDisabled` arm from
/// `Accounts::folds` (or the `disabled` cell from `AccountRow`) and the live
/// half goes red; delete the `DisabledAccounts` hydrate and the export half does.
#[tokio::test]
async fn the_disabled_fact_is_its_own_cell_and_rides_beside_a_real_share() {
    let (m, addr, tmp, _cfg) = start().await;

    // He lent 60% and then took the account out. Two acts, two rows, two facts.
    m.store.set_pool_share("first", 0.6, Some("lent 60%")).unwrap();
    m.store
        .set_account_disabled("first", true, Some("not a paid account any more"))
        .unwrap();

    let mut c = Client::connect(addr).await;
    c.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;
    let mut export = None;
    for _ in 0..12 {
        let Some(f) = c.next().await else { break };
        if f["type"] == "shape" && f["op"] == "read" {
            export = Some(f);
            break;
        }
    }
    let e = export.expect("no export arrived");

    // ── HYDRATE: the cell exists, is `known`, and carries the state.
    assert_eq!(e["cells"]["disabled"]["state"], json!("known"), "{e}");
    assert_eq!(e["row"]["disabled"]["disabled"], json!(true), "{e}");
    assert!(e["row"]["disabled"]["disabled_at"].is_string(), "{e}");
    assert_eq!(
        e["row"]["disabled"]["note"],
        json!("not a paid account any more"),
        "{e}"
    );
    // ── AND THE SHARE BESIDE IT IS THE REAL ONE. This is the whole change in
    // one assertion: two true facts about one account, neither derived from the
    // other, on one row at one instant.
    assert_eq!(e["row"]["share"]["share"], json!(0.6), "{e}");
    assert_eq!(e["row"]["share"]["disabled"], json!(true), "{e}");
    // The other account is untouched, so none of the above is a payload-wide
    // default that would have passed for any input.
    assert_eq!(e["shared_cells"].get("machine_login").is_some(), true);

    // ── LIVE: the enable moves the `disabled` cell ON ITS OWN FACT.
    c.drain().await;
    m.store.set_account_disabled("first", false, None).unwrap();
    m.emit_disabled_fact("first");
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    let frames = c.drain().await;
    let row = frames
        .iter()
        .rev()
        .find(|f| f["type"] == "shape" && f["event"]["kind"] == "row")
        .unwrap_or_else(|| panic!("no row event: {frames:?}"));
    assert_eq!(row["event"]["row"]["disabled"]["disabled"], json!(false), "{row}");
    assert!(row["event"]["row"]["disabled"]["disabled_at"].is_null(), "{row}");
    assert_eq!(
        row["event"]["row"]["share"]["share"],
        json!(0.6),
        "the enable supplied no number and moved none: {row}"
    );
    drop(tmp);
}

/// **THE FOLD NEVER READS A CLOCK.** Mechanically checked: `folds` and every
/// cell setter take the input's stamp, so a clock call inside the fold body is
/// a source-level defect this test names.
#[test]
fn the_fold_reads_no_clock() {
    let src = include_str!("../../src/shapes/accounts.rs");
    let start = src
        .find("fn folds(")
        .expect("Accounts::folds was renamed — re-point this test");
    let body = &src[start..];
    let end = body
        .find("\n    }\n}")
        .expect("the end of folds moved — re-point this test");
    let body = &body[..end];
    // `clock::now` is on this list because `src/clock.rs` would otherwise be a
    // NEW DOOR THROUGH THIS BAN: the seam exists so that a compared instant goes
    // through one place, and a fold is the one body that must read no instant at
    // all. A seam that let a banned read back in under a different name would be
    // worse than no seam.
    for bad in [
        "Utc::now",
        "Local::now",
        "clock::now",
        "now_rfc3339",
        "Instant::now",
        "SystemTime::now",
    ] {
        assert!(
            !body.contains(bad),
            "`{bad}` appears inside Accounts::folds — the fold must take its only \
             notion of time from the input's stamp (`tx.at()`)"
        );
    }
    assert!(
        body.contains("tx.at()"),
        "folds must take the stamp from the input, or this test proves nothing"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. TASK SUPERVISION
// ─────────────────────────────────────────────────────────────────────────────

/// A shape whose fold panics on command — the only way to exercise the death
/// path without waiting for a real bug.
struct Boom;

#[derive(Default)]
struct BoomRoot {
    n: u64,
    dirty: bool,
}

impl Root for BoomRoot {
    fn keys(&self) -> Vec<String> {
        vec!["k".to_string()]
    }
    fn row(&self, _: &str) -> Option<Value> {
        Some(json!({ "n": self.n }))
    }
    fn cells(&self, _: &str) -> Option<Value> {
        Some(json!({ "n": CellState::Known {
            complete: true,
            folded_at: chrono::Utc::now(),
            since_init: false,
            covers_since: None,
        }}))
    }
    fn shared(&self) -> Value {
        json!({})
    }
    fn shared_cells(&self) -> Value {
        json!({})
    }
    fn drain(&mut self) -> Outbox {
        Outbox {
            rows: if std::mem::take(&mut self.dirty) {
                vec!["k".to_string()]
            } else {
                vec![]
            },
            ..Default::default()
        }
    }
    fn mark_dead(&mut self, _: &str) {}
}


// The cells this file's test roots emit, declared — `Shape::CELLS` is required
// (see `shapewire::cell`), and a test shape declares what its `Root::cells`
// actually spells.
shapewire::declare_cells! {
    N_CELLS = [("n", shapewire::operation::DType::Scalar(shapewire::operation::Scalar::Int), shapewire::operation::Presence::Required, [])]
}

impl Shape<threads::shape::Input> for Boom {
    const NAME: &'static str = "boom";
    const CELLS: Cells = N_CELLS;
    const SHARED: Cells = Cells::NONE;
    const TRACKING: Tracking = Tracking::Lazy;
    type Producer = ThreadManager;
    type Root = BoomRoot;

    fn root(&self) -> BoomRoot {
        BoomRoot::default()
    }
    fn hydrates(&mut self, _: &ThreadManager, _: &mut BoomRoot, _: Timestamp) {}
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, BoomRoot>) {
        if let Input::Push(Push::Info { reason, .. }) = &input.value {
            if reason == "boom" {
                panic!("the fold panicked, as this test asked it to");
            }
        }
        tx.n += 1;
        tx.dirty = true;
    }
}

/// **A DEAD SHAPE TASK EMITS EXACTLY ONE FINAL EXPORT, AND THEN REFUSES.**
///
/// Operator ruling: hold the task's `JoinHandle`; when it dies, mark the shape
/// dead, refuse `shapes.subscribe` with an error, and emit ONE final export so
/// mirrors stop showing stale state as current. No timer, no periodic frame.
///
/// This is the failure the critique named as the reason rev 3 was not a plain
/// BUILD: without it the FIFO fills behind an orphaned task, every board holds
/// its last mirror forever, and a fresh subscribe answers `{queued: true}` and
/// then nothing.
///
/// **Forced red** by deleting the supervisor `tokio::spawn` in `Hub::register`:
/// `dead_frames.len()` is then 0 and `subscribe` still returns `Ok`.
#[tokio::test]
async fn a_dead_shape_task_tells_every_mirror_once_and_then_refuses() {
    let (m, _addr, tmp, _cfg) = start().await;
    let hub: Arc<Hub> = m.hub.clone();
    hub.register(Boom, m.clone());

    // Connection 7's OWN channel, opened before the subscribe is enqueued —
    // exactly the order `drive` uses, and for the same reason: an export emitted
    // into a channel nobody holds is an export nobody gets.
    let mut rx = hub.connect(7);
    hub.subscribe("boom", Some("k"), 7, None).unwrap();
    // The export for connection 7.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut pre = 0;
    while let Ok(f) = rx.try_recv() {
        if f.wire.op == Op::Read {
            pre += 1;
        }
    }
    assert_eq!(pre, 1, "the live shape must export exactly once on subscribe");

    // Kill it.
    hub.emit(Push::Info {
        domain: "x".into(),
        reason: "boom".into(),
        account: None,
        detail: None,
        at: "now".into(),
    });
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    // …and keep emitting, to prove the death frame is not repeated.
    for _ in 0..5 {
        hub.emit(Push::Info {
            domain: "x".into(),
            reason: "after".into(),
            account: None,
            detail: None,
            at: "now".into(),
        });
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let mut dead_frames = Vec::new();
    while let Ok(f) = rx.try_recv() {
        if f.wire.epoch.state == threads::shape::ShapeState::Dead {
            dead_frames.push(f);
        }
    }
    assert_eq!(
        dead_frames.len(),
        1,
        "EXACTLY ONE final export — no timer, no periodic frame"
    );
    let f = &dead_frames[0];
    assert_eq!(f.wire.op, Op::Read, "the death frame is an export, not an occurrence");
    assert!(f.to.is_none(), "it goes to every mirror, not to one connection");
    assert!(
        f.key.is_none(),
        "it is a fact about the SHAPE, so one frame reaches every held key"
    );
    assert!(
        f.wire.epoch.dead_reason.as_deref().unwrap_or("").contains("panic"),
        "the mirror is told WHY: {:?}",
        f.wire.epoch.dead_reason
    );

    // And the refusal.
    match hub.subscribe("boom", Some("k"), 8, None) {
        Err(ShapeError::Dead { shape, reason }) => {
            assert_eq!(shape, "boom");
            assert!(!reason.is_empty());
        }
        other => panic!("a dead shape must REFUSE a subscribe, not queue it: {other:?}"),
    }

    // The orphaned FIFO stops accepting, so it cannot fill behind a task that
    // is gone.
    assert!(!hub.tracked("boom"), "a dead shape must stop tracking");

    // A LIVE shape on the same hub is unaffected — the death is per shape.
    assert!(hub.is_dead("accounts").is_none());
    drop(tmp);
}

/// A future `at` is a timer; an absurd one is REFUSED rather than queued into a
/// `sleep_until` that would overflow and panic the task (which would then trip
/// the death path for what is really a bad request).
#[tokio::test]
async fn a_future_at_is_bounded_and_a_past_at_is_treated_as_absent() {
    let (m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;

    let far = (chrono::Utc::now() + chrono::Duration::days(3650)).to_rfc3339();
    let r = c
        .request(
            "1",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({"key":"first","at":far}),
        )
        .await;
    assert_eq!(r["outcome"]["type"], json!("error"), "{r}");
    assert!(
        r["outcome"]["error"].as_str().unwrap().contains("24h"),
        "the refusal names the bound: {r}"
    );

    // A PAST `at` is treated as absent (ruling 4) — it does not error, and the
    // export it produces reports the REAL fold point, never the requested `at`.
    let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let r = c
        .request(
            "2",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({"key":"first","at":past}),
        )
        .await;
    assert_eq!(r["outcome"]["payload"]["queued"], json!(true), "{r}");
    let mut export = None;
    for _ in 0..12 {
        let Some(f) = c.next().await else { break };
        if f["type"] == "shape" && f["op"] == "read" {
            export = Some(f);
            break;
        }
    }
    let e = export.expect("no export for a past `at`");
    let as_of: Timestamp = e["as_of"].as_str().unwrap().parse().unwrap();
    let requested: Timestamp = past.parse().unwrap();
    assert!(
        as_of > requested,
        "`as_of` is the real fold point, not the requested `at`: {e}"
    );
    let _ = m;
    drop(tmp);
}

/// Two consumers, N >= 2: both mirrors are the same, the subscriber count is
/// exact, and the lazy shape stops when the last one goes away.
#[tokio::test]
async fn two_consumers_hold_the_same_mirror_and_the_lazy_shape_stops_on_the_last_one() {
    let (m, addr, tmp, _cfg) = start().await;
    let mut a = Client::connect(addr).await;
    let mut b = Client::connect(addr).await;
    a.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;
    b.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;
    let pick = |fs: &[Value]| -> Value {
        fs.iter()
            .find(|f| f["type"] == "shape" && f["op"] == "read")
            .cloned()
            .expect("no export")
    };
    let fa = pick(&a.drain().await);
    let fb = pick(&b.drain().await);
    assert_eq!(fa["row"], fb["row"], "two mirrors of one key must be equal");

    let list = a.request("2", "shapes/list", json!({})).await;
    assert_eq!(
        list["outcome"]["payload"]["shapes"][0]["subscribers"],
        json!(2)
    );

    drop(a);
    drop(b);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert!(
        !m.hub.tracked("accounts"),
        "a Lazy shape stops tracking when its last subscriber's connection drops"
    );
    drop(tmp);
}

/// **A KEYLESS UNSUBSCRIBE LETS A LAZY SHAPE GO IDLE.**
///
/// # The defect
///
/// A keyless subscribe on a keyed shape means *"every key I know about"*, and
/// `do_subscribe` records it as `(conn, None)` PLUS `(conn, Some(k))` for every key
/// it exports — that pairing is how the delivery gate knows which keys the
/// connection holds. The keyless UNSUBSCRIBE removed exactly `(conn, None)`, one of
/// N+1 entries. So the set stayed non-empty, `stop_if_idle` saw a live subscriber,
/// and **a Lazy shape whose only keyless consumer cleanly unsubscribed never went
/// idle again**: it kept its root, kept accepting inputs, and kept folding for
/// nobody until the process ended. Dropping the SOCKET worked (that arm retains by
/// connection); only the clean door was broken, which is why no test caught it —
/// `tests/shapes.rs`'s existing unsubscribe is keyed.
///
/// It also made two `shapes.list` fields disagree for the wrong reason: the ledger
/// closed its `(conn, shape)` entry at once, so `consumers` was empty while
/// `subscribers` still said 1.
///
/// **Forced red** by restoring `self.subs.remove(&(conn, key))` in the
/// `Item::Unsubscribe` arm of `ShapeTask::handle`.
#[tokio::test]
async fn a_keyless_unsubscribe_releases_every_key_it_took() {
    let (m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;

    // KEYLESS — "every key you know about". The fixture has two accounts, so this
    // is the N+1 case and not a degenerate one: with a single key the buggy
    // removal and the correct one are indistinguishable.
    c.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({}))
        .await;
    let frames = c.drain().await;
    let exports = frames
        .iter()
        .filter(|f| f["type"] == "shape" && f["op"] == "read")
        .count();
    assert_eq!(exports, 2, "one export per key, so N is 2 here: {frames:#?}");
    assert!(m.hub.tracked("accounts"));

    c.request("2", &format!("shapes/{}/unsubscribe", "accounts"), json!({}))
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    assert!(
        !m.hub.tracked("accounts"),
        "a Lazy shape with one keyless consumer that cleanly unsubscribed must go \
         idle. It has not: the per-key entries the keyless subscribe created \
         outlived the unsubscribe that was supposed to remove them, so this fold \
         keeps hydrated state and keeps accepting inputs for nobody."
    );
    // And the two count fields agree about it, which is the other half of the
    // same bug.
    let list = m.hub.list();
    let accounts = list.iter().find(|s| s.name == "accounts").unwrap();
    assert_eq!(accounts.subscribers, 0, "no connection is subscribed");
    assert!(accounts.consumers.is_empty(), "and none is listed either");
    drop(tmp);
}

/// **THE MACHINE ALIAS IS REFUSED, NOT ACCEPTED-AND-STRANDED** — from the
/// consumer's chair, which is the only place the real cost was visible.
///
/// `wire::Resolving` names this exact hazard: *"a producer whose fold re-keys an
/// input … must name the key the fact will fold under, not the alias it was called
/// with. An id under a key that produces no frame is NOT delivered to some other key
/// as a consolation: it stays pending and the promise stays visibly unanswered."*
///
/// `accounts.disable {account: "default"}` did exactly that. The store accepted it
/// (`set_account_disabled` inserts a row for any string), the promise opened under
/// `"default"`, and `AccountsRoot::resolve` re-keys `default` onto whichever pool
/// account hosts the machine login — or onto NOTHING, which is this fixture, since
/// no pool login matches the machine store. So no frame was ever keyed `"default"`,
/// `take_key("default")` found nothing on every flush, and the id sat in the task's
/// `pending_promises` for the life of the process. The consumer got
/// `{promise_id}` and then silence, forever.
///
/// **Forced red** by deleting the `refuse_the_machine_alias` call from the
/// `accounts.disable` arm: the command is accepted and no frame ever carries its id.
#[tokio::test]
async fn the_machine_alias_is_refused_instead_of_stranding_a_promise() {
    let (_m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;
    // KEYLESS, so this connection holds every key there is — it could not miss a
    // frame for being subscribed to the wrong one.
    c.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({}))
        .await;
    c.drain().await;

    let reply = c
        .request("2", &format!("accounts/{}/disable", "default"), json!({}))
        .await;
    assert_eq!(
        reply["outcome"]["type"],
        json!("error"),
        "the alias is not an account slot, and a command about it is refused where \
         the caller can still fix it: {reply}"
    );
    let msg = reply["outcome"]["error"].as_str().unwrap_or_default();
    assert!(
        msg.contains("machine login"),
        "and the refusal names the door: {msg}"
    );

    // Nothing was accepted, so there is no id — and therefore no id that a frame
    // has to carry. This is the assertion that fails when the refusal is removed:
    // the reply carries `promise_id` and nothing ever answers it.
    assert!(
        reply["outcome"]["payload"]["promise_id"].is_null(),
        "a refusal mints no promise: {reply}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert!(
        !c.seen.iter().any(|f| f["type"] == "shape" && f["key"] == json!("default")),
        "and `default` never becomes a key of this shape: {:#?}",
        c.seen
    );
    drop(tmp);
}

/// **WHO IS SUBSCRIBED, OVER A REAL SOCKET — the passive consumer
/// understanding, end to end.**
///
/// The defect, 2026-09-22: two board processes were running against this
/// daemon, one three days old and spinning at 98% CPU on superseded code, and
/// nothing could have told the operator that. `shapes.list` said
/// `subscribers: 2` and stopped there.
///
/// > *"Maybe at subscribe time we have the subscriber input their identity and
/// > we log the subscribers to each shape, for passive consumer understanding.
/// > That way if we change a shape we can be aware of stale consumers."*
///
/// This is that, reproduced: two connections, two DIFFERENT builds claiming the
/// same `shape_version` — which is exactly how the incident looked — and
/// `shapes.list` telling them apart.
///
/// **`consumers` IS NOT A SECOND SPELLING OF `subscribers`**, but they DO agree
/// on their count, and the paragraph that used to sit here said otherwise — it
/// claimed `subscribers` counts `(conn, key)` pairs so that a keyless subscribe
/// is "one consumer and several subscribers". The task has always deduped on the
/// connection id. See `subscribers_counts_connections_and_not_conn_key_pairs`,
/// which is the test that discriminates: this one subscribes two connections to
/// one key each, where both readings give 2.
///
/// **THE MUTATION.** Delete the `info.consumers = ...` line in `Hub::list`, or
/// stop threading the `consumer` param into `subscribe_as` in
/// `handle_connection_method`, and this goes red.
#[tokio::test]
async fn shapes_list_says_who_is_subscribed_and_which_build_each_one_is() {
    let (_m, addr, tmp, _cfg) = start().await;
    let mut stale = Client::connect(addr).await;
    let mut fresh = Client::connect(addr).await;

    // The three-day-old board and today's board. SAME declared shape version —
    // they agree about the contract and differ by three days of code, which is
    // why `build` had to exist beside `shape_version`.
    stale
        .request(
            "1",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({
                                "key": "first",
                "consumer": {
                    "name": "threads-tui",
                    "shape_version": "2026-09-20",
                    "pid": 90001,
                    "build": "three-days-old"
                }
            }),
        )
        .await;
    fresh
        .request(
            "1",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({
                                "key": "first",
                "consumer": {
                    "name": "threads-tui",
                    "shape_version": "2026-09-20",
                    "pid": 90002,
                    "build": "todays-build"
                }
            }),
        )
        .await;
    stale.drain().await;
    fresh.drain().await;

    let list = fresh.request("2", "shapes/list", json!({})).await;
    let shape = &list["outcome"]["payload"]["shapes"][0];
    assert_eq!(shape["subscribers"], json!(2));
    let cs = shape["consumers"].as_array().expect("consumers on the shape");
    assert_eq!(cs.len(), 2, "one row per CONSUMER of this shape");
    let builds: Vec<&str> = cs.iter().map(|c| c["build"].as_str().unwrap()).collect();
    assert!(
        builds.contains(&"three-days-old") && builds.contains(&"todays-build"),
        "THE WHOLE POINT: two processes claiming one contract version are told \
         apart by their builds — {builds:?}"
    );
    for c in cs {
        assert_eq!(c["name"], "threads-tui");
        assert_eq!(c["shape_version"], "2026-09-20");
        assert!(c["pid"].is_u64(), "and the pid that makes it actionable");
        assert!(c["since"].is_string(), "and when it connected — his 'three days old'");
        // **A KEY SET, NOT A SCALAR** (2026-09-26). A connection can hold several
        // keys on one shape; the scalar `key` held the LAST one named and reported
        // a consumer as watching what it had most recently asked for rather than
        // what it was watching. `keyless` is the *"every key you know about"* ask,
        // its own member rather than a `null` in the list.
        assert_eq!(c["keys"], json!(["first"]));
        assert_eq!(c["keyless"], json!(false));
        assert!(
            c.get("key").is_none(),
            "the scalar is gone, not kept beside the list: {c}"
        );
    }

    // ── THE TRANSPORT IS WHAT CLOSES AN ENTRY. No reaper, no TTL: the stale
    //    board's connection ends and its row goes with it.
    drop(stale);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let list = fresh.request("3", "shapes/list", json!({})).await;
    let cs = list["outcome"]["payload"]["shapes"][0]["consumers"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(cs.len(), 1, "a connection that ended is not a subscriber");
    assert_eq!(cs[0]["build"], "todays-build");

    // ── AND THE CLEAN DOOR CLOSES IT TOO. `shapes.unsubscribe` is the other
    //    half of the wiring, and it is asserted here rather than only against
    //    the ledger's own API: the FIRST cut of these tests exercised
    //    `ConsumerLedger::disconnected` directly, so deleting the ledger call
    //    from `Hub::unsubscribe` altogether left every test green. A green that
    //    does not fail under its own mutation is worse than no test.
    fresh
        .request("4", &format!("shapes/{}/unsubscribe", "accounts"), json!({"key":"first"}))
        .await;
    let list = fresh.request("5", "shapes/list", json!({})).await;
    assert_eq!(
        list["outcome"]["payload"]["shapes"][0]["consumers"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "an unsubscribed consumer is not subscribed"
    );
    drop(tmp);
}

/// Everything already buffered for this connection, counted.
fn drain_frames(f: &mut threads::shape::ConnFrames) -> usize {
    let mut n = 0;
    while f.try_recv().is_ok() {
        n += 1;
    }
    n
}

/// **A FRAME DIRECTED AT ONE CONNECTION DOES NOT CONSUME ANOTHER'S BUFFER.**
///
/// The defect: ONE `broadcast::Sender<Arc<ShapeFrame>>` at capacity 1,024, shared
/// by every connection, with `ShapeFrame::to` examined in `drive` AFTER the send.
/// A `broadcast` counts every send against every receiver's buffer, so an export
/// addressed to connection 7 occupied a slot in connection 3's buffer, which
/// connection 3 had to drain or lag — and a lag's repair is a re-subscribe of
/// every key it holds, which is N more sends into everybody's buffer again. A
/// re-init did it unprompted, once per key, at the moment the producer was already
/// under enough load to have overflowed a FIFO.
///
/// **The contract, stated so it does not describe the implementation:** the number
/// of frames that enter B's buffer does not depend on how many frames are directed
/// at A. That sentence is true of a per-connection channel and false of a shared
/// broadcast, and it does not name either.
///
/// **N >= 2 on both axes**, because a thing that arbitrates between two of
/// something cannot be proved by one of anything: two connections, and a burst
/// strictly larger than one whole buffer, so that under the shared broadcast B
/// would have been pushed past `Lagged` rather than merely made to work.
#[tokio::test]
async fn a_directed_frame_does_not_consume_another_connections_buffer() {
    let (m, _addr, tmp, _cfg) = start().await;
    let hub: Arc<Hub> = m.hub.clone();

    // Ids well clear of anything `drive` mints, since this test speaks to the hub
    // rather than over a socket.
    let (a_conn, b_conn) = (9001u64, 9002u64);
    let mut a = hub.connect(a_conn);
    let mut b = hub.connect(b_conn);

    // Both watch the whole shape, so both are entitled to every undirected frame
    // it emits — which is what makes B's silence below a statement about
    // ADDRESSING and not about subscription.
    hub.subscribe("accounts", None, a_conn, None).unwrap();
    hub.subscribe("accounts", None, b_conn, None).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let b_baseline = drain_frames(&mut b);
    let _ = drain_frames(&mut a);
    assert!(
        b_baseline >= 2,
        "B must have had its OWN exports, or this test proves nothing about a \
         channel that works: {b_baseline}"
    );
    assert_eq!(b.take_lost(), 0, "and nothing was dropped getting them");

    // ── THE BURST, ADDRESSED ENTIRELY TO A ───────────────────────────────────
    //
    // Strictly more than one whole buffer, so a shared channel could not have
    // absorbed it on B's behalf.
    let burst = threads::shape::CONN_FRAME_CAPACITY * 2;
    for _ in 0..burst {
        hub.subscribe("accounts", Some("first"), a_conn, None)
            .unwrap();
    }
    // The task processes control items serially; wait until it has, or give up
    // loudly rather than measure a burst that never happened.
    // A's own drops are A's business — a connection that will not drain its own
    // buffer is the one that pays, which is rule 2.3 from the producer's side. So
    // both halves are counted: what A read and what A was too slow to take.
    let mut a_seen = 0usize;
    let mut a_lost = 0u64;
    for _ in 0..160 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        a_seen += drain_frames(&mut a);
        a_lost += a.take_lost();
        if a_seen + a_lost as usize >= burst {
            break;
        }
    }

    let a_total = a_seen + a_lost as usize;
    let b_after = drain_frames(&mut b);
    let b_lost = b.take_lost();

    assert!(
        a_total > threads::shape::CONN_FRAME_CAPACITY,
        "the burst has to have been bigger than ONE buffer or the claim is \
         untested — A saw {a_total} of {burst}"
    );
    assert_eq!(
        (b_after, b_lost),
        (0, 0),
        "B was sent {a_total}+ frames' worth of nothing: not one of A's {burst} \
         directed exports may enter B's buffer, and not one of them may cost B a \
         dropped frame. On the shared broadcast every single one did, and B would \
         have lagged past its whole buffer and then re-subscribed every key it \
         holds — which is the amplification this exists to stop."
    );

    // And B's own channel is still WORKING, not merely quiet: a frame owed to the
    // whole shape still arrives.
    shake(&m);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert!(
        drain_frames(&mut b) > 0,
        "a quiet channel and a dead one are not the same thing"
    );
    drop(tmp);
}

/// **`subscribers` COUNTS CONNECTIONS, NOT `(conn, key)` PAIRS.**
///
/// The field's own doc said the opposite for as long as both fields have existed,
/// and built an argument on it: that `consumers` had to be a separate list
/// BECAUSE the two answered different questions, `subscribers` being a pair count
/// and `consumers` a per-connection one. The task's `publish` collects `subs`
/// into a `HashSet` on the connection id, so it never was. The code is what is
/// right here and the doc is what changed — `subscribers` is on the `shapes.list`
/// wire and the board reads it, and `docs/shapes.md` 0's own probe recorded it as
/// `subscribers 1 … keys 4` on the operator's single board, so a pair count would
/// have moved a live number under a live consumer to match a comment.
///
/// **This is the test that DISCRIMINATES**, and the one the suite did not have:
/// every existing assertion used one key per connection, where both readings give
/// the same integer. One connection, a KEYLESS subscribe, and a shape with two or
/// more keys is the only arrangement where they differ — and it is proved at
/// N >= 2 on both axes, keys and connections, because a field that arbitrates
/// between two countings cannot be proved by one of anything.
///
/// **THE MUTATION.** Replace the task's `publish` body with
/// `self.info.subscribers = self.subs.len()`, which is the pair count the doc
/// described. The first assertion goes red with 2 against 1.
#[tokio::test]
async fn subscribers_counts_connections_and_not_conn_key_pairs() {
    let (_m, addr, tmp, _cfg) = start().await;
    let mut a = Client::connect(addr).await;

    // ONE connection, KEYLESS — which the task expands to one `(conn, key)` pair
    // per key it holds, so the two countings now disagree by construction.
    a.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({}))
        .await;
    a.drain().await;

    let list = a.request("2", "shapes/list", json!({})).await;
    let shape = &list["outcome"]["payload"]["shapes"][0];
    let keys = shape["keys"].as_array().expect("keys").len();
    assert!(
        keys >= 2,
        "this test is vacuous at one key — the fixture must hold at least two: {shape}"
    );
    assert_eq!(
        shape["subscribers"],
        json!(1),
        "ONE board is one subscriber, whatever its keyless subscribe expanded to. \
         A pair count would say {keys} here, and the operator's board would read \
         {keys} watchers where there is one: {shape}"
    );
    assert_eq!(
        shape["consumers"].as_array().map(|c| c.len()),
        Some(1),
        "and the identity list agrees, which is the whole of what the two fields \
         now share: {shape}"
    );

    // ── AND IT IS NOT STUCK AT ONE. A SECOND connection, also keyless. ───────
    let mut b = Client::connect(addr).await;
    b.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({}))
        .await;
    b.drain().await;

    let list = a.request("3", "shapes/list", json!({})).await;
    let shape = &list["outcome"]["payload"]["shapes"][0];
    assert_eq!(
        shape["subscribers"],
        json!(2),
        "two boards are two subscribers — the dedupe is on the CONNECTION, not a \
         clamp to one: {shape}"
    );
    assert_eq!(
        shape["consumers"].as_array().map(|c| c.len()),
        Some(2),
        "{shape}"
    );

    // ── AND ONE LEAVING TAKES EXACTLY ONE OFF. ──────────────────────────────
    drop(b);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let list = a.request("4", "shapes/list", json!({})).await;
    let shape = &list["outcome"]["payload"]["shapes"][0];
    assert_eq!(shape["subscribers"], json!(1), "{shape}");
    drop(tmp);
}

/// **`unplaced_status` IS A MEASUREMENT AGAIN.**
///
/// It has been on the `shapes.list` wire since the field was written and was
/// **never incremented anywhere in the crate** — declared at `shape.rs:1225`,
/// initialised to `0`, and referenced nowhere else. The `0` a client read was
/// the initialiser, not a count, and the board reads it
/// (`threads_tui/render.py:168`): a lie with a schema and a consumer.
///
/// It was never wired because of where it lives: `Shape::folds` gets a `Tx`
/// over the root and nothing else, so the fold that SEES an unattributed status
/// cannot reach the counter. The task can see both, which is where it now
/// happens.
///
/// **THE MUTATION.** Delete the `unplaced_status += 1` in the task's
/// `Item::Input` arm and the count goes back to being structurally zero.
#[tokio::test]
async fn an_unattributed_status_is_counted_rather_than_read_as_zero() {
    let (m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;
    c.request("1", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;
    c.drain().await;

    let status = |account: Option<&str>| Push::Status {
        thread_id: "t1".into(),
        state: "active".into(),
        executing: true,
        queued: 0,
        unread: 0,
        detail: None,
        account: account.map(str::to_string),
    };
    // Two the daemon could not name, and one it could.
    m.hub.emit(status(None));
    m.hub.emit(status(Some("first")));
    m.hub.emit(status(None));
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    c.drain().await;

    let list = c.request("2", "shapes/list", json!({})).await;
    assert_eq!(
        list["outcome"]["payload"]["shapes"][0]["unplaced_status"],
        json!(2),
        "COUNTED, NEVER GUESSED AT — and the one with an account is not counted"
    );
    drop(tmp);
}

/// **THE ARMED-TIMER CAP REFUSES ON THE REQUEST ID, NOT INTO A LOG.**
///
/// **The defect, 2026-09-22.** `MAX_ARMED_TIMERS_PER_CONN` was checked inside
/// the shape task, on an item the RPC had ALREADY answered `{"queued": true}`.
/// Over the limit, the task logged a `warn!` and returned — no export, no error
/// frame, no death notice — and the consumer waited forever on a subscription
/// it had been told it had. `ShapeError::TooManyTimers` was declared and
/// formatted and **never constructed anywhere in the crate**: the error for this
/// exact condition existed and went unused.
///
/// That is the failure class the whole design is built against, arriving
/// through a new door. An ack that means nothing is worse than an error,
/// because the consumer stops looking.
///
/// It matters more now than it did: re-subscribe is the ONLY recovery mechanism
/// in the chassis contract — there is no re-emit and no replay — so a consumer
/// clocking itself with `at` and silently losing one cycle has no way back.
///
/// The test walks the cap to its edge and one past it, then proves the
/// RESERVATION is released when a timer FIRES — otherwise a consumer doing
/// exactly what `docs/shapes.md` §2.5 tells it to ("issue the next subscribe
/// when the last export arrives") would run out of allowance after four
/// perfectly successful cycles.
///
/// **THE MUTATION.** Delete the `at.is_some()` reservation block from
/// `Hub::subscribe` and the refusal goes back to being a log line: the fifth
/// subscribe answers `{queued: true}` and the assertion goes red.
#[tokio::test]
async fn a_future_at_subscribe_past_the_cap_is_refused_on_the_request_id() {
    let (_m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;
    let ahead = |secs: i64| {
        (chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339()
    };

    // Four is the cap, and four must all be accepted.
    for i in 0..4 {
        let r = c
            .request(
                &format!("a{i}"),
                &format!("shapes/{}/subscribe", "accounts"),
                json!({"key":"first","at": ahead(3600 + i)}),
            )
            .await;
        assert_eq!(
            r["outcome"]["type"], json!("success"),
            "the cap is four, so four must be accepted; {i} was not"
        );
    }

    // The fifth is REFUSED, and the refusal reaches the caller.
    let r = c
        .request(
            "a5",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({"key":"first","at": ahead(3700)}),
        )
        .await;
    assert_eq!(
        r["outcome"]["type"], json!("error"),
        "A CONTROL THAT REFUSES QUIETLY IS A CONTROL NOBODY CAN ACT ON. This \
         used to answer `{{queued: true}}` and then drop the item in a log line"
    );
    let err = r["outcome"]["error"].as_str().unwrap();
    assert!(
        err.contains("pending future-`at` subscriptions"),
        "and it is `ShapeError::TooManyTimers`, the variant that was written for \
         this and never constructed: {err}"
    );
    assert!(err.contains("accounts"), "naming the shape: {err}");
    // ── AND IT NAMES THE CEILING — rule 2.8's refusal clause ────────────────
    //
    // **It did not, until 2026-10-02.** The variant carried the shape alone, so
    // a consumer was told it had asked for too many and not how many it may
    // have: the refusal that says you were wrong and not what to ask instead,
    // which `KeyRequired` and `AboveMax` were both changed to stop doing. The
    // number is read off the slot's published `max.armed_timers_per_conn`, which
    // `chassis_api.rs` asserts is 4 on `shapes.list`, so the refusal and the
    // contract cannot disagree.
    assert!(
        err.contains("at most 4"),
        "rule 2.8: a refusal NAMES the ceiling, and `shapes.list` publishes the \
         same 4 as `max.armed_timers_per_conn`: {err}"
    );
    assert!(
        err.contains("not clamped"),
        "and says it is not clamped — a clamped answer is a second claim about a \
         number nobody can see: {err}"
    );

    // ── AND A SLOT COMES BACK WHEN A TIMER FIRES ───────────────────────────
    //
    // A consumer clocking itself is the whole reason `at` exists. If a fired
    // timer kept its reservation, that consumer would be refused on its fifth
    // cycle having done nothing wrong.
    let mut d = Client::connect(addr).await;
    for i in 0..4 {
        let r = d
            .request(
                &format!("b{i}"),
                &format!("shapes/{}/subscribe", "accounts"),
                json!({"key":"first","at": ahead(1)}),
            )
            .await;
        assert_eq!(r["outcome"]["type"], json!("success"));
    }
    // Let all four fire.
    tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
    d.drain().await;
    let r = d
        .request(
            "b5",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({"key":"first","at": ahead(3600)}),
        )
        .await;
    assert_eq!(
        r["outcome"]["type"], json!("success"),
        "a FIRED timer holds no slot: the reservation covers the waiting, not \
         the lifetime of the connection"
    );
    drop(tmp);
}

/// **AN ABSENT IDENTITY IS REFUSED AT THE WIRE, AND SO IS A HALF-FILLED ONE.**
///
/// This test REPLACES `a_blank_identity_is_refused_and_an_absent_one_subscribes_unidentified`,
/// whose second half asserted that anonymity was served and whose name promised
/// it. The operator ruled on 2026-10-08 that identity is compulsory — *"what I
/// want is consumers to be forced to say who they are so we can track
/// consumers"* — which inverted that half, so the name had to go with it.
///
/// It is the END-TO-END twin of `shapewire::ledger`'s unit test: that one proves
/// `parse` refuses, this one proves the REFUSAL REACHES THE WIRE through the
/// door `threads` serves — the legacy door until R2.S4, the chassis door (the
/// only one) since. A rule proven only at the parser is a rule that can be
/// bypassed by a door that forgets to call it.
///
/// Three claims:
///
/// 1. a half-stated identity is refused, naming the field;
/// 2. an ABSENT identity is refused, in the shared sentence;
/// 3. neither opened a subscription — `shapes.list` reports no consumer — and a
///    COMPLETE identity still does, which is the positive sentinel that stops
///    this passing against a door that refuses every subscribe there is.
///
/// **THE MUTATION.** Restore `Ok(None)` for the absent case in
/// `ConsumerIdentity::parse` and claim 2 reddens; stop calling `parse` in the
/// chassis's `shapes/{shape}/subscribe` arm (`handle_connection_method` on the
/// retired legacy door) and claims 1 and 2 both do; drop its emptiness filter
/// and claim 1 does.
#[tokio::test]
async fn an_absent_identity_is_refused_at_the_wire_and_so_is_a_blank_one() {
    let (_m, addr, tmp, _cfg) = start().await;
    let mut c = Client::connect(addr).await;

    let r = c
        .request(
            "1",
            &format!("shapes/{}/subscribe", "accounts"),
            json!({"key":"first","consumer":{"name":"","shape_version":"1"}}),
        )
        .await;
    assert_eq!(r["outcome"]["type"], json!("error"), "blank is not an answer");
    let err = r["outcome"]["error"].as_str().unwrap();
    assert!(err.contains("consumer.name"), "and it says which field: {err}");

    // AND THE SUBSCRIPTION WAS NOT OPENED. A ledger that recorded a refused
    // subscribe would be asserting something untrue about the present.
    let list = c.request("2", "shapes/list", json!({})).await;
    assert_eq!(
        list["outcome"]["payload"]["shapes"][0]["consumers"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // ── ABSENT IS REFUSED TOO, AND IN THE ONE SHARED SENTENCE.
    //
    // This block used to assert the opposite — that the subscribe was served and
    // LISTED "with a hole where the name goes, so the gap is legible". A hole in
    // the ledger is exactly what the ruling removed: the point of tracking
    // consumers is that there is nobody the producer cannot name.
    // `request_verbatim`, NOT `request`: the convenience helper would add the
    // very identity this block is proving the absence of. See its doc comment —
    // this test went green through `request` before that was caught.
    let r = c
        .request_verbatim("3", &format!("shapes/{}/subscribe", "accounts"), json!({"key":"first"}))
        .await;
    assert_eq!(
        r["outcome"]["type"],
        json!("error"),
        "rule 2.1: a subscribe that names no consumer must be refused, not \
         served as a hole in the ledger: {r}"
    );
    let err = r["outcome"]["error"].as_str().unwrap();
    assert_eq!(
        err,
        shapewire::ledger::IDENTITY_REQUIRED_REFUSAL,
        "and this door must say what the chassis door says, to the byte — one \
         rule cannot tell two consumers two different things: {err}"
    );

    // AND IT OPENED NOTHING. Same reasoning as the blank case above: a ledger
    // that listed a refused subscribe would assert something untrue about the
    // present.
    c.drain().await;
    let list = c.request("4", "shapes/list", json!({})).await;
    assert_eq!(
        list["outcome"]["payload"]["shapes"][0]["consumers"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "a refused subscribe is not a subscriber"
    );

    // ── THE POSITIVE SENTINEL: A COMPLETE IDENTITY STILL SUBSCRIBES, AND IS
    //    LISTED BY NAME. Without this the two refusals above would pass against
    //    a door that had simply stopped accepting subscribes at all.
    c.request(
        "5",
        &format!("shapes/{}/subscribe", "accounts"),
        json!({"key":"first","consumer":{"name":"threads-tui","shape_version":"shapes.md@rev4"}}),
    )
    .await;
    c.drain().await;
    let list = c.request("6", "shapes/list", json!({})).await;
    let cs = list["outcome"]["payload"]["shapes"][0]["consumers"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(cs.len(), 1, "a well-formed subscribe still subscribes");
    assert_eq!(
        cs[0]["name"],
        json!("threads-tui"),
        "and the ledger now has a NAME where the hole used to be, which is the \
         whole point of the ruling: {:?}",
        cs[0]
    );
    assert_eq!(cs[0]["shape_version"], json!("shapes.md@rev4"));
    drop(tmp);
}

/// `Hydration` provenance is a labelled read, and `Empty` is a STATEMENT, not
/// an absence — the distinction the export's three-valued cell state carries to
/// the screen.
#[test]
fn empty_and_partial_are_different_verdicts() {
    let e: Hydration<u8> = Hydration::Empty { reason: "none" };
    let p: Hydration<u8> = Hydration::Partial {
        got: vec![1],
        reason: "one failed".into(),
    };
    let c: Hydration<u8> = Hydration::Complete(vec![1, 2]);
    assert!(e.is_complete(), "empty is a complete answer: there are none");
    assert!(!p.is_complete());
    assert!(c.is_complete());
    assert_eq!(e.rows().len(), 0);
    assert_eq!(p.rows().len(), 1);
    assert!(c.reason().is_none());
}
