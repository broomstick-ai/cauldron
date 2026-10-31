//! # R10.W1 — a child's reply wakes its parent; mail is never silently held
//!
//! The live bug (P1, an internal thread): a coordinator sat idle 55 minutes
//! because its child's receipt — `agents_message` to the parent — rode
//! `home:<parent>` and was HELD (the home owner row carried no `wakeable`),
//! though the child was spawned into a network where the parent IS wakeable;
//! and a child run that a MESSAGE started (not the spawn) ended without waking
//! the parent at all (`managed::report` returned once the child was no longer
//! `outstanding`).
//!
//! Each guard is forced red in an internal report (substrate
//! `interface-layer`), with its opposing arm:
//!
//! * `the_8e4fca10_shape_*` — parent + child on a non-home network, the
//!   parent's own home lowered to not-wakeable: the child's unnamed
//!   `agents_message` to the parent must ride the network where the parent is
//!   wakeable and start a parent turn within a bound far below the resume
//!   tick (60 s here). Forced: the routing choice reverted (first route wins
//!   ⇒ home ⇒ held).
//! * `a_reply_over_home_wakes_*` — no other network: the home owner is
//!   wakeable by default (`direct`). Forced: `Flags::home_owner` without
//!   `wakeable` ⇒ held.
//! * `a_genuinely_unwakeable_*` — the sender's result says `held` (and its
//!   `mail_queued` fact), while the wakeable arms above say `delivered`.
//! * `a_message_triggered_run_end_wakes_*` — N = 2 children each re-tasked by
//!   the parent's mail: each run's end posts exactly one reply and wakes the
//!   parent; a `wake: never` child re-tasked the same way posts none.

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
        // The longest period in the dispatch path: a WAITING queue row is
        // dispatched by this tick at the latest. Every "woken" below must
        // happen well inside it, so it is the post's own wake, not the tick.
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

async fn call(m: &ThreadManager, caller: &str, name: &str, args: Value) -> Result<Value, String> {
    threads::agents::call(m, &claims(caller), name, args)
        .await
        .map_err(|r| format!("{} {}", r.code, r.message))
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

/// Every `user_message` metadata on `id`.
fn user_meta(m: &ThreadManager, id: &str) -> Vec<Value> {
    m.store
        .events_from(id, 0, 100_000)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            ThreadEvent::UserMessage { metadata, .. } => Some(metadata),
            _ => None,
        })
        .collect()
}

/// Spawn through the real façade; return once the child's task turn ended and
/// it is idle.
async fn spawn(m: &ThreadManager, parent: &str, extra: Value) -> String {
    let mut args = json!({"task": "child task: say done"});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    let ack = call(m, parent, "agents_spawn", args).await.unwrap_or_else(|e| panic!("agents_spawn refused: {e}"));
    let child = ack["thread_id"].as_str().unwrap().to_string();
    wait_for(&format!("{child}'s task turn"), 30, || {
        m.store.tail_events(&child, 50).unwrap().iter().any(|e| matches!(e.event, ThreadEvent::TurnEnded { .. }))
    })
    .await;
    idle(m, &child).await;
    child
}

/// The parent lowers its own `wakeable` in its home network (a member may
/// always lower its own flags).
async fn parent_holds_home(m: &ThreadManager, p: &str) {
    call(m, p, "agents_network_configure", json!({"network": format!("home:{p}"), "flags": {p: {"view": true, "configure": true}}}))
        .await
        .unwrap();
}

/// Give the child a route back to the parent over `home:<p>`: the parent's
/// own mail to it there (delivery grants a route back, §1.5). Waits for the
/// child's run it starts to end, and for the parent to settle.
async fn parent_mails_child_over_home(m: &ThreadManager, p: &str, c: &str) {
    let c0 = turns(m, c);
    call(m, p, "agents_message", json!({"agent": c, "body": "a note from your parent", "network": format!("home:{p}")}))
        .await
        .unwrap();
    wait_for("the child's mail-started turn", 20, || turns(m, c) > c0).await;
    idle(m, c).await;
    idle(m, p).await;
}

