//! The three prerequisites calibration-aware account selection is blocked on
//! (harness artifact `account-selection-design`, §1b and §3.1–3.2).
//!
//! Every test here drives the daemon in-process against the FAKE claude fixture.
//! No real claude turn is ever spawned, including the `/usage` poll; `THREADS_HOME`
//! is never resolved (every manager gets an explicit `data_dir` under a tempdir),
//! so `~/.threads` cannot come into existence; and no test touches the operator's
//! live session, a real login, or the keychain.
//!
//! 1. **A thread's account is STICKY** — `resolve_account` was re-run on every
//!    rehydration (any daemon restart) and on every `threads.update` patch, which
//!    pulled a failed-over thread back to the alphabetically first account and
//!    restarted its conversation inside that account's config dir. DEC-26 says the
//!    opposite: a thread with history goes back where the conversation lives, or
//!    waits.
//!
//! 2. **The gauge actually polls, at the EDGES of our activity** — nothing polled
//!    `/usage` at all in the production configuration (no budget key, no
//!    threshold), so there were zero brackets and therefore zero calibration
//!    samples however the threads were arranged. Polling when calibration wants a
//!    reading fixes the first half; polling at idle transitions and before
//!    dispatch fixes the second, by putting a poll in the adjacent idle gap BY
//!    CONSTRUCTION instead of hoping a 15-minute sweep lands in it.
//!
//! 3. **The registry knows identity and health** — two labels for one login
//!    counted as two accounts (so every pool count and exclusivity claim over them
//!    was false), and a login whose OAuth session cannot be refreshed stayed a
//!    live failover candidate handing threads a hard auth failure.

#[path = "support/keychain.rs"]
mod keychain;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use threads::settings::{SettingsPatch, ThreadSettings};
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
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "200");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("THREADS_EDGE_PAIR_GAP_MS", "10");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    // The machine-default account's transcript store is `<HOME>/.claude` — the
    // OPERATOR'S OWN login. Point the transcript home at a scratch dir so no
    // test can read or write inside it (account moves now port transcripts).
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// A daemon whose one extra account, `probe`, has a config dir holding a
/// `usage-seq` file: the `/usage` fixture walks those session percentages, one
/// per poll, and then holds at the last. Per-config-dir rather than per-env-var
/// so two of these can run concurrently in one test process.
fn probing_manager(dir: &std::path::Path, seq: &str, ladder_secs: Vec<f64>) -> ThreadManager {
    let acct = dir.join("probe");
    std::fs::create_dir_all(&acct).unwrap();
    std::fs::write(acct.join("usage-seq"), seq).unwrap();
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert("probe".to_string(), acct.to_string_lossy().to_string());
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    build_manager(Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        probe_interval_secs: 0,
        calibrate: true,
        edge_poll: true,
        settle_ladder_secs: ladder_secs,
        ..Default::default()
    })
    .unwrap()
}

/// A thread PINNED to the `probe` account, so the test drives exactly the
/// account whose readings it scripted.
fn probing_thread(m: &ThreadManager, cwd: &std::path::Path) -> String {
    let mut s = settings(cwd);
    s.account = threads::settings::AccountChoice::Named("probe".to_string());
    let id = m.create_thread(None, s).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "probe");
    id
}

