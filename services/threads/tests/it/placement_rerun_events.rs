//! **PLACEMENT RE-RUNS WHEN THE WORLD CHANGES — ON EVENTS, NOT ON A CLOCK.**
//!
//! The operator, on what `parked` means and on what was wrong with the way it
//! was served:
//!
//! > *"note that the parked status means that there is a pending turn and it has
//! > not been placed. There should not be a per-thread retry timestamp. And there
//! > are certain events which tell placement to rerun the full placement
//! > algorithm - e.g. new_account, limit_expired, and others. We just need to
//! > make sure that those events are complete."*
//!
//! `parked` is a PLACEMENT STATE, not a timer state. The bell
//! (`account_picture_changed[_for]`) existed and fourteen causes rang it — and
//! every one of them woke `retry_sweep`, the sweep for the per-thread retry
//! floors, while `resume_sweep`, the sweep for the state he is actually
//! describing, **was on no event at all**: a five-second timer and nothing else.
//! The event-driven path served the mechanism he wants removed and the
//! placement-shaped state was polled.
//!
//! This file pins the repair, which is three things:
//!
//! 1. **ONE BELL, BOTH SWEEPS.** `placement_pass` runs `resume_sweep` and
//!    `retry_sweep` off a single `Notify`, so one coalescing window covers both.
//!    They stay two sweeps because they serve two states (`parked_turns` and
//!    waiting queues through `maybe_dispatch`; `retries` through `fire_retry`).
//! 2. **THE EVENT SET COMPLETED.** `limit_expired` and `new_account` did not
//!    exist; a `/usage` reading that crossed a thread's thresholds rang nothing;
//!    a budget edit reached only the parked half.
//! 3. **THE TIMER DEMOTED AND INSTRUMENTED.** It stays as a backstop, because an
//!    event set can always turn out to be incomplete and a thread silently
//!    parked forever is worse than a cheap poll — but a backstop that quietly
//!    carries the work is exactly how the old wiring survived, so a backstop pass
//!    that places anything is counted and logged as a DEFECT.
//!
//! # How the proofs are opposed
//!
//! Every ring has a test that reddens if the ring is REMOVED and a sibling that
//! reddens if the ring is made UNCONDITIONAL, so neither a missing event nor a
//! degenerate always-wake passes this file. The pairs:
//!
//! | ring removed reddens | always-ring reddens |
//! |---|---|
//! | `an_expired_limit_window_rings_limit_expired` | `an_unexpired_window_rings_nothing`, `a_window_that_already_rang_does_not_ring_again` |
//! | `a_new_account_in_the_pool_rings_new_account` | `a_roster_that_did_not_move_rings_nothing` |
//! | `a_recorded_usage_reading_wakes_placement` | `a_refused_probe_wakes_nothing` |
//! | `the_budget_handlers_wake_placement` | `the_refused_budget_delete_wakes_nothing` |
//! | `a_parked_thread_is_placed_by_an_event_with_the_backstop_disabled` | `with_no_event_and_no_backstop_a_parked_thread_stays_parked` |
//!
//! Nothing here spawns a real claude turn: every manager gets an explicit
//! `data_dir` under a tempdir, `THREADS_CLAUDE_BIN` points at the fake fixture
//! and `THREADS_CLAUDE_HOME` points transcript storage at scratch, so the
//! operator's `~/.claude`, the live daemon's database and the keychain are all
//! out of reach.

#[path = "support/keychain.rs"]
mod keychain;
#[path = "support/fake_login.rs"]
mod fake_login;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::accounts::AccountState;
use threads::event::Push;
use threads::settings::ThreadSettings;
use threads::store::ParkedTurnRow;
use threads::runtime::PassOrigin;
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

/// **THE BACKSTOP IS OFF FOR THIS WHOLE BINARY, AND THAT IS THE POINT.**
///
/// `THREADS_RESUME_TICK_MS=0` removes the fallback timer leg entirely, so a
/// parked thread that gets placed in this file was placed BY AN EVENT. With the
/// five-second poll still running the headline proof would prove nothing — the
/// poll would have done it within five seconds either way.
///
/// It is set per process rather than per test because `std::env` is
/// process-global and these tests run concurrently inside one binary; a test
/// that flipped it would flip it for its neighbours. The roster period is
/// shortened for the same reason `THREADS_HEARTBEAT_MS` is: thirty seconds is the
/// right production cadence for human-latency news and a wrong test duration.
fn set_env() {
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "200");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("THREADS_RESUME_TICK_MS", "0");
    std::env::set_var("THREADS_ROSTER_RELOAD_MS", "60");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// One account this crate OWNS — its own config dir under the tempdir, so no
/// spawn can resolve to `~/.claude`.
fn config_at(dir: &Path, names: &[&str]) -> Config {
    let mut accounts = std::collections::BTreeMap::new();
    for n in names {
        std::fs::create_dir_all(dir.join(n)).unwrap();
        accounts.insert(n.to_string(), dir.join(n).to_string_lossy().to_string());
    }
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        // BOTH gauge cadences off — the configuration under which the roster
        // re-read used to be dead code, because its task was never spawned.
        probe_interval_secs: 0,
        probe_interval_busy_secs: 0,
        ..Default::default()
    }
}

fn plain(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

fn far_future() -> String {
    "2099-01-01T00:00:00+00:00".to_string()
}

fn long_past() -> String {
    "2000-01-01T00:00:00+00:00".to_string()
}

/// Write `name`'s limit window (or clear it).
fn limit(m: &ThreadManager, name: &str, until: Option<&str>) {
    m.store
        .upsert_account_state(&AccountState {
            name: name.into(),
            config_dir: None,
            limit_until: until.map(str::to_string),
            ..Default::default()
        })
        .unwrap();
}

fn park(m: &ThreadManager, id: &str) {
    m.store
        .upsert_parked_turn(&ParkedTurnRow {
            thread_id: id.to_string(),
            prompt: "the operator's pending turn".into(),
            message_ids: vec![1],
            attempt: 1,
            reason: "test: barred".into(),
            budget_key: None,
            parked_at: threads::store::now_rfc3339(),
        })
        .unwrap();
}

/// Every `Push::Info` the manager broadcast, in order, as `(domain, reason)`.
fn watch_info(m: &ThreadManager) -> Arc<Mutex<Vec<(String, String)>>> {
    let mut rx = m.subscribe();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Info { domain, reason, .. } = p {
                out.lock().unwrap().push((domain, reason));
            }
        }
    });
    seen
}

fn reasons(seen: &Arc<Mutex<Vec<(String, String)>>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|(_, r)| r.clone())
        .collect()
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

/// The comment-free body of one `src/` file, so a source guard reads CODE and
/// cannot be satisfied — or falsified — by the prose that explains it. The
/// self-witness class: a ban on a string is satisfied by its own explanatory
/// comment unless the comments come out first.
fn code_of(src: &str) -> String {
    src.lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("//")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The code between two anchors. `from` is matched at its LAST occurrence, so a
/// method name that also appears in `server.rs`'s `METHODS` list picks the match
/// arm rather than the catalogue entry — the catalogue is a list of strings and
/// the arm is the code.
fn slice<'a>(body: &'a str, from: &str, to: &str) -> &'a str {
    let start = body
        .rfind(from)
        .unwrap_or_else(|| panic!("anchor moved, re-point this test: {from}"));
    let rest = &body[start..];
    let end = rest
        .find(to)
        .unwrap_or_else(|| panic!("anchor moved, re-point this test: {to}"));
    &rest[..end]
}

