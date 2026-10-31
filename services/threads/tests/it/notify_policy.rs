//! # R10.M3 — links and policy: a `notify_policy` row per (owner, peer, network)
//!
//! `docs/MESSAGING-ORCHESTRATION.md` §2.2, §4, §8, §10.2 (the R10.M3 row). The
//! operator approved R10.M3 "with `direct` as the default". Each guard is
//! forced red in an internal report (substrate `interface-layer`),
//! with its opposing arm:
//!
//! * `the_default_is_direct_and_a_hold_link_rides_*` — N = 2 owners (p, q),
//!   N = 3 peers: a link no act set wakes its owner (c2 → p, d1 → q); p's
//!   `notify: hold` on c1 makes c1's answer RIDE (a `ride` row, a
//!   `dispatch_changed{notify_hold}`, no turn) while c2 on the same owner and
//!   d1 on the other owner still wake. Forced: `policy::DEFAULT = Hold` (the
//!   default arms go red) and `policy::get` ignoring rows (the hold arm goes
//!   red).
//! * `agents_policy_changes_a_link_*` — no message sent: `hold` makes the
//!   peer's own mail `held` (told to the sender, with the reason), `direct`
//!   makes the next one wake.
//! * `a_notify_on_a_peer_you_do_not_reach_is_no_route_*` — `agents_message`
//!   and `agents_policy` on a stranger, an unknown id, and over a network the
//!   peer is not in: `no_route`, nothing written. Opposing: on its own child,
//!   accepted and written.
//! * `wake_is_refused_unknown_param_naming_notify_off_spawn` — message and
//!   policy (R10.M3b put `wake` back on spawn, deprecated — below).
//!
//! R10.M3b (Q-23-B), forced red in an internal report;
//! DEPRECATED(wake, remove after DEPLOY-9n):
//!
//! * `wake_never_keeps_its_old_meaning_*` — N = 2 `never` children: neither
//!   the spawn report nor a later run's end wakes the parent, and NO policy
//!   row is written; each child's own `agents_message` to the parent is
//!   `delivered` and wakes it. Opposing: N = 2 `wake: end` children each wake
//!   the parent by their spawn report.
//! * `wake_beside_notify_is_refused_*` — both doors, both pairings; nothing
//!   spawned, nothing counted. Opposing: each word alone is accepted.
//! * `a_wake_spawn_carries_the_warning_and_is_counted_*` — every spawn with
//!   `wake` (N = 3, both values, both doors) answers the warning and appends a
//!   `deprecated_param` event on the caller's ledger; opposing: a spawn
//!   without `wake` (bare, or `notify`) carries no `warning` and adds no event.
//! * `the_policy_table_migration_*` — a store from before R10.M3 gains the
//!   table on open; every later open is a no-op that keeps the rows.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};
use shapewire::token::Claims;
use threads::config::Config;
use threads::event::ThreadEvent;
use threads::{build_manager, ThreadManager};

fn env() -> &'static Path {
    static ENV: OnceLock<tempfile::TempDir> = OnceLock::new();
    ENV.get_or_init(|| {
        let tmp = tempfile::tempdir().unwrap();
        let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"));
        std::env::set_var("CAULDRON_HOME", tmp.path().join("cauldron"));
        std::env::set_var("THREADS_CLAUDE_BIN", fake);
        std::env::set_var("THREADS_TAIL_POLL_MS", "25");
        // The longest period in the dispatch path: a WAITING eager row is
        // dispatched by this tick at the latest. Every "woken" below happens
        // well inside it, so it is the post's own wake and not the tick; a
        // `ride` row is never the cause of a turn, tick or no tick, and that
        // is asserted structurally (the row's `dispatch`), not by waiting.
        std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
        std::env::set_var("FAKE_STATE_DIR", tmp.path().join("fake-state"));
        std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
        std::fs::create_dir_all(tmp.path().join("fake-state")).unwrap();
        tmp
    })
    .path()
}

