//! **R5.S3 — CAULDRON HOSTS EVERY SERVICE: the ui-server form and the
//! soup-backup form, under a real daemon, with real kills.**
//!
//! Every test starts a THROWAWAY cauldron (its own `CAULDRON_HOME` under the
//! system temp dir, its own environment, kernel-assigned ports). Nothing reads
//! or writes `~/.cauldron`, nothing signals a live daemon, nothing touches
//! launchd. The services are fakes (one python script) installed the way the
//! real ones are: the daemon fake as a BUNDLE artifact (`Fake.app`, like
//! `AUI.app`) with `probe = "http"`, a `[launch] cwd` and a separate health
//! port (ui-server's 9009 mutex + 9010 `/health`); the job fake as a
//! scheduled `kind = "job"` (soup-backup's `run` / `status`).
//!
//! Claims, each paired with its opposite:
//!
//! 1. an http-probed bundle service comes up, runs in its declared cwd, is
//!    `live:unverified` ONLY because its health answered 200, and is
//!    relaunched by the daemon after a SIGKILL (`launch`, `why = down`);
//! 2. the same kill under `restart = "never"` is reported and NOT relaunched,
//!    outlasting the backoff (3 s) and 8 poll periods;
//! 3. a health check that does not answer 200 is `unhealthy`, never up — and
//!    the daemon stops that instance (`stop`, `why = down`);
//! 4. a job runs at load, a SIGKILLed pass is recorded (`ran`, signal 9) and
//!    re-run on `retry` — and under `restart = "never"` it is not;
//! 5. a job whose `requires` is missing is `job:blocked` and never runs until
//!    the file exists — then within ~3 polls; a failing `health_args` reads
//!    `job:unhealthy`;
//! 6. a job whose PINNED build still carries `[migration]` is refused, never
//!    run (R5.S3b), beside a clean job that runs — and `status` shows it as
//!    `job:refused` with its refusal, never running the refused build's
//!    health check, even a failing one (R5.S3c), beside the clean job whose
//!    health check `status` does run;
//! 7. that refusal is decided from the PINNED build (R5.S3d): a clean label
//!    re-pinned over a refused one reads `job:never-run` at once, and another
//!    label that still carries `[migration]` reads `job:refused` at once;
//! 8. the holder itself persists a calendar job's `last_slot` in `jobs.json`
//!    (R5.S3d, R5.S4b inspector F1), as the slot the test computes.
//!
//! Every "nothing happened" is asserted only after a same-sandbox POSITIVE
//! control has shown the daemon doing that very thing (R5.S3b, doctrine 4).

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
# R5.S3 fake: a ui-server-shaped daemon, or a soup-backup-shaped job pass.
import os, socket, sys, time, threading
def note(path, line):
    with open(path, "a") as f: f.write(line + "\n")
# R5.S9: an OWNER WATCH. The fixture exits within ~1 s of the test process
# that owns it being gone, so a test binary that dies without unwinding
# (SIGKILL, abort, a harness timeout) cannot orphan it with ppid 1.
def owner_watch(owner):
    while True:
        time.sleep(1)
        try:
            os.kill(owner, 0)
        except ProcessLookupError:
            os._exit(0)
        except PermissionError:
            pass
if os.environ.get("FAKE_OWNER"):
    threading.Thread(target=owner_watch, args=(int(os.environ["FAKE_OWNER"]),), daemon=True).start()
args = sys.argv[1:]
if args[:1] == ["run"]:            # one job pass
    note(os.environ["FAKE_PIDS"], str(os.getpid()))
    note(os.environ["FAKE_RUNS"], "start %d" % os.getpid())
    time.sleep(float(os.environ.get("FAKE_SLEEP", "0")))
    sys.exit(int(os.environ.get("FAKE_RC", "0")))
if args[:1] == ["health"]:         # a job's health check
    note(os.environ["FAKE_RUNS"], ("health " + os.environ.get("FAKE_WHO", "")).strip())
    sys.exit(4 if os.path.exists(os.environ["FAKE_SICK"]) else 0)
# the daemon: a MUTEX port that accepts and closes, and an HTTP health port
note(os.environ["FAKE_PIDS"], str(os.getpid()))
note(os.environ["FAKE_CWD"], os.getcwd())
# R9.C6: the THREADS shape — one port that is both the address and the health
# URL (the chassis door serves /health). With FAKE_STALL present it still
# ACCEPTS every connection (the kernel would anyway) but never answers: the
# wedge `probe = "connect"` reads as live.
same = os.environ["FAKE_HEALTH_PORT"] == os.environ["FAKE_PORT"]
stall = os.environ.get("FAKE_STALL", "")
held = []
if not same:
    lock = socket.socket(); lock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    lock.bind(("127.0.0.1", int(os.environ["FAKE_PORT"]))); lock.listen(16)
    def drain():
        while True:
            c, _ = lock.accept(); c.close()
    threading.Thread(target=drain, daemon=True).start()
h = socket.socket(); h.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
h.bind(("127.0.0.1", int(os.environ["FAKE_HEALTH_PORT"]))); h.listen(128)
while True:
    c, _ = h.accept()
    if stall and os.path.exists(stall):
        held.append(c)
        continue
    try:
        # Read to the blank line, as a real server does (R5.S3b: one recv saw
        # only `GET ` when the client wrote the request in pieces).
        c.settimeout(5)
        data = b""
        while b"\r\n\r\n" not in data and len(data) < 8192:
            more = c.recv(1024)
            if not more: break
            data += more
        req = data.decode(errors="replace").split(" ")
        path = req[1] if len(req) > 1 else "/"
        ok = path == "/health" and not os.path.exists(os.environ["FAKE_SICK"])
        c.sendall(b"HTTP/1.0 200 OK\r\n\r\nok\n" if ok else b"HTTP/1.0 503 Sick\r\n\r\nno\n")
    except Exception:
        pass
    c.close()
"#;

struct Sandbox {
    root: PathBuf,
    env: String,
    daemon: Option<Child>,
    log: PathBuf,
    /// JOBS-DST: extra environment for every `cauldron` this sandbox runs
    /// (the injected zone, `TZ`).
    extra_env: Vec<(String, String)>,
}

/// R5.S9: the DROP GUARD. It runs on success AND on panic (unwind), and it
/// does not trust `fake-pids`: a fixture the holder had just relaunched (the
/// `sick` test stops and relaunches its service on every poll) may not have
/// written its pid yet when the file is read, and services live in their OWN
/// process group by design (`cli/src/main.rs`, `process_group(0)`), so killing
/// the holder never reaches them. So after the holder is dead and waited —
/// nothing can spawn another — every process whose command line names THIS
/// sandbox's root is killed, and the scan is repeated until it is empty.
/// A fixture still alive after that FAILS the test (or, mid-panic, is shouted
/// on stderr, since a second panic would abort).
impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        if let Ok(t) = std::fs::read_to_string(self.root.join("fake-pids")) {
            for p in t.lines() {
                let _ = Command::new("/bin/kill").args(["-9", p.trim()]).stderr(Stdio::null()).status();
            }
        }
        let survivors = reap_fixtures(&self.root);
        if std::env::var_os("KEEP_SANDBOX").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
        if !survivors.is_empty() {
            let msg = format!("R5.S9: fixture(s) of {} survived their test: {survivors:?}", self.root.display());
            if std::thread::panicking() {
                eprintln!("{msg}");
            } else {
                panic!("{msg}");
            }
        }
    }
}

/// Every live process whose command line names `root`. IDENTITY: the root
/// carries the test binary's pid and a nanosecond stamp, so nothing but this
/// sandbox's own fixtures can match it — and `ps` itself does not (its
/// command line is `/bin/ps -wwaxo pid=,command=`).
fn fixtures_of(root: &std::path::Path) -> Vec<(u32, String)> {
    let needle = root.display().to_string();
    let out = Command::new("/bin/ps").args(["-wwaxo", "pid=,command="]).output().expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(&needle))
        .filter_map(|l| {
            let (pid, cmd) = l.trim_start().split_once(' ')?;
            Some((pid.parse().ok()?, cmd.to_string()))
        })
        .filter(|(pid, _)| *pid != std::process::id())
        .collect()
}

