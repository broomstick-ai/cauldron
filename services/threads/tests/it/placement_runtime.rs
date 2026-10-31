//! The RUNTIME half of placement (harness artifact `placement-correctness-report`
//! §4) — the two edits `budgets.rs` deliberately left behind.
//!
//! `budgets.rs` learned to rank accounts properly at `8c708b9`, but nothing an
//! AUI thread does could reach that ranking, because both doors into it were
//! shut in `runtime.rs`:
//!
//! 1. **`resolve_account`'s `Auto` arm** took the alphabetically first entry of
//!    the selectable pool. That is the ONLY placement decision most threads ever
//!    get — a thread's account is sticky, so stickiness then held an arbitrary
//!    choice forever.
//! 2. **`admit_or_park`'s short-circuit** returned early for a thread with no
//!    budget key and no thresholds, which is every AUI thread, so the gate that
//!    now places threads was never reached by the threads that make up the whole
//!    deployment.
//!
//! And the invariant that bounds both: **placement is not gating.** A thread
//! with nothing to enforce still always runs — it may be MOVED by this path,
//! never stopped by it. The third test holds that line.
//!
//! Every test drives the manager in-process against the FAKE claude fixture. No
//! real claude turn is ever spawned, `THREADS_HOME` is never resolved (every
//! manager gets an explicit `data_dir` under a tempdir, so `~/.threads` cannot
//! come into existence), and nothing here touches the operator's login, the
//! keychain, or the AUI.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use threads::event::Push;
use threads::gauge::Axis;
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

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
    // test can read or write inside it (account moves now port transcripts).
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// A config over `dir` with `extra` registered accounts. The daemon no longer
/// registers a machine-default row at all (2026-08-27), so the pool is exactly
/// what is named here — every account a placement test reasons about has to be
/// listed, with its own login directory under `dir`.
///
/// The naming still carries the argument: an `aa-…` account and a `zz-…`
/// account, so the ALPHABETICALLY first account and the one placement should
/// choose are never the same account by accident.
fn config_at(dir: &std::path::Path, extra: &[&str]) -> Config {
    let mut accounts = std::collections::BTreeMap::new();
    for name in extra {
        // A config dir that need not exist: the fixture ignores
        // CLAUDE_CONFIG_DIR, and an unreadable dir simply yields no identity.
        accounts.insert(name.to_string(), dir.join(name).to_string_lossy().to_string());
    }
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        // These tests are about placement, not polling: a real edge poll would
        // add readings they do not reason about.
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

/// An AUI thread: `Auto` account, NO thresholds, and the INHERITED default
/// budget key. Nothing to enforce — which is exactly the shape both defects
/// were invisible for.
///
/// Since INTENT 72b "ungated" no longer means "no key". Every thread is
/// associated with a budget, and the fleet-wide default carries no ceiling on
/// any axis, so it attributes spend and can never refuse a turn. The property
/// this helper pins is therefore the one that actually matters to placement —
/// nothing here can stop a turn — not the absence of a key.
fn ungated(cwd: &std::path::Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    assert_eq!(
        s.budget_key.as_deref(),
        Some(threads::budgets::DEFAULT_BUDGET_KEY),
        "every thread is associated with a budget (INTENT 72b)"
    );
    // …and nothing gates it. Since 2026-08-15 an AUI thread DOES carry a
    // threshold — `Thresholds::weekly` defaults to
    // `DEFAULT_WEEKLY_THRESHOLD_PCT` so the fleet has margin below the cliff —
    // so `thresholds.is_empty()` is no longer the right question. The property
    // these tests need is that nothing can REFUSE this thread, and a threshold
    // on an axis the gauge has never reported cannot: CON-5 stands the gate
    // down on an unobserved axis, and every manager here runs with the probe
    // off and no observations at all.
    assert_eq!(
        s.thresholds.weekly,
        Some(threads::budgets::DEFAULT_WEEKLY_THRESHOLD_PCT),
        "an AUI thread carries the fleet-wide weekly threshold"
    );
    assert_eq!(s.thresholds.session, None);
    assert_eq!(s.thresholds.fable, None);
    assert_eq!(s.thresholds.cash, None);
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

fn durable_account(m: &ThreadManager, id: &str) -> Option<String> {
    m.store.get_thread(id).unwrap().unwrap().account
}

/// Record every event kind the firehose carries, from now on.
///
/// `turn_started`, `turn_ended` and `turn_parked` all emit with no message id,
/// and the ledger reader filters those out (`events_by_range` requires
/// `message_id IS NOT NULL`), so the firehose is where a test watches for them.
fn watch(m: &ThreadManager) -> Arc<Mutex<Vec<String>>> {
    let mut rx = m.subscribe();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Event(ev) = p {
                out.lock().unwrap().push(ev.event.kind().to_string());
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

// ── edit 1: `resolve_account`'s `Auto` arm ───────────────────────────────────

/// CREATE time is a placement, not an alphabetical pick.
///
/// This is the edit that matters most. A thread's account is STICKY, so the
/// account chosen here is the account the thread lives on for the rest of its
/// life — an AUI thread carries no key and no thresholds and therefore never
/// asks the gate to move it. `aa-first` sorts first and is 92% through its
/// session; `zz-second` sorts last and is at 4%. The old arm took the first
/// name and got it exactly backwards.
///
/// CON-27: the loaded account that sorts first used to be `default` itself, and
/// the whole test rode on the machine login being a candidate. It is not one any
/// more — it cannot be made into a `Placeable` at all — so the alphabetical trap
/// is now baited with `aa-first`, an account threads OWNS. The two pools say the
/// distinction out loud below: `default` is still VISIBLE, and still not
/// somewhere a thread may be put.
#[tokio::test]
async fn create_time_auto_places_the_thread_instead_of_taking_the_first_name() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-first", "zz-second"])).unwrap();
    assert_eq!(
        m.selectable_pool()
            .iter()
            .map(|p| p.name())
            .collect::<Vec<_>>(),
        vec!["aa-first", "zz-second"]
    );
    // This asserted `["aa-first", "default", "zz-second"]` — *"the machine
    // default is still visible, only placement excludes it"* — until
    // 2026-08-27, when the operator struck the visible/placeable split:
    // *"I do not want default even in the purview of the system."* The daemon's
    // account list IS the pool now, and the two lists are the same list.
    assert_eq!(
        m.account_pool(),
        vec!["aa-first", "zz-second"],
        "the daemon's account list is the pool, and the machine default is not in it"
    );

    observe(&m, "aa-first", 92.0);
    observe(&m, "zz-second", 4.0);

    let id = m.create_thread(None, ungated(tmp.path())).unwrap();

    assert_eq!(
        m.serving_account(&id).unwrap(),
        "zz-second",
        "`Auto` must PLACE the thread; the old arm took the alphabetically first name"
    );
    // Durably, and before a single turn has run — so the sticky account is the
    // placed one and there is nothing to correct later.
    assert_eq!(durable_account(&m, &id).as_deref(), Some("zz-second"));
    // The thread was placed on the SESSION readings alone, and it carries no
    // session threshold — so this is the "notional full account" fallback
    // doing its job. Without it every AUI thread is unrankable.
    //
    // Since 2026-08-15 an AUI thread does carry a WEEKLY threshold, and the
    // fallback has to keep working anyway: `reachable_tokens` no longer drops
    // the ungated axes just because some other axis has a ceiling. If it did,
    // the weekly default alone would blind placement to the 92% session above
    // and this assertion's account would flip back to `aa-first`.
    let th = ungated(tmp.path()).thresholds;
    assert_eq!(th.session, None, "no session ceiling to rank against");
    assert!(th.weekly.is_some(), "and a weekly one that must not suppress it");
}

// ── edit 2: `admit_or_park`'s short-circuit ──────────────────────────────────

/// A thread with nothing to enforce is still PLACED at admission.
///
/// The removed short-circuit (`budget_key.is_none() && thresholds.is_empty()`)
/// fired for every AUI thread on every turn, so a reading that arrived AFTER
/// create time could never be acted on at all: the thread sat on whatever
/// account it was born on until the daemon was restarted, forever.
///
/// Note the ordering — nothing is observed when the thread is created, so
/// create-time placement has no measured axis and the tie leaves it on
/// `aa-first`. The readings land afterwards, which is the only way to isolate
/// this edit from the one above.
///
/// CON-27: the account the tie lands on used to be `default`, because the
/// machine login sorted first among the candidates. It is no longer a candidate
/// at all, so the test registers `aa-first` — an account threads OWNS — to be
/// the account the thread is born on and then moved OFF. What the test is about
/// is unchanged: a reading that arrives after create time still has to move an
/// ungated thread.
#[tokio::test]
async fn an_ungated_thread_is_placed_at_admission_not_short_circuited_past_it() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-first", "zz-second"])).unwrap();

    let id = m.create_thread(None, ungated(tmp.path())).unwrap();
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "aa-first",
        "with nothing observed, an exact tie stays where it started"
    );

    observe(&m, "aa-first", 92.0);
    observe(&m, "zz-second", 4.0);

    let seen = watch(&m);
    m.send(
        &id,
        "hello".to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();

    assert!(
        eventually(|| m.serving_account(&id).unwrap() == "zz-second").await,
        "admission must place an ungated thread, not skip the gate for it: {:?}",
        m.serving_account(&id)
    );
    assert_eq!(durable_account(&m, &id).as_deref(), Some("zz-second"));
    assert!(
        eventually(|| saw(&seen, "account_switched")).await,
        "the move is auditable: {:?}",
        seen.lock().unwrap()
    );

    // And it RAN. Placement moved the turn; it did not hold it.
    assert!(
        eventually(|| saw(&seen, "turn_ended")).await,
        "the turn must complete on the account it was placed on: {:?}",
        seen.lock().unwrap()
    );
    assert!(!saw(&seen, "turn_parked"), "placement is not gating");
    assert_eq!(m.park_state(&id).unwrap(), serde_json::Value::Null);
}

// ── the invariant: an ungated thread ALWAYS runs ─────────────────────────────

/// Removing the short-circuit routed ungated threads through a gate that can
/// say NO. It is not allowed to, and this is the case where it otherwise would.
///
/// The gate is made to refuse: the ONE account threads owns is inside a
/// usage-limit window, so `admit` has a candidate it may not use and parks — a
/// refusal invented entirely by account bookkeeping, on a thread that carries no
/// budget key and no threshold and so has nothing that could possibly be
/// refused. Before the short-circuit was removed this thread ran; it must still
/// run. Every AUI thread has exactly this shape, so a regression here would be a
/// deployment-wide freeze.
///
/// CON-27 changed what "nothing to place it on" can MEAN, and this is the one
/// test in the file whose scenario moved rather than just its account names. It
/// used to say it with an EMPTY registry — no accounts at all, `ParkCause::Scope`
/// with "no accounts are registered". That sentence is no longer reachable from
/// a live thread: a thread cannot be CREATED when the placement pool is empty
/// (`create_thread` fails outright), so a manager with nothing placeable never
/// has a thread to run in the first place.
///
/// So the emptiness is moved one step in: the account IS registered and IS
/// placeable — it has to be, because the thread lives there and has to be able to
/// spawn there — and it is BARRED. Placement is handed a pool and finds nothing
/// in it that may serve a turn, which is the same refusal from the gate's point
/// of view and the shape a real deployment actually reaches (every login rate
/// limited at once). The invariant under test is untouched: the gate says no,
/// and the ungated thread runs anyway.
#[tokio::test]
async fn an_ungated_thread_still_runs_when_placement_has_nothing_to_place_it_on() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // CON-27: an account threads OWNS, with its own config dir under the
    // tempdir. `&[]` would leave only the machine `default`, and a thread can no
    // longer be born there at all.
    let cfg = config_at(tmp.path(), &["aa-owned"]);
    let m = build_manager(cfg.clone()).unwrap();
    let id = m.create_thread(None, ungated(tmp.path())).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "aa-owned");
    drop(m);

    // The same store, rehydrated by a manager that registers exactly the one
    // account the thread already sits on — and nothing else, not even the
    // machine default. Then that one account is put inside a usage-limit window
    // that does not expire, so the pool the gate sees is non-empty and entirely
    // unusable.
    let store = threads::Store::open(tmp.path().join("threads.db")).unwrap();
    store
        .upsert_account_state(&threads::accounts::AccountState {
            name: "aa-owned".to_string(),
            limit_until: Some("2099-01-01T00:00:00+00:00".to_string()),
            ..Default::default()
        })
        .unwrap();
    let mut registry = threads::accounts::Registry::default();
    registry.add(
        "aa-owned",
        Some(tmp.path().join("aa-owned").to_string_lossy().to_string()),
    );
    let pool = threads::pool::Pool::from_registry(&registry);
    let m = ThreadManager::new(store, registry, pool, cfg, tmp.path().to_path_buf());
    assert_eq!(m.account_pool(), vec!["aa-owned"]);
    assert_eq!(m.serving_account(&id).unwrap(), "aa-owned");

    // The gate now says PARK here: one candidate, barred.
    let (unavailable, unhealthy) = m.barred_accounts();
    assert_eq!(unavailable, vec!["aa-owned"], "the one candidate is barred");
    let pool = m.placement_pool();
    let req = threads::budgets::AdmissionRequest {
        budget_key: None,
        thresholds: &Default::default(),
        accounts: &pool,
        current_account: "aa-owned",
        pinned: false,
        history_accounts: &[],
        unavailable: &unavailable,
        unhealthy: &unhealthy,
        demand: threads::budgets::Demand::default(),
    };
    assert!(
        matches!(
            m.budgets.admit(&req).unwrap(),
            threads::budgets::Admission::Park { .. }
        ),
        "this test is only meaningful while the gate would refuse"
    );

    let seen = watch(&m);
    m.send(
        &id,
        "hello".to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();

    // …and the thread runs anyway.
    assert!(
        eventually(|| saw(&seen, "turn_ended")).await,
        "an ungated thread must ALWAYS run: placement is not gating — saw {:?}",
        seen.lock().unwrap()
    );
    assert!(
        !saw(&seen, "turn_parked"),
        "a thread with no budget and no thresholds has nothing that can be refused"
    );
    assert_eq!(m.park_state(&id).unwrap(), serde_json::Value::Null);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