fn config_at(dir: &Path) -> Config {
    let mut map = BTreeMap::new();
    std::fs::create_dir_all(dir.join("aa-owned")).unwrap();
    map.insert("aa-owned".to_string(), dir.join("aa-owned").to_string_lossy().to_string());
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts: map,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        probe_interval_busy_secs: 0,
        ..Default::default()
    }
}

/// Bound on "woken": 10 s, against a 60 s resume tick.
const WOKEN_WITHIN: u64 = 10;

async fn open(dir: &Path) -> ThreadManager {
    env();
    threads::clock::testing::ticking();
    let m = build_manager(config_at(dir)).unwrap();
    m.recover().await;
    m
}

fn root(m: &ThreadManager, cwd: &Path) -> String {
    let settings = json!({"cwd": cwd.to_string_lossy(), "model": "sonnet", "account": {"named": "aa-owned"}});
    m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
}

fn claims(id: &str) -> Claims {
    Claims { thread_id: id.to_string(), run_id: format!("{id}/run-1"), root: id.to_string(), depth: 0, turn: "test".to_string() }
}

/// `(code, message)` on refusal.
async fn call(m: &ThreadManager, caller: &str, name: &str, args: Value) -> Result<Value, (String, String)> {
    threads::agents::call(m, &claims(caller), name, args)
        .await
        .map_err(|r| (r.code.to_string(), r.message))
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out ({secs}s) waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn idle(m: &ThreadManager, id: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !m.is_idle(id).unwrap() {
        assert!(tokio::time::Instant::now() < deadline, "{id} never went idle");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn turns(m: &ThreadManager, id: &str) -> usize {
    m.store
        .events_from(id, 0, 100_000)
        .unwrap()
        .iter()
        .filter(|e| matches!(e.event, ThreadEvent::TurnStarted { .. }))
        .count()
}

fn customs_on(m: &ThreadManager, id: &str, kind: &str) -> Vec<Value> {
    m.store
        .events_from(id, 0, 100_000)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            ThreadEvent::Custom { event_type, data } if event_type == kind => Some(data),
            _ => None,
        })
        .collect()
}

/// `(message_id, metadata)` of every `user_message` on `id`.
fn user_msgs(m: &ThreadManager, id: &str) -> Vec<(Option<u64>, Value)> {
    m.store
        .events_from(id, 0, 100_000)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            ThreadEvent::UserMessage { metadata, .. } => Some((e.message_id, metadata)),
            _ => None,
        })
        .collect()
}

/// The message id of the run-end reply from `c` on `p`, once posted.
fn reply_from(m: &ThreadManager, p: &str, c: &str) -> Option<u64> {
    user_msgs(m, p)
        .into_iter()
        .find(|(_, md)| md["reply_id"].as_str().is_some_and(|r| r.starts_with(&format!("reply:{c}:"))))
        .and_then(|(mid, _)| mid)
}

async fn spawn(m: &ThreadManager, parent: &str, extra: Value) -> String {
    let mut args = json!({"task": "child task: say done"});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    let ack = call(m, parent, "agents_spawn", args).await.unwrap_or_else(|e| panic!("agents_spawn refused: {e:?}"));
    let child = ack["thread_id"].as_str().unwrap().to_string();
    wait_for(&format!("{child}'s task turn"), 30, || {
        m.store.tail_events(&child, 50).unwrap().iter().any(|e| matches!(e.event, ThreadEvent::TurnEnded { .. }))
    })
    .await;
    idle(m, &child).await;
    child
}

/// p re-tasks c by mail (with `extra` merged in) and waits for c's run to end.
async fn retask(m: &ThreadManager, p: &str, c: &str, extra: Value) {
    let c0 = turns(m, c);
    let mut args = json!({"agent": c, "body": "second task: say again"});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    call(m, p, "agents_message", args).await.unwrap_or_else(|e| panic!("agents_message refused: {e:?}"));
    wait_for(&format!("{c}'s mail-started run"), 20, || turns(m, c) > c0).await;
    idle(m, c).await;
}

