//! Transcript porting, budget-trip failover, and the external-transcript
//! reconcile (msg #1028 / intent item 39).
//!
//! The operator superseded a standing decision:
//!
//! > "on the thread never moves… that's not what I mean. If we're running a
//! > thread on one account and then it hits the budget for that account, we
//! > should continue running the thread on the other account. […] I just care
//! > about continuity and it staying within its threshold and budget. In fact,
//! > I would want it to fall over to a different account if it failed on one
//! > account."
//!
//! What "a thread with history never moves" was protecting is the CONVERSATION,
//! and the conversation is one file — the claude session transcript at
//! `<config_dir>/projects/<cwd-slug>/<session>.jsonl`. Port the file and the
//! thread ports.
//!
//! Every test here drives the daemon in-process against the FAKE claude
//! fixture. No real claude turn is ever spawned; `THREADS_HOME` is never
//! resolved (every manager gets an explicit `data_dir` under a tempdir, so
//! `~/.threads` cannot come into existence); `THREADS_CLAUDE_HOME` points the
//! machine-default account's transcript store at a scratch dir, so nothing here
//! can read or write inside the operator's own `~/.claude`; and nothing touches
//! the keychain or the live AUI.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use threads::accounts::{
    locate_transcript, port_transcript, port_transcript_file, projects_dir_for, transcript_events,
    PortOutcome,
};
use threads::event::{Push, ThreadEvent};
use threads::gauge::Axis;
use threads::settings::{AccountChoice, ThreadSettings};
use threads::{build_manager, Config, ThreadManager};

// ── fixtures ────────────────────────────────────────────────────────────────

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
    // The machine-default account's transcript store is `<HOME>/.claude` — the
    // OPERATOR'S OWN login. Point the transcript home at a scratch dir so no
    // test can read or write inside it.
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn claude_home() -> PathBuf {
    std::env::temp_dir().join("threads-tests-claude-home")
}

/// A config over `dir` with `extra` additional registered accounts, each with a
/// real config dir under `dir` (so its transcript store is inside the tempdir).
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

fn watch(m: &ThreadManager) -> Arc<Mutex<Vec<String>>> {
    let mut rx = m.subscribe();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Event(ev) = p {
                // A `Custom` is recorded as `custom:<event_type>`: since R10.M1
                // every dispatch writes a `custom` (`delivery_attempted`), so the
                // bare kind no longer says WHICH custom happened.
                let k = match &ev.event {
                    ThreadEvent::Custom { event_type, .. } => format!("custom:{event_type}"),
                    e => e.kind().to_string(),
                };
                out.lock().unwrap().push(k);
            }
        }
    });
    seen
}

fn saw(seen: &Arc<Mutex<Vec<String>>>, kind: &str) -> bool {
    seen.lock().unwrap().iter().any(|k| k == kind)
}

fn observe(m: &ThreadManager, account: &str, pct: f64) {
    m.budgets
        .observe(account, Axis::Session, pct, None, "usage_poll")
        .unwrap();
}

