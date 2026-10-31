//! # R4.S8 — the spawn parameters, `intent: interrupt` and `agents_gather`, live
//!
//! HOLDS R20: `agents_spawn`'s `agent`, `effort`, `networks` and `network`,
//! `intent: interrupt`, and `agents_gather` (spelling (c)) answered
//! `not_available_yet`. Each now does what `docs/managed-subagents.md` §1.5 /
//! §2.4 / §2.5 and `docs/agent-networks.md` §4.2 say, and each has a guard
//! here written as an OPPOSED pair: the effect when the parameter is given,
//! beside its absence (or its refusal) when it is not — so neither a constant
//! "always" nor a constant "never" implementation passes.
//!
//! No real claude runs: `THREADS_CLAUDE_BIN` is `tests/fixtures/fake-claude-capture`,
//! which records the argv the daemon handed a turn whose prompt carries
//! `capture=<path>`, and holds that turn open until `release=<path>` exists.
//! A managed child's TASK is its first prompt, so a child spawned with
//! `capture=…` records its own argv: the evidence is what the daemon actually
//! put on the child's command line, not what the spawn wrote into a ledger.
//!
//! Every daemon has its own data dir under its own tempdir; nothing names a
//! live port, a live data dir, or `~/.cauldron`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shapewire::envelope::codes;
use shapewire::token::Claims;
use threads::config::Config;
use threads::event::ThreadEvent;
use threads::{build_manager, ThreadManager};