/// **A THREAD THE GATE CAN REFUSE, AND WHY IT IS BUILT THIS WAY.**
///
/// A bar alone cannot park an ordinary AUI thread, and that is deliberate crate
/// behaviour rather than an obstacle to route around: `admit_or_park` computes
/// `ungated` and *"a thread with nothing to enforce must always run"* — which is
/// what makes a WRONG `limit_until` falsifiable, since an ungated thread runs
/// anyway, completes, and `note_account_served` retires the window.
///
/// So the blocker here is the one that parks threads in the real deployment: a
/// weekly reading above the thread's own `Thresholds` ceiling
/// (`DEFAULT_WEEKLY_THRESHOLD_PCT`, 95). Recording a reading also makes
/// `thresholds_can_refuse` true, which is what moves the thread out of the
/// ungated branch at all.
///
/// It is also exactly gap 4's subject, which is the point: the thing that parks
/// most threads is a threshold crossing, and a threshold crossing used to move
/// placement silently.
fn observe_weekly(m: &ThreadManager, name: &str, pct: f64) {
    m.budgets
        .observe(name, threads::gauge::Axis::WeeklyAll, pct, None, "test")
        .unwrap();
}

// ═════════════════════════════════════════════════════════════════════════
// 1. THE READING, VERIFIED — which reasons exist, and what each one wakes
// ═════════════════════════════════════════════════════════════════════════

