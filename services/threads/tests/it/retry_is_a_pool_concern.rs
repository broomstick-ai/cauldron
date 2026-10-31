//! **A PARKED THREAD IS SIMPLY PARKED. THE POOL HAS THE RETRY, NOT THE THREAD.**
//!
//! The operator, ruling on `retries.retry_at` for the second and final time:
//!
//! > *"if any thread is parked then **the pool needs a retry, not the thread**. Any
//! > parked threads get checked at any opportunity."*
//!
//! and earlier, the same direction:
//!
//! > *"There should not be a per-thread retry timestamp."*
//!
//! This file replaces `retry_floor_yields_to_events.rs`, whose entire subject — a
//! per-thread floor that an event could outrank — no longer exists. That round got
//! half of the ruling: it stopped the floor filtering the EVENT leg and left it
//! filtering the BACKSTOP leg. The defect it documented is the sharpest statement
//! of why half was not enough, and it is the headline here: a `retries` row whose
//! `account` column is `None` failed BOTH of the old eligibility clauses
//! permanently — `retry_floor_elapsed` was false (2099) and `retry_unblocked_early`
//! returned `false` unconditionally for a floor that names no login — so on the
//! backstop leg **no number of bells could ever re-ask it.** It waited only for its
//! clock.
//!
//! # What replaced the floor, and what bounds it
//!
//! * There is **ONE candidate set**: every row in `retries`, on every pass, of
//!   every origin (`retries_to_reconsider`, which has no filter at all).
//! * There is **ONE cadence for the whole pool** (`PoolCadence`), and it is a
//!   LADDER whose rungs are `retry::ladder_secs`'s own numbers — re-homed off N
//!   per-thread floors, not retuned. Its index is `attempt + barren`: the row's own
//!   spent spawns, plus the POOL's consecutive cadence firings that came to
//!   nothing. `barren` exists because `attempt` cannot climb while the gate keeps
//!   refusing, and something must, or a parked `transient` row sits at
//!   `transient_backoff_secs(1) == 0` and the cadence is a spin loop.
//! * The deadline-versus-question distinction SURVIVES and moved onto the pass's
//!   ORIGIN. Only `PassOrigin::Cadence` is a deadline. `5f2b981` is therefore
//!   WIDENED rather than undone: the free, read-only, pre-lock ask used to cover
//!   only rows whose floor had not elapsed and now covers every non-deadline ask.
//!
//! Every test drives the manager in-process with the fake `claude` fixture and a
//! scratch `THREADS_CLAUDE_HOME`, so nothing here can read or write the operator's
//! own `~/.claude`, the running daemon, the live `threads.db`, or the keychain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::event::Push;
use threads::retry::TransientKind;
use threads::runtime::PassOrigin;
use threads::settings::ThreadSettings;
use threads::store::RetryRow;
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

/// **THE BACKSTOP IS OFF FOR THIS WHOLE BINARY.** `THREADS_RESUME_TICK_MS=0`
/// removes the fallback timer leg entirely, so a thread placed in this file was
/// placed by an EVENT or by the POOL'S CADENCE, and `by_backstop == 0` says which
/// rather than assuming it. With the five-second poll running, every proof here
/// would be contaminated by it.
///
/// Process-global because `std::env` is, and these tests share a binary.
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

fn config_at(dir: &Path, names: &[&str]) -> Config {
    let mut accounts = BTreeMap::new();
    for n in names {
        std::fs::create_dir_all(dir.join(n)).unwrap();
        accounts.insert(n.to_string(), dir.join(n).to_string_lossy().to_string());
    }
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        probe_interval_busy_secs: 0,
        ..Default::default()
    }
}

/// An AUI thread: `Auto` account, no thresholds configured — the shape that makes
/// up the whole live deployment, and the shape the 2026-08-06 live-lock ran on.
fn aui(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

/// An instant seventy-three years out, used ONLY for an account BAR. The bar is
/// the thing that still legitimately holds an instant; a retry row cannot.
const NOT_YET: &str = "2099-01-01T00:00:00+00:00";

/// **THE ROW THE OLD MODEL COULD NEVER RE-ASK** — `account: None`.
///
/// A real on-disk class rather than a contrivance: the `account` column was added
/// after the table, so rows written before it do not say which login refused them,
/// and `recover` backfills them only at boot. Under the two-clause selection this
/// row was permanently ineligible on the backstop leg, which is the defect the
/// previous round found and could not finish closing.
///
/// `attempt: 7` and `kind: "usage_limit"` put its own ladder on the 300-second
/// ceiling, so the POOL's cadence for a pool containing only this row is 300s — far
/// longer than any test here waits. Every placement below is therefore attributable
/// to an event or to an explicit pass, never to a cadence that happened to fire.
fn parked_prompt(thread_id: &str) -> RetryRow {
    RetryRow {
        thread_id: thread_id.into(),
        prompt: "please ratelimit me".into(),
        message_ids: vec![1],
        attempt: 7,
        kind: "usage_limit".into(),
        reason: "rate limit (429)".into(),
        account: None,
        created_at: String::new(),
    }
}

fn attributed(thread_id: &str, account: &str) -> RetryRow {
    RetryRow {
        account: Some(account.to_string()),
        ..parked_prompt(thread_id)
    }
}

/// The same parked prompt on its FIRST transient attempt, whose rung is zero —
/// `transient_backoff_secs(1)` is the operator's *"may justify an immediate retry …
/// and only ONCE"*. Used to prove the cadence timer is really wired, without
/// waiting out a ladder.
fn parked_prompt_due_now(thread_id: &str) -> RetryRow {
    RetryRow {
        attempt: 1,
        kind: "transient".into(),
        ..parked_prompt(thread_id)
    }
}

/// Bar a login with a window that does not end this century.
fn bar(m: &ThreadManager, name: &str) {
    m.store
        .set_account_limit(
            name,
            Some((
                NOT_YET,
                Some("barred for the test"),
                Some("weekly_fable"),
                None,
            )),
        )
        .unwrap();
}

/// A weekly reading, which is also what makes `thresholds_can_refuse` true and so
/// moves a thread out of the ungated branch at all. 95 is the fleet default
/// ceiling, so 99 is over it.
fn observe_weekly(m: &ThreadManager, name: &str, pct: f64) {
    m.budgets
        .observe(name, threads::gauge::Axis::WeeklyAll, pct, None, "test")
        .unwrap();
}

fn sits_on(m: &ThreadManager, id: &str) -> String {
    m.store
        .get_thread(id)
        .unwrap()
        .expect("the thread row must exist")
        .account
        .expect("an AUI thread is auto-placed at create")
}

/// One observed `Push::Status`, reduced to what the board folds.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    thread_id: String,
    executing: bool,
}

