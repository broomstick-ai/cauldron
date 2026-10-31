//! **AN ADOPTED TURN READS THE PROMPT BACK, AND LANDS IT DURABLY.**
//!
//! Operator, verbatim: *"Why would my message ever get dropped? Like, there's
//! the thread history. I don't understand why we have this concern."*
//!
//! He was right. `adopt_turn` used to carry a comment claiming a mid-turn
//! usage-limit kill was impossible on a re-adopted execution because *"the
//! operator's message would be gone, with no durable home to be redelivered
//! from."* **That sentence was false.** The message is on disk twice from the
//! moment a turn goes running:
//!
//! * the `events` ledger, kind `user_message`, written by `send_declaring`
//!   BEFORE dispatch;
//! * `queue.text` — `mark_delivered` flips `state` and does not clear the text.
//!
//! Neither table is ever deleted from; `store.rs` says so itself. The real
//! defect was narrow and mechanical: `park_killed_turn` and `schedule_retry`
//! take `prompt: &str`, so they are downstream of an in-memory `String` that
//! `run_turn` owns and `adopt_turn` never had. A MISSING SELECT, not a
//! data-loss risk — and it is closed by persisting the prompt beside the
//! `running_turns` row the adopter already reads.
//!
//! Two defects fall to the same column, and this file owns both:
//!
//! 1. **The declined `limit_kill`** — a turn in flight across a daemon restart
//!    was never interrupted mid-turn and had to take its own 429.
//! 2. **CON-4, on the adopted budget path** — `adopt_turn`'s budget branch
//!    SIGTERM'd and then fell through to `finish_turn` with no park at all, so
//!    an adopted turn's prompt really was dropped on a budget kill. CON-4,
//!    verbatim: *"A budget or threshold trip must PARK the turn and surface it,
//!    never discard it… killing ends the execution, it does not lose the
//!    operator's prompt."*
//!
//! ## THE ACCEPTED TRADE, stated once
//!
//! Re-delivering an adopted turn re-runs a turn that already burned tokens. That
//! is not new: `park_killed_turn` has done exactly this on every budget kill
//! since CON-4. An execution torn down mid-flight has no partial result to
//! resume from, so the only two options are re-run it or lose the operator's
//! message, and CON-4 settles which of those is acceptable.
//!
//! ## THE `None` FALLBACK IS A TEST, NOT AN OVERSIGHT
//!
//! `prompt` is NULLABLE. A `running_turns` row written by the daemon that is
//! live right now has no prompt in it, and after this build lands that row must
//! behave exactly as it did before — decline, and say so. That makes the
//! limitation a ONE-RESTART transient instead of a permanent contract, and
//! `an_adopted_turn_with_no_stored_prompt_declines_exactly_as_before` is what
//! stops a later agent from "simplifying" the fallback away.
//!
//! Every test here drives the real manager in-process. No real claude is ever
//! spawned: the "child" is a `sleep` this test owns, put in its own process
//! group exactly as `run_once` puts a real one, so the production teardown
//! (`killpg`) reaches it and reaches nothing else.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::budgets::{BudgetConfig, TripAction};
use threads::event::{Push, StoredEvent};
use threads::settings::ThreadSettings;
use threads::store::{EstimateRow, RunningTurnRow};
use threads::{build_manager, Config, ThreadManager};

// ── fixtures ─────────────────────────────────────────────────────────────

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

/// Accounts threads OWNS (CON-27: the machine default is never placeable).
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
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn settings(cwd: &Path, account: &str) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s.account = threads::settings::AccountChoice::Named(account.into());
    s
}

/// Make this account's `/usage` report an exhausted weekly pool — the reading
/// that raises `limit_kill` with no 429 anywhere.
fn weekly_reads(dir: &Path, account: &str, pct: u32) {
    std::fs::write(dir.join(account).join("usage-weekly"), pct.to_string()).unwrap();
}

