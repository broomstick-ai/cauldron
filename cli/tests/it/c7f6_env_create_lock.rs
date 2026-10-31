//! **R9.C7f6 (gap C-7, inspect R9.C7f5 F2, ledger R9.C7f5-2) — `cauldron env
//! create` WRITES env.toml UNDER THE env.toml WRITERS' LOCK.**
//!
//! `env create` checks, unlocked, that the environment's directory does not
//! exist, then builds and saves a fresh env.toml. That save used to be a
//! blind write outside `Manifest::lock`: a `cauldron pin` (whose
//! `ensure_manifest` creates the environment and pins) or a second `env
//! create` of the same name landing between the check and the save had its
//! env.toml overwritten. The create now takes the lock, re-checks under it,
//! and refuses — leaving the other writer's directory alone — if env.toml
//! appeared.
//!
//! The interleaving is made certain: `CAULDRON_TEST_CREATE_WINDOW_MS` holds
//! process A between its unlocked check and the lock, and A says on stderr
//! when it is there; only then does the other writer run (doctrine 4: the
//! window is injected and named in the test name).
//!
//! Every test runs a THROWAWAY cauldron: its own `CAULDRON_HOME` under the
//! system temp dir. Nothing touches `~/.cauldron` and no daemon is started.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

const WINDOW_MS: u64 = 3000;
/// For the held-lock pair: A's window, and how long the other process holds
/// the lock measured from when A entered that window.
const SHORT_WINDOW_MS: u64 = 1000;
const HOLD_MS: u64 = 4000;

struct Sandbox {
    root: PathBuf,
    env: String,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "cauldron-c7f6-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("c7f6{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        let sb = Sandbox { root, env };
        sb.declare("s1");
        assert!(!sb.data_root().exists(), "the environment must not exist yet");
        sb
    }

    fn data_root(&self) -> PathBuf {
        self.root.join("data").join(&self.env)
    }

    fn manifest(&self) -> PathBuf {
        self.data_root().join("env.toml")
    }

