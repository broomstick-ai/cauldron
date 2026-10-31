//! **R13.1 — A DELIVERED MESSAGE IS NEVER RE-SENT.**
//!
//! an internal report (substrate, ee223e2): an internal thread
//! ("ripples") got message 62 four times — every retry, re-park and restart
//! recovery wrote the stored prompt into a `--resume`d conversation that
//! already held it and hours of work on it. The fix asks ONE door, the stdin
//! write in `run_once`, whether the ledger shows the message delivered; every
//! re-entry path reaches that door, and each path is driven here through its
//! own entry point:
//!
//! | path (cauldron main → this base)            | test                                   |
//! |---------------------------------------------|----------------------------------------|
//! | stdin write `run_once`                      | all of them (the fake records stdin)   |
//! | in-turn ladder (`run_turn` loop `RunAgain`) | `in_turn_failover_*`                   |
//! | `fire_retry` → `run_turn`                   | `a_fired_retry_*`                      |
//! | L-1 restart recovery → `schedule_retry`     | `a_restart_*`, `resume_after_kill_*`   |
//! | parked resume (`maybe_dispatch`)            | `a_parked_turn_*`                      |
//! | projected re-append (`ledger/turn.rs`)      | `a_projected_retry_*`                  |
//! | R10.M2-1 cancel of a retried delivery       | `cancelling_a_delivered_retry_*`       |
//!
//! Every path has both arms (doctrine 2): DELIVERED (the model answered on an
//! earlier attempt) ⇒ the resumed session gets the continue note only and the
//! original text is NOT on stdin; UNDELIVERED ⇒ the original goes out, once
//! per attempt, and never a continue note — so neither "always continue" nor
//! "always re-send" passes a pair.
//!
//! The fake (`fixtures/fake-claude-resend`) appends every spawn's stdin to
//! `stdin.log` in the thread's cwd, so the counts below are counts of what
//! reached a process, across attempts and across daemon lives.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use threads::event::{StoredEvent, ThreadEvent};
use threads::runtime::PassOrigin;
use threads::settings::ThreadSettings;
use threads::store::{ParkedTurnRow, RetryRow, RunningTurnRow};
use threads::{build_manager, Config, ThreadManager};

const NOTE: &str = "already delivered";
const RESUMED: &str = "cauldron.threads/delivery_resumed";

fn set_env() {
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude-resend"));
    first_exec::warmed("tests/fixtures/fake-claude");
    std::env::set_var("THREADS_CLAUDE_BIN", fake);
    std::env::set_var("THREADS_HEARTBEAT_MS", "100");
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &Path, accounts: &[&str]) -> Config {
    let mut map = BTreeMap::new();
    for a in accounts {
        let cd = dir.join(a);
        std::fs::create_dir_all(&cd).unwrap();
        map.insert(a.to_string(), cd.to_string_lossy().to_string());
    }
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts: map,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

/// The thread's cwd — where the fake keeps `plan` and `stdin.log`.
fn cwd(dir: &Path) -> PathBuf {
    let c = dir.join("work");
    std::fs::create_dir_all(&c).unwrap();
    c
}

fn settings(cwd: &Path, account: Option<&str>) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    if let Some(a) = account {
        s.account = threads::settings::AccountChoice::Named(a.into());
    }
    s
}

fn plan(cwd: &Path, modes: &[&str]) {
    std::fs::write(cwd.join("plan"), modes.join("\n") + "\n").unwrap();
}

/// Every spawn's stdin, in order: `(resumed, text)`.
fn spawns(cwd: &Path) -> Vec<(bool, String)> {
    let log = std::fs::read_to_string(cwd.join("stdin.log")).unwrap_or_default();
    let mut out: Vec<(bool, String)> = Vec::new();
    for line in log.lines() {
        if let Some(rest) = line.strip_prefix("@@SPAWN ") {
            out.push((rest.contains("resume=1"), String::new()));
        } else if let Some(last) = out.last_mut() {
            last.1.push_str(line);
            last.1.push('\n');
        }
    }
    out
}

/// How many times `token` reached ANY process's stdin.
fn on_stdin(cwd: &Path, token: &str) -> usize {
    spawns(cwd).iter().map(|(_, t)| t.matches(token).count()).sum()
}

async fn eventually(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < end {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

fn kinds(m: &ThreadManager, id: &str) -> Vec<String> {
    m.store
        .list_events(id, 0, 100_000, None)
        .unwrap()
        .iter()
        .map(|e| e.event.kind().to_string())
        .collect()
}

fn custom(m: &ThreadManager, id: &str, event_type: &str) -> Vec<Value> {
    m.store
        .list_events(id, 0, 100_000, Some(&["custom".to_string()]))
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            ThreadEvent::Custom { event_type: t, data } if t == event_type => Some(data),
            _ => None,
        })
        .collect()
}

fn send(m: &ThreadManager, id: &str, text: &str) -> u64 {
    m.send(id, text.to_string(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap()
}

/// Wait until the thread has run `n` spawns and gone idle with nothing owed.
async fn settle(m: &ThreadManager, id: &str, cwd: &Path, n: usize) {
    let ok = eventually(30, || {
        // A retry the pool has not fired yet is fired now: the cadence is the
        // deadline route, so it runs regardless of the backoff's clock.
        if m.store.get_retry(id).unwrap().is_some() {
            m.retry_sweep(PassOrigin::Cadence);
        }
        spawns(cwd).len() >= n
            && m.store.get_retry(id).unwrap().is_none()
            && m.store.get_parked_turn(id).unwrap().is_none()
            && m.is_idle(id).unwrap_or(false)
    })
    .await;
    assert!(
        ok,
        "the thread never settled after {n} spawns: spawns={:?} ledger={:?}",
        spawns(cwd),
        kinds(m, id)
    );
}

fn append(m: &ThreadManager, id: &str, event: ThreadEvent) {
    let seq = m.store.max_seq(id).unwrap() + 1;
    m.store
        .append_event(&StoredEvent {
            thread_id: id.to_string(),
            seq,
            message_id: None,
            ts: threads::store::now_rfc3339(),
            event,
        })
        .unwrap();
}

/// The previous daemon's ledger for a delivery of `msg`: queued, attempted,
/// `turn_started` — and, when `answered`, the model's reply to it.
fn prior_attempt(m: &ThreadManager, id: &str, msg: u64, words: &str, answered: bool) {
    let qid = m
        .store
        .enqueue(id, msg, words, "immediate", &Value::Null, &[])
        .unwrap();
    m.store.mark_delivered(&[qid]).unwrap();
    append(m, id, ThreadEvent::TurnStarted { attempt: 1, message_ids: vec![msg] });
    if answered {
        append(
            m,
            id,
            ThreadEvent::AssistantText {
                text: "on it — reading the feedback now".into(),
                parent_tool_use_id: None,
                api_message_id: None,
            },
        );
    }
}

fn dead_pid() -> i64 {
    let mut c = std::process::Command::new("true").spawn().unwrap();
    c.wait().unwrap();
    c.id() as i64
}

/// Assert the DELIVERED arm on the one re-entry spawn `k` (0-based).
fn assert_continued(cwd: &Path, k: usize, token: &str, msg: u64) {
    let s = spawns(cwd);
    let (resumed, text) = &s[k];
    assert!(*resumed, "the re-entry must RESUME the conversation: {s:?}");
    assert!(
        !text.contains(token),
        "a DELIVERED message was written to stdin again (spawn {k}): {text:?}"
    );
    assert!(
        text.contains(NOTE) && text.contains(&format!("#{msg}")),
        "the resumed session must get the continue note naming #{msg}: {text:?}"
    );
}

/// Assert the UNDELIVERED arm on spawn `k`.
fn assert_resent(cwd: &Path, k: usize, token: &str) {
    let s = spawns(cwd);
    let (_, text) = &s[k];
    assert_eq!(
        text.matches(token).count(),
        1,
        "an UNDELIVERED message must go out exactly once on its re-entry (spawn {k}): {text:?}"
    );
    assert!(!text.contains(NOTE), "no continue note for a message the model never saw: {text:?}");
}

// ── `fire_retry` → `run_turn` (cauldron main runtime.rs:13697) ──────────────

/// DELIVERED: the model answered, then a transient error put the delivery on
/// the ladder; the fired retry resumes with the note only, and the C event
/// records it.
#[tokio::test(flavor = "multi_thread")]
async fn a_fired_retry_of_a_delivered_message_resumes_with_a_continue_note_only() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = cwd(tmp.path());
    plan(&cwd, &["answer_transient", "ok"]);
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let id = m.create_thread(None, settings(&cwd, Some("solo"))).unwrap();
    let token = "ALPHA-fired-retry-delivered";
    let msg = send(&m, &id, token);
    settle(&m, &id, &cwd, 2).await;

    assert_eq!(spawns(&cwd).len(), 2, "one failure, one retry: {:?}", spawns(&cwd));
    assert_continued(&cwd, 1, token, msg);
    assert_eq!(on_stdin(&cwd, token), 1, "the message reached a process exactly ONCE");
    // C: the re-entry is a ledger fact, with every field.
    let ev = custom(&m, &id, RESUMED);
    assert_eq!(ev.len(), 1, "one re-entry, one event: {ev:?}");
    let e = &ev[0];
    assert_eq!(e["attempt"], 2);
    assert_eq!(e["message_ids"], json!([msg]));
    assert_eq!(e["sent"], "continue");
    assert_eq!(e["withheld_ids"], json!([msg]));
    assert_eq!(e["resent_ids"], json!([]));
    assert_eq!(e["delivered_ids"], json!([msg]));
    assert_eq!(e["resumed_session"], true);
    assert_eq!(e["evidence"], "model_output");
    assert_eq!(e["why"]["kind"], "retry_scheduled", "why: the cut that preceded it: {e}");
    assert!(e["write_error"].is_null(), "{e}");
}

/// UNDELIVERED (the opposing arm): the error came before the model said a
/// word, so the retry sends the original — once — and no note.
#[tokio::test(flavor = "multi_thread")]
async fn a_fired_retry_of_an_undelivered_message_sends_it_once() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = cwd(tmp.path());
    plan(&cwd, &["silent_transient", "ok"]);
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let id = m.create_thread(None, settings(&cwd, Some("solo"))).unwrap();
    let token = "ALPHA-fired-retry-undelivered";
    let msg = send(&m, &id, token);
    settle(&m, &id, &cwd, 2).await;

    assert_eq!(spawns(&cwd).len(), 2, "{:?}", spawns(&cwd));
    assert_resent(&cwd, 1, token);
    let ev = custom(&m, &id, RESUMED);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0]["sent"], "prompt");
    assert_eq!(ev[0]["resent_ids"], json!([msg]));
    assert_eq!(ev[0]["withheld_ids"], json!([]));
    assert_eq!(ev[0]["delivered_ids"], json!([]));
    assert!(ev[0]["evidence"].is_null());
}

