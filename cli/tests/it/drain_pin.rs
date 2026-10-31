//! **R9.C7c (gap C-7) — A PIN WHILE TURNS EXECUTE IS SAFE: THE SERVICE DRAINS,
//! THEN IS RESTARTED ONTO THE NEW BUILD, AND NO TURN IS LOST OR RE-BOUGHT.**
//! The cauldron half.
//!
//! R9.C7 made a re-pin wait for a quiet moment. Prod threads never has one —
//! no idle window from 23:35Z to 03:38Z, long turns handing off back to back
//! (an internal report §C) — so that wait was forever. Now a
//! service that declares `[quiet.drain]` is ASKED TO DRAIN: it starts no new
//! turn and queues new prompts, its running turns finish, and the restart
//! follows; or, at the declared cap, the turns still running are handed off.
//!
//! Every test runs a THROWAWAY cauldron (its own `CAULDRON_HOME` under the
//! system temp dir, kernel-assigned ports). Nothing touches `~/.cauldron`, a
//! live daemon, or launchd. The service is a python fake hosted the way
//! threads is, speaking threads' drain door exactly: `SIGUSR1` begins a drain,
//! `SIGUSR2` cancels it, and its gate answers `restart_gate.{idle, turns,
//! draining_since}`. Its TURNS are real processes in their own sessions (as a
//! threads turn's `claude` leads its own process group), so a turn outlives
//! the fake exactly as a turn outlives threads, and the next instance of the
//! fake counts the survivors from `turn-pids` the way threads re-adopts from
//! `running_turns`. The real binary's half — that a threads turn really does
//! survive and is re-adopted — is `services/threads/tests/drain_pin.rs`.
//!
//! THE LONGEST PERIOD ON THIS PATH (doctrine 4) is the supervisor's pass,
//! `poll_secs` = 1 here: the gate is re-read once per pass and nothing else
//! re-checks it. Every "not restarted" is asserted after the daemon LOGGED its
//! decision and then again after `HOLD` = 6 s (six passes, longer than prod's
//! 4 s). The cap is injected SHORT (`cap_secs = 3`) and named in test names.
//!
//! Claims, each paired:
//! (a) a pin while a turn executes does not restart until that turn ends, and
//!     the turn ends on its own ("ok", not killed). Paired: an idle service is
//!     restarted on the next pass and is never asked to drain.
//! (c) the cap fires after `cap_secs`, not before, and hands the running turn
//!     off: the turn is not killed, keeps running under the new instance, and
//!     ends once. Paired: `at_cap = "extend"` does not restart past the cap.
//! (d) a pin back to the running build cancels the drain: no restart, and
//!     queued work starts.
//! (e) doctrine 10 — THE PROD POPULATION: a service that is NEVER idle
//!     (continuous back-to-back turns). The pin completes. At base this defers
//!     forever, so this test is red there.
//! plus: a build that declares a drain but whose process does NOT serve the
//!     door is never signalled (SIGUSR1's default is to terminate), and who
//!     declares a drain today, read off the shipped files.

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