/// A transcript body of `n` events. Shaped like the real thing (one JSON object
/// per line) so `transcript_events` is counting what it counts in production.
fn body(session: &str, n: usize) -> String {
    (0..n)
        .map(|i| json!({"sessionId": session, "seq": i, "type": "user"}).to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// Plant a transcript for `session` in the store `config_dir` names, under the
/// cwd slug `slug`. Returns the path.
fn plant(config_dir: Option<&str>, home: &Path, slug: &str, session: &str, n: usize) -> PathBuf {
    let dir = projects_dir_for(config_dir, home).join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    std::fs::write(&path, body(session, n)).unwrap();
    path
}

/// A slug unique to this test run, so the shared scratch `claude_home` can host
/// many tests at once without collisions.
fn slug(tag: &str) -> String {
    format!("-tmp-transcript-port-{tag}-{}", uuid_ish())
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

// ── 1. the port mechanism ───────────────────────────────────────────────────

/// Copy, verify, activate — and NEVER delete the source.
///
/// The source surviving is the whole crash-safety argument: which copy is
/// authoritative is decided by the thread's daemon record, not by the
/// filesystem, so leaving both intact means no crash point can produce a thread
/// whose conversation is nowhere.
#[test]
fn a_port_copies_verifies_and_activates_without_ever_deleting_the_source() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let home = claude_home();
    let a = tmp.path().join("acct-a").to_string_lossy().to_string();
    let b = tmp.path().join("acct-b").to_string_lossy().to_string();
    let session = format!("sess-{}", uuid_ish());
    let sl = slug("port");

    let source = plant(Some(&a), &home, &sl, &session, 7);

    let outcome = port_transcript(Some(&a), Some(&b), &home, &session).unwrap();
    let PortOutcome::Ported {
        dest,
        events,
        replaced,
    } = outcome
    else {
        panic!("expected a port, got {outcome:?}");
    };
    assert_eq!(events, 7);
    assert_eq!(replaced, None);

    // The destination reuses the SOURCE directory's own basename. The cwd slug
    // is never re-derived — a re-implementation of claude's slug rule that
    // drifted would answer "the conversation is not here", which is the one
    // answer that must never be wrong.
    assert_eq!(dest.parent().unwrap().file_name().unwrap(), sl.as_str());
    assert_eq!(dest, projects_dir_for(Some(&b), &home).join(&sl).join(format!("{session}.jsonl")));

    // Byte-identical, and the source is untouched.
    assert_eq!(
        std::fs::read_to_string(&dest).unwrap(),
        std::fs::read_to_string(&source).unwrap()
    );
    assert!(source.is_file(), "the source is NEVER deleted");
    assert_eq!(transcript_events(&source).unwrap(), 7);

    // Both stores now answer "yes, the conversation is here" — which is exactly
    // what makes the daemon record, not the filesystem, the authority.
    assert_eq!(
        locate_transcript(Some(&b), &home, &session).as_deref(),
        Some(dest.as_path())
    );
    assert_eq!(
        locate_transcript(Some(&a), &home, &session).as_deref(),
        Some(source.as_path())
    );

    // Idempotent: porting again keeps what is already there.
    let again = port_transcript(Some(&a), Some(&b), &home, &session).unwrap();
    assert!(matches!(again, PortOutcome::Kept { .. }), "{again:?}");

    // Nothing to port is not an error — it is an answer.
    let none = port_transcript(Some(&b), Some(&a), &home, "no-such-session").unwrap();
    assert_eq!(none, PortOutcome::NoSource);
}

/// A crash at any point BEFORE activation leaves a usable state.
///
/// The only artifact a crashed port can leave in the destination is the
/// dot-prefixed `.port-…` temp, which is not a `.jsonl` and so cannot be
/// mistaken for a transcript by the scan that locates one. The destination
/// therefore still answers "the conversation is not here", the source is
/// unchanged, and the next port sweeps the corpse and succeeds.
#[test]
fn a_crash_before_activation_leaves_the_source_authoritative_and_no_half_transcript() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let home = claude_home();
    let a = tmp.path().join("crash-a").to_string_lossy().to_string();
    let b = tmp.path().join("crash-b").to_string_lossy().to_string();
    let session = format!("sess-{}", uuid_ish());
    let sl = slug("crash");

    let source = plant(Some(&a), &home, &sl, &session, 9);

    // Exactly what a crash between step 2 (copy) and step 4 (rename) leaves:
    // a half-written temp beside a destination that does not exist.
    let dest_dir = projects_dir_for(Some(&b), &home).join(&sl);
    std::fs::create_dir_all(&dest_dir).unwrap();
    let corpse = dest_dir.join(format!(".{session}.jsonl.port-999999-1"));
    std::fs::write(&corpse, "{\"half\": tru").unwrap();

    // The destination is NOT a transcript store for this session…
    assert_eq!(locate_transcript(Some(&b), &home, &session), None);
    // …and the source still is.
    assert_eq!(
        locate_transcript(Some(&a), &home, &session).as_deref(),
        Some(source.as_path())
    );
    assert_eq!(transcript_events(&source).unwrap(), 9);

    // Re-running the port sweeps the corpse and completes.
    let outcome = port_transcript(Some(&a), Some(&b), &home, &session).unwrap();
    assert!(matches!(outcome, PortOutcome::Ported { events: 9, .. }), "{outcome:?}");
    assert!(!corpse.exists(), "a crashed port's temp is swept by the next one");
    assert!(source.is_file());
    assert_eq!(
        std::fs::read_to_string(outcome.destination().unwrap()).unwrap(),
        std::fs::read_to_string(&source).unwrap()
    );
    // And no temp survives a SUCCESSFUL port either.
    let leftovers: Vec<_> = std::fs::read_dir(&dest_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".port-"))
        .collect();
    assert!(leftovers.is_empty(), "left {leftovers:?}");
}

