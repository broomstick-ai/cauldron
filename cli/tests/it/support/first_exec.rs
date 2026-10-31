//! **THE FIRST EXEC OF A FRESHLY WRITTEN EXECUTABLE IS A PERIOD, AND IT IS
//! DECLARED HERE (R9.F1).**
//!
//! macOS assesses a newly written executable FILE (XProtect) before its first
//! exec, once per file: a second exec of the same file is not assessed again,
//! and a byte-identical copy at a new path is. The cost is set by how busy the
//! machine-wide scanner is, not by the file, and on this machine it is large:
//!
//! * R9.C5b (r31, eight steps building): a fresh two-line `#!/bin/sh` script
//!   took 3.5–**20.46 s** to its first exec and 0.03–0.06 s to its second; a
//!   fresh copy of debug `cauldron` 4.72–8.62 s, then 0.012 s.
//! * MV-C3 (r31): a fresh copy of debug `cauldron` 13.34 s to `--help`, then
//!   0.00 s.
//! * R9.F1 (r32, load 28–55): fresh `#!/bin/sh` scripts 14.76, 10.07,
//!   10.11 s first, 0.04–0.12 s second; a fresh `cauldron` copy 8.57 s, then
//!   0.04 s.
//!
//! Every supervisor test writes such files — a fixture script, the build copy
//! `cauldron build` makes of it, and the holder's own launch path
//! `<root>/bin/cauldron-<env>`, which `daemon` copies from its binary and
//! execs — and execs them inside deadlines: the test's own first wait, and
//! the product's own launch deadlines (`start_timeout_ms`, `HOSTED_BIND_WAIT`),
//! where a first exec that overruns counts as a FAILED START against
//! `max_start_failures` (with `auto_restart = false`, a failed first launch is
//! never retried at all). That charged the scanner to the supervisor.
//!
//! So each test file here does two things with this module:
//!
//! 1. [`pay_first_exec`] — every executable the test can name before the
//!    clock starts is exec'd once, here, with no deadline, and the time it
//!    took is printed. The deadlines then measure the supervisor again.
//! 2. [`first_wait`] — every FIRST wait (the first sight of a thing the test
//!    just caused) is sized as the DECLARED period on its path plus
//!    [`first_exec_worst`] for each fresh exec on that path, and the period
//!    is named where it is used. A longer positive wait costs time only on a
//!    run that is failing anyway; a negative wait is never sized from this.
//!
//! And one thing for proving it: [`slow_first_exec_py`] / [`slow_first_exec_sh`]
//! inject a forced slow first exec into a fixture (doctrine 1), keyed per FILE
//! exactly as the scanner is, when `CAULDRON_TEST_SLOW_FIRST_EXEC_SECS` is set.
//! Unset (the default), the fixtures are byte-identical to the ones without it.

#![allow(dead_code)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// **The measured worst-case first exec of a fresh executable on this
/// machine**: 20.46 s (R9.C5b, r31), rounded up to the next whole second. See
/// the module doc for every measurement behind it.
pub const FIRST_EXEC_WORST: Duration = Duration::from_secs(21);

/// The worst first exec THIS test binary has paid so far, in ms.
static WORST_PAID_MS: AtomicU64 = AtomicU64::new(0);

/// The first-exec cost to budget for: the declared [`FIRST_EXEC_WORST`], or
/// the worst one this run has actually paid in [`pay_first_exec`] if the
/// scanner is slower today than it has ever been measured.
pub fn first_exec_worst() -> Duration {
    FIRST_EXEC_WORST.max(Duration::from_millis(WORST_PAID_MS.load(Ordering::SeqCst)))
}

/// A FIRST wait: `declared` (the longest declared period on the path, named
/// by the caller) plus [`first_exec_worst`] for each of `fresh_execs` fresh
/// executables whose first exec is on that path.
pub fn first_wait(declared: Duration, fresh_execs: u32) -> Duration {
    declared + first_exec_worst() * fresh_execs
}

/// Exec `cmd` once, with no deadline, so its first-exec assessment is paid
/// here and not inside a wait. Its exit status is not the point (a fixture may
/// refuse to run without its environment); that it could be exec'd at all is
/// asserted. Returns, prints and records the time it took.
pub fn pay_first_exec(cmd: &mut Command, what: &str) -> Duration {
    let t0 = Instant::now();
    let st = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let took = t0.elapsed();
    eprintln!("R9.F1: first exec of {what}: {:.2}s ({st:?})", took.as_secs_f64());
    assert!(st.is_ok(), "could not exec {what}: {st:?}");
    WORST_PAID_MS.fetch_max(took.as_millis() as u64, Ordering::SeqCst);
    took
}

/// Every regular executable file under `dir` (recursively) — a build
/// directory's binary, wherever its declaration put it (a bundle nests it).
pub fn executables_under(dir: &Path) -> Vec<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
            if md.is_dir() {
                stack.push(p);
            } else if md.is_file() && md.permissions().mode() & 0o111 != 0 {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// The injected first-exec delay, in seconds, or 0 (off).
pub fn injected_secs() -> u64 {
    std::env::var("CAULDRON_TEST_SLOW_FIRST_EXEC_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// `script` (a `#!/usr/bin/env python3` fixture) with a FORCED SLOW FIRST
/// EXEC injected after its shebang: the first exec of each FILE (by real
/// path, recorded under `marks`) sleeps [`injected_secs`]; every later exec
/// of that file does not — the scanner's shape. Unchanged when the knob is off.
pub fn slow_first_exec_py(script: &str, marks: &Path) -> String {
    let n = injected_secs();
    if n == 0 {
        return script.to_string();
    }
    std::fs::create_dir_all(marks).unwrap();
    let (shebang, rest) = script.split_once('\n').expect("a shebang line");
    format!(
        "{shebang}\nimport os as _f1o, sys as _f1s, time as _f1t\n\
         _f1m = {marks:?} + '/' + _f1o.path.realpath(_f1s.argv[0]).replace('/', '%')\n\
         if not _f1o.path.exists(_f1m):\n    open(_f1m, 'w').close(); _f1t.sleep({n})\n{rest}",
        marks = marks.display().to_string()
    )
}

/// The same injection for a `#!/bin/sh` fixture.
pub fn slow_first_exec_sh(script: &str, marks: &Path) -> String {
    let n = injected_secs();
    if n == 0 {
        return script.to_string();
    }
    std::fs::create_dir_all(marks).unwrap();
    let (shebang, rest) = script.split_once('\n').expect("a shebang line");
    format!(
        "{shebang}\n_f1m=\"{}/$(/bin/realpath \"$0\" | /usr/bin/tr / %)\"\n\
         [ -e \"$_f1m\" ] || {{ : > \"$_f1m\"; /bin/sleep {n}; }}\n{rest}",
        marks.display()
    )
}
