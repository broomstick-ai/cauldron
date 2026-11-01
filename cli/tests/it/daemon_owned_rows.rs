//! **R9.C1c (ledger R9.C1b-1) — UNDER THE DAEMON, AN OWNED ROW WHOSE PROCESS
//! IS STILL RUNNING IS STOPPED AND RELAUNCHED, WHATEVER ITS PROBE STATE.**
//!
//! C-1b made the launch backstop REFUSE to launch over an owned row whose pid
//! is alive (`Verdict::owned_and_running`), in any probe state. The daemon's
//! two launch sites were not changed with it: the DOWN path in `tend` stopped
//! only `unhealthy` and `exiting` rows first, and `restart_now` only `live`,
//! `live:unverified`, `unhealthy` and `exiting` ones. An owned row in
//! `unknown:timeout` or `stale:wrong-*` therefore went straight to the launch,
//! the backstop refused, and that refusal was counted as a FAILED START: the
//! DOWN path backed off and PARKED the service with its old process still
//! running, and a requested restart failed. Both sites now use
//! `owned_and_running`, so the row is stopped (drained first where the service
//! declares a drain door) and relaunched.
//!
//! The fake is a REGISTERING service (it writes its own row, pid source
//! `self`, so cauldron owns it) with `probe = "seam"`, because only the HELLO
//! handshake can yield `unknown:timeout` and `stale:wrong-*`: a hosted row has
//! no boot token and tops out at `live:unverified`. While the file `mute`
//! names ITS pid it accepts HELLO and says nothing (the probe's 1.5 s read
//! times out: `unknown:timeout`); while `wrong` names its pid it answers with
//! a different boot token (`stale:wrong-owner`). A fresh incarnation has a new
//! pid, so it answers normally. On SIGTERM it records whether a turn was
//! running (`term-busy` / `term-idle`), closes its listener, records
//! `draining`, sleeps `FAKE_DRAIN` s, records `down` and exits 0; a SECOND
//! SIGTERM records `second-signal` and exits 9. With a declared `[quiet]` it
//! serves threads' drain door on `GET /gate` (`busy` file = one running turn)
//! and records `drain-begin` on SIGUSR1. The `events` file is written only by
//! the fake processes (doctrine 3).
//!
//! THE LONGEST PERIOD on these paths is the pass (`poll_secs` = 1) times
//! `down_after` = 3, plus the probe's 1.5 s HELLO timeout per pass; every
//! negative claim is asserted after the daemon logged its decision and again
//! after `HOLD` = 6 s. Every test runs a THROWAWAY cauldron (own
//! `CAULDRON_HOME` under the temp dir, own environment, kernel-assigned
//! ports); nothing touches `~/.cauldron`, a live daemon or launchd.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

const FAKE: &str = r#"#!/usr/bin/env python3
import json, os, signal, socket, sys, threading, time
if sys.argv[1:] == ["--build-stamp"]:
    print("commit=c1cc1cc1cc1cc1cc1cc1cc1cc1cc1cc1cc1cc1cc provenance=pin dirty=0 epoch=1")
    sys.exit(0)
E = os.environ
root = E["FAKE_ROOT"]
def P(n): return os.path.join(root, n)
drain = float(E.get("FAKE_DRAIN", "0"))
def note(word):
    with open(P("events"), "a") as f:
        f.write("%s %d %.3f\n" % (word, os.getpid(), time.time()))
def owner_watch(owner):
    while True:
        time.sleep(1)
        try: os.kill(owner, 0)
        except ProcessLookupError: os._exit(0)
        except PermissionError: pass
