//! **R9.C7f7 (gap C-7, inspect R9.C7h finding 2) — THE HOLDER'S RELAUNCH
//! DECISION ITSELF READS `503 migrating` AS ALIVE AND `503 recovery_stuck` AS
//! DOWN.**
//!
//! C7h's guard (d) (`libs/env/tests/hosting.rs`) pins the probe's VERDICT on
//! a real socket: `503 {"status":"migrating"}` → `live:unverified`,
//! `503 {"status":"recovery_stuck"}` → `unhealthy`. The relaunch is one hop
//! further, in `tend` (`cli/src/daemon.rs`): a live/`live:unverified` row is
//! left alone, an `unhealthy` one is stopped and launched again. Nothing
//! asserted that hop, so an edit that relaunched `live:unverified` rows — or
//! read anything but `seams::probe` there — stayed green. This drives a REAL
//! holder over a REAL hosted fake whose `/health` is switched between the two
//! answers, and asserts on the relaunch.
//!
//! THE LONGEST PERIOD on the path to a relaunch is the pass (`poll_secs` = 1)
//! times `down_after` (1) plus the backoff (`backoff_max_secs` = 2): the
//! "not relaunched" arm is held for 8 s AND until the fake has answered
//! `migrating` at least 6 times (so the holder really read it, every pass).

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

#[path = "support/first_exec.rs"]
mod first_exec;

const FAKE: &str = r#"#!/usr/bin/env python3
# A hosted fake with an HTTP /health. While `mode` names THIS pid, it answers
# 503 with that status (`migrating` or `recovery_stuck`); otherwise 200 ok.
# Every answer is noted in `answers` as "<pid> <status>".
import os, socket, sys, json
if sys.argv[1:] == ["--build-stamp"]:
    print("commit=5eed5eed5eed5eed5eed5eed5eed5eed5eed5eed provenance=pin dirty=0 epoch=1")
    sys.exit(0)
root = os.environ["FAKE_ROOT"]
with open(os.path.join(root, "fake-pids"), "a") as f: f.write("%d\n" % os.getpid())
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(os.environ["FAKE_PORT"]))); s.listen(16)
while True:
    c, _ = s.accept()
    try:
        c.settimeout(5)
        data = b""
        while b"\r\n\r\n" not in data and len(data) < 8192:
            more = c.recv(1024)
            if not more: break
            data += more
        if data:
            try: pid, status = open(os.path.join(root, "mode")).read().split()
            except Exception: pid, status = "", "ok"
            if pid != str(os.getpid()): status = "ok"
            code = b"200 OK" if status == "ok" else b"503 Service Unavailable"
            with open(os.path.join(root, "answers"), "a") as f: f.write("%d %s\n" % (os.getpid(), status))
            c.sendall(b"HTTP/1.0 " + code + b"\r\nContent-Type: application/json\r\n\r\n" + json.dumps({"status": status}).encode())
    except Exception:
        pass
    c.close()
"#;

const POLICY: &str = "auto_restart = true\nauto_rollback = false\npoll_secs = 1\ndown_after = 1\n\
                      max_start_failures = 3\nbackoff_base_secs = 1\nbackoff_max_secs = 2\n\
                      stable_after_secs = 600";
const HOLD: Duration = Duration::from_secs(8);
const BUDGET: Duration = Duration::from_secs(3 * launch_wait::START_TIMEOUT_SECS + 30 + 30);
const FIRST_UP_DECLARED: Duration = Duration::from_secs(1 + launch_wait::START_TIMEOUT_SECS + 30);

