//! # A FRESH EXECUTABLE'S FIRST EXEC IS NOT WHAT A PREFLIGHT TEST MEASURES
//!
//! `update::self_check` gives a candidate `SELF_CHECK_TIMEOUT` (20 s) to answer
//! `--self-check`. The verb itself returns in milliseconds (`main.rs`
//! `self_check()` prints and exits before logging, config or any file). But a
//! test candidate is a file this test JUST WROTE (`fs::copy` of
//! `CARGO_BIN_EXE_threads`, or a `codesign --force` that gives it a new
//! CDHash), and on this machine the FIRST exec of a newly written executable
//! waits on the system's per-file assessment before `main` runs. That wait is
//! shared, machine-wide state: it grows with how many other fresh executables
//! every other test binary and every concurrent cargo build is writing at the
//! same moment. Measured (R9.F2, 2026-10-16) on a 64 MB debug `threads`:
//! first exec 5.3 s / second exec 0.12 s at load 160; and R9.C5b measured
//! fresh-file first execs of 6.6–20.5 s at load 16–20. The second exec of the
//! same file is ~0.1 s.
//!
//! So a preflight test whose candidate is a fresh file was really asserting
//! "the machine's executable scanner answered within 20 s", and went red in the
//! full suite and green alone. The fix is to pay the first exec HERE, outside
//! the timed window, with a budget sized from that measurement
//! ([`FIRST_EXEC_BUDGET`], ~6x the worst first exec measured), so the preflight
//! under test times its own work and nothing else.
//!
//! Included with `#[path = "support/first_exec.rs"] mod first_exec;`.

#![allow(dead_code)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Worst fresh-file first exec measured on this machine is 20.5 s (R9.C5b);
/// six times that. A first exec slower than this is a machine that cannot run
/// the suite, and it says so rather than timing out somewhere downstream.
pub const FIRST_EXEC_BUDGET: Duration = Duration::from_secs(120);

/// **THE FIXTURE SCRIPTS PAY THE SAME TOLL.** `tests/fixtures/fake-claude` and
/// `fake-claude-capture` are files in THIS checkout: every new worktree (every
/// step, every inspector) holds fresh copies, and the first exec of a fresh
/// script waits on the same per-file assessment — measured 5.5 s at load 13 and
/// ~14 s under a fresh-executable storm (R9.F2), against a 0.06 s second exec.
/// Whichever test execs the fixture FIRST pays it inside its own `eventually`
/// window (10–20 s), which is why the red moved from binary to binary and was
/// always green when re-run alone (the full run had already paid it).
/// `selection_prereqs` went 8 red of 11 on a fresh `fake-claude` under storm.
///
/// `warmed` takes the fixture's path relative to the crate, execs it once per
/// test process in a mode with no side effects (`/usage` for `fake-claude`:
/// answered from argv before stdin is read; no args and empty stdin for
/// `fake-claude-capture`: no capture path, so it writes nothing), with
/// `CLAUDE_CONFIG_DIR` removed so no inherited login dir is ever read, and
/// returns the same path for the caller's `join`. Every test in the process
/// that asks for it waits for that one exec, so none of them times it.
pub fn warmed(rel: &'static str) -> &'static str {
    warm_once(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel));
    rel
}

/// [`warmed`] for a path that is already absolute (`concat!(env!(..), ..)`).
pub fn warmed_abs(abs: &'static str) -> &'static str {
    warm_once(std::path::PathBuf::from(abs));
    abs
}

fn warm_once(path: std::path::PathBuf) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static DONE: OnceLock<Mutex<HashSet<std::path::PathBuf>>> = OnceLock::new();
    // Held across the exec on purpose: a second test asking for the same
    // fixture must wait for the first exec rather than start its own.
    let mut done = DONE
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if done.contains(&path) {
        return;
    }
    let args: &[&str] = match path.file_name().and_then(|n| n.to_str()) {
        Some("fake-claude") => &["/usage"],
        // R13.1: hands `/usage` to `fake-claude` before reading stdin or
        // writing anything, so it is side-effect free the same way.
        Some("fake-claude-resend") => &["/usage"],
        Some("fake-claude-capture") => &[],
        other => panic!("first_exec::warmed has no side-effect-free mode for {other:?}"),
    };
    run_within_budget(&path, args);
    done.insert(path);
}

fn run_within_budget(path: &Path, args: &[&str]) -> Duration {
    let started = Instant::now();
    let mut child = Command::new(path)
        .args(args)
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("could not exec {} to warm it: {e}", path.display()));
    loop {
        match child.try_wait().expect("wait on the warm-up exec") {
            Some(_) => return started.elapsed(),
            None if started.elapsed() > FIRST_EXEC_BUDGET => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "{} did not finish its FIRST exec within {:?}",
                    path.display(),
                    FIRST_EXEC_BUDGET
                );
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Exec `path --self-check` once and wait for it, so the file's first-exec
/// cost is paid before any timed preflight runs it. Panics (naming the elapsed
/// time) if the binary does not answer within [`FIRST_EXEC_BUDGET`] or fails.
pub fn warm(path: &Path) -> Duration {
    let started = Instant::now();
    let mut child = Command::new(path)
        .arg(threads::update::SELF_CHECK_FLAG)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("could not exec {} to warm it: {e}", path.display()));
    loop {
        match child.try_wait().expect("wait on the warm-up exec") {
            Some(status) => {
                assert!(
                    status.success(),
                    "{} --self-check exited {status} during warm-up",
                    path.display()
                );
                return started.elapsed();
            }
            None if started.elapsed() > FIRST_EXEC_BUDGET => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "{} did not finish its FIRST exec within {:?} — the machine's \
                     fresh-executable assessment is slower than any measured here",
                    path.display(),
                    FIRST_EXEC_BUDGET
                );
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}