/// A config over `dir`, with `extra` additional registered accounts. The machine
/// `default` is always registered too (`ensure_default`), so `extra: ["zz-second"]`
/// gives a two-label pool whose alphabetically FIRST label is `default`.
fn config_at(dir: &std::path::Path, extra: &[&str]) -> Config {
    let mut accounts = std::collections::BTreeMap::new();
    for name in extra {
        // A config dir that does not exist is fine: the fixture ignores
        // CLAUDE_CONFIG_DIR entirely, and an unreadable dir simply yields no
        // identity fingerprint — which is never treated as a match.
        accounts.insert(
            name.to_string(),
            dir.join(name).to_string_lossy().to_string(),
        );
    }
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        // Off unless a test is about polling: these tests are about selection,
        // and a real edge poll would add readings they do not reason about.
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn settings(cwd: &std::path::Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

/// Wait for `f` to hold, polling every 25ms. Returns false on timeout.
async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..400 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

fn durable_account(m: &ThreadManager, id: &str) -> Option<String> {
    m.store.get_thread(id).unwrap().unwrap().account
}

// ── 1. stickiness (design §1b — a live DEC-26 violation) ─────────────────────

/// A thread that has failed over stays on the account its conversation lives on,
/// across a daemon restart AND across a routine settings patch.
///
/// Both re-resolution sites used to re-run `Auto`, which means "the
/// alphabetically first registered account, unconditionally": the thread was
/// pulled back to `default`, and because the claude session for a thread id lives
/// inside one account's config dir, that silently restarted the conversation.
/// AUI patches settings for routine things — floating model tiers make a model
/// change a `threads.update` — so this fired constantly, not rarely.
#[tokio::test]
async fn a_failed_over_thread_is_not_yanked_back_by_a_restart_or_a_settings_patch() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // CON-27: `aa-first` plays the role the machine `default` used to play here
    // — the account that sorts first among the ones threads may actually place
    // on. The machine default is not one of them any more.
    let cfg = config_at(tmp.path(), &["aa-first", "zz-second"]);
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(cfg.clone()).unwrap();

    // `Auto` picks the first selectable account at CREATE time.
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "aa-first");

    // A 429 fails the thread over to the other label…
    m.send(
        &id,
        "ratelimit please".to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();
    assert!(
        eventually(|| m.serving_account(&id).unwrap() == "zz-second").await,
        "the 429 failover must move the thread: {:?}",
        m.serving_account(&id)
    );
    assert_eq!(durable_account(&m, &id).as_deref(), Some("zz-second"));

    // …and a routine settings patch (a model change, which is what AUI sends)
    // must leave it exactly there. Before the fix this re-resolved `Auto` and
    // wrote `aa-first` back over it.
    let patch = SettingsPatch {
        model: Some("sonnet".to_string()),
        ..Default::default()
    };
    m.update_settings(&id, patch).unwrap();
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "zz-second",
        "a settings patch that says nothing about the account must not move the thread"
    );
    assert_eq!(durable_account(&m, &id).as_deref(), Some("zz-second"));

    // A DAEMON RESTART: a brand-new manager over the same data dir, so the
    // thread is rehydrated from the store with an empty in-memory map.
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m2 = build_manager(cfg).unwrap();
    assert_eq!(
        m2.serving_account(&id).unwrap(),
        "zz-second",
        "rehydration must not re-pick the alphabetically first account"
    );
    assert_eq!(durable_account(&m2, &id).as_deref(), Some("zz-second"));

    // Stickiness is not stubbornness: an EXPLICIT account patch is honoured (a
    // pin outranks everything), and so is an account that has left the registry.
    // CON-27 narrows what a pin may NAME, not whether a pin wins: `aa-first` is
    // placeable, so it is honoured exactly as before.
    let patch = SettingsPatch {
        account: Some(threads::settings::AccountChoice::Named("aa-first".into())),
        ..Default::default()
    };
    m2.update_settings(&id, patch).unwrap();
    assert_eq!(m2.serving_account(&id).unwrap(), "aa-first");

    // ── STICKINESS IS NOW UNCONDITIONAL, AND THIS ASSERTION IS INVERTED ──
    //
    // This block used to assert `assert_ne!(landed, "retired-label")` —
    // *"an un-registered account is the one condition that breaks stickiness"*.
    // That condition is REMOVED (2026-08-27) and the reason is not cosmetic.
    //
    // When the machine default stopped being registered, that condition started
    // firing on every thread stranded on it. There are 119 of those on the
    // operator's machine. Boot recovery would have moved all 119 account
    // columns WITHOUT carrying their transcripts — silent reassignment, in
    // bulk, to conversations he still has. DEC-56 forbids exactly that, and the
    // old rule's justification ("staying would mean spawning with a config dir
    // that does not exist") turns out to be false: NOTHING SPAWNS WITHOUT
    // PASSING THE GATE, and the gate cannot admit a thread on an account
    // outside the pool — it parks for continuity and `failover_target` moves it
    // BY PORTING.
    //
    // So the thread stays put here, and moving it is the gate's job, one layer
    // up, where the transcript goes with it. That is strictly better than what
    // this test used to pin: the old behaviour orphaned the conversation.
    let tmp2 = tempfile::tempdir().unwrap();
    let cfg2 = config_at(tmp2.path(), &["aa-first", "zz-second"]);
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m3 = build_manager(cfg2).unwrap();
    let orphan = m3.create_thread(None, settings(tmp2.path())).unwrap();
    let mut row = m3.store.get_thread(&orphan).unwrap().unwrap();
    row.account = Some("retired-label".to_string());
    m3.store.upsert_thread(&row).unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m4 = build_manager(config_at(tmp2.path(), &["aa-first", "zz-second"])).unwrap();
    let landed = m4.serving_account(&orphan).unwrap();
    assert_eq!(
        landed, "retired-label",
        "boot recovery must NOT reassign a thread's account column — that moves it \
         without its transcript. The gate ports it off on its next turn."
    );
    assert!(durable_account(&m4, &orphan).as_deref() == Some("retired-label"));
    // …and it is emphatically not somewhere anything may be PLACED, so the gate
    // will have to move it.
    assert!(!m4.is_placeable("retired-label"));
}

