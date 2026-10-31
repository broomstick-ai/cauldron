//! **GAP A-4 — A TURN THAT RUNS ONTO PAID OVERAGE, OR WHOSE REQUEST THE VENDOR
//! REJECTS, BARS ITS ACCOUNT FOR THE NEXT PLACEMENT.**
//!
//! `rate_limit_event` carries `status` and `isUsingOverage`, and the runtime's
//! arm dropped both with `..`. Worse, the shape claude has emitted since at
//! least 2026-09-28 has no top-level `utilization` — it carries
//! `unifiedWindows` — so the parser keeps it as `Unknown` and the runtime never
//! read it at all. A turn ran into paid overage, the TUI transcript said "on
//! overage", and placement kept loading work onto that account.
//!
//! Every stream line here is the CURRENT shape, copied from the ledger
//! (`libs/shapewire/seam/conformance/one_thread.jsonl`) by the fake's
//! `limitpush` mode with its resets computed from now.
//!
//! # Forced red
//!
//! * At base `17efad03` (no reader of the line at all): §1 and §2 red — the
//!   account is never barred. §3 green (nothing to bar) EXCEPT its
//!   non-vacuity check, which is red because the windows never reach the
//!   gauge.
//! * `bars()` returning `true` always: §3 and the parser test red. Returning
//!   `false`: §1, §2 and the parser test red.
//! * The bar written on the 1h default instead of the named reset: §1 red.
//! * Clearing an `overage` bar in `note_account_served` (the pre-fix body):
//!   §1 red at "the bar survives the turn that ran onto the overage".
//! * Writing through `note_account_limit_for` (which ends in
//!   `raise_limit_kill`): §1 and §2 red at "the running turn is not torn down".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::json;
use threads::event::Push;
use threads::gauge::Axis;
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env() {
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// `aa-over` sorts first, so an unplaced thread is born on it; `zz-free` is
/// where placement must send the next turn once `aa-over` is barred.
fn config_at(dir: &Path) -> Config {
    let mut map = BTreeMap::new();
    for name in ["aa-over", "zz-free"] {
        let cd = dir.join(name);
        std::fs::create_dir_all(&cd).unwrap();
        map.insert(name.to_string(), cd.to_string_lossy().to_string());
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

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

fn send(m: &ThreadManager, id: &str, text: &str) {
    m.send(
        id,
        text.to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();
}

/// Every pushed event, as `(thread_id, kind)`.
fn watch(m: &ThreadManager) -> Arc<Mutex<Vec<(String, String)>>> {
    let mut rx = m.subscribe();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Event(ev) = p {
                out.lock()
                    .unwrap()
                    .push((ev.thread_id.clone(), ev.event.kind().to_string()));
            }
        }
    });
    seen
}

fn count(seen: &Arc<Mutex<Vec<(String, String)>>>, id: &str, kind: &str) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|(t, k)| t == id && k == kind)
        .count()
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..600 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

fn state_of(m: &ThreadManager, name: &str) -> threads::accounts::AccountState {
    m.store
        .list_account_states()
        .unwrap()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no accounts row for {name}"))
}

fn barred(m: &ThreadManager, name: &str) -> bool {
    m.barred_accounts().0.contains(&name.to_string())
}

fn release_file(dir: &Path) -> PathBuf {
    dir.join("release-turn")
}

/// Run `push` on a thread born on `aa-over`, holding the turn after the event.
/// Returns once the turn is running and has had time to emit the event.
async fn held_turn(m: &ThreadManager, dir: &Path, push: &str) -> String {
    let id = m.create_thread(None, settings(dir)).unwrap();
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "aa-over",
        "non-vacuity: the thread is born on the account the push is about"
    );
    send(
        m,
        &id,
        &format!("limitpush push={push} release={}", release_file(dir).display()),
    );
    id
}