// ── the in-turn ladder: `run_turn`'s loop, `RunAgain` (ripples attempt 2) ────

/// DELIVERED: the model answered, then the account ran out of credit and
/// placement moved the turn to the second account — the loop re-runs at once
/// (no retry row) and the moved session gets the note only. Two accounts,
/// because a failover is not reachable at N=1.
#[tokio::test(flavor = "multi_thread")]
async fn in_turn_failover_of_a_delivered_message_continues_without_resending() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = cwd(tmp.path());
    plan(&cwd, &["answer_credit", "ok"]);
    // The CLI leaves a transcript, so the failover PORTS the conversation and
    // the moved spawn resumes it (without one it starts fresh — and then the
    // original text is correctly sent again; see the opposing test).
    std::fs::write(cwd.join("write-transcript"), b"").unwrap();
    let m = build_manager(config_at(tmp.path(), &["acct-0", "acct-1"])).unwrap();
    let id = m.create_thread(None, settings(&cwd, None)).unwrap();
    let before = m.serving_account(&id).unwrap();
    let token = "ALPHA-in-turn-delivered";
    let msg = send(&m, &id, token);
    settle(&m, &id, &cwd, 2).await;

    assert_ne!(m.serving_account(&id).unwrap(), before, "non-vacuity: placement moved the turn");
    assert!(
        !kinds(&m, &id).iter().any(|k| k == "retry_scheduled"),
        "non-vacuity: this is the in-turn loop, not the ladder: {:?}",
        kinds(&m, &id)
    );
    assert_continued(&cwd, 1, token, msg);
    assert_eq!(on_stdin(&cwd, token), 1);
    let ev = custom(&m, &id, RESUMED);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0]["sent"], "continue");
}

/// UNDELIVERED arm of the in-turn loop: a credit error before any output
/// moves the turn too, and the original goes out on the new account once.
#[tokio::test(flavor = "multi_thread")]
async fn in_turn_failover_of_an_undelivered_message_sends_it_once() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = cwd(tmp.path());
    // `silent_transient` is not a credit error; write the credit line raw.
    std::fs::write(cwd.join("plan"), "silent_credit\nok\n").unwrap();
    let m = build_manager(config_at(tmp.path(), &["acct-0", "acct-1"])).unwrap();
    let id = m.create_thread(None, settings(&cwd, None)).unwrap();
    let before = m.serving_account(&id).unwrap();
    let token = "ALPHA-in-turn-undelivered";
    let _msg = send(&m, &id, token);
    settle(&m, &id, &cwd, 2).await;

    assert_ne!(m.serving_account(&id).unwrap(), before, "non-vacuity: placement moved the turn");
    assert_resent(&cwd, 1, token);
    assert_eq!(custom(&m, &id, RESUMED)[0]["sent"], "prompt");
}

// ── L-1: restart recovery → `schedule_retry` (cauldron main runtime.rs:14820) ─

/// The previous daemon died mid-turn; at boot `recover` puts the delivery on
/// the ladder and the retry fires. DELIVERED and UNDELIVERED side by side, on
/// two threads of one daemon (N = 2), so one predicate answers both.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_resumes_a_delivered_turn_and_resends_an_undelivered_one() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd_a = tmp.path().join("work-a");
    let cwd_b = tmp.path().join("work-b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    let (a, b) = {
        let m1 = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
        let a = m1.create_thread(None, settings(&cwd_a, Some("solo"))).unwrap();
        let b = m1.create_thread(None, settings(&cwd_b, Some("solo"))).unwrap();
        for (id, words, answered) in [(&a, "ALPHA-restart-delivered", true), (&b, "BRAVO-restart-undelivered", false)] {
            prior_attempt(&m1, id, 7, words, answered);
            m1.store
                .upsert_running_turn(&RunningTurnRow {
                    thread_id: id.clone(),
                    pid: dead_pid(),
                    spawn_cookie: "cookie".into(),
                    output_path: tmp.path().join(format!("{id}.jsonl")).to_string_lossy().to_string(),
                    byte_offset: 0,
                    started_at: threads::store::now_rfc3339(),
                    prompt: Some(words.to_string()),
                    message_ids: vec![7],
                    attempt: 1,
                })
                .unwrap();
        }
        (a, b)
    };
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    m.recover().await;
    assert!(m.store.get_retry(&a).unwrap().is_some(), "non-vacuity: L-1 put A on the ladder");
    settle(&m, &a, &cwd_a, 1).await;
    settle(&m, &b, &cwd_b, 1).await;

    assert_continued(&cwd_a, 0, "ALPHA-restart-delivered", 7);
    assert_resent(&cwd_b, 0, "BRAVO-restart-undelivered");
    let ea = custom(&m, &a, RESUMED);
    assert_eq!(ea.len(), 1, "{ea:?}");
    assert_eq!(ea[0]["sent"], "continue");
    assert_eq!(ea[0]["why"]["kind"], "retry_scheduled");
    assert!(
        ea[0]["why"]["reason"].as_str().unwrap_or("").contains("restarted"),
        "why names the restart: {}",
        ea[0]
    );
    assert_eq!(custom(&m, &b, RESUMED)[0]["sent"], "prompt");
}

// ── parked resume (cauldron main runtime.rs:5558) ───────────────────────────

/// A delivery parked by the previous daemon goes back out through
/// `maybe_dispatch`'s parked branch. N = 2 again: A was answered before it
/// was parked, B was parked before it ran.
#[tokio::test(flavor = "multi_thread")]
async fn a_parked_turn_resumes_with_a_note_if_delivered_and_sends_once_if_not() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd_a = tmp.path().join("work-a");
    let cwd_b = tmp.path().join("work-b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    let (a, b) = {
        let m1 = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
        let a = m1.create_thread(None, settings(&cwd_a, Some("solo"))).unwrap();
        let b = m1.create_thread(None, settings(&cwd_b, Some("solo"))).unwrap();
        for (id, words, answered) in [(&a, "ALPHA-parked-delivered", true), (&b, "BRAVO-parked-undelivered", false)] {
            prior_attempt(&m1, id, 3, words, answered);
            m1.store
                .upsert_parked_turn(&ParkedTurnRow {
                    thread_id: id.clone(),
                    prompt: words.to_string(),
                    message_ids: vec![3],
                    attempt: 2,
                    reason: "usage limit (test)".into(),
                    budget_key: None,
                    parked_at: threads::store::now_rfc3339(),
                })
                .unwrap();
        }
        (a, b)
    };
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    m.nudge_thread(&a);
    m.nudge_thread(&b);
    settle(&m, &a, &cwd_a, 1).await;
    settle(&m, &b, &cwd_b, 1).await;

    // Non-vacuity: the queue rows are `delivered`, so nothing but the parked
    // branch could have spawned these turns, and it consumed its row.
    assert!(m.store.get_parked_turn(&a).unwrap().is_none() && m.store.get_parked_turn(&b).unwrap().is_none());
    assert_continued(&cwd_a, 0, "ALPHA-parked-delivered", 3);
    assert_resent(&cwd_b, 0, "BRAVO-parked-undelivered");
    let ea = custom(&m, &a, RESUMED);
    assert_eq!(ea.len(), 1, "{ea:?}");
    assert_eq!(ea[0]["sent"], "continue");
    assert_eq!(ea[0]["attempt"], 2, "the parked attempt number is carried: {}", ea[0]);
    assert_eq!(custom(&m, &b, RESUMED)[0]["sent"], "prompt");
}

// ── B: the projected thread's per-attempt re-append (ledger/turn.rs) ─────────

fn user_lines_for(dir: &Path, id: &str, msg: u64) -> usize {
    let l = threads::ledger::Ledger::read_jsonl(&threads::ledger::turn::ledger_path(dir, id)).unwrap();
    l.events()
        .iter()
        .filter(|e| {
            e.kind == threads::ledger::Kind::User
                && e.body.pointer("/metadata/daemon_message_ids") == Some(&json!([msg]))
        })
        .count()
}

