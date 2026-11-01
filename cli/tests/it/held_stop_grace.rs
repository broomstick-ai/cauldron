//! **R9.C5c (gap C-5, follow-up): a held `cauldron stop` waits for the
//! service's OWN declared `stop_grace_secs`, plus a stated margin — not a fixed
//! constant.**
//!
//! Before C-5c the wait was `poll_secs × 2 + STOP_GRACE_SECS (15) + 10`: at
//! `poll_secs = 1` that is 27 s, whatever the service declares. A service whose
//! clean stop takes longer (`threads` declares 75 s) was reported "not
//! confirmed" (exit 6) while the holder was still, correctly, waiting on it.
//! After: `poll_secs × 2 + stop_grace + HELD_STOP_MARGIN_SECS (10)`.
//!
//! The periods are INJECTED, per declaration and per manifest: `poll_secs = 1`
//! and each fake's own `stop_grace_secs`. The graces are chosen ABOVE the
//! base's fixed 27 s, because a grace below it cannot tell the two
//! implementations apart (a 10 s grace / 8 s stop is green at the base too).
//!
//! Claims:
//! 1. `held_stop_of_a_service_that_drains_33s_under_a_45s_grace_is_reported_stopped`
//!    — red at the base (exit 6 at 27 s, mid-drain).
//! 2. `held_stop_of_a_service_that_never_stops_under_a_35s_grace_fails_after_the_grace_not_before`
//!    — `escalate = false`, TERM ignored: the holder gives up at its grace and
//!    parks it; `stop` says it is STILL RUNNING, exit 1, no earlier than 35 s.
//!    Red at the base (exit 6 at 27 s, before the holder had decided).
//! 3. `held_stop_whose_holder_never_confirms_gives_up_at_grace_35s_plus_2_polls_plus_10s_margin`
//!    — the holder is SIGSTOPped mid-stop, so nothing ever confirms: `stop`
//!    gives up (exit 6) at its own deadline, 47 s — not before (base: 27 s) and
//!    not unboundedly after.
//!
//! Throwaway everything: own `CAULDRON_HOME` under the temp dir, own env name,
//! kernel-assigned ports, a python fake as the service. Nothing touches
//! `~/.cauldron`, launchd, or a live process.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

/// `FAKE_MODE`: `drain:<secs>` closes its port on TERM, sleeps, exits 0 (a
/// draining service); `ignore` records the TERM in `FAKE_TERMS` and keeps
/// serving (a service that never stops).
const FAKE: &str = r#"#!/usr/bin/env python3
import os, signal, socket, time
port = int(os.environ["FAKE_PORT"])
mode = os.environ.get("FAKE_MODE", "")
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen(8)
with open(os.environ["FAKE_PIDS"], "a") as f:
    f.write(f"{os.getpid()}\n")
def on_term(_sig, _frm):
    with open(os.environ["FAKE_TERMS"], "a") as f:
        f.write(f"{time.time()}\n")
    if mode.startswith("drain:"):
        s.close()
        time.sleep(int(mode.split(":")[1]))
        os._exit(0)
signal.signal(signal.SIGTERM, on_term)
while True:
    try:
        c, _ = s.accept()
        c.close()
    except OSError:
        time.sleep(0.1)
"#;

const POLICY: &str = "auto_restart = true\nauto_rollback = false\npoll_secs = 1\ndown_after = 1\n\
                      max_start_failures = 3\nbackoff_base_secs = 3\nbackoff_max_secs = 30\n\
                      stable_after_secs = 600";

const POLL_SECS: u64 = 1;
/// `HELD_STOP_MARGIN_SECS` in `cli/src/main.rs` — the margin this step states.
const MARGIN_SECS: u64 = 10;
/// The base's fixed wait at `poll_secs = 1`: 1 × 2 + 15 + 10.
const BASE_BUDGET_SECS: u64 = 27;

const BUDGET: Duration = Duration::from_secs(40);

struct Sandbox {
    root: PathBuf,
    env: String,
    daemon: Option<Child>,
    log: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = Command::new("/bin/kill").args(["-CONT", &d.id().to_string()]).status();
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

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "cauldron-r9c5c-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        assert!(!root.starts_with(home().join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("r9c5c{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::create_dir_all(root.join("data").join(&env)).unwrap();
        std::fs::write(
            root.join("data").join(&env).join("env.toml"),
            format!("[supervisor]\n{POLICY}\n"),
        )
        .unwrap();
        let fake = root.join("fake-daemon");
        std::fs::write(&fake, launch_wait::slow_launch_py(FAKE, &root.join("f2g-marks"))).unwrap();
        Command::new("/bin/chmod").args(["+x"]).arg(&fake).status().unwrap();
        let log = root.join("daemon.log");
        Sandbox { root, env, daemon: None, log }
    }

