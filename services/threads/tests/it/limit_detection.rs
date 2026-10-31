//! **THE DAEMON MUST ACT ON WHAT IT ALREADY KNOWS.**
//!
//! On 2026-08-14 thread `966825e8` was thirty-two minutes into a turn on
//! `owner-account`. At 19:26:13 the daemon's own `/usage` poller read
//! `weekly_all = 100.0` and did nothing with it; it then placed five more
//! threads onto that exhausted account. Between 19:33 and 19:36 six subagents
//! died with "You've hit your weekly limit · resets 1pm (America/Chicago)", and
//! every one of those records was parsed, persisted, and never inspected —
//! `ThreadEvent::TaskUpdated` had no arm in the runtime at all. Failover fired
//! at 19:39:37, correctly and in 616 ms, only once the MAIN turn took its own
//! 429. Net: thirty-three minutes of operator-facing silence, none of it for
//! want of information.
//!
//! Three fixes, one per section, plus the guard that keeps the second one from
//! being worse than the disease:
//!
//! 1. **The gauge can set availability.** A measured exhausted `/usage` reading
//!    writes `accounts.limit_until` exactly as a 429 does — and the poller that
//!    produces it may not stop running.
//! 2. **A subagent's usage limit is the account's usage limit**, with a
//!    false-positive guard that refuses a benign error merely containing the
//!    substring `resets`.
//! 3. **The interrupt is mid-turn.** A turn running on an account that has just
//!    been barred is torn down and handed back to admission, rather than left
//!    to discover the limit for itself.
//!
//! Every test drives the manager in-process against the FAKE claude fixture. No
//! real claude turn is ever spawned; every manager gets an explicit `data_dir`
//! under a tempdir, and `THREADS_CLAUDE_HOME` points transcript storage at a
//! scratch dir, so nothing here can read or write inside the operator's own
//! `~/.claude`, the running daemon, or the keychain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::budgets::Thresholds;
use threads::gauge::Axis;
use threads::event::{Push, StoredEvent};
use threads::runtime::subagent_limit_signal;
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

// ═════════════════════════════════════════════════════════════════════════
// FIXTURES
// ═════════════════════════════════════════════════════════════════════════

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
    std::env::set_var("THREADS_KILL_GRACE_MS", "150");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// Accounts that threads OWNS (CON-27: the machine default is never placeable),
/// each with its own config dir so the `/usage` fixture can be steered per
/// account.
fn config_at(dir: &Path, accounts: &[&str]) -> Config {
    let mut map = BTreeMap::new();
    for name in accounts {
        let cd = dir.join(name);
        std::fs::create_dir_all(&cd).unwrap();
        map.insert(name.to_string(), cd.to_string_lossy().to_string());
    }
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts: map,
        calibrate: false,
        edge_poll: false,
        // No BACKGROUND sweep: every poll in this file is explicit, so the
        // assertions are about the poll that the test took and not about one
        // that happened to land.
        probe_interval_secs: 0,
        ..Default::default()
    }
}

/// Make this account's `/usage` report an exhausted weekly pool — the
/// 19:26:13 reading, with no 429 anywhere.
fn weekly_reads(dir: &Path, account: &str, pct: u32) {
    std::fs::write(dir.join(account).join("usage-weekly"), pct.to_string()).unwrap();
}

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

fn state_of(m: &ThreadManager, name: &str) -> threads::accounts::AccountState {
    m.store
        .list_account_states()
        .unwrap()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no accounts row for {name}"))
}

fn limited(m: &ThreadManager, name: &str) -> bool {
    state_of(m, name).limit_until.is_some()
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
    seen.lock().unwrap().iter().any(|e| e.event.kind() == kind)
}

fn count(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|e| e.event.kind() == kind)
        .count()
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
// 1. THE GAUGE SETS AVAILABILITY
// ═════════════════════════════════════════════════════════════════════════