/// A projected thread's retry: ONE `user` line for the batch on the ledger
/// (so one copy in every later render), and the stdin of the retry is the
/// note — the render already carries the message.
#[tokio::test(flavor = "multi_thread")]
async fn a_projected_retry_keeps_one_prompt_line_and_sends_only_the_note() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = cwd(tmp.path());
    plan(&cwd, &["answer_transient", "ok"]);
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let mut s = settings(&cwd, Some("solo"));
    s.projection = threads::settings::Projection::Projected;
    let id = m.create_thread(None, s).unwrap();
    let token = "ALPHA-projected";
    let msg = send(&m, &id, token);
    settle(&m, &id, &cwd, 2).await;

    assert_eq!(spawns(&cwd).len(), 2, "{:?}", spawns(&cwd));
    assert_eq!(
        user_lines_for(tmp.path(), &id, msg),
        1,
        "the delivery's prompt is on the ledger ONCE, not once per attempt"
    );
    assert_continued(&cwd, 1, token, msg);
    let ev = custom(&m, &id, RESUMED);
    assert_eq!(ev[0]["sent"], "continue");
}

// ── the full story: resume after a kill, across two daemon lives ────────────

/// **RESUME AFTER KILL.** Message A is delivered and answered; the daemon
/// dies (its runtime is torn down) and the claude process is killed; message
/// B was queued behind A and never ran. The next daemon recovers: A resumes
/// with the note, B goes out with its own text. Across BOTH lives, each
/// message's text reaches a process exactly ONCE — a count, not a presence.
#[test]
fn resume_after_kill_each_message_reaches_stdin_exactly_once_across_both_lives() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = cwd(tmp.path());
    plan(&cwd, &["answer_hold", "ok", "ok"]);
    let (alpha, bravo) = ("ALPHA-kill-delivered", "BRAVO-kill-queued");

    // ── life 1 ──
    let rt1 = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let (id, a_msg, pid, boot_locks) = rt1.block_on(async {
        let m1 = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
        let id = m1.create_thread(None, settings(&cwd, Some("solo"))).unwrap();
        let a_msg = send(&m1, &id, alpha);
        assert!(
            eventually(20, || kinds(&m1, &id).iter().any(|k| k == "assistant_text")).await,
            "life 1: the model answered A: {:?}",
            kinds(&m1, &id)
        );
        send(&m1, &id, bravo);
        let pid = m1.store.get_running_turn(&id).unwrap().expect("A is running").pid;
        let locks = m1.store.list_thread_locks().unwrap();
        (id, a_msg, pid, locks)
    });
    // The daemon dies: every task it had stops here.
    rt1.shutdown_timeout(Duration::from_secs(2));
    // …and the claude process is killed (its whole group).
    let _ = std::process::Command::new("kill").args(["-9", &format!("-{pid}")]).status();
    let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
    // The dead daemon's lock names THIS test process's pid, which is alive; a
    // real dead daemon's pid would not be. Release it as its death would.
    {
        let m = build_manager(config_at(tmp.path(), &["solo"])).ok();
        if let Some(m) = m {
            for l in &boot_locks {
                let _ = m.store.release_thread_lock(&l.thread_id, &l.boot_id);
            }
        }
    }
    assert_eq!(on_stdin(&cwd, alpha), 1, "life 1 sent A once");
    assert_eq!(on_stdin(&cwd, bravo), 0, "life 1 never ran B");

    // ── life 2 ──
    let rt2 = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt2.block_on(async {
        let m2 = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
        m2.recover().await;
        settle(&m2, &id, &cwd, 3).await;
        let s = spawns(&cwd);
        assert_eq!(s.len(), 3, "A, A resumed, B: {s:?}");
        assert_continued(&cwd, 1, alpha, a_msg);
        assert!(s[2].1.contains(bravo) && !s[2].1.contains(alpha), "B goes out alone: {s:?}");
        assert_eq!(on_stdin(&cwd, alpha), 1, "A reached a process exactly ONCE across both lives");
        assert_eq!(on_stdin(&cwd, bravo), 1, "B reached a process exactly ONCE across both lives");
        let ev = custom(&m2, &id, RESUMED);
        assert_eq!(ev.len(), 1, "one re-entry (A's), and B's first send is not one: {ev:?}");
        assert_eq!(ev[0]["message_ids"], json!([a_msg]));
        assert_eq!(ev[0]["sent"], "continue");
    });
    rt2.shutdown_timeout(Duration::from_secs(2));
}

// ── R10.M2-1: cancelling a retried delivery ─────────────────────────────────

/// A cancel lands on a delivery that is on the ladder. DELIVERED: the model
/// already had the message, so it does NOT ride the next turn and no
/// `delivery_cancelled` claims it was never delivered. UNDELIVERED (thread B):
/// it rides, as R10.M2 specifies.
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_delivered_retry_does_not_ride_it_but_an_undelivered_one_rides() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = cwd(tmp.path());
    let mut ids = Vec::new();
    // Written by the previous daemon, then read by a fresh one: rows appended
    // straight to the store behind a LIVE manager would collide with its
    // in-memory seq counter.
    let m1 = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    for (words, answered) in [("ALPHA-cancel-delivered", true), ("BRAVO-cancel-undelivered", false)] {
        let id = m1.create_thread(None, settings(&cwd, Some("solo"))).unwrap();
        prior_attempt(&m1, &id, 5, words, answered);
        m1.store
            .upsert_retry(&RetryRow {
                thread_id: id.clone(),
                prompt: words.to_string(),
                message_ids: vec![5],
                attempt: 2,
                kind: "transient".into(),
                reason: "test".into(),
                account: Some("solo".into()),
                created_at: threads::store::now_rfc3339(),
            })
            .unwrap();
        ids.push(id);
    }
    drop(m1);
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    for id in &ids {
        m.cancel_turn(id).unwrap();
    }
    let cancelled = |id: &str| custom(&m, id, threads::event::delivery::CANCELLED);
    // A: delivered — still owed nothing, not ridden.
    assert_eq!(m.store.rideable(&ids[0], &[5]).unwrap(), vec![5], "a delivered message is not put back on the queue");
    assert!(cancelled(&ids[0]).is_empty(), "no false delivery_cancelled: {:?}", cancelled(&ids[0]));
    // B: undelivered — it rides (R10.M2): the attempt ends
    // `delivery_cancelled{dispatch_now: ride}` naming it. (Its row may already
    // have gone out again on the ride by the time this reads the queue, so the
    // event is the assertion.)
    let cb = cancelled(&ids[1]);
    assert_eq!(cb.len(), 1, "{cb:?}");
    assert_eq!(cb[0]["message_ids"], json!([5]));
    assert_eq!(cb[0]["dispatch_now"], "ride");
    assert!(m.store.get_retry(&ids[0]).unwrap().is_none() && m.store.get_retry(&ids[1]).unwrap().is_none());
}

// ── R13.2: an AMBIGUOUS delivery (stdin written, no output, process gone) ───

const AMBIGUOUS_NOTE: &str = "If you cannot see message";

fn kill_group(pid: i64) {
    let _ = std::process::Command::new("kill").args(["-9", &format!("-{pid}")]).status();
    let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
}

/// **KILL-BEFORE-OUTPUT, THREE TIMES, ACROSS FOUR DAEMON LIVES.** Each spawn
/// reads its stdin, prints only `init` (no model output), and then the daemon
/// dies and the claude process group is `kill -9`ed. The next life's
/// `recover()` puts the delivery back on the ladder (L-1) and the retry fires.
/// That is the ambiguous window the R13.1 inspection found: under R13.1 every
/// retry re-sent the original, once per retry, up to the ladder's ceiling.
/// Now it is the first send plus ONE re-send, then the ambiguous note. So
/// across four spawns the text reaches stdin at most twice, and the later
/// spawns ask the model to say so if it does not have the message.
///
/// (A child killed while its daemon LIVES is not used here: with no exit code
/// and no stall it lands on the clean verdict and ends the delivery — see the
/// R13.2 report, finding 1.)
#[test]
fn kill_before_output_across_three_retries_puts_the_text_on_stdin_at_most_twice() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = cwd(tmp.path());
    plan(&cwd, &["silent_hold", "silent_hold", "silent_hold", "ok"]);
    let token = "ALPHA-kill-before-output";
    let mut thread: Option<(String, u64)> = None;
    for life in 1..=3usize {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let (id, msg, pid, locks) = rt.block_on(async {
            let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
            let (id, msg) = match &thread {
                None => {
                    let id = m.create_thread(None, settings(&cwd, Some("solo"))).unwrap();
                    let msg = send(&m, &id, token);
                    (id, msg)
                }
                Some((id, msg)) => {
                    m.recover().await;
                    (id.clone(), *msg)
                }
            };
            let mut pid = None;
            let ok = eventually(30, || {
                if m.store.get_retry(&id).unwrap().is_some() {
                    m.retry_sweep(PassOrigin::Cadence);
                }
                // The child read its stdin and came up, AND the daemon has
                // recorded that (`system_init`): only then is it killed. A
                // child killed before the daemon read its `init` leaves no
                // evidence it ever got the text, and is re-sent (safe side).
                let inits = kinds(&m, &id).iter().filter(|k| k.as_str() == "system_init").count();
                if spawns(&cwd).len() >= life && inits >= life {
                    pid = m.store.get_running_turn(&id).unwrap().map(|r| r.pid);
                }
                pid.is_some()
            })
            .await;
            assert!(ok, "life {life}: spawn {life} never ran: {:?} {:?}", spawns(&cwd), kinds(&m, &id));
            (id, msg, pid.unwrap(), m.store.list_thread_locks().unwrap())
        });
        rt.shutdown_timeout(Duration::from_secs(2));
        kill_group(pid);
        // The dead daemon's lock names THIS (live) test process; release it as
        // the daemon's death would.
        if let Ok(m) = build_manager(config_at(tmp.path(), &["solo"])) {
            for l in &locks {
                let _ = m.store.release_thread_lock(&l.thread_id, &l.boot_id);
            }
        }
        thread = Some((id, msg));
    }
    let (id, msg) = thread.unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
        m.recover().await;
        settle(&m, &id, &cwd, 4).await;

        let s = spawns(&cwd);
        assert_eq!(s.len(), 4, "three killed spawns and the one that answered: {s:?}");
        assert_eq!(
            on_stdin(&cwd, token),
            2,
            "an ambiguous delivery goes in at most TWICE (send + one re-send), not once per retry: {s:?}"
        );
        for k in [2usize, 3] {
            assert!(s[k].0, "spawn {k} resumes: {s:?}");
            assert!(
                !s[k].1.contains(token) && s[k].1.contains(AMBIGUOUS_NOTE) && s[k].1.contains(&format!("#{msg}")),
                "spawn {k} gets the ambiguous note, not the text: {:?}",
                s[k].1
            );
        }
        // C: the ambiguity is a ledger fact.
        let ev = custom(&m, &id, RESUMED);
        assert_eq!(ev.len(), 3, "three re-entries: {ev:?}");
        assert_eq!(ev[0]["sent"], "prompt");
        assert_eq!(ev[0]["ambiguous"], false);
        assert_eq!(ev[0]["prompt_writes"], 1);
        for e in &ev[1..] {
            assert_eq!(e["sent"], "continue", "{e}");
            assert_eq!(e["ambiguous"], true, "{e}");
            assert_eq!(e["evidence"], "stdin_written_no_output", "{e}");
            assert_eq!(e["withheld_ids"], json!([msg]), "{e}");
            assert_eq!(e["delivered_ids"], json!([]), "not DELIVERED, only written: {e}");
        }
        assert_eq!(ev[1]["prompt_writes"], 2);
    });
    rt.shutdown_timeout(Duration::from_secs(2));
}

