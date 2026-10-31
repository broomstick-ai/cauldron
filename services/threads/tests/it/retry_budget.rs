//! **The retry budgets are TIME, a server error is not a lost network, and a
//! retry does not restart the turn's clock.**
//!
//! Thread 60f0380d (2026-10-14, the "mishmash" thread): forty `server_error`
//! refusals whose prose read `Can't reach the API server … (ENOTFOUND)`. Three
//! defects compounded:
//!
//! 1. **The prose won over the producer's named cause**, so every refusal was
//!    classified as CONNECTIVITY and handed the forty-attempt network ladder.
//! 2. **That ladder was enforced as a COUNT**, though its own comment justified
//!    it as ~26 minutes of wall-clock patience. Each attempt ran ~3 minutes of
//!    real subagent work before failing, so forty attempts were 2½ days and
//!    $1,212.87.
//! 3. **Every retry restarted the turn's clock** (the execution lock's
//!    `acquired_at`, which the non-idle roster read first), so the thread read
//!    as freshly busy on every attempt rather than as stuck.
//!
//! One section per defect. Each end-to-end test drives the manager in-process
//! against the FAKE claude fixture; no real claude is spawned.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use threads::event::{Push, StoredEvent, ThreadEvent};
use threads::retry::{
    classify_api_error, connectivity_exhausted, server_error_exhausted, RetryDecision,
    TransientKind, CONNECTIVITY_BUDGET_SECS, MAX_CONNECTIVITY_ATTEMPTS,
    MAX_SERVER_ERROR_ATTEMPTS, SERVER_ERROR_BUDGET_SECS,
};
use threads::settings::ThreadSettings;
use threads::store::{DeliveryClock, RetryRow};
use threads::{build_manager, Config, ThreadManager};

/// The verbatim text of every one of 60f0380d's forty refusals.
const MISHMASH_TEXT: &str =
    "API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)";

// ════════════════════════════════════════════════════════════════════════════
// FIXTURES (the same shape as network_resilience.rs)
// ════════════════════════════════════════════════════════════════════════════

fn set_env() {
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake.to_string_lossy().to_string());
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "200");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &Path) -> Config {
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        dir.join("aa-owned").to_string_lossy().to_string(),
    );
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
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

fn watch(m: &ThreadManager) -> Arc<Mutex<Vec<StoredEvent>>> {
    let mut rx = m.subscribe();
    let seen = Arc::new(Mutex::new(Vec::<StoredEvent>::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Event(ev) = p {
                out.lock().unwrap().push(ev);
            }
        }
    });
    seen
}

fn kinds(seen: &Arc<Mutex<Vec<StoredEvent>>>) -> Vec<&'static str> {
    seen.lock().unwrap().iter().map(|e| e.event.kind()).collect()
}