/// Every `reason` literal handed to one of the two bells, read out of `src/`.
///
/// Comment lines are stripped first: `runtime.rs` discusses
/// `account_picture_changed_for` at length in prose, and a scanner that counted
/// the prose would report a reason that no code ever rings.
fn rung_reasons(call: &str) -> Vec<String> {
    let mut out = Vec::new();
    for src in [
        include_str!("../../src/runtime.rs"),
        include_str!("../../src/server.rs"),
        include_str!("../../src/accounts.rs"),
        include_str!("../../src/budgets.rs"),
        include_str!("../../src/gauge.rs"),
        include_str!("../../src/login.rs"),
        include_str!("../../src/main.rs"),
        include_str!("../../src/update.rs"),
        include_str!("../../src/store.rs"),
    ] {
        let code = code_of(src);
        let mut rest = code.as_str();
        while let Some(i) = rest.find(call) {
            rest = &rest[i + call.len()..];
            if let Some(end) = rest.find('"') {
                let lit = &rest[..end];
                // A literal only — `reason` forwarded as a variable is a
                // re-ring of somebody else's tag, not a new cause.
                if !lit.is_empty() && lit.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    out.push(lit.to_string());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// **THE EVENT SET, NAMED.** The fourteen that were already there, plus the two
/// non-events that now have a cause, on the ANNOUNCING bell.
///
/// `rename` is absent on purpose and must stay absent: it reloads the pool and
/// announces `renamed` without ringing, because the same logins are selectable
/// under one new name. Its own call site argues this; an implementation that
/// "completed" the set by adding it would be making placement re-run for a
/// cosmetic change.
///
/// FORCED RED, run and recorded: routing `notice_expired_limits` through
/// `placement_wake` instead of the announcing bell drops `limit_expired` from the
/// left-hand side and prints `the announcing bell's reason set moved … | left:
/// [… "limit_cleared", "limit_falsified_by_gauge" …] | right: [… "limit_cleared",
/// "limit_expired", "limit_falsified_by_gauge" …]`. Adding a ring for `renamed`
/// reddens it from the other side.
#[test]
fn the_announcing_bell_rings_for_these_reasons_and_no_others() {
    let got = rung_reasons("account_picture_changed_for(\"");
    let mut plain_bell = rung_reasons("account_picture_changed(\"");
    let mut all = got;
    all.append(&mut plain_bell);
    all.sort();
    all.dedup();

    let expected: Vec<String> = [
        // ── the fourteen that already existed ───────────────────────────
        "account_disabled",
        "account_enabled",
        "credit_exhausted",
        "deleted",
        "limit_cleared",
        "limit_falsified_by_gauge",
        "limit_recorded",
        "login_dead",
        "login_expired",
        "login_restored",
        "login_revived",
        "pool_reloaded",
        "pool_share",
        "rotated",
        // ── the two non-events that now have a cause ────────────────────
        // A limit window reaching its end: gap 2, and gap 6 with it, because a
        // credit bar is a `limit_until` window like any other.
        "limit_expired",
        // An account joining the roster, noticed unconditionally.
        "new_account",
        // A WIPED slot that is no longer wiped (signed in, or a turn completed
        // on it). It was barred for as long as it was wiped, outlasting its
        // timed login bar, so its return is an input to placement (H179).
        "slot_signed_in",
        // R12.10: a re-check on his enable or a verified login that LIFTED a
        // flag (wiped / expired / dead / login bar) — the account is placeable
        // again now, so placement re-runs. A re-check that keeps everything
        // does not ring.
        "flags_cleared",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut expected = expected;
    expected.sort();

    assert_eq!(
        all, expected,
        "the announcing bell's reason set moved. Every cause that moves an input \
         to placement must ring, and a cosmetic change must not — if this is a \
         deliberate addition, add it to this list and say why in the call site."
    );
}

/// **THE WAKE-ONLY BELL**, for the two causes that move placement and have
/// already announced (or have nothing to announce on).
///
/// FORCED RED, run and recorded: removing `placement_wake("usage_observed")` from
/// `probe_account` prints
/// `the wake-only bell's reason set moved | left: ["budget_disabled", "budget_enabled", "budget_set"] | right: ["budget_disabled", "budget_enabled", "budget_set", "usage_observed"]`.
#[test]
fn the_wake_only_bell_rings_for_these_reasons_and_no_others() {
    let got = rung_reasons("placement_wake(\"");
    assert_eq!(
        got,
        vec![
            // A budget edit has no shape to announce on (`Visible::Never`) and
            // very much moves placement.
            "budget_disabled".to_string(),
            "budget_enabled".to_string(),
            "budget_set".to_string(),
            // A turn ENDING (H179): the operator lists it among the placement
            // events, and it is already on the status stream, so it only wakes.
            "turn_completed".to_string(),
            // A `/usage` reading already announced itself on the `gauge`
            // domain, per reading, with the number in it.
            "usage_observed".to_string(),
        ],
        "the wake-only bell's reason set moved"
    );
}

/// **`resume_sweep` IS ON THE BELL, AND IT WAS ON NONE OF THEM.**
///
/// The headline defect, pinned structurally as well as behaviourally, because the
/// behavioural proof could pass on a daemon whose timer someone put back.
///
/// Three clauses:
/// * `placement_pass` runs BOTH sweeps, so one bell serves both states.
/// * The one task that waits on `account_picture` calls `placement_pass`, not
///   `retry_sweep` alone — which is the exact shape of the old wiring.
/// * No task calls `resume_sweep` off a bare `sleep` any more. That was the whole
///   mechanism: `loop { sleep(tick); resume_sweep() }`.
///
/// FORCED RED, run and recorded: restoring the old
/// `loop { tokio::time::sleep(tick).await; mgr.resume_sweep(); }` task gives
/// `no task may poll resume_sweep off a bare sleep — placement waits on the bell,
/// and the backstop goes through placement_pass so its firing is counted.
/// Offending line:                 poll_mgr.resume_sweep();`.
#[test]
fn the_bell_wakes_the_placement_sweep_and_the_retry_sweep() {
    let code = code_of(include_str!("../../src/runtime.rs"));

    let pass = slice(&code, "pub fn placement_pass(", "async fn fire_retry(");
    assert!(
        pass.contains("self.resume_sweep_placing()"),
        "placement_pass must run the PARKED sweep — the state the operator \
         defines as a pending turn that has not been placed"
    );
    assert!(
        pass.contains("self.retry_sweep(origin)"),
        "placement_pass must run the retry sweep too: one bell, both states — and it \
         must hand it the pass's ORIGIN, which is what decides whether an ask is a \
         question or the pool's own deadline"
    );

    // Code anchors on both ends: `code_of` has stripped every comment, so a
    // banner line cannot be a landmark. That is the point of stripping them — a
    // guard anchored on prose is a guard an edit to the prose can switch off.
    let watcher = slice(
        &code,
        "let mut due: Option<tokio::time::Instant>",
        "let every = if tick.is_zero()",
    );
    assert!(
        watcher.contains("mgr.account_picture.notified()") && watcher.contains("placement_pass("),
        "the task that waits on the bell must run a full placement pass, not one \
         sweep of the two"
    );

    // The negative clause. `resume_sweep` may be called from a handler, from
    // `recover`, from a test — but never again from a loop whose only trigger is
    // a clock.
    for (i, line) in code.lines().enumerate() {
        if line.contains("resume_sweep()") {
            let window = code
                .lines()
                .skip(i.saturating_sub(3))
                .take(4)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !window.contains("tokio::time::sleep"),
                "no task may poll resume_sweep off a bare sleep — placement waits \
                 on the bell, and the backstop goes through placement_pass so its \
                 firing is counted. Offending line: {line}"
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 2. THE HEADLINE PROOF — an event places a parked thread, timer disabled
// ═════════════════════════════════════════════════════════════════════════

/// **A PARKED THREAD IS PLACED BY AN EVENT, WITH THE BACKSTOP SWITCHED OFF.**
///
/// `THREADS_RESUME_TICK_MS=0` (see `set_env`) leaves the watcher waiting on
/// `account_picture` and nothing else. So the whole of the daemon's ability to
/// notice that this thread can now run is the bell — if the ring is removed, the
/// thread waits forever, which is exactly what `resume_sweep` being on no event
/// meant before the backstop covered for it.
///
/// The thread is held by a THRESHOLD rather than by a lock, deliberately: a
/// threshold is an input to PLACEMENT and is what parks threads in the real
/// deployment, so a reading that crosses back under it is precisely the world
/// change the operator is describing. (See `observe_weekly` for why a bar alone
/// would not do: an ungated thread runs anyway, on purpose.)
///
/// FORCED RED, run and recorded: dropping the `account_picture.notify_one()` from
/// `placement_wake` leaves the reading recorded and nothing woken, and this fails
/// with `the parked thread must be placed BY THE EVENT — the backstop is switched
/// off, so nothing else can place it`.
///
/// FORCED RED, the other way, also run: leaving the ring in but replacing
/// `self.resume_sweep_placing()` in `placement_pass` with an empty list fails the
/// same assertion with the same text — which is the ORIGINAL DEFECT reproduced
/// exactly, since `retry_sweep` alone has never looked at `parked_turns`.
#[tokio::test]
async fn a_parked_thread_is_placed_by_an_event_with_the_backstop_disabled() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    m.start_watchers();

    let id = m.create_thread(None, plain(tmp.path())).unwrap();
    // OVER THE CEILING, before the park, so the first pass has something to
    // refuse: 99% weekly against a 95% threshold.
    observe_weekly(&m, "aa-owned", 99.0);
    park(&m, &id);

    // One pass while the account is over the ceiling: the gate refuses and the
    // row stays. This is the control — it says the park is real, so the placement
    // later is not an artefact of a row nothing was ever blocking.
    assert_eq!(
        m.placement_pass(PassOrigin::Event),
        Vec::<String>::new(),
        "an account over the thread's threshold must place nothing"
    );
    assert!(
        m.store.get_parked_turn(&id).unwrap().is_some(),
        "and a refusal never consumes the operator's prompt"
    );

    // ── THE WORLD CHANGES, AND ONLY THE EVENT CARRIES IT ─────────────────
    // 99% → 10%: the crossing that unparks every thread carrying this ceiling,
    // and the one that used to ring nothing at all.
    observe_weekly(&m, "aa-owned", 10.0);
    m.placement_wake("usage_observed");

    assert!(
        eventually(|| m.store.get_parked_turn(&id).unwrap().is_none()).await,
        "the parked thread must be placed BY THE EVENT — the backstop is \
         switched off, so nothing else can place it"
    );

    let p = m.placement_passes();
    assert_eq!(
        p.by_backstop, 0,
        "no backstop pass may have run at all: the timer is off, and a count \
         above zero means something put a clock back"
    );
    assert!(
        p.by_event >= 1,
        "the pass that placed it must be attributed to an event, got {p:?}"
    );
    assert_eq!(
        p.backstop_placed, 0,
        "and nothing may be reported as a missing event"
    );
    assert!(
        m.last_placed_threads().contains(&id) || p.placed >= 1,
        "and the pass must be able to say which thread it placed"
    );
}

/// **THE OPPOSING PROOF: no event, no backstop, no placement.**
///
/// Byte for byte the same situation as the test above, with the bell NOT rung.
/// It is what makes that test mean something: without this, an implementation
/// that placed parked threads on any schedule at all — a hidden timer, a sweep
/// inside another watcher — would pass the headline and this file would be
/// proving nothing.
///
/// It is also the shape of the original defect stated as a property. Before the
/// repair this test would have PASSED for the wrong reason (the bell woke only
/// `retry_sweep`, which never looks at `parked_turns`) and the headline above
/// would have failed. The pair can only both be green if the bell reaches the
/// parked sweep and only the bell does.
///
/// FORCED RED, run and recorded: adding `mgr.placement_pass(PassOrigin::Backstop)` to the
/// noticer task's loop — a plausible "while we are awake anyway" optimisation —
/// places the thread and gives `the account is back under the ceiling, but
/// NOTHING RANG: a parked thread must not be placed by a clock`. That forcing was
/// GREEN at first and the wait below is why; see its comment.
#[tokio::test]
async fn with_no_event_and_no_backstop_a_parked_thread_stays_parked() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    m.start_watchers();

    let id = m.create_thread(None, plain(tmp.path())).unwrap();
    observe_weekly(&m, "aa-owned", 99.0);
    park(&m, &id);
    assert_eq!(m.placement_pass(PassOrigin::Event), Vec::<String>::new());

    // The world changes — and nobody says so.
    observe_weekly(&m, "aa-owned", 10.0);

    // **LONGER THAN EVERY CLOCK IN THE PROCESS, AND THE NUMBER IS LOAD-BEARING.**
    // 600ms was the first draft and it was too short to catch the forcing this
    // test exists for: adding `placement_pass(PassOrigin::Backstop)` to the NOTICER's loop left
    // it green, because with the backstop off the noticer's own floor is one
    // second and the assertion fired before the noticer had ticked once. A
    // negative test is only as strong as the longest period it outlasts, so this
    // waits past the noticer floor (1s) and past `THREADS_ROSTER_RELOAD_MS`
    // (60ms) with margin.
    tokio::time::sleep(Duration::from_millis(1_600)).await;
    assert!(
        m.store.get_parked_turn(&id).unwrap().is_some(),
        "the account is back under the ceiling, but NOTHING RANG: a parked thread \
         must not be placed by a clock"
    );
    assert_eq!(
        m.placement_passes().by_backstop,
        0,
        "and no backstop pass may exist to have done it"
    );

    // And the moment anything does ring, it goes. Proves the thread was placeable
    // throughout, so the 600ms of nothing above was the bell's absence and not an
    // unrelated refusal.
    m.placement_wake("usage_observed");
    assert!(
        eventually(|| m.store.get_parked_turn(&id).unwrap().is_none()).await,
        "the same thread, same state, one ring: placed"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 3. GAP 2 AND GAP 6 — a limit window reaching its end
// ═════════════════════════════════════════════════════════════════════════

/// **A WINDOW THAT HAS ENDED IS AN EVENT.** Gap 2, and gap 6 with it.
///
/// Nothing happens at the instant a bar ends, so somebody has to notice. The old
/// code knew — *"the one input that has no event to ring, a limit window quietly
/// expiring"* — and answered by polling the SWEEP. This answers by polling an
/// EVENT SOURCE: look, and if a window ended since the last look, ring
/// `limit_expired` like any other cause. The sweep stays event-driven, so the
/// backstop's firing still means something.
///
/// A credit bar needs no second mechanism: `note_account_credit_exhausted`
/// writes `now + CREDIT_BAR_SECS` into the same `limit_until` column — *"honestly
/// a RE-PROBE INTERVAL rather than a prediction"* — so "the credit bar ended, let
/// one turn through to find out" is this same edge.
///
/// FORCED RED, run and recorded: deleting the
/// `account_picture_changed_for("limit_expired", ..)` line gives
/// `an ended window must ring limit_expired: []`.
#[tokio::test]
async fn an_expired_limit_window_rings_limit_expired() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let info = watch_info(&m);

    // The baseline look, while the bar still holds: the noticer learns that this
    // label is barred, which is what lets the END be an edge rather than a
    // standing condition.
    limit(&m, "aa-owned", Some(&far_future()));
    assert_eq!(
        m.notice_expired_limits(),
        Vec::<String>::new(),
        "a live window has not ended"
    );

    // The clock passes it. (Written as a past instant rather than slept through,
    // for the reason `effective_limit_until` takes `now` as a parameter: a test
    // has to be able to name the instant it is asking about.)
    limit(&m, "aa-owned", Some(&long_past()));

    assert_eq!(
        m.notice_expired_limits(),
        vec!["aa-owned".to_string()],
        "the window ended, so the noticer must name the login it ended for"
    );
    assert!(
        eventually(|| reasons(&info).contains(&"limit_expired".to_string())).await,
        "an ended window must ring limit_expired: {:?}",
        reasons(&info)
    );
}

/// **A WINDOW THAT HAS NOT ENDED RINGS NOTHING** — the always-ring opposition.
///
/// `limit_until` is a watermark nothing rewrites, so "is it in the past" is a
/// CONDITION that stays true forever once true. A noticer that rang the condition
/// rather than the edge would ring every tick for the rest of the daemon's life,
/// which is a poll wearing an event's name.
///
/// FORCED RED, run and recorded: ringing whatever the look finds rather than the
/// edge against `barred_seen` gives
/// `a window that has not ended must ring nothing | left: ["aa-owned"] | right: []`.
#[tokio::test]
async fn an_unexpired_window_rings_nothing() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let info = watch_info(&m);
    limit(&m, "aa-owned", Some(&far_future()));
    for _ in 0..4 {
        assert_eq!(
            m.notice_expired_limits(),
            Vec::<String>::new(),
            "a window that has not ended must ring nothing"
        );
    }
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        !reasons(&info).contains(&"limit_expired".to_string()),
        "and nothing may reach a client: {:?}",
        reasons(&info)
    );
}

/// **IT RINGS ONCE.** The same edge, asserted as a count rather than a boolean.
///
/// The second half of the always-ring opposition, and the one that actually
/// catches the storm: a noticer on a five-second tick that rang a standing
/// condition would wake placement twelve times a minute, forever, for a window
/// that ended once — and because the backstop would then never be the thing that
/// placed anything, `backstop_placed` would read zero and the defect would look
/// like health.
#[tokio::test]
async fn a_window_that_already_rang_does_not_ring_again() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    limit(&m, "aa-owned", Some(&far_future()));
    m.notice_expired_limits();
    limit(&m, "aa-owned", Some(&long_past()));

    assert_eq!(m.notice_expired_limits(), vec!["aa-owned".to_string()]);
    for _ in 0..5 {
        assert_eq!(
            m.notice_expired_limits(),
            Vec::<String>::new(),
            "an end is an EDGE: the second look at the same ended window is not \
             news, and ringing it again is the poll this replaces"
        );
    }
}

/// **A BAR CLEARED BY A SERVED TURN IS NOT ALSO AN EXPIRY.**
///
/// Three things retract a bar and each already had its own event:
/// `note_account_served` CLEARS the column (`limit_cleared`), a reading under the
/// ceiling FALSIFIES it (`limit_falsified_by_gauge`), and re-fingerprinting makes
/// it STALE. Only expiry was missing, so the noticer's test is narrow on purpose
/// — the row must still CARRY a window whose end has passed. A noticer that
/// merely watched barred-ness disappear would double-ring every one of the three
/// under the wrong name.
///
/// FORCED RED, run and recorded: treating a NULL column as an ended window —
/// `was barred && !is barred` instead of `was barred && the window it still
/// carries has ended` — gives
/// `a cleared column is limit_cleared's event, not an expiry | left: ["aa-owned"] | right: []`.
#[tokio::test]
async fn a_cleared_bar_is_not_reported_as_an_expiry() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    limit(&m, "aa-owned", Some(&far_future()));
    m.notice_expired_limits();

    // What `note_account_served` does: the column goes to NULL.
    limit(&m, "aa-owned", None);
    assert_eq!(
        m.notice_expired_limits(),
        Vec::<String>::new(),
        "a cleared column is limit_cleared's event, not an expiry"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 4. GAP 3 — a new account, noticed without a gauge
// ═════════════════════════════════════════════════════════════════════════

/// **AN ACCOUNT ADOPTED IN A TERMINAL IS NOTICED, ON A DAEMON WITH NO GAUGE.**
///
/// The roster re-read lived on the gauge tick, and that task is spawned only
/// `if let Some(tick)` — so with `probe_interval_secs = 0` and
/// `probe_interval_busy_secs = 0`, which `config_at` sets here and which is a
/// supported configuration, `reload_pool` was never called and a new account was
/// invisible until a restart. A placement input whose noticing is conditional on
/// the GAUGE being switched on is one that is sometimes not noticed at all.
///
/// It is also the event the operator named first. `new_account` is distinguished
/// from `pool_reloaded` here rather than inside `reload_pool`, because
/// `accounts.rename` also reloads and deliberately does not ring.
///
/// FORCED RED, run and recorded: taking `notice_roster_change` off the noticer —
/// which is what putting the re-read back on the gauge tick amounts to on this
/// config — leaves the pool at one member and gives `an account adopted
/// underneath the daemon must join the pool with no gauge running: ["aa-owned"]`.
#[tokio::test]
async fn a_new_account_in_the_pool_rings_new_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let info = watch_info(&m);
    m.start_watchers();
    assert_eq!(m.account_pool(), vec!["aa-owned".to_string()]);

    // What `scripts/add-account` + `threads accounts adopt` do from a CHILD
    // process, with the daemon none the wiser.
    std::fs::create_dir_all(tmp.path().join("zz-new")).unwrap();
    let reg = tmp.path().join("accounts.toml");
    let mut toml = std::fs::read_to_string(&reg).unwrap_or_default();
    toml.push_str(&format!(
        "\n[accounts.zz-new]\nconfig_dir = \"{}\"\n",
        tmp.path().join("zz-new").display()
    ));
    std::fs::write(&reg, toml).unwrap();

    assert!(
        eventually(|| m.account_pool().len() == 2).await,
        "an account adopted underneath the daemon must join the pool with no \
         gauge running: {:?}",
        m.account_pool()
    );
    assert!(
        eventually(|| reasons(&info).contains(&"new_account".to_string())).await,
        "and it must ring new_account, named, so the board can say which login \
         arrived: {:?}",
        reasons(&info)
    );
}

/// **A ROSTER THAT DID NOT MOVE RINGS NOTHING** — the always-ring opposition for
/// gap 3, and the rule `reload_pool`'s own docs state: *"a client that re-reads
/// on a no-op is a client we taught to poll again."*
///
/// FORCED RED, run and recorded: ringing unconditionally instead of on
/// `reload_pool()`'s `true` gives
/// `a reload that found the same roster must not ring | left: Some("pool_reloaded") | right: None`.
#[tokio::test]
async fn a_roster_that_did_not_move_rings_nothing() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let info = watch_info(&m);
    for _ in 0..4 {
        assert_eq!(
            m.notice_roster_change(),
            None,
            "a reload that found the same roster must not ring"
        );
    }
    tokio::time::sleep(Duration::from_millis(80)).await;
    let r = reasons(&info);
    assert!(
        !r.contains(&"new_account".to_string()) && !r.contains(&"pool_reloaded".to_string()),
        "and nothing may reach a client: {r:?}"
    );
}

/// **A RENAME IS NOT A PLACEMENT EVENT**, and the exemption the operator asked to
/// be left alone is held by a test rather than by a comment.
///
/// `accounts.rename` reloads the pool — the login directory moved on disk, so
/// `config_dir_for` would otherwise hand placement a path that is gone — and
/// announces `renamed`. It must not ring, because the same logins are selectable
/// under one new name.
///
/// FORCED RED, run and recorded: adding an
/// `account_picture_changed_for("renamed", ..)` to that handler gives
/// `accounts.rename must announce without ringing the bell: nothing about
/// PLACEMENT moved, and waking the sweeps to report a cosmetic change is a side
/// effect on the wrong system`.
#[test]
fn a_rename_announces_without_ringing() {
    let code = code_of(include_str!("../../src/server.rs"));
    let arm = slice(&code, "\"accounts/{account}/rename\"", "\"accounts/{account}/delete\"");
    assert!(
        arm.contains("manager.reload_pool()"),
        "a rename must still re-derive the pool: the login directory moved"
    );
    assert!(
        arm.contains("manager.emit_info("),
        "and it must still announce — the roster label every client is showing \
         no longer exists"
    );
    assert!(
        !arm.contains("account_picture_changed") && !arm.contains("placement_wake"),
        "accounts.rename must announce without ringing the bell: nothing about \
         PLACEMENT moved, and waking the sweeps to report a cosmetic change is a \
         side effect on the wrong system"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 5. GAP 4 — a threshold crossing, which is what parks most threads
// ═════════════════════════════════════════════════════════════════════════

/// **A RECORDED `/usage` READING WAKES PLACEMENT.**
///
/// `probe_account` rang the bell at the two ends of the range only — it wrote a
/// bar (`limit_recorded`) or retracted one (`limit_falsified_by_gauge`) — and said
/// nothing in between. A thread's `Thresholds` are its own number, well below any
/// bar: a reading that takes an account from 96% back to 91% crosses a 95%
/// threshold and unparks every thread carrying it. That whole interior was
/// invisible because the two endpoints were wired and the middle was not.
///
/// Driven through the real `probe_account` against the fake fixture's `/usage`
/// branch, so this proves the reading path and not a mock of it.
///
/// The watcher is deliberately NOT started: the pass is taken by hand so the
/// reason set it consumes is readable. With the watcher running, the fixture's
/// three readings become one wake plus one stored permit, the second pass drains
/// an empty set, and the assertion would be racing the scheduler for no gain —
/// the watcher's half is already proved by the headline test.
///
/// FORCED RED, run and recorded: deleting `self.placement_wake("usage_observed")`
/// leaves nothing pending and gives
/// `a recorded reading must wake placement, and the pass must name it … | left: [] | right: ["usage_observed"]`.
#[tokio::test]
async fn a_recorded_usage_reading_wakes_placement() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    let n = m.probe_account_for_test("aa-owned").await.unwrap();
    assert!(n > 0, "the fixture must have recorded readings, got {n}");

    m.placement_pass(PassOrigin::Event);
    assert_eq!(
        m.last_placement_reasons(),
        vec!["usage_observed".to_string()],
        "a recorded reading must wake placement, and the pass must name it — a \
         threshold crossing is what parks most threads and it used to move \
         placement silently"
    );
    let p = m.placement_passes();
    assert_eq!(
        p.total, 1,
        "three readings on three axes are ONE pass, not three: {p:?}"
    );
    assert_eq!(p.by_backstop, 0, "by the event, not by a clock");
}

/// **A PROBE THAT RECORDED NOTHING WAKES NOTHING** — the always-ring opposition.
///
/// A name with no login directory is REFUSED rather than polled, because an unset
/// `CLAUDE_CONFIG_DIR` would run `/usage` against the operator's own terminal
/// login. No reading, no placement input, no ring. A wake hung on the ATTEMPT
/// rather than on the RECORD would re-run placement on every failed poll of every
/// unpollable name, on the gauge's cadence, forever.
///
/// FORCED RED, run and recorded: hanging the wake on entry to `probe_account`
/// instead of on the `observe` success arm gives
/// `a probe that recorded nothing must wake nothing | left: ["usage_observed"] | right: []`.
#[tokio::test]
async fn a_refused_probe_wakes_nothing() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    let err = m
        .probe_account_for_test("not-a-pool-member")
        .await
        .expect_err("a name with no login directory must be refused");
    assert!(
        err.contains("no login directory"),
        "and refused for THAT reason: {err}"
    );
    m.placement_pass(PassOrigin::Event);
    assert_eq!(
        m.last_placement_reasons(),
        Vec::<String>::new(),
        "a probe that recorded nothing must wake nothing"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 6. GAP 5 — a budget edit reached only half the waiting work
// ═════════════════════════════════════════════════════════════════════════

/// **A BUDGET EDIT IS A PLACEMENT EVENT, NOT A `resume_sweep()` CALL.**
///
/// This gap was half-closed and that is why it survived: `budgets/{key}/set` and
/// `budgets/{key}/disable|enable` already called `resume_sweep()` directly. So the
/// PARKED threads were served and every RETRYING one was left waiting for the
/// tick — and a thread on the usage-limit ladder is precisely the thread a raised
/// limit frees. Routing both through the bell wakes both states through one
/// coalescing window.
///
/// A source guard because `route_request` is `pub(crate)`: no integration test can
/// reach the handler. It reads the comment-free body of `src/server.rs` — not this
/// file — so the ban cannot be satisfied by the prose that explains it.
///
/// FORCED RED, run and recorded: putting `manager.resume_sweep();` back in the
/// `set` arm gives `budgets/{key}/set must ring the bell: a raised limit can free
/// a retrying thread as readily as a parked one`.
#[test]
fn the_budget_handlers_wake_placement() {
    let code = code_of(include_str!("../../src/server.rs"));

    let set_arm = slice(&code, "\"budgets/{key}/set\" =>", "\"budgets/{key}/disable\"");
    assert!(
        set_arm.contains("manager.placement_wake("),
        "budgets/{{key}}/set must ring the bell: a raised limit can free a \
         retrying thread as readily as a parked one"
    );
    assert!(
        !set_arm.contains("resume_sweep"),
        "budgets/{{key}}/set must ring the bell, not call one sweep of the two"
    );

    let toggle = slice(
        &code,
        "\"budgets/{key}/disable\" | \"budgets/{key}/enable\" =>",
        "\"budgets/{key}/delete\" =>",
    );
    assert!(
        toggle.contains("manager.placement_wake("),
        "enable/disable must ring the bell too — a disabled key DENIES every turn \
         that names it, including ones sitting on a retry floor"
    );
    assert!(
        !toggle.contains("resume_sweep"),
        "and must not reach past it to one sweep"
    );
}

/// **A REFUSED BUDGET CALL WAKES NOTHING** — the always-ring opposition for gap 5.
///
/// `budgets/{key}/delete` is a permanent refusal (*"budget keys are never
/// deleted"*). Nothing was written, so no input to placement moved, so a wake
/// there would be placement re-running because a client asked for something it
/// was told it cannot have.
///
/// FORCED RED, run and recorded: adding a `placement_wake` to that arm gives
/// `a refused call must not wake placement: nothing was written`.
#[test]
fn the_refused_budget_delete_wakes_nothing() {
    let code = code_of(include_str!("../../src/server.rs"));
    let arm = slice(
        &code,
        "\"budgets/{key}/delete\" =>",
        "\"threads/{thread_id}/budget_assign\"",
    );
    assert!(
        arm.contains("Err(Error::Config("),
        "the delete arm must still be a refusal — re-point this test if it moved"
    );
    assert!(
        !arm.contains("placement_wake") && !arm.contains("resume_sweep"),
        "a refused call must not wake placement: nothing was written"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 7. COALESCING — twenty-one events must not be twenty-one passes
// ═════════════════════════════════════════════════════════════════════════

/// **A STORM COSTS ONE PASS, AND THE PASS STILL KNOWS WHAT WOKE IT.**
///
/// The event set grew from fourteen rings to twenty-one, and several fire
/// together: a pool reload re-asserts the limit level, the share and the
/// inclusion state of every account, so one `reload_pool` can be five rings
/// before placement has run once. Placement must not re-run five times.
///
/// The coalescing is a property of the primitive rather than a debounce anyone
/// tunes: `Notify::notify_one` stores ONE permit. The reasons are kept beside it
/// in a set, because the permit is the one thing that throws the name away.
///
/// Deterministic, not timing-dependent, and it does not involve the watcher task
/// at all: `#[tokio::test]` is a current-thread runtime, so the twenty
/// synchronous `placement_wake` calls below cannot be interleaved with anything,
/// and the single `placement_pass` that follows is the only pass there is.
///
/// FORCED RED, run and recorded: running a pass inline from `placement_wake` —
/// the obvious "just re-run placement when something changes" implementation —
/// gives `twenty rings must cost ONE pass, got 21` (the twenty inline passes plus
/// the deliberate one). Clearing `wake_pending` on the ring rather than on the
/// pass reddens the reason assertion instead.
#[tokio::test]
async fn a_storm_of_events_costs_one_placement_pass() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    // Five distinct causes, four times each — the shape of a pool reload that
    // enables an account and clears a bar.
    let causes = [
        "pool_reloaded",
        "account_enabled",
        "limit_cleared",
        "limit_expired",
        "pool_share",
    ];
    for _ in 0..4 {
        for c in causes {
            m.placement_wake(c);
        }
    }
    m.placement_pass(PassOrigin::Event);

    let passes = m.placement_passes().total;
    assert_eq!(
        passes, 1,
        "twenty rings must cost ONE pass, got {passes}: placement re-running once \
         per ring is twenty full sweeps for one change of the world"
    );

    let mut named = m.last_placement_reasons();
    named.sort();
    let mut expected: Vec<String> = causes.iter().map(|c| c.to_string()).collect();
    expected.sort();
    assert_eq!(
        named, expected,
        "and the single pass must still name every distinct cause: coalescing may \
         drop the REPETITION, never the REASON"
    );
}

/// **THE SAME STORM THROUGH THE LIVE WATCHER**, which is where the coalescing has
/// to hold in the daemon rather than in a direct call.
///
/// The bound is TWO, not one, and the two is a property of `Notify` worth stating
/// rather than papering over: the first ring wakes the task that is parked on
/// `notified()`, the remaining nineteen find no waiter registered and store ONE
/// permit between them, which the task consumes on its next turn round the loop.
/// So a burst of N costs at most 2 — a constant, not a function of N, which is the
/// property that matters. Asserting exactly 1 here would be asserting a scheduling
/// accident.
///
/// FORCED RED, run and recorded: a pass run inline per ring gives
/// `a burst must cost at most two passes, got 22`. `notify_waiters` in place of
/// `notify_one` loses the stored permit instead, and reddens the first assertion
/// whenever the task is mid-pass when the burst lands.
#[tokio::test]
async fn a_storm_through_the_live_watcher_costs_at_most_two_passes() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    m.start_watchers();
    // Let the watcher reach its wait.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        m.placement_passes().total,
        0,
        "with the backstop off, an idle daemon runs NO placement passes at all — \
         which is the whole difference from a five-second poll"
    );

    for _ in 0..4 {
        for c in [
            "pool_reloaded",
            "account_enabled",
            "limit_cleared",
            "limit_expired",
            "pool_share",
        ] {
            m.placement_wake(c);
        }
    }

    assert!(
        eventually(|| m.placement_passes().total > 0).await,
        "the storm must wake placement at all"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    let passes = m.placement_passes().total;
    assert!(
        passes <= 2,
        "a burst must cost at most two passes, got {passes}"
    );
    assert_eq!(
        m.placement_passes().by_backstop,
        0,
        "and every one of them by the event, not by a clock"
    );
}

/// **A REASON SET IS CONSUMED, NOT ACCUMULATED.**
///
/// The other half of coalescing, and the leak it prevents: `wake_pending` is a
/// bounded set only because every pass takes the whole of it. A pass that read it
/// without draining would make the second pass report the first pass's causes,
/// which is how a diagnostic turns into a lie.
///
/// FORCED RED, run and recorded: cloning `wake_pending` instead of
/// `std::mem::take`-ing it gives `the second pass must report ITS cause, not the
/// first pass's as well | left: ["limit_expired", "new_account"] | right:
/// ["new_account"]`.
#[tokio::test]
async fn each_pass_consumes_its_own_reasons() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    m.placement_wake("limit_expired");
    m.placement_pass(PassOrigin::Event);
    assert_eq!(m.last_placement_reasons(), vec!["limit_expired".to_string()]);

    m.placement_wake("new_account");
    m.placement_pass(PassOrigin::Event);
    assert_eq!(
        m.last_placement_reasons(),
        vec!["new_account".to_string()],
        "the second pass must report ITS cause, not the first pass's as well"
    );

    m.placement_pass(PassOrigin::Backstop);
    assert_eq!(
        m.last_placement_reasons(),
        Vec::<String>::new(),
        "and a backstop pass has no reason to give — which is exactly what makes \
         it distinguishable from the mechanism"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 8. THE BACKSTOP IS A FALLBACK, AND ITS FIRING IS A DEFECT REPORT
// ═════════════════════════════════════════════════════════════════════════

/// **A BACKSTOP PASS THAT PLACES WORK IS A MISSING EVENT, AND IT SAYS SO.**
///
/// The operator's own objection to keeping the timer: *"a backstop that fires
/// constantly also hides a missing event, which is how gap 1 survived."* He is
/// right, and the answer is not to remove the backstop — an event set can always
/// be incomplete and a thread silently parked forever is the worse failure — but
/// to make the backstop's success VISIBLE. A poll that finds work the events
/// should have woken it for is a defect report, not a success, and
/// `backstop_placed` is that report's counter.
///
/// Both directions are asserted, because the counter is only useful if it
/// discriminates: the same placement, attributed to an event, must count zero.
///
/// FORCED RED, run and recorded: making the `backstop_placed` increment a no-op
/// gives `a backstop pass that PLACED something must be counted as a missing
/// event | left: 0 | right: 1`. Counting it on every backstop pass instead reddens
/// `an_empty_backstop_pass_is_not_a_defect` with
/// `an empty poll is the backstop doing its job: insurance that never paid out |
/// left: 3 | right: 0`.
#[tokio::test]
async fn a_backstop_pass_that_places_work_is_reported_as_a_defect() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let id = m.create_thread(None, plain(tmp.path())).unwrap();
    park(&m, &id);

    // Nothing is barring it — exactly the situation that means an event was
    // missed, since something made it placeable and nobody rang.
    let placed = m.placement_pass(PassOrigin::Backstop);
    assert_eq!(
        placed,
        vec![id.clone()],
        "the pass must say WHICH thread it placed: a defect report that cannot \
         name the stranded thread is not actionable"
    );
    let p = m.placement_passes();
    assert_eq!(
        p.by_backstop, 1,
        "the pass must be attributed to the backstop"
    );
    assert_eq!(
        p.backstop_placed, 1,
        "a backstop pass that PLACED something must be counted as a missing event"
    );
    assert_eq!(p.by_event, 0, "and not as an event pass");

    // The same placement, by an event: the mechanism working.
    let id2 = m.create_thread(None, plain(tmp.path())).unwrap();
    park(&m, &id2);
    m.placement_wake("limit_expired");
    assert_eq!(m.placement_pass(PassOrigin::Event), vec![id2]);
    let p = m.placement_passes();
    assert_eq!(
        p.backstop_placed, 1,
        "a pass an event woke is the mechanism working, not a defect"
    );
    assert_eq!(p.by_event, 1);
    assert_eq!(p.placed, 2, "both placements are still counted");
}

/// **A BACKSTOP PASS THAT FINDS NOTHING IS NOT A DEFECT.** The ordinary state of
/// a healthy daemon, and the reason `backstop_placed` is counted rather than
/// `by_backstop`: the poll running is free and expected; the poll CARRYING the
/// mechanism is the thing worth an `error` line.
///
/// FORCED RED, run and recorded: incrementing `backstop_placed` on every backstop
/// pass rather than only on one that placed something gives `an empty poll is the
/// backstop doing its job: insurance that never paid out | left: 3 | right: 0`.
#[tokio::test]
async fn an_empty_backstop_pass_is_not_a_defect() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    for _ in 0..3 {
        assert_eq!(m.placement_pass(PassOrigin::Backstop), Vec::<String>::new());
    }
    let p = m.placement_passes();
    assert_eq!(p.by_backstop, 3);
    assert_eq!(
        p.backstop_placed, 0,
        "an empty poll is the backstop doing its job: insurance that never paid out"
    );
    assert_eq!(p.placed, 0);
}

// ═════════════════════════════════════════════════════════════════════════
// 6. PARKING IS A POOL STATE, NOT AN ACCOUNT ONE (H179)
// ═════════════════════════════════════════════════════════════════════════
//
// The operator's ruling: *"you never park TO an account. Parked threads wait in
// the pool; any placement event triggers re-placing ALL parked threads, each onto
// whichever account admission picks."* The board drew "3 parked on
// alice-example-com", which read as the daemon holding those turns FOR cc1.
//
// These pin the daemon half: a turn parked while its thread sat on account A is
// re-placed onto B the moment B becomes available — by B's own event, with the
// backstop off, and without A ever changing. If the re-run were per-account
// (only threads homed on the account that rang), or if the parked turn were
// bound to its last account, every one of these would stay parked forever.

/// Two accounts. The thread lives on `aa-home` (its conversation is there, and
/// `aa-home` is over the thread's weekly threshold for good), and `zz-other` is
/// withheld by `withhold` before the thread is created, so creation can only land
/// on `aa-home`. Returns with the turn parked and a control pass proving it.
async fn parked_on_home(
    tmp: &Path,
    withhold: impl Fn(&ThreadManager),
) -> (ThreadManager, String) {
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp, &["aa-home", "zz-other"])).unwrap();
    m.start_watchers();
    withhold(&m);
    let id = m.create_thread(None, plain(tmp)).unwrap();
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "aa-home",
        "fixture: with zz-other withheld the thread can only be homed on aa-home"
    );
    // Its CONVERSATION is on aa-home — the case the continuity rule is about, so
    // the move below has to go through failover, not the fresh-thread path.
    m.store.record_thread_account(&id, "aa-home").unwrap();
    observe_weekly(&m, "aa-home", 99.0);
    observe_weekly(&m, "zz-other", 10.0);
    park(&m, &id);
    assert_eq!(
        m.placement_pass(PassOrigin::Event),
        Vec::<String>::new(),
        "control: aa-home is over the ceiling and zz-other is withheld — nothing \
         in the pool can take this turn yet"
    );
    assert!(m.store.get_parked_turn(&id).unwrap().is_some());
    (m, id)
}

async fn assert_replaced_on_other(m: &ThreadManager, id: &str, what: &str) {
    assert!(
        eventually(|| m.store.get_parked_turn(id).unwrap().is_none()).await,
        "{what}: the parked turn must be re-placed by THAT account's event — it was \
         parked while homed on aa-home, and aa-home never changed. park={:?}",
        m.park_state(id).unwrap()
    );
    assert_eq!(
        m.serving_account(id).unwrap(),
        "zz-other",
        "{what}: and placed where admission put it (zz-other), not on its last home"
    );
    assert_eq!(m.placement_passes().backstop_placed, 0, "{what}: by the event, not a clock");
}

/// **ENABLE re-places a turn parked on ANOTHER account.**
///
/// What the handler for `accounts/{account}/enable` does, in its order: delete
/// the disable row, then ring `account_enabled` (the guard above pins that the
/// handler rings it).
#[tokio::test]
async fn enabling_b_re_places_a_turn_parked_while_homed_on_a() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let (m, id) = parked_on_home(tmp.path(), |m| {
        m.store
            .set_account_disabled("zz-other", true, Some("test"))
            .unwrap();
    })
    .await;
    m.store.set_account_disabled("zz-other", false, None).unwrap();
    m.account_picture_changed_for("account_enabled", Some("zz-other"));
    assert_replaced_on_other(&m, &id, "enable").await;
}