/// A port that cannot activate reports the failure and leaves BOTH stores as
/// they were — it never half-lands.
#[cfg(unix)]
#[test]
fn a_port_that_cannot_write_fails_loudly_and_changes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let home = claude_home();
    let a = tmp.path().join("ro-a").to_string_lossy().to_string();
    let b = tmp.path().join("ro-b").to_string_lossy().to_string();
    let session = format!("sess-{}", uuid_ish());
    let sl = slug("readonly");

    let source = plant(Some(&a), &home, &sl, &session, 5);
    let dest_dir = projects_dir_for(Some(&b), &home).join(&sl);
    std::fs::create_dir_all(&dest_dir).unwrap();
    std::fs::set_permissions(&dest_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    let err = port_transcript(Some(&a), Some(&b), &home, &session).unwrap_err();
    assert!(
        format!("{err}").contains("copying"),
        "the failure must name what it could not do: {err}"
    );

    std::fs::set_permissions(&dest_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(locate_transcript(Some(&b), &home, &session), None);
    assert_eq!(transcript_events(&source).unwrap(), 5);
}

/// NEWEST WINS. A stale copy from an earlier port never clobbers a longer one,
/// in either direction — and "newest" means "more events", because transcripts
/// are append-only, so the longer file holds the shorter one's history plus
/// more.
#[test]
fn newest_wins_when_the_destination_holds_a_stale_copy_from_an_earlier_port() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let home = claude_home();
    let a = tmp.path().join("stale-a").to_string_lossy().to_string();
    let b = tmp.path().join("stale-b").to_string_lossy().to_string();
    let session = format!("sess-{}", uuid_ish());
    let sl = slug("stale");

    // An earlier port left 3 events on B; A has since advanced to 11.
    let source = plant(Some(&a), &home, &sl, &session, 11);
    let stale = plant(Some(&b), &home, &sl, &session, 3);

    let outcome = port_transcript(Some(&a), Some(&b), &home, &session).unwrap();
    assert_eq!(
        outcome,
        PortOutcome::Ported {
            dest: stale.clone(),
            events: 11,
            replaced: Some(3),
        }
    );
    assert_eq!(transcript_events(&stale).unwrap(), 11);

    // Now the other way: the source is the SHORTER copy, so the destination is
    // kept untouched. This is the case that would silently roll a conversation
    // back if "newest wins" were not enforced on both sides of the port.
    std::fs::write(&source, body(&session, 4)).unwrap();
    let outcome = port_transcript(Some(&a), Some(&b), &home, &session).unwrap();
    assert_eq!(
        outcome,
        PortOutcome::Kept {
            dest: stale.clone(),
            dest_events: 11,
            source_events: 4,
        }
    );
    assert_eq!(transcript_events(&stale).unwrap(), 11, "never rolled back");

    // Equal lengths also keep the destination — a port must not churn a file
    // for nothing.
    std::fs::write(&source, body(&session, 11)).unwrap();
    let outcome = port_transcript(Some(&a), Some(&b), &home, &session).unwrap();
    assert!(matches!(outcome, PortOutcome::Kept { .. }), "{outcome:?}");
}

/// The locator SCANS the project dirs; it does not re-derive the cwd slug. When
/// a thread's cwd changed mid-life and two slug dirs hold the same session, the
/// longer one is the live conversation.
#[test]
fn locating_a_transcript_scans_every_project_dir_and_prefers_the_longer_one() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let home = claude_home();
    let a = tmp.path().join("scan-a").to_string_lossy().to_string();
    let session = format!("sess-{}", uuid_ish());

    assert_eq!(locate_transcript(Some(&a), &home, &session), None, "no store yet");

    let one = plant(Some(&a), &home, &slug("scan-one"), &session, 2);
    assert_eq!(locate_transcript(Some(&a), &home, &session).as_deref(), Some(one.as_path()));

    let two = plant(Some(&a), &home, &slug("scan-two"), &session, 6);
    assert_eq!(
        locate_transcript(Some(&a), &home, &session).as_deref(),
        Some(two.as_path()),
        "the longer transcript is the live conversation"
    );
}

