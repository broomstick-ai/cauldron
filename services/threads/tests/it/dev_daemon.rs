//! # The throwaway dev daemon: its production guard, and its seeded scenarios
//!
//! Two halves, and the first one is the one that matters.
//!
//! **THE GUARD IS FORCED RED IN BOTH DIRECTIONS.** A guard that can never fire is
//! a sentence that can never fail, and a guard that fires on everything is a
//! guard nobody can use — so there are tests for both, and each names the exact
//! mutation that breaks it. The refusing tests go red if
//! `dev::production_objection` loses a clause; the admitting test goes red if it
//! gains a clause that is too broad, which is the failure mode a denylist
//! actually dies of.
//!
//! **No test here touches production.** Every path is a `tempfile::tempdir`, and
//! the one test that asks the guard about a real production location asks it
//! about a path it constructs and never opens. Nothing in this file creates a
//! `Store` anywhere but a temp dir.

use std::path::{Path, PathBuf};

use threads::dev;
use threads::store::Store;

/// `production_objection` reads `THREADS_HOME`, `HOME` and `CAULDRON_HOME`, which
/// are process-global. Tests inside one binary run in parallel threads, so the
/// env-reading tests are serialised behind this rather than racing each other.
static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. THE GUARD — RED
// ═════════════════════════════════════════════════════════════════════════════

/// **THE PRODUCTION DATA DIR IS REFUSED.**
///
/// The structural clause: a data dir inside a cargo source tree is refused,
/// because a worktree is cut from a commit and state kept there is work waiting
/// to be lost. Until 2026-10-07 this clause ALSO happened to carry production,
/// because production was `<repo>/lib/threads` — a crate directory, `Cargo.toml`
/// and all. It is `~/.cauldron/data/prod/threads/store` now and clause 1
/// (`$THREADS_HOME`) carries production; this clause stands on its own rationale
/// and on the rollback copy still sitting in `lib/threads/`.
///
/// This crate's own manifest dir IS such a directory, so the test asks about
/// itself and needs no fixture at all.
///
/// **Forced red** by deleting the `p.join("Cargo.toml").is_file()` arm from
/// `dev::production_objection`: the call answers `None` and this fails with
/// `the crate's own directory must be refused as a data dir`.
#[test]
fn the_production_data_dir_is_refused() {
    let _g = ENV.lock().unwrap();
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert!(
        crate_dir.join("Cargo.toml").is_file(),
        "precondition: this test's whole point is that a crate dir is one"
    );

    let why = dev::production_objection(&crate_dir);
    assert!(
        why.is_some(),
        "the crate's own directory must be refused as a data dir — on this machine \
         $THREADS_HOME points at exactly this shape of path and it holds the 3.8 GB \
         production store"
    );
    let why = why.unwrap();
    assert!(
        why.contains("cargo source tree"),
        "the refusal must say WHICH rule it hit, so it is actionable: {why}"
    );

    // And `claim_dev_dir` must refuse for the same reason rather than only
    // `production_objection` knowing about it.
    let err = dev::claim_dev_dir(&crate_dir).expect_err("claim must refuse it too");
    assert!(
        err.to_string().contains("REFUSING TO START"),
        "the refusal the operator sees: {err}"
    );
}

/// **`$THREADS_HOME` IS REFUSED, WHEREVER IT POINTS.**
///
/// This is the clause that does not depend on the production store being in a
/// source tree. `THREADS_HOME` is how the live daemon is aimed, so anybody who
/// has sourced `paths.env` — or copied a line out of it — has production one
/// defaulted path away, and the dev daemon must not follow it.
///
/// **Forced red** by deleting the `THREADS_HOME` arm: the temp dir has no
/// `Cargo.toml` above it and is not `~/.threads` or `~/.cauldron`, so every other
/// clause passes and this fails with `a dir that IS $THREADS_HOME must be
/// refused`.
#[test]
fn threads_home_is_refused_wherever_it_points() {
    let _g = ENV.lock().unwrap();
    let t = tmp();
    let prod = t.path().join("pretend-production");
    std::fs::create_dir_all(&prod).unwrap();

    let before = std::env::var_os("THREADS_HOME");
    std::env::set_var("THREADS_HOME", &prod);

    let why = dev::production_objection(&prod);
    let inner = dev::production_objection(&prod.join("nested"));

    match before {
        Some(v) => std::env::set_var("THREADS_HOME", v),
        None => std::env::remove_var("THREADS_HOME"),
    }

    let why = why.expect("a dir that IS $THREADS_HOME must be refused");
    assert!(why.contains("THREADS_HOME"), "{why}");
    assert!(
        inner.is_some(),
        "and so must anything INSIDE it — production's turns/ and accounts/ live there"
    );
}

