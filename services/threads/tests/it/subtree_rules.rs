//! # R9.UI1 — the operator's two subagent rulings, at the daemon
//!
//! * **UI-a: recent threads hide subagents.** `threads/recent-updates` never
//!   carries a thread with a `parent_id` — not from the hydrate (the durable
//!   `last_user_input_at` order) and not from the live fold (the child's task
//!   `user_message`, which moves `last_user_input_at` like any other).
//! * **UI-b: a parent's status rolls up from its whole subtree.** The rule is
//!   `shapewire::subtree::rollup` (its own unit tests take rules 1–4 one by
//!   one); what the daemon owes is the input: `threads/non-idle` rows carry
//!   `ancestors`, so the rule applied to the served roster reaches a
//!   grandchild through an idle child that is not on the roster at all.
//!
//! Every spawn here is `wake: never`, so no report wakes a parent and moves its
//! real status under the levels these tests emit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shapewire::subtree::{rollup_in, Rollup};
use shapewire::token::Claims;
use threads::agents;
use threads::config::Config;
use threads::event::Push;
use threads::shape::{ConnFrames, Derived, Op, Shape};
use threads::shapes::{nonidle, recent};
use threads::{build_manager, ThreadEvent, ThreadManager};

static SERIAL: Mutex<()> = Mutex::new(());

fn set_env(tmp: &Path) {
    std::env::set_var("CAULDRON_HOME", tmp.join("cauldron"));
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var("FAKE_STATE_DIR", tmp.join("fake-state"));
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::remove_var("THREADS_CHASSIS_PORT");
    std::fs::create_dir_all(tmp.join("fake-state")).unwrap();
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

fn turn_ended(m: &ThreadManager, id: &str) -> bool {
    m.store
        .tail_events(id, 1000)
        .unwrap()
        .iter()
        .any(|e| matches!(e.event, ThreadEvent::ResultTurn { .. }))
        && m.store.get_running_turn(id).unwrap().is_none()
}

fn drain(f: &mut ConnFrames) -> Vec<Arc<threads::shape::ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

/// The newest whole row this connection holds for `key` — export or live.
fn newest_row(frames: &[Arc<threads::shape::ShapeFrame>], key: &str) -> Option<Value> {
    frames
        .iter()
        .filter(|f| f.wire.key.as_deref() == Some(key))
        .filter_map(|f| match (&f.wire.op, &f.wire.event, &f.wire.row) {
            (Op::Live, Some(Derived::Row { row }), _) => Some(row.clone()),
            (Op::Read, _, Some(row)) => Some(row.clone()),
            _ => None,
        })
        .last()
}

/// The ids `threads/recent-updates`' newest row carries, in order.
fn recent_ids(frames: &[Arc<threads::shape::ShapeFrame>]) -> Vec<String> {
    newest_row(frames, recent::FLEET)
        .and_then(|r| r["threads"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| e["id"].as_str().map(str::to_string))
        .collect()
}

const FRAME_DEADLINE: Duration = Duration::from_secs(20);

struct Fx {
    rt: tokio::runtime::Runtime,
    m: ThreadManager,
    tmp: tempfile::TempDir,
}

fn start() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let m = rt.block_on(async {
        threads::clock::testing::ticking();
        let m = build_manager(config_at(tmp.path())).unwrap();
        m.recover().await;
        m
    });
    Fx { rt, m, tmp }
}

impl Fx {
    fn thread(&self) -> String {
        let _ctx = self.rt.enter();
        let settings = json!({
            "cwd": self.tmp.path().to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
        });
        self.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    }
    /// `caller` spawns a `wake: never` child through the real `agents_spawn`.
    fn spawn(&self, caller: &str, rid: &str) -> String {
        let claims = Claims {
            thread_id: caller.to_string(),
            run_id: format!("{caller}/run-1"),
            root: caller.to_string(),
            depth: 0,
            turn: "cookie".into(),
        };
        let args = json!({"task": format!("task-{rid}"), "request_id": rid, "notify": "hold"});
        let out = self
            .rt
            .block_on(agents::call(&self.m, &claims, "agents_spawn", args))
            .unwrap_or_else(|e| panic!("agents_spawn from {caller}: {} {}", e.code, e.message));
        out["thread_id"].as_str().unwrap().to_string()
    }
    /// He speaks in `id`, through the real send path.
    fn speak(&self, id: &str) {
        let _ctx = self.rt.enter();
        self.m
            .send(id, "something he said".to_string(), threads::SendIntent::Immediate, json!({}), false)
            .expect("the send takes");
    }
    fn wait_ended(&self, ids: &[&str]) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ids.iter().all(|id| turn_ended(&self.m, id)) && Instant::now() < deadline {
            self.settle(25);
        }
        assert!(ids.iter().all(|id| turn_ended(&self.m, id)), "every run ended");
    }
    fn settle(&self, ms: u64) {
        self.rt.block_on(async { tokio::time::sleep(Duration::from_millis(ms)).await });
    }
    fn until(
        &self,
        f: &mut ConnFrames,
        acc: &mut Vec<Arc<threads::shape::ShapeFrame>>,
        done: impl Fn(&[Arc<threads::shape::ShapeFrame>]) -> bool,
    ) {
        let deadline = Instant::now() + FRAME_DEADLINE;
        loop {
            acc.extend(drain(f));
            if done(acc) || Instant::now() >= deadline {
                return;
            }
            self.settle(25);
        }
    }

    /// A parent `p` with a child `c` and a grandchild `g` (`c` spawned `g`),
    /// plus an unrelated top-level thread `t`. He speaks in `p` and `t`.
    fn family(&self) -> (String, String, String, String) {
        let p = self.thread();
        let t = self.thread();
        let c = self.spawn(&p, "c");
        self.wait_ended(&[&c]);
        let g = self.spawn(&c, "g");
        self.wait_ended(&[&g]);
        for id in [&p, &t] {
            self.speak(id);
            self.wait_ended(&[id]);
        }
        (p, c, g, t)
    }

    /// POSITIVE SENTINEL for UI-a: the durable order DOES hold the child and
    /// the grandchild (their task `user_message` stamped
    /// `last_user_input_at`), so a recents list without them is the filter's
    /// doing and not a fixture that never gave them a place.
    fn assert_the_column_holds(&self, ids: &[&str]) {
        let order: Vec<String> = self.m.store.recent_threads(100).unwrap().into_iter().map(|(id, _)| id).collect();
        for id in ids {
            assert!(order.iter().any(|o| o == id), "sentinel: {id} has a last_user_input_at: {order:?}");
        }
    }
}