// ── 2. budget-trip failover ─────────────────────────────────────────────────

/// **The failover.** A thread whose account trips a threshold now PORTS and
/// continues on an admissible sibling instead of parking until the reset.
///
/// This supersedes round-7 item 7 ("a thread with history parks for
/// continuity"). What that rule protected — the conversation — is the file this
/// test watches move.
#[tokio::test]
async fn a_trip_fails_the_thread_over_and_the_conversation_goes_with_it() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let home = claude_home();
    // CON-27: the thread starts on `aa-first`, an account threads owns. It used
    // to start on the machine `default`, which placement can no longer name.
    let m = build_manager(config_at(tmp.path(), &["aa-first", "zz-second"])).unwrap();
    let first = tmp.path().join("aa-first").to_string_lossy().to_string();
    let second = tmp.path().join("zz-second").to_string_lossy().to_string();

    let mut s = settings(tmp.path());
    s.thresholds.session = Some(50.0);
    let id = m.create_thread(None, s).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "aa-first");

    // One turn, so the thread has conversation history on `aa-first`…
    m.send(&id, "hello".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(eventually(|| !m.store.thread_accounts(&id).unwrap().is_empty()).await);
    assert_eq!(m.store.thread_accounts(&id).unwrap(), vec!["aa-first".to_string()]);

    // …and a real transcript on `aa-first`'s store to prove it moves.
    let sl = slug("failover");
    let source = plant(Some(&first), &home, &sl, &id, 12);

    // Its account trips; the sibling has room.
    observe(&m, "aa-first", 90.0);
    observe(&m, "zz-second", 4.0);

    let seen = watch(&m);
    m.send(&id, "and again".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();

    assert!(
        eventually(|| m.serving_account(&id).unwrap() == "zz-second").await,
        "a trip is exactly the moment stickiness yields: {:?}",
        m.serving_account(&id)
    );
    // The CONVERSATION moved, byte for byte, and the source still exists.
    let ported = locate_transcript(Some(&second), &home, &id).expect("the conversation ported");
    assert_eq!(transcript_events(&ported).unwrap(), 12);
    assert_eq!(
        std::fs::read_to_string(&ported).unwrap(),
        std::fs::read_to_string(&source).unwrap()
    );
    assert!(source.is_file(), "the source is never deleted");

    // Auditable, durable, and it RAN rather than parking.
    assert!(
        eventually(|| saw(&seen, "custom:transcript_ported")).await,
        "the port is on the ledger"
    );
    assert!(eventually(|| saw(&seen, "account_switched")).await);
    assert!(eventually(|| saw(&seen, "turn_ended")).await, "{:?}", seen.lock().unwrap());
    assert!(!saw(&seen, "turn_parked"), "an admissible sibling means no park");
    assert_eq!(m.park_state(&id).unwrap(), serde_json::Value::Null);
    assert_eq!(
        m.store.get_thread(&id).unwrap().unwrap().account.as_deref(),
        Some("zz-second")
    );
    // The destination is recorded as history, so the continuity gate treats the
    // thread as living there from now on.
    assert!(m.store.thread_accounts(&id).unwrap().contains(&"zz-second".to_string()));
}

/// A thread NOBODY admits still parks — and the work is never dropped.
///
/// Parking stays the terminal state; failover only removes the case where a
/// sibling would have taken the turn. With every account over the threshold
/// there is nowhere to go, so the prompt waits, whole, and resumes by itself
/// when usage clears.
#[tokio::test]
async fn a_thread_no_account_admits_still_parks_and_the_work_is_never_dropped() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-first", "zz-second"])).unwrap();
    // The resume sweep is what makes a park a WAIT rather than a stop; the
    // daemon starts it from `serve()`/`recover()`, and this test drives the
    // manager directly.
    m.start_watchers();

    let mut s = settings(tmp.path());
    s.thresholds.session = Some(50.0);
    let id = m.create_thread(None, s).unwrap();

    m.send(&id, "hello".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(eventually(|| !m.store.thread_accounts(&id).unwrap().is_empty()).await);

    // EVERY account is over the ceiling.
    observe(&m, "aa-first", 91.0);
    observe(&m, "zz-second", 92.0);

    let seen = watch(&m);
    m.send(&id, "and again".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();

    assert!(
        eventually(|| saw(&seen, "turn_parked")).await,
        "with nowhere to go the thread must PARK: {:?}",
        seen.lock().unwrap()
    );
    assert!(!saw(&seen, "account_switched"), "there was no admissible sibling");
    assert_eq!(m.serving_account(&id).unwrap(), "aa-first");
    assert_ne!(m.park_state(&id).unwrap(), serde_json::Value::Null);
    // CON-4: parked, never dropped — the prompt is still waiting in the queue.
    assert!(
        !m.store.waiting_queue(&id).unwrap().is_empty(),
        "a parked turn's work stays exactly where it was"
    );

    // …and when a sibling clears, the resume sweep fails it over on its own.
    observe(&m, "zz-second", 3.0);
    assert!(
        eventually(|| m.serving_account(&id).unwrap() == "zz-second").await,
        "a park is a wait, not a terminal state"
    );
    assert!(eventually(|| saw(&seen, "turn_ended")).await);
}

/// A PIN is a pin: a pinned thread parks rather than failing over.
#[tokio::test]
async fn a_pinned_thread_parks_instead_of_failing_over() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-first", "zz-second"])).unwrap();

    // CON-27: a pin names a placeable account. Pinning the machine default is
    // refused and degrades to an ordinary placement — `a_pin_on_the_machine_
    // default_is_refused_and_placed_automatically` in
    // `default_out_of_placement.rs` owns that behaviour.
    let mut s = settings(tmp.path());
    s.thresholds.session = Some(50.0);
    s.account = AccountChoice::Named("aa-first".to_string());
    let id = m.create_thread(None, s).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "aa-first");

    // The pinned account trips; the sibling is wide open and would admit.
    observe(&m, "aa-first", 90.0);
    observe(&m, "zz-second", 2.0);

    let seen = watch(&m);
    m.send(&id, "hello".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();

    assert!(
        eventually(|| saw(&seen, "turn_parked")).await,
        "a pinned thread waits for ITS account: {:?}",
        seen.lock().unwrap()
    );
    assert!(!saw(&seen, "account_switched"), "a pin is never re-placed");
    assert_eq!(m.serving_account(&id).unwrap(), "aa-first");
}

/// **Failover never dodges a budget.** A budget meters the POOL and follows the
/// thread across accounts by construction, so a budget trip is not a property of
/// the account and moving cannot clear it.
///
/// Both halves are checked: the direct case (the gate parks with cause
/// `budget`, which is not a failover cause at all) and the sharp one — a
/// CONTINUITY park, which IS a failover cause, whose second ask must still hit
/// the budget check and refuse.
#[tokio::test]
async fn failover_never_dodges_a_pooled_budget() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-first", "zz-second"])).unwrap();

    // A cash budget over the whole POOL. The fixture's turn costs $0.01, so the
    // first turn exhausts a half-cent limit and the second one meets a tripped
    // budget with a wide-open sibling account beside it.
    m.budgets
        .set_config(&threads::BudgetConfig {
            key: "pooled".into(),
            parent_key: None,
            session_pct: None,
            weekly_pct: None,
            fable_pct: None,
            cash_usd: Some(0.005),
            accounts: vec!["aa-first".into(), "zz-second".into()],
            on_trip: Default::default(),
            disabled: false,
            disabled_at: None,
        })
        .unwrap();

    let mut s = settings(tmp.path());
    s.budget_key = Some("pooled".into());
    let id = m.create_thread(None, s).unwrap();
    let on = m.serving_account(&id).unwrap();

    let seen = watch(&m);
    m.send(&id, "hello".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(eventually(|| saw(&seen, "turn_ended")).await, "the first turn spends the budget");

    // ── half 1: `budget` is not a failover cause ──
    m.send(&id, "again".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| saw(&seen, "turn_parked")).await,
        "an exhausted budget parks: {:?}",
        seen.lock().unwrap()
    );
    assert!(
        !saw(&seen, "account_switched"),
        "a budget follows the thread across accounts — moving buys nothing"
    );
    assert_eq!(m.serving_account(&id).unwrap(), on);
    let park = m.park_state(&id).unwrap();
    assert!(
        park["reason"].as_str().unwrap_or_default().contains('$'),
        "the park must be the CASH budget: {park}"
    );

    // ── half 2: the CONTINUITY path re-checks the budget on its second ask ──
    //
    // Directly against the gate: history on `aa-first`, `aa-first` over a
    // threshold, `zz-second` clear. Without a budget that is a failover; with
    // the exhausted budget it must still park, because the second ask runs the
    // budget check after it picks the target.
    //
    // CON-27: the pool is asked for, not written out. `AdmissionRequest` takes
    // `Placeable`, so there is no spelling of this fixture that puts the
    // machine default in front of the gate.
    observe(&m, "aa-first", 90.0);
    observe(&m, "zz-second", 2.0);
    let thresholds = threads::Thresholds {
        session: Some(50.0),
        ..Default::default()
    };
    let pool = m.placement_pool();
    let history = vec!["aa-first".to_string()];
    let free = threads::budgets::AdmissionRequest {
        budget_key: None,
        thresholds: &thresholds,
        accounts: &pool,
        current_account: "aa-first",
        pinned: false,
        history_accounts: &[],
        unavailable: &[],
        unhealthy: &[],
        demand: threads::budgets::Demand::default(),
    };
    assert!(
        matches!(
            m.budgets.admit(&free).unwrap(),
            threads::budgets::Admission::Admit { switch_to: Some(ref a) } if a == "zz-second"
        ),
        "with the conversation free to move and no budget, zz-second admits — \
         this is what the failover's second ask asks"
    );
    let keyed = threads::budgets::AdmissionRequest {
        budget_key: Some("pooled"),
        history_accounts: &history,
        ..free.clone()
    };
    let keyed_free = threads::budgets::AdmissionRequest {
        history_accounts: &[],
        ..keyed.clone()
    };
    // The first ask parks for CONTINUITY — an account-shaped cause, and so a
    // failover candidate. This is the case that would be a hole.
    match m.budgets.admit(&keyed).unwrap() {
        threads::budgets::Admission::Park { cause, .. } => assert_eq!(
            cause,
            threads::budgets::ParkCause::Continuity,
            "this test is only meaningful while the first ask LOOKS like a failover"
        ),
        other => panic!("expected a continuity park, got {other:?}"),
    }
    // …and the failover's second ask — the one that says "the conversation can
    // follow you" — still runs the budget check after it picks its target, so
    // it refuses. `failover_target` only ever moves a thread on an `Admit`.
    match m.budgets.admit(&keyed_free).unwrap() {
        threads::budgets::Admission::Park { cause, .. } => assert_eq!(
            cause,
            threads::budgets::ParkCause::Budget,
            "the failover's second ask must be stopped by the BUDGET"
        ),
        other => panic!("failover admitted past an exhausted pooled budget: {other:?}"),
    }
}