/// **A measured 100% bars the account, with no 429 anywhere.**
///
/// The 19:26:13 reading, and the whole of the thirteen-minute gap. Before this
/// the only writer of `accounts.limit_until` in the daemon was the 429 path,
/// which needs an `Arc<ThreadState>`; the poller has an account name and no
/// thread, so the one component that MEASURES exhaustion could not express it.
#[tokio::test]
async fn a_measured_exhausted_reading_bars_the_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["exhausted", "healthy"])).unwrap();
    weekly_reads(tmp.path(), "exhausted", 100);
    weekly_reads(tmp.path(), "healthy", 34);

    assert!(!limited(&m, "exhausted"), "nothing is barred before the poll");

    m.probe_accounts_once(true).await.unwrap();

    assert!(
        limited(&m, "exhausted"),
        "a MEASURED 100% is a fact about capacity, and the poller is now allowed to say so: {:?}",
        state_of(&m, "exhausted")
    );
    assert!(
        !limited(&m, "healthy"),
        "and 34% is not a bar — the poller barred what it measured, not what it polled"
    );
    // The bar is written where placement reads it.
    assert!(m.barred_accounts().0.contains(&"exhausted".to_string()));
}

/// **The axis is recorded in the SAME vocabulary the 429 path uses.**
///
/// `accounts.limit_axis` is written by two producers now. The 429 path writes
/// `gauge::Axis::as_str` (via `classify_limit_pool`); the poller must not write
/// the raw `/usage` pool label beside it, or the column stops meaning anything
/// and nothing can ever be scoped to a quota.
#[tokio::test]
async fn the_gauge_records_the_axis_not_the_raw_usage_label() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["exhausted"])).unwrap();
    weekly_reads(tmp.path(), "exhausted", 100);

    m.probe_accounts_once(true).await.unwrap();

    assert_eq!(
        state_of(&m, "exhausted").limit_axis.as_deref(),
        Some(Axis::WeeklyAll.as_str()),
        "an axis string, exactly as `classify_limit_pool` produces on the 429 path"
    );
}

/// **A false bar self-heals on the first completed turn.**
///
/// The safety net under letting a poll write a bar at all. `note_account_served`
/// clears `limit_until` on ANY completed turn, so the worst case of a wrong
/// reading is one admission cycle, not a window that has to expire on a foreign
/// clock.
#[tokio::test]
async fn a_bar_the_gauge_wrote_is_cleared_by_a_completed_turn() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    weekly_reads(tmp.path(), "solo", 100);

    m.probe_accounts_once(true).await.unwrap();
    assert!(limited(&m, "solo"));

    let mut s = settings(tmp.path());
    s.account = threads::settings::AccountChoice::Named("solo".into());
    // Ungated, so the one account left runs anyway and the bar is falsifiable
    // — the 2026-08-02 clause, which this fix must not have taken away.
    s.thresholds = Thresholds::ungated();
    let id = m.create_thread(None, s).unwrap();
    let seen = watch(&m);
    send(&m, &id, "hello");

    assert!(eventually(|| saw(&seen, "turn_ended")).await, "the turn runs");
    assert!(
        eventually(|| !limited(&m, "solo")).await,
        "a completed turn falsifies the window the gauge wrote: {:?}",
        state_of(&m, "solo")
    );
}

