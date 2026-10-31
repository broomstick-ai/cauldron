//! **C-1b: AN OWNED ROW WHOSE PID IS ALIVE IS NEVER CLEARED UNSIGNALLED, NEVER
//! LAUNCHED OVER, AND STOPPED BEFORE A RESTART — WHATEVER ITS PROBE STATE.**
//! (R9.C1 second inspection, finding N1.)
//!
//! Before C-1b, `stop`, the launch backstop and `restart` read "is the
//! process gone?" off the PROBE STATE for every state but `stale:no-listener`.
//! An owned row whose service answered `unhealthy` (or `unknown:timeout`, or
//! `stale:wrong-*`) fell into `stop`'s "stale seam" arm, which CLEARED the row
//! with no signal and left the process running; `start` cleared it as stale
//! and forked a second instance; `restart` skipped the stop and did the same.
//!
//! The fake is a REGISTERING service (`registers = true`), because that is the
//! launch path with no bind check: a hosted service's quiet-port-only backstop
//! was covered by `launch_hosted` refusing a held port, a registering one was
//! not. It writes its OWN row (pid source `self`, so cauldron owns it) with
//! `probe = "http"` and its health URL on its own socket, answers `GET
//! /health` 200 — or 500 while the file `sick` names ITS pid (so a fresh
//! incarnation is healthy) — and answers `WARNINGS` like any producer. On
//! SIGTERM it closes its listener, writes `draining`, sleeps `FAKE_DRAIN`
//! seconds, writes `down` and exits 0; a SECOND SIGTERM writes `second-signal`
//! and exits 9. Every incarnation writes `up`. The EVENTS file is written by
//! the processes themselves, never by cauldron or this test (doctrine 3).
//!
//! Every test runs a THROWAWAY cauldron (own `CAULDRON_HOME` under the temp
//! dir, own environment, kernel-assigned ports). Nothing touches
//! `~/.cauldron`, a live daemon or launchd.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

const FAKE: &str = r#"#!/usr/bin/env python3
import json, os, signal, socket, sys, threading, time
if sys.argv[1:] == ["--build-stamp"]:
    print("commit=c1bc1bc1bc1bc1bc1bc1bc1bc1bc1bc1bc1bc1bc provenance=pin dirty=0 epoch=1")
    sys.exit(0)
E = os.environ
ev = E["FAKE_EVENTS"]
sick = E["FAKE_SICK"]
drain = float(E.get("FAKE_DRAIN", "0"))
def note(word):
    with open(ev, "a") as f:
        f.write("%s %d %.3f\n" % (word, os.getpid(), time.time()))
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
host, port = E["CAULDRON_BIND"].rsplit(":", 1)
s.bind((host, int(port))); s.listen(16)
addr = "%s:%d" % s.getsockname()
seams = E["CAULDRON_SEAMS"]
lock = seams[: -len(".json")] + ".lock" if seams.endswith(".json") else seams + ".lock"
root = os.path.normpath(E["CAULDRON_DATA_ROOT"])
while True:
    try:
        fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY); break
    except FileExistsError:
        time.sleep(0.02)
try:
    try:
        t = json.load(open(seams))
    except FileNotFoundError:
        t = {"table_version": 1, "env": E["CAULDRON_ENV"], "data_root": root, "seams": {}}
    svc = E["CAULDRON_SERVICE"]
    t["seams"][svc] = {
        "service": svc, "addr": addr, "pid": os.getpid(),
        "boot_token": E["CAULDRON_BOOT_TOKEN"], "version": E["CAULDRON_VERSION"],
        "commit": E["CAULDRON_COMMIT"], "dirty": E["CAULDRON_DIRTY"] in ("1", "true"),
        "binary": E.get("CAULDRON_BINARY", ""), "env": E["CAULDRON_ENV"], "data_root": root,
        "data_dir": E["CAULDRON_SERVICE_DATA_DIR"], "started_at": int(time.time()),
        "probe": "http", "health": "http://%s/health" % addr,
        "registered": True, "pid_source": "self",
    }
    tmp = seams + ".tmp.%d" % os.getpid()
    with open(tmp, "w") as f:
        json.dump(t, f, indent=2); f.write("\n")
    os.rename(tmp, seams)
finally:
    os.close(fd); os.remove(lock)
note("up")
since = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
def is_sick():
    try:
        return open(sick).read().strip() == str(os.getpid())
    except OSError:
        return False
