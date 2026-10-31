//! **R9.C2 (gap C-2): A PIN TO A DIFFERENT BUILD REVIVES A PARKED SERVICE.**
//!
//! C-2: "The park warning says to use `pin` or `rollback`. He pins a fixed
//! build and nothing happens until the daemon restarts." A parked service
//! never looked at its pin again, so the operator's fix was invisible to the
//! supervisor that asked for it.
//!
//! Every test here is a THROWAWAY cauldron (its own `CAULDRON_HOME` under the
//! temp dir, its own environment, kernel-assigned ports) running hosted python
//! fakes — the same sandbox shape as `supervisor_restarts.rs`. Nothing reads or
//! writes `~/.cauldron`; nothing signals a live daemon.
//!
//! Claims (each forced red once — see the R9.C2 report):
//!
//! 1. `r9c2_a_parked_service_pinned_to_a_different_build_runs_it` — a build
//!    that binds and dies 1 s later parks; pinning a different, working build
//!    clears the park and the daemon launches it (a `launch` record with
//!    `why = repin` on the new label), with no daemon restart.
//! 2. `r9c2_a_parked_service_pinned_to_the_same_build_stays_parked_outlasting_6_polls`
//!    — the pair: the SAME build pinned again, by its label and then as
//!    `latest`, does not revive it. Asserted only after the daemon has said
//!    PARKED on at least 4 further passes after the pin (so it read the pin),
//!    over 6 s — past `poll_secs = 1` and the 2 s backoff ceiling. The passes
//!    are the daemon's own per-site count over `WARNINGS` (R9.F2c), not log
//!    lines, which the log's power-of-two dedup thins out.
//! 3. `r9c2_a_revive_after_a_rollback_keeps_the_rollback_record_and_good_history`
//!    — after an auto-rollback away from a bad build and a later park, a pin
//!    to a third build revives the service and leaves the persisted record
//!    alone: the ROLLBACK supervision naming the bad build, and `env.toml`'s
//!    good history (the good build still in it, the bad build never in it).

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

const FAKE: &str = r#"#!/usr/bin/env python3
# A fake hosted daemon: binds the port it is told and sits, accepting and
# closing. Appends its pid to a file the test reads, so every incarnation can
# be cleaned up.
import os, socket, sys, time
port = int(os.environ["FAKE_PORT"])
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen(8)
with open(os.environ["FAKE_PIDS"], "a") as f:
    f.write(f"{os.getpid()}\n")
while True:
    c, _ = s.accept()
    c.close()
"#;

/// The FAKE that binds, serves for `secs` seconds, then exits 3 — a build that
/// never proves itself, so the daemon gives up on it.
fn dying_fake(secs: u64) -> String {
    FAKE.replace(
        "while True:\n    c, _ = s.accept()\n    c.close()\n",
        &format!(
            "deadline = time.time() + {secs}\ns.settimeout(0.2)\nwhile time.time() < deadline:\n    \
             try:\n        c, _ = s.accept(); c.close()\n    except socket.timeout:\n        pass\n\
             sys.exit(3)\n"
        ),
    )
}

/// The working FAKE under a different build label (its bytes differ).
fn fixed_fake(tag: &str) -> String {
    format!("{FAKE}# fixed build {tag}\n")
}

/// The supervisor's pass period in every sandbox here.
const POLL_SECS: u64 = 1;
/// The longest backoff a relaunch can wait — the longest period a "nothing
/// was relaunched" assertion has to outlast.
const BACKOFF_MAX_SECS: u64 = 2;
/// Parked passes a pin must survive, counted by the daemon, before "it stayed
/// parked" is asserted. 4 passes + the backoff ceiling = 6 poll periods.
const PASSES_AFTER_PIN: u64 = 4;

/// How long `n` daemon passes may take: `n` poll periods, with 15x headroom
/// for a pass's own work under a full-suite load. Derived from the period so
/// the bound moves if the policy does.
fn passes_budget(n: u64) -> Duration {
    Duration::from_secs(n * POLL_SECS * 15)
}

fn policy(rollback: bool, stable: u64) -> String {
    format!(
        "auto_restart = true\nauto_rollback = {rollback}\npoll_secs = {POLL_SECS}\ndown_after = 1\n\
         max_start_failures = 3\nbackoff_base_secs = 1\nbackoff_max_secs = {BACKOFF_MAX_SECS}\n\
         stable_after_secs = {stable}"
    )
}

