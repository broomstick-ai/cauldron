//! **A usage limit is a fact about a LOGIN, and a completed turn can falsify
//! it.**
//!
//! On 2026-08-02 the `threads` daemon believed a healthy login was dead for
//! five and a half hours, drove the operator's thread off it onto the one
//! genuinely exhausted account, and would not have changed its mind before the
//! foreign clock ran out. Two clauses produced that, and each has tests here
//! that fail without it.
//!
//! **The limit had the wrong SUBJECT.** `note_account_limit` wrote
//! `accounts.limit_until` against the LABEL the failing thread was spawned
//! under. A label is a name for a login, not a login: the machine-default label
//! has no config dir, so it is whatever `~/.claude.json` currently holds, and a
//! human `/login` re-points it without the daemon being told. `identity` is
//! recomputed only in `ThreadManager::new`, blind — nothing compares the new
//! fingerprint with the old. So on Aug 1 the `default` label took a legitimate
//! weekly limit while it named Login B; at ~17:00Z on Aug 2 the operator logged
//! it into Login A; and at the 19:23:39Z boot `refresh_identities` re-stamped
//! it, at which instant `accounts::with_aliases` promoted Login B's day-old
//! limit into a bar on every label of Login A. Ninety-nine seconds later the
//! gate had nowhere to put the operator's next message and ported a
//! 3,579-event transcript onto the exhausted login.
//!
//! **Nothing could contradict a limit.** `note_account_limit` was the only
//! writer of `limit_until` in the whole daemon. A window went in on a 429 and
//! came out only when its own recorded instant passed. The `/usage` poll
//! watched that same login report 29% every fifteen minutes throughout, and
//! turns completed on it the whole time, and neither had any way to say so.
//!
//! # The label in these tests used to be `default`, and is now `aa-repointed`
//!
//! The incident happened to the machine-default label. As of 2026-08-27 that
//! label is not the daemon's at all — it is not a pool member, so it cannot
//! take a limit, cannot be barred, and cannot be listed. Renaming the fixture
//! is NOT a way of keeping the tests green past a behaviour change: the
//! mechanism they pin is *"a label is a name for a login, not a login"*, and
//! that is MORE true of a pool account than it ever was of the machine default,
//! because `add-account --force` re-points a pool label at a different login as
//! a routine operation. The operator named exactly this when he moved the pool
//! share off the login and onto the name: *"the login slot is subject to
//! change."* So the subject-of-a-limit invariant is now pinned where it still
//! bites.
//!
//! Every test drives the manager in-process against the FAKE claude fixture. No
//! real claude turn is ever spawned; every manager gets an explicit `data_dir`
//! under a tempdir, and `THREADS_CLAUDE_HOME` points transcript storage at a
//! scratch dir, so nothing here can read or write inside the operator's own
//! `~/.claude`, the running daemon, or the keychain.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::accounts::AccountState;
use threads::event::{Push, StoredEvent};
use threads::settings::ThreadSettings;
use threads::store::RetryRow;
use threads::{build_manager, Config, ThreadManager};

// ═════════════════════════════════════════════════════════════════════════
// FIXTURES
// ═════════════════════════════════════════════════════════════════════════

/// The two real login fingerprints from the incident, so the reconstruction is
/// the operator's own database and not a toy.
const LOGIN_A: &str = "d11af2d3.683974df"; // owner@example.ai
const LOGIN_B: &str = "c127c516.5810305a"; // owner@example.com

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

/// An AUI thread: `Auto` account, no budget key, no thresholds.
fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

fn far_future() -> String {
    "2099-01-01T00:00:00+00:00".to_string()
}

fn identity(m: &ThreadManager, name: &str, fp: &str) {
    m.store.set_account_identity(name, None, Some(fp)).unwrap();
}

/// Record a limit window on one label, stamped with the login it was OBSERVED
/// on. Passing a different fingerprint than the label's current `identity` is
/// exactly the re-pointed-label case.
fn limit_observed_on(m: &ThreadManager, name: &str, until: &str, observed_on: Option<&str>) {
    m.store
        .set_account_limit(name, Some((until, Some("8pm (America/Chicago)"), None, observed_on)))
        .unwrap();
}

fn states_of(m: &ThreadManager) -> BTreeMap<String, AccountState> {
    m.store
        .list_account_states()
        .unwrap()
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect()
}