/// Kill every fixture of `root` (identity-checked by `fixtures_of` right
/// before each kill) until a scan finds none, within 10 s. Returns whatever
/// is still alive after that — empty is the only passing answer.
fn reap_fixtures(root: &std::path::Path) -> Vec<(u32, String)> {
    let t0 = Instant::now();
    loop {
        let found = fixtures_of(root);
        if found.is_empty() || t0.elapsed() > Duration::from_secs(10) {
            return found;
        }
        for (pid, _) in &found {
            let _ = Command::new("/bin/kill").args(["-9", &pid.to_string()]).stderr(Stdio::null()).status();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `cmd` with every `FAKE_*` variable removed, so a fake exec'd only to pay
/// its first exec raises at its first `os.environ[...]` and does nothing.
fn no_fake_env(mut cmd: Command) -> Command {
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("FAKE_") {
            cmd.env_remove(k);
        }
    }
    cmd
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

fn kill9(pid: u32) {
    assert!(Command::new("/bin/kill").args(["-9", &pid.to_string()]).status().unwrap().success());
    wait_for("the killed pid to be gone", Duration::from_secs(10), || (!alive(pid)).then_some(()));
}

const POLICY: &str = "auto_restart = true\nauto_rollback = false\npoll_secs = 1\ndown_after = 1\n\
                      max_start_failures = 3\nbackoff_base_secs = 3\nbackoff_max_secs = 30\n\
                      stable_after_secs = 600";
/// **The declared period on the path to a service's first sight (R9.F1)**,
/// replacing a fixed 120 s (itself raised from 40 s when the R5.S3 final run
/// timed out every wait at load average 26 — that was first-exec cost, see
/// `support/first_exec.rs`): the holder's first pass (`poll_secs` 1 x
/// `down_after` 1 = 1 s); one launch bounded by `HOSTED_BIND_WAIT` (45 s,
/// `cli/src/daemon.rs`) and then the declared `start_timeout_ms`
/// ([`launch_wait::START_TIMEOUT`], 60 s since R9.F2g) for the http health
/// check; if that launch failed, one backoff of at most `backoff_max_secs`
/// (30 s) before a launch that succeeds. 1 + 45 + 60 + 30 = 136 s. Every positive wait here is a bound on a HANG; the negative tests
/// use their own fixed waits and are unaffected.
const FIRST_UP_DECLARED: Duration = Duration::from_secs(1 + 45 + launch_wait::START_TIMEOUT_SECS + 30);

/// Every positive wait: [`FIRST_UP_DECLARED`] plus the measured worst-case
/// first exec of the two fresh files on that path — the holder's own launch
/// path and the service's build copy — which `start_daemon` and
/// `build_and_pin` pay before the clock starts; still budgeted, so a pre-pay
/// that stops reaching one reads as slow, not red.
fn budget() -> Duration {
    first_exec::first_wait(FIRST_UP_DECLARED, 2)
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "cauldron-r5s3-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("r5s3{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::create_dir_all(root.join("data").join(&env)).unwrap();
        std::fs::write(root.join("data").join(&env).join("env.toml"), format!("[supervisor]\n{POLICY}\n")).unwrap();
        // The fake, as a BUNDLE (the ui-server shape) and as a plain file (the job shape).
        let macos = root.join("Fake.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        for p in [macos.join("fake"), root.join("fake-job")] {
            std::fs::write(&p, launch_wait::slow_launch_py(&first_exec::slow_first_exec_py(FAKE, &root.join("first-exec-marks")), &root.join("f2g-marks"))).unwrap();
            Command::new("/bin/chmod").arg("+x").arg(&p).status().unwrap();
        }
        std::fs::create_dir_all(root.join("cwd-here")).unwrap();
        let log = root.join("daemon.log");
        Sandbox { root, env, daemon: None, log, extra_env: Vec::new() }
    }

    fn cauldron(&self, args: &[&str]) -> (i32, String) {
        self.cauldron_with(args, &[])
    }

    /// [`Sandbox::cauldron`] with more environment (JOBS-DST: an injected
    /// `CAULDRON_TEST_NOW` and `TZ`).
    fn cauldron_with(&self, args: &[&str], more: &[(&str, &str)]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_cauldron"))
            .args(args)
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .env(crate::grants::ALLOW_ENV, crate::grants::owned(&self.root, &self.daemon.iter().map(Child::id).collect::<Vec<_>>()))
            .envs(self.extra_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .envs(more.iter().copied())
            .output()
            .expect("spawn cauldron");
        (
            out.status.code().unwrap_or(-1),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    }

    fn common_env(&self) -> String {
        format!(
            "FAKE_PIDS = \"{}\"\nFAKE_CWD = \"{}\"\nFAKE_RUNS = \"{}\"\nFAKE_SICK = \"{}\"\nFAKE_OWNER = \"{}\"\n",
            self.root.join("fake-pids").display(),
            self.root.join("fake-cwd").display(),
            self.root.join("fake-runs").display(),
            self.root.join("sick").display(),
            std::process::id(),
        )
    }

    /// The ui-server shape: bundle, http probe on a second port, a cwd.
    fn declare_daemon(&self, name: &str, health_path: &str, extra_top: &str) -> (u16, u16) {
        let (port, hport) = (free_port(), free_port());
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"http\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n{extra_top}\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\nhealth = \"http://127.0.0.1:{hport}{health_path}\"\ncwd = \"{}\"\n\
                 [env]\nFAKE_PORT = \"{port}\"\nFAKE_HEALTH_PORT = \"{hport}\"\n{}\
                 [build]\nartifact = \"{}\"\nbin = \"Fake.app/Contents/MacOS/fake\"\n",
                self.root.join("cwd-here").display(),
                self.common_env(),
                self.root.join("Fake.app").display()
            ),
        )
        .unwrap();
        (port, hport)
    }

    /// R9.C6 — THE THREADS SHAPE: one port that is the address AND serves
    /// `/health` (the chassis door), probed by `probe` (`http` or `connect`),
    /// with its own stall switch at `<root>/stall-<name>`.
    fn declare_threads_shaped(&self, name: &str, probe: &str) -> u16 {
        let port = free_port();
        let health = if probe == "http" {
            // Written exactly as `services/threads.toml` writes it, so the
            // per-environment expansion is on the path under test too.
            "health = \"http://${CAULDRON_HOSTED_ADDR}/health\"\n".to_string()
        } else {
            String::new()
        };
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"{probe}\"\n\
                 stop_signal = \"TERM\"\nescalate = true\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\n{health}cwd = \"{}\"\n\
                 [env]\nFAKE_PORT = \"{port}\"\nFAKE_HEALTH_PORT = \"{port}\"\nFAKE_STALL = \"{}\"\n{}\
                 [build]\nartifact = \"{}\"\nbin = \"Fake.app/Contents/MacOS/fake\"\n",
                self.root.join("cwd-here").display(),
                self.stall_file(name).display(),
                self.common_env(),
                self.root.join("Fake.app").display()
            ),
        )
        .unwrap();
        port
    }

    fn stall_file(&self, name: &str) -> PathBuf {
        self.root.join(format!("stall-{name}"))
    }

    /// The soup-backup shape: a scheduled job.
    fn declare_job(&self, name: &str, extra_top: &str, schedule: &str, env: &str) {
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"job\"\nregisters = false\nprobe = \"none\"\n{extra_top}\n\
                 [schedule]\n{schedule}\n\
                 [env]\n{}{env}\n\
                 [build]\nartifact = \"{}\"\nbin = \"fake-job\"\n",
                self.common_env(),
                self.root.join("fake-job").display()
            ),
        )
        .unwrap();
    }

    fn build_and_pin(&self, name: &str) {
        let (rc, out) = self.cauldron(&["build", name]);
        assert_eq!(rc, 0, "build {name}: {out}");
        let (rc, out) = self.cauldron(&["versions", name, "--json"]);
        assert_eq!(rc, 0, "versions {name}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        let label = v[0]["label"].as_str().expect("a label").to_string();
        self.pay_build(name, &label);
        let (rc, out) = self.cauldron(&["pin", name, &label]);
        assert_eq!(rc, 0, "pin {name}: {out}");
    }

    /// R9.F1: pay the first exec of the copy `cauldron build` just made of
    /// the fixture (a NEW file, so a new assessment — `support/first_exec.rs`)
    /// outside every deadline. No `FAKE_*` in the environment, so the fake
    /// raises before it notes, binds or runs anything.
    fn pay_build(&self, name: &str, label: &str) {
        let dir = self.root.join("data").join("shared").join("builds").join(name).join(label);
        let exes = first_exec::executables_under(&dir);
        assert!(!exes.is_empty(), "sentinel: `build` left no executable in {}", dir.display());
        for x in exes {
            first_exec::pay_first_exec(&mut no_fake_env(Command::new(&x)), &format!("{name}'s build {label}"));
        }
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
            .env(crate::grants::ALLOW_ENV, crate::grants::owned(&self.root, &[]))
            .envs(self.extra_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("spawn daemon");
        self.daemon = Some(child);
    }

    fn stop_daemon(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
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

    fn supervisions(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.root.join("data").join(&self.env).join("supervisions.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn acts(&self, name: &str, act: &str) -> Vec<serde_json::Value> {
        self.supervisions().into_iter().filter(|r| r["service"] == name && r["act"] == act).collect()
    }

    fn status_state(&self, name: &str) -> (String, String) {
        let (_, out) = self.cauldron(&["status", name, "--json"]);
        let v: serde_json::Value = match serde_json::from_str(&out) {
            Ok(v) => v,
            Err(_) => return (String::new(), out),
        };
        let rows = v["services"].as_array().cloned().or_else(|| v.as_array().cloned()).unwrap_or_default();
        let row = rows.into_iter().find(|r| r["service"] == name).unwrap_or_default();
        (row["state"].as_str().unwrap_or("").to_string(), row["detail"].as_str().unwrap_or("").to_string())
    }

    fn runs(&self) -> Vec<u32> {
        std::fs::read_to_string(self.root.join("fake-runs"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix("start ").and_then(|p| p.trim().parse().ok()))
            .collect()
    }
}

// ═══════════════════════════════════════════════ the ui-server shape ════

#[test]
fn an_http_probed_bundle_service_runs_in_its_cwd_and_is_restarted_after_a_kill() {
    let mut sb = Sandbox::new("ui");
    let (port, _) = sb.declare_daemon("ui", "/health", "");
    sb.build_and_pin("ui");
    // The bundle was installed WHOLE: the executable sits inside a copy of the
    // bundle in the version directory, not as a lone file.
    let builds = sb.root.join("data").join("shared").join("builds").join("ui");
    let label = std::fs::read_dir(&builds).unwrap().next().unwrap().unwrap().path();
    assert!(label.join("Fake.app/Contents/MacOS/fake").is_file(), "bundle not installed whole under {}", label.display());

    sb.start_daemon();
    let first = wait_for("ui up under the daemon", budget(), || sb.up("ui", port));
    let cwd = std::fs::read_to_string(sb.root.join("fake-cwd")).unwrap();
    assert_eq!(cwd.lines().next().unwrap(), sb.root.join("cwd-here").canonicalize().unwrap().display().to_string());
    let (state, detail) = sb.status_state("ui");
    assert_eq!(state, "live:unverified", "{detail}");
    assert!(detail.contains("answered 200"), "live must be BECAUSE health answered 200: {detail}");

    kill9(first);
    let second = wait_for("ui relaunched after SIGKILL", budget(), || sb.up("ui", port).filter(|p| *p != first));
    assert_ne!(first, second);
    let downs: Vec<_> = sb.acts("ui", "launch").into_iter().filter(|r| r["why"] == "down").collect();
    assert!(downs.len() >= 2, "a launch/down per start (initial + restart): {:?}\n{}", downs, sb.log());
}

#[test]
fn with_restart_never_a_killed_service_stays_down_past_the_backoff() {
    let mut sb = Sandbox::new("never");
    let (port, _) = sb.declare_daemon("ui", "/health", "restart = \"never\"");
    // R5.S3b, doctrine 4: a POSITIVE CONTROL in the same daemon. `ctl` is the
    // same shape with the default `restart = "always"`, killed at the same
    // moment; it must be relaunched — through the same down-detection and
    // the same backoff — before `ui` is asserted still down. So the negative
    // waits as long as THIS daemon's real cycle takes, however loaded the
    // machine is, not a fixed guess at it.
    let (cport, _) = sb.declare_daemon("ctl", "/health", "");
    sb.build_and_pin("ui");
    sb.build_and_pin("ctl");
    sb.start_daemon();
    let first = wait_for("ui up under the daemon", budget(), || sb.up("ui", port));
    let cfirst = wait_for("ctl up under the daemon", budget(), || sb.up("ctl", cport));
    // CLI-ISO: the launch writes the row BEFORE it appends its LAUNCH record
    // (cli/src/main.rs, the one chokepoint), so `up` can be true while the
    // record is still owed; a count taken then reads 0 and the owed record
    // later reads as "a relaunch". Count once ui's first launch is recorded.
    let launches = wait_for("ui's first launch recorded", budget(), || {
        let n = sb.acts("ui", "launch").len();
        (n >= 1).then_some(n)
    });
    let killed = Instant::now();
    kill9(first);
    kill9(cfirst);
    wait_for("the control (restart = always) relaunched", budget(), || sb.up("ctl", cport).filter(|p| *p != cfirst));
    // And never less than 8 s: the 3 s backoff and 1 s poll, several times over.
    if let Some(rest) = Duration::from_secs(8).checked_sub(killed.elapsed()) {
        std::thread::sleep(rest);
    }
    assert!(!listening(port), "restart = never, yet something is listening again\n{}", sb.log());
    assert_eq!(sb.acts("ui", "launch").len(), launches, "a relaunch under restart = never\n{}", sb.log());
    assert!(sb.log().contains("restart = \\\"never\\\"") || sb.log().contains("restart = \"never\""), "{}", sb.log());
}

#[test]
fn a_health_check_that_does_not_answer_200_is_unhealthy_never_up() {
    // (a) A WRONG health path: the launch waits for 200, never gets it, and
    // the instance is killed again — nothing is recorded as up.
    let mut sb = Sandbox::new("badpath");
    let (port, _) = sb.declare_daemon("ui", "/nope", "");
    sb.build_and_pin("ui");
    sb.start_daemon();
    wait_for("a failed start that names the health check", budget(), || {
        sb.log().contains("never became healthy").then_some(())
    });
    let (state, detail) = sb.status_state("ui");
    assert_ne!(state, "live:unverified", "a 503 health check read as up: {detail}");
    assert!(sb.pid_of("ui").is_none(), "a row was written for an instance that never answered 200");
    let _ = port;
    drop(sb);

    // (b) HEALTHY, then SICK: the same running instance turns `unhealthy`,
    // and the daemon stops it (`stop`, why = down).
    let mut sb = Sandbox::new("sick");
    let (port, _) = sb.declare_daemon("ui", "/health", "");
    sb.build_and_pin("ui");
    sb.start_daemon();
    let first = wait_for("ui up", budget(), || sb.up("ui", port));
    std::fs::write(sb.root.join("sick"), "").unwrap();
    let (state, detail) = wait_for("status to read unhealthy", budget(), || {
        let (s, d) = sb.status_state("ui");
        (s == "unhealthy" || !alive(first)).then_some((s, d))
    });
    if alive(first) {
        assert_eq!(state, "unhealthy", "{detail}");
    }
    wait_for("the daemon to stop the unhealthy instance", budget(), || {
        sb.acts("ui", "stop").into_iter().find(|r| r["why"] == "down")
    });
}

/// **R9.C6 (gap C-6) — A WEDGED THREADS THAT STILL ACCEPTS TCP IS RESTARTED
/// UNDER `probe = "http"`, AND A HEALTHY ONE BESIDE IT IS NOT.**
///
/// Two threads-shaped services in ONE daemon (one port each, serving its own
/// `/health`): `wedged` is made to accept connections and never answer them;
/// `fine` is left alone. Claims:
///
/// 1. `wedged` reads `unhealthy` while its port still completes the TCP
///    handshake — the state `probe = "connect"` reads as live — and the
///    daemon stops it (`stop`, why = down) and launches a new instance, which
///    comes up once the stall is lifted;
/// 2. OPPOSED, in the same daemon: `fine`, probed by the same code on the
///    same poll, is never stopped and never relaunched, asserted only after
///    `wedged`'s full detect→stop→relaunch cycle has run (the positive
///    control) and then 8 more poll periods (1 s each).
///
/// Forced red (R9.C6 report): `wedged` declared `probe = "connect"` → claim 1
/// times out with no stop recorded.
#[test]
fn r9c6_a_wedged_service_that_still_accepts_tcp_is_restarted_under_http_and_a_healthy_one_is_not() {
    let mut sb = Sandbox::new("wedge");
    let wport = sb.declare_threads_shaped("wedged", "http");
    let fport = sb.declare_threads_shaped("fine", "http");
    sb.build_and_pin("wedged");
    sb.build_and_pin("fine");
    sb.start_daemon();
    let first = wait_for("wedged up", budget(), || sb.up("wedged", wport));
    let fine = wait_for("fine up", budget(), || sb.up("fine", fport));
    let fine_launches = sb.acts("fine", "launch").len();

    std::fs::write(sb.stall_file("wedged"), "").unwrap();
    // What `connect` sees: the handshake still completes on the wedged port.
    assert!(listening(wport), "premise: a wedged service still accepts TCP");
    let (state, detail) = wait_for("status to read the wedged service unhealthy", budget(), || {
        let (s, d) = sb.status_state("wedged");
        (s == "unhealthy" || !alive(first)).then_some((s, d))
    });
    if alive(first) {
        assert_eq!(state, "unhealthy", "{detail}");
        assert!(listening(wport), "unhealthy while still accepting TCP is the whole claim");
    }
    let stop = wait_for("the daemon to stop the wedged instance", budget(), || {
        sb.acts("wedged", "stop").into_iter().find(|r| r["why"] == "down")
    });
    std::fs::remove_file(sb.stall_file("wedged")).unwrap();
    let second = wait_for("a new wedged instance launched and healthy", budget(), || {
        sb.up("wedged", wport).filter(|p| *p != first)
    });
    assert_ne!(first, second, "stop: {stop}");
    assert!(!alive(first), "the wedged pid {first} survived its stop");

    // ── OPPOSED: the healthy one, after the positive control's full cycle.
    std::thread::sleep(Duration::from_secs(8));
    assert_eq!(sb.up("fine", fport), Some(fine), "the healthy service was replaced\n{}", sb.log());
    assert!(sb.acts("fine", "stop").is_empty(), "the healthy service was stopped: {:?}", sb.acts("fine", "stop"));
    assert_eq!(sb.acts("fine", "launch").len(), fine_launches, "the healthy service was relaunched\n{}", sb.log());
}

/// R5.S9 (H179-1, doctrine 10: who is in the degraded state). A test that
/// PANICS with a sick fixture running still reaps it — and reaps one whose pid
/// is missing from `fake-pids`, the relaunch race that orphaned pids 13465 and
/// 40066 (ppid 1, `cauldron-r5s3-sick-*`). The holder is killed first, so the
/// fixture is unattended (services outlive their holder by design) and only
/// the drop guard can end it.
#[test]
fn r5s9_a_panicking_test_still_reaps_its_sick_fixture_even_one_missing_from_fake_pids() {
    let seen = std::sync::Mutex::new(None::<(u32, PathBuf)>);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut sb = Sandbox::new("sickpanic");
        let (port, _) = sb.declare_daemon("ui", "/health", "");
        sb.build_and_pin("ui");
        sb.start_daemon();
        let pid = wait_for("ui up", budget(), || sb.up("ui", port));
        sb.stop_daemon();
        std::fs::write(sb.root.join("sick"), "").unwrap();
        std::fs::remove_file(sb.root.join("fake-pids")).unwrap();
        assert!(alive(pid), "premise: the fixture outlives its SIGKILLed holder");
        assert!(
            fixtures_of(&sb.root).iter().any(|(p, _)| *p == pid),
            "premise: the scan finds the running fixture by its sandbox root: {:?}",
            fixtures_of(&sb.root)
        );
        *seen.lock().unwrap() = Some((pid, sb.root.clone()));
        panic!("R5.S9 forced panic with a sick fixture running and absent from fake-pids");
    }));
    assert!(r.is_err(), "premise: the inner test panicked");
    let (pid, root) = seen.lock().unwrap().take().expect("the inner test reached its panic");
    assert!(!alive(pid), "the sick fixture {pid} survived a panicking test");
    assert!(fixtures_of(&root).is_empty(), "fixtures survived: {:?}", fixtures_of(&root));
}

/// R5.S9: the fixture's OWNER WATCH, for the case no drop guard can cover — a
/// test binary that dies without unwinding. Opposed: while its owner lives
/// the fixture keeps running past 3 watch periods (1 s each); once the owner
/// is gone it exits by itself.
#[test]
fn r5s9_a_fixture_exits_once_its_owning_test_process_is_gone_outlasting_3_watch_periods() {
    let sb = Sandbox::new("owner");
    // R9.F1: `fake-job` was written by `Sandbox::new` and never exec'd; pay
    // that first exec here, so the 3.5 s hold below is spent by a RUNNING
    // fixture and not by the scanner (`support/first_exec.rs`).
    first_exec::pay_first_exec(&mut no_fake_env(Command::new(sb.root.join("fake-job"))), "fake-job");
    let mut owner = Command::new("/bin/sleep").arg("600").spawn().unwrap();
    let mut fake = Command::new(sb.root.join("fake-job"))
        .arg("run")
        .env("FAKE_PIDS", sb.root.join("fake-pids"))
        .env("FAKE_RUNS", sb.root.join("fake-runs"))
        .env("FAKE_SLEEP", "600")
        .env("FAKE_OWNER", owner.id().to_string())
        .spawn()
        .unwrap();
    let pid = fake.id();
    // PREMISE (R9.F1, doctrine 4): the hold below outlasts 3 watch periods
    // only if it starts once the watch is RUNNING. The fixture starts its
    // owner watch, then notes `start <pid>`; until that line exists a hold
    // was being spent on the exec itself, and "still alive at 3.5 s" held for
    // a fixture that had not yet run a line. First wait: the declared watch
    // period (1 s) plus one fresh first exec (paid above; still budgeted).
    wait_for(
        "the fixture to start its owner watch",
        first_exec::first_wait(Duration::from_secs(1), 1),
        || sb.runs().contains(&pid).then_some(()),
    );
    std::thread::sleep(Duration::from_millis(3500));
    assert!(fake.try_wait().unwrap().is_none(), "the fixture exited while its owner was alive");
    let _ = owner.kill();
    let _ = owner.wait();
    let gone = wait_for("the fixture to notice its owner is gone", Duration::from_secs(15), || {
        fake.try_wait().unwrap()
    });
    assert!(gone.success(), "the owner watch exits 0: {gone:?} (pid {pid})");
}

// ═════════════════════════════════════════════ the soup-backup shape ════

#[test]
fn a_job_runs_at_load_and_a_killed_pass_is_retried() {
    let mut sb = Sandbox::new("job");
    sb.declare_job("jb", "", "every_secs = 3600\nargs = [\"run\"]\nretry_secs = 2", "FAKE_SLEEP = \"30\"\n");
    sb.build_and_pin("jb");
    sb.start_daemon();
    let first = wait_for("the first pass", budget(), || sb.runs().first().copied());
    assert_eq!(sb.acts("jb", "run")[0]["why"], "schedule");
    let (state, _) = sb.status_state("jb");
    assert_eq!(state, "job:running");
    kill9(first);
    let second = wait_for("a retried pass", budget(), || sb.runs().get(1).copied());
    assert_ne!(first, second);
    let ran = sb.acts("jb", "ran");
    assert!(ran.iter().any(|r| r["detail"].as_str().unwrap_or("").contains("killed by signal 9")), "{ran:?}");
    let runs = sb.acts("jb", "run");
    assert_eq!(runs.last().unwrap()["why"], "retry", "{runs:?}");
}

#[test]
fn a_job_with_restart_never_is_not_retried_after_a_kill() {
    let mut sb = Sandbox::new("jobnever");
    sb.declare_job("jb", "restart = \"never\"", "every_secs = 3600\nargs = [\"run\"]\nretry_secs = 2", "FAKE_SLEEP = \"30\"\n");
    // R5.S3b, doctrine 4: the same job with `restart = "always"`, killed at
    // the same moment, must be RETRIED before `jb` is asserted not re-run —
    // the negative outlasts this daemon's real retry path, not a guess at it.
    sb.declare_job("ctl", "", "every_secs = 3600\nargs = [\"run\"]\nretry_secs = 2", "FAKE_SLEEP = \"30\"\n");
    sb.build_and_pin("jb");
    sb.build_and_pin("ctl");
    sb.start_daemon();
    let pid_of_run = |sb: &Sandbox, name: &str| -> Option<u32> {
        sb.acts(name, "run").first().and_then(|r| r["pid"].as_u64()).map(|p| p as u32)
    };
    let first = wait_for("jb's first pass", budget(), || pid_of_run(&sb, "jb"));
    let cfirst = wait_for("ctl's first pass", budget(), || pid_of_run(&sb, "ctl"));
    wait_for("both passes running", budget(), || (sb.runs().len() >= 2).then_some(()));
    let killed = Instant::now();
    kill9(first);
    kill9(cfirst);
    wait_for("the end of jb's killed pass to be recorded", budget(), || sb.acts("jb", "ran").first().cloned());
    wait_for("the control (restart = always) retried", budget(), || {
        sb.acts("ctl", "run").into_iter().find(|r| r["why"] == "retry")
    });
    // And never less than 8 s: retry_secs 2 and poll 1, several times over.
    if let Some(rest) = Duration::from_secs(8).checked_sub(killed.elapsed()) {
        std::thread::sleep(rest);
    }
    assert_eq!(sb.acts("jb", "run").len(), 1, "restart = never, yet the pass was re-run\n{}", sb.log());
    let ran = sb.acts("jb", "ran");
    assert!(ran[0]["detail"].as_str().unwrap().contains("NOT retried"), "{ran:?}");
}

#[test]
fn a_job_missing_its_requirement_is_blocked_and_a_failing_health_reads_unhealthy() {
    let mut sb = Sandbox::new("jobreq");
    let cfg = sb.root.join("the-config");
    sb.declare_job(
        "jb",
        "",
        &format!("every_secs = 3600\nargs = [\"run\"]\nhealth_args = [\"health\"]\nrequires = [\"{}\"]", cfg.display()),
        "",
    );
    // R5.S3b, doctrine 4: an UNBLOCKED twin in the same daemon, so "no pass
    // ran" is asserted only after this daemon has demonstrably run a pass.
    sb.declare_job("ctl", "", "every_secs = 3600\nargs = [\"run\"]", "");
    sb.build_and_pin("jb");
    sb.build_and_pin("ctl");
    sb.start_daemon();
    wait_for("status to say blocked", budget(), || (sb.status_state("jb").0 == "job:blocked").then_some(()));
    wait_for("the unblocked control's pass", budget(), || sb.acts("ctl", "ran").first().cloned());
    std::thread::sleep(Duration::from_secs(3));
    assert!(sb.acts("jb", "run").is_empty(), "a pass ran with its requirement missing");
    // The HEALTH CHECK is not run either: a blocked job asked on its defaults
    // answers about some other environment's data (measured in the R5.S3 proof).
    let (state, detail) = sb.status_state("jb");
    assert_eq!(state, "job:blocked", "{detail}");
    let calls = std::fs::read_to_string(sb.root.join("fake-runs")).unwrap_or_default();
    assert!(!calls.contains("health"), "the health check ran while blocked: {calls:?}");
    // R5.S3b — THE LATENCY IS THE BEHAVIOUR (0373bc92): a blocked job is
    // re-checked EVERY pass, so its pass starts within a few polls of the
    // requirement appearing. Bound: 3 polls (poll_secs = 1) + 3 s for the
    // fake's own start. The old code waited up to 30 s; `budget()`
    // alone swallowed that (the inspector reverted it and stayed green).
    let appeared = Instant::now();
    std::fs::write(&cfg, "x").unwrap();
    wait_for("the pass once the requirement exists", budget(), || sb.acts("jb", "run").first().cloned());
    let latency = appeared.elapsed();
    assert!(
        latency < Duration::from_secs(6),
        "the blocked job's pass started {latency:?} after its requirement appeared; \
         re-checked every 1 s poll, it must start within ~3 polls\n{}",
        sb.log()
    );
    wait_for("job:ok", budget(), || (sb.status_state("jb").0 == "job:ok").then_some(()));
    std::fs::write(sb.root.join("sick"), "").unwrap();
    let (state, detail) = sb.status_state("jb");
    assert_eq!(state, "job:unhealthy", "{detail}");
    assert!(detail.contains("FAILING"), "{detail}");
}

/// R5.S3b — the job twin of `launch_for_daemon`'s `[migration]` refusal,
/// along the inspector's path: build WITH `[migration]`, delete it from the
/// shipped file (the cut-over commit, before a rebuild), pin the earlier
/// label (`pin` reads only the shipped file), run the daemon. The pinned
/// build still says another launcher runs it, so the holder must not.
/// Paired with a clean job in the same daemon that DOES run.
#[test]
fn a_job_pinned_to_a_build_that_still_carries_migration_is_refused_not_run() {
    let mut sb = Sandbox::new("jobmig");
    // R5.S3c: both declare a health check, and the fake notes WHOSE ran.
    let schedule = "every_secs = 3600\nargs = [\"run\"]\nhealth_args = [\"health\"]";
    sb.declare_job("mj", "", schedule, "FAKE_WHO = \"mj\"\n");
    let decl = sb.root.join("services").join("mj.toml");
    let clean = std::fs::read_to_string(&decl).unwrap();
    std::fs::write(&decl, format!("{clean}\n[migration]\nlaunched_by = \"launchd:com.example.fake\"\nsteps = [\"delete this section\"]\n")).unwrap();
    let (rc, out) = sb.cauldron(&["build", "mj"]);
    assert_eq!(rc, 0, "build mj (build does not refuse [migration]): {out}");
    let (rc, out) = sb.cauldron(&["versions", "mj", "--json"]);
    assert_eq!(rc, 0, "versions mj: {out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    let label = v[0]["label"].as_str().expect("a label").to_string();
    // Positive control of the SHIPPED-file check: pinned now, it is refused.
    let (rc, out) = sb.cauldron(&["pin", "mj", &label]);
    assert_ne!(rc, 0, "pin of a [migration] declaration was accepted: {out}");
    assert!(out.contains("REFUSED"), "{out}");
    // The cut-over edit, then the stale label.
    std::fs::write(&decl, &clean).unwrap();
    let (rc, out) = sb.cauldron(&["pin", "mj", &label]);
    assert_eq!(rc, 0, "pin of the pre-edit label once the shipped file is clean: {out}");
    sb.declare_job("ctl", "", schedule, "FAKE_WHO = \"ctl\"\n");
    sb.build_and_pin("ctl");
    sb.start_daemon();
    wait_for("the clean control's pass", budget(), || sb.acts("ctl", "run").first().cloned());
    // Either outcome ends the wait, so a missing check is red at once, not
    // after the budget: refused (the state) or run (the defect).
    wait_for("mj refused or run", budget(), || {
        (sb.status_state("mj").0 == "job:refused" || !sb.acts("mj", "run").is_empty()).then_some(())
    });
    assert!(sb.acts("mj", "run").is_empty(), "the holder RAN a job whose pinned build carries [migration]\n{}", sb.log());
    let (state, detail) = sb.status_state("mj");
    assert_eq!(state, "job:refused", "{detail}");
    assert!(detail.contains("launchd:com.example.fake"), "the refusal must name the other launcher: {detail}");
    // R5.S3c (R5.S3b inspector finding 3): a FAILING health check must not
    // hide `job:refused`, and `status` must not run the refused build's check
    // at all. Positive control first: in this sandbox, sick, `status` DOES run
    // the clean job's check and reads it unhealthy — so the check is reachable
    // and failing, and only the refusal can keep it off mj's row.
    std::fs::write(sb.root.join("sick"), "").unwrap();
    wait_for("the control's pass to finish", budget(), || sb.acts("ctl", "ran").first().cloned());
    let (cstate, cdetail) = sb.status_state("ctl");
    assert_eq!(cstate, "job:unhealthy", "the control's failing health check: {cdetail}");
    let (state, detail) = sb.status_state("mj");
    assert_eq!(state, "job:refused", "a failing health check hid the refusal: {detail}");
    assert!(detail.contains("REFUSED") && detail.contains("launchd:com.example.fake"), "{detail}");
    let calls = std::fs::read_to_string(sb.root.join("fake-runs")).unwrap_or_default();
    assert!(calls.lines().any(|l| l == "health ctl"), "the control's health check never ran: {calls:?}");
    assert!(!calls.lines().any(|l| l == "health mj"), "status ran the REFUSED build's health check: {calls:?}");
}

/// R5.S3d (R5.S3c inspector finding 1): `status` decides a refusal from the
/// PINNED build, not from the record. Along the runbook's stale-label
/// recovery for the ALARM's shape (`run_at_load = false`, so no pass rewrites
/// the record after a re-pin): refused → rebuild → pin the NEW clean label →
/// `status` reads `job:never-run` at once, with the holder down and with it
/// up. Opposed: a re-pin to another label that STILL carries `[migration]`
/// reads `job:refused` at once — before the holder has seen it, while the
/// record names a different label.
#[test]
fn a_clean_label_re_pinned_over_a_refused_one_is_not_reported_refused() {
    let mut sb = Sandbox::new("jobrepin");
    let schedule = "every_secs = 3600\nrun_at_load = false\nargs = [\"run\"]";
    sb.declare_job("mj", "", schedule, "");
    let decl = sb.root.join("services").join("mj.toml");
    let clean = std::fs::read_to_string(&decl).unwrap();
    let with_migration =
        format!("{clean}\n[migration]\nlaunched_by = \"launchd:com.example.fake\"\nsteps = [\"delete this section\"]\n");
    let newest = |sb: &Sandbox| -> String {
        let (rc, out) = sb.cauldron(&["versions", "mj", "--json"]);
        assert_eq!(rc, 0, "versions mj: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        v[0]["label"].as_str().expect("a label").to_string()
    };
    // A new label needs new artifact bytes: this unstamped sandbox derives the
    // label from the artifact (R5.S3c inspector, 5b).
    let artifact = sb.root.join("fake-job");
    let new_bytes = |tag: &str| {
        let p = &artifact;
        let t = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, format!("{t}# {tag}\n")).unwrap();
    };
    let job_record = |sb: &Sandbox| -> serde_json::Value {
        let t = std::fs::read_to_string(sb.root.join("data").join(&sb.env).join("jobs.json")).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&t).ok().map(|v| v["mj"].clone()).unwrap_or_default()
    };

    // 1. The stale label (built WITH `[migration]`, shipped file then cleaned).
    std::fs::write(&decl, &with_migration).unwrap();
    let (rc, out) = sb.cauldron(&["build", "mj"]);
    assert_eq!(rc, 0, "build mj: {out}");
    let stale = newest(&sb);
    std::fs::write(&decl, &clean).unwrap();
    let (rc, out) = sb.cauldron(&["pin", "mj", &stale]);
    assert_eq!(rc, 0, "pin of the stale label: {out}");
    sb.declare_job("ctl", "", "every_secs = 3600\nargs = [\"run\"]", "");
    sb.build_and_pin("ctl");
    sb.start_daemon();
    wait_for("the holder to record mj refused", budget(), || (job_record(&sb)["state"] == "refused").then_some(()));
    assert_eq!(sb.status_state("mj").0, "job:refused");
    sb.stop_daemon();

    // 2. The remedy: rebuild from the clean file, pin the NEW label.
    new_bytes("v2");
    let (rc, out) = sb.cauldron(&["build", "mj"]);
    assert_eq!(rc, 0, "rebuild mj: {out}");
    let fresh = newest(&sb);
    assert_ne!(fresh, stale, "sentinel: the rebuild must be a different label");
    let (rc, out) = sb.cauldron(&["pin", "mj", &fresh]);
    assert_eq!(rc, 0, "pin of the fresh label: {out}");
    // Sentinel (doctrine 9): the RECORD still says refused, under the OLD
    // label — so `status` reading never-run below is status deciding, not a
    // record that happened to be rewritten.
    let r = job_record(&sb);
    assert_eq!((r["state"].as_str(), r["version"].as_str()), (Some("refused"), Some(stale.as_str())), "{r}");
    let (state, detail) = sb.status_state("mj");
    assert_eq!(state, "job:never-run", "a CLEAN label is pinned and status still says refused: {detail}");
    assert!(!detail.contains("launchd:com.example.fake"), "the old label's refusal text is still shown: {detail}");
    // And with the holder up: it polls (positive control: THIS holder's first
    // look at ctl — since R5.S6 a `skip`, because ctl ran in the previous
    // holder well within its 3600 s, where before it was a re-run at load),
    // mj is not due for 3600 s, and the row stays never-run.
    let ctl_looks = |sb: &Sandbox| sb.acts("ctl", "run").len() + sb.acts("ctl", "skip").len();
    let ctl_before = ctl_looks(&sb);
    sb.start_daemon();
    wait_for("the new holder's first look at ctl", budget(), || (ctl_looks(&sb) > ctl_before).then_some(()));
    std::thread::sleep(Duration::from_secs(3)); // 3 polls at poll_secs = 1
    let (state, detail) = sb.status_state("mj");
    assert_eq!(state, "job:never-run", "with the holder up: {detail}");
    assert!(sb.acts("mj", "run").is_empty(), "mj ran (run_at_load = false, every 3600 s)\n{}", sb.log());
    sb.stop_daemon();

    // 3. Opposed: a re-pin to ANOTHER label that still carries `[migration]`
    // is refused at once, with the holder down and the record naming `stale`.
    std::fs::write(&decl, &with_migration).unwrap();
    new_bytes("v3");
    let (rc, out) = sb.cauldron(&["build", "mj"]);
    assert_eq!(rc, 0, "build mj v3: {out}");
    let refused_again = newest(&sb);
    assert!(refused_again != stale && refused_again != fresh, "sentinel: a third label");
    std::fs::write(&decl, &clean).unwrap();
    let (rc, out) = sb.cauldron(&["pin", "mj", &refused_again]);
    assert_eq!(rc, 0, "pin of the third (still-[migration]) label: {out}");
    assert_eq!(job_record(&sb)["version"].as_str(), Some(stale.as_str()), "sentinel: the holder has not seen this pin");
    let (state, detail) = sb.status_state("mj");
    assert_eq!(state, "job:refused", "a pinned build that carries [migration] must read refused: {detail}");
    assert!(detail.contains("REFUSED") && detail.contains("launchd:com.example.fake"), "{detail}");
}

/// R5.S3d-1 (folded into R5.S7): the holder's refusal log line is said once
/// PER PINNED LABEL, not once per `refused` state. One running holder; label
/// A (still `[migration]`) is pinned and refused, and the line naming A is
/// said once across 3 more polls (poll_secs = 1); then label B, also still
/// `[migration]`, is re-pinned with no pass in between — the record already
/// says `refused` — and the log must name B. Before the fix the B refusal
/// was never logged, because only the recorded state was compared.
#[test]
fn r5s3d1_a_refusal_of_a_second_label_is_logged_though_the_record_already_says_refused() {
    let mut sb = Sandbox::new("refuse2");
    sb.declare_job("mj", "", "every_secs = 3600\nrun_at_load = false\nargs = [\"run\"]", "");
    let decl = sb.root.join("services").join("mj.toml");
    let clean = std::fs::read_to_string(&decl).unwrap();
    let with_migration =
        format!("{clean}\n[migration]\nlaunched_by = \"launchd:com.example.fake\"\nsteps = [\"delete this section\"]\n");
    let artifact = sb.root.join("fake-job");
    let job_record = |sb: &Sandbox| -> serde_json::Value {
        let t = std::fs::read_to_string(sb.root.join("data").join(&sb.env).join("jobs.json")).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&t).ok().map(|v| v["mj"].clone()).unwrap_or_default()
    };
    // A label built WITH `[migration]`, pinned once the shipped file is clean.
    let refused_label = |sb: &Sandbox, tag: &str| -> String {
        let t = std::fs::read_to_string(&artifact).unwrap();
        std::fs::write(&artifact, format!("{t}# {tag}\n")).unwrap();
        std::fs::write(&decl, &with_migration).unwrap();
        let (rc, out) = sb.cauldron(&["build", "mj"]);
        assert_eq!(rc, 0, "build mj {tag}: {out}");
        let (rc, out) = sb.cauldron(&["versions", "mj", "--json"]);
        assert_eq!(rc, 0, "{out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        let label = v[0]["label"].as_str().expect("a label").to_string();
        std::fs::write(&decl, &clean).unwrap();
        let (rc, out) = sb.cauldron(&["pin", "mj", &label]);
        assert_eq!(rc, 0, "pin {label}: {out}");
        label
    };
    let said = |sb: &Sandbox, label: &str| {
        sb.log().lines().filter(|l| l.contains(&format!("pinned version `{label}` of `mj`")) && l.contains("REFUSED")).count()
    };

    let a = refused_label(&sb, "a");
    sb.start_daemon();
    wait_for("the holder to record label A refused", budget(), || {
        let r = job_record(&sb);
        (r["state"] == "refused" && r["version"] == a.as_str()).then_some(())
    });
    std::thread::sleep(Duration::from_secs(3)); // 3 polls at poll_secs = 1
    assert_eq!(said(&sb, &a), 1, "the refusal of A must be said exactly once\n{}", sb.log());

    let b = refused_label(&sb, "b");
    assert_ne!(a, b, "sentinel: two different labels");
    wait_for("the holder to record label B refused", budget(), || {
        let r = job_record(&sb);
        (r["state"] == "refused" && r["version"] == b.as_str()).then_some(())
    });
    assert!(sb.acts("mj", "run").is_empty(), "sentinel: no pass ran between the two refusals");
    std::thread::sleep(Duration::from_secs(3)); // 3 polls at poll_secs = 1
    assert_eq!(said(&sb, &b), 1, "the refusal of the SECOND label was not logged (or logged every poll)\n{}", sb.log());
    assert_eq!(said(&sb, &a), 1, "{}", sb.log());
}

/// R5.S4b inspector F1: nothing guarded the `last_slot` WRITE in `jobs.json`
/// (both `save_one` calls in `tend_with` writing `None` stayed 60/60 green,
/// because every slot test built its records by hand). Here the real holder
/// writes it: a calendar job (`at` twelve hours from now, so the slot is
/// nowhere near the start time) runs at load, and `jobs.json` must carry the
/// slot the test computes on its own clock — while running (the spawn write)
/// and after the pass (the `ran` write).
///
/// JOBS-DST: the holder runs with its zone INJECTED (`TZ=UTC`), so the
/// expected slot is plain arithmetic on one reading of now, taken once —
/// no `/bin/date`, no machine zone, no DST, and no second reading that a
/// midnight could fall between.
#[test]
fn the_holder_persists_the_calendar_slot_a_pass_was_started_for() {
    let mut sb = Sandbox::new("jobslot");
    sb.extra_env.push(("TZ".into(), "UTC".into()));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let sod = now % 86_400;
    let target = (sod + 12 * 3600) % 86_400 / 60 * 60;
    let at = format!("{:02}:{:02}", target / 3600, target % 3600 / 60);
    let expected = now - (sod + 86_400 - target) % 86_400;
    sb.declare_job("cj", "", &format!("at = \"{at}\"\nargs = [\"run\"]"), "FAKE_SLEEP = \"3\"\n");
    sb.build_and_pin("cj");
    let record = |sb: &Sandbox| -> serde_json::Value {
        let t = std::fs::read_to_string(sb.root.join("data").join(&sb.env).join("jobs.json")).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&t).ok().map(|v| v["cj"].clone()).unwrap_or_default()
    };
    sb.start_daemon();
    let check = |r: &serde_json::Value, when: &str| {
        let slot = r["last_slot"].as_u64().unwrap_or_else(|| panic!("{when}: jobs.json carries no last_slot: {r}"));
        assert!(
            slot.abs_diff(expected) <= 2,
            "{when}: jobs.json last_slot {slot} is not the {at} slot {expected} (off by {} s): {r}",
            slot as i64 - expected as i64
        );
        let start = r["last_start"].as_u64().expect("last_start");
        assert!(start.abs_diff(slot) > 3600, "sentinel: the slot must not be the start time ({start}): {r}");
    };
    let running = wait_for("cj running", budget(), || {
        let r = record(&sb);
        (r["state"] == "running").then_some(r)
    });
    check(&running, "while running (the spawn write)");
    let done = wait_for("cj's pass recorded", budget(), || {
        let r = record(&sb);
        (r["state"] == "ok" || r["state"] == "failed").then_some(r)
    });
    assert_eq!(done["state"], "ok", "{done}\n{}", sb.log());
    check(&done, "after the pass (the `ran` write)");
}

