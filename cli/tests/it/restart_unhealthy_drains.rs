//! **R9.C7e (gap C-7) — `cauldron restart` ON AN UNHEALTHY (503) SERVICE
//! DRAINS INSTEAD OF KILLING ITS TURNS.**
//!
//! R9.C7d routed `cauldron restart` through the drain, but only for a LIVE
//! service: `Ask::Restart` read the row's probe state and anything but `live`
//! or `live:unverified` restarted NOW. Threads answers `/health` 503 on a
//! stale store and still carries its restart gate in that body, so the one
//! moment the operator is most likely to type `cauldron restart threads` — it
//! looks wedged — was the moment every running turn was SIGTERMed. Now an
//! unhealthy service is asked to drain too, and (as the C6 503 relaunch does,
//! R9.C7d-1) it is relaunched at the drain cap whatever `at_cap` says.
//!
//! Every pass after the request finds the service NOT live, so the restart is
//! finished in `tend`'s not-live branch, before `down_after`, the
//! `auto_restart = false` early return and the backoff can swallow it. These
//! tests run with `auto_restart = false` ON PURPOSE: the supervisor's own 503
//! relaunch (C7d) then never acts, so whatever drains and restarts the
//! service is the REQUEST and nothing else.
//!
//! Same throwaway-sandbox fake as `drain_pin.rs` (copied, not shared, so the
//! two files can change independently): a hosted python daemon with threads'
//! drain door, turns in their own sessions, `wedged` naming a pid makes that
//! pid answer 503. THE LONGEST PERIOD on this path is the pass, `poll_secs` =
//! 1; every "not restarted" is asserted after the daemon logged its decision
//! and again after `HOLD` = 6 s. The cap is injected short (3 s) where named.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

const FAKE: &str = r#"#!/usr/bin/env python3
# R9.C7c fake: a hosted daemon with threads' drain door. Turns are processes in
# their own sessions, so they outlive this process the way a threads turn does.
import os, signal, socket, subprocess, sys, threading, time, json
root = os.environ["FAKE_ROOT"]
def P(n): return os.path.join(root, n)
def note(name, line):
    with open(P(name), "a") as f: f.write(line + "\n")
def owner_watch(owner):
    while True:
        time.sleep(1)
        try: os.kill(owner, 0)
        except ProcessLookupError: os._exit(0)
        except PermissionError: pass
threading.Thread(target=owner_watch, args=(int(os.environ["FAKE_OWNER"]),), daemon=True).start()
signal.signal(signal.SIGCHLD, signal.SIG_IGN)  # turns are reaped as they end
note("fake-pids", str(os.getpid()))
door = not os.path.exists(P("nodoor"))
state = {"since": None}
def on_usr1(*_):
    if state["since"] is None: state["since"] = int(time.time())
    note("drain-log", "begin %d" % os.getpid())
def on_usr2(*_):
    state["since"] = None
    note("drain-log", "cancel %d" % os.getpid())
if door:
    signal.signal(signal.SIGUSR1, on_usr1)
    signal.signal(signal.SIGUSR2, on_usr2)
def turn_pids():
    try: return [int(x) for x in open(P("turn-pids")).read().split()]
    except FileNotFoundError: return []
def alive(p):
    try: os.kill(p, 0); return True
    except ProcessLookupError: return False
    except PermissionError: return True
def running(): return sum(1 for p in turn_pids() if alive(p))
def start_turn(script):
    t = subprocess.Popen(["/bin/sh", "-c", script, "fake-turn", root], start_new_session=True)
    note("turn-pids", str(t.pid)); note("turn-starts", str(t.pid))
HELD = 'while [ -f "$1/hold" ]; do sleep 0.1; done; echo "ok $$" >> "$1/done"'
SHORT = 'sleep 1.2; echo "ok $$" >> "$1/done"'
def starter():
    while True:
        time.sleep(0.1)
        if state["since"] is not None: continue   # draining: start nothing, queue stays
        if os.path.exists(P("want-held")):
            os.remove(P("want-held")); start_turn(HELD)
        if os.path.exists(P("continuous")):
            start_turn(SHORT); time.sleep(0.3)
        # R9.C7b: a prompt that ARRIVED (here, while draining) starts once this
        # instance admits turns, and says which instance ran it.
        if os.path.exists(P("queued")):
            os.remove(P("queued")); start_turn('echo "intake %d" >> "$1/intake"' % os.getpid())
