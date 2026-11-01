//! **R9.C7b2 (gap C-7) — A RE-PIN THE DAEMON HOLDS BACK WITHOUT A DRAIN IS
//! READ, NOT JUST HELD; A PARK SAYS WHAT IT IS WAITING ON.**
//!
//! C7 inspect F1: a re-pin deferred because the running build is busy (and has
//! no drain door) or its quiet signal is unreadable was said once into the
//! daemon's log. C7c put a "WAITING" line in `cauldron status`, re-derived
//! from a fresh gate read, with no since-when: an hours-old wait read the same
//! as a new one. Now the daemon records the deferral (target, since, its own
//! reason) and serves it on `STATUS`; `cauldron status` reads that.
//!
//! R9.C2-3: a parked service's wording said a pin to a different build would
//! revive it, which is false for a STOP-park, and `cauldron pin` said "what its
//! next start launches" for any service not running.
//!
//! Sandbox: a copy of `quiet_repin.rs`'s (throwaway `CAULDRON_HOME`, kernel
//! ports, a python fake serving its quiet signal). Nothing touches
//! `~/.cauldron` or a live daemon. THE LONGEST PERIOD on the deferral path is
//! the pass (`poll_secs = 1`); the since-is-stable check spans 3 passes.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const FAKE: &str = r#"#!/usr/bin/env python3
# R9.C7 fake: a hosted daemon whose quiet signal is three files.
import os, socket, sys, time, threading, json
def note(path, line):
    with open(path, "a") as f: f.write(line + "\n")
def owner_watch(owner):
    while True:
        time.sleep(1)
        try:
            os.kill(owner, 0)
        except ProcessLookupError:
            os._exit(0)
        except PermissionError:
            pass
import signal
if os.path.exists(os.path.join(os.environ["FAKE_ROOT"], "ignore-term")):
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
threading.Thread(target=owner_watch, args=(int(os.environ["FAKE_OWNER"]),), daemon=True).start()
note(os.environ["FAKE_PIDS"], str(os.getpid()))
root = os.environ["FAKE_ROOT"]
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
            note(os.path.join(root, "gate-reads"), str(os.getpid()))
        if os.path.exists(os.path.join(root, "garbled")):
            body = b"this is not json"
        else:
            idle = not os.path.exists(os.path.join(root, "busy"))
            body = json.dumps({"status": "ok", "restart_gate": {"idle": idle, "turns": 0 if idle else 2}}).encode()
        c.sendall(b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n" + body)
    except Exception:
        pass
    c.close()
"#;

const POLICY: &str = "auto_restart = true\nauto_rollback = false\npoll_secs = 1\ndown_after = 3\n\
                      max_start_failures = 3\nbackoff_base_secs = 1\nbackoff_max_secs = 5\n\
                      stable_after_secs = 600";
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
            "cauldron-r9c7b2-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("r9c7b2{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::create_dir_all(root.join("data").join(&env)).unwrap();
        std::fs::write(root.join("data").join(&env).join("env.toml"), format!("[supervisor]\n{POLICY}\n")).unwrap();
        std::fs::write(root.join("fake"), FAKE).unwrap();
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

    /// threads' shape: hosted, connect-probed, the quiet signal on its own
    /// address. `quiet` is the `[quiet]` section, or "" for none.
    fn declare(&self, name: &str, quiet: &str) -> u16 {
        self.declare_art(name, quiet, "fake")
    }

    /// As [`Sandbox::declare`], building from the artifact file `art` under
    /// the sandbox root (a second service needs bytes of its own).
    fn declare_art(&self, name: &str, quiet: &str, art: &str) -> u16 {
        let port = free_port();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = 10000\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\n\
                 [env]\nFAKE_PORT = \"{port}\"\nFAKE_PIDS = \"{}\"\nFAKE_ROOT = \"{}\"\nFAKE_OWNER = \"{}\"\n\
                 {quiet}\n\
                 [build]\nartifact = \"{}\"\nbin = \"{art}\"\n",
                self.root.join("fake-pids").display(),
                self.root.display(),
                std::process::id(),
                self.root.join(art).display()
            ),
        )
        .unwrap();
        port
    }

    /// Build the artifact as it is now and return the new label.
    fn build(&self, name: &str) -> String {
        let (rc, out) = self.cauldron(&["build", name]);
        assert_eq!(rc, 0, "build {name}: {out}");
        let (rc, out) = self.cauldron(&["versions", name, "--json"]);
        assert_eq!(rc, 0, "versions {name}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        v[0]["label"].as_str().expect("a label").to_string()
    }

    /// A new label needs new artifact bytes (the label is derived from them).
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

    fn seam(&self, name: &str) -> serde_json::Value {
        let t = std::fs::read_to_string(self.root.join("data").join(&self.env).join("seams.json")).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&t).ok().map(|v| v["seams"][name].clone()).unwrap_or_default()
    }

    /// `(pid, version)` of the instance up on `port`, if one is.
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
        std::fs::remove_file(self.root.join(f)).unwrap();
    }

    /// Up on v1 under a running daemon. Returns `(port, v1, pid1)`.
    fn up_on_v1(&mut self, name: &str, quiet: &str) -> (u16, String, u32) {
        let port = self.declare(name, quiet);
        let v1 = self.build(name);
        self.pin(name, &v1);
        self.start_daemon();
        let (pid, ver) = wait_for("v1 up under the daemon", BUDGET, || self.up(name, port));
        assert_eq!(ver, v1);
        (port, v1, pid)
    }
}