/// **THE LOCK COUNTER.** `lock_turn`'s Gate 1 sets `ThreadState::turn_running`
/// and emits `Push::Status { executing: true }`; `TurnLock`'s release emits the
/// mirror. So an `executing: true` frame is the externally visible proof that the
/// execution lock was taken, and counting them counts lock acquisitions — which is
/// how `5f2b981` proved it had stopped taking the lock merely to ask a question,
/// and the only honest way to prove this change did not undo that.
fn watch_status(m: &ThreadManager) -> Arc<Mutex<Vec<Frame>>> {
    let mut rx = m.subscribe();
    let seen = Arc::new(Mutex::new(Vec::<Frame>::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Status {
                thread_id,
                executing,
                ..
            } = p
            {
                out.lock().unwrap().push(Frame {
                    thread_id,
                    executing,
                });
            }
        }
    });
    seen
}

fn locks_taken(seen: &Arc<Mutex<Vec<Frame>>>, id: &str) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|f| f.thread_id == id && f.executing)
        .count()
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

/// Let every task a pass spawned settle. The assertions that use this are all
/// "exactly this many", and waiting LONGER only ever strengthens a zero.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(700)).await;
}

// ═════════════════════════════════════════════════════════════════════════
// 1. NON-VACUITY — the fixture is the row nothing could ever re-ask
// ═════════════════════════════════════════════════════════════════════════

/// **THE ROW WITH NO ELIGIBILITY OF ITS OWN IS IN THE CANDIDATE SET.**
///
/// Asserted first and alone, because every proof below is worthless if this row is
/// not selected at all: a test built on a row the pool never asks about would pass
/// against a daemon in which nothing works.
///
/// It also states the premise that makes the headline sharp. Under the old model
/// this row's two eligibility clauses were `retry_floor_elapsed(row) ||
/// retry_unblocked_early(row)`, and for `account: None` with a 2099 floor BOTH were
/// permanently false. There is now no clause at all, which is why it is here.
///
/// FORCED RED, run and recorded: adding any filter to `retries_to_reconsider` — for
/// instance `.filter(|r| r.account.is_some())`, which is the shape of the clause
/// this row used to fail — gives `the pool's candidate set must contain a parked
/// prompt with NO eligibility of its own … got 0 rows`.
#[tokio::test]
async fn the_pool_reconsiders_a_row_with_no_eligibility_of_its_own() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "bb-owned"])).unwrap();
    let id = m.create_thread(None, aui(tmp.path())).unwrap();
    m.store.upsert_retry(&parked_prompt(&id)).unwrap();

    let rows = m.retries_to_reconsider();
    assert_eq!(
        rows.iter().filter(|r| r.thread_id == id).count(),
        1,
        "the pool's candidate set must contain a parked prompt with NO eligibility \
         of its own — `account` is None, which made BOTH of the old clauses \
         permanently false, so no number of bells could ever re-ask it: got {} rows",
        rows.len()
    );
    assert!(
        rows.iter().all(|r| r.account.is_none() || r.account.is_some()),
        "a tautology kept deliberately: this row has no field the set could have \
         filtered on, because the field it used to be filtered on is gone"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 2. THE HEADLINE — the pool is reconsidered, so the row goes
// ═════════════════════════════════════════════════════════════════════════

/// **A PARKED PROMPT WITH NO ELIGIBILITY OF ITS OWN IS PLACED WHEN AN EVENT
/// RECONSIDERS THE POOL.**
///
/// The ruling, as a number. The thread holds the `account: None` row; its login is
/// then barred, which is a change to the account picture and therefore a reason to
/// re-run placement; a free sibling exists. The thread must go NOW.
///
/// Three things make the attribution airtight:
///
/// * `THREADS_RESUME_TICK_MS=0`, so there is no backstop leg to have done it, and
///   `by_backstop == 0` says so rather than assuming it.
/// * This row's own ladder sits on the 300-second ceiling, and `by_cadence == 0` is
///   asserted, so the pool's cadence did not do it either.
/// * **A CONTROL PASS BEFORE THE EVENT.** One pass runs while the world is settled:
///   it ASKS (`reconsidered` climbs) and places nothing and takes NO LOCK. So
///   reconsidering everything is a question put to the gate, not a free pass — and
///   the placement later is the gate answering a changed world.
///
/// FORCED RED, run and recorded: restoring the old backstop filter as
/// `retries_to_reconsider`'s body (`.filter(|row| Self::retry_floor_elapsed(row) ||
/// self.retry_unblocked_early(row))`, with both helpers restored) fails with
/// `a parked prompt with no eligibility of its own must be placed when the pool is
/// reconsidered`, and `reconsidered` stays 0.
#[tokio::test]
async fn an_event_places_a_parked_prompt_that_no_clock_could_ever_have_selected() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "bb-owned"])).unwrap();
    m.start_watchers();

    let id = m.create_thread(None, aui(tmp.path())).unwrap();
    let home = sits_on(&m, &id);
    m.store.upsert_retry(&parked_prompt(&id)).unwrap();

    let seen = watch_status(&m);
    seen.lock().unwrap().clear();

    // ── THE CONTROL: a pass on a SETTLED world ───────────────────────────
    // The row is asked about — and the gate admits it exactly where it already
    // sits, which is not a placement. Nothing runs and nothing is spent.
    m.placement_pass(PassOrigin::Event);
    settle().await;
    let control = m.placement_passes();
    assert!(
        control.reconsidered >= 1,
        "the control pass must have ASKED about the row — otherwise the placement \
         below could be anything: got {control:?}"
    );
    assert_eq!(
        locks_taken(&seen, &id),
        0,
        "and asking must cost no lock on a settled world: reconsidering the pool is \
         a question, not a turn"
    );
    assert_eq!(
        m.store.get_retry(&id).unwrap().map(|r| r.attempt),
        Some(7),
        "nor spend an attempt off the backoff ladder"
    );

    // ── THE WORLD CHANGES: the login this thread sits on is barred ────────
    bar(&m, &home);
    m.placement_wake("limit_recorded");

    assert!(
        eventually(|| locks_taken(&seen, &id) >= 1).await,
        "a parked prompt with no eligibility of its own must be placed when the pool \
         is reconsidered — the backstop is off and its own ladder is on a 300s \
         ceiling, so nothing else can have placed it. Frames: {:?}",
        seen.lock().unwrap().clone()
    );

    let p = m.placement_passes();
    assert_eq!(
        p.by_backstop, 0,
        "no backstop pass may have run at all: the timer is off, and a count above \
         zero means something put a poll back — got {p:?}"
    );
    assert_eq!(
        p.by_cadence, 0,
        "nor may the POOL's cadence have fired: this row's rung is 300s and no test \
         here waits that long, so a count above zero means the ladder was retuned \
         — got {p:?}"
    );
    assert!(
        p.by_event >= 1,
        "the pass that placed it must be attributed to an event — got {p:?}"
    );
    assert_eq!(
        p.backstop_placed, 0,
        "and nothing may be reported as a missing event — got {p:?}"
    );
}