fn assert_recents(ids: &[String], p: &str, c: &str, g: &str, t: &str) {
    assert!(ids.iter().any(|i| i == p), "the parent is in the recents: {ids:?}");
    assert!(ids.iter().any(|i| i == t), "the top-level thread is in the recents: {ids:?}");
    assert!(!ids.iter().any(|i| i == c), "the child is NOT in the recents: {ids:?}");
    assert!(!ids.iter().any(|i| i == g), "the grandchild is NOT in the recents: {ids:?}");
}

/// **UI-a, THE HYDRATE.** Everything happened before the subscribe, so the
/// cold export is `RecentOrder`'s answer off `threads.last_user_input_at`.
#[test]
fn ui_a_the_recents_hydrate_leaves_out_a_child_and_a_grandchild() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let (p, c, g, t) = fx.family();
    fx.assert_the_column_holds(&[&p, &c, &g, &t]);
    let mut f = fx.m.hub.connect(71);
    fx.m.hub.subscribe(recent::Recent::NAME, Some(recent::FLEET), 71, None).unwrap();
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| newest_row(fr, recent::FLEET).is_some());
    assert_recents(&recent_ids(&frames), &p, &c, &g, &t);
    fx.rt.shutdown_background();
}

/// **UI-a, THE LIVE FOLD.** Subscribed BEFORE anything exists: every id
/// reaches the index through the fold (the child's task `user_message`
/// included), and only the parent and the top-level thread are let in.
#[test]
fn ui_a_the_recents_fold_leaves_out_a_child_and_a_grandchild() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let mut f = fx.m.hub.connect(72);
    fx.m.hub.subscribe(recent::Recent::NAME, Some(recent::FLEET), 72, None).unwrap();
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| newest_row(fr, recent::FLEET).is_some());
    let (p, c, g, t) = fx.family();
    fx.assert_the_column_holds(&[&p, &c, &g, &t]);
    let (pp, tt) = (p.clone(), t.clone());
    fx.until(&mut f, &mut frames, move |fr| {
        let ids = recent_ids(fr);
        ids.contains(&pp) && ids.contains(&tt)
    });
    // Give a late child frame its chance to land before reading the order.
    fx.settle(300);
    frames.extend(drain(&mut f));
    assert_recents(&recent_ids(&frames), &p, &c, &g, &t);
    fx.rt.shutdown_background();
}

fn st(id: &str, state: &str, executing: bool) -> Push {
    Push::Status {
        thread_id: id.to_string(),
        state: state.to_string(),
        executing,
        queued: 0,
        unread: 0,
        detail: None,
        account: Some("aa-owned".to_string()),
    }
}