impl Sandbox {
    /// `cauldron status` — text, and the JSON answer's `restart_gates` row.
    fn status(&self, name: &str) -> (String, serde_json::Value) {
        let (rc, text) = self.cauldron(&["status"]);
        assert!(rc == 0 || rc == 1, "status rc {rc}: {text}");
        let (_, js) = self.cauldron(&["status", "--json"]);
        let v: serde_json::Value = serde_json::from_str(&js).unwrap_or_else(|e| panic!("{e}: {js}"));
        (text, v["restart_gates"][name].clone())
    }

    /// `name`'s row in the holding daemon's own `STATUS` answer.
    fn holder_row(&self, name: &str) -> serde_json::Value {
        let t = std::fs::read_to_string(self.root.join("data").join(&self.env).join("seams.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
        let addr: SocketAddr = v["seams"]["cauldron"]["addr"].as_str().expect("holder addr").parse().unwrap();
        let s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).expect("holder");
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut w = s.try_clone().unwrap();
        w.write_all(b"STATUS\n").unwrap();
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).unwrap();
        let e: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_else(|x| panic!("{x}: {line}"));
        e["hosting"].as_array().unwrap().iter().find(|r| r["service"] == name).cloned().expect("row")
    }
}

const QUIET: &str = "[quiet]\npath = \"/gate\"\npointer = \"/restart_gate/idle\"\n";

/// A build that never starts: exits at once, before it binds.
const DEAD: &str = "#!/usr/bin/env python3\nimport sys\nsys.exit(3)\n";

// ═══════════════════════════════════════════════════════════════ (a) ════