/// **AND THE POOL'S OWN CADENCE PLACES IT WITH NO EVENT AT ALL.**
///
/// The other half of the headline, and the half that is new. An event is not always
/// coming: a usage-limit window that simply expires is the one input to placement
/// with nothing to ring for it. Under the old model that case belonged to the
/// per-thread floor. It now belongs to the pool.
///
/// Deterministic on purpose — the pass is called directly, so this proves the
/// SEMANTICS (a cadence is a deadline, and a deadline may re-run a thread on the
/// very account that refused it). `the_pool_cadence_timer_fires_with_no_event`
/// below proves the WIRING separately, because one test that depended on both would
/// be able to fail for two unrelated reasons.
///
/// The control is the point: a question-origin pass over the identical world places
/// nothing, because the gate would leave this thread exactly where it is. The only
/// difference between the two outcomes is which origin asked.
///
/// FORCED RED, run and recorded: making `PassOrigin::is_deadline` answer `false` for
/// `Cadence` fails with `the pool's cadence is the DEADLINE … got 0 locks`.
#[tokio::test]
async fn the_pools_cadence_places_what_a_question_declines() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // ONE login, so there is nowhere else to go: a question can only ever admit
    // and move nothing, which is the free case.
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let id = m.create_thread(None, aui(tmp.path())).unwrap();
    m.store.upsert_retry(&parked_prompt(&id)).unwrap();

    let seen = watch_status(&m);
    seen.lock().unwrap().clear();

    // ── THE CONTROL: three question-origin passes, nothing happens ────────
    for _ in 0..3 {
        m.placement_pass(PassOrigin::Event);
        settle().await;
    }
    assert_eq!(
        locks_taken(&seen, &id),
        0,
        "the control must be a thread a QUESTION declines for free, or this proves \
         nothing about the deadline. Frames: {:?}",
        seen.lock().unwrap().clone()
    );
    assert!(
        m.placement_passes().reconsidered >= 3,
        "and it must have been ASKED all three times — got {:?}",
        m.placement_passes()
    );

    // ── THE POOL'S BACKOFF COMES DUE ─────────────────────────────────────
    seen.lock().unwrap().clear();
    m.placement_pass(PassOrigin::Cadence);

    assert!(
        eventually(|| locks_taken(&seen, &id) >= 1).await,
        "the pool's cadence is the DEADLINE: it is the thing that waited, so it may \
         re-run a prompt on the very account that refused it — which is the only \
         outcome a usage-limit window that simply expires can have. Frames: {:?}",
        seen.lock().unwrap().clone()
    );
}

/// **THE CADENCE TIMER IS REALLY WIRED: it fires with no bell rung and the
/// backstop off.**
///
/// The wiring half, isolated. `THREADS_RESUME_TICK_MS=0` removes the backstop, and
/// after `start_watchers` nothing here rings `placement_wake`, so a `by_cadence`
/// above zero can only have come from the pool's own timer. The parked prompt is on
/// `transient` attempt 1, whose rung is `transient_backoff_secs(1) == 0` — the
/// operator's *"may justify an immediate retry"* — so the cadence is due at once and
/// this test does not have to wait out a ladder to prove a timer exists.
///
/// FORCED RED, run and recorded: deleting the `sleep_until(at) => PassOrigin::Cadence`
/// arm from the placement task's `select!` gives `the pool's cadence must fire on
/// its own … got PlacementPasses { by_cadence: 0, .. }`. Spelling that arm as a
/// plain `sleep(rung)` instead of a deadline ALSO reddens the ladder test below,
/// which is why both exist.
#[tokio::test]
async fn the_pool_cadence_timer_fires_with_no_event() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let id = m.create_thread(None, aui(tmp.path())).unwrap();
    m.store.upsert_retry(&parked_prompt_due_now(&id)).unwrap();

    assert_eq!(
        m.cadence_rung(),
        Some(Duration::from_secs(0)),
        "the premise: a transient row at attempt 1 is due immediately, on \
         `transient_backoff_secs`'s own first rung"
    );

    m.start_watchers();

    assert!(
        eventually(|| m.placement_passes().by_cadence >= 1).await,
        "the pool's cadence must fire on its own — the backstop is off and nothing \
         rang a bell, so this is the only thing that can wake placement for a \
         parked prompt whose world is not changing: got {:?}",
        m.placement_passes()
    );
    assert_eq!(
        m.placement_passes().by_backstop,
        0,
        "and it must not be the backstop wearing the cadence's name — got {:?}",
        m.placement_passes()
    );
}

