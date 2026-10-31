//! **R6.S3 — an environment's whole life, with documented commands only.**
//!
//! `environments.rs` (R6.S1) proves two environments RUN side by side once
//! somebody has hand-written their `env.toml`. This file proves the commands
//! an operator types to get there and back, and the two refusals the brief
//! names:
//!
//! * a hosted port that collides with PROD's is refused at `pin`, and a free
//!   one is accepted (opposed pair);
//! * a data root that RESOLVES to prod's (a symlinked environment directory)
//!   is refused, and a real one beside it is not;
//! * `env create --from` seeds pins + data + a fresh hosted port, and is
//!   refused while the source is running;
//! * `env remove` is a dry run without `--yes`, refuses prod and a running
//!   environment without `--stop`, and with `--stop --yes` leaves no process
//!   and no directory.
//!
//! Throwaway `HOME` per test; `CAULDRON_HOME`/`CAULDRON_ENV`/`ENV` removed
//! from every child. Every pid this file causes is killed on the way out.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

#[path = "support/first_exec.rs"]
mod first_exec;

const BIN: &str = env!("CARGO_BIN_EXE_cauldron");
const LABEL: &str = "0123456789ab-dev";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

struct Home(PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        // Every pid a hosted fixture wrote into a boot line under this HOME,
        // killed only if its cwd is still under this HOME (a reused pid is
        // somebody else's).
        let mut stack = vec![self.0.clone()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && !p.is_symlink() {
                    stack.push(p);
                } else if p.file_name().and_then(|n| n.to_str()) == Some("hosted-boots.log") {
                    let t = std::fs::read_to_string(&p).unwrap_or_default();
                    for pid in t.split_whitespace().filter_map(|w| w.strip_prefix("pid=")?.parse::<u32>().ok()) {
                        if is_ours(pid, &self.0) {
                            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
                        }
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn is_ours(pid: u32, home: &Path) -> bool {
    let Ok(home) = home.canonicalize() else { return false };
    let Ok(out) = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix('n'))
        .any(|cwd| Path::new(cwd).starts_with(&home))
}

fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn fresh(tag: &str) -> Home {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let h = std::env::temp_dir().join(format!(
        "cauldron-life-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&h);
    std::fs::create_dir_all(&h).unwrap();
    Home(h)
}

fn data(home: &Path, env: &str) -> PathBuf {
    home.join(".cauldron/data").join(env)
}

fn cauldron(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN)
        .env("HOME", home)
        .env_remove("CAULDRON_HOME")
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .args(args)
        .output()
        .expect("spawn cauldron");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

const HOSTED_SCRIPT: &str = r#"#!/bin/sh
exec /usr/bin/python3 -c '
import os, socket
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(os.environ["HOSTED_PORT"]))); s.listen(8)
open("hosted-boots.log", "a").write("env=%s port=%s pid=%d\n" % (os.environ["CAULDRON_ENV"], os.environ["HOSTED_PORT"], os.getpid()))
while True:
    c, _ = s.accept(); c.close()
'
"#;

/// **The declared period on the path from a new holder's start to its first
/// launched service's row (R9.F1)**: a new environment's policy is the
/// default (`poll_secs` 4, `down_after` 3, `auto_restart = false`), so the
/// holder launches its pin after `down_after` x `poll_secs` = 12 s of
/// liveness misses, and that launch is bounded by `HOSTED_BIND_WAIT` (45 s,
/// `cli/src/daemon.rs`). With `auto_restart = false` there is no retry, so
/// nothing longer is on the path. 12 + 45 = 57 s.
const FIRST_ROW_DECLARED: Duration = Duration::from_secs(3 * 4 + 45);

/// A FIRST wait for a holder's rows: [`FIRST_ROW_DECLARED`] plus the
/// measured worst-case first exec (`support/first_exec.rs`) of the two fresh
/// files on that path — the holder's own launch path and the `hostedecho`
/// fixture — both paid before the clock starts (`place_launch_path`,
/// `root_with_hosted`) and still budgeted, so a pre-pay that stops reaching
/// one reads as slow, not red. Replaces a fixed 40 s, which under a busy
/// scanner the two first execs alone could exceed (R9.C5b).
fn first_row() -> Duration {
    first_exec::first_wait(FIRST_ROW_DECLARED, 2)
}

/// Place `env`'s own launch path (`cauldron --env <env> promote`, the
/// documented way, the same copy `daemon` would make) and pay its first exec
/// here, outside every deadline, so the holder's re-exec onto it is not its
/// first (R9.C5b's offered patch, taken by R9.F1).
fn place_launch_path(home: &Path, env: &str) {
    let (rc, out, err) = cauldron(home, &["--env", env, "promote"]);
    assert_eq!(rc, 0, "{out}{err}");
    let own = home.join(".cauldron/bin").join(format!("cauldron-{env}"));
    assert!(own.is_file(), "`promote` did not place {}: {out}", own.display());
    first_exec::pay_first_exec(
        Command::new(&own)
            .env("HOME", home)
            .env_remove("CAULDRON_HOME")
            .env_remove("CAULDRON_ENV")
            .env_remove("ENV")
            .args(["envs"]),
        &format!("{env}'s launch path"),
    );
}

/// A root with the `hostedecho` declaration (declared port `port`) and one
/// installed build of it.
fn root_with_hosted(home: &Path, port: u16) {
    let svc = home.join(".cauldron/services");
    std::fs::create_dir_all(&svc).unwrap();
    std::fs::write(
        svc.join("hostedecho.toml"),
        format!(
            r#"name        = "hostedecho"
description = "test fixture: a foreign daemon on a declared port"
kind        = "daemon"
registers   = false
probe       = "connect"
stop_signal = "TERM"
escalate    = true
start_timeout_ms = {START_TIMEOUT_MS}

[launch]
addr = "127.0.0.1:{port}"

[env]
HOSTED_PORT = "${{CAULDRON_HOSTED_PORT}}"

[build]
artifact = "/nowhere/hostedecho"
bin      = "hostedecho"
profile  = "release"
"#
        ),
    )
    .unwrap();
    let dir = home.join(".cauldron/data/shared/builds/hostedecho").join(LABEL);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("hostedecho"), launch_wait::slow_launch_sh(&first_exec::slow_first_exec_sh(HOSTED_SCRIPT, &home.join("first-exec-marks")), &home.join("f2g-marks")))
        .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("hostedecho"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // R9.F1: pay the fixture's first exec here, outside every deadline: the
    // holder execs this very file inside `HOSTED_BIND_WAIT`, and with
    // `auto_restart = false` a launch that overran is never retried. No
    // `HOSTED_PORT`, so the python raises before it binds or writes a boot
    // line; only the exec matters.
    first_exec::pay_first_exec(
        Command::new(dir.join("hostedecho")).current_dir(home).env_remove("HOSTED_PORT"),
        "the hostedecho fixture",
    );
    let rec = serde_json::json!({
        "service": "hostedecho", "label": LABEL, "bin": "hostedecho", "package": "hostedecho",
        "profile": "dev", "workspace": "/nowhere", "built_at": 1,
        "commit": COMMIT, "dirty": false, "content": "0123456789abcdef", "source_commit": null
    });
    std::fs::write(dir.join("build.json"), serde_json::to_string_pretty(&rec).unwrap()).unwrap();
}

fn pid_in(home: &Path, env: &str) -> u32 {
    let t = std::fs::read_to_string(data(home, env).join("seams.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
    v["seams"]["hostedecho"]["pid"].as_u64().expect("a hostedecho row") as u32
}

/// **A port collision with prod is refused at `pin`; a free port is not.**
///
/// Opposed: the same command with `--addr` on a free port succeeds and writes
/// the override, so a `pin` that refused every hosted service fails it, and
/// one that accepted everything fails both refusals. Prod's `env.toml` is
/// asserted byte-identical across the whole test.
#[test]
fn a_hosted_port_colliding_with_prod_is_refused_and_a_free_one_is_accepted() {
    let home = fresh("port");
    let h = &home.0;
    let declared = free_port();
    root_with_hosted(h, declared);
    let (rc, _, err) = cauldron(h, &["pin", "hostedecho", LABEL]);
    assert_eq!(rc, 0, "prod pins the declared port: {err}");
    let prod_before = std::fs::read(data(h, "prod").join("env.toml")).unwrap();

    let (rc, _, err) = cauldron(h, &["env", "create", "dev"]);
    assert_eq!(rc, 0, "{err}");
    // no --addr: dev would inherit the declaration's port, which prod claims
    let (rc, _, err) = cauldron(h, &["--env", "dev", "pin", "hostedecho", LABEL]);
    assert_ne!(rc, 0, "pin onto prod's declared port must be refused");
    assert!(err.contains("COLLIDES") && err.contains("environment `prod`"), "{err}");
    // explicit --addr on prod's port
    let (rc, _, err) =
        cauldron(h, &["--env", "dev", "pin", "hostedecho", LABEL, "--addr", &format!("127.0.0.1:{declared}")]);
    assert_ne!(rc, 0, "an explicit --addr on prod's port must be refused");
    assert!(err.contains("COLLIDES"), "{err}");
    // and nothing was written by either refusal
    let dev_toml = std::fs::read_to_string(data(h, "dev").join("env.toml")).unwrap();
    assert!(!dev_toml.contains("hostedecho"), "a refused pin wrote: {dev_toml}");
    // a free port is accepted and recorded
    let mine = free_port();
    let (rc, out, err) =
        cauldron(h, &["--env", "dev", "pin", "hostedecho", LABEL, "--addr", &format!("127.0.0.1:{mine}")]);
    assert_eq!(rc, 0, "a free port must be accepted: {out}{err}");
    let dev_toml = std::fs::read_to_string(data(h, "dev").join("env.toml")).unwrap();
    assert!(dev_toml.contains(&format!("127.0.0.1:{mine}")), "{dev_toml}");
    assert_eq!(std::fs::read(data(h, "prod").join("env.toml")).unwrap(), prod_before, "prod's env.toml changed");
}

/// **A data root that resolves to prod's is refused; a real one is not.**
#[test]
fn an_environment_whose_data_root_resolves_to_prods_is_refused() {
    let home = fresh("alias");
    let h = &home.0;
    root_with_hosted(h, free_port());
    let (rc, _, err) = cauldron(h, &["pin", "hostedecho", LABEL]);
    assert_eq!(rc, 0, "{err}");
    std::os::unix::fs::symlink(data(h, "prod"), data(h, "evil")).unwrap();
    for verb in [&["--env", "evil", "env", "show"][..], &["--env", "evil", "status"][..], &["--env", "evil", "daemon", "--once"][..]] {
        let (rc, out, err) = cauldron(h, verb);
        assert_ne!(rc, 0, "{verb:?} must be refused: {out}");
        assert!(err.contains("RESOLVES TO") && err.contains("PRODUCTION"), "{verb:?}: {err}");
    }
    // `env create` refuses too (the name exists, as a link)
    let (rc, _, _) = cauldron(h, &["env", "create", "evil"]);
    assert_ne!(rc, 0);
    // the opposing half: a real environment beside it resolves fine, and the
    // listing still works, naming the refused one
    let (rc, _, err) = cauldron(h, &["env", "create", "real"]);
    assert_eq!(rc, 0, "{err}");
    let (rc, out, err) = cauldron(h, &["--env", "real", "env", "show"]);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("data/real"), "{out}");
    let (rc, out, err) = cauldron(h, &["envs"]);
    assert_eq!(rc, 0, "{err}");
    assert!(err.contains("environment `evil`") && out.contains("prod"), "{out}{err}");
}

/// **Seed, run, isolate, tear down — every step a `cauldron` command.**
#[test]
fn seed_from_another_environment_then_remove_it_with_nothing_left_running() {
    let home = fresh("seed");
    let h = &home.0;
    let declared = free_port();
    root_with_hosted(h, declared);

    let alpha_port = free_port();
    assert_eq!(cauldron(h, &["env", "create", "alpha"]).0, 0);
    let (rc, _, err) =
        cauldron(h, &["--env", "alpha", "pin", "hostedecho", LABEL, "--addr", &format!("127.0.0.1:{alpha_port}")]);
    assert_eq!(rc, 0, "{err}");
    // R9.C4 (gap C-4): `start` records good only once a build has stayed up
    // `stable_after_secs`. This test needs alpha to HAVE a good label (the
    // opposing half of the per-environment check below), so alpha declares
    // the period zero — the documented way to ask for start to count.
    let alpha_toml = data(h, "alpha").join("env.toml");
    let t = std::fs::read_to_string(&alpha_toml).unwrap();
    assert!(t.contains("stable_after_secs = 60"), "premise: alpha's env.toml states the default period: {t}");
    std::fs::write(&alpha_toml, t.replace("stable_after_secs = 60", "stable_after_secs = 0")).unwrap();
    let (rc, out, err) = cauldron(h, &["--env", "alpha", "start", "hostedecho"]);
    assert_eq!(rc, 0, "{out}{err}");
    let alpha_pid = pid_in(h, "alpha");
    assert!(alive(alpha_pid));

    // seeding from a RUNNING environment is refused, and leaves nothing
    let (rc, _, err) = cauldron(h, &["env", "create", "beta", "--from", "alpha"]);
    assert_ne!(rc, 0);
    assert!(err.contains("RUNNING"), "{err}");
    assert!(!data(h, "beta").exists(), "a refused seed left a directory");
    // removing a running environment without --stop is refused; dry run is rc 2
    let (rc, out, _) = cauldron(h, &["env", "remove", "alpha"]);
    assert_eq!(rc, 2, "{out}");
    assert!(out.contains("DRY RUN") && data(h, "alpha").exists());
    let (rc, _, err) = cauldron(h, &["env", "remove", "alpha", "--yes"]);
    assert_ne!(rc, 0);
    assert!(err.contains("RUNNING") && alive(alpha_pid), "{err}");

    // stop alpha, then seed beta from it
    assert_eq!(cauldron(h, &["--env", "alpha", "stop"]).0, 0);
    let (rc, out, err) = cauldron(h, &["env", "create", "beta", "--from", "alpha"]);
    assert_eq!(rc, 0, "{out}{err}");
    let seeded = data(h, "beta").join("hostedecho/hosted-boots.log");
    assert_eq!(
        std::fs::read_to_string(&seeded).unwrap(),
        std::fs::read_to_string(data(h, "alpha").join("hostedecho/hosted-boots.log")).unwrap(),
        "beta's data is a copy of alpha's"
    );
    let beta_toml = std::fs::read_to_string(data(h, "beta").join("env.toml")).unwrap();
    assert!(beta_toml.contains(LABEL), "{beta_toml}");
    assert!(!beta_toml.contains(&format!(":{alpha_port}")), "the source's port was copied: {beta_toml}");
    // `good` is per environment: alpha's start recorded LABEL good there, and a
    // seed that copied it would serialise `good = ["…"]` here.
    assert!(
        std::fs::read_to_string(data(h, "alpha").join("env.toml")).unwrap().contains(&format!("good = [\"{LABEL}\"]")),
        "the opposing half: alpha DID record a good label"
    );
    assert!(!beta_toml.contains(&format!("good = [\"{LABEL}")), "`good` is per environment and was copied: {beta_toml}");

    // beta runs on its own port; its write lands in beta only
    let (rc, out, err) = cauldron(h, &["--env", "beta", "start", "hostedecho"]);
    assert_eq!(rc, 0, "{out}{err}");
    let beta_pid = pid_in(h, "beta");
    let alpha_log = std::fs::read_to_string(data(h, "alpha").join("hostedecho/hosted-boots.log")).unwrap();
    assert!(!alpha_log.contains("env=beta"), "beta wrote into alpha: {alpha_log}");
    assert!(std::fs::read_to_string(&seeded).unwrap().contains("env=beta"));

    // prod is never removed
    let (rc, _, err) = cauldron(h, &["env", "remove", "prod", "--yes", "--stop"]);
    assert_ne!(rc, 0);
    assert!(err.contains("never removed"), "{err}");

    // teardown: no process, no directory
    let (rc, out, err) = cauldron(h, &["env", "remove", "beta", "--stop", "--yes"]);
    assert_eq!(rc, 0, "{out}{err}");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!alive(beta_pid), "beta's service survived its removal");
    assert!(!data(h, "beta").exists(), "beta's data root survived its removal");
    assert!(data(h, "alpha").exists(), "removing beta touched alpha");
}

/// **`build` is per ROOT and must not bring an environment into existence.**
/// Before R6.S3, `cauldron --env <name> build ...` ran `ensure_dirs`, which
/// created `data/<name>/` — after which `env create <name>` was refused as
/// "already exists". Found doing the live proof. Opposed: the create that
/// follows must SUCCEED, and the environment must not exist before it.
#[test]
fn building_while_naming_an_environment_does_not_create_it() {
    let home = fresh("build");
    let h = &home.0;
    root_with_hosted(h, free_port());
    // the build itself fails (its artifact path is /nowhere); what matters is
    // what it left behind
    let _ = cauldron(h, &["--env", "ghost", "build", "hostedecho"]);
    assert!(h.join(".cauldron/data/shared/builds").is_dir(), "the root-level dirs are still made");
    assert!(!data(h, "ghost").exists(), "`build` created environment `ghost`");
    let (rc, out, err) = cauldron(h, &["env", "create", "ghost"]);
    assert_eq!(rc, 0, "{out}{err}");
}

/// **A new environment's daemon RUNS what it pins** — once — even though its
/// policy is `auto_restart = false` (the default for every new environment).
/// Found on the R6.S3 live proof: `env create`, `pin`, `daemon`, and nothing
/// ran, ever. Opposed: after the first launch, a death is NOT relaunched — the
/// policy still governs RE-launching. The wait outlasts `down_after` (3) x
/// `poll_secs` (4 s) = 12 s, the longest period on that path (doctrine 4).
#[test]
fn a_new_environments_daemon_launches_its_pins_once_and_does_not_relaunch_past_12s() {
    let home = fresh("first");
    let h = &home.0;
    root_with_hosted(h, free_port());
    assert_eq!(cauldron(h, &["env", "create", "solo"]).0, 0);
    let port = free_port();
    let (rc, _, err) =
        cauldron(h, &["--env", "solo", "pin", "hostedecho", LABEL, "--addr", &format!("127.0.0.1:{port}")]);
    assert_eq!(rc, 0, "{err}");
    place_launch_path(h, "solo");
    let log = std::fs::File::create(h.join("daemon.log")).unwrap();
    let mut daemon = Command::new(BIN)
        .env("HOME", h)
        .env_remove("CAULDRON_HOME")
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .args(["--env", "solo", "daemon"])
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let boots = data(h, "solo").join("hostedecho/hosted-boots.log");
    let t0 = std::time::Instant::now();
    // the ROW, not the boot line: the boot line is written before the
    // supervisor records the row, and the pid comes from the row
    let row = || {
        std::fs::read_to_string(data(h, "solo").join("seams.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["seams"]["hostedecho"]["pid"].as_u64())
    };
    let within = first_row();
    while row().is_none() && t0.elapsed() < within {
        std::thread::sleep(Duration::from_millis(200));
    }
    let launched = row().is_some() && boots.exists();
    if launched {
        // kill the service; the daemon must NOT bring it back
        let pid = pid_in(h, "solo");
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        std::thread::sleep(Duration::from_secs(20));
    }
    let _ = daemon.kill();
    let _ = daemon.wait();
    let dlog = std::fs::read_to_string(h.join("daemon.log")).unwrap_or_default();
    assert!(launched, "the daemon never launched its pinned service within {within:?}:\n{dlog}");
    let n = std::fs::read_to_string(&boots).unwrap().lines().count();
    assert_eq!(n, 1, "auto_restart=false relaunched a dead service ({n} boots):\n{dlog}");
}

// ═══════════════════════════════════════════════════ R6.S5 — `cauldron down` ══

/// Holder pids this test started; each is killed on the way out, but only if
/// its command line is still a `… daemon` running from under THIS test's HOME
/// (a reused pid is somebody else's).
///
/// R6.S5b: the guard used to require `BIN` (this target's `cauldron`) in the
/// command line, and it never matched — a holder runs from its environment's
/// own launch path, `<HOME>/.cauldron/bin/cauldron-<env>` (`ps` shows that
/// path, not `BIN`), so every holder a FAILING test left behind survived the
/// test (five leaked `cauldron-fakeprod … daemon` holders from five forced
/// reds, measured). Green runs hid it: there `down` stops them first.
///
/// R9.F1: the guard runs on EVERY exit path, a panic included (it is a
/// drop guard; `holders` is declared after `home`, so it drops first, and
/// `Home`'s own guard then kills the services by their boot lines). The pid
/// is registered the moment the holder is SPAWNED (`$!` from the detaching
/// shell — the holder later execs onto its own launch path, which keeps the
/// pid), not when its seam row appears: a red before the row (the 40 s wait
/// timing out under load) left the holder with nobody holding its pid, and
/// 14 such `cauldron-life-down*` holders were found with ppid 1, some 4 h
/// old. Identity before the kill: a `… daemon` command line running either
/// THIS test's `BIN` (not yet exec'd) or a file under THIS test's HOME.
/// Each kill is then waited out, so a holder is gone, not merely signalled,
/// when the guard returns.
struct Holders(Vec<u32>, PathBuf);
impl Drop for Holders {
    fn drop(&mut self) {
        let home = self.1.display().to_string();
        for &pid in &self.0 {
            let ours = command_of(pid).map(|c| c.contains("daemon") && (c.contains(&home) || c.contains(BIN)));
            if ours != Some(true) {
                continue;
            }
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            let t0 = std::time::Instant::now();
            while running(pid) && t0.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(50));
            }
            eprintln!("R9.F1 Holders guard: killed holder pid {pid} (gone: {})", !running(pid));
        }
    }
}

/// The command line of `pid`, or None when no such process exists. A zombie
/// is reported as None: `kill -0` succeeds on one, so `alive` alone would let
/// an unreaped holder read as running (the holders here are detached, so
/// launchd reaps them, but the probe must not depend on that).
fn command_of(pid: u32) -> Option<String> {
    let out = Command::new("ps").args(["-o", "stat=,command=", "-p", &pid.to_string()]).output().ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if t.is_empty() || t.starts_with('Z') {
        return None;
    }
    Some(t)
}

fn running(pid: u32) -> bool {
    command_of(pid).is_some()
}

/// A row's pid from `seams.json`, if the row is there.
fn row_pid(home: &Path, env: &str, service: &str) -> Option<u32> {
    let t = std::fs::read_to_string(data(home, env).join("seams.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&t).ok()?;
    v["seams"][service]["pid"].as_u64().map(|p| p as u32)
}

/// Start `env`'s holder DETACHED (as the operator does, `nohup … &`): the
/// shell exits, the holder is reparented, and nothing in this process waits on
/// it. Returns `(holder pid, hostedecho pid)` once both rows exist and both
/// pids are running.
fn detached_holder(home: &Path, env: &str, holders: &mut Holders) -> (u32, u32) {
    let log = home.join(format!("daemon-{env}.log"));
    let st = Command::new("/bin/sh")
        .env("HOME", home)
        .env_remove("CAULDRON_HOME")
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .args(["-c", &format!("'{BIN}' --env {env} daemon > '{}' 2>&1 < /dev/null & echo $!", log.display())])
        .stderr(Stdio::inherit())
        .output()
        .unwrap();
    assert!(st.status.success());
    // R9.F1: the holder's pid, from the shell that detached it, onto the kill
    // list BEFORE anything below can panic (see `Holders`).
    let spawned: u32 = String::from_utf8_lossy(&st.stdout).trim().parse().expect("the detached holder's pid ($!)");
    holders.0.push(spawned);
    let t0 = std::time::Instant::now();
    let within = first_row();
    loop {
        // The holder's pid goes to the kill list the moment ITS row exists,
        // not once both rows do: a red before the service row used to leak
        // the holder (R9.C5b finding 2; nine `cauldron-life-*` holders with
        // ppid 1 were left running in r31).
        if let Some(h) = row_pid(home, env, "cauldron") {
            if !holders.0.contains(&h) {
                holders.0.push(h);
            }
        }
        if let (Some(h), Some(s)) = (row_pid(home, env, "cauldron"), row_pid(home, env, "hostedecho")) {
            if running(h) && running(s) {
                return (h, s);
            }
        }
        if t0.elapsed() > within {
            panic!("`{env}`'s holder never came up with its service:\n{}", std::fs::read_to_string(&log).unwrap_or_default());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Every direct child of `pid`, by `ps` (not `pgrep -f`, which can match itself).
///
/// **DIRECT children only, deliberately (R6.S5b).** What `down` owns is the
/// holder and each service a holder launched — the rows of the seam table, each
/// stopped by its own pid and each leading its own process group
/// (`cmd.process_group(0)` at launch); in this test they are also the holder's
/// direct children, which is what the sentinel below asserts. A service's OWN children are that
/// service's to stop, by the service's own stop path, and `down` reaches them
/// only through it: `threads` puts each turn's `claude` in a process group of
/// its own (`services/threads/src/runtime.rs`, "The turn's child leads its OWN
/// process group"), so a signal to the service's group would not reach them
/// anyway, and a turn in flight across a `threads` restart is ADOPTED by the
/// next one (`services/threads/tests/adopted_turn_landing.rs`) — a `down` that
/// swept the whole tree would kill turns the design keeps alive on purpose.
/// The `hostedecho` fixture has no children, so a grandchild check here would
/// be a search that can find nothing (doctrine 9). Whether `down` should end
/// adopted turns is a policy question, recorded in the R6.S5b report.
fn children_of(pid: u32) -> Vec<u32> {
    let out = Command::new("ps").args(["-A", "-o", "pid=,ppid="]).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let (c, p) = (w.next()?.parse::<u32>().ok()?, w.next()?.parse::<u32>().ok()?);
            (p == pid).then_some(c)
        })
        .collect()
}

fn env_with_service(home: &Path, env: &str) {
    let (rc, _, err) = cauldron(home, &["env", "create", env]);
    assert_eq!(rc, 0, "{err}");
    let (rc, _, err) =
        cauldron(home, &["--env", env, "pin", "hostedecho", LABEL, "--addr", &format!("127.0.0.1:{}", free_port())]);
    assert_eq!(rc, 0, "{err}");
    place_launch_path(home, env);
}

/// **`down` stops an environment's holder AND its services, keeps its data,
/// and touches no other environment; `daemon` brings it back.**
///
/// R6.S4 F2: `kill -TERM <holder>` left its threads orphaned and serving.
/// Every liveness check is by pid from the seam rows, read with `ps` (a
/// zombie counts as gone), never `pgrep -f`. Opposed: before `down` every pid
/// is asserted running (so "gone" can be false), and the OTHER environment's
/// holder and service are asserted still running after it (so a `down` that
/// stopped everything in the root fails).
#[test]
fn down_stops_the_holder_and_its_services_keeps_the_data_and_leaves_other_envs_running() {
    let home = fresh("down");
    let h = &home.0;
    let mut holders = Holders(Vec::new(), h.clone());
    root_with_hosted(h, free_port());
    env_with_service(h, "alpha");
    env_with_service(h, "beta");
    let (a_holder, a_svc) = detached_holder(h, "alpha", &mut holders);
    let (b_holder, b_svc) = detached_holder(h, "beta", &mut holders);
    let a_children = children_of(a_holder);
    assert!(a_children.contains(&a_svc), "the sentinel: alpha's service is its holder's child ({a_children:?})");
    let toml_before = std::fs::read(data(h, "alpha").join("env.toml")).unwrap();

    let (rc, out, err) = cauldron(h, &["--env", "alpha", "down"]);
    assert_eq!(rc, 0, "{out}{err}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!running(a_holder), "alpha's holder {a_holder} survived `down`");
    assert!(!running(a_svc), "alpha's service {a_svc} survived `down` (orphaned, R6.S4 F2)");
    for c in &a_children {
        assert!(!running(*c), "a child {c} of alpha's holder survived `down`");
    }
    // the dead holder's row is cleared, so `envs` does not name a gone pid
    assert_eq!(row_pid(h, "alpha", "cauldron"), None, "`down` left the dead holder's row in the table");
    // the data, the pins and the listing are kept
    assert_eq!(std::fs::read(data(h, "alpha").join("env.toml")).unwrap(), toml_before, "`down` changed env.toml");
    assert!(data(h, "alpha").join("hostedecho/hosted-boots.log").is_file(), "`down` lost the service's data");
    let (rc, out, _) = cauldron(h, &["envs"]);
    assert_eq!(rc, 0);
    assert!(out.lines().any(|l| l.contains("alpha")), "alpha is no longer listed:\n{out}");
    // the other environment is untouched
    assert!(running(b_holder) && running(b_svc), "`--env alpha down` stopped beta");
    // a second `down` is a no-op, not an error
    let (rc, out, err) = cauldron(h, &["--env", "alpha", "down"]);
    assert_eq!(rc, 0, "{out}{err}");
    assert!(out.contains("nothing is running"), "{out}");
    // and `daemon` brings it back on the same data
    let (a2_holder, a2_svc) = detached_holder(h, "alpha", &mut holders);
    assert_ne!(a2_holder, a_holder);
    assert_ne!(a2_svc, a_svc);
    let boots = std::fs::read_to_string(data(h, "alpha").join("hostedecho/hosted-boots.log")).unwrap();
    assert_eq!(boots.lines().count(), 2, "the restarted service did not append to the kept data:\n{boots}");
    for env in ["alpha", "beta"] {
        assert_eq!(cauldron(h, &["--env", env, "down"]).0, 0);
    }
    assert!(!running(a2_holder) && !running(a2_svc) && !running(b_holder) && !running(b_svc));
}

/// **`down` refuses a SUPERVISED environment without `--supervised`.**
///
/// Never real prod: a throwaway root, and an environment `fakeprod` made
/// supervised by a fake plist at the path `launchd::plist_path` reads (under
/// this test's own `HOME`), plus `prod` refused by name in the same root.
/// Opposed: without the flag the holder and service are still RUNNING after
/// the refusal; with it, both are gone — so a `down` that refused everything
/// and one that refused nothing each fail.
#[test]
fn down_refuses_a_launchd_supervised_environment_without_the_flag_and_stops_it_with_it() {
    let home = fresh("downsup");
    let h = &home.0;
    let mut holders = Holders(Vec::new(), h.clone());
    root_with_hosted(h, free_port());
    env_with_service(h, "fakeprod");
    let agents = h.join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("com.broomstick.cauldron-fakeprod.plist"), "<plist/>").unwrap();
    let (holder, svc) = detached_holder(h, "fakeprod", &mut holders);

    // Each refusal is asserted by text ONLY that refusal produces (R6.S5b; the
    // R6.S5 inspector found `contains("launchd")` matched both, since both
    // messages go on to say "Under launchd KeepAlive ..."). The plist arm names
    // the plist it found; the prod arm says "the operator's live environment".
    let plist = agents.join("com.broomstick.cauldron-fakeprod.plist");
    let (rc, out, err) = cauldron(h, &["--env", "fakeprod", "down"]);
    assert_ne!(rc, 0, "a launchd-supervised environment must be refused: {out}");
    assert!(err.contains("REFUSED"), "{err}");
    assert!(
        err.contains(&format!("it is installed under launchd ({})", plist.display())),
        "the plist refusal must name the plist it found: {err}"
    );
    assert!(!err.contains("operator's live environment"), "fakeprod was refused as if it were prod: {err}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(running(holder) && running(svc), "a refused `down` stopped something");

    // prod, by name, with no plist at all (it exists: its pin made it)
    let (rc, _, err) = cauldron(h, &["pin", "hostedecho", LABEL]);
    assert_eq!(rc, 0, "{err}");
    // the sentinel: no prod plist under this HOME, so only the name arm can refuse
    assert!(!agents.join("com.broomstick.cauldron.plist").exists() && !agents.join("ai.broomstick.cauldron.plist").exists());
    let (rc, _, err) = cauldron(h, &["--env", "prod", "down"]);
    assert_ne!(rc, 0);
    assert!(err.contains("REFUSED"), "{err}");
    assert!(err.contains("it is `prod`, the operator's live environment"), "prod must be refused by name: {err}");
    assert!(!err.contains("installed under launchd"), "prod (no plist here) was refused as a plist env: {err}");

    let (rc, out, err) = cauldron(h, &["--env", "fakeprod", "down", "--supervised"]);
    assert_eq!(rc, 0, "{out}{err}");
    assert!(out.contains("WARNING"), "{out}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!running(holder) && !running(svc), "`down --supervised` left a process running");
}

/// **REL-4.S8b: `pin --addr auto` — only prod binds the declared port, and
/// only while it is free; every other environment gets its own.**
///
/// The declared port here is a scratch one from the kernel (never a real prod
/// port). Arms, opposed: prod with the declared port free keeps it (no
/// override written); `dev` with that port free and unclaimed still gets a
/// different one; prod with the declared port HELD by a listener gets its own.
#[test]
fn pin_addr_auto_gives_only_prod_the_declared_port_and_only_when_free() {
    let home = fresh("auto");
    let h = &home.0;
    let declared = free_port();
    root_with_hosted(h, declared);

    // dev first, while prod pins nothing: the declared port is free and
    // unclaimed, and dev must STILL not take it.
    let (rc, _, err) = cauldron(h, &["env", "create", "dev"]);
    assert_eq!(rc, 0, "{err}");
    let (rc, out, err) = cauldron(h, &["--env", "dev", "pin", "hostedecho", LABEL, "--addr", "auto"]);
    assert_eq!(rc, 0, "{out}{err}");
    let dev_toml = std::fs::read_to_string(data(h, "dev").join("env.toml")).unwrap();
    let dev_addr = dev_toml
        .lines()
        .find_map(|l| l.trim().strip_prefix("addr = \"")?.strip_suffix('"').map(str::to_string))
        .unwrap_or_else(|| panic!("dev got no address override: {dev_toml}"));
    let dev_port: u16 = dev_addr.rsplit(':').next().unwrap().parse().unwrap();
    assert_ne!(dev_port, declared, "a non-prod env was given the declared port: {dev_toml}");
    for p in [3651u16, 3653, 3753, 9010] {
        assert_ne!(dev_port, p, "a non-prod env was given prod's port {p}");
    }

    // prod, declared port free and claimed by nobody else: keeps it.
    let (rc, out, err) = cauldron(h, &["pin", "hostedecho", LABEL, "--addr", "auto"]);
    assert_eq!(rc, 0, "{out}{err}");
    assert!(out.contains("keeps its declared address"), "{out}");
    let prod_toml = std::fs::read_to_string(data(h, "prod").join("env.toml")).unwrap();
    assert!(!prod_toml.contains("addr ="), "prod with a free default got an override: {prod_toml}");

    // prod2-as-prod is not possible in one root, so the busy arm re-pins prod
    // with the declared port HELD: it must get its own port instead.
    let hold = std::net::TcpListener::bind(("127.0.0.1", declared)).expect("hold the declared port");
    let (rc, out, err) = cauldron(h, &["pin", "hostedecho", LABEL, "--addr", "auto"]);
    drop(hold);
    assert_eq!(rc, 0, "{out}{err}");
    assert!(out.contains("gets its own port"), "{out}");
    let prod_toml = std::fs::read_to_string(data(h, "prod").join("env.toml")).unwrap();
    assert!(prod_toml.contains("addr = \"127.0.0.1:"), "prod with a busy default kept it: {prod_toml}");
    assert!(!prod_toml.contains(&format!(":{declared}\"")), "{prod_toml}");
}

/// **REL-4.S8c (inspect REL-4.S8b-I N1): `build --env-copy` never writes the
/// shared version.** A plain build installs `<label>` — the directory every
/// environment in the root shares and prod may be pinned to. `--env-copy` from
/// `dev` installs `<label>+dev`, then `+dev.2`, and the shared image and its
/// build.json are byte-identical before and after both. Opposed: a second
/// PLAIN build of the same bytes DOES rewrite the shared build.json (a new
/// `built_at`), so the byte check can see a write.
#[test]
fn build_env_copy_installs_a_new_label_and_never_writes_the_shared_one() {
    let root = std::env::temp_dir().join(format!(
        "cauldron-envcopy-it-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(root.join("services")).unwrap();
    let art = root.join("art.sh");
    std::fs::write(&art, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(
        root.join("services/ec.toml"),
        format!(
            "name = \"ec\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
             [launch]\naddr = \"127.0.0.1:1\"\n[build]\nartifact = \"{}\"\nbin = \"ec\"\n",
            art.display()
        ),
    )
    .unwrap();
    let run = |env_name: &str, extra: &[&str]| -> serde_json::Value {
        let mut args = vec!["--env", env_name, "build", "ec", "--json"];
        args.extend_from_slice(extra);
        let out = Command::new(BIN)
            .args(&args)
            .env("CAULDRON_HOME", &root)
            .env_remove("CAULDRON_ENV")
            .env_remove("ENV")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(out.status.success(), "{args:?}: {text}{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
    };
    let label = run("prod", &[])[0]["label"].as_str().unwrap().to_string();
    let shared = root.join("data/shared/builds/ec").join(&label);
    let snap = || (std::fs::read(shared.join("ec")).unwrap(), std::fs::read(shared.join("build.json")).unwrap());
    let before = snap();
    std::thread::sleep(Duration::from_millis(1100)); // built_at is in seconds
    let c1 = run("dev", &["--env-copy"])[0]["label"].as_str().unwrap().to_string();
    let c2 = run("dev", &["--env-copy"])[0]["label"].as_str().unwrap().to_string();
    assert_eq!(c1, format!("{label}+dev"));
    assert_eq!(c2, format!("{label}+dev.2"));
    for c in [&c1, &c2] {
        let rec = std::fs::read_to_string(root.join("data/shared/builds/ec").join(c).join("build.json")).unwrap();
        assert!(rec.contains(&format!("\"label\": \"{c}\"")), "{c}: {rec}");
    }
    assert!(snap() == before, "`build --env-copy` wrote the shared version {label}");
    // Opposed: a plain rebuild of the same bytes rewrites the shared record.
    let again = run("dev", &[])[0]["label"].as_str().unwrap().to_string();
    assert_eq!(again, label);
    assert!(snap().1 != before.1, "the byte check cannot see a write to the shared build.json");
    let _ = std::fs::remove_dir_all(&root);
}
