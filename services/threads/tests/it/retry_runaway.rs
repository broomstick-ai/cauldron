//! **A retry that has failed hundreds of times is a loop, not a backoff.**
//!
//! On 2026-08-06 the operator's running daemon held one `retries` row carrying
//! `attempt = 924`, `kind = usage_limit`, `reason = rate limit (429)`. The
//! counter was not lying and was not double-counting: `attempt` is incremented
//! in exactly one place, `run_turn`, AFTER `run_once` has returned — so every
//! one of those 924 units was a real `claude` child process that was spawned,
//! ran, and was refused. Nothing in the crate ever decided that was too many,
//! because the usage-limit ladder was the one ladder with no ceiling:
//!
//! > *"Usage-limit retries are NOT capped — they wait for the reset and try
//! > again."*
//!
//! That exemption was written about WAITING, and waiting is the one thing
//! `attempt` does not measure. A retry that sleeps a week and then succeeds
//! costs one attempt; a retry that reaches 924 has not been waiting at all.
//! Bounding the counter therefore takes nothing away from patience — it only
//! stops the daemon spending processes forever on an answer that is not coming.
//!
//! Two properties are load-bearing here and each has tests below that fail
//! without it:
//!
//! * **There is a top.** A usage-limit retry gives up at a ceiling, clears its
//!   row, and fails the turn VISIBLY — the same "NEVER SILENTLY" contract the
//!   transient and connectivity ladders already honour, which this ladder had
//!   no end at which to honour.
//! * **It escalates before it stops, and it escalates REPEATEDLY.** Lifted from
//!   the deferred-restart starvation bound in `update.rs`, including the reason
//!   that module gives for repeating rather than deduplicating: its own
//!   deferral line was deduplicated forever, so *"an hour of starvation left ONE
//!   line in the ledger and looked like seven quiet deferrals."* One warning at
//!   attempt 12 would describe attempt 924 identically.
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

use threads::event::{Audience, Push, StoredEvent, ThreadEvent};
use threads::retry::{
    usage_limit_escalate_every, usage_limit_starving_from, usage_limit_verdict,
    UsageLimitVerdict, DEFAULT_MAX_USAGE_LIMIT_ATTEMPTS,
};
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

/// An AUI thread — `Auto` account, no budget key — on a SHORT usage-limit
/// leash. The ceiling is per-thread policy precisely so a test can say "this
/// thread gives up at 1" without touching the ladder every other thread uses.
fn settings_with_ceiling(cwd: &Path, ceiling: u32) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s.retry_policy.max_usage_limit_attempts = ceiling;
    s
}

/// A parked prompt mid-ladder: `attempt` already spent, so a cadence-origin pass
/// re-asks placement and spends attempt `attempt + 1`.
fn retry_row(thread_id: &str, attempt: u32, account: &str) -> RetryRow {
    RetryRow {
        thread_id: thread_id.into(),
        prompt: "please ratelimit me".into(),
        message_ids: vec![1],
        attempt,
        kind: "usage_limit".into(),
        reason: "rate limit (429)".into(),
        account: Some(account.to_string()),
        created_at: String::new(),
    }
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
    seen.lock().unwrap().iter().any(|e| e.event.kind() == kind)
}