// ── 2. the gauge polls, at the edges (design §3.1.3 + §3.2) ──────────────────

/// The background sweep polls because CALIBRATION wants a reading — no budget
/// key, no threshold, nothing else on the machine.
///
/// This is the defect that dominated everything else in the area: the sweep only
/// polled when a budget key or a per-thread threshold existed, AUI sets neither,
/// so in the real deployment no `/usage` observation was ever recorded. No polls
/// means no brackets, which means no samples and `tokens_per_percent` absent
/// forever, however the threads are arranged.
#[tokio::test]
async fn the_background_sweep_polls_because_calibration_wants_a_reading() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let mut accounts = std::collections::BTreeMap::new();
    // CON-27: the sweep's precondition is "some axis of some SELECTABLE account
    // is uncalibrated", and selectable now means placeable — an account threads
    // OWNS, with its own config dir. Registering nothing at all leaves only the
    // machine `default`, which is registered and visible but never selectable,
    // so the sweep would correctly find nothing it is allowed to poll for and
    // the defect this test guards would be invisible again.
    accounts.insert(
        "aa-first".to_string(),
        tmp.path().join("aa-first").to_string_lossy().to_string(),
    );
    let cfg = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        probe_interval_secs: 0,
        // The PRODUCTION defaults, stated explicitly.
        calibrate: true,
        edge_poll: true,
        ..Default::default()
    };
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(cfg).unwrap();

    // Not forced — this is the timed sweep's own precondition, and nothing on
    // this machine has a budget key or a threshold.
    let n = m
        .probe_accounts_once(false)
        .await
        .expect("an uncalibrated axis is a reason to poll");
    // Three axes per /usage poll, and ONE account is polled. This asserted 6 —
    // `aa-first` and the machine `default` — on the reasoning that the machine
    // default is *"still registered, still visible and still metered, so a
    // reading is still taken for it"*. That reading was taken by SPAWNING
    // `claude` with `CLAUDE_CONFIG_DIR` unset, i.e. against `~/.claude`, the
    // operator's own terminal login, on a timer. *"Threads should never access
    // my default account."* It is not polled, and it is not in the pool.
    assert_eq!(n, 3, "three axes per account, from one /usage poll each");
    let pool = m.account_pool();
    assert_eq!(pool, vec!["aa-first"]);
    let g = m.budgets.gauge_state(&pool).unwrap();
    assert_eq!(g[0]["axes"]["session"]["observed_pct"], 12.0);
    assert_eq!(g[0]["axes"]["session"]["source"], "usage_poll");
}

/// EDGE POLLING: a poll lands in the idle gap on each side of a window by
/// construction, so the window is bracketed and a sample is recorded — and the
/// closing ladder measures the settle delay while it is at it.
///
/// The scripted meter reads 41% twice in the leading idle gap (the opening pair —
/// agreeing, which is what lets `IdleMovement` witness quiet rather than stay
/// silent), then 45% for the whole closing sequence: a meter that had already
/// caught up when the first ladder rung read it.
#[tokio::test]
async fn edge_polls_bracket_a_window_and_measure_the_settle_delay() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // A ladder in fractions of a second, so the test does not wait two minutes
    // for what production spends on three free polls.
    let m = probing_manager(tmp.path(), "41,41,45,45,45", vec![0.04, 0.12, 0.24]);
    let id = probing_thread(&m, tmp.path());

    m.send(
        &id,
        "hello".to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();

    // The closing ladder runs in the trailing idle gap, then reports where the
    // reading stopped moving.
    assert!(
        eventually(|| m.settle_profile("probe").is_some()).await,
        "the closing edge must walk a settle ladder"
    );
    let profile = m.settle_profile("probe").unwrap();
    // Two rungs, not three: the ladder stops the moment two consecutive readings
    // agree, because a third can only confirm what the second already showed.
    assert_eq!(
        profile.rungs.len(),
        2,
        "the ladder stops at the first agreement: {}",
        profile.describe()
    );
    assert!(!profile.still_moving, "{}", profile.describe());
    assert_eq!(
        profile.settled_after_ms,
        Some(40),
        "the meter had already caught up at the first rung: {}",
        profile.describe()
    );
    assert_eq!(profile.settled_after_secs(), Some(0.04));

    // And the window it bracketed produced a real calibration sample — the thing
    // that was structurally impossible before, because nothing polled at all.
    assert!(
        eventually(|| m
            .store
            .get_estimate("probe", "session")
            .unwrap()
            .is_some())
            .await,
        "an edge-bracketed window must calibrate: {:#}",
        m.budgets.estimation_state().unwrap()
    );
    let est = m.store.get_estimate("probe", "session").unwrap().unwrap();
    // 41% -> 45% is a 4-point delta over the turn's own 130 tokens.
    assert!((est.tokens_per_percent - 130.0 / 4.0).abs() < 0.51, "{est:?}");
    assert_eq!(est.sample_count, 1);

    // Both edges left their pair behind: the closing pair is the leading gap of
    // whatever window comes next, so the NEXT dispatch pays no opening poll at
    // all. In steady state edge polling costs the ladder and nothing more.
    assert!(
        eventually(|| m.polls_in_idle_gap("probe") == 2).await,
        "the closing PAIR is what witnesses quiet, so both halves are recorded: {}",
        m.polls_in_idle_gap("probe")
    );
}

