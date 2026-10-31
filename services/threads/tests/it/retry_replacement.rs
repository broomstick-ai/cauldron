//! **A retrying thread waits on the ACCOUNT PICTURE, not on a clock.**
//!
//! `a3d6b86` made the 429 path *choose* through the one placement authority
//! instead of its own ranking. It left the *when* alone: `retry_at` was written
//! down from the reset prose of the account that had just refused, and
//! `arm_retry_timer` slept until that instant. Nothing re-evaluated placement in
//! between — not a limit expiring, not a login being revived, not an account
//! being added. A thread that could have run the moment some other login came
//! back instead sat until one particular account's clock ran out.
//!
//! What replaced it, in two steps. `3af681e` made `retry_at` a floor on the
//! ACCOUNT that refused and re-asked placement whenever an input to placement
//! moved. Then the operator removed the floor outright:
//!
//! > *"There should not be a per-thread retry timestamp."*
//! > *"if any thread is parked then the pool needs a retry, not the thread."*
//!
//! So a `retries` row is now a PARKED PROMPT and nothing else, the pool holds one
//! cadence for all of them, and `retries_to_reconsider` has no eligibility clause at
//! all. Two properties are load-bearing here and each has a test that fails if its
//! clause is removed:
//!
//! * **The re-ask CHOOSES nothing.** It answers "may this run now", and
//!   `admit_or_park` still answers "so where does this thread go" — which is how
//!   continuity survives, because that gate is the only thing that moves a
//!   thread with history and it moves it by porting the transcript (DEC-56).
//! * **Two labels for one login are not two accounts.** This is the clause the
//!   live incident turned on: `default` and `owner-account` share an
//!   identity fingerprint, so "the other account is free" was never true of them.
//!   The clause used to live in `retry_unblocked_early`, a floor-lifting pre-guess
//!   that is deleted with the floor; it now lives where every caller gets it, in
//!   `barred_accounts`, and that is where it is asserted below.
//!
//! Every test drives the manager in-process against the FAKE claude fixture. No
//! real claude turn is ever spawned; every manager gets an explicit `data_dir`
//! under a tempdir, and `THREADS_CLAUDE_HOME` points transcript storage at a
//! scratch dir, so nothing here can read or write inside the operator's own
//! `~/.claude`, the running daemon, or the keychain.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::accounts::AccountState;
use threads::event::{Push, StoredEvent};
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

/// A thread the gate will REFUSE: it carries a budget key that does not exist,
/// which parks on `unknown_key` — a cause no account change can clear, so
/// failover declines it and the thread is not ungated. The cheapest honest
/// refusal available.
fn refused_settings(cwd: &Path) -> ThreadSettings {
    let mut s = settings(cwd);
    s.budget_key = Some("no-such-budget-key".into());
    s
}

fn far_future() -> String {
    "2099-01-01T00:00:00+00:00".to_string()
}

/// Mark `name` as inside a usage-limit window until `until` (or clear it).
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

/// Give `name` a login identity fingerprint. Two labels handed the same string
/// are two names for ONE login.
fn identity(m: &ThreadManager, name: &str, fp: &str) {
    m.store.set_account_identity(name, None, Some(fp)).unwrap();
}

/// A parked prompt. It carries no instant, because the row cannot: when it is
/// reconsidered is a POOL-level question now.
fn retry_row(thread_id: &str, account: Option<&str>) -> RetryRow {
    RetryRow {
        thread_id: thread_id.into(),
        prompt: "hello".into(),
        message_ids: vec![1],
        attempt: 2,
        kind: "usage_limit".into(),
        reason: "rate limit (429)".into(),
        account: account.map(str::to_string),
        created_at: String::new(),
    }
}

fn reconsidered_ids(m: &ThreadManager) -> Vec<String> {
    m.retries_to_reconsider()
        .into_iter()
        .map(|r| r.thread_id)
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

fn count(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|e| e.event.kind() == kind)
        .count()
}

// ═════════════════════════════════════════════════════════════════════════
// 1. THE SOURCE GUARD — the re-ask asks, it does not choose
// ═════════════════════════════════════════════════════════════════════════