/// THE CLAIM. A re-pin the daemon declines because the running build is busy
/// and has no drain door is in `cauldron status` WITH SINCE-WHEN AND WHY, read
/// off the daemon's own record — the `since` does not move between two status
/// reads passes apart (a re-derived "now" would) — and it is gone once the
/// restart lands.
#[test]
fn r9c7b2_a_busy_deferred_repin_shows_since_and_why_outlasting_3_polls_and_clears_on_restart() {
    let mut sb = Sandbox::new("busy");
    let (port, v1, pid1) = sb.up_on_v1("q", QUIET);
    sb.touch("busy");
    sb.new_bytes("v2");
    let v2 = sb.build("q");
    assert_ne!(v2, v1, "sentinel: a second label");
    sb.pin("q", &v2);
    wait_for("the daemon to decline the re-pin", BUDGET, || sb.log().contains("is NOT QUIET").then_some(()));

    let row = sb.holder_row("q");
    assert_eq!(row["deferred"]["repin_to"], v2.as_str(), "the holder records the wait: {row}");
    assert_eq!(row["deferred"]["kind"], "busy", "{row}");
    let (text, gate) = sb.status("q");
    assert!(text.contains(&format!("re-pin to {v2} WAITING since ")), "{text}");
    assert!(text.contains("no drain door") && text.contains("/restart_gate/idle = false"), "the reason the daemon read:\n{text}");
    let since = gate["waiting_since"].as_u64().unwrap_or_else(|| panic!("no waiting_since: {gate}"));
    assert_eq!(gate["waiting_kind"], "busy", "{gate}");
    assert_eq!(row["deferred"]["since"].as_u64(), Some(since), "status reads the holder's since");

    // Three passes later the wait is the SAME wait: older, not re-stamped.
    std::thread::sleep(Duration::from_secs(3));
    let (_, gate2) = sb.status("q");
    assert_eq!(gate2["waiting_since"].as_u64(), Some(since), "since moved: {gate} -> {gate2}");
    assert!(gate2["waiting_secs"].as_u64().unwrap_or_default() >= 3, "{gate2}");
    assert_eq!(sb.up("q", port), Some((pid1, v1.clone())), "premise: still waiting\n{}", sb.log());

    // Cleared when the restart happens.
    sb.rm("busy");
    let (_, ver) = wait_for("q restarted onto v2", BUDGET, || sb.up("q", port).filter(|(p, _)| *p != pid1));
    assert_eq!(ver, v2);
    let (text, gate) = sb.status("q");
    assert!(!text.contains("WAITING"), "a landed re-pin still shows a wait:\n{text}");
    assert!(gate.is_null(), "{gate}");
    assert!(sb.holder_row("q")["deferred"].is_null(), "{}", sb.holder_row("q"));
}

/// The same claim for the other no-drain deferral: a quiet signal that cannot
/// be read. The reason is the daemon's ("not JSON"), and so is the since.
#[test]
fn r9c7b2_a_unreadable_deferred_repin_shows_since_and_the_read_failure() {
    let mut sb = Sandbox::new("garb");
    let (_port, v1, _pid1) = sb.up_on_v1("g", QUIET);
    sb.touch("garbled");
    sb.new_bytes("v2");
    let v2 = sb.build("g");
    assert_ne!(v2, v1);
    sb.pin("g", &v2);
    wait_for("the daemon to log the unreadable signal", BUDGET, || {
        sb.log().contains("quiet signal CANNOT BE READ").then_some(())
    });
    let (text, gate) = sb.status("g");
    assert!(text.contains(&format!("re-pin to {v2} WAITING since ")), "{text}");
    assert!(text.contains("cannot be read") && text.contains("not JSON"), "{text}");
    assert_eq!(gate["waiting_kind"], "unreadable", "{gate}");
    assert!(gate["waiting_since"].as_u64().is_some(), "{gate}");
}

/// OPPOSES (a): a re-pin that is NOT deferred — the service reads quiet — is
/// restarted, and no pass shows a wait: the holder never records one, and
/// `status` has no gate line before, during or after.
#[test]
fn r9c7b2_a_repin_that_is_not_deferred_shows_no_wait() {
    let mut sb = Sandbox::new("idle");
    let (port, v1, pid1) = sb.up_on_v1("i", QUIET);
    let (text, gate) = sb.status("i");
    assert!(!text.contains("WAITING") && gate.is_null(), "a wait with nothing pinned:\n{text}");
    sb.new_bytes("v2");
    let v2 = sb.build("i");
    assert_ne!(v2, v1);
    sb.pin("i", &v2);
    let (_, ver) = wait_for("i restarted onto v2", BUDGET, || {
        let row = sb.holder_row("i");
        assert!(row["deferred"].is_null(), "a quiet re-pin recorded as deferred: {row}");
        sb.up("i", port).filter(|(p, _)| *p != pid1)
    });
    assert_eq!(ver, v2);
    assert!(sb.log().contains("is quiet ("), "premise: it was read quiet\n{}", sb.log());
    let (text, gate) = sb.status("i");
    assert!(!text.contains("WAITING") && gate.is_null(), "{text}");
}