fn count(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> usize {
    seen.lock().unwrap().iter().filter(|e| e.event.kind() == kind).count()
}

fn last_of(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> Option<ThreadEvent> {
    seen.lock()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e.event.kind() == kind)
        .map(|e| e.event.clone())
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..400 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

fn ago(secs: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339()
}

/// Stand a thread at a point in its retry ladder without driving the real
/// attempts that would take it there.
fn seed_retry(m: &ThreadManager, id: &str, prompt: &str, attempt: u32, kind: &str) {
    m.store
        .upsert_retry(&RetryRow {
            thread_id: id.to_string(),
            prompt: prompt.into(),
            message_ids: vec![1],
            attempt,
            kind: kind.into(),
            reason: "seeded".into(),
            account: Some("aa-owned".into()),
            created_at: String::new(),
        })
        .unwrap();
}

fn seed_streak(m: &ThreadManager, id: &str, kind: &str, since_secs_ago: i64) {
    m.store
        .put_delivery_clock(
            id,
            &[1],
            &DeliveryClock {
                turn_started_at: ago(since_secs_ago + 180),
                streak_kind: Some(kind.into()),
                streak_since: Some(ago(since_secs_ago)),
            },
        )
        .unwrap();
}

// ════════════════════════════════════════════════════════════════════════════
// (b) A SERVER ERROR IS ITS OWN CLASS, ON A SMALL BUDGET
// ════════════════════════════════════════════════════════════════════════════

/// **The producer's named cause outranks its prose.** 60f0380d's refusal was
/// `error: "server_error"` with network-sounding text; it was classified as
/// connectivity. It must be `ServerError`, and never connectivity.
#[test]
fn a_server_error_is_not_connectivity_even_when_its_prose_names_the_network() {
    let d = classify_api_error(Some("server_error"), None, MISHMASH_TEXT);
    match &d {
        RetryDecision::Retry { kind, .. } => {
            assert_eq!(*kind, TransientKind::ServerError, "got {d:?}");
            assert!(!kind.is_connectivity(), "a server error is not a lost network");
        }
        other => panic!("a server error must be retried on its own ladder, got {other:?}"),
    }
    assert_eq!(d.kind_str(), "server_error");

    // The other real `server_error` captures, and a bare 5xx status.
    for (error, status, text) in [
        (Some("server_error"), None, "API Error: Connection closed mid-response"),
        (Some("server_error"), Some(529), "API Error: 529 Overloaded"),
        (None, Some(503), "connection reset by peer"),
        (None, Some(500), "something went wrong"),
    ] {
        let d = classify_api_error(error, status, text);
        assert_eq!(d.kind_str(), "server_error", "{error:?}/{status:?}/{text:?} -> {d:?}");
    }

    // Account-scoped prose still wins: those ladders act on the ACCOUNT.
    assert!(matches!(
        classify_api_error(Some("server_error"), None, "Your credit balance is too low"),
        RetryDecision::CreditBalance { .. }
    ));

    // The wire tag round-trips, so a persisted row reads back as itself.
    assert_eq!(
        threads::retry::kind_from_str("server_error"),
        Some(TransientKind::ServerError)
    );
}

/// The budget is small, in attempts AND in time.
#[test]
fn the_server_error_budget_is_a_few_attempts_or_a_few_minutes() {
    assert!(MAX_SERVER_ERROR_ATTEMPTS <= 3);
    assert!(SERVER_ERROR_BUDGET_SECS <= 10 * 60);
    assert!(!server_error_exhausted(1, 0));
    assert!(!server_error_exhausted(MAX_SERVER_ERROR_ATTEMPTS - 1, 0));
    assert!(server_error_exhausted(MAX_SERVER_ERROR_ATTEMPTS, 0), "attempt ceiling");
    assert!(server_error_exhausted(1, SERVER_ERROR_BUDGET_SECS), "time ceiling, even on attempt 1");
}

/// End to end: a fresh `server_error` is parked on the SERVER-ERROR ladder,
/// not the connectivity one.
#[tokio::test]
async fn a_live_server_error_is_retried_as_a_server_error() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    m.send(&id, "servererr please".into(), Default::default(), json!({}), false)
        .unwrap();
    assert!(
        eventually(|| count(&seen, "retry_scheduled") >= 1).await,
        "{:?}",
        kinds(&seen)
    );
    let row = m.store.get_retry(&id).unwrap().expect("the work is held");
    assert_eq!(
        row.kind, "server_error",
        "60f0380d's refusal must not ride the connectivity ladder"
    );
}

/// End to end: the third server error ends the delivery, visibly.
#[tokio::test]
async fn a_server_error_gives_up_at_its_attempt_ceiling() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    seed_retry(&m, &id, "servererr please", MAX_SERVER_ERROR_ATTEMPTS, "server_error");
    m.retry_sweep(threads::runtime::PassOrigin::Cadence);
    assert!(
        eventually(|| count(&seen, "turn_failed") >= 1).await,
        "attempt {MAX_SERVER_ERROR_ATTEMPTS} of a server error must be the last: {:?}",
        kinds(&seen)
    );
    let Some(ThreadEvent::TurnFailed { error_kind, .. }) = last_of(&seen, "turn_failed") else {
        panic!("no failure event")
    };
    assert_eq!(error_kind, "server_error");
    assert!(eventually(|| m.store.get_retry(&id).unwrap().is_none()).await);
}