fn env() -> &'static Path {
    static ENV: OnceLock<tempfile::TempDir> = OnceLock::new();
    ENV.get_or_init(|| {
        let tmp = tempfile::tempdir().unwrap();
        let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude-capture"));
        std::env::set_var("CAULDRON_HOME", tmp.path().join("cauldron"));
        std::env::set_var("THREADS_CLAUDE_BIN", fake);
        std::env::set_var("THREADS_TAIL_POLL_MS", "25");
        std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
        std::env::set_var("FAKE_STATE_DIR", tmp.path().join("fake-state"));
        std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
        std::env::remove_var("THREADS_CHASSIS_PORT");
        // The gather backstop stretched past every bound below, so a gather
        // that returns early was woken by a turn end's ping (doctrine 4).
        std::env::set_var("THREADS_GATHER_RECHECK_MS", "60000");
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

struct Daemon {
    m: ThreadManager,
}

impl Daemon {
    async fn open(dir: &Path) -> Self {
        env();
        threads::clock::testing::ticking();
        let m = build_manager(config_at(dir)).unwrap();
        m.recover().await;
        Self { m }
    }

    fn thread(&self, cwd: &Path) -> String {
        let settings = json!({"cwd": cwd.to_string_lossy(), "model": "sonnet", "account": {"named": "aa-owned"}});
        self.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    }

    async fn call(&self, caller: &str, tool: &str, args: Value) -> Result<Value, shapewire::envelope::Refusal> {
        let claims = Claims {
            thread_id: caller.to_string(),
            run_id: format!("{caller}/run-1"),
            root: caller.to_string(),
            depth: 0,
            turn: "cookie".into(),
        };
        threads::agents::call(&self.m, &claims, tool, args).await
    }

    async fn spawn(&self, parent: &str, args: Value) -> Value {
        let mut a = args;
        a["notify"] = json!("hold");
        self.call(parent, "agents_spawn", a)
            .await
            .unwrap_or_else(|e| panic!("spawn: {} {}", e.code, e.message))
    }
}

async fn until(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f()
}

/// The argv the fake recorded for `capture`, once its turn has started.
async fn argv(capture: &Path) -> Vec<String> {
    let done = PathBuf::from(format!("{}.done", capture.display()));
    assert!(until(20, || done.exists()).await, "the captured turn never ran ({})", capture.display());
    std::fs::read_to_string(format!("{}.argv", capture.display()))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn flag_value(argv: &[String], flag: &str) -> Option<String> {
    argv.iter().position(|a| a == flag).and_then(|i| argv.get(i + 1)).cloned()
}

fn events(m: &ThreadManager, id: &str) -> Vec<ThreadEvent> {
    m.store.tail_events(id, 1000).unwrap().into_iter().map(|e| e.event).collect()
}

fn member_flags(m: &ThreadManager, net: &str, who: &str) -> Option<Value> {
    m.store
        .network_members(net)
        .unwrap()
        .into_iter()
        .find(|r| r.thread_id == who && r.state == "member")
        .map(|r| serde_json::from_str(&r.flags_json).unwrap())
}

fn networks_of(m: &ThreadManager, who: &str) -> Vec<String> {
    m.store.networks_with_member(who, "member").unwrap().into_iter().map(|r| r.id).filter(|i| !i.starts_with("home:")).collect()
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. `agent` and `effort` reach the child's command line
// ═════════════════════════════════════════════════════════════════════════════

/// **`agent` IS `--agent`, `effort` IS `--effort`, ON THE CHILD'S OWN ARGV —
/// AND NEITHER IS THERE WHEN THE SPAWN DID NOT NAME IT.**
///
/// Forcings (report): drop the `--agent` push in `SpawnConfig::build_args` →
/// the first assertion is red; push `--effort` unconditionally → the plain
/// child's assertion is red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_and_effort_reach_the_childs_argv_and_are_absent_when_unnamed() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    let named = dir.path().join("named");
    let plain = dir.path().join("plain");
    d.spawn(&r, json!({"task": format!("capture={}", named.display()), "agent": "reviewer", "effort": "high", "request_id": "n"})).await;
    d.spawn(&r, json!({"task": format!("capture={}", plain.display()), "request_id": "p"})).await;
    let a = argv(&named).await;
    assert_eq!(flag_value(&a, "--agent").as_deref(), Some("reviewer"), "{a:?}");
    assert_eq!(flag_value(&a, "--effort").as_deref(), Some("high"), "{a:?}");
    let b = argv(&plain).await;
    assert!(!b.iter().any(|x| x == "--agent" || x == "--effort"), "an unnamed agent/effort adds nothing: {b:?}");
}

/// **A BAD `effort` OR `agent` IS REFUSED BEFORE THE COMMIT POINT, AND NOTHING
/// IS SPAWNED; A GOOD ONE IS ACCEPTED.** `effort` is checked against the
/// CLI's five levels; `agent` is opaque but never empty, flag-shaped or a
/// non-string. The parameter that is still not honoured
/// (`max_thinking_tokens`) is still refused `not_available_yet`; `json_schema`
/// left that list at R11.J3 and is judged instead (`bad_schema`, section 5).
///
/// Forcing (report): delete the `EFFORT_LEVELS` check → `turbo` is accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_effort_or_agent_is_refused_before_the_commit_and_spawns_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    for (args, code) in [
        (json!({"task": "t", "effort": "turbo"}), codes::MALFORMED),
        (json!({"task": "t", "effort": 3}), codes::MALFORMED),
        (json!({"task": "t", "agent": ""}), codes::MALFORMED),
        (json!({"task": "t", "agent": "--dangerously"}), codes::MALFORMED),
        (json!({"task": "t", "agent": ["x"]}), codes::MALFORMED),
        (json!({"task": "t", "json_schema": {"type": 7}}), threads::managed::BAD_SCHEMA),
        (json!({"task": "t", "max_thinking_tokens": 5}), codes::NOT_AVAILABLE_YET),
    ] {
        let e = d.call(&r, "agents_spawn", args.clone()).await.expect_err(&args.to_string());
        assert_eq!(e.code, code, "{args}: {}", e.message);
    }
    assert_eq!(d.m.store.list_threads(100).unwrap().len(), 1, "nothing was spawned");
    assert!(d.m.managed().children_of(&r).is_empty());
    for lvl in threads::settings::EFFORT_LEVELS {
        d.spawn(&r, json!({"task": "t", "effort": lvl, "request_id": lvl})).await;
    }
    assert_eq!(d.m.managed().children_of(&r).len(), 5);
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. `network` and `networks`
// ═════════════════════════════════════════════════════════════════════════════

/// **`network` MINTS A TWO-MEMBER NETWORK WITH THE CHILD, IN THE SAME ACT;
/// WITHOUT IT, NONE IS MINTED.** The ack names the minted id; the roster is
/// exactly parent and child with the flags asked for (and the defaults when
/// not); the rules are the ones asked for; and the network carries mail —
/// the child messages its parent over it, which its home alone never allows
/// (N3: a child does not see its parent).
///
/// Forcing (report): `apply_spawn_networks` returns early → no roster, and
/// the child's message is `no_route`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_mints_a_two_member_network_with_the_child_and_none_without_it() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    let ack = d
        .spawn(&r, json!({"task": "t", "request_id": "c", "network": {"name": "pair", "child": {"view": true, "wakeable": true}, "rules": {"max_chars": 100}}}))
        .await;
    let c = ack["thread_id"].as_str().unwrap().to_string();
    let n = ack["network"].as_str().expect("the minted network is acknowledged").to_string();
    let mut roster: Vec<String> = d.m.store.network_members(&n).unwrap().into_iter().map(|r| r.thread_id).collect();
    roster.sort();
    let mut want = vec![r.clone(), c.clone()];
    want.sort();
    assert_eq!(roster, want);
    assert_eq!(member_flags(&d.m, &n, &c).unwrap(), json!({"view": true, "visible": false, "wakeable": true, "interruptible": false, "inspectable": false, "imposable": false, "members": false, "configure": false}));
    assert_eq!(member_flags(&d.m, &n, &r).unwrap()["members"], json!(true), "the parent gets creator flags by default");
    let row = d.m.store.get_network(&n).unwrap().unwrap();
    assert_eq!(row.name, "pair");
    assert_eq!(serde_json::from_str::<Value>(&row.rules_json).unwrap()["max_chars"], json!(100));
    // It carries mail the child could not otherwise send.
    d.call(&c, "agents_message", json!({"agent": r, "body": "hi", "network": n})).await.expect("c → r over the minted network");
    let e = d.call(&c, "agents_message", json!({"agent": r, "body": &"x".repeat(101), "network": n})).await.unwrap_err();
    assert_eq!(e.code, "too_large", "the minted network's rules hold");
    // A replay of the same request answers the same network, and mints no second.
    let again = d.spawn(&r, json!({"task": "t", "request_id": "c", "network": {}})).await;
    assert_eq!(again["network"], json!(n));
    assert_eq!(networks_of(&d.m, &r), vec![n.clone()]);

    // Without `network`: nothing minted, nothing acknowledged, and the child
    // still cannot address its parent.
    let plain = d.spawn(&r, json!({"task": "t", "request_id": "p"})).await;
    let p = plain["thread_id"].as_str().unwrap().to_string();
    assert!(plain.get("network").is_none(), "{plain}");
    assert!(networks_of(&d.m, &p).is_empty());
    assert_eq!(networks_of(&d.m, &r).len(), 1);
    assert_eq!(d.call(&p, "agents_message", json!({"agent": r, "body": "hi"})).await.unwrap_err().code, "no_route");

    // Bad shapes refused before the commit point.
    for net in [json!({"child": {"fly": true}}), json!({"rules": {"max_chars": 0}}), json!({"colour": 1}), json!("x")] {
        let e = d.call(&r, "agents_spawn", json!({"task": "t", "network": net.clone()})).await.expect_err(&net.to_string());
        assert!(e.code == codes::MALFORMED || e.code == "too_large", "{net}: {} {}", e.code, e.message);
    }
    assert_eq!(d.m.managed().children_of(&r).len(), 2, "no refused spawn made a child");
}

/// **`networks` ADMITS THE CHILD INTO NETWORKS THE PARENT HOLDS `members` IN
/// — AND ONLY THOSE.** Accepted: a network r created (`{id: flags}` form with
/// the flags given; `[id]` form with the defaults). Refused, before anything
/// is written: a network r is in without `members` (`no_right`), one it is not
/// in or that does not exist (`no_route`, the same answer), a home (`home`).
///
/// Forcing (report): drop the `members` check in `spawn_params` → the
/// `no_right` case spawns a child into a network its parent may not add to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn networks_admits_the_child_where_the_parent_holds_members_and_nowhere_else() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    let own = d.call(&r, "agents_network_create", json!({"name": "own"})).await.unwrap()["network"].as_str().unwrap().to_string();
    let c1 = d.spawn(&r, json!({"task": "t", "request_id": "c1", "networks": {own.clone(): {"view": true, "inspectable": true}}})).await["thread_id"]
        .as_str().unwrap().to_string();
    assert_eq!(member_flags(&d.m, &own, &c1).unwrap()["inspectable"], json!(true));
    assert_eq!(member_flags(&d.m, &own, &c1).unwrap()["visible"], json!(false));
    let c2 = d.spawn(&r, json!({"task": "t", "request_id": "c2", "networks": [own.clone()]})).await["thread_id"].as_str().unwrap().to_string();
    let f2 = member_flags(&d.m, &own, &c2).unwrap();
    assert_eq!((f2["view"].clone(), f2["visible"].clone(), f2["wakeable"].clone()), (json!(true), json!(true), json!(true)));
    // c1 and c2 now see each other over `own`: c2 is visible there.
    d.call(&c1, "agents_message", json!({"agent": c2, "body": "hi", "network": own})).await.expect("siblings joined at birth");

    // A network r is in WITHOUT `members`: x's network, with r seeded as a
    // view-only member (the row an accepted offer of `{view}` would leave).
    let x = d.thread(dir.path());
    let xs = d.call(&x, "agents_network_create", json!({"name": "xs"})).await.unwrap()["network"].as_str().unwrap().to_string();
    d.m.store
        .upsert_member(&threads::store::networks::MemberRow {
            network_id: xs.clone(),
            thread_id: r.clone(),
            flags_json: json!({"view": true}).to_string(),
            state: "member".into(),
            joined_by: x.clone(),
            joined_at: "2026-10-15T00:00:00Z".into(),
        })
        .unwrap();
    let before = d.m.managed().children_of(&r).len();
    for (nets, code) in [
        (json!({xs.clone(): {}}), "no_right"),
        (json!(["no-such-network"]), "no_route"),
        (json!([format!("home:{r}")]), "home"),
        (json!({own.clone(): {"fly": true}}), codes::MALFORMED),
        (json!("own"), codes::MALFORMED),
    ] {
        let e = d.call(&r, "agents_spawn", json!({"task": "t", "networks": nets.clone()})).await.expect_err(&nets.to_string());
        assert_eq!(e.code, code, "{nets}: {}", e.message);
    }
    assert_eq!(d.m.managed().children_of(&r).len(), before, "no refused spawn made a child");
    assert!(member_flags(&d.m, &xs, &c1).is_none());
}