/// **§1 — overage bars the account; the running turn finishes; the bar
/// survives that turn's completion; the NEXT turn goes elsewhere.**
#[tokio::test]
async fn an_overage_push_bars_the_account_and_the_next_turn_goes_elsewhere() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    let seen = watch(&m);
    assert!(!barred(&m, "aa-over"), "non-vacuity: nothing is barred before the push");

    let id = held_turn(&m, tmp.path(), "overage").await;

    assert!(
        eventually(|| barred(&m, "aa-over")).await,
        "a rate_limit_event with isUsingOverage:true must bar the account: {:?}",
        state_of(&m, "aa-over")
    );
    let st = state_of(&m, "aa-over");
    assert_eq!(st.limit_axis.as_deref(), Some(threads::runtime::OVERAGE_AXIS));
    // The window is the exhausted five-hour reset (~2h out) plus the slack —
    // not the 1h default and not the 3-day weekly.
    let secs = (DateTime::parse_from_rfc3339(st.limit_until.as_deref().unwrap())
        .unwrap()
        .with_timezone(&Utc)
        - Utc::now())
    .num_seconds();
    assert!(
        (7200 + 60 - 15..=7200 + 60 + 5).contains(&secs),
        "the bar ends a minute past the exhausted window's own reset: {secs}s"
    );
    assert!(!barred(&m, "zz-free"), "and only that account");

    // The running turn is NOT torn down: release it and it completes normally.
    assert_eq!(count(&seen, &id, "turn_ended"), 0, "the turn is still held");
    std::fs::write(release_file(tmp.path()), b"").unwrap();
    assert!(
        eventually(|| count(&seen, &id, "turn_ended") == 1).await,
        "the held turn must finish: {:?}",
        seen.lock().unwrap()
    );
    assert_eq!(
        count(&seen, &id, "turn_started"),
        1,
        "the running turn is not torn down and re-run: {:?}",
        seen.lock().unwrap()
    );
    assert_eq!(count(&seen, &id, "turn_parked"), 0);
    assert_eq!(count(&seen, &id, "retry_scheduled"), 0);

    // The bar survives the turn that ran onto the overage.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        barred(&m, "aa-over"),
        "the bar survives the turn that ran onto the overage — its completion \
         proves the account serves on paid usage, not that it serves for free: {:?}",
        state_of(&m, "aa-over")
    );

    // The NEXT placement goes elsewhere.
    send(&m, &id, "and again");
    assert!(
        eventually(|| m.serving_account(&id).unwrap() == "zz-free").await,
        "the next turn must not be placed on the overage account: {:?}",
        m.serving_account(&id)
    );
    assert!(
        eventually(|| count(&seen, &id, "turn_ended") == 2).await,
        "and it runs there: {:?}",
        seen.lock().unwrap()
    );
}

/// **§2 — `status:"rejected"` bars the account while the turn is still
/// running, without tearing it down, and a turn placed meanwhile goes
/// elsewhere.**
#[tokio::test]
async fn a_rejected_push_bars_the_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    let seen = watch(&m);

    let id = held_turn(&m, tmp.path(), "rejected").await;

    assert!(
        eventually(|| barred(&m, "aa-over")).await,
        "a rate_limit_event with status:\"rejected\" must bar the account: {:?}",
        state_of(&m, "aa-over")
    );
    assert_eq!(
        state_of(&m, "aa-over").limit_axis.as_deref(),
        Some(Axis::Session.as_str()),
        "a refusal without overage is recorded on the refused window's own axis"
    );

    // A second thread placed while the first is still running on aa-over.
    let other = m.create_thread(None, settings(tmp.path())).unwrap();
    send(&m, &other, "hello");
    assert!(
        eventually(|| count(&seen, &other, "turn_ended") == 1).await,
        "the other thread's turn runs: {:?}",
        seen.lock().unwrap()
    );
    assert_eq!(
        m.serving_account(&other).unwrap(),
        "zz-free",
        "the next placement goes elsewhere"
    );

    // The held turn was never torn down.
    assert_eq!(count(&seen, &id, "turn_ended"), 0, "still held");
    std::fs::write(release_file(tmp.path()), b"").unwrap();
    assert!(eventually(|| count(&seen, &id, "turn_ended") == 1).await);
    assert_eq!(count(&seen, &id, "turn_started"), 1, "{:?}", seen.lock().unwrap());
    assert_eq!(count(&seen, &id, "turn_parked"), 0);
}