/// **THE 8e4fca10 SHAPE.** p spawns c into a minted network `dag` (p and c
/// both wakeable there, as `cauldron-interface-dag` was); p's mail to c over
/// `home:p` gives c its route back over home, which comes FIRST in c's
/// network order. p has lowered its own `wakeable` in its home — so home is a
/// route on which p cannot be woken. c answers WITHOUT naming a network: the
/// answer must ride `dag`, say `delivered`, and start p's turn within
/// [`WOKEN_WITHIN`] s against a 60 s resume tick.
///
/// Forced red in the report: `networks::route` reverted to "first route wins"
/// — c's answer rides `home:p`, is `held`, and p is never woken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_8e4fca10_shape_a_childs_answer_rides_the_network_that_wakes_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let c = spawn(&m, &p, json!({"network": {"name": "dag"}})).await;
    // R10.M3: `wake: never` is gone; a default (`direct`) child's spawn
    // report wakes p — let that turn start and end before measuring.
    wait_for("p woken by the spawn report", 20, || turns(&m, &p) >= 1).await;
    idle(&m, &p).await;
    parent_holds_home(&m, &p).await;
    parent_mails_child_over_home(&m, &p, &c).await;

    let p0 = turns(&m, &p);
    let ack = call(&m, &c, "agents_message", json!({"agent": p, "body": "receipt: done"})).await.unwrap();
    assert_eq!(ack["delivery"], json!("delivered"), "the child's answer must not be held: {ack}");
    let sent = customs_on(&m, &c, "mail_queued");
    let last = sent.last().unwrap();
    assert!(!last["network"].as_str().unwrap().starts_with("home:"), "rode home, not the network that wakes p: {last}");
    assert_eq!(last["delivery"], json!("delivered"), "{last}");
    assert_eq!(m.store.count_held_mail(&p).unwrap(), 0, "nothing held for p");
    wait_for("p woken by its child's answer", WOKEN_WITHIN, || turns(&m, &p) > p0).await;
    idle(&m, &p).await;
}

/// **A REPLY OVER HOME WAKES THE PARENT BY DEFAULT (`direct`).** c is in no
/// network but its parent's home; p mails it there, c answers unnamed. The
/// home owner is `wakeable` by default, so the answer is `delivered` and p's
/// turn starts within [`WOKEN_WITHIN`] s.
///
/// Forced red in the report: `Flags::home_owner` without `wakeable` (the
/// pre-R10.W1 row) — `held`, p not woken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_over_home_wakes_the_parent_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let c = spawn(&m, &p, json!({})).await;
    // R10.M3: `wake: never` is gone; a default (`direct`) child's spawn
    // report wakes p — let that turn start and end before measuring.
    wait_for("p woken by the spawn report", 20, || turns(&m, &p) >= 1).await;
    idle(&m, &p).await;
    parent_mails_child_over_home(&m, &p, &c).await;

    let p0 = turns(&m, &p);
    let ack = call(&m, &c, "agents_message", json!({"agent": p, "body": "receipt: done"})).await.unwrap();
    assert_eq!(ack["delivery"], json!("delivered"), "{ack}");
    assert_eq!(customs_on(&m, &c, "mail_queued").last().unwrap()["network"], json!(format!("home:{p}")));
    wait_for("p woken by its child's answer over home", WOKEN_WITHIN, || turns(&m, &p) > p0).await;
    idle(&m, &p).await;
}

/// **A GENUINELY UNWAKEABLE RECIPIENT: THE SENDER IS TOLD `held`.** p lowered
/// its home `wakeable` and shares no other network with c. c's answer is held
/// — and c's tool result says `delivery: held`, as does its `mail_queued`
/// fact (with a reason); the mail is in p's held store and starts no turn.
/// The opposing arm is every `delivered` above: a constant answer fails one
/// side or the other.
///
/// Forced red in the report: the result's `delivery` constant `delivered`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_genuinely_unwakeable_recipient_is_reported_held_to_the_sender() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let c = spawn(&m, &p, json!({"notify": "hold"})).await;
    idle(&m, &p).await;
    parent_holds_home(&m, &p).await;
    parent_mails_child_over_home(&m, &p, &c).await;

    let p0 = turns(&m, &p);
    let ack = call(&m, &c, "agents_message", json!({"agent": p, "body": "receipt: done"})).await.unwrap();
    assert_eq!(ack["delivery"], json!("held"), "{ack}");
    let fact = customs_on(&m, &c, "mail_queued").last().unwrap().clone();
    assert_eq!(fact["delivery"], json!("held"), "{fact}");
    assert!(fact["reason"].as_str().is_some_and(|r| r.contains("not wakeable")), "{fact}");
    assert_eq!(m.store.count_held_mail(&p).unwrap(), 1);
    assert!(m.store.waiting_queue(&p).unwrap().is_empty(), "held mail is no queue row, so no tick can start it");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(turns(&m, &p), p0, "held mail woke p");
}

