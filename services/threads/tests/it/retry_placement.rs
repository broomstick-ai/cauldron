//! **One placement authority** — the 429 retry path asks the gate, it does not
//! choose (intent item 58, msg #1465).
//!
//! > "Holding that fall over behavior for the retry in the placement is
//! > something that we should knock out immediately."
//!
//! `try_switch_account` was a SECOND placement implementation. On a rate limit
//! it ranked the registry itself through `accounts::select_failover`, moved the
//! thread, and `continue`d straight into the next attempt — so the budget's
//! scope, the pooled trip, the placement deadbands, the round-robin cursor and
//! the admission gate itself all sat one function away and were never asked. It
//! predates DEC-56 as amended, and the rule was written against exactly this
//! shape: *"you don't directly attempt to put it in another account. You put it
//! back to the balancer."*
//!
//! What replaces it is not a smaller selector, it is NO selector. The retry path
//! now does the two things only it can do — stamp the account that just
//! answered 429 with its limit window, and report whether the answer moved the
//! thread — and asks `admit_or_park`, the same gate the queue, the parked-turn
//! redelivery and the retry timer go through. The fact it learned travels as
//! `AdmissionRequest::unavailable`.
//!
//! The first test is the SOURCE GUARD, in the shape of
//! `system_events::the_interrupt_path_contains_no_placement_logic`: it reads the
//! turn runner's body and fails on sight of account-ranking vocabulary. The rest
//! are behavioural and each fails without the change.
//!
//! Every test drives the manager in-process against the FAKE claude fixture. No
//! real claude turn is ever spawned; `THREADS_HOME` is never resolved (every
//! manager gets an explicit `data_dir` under a tempdir, so `~/.threads` cannot
//! come into existence); `THREADS_CLAUDE_HOME` points the machine-default
//! account's transcript store at a scratch dir, so nothing here can read or
//! write inside the operator's own `~/.claude`; and nothing touches the
//! keychain, the running daemon, or the AUI.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use threads::accounts::{locate_transcript, projects_dir_for, transcript_events};
use threads::event::{Push, StoredEvent, ThreadEvent};
use threads::gauge::Axis;
use threads::settings::{Failover, ThreadSettings};
use threads::{build_manager, Config, ThreadManager};

// ═════════════════════════════════════════════════════════════════════════
// 1. THE SOURCE GUARD
// ═════════════════════════════════════════════════════════════════════════