fn state_of(m: &ThreadManager, name: &str) -> AccountState {
    states_of(m)
        .remove(name)
        .unwrap_or_else(|| panic!("no accounts row for {name}"))
}

fn barred(m: &ThreadManager) -> Vec<String> {
    m.barred_accounts().0
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

fn saw(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> bool {
    seen.lock()
        .unwrap()
        .iter()
        .any(|e| e.event.kind() == kind)
}

fn send(m: &ThreadManager, id: &str, text: &str) {
    m.send(
        id,
        text.to_string(),
        threads::SendIntent::Immediate,
        serde_json::json!({}),
        false,
    )
    .unwrap();
}

// ═════════════════════════════════════════════════════════════════════════
// 1. THE SUBJECT OF A LIMIT — a login, not a label
// ═════════════════════════════════════════════════════════════════════════

/// **A re-pointed label does not carry its previous login's limit.**
///
/// The incident in one assertion, with the operator's own fingerprints. The
/// `default` label took a real weekly limit while it named Login B. It was then
/// logged into Login A. The limit is still on the row and its clock has not run
/// out — but it describes a login this label no longer names, so it bars
/// nothing, and above all it does not widen onto Login A's other label.
///
/// Without `AccountState::limit_is_stale` this returns both Login A labels and
/// the daemon has no healthy account at all.
#[tokio::test]
async fn a_relabelled_login_does_not_inherit_the_previous_logins_limit() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["owner-account", "owner-gmail"]))
        .unwrap();

    // After the 19:23:39Z boot: `default` points at Login A, like its sibling.
    identity(&m, "aa-repointed", LOGIN_A);
    identity(&m, "owner-account", LOGIN_A);
    identity(&m, "owner-gmail", LOGIN_B);

    // …but the window sitting on it was observed on Login B, the day before.
    limit_observed_on(&m, "aa-repointed", &far_future(), Some(LOGIN_B));
    // Login B really is exhausted, on its own label, attributed to itself.
    limit_observed_on(&m, "owner-gmail", &far_future(), Some(LOGIN_B));

    assert_eq!(
        barred(&m),
        vec!["owner-gmail".to_string()],
        "only the login that actually refused may be barred; `default` carries a limit \
         belonging to a login it no longer points at, and widening that onto \
         `owner-account` is what stranded the thread"
    );
    assert!(
        state_of(&m, "aa-repointed").available(&threads::store::now_rfc3339()),
        "a limit stamped with a different login does not bar this one"
    );
}

/// **The guard does not un-bar a genuine limit.**
///
/// The anti-test. A window stamped with the login the label still points at is
/// exactly as binding as it ever was, and it still widens across siblings —
/// which is the clause that stops "fail over to the other name for the same
/// account".
#[tokio::test]
async fn a_limit_stamped_with_its_own_login_still_bars_every_label_of_it() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["owner-account", "owner-gmail", "aa-repointed"]))
        .unwrap();

    identity(&m, "aa-repointed", LOGIN_A);
    identity(&m, "owner-account", LOGIN_A);
    identity(&m, "owner-gmail", LOGIN_B);

    limit_observed_on(&m, "aa-repointed", &far_future(), Some(LOGIN_A));

    assert_eq!(
        barred(&m),
        vec![
            "aa-repointed".to_string(),
            "owner-account".to_string()
        ],
        "Login A refused, so both of its labels are barred and Login B is untouched"
    );
}

/// **A limit nobody can attribute keeps barring.**
///
/// Rows written before `limit_identity` existed carry `None`, and so does a 429
/// taken while the config dir held no readable identity. Absence of evidence
/// must never un-bar: guessing "unattributed, so it cannot apply" would hammer
/// an account that really is exhausted, which is the worse of the two errors.
#[tokio::test]
async fn an_unattributed_limit_still_bars() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["owner-account", "aa-repointed"])).unwrap();

    identity(&m, "aa-repointed", LOGIN_A);
    identity(&m, "owner-account", LOGIN_A);
    limit_observed_on(&m, "aa-repointed", &far_future(), None);

    assert!(
        barred(&m).contains(&"aa-repointed".to_string()),
        "a limit with no recorded subject is still a limit"
    );
    assert!(
        !state_of(&m, "aa-repointed").limit_is_stale(),
        "unknown is not the same as contradicted"
    );
}