struct Sandbox {
    root: PathBuf,
    env: String,
    daemon: Option<Child>,
    log: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
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

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "cauldron-c7f7-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("c7f7{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::create_dir_all(root.join("data").join(&env)).unwrap();
        std::fs::write(root.join("data").join(&env).join("env.toml"), format!("[supervisor]\n{POLICY}\n")).unwrap();
        std::fs::write(
            root.join("fake"),
            launch_wait::slow_launch_py(
                &first_exec::slow_first_exec_py(FAKE, &root.join("first-exec-marks")),
                &root.join("f2g-marks"),
            ),
        )
        .unwrap();
        Command::new("/bin/chmod").arg("+x").arg(root.join("fake")).status().unwrap();
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
        (
            out.status.code().unwrap_or(-1),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    }

    /// Hosted, `probe = "http"` against the fake's `/health`, as threads is.
    fn register(&self, name: &str) -> u16 {
        let port = free_port();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"http\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\nhealth = \"http://127.0.0.1:{port}/health\"\n\
                 [env]\nFAKE_PORT = \"{port}\"\nFAKE_ROOT = \"{}\"\n\
                 [build]\nartifact = \"{}\"\nbin = \"fake\"\n",
                self.root.display(),
                self.root.join("fake").display()
            ),
        )
        .unwrap();
        let (rc, out) = self.cauldron(&["build", name]);
        assert_eq!(rc, 0, "build {name}: {out}");
        let (rc, out) = self.cauldron(&["versions", name, "--json"]);
        assert_eq!(rc, 0, "versions {name}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        let label = v[0]["label"].as_str().expect("a label").to_string();
        let dir = self.root.join("data").join("shared").join("builds").join(name).join(&label);
        let exes = first_exec::executables_under(&dir);
        assert!(!exes.is_empty(), "sentinel: `build` left no executable in {}", dir.display());
        for x in exes {
            first_exec::pay_first_exec(Command::new(&x).arg("--build-stamp"), &format!("{name}'s build"));
        }
        let (rc, out) = self.cauldron(&["pin", name, &label]);
        assert_eq!(rc, 0, "pin {name}: {out}");
        port
    }

    fn start_daemon(&mut self) {
        let own = self.root.join("bin").join(format!("cauldron-{}", self.env));
        if !own.is_file() {
            let (rc, out) = self.cauldron(&["promote"]);
            assert_eq!(rc, 0, "promote: {out}");
            first_exec::pay_first_exec(
                Command::new(&own).arg("envs").env("CAULDRON_HOME", &self.root).env("CAULDRON_ENV", &self.env),
                "the holder's launch path",
            );
        }
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

    fn set_mode(&self, pid: u32, status: &str) {
        std::fs::write(self.root.join("mode"), format!("{pid} {status}\n")).unwrap();
    }

    /// How many `/health` answers `pid` has given with `status`.
    fn answered(&self, pid: u32, status: &str) -> usize {
        let want = format!("{pid} {status}");
        std::fs::read_to_string(self.root.join("answers")).unwrap_or_default().lines().filter(|l| *l == want).count()
    }
}

/// THE CLAIM AND ITS PAIR, on one holder: a service answering `503 migrating`
/// is NOT relaunched (same pid, no new launch) across 8 s and at least 6
/// migrating answers; the same service answering `503 recovery_stuck` IS
/// relaunched (a new pid, one more launch). Forced red by making `tend`'s
/// `live` exclude `live:unverified` (the migrating arm relaunches) and,
/// opposed, by making it include `unhealthy` (the stuck arm never does).
#[test]
fn the_holder_does_not_relaunch_a_503_migrating_service_outlasting_8s_and_does_relaunch_a_503_recovery_stuck_one() {
    let mut sb = Sandbox::new("mig");
    let port = sb.register("svc");
    sb.start_daemon();
    let p1 = wait_for("svc up", first_exec::first_wait(FIRST_UP_DECLARED, 2), || sb.up("svc", port));
    let launches_up = sb.launches("svc");

    // ── THE CLAIM: migrating is alive, and the relaunch line leaves it alone.
    sb.set_mode(p1, "migrating");
    let t0 = Instant::now();
    // Ends early on a relaunch (a new launch record, or p1 gone), so a
    // regression reds at THE CLAIM below and not as a timeout here.
    wait_for("the holder to read `migrating` 6 times, or a relaunch", BUDGET, || {
        let relaunched = sb.launches("svc") != launches_up || !alive(p1);
        (relaunched || (sb.answered(p1, "migrating") >= 6 && t0.elapsed() >= HOLD)).then_some(())
    });
    let log = sb.log();
    assert_eq!(sb.launches("svc"), launches_up, "a service answering 503 migrating was relaunched:\n{log}");
    assert_eq!(sb.pid_of("svc"), Some(p1), "the row moved off the migrating pid:\n{log}");
    assert!(alive(p1), "the migrating service was stopped:\n{log}");

    // ── THE PAIR: recovery_stuck is down, and the relaunch line relaunches it.
    sb.set_mode(p1, "recovery_stuck");
    let p2 = wait_for("svc relaunched off recovery_stuck", BUDGET, || sb.up("svc", port).filter(|p| *p != p1));
    let log = sb.log();
    assert!(sb.answered(p1, "recovery_stuck") >= 1, "sentinel: the holder never read recovery_stuck:\n{log}");
    assert_ne!(p2, p1);
    assert_eq!(sb.launches("svc"), launches_up + 1, "recovery_stuck was not relaunched exactly once:\n{log}");
    wait_for("the stuck pid stopped", Duration::from_secs(30), || (!alive(p1)).then_some(()));
}