/// **LOGIN RESTORED re-places a turn parked on ANOTHER account.**
#[tokio::test]
async fn a_login_restored_on_b_re_places_a_turn_parked_while_homed_on_a() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let (m, id) = parked_on_home(tmp.path(), |m| {
        m.store
            .set_account_limit(
                "zz-other",
                Some((&far_future(), Some("login expired"), Some("login"), None)),
            )
            .unwrap();
    })
    .await;
    // R12.5b: the restore lifts the bar only once the re-check verifies the slot.
    fake_login::sign_in_verified(&m, "zz-other", "zz-other@x.test");
    assert!(
        !m.note_login_restored("zz-other").is_empty(),
        "fixture: the login bar on zz-other is what the restore lifts"
    );
    assert_replaced_on_other(&m, &id, "login restored").await;
}

/// **A LIMIT WINDOW ENDING ON B re-places a turn parked on A** — the exact
/// shape of the log line that read as per-account ("LIMIT WINDOW ENDED …
/// account=cc1"): the account named on the bell is narration, the pass is the
/// whole pool.
#[tokio::test]
async fn a_limit_window_ending_on_b_re_places_a_turn_parked_while_homed_on_a() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let (m, id) = parked_on_home(tmp.path(), |m| limit(m, "zz-other", Some(&far_future()))).await;
    // Arm the edge the noticer takes, then end the window.
    m.notice_expired_limits();
    limit(&m, "zz-other", Some(&long_past()));
    assert_eq!(m.notice_expired_limits(), vec!["zz-other".to_string()]);
    assert_replaced_on_other(&m, &id, "limit expired").await;
}