/// **A label whose identity is unknown does not get un-barred either.**
///
/// The mirror of the above: the limit names a login, the label no longer names
/// anything readable. That is a gap in our knowledge, not proof the limit moved
/// on.
#[tokio::test]
async fn a_limit_on_a_label_of_unknown_identity_still_bars() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-repointed"])).unwrap();

    m.store.set_account_identity("aa-repointed", None, None).unwrap();
    limit_observed_on(&m, "aa-repointed", &far_future(), Some(LOGIN_B));

    assert!(
        barred(&m).contains(&"aa-repointed".to_string()),
        "an unreadable identity is not evidence the limit belongs elsewhere"
    );
}

/// **What the operator is shown is what the gate applies.**
///
/// The incoherence he caught — one label reporting a limit while its sibling,
/// the same login, reported none — was readable straight off `accounts status`
/// and the `accounts.list` payload, because those printed the raw column. They
/// now report the EFFECTIVE window, so a bar that binds nothing cannot be
/// displayed as though it does.
#[tokio::test]
async fn a_limit_the_gate_ignores_is_not_reported_as_live() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["owner-account", "aa-repointed"])).unwrap();

    identity(&m, "aa-repointed", LOGIN_A);
    identity(&m, "owner-account", LOGIN_A);
    limit_observed_on(&m, "aa-repointed", &far_future(), Some(LOGIN_B));

    let st = state_of(&m, "aa-repointed");
    assert!(st.limit_until.is_some(), "the raw column is preserved for audit");
    assert_eq!(
        st.effective_limit_until(&threads::store::now_rfc3339()),
        None,
        "…but nothing may report it as a live window"
    );

    let rows = threads::accounts::activity_by_account(
        &m.pool(),
        &[],
        &states_of(&m),
        &threads::store::now_rfc3339(),
        None,
    );
    let shown = rows
        .iter()
        .find(|r| r.account == "aa-repointed")
        .expect("`default` must report");
    assert_eq!(
        shown.limit_until, None,
        "the operator must not be told an account is limited when placement disagrees"
    );
    assert!(
        shown.available,
        "…and the two halves of that row must agree with each other"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 2. WHAT A 429 RECORDS
// ═════════════════════════════════════════════════════════════════════════

/// **THE FIXTURE'S 429 NAMES A LIVE WINDOW AT EVERY HOUR OF THE DAY.**
///
/// The guard on the defect that took the three tests below red on 2026-10-06
/// while the commit responsible reported the crate green.
///
/// `fake-claude`'s `ratelimit` branch hard-coded `resets 2:40am
/// (America/Chicago)`. `804cffd` then made `reset::ResetTime::resolve` answer
/// `None` for time-only prose whose clock time has already gone by and is not a
/// genuine midnight wrap — which is CORRECT and is not in question here: a
/// window that has ended bars nothing, and `note_account_limit_for` writes no
/// window, no identity and no axis for it, by design. `reset.rs`'s own
/// `time_only_prose_that_went_by_this_morning_is_no_window` pins that with
/// **this very string**: `2:40am` resolved at `03:00` is no window at all.
///
/// So the fixture and the resolver's own unit test contradicted each other by
/// construction, and which one won depended on the hour the suite ran in.
/// `2:40am` resolves only inside `MAX_MIDNIGHT_ROLL` (six hours), so the three
/// tests below were green between 20:40 and 02:40 local and red the other
/// eighteen hours of every day. The fix's author ran them at ~22:45 local and
/// saw the whole crate pass; the operator ran them at 03:34 local and saw
/// exactly these three fail with `limit_until: None, limit_axis: None`.
///
/// A fixture that rots is a test that fails again next week and teaches
/// everyone to ignore it, so the fixture now COMPUTES its reset two hours ahead
/// of `now`, and this asserts the PROPERTY rather than any instant: whatever the
/// wall clock says, the reset the fixture emits resolves to a future window.
///
/// It sweeps all twenty-four hourly frames with the fixture's own `date` and the
/// resolver reading the SAME simulated clock, so unlike the tests below it
/// cannot be green by luck of the hour — and it goes red on any hard-coded clock
/// time, in eighteen of the twenty-four frames. That is the regression being
/// prevented.
#[test]
fn the_fixtures_429_resolves_to_a_live_window_in_every_hourly_frame() {
    use std::io::Write;

    for shift in 0..24i64 {
        // `Etc/GMT+N` is UTC MINUS N — the POSIX sign inversion — so the
        // fixture's `date -v+2H` and the naive frame we resolve in are the same
        // simulated clock, placed `shift` hours behind UTC.
        let tz = format!("Etc/GMT+{shift}");
        let mut child = std::process::Command::new(fake_claude())
            .env("TZ", &tz)
            .env("FAKE_STATE_DIR", std::env::temp_dir())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("the fake claude fixture is executable");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"please ratelimit me")
            .unwrap();
        let out = child.wait_with_output().expect("the fixture exits");
        let stdout = String::from_utf8_lossy(&out.stdout);

        let line = stdout
            .lines()
            .find(|l| l.contains("\"apiErrorStatus\":429"))
            .unwrap_or_else(|| panic!("the fixture must emit a 429 in {tz}: {stdout}"));
        let v: serde_json::Value = serde_json::from_str(line).expect("the 429 line is JSON");
        let body = v["message"]["content"][0]["text"]
            .as_str()
            .expect("the 429 carries assistant text")
            .to_string();

        // The axis has to stay knowable: the prose names the SESSION quota.
        assert_eq!(
            threads::gauge::classify_limit_pool(&body),
            Some(threads::gauge::Axis::Session),
            "the fixture's refusal must still name the session quota in {tz}: {body}"
        );

        let parsed = threads::reset::parse_limit_reset(&body)
            .unwrap_or_else(|| panic!("the fixture's reset prose must parse in {tz}: {body}"));
        let now_local = (chrono::Utc::now() - chrono::Duration::hours(shift)).naive_utc();
        let resolved = parsed.resolve(now_local);
        let at = resolved.unwrap_or_else(|| {
            panic!(
                "the fixture's reset must name a FUTURE window in {tz} (simulated local \
                 {now_local}), or the 429 records nothing at all and the three tests below \
                 fail for reasons that have nothing to do with what they assert. \
                 Prose was {body:?} — a hard-coded clock time is what this catches."
            )
        });
        let ahead = at - now_local;
        assert!(
            ahead > chrono::Duration::zero() && ahead <= chrono::Duration::hours(3),
            "a session reset lives inside the five-hour session pool; in {tz} this one \
             resolved {ahead} ahead of simulated local {now_local} (prose {body:?})"
        );
    }
}