/// End to end: a server error that has been running past its TIME budget
/// ends on its first failure inside that run, whatever the attempt count.
#[tokio::test]
async fn a_server_error_gives_up_on_elapsed_time_at_attempt_one() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    seed_streak(&m, &id, "server_error", SERVER_ERROR_BUDGET_SECS as i64 + 60);
    seed_retry(&m, &id, "servererr please", 1, "server_error");
    m.retry_sweep(threads::runtime::PassOrigin::Cadence);
    assert!(
        eventually(|| count(&seen, "turn_failed") >= 1).await,
        "{:?}",
        kinds(&seen)
    );
}

// ════════════════════════════════════════════════════════════════════════════
// (a) THE CONNECTIVITY BUDGET IS WALL-CLOCK, NOT A COUNT
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn the_connectivity_budget_is_twenty_six_minutes_of_wall_clock() {
    assert_eq!(CONNECTIVITY_BUDGET_SECS, 26 * 60);
    assert!(!connectivity_exhausted(0));
    assert!(!connectivity_exhausted(CONNECTIVITY_BUDGET_SECS - 1));
    assert!(connectivity_exhausted(CONNECTIVITY_BUDGET_SECS));
}

/// **Past the time budget, a connectivity failure ends the delivery even at
/// attempt 2.** Under the count this would have scheduled attempt 3 of 40.
#[tokio::test]
async fn connectivity_gives_up_on_elapsed_time_even_at_a_low_attempt_count() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    seed_streak(&m, &id, "connectivity", CONNECTIVITY_BUDGET_SECS as i64 + 4 * 60);
    seed_retry(&m, &id, "netresult please", 2, "connectivity");
    m.retry_sweep(threads::runtime::PassOrigin::Cadence);
    assert!(
        eventually(|| count(&seen, "turn_failed") >= 1).await,
        "30 minutes of a lost network is past the budget, whatever the count: {:?}",
        kinds(&seen)
    );
    let Some(ThreadEvent::TurnFailed { error_kind, message }) = last_of(&seen, "turn_failed")
    else {
        panic!("no failure event")
    };
    assert_eq!(error_kind, "connectivity");
    assert!(message.contains("30 minutes"), "the minutes are MEASURED: {message}");
    assert_eq!(count(&seen, "retry_scheduled"), 0, "{:?}", kinds(&seen));
}

/// **Inside the time budget, the attempt count does not end it** — attempt 45
/// is past the old forty-attempt ceiling and must still be retried.
#[tokio::test]
async fn connectivity_keeps_retrying_past_the_old_attempt_ceiling_inside_its_budget() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let attempt = MAX_CONNECTIVITY_ATTEMPTS + 5;
    let seen = watch(&m);
    seed_streak(&m, &id, "connectivity", 60);
    seed_retry(&m, &id, "netresult please", attempt, "connectivity");
    m.retry_sweep(threads::runtime::PassOrigin::Cadence);
    assert!(
        eventually(|| count(&seen, "retry_scheduled") >= 1).await,
        "one minute into an outage is inside the budget: {:?} / {:?}",
        kinds(&seen),
        last_of(&seen, "turn_failed")
    );
    assert_eq!(count(&seen, "turn_failed"), 0, "{:?}", kinds(&seen));
    assert_eq!(m.store.get_retry(&id).unwrap().unwrap().attempt, attempt + 1);
}