    /// A registering service: `pin <it> latest` needs no build and no address.
    fn declare(&self, name: &str) {
        std::fs::write(
            self.root.join("services").join(format!("{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"daemon\"\nregisters = true\nprobe = \"seam\"\n\
                 stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
                 [build]\nartifact = \"{}\"\nbin = \"fake\"\n",
                self.root.join("fake").display()
            ),
        )
        .unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cauldron"));
        c.args(args)
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .env_remove("CAULDRON_TEST_CREATE_WINDOW_MS")
            .env_remove("CAULDRON_TEST_ENSURE_WINDOW_MS")
            .env_remove("CAULDRON_TEST_PIN_WINDOW_MS")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn create_cmd(&self, auto_restart: bool) -> Command {
        let mut args = vec!["env", "create", self.env.as_str()];
        if auto_restart {
            args.push("--auto-restart");
        }
        self.cmd(&args)
    }

    fn toml(&self) -> toml::Value {
        let t = std::fs::read_to_string(self.manifest()).unwrap();
        toml::from_str(&t).unwrap_or_else(|e| panic!("{e}: {t}"))
    }

    fn auto_restart(&self) -> bool {
        self.toml()
            .get("supervisor")
            .and_then(|s| s.get("auto_restart"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    fn pinned(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .toml()
            .get("services")
            .and_then(|s| s.as_table())
            .map(|t| t.keys().cloned().collect())
            .unwrap_or_default();
        out.sort();
        out
    }
}

fn run(mut c: Command) -> (i32, String) {
    let o = c.output().expect("spawn cauldron");
    (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

/// A running `env create` held in its window, with its output collected.
struct Held {
    child: Child,
    reader: std::thread::JoinHandle<String>,
    entered: Instant,
}

impl Held {
    /// Spawn `c` with the create window set to `ms`, and return once it says
    /// it is inside the window (past its unlocked check, before the lock).
    fn spawn(mut c: Command, ms: u64) -> Held {
        c.env("CAULDRON_TEST_CREATE_WINDOW_MS", ms.to_string());
        let mut child = c.spawn().expect("spawn env create A");
        let err = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut all = String::new();
            for line in BufReader::new(err).lines() {
                let line = line.unwrap_or_default();
                if line.contains("env create window open") {
                    let _ = tx.send(());
                }
                all.push_str(&line);
                all.push('\n');
            }
            all
        });
        rx.recv_timeout(Duration::from_secs(60)).expect("env create A never reached its window");
        Held { child, reader, entered: Instant::now() }
    }

    fn finish(mut self) -> (i32, String, Duration) {
        let st = self.child.wait().unwrap();
        let took = self.entered.elapsed();
        let mut text = String::new();
        self.child.stdout.take().unwrap().read_to_string(&mut text).unwrap();
        text.push_str(&self.reader.join().unwrap());
        (st.code().unwrap_or(-1), text, took)
    }
}

/// GUARD (a): two real `env create` processes on one environment. A is held
/// between its unlocked check and its write; B creates the environment with
/// `--auto-restart` inside that window. A must refuse, and B's env.toml —
/// its `auto_restart = true` — must survive whole. Forced red by reverting to
/// the unlocked save: A overwrites it with the default (`auto_restart =
/// false`) and reports success.
#[test]
fn c7f6_a_two_creates_one_held_3000ms_one_wins_and_nothing_is_lost() {
    let sb = Sandbox::new("cc");
    let a = Held::spawn(sb.create_cmd(false), WINDOW_MS);
    assert!(!sb.data_root().exists(), "the environment exists before B ran: the race was not set up");

    let (rc, out) = run(sb.create_cmd(true));
    assert_eq!(rc, 0, "create B: {out}");
    assert!(a.entered.elapsed() < Duration::from_millis(WINDOW_MS), "B finished after A's window closed");
    assert!(sb.auto_restart(), "sentinel: B's env.toml does not carry its auto_restart before A resumes");

    let (a_rc, a_text, _) = a.finish();
    assert!(sb.auto_restart(), "B's auto_restart was lost: A overwrote env.toml\nA:\n{a_text}\nB:\n{out}");
    assert_ne!(a_rc, 0, "A reported success on an environment B had created:\n{a_text}");
    assert!(a_text.contains("REFUSED"), "A did not say it refused: {a_text}");
    assert!(sb.data_root().is_dir(), "A removed the directory B created");
}

/// GUARD (a'): the F2 race itself. A (`env create`) is held in its window; B
/// (`pin s1`) creates the environment through `ensure_manifest` and pins. A
/// must refuse and B's pin must survive. Forced red by the unlocked save: A's
/// empty manifest replaces B's, and the pin is lost.
#[test]
fn c7f6_b_create_held_3000ms_does_not_overwrite_a_concurrent_pin() {
    let sb = Sandbox::new("cp");
    let a = Held::spawn(sb.create_cmd(false), WINDOW_MS);
    assert!(!sb.data_root().exists(), "the environment exists before B ran: the race was not set up");

    let (rc, out) = run(sb.cmd(&["pin", "s1", "latest"]));
    assert_eq!(rc, 0, "pin B: {out}");
    assert!(a.entered.elapsed() < Duration::from_millis(WINDOW_MS), "B finished after A's window closed");
    assert_eq!(sb.pinned(), vec!["s1".to_string()], "sentinel: B's pin is not in env.toml before A resumes");

    let (a_rc, a_text, _) = a.finish();
    assert_eq!(sb.pinned(), vec!["s1".to_string()], "a concurrent pin was lost\nA:\n{a_text}\nB:\n{out}");
    assert_ne!(a_rc, 0, "A reported success on an environment B had created:\n{a_text}");
}

/// GUARD (b): another PROCESS (this test) holds the env.toml lock, without
/// writing env.toml, from inside A's window until `HOLD_MS` after A entered
/// it. A must wait for the lock — it is still running just before the
/// release — and then succeed with its own content. Forced red by the
/// unlocked save: A does not wait, and has exited long before the release.
#[test]
fn c7f6_c_create_waits_for_a_lock_held_4000ms_then_succeeds() {
    let sb = Sandbox::new("lk");
    let mut a = Held::spawn(sb.create_cmd(true), SHORT_WINDOW_MS);
    std::fs::create_dir_all(sb.data_root()).unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(sb.data_root().join(".env.toml.lock"))
        .unwrap();
    lock.try_lock().expect("sentinel: the lock was free when this process took it");
    let release_at = a.entered + Duration::from_millis(HOLD_MS);
    std::thread::sleep(release_at.saturating_duration_since(Instant::now() + Duration::from_millis(300)));
    // Just before the release: A is past its window (by ~2.7s) and blocked.
    assert!(a.entered.elapsed() > Duration::from_millis(SHORT_WINDOW_MS + 1500), "sentinel: A's window has not closed yet");
    assert!(a.child.try_wait().unwrap().is_none(), "A did not wait for the held lock");
    assert!(!sb.manifest().exists(), "A wrote env.toml while another process held the lock");
    std::thread::sleep(release_at.saturating_duration_since(Instant::now()));
    drop(lock);

    let (a_rc, a_text, took) = a.finish();
    assert_eq!(a_rc, 0, "A: {a_text}");
    assert!(took >= Duration::from_millis(HOLD_MS), "A finished before the lock was released: {took:?}");
    assert!(a_text.contains("created environment"), "{a_text}");
    assert!(sb.auto_restart(), "A's own content is not in env.toml");
}

/// Opposing (b): the same create with nobody holding the lock does not wait
/// — it finishes well before `HOLD_MS` — and creates the environment. A
/// create that always waited, or always refused, fails here.
#[test]
fn c7f6_d_create_with_the_lock_free_does_not_wait_and_creates() {
    let sb = Sandbox::new("free");
    let a = Held::spawn(sb.create_cmd(true), SHORT_WINDOW_MS);
    let (a_rc, a_text, took) = a.finish();
    assert_eq!(a_rc, 0, "A: {a_text}");
    assert!(took < Duration::from_millis(HOLD_MS), "A waited with the lock free: {took:?}");
    assert!(a_text.contains("created environment"), "{a_text}");
    assert!(sb.auto_restart());
    // And a second create of the now-existing name is refused, unchanged.
    let (rc, out) = run(sb.create_cmd(false));
    assert_ne!(rc, 0, "{out}");
    assert!(sb.auto_restart(), "a refused second create changed env.toml");
}