/// **A TURN COMPLETING ANYWHERE re-runs placement for every parked turn.**
///
/// The operator lists "a turn completing" among the placement events. Before
/// this, `finish_turn` re-asked only ITS OWN thread's queue; a parked turn on
/// another thread waited for some other bell. Here the input that frees the
/// parked turn moves silently (a reading recorded with no ring), and the only
/// thing that happens afterwards is an unrelated thread's turn ending.
///
/// FORCED RED, run and recorded: without the `turn_completed` wake in
/// `finish_turn` the parked turn stays parked and this fails on its first
/// assertion.
#[tokio::test]
async fn a_turn_completing_on_another_thread_re_places_every_parked_turn() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    m.start_watchers();
    let parked = m.create_thread(None, plain(tmp.path())).unwrap();
    let other = m.create_thread(None, plain(tmp.path())).unwrap();
    observe_weekly(&m, "aa-owned", 99.0);
    park(&m, &parked);
    assert_eq!(m.placement_pass(PassOrigin::Event), Vec::<String>::new());

    // The world moves with NO bell (the reading is recorded directly).
    observe_weekly(&m, "aa-owned", 10.0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        m.store.get_parked_turn(&parked).unwrap().is_some(),
        "control: nothing rang, the backstop is off, so nothing has re-placed it yet"
    );

    // An unrelated thread takes a turn and finishes it.
    m.send(&other, "hello".into(), threads::SendIntent::Immediate, serde_json::json!({}), false)
        .unwrap();
    assert!(
        eventually(|| m.store.get_parked_turn(&parked).unwrap().is_none()).await,
        "a turn completing is a placement event: every parked turn in the pool must \
         be re-asked when one ends"
    );
}

