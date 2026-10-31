//! **The mirror of the alias collapse: a PLACEABLE account that shares a login
//! with the UNPLACEABLE machine default.**
//!
//! `564386f` fixed the original: alias collapse picked the alphabetically first
//! label as canonical, which made the machine login (`default`) the canonical
//! name for an account threads owned, and produced a 61%/8% consumption skew.
//! The fix was structural — a `Placeable` newtype with no infallible
//! constructor, and a `placeable_pool` that DROPS `default` before it collapses
//! rather than after.
//!
//! On 2026-08-06 the operator signed his machine login into a brand-new account
//! and produced the mirror configuration: `alice-example-com` — placeable, and
//! the only unlimited account he had — came to share the identity fingerprint
//! `476d69df.684c8b5d` with `default`. Every live thread was running on it. The
//! prior suite never constructed this case: `tests/default_out_of_placement.rs`
//! guards the TYPE and contains no shared-identity fixture at all.
//!
//! What these tests establish, in two halves:
//!
//! * **Placement is safe, and safe by construction rather than by luck.**
//!   Dropping before collapsing means `default` cannot be a unit's canonical
//!   whatever the alphabet says — and the alphabet does say different things
//!   here, since `alice-example-com` sorts BEFORE `default` while
//!   `owner-account` sorts after it. A fix that had merely reordered
//!   the sort would pass one of these and fail the other.
//!
//! * **Measurement was NOT safe.** `calibration_wanted` states the rule —
//!   *"measuring a login twice under two names produces two numbers for one
//!   limit"* — and the poll broke it, asking `registry.names()` and so probing
//!   `/usage` once per LABEL. On the operator's live database that is exactly
//!   what happened: two observation streams for one pool, disagreeing by 88
//!   percentage points on the weekly axis.
//!
//! Nothing here spawns a real turn, and no test touches the operator's own
//! `~/.claude`, the running daemon, or the keychain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};


use threads::{build_manager, Config, ThreadManager};

// ═════════════════════════════════════════════════════════════════════════
// FIXTURES — the operator's own fleet, as it stood on 2026-08-06
// ═════════════════════════════════════════════════════════════════════════

/// The live fingerprint the new account and the machine login came to share.
const SHARED_LOGIN: &str = "476d69df.684c8b5d";
/// A second, genuinely separate login.
const OTHER_LOGIN: &str = "c127c516.5810305a";

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

fn placeable_names(m: &ThreadManager) -> Vec<String> {
    let mut v: Vec<String> = m
        .selectable_pool()
        .into_iter()
        .map(|p| p.into_name())
        .collect();
    v.sort();
    v
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

// ═════════════════════════════════════════════════════════════════════════
// 1. PLACEMENT — does the 564386f fix still hold in the mirror case?
// ═════════════════════════════════════════════════════════════════════════

/// **A placeable account sharing the machine login's fingerprint is still the
/// one that gets placed on, and `default` is still not.**
///
/// The mirror case, stated once. `alice-example-com` sorts BEFORE `default`, so
/// this configuration would pass even under the original alphabetical rule —
/// which is precisely why the next test exists.
#[tokio::test]
async fn a_placeable_account_that_aliases_the_machine_login_is_still_placeable() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["alice-example-com"])).unwrap();

    identity(&m, "default", SHARED_LOGIN);
    identity(&m, "alice-example-com", SHARED_LOGIN);

    assert_eq!(
        placeable_names(&m),
        vec!["alice-example-com".to_string()],
        "the collapse must keep the account threads OWN and drop the machine login, \
         not the other way round"
    );
    assert!(
        !m.placement_pool().iter().any(|p| p.name() == "default"),
        "the machine login is never a place to run, aliased or not"
    );
}

/// **And it holds when the alphabet argues the other way.**
///
/// `owner-account` sorts AFTER `default`, so under the original
/// alphabetically-first rule the machine login would win this unit and become
/// the canonical name for an account threads own — which is the 61%/8% skew,
/// exactly. It does not, because `placeable_pool` drops before it collapses:
/// `default` is not in the set the canonical is chosen from, so there is no
/// ordering it can win.
#[tokio::test]
async fn the_machine_login_cannot_win_a_unit_even_when_it_sorts_first() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["owner-account"])).unwrap();

    identity(&m, "default", SHARED_LOGIN);
    identity(&m, "owner-account", SHARED_LOGIN);

    assert!(
        "default" < "owner-account",
        "this test is only meaningful while the machine login sorts first"
    );
    assert_eq!(
        placeable_names(&m),
        vec!["owner-account".to_string()],
        "drop-then-collapse must beat the alphabet; a fix that only reordered the \
         sort would fail here"
    );
}

