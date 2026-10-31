//! R10.M2c (TC-45, L-8): a turn never waits on its opening `/usage` pair.
//!
//! On 2026-10-27 (an internal thread, an internal report) prod's
//! `/usage` probes took 20-60 s, and every cold-gap dispatch held its attempt
//! open for 53-62 s before `turn_started`, awaiting the opening edge pair. To
//! the operator the thread was uninterruptible and his message was not with
//! the model. The pair is now waited on for at most `THREADS_EDGE_OPEN_BOUND_MS`
//! (1.5 s here, 5 s in prod) and a cancel ends the wait at once.
//!
//! The probe latency is injected per account: the fake claude sleeps the
//! seconds in the config dir's `usage-sleep` before answering `/usage`.
//!
//! - slow probe (8 s, outlasted): `turn_started` follows `delivery_attempted`
//!   inside the bound, and the abandoned reading is never recorded late;
//! - fast probe (the opposing case): the pair lands and the window calibrates
//!   exactly as before;
//! - cancel during a slow probe: the thread is free well inside the bound,
//!   and his next send starts a turn carrying the rider.

#[path = "support/keychain.rs"]
mod keychain;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::json;
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadEvent, ThreadManager};

const PROBE_SLEEP_SECS: u64 = 8;
const BOUND_MS: u64 = 1_500;

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
    std::env::set_var("THREADS_EDGE_OPEN_BOUND_MS", BOUND_MS.to_string());
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// One extra account, `probe`, whose `/usage` walks `seq` and, when
/// `sleep_secs` is set, answers only after that many seconds. Calibration is on
/// and the account has no estimate, so `edge_poll_wanted` is true and every
/// dispatch on a cold gap takes the opening pair.
fn probing_manager(dir: &std::path::Path, seq: &str, sleep_secs: Option<u64>, ladder: Vec<f64>) -> ThreadManager {
    let acct = dir.join("probe");
    std::fs::create_dir_all(&acct).unwrap();
    std::fs::write(acct.join("usage-seq"), seq).unwrap();
    if let Some(s) = sleep_secs {
        std::fs::write(acct.join("usage-sleep"), s.to_string()).unwrap();
    }
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert("probe".to_string(), acct.to_string_lossy().to_string());
    keychain::install();
    build_manager(Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        probe_interval_secs: 0,
        calibrate: true,
        edge_poll: true,
        settle_ladder_secs: ladder,
        ..Default::default()
    })
    .unwrap()
}

fn probing_thread(m: &ThreadManager, cwd: &std::path::Path) -> String {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s.account = threads::settings::AccountChoice::Named("probe".to_string());
    let id = m.create_thread(None, s).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "probe");
    id
}

fn events(m: &ThreadManager, id: &str) -> Vec<ThreadEvent> {
    m.store.tail_events(id, 5000).unwrap().into_iter().map(|e| e.event).collect()
}

fn attempted(m: &ThreadManager, id: &str) -> usize {
    events(m, id)
        .iter()
        .filter(|e| matches!(e, ThreadEvent::Custom { event_type, .. } if event_type == "cauldron.threads/delivery_attempted"))
        .count()
}

fn turns(m: &ThreadManager, id: &str) -> Vec<Vec<u64>> {
    events(m, id)
        .into_iter()
        .filter_map(|e| match e {
            ThreadEvent::TurnStarted { message_ids, .. } => Some(message_ids),
            _ => None,
        })
        .collect()
}

