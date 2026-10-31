//! **THE SPAWN GUARD — a turn may never run as the operator himself.**
//!
//! `spawn.rs` turned `config_dir: None` into `cmd.env_remove("CLAUDE_CONFIG_DIR")`,
//! and an unset `CLAUDE_CONFIG_DIR` is `~/.claude` — the operator's own terminal
//! login, which threads was never given. `ThreadManager::config_dir_for`
//! answers `None` for any name the pool does not carry, so any turn that
//! reached the spawn site holding such a name spawned AS HIM: no error, no
//! park, no ledger event, a turn that simply succeeded on the wrong account.
//!
//! Its docstring argues that cannot happen — *"through the pool there is no
//! such name: an entry either names its login directory or is not an entry"* —
//! and that argument covers PLACEMENT while saying nothing about the SPAWN. The
//! first test here is the gap: a thread placed on a real pool account, and an
//! account list that no longer carries it by the time the turn runs. It needs
//! no reloadable pool to reproduce — the operator editing `accounts.toml` and
//! restarting is enough — and the reloadable pool (`PoolCell`) only widens the
//! window from "across a restart" to "mid-turn".
//!
//! **And the carve-out that makes this not a one-liner.** `None` is the
//! CORRECT answer for the machine default: that account IS "the variable is
//! unset". 151 of the 1,849 threads in the live store sit on it and spawn that
//! way every day (`docs/spawn-config-dir-guard.md` carries the census).
//! `reresolve_account` leaves them there deliberately rather than silently
//! reassigning them (DEC-56). The second test is those 151 threads: a guard
//! that read "None is always wrong" would park every one of them, which is a
//! far worse outage than the hole it closed.
//!
//! Every test drives the manager in-process against the FAKE claude fixture. No
//! real claude turn is ever spawned, `THREADS_HOME` is never resolved, and
//! `THREADS_CLAUDE_HOME` points the transcript store at a scratch dir so
//! nothing here can read or write inside the operator's own `~/.claude`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use threads::event::Push;
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
    // test can read or write inside it. This file spawns a machine-default
    // thread on purpose, so it is the file that needs this most.
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &std::path::Path, accounts: &[&str]) -> Config {
    let mut map = std::collections::BTreeMap::new();
    for name in accounts {
        map.insert(name.to_string(), dir.join(name).to_string_lossy().to_string());
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

/// An AUI thread: `Auto` account, nothing that can refuse it. This shape is the
/// entire deployed fleet, and it is the shape the hazard needed: an UNGATED
/// thread converts every gate refusal into "run anyway", so the admission gate
/// noticing that its account is not placeable does not stop it — it runs, and
/// before this guard existed it ran on `~/.claude`.
fn ungated(cwd: &std::path::Path) -> ThreadSettings {
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
                out.lock().unwrap().push(ev.event.kind().to_string());
            }
        }
    });
    seen
}

fn saw(seen: &Arc<Mutex<Vec<String>>>, kind: &str) -> bool {
    seen.lock().unwrap().iter().any(|k| k == kind)
}

/// Rewrite the account column of a thread row, in the store, behind the
/// manager's back.
///
/// This is not a contrivance: it is the state 151 live threads are in. Nothing
/// in today's code PLACES a thread on `default` — it cannot, the machine
/// default is not a pool entry — and yet those rows exist, because they were
/// placed before the pool did and `reresolve_account` deliberately leaves them
/// where they are rather than moving an account column without its transcript
/// (DEC-56). Writing the column directly is how a test reaches that state
/// without a time machine.
fn set_account_column(m: &ThreadManager, id: &str, account: &str) {
    let mut row = m.store.get_thread(id).unwrap().unwrap();
    row.account = Some(account.to_string());
    row.updated_at = threads::store::now_rfc3339();
    m.store.upsert_thread(&row).unwrap();
}

// ── the hazard, and the carve-out, on the SAME setup ────────────────────────
//
// Both tests below strand a thread on an account the pool does not carry, with
// nowhere to fail over to, and let it run. They differ in ONE thing: the name
// on the account column. That is the predicate, made visible — `aa-first` is
// refused and `default` is not, and no other difference between the two tests
// explains it.