/// **THE OPPOSING ARM, both sides on one daemon (N = 2).** The previous daemon
/// left two deliveries cut off by restarts, each after THREE attempts:
/// * A — every attempt died BEFORE its stdin write (no child came up: no
///   `system_init`):
///   nothing ever reached a process, so the text goes out, exactly once;
/// * B — two attempts DID write it (their children came up: `system_init`),
///   none answered: it
///   is ambiguous and already re-sent once, so the resumed session gets the
///   ambiguous note, and the text does not reach stdin again.
#[tokio::test(flavor = "multi_thread")]
async fn kill_before_stdin_sends_once_and_twice_written_sends_the_note() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd_a = tmp.path().join("work-a");
    let cwd_b = tmp.path().join("work-b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    let (a, b) = {
        let m1 = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
        let a = m1.create_thread(None, settings(&cwd_a, Some("solo"))).unwrap();
        let b = m1.create_thread(None, settings(&cwd_b, Some("solo"))).unwrap();
        for (id, words, written) in [(&a, "ALPHA-before-stdin", 0u32), (&b, "BRAVO-twice-written", 2)] {
            prior_attempt(&m1, id, 9, words, false);
            if written >= 1 {
                append(&m1, id, ThreadEvent::SystemInit { claude_session_id: None, cwd: None, model: None, tools: vec![], mcp_servers: Value::Null, permission_mode: None, slash_commands: vec![], api_key_source: None, agents: Value::Null });
            }
            for att in 2..=3u32 {
                append(&m1, id, ThreadEvent::TurnStarted { attempt: att, message_ids: vec![9] });
                if written >= att {
                    append(&m1, id, ThreadEvent::SystemInit { claude_session_id: None, cwd: None, model: None, tools: vec![], mcp_servers: Value::Null, permission_mode: None, slash_commands: vec![], api_key_source: None, agents: Value::Null });
                }
            }
            m1.store
                .upsert_running_turn(&RunningTurnRow {
                    thread_id: id.clone(),
                    pid: dead_pid(),
                    spawn_cookie: "cookie".into(),
                    output_path: tmp.path().join(format!("{id}.jsonl")).to_string_lossy().to_string(),
                    byte_offset: 0,
                    started_at: threads::store::now_rfc3339(),
                    prompt: Some(words.to_string()),
                    message_ids: vec![9],
                    attempt: 3,
                })
                .unwrap();
        }
        (a, b)
    };
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    m.recover().await;
    settle(&m, &a, &cwd_a, 1).await;
    settle(&m, &b, &cwd_b, 1).await;

    assert_eq!(on_stdin(&cwd_a, "ALPHA-before-stdin"), 1, "never written before ⇒ sent exactly once");
    assert_resent(&cwd_a, 0, "ALPHA-before-stdin");
    let ea = custom(&m, &a, RESUMED);
    assert_eq!((ea[0]["sent"].clone(), ea[0]["ambiguous"].clone(), ea[0]["prompt_writes"].clone()), (json!("prompt"), json!(false), json!(0)), "{ea:?}");

    assert_eq!(on_stdin(&cwd_b, "BRAVO-twice-written"), 0, "written twice ⇒ not a third time");
    let sb = spawns(&cwd_b);
    assert!(sb[0].0 && sb[0].1.contains(AMBIGUOUS_NOTE) && sb[0].1.contains("#9"), "{sb:?}");
    let eb = custom(&m, &b, RESUMED);
    assert_eq!(eb[0]["sent"], "continue", "{eb:?}");
    assert_eq!(eb[0]["ambiguous"], true);
    assert_eq!(eb[0]["prompt_writes"], 2);
    assert_eq!(eb[0]["evidence"], "stdin_written_no_output");
}

// ── R13.3: a child killed while its daemon is UP never completes silently ───

/// Wait until spawn `k` has come up AND the daemon has recorded its `init`,
/// then `kill -9` its process group by the pid THIS test read off the
/// `running_turns` row. The daemon stays up throughout.
async fn kill_spawn_in_process(m: &ThreadManager, id: &str, cwd: &Path, k: usize) {
    let mut pid = None;
    let ok = eventually(30, || {
        if m.store.get_retry(id).unwrap().is_some() {
            m.retry_sweep(PassOrigin::Cadence);
        }
        let inits = kinds(m, id).iter().filter(|x| x.as_str() == "system_init").count();
        if spawns(cwd).len() >= k && inits >= k {
            pid = m.store.get_running_turn(id).unwrap().map(|r| r.pid);
        }
        pid.is_some()
    })
    .await;
    assert!(ok, "spawn {k} never came up: {:?} {:?}", spawns(cwd), kinds(m, id));
    kill_group(pid.unwrap());
    assert!(
        eventually(30, || m.store.get_running_turn(id).unwrap().is_none()).await,
        "the daemon never saw spawn {k} die"
    );
}

/// **IN-PROCESS KILL BEFORE OUTPUT ⇒ RE-DELIVERED, NEVER SILENTLY DONE.** Two
/// threads on one live daemon (N = 2):
/// * A — its child reads the text, comes up, and is `kill -9`ed before any
///   output. Before R13.3 that was the CLEAN verdict: the delivery ended, no
///   retry, no `turn_failed`. Now it lands on the ladder and the text goes
///   out again (one re-send: the R13.2 cap allows two writes), and the turn
///   then finishes.
/// * B — the opposing arm: a normal turn with output and a `result`. Done:
///   one spawn, no retry, no re-entry.
#[tokio::test(flavor = "multi_thread")]
async fn an_in_process_kill_before_output_is_redelivered_and_a_normal_turn_is_done() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd_a = tmp.path().join("work-a");
    let cwd_b = tmp.path().join("work-b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    plan(&cwd_a, &["silent_hold", "ok"]);
    plan(&cwd_b, &["ok"]);
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let a = m.create_thread(None, settings(&cwd_a, Some("solo"))).unwrap();
    let b = m.create_thread(None, settings(&cwd_b, Some("solo"))).unwrap();
    let (ta, tb) = ("ALPHA-inproc-kill", "BRAVO-normal-turn");
    send(&m, &a, ta);
    send(&m, &b, tb);
    kill_spawn_in_process(&m, &a, &cwd_a, 1).await;
    settle(&m, &a, &cwd_a, 2).await;
    settle(&m, &b, &cwd_b, 1).await;

    let ka = kinds(&m, &a);
    assert!(
        ka.iter().any(|k| k == "retry_scheduled"),
        "the killed turn went on the ladder, not to a clean verdict: {ka:?}"
    );
    let sa = spawns(&cwd_a);
    assert_eq!(sa.len(), 2, "the message was re-delivered: {sa:?}");
    assert_eq!(on_stdin(&cwd_a, ta), 2, "first send + one re-send: {sa:?}");
    assert_eq!(custom(&m, &a, RESUMED)[0]["sent"], "prompt");

    let kb = kinds(&m, &b);
    assert_eq!(spawns(&cwd_b).len(), 1, "a normal turn is done after one spawn");
    assert_eq!(on_stdin(&cwd_b, tb), 1);
    assert!(!kb.iter().any(|k| k == "retry_scheduled" || k == "turn_failed"), "{kb:?}");
    assert!(custom(&m, &b, RESUMED).is_empty(), "a normal turn is not re-entered");
}