/// **A FALSE BAR MUST NOT NEED A TURN TO FALSIFY IT, BECAUSE THE BAR IS WHAT
/// PREVENTS ONE.** The 2026-10-06 incident, in its own shape.
///
/// The test above is the safety net the gauge-bar was shipped with, and its doc
/// comment says "the worst case of a wrong reading is one admission cycle, not a
/// window that has to expire on a foreign clock". That was wrong, and it was
/// wrong in a way only this case shows: `note_account_served` needs a COMPLETED
/// TURN, and a barred account cannot start one. The test above only passes
/// because it hands the thread `Thresholds::ungated()` and the account is the
/// only one in the pool.
///
/// On 2026-10-06 the operator's `owner-gmail` was barred until
/// `2026-10-07T03:19:00Z` by a misparsed session reset (24 hours from prose that
/// had passed by twenty-three seconds). At 03:44:43Z this very poller read
/// `session = 8%` of a fresh window, logged `account usage pct=8`, and left the
/// bar standing. `note_login_restored` clears only the `login` axis, so there
/// was no path back at all and three threads sat parked.
///
/// **NO TURN RUNS IN THIS TEST.** The only thing that happens between the bar
/// and the clear is a second reading, and that has to be enough.
#[tokio::test]
async fn a_bar_the_gauge_wrote_is_cleared_when_the_gauge_measures_the_quota_open() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();

    weekly_reads(tmp.path(), "solo", 100);
    m.probe_accounts_once(true).await.unwrap();
    assert!(limited(&m, "solo"), "the measured 100% bars it, as it must");
    assert_eq!(
        state_of(&m, "solo").limit_axis.as_deref(),
        Some(Axis::WeeklyAll.as_str())
    );

    // The window ended. Same account, same poller, same surface — a new reading.
    weekly_reads(tmp.path(), "solo", 34);
    m.probe_accounts_once(true).await.unwrap();

    assert!(
        !limited(&m, "solo"),
        "the poll that MEASURED 34% on the very quota the bar was recorded on has \
         falsified it; believing the number only in the barring direction is a ratchet, \
         and a bar nothing can contradict is what cost the operator his account: {:?}",
        state_of(&m, "solo")
    );
    assert!(
        !m.barred_accounts().0.contains(&"solo".to_string()),
        "and the clear reaches placement, not just the row"
    );
}

/// **THE CLEAR IS SCOPED TO THE QUOTA IT MEASURED.** A fresh session says
/// nothing about an exhausted week.
///
/// This is also guarded, incidentally but strictly, by
/// `a_measured_exhausted_reading_bars_the_account` above: one `/usage` response
/// carries all three axes, and `weekly_fable` (56%) and `session` (12%) are read
/// in the SAME loop iteration pass that writes the `weekly_all` bar. An
/// unscoped clear would delete that bar microseconds after writing it and every
/// barring test in this file would fail. Asserted on its own here anyway,
/// because "another test happens to cover it" is not where a safety property
/// should live.
#[tokio::test]
async fn a_measured_open_quota_never_clears_a_bar_on_a_different_quota() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();

    // `usage-weekly` drives weekly_all only; the fixture reports session at 12%
    // and weekly_fable at 56% alongside it, both well under the ceiling.
    weekly_reads(tmp.path(), "solo", 100);
    m.probe_accounts_once(true).await.unwrap();

    assert!(
        limited(&m, "solo"),
        "two open axes were measured in the same response as the exhausted one, and \
         neither may touch a bar that is not about them: {:?}",
        state_of(&m, "solo")
    );
    assert_eq!(
        state_of(&m, "solo").limit_axis.as_deref(),
        Some(Axis::WeeklyAll.as_str()),
        "and the bar still names the quota that is actually exhausted"
    );
}

/// **A LOGIN BAR IS NOT A QUOTA BAR AND A QUOTA READING MAY NOT LIFT IT.**
///
/// `limit_axis = "login"` means the credential failed to authenticate; only a
/// successful login lifts it (`note_login_restored`), and `accounts status`
/// prints an ask rather than a wait for it. An expired OAuth session will happily
/// report 12% of its session pool, so an unscoped clear would un-bar a login
/// that cannot serve a single turn — and would do it on every poll, for ever.
#[tokio::test]
async fn a_measured_open_quota_never_clears_a_login_bar() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();

    // The shape `note_login_health` writes: a far-future window on the `login`
    // axis, which no quota reading may contradict.
    m.store
        .set_account_limit(
            "solo",
            Some((
                "2027-01-01T00:00:00+00:00",
                Some("Failed to authenticate: OAuth session expired and could not be refreshed"),
                Some("login"),
                None,
            )),
        )
        .unwrap();
    assert!(limited(&m, "solo"));

    weekly_reads(tmp.path(), "solo", 34);
    m.probe_accounts_once(true).await.unwrap();

    assert!(
        limited(&m, "solo"),
        "a healthy quota reading does not re-authenticate anything: {:?}",
        state_of(&m, "solo")
    );
    assert_eq!(
        state_of(&m, "solo").limit_axis.as_deref(),
        Some("login"),
        "and the axis — which is what `accounts status` reads to print the ask — survives"
    );
}