/// **A MESSAGE-TRIGGERED RUN'S END WAKES THE PARENT, AS THE SPAWN'S DOES —
/// N = 2, EACH EXACTLY ONCE.** p spawns c1 and c2 (default wake) and is woken
/// once by their reports. p then re-tasks each with `agents_message`; each
/// run's end posts ONE reply (`reply:<child>:<turn>`) and p's turn starts
/// within [`WOKEN_WITHIN`] s. Opposing arm: c3, spawned `wake: never` and
/// re-tasked the same way, posts no reply and wakes nobody.
///
/// Forced red in the report: `managed::report`'s not-outstanding branch back
/// to `return Ok(())` — no reply, p never woken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_triggered_run_end_wakes_the_parent_once_per_run_for_each_child() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let c1 = spawn(&m, &p, json!({})).await;
    let c2 = spawn(&m, &p, json!({})).await;
    let c3 = spawn(&m, &p, json!({"notify": "hold"})).await;
    // The spawn reports (c1, c2) wake p; let it settle.
    wait_for("p woken by the spawn reports", 20, || turns(&m, &p) >= 1).await;
    idle(&m, &p).await;
    let replies = |m: &ThreadManager| -> Vec<String> {
        user_meta(m, &p).iter().filter_map(|md| md["reply_id"].as_str().map(str::to_string)).collect()
    };
    assert!(replies(&m).is_empty(), "the spawn's own run is the report, not a reply: {:?}", replies(&m));

    for (i, c) in [&c1, &c2].into_iter().enumerate() {
        let p0 = turns(&m, &p);
        let c0 = turns(&m, c);
        call(&m, &p, "agents_message", json!({"agent": c, "body": format!("second task {i}: say again")})).await.unwrap();
        wait_for("the child's mail-started run", 20, || turns(&m, c) > c0).await;
        wait_for(&format!("p woken by {c}'s message-triggered run end"), WOKEN_WITHIN, || turns(&m, &p) > p0).await;
        idle(&m, c).await;
        idle(&m, &p).await;
    }
    // c3: the parent asked not to be told (`wake: never`).
    let p0 = turns(&m, &p);
    let c0 = turns(&m, &c3);
    call(&m, &p, "agents_message", json!({"agent": c3, "body": "second task: say again"})).await.unwrap();
    wait_for("c3's mail-started run", 20, || turns(&m, &c3) > c0).await;
    idle(&m, &c3).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(turns(&m, &p), p0, "a wake: never child's run end woke p");

    let got = replies(&m);
    for c in [&c1, &c2] {
        let n = got.iter().filter(|r| r.starts_with(&format!("reply:{c}:"))).count();
        assert_eq!(n, 1, "exactly one reply from {c}: {got:?}");
    }
    assert!(!got.iter().any(|r| r.contains(&c3)), "no reply from the wake: never child: {got:?}");
    assert_eq!(got.len(), 2, "{got:?}");
}

/// How many messages on `p` are from child `c` — its explicit mail (any
/// `mail.from == c`) plus run-end replies naming it. One run's answer is one.
fn from_child(m: &ThreadManager, p: &str, c: &str) -> (usize, usize) {
    let metas = user_meta(m, p);
    let mail = metas.iter().filter(|md| md["mail"]["kind"] == "message" && md["mail"]["from"] == c).count();
    let replies = metas.iter().filter(|md| md["reply_id"].as_str().is_some_and(|r| r.starts_with(&format!("reply:{c}:")))).count();
    (mail, replies)
}