    fn cauldron(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_cauldron"))
            .args(args)
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .env(crate::grants::ALLOW_ENV, crate::grants::owned(&self.root, &self.daemon.iter().map(Child::id).collect::<Vec<_>>()))
            .output()
            .expect("spawn cauldron");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// Declare, build and pin one hosted fake on its own port, with its own
    /// clean-stop terms.
    fn register(&self, name: &str, mode: &str, grace: u64, escalate: bool) -> u16 {
        let port = free_port();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 stop_grace_secs = {grace}\nescalate = {escalate}\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\n\
                 [env]\nFAKE_PORT = \"{port}\"\nFAKE_PIDS = \"{}\"\nFAKE_TERMS = \"{}\"\n\
                 FAKE_MODE = \"{mode}\"\n\
                 [build]\nartifact = \"{}\"\nbin = \"fake-daemon\"\n",
                self.root.join("fake-pids").display(),
                self.terms().display(),
                self.root.join("fake-daemon").display()
            ),
        )
        .unwrap();
        let (rc, out) = self.cauldron(&["build", name]);
        assert_eq!(rc, 0, "build {name}: {out}");
        let (rc, out) = self.cauldron(&["versions", name, "--json"]);
        assert_eq!(rc, 0, "versions {name}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        let label = v[0]["label"].as_str().expect("a label").to_string();
        let (rc, out) = self.cauldron(&["pin", name, &label]);
        assert_eq!(rc, 0, "pin {name}: {out}");
        port
    }

    fn terms(&self) -> PathBuf {
        self.root.join("fake-terms")
    }