/// **Two labels for one login still count as ONE account.**
///
/// The property the whole collapse exists for, asserted in the mirror
/// configuration: four registry labels, three real logins.
#[tokio::test]
async fn a_placeable_alias_of_the_machine_login_does_not_inflate_the_account_count() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(
        tmp.path(),
        &["alice-example-com", "owner-gmail"],
    ))
    .unwrap();

    identity(&m, "default", SHARED_LOGIN);
    identity(&m, "alice-example-com", SHARED_LOGIN);
    identity(&m, "owner-gmail", OTHER_LOGIN);

    // TWO, not three. This asserted `3` — *"three registry labels are
    // visible"* — until 2026-08-27, when the operator struck the
    // visible/placeable split outright: *"I do not want default even in the
    // purview of the system. Why does it even see that there is a default?"*
    // The daemon's account list is the POOL now, and the only thing the wider
    // list ever carried was the machine default.
    assert_eq!(
        m.account_pool().len(),
        2,
        "the daemon sees the pool, and the machine default is not in it"
    );
    assert_eq!(
        placeable_names(&m),
        vec![
            "alice-example-com".to_string(),
            "owner-gmail".to_string()
        ],
        "but only two of them are places to run, and they are two DIFFERENT logins"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 2. MEASUREMENT — one login, one reading
// ═════════════════════════════════════════════════════════════════════════

/// **A login that carries two labels is measured ONCE.**
///
/// The defect, in one assertion. `probe_accounts_once` asked
/// `registry.names()`, so the shared login was polled under both of its names
/// and recorded two independent `usage_observations` streams — and on the
/// operator's live database those two streams disagreed by 88 points on the
/// weekly axis, because they were taken hours apart and nothing ever reconciled
/// them.
#[tokio::test]
async fn one_login_is_measured_once_even_when_it_carries_two_labels() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["alice-example-com"])).unwrap();

    identity(&m, "default", SHARED_LOGIN);
    identity(&m, "alice-example-com", SHARED_LOGIN);

    assert_eq!(
        sorted(m.poll_targets()),
        vec!["alice-example-com".to_string()],
        "two names for one pool must not produce two readings of it"
    );
}

/// **And it is measured under the name PLACEMENT reads.**
///
/// Collapsing to one reading is only half of it. The gauge, the budget meters
/// and `limit_estimates` are all keyed by LABEL, so a reading recorded against
/// `default` is a reading no placement decision will ever consult — the login
/// would be polled once and still be invisible where it counts.
#[tokio::test]
async fn the_measured_label_is_the_one_placement_will_read() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // The label sorts AFTER `default`, so the unit's plain canonical would be
    // the machine login. The poll must not follow it there.
    let m = build_manager(config_at(tmp.path(), &["owner-account"])).unwrap();

    identity(&m, "default", SHARED_LOGIN);
    identity(&m, "owner-account", SHARED_LOGIN);

    assert_eq!(
        sorted(m.poll_targets()),
        vec!["owner-account".to_string()],
        "the reading must land on the placeable label, or it informs nothing"
    );
}

/// **A machine login that aliases nothing IS NOT POLLED — this assertion is
/// inverted, and the inversion is the point.**
///
/// This test used to assert the opposite: that while `default` is nobody's
/// alias it is the only name for its login, so dropping it from the poll would
/// "silently stop measuring a real account". That reasoning was wrong about
/// whose account it is.
///
/// **What the old behaviour actually did.** `poll_targets` fell back to the
/// selection unit's canonical label when a login had no placeable member, so it
/// returned `"default"`. `probe_account("default")` then resolved a config dir
/// of `None`, and `None` means leave `CLAUDE_CONFIG_DIR` UNSET — so on every
/// probe tick the daemon ran `claude -p "/usage"` against `~/.claude`, the
/// operator's own terminal login, and wrote the reading back under `"default"`
/// where the `claude-accounts` TUI drew it as a bar row. That is not
/// visibility, it is ACCESS, and it stands against the plainest thing he has
/// said about this account: *"threads should never access my default account."*
///
/// So the fix is not a filter in `poll_targets`. `poll_targets` iterates the
/// POOL, and there is no non-pool name left for the fallback to reach.
///
/// **THE MUTATION.** Point `poll_targets` back at `self.registry.names()` and
/// this test goes red immediately.
#[tokio::test]
async fn the_machine_login_is_never_polled() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["owner-gmail"])).unwrap();

    identity(&m, "aa-repointed", SHARED_LOGIN);
    identity(&m, "owner-gmail", OTHER_LOGIN);

    assert_eq!(
        sorted(m.poll_targets()),
        vec!["owner-gmail".to_string()],
        "the probe spawns `claude` per target; a target outside the pool is a spawn \
         against a login threads was never given"
    );
}

/// **A label whose identity cannot be read is never collapsed into anything.**
///
/// Absence of evidence must not merge two accounts: merging wrongly hides a
/// real account from measurement, which is the worse of the two errors and the
/// rule `selection_units` already states. An unreadable config dir is the
/// normal state of a half-registered account, which is exactly the condition
/// the fleet was in when this was measured.
#[tokio::test]
async fn an_unknown_identity_is_never_folded_into_another_login() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["alice-example-com", "half-registered"])).unwrap();

    identity(&m, "default", SHARED_LOGIN);
    identity(&m, "alice-example-com", SHARED_LOGIN);
    // `half-registered` is given NO identity at all.

    let targets = sorted(m.poll_targets());
    assert!(
        targets.contains(&"half-registered".to_string()),
        "a label of unknown identity is its own login until proven otherwise: {targets:?}"
    );
    assert!(
        targets.contains(&"alice-example-com".to_string()),
        "and the known alias pair still collapses to its placeable name: {targets:?}"
    );
    assert!(
        !targets.contains(&"default".to_string()),
        "while the machine login, which IS a proven alias, is not polled twice: {targets:?}"
    );
}

/// **The poll and the calibration question ask the same thing.**
///
/// A source guard. The rule that a login is measured once lives in a doc
/// comment on `calibration_wanted` and was broken by the very next function,
/// which asked `registry.names()` instead. The two must not drift apart again:
/// whatever decides WHETHER a reading is wanted and whatever decides WHERE to
/// take it both have to speak in logins.
#[test]
fn the_poll_does_not_ask_the_raw_label_list() {
    let src = include_str!("../../src/runtime.rs");
    let start = src
        .find("pub async fn probe_accounts_once")
        .expect("probe_accounts_once is gone");
    let body = &src[start..start + 1200];

    assert!(
        !body.contains("self.registry.names()"),
        "the poll is asking the raw visible label list again, which measures an \
         aliased login once per NAME"
    );
    assert!(
        body.contains("poll_targets"),
        "the poll must go through the one-reading-per-login decision"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