fn calibrate(m: &ThreadManager, account: &str, axis: &str, tpp: f64) {
    m.store
        .upsert_estimate(&EstimateRow {
            account: account.into(),
            axis: axis.into(),
            tokens_per_percent: tpp,
            sample_count: threads::gauge::PRIOR_SAMPLES as u64,
            last_obs_id: 0,
            updated_at: chrono::Utc::now().to_rfc3339(),
        })
        .unwrap();
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

/// What [`watch_adoption`]'s latch decided.
const ADOPTION_PENDING: u8 = 0;
/// `recover` re-adopted the orphan: the thread's first `Liveness` with
/// `parent_running` arrived before any `turn_interrupted` / `turn_started`.
const ADOPTED: u8 = 1;
/// `recover` did NOT adopt: a `turn_interrupted` or `turn_started` for the
/// thread came first. That is the non-adopted path (`recover` interrupts a
/// row whose pid is gone, and the retry ladder starts a FRESH turn).
const NOT_ADOPTED: u8 = 2;

/// A LATCH on "`adopt_turn` started for `id`": its first act is a
/// `Liveness` beat with `parent_running` for the thread, and the latch keeps
/// it.
///
/// **Why not `is_locked` (R9.F2e).** The lock is a WINDOW, not a fact: it is
/// held only while the adopted turn runs. When the burn is already in the
/// output file the whole adopted turn — read, trip, SIGTERM, one 20 ms
/// `tail_poll`, the pid reads dead (a zombie counts as dead), park, unlock —
/// can finish in ~40 ms. Under load the test task was descheduled across that
/// window, polled `is_locked` for 15 s after the lock was already gone, and
/// failed ~1 run in 3 ("the orphan was re-adopted", :421). A latch subscribed
/// BEFORE `recover` cannot miss it, however late the test looks.
///
/// **Why it DISARMS (R9.L2c, inspect F-1).** A `Liveness{parent_running}` is
/// not specific to adoption: `run_turn` emits the same frame. With adoption
/// broken, `recover` emits `turn_interrupted`, the retry ladder starts a fresh
/// `run_turn`, and its frame used to trip the latch — so the test went red
/// LATER, at an assertion that named the wrong cause (budget never tripped),
/// or not at all if the fresh turn happened to trip it. The broadcast is
/// ordered, and on the non-adopted path a `turn_interrupted` (then a
/// `turn_started`) for the thread always precedes the fresh turn's frame,
/// while `adopt_turn` emits its frame before either. So the FIRST of those
/// three for the thread decides, and the test asserts on that decision.
fn watch_adoption(m: &ThreadManager, id: &str) -> Arc<std::sync::atomic::AtomicU8> {
    let mut rx = m.subscribe();
    let verdict = Arc::new(std::sync::atomic::AtomicU8::new(ADOPTION_PENDING));
    let out = verdict.clone();
    let id = id.to_string();
    tokio::spawn(async move {
        loop {
            let decided = match rx.recv().await {
                Ok(Push::Liveness {
                    thread_id,
                    parent_running: true,
                    ..
                }) if thread_id == id => ADOPTED,
                Ok(Push::Event(ev))
                    if ev.thread_id == id
                        && matches!(ev.event.kind(), "turn_interrupted" | "turn_started") =>
                {
                    NOT_ADOPTED
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            };
            out.store(decided, std::sync::atomic::Ordering::SeqCst);
            return;
        }
    });
    verdict
}

/// Wait for the latch to decide, and fail HERE — naming the cause — unless it
/// decided the orphan was adopted.
async fn assert_adopted(
    verdict: &Arc<std::sync::atomic::AtomicU8>,
    seen: &Arc<Mutex<Vec<StoredEvent>>>,
) {
    let load = || verdict.load(std::sync::atomic::Ordering::SeqCst);
    eventually(|| load() != ADOPTION_PENDING).await;
    assert_eq!(
        load(),
        ADOPTED,
        "the orphan was re-adopted (latch = {}: 1 adopted, 0 undecided, 2 recover interrupted \
         it and a fresh turn ran instead); ledger {:?}",
        load(),
        seen.lock().unwrap().iter().map(|e| e.event.kind()).collect::<Vec<_>>()
    );
}

fn saw(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> bool {
    seen.lock().unwrap().iter().any(|e| e.event.kind() == kind)
}

/// A stand-in for the previous daemon's still-running child.
///
/// `process_group(0)` is not decoration: `run_once` does exactly this so a
/// teardown can `killpg` the whole tree, and without it here the production
/// teardown would aim `killpg` at a group id that is not this process's — it
/// would signal nothing, and the test would be asserting against a code path it
/// never actually exercised.
fn orphan_child() -> std::process::Child {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("30");
    cmd.process_group(0);
    cmd.spawn().expect("spawn the stand-in child")
}

/// The line the fake-claude `bigburn` branch streams: one assistant message
/// carrying an enormous `usage` block. Fed through the adopted turn's own
/// `process_line`, it is what charges the budget mid-flight.
const BURN_LINE: &str = r#"{"type":"assistant","message":{"id":"msg_fake_burn","model":"claude-fable-5","content":[{"type":"text","text":"burning"}],"usage":{"input_tokens":5000000,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#;

/// Record an in-flight turn for `id`, as the previous daemon would have left it.
#[allow(clippy::too_many_arguments)]
fn record_in_flight(
    m: &ThreadManager,
    id: &str,
    pid: i64,
    out_path: &Path,
    prompt: Option<&str>,
    message_ids: Vec<u64>,
    attempt: u32,
) {
    m.store
        .upsert_running_turn(&RunningTurnRow {
            thread_id: id.to_string(),
            pid,
            spawn_cookie: "cookie".into(),
            output_path: out_path.to_string_lossy().to_string(),
            byte_offset: 0,
            started_at: threads::store::now_rfc3339(),
            prompt: prompt.map(str::to_string),
            message_ids,
            attempt,
        })
        .unwrap();
}

// ═════════════════════════════════════════════════════════════════════════
// 1. THE USAGE-LIMIT INTERRUPT, ON THE ADOPTED PATH
// ═════════════════════════════════════════════════════════════════════════

/// **An adopted turn with a stored prompt honours `limit_kill` and lands the
/// prompt durably.**
///
/// One account, so there is nowhere to fail over to and the RETRY LADDER — not
/// a lucky re-placement — is what has to hold the work. The landing is the same
/// one `run_turn`'s 429 branch reaches, via the same
/// `land_on_the_usage_limit_ladder`: `retries` is the prompt's durable home, and
/// `fire_retry` re-enters `admit_or_park` (audit F1), which is the ONE
/// admission authority. Nothing here places anything.
///
/// Forced to fail before it was trusted, twice:
/// * making `adopt_turn` read `row.prompt` as `None` unconditionally leaves the
///   child alive and writes no retry row — the pre-fix behaviour, exactly;
/// * deleting the `land_on_the_usage_limit_ladder` call leaves `turn_killed` on
///   the ledger with NO durable home for the prompt, which is the CON-4
///   violation the old comment was (rightly) afraid of.
#[tokio::test(flavor = "multi_thread")]
async fn an_adopted_turn_honours_the_limit_kill_and_lands_the_prompt() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let seen = watch(&m);
    let id = m.create_thread(None, settings(tmp.path(), "solo")).unwrap();

    let mut child = orphan_child();
    let out_path = tmp.path().join("orphan.jsonl");
    std::fs::write(&out_path, "").unwrap();
    record_in_flight(
        &m,
        &id,
        child.id() as i64,
        &out_path,
        Some("the operator's words, verbatim"),
        vec![7],
        2,
    );

    // R9.L2c (inspect F-1): the latch, not `is_locked` — a retry's fresh turn
    // holds the lock too, so `is_locked` passed with adoption broken.
    let adopted = watch_adoption(&m, &id);
    m.recover().await;
    assert_adopted(&adopted, &seen).await;

    // The 19:26:13 reading: the account this turn is running on is exhausted,
    // with no 429 anywhere.
    weekly_reads(tmp.path(), "solo", 100);
    m.probe_accounts_once(true).await.unwrap();

    assert!(
        eventually(|| saw(&seen, "turn_killed")).await,
        "the adopted turn was torn down: {:?}",
        seen.lock().unwrap().iter().map(|e| e.event.kind()).collect::<Vec<_>>()
    );
    assert!(eventually(|| saw(&seen, "turn_ended")).await, "and the turn closed");
    assert!(
        eventually(|| saw(&seen, "thread_system_interrupted")).await,
        "DEC-56: the thread is flagged, exactly as on the `run_once` path"
    );

    // ── CON-4: THE PROMPT HAS A DURABLE HOME ──
    let row = eventually(|| m.store.get_retry(&id).unwrap().is_some()).await;
    assert!(row, "a torn-down adopted turn's prompt lands on the retry ladder");
    let retry = m.store.get_retry(&id).unwrap().unwrap();
    assert_eq!(
        retry.prompt, "the operator's words, verbatim",
        "…verbatim, read back off the `running_turns` row"
    );
    assert_eq!(retry.message_ids, vec![7], "with the deliveries it answers");
    assert_eq!(
        retry.attempt, 3,
        "and it CONTINUES the ladder the killed execution was on (2), rather than \
         restarting it at 1 and re-buying the whole ceiling"
    );
    assert_eq!(
        retry.account.as_deref(),
        Some("solo"),
        "floored on the login that was barred, never on the thread"
    );

    // The child really was signalled — it would otherwise sleep for 30s.
    // Reaped through `try_wait` rather than `kill(pid, 0)`: this test OWNS the
    // process, so a signalled child is a zombie until we reap it and
    // `kill(pid, 0)` would still answer "alive".
    assert!(
        eventually(|| child.try_wait().map(|s| s.is_some()).unwrap_or(false)).await,
        "the re-adopted child is still its own process-group leader, so the same teardown \
         reaches it across a daemon restart"
    );
}

/// **An adopted turn with a NULL prompt declines exactly as before.**
///
/// The row every currently-live turn has. `prompt` is nullable precisely so
/// that this case exists and is honest: without a prompt there is nothing to
/// hand `schedule_retry`, and tearing the execution down anyway would be the
/// CON-4 violation. So it does not tear down — the turn discovers the limit the
/// old way, by taking its own 429 — and the limitation lasts exactly one
/// restart.
///
/// Forced to fail before it was trusted: making the `limit_kill` branch act
/// unconditionally (dropping the `recovered.is_none()` guard) kills the child
/// and emits `turn_killed` with no durable home for the prompt.
#[tokio::test(flavor = "multi_thread")]
async fn an_adopted_turn_with_no_stored_prompt_declines_exactly_as_before() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let seen = watch(&m);
    let id = m.create_thread(None, settings(tmp.path(), "solo")).unwrap();

    let mut child = orphan_child();
    let out_path = tmp.path().join("orphan.jsonl");
    std::fs::write(&out_path, "").unwrap();
    // A PRE-MIGRATION ROW: no prompt, no message ids, attempt at the column
    // default.
    record_in_flight(&m, &id, child.id() as i64, &out_path, None, vec![], 1);

    m.recover().await;
    assert!(eventually(|| m.is_locked(&id)).await, "the orphan was re-adopted");

    weekly_reads(tmp.path(), "solo", 100);
    m.probe_accounts_once(true).await.unwrap();
    // The account IS barred either way — the daemon acted on what it knew. What
    // it did NOT do is tear down an execution it could not land.
    assert!(
        eventually(|| m
            .store
            .list_account_states()
            .unwrap()
            .iter()
            .any(|s| s.name == "solo" && s.limit_until.is_some()))
        .await,
        "the login is recorded as barred"
    );

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        !saw(&seen, "turn_killed"),
        "a row with no prompt must not be torn down — there is nowhere to land it"
    );
    assert!(
        child.try_wait().unwrap().is_none(),
        "…and the child is still running, exactly as before this column existed"
    );
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "nothing was scheduled, because nothing was killed"
    );

    child.kill().ok();
    child.wait().ok();
    assert!(eventually(|| !m.is_locked(&id)).await, "and it frees the thread on exit");
}

