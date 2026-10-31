//! **R10.M2 — AN OPERATOR CANCEL DEFERS, IT NEVER DROPS** (L-8, E21-4).
//!
//! The operator: *"cancel their delivery but keep them ready to deliver on the
//! next turn … separately track sent and whether to send … I don't want to
//! lose the messages, above all."* (`docs/MESSAGING-ORCHESTRATION.md` §5.3
//! rules 2–3, §10.2 row R10.M2.)
//!
//! One test per operator-cancel path that used to leave a queued message or a
//! prompt in a terminal state:
//!
//! | path | test |
//! |---|---|
//! | `cancel_turn`, an OPEN attempt (rows attempted, no `turn_started`) | `three_queued_cancelled_mid_attempt_ride_the_next_turn_in_order` |
//! | `cancel_turn`, a retry ladder's prompt | `a_cancelled_retry_ladders_message_rides_the_next_turn` |
//! | `cancel_turn`, a parked turn's prompt | `a_cancelled_parked_turns_message_rides_the_next_turn` |
//! | `cancel`, the cascade's queued rows (N1's walk) | `a_cancelled_subtrees_queued_messages_ride_and_never_start_a_turn` |
//! | `cancel`, a member gated with only queued work | `a_gated_member_cancelled_rides_its_queue_and_ends_cancelled_to_its_parent` |
//!
//! The opposing case in each is a thread shaped the same and NOT cancelled,
//! or the operator's explicit withdraw — the one act that may stay terminal.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shapewire::token::Claims;
use threads::agents;
use threads::config::Config;
use threads::{build_manager, ThreadEvent, ThreadManager};