// ── 3. the external-transcript reconcile ────────────────────────────────────

/// The reconcile prefers the LONGER transcript — the one the external run
/// advanced — and merges it into the thread's current account store.
#[tokio::test]
async fn reconcile_prefers_the_longer_transcript() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let home = claude_home();
    // CON-27: one placeable account, so the reconcile lands somewhere
    // deterministic without the machine default being the answer.
    let m = build_manager(config_at(tmp.path(), &["aa-first"])).unwrap();
    let first = tmp.path().join("aa-first").to_string_lossy().to_string();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let account = m.serving_account(&id).unwrap();
    assert_eq!(account, "aa-first");

    let sl = slug("reconcile");
    // What the daemon has: 4 events. What the AUI fallback advanced while the
    // daemon was down, in a store of its own: 9.
    let resident = plant(Some(&first), &home, &sl, &id, 4);
    let external = plant(
        Some(&tmp.path().join("machine-login").to_string_lossy().to_string()),
        &home,
        &sl,
        &id,
        9,
    );

    let out = m
        .reconcile_transcript(&id, &external.to_string_lossy())
        .unwrap()
        .0;
    assert_eq!(out["outcome"], "ported");
    assert_eq!(out["reconciled"], true);
    assert_eq!(out["thread_id"], id.as_str());
    assert_eq!(out["account"], "aa-first");
    assert_eq!(out["destination"], resident.to_string_lossy().as_ref());
    assert_eq!(out["destination_events"], 9);
    assert_eq!(out["replaced_events"], 4);

    assert_eq!(transcript_events(&resident).unwrap(), 9);
    assert_eq!(
        std::fs::read_to_string(&resident).unwrap(),
        std::fs::read_to_string(&external).unwrap()
    );
    assert!(external.is_file(), "the external copy is never deleted");

    // Idempotent: handing the same path back answers "kept".
    let again = m
        .reconcile_transcript(&id, &external.to_string_lossy())
        .unwrap()
        .0;
    assert_eq!(again["outcome"], "kept");
    assert_eq!(again["reconciled"], false);

    // And a SHORTER external copy never rolls the daemon's store back.
    std::fs::write(&external, body(&id, 2)).unwrap();
    let shorter = m
        .reconcile_transcript(&id, &external.to_string_lossy())
        .unwrap()
        .0;
    assert_eq!(shorter["outcome"], "kept");
    assert_eq!(shorter["destination_events"], 9);
    assert_eq!(transcript_events(&resident).unwrap(), 9);
}