/// **A 429 is recorded against every label of the login that refused.**
///
/// It used to land on one label — whichever name the failing thread happened to
/// be spawned under — leaving its sibling's row blank. Two names for one account
/// could then hold contradictory beliefs about that account, and the stored
/// state needed a read-time join to make sense.
#[tokio::test]
async fn a_429_records_the_limit_on_every_label_of_the_login() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["zz-alias", "aa-repointed"])).unwrap();

    identity(&m, "aa-repointed", LOGIN_A);
    identity(&m, "zz-alias", LOGIN_A);

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    send(&m, &id, "please ratelimit me");

    assert!(
        eventually(|| state_of(&m, "aa-repointed").limit_until.is_some()).await,
        "the 429 must be recorded at all: {:?}",
        seen.lock().unwrap().len()
    );
    assert!(
        eventually(|| state_of(&m, "zz-alias").limit_until.is_some()).await,
        "the sibling label names the same login, so it holds the same window"
    );
    assert_eq!(
        state_of(&m, "aa-repointed").limit_until,
        state_of(&m, "zz-alias").limit_until,
        "two labels for one login may not disagree about that login's limit"
    );
}

/// **The recorded limit says which login it was observed on.**
///
/// Without the stamp there is nothing later to compare against, and a
/// re-pointed label silently exports the window to a login that never refused.
#[tokio::test]
async fn a_429_stamps_the_login_it_was_observed_on() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // CON-27: this test RUNS a turn, so it needs an account threads may place
    // on. It used to lean on the machine `default` that `ensure_default`
    // registers on every boot, which placement can no longer name.
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    identity(&m, "aa-owned", LOGIN_A);
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    send(&m, &id, "please ratelimit me");

    assert!(
        eventually(|| state_of(&m, "aa-owned").limit_until.is_some()).await,
        "the 429 must be recorded"
    );
    assert_eq!(
        state_of(&m, "aa-owned").limit_identity.as_deref(),
        Some(LOGIN_A),
        "the window must name the login it was taken on"
    );
}