/// **A thread whose account has left the pool is REFUSED A LOGIN — it does not
/// spawn on the operator's own.**
///
/// The scenario is one the operator has actually performed: an account is
/// removed from `accounts.toml` and the daemon restarts. The thread was placed
/// on `aa-first` while it existed, its account is STICKY (`reresolve_account`
/// keeps `Auto` threads where they are, unconditionally, since the 119-thread
/// boot move), and the pool that comes back no longer carries the name. So
/// `config_dir_for("aa-first")` answers `None`, the gate has nowhere to port
/// the thread to, and — because the thread is UNGATED, which every AUI thread
/// is — it used to be run ANYWAY.
///
/// Before the guard this test was RED in the worst possible way: `turn_ended`,
/// no park, a completed turn — which is what a turn spawned with
/// `CLAUDE_CONFIG_DIR` removed looks like from the outside. It ran on
/// `~/.claude`, the operator's own terminal login, and said nothing.
///
/// A reloadable pool is NOT required to reach this; it only widens the window
/// from "across a restart" to "mid-turn".
#[tokio::test]
async fn an_account_that_left_the_pool_is_refused_instead_of_spawning_as_the_operator() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();

    // ── the daemon that placed the thread ────────────────────────────────
    let id = {
        let m = build_manager(config_at(tmp.path(), &["aa-first"])).unwrap();
        let id = m.create_thread(None, ungated(tmp.path())).unwrap();
        assert_eq!(
            m.serving_account(&id).unwrap(),
            "aa-first",
            "the thread is placed on a REAL pool account — this hazard is not about a bad \
             placement, it is about a good placement going stale"
        );
        id
    };

    // ── the operator removes that account, and the daemon restarts ───────
    std::fs::write(tmp.path().join("accounts.toml"), "").unwrap();
    let m = build_manager(config_at(tmp.path(), &[])).unwrap();
    assert!(
        m.account_pool().is_empty(),
        "the pool no longer carries the account this thread lives on, and carries nothing \
         else either — so there is nowhere to port it to and the gate's failover cannot \
         quietly rescue this case"
    );
    // R4.S4b: the "stays parked QUIETLY" check below is about the resume
    // tick, and the tick only runs once the watchers are started — without
    // this line that check could not fail on the path it names.
    m.start_watchers();

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
        eventually(|| saw(&seen, "turn_parked")).await,
        "the turn must be REFUSED and parked, never spawned with CLAUDE_CONFIG_DIR unset: {:?}",
        seen.lock().unwrap()
    );
    assert!(
        !saw(&seen, "turn_ended"),
        "nothing may have been spawned at all — a `turn_ended` here is a turn that ran on \
         the operator's own login: {:?}",
        seen.lock().unwrap()
    );

    // The thread is still sitting on `aa-first`: stickiness (DEC-56 forbids
    // moving its account without carrying its transcript), and NO ACCOUNT WAS
    // SUBSTITUTED. Quietly picking a healthy account would be exactly that
    // forbidden move, done by the least appropriate component.
    assert_eq!(m.serving_account(&id).unwrap(), "aa-first");

    // The refusal NAMES what happened, on the wire, where a client reads it —
    // the point of the guard is to convert a silent wrong-account success into
    // something visible.
    let park = m.park_state(&id).unwrap();
    let reason = park["reason"].as_str().unwrap_or_default().to_string();
    assert!(
        reason.contains("aa-first") && reason.contains("CLAUDE_CONFIG_DIR"),
        "the park reason must name the account and the variable: {park}"
    );

    // CON-4: his words are KEPT. A gate park leaves the delivery queued, so the
    // message is still there to be sent the moment the account comes back.
    assert!(
        !m.store.waiting_queue(&id).unwrap().is_empty(),
        "a parked turn never drops the prompt"
    );

    // And it stays parked QUIETLY. `resume_sweep` re-asks the gate on a timer;
    // before the refusal was also placed at the gate, the spawn-site park was
    // cleared and re-dispatched every tick — a hot loop that emitted a
    // park/resume pair each time.
    let ticks = seen.lock().unwrap().iter().filter(|k| *k == "turn_parked").count();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let ticks_later = seen.lock().unwrap().iter().filter(|k| *k == "turn_parked").count();
    assert_eq!(
        ticks, ticks_later,
        "a latched park does not re-announce itself on every resume tick: {:?}",
        seen.lock().unwrap()
    );
}