/// R5.S6 (H179-1, Deploy5-4): a holder restart is not a new load. Two real
/// holders in one sandbox. After the first has run every job once:
/// - `rj` (every 3600 s) ran seconds ago — the second holder does NOT run it
///   at load and records `skip` / `recent` with the reason;
/// - `sj` (every 5 s, and the restart waits 6 s) is overdue — it runs at once
///   and records no skip;
/// - `nj`, pinned between the holders, has never run — it runs at once.
/// `sj` and `nj` are this holder's positive controls; `rj`'s "no run" is
/// asserted only after them and after 4 more polls (poll_secs = 1), the
/// longest period in the path short of rj's own 3600 s (doctrine 4).
#[test]
fn r5s6_a_holder_restart_skips_a_recent_run_at_load_job_outlasting_4_polls() {
    let mut sb = Sandbox::new("loadskip");
    sb.declare_job("rj", "", "every_secs = 3600\nargs = [\"run\"]", "");
    sb.declare_job("sj", "", "every_secs = 5\nargs = [\"run\"]", "");
    sb.build_and_pin("rj");
    sb.build_and_pin("sj");
    let by = |r: &serde_json::Value| r["by"]["pid"].as_u64().unwrap_or(0) as u32;
    let record = |sb: &Sandbox, name: &str| -> serde_json::Value {
        let t = std::fs::read_to_string(sb.root.join("data").join(&sb.env).join("jobs.json")).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&t).ok().map(|v| v[name].clone()).unwrap_or_default()
    };

    sb.start_daemon();
    let h1 = sb.daemon.as_ref().unwrap().id();
    for name in ["rj", "sj"] {
        wait_for(&format!("holder 1's pass of {name} to end"), budget(), || {
            sb.acts(name, "ran").into_iter().find(|r| by(r) == h1)
        });
    }
    sb.stop_daemon();
    std::thread::sleep(Duration::from_secs(6)); // sj's 5 s period passes
    sb.declare_job("nj", "", "every_secs = 3600\nargs = [\"run\"]", "");
    sb.build_and_pin("nj");

    // Sentinels (doctrine 9): the three records the second holder decides on
    // are genuinely different — rj recent, sj older than its period, nj none.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let rj_start = record(&sb, "rj")["last_start"].as_u64().expect("rj has a last_start");
    let sj_start = record(&sb, "sj")["last_start"].as_u64().expect("sj has a last_start");
    assert!(now - rj_start < 3600, "sentinel: rj's pass must be within its period ({rj_start} vs {now})");
    assert!(now - sj_start >= 5, "sentinel: sj's pass must be older than its 5 s period ({sj_start} vs {now})");
    assert!(record(&sb, "nj").is_null(), "sentinel: nj must have no record");
    assert!(sb.acts("rj", "skip").is_empty(), "sentinel: no skip before the restart");
    let rj_runs = sb.acts("rj", "run").len();
    assert_eq!(rj_runs, 1, "sentinel: holder 1 ran rj once");

    sb.start_daemon();
    let h2 = sb.daemon.as_ref().unwrap().id();
    assert_ne!(h1, h2);
    for name in ["sj", "nj"] {
        wait_for(&format!("holder 2 to run {name} at load"), budget(), || {
            sb.acts(name, "run").into_iter().find(|r| by(r) == h2)
        });
    }
    let skip = wait_for("holder 2 to record rj's skip", budget(), || {
        sb.acts("rj", "skip").into_iter().find(|r| by(r) == h2)
    });
    std::thread::sleep(Duration::from_secs(4)); // 4 polls at poll_secs = 1
    assert_eq!(sb.acts("rj", "run").len(), rj_runs, "holder 2 re-ran rj, which ran seconds ago\n{}", sb.log());
    assert_eq!(sb.acts("rj", "skip").len(), 1, "the skip must be recorded once, not every poll\n{}", sb.log());
    assert_eq!(skip["why"], "recent", "{skip}");
    let detail = skip["detail"].as_str().unwrap_or("");
    assert!(detail.contains("run_at_load pass NOT started") && detail.contains("every_secs = 3600"), "{detail}");
    for name in ["sj", "nj"] {
        assert!(sb.acts(name, "skip").is_empty(), "{name} recorded a skip but was owed a load run\n{}", sb.log());
    }
    // The verb shows it, filterable by its reason.
    let (rc, out) = sb.cauldron(&["supervisions", "--service", "rj", "--why", "recent"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("(1 matching)") && out.contains("skip") && out.contains("run_at_load pass NOT started"), "{out}");
}