/// **The recorded limit says which QUOTA refused.**
///
/// The axis was known inside `interpret` — `LimitHit.pool`, from
/// `gauge::classify_limit_pool` — and was consumed into a `usage_observations`
/// row and dropped, so the 429 branch five hundred lines later recorded a bar
/// that could not name what caused it. It is carried through now. The bar is
/// still account-wide; this is what makes scoping it possible at all.
#[tokio::test]
async fn a_429_records_which_quota_refused() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // CON-27: an account threads owns, because this test runs a turn.
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    identity(&m, "aa-owned", LOGIN_A);
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    send(&m, &id, "please ratelimit me");

    assert!(
        eventually(|| state_of(&m, "aa-owned").limit_axis.is_some()).await,
        "the fixture's refusal names the session limit, so the axis is knowable: {:?}",
        state_of(&m, "aa-owned")
    );
    assert_eq!(
        state_of(&m, "aa-owned").limit_axis.as_deref(),
        Some("session"),
        "the quota recorded must be the one the prose named"
    );
}

/// **AN IMPLAUSIBLE WINDOW IS REFUSED, NOT WRITTEN.**
///
/// The 2026-08-15 bar, generalized. `owner-account` took a 429 whose
/// prose read `Aug 17 at 5:49pm (America/Chicago)`; the resolver rolled the
/// passed date forward by a YEAR and `note_account_limit_for` wrote
/// `2027-08-17T22:49Z` into `accounts.limit_until` with no bound of any kind.
/// The account was barred for five days at `session_pct=0.0`, and nothing could
/// undo it: the only thing that clears a window early is a completed turn on
/// that account, and the bar is what stops one from starting.
///
/// The resolver is fixed. This is the second line of defence, and it is what
/// makes the whole CLASS survivable: no pool `claude` has is longer than seven
/// days, `MAX_PARSED_RESET` is eight, and a refusal claiming a month is not a
/// limit we failed to understand — it is one that cannot be real. The fixture
/// emits exactly that: a well-formed 429 whose reset is thirty days out.
///
/// Note what is asserted and what is not: the 429 still happened, is still
/// classified, and the thread still backs off on the retry ladder. The only
/// thing refused is taking the ACCOUNT out of service on the strength of it.
#[tokio::test]
async fn a_429_whose_window_could_not_be_real_never_bars_the_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    identity(&m, "aa-owned", LOGIN_A);
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    send(&m, &id, "please farlimit me");

    // The refusal really did arrive and really was taken as a usage limit —
    // otherwise this test would pass vacuously on a 429 that never happened.
    assert!(
        eventually(|| saw(&seen, "retry_scheduled")).await,
        "the 429 must still be seen and still back off: {:?}",
        seen.lock().unwrap().iter().map(|e| e.event.kind()).collect::<Vec<_>>()
    );

    let row = state_of(&m, "aa-owned");
    assert_eq!(
        row.limit_until, None,
        "a window a month out cannot be a real limit and must never reach the accounts \
         table — writing it bars a healthy login with no way back: {row:?}"
    );
    let (unavailable, _) = m.barred_accounts();
    assert!(
        !unavailable.contains(&"aa-owned".to_string()),
        "the account stays in service: {unavailable:?}"
    );
}

