//! **A QUESTION IS NOT A TURN.**
//!
//! Since 2026-08-28 the operator's daemon logged this 27,705 times, on exactly
//! three thread ids, every five seconds:
//!
//! > `WARN thread lock released by Drop — a turn path exited without releasing`
//!
//! The chain: `retry_sweep` re-asks placement for every parked prompt, and most
//! of those asks are QUESTIONS rather than deadlines. That ask is correct and this
//! file does not take it away. What was wrong is that `fire_retry` took the
//! EXECUTION LOCK
//! merely to ask the question: `lock_turn` emits `Push::Status { executing: true }`,
//! the no-progress guard then returned without running anything, and the
//! `TurnLock` went out of scope — warning, `release()`, and a second
//! `Push::Status { executing: false }`.
//!
//! Two frames per thread per tick, three threads, every five seconds, forever.
//! The `accounts` shape folds them honestly and the board redraws on each, so
//! the operator saw `running 1→2→3` then `3→2→1→0` then a four-second plateau,
//! on a loop. It also corrupted the gauge: `in_flight_count` reads
//! `ThreadState::turn_running`, which `lock_turn` had just set, so the daemon
//! logged `account usage … pct=42 threads=3` counting three phantom locks as
//! running turns.
//!
//! # Re-grounded 2026-10-06: the question/deadline line moved off the clock
//!
//! Every test here used to set a per-thread `retries.retry_at` — tomorrow for a
//! question, the year 2000 for a deadline — because `fire_retry` read
//! `retry_floor_elapsed(&row)` to tell them apart. The operator removed that
//! timestamp (*"There should not be a per-thread retry timestamp"*), so the
//! distinction is now the ORIGIN of the pass: [`PassOrigin::Event`] and
//! [`PassOrigin::Backstop`] ask questions, [`PassOrigin::Cadence`] — the pool's own
//! backoff — is the one deadline.
//!
//! **`5f2b981` is not undone by that; it is widened.** The free, lock-free ask used
//! to be scoped to rows whose floor had not elapsed. It now covers EVERY ask that
//! is not the pool's deadline, which is strictly more of them — and it has to,
//! because the pool now reconsiders everything at every opportunity.
//!
//! The tests below are written as FRAME COUNTS, not as "no error". The central
//! one asserts ZERO `Push::Status` frames for a question-origin ask; restoring
//! the lock-first order makes it count six again, which is how it was forced red.
//!
//! Every test drives the manager in-process. `build_manager` does NOT call
//! `start_watchers`, so the only thing that sweeps is this file's own
//! `retry_sweep(..)` call and the frame counts are deterministic. No real claude
//! turn is ever spawned: `THREADS_CLAUDE_BIN` is the fake fixture and
//! `THREADS_CLAUDE_HOME` points transcript storage at a scratch dir, so nothing
//! here can read or write inside the operator's own `~/.claude`, the running
//! daemon, the live `threads.db`, or the keychain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::event::Push;
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

/// An AUI thread: `Auto` account, no budget key, no thresholds — so
/// `admit_or_park` runs the gate purely for PLACEMENT and can never refuse.
/// This is the shape all three churning threads had.
fn aui_settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

/// **A PARKED PROMPT, AND IT CARRIES NO INSTANT AT ALL.**
///
/// There used to be two of these — one with a `retry_at` of 2099 (a question) and
/// one with 2000 (a deadline). There is one now, because the row cannot express
/// the difference any more and must not: *"a parked thread is simply parked."*
/// Which route an ask takes is the pass's [`PassOrigin`], passed at the call site
/// of each test instead of smuggled in through the fixture.
///
/// `attempt: 7` is load-bearing in every test below: a refused ask must leave it
/// at 7, because the ladder is only ever spent by a turn that RAN.
fn retry_row(thread_id: &str, account: &str) -> RetryRow {
    RetryRow {
        thread_id: thread_id.into(),
        prompt: "please ratelimit me".into(),
        message_ids: vec![1],
        attempt: 7,
        kind: "usage_limit".into(),
        reason: "rate limit (429)".into(),
        account: Some(account.to_string()),
        created_at: String::new(),
    }
}

