//! **A successful login reopens the account immediately, and clears exactly the
//! bar it is evidence about.**
//!
//! > *"if we're asking for the login codes from threads by using that [force]
//! > function I just mentioned, then we'll know when the login has succeeded and
//! > you can drop that flag and immediately add that account to the pool as
//! > available."*
//!
//! The login flow is going to be issued THROUGH threads, so threads observes
//! the success directly — there is nothing to wait for and no separate
//! confirmation step. `ThreadManager::note_login_restored` is the hook that flow
//! calls; building the flow itself is separate work and is not done here.
//!
//! Two properties are load-bearing and each has a test that fails without it:
//!
//! 1. **It clears the login bar on every label of the login, at once**, without
//!    waiting for the recheck window — which is the whole point.
//! 2. **It clears NOTHING ELSE.** A successful `/login` is evidence about the
//!    CREDENTIAL. It says nothing about quota, so a usage-limit bar on the same
//!    account is left standing. Clearing it would be the same class-loss this
//!    audit exists to remove, running in the other direction: treating one
//!    axis's good news as another's.
//!
//! The automatic path is untouched — the bar still expires on its own ladder
//! and a completed turn still clears it. *"we should still do the automatic
//! tries, you know, just as a backup."*

use std::collections::BTreeMap;
use std::path::Path;

use threads::accounts::AccountState;
use threads::{build_manager, Config, ThreadManager};

#[path = "support/fake_login.rs"]
mod fake_login;

/// R12.5b: a reported login clears only after the re-check's trusted read
/// verifies the slot, so every test here that REPORTS a login first gives the
/// slot a credential the (fake) server reads as its own address. The opposing
/// cases — crossed and unreadable reports clear nothing — are
/// `tests/daemon_login_guard.rs` f2/f3.
fn verified(m: &ThreadManager, name: &str) {
    fake_login::sign_in_verified(m, name, &format!("{name}@x.test"));
}

const LOGIN_A: &str = "d11af2d3.683974df";
const LOGIN_B: &str = "c127c516.5810305a";

fn config_at(dir: &Path, extra: &[&str]) -> Config {
    let mut accounts = BTreeMap::new();
    for name in extra {
        accounts.insert(name.to_string(), dir.join(name).to_string_lossy().to_string());
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

fn identity(m: &ThreadManager, name: &str, fp: &str) {
    m.store.set_account_identity(name, None, Some(fp)).unwrap();
}

fn far_future() -> String {
    "2099-01-01T00:00:00+00:00".to_string()
}

/// Record a bar on a label, on a named axis, stamped with a login.
fn bar(m: &ThreadManager, name: &str, axis: &str, on: &str) {
    m.store
        .set_account_limit(
            name,
            Some((&far_future(), Some("reason"), Some(axis), Some(on))),
        )
        .unwrap();
}

fn state_of(m: &ThreadManager, name: &str) -> AccountState {
    m.store
        .list_account_states()
        .unwrap()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no accounts row for {name}"))
}

fn is_barred(m: &ThreadManager, name: &str) -> bool {
    !state_of(m, name).available(&threads::store::now_rfc3339())
}

/// **The hook reopens the account on the spot.**
///
/// Without `note_login_restored` the only way out of a login bar is the clock
/// (`retry::login_bar_secs`) or a completed turn. Both are backups; neither is
/// immediate, and neither can be triggered by the thing that actually knows the
/// login succeeded.
#[test]
fn a_successful_login_reopens_the_account_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["acct-a", "acct-a-alias"])).unwrap();
    identity(&m, "acct-a", LOGIN_A);
    verified(&m, "acct-a");
    identity(&m, "acct-a-alias", LOGIN_A);

    bar(&m, "acct-a", "login", LOGIN_A);
    bar(&m, "acct-a-alias", "login", LOGIN_A);
    assert!(is_barred(&m, "acct-a"), "the bar must be in place first");
    assert!(is_barred(&m, "acct-a-alias"));

    let reopened = m.note_login_restored("acct-a");

    assert!(
        !is_barred(&m, "acct-a"),
        "a successful login must reopen the account NOW — its bar is still recorded, so \
         nothing but this hook has run"
    );
    assert!(
        !is_barred(&m, "acct-a-alias"),
        "the bar was written on every LABEL of the login, so the clear must be exactly as \
         wide as the bar it removes"
    );
    assert_eq!(
        reopened,
        vec!["acct-a".to_string(), "acct-a-alias".to_string()],
        "the hook reports what it reopened so the caller can say so"
    );
}