/// **An unparseable reset never clears a live window.**
///
/// `note_account_limit` used to write its computed `Option` straight through an
/// unconditional `limit_until=excluded.limit_until`, so a 429 whose prose did
/// not parse wrote NULL over a valid future window and un-barred the account —
/// and, through the alias widening, every sibling label of it — instantly. The
/// retry ladder's backoff owns the no-reset case; the accounts table does not
/// get to forget what it knows because one refusal was mute.
#[tokio::test]
async fn an_unparseable_reset_never_clears_a_live_window() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-repointed"])).unwrap();

    identity(&m, "aa-repointed", LOGIN_A);
    limit_observed_on(&m, "aa-repointed", &far_future(), Some(LOGIN_A));

    let src = include_str!("../../src/runtime.rs");
    let start = src
        .find("    fn note_account_limit(")
        .expect("note_account_limit was renamed — re-point this test");
    let body = &src[start..];
    let end = body
        .find("    /// **A turn completed on this account")
        .expect("the item after note_account_limit moved — re-point this test");
    let body = &body[..end];
    assert!(
        body.contains("let Some(until) = limit_until else"),
        "a 429 with no parseable reset must RETURN, not write a None window that erases \
         a live one"
    );
    assert!(
        !body.contains("upsert_account_state"),
        "the 429 path must use the narrow limit writer, which cannot touch config_dir, \
         identity or health"
    );

    assert!(
        barred(&m).contains(&"aa-repointed".to_string()),
        "the live window is still in force"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 3. A COMPLETED TURN FALSIFIES A LIMIT
// ═════════════════════════════════════════════════════════════════════════

/// **A turn that completes clears the window it just disproved.**
///
/// This is the safety net under everything above. Before it, `limit_until` had
/// exactly one writer and no reader could contradict it: a window went in on a
/// 429 and came out only when its own clock ran out. However the daemon comes to
/// hold a wrong limit — a re-pointed label, a quota that reset early, a clock
/// skew — a single successful turn now retires it, in seconds, rather than
/// hours.
#[tokio::test]
async fn a_successful_turn_clears_the_login_limit() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // CON-27: an account threads owns, because this test runs a turn on it.
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    identity(&m, "aa-owned", LOGIN_A);
    limit_observed_on(&m, "aa-owned", &far_future(), Some(LOGIN_A));
    assert!(barred(&m).contains(&"aa-owned".to_string()));

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    send(&m, &id, "hello");

    assert!(
        eventually(|| saw(&seen, "turn_ended")).await,
        "the ungated thread runs anyway — that is what makes the limit falsifiable"
    );
    assert!(
        eventually(|| state_of(&m, "aa-owned").limit_until.is_none()).await,
        "a completed turn is proof the account can serve; the recorded window was wrong \
         and must not be left to expire on its own clock: {:?}",
        state_of(&m, "aa-owned")
    );
    assert!(
        !barred(&m).contains(&"aa-owned".to_string()),
        "and placement sees the capacity immediately"
    );
}

/// **Success on one label clears its sibling's window too.**
///
/// The clear has the same subject as the bar. If it did not, a success on
/// `owner-account` would leave `default`'s row set, and the alias
/// widening would put the bar straight back on the login that had just
/// demonstrably served a turn.
#[tokio::test]
async fn a_successful_turn_on_one_label_clears_its_siblings_window() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["zz-alias"])).unwrap();

    identity(&m, "aa-repointed", LOGIN_A);
    identity(&m, "zz-alias", LOGIN_A);
    limit_observed_on(&m, "aa-repointed", &far_future(), Some(LOGIN_A));
    limit_observed_on(&m, "zz-alias", &far_future(), Some(LOGIN_A));

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    send(&m, &id, "hello");

    assert!(eventually(|| saw(&seen, "turn_ended")).await, "the turn must run");
    assert!(
        eventually(|| state_of(&m, "zz-alias").limit_until.is_none()).await,
        "the turn ran on one name for this login; the window is a fact about the login: {:?}",
        state_of(&m, "zz-alias")
    );
    assert!(barred(&m).is_empty(), "nothing is barred once the login has served");
}

/// **A success on a limited login does not free a DIFFERENT login.**
///
/// The other half of the clause: the clear is exactly as wide as the login, and
/// not one label wider.
#[tokio::test]
async fn a_successful_turn_never_clears_another_logins_window() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // CON-27: two accounts threads owns. Login A used to be carried by the
    // machine `default`, which nothing may be placed on any more.
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "zz-other"])).unwrap();

    identity(&m, "aa-owned", LOGIN_A);
    identity(&m, "zz-other", LOGIN_B);
    // THE MACHINE DEFAULT IS PINNED, and this test does not work without it.
    // `LOGIN_B` is the operator's REAL `owner@example.com` fingerprint, and
    // `refresh_identities` fingerprints the `default` label from the real
    // `~/.claude.json` at boot — so on his own machine `default` genuinely IS
    // Login B, `with_aliases` correctly bars it alongside `zz-other`, and the
    // assertion below failed for a reason that had nothing to do with the
    // subject of a clear. The fixture now owns every identity it reasons about
    // instead of inheriting one from whoever is logged in.
    identity(&m, "aa-repointed", "ffffffff.ffffffff");
    limit_observed_on(&m, "aa-owned", &far_future(), Some(LOGIN_A));
    limit_observed_on(&m, "zz-other", &far_future(), Some(LOGIN_B));

    // Named, so the assertion is about the SUBJECT of the clear and not about
    // which of two barred logins `Auto` happened to rank first.
    let mut s = settings(tmp.path());
    s.account = threads::settings::AccountChoice::Named("aa-owned".into());
    let id = m.create_thread(None, s).unwrap();
    let seen = watch(&m);
    send(&m, &id, "hello");

    assert!(eventually(|| saw(&seen, "turn_ended")).await, "the turn must run");
    assert!(
        eventually(|| state_of(&m, "aa-owned").limit_until.is_none()).await,
        "the login that served is freed"
    );
    assert_eq!(
        barred(&m),
        vec!["zz-other".to_string()],
        "a turn proves nothing about a login it did not run on"
    );
}

