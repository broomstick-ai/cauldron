//! **C-1b (R9.C1 second inspection, N2): `cauldron env remove --yes --stop`
//! during a drain never deletes the data root while an owned pid is alive.**
//!
//! `env remove` is the destructive one of the F2 verbs, and until this test
//! its guarantee rested on reading the code (`envs.rs`: the live filter, then
//! `stop_everything`, whose stop path waits for the PID and whose step 4 bails
//! before `remove_dir_all`). Here a hosted fake that `threads`'s shape — on
//! SIGTERM it closes its port FIRST, then drains — is put mid-drain by the
//! test (a drain cauldron did not start), and `env remove --yes --stop` runs
//! while a sampler thread watches, every 20 ms, two facts that neither
//! cauldron nor the test writes: is the fake's pid alive (`ps`), and does the
//! data root exist. The claim is that no sample ever sees the pid alive with
//! the data root gone.
//!
//! The events file and the fake live OUTSIDE the data root, so deleting it
//! does not delete the evidence. A THROWAWAY cauldron throughout: own
//! `CAULDRON_HOME` under the temp dir, kernel-assigned port; nothing touches
//! `~/.cauldron`, a live daemon or launchd.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

const FAKE: &str = r#"#!/usr/bin/env python3
import os, signal, socket, time
port = int(os.environ["FAKE_PORT"])
ev = os.environ["FAKE_EVENTS"]
drain = float(os.environ.get("FAKE_DRAIN", "0"))
def note(word):
    with open(ev, "a") as f:
        f.write("%s %d %.3f\n" % (word, os.getpid(), time.time()))
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

/// A POSITIVE wait's bound (hangs only).
const BUDGET: Duration = Duration::from_secs(90);

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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn listening(port: u16) -> bool {
    let sa: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    TcpStream::connect_timeout(&sa, Duration::from_millis(200)).is_ok()
}

/// Alive and not a zombie.
fn alive(pid: u32) -> bool {
    match Command::new("/bin/ps").args(["-o", "stat=", "-p", &pid.to_string()]).output() {
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
        let fake = root.join("fake-daemon");
        std::fs::write(&fake, launch_wait::slow_launch_py(FAKE, &root.join("f2g-marks"))).unwrap();
        Command::new("/bin/chmod").args(["+x"]).arg(&fake).status().unwrap();
        Sandbox { root, env }
    }

    fn data_root(&self) -> PathBuf {
        self.root.join("data").join(&self.env)
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

    fn register(&self, name: &str, drain: &str) -> u16 {
        let port = free_port();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
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

    fn row_pid(&self, name: &str) -> Option<u32> {
        let t = std::fs::read_to_string(self.data_root().join("seams.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&t).ok()?;
        v["seams"][name]["pid"].as_u64().map(|p| p as u32)
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
}

/// The owned pid is mid-drain (port already quiet, 4 s left — the longest
/// period in the path, and the window the claim must hold across) when `env
/// remove --yes --stop` runs. At every 20 ms sample: pid alive ⇒ data root
/// present. Paired: the removal DID happen once the pid exited (the fix is not
/// "never remove"), and the drain was not re-signalled.
#[test]
fn env_remove_stop_during_a_4s_drain_keeps_the_data_root_until_the_owned_pid_exits() {
    let sb = Sandbox::new("rmdrain");
    let port = sb.register("svc", "4");
    let (rc, out) = sb.cauldron(&["start", "svc"]);
    assert_eq!(rc, 0, "{out}");
    let pid = wait_for("svc up, its row written", BUDGET, || {
        let p = sb.events().iter().rev().find(|(w, _)| w == "up").map(|(_, p)| *p)?;
        (listening(port) && sb.row_pid("svc") == Some(p)).then_some(p)
    });
    assert!(Command::new("/bin/kill").args(["-TERM", &pid.to_string()]).status().unwrap().success());
    wait_for("the port to go quiet", BUDGET, || (!listening(port)).then_some(()));
    assert!(alive(pid), "premise: the owned pid is draining when `env remove` runs");

    let stop = Arc::new(AtomicBool::new(false));
    let (stop2, dr) = (stop.clone(), sb.data_root());
    let sampler = std::thread::spawn(move || {
        let (mut samples, mut alive_samples, mut violation) = (0usize, 0usize, None);
        while !stop2.load(Ordering::SeqCst) {
            // Order matters: read the DIRECTORY first, then the pid. A pid
            // alive AFTER the directory was seen gone was alive when it went.
            let exists = dr.exists();
            let live = alive(pid);
            samples += 1;
            if live {
                alive_samples += 1;
                if !exists && violation.is_none() {
                    violation = Some(samples);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        (samples, alive_samples, violation)
    });
    let (rc, out) = sb.cauldron(&["env", "remove", &sb.env, "--yes", "--stop"]);
    let alive_at_return = alive(pid);
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::SeqCst);
    let (samples, alive_samples, violation) = sampler.join().unwrap();

    assert!(
        violation.is_none(),
        "the data root was GONE while owned pid {pid} was still alive (sample {violation:?} of \
         {samples}). rc={rc}:\n{out}\ntimeline {:?}",
        sb.events()
    );
    // The window was observed, not assumed: the sampler saw the pid alive
    // while `env remove` ran.
    assert!(alive_samples > 0, "the sampler never saw the drain ({samples} samples):\n{out}");
    assert_eq!(rc, 0, "{out}");
    assert!(!alive_at_return, "`env remove` returned with pid {pid} alive:\n{out}");
    assert!(!sb.data_root().exists(), "the removal must have happened once the pid exited:\n{out}");
    assert!(sb.has("down", pid), "the drain must have finished cleanly: {:?}", sb.events());
    assert!(!sb.has("second-signal", pid), "the draining service was re-signalled: {:?}", sb.events());
}