/// **THE R13.2 CAP HOLDS ON THIS PATH.** Three in-process kills before output
/// on a live daemon: the text is written twice (send + one re-send), then
/// every later resumed spawn gets the ambiguous note.
#[tokio::test(flavor = "multi_thread")]
async fn in_process_kills_before_output_keep_the_two_write_cap() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = cwd(tmp.path());
    plan(&cwd, &["silent_hold", "silent_hold", "silent_hold", "ok"]);
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let id = m.create_thread(None, settings(&cwd, Some("solo"))).unwrap();
    let token = "ALPHA-inproc-cap";
    let msg = send(&m, &id, token);
    for k in 1..=3 {
        kill_spawn_in_process(&m, &id, &cwd, k).await;
    }
    settle(&m, &id, &cwd, 4).await;

    let s = spawns(&cwd);
    assert_eq!(s.len(), 4, "{s:?}");
    assert_eq!(on_stdin(&cwd, token), 2, "at most two writes, then the note: {s:?}");
    for k in [2usize, 3] {
        assert!(
            s[k].0 && !s[k].1.contains(token) && s[k].1.contains(AMBIGUOUS_NOTE) && s[k].1.contains(&format!("#{msg}")),
            "spawn {k}: {:?}",
            s[k].1
        );
    }
    let ev = custom(&m, &id, RESUMED);
    assert_eq!(ev.len(), 3, "{ev:?}");
    assert_eq!(ev[2]["ambiguous"], true);
    assert!(!kinds(&m, &id).iter().any(|k| k == "turn_failed"), "below the ceiling nothing fails");
}

/// **AT THE CEILING IT FAILS LOUDLY.** The unclassified mid-execution class
/// has a fixed ceiling (`retry::DEFAULT_MAX_TRANSIENT_ATTEMPTS`). A child
/// killed before output on EVERY attempt up to it ends the delivery with a
/// visible `turn_failed` and the text kept, re-selectable. It never ends
/// silently and never spawns past the ceiling.
#[tokio::test(flavor = "multi_thread")]
async fn in_process_kills_to_the_ceiling_fail_loudly_and_keep_the_text() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = cwd(tmp.path());
    let n = threads::retry::DEFAULT_MAX_TRANSIENT_ATTEMPTS as usize;
    let modes: Vec<&str> = std::iter::repeat("silent_hold").take(n).chain(["ok"]).collect();
    plan(&cwd, &modes);
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let id = m.create_thread(None, settings(&cwd, Some("solo"))).unwrap();
    let token = "ALPHA-inproc-ceiling";
    send(&m, &id, token);
    for k in 1..=n {
        kill_spawn_in_process(&m, &id, &cwd, k).await;
    }
    assert!(
        eventually(30, || kinds(&m, &id).iter().any(|k| k == "turn_failed")).await,
        "the exhausted delivery fails VISIBLY: {:?}",
        kinds(&m, &id)
    );
    let failed = m.store.failed_deliveries(&id).unwrap();
    assert_eq!(failed.len(), 1, "the text is kept, re-selectable");
    assert!(failed[0].text.contains(token));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(spawns(&cwd).len(), n, "no spawn past the ceiling");
    assert_eq!(on_stdin(&cwd, token), 2, "and the two-write cap held all the way up");
}

// ── R13.4: the LAST word decides, not the first result ───────────────────────

/// Wait until thread `id`'s ledger holds at least `n` events of `kind` AND a
/// running child, then `kill -9` its process group by the pid THIS test read
/// off that thread's `running_turns` row. The daemon stays up throughout.
async fn kill_after(m: &ThreadManager, id: &str, kind: &str, n: usize) {
    let mut pid = None;
    let ok = eventually(30, || {
        let seen = kinds(m, id).iter().filter(|x| x.as_str() == kind).count();
        if seen >= n {
            pid = m.store.get_running_turn(id).unwrap().map(|r| r.pid);
        }
        pid.is_some()
    })
    .await;
    assert!(ok, "never saw {n}× {kind} with a running child: {:?}", kinds(m, id));
    kill_group(pid.unwrap());
    assert!(
        eventually(30, || m.store.get_running_turn(id).unwrap().is_none()).await,
        "the daemon never saw the child die"
    );
}

/// Wait until the killed delivery has LANDED — `turn_ended` written, nothing
/// running — then force the pool's deadline route once more, so a retry the
/// verdict scheduled would have FIRED (its spawn is what the negative arm
/// counts), and wait out one more tail-poll period.
async fn landed(m: &ThreadManager, id: &str) {
    assert!(
        eventually(30, || {
            kinds(m, id).iter().any(|k| k == "turn_ended")
                && m.store.get_running_turn(id).unwrap().is_none()
                && m.is_idle(id).unwrap_or(false)
        })
        .await,
        "the killed turn never landed: {:?}",
        kinds(m, id)
    );
    m.retry_sweep(PassOrigin::Cadence);
    tokio::time::sleep(Duration::from_millis(500)).await;
}

fn assert_done_after_one_spawn(m: &ThreadManager, id: &str, cwd: &Path, token: &str) {
    let k = kinds(m, id);
    assert!(
        !k.iter().any(|x| x == "retry_scheduled" || x == "turn_failed"),
        "a turn whose LAST word was a clean result is DONE, whatever trails it: {k:?}"
    );
    assert!(m.store.get_retry(id).unwrap().is_none(), "no ladder row");
    assert_eq!(spawns(cwd).len(), 1, "no re-entry: {:?}", spawns(cwd));
    assert_eq!(on_stdin(cwd, token), 1);
    assert!(custom(m, id, RESUMED).is_empty(), "a finished turn is not re-entered");
}

/// **G1 — A KILL AFTER AN EARLIER, NOTIFICATION-ONLY RESULT IS LADDERED.** The
/// DEPLOY-9l attempt-2 shape, two threads on one live daemon (N = 2):
/// * A — on `--resume` the CLI first runs a queued `<task-notification>` as
///   its own mini-turn (`init`, clean `result`), THEN opens the operator's
///   turn (second `init`, a `tool_use`) and is `kill -9`ed mid-work. Before
///   R13.4 the first result latched the turn "done": `turn_ended` and
///   nothing else, the message silently lost. Now it goes on the ladder, and
///   because the model had started on it the retry RESUMES with the continue
///   note — the operator's text reaches stdin exactly once.
/// * B — the opposing arm: a clean `result` that IS the last word, followed
///   only by the background-task lines a finished turn streams, then killed.
///   Done: one spawn, no retry.
#[tokio::test(flavor = "multi_thread")]
async fn a_kill_after_an_earlier_notification_result_is_laddered_and_a_last_word_result_is_done() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd_a = tmp.path().join("work-a");
    let cwd_b = tmp.path().join("work-b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    plan(&cwd_a, &["notify_then_work_hold", "ok"]);
    plan(&cwd_b, &["answer_trailing_hold"]);
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let a = m.create_thread(None, settings(&cwd_a, Some("solo"))).unwrap();
    let b = m.create_thread(None, settings(&cwd_b, Some("solo"))).unwrap();
    let (ta, tb) = ("ALPHA-N9L-41cb2944", "BRAVO-last-word");
    let msg_a = send(&m, &a, ta);
    send(&m, &b, tb);
    kill_after(&m, &a, "tool_use", 1).await;
    kill_after(&m, &b, "task_notification", 1).await;
    settle(&m, &a, &cwd_a, 2).await;
    landed(&m, &b).await;

    let ka = kinds(&m, &a);
    assert_eq!(
        ka.iter().filter(|k| k.as_str() == "result_turn").count(),
        2,
        "the notification's result and the retried turn's: {ka:?}"
    );
    assert!(
        ka.iter().any(|k| k == "retry_scheduled"),
        "a death after an EARLIER turn's result is not this delivery's end: {ka:?}"
    );
    assert_eq!(spawns(&cwd_a).len(), 2, "{:?}", spawns(&cwd_a));
    assert_eq!(on_stdin(&cwd_a, ta), 1, "the operator's text went in ONCE: {:?}", spawns(&cwd_a));
    assert_continued(&cwd_a, 1, ta, msg_a);
    assert_eq!(custom(&m, &a, RESUMED)[0]["sent"], "continue");
    assert!(!ka.iter().any(|k| k == "turn_failed"), "{ka:?}");

    assert_done_after_one_spawn(&m, &b, &cwd_b, tb);
}

/// **G2 — A GENUINELY CLEAN LAST WORD IS NEVER RE-RUN** (the opposing guard,
/// N = 2 on one daemon). Both children deliver a clean `result` and then
/// stream only the background-task lines a finished CLI turn writes after it
/// (DEPLOY-9l attempt 1, seq 83→86):
/// * C is then `kill -9`ed (no exit code);
/// * D is released and exits 0 with nothing more.
/// Neither may retry: "any event after the result re-opens the turn" would
/// re-run both finished, paid turns, and that is what this guard rules out.
#[tokio::test(flavor = "multi_thread")]
async fn a_last_word_result_followed_only_by_task_lines_is_done_killed_or_exited() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd_c = tmp.path().join("work-c");
    let cwd_d = tmp.path().join("work-d");
    std::fs::create_dir_all(&cwd_c).unwrap();
    std::fs::create_dir_all(&cwd_d).unwrap();
    plan(&cwd_c, &["answer_trailing_hold"]);
    plan(&cwd_d, &["answer_trailing_hold"]);
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let c = m.create_thread(None, settings(&cwd_c, Some("solo"))).unwrap();
    let d = m.create_thread(None, settings(&cwd_d, Some("solo"))).unwrap();
    let (tc, td) = ("CHARLIE-killed-after-last-word", "DELTA-exit0-after-last-word");
    send(&m, &c, tc);
    send(&m, &d, td);
    kill_after(&m, &c, "task_notification", 1).await;
    assert!(
        eventually(30, || kinds(&m, &d).iter().any(|k| k == "task_notification")).await,
        "{:?}",
        kinds(&m, &d)
    );
    std::fs::write(cwd_d.join("release"), b"").unwrap();
    landed(&m, &c).await;
    landed(&m, &d).await;

    assert_done_after_one_spawn(&m, &c, &cwd_c, tc);
    assert_done_after_one_spawn(&m, &d, &cwd_d, td);
}