/// **THE DEFAULT IS `direct`, AND AN EXPLICIT `hold` ROW RIDES — N = 2 OWNERS,
/// 3 PEERS.** p spawns c1, c2; q spawns d1 (all default). After the spawn
/// reports settle:
///
/// * p re-tasks c1 with `notify: hold`: the row is written (owner p, peer c1,
///   `home:p`), c1's run-end reply is POSTED on p as a `ride` queue row with a
///   `dispatch_changed{reason: notify_hold}` naming it, and p does NOT start a
///   turn.
/// * p re-tasks c2 with no `notify` (no row): the reply wakes p within
///   [`WOKEN_WITHIN`] s — and its turn carries the riding c1 reply.
/// * q re-tasks d1 with no `notify`: q is woken. p's `hold` on c1 is p's alone.
///
/// Forced red in the report: `policy::DEFAULT = Hold` (c2/d1 never wake their
/// owners), and `policy::get` returning `DEFAULT` for every link (c1's reply
/// wakes p).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_default_is_direct_and_a_hold_link_rides_n2_owners() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let q = root(&m, dir.path());
    let c1 = spawn(&m, &p, json!({})).await;
    let c2 = spawn(&m, &p, json!({})).await;
    let d1 = spawn(&m, &q, json!({})).await;
    wait_for("p and q woken by the spawn reports", 20, || turns(&m, &p) >= 1 && turns(&m, &q) >= 1).await;
    idle(&m, &p).await;
    idle(&m, &q).await;
    // Sentinel: nothing set yet — the default is a property of NO ROW.
    assert!(m.store.notify_policies_of(&p).unwrap().is_empty());
    assert!(m.store.notify_policies_of(&q).unwrap().is_empty());

    // c1: hold.
    let p0 = turns(&m, &p);
    retask(&m, &p, &c1, json!({"notify": "hold"})).await;
    let home = format!("home:{p}");
    assert_eq!(m.store.notify_policy(&p, &c1, &home).unwrap().as_deref(), Some("hold"));
    let ev = customs_on(&m, &p, "notify_policy");
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0]["peer"], json!(c1));
    assert_eq!(ev[0]["notify"], json!("hold"));
    wait_for("c1's reply is posted on p", WOKEN_WITHIN, || reply_from(&m, &p, &c1).is_some()).await;
    let mid = reply_from(&m, &p, &c1).unwrap();
    let row = m.store.waiting_queue(&p).unwrap().into_iter().find(|r| r.message_id == mid);
    assert_eq!(row.map(|r| r.dispatch).as_deref(), Some("ride"), "c1's reply must ride, never be the cause of a turn");
    let changed = customs_on(&m, &p, "cauldron.threads/dispatch_changed");
    assert!(
        changed.iter().any(|d| d["reason"] == "notify_hold" && d["message_ids"].as_array().is_some_and(|a| a.contains(&json!(mid)))),
        "{changed:?}"
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(turns(&m, &p), p0, "a hold link's reply woke p");

    // c2: no row ⇒ direct ⇒ wakes, carrying the rider.
    retask(&m, &p, &c2, json!({})).await;
    assert_eq!(m.store.notify_policy(&p, &c2, &home).unwrap(), None);
    wait_for("p woken by c2's reply (default direct)", WOKEN_WITHIN, || turns(&m, &p) > p0).await;
    idle(&m, &p).await;
    wait_for("the rider went out with p's turn", WOKEN_WITHIN, || {
        !m.store.waiting_queue(&p).unwrap().iter().any(|r| r.message_id == mid)
    })
    .await;

    // d1, on the OTHER owner: no row ⇒ wakes q.
    let q0 = turns(&m, &q);
    retask(&m, &q, &d1, json!({})).await;
    wait_for("q woken by d1's reply (default direct)", WOKEN_WITHIN, || turns(&m, &q) > q0).await;
    idle(&m, &q).await;
    assert!(m.store.notify_policies_of(&q).unwrap().is_empty(), "p's hold leaked onto q");
}