/// **A SPAWN KILLED AFTER ITS COMMIT POINT GETS ITS NETWORKS FROM RECOVERY.**
/// The first daemon halts right after the parent's `child{started}` (no
/// thread row, no membership); the second, over the same data dir, recovers
/// the child — and the child is a member of the minted network and the named
/// one. The first life is shown to have written no membership, so what the
/// second shows is recovery's work.
///
/// Forcing (report): `materialize` without `apply_spawn_networks` → the
/// child exists after recovery but is in neither network.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spawn_killed_after_the_commit_gets_its_networks_from_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let (r, own, c) = {
        let d = Daemon::open(dir.path()).await;
        let r = d.thread(dir.path());
        let own = d.call(&r, "agents_network_create", json!({"name": "own"})).await.unwrap()["network"].as_str().unwrap().to_string();
        d.m.managed().halt_after(Some(threads::managed::Halt::AfterCommit));
        let e = d.call(&r, "agents_spawn", json!({"task": "t", "request_id": "k", "notify": "hold", "networks": [own.clone()], "network": {"name": "pair"}})).await;
        assert!(e.is_err(), "the halt stops the spawn as a kill would");
        let c = d.m.managed().children_of(&r).pop().expect("committed");
        assert!(d.m.store.get_thread(&c).unwrap().is_none());
        assert!(networks_of(&d.m, &c).is_empty(), "the first life wrote no membership");
        (r, own, c)
    };
    let d = Daemon::open(dir.path()).await;
    assert!(until(10, || d.m.store.get_thread(&c).unwrap().is_some()).await, "recovery made the child");
    let mine = networks_of(&d.m, &c);
    assert!(mine.contains(&own), "{mine:?}");
    assert_eq!(mine.len(), 2, "the named network and the minted one: {mine:?}");
    // The replayed spawn answers the network the commit recorded.
    let again = d.call(&r, "agents_spawn", json!({"task": "t", "request_id": "k", "notify": "hold"})).await.unwrap();
    assert!(mine.contains(&again["network"].as_str().unwrap().to_string()), "{again}");
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. `intent: interrupt`
// ═════════════════════════════════════════════════════════════════════════════