// ── R13.5: the adopt path judges on the last word; a mini-turn's output is ──
// ── not this delivery's evidence ─────────────────────────────────────────────

/// **G2 — A RE-ADOPTED PROCESS WHOSE PREFIX CARRIES AN EARLIER RESULT AND THEN
/// MORE WORK IS NOT READ AS DONE** (+ G4's adopt arm), N = 2 threads, two
/// daemon lives.
///
/// Life 1 runs both threads until their streams are written, then the daemon
/// dies WITHOUT killing the children (a restart). Life 2's `recover()`
/// re-adopts both live children; `adopt_turn` re-reads the prefix life 1
/// already consumed (`seed_verdict_from_prefix`), and the test then
/// `kill -9`s each child by the pid it read off THAT thread's
/// `running_turns` row in life 2.
/// * A — `notify_then_work_hold`: the prefix is a notification mini-turn
///   (`init`, clean `result`), then the operator's turn (`init`, `tool_use`).
///   Before R13.5 the sticky first result made the adopted verdict "done"
///   (`adopted_verdict` → `None`): the message silently lost across a
///   restart. Now it is laddered and, the model having started on it, the
///   retry resumes with the continue note — the text never goes in again.
/// * B — `answer_trailing_hold` (G4, opposing): the prefix ends on a clean
///   `result` followed only by background-task lines. Done after one spawn.
#[test]
fn an_adopted_child_whose_prefix_has_an_earlier_result_then_work_is_laddered_and_a_last_word_result_is_done() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd_a = tmp.path().join("work-a");
    let cwd_b = tmp.path().join("work-b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    plan(&cwd_a, &["notify_then_work_hold", "ok"]);
    plan(&cwd_b, &["answer_trailing_hold"]);
    let (ta, tb) = ("ALPHA-adopt-earlier-result", "BRAVO-adopt-last-word");

    // ── life 1 ──
    let rt1 = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let (a, b, msg_a, pids, boot_locks) = rt1.block_on(async {
        let m1 = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
        let a = m1.create_thread(None, settings(&cwd_a, Some("solo"))).unwrap();
        let b = m1.create_thread(None, settings(&cwd_b, Some("solo"))).unwrap();
        let msg_a = send(&m1, &a, ta);
        send(&m1, &b, tb);
        let mut pids = Vec::new();
        for (id, kind) in [(&a, "tool_use"), (&b, "task_notification")] {
            assert!(
                eventually(30, || kinds(&m1, id).iter().any(|k| k == kind)).await,
                "life 1 never streamed {kind}: {:?}",
                kinds(&m1, id)
            );
            let row = m1.store.get_running_turn(id).unwrap().expect("running in life 1");
            // Non-vacuity: what life 2 adopts is a PREFIX that already holds
            // the result (the offset life 1 persisted covers the whole stream).
            let len = std::fs::metadata(&row.output_path).unwrap().len();
            assert!(
                eventually(10, || m1.store.get_running_turn(id).unwrap().is_some_and(|r| r.byte_offset == len)).await,
                "life 1 persisted the offset past the whole stream"
            );
            pids.push(row.pid);
        }
        let locks = m1.store.list_thread_locks().unwrap();
        (a, b, msg_a, pids, locks)
    });
    // The daemon dies; the children do NOT (a restart, not a kill).
    rt1.shutdown_timeout(Duration::from_secs(2));
    {
        let m = build_manager(config_at(tmp.path(), &["solo"])).ok();
        if let Some(m) = m {
            for l in &boot_locks {
                let _ = m.store.release_thread_lock(&l.thread_id, &l.boot_id);
            }
        }
    }

    // ── life 2 ──
    let rt2 = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt2.block_on(async {
        let m2 = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
        m2.recover().await;
        for (id, pid1) in [(&a, pids[0]), (&b, pids[1])] {
            // Non-vacuity: ADOPTED, not interrupted — the same live child.
            let row = m2.store.get_running_turn(id).unwrap().expect("re-adopted, not cleared");
            assert_eq!(row.pid, pid1, "life 2 adopted life 1's child");
            assert!(
                !kinds(&m2, id).iter().any(|k| k == "turn_interrupted"),
                "an adopted child is not a restart interruption: {:?}",
                kinds(&m2, id)
            );
            kill_group(row.pid);
        }
        for id in [&a, &b] {
            assert!(
                eventually(30, || m2.store.get_running_turn(id).unwrap().is_none()).await,
                "life 2 never saw the adopted child die"
            );
        }
        settle(&m2, &a, &cwd_a, 2).await;
        landed(&m2, &b).await;

        let ka = kinds(&m2, &a);
        assert!(
            ka.iter().any(|k| k == "retry_scheduled"),
            "an adopted death after an EARLIER turn's result is not this delivery's end: {ka:?}"
        );
        assert_eq!(spawns(&cwd_a).len(), 2, "{:?}", spawns(&cwd_a));
        assert_eq!(on_stdin(&cwd_a, ta), 1, "the operator's text went in ONCE across both lives");
        assert_continued(&cwd_a, 1, ta, msg_a);
        let ev = custom(&m2, &a, RESUMED);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0]["sent"], "continue");
        assert!(!ka.iter().any(|k| k == "turn_failed"), "{ka:?}");

        assert_done_after_one_spawn(&m2, &b, &cwd_b, tb);
    });
    rt2.shutdown_timeout(Duration::from_secs(2));
}

/// The note a re-entry gets when the reply was already delivered and a LATER
/// step was cut (R13.5-inspect finding 3).
const REPLIED: &str = "Your reply to message";

/// Assert the REPLIED arm on spawn `k`: resumed, the replied note naming
/// #msg, never the text, and never the "your turn was interrupted" wording.
fn assert_replied(cwd: &Path, k: usize, token: &str, msg: u64) {
    let s = spawns(cwd);
    let (resumed, text) = &s[k];
    assert!(*resumed, "the re-entry must RESUME the conversation: {s:?}");
    assert!(!text.contains(token), "an ANSWERED message was written to stdin again (spawn {k}): {text:?}");
    assert!(
        text.contains(REPLIED) && text.contains(&format!("#{msg}")) && text.contains("later step"),
        "the resumed session must be told its reply to #{msg} was delivered: {text:?}"
    );
    assert!(!text.contains("was interrupted and this session"), "not the turn-interrupted note: {text:?}");
    assert!(!text.contains(UNSURE), "a delivered reply is not the UNSURE note (R13.6): {text:?}");
}

/// R13.6 (R13.5-inspect-2 F-a): the note a re-entry gets when the turn taken
/// as the operator's spoke while ANNOUNCED — it may have been a pre-prompt
/// mini-turn — so the message may or may not have been answered.
const UNSURE: &str = "may or may not have been answered";

/// Assert the UNSURE arm on spawn `k`: resumed, never the text, the unsure
/// note naming #msg with its "say so" line, and NEVER the replied note's
/// "Do not answer it again" (which would silence a message the model may
/// never have seen).
fn assert_unsure(cwd: &Path, k: usize, token: &str, msg: u64) {
    let s = spawns(cwd);
    let (resumed, text) = &s[k];
    assert!(*resumed, "the re-entry must RESUME the conversation: {s:?}");
    assert!(!text.contains(token), "the text was written to stdin again (spawn {k}): {text:?}");
    assert!(
        text.contains(UNSURE) && text.contains(&format!("If you cannot see message #{msg} above, say so")),
        "an announced speaking turn must get the UNSURE note naming #{msg}: {text:?}"
    );
    assert!(
        !text.contains("Do not answer it again") && !text.contains(REPLIED),
        "never the replied note when the message may not have been seen: {text:?}"
    );
}