fn first_of(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> Option<ThreadEvent> {
    seen.lock()
        .unwrap()
        .iter()
        .find(|e| e.event.kind() == kind)
        .map(|e| e.event.clone())
}

// ═════════════════════════════════════════════════════════════════════════
// 1. THE BOUND ITSELF — a pure decision, testable without a daemon
// ═════════════════════════════════════════════════════════════════════════

/// **The ladder has a top.**
///
/// The single assertion the 924 row was missing. Before this, no value of
/// `attempt` — not 924, not 9,240 — produced any answer other than "arm another
/// one".
#[test]
fn a_usage_limit_ladder_that_reaches_its_ceiling_gives_up() {
    let ceiling = DEFAULT_MAX_USAGE_LIMIT_ATTEMPTS;

    assert_eq!(
        usage_limit_verdict(ceiling, ceiling),
        UsageLimitVerdict::GiveUp,
        "a retry at its ceiling must stop"
    );
    assert_eq!(
        usage_limit_verdict(924, ceiling),
        UsageLimitVerdict::GiveUp,
        "the attempt count measured live on the operator's daemon must not be reachable"
    );
    assert_eq!(
        usage_limit_verdict(1, ceiling),
        UsageLimitVerdict::Wait,
        "an ordinary first retry is still an ordinary retry — the bound must not \
         ration the legitimate case, which costs about one attempt per reset window"
    );
}

/// **It says so before it stops, and it keeps saying so.**
///
/// The anti-dedup property, inherited from `update.rs`'s own post-mortem. A
/// bound that escalated exactly once would leave attempt 924 looking precisely
/// like attempt 12 in the ledger, which is the failure mode being fixed, not a
/// fix for it.
#[test]
fn a_starving_retry_says_so_more_than_once_before_it_is_given_up() {
    let ceiling = DEFAULT_MAX_USAGE_LIMIT_ATTEMPTS;
    let loud: Vec<u32> = (1..=ceiling)
        .filter(|a| usage_limit_verdict(*a, ceiling) == UsageLimitVerdict::Starving)
        .collect();

    assert!(
        loud.len() > 1,
        "one warning for an entire ladder is how an hour of starvation left ONE line \
         in update.rs's ledger; got {loud:?}"
    );
    assert!(
        loud.iter().all(|a| *a < ceiling),
        "every escalation must land while the retry is still armed — the give-up is a \
         separate, terminal surface: {loud:?}"
    );
    assert_eq!(
        loud.first().copied(),
        Some(usage_limit_starving_from(ceiling)),
        "the first loud line lands at the starvation threshold"
    );
}

/// **Lowering the ceiling moves the escalation with it rather than disabling
/// it.**
///
/// The thresholds are derived from the ceiling for exactly this reason. A fixed
/// threshold plus a per-thread ceiling means any thread whose ceiling sits under
/// that threshold gives up having never once said anything — silence at the
/// precise moment the mechanism exists to break silence.
#[test]
fn every_ceiling_escalates_at_least_once_before_giving_up() {
    for ceiling in 1..=128u32 {
        assert!(
            usage_limit_escalate_every(ceiling) >= 1,
            "a zero repeat interval would divide by zero at ceiling {ceiling}"
        );
        let loud = (1..ceiling)
            .filter(|a| usage_limit_verdict(*a, ceiling) == UsageLimitVerdict::Starving)
            .count();
        assert!(
            ceiling == 1 || loud >= 1,
            "ceiling {ceiling} gives up without ever having escalated"
        );
    }
}

/// **A starving retry is an operator-account fact and never reaches the model.**
///
/// It names a login and the limit window that login is stuck behind. That is
/// the same category as `retry_scheduled`, and the same prohibition applies:
/// the model cannot act on the operator's quota and would only reason about a
/// resource it does not control and cannot observe.
#[test]
fn a_starving_retry_is_never_spoken_to_the_model() {
    let ev = ThreadEvent::RetryStarving {
        error_kind: "usage_limit".into(),
        reason: "rate limit (429)".into(),
        attempt: 12,
        give_up_at: 48,
        account: "alice-example-com".into(),
        // THE POOL'S next cadence, not a per-thread floor. The wire field survives
        // the move and no consumer is migrated by it; what it names changed.
        retry_at: "2026-10-06T00:05:00+00:00".into(),
        since: None,
    };
    assert_eq!(
        ev.audience(),
        Audience::LedgerOnly,
        "the leak channel stays closed: a quota fact is the operator's, not the thread's"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 2. THE DAEMON ACTUALLY STOPS
// ═════════════════════════════════════════════════════════════════════════

/// **A usage-limit retry that reaches the ceiling stops spawning and fails the
/// turn where the operator can see it.**
///
/// This is the 924 row, reconstructed at a ceiling of two: a retry already one
/// attempt in, its floor elapsed, sitting on an account that answers 429. The
/// sweep re-asks, the turn runs, the refusal comes back — and instead of arming
/// attempt three, four and eventually nine hundred, the row is cleared and a
/// `turn_failed` goes on the ledger.
///
/// Clearing the row is what makes it visible everywhere at once: `thread_status`
/// derives `retrying` purely from that row existing, so the thread leaves
/// `retrying` in the same instant, and the RETRYING column of
/// `threads accounts status` drops it too.
#[tokio::test]
async fn a_usage_limit_retry_at_its_ceiling_is_given_up_visibly() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // CON-27: this test RUNS a turn, so it needs an account threads may place
    // on — the machine `default` is registered but never placeable.
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let id = m
        .create_thread(None, settings_with_ceiling(tmp.path(), 2))
        .unwrap();
    let seen = watch(&m);

    m.store.upsert_retry(&retry_row(&id, 1, "aa-owned")).unwrap();
    m.retry_sweep(threads::runtime::PassOrigin::Cadence);

    assert!(
        eventually(|| saw(&seen, "turn_failed")).await,
        "reaching the ceiling must fail the turn VISIBLY, never silently"
    );
    assert!(
        eventually(|| m.store.get_retry(&id).unwrap().is_none()).await,
        "the retry row must be cleared, or the thread reads `retrying` forever with \
         nothing behind it"
    );

    match first_of(&seen, "turn_failed") {
        Some(ThreadEvent::TurnFailed { message, .. }) => {
            assert!(
                message.contains("aa-owned"),
                "the failure must NAME the login that never cleared, because that is \
                 what the operator can act on: {message:?}"
            );
            assert!(
                message.contains('2'),
                "and how many attempts were spent: {message:?}"
            );
        }
        other => panic!("expected a TurnFailed, got {other:?}"),
    }
}

/// **A retry short of the ceiling escalates and STAYS ARMED.**
///
/// The escalation is a warning, not the give-up. Getting this wrong in the
/// other direction — stopping at the first loud line — would turn a bound
/// meant to prevent 924 wasted spawns into one that abandons the operator's
/// turn on the twelfth, which is well inside what a genuine reset can explain.
#[tokio::test]
async fn a_starving_retry_is_reported_and_keeps_retrying() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    // Ceiling 8 ⇒ starves from attempt 2, repeats every attempt, gives up at 8.
    let m_ceiling = 8;
    let id = m
        .create_thread(None, settings_with_ceiling(tmp.path(), m_ceiling))
        .unwrap();
    let seen = watch(&m);

    m.store.upsert_retry(&retry_row(&id, 1, "aa-owned")).unwrap();
    m.retry_sweep(threads::runtime::PassOrigin::Cadence);

    assert!(
        eventually(|| saw(&seen, "retry_starving")).await,
        "a retry past the starvation threshold must say so on the operator's channel"
    );
    assert!(
        !saw(&seen, "turn_failed"),
        "escalating is not giving up — this retry is far short of its ceiling"
    );
    assert!(
        m.store.get_retry(&id).unwrap().is_some(),
        "and it must still be armed: the whole point is that it keeps trying, loudly"
    );

    match first_of(&seen, "retry_starving") {
        Some(ThreadEvent::RetryStarving {
            attempt,
            give_up_at,
            account,
            ..
        }) => {
            assert_eq!(attempt, 2, "it reports the attempt it has actually reached");
            assert_eq!(
                give_up_at, m_ceiling,
                "and where the ceiling is, so the row answers 'how bad is this' on its own"
            );
            assert_eq!(
                account, "aa-owned",
                "and which login it is starving behind — the same thing update.rs \
                 reports about a starving restart"
            );
        }
        other => panic!("expected a RetryStarving, got {other:?}"),
    }
}

/// **The exemption that produced 924 is gone from the source.**
///
/// A guard against the specific regression of someone reinstating "usage limits
/// retry indefinitely" while the counter still measures spawned processes. The
/// sentence and the mechanism went together; if the sentence comes back the
/// mechanism will follow it.
#[test]
fn the_usage_limit_ladder_no_longer_claims_to_be_unbounded() {
    let runtime = include_str!("../../src/runtime.rs");
    let settings = include_str!("../../src/settings.rs");

    assert!(
        !runtime.contains("Usage limits retry indefinitely."),
        "the 429 path still claims to retry indefinitely"
    );
    // Deliberately NOT a ban on the old sentence: it survives in `settings.rs`
    // as a QUOTATION of what the field replaced, which is worth keeping. What
    // must hold is that the ceiling it denied now exists as policy.
    assert!(
        settings.contains("max_usage_limit_attempts"),
        "RetryPolicy carries no usage-limit ceiling"
    );
    assert!(
        runtime.contains("usage_limit_verdict"),
        "the 429 path must consult the bound"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
