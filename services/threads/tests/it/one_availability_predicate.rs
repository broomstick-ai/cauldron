//! **ONE AVAILABILITY PREDICATE, AND AN UNAVAILABLE ACCOUNT IS NEVER TRIED (H179).**
//!
//! The operator, 2026-10-16: cc2's login showed expired, frozen and wiped on
//! the dashboard, and placement still put mishmash there first, falling back to
//! cc1 only after the turn failed on the credential. *"That should not have
//! happened… Why did placement even try to place there? And why didn't it try
//! placing somewhere else?"* Disabled, a wiped / expired / stranded login, an
//! auth failure and an unpaid / 401 / 403 / billing refusal all mean "account
//! not available", and an unavailable account is never tried — on a new
//! placement, a resume, a re-place after a restart, or a subagent spawn.
//!
//! THE OBSERVABLE, in every test: the "unavailable" account `aa-bad` has NO
//! `auth-live` file, so the fixture fails any turn spawned there with the
//! real CLI's `authentication_failed` shape, which writes a `login` bar on it.
//! `zz-good` has one and answers normally. So "a login bar appeared on aa-bad"
//! is exactly "a turn was TRIED on aa-bad". The ranking is rigged toward
//! `aa-bad` (0% against 80%), so nothing but the predicate keeps work off it.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use threads::gauge::Axis;
use threads::{build_manager, Config, SendIntent, ThreadManager, ThreadSettings};

const BAD: &str = "aa-bad";
const GOOD: &str = "zz-good";
const PROMPT: &str = "authresult please";

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env() {
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "200");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("THREADS_EDGE_PAIR_GAP_MS", "10");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &Path) -> Config {
    let mut accounts = BTreeMap::new();
    for name in [BAD, GOOD] {
        let cd = dir.join(name);
        std::fs::create_dir_all(&cd).unwrap();
        accounts.insert(name.to_string(), cd.to_string_lossy().to_string());
    }
    // Only the good account's login answers.
    std::fs::write(dir.join(GOOD).join("auth-live"), "").unwrap();
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

/// Rig the ranker toward `aa-bad`: untouched against 80% consumed.
fn rig(m: &ThreadManager) {
    for (a, pct) in [(BAD, 0.0), (GOOD, 80.0)] {
        m.budgets
            .observe(a, Axis::WeeklyAll, pct, Some("2099-01-01T00:00:00Z".into()), "usage_poll")
            .unwrap();
        m.budgets.observe(a, Axis::Session, pct, None, "usage_poll").unwrap();
    }
}

/// `aa-bad`'s slot reads `health`; every other slot reads Live.
fn probe(m: &ThreadManager, health: threads::accounts::SlotHealth) {
    m.set_slot_probe_for_test(Arc::new(move |label: &str, _dir: Option<&str>| {
        if label == BAD {
            health.clone()
        } else {
            threads::accounts::SlotHealth::Live
        }
    }));
}

/// Was a turn ever TRIED on `aa-bad`? Only a spawn there can write this bar.
fn tried_on_bad(m: &ThreadManager) -> Option<String> {
    m.store
        .list_account_states()
        .unwrap()
        .into_iter()
        .find(|s| s.name == BAD)
        .filter(|s| s.limit_axis.as_deref() == Some("login"))
        .map(|s| format!("login bar on {BAD}: {:?} {:?}", s.limit_until, s.last_limit_prose))
}

fn ran_on(m: &ThreadManager, id: &str, account: &str) -> bool {
    m.store
        .thread_accounts(id)
        .unwrap()
        .contains(&account.to_string())
        && !m.is_locked(id)
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..240 {
        if f() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    f()
}

// ── 1. NEW PLACEMENT ────────────────────────────────────────────────────────

/// A fresh thread is placed on an available account even though the ranking
/// prefers the one whose login reads EXPIRED.
///
/// FORCED RED without the fix: `SlotHealth::needs_sign_in` was `Wiped` only, so
/// an expired slot was a candidate, won the rigged ranking, and its first turn
/// failed on the credential before the fallback.
#[tokio::test]
async fn a_new_placement_never_tries_an_expired_login() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    probe(&m, threads::accounts::SlotHealth::Expired);
    rig(&m);
    m.recover().await;
    m.start_watchers();

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    m.send(&id, PROMPT.into(), SendIntent::Immediate, json!({}), false).unwrap();
    assert!(
        eventually(|| ran_on(&m, &id, GOOD)).await,
        "the turn runs on the available account. account={:?} park={:?}",
        m.serving_account(&id).unwrap(),
        m.park_state(&id).unwrap()
    );
    assert_eq!(tried_on_bad(&m), None, "an expired login is never tried");
    assert!(
        !m.store.thread_accounts(&id).unwrap().contains(&BAD.to_string()),
        "and no conversation was ever established there"
    );
}

// ── 2. RESUME: the account a thread already sits on becomes unavailable ──────

/// A thread whose whole conversation is on an account the operator then
/// DISABLES (an unpaid account) is moved, conversation carried, BEFORE its next
/// turn — it is not run there first.
///
/// FORCED RED without the fix: a disable only removed the account from new
/// placement, and "work already on a disabled account finishes there", so the
/// next turn ran on it, failed, and only then moved.
#[tokio::test]
async fn a_resume_never_tries_a_disabled_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    probe(&m, threads::accounts::SlotHealth::Live);
    rig(&m);
    m.start_watchers();
    // While it is still paid for, `aa-bad` answers.
    std::fs::write(tmp.path().join(BAD).join("auth-live"), "").unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), BAD, "fixture: ranked there");
    m.send(&id, PROMPT.into(), SendIntent::Immediate, json!({}), false).unwrap();
    assert!(eventually(|| ran_on(&m, &id, BAD)).await, "fixture: one turn there");
    assert_eq!(
        m.store.thread_accounts(&id).unwrap(),
        vec![BAD.to_string()],
        "fixture: its whole conversation is on the account about to be disabled"
    );

    // He stops paying for it and disables it. Its login stops answering.
    std::fs::remove_file(tmp.path().join(BAD).join("auth-live")).unwrap();
    m.store.set_account_disabled(BAD, true, Some("unpaid")).unwrap();

    m.send(&id, "authresult again".into(), SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| ran_on(&m, &id, GOOD)).await,
        "the next turn runs on the available account. account={:?} park={:?}",
        m.serving_account(&id).unwrap(),
        m.park_state(&id).unwrap()
    );
    assert_eq!(tried_on_bad(&m), None, "a disabled account is never tried");
}