/// The reconcile refuses what it cannot identify rather than guessing.
#[tokio::test]
async fn reconcile_refuses_an_unknown_thread_or_a_missing_file() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // CON-27: an explicitly registered placeable account. This test only needs
    // SOME thread to refuse a reconcile against, but a thread has to be born
    // somewhere threads owns, and `&[]` leaves only the machine default — which
    // is no longer a place a thread can be created.
    let m = build_manager(config_at(tmp.path(), &["aa-first"])).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();

    let missing = tmp.path().join("nope.jsonl");
    let err = m.reconcile_transcript(&id, &missing.to_string_lossy()).unwrap_err();
    assert!(format!("{err}").contains("nothing to reconcile"), "{err}");

    let planted = plant(None, &claude_home(), &slug("unknown"), &id, 1);
    let err = m
        .reconcile_transcript("not-a-thread", &planted.to_string_lossy())
        .unwrap_err();
    assert!(!format!("{err}").is_empty());
}

/// A reconciled thread RESUMES rather than restarting: the destination now
/// holds the conversation, so the next daemon turn is not a first turn.
#[tokio::test]
async fn a_reconciled_thread_resumes_instead_of_restarting() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let home = claude_home();
    // CON-27: an explicitly registered placeable account. `&[]` used to mean
    // "just the machine default", which is no longer somewhere a thread can be
    // created.
    let m = build_manager(config_at(tmp.path(), &["aa-first"])).unwrap();
    let first = tmp.path().join("aa-first").to_string_lossy().to_string();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();

    let sl = slug("resume");
    let external = plant(
        Some(&tmp.path().join("elsewhere").to_string_lossy().to_string()),
        &home,
        &sl,
        &id,
        6,
    );
    let out = m
        .reconcile_transcript(&id, &external.to_string_lossy())
        .unwrap()
        .0;
    assert_eq!(out["outcome"], "ported");

    // The account's store holds it now — the thread has a session to resume.
    assert!(locate_transcript(Some(&first), &home, &id).is_some());
    // And the daemon records that this account carries the conversation.
    assert!(m.store.thread_accounts(&id).unwrap().contains(&"aa-first".to_string()));
}