/// Poll `f` every 25 ms until it holds or `limit` passes.
async fn within(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

fn send(m: &ThreadManager, id: &str, text: &str) -> u64 {
    m.send(id, text.to_string(), threads::SendIntent::Immediate, json!({}), false).unwrap()
}

/// The 8 s probe stands in for prod's 20-60 s. The turn must start within the
/// bound plus slack (4 s), well before one probe could answer. After that the
/// test waits past the probe's 8 s and asserts the abandoned reading was never
/// recorded: a reading landing inside the window would be a false `before`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turn_starts_within_bound_while_the_opening_usage_probe_sleeps_8s() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = probing_manager(tmp.path(), "41", Some(PROBE_SLEEP_SECS), vec![]);
    let id = probing_thread(&m, tmp.path());

    let mid = send(&m, &id, "hello");
    assert!(within(Duration::from_secs(5), || attempted(&m, &id) == 1).await, "the send opened an attempt");
    let opened = Instant::now();
    let started = within(Duration::from_secs(4), || !turns(&m, &id).is_empty()).await;
    assert!(
        started,
        "turn_started must follow delivery_attempted within the {BOUND_MS} ms bound (+slack), not wait \
         on a {PROBE_SLEEP_SECS} s /usage probe; waited {:?}, turns {:?}",
        opened.elapsed(),
        turns(&m, &id)
    );
    assert_eq!(turns(&m, &id), vec![vec![mid]]);

    // Outlast the probe: the abandoned pair records nothing, ever.
    tokio::time::sleep(Duration::from_secs(PROBE_SLEEP_SECS + 2)).await;
    assert_eq!(
        m.budgets.last_observed_at("probe"),
        None,
        "an opening reading abandoned at the bound must never be recorded late, inside the window"
    );
}

/// The opposing case: probes answer at once, so the pair lands inside the bound
/// and the window it brackets calibrates exactly as before R10.M2c.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fast_opening_probes_still_bracket_and_calibrate_the_window() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = probing_manager(tmp.path(), "41,41,45,45,45", None, vec![0.04, 0.12, 0.24]);
    let id = probing_thread(&m, tmp.path());
    let mid = send(&m, &id, "hello");
    assert!(within(Duration::from_secs(10), || turns(&m, &id) == vec![vec![mid]]).await);
    assert!(
        within(Duration::from_secs(10), || m.store.get_estimate("probe", "session").unwrap().is_some()).await,
        "a fast opening pair must still bracket the window: {:#}",
        m.budgets.estimation_state().unwrap()
    );
    let est = m.store.get_estimate("probe", "session").unwrap().unwrap();
    assert_eq!(est.sample_count, 1, "{est:?}");
}

/// A cancel during a slow opening probe frees the thread at once. The 500 ms
/// limit is under the 1.5 s bound, so only the cancel arm can meet it, not the
/// bound. His next send then starts a turn carrying the rider (L-8).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_during_an_8s_opening_probe_frees_the_thread_inside_the_bound() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = probing_manager(tmp.path(), "41", Some(PROBE_SLEEP_SECS), vec![]);
    let id = probing_thread(&m, tmp.path());

    let first = send(&m, &id, "first");
    assert!(within(Duration::from_secs(5), || attempted(&m, &id) == 1).await);
    assert!(!m.is_idle(&id).unwrap(), "sentinel: the attempt holds the thread before the cancel");
    m.cancel(&id).unwrap();
    let cancelled = Instant::now();
    let freed = within(Duration::from_millis(500), || m.is_idle(&id).unwrap()).await;
    assert!(
        freed,
        "the cancel must end the wait on the opening probe at once (freed after {:?}; bound {BOUND_MS} ms)",
        cancelled.elapsed()
    );
    assert!(turns(&m, &id).is_empty(), "the cancelled attempt started no turn");

    let second = send(&m, &id, "second");
    assert!(
        within(Duration::from_secs(4), || !turns(&m, &id).is_empty()).await,
        "his next send starts a turn inside the bound"
    );
    assert_eq!(turns(&m, &id), vec![vec![first, second]], "the rider goes first");
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;