/// **A POOL RELOAD THAT ADDS B re-places a turn parked while homed on A.**
///
/// This was the daemon's real binding. The pass ran and admission picked B,
/// but failover's port resolved B's transcript store through the BOOT-TIME
/// registry, where an account added after boot does not exist — so the port
/// failed ("failover target admits this thread but the conversation could not
/// be carried"), and the turn parked for CONTINUITY under A, for good: the
/// exact reason text on the operator's board.
///
/// FORCED RED, run and recorded: with `transcript_store_of` reading only
/// `self.registry`, this fails on its first assertion with the park still in
/// place.
#[tokio::test]
async fn a_new_account_joining_re_places_a_turn_parked_while_homed_on_a() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-home"])).unwrap();
    m.start_watchers();
    let id = m.create_thread(None, plain(tmp.path())).unwrap();
    m.store.record_thread_account(&id, "aa-home").unwrap();
    observe_weekly(&m, "aa-home", 99.0);
    park(&m, &id);
    assert_eq!(m.placement_pass(PassOrigin::Event), Vec::<String>::new());

    std::fs::create_dir_all(tmp.path().join("zz-other")).unwrap();
    let reg = tmp.path().join("accounts.toml");
    let mut toml = std::fs::read_to_string(&reg).unwrap_or_default();
    toml.push_str(&format!(
        "\n[accounts.zz-other]\nconfig_dir = \"{}\"\n",
        tmp.path().join("zz-other").display()
    ));
    std::fs::write(&reg, toml).unwrap();
    assert_replaced_on_other(&m, &id, "new account").await;
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