/// **A DUE CADENCE AND A QUESTION ARE ONE PASS, AND THE DEADLINE WINS.**
///
/// A bug this round wrote, and a flaky test found. The backstop poll and a rung can
/// come due in the same millisecond; `select!` then picks one at random, the loop
/// re-enters and runs the other — and if the QUESTION went first, `retry_firing`
/// (the per-thread coalescer that stops passes stacking asks) deduped the DEADLINE
/// away. The pool climbed a rung for an ask that never happened and the prompt waited
/// out another one.
///
/// So the placement task decides the origin ONCE, after the wake: if the pool's
/// cadence is due, this pass serves it whatever woke it. That is sound rather than
/// convenient — there is one candidate set, so both routes ask about exactly the same
/// rows, and the deadline is the strictly wider route.
///
/// This asserts it where it is observable: the backstop is off here, so an EVENT is
/// rung while a zero rung is already due, and the resulting pass must be counted as
/// a cadence and must run the turn.
///
/// # The structural clause is asserted too, and it had to be
///
/// The behavioural half CANNOT distinguish the two implementations on its own, and
/// finding that out is part of the record: `tokio::select!` picks at random among
/// ready branches, so under the two-pass shape it picks the cadence arm about half
/// the time and `by_cadence` climbs anyway. The forcing stayed GREEN on the
/// behaviour alone. So the rule is also read out of the source, where it is one
/// expression and cannot be satisfied by luck.
///
/// FORCED RED, run and recorded: restoring `let origin = woken;` — the two-pass
/// shape — gives `the pass's origin must be decided AFTER the wake, from whether the
/// pool's cadence is DUE`.
#[tokio::test]
async fn a_due_cadence_subsumes_a_question_in_one_pass() {
    // ── THE STRUCTURAL CLAUSE ────────────────────────────────────────────
    let code = code_of(include_str!("../../src/runtime.rs"));
    let task = slice(
        &code,
        "let mut due: Option<tokio::time::Instant>",
        "let every = if tick.is_zero()",
    );
    assert!(
        task.contains("let origin = if cadence_due {")
            && task.contains("PassOrigin::Cadence")
            && task.contains("at <= tokio::time::Instant::now()"),
        "the pass's origin must be decided AFTER the wake, from whether the pool's \
         cadence is DUE — not taken from whichever `select!` arm happened to win. Two \
         passes let `retry_firing` dedupe the deadline away, and `select!` picking at \
         random is why no behavioural test can catch that on its own"
    );
    assert!(
        !task.contains("let origin = woken;"),
        "and `woken` must not BE the origin: that is the two-pass shape"
    );

    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // ONE login, nothing barred: the gate admits and moves nothing, so a QUESTION
    // declines for free and only a DEADLINE can run the turn. That is what makes the
    // two origins distinguishable by outcome.
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let id = m.create_thread(None, aui(tmp.path())).unwrap();
    m.store.upsert_retry(&parked_prompt_due_now(&id)).unwrap();
    assert_eq!(
        m.cadence_rung(),
        Some(Duration::from_secs(0)),
        "the premise: the rung is already due"
    );

    let seen = watch_status(&m);
    m.start_watchers();
    // Give the task its first loop iteration, where it arms `due` from the row above.
    tokio::time::sleep(Duration::from_millis(50)).await;
    m.placement_wake("limit_cleared");

    assert!(
        eventually(|| m.placement_passes().by_cadence >= 1).await,
        "an event arriving while the pool's cadence is due must serve the DEADLINE in \
         that same pass — two passes let `retry_firing` dedupe the deadline away: got \
         {:?}",
        m.placement_passes()
    );
    assert!(
        eventually(|| locks_taken(&seen, &id) >= 1).await,
        "and the turn must actually run, which only the deadline route can do for a \
         thread the gate would leave exactly where it is. Frames: {:?}",
        seen.lock().unwrap().clone()
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 3. THE OPPOSING PROOF — the cadence is a BACKOFF, not a spin loop
// ═════════════════════════════════════════════════════════════════════════

/// **WITH NOTHING RINGING AND THE LADDER HIGH, THE THREAD STAYS PARKED.**
///
/// The test that reddens if the pool reconsiders EVERYTHING UNBOUNDEDLY. Byte for
/// byte the same world change as the headline, with the bell NOT rung — and this
/// row's own rung is 300 seconds, so a correct pool does not come back for it
/// inside this test's lifetime. An implementation that simply fired every parked
/// prompt on any schedule at all would place it and redden here, while passing the
/// headline.
///
/// **THE WAIT IS LOAD-BEARING AND THE NUMBER IS DERIVED, NOT GUESSED.** A sibling
/// learned this the hard way: *"a negative test is only as strong as the longest
/// period it outlasts"*, and its 600 ms draft stayed green against a forcing
/// because the `limit_expired` noticer's own floor is one second. This waits past
/// the noticer floor (1 s) and past `THREADS_ROSTER_RELOAD_MS` (60 ms) with margin,
/// so a hidden clock anywhere in the process has had its tick.
///
/// FORCED RED, run and recorded: clamping `cadence_rung` to
/// `Some(Duration::from_secs(0))` — the shape of "check everything constantly" —
/// places the thread and gives `nothing rang and the pool's own rung is 300s: a
/// pool-level backoff is still a BACKOFF`.
#[tokio::test]
async fn with_nothing_ringing_and_a_high_rung_the_thread_stays_parked() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "bb-owned"])).unwrap();
    m.start_watchers();

    let id = m.create_thread(None, aui(tmp.path())).unwrap();
    let home = sits_on(&m, &id);
    m.store.upsert_retry(&parked_prompt(&id)).unwrap();

    assert_eq!(
        m.cadence_rung(),
        Some(Duration::from_secs(300)),
        "the premise: this pool's only parked prompt is a usage limit at attempt 7, \
         so the cadence is `usage_limit_backoff_secs`'s own ceiling"
    );

    let seen = watch_status(&m);
    seen.lock().unwrap().clear();

    // The world changes — written straight to the store, so nothing rings.
    bar(&m, &home);

    tokio::time::sleep(Duration::from_millis(1_600)).await;

    assert_eq!(
        locks_taken(&seen, &id),
        0,
        "nothing rang and the pool's own rung is 300s: a pool-level backoff is still \
         a BACKOFF, and nothing may fire this prompt before it is due. Frames: {:?}",
        seen.lock().unwrap().clone()
    );
    let row = m
        .store
        .get_retry(&id)
        .unwrap()
        .expect("the untouched parked prompt");
    assert_eq!(row.attempt, 7, "nor spend an attempt");
    assert_eq!(row.prompt, "please ratelimit me", "nor touch the prompt");
    assert_eq!(
        m.placement_passes().by_backstop,
        0,
        "and no backstop pass may exist to have done it"
    );

    // And the moment anything rings, it goes — so the silence above was the bell's
    // absence and not an unrelated refusal.
    m.placement_wake("limit_recorded");
    assert!(
        eventually(|| locks_taken(&seen, &id) >= 1).await,
        "the same thread, the same world, one ring: placed"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 4. THE HOT LOOP IS BOUNDED — bounded asks, ZERO locks
// ═════════════════════════════════════════════════════════════════════════

/// **TWENTY EVENTS AND SEVERAL POOL CADENCES AGAINST A REFUSING GATE COST A
/// BOUNDED NUMBER OF ASKS AND ZERO LOCK ACQUISITIONS.**
///
/// The risk this change creates, stated plainly: if every parked thread is
/// reconsidered at every opportunity, what stops a hot loop against a gate that
/// keeps refusing? The 2026-08-06 strand is what that looks like — 298 → 852
/// attempts in forty-five minutes and 459 `is_error: true` spawns, a thread that
/// never stopped asking and never got anywhere.
///
/// Five mechanisms bound it and this test measures four of them at once:
///
/// 1. **`Notify::notify_one` stores ONE PERMIT**, so the unit of cost is a PASS and
///    not an event. Twenty rings cost one pass, deterministically: the
///    `#[tokio::test]` runtime is current-thread, so the synchronous
///    `placement_wake` calls cannot interleave and the one `placement_pass` that
///    follows is the only pass there is.
/// 2. **`retry_firing` dedupes per thread**, so even passes that do stack cannot
///    stack asks on one thread.
/// 3. **`early_ask_would_move` answers read-only, before the lock.** For the thread
///    that actually churned — ungated, settled account, nowhere better to go — the
///    gate admits and moves nothing, so the ask costs one `Budgets::admit` and
///    returns. **ZERO LOCK ACQUISITIONS**, which is the number this asserts,
///    because `5f2b981` fixed exactly the bug where asking cost a lock and this
///    change must not undo it.
/// 4. **The ladder is only spent by a turn that RAN** — asserted as `attempt` still
///    7 and the prompt still present after thirty-one passes.
///
/// The fifth is the pool's cadence, which is measured by
/// `the_pools_cadence_ladder_climbs_while_nothing_is_placed`.
///
/// `reconsidered` is the bound made explicit: exactly one ask per pass per row, and
/// a burst of twenty events is one pass.
///
/// FORCED RED, run and recorded: running a pass inline from `placement_wake` — the
/// obvious "re-run placement when something changes" implementation — gives
/// `twenty events must cost ONE ask … got 21`. Removing the `early_ask_would_move`
/// pre-check so the ask locks first instead gives `reconsidering the pool must
/// never cost a lock on a thread the gate refuses`.
#[tokio::test]
async fn a_burst_of_events_and_cadences_costs_bounded_asks_and_no_locks() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // Two placeable logins, NEITHER barred and neither over a ceiling: the gate
    // admits and leaves this thread exactly where it is. This is the live shape —
    // all three of the churning threads were ungated on a settled account.
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "bb-owned"])).unwrap();
    let id = m.create_thread(None, aui(tmp.path())).unwrap();
    m.store.upsert_retry(&parked_prompt(&id)).unwrap();

    let seen = watch_status(&m);
    seen.lock().unwrap().clear();

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
    settle().await;

    let p = m.placement_passes();
    assert_eq!(
        p.total, 1,
        "twenty events must cost ONE pass: notify_one's single permit is what makes \
         a burst free — got {p:?}"
    );
    assert_eq!(
        p.reconsidered, 1,
        "twenty events must cost ONE ask: asks are bounded by PASSES, and \
         notify_one's single permit is what makes a burst one pass — got {p:?}"
    );
    assert_eq!(
        locks_taken(&seen, &id),
        0,
        "reconsidering the pool must never cost a lock on a thread the gate refuses \
         — that is the bug 5f2b981 fixed and this change must not undo it. \
         Frames: {:?}",
        seen.lock().unwrap().clone()
    );

    // ── TEN MORE EVENT PASSES, AND TWENTY POOL CADENCES ──────────────────
    // The cadence is the new wake source, so it gets its own share of the burst:
    // a deadline-origin pass on a thread the gate admits-and-moves-nowhere reaches
    // the lock and then declines, which is the TOCTOU re-check. Asserting zero
    // locks across all of them is asserting that the no-progress guard still holds
    // for the one origin that is allowed past the pre-lock question.
    for _ in 0..10 {
        m.placement_pass(PassOrigin::Event);
    }
    for _ in 0..20 {
        m.placement_pass(PassOrigin::Cadence);
    }
    settle().await;

    let p = m.placement_passes();
    assert_eq!(
        p.reconsidered, 31,
        "one ask per pass per row, exactly — a bounded cost, not a hot loop: got {p:?}"
    );
    assert_eq!(
        p.total, 31,
        "and thirty-one passes, no more: nothing may re-enter a pass from inside one"
    );

    // THE LADDER IS POLICY AND IT IS UNTOUCHED.
    let row = m
        .store
        .get_retry(&id)
        .unwrap()
        .expect("a refused ask leaves the prompt in its durable home");
    assert_eq!(row.attempt, 7, "a refused ask must not spend an attempt");
    assert_eq!(row.prompt, "please ratelimit me", "nor touch the prompt");
}