/// **`interrupt` CUTS THE RUNNING TURN AND THE MAIL IS THE NEXT TURN;
/// `immediate` DOES NOT CUT.** c runs a turn held open until a release file
/// that is never written. An `immediate` message leaves it running past
/// 1.5 s — sixty tail polls (25 ms) — so it was not cut; an `interrupt` then
/// ends it (a `turn_interrupted`, and no release was ever written) and the
/// mail runs as the turn that follows. A network that does not make c
/// `interruptible` refuses `not_interruptible`, and so does a broadcast to it.
///
/// Forcings (report): drop the `manager.cancel` in `deliver` → the turn is
/// never cut (red); cancel on every intent → the `immediate` leg is red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_cuts_the_running_turn_and_the_mail_runs_next_and_immediate_does_not_cut() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    let cap = dir.path().join("cap");
    let never = dir.path().join("never-released");
    let c = d.spawn(&r, json!({"task": format!("capture={} release={}", cap.display(), never.display()), "request_id": "c"})).await["thread_id"]
        .as_str().unwrap().to_string();
    argv(&cap).await;
    assert!(d.m.store.get_running_turn(&c).unwrap().is_some());
    let interrupted = |m: &ThreadManager| events(m, &c).iter().filter(|e| matches!(e, ThreadEvent::TurnInterrupted { .. })).count();

    // Not interruptible over a created network that does not say so.
    let calm = d.call(&r, "agents_network_create", json!({"name": "calm", "members": {c.clone(): {"wakeable": true}}})).await.unwrap()["network"]
        .as_str().unwrap().to_string();
    let e = d.call(&r, "agents_message", json!({"agent": c, "body": "stop", "intent": "interrupt", "network": calm})).await.unwrap_err();
    assert_eq!(e.code, "not_interruptible");
    let e = d.call(&r, "agents_broadcast", json!({"network": calm, "body": "stop", "intent": "interrupt"})).await.unwrap_err();
    assert_eq!(e.code, "not_interruptible");

    // `immediate` queues behind the running turn and does not cut it.
    d.call(&r, "agents_message", json!({"agent": c, "body": "later", "intent": "immediate", "network": format!("home:{r}")})).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(d.m.store.get_running_turn(&c).unwrap().is_some(), "immediate did not cut the turn");
    assert_eq!(interrupted(&d.m), 0);

    // `interrupt` over the home network (descendants are interruptible there).
    d.call(&r, "agents_message", json!({"agent": c, "body": "STOP-NOW", "intent": "interrupt", "network": format!("home:{r}")})).await.unwrap();
    assert!(until(10, || interrupted(&d.m) >= 1).await, "the running turn was cut");
    assert!(!never.exists());
    // …and the mail is delivered as the turn that follows, which then ends.
    assert!(
        until(20, || {
            let ev = events(&d.m, &c);
            let mail = ev.iter().rposition(|e| matches!(e, ThreadEvent::UserMessage { text, .. } if text.contains("STOP-NOW")));
            let cut = ev.iter().rposition(|e| matches!(e, ThreadEvent::TurnInterrupted { .. }));
            let ended = ev.iter().rposition(|e| matches!(e, ThreadEvent::ResultTurn { .. }));
            matches!((mail, cut, ended), (Some(_), Some(c), Some(t)) if t > c) && d.m.store.get_running_turn(&c).unwrap().is_none()
        })
        .await,
        "the mail ran as the turn after the cut"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 4. `agents_gather` — the bounded block
// ═════════════════════════════════════════════════════════════════════════════

/// **GATHER WAITS AT MOST `poll_s`, ANSWERS `satisfied: false` WITH THE
/// PENDING RUNS, AND ANSWERS `satisfied: true` WITH RESULTS THE MOMENT THE
/// PREDICATE HOLDS.** Two children held open.
///
/// * `all` over both, `poll_s: 1`: returns unsatisfied after ≥ 1 s and well
///   under 5 s, pending both (a constant `true` fails here).
/// * c1 released DURING a `poll_s: 30` gather for `any`: it returns satisfied,
///   long before 30 s — the turn-end ping, not the interval (a constant
///   `false`, or a gather that only sleeps out `poll_s`, fails here).
/// * `count:2` is still unsatisfied; `any_state:[running]` is satisfied.
/// * c2 released: `all` satisfied, both results carry the child's words.
///
/// Forcing (report): `Until::holds` → `false` always → the `any` leg times
/// out red; → `true` always → the first leg is red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gather_waits_boundedly_and_answers_when_the_predicate_holds() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    let (cap1, rel1) = (dir.path().join("cap1"), dir.path().join("rel1"));
    let (cap2, rel2) = (dir.path().join("cap2"), dir.path().join("rel2"));
    let a1 = d.spawn(&r, json!({"task": format!("capture={} release={}", cap1.display(), rel1.display()), "request_id": "g1"})).await;
    let a2 = d.spawn(&r, json!({"task": format!("capture={} release={}", cap2.display(), rel2.display()), "request_id": "g2"})).await;
    let (c1, c2) = (a1["thread_id"].as_str().unwrap().to_string(), a2["thread_id"].as_str().unwrap().to_string());
    argv(&cap1).await;
    argv(&cap2).await;

    let t = Instant::now();
    let g = d.call(&r, "agents_gather", json!({"runs": "children", "until": "all", "poll_s": 1})).await.unwrap();
    let took = t.elapsed();
    assert_eq!(g["satisfied"], json!(false), "{g}");
    assert_eq!(g["timed_out"], json!(false), "{g}");
    let mut pending: Vec<String> = g["pending"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    pending.sort();
    let mut both = vec![c1.clone(), c2.clone()];
    both.sort();
    assert_eq!(pending, both);
    assert!(took >= Duration::from_millis(950) && took < Duration::from_secs(5), "bounded by poll_s: {took:?}");

    // Release c1 a moment into a long gather.
    let rel = rel1.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(rel, b"go").unwrap();
    });
    let t = Instant::now();
    // Runs named by run id (the spawn's `run_id`) as well as by thread id.
    let g = d.call(&r, "agents_gather", json!({"runs": [a1["run_id"], c2], "until": "any", "poll_s": 30})).await.unwrap();
    assert_eq!(g["satisfied"], json!(true), "{g}");
    assert!(t.elapsed() < Duration::from_secs(10), "woken by the turn end, not the interval: {:?}", t.elapsed());
    let row1 = g["rows"].as_array().unwrap().iter().find(|x| x["thread_id"] == json!(c1)).unwrap().clone();
    assert_eq!(row1["state"], json!("ended"));
    assert_eq!(row1["run_id"], a1["run_id"]);
    assert!(row1["result"]["text"].as_str().unwrap().contains("Hello from fake claude."), "{row1}");
    assert_eq!(g["pending"], json!([c2]));

    let g = d.call(&r, "agents_gather", json!({"runs": "children", "until": "count:2", "poll_s": 1})).await.unwrap();
    assert_eq!(g["satisfied"], json!(false), "{g}");
    let g = d.call(&r, "agents_gather", json!({"runs": "children", "until": "any_state:[running]", "poll_s": 1})).await.unwrap();
    assert_eq!(g["satisfied"], json!(true), "{g}");

    std::fs::write(&rel2, b"go").unwrap();
    let g = d.call(&r, "agents_gather", json!({"runs": "children", "until": "all", "poll_s": 20})).await.unwrap();
    assert_eq!(g["satisfied"], json!(true), "{g}");
    assert_eq!(g["pending"], json!([]));
    for row in g["rows"].as_array().unwrap() {
        assert_eq!(row["outcome"], json!("completed"), "{row}");
    }
}