/// **A machine-default thread still spawns with `CLAUDE_CONFIG_DIR` UNSET, and
/// that is correct.**
///
/// Identical setup to the test above — stranded off the pool, nowhere to port
/// to, ungated — with `default` on the account column instead of `aa-first`.
/// It must RUN.
///
/// This is the regression that matters more than the hole. 151 of the 1,849
/// threads in the live store sit on `default` (verified `sqlite3 -readonly`
/// against `lib/threads/threads.db`; `docs/spawn-config-dir-guard.md` carries
/// the census), and the machine default IS "the variable is unset": it has no
/// login directory to name, by construction (`pool::ClaudeAccount::new`), and
/// `reresolve_account` leaves those threads where they are because moving a
/// thread's account without its transcript is forbidden (DEC-56).
///
/// So the predicate is an IDENTITY, not a blanket ban on `None`, and this test
/// is what holds that line: make the guard strict — refuse every `None` — and
/// it goes red here, standing in for 151 live threads going silent at once.
#[tokio::test]
async fn a_machine_default_thread_still_spawns_with_no_config_dir() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();

    let id = {
        let m = build_manager(config_at(tmp.path(), &["aa-first"])).unwrap();
        let id = m.create_thread(None, ungated(tmp.path())).unwrap();
        // Strand it on the machine default: the state those 151 rows are in,
        // and one no code path can produce any more.
        set_account_column(&m, &id, "default");
        id
    };

    std::fs::write(tmp.path().join("accounts.toml"), "").unwrap();
    let m = build_manager(config_at(tmp.path(), &[])).unwrap();
    assert!(
        !m.account_pool().iter().any(|n| n == "default"),
        "the machine default is not a pool member — it is not excluded from the pool, it was \
         never expressible in one"
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

    assert!(
        eventually(|| saw(&seen, "turn_ended")).await,
        "a machine-default thread spawns with no config dir and always has: {:?}",
        seen.lock().unwrap()
    );
    assert!(
        !saw(&seen, "turn_parked"),
        "the guard must not park the 151 threads that legitimately run this way: {:?}",
        seen.lock().unwrap()
    );
    assert_eq!(
        m.park_state(&id).unwrap(),
        serde_json::Value::Null,
        "nothing is parked"
    );
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "default",
        "and it stays where it is — evicting it is the admission gate's job, by PORTING"
    );
}

// ── the predicate itself ─────────────────────────────────────────────────────

/// **THE PREDICATE, stated as asserts.**
///
/// Read through `spawn_login_for_test` for the reason `probe_account_for_test`
/// exists: the DECISION is the thing worth testing, and proving it through the
/// private path would mean observing how `claude` was actually spawned — which,
/// for the one case that matters, is an observation you can only make by
/// PERFORMING the bad spawn.
#[tokio::test]
async fn only_the_machine_default_may_spawn_with_no_login_directory() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-first"])).unwrap();
    let dir = tmp.path().join("aa-first").to_string_lossy().to_string();

    // A pool account with its directory: the ordinary case.
    assert_eq!(
        m.spawn_login_for_test("aa-first", Some(&dir)),
        Ok(Some(dir.clone()))
    );

    // A POOL ACCOUNT THAT RESOLVED TO NOTHING — the hazard, refused.
    assert!(
        m.spawn_login_for_test("aa-first", None).is_err(),
        "a pool account with no login directory must be refused, never spawned unset"
    );

    // An account the pool has never heard of — a rename, a delete, a pool
    // reload landing between placement and spawn. Refused for the same reason,
    // and this is the case a "pool members must have a directory" check would
    // have waved straight through.
    assert!(m.spawn_login_for_test("vanished", None).is_err());

    // The EMPTY STRING is not a directory. It is how the registry has always
    // spelled "machine default" in `config.toml`'s inline seeds, so accepting
    // it would reopen the hole through a different door.
    assert!(m.spawn_login_for_test("aa-first", Some("")).is_err());

    // THE CARVE-OUT: the machine default, and only it.
    assert_eq!(m.spawn_login_for_test("default", None), Ok(None));

    // …and the refusal says enough to act on.
    let err = m.spawn_login_for_test("vanished", None).unwrap_err();
    assert!(err.contains("vanished") && err.contains("CLAUDE_CONFIG_DIR"), "{err}");
}