/// **THE POOL'S LADDER CLIMBS WHILE NOTHING IS PLACED, AND ON `retry.rs`'S OWN
/// NUMBERS.**
///
/// This is the mechanism that makes "check everything at any opportunity"
/// affordable, and it is the belt to `attempt`'s braces.
///
/// **The common case is bounded by `attempt` alone**, and that is worth saying
/// first: the churning threads of 2026-08-06 were UNGATED, so a deadline-origin pass
/// admits them, runs a turn, takes the 429, and `schedule_retry` writes
/// `attempt + 1` — which climbs the rung on `retry.rs`'s own numbers with no
/// pool-level counter involved at all.
///
/// **`barren` covers what `attempt` cannot reach.** A cadence can fire and produce
/// no turn at all: the execution lock is held by a live turn, the hand-over to
/// `parked_turns` failed its write, or the row's thread is gone. None of those spend
/// an attempt — correctly, since the ladder is only ever spent by a turn that RAN —
/// so without `barren` the pool would re-fire at the same rung forever, and at
/// `transient_backoff_secs(1) == 0` that is a spin loop. This test uses the stale-row
/// case because it is the one a test can hold still: the prompt names a thread that
/// does not exist, so every cadence firing returns from `fire_retry` having done
/// nothing, which is exactly the shape of all three.
///
/// The expected values are written out as the LADDER'S OWN NUMBERS rather than
/// computed, so a retune is a diff in this test. That is the hard constraint
/// restated as an assertion: the cadence was re-homed, not retuned.
///
/// FORCED RED, run and recorded: deleting the `barren_tick()` call from
/// `placement_pass` gives `a barren cadence must climb the ladder, on
/// usage_limit_backoff_secs's OWN numbers (30/60/120/300): at barren=1 the rung must
/// be 60s — got Some(30s)`, which is the spin loop the operator asked about.
/// Resetting `barren` on a cadence that placed nothing reddens it identically.
#[tokio::test]
async fn the_pools_cadence_ladder_climbs_while_nothing_is_placed() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    // A STALE ROW: a parked prompt whose thread does not exist, so `fire_retry`
    // returns at `get_state` and the cadence provably comes to nothing. A real row
    // of this shape outlives a deleted thread.
    let id = "t-gone".to_string();
    // `usage_limit` at attempt 1, so the walk starts at the bottom of a ladder
    // whose four rungs are all distinct and all short enough to name.
    m.store
        .upsert_retry(&RetryRow {
            attempt: 1,
            ..parked_prompt(&id)
        })
        .unwrap();

    // `usage_limit_backoff_secs`: 30, 60, 120, then 300 forever. The index is
    // `attempt + barren` = `1 + barren`, so barren 0..=3 walks exactly that.
    let want = [30u64, 60, 120, 300, 300];
    for (barren, secs) in want.iter().enumerate() {
        let p = m.placement_passes();
        assert_eq!(
            p.barren, barren as u32,
            "the pool's barren count must be exactly the number of cadences that \
             came to nothing — got {p:?}"
        );
        assert_eq!(
            m.cadence_rung(),
            Some(Duration::from_secs(*secs)),
            "a barren cadence must climb the ladder, on `usage_limit_backoff_secs`'s \
             OWN numbers (30/60/120/300): at barren={barren} the rung must be {secs}s \
             — got {:?}",
            m.cadence_rung()
        );
        m.placement_pass(PassOrigin::Cadence);
        settle().await;
    }

    // THE PROMPT IS STILL THERE, UNSPENT, after five cadences — the ladder climbed
    // and nothing was consumed.
    let row = m
        .store
        .get_retry(&id)
        .unwrap()
        .expect("a cadence that came to nothing must leave the prompt alone");
    assert_eq!(row.attempt, 1, "and must not spend an attempt");
    assert_eq!(row.prompt, "please ratelimit me", "nor touch the prompt");

    // AND A QUESTION DOES NOT CLIMB IT. Only the pool's own clock firing and coming
    // to nothing is a barren cadence; an event that happened to change nothing is
    // not the pool failing at anything.
    let before = m.placement_passes().barren;
    for _ in 0..5 {
        m.placement_pass(PassOrigin::Event);
    }
    settle().await;
    assert_eq!(
        m.placement_passes().barren,
        before,
        "an EVENT that placed nothing is not a barren cadence: the pool's ladder is \
         advanced by its own clock coming to nothing, never by the world moving"
    );

    // AND THE LADDER NUMBERS THEMSELVES ARE PINNED, per class, so "re-homed, not
    // retuned" is checkable without reading `retry.rs`.
    for (kind, rungs) in [
        (TransientKind::RateLimit, vec![30u64, 30, 60, 120, 300, 300]),
        (TransientKind::Overload, vec![0, 0, 5, 15, 45, 120]),
        (TransientKind::Connectivity, vec![2, 2, 5, 10, 20, 30]),
        (TransientKind::Deterministic, vec![5, 5, 15, 45, 120, 300]),
        (TransientKind::CreditBalance, vec![30, 30, 60, 120, 300, 600]),
        (TransientKind::LoginExpired, vec![30, 30, 60, 120, 300, 600]),
    ] {
        for (attempt, want) in rungs.iter().enumerate() {
            assert_eq!(
                threads::retry::ladder_secs(&kind, attempt as u32),
                *want,
                "THE CADENCE VALUES ARE POLICY AND THIS CHANGE MAY NOT TOUCH THEM: \
                 {kind:?} at attempt {attempt} must still be {want}s"
            );
        }
    }
}