static SERIAL: Mutex<()> = Mutex::new(());

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env(tmp: &Path) {
    std::env::set_var("CAULDRON_HOME", tmp.join("cauldron"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    // Every redelivery path ticks at 100 ms here, so the 1.5 s quiet windows
    // below outlast fifteen resume ticks (doctrine 4).
    std::env::set_var("THREADS_RESUME_TICK_MS", "100");
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

struct Life {
    rt: tokio::runtime::Runtime,
    m: ThreadManager,
}

impl Life {
    fn boot(dir: &Path) -> Life {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let m = rt.block_on(async {
            threads::clock::testing::ticking();
            let m = build_manager(config_at(dir)).unwrap();
            m.recover().await;
            m
        });
        Life { rt, m }
    }

    fn kill(self) {
        let Life { rt, m } = self;
        rt.shutdown_background();
        drop(m);
    }

    fn thread(&self, dir: &Path) -> String {
        let settings = json!({
            "cwd": dir.to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
        });
        let _ctx = self.rt.enter();
        self.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    }

    fn call(&self, caller: &str, tool: &str, args: Value) -> Result<Value, shapewire::envelope::Refusal> {
        let claims = Claims {
            thread_id: caller.to_string(),
            run_id: format!("{caller}/run-1"),
            root: caller.to_string(),
            depth: 0,
            turn: "cookie".into(),
        };
        self.rt.block_on(agents::call(&self.m, &claims, tool, args))
    }

    fn until(&self, secs: u64, mut f: impl FnMut(&ThreadManager) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if f(&self.m) {
                return true;
            }
            self.rt.block_on(async { tokio::time::sleep(Duration::from_millis(25)).await });
        }
        f(&self.m)
    }

    fn quiet(&self, ms: u64) {
        self.rt.block_on(async { tokio::time::sleep(Duration::from_millis(ms)).await });
    }

    fn send(&self, id: &str, text: &str) -> u64 {
        let _ctx = self.rt.enter();
        self.m.send(id, text.into(), threads::SendIntent::Immediate, json!({}), false).unwrap()
    }

    fn cancel(&self, id: &str) {
        let _ctx = self.rt.enter();
        self.m.cancel(id).unwrap();
    }
}

fn stored(m: &ThreadManager, id: &str) -> Vec<threads::StoredEvent> {
    m.store.tail_events(id, 5000).unwrap()
}

fn events(m: &ThreadManager, id: &str) -> Vec<ThreadEvent> {
    stored(m, id).into_iter().map(|e| e.event).collect()
}

/// The message ids of every `turn_started`, one Vec per turn, in order.
fn turns(m: &ThreadManager, id: &str) -> Vec<Vec<u64>> {
    events(m, id)
        .into_iter()
        .filter_map(|e| match e {
            ThreadEvent::TurnStarted { message_ids, .. } => Some(message_ids),
            _ => None,
        })
        .collect()
}

fn ended_turns(m: &ThreadManager, id: &str) -> usize {
    events(m, id).iter().filter(|e| matches!(e, ThreadEvent::ResultTurn { .. })).count()
}

fn operator_interrupts(m: &ThreadManager, id: &str) -> usize {
    events(m, id)
        .iter()
        .filter(|e| matches!(e, ThreadEvent::TurnInterrupted { trigger, .. } if trigger == "operator"))
        .count()
}

/// `(message_id, dispatch)` of every row `waiting_queue` offers.
fn queue(m: &ThreadManager, id: &str) -> Vec<(u64, String)> {
    m.store.waiting_queue(id).unwrap().into_iter().map(|r| (r.message_id, r.dispatch)).collect()
}

fn rides(m: &ThreadManager, id: &str, mids: &[u64]) -> bool {
    let q = queue(m, id);
    mids.iter().all(|mid| q.iter().any(|(i, d)| i == mid && d == "ride"))
}

fn ids(v: &Value) -> Vec<u64> {
    v.as_array().map(|a| a.iter().filter_map(|i| i.as_u64()).collect()).unwrap_or_default()
}

/// **THE SENT RECORD, PER MESSAGE**, read off the ledger in seq order:
/// `attempted` (`delivery_attempted` naming it), `cancelled`
/// (`delivery_cancelled` naming it), `deferred` (`dispatch_changed`), and
/// `delivered` (`turn_started` naming it).
fn sent_record(m: &ThreadManager, id: &str, mid: u64) -> Vec<String> {
    let mut out = Vec::new();
    for e in stored(m, id) {
        match e.event {
            ThreadEvent::Custom { event_type, data } => {
                let word = match event_type.as_str() {
                    "cauldron.threads/delivery_attempted" => "attempted",
                    "cauldron.threads/delivery_cancelled" => "cancelled",
                    "cauldron.threads/dispatch_changed" => "deferred",
                    _ => continue,
                };
                if ids(&data["message_ids"]).contains(&mid) {
                    out.push(word.to_string());
                }
            }
            ThreadEvent::TurnStarted { message_ids, .. } if message_ids.contains(&mid) => {
                out.push("delivered".to_string())
            }
            _ => {}
        }
    }
    out
}

fn cancelled_events(m: &ThreadManager, id: &str) -> Vec<Value> {
    events(m, id)
        .into_iter()
        .filter_map(|e| match e {
            ThreadEvent::Custom { event_type, data } if event_type == "cauldron.threads/delivery_cancelled" => Some(data),
            _ => None,
        })
        .collect()
}

struct Releases(Vec<PathBuf>);
impl Drop for Releases {
    fn drop(&mut self) {
        for r in &self.0 {
            let _ = std::fs::write(r, b"go");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn spawn_ok(l: &Life, caller: &str, task: &str, rid: &str) -> String {
    let ok = l
        .call(caller, "agents_spawn", json!({"task": task, "request_id": rid}))
        .unwrap_or_else(|e| panic!("spawn from {caller}: {} {}", e.code, e.message));
    ok["thread_id"].as_str().unwrap().to_string()
}

fn finished_outcomes(dir: &Path, parent: &str) -> BTreeMap<String, String> {
    let p = dir.join("ledgers").join(format!("{parent}.jsonl"));
    let Ok(l) = threads::ledger::Ledger::read_jsonl(&p) else { return BTreeMap::new() };
    l.events()
        .iter()
        .filter(|e| e.kind == threads::ledger::Kind::Child && e.body["phase"] == "finished")
        .map(|e| {
            (
                e.body["child_id"].as_str().unwrap().to_string(),
                e.body["outcome"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// Run a `holdturn` on `id`, then queue `texts` behind it; returns their ids.
fn hold_then_queue(a: &Life, id: &str, release: &Path, texts: &[&str]) -> Vec<u64> {
    a.send(id, &format!("holdturn release={}", release.display()));
    assert!(a.until(10, |m| m.store.get_running_turn(id).unwrap().is_some()), "{id}'s holdturn runs");
    let out: Vec<u64> = texts.iter().map(|t| a.send(id, t)).collect();
    let q: Vec<u64> = queue(&a.m, id).into_iter().map(|(i, _)| i).collect();
    assert_eq!(q, out, "sentinel: {id}'s messages wait behind its running turn, in order");
    out
}

// ═════════════════════════════════════════════════════════════════════════
// THE "PROVES" COLUMN OF §10.2 R10.M2
// ═════════════════════════════════════════════════════════════════════════

/// **CANCEL A THREAD WITH 3 QUEUED (IN AN OPEN ATTEMPT); SEND 1; THE NEXT
/// TURN'S `turn_started` NAMES ALL 4, IN ORDER; THE LEDGER SHOWS
/// attempted / cancelled / attempted / delivered FOR EACH OF THE 3.**
///
/// T runs a `holdturn`; A, B, C queue behind it, and W too — which the
/// operator then WITHDRAWS (`queue_cancel`, the one act that may stay
/// terminal). The drain that follows the holdturn opens ONE attempt on A, B,
/// C and is held (`hold_attempts_for_test`) between the attempt and
/// `turn_started` — the window a real dispatch spends awaiting the opening
/// `/usage` edge. The operator cancels T there.
///
/// - nothing starts: past the held task's release and 1.5 s (fifteen 100 ms
///   resume ticks) T has still started only the holdturn — red when riders
///   are allowed to cause a turn, or the dispatch task ignores the cancel;
/// - D, sent next, starts a turn naming `[A, B, C, D]` — red when the cancel
///   drops (rows withdrawn / left attempted) or riders do not go first;
/// - each of A, B, C reads `attempted, cancelled, attempted, delivered`;
/// - opposed: W is never carried (a cancel that re-offered every row, the
///   withdrawn one included, fails), and U — shaped the same, its held attempt
///   released WITHOUT a cancel — delivers `[A', B', C']` in that attempt with
///   no `cancelled` (so the hold itself is not what stops a turn).
#[test]
fn three_queued_cancelled_mid_attempt_ride_the_next_turn_in_order() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let rel = |n: &str| tmp.path().join(format!("release-{n}"));
    let _releases = Releases(vec![rel("t"), rel("u")]);
    let a = Life::boot(tmp.path());

    let t = a.thread(tmp.path());
    let u = a.thread(tmp.path());
    let abcw = hold_then_queue(&a, &t, &rel("t"), &["MSG-A", "MSG-B", "MSG-C", "MSG-W"]);
    let (abc, w) = (abcw[..3].to_vec(), abcw[3]);
    let abc_u = hold_then_queue(&a, &u, &rel("u"), &["MSG-A2", "MSG-B2", "MSG-C2"]);
    {
        let _ctx = a.rt.enter();
        assert!(a.m.cancel_queued(&t, w).unwrap().0, "the operator withdraws W");
    }
    for id in [&t, &u] {
        a.m.hold_attempts_for_test(id).unwrap();
    }
    std::fs::write(rel("t"), b"go").unwrap();
    std::fs::write(rel("u"), b"go").unwrap();
    assert!(
        a.until(10, |m| m.open_attempt_for_test(&t).unwrap() == Some(abc.clone())),
        "sentinel: T's drain opened one attempt on A, B, C and is held there (open: {:?})",
        a.m.open_attempt_for_test(&t).unwrap()
    );
    assert!(a.until(10, |m| m.open_attempt_for_test(&u).unwrap() == Some(abc_u.clone())), "sentinel: and U's");
    assert_eq!(turns(&a.m, &t).len(), 1, "sentinel: only the holdturn has started on T");
    for mid in &abc {
        assert_eq!(sent_record(&a.m, &t, *mid), ["attempted"], "sentinel: {mid} is attempted, not delivered");
    }

    a.cancel(&t);
    assert_eq!(a.m.open_attempt_for_test(&t).unwrap(), None, "the cancel took the open attempt");
    assert!(rides(&a.m, &t, &abc), "A, B, C are back on the queue `ride`: {:?}", queue(&a.m, &t));
    assert_eq!(operator_interrupts(&a.m, &t), 1, "the cancel is one operator interrupt");
    let c = cancelled_events(&a.m, &t);
    assert_eq!(c.len(), 1, "one delivery_cancelled: {c:?}");
    assert_eq!(ids(&c[0]["message_ids"]), abc);
    assert_eq!(c[0]["reason"], "operator_cancel");
    assert_eq!(c[0]["dispatch_now"], "ride");
    assert!(c[0]["attempt"].as_str().is_some_and(|s| s.starts_with("att:")), "it names the attempt it ended: {}", c[0]);

    a.m.release_attempt_for_test(&t).unwrap();
    a.m.release_attempt_for_test(&u).unwrap();
    assert!(a.until(10, |m| ended_turns(m, &u) == 2), "U's released attempt runs");
    assert_eq!(turns(&a.m, &u)[1], abc_u, "U delivers its held attempt, A' B' C' in order");
    assert!(cancelled_events(&a.m, &u).is_empty(), "and nothing on U was cancelled");
    a.quiet(1_500);
    assert_eq!(turns(&a.m, &t).len(), 1, "riders start no turn, past the release and fifteen resume ticks");
    assert!(a.m.is_idle(&t).unwrap(), "a thread holding only riders is idle");

    let d = a.send(&t, "MSG-D");
    assert!(a.until(10, |m| turns(m, &t).len() == 2 && ended_turns(m, &t) == 2), "D starts a turn");
    let mut want = abc.clone();
    want.push(d);
    assert_eq!(turns(&a.m, &t)[1], want, "the next turn names A, B, C, D in order");
    for mid in &abc {
        assert_eq!(
            sent_record(&a.m, &t, *mid),
            ["attempted", "cancelled", "attempted", "delivered"],
            "message {mid}'s sent record"
        );
    }
    assert!(!turns(&a.m, &t).concat().contains(&w), "the withdrawn W is never carried");
    assert!(queue(&a.m, &t).is_empty(), "nothing is left queued");
    a.kill();
}

// ═════════════════════════════════════════════════════════════════════════
// THE OTHER cancel_turn PATHS: A RETRY LADDER'S AND A PARKED TURN'S PROMPT
// ═════════════════════════════════════════════════════════════════════════

/// **A RETRY LADDER CANCELLED: THE LADDER IS DROPPED, ITS MESSAGE IS NOT — IT
/// RIDES THE NEXT TURN, AHEAD OF THE MESSAGE THAT STARTS IT.**
///
/// `badmodel` lands on the deterministic ladder (first rung 5 s). The
/// operator cancels. Red before R10.M2 at the release (`clear_retry` alone):
/// the row stayed `attempted`, the next turn named only D, and `badmodel`'s
/// text was never offered again. The retry row is gone and nothing runs past
/// 6 s (the first rung outlasted); D then starts a turn naming `[bad, D]`.
///
/// Opposed: a second thread on the same ladder, not cancelled, keeps its row
/// and is re-fired by its own ladder — so "drop every ladder" fails.
#[test]
fn a_cancelled_retry_ladders_message_rides_the_next_turn() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let t = a.thread(tmp.path());
    let kept = a.thread(tmp.path());
    let bad = a.send(&t, "badmodel");
    a.send(&kept, "badmodel");
    for id in [&t, &kept] {
        assert!(a.until(15, |m| m.store.get_retry(id).unwrap().is_some()), "sentinel: {id} is on a ladder");
    }
    assert_eq!(turns(&a.m, &t), vec![vec![bad]], "sentinel: one attempt has run");

    a.cancel(&t);
    assert!(a.m.store.get_retry(&t).unwrap().is_none(), "the ladder is dropped");
    assert!(rides(&a.m, &t, &[bad]), "its message rides: {:?}", queue(&a.m, &t));
    let c = cancelled_events(&a.m, &t);
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(ids(&c[0]["message_ids"]), vec![bad]);
    assert_eq!(c[0]["reason"], "operator_cancel");

    a.quiet(6_000);
    assert_eq!(turns(&a.m, &t).len(), 1, "nothing re-fires past the first 5 s rung");
    assert!(a.until(30, |m| turns(m, &kept).len() >= 2), "the UN-cancelled ladder re-fires: {:?}", turns(&a.m, &kept));
    assert_eq!(turns(&a.m, &t).len(), 1, "while the cancelled one still has not");

    let d = a.send(&t, "MSG-D");
    assert!(a.until(10, |m| turns(m, &t).len() == 2), "D starts a turn");
    assert_eq!(turns(&a.m, &t)[1], vec![bad, d], "the ladder's message rides first");
    a.cancel(&t);
    a.cancel(&kept);
    a.kill();
}

/// A thread flipped to `projected` whose ledger holds a prompt with no text:
/// every turn on it is refused by the render, which PARKS the prompt (a real
/// `parked_turns` row over a real queue row).
fn projection_refused_thread(a: &Life, dir: &Path, text: &str) -> (String, u64) {
    use threads::ledger::{Draft, Kind, Ledger, ThreadMeta};
    use threads::settings::{Projection, SettingsPatch};
    let id = a.thread(dir);
    a.m.update_settings(&id, SettingsPatch { projection: Some(Projection::Projected), ..Default::default() })
        .unwrap();
    let lp = threads::ledger::turn::ledger_path(dir, &id);
    std::fs::create_dir_all(lp.parent().unwrap()).unwrap();
    let mut seeded = Ledger::new(ThreadMeta::root(id.clone(), "claude_code", "2026-10-10T00:00:00Z"));
    seeded
        .append(Draft::new(Kind::User, "2026-10-10T00:00:01Z", json!({"text": null})).with_id("lost-prompt"))
        .unwrap();
    seeded.write_jsonl(&lp).unwrap();
    let mid = a.send(&id, text);
    assert!(a.until(15, |m| m.store.get_parked_turn(&id).unwrap().is_some()), "the projection refusal parks {id}");
    (id, mid)
}

/// **A PARKED TURN CANCELLED: THE PARK IS DROPPED, ITS MESSAGE IS NOT — IT
/// RIDES THE NEXT TURN.**
///
/// Red before R10.M2 at the release (`clear_parked_turn` alone): the parked
/// prompt was the message's last live copy and the next turn named only D.
/// The parked row is gone, nothing redelivers it past 1.5 s of 100 ms resume
/// ticks plus three direct sweeps, and D starts a turn naming `[parked, D]`.
///
/// Opposed: a second parked thread, not cancelled, keeps its row and its
/// message is not set `ride`.
#[test]
fn a_cancelled_parked_turns_message_rides_the_next_turn() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    std::env::remove_var("THREADS_PROJECTION_HOLD_MS");
    let a = Life::boot(tmp.path());
    let (t, parked) = projection_refused_thread(&a, tmp.path(), "PARKED-THEN-CANCELLED");
    let (kept, kept_mid) = projection_refused_thread(&a, tmp.path(), "PARKED-STILL-OWED");
    let started_before = turns(&a.m, &t).len();

    a.cancel(&t);
    assert!(a.m.store.get_parked_turn(&t).unwrap().is_none(), "the park is dropped");
    assert!(rides(&a.m, &t, &[parked]), "its message rides: {:?}", queue(&a.m, &t));
    assert_eq!(ids(&cancelled_events(&a.m, &t)[0]["message_ids"]), vec![parked]);
    assert!(a.m.store.get_parked_turn(&kept).unwrap().is_some(), "the other park is untouched");
    assert!(!rides(&a.m, &kept, &[kept_mid]), "and its message is not deferred");

    for _ in 0..3 {
        a.rt.block_on(async { a.m.resume_sweep() });
    }
    a.quiet(1_500);
    assert_eq!(turns(&a.m, &t).len(), started_before, "nothing redelivers the cancelled park");

    let d = a.send(&t, "MSG-D");
    assert!(a.until(10, |m| turns(m, &t).len() == started_before + 1), "D starts a turn");
    assert_eq!(turns(&a.m, &t).last().unwrap(), &vec![parked, d], "the parked message rides first");
    a.kill();
}

// ═════════════════════════════════════════════════════════════════════════
// THE CASCADE (N1's walk, N1b's verb replaced): QUEUED ROWS RIDE
// ═════════════════════════════════════════════════════════════════════════

/// **A CANCELLED SUBTREE'S QUEUED MESSAGES — ON THE ROOT AND ON A CHILD — RIDE:
/// THEY START NO TURN, THE CHILD'S RUN ENDS `cancelled`, AND EACH IS CARRIED
/// BY THAT THREAD'S NEXT TURN.**
///
/// P runs a holdturn and spawns C, which runs one too; a follow-up queues
/// behind each. The operator cancels P. Red at the release (`cancel_turn`
/// only): both follow-ups were dispatched by the epilogues of the turns the
/// cancel cut — two turns he had just said not to take. With R9.N1b's
/// withdraw: never carried at all.
///
/// Opposed: O, an unrelated root shaped like P and not cancelled, runs its
/// queued follow-up as soon as its holdturn is released.
#[test]
fn a_cancelled_subtrees_queued_messages_ride_and_never_start_a_turn() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let rel = |n: &str| tmp.path().join(format!("release-{n}"));
    let _releases = Releases(["p", "c", "o"].iter().map(|n| rel(n)).collect());
    let a = Life::boot(tmp.path());

    let p = a.thread(tmp.path());
    let o = a.thread(tmp.path());
    let p_follow = hold_then_queue(&a, &p, &rel("p"), &["FOLLOW-UP-TO-P"]);
    let o_follow = hold_then_queue(&a, &o, &rel("o"), &["FOLLOW-UP-TO-O"]);
    let c = spawn_ok(&a, &p, &format!("holdturn release={}", rel("c").display()), "c");
    assert!(a.until(10, |m| m.store.get_running_turn(&c).unwrap().is_some()), "the child runs");
    let c_follow = vec![a.send(&c, "FOLLOW-UP-TO-C")];

    a.cancel(&p);
    assert!(rides(&a.m, &p, &p_follow), "the root's follow-up rides: {:?}", queue(&a.m, &p));
    assert!(rides(&a.m, &c, &c_follow), "the child's follow-up rides: {:?}", queue(&a.m, &c));
    assert!(
        a.until(10, |_| finished_outcomes(tmp.path(), &p).get(&c).map(String::as_str) == Some("cancelled")),
        "the child's run ends `cancelled` to its parent (riders do not keep it busy): {:?}",
        finished_outcomes(tmp.path(), &p)
    );
    std::fs::write(rel("o"), b"go").unwrap();
    assert!(a.until(10, |m| turns(m, &o).len() == 2), "the UN-cancelled root runs its follow-up");
    assert_eq!(turns(&a.m, &o)[1], o_follow);
    a.quiet(1_500);
    assert_eq!(turns(&a.m, &p).len(), 1, "the cancelled root started no turn for its rider");
    assert_eq!(turns(&a.m, &c).len(), 1, "nor did the cancelled child");
    for (id, follow) in [(&p, &p_follow), (&c, &c_follow)] {
        let d = a.send(id, "NEXT");
        assert!(a.until(10, |m| turns(m, id).len() == 2), "{id}: the next message starts a turn");
        let mut want = follow.clone();
        // P also holds the cascade's report: C's `cancelled` wake, withheld by
        // the cancel and queued `ride` (R10.M2) — it rides too, in queue order.
        if id == &p {
            let wake: Vec<u64> = stored(&a.m, &p)
                .into_iter()
                .filter(|e| matches!(&e.event, ThreadEvent::UserMessage { metadata, .. } if metadata["origin"] == "agents"))
                .filter_map(|e| e.message_id)
                .collect();
            assert_eq!(wake.len(), 1, "the cascade's report reached P once, as a wake");
            want.extend(wake);
        }
        want.push(d);
        assert_eq!(turns(&a.m, id)[1], want, "{id}: the riders go first, oldest first");
        assert_eq!(sent_record(&a.m, id, follow[0]), ["deferred", "attempted", "delivered"]);
    }
    a.kill();
}

/// **A MEMBER WHOSE WHOLE RUN IS ONE MESSAGE QUEUED AT THE ADMISSION GATE:
/// ITS MESSAGE RIDES, ITS GATE PARK IS CLEARED, ITS RUN ENDS `cancelled` TO
/// ITS PARENT, AND IT NEVER RUNS ONCE THE GATE LIFTS.**
///
/// The account's weekly reading is put over the default 95 % threshold, so a
/// grandchild G spawned under a running child is queued and parked at the
/// gate — and so is OC, the child of an unrelated root. The operator cancels
/// the grandparent. Red with the walk removed: G ran the moment the gate
/// lifted, on the cancelled parent's key. Red with R9.N1b's withdraw: G's task
/// is never carried anywhere — here it rides, and a message the operator
/// sends to G carries it.
///
/// Opposed: OC keeps its eager row and its park, and runs when the gate lifts.
#[test]
fn a_gated_member_cancelled_rides_its_queue_and_ends_cancelled_to_its_parent() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let rel = |n: &str| tmp.path().join(format!("release-{n}"));
    let _releases = Releases(["p", "c"].iter().map(|n| rel(n)).collect());
    let hold = |n: &str| format!("holdturn release={}", rel(n).display());
    let a = Life::boot(tmp.path());

    let parent = a.thread(tmp.path());
    a.send(&parent, &hold("p"));
    assert!(a.until(10, |m| m.store.get_running_turn(&parent).unwrap().is_some()), "the parent runs");
    let child = spawn_ok(&a, &parent, &hold("c"), "c");
    assert!(a.until(10, |m| m.store.get_running_turn(&child).unwrap().is_some()), "the child runs");

    a.m.budgets
        .observe("aa-owned", threads::gauge::Axis::WeeklyAll, 99.0, None, "test")
        .unwrap();
    let grandchild = spawn_ok(&a, &child, "GRANDCHILD-TASK-QUEUED-AT-THE-GATE", "g");
    let other = a.thread(tmp.path());
    let other_child = spawn_ok(&a, &other, "OTHER-TREE-TASK-QUEUED-AT-THE-GATE", "oc");
    for t in [&grandchild, &other_child] {
        assert!(
            a.until(10, |m| queue(m, t).len() == 1 && !m.park_state(t).unwrap().is_null()),
            "sentinel: {t} is queued and parked at the gate ({:?})",
            queue(&a.m, t)
        );
        assert!(turns(&a.m, t).is_empty(), "sentinel: {t} has not run");
    }
    let g_task = queue(&a.m, &grandchild)[0].0;

    a.cancel(&parent);
    assert!(rides(&a.m, &grandchild, &[g_task]), "the grandchild's task rides: {:?}", queue(&a.m, &grandchild));
    assert!(a.m.park_state(&grandchild).unwrap().is_null(), "its gate park is cleared");
    assert_eq!(operator_interrupts(&a.m, &grandchild), 1, "its run ended as one operator interrupt");
    assert!(
        a.until(10, |_| finished_outcomes(tmp.path(), &child).get(&grandchild).map(String::as_str) == Some("cancelled")),
        "the grandchild's run ended `cancelled` to its parent: {:?}",
        finished_outcomes(tmp.path(), &child)
    );
    assert_eq!(queue(&a.m, &other_child)[0].1, "eager", "the other tree's gated child keeps its eager row");
    assert!(!a.m.park_state(&other_child).unwrap().is_null(), "and its park");

    a.m.budgets
        .observe("aa-owned", threads::gauge::Axis::WeeklyAll, 10.0, None, "test")
        .unwrap();
    for _ in 0..3 {
        a.rt.block_on(async {
            a.m.resume_sweep();
            tokio::time::sleep(Duration::from_millis(300)).await;
        });
    }
    assert!(a.until(15, |m| ended_turns(m, &other_child) == 1), "the UN-cancelled gated child runs once the gate lifts");
    a.quiet(1_500);
    assert!(turns(&a.m, &grandchild).is_empty(), "the cancelled grandchild never ran on its own");

    let next = a.send(&grandchild, "OPERATOR-NEXT-TO-G");
    assert!(a.until(10, |m| turns(m, &grandchild).len() == 1), "the operator's next message starts a turn");
    assert_eq!(turns(&a.m, &grandchild)[0], vec![g_task, next], "carrying the deferred task first");
    a.kill();
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