/// R5.S7 (H179-1, R5.S6-1 c): a `running` record whose holder died is ASKED,
/// not trusted. Holder 1 starts a 30 s pass of four `every_secs = 3600` jobs
/// and is SIGKILLed mid-pass (each pass is in its own process group, so it
/// outlives the holder). Then, before holder 2:
/// - `aj`: its pass is left running — holder 2 SKIPS it (no second pass);
/// - `dj`: its pass is killed — holder 2 runs it at load and records the
///   unseen end as `ran` / `orphaned` ("no process");
/// - `cj`: its pass is killed and its record's pid is pointed at a live
///   process that started AFTER the pass (a recycled pid) — holder 2 runs it
///   and the `ran` / `orphaned` says "recycled";
/// - `oj`: its pass is killed and its record's pid removed (the old format)
///   — holder 2 keeps R5.S6's skip, though the pass is dead.
/// `dj` and `cj` are the positive controls; `aj`'s and `oj`'s "no run" is
/// asserted only after them and after 4 more polls (poll_secs = 1), the
/// longest period in the path short of the jobs' own 3600 s (doctrine 4).
#[test]
fn r5s7_a_dead_holders_running_record_counts_only_while_its_pass_lives_outlasting_4_polls() {
    let mut sb = Sandbox::new("orphan");
    for name in ["aj", "dj", "cj", "oj"] {
        sb.declare_job(name, "", "every_secs = 3600\nargs = [\"run\"]", "FAKE_SLEEP = \"30\"\n");
        sb.build_and_pin(name);
    }
    let by = |r: &serde_json::Value| r["by"]["pid"].as_u64().unwrap_or(0) as u32;
    let jobs_json = sb.root.join("data").join(&sb.env).join("jobs.json");
    let all = |p: &PathBuf| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap_or_default()).unwrap_or_default()
    };

    sb.start_daemon();
    let h1 = sb.daemon.as_ref().unwrap().id();
    let mut pass = std::collections::BTreeMap::new();
    for name in ["aj", "dj", "cj", "oj"] {
        let r = wait_for(&format!("holder 1's pass of {name} to be running"), budget(), || {
            let r = all(&jobs_json)[name].clone();
            (r["state"] == "running").then_some(r)
        });
        pass.insert(name, (r["pid"].as_u64().expect("a running record names its pid") as u32, r["last_start"].as_u64().unwrap()));
    }
    sb.stop_daemon(); // SIGKILL: the holder dies mid-pass
    for name in ["dj", "cj", "oj"] {
        kill9(pass[name].0);
    }
    std::thread::sleep(Duration::from_secs(3)); // so the decoy starts > 2 s after cj's pass
    let mut decoy = Command::new("/bin/sleep").arg("120").spawn().unwrap();
    let decoy_pid = decoy.id();
    let mut j = all(&jobs_json);
    j["cj"]["pid"] = serde_json::json!(decoy_pid);
    j["oj"].as_object_mut().unwrap().remove("pid");
    std::fs::write(&jobs_json, serde_json::to_string_pretty(&j).unwrap()).unwrap();

    // Sentinels (doctrine 9): the four records are all recent `running`, and
    // differ only in what their pid names.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let j = all(&jobs_json);
    for name in ["aj", "dj", "cj", "oj"] {
        assert_eq!(j[name]["state"], "running", "sentinel: {name}: {}", j[name]);
        assert!(now - j[name]["last_start"].as_u64().unwrap() < 3600, "sentinel: {name} is recent");
    }
    assert!(alive(pass["aj"].0), "sentinel: aj's pass outlived its holder");
    assert!(!alive(pass["dj"].0), "sentinel: dj's pass is dead");
    assert!(alive(decoy_pid) && decoy_pid != pass["cj"].0, "sentinel: cj's record names a live, different process");
    assert!(j["oj"].get("pid").is_none() && !alive(pass["oj"].0), "sentinel: oj has no pid and its pass is dead");
    for name in ["aj", "dj", "cj", "oj"] {
        assert_eq!(sb.acts(name, "run").len(), 1, "sentinel: holder 1 ran {name} once");
        assert!(sb.acts(name, "ran").is_empty() && sb.acts(name, "skip").is_empty(), "sentinel: {name}");
    }

    sb.start_daemon();
    let h2 = sb.daemon.as_ref().unwrap().id();
    assert_ne!(h1, h2);
    let mut rerun_pid = std::collections::BTreeMap::new();
    for (name, word) in [("dj", "no process has pid"), ("cj", "recycled pid")] {
        let rerun = wait_for(&format!("holder 2 to run {name} at load"), budget(), || {
            sb.acts(name, "run").into_iter().find(|r| by(r) == h2)
        });
        // Inspector H-1: the orphan's `failed` record and the re-run's
        // `running` record are written in the SAME poll here; the re-run's
        // must be the one that stays (it is re-checked after the 4-poll wait).
        wait_for(&format!("jobs.json to say {name} is running its re-run"), budget(), || {
            let r = all(&jobs_json)[name].clone();
            (r["state"] == "running" && r["pid"] == rerun["pid"]).then_some(())
        });
        rerun_pid.insert(name, rerun["pid"].clone());
        let o = wait_for(&format!("holder 2 to record {name}'s orphaned pass"), budget(), || {
            sb.acts(name, "ran").into_iter().find(|r| by(r) == h2)
        });
        assert_eq!(o["why"], "orphaned", "{o}");
        assert_eq!(o["pid"].as_u64(), Some(if name == "cj" { decoy_pid } else { pass[name].0 } as u64), "{o}");
        let d = o["detail"].as_str().unwrap_or("");
        assert!(d.contains(word) && d.contains("UNFINISHED"), "{name}: {d}");
    }
    for (name, word) in [("aj", "still running"), ("oj", "names no pid")] {
        let s = wait_for(&format!("holder 2 to record {name}'s skip"), budget(), || {
            sb.acts(name, "skip").into_iter().find(|r| by(r) == h2)
        });
        assert_eq!(s["why"], "recent", "{s}");
        assert!(s["detail"].as_str().unwrap_or("").contains(word), "{name}: {s}");
    }
    std::thread::sleep(Duration::from_secs(4)); // 4 polls at poll_secs = 1
    for name in ["aj", "oj"] {
        assert_eq!(sb.acts(name, "run").len(), 1, "holder 2 ran {name}, whose record it must skip\n{}", sb.log());
        assert!(sb.acts(name, "ran").is_empty(), "{name} was recorded orphaned\n{}", sb.log());
        assert_eq!(sb.acts(name, "skip").len(), 1, "{name}: one skip, not one per poll");
    }
    for name in ["dj", "cj"] {
        assert!(sb.acts(name, "skip").is_empty(), "{name} recorded a skip but was owed a run\n{}", sb.log());
        assert_eq!(sb.acts(name, "ran").len(), 1, "{name}: one orphaned `ran`, not one per poll");
        let r = all(&jobs_json)[name].clone();
        assert!(r["state"] == "running" && r["pid"] == rerun_pid[name], "{name}: the re-run's record was overwritten: {r}");
    }
    assert!(alive(pass["aj"].0), "aj's pass is still the one running");
    let (rc, out) = sb.cauldron(&["supervisions", "--service", "dj", "--why", "orphaned"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("(1 matching)") && out.contains("ran"), "{out}");
    let _ = Command::new("/bin/kill").args(["-9", &pass["aj"].0.to_string()]).status();
    let _ = decoy.kill();
    let _ = decoy.wait();
}