/// **THE TRAP: the poller may not switch itself off.**
///
/// `gauge_needed()` used to be true only if a budget enforced, a thread carried
/// a threshold, or CALIBRATION still wanted a reading. On the live deployment
/// the third was the only one true — so the moment the last axis of the last
/// account calibrated, the sweep would refuse, the poller would stop, and the
/// fix above would become a silent no-op. Nothing enforces here, no thread
/// exists at all, and calibration is off; the sweep polls anyway, because the
/// gauge can bar and a barring mechanism that stops running is not one.
#[tokio::test]
async fn the_background_sweep_keeps_polling_once_calibration_is_done() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();

    // `false` is the BACKGROUND sweep's own argument — the one that used to
    // refuse.
    let rows = m
        .probe_accounts_once(false)
        .await
        .expect("the background sweep must still poll when the gauge can bar");
    // Three axes per LOGIN, and `poll_targets` polls the machine default too.
    assert!(rows >= 3, "at least one account's three axes, got {rows}");
}

/// …and it CAN be switched off, because it is configuration and not a law.
#[tokio::test]
async fn barring_on_the_gauge_is_a_configurable_ceiling() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(Config {
        gauge_bar_pct: 0.0,
        ..config_at(tmp.path(), &["exhausted"])
    })
    .unwrap();
    weekly_reads(tmp.path(), "exhausted", 100);

    m.probe_accounts_once(true).await.unwrap();
    assert!(
        !limited(&m, "exhausted"),
        "with `gauge_bar_pct` off the poller observes and does not bar"
    );
    assert!(
        m.probe_accounts_once(false).await.is_err(),
        "and with nothing else needing a reading, the sweep goes back to refusing"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 2. THE SUBAGENT SIGNAL — AND THE GUARD ON IT
// ═════════════════════════════════════════════════════════════════════════

/// **The six records that were parsed and dropped.**
///
/// The parent turn here is entirely healthy and ends successfully — exactly the
/// 19:33-to-19:36 shape. Its subagent dies on the weekly limit, and that alone
/// bars the account.
#[tokio::test]
async fn a_subagent_dying_on_a_usage_limit_bars_the_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["primary", "spare"])).unwrap();

    let mut s = settings(tmp.path());
    s.account = threads::settings::AccountChoice::Named("primary".into());
    let id = m.create_thread(None, s).unwrap();
    let seen = watch(&m);
    send(&m, &id, "limitsubagent please");

    assert!(eventually(|| saw(&seen, "task_updated")).await, "the record arrives");
    assert!(
        eventually(|| limited(&m, "primary")).await,
        "the subagent's 429 is the ACCOUNT's 429 — attribution is exact, because a \
         subagent inherits CLAUDE_CONFIG_DIR from its parent: {:?}",
        state_of(&m, "primary")
    );
    assert_eq!(
        state_of(&m, "primary").limit_axis.as_deref(),
        Some(Axis::WeeklyAll.as_str()),
        "and it names the quota that refused"
    );
    // The turn's ENDING becomes a 429 even though the parent's own stream never
    // produced one, so the existing retry + failover branch fires.
    assert!(
        eventually(|| saw(&seen, "thread_system_interrupted")).await,
        "DEC-56's hard break, from a signal the parent stream never carried"
    );
}