// ── 3. RE-PLACE AFTER A RESTART ────────────────────────────────────────────

/// The live 20:40:42Z shape: the daemon restarts with a wake waiting for a
/// thread whose conversation sits on a slot whose login expired while it was
/// down. The boot reading puts the slot out of placement, and the waiting turn
/// is re-placed straight onto the available account.
///
/// FORCED RED without the fix: the boot seed only knew `Wiped`, so an expired
/// slot was a placement candidate at boot and the waiting turn was run there
/// first.
#[tokio::test]
async fn a_resume_after_restart_never_tries_an_expired_login() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let id = {
        keychain::install(); // CON-28: no real keychain (R9.F2d)
        let m1 = build_manager(config_at(tmp.path())).unwrap();
        probe(&m1, threads::accounts::SlotHealth::Live);
        rig(&m1);
        std::fs::write(tmp.path().join(BAD).join("auth-live"), "").unwrap();
        let id = m1.create_thread(None, settings(tmp.path())).unwrap();
        assert_eq!(m1.serving_account(&id).unwrap(), BAD, "fixture: ranked there");
        m1.send(&id, PROMPT.into(), SendIntent::Immediate, json!({}), false).unwrap();
        assert!(eventually(|| ran_on(&m1, &id, BAD)).await, "fixture: one turn there");
        // The wake that is waiting when the daemon goes down: queued behind a
        // held lock, so nothing in THIS process dispatches it.
        let held = m1.try_lock_thread(&id, "test").unwrap();
        m1.send(&id, "authresult wake".into(), SendIntent::Immediate, json!({}), false)
            .unwrap();
        held.release();
        assert_eq!(m1.store.waiting_queue(&id).unwrap().len(), 1, "fixture: one waiting wake");
        id
    };

    // DOWN. The slot's login expires while nothing is watching.
    std::fs::remove_file(tmp.path().join(BAD).join("auth-live")).unwrap();

    // UP: a new manager over the same data, exactly as a restart builds one.
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m2 = build_manager(config_at(tmp.path())).unwrap();
    probe(&m2, threads::accounts::SlotHealth::Expired);
    m2.recover().await;
    m2.start_watchers();
    assert!(
        eventually(|| ran_on(&m2, &id, GOOD) && m2.store.waiting_queue(&id).unwrap().is_empty())
            .await,
        "the waiting wake is re-placed onto the available account. account={:?} park={:?}",
        m2.serving_account(&id).unwrap(),
        m2.park_state(&id).unwrap()
    );
    assert_eq!(tried_on_bad(&m2), None, "an expired login is never tried after a restart");
}