threading.Thread(target=owner_watch, args=(int(E["FAKE_OWNER"]),), daemon=True).start()
def names_me(f):
    try: return open(P(f)).read().strip() == str(os.getpid())
    except OSError: return False
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
host, port = E["CAULDRON_BIND"].rsplit(":", 1)
s.bind((host, int(port))); s.listen(16)
addr = "%s:%d" % s.getsockname()
seams = E["CAULDRON_SEAMS"]
lock = seams[: -len(".json")] + ".lock" if seams.endswith(".json") else seams + ".lock"
droot = os.path.normpath(E["CAULDRON_DATA_ROOT"])
svc = E["CAULDRON_SERVICE"]
while True:
    try:
        fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY); break
    except FileExistsError:
        time.sleep(0.02)
try:
    try:
        t = json.load(open(seams))
    except FileNotFoundError:
        t = {"table_version": 1, "env": E["CAULDRON_ENV"], "data_root": droot, "seams": {}}
    t["seams"][svc] = {
        "service": svc, "addr": addr, "pid": os.getpid(),
        "boot_token": E["CAULDRON_BOOT_TOKEN"], "version": E["CAULDRON_VERSION"],
        "commit": E["CAULDRON_COMMIT"], "dirty": E["CAULDRON_DIRTY"] in ("1", "true"),
        "binary": E.get("CAULDRON_BINARY", ""), "env": E["CAULDRON_ENV"], "data_root": droot,
        "data_dir": E["CAULDRON_SERVICE_DATA_DIR"], "started_at": int(time.time()),
        "probe": "seam", "registered": True, "pid_source": "self",
    }
    tmp = seams + ".tmp.%d" % os.getpid()
    with open(tmp, "w") as f:
        json.dump(t, f, indent=2); f.write("\n")
    os.rename(tmp, seams)
finally:
    os.close(fd); os.remove(lock)
note("up")
counts_since = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
state = {"draining": False, "since": None}
def on_usr1(*_):
    if state["since"] is None: state["since"] = int(time.time())
    note("drain-begin")
def on_usr2(*_):
    state["since"] = None
    note("drain-cancel")
