//! **R9.C7f7 (gap C-7, inspect R9.C7f5 F1) — AN AUTO-ROLLBACK THAT COULD NOT
//! BE WRITTEN IS OWED, AND IS PAID BEFORE ANYTHING IS LAUNCHED.**
//!
//! When a pin exhausts `max_start_failures`, the holder re-pins env.toml to
//! the newest good build under the writers' lock, bounded by
//! `HOLDER_ENV_LOCK_BOUND` (500 ms). A busy lock fails that write. Before
//! C7f7 nothing remembered the decision: the service sat down on the bad pin,
//! and the down path LAUNCHED THE BAD PIN AGAIN to re-reach `exhausted` — one
//! extra launch of a build already known to be bad, plus its backoff, per
//! busy-lock miss. Now the miss is remembered on the service's entry and the
//! top of every pass retries the rollback, launching nothing until it lands.
//!
//! The busy lock is REAL: this test process takes `Manifest::lock` on the
//! environment's `.env.toml.lock` (a flock, the same one every writer takes)
//! and holds it across the moment the holder rolls back. Two processes: the
//! test (lock holder) and a throwaway `cauldron daemon` (THE CLAIM's subject).
//!
//! Claims:
//! 1. `a_rollback_missed_on_a_busy_lock_is_retried_every_pass_and_the_bad_pin_is_launched_0_more_times_outlasting_8s`
//!    — while the lock is held past the miss (8 s: four times
//!    `backoff_max_secs` = 2 s plus `poll_secs` = 1 s, the longest periods
//!    before a relaunch on that path), the bad pin is launched ZERO more
//!    times and the daemon says, every pass, that it is retrying the owed
//!    rollback. Released, the rollback lands (a `repin` record, why =
//!    rollback) and the GOOD build runs again — so "launch nothing, ever" is
//!    red too.
//! 3. `an_owed_retry_waiting_on_the_lock_does_not_overwrite_a_third_pin_written_by_that_lock_holder`
//!    — inspect R9.C7f7 F1: the operator's pin of a THIRD build lands while
//!    the owed retry is waiting on the lock (he is the writer it waits on).
//!    The rollback is compare-and-set under the lock, so his pin stands, no
//!    rollback is recorded, and the daemon says why. Opposed by claim 1, where
//!    nothing moves the pin and the rollback IS written.
//! 2. `an_owed_rollback_is_void_once_the_pin_moves_off_the_bad_build` — the
//!    opposing arm: while the rollback is owed, the pin is moved (by the lock
//!    holder, as a `cauldron pin` would) to the good build. The holder drops
//!    the debt, writes NO rollback, and launches the new pin — so "retry the
//!    rollback whatever the pin says" is red.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

#[path = "support/first_exec.rs"]
mod first_exec;

use ::env::decl::Manifest;

const FAKE: &str = r#"#!/usr/bin/env python3
# A fake hosted daemon: binds the port it is told and sits, accepting and
# closing every connection. Appends its pid to a file the test reads.
import os, socket, sys, time
if sys.argv[1:] == ["--build-stamp"]:
    print("commit=5eed5eed5eed5eed5eed5eed5eed5eed5eed5eed provenance=pin dirty=0 epoch=1")
    sys.exit(0)
port = int(os.environ["FAKE_PORT"])
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen(8)
with open(os.environ["FAKE_PIDS"], "a") as f:
    f.write(f"{os.getpid()}\n")
while True:
    c, _ = s.accept()
    c.close()
"#;

/// `stable_after_secs` = 6: the good build is recorded good after 6 s up;
/// the dying build lives 2 s, well inside it.
const STABLE: u64 = 6;
const DIES_AFTER: u64 = 2;
/// How long the lock is held PAST the miss: four times the 2 s backoff
/// ceiling, so with the retry removed the bad pin is relaunched inside it.
const HOLD: Duration = Duration::from_secs(8);

fn policy() -> String {
    format!(
        "auto_restart = true\nauto_rollback = true\npoll_secs = 1\ndown_after = 1\n\
         max_start_failures = 3\nbackoff_base_secs = 1\nbackoff_max_secs = 2\n\
         stable_after_secs = {STABLE}"
    )
}