threading.Thread(target=starter, daemon=True).start()
def on_term(*_):
    note("kills", "%d running=%d" % (os.getpid(), running()))
    os._exit(0)
signal.signal(signal.SIGTERM, on_term)
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
        n = running()
        gate = {"idle": n == 0, "turns": n,
                "turn_threads": ["turn-%d" % p for p in turn_pids() if alive(p)]}
        if door: gate["draining_since"] = state["since"]
        note("gate-reads", "%d %d" % (os.getpid(), n))
        # R9.C7d: `wedged` naming this pid makes it answer 503, as threads does
        # on a stale store, still carrying its restart gate. A relaunched
        # instance has another pid, so it answers 200 again.
        wedged = open(P("wedged")).read().strip() == str(os.getpid()) if os.path.exists(P("wedged")) else False
        status = b"503 Service Unavailable" if wedged else b"200 OK"
        body = json.dumps({"status": "wedged" if wedged else "ok", "restart_gate": gate}).encode()
        c.sendall(b"HTTP/1.0 " + status + b"\r\nContent-Type: application/json\r\n\r\n" + body)
    except Exception:
        pass
    c.close()
"#;

const POLICY: &str = "auto_restart = false\nauto_rollback = false\npoll_secs = 1\ndown_after = 3\n\
                      max_start_failures = 3\nbackoff_base_secs = 1\nbackoff_max_secs = 5\n\
                      stable_after_secs = 600";
const BUDGET: Duration = Duration::from_secs(120);
/// Six `poll_secs = 1` passes, and more than production's default 4 s pass.
const HOLD: Duration = Duration::from_secs(6);
/// The injected cap (doctrine 4): short enough to fire inside a test.
const CAP_SECS: u64 = 3;