// ═══════════════════════════════════════════════════════════════ (b) ════

/// A STOP-parked service's wording names what it waits for — `cauldron
/// restart`, and not a pin — in the daemon's warning, in `status`, and in what
/// `cauldron pin` says.
#[test]
fn r9c7b2_b_a_stop_parked_service_says_it_waits_for_a_restart_not_a_pin() {
    let mut sb = Sandbox::new("stop");
    let (port, v1, _pid1) = sb.up_on_v1("s", "");
    let (rc, out) = sb.cauldron(&["stop", "s"]);
    assert_eq!(rc, 0, "stop: {out}\n{}", sb.log());
    wait_for("a parked pass for s", BUDGET, || sb.log().contains("`s` is PARKED in").then_some(()));
    assert!(!listening(port), "premise: stopped means down");
    let log = sb.log();
    assert!(log.contains("STOPPED by request") && log.contains("waiting for `cauldron restart s`"), "{log}");
    assert!(!log.contains("a build OTHER than"), "a stop-park told to pin:\n{log}");
    let (text, gate) = sb.status("s");
    assert!(text.contains("PARKED: stopped by request, waiting for `cauldron restart s`"), "{text}");
    assert_eq!(gate["parked_by"], "stop", "{gate}");
    sb.new_bytes("v2");
    let v2 = sb.build("s");
    assert_ne!(v2, v1);
    let said = sb.pin("s", &v2);
    assert!(said.contains("STOPPED by request") && said.contains("a pin does not revive"), "{said}");
    assert!(!said.contains("what its next start launches"), "{said}");
}

/// The pair: a FAILURE-parked service waits for a DIFFERENT build. Its wording
/// names the build it gave up on; a pin to that same build says it stays
/// parked, and a pin to another says the daemon launches it — which it does.
#[test]
fn r9c7b2_b_a_failure_parked_service_names_the_build_it_waits_to_replace() {
    let mut sb = Sandbox::new("fail");
    std::fs::write(sb.root.join("art"), DEAD).unwrap();
    let port = sb.declare_art("f", "", "art");
    let bad = sb.build("f");
    sb.pin("f", &bad);
    sb.start_daemon();
    wait_for("f PARKED", Duration::from_secs(240), || sb.log().contains("`f` is PARKED in").then_some(()));
    let log = sb.log();
    assert!(log.contains(&format!("failed starts on {bad}")) && log.contains("a build OTHER than that one"), "{log}");
    assert!(!log.contains("STOPPED by request"), "{log}");
    let (text, gate) = sb.status("f");
    assert!(text.contains(&format!("PARKED: the daemon gave up on {bad}")), "{text}");
    assert_eq!(gate["parked_by"], "failures", "{gate}");
    assert_eq!(gate["parked_on"], bad.as_str(), "{gate}");

    let said = sb.pin("f", &bad);
    assert!(said.contains("STAYS PARKED") && said.contains(&format!("on {bad}")), "{said}");

    std::fs::write(sb.root.join("art"), FAKE).unwrap();
    let good = sb.build("f");
    assert_ne!(good, bad);
    let said = sb.pin("f", &good);
    assert!(said.contains("clears the park and launches") && said.contains(&good), "{said}");
    let (_, ver) = wait_for("f revived onto the good build", BUDGET, || sb.up("f", port));
    assert_eq!(ver, good, "the wording's promise held");
}

// ═══════════════════════════════════════════════════ R9.C7b3 (a) ════
//
// C7b2 inspect F1: the unpin sweep dropped the service from the child table
// and left its deferral record. When the sweep's stop FAILS (the process
// ignores its clean stop and may not be hard-killed) the process lives on;
// a later re-pin to the same build re-attaches it, reads it busy, and
// `deferral_hold` kept the OLD since — a stale, longer wait. An unpin ends
// the wait. THE LONGEST PERIOD here is the pass (`poll_secs = 1`); the
// keeps-the-wait check spans 3 passes.