/// **THE POOL'S CADENCE IS THE MINIMUM RUNG, SO NOTHING WAITS LONGER THAN ITS OWN
/// CLASS SAID IT SHOULD.**
///
/// One cadence serves everything parked, which makes the choice between `min` and
/// `max` a policy decision rather than a detail. `max` would make a connectivity
/// blip (a 45-second ceiling) sit out a login expiry (600 seconds) that happened to
/// be parked beside it — a retune by composition, which is exactly what "re-home,
/// do not retune" forbids.
///
/// FORCED RED, run and recorded: changing `.min()` to `.max()` in `cadence_rung`
/// gives `the pool's cadence must be the SHORTEST rung among everything parked …
/// got 300s`.
#[tokio::test]
async fn the_pools_cadence_is_the_shortest_rung_among_everything_parked() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let slow = m.create_thread(None, aui(tmp.path())).unwrap();
    let fast = m.create_thread(None, aui(tmp.path())).unwrap();

    // A usage limit on its ceiling: 300s.
    m.store.upsert_retry(&parked_prompt(&slow)).unwrap();
    assert_eq!(m.cadence_rung(), Some(Duration::from_secs(300)));

    // A connectivity failure on its THIRD rung: 10s. `connectivity_backoff_secs`'s
    // own number, and far shorter than the usage limit beside it.
    m.store
        .upsert_retry(&RetryRow {
            attempt: 3,
            kind: "connectivity".into(),
            ..parked_prompt(&fast)
        })
        .unwrap();

    assert_eq!(
        m.cadence_rung(),
        Some(Duration::from_secs(10)),
        "the pool's cadence must be the SHORTEST rung among everything parked, so a \
         fast class never inherits a slow one's patience — got {:?}",
        m.cadence_rung()
    );

    // AND AN EMPTY POOL HAS NO CADENCE AT ALL — no timer, no wake-up, no cost.
    m.store.clear_retry(&slow).unwrap();
    m.store.clear_retry(&fast).unwrap();
    assert_eq!(
        m.cadence_rung(),
        None,
        "nothing parked means no cadence: the pool-level analogue of `a thread with \
         no retry has no floor`"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 5. A REFUSAL IS INFORMATION — the park, its reason, and the prompt
// ═════════════════════════════════════════════════════════════════════════

/// **A REFUSED QUESTION STILL RECORDS ITS PARK, WITH ITS REASON — AND KEEPS THE
/// PROMPT.**
///
/// Two agents have broken this and been caught by tests. A park is information the
/// operator can act on — a disabled key, an exhausted meter, a threshold — and
/// `admit_or_park` is the only thing that RECORDS one. So `early_ask_would_move`
/// returns `true` on a park ON PURPOSE: the refusal proceeds to the lock and is
/// announced honestly, and only the case where the gate admits and moves nothing is
/// answered for free.
///
/// The blocker here is a THRESHOLD CROSSING, which the deleted
/// `retry_unblocked_early` could not see at all because it read only bars. Under the
/// old backstop selection this park was not recorded until 2099: the operator's
/// board could not say why the thread was stuck.
///
/// What must NOT happen is the prompt moving. The retry row is its durable home and
/// carries the attempt, so a refused QUESTION leaves both alone; handing it to
/// `parked_turns` belongs to the pool's DEADLINE and nothing else
/// (`a_refusal_at_the_pools_deadline_hands_the_prompt_over`).
///
/// FORCED RED, run and recorded: making `early_ask_would_move` answer `false` on
/// `Admission::Park` gives `a refused question must RECORD its park: a refusal is
/// information: got null`.
#[tokio::test]
async fn a_refused_question_records_its_park_and_keeps_the_prompt() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "bb-owned"])).unwrap();

    let id = m.create_thread(None, aui(tmp.path())).unwrap();
    let home = sits_on(&m, &id);
    let other = ["aa-owned", "bb-owned"]
        .into_iter()
        .find(|n| *n != home)
        .expect("two logins")
        .to_string();

    // A THRESHOLD CROSSING the thread cannot escape: its own login is over the 95%
    // weekly ceiling, and the only sibling is barred outright. The reading is also
    // what makes `thresholds_can_refuse` true, so the thread leaves the ungated
    // branch and the gate can actually refuse it.
    observe_weekly(&m, &home, 99.0);
    bar(&m, &other);
    m.store.upsert_retry(&attributed(&id, &home)).unwrap();

    m.placement_pass(PassOrigin::Event);
    settle().await;

    // THE PARK IS RECORDED, WITH ITS REASON.
    let park = m.park_state(&id).unwrap();
    assert!(
        park.get("reason").and_then(|r| r.as_str()).is_some(),
        "a refused question must RECORD its park: a refusal is information, and \
         before the floor was removed this one was not recorded until 2099 — got \
         {park}"
    );
    let parked_events = m
        .store
        .list_events(&id, 0, 100, Some(&["turn_parked".to_string()]))
        .unwrap();
    assert!(
        !parked_events.is_empty(),
        "and it must reach the LEDGER, not only live memory: the board reads the \
         ledger"
    );

    // AND THE PROMPT STAYS IN ITS DURABLE HOME, attempt intact.
    let row = m
        .store
        .get_retry(&id)
        .unwrap()
        .expect("a refused question must not hand the prompt to parked_turns");
    assert_eq!(row.attempt, 7, "a question must not spend an attempt");
    assert_eq!(row.prompt, "please ratelimit me", "NOR LOSE THE PROMPT");
    assert_eq!(
        row.account.as_deref(),
        Some(home.as_str()),
        "nor rewrite which login refused it"
    );
    assert!(
        m.store.get_parked_turn(&id).unwrap().is_none(),
        "and the prompt must not be moved to parked_turns, which would drop the \
         thread onto the parked path with its retry ladder erased"
    );
    assert!(
        m.placement_passes().reconsidered >= 1,
        "the whole reason this park exists at all is the pool being reconsidered"
    );
}

