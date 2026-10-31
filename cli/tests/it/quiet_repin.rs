//! **R9.C7 (gap C-7) — A RE-PIN RESTARTS A SERVICE ONLY AT A QUIET MOMENT.**
//!
//! Before: a daemon holding an environment restarted a running service onto a
//! new pin on its very next pass (`poll_secs`, 4 s by default), whatever it was
//! doing, while `cauldron pin` said "nothing was started or stopped". Now a
//! service may declare `[quiet] path/pointer`; the daemon reads it on every
//! pass and restarts only on a pass that reads `true`.
//!
//! Every test starts a THROWAWAY cauldron (its own `CAULDRON_HOME` under the
//! system temp dir, kernel-assigned ports). Nothing touches `~/.cauldron`, a
//! live daemon, or launchd. The service is one python fake, hosted the way
//! threads is (`registers = false`, `probe = "connect"`), serving its quiet
//! signal as JSON on its own address the way threads' `/health` does.
//!
//! THE LONGEST PERIOD ON THIS PATH (doctrine 4) is the supervisor's pass,
//! `poll_secs`: the quiet signal is re-read once per pass and nothing else
//! re-checks it. The sandbox runs `poll_secs = 1`; every "no restart" below is
//! asserted only after the daemon has LOGGED that it saw the new pin and
//! declined (so the pass that would have restarted it has happened), and then
//! again after `HOLD` = 6 s more — six sandbox passes, and longer than the
//! production default of 4 s that the gap measured.
//!
//! Claims, each paired with its opposite:
//!
//! (a) a re-pin while the service reads BUSY is not restarted for `HOLD`, and
//!     the moment it reads quiet it is (`launch`, `why = repin`, new pid, new
//!     version). Opposed: the SAME busy fixture with no `[quiet]` declared is
//!     restarted at once — today's behaviour, kept;
//! (b) a quiet signal that cannot be read (not JSON) is NOT a quiet moment: no
//!     restart for `HOLD`, and both the daemon and `cauldron pin` say so. Then
//!     it becomes readable-and-quiet and the restart happens, so the refusal is
//!     not a path that can never restart;
//! (c) doctrine 10 — who is in which regime today, read off the shipped
//!     declarations.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

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
/// Six `poll_secs = 1` passes, and more than production's default 4 s pass.
const HOLD: Duration = Duration::from_secs(6);

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
            "cauldron-r9c7-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("r9c7{tag}");
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

    /// threads' shape: hosted, connect-probed, the quiet signal on its own
    /// address. `quiet` is the `[quiet]` section, or "" for none.
    fn declare(&self, name: &str, quiet: &str) -> u16 {
        let port = free_port();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\n\
                 [env]\nFAKE_PORT = \"{port}\"\nFAKE_PIDS = \"{}\"\nFAKE_ROOT = \"{}\"\nFAKE_OWNER = \"{}\"\n\
                 {quiet}\n\
                 [build]\nartifact = \"{}\"\nbin = \"fake\"\n",
                self.root.join("fake-pids").display(),
                self.root.display(),
                std::process::id(),
                self.root.join("fake").display()
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

const QUIET: &str = "[quiet]\npath = \"/gate\"\npointer = \"/restart_gate/idle\"\n";

// ═══════════════════════════════════════════════════════════════ (a) ════

#[test]
fn r9c7_a_repin_while_busy_is_not_restarted_outlasting_6_polls_then_is_once_quiet() {
    let mut sb = Sandbox::new("busy");
    let (port, v1, pid1) = sb.up_on_v1("q", QUIET);

    sb.touch("busy");
    sb.new_bytes("v2");
    let v2 = sb.build("q");
    assert_ne!(v2, v1, "sentinel: a second label");
    let said = sb.pin("q", &v2);
    // The CLI no longer claims nothing will happen; it says what will.
    assert!(!said.contains("nothing was started or stopped"), "{said}");
    assert!(said.contains("at the first pass that reads it quiet") && said.contains("busy right now"), "{said}");

    // The daemon SAW the new pin and declined (the pass that used to restart).
    wait_for("the daemon to log that q is not quiet", BUDGET, || sb.log().contains("is NOT QUIET").then_some(()));
    let reads_then = std::fs::read_to_string(sb.root.join("gate-reads")).unwrap_or_default().lines().count();
    std::thread::sleep(HOLD);
    assert_eq!(sb.up("q", port), Some((pid1, v1.clone())), "restarted while busy\n{}", sb.log());
    assert_eq!(sb.repin_launches("q"), 0, "a repin launch while busy\n{}", sb.log());
    // It kept re-checking (a bounded re-check every pass, not one look).
    let reads_now = std::fs::read_to_string(sb.root.join("gate-reads")).unwrap_or_default().lines().count();
    assert!(reads_now >= reads_then + 3, "the signal was not re-read each pass: {reads_then} -> {reads_now}");
    // Said once, not once per pass.
    assert_eq!(sb.log().matches("is NOT QUIET").count(), 1, "{}", sb.log());

    // Opposed: quiet arrives → restarted onto the pin.
    sb.rm("busy");
    let (pid2, ver2) = wait_for("q restarted onto v2 once quiet", BUDGET, || sb.up("q", port).filter(|(p, _)| *p != pid1));
    assert_eq!(ver2, v2);
    assert_ne!(pid2, pid1);
    assert!(sb.repin_launches("q") >= 1, "{}", sb.log());
    assert!(sb.log().contains("is quiet ("), "{}", sb.log());
}

/// Opposes (a): the same BUSY fixture with no `[quiet]` is restarted at once,
/// so (a)'s hold is the declaration being read, not the fixture being slow.
#[test]
fn r9c7_a_without_a_quiet_declaration_a_busy_service_is_restarted_as_before() {
    let mut sb = Sandbox::new("undecl");
    let (port, v1, pid1) = sb.up_on_v1("u", "");
    sb.touch("busy");
    sb.new_bytes("v2");
    let v2 = sb.build("u");
    assert_ne!(v2, v1);
    let said = sb.pin("u", &v2);
    assert!(said.contains("WHETHER OR NOT IT IS MID-WORK"), "{said}");
    let (_, ver) = wait_for("u restarted onto v2", BUDGET, || sb.up("u", port).filter(|(p, _)| *p != pid1));
    assert_eq!(ver, v2);
    assert!(!sb.log().contains("NOT QUIET"), "{}", sb.log());
}

// ═══════════════════════════════════════════════════════════════ (b) ════

#[test]
fn r9c7_b_an_unreadable_quiet_signal_is_not_restarted_outlasting_6_polls_and_says_so() {
    let mut sb = Sandbox::new("garb");
    let (port, v1, pid1) = sb.up_on_v1("g", QUIET);

    sb.touch("garbled");
    sb.new_bytes("v2");
    let v2 = sb.build("g");
    assert_ne!(v2, v1);
    let said = sb.pin("g", &v2);
    assert!(said.contains("cannot be now") && said.contains("will NOT restart it (fail safe)"), "{said}");
    assert!(!said.contains("nothing was started or stopped"), "{said}");

    wait_for("the daemon to log the unreadable signal", BUDGET, || {
        sb.log().contains("quiet signal CANNOT BE READ").then_some(())
    });
    assert!(sb.log().contains("not JSON"), "the refusal names what was wrong\n{}", sb.log());
    std::thread::sleep(HOLD);
    assert_eq!(sb.up("g", port), Some((pid1, v1.clone())), "restarted on an unreadable signal\n{}", sb.log());
    assert_eq!(sb.repin_launches("g"), 0, "{}", sb.log());

    // Not a dead end: readable and quiet → the restart happens.
    sb.rm("garbled");
    let (_, ver) = wait_for("g restarted once readable and quiet", BUDGET, || sb.up("g", port).filter(|(p, _)| *p != pid1));
    assert_eq!(ver, v2);
}

// ═══════════════════════════════════════════════════════════════ (c) ════

/// Doctrine 10: name who is in each regime today, from the SHIPPED files.
/// `threads` is the only service that declares a quiet signal; every other
/// declaration is in the "restarted as before" regime. And — the regime the
/// unreadable path guards — the running prod threads build predates this
/// section, so its snapshot declares none and its next re-pin is restarted as
/// before; the first build carrying `[quiet]` is the first one waited for.
#[test]
fn r9c7_c_threads_alone_declares_a_quiet_signal_and_every_other_service_does_not() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("services");
    let mut declares = Vec::new();
    let mut none = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let v: toml::Value = toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let name = v["name"].as_str().unwrap().to_string();
        match v.get("quiet") {
            Some(q) => declares.push((name, q["path"].as_str().map(str::to_string), q["pointer"].as_str().map(str::to_string))),
            None => none.push(name),
        }
    }
    none.sort();
    assert_eq!(
        declares,
        vec![("threads".to_string(), Some("/health".to_string()), Some("/restart_gate/idle".to_string()))]
    );
    // Counterpart: the services that keep today's behaviour, by name, so a
    // file the predicate failed to read cannot pass as "declares none".
    assert_eq!(
        none,
        ["claude-account-expiry-alarm", "echo", "soup-backup", "tally", "tts", "ui-server"]
    );
}