/// **THE OPERATOR'S RULING (TC-45, r46): AFTER AN INTERRUPT, HELD MESSAGES
/// RIDE THE NEXT TURN THE OPERATOR STARTS AND NEVER START ONE ON THEIR OWN.**
///
/// Three messages are held when he interrupts: one in an OPEN attempt (its
/// opening `/usage` probe sleeps 3 s, so the attempt is open at the cancel)
/// and two queued behind it. After the cancel the thread must stay idle, all
/// three must stay pending (`ride`, undelivered, kept) and no turn may start
/// for 6 s — longer than every period in the path, each outlasted:
/// - the backstop placement poll, `THREADS_RESUME_TICK_MS` = 150 ms (~40 passes);
/// - the opening-pair bound, `THREADS_EDGE_OPEN_BOUND_MS` = 1.5 s;
/// - the abandoned probe itself, 3 s (its blocking `claude -p /usage` child
///   runs to the end even though the pair task was aborted);
/// - the pair gap, `THREADS_EDGE_PAIR_GAP_MS` = 10 ms.
/// No retry ladder or parked turn exists (sentinels below), so no retry timer
/// or pool cadence is armed. Opposed by his genuine next send, which starts a
/// turn carrying the three riders first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_with_held_messages_stays_idle_and_pending_past_backstop_bound_and_probe_6s() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = probing_manager(tmp.path(), "41", Some(3), vec![]);
    let id = probing_thread(&m, tmp.path());

    let a = send(&m, &id, "first");
    assert!(within(Duration::from_secs(5), || attempted(&m, &id) == 1).await);
    let b = send(&m, &id, "second");
    let c = send(&m, &id, "third");
    assert!(!m.is_idle(&id).unwrap(), "sentinel: the attempt holds the thread before the cancel");
    m.cancel(&id).unwrap();
    assert!(within(Duration::from_secs(1), || m.is_idle(&id).unwrap()).await, "the cancel frees the thread");

    let rows = || -> Vec<(u64, String)> {
        m.store.waiting_queue(&id).unwrap().into_iter().map(|r| (r.message_id, r.dispatch)).collect()
    };
    let ride = threads::event::delivery::RIDE.to_string();
    assert_eq!(rows(), vec![(a, ride.clone()), (b, ride.clone()), (c, ride.clone())], "all three held, ride");
    assert!(m.store.get_retry(&id).unwrap().is_none(), "sentinel: no retry ladder armed");
    assert!(m.store.get_parked_turn(&id).unwrap().is_none(), "sentinel: no parked turn");

    // The negative window: 6 s outlasts the backstop (150 ms), the bound
    // (1.5 s) and the abandoned probe (3 s). Checked every 25 ms.
    let end = Instant::now() + Duration::from_secs(6);
    while Instant::now() < end {
        assert!(
            turns(&m, &id).is_empty() && attempted(&m, &id) == 1,
            "riders alone started a turn {:?} after the interrupt, with no new send (attempts {})",
            turns(&m, &id),
            attempted(&m, &id)
        );
        assert!(m.is_idle(&id).unwrap(), "the thread left idle with only riders queued");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(rows(), vec![(a, ride.clone()), (b, ride.clone()), (c, ride)], "still pending and kept after 6 s");

    // Opposing case: his own next send starts the turn, riders first.
    let d = send(&m, &id, "fourth");
    assert!(within(Duration::from_secs(20), || !turns(&m, &id).is_empty()).await, "his next send starts a turn");
    assert_eq!(turns(&m, &id), vec![vec![a, b, c, d]], "the riders go first, in order");
}