// ── the login directory a turn spawns with is the POOL'S, as of the spawn ────

/// **A pool reload that moves an account's login directory reaches the NEXT
/// turn — the spawn never uses a directory cached at placement.**
///
/// The live incident, 2026-10-15 19:17:51Z: the operator signed `cc1` in, the
/// slot's manifest now named its login directory by a DIFFERENT STRING (the
/// store-side path through the `accounts` symlink, not the repo-side one).
/// Claude Code keys its keychain item on that string, so the old string's
/// credentials were dead and the new one's fresh. The pool reloaded, the
/// `/usage` probe used the new string and succeeded — and three turns spawned
/// seconds later with the OLD string, all `authentication_failed`, and the
/// retry ladder moved their messages to `failed`. The turn's
/// `CLAUDE_CONFIG_DIR` came from `ThreadState::config_dir`, resolved when the
/// thread was placed and never refreshed by a pool reload.
///
/// Modelled with the `authresult` fixture, which answers like a dead login
/// unless `auth-live` sits in the `CLAUDE_CONFIG_DIR` it was spawned with: the
/// old directory has no marker, the new one does. Only a spawn that reads the
/// pool at spawn time can succeed.
#[tokio::test]
async fn a_pool_reload_that_moves_a_login_dir_reaches_the_next_spawn() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-moved"])).unwrap();
    let id = m.create_thread(None, ungated(tmp.path())).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "aa-moved");

    // The old directory is a DEAD login (no `auth-live`); the new one works.
    let old = tmp.path().join("aa-moved");
    let new = tmp.path().join("aa-moved-signed-in");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&new).unwrap();
    std::fs::write(new.join("auth-live"), b"").unwrap();

    // The re-sign-in rewrites the registry underneath the daemon, and the
    // daemon reloads the pool.
    let reg = tmp.path().join("accounts.toml");
    let toml = std::fs::read_to_string(&reg).unwrap();
    let old_s = format!("\"{}\"", old.display());
    assert!(toml.contains(&old_s), "registry names the old dir: {toml}");
    std::fs::write(&reg, toml.replace(&old_s, &format!("\"{}\"", new.display()))).unwrap();
    assert!(m.reload_pool(), "the pool must see the moved login directory");
    assert_eq!(
        m.pool().login_dir("aa-moved"),
        Some(new.to_string_lossy().as_ref()),
        "the pool now names the new directory — the string the /usage probe uses"
    );

    let seen = watch(&m);
    m.send(
        &id,
        "authresult".to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();

    assert!(
        eventually(|| saw(&seen, "turn_ended") || saw(&seen, "retry_scheduled")).await,
        "the turn must run: {:?}",
        seen.lock().unwrap()
    );
    assert!(
        saw(&seen, "turn_ended") && !saw(&seen, "retry_scheduled") && !saw(&seen, "api_error"),
        "the turn must spawn with the pool's CURRENT login directory, not the one cached \
         at placement: {:?}",
        seen.lock().unwrap()
    );
}

// ── R4.S4b (HOLDS R17): the login refusal's hot loop ───────────────────────