/// **`~/.threads` IS REFUSED EVEN THOUGH IT DOES NOT EXIST ON THIS MACHINE.**
///
/// That is the whole reason it is named. `config::state_home()` falls back to it,
/// so it is where a default-config `threads` daemon keeps its store — and a guard
/// that only refused what is currently populated would stop working the moment
/// somebody populated it.
///
/// **Forced red** by deleting the `~/.threads` arm.
#[test]
fn the_built_in_state_home_is_refused_even_when_absent() {
    let _g = ENV.lock().unwrap();
    let t = tmp();
    let fake_home = t.path().to_path_buf();
    let dot_threads = fake_home.join(".threads");
    assert!(!dot_threads.exists(), "precondition: it does not exist");

    let before_home = std::env::var_os("HOME");
    let before_th = std::env::var_os("THREADS_HOME");
    std::env::set_var("HOME", &fake_home);
    std::env::remove_var("THREADS_HOME");

    let why = dev::production_objection(&dot_threads);

    if let Some(v) = before_home {
        std::env::set_var("HOME", v);
    }
    if let Some(v) = before_th {
        std::env::set_var("THREADS_HOME", v);
    }

    let why = why.expect("~/.threads must be refused even when it does not exist");
    assert!(why.contains("production state home"), "{why}");
}

/// **THE CAULDRON HOME IS REFUSED.**
///
/// `~/.cauldron` is not a `threads` data dir — it is the chassis consumer ledger
/// shared by every producer on this machine, and the hard don'ts name it. Since
/// 2026-10-07 it ALSO holds the live store, at `data/prod/threads/store`, so
/// `data/prod/` and the reserved `data/shared/` sibling are refused here too.
///
/// What is NOT refused any more is `data/<env>/` for an ordinary environment —
/// see [`a_cauldron_environment_data_root_is_admitted`], which is this test's
/// other half. One without the other is half a guard: before 2026-10-07 this
/// clause refused the entire tree, including `data/dev/`, which is the one place
/// the machine was reorganised to provide.
///
/// **Forced red** by deleting the `cauldron_home` arm.
#[test]
fn the_cauldron_home_is_refused() {
    let _g = ENV.lock().unwrap();
    let t = tmp();
    let cauldron = t.path().join("cauldron");
    std::fs::create_dir_all(&cauldron).unwrap();

    let before = std::env::var_os("CAULDRON_HOME");
    let before_th = std::env::var_os("THREADS_HOME");
    std::env::set_var("CAULDRON_HOME", &cauldron);
    std::env::remove_var("THREADS_HOME");

    // Everything that is NOT an ordinary environment's data root.
    let refused: Vec<(PathBuf, Option<String>)> = [
        cauldron.clone(),                                        // the home itself
        cauldron.join("chassis"),                                // the shared ledger
        cauldron.join("projects.json"),
        cauldron.join("data"),                                   // `data/` is not an env
        cauldron.join("data/prod"),                              // PRODUCTION
        cauldron.join("data/prod/threads/store"),                // the live store itself
        cauldron.join("data/shared"),                            // the reserved sibling
        cauldron.join("data/shared/weights/whisper"),
    ]
    .into_iter()
    .map(|p| {
        let why = dev::production_objection(&p);
        (p, why)
    })
    .collect();

    match before {
        Some(v) => std::env::set_var("CAULDRON_HOME", v),
        None => std::env::remove_var("CAULDRON_HOME"),
    }
    if let Some(v) = before_th {
        std::env::set_var("THREADS_HOME", v);
    }

    for (p, why) in &refused {
        let why = why
            .as_deref()
            .unwrap_or_else(|| panic!("THE GUARD DID NOT REFUSE {} — that is a failure", p.display()));
        println!("RED (refused, as it must be): {}\n    {why}", p.display());
    }
    // And each refusal says WHICH rule it hit, so it is actionable.
    assert!(refused[0].1.as_ref().unwrap().contains("cauldron home"), "{:?}", refused[0].1);
    assert!(refused[4].1.as_ref().unwrap().contains("PRODUCTION"), "{:?}", refused[4].1);
    assert!(refused[6].1.as_ref().unwrap().contains("shared"), "{:?}", refused[6].1);
}