/// One observed `Push::Status`, reduced to the three values the board folds.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    thread_id: String,
    state: String,
    executing: bool,
}

/// Collect every `Push::Status` the daemon emits. Status frames are the whole
/// subject here, so this deliberately keeps them ALL rather than filtering to a
/// kind — a frame for the wrong thread is evidence too.
fn watch_status(m: &ThreadManager) -> Arc<Mutex<Vec<Frame>>> {
    let mut rx = m.subscribe();
    let seen = Arc::new(Mutex::new(Vec::<Frame>::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Status {
                thread_id,
                state,
                executing,
                ..
            } = p
            {
                out.lock().unwrap().push(Frame {
                    thread_id,
                    state,
                    executing,
                });
            }
        }
    });
    seen
}

fn frames_for(seen: &Arc<Mutex<Vec<Frame>>>, id: &str) -> Vec<Frame> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|f| f.thread_id == id)
        .cloned()
        .collect()
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

/// Sweep `n` times at one origin and let every spawned re-ask settle. The
/// assertions that use this are all "exactly this many frames", and waiting LONGER
/// only ever strengthens a zero — so a slow machine cannot turn this green.
async fn sweep_n(m: &ThreadManager, origin: PassOrigin, n: usize) {
    for _ in 0..n {
        m.retry_sweep(origin);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    tokio::time::sleep(Duration::from_millis(700)).await;
}

// ═════════════════════════════════════════════════════════════════════════
// 1. THE CENTRAL TEST — a not-ready early ask costs NOTHING
// ═════════════════════════════════════════════════════════════════════════

/// **A QUESTION-ORIGIN ask emits ZERO status frames and takes ZERO locks.**
///
/// This is the whole bug, as a number. Before the fix each sweep produced
/// exactly two frames for this thread — `executing: true` from `lock_turn` and
/// `executing: false` from the `TurnLock`'s `Drop` — so three sweeps produced
/// SIX, which is the `1→2→3 … 3→2→1→0` the operator was watching. After it,
/// three sweeps produce none at all.
///
/// The `retries_to_reconsider` assertion is what stops this test being a
/// decoration. A row the sweep never selects would make every frame count below
/// trivially zero, and the test would pass against a daemon in which the bug was
/// fully intact. It is asserted as a VALUE (`1`) rather than as `is_empty()` so
/// that a fixture which stops reaching `fire_retry` fails loudly instead of
/// silently proving nothing.
///
/// FORCED RED, run and recorded: changing `fire_retry`'s guard from
/// `!origin.is_deadline()` to `origin.is_deadline()` — i.e. letting a question take
/// the lock — fails with `ASKING IS NOT EXECUTING: three question-origin passes …
/// must emit no Push::Status at all. Got 6 frame(s)`.
#[tokio::test]
async fn a_question_origin_ask_emits_no_status_frames_and_takes_no_lock() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // TWO placeable logins, so the world is a plausible one for an early ask —
    // nothing here needs a free sibling to be SELECTED any more (the pool asks
    // about everything), only to make the scenario realistic.
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "bb-owned"])).unwrap();
    let id = m.create_thread(None, aui_settings(tmp.path())).unwrap();
    let sits_on = m
        .store
        .get_thread(&id)
        .unwrap()
        .expect("the thread row must exist")
        .account
        .expect("an AUI thread is auto-placed at create");

    // Neither login is barred, so placement leaves a settled thread exactly
    // where it is — the early ask CANNOT move this thread, and that is the
    // common case that was costing two frames a tick.
    m.store.upsert_retry(&retry_row(&id, &sits_on)).unwrap();

    assert_eq!(
        m.retries_to_reconsider()
            .iter()
            .filter(|r| r.thread_id == id)
            .count(),
        1,
        "NON-VACUITY: the pool must actually select this row and reach `fire_retry`. \
         If this is 0 the frame counts below prove nothing at all."
    );

    let seen = watch_status(&m);
    seen.lock().unwrap().clear();

    sweep_n(&m, PassOrigin::Event, 3).await;

    let mine = frames_for(&seen, &id);
    assert_eq!(
        mine.len(),
        0,
        "ASKING IS NOT EXECUTING: three question-origin passes over a parked prompt \
         must emit no `Push::Status` at all. Got {} frame(s): {mine:?}",
        mine.len()
    );
    assert_eq!(
        mine.iter().filter(|f| f.executing).count(),
        0,
        "no frame may ever announce this thread as executing — got {mine:?}"
    );

    // AND THE PROMPT IS KEPT, UNSPENT. The retry row is the prompt's durable home;
    // a refused question must leave it and its attempt exactly where they were.
    let row = m
        .store
        .get_retry(&id)
        .unwrap()
        .expect("the refused question must leave the parked prompt in place");
    assert_eq!(row.attempt, 7, "a question must not spend an attempt");
    assert_eq!(
        row.prompt, "please ratelimit me",
        "NOR MAY IT LOSE THE PROMPT — this row is the prompt's only home while the \
         thread waits"
    );
    assert_eq!(
        row.account.as_deref(),
        Some(sits_on.as_str()),
        "nor rewrite which login refused it"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 2. THE GAUGE — a question is not a running turn
// ═════════════════════════════════════════════════════════════════════════

/// **The gauge no longer counts a question as a running turn.**
///
/// `in_flight_count` — the `threads=` field on the daemon's `account usage` line
/// — counts a thread when `ThreadState::turn_running` is set, and `lock_turn`'s
/// Gate 1 sets exactly that. `thread_status`'s `executing` is derived from the
/// same bit, so an `executing: true` frame is the one externally visible proof
/// that the bit was set, and its absence is the proof that it never was. That is
/// why this asserts on the frame's VALUE rather than sampling a counter after
/// the fact: the phantom lock lived for microseconds inside a spawned task, so a
/// post-hoc sample reads zero whether the bug is present or not, and would be a
/// decoration.
#[tokio::test]
async fn a_swept_question_never_sets_the_bit_the_gauge_counts() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "bb-owned"])).unwrap();
    let id = m.create_thread(None, aui_settings(tmp.path())).unwrap();
    let sits_on = m
        .store
        .get_thread(&id)
        .unwrap()
        .unwrap()
        .account
        .expect("auto-placed");
    m.store.upsert_retry(&retry_row(&id, &sits_on)).unwrap();

    assert_eq!(
        m.retries_to_reconsider()
            .iter()
            .filter(|r| r.thread_id == id)
            .count(),
        1,
        "NON-VACUITY: the pool must select this row"
    );

    let seen = watch_status(&m);
    seen.lock().unwrap().clear();
    sweep_n(&m, PassOrigin::Event, 3).await;

    let executing: Vec<Frame> = frames_for(&seen, &id)
        .into_iter()
        .filter(|f| f.executing)
        .collect();
    assert_eq!(
        executing.len(),
        0,
        "the gauge counts `turn_running`; an asked-and-refused retry must never set it. \
         Got {} frame(s) announcing a phantom turn: {executing:?}",
        executing.len()
    );
    assert!(
        !m.is_locked(&id),
        "and no lock row may be left behind for the gauge to find"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 3. THE EARLY ASK STILL WORKS — the intent survives
// ═════════════════════════════════════════════════════════════════════════

/// **A question that CAN move the thread still runs it.**
///
/// The regression guard on the fix, and the test that OPPOSES the two above: a
/// change that answers every question with "no" would stop the churn by deleting
/// the feature, and would redden exactly this. Here the login that refused is
/// barred and a free sibling exists, so placement really does have somewhere to
/// put this thread: the ask must reach the lock, announce a turn, and run.
///
/// Forced red by making the pre-lock probe answer `false` unconditionally: the
/// turn then never starts and this asserts on the absence of the very frames the
/// test above asserts must not exist, so the two cannot both be satisfied by a
/// daemon that simply does nothing.
#[tokio::test]
async fn an_early_ask_that_can_move_the_thread_still_runs_it() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "bb-owned"])).unwrap();
    let id = m.create_thread(None, aui_settings(tmp.path())).unwrap();
    let sits_on = m
        .store
        .get_thread(&id)
        .unwrap()
        .unwrap()
        .account
        .expect("auto-placed");

    // BAR THE LOGIN THAT REFUSED, and leave the sibling free. Now placement has a
    // reason and a destination, so the question is answerable.
    //
    // The instant below is the BAR's, which is the one place an instant still
    // legitimately lives; a parked prompt carries none.
    const BAR_UNTIL: &str = "2099-01-01T00:00:00+00:00";
    m.store
        .set_account_limit(
            &sits_on,
            Some((BAR_UNTIL, Some("resets tomorrow"), Some("weekly_fable"), None)),
        )
        .unwrap();
    m.store.upsert_retry(&retry_row(&id, &sits_on)).unwrap();

    assert_eq!(
        m.retries_to_reconsider()
            .iter()
            .filter(|r| r.thread_id == id)
            .count(),
        1,
        "NON-VACUITY: the pool must select this row"
    );

    let seen = watch_status(&m);
    seen.lock().unwrap().clear();

    m.retry_sweep(PassOrigin::Event);

    assert!(
        eventually(|| frames_for(&seen, &id).iter().any(|f| f.executing)).await,
        "a question with somewhere to go MUST still announce and run a turn — \
         got {:?}",
        frames_for(&seen, &id)
    );
}

