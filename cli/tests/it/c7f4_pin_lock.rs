//! **R9.C7f4 (gap C-7, ledger R9.C7f3-2) — TWO CONCURRENT `cauldron pin`
//! WRITES NEVER LOSE ONE.**
//!
//! `pin` loads env.toml, changes one service's row and saves the whole file.
//! Two pins whose load-to-save spans overlapped both loaded the same file, and
//! the second save dropped the first pin. Every env.toml read-modify-write now
//! holds `Manifest::lock` (an exclusive flock on `.env.toml.lock`) from its
//! load to its save.
//!
//! The overlap is made certain, not hoped for: `CAULDRON_TEST_PIN_WINDOW_MS`
//! holds each pin `WINDOW_MS` between its load and its save (doctrine 4: the
//! period is injected and named in the test names). Without the lock, every
//! pin started together loads before any saves, so all but one pin is lost.
//!
//! Every test runs a THROWAWAY cauldron: its own `CAULDRON_HOME` under the
//! system temp dir. Nothing touches `~/.cauldron` and no daemon is started.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

const WINDOW_MS: u64 = 400;
const PINS: usize = 4;

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
            "cauldron-c7f4-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
        let env = format!("c7f4{tag}");
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::create_dir_all(root.join("data").join(&env)).unwrap();
        std::fs::write(root.join("data").join(&env).join("env.toml"), "[supervisor]\n").unwrap();
        let sb = Sandbox { root, env };
        for i in 0..PINS {
            sb.declare(&format!("s{i}"));
        }
        sb
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

    fn pin_cmd(&self, service: &str, window_ms: u64) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cauldron"));
        c.args(["pin", service, "latest"])
            .env("CAULDRON_HOME", &self.root)
            .env("CAULDRON_ENV", &self.env)
            .env("CAULDRON_TEST_PIN_WINDOW_MS", window_ms.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn pinned(&self) -> Vec<String> {
        let t = std::fs::read_to_string(self.root.join("data").join(&self.env).join("env.toml")).unwrap();
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

/// GUARD: four real `cauldron pin` processes, started together, each holding
/// a 400 ms load-to-save window: every pin lands in env.toml. Forced red by
/// removing the lock from `verb_pin` (a lost write: one pin survives).
#[test]
fn c7f4_a_four_concurrent_pins_all_land_across_a_400ms_load_to_save_window() {
    let sb = Sandbox::new("conc");
    let t0 = Instant::now();
    let children: Vec<_> = (0..PINS)
        .map(|i| sb.pin_cmd(&format!("s{i}"), WINDOW_MS).spawn().expect("spawn pin"))
        .collect();
    let mut outs = Vec::new();
    for ch in children {
        let o = ch.wait_with_output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        assert_eq!(o.status.code(), Some(0), "{text}");
        outs.push(text);
    }
    let took = t0.elapsed();
    let want: Vec<String> = (0..PINS).map(|i| format!("s{i}")).collect();
    assert_eq!(sb.pinned(), want, "a concurrent pin was lost (took {took:?})\n{}", outs.join("\n---\n"));
    // Sentinel that the windows really were serialised by the lock rather than
    // never overlapping: four 400 ms windows one after another.
    assert!(took >= Duration::from_millis(WINDOW_MS * PINS as u64), "pins did not queue on the lock: {took:?}");
}

/// Opposing: a single pin, with and without the window, still lands — and the
/// lock is released when it exits (a second pin after it is not blocked).
#[test]
fn c7f4_b_a_single_pin_lands_and_releases_the_lock_after_a_2000ms_window() {
    let sb = Sandbox::new("one");
    let t0 = Instant::now();
    let (rc, out) = run(sb.pin_cmd("s0", 2000));
    assert_eq!(rc, 0, "{out}");
    // Sentinel: the injected window is in effect (the instrument of test a).
    // 2000 ms, not WINDOW_MS: an unwindowed pin can itself take 400 ms (seen
    // when this was forced), so only a window well past a pin's own run time
    // can fail this check.
    assert!(t0.elapsed() >= Duration::from_millis(2000), "the test window was not applied: {:?}", t0.elapsed());
    assert_eq!(sb.pinned(), vec!["s0".to_string()]);
    let (rc, out) = run(sb.pin_cmd("s1", 0));
    assert_eq!(rc, 0, "{out}");
    assert_eq!(sb.pinned(), vec!["s0".to_string(), "s1".to_string()]);
}