/// A meter that has NOT finished settling never closes the bracket — so the
/// sample is taken from the SETTLED delta, and the limit is not over-estimated.
///
/// This is why no settle-delay constant is hard-coded anywhere. A closing reading
/// taken while the meter is still catching up understates the delta; an
/// understated delta divides into an overstated tokens-per-percent; and an
/// overstated limit is one that never trips. Here the meter reads 43% when the
/// first rung looks and 45% by the second. The ladder reads BOTH without
/// recording either — a raw read is free and cannot become bracket material — and
/// only the settled 45% is written as an observation.
///
/// The number is the assertion: 4 points of movement, not 2. Recording the first
/// rung would have produced exactly twice the tokens-per-percent, i.e. twice the
/// limit, i.e. a budget that trips at 200%.
#[tokio::test]
async fn a_still_settling_meter_does_not_close_the_bracket_early() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = probing_manager(tmp.path(), "41,41,43,45,45", vec![0.04, 0.12, 0.24]);
    let id = probing_thread(&m, tmp.path());
    m.send(
        &id,
        "hello".to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();

    assert!(
        eventually(|| m
            .settle_profile("probe")
            .map(|p| p.rungs.len() == 3)
            .unwrap_or(false))
            .await,
        "the ladder must keep reading while the meter moves"
    );
    let profile = m.settle_profile("probe").unwrap();
    // The MEASUREMENT: still climbing when the first rung looked, level by the
    // second, so the settle delay is the second rung — measured, not assumed.
    assert_eq!(profile.settled_after_ms, Some(120), "{}", profile.describe());
    assert!(!profile.still_moving, "{}", profile.describe());
    assert!(
        (profile.moved_pct - 2.0).abs() < 1e-9,
        "two points of settle lag: {}",
        profile.describe()
    );

    // …and the sample it went on to record spans the SETTLED delta.
    assert!(
        eventually(|| m
            .store
            .get_estimate("probe", "session")
            .unwrap()
            .is_some())
            .await,
        "a settled bracket must calibrate: {:#}",
        m.budgets.estimation_state().unwrap()
    );
    let est = m.store.get_estimate("probe", "session").unwrap().unwrap();
    assert!(
        (est.tokens_per_percent - 130.0 / 4.0).abs() < 0.51,
        "41% -> 45% (settled), NOT 41% -> 43% (still moving), which would have \
         estimated a limit twice too large: {est:?}"
    );
}

/// A ladder that runs out while the meter is still moving records NOTHING: the
/// window stays unbracketed rather than calibrating from a reading in motion.
///
/// A refused sample costs one free poll. A wrong one silently disables a budget,
/// so under-confident is the only acceptable direction here.
#[tokio::test]
async fn a_ladder_that_never_settles_records_no_closing_poll() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // The meter climbs on every single read — the ladder can never see two
    // consecutive readings agree.
    let m = probing_manager(tmp.path(), "41,42,43,44,45,46,47,48", vec![0.04, 0.12, 0.24]);
    let id = probing_thread(&m, tmp.path());
    m.send(
        &id,
        "hello".to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();

    assert!(
        eventually(|| m
            .settle_profile("probe")
            .map(|p| p.rungs.len() == 3)
            .unwrap_or(false))
            .await,
        "the whole ladder must be walked before giving up"
    );
    let profile = m.settle_profile("probe").unwrap();
    assert!(profile.still_moving, "{}", profile.describe());
    assert_eq!(profile.settled_after_ms, None, "{}", profile.describe());
    assert!(profile.describe().contains("STILL MOVING"));

    // No closing observation was recorded, so no sample exists — and the leading
    // gap of the next window is left empty, which is what makes the next opening
    // edge pay for its own pair.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        m.store.get_estimate("probe", "session").unwrap().is_none(),
        "a meter in motion must not calibrate: {:#}",
        m.budgets.estimation_state().unwrap()
    );
    assert_eq!(
        m.polls_in_idle_gap("probe"),
        0,
        "the unsettled ladder recorded nothing at all"
    );

    // ── THE POSITIVE CONTROL, ADDED 2026-09-26 ───────────────────────────────
    //
    // Both assertions above read ZERO after a fixed sleep, and a test that
    // asserts nothing happened proves nothing unless something CAN happen and is
    // shown to. The controlling case lived in a SIBLING test, which means this
    // one would still pass against a build where calibration had stopped
    // working entirely — and "it did not calibrate" is exactly what such a build
    // and a correct one both look like from here.
    //
    // A second manager, over its own directory, differing ONLY in the scripted
    // meter: one that levels off. Everything else — the ladder, the account, the
    // send — is identical, so the difference in outcome is attributable to the
    // one thing that changed.
    let settling_tmp = tempfile::tempdir().unwrap();
    let s = probing_manager(settling_tmp.path(), "41,41,43,45,45", vec![0.04, 0.12, 0.24]);
    let sid = probing_thread(&s, settling_tmp.path());
    s.send(
        &sid,
        "hello".to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();
    assert!(
        eventually(|| s
            .store
            .get_estimate("probe", "session")
            .unwrap()
            .is_some())
            .await,
        "CONTROL: a meter that DOES settle calibrates on this fixture, so the two \
         zeroes above are facts about the unsettled ladder and not about a \
         calibration path that never fires: {:#}",
        s.budgets.estimation_state().unwrap()
    );
    assert!(
        s.polls_in_idle_gap("probe") > 0,
        "CONTROL: and it records a closing poll in the idle gap, which is the \
         second thing asserted at zero above"
    );
}