/// **G3 — WHICH TURN OF A RESUMED PROCESS WAS THE OPERATOR'S**
/// (`delivery_evidence`), N = 2 threads on one live daemon, each `kill -9`ed
/// by the pid on its own `running_turns` row once its second `init` is on the
/// ledger. Both start with an ANNOUNCED notification mini-turn (a
/// `task_notification` before any `init`, as the CLI does).
/// * A — `notify_silent_init_hold`, THE PROD SHAPE (DEPLOY-9m seq 54-57, all
///   121 prod specimens): the mini-turn is SILENT (`init`, clean `result`);
///   the operator's turn opens (second, un-announced `init`) and dies before
///   the model writes a word. Nothing the model said could be his, so the
///   original is RE-SENT, exactly once, and no note.
/// * B — `notify_text_init_hold`: the announced mini-turn SPEAKS. The stream
///   cannot tell a speaking mini-turn from the operator's own turn with a
///   notification folded in (G6), and ruling R13.5-4 takes the continue
///   path: never a duplicate. R13.6 (R13.5-inspect-2 F-a): the note on stdin
///   is the UNSURE note ("may or may not have been answered … if you cannot
///   see message #N above, say so"), never the replied note's "Do not answer
///   it again" — the possible loss is loud. The event's `sent` reads
///   "continue" for every note, so this reads the STDIN wording.
/// * C — `notify_text_work_hold`: the same speaking announced mini-turn, then
///   an UN-announced turn that starts work (`tool_use`) and is killed
///   mid-work. That later turn, not the mini-turn, decides whether the reply
///   was delivered (R13.7, IS-55): it never ended, so the mini-turn's
///   `result` is no proof, and the retry gets the UNSURE note — never the
///   replied note's "Do not answer it again" (which it got before R13.7).
///   The text reaches stdin once.
#[tokio::test(flavor = "multi_thread")]
async fn a_silent_announced_mini_turn_then_an_unanswered_turn_resends_once_and_a_speaking_one_continues() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd_a = tmp.path().join("work-a");
    let cwd_b = tmp.path().join("work-b");
    let cwd_c = tmp.path().join("work-c");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    std::fs::create_dir_all(&cwd_c).unwrap();
    plan(&cwd_a, &["notify_silent_init_hold", "ok"]);
    plan(&cwd_b, &["notify_text_init_hold", "ok"]);
    plan(&cwd_c, &["notify_text_work_hold", "ok"]);
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let a = m.create_thread(None, settings(&cwd_a, Some("solo"))).unwrap();
    let b = m.create_thread(None, settings(&cwd_b, Some("solo"))).unwrap();
    let c = m.create_thread(None, settings(&cwd_c, Some("solo"))).unwrap();
    let (ta, tb, tc) = ("ALPHA-silent-mini-turn-unseen", "BRAVO-speaking-mini-turn", "CHARLIE-speaking-mini-turn-then-work");
    let msg_a = send(&m, &a, ta);
    let msg_b = send(&m, &b, tb);
    let msg_c = send(&m, &c, tc);
    kill_after(&m, &a, "system_init", 2).await;
    kill_after(&m, &b, "system_init", 2).await;
    kill_after(&m, &c, "tool_use", 1).await;
    settle(&m, &a, &cwd_a, 2).await;
    settle(&m, &b, &cwd_b, 2).await;
    settle(&m, &c, &cwd_c, 2).await;

    // A: the prod shape → re-sent once.
    let ka = kinds(&m, &a);
    assert!(ka.iter().any(|k| k == "task_notification"), "non-vacuity: announced: {ka:?}");
    assert!(ka.iter().any(|k| k == "retry_scheduled"), "{ka:?}");
    assert_eq!(spawns(&cwd_a).len(), 2, "{:?}", spawns(&cwd_a));
    assert_resent(&cwd_a, 1, ta);
    assert_eq!(on_stdin(&cwd_a, ta), 2, "the first write plus exactly ONE re-send: {:?}", spawns(&cwd_a));
    let ea = custom(&m, &a, RESUMED);
    assert_eq!(ea.len(), 1, "{ea:?}");
    assert_eq!(ea[0]["sent"], "prompt", "{}", ea[0]);
    assert_eq!(ea[0]["resent_ids"], json!([msg_a]));
    assert_eq!(ea[0]["delivered_ids"], json!([]));

    // B: a speaking announced turn → continue, never re-sent.
    let kb = kinds(&m, &b);
    assert!(
        kb.iter().take_while(|k| k.as_str() != "system_init").any(|k| k == "task_notification")
            && kb.iter().any(|k| k == "assistant_text"),
        "non-vacuity: announced AND speaking: {kb:?}"
    );
    assert_eq!(spawns(&cwd_b).len(), 2, "{:?}", spawns(&cwd_b));
    assert_eq!(on_stdin(&cwd_b, tb), 1, "{:?}", spawns(&cwd_b));
    let s = spawns(&cwd_b);
    assert!(s[1].0 && !s[1].1.contains(tb) && s[1].1.contains(&format!("#{msg_b}")), "{s:?}");
    let eb = custom(&m, &b, RESUMED);
    assert_eq!(eb.len(), 1, "{eb:?}");
    assert_eq!(eb[0]["sent"], "continue", "{}", eb[0]);
    assert_eq!(eb[0]["delivered_ids"], json!([msg_b]));
    // R13.6: the stdin wording, not the event field.
    assert_unsure(&cwd_b, 1, tb, msg_b);
    assert_eq!(eb[0]["unsure_ids"], json!([msg_b]), "{}", eb[0]);
    assert_eq!(eb[0]["replied_ids"], json!([]), "{}", eb[0]);

    // C: a later un-announced turn spoke (work) → evidence it was seen, not unsure.
    let kc = kinds(&m, &c);
    let shape_c: Vec<&str> = kc
        .iter()
        .take_while(|x| x.as_str() != "turn_ended")
        .map(|x| x.as_str())
        .filter(|x| matches!(*x, "system_init" | "assistant_text" | "result_turn" | "task_notification" | "tool_use"))
        .collect();
    assert_eq!(
        shape_c,
        vec!["task_notification", "system_init", "assistant_text", "result_turn", "system_init", "tool_use"],
        "non-vacuity: announced speaking mini-turn, then an un-announced turn that works: {kc:?}"
    );
    assert_eq!(spawns(&cwd_c).len(), 2, "{:?}", spawns(&cwd_c));
    assert_eq!(on_stdin(&cwd_c, tc), 1, "{:?}", spawns(&cwd_c));
    let sc = spawns(&cwd_c);
    assert!(sc[1].0 && !sc[1].1.contains(tc) && sc[1].1.contains(&format!("#{msg_c}")), "{sc:?}");
    // R13.7: cut mid-work after a speaking mini-turn → UNSURE, never replied.
    assert_unsure(&cwd_c, 1, tc, msg_c);
    let ec = custom(&m, &c, RESUMED);
    assert_eq!(ec.len(), 1, "{ec:?}");
    assert_eq!(ec[0]["sent"], "continue", "{}", ec[0]);
    assert_eq!(ec[0]["unsure_ids"], json!([msg_c]), "{}", ec[0]);
    assert_eq!(ec[0]["replied_ids"], json!([]), "{}", ec[0]);
}

/// **G5 — AN ANSWERED MESSAGE IS NEVER RE-SENT, WHATEVER WAKES THE PROCESS
/// AFTER IT** (`delivery_evidence`), N = 2 threads on one live daemon, each
/// `kill -9`ed by the pid on its own `running_turns` row once its SECOND
/// `init` is on the ledger. Both answered the operator first (`init`, text,
/// clean `result`), then a new CLI turn opened that wrote nothing:
/// * A — `answer_wake_init_hold`: the wake is announced (a
///   `task_notification` for a task this process launched, then `init`).
/// * B — `answer_bare_init_hold`: the wake comes with NO announcing line.
///   That stream is byte-for-byte G3-A's minus the announcement — an
///   un-announced pre-prompt mini-turn would look exactly like this — and the
///   daemon takes the continue note: never a duplicate (executor-27's
///   ruling), the residual loss reported.
/// Both get the REPLIED note (their reply was delivered; a later step was
/// cut — R13.5-inspect finding 3) and the text reaches stdin exactly once.
/// The first R13.5 floor ("first `init` after the first `result`") re-sent
/// both. Opposed by G3-A, which must still re-send.
#[tokio::test(flavor = "multi_thread")]
async fn an_answered_message_woken_by_an_init_only_then_killed_continues_and_is_never_resent() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd_a = tmp.path().join("work-a");
    let cwd_b = tmp.path().join("work-b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    plan(&cwd_a, &["answer_wake_init_hold", "ok"]);
    plan(&cwd_b, &["answer_bare_init_hold", "ok"]);
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let a = m.create_thread(None, settings(&cwd_a, Some("solo"))).unwrap();
    let b = m.create_thread(None, settings(&cwd_b, Some("solo"))).unwrap();
    let (ta, tb) = ("ALPHA-answered-announced-wake", "BRAVO-answered-bare-wake");
    let msg_a = send(&m, &a, ta);
    let msg_b = send(&m, &b, tb);
    kill_after(&m, &a, "system_init", 2).await;
    kill_after(&m, &b, "system_init", 2).await;
    settle(&m, &a, &cwd_a, 2).await;
    settle(&m, &b, &cwd_b, 2).await;

    for (id, cwd, token, msg) in [(&a, &cwd_a, ta, msg_a), (&b, &cwd_b, tb, msg_b)] {
        let k = kinds(&m, id);
        // The killed attempt's stream: everything before its `turn_ended`.
        let first: Vec<&str> = k
            .iter()
            .take_while(|x| x.as_str() != "turn_ended")
            .map(|x| x.as_str())
            .filter(|x| matches!(*x, "system_init" | "assistant_text" | "result_turn"))
            .collect();
        assert_eq!(
            first,
            vec!["system_init", "assistant_text", "result_turn", "system_init"],
            "non-vacuity: answered, then a new turn opened and wrote nothing: {k:?}"
        );
        assert!(k.iter().any(|x| x == "retry_scheduled"), "the woken, killed turn is laddered: {k:?}");
        assert_eq!(spawns(cwd).len(), 2, "{:?}", spawns(cwd));
        assert_replied(cwd, 1, token, msg);
        assert_eq!(on_stdin(cwd, token), 1, "an ANSWERED message reached stdin exactly ONCE: {:?}", spawns(cwd));
        let ev = custom(&m, id, RESUMED);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0]["sent"], "continue", "{}", ev[0]);
        assert_eq!(ev[0]["delivered_ids"], json!([msg]));
        assert_eq!(ev[0]["resent_ids"], json!([]));
        assert_eq!(ev[0]["replied_ids"], json!([msg]));
        assert_eq!(ev[0]["unsure_ids"], json!([]), "R13.6: a clean result before the cut is not unsure");
    }
    assert!(
        kinds(&m, &a).iter().any(|x| x == "task_notification"),
        "non-vacuity: A's wake was announced"
    );
    assert!(
        !kinds(&m, &b).iter().any(|x| x == "task_notification"),
        "non-vacuity: B's wake was not"
    );
}