/// **`agents_policy` CHANGES A LINK WITHOUT SENDING, AND GOVERNS THE PEER'S
/// OWN MAIL.** p spawns c (default) and mails it over home (c gains its route
/// back). p sets `hold` with `agents_policy` — no message: c's next mail to p
/// is `held`, told to c with the policy as the reason, and p is not woken. p
/// sets `direct`: c's next mail is `delivered` and wakes p.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_policy_changes_a_link_and_governs_the_peers_own_mail() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let c = spawn(&m, &p, json!({})).await;
    wait_for("p woken by the spawn report", 20, || turns(&m, &p) >= 1).await;
    idle(&m, &p).await;
    let pr = turns(&m, &p);
    retask(&m, &p, &c, json!({"network": format!("home:{p}")})).await;
    // c's run-end reply (default direct) wakes p: let that turn happen and
    // end, so nothing already in flight is counted against the hold below.
    wait_for("c's reply is posted on p", WOKEN_WITHIN, || reply_from(&m, &p, &c).is_some()).await;
    wait_for("p woken by c's reply", WOKEN_WITHIN, || turns(&m, &p) > pr).await;
    idle(&m, &p).await;

    let c_in = m.store.events_from(&c, 0, 100_000).unwrap().len();
    let ack = call(&m, &p, "agents_policy", json!({"agent": c, "notify": "hold"})).await.unwrap();
    assert!(ack["promise_id"].is_u64(), "{ack}");
    assert_eq!(m.store.events_from(&c, 0, 100_000).unwrap().len(), c_in, "agents_policy sent c something");
    assert_eq!(m.store.notify_policy(&p, &c, &format!("home:{p}")).unwrap().as_deref(), Some("hold"));
    let p0 = turns(&m, &p);
    let ack = call(&m, &c, "agents_message", json!({"agent": p, "body": "receipt one"})).await.unwrap();
    assert_eq!(ack["delivery"], json!("held"), "{ack}");
    let fact = customs_on(&m, &c, "mail_queued").last().unwrap().clone();
    assert!(fact["reason"].as_str().is_some_and(|r| r.contains("`hold`")), "{fact}");
    assert_eq!(m.store.count_held_mail(&p).unwrap(), 1);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(turns(&m, &p), p0, "held mail woke p");

    call(&m, &p, "agents_policy", json!({"agent": c, "notify": "direct"})).await.unwrap();
    let ack = call(&m, &c, "agents_message", json!({"agent": p, "body": "receipt two"})).await.unwrap();
    assert_eq!(ack["delivery"], json!("delivered"), "{ack}");
    wait_for("p woken once the link is direct again", WOKEN_WITHIN, || turns(&m, &p) > p0).await;
    idle(&m, &p).await;
}

/// **A `notify` ON A PEER YOU DO NOT REACH IS `no_route`, AND NOTHING IS
/// WRITTEN** (§8). p and a stranger s (another root, no shared network);
/// `agents_message` with `notify` and `agents_policy` are refused `no_route`
/// for s, for an id that does not exist (the same answer, N2), and for p's
/// own child over a network the child is not in. Opposing arm: p's
/// `agents_policy` on its own child is accepted and written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_notify_on_a_peer_you_do_not_reach_is_no_route_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let s = root(&m, dir.path());
    let c = spawn(&m, &p, json!({"notify": "hold"})).await;
    let rows0 = m.store.notify_policies_of(&p).unwrap();
    assert_eq!(rows0.len(), 1, "sentinel: the spawn's own link row: {rows0:?}");
    let ev0 = customs_on(&m, &p, "notify_policy").len();
    for who in [s.as_str(), "no-such-thread"] {
        let e = call(&m, &p, "agents_message", json!({"agent": who, "body": "hi", "notify": "hold"})).await.unwrap_err();
        assert_eq!(e.0, "no_route", "{e:?}");
        let e = call(&m, &p, "agents_policy", json!({"agent": who, "notify": "hold"})).await.unwrap_err();
        assert_eq!(e.0, "no_route", "{e:?}");
    }
    let e = call(&m, &p, "agents_policy", json!({"agent": c, "notify": "direct", "network": format!("home:{s}")})).await.unwrap_err();
    assert_eq!(e.0, "no_route", "{e:?}");
    assert_eq!(m.store.notify_policies_of(&p).unwrap(), rows0, "a refused notify wrote a row");
    assert_eq!(customs_on(&m, &p, "notify_policy").len(), ev0, "a refused notify wrote an event");
    // Opposing arm.
    call(&m, &p, "agents_policy", json!({"agent": c, "notify": "direct"})).await.unwrap();
    assert_eq!(m.store.notify_policy(&p, &c, &format!("home:{p}")).unwrap().as_deref(), Some("direct"));
    assert_eq!(customs_on(&m, &p, "notify_policy").len(), ev0 + 1);
}

