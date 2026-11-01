//! **C-1: A SERVICE CAULDRON OWNS IS GONE WHEN ITS PID IS GONE, NOT WHEN ITS
//! PORT GOES QUIET** — the real binary, real hosted children, real signals.
//!
//! `threads` closes its port FIRST and then drains for up to 45 s. Before
//! C-1, `stop_by_seam` read the quiet port as "stopped" within ~50 ms, every
//! caller then launched a replacement onto the same data directory, and a
//! drain that kept its port open past 15 s was SIGKILLed.
//!
//! The fake below is that shape: on SIGTERM it closes its listener, writes
//! `draining`, sleeps `FAKE_DRAIN` seconds, writes `down` and exits 0. A
//! SECOND SIGTERM during the drain writes `second-signal` and exits 9 —
//! `threads` abandons its drain on a second signal, so a stop path that
//! re-signals a draining service is visible here. Every incarnation writes
//! `up` when it binds. The EVENTS FILE is written by the services themselves,
//! not by cauldron and not by this test, so an overlap of two instances is
//! read off the processes' own timeline (doctrine 3: the probe is not the
//! thing under test).
//!
//! Every test runs a THROWAWAY cauldron (own `CAULDRON_HOME` under the temp
//! dir, own environment, kernel-assigned ports). Nothing touches
//! `~/.cauldron`, a live daemon or launchd.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

const FAKE: &str = r#"#!/usr/bin/env python3
import os, signal, socket, sys, time
port = int(os.environ["FAKE_PORT"])
ev = os.environ["FAKE_EVENTS"]
drain = float(os.environ.get("FAKE_DRAIN", "0"))
def note(word):
    with open(ev, "a") as f:
        f.write(f"{word} {os.getpid()} {time.time():.3f}\n")
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen(8)
note("up")
state = {"draining": False}
def on_term(signum, frame):
    if state["draining"]:
        note("second-signal")
        os._exit(9)
    state["draining"] = True
    s.close()
    note("draining")
    time.sleep(drain)
    note("down")
    os._exit(0)
signal.signal(signal.SIGTERM, on_term)
while True:
    try:
        c, _ = s.accept()
        c.close()
    except OSError:
        time.sleep(0.05)