// ── 4. SUBAGENT SPAWN ──────────────────────────────────────────────────────

/// A subagent is created through the same door as any thread and inherits its
/// parent's `Auto` placement: it lands on an available account, never on one
/// whose login is WIPED-and-DISABLED (the cc2 / owner-account state), even when the
/// ranking prefers it.
///
/// FORCED RED without the fix: an EXPIRED slot passed `selectable_pool`, so the
/// child was created there and its task failed before any fallback.
#[tokio::test]
async fn a_subagent_spawn_never_tries_an_unavailable_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    probe(&m, threads::accounts::SlotHealth::Expired);
    rig(&m);
    m.recover().await;
    m.start_watchers();

    let mut ps = settings(tmp.path());
    ps.account = threads::settings::AccountChoice::Named(GOOD.into());
    let parent = m.create_thread(None, ps).unwrap();
    // Hand the parent back to the ranker, the way AUI threads are placed.
    m.update_settings(
        &parent,
        threads::settings::SettingsPatch {
            account: Some(threads::settings::AccountChoice::Auto),
            ..Default::default()
        },
    )
    .unwrap();
    let reply = threads::managed::spawn(
        &m,
        &json!({"caller": parent, "task": "authresult child task", "notify": "hold"}),
    )
    .unwrap();
    let child = reply["thread_id"].as_str().unwrap().to_string();
    assert!(
        eventually(|| ran_on(&m, &child, GOOD)).await,
        "the child's task runs on the available account. account={:?} park={:?}",
        m.serving_account(&child).unwrap(),
        m.park_state(&child).unwrap()
    );
    assert_eq!(tried_on_bad(&m), None, "a subagent never tries an unavailable account");
}

// ── 5. AN UNGATED THREAD WITH NOWHERE ELSE TO GO PARKS ─────────────────────

/// When every OTHER account is inside a usage window, the ungated escape hatch
/// used to run a thread "where it is". Where it is is unavailable (disabled),
/// so it parks instead — and is never tried there.
///
/// FORCED RED without the fix: the hatch refused only a WIPED slot; a disabled
/// one was run.
#[tokio::test]
async fn the_escape_hatch_never_runs_a_thread_on_an_unavailable_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    probe(&m, threads::accounts::SlotHealth::Live);
    rig(&m);
    m.start_watchers();
    std::fs::write(tmp.path().join(BAD).join("auth-live"), "").unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    m.send(&id, PROMPT.into(), SendIntent::Immediate, json!({}), false).unwrap();
    assert!(eventually(|| ran_on(&m, &id, BAD)).await, "fixture: one turn there");

    std::fs::remove_file(tmp.path().join(BAD).join("auth-live")).unwrap();
    m.store.set_account_disabled(BAD, true, Some("unpaid")).unwrap();
    m.store
        .set_account_limit(
            GOOD,
            Some(("2099-01-01T00:00:00+00:00", Some("limit"), Some("session"), None)),
        )
        .unwrap();
    m.send(&id, "authresult stranded".into(), SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| m.park_state(&id).unwrap() != Value::Null).await,
        "it parks: {:?}",
        m.park_state(&id).unwrap()
    );
    for _ in 0..10 {
        m.placement_pass(threads::runtime::PassOrigin::Cadence);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(tried_on_bad(&m), None, "never run on the disabled account");
}