/// `port_transcript_file` places an external copy under the destination's own
/// slug directory when the destination already knows one, and otherwise carries
/// the source's slug across.
#[test]
fn the_reconcile_destination_prefers_the_stores_own_slug_directory() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let home = claude_home();
    let dest = tmp.path().join("slugdest").to_string_lossy().to_string();
    let session = format!("sess-{}", uuid_ish());
    let external_slug = slug("ext");
    let resident_slug = slug("res");

    let external = plant(
        Some(&tmp.path().join("extstore").to_string_lossy().to_string()),
        &home,
        &external_slug,
        &session,
        8,
    );

    // No resident copy: the source's own slug comes across.
    let out = port_transcript_file(&external, Some(&dest), &home, &session).unwrap();
    assert_eq!(
        out.destination().unwrap(),
        projects_dir_for(Some(&dest), &home)
            .join(&external_slug)
            .join(format!("{session}.jsonl"))
    );

    // A resident copy under a DIFFERENT slug is the definitive destination —
    // the store has actually run this session there.
    let dest2 = tmp.path().join("slugdest2").to_string_lossy().to_string();
    let resident = plant(Some(&dest2), &home, &resident_slug, &session, 2);
    let out = port_transcript_file(&external, Some(&dest2), &home, &session).unwrap();
    assert_eq!(out.destination().unwrap(), resident.as_path());
    assert_eq!(transcript_events(&resident).unwrap(), 8);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
