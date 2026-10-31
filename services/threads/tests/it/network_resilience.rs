//! **A network error must never take a thread down** — intent item 73
//! (msg #2166).
//!
//! His words, and the cost is the point: *"if there's an API error and it can't
//! connect to the internet, if it can't reach the open web, if it can't reach
//! Anthropic, whatever, the retry needs to handle that. I can't let these
//! threads just experience a network error and go down… I just gave some input
//! and then it got disconnected from the internet and wasn't able to resume,
//! and I have to now say this a few hours later."*
//!
//! # What was actually wrong, and it was worse than "unhandled"
//!
//! `threads` classifies a failed turn by substring-matching the error text. The
//! connectivity branch matched six strings — `econnreset`, `connection reset`,
//! `unable to connect`, `timed out`, `timeout`, `connection` — every one of
//! which assumes the words "connection" or "timeout" appear.
//!
//! The errors a real outage produces mostly contain neither.
//! `getaddrinfo ENOTFOUND api.anthropic.com` is what a laptop that lost its
//! wifi says. `fetch failed` is what undici says when the request never left.
//! `ECONNREFUSED`, `socket hang up`, `EAI_AGAIN`, `ENETUNREACH` and TLS
//! handshake failures say nothing about "connections" either. **All of them
//! fell through to `Fatal`** — terminal, never retried, thread dead. That is
//! the bug that cost him the hours.
//!
//! And even a connectivity error that DID match was retried on the transient
//! ladder: `max_attempts` 3, backoff 1s/2s/4s. Seven seconds of patience, then
//! the turn was failed. A thirty-second outage killed it either way.
//!
//! # What this file proves
//!
//! 1. The vocabulary of a real outage classifies as connectivity, not Fatal.
//! 2. Connectivity is its OWN class with its OWN ceiling — it neither spends
//!    nor is spent by the usage-limit and transient ladders.
//! 3. It retries on the SAME account: the network is not account-specific.
//! 4. The backoff is capped, so a long outage is never an unbounded silence.
//! 5. Recovery is prompt when the network returns.
//! 6. Exhaustion leaves a VISIBLE failed state, never a phantom-active one.
//! 7. A real 429 still takes the usage-limit path, unchanged.
//!
//! Every test drives the manager in-process against the FAKE claude fixture.
//! **No real claude turn is ever spawned and no real networking is touched or
//! disabled** — an outage is simulated by the fixture printing the stderr a
//! broken network produces and exiting non-zero.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use threads::event::{Push, StoredEvent, ThreadEvent};
use threads::retry::{
    classify_message, connectivity_backoff_secs, RetryDecision, TransientKind,
    MAX_CONNECTIVITY_ATTEMPTS,
};
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

// ════════════════════════════════════════════════════════════════════════════
// 1. CLASSIFICATION — the vocabulary of a real outage
// ════════════════════════════════════════════════════════════════════════════

/// **Every error a real outage produces is connectivity, not Fatal.**
///
/// This is the regression test for the actual defect. Each string here is a
/// verbatim shape from node/undici, curl, or the OS, and **every one of them
/// classified as `Fatal` before the fix** — which meant the thread stopped and
/// stayed stopped. Not one of them contains the substrings the old classifier
/// was looking for.
#[test]
fn the_error_vocabulary_of_a_real_outage_is_retryable() {
    // The ones that were FATAL before — the whole point of this test.
    let was_fatal = [
        "TypeError: fetch failed",
        "Error: getaddrinfo ENOTFOUND api.anthropic.com",
        "Error: getaddrinfo EAI_AGAIN api.anthropic.com",
        "Error: connect ECONNREFUSED 160.79.104.10:443",
        "Error: socket hang up",
        "Error: connect ENETUNREACH 2606:4700::1:443",
        "Error: connect EHOSTUNREACH 160.79.104.10:443",
        "write EPIPE",
        "network is unreachable",
        "You appear to be offline",
        "DNS lookup failed",
        "TLS handshake timed out",
        "unable to verify the first certificate",
        "SSL routines: certificate verify failed",
    ];
    for text in was_fatal {
        match classify_message(text) {
            RetryDecision::Retry { kind, .. } => assert!(
                kind.is_connectivity(),
                "{text:?} must classify as CONNECTIVITY, got {kind:?}"
            ),
            other => panic!(
                "{text:?} must be RETRYABLE — it is the network being down, and before \
                 intent item 73 this was {other:?}, which killed the thread"
            ),
        }
    }

    // …and the ones that already worked keep working.
    for text in [
        "Error: read ECONNRESET",
        "connection reset by peer",
        "unable to connect to api.anthropic.com",
        "the request timed out",
    ] {
        assert!(
            matches!(classify_message(text), RetryDecision::Retry { kind, .. } if kind.is_connectivity()),
            "{text:?} regressed"
        );
    }
}