const POLICY: &str = "auto_restart = true\nauto_rollback = false\npoll_secs = 1\ndown_after = 3\n\
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
            "cauldron-c7c-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("c7c{tag}");
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
            .output()
            .expect("spawn cauldron");
        (
            out.status.code().unwrap_or(-1),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    }

    fn declare(&self, name: &str, quiet: &str) -> u16 {
        self.declare_probe(name, quiet, false)
    }

    /// `http = true`: `probe = "http"` against the fake's `/gate`, so a 503
    /// from it reads UNHEALTHY (R9.C7d), the way threads' `/health` does.
    fn declare_probe(&self, name: &str, quiet: &str, http: bool) -> u16 {
        let port = free_port();
        let probe = if http {
            "probe = \"http\"\n".to_string()
        } else {
            "probe = \"connect\"\n".to_string()
        };
        let health = if http { format!("health = \"http://127.0.0.1:{port}/gate\"\n") } else { String::new() };
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\n{probe}\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\n{health}\
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

    fn new_bytes(&self, tag: &str) {
        let p = self.root.join("fake");
        let t = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, format!("{t}# {tag}\n")).unwrap();
    }

    fn pin(&self, name: &str, label: &str) -> String {
        let (rc, out) = self.cauldron(&["pin", name, label]);
        assert_eq!(rc, 0, "pin {name} {label}: {out}");
        out
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

    fn repin_launches(&self, name: &str) -> usize {
        std::fs::read_to_string(self.root.join("data").join(&self.env).join("supervisions.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|r| r["service"] == name && r["act"] == "launch" && r["why"] == "repin")
            .count()
    }

    fn touch(&self, f: &str) {
        std::fs::write(self.root.join(f), "").unwrap();
    }

    fn rm(&self, f: &str) {
        let _ = std::fs::remove_file(self.root.join(f));
    }

    fn up_on_v1(&mut self, name: &str, quiet: &str) -> (u16, String, u32) {
        let port = self.declare(name, quiet);
        let v1 = self.build(name);
        self.pin(name, &v1);
        self.start_daemon();
        let (pid, ver) = wait_for("v1 up under the daemon", BUDGET, || self.up(name, port));
        assert_eq!(ver, v1);
        (port, v1, pid)
    }

    /// A held turn running on the fake (its own session; ends on `rm hold`).
    fn hold_a_turn(&self) -> u32 {
        self.touch("hold");
        self.touch("want-held");
        wait_for("the held turn started", BUDGET, || {
            self.file("turn-starts").lines().next().and_then(|l| l.trim().parse().ok())
        })
    }

    /// A second label for `name`.
    fn v2(&self, name: &str, v1: &str) -> String {
        self.new_bytes("v2");
        let v2 = self.build(name);
        assert_ne!(v2, v1, "sentinel: a second label");
        v2
    }

    fn drain_begins(&self) -> usize {
        self.file("drain-log").lines().filter(|l| l.starts_with("begin")).count()
    }

    fn status(&self) -> String {
        self.cauldron(&["status"]).1
    }
}

// ═══════════════════════════════════════════════════════════════ (a) ════

#[test]
fn c7c_a_a_pin_while_a_turn_executes_drains_and_waits_for_it_outlasting_6_polls() {
    let mut sb = Sandbox::new("a");
    let (port, v1, pid1) = sb.up_on_v1("t", &drain(1800, "handoff"));
    let turn = sb.hold_a_turn();
    let v2 = sb.v2("t", &v1);
    let said = sb.pin("t", &v2);
    assert!(said.contains("WITHOUT INTERRUPTING") && said.contains("DRAIN"), "{said}");

    wait_for("the daemon to ask t to drain", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    assert!(sb.log().contains("Asked it to DRAIN"), "{}", sb.log());
    std::thread::sleep(HOLD);
    assert_eq!(sb.up("t", port), Some((pid1, v1.clone())), "restarted while a turn ran\n{}", sb.log());
    assert_eq!(sb.repin_launches("t"), 0, "{}", sb.log());
    assert!(alive(turn), "the turn was touched");
    // C-7 visibility: status says what the re-pin is waiting on.
    let st = sb.status();
    assert!(st.contains("t: draining since ") && st.contains("1 running, cap at "), "{st}");

    sb.rm("hold");
    let (pid2, ver2) = wait_for("t restarted onto v2 once the turn ended", BUDGET, || {
        sb.up("t", port).filter(|(p, _)| *p != pid1)
    });
    assert_eq!(ver2, v2);
    assert_ne!(pid2, pid1);
    assert_eq!(sb.file("done").lines().count(), 1, "the turn ended on its own, once: {}", sb.file("done"));
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=0"), "stopped with a turn running");
    assert!(sb.log().contains("has DRAINED"), "{}", sb.log());
}

/// Paired with (a): an idle service is restarted on the next pass, and is
/// never asked to drain.
#[test]
fn c7c_a_an_idle_service_is_restarted_promptly_and_never_asked_to_drain() {
    let mut sb = Sandbox::new("idle");
    let (port, v1, pid1) = sb.up_on_v1("i", &drain(1800, "handoff"));
    let v2 = sb.v2("i", &v1);
    let t0 = Instant::now();
    sb.pin("i", &v2);
    let (_, ver) = wait_for("i restarted onto v2", BUDGET, || sb.up("i", port).filter(|(p, _)| *p != pid1));
    assert_eq!(ver, v2);
    assert!(t0.elapsed() < Duration::from_secs(30), "an idle restart took {:?}", t0.elapsed());
    assert_eq!(sb.drain_begins(), 0, "an idle service was asked to drain: {}", sb.file("drain-log"));
}

// ═══════════════════════════════════════════════════════════════ (c) ════

#[test]
fn c7c_c_the_3s_cap_fires_and_hands_off_the_turn_still_running_without_killing_it() {
    let mut sb = Sandbox::new("cap");
    let (port, v1, pid1) = sb.up_on_v1("c", &drain(CAP_SECS, "handoff"));
    let turn = sb.hold_a_turn();
    let v2 = sb.v2("c", &v1);
    sb.pin("c", &v2);
    wait_for("asked to drain", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    let asked = Instant::now();
    let (pid2, ver) = wait_for("c restarted at the cap", BUDGET, || sb.up("c", port).filter(|(p, _)| *p != pid1));
    // Not before the cap: the service's own `since` is whole seconds, so the
    // earliest the cap can fire is CAP_SECS - 1 s after the request.
    assert!(asked.elapsed() >= Duration::from_secs(CAP_SECS - 1), "fired early: {:?}", asked.elapsed());
    assert_eq!(ver, v2);
    assert!(sb.log().contains("reached its drain cap") && sb.log().contains("HANDING THOSE TURNS OFF"), "{}", sb.log());
    // Handed off: the turn was running at the stop, was not killed, and the
    // new instance counts it (its gate reads 1 running).
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=1"));
    assert!(alive(turn), "the handed-off turn was killed");
    let _ = pid2;
    assert_eq!(http_gate(port)["restart_gate"]["turns"], 1, "the new instance does not count the handed-off turn");
    // And it ends once, by itself — not re-run.
    sb.rm("hold");
    wait_for("the handed-off turn to end", BUDGET, || (!alive(turn)).then_some(()));
    assert_eq!(sb.file("done").lines().count(), 1, "{}", sb.file("done"));
    assert_eq!(sb.file("turn-starts").lines().count(), 1, "re-run: {}", sb.file("turn-starts"));
    assert_eq!(sb.repin_launches("c"), 1, "{}", sb.log());
}

/// Paired with (c): `at_cap = "extend"` does NOT restart past the cap, and
/// the restart follows when the turn ends.
#[test]
fn c7c_c_at_cap_extend_does_not_restart_past_the_3s_cap_outlasting_6_polls() {
    let mut sb = Sandbox::new("ext");
    let (port, v1, pid1) = sb.up_on_v1("x", &drain(CAP_SECS, "extend"));
    let turn = sb.hold_a_turn();
    let v2 = sb.v2("x", &v1);
    sb.pin("x", &v2);
    wait_for("the daemon to say it is extending", BUDGET, || sb.log().contains("at_cap = \"extend\"").then_some(()));
    std::thread::sleep(HOLD);
    assert_eq!(sb.up("x", port), Some((pid1, v1.clone())), "restarted past an extend cap\n{}", sb.log());
    assert!(alive(turn));
    // R9.C7d / F-A: the at-cap warning no longer offers `cauldron restart` as
    // "now regardless"; it names the one path that ends the wait and its cost.
    assert!(!sb.log().contains("regardless") && sb.log().contains("SIGPIPE"), "{}", sb.log());
    sb.rm("hold");
    let (_, ver) = wait_for("x restarted once the turn ended", BUDGET, || sb.up("x", port).filter(|(p, _)| *p != pid1));
    assert_eq!(ver, v2);
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=0"));
}

// ═══════════════════════════════════════════════════════════════ (d) ════

#[test]
fn c7c_d_a_pin_back_to_the_running_build_cancels_the_drain_and_restarts_nothing() {
    let mut sb = Sandbox::new("back");
    let (port, v1, pid1) = sb.up_on_v1("b", &drain(1800, "handoff"));
    sb.hold_a_turn();
    let v2 = sb.v2("b", &v1);
    sb.pin("b", &v2);
    wait_for("asked to drain", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    // Work queued during the drain.
    sb.touch("want-held");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(sb.file("turn-starts").lines().count(), 1, "sentinel: nothing new started while draining");

    let said = sb.pin("b", &v1);
    assert!(said.contains("DRAINING since") && said.contains("cancels that drain"), "{said}");
    wait_for("the daemon to cancel the drain", BUDGET, || sb.file("drain-log").contains("cancel").then_some(()));
    wait_for("the queued work to start", BUDGET, || (sb.file("turn-starts").lines().count() == 2).then_some(()));
    assert!(sb.log().contains("Cancelled the drain"), "{}", sb.log());
    std::thread::sleep(HOLD);
    assert_eq!(sb.up("b", port), Some((pid1, v1)), "restarted on a pin back\n{}", sb.log());
    assert_eq!(sb.repin_launches("b"), 0);
    sb.rm("hold");
}

// ═══════════════════════════════════════════════════════════════ (e) ════

/// **Doctrine 10, the prod population: a service that is NEVER idle.** Turns
/// start back to back and overlap, so the running set never reaches zero on
/// its own (sampled below as a sentinel). Before R9.C7c the re-pin waited for
/// idle and so waited forever; now it completes, with no turn killed.
#[test]
fn c7c_e_a_pin_completes_under_continuous_back_to_back_turns() {
    let mut sb = Sandbox::new("busy");
    let (port, v1, pid1) = sb.up_on_v1("p", &drain(1800, "handoff"));
    sb.touch("continuous");
    // Sentinel: this service is never idle on its own. 20 reads over 4 s.
    wait_for("turns running", BUDGET, || (sb.file("turn-starts").lines().count() >= 3).then_some(()));
    for _ in 0..20 {
        let gate = http_gate(port);
        assert_eq!(gate["restart_gate"]["idle"], false, "the fixture went idle by itself: {gate}");
        std::thread::sleep(Duration::from_millis(200));
    }
    let v2 = sb.v2("p", &v1);
    sb.pin("p", &v2);
    let (_, ver) = wait_for("p restarted onto v2 under continuous turns", BUDGET, || {
        sb.up("p", port).filter(|(p, _)| *p != pid1)
    });
    assert_eq!(ver, v2);
    assert_eq!(sb.file("kills").lines().next().map(str::trim), Some(format!("{pid1} running=0").as_str()), "a turn was running at the stop");
    sb.rm("continuous");
}

fn http_gate(port: u16) -> serde_json::Value {
    use std::io::{Read, Write};
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(b"GET /gate HTTP/1.0\r\n\r\n").unwrap();
    let mut b = String::new();
    s.read_to_string(&mut b).unwrap();
    serde_json::from_str(b.split_once("\r\n\r\n").unwrap().1).unwrap()
}

// ═════════════════════════════════════════════════════ the danger ═══════

/// A build that DECLARES `[quiet.drain]` but whose process does not serve the
/// door (no `draining_since` key) is never signalled: SIGUSR1's default is to
/// terminate, and a fake without the handler dies of it. It keeps R9.C7's
/// plain wait instead, and is restarted when it reads idle.
#[test]
fn c7c_a_process_without_the_drain_door_is_never_signalled_outlasting_6_polls() {
    let mut sb = Sandbox::new("nodoor");
    sb.touch("nodoor");
    let (port, v1, pid1) = sb.up_on_v1("n", &drain(CAP_SECS, "handoff"));
    sb.hold_a_turn();
    let v2 = sb.v2("n", &v1);
    sb.pin("n", &v2);
    wait_for("the plain busy wait", BUDGET, || sb.log().contains("is NOT QUIET").then_some(()));
    // R9.C7d / F-A: C7's busy message names what `cauldron restart` costs here.
    assert!(!sb.log().contains("regardless") && sb.log().contains("SIGPIPE"), "{}", sb.log());
    std::thread::sleep(HOLD);
    assert_eq!(sb.up("n", port), Some((pid1, v1)), "signalled to death or restarted\n{}", sb.log());
    assert!(sb.file("drain-log").is_empty());
    sb.rm("hold");
    let (_, ver) = wait_for("n restarted once idle", BUDGET, || sb.up("n", port).filter(|(p, _)| *p != pid1));
    assert_eq!(ver, v2);
}

// ═════════════════════════════════════════════════ doctrine 10 ══════════

/// Who drains today, from the SHIPPED declarations: threads, with USR1/USR2,
/// a 600 s cap and `handoff` (R9.C7b, E18-15: `extend` held prod intake for
/// 2h35m; handoff is safe since R9.L2 retries a SIGPIPE'd child); every other
/// service declares none.
#[test]
fn c7c_threads_alone_declares_a_drain_and_every_other_service_does_not() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("services");
    let mut drains = Vec::new();
    let mut none = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let v: toml::Value = toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let name = v["name"].as_str().unwrap().to_string();
        match v.get("quiet").and_then(|q| q.get("drain")) {
            Some(d) => drains.push((name, d.clone())),
            None => none.push(name),
        }
    }
    none.sort();
    assert_eq!(drains.len(), 1, "{drains:?}");
    let (name, d) = &drains[0];
    assert_eq!(name, "threads");
    assert_eq!(d["start"].as_str(), Some("USR1"));
    assert_eq!(d["cancel"].as_str(), Some("USR2"));
    assert_eq!(d["since"].as_str(), Some("/restart_gate/draining_since"));
    assert_eq!(d["running"].as_str(), Some("/restart_gate/turns"));
    assert_eq!(d.get("cap_secs").and_then(|c| c.as_integer()), Some(600));
    assert!(d["cap_secs"].as_integer().unwrap() <= 600, "E18-15: a drain blocks intake for at most 10 min");
    assert_eq!(d["at_cap"].as_str(), Some("handoff"));
    assert_eq!(d["turn_ids"].as_str(), Some("/restart_gate/turn_threads"));
    assert_eq!(none, ["claude-account-expiry-alarm", "echo", "soup-backup", "tally", "tts", "ui-server"]);
}

// ═══════════════════════════════════════════════ R9.C7d: restart drains ═══
//
// `cauldron restart` and the unhealthy (503) relaunch go through the same drain
// as a re-pin. THE LONGEST PERIOD on these paths is still the supervisor's
// pass (`poll_secs` = 1): every "not restarted" is asserted after the daemon
// logged its decision and again after `HOLD` (six passes). The unhealthy path
// adds `down_after` = 3 failed checks before it acts at all, which the
// `wait_for` budgets cover.

impl Sandbox {
    fn launches_why(&self, name: &str, why: &str) -> usize {
        std::fs::read_to_string(self.root.join("data").join(&self.env).join("supervisions.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|r| r["service"] == name && r["act"] == "launch" && r["why"] == why)
            .count()
    }

    fn restart(&self, name: &str) -> String {
        let (rc, out) = self.cauldron(&["restart", name]);
        assert_eq!(rc, 0, "restart {name}: {out}");
        out
    }

    /// v1 up under the daemon, probed over HTTP (`/gate`), so a 503 reads unhealthy.
    fn up_on_v1_http(&mut self, name: &str, quiet: &str) -> (u16, String, u32) {
        let port = self.declare_probe(name, quiet, true);
        let v1 = self.build(name);
        self.pin(name, &v1);
        self.start_daemon();
        let (pid, ver) = wait_for("v1 up under the daemon", BUDGET, || self.up(name, port));
        assert_eq!(ver, v1);
        (port, v1, pid)
    }
}

/// `cauldron restart` while a turn executes: the service is asked to drain and
/// is restarted only after the turn ends on its own. At base (`Ask::Restart` →
/// `restart_now`) it was stopped at once with the turn running.
#[test]
fn c7d_a_restart_while_a_turn_executes_drains_and_waits_for_it_outlasting_6_polls() {
    let mut sb = Sandbox::new("rst");
    let (port, v1, pid1) = sb.up_on_v1("r", &drain(1800, "extend"));
    let turn = sb.hold_a_turn();
    let said = sb.restart("r");
    assert!(said.contains("asked to drain first"), "{said}");

    wait_for("the daemon to ask r to drain for the restart", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    assert!(sb.log().contains("the requested restart of `r`: it is busy"), "{}", sb.log());
    std::thread::sleep(HOLD);
    assert_eq!(sb.up("r", port), Some((pid1, v1.clone())), "restarted while a turn ran\n{}", sb.log());
    assert_eq!(sb.launches_why("r", "requested"), 0, "{}", sb.log());
    assert!(alive(turn), "the turn was touched");
    assert!(sb.file("kills").is_empty(), "stopped mid-turn: {}", sb.file("kills"));

    sb.rm("hold");
    let (pid2, ver2) = wait_for("r restarted once the turn ended", BUDGET, || {
        sb.up("r", port).filter(|(p, _)| *p != pid1)
    });
    assert_ne!(pid2, pid1);
    assert_eq!(ver2, v1, "a restart relaunches the pin");
    assert_eq!(sb.file("done").lines().count(), 1, "the turn ended on its own, once: {}", sb.file("done"));
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=0"), "stopped with a turn running");
    assert_eq!(sb.launches_why("r", "requested"), 1, "{}", sb.log());
    assert!(sb.log().contains("has DRAINED for the requested restart"), "{}", sb.log());
}

/// Paired with the above: with no turn running, `cauldron restart` is
/// immediate and the service is never asked to drain.
#[test]
fn c7d_a_restart_with_no_turn_running_is_immediate_and_never_drains() {
    let mut sb = Sandbox::new("rsti");
    let (port, v1, pid1) = sb.up_on_v1("q", &drain(1800, "extend"));
    let t0 = Instant::now();
    sb.restart("q");
    let (_, ver) = wait_for("q restarted", BUDGET, || sb.up("q", port).filter(|(p, _)| *p != pid1));
    assert_eq!(ver, v1);
    assert!(t0.elapsed() < Duration::from_secs(30), "an idle restart took {:?}", t0.elapsed());
    assert_eq!(sb.drain_begins(), 0, "an idle service was asked to drain: {}", sb.file("drain-log"));
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=0"));
    assert_eq!(sb.launches_why("q", "requested"), 1, "{}", sb.log());
}

/// A requested restart under `at_cap = "extend"` does not restart past the cap,
/// and the warning no longer offers a "now regardless" path without naming what
/// it costs (F-A): only a stop ends it sooner, and that kills the turns.
#[test]
fn c7d_a_restart_at_the_3s_extend_cap_keeps_waiting_and_names_the_cost_outlasting_6_polls() {
    let mut sb = Sandbox::new("rstx");
    let (port, v1, pid1) = sb.up_on_v1("y", &drain(CAP_SECS, "extend"));
    let turn = sb.hold_a_turn();
    sb.restart("y");
    wait_for("the daemon to say the restart is extending", BUDGET, || {
        sb.log().contains("the requested restart waits for them").then_some(())
    });
    std::thread::sleep(HOLD);
    assert_eq!(sb.up("y", port), Some((pid1, v1)), "restarted past an extend cap\n{}", sb.log());
    assert!(alive(turn));
    let log = sb.log();
    assert!(!log.contains("regardless"), "a wait still offers `now regardless`: {log}");
    assert!(log.contains("SIGPIPE") && log.contains("`cauldron stop y`"), "{log}");
    sb.rm("hold");
    wait_for("y restarted once the turn ended", BUDGET, || sb.up("y", port).filter(|(p, _)| *p != pid1));
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=0"));
}

/// A build with NO drain door still restarts on request, at once, exactly as
/// before — and is never sent SIGUSR1 (which would kill it). The log says the
/// restart kills its running turn.
#[test]
fn c7d_a_restart_of_a_build_without_the_door_is_immediate_and_says_it_kills() {
    let mut sb = Sandbox::new("rstn");
    sb.touch("nodoor");
    let (port, v1, pid1) = sb.up_on_v1("o", &drain(1800, "extend"));
    sb.hold_a_turn();
    sb.restart("o");
    let (_, ver) = wait_for("o restarted at once", Duration::from_secs(30), || sb.up("o", port).filter(|(p, _)| *p != pid1));
    assert_eq!(ver, v1);
    assert!(sb.file("drain-log").is_empty(), "signalled: {}", sb.file("drain-log"));
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=1"), "premise: a turn was running at the stop");
    assert!(sb.log().contains("has no drain door") && sb.log().contains("SIGPIPE"), "{}", sb.log());
    sb.rm("hold");
}

/// The C6 relaunch of a service answering 503 drains first: while its turn
/// runs it is asked to drain and NOT relaunched; when the turn ends it is. At
/// base the unhealthy path stopped it with the turn running.
#[test]
fn c7d_b_the_503_relaunch_drains_and_waits_for_the_turn_outlasting_6_polls() {
    let mut sb = Sandbox::new("sick");
    let (port, _v1, pid1) = sb.up_on_v1_http("s", &drain(1800, "extend"));
    let turn = sb.hold_a_turn();
    // The first launch is itself `why = down`; only relaunches after the wedge count.
    let launched_before = sb.launches_why("s", "down");
    std::fs::write(sb.root.join("wedged"), pid1.to_string()).unwrap();
    wait_for("the daemon to ask the unhealthy s to drain", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    assert!(sb.log().contains("the relaunch of the unhealthy `s`: it is busy"), "{}", sb.log());
    std::thread::sleep(HOLD);
    assert!(alive(pid1), "relaunched while a turn ran\n{}", sb.log());
    assert!(sb.file("kills").is_empty(), "stopped mid-turn: {}", sb.file("kills"));
    assert_eq!(sb.launches_why("s", "down"), launched_before, "{}", sb.log());
    assert!(alive(turn));

    sb.rm("hold");
    let (pid2, _) = wait_for("s relaunched once the turn ended", BUDGET, || sb.up("s", port).filter(|(p, _)| *p != pid1));
    assert_ne!(pid2, pid1);
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=0"), "stopped with a turn running");
    assert_eq!(sb.file("done").lines().count(), 1);
    assert_eq!(sb.launches_why("s", "down"), launched_before + 1, "{}", sb.log());
}

/// Paired: an unhealthy service is relaunched at the drain cap even under
/// `extend` (a wedged store must not mean an indefinite outage), handing the
/// running turn off rather than waiting forever.
#[test]
fn c7d_b_the_503_relaunch_goes_ahead_at_the_3s_cap_even_under_extend() {
    let mut sb = Sandbox::new("sickc");
    let (port, _v1, pid1) = sb.up_on_v1_http("k", &drain(CAP_SECS, "extend"));
    let turn = sb.hold_a_turn();
    std::fs::write(sb.root.join("wedged"), pid1.to_string()).unwrap();
    wait_for("asked to drain", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    let asked = Instant::now();
    wait_for("k relaunched at the cap", BUDGET, || sb.up("k", port).filter(|(p, _)| *p != pid1));
    assert!(asked.elapsed() >= Duration::from_secs(CAP_SECS - 1), "fired early: {:?}", asked.elapsed());
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=1"), "premise: handed off with the turn running");
    assert!(alive(turn), "the handed-off turn was killed");
    assert!(sb.log().contains("It is UNHEALTHY, so it is relaunched now"), "{}", sb.log());
    sb.rm("hold");
}

/// Paired: an unhealthy build WITHOUT the door is relaunched as before, never
/// signalled.
#[test]
fn c7d_b_the_503_relaunch_of_a_build_without_the_door_is_unchanged() {
    let mut sb = Sandbox::new("sickn");
    sb.touch("nodoor");
    let (port, _v1, pid1) = sb.up_on_v1_http("w", &drain(1800, "extend"));
    sb.hold_a_turn();
    std::fs::write(sb.root.join("wedged"), pid1.to_string()).unwrap();
    wait_for("w relaunched", BUDGET, || sb.up("w", port).filter(|(p, _)| *p != pid1));
    assert!(sb.file("drain-log").is_empty(), "signalled: {}", sb.file("drain-log"));
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=1"));
    sb.rm("hold");
}

// ═══════════════════════════════════════ R9.C7b (E18-15): a BOUNDED drain ═══
//
// DEPLOY-9c's draining threads pin held prod intake for 2h35m: at the 1800 s
// cap, `at_cap = "extend"` kept waiting for one live coordinator turn, and the
// operator's own message waited ~1 h behind it (ledger E18-15). Threads now
// declares `handoff` with a cap of 600 s. These two tests run the SHIPPED
// `[quiet]` of `services/threads.toml` — read from the file, not restated — so
// setting `at_cap` back to `extend` there turns the first one red (the pin
// never completes inside its window). Only `cap_secs` is shrunk, and only in
// the first test (doctrine 4: the cap is the longest period on that path).
// The real binary's half (the turn is re-adopted and lands its own verdict)
// is `services/threads/tests/drain_pin.rs::c7b_…`.

/// `[quiet]` + `[quiet.drain]` exactly as `services/threads.toml` ships them,
/// with `cap_secs` replaced when `cap` is given.
fn shipped_threads_quiet(cap: Option<u64>) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("services").join("threads.toml");
    let v: toml::Value = toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    let mut q = v.get("quiet").expect("threads declares [quiet]").clone();
    if let Some(c) = cap {
        let d = q.get_mut("drain").and_then(|d| d.as_table_mut()).expect("threads declares [quiet.drain]");
        d.insert("cap_secs".into(), toml::Value::Integer(c as i64));
    }
    let mut root = toml::map::Map::new();
    root.insert("quiet".into(), q);
    toml::to_string(&toml::Value::Table(root)).unwrap()
}

/// A turn that NEVER finishes on its own: the pin still completes, at the cap,
/// by handoff; the turn is not killed and is counted by the new instance; and
/// a prompt that arrived mid-drain runs on the new instance while the
/// handed-off turn is still running — intake resumed, bounded by the cap.
/// RED with the shipped `at_cap = "extend"`: no restart inside 60 s.
#[test]
fn c7b_shipped_threads_drain_hands_a_never_ending_turn_off_at_a_3s_cap_and_intake_resumes() {
    let mut sb = Sandbox::new("c7bh");
    let (port, v1, pid1) = sb.up_on_v1("th", &shipped_threads_quiet(Some(CAP_SECS)));
    let turn = sb.hold_a_turn();
    let v2 = sb.v2("th", &v1);
    sb.pin("th", &v2);
    wait_for("asked to drain", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    let asked = Instant::now();
    sb.touch("queued"); // a prompt arrives while the drain holds intake
    let (pid2, ver) = wait_for(
        "the pin to COMPLETE at the cap (th restarted onto v2) — an `extend` cap never does",
        Duration::from_secs(60),
        || sb.up("th", port).filter(|(p, _)| *p != pid1),
    );
    assert_eq!(ver, v2);
    assert!(asked.elapsed() >= Duration::from_secs(CAP_SECS - 1), "fired before the cap: {:?}", asked.elapsed());
    assert!(sb.log().contains("HANDING THOSE TURNS OFF"), "{}", sb.log());
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=1"), "stopped with the turn running, once");
    assert!(alive(turn), "the never-ending turn was killed by the handoff");
    // Intake resumes on the NEW instance, while the handed-off turn still runs.
    let ran = wait_for("the prompt queued mid-drain to run", BUDGET, || {
        let t = sb.file("intake");
        (!t.is_empty()).then_some(t)
    });
    assert_eq!(ran.trim(), format!("intake {pid2}"), "the queued prompt ran on the wrong instance");
    assert!(alive(turn), "intake waited for the handed-off turn");
    assert_eq!(http_gate(port)["restart_gate"]["turn_threads"], serde_json::json!([format!("turn-{turn}")]));
    let st = sb.status();
    assert!(!st.contains("draining since"), "a finished drain is still shown: {st}");
    sb.rm("hold");
    wait_for("the handed-off turn to end on its own", BUDGET, || (!alive(turn)).then_some(()));
    assert_eq!(sb.file("done").lines().count(), 1, "{}", sb.file("done"));
    let starts = sb.file("turn-starts");
    assert_eq!(starts.lines().filter(|l| l.trim() == turn.to_string()).count(), 1, "re-run: {starts}");
    assert_eq!(sb.repin_launches("th"), 1, "{}", sb.log());
}

/// Paired: under the SHIPPED cap (600 s, unmodified), a turn that ends before
/// the cap is not handed off — the restart follows the drain — and while the
/// drain runs `cauldron status` shows it: since-when, its AGE, the cap, and
/// the turn it waits for (E18-15). Asserted again after `HOLD` (six passes).
#[test]
fn c7b_shipped_threads_drain_a_turn_ending_before_the_600s_cap_is_not_handed_off_and_status_names_it() {
    let mut sb = Sandbox::new("c7bp");
    let (port, v1, pid1) = sb.up_on_v1("tp", &shipped_threads_quiet(None));
    let turn = sb.hold_a_turn();
    let v2 = sb.v2("tp", &v1);
    sb.pin("tp", &v2);
    wait_for("asked to drain", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    let st = wait_for("status to show the drain", BUDGET, || {
        let s = sb.status();
        s.contains("tp: draining since").then_some(s)
    });
    for want in ["(for ", "1 running, cap at ", "(600s)", "handed off to the new process"] {
        assert!(st.contains(want), "status lacks {want:?}: {st}");
    }
    assert!(st.contains(&format!("waiting for: turn-{turn}")), "status does not name the turn: {st}");
    std::thread::sleep(HOLD);
    assert_eq!(sb.up("tp", port), Some((pid1, v1.clone())), "restarted while the turn ran\n{}", sb.log());
    let st = sb.status();
    let age: u64 = st
        .split("(for ")
        .nth(1)
        .and_then(|r| r.split('s').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no age in seconds: {st}"));
    assert!(age >= HOLD.as_secs(), "the drain's age does not advance: {age}s after {HOLD:?}: {st}");
    sb.rm("hold");
    let (_, ver) = wait_for("tp restarted once the turn ended", BUDGET, || sb.up("tp", port).filter(|(p, _)| *p != pid1));
    assert_eq!(ver, v2);
    assert!(sb.log().contains("has DRAINED") && !sb.log().contains("HANDING"), "{}", sb.log());
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=0"));
    assert_eq!(sb.file("done").lines().count(), 1);
    assert!(!sb.status().contains("draining since"), "the line outlived the restart");
}

// ══════════════════════════════════════ R9.C7f: a pin's own drain terms ═══
//
// The drain obeys the RUNNING build's `[quiet.drain]`. Prod threads' running
// build declares `extend` (no upper bound), so the pin installing a build
// that declares `handoff` would wait behind any long turn (E18-15). `cauldron
// pin <svc> <ver> --at-cap handoff --drain-cap N` overrides it for THAT pin.
// THE LONGEST PERIOD is still the supervisor's pass (`poll_secs` = 1); every
// "not restarted" is asserted after the daemon logged its extend decision and
// again after `HOLD` (six passes).

impl Sandbox {
    fn pin_with(&self, name: &str, label: &str, extra: &[&str]) -> (i32, String) {
        let mut a = vec!["pin", name, label];
        a.extend_from_slice(extra);
        self.cauldron(&a)
    }

    fn env_toml(&self) -> String {
        std::fs::read_to_string(self.root.join("data").join(&self.env).join("env.toml")).unwrap_or_default()
    }
}

/// GUARD (1): over a running build that declares `extend` (cap 1800 s), a
/// pin with `--at-cap handoff --drain-cap 4` restarts it at ~4 s, and the turn
/// still running is handed off — not killed, counted by the new instance,
/// ended once by itself. `status` shows the effective terms and that the pin
/// overrode them.
#[test]
fn c7f_1_pin_at_cap_handoff_drain_cap_4s_overrides_a_running_extend_and_hands_the_turn_off() {
    let mut sb = Sandbox::new("ovr");
    let (port, v1, pid1) = sb.up_on_v1("o", &drain(1800, "extend"));
    let turn = sb.hold_a_turn();
    let v2 = sb.v2("o", &v1);
    let (rc, said) = sb.pin_with("o", &v2, &["--at-cap", "handoff", "--drain-cap", "4"]);
    assert_eq!(rc, 0, "{said}");
    assert!(said.contains("OVERRIDDEN by this pin"), "pin does not say it overrides: {said}");
    wait_for("asked to drain", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    let asked = Instant::now();
    let st = sb.status();
    assert!(
        st.contains("o: draining since ")
            && st.contains("(4s)")
            && st.contains("at_cap handoff / cap 4s OVERRIDDEN by this pin (the running build declares extend / 1800s)"),
        "status does not show the effective, overridden terms: {st}"
    );
    // Restarted at the 4 s cap, not at 1800 s and not never (extend).
    let (_, ver) = wait_for("o restarted at the pin's 4s cap", Duration::from_secs(40), || {
        sb.up("o", port).filter(|(p, _)| *p != pid1)
    });
    assert!(asked.elapsed() >= Duration::from_secs(4 - 1), "fired before the pin's cap: {:?}", asked.elapsed());
    assert_eq!(ver, v2);
    assert!(sb.log().contains("HANDING THOSE TURNS OFF") && sb.log().contains("OVERRIDDEN by this pin"), "{}", sb.log());
    // Re-adopted, not killed.
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=1"));
    assert!(alive(turn), "the handed-off turn was killed");
    assert_eq!(http_gate(port)["restart_gate"]["turns"], 1, "the new instance does not count the handed-off turn");
    sb.rm("hold");
    wait_for("the handed-off turn to end", BUDGET, || (!alive(turn)).then_some(()));
    assert_eq!(sb.file("done").lines().count(), 1, "{}", sb.file("done"));
    assert_eq!(sb.file("turn-starts").lines().count(), 1, "re-run: {}", sb.file("turn-starts"));
    assert_eq!(sb.repin_launches("o"), 1, "{}", sb.log());
}

/// GUARD (2), opposing (1): with NO flags, the running declaration's `extend`
/// still extends past its 3 s cap — even when the PREVIOUS pin of this
/// service carried `--at-cap handoff --drain-cap 3600` (an override is this
/// pin's only, and a pin without flags clears it).
///
/// R9.C7f4 (inspect R9.C7f3 F1): the stale override is given to a pin the
/// holder CANNOT spend yet — v2, pinned with flags while a turn is held, so
/// its drain stays open (handoff at 3600 s) — and it is still in env.toml when
/// the flagless pin of v3 runs. Since R9.C7f3 a flagged pin of the RUNNING
/// build is spent by the holder at once, so the old setup never put a stale
/// override in front of `verb_pin`.
#[test]
fn c7f_2_absent_flags_extend_still_extends_past_the_3s_cap_outlasting_6_polls() {
    let mut sb = Sandbox::new("noovr");
    let (port, v1, pid1) = sb.up_on_v1("q", &drain(CAP_SECS, "extend"));
    let turn = sb.hold_a_turn();
    let v2 = sb.v2("q", &v1);
    let (rc, out) = sb.pin_with("q", &v2, &["--at-cap", "handoff", "--drain-cap", "3600"]);
    assert_eq!(rc, 0, "{out}");
    wait_for("asked to drain for v2", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    // Sentinel: the v2 override is live and unspent when the next pin runs —
    // the drain it governs is open and its pin has not completed.
    let o = sb.pin_drain("q").unwrap_or_else(|| panic!("sentinel: v2's override is not in env.toml: {}", sb.env_toml()));
    assert_eq!(o["for_version"].as_str(), Some(v2.as_str()), "{o}");
    assert!(sb.status().contains("OVERRIDDEN"), "sentinel: v2's drain runs under its override: {}", sb.status());
    sb.new_bytes("v3");
    let v3 = sb.build("q");
    assert!(v3 != v1 && v3 != v2, "sentinel: a third label {v3}");
    let said = sb.pin("q", &v3);
    assert!(!said.contains("OVERRIDDEN"), "{said}");
    assert!(sb.pin_drain("q").is_none(), "the flagless pin kept a stale override: {}", sb.env_toml());
    wait_for("the daemon to say it is extending", BUDGET, || sb.log().contains("at_cap = \"extend\"").then_some(()));
    std::thread::sleep(HOLD);
    assert_eq!(sb.up("q", port), Some((pid1, v1.clone())), "restarted past an extend cap\n{}", sb.log());
    assert!(alive(turn));
    assert!(!sb.log().contains("HANDING THOSE TURNS OFF"), "{}", sb.log());
    let st = sb.status();
    assert!(st.contains("(3s)") && !st.contains("OVERRIDDEN"), "{st}");
    sb.rm("hold");
    let (_, ver) = wait_for("q restarted once the turn ended", BUDGET, || sb.up("q", port).filter(|(p, _)| *p != pid1));
    assert_eq!(ver, v3);
    assert_eq!(sb.file("kills").trim(), format!("{pid1} running=0"));
}

/// GUARD (3): `--drain-cap 0` and `--drain-cap 4000` are refused BEFORE any
/// drain opens: non-zero exit, the typed reason, env.toml untouched, and the
/// busy service is never asked to drain over `HOLD` (six passes). Paired:
/// `--drain-cap 3600` (the bound) is accepted, and the drain opens.
#[test]
fn c7f_3_drain_cap_0_and_4000_are_refused_before_any_drain_opens_outlasting_6_polls() {
    let mut sb = Sandbox::new("bad");
    let (port, v1, pid1) = sb.up_on_v1("z", &drain(1800, "extend"));
    sb.hold_a_turn();
    let v2 = sb.v2("z", &v1);
    let before = sb.env_toml();
    for (cap, why) in [("0", "`--drain-cap 0` is refused"), ("4000", "`--drain-cap 4000` is refused")] {
        let (rc, out) = sb.pin_with("z", &v2, &["--at-cap", "handoff", "--drain-cap", cap]);
        assert_ne!(rc, 0, "--drain-cap {cap} was accepted: {out}");
        assert!(out.contains(why) && out.contains("1..=3600"), "--drain-cap {cap}: {out}");
        assert_eq!(sb.env_toml(), before, "--drain-cap {cap} wrote env.toml");
    }
    let (rc, out) = sb.pin_with("z", &v2, &["--at-cap", "sometimes"]);
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("`--at-cap sometimes` is refused"), "{out}");
    std::thread::sleep(HOLD);
    assert_eq!(sb.drain_begins(), 0, "a drain opened on a refused pin: {}", sb.file("drain-log"));
    assert_eq!(sb.up("z", port), Some((pid1, v1.clone())));
    // Paired: the bound itself is accepted, and that pin does drain.
    let (rc, out) = sb.pin_with("z", &v2, &["--drain-cap", "3600"]);
    assert_eq!(rc, 0, "--drain-cap 3600 refused: {out}");
    wait_for("the accepted pin to drain", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    sb.rm("hold");
    let (_, ver) = wait_for("z restarted once the turn ended", BUDGET, || sb.up("z", port).filter(|(p, _)| *p != pid1));
    assert_eq!(ver, v2);
}

// ═══════════════════════════════════════════════════════════ R9.C7f2 ════
// A pin's drain override is cleared once its pin COMPLETES (the re-pin
// restart, a failed start of the pin, or a pin back), so it never re-applies
// to a later drain it was not given for (inspect F1, ledger E19-8).

impl Sandbox {
    /// `env.toml`'s `[services.<name>.drain]`, parsed (not grepped: the file
    /// also carries comments and other services).
    fn pin_drain(&self, name: &str) -> Option<toml::Value> {
        let v: toml::Value = toml::from_str(&self.env_toml()).unwrap_or_else(|e| panic!("{e}: {}", self.env_toml()));
        v.get("services").and_then(|s| s.get(name)).and_then(|p| p.get("drain")).cloned()
    }

    /// Writes `good` for `name` into env.toml, so `cauldron rollback` has a
    /// target (this sandbox's `stable_after_secs = 600` proves nothing good).
    fn set_good(&self, name: &str, good: &[&str]) {
        let mut v: toml::Value = toml::from_str(&self.env_toml()).unwrap();
        let p = v["services"].get_mut(name).expect("pinned").as_table_mut().unwrap();
        p.insert("good".into(), toml::Value::Array(good.iter().map(|g| toml::Value::String(g.to_string())).collect()));
        std::fs::write(self.root.join("data").join(&self.env).join("env.toml"), toml::to_string(&v).unwrap()).unwrap();
    }
}

/// GUARD (C7f2-1): the override APPLIES to its own pin — over a running build
/// that declares `extend` / 1800 s, `--at-cap handoff --drain-cap 3` restarts
/// it at the 3 s cap — and once that re-pin restart has happened it is GONE
/// from env.toml and `status` no longer shows it.
#[test]
fn c7f2_1_the_override_applies_to_its_own_pin_and_is_absent_once_that_pin_completes() {
    let mut sb = Sandbox::new("clr");
    let (port, v1, pid1) = sb.up_on_v1("c", &drain(1800, "extend"));
    let turn = sb.hold_a_turn();
    let v2 = sb.v2("c", &v1);
    let (rc, said) = sb.pin_with("c", &v2, &["--at-cap", "handoff", "--drain-cap", "3"]);
    assert_eq!(rc, 0, "{said}");
    // Sentinel: the override was written, bound to this pin.
    let o = sb.pin_drain("c").unwrap_or_else(|| panic!("sentinel: no override written: {}", sb.env_toml()));
    assert_eq!(o["for_version"].as_str(), Some(v2.as_str()), "{o}");
    // APPLIED: an extend/1800 build is restarted at the pin's 3 s cap.
    let (_, ver) = wait_for("c restarted at the pin's 3s cap", Duration::from_secs(40), || {
        sb.up("c", port).filter(|(p, _)| *p != pid1)
    });
    assert_eq!(ver, v2);
    assert!(sb.log().contains("HANDING THOSE TURNS OFF"), "{}", sb.log());
    // ABSENT once the pin completed.
    wait_for("the override cleared after its pin completed", Duration::from_secs(10), || {
        sb.pin_drain("c").is_none().then_some(())
    });
    assert!(!sb.status().contains("OVERRIDDEN"), "{}", sb.status());
    assert!(alive(turn), "the handed-off turn was killed");
    sb.rm("hold");
    wait_for("the handed-off turn to end", BUDGET, || (!alive(turn)).then_some(()));
}

/// GUARD (C7f2-2): a pin BACK (a rollback while the override's drain is still
/// open) clears it, and a LATER pin of that same version drains under the
/// build's own declaration. The declaration is `handoff` / 3 s; the override
/// is `extend` / 3600 s, so the two are told apart by behaviour: under the
/// override nothing restarts over `HOLD` (six passes, past the 3 s cap);
/// under the declaration the build is restarted at 3 s.
#[test]
fn c7f2_2_a_pin_back_clears_it_and_a_later_pin_of_that_version_drains_under_the_declaration_outlasting_6_polls() {
    let mut sb = Sandbox::new("back");
    let (port, v1, pid1) = sb.up_on_v1("b", &drain(CAP_SECS, "handoff"));
    let turn = sb.hold_a_turn();
    let v2 = sb.v2("b", &v1);
    let (rc, said) = sb.pin_with("b", &v2, &["--at-cap", "extend", "--drain-cap", "3600"]);
    assert_eq!(rc, 0, "{said}");
    // `rollback` re-pins `good[1]` (`Manifest::previous_good`).
    sb.set_good("b", &[&v2, &v1]);
    wait_for("asked to drain", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    // Opposing sentinel: the override IS in force for its own pin — the
    // declared 3 s handoff does not fire.
    std::thread::sleep(HOLD);
    assert_eq!(sb.up("b", port), Some((pid1, v1.clone())), "the override did not apply\n{}", sb.log());
    assert!(sb.status().contains("OVERRIDDEN by this pin"), "{}", sb.status());
    // PIN BACK: rollback to the running v1.
    let (rc, out) = sb.cauldron(&["rollback", "b"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains(&format!("-> {v1}")), "{out}");
    assert!(sb.pin_drain("b").is_none(), "a pin back left the override: {}", sb.env_toml());
    // A LATER pin of the same version, by `rollback` again (v2 now `good[1]`).
    sb.set_good("b", &[&v1, &v2]);
    let (rc, out) = sb.cauldron(&["rollback", "b"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains(&format!("-> {v2}")), "{out}");
    let (_, ver) = wait_for("b restarted at the DECLARED 3s handoff cap", Duration::from_secs(40), || {
        sb.up("b", port).filter(|(p, _)| *p != pid1)
    });
    assert_eq!(ver, v2);
    assert!(alive(turn), "the handed-off turn was killed");
    sb.rm("hold");
    wait_for("the handed-off turn to end", BUDGET, || (!alive(turn)).then_some(()));
}

/// GUARD (C7f2-3): a pin that FAILS (its build will not start) also clears
/// its override.
#[test]
fn c7f2_3_a_failed_pin_also_clears_its_override() {
    let mut sb = Sandbox::new("fail");
    let (_port, v1, _pid1) = sb.up_on_v1("f", &drain(1800, "extend"));
    let good = std::fs::read_to_string(sb.root.join("fake")).unwrap();
    std::fs::write(sb.root.join("fake"), "#!/usr/bin/env python3\nimport sys\nsys.exit(3)\n").unwrap();
    let v2 = sb.build("f");
    assert_ne!(v2, v1, "sentinel: a second label");
    std::fs::write(sb.root.join("fake"), good).unwrap();
    let (rc, said) = sb.pin_with("f", &v2, &["--at-cap", "handoff", "--drain-cap", "3"]);
    assert_eq!(rc, 0, "{said}");
    assert!(sb.pin_drain("f").is_some(), "sentinel: no override written: {}", sb.env_toml());
    wait_for("the pin to fail", BUDGET, || {
        let l = sb.log();
        (l.contains("failed to start") || l.contains("EXITED")).then_some(())
    });
    wait_for("the override cleared after its pin failed", Duration::from_secs(10), || {
        sb.pin_drain("f").is_none().then_some(())
    });
}

// ═══════════════════════════════════════════════════════════ R9.C7f3 ════
// Inspect R9.C7f2 findings 1 and 2: the launch-path clear had no guard, and a
// pin that resolves to the build ALREADY RUNNING (`latest` while the newest
// build runs) never completed, so its override waited for the next build.

/// GUARD (C7f3-a): a flagged pin LAUNCHED WITH NOTHING RUNNING completes at
/// that launch, and its override is cleared from env.toml — here a launch that
/// FAILS, so no other site can clear it: the service never comes up, so there
/// is no running build for a re-pin restart (daemon.rs `restart_now` site) or
/// for the already-running branch to complete it. Before R9.C7f3 removing the
/// launch-path `pin_completed` left all three C7f2 tests green (inspect F1).
#[test]
fn c7f3_a_a_pin_launched_with_nothing_running_clears_its_override_even_when_the_launch_fails() {
    let mut sb = Sandbox::new("down");
    sb.declare("w", &drain(1800, "extend"));
    std::fs::write(sb.root.join("fake"), "#!/usr/bin/env python3\nimport sys\nsys.exit(3)\n").unwrap();
    let v1 = sb.build("w");
    let (rc, said) = sb.pin_with("w", &v1, &["--at-cap", "handoff", "--drain-cap", "3"]);
    assert_eq!(rc, 0, "{said}");
    // Sentinel: written, bound to this pin, with no holder yet to clear it.
    let o = sb.pin_drain("w").unwrap_or_else(|| panic!("sentinel: no override written: {}", sb.env_toml()));
    assert_eq!(o["for_version"].as_str(), Some(v1.as_str()), "{o}");
    sb.start_daemon();
    wait_for("the launch of the pin to fail", BUDGET, || {
        let l = sb.log();
        (l.contains("failed to start") || l.contains("EXITED")).then_some(())
    });
    wait_for("the override cleared after its launch", Duration::from_secs(10), || {
        sb.pin_drain("w").is_none().then_some(())
    });
    assert!(sb.seam("w")["pid"].as_u64().map(|p| alive(p as u32)) != Some(true), "sentinel: w never came up: {}", sb.log());
    assert!(sb.log().contains("has completed, so the drain terms it carried"), "{}", sb.log());
}

/// GUARD (C7f3-b): `cauldron pin <svc> latest --at-cap extend --drain-cap
/// 3600` while the newest build ALREADY RUNS completes at once (nothing to
/// drain), so the holder spends the override; the re-pin onto the NEXT build
/// `cauldron build` produces then drains under that running build's own
/// declaration, `handoff` / 3 s, and is restarted at its cap. Before R9.C7f3
/// the override stayed bound to `latest` and applied to that later drain:
/// `extend` / 3600 s, so nothing restarted within the 40 s wait.
#[test]
fn c7f3_b_pin_latest_with_drain_flags_while_latest_runs_is_spent_restarts_nothing_outlasting_6_polls_and_the_next_build_drains_as_declared() {
    let mut sb = Sandbox::new("latest");
    let (port, v1, pid1) = sb.up_on_v1("l", &drain(CAP_SECS, "handoff"));
    let (rc, said) = sb.pin_with("l", "latest", &["--at-cap", "extend", "--drain-cap", "3600"]);
    assert_eq!(rc, 0, "{said}");
    // Sentinel: the CLI wrote the override (read off its report: the holder
    // may already have spent it by the time env.toml is read).
    assert!(said.contains("this pin carries its own drain terms (at_cap extend, cap 3600s)"), "{said}");
    wait_for("the holder to spend the override of a pin that already runs", Duration::from_secs(10), || {
        sb.pin_drain("l").is_none().then_some(())
    });
    // R9.C7f5: WAITED for, not read at the instant env.toml changed. The
    // holder's save renames env.toml into place and THEN fsyncs the directory
    // (F_FULLFSYNC) before it prints this line, so under full-suite I/O load
    // the cleared file was seen while the line was still unwritten (red 2/2 in
    // R9.C7f5's full runs, the log ending at "came up").
    wait_for("the holder to say the pin completed", Duration::from_secs(10), || {
        sb.log().contains("pin latest has completed").then_some(())
    });
    // R9.C7f4 (inspect R9.C7f3 F3, doctrine 4): "restarted nothing" holds
    // across HOLD — six `poll_secs = 1` passes, the longest period on this
    // path — not at one instant right after the clear.
    std::thread::sleep(HOLD);
    assert_eq!(sb.up("l", port), Some((pid1, v1.clone())), "the spent pin restarted something\n{}", sb.log());
    assert_eq!(sb.repin_launches("l"), 0, "the spent pin launched something\n{}", sb.log());
    // The next build: `latest` now resolves past the running v1.
    let turn = sb.hold_a_turn();
    let v2 = sb.v2("l", &v1);
    wait_for("asked to drain for the next build", BUDGET, || (sb.drain_begins() > 0).then_some(()));
    let (_, ver) = wait_for("l restarted at the DECLARED 3s handoff cap", Duration::from_secs(40), || {
        sb.up("l", port).filter(|(p, _)| *p != pid1)
    });
    assert_eq!(ver, v2);
    assert!(sb.log().contains("HANDING THOSE TURNS OFF"), "{}", sb.log());
    assert!(alive(turn), "the handed-off turn was killed");
    sb.rm("hold");
    wait_for("the handed-off turn to end", BUDGET, || (!alive(turn)).then_some(()));
}