/// **THE FALSE-POSITIVE GUARD. A subagent that merely READ the word `resets`
/// must not bar a healthy account.**
///
/// `retry::classify_message`'s rate-limit branch matches the bare substring
/// `"resets"`. Routing the subagent's error through it would mean that any
/// subagent failing while reading this crate's own retry code takes the
/// account — and every label of the login — out of service. The arm therefore
/// requires a terminal status, a classifiable QUOTA, and a reset that parses to
/// a real instant.
#[tokio::test]
async fn a_benign_error_containing_resets_never_bars_the_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["primary", "spare"])).unwrap();

    let mut s = settings(tmp.path());
    s.account = threads::settings::AccountChoice::Named("primary".into());
    let id = m.create_thread(None, s).unwrap();
    let seen = watch(&m);
    send(&m, &id, "benignsubagent please");

    assert!(eventually(|| saw(&seen, "task_updated")).await, "the record arrives");
    assert!(eventually(|| saw(&seen, "turn_ended")).await, "and the turn finishes");
    // Settle: give the arm every chance to misfire before asserting it did not.
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(
        !limited(&m, "primary"),
        "the error says `resets` and nothing else — no quota, no time — and a healthy \
         account must survive a subagent reading source code: {:?}",
        state_of(&m, "primary")
    );
    assert!(
        !saw(&seen, "thread_system_interrupted"),
        "and nothing was interrupted"
    );
    assert_eq!(count(&seen, "turn_ended"), 1, "no retry ladder was started");
}