/// **`wake` IS OFF EVERY SURFACE BUT SPAWN'S: REFUSED `unknown_param` NAMING
/// `notify` ON `agents_message` AND `agents_policy`** (which never took it).
/// R10.M3b put `wake` back on `agents_spawn` for one release, deprecated —
/// its guards are the R10.M3b section of this file. Opposing arms: `notify: hold` is
/// accepted, a bad word is `malformed`, and a gather is `not_available_yet`
/// naming R10.M5 — and none of the refused calls spawned anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wake_is_refused_unknown_param_naming_notify_off_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let before = m.store.list_threads(100_000).unwrap().len();
    let e = call(&m, &p, "agents_message", json!({"agent": p, "body": "b", "wake": "end"})).await.unwrap_err();
    assert_eq!(e.0, "unknown_param", "{e:?}");
    assert!(e.1.contains("removed by R10.M3") && e.1.contains("use `notify`"), "{e:?}");
    let e = call(&m, &p, "agents_policy", json!({"agent": p, "notify": "hold", "wake": "never"})).await.unwrap_err();
    assert_eq!(e.0, "unknown_param", "{e:?}");
    assert!(e.1.contains("use `notify`"), "{e:?}");
    let e = call(&m, &p, "agents_spawn", json!({"task": "t", "notify": "loud"})).await.unwrap_err();
    assert_eq!(e.0, "malformed", "{e:?}");
    let e = call(&m, &p, "agents_spawn", json!({"task": "t", "notify": {"gather": "g:1"}})).await.unwrap_err();
    assert_eq!(e.0, "not_available_yet", "{e:?}");
    assert!(e.1.contains("R10.M5"), "{e:?}");
    assert_eq!(m.store.list_threads(100_000).unwrap().len(), before, "a refused spawn spawned");
    // Opposing: the replacement is accepted.
    let ack = call(&m, &p, "agents_spawn", json!({"task": "t", "notify": "hold"})).await.unwrap();
    assert!(ack["thread_id"].is_string(), "{ack}");
}

/// **THE MIGRATION `notify_policy_table`: IDEMPOTENT, AND A STORE FROM BEFORE
/// R10.M3 GAINS THE TABLE.** Open a store, drop the table (a database written
/// before R10.M3), re-open: the table is back and writable. Open again: the
/// row survives — the re-run is a no-op.
///
/// Forced red in the report: the `CREATE TABLE` without `IF NOT EXISTS` (the
/// second open fails).
#[test]
fn the_policy_table_migration_is_idempotent_and_reaches_an_old_store() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("threads.db");
    drop(threads::store::Store::open(&db).unwrap());
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("DROP TABLE notify_policy").unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notify_policy'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "sentinel: the store now predates R10.M3");
    }
    let s = threads::store::Store::open(&db).unwrap();
    s.set_notify_policy("o", "p", "home:o", "hold", "{}", "2026-10-25T00:00:00Z").unwrap();
    s.set_notify_policy("o", "p", "home:o", "direct", "{}", "2026-10-25T00:00:01Z").unwrap();
    drop(s);
    for _ in 0..2 {
        let s = threads::store::Store::open(&db).unwrap();
        assert_eq!(s.notify_policy("o", "p", "home:o").unwrap().as_deref(), Some("direct"), "latest write wins and survives re-open");
        assert_eq!(s.notify_policies_of("o").unwrap().len(), 1);
    }
}