// ── 6. THE DASHBOARD RENDERS THE SAME PREDICATE ─────────────────────────────

/// The `accounts` shape's `pressure` row carries the predicate's verdict, so the
/// board cannot draw an account WIPED / EXPIRED / DISABLED beside a pressure row
/// that says it is ranked and available.
///
/// FORCED RED without the fix: the row had no `available` field, and an expired
/// slot's row read `unavailable: false, ranked: true`.
#[tokio::test]
async fn the_dashboard_row_is_the_placement_predicate() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path())).unwrap();
    probe(&m, threads::accounts::SlotHealth::Expired);
    rig(&m);
    m.recover().await;
    m.store.set_account_disabled(BAD, true, Some("unpaid")).unwrap();
    let (addr, _h) =
        threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

    let predicate = m.account_unavailability();
    assert_eq!(
        predicate.get(BAD),
        Some(&vec!["disabled".to_string(), "login_expired".to_string()]),
        "{predicate:?}"
    );
    assert!(!predicate.contains_key(GOOD), "{predicate:?}");

    let bad = published_row(addr, BAD).await;
    assert_eq!(bad["pressure"]["available"], json!(false), "{bad}");
    assert_eq!(
        bad["pressure"]["unavailable_reasons"],
        json!(["disabled", "login_expired"]),
        "{bad}"
    );
    assert_eq!(bad["pressure"]["unavailable"], json!(true), "{bad}");
    assert_eq!(bad["pressure"]["ranked"], json!(false), "{bad}");
    let good = published_row(addr, GOOD).await;
    assert_eq!(good["pressure"]["available"], json!(true), "{good}");
    assert_eq!(good["pressure"]["unavailable_reasons"], json!([]), "{good}");
}

async fn published_row(addr: std::net::SocketAddr, key: &str) -> Value {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    ws.send(Message::Text(
        json!({"type":"request","id":"1","method":"shapes/accounts/subscribe",
               "params":{"key":key,
                         "consumer":{"name":"threads-tests","shape_version":"shapes.md@rev4"}}})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    for _ in 0..40 {
        let got = tokio::time::timeout(std::time::Duration::from_millis(1500), ws.next()).await;
        let Ok(Some(Ok(Message::Text(t)))) = got else {
            break;
        };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["type"] == "shape" && v["op"] == "read" && v["key"] == json!(key) {
            return v["row"].clone();
        }
    }
    panic!("no `accounts` export for {key} arrived");
}

// ── 7. 401 / 403 / 402 / BILLING ARE FACTS ABOUT THE ACCOUNT ─────────────────

/// The operator: "unpaid / 401 / 403 / billing errors" make an account
/// unavailable. Each must classify as ACCOUNT-scoped — `LoginExpired` (barred,
/// re-placed) or `CreditBalance` (barred, re-placed) — never as a `Fatal`
/// request error that is retried on the same account.
///
/// FORCED RED without the fix: a bare 401 / 403 / 402 fell to `Fatal`, and the
/// named `billing_error` / `permission_error` / `authentication_error` to the
/// prose classifier, which found nothing account-scoped in an empty message.
#[test]
fn unpaid_401_403_and_billing_refusals_bar_the_account() {
    use threads::retry::{classify_api_error, RetryDecision};
    for (error, status) in [
        (None, Some(401)),
        (None, Some(403)),
        (Some("authentication_error"), None),
        (Some("permission_error"), None),
    ] {
        assert!(
            matches!(classify_api_error(error, status, ""), RetryDecision::LoginExpired { .. }),
            "{error:?}/{status:?}: {:?}",
            classify_api_error(error, status, "")
        );
    }
    for (error, status) in [(None, Some(402)), (Some("billing_error"), None)] {
        assert!(
            matches!(classify_api_error(error, status, ""), RetryDecision::CreditBalance { .. }),
            "{error:?}/{status:?}: {:?}",
            classify_api_error(error, status, "")
        );
    }
    // A bad REQUEST is still not an account fact.
    assert!(matches!(
        classify_api_error(None, Some(400), "prompt is too long"),
        RetryDecision::Fatal { .. }
    ));
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
