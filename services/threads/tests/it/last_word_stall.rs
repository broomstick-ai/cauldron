//! **R13.5 — THE STALL ARM JUDGES ON THE LAST WORD, TOO.**
//!
//! R13.4 moved R13.3's no-verdict guard off the sticky "a clean `result` has
//! arrived" latch (`TurnSignals::result_delivered`) onto "the process's LAST
//! word was a clean `result`" (`TurnSignals::last_word_result`), because one
//! `-p --resume` process can stream SEVERAL CLI turns — a queued
//! `<task-notification>` mini-turn first, then the operator's. The output-stall
//! teardown (`StallClock::retry_decision`, `reaped_after_verdict`) stayed on
//! the sticky latch under executor ruling E17-8, so a notification result
//! followed by the operator's turn HANGING was reaped as "delivered": the
//! message silently lost. Executor-27 overrode E17-8 (round 42, H179-3); this
//! file is the stall arm's guard. The adopt arm and the evidence window live
//! in `tests/no_resend.rs` (G2, G3).
//!
//! A separate test binary because the stall bound is process-global
//! (`THREADS_OUTPUT_STALL_MS`): `no_resend.rs` holds children for the test to
//! kill, and a short bound there would reap them first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use threads::event::ThreadEvent;
use threads::runtime::PassOrigin;
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

/// The stall bound. Long enough that a fake slowed by a loaded machine is not
/// reaped before its first lines (which would ladder the opposing arm and read
/// as a false red), short enough to keep the test under ~15 s.
const STALL_MS: u64 = 4000;
const NOTE: &str = "already delivered";
const RESUMED: &str = "cauldron.threads/delivery_resumed";

fn set_env() {
    // Identical values in every test of this binary, so the global env cannot race.
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude-resend"));
    first_exec::warmed("tests/fixtures/fake-claude");
    std::env::set_var("THREADS_CLAUDE_BIN", fake);
    std::env::set_var("THREADS_OUTPUT_STALL_MS", STALL_MS.to_string());
    std::env::set_var("THREADS_HEARTBEAT_MS", "100");
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("THREADS_KILL_GRACE_MS", "300");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &Path) -> Config {
    let mut map = BTreeMap::new();
    let cd = dir.join("solo");
    std::fs::create_dir_all(&cd).unwrap();
    map.insert("solo".to_string(), cd.to_string_lossy().to_string());
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts: map,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s.account = threads::settings::AccountChoice::Named("solo".into());
    s
}

fn plan(cwd: &Path, modes: &[&str]) {
    std::fs::write(cwd.join("plan"), modes.join("\n") + "\n").unwrap();
}

/// Every spawn's stdin, in order: `(resumed, text)` (the fake's `stdin.log`).
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

