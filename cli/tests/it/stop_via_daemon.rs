//! **R9.C5 (gap C-5): `cauldron stop` in an environment a daemon HOLDS stops
//! the service THROUGH the holder, which parks it — so it stays down.**
//!
//! Before C-5, `stop` never asked whether a daemon held the environment. It
//! signalled the service itself, printed "stopped", and one supervision pass
//! later the holder found the service down and relaunched it.
//!
//! The period the negative claim must outlast is the HOLDER'S RELAUNCH PERIOD:
//! `poll_secs × down_after` plus the first relaunch's backoff (the first
//! relaunch is immediate). Here `poll_secs = 1`, `down_after = 1`, so the
//! holder relaunches a down service within ~1-2 s (measured at the base: see
//! the R9.C5 report). The test waits 6 s — past three relaunch periods and past
//! `backoff_base_secs` (3 s) — and asserts the port is still empty and no new
//! `launch` was recorded.
//!
//! Claims:
//! 1. `held_stop_parks_the_service_and_it_stays_down_past_3_relaunch_periods_of_2s`
//!    — red at the base (the holder relaunched it).
//! 2. `unheld_stop_still_stops_the_service_itself` — the paired case: no
//!    daemon, `stop` signals the service itself as before, exit 0.
//! 3. `held_stop_with_an_unreachable_holder_stops_nothing_and_says_so` — the
//!    holder is SIGSTOPped (it still holds the kernel lock, its seam does not
//!    answer): `stop` exits non-zero, says nothing was stopped, and the service
//!    is still up. Red at the base, which signalled it behind the holder's back
//!    and printed success.
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

const FAKE: &str = r#"#!/usr/bin/env python3
import os, socket
port = int(os.environ["FAKE_PORT"])
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen(8)
with open(os.environ["FAKE_PIDS"], "a") as f:
    f.write(f"{os.getpid()}\n")
while True:
    c, _ = s.accept()
    c.close()
"#;

const POLICY: &str = "auto_restart = true\nauto_rollback = false\npoll_secs = 1\ndown_after = 1\n\
                      max_start_failures = 3\nbackoff_base_secs = 3\nbackoff_max_secs = 30\n\
                      stable_after_secs = 600";

#[path = "support/first_exec.rs"]
mod first_exec;

const BUDGET: Duration = Duration::from_secs(40);

/// **The declared period on the path to a service's FIRST "up" under a fresh
/// holder (R9.F1)**: the holder's first pass (`poll_secs` 1 x `down_after` 1
/// = 1 s), one launch bounded by `HOSTED_BIND_WAIT` (45 s,
/// `cli/src/daemon.rs`) for these hosted fakes, then, if it failed, one
/// backoff of at most `backoff_max_secs` (30 s) before a launch that
/// succeeds. 1 + 45 + 30 = 76 s.
const FIRST_UP_DECLARED: Duration = Duration::from_secs(1 + 45 + 30);

/// A FIRST wait: [`FIRST_UP_DECLARED`] plus the measured worst-case first
/// exec (`support/first_exec.rs`) of the two fresh files on its path — the
/// holder's own launch path and the service's build copy — both paid before
/// the clock starts (`start_daemon`, `register`), still budgeted. It was the
/// fixed 40 s BUDGET, red in R9.F1's final-tip run (load 60) and under a
/// forced 21 s first exec, at "svc-a up under the daemon".
fn first_up() -> Duration {
    first_exec::first_wait(FIRST_UP_DECLARED, 2)
}