/// **A LOGIN REFUSAL IS NOT RE-ASKED UNTIL SOMETHING IT DEPENDS ON MOVES** —
/// and when it moves, the parked turn runs.
///
/// The gate's own login check (`admit_or_park`, the ungated hatch above) only
/// covers a turn the gate PARKS. A thread with `budgeting_off` set bypasses the
/// gate entirely (`admit_or_park` returns `true` at its first line), so it
/// reaches the spawn guard every time: `refuse_unresolved_login` parks the
/// turn, `run_turn` breaks, `finish_turn` calls `maybe_dispatch`, the parked
/// row is redelivered, refused again — synchronously, forever. The same shape
/// as the projection refusal's hot loop (R4.S4a inspect finding 1), which
/// `ThreadState::projection_hold` gates; the login twin was left ungated.
///
/// Measured before the fix (this test, gate removed): see
/// an internal report — thousands of `turn_parked` in 2 s.
///
/// The longest period on the path is the placement backstop tick
/// (`THREADS_RESUME_TICK_MS` = 150 ms here); the 2 s window outlasts it
/// thirteen times. The hold itself is 30 s (`THREADS_PROJECTION_HOLD_MS`
/// default), longer than the window, so exactly one park may exist.
///
/// Opposition (doctrine 2): a gate that never redelivers passes the first
/// half, so the second half puts the account BACK in the pool — the input the
/// refusal depends on moves — and the parked turn must run and end.
///
/// **THE MUTATION.** In `runtime.rs::refuse_unresolved_login`, delete the line
/// that arms `projection_hold` with `login_fingerprint`. Or, for the second
/// half, make `maybe_dispatch`'s gate ignore a fingerprint change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_login_refusal_is_parked_once_and_runs_when_the_account_returns_2s_outlasts_the_150ms_tick() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let mut settings = ungated(tmp.path());
    settings.budgeting_off = true;

    // ── the daemon that placed the thread ────────────────────────────────
    let id = {
        let m = build_manager(config_at(tmp.path(), &["aa-first"])).unwrap();
        let id = m.create_thread(None, settings).unwrap();
        assert_eq!(m.serving_account(&id).unwrap(), "aa-first");
        id
    };
    let reg = tmp.path().join("accounts.toml");
    let with_account = std::fs::read_to_string(&reg).unwrap();
    assert!(with_account.contains("aa-first"), "the registry carried the account: {with_account}");

    // ── the operator removes that account, and the daemon restarts ───────
    std::fs::write(&reg, "").unwrap();
    let m = build_manager(config_at(tmp.path(), &[])).unwrap();
    assert!(m.account_pool().is_empty());
    // The placement loop, so the 150 ms backstop tick really re-asks this
    // thread during the window below (without it the only re-ask is the
    // refusal's own `finish_turn`).
    m.start_watchers();

    let seen = watch(&m);
    m.send(&id, "hello".to_string(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| saw(&seen, "turn_parked")).await,
        "the turn is refused a login and parked: {:?}",
        seen.lock().unwrap()
    );
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    let count = |k: &str| seen.lock().unwrap().iter().filter(|s| s.as_str() == k).count();
    assert_eq!(
        (count("turn_parked"), count("turn_resumed"), count("turn_ended")),
        (1, 0, 0),
        "a login refusal is parked once and not re-asked while its inputs are unchanged"
    );
    assert!(m.store.get_parked_turn(&id).unwrap().is_some(), "his prompt is held in the parked row");
    let park = m.park_state(&id).unwrap();
    assert!(
        park["reason"].as_str().unwrap_or_default().contains("CLAUDE_CONFIG_DIR"),
        "it is the SPAWN GUARD's refusal that parked it (the gate was bypassed): {park}"
    );

    // ── the account comes back: the input moved, so the turn runs ────────
    std::fs::write(&reg, &with_account).unwrap();
    assert!(m.reload_pool(), "the pool sees the account again");
    m.placement_wake("test: account restored");
    assert!(
        eventually(|| saw(&seen, "turn_ended")).await,
        "with the account back in the pool, the parked turn ran: {:?}",
        seen.lock().unwrap()
    );
    assert_eq!(count("turn_parked"), 1, "and it was never refused a second time");
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