"#;

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
        for (_, pid) in self.events() {
            let _ = Command::new("/bin/kill").args(["-9", &pid.to_string()]).stderr(Stdio::null()).status();
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

/// A POSITIVE wait's bound, derived as in `supervisor_restarts.rs`: up to 3
/// launches x the declared `start_timeout_ms` ([`launch_wait::START_TIMEOUT`],
/// 60 s since R9.F2g, longer than the 45 s `HOSTED_BIND_WAIT` these hosted
/// fakes bind under) + the 30 s backoff cap, + 30 s for the holder and a
/// python3 start under load. Never the bound of a negative claim here — those
/// are timed against the drain itself.
const BUDGET: Duration = Duration::from_secs(3 * launch_wait::START_TIMEOUT_SECS + 30 + 30);

impl Sandbox {
    fn new(tag: &str, supervisor: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "cauldron-c1-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("c1{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::create_dir_all(root.join("data").join(&env)).unwrap();
        std::fs::write(root.join("data").join(&env).join("env.toml"), format!("[supervisor]\n{supervisor}\n"))
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
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }

    /// Declare, build and pin one hosted fake that drains `drain` seconds on
    /// SIGTERM. `extra` is appended at the TOP level of the declaration.
    fn register(&self, name: &str, drain: &str, extra: &str) -> u16 {
        let port = free_port();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n{extra}\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\n\
                 [env]\nFAKE_PORT = \"{port}\"\nFAKE_EVENTS = \"{}\"\nFAKE_DRAIN = \"{drain}\"\n\
                 [build]\nartifact = \"{}\"\nbin = \"fake-daemon\"\n",
                self.root.join("events").display(),
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

    /// The pid the seam table records for `name` — written only once the
    /// launcher has seen the child listening, so a signal sent before it
    /// lands inside the launch itself, not in a running service.
    fn row_pid(&self, name: &str) -> Option<u32> {
        let t = std::fs::read_to_string(self.root.join("data").join(&self.env).join("seams.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&t).ok()?;
        v["seams"][name]["pid"].as_u64().map(|p| p as u32)
    }

    /// The services' own timeline: `(word, pid)` in the order written.
    fn events(&self) -> Vec<(String, u32)> {
        std::fs::read_to_string(self.root.join("events"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let mut p = l.split_whitespace();
                Some((p.next()?.to_string(), p.next()?.parse().ok()?))
            })
            .collect()
    }

    fn has(&self, word: &str, pid: u32) -> bool {
        self.events().iter().any(|(w, p)| w == word && *p == pid)
    }

    /// The pid of the incarnation that wrote `up` most recently.
    fn last_up(&self) -> Option<u32> {
        self.events().iter().rev().find(|(w, _)| w == "up").map(|(_, p)| *p)
    }

    /// The claim itself, read off the processes' own timeline: no
    /// incarnation wrote `up` while an earlier one had not yet exited.
    fn assert_never_two_at_once(&self) {
        let ev = self.events();
        let mut running: Vec<u32> = vec![];
        for (w, p) in &ev {
            match w.as_str() {
                "up" => {
                    assert!(
                        running.is_empty(),
                        "pid {p} came UP while {running:?} had not exited — two instances on one \
                         data directory. timeline: {ev:?}\ndaemon log:\n{}",
                        self.log()
                    );
                    running.push(*p);
                }
                "down" | "second-signal" => running.retain(|x| x != p),
                _ => {}
            }
        }
    }
}

// ── `cauldron stop` ─────────────────────────────────────────────────────

/// The red on the old logic: `stop` returned ~50 ms after the port went
/// quiet, with the pid still draining. The drain (3 s) is the longest period
/// in the path and the stop is timed against it.
#[test]
fn stop_waits_for_the_pid_through_a_3s_drain_not_the_quiet_port() {
    let sb = Sandbox::new("stopdrain", "auto_restart = false");
    let port = sb.register("svc", "3", "");
    let (rc, out) = sb.cauldron(&["start", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let pid = wait_for("svc up", BUDGET, || sb.last_up().filter(|_| listening(port)));

    let t0 = Instant::now();
    let (rc, out) = sb.cauldron(&["stop", "svc"]);
    let took = t0.elapsed();
    assert_eq!(rc, 0, "{out}");
    assert!(
        !alive(pid),
        "`stop` returned after {took:?} saying stopped while pid {pid} is still running (draining):\n{out}"
    );
    assert!(sb.has("down", pid), "pid {pid} must have finished its drain cleanly: {:?}", sb.events());
    assert!(took >= Duration::from_millis(2500), "stop took {took:?}, shorter than the 3 s drain");
    assert!(!sb.has("second-signal", pid), "the draining service was signalled twice: {:?}", sb.events());
    assert!(!out.contains("SIGKILL"), "{out}");
}

/// Paired (doctrine 2): a service that exits at once is reported stopped at
/// once — the fix is not "always wait out the grace".
#[test]
fn a_service_that_exits_promptly_is_stopped_promptly() {
    let sb = Sandbox::new("stopprompt", "auto_restart = false");
    let port = sb.register("svc", "0", "");
    let (rc, out) = sb.cauldron(&["start", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let pid = wait_for("svc up", BUDGET, || sb.last_up().filter(|_| listening(port)));
    let t0 = Instant::now();
    let (rc, out) = sb.cauldron(&["stop", "svc"]);
    let took = t0.elapsed();
    assert_eq!(rc, 0, "{out}");
    assert!(!alive(pid), "{out}");
    assert!(sb.has("down", pid), "{:?}", sb.events());
    assert!(took < Duration::from_secs(3), "a prompt exit took {took:?} to be reported (grace is 15 s):\n{out}");
}

// ── the grace is the declaration's ────────────────────────────────────────

/// Doctrine 4: the period outlasted is the OLD 15 s grace. A 20 s drain
/// under a declared 25 s grace finishes cleanly — not SIGKILLed at 15 s.
#[test]
fn a_20s_drain_past_the_old_15s_grace_is_not_sigkilled_under_a_declared_25s_grace() {
    let sb = Sandbox::new("grace25", "auto_restart = false");
    let port = sb.register("svc", "20", "stop_grace_secs = 25");
    let (rc, out) = sb.cauldron(&["start", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let pid = wait_for("svc up", BUDGET, || sb.last_up().filter(|_| listening(port)));
    let t0 = Instant::now();
    let (rc, out) = sb.cauldron(&["stop", "svc"]);
    let took = t0.elapsed();
    assert_eq!(rc, 0, "{out}");
    assert!(took > Duration::from_secs(15), "the stop must have outlasted the old 15 s grace; took {took:?}");
    assert!(!out.contains("SIGKILL"), "a 20 s drain under a 25 s grace was escalated:\n{out}");
    assert!(
        sb.has("down", pid),
        "pid {pid} did not finish its 20 s drain cleanly when stop returned (SIGKILLed, or stop \
         returned early): {:?}\n{out}",
        sb.events()
    );
    assert!(!alive(pid));
}

/// Paired: the key is READ — a 6 s drain under a declared 2 s grace IS
/// escalated, so a constant long grace would fail here.
#[test]
fn a_6s_drain_under_a_declared_2s_grace_is_sigkilled() {
    let sb = Sandbox::new("grace2", "auto_restart = false");
    let port = sb.register("svc", "6", "stop_grace_secs = 2");
    let (rc, out) = sb.cauldron(&["start", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let pid = wait_for("svc up", BUDGET, || sb.last_up().filter(|_| listening(port)));
    let (rc, out) = sb.cauldron(&["stop", "svc"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("SIGKILL"), "a 6 s drain under a 2 s grace must be escalated:\n{out}");
    assert!(!alive(pid), "{out}");
    assert!(!sb.has("down", pid), "it was killed, so it never wrote `down`: {:?}", sb.events());
}

// ── restarts: never two instances at once ────────────────────────────────

/// `cauldron restart` with no daemon holding the environment.
#[test]
fn a_restart_does_not_start_a_second_instance_while_the_old_one_drains_3s() {
    let sb = Sandbox::new("restartcli", "auto_restart = false");
    let port = sb.register("svc", "3", "");
    let (rc, out) = sb.cauldron(&["start", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let old = wait_for("svc up", BUDGET, || sb.last_up().filter(|_| listening(port)));
    let (rc, out) = sb.cauldron(&["restart", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let new = wait_for("a new incarnation up", BUDGET, || sb.last_up().filter(|p| *p != old && listening(port)));
    assert_ne!(old, new);
    sb.assert_never_two_at_once();
    assert!(sb.has("down", old), "{:?}", sb.events());
}

/// The daemon's DOWN path. The test sends SIGTERM itself — a drain cauldron
/// did not start — and the port goes quiet for 4 s while the pid lives. The
/// daemon reads DOWN after one 1 s poll; before C-1 it launched a second
/// instance at once. Now it waits for the pid, WITHOUT signalling it again,
/// and only then launches. The daemon is the fake's PARENT here, so the old
/// pid is a zombie between its exit and the reap: this also proves a zombie
/// reads as gone (else the stop would wait out the 15 s grace and SIGKILL).
#[test]
fn the_daemon_waits_out_a_4s_drain_it_did_not_start_before_relaunching() {
    let mut sb = Sandbox::new("downpath", "auto_restart = true\nauto_rollback = false\npoll_secs = 1\ndown_after = 1\nbackoff_base_secs = 1\nstable_after_secs = 600");
    let port = sb.register("svc", "4", "");
    sb.start_daemon();
    let old = wait_for("svc up under the daemon, its row written", BUDGET, || {
        sb.last_up().filter(|p| listening(port) && sb.row_pid("svc") == Some(*p))
    });
    let t0 = Instant::now();
    assert!(Command::new("/bin/kill").args(["-TERM", &old.to_string()]).status().unwrap().success());
    let new = wait_for("relaunched", BUDGET, || sb.last_up().filter(|p| *p != old && listening(port)));
    let took = t0.elapsed();
    assert_ne!(old, new);
    sb.assert_never_two_at_once();
    assert!(sb.has("down", old), "the old pid must have finished its drain: {:?}", sb.events());
    assert!(!sb.has("second-signal", old), "the draining service was re-signalled: {:?}\n{}", sb.events(), sb.log());
    assert!(took < Duration::from_secs(14), "relaunch took {took:?}: the zombie was waited on past the drain\n{}", sb.log());
    assert!(!sb.log().contains("SIGKILL"), "{}", sb.log());
    // The DOWN path WAITED for the pid; it did not try to launch on top of the
    // drain and get refused (that refusal counts toward rollback and park).
    assert!(!sb.log().contains("failed to start"), "a launch was attempted during the drain:\n{}", sb.log());
}

/// The daemon's RESTART path (`restart_now`), with the DOWN path held off by
/// `down_after = 30` on a 1 s poll (a queued RESTART is served on the next
/// pass, so the poll must be short): the old pid is draining with its port
/// already quiet when the restart arrives. Before C-1 `restart_now` skipped
/// the stop (nothing listening) and launched on top of the drain.
#[test]
fn a_requested_restart_during_a_4s_drain_waits_for_the_pid_without_resignalling() {
    let mut sb = Sandbox::new("restartnow", "auto_restart = true\nauto_rollback = false\npoll_secs = 1\ndown_after = 30\nstable_after_secs = 600");
    let port = sb.register("svc", "4", "");
    // `start`, then the daemon, which re-attaches it on its first pass. (Left
    // to the daemon, the FIRST launch also waits out `down_after` = 30 polls.)
    let (rc, out) = sb.cauldron(&["start", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let old = wait_for("svc up, its row written", BUDGET, || {
        sb.last_up().filter(|p| listening(port) && sb.row_pid("svc") == Some(*p))
    });
    sb.start_daemon();
    wait_for("the daemon to re-attach svc", BUDGET, || sb.log().contains("re-attached `svc`").then_some(()));
    assert!(Command::new("/bin/kill").args(["-TERM", &old.to_string()]).status().unwrap().success());
    wait_for("the port to go quiet", BUDGET, || (!listening(port)).then_some(()));
    assert!(alive(old), "premise: the old pid is still draining when the restart arrives");
    let (rc, out) = sb.cauldron(&["restart", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let new = wait_for("restarted", BUDGET, || sb.last_up().filter(|p| *p != old && listening(port)));
    assert_ne!(old, new);
    sb.assert_never_two_at_once();
    assert!(sb.has("down", old), "{:?}", sb.events());
    assert!(!sb.has("second-signal", old), "the draining service was re-signalled: {:?}\n{}", sb.events(), sb.log());
}

// ── R9.C1 inspect F1/F2: the verbs that read a quiet port as "not running" ──

/// Samples, every 50 ms on its own thread, how many of the services' own
/// `up` pids are alive at once (`kill -0`), until told to stop. Returns the
/// largest count seen and the sample it was seen at.
fn sample_live_pids(sb_root: PathBuf, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) -> std::thread::JoinHandle<(usize, Vec<u32>)> {
    std::thread::spawn(move || {
        let mut worst = (0usize, vec![]);
        while !stop.load(std::sync::atomic::Ordering::SeqCst) {
            let ups: Vec<u32> = std::fs::read_to_string(sb_root.join("events"))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| {
                    let mut p = l.split_whitespace();
                    (p.next()? == "up").then_some(())?;
                    p.next()?.parse().ok()
                })
                .collect();
            let live: Vec<u32> = ups.into_iter().filter(|p| alive(*p)).collect();
            if live.len() > worst.0 {
                worst = (live.len(), live);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        worst
    })
}

/// F1: the inspector's reproduction. A drain cauldron did not start (the test
/// sends SIGTERM; the port goes quiet, the pid lives 6 s), then `cauldron
/// stop`, then `cauldron start` at once. Before the fix `stop` CLEARED the
/// row as stale and exited 0 with the pid alive, and `start` — its backstop
/// reading a row that no longer existed — forked a second instance. Claim:
/// at EVERY 50 ms sample, at most one of the services' pids is alive.
#[test]
fn stop_then_start_during_a_6s_drain_never_has_two_live_pids_at_any_sample() {
    let sb = Sandbox::new("stopstart", "auto_restart = false");
    let port = sb.register("svc", "6", "");
    let (rc, out) = sb.cauldron(&["start", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let old = wait_for("svc up, its row written", BUDGET, || {
        sb.last_up().filter(|p| listening(port) && sb.row_pid("svc") == Some(*p))
    });
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = sample_live_pids(sb.root.clone(), stop.clone());
    assert!(Command::new("/bin/kill").args(["-TERM", &old.to_string()]).status().unwrap().success());
    wait_for("the port to go quiet", BUDGET, || (!listening(port)).then_some(()));
    assert!(alive(old), "premise: the old pid is draining when `stop` runs");
    let (rc_stop, out_stop) = sb.cauldron(&["stop", "svc"]);
    let alive_after_stop = alive(old);
    let (rc_start, out_start) = sb.cauldron(&["start", "svc"]);
    // Sample until the old pid has exited AND a new one is up (or 10 s more).
    let t0 = Instant::now();
    while (alive(old) || sb.last_up() == Some(old)) && t0.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let (worst, which) = sampler.join().unwrap();
    assert!(
        worst <= 1,
        "{worst} live pids at one sample: {which:?}. stop rc={rc_stop} alive_after_stop={alive_after_stop}:\n{out_stop}\nstart rc={rc_start}:\n{out_start}\ntimeline {:?}",
        sb.events()
    );
    assert_eq!(rc_stop, 0, "{out_stop}");
    assert!(!alive_after_stop, "`stop` returned with the draining pid still alive:\n{out_stop}");
    assert!(sb.has("down", old), "the drain must have finished cleanly: {:?}", sb.events());
    assert!(!sb.has("second-signal", old), "the draining service was re-signalled: {:?}", sb.events());
    // Paired: the start that followed DID start one (the fix is not "never start").
    assert_eq!(rc_start, 0, "{out_start}");
    let new = wait_for("the new instance up", BUDGET, || sb.last_up().filter(|p| *p != old && listening(port)));
    assert_ne!(new, old);
}

/// F2: `cauldron down` during a drain. Before the fix it read the quiet port
/// as "nothing is running" and returned with the pid still draining on the
/// data root. Paired with the drain finishing cleanly (no second signal).
#[test]
fn down_during_a_4s_drain_waits_for_the_pid_and_does_not_report_nothing_running() {
    let sb = Sandbox::new("downverb", "auto_restart = false");
    let port = sb.register("svc", "4", "");
    let (rc, out) = sb.cauldron(&["start", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let old = wait_for("svc up, its row written", BUDGET, || {
        sb.last_up().filter(|p| listening(port) && sb.row_pid("svc") == Some(*p))
    });
    assert!(Command::new("/bin/kill").args(["-TERM", &old.to_string()]).status().unwrap().success());
    wait_for("the port to go quiet", BUDGET, || (!listening(port)).then_some(()));
    assert!(alive(old), "premise: draining");
    let (rc, out) = sb.cauldron(&["down"]);
    assert_eq!(rc, 0, "{out}");
    assert!(!out.contains("nothing is running"), "`down` called a draining service not running:\n{out}");
    assert!(!alive(old), "`down` returned with pid {old} still draining:\n{out}");
    assert!(sb.has("down", old), "{:?}", sb.events());
    assert!(!sb.has("second-signal", old), "{:?}", sb.events());
}