/// The guard, stated directly against every shape the CLI actually emits.
///
/// 2,407 captured `task_updated` patches: 2,095 `{status,end_time}`, 183
/// `{is_backgrounded}` with no status at all, 129 adding `error`.
#[test]
fn the_subagent_guard_admits_only_a_real_usage_limit() {
    let real = "Agent terminated early due to an API error: You've hit your weekly limit \
                · resets Aug 2 at 8pm (America/Chicago)";

    // The one true positive.
    let (axis, reset) = subagent_limit_signal(Some("failed"), Some(real), None)
        .expect("a terminal failure naming a quota and a parseable reset");
    assert_eq!(axis, Axis::WeeklyAll);
    assert_eq!(reset, "Aug 2 at 8pm (America/Chicago)");
    // `killed` is terminal too.
    assert!(subagent_limit_signal(Some("killed"), Some(real), None).is_some());

    // …and every rejection.
    assert!(
        subagent_limit_signal(Some("completed"), Some(real), None).is_none(),
        "a completed task is not a death, whatever its text says"
    );
    assert!(
        subagent_limit_signal(None, Some(real), Some(true)).is_none(),
        "an `is_backgrounded` patch carries no status and is not a death"
    );
    assert!(
        subagent_limit_signal(Some("failed"), None, None).is_none(),
        "no error text, no claim"
    );
    assert!(
        subagent_limit_signal(Some("failed"), Some("   "), None).is_none(),
        "and empty text is not text"
    );
    assert!(
        subagent_limit_signal(
            Some("failed"),
            Some("the rate-limit branch matches the bare substring resets"),
            None
        )
        .is_none(),
        "THE FALSE POSITIVE: `resets` with no quota and no time is prose, not a refusal"
    );
    assert!(
        subagent_limit_signal(
            Some("failed"),
            Some("You've hit your weekly limit, resets soon"),
            None
        )
        .is_none(),
        "a quota with no PARSEABLE reset is refused — `note_account_limit` would write \
         nothing anyway, and a bar nobody can date is a bar nobody can lift"
    );
    assert!(
        subagent_limit_signal(Some("failed"), Some("resets at 8pm"), None).is_none(),
        "a time with no quota is refused: nothing to write into `limit_axis`"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 3. THE MID-TURN INTERRUPT
// ═════════════════════════════════════════════════════════════════════════

/// **A turn running on an account that has just been barred is torn down and
/// handed back to the balancer — and it finishes, on the other account.**
///
/// The thirty-two minutes. The poller learns the account is exhausted while the
/// turn is in flight; the flag is raised, the tail loop tears the execution
/// down through the SAME teardown a budget kill uses, and `run_turn` routes the
/// result into the EXISTING 429 branch — `note_account_limit` → `admit_or_park`
/// → `interrupt_thread` → continue. No new placement logic anywhere.
#[tokio::test(flavor = "multi_thread")]
async fn a_barred_account_interrupts_the_turn_running_on_it() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["hot", "cool"])).unwrap();
    weekly_reads(tmp.path(), "hot", 100);
    weekly_reads(tmp.path(), "cool", 10);

    // AUTO, not `Named`. A named account is a PIN, and a pin is never
    // re-placed — `failover_target` refuses one outright, which is correct and
    // is also not the case under test. An AUI thread sends `"account":"auto"`,
    // so the thread is steered onto `hot` the way the balancer steers anything:
    // by making `hot` the better account at create time.
    m.budgets
        .observe("hot", Axis::Session, 1.0, None, "usage_poll")
        .unwrap();
    m.budgets
        .observe("cool", Axis::Session, 99.0, None, "usage_poll")
        .unwrap();
    let s = settings(tmp.path());
    assert!(
        s.limit_kill_mid_turn,
        "the operator asked for this ON by default for AUI threads (2026-08-15)"
    );
    let id = m.create_thread(None, s).unwrap();
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "hot",
        "placement put it on the account that is about to be exhausted"
    );
    let seen = watch(&m);
    // The tempdir is in the PROMPT, deliberately. The fixture keys its
    // once-then-succeed marker on a checksum of the prompt, and that marker
    // lives in a shared `/tmp` — so two tests using the same words, or one test
    // run twice, would share a marker and the second would take the retry
    // branch on its first call. A path that is unique per run makes the token
    // unique per run.
    send(&m, &id, &format!("midturnkill failover {}", tmp.path().display()));

    // The turn is DEFINITELY executing before the gauge speaks.
    assert!(
        eventually(|| saw(&seen, "turn_started")).await,
        "the turn started"
    );
    assert!(
        eventually(|| m.store.list_running_turns().unwrap().iter().any(|r| r.thread_id == id))
            .await,
        "…and there is a real child process to interrupt"
    );

    // 19:26:13.
    m.probe_accounts_once(true).await.unwrap();
    assert!(limited(&m, "hot"), "the poll barred the account under the turn");

    assert!(
        eventually(|| saw(&seen, "turn_killed")).await,
        "the execution was torn down mid-turn rather than left to run to a 429 it was \
         never going to survive: {:?}",
        seen.lock().unwrap().iter().map(|e| e.event.kind()).collect::<Vec<_>>()
    );
    assert!(
        eventually(|| saw(&seen, "thread_system_interrupted")).await,
        "DEC-56: the thread is FLAGGED and handed back"
    );

    // CON-4 and the point of the whole exercise: the work lands, on the other
    // account, without the operator saying anything again.
    assert!(
        eventually(|| m.serving_account(&id).unwrap() == "cool").await,
        "the balancer moved it: {:?} (parked: {:?})",
        m.serving_account(&id),
        m.store.list_parked_turns().unwrap()
    );
    assert!(
        eventually(|| count(&seen, "turn_ended") >= 2).await,
        "and the retry ran"
    );
    // ── THIS IS THE RESUME TEST ──
    //
    // The fixture's second call EXITS NON-ZERO unless it was invoked with
    // `--resume <session>` AND the transcript's last record is still the
    // mid-stream assistant turn it wrote before the SIGKILL. So a green here is
    // the substantive claim: `--resume` replays a conversation that was killed
    // mid-thought, and `trimmable_tail` refused to roll that real work off.
    // A `turn_failed` is how that claim comes back false.
    assert!(
        eventually(|| saw(&seen, "result_turn")).await && !saw(&seen, "turn_failed"),
        "the resumed turn SUCCEEDED: {:?}",
        seen.lock().unwrap().iter().map(|e| e.event.kind()).collect::<Vec<_>>()
    );
    // …and the killed turn's own work is still on the conversation.
    let tx = std::fs::read_to_string(
        std::fs::read_dir(tmp.path().join("cool").join("projects"))
            .unwrap()
            .filter_map(|e| e.ok())
            .flat_map(|e| std::fs::read_dir(e.path()).into_iter().flatten().filter_map(|f| f.ok()))
            .map(|f| f.path())
            .find(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .expect("the conversation was PORTED to the account it moved to"),
    )
    .unwrap();
    assert!(
        tx.contains("half a thought"),
        "the mid-stream assistant record survived the SIGKILL and the move"
    );
}