def serve(c):
    try:
        c.settimeout(1.0)
        f = c.makefile("rb")
        line = f.readline().decode(errors="replace").split()
        if line[:1] == ["WARNINGS"]:
            shape = line[1] if len(line) > 1 else "warnings"
            c.sendall((json.dumps({"shape": shape, "shape_version": 1, "producer": svc,
                                   "counts_since": since, "warn": 0, "error": 0}) + "\n").encode())
        elif line[:1] == ["GET"]:
            while f.readline() not in (b"\r\n", b"\n", b""):
                pass
            code = "500 Internal Server Error" if is_sick() else "200 OK"
            body = b"sick" if is_sick() else b"ok"
            c.sendall(("HTTP/1.1 %s\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % (code, len(body))).encode() + body)
    except Exception:
        pass
    finally:
        c.close()
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
    except OSError:
        time.sleep(0.05)
        continue
    threading.Thread(target=serve, args=(c,), daemon=True).start()
"#;

struct Sandbox {
    root: PathBuf,
    env: String,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // Only the fakes THIS sandbox started: every pid in its own timeline.
        for (_, pid) in self.events() {
            let _ = Command::new("/bin/kill").args(["-9", &pid.to_string()]).stderr(Stdio::null()).status();
        }
        if std::env::var_os("KEEP_SANDBOX").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn alive(pid: u32) -> bool {
    // A zombie answers `kill -0`; `ps -o stat=` tells it apart.
    let out = Command::new("/bin/ps").args(["-o", "stat=", "-p", &pid.to_string()]).output();
    match out {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            !s.is_empty() && !s.starts_with('Z')
        }
        Err(_) => false,
    }
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

/// A POSITIVE wait's bound (hangs only): the declared start timeout
/// ([`launch_wait::START_TIMEOUT`], 60 s, R9.F2g), a python3 start and a probe
/// under load. Held above the start timeout so a launch that does time out
/// still reds inside it. Never the bound of a negative claim.
const BUDGET: Duration = Duration::from_secs(launch_wait::START_TIMEOUT_SECS + 30);

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "cauldron-c1b-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("c1b{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::create_dir_all(root.join("data").join(&env)).unwrap();
        std::fs::write(root.join("data").join(&env).join("env.toml"), "[supervisor]\nauto_restart = false\n")
            .unwrap();
        Sandbox { root, env }
    }

    fn cauldron(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_cauldron"))
            .args(args)
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .output()
            .expect("spawn cauldron");
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }

    /// Declare, build and pin the registering fake, draining `drain` s on TERM.
    /// The DECLARATION says `probe = "connect"` (a registering service has no
    /// `[launch] health` to name); the ROW the fake writes says `probe =
    /// "http"` with its health URL, and the row is what `seams::probe` reads.
    fn register(&self, name: &str, drain: &str) {
        self.register_script(name, drain, FAKE);
    }

    /// [`Sandbox::register`] with `script` as the fake.
    fn register_script(&self, name: &str, drain: &str, script: &str) {
        let fake = self.root.join("owned-fake");
        std::fs::write(&fake, launch_wait::slow_launch_py(script, &self.root.join("f2g-marks"))).unwrap();
        Command::new("/bin/chmod").args(["+x"]).arg(&fake).status().unwrap();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = true\nprobe = \"connect\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [env]\nFAKE_EVENTS = \"{}\"\nFAKE_SICK = \"{}\"\nFAKE_DRAIN = \"{drain}\"\n\
                 [build]\nartifact = \"{}\"\nbin = \"owned-fake\"\nstamp_args = [\"--build-stamp\"]\n",
                self.root.join("events").display(),
                self.root.join("sick").display(),
                fake.display()
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
    }

    fn seams_path(&self) -> PathBuf {
        self.root.join("data").join(&self.env).join("seams.json")
    }

    fn row(&self, name: &str) -> Option<serde_json::Value> {
        let t = std::fs::read_to_string(self.seams_path()).ok()?;
        let v: serde_json::Value = serde_json::from_str(&t).ok()?;
        let r = v["seams"][name].clone();
        (!r.is_null()).then_some(r)
    }

    fn row_pid(&self, name: &str) -> Option<u32> {
        self.row(name)?["pid"].as_u64().map(|p| p as u32)
    }

    /// The probe's own word for the row, read through cauldron's `status`
    /// JSON — the same `seams::probe` the verbs read.
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

    fn ups(&self) -> Vec<u32> {
        self.events().iter().filter(|(w, _)| w == "up").map(|(_, p)| *p).collect()
    }

    /// Start the fake through `cauldron start`, then make THAT pid answer its
    /// health check 500, and wait until the probe says `unhealthy`. Returns
    /// the pid. The premise every test below stands on, asserted.
    fn start_unhealthy(&self, name: &str) -> u32 {
        let (rc, out) = self.cauldron(&["start", name]);
        assert_eq!(rc, 0, "start {name}: {out}");
        let pid = wait_for("the fake up with its row written", BUDGET, || {
            let p = *self.ups().last()?;
            (self.row_pid(name) == Some(p)).then_some(p)
        });
        std::fs::write(self.root.join("sick"), pid.to_string()).unwrap();
        wait_for("the probe to read the owned row as unhealthy", BUDGET, || {
            (self.state(name) == "unhealthy").then_some(())
        });
        assert!(alive(pid), "premise: the owned pid is alive");
        assert_eq!(self.row(name).unwrap()["pid_source"], "self", "premise: the row is owned");
        pid
    }

    /// No incarnation wrote `up` while an earlier one had not exited — read
    /// off the processes' own timeline.
    fn assert_never_two_at_once(&self) {
        let ev = self.events();
        let mut running: Vec<u32> = vec![];
        for (w, p) in &ev {
            match w.as_str() {
                "up" => {
                    assert!(
                        running.is_empty(),
                        "pid {p} came UP while {running:?} had not exited — two instances on one \
                         data directory. timeline: {ev:?}"
                    );
                    running.push(*p);
                }
                "down" | "second-signal" => running.retain(|x| x != p),
                _ => {}
            }
        }
    }
}

// ── (a) stop ─────────────────────────────────────────────────────────────

/// An OWNED row, pid alive, probe `unhealthy`: `stop` SIGNALS it and WAITS
/// for the pid (a 3 s drain is the longest period in the path, and the stop is
/// timed against it), and the row is cleared only after the pid is gone.
/// Before C-1b: "cleared the stale seam … no signal was sent", rc 0, pid
/// running with no row.
#[test]
fn stop_signals_an_owned_unhealthy_pid_and_waits_out_its_3s_drain_before_clearing_the_row() {
    let sb = Sandbox::new("stopsick");
    sb.register("svc", "3");
    let pid = sb.start_unhealthy("svc");
    let t0 = Instant::now();
    let (rc, out) = sb.cauldron(&["stop", "svc"]);
    let took = t0.elapsed();
    let alive_at_return = alive(pid);
    assert_eq!(rc, 0, "{out}");
    assert!(
        !out.contains("no signal was sent"),
        "`stop` cleared an owned, running row unsignalled:\n{out}"
    );
    assert!(sb.has("draining", pid), "pid {pid} was never signalled: {:?}\n{out}", sb.events());
    assert!(!alive_at_return, "`stop` returned after {took:?} with pid {pid} still running:\n{out}");
    assert!(sb.has("down", pid), "pid {pid} must have finished its drain cleanly: {:?}", sb.events());
    assert!(!sb.has("second-signal", pid), "signalled twice: {:?}", sb.events());
    assert!(took >= Duration::from_millis(2500), "stop took {took:?}, shorter than the 3 s drain:\n{out}");
    assert!(sb.row("svc").is_none(), "the row must be cleared once the pid is gone");
}

// ── (b) start ────────────────────────────────────────────────────────────

/// Same row: `start` REFUSES, forks nothing, and leaves the row and the pid
/// alone. Negative claim bounded by the fake's own start: we wait past the
/// declared `start_timeout_ms`'s worth of a launch by checking after the
/// verb RETURNED (the launch path is synchronous: a fork writes `up` before
/// `start` returns 0), plus 2 s for a straggler.
#[test]
fn start_refuses_over_an_owned_unhealthy_pid_and_forks_nothing() {
    let sb = Sandbox::new("startsick");
    sb.register("svc", "0");
    let pid = sb.start_unhealthy("svc");
    let (rc, out) = sb.cauldron(&["start", "svc"]);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(sb.ups(), vec![pid], "a second instance came up beside pid {pid}:\n{out}\n{:?}", sb.events());
    assert_ne!(rc, 0, "`start` must refuse over an owned live pid:\n{out}");
    assert!(out.contains("STILL RUNNING"), "the refusal names why:\n{out}");
    assert!(alive(pid), "the refusal must not touch the running pid");
    assert_eq!(sb.row_pid("svc"), Some(pid), "the row must be left in place");
    assert!(!sb.has("draining", pid), "nothing was to be signalled: {:?}", sb.events());
}

// ── (c) restart ──────────────────────────────────────────────────────────

/// Same row: `restart` stops it first (signal, wait out its 3 s drain), then
/// launches; at no point are two incarnations up. Before C-1b `restart` only
/// stopped `live`/`live:unverified` rows, so the launch went straight on top.
#[test]
fn restart_stops_an_owned_unhealthy_pid_through_its_3s_drain_before_launching() {
    let sb = Sandbox::new("restartsick");
    sb.register("svc", "3");
    let old = sb.start_unhealthy("svc");
    let (rc, out) = sb.cauldron(&["restart", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let new = wait_for("a new incarnation", BUDGET, || sb.ups().into_iter().find(|p| *p != old));
    sb.assert_never_two_at_once();
    assert!(sb.has("down", old), "the old pid must have drained and exited: {:?}\n{out}", sb.events());
    assert!(!sb.has("second-signal", old), "{:?}", sb.events());
    assert_eq!(sb.row_pid("svc"), Some(new), "the row is the new incarnation's");
    assert_eq!(sb.ups().len(), 2, "exactly one relaunch: {:?}", sb.events());
}

// ── paired: an UNOWNED stale row is still the old lie to clear ──────────

/// Doctrine 2, opposing (a): the SAME `unhealthy` answer from the same
/// listener, but the row's pid is not this row's process any more — its
/// `started_at` predates the process by hours, which is how a REUSED pid reads
/// (`row_pid_alive`). That row is a lie: `stop` clears it and SIGNALS NOTHING.
/// A predicate that ignored the pid (`signalable()` alone) would signal a
/// stranger here.
#[test]
fn stop_clears_a_stale_row_whose_pid_is_not_its_process_and_signals_nothing() {
    let sb = Sandbox::new("unowned");
    sb.register("svc", "0");
    let pid = sb.start_unhealthy("svc");
    // Rewrite the row as one written long before this process existed.
    let mut t: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(sb.seams_path()).unwrap()).unwrap();
    t["seams"]["svc"]["started_at"] = serde_json::json!(1_000_000_000u64);
    std::fs::write(sb.seams_path(), serde_json::to_string_pretty(&t).unwrap()).unwrap();
    assert_eq!(sb.state("svc"), "unhealthy", "premise: the same probe answer as the owned case");
    let (rc, out) = sb.cauldron(&["stop", "svc"]);
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("no signal was sent"), "{out}");
    assert!(sb.row("svc").is_none(), "the stale row must be cleared:\n{out}");
    assert!(alive(pid), "a pid that is not this row's process must not be signalled:\n{out}");
    assert!(!sb.has("draining", pid), "it was signalled: {:?}\n{out}", sb.events());
}

// ── R9.F2g: the raised launch wait is still a bound ───────────────────────

/// A registering fake that answers its build stamp and then, launched, never
/// writes its seam: it sleeps until it is killed.
const NEVER_UP: &str = r#"#!/usr/bin/env python3
import sys, time
if sys.argv[1:] == ["--build-stamp"]:
    print("commit=c1bc1bc1bc1bc1bc1bc1bc1bc1bc1bc1bc1bc1bc provenance=pin dirty=0 epoch=1")
    sys.exit(0)
time.sleep(3600)
"#;

/// OPPOSING (R9.F2g): raising every fixture's launch wait to 60 s
/// ([`launch_wait::START_TIMEOUT_MS`]) must not make a launch unbounded. A
/// service that never writes its seam fails a synchronous `cauldron start`
/// at the declared 60 s — not before it (the wait really is 60 s, not the old
/// 10 s) and not past [`BUDGET`] (90 s: the wait is still a bound, and the
/// verb still kills what it forked). The verb is run on its own deadline, so
/// an unbounded launch reds here instead of hanging the suite.
#[test]
fn f2g_a_launch_that_never_writes_its_seam_fails_at_the_60s_start_timeout_within_90s() {
    let sb = Sandbox::new("neverup");
    sb.register_script("svc", "0", NEVER_UP);
    let t0 = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cauldron"))
        .args(["start", "svc"])
        .env("CAULDRON_HOME", &sb.root)
        .env("CAULDRON_ENV", &sb.env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cauldron start");
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if t0.elapsed() > BUDGET {
            let _ = child.kill();
            let _ = child.wait();
            // The fake the verb forked is in its own process group and would
            // outlive the verb: kill it by this sandbox's unique root.
            let _ = Command::new("/usr/bin/pkill").arg("-f").arg(sb.root.display().to_string()).status();
            panic!("`cauldron start` of a service that never comes up was still waiting after {BUDGET:?}: the launch wait is unbounded");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let took = t0.elapsed();
    let o = child.wait_with_output().unwrap();
    let out = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(!status.success(), "a launch that never wrote its seam was reported started ({took:?}):\n{out}");
    assert!(
        out.contains("did not write its seam") && out.contains("within 60000ms"),
        "not the 60 s start timeout's refusal ({took:?}):\n{out}"
    );
    assert!(took >= Duration::from_secs(60), "failed after {took:?}, before the declared 60 s launch wait:\n{out}");
    assert!(took < BUDGET, "failed after {took:?}, past {BUDGET:?}:\n{out}");
    assert!(sb.row("svc").is_none(), "a row was left for a service that never came up: {:?}", sb.row("svc"));
}