/// **`timeout_s` COUNTS ACROSS POLLS; A BAD PREDICATE, AN UNBOUNDED `poll_s`,
/// AND A RUN THAT IS NOT THE CALLER'S ARE REFUSED.** The first call (poll 1 s
/// of a 2 s timeout) is not timed out; the second, asked again, is — the
/// timeout ran from the FIRST ask, not this one (a per-call timeout would
/// answer `timed_out: false` again). A sibling, the caller itself and an
/// unknown id are `forbidden`.
///
/// Forcing (report): key the first-asked map per call (insert `now` always)
/// → the second call is not timed out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gather_timeout_counts_across_polls_and_bad_asks_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    let (cap, rel) = (dir.path().join("cap"), dir.path().join("rel"));
    let c = d.spawn(&r, json!({"task": format!("capture={} release={}", cap.display(), rel.display()), "request_id": "c"})).await["thread_id"]
        .as_str().unwrap().to_string();
    let sib = d.spawn(&r, json!({"task": "t", "request_id": "s"})).await["thread_id"].as_str().unwrap().to_string();
    argv(&cap).await;

    let ask = json!({"runs": [c], "until": "all", "poll_s": 1, "timeout_s": 2});
    let g = d.call(&r, "agents_gather", ask.clone()).await.unwrap();
    assert_eq!((g["satisfied"].clone(), g["timed_out"].clone()), (json!(false), json!(false)), "{g}");
    let t = Instant::now();
    let g = d.call(&r, "agents_gather", ask.clone()).await.unwrap();
    assert_eq!(g["timed_out"], json!(true), "the timeout ran from the first ask: {g}");
    assert!(t.elapsed() < Duration::from_millis(1500), "{:?}", t.elapsed());
    // A timed-out gather is forgotten: asking again starts a fresh timeout.
    let g = d.call(&r, "agents_gather", ask).await.unwrap();
    assert_eq!(g["timed_out"], json!(false), "{g}");

    for (caller, args, code) in [
        (&r, json!({"runs": "children", "until": "most"}), codes::MALFORMED),
        (&r, json!({"runs": "children", "until": "count:9"}), codes::MALFORMED),
        (&r, json!({"runs": "children", "until": "any_state:[sleeping]"}), codes::MALFORMED),
        (&r, json!({"runs": "children", "until": "all", "poll_s": 301}), codes::MALFORMED),
        (&r, json!({"runs": "kids", "until": "all"}), codes::MALFORMED),
        (&r, json!({"runs": ["no-such-run"], "until": "all"}), codes::FORBIDDEN),
        (&r, json!({"runs": [r.clone()], "until": "all"}), codes::FORBIDDEN),
        (&sib, json!({"runs": [c.clone()], "until": "all"}), codes::FORBIDDEN),
        (&c, json!({"runs": "children", "until": "all"}), codes::MALFORMED),
    ] {
        let e = d.call(caller, "agents_gather", args.clone()).await.expect_err(&args.to_string());
        assert_eq!(e.code, code, "{args}: {}", e.message);
    }
    std::fs::write(&rel, b"go").unwrap();
}