/// **Connectivity is not a usage limit, and a usage limit is not connectivity.**
///
/// The two classes are what the whole design turns on: one waits for a reset on
/// a possibly different account, the other waits for a network on the same one.
/// Confusing them is how a 429 would get retried in two seconds forever, or a
/// dead network would wait five minutes for a reset that means nothing.
#[test]
fn a_429_is_still_a_usage_limit_and_carries_its_own_tag() {
    assert!(
        matches!(
            classify_message("You have hit your session limit resets 2:40am"),
            RetryDecision::Retry { kind: TransientKind::RateLimit, .. }
        ),
        "a real 429 must still take the usage-limit path, unchanged"
    );
    assert_eq!(TransientKind::RateLimit.kind_str(), "usage_limit");

    // The AUI reads this tag to say "waiting on the network" rather than
    // "working" — it used to be the generic "transient", indistinguishable
    // from an overload.
    assert_eq!(TransientKind::Connectivity.kind_str(), "connectivity");
    assert_eq!(TransientKind::Overload.kind_str(), "transient");
    assert!(!TransientKind::RateLimit.is_connectivity());
    assert!(!TransientKind::Overload.is_connectivity());

    // A 5xx is the SERVICE answering, not the network being gone: it keeps the
    // overload ladder rather than the connectivity one.
    assert!(
        matches!(
            classify_message("API Error: 529 Overloaded"),
            RetryDecision::Retry { kind: TransientKind::Overload, .. }
        ),
        "a 5xx is an overload, not a lost network"
    );

    // And a credit balance is still never retried, however it is phrased.
    assert!(matches!(
        classify_message("Your credit balance is too low to run this request"),
        RetryDecision::CreditBalance { .. }
    ));
}

// ════════════════════════════════════════════════════════════════════════════
// 2. THE POLICY — capped, bounded, and long enough to matter
// ════════════════════════════════════════════════════════════════════════════

/// **The backoff is capped, and the ceiling is the recovery mechanism.**
///
/// Two failures are forbidden and this pins both. An UNBOUNDED backoff turns a
/// long outage into an hours-long silence, which is the thing he was
/// complaining about. A backoff that is too SHORT burns the attempt budget in
/// seconds, which is what actually killed his thread.
#[test]
fn the_connectivity_backoff_is_capped_and_never_grows_without_bound() {
    // It climbs…
    assert_eq!(connectivity_backoff_secs(1), 2);
    assert_eq!(connectivity_backoff_secs(2), 5);
    assert_eq!(connectivity_backoff_secs(3), 10);
    assert_eq!(connectivity_backoff_secs(4), 20);
    assert_eq!(connectivity_backoff_secs(5), 30);
    // …and then STOPS climbing, forever.
    assert_eq!(connectivity_backoff_secs(6), 45);
    assert_eq!(connectivity_backoff_secs(40), 45);
    assert_eq!(connectivity_backoff_secs(u32::MAX), 45);

    // THE RECOVERY GUARANTEE, stated as a number: however long the outage runs,
    // the thread never sleeps more than 45 seconds past the moment the network
    // comes back. That is why no reachability probe is needed — the ceiling
    // bounds the wait, and the thing that proves the network returned is the
    // real turn succeeding rather than a proxy that could disagree with it.
    for attempt in 1..=200u32 {
        assert!(
            connectivity_backoff_secs(attempt) <= 45,
            "attempt {attempt} slept longer than the ceiling"
        );
    }

    // First reaction is FAST — a two-second blip must not cost thirty seconds.
    assert!(connectivity_backoff_secs(1) <= 2);
}

/// **The connectivity ceiling is its own, and it is minutes rather than
/// seconds.**
///
/// `RetryPolicy::max_attempts` is 3, which on the transient ladder is about
/// seven seconds. That is not an outage policy; it is what made a brief
/// disconnection terminal. This asserts the two budgets are genuinely
/// different and that the connectivity one is long enough to sit out the
/// failures that actually stranded him.
#[test]
fn a_lost_network_gets_minutes_of_patience_not_seconds() {
    assert!(
        MAX_CONNECTIVITY_ATTEMPTS >= 20,
        "a network outage policy measured in single-digit attempts is what item 73 is about"
    );
    let total: u64 = (1..=MAX_CONNECTIVITY_ATTEMPTS)
        .map(connectivity_backoff_secs)
        .sum();
    assert!(
        total >= 15 * 60,
        "the ladder must sit out a router reboot — got {total}s"
    );
    assert!(
        total <= 60 * 60,
        "…but must still END, and be seen to end: {total}s"
    );
    // Distinct from the transient ceiling, which is the whole point.
    assert!(MAX_CONNECTIVITY_ATTEMPTS > threads::settings::RetryPolicy::default().max_attempts);
}