/// A different class starts a new run: an hour behind a server error does not
/// spend the connectivity budget.
#[tokio::test]
async fn a_change_of_class_restarts_the_time_budget() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    seed_streak(&m, &id, "server_error", 3600);
    seed_retry(&m, &id, "netresult please", 2, "server_error");
    m.retry_sweep(threads::runtime::PassOrigin::Cadence);
    assert!(
        eventually(|| count(&seen, "retry_scheduled") >= 1).await,
        "{:?}",
        kinds(&seen)
    );
    assert_eq!(count(&seen, "turn_failed"), 0);
}

// ════════════════════════════════════════════════════════════════════════════
// (c) A RETRY DOES NOT RESET THE TURN'S started_at
// ════════════════════════════════════════════════════════════════════════════

/// **The delivery's start survives its retries**, on the durable clock and on
/// the non-idle roster's `since`. Before, a retried attempt took a fresh
/// execution lock, the roster read the lock's `acquired_at` first, and a thread
/// forty attempts deep looked as if it had just started.
#[tokio::test]
async fn a_retry_does_not_reset_the_turns_started_at() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    m.send(&id, "netdown please".into(), Default::default(), json!({}), false)
        .unwrap();
    assert!(eventually(|| count(&seen, "retry_scheduled") >= 1).await);
    let first = m
        .store
        .delivery_clock(&id)
        .unwrap()
        .expect("the delivery has a clock")
        .turn_started_at;

    // A real retry: a second spawn, a second lock, a second failure.
    tokio::time::sleep(Duration::from_millis(50)).await;
    m.retry_sweep(threads::runtime::PassOrigin::Cadence);
    assert!(
        eventually(|| count(&seen, "retry_scheduled") >= 2).await,
        "{:?}",
        kinds(&seen)
    );
    assert_eq!(count(&seen, "turn_started"), 2, "two real attempts ran");

    let clock = m.store.delivery_clock(&id).unwrap().unwrap();
    assert_eq!(clock.turn_started_at, first, "a retry must not move the turn's start");

    let since = m
        .thread_rows_non_idle()
        .unwrap()
        .rows
        .into_iter()
        .find(|r| r.thread_id == id)
        .and_then(|r| r.since)
        .expect("a retrying thread is non-idle and carries a since");
    let first_t = chrono::DateTime::parse_from_rfc3339(&first).unwrap();
    assert_eq!(
        since.timestamp_millis(),
        first_t.timestamp_millis(),
        "the roster's since is the FIRST attempt's start, not the latest retry's"
    );
}

/// A different delivery DOES start a new clock — the start is per delivery,
/// not per thread.
#[test]
fn a_new_delivery_starts_a_new_clock_and_the_same_one_keeps_it() {
    let tmp = tempfile::tempdir().unwrap();
    let store = threads::store::Store::open(tmp.path().join("t.db")).unwrap();
    store
        .put_delivery_clock(
            "t",
            &[7, 8],
            &DeliveryClock {
                turn_started_at: "2026-10-12T00:00:00+00:00".into(),
                streak_kind: Some("connectivity".into()),
                streak_since: Some("2026-10-12T00:03:00+00:00".into()),
            },
        )
        .unwrap();
    // Same delivery (a retry; or a superset after a fold-in): kept.
    let c = store.begin_delivery_attempt("t", &[7, 8, 9]).unwrap();
    assert_eq!(c.turn_started_at, "2026-10-12T00:00:00+00:00");
    assert_eq!(store.note_delivery_failure("t", "connectivity").unwrap(), "2026-10-12T00:03:00+00:00");
    // A different class: a new streak.
    assert_ne!(store.note_delivery_failure("t", "server_error").unwrap(), "2026-10-12T00:03:00+00:00");
    // A different delivery: a new clock.
    let c = store.begin_delivery_attempt("t", &[10]).unwrap();
    assert_ne!(c.turn_started_at, "2026-10-12T00:00:00+00:00");
    assert_eq!(c.streak_kind, None);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