/// **The retry path must contain no independent account ranking.**
///
/// A second placement implementation does not fail loudly, it DRIFTS — it
/// bypasses whichever gate it does not know about, and it splits ownership of
/// "where does this thread run" across two files. So the ban is structural: this
/// reads the turn runner's own body and fails on sight of any identifier that
/// would let it pick an account for itself.
///
/// If placement must behave differently on a rate limit, that difference is an
/// INPUT to `Budgets::place` / `Budgets::admit` and is implemented there
/// (`AdmissionRequest::unavailable` is exactly that input).
#[test]
fn the_retry_path_contains_no_account_ranking() {
    let src = include_str!("../../src/runtime.rs");
    let start = src
        .find("    async fn run_turn(")
        .expect("run_turn was renamed — re-point this test");
    let body = &src[start..];
    let end = body
        .find("fn park_killed_turn(")
        .expect("the item after run_turn moved — re-point this test");
    let body = &body[..end];
    // CODE only. The comment in that branch names the deleted selector on
    // purpose — the history is the reason the ban exists — and a guard that
    // could not tell prose from an identifier would forbid explaining itself.
    let code: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let body = code.as_str();

    // The rate-limit branch is the one this test exists for; guarding the whole
    // turn runner is deliberate, because a selector re-grown anywhere in it
    // would be reachable from that branch.
    assert!(
        body.contains("TransientKind::RateLimit"),
        "the slice must actually contain the 429 branch"
    );

    for banned in [
        "select_failover",
        "try_switch_account",
        "selectable_pool",
        "account_states",
        "barred_accounts",
        "unavailable_accounts",
        "unhealthy_accounts",
        "selection_units",
        "distinct_healthy_logins",
        "self.registry",
        "self.budgets",
        "PlacementRequest",
        "AdmissionRequest",
        ".place(",
        ".admit(",
        "switch_account(",
        "move_thread",
        "port_conversation",
        "config_dir_for",
        "limit_until",
        ".selectable(",
        ".healthy(",
    ] {
        assert!(
            !body.contains(banned),
            "the retry path grew account ranking ({banned:?}). The 429 path records its \
             CAUSE and asks `admit_or_park`; if placement must differ on a rate limit, \
             that is an INPUT to Budgets::place / Budgets::admit."
        );
    }

    // …and it does the three things it IS allowed to do: record the cause, ask
    // the one authority, and hand the wait to the existing ladder.
    assert!(
        body.contains("note_account_limit"),
        "the cause has to be recorded where placement reads it"
    );
    assert!(
        body.contains("admit_or_park"),
        "the retry must ask the same gate as every other turn"
    );
    assert!(
        body.contains("schedule_retry"),
        "the ladder still owns run-now-versus-wait"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 2. FIXTURES
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
    // NEVER the operator's own `~/.claude`.
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn claude_home() -> PathBuf {
    std::env::temp_dir().join("threads-tests-claude-home")
}

fn config_at(dir: &Path, extra: &[&str]) -> Config {
    let mut accounts = std::collections::BTreeMap::new();
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

fn count(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|e| e.event.kind() == kind)
        .count()
}

fn first_of(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> Option<ThreadEvent> {
    seen.lock()
        .unwrap()
        .iter()
        .find(|e| e.event.kind() == kind)
        .map(|e| e.event.clone())
}

fn observe(m: &ThreadManager, account: &str, pct: f64) {
    m.budgets
        .observe(account, Axis::Session, pct, None, "usage_poll")
        .unwrap();
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

/// Plant a transcript for `session` in the store `config_dir` names.
fn plant(config_dir: Option<&str>, home: &Path, slug: &str, session: &str, n: usize) -> PathBuf {
    let dir = projects_dir_for(config_dir, home).join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    let body = (0..n)
        .map(|i| json!({"sessionId": session, "seq": i, "type": "user"}).to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&path, body).unwrap();
    path
}

fn slug(tag: &str) -> String {
    format!("-tmp-retry-placement-{tag}-{}", uuid_ish())
}

// ═════════════════════════════════════════════════════════════════════════
// 3. BEHAVIOUR
// ═════════════════════════════════════════════════════════════════════════

/// **A rate-limited retry lands where PLACEMENT says, and nowhere else — and it
/// carries its conversation there.**
///
/// Three accounts, chosen so that the two answers DIFFER. `aa-busy` is 95%
/// through its session and sorts first; `zz-free` is at 3% and sorts last. The
/// deleted selector took "the first registered account other than the current
/// one that is a live candidate", i.e. `aa-busy` — the most exhausted account in
/// the registry — because alphabetical order was the whole of its ranking. The
/// gate ranks on the week, then the session, and picks `zz-free`.
///
/// CON-27: the thread STARTS on `bb-start`, a placeable account, where it used
/// to start on `default`. The machine default cannot be a starting point any
/// more because it cannot be placed on at all, so the test names an account
/// threads actually owns. Nothing else about the scenario changes — `bb-start`
/// sorts between the other two, so `aa-busy` is still the alphabetical pick the
/// deleted selector would have made.
///
/// The conversation is the second half. A thread with history moves only by
/// `move_thread` with `port_must_succeed`, so the transcript arrives on the
/// target store byte for byte before the thread is said to be there.
#[tokio::test]
async fn a_rate_limited_retry_lands_where_placement_says_and_ports_its_conversation() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let home = claude_home();
    let m = build_manager(config_at(tmp.path(), &["aa-busy", "bb-start", "zz-free"])).unwrap();
    let free = tmp.path().join("zz-free").to_string_lossy().to_string();

    // Readings first, so create-time placement is decided by them: `bb-start`
    // has the most room and takes the thread.
    observe(&m, "bb-start", 1.0);
    observe(&m, "aa-busy", 95.0);
    observe(&m, "zz-free", 3.0);

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "bb-start");

    // One good turn, so the thread has conversation HISTORY on `bb-start`…
    m.send(&id, "hello".into(), Default::default(), json!({}), false)
        .unwrap();
    assert!(eventually(|| !m.store.thread_accounts(&id).unwrap().is_empty()).await);
    assert_eq!(
        m.store.thread_accounts(&id).unwrap(),
        vec!["bb-start".to_string()]
    );

    // …and a real transcript on `bb-start`'s store to prove it moves with it.
    //
    // CON-27: `bb-start`'s store, NOT the machine default's. A transcript lives
    // under `<config_dir>/projects/<slug>/`, and since the thread is born on an
    // account threads owns, that is `<tmp>/bb-start/projects/`. Planting under
    // `home` (the machine login's `~/.claude`) would leave the port with
    // nothing to find on the source account — and would be a test writing into
    // the operator's own store, which is exactly what CON-27 forbids.
    let start = tmp.path().join("bb-start").to_string_lossy().to_string();
    let sl = slug("failover");
    let source = plant(Some(&start), &home, &sl, &id, 9);

    let seen = watch(&m);
    m.send(&id, "ratelimit please".into(), Default::default(), json!({}), false)
        .unwrap();

    assert!(
        eventually(|| saw(&seen, "account_switched")).await,
        "the gate had somewhere to put this thread and should have said so"
    );
    // THE PLACEMENT. The FIRST move is the one the 429 asked for — the fixture
    // answers 429 on every account, so the thread walks the pool afterwards and
    // only this event pins where the retry was placed.
    let Some(ThreadEvent::AccountSwitched { from, to, .. }) = first_of(&seen, "account_switched")
    else {
        panic!("no switch event")
    };
    assert_eq!(from, "bb-start");
    assert_eq!(
        to, "zz-free",
        "placement ranks on usage; `aa-busy` is the alphabetical pick the \
         deleted selector would have made, at 95% of its session"
    );

    // THE CONVERSATION WENT WITH IT, byte for byte, and the source survives.
    let ported = locate_transcript(Some(&free), &home, &id).expect("the conversation ported");
    assert_eq!(transcript_events(&ported).unwrap(), 9);
    assert_eq!(
        std::fs::read_to_string(&ported).unwrap(),
        std::fs::read_to_string(&source).unwrap()
    );
    assert!(source.is_file(), "the source is never deleted");
    // Recorded as history, so the continuity gate treats the thread as living
    // there from now on.
    assert!(m
        .store
        .thread_accounts(&id)
        .unwrap()
        .contains(&"zz-free".to_string()));

    // DEC-55: the interrupt event REPORTS the move; it did not make it.
    assert!(eventually(|| saw(&seen, "thread_system_interrupted")).await);
    let Some(ThreadEvent::ThreadSystemInterrupted {
        switched,
        from_account,
        to_account,
        ..
    }) = first_of(&seen, "thread_system_interrupted")
    else {
        panic!("no interrupt event")
    };
    assert!(switched, "a switchover happened and the flag says so");
    assert_eq!(from_account.as_deref(), Some("bb-start"));
    assert_eq!(to_account.as_deref(), Some("zz-free"));
}

/// **A retry placement REFUSES parks — it never runs anyway.**
///
/// This is the retry admission gate from `5cce58f`, which the old path could
/// dodge outright: on a successful switch it incremented the attempt and
/// `continue`d into a fresh execution without passing `admit_or_park` at all.
///
/// One registered account, so the 429 leaves nowhere to go. The thread carries a
/// budget key, so it is not the ungated shape that always runs, and the gate is
/// entitled to say no. What must come out: a `turn_parked` naming
/// `unavailable`, exactly ONE execution, and the work still waiting.
///
/// CON-27: "one account" now has to be one PLACEABLE account, registered
/// explicitly. It used to be spelled `&[]` — no extra accounts — and leaned on
/// the machine `default` that `ensure_default` registers on every boot to be
/// the one. That is exactly the accident this constraint removes, and with the
/// pool empty the daemon now refuses to create the thread at all, so the
/// fixture names the account it means.
#[tokio::test]
async fn a_retry_the_gate_refuses_parks_rather_than_running() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["only-one"])).unwrap();

    // The thread has to be GATED for a park to be permitted, without gating on
    // anything of its own.
    //
    // A limitless key used to be the way to get that, but since INTENT 72b it
    // is exactly the shape every thread inherits by default and it deliberately
    // no longer gates anything — otherwise forcing budgets onto the fleet would
    // have changed behaviour. So the key stays for ATTRIBUTION and the gated
    // property now comes from a threshold nothing has ever observed against:
    // `configured()` is non-empty, so the thread is gated, while the ceiling
    // itself is set out of reach.
    m.budgets
        .set_config(&threads::BudgetConfig {
            key: "keyed".into(),
            parent_key: None,
            session_pct: None,
            weekly_pct: None,
            fable_pct: None,
            cash_usd: None,
            accounts: vec!["only-one".into()],
            on_trip: Default::default(),
            disabled: false,
            disabled_at: None,
        })
        .unwrap();

    let mut s = settings(tmp.path());
    s.budget_key = Some("keyed".into());
    // 150% of a percentage gauge is a ceiling nothing can ever cross, so the
    // thread is gated while this gate can never be the reason it parks — which
    // keeps `unavailable` (the limit window) as the cause under test.
    s.thresholds.session = Some(150.0);
    let id = m.create_thread(None, s).unwrap();

    let seen = watch(&m);
    m.send(&id, "ratelimit please".into(), Default::default(), json!({}), false)
        .unwrap();

    assert!(
        eventually(|| saw(&seen, "turn_parked")).await,
        "a refused retry PARKS: {:?}",
        seen.lock().unwrap().iter().map(|e| e.event.kind()).collect::<Vec<_>>()
    );
    let Some(ThreadEvent::TurnParked { cause, reason, .. }) = first_of(&seen, "turn_parked") else {
        panic!("no park event")
    };
    assert_eq!(
        cause, "unavailable",
        "the cause the caller can act on — nothing crossed a ceiling: {reason}"
    );

    // ONE execution. The old path's `continue` is what this forbids.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        count(&seen, "turn_started"),
        1,
        "a parked retry must not run anyway"
    );
    assert!(!saw(&seen, "account_switched"), "there was nowhere to go");
    assert_eq!(m.serving_account(&id).unwrap(), "only-one");

    // CON-4: parked, never dropped — the ladder still holds the prompt.
    assert!(saw(&seen, "retry_scheduled"), "the work is handed back, not lost");
    assert!(m.store.get_retry(&id).unwrap().is_some());
    assert_ne!(m.park_state(&id).unwrap(), serde_json::Value::Null);
}