// ── 3. identity + health (design §3.1) ───────────────────────────────────────

/// **A login that cannot authenticate leaves the PLACEMENT pool, says why, and
/// the turn moves — the wave-two contract.**
///
/// # This test's expectation was changed deliberately on 2026-08-26
///
/// It used to assert that an auth failure raised `AccountState::dead_since` and
/// that the account left `selectable_pool` while the thread STAYED on it
/// (`unavailable` empty, "the thread already on it keeps taking its turns
/// there, which is the only thing that ever clears the flag"). That design was
/// coherent and it is now SUPERSEDED, on the operator's ruling:
///
/// > *"we need to fold that in as one of the failure and retry mechanisms that
/// > bars an account for a while."* — with the turn retried elsewhere.
///
/// The two cannot both hold. `dead_since` is clearable ONLY because it pins the
/// thread in place to do the clearing; the moment the turn is failed over, no
/// thread is left on the account to succeed there and `placeable_pool` refuses
/// to send a new one, so the flag would latch for good. So login expiry now
/// takes the ordinary account bar — `limit_until`, every label of the login,
/// `limit_axis = "login"` — which expires and which `note_account_served`
/// lifts. See `retry.rs::LOGIN_BAR_SECS` and `tests/retry_closure.rs`.
///
/// What did NOT change: the detection predicate is still
/// `accounts::is_dead_login_error`, still narrow, and a rate limit, an overload
/// and an exhausted credit balance still must not take a working account out of
/// the pool.
///
/// FORCED RED (two mutations, both observed):
///   * removing the `is_dead_login_error` block from `classify_message` fails at
///     the bar assertion — the failure lands on the deterministic ladder, which
///     bars nothing;
///   * re-adding `note_login_health(&state, Some(&reason))` to the login branch
///     fails the `healthy` assertion, which is the guard against reintroducing
///     the latch this wave removed.
#[tokio::test]
async fn a_dead_login_leaves_the_pool_and_stays_visible() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-first", "zz-second"])).unwrap();

    assert_eq!(m.selectable_pool(), vec!["aa-first", "zz-second"]);
    // TWO, not three: the third was the machine default, which is no longer in
    // the pool and therefore no longer counted as capacity the daemon holds.
    assert_eq!(m.distinct_healthy_logins(), 2);

    // Ceiling 1: this test is about the BAR, not the length of the ladder.
    let mut st = settings(tmp.path());
    st.retry_policy.max_login_attempts = 1;
    let id = m.create_thread(None, st).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "aa-first");
    m.send(
        &id,
        "authfail please".to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();

    // THE BAR LANDS. Through `limit_until` — the crate's one bar — which is what
    // `unavailable` reads and what placement is handed.
    assert!(
        eventually(|| m.barred_accounts().0.contains(&"aa-first".to_string())).await,
        "an expired login must leave the placement pool: {:?}",
        m.barred_accounts()
    );

    // AND IT IS NOT FLAGGED DEAD. This assertion is the guard on the decision
    // above: setting both would move the turn away and then leave `dead_since`
    // with nothing able to clear it.
    let (_, unhealthy) = m.barred_accounts();
    assert!(
        unhealthy.is_empty(),
        "login expiry must not raise the dead flag as well — it would latch: {unhealthy:?}"
    );
    assert_eq!(
        m.distinct_healthy_logins(),
        2,
        "a barred login is still a login that exists; the honest count does not drop \
         (2, not 3 — the third was the machine default, which the daemon no longer holds)"
    );

    // VISIBLE, WITH THE REASON AND THE AXIS — this is what `accounts status`
    // renders as "run `claude /login`" rather than as a quota window.
    let rows = threads::accounts::collect_activity(
        &m.store,
        &m.pool(),
        &std::collections::BTreeMap::new(),
        None,
        100,
    )
    .unwrap();
    let barred = rows.iter().find(|r| r.account == "aa-first").unwrap();
    assert!(barred.registered, "a barred account is still registered");
    assert!(
        barred.healthy,
        "the dead flag is not this path's lever any more"
    );
    assert_eq!(
        barred.limit_axis.as_deref(),
        Some("login"),
        "the bar must name WHY, or the status surface tells him to wait instead of to act"
    );
    assert!(
        barred
            .last_limit_prose
            .as_deref()
            .unwrap_or_default()
            .contains("OAuth session expired"),
        "the verbatim cause is kept for diagnosis (and NOT read aloud to him): {:?}",
        barred.last_limit_prose
    );
}