/// **A LEGITIMATE ENVIRONMENT'S DATA ROOT IS ADMITTED — THE OTHER FORCED RED.**
///
/// On 2026-10-07 the live store moved into `~/.cauldron/data/prod/threads/store`,
/// and the cauldron-home clause, which had been refusing the whole tree, started
/// refusing `data/dev/` with it. Segmenting state by environment is the entire
/// purpose of that mechanism, so a guard that refuses every environment because
/// one of them is production is not a guard, it is an outage.
///
/// **Forced red** two ways, and either alone holds: make the `cauldron_home`
/// clause refuse the whole tree again (the pre-2026-10-07 behaviour — every
/// refusing test above still passes and this one fails), or add `"dev"` to
/// `dev::CAULDRON_ENVS_REFUSED`.
#[test]
fn a_cauldron_environment_data_root_is_admitted() {
    let _g = ENV.lock().unwrap();
    let t = tmp();
    let cauldron = t.path().join("cauldron");
    std::fs::create_dir_all(&cauldron).unwrap();

    let before = std::env::var_os("CAULDRON_HOME");
    let before_th = std::env::var_os("THREADS_HOME");
    std::env::set_var("CAULDRON_HOME", &cauldron);
    std::env::remove_var("THREADS_HOME");

    // The data root itself, and the decided `data/<env>/<service>/<kind>/` shape
    // inside it — which is exactly the shape production occupies.
    let admitted: Vec<(PathBuf, Option<String>)> = [
        cauldron.join("data/dev"),
        cauldron.join("data/dev/threads"),
        cauldron.join("data/dev/threads/store"),
        cauldron.join("data/other/threads/store"),
        cauldron.join("data/e2e_test/threads/store"),
    ]
    .into_iter()
    .map(|p| {
        let why = dev::production_objection(&p);
        (p, why)
    })
    .collect();

    // A real claim in the one that matters, so this is not only an opinion about
    // a path: the dev daemon has to be able to USE it.
    let dev_store = cauldron.join("data/dev/threads/store");
    let claimed = dev::claim_dev_dir(&dev_store);

    match before {
        Some(v) => std::env::set_var("CAULDRON_HOME", v),
        None => std::env::remove_var("CAULDRON_HOME"),
    }
    if let Some(v) = before_th {
        std::env::set_var("THREADS_HOME", v);
    }

    for (p, why) in &admitted {
        if let Some(why) = why {
            panic!(
                "THE GUARD REFUSED A LEGITIMATE ENVIRONMENT DATA ROOT {}: {why}",
                p.display()
            );
        }
        println!("GREEN (admitted, as it must be): {}", p.display());
    }
    claimed.expect("a dev daemon must be able to claim a store inside data/dev/");
    assert!(
        dev_store.join(dev::DEV_MARKER).is_file(),
        "and the claim leaves its marker, so the marker lock still governs it"
    );
    println!("GREEN (claimed, marker written): {}", dev_store.display());
}

/// **PRODUCTION'S REAL, CURRENT PATH IS REFUSED — ASKED ABOUT BY NAME.**
///
/// The tests above use temp dirs so they cannot touch anything. This one asks
/// the guard about the operator's actual live store, as a path it constructs and
/// never opens, because every clause above is scoped by an environment variable
/// and the one question that matters is whether the real path comes back
/// refused. Nothing is created, read or written.
///
/// **Forced red** by scoping the cauldron-home clause's exception to include
/// `prod`, or by deleting the clause.
#[test]
fn the_real_live_store_is_refused_by_path() {
    let _g = ENV.lock().unwrap();
    const LIVE: &str = "/Users/testuser/.cauldron/data/prod/threads/store";

    let before_c = std::env::var_os("CAULDRON_HOME");
    let before_th = std::env::var_os("THREADS_HOME");
    let before_h = std::env::var_os("HOME");
    // Deliberately WITHOUT $THREADS_HOME, so clause 1 cannot be what refuses it
    // — this asks whether the cauldron-home clause alone still holds the line.
    std::env::remove_var("THREADS_HOME");
    std::env::remove_var("CAULDRON_HOME");
    std::env::set_var("HOME", "/Users/testuser");

    let why = dev::production_objection(Path::new(LIVE));
    let data_root = dev::production_objection(Path::new("/Users/testuser/.cauldron/data/prod"));

    if let Some(v) = before_c {
        std::env::set_var("CAULDRON_HOME", v);
    }
    if let Some(v) = before_th {
        std::env::set_var("THREADS_HOME", v);
    }
    match before_h {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }

    let why = why.expect("THE GUARD DID NOT REFUSE THE OPERATOR'S LIVE STORE");
    println!("RED (the real live store, refused): {LIVE}\n    {why}");
    assert!(why.contains("PRODUCTION"), "the refusal must name production: {why}");
    assert!(
        data_root.is_some(),
        "and the prod environment's whole data root, not just the store"
    );
}