/// **THE AXIS, again.** A login is not a quota, and good news about one is not
/// good news about the other.
///
/// This is the test that fails if `note_login_restored` is written as "clear
/// the bar" instead of "clear the LOGIN bar". `note_account_served` may clear a
/// quota bar because a completed turn genuinely falsifies one — the account
/// served, so it was not out of quota. A `/login` proves nothing of the kind,
/// and clearing a real weekly limit on the strength of it would put the next
/// turn straight back into a 429.
#[test]
fn a_successful_login_does_not_clear_a_usage_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["acct-a"])).unwrap();
    identity(&m, "acct-a", LOGIN_A);
    verified(&m, "acct-a");

    bar(&m, "acct-a", "weekly", LOGIN_A);
    assert!(is_barred(&m, "acct-a"));

    let reopened = m.note_login_restored("acct-a");

    assert!(
        is_barred(&m, "acct-a"),
        "a successful login must NOT lift a USAGE limit — it is evidence about the \
         credential and says nothing whatever about quota"
    );
    assert!(
        reopened.is_empty(),
        "nothing was reopened, and the hook must say so rather than claim a clear it did \
         not make: {reopened:?}"
    );
}

/// It touches only the login it was told about. A second, unrelated login stays
/// barred.
#[test]
fn a_successful_login_reopens_only_that_login() {
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["acct-a", "acct-b"])).unwrap();
    identity(&m, "acct-a", LOGIN_A);
    verified(&m, "acct-a");
    identity(&m, "acct-b", LOGIN_B);

    bar(&m, "acct-a", "login", LOGIN_A);
    bar(&m, "acct-b", "login", LOGIN_B);

    m.note_login_restored("acct-a");

    assert!(!is_barred(&m, "acct-a"));
    assert!(
        is_barred(&m, "acct-b"),
        "a different login's bar is not evidence about this one"
    );
}

/// Idempotent and quiet. The login flow may fire this whenever it sees a
/// success without first checking whether anything needed reopening.
#[test]
fn reopening_an_unbarred_account_is_a_quiet_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["acct-a"])).unwrap();
    identity(&m, "acct-a", LOGIN_A);
    verified(&m, "acct-a");

    assert!(m.note_login_restored("acct-a").is_empty());
    assert!(!is_barred(&m, "acct-a"));
    // Twice, for the same reason.
    assert!(m.note_login_restored("acct-a").is_empty());
    assert!(!is_barred(&m, "acct-a"));
}

/// A successful login also clears the PERMANENT flag, not just the expiring
/// bar. `dead_since` is the same fact in its non-expiring form; a hook that
/// lifted one and left the other would leave the account out of the pool while
/// reporting that it had reopened it.
#[test]
fn a_successful_login_clears_the_dead_login_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["acct-a"])).unwrap();
    identity(&m, "acct-a", LOGIN_A);
    verified(&m, "acct-a");

    m.store
        .set_account_health(
            "acct-a",
            Some((
                &threads::store::now_rfc3339(),
                "OAuth session expired and could not be refreshed",
            )),
        )
        .unwrap();
    assert!(!state_of(&m, "acct-a").healthy(), "must be dead first");

    m.note_login_restored("acct-a");

    assert!(
        state_of(&m, "acct-a").healthy(),
        "a successful login falsifies `dead_since` — it is the permanent form of exactly \
         the fact the login just disproved"
    );
}