/// Two registry labels pointing at ONE login count as one account.
///
/// The live case is `capture-secondary-live` and `default`: same `accountUuid`,
/// same `emailAddress`, one subscription, one set of usage limits. Every selector
/// treated them as two accounts, so a "failover" between them re-hit the very
/// limit it was running from, at the cost of the thread's conversation — and any
/// "we have this account to ourselves" or "two accounts is two hundred percent"
/// statement over them was arithmetic on a fiction.
#[tokio::test]
async fn two_labels_for_one_login_count_as_one_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();

    // Two snapshot dirs holding the SAME `oauthAccount` identity — which is
    // exactly what cloning a keychain credential into a second config dir
    // produces — and one holding a different login.
    let write_identity = |name: &str, acct: &str, email: &str| {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".claude.json"),
            json!({"oauthAccount": {
                "accountUuid": acct,
                "emailAddress": email,
                "organizationUuid": "org-1",
            }})
            .to_string(),
        )
        .unwrap();
    };
    write_identity("clone-a", "acct-1", "me@example.com");
    write_identity("clone-b", "acct-1", "me@example.com");
    write_identity("separate", "acct-2", "other@example.com");

    let mut accounts = std::collections::BTreeMap::new();
    for name in ["clone-a", "clone-b", "separate"] {
        accounts.insert(
            name.to_string(),
            tmp.path().join(name).to_string_lossy().to_string(),
        );
    }
    let cfg = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    };
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(cfg).unwrap();

    // THREE pool names but TWO logins: the two clones collapse into one. This
    // used to read four-and-three, the extra of each being the machine default
    // that `ensure_default` registered on every boot. The daemon does not
    // create that row any more, so the counts are of accounts it was actually
    // given.
    assert_eq!(m.account_pool().len(), 3);
    assert_eq!(m.distinct_healthy_logins(), 2);
    // CON-27, and this line is where the live bug actually lived. The machine
    // default is dropped BEFORE the collapse, so it is absent here even though
    // it is a healthy login and sorts ahead of `separate`. On the operator's
    // own machine `default` and `owner-account` carry the SAME
    // identity, so under the old order they collapsed into one unit and
    // `default` — sorting first — became the canonical name of an account
    // threads owns. Placement then named the machine login every time. Dropping
    // first means a canonical label is always a label threads owns.
    assert_eq!(
        m.selectable_pool(),
        vec!["clone-a", "separate"],
        "one label per login, alphabetically first as the canonical one — and \
         never the machine default, which leaves before the collapse"
    );

    // Failover never hops between two names for one login. `clone-a` hits a
    // usage limit; the limit belongs to the LOGIN, so `clone-b` is barred with
    // it and placement is left with `separate` alone — the machine default was
    // never a destination it could have offered (CON-27). The bar itself is
    // computed over the VISIBLE pool, because a limit is a fact about a login
    // whether or not threads may place on it.
    let states: std::collections::BTreeMap<_, _> = m
        .store
        .list_account_states()
        .unwrap()
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect();
    let mut limited = states.clone();
    limited.get_mut("clone-a").unwrap().limit_until =
        Some((chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339());
    assert_eq!(
        threads::accounts::unavailable_accounts(
            &m.account_pool(),
            &limited,
            &threads::store::now_rfc3339()
        ),
        vec!["clone-a".to_string(), "clone-b".to_string()],
        "one login's limit window bars every label for that login"
    );

    // And the status surface says which label is the alias of which, so nobody
    // has to re-derive it.
    let rows = threads::accounts::collect_activity(
        &m.store,
        &m.pool(),
        &std::collections::BTreeMap::new(),
        None,
        100,
    )
    .unwrap();
    let unit = |n: &str| {
        rows.iter()
            .find(|r| r.account == n)
            .unwrap()
            .selection_unit
            .clone()
    };
    assert_eq!(unit("clone-a").as_deref(), Some("clone-a"));
    assert_eq!(unit("clone-b").as_deref(), Some("clone-a"));
    assert_eq!(unit("separate"), None, "the only label for its login");
}