/// **It is a FLAG.** Off, the turn is left alone — today's behaviour, still
/// reachable, because `TripAction::Finish`'s reasoning ("killing mid-turn can
/// leave half-applied edits behind") did not stop being true.
#[tokio::test(flavor = "multi_thread")]
async fn the_mid_turn_interrupt_can_be_turned_off_per_thread() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["hot", "cool"])).unwrap();
    weekly_reads(tmp.path(), "hot", 100);

    let mut s = settings(tmp.path());
    s.account = threads::settings::AccountChoice::Named("hot".into());
    s.limit_kill_mid_turn = false;
    let id = m.create_thread(None, s).unwrap();
    let seen = watch(&m);
    send(&m, &id, "slowturn please");

    assert!(eventually(|| saw(&seen, "turn_started")).await);
    m.probe_accounts_once(true).await.unwrap();
    assert!(limited(&m, "hot"), "the account is barred either way");

    assert!(
        eventually(|| saw(&seen, "turn_ended")).await,
        "the turn finishes on its own"
    );
    assert!(
        !saw(&seen, "turn_killed"),
        "…and was never torn down: the flag is off"
    );
}

/// **The prompt survives.** CON-4, on the interrupt path specifically: the
/// interrupted turn's message is still the thread's, whether the balancer
/// re-ran it immediately or the ladder took it.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_turn_never_loses_its_prompt() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // ONE account, so there is nowhere to fail over to and the ladder — not a
    // lucky re-placement — is what has to hold the work.
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    weekly_reads(tmp.path(), "solo", 100);

    let mut s = settings(tmp.path());
    s.account = threads::settings::AccountChoice::Named("solo".into());
    let id = m.create_thread(None, s).unwrap();
    let seen = watch(&m);
    send(&m, &id, &format!("midturnkill solo {}", tmp.path().display()));

    assert!(eventually(|| saw(&seen, "turn_started")).await);
    assert!(
        eventually(|| m.store.list_running_turns().unwrap().iter().any(|r| r.thread_id == id))
            .await
    );
    m.probe_accounts_once(true).await.unwrap();

    assert!(eventually(|| saw(&seen, "turn_killed")).await, "torn down");
    assert!(
        eventually(|| {
            // Either a durable retry row or a parked turn — both are homes the
            // prompt is redelivered from; what may never happen is neither.
            m.store.get_retry(&id).unwrap().is_some()
                || m.store
                    .list_parked_turns()
                    .unwrap()
                    .iter()
                    .any(|p| p.thread_id == id)
        })
        .await,
        "a killed turn's prompt has a durable home (CON-4) — it is never dropped"
    );
}