/// **THE SAME RULING ON THE OTHER CALL SITE: a PARKED turn's redelivery.**
/// `maybe_dispatch`'s parked branch clears the `parked_turns` row, takes the
/// lock and spawns a task that awaits the opening pair before `run_turn`.
/// Before fix 2 that pair was not cancel-watched (`edge_poll_open(.., None)`):
/// an interrupt landing in it found no open attempt, no retry and no parked
/// row, only set `cancel_requested`, and `run_once` cleared that flag on
/// entry, so the parked prompt started a turn at the bound with no new send.
///
/// The parked prompt's message P is a REAL queue row, attempted and never
/// delivered (the residue of a turn parked before it reached the process).
/// After the interrupt: no turn starts for 6 s, and P stays pending (`ride`,
/// kept, not dropped) — 6 s outlasts every period in the path: the backstop
/// placement poll `THREADS_RESUME_TICK_MS` = 150 ms (~40 passes), the
/// opening-pair bound `THREADS_EDGE_OPEN_BOUND_MS` = 1.5 s, the abandoned
/// 3 s probe, and the pair gap `THREADS_EDGE_PAIR_GAP_MS` = 10 ms. No retry
/// ladder exists (sentinel), so no retry timer is armed. Opposed by his
/// genuine next send, which starts a turn naming `[P, new]`, P first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_during_a_parked_redelivery_opening_pair_starts_no_turn_6s() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = probing_manager(tmp.path(), "41", Some(3), vec![]);
    let id = probing_thread(&m, tmp.path());
    // P's id sits clear of the ids `send` allocates (1, 2, ...).
    const P: u64 = 1000;
    // The parked row goes in FIRST: from here `maybe_dispatch` takes the parked
    // branch before it ever reads the queue, so the backstop tick can redeliver
    // the park but can never drain P as an ordinary queue row.
    m.store
        .upsert_parked_turn(&threads::store::ParkedTurnRow {
            thread_id: id.clone(),
            prompt: "PARKED-PROMPT".into(),
            message_ids: vec![P],
            attempt: 1,
            reason: "budget exhausted (test park)".into(),
            budget_key: None,
            parked_at: threads::store::now_rfc3339(),
        })
        .unwrap();
    m.store.enqueue(&id, P, "PARKED-PROMPT", "immediate", &serde_json::Value::Null, &[]).unwrap();
    let row = m.store.waiting_queue(&id).unwrap();
    assert_eq!(row.iter().map(|r| r.message_id).collect::<Vec<_>>(), vec![P], "sentinel: P is queued");
    m.open_attempt_for_rows(&id, &row).unwrap();
    assert!(m.store.waiting_queue(&id).unwrap().is_empty(), "sentinel: P is attempted, not waiting");

    m.resume_sweep();
    assert!(
        within(Duration::from_secs(1), || m.store.get_parked_turn(&id).unwrap().is_none()).await,
        "sentinel: the sweep took the parked turn (row cleared, task spawned)"
    );
    assert!(turns(&m, &id).is_empty(), "sentinel: still inside the opening pair");
    assert!(!m.is_idle(&id).unwrap(), "sentinel: the redelivery holds the thread before the cancel");
    m.cancel(&id).unwrap();
    assert!(
        within(Duration::from_millis(500), || m.is_idle(&id).unwrap()).await,
        "the cancel frees the thread at once, inside the {BOUND_MS} ms bound"
    );
    assert!(m.store.get_retry(&id).unwrap().is_none(), "sentinel: no retry ladder armed");
    assert!(m.store.get_parked_turn(&id).unwrap().is_none(), "sentinel: the park is not re-made");

    let rows = || -> Vec<(u64, String)> {
        m.store.waiting_queue(&id).unwrap().into_iter().map(|r| (r.message_id, r.dispatch)).collect()
    };
    let ride = threads::event::delivery::RIDE.to_string();
    let end = Instant::now() + Duration::from_secs(6);
    while Instant::now() < end {
        assert!(
            turns(&m, &id).is_empty(),
            "the parked prompt started a turn {:?} after the interrupt, with no new send",
            turns(&m, &id)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(rows(), vec![(P, ride)], "the parked prompt's message is still pending, ride, after 6 s");
    assert!(m.is_idle(&id).unwrap(), "idle with only the rider queued");

    // Opposing case: his own next send starts the turn, the parked message first.
    let d = send(&m, &id, "fourth");
    assert!(within(Duration::from_secs(20), || !turns(&m, &id).is_empty()).await, "his next send starts a turn");
    assert_eq!(turns(&m, &id), vec![vec![P, d]], "the parked message rides first");
}
