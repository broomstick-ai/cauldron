//! **A-1 (R9.A1): A SLOT THAT GOES EXPIRED / WIPED / STRANDED MID-DAY BARS
//! PLACEMENT WITHIN ONE CREDENTIAL SAMPLE — NOT AFTER A FAILED TURN.**
//!
//! `docs/GAP-AUDIT.md` A-1: the board drew EXPIRED within five minutes of a
//! refresh token expiring mid-day, but the sign-in set placement reads was
//! written only at boot and after an auth failure, so placement kept choosing
//! the account until a turn failed on the credential — the H179 incident again,
//! after boot instead of at boot.
//!
//! THE OBSERVABLE: the ranking is rigged toward `aa-bad` (0% against 80%), so a
//! fresh thread lands there unless the one availability predicate bars it. No
//! turn is ever run in the barring tests: the bar must come from the SAMPLE.
//!
//! The sample period is shrunk to 200 ms (`THREADS_CREDENTIAL_SAMPLE_MS`,
//! doctrine 4); the negative test outlasts five of those periods.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use threads::accounts::SlotHealth;
use threads::gauge::Axis;
use threads::{build_manager, Config, ThreadManager, ThreadSettings};

const BAD: &str = "aa-bad";
const GOOD: &str = "zz-good";
const SAMPLE_MS: u64 = 200;

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
    std::env::set_var("THREADS_CREDENTIAL_SAMPLE_MS", SAMPLE_MS.to_string());
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
        std::fs::write(cd.join("auth-live"), "").unwrap();
        accounts.insert(name.to_string(), cd.to_string_lossy().to_string());
    }
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
fn probe(m: &ThreadManager, health: SlotHealth) {
    m.set_slot_probe_for_test(Arc::new(move |label: &str, _dir: Option<&str>| {
        if label == BAD {
            health.clone()
        } else {
            SlotHealth::Live
        }
    }));
}

/// Where a brand-new thread is placed right now.
fn placed(m: &ThreadManager, cwd: &Path) -> String {
    let id = m.create_thread(None, settings(cwd)).unwrap();
    m.serving_account(&id).unwrap()
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

/// A slot that reads Live at boot flips to EXPIRED with no turn run anywhere.
/// After one sample tick it is out of placement, with the predicate's word.
///
/// FORCED RED at 97614bfd: `sample_credentials` only emitted
/// `CredentialChanged`, the sign-in set was written only at boot and after an
/// auth failure, so `aa-bad` stayed available and won the rigged ranking.
#[tokio::test]
async fn a_slot_expiring_between_samples_is_barred_after_one_200ms_sample_tick() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    probe(&m, SlotHealth::Live);
    rig(&m);
    m.recover().await;
    assert_eq!(placed(&m, tmp.path()), BAD, "fixture: Live at boot, ranked first");

    // The refresh token expires mid-day. No turn runs.
    probe(&m, SlotHealth::Expired);
    assert!(
        eventually(|| m.account_unavailable_reasons(BAD).is_some()).await,
        "one credential sample bars the expired slot without a turn failing on it"
    );
    assert_eq!(
        m.account_unavailable_reasons(BAD),
        Some(vec!["login_expired".to_string()]),
        "in the predicate's own word"
    );
    assert_eq!(placed(&m, tmp.path()), GOOD, "placement refuses it");
    assert!(
        m.store
            .list_account_states()
            .unwrap()
            .into_iter()
            .all(|s| !(s.name == BAD && s.limit_axis.as_deref() == Some("login"))),
        "no auth failure wrote the bar: the sample did"
    );
}

/// Paired (doctrine 2): a slot that stays Live stays placeable across five
/// sample periods — the sampler does not bar everything it reads.
#[tokio::test]
async fn a_live_slot_stays_placeable_across_five_200ms_sample_ticks() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    probe(&m, SlotHealth::Live);
    rig(&m);
    m.recover().await;
    tokio::time::sleep(std::time::Duration::from_millis(5 * SAMPLE_MS + 100)).await;
    assert_eq!(m.account_unavailable_reasons(BAD), None, "Live is available");
    assert_eq!(placed(&m, tmp.path()), BAD, "and still ranked first");
}

/// The SAMPLE PATH both bars and lifts, with nothing else running: no watchers
/// (so no 30 s recheck, no bar-expiry tick), no turn. WIPED, EXPIRED and
/// STRANDED each bar with their own word; reading Live again lifts the bar on
/// the very next sample.
///
/// FORCED RED at 97614bfd on the first assertion of each round: the sample
/// wrote nothing placement reads.
#[tokio::test]
async fn the_sample_bars_each_sign_in_reading_and_lifts_it_when_live_again() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    rig(&m);
    for (health, word) in [
        (SlotHealth::Expired, "login_expired"),
        (SlotHealth::Wiped, "login_wiped"),
        (SlotHealth::Stranded, "login_stranded"),
    ] {
        probe(&m, health);
        m.sample_credentials();
        assert_eq!(
            m.account_unavailable_reasons(BAD),
            Some(vec![word.to_string()]),
            "one sample bars a slot reading {word}"
        );
        assert_eq!(placed(&m, tmp.path()), GOOD, "placement refuses {word}");

        probe(&m, SlotHealth::Live);
        m.sample_credentials();
        assert_eq!(
            m.account_unavailable_reasons(BAD),
            None,
            "the next sample reading Live lifts {word}"
        );
        assert_eq!(placed(&m, tmp.path()), BAD, "and placement takes it again");
    }
}

// R9.F2b: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