/// R5.S7 inspector F1 / G-1 (test written by the second inspector): the
/// orphaned pass's re-run FAILS TO SPAWN (the built job is not executable).
/// Over 6 polls the orphan is recorded once and jobs.json / status read
/// `failed`, not `running` with a dead pid; restore exec and the one re-run's
/// `running` record wins.

#[test]
fn r5s7_an_orphan_whose_rerun_cannot_spawn_reads_failed_once_then_running_outlasting_6_polls() {
    let mut sb = Sandbox::new("orphspawn");
    sb.declare_job("sj", "", "every_secs = 3600\nargs = [\"run\"]", "FAKE_SLEEP = \"30\"\n");
    sb.build_and_pin("sj");
    let by = |r: &serde_json::Value| r["by"]["pid"].as_u64().unwrap_or(0) as u32;
    let jobs_json = sb.root.join("data").join(&sb.env).join("jobs.json");
    let all = |p: &PathBuf| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap_or_default()).unwrap_or_default()
    };
    sb.start_daemon();
    let r = wait_for("holder 1 pass running", budget(), || {
        let r = all(&jobs_json)["sj"].clone();
        (r["state"] == "running").then_some(r)
    });
    let pass = r["pid"].as_u64().unwrap() as u32;
    sb.stop_daemon();
    kill9(pass);
    // make the job non-executable: the BUILT copy the holder runs (and the source, which `find` also matches)
    let out = Command::new("/usr/bin/find").arg(&sb.root).args(["-name", "fake-job", "-type", "f"]).output().unwrap();
    let paths: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(String::from).collect();
    assert!(paths.iter().any(|p| p.contains("/builds/")), "sentinel: the BUILT fake-job was found: {paths:?}");
    for p in &paths { Command::new("/bin/chmod").arg("-x").arg(p).status().unwrap(); }
    sb.start_daemon();
    let h2 = sb.daemon.as_ref().unwrap().id();
    wait_for("holder 2 orphan row", budget(), || sb.acts("sj", "ran").into_iter().find(|r| by(r) == h2));
    std::thread::sleep(Duration::from_secs(6)); // 6 polls at poll_secs = 1
    let ran = sb.acts("sj", "ran");
    let j = all(&jobs_json)["sj"].clone();
    assert!(sb.acts("sj", "run").iter().all(|r| by(r) != h2), "sentinel: the re-run never started");
    assert!(sb.log().contains("spawning a pass"), "sentinel: spawn failed\n{}", sb.log());
    assert_eq!(ran.len(), 1, "orphan recorded once over 6 polls");
    assert_eq!(j["state"], "failed", "jobs.json after orphan + failed spawn: {j}");
    assert!(j.get("pid").map_or(true, |p| p.is_null()), "{j}");
    let (st, _) = sb.status_state("sj");
    assert_eq!(st, "job:failed", "status");
    // Opposed / normal path: restore exec, the re-run starts and its `running` record wins.
    for p in &paths { Command::new("/bin/chmod").arg("+x").arg(p).status().unwrap(); }
    let run = wait_for("holder 2 re-run starts", budget(), || sb.acts("sj", "run").into_iter().find(|r| by(r) == h2));
    let j = wait_for("jobs.json running with the new pid", budget(), || {
        let r = all(&jobs_json)["sj"].clone();
        (r["state"] == "running" && r["pid"] == run["pid"]).then_some(r)
    });
    std::thread::sleep(Duration::from_secs(3));
    let j2 = all(&jobs_json)["sj"].clone();
    let _ = j;
    assert_eq!(j2["state"], "running", "the failed record overwrote the re-run's running record: {j2}");
    assert_eq!(sb.acts("sj", "ran").len(), 1, "no second orphan row after the re-run");
    assert_eq!(sb.acts("sj", "run").iter().filter(|r| by(r) == h2).count(), 1, "one re-run");
    let _ = Command::new("/bin/kill").args(["-9", &run["pid"].as_u64().unwrap().to_string()]).status();
}