    fn start_daemon(&mut self) {
        let log = std::fs::File::create(&self.log).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_cauldron"))
            .arg("daemon")
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .env(crate::grants::ALLOW_ENV, crate::grants::owned(&self.root, &[]))
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("spawn daemon");
        self.daemon = Some(child);
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn pid_of(&self, name: &str) -> Option<u32> {
        let t = std::fs::read_to_string(self.root.join("data").join(&self.env).join("seams.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&t).ok()?;
        v["seams"][name]["pid"].as_u64().map(|p| p as u32)
    }

    fn up(&self, name: &str, port: u16) -> Option<u32> {
        let pid = self.pid_of(name)?;
        (alive(pid) && listening(port)).then_some(pid)
    }

    /// One fake up under the daemon, and the holder's own row written.
    fn held_with(&mut self, mode: &str, grace: u64, escalate: bool) -> (u16, u32) {
        let port = self.register("svc-a", mode, grace, escalate);
        self.start_daemon();
        let pid = wait_for("svc-a up under the daemon", BUDGET, || self.up("svc-a", port));
        wait_for("the holder's own seam row", BUDGET, || self.pid_of("cauldron").filter(|p| alive(*p)));
        (port, pid)
    }
}

#[test]
fn held_stop_of_a_service_that_drains_33s_under_a_45s_grace_is_reported_stopped() {
    const GRACE: u64 = 45;
    const DRAIN: u64 = 33;
    // Premise: the drain outlasts the base's fixed wait, and fits the grace.
    assert!(DRAIN > BASE_BUDGET_SECS + 3 && DRAIN + 5 < GRACE);
    let mut sb = Sandbox::new("drain");
    let (port, pid) = sb.held_with(&format!("drain:{DRAIN}"), GRACE, true);

    let t0 = Instant::now();
    let (rc, out) = sb.cauldron(&["stop", "svc-a"]);
    let took = t0.elapsed();
    println!("{out}\n(took {took:?})");
    assert_eq!(rc, 0, "a {DRAIN}s drain under a {GRACE}s grace must be confirmed stopped:\n{out}\n{}", sb.log());
    assert!(out.contains("PARKED"), "stop must say the holder parked it:\n{out}");
    assert!(!out.contains("not confirmed") && !out.contains("NOT confirmed"), "{out}");
    // It drained: the TERM was received and the process outlived the old 27 s.
    assert!(std::fs::read_to_string(sb.terms()).unwrap_or_default().lines().count() >= 1, "no TERM seen");
    assert!(took >= Duration::from_secs(DRAIN), "confirmed before the drain ended ({took:?})");
    assert!(!alive(pid), "pid {pid} still alive after a confirmed stop");
    assert!(!listening(port));
    // It drained cleanly — not SIGKILLed at a grace mis-sized somewhere else.
    assert!(!sb.log().contains("sending SIGKILL"), "the holder escalated:\n{}", sb.log());
}

#[test]
fn held_stop_of_a_service_that_never_stops_under_a_35s_grace_fails_after_the_grace_not_before() {
    const GRACE: u64 = 35;
    assert!(GRACE > BASE_BUDGET_SECS + 3);
    let mut sb = Sandbox::new("never");
    let (port, pid) = sb.held_with("ignore", GRACE, false);

    let t0 = Instant::now();
    let (rc, out) = sb.cauldron(&["stop", "svc-a"]);
    let took = t0.elapsed();
    println!("{out}\n(took {took:?})");
    assert_eq!(rc, 1, "a service that never stopped must be reported FAILED (exit 1):\n{out}\n{}", sb.log());
    assert!(out.contains("STILL RUNNING"), "stop must say it is still running:\n{out}");
    assert!(
        took >= Duration::from_secs(GRACE),
        "reported after {took:?}, BEFORE the service's own {GRACE}s grace ran out:\n{out}"
    );
    assert!(
        took <= Duration::from_secs(GRACE + POLL_SECS * 2 + MARGIN_SECS),
        "reported after {took:?}, past grace + 2 polls + margin:\n{out}"
    );
    // The paired fact: it really is still running, so "failed" is the truth.
    assert_eq!(sb.up("svc-a", port), Some(pid), "svc-a should still be up (escalate = false)");
}

#[test]
fn held_stop_whose_holder_never_confirms_gives_up_at_grace_35s_plus_2_polls_plus_10s_margin() {
    const GRACE: u64 = 35;
    let deadline = GRACE + POLL_SECS * 2 + MARGIN_SECS; // 47
    let mut sb = Sandbox::new("silent");
    let (_port, _pid) = sb.held_with("ignore", GRACE, false);
    let holder = sb.daemon.as_ref().unwrap().id();

    let out_path = sb.root.join("stop.out");
    let out_file = std::fs::File::create(&out_path).unwrap();
    let t0 = Instant::now();
    let mut stop = Command::new(env!("CARGO_BIN_EXE_cauldron"))
        .args(["stop", "svc-a"])
        .env("CAULDRON_HOME", &sb.root)
        .env("CAULDRON_ENV", &sb.env)
        .env(crate::grants::ALLOW_ENV, crate::grants::owned(&sb.root, &[holder]))
        .stdout(out_file.try_clone().unwrap())
        .stderr(out_file)
        .spawn()
        .expect("spawn stop");
    // The holder has taken the ask and signalled the fake (which records the
    // TERM and ignores it): it is inside its grace wait. Freeze it there, so
    // nothing ever reports the service parked.
    wait_for("the holder's TERM to reach svc-a", BUDGET, || {
        (std::fs::read_to_string(sb.terms()).unwrap_or_default().lines().count() >= 1).then_some(())
    });
    assert!(Command::new("/bin/kill").args(["-STOP", &holder.to_string()]).status().unwrap().success());
    let status = loop {
        if let Some(s) = stop.try_wait().unwrap() {
            break s;
        }
        if t0.elapsed() > Duration::from_secs(deadline * 3) {
            let _ = stop.kill();
            let _ = stop.wait();
            let _ = Command::new("/bin/kill").args(["-CONT", &holder.to_string()]).status();
            panic!("`stop` was still waiting after {:?}", t0.elapsed());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let took = t0.elapsed();
    let _ = Command::new("/bin/kill").args(["-CONT", &holder.to_string()]).status();
    let out = std::fs::read_to_string(&out_path).unwrap_or_default();
    println!("{out}\n(took {took:?})");
    assert_eq!(status.code(), Some(6), "accepted but never confirmed is exit 6:\n{out}");
    assert!(
        took >= Duration::from_secs(deadline),
        "gave up after {took:?}, before grace {GRACE}s + 2 polls + {MARGIN_SECS}s margin = {deadline}s:\n{out}"
    );
    // One STATUS ask to a frozen holder can take its 2 s connect + 5 s read.
    assert!(
        took <= Duration::from_secs(deadline + 10),
        "gave up after {took:?}, long past its {deadline}s deadline:\n{out}"
    );
    assert!(out.contains(&format!("within {deadline}s")), "the stated wait must be {deadline}s:\n{out}");
}