/// **R10.W1b (i) — ONE RUN, ONE ANSWER: A CHILD THAT MAILS ITS PARENT IN THE
/// RUN IS NOT ALSO REPLIED FOR** (the R10.W1 inspector's F4; R13's rule, a
/// delivered message is never sent twice). N = 2 default-wake children c1, c2
/// are each re-tasked by p's mail with a turn the test holds open; inside it
/// each sends p a receipt with `agents_message`; then the turn is released and
/// the run ends. p must hold exactly ONE message from each for that run (the
/// receipt; zero run-end replies). Opposing arm: c3, re-tasked the same way
/// but sending no receipt, gets exactly one run-end reply — the suppression is
/// not a constant.
///
/// Forced red in the report: the `answered_in_run` check removed (c1/c2 give
/// two), and `answered_in_run` constant `true` (c3 gives none).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_run_one_answer_a_child_that_mailed_its_parent_gets_no_second_reply() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let kids = [spawn(&m, &p, json!({})).await, spawn(&m, &p, json!({})).await, spawn(&m, &p, json!({})).await];
    wait_for("p woken by the spawn reports", 20, || turns(&m, &p) >= 1).await;
    idle(&m, &p).await;
    for (i, c) in kids.iter().enumerate() {
        let receipt = i < 2;
        let release = dir.path().join(format!("release-{i}"));
        let c0 = turns(&m, c);
        call(&m, &p, "agents_message", json!({"agent": c, "body": format!("holdturn release={} task {i}", release.display())}))
            .await
            .unwrap();
        wait_for("the child's held turn is running", 20, || m.store.get_running_turn(c).unwrap().is_some() && turns(&m, c) > c0).await;
        if receipt {
            let ack = call(&m, c, "agents_message", json!({"agent": p, "body": format!("receipt {i}: done")})).await.unwrap();
            assert_eq!(ack["delivery"], json!("delivered"), "{ack}");
        }
        std::fs::write(&release, b"go").unwrap();
        idle(&m, c).await;
        // The run-end reply (when one is owed) is posted in the turn's
        // epilogue, before the child reads idle; give the parent time to settle.
        tokio::time::sleep(Duration::from_millis(500)).await;
        idle(&m, &p).await;
    }
    for (i, c) in kids.iter().enumerate() {
        let (mail, replies) = from_child(&m, &p, c);
        if i < 2 {
            assert_eq!((mail, replies), (1, 0), "child {i} mailed p in its run: exactly one message, no run-end reply");
        } else {
            assert_eq!((mail, replies), (0, 1), "child {i} sent nothing: exactly one run-end reply");
        }
    }
}

/// **R10.W1b re-dispatch (inspector H1) — A HELD RECEIPT IS NOT THE RUN'S
/// ANSWER: THE RUN-END REPLY STILL WAKES THE PARENT.** c (default wake) is in
/// `dag`, where p lowers its own `wakeable`; p's home stays wakeable. p
/// re-tasks c over home with a held-open turn; inside it c answers p NAMING
/// `dag`, so the receipt is `held` (sentinel). The run ends: p must be woken
/// within [`WOKEN_WITHIN`] s (60 s resume tick) by exactly one run-end reply.
/// Opposing arm: `one_run_one_answer_*` — a DELIVERED receipt suppresses it.
///
/// Forced red in the report: `answered_in_run` counting held receipts too
/// (the `delivery == delivered` condition dropped) — p sleeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_receipt_does_not_suppress_the_run_end_reply_and_the_parent_wakes() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let c = spawn(&m, &p, json!({"network": {"name": "dag"}})).await;
    wait_for("p woken by the spawn report", 20, || turns(&m, &p) >= 1).await;
    idle(&m, &p).await;
    let dag = threads::networks::networks_of(&m, &p)
        .unwrap()
        .into_iter()
        .find(|n| !n.id.starts_with("home:"))
        .expect("the dag network");
    let mut mine = threads::networks::members(&m, &dag).unwrap()[&p];
    assert!(mine.wakeable, "sentinel: p starts wakeable in dag");
    mine.wakeable = false;
    call(&m, &p, "agents_network_configure", json!({"network": dag.id, "flags": {p.clone(): serde_json::to_value(mine).unwrap()}}))
        .await
        .unwrap();
    let release = dir.path().join("release-held");
    let c0 = turns(&m, &c);
    call(&m, &p, "agents_message", json!({"agent": c, "body": format!("holdturn release={} task held", release.display()), "network": format!("home:{p}")}))
        .await
        .unwrap();
    wait_for("c's held turn is running", 20, || m.store.get_running_turn(&c).unwrap().is_some() && turns(&m, &c) > c0).await;
    idle(&m, &p).await;
    let ack = call(&m, &c, "agents_message", json!({"agent": p, "body": "receipt: done", "network": dag.id})).await.unwrap();
    assert_eq!(ack["delivery"], json!("held"), "sentinel: the receipt is held: {ack}");
    let p0 = turns(&m, &p);
    std::fs::write(&release, b"go").unwrap();
    idle(&m, &c).await;
    wait_for("p woken by c's run end after a HELD receipt", WOKEN_WITHIN, || turns(&m, &p) > p0).await;
    idle(&m, &p).await;
    assert_eq!(from_child(&m, &p, &c).1, 1, "exactly one run-end reply");
}