// ---------------------------------------------------------------------------
// R10.M3b (implements R10.M3, Q-23 option B): the old `wake` keeps working for
// ONE release, with its old meaning and a warning.
// DEPRECATED(wake, remove after DEPLOY-9n): delete this section with the param.
// ---------------------------------------------------------------------------

const WARNING: &str = "`wake` is deprecated and will be refused next release; omit it to be woken by receipts, or use `agents_policy`.";

/// Eager (non-`ride`) rows still waiting on `id`'s queue: the only thing the
/// resume tick (60 s here, the longest period in the path) could ever turn
/// into a turn. None, plus no turn, outlasts the tick structurally.
fn eager_waiting(m: &ThreadManager, id: &str) -> usize {
    m.store.waiting_queue(id).unwrap().into_iter().filter(|r| r.dispatch != "ride").count()
}

/// **`wake: never` KEEPS ITS OLD MEANING — N = 2 CHILDREN EACH WAY.**
///
/// * c3, c4 spawned `wake: end`: each one's spawn report wakes p (the
///   default, opposing arm).
/// * c1, c2 spawned `wake: never`: no policy row is written (sentinel: never
///   is NOT hold); after both task runs end, p has started no turn and holds
///   no eager row. p re-tasks each by mail: their later runs end and post no
///   reply that wakes p.
/// * each of c1, c2 then mails p (`agents_message`): `delivered`, and p is
///   woken within [`WOKEN_WITHIN`] s.
///
/// Forced red in the report: `never` mapped to a `hold` row (the mail is
/// `held`, p not woken) and `never` mapped to `end` (p woken by the spawn
/// report).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wake_never_keeps_its_old_meaning_no_run_end_wake_but_mail_wakes_n2() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());

    // Opposing arm: `wake: end` ⇒ woken, twice.
    for _ in 0..2 {
        let p0 = turns(&m, &p);
        spawn(&m, &p, json!({"wake": "end"})).await;
        wait_for("p woken by a `wake: end` child's spawn report", WOKEN_WITHIN, || turns(&m, &p) > p0).await;
        idle(&m, &p).await;
    }

    let p0 = turns(&m, &p);
    let c1 = spawn(&m, &p, json!({"wake": "never"})).await;
    let c2 = spawn(&m, &p, json!({"wake": "never"})).await;
    assert!(m.store.notify_policies_of(&p).unwrap().is_empty(), "`wake: never` wrote a policy row: never is not hold");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(turns(&m, &p), p0, "a `wake: never` child's spawn report woke p");
    assert_eq!(eager_waiting(&m, &p), 0, "an eager row waits on p: the tick would wake it");

    for c in [&c1, &c2] {
        retask(&m, &p, c, json!({})).await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(turns(&m, &p), p0, "a `wake: never` child's later run end woke p");
    assert_eq!(eager_waiting(&m, &p), 0, "an eager row waits on p after the later runs");

    for (i, c) in [&c1, &c2].into_iter().enumerate() {
        let pb = turns(&m, &p);
        let ack = call(&m, c, "agents_message", json!({"agent": p, "body": format!("receipt {i}")})).await.unwrap();
        assert_eq!(ack["delivery"], json!("delivered"), "a `wake: never` child's mail was not delivered: {ack}");
        wait_for("p woken by a `wake: never` child's own mail", WOKEN_WITHIN, || turns(&m, &p) > pb).await;
        idle(&m, &p).await;
    }
}