impl Sandbox {
    /// The declaration's clean stop is ignored by the fake (with
    /// `ignore-term` touched before launch) and it may not be hard-killed, so
    /// the unpin sweep's stop FAILS within a second and the process lives on.
    fn stubborn(&self, name: &str) {
        let p = self.root.join("services").join(format!("{name}.toml"));
        let t = std::fs::read_to_string(&p).unwrap();
        let at = "start_timeout_ms = 10000\n";
        assert!(t.contains(at), "{t}");
        std::fs::write(&p, t.replace(at, &format!("{at}escalate = false\nstop_grace_secs = 1\n"))).unwrap();
    }

    /// The operator's unpin: the service leaves the manifest.
    fn unpin(&self, name: &str) {
        let path = self.root.join("data").join(&self.env).join("env.toml");
        let mut m = ::env::decl::Manifest::load(&path).unwrap();
        assert!(m.services.remove(name).is_some(), "premise: `{name}` was pinned");
        m.save(&path).unwrap();
    }

    /// `name`'s row in the holder's `STATUS`, if it has one.
    fn holder_row_opt(&self, name: &str) -> Option<serde_json::Value> {
        let t = std::fs::read_to_string(self.root.join("data").join(&self.env).join("seams.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&t).ok()?;
        let addr: SocketAddr = v["seams"]["cauldron"]["addr"].as_str()?.parse().ok()?;
        let s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        let mut w = s.try_clone().ok()?;
        w.write_all(b"STATUS\n").ok()?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).ok()?;
        let e: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        e["hosting"].as_array()?.iter().find(|r| r["service"] == name).cloned()
    }

    /// The holder's recorded wait for `name` onto `repin_to`: its `since`.
    fn deferred_since(&self, name: &str, repin_to: &str) -> Option<u64> {
        let row = self.holder_row_opt(name)?;
        (row["deferred"]["repin_to"] == repin_to).then(|| row["deferred"]["since"].as_u64()).flatten()
    }
}

fn secs_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

/// THE CLAIM. Pin → deferred (busy) → unpin, whose stop fails so the process
/// lives on → re-pin to the SAME build: the wait the daemon records starts at
/// the re-pin's pass, not at the first wait.
#[test]
fn r9c7b3_a_unpin_then_repin_of_the_same_build_starts_a_fresh_wait() {
    let mut sb = Sandbox::new("unpin");
    sb.touch("ignore-term");
    let port = sb.declare("u", QUIET);
    sb.stubborn("u");
    let v1 = sb.build("u");
    sb.pin("u", &v1);
    sb.start_daemon();
    let (pid1, _) = wait_for("v1 up under the daemon", BUDGET, || sb.up("u", port));
    sb.touch("busy");
    sb.new_bytes("v2");
    let v2 = sb.build("u");
    assert_ne!(v2, v1, "sentinel: a second label");
    sb.pin("u", &v2);
    let s1 = wait_for("the daemon to hold the re-pin", BUDGET, || sb.deferred_since("u", &v2));

    // The first wait is now measurably older than anything recorded after it.
    std::thread::sleep(Duration::from_secs(2));
    let unpinned_at = secs_now();
    assert!(unpinned_at > s1, "sentinel: the unpin is later than the first wait ({s1} vs {unpinned_at})");
    sb.unpin("u");
    wait_for("the unpin sweep's stop to fail", BUDGET, || {
        sb.log().contains("stopping the unpinned `u`").then_some(())
    });
    wait_for("the sweep to drop `u` from the holder's table", BUDGET, || {
        sb.holder_row_opt("u").is_none().then_some(())
    });
    assert_eq!(sb.up("u", port), Some((pid1, v1.clone())), "premise: the stop failed and v1 runs on\n{}", sb.log());

    sb.pin("u", &v2);
    let s2 = wait_for("the re-pin held again", BUDGET, || sb.deferred_since("u", &v2));
    assert!(
        s2 >= unpinned_at,
        "the wait after an unpin inherited the old one: since {s2}, first wait {s1}, unpinned at {unpinned_at}"
    );
    assert_eq!(sb.up("u", port), Some((pid1, v1)), "premise: still waiting, not restarted\n{}", sb.log());
}

/// OPPOSES (a): with NO unpin, a re-pin to the same build is the same wait —
/// the record and its since are kept across the re-pin and 3 passes after it.
#[test]
fn r9c7b3_a_repin_of_the_same_build_without_an_unpin_keeps_the_wait_outlasting_3_polls() {
    let mut sb = Sandbox::new("keep");
    let (port, v1, pid1) = sb.up_on_v1("k", QUIET);
    sb.touch("busy");
    sb.new_bytes("v2");
    let v2 = sb.build("k");
    assert_ne!(v2, v1, "sentinel: a second label");
    sb.pin("k", &v2);
    let s1 = wait_for("the daemon to hold the re-pin", BUDGET, || sb.deferred_since("k", &v2));
    std::thread::sleep(Duration::from_secs(2));
    assert!(secs_now() > s1, "sentinel: a re-stamped since would differ");
    sb.pin("k", &v2);
    std::thread::sleep(Duration::from_secs(3));
    let s2 = sb.deferred_since("k", &v2).unwrap_or_else(|| panic!("the wait was dropped\n{}", sb.log()));
    assert_eq!(s2, s1, "a re-pin with no unpin re-stamped the wait");
    assert_eq!(sb.up("k", port), Some((pid1, v1)), "premise: still waiting\n{}", sb.log());
}

// ═══════════════════════════════════════════════════════ PIN-RACE ════
//
// CLI-ISO-I F2: `pin` SAVED the pin and only then read the park, so with
// `poll_secs = 1` the holder could revive a failure-park (or a stop could
// park the service) between the two, and `pin` described a state it was not
// written over — `r9c7b2_b_a_failure_parked…` went red once under load. Now
// the read and the save are one step under the env.toml writers' lock, and
// the holder takes that lock to park. These two tests HOLD that step open
// (`CAULDRON_TEST_PIN_WINDOW_MS`) and land the other party exactly inside it,
// at the moment `CAULDRON_TEST_PIN_READ_MARK` says the read is done, so the
// interleaving the full suite only sometimes hit happens every run.

const PIN_WINDOW_MS: u64 = 4000;

impl Sandbox {
    /// `cauldron pin name label`, held open between its read of the park and
    /// its save. Returns the child and the mark file it writes on the read.
    fn pin_held_open(&self, name: &str, label: &str) -> (Child, PathBuf) {
        let mark = self.root.join(format!("pin-read-{name}"));
        let child = Command::new(env!("CARGO_BIN_EXE_cauldron"))
            .args(["pin", name, label])
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .env("CAULDRON_TEST_PIN_WINDOW_MS", PIN_WINDOW_MS.to_string())
            .env("CAULDRON_TEST_PIN_READ_MARK", &mark)
            .env(crate::grants::ALLOW_ENV, crate::grants::owned(&self.root, &self.daemon.iter().map(Child::id).collect::<Vec<_>>()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn cauldron pin");
        (child, mark)
    }

    fn pinned(&self, name: &str) -> Option<String> {
        let path = self.root.join("data").join(&self.env).join("env.toml");
        ::env::decl::Manifest::load(&path).ok()?.pin(name)
    }
}

fn finish(child: Child) -> (i32, String) {
    let out = child.wait_with_output().expect("wait for cauldron pin");
    (
        out.status.code().unwrap_or(-1),
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
    )
}

/// F2 itself: a pin to a different build than a failure-park's lands while
/// the holder passes every second. The holder cannot clear the park until the
/// pin is SAVED, and the pin read the park before saving, so it says the park
/// is cleared and the new build launched — and both happen.
#[test]
fn pin_race_a_failure_park_is_read_before_the_pin_is_saved_and_neither_is_lost() {
    let mut sb = Sandbox::new("prfail");
    std::fs::write(sb.root.join("art"), DEAD).unwrap();
    let port = sb.declare_art("f", "", "art");
    let bad = sb.build("f");
    sb.pin("f", &bad);
    sb.start_daemon();
    wait_for("f PARKED", Duration::from_secs(240), || sb.log().contains("`f` is PARKED in").then_some(()));
    std::fs::write(sb.root.join("art"), FAKE).unwrap();
    let good = sb.build("f");
    assert_ne!(good, bad);

    let (child, mark) = sb.pin_held_open("f", &good);
    wait_for("pin to read the park", BUDGET, || mark.exists().then_some(()));
    // Two holder passes inside the held step: the park still stands, because
    // the pin that would clear it is not written yet.
    std::thread::sleep(Duration::from_millis(2000));
    let row = sb.holder_row("f");
    assert_eq!(row["parked"], true, "the park was cleared inside pin's read→write step: {row}\n{}", sb.log());
    assert_eq!(row["parked_on"], bad.as_str(), "{row}");
    let (rc, said) = finish(child);
    assert_eq!(rc, 0, "{said}");
    // The pin's wording is about the park it was written over.
    assert!(said.contains("clears the park and launches") && said.contains(&good), "{said}");
    assert!(!said.contains("what its next start launches"), "{said}");
    // Neither update lost: the pin is in env.toml, and the park it cleared
    // was the one it read — the daemon launches the good build.
    assert_eq!(sb.pinned("f").as_deref(), Some(good.as_str()), "the pin was lost");
    let (_, ver) = wait_for("f revived onto the good build", BUDGET, || sb.up("f", port));
    assert_eq!(ver, good, "the wording's promise held");
}

/// A STOP-park requested while a pin is between its read and its save waits
/// for the save, then lands: the pin is written over the state it read (the
/// service running), and the park is not lost to the pin (it stays parked, a
/// pin never revives a stop-park).
#[test]
fn pin_race_a_stop_park_landing_inside_pins_read_write_step_waits_for_the_save_and_neither_is_lost() {
    let mut sb = Sandbox::new("prstop");
    let (port, v1, _pid1) = sb.up_on_v1("p", "");
    sb.new_bytes("v2");
    let v2 = sb.build("p");
    assert_ne!(v2, v1);

    let (child, mark) = sb.pin_held_open("p", &v2);
    wait_for("pin to read the park", BUDGET, || mark.exists().then_some(()));
    let t_read = Instant::now();
    let (rc, out) = sb.cauldron(&["stop", "p"]);
    let stop_took = t_read.elapsed();
    assert_eq!(rc, 0, "stop: {out}\n{}", sb.log());
    let (rc, said) = finish(child);
    assert_eq!(rc, 0, "{said}");
    // The park waited for the pin's save (the step held open PIN_WINDOW_MS
    // from the read): it did not land between the read and the write.
    assert!(
        stop_took >= Duration::from_millis(PIN_WINDOW_MS - 300),
        "the stop-park landed inside pin's read→write step ({stop_took:?} < {PIN_WINDOW_MS}ms):\n{said}\n{}",
        sb.log()
    );
    // The pin describes the state it was written over: running, under a holder.
    assert!(said.contains("restarts the running `p`"), "{said}");
    // Neither update lost: v2 is pinned, and p is stop-parked and down, and
    // stays so across passes with the new pin in place.
    assert_eq!(sb.pinned("p").as_deref(), Some(v2.as_str()), "the pin was lost");
    for _ in 0..3 {
        let row = sb.holder_row("p");
        assert_eq!(row["parked"], true, "{row}\n{}", sb.log());
        assert_eq!(row["parked_by"], "stop", "{row}");
        assert!(!listening(port), "the stop-park was undone:\n{}", sb.log());
        std::thread::sleep(Duration::from_millis(1100));
    }
}