/// **R10.W1b (ii) — THE `delivery` KEY IS EXEMPT FROM THE COMMAND-REPLY GUARD
/// FOR `agents_message` ONLY.** Every COMMAND tool on the surface is handed an
/// acknowledgement carrying `delivery`: `agents_message`'s passes, every other
/// command's is refused. Sentinel: more than one command was tried, and the
/// plain acknowledgement passes for each (so a refusal is the key's, not the
/// shape's).
///
/// Forced red in the report: the exemption widened to every command.
#[test]
fn the_delivery_key_is_allowed_on_agents_message_and_refused_on_every_other_command() {
    use threads::agents::{acknowledgement_only, Kind, SURFACE};
    let plain = json!({"promise_id": 1, "visible": true});
    let mut with = plain.clone();
    with["delivery"] = json!("held");
    let mut others = 0;
    for t in SURFACE.iter().filter(|t| matches!(t.kind, Kind::Command)) {
        assert!(acknowledgement_only(t, plain.clone()).is_ok(), "{}: the plain acknowledgement must pass", t.name);
        let r = acknowledgement_only(t, with.clone());
        if t.name == "agents_message" {
            assert!(r.is_ok(), "agents_message must carry `delivery`");
        } else {
            others += 1;
            assert!(r.is_err(), "{} carried `delivery` past the guard", t.name);
        }
    }
    assert!(others >= 5, "the surface offered only {others} other commands");
}

/// **R10.W1b (iii) — A REPLY WHOSE POST FAILED IS POSTED EXACTLY ONCE ACROSS
/// DAEMON LIVES** (the R10.W1 inspector's F2: `reconcile`'s re-post was
/// unguarded). Life one: c is re-tasked; its run ends; the reply's post fails
/// (`Halt::BeforeReplyPost`) and the live retry is pushed out of reach (600 s
/// backoff), so nothing reaches p — asserted, with `retries_owed` as the
/// sentinel that the failure happened. Life two (same data dir): boot
/// reconciliation posts it — one `reply_id` for c. Life three: boot runs
/// again — still exactly one.
///
/// Forced red in the report: `reconcile`'s reply branch made a no-op (life two
/// never posts), and `reply`'s `sent_with` dedupe removed (life three posts a
/// second).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_whose_post_failed_is_posted_exactly_once_across_daemon_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path()).await;
    let p = root(&m, dir.path());
    let c = spawn(&m, &p, json!({})).await;
    wait_for("p woken by the spawn report", 20, || turns(&m, &p) >= 1).await;
    idle(&m, &p).await;
    m.managed().set_retry_backoff(Duration::from_secs(600), Duration::from_secs(600));
    m.managed().halt_after(Some(threads::managed::Halt::BeforeReplyPost));
    let c0 = turns(&m, &c);
    call(&m, &p, "agents_message", json!({"agent": c, "body": "second task: say again"})).await.unwrap();
    wait_for("c's mail-started run", 20, || turns(&m, &c) > c0).await;
    idle(&m, &c).await;
    wait_for("the failed reply is owed a retry", 10, || m.managed().retries_owed() >= 1).await;
    assert_eq!(from_child(&m, &p, &c).1, 0, "the halted post reached p");
    drop(m);

    for life in [2, 3] {
        let m = open(dir.path()).await;
        wait_for(&format!("life {life}: the reply is on p"), 20, || from_child(&m, &p, &c).1 >= 1).await;
        idle(&m, &p).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(from_child(&m, &p, &c).1, 1, "life {life}: exactly one reply from c");
        drop(m);
    }
}

#[path = "support/first_exec.rs"]
mod first_exec;