/// **G6 — THE OPERATOR'S OWN TURN CAN BE ANNOUNCED; IF IT SPOKE, IT IS HIS**
/// (R13.5-inspect finding 1, its q1d), N = 2 threads on one live daemon,
/// each `kill -9`ed by the pid on its own `running_turns` row once its last
/// (bare, silent) wake `init` is on the ledger.
/// * A — `announced_turn_answer_bare_hold`: an announced silent mini-turn,
///   then the operator's turn — itself announced by a second notification —
///   ANSWERS (`init`, text, `result`), then a bare `init` wake.
/// * B — `folded_notice_answer_hold`: one announced turn that answers with a
///   notification folded in (`init`, text, notification, `result`), then a
///   bare `init` wake.
/// "The first UN-announced turn" took each bare wake as his, found no
/// output, and wrote the answered prompt again (2x on stdin). An announced
/// turn is skipped only while SILENT, so both continue and the text reaches
/// stdin exactly once. Their turn was taken as his only because it SPOKE
/// while announced — G3-B's shape (B differs from it only by the folded
/// notification) — so since R13.6 both get the UNSURE note, not the
/// replied note: the stream cannot tell his answered turn from a speaking
/// mini-turn. Opposed by G3-A (silent announced mini-turn → re-sent).
#[tokio::test(flavor = "multi_thread")]
async fn an_announced_operator_turn_that_answered_is_never_resent_when_a_bare_wake_follows() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd_a = tmp.path().join("work-a");
    let cwd_b = tmp.path().join("work-b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    plan(&cwd_a, &["announced_turn_answer_bare_hold", "ok"]);
    plan(&cwd_b, &["folded_notice_answer_hold", "ok"]);
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let a = m.create_thread(None, settings(&cwd_a, Some("solo"))).unwrap();
    let b = m.create_thread(None, settings(&cwd_b, Some("solo"))).unwrap();
    let (ta, tb) = ("ALPHA-announced-own-turn", "BRAVO-folded-notice");
    let msg_a = send(&m, &a, ta);
    let msg_b = send(&m, &b, tb);
    kill_after(&m, &a, "system_init", 3).await;
    kill_after(&m, &b, "system_init", 2).await;
    settle(&m, &a, &cwd_a, 2).await;
    settle(&m, &b, &cwd_b, 2).await;

    for (id, cwd, token, msg, shape) in [
        (&a, &cwd_a, ta, msg_a, vec!["task_notification", "system_init", "result_turn", "task_notification", "system_init", "assistant_text", "result_turn", "system_init"]),
        (&b, &cwd_b, tb, msg_b, vec!["task_notification", "system_init", "assistant_text", "task_notification", "result_turn", "system_init"]),
    ] {
        let k = kinds(&m, id);
        let first: Vec<&str> = k
            .iter()
            .take_while(|x| x.as_str() != "turn_ended")
            .map(|x| x.as_str())
            .filter(|x| matches!(*x, "system_init" | "assistant_text" | "result_turn" | "task_notification"))
            .collect();
        assert_eq!(first, shape, "non-vacuity: the killed attempt's stream: {k:?}");
        assert_eq!(spawns(cwd).len(), 2, "{:?}", spawns(cwd));
        assert_eq!(on_stdin(cwd, token), 1, "an ANSWERED message reached stdin exactly ONCE: {:?}", spawns(cwd));
        assert_unsure(cwd, 1, token, msg);
        let ev = custom(&m, id, RESUMED);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0]["sent"], "continue", "{}", ev[0]);
        assert_eq!(ev[0]["resent_ids"], json!([]));
        assert_eq!(ev[0]["unsure_ids"], json!([msg]), "{}", ev[0]);
    }
}

/// One `system_init` as the CLI streams it (only the shape matters here).
fn init_ev() -> ThreadEvent {
    ThreadEvent::SystemInit {
        claude_session_id: Some("fake-sess".into()),
        cwd: None,
        model: Some("claude-fable-5".into()),
        tools: vec![],
        mcp_servers: Value::Null,
        permission_mode: None,
        slash_commands: vec![],
        api_key_source: None,
        agents: Value::Null,
    }
}

fn text_ev(words: &str) -> ThreadEvent {
    ThreadEvent::AssistantText { text: words.into(), parent_tool_use_id: None, api_message_id: None }
}

fn result_ev() -> ThreadEvent {
    ThreadEvent::ResultTurn {
        subtype: "success".into(),
        is_error: false,
        total_cost_usd: None,
        num_turns: Some(1),
        duration_ms: None,
        stop_reason: Some("end_turn".into()),
        model_usage: Value::Null,
        structured_output: None,
    }
}

fn announce_ev(n: u32) -> ThreadEvent {
    ThreadEvent::TaskNotification {
        task_id: format!("bprev{n}"),
        tool_use_id: None,
        status: Some("stopped".into()),
        output_file: None,
        summary: Some("Background shell command did not finish before the previous session ended".into()),
        total_tokens: None,
        tool_uses: None,
        duration_ms: None,
        session_id: Some("fake-sess".into()),
    }
}

/// **G7 — A REPLY CUT MID-TOOL AFTER A SPEAKING MINI-TURN IS NEVER TOLD "DO
/// NOT ANSWER IT AGAIN"** (R13.7, IS-55; R13.6-inspect F-b), through L-1
/// restart recovery: the previous daemon died with the child mid-turn, N = 2
/// threads on one daemon. Both ledgers open with an ANNOUNCED notification
/// mini-turn that SPEAKS (`task_notification`, `init`, text, clean
/// `result`), then an UN-announced turn that speaks:
/// * A — that turn writes text and calls a tool (`init`, text, `tool_use`)
///   and the process dies mid-tool. His turn never ended, so the
///   mini-turn's `result` proves nothing: UNSURE note, `replied_ids` empty.
///   Before R13.7 the first speaking turn's `result` made it REPLIED.
/// * B — the opposing arm: that turn ANSWERS and ends (`init`, text, clean
///   `result`), then a bare `init` wake dies. A genuinely completed reply:
///   the REPLIED note, `unsure_ids` empty. A constant "unsure" fails here.
/// Neither text reaches stdin again.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_cut_mid_tool_after_a_speaking_mini_turn_is_unsure_and_a_completed_reply_is_replied() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd_a = tmp.path().join("work-a");
    let cwd_b = tmp.path().join("work-b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    let (ta, tb) = ("ALPHA-mini-turn-then-cut-mid-tool", "BRAVO-mini-turn-then-completed-reply");
    let (a, b) = {
        let m1 = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
        let a = m1.create_thread(None, settings(&cwd_a, Some("solo"))).unwrap();
        let b = m1.create_thread(None, settings(&cwd_b, Some("solo"))).unwrap();
        for (id, words) in [(&a, ta), (&b, tb)] {
            prior_attempt(&m1, id, 7, words, false);
            // The speaking announced mini-turn.
            append(&m1, id, announce_ev(1));
            append(&m1, id, init_ev());
            append(&m1, id, text_ev("checking that background task"));
            append(&m1, id, result_ev());
            // The un-announced turn that speaks.
            append(&m1, id, init_ev());
            append(&m1, id, text_ev("on it — running the build"));
        }
        append(
            &m1,
            &a,
            ThreadEvent::ToolUse {
                id: "toolu_cut".into(),
                name: "Bash".into(),
                input: json!({"command": "cargo build"}),
                parent_tool_use_id: None,
            },
        );
        append(&m1, &b, result_ev());
        append(&m1, &b, init_ev());
        for (id, words) in [(&a, ta), (&b, tb)] {
            m1.store
                .upsert_running_turn(&RunningTurnRow {
                    thread_id: id.clone(),
                    pid: dead_pid(),
                    spawn_cookie: "cookie".into(),
                    output_path: tmp.path().join(format!("{id}.jsonl")).to_string_lossy().to_string(),
                    byte_offset: 0,
                    started_at: threads::store::now_rfc3339(),
                    prompt: Some(words.to_string()),
                    message_ids: vec![7],
                    attempt: 1,
                })
                .unwrap();
        }
        (a, b)
    };
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    m.recover().await;
    assert!(m.store.get_retry(&a).unwrap().is_some(), "non-vacuity: L-1 put A on the ladder");
    assert!(m.store.get_retry(&b).unwrap().is_some(), "non-vacuity: L-1 put B on the ladder");
    settle(&m, &a, &cwd_a, 1).await;
    settle(&m, &b, &cwd_b, 1).await;

    // A: cut mid-tool → UNSURE.
    assert_eq!(spawns(&cwd_a).len(), 1, "{:?}", spawns(&cwd_a));
    assert_eq!(on_stdin(&cwd_a, ta), 0, "the delivered text is never written again: {:?}", spawns(&cwd_a));
    assert_unsure(&cwd_a, 0, ta, 7);
    let ea = custom(&m, &a, RESUMED);
    assert_eq!(ea.len(), 1, "{ea:?}");
    assert_eq!(ea[0]["sent"], "continue", "{}", ea[0]);
    assert_eq!(ea[0]["delivered_ids"], json!([7]), "{}", ea[0]);
    assert_eq!(ea[0]["unsure_ids"], json!([7]), "{}", ea[0]);
    assert_eq!(ea[0]["replied_ids"], json!([]), "{}", ea[0]);

    // B: the later turn ended cleanly → REPLIED.
    assert_eq!(spawns(&cwd_b).len(), 1, "{:?}", spawns(&cwd_b));
    assert_eq!(on_stdin(&cwd_b, tb), 0, "the delivered text is never written again: {:?}", spawns(&cwd_b));
    assert_replied(&cwd_b, 0, tb, 7);
    let eb = custom(&m, &b, RESUMED);
    assert_eq!(eb.len(), 1, "{eb:?}");
    assert_eq!(eb[0]["sent"], "continue", "{}", eb[0]);
    assert_eq!(eb[0]["replied_ids"], json!([7]), "{}", eb[0]);
    assert_eq!(eb[0]["unsure_ids"], json!([]), "{}", eb[0]);
}

#[path = "support/first_exec.rs"]
mod first_exec;