fn drain(cap: u64, at_cap: &str) -> String {
    format!(
        "[quiet]\npath = \"/gate\"\npointer = \"/restart_gate/idle\"\n\
         [quiet.drain]\nstart = \"USR1\"\ncancel = \"USR2\"\nsince = \"/restart_gate/draining_since\"\n\
         running = \"/restart_gate/turns\"\ncap_secs = {cap}\nat_cap = \"{at_cap}\"\n"
    )
}

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
        for f in ["fake-pids", "turn-pids"] {
            if let Ok(t) = std::fs::read_to_string(self.root.join(f)) {
                for p in t.lines() {
                    let _ = Command::new("/bin/kill").args(["-9", p.trim()]).stderr(Stdio::null()).status();
                }
            }
        }
        let survivors = reap_fixtures(&self.root);
        if std::env::var_os("KEEP_SANDBOX").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
        if !survivors.is_empty() {
            let msg = format!("fixture(s) of {} survived their test: {survivors:?}", self.root.display());
            if std::thread::panicking() {
                eprintln!("{msg}");
            } else {
                panic!("{msg}");
            }
        }
    }
}

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
            "cauldron-c7e-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("c7e{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::create_dir_all(root.join("data").join(&env)).unwrap();
        std::fs::write(root.join("data").join(&env).join("env.toml"), format!("[supervisor]\n{POLICY}\n")).unwrap();
        std::fs::write(root.join("fake"), launch_wait::slow_launch_py(FAKE, &root.join("f2g-marks"))).unwrap();
        Command::new("/bin/chmod").arg("+x").arg(root.join("fake")).status().unwrap();
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
        (
            out.status.code().unwrap_or(-1),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    }

    /// Hosted, `probe = "http"` against the fake's `/gate`, so a 503 from it
    /// reads UNHEALTHY, the way threads' `/health` does.
    fn declare(&self, name: &str, quiet: &str) -> u16 {
        let port = free_port();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"http\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\nhealth = \"http://127.0.0.1:{port}/gate\"\n\
                 [env]\nFAKE_PORT = \"{port}\"\nFAKE_ROOT = \"{}\"\nFAKE_OWNER = \"{}\"\n\
                 {quiet}\n\
                 [build]\nartifact = \"{}\"\nbin = \"fake\"\n",
                self.root.display(),
                std::process::id(),
                self.root.join("fake").display()
            ),
        )
        .unwrap();
        port
    }

    fn build(&self, name: &str) -> String {
        let (rc, out) = self.cauldron(&["build", name]);
        assert_eq!(rc, 0, "build {name}: {out}");
        let (rc, out) = self.cauldron(&["versions", name, "--json"]);
        assert_eq!(rc, 0, "versions {name}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        v[0]["label"].as_str().expect("a label").to_string()
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

    fn file(&self, f: &str) -> String {
        std::fs::read_to_string(self.root.join(f)).unwrap_or_default()
    }

    fn seam(&self, name: &str) -> serde_json::Value {
        let t = std::fs::read_to_string(self.root.join("data").join(&self.env).join("seams.json")).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&t).ok().map(|v| v["seams"][name].clone()).unwrap_or_default()
    }

    fn up(&self, name: &str, port: u16) -> Option<(u32, String)> {
        let s = self.seam(name);
        let pid = s["pid"].as_u64()? as u32;
        let ver = s["version"].as_str()?.to_string();
        (alive(pid) && listening(port)).then_some((pid, ver))
    }

    fn touch(&self, f: &str) {
        std::fs::write(self.root.join(f), "").unwrap();
    }

    fn rm(&self, f: &str) {
        let _ = std::fs::remove_file(self.root.join(f));
    }

    fn launches_why(&self, name: &str, why: &str) -> usize {
        std::fs::read_to_string(self.root.join("data").join(&self.env).join("supervisions.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|r| r["service"] == name && r["act"] == "launch" && r["why"] == why)
            .count()
    }

    fn drain_begins(&self) -> usize {
        self.file("drain-log").lines().filter(|l| l.starts_with("begin")).count()
    }

    /// The probe's own word for the row, through `status --json`.
    fn state(&self, name: &str) -> String {
        let (_, out) = self.cauldron(&["status", "--json"]);
        let v: serde_json::Value = match serde_json::from_str(&out) {
            Ok(v) => v,
            Err(_) => return format!("unparsed: {out}"),
        };
        let find = |arr: &serde_json::Value| -> Option<String> {
            arr.as_array()?
                .iter()
                .find(|s| s["service"] == name)
                .and_then(|s| s["state"].as_str().map(str::to_string))
        };
        find(&v["services"]).or_else(|| find(&v)).unwrap_or_else(|| format!("absent: {out}"))
    }

    /// v1 up under the daemon, a held turn running on it, then THAT pid made
    /// to answer 503 — and the probe seen to read it `unhealthy`. The premise
    /// every test below stands on, asserted. Returns (port, pid, turn pid).
    fn wedged_with_a_turn(&mut self, name: &str, quiet: &str, turn: bool) -> (u16, u32, Option<u32>) {
        let port = self.declare(name, quiet);
        let v1 = self.build(name);
        let (rc, out) = self.cauldron(&["pin", name, &v1]);
        assert_eq!(rc, 0, "pin: {out}");
        self.start_daemon();
        let (pid, _) = wait_for("v1 up under the daemon", BUDGET, || self.up(name, port));
        let t = turn.then(|| {
            self.touch("hold");
            self.touch("want-held");
            wait_for("the held turn started", BUDGET, || {
                self.file("turn-starts").lines().next().and_then(|l| l.trim().parse::<u32>().ok())
            })
        });
        std::fs::write(self.root.join("wedged"), pid.to_string()).unwrap();
        wait_for("the probe to read the service unhealthy", BUDGET, || {
            (self.state(name) == "unhealthy").then_some(())
        });
        // auto_restart = false: the supervisor's own 503 relaunch stands down,
        // and says so, before the request is made.
        wait_for("the daemon to stand down on its own relaunch", BUDGET, || {
            self.log().contains("`auto_restart = false` here").then_some(())
        });
        assert!(self.file("kills").is_empty(), "premise: nothing stopped it yet: {}", self.file("kills"));
        assert_eq!(self.drain_begins(), 0, "premise: nothing asked it to drain yet");
        (port, pid, t)
    }

    fn restart(&self, name: &str) -> String {
        let (rc, out) = self.cauldron(&["restart", name]);
        assert_eq!(rc, 0, "restart {name}: {out}");
        out
    }
}

/// THE CLAIM. `cauldron restart` on a 503 service with a turn running asks it
/// to drain and restarts it only when the turn has ended on its own. At base
/// (32bef989) `Ask::Restart` read `unhealthy` as "not live", restarted NOW,
/// and the fake's own `kills` line said `running=1`.
#[test]
fn c7e_restart_of_a_503_service_with_a_turn_running_drains_and_waits_for_it_outlasting_6_polls() {
    let mut sb = Sandbox::new("wait");
    let (port, pid1, turn) = sb.wedged_with_a_turn("u", &drain(1800, "handoff"), true);
    let turn = turn.unwrap();
    sb.restart("u");
    wait_for("the daemon to ask u to drain for the restart", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    std::thread::sleep(HOLD);
    assert!(alive(pid1), "restarted while a turn ran\n{}", sb.log());
    assert!(sb.file("kills").is_empty(), "stopped mid-turn: {}\n{}", sb.file("kills"), sb.log());
    assert_eq!(sb.launches_why("u", "requested"), 0, "{}", sb.log());
    assert!(alive(turn), "the turn was touched");

    sb.rm("hold");
    let (pid2, _) = wait_for("u restarted once the turn ended", BUDGET, || {
        sb.up("u", port).filter(|(p, _)| *p != pid1)
    });
    assert_ne!(pid2, pid1);
    assert_eq!(sb.file("done").lines().count(), 1, "the turn ended on its own, once: {}", sb.file("done"));
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=0"), "stopped with a turn running");
    wait_for("the restart's `launch` record", BUDGET, || (sb.launches_why("u", "requested") > 0).then_some(()));
    assert_eq!(sb.launches_why("u", "requested"), 1, "{}", sb.log());
}

/// Paired (doctrine 2): with NO turn running, the same request on the same
/// 503 service restarts it promptly and never asks it to drain — so "always
/// wait" fails this one, as "never wait" fails the one above.
#[test]
fn c7e_restart_of_a_503_service_with_no_turn_running_is_immediate_and_never_drains() {
    let mut sb = Sandbox::new("idle");
    let (port, pid1, _) = sb.wedged_with_a_turn("i", &drain(1800, "handoff"), false);
    let t0 = Instant::now();
    sb.restart("i");
    wait_for("i restarted", BUDGET, || sb.up("i", port).filter(|(p, _)| *p != pid1));
    assert!(t0.elapsed() < Duration::from_secs(30), "an idle restart took {:?}", t0.elapsed());
    assert_eq!(sb.drain_begins(), 0, "an idle service was asked to drain: {}", sb.file("drain-log"));
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=0"));
    wait_for("the restart's `launch` record", BUDGET, || (sb.launches_why("i", "requested") > 0).then_some(()));
    assert_eq!(sb.launches_why("i", "requested"), 1, "{}", sb.log());
}

/// Consistent with C7d (R9.C7d-1): an UNHEALTHY service is relaunched at the
/// drain cap whatever `at_cap` says. Under `extend` and a turn that never
/// ends, the requested restart still happens at the injected 3 s cap — not
/// before it — and the running turn is handed off, not killed.
#[test]
fn c7e_restart_of_a_503_service_goes_ahead_at_the_3s_cap_even_under_extend() {
    let mut sb = Sandbox::new("cap");
    let (port, pid1, turn) = sb.wedged_with_a_turn("c", &drain(CAP_SECS, "extend"), true);
    let turn = turn.unwrap();
    sb.restart("c");
    wait_for("asked to drain", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    let asked = Instant::now();
    wait_for("c relaunched at the cap", BUDGET, || sb.up("c", port).filter(|(p, _)| *p != pid1));
    assert!(asked.elapsed() >= Duration::from_secs(CAP_SECS - 1), "fired early: {:?}", asked.elapsed());
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=1"), "premise: handed off with the turn running");
    assert!(alive(turn), "the handed-off turn was killed");
    assert!(sb.log().contains("It is UNHEALTHY, so it is relaunched now"), "{}", sb.log());
    wait_for("the restart's `launch` record", BUDGET, || (sb.launches_why("c", "requested") > 0).then_some(()));
    assert_eq!(sb.launches_why("c", "requested"), 1, "{}", sb.log());
    sb.rm("hold");
}