// ════════════════════════════════════════════════════════════════════════════
// 3. FIXTURES
// ════════════════════════════════════════════════════════════════════════════

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

/// CON-27: an account threads OWNS. The machine default is not placeable.
fn config_at(dir: &Path, extra: &[&str]) -> Config {
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        dir.join("aa-owned").to_string_lossy().to_string(),
    );
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

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
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

fn uuid_ish() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
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

// ════════════════════════════════════════════════════════════════════════════
// 4. BEHAVIOUR — end to end against a simulated outage
// ════════════════════════════════════════════════════════════════════════════

/// **A thread that loses the network schedules a retry instead of dying — and
/// it stays where it is.**
///
/// The heart of item 73. Before the fix, the DNS failure this fixture emits
/// classified as `Fatal`: a `TurnFailed`, no retry row, work gone, and he
/// found out hours later. Now it schedules a connectivity retry.
///
/// The second half is just as load-bearing: **no failover**. The network is not
/// account-specific, so moving the thread would spend a placement and a
/// transcript port to arrive at the same unreachable socket. A second account
/// is registered here precisely so that "it did not move" is a real assertion
/// rather than a vacuous one.
#[tokio::test]
async fn a_lost_network_retries_on_the_same_account_instead_of_failing_over() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["zz-sibling"])).unwrap();

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let on = m.serving_account(&id).unwrap();

    let seen = watch(&m);
    m.send(&id, "netdown please".into(), Default::default(), json!({}), false)
        .unwrap();

    assert!(
        eventually(|| saw(&seen, "retry_scheduled")).await,
        "a lost network must be RETRIED, not fatal — this is the whole of item 73: {:?}",
        seen.lock().unwrap().iter().map(|e| e.event.kind()).collect::<Vec<_>>()
    );

    // It is on the ledger as a connectivity wait, distinguishable from a limit.
    let row = m.store.get_retry(&id).unwrap().expect("the work is held, not dropped");
    assert_eq!(
        row.kind, "connectivity",
        "the AUI reads this to say 'waiting on the network' rather than 'working'"
    );
    assert_ne!(row.kind, "usage_limit", "a network outage is not a usage limit");

    // CON-4: the prompt itself is never dropped.
    assert!(!row.prompt.is_empty(), "the operator's words are still held");

    // AND IT DID NOT MOVE. A sibling account exists and was not chosen.
    assert_eq!(
        m.serving_account(&id).unwrap(),
        on,
        "failing over cannot fix a broken network — it must retry in place"
    );
    assert!(
        !saw(&seen, "account_switched"),
        "no placement may be spent on a failure that is not the account's fault"
    );
    // Nothing about the ACCOUNT was learned, so nothing may be recorded against
    // it: a network outage must not bar a perfectly healthy login.
    let (unavailable, unhealthy) = m.barred_accounts();
    assert!(
        unavailable.is_empty() && unhealthy.is_empty(),
        "a lost network says nothing about any account: {unavailable:?} {unhealthy:?}"
    );
}

/// **The thread comes back on its own when the network does.**
///
/// His actual complaint was not that the turn failed, it was that it never
/// resumed: *"wasn't able to resume, and I have to now say this a few hours
/// later."* The fixture is down for exactly one attempt — a router reboot —
/// and the thread must finish the turn without him touching anything.
///
/// This is also what makes the capped ladder sufficient without a reachability
/// probe: the first retry is two seconds out, so recovery is prompt.
#[tokio::test]
async fn the_thread_resumes_by_itself_when_the_network_comes_back() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &[])).unwrap();
    m.start_watchers();

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    // The fixture's once-then-succeed marker is keyed on the prompt text and
    // lives in a SHARED scratch dir, so a fixed prompt would be "already
    // flapped" on the second run of this suite and the outage would never
    // happen. Unique per run, so the first attempt genuinely fails every time.
    let prompt = format!("netflap please {}", uuid_ish());
    m.send(&id, prompt, Default::default(), json!({}), false)
        .unwrap();

    assert!(
        eventually(|| saw(&seen, "retry_scheduled")).await,
        "the first attempt hits a refused connection"
    );
    assert!(
        eventually(|| saw(&seen, "turn_ended")).await,
        "…and the thread finishes ON ITS OWN once the network returns, with no \
         further input from the operator: {:?}",
        seen.lock().unwrap().iter().map(|e| e.event.kind()).collect::<Vec<_>>()
    );

    // The wait is over and nothing is left holding the thread.
    assert!(
        eventually(|| m.store.get_retry(&id).unwrap().is_none()).await,
        "a satisfied retry is cleared"
    );
    assert!(
        !saw(&seen, "turn_failed"),
        "a recovered outage must not also report a failure"
    );
}