const BUDGET: Duration = Duration::from_secs(120);

struct Sandbox {
    root: PathBuf,
    env: String,
    daemon: Option<Child>,
    log: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            for p in children_of(d.id()) {
                let _ = Command::new("/bin/kill").args(["-9", &p.to_string()]).status();
            }
            let _ = d.kill();
            let _ = d.wait();
        }
        if let Ok(t) = std::fs::read_to_string(self.root.join("fake-pids")) {
            for p in t.lines() {
                let _ = Command::new("/bin/kill").args(["-9", p.trim()]).status();
            }
        }
        if std::env::var_os("KEEP_SANDBOX").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn children_of(pid: u32) -> Vec<u32> {
    let out = Command::new("/usr/bin/pgrep").args(["-P", &pid.to_string()]).output();
    out.map(|o| {
        String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect()
    })
    .unwrap_or_default()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn listening(port: u16) -> bool {
    let sa: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    TcpStream::connect_timeout(&sa, Duration::from_millis(200)).is_ok()
}

fn alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn wait_for<T>(what: &str, budget: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if t0.elapsed() > budget {
            panic!("timed out after {budget:?} waiting for: {what}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn dirs_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

impl Sandbox {
    fn new(tag: &str, supervisor: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "cauldron-r9c2-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        assert!(!root.starts_with(dirs_home().join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("r9c2{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::create_dir_all(root.join("data").join(&env)).unwrap();
        std::fs::write(root.join("data").join(&env).join("env.toml"), format!("[supervisor]\n{supervisor}\n"))
            .unwrap();
        let log = root.join("daemon.log");
        Sandbox { root, env, daemon: None, log }
    }

    fn cauldron(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_cauldron"))
            .args(args)
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .output()
            .expect("spawn cauldron");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    fn declare(&self, name: &str) -> u16 {
        let port = free_port();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\n\
                 [env]\nFAKE_PORT = \"{port}\"\nFAKE_PIDS = \"{}\"\n\
                 [build]\nartifact = \"{}\"\nbin = \"fake-daemon\"\n",
                self.root.join("fake-pids").display(),
                self.root.join("fake-daemon").display()
            ),
        )
        .unwrap();
        port
    }

    /// Overwrite the artifact with `script`, build, and return the NEW label.
    fn build_script(&self, name: &str, script: &str) -> String {
        let (_, before) = self.cauldron(&["versions", name, "--json"]);
        let fake = self.root.join("fake-daemon");
        std::fs::write(&fake, launch_wait::slow_launch_py(script, &self.root.join("f2g-marks"))).unwrap();
        Command::new("/bin/chmod").args(["+x"]).arg(&fake).status().unwrap();
        let (rc, out) = self.cauldron(&["build", name]);
        assert_eq!(rc, 0, "build {name}: {out}");
        let (rc, out) = self.cauldron(&["versions", name, "--json"]);
        assert_eq!(rc, 0, "versions {name}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        let new: Vec<String> = v
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| b["label"].as_str().map(str::to_string))
            .filter(|l| !before.contains(l.as_str()))
            .collect();
        assert_eq!(new.len(), 1, "expected exactly one new build label: {new:?}\n{out}");
        new[0].clone()
    }

    fn pin(&self, name: &str, label: &str) {
        let (rc, out) = self.cauldron(&["pin", name, label]);
        assert_eq!(rc, 0, "pin {name} {label}: {out}");
    }

    fn start_daemon(&mut self) {
        let log = std::fs::File::create(&self.log).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_cauldron"))
            .arg("daemon")
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("spawn daemon");
        self.daemon = Some(child);
    }

    fn daemon_pid(&self) -> u32 {
        self.daemon.as_ref().expect("daemon started").id()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn seam(&self, name: &str) -> Option<(u32, String)> {
        let t = std::fs::read_to_string(self.root.join("data").join(&self.env).join("seams.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&t).ok()?;
        let s = &v["seams"][name];
        Some((s["pid"].as_u64()? as u32, s["version"].as_str()?.to_string()))
    }

    /// Up ON `label`: the row names a live pid on that version and the port answers.
    fn up_on(&self, name: &str, port: u16, label: &str) -> Option<u32> {
        let (pid, version) = self.seam(name)?;
        (version == label && alive(pid) && listening(port)).then_some(pid)
    }

    fn supervisions(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.root.join("data").join(&self.env).join("supervisions.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn launches(&self, name: &str) -> usize {
        self.supervisions().iter().filter(|r| r["service"] == name && r["act"] == "launch").count()
    }

    fn manifest(&self) -> ::env::decl::Manifest {
        ::env::decl::Manifest::load(&self.root.join("data").join(&self.env).join("env.toml")).unwrap()
    }

    /// How many passes have said the service is parked, as far as the LOG
    /// shows. The daemon dedups a repeated warning to its 1st, 2nd, 4th, 8th …
    /// occurrence (`counters::emits`), so this undercounts passes
    /// logarithmically: good for "at least one pass said so", useless for
    /// "N more passes happened" — see [`Sandbox::parked_count`].
    fn parked_passes(&self, name: &str) -> usize {
        self.log().matches(&format!("`{name}` is PARKED in")).count()
    }

    /// **How many passes have said `name` is parked, as the DAEMON counts
    /// them** — one per pass, never suppressed (`counters::record`: "THE COUNT
    /// IS NEVER SUPPRESSED"). Read over the daemon's own seam with the
    /// `WARNINGS` word: the per-call-site row whose current message is the
    /// park warning for `name`. `None` until the daemon answers with that row.
    ///
    /// R9.F2c: the pair test used to wait for 4 more LOG lines, which the
    /// log's power-of-two dedup turned into ~128 passes for the second pin —
    /// it passed alone by seconds and failed under load. This count moves on
    /// every pass, so "4 more passes" arrives in about 4 poll periods.
    /// Single-service sandboxes only: the row is per call site, so two parked
    /// services would share one count.
    fn parked_count(&self, name: &str) -> Option<u64> {
        let t = std::fs::read_to_string(self.root.join("data").join(&self.env).join("seams.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&t).ok()?;
        let addr: SocketAddr = v["seams"]["cauldron"]["addr"].as_str()?.parse().ok()?;
        let s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        let mut w = s.try_clone().ok()?;
        w.write_all(b"WARNINGS warnings\n").ok()?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).ok()?;
        let e: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        let prefix = format!("`{name}` is PARKED in");
        e["kinds"].as_array()?.iter().find(|k| k["message"].as_str().is_some_and(|m| m.starts_with(&prefix)))?["count"]
            .as_u64()
    }

    fn park(&self, name: &str) {
        wait_for(&format!("{name} PARKED"), BUDGET, || {
            self.log().contains(&format!("PARKED `{name}`")).then_some(())
        });
    }
}

/// THE CLAIM. Parked on a build that binds and dies; a pin to a different,
/// working build runs it — without the daemon restarting.
#[test]
fn r9c2_a_parked_service_pinned_to_a_different_build_runs_it() {
    let mut sb = Sandbox::new("revive", &policy(false, 8));
    let port = sb.declare("svc-a");
    let bad = sb.build_script("svc-a", &dying_fake(1));
    sb.pin("svc-a", &bad);
    sb.start_daemon();
    let holder = sb.daemon_pid();
    sb.park("svc-a");
    // Premise: it is parked and stays down, so what follows is the pin's doing.
    wait_for("a pass that says it is parked", BUDGET, || (sb.parked_passes("svc-a") >= 1).then_some(()));
    assert!(!listening(port), "premise: parked means down:\n{}", sb.log());

    let fixed = sb.build_script("svc-a", &fixed_fake("1"));
    assert_ne!(fixed, bad);
    sb.pin("svc-a", &fixed);
    let pid = wait_for("svc-a running the newly pinned build", BUDGET, || sb.up_on("svc-a", port, &fixed));
    let log = sb.log();
    assert!(log.contains("was PARKED"), "the daemon did not say it revived the park:\n{log}");
    // R9.C7b3 (C7b2 inspect F2): the revive sentence, WHOLE — a prefix match
    // passed with ~22-space runs in its body. 3 = max_start_failures, the
    // count the park is reached at (`policy`).
    let revived = log.lines().find(|l| l.contains("`svc-a` was PARKED")).expect("the revive line");
    assert_eq!(
        revived,
        format!(
            "cauldron[warn] `svc-a` was PARKED in `{env}` on {bad} and `{env}` now pins {fixed}, a different \
             build. A new build is the fix the park asked for, so the park and its 3 failed launch(es) are \
             cleared and {fixed} is launched now.",
            env = sb.env
        ),
        "the revive sentence, whole"
    );
    let last = sb
        .supervisions()
        .into_iter()
        .rev()
        .find(|r| r["service"] == "svc-a" && r["act"] == "launch")
        .expect("a launch record");
    assert_eq!(last["why"], "repin", "the revival must be recorded as a re-pin launch: {last}");
    assert_eq!(last["version"], fixed.as_str(), "{last}");
    assert_eq!(last["pid"].as_u64(), Some(pid as u64), "{last}");
    // The SAME holder did it: no daemon restart was needed.
    assert!(alive(holder), "the daemon died:\n{log}");
    assert_eq!(sb.daemon_pid(), holder);
}

/// THE PAIR. The same build, pinned again by label and then as `latest`
/// (which resolves to it, since it is the only build), stays parked.
#[test]
fn r9c2_a_parked_service_pinned_to_the_same_build_stays_parked_outlasting_6_polls() {
    let mut sb = Sandbox::new("same", &policy(false, 8));
    let port = sb.declare("svc-a");
    let bad = sb.build_script("svc-a", &dying_fake(1));
    sb.pin("svc-a", &bad);
    sb.start_daemon();
    sb.park("svc-a");
    let launches_at_park = sb.launches("svc-a");
    assert!(launches_at_park >= 3, "parked after only {launches_at_park} launches:\n{}", sb.log());

    for pin in [bad.as_str(), "latest"] {
        sb.pin("svc-a", pin);
        // Read AFTER the pin returned, so every pass counted from here on
        // started after the new pin was on disk.
        let passes = wait_for("the daemon's own count of parked passes", BUDGET, || sb.parked_count("svc-a"));
        // The daemon READ the new pin: PASSES_AFTER_PIN more parked passes
        // after it, each of which resolved the pin (daemon.rs, R9.C2 block)
        // and chose to stay parked. Counted by the daemon, not by log lines.
        wait_for(
            &format!("{PASSES_AFTER_PIN} more parked passes after the pin ({pin})"),
            passes_budget(PASSES_AFTER_PIN),
            || (sb.parked_count("svc-a")? >= passes + PASSES_AFTER_PIN).then_some(()),
        );
        // ...and past the longest period in the path, the backoff ceiling.
        std::thread::sleep(Duration::from_secs(BACKOFF_MAX_SECS));
        let log = sb.log();
        assert_eq!(sb.launches("svc-a"), launches_at_park, "relaunched on a pin of the same build ({pin}):\n{log}");
        assert!(!listening(port), "svc-a is up on a pin of the same build ({pin}):\n{log}");
        assert!(!log.contains("was PARKED"), "revived on a pin of the same build ({pin}):\n{log}");
    }
}

/// A revival leaves the persisted record alone. Good build G proven; bad
/// build B pinned, auto-rolled back to G (the record naming B); G then made
/// unlaunchable (its build's binary removed) and killed, so it parks with
/// nothing behind it; a third build C is pinned and runs.
#[test]
fn r9c2_a_revive_after_a_rollback_keeps_the_rollback_record_and_good_history() {
    const STABLE: u64 = 4;
    let mut sb = Sandbox::new("record", &policy(true, STABLE));
    let port = sb.declare("svc-a");
    let good = sb.build_script("svc-a", FAKE);
    sb.pin("svc-a", &good);
    sb.start_daemon();
    wait_for("the good build recorded good", BUDGET, || {
        (sb.manifest().good("svc-a").first() == Some(&good)).then_some(())
    });

    let bad = sb.build_script("svc-a", &dying_fake(1));
    sb.pin("svc-a", &bad);
    let rb = wait_for("an auto-rollback re-pin", BUDGET, || {
        sb.supervisions()
            .into_iter()
            .find(|r| r["service"] == "svc-a" && r["act"] == "repin" && r["why"] == "rollback")
    });
    assert_eq!(rb["was_version"], bad.as_str(), "{rb}");
    let g = wait_for("the good build back after the rollback", BUDGET, || sb.up_on("svc-a", port, &good));

    // G can no longer launch; kill it. Its relaunches fail, and with G the
    // only good build there is nothing to roll back to: it parks.
    let bin = sb.root.join("data").join("shared").join("builds").join("svc-a").join(&good).join("fake-daemon");
    assert!(bin.exists(), "premise: the good build's binary is at {}", bin.display());
    std::fs::remove_file(&bin).unwrap();
    assert!(Command::new("/bin/kill").args(["-9", &g.to_string()]).status().unwrap().success());
    sb.park("svc-a");
    let rollbacks_before: Vec<_> = sb
        .supervisions()
        .into_iter()
        .filter(|r| r["service"] == "svc-a" && r["act"] == "repin")
        .collect();
    let good_before = sb.manifest().good("svc-a");
    assert!(good_before.contains(&good) && !good_before.contains(&bad), "premise: {good_before:?}");

    let third = sb.build_script("svc-a", &fixed_fake("3"));
    sb.pin("svc-a", &third);
    wait_for("svc-a running the third build", BUDGET, || sb.up_on("svc-a", port, &third));
    let log = sb.log();
    assert!(log.contains("was PARKED"), "{log}");

    let rollbacks_after: Vec<_> = sb
        .supervisions()
        .into_iter()
        .filter(|r| r["service"] == "svc-a" && r["act"] == "repin")
        .collect();
    assert_eq!(rollbacks_after, rollbacks_before, "the revival changed the rollback record:\n{log}");
    assert!(
        rollbacks_after.iter().any(|r| r["why"] == "rollback" && r["was_version"] == bad.as_str()),
        "the record of the bad build is gone: {rollbacks_after:?}"
    );
    let good_after = sb.manifest().good("svc-a");
    assert!(good_after.contains(&good), "the revival dropped the good history: {good_after:?}\n{log}");
    assert!(!good_after.contains(&bad), "the bad build entered the good history: {good_after:?}\n{log}");
    assert_eq!(sb.manifest().pin("svc-a").as_deref(), Some(third.as_str()));
}

/// R9.C2-1 (inspect 87a2f85): a STOP-park is the operator's own act and is NOT
/// revived by a pin — paired, in the same daemon and on the same pass
/// cadence, with a FAILURE-park that the same kind of pin (a different build)
/// does revive. So the stop-park staying down is the park's kind, not a
/// daemon that ignores pins. Outlasts 4 parked passes after the pin plus 2 s
/// (`poll_secs = 1`, backoff ceiling 2 s).
#[test]
fn r9c2_a_stop_parked_service_pinned_to_a_different_build_stays_down_while_a_failure_park_revives_outlasting_6_polls() {
    let mut sb = Sandbox::new("stoppark", &policy(false, 8));
    let pa = sb.declare("svc-a");
    let pb = sb.declare("svc-b");
    let a = sb.build_script("svc-a", FAKE);
    sb.pin("svc-a", &a);
    let bad = sb.build_script("svc-b", &dying_fake(1));
    sb.pin("svc-b", &bad);
    sb.start_daemon();
    wait_for("svc-a up on A", BUDGET, || sb.up_on("svc-a", pa, &a));
    sb.park("svc-b");

    // svc-a: parked by a requested STOP.
    let (rc, out) = sb.cauldron(&["stop", "svc-a"]);
    assert_eq!(rc, 0, "stop: {out}\n{}", sb.log());
    wait_for("a parked pass for svc-a after the stop", BUDGET, || (sb.parked_passes("svc-a") >= 1).then_some(()));
    assert!(!listening(pa), "premise: stopped means down:\n{}", sb.log());
    let launches_a = sb.launches("svc-a");

    // The same kind of pin to both: a different, working build.
    let fixed = fixed_fake("stop");
    let a2 = sb.build_script("svc-a", &fixed);
    let b2 = sb.build_script("svc-b", &fixed);
    assert_ne!(a2, a);
    assert_ne!(b2, bad);
    let passes = sb.parked_passes("svc-a");
    sb.pin("svc-a", &a2);
    sb.pin("svc-b", &b2);

    // PAIR: the failure-park revives on that pin.
    wait_for("the failure-parked svc-b running its new build", BUDGET, || sb.up_on("svc-b", pb, &b2));
    assert!(sb.log().contains("`svc-b` was PARKED"), "{}", sb.log());

    // CLAIM: the stop-park does not.
    wait_for("4 more parked passes for svc-a after the pin", BUDGET, || {
        (sb.parked_passes("svc-a") >= passes + 4).then_some(())
    });
    std::thread::sleep(Duration::from_secs(2));
    let log = sb.log();
    assert_eq!(sb.launches("svc-a"), launches_a, "a STOP-park was relaunched by a pin:\n{log}");
    assert!(!listening(pa), "a STOP-parked svc-a is up after a pin to {a2}:\n{log}");
    assert!(!log.contains("`svc-a` was PARKED"), "a STOP-park was revived by a pin:\n{log}");
}
