//! **THE ONE LAUNCH WAIT EVERY CLI TEST FIXTURE DECLARES (R9.F2g).**
//!
//! A registering fixture declares `start_timeout_ms`: how long a launch (the
//! daemon's, or a synchronous `cauldron start`) waits for the fresh process
//! to write its own seam before it kills it and counts a FAILED START. Every
//! fixture here used to declare 10 s. R9.F2f measured why that was too short:
//! under full-suite load the host held fresh fakes at exec — sampled 4 s
//! after fork still at `_dyld_start` +0 of `/usr/bin/env`, not one
//! instruction run — so a FIRST launch failed for reasons that had nothing to
//! do with the code under test (`owned_live_pid` went red 4/4 in R9.F2f's
//! tip run with `did not write its seam … within 10000ms`; ledger
//! `R9.F2f-1`). F2f raised `daemon_owned_rows` alone to 60 s; this module is
//! that wait, shared, so every file declares the same one.
//!
//! This is the PREMISE's wait, not a claim about the product: the product's
//! own launch wait is whatever a declaration says, and nothing here asserts
//! on its length except the one opposing guard that proves a launch which
//! never comes up still fails within it (`owned_live_pid.rs`). Every positive
//! wait that a launch can sit inside must outlast [`START_TIMEOUT`], so a
//! launch that DOES time out still reds inside the test's own bound.
//!
//! And one thing for proving it (doctrine 1): [`slow_launch_py`] /
//! [`slow_launch_sh`] inject a forced SLOW FIRST LAUNCH into a fixture when
//! `CAULDRON_TEST_SLOW_LAUNCH_SECS` is set — the first LAUNCH (an exec that
//! carries `CAULDRON_SERVICE`, which only a launch sets; a `--build-stamp`
//! exec or a pre-paid first exec does not) of each FILE, by real path, sleeps
//! that long before it can write its seam. Unset (the default), the fixture
//! is byte-identical to the one without it.

#![allow(dead_code)]

use std::path::Path;
use std::time::Duration;

/// The declared `start_timeout_ms` of every fixture under `cli/tests`.
pub const START_TIMEOUT_MS: u64 = 60_000;

/// [`START_TIMEOUT_MS`] as a duration, for the bounds derived from it.
pub const START_TIMEOUT: Duration = Duration::from_millis(START_TIMEOUT_MS);

/// [`START_TIMEOUT_MS`] in whole seconds, for `const` bound arithmetic.
pub const START_TIMEOUT_SECS: u64 = START_TIMEOUT_MS / 1000;

/// The injected first-launch delay, in seconds, or 0 (off).
pub fn injected_secs() -> u64 {
    std::env::var("CAULDRON_TEST_SLOW_LAUNCH_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// `script` (a `#!/usr/bin/env python3` fixture) with a FORCED SLOW FIRST
/// LAUNCH injected after its shebang: the first exec of each FILE (by real
/// path, recorded under `marks`) that carries `CAULDRON_SERVICE` sleeps
/// [`injected_secs`] before anything else runs. Unchanged when the knob is off.
pub fn slow_launch_py(script: &str, marks: &Path) -> String {
    let n = injected_secs();
    if n == 0 {
        return script.to_string();
    }
    std::fs::create_dir_all(marks).unwrap();
    let (shebang, rest) = script.split_once('\n').expect("a shebang line");
    format!(
        "{shebang}\nimport os as _f2go, sys as _f2gs, time as _f2gt\n\
         _f2gm = {marks:?} + '/' + _f2go.path.realpath(_f2gs.argv[0]).replace('/', '%')\n\
         if _f2go.environ.get('CAULDRON_SERVICE') and not _f2go.path.exists(_f2gm):\n    \
         open(_f2gm, 'w').close(); _f2gt.sleep({n})\n{rest}",
        marks = marks.display().to_string()
    )
}

/// The same injection for a `#!/bin/sh` fixture.
pub fn slow_launch_sh(script: &str, marks: &Path) -> String {
    let n = injected_secs();
    if n == 0 {
        return script.to_string();
    }
    std::fs::create_dir_all(marks).unwrap();
    let (shebang, rest) = script.split_once('\n').expect("a shebang line");
    format!(
        "{shebang}\nif [ -n \"$CAULDRON_SERVICE\" ]; then _f2gm=\"{}/$(/bin/realpath \"$0\" | /usr/bin/tr / %)\"; \
         [ -e \"$_f2gm\" ] || {{ : > \"$_f2gm\"; /bin/sleep {n}; }}; fi\n{rest}",
        marks.display()
    )
}