/// **`wake` BESIDE `notify` IS REFUSED, ON BOTH DOORS, AND NOTHING IS
/// SPAWNED OR COUNTED.** Both pairings (never+direct, end+hold) on the MCP
/// façade and on the `/ws` arm (`managed::spawn`). Opposing: `wake` alone and
/// `notify` alone are each accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wake_beside_notify_is_refused_on_both_doors() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let before = m.store.list_threads(100_000).unwrap().len();
    for (w, n) in [("never", "direct"), ("end", "hold")] {
        let e = call(&m, &p, "agents_spawn", json!({"task": "t", "wake": w, "notify": n})).await.unwrap_err();
        assert_eq!(e.0, "malformed", "{e:?}");
        assert!(e.1.contains("both `wake` and `notify`"), "{e:?}");
        match threads::managed::spawn(&m, &json!({"caller": p, "task": "t", "wake": w, "notify": n})) {
            Err(threads::error::Error::Refused { code, message }) => {
                assert_eq!(code, "malformed");
                assert!(message.contains("both `wake` and `notify`"), "{message}");
            }
            other => panic!("the /ws door accepted `wake` beside `notify`: {other:?}"),
        }
    }
    assert_eq!(m.store.list_threads(100_000).unwrap().len(), before, "a refused spawn spawned");
    assert!(customs_on(&m, &p, "deprecated_param").is_empty(), "a refused spawn was counted");
    assert!(m.store.notify_policies_of(&p).unwrap().is_empty(), "a refused spawn wrote a policy row");
    // Opposing: each word alone.
    for args in [json!({"task": "t", "wake": "never"}), json!({"task": "t", "notify": "hold"})] {
        let ack = call(&m, &p, "agents_spawn", args.clone()).await.unwrap_or_else(|e| panic!("{args} refused: {e:?}"));
        assert!(ack["thread_id"].is_string(), "{ack}");
    }
}

/// **A SPAWN WITH `wake` IS ANSWERED WITH THE WARNING AND COUNTED ON THE
/// CALLER'S LEDGER** — N = 3 (end and never on the MCP façade, never on the
/// `/ws` arm). Opposing: a bare spawn and a `notify` spawn carry no
/// `warning` and add no `deprecated_param` event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wake_spawn_carries_the_warning_and_is_counted_on_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    // Opposing first: nothing counted, no warning.
    for args in [json!({"task": "t"}), json!({"task": "t", "notify": "direct"})] {
        let ack = call(&m, &p, "agents_spawn", args).await.unwrap();
        assert!(ack["thread_id"].is_string(), "{ack}");
        assert!(ack.get("warning").is_none(), "a spawn without `wake` was warned: {ack}");
    }
    assert!(customs_on(&m, &p, "deprecated_param").is_empty(), "a spawn without `wake` was counted");

    let mut kids = Vec::new();
    for w in ["end", "never"] {
        let ack = call(&m, &p, "agents_spawn", json!({"task": "t", "wake": w})).await.unwrap();
        assert_eq!(ack["warning"], json!(WARNING), "{ack}");
        kids.push((w, ack["thread_id"].as_str().unwrap().to_string()));
    }
    let ack = threads::managed::spawn(&m, &json!({"caller": p, "task": "t", "wake": "never"})).unwrap();
    assert_eq!(ack["warning"], json!(WARNING), "the /ws door: {ack}");
    kids.push(("never", ack["thread_id"].as_str().unwrap().to_string()));

    let ev = customs_on(&m, &p, "deprecated_param");
    assert_eq!(ev.len(), 3, "{ev:?}");
    for ((w, c), e) in kids.iter().zip(&ev) {
        assert_eq!(e["act"], json!("agents_spawn"), "{e}");
        assert_eq!(e["param"], json!("wake"), "{e}");
        assert_eq!(e["value"], json!(w), "{e}");
        assert_eq!(e["child_id"], json!(c), "{e}");
    }
}

#[path = "support/first_exec.rs"]
mod first_exec;