// ═════════════════════════════════════════════════════════════════════════
// 2. CON-4 ON THE ADOPTED BUDGET PATH
// ═════════════════════════════════════════════════════════════════════════

/// **The adopted budget-kill path PARKS.**
///
/// This is a PRE-EXISTING gap, not one the limit-kill work introduced:
/// `adopt_turn`'s budget branch signalled the process group and then fell
/// straight through to `finish_turn`, so the prompt of an adopted turn was
/// dropped on every budget kill. `run_once`'s twin has called
/// `park_killed_turn` since CON-4; this one could not, because it had no
/// prompt. The same `Option<String>` closes both.
///
/// The trip is driven the way the adopted loop actually sees usage: the burn
/// line is in the durable output file, `adopt_turn` tails it, `process_line` →
/// `charge_budget` → `check_trip_midturn` orders the teardown. Nothing is
/// simulated except the child.
///
/// Forced to fail before it was trusted: deleting the `park_killed_turn` call
/// leaves `budget_tripped` and `turn_killed` on the ledger and `parked_turns`
/// empty — which is the defect, reproduced exactly.
#[tokio::test(flavor = "multi_thread")]
async fn an_adopted_turn_killed_by_a_budget_parks_its_prompt() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["solo"])).unwrap();
    let seen = watch(&m);

    // 1% of the session axis = 1000 tokens; the key allows 1% and KILLS.
    calibrate(&m, "solo", "session", 1000.0);
    calibrate(&m, "solo", "weekly_all", 1000.0);
    let mut cfg = BudgetConfig::new("tiny");
    cfg.session_pct = Some(1.0);
    cfg.on_trip = TripAction::Kill;
    cfg.accounts = vec!["solo".into()];
    m.budgets.set_config(&cfg).unwrap();

    let mut s = settings(tmp.path(), "solo");
    s.budget_key = Some("tiny".into());
    let id = m.create_thread(None, s).unwrap();

    let mut child = orphan_child();
    // The output file ALREADY holds the burn — this is a turn that streamed its
    // usage before the daemon died, which is the whole reason `byte_offset`
    // exists.
    let out_path = tmp.path().join("orphan.jsonl");
    std::fs::write(&out_path, format!("{BURN_LINE}\n")).unwrap();
    record_in_flight(
        &m,
        &id,
        child.id() as i64,
        &out_path,
        Some("park me, do not drop me"),
        vec![41, 42],
        1,
    );

    let adopted = watch_adoption(&m, &id);
    m.recover().await;
    assert_adopted(&adopted, &seen).await;

    assert!(
        eventually(|| saw(&seen, "budget_tripped")).await,
        "the adopted turn's own stream charged the meter and tripped it: {:?}",
        seen.lock().unwrap().iter().map(|e| e.event.kind()).collect::<Vec<_>>()
    );
    assert!(
        eventually(|| saw(&seen, "turn_killed")).await,
        "and the teardown is on the ledger, as it is on the `run_once` path"
    );

    // ── CON-4 ──
    assert!(
        eventually(|| m.store.get_parked_turn(&id).unwrap().is_some()).await,
        "a budget kill PARKS the prompt; it never discards it"
    );
    let parked = m.store.get_parked_turn(&id).unwrap().unwrap();
    assert_eq!(parked.prompt, "park me, do not drop me", "verbatim");
    assert_eq!(parked.message_ids, vec![41, 42]);
    assert_eq!(parked.budget_key.as_deref(), Some("tiny"));
    assert!(
        eventually(|| child.try_wait().map(|s| s.is_some()).unwrap_or(false)).await,
        "and the execution really ended"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 3. THE FALSE COMMENT IS GONE
// ═════════════════════════════════════════════════════════════════════════

/// **The claim that the prompt has "no durable home" must not come back.**
///
/// It was wrong when it was written and it justified declining a teardown that
/// the operator had every right to expect. Leaving the fix in place while
/// leaving the wrong reasoning beside it is worse than either alone: the next
/// agent reads the comment, believes it, and re-derives the same refusal
/// somewhere else.
///
/// Forced to fail before it was trusted: pasting the old sentence back into
/// `adopt_turn` turns this red.
#[test]
fn the_adopter_no_longer_claims_the_prompt_has_nowhere_to_land() {
    let src = include_str!("../../src/runtime.rs");
    let body = src
        .split_once("async fn adopt_turn(")
        .expect("adopt_turn exists")
        .1;
    for gone in ["IS DELIBERATELY NOT READ HERE", "does NOT hold the prompt"] {
        assert!(
            !body.contains(gone),
            "the false justification is back in `adopt_turn`: {gone:?}. The operator's message \
             is on the `events` ledger (`user_message`) and in `queue.text` from before \
             dispatch, and neither is ever deleted from — it is also on the `running_turns` \
             row this function holds."
        );
    }
    // The old sentence IS still quoted in `adopt_turn`, deliberately: a reader
    // who remembers it has to be able to find it and find it REFUTED, or the
    // deletion just looks like the reasoning went missing. What is pinned is
    // that it never appears again as an assertion — only inside the paragraph
    // that calls it false.
    if let Some(at) = body.find("no durable home to be redelivered from") {
        let around = &body[at.saturating_sub(400)..(at + 400).min(body.len())];
        assert!(
            around.contains("That sentence was false"),
            "the old claim is being made again rather than quoted and refuted: {around:?}"
        );
    }
    // And the thing that replaced it is actually there.
    assert!(
        body.contains("state.limit_kill.swap(false"),
        "`adopt_turn` must read `limit_kill` — declining to is the defect"
    );
    assert!(
        body.contains("park_killed_turn"),
        "`adopt_turn`'s budget branch must PARK (CON-4), not fall through to `finish_turn`"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