/// The roster's `(id, ancestors, state, executing)`.
fn roster(frames: &[Arc<threads::shape::ShapeFrame>]) -> Vec<(String, Vec<String>, String, bool)> {
    newest_row(frames, nonidle::FLEET)
        .and_then(|r| r["threads"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|e| {
            (
                e["id"].as_str().unwrap_or("").to_string(),
                e["ancestors"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default(),
                e["state"].as_str().unwrap_or("").to_string(),
                e["executing"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

/// UI-b applied to the SERVED roster, with `own` the thread's own level.
fn rolled(r: &[(String, Vec<String>, String, bool)], id: &str, own: &str, own_exec: bool) -> Rollup {
    rollup_in(id, own, own_exec, r.iter().map(|(_, a, s, e)| (a, s.as_str(), *e)))
}

/// **UI-b, AT THE DAEMON.** `p → c → g` plus a second child `c2`. Each phase
/// emits real `Push::Status` levels (the edge `threads/non-idle` folds), waits
/// for the roster to show them, and applies the shared rule to the roster AS
/// SERVED. `c` stays idle — off the roster — throughout, so `p` sees `g` only
/// through `g`'s `ancestors`: the 3-deep case.
#[test]
fn ui_b_the_non_idle_roster_carries_ancestors_and_the_rule_rolls_up_through_it() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = start();
    let p = fx.thread();
    let c = fx.spawn(&p, "c");
    fx.wait_ended(&[&c]);
    let mut f = fx.m.hub.connect(73);
    fx.m.hub.subscribe(nonidle::NonIdle::NAME, None, 73, None).unwrap();
    let mut frames = Vec::new();
    fx.until(&mut f, &mut frames, |fr| newest_row(fr, nonidle::FLEET).is_some());
    // `g` and `c2` are spawned AFTER the hydrate: their lineage reaches the
    // shape through `Fact::ThreadChildren` (the fold), `c`'s through the
    // hydrate.
    let g = fx.spawn(&c, "g");
    let c2 = fx.spawn(&p, "c2");
    fx.wait_ended(&[&g, &c2]);
    fx.settle(200);
    frames.extend(drain(&mut f));

    // Emit `levels` (re-stated past the 100 ms debounce until the frame shows
    // them, for `children_cell.rs`' reason) and return the served roster.
    let mut phase = |levels: &[(&str, &str, bool)]| {
        let want: Vec<(String, String, bool)> =
            levels.iter().map(|(i, s, e)| (i.to_string(), s.to_string(), *e)).collect();
        let shows = |fr: &[Arc<threads::shape::ShapeFrame>]| {
            let r = roster(fr);
            want.iter().all(|(id, s, e)| {
                if s == "idle" {
                    !r.iter().any(|x| &x.0 == id)
                } else {
                    r.iter().any(|x| &x.0 == id && &x.2 == s && x.3 == *e)
                }
            })
        };
        let deadline = Instant::now() + FRAME_DEADLINE;
        loop {
            for (id, s, e) in levels {
                fx.m.hub.emit(st(id, s, *e));
            }
            fx.settle(150);
            frames.extend(drain(&mut f));
            if shows(&frames) || Instant::now() >= deadline {
                break;
            }
        }
        roster(&frames)
    };

    // ── the 3-deep subtree, rule 2: only the grandchild is active ──────────
    let r = phase(&[(&p, "idle", false), (&c, "idle", false), (&c2, "idle", false), (&g, "active", true)]);
    let anc = |id: &str| r.iter().find(|x| x.0 == id).map(|x| x.1.clone());
    assert_eq!(anc(&g), Some(vec![c.clone(), p.clone()]), "g's ancestors, nearest first: {r:?}");
    assert!(!r.iter().any(|x| x.0 == c), "c is idle and off the roster: {r:?}");
    assert_eq!(rolled(&r, &p, "idle", false), Rollup::SubtreeActive, "p, through idle c: {r:?}");
    assert_eq!(rolled(&r, &c, "idle", false), Rollup::SubtreeActive, "c: {r:?}");

    // ── rule 2 against rule 3: a parked descendant beside an active one ────
    let r = phase(&[(&g, "parked", false), (&c2, "active", true)]);
    assert_eq!(anc_of(&r, &c2), Some(vec![p.clone()]), "{r:?}");
    assert_eq!(rolled(&r, &p, "idle", false), Rollup::SubtreeActive, "parked + active is active: {r:?}");

    // ── rule 3: parked only ───────────────────────────────────────────────
    let r = phase(&[(&c2, "idle", false)]);
    assert_eq!(rolled(&r, &p, "idle", false), Rollup::SubtreeParked, "parked only is parked: {r:?}");
    assert_eq!(rolled(&r, &c2, "idle", false), Rollup::Own, "c2 has no parked descendant: {r:?}");

    // ── rule 4: nothing below ─────────────────────────────────────────────
    let r = phase(&[(&g, "idle", false)]);
    assert_eq!(rolled(&r, &p, "idle", false), Rollup::Own, "nothing gives own state: {r:?}");

    // ── rule 1: the parent itself active, its descendants idle ─────────────
    let r = phase(&[(&p, "active", true)]);
    assert_eq!(anc_of(&r, &p), Some(vec![]), "a root's ancestors are empty: {r:?}");
    assert_eq!(rolled(&r, &p, "active", true), Rollup::SelfActive, "{r:?}");
    fx.rt.shutdown_background();
}

fn anc_of(r: &[(String, Vec<String>, String, bool)], id: &str) -> Option<Vec<String>> {
    r.iter().find(|x| x.0 == id).map(|x| x.1.clone())
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