/// **THE POOL'S OWN CADENCE IS A DEADLINE, AND IT RUNS ON THE SAME ACCOUNT.**
///
/// The re-grounded form of `a_retry_whose_floor_has_elapsed_still_runs`. Nothing is
/// barred and no sibling is free, so placement leaves this thread exactly where it
/// is — the case every question-origin pass above declines for free. A
/// [`PassOrigin::Cadence`] pass must take the lock and run it anyway, because the
/// pool's backoff is what waited and a usage-limit window that simply expires has
/// no other outcome.
///
/// **This is the test that opposes the hot-loop bound.** A change that applied the
/// no-progress guard to EVERY origin — the obvious over-reach when removing the
/// floor — would make the pool reconsider forever and never run anything, and this
/// is what reddens.
///
/// FORCED RED, run and recorded: dropping the `!origin.is_deadline()` scope from
/// `fire_retry`'s no-progress guard fails with `the pool's own cadence is a
/// DEADLINE … must take the lock and run — got []`.
#[tokio::test]
async fn the_pools_cadence_is_a_deadline_and_runs_on_the_same_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "bb-owned"])).unwrap();
    let id = m.create_thread(None, aui_settings(tmp.path())).unwrap();
    let sits_on = m
        .store
        .get_thread(&id)
        .unwrap()
        .unwrap()
        .account
        .expect("auto-placed");

    m.store.upsert_retry(&retry_row(&id, &sits_on)).unwrap();

    // NON-VACUITY, THE OTHER WAY ROUND: prove the gate would move nothing, so that
    // a question-origin pass genuinely WOULD have declined this and the only thing
    // separating the two outcomes is the origin.
    let seen = watch_status(&m);
    seen.lock().unwrap().clear();
    sweep_n(&m, PassOrigin::Event, 1).await;
    assert_eq!(
        frames_for(&seen, &id).len(),
        0,
        "the control must be a thread a QUESTION declines for free, or this proves \
         nothing about the deadline: got {:?}",
        frames_for(&seen, &id)
    );

    seen.lock().unwrap().clear();
    m.retry_sweep(PassOrigin::Cadence);

    assert!(
        eventually(|| frames_for(&seen, &id).iter().any(|f| f.executing)).await,
        "the pool's own cadence is a DEADLINE, not a question: it must take the lock \
         and run — got {:?}",
        frames_for(&seen, &id)
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