/// **Clearing a falsified window re-asks placement, it does not wait for a
/// tick.**
///
/// `3af681e` made a retrying thread wait on the account picture rather than on
/// one account's clock, and `account_picture_changed` is the bell it listens
/// for. A limit being falsified is precisely such a change — capacity appearing
/// where the daemon believed there was none — so it must ring. Without this the
/// fix above would still leave the operator's thread asleep until a clock.
///
/// # Re-grounded 2026-10-06: the bell is the whole subject now
///
/// This asserted that the floored row entered and left `retries_to_reask`, which was
/// an ELIGIBILITY query: the row became selectable once `retry_unblocked_early` could
/// see a free sibling. There is no eligibility any more — the pool reconsiders every
/// parked prompt at every opportunity — so the proxy is gone and what is left is the
/// thing the test was always about: **did the falsified window RING?** A prompt that
/// is always in the candidate set is only ever as responsive as the bell is complete.
///
/// FORCED RED, run and recorded: deleting `account_picture_changed_for("limit_cleared",
/// …)` from `note_account_limit_cleared` gives `a falsified window must RING … got
/// []`.
#[tokio::test]
async fn a_falsified_window_wakes_a_thread_floored_on_another_login() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // CON-27: Login A is carried by an account threads owns.
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "zz-other"])).unwrap();

    identity(&m, "aa-owned", LOGIN_A);
    identity(&m, "zz-other", LOGIN_B);
    // THE MACHINE DEFAULT IS PINNED, and this test does not work without it.
    // `LOGIN_B` is the operator's REAL `owner@example.com` fingerprint, and
    // `refresh_identities` fingerprints the `default` label from the real
    // `~/.claude.json` at boot — so on his own machine `default` genuinely IS
    // Login B, `with_aliases` correctly bars it alongside `zz-other`, and the
    // assertion below failed for a reason that had nothing to do with the
    // subject of a clear. The fixture now owns every identity it reasons about
    // instead of inheriting one from whoever is logged in.
    identity(&m, "aa-repointed", "ffffffff.ffffffff");
    limit_observed_on(&m, "aa-owned", &far_future(), Some(LOGIN_A));
    limit_observed_on(&m, "zz-other", &far_future(), Some(LOGIN_B));

    // A second thread is floored on Login B, seventy years out — the Cauldron
    // thread's situation exactly.
    let floored = m.create_thread(None, settings(tmp.path())).unwrap();
    m.store
        .upsert_retry(&RetryRow {
            thread_id: floored.clone(),
            prompt: "the operator's stranded prompt".into(),
            message_ids: vec![1],
            attempt: 2,
            kind: "usage_limit".into(),
            reason: "rate limit (429)".into(),
            account: Some("zz-other".into()),
            created_at: String::new(),
        })
        .unwrap();
    // THE CANDIDATE SET IS UNCONDITIONAL, and that is the ruling rather than an
    // aside: this prompt is asked about from the moment it is parked, whether or not
    // any login is free. What it is waiting for is a REASON, not a clock.
    assert_eq!(
        m.retries_to_reconsider()
            .iter()
            .filter(|r| r.thread_id == floored)
            .count(),
        1,
        "every login is barred and the prompt is STILL reconsidered: *any parked \
         threads get checked at any opportunity*"
    );
    // Drain whatever the fixture's own setup rang, so the bell asserted below is the
    // falsified window's and nothing else's.
    m.placement_pass(threads::runtime::PassOrigin::Event);

    // A turn completes on Login A, which falsifies Login A's window. Named, so
    // the assertion is about the bell and not about which login `Auto` happened
    // to rank first with both of them barred.
    let mut live_settings = settings(tmp.path());
    live_settings.account = threads::settings::AccountChoice::Named("aa-owned".into());
    let live = m.create_thread(None, live_settings).unwrap();
    let seen = watch(&m);
    send(&m, &live, "hello");
    assert!(eventually(|| saw(&seen, "turn_ended")).await, "the turn must run");

    // A LOGIN CAME BACK, SO THE BELL MUST HAVE RUNG. The next pass drains the reason
    // set the falsified window put there; the parked prompt is reconsidered by that
    // pass rather than at any instant of its own.
    assert!(
        eventually(|| {
            m.placement_pass(threads::runtime::PassOrigin::Event);
            m.last_placement_reasons()
                .iter()
                .any(|r| r == "limit_cleared" || r == "limit_falsified_by_gauge")
        })
        .await,
        "a falsified window must RING: a login came back, so the thread parked behind \
         a DIFFERENT one is reconsidered now rather than at any instant of its own — \
         got {:?}",
        m.last_placement_reasons()
    );
    assert!(
        m.placement_passes().reconsidered >= 1,
        "and that pass must actually have asked about the parked prompt — got {:?}",
        m.placement_passes()
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 4. THE INCIDENT, END TO END
// ═════════════════════════════════════════════════════════════════════════

/// **The gate does not drive a thread off a healthy login for a foreign
/// limit.**
///
/// The reconstruction, with the operator's fingerprints and label names. The
/// thread is sitting on Login A, which is healthy. `default` — the other name
/// for Login A — carries Login B's day-old window. Login B itself is genuinely
/// exhausted.
///
/// What the daemon did on the night: it read both Login A labels as barred,
/// declared `owner-gmail` the only admissible account, ported the
/// conversation onto it and took a weekly 429 there eleven seconds later — then
/// floored on that foreign clock for five and a half hours.
///
/// What it must do: stay, and run.
#[tokio::test]
async fn the_gate_does_not_move_a_thread_off_a_healthy_login_for_a_foreign_limit() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["owner-account", "owner-gmail"]))
        .unwrap();

    identity(&m, "aa-repointed", LOGIN_A);
    identity(&m, "owner-account", LOGIN_A);
    identity(&m, "owner-gmail", LOGIN_B);

    // `Auto`, like his thread — a pinned thread never moves, which would make
    // this pass for a reason that has nothing to do with the defect. One real
    // turn first, so the thread has a CONVERSATION on Login A: history is what
    // puts the gate on the CONTINUITY branch, the branch that moves a thread by
    // carrying its transcript, and that choice is made at admission — before any
    // turn can run and falsify anything.
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let first = watch(&m);
    send(&m, &id, "hello");
    assert!(
        eventually(|| saw(&first, "turn_ended")).await,
        "the thread needs real history on Login A before the incident begins"
    );
    let home = m.serving_account(&id).unwrap();
    assert_ne!(home, "owner-gmail", "it starts on Login A");
    m.store.record_thread_account(&id, "aa-repointed").unwrap();
    m.store
        .record_thread_account(&id, "owner-account")
        .unwrap();

    // NOW the state at 19:25:18Z. Aug 1: `default` named Login B and took a real
    // weekly limit. Aug 2 ~17:00Z it was logged into Login A; the 19:23:39Z boot
    // re-stamped its identity, and that day-old foreign window became a bar on
    // Login A. Login B is genuinely at 100% weekly until 01:00Z.
    limit_observed_on(&m, "aa-repointed", &far_future(), Some(LOGIN_B));
    limit_observed_on(&m, "owner-gmail", &far_future(), Some(LOGIN_B));
    assert_eq!(
        barred(&m),
        vec!["owner-gmail".to_string()],
        "only Login B ever refused; barring Login A here is the whole defect"
    );

    let seen = watch(&m);
    send(&m, &id, "the operator's next message");

    assert!(
        eventually(|| saw(&seen, "turn_ended")).await,
        "the turn must run on the healthy login: {:?}",
        seen.lock().unwrap()
    );
    assert_eq!(
        m.serving_account(&id).unwrap(),
        home,
        "the gate ported a 3,579-event transcript onto the exhausted login because a stale \
         window on `default` barred both labels of the healthy one"
    );
    assert!(
        !saw(&seen, "account_switched"),
        "there was nothing to fail over FROM: {:?}",
        seen.lock().unwrap()
    );
    assert!(
        !saw(&seen, "turn_parked"),
        "and it did not wait, either: capacity existed the whole time"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