/// **The interrupt path still contains no placement logic.**
///
/// `tests/system_events.rs` guards `interrupt_thread`'s body. This guards the
/// two new pieces that sit beside it: the flag-raiser and the teardown block
/// must not have become a second, private balancer. DEC-56 as amended, msg
/// #1412: "you don't directly attempt to put it in another account. You put it
/// back to the balancer."
#[test]
fn the_kill_flag_and_its_teardown_contain_no_placement_logic() {
    let src = include_str!("../../src/runtime.rs");

    let start = src
        .find("    fn raise_limit_kill(")
        .expect("raise_limit_kill was renamed — re-point this test");
    let body = &src[start..];
    let end = body
        .find("fn limit_kill_enabled(")
        .expect("the item after raise_limit_kill moved — re-point this test");
    let raiser = &body[..end];

    let start = src
        .find("if state.limit_kill.swap(false, Ordering::SeqCst) {")
        .expect("the tail loop's limit-kill block moved — re-point this test");
    let teardown = &src[start..start + 2_000];

    for (what, body) in [("the flag raiser", raiser), ("the teardown block", teardown)] {
        for banned in [
            "resolve_account",
            "selectable_pool",
            "move_thread",
            "failover",
            "PlacementRequest",
            "AdmissionRequest",
            "admit_or_park",
            "self.budgets",
            "self.registry",
            ".place(",
            ".admit(",
        ] {
            assert!(
                !body.contains(banned),
                "{what} grew placement logic ({banned:?}). The interrupt path flags and \
                 tears down; `admit_or_park` decides where the work goes."
            );
        }
    }
    // …and the raiser does the one thing it IS for.
    assert!(raiser.contains("limit_kill.swap(true"), "it raises the flag");
    assert!(
        raiser.contains("limit_kill_enabled"),
        "…only where the flag is enabled"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 4. THE AUI DEFAULTS
// ═════════════════════════════════════════════════════════════════════════

/// **Every AUI thread carries a weekly threshold of 95, and the mid-turn
/// interrupt, without AUI sending either.**
///
/// AUI's `build_thread_settings` sends `cwd`, `model`, `account`,
/// `append_system_prompt`, `add_dirs`, `permission_mode`, `plugin_roots`,
/// `inject_context`, `app_id` and `metadata` — and nothing else, ever. So both
/// defaults have to live here, in the crate, or they do not exist. The blob
/// below is AUI's, verbatim in shape.
#[test]
fn an_aui_thread_inherits_the_weekly_threshold_and_the_interrupt() {
    let aui = serde_json::json!({
        "cwd": "/Users/testuser/minds/mythcellaneous",
        "model": "fable",
        "account": "auto",
        "add_dirs": [],
        "plugin_roots": [],
        "inject_context": true,
        "app_id": "aui",
        "metadata": { "aui_project": "mythcellaneous" },
    });
    let s: ThreadSettings = serde_json::from_value(aui).unwrap();

    assert_eq!(
        s.thresholds.weekly,
        Some(threads::budgets::DEFAULT_WEEKLY_THRESHOLD_PCT),
        "margin below the cliff, so a thread parks while the account can still serve"
    );
    assert!(s.limit_kill_mid_turn, "and the interrupt, per the operator");
}

/// **THE BACKFILL.** Every thread already in the live database serializes
/// `"thresholds":{}` — no `weekly` key — so the default has to sit inside
/// `Thresholds` rather than on the field, or 129 live threads inherit nothing.
/// No migration, no restart.
#[test]
fn a_thread_stored_before_this_change_inherits_the_threshold_on_read() {
    let stored = serde_json::json!({
        "cwd": "/tmp", "model": "fable",
        "thresholds": {},
        "budget_key": "aui:default",
    });
    let s: ThreadSettings = serde_json::from_value(stored).unwrap();
    assert_eq!(
        s.thresholds.weekly,
        Some(threads::budgets::DEFAULT_WEEKLY_THRESHOLD_PCT)
    );
    assert!(s.limit_kill_mid_turn);
}

/// **…and a deliberate opt-out round-trips.**
///
/// The hazard the `budget_key` doc names: with a non-`None` serde default, a
/// deliberate unset that is serialized by being LEFT OUT reads back as the
/// default and silently undoes the operator's act. So `Thresholds::weekly`
/// alone among the four is written even when `None`.
#[test]
fn an_explicit_null_weekly_threshold_survives_a_round_trip() {
    let s: ThreadSettings = serde_json::from_value(serde_json::json!({
        "cwd": "/tmp", "model": "fable",
        "thresholds": { "weekly": null },
    }))
    .unwrap();
    assert_eq!(s.thresholds.weekly, None, "an explicit null ungates");

    let round: ThreadSettings =
        serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(
        round.thresholds.weekly, None,
        "and it is still ungated after a trip through the database — absent means \
         inherit, explicit null means ungated, and the two stay distinguishable on disk"
    );
}

/// **`Thresholds::default()` and `from_str(\"{}\")` agree.**
/// A derived `Default` would have given `None` while an empty JSON object gave
/// 95, so every consumer that built settings in Rust would be ungated and every
/// consumer that came over the wire would be gated.
#[test]
fn the_rust_default_and_the_wire_default_are_the_same_thresholds() {
    let wire: Thresholds = serde_json::from_str("{}").unwrap();
    assert_eq!(wire, Thresholds::default());
    assert_eq!(
        Thresholds::ungated(),
        Thresholds {
            session: None,
            fable: None,
            weekly: None,
            cash: None
        }
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