signal.signal(signal.SIGUSR1, on_usr1)
signal.signal(signal.SIGUSR2, on_usr2)
def serve(c):
    try:
        c.settimeout(10.0)
        f = c.makefile("rb")
        line = f.readline().decode(errors="replace").split()
        if line[:1] == ["WARNINGS"]:
            shape = line[1] if len(line) > 1 else "warnings"
            c.sendall((json.dumps({"shape": shape, "shape_version": 1, "producer": svc,
                                   "counts_since": counts_since, "warn": 0, "error": 0}) + "\n").encode())
        elif line[:1] == ["HELLO"]:
            if names_me("mute"):
                time.sleep(4)
                return
            tok = E["CAULDRON_BOOT_TOKEN"]
            if names_me("wrong"):
                tok = "not-" + tok
            c.sendall((json.dumps({"service": svc, "env": E["CAULDRON_ENV"], "data_root": droot,
                                   "boot_token": tok, "version": E["CAULDRON_VERSION"],
                                   "commit": E["CAULDRON_COMMIT"], "pid": os.getpid(),
                                   "addr": addr}) + "\n").encode())
        elif line[:1] == ["GET"]:
            while f.readline() not in (b"\r\n", b"\n", b""):
                pass
            busy = os.path.exists(P("busy"))
            gate = {"idle": not busy, "turns": 1 if busy else 0, "draining_since": state["since"]}
            body = json.dumps({"status": "ok", "restart_gate": gate}).encode()
            c.sendall(b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n" + body)
    except Exception:
        pass
    finally:
        c.close()
def on_term(*_):
    if state["draining"]:
        note("second-signal")
        os._exit(9)
    state["draining"] = True
    note("term-busy" if os.path.exists(P("busy")) else "term-idle")
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

/// `auto_restart` is the one knob the tests turn: on for the DOWN path, off
/// for the requested restart (so the DOWN path stands down and the request is
/// the only actor).
fn policy(auto_restart: bool) -> String {
    format!(
        "auto_restart = {auto_restart}\nauto_rollback = false\npoll_secs = 1\ndown_after = 3\n\
         max_start_failures = 3\nbackoff_base_secs = 1\nbackoff_max_secs = 5\nstable_after_secs = 600"
    )
}

const BUDGET: Duration = Duration::from_secs(120);
// The fake's declared `start_timeout_ms` is the shared launch wait,
// [`launch_wait::START_TIMEOUT_MS`] (60 s): R9.F2f raised it here first,
// R9.F2g moved it to `support/launch_wait.rs` for every cli test file. Held
// under `BUDGET` so a launch that does time out still reds in time.
/// Six `poll_secs = 1` passes (each also waits out a 1.5 s HELLO timeout).
const HOLD: Duration = Duration::from_secs(6);

const DOOR: &str = "[quiet]\npath = \"/gate\"\npointer = \"/restart_gate/idle\"\n\
                    [quiet.drain]\nstart = \"USR1\"\ncancel = \"USR2\"\n\
                    since = \"/restart_gate/draining_since\"\nrunning = \"/restart_gate/turns\"\n\
                    cap_secs = 600\nat_cap = \"handoff\"\n";

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
        // Only the fakes THIS sandbox started: every pid in its own timeline,
        // then anything still running out of its own directory.
        for (_, pid) in self.events() {
            let _ = Command::new("/bin/kill").args(["-9", &pid.to_string()]).stderr(Stdio::null()).status();
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

impl Sandbox {
    fn new(tag: &str, auto_restart: bool) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "cauldron-c1c-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("c1c{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::create_dir_all(root.join("data").join(&env)).unwrap();
        std::fs::write(
            root.join("data").join(&env).join("env.toml"),
            format!("[supervisor]\n{}\n", policy(auto_restart)),
        )
        .unwrap();
        std::fs::write(root.join("owned-fake"), launch_wait::slow_launch_py(FAKE, &root.join("f2g-marks"))).unwrap();
        Command::new("/bin/chmod").arg("+x").arg(root.join("owned-fake")).status().unwrap();
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

    /// Declare, build and pin the registering fake (`probe = "seam"`),
    /// draining `drain` s on TERM, with `quiet` appended to its declaration.
    fn register(&self, name: &str, drain: &str, quiet: &str) {
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = true\nprobe = \"seam\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [env]\nFAKE_ROOT = \"{}\"\nFAKE_DRAIN = \"{drain}\"\nFAKE_OWNER = \"{}\"\n\
                 {quiet}\n\
                 [build]\nartifact = \"{}\"\nbin = \"owned-fake\"\nstamp_args = [\"--build-stamp\"]\n",
                self.root.display(),
                std::process::id(),
                self.root.join("owned-fake").display()
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

    /// Where the daemon's log ends now: a mark for [`Sandbox::log_since`].
    fn log_mark(&self) -> usize {
        std::fs::metadata(&self.log).map(|m| m.len() as usize).unwrap_or(0)
    }

    /// What the daemon said after `mark` (bytes; lossy at the cut).
    fn log_since(&self, mark: usize) -> String {
        let b = std::fs::read(&self.log).unwrap_or_default();
        String::from_utf8_lossy(b.get(mark..).unwrap_or(&[])).into_owned()
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

    fn launches_why(&self, name: &str, why: &str) -> usize {
        std::fs::read_to_string(self.root.join("data").join(&self.env).join("supervisions.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|r| r["service"] == name && r["act"] == "launch" && r["why"] == why)
            .count()
    }

    /// v1 up under the daemon, with its row owned and written by the process.
    fn up_under_daemon(&mut self, name: &str) -> u32 {
        self.start_daemon();
        let pid = wait_for("the fake up under the daemon with its row written", BUDGET, || {
            let p = *self.ups().last()?;
            (self.row_pid(name) == Some(p)).then_some(p)
        });
        wait_for("the probe to read it live", BUDGET, || (self.state(name) == "live").then_some(()));
        assert_eq!(self.row(name).unwrap()["pid_source"], "self", "premise: the row is owned");
        // The fake writes its row before the daemon appends the first launch's
        // record; a count read before then is off by one.
        wait_for("the first launch's record", BUDGET, || (self.launches_why(name, "down") == 1).then_some(()));
        pid
    }

    /// Make `pid` answer HELLO as `mode` (`mute` / `wrong`) and wait until the
    /// probe reads `want`, with the pid alive: the premise, asserted.
    fn degrade(&self, name: &str, pid: u32, mode: &str, want: &str) {
        std::fs::write(self.root.join(mode), pid.to_string()).unwrap();
        wait_for(&format!("the probe to read the owned row as {want}"), BUDGET, || {
            (self.state(name) == want).then_some(())
        });
        assert!(alive(pid), "premise: the owned pid is alive");
    }

    /// The next incarnation after `old` — or, as soon as the daemon says the
    /// launch FAILED (the C-1b refusal counted as a failed start) or parked
    /// the service, a red naming that. Only what the daemon said since
    /// `mark` (taken once the premise held, before the row was degraded)
    /// counts: the claim is about the relaunch, and a FIRST launch that was
    /// slow under load said "failed to start" before the premise existed
    /// (R9.F2f; ledger R9.C1d-1).
    fn relaunched_after(&self, old: u32, mark: usize) -> u32 {
        wait_for("a new incarnation", BUDGET, || {
            let log = self.log_since(mark);
            for bad in ["failed to start", "PARKED", "restarting `svc`:"] {
                assert!(!log.contains(bad), "the daemon said `{bad}` instead of relaunching:\n{log}\n{:?}", self.events());
            }
            self.ups().into_iter().find(|p| *p != old)
        })
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

// ── the DOWN path in `tend` ─────────────────────────────────────────────

fn down_path_relaunches(tag: &str, mode: &str, want: &str) {
    let mut sb = Sandbox::new(tag, true);
    sb.register("svc", "2", "");
    let old = sb.up_under_daemon("svc");
    let downs = sb.launches_why("svc", "down");
    let mark = sb.log_mark();
    sb.degrade("svc", old, mode, want);
    let new = sb.relaunched_after(old, mark);
    sb.assert_never_two_at_once();
    assert!(sb.has("draining", old), "pid {old} was never signalled: {:?}", sb.events());
    assert!(sb.has("down", old), "pid {old} must have finished its 2 s drain: {:?}", sb.events());
    assert!(!sb.has("second-signal", old), "signalled twice: {:?}", sb.events());
    wait_for("the new incarnation's row", BUDGET, || (sb.row_pid("svc") == Some(new)).then_some(()));
    // The fake writes its row before the daemon appends the launch record.
    wait_for("the relaunch's `launch` record", BUDGET, || (sb.launches_why("svc", "down") > downs).then_some(()));
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(sb.launches_why("svc", "down"), downs + 1, "exactly one relaunch:\n{}", sb.log());
    assert_eq!(sb.ups().len(), 2, "{:?}", sb.events());
}

/// An owned row answering `unknown:timeout` (its HELLO goes unanswered past
/// the probe's 1.5 s) is, after `down_after` = 3 passes, STOPPED — signalled,
/// its 2 s drain waited out — and relaunched. At base the launch went on top,
/// the C-1b backstop refused, the daemon logged "failed to start" and after
/// three of those PARKED the service with its old process running.
#[test]
fn c1c_down_an_owned_row_in_unknown_timeout_is_stopped_through_its_2s_drain_and_relaunched() {
    down_path_relaunches("tmo", "mute", "unknown:timeout");
}

/// The same for `stale:wrong-owner` (the listener answers another boot token).
#[test]
fn c1c_down_an_owned_row_in_stale_wrong_owner_is_stopped_through_its_2s_drain_and_relaunched() {
    down_path_relaunches("wro", "wrong", "stale:wrong-owner");
}

/// "With drain where the service declares one": the same `unknown:timeout`
/// row, now declaring threads' drain door with a turn running, is ASKED TO
/// DRAIN and is not stopped while the turn runs (outlasting 6 polls after the
/// ask); when the turn ends it is stopped idle and relaunched. Without the
/// gate covering `owned_and_running`, the pre-stop SIGTERMs it mid-turn
/// (`term-busy`).
#[test]
fn c1c_down_an_owned_timeout_row_with_a_drain_door_drains_its_turn_first_outlasting_6_polls() {
    let mut sb = Sandbox::new("door", true);
    sb.register("svc", "0", DOOR);
    let old = sb.up_under_daemon("svc");
    let mark = sb.log_mark();
    std::fs::write(sb.root.join("busy"), "").unwrap();
    sb.degrade("svc", old, "mute", "unknown:timeout");
    wait_for("the daemon to ask the timed-out row to drain", BUDGET, || {
        assert!(!sb.has("term-busy", old), "stopped mid-turn without a drain:\n{}\n{:?}", sb.log(), sb.events());
        sb.has("drain-begin", old).then_some(())
    });
    std::thread::sleep(HOLD);
    assert!(alive(old), "stopped while its turn ran:\n{}", sb.log());
    assert!(!sb.has("draining", old), "signalled while its turn ran: {:?}", sb.events());
    assert_eq!(sb.ups(), vec![old], "relaunched while its turn ran: {:?}", sb.events());

    std::fs::remove_file(sb.root.join("busy")).unwrap();
    let new = sb.relaunched_after(old, mark);
    sb.assert_never_two_at_once();
    assert!(sb.has("term-idle", old), "{:?}", sb.events());
    assert!(!sb.has("term-busy", old), "{:?}", sb.events());
    assert!(sb.has("down", old), "{:?}", sb.events());
    assert_ne!(new, old);
}

/// Doctrine 2, opposing the DOWN-path claims: the SAME `unknown:timeout`
/// answer from the same listener, but the row's pid is NOT this row's process
/// any more — its `started_at` predates the process by decades, which is how
/// a REUSED pid reads (`row_pid_alive`). That row is not `owned_and_running`:
/// nothing is signalled, the row is cleared as stale and the service is
/// launched. A predicate that read `signalable()` alone would signal a
/// stranger here.
#[test]
fn c1c_down_a_timeout_row_whose_pid_is_not_its_process_is_relaunched_and_its_pid_never_signalled() {
    let mut sb = Sandbox::new("reuse", true);
    sb.register("svc", "0", "");
    let old = sb.up_under_daemon("svc");
    let mut t: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(sb.seams_path()).unwrap()).unwrap();
    t["seams"]["svc"]["started_at"] = serde_json::json!(1_000_000_000u64);
    let tmp = sb.seams_path().with_extension("json.c1c-tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&t).unwrap()).unwrap();
    std::fs::rename(&tmp, sb.seams_path()).unwrap();
    sb.degrade("svc", old, "mute", "unknown:timeout");
    let new = wait_for("a new incarnation", BUDGET, || sb.ups().into_iter().find(|p| *p != old));
    std::thread::sleep(Duration::from_secs(2));
    assert_ne!(new, old);
    assert!(alive(old), "a pid that is not this row's process was stopped:\n{}", sb.log());
    assert!(!sb.has("draining", old), "a pid that is not this row's process was signalled: {:?}", sb.events());
    assert!(!sb.has("drain-begin", old), "{:?}", sb.events());
}

// ── `restart_now` ───────────────────────────────────────────────────────

/// `cauldron restart` on an owned row in `stale:wrong-owner`: stopped first
/// (signalled, 2 s drain waited out), then relaunched; never two at once. At
/// base `restart_now` stopped only live/unhealthy/exiting rows, so the launch
/// went on top, the C-1b backstop refused and the daemon logged "restarting
/// `svc`: …" with nothing restarted. `auto_restart = false` here so the DOWN
/// path stands down (asserted) and the request is the only actor.
#[test]
fn c1c_restart_now_stops_an_owned_wrong_owner_row_through_its_2s_drain_then_relaunches() {
    let mut sb = Sandbox::new("rst", false);
    sb.register("svc", "2", "");
    let old = sb.up_under_daemon("svc");
    let mark = sb.log_mark();
    sb.degrade("svc", old, "wrong", "stale:wrong-owner");
    wait_for("the DOWN path to stand down under auto_restart = false", BUDGET, || {
        sb.log_since(mark).contains("`auto_restart = false` here").then_some(())
    });
    assert!(!sb.has("draining", old), "premise: nothing stopped it yet: {:?}", sb.events());
    let (rc, out) = sb.cauldron(&["restart", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let new = sb.relaunched_after(old, mark);
    sb.assert_never_two_at_once();
    assert!(sb.has("draining", old), "{:?}", sb.events());
    assert!(sb.has("down", old), "{:?}", sb.events());
    assert!(!sb.has("second-signal", old), "{:?}", sb.events());
    wait_for("the new incarnation's row", BUDGET, || (sb.row_pid("svc") == Some(new)).then_some(()));
    wait_for("the restart's `launch` record", BUDGET, || (sb.launches_why("svc", "requested") > 0).then_some(()));
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(sb.launches_why("svc", "requested"), 1, "{}", sb.log());
    assert_eq!(sb.ups().len(), 2, "{:?}", sb.events());
}

// ── R9.C1d (R9.C1c inspect F1): the DOWN drain gate's reused-pid check ───
//
// In `tend`, the DOWN path's drain gate (`(unhealthy || owned_running) &&
// !exiting`) runs BEFORE `stop_by_seam`, and `restart_gate` SIGNALS the row's
// pid itself (`[quiet.drain] start`, SIGUSR1 here) when the service has a
// drain door and a turn running. So the `pid_alive` half of
// `owned_and_running` is load-bearing there: a gate that read `signalable()`
// alone would send the drain signal to a REUSED pid — a stranger — and then
// WAIT on it, skipping the relaunch every pass, before `stop_by_seam`'s own
// reused-pid refusal is ever reached. The C1c opposing test declared no door,
// so it could not see this. These two are one opposing pair on the same
// row (owned, `stale:wrong-owner`, drain door, one turn running); only the
// row's `started_at` differs.
//
// THE LONGEST PERIOD on this path is `down_after` = 3 passes of `poll_secs` =
// 1 (no HELLO timeout under `wrong`). The negative claims below are asserted
// after the daemon has already relaunched — i.e. after the gate's decision
// for this row was taken — and again 2 s (two more passes) later.

/// Write the row's `started_at` as 2001: the pid now reads as a process that
/// started decades after its row, which is how a REUSED pid reads
/// (`row_pid_alive` -> false). The process at that pid is the old fake,
/// still alive and still serving the drain door: the stranger.
fn make_row_pid_reused(sb: &Sandbox, name: &str) {
    let mut t: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(sb.seams_path()).unwrap()).unwrap();
    t["seams"][name]["started_at"] = serde_json::json!(1_000_000_000u64);
    let tmp = sb.seams_path().with_extension("json.c1d-tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&t).unwrap()).unwrap();
    std::fs::rename(&tmp, sb.seams_path()).unwrap();
}

/// F1's case: owned `stale:wrong-owner` row, drain door served, a turn
/// running — but the row's pid is REUSED. The gate does not treat it as the
/// live service: the stranger is never asked to drain (no SIGUSR1), never
/// stopped (no SIGTERM), the daemon never says it waits on it, and the
/// relaunch is not skipped on its account.
#[test]
fn c1d_down_gate_never_drains_signals_or_waits_on_a_reused_pid_behind_a_busy_door_and_relaunches() {
    let mut sb = Sandbox::new("c1dreuse", true);
    sb.register("svc", "0", DOOR);
    let old = sb.up_under_daemon("svc");
    std::fs::write(sb.root.join("busy"), "").unwrap();
    make_row_pid_reused(&sb, "svc");
    sb.degrade("svc", old, "wrong", "stale:wrong-owner");
    let new = wait_for("a new incarnation (the relaunch not skipped)", BUDGET, || {
        assert!(!sb.has("drain-begin", old), "the reused pid was asked to DRAIN:\n{}\n{:?}", sb.log(), sb.events());
        sb.ups().into_iter().find(|p| *p != old)
    });
    std::thread::sleep(Duration::from_secs(2));
    assert_ne!(new, old);
    assert!(alive(old), "the reused pid's process was stopped:\n{}", sb.log());
    assert!(!sb.has("drain-begin", old), "the reused pid was asked to DRAIN: {:?}", sb.events());
    assert!(!sb.has("draining", old), "the reused pid was SIGTERMed: {:?}", sb.events());
    let log = sb.log();
    assert!(!log.contains("Asked it to DRAIN"), "the gate signalled the reused pid:\n{log}");
    assert!(!log.contains("waits for the running turns"), "the gate waited on the reused pid:\n{log}");
}

/// Doctrine 2, the other arm: the SAME row, door and running turn, with its
/// genuine `started_at` — the pid IS the service. The gate does treat it as
/// live: asks it to drain, does not stop or relaunch it while the turn runs
/// (outlasting 6 polls of the wrong-owner pass after the ask), and relaunches
/// once it is idle. A gate that never fires on `stale:wrong-owner` fails here.
#[test]
fn c1d_down_gate_drains_the_genuine_owned_pid_behind_a_busy_door_outlasting_6_polls_then_relaunches() {
    let mut sb = Sandbox::new("c1dlive", true);
    sb.register("svc", "0", DOOR);
    let old = sb.up_under_daemon("svc");
    let mark = sb.log().len();
    std::fs::write(sb.root.join("busy"), "").unwrap();
    sb.degrade("svc", old, "wrong", "stale:wrong-owner");
    wait_for("the daemon to ask the owned row to drain", BUDGET, || {
        assert!(!sb.has("term-busy", old), "stopped mid-turn without a drain:\n{}\n{:?}", sb.log(), sb.events());
        assert_eq!(sb.ups(), vec![old], "relaunched without asking it to drain:\n{}\n{:?}", sb.log(), sb.events());
        sb.has("drain-begin", old).then_some(())
    });
    std::thread::sleep(HOLD);
    assert!(alive(old), "stopped while its turn ran:\n{}", sb.log());
    assert!(!sb.has("draining", old), "signalled while its turn ran: {:?}", sb.events());
    assert_eq!(sb.ups(), vec![old], "relaunched while its turn ran: {:?}", sb.events());

    std::fs::remove_file(sb.root.join("busy")).unwrap();
    // Not `relaunched_after`: it scans the WHOLE log, so a first launch that
    // was slow to write its row under load ("failed to start" before the
    // premise was even set up) reds it for a reason this claim is not about.
    // Only what the daemon said since the row was degraded counts here.
    let new = wait_for("a new incarnation once idle", BUDGET, || {
        let since = sb.log()[mark..].to_string();
        for bad in ["failed to start", "PARKED"] {
            assert!(!since.contains(bad), "the daemon said `{bad}` instead of relaunching:\n{since}\n{:?}", sb.events());
        }
        sb.ups().into_iter().find(|p| *p != old)
    });
    sb.assert_never_two_at_once();
    assert!(sb.has("term-idle", old), "{:?}", sb.events());
    assert_ne!(new, old);
}
