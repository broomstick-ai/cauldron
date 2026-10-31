//! The stress-test DRIVER, exercised against the fake-claude fixture
//! (round-7 item 8). The driver is what the coordinator will point at the
//! operator's live accounts, so every claim it makes has to be true here first:
//!
//! * it refuses to start when anything else is running — exactly ONE execution
//!   at a time;
//! * it refuses to start when another driver holds the lock;
//! * it sets the value under test, sends ONE message, and classifies the
//!   outcome (`gated` / `parked` / `tripped` / `killed` / `finished`);
//! * it puts the configuration back when it is done.
//!
//! **No real claude runs here** — the daemon under test is wired to the fixture,
//! exactly like every other suite in this crate.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use threads::store::EstimateRow;
use threads::{build_manager, Config};

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

async fn start() -> (threads::ThreadManager, SocketAddr, tempfile::TempDir) {
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_HEARTBEAT_MS", "100");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "300");
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("THREADS_KILL_GRACE_MS", "200");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    // The machine-default account's transcript store is `<HOME>/.claude` — the
    // OPERATOR'S OWN login. Point the transcript home at a scratch dir so no
    // test can read or write inside it (account moves now port transcripts).
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
    let tmp = tempfile::tempdir().unwrap();
    // CON-27: the driver's whole job is to place a REAL thread and watch it, so
    // this fixture needs an account threads OWNS. `ensure_default` still
    // registers the machine `default` on every boot, but nothing may be placed
    // there any more, so a config with no accounts of its own fails at
    // `threads.create` and the driver never gets far enough to report anything.
    // `aa-owned` sorts first, so it plays exactly the role `default` used to
    // play in the ranking — and in `gauge_before.accounts[0]` below.
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        tmp.path().join("aa-owned").to_string_lossy().to_string(),
    );
    let config = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        ..Default::default()
    };
    let manager = build_manager(config).unwrap();
    let (addr, _handle) = threads::chassis::serve(std::sync::Arc::new(manager.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (manager, addr, tmp)
}

/// Run the driver binary and return `(exit_code, parsed stdout report)`.
fn run_driver(addr: SocketAddr, lock: &std::path::Path, args: &[&str]) -> (i32, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_threads-stress"))
        .arg("--url")
        .arg(format!("ws://{addr}/ws"))
        .arg("--lock")
        .arg(lock)
        .args(args)
        .env("THREADS_CLAUDE_BIN", fake_claude())
        .output()
        .expect("running threads-stress");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
        panic!(
            "driver produced no JSON report.\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code().unwrap_or(-1), report)
}

fn calibrate(m: &threads::ThreadManager, account: &str, axis: &str, tpp: f64) {
    m.store
        .upsert_estimate(&EstimateRow {
            account: account.into(),
            axis: axis.into(),
            tokens_per_percent: tpp,
            sample_count: 3,
            last_obs_id: 0,
            updated_at: chrono::Utc::now().to_rfc3339(),
        })
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_driver_runs_one_execution_and_reports_it_finished() {
    let (_m, addr, tmp) = start().await;
    let lock = tmp.path().join("stress.lock");
    let cwd = tmp.path().to_string_lossy().to_string();

    let (code, report) = tokio::task::spawn_blocking(move || {
        run_driver(
            addr,
            &lock,
            &[
                "--new-thread",
                "--cwd",
                &cwd,
                "--text",
                "hello from the stress driver",
                "--timeout-secs",
                "30",
            ],
        )
    })
    .await
    .unwrap();

    assert_eq!(code, 0, "report: {report}");
    assert_eq!(report["outcome"], "finished");
    assert_eq!(report["turn_started"], true);
    assert_eq!(report["exit_code"], 0);
    assert!(report["thread_id"].is_string());
    // The lock is released for the next run.
    assert!(!tmp.path().join("stress.lock").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_driver_reports_a_threshold_gate_without_ever_spawning() {
    // The operator's own stress recipe: "setting the thresholds lower than what
    // the values are right now."
    let (m, addr, tmp) = start().await;
    m.budgets
        .observe(
            // CON-27: the reading has to be on the account the thread will
            // actually SERVE on, or the threshold has nothing to gate against.
            // That is `aa-owned` now, not the machine default.
            "aa-owned",
            threads::gauge::Axis::WeeklyFable,
            88.0,
            None,
            "usage_poll",
        )
        .unwrap();
    let lock = tmp.path().join("stress.lock");
    let cwd = tmp.path().to_string_lossy().to_string();

    let (code, report) = tokio::task::spawn_blocking(move || {
        run_driver(
            addr,
            &lock,
            &[
                "--new-thread",
                "--cwd",
                &cwd,
                "--threshold-fable",
                "60",
                "--text",
                "this must never reach claude",
                "--settle-secs",
                "2",
                "--timeout-secs",
                "30",
            ],
        )
    })
    .await
    .unwrap();

    assert_eq!(code, 0, "report: {report}");
    assert_eq!(report["outcome"], "gated");
    assert_eq!(report["turn_started"], false);
    assert_eq!(report["parked"]["cause"], "threshold");
    assert_eq!(report["restored"], true);
    // The gauge reading it gated against is on the record.
    assert_eq!(
        report["gauge_before"]["accounts"][0]["axes"]["weekly_fable"]["observed_pct"],
        88.0
    );
    // …and the threshold it set was put back — the thread is back to whatever
    // it carried before the trial, which since 2026-08-15 is the fleet-wide
    // weekly threshold (`DEFAULT_WEEKLY_THRESHOLD_PCT`) and NOT an empty
    // object. What is being pinned is that the driver restores; the value it
    // restores to is the crate's default, stated here rather than assumed.
    let id = report["thread_id"].as_str().unwrap();
    let row = m.store.get_thread(id).unwrap().unwrap();
    let settings: Value = serde_json::from_str(&row.settings_json).unwrap();
    assert_eq!(
        settings["thresholds"],
        json!({ "weekly": threads::budgets::DEFAULT_WEEKLY_THRESHOLD_PCT }),
        "the fable threshold the trial set is gone; the default it started with is back"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_driver_reports_a_kill_and_restores_the_budget() {
    // The kill-toggle arm of the live trial: a tiny budget plus `--on-trip
    // kill`, which is the ONLY way a mid-turn teardown happens now (item 3).
    let (m, addr, tmp) = start().await;
    // CON-27: calibrate the meter of the account the turn will run on.
    calibrate(&m, "aa-owned", "session", 1000.0);
    calibrate(&m, "aa-owned", "weekly_all", 1000.0);
    let lock = tmp.path().join("stress.lock");
    let cwd = tmp.path().to_string_lossy().to_string();
    let burnfile = tmp.path().join("burn.pid");
    let text = format!("bigburn burnfile={}", burnfile.display());

    let (code, report) = tokio::task::spawn_blocking(move || {
        run_driver(
            addr,
            &lock,
            &[
                "--new-thread",
                "--cwd",
                &cwd,
                "--key",
                "stress-kill",
                "--accounts",
                "aa-owned",
                "--session-pct",
                "1",
                "--on-trip",
                "kill",
                "--text",
                &text,
                "--timeout-secs",
                "30",
            ],
        )
    })
    .await
    .unwrap();

    assert_eq!(code, 0, "report: {report}");
    assert_eq!(report["outcome"], "killed");
    assert_eq!(report["tripped"]["axis"], "session");
    assert_eq!(report["tripped"]["action"], "kill");
    assert_eq!(report["killed"], true);
    // CON-4 even here: the killed prompt was PARKED, not lost — and the
    // driver's own report carries the proof.
    let parked = report["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "turn_parked")
        .expect("a killed turn parks its prompt");
    assert_eq!(parked["event"]["cause"], "budget");
    assert_eq!(parked["event"]["budget_key"], "stress-kill");
    // Q-34: budget keys are never deleted, so the key the driver MINTED for the
    // run is DISABLED instead — and the test thread is unpointed from it first,
    // so nothing is left aimed at a key that denies. The run leaves no live 1%
    // budget on the operator's machine, and its spend stays queryable.
    let left = m
        .budgets
        .get_config("stress-kill")
        .unwrap()
        .expect("a minted key is disabled, never deleted");
    assert!(left.disabled, "the driver must disable the key it minted");
    assert_eq!(left.session_pct, Some(1.0));
    let id = report["thread_id"].as_str().unwrap();
    assert_eq!(m.thread_budget_key(id).unwrap(), None);
    // Its accounting survived the restore — the whole point of disabling.
    assert!(!m.store.charge_accounts("stress-kill").unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_driver_re_enables_a_disabled_key_for_the_run_and_disables_it_again() {
    // Q-34's mirror image on the way IN. A disabled key denies every turn, so a
    // run against one would only ever measure the disable gate. The driver
    // enables it for the duration, says so in its report, and puts the
    // operator's deny switch back exactly as it found it.
    let (m, addr, tmp) = start().await;
    m.budgets
        .set_config(&{
            let mut c = threads::budgets::BudgetConfig::new("stress-off");
            c.accounts = vec!["aa-owned".into()];
            c.session_pct = Some(90.0);
            c
        })
        .unwrap();
    m.budgets.set_disabled("stress-off", true).unwrap();
    let lock = tmp.path().join("stress.lock");
    let cwd = tmp.path().to_string_lossy().to_string();

    let (code, report) = tokio::task::spawn_blocking(move || {
        run_driver(
            addr,
            &lock,
            &[
                "--new-thread",
                "--cwd",
                &cwd,
                "--key",
                "stress-off",
                "--session-pct",
                "90",
                "--text",
                "hello",
                "--timeout-secs",
                "30",
            ],
        )
    })
    .await
    .unwrap();

    assert_eq!(code, 0, "report: {report}");
    // The run actually RAN rather than being denied by the disable gate…
    assert_eq!(report["outcome"], "finished");
    assert_eq!(report["applied"]["re_enabled_for_the_run"], true);
    // …and the key is disabled again afterwards.
    assert!(
        m.budgets
            .get_config("stress-off")
            .unwrap()
            .unwrap()
            .disabled,
        "the driver must put the operator's deny switch back"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_driver_refuses_when_another_thread_is_active() {
    let (m, addr, tmp) = start().await;
    let lock = tmp.path().join("stress.lock");
    let cwd = tmp.path().to_string_lossy().to_string();

    // Someone else's thread is mid-turn, and STAYS mid-turn until this test
    // releases it. This used to be `slowturn` (a fixed 0.7 s sleep), which made
    // the test a race against the driver binary's own start-up: when the
    // driver took longer than the other turn to look (the first exec after a
    // relink, or load), it correctly saw nothing active and exited 0 — the
    // R4.S3-land inspector's F1, `left: 0 right: 3` at elapsed_ms ~3.2 s; the
    // same race cauldron's moved copy fixed in R3.S2.M. `holdturn` runs until
    // the release file exists (20 s fixture ceiling), so the window is no
    // longer a guess about scheduling (R4.S3b).
    let release = tmp.path().join("release-other-turn");
    let other = m
        .create_thread(None, threads::ThreadSettings::new(&cwd, "sonnet"))
        .unwrap();
    m.send(
        &other,
        format!("holdturn release={}", release.display()),
        threads::SendIntent::Immediate,
        Value::Null,
        false,
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let lock2 = lock.clone();
    let cwd2 = cwd.clone();
    let (code, report) = tokio::task::spawn_blocking(move || {
        run_driver(
            addr,
            &lock2,
            &["--new-thread", "--cwd", &cwd2, "--text", "hi"],
        )
    })
    .await
    .unwrap();

    assert_eq!(code, 3, "report: {report}");
    assert_eq!(report["outcome"], "refused");
    assert!(!report["active"].as_array().unwrap().is_empty());
    // It never created a thread, never sent anything.
    assert_eq!(m.store.list_threads(100).unwrap().len(), 1);
    // A REFUSAL still releases the lock. `die()` exits the process, which runs
    // no destructors, so this only holds because the exit path releases it
    // explicitly — otherwise every refusal poisons the next run.
    assert!(
        !lock.exists(),
        "a refused run must not leave its lock file behind"
    );

    // A second driver cannot run while one holds the lock either — and the
    // refusal leaves the OTHER run's lock exactly where it found it.
    std::fs::write(&lock, "999999").unwrap();
    let lock3 = lock.clone();
    let (code, report) = tokio::task::spawn_blocking(move || {
        run_driver(addr, &lock3, &["--new-thread", "--cwd", &cwd, "--text", "hi"])
    })
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(code, 3, "report: {report}");
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap().trim(),
        "999999",
        "refusing must never delete the lock another run holds"
    );
    // Let the held turn finish rather than ride out the fixture's ceiling.
    std::fs::write(&release, "").unwrap();
}

/// The lock is taken BEFORE the daemon is contacted, so the failure path that
/// matters most is the one where there is no daemon at all: a mistyped port, a
/// stopped daemon, an ad hoc invocation with no `--url`. That path exits
/// through `die()`, which does not run destructors — and it left a lock file
/// holding a dead PID sitting under `$THREADS_HOME` (observed 2026-07-29 under
/// `~/.threads`, the one directory this tooling must never touch), refusing
/// every later run until someone deleted it by hand.
#[tokio::test(flavor = "multi_thread")]
async fn a_failure_to_connect_leaves_no_lock_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let lock = tmp.path().join("stress.lock");
    // Bind a port and immediately drop it: nothing is listening there now.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };

    let lock2 = lock.clone();
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_threads-stress"))
            .arg("--url")
            .arg(format!("ws://{dead}/ws"))
            .arg("--lock")
            .arg(&lock2)
            .args(["--new-thread", "--text", "this never reaches a daemon"])
            .output()
            .expect("running threads-stress")
    })
    .await
    .unwrap();

    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("connecting to"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !lock.exists(),
        "a run that could not connect must not leave its lock file behind"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