/// **The retry re-ask path must contain no placement of its own.**
///
/// This is the same ban `retry_placement::the_retry_path_contains_no_account_ranking`
/// puts on the 429 branch, extended to the mechanism that now WAKES a retry.
/// The re-ask is allowed to READ the account picture — that is the whole of its
/// job, deciding whether an input moved — but the moment it picks a target it
/// becomes a second placement implementation, and the first thing such a thing
/// does is move a thread with history without porting its conversation.
///
/// Continuity (DEC-56) is preserved structurally rather than by a rule anyone
/// has to remember: the re-ask cannot reassign a thread, because the only path
/// out of it to a different account runs through `admit_or_park`, and that gate
/// moves a thread with history only via `move_thread` with `port_must_succeed`.
#[test]
fn the_retry_reask_path_chooses_nothing() {
    let src = include_str!("../../src/runtime.rs");
    let start = src
        .find("    pub fn cadence_rung(")
        .expect("cadence_rung was renamed — re-point this test");
    let body = &src[start..];
    let end = body
        .find("    /// A retry the admission gate refused")
        .expect("the item after fire_retry moved — re-point this test");
    let body = &body[..end];
    let code: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("///"))
        .collect::<Vec<_>>()
        .join("\n");
    let body = code.as_str();

    assert!(
        body.contains("fn retry_sweep(") && body.contains("fn fire_retry("),
        "the slice must actually contain the re-ask mechanism"
    );
    assert!(
        body.contains("fn retries_to_reconsider("),
        "and the candidate set, which is now the whole of the selection"
    );

    for banned in [
        "select_failover",
        "try_switch_account",
        "failover_target",
        "PlacementRequest",
        "AdmissionRequest",
        ".place(",
        ".admit(",
        "switch_account(",
        "move_thread",
        "port_conversation",
    ] {
        assert!(
            !body.contains(banned),
            "the retry re-ask grew placement of its own ({banned:?}). It may decide WHETHER \
             to ask again; `admit_or_park` decides WHERE the thread goes, and it is what \
             carries a thread's conversation with it (DEC-56)."
        );
    }

    assert!(
        body.contains("admit_or_park"),
        "the re-ask must go through the same gate as every other turn"
    );
    // THE ORIGIN IS CHECKED BEFORE THE PROMPT IS HANDED OVER, so a QUESTION the gate
    // refuses cannot spend the prompt's place in `retries`. This read
    // `retry_floor_elapsed` and now reads the pass's origin, which is the whole of
    // the re-grounding: the pool's deadline hands over, a question never does.
    let park = body.find("park_retried_turn").expect("no handover in the slice");
    let guard = body[..park]
        .rfind("origin.is_deadline()")
        .expect("the handover is not guarded by the pass's origin");
    assert!(
        guard < park,
        "a refused QUESTION must keep the prompt in `retries` rather than park it — \
         only the pool's own deadline hands it over"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 2. AN ALIAS IS NEVER FREE CAPACITY — the live incident's clause, re-homed
// ═════════════════════════════════════════════════════════════════════════

/// **THE LIVE INCIDENT, AS A TEST. Two labels for one login are not two
/// accounts.**
///
/// The registry held three names and only two logins: `default` and
/// `owner-account` carry one identity fingerprint, `owner-gmail`
/// carries another. Only one label had a limit window written against it, so
/// reading the `accounts` table by label says the other is free — and it is not,
/// because a limit is a fact about the LOGIN.
///
/// A re-ask that believed the label would wake the thread every tick into the very
/// limit it was waiting out. For an ungated AUI thread that is not a cheap mistake:
/// the gate's park is downgraded to run-anyway, so each wake is a real turn fired at
/// an exhausted account.
///
/// # Re-homed 2026-10-06, and this is the point of keeping it here
///
/// This section used to hold SIX tests, all of them exercising
/// `retry_unblocked_early` — a cheap pre-guess at "is some other login free",
/// written to decide whether a per-thread floor could be lifted EARLY. The floor is
/// gone, so the question has no caller and the function is deleted; a prompt is now
/// reconsidered unconditionally and the GATE answers for real.
///
/// What must not be lost with it is the clause, and the clause was never really
/// about retries: it is `barred_accounts`, which widens every limit across every
/// label of a login through `with_aliases`. Asserting it HERE, in the file the
/// incident is documented in, is what stops it being quietly re-weakened by someone
/// who only reads this file — and asserting it on `barred_accounts` rather than on a
/// retry-path wrapper is what makes it true for every caller instead of for one.
///
/// FORCED RED, run and recorded: dropping `with_aliases` from `barred_accounts` gives
/// `an alias is the same LOGIN … counting it as free capacity is the inference that
/// left a coordinator thread parked for hours: unavailable=["aa-primary"]`.
#[tokio::test]
async fn an_alias_is_never_free_capacity() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["alias", "other", "aa-primary"])).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();

    // `aa-primary` and `alias` are ONE login; `other` is a second.
    identity(&m, "aa-primary", "d11af2d3.683974df");
    identity(&m, "alias", "d11af2d3.683974df");
    identity(&m, "other", "c127c516.5810305a");

    // Only ONE LABEL of the first login says it is cold, exactly as the live table
    // did.
    limit(&m, "aa-primary", Some(&far_future()));
    assert!(
        m.store
            .list_account_states()
            .unwrap()
            .iter()
            .any(|s| s.name == "alias" && s.limit_until.is_none()),
        "fixture: the alias label must look free when read BY LABEL, or this test is \
         asserting nothing"
    );

    let (unavailable, _unhealthy) = m.barred_accounts();
    assert!(
        unavailable.iter().any(|n| n == "alias"),
        "an alias is the same LOGIN, and a limit is a fact about a login — counting \
         it as free capacity is the inference that left a coordinator thread parked \
         for hours while looking like it had somewhere to go: unavailable={unavailable:?}"
    );
    assert!(
        unavailable.iter().any(|n| n == "aa-primary"),
        "and the label the window was written against is barred too, obviously"
    );
    assert!(
        !unavailable.iter().any(|n| n == "other"),
        "while a GENUINELY different login is not barred — otherwise the assertion \
         above would be satisfied by barring everything: unavailable={unavailable:?}"
    );

    // AND THE PARKED PROMPT IS STILL RECONSIDERED REGARDLESS, which is the ruling:
    // what the alias clause protects is the ANSWER the gate gives, never whether the
    // question is asked.
    m.store.upsert_retry(&retry_row(&id, Some("other"))).unwrap();
    assert_eq!(
        reconsidered_ids(&m),
        vec![id],
        "*any parked threads get checked at any opportunity* — there is no \
         eligibility clause left for an alias to fool"
    );
}