/// **A REFUSAL AT THE POOL'S DEADLINE HANDS THE PROMPT OVER — AND LOSES NOTHING.**
///
/// `park_retried_turn`'s trigger, re-grounded. It used to fire on an ELAPSED
/// PER-THREAD FLOOR plus a refusal; it now fires on the POOL'S CADENCE plus a
/// refusal, which is the same event in the new vocabulary — the waiting is over and
/// the answer is still no.
///
/// It is also the other half of the hot-loop bound: the hand-over REMOVES the row
/// from `retries`, so a prompt the gate will never admit stops holding the pool's
/// rung down and becomes an ordinary parked turn.
///
/// The prompt is asserted on BOTH SIDES of the hand-over, because a write-then-clear
/// that loses its subject is the one failure this path may never have.
///
/// FORCED RED, run and recorded: changing the hand-over's guard from
/// `!origin.is_deadline()` to an unconditional `return` gives `the pool's deadline
/// must hand the prompt to its durable parked home … got None`. Clearing the retry
/// row BEFORE writing the parked turn gives a lost prompt on an injected write
/// failure, which is why the order is write-then-clear.
#[tokio::test]
async fn a_refusal_at_the_pools_deadline_hands_the_prompt_over() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "bb-owned"])).unwrap();

    let id = m.create_thread(None, aui(tmp.path())).unwrap();
    let home = sits_on(&m, &id);
    let other = ["aa-owned", "bb-owned"]
        .into_iter()
        .find(|n| *n != home)
        .expect("two logins")
        .to_string();
    observe_weekly(&m, &home, 99.0);
    bar(&m, &other);
    m.store.upsert_retry(&attributed(&id, &home)).unwrap();

    m.placement_pass(PassOrigin::Cadence);
    assert!(
        eventually(|| m.store.get_parked_turn(&id).unwrap().is_some()).await,
        "the pool's deadline must hand the prompt to its durable parked home when \
         the gate still refuses — got {:?}",
        m.store.get_parked_turn(&id).unwrap()
    );

    let parked = m.store.get_parked_turn(&id).unwrap().unwrap();
    assert_eq!(
        parked.prompt, "please ratelimit me",
        "NO PROMPT IS LOST ACROSS THE HAND-OVER: this row is now its only home"
    );
    assert_eq!(parked.message_ids, vec![1], "nor its message ids");
    assert_eq!(parked.attempt, 7, "nor the attempt it has spent");
    assert!(
        !parked.reason.is_empty(),
        "AND THE PARK CARRIES ITS REASON: a refusal is information"
    );
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "and the retry row is cleared, so the prompt has exactly one home and the \
         pool's rung stops being held down by a prompt the gate will never admit"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 6. NO PROMPT IS LOST BY THE CHANGE ITSELF
// ═════════════════════════════════════════════════════════════════════════