/// **A WIPED SLOT STAYS OUT OF PLACEMENT AFTER ITS TIMED LOGIN BAR ENDS.**
///
/// The live thrash (2026-10-14, `alice-example-com`): the slot's keychain store
/// held no refresh token (`SlotHealth::Wiped`), a turn on it failed "OAuth
/// session expired and could not be refreshed", the runtime wrote the TIMED
/// login bar, the bar expired, `notice_expired_limits` re-ran placement, and the
/// same threads were placed back on the same slot to fail the same way — every
/// ten minutes, forever. "wiped" was display-only.
///
/// A wiped slot is a positive fact no timed recheck can change, so it must stay
/// barred for as long as it is wiped — and come back the moment its
/// classification changes. The probe is a stand-in: no keychain is touched.
///
/// FORCED RED: dropping the `sign_in_needed` filter from `barred_accounts` and
/// `selectable_pool` fails at the "still barred after the bar ended" assertion.
#[tokio::test]
async fn a_wiped_slot_stays_out_of_placement_after_its_login_bar_ends() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-first", "zz-second"])).unwrap();
    let wiped = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let probe = |wiped: Arc<std::sync::atomic::AtomicBool>| -> threads::runtime::SlotProbe {
        Arc::new(move |label: &str, _dir: Option<&str>| {
            if label == "aa-first" && wiped.load(std::sync::atomic::Ordering::SeqCst) {
                threads::accounts::SlotHealth::Wiped
            } else {
                threads::accounts::SlotHealth::Live
            }
        })
    };
    m.set_slot_probe_for_test(probe(wiped.clone()));

    let mut st = settings(tmp.path());
    st.retry_policy.max_login_attempts = 1;
    let id = m.create_thread(None, st).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "aa-first");
    m.send(
        &id,
        "authfail please".to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();
    assert!(
        eventually(|| m.barred_accounts().0.contains(&"aa-first".to_string())).await,
        "the auth failure bars the login: {:?}",
        m.barred_accounts()
    );

    // THE TIMED BAR ENDS — exactly what its expiry does to placement's inputs.
    m.store.set_account_limit("aa-first", None).unwrap();
    m.notice_expired_limits();

    assert!(
        m.barred_accounts().0.contains(&"aa-first".to_string()),
        "a wiped slot must stay unavailable after its timed login bar ends, or placement \
         re-runs straight back onto it: {:?}",
        m.barred_accounts()
    );
    assert!(
        !m.selectable_pool().iter().any(|p| p == &"aa-first".to_string()),
        "a wiped slot is not selectable: {:?}",
        m.selectable_pool()
    );

    // THE OPERATOR SIGNS THE SLOT IN: classification changes, the slot returns.
    wiped.store(false, std::sync::atomic::Ordering::SeqCst);
    m.set_slot_probe_for_test(probe(wiped.clone()));
    assert!(
        eventually(|| {
            m.notice_expired_limits();
            !m.barred_accounts().0.contains(&"aa-first".to_string())
        })
        .await,
        "a slot that is no longer wiped is placeable again: {:?}",
        m.barred_accounts()
    );
    assert!(m.selectable_pool().iter().any(|p| p == &"aa-first".to_string()));
}

/// **THE RE-RUN PATH: a bar ending must not launch a turn on a wiped slot.**
///
/// The exact live loop (2026-10-14): when `alice-example-com`'s timed login bar
/// ended, `LIMIT WINDOW ENDED — placement re-runs now` fired, the threads parked
/// on it were re-asked, nowhere else could take them, and the ungated escape
/// hatch RAN them where they sat — on the wiped slot — so each failed "OAuth
/// session expired" and wrote the login bar again. This drives that path: the
/// only other account is limit-barred, so failover has nowhere to go, and the
/// observable is whether a fresh login bar lands on the slot after its first
/// one ended (it can only land if a turn was launched there). Before the fix the
/// deadline pass admits the thread straight back onto the slot.
///
/// FORCED RED, both observed: turning the wiped reading off
/// (`note_slot_after_auth_failure` records nothing) fails at the "no turn was
/// launched" assertion; so does dropping only the escape-hatch guard in
/// `admit_or_park`, with the barred-accounts filter left in place.
#[tokio::test]
async fn a_bar_ending_does_not_relaunch_turns_on_a_wiped_slot() {
    bar_ending_does_not_relaunch(true, false).await;
}

