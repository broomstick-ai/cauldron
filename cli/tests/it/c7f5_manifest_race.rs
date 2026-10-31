//! **R9.C7f5 (gap C-7, inspect R9.C7f4 low 1) — A FIRST-CREATE OF env.toml
//! NEVER OVERWRITES A CONCURRENT `cauldron pin`.**
//!
//! `ensure_manifest` checks `env.toml` exists and, if not, creates an empty
//! one. That create used to be a blind save outside the env.toml writers'
//! lock: a `pin` that created AND pinned between another process's check and
//! its save was overwritten by the empty manifest, and its pin was lost. The
//! create now re-checks under the lock (`Manifest::create_if_absent`).
//!
//! The interleaving is made certain: `CAULDRON_TEST_ENSURE_WINDOW_MS` holds
//! process A between its unlocked check and its create, and A says on stderr
//! when it is there; only then does process B run its whole `pin` (doctrine 4:
//! the window is injected and named in the test name).
//!
//! Every test runs a THROWAWAY cauldron: its own `CAULDRON_HOME` under the
//! system temp dir. Nothing touches `~/.cauldron` and no daemon is started.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

const WINDOW_MS: u64 = 3000;

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
    /// A cauldron root with two declared services and NO data directory for
    /// the environment: the first `pin` into it is what creates it.
    fn new(tag: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "cauldron-c7f5-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("c7f5{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        let sb = Sandbox { root, env };
        sb.declare("s0");
        sb.declare("s1");
        assert!(!sb.manifest().exists(), "the environment must start with no env.toml");
        sb
    }

    fn manifest(&self) -> PathBuf {
        self.root.join("data").join(&self.env).join("env.toml")
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

    fn pin_cmd(&self, service: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cauldron"));
        c.args(["pin", service, "latest"])
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .env_remove("CAULDRON_TEST_ENSURE_WINDOW_MS")
            .env_remove("CAULDRON_TEST_PIN_WINDOW_MS")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn pinned(&self) -> Vec<String> {
        let t = std::fs::read_to_string(self.manifest()).unwrap();
        let v: toml::Value = toml::from_str(&t).unwrap_or_else(|e| panic!("{e}: {t}"));
        let mut out: Vec<String> = v
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

/// GUARD: process A (`pin s0`) is held between `ensure_manifest`'s unlocked
/// check and its create; process B (`pin s1`) meanwhile creates env.toml and
/// pins; then A creates-or-not and pins. Both pins land. Forced red by making
/// the create a blind save again (B's pin is overwritten: only s0 survives).
#[test]
fn c7f5_a_first_create_held_3000ms_does_not_overwrite_a_concurrent_pin() {
    let sb = Sandbox::new("race");
    let mut a = sb.pin_cmd("s0");
    a.env("CAULDRON_TEST_ENSURE_WINDOW_MS", WINDOW_MS.to_string());
    let mut a = a.spawn().expect("spawn pin A");
    let a_err = a.stderr.take().unwrap();
    let mut a_out = a.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(a_err).lines() {
            let line = line.unwrap_or_default();
            if line.contains("ensure_manifest window open") {
                let _ = tx.send(());
            }
            all.push_str(&line);
            all.push('\n');
        }
        all
    });
    // A is now past its existence check (it saw no env.toml) and waiting.
    rx.recv_timeout(Duration::from_secs(60)).expect("pin A never reached the ensure_manifest window");
    let t_in = Instant::now();
    assert!(!sb.manifest().exists(), "env.toml exists before B ran: the race was not set up");

    let (rc, out) = run(sb.pin_cmd("s1"));
    assert_eq!(rc, 0, "pin B: {out}");
    // Sentinel: B created and pinned INSIDE A's window, so A's create is the
    // one that would overwrite it.
    assert!(t_in.elapsed() < Duration::from_millis(WINDOW_MS), "B finished after A's window closed: {:?}", t_in.elapsed());
    assert_eq!(sb.pinned(), vec!["s1".to_string()], "B's pin is not in env.toml before A creates");
    assert!(out.contains("created environment"), "B did not create the environment: {out}");

    let st = a.wait().unwrap();
    let mut a_text = String::new();
    a_out.read_to_string(&mut a_text).unwrap();
    a_text.push_str(&reader.join().unwrap());
    assert_eq!(st.code(), Some(0), "pin A: {a_text}");
    assert_eq!(sb.pinned(), vec!["s0".to_string(), "s1".to_string()], "a concurrent pin was lost\nA:\n{a_text}\nB:\n{out}");
    assert!(!a_text.contains("created environment"), "A claimed to create an environment B had created: {a_text}");
}

/// Opposing: a single first `pin` into a new environment creates it and
/// lands its pin, and a second `pin` there does not re-create it.
#[test]
fn c7f5_b_a_single_first_create_still_creates_and_pins() {
    let sb = Sandbox::new("one");
    let (rc, out) = run(sb.pin_cmd("s0"));
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("created environment"), "{out}");
    assert_eq!(sb.pinned(), vec!["s0".to_string()]);
    let (rc, out) = run(sb.pin_cmd("s1"));
    assert_eq!(rc, 0, "{out}");
    assert!(!out.contains("created environment"), "re-created an existing environment: {out}");
    assert_eq!(sb.pinned(), vec!["s0".to_string(), "s1".to_string()]);
}