fn count(m: &ThreadManager, id: &str, kind: &str) -> usize {
    kinds(m, id).iter().filter(|k| k.as_str() == kind).count()
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

/// **G1 — A STALL AFTER AN EARLIER NOTIFICATION RESULT IS LADDERED** (+ G4's
/// stall arm), N = 2 threads on one live daemon; the daemon's own stall
/// teardown is the kill (no test kill: the arm under test is the stall's).
/// * A — `notify_then_work_hold`: a queued notification mini-turn (`init`,
///   clean `result`), then the operator's turn (`init`, `tool_use`), then
///   SILENCE past the bound. Under the sticky latch the stall reaped it as
///   "delivered" and the turn landed clean: the operator's message lost.
///   Now: laddered, and the retry resumes with the continue note — the text
///   reaches stdin exactly once.
/// * B — `answer_trailing_hold` (G4, opposing): the turn answered, its clean
///   `result` is the last word (only background-task lines trail it), then
///   silence past the bound. Reaped and DONE: one spawn, no retry — waited
///   past 2x the stall bound after the reap, and the pool's deadline route
///   forced, so a retry that was coming would have fired.
#[tokio::test(flavor = "multi_thread")]
async fn a_stall_after_an_earlier_notification_result_is_laddered_and_a_last_word_result_stall_is_done_for_8000ms_after(
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd_a = tmp.path().join("work-a");
    let cwd_b = tmp.path().join("work-b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    plan(&cwd_a, &["notify_then_work_hold", "ok"]);
    plan(&cwd_b, &["answer_trailing_hold"]);
    let m = build_manager(config_at(tmp.path())).unwrap();
    m.recover().await;
    let a = m.create_thread(None, settings(&cwd_a)).unwrap();
    let b = m.create_thread(None, settings(&cwd_b)).unwrap();
    let (ta, tb) = ("ALPHA-stall-earlier-result", "BRAVO-stall-last-word");
    let msg_a = send(&m, &a, ta);
    send(&m, &b, tb);

    // Both reaped by the STALL (one `turn_killed` each), not by anything else.
    for id in [&a, &b] {
        assert!(
            eventually(40, || count(&m, id, "turn_killed") >= 1 && count(&m, id, "turn_ended") >= 1).await,
            "the stall never reaped the silent child: {:?}",
            kinds(&m, id)
        );
    }
    // Let a retry that is coming fire: force the deadline route, past 2x the bound.
    let settled = eventually(40, || {
        if m.store.get_retry(&a).unwrap().is_some() {
            m.retry_sweep(PassOrigin::Cadence);
        }
        spawns(&cwd_a).len() >= 2
            && m.store.get_retry(&a).unwrap().is_none()
            && m.is_idle(&a).unwrap_or(false)
    })
    .await;
    m.retry_sweep(PassOrigin::Cadence);
    tokio::time::sleep(Duration::from_millis(2 * STALL_MS)).await;

    // A: laddered with the continue note, the text in ONCE.
    let ka = kinds(&m, &a);
    assert!(
        settled,
        "A must be retried after the stall, not landed on the notification's result: \
         spawns={:?} ledger={ka:?}",
        spawns(&cwd_a)
    );
    assert_eq!(count(&m, &a, "turn_killed"), 1, "reaped once by the stall: {ka:?}");
    assert!(ka.iter().any(|k| k == "retry_scheduled"), "{ka:?}");
    let s = spawns(&cwd_a);
    assert_eq!(s.len(), 2, "{s:?}");
    assert!(s[1].0, "the retry RESUMES the conversation: {s:?}");
    assert!(
        s[1].1.contains(NOTE) && s[1].1.contains(&format!("#{msg_a}")) && !s[1].1.contains(ta),
        "the retry carries the continue note, never the text: {s:?}"
    );
    assert!(
        !s[1].1.contains("Your reply to message"),
        "no reply had been delivered (the only result was the mini-turn's), so not the replied note: {s:?}"
    );
    assert_eq!(on_stdin(&cwd_a, ta), 1, "the operator's text reached stdin exactly ONCE: {s:?}");
    let ea = custom(&m, &a, RESUMED);
    assert_eq!(ea.len(), 1, "{ea:?}");
    assert_eq!(ea[0]["sent"], "continue", "{}", ea[0]);
    assert!(!ka.iter().any(|k| k == "turn_failed"), "{ka:?}");
    assert!(
        custom(&m, &a, "post_result_output_truncated").is_empty(),
        "a laddered turn's output is not 'truncated, not retried': {ka:?}"
    );

    // B: done — the last word was a clean result.
    let kb = kinds(&m, &b);
    assert_eq!(count(&m, &b, "turn_killed"), 1, "reaped once by the stall: {kb:?}");
    assert!(
        !kb.iter().any(|k| k == "retry_scheduled" || k == "turn_failed"),
        "a stall whose LAST word was a clean result lands on it: {kb:?}"
    );
    assert!(m.store.get_retry(&b).unwrap().is_none(), "no ladder row");
    assert_eq!(spawns(&cwd_b).len(), 1, "no re-entry: {:?}", spawns(&cwd_b));
    assert_eq!(on_stdin(&cwd_b, tb), 1);
    assert!(custom(&m, &b, RESUMED).is_empty(), "a finished turn is not re-entered");
    assert!(m.is_idle(&b).unwrap_or(false), "B lands idle");
}

/// **R13.5-inspect finding 3 — A STALL AFTER THE REPLY WAS DELIVERED TELLS
/// THE MODEL SO.** N = 2 threads on one live daemon, both
/// `answer_wake_work_hold`: the operator's turn ANSWERS (`init`, text, clean
/// `result`), then an announced wake opens a new turn that starts work
/// (`init`, `tool_use`) and goes silent past the stall bound. Since the
/// E17-8 inversion that is laddered, and the retry used to be told "Your
/// turn on message #N was interrupted … Continue the work" — about a message
/// it had already answered, which invites a second reply. It is now told
/// its reply to #N was delivered and a later step was cut. Opposed by
/// G1-A (no reply before the cut → the turn-interrupted note, asserted
/// there). The text reaches stdin exactly once either way.
#[tokio::test(flavor = "multi_thread")]
async fn a_stall_in_a_wake_after_the_reply_tells_the_retry_the_reply_was_delivered() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    m.recover().await;
    let mut cases = Vec::new();
    for (name, token) in [("work-c", "CHARLIE-replied-then-stalled"), ("work-d", "DELTA-replied-then-stalled")] {
        let cwd = tmp.path().join(name);
        std::fs::create_dir_all(&cwd).unwrap();
        plan(&cwd, &["answer_wake_work_hold", "ok"]);
        let id = m.create_thread(None, settings(&cwd)).unwrap();
        let msg = send(&m, &id, token);
        cases.push((id, cwd, token, msg));
    }
    for (id, cwd, token, msg) in &cases {
        let ok = eventually(40, || {
            if m.store.get_retry(id).unwrap().is_some() {
                m.retry_sweep(PassOrigin::Cadence);
            }
            spawns(cwd).len() >= 2 && m.store.get_retry(id).unwrap().is_none() && m.is_idle(id).unwrap_or(false)
        })
        .await;
        let k = kinds(&m, id);
        assert!(ok, "the stalled wake must be laddered and retried: spawns={:?} ledger={k:?}", spawns(cwd));
        assert_eq!(count(&m, id, "turn_killed"), 1, "reaped once by the stall: {k:?}");
        let s = spawns(cwd);
        assert_eq!(s.len(), 2, "{s:?}");
        assert!(s[1].0, "resumed: {s:?}");
        assert!(!s[1].1.contains(*token), "never the text: {s:?}");
        assert!(
            s[1].1.contains("Your reply to message") && s[1].1.contains(&format!("#{msg}")) && s[1].1.contains("later step"),
            "the retry is told its reply was delivered: {s:?}"
        );
        assert!(!s[1].1.contains("was interrupted and this session"), "not the turn-interrupted note: {s:?}");
        assert_eq!(on_stdin(cwd, token), 1, "{s:?}");
        let ev = custom(&m, id, RESUMED);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0]["sent"], "continue");
        assert_eq!(ev[0]["replied_ids"], json!([msg]));
    }
}

#[path = "support/first_exec.rs"]
mod first_exec;