// ═════════════════════════════════════════════════════════════════════════════
// 5. `json_schema` — structured output, Phase A (R11.J3, STRUCTURED-OUTPUT §3.1, §3.2)
// ═════════════════════════════════════════════════════════════════════════════

/// The `--json-schema` value the fake recorded, parsed — `None` when the flag
/// is absent. Parsed rather than compared as text, so key order is not the
/// thing under test.
fn schema_flag(argv: &[String]) -> Option<Value> {
    flag_value(argv, "--json-schema").map(|s| serde_json::from_str(&s).expect("--json-schema carries JSON"))
}

/// The metadata of every `user_message` on `id`, in order.
fn user_meta(m: &ThreadManager, id: &str) -> Vec<Value> {
    events(m, id)
        .into_iter()
        .filter_map(|e| match e {
            ThreadEvent::UserMessage { metadata, .. } => Some(metadata),
            _ => None,
        })
        .collect()
}

/// **A SPAWN'S SCHEMA RIDES TURN 1'S ARGV AND NOT TURN 2'S; A SPAWN WITHOUT
/// ONE PUTS NONE ON ITS ARGV.** The child's first turn delivers its task and
/// carries `--json-schema` with the schema the spawn named (parsed equal);
/// a follow-up queued while that turn is held open runs as turn 2 and carries
/// no `--json-schema`; a sibling spawned without a schema carries none on its
/// first turn. The schema is recorded on the TASK message, in the daemon's
/// own metadata key, and not on the follow-up. Opposed: an implementation
/// that never passes the flag fails turn 1; one that always passes it, or
/// makes it sticky on the thread, fails the plain sibling or turn 2.
///
/// Forcings (report): drop the `--json-schema` push in `build_args` → turn 1
/// red; make `turn_json_schema` ignore `message_ids` (any schema the thread
/// ever had) → turn 2 red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spawns_schema_rides_turn_one_argv_and_not_turn_two() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    let schema = json!({"type": "object", "required": ["verdict"], "properties": {"verdict": {"type": "string", "enum": ["merge", "block"]}}});
    let (cap1, rel1, cap2) = (dir.path().join("cap1"), dir.path().join("rel1"), dir.path().join("cap2"));
    let c = d
        .spawn(&r, json!({"task": format!("capture={} release={}", cap1.display(), rel1.display()), "json_schema": schema, "request_id": "j"}))
        .await["thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    let plain = dir.path().join("plain");
    d.spawn(&r, json!({"task": format!("capture={}", plain.display()), "request_id": "p"})).await;

    let t1 = argv(&cap1).await;
    assert_eq!(schema_flag(&t1), Some(schema.clone()), "turn 1 carries the spawn's schema: {t1:?}");
    // Turn 1 is held open; this follow-up queues behind it and is turn 2.
    d.m.send(&c, format!("capture={}", cap2.display()), threads::event::SendIntent::Immediate, Value::Null, false).unwrap();
    std::fs::write(&rel1, b"go").unwrap();
    let t2 = argv(&cap2).await;
    assert!(t2.iter().any(|a| a == "--resume"), "turn 2 is a later turn of the same session: {t2:?}");
    assert_eq!(schema_flag(&t2), None, "turn 2 carries no schema: {t2:?}");
    let p = argv(&plain).await;
    assert_eq!(schema_flag(&p), None, "a spawn without a schema adds no --json-schema: {p:?}");

    let meta = user_meta(&d.m, &c);
    assert_eq!(meta.len(), 2, "{meta:?}");
    assert_eq!(meta[0][threads::managed::META_KEY]["json_schema"], schema, "recorded on the task message: {}", meta[0]);
    assert!(meta[1].get(threads::managed::META_KEY).is_none(), "and not on the follow-up: {}", meta[1]);
}