#[test]
fn a_signed_bundle_is_installed_whole_with_its_seal_intact() {
    let sb = Sandbox::new("seal");
    let app = sb.root.join("Fake.app");
    std::fs::write(
        app.join("Contents").join("Info.plist"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\
         <key>CFBundleExecutable</key><string>fake</string>\
         <key>CFBundleIdentifier</key><string>test.r5s3.fake</string></dict></plist>\n",
    )
    .unwrap();
    let signed = Command::new("/usr/bin/codesign").args(["-s", "-", "--force"]).arg(&app).status().unwrap();
    assert!(signed.success(), "premise: the fake bundle can be ad-hoc signed");
    let verify = |p: &std::path::Path| {
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict"])
            .arg(p)
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(verify(&app), "premise: the source bundle verifies");
    sb.declare_daemon("ui", "/health", "");
    let (rc, out) = sb.cauldron(&["build", "ui"]);
    assert_eq!(rc, 0, "build of a signed bundle: {out}");
    let builds = sb.root.join("data").join("shared").join("builds").join("ui");
    let label = std::fs::read_dir(&builds).unwrap().next().unwrap().unwrap().path();
    let installed = label.join("Fake.app");
    assert!(verify(&installed), "the INSTALLED bundle must verify: {}", installed.display());
}

/// R5.S5 (H179-1, Deploy5-2): `status` prints a CALENDAR job's next slot —
/// in `--json` as `next_slot {unix, utc, local, due}` and in the text detail —
/// and an INTERVAL job's `next_slot` is `null`. No holder runs: the slot is
/// read off the record exactly as the holder's first look reads it.
///
/// JOBS-DST: replaces CLI-ISO's `/bin/date` arithmetic, which read the
/// current date on every call and was itself wrong across both transitions.
/// Every `status` here runs on an INJECTED clock — `CAULDRON_TEST_NOW` (the
/// isolated process's wall clock) and `TZ` — at fixed instants on the two
/// 2026 US transition nights, and every expected value is a literal (from
/// Python's `zoneinfo` when the test was written). Nothing here reads the
/// real clock or the machine's zone. The pre-JOBS-DST arithmetic gives the
/// value in each row's `old` note; the fix gives `unix`.
#[test]
fn status_prints_a_calendar_jobs_next_slot_in_utc_and_local() {
    let sb = Sandbox::new("nextslot");
    let jobs_json = sb.root.join("data").join(&sb.env).join("jobs.json");
    for (name, at) in [("cj", "03:15"), ("dj", "23:00"), ("gj", "02:30")] {
        sb.declare_job(name, "", &format!("at = \"{at}\"\nargs = [\"run\"]\nrun_at_load = false"), "");
        sb.build_and_pin(name);
    }
    sb.declare_job("ij", "", "every_secs = 3600\nargs = [\"run\"]\nrun_at_load = false", "");
    sb.build_and_pin("ij");
    let row = |name: &str, now: u64, tz: &str| -> serde_json::Value {
        let now = now.to_string();
        let (rc, out) = sb.cauldron_with(&["status", name, "--json"], &[("CAULDRON_TEST_NOW", &now), ("TZ", tz)]);
        assert_eq!(rc, 0, "{out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        v["services"].as_array().unwrap().iter().find(|r| r["service"] == name).cloned().unwrap()
    };
    const LA: &str = "America/Los_Angeles";

    // (job, injected now, TZ, the record's last_slot (None: never run),
    //  expected unix, utc, local, due, what it shows)
    #[rustfmt::skip]
    let cases: [(&str, u64, &str, Option<u64>, u64, &str, &str, bool, &str); 6] = [
        // now 2026-11-01T08:30Z = 01:30 PDT, the FIRST 01:30 of the fall-back night.
        ("cj", 1_793_521_800, LA, None, 1_793_531_700, "2026-11-01T11:15:00Z", "2026-11-01T03:15:00-08:00 PST", false,
         "never run, 01:30 PDT: the upcoming 03:15 is PST (old: 10:15Z, now + 1 h 45 m of wall clock)"),
        // now 2026-11-01T09:30Z = 01:30 PST, the SECOND 01:30.
        ("cj", 1_793_525_400, LA, None, 1_793_531_700, "2026-11-01T11:15:00Z", "2026-11-01T03:15:00-08:00 PST", false,
         "never run, 01:30 PST: the same 03:15 PST"),
        // now 2026-11-02T06:00Z = Nov 1 22:00 PST; last slot Oct 28 23:00 PDT.
        ("dj", 1_793_599_200, LA, Some(1_793_253_600), 1_793_512_800, "2026-11-01T06:00:00Z", "2026-10-31T23:00:00-07:00 PDT", true,
         "three slots missed, 22:00 PST: due at yesterday's 23:00 PDT (old: 07:00Z, now - 23 h)"),
        // now 2026-03-09T05:00Z = Mar 8 22:00 PDT; last slot Mar 5 23:00 PST.
        ("dj", 1_773_032_400, LA, Some(1_772_780_400), 1_772_953_200, "2026-03-08T07:00:00Z", "2026-03-07T23:00:00-08:00 PST", true,
         "three slots missed, 22:00 PDT: due at yesterday's 23:00 PST (old: 06:00Z, now - 23 h)"),
        // now 2026-03-08T09:30Z = 01:30 PST, half an hour before the gap.
        ("gj", 1_772_962_200, LA, None, 1_772_965_800, "2026-03-08T10:30:00Z", "2026-03-08T03:30:00-07:00 PDT", false,
         "never run: a NONEXISTENT 02:30 runs at 03:30 PDT, moved forward by the gap"),
        // A zone with no DST, the same instant as the first row.
        ("cj", 1_793_521_800, "America/Phoenix", None, 1_793_528_100, "2026-11-01T10:15:00Z", "2026-11-01T03:15:00-07:00 MST", false,
         "no DST: 01:30 MST, the 03:15 MST slot"),
    ];
    for (name, now, tz, last, unix, utc, local, due, what) in cases {
        match last {
            None => {
                let _ = std::fs::remove_file(&jobs_json);
            }
            Some(l) => std::fs::write(
                &jobs_json,
                format!("{{\"{name}\": {{\"state\": \"ok\", \"version\": \"v\", \"detail\": \"a pass\", \"last_slot\": {l}}}}}"),
            )
            .unwrap(),
        }
        let r = row(name, now, tz);
        let n = &r["next_slot"];
        assert_eq!(n["unix"].as_u64(), Some(unix), "{what}: {r}");
        assert_eq!(n["utc"].as_str(), Some(utc), "{what}: {r}");
        assert_eq!(n["local"].as_str(), Some(local), "{what}: {r}");
        assert_eq!(n["due"], due, "{what}: {r}");
        assert_eq!(r["detail"].as_str().unwrap().contains("DUE NOW"), due, "{what}: {r}");
    }
    // Sentinel: the injected clock is what `status` read — the same job and
    // record a day later (2026-11-02T08:30Z) gives the NEXT day's slot.
    let _ = std::fs::remove_file(&jobs_json);
    let r = row("cj", 1_793_521_800 + 86_400, LA);
    assert_eq!(r["next_slot"]["unix"].as_u64(), Some(1_793_531_700 + 86_400), "the injected now was not read: {r}");
    // The text status carries the same slot.
    let (rc, out) = sb.cauldron_with(&["status", "cj"], &[("CAULDRON_TEST_NOW", "1793521800"), ("TZ", LA)]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("next slot 2026-11-01T11:15:00Z"), "text status lacks the next slot:\n{out}");

    // An interval job has no slot.
    let r = row("ij", 1_793_521_800, LA);
    assert!(r["next_slot"].is_null(), "an interval job printed a next slot: {r}");
    assert!(!r["detail"].as_str().unwrap().contains("next slot"), "{r}");
}