/// **A DIRECTORY SOMEBODY ELSE'S DAEMON ALREADY USES IS REFUSED.**
///
/// This is the marker lock, and it is the half that does not depend on anyone's
/// imagination. The denylist can only refuse paths somebody thought of, and the
/// production store has already moved once (it is not `~/.threads`). A populated
/// directory with no marker is not the dev daemon's, whatever its path.
///
/// **Forced red** two ways, and either alone holds: delete the
/// `entries.next().is_some()` refusal in `claim_dev_dir`, or make it write the
/// marker before checking. Both make `claim_dev_dir` return `Ok` and this fails
/// with `a populated dir with no marker must be refused`.
#[test]
fn a_populated_directory_without_the_marker_is_refused() {
    let _g = ENV.lock().unwrap();
    let t = tmp();
    let someone_elses = t.path().join("someone-elses-store");
    std::fs::create_dir_all(&someone_elses).unwrap();
    std::fs::write(someone_elses.join("threads.db"), b"not really a db").unwrap();

    let err = dev::claim_dev_dir(&someone_elses)
        .expect_err("a populated dir with no marker must be refused");
    let msg = err.to_string();
    assert!(msg.contains("REFUSING TO START"), "{msg}");
    assert!(
        !someone_elses.join(dev::DEV_MARKER).exists(),
        "and a refused claim must not have written its marker into it"
    );
}