/// **A BAD OR OVERSIZED SCHEMA IS REFUSED BEFORE THE COMMIT POINT AND NOTHING
/// IS SPAWNED OR QUEUED; A GOOD ONE, UP TO EXACTLY THE CEILING, IS ACCEPTED.**
/// `bad_schema` for a document the 2020-12 meta-schema rejects, and for one
/// that does not compile (an invalid `pattern`); `schema_too_large` one byte
/// over 16 KiB. Opposed: the same act at exactly 16 KiB, and `{}`, spawn.
///
/// Forcings (report): delete the meta-schema check → `{"type": 7}` spawns
/// (red); flip the size comparison to `>=` → the exactly-16-KiB leg is red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_or_oversized_schema_is_refused_and_nothing_is_queued() {
    use threads::managed::{BAD_SCHEMA, MAX_SCHEMA_BYTES, SCHEMA_TOO_LARGE};
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let r = d.thread(dir.path());
    let sized = |n: usize| {
        let base = json!({"type": "object", "description": ""}).to_string().len();
        json!({"type": "object", "description": "x".repeat(n - base)})
    };
    assert_eq!(sized(MAX_SCHEMA_BYTES).to_string().len(), MAX_SCHEMA_BYTES);
    for (schema, code) in [
        (json!({"type": 7}), BAD_SCHEMA),
        (json!({"type": "object", "required": "verdict"}), BAD_SCHEMA),
        (json!({"type": "object", "properties": {"a": {"type": "string", "pattern": "("}}}), BAD_SCHEMA),
        (sized(MAX_SCHEMA_BYTES + 1), SCHEMA_TOO_LARGE),
    ] {
        let args = json!({"task": "t", "json_schema": schema, "notify": "hold"});
        let e = d.call(&r, "agents_spawn", args).await.expect_err("refused");
        assert_eq!(e.code, code, "{}", e.message);
    }
    assert_eq!(d.m.store.list_threads(100).unwrap().len(), 1, "nothing was spawned");
    assert!(d.m.managed().children_of(&r).is_empty(), "nothing was committed");
    // No child thread exists (above), so no task message was queued anywhere.
    // Opposed: good schemas spawn, the largest allowed among them.
    for (i, schema) in [json!({}), json!({"type": "object"}), sized(MAX_SCHEMA_BYTES)].into_iter().enumerate() {
        d.spawn(&r, json!({"task": "t", "json_schema": schema, "request_id": format!("ok{i}")})).await;
    }
    assert_eq!(d.m.managed().children_of(&r).len(), 3);
}