/// Three holder relaunch periods (~2 s each at poll 1 × down_after 1) and past
/// the 3 s backoff base.
const OUTLAST: Duration = Duration::from_secs(6);

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
            "cauldron-r9c5-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        assert!(!root.starts_with(home().join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("r9c5{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::create_dir_all(root.join("data").join(&env)).unwrap();
        std::fs::write(
            root.join("data").join(&env).join("env.toml"),
            format!("[supervisor]\n{POLICY}\n"),
        )
        .unwrap();
        let fake = root.join("fake-daemon");
        std::fs::write(&fake, launch_wait::slow_launch_py(&first_exec::slow_first_exec_py(FAKE, &root.join("first-exec-marks")), &root.join("f2g-marks"))).unwrap();
        Command::new("/bin/chmod").args(["+x"]).arg(&fake).status().unwrap();
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

    /// Declare, build and pin one hosted fake on its own port.
    fn register(&self, name: &str) -> u16 {
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
        let (rc, out) = self.cauldron(&["build", name]);
        assert_eq!(rc, 0, "build {name}: {out}");
        let (rc, out) = self.cauldron(&["versions", name, "--json"]);
        assert_eq!(rc, 0, "versions {name}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        let label = v[0]["label"].as_str().expect("a label").to_string();
        // R9.F1: the build copy is a NEW file; pay its first exec here, outside
        // every deadline. No `FAKE_*` in this environment, so the fake raises at
        // its first `os.environ[...]` and binds nothing.
        let dir = self.root.join("data").join("shared").join("builds").join(name).join(&label);
        let exes = first_exec::executables_under(&dir);
        assert!(!exes.is_empty(), "sentinel: `build` left no executable in {}", dir.display());
        for x in exes {
            let mut once = Command::new(&x);
            once.env_remove("FAKE_PORT").env_remove("FAKE_PIDS");
            first_exec::pay_first_exec(&mut once, &format!("{name}'s build {label}"));
        }
        let (rc, out) = self.cauldron(&["pin", name, &label]);
        assert_eq!(rc, 0, "pin {name}: {out}");
        port
    }

    /// R9.F1: place this environment's own launch path the documented way
    /// (`promote`, the same byte copy `daemon` makes at start) and pay its
    /// first exec here, so the holder's exec onto it is not its first. Once
    /// per sandbox: a second `promote` would write a NEW file.
    fn place_launch_path(&self) {
        let own = self.root.join("bin").join(format!("cauldron-{}", self.env));
        if own.is_file() {
            return;
        }
        let (rc, out) = self.cauldron(&["promote"]);
        assert_eq!(rc, 0, "promote: {out}");
        assert!(own.is_file(), "`promote` did not place {}: {out}", own.display());
        first_exec::pay_first_exec(
            Command::new(&own).arg("envs").env("CAULDRON_HOME", &self.root).env("CAULDRON_ENV", &self.env),
            "the holder's launch path",
        );
    }

    fn start_daemon(&mut self) {
        self.place_launch_path();
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

    fn launches(&self, name: &str) -> usize {
        std::fs::read_to_string(self.root.join("data").join(&self.env).join("supervisions.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|r| r["service"] == name && r["act"] == "launch")
            .count()
    }

    /// Both fakes up under the daemon, and its first pass has seen them.
    fn held_with_two(&mut self) -> (u16, u16, u32, u32) {
        let pa = self.register("svc-a");
        let pb = self.register("svc-b");
        self.start_daemon();
        let a = wait_for("svc-a up under the daemon", first_up(), || self.up("svc-a", pa));
        let b = wait_for("svc-b up under the daemon", first_up(), || self.up("svc-b", pb));
        // Premise: the holder's own seam row is written (it is what `stop`
        // probes to decide the environment is HELD and reachable).
        wait_for("the holder's own seam row", BUDGET, || self.pid_of("cauldron").filter(|p| alive(*p)));
        (pa, pb, a, b)
    }
}

#[test]
fn held_stop_parks_the_service_and_it_stays_down_past_3_relaunch_periods_of_2s() {
    let mut sb = Sandbox::new("held");
    let (pa, pb, _a, b) = sb.held_with_two();
    let launches_before = sb.launches("svc-a");

    let (rc, out) = sb.cauldron(&["stop", "svc-a"]);
    println!("{out}");
    assert_eq!(rc, 0, "stop through the holder must confirm: {out}\n{}", sb.log());
    assert!(out.contains("PARKED"), "stop must say the holder parked it:\n{out}");
    assert!(!listening(pa), "svc-a still listening right after stop:\n{out}");

    // THE CLAIM: outlast the holder's relaunch period, three times over.
    std::thread::sleep(OUTLAST);
    assert!(
        !listening(pa),
        "svc-a was RELAUNCHED within {OUTLAST:?} of `stop` — the holder undid the stop:\n{out}\n{}",
        sb.log()
    );
    assert_eq!(sb.launches("svc-a"), launches_before, "a new launch of svc-a was recorded:\n{}", sb.log());
    // The sibling is untouched.
    assert_eq!(sb.up("svc-b", pb), Some(b), "svc-b was disturbed:\n{}", sb.log());

    // And it comes back on request: the park is not a dead end.
    let (rc, out) = sb.cauldron(&["restart", "svc-a"]);
    assert_eq!(rc, 0, "{out}");
    wait_for("svc-a back after restart", BUDGET, || sb.up("svc-a", pa));
}

#[test]
fn unheld_stop_still_stops_the_service_itself() {
    let sb = Sandbox::new("unheld");
    let pa = sb.register("svc-a");
    let (rc, out) = sb.cauldron(&["start", "svc-a"]);
    assert_eq!(rc, 0, "{out}");
    let a = wait_for("svc-a up", first_up(), || sb.up("svc-a", pa));

    let (rc, out) = sb.cauldron(&["stop", "svc-a"]);
    println!("{out}");
    assert_eq!(rc, 0, "unheld stop: {out}");
    assert!(!out.contains("HELD"), "no holder here, so nothing may claim one:\n{out}");
    wait_for("svc-a's pid gone", Duration::from_secs(10), || (!alive(a)).then_some(()));
    assert!(!listening(pa), "svc-a still listening after an unheld stop:\n{out}");
    // The stop was recorded by the one stop path, as before.
    let recs = std::fs::read_to_string(sb.root.join("data").join(&sb.env).join("supervisions.jsonl"))
        .unwrap_or_default();
    assert!(
        recs.lines().any(|l| l.contains("\"svc-a\"") && l.contains("\"stop\"")),
        "no stop record:\n{recs}"
    );
}

#[test]
fn held_stop_with_an_unreachable_holder_stops_nothing_and_says_so() {
    let mut sb = Sandbox::new("wedged");
    let (pa, _pb, a, _b) = sb.held_with_two();
    let holder = sb.daemon.as_ref().unwrap().id();
    // Wedge the holder: it keeps the kernel lock (the process exists) but its
    // seam no longer answers a handshake.
    assert!(Command::new("/bin/kill").args(["-STOP", &holder.to_string()]).status().unwrap().success());

    let (rc, out) = sb.cauldron(&["stop", "svc-a"]);
    println!("{out}");
    // Resume before asserting so a failure does not leave a stopped process.
    let _ = Command::new("/bin/kill").args(["-CONT", &holder.to_string()]).status();
    assert_ne!(rc, 0, "a stop that stopped nothing must not exit 0:\n{out}");
    assert!(out.contains("NOTHING WAS STOPPED"), "stop must say nothing was stopped:\n{out}");
    assert!(!out.contains("stopped `svc-a`"), "a false `stopped`:\n{out}");
    assert_eq!(sb.up("svc-a", pa), Some(a), "svc-a was signalled behind the holder's back:\n{out}");
}