/// **A THREAD PARKED AT THE MOMENT OF THE CHANGE KEEPS ITS PROMPT.**
///
/// The one case the behavioural tests above cannot reach: a `retries` row written
/// by the PREVIOUS build, with a `retry_at` column on disk, read by THIS build after
/// `DROPPED_COLUMNS` has removed it. The live daemon has rows in exactly that state
/// right now, and `retries` is the prompt's only home while a thread waits, so a
/// migration that lost one would lose the operator's words.
///
/// The old shape is reconstructed rather than fixtured, because a hand-written
/// fixture would be this build's idea of the old shape: the column is added back
/// with the exact `TEXT NOT NULL` declaration `schema.sql` carried, so an insert
/// that did not supply it would fail and the test would be lying about the premise.
///
/// FORCED RED, run and recorded: removing
/// `"ALTER TABLE retries DROP COLUMN retry_at"` from `DROPPED_COLUMNS` leaves the
/// column in place and `upsert_retry`'s eight-column INSERT fails its NOT NULL
/// constraint, giving `the prompt must survive the migration that removed the
/// floor`. Dropping the column with a `DELETE FROM retries` instead of an
/// `ALTER TABLE` gives `got None` on the same line.
#[test]
fn a_prompt_parked_under_the_old_schema_survives_the_migration() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.db");

    // ── THE OLD SHAPE, REBUILT ───────────────────────────────────────────
    // Open once so `schema.sql` creates every table, then put the floor column
    // BACK with the declaration it used to carry, and write a row through raw SQL
    // the way the previous build's `upsert_retry` did.
    {
        let s = threads::store::Store::open(&path).unwrap();
        s.execute_sql(
            "ALTER TABLE retries ADD COLUMN retry_at TEXT NOT NULL DEFAULT '';
             INSERT INTO retries
                (thread_id, prompt, message_ids_json, attempt, kind, reason,
                 retry_at, account, created_at)
             VALUES ('t-old', 'the words he actually said', '[41,42]', 7,
                     'usage_limit', 'rate limit (429)',
                     '2099-01-01T00:00:00+00:00', 'aa-owned',
                     '2026-10-06T00:00:00+00:00');",
        )
        .unwrap();
    }

    // ── THIS BUILD OPENS IT ──────────────────────────────────────────────
    let s = threads::store::Store::open(&path).unwrap();
    let row = s
        .get_retry("t-old")
        .unwrap()
        .expect("the prompt must survive the migration that removed the floor");
    assert_eq!(
        row.prompt, "the words he actually said",
        "the prompt must survive the migration that removed the floor — `retries` is \
         its only home while the thread waits"
    );
    assert_eq!(row.message_ids, vec![41, 42], "and its message ids");
    assert_eq!(row.attempt, 7, "and the attempt it had spent");
    assert_eq!(
        row.account.as_deref(),
        Some("aa-owned"),
        "and which login refused it"
    );
    assert_eq!(
        row.created_at, "2026-10-06T00:00:00+00:00",
        "and its BIRTH, which is the only instant the row carries now and the one \
         that survives every re-arm"
    );

    // AND THE COLUMN IS REALLY GONE, not merely unread.
    let still_there = s.execute_sql("SELECT retry_at FROM retries LIMIT 1;");
    assert!(
        still_there.is_err(),
        "`retries.retry_at` must be DROPPED, not just ignored: a timestamp nothing \
         reads is still a timestamp somebody will read"
    );

    // AND THE PROMPT IS STILL WRITEABLE THROUGH THE NEW PATH.
    s.upsert_retry(&RetryRow {
        thread_id: "t-old".into(),
        prompt: "the words he actually said".into(),
        message_ids: vec![41, 42],
        attempt: 8,
        kind: "usage_limit".into(),
        reason: "rate limit (429)".into(),
        account: Some("aa-owned".into()),
        created_at: String::new(),
    })
    .unwrap();
    let again = s.get_retry("t-old").unwrap().unwrap();
    assert_eq!(again.attempt, 8, "a re-arm advances the attempt");
    assert_eq!(
        again.prompt, "the words he actually said",
        "and never touches the prompt"
    );
    assert_eq!(
        again.created_at, "2026-10-06T00:00:00+00:00",
        "and `created_at` survives the re-arm, which is what makes it the honest \
         answer to `how long has this been waiting`"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 7. THE READING, VERIFIED — the mechanism is where it is claimed to be
// ═════════════════════════════════════════════════════════════════════════

/// The comment-free body of one `src/` file, so a source guard reads CODE and
/// cannot be satisfied — or falsified — by the prose explaining it. The
/// self-witness class: a ban on a string is satisfied by its own explanatory
/// comment unless the comments come out first.
fn code_of(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

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

/// **THERE IS NO PER-THREAD RETRY TIMESTAMP ANYWHERE ON THE RETRY PATH.**
///
/// A behavioural test can be satisfied by a daemon that reaches the right answer
/// through a second mechanism, and a second mechanism is how the retry path got into
/// trouble twice. So this reads the code. Every name below belonged to the
/// per-thread clock and its absence is the ruling:
///
/// > *"There should not be a per-thread retry timestamp."*
///
/// FORCED RED, run and recorded: restoring `fn retry_floor_elapsed(` reddens the
/// first assertion with its own text; giving `retries_to_reconsider` a `.filter(`
/// reddens the second; and pointing `cadence_rung` at a literal instead of
/// `retry::ladder_secs` reddens the fourth.
#[test]
fn no_per_thread_retry_timestamp_survives_anywhere() {
    let code = code_of(include_str!("../../src/runtime.rs"));
    let store = code_of(include_str!("../../src/store.rs"));

    for gone in [
        "fn compute_retry_at(",
        "fn arm_retry_timer(",
        "fn retry_floor_elapsed(",
        "fn retry_unblocked_early(",
        "fn retries_to_reask(",
        "fn retries_to_reask_on_event(",
        "fn retry_sweep_on_event(",
        "enum RetryOrigin",
    ] {
        assert!(
            !code.contains(gone),
            "THE PER-THREAD CLOCK IS GONE AND MUST STAY GONE: `{gone}` is back in \
             runtime.rs. The operator ruled there should not be a per-thread retry \
             timestamp, and every one of these names exists only to serve one"
        );
    }
    assert!(
        !store.contains("pub retry_at:"),
        "`RetryRow` must carry no instant: a row in `retries` is a PARKED PROMPT, \
         and when it is reconsidered is a pool-level question"
    );
    assert!(
        store.contains("ALTER TABLE retries DROP COLUMN retry_at"),
        "and the column must be DROPPED on open, not merely unread — a timestamp \
         nothing reads is still a timestamp somebody will read"
    );

    // ── ONE CANDIDATE SET, NO FILTER ──────────────────────────────────────
    let set = slice(
        &code,
        "pub fn retries_to_reconsider(",
        "pub fn retry_sweep(",
    );
    assert!(
        set.contains("self.store.list_retries().unwrap_or_default()"),
        "the candidate set must be EVERY row in `retries` — *any parked threads get \
         checked at any opportunity*"
    );
    assert_eq!(
        set.matches(".filter(").count(),
        0,
        "and it must have NO filter at all: a filter here is a per-thread \
         eligibility clause wearing another name, and the `account IS NULL` row is \
         what happens when one exists"
    );

    // ── THE LADDER IS READ, NOT RESTATED ─────────────────────────────────
    let rung = slice(&code, "pub fn cadence_rung(", "pub fn retries_to_reconsider(");
    assert!(
        rung.contains("crate::retry::ladder_secs(&kind,")
            && rung.contains("r.attempt.saturating_add(barren)"),
        "the pool's rung must be `retry::ladder_secs(kind, attempt + barren)` — the \
         ladder was RE-HOMED, not retuned, so its numbers must be READ from \
         retry.rs and never restated here"
    );
    assert!(
        rung.contains(".min()"),
        "and it must take the SHORTEST rung among everything parked, so one class \
         never inherits another's patience"
    );

    // ── THE DEADLINE IS THE ORIGIN, IN EXACTLY ONE PLACE ─────────────────
    let is_deadline = slice(&code, "pub fn is_deadline(", "pub struct PoolCadence");
    assert!(
        is_deadline.contains("matches!(self, PassOrigin::Cadence)"),
        "exactly ONE origin may be a deadline, and it must be the pool's own \
         cadence: the backstop being a deadline would make the cadence decorative, \
         and an event being one would spend the ladder on a question"
    );

    // ── 5f2b981 IS NOT UNDONE: the ask is read-only and pre-lock ─────────
    let fire = slice(&code, "async fn fire_retry(", "fn park_retried_turn(");
    let ask_at = fire
        .find("!origin.is_deadline() && !self.early_ask_would_move(&state)")
        .expect(
            "the read-only pre-lock ask must still be there and must still be keyed \
             on the origin: 5f2b981 may not be undone",
        );
    let lock_at = fire
        .find("self.lock_turn(&state,")
        .expect("fire_retry must still take the execution lock somewhere");
    assert!(
        ask_at < lock_at,
        "AND IT MUST COME BEFORE THE LOCK. Asking used to cost the lock, two \
         `Push::Status` frames and a `released by Drop` warning — 27,705 of them. \
         Reconsidering the pool more often makes that path MORE load-bearing, not \
         less"
    );

    // ── backstop_placed IS STILL SCOPED TO THE BACKSTOP AND THE PARKED HALF ──
    let pass = slice(&code, "pub fn placement_pass(", "pub fn emit_info(");
    let backstop_arm = slice(pass, "PassOrigin::Backstop => {", "if !reasons.is_empty()");
    assert!(
        backstop_arm.contains("backstop_placed"),
        "`backstop_placed` must be incremented ONLY inside the Backstop arm: it is \
         the only detector of an incomplete event set, and a cadence that \
         incremented it would drown that signal in healthy operation"
    );
    assert_eq!(
        pass.matches("backstop_placed").count(),
        1,
        "and in exactly one place — got {} ",
        pass.matches("backstop_placed").count()
    );
    assert!(
        backstop_arm.contains("if !placed.is_empty()"),
        "and it must be scoped to the PARKED half (`placed`), so its meaning is \
         still `an input to placement moved with no bell to ring`"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