/// **Boot teaches a parked prompt which login refused it.**
///
/// The rows already on disk when the `account` column arrived are the ones that
/// matter most — they belong to threads that are stuck right now. Their subject is
/// knowable at exactly one moment: a retry row exists precisely because the gate did
/// NOT move the thread, so the account it is still sitting on is the account that
/// refused it.
///
/// This OUTLIVED the floor it was written for. It is no longer eligibility — nothing
/// gates on it — but `RetryStarving` has to be able to name the login a delivery is
/// piling up behind, and an unattributed row cannot.
///
/// FORCED RED, run and recorded: deleting the backfill loop from `recover` gives
/// `boot is where a parked prompt learns which login refused it: got None`.
#[tokio::test]
async fn boot_teaches_a_parked_prompt_which_login_refused_it() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["second", "aa-primary"])).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let sitting_on = m.serving_account(&id).unwrap();

    m.store.upsert_retry(&retry_row(&id, None)).unwrap();
    assert!(m.store.get_retry(&id).unwrap().unwrap().account.is_none());

    m.recover().await;

    assert_eq!(
        m.store.get_retry(&id).unwrap().unwrap().account,
        Some(sitting_on),
        "boot is where a parked prompt learns which login refused it"
    );
}
// ═════════════════════════════════════════════════════════════════════════
// 3. WHAT A QUESTION MAY AND MAY NOT SPEND
// ═════════════════════════════════════════════════════════════════════════

/// **A QUESTION the gate refuses keeps the prompt where it is.**
///
/// Most reconsiderations are questions: an event, or the backstop. A "no" to a
/// question is information, not a verdict — so the retry row, which is the prompt's
/// durable home and carries the attempt count, stays exactly where it is. Handing the
/// prompt to `parked_turns` on every refusal would erase the ladder on the first
/// event that happened to arrive while the gate was saying no.
///
/// FORCED RED, run and recorded: dropping the `!origin.is_deadline()` guard above
/// `park_retried_turn` gives `a refused QUESTION must NOT consume the retry row`.
#[tokio::test]
async fn a_question_the_gate_refuses_keeps_the_prompt() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["second", "aa-primary"])).unwrap();
    let id = m.create_thread(None, refused_settings(tmp.path())).unwrap();

    identity(&m, "aa-primary", "aaaa.1111");
    identity(&m, "second", "bbbb.2222");
    m.store.upsert_retry(&retry_row(&id, Some("aa-primary"))).unwrap();

    // The pool reconsiders it unconditionally, and this pass is a QUESTION…
    assert_eq!(reconsidered_ids(&m), vec![id.clone()]);
    m.retry_sweep(threads::runtime::PassOrigin::Event);

    // …and the gate refuses, because the key does not exist.
    assert!(
        eventually(|| m.park_state(&id).map(|p| !p.is_null()).unwrap_or(false)).await,
        "the gate should have parked this thread on its unknown key"
    );
    let row = m
        .store
        .get_retry(&id)
        .unwrap()
        .expect("a refused QUESTION must NOT consume the retry row — the prompt and \
                 the attempt count live there and the next reconsideration has to be \
                 able to ask again");
    assert_eq!(row.attempt, 2, "nor spend an attempt off the ladder");
    assert_eq!(row.prompt, "hello", "NOR LOSE THE PROMPT");
    assert!(
        m.store.list_parked_turns().unwrap().is_empty(),
        "and the prompt must not be handed to `parked_turns`: that belongs to the \
         pool's own deadline and to nothing else"
    );
}