/// **A SPAWN KILLED AFTER ITS COMMIT GETS ITS SCHEMA FROM RECOVERY.** The
/// first life commits the spawn and dies before the child's thread row or
/// task message exist; the second life's recovery sends the task, and its
/// turn carries the schema — so the schema came from the committed
/// `child{started}` record, not from the call's parameters, which the second
/// life never saw.
///
/// Forcing (report): `materialize` without the `cauldron.threads` metadata →
/// the recovered turn has no `--json-schema` (red).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spawn_killed_after_the_commit_gets_its_schema_from_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let schema = json!({"type": "object", "required": ["ok"]});
    let cap = dir.path().join("cap");
    let c = {
        let d = Daemon::open(dir.path()).await;
        let r = d.thread(dir.path());
        d.m.managed().halt_after(Some(threads::managed::Halt::AfterCommit));
        let e = d
            .call(&r, "agents_spawn", json!({"task": format!("capture={}", cap.display()), "json_schema": schema, "request_id": "k", "notify": "hold"}))
            .await;
        assert!(e.is_err(), "the halt stops the spawn as a kill would");
        let c = d.m.managed().children_of(&r).pop().expect("committed");
        assert!(d.m.store.get_thread(&c).unwrap().is_none());
        c
    };
    let d = Daemon::open(dir.path()).await;
    let a = argv(&cap).await;
    assert_eq!(schema_flag(&a), Some(schema), "{a:?}");
    assert_eq!(user_meta(&d.m, &c).len(), 1);
}

/// **THE BATCH-LEVEL SCHEMA IS EVERY ITEM'S DEFAULT; AN ITEM'S OWN WINS.**
/// Opposed: an item with its own schema keeps it, an item without inherits,
/// and a batch without one adds nothing.
///
/// Forcing (report): `batch_item_params` returning the item unchanged → the
/// inherit leg is red; always taking the batch's → the own-schema leg is red.
#[test]
fn the_batch_schema_is_each_items_default_and_an_items_own_wins() {
    use threads::managed::batch_item_params;
    let (a, b) = (json!({"type": "object"}), json!({"type": "array"}));
    let batch = json!({"json_schema": a});
    assert_eq!(batch_item_params(&batch, &json!({"task": "t"}))["json_schema"], a);
    assert_eq!(batch_item_params(&batch, &json!({"task": "t", "json_schema": b}))["json_schema"], b);
    assert!(batch_item_params(&json!({}), &json!({"task": "t"})).get("json_schema").is_none());
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;

// ═════════════════════════════════════════════════════════════════════════════
// TUI-P1a — a spawned child is tagged as an agent's thread from birth
// ═════════════════════════════════════════════════════════════════════════════

/// The `metadata` blob a thread was CREATED with, off its stored row.
fn stored_metadata(m: &ThreadManager, id: &str) -> Value {
    let row = m.store.get_thread(id).unwrap().unwrap_or_else(|| panic!("no row for {id}"));
    let s: Value = serde_json::from_str(&row.settings_json).unwrap();
    s.get("metadata").cloned().unwrap_or(Value::Null)
}

/// **EVERY SPAWNED CHILD CARRIES `metadata.agent`, AND THE THREAD HE STARTED
/// DOES NOT.** A child copies its parent's settings, `app_id` included, so by
/// app id alone it looks like one of his; the tag is what makes "not his" true
/// in the settings it is CREATED with (TUI-P1a, checklist TC-05/TC-06). N = 2
/// children, each tagged with its own parent; the parent's client blob survives
/// the copy beside the tag.
///
/// Forcings (report): delete the `tag_agent` call in `managed::materialize` →
/// both children's assertions are red; tag the PARENT too (call `tag_agent` in
/// `create_thread_acked`) → the parent's opposing assertion is red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spawned_child_is_tagged_an_agents_thread_and_his_own_thread_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::open(dir.path()).await;
    let settings = json!({"cwd": dir.path().to_string_lossy(), "model": "sonnet",
        "account": {"named": "aa-owned"}, "app_id": "harness-tui",
        "metadata": {"aui_project": "_miscellaneous"}});
    let r = d.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap();
    let mut kids = Vec::new();
    for rid in ["k1", "k2"] {
        let ack = d.spawn(&r, json!({"task": "t", "request_id": rid})).await;
        kids.push(ack["thread_id"].as_str().unwrap().to_string());
    }
    assert_ne!(kids[0], kids[1], "two spawns, two children");
    for k in &kids {
        assert!(until(10, || d.m.store.get_thread(k).unwrap().is_some()).await, "child {k} has no row");
        let meta = stored_metadata(&d.m, k);
        assert_eq!(meta["agent"], json!(format!("agents_spawn:{r}")), "child {k} is not tagged: {meta}");
        assert_eq!(meta["aui_project"], json!("_miscellaneous"), "the parent's client blob was lost: {meta}");
    }
    let own = stored_metadata(&d.m, &r);
    assert!(own.get("agent").is_none(), "the thread HE started is tagged as an agent's: {own}");
}