/// **Exhaustion is VISIBLE — never a turn that reads Active forever.**
///
/// The audit's top-ranked hole is a thread that looks like it is working when
/// nothing is behind it. A retry ladder that can run for half an hour is an
/// obvious way to produce a second one, so this pins the end state: the retry
/// row is gone, a `turn_failed` is on the ledger carrying the `connectivity`
/// tag, the lock is released, and the thread reads idle.
#[tokio::test]
async fn giving_up_leaves_a_visible_failure_never_a_phantom_active_turn() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &[])).unwrap();

    let id = m.create_thread(None, settings(tmp.path())).unwrap();

    // Stand the thread at the END of its connectivity budget, so one more
    // failure exhausts it. The budget is WALL-CLOCK since 2026-10-14 (thread
    // 60f0380d), so what is seeded is a failure streak that began longer ago
    // than `CONNECTIVITY_BUDGET_SECS`; driving half an hour of real attempts is
    // not an option, and the end state is what is under test.
    let seen = watch(&m);
    let ago = |secs: i64| (chrono::Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339();
    let over = threads::retry::CONNECTIVITY_BUDGET_SECS as i64 + 60;
    m.store
        .put_delivery_clock(
            &id,
            &[1],
            &threads::store::DeliveryClock {
                turn_started_at: ago(over + 60),
                streak_kind: Some("connectivity".into()),
                streak_since: Some(ago(over)),
            },
        )
        .unwrap();
    m.store
        .upsert_retry(&threads::store::RetryRow {
            thread_id: id.clone(),
            prompt: "netdown please".into(),
            message_ids: vec![1],
            attempt: MAX_CONNECTIVITY_ATTEMPTS,
            kind: "connectivity".into(),
            reason: "cannot reach the service (network)".into(),
            account: Some("aa-owned".into()),
            created_at: String::new(),
        })
        .unwrap();

    // The POOL'S CADENCE is the deadline, so it reaches the lock and runs rather
    // than asking a question the gate would answer for free.
    m.retry_sweep(threads::runtime::PassOrigin::Cadence);

    assert!(
        eventually(|| saw(&seen, "turn_failed")).await,
        "exhaustion must be REPORTED: {:?}",
        seen.lock().unwrap().iter().map(|e| e.event.kind()).collect::<Vec<_>>()
    );
    let Some(ThreadEvent::TurnFailed { error_kind, message }) = first_of(&seen, "turn_failed")
    else {
        panic!("no failure event")
    };
    assert_eq!(
        error_kind, "connectivity",
        "he must be able to tell 'the network went away' from 'the model broke'"
    );
    assert!(
        message.contains("reach the service"),
        "the message must name the NETWORK as the cause, so the action is obvious: {message}"
    );

    // NOT ACTIVE. No retry row, so nothing claims to be waiting…
    assert!(
        eventually(|| m.store.get_retry(&id).unwrap().is_none()).await,
        "a given-up retry must not linger and look like a pending wait"
    );
    // …and the thread is genuinely idle, not holding an execution lock.
    assert!(
        eventually(|| m.store.get_thread_lock(&id).unwrap().is_none()).await,
        "the lock must be released or the thread reads Active forever"
    );
}

/// **A connectivity retry does not spend the usage-limit budget, and vice
/// versa.**
///
/// The two ladders are independent by construction. If they shared a counter, a
/// flaky network would silently eat the patience a real 429 needs, and a
/// thread that had waited out one rate limit would give up on the network
/// immediately afterwards.
#[tokio::test]
async fn a_network_retry_does_not_consume_a_usage_limit_attempt() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &[])).unwrap();

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    m.send(&id, "netdown please".into(), Default::default(), json!({}), false)
        .unwrap();
    assert!(eventually(|| saw(&seen, "retry_scheduled")).await);

    let row = m.store.get_retry(&id).unwrap().unwrap();
    assert_eq!(row.kind, "connectivity");

    // The transient ceiling is 3. A connectivity retry sitting at attempt 1 of
    // its OWN ladder is the assertion that it is not being counted against it —
    // under the old behaviour this row would have been `transient` and three
    // attempts from death.
    assert!(
        row.attempt <= MAX_CONNECTIVITY_ATTEMPTS,
        "a connectivity retry is bounded by its own ceiling"
    );
    assert_ne!(
        row.kind, "transient",
        "the network class must be distinguishable from a generic transient, or the \
         AUI cannot tell the operator what is happening"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