/// **A refusal AT THE POOL'S DEADLINE still hands the prompt over.**
///
/// The counterpart, so the question path cannot be mistaken for a licence to stop
/// parking. `park_retried_turn`'s trigger was an ELAPSED FLOOR plus a refusal and is
/// now the POOL'S CADENCE plus a refusal — the same event in the new vocabulary: the
/// waiting is over and the answer is still no, so the prompt moves to the one place a
/// gate-refused prompt is re-dispatched from.
///
/// FORCED RED, run and recorded: making `PassOrigin::is_deadline` answer `false` for
/// `Cadence` gives `the pool's own deadline plus a refusal is the handover this path
/// has always done`.
#[tokio::test]
async fn a_refusal_at_the_pools_deadline_still_hands_the_prompt_over() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["second", "aa-primary"])).unwrap();
    let id = m.create_thread(None, refused_settings(tmp.path())).unwrap();

    identity(&m, "aa-primary", "aaaa.1111");
    m.store.upsert_retry(&retry_row(&id, Some("aa-primary"))).unwrap();

    m.retry_sweep(threads::runtime::PassOrigin::Cadence);

    assert!(
        eventually(|| m.store.get_parked_turn(&id).unwrap().is_some()).await,
        "the pool's own deadline plus a refusal is the handover this path has always \
         done"
    );
    assert_eq!(
        m.store.get_parked_turn(&id).unwrap().unwrap().prompt,
        "hello",
        "AND THE PROMPT ARRIVES INTACT: the hand-over is write-then-clear precisely \
         so nothing can be lost between the two homes"
    );
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "the retry row is cleared once the prompt has a new durable home"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 4. EXACTLY ONCE
// ═════════════════════════════════════════════════════════════════════════

/// **The stored prompt runs exactly once, however many things wake it.**
///
/// The pool reconsiders everything at every opportunity, and a retry row holds a
/// real prompt from the operator's own thread — running it twice would duplicate a
/// turn in his transcript. Three things race here: a boot, and two cadence-origin
/// sweeps. Exactly one turn may start.
///
/// Two mechanisms make that true and this test fails if either is removed. The
/// `retry_firing` set stops a sweep stacking a second fire on a thread it is already
/// re-asking, and `lock_turn` — which a retry takes like any other execution — stops
/// two sweeps both entering.
///
/// It is MORE load-bearing after the move to a pool cadence, not less: there is no
/// per-thread timer to coalesce against any more, so `retry_firing` and the lock are
/// the whole of the protection.
#[tokio::test]
async fn the_stored_prompt_runs_exactly_once() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["second", "aa-primary"])).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);

    identity(&m, "aa-primary", "aaaa.1111");
    identity(&m, "second", "bbbb.2222");
    m.store.upsert_retry(&retry_row(&id, Some("aa-primary"))).unwrap();

    // Boot (which no longer arms anything per thread) and two deadline passes,
    // together.
    m.recover().await;
    m.retry_sweep(threads::runtime::PassOrigin::Cadence);
    m.retry_sweep(threads::runtime::PassOrigin::Cadence);

    assert!(
        eventually(|| count(&seen, "turn_ended") >= 1).await,
        "the retry should have run"
    );
    // Past everything the racing paths could have queued.
    tokio::time::sleep(Duration::from_millis(600)).await;

    assert_eq!(
        count(&seen, "turn_started"),
        1,
        "the stored prompt is a real turn in the operator's thread; every \
         reconsideration path together may start it once and only once"
    );
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "a retry that succeeded clears its row"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