/// **The account that just failed is not chosen again while a healthy sibling
/// exists — and the CAUSE is what does it, not the gauge.**
///
/// Asked directly of the gate, twice, over identical readings that FAVOUR the
/// account being left: `aa-first` at 4%, `zz-second` at 40%. With nothing
/// excluded the thread rightly stays put. Name `aa-first` in `unavailable` — the
/// one thing the 429 path records — and the same gate moves it, choosing the
/// sibling by the same ranking every other placement uses.
///
/// CON-27, and this is the load-bearing line of the test now: the pool comes
/// from `placement_pool()` rather than a hand-written list of names. The gate
/// takes `Placeable`, so a test can no longer invent a pool containing the
/// machine default — which is precisely the guarantee, expressed as the thing
/// the test is unable to write.
#[tokio::test]
async fn the_cause_is_what_moves_the_thread_not_the_gauge() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-first", "zz-second"])).unwrap();

    observe(&m, "aa-first", 4.0);
    observe(&m, "zz-second", 40.0);

    let pool = m.placement_pool();
    let history = vec!["aa-first".to_string()];
    let thresholds = threads::Thresholds::ungated();
    let settled = threads::budgets::AdmissionRequest {
        budget_key: None,
        thresholds: &thresholds,
        accounts: &pool,
        current_account: "aa-first",
        pinned: false,
        history_accounts: &history,
        unavailable: &[],
        unhealthy: &[],
        demand: threads::budgets::Demand::default(),
    };
    assert!(
        matches!(
            m.budgets.admit(&settled).unwrap(),
            threads::budgets::Admission::Admit { switch_to: None }
        ),
        "on the better-ranked account with history, the thread stays — this test \
         is only meaningful while the gauge alone would keep it here"
    );

    // The 429, carried in.
    let limited = vec!["aa-first".to_string()];
    let after = threads::budgets::AdmissionRequest {
        unavailable: &limited,
        ..settled.clone()
    };
    // First ask: the thread has history only where it may not run, so the gate
    // parks for CONTINUITY — and `unavailable` is what took `aa-first` out.
    match m.budgets.admit(&after).unwrap() {
        threads::budgets::Admission::Park { cause, reason, .. } => {
            assert_eq!(cause, threads::budgets::ParkCause::Continuity, "{reason}");
        }
        other => panic!("expected a continuity park, got {other:?}"),
    }
    // The failover's second ask — the conversation is free to move — picks the
    // sibling, and never the account that just answered 429.
    let free = threads::budgets::AdmissionRequest {
        history_accounts: &[],
        ..after.clone()
    };
    match m.budgets.admit(&free).unwrap() {
        threads::budgets::Admission::Admit { switch_to } => assert_eq!(
            switch_to.as_deref(),
            Some("zz-second"),
            "the healthy sibling, chosen by the one ranking"
        ),
        other => panic!("expected an admit, got {other:?}"),
    }

    // And the set really is derived from what the 429 records: nothing is
    // limited yet, so nothing is barred.
    let (unavailable, unhealthy) = m.barred_accounts();
    assert!(unavailable.is_empty() && unhealthy.is_empty());
}