/// **TEARDOWN RUNS THE SAME GUARD, SO IT CANNOT `rm -rf` PRODUCTION.**
///
/// A teardown that trusted its argument would be the guard on the wrong side of
/// the door. It is the dangerous half of the tool, so it is tested against the
/// same production shape the start path is.
///
/// **Forced red** by deleting the `production_objection` call from
/// `teardown_dev_dir`: it would then proceed to the marker check, and for a
/// directory that happens to carry both a marker and a production path it would
/// delete. The assertion that catches it is that the crate dir is refused and
/// still exists.
#[test]
fn teardown_refuses_production_and_refuses_an_unmarked_dir() {
    let _g = ENV.lock().unwrap();
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let err = dev::teardown_dev_dir(&crate_dir).expect_err("teardown must refuse production");
    assert!(err.to_string().contains("REFUSING TO DELETE"), "{err}");
    assert!(
        crate_dir.join("Cargo.toml").is_file(),
        "and the refusal must have removed nothing"
    );

    // An ordinary directory that is not production but carries no marker is also
    // refused: the dev daemon deletes only what it made.
    let t = tmp();
    let unmarked = t.path().join("unmarked");
    std::fs::create_dir_all(&unmarked).unwrap();
    std::fs::write(unmarked.join("something-precious"), b"x").unwrap();
    let err = dev::teardown_dev_dir(&unmarked).expect_err("an unmarked dir must be refused");
    assert!(err.to_string().contains("carries no"), "{err}");
    assert!(unmarked.join("something-precious").exists(), "nothing removed");
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. THE GUARD — GREEN, WHICH IS THE OTHER FORCED RED
// ═════════════════════════════════════════════════════════════════════════════

/// **A FRESH DEV DIR IS ADMITTED, AND THE MARKER IS WRITTEN.**
///
/// This is the forced red in the OTHER direction, and it is not a formality: the
/// failure mode of a path denylist is that it grows a clause too broad and starts
/// refusing everything, at which point every refusing test above still passes and
/// the tool is dead. A guard that can never say yes is as useless as one that can
/// never say no.
///
/// **Forced red** by making `production_objection` return `Some(..)`
/// unconditionally — which is the mutation that makes every other test in this
/// file pass. This fails with `a fresh dev dir must be admitted`.
///
/// Also the idempotency of the claim, which is what makes running the dev daemon
/// twice safe.
#[test]
fn a_fresh_dev_dir_is_admitted_and_claiming_is_idempotent() {
    let _g = ENV.lock().unwrap();
    let t = tmp();
    let dev_dir = t.path().join("threads-dev");

    assert_eq!(
        dev::production_objection(&dev_dir),
        None,
        "a fresh dev dir must be admitted — a guard that refuses everything is not a guard"
    );

    dev::claim_dev_dir(&dev_dir).expect("a fresh dev dir must be claimable");
    assert!(
        dev_dir.join(dev::DEV_MARKER).is_file(),
        "the claim must leave its marker behind"
    );

    // Twice. A second claim is the ordinary case — the operator restarting the
    // dev daemon — and it must be a no-op rather than a refusal.
    dev::claim_dev_dir(&dev_dir).expect("claiming an already-claimed dir must be a no-op");

    // And an EMPTY pre-existing directory is fine: it is nobody's state.
    let empty = t.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    dev::claim_dev_dir(&empty).expect("an empty directory is nobody's state");
}

/// **TEARDOWN ACTUALLY DELETES A DIR IT MADE.** The green half of the dangerous
/// verb — without this, "teardown refuses production" could be satisfied by a
/// teardown that refuses everything.
#[test]
fn teardown_removes_what_the_dev_daemon_made() {
    let _g = ENV.lock().unwrap();
    let t = tmp();
    let dev_dir = t.path().join("threads-dev");
    dev::claim_dev_dir(&dev_dir).unwrap();
    std::fs::write(dev_dir.join("threads.db"), b"synthetic").unwrap();

    assert!(dev::teardown_dev_dir(&dev_dir).unwrap(), "it was there");
    assert!(!dev_dir.exists(), "and it is gone");
    assert!(
        !dev::teardown_dev_dir(&dev_dir).unwrap(),
        "and tearing down twice reports nothing to do rather than failing"
    );
}

/// **THE DEV PORTS ARE NOT THE PRODUCTION PORTS**, and the dev home is not the
/// production home.
///
/// Cheap, and it is the assertion that would catch somebody "simplifying" the
/// constants back toward the defaults. `DEV_HOME_ENV` is checked by name because
/// the whole point is that it is not `THREADS_HOME`.
#[test]
fn the_dev_endpoints_cannot_collide_with_production() {
    // ONE dev port since R2.S4 (the legacy `DEV_PORT`, 13651, went with the
    // legacy door it mirrored).
    assert_ne!(dev::DEV_CHASSIS_PORT, threads::config::DEFAULT_CHASSIS_PORT);
    // 3649 is mesh and 3652 is tts; `promote` health-probes both. 3651 is the
    // retired legacy door, still bound by any daemon older than R2.S4.
    for taken in [3649u16, 3650, 3651, 3652, 3653] {
        assert_ne!(dev::DEV_CHASSIS_PORT, taken);
    }
    assert_eq!(
        dev::DEV_HOME_ENV, "THREADS_DEV_HOME",
        "it must NOT be THREADS_HOME — that is how production is aimed"
    );

    let t = tmp();
    let c = dev::dev_config(t.path());
    assert_eq!(c.chassis_port, dev::DEV_CHASSIS_PORT);
    assert_eq!(
        c.data_dir.as_deref(),
        Some(t.path().to_string_lossy().as_ref()),
        "the data dir is EXPLICIT, so `resolved_data_dir` never consults state_home()"
    );
    // Every periodic `claude` spawn off: the gauge legs must not shell out from a
    // daemon somebody started to look at a bug.
    assert_eq!(c.probe_interval_secs, 0);
    assert_eq!(c.probe_interval_busy_secs, 0);
    assert!(!c.calibrate);
    assert!(!c.edge_poll);
    assert_eq!(c.accounts.len(), dev::ACCOUNTS.len());
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. THE SEEDED SCENARIOS
// ═════════════════════════════════════════════════════════════════════════════

fn seeded(scale: dev::Scale) -> (tempfile::TempDir, Store, dev::SeedReport) {
    let t = tmp();
    let store = Store::open(t.path().join("threads.db")).unwrap();
    let report = dev::seed(&store, scale).unwrap();
    (t, store, report)
}

/// **ONE DISABLED-AND-DEAD ACCOUNT, ONE RATE-LIMITED, ONE HEALTHY** — and the
/// three mechanisms are genuinely different columns in genuinely different
/// tables, which is why conflating them in a diagnosis goes wrong.
#[test]
fn the_three_account_states_are_seeded_and_are_three_different_mechanisms() {
    let (_t, store, r) = seeded(dev::Scale::default());
    assert_eq!(r.accounts, 3);
    let now = threads::store::now_rfc3339();

    // DISABLED is row presence in `account_disabled`, not a column.
    let disabled = store.list_disabled_accounts().unwrap();
    assert_eq!(disabled.len(), 1, "exactly one disabled account: {disabled:?}");
    assert_eq!(disabled[0].0, dev::ACCOUNT_DEAD);
    assert!(
        store.account_disabled_at(dev::ACCOUNT_HEALTHY).unwrap().is_none(),
        "and the healthy one is not disabled"
    );

    let state = |n: &str| store.account_state(n).unwrap().expect("seeded");

    // DEAD is `accounts.dead_since`.
    let dead = state(dev::ACCOUNT_DEAD);
    assert!(dead.dead_since.is_some(), "the dead login carries dead_since");
    assert!(dead.dead_reason.is_some());
    assert!(!dead.healthy(), "so it is not healthy");
    assert!(!dead.selectable(&now), "and not selectable");

    // RATE-LIMITED is `accounts.limit_until`, honoured only while
    // `limit_identity` matches `identity` — a mismatch is `limit_is_stale` and the
    // window is ignored, which is the failure this scenario exists to show.
    let limited = state(dev::ACCOUNT_LIMITED);
    assert!(limited.limit_until.is_some(), "the limited one carries a window");
    assert!(
        !limited.limit_is_stale(),
        "and the window must NOT be stale, or the account reads available and the \
         scenario is silently absent"
    );
    assert!(!limited.available(&now), "so it is unavailable");
    assert!(limited.healthy(), "but its login is alive — that is the point");
    assert!(limited.last_limit_prose.is_some());
    assert_eq!(limited.limit_axis.as_deref(), Some("weekly_all"));

    // HEALTHY is all three absences.
    let healthy = state(dev::ACCOUNT_HEALTHY);
    assert!(healthy.healthy() && healthy.available(&now) && healthy.selectable(&now));
}

/// **SEVERAL THREADS PARKED ON AN UNAVAILABLE ACCOUNT**, with their prompts held
/// verbatim, and each holding a retry floor.
///
/// `parked >= 2` is deliberate: a mechanism that arbitrates is not exercised by
/// its empty or single case.
///
/// The retry row is not decoration — it is what stops `resume_sweep` from
/// redelivering the parked prompt at boot and, through `note_account_served`,
/// CLEARING the rate-limit window. That was measured, not supposed; see the
/// comment on `dev::seed_parked_on_unavailable_account`.
#[test]
fn threads_are_parked_on_the_unavailable_account_with_their_prompts_held() {
    let (_t, store, r) = seeded(dev::Scale::default());
    let parked = store.list_parked_turns().unwrap();
    assert!(
        parked.len() >= 2,
        "a park is an arbitration; prove it at N >= 2, got {}",
        parked.len()
    );
    assert_eq!(parked.len(), r.parked);

    for p in &parked {
        assert!(p.thread_id.starts_with("dev-"), "synthetic ids only: {p:?}");
        assert!(
            p.reason.contains(dev::ACCOUNT_LIMITED),
            "the reason must name the account that is unavailable: {}",
            p.reason
        );
        assert!(!p.prompt.is_empty(), "CON-4: a park never drops what was said");
        assert!(!p.message_ids.is_empty());

        // The thread really is placed on the unavailable account.
        let row = store.get_thread(&p.thread_id).unwrap().expect("thread row");
        assert_eq!(row.account.as_deref(), Some(dev::ACCOUNT_LIMITED));

        // THE PARKED PROMPT, attributed to that same account.
        //
        // It carried a `retry_at` in the future, so the fixture could not eat itself
        // by firing its own floor. The floor is gone (operator: *"There should not be
        // a per-thread retry timestamp"*) and the ACCOUNT BAR is what holds the
        // fixture still now — which is also what held the real thing still, so the
        // fixture is a closer likeness than it was. What this asserts instead is that
        // the row has a PROMPT, because that is the only thing it is for.
        let retry = store
            .get_retry(&p.thread_id)
            .unwrap()
            .expect("every parked thread holds a parked prompt, or resume_sweep runs it");
        assert_eq!(retry.account.as_deref(), Some(dev::ACCOUNT_LIMITED));
        assert!(
            !retry.prompt.is_empty(),
            "a parked prompt with no prompt in it is a dropped turn: {retry:?}"
        );
        let limited = store
            .list_account_states()
            .unwrap()
            .into_iter()
            .find(|s| s.name == dev::ACCOUNT_LIMITED)
            .expect("the limited account must exist");
        let until = limited
            .limit_until
            .expect("the fixture's stillness is the ACCOUNT BAR now, not a row's floor");
        let at = chrono::DateTime::parse_from_rfc3339(&until).expect("rfc3339");
        assert!(
            at > chrono::Utc::now(),
            "a bar in the past lifts immediately and the fixture eats itself: {until}"
        );

        // And the ledger says so, with the cause string a real park produces.
        let kinds: Vec<String> = store
            .list_events(&p.thread_id, 0, 100, None)
            .unwrap()
            .into_iter()
            .map(|e| e.event.kind().to_string())
            .collect();
        assert!(kinds.contains(&"turn_parked".to_string()), "{kinds:?}");
    }
}

/// **A QUEUE WITH ITEMS WAITING AND ITEMS FAILED.**
///
/// Both at once on the same thread, which is the combination that is awkward to
/// produce by hand: `Store::fail_deliveries` is guarded on `state='delivered'`, so
/// a failed row can only be reached through `enqueue → mark_delivered →
/// fail_deliveries`. The seed walks those transitions rather than writing the
/// terminal state, so the fixture exercises the same state machine the daemon
/// does.
#[test]
fn the_queue_holds_both_waiting_and_failed_items() {
    let scale = dev::Scale::default();
    let (_t, store, r) = seeded(scale);
    assert!(r.queue_waiting > 0 && r.queue_failed > 0);

    let mut threads_with_both = 0;
    for row in store.list_threads(1000).unwrap() {
        let waiting = store.waiting_queue(&row.id).unwrap();
        let failed = store.failed_deliveries(&row.id).unwrap();
        if waiting.is_empty() && failed.is_empty() {
            continue;
        }
        assert_eq!(waiting.len(), scale.waiting_per_thread, "on {}", row.id);
        assert_eq!(failed.len(), scale.failed_per_thread, "on {}", row.id);
        for w in &waiting {
            assert_eq!(w.state, "waiting");
        }
        for f in &failed {
            assert_eq!(f.state, "failed");
            assert!(!f.text.is_empty(), "a failed row keeps the words verbatim");
        }
        threads_with_both += 1;

        // THE HELD LOCK, which is why the waiting rows survive `resume_sweep`.
        let lock = store
            .get_thread_lock(&row.id)
            .unwrap()
            .expect("a queue-bearing thread holds a foreign lock, or boot dispatches it");
        assert_eq!(lock.daemon_pid, 1, "pid 1 is launchd and is always alive");
        assert_ne!(
            lock.boot_id, "",
            "and the holder must be a boot_id this process could never mint"
        );
    }
    assert!(
        threads_with_both >= 2,
        "a queue is an arbitration; prove it at N >= 2, got {threads_with_both}"
    );
}

/// **SOME THREADS HAVE USER MESSAGES AND SOME HAVE NONE AT ALL — AND THE SILENT
/// ONES ARE ABSENT FROM THE RECENTS INDEX ENTIRELY.**
///
/// This is the one the operator called out, and the assertion that matters is the
/// last one. HEAD's `threads/recent-updates` takes its order from
/// `threads.last_user_input_at`, which `bump_last_seq` sets only for a
/// `user_message`; `Store::recent_threads` filters a NULL out, and the shape
/// cannot mint a position the store does not offer. So a thread never spoken in is
/// not last in the index — it is not in it.
///
/// **Forced red** by passing `Some(&ts)` unconditionally in `dev::append`'s
/// `bump_last_seq` call (i.e. dropping the `is_user_message` test): the silent
/// threads acquire a `last_user_input_at`, `recent_threads` returns all of them,
/// and this fails with `a thread never spoken in must be ABSENT from the recents
/// order`.
#[test]
fn a_thread_never_spoken_in_is_absent_from_the_recents_index() {
    let scale = dev::Scale::default();
    let (_t, store, _r) = seeded(scale);

    let mut silent = Vec::new();
    let mut spoken = Vec::new();
    for row in store.list_threads(1000).unwrap() {
        let users = store
            .list_events(&row.id, 0, 1000, None)
            .unwrap()
            .into_iter()
            .filter(|e| e.event.is_user_message())
            .count();
        if users == 0 {
            silent.push(row.id);
        } else {
            spoken.push(row.id);
        }
    }
    assert!(
        silent.len() >= 2,
        "at least two threads with NO user messages at all, got {silent:?}"
    );
    assert!(
        !spoken.is_empty(),
        "and threads that HAVE been spoken in, or 'absent from the index' means nothing"
    );

    // The column: NULL for the silent ones, set for the rest.
    for id in &silent {
        assert_eq!(
            store.last_user_input_at(id).unwrap(),
            None,
            "{id} has no user message, so it can have no last_user_input_at"
        );
    }
    for id in &spoken {
        assert!(store.last_user_input_at(id).unwrap().is_some(), "{id}");
    }

    // AND THE INDEX. This is the bug.
    let order: Vec<String> = store
        .recent_threads(1000)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    for id in &silent {
        assert!(
            !order.contains(id),
            "a thread never spoken in must be ABSENT from the recents order, not last \
             in it — {id} is in {order:?}"
        );
    }
    for id in &spoken {
        assert!(order.contains(id), "{id} must hold a position: {order:?}");
    }
    assert_eq!(
        order.len(),
        spoken.len(),
        "the index is exactly the spoken-in threads"
    );
}

/// **RUNNING IT TWICE MUST NOT ACCUMULATE GARBAGE.**
///
/// The one non-idempotent write in the whole seeding path is `Store::enqueue` —
/// a plain INSERT against an AUTOINCREMENT id — so this is really a test of
/// `dev::ensure_queue`'s count-and-top-up.
///
/// **Forced red** by replacing `for n in have_waiting..waiting` with
/// `for n in 0..waiting` in `dev::ensure_queue`: the second seed stacks a second
/// set of rows and this fails on the queue counts.
#[test]
fn seeding_twice_adds_nothing() {
    let scale = dev::Scale::default();
    let (_t, store, first) = seeded(scale);

    let census = |store: &Store| -> (usize, usize, usize, usize, usize) {
        let threads = store.list_threads(10_000).unwrap();
        let mut waiting = 0;
        let mut failed = 0;
        let mut events = 0;
        for t in &threads {
            waiting += store.waiting_queue(&t.id).unwrap().len();
            failed += store.failed_deliveries(&t.id).unwrap().len();
            events += store.list_events(&t.id, 0, 100_000, None).unwrap().len();
        }
        (
            threads.len(),
            waiting,
            failed,
            events,
            store.list_parked_turns().unwrap().len(),
        )
    };
    let before = census(&store);

    let second = dev::seed(&store, scale).unwrap();
    assert_eq!(second.threads_created, 0, "no new threads");
    assert_eq!(second.events, 0, "no new events");
    assert_eq!(second.queue_waiting, 0, "no new waiting rows");
    assert_eq!(second.queue_failed, 0, "no new failed rows");
    assert_eq!(second.threads_already_present, first.threads_created);

    assert_eq!(census(&store), before, "the store is byte-for-byte as populous");

    // A THIRD time, because two runs can hide an off-by-one that three exposes.
    dev::seed(&store, scale).unwrap();
    assert_eq!(census(&store), before);
}

/// **THE SCALE IS A PARAMETER, AND RAISING IT TOPS UP RATHER THAN DOUBLING.**
#[test]
fn the_scale_is_a_parameter_and_raising_it_tops_up() {
    let (_t, store, _) = seeded(dev::Scale::default());
    let small = dev::Scale::default();
    assert_eq!(store.list_threads(10_000).unwrap().len(), small.threads());

    let big = dev::Scale::times(3);
    assert_eq!(big.threads(), small.threads() * 3);
    dev::seed(&store, big).unwrap();
    assert_eq!(
        store.list_threads(10_000).unwrap().len(),
        big.threads(),
        "raising the scale must TOP UP to the new size, not add a second cohort"
    );
    for t in store.list_threads(10_000).unwrap() {
        let w = store.waiting_queue(&t.id).unwrap().len();
        if w > 0 {
            assert_eq!(w, big.waiting_per_thread, "topped up on {}", t.id);
        }
    }
}

/// **EVERY SEEDED ROW IS OBVIOUSLY SYNTHETIC.**
///
/// A dev store that found its way into a diagnosis must announce itself. Every
/// thread id carries the prefix and every account name starts `dev-`.
#[test]
fn nothing_seeded_could_be_mistaken_for_real_data() {
    let (_t, store, _) = seeded(dev::Scale::default());
    for row in store.list_threads(10_000).unwrap() {
        assert!(
            row.id.starts_with(dev::THREAD_PREFIX),
            "thread id must announce itself: {}",
            row.id
        );
    }
    for s in store.list_account_states().unwrap() {
        assert!(s.name.starts_with("dev-"), "account name: {}", s.name);
    }
    for name in dev::ACCOUNTS {
        assert!(name.starts_with("dev-"));
    }
}

/// `normalize` has to cope with a path whose leaf does not exist yet, because
/// that is the ordinary case for a dev dir on its first run. Without it
/// `~/.threads-dev` and `/Users/x/./.threads-dev` compare unequal and every
/// clause of the guard is decorative.
#[test]
fn the_guard_sees_through_an_unresolved_leaf_and_a_dot_segment() {
    let _g = ENV.lock().unwrap();
    let t = tmp();
    let prod = t.path().join("prod");
    std::fs::create_dir_all(&prod).unwrap();

    let before = std::env::var_os("THREADS_HOME");
    // Aim THREADS_HOME through a `.` segment at a path with a not-yet-existing
    // leaf, and ask about the plain spelling of the same place.
    std::env::set_var(
        "THREADS_HOME",
        Path::new(t.path()).join(".").join("prod").join("deeper"),
    );
    let why = dev::production_objection(&prod.join("deeper"));
    match before {
        Some(v) => std::env::set_var("THREADS_HOME", v),
        None => std::env::remove_var("THREADS_HOME"),
    }

    assert!(
        why.is_some(),
        "two spellings of one production path must compare equal, or the guard is prose"
    );
}