/// **§3, the opposing case — `status:"allowed"`, overage false (and the decoy
/// `overageStatus:"rejected"`, which only says overage is not available) bars
/// nothing, and the next turn stays where it is.** Non-vacuous: the same line's
/// windows DID reach the gauge, so it was read.
#[tokio::test]
async fn an_allowed_push_without_overage_bars_nothing() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    let seen = watch(&m);

    // HELD, and asserted while held: a completed turn clears a non-overage
    // bar (`note_account_served`), so "nothing barred" checked after the turn
    // ends would pass even if this line HAD barred. Forced: `bars()` returning
    // `true` was green here before the hold was added.
    let id = held_turn(&m, tmp.path(), "allowed").await;

    // Non-vacuity: the line was READ — both windows reached the gauge.
    let obs = |axis: Axis| {
        m.store
            .recent_observations("aa-over", axis.as_str(), 1)
            .unwrap()
            .into_iter()
            .next()
    };
    assert!(
        eventually(|| obs(Axis::Session).is_some() && obs(Axis::WeeklyAll).is_some()).await,
        "both unifiedWindows must be recorded as gauge observations"
    );
    let s = obs(Axis::Session).unwrap();
    let w = obs(Axis::WeeklyAll).unwrap();
    assert_eq!((s.pct.round(), s.source.as_str()), (20.0, "rate_limit_event"));
    assert_eq!((w.pct.round(), w.source.as_str()), (3.0, "rate_limit_event"));
    assert!(s.resets_at.is_some() && w.resets_at.is_some());

    // The bar would be written in the SAME synchronous call that recorded the
    // windows, microseconds after them; 300ms also outlasts the 20ms tail poll.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count(&seen, &id, "turn_ended"), 0, "still held");
    assert!(
        !barred(&m, "aa-over"),
        "status allowed with no overage must not bar: {:?}",
        state_of(&m, "aa-over")
    );
    assert_eq!(state_of(&m, "aa-over").limit_until, None);

    std::fs::write(release_file(tmp.path()), b"").unwrap();
    assert!(eventually(|| count(&seen, &id, "turn_ended") == 1).await);
    send(&m, &id, "and again");
    assert!(eventually(|| count(&seen, &id, "turn_ended") == 2).await);
    assert_eq!(m.serving_account(&id).unwrap(), "aa-over", "the next turn stays");
}

/// The parser half, on the verbatim ledger line: read, and benign.
#[test]
fn the_verbatim_ledger_line_is_read_and_does_not_bar() {
    let line = json!({"rate_limit_info": {"isUsingOverage": false,
        "overageDisabledReason": "org_level_disabled", "overageStatus": "rejected",
        "rateLimitType": "five_hour", "resetsAt": 1790631000, "status": "allowed",
        "unifiedWindows": {"five_hour": {"resetsAt": 1790631000, "utilization": 0.2},
                           "seven_day": {"resetsAt": 1791151200, "utilization": 0.03}}},
        "session_id": "57db2ec9-e6c0-4492-89d0-19f7070ee0db", "type": "rate_limit_event"});
    let p = threads::stream::RateLimitPush::from_line(&line).expect("read");
    assert_eq!(p.windows.len(), 2);
    assert!(!p.bars());
    let mut over = line.clone();
    over["rate_limit_info"]["isUsingOverage"] = json!(true);
    assert!(threads::stream::RateLimitPush::from_line(&over).unwrap().bars());
    let mut rej = line.clone();
    rej["rate_limit_info"]["status"] = json!("rejected");
    assert!(threads::stream::RateLimitPush::from_line(&rej).unwrap().bars());
}

// R9.F2b: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