/// **A PIN is still a pin.** The refusal used to be the first line of
/// `try_switch_account` (`Failover::PinnedAccount` ⇒ return false); deleting
/// that function left the gate as the only place it lives, so this is the test
/// that would catch it going missing.
#[tokio::test]
async fn a_pinned_thread_still_never_moves_on_a_rate_limit() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-first", "zz-second"])).unwrap();

    // The sibling is wide open and would otherwise take this thread.
    observe(&m, "aa-first", 5.0);
    observe(&m, "zz-second", 2.0);

    // CON-27: the pin names a PLACEABLE account. Pinning the machine default is
    // a different behaviour with its own test below — a pin is the one
    // "placement" a human writes by hand, so it is the one that has to be
    // refused by hand.
    let mut s = settings(tmp.path());
    s.retry_policy.failover = Failover::PinnedAccount {
        account: "aa-first".into(),
    };
    let id = m.create_thread(None, s).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "aa-first");

    let seen = watch(&m);
    m.send(&id, "ratelimit please".into(), Default::default(), json!({}), false)
        .unwrap();

    assert!(
        eventually(|| saw(&seen, "retry_scheduled")).await,
        "a pinned thread waits for ITS account's reset"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!saw(&seen, "account_switched"), "a pin is never re-placed");
    assert_eq!(m.serving_account(&id).unwrap(), "aa-first");

    let Some(ThreadEvent::ThreadSystemInterrupted { switched, .. }) =
        first_of(&seen, "thread_system_interrupted")
    else {
        panic!("no interrupt event")
    };
    assert!(!switched, "nothing moved, and the flag says so");
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