/// The same loop on a slot that is wiped AND disabled — the second live
/// sighting (cc1 disabled at 18:17Z, a turn launched on it anyway at 18:24Z).
/// A disable alone lets work already there finish (`account_disable.rs`); a
/// wiped login has no work that can finish, so the wiped bar must still hold.
#[tokio::test]
async fn a_bar_ending_does_not_relaunch_turns_on_a_wiped_disabled_slot() {
    bar_ending_does_not_relaunch(true, true).await;
}

async fn bar_ending_does_not_relaunch(wiped: bool, disable: bool) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-first", "zz-second"])).unwrap();
    m.set_slot_probe_for_test(Arc::new(move |label: &str, _dir: Option<&str>| {
        if wiped && label == "aa-first" {
            threads::accounts::SlotHealth::Wiped
        } else {
            threads::accounts::SlotHealth::Live
        }
    }));
    // Nowhere to fail over to: the only other login is inside a usage window.
    m.store
        .set_account_limit(
            "zz-second",
            Some(("2099-01-01T00:00:00+00:00", Some("limit"), Some("session"), None)),
        )
        .unwrap();

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "aa-first");
    m.send(
        &id,
        "authfail please".to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();
    let login_bar = |m: &ThreadManager| {
        m.store
            .list_account_states()
            .unwrap()
            .into_iter()
            .find(|s| s.name == "aa-first")
            .and_then(|s| s.limit_until)
    };
    assert!(
        eventually(|| login_bar(&m).is_some()).await,
        "the first auth failure writes the login bar"
    );
    // Let the first failure's own retry ladder play out before the bar ends.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    if disable {
        m.store.set_account_disabled("aa-first", true, None).unwrap();
    }

    // THE BAR ENDS — its window passes, exactly as a real expiry leaves the row
    // (a `limit_until` in the past, not a cleared one) — and the sweep that
    // re-runs placement on that edge fires, plus the pool's DEADLINE pass, which
    // is what re-asks a retrying thread when the cadence comes due (the
    // ten-minute re-run in the live log).
    let ended = "2000-01-01T00:00:00+00:00";
    m.store
        .set_account_limit("aa-first", Some((ended, Some("login"), Some("login"), None)))
        .unwrap();
    for _ in 0..20 {
        m.notice_expired_limits();
        m.placement_pass(threads::runtime::PassOrigin::Cadence);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        login_bar(&m).as_deref() == Some(ended),
        "no turn may be launched on this slot after its bar ends — a fresh login bar \
         means one was, and failed on the credential again (the thrash): {:?}",
        login_bar(&m)
    );
    assert_eq!(m.serving_account(&id).unwrap(), "aa-first");
}

/// **A SLOT THAT IS ALREADY WIPED WHEN THE DAEMON BOOTS IS OUT OF PLACEMENT
/// BEFORE THE FIRST TURN, NOT AFTER IT FAILS ON IT** (H179, 2026-10-16).
///
/// Live: threads restarted onto 8bf1a4b8 at 20:40:31Z; `sign_in_needed` is
/// memory only, so it booted empty and the very first placement (the parent
/// 60f0380d's agent-report wake) moved the conversation to `bob-example-com`
/// — wiped since before the restart — where the turn failed "OAuth session
/// expired and could not be refreshed" and was only then retried on cc1. Every
/// restart paid one failed turn (and a conversation port) per wiped slot.
///
/// FORCED RED: delete the `seed_sign_in_needed()` call in
/// `ThreadManager::recover` → the "barred at boot" assertion fails.
#[tokio::test]
async fn a_slot_wiped_before_boot_is_barred_from_the_first_placement() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-first", "zz-second"])).unwrap();
    m.set_slot_probe_for_test(Arc::new(|label: &str, _dir: Option<&str>| {
        if label == "aa-first" {
            threads::accounts::SlotHealth::Wiped
        } else {
            threads::accounts::SlotHealth::Live
        }
    }));
    assert!(m.barred_accounts().0.is_empty(), "nothing has been read yet: {:?}", m.barred_accounts());
    m.recover().await;
    assert!(
        m.barred_accounts().0.contains(&"aa-first".to_string()),
        "a slot wiped before boot is barred at boot, before any turn fails on it: {:?}",
        m.barred_accounts()
    );
    assert!(
        !m.barred_accounts().0.contains(&"zz-second".to_string()),
        "a live slot is not barred: {:?}",
        m.barred_accounts()
    );
    assert!(
        !m.selectable_pool().iter().any(|p| p == "aa-first"),
        "the wiped slot is not selectable: {:?}",
        m.selectable_pool()
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