/// The positive budget: up to 3 launches bounded by the declared start
/// timeout, backoff, the holder and a python start under load (as
/// `supervisor_restarts.rs` derives it).
const BUDGET: Duration = Duration::from_secs(3 * launch_wait::START_TIMEOUT_SECS + 30 + 30);
const FIRST_UP_DECLARED: Duration = Duration::from_secs(1 + launch_wait::START_TIMEOUT_SECS + 30);

fn first_up() -> Duration {
    first_exec::first_wait(FIRST_UP_DECLARED, 2)
}

/// The dying build. Its FIRST act after the stamp check, before it binds,
/// is to append a line to `bad-starts` beside `fake-pids`, so every launch
/// ATTEMPT of it is counted — one that dies before the holder sees it bind
/// (a failed start, which writes no `launch` record) included.
fn dying_fake(secs: u64) -> String {
    FAKE.replace(
        "port = int(os.environ[\"FAKE_PORT\"])\n",
        "with open(os.path.join(os.path.dirname(os.environ[\"FAKE_PIDS\"]), \"bad-starts\"), \"a\") as f:\n    \
         f.write(\"%d\\n\" % os.getpid())\nport = int(os.environ[\"FAKE_PORT\"])\n",
    )
    .replace(
        "while True:\n    c, _ = s.accept()\n    c.close()\n",
        &format!(
            "deadline = time.time() + {secs}\ns.settimeout(0.2)\nwhile time.time() < deadline:\n    \
             try:\n        c, _ = s.accept(); c.close()\n    except socket.timeout:\n        pass\n\
             sys.exit(3)\n"
        ),
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
            for p in children_of(d.id()) {
                let _ = Command::new("/bin/kill").args(["-9", &p.to_string()]).status();
            }
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

fn children_of(ppid: u32) -> Vec<u32> {
    let out = Command::new("/bin/ps").args(["-axo", "pid=,ppid="]).output().expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 2 && f[1].parse::<u32>().ok()? == ppid).then(|| f[0].parse().ok()).flatten()
        })
        .collect()
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
        std::fs::write(root.join("data").join(&env).join("env.toml"), format!("[supervisor]\n{}\n", policy()))
            .unwrap();
        let log = root.join("daemon.log");
        Sandbox { root, env, daemon: None, log }
    }

    fn manifest(&self) -> PathBuf {
        self.root.join("data").join(&self.env).join("env.toml")
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

    fn declare(&self, name: &str) -> u16 {
        let port = free_port();
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [launch]\naddr = \"127.0.0.1:{port}\"\n\
                 [env]\nFAKE_PORT = \"{port}\"\nFAKE_PIDS = \"{}\"\n\
                 [build]\nartifact = \"{}\"\nbin = \"fake-daemon\"\n",
                self.root.join("fake-pids").display(),
                self.root.join("fake-daemon").display()
            ),
        )
        .unwrap();
        port
    }

    /// Overwrite the artifact with `script`, build, pay its first exec, and
    /// return the NEW label.
    fn build_script(&self, name: &str, script: &str) -> String {
        let (_, before) = self.cauldron(&["versions", name, "--json"]);
        std::fs::write(
            self.root.join("fake-daemon"),
            launch_wait::slow_launch_py(
                &first_exec::slow_first_exec_py(script, &self.root.join("first-exec-marks")),
                &self.root.join("f2g-marks"),
            ),
        )
        .unwrap();
        Command::new("/bin/chmod").args(["+x"]).arg(self.root.join("fake-daemon")).status().unwrap();
        let (rc, out) = self.cauldron(&["build", name]);
        assert_eq!(rc, 0, "build {name}: {out}");
        let (rc, out) = self.cauldron(&["versions", name, "--json"]);
        assert_eq!(rc, 0, "versions {name}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        let new: Vec<String> = v
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| b["label"].as_str().map(str::to_string))
            .filter(|l| !before.contains(l.as_str()))
            .collect();
        assert_eq!(new.len(), 1, "expected exactly one new build label: {new:?}\n{out}");
        let dir = self.root.join("data").join("shared").join("builds").join(name).join(&new[0]);
        let exes = first_exec::executables_under(&dir);
        assert!(!exes.is_empty(), "sentinel: `build` left no executable in {}", dir.display());
        for x in exes {
            first_exec::pay_first_exec(Command::new(&x).arg("--build-stamp"), &format!("{name}'s build {}", new[0]));
        }
        new[0].clone()
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

    fn supervisions(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.root.join("data").join(&self.env).join("supervisions.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Every launch ATTEMPT of the dying build (see [`dying_fake`]).
    fn bad_starts(&self) -> usize {
        std::fs::read_to_string(self.root.join("bad-starts")).unwrap_or_default().lines().count()
    }

    fn rollbacks(&self, name: &str) -> Vec<serde_json::Value> {
        self.supervisions()
            .into_iter()
            .filter(|r| r["service"] == name && r["act"] == "repin" && r["why"] == "rollback")
            .collect()
    }

    fn good(&self, name: &str) -> Vec<String> {
        Manifest::load(&self.manifest()).map(|m| m.good(name)).unwrap_or_default()
    }

    fn pinned(&self, name: &str) -> Option<String> {
        Manifest::load(&self.manifest()).ok()?.pin(name)
    }
}

/// How many times the daemon logged a MISSED rollback write for `name`.
fn misses(log: &str, name: &str) -> usize {
    log.lines().filter(|l| l.contains(&format!("auto-rollback of `{name}` off")) && l.contains("NOT written")).count()
}

/// How many times the daemon logged retrying an OWED rollback for `name`.
fn retries(log: &str, name: &str) -> usize {
    log.lines().filter(|l| l.contains(&format!("`{name}` is OWED an auto-rollback"))).count()
}

/// Bring `svc-a` up on a good build, wait until it is recorded good, then pin
/// the dying build and TAKE THE WRITERS' LOCK, holding it until the holder
/// has missed its rollback write. Returns (sandbox, port, good, bad, lock).
fn up_to_the_missed_rollback(tag: &str) -> (Sandbox, u16, String, String, ::env::decl::ManifestLock) {
    let mut sb = Sandbox::new(tag);
    let port = sb.declare("svc-a");
    let good = sb.build_script("svc-a", FAKE);
    let (rc, out) = sb.cauldron(&["pin", "svc-a", &good]);
    assert_eq!(rc, 0, "{out}");
    sb.start_daemon();
    wait_for("the good build up", first_up(), || sb.up("svc-a", port));
    wait_for("the good build recorded good", BUDGET, || (sb.good("svc-a").first() == Some(&good)).then_some(()));

    let bad = sb.build_script("svc-a", &dying_fake(DIES_AFTER));
    assert_ne!(bad, good);
    let (rc, out) = sb.cauldron(&["pin", "svc-a", &bad]);
    assert_eq!(rc, 0, "{out}");
    // THE BUSY LOCK: the same flock every env.toml writer takes.
    let lock = Manifest::lock(&sb.manifest()).expect("take the env.toml writers' lock");
    wait_for("the holder to miss its rollback write on the busy lock", BUDGET, || {
        (misses(&sb.log(), "svc-a") >= 1).then_some(())
    });
    let log = sb.log();
    // Premises (doctrine 9): the rollback really was decided after exactly
    // max_start_failures = 3 launches of the bad build, and really was NOT
    // written — env.toml still pins the bad build and no rollback is recorded.
    assert_eq!(sb.bad_starts(), 3, "premise: exhausted after 3 launch attempts of the bad pin:\n{log}");
    assert_eq!(sb.pinned("svc-a").as_deref(), Some(bad.as_str()), "premise: the re-pin was written through a held lock");
    assert!(sb.rollbacks("svc-a").is_empty(), "premise: a rollback was recorded through a held lock:\n{log}");
    (sb, port, good, bad, lock)
}

#[test]
fn a_rollback_missed_on_a_busy_lock_is_retried_every_pass_and_the_bad_pin_is_launched_0_more_times_outlasting_8s() {
    let (sb, port, good, bad, lock) = up_to_the_missed_rollback("owed");
    let at_miss = sb.bad_starts();
    let t0 = Instant::now();
    std::thread::sleep(HOLD);
    let log = sb.log();
    // THE CLAIM: no launch of the known-bad pin while its rollback is owed.
    assert_eq!(
        sb.bad_starts() - at_miss,
        0,
        "the bad pin was launched again while its rollback was owed (lock held {:?}):\n{log}",
        t0.elapsed()
    );
    assert!(!listening(port), "something is up on the bad pin's port while the rollback is owed:\n{log}");
    // ...and the rollback is retried, once a pass, not forgotten.
    assert!(retries(&log, "svc-a") >= 3, "the owed rollback was not retried every pass over {HOLD:?}:\n{log}");
    drop(lock);
    // Released: the next retry writes the rollback, and the good build runs.
    let rb = wait_for("the owed rollback written", BUDGET, || sb.rollbacks("svc-a").into_iter().next());
    assert_eq!(rb["was_version"], bad.as_str(), "{rb}");
    assert_eq!(rb["version"], good.as_str(), "{rb}");
    assert_eq!(sb.pinned("svc-a").as_deref(), Some(good.as_str()), "env.toml not re-pinned to the good build");
    wait_for("the good build relaunched after the owed rollback", BUDGET, || sb.up("svc-a", port));
    let log = sb.log();
    assert_eq!(sb.bad_starts(), 3, "the bad pin was launched past max_start_failures = 3:\n{log}");
}

#[test]
fn an_owed_rollback_is_void_once_the_pin_moves_off_the_bad_build() {
    let (sb, port, good, bad, lock) = up_to_the_missed_rollback("void");
    // The operator's re-pin lands while the rollback is owed: written by the
    // lock's holder (this process), exactly as `cauldron pin` writes it.
    let mut m = Manifest::load(&sb.manifest()).unwrap();
    m.set_version("svc-a", &good);
    m.save(&sb.manifest()).unwrap();
    wait_for("the holder to drop the owed rollback", BUDGET, || {
        sb.log().contains("so the rollback is no longer owed").then_some(())
    });
    wait_for("the newly pinned build launched", BUDGET, || sb.up("svc-a", port));
    let log = sb.log();
    assert!(sb.rollbacks("svc-a").is_empty(), "a void rollback was written:\n{log}");
    assert_eq!(sb.bad_starts(), 3, "the bad pin was launched again:\n{log}");
    drop(lock);
}

#[test]
fn an_owed_retry_waiting_on_the_lock_does_not_overwrite_a_third_pin_written_by_that_lock_holder() {
    let (sb, _port, good, bad, lock) = up_to_the_missed_rollback("cas");
    // A third build, never proven good: not the rollback target.
    let g2 = sb.build_script("svc-a", &FAKE.replace("# A fake hosted daemon:", "# A fake hosted daemon (g2):"));
    assert!(g2 != good && g2 != bad, "sentinel: three distinct builds");
    // Land the pin INSIDE a retry's lock wait: right after a fresh retry line,
    // which is printed just before the retry's HOLDER_ENV_LOCK_BOUND (500 ms)
    // wait on this very lock.
    let n = retries(&sb.log(), "svc-a");
    wait_for("a fresh owed-retry line", BUDGET, || (retries(&sb.log(), "svc-a") > n).then_some(()));
    let t_seen = Instant::now();
    let mut m = Manifest::load(&sb.manifest()).unwrap();
    m.set_version("svc-a", &g2);
    m.save(&sb.manifest()).unwrap();
    drop(lock);
    let released = t_seen.elapsed();
    wait_for("the holder to drop the stale rollback", BUDGET, || {
        let l = sb.log();
        (l.contains("NOT rolling `svc-a` back") || !sb.rollbacks("svc-a").is_empty()).then_some(())
    });
    std::thread::sleep(Duration::from_secs(3));
    let log = sb.log();
    // THE CLAIM: his pin stands and nothing was recorded as a rollback.
    assert_eq!(
        sb.pinned("svc-a").as_deref(),
        Some(g2.as_str()),
        "the operator's later pin of g2 was overwritten by the owed rollback (released {released:?} after the \
         retry line; rollbacks {:?}):\n{log}",
        sb.rollbacks("svc-a")
    );
    assert!(sb.rollbacks("svc-a").is_empty(), "a stale rollback was recorded:\n{log}");
    // Sentinel (doctrine 9): the pin landed DURING the retry's wait, so it was
    // the compare-and-set that refused — not the pass-start void, which a pin
    // landing between passes would exercise instead.
    assert!(
        log.contains("NOT rolling `svc-a` back"),
        "the race was not exercised inside a retry's lock wait (released {released:?} after the retry line):\n{log}"
    );
}
