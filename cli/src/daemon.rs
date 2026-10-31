//! **`cauldron daemon` — the process that hosts an environment's services.**
//!
//! The operator's ruling, verbatim: *"no, it's a daemon with a process. It's
//! eventually going to facilitate connection to other nodes in a mesh network.
//! It needs a supervisord script or whatever to keep it alive so it can keep
//! the rest alive."*
//!
//! So this is a long-lived process, one per environment, and the regress
//! bottoms out where it already bottoms out for four jobs on this machine
//! today:
//!
//! ```text
//!   launchd  com.broomstick.cauldron       -> cauldron        (prod)
//!            com.broomstick.cauldron-dev   -> cauldron-dev    (dev)
//!
//!   cauldron (one per environment) hosts the services its env.toml pins
//! ```
//!
//! **launchd keeps cauldron alive so cauldron can keep the services alive.**
//! That is the whole of the regress and it ends there honestly: launchd is PID
//! 1's agent manager, its failure mode is the login session being gone, and
//! that is the one failure no software in the session can observe or repair
//! and the one he notices without help.
//!
//! # What this is NOT, and the design pass it overrules
//!
//! `docs/supervision-model.md` proposed `cauldron supervise`, a **foreground
//! verb**. He ruled a daemon. The difference is not cosmetic: a foreground
//! verb is something a human runs and watches, so its lifetime is a terminal
//! session's and its name in `ps` is whatever the shell was. A daemon is a
//! process with a launchd job, a stable name and its own signing identity —
//! which is the whole of why there is one per environment (see
//! [`crate::launchd`]).
//!
//! It does not daemonize itself, and that is not a contradiction. Under
//! launchd a job that forks and exits is a job launchd thinks died; `KeepAlive`
//! would restart it forever. The process launchd starts IS the daemon. The
//! word names what it is — long-lived, supervised, nobody's child but PID 1's
//! agent manager — not a fork() it performs.
//!
//! # The singleton, and the sixteen days
//!
//! One holder per environment, and the hold is a **kernel lock** —
//! [`env::hold`]. Not a pidfile. The predecessor of this file wrote a pidfile
//! and exited 0 when `kill -0` on the recorded pid succeeded; on 2026-09-22 a
//! reboot left a stale pidfile whose pid had been **reused by an unrelated
//! process on the new boot**, so the new supervisor read it, found the number
//! alive, said "another supervisor is already running" and exited cleanly.
//! launchd honoured the clean exit, and the voice daemon went unsupervised for
//! sixteen days.
//!
//! Two consequences are written into this file rather than left as advice:
//!
//! * **Nothing is read in order to decide whether to run.** The hold is the
//!   kernel's answer about an open file description.
//! * **A refused hold exits NON-ZERO** ([`EXIT_HELD`]). Under the plist this
//!   file's installer writes, `KeepAlive` is unconditional, so no exit code
//!   can tell launchd to stand down. The 2026-09-22 failure needed both
//!   halves: a wrong answer, and an exit code that made launchd believe it.
//!
//! # What it does, once it holds an environment
//!
//! Re-attach, then launch, then watch. On start it loads `seams.json` and
//! probes every row; a row that is still live is a service a previous
//! incarnation of this daemon started, and it is left exactly as it is. That
//! is why launchd restarting cauldron does not disturb a running service: the
//! children are in their own process groups, so they are not in the job's, and
//! the daemon finds them again through the registry rather than through
//! parentage.
//!
//! Everything pinned in `env.toml` with no live row is launched. A service is
//! down when its liveness check has failed `down_after` times in a row — never
//! on the first failure, because the single most expensive misreading
//! available to a supervisor is treating a stall as a death and launching a
//! competitor.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use env::decl::{self, Manifest, Supervisor as Policy};
use env::hold::{self, Attempt, Hold};
use env::seams::{self, SeamState, ServiceEnv, Table};
use env::supervisions;
use env::warnings::{Answered, Conn, Export, Kind, Service, ServesWarnings, Verb};
use env::{guard, Environment};

/// **The exit code for "another process holds this environment".**
///
/// Non-zero, deliberately and load-bearingly. `KeepAlive` in the generated
/// plist is unconditional (`true`, never `SuccessfulExit = false`), so launchd
/// restarts this job whatever it exits with — but the code still has to be
/// honest, because a human and a script both read it, and because the thing
/// that went wrong in 2026-09-22 was a refusal reported as a success.
pub const EXIT_HELD: i32 = 9;

/// The service name the daemon registers for itself. Joins `logs` in
/// `RESERVED_SERVICE_NAMES` so no declaration can claim it.
pub const SELF_SERVICE: &str = "cauldron";

/// **THE SUPERVISOR'S OWN VERSION LABEL**, derived from its own commit by the
/// same rule `cauldron build` uses for everything else — twelve characters of
/// the commit, plus `+dirty` when the tree that produced it was not clean.
///
/// It is derived rather than stored because the supervisor is not installed by
/// `cauldron build` and so has no build record to read. What it must not be is
/// a hand-written string: the one row in the registry written by the process
/// that refuses to start a service whose build record names no commit cannot
/// be the row that names none.
fn self_version() -> String {
    let c = env!("CAULDRON_SELF_COMMIT");
    if c.is_empty() {
        // Honest, and visible. This build was made outside a git worktree, so
        // it has no version identity — and `status` will show exactly that
        // rather than a plausible-looking label.
        return "unstamped".to_string();
    }
    let short: String = c.chars().take(12).collect();
    // Fail-closed (R1.S3b): only `0` is clean. `unknown` — the build script
    // could not run `git status`, could not parse it, or could not derive the
    // dirt scope — is rendered as such, never as a bare clean label.
    match env!("CAULDRON_SELF_DIRTY") {
        "0" => short,
        "1" => format!("{short}+dirty"),
        _ => format!("{short} (dirt unknown)"),
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ═════════════════════════════════════════════════════ THE WARNINGS SHAPE ════

/// The daemon's own warnings, counted by CALL SITE like any producer's.
///
/// # Why the supervisor is under the obligation it imposes
///
/// Because it registers a seam. `ServiceEnv::register` is the call that makes
/// a process visible to cauldron at all, it takes an `Attestation`, and the
/// only way to mint one is `attest_warnings!`, which does not type-check
/// without a `ServesWarnings` impl. The daemon registers, so the daemon
/// implements it, so the compile-time check runs against the supervisor on
/// exactly the terms it runs against `echo`.
///
/// That is not symmetry for its own sake. The supervisor is the one producer
/// whose warnings nobody else can report: a restart, a backoff, an auto-
/// rollback and a park are facts about supervision, and the only process that
/// knows them is the one doing it. Before this there was nowhere for them to
/// go but a log.
mod counters {
    use super::*;

    pub static WARN: AtomicU64 = AtomicU64::new(0);
    pub static ERROR: AtomicU64 = AtomicU64::new(0);

    pub struct Row {
        pub count: u64,
        pub level: &'static str,
        pub message: String,
        /// How many times IN A ROW the current message has been recorded at
        /// this site. Resets to 1 when the message changes. Drives `emits`.
        pub streak: u64,
        /// The current message in full, so a change is detected on the whole
        /// text and not on the 160-char prefix kept for the shape.
        pub last: String,
    }

    pub struct State {
        pub kinds: BTreeMap<(&'static str, u32), Row>,
        pub sink: Option<PathBuf>,
        pub sink_max_bytes: u64,
        pub sink_errors: u64,
    }

    pub static STATE: Mutex<State> = Mutex::new(State {
        kinds: BTreeMap::new(),
        sink: None,
        sink_max_bytes: SINK_MAX_BYTES,
        sink_errors: 0,
    });

    /// **THE DISK BOUND (DG-3): 4 MiB live, one rotated `.1`, 8 MiB total** —
    /// the bound threads puts on its own warnings record
    /// (`services/threads/src/warnings.rs`, `WARNINGS_MAX_BYTES`) and
    /// `shapewire::warnings::SINK_MAX_BYTES`, both of which read
    /// `shapewire::ledger::LEDGER_MAX_BYTES`. Restated here because this crate
    /// does not depend on shapewire; the unit test below pins the value.
    pub const SINK_MAX_BYTES: u64 = 4 * 1024 * 1024;

    pub fn attach(dir: &std::path::Path) {
        attach_bounded(dir, SINK_MAX_BYTES);
    }

    pub fn attach_bounded(dir: &std::path::Path, max_bytes: u64) {
        if let Ok(mut st) = STATE.lock() {
            st.sink = Some(dir.join("warnings").join("warnings.jsonl"));
            st.sink_max_bytes = max_bytes;
        }
    }

    /// Append one line, rotating first when it would carry the live file past
    /// `max_bytes`: rename the live file to `<path>.1` (dropping the old `.1`),
    /// then append to a fresh one — the `.1`-rename of
    /// `shapewire::ledger`'s rotation. The newest line is always in the live
    /// file; the total stays under `2 * max_bytes` plus one line.
    pub fn append_bounded(path: &std::path::Path, max_bytes: u64, line: &str) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let held = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if held > 0 && held + line.len() as u64 + 1 > max_bytes {
            let mut rotated = path.as_os_str().to_os_string();
            rotated.push(".1");
            std::fs::rename(path, PathBuf::from(rotated))?;
        }
        let mut fh = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(fh, "{line}")
    }

    /// Whether the `streak`-th consecutive identical event at one site is
    /// EMITTED (stderr and the sink) or only COUNTED.
    ///
    /// The first is always emitted; after that, the 2nd, 4th, 8th, 16th …
    /// A defect the daemon re-meets every poll — a pinned service whose
    /// declaration will not parse, four seconds apart, forever — used to
    /// write four identical lines every four seconds for as long as the
    /// daemon lived, which is a log nobody reads and a `warnings.jsonl` that
    /// grows without bound while saying one thing. Powers of two keep the
    /// record's growth logarithmic, keep every emitted line stating how
    /// many it stands for, and need no clock — so the rule is the same in a
    /// test as in the daemon.
    ///
    /// One function so the policy can be forced red on its own.
    pub fn emits(streak: u64) -> bool {
        streak.is_power_of_two()
    }

    /// Record one event at a call site, and — unless it is a suppressed
    /// repeat — ALSO put it on stderr. Returns whether it was emitted.
    ///
    /// Both, deliberately. The counts are what a consumer pulls; stderr is
    /// where launchd's `StandardErrorPath` sends it, which is the only record
    /// that survives the daemon dying — and a supervisor's most interesting
    /// warnings are the ones it emitted just before it stopped being able to
    /// answer for them.
    ///
    /// THE COUNT IS NEVER SUPPRESSED. Every call increments the level total
    /// and the site's row; only the stderr line and the sink record are held
    /// back, per `emits`, when the same site reports the same message again.
    /// A changed message at a site is emitted at once and starts a new
    /// streak, so a site that alternates between two defects hides neither.
    pub fn record(level: &'static str, file: &'static str, line: u32, message: &str) -> bool {
        match level {
            "error" => ERROR.fetch_add(1, Ordering::Relaxed),
            _ => WARN.fetch_add(1, Ordering::Relaxed),
        };
        let Ok(mut st) = STATE.lock() else {
            // A poisoned lock still gets the line: counting is what broke,
            // not the event.
            eprintln!("cauldron[{level}] {message}");
            return true;
        };
        let e = st.kinds.entry((file, line)).or_insert_with(|| Row {
            count: 0,
            level,
            message: message.chars().take(160).collect(),
            streak: 0,
            last: String::new(),
        });
        e.count += 1;
        if e.last == message {
            e.streak += 1;
        } else {
            e.streak = 1;
            e.last = message.to_string();
            e.message = message.chars().take(160).collect();
        }
        let streak = e.streak;
        if !emits(streak) {
            return false;
        }
        if streak == 1 {
            eprintln!("cauldron[{level}] {message}");
        } else {
            eprintln!(
                "cauldron[{level}] {message} (x{streak} in a row at this site; next line at x{})",
                streak * 2
            );
        }
        let Some(path) = st.sink.clone() else { return true };
        // THE DURABLE HALF. Its failures are COUNTED, never logged: a failed
        // write to the warnings record, reported through the warning path, is
        // the original defect with recursion added.
        let rec = serde_json::json!({
            "at": now(), "level": level, "target": file, "line": line, "message": message,
            "streak": streak,
        });
        let wrote = append_bounded(&path, st.sink_max_bytes, &rec.to_string());
        if wrote.is_err() {
            st.sink_errors += 1;
        }
        true
    }
}

#[cfg(test)]
mod counters_tests {
    //! The dedup is proven on TWO sites interleaved (N >= 2: a streak is
    //! per site, and one site's repeats must not touch another's), and on a
    //! message CHANGE, which is the case a naive "suppress after the first"
    //! would get wrong. Counts are read from the per-site row, not the
    //! process-wide atomics, because other tests in this binary share those.
    use super::counters::{emits, record, STATE};

    fn count_at(file: &'static str, line: u32) -> u64 {
        STATE.lock().unwrap().kinds.get(&(file, line)).map(|r| r.count).unwrap_or(0)
    }

    #[test]
    fn emits_on_powers_of_two_only() {
        let got: Vec<u64> = (1..=20).filter(|&n| emits(n)).collect();
        assert_eq!(got, vec![1, 2, 4, 8, 16]);
    }

    #[test]
    fn a_repeated_message_is_counted_every_time_and_emitted_logarithmically() {
        const F: &str = "counters_tests::site-a";
        let mut emitted = Vec::new();
        for _ in 0..9 {
            emitted.push(record("warn", F, 1, "the same defect, every poll"));
        }
        assert_eq!(
            emitted,
            vec![true, true, false, true, false, false, false, true, false],
            "1st, 2nd, 4th, 8th emitted; the rest suppressed"
        );
        assert_eq!(count_at(F, 1), 9, "every call counted, suppressed or not");
    }

    #[test]
    fn a_changed_message_is_emitted_at_once_and_restarts_the_streak() {
        const F: &str = "counters_tests::site-b";
        for _ in 0..3 {
            record("error", F, 2, "first defect");
        }
        // 3rd was suppressed; a DIFFERENT message at the same site is not.
        assert!(record("error", F, 2, "second defect"), "a change is emitted at once");
        assert!(record("error", F, 2, "second defect"), "streak restarted: 2nd of the new message");
        assert!(!record("error", F, 2, "second defect"), "3rd of the new message suppressed");
        assert_eq!(count_at(F, 2), 6);
        let st = STATE.lock().unwrap();
        let row = st.kinds.get(&(F, 2)).unwrap();
        assert_eq!(row.streak, 3);
        assert_eq!(row.message, "second defect", "the shape sees the CURRENT message");
    }

    /// The sink is process-global; every test that ATTACHES it holds this so
    /// none re-points it under another.
    static SINK_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// **DG-3: the daemon's sink rotates past its bound and a reader still
    /// sees the newest warning** — through `record`, the daemon's own path.
    /// Every event has a distinct message so each one is emitted. OPPOSED by
    /// the under-the-bound arm: the same writes against a large bound leave no
    /// `.1`, so rotate-always fails as surely as rotate-never.
    #[test]
    fn the_sink_rotates_past_its_bound_and_the_newest_warning_is_in_the_live_file() {
        let _g = SINK_GATE.lock().unwrap_or_else(|e| e.into_inner());
        const F: &str = "counters_tests::site-rotate";
        const MAX: u64 = 2000;
        let dir = std::env::temp_dir().join(format!("cauldron-sink-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        super::counters::attach_bounded(&dir, MAX);
        for i in 0..60 {
            assert!(record("warn", F, 1, &format!("distinct defect number {i}, long enough to fill a bound")));
        }
        let live = dir.join("warnings").join("warnings.jsonl");
        let rotated = dir.join("warnings").join("warnings.jsonl.1");
        let newest = std::fs::read_to_string(&live)
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["target"] == F)
            .last()
            .expect("the live file holds this site's lines");
        assert_eq!(
            newest["message"], "distinct defect number 59, long enough to fill a bound",
            "a reader of the live file sees the NEWEST warning"
        );
        let live_len = std::fs::metadata(&live).unwrap().len();
        assert!(live_len <= MAX, "the live file is bounded: {live_len} > {MAX}");
        assert!(rotated.exists(), "the previous generation is kept as `.1`");
        let rot_len = std::fs::metadata(&rotated).unwrap().len();
        assert!(live_len + rot_len <= 2 * MAX, "total bounded: {live_len} + {rot_len}");
        let _ = std::fs::remove_dir_all(&dir);

        // OPPOSING: under the bound, no rotation.
        let dir2 = std::env::temp_dir().join(format!("cauldron-sink-under-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir2);
        super::counters::attach_bounded(&dir2, 1024 * 1024);
        for i in 0..60 {
            record("warn", F, 2, &format!("distinct defect number {i}, long enough to fill a bound"));
        }
        assert!(!dir2.join("warnings").join("warnings.jsonl.1").exists(), "under the bound nothing rotates");
        let kept = std::fs::read_to_string(dir2.join("warnings").join("warnings.jsonl"))
            .unwrap()
            .lines()
            .filter(|l| l.contains("site-rotate"))
            .count();
        assert_eq!(kept, 60, "and every line is still in the live file");
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn the_production_bound_is_threads_4_mib_live_8_mib_total() {
        let _g = SINK_GATE.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(super::counters::SINK_MAX_BYTES, 4 * 1024 * 1024);
        let before = {
            let st = STATE.lock().unwrap();
            (st.sink.clone(), st.sink_max_bytes)
        };
        let dir = std::env::temp_dir().join(format!("cauldron-sink-bound-{}", std::process::id()));
        super::counters::attach(&dir);
        let applied = STATE.lock().unwrap().sink_max_bytes;
        {
            // Put the process-global sink back as it was; nothing is written.
            let mut st = STATE.lock().unwrap();
            st.sink = before.0;
            st.sink_max_bytes = before.1;
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(applied, super::counters::SINK_MAX_BYTES, "attach applies the bound");
    }

    /// THE EMISSION ITSELF, witnessed: the sink file. Return values alone
    /// let an always-emit implementation pass (the inspector forced it).
    /// Lines are filtered by this site's `target`, because the sink is
    /// process-global and sibling tests write to it concurrently.
    #[test]
    fn the_sink_holds_one_line_per_emitted_event_and_none_for_suppressed_ones() {
        let _g = SINK_GATE.lock().unwrap_or_else(|e| e.into_inner());
        const F: &str = "counters_tests::site-sink";
        let dir = std::env::temp_dir().join(format!("cauldron-sink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        super::counters::attach(&dir);
        for _ in 0..9 {
            record("warn", F, 7, "persistent parse error");
        }
        let text = std::fs::read_to_string(dir.join("warnings").join("warnings.jsonl")).unwrap();
        let streaks: Vec<u64> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["target"] == F && v["line"] == 7)
            .map(|v| v["streak"].as_u64().unwrap())
            .collect();
        assert_eq!(streaks, vec![1, 2, 4, 8], "nine events, four sink lines, each stamped with its streak");
        assert_eq!(count_at(F, 7), 9, "and all nine counted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_sites_interleaved_keep_separate_streaks() {
        const F: &str = "counters_tests::site-c";
        // Interleave: site 10 and site 11, same text. Each site's 3rd call is
        // suppressed on ITS OWN count, not on the combined one.
        assert!(record("warn", F, 10, "x"));
        assert!(record("warn", F, 11, "x"));
        assert!(record("warn", F, 10, "x"));
        assert!(record("warn", F, 11, "x"));
        assert!(!record("warn", F, 10, "x"));
        assert!(!record("warn", F, 11, "x"));
        assert!(record("warn", F, 10, "x"), "site 10's 4th is emitted");
        assert_eq!(count_at(F, 10), 4);
        assert_eq!(count_at(F, 11), 3);
    }
}

macro_rules! warn_at {
    ($($arg:tt)*) => { crate::daemon::counters::record("warn", file!(), line!(), &format!($($arg)*)) };
}
macro_rules! error_at {
    ($($arg:tt)*) => { crate::daemon::counters::record("error", file!(), line!(), &format!($($arg)*)) };
}

// ═══════════════════════════════════════════════════════ WHAT IT IS HOSTING ══

/// What the daemon knows about one service it hosts.
#[derive(Debug, Clone)]
struct Child {
    /// `None` until it has come up once in this incarnation.
    pid: Option<u32>,
    /// The version label it is running.
    label: String,
    /// Consecutive failed liveness checks. Reset by any success.
    misses: u32,
    /// Consecutive launches on the current pin that did not PROVE the build:
    /// a start that failed, or (R9.C3, gap C-3) a start that came up and then
    /// died before staying up `stable_after_secs`. This is what
    /// `auto_rollback` counts and what parking counts. It is reset by the
    /// service being seen up for `stable_after_secs` (and by a re-pin) —
    /// never by a bind, since a build that binds and then panics 20 s later
    /// binds every time.
    ///
    /// **PER INCARNATION, deliberately, and not persisted.** A fresh daemon
    /// gives the pinned version a fresh chance. The alternative — writing the
    /// count to disk — would mean a park that survives a reboot: the service
    /// would come back from a power cut already given up on, and clearing it
    /// would need a verb that exists only to edit a counter. The cost is that
    /// `--once` cannot reach a rollback, since each invocation starts at zero,
    /// and `scripts/prove-supervisor` therefore drives a REAL long-lived
    /// daemon for that section rather than a loop of one-shot passes. launchd's
    /// `ThrottleInterval` is what bounds the restart-the-daemon case.
    start_failures: u32,
    /// **PARKED: stop trying, keep saying so.**
    ///
    /// The old supervisor's one unambiguously right decision, kept. A service
    /// whose pinned version will not start and whose rollback target will not
    /// start either is not something to retry forever: a restart loop is a
    /// supervisor converting one failure into a stream of them, and it buries
    /// the original cause under its own noise.
    parked: bool,
    /// **WHICH BUILD IT WAS PARKED ON** (R9.C2, gap C-2), set only when the
    /// park is the supervisor giving up after `max_start_failures` — never by
    /// a requested STOP, which parks too but is the operator's own act and is
    /// not undone by a pin. `Some(None)` is a pin that resolved to no build
    /// at all. A later pass whose pin resolves to a DIFFERENT build clears
    /// the park and launches it: a new build is the operator's fix, and the
    /// park warning tells him to pin one. The same build stays parked.
    parked_on: Option<Option<String>>,
    /// Was this child re-attached from the registry rather than forked by this
    /// incarnation? Carried so the status answer can say so: a re-attached
    /// child's pid is one the daemon read, not one it was handed, and the
    /// difference is exactly the thing this whole crate is careful about.
    reattached: bool,
    /// **CONSECUTIVE LAUNCHES** of this service by this incarnation that did
    /// not stay up for `stable_after_secs`. The backoff exponent. A start that
    /// SUCCEEDED and then died counts here exactly as a failed start does.
    /// Since R9.C3 `start_failures` sees that crash too (through
    /// [`Child::unproven`]); this counter stays the backoff's.
    launches: u32,
    /// **UP, BUT NOT YET PROVEN** (R9.C3, gap C-3). Set when a launch by this
    /// loop comes up; cleared when the service is seen up for
    /// `stable_after_secs`. A service seen DOWN while this is set died before
    /// proving its build, and that death is counted in `start_failures`
    /// exactly as a failed start is — which is what lets a bind-then-panic
    /// crash loop reach auto-rollback or park.
    unproven: bool,
    /// The label this incarnation has recorded GOOD for this service, so a
    /// stable service is written to `env.toml` once, not once per pass.
    good_recorded: Option<String>,
    /// When the last launch was attempted. What `stable_after_secs` is
    /// measured from.
    launched_at: Option<Instant>,
    /// **AN AUTO-ROLLBACK DECIDED BUT NOT WRITTEN** (R9.C7f7, inspect R9.C7f5
    /// F1): `(the exhausted pin, its failure count)`. Set when [`exhausted`]
    /// chose to roll back and the env.toml writers' lock was busy past
    /// `HOLDER_ENV_LOCK_BOUND`. While it is set, the TOP of every pass retries
    /// the rollback and launches nothing — so a busy lock never costs another
    /// launch of a build already known to be bad. Cleared when the rollback
    /// is written, when the service parks instead, or when the pin has moved
    /// off that build (someone else re-pinned: the debt is void).
    rollback_owed: Option<(String, u32)>,
    /// No launch before this instant. Set after every launch attempt from
    /// [`Policy::backoff_secs`].
    not_before: Option<Instant>,
}

impl Child {
    fn new(reattached: bool) -> Self {
        Child {
            pid: None,
            label: String::new(),
            misses: 0,
            start_failures: 0,
            parked: false,
            parked_on: None,
            reattached,
            launches: 0,
            launched_at: None,
            not_before: None,
            unproven: false,
            good_recorded: None,
            rollback_owed: None,
        }
    }

    // The clock is a PARAMETER on all three, never read inside. The daemon
    // passes `Instant::now()`; the tests pass instants of their own, which is
    // how the stable reset — a 60-second property — is proven without
    // waiting 60 seconds (R5.S1b, finding F-1 of the R5.S1 inspection: the
    // reset was unreachable from every test and could be broken silently).

    /// Count one launch attempt at `now` and schedule the earliest next one.
    /// Returns the wait, in seconds, before that next launch.
    fn launched(&mut self, policy: &Policy, now: Instant) -> u64 {
        self.launches = self.launches.saturating_add(1);
        self.launched_at = Some(now);
        let wait = policy.backoff_secs(self.launches);
        self.not_before = Some(now + Duration::from_secs(wait));
        wait
    }

    /// The service was seen LIVE at `now`. If it has been up for
    /// `stable_after_secs` since its last launch, the crash loop — if there was
    /// one — is over, so the consecutive-launch count and the backoff reset.
    /// Returns whether it reset, so the caller can say so.
    fn observed_live(&mut self, policy: &Policy, now: Instant) -> bool {
        let stable = self
            .launched_at
            .map(|t| now.saturating_duration_since(t).as_secs() >= policy.stable_after_secs)
            .unwrap_or(true);
        if self.launches > 0 && stable {
            self.launches = 0;
            self.not_before = None;
            return true;
        }
        false
    }

    /// A launch by this loop CAME UP: bound its address (hosted) or
    /// registered (registering). That is not proof the build works — only
    /// staying up is — so it is marked unproven, and `start_failures` is
    /// deliberately left where it was.
    fn came_up(&mut self) {
        self.unproven = true;
    }

    /// The service was seen LIVE, up `up_secs` by its row's own start time.
    /// Up `stable_after_secs` or longer proves the launch: the unproven mark
    /// and the failure count clear. Returns whether it is stable, which is
    /// when "good" may be recorded. A live service this loop did not launch
    /// (re-attached) carries no unproven mark, so its count clears as it
    /// always did.
    fn proven_by(&mut self, policy: &Policy, up_secs: u64) -> bool {
        let stable = up_secs >= policy.stable_after_secs;
        if stable {
            self.unproven = false;
        }
        if !self.unproven {
            self.start_failures = 0;
        }
        stable
    }

    /// The service was seen DOWN. If the last launch came up and died without
    /// having been seen up for `stable_after_secs`, that launch is counted as
    /// a failed one: `Some(new count)`. Counted once per death.
    fn died_unproven(&mut self) -> Option<u32> {
        if !self.unproven {
            return None;
        }
        self.unproven = false;
        self.start_failures = self.start_failures.saturating_add(1);
        Some(self.start_failures)
    }

    /// How long a DOWN service must still wait at `now` before its next
    /// launch; `None` when it may launch now.
    fn backoff_remaining(&self, now: Instant) -> Option<Duration> {
        self.not_before.filter(|t| now < *t).map(|t| t - now)
    }
}

/// R5.S2 (DEC-34): the MCP tool host's seam words, in their own module so the
/// supervisor loop is untouched by them. Declared after the counting macros so
/// they are in scope inside it.
mod mcp;
// R6.S6b (H179-1): the self-update's view of the launch path.
mod image;

/// The daemon, as a cauldron service.
struct Daemon {
    se: ServiceEnv,
    addr: SocketAddr,
    /// This process's start, carried on every export as `counts_since`, so a
    /// restart reads as a restart and not as a count falling to zero.
    started_at: String,
    /// What it is hosting. Shared with the socket thread, which reads it to
    /// answer `STATUS` and writes a request into `asks` for the loop to act
    /// on.
    children: Arc<Mutex<BTreeMap<String, Child>>>,
    /// **Requests from the socket, for the LOOP to perform.**
    ///
    /// The socket thread never starts, stops or signals anything. It appends a
    /// word here and the supervision loop acts on it, so there is exactly one
    /// thread that forks and signals — which is the same one-owner argument
    /// the whole daemon is, applied inside the process. A connection that
    /// could kill a child directly would be a second launcher reached over
    /// TCP.
    asks: Arc<Mutex<Vec<Ask>>>,
    /// **THE MCP TOOLS THIS SUPERVISOR OWNS** (R5.S2, DEC-34): the declared
    /// tool servers and the ONE awake slot. See [`mcp`]. No outer lock
    /// (R5.S2b): the host locks per server, so one stuck tool cannot stall
    /// another connection's `MCP_TOOLS` or `MCP_CALL`.
    tools: Arc<mcp_host::ToolHost>,
}

/// The operator's asks, as a closed set. `OR-182`'s *"AUI only needs to send a
/// suicide request to the daemon or its supervisor"* lands here as
/// `Ask::Restart`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ask {
    Restart(String),
    Stop(String),
}

impl ServesWarnings for Daemon {
    const SHAPE: &'static str = env::warnings::SHAPE;

    fn warnings(&self, asked: &str) -> Result<Export> {
        if !asked.is_empty() && asked != Self::SHAPE {
            bail!("this service serves the `{}` shape, not `{asked}`", Self::SHAPE);
        }
        let warn = counters::WARN.load(Ordering::Relaxed);
        let error = counters::ERROR.load(Ordering::Relaxed);
        let (mut kinds, sink_errors, attached) = match counters::STATE.lock() {
            Ok(st) => (
                st.kinds
                    .iter()
                    .map(|(&(file, line), r)| Kind {
                        target: file.to_string(),
                        line,
                        level: r.level.to_string(),
                        count: r.count,
                        message: r.message.clone(),
                    })
                    .collect::<Vec<_>>(),
                st.sink_errors,
                st.sink.is_some(),
            ),
            // A poisoned lock means a panic happened while counting. The
            // totals are atomics and still true, so report them and report an
            // EMPTY breakdown rather than inventing one.
            Err(_) => (Vec::new(), 0, false),
        };
        kinds.sort_by(|a, b| {
            b.count.cmp(&a.count).then_with(|| a.target.cmp(&b.target)).then_with(|| a.line.cmp(&b.line))
        });
        kinds.truncate(5);
        Ok(Export {
            shape: Self::SHAPE.to_string(),
            shape_version: env::warnings::SHAPE_VERSION,
            producer: self.se.service.clone(),
            counts_since: self.started_at.clone(),
            warn,
            error,
            sink_errors,
            sink_attached: attached,
            kinds,
        })
    }
}

impl Service for Daemon {
    /// `HELLO` and `WARNINGS` are absent on purpose and cannot be added:
    /// `attest_warnings!` refuses at compile time to let a service shadow one
    /// of the supervisor's own two words.
    const VERBS: &'static [Verb<Daemon>] = &[
        Verb::new("STATUS", Daemon::v_status),
        Verb::new("RESTART", Daemon::v_restart),
        Verb::new("STOP", Daemon::v_stop),
        Verb::new(mcp_host::wire::TOOLS, Daemon::v_mcp_tools),
        Verb::new(mcp_host::wire::CALL, Daemon::v_mcp_call),
        Verb::new(mcp_host::wire::STATUS, Daemon::v_mcp_status),
        Verb::new(mcp_host::wire::SLEEP, Daemon::v_mcp_sleep),
    ];
    fn service_env(&self) -> &ServiceEnv {
        &self.se
    }
    fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Daemon {
    /// Everything the daemon is holding, as one JSON line.
    ///
    /// **This is the answer §5 of the design rests on.** The question "is
    /// anything supervising this environment" is answered by a party that is
    /// ALIVE — a reader connecting to this socket — rather than by a report
    /// from the party that might be gone. A dead supervisor cannot say it is
    /// dead, and every channel in which the absent party is the one that
    /// speaks has that property, including a warnings shape.
    fn v_status(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        let kids = self.children.lock().map_err(|_| anyhow::anyhow!("poisoned"))?;
        let rows: Vec<_> = kids
            .iter()
            .map(|(name, ch)| {
                serde_json::json!({
                    "service": name,
                    "pid": ch.pid,
                    "version": ch.label,
                    "misses": ch.misses,
                    "start_failures": ch.start_failures,
                    "parked": ch.parked,
                    // R9.C7b2: WHAT A PARK IS WAITING ON. `stop` is a
                    // requested stop (only a restart undoes it); `failures`
                    // is the supervisor giving up on `parked_on`, which a pin
                    // to any other build revives.
                    "parked_by": match (ch.parked, &ch.parked_on) {
                        (false, _) => serde_json::Value::Null,
                        (true, None) => "stop".into(),
                        (true, Some(_)) => "failures".into(),
                    },
                    "parked_on": ch.parked_on.clone().flatten(),
                    "deferred": deferral_of(name).map(|d| serde_json::json!({
                        "repin_to": d.repin_to, "running": d.running, "since": d.since,
                        "kind": d.kind, "why": d.why,
                    })),
                    "reattached": ch.reattached,
                })
            })
            .collect();
        c.line(&serde_json::json!({
            "holder": self.se.service,
            "env": self.se.env,
            "pid": std::process::id(),
            "since": self.started_at,
            "hosting": rows,
        })
        .to_string())?;
        Ok(())
    }

    fn v_restart(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.queue(c, Ask::Restart(rest.trim().to_string()), rest.trim())
    }
    fn v_stop(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        self.queue(c, Ask::Stop(rest.trim().to_string()), rest.trim())
    }

    fn queue(&self, c: &mut Conn<'_>, ask: Ask, service: &str) -> Result<()> {
        if service.is_empty() {
            c.line("ERR name a service")?;
            return Ok(());
        }
        let known = self
            .children
            .lock()
            .map(|k| k.contains_key(service))
            .unwrap_or(false);
        if !known {
            // Refused rather than queued. A request for a service this
            // environment does not host is a mistake worth reporting at the
            // moment it is made, not a no-op the loop discovers later with
            // nobody listening.
            c.line(&format!(
                "ERR `{service}` is not hosted in environment `{}`",
                self.se.env
            ))?;
            return Ok(());
        }
        let mut q = self.asks.lock().map_err(|_| anyhow::anyhow!("poisoned"))?;
        if !q.contains(&ask) {
            q.push(ask);
        }
        c.line("OK queued")?;
        Ok(())
    }
}

/// Serve one connection until the peer closes it. Routed through
/// `env::warnings::dispatch`, not a local `match`, so `WARNINGS` is answered by
/// the contract crate's own code — which is what makes the compile-time check
/// about more than a decorative impl.
/// **Record something about THE SUPERVISOR ITSELF**, not about a service.
///
/// `service` on these records is the supervisor's own service name and `boot`
/// is the boot token it minted for itself, so an incarnation is identified the
/// same way a service instance is — by a token, never by a pid and never by a
/// process NAME. The name matters: there are two unrelated binaries called
/// `cauldron` on this machine (this one, and a project registry vendored at
/// `harness/libs/cauldron/src/bin/cauldron.rs`, copied across four harness
/// worktrees), so a record that said "a process named cauldron did this" would
/// be ambiguous on its face. `By::root` is the ownership field and it is the
/// cauldron root this process serves — the same discipline `prove-warnings`'
/// section 6 moved to when it stopped counting `pgrep -x cauldron` and started
/// reading `CAULDRON_HOME` off the process's own environment.
fn note_self(env: &Environment, d: &Daemon, act: &str, why: &str, detail: String) {
    let mut r = supervisions::note(env, crate::SELF_COMMIT, &d.se.service, act, why);
    r.version = d.se.version.clone();
    r.boot = d.se.boot_token.clone();
    r.pid = Some(std::process::id());
    r.detail = detail;
    if let Err(e) = supervisions::append(&env.data_root, &r) {
        // `error_at!`, not a bail: a supervisor that refuses to run because it
        // cannot write its own log has converted a logging fault into an
        // outage. But it is never silent — see `write_failure`.
        error_at!("{}", supervisions::write_failure(&d.se.service, act, &env.data_root, &e));
    }
}

fn serve(d: &Daemon, stream: TcpStream) -> Result<()> {
    let mut w = stream.try_clone()?;
    let mut r = BufReader::new(stream);
    let mut c = Conn { out: &mut w, frames: 0 };
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let mut it = line.splitn(2, ' ');
        let verb = it.next().unwrap_or("").to_string();
        let rest = it.next().unwrap_or("").trim().to_string();
        match env::warnings::dispatch(d, &verb, &rest, &mut c)? {
            Answered::Yes => {}
            Answered::NotMine => {
                warn_at!("unknown request `{verb}` on the supervisor seam");
                c.line(&format!("ERR unknown request `{verb}`"))?;
            }
        }
        c.out.flush()?;
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════ THE VERB ════

pub fn verb_daemon(args: &[String]) -> Result<i32> {
    let once = args.iter().any(|a| a == "--once");
    for a in args {
        if a.starts_with('-') && a != "--once" {
            bail!("unknown option `{a}` for `cauldron daemon` (only `--once`)");
        }
    }
    let env = Environment::resolve()?;
    // ── A NON-PROD HOLDER RUNS FROM ITS OWN FILE (R6.S6) ────────────────
    // Before the hold and before anything is spawned: if this is not the
    // environment's own launch path, this EXECS it and does not return. The
    // self-update check below then watches that file and no other, so a prod
    // deploy cannot stop another environment's holder. No-op in prod.
    crate::launchd::hold_from_own_launch_path(&env)?;
    env.ensure_dirs()?;
    // ── DG-2b: THE LAUNCHD LOG, ROTATED AS LAUNCHD HAS JUST OPENED IT ────
    // Before the hold, still single-threaded: past 64 MiB it is renamed to
    // `.1` and this image re-execs onto a fresh file (and does not return).
    // Every deploy reaches this line; `cauldron install` does not run on one.
    crate::launchd::rotate_launchd_log_at_start(&env)?;
    println!("{}", env.banner());

    // ── THE HOLD, FIRST, BEFORE ANYTHING IS READ OR SPAWNED ──────────────
    let process_name = crate::launchd::process_name(&env.name);
    let commit = env!("CAULDRON_SELF_COMMIT").to_string();
    let note = hold::note_for(&env.name, &process_name, &commit);
    let held = match hold::try_hold(&env.data_root, note)? {
        Attempt::Held(h) => h,
        Attempt::Taken { path, note } => {
            eprintln!("cauldron: {}", hold::refusal(&env.name, &path, note.as_ref()));
            // NON-ZERO. See `EXIT_HELD`.
            return Ok(EXIT_HELD);
        }
    };
    println!(
        "cauldron: holding environment `{}` ({}), process `{}`, pid {}",
        env.name,
        held.path().display(),
        process_name,
        std::process::id()
    );

    run(&env, held, once)
}

fn run(env: &Environment, _held: Hold, once: bool) -> Result<i32> {
    let m = Manifest::load(&env.manifest())?;
    let policy = m.supervisor.clone();
    println!(
        "cauldron: policy auto_restart={} auto_rollback={} poll={}s down_after={} \
         max_start_failures={} backoff={}s..{}s stable_after={}s",
        policy.auto_restart,
        policy.auto_rollback,
        policy.poll_secs,
        policy.down_after,
        policy.max_start_failures,
        policy.backoff_base_secs,
        policy.backoff_max_secs,
        policy.stable_after_secs
    );
    if !policy.auto_restart {
        println!(
            "cauldron: auto_restart is OFF in this environment, so this daemon WATCHES and \
             REPORTS and will not relaunch anything. That is a real mode, not a degraded one: \
             it is what a test environment wants and it is the mode in which a restart loop is \
             diagnosable."
        );
    }

    // ── THE DAEMON'S OWN SEAM ────────────────────────────────────────────
    // `supervisor_data`, not `service_data`: the latter REFUSES the name
    // `cauldron`, and this is the owner the reservation exists for.
    let data_dir = env.supervisor_data()?;
    std::fs::create_dir_all(&data_dir)?;
    counters::attach(&data_dir);
    let listener = TcpListener::bind("127.0.0.1:0").context("binding the supervisor seam")?;
    let addr = listener.local_addr()?;
    let se = ServiceEnv {
        service: SELF_SERVICE.to_string(),
        env: env.name.clone(),
        data_root: guard::normalize(&env.data_root)?,
        data_dir: data_dir.clone(),
        seams: env.seams(),
        // The daemon mints its OWN boot token, exactly as `start` mints one
        // for a service. Its row is therefore probeable to `live` like any
        // other: a face asking "is anything holding prod" gets a handshake,
        // not a file read.
        boot_token: crate::mint_token(),
        version: self_version(),
        commit: env!("CAULDRON_SELF_COMMIT").to_string(),
        // `!= "0"`, not `== "1"`: the row has only a bool, and unknown dirt is
        // NOT clean. The version label above says which of the two it was.
        dirty: env!("CAULDRON_SELF_DIRTY") != "0",
        bind: addr.to_string(),
    };
    let children: Arc<Mutex<BTreeMap<String, Child>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let asks: Arc<Mutex<Vec<Ask>>> = Arc::new(Mutex::new(Vec::new()));
    let tools = Arc::new(mcp::load_host(env));
    let d = Arc::new(Daemon {
        se,
        addr,
        started_at: now().to_string(),
        children: children.clone(),
        asks: asks.clone(),
        tools,
    });
    // THE INCARNATION, STAMPED ON EVERY SUPERVISION THIS PROCESS RECORDS, and
    // set before anything can be recorded. It is what makes the ledger survive
    // this daemon's own restart as something a reader can still take apart: a
    // reused pid looks like two records with one `by.pid` and two `by.since`,
    // which is the one shape that tells them apart. launchd restarts this
    // process freely — on self-update it EXITS on purpose so the new bytes are
    // launched — so "which incarnation" is a question with several answers
    // inside one window.
    supervisions::set_incarnation(&d.started_at);
    // THE INCARNATION IS IN THE RECORD, not merely inferrable from `by.since`
    // changing between two service records. launchd restarts this process
    // freely (`KeepAlive = true`), and a window in which no service act
    // happened would otherwise carry no trace that the supervisor itself had
    // been replaced inside it.
    note_self(
        env,
        &d,
        supervisions::Act::HOLD,
        supervisions::Why::STARTED,
        format!(
            "took the hold on `{}` as pid {}; auto_restart={} auto_rollback={} poll={}s. Why \
             launchd started this process is NOT stated here, because a fresh process cannot \
             know: a preceding `exit`/`self_update` record is what makes it a self-update, and \
             the absence of one is a crash, a login or a first bootstrap.",
            env.name,
            std::process::id(),
            policy.auto_restart,
            policy.auto_rollback,
            policy.poll_secs
        ),
    );

    // THE COMPILE-TIME CHECK, on the supervisor, at the only place it could
    // be: the act of writing a seam. The supervisor is under the obligation it
    // imposes on everybody else.
    let attested = env::warnings::attest_warnings!(Daemon);
    let binary = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    d.se.register(&addr, &binary, "seam", &attested)?;
    println!(
        "cauldron: supervisor seam registered at {addr} (shape `{}`, attested at compile time)",
        attested.shape()
    );

    {
        let d2 = d.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                // ONE THREAD PER CONNECTION (R5.S2). An `MCP_CALL` holds its
                // connection for as long as the tool takes; served inline it
                // would stall every `STATUS` behind it. Nothing here forks a
                // SERVICE — the asks are still queued for the loop — and tool
                // processes are serialized by the host's own per-server locks.
                let d3 = d2.clone();
                std::thread::spawn(move || {
                    if let Err(e) = serve(&d3, stream) {
                        warn_at!("supervisor connection failed: {e:#}");
                    }
                });
            }
        });
    }

    let r = supervise(env, &d, &policy, once);
    // Clear our own row on the way out, so a reader never sees a supervisor
    // seam for a supervisor that chose to stop. A CRASH leaves the row, and
    // that is correct: the row then probes stale, which is the truth.
    let _ = crate::clear(env, SELF_SERVICE);
    r
}

/// The loop.
fn supervise(env: &Environment, d: &Daemon, policy: &Policy, once: bool) -> Result<i32> {
    let my_image = std::env::current_exe().ok();
    // R6.S6b: inode, size, mtime, then content when only ctime moved
    // (`daemon/image.rs`). Was mtime alone, which a `promote` from a source
    // with the same mtime slipped past.
    let mut my_stamp = my_image.as_ref().and_then(|p| image::Watched::at(p));

    loop {
        // Re-read the manifest EVERY pass. The pin is the operator's
        // instruction and `cauldron pin` writes it to a file; a daemon that
        // cached it would need a restart to notice a promotion, which would
        // make the pin a thing you tell the supervisor rather than a thing you
        // write down.
        let m = match Manifest::load(&env.manifest()) {
            Ok(m) => m,
            Err(e) => {
                error_at!("cannot read {}: {e:#}", env.manifest().display());
                sleep(policy.poll_secs);
                continue;
            }
        };

        // PRESENCE IS THE PIN. A service runs here if and only if it is pinned
        // here, per his S-7 ruling, so there is no second list and nothing to
        // keep in step with anything.
        let pinned: Vec<String> = m.services.keys().cloned().collect();

        // NO DECLARATION IS LOADED HERE, deliberately. This loop used to
        // `decl::load` the shipped file per pinned service and hand it to
        // `tend`, which is how a pinned OLD build came to be relaunched under
        // a NEW declaration (R3.S2.0b). The declaration a pinned version runs
        // under is the one in its build directory, and `tend` resolves it at
        // the one moment it needs it — the launch.
        for name in &pinned {
            // R5.S3: a scheduled JOB is a pass, not a process; it is tended by
            // `jobs`, never by the liveness loop.
            let r = if crate::jobs::is_job(env, &m, name) {
                crate::jobs::tend(env, &m, policy, name)
            } else {
                tend(env, d, policy, &m, name)
            };
            if let Err(e) = r {
                error_at!("`{name}`: {e:#}");
            }
        }
        crate::jobs::sweep(env, &pinned);

        // Services in the registry that are NOT pinned any more: the operator
        // unpinned them, which is how a deliberate stop is expressed. A stop is
        // a STATE the supervisor holds, never a reason for the supervisor to
        // exit.
        if policy.auto_restart {
            if let Ok(t) = Table::load(&env.name, &env.data_root, &env.seams()) {
                for (name, seam) in &t.seams {
                    if name == SELF_SERVICE || pinned.contains(name) {
                        continue;
                    }
                    // R9.C7b3 (C-7, C7b2 inspect F1): an unpin ENDS any re-pin
                    // wait, whatever the stop below does. A stop that fails
                    // leaves the process live, and a later re-pin to the same
                    // build would otherwise inherit this wait's old `since`.
                    deferral_clear(name);
                    if seams::probe(seam).state == SeamState::Down {
                        continue;
                    }
                    // The same asymmetry as in `tend`: a row cauldron may not
                    // signal cannot be stopped by cauldron, and RETRYING it
                    // every pass is a loop that cannot succeed. Say it once,
                    // say what clears it, attempt nothing.
                    if let Some(why) = seam.refusal_to_signal() {
                        warn_at!(
                            "`{name}` is running at {} and is NOT pinned in `{}`, so cauldron \
                             would stop it — but its row is one cauldron may not signal: \
                             {why} Nothing is being retried. Stop it by whatever started it, \
                             and remove its row from {} if you want cauldron to forget it.",
                            seam.addr,
                            env.name,
                            env.seams().display()
                        );
                        continue;
                    }
                    warn_at!(
                        "`{name}` is running at {} but is no longer pinned in `{}`; stopping it \
                         (an unpin is how a deliberate stop is expressed)",
                        seam.addr,
                        env.name
                    );
                    // The RUNNING version's stop terms, from its own build's
                    // declaration (`stop_terms`), never the shipped file's.
                    let (sig, esc) = crate::stop_terms(env, seam);
                    if let Err(e) =
                        crate::stop_by_seam(env, seam, &sig, esc, supervisions::Why::UNPINNED)
                    {
                        error_at!("stopping the unpinned `{name}`: {e:#}");
                    }
                    if let Ok(mut k) = d.children.lock() {
                        k.remove(name);
                    }
                }
            }
        }

        // The operator's asks, performed by the LOOP and never by the socket.
        let queued: Vec<Ask> = d.asks.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for ask in queued {
            match ask {
                Ask::Restart(name) => {
                    warn_at!("restart of `{name}` requested on the supervisor seam");
                    // R9.C7d: a LIVE service that can drain is asked to, and
                    // `tend` restarts it once its running turns end. Anything
                    // else — down, not ours to signal, no door — restarts now.
                    //
                    // R9.C7e: and so is an UNHEALTHY one (a 503 still carries
                    // its restart gate, as threads' does on a stale store).
                    // Killing its turns because its health check failed is
                    // what the drain exists to avoid. Read as unhealthy, the
                    // gate relaunches it at the cap whatever `at_cap` says,
                    // exactly as the 503 relaunch does (R9.C7d-1).
                    let live = Table::load(&env.name, &env.data_root, &env.seams())
                        .ok()
                        .and_then(|t| t.seams.get(&name).cloned())
                        .filter(|s| s.signalable())
                        .and_then(|s| match seams::probe(&s).state {
                            SeamState::Live | SeamState::LiveUnverified => Some((s, false)),
                            SeamState::Unhealthy => Some((s, true)),
                            _ => None,
                        });
                    let gate = match &live {
                        Some((s, unhealthy)) => {
                            restart_gate(env, policy, &name, s, supervisions::Why::REQUESTED, *unhealthy)
                        }
                        None => RestartGate::Now,
                    };
                    if gate == RestartGate::Now {
                        if let Err(e) = restart_now(env, d, &m, &name, supervisions::Why::REQUESTED) {
                            error_at!("restarting `{name}`: {e:#}");
                        }
                    }
                }
                Ask::Stop(name) => {
                    warn_at!("stop of `{name}` requested on the supervisor seam");
                    if let Ok(t) = Table::load(&env.name, &env.data_root, &env.seams()) {
                        if let Some(seam) = t.seams.get(&name) {
                            let (sig, esc) = crate::stop_terms(env, seam);
                            if let Err(e) =
                                crate::stop_by_seam(env, seam, &sig, esc, supervisions::Why::REQUESTED)
                            {
                                error_at!("stopping `{name}`: {e:#}");
                            }
                        }
                    }
                    // Parked, so the next pass does not immediately relaunch
                    // what was just deliberately stopped. An explicit stop that
                    // the supervisor undoes four seconds later is not a stop.
                    if let Ok(mut k) = d.children.lock() {
                        if let Some(ch) = k.get_mut(&name) {
                            ch.parked = true;
                            // A STOP-park: not revived by a pin (R9.C2).
                            ch.parked_on = None;
                            ch.pid = None;
                        }
                    }
                }
            }
        }

        // ── SELF-UPDATE, AND WHY IT IS AN EXIT ──────────────────────────
        //
        // When the image at this process's launch path is no longer the one
        // this process is, the daemon EXITS and launchd starts the new bytes.
        // His ruling: drop execv. Once a holder owns launching, self-update is
        // the holder's job, and an execv inside the daemon would be a second
        // mechanism for one fact — with a ~25 ms window in which the port is
        // unheld, which is measured to have produced a second daemon on 12 of
        // the last 13 self-updates of the service that does it.
        //
        // Exiting is safe precisely because of re-attach: the children are in
        // their own process groups, so they are not in launchd's job, and the
        // next incarnation finds them through the registry.
        if let (Some(p), Some(was)) = (&my_image, &mut my_stamp) {
            if let Some(what) = was.changed(p) {
                {
                    // RECORDED BEFORE THE EXIT, by the only party that can
                    // state this reason. The next incarnation writes a
                    // `hold`/`started`; the PAIR is what distinguishes a
                    // self-update from a crash-restart, which are otherwise
                    // the same aftermath.
                    note_self(
                        env,
                        d,
                        supervisions::Act::EXIT,
                        supervisions::Why::SELF_UPDATE,
                        format!(
                            "the image at my launch path {} is no longer the one I am ({what} \
                             changed); exiting so launchd starts the new bytes. Every hosted \
                             service keeps running -- they are in their own process groups, so \
                             no service act accompanies this and none should.",
                            p.display()
                        ),
                    );
                    warn_at!(
                        "the image at my launch path {} has changed; exiting so launchd starts \
                         the new bytes ({what} changed). Every hosted service keeps running — \
                         they are in their own process groups and the next incarnation \
                         re-attaches through the registry.",
                        p.display()
                    );
                    return Ok(0);
                }
            }
        }

        // Reap anything this incarnation forked that has since exited, so a
        // daemon that restarts services for weeks does not accumulate
        // zombies. Observed within minutes of the first cutover.
        reap();

        if once {
            println!("cauldron: --once, so one pass and out");
            return Ok(0);
        }
        sleep(policy.poll_secs);
    }
}

fn sleep(secs: u64) {
    std::thread::sleep(Duration::from_secs(secs.max(1)));
}

/// **ONE SERVICE, ONE PASS.** Re-attach, launch, watch, restart, roll back,
/// park — in that order, because each step's evidence is the previous step's
/// conclusion.
fn tend(
    env: &Environment,
    d: &Daemon,
    policy: &Policy,
    m: &Manifest,
    name: &str,
) -> Result<()> {
    let name = name.to_string();
    // UNREACHABLE BY CONSTRUCTION: `supervise` iterates `m.services.keys()`,
    // so every name that reaches here is pinned. Handled rather than
    // `expect`ed, because a panic would take the supervisor down and the
    // honest response to "this environment does not pin this service" is to
    // not tend it — which is what the unpinned sweep in `supervise` does.
    let Some(pin) = m.pin(&name) else { return Ok(()) };

    let table = Table::load(&env.name, &env.data_root, &env.seams())?;
    let seam = table.seams.get(&name);
    let verdict = seam.map(seams::probe);
    let live = matches!(
        verdict.as_ref().map(|v| &v.state),
        Some(SeamState::Live) | Some(SeamState::LiveUnverified)
    );

    let mut kids = d.children.lock().map_err(|_| anyhow::anyhow!("poisoned child table"))?;
    // `reattached` defaults TRUE for a row that already existed when this
    // incarnation started: if it is live and we did not fork it, we found it.
    let entry = kids.entry(name.clone()).or_insert_with(|| Child::new(live));

    // ── R9.C7f7: AN OWED AUTO-ROLLBACK IS PAID FIRST, BEFORE ANYTHING ELSE.
    // A previous pass decided to roll this service back off `pin` and could
    // not write the re-pin (the env.toml lock was busy past its bound). Without
    // this the service sat down on the bad pin, and the down path below
    // LAUNCHED IT AGAIN to re-reach `exhausted` — one more launch of a build
    // already known to be bad, plus its backoff, per busy-lock miss. Now the
    // rollback is retried here and the pass ends: nothing is launched until it
    // is written (the pass after that launches the rollback target) or the
    // service parks. A pin that has moved off the owed build voids the debt:
    // the operator's own re-pin is the newer decision.
    if let Some((owed, failures)) = entry.rollback_owed.clone() {
        if owed == pin {
            drop(kids);
            error_at!(
                "`{name}` is OWED an auto-rollback off {pin} ({failures} failed or unstable \
                 launches; the last pass could not write the re-pin). Retrying it now; nothing is \
                 launched until it is written"
            );
            return exhausted(env, d, policy, m, &name, &pin, failures);
        }
        println!(
            "cauldron: `{name}` was owed an auto-rollback off {owed}, but `{}` now pins {pin}; \
             that re-pin is newer than the rollback, so the rollback is no longer owed and \
             {pin} gets a fresh count of launches",
            env.name
        );
        // A different build is a fresh chance, as a re-pin out of a park is:
        // the bad pin's failures and backoff are not the new pin's.
        entry.rollback_owed = None;
        entry.start_failures = 0;
        entry.unproven = false;
        entry.launches = 0;
        entry.not_before = None;
    }

    if live {
        // ── A LIVE ROW CAULDRON MAY NOT SIGNAL IS NOT OURS TO MANAGE ──────
        //
        // Re-attach assumes a live row describes a process a PREVIOUS
        // INCARNATION OF THIS DAEMON started — so its pid came from a fork and
        // `signalable()` is true. A row whose pid came from an inference does
        // not describe that, and there is exactly one way to have one: it was
        // written by the deleted `adopt`, before hosting existed. Production's
        // own `seams.json` carried two.
        //
        // What this used to do was try to RESTART onto the pin, which called
        // `stop_by_seam`, which correctly refused — once every `poll_secs`,
        // forever, with a four-line error. Measured on the live machine: the
        // holder logged that refusal about once a second for twenty minutes
        // while tts was pinned and its pre-hosting row was still in the table.
        //
        // The refusal is right and the retry was wrong. A row cauldron cannot
        // signal is a row cauldron cannot replace, so retrying cannot ever
        // succeed — it is the restart-loop failure mode wearing a refusal as a
        // disguise. So: say it ONCE per pass, say exactly what clears it, and
        // attempt nothing.
        let seam = seam.expect("live implies a row");
        if let Some(why) = seam.refusal_to_signal() {
            warn_at!(
                "`{name}` is pinned in `{}` and something IS listening at {}, but its row is \
                 one cauldron may not signal: {why}\n\
                 So cauldron will not take this service over and will not touch the process. \
                 This is a row from before hosting existed — `cauldron adopt` wrote rows about \
                 processes it would never own, and this is one of them. Cauldron cannot replace \
                 a row it cannot signal, so nothing is being retried: a retry here is a restart \
                 loop wearing a refusal as a disguise.\n\
                 To hand it over: stop the process by whatever started it, then remove the \
                 `{name}` row from {} so the next pass launches it from the pin.",
                env.name,
                seam.addr,
                env.seams().display()
            );
            return Ok(());
        }

        // ── RE-ATTACH, which is the whole reason launchd may restart this
        // daemon freely. A live row is a service a previous incarnation of
        // this daemon started; it is ours and it is left exactly as it is.
        if entry.pid.is_none() {
            let s = seam;
            println!(
                "cauldron: re-attached `{name}` at {} (pid {}, version {}) — not restarted, \
                 because it is already the thing the pin asks for",
                s.addr, s.pid, s.version
            );
            entry.pid = Some(s.pid);
            entry.label = s.version.clone();
            entry.reattached = true;
        }
        entry.misses = 0;
        entry.parked = false;
        entry.parked_on = None;
        // ── PROVEN, AND ONLY THEN GOOD (R9.C3, gap C-3). Up is measured from
        // the row's own `started_at`, which every launch path writes — so a
        // re-pin restart and a re-attached child are measured the same way as
        // a launch from this loop. Bound is not proven: a build that binds and
        // panics 20 s later binds on every launch. Until the service has been
        // up `stable_after_secs` its failure count stands and "good" is not
        // written.
        let up_secs = crate::secs_since_epoch().saturating_sub(seam.started_at);
        let stable = entry.proven_by(policy, up_secs);
        // EVERY ROW, hosted AND registering (R9.C4, gap C-4). This is the one
        // place a launch's good record is written under a holder: it is where
        // the stable period can actually be observed. `launch_for_daemon`
        // returns at bind or registration, when nothing has been up long
        // enough to prove anything. Before C-4 this was hosted-only, so a
        // registering service the daemon launched (tts) was never recorded
        // good and its history held only what `start` once wrote at bind.
        if stable
            && entry.good_recorded.as_deref() != Some(seam.version.as_str())
        {
            match crate::record_good_if_stable(env, &name, &seam.version, up_secs, policy.stable_after_secs) {
                Ok(true) => {
                    println!(
                        "cauldron: `{name}` has stayed up {up_secs}s (stable_after = {}s) on {}; \
                         recorded it GOOD in `{}`",
                        policy.stable_after_secs, seam.version, env.name
                    );
                    entry.good_recorded = Some(seam.version.clone());
                }
                Ok(false) => {}
                Err(e) => {
                    error_at!("recording `{name}` {} good after a stable run: {e:#}", seam.version);
                }
            }
        }
        // ── STABLE: the backoff resets. Up for `stable_after_secs` since the
        // last launch means the crash loop, if there was one, is over.
        let launches_before = entry.launches;
        if entry.observed_live(policy, Instant::now()) {
            println!(
                "cauldron: `{name}` has stayed up for {}s after {} consecutive launch(es); its \
                 restart backoff is reset",
                policy.stable_after_secs, launches_before
            );
        }

        // ── R9.C7d: A RESTART WAITING FOR ITS DRAIN. Checked before the
        // re-pin below, whose pin-back branch would otherwise cancel the drain
        // this restart asked for (the pin IS the running build here).
        if let Some(why) = drain_restart_pending(&name) {
            drop(kids);
            deferral_clear(&name);
            if why == supervisions::Why::DOWN {
                // It was draining to be relaunched as unhealthy, and it
                // answers 200 again: the wedge cleared, so nothing restarts it.
                drain_restart_forget(&name);
                quiet_forget(&name);
                println!(
                    "cauldron: `{name}` is healthy again while draining for its unhealthy \
                     relaunch; the relaunch is dropped and the drain cancelled"
                );
                cancel_drain_if_draining(env, &name, seam);
                return Ok(());
            }
            return match restart_gate(env, policy, &name, seam, why, false) {
                RestartGate::Wait => Ok(()),
                RestartGate::Now => restart_now(env, d, m, &name, why),
            };
        }

        // ── PICK UP A NEW PIN. His 2026-09-02 ruling, made literal: "the
        // supervisor's job is to make the thing restart and pick up the new
        // binary."
        let want = resolve_label(env, &name, &pin).ok();
        // R9.C7b2: a wait is recorded only by the two arms below that decline
        // a re-pin with no drain; any pass that does not reach them ends it.
        if !(want.as_ref().is_some_and(|w| *w != entry.label) && !entry.label.is_empty() && policy.auto_restart) {
            deferral_clear(&name);
        }
        if let (Some(want), false) = (want, entry.label.is_empty()) {
            if want != entry.label && policy.auto_restart {
                // ── R9.C7: AT A QUIET MOMENT, or not at all this pass. The
                // child table is released first: the read below is a socket
                // round trip and the supervisor seam must not wait on it.
                let running = entry.label.clone();
                drop(kids);
                let gate = read_gate(env, seam);
                // R9.C7f: this pin's own drain terms, when it carries any.
                let drain = gate
                    .drain
                    .filter(|d| d.served)
                    .map(|d| d.with_override(pin_drain_override_or_warn(m, &name)));
                match gate.quiet {
                    QuietRead::Undeclared => deferral_clear(&name),
                    QuietRead::Quiet(read) => {
                        deferral_clear(&name);
                        quiet_forget(&name);
                        drain_unwatch(&name);
                        match drain.as_ref().and_then(|d| d.since) {
                            Some(since) => println!(
                                "cauldron: `{name}` has DRAINED (draining since {}; {read}): no \
                                 turn is running, so the re-pin restart onto {want} goes ahead and \
                                 the new process starts the prompts queued during the drain",
                                crate::jobs::utc_iso(since)
                            ),
                            None => println!(
                                "cauldron: `{name}` is quiet ({read}), so the re-pin restart goes ahead"
                            ),
                        }
                    }
                    // ── R9.C7c: BUSY, AND IT CAN DRAIN. Ask it to; then wait for
                    // the running turns, or for the cap.
                    QuietRead::Busy(read) if drain.is_some() => {
                        let d = drain.expect("guarded");
                        // A drain is its own record, with its own since.
                        deferral_clear(&name);
                        match drain_step(env, policy, &name, seam, &running, &want, &read, &d) {
                            DrainStep::Wait => return Ok(()),
                            DrainStep::HandOff => {}
                        }
                    }
                    QuietRead::Busy(read) => {
                        deferral_hold(&name, &running, &want, "busy", &read);
                        quiet_say_once(&name, format!("busy {want}"), || {
                            println!(
                                "cauldron: `{name}` is running version {running} and `{}` now pins \
                                 {want}, but it is NOT QUIET ({read}). Not restarting it: the pin \
                                 is recorded, and the first pass that reads it quiet (one every \
                                 {}s) restarts it. Its running build has no drain door, so \
                                 `cauldron restart {name}` would restart it now, and that \
                                 {KILLS_TURNS}.",
                                env.name, policy.poll_secs
                            )
                        });
                        return Ok(());
                    }
                    QuietRead::Unreadable(why) => {
                        deferral_hold(&name, &running, &want, "unreadable", &why);
                        quiet_say_once(&name, format!("unreadable {want}"), || {
                            warn_at!(
                                "`{name}` is running version {running} and `{}` now pins {want}, \
                                 but its quiet signal CANNOT BE READ: {why}. Not restarting it \
                                 (fail safe: an unreadable signal is not a quiet moment). The pin \
                                 is recorded and every pass reads the signal again. `cauldron \
                                 restart {name}` would restart it now, and if turns are running \
                                 that {KILLS_TURNS}.",
                                env.name
                            );
                        });
                        return Ok(());
                    }
                }
                warn_at!(
                    "`{name}` is running version {} and `{}` now pins {}; restarting it onto \
                     the pin",
                    running,
                    env.name,
                    want
                );
                // R9.C7f2: the re-pin restart IS this pin completing, whether
                // it succeeded or failed; its drain override is spent.
                let r = restart_now(env, d, m, &name, supervisions::Why::REPIN);
                pin_completed(env, &name, &pin);
                return r;
            } else if want == entry.label {
                // ── R9.C7f3 (C-7, inspect R9.C7f2 F2): THE PIN IS ALREADY THE
                // RUNNING BUILD, so it has COMPLETED with nothing to drain and
                // any override it carries is spent. Without this, `cauldron
                // pin <svc> latest --at-cap/--drain-cap` while the newest build
                // already runs kept its override, and it applied to the re-pin
                // onto whatever `cauldron build` produced next — a drain the
                // operator never had in view. Checked on the pass's snapshot
                // first, so env.toml is only re-read when there is one.
                let ours = m
                    .services
                    .get(&name)
                    .and_then(|p| p.drain.as_ref())
                    .is_some_and(|d| d.for_version == pin);
                let watch = drain_watch_due(&name);
                if ours || watch {
                    drop(kids);
                }
                if ours {
                    pin_completed(env, &name, &pin);
                }
                if watch {
                    // ── R9.C7c: A DRAIN NOTHING IS GOING TO FINISH. The pin is
                    // the build that is running (a pin back, or a holder that
                    // restarted mid-drain), so nothing will replace this
                    // process: cancel the drain and its queued prompts start.
                    cancel_drain_if_draining(env, &name, seam);
                }
            }
        }
        return Ok(());
    }

    // ── NOT LIVE ────────────────────────────────────────────────────────
    //
    // A STALE ROW IS CLEARED WITHOUT ANY CLAIM OF OWNERSHIP, and that is the
    // asymmetry worth naming: signalling a process needs proof that it is
    // ours, but deleting a row that says a service is reachable where it is
    // not needs none. The row is a lie either way, and leaving a pre-hosting
    // row in place would block this service from ever being hosted — see the
    // refusal above, which is the case this clears the way out of.
    //
    // R9.C7b2: a service that is not live is not waiting on a re-pin; its
    // next launch is from the pin, so any recorded wait is over.
    deferral_clear(&name);
    if let Some(s) = seam {
        if verdict.as_ref().map(|v| v.state.is_stale()).unwrap_or(false) && !s.signalable() {
            warn_at!(
                "clearing a stale pre-hosting row for `{name}`: it claims {} and nothing is \
                 listening there. No signal was sent and none was needed — the row is a \
                 statement about an address, and this one is false. Cauldron can now launch \
                 `{name}` from the pin.",
                s.addr
            );
            let _ = crate::clear(env, &name);
        }
    }
    // ── R9.C7e: A REQUESTED RESTART WAITING FOR ITS DRAIN, ON A SERVICE THAT
    // IS NOT LIVE. `cauldron restart` on an unhealthy (503) service asks it to
    // drain, and every pass after that lands HERE, not in the live branch
    // above — so it is finished here, under the same gate, before anything
    // below (`down_after`, `auto_restart = false`, backoff) can swallow it.
    // A request is the operator's act; the environment's restart policy is
    // about what the supervisor does on its own.
    if drain_restart_pending(&name) == Some(supervisions::Why::REQUESTED) {
        drop(kids);
        let why = supervisions::Why::REQUESTED;
        let draining = match (seam, verdict.as_ref()) {
            (Some(s), Some(v)) if v.owned_and_running(s) && !v.owned_and_exiting(s) => Some(s),
            _ => None,
        };
        let Some(s) = draining else {
            // Mid-exit (`restart_now` waits for it), or gone: nothing is left
            // to drain, and the request still stands.
            drain_restart_forget(&name);
            quiet_forget(&name);
            drain_unwatch(&name);
            return restart_now(env, d, m, &name, why);
        };
        return match restart_gate(env, policy, &name, s, why, true) {
            RestartGate::Wait => Ok(()),
            RestartGate::Now => restart_now(env, d, m, &name, why),
        };
    }
    // ── R9.C2 (gap C-2): A PIN TO A DIFFERENT BUILD REVIVES A PARK. The park
    // warning below tells him to `cauldron pin` a fixed build; before this,
    // doing so changed nothing until the daemon itself restarted, because a
    // parked service never looked at its pin again. So the pin is resolved
    // every pass, and when it names a DIFFERENT build from the one the
    // supervisor gave up on, the park and the failure count clear and the
    // new build is launched on this pass. The SAME build — re-pinned, or
    // reached by another name such as `latest` — stays parked: nothing about
    // it changed. A STOP-park (`parked_on = None`) is never revived by a pin.
    //
    // C7's quiet-moment rule is not consulted, and does not need to be: a
    // parked service is not live (a live pass clears the park above), so
    // there is no running instance mid-work for this launch to interrupt. It
    // is a launch of something down, exactly as the DOWN path below is.
    //
    // Nothing persisted is touched: `env.toml`'s good history and pin, and
    // the ROLLBACK record in `supervisions.jsonl` that names the bad build,
    // stay exactly as they were. Only this incarnation's counters reset —
    // the same per-build reset a re-pin of a live service gets.
    let mut why = supervisions::Why::DOWN;
    if entry.parked && policy.auto_restart {
        if let Some(on) = entry.parked_on.clone() {
            if let Some(now) = resolve_label(env, &name, &pin).ok().filter(|n| on.as_deref() != Some(n.as_str())) {
                warn_at!(
                    "`{name}` was PARKED in `{}` on {} and `{}` now pins {now}, a different build. \
                     A new build is the fix the park asked for, so the park and its {} failed \
                     launch(es) are cleared and {now} is launched now.",
                    env.name,
                    on.as_deref().unwrap_or("a pin that resolved to no build"),
                    env.name,
                    entry.start_failures
                );
                entry.parked = false;
                entry.parked_on = None;
                entry.start_failures = 0;
                entry.unproven = false;
                entry.launches = 0;
                entry.not_before = None;
                // Already known down — it was parked — so the down_after
                // grace is not waited out again.
                entry.misses = policy.down_after.saturating_sub(1);
                why = supervisions::Why::REPIN;
            }
        }
    }
    if entry.parked {
        // Say so every pass. A parked service is the one state where silence
        // would be indistinguishable from health, and it is the state that
        // needs a human.
        //
        // R9.C7b2: and say WHAT IT IS WAITING ON, which depends on the park's
        // kind. One call site still (the `WARNINGS` per-site count is what the
        // R9.C2 tests read), with the kind in its tail. Before, both kinds
        // said a pin to a different build would bring it back, which is false
        // for a STOP-park (R9.C2: a pin never revives one).
        let waiting = match &entry.parked_on {
            None => format!(
                "it was STOPPED by request and is NOT being restarted. It is waiting for \
                 `cauldron restart {name}`, and only that: a pin, to any build, does not revive a \
                 stopped service"
            ),
            Some(on) => format!(
                "after {} failed starts on {} and is NOT being retried. It is waiting for a \
                 `cauldron pin`/`cauldron rollback` to a build OTHER than that one (the next \
                 pass launches it), or `cauldron restart {name}` to try the same build again",
                entry.start_failures,
                on.as_deref().unwrap_or("a pin that resolved to no build")
            ),
        };
        warn_at!("`{name}` is PARKED in `{}`: {waiting}.", env.name);
        return Ok(());
    }

    entry.misses += 1;
    if entry.misses < policy.down_after {
        // NOT down yet, and this is the defence. A stall is not a death, and
        // a supervisor that launches a competitor on the first failed check is
        // how one service becomes two on one data directory.
        warn_at!(
            "`{name}` failed its liveness check ({}/{}) — {}",
            entry.misses,
            policy.down_after,
            verdict.as_ref().map(|v| v.detail.clone()).unwrap_or_else(|| "no row in this environment's table".into())
        );
        return Ok(());
    }

    // ── THE FIRST LAUNCH IS NOT A RESTART (R6.S3). `auto_restart = false` —
    // the default for every new environment — used to mean this daemon never
    // launched anything at all: a pinned service with no row was reported DOWN
    // forever, `start` deferred to the holder, and the holder deferred to the
    // policy. Measured on the R6.S3 proof: `env create`, `pin`, `daemon`, and
    // nothing ran. The module's own contract is "everything pinned with no
    // live row is launched"; what the policy withholds is RE-launching. So
    // each pinned service is launched ONCE per incarnation, when this
    // incarnation has neither launched it nor found it running; after that,
    // with auto_restart off, a death is reported and nothing more.
    let first_launch = entry.launched_at.is_none() && !entry.reattached;
    if !policy.auto_restart && !first_launch {
        warn_at!(
            "`{name}` is DOWN in `{}` after {} failed checks, and `auto_restart = false` here, \
             so nothing is being launched. This is the environment's policy and not a failure \
             to act.",
            env.name,
            entry.misses
        );
        entry.misses = 0;
        return Ok(());
    }

    // ── R5.S3: THE DECLARATION'S OWN RESTART POLICY, the inner switch. A
    // service this incarnation launched or re-attached (so it has a pid
    // here) and then saw die is NOT relaunched under `restart = "never"`.
    if entry.pid.is_some() && !crate::jobs::restart_allowed(env, m, &name) {
        warn_at!(
            "`{name}` is DOWN in `{}` after {} failed checks, and its declaration says \
             `restart = \"never\"`, so nothing is being launched. That is the service's policy \
             and not a failure to act.",
            env.name,
            entry.misses
        );
        entry.misses = 0;
        return Ok(());
    }

    // ── BACKOFF. Down, and allowed to restart — but not yet. Said every pass,
    // because a service waiting out its backoff is down and silence would be
    // indistinguishable from the supervisor having forgotten it.
    // ── A LAUNCH THAT CAME UP AND DIED BEFORE IT WAS STABLE IS A FAILED
    // LAUNCH (R9.C3, gap C-3). Counted toward rollback and park exactly as a
    // failed start is, once per death. Without this a build that binds and
    // then panics cleared its own count on every launch and looped forever.
    if let Some(failures) = entry.died_unproven() {
        error_at!(
            "`{name}` came up on {} and died before staying up {}s, so that launch did not \
             prove the build ({failures}/{} failed or unstable launches)",
            entry.label,
            policy.stable_after_secs,
            policy.max_start_failures
        );
        if failures >= policy.max_start_failures {
            drop(kids);
            return exhausted(env, d, policy, m, &name, &pin, failures);
        }
    }
    if let Some(left) = entry.backoff_remaining(Instant::now()) {
        warn_at!(
            "`{name}` is DOWN in `{}` and backing off: {} consecutive launch(es) without \
             staying up {}s, next attempt in {}s",
            env.name,
            entry.launches,
            policy.stable_after_secs,
            left.as_secs() + 1
        );
        return Ok(());
    }
    // ── R9.C7d: AN UNHEALTHY SERVICE THAT CAN DRAIN IS ASKED TO FIRST. Its
    // 503 answer still says how many turns are running; while any are, the
    // relaunch waits for them (bounded by the drain cap, see `restart_gate`).
    // Checked before the launch is counted, so a wait costs no backoff. A
    // service that is mid-exit, or not ours to signal, or has no door, is
    // relaunched exactly as before.
    //
    // R9.C1c: and so is any OWNED row whose process is still running, whatever
    // the probe made of its port (`unknown:timeout`, `stale:wrong-*`): it is
    // stopped below before the launch, so it gets the same chance to drain.
    let exiting = matches!((verdict.as_ref(), seam), (Some(v), Some(s)) if v.owned_and_exiting(s));
    let owned_running = matches!((verdict.as_ref(), seam), (Some(v), Some(s)) if v.owned_and_running(s));
    let unhealthy = matches!(verdict.as_ref().map(|v| &v.state), Some(SeamState::Unhealthy));
    if (unhealthy || owned_running) && !exiting {
        if let Some(s) = seam.filter(|s| s.signalable()) {
            drop(kids);
            if restart_gate(env, policy, &name, s, supervisions::Why::DOWN, true) == RestartGate::Wait {
                return Ok(());
            }
            kids = d.children.lock().map_err(|_| anyhow::anyhow!("poisoned child table"))?;
        }
    }
    // Launching now: no restart is waiting for a drain any more (a crash
    // mid-drain must not leave one to restart the fresh process again).
    drain_restart_forget(&name);
    let entry = kids.get_mut(&name).expect("inserted above");
    let wait = entry.launched(policy, Instant::now());
    let launch_no = entry.launches;

    let failures = entry.start_failures;
    drop(kids);

    // ── R5.S3: LISTENING BUT UNHEALTHY. The launch below refuses to fork
    // onto an address something holds, so an unhealthy child that cauldron
    // forked (its pid is proof) is stopped first; then it is launched like
    // any down service, and a failure counts toward rollback as usual.
    //
    // C-1: and a row whose listener is gone but whose OWNED pid is not — a
    // service mid-drain, its port closed first — goes through the same stop,
    // which WAITS for the pid (no second signal) under the declared grace.
    // Launching here instead put a second instance on the same data
    // directory as the one still draining.
    //
    // R9.C1c (ledger R9.C1b-1): and ANY owned row whose pid is still running
    // — `unknown:timeout` and `stale:wrong-*` too ([`Verdict::owned_and_running`],
    // which includes `exiting`). Since C-1b the launch below REFUSES over such
    // a pid, and that refusal counted as a failed start: a service whose port
    // stopped answering in time was parked, never relaunched, with its old
    // process still running.
    if exiting || unhealthy || owned_running {
        if let Some(s) = seam.filter(|s| s.signalable()) {
            let (sig, esc) = crate::stop_terms(env, s);
            if let Err(e) = crate::stop_by_seam(env, s, &sig, esc, supervisions::Why::DOWN) {
                error_at!("stopping the unhealthy `{name}`: {e:#}");
            }
        }
    }

    error_at!(
        "`{name}` is DOWN in `{}`; launching it (consecutive launch {launch_no}; the next one, if \
         this does not stay up, waits {wait}s)",
        env.name
    );
    // ── WHICH BUILD, THEN WHICH DECLARATION — two questions, two answers ──
    //
    // The pin resolves to a build, and a pin that does not resolve (the label
    // is not built here) is a FAILED START, counted toward rollback exactly as
    // it always was. The build's declaration is then read from ITS directory
    // — the snapshot, or the shipped file for a build that predates snapshots
    // — and a build whose declaration cannot be resolved is NOT counted: it
    // is reported, every pass, as it was when this loop loaded the shipped
    // file up front. Rolling back on a declaration fault would re-pin the
    // operator's file over a problem that is not the build's, and the
    // rollback target would most likely hit the same fault.
    let launchable = match decl::resolve_version(&env.builds_for(&name), &pin, &env.name) {
        Ok((label, rec)) => match crate::declaration_for_build(env, &name, &label, &rec) {
            Ok((dcl, source)) => Ok(crate::Launchable { label, rec, decl: dcl, source }),
            Err(e) => {
                error_at!(
                    "`{name}` is pinned to {label} in `{}` but has no usable declaration for \
                     that build: {e:#}",
                    env.name
                );
                return Ok(());
            }
        },
        Err(e) => Err(e.context(format!("resolving version `{pin}` for service `{name}`"))),
    };
    let launched = launchable.and_then(|l| {
        crate::launch_for_daemon(env, &l, why).map(|r| (l.source, r))
    });
    // R9.C7f2: a launch of the pin, up or failed, completes it — nothing is
    // running to drain, so an override still in env.toml is spent.
    pin_completed(env, &name, &pin);
    match launched {
        Ok((source, (pid, label, addr))) => {
            println!(
                "cauldron: `{name}` came up at {addr} (pid {pid}, version {label}; declaration: {})",
                source.describe()
            );
            let mut kids = d.children.lock().map_err(|_| anyhow::anyhow!("poisoned"))?;
            let e = kids.get_mut(&name).expect("inserted above");
            e.pid = Some(pid);
            e.label = label;
            e.misses = 0;
            // NOT `start_failures = 0` (R9.C3): coming up is not proof. The
            // count clears when the service is seen up `stable_after_secs`.
            e.came_up();
            e.reattached = false;
            Ok(())
        }
        Err(e) => {
            let failures = failures + 1;
            error_at!("`{name}` failed to start ({failures}/{}): {e:#}", policy.max_start_failures);
            {
                let mut kids = d.children.lock().map_err(|_| anyhow::anyhow!("poisoned"))?;
                let ent = kids.get_mut(&name).expect("inserted above");
                ent.start_failures = failures;
                ent.misses = 0;
            }
            if failures < policy.max_start_failures {
                return Ok(());
            }
            exhausted(env, d, policy, m, &name, &pin, failures)
        }
    }
}

/// **OUT OF LAUNCHES ON THIS PIN: roll back, or park.** `failures` launches in
/// a row did not prove the pinned build — they failed to start or (R9.C3) came
/// up and died before staying up `stable_after_secs`. One body for both, so a
/// crash loop ends the way a start loop always has.
fn exhausted(
    env: &Environment,
    d: &Daemon,
    policy: &Policy,
    m: &Manifest,
    name: &str,
    pin: &str,
    failures: u32,
) -> Result<()> {
    let name = name.to_string();
    let pin = pin.to_string();
    // ── AUTO-ROLLBACK ──────────────────────────────────────────
    if policy.auto_rollback {
        if let Some(prev) = m.rollback_target(&name, &pin) {
            // THE RE-PIN IS WRITTEN TO env.toml, so the rollback
            // survives this daemon and the operator reads what
            // happened in the file he already reads — rather than
            // being told by a process that may not be there when
            // he looks.
            // R9.C7f4: under the env.toml writers' lock (ledger R9.C7f3-2).
            // R9.C7f5: bounded — a busy lock fails this rollback after
            // HOLDER_ENV_LOCK_BOUND instead of stalling the supervisor.
            // R9.C7f7 (inspect R9.C7f5 F1): and the miss is REMEMBERED on the
            // service's entry as `rollback_owed`. The top of the next pass
            // (`tend`) retries this rollback before it launches anything, and
            // keeps retrying, one attempt per pass, until the re-pin is written
            // — the bad pin is never launched again to re-reach this point.
            //
            // R9.C7f7 (inspect F1): COMPARE-AND-SET. The pin in `m` was read
            // at pass start, and this write may have waited HOLDER_ENV_LOCK_BOUND
            // for the very writer that holds the lock — the operator's own
            // `cauldron pin`. So the rollback is written only if env.toml, read
            // UNDER the lock, still pins the build that exhausted. If it pins
            // anything else, his pin is newer than this decision: nothing is
            // written, no rollback is recorded, and the debt is dropped.
            let wrote = Manifest::update_within(&env.manifest(), HOLDER_ENV_LOCK_BOUND, |m2| {
                let now = m2.pin(&name);
                if now.as_deref() != Some(pin.as_str()) {
                    return Ok(Some(now));
                }
                m2.set_version(&name, &prev);
                Ok(None)
            });
            if let Ok(Some(now)) = &wrote {
                println!(
                    "cauldron: NOT rolling `{name}` back off {pin}: `{}` now pins {}, written since \
                     the rollback was decided. That pin is newer than the rollback, so it stands; \
                     the rollback is dropped and the next pass tends the new pin",
                    env.name,
                    now.as_deref().unwrap_or("nothing")
                );
                let mut kids = d.children.lock().map_err(|_| anyhow::anyhow!("poisoned"))?;
                if let Some(ent) = kids.get_mut(&name) {
                    ent.rollback_owed = None;
                    ent.start_failures = 0;
                    ent.unproven = false;
                    ent.launches = 0;
                    ent.not_before = None;
                }
                return Ok(());
            }
            if let Err(e) = wrote {
                let mut kids = d.children.lock().map_err(|_| anyhow::anyhow!("poisoned"))?;
                if let Some(ent) = kids.get_mut(&name) {
                    ent.rollback_owed = Some((pin.clone(), failures));
                }
                return Err(e.context(format!(
                    "auto-rollback of `{name}` off {pin} to {prev} was NOT written; it is owed, and \
                     the next pass retries it before launching anything"
                )));
            }
            // RECORDED, although no process started or stopped here.
            // Without this line the launch on the next pass reads as
            // `why = down` onto a version that changed for no stated
            // reason, and the rollback — the supervisor's largest
            // unilateral act — would be the one thing it does that it
            // does not say it did.
            let mut r = supervisions::note(
                env,
                crate::SELF_COMMIT,
                &name,
                supervisions::Act::REPIN,
                supervisions::Why::ROLLBACK,
            );
            r.was_version = pin.clone();
            r.version = prev.clone();
            r.detail = format!(
                "{failures} failed or unstable launches on {pin}; re-pinned to the newest \
                 version proven good HERE. Nothing was signalled — the next pass launches it."
            );
            if let Err(e) = supervisions::append(&env.data_root, &r) {
                error_at!(
                    "{}",
                    supervisions::write_failure(
                        &name,
                        supervisions::Act::REPIN,
                        &env.data_root,
                        &e
                    )
                );
            }
            error_at!(
                "AUTO-ROLLBACK: `{name}` would not start, or would not stay up {}s, on its \
                 pinned version {pin} after {failures} attempts, so `{}` is re-pinned to \
                 {prev} — the newest version PROVEN GOOD HERE by having stayed up in this \
                 environment, and not the one that just failed. The re-pin is written \
                 to {}, so it is a fact you can read rather than a message you had to \
                 be present for. The next pass launches it.",
                policy.stable_after_secs,
                env.name,
                env.manifest().display()
            );
            let mut kids = d.children.lock().map_err(|_| anyhow::anyhow!("poisoned"))?;
            let ent = kids.get_mut(&name).expect("inserted above");
            ent.start_failures = 0;
            ent.rollback_owed = None;
            return Ok(());
        } else {
            error_at!(
                "`{name}` has no good version in `{}` to roll back to that is not the \
                 one already pinned. A version becomes good by staying up \
                 `stable_after_secs` HERE, so an environment that has never had one start has nothing \
                 behind it.",
                env.name
            );
        }
    }
    // ── PARK ───────────────────────────────────────────────────
    let mut kids = d.children.lock().map_err(|_| anyhow::anyhow!("poisoned"))?;
    let ent = kids.get_mut(&name).expect("inserted above");
    ent.parked = true;
    ent.rollback_owed = None;
    // R9.C2: the build given up on, so a pin to a different one revives it.
    ent.parked_on = Some(resolve_label(env, &name, &pin).ok());
    error_at!(
        "PARKED `{name}` in `{}` after {failures} failed or unstable launches{}. Retrying forever \
         would convert one failure into a stream of them and bury the cause under the \
         supervisor's own noise.",
        env.name,
        if policy.auto_rollback { " and an exhausted rollback" } else { " (auto_rollback is off here)" }
    );
    Ok(())
}

/// Stop it if it is up, then launch it on whatever the pin now says.
/// Stop it if it is up, then launch it on whatever the pin now says.
///
/// `why` is carried through to BOTH halves, so the stop record and the launch
/// record of one restart agree about the reason. A reader that found them
/// disagreeing would be reading two acts, not one.
fn restart_now(
    env: &Environment,
    d: &Daemon,
    m: &Manifest,
    name: &str,
    why: &str,
) -> Result<()> {
    // R9.C7b2: whatever this restart is for, a re-pin it was waiting on is over.
    deferral_clear(name);
    // THE STOP IS UNDER THE RUNNING VERSION'S DECLARATION AND THE LAUNCH IS
    // UNDER THE PINNED VERSION'S. They can differ — that is what a re-pin
    // across a declaration change IS — and each half reads its own build's
    // snapshot rather than the shipped file.
    //
    // C-1: a row whose listener is already gone but whose pid is not (an
    // owned service mid-drain) is STOPPED too — that is, waited for, without
    // a second signal — before the launch. Skipping it is how a restart
    // started a second `threads` on top of one still checkpointing.
    if let Ok(t) = Table::load(&env.name, &env.data_root, &env.seams()) {
        if let Some(seam) = t.seams.get(name) {
            let v = seams::probe(seam);
            // R9.C1c (ledger R9.C1b-1): `owned_and_running`, not only
            // `owned_and_exiting` — an owned row in `unknown:timeout` or
            // `stale:wrong-*` is still a running process, the launch below
            // refuses over it (C-1b), and the restart failed instead of
            // happening.
            if matches!(v.state, SeamState::Live | SeamState::LiveUnverified | SeamState::Unhealthy)
                || v.owned_and_running(seam)
            {
                let (sig, esc) = crate::stop_terms(env, seam);
                crate::stop_by_seam(env, seam, &sig, esc, why)?;
            }
        }
    }
    let l = crate::resolve_launch(env, m, name)?;
    let (pid, label, addr) = crate::launch_for_daemon(env, &l, why)?;
    println!(
        "cauldron: restarted `{name}` at {addr} (pid {pid}, version {label}; declaration: {})",
        l.source.describe()
    );
    if let Ok(mut k) = d.children.lock() {
        let e = k.entry(name.to_string()).or_insert_with(|| Child::new(false));
        e.pid = Some(pid);
        e.label = label;
        e.misses = 0;
        // A restart onto a pin is the FIRST launch of that pin: its count
        // starts at zero — and the launch is UNPROVEN (R9.C4, R9.C3-2). Without
        // `came_up` here the first death after a re-pin restart was not
        // counted, so a bad pin took `max_start_failures + 1` launches to roll
        // back (4, against a declared 3).
        e.start_failures = 0;
        e.came_up();
        e.parked = false;
        e.parked_on = None;
        e.reattached = false;
    }
    Ok(())
}

fn resolve_label(env: &Environment, service: &str, pin: &str) -> Result<String> {
    let (label, _) = decl::resolve_version(&env.builds_for(service), pin, &env.name)?;
    Ok(label)
}

/// How long the daemon waits for a hosted child to bind before calling the
/// start a failure. Longer than a registering service's `start_timeout_ms`,
/// because a hosted child has to be POLLED (it cannot announce itself) and
/// because the two real ones are slow: `threads` opens a large sqlite store
/// with a WAL to replay.
pub const HOSTED_BIND_WAIT: Duration = Duration::from_secs(45);

/// **POLL UNTIL OUR CHILD IS LISTENING ON `addr` — not until SOMETHING is.**
///
/// # The bug this function had, measured on the live machine
///
/// It used to return as soon as a `connect` to `addr` succeeded. That proves a
/// listener exists; it does not prove the listener is the process we just
/// forked. During the `tts` cutover a second launcher — AUI's `ui-server`,
/// which spawns `tts` on demand and which cauldron may not change — took 3652
/// four seconds before cauldron's child got there. Cauldron's child then
/// failed with `Address already in use`, exited, and `wait_for_listener`
/// returned OK anyway because AUI's daemon was answering. `record_hosted`
/// wrote a row saying pid 1745 was serving 127.0.0.1:3652 while 1745 was a
/// zombie and 1736 held the port.
///
/// That row is the one thing the seam table exists not to contain: a
/// confident lie. And it is worse than a stale row, because `stop` WOULD
/// signal it — `pid_source` is `child`, which is proof, and the pid was
/// proof when it was recorded. A later `cauldron stop tts` would have
/// signalled a dead pid, or a recycled one.
///
/// # What it checks instead
///
/// Three things, in this order, and the order matters:
///
/// 1. **Is the child still alive?** A child that exited cannot be the
///    listener, whatever is answering. This is checked FIRST because it is the
///    cheap one and because it is the case that actually happened.
/// 2. **Is anything listening?**
/// 3. **Is the process holding the listening socket OUR CHILD?** Asked of the
///    kernel, by pid, and compared against the pid the fork returned.
///
/// Step 3 uses `lsof` to read the socket's owner, which is the mechanism
/// `PidSource::SOCKET_OWNER` was deleted for — and the difference is the whole
/// point. As a *source* of a pid it was an inference dressed as a fact: it
/// told you who happened to hold a port and you wrote that down as the pid you
/// may signal. As a *check* it is the opposite: the pid is already known from
/// the fork, and this asks the kernel to confirm it. An inference used to
/// CORROBORATE a fact is not the same act as an inference used to MANUFACTURE
/// one.
pub fn wait_for_listener(addr: &str, child: &mut std::process::Child, budget: Duration) -> Result<()> {
    let sa: SocketAddr = addr
        .parse()
        .with_context(|| format!("`{addr}` is not a socket address"))?;
    let pid = child.id();
    let deadline = Instant::now() + budget;
    let mut tries = 0u32;
    loop {
        // (1) THE CHILD, FIRST.
        if let Ok(Some(status)) = child.try_wait() {
            bail!(
                "`{addr}` was being waited on, but the child cauldron forked (pid {pid}) has \
                 already EXITED with {status}. Whatever may or may not be listening on that \
                 address, it is not this process, and a row naming it would be a confident lie \
                 — which is the one thing the seam table exists not to contain. Its log will \
                 say why it exited; the common cause is that something else took the port \
                 first, because a hosted service's port is declared rather than \
                 kernel-assigned."
            );
        }
        if TcpStream::connect_timeout(&sa, Duration::from_millis(500)).is_ok() {
            // (3) AND IT HAS TO BE OURS.
            match listening_pid(&sa) {
                Some(owner) if owner == pid => return Ok(()),
                Some(owner) => {
                    bail!(
                        "something IS listening on {addr}, and it is pid {owner}, NOT the child \
                         cauldron forked (pid {pid}). Cauldron will not write a row naming its \
                         own child against somebody else's listener. A hosted service's port is \
                         declared rather than kernel-assigned, so another launcher can win it: \
                         find out what pid {owner} is (`ps -p {owner} -o ppid=,command=`) before \
                         anything else."
                    );
                }
                // The kernel would not say. NOT read as ours: a check that
                // cannot answer is not a check that answered yes.
                None => {
                    if Instant::now() > deadline {
                        bail!(
                            "something is listening on {addr} but the kernel would not say which \
                             process holds the socket, so cauldron cannot confirm it is the child \
                             it forked (pid {pid}). That is not read as a yes."
                        );
                    }
                }
            }
        }
        tries += 1;
        if Instant::now() > deadline {
            bail!(
                "the child cauldron forked (pid {pid}) is alive but nothing of its came up on \
                 {addr} within {}s ({tries} attempts). A hosted binary cannot announce itself, \
                 so this is the only evidence available that it started.",
                budget.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Which process holds the LISTENING socket on `sa`, per the kernel.
///
/// `None` when it cannot be established — including when two processes somehow
/// answer, which is never read as "probably ours".
fn listening_pid(sa: &SocketAddr) -> Option<u32> {
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-t", &format!("-iTCP:{}", sa.port()), "-sTCP:LISTEN"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let pids: Vec<u32> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .collect();
    match pids.as_slice() {
        [one] => Some(*one),
        _ => None,
    }
}

/// **CHILDREN THIS INCARNATION FORKED, HELD SO THEY CAN BE REAPED.**
///
/// A `Child` that is dropped is never waited on, so an exited child becomes a
/// ZOMBIE until its parent dies. For a one-shot CLI that is free — the process
/// exits in a moment and init reaps. For a daemon that restarts services for
/// weeks it is a leak, and it was visible within minutes of the first cutover:
/// `ps` showed `1745 1639 <defunct>`, a defunct tts under the holder.
///
/// Re-attached children are not in here and cannot be: this incarnation did
/// not fork them, so it holds no handle and `wait` on another process's child
/// is not a thing. They are covered by the handshake, which is the evidence
/// that does not depend on parentage — the same reason re-attach works at all.
pub static FORKED: Mutex<Vec<std::process::Child>> = Mutex::new(Vec::new());

/// Reap what has exited. Called once per supervision pass.
pub fn reap() {
    let Ok(mut kids) = FORKED.lock() else { return };
    kids.retain_mut(|c| !matches!(c.try_wait(), Ok(Some(_))));
}

/// How long [`reap_stopped`] waits for a child that has just been stopped to
/// finish exiting. The stop already waited for its ADDRESS to go quiet; the
/// process exit follows within milliseconds, so this is a ceiling, not a cost.
pub const REAP_STOPPED_WAIT: Duration = Duration::from_secs(2);

/// **REAP THE CHILD A STOP JUST TOOK DOWN, AT THE STOP (R5.S8, D-4).**
///
/// Called by `stop_by_seam` — the one stop path — after the address went
/// quiet. If `pid` is a child this incarnation forked (it is in [`FORKED`]),
/// wait up to [`REAP_STOPPED_WAIT`] for it to exit and reap it, so a
/// requested stop or restart leaves no `<defunct>` entry behind even for the
/// rest of the pass. A pid not in [`FORKED`] (a re-attached service, or a
/// one-shot CLI that forked nothing) returns at once: there is no handle, and
/// there is nothing of ours to reap.
///
/// D-4's zombie was not a slow reap but a MISSING handle — `start_one`
/// dropped the registering child's `Child`, so neither this nor [`reap`] could
/// ever see it. `start_one` now hands its child to [`FORKED`] as
/// `launch_hosted` always did; this closes the remaining window between a stop
/// and the end-of-pass [`reap`].
pub fn reap_stopped(pid: u32) {
    let deadline = Instant::now() + REAP_STOPPED_WAIT;
    loop {
        {
            let Ok(mut kids) = FORKED.lock() else { return };
            let Some(i) = kids.iter().position(|c| c.id() == pid) else { return };
            if matches!(kids[i].try_wait(), Ok(Some(_)) | Err(_)) {
                kids.swap_remove(i);
                return;
            }
        }
        if Instant::now() > deadline {
            // Still running past the stop: left for the per-pass `reap`.
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ═══════════════════════════════════════ R9.C7: THE QUIET MOMENT (gap C-7) ═══
//
// A re-pin used to restart a running service on the very next pass — within
// `poll_secs` (4 s by default) — whatever it was doing, while `cauldron pin`
// said "at the next quiet moment" and "nothing was started or stopped". For
// `threads` that meant a SIGTERM over executing turns, which then land wrong
// after the restart (gap L-2: a duplicate paid turn, or a lost message).
//
// Now a service may DECLARE a quiet signal, and a re-pin restart waits for it:
//
//     [quiet]
//     path = "/health"                 # an HTTP GET on the row's own address
//     pointer = "/restart_gate/idle"   # a JSON pointer to a boolean
//
// * `true`  → quiet: the restart goes ahead on this pass.
// * `false` → busy: NO restart. The pin is already recorded in `env.toml`;
//   every pass (`poll_secs`) reads the signal again, and the first pass that
//   reads `true` restarts it. That pass IS the bounded re-check — nothing
//   spins between passes.
// * anything else (no answer, not 200, not JSON, no such field, not a
//   boolean) → UNREADABLE: NO restart, FAIL SAFE, and the daemon says so. A
//   signal that cannot be read is not a quiet moment. `cauldron restart`
//   does not wait for quiet; since R9.C7d it does wait for a DRAIN when the
//   running build has the door (`restart_gate`).
// * no `[quiet]` → today's behaviour, unchanged: the restart goes ahead.
//
// THE DECLARATION READ IS THE RUNNING VERSION'S (its build snapshot), as
// `stop_terms` reads the stop signal: whether a binary serves a quiet signal
// is a property of the binary that is up, not of the file that will build the
// next one. A running build from before its declaration had `[quiet]` is
// restarted exactly as before — which is why the FIRST deploy of a `[quiet]`
// declaration still restarts unconditionally.
//
// Parsed out of `Declaration::text` rather than added to `Declaration`:
// `libs/env/src/decl.rs` was outside this step's bounds, and serde already
// ignores the unknown key there.

/// What a service's declared quiet signal said, read once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QuietRead {
    /// The running version declares no `[quiet]`: nothing to wait for.
    Undeclared,
    /// It read quiet. Carries what was read, for the log.
    Quiet(String),
    /// It read busy. Carries what was read.
    Busy(String),
    /// It could not be read. Carries why. **Treated as busy** — fail safe.
    Unreadable(String),
}

/// How long one read of a quiet signal may take, end to end per socket op.
pub(crate) const QUIET_READ_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(serde::Deserialize)]
struct QuietFile {
    quiet: Option<QuietStanza>,
}

#[derive(serde::Deserialize)]
struct QuietStanza {
    path: String,
    pointer: String,
    /// R9.C7c: how to ask this service to DRAIN, and how long to wait for it.
    #[serde(default)]
    drain: Option<DrainStanza>,
}

/// `[quiet]` out of one declaration's text. `Err` is a stanza that is
/// present and malformed — which is itself UNREADABLE, never "undeclared".
fn quiet_stanza(text: &str) -> std::result::Result<Option<QuietStanza>, String> {
    match toml::from_str::<QuietFile>(text) {
        Ok(f) => Ok(f.quiet),
        Err(e) => {
            // Only a malformed `[quiet]` is ours to report; the declaration
            // as a whole already parsed or we would not be here.
            if text.lines().any(|l| l.trim() == "[quiet]") {
                Err(format!("its `[quiet]` section does not parse: {e}"))
            } else {
                Ok(None)
            }
        }
    }
}

/// **READ THE RESTART GATE OF THE INSTANCE `seam` DESCRIBES**: its quiet
/// signal, and (R9.C7c) the drain the same answer carries. One GET.
pub(crate) fn read_gate(env: &Environment, seam: &seams::Seam) -> GateRead {
    read_gate_answering(env, seam, &[200])
}

/// [`read_gate`] of a service whose health check is answering 503 (R9.C7d).
///
/// Threads' liveness layer answers `/health` 503 when its store heartbeat is
/// stale and carries `restart_gate` in that body too, so the supervisor can
/// tell whether turns are still running before it relaunches a wedged
/// service. Only the 503 relaunch reads this way; a quiet read for a re-pin
/// still needs a 200.
pub(crate) fn read_gate_unhealthy(env: &Environment, seam: &seams::Seam) -> GateRead {
    read_gate_answering(env, seam, &[200, 503])
}

fn read_gate_answering(env: &Environment, seam: &seams::Seam, ok: &[u16]) -> GateRead {
    let running = decl::resolve_version(&env.builds_for(&seam.service), &seam.version, &env.name)
        .ok()
        .and_then(|(label, rec)| crate::declaration_for_build(env, &seam.service, &label, &rec).ok())
        .map(|(d, _)| d);
    let Some(d) = running else {
        // No declaration for the running version at all. `stop_terms` falls
        // back to the shipped file here; the quiet signal does the same, so a
        // shipped `[quiet]` is still honoured (and, if the binary does not
        // serve it, reads unreadable — the safe direction).
        return match decl::load(&env.service_dir(), &seam.service) {
            Ok(d) => read_gate_from_answering(&d.text, &seam.addr, ok),
            Err(_) => GateRead::only(QuietRead::Undeclared),
        };
    };
    read_gate_from_answering(&d.text, &seam.addr, ok)
}

/// The quiet half of [`read_gate_from`].
#[cfg(test)]
pub(crate) fn read_quiet_from(text: &str, addr: &str) -> QuietRead {
    read_gate_from(text, addr).quiet
}

/// [`read_gate`] once the declaration text is in hand.
#[cfg(test)]
pub(crate) fn read_gate_from(text: &str, addr: &str) -> GateRead {
    read_gate_from_answering(text, addr, &[200])
}

fn read_gate_from_answering(text: &str, addr: &str, ok: &[u16]) -> GateRead {
    let stanza = match quiet_stanza(text) {
        Ok(Some(s)) => s,
        Ok(None) => return GateRead::only(QuietRead::Undeclared),
        Err(e) => return GateRead::only(QuietRead::Unreadable(e)),
    };
    if let Some(d) = &stanza.drain {
        if let Err(e) = d.check() {
            return GateRead::only(QuietRead::Unreadable(e));
        }
    }
    let url = format!("http://{addr}{}", stanza.path);
    let body = match http_get_body(addr, &stanza.path, QUIET_READ_TIMEOUT, ok) {
        Ok(b) => b,
        Err(e) => return GateRead::only(QuietRead::Unreadable(format!("GET {url} failed: {e:#}"))),
    };
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return GateRead::only(QuietRead::Unreadable(format!(
                "GET {url} answered something that is not JSON ({e})"
            )))
        }
    };
    let drain = stanza.drain.clone().map(|d| d.read(&v));
    GateRead { quiet: quiet_of(&v, &url, &stanza), drain }
}

fn quiet_of(v: &serde_json::Value, url: &str, stanza: &QuietStanza) -> QuietRead {
    match v.pointer(&stanza.pointer) {
        Some(serde_json::Value::Bool(true)) => {
            QuietRead::Quiet(format!("{url} {} = true", stanza.pointer))
        }
        Some(serde_json::Value::Bool(false)) => {
            let around = stanza
                .pointer
                .rsplit_once('/')
                .and_then(|(parent, _)| v.pointer(parent).cloned())
                .map(|p| format!(" ({p})"))
                .unwrap_or_default();
            QuietRead::Busy(format!("{url} {} = false{around}", stanza.pointer))
        }
        Some(other) => QuietRead::Unreadable(format!(
            "{url} {} is {other}, not a boolean",
            stanza.pointer
        )),
        None => QuietRead::Unreadable(format!(
            "{url} answered JSON with no {} in it — the running binary does not serve this signal",
            stanza.pointer
        )),
    }
}

/// One `GET`, a status in `ok` required (200 for every quiet read), body
/// returned. HTTP/1.0 semantics (`Connection: close`, read to EOF), so no
/// chunking is asked for.
fn http_get_body(addr: &str, path: &str, timeout: Duration, ok: &[u16]) -> Result<String> {
    use std::io::Read;
    use std::net::ToSocketAddrs;
    let sa = addr
        .to_socket_addrs()
        .with_context(|| format!("resolving `{addr}`"))?
        .next()
        .with_context(|| format!("`{addr}` resolved to nothing"))?;
    let mut s = TcpStream::connect_timeout(&sa, timeout).with_context(|| format!("nothing answered at {addr}"))?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes())?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = s.read(&mut chunk).with_context(|| format!("reading the answer from {addr}"))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 1 << 20 {
            bail!("the answer from {addr} is over 1 MiB");
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").context("no end of headers in the answer")?;
    let status = head.lines().next().unwrap_or("");
    let code = status.split_whitespace().nth(1).and_then(|c| c.parse::<u16>().ok());
    if !code.is_some_and(|c| ok.contains(&c)) {
        bail!("answered `{status}`, not 200");
    }
    Ok(body.to_string())
}

/// **SAY A WAIT ONCE PER CHANGE, not once per pass.** A busy service polled
/// every 4 s for an hour is 900 identical lines; the operator needs the
/// transitions. Keyed by service; the value is the kind and the wanted label.
static QUIET_SAID: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

fn quiet_say_once(service: &str, key: String, say: impl FnOnce()) {
    let Ok(mut m) = QUIET_SAID.lock() else { return say() };
    if m.get(service) != Some(&key) {
        m.insert(service.to_string(), key);
        say();
    }
}

fn quiet_forget(service: &str) {
    if let Ok(mut m) = QUIET_SAID.lock() {
        m.remove(service);
    }
}

/// **R9.C7b2 (gap C-7): A RE-PIN THIS DAEMON IS HOLDING BACK WITHOUT A DRAIN**,
/// as the daemon decided it: which build it is waiting to restart onto, since
/// when, and why. Written by the two arms of [`tend`] that decline a re-pin
/// and ask for no drain (the running build reads busy and has no drain door,
/// or its quiet signal cannot be read), cleared on every other outcome of that
/// pass and by every restart. Served on `STATUS` (`hosting[].deferred`) so
/// `cauldron status` READS the daemon's own decision and its age, instead of
/// re-deriving a reason from a fresh gate read and having no "since" at all —
/// which is how a busy-deferred re-pin waited hours unseen (C7 inspect F1).
///
/// Per incarnation, like the park: a new daemon re-reads the gate on its first
/// pass and the wait is re-recorded from then. `since` is the first pass that
/// declined THIS target build, and is kept while the reason changes between
/// busy and unreadable; a pin to a different target starts a new wait.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Deferral {
    pub repin_to: String,
    pub running: String,
    pub since: u64,
    /// `busy` | `unreadable`
    pub kind: &'static str,
    pub why: String,
}

static DEFERRED: Mutex<BTreeMap<String, Deferral>> = Mutex::new(BTreeMap::new());

fn deferral_hold(service: &str, running: &str, repin_to: &str, kind: &'static str, why: &str) {
    let Ok(mut m) = DEFERRED.lock() else { return };
    let since = match m.get(service) {
        Some(d) if d.repin_to == repin_to => d.since,
        _ => crate::secs_since_epoch(),
    };
    m.insert(
        service.to_string(),
        Deferral { repin_to: repin_to.into(), running: running.into(), since, kind, why: why.into() },
    );
}

fn deferral_clear(service: &str) {
    if let Ok(mut m) = DEFERRED.lock() {
        m.remove(service);
    }
}

fn deferral_of(service: &str) -> Option<Deferral> {
    DEFERRED.lock().ok().and_then(|m| m.get(service).cloned())
}

// ══ THE DRAIN (R9.C7c, gap C-7) ════════════════════════════════════════════
//
// R9.C7 made a re-pin wait for a quiet moment, and in production a quiet
// moment does not come: from 23:35Z to 03:38Z on 2026-10-16/17 prod threads
// never once had zero turns executing, because long turns hand off back to
// back (an internal report §C). Waiting for idle there is
// waiting forever.
//
// So a service that can, DRAINS: asked by a signal its running build declares
// (`[quiet.drain] start`), it starts no new turn and queues new prompts, and
// the turns already running finish. The running set then reaches zero — the
// same idle R9.C7 waits for — and the restart goes ahead. If the running set
// has not reached zero by the CAP (`cap_secs`, measured from the instant the
// SERVICE says it began draining, so a holder restart does not reset it), the
// declaration's `at_cap` decides:
//
// * `extend` (the default for a declaration that names none) — keep waiting,
//   and say so once. Never kill. UNBOUNDED: it waits for a LIVE long turn as
//   long as that turn runs, and every new prompt waits behind it. In prod
//   that held threads' intake for 2h35m, the operator's own message ~1 h
//   (DEPLOY-9c, ledger E18-15). Declare it only where that is acceptable.
// * `handoff` — restart anyway, and let the new process re-adopt the turns
//   still running (`recover` → `adopt_turn`). MEASURED against the real
//   binary (`services/threads/tests/drain_pin.rs`): a child that only writes
//   its stdout file survives and is landed once, with its own verdict; a
//   child that writes to STDERR after its daemon is gone still dies of
//   SIGPIPE, and since R9.L2 (ruling E18-7) the adopter schedules a retry for
//   it instead of losing the prompt. THREADS DECLARES `handoff` (R9.C7b), with
//   a cap of 600 s, so a drain blocks intake for at most that long.
//
// A drain is CANCELLED when the pin goes back to the build that is running
// (`cancel`), and its queued prompts start at once.
//
// THE ONE DANGER, made unreachable: the default disposition of SIGUSR1/2 is to
// terminate. A build that predates the door would die of the request instead
// of draining. So the request is sent only with TWO witnesses — the running
// build's declaration declares `[quiet.drain]`, AND that process's own answer
// carries the `since` key (`DrainRead::served`). An older build fails the
// second and keeps R9.C7's plain wait.

/// The cap a `[quiet.drain]` gets when it names none: 30 minutes.
///
/// Chosen against the prod record, not a guess. The longest turn measured
/// while prod threads was never idle was 70 min (de512765, 23:39→00:49Z),
/// another ran 104 min; the short ones were 4–100 s. A cap is where the
/// supervisor stops waiting quietly: under the default `extend` it says so
/// once (a warning) and keeps waiting, never killing; under `handoff` it
/// restarts. 30 min lets nearly every turn simply finish, and makes the rare
/// 70-minute turn a visible, named wait rather than a silent one.
pub(crate) const DEFAULT_DRAIN_CAP_SECS: u64 = 30 * 60;

fn default_drain_cap_secs() -> u64 {
    DEFAULT_DRAIN_CAP_SECS
}

/// What the cap does to turns still running when it fires.
#[derive(serde::Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AtCap {
    /// Restart; the new process re-adopts the turns still running, and a
    /// child SIGPIPE'd by the restart is retried (R9.L2, E18-7). What threads
    /// declares (R9.C7b): it bounds how long a drain blocks intake.
    Handoff,
    /// Keep waiting for them. Never kill. The default when none is declared,
    /// and UNBOUNDED for a live long turn (see the module note above).
    #[default]
    Extend,
}

/// `[quiet.drain]`, as a service declares it.
#[derive(serde::Deserialize, Clone, Debug)]
pub(crate) struct DrainStanza {
    /// The signal that begins a drain (`USR1` or `USR2`).
    pub start: String,
    /// The signal that cancels one.
    pub cancel: String,
    /// JSON pointer, in the `[quiet] path` answer, to when the drain began:
    /// whole epoch seconds, or `null` when not draining. Its PRESENCE is the
    /// second witness that the running process has the door.
    pub since: String,
    /// JSON pointer to how many turns are running.
    pub running: String,
    #[serde(default = "default_drain_cap_secs")]
    pub cap_secs: u64,
    #[serde(default)]
    pub at_cap: AtCap,
    /// R9.C7b: JSON pointer to WHICH turns are running — an array of strings
    /// (threads serves its thread ids at `/restart_gate/turn_threads`). Optional
    /// and informational: `cauldron status` names the turns a drain waits for.
    /// Never read to decide anything; `running` is the count the gate obeys.
    #[serde(default)]
    pub turn_ids: Option<String>,
}

impl DrainStanza {
    /// A drain stanza cauldron cannot act on safely is UNREADABLE, never
    /// "no drain": the signals are the only two whose sending is bounded by
    /// the two-witness rule above.
    fn check(&self) -> std::result::Result<(), String> {
        for (k, sig) in [("start", &self.start), ("cancel", &self.cancel)] {
            if !matches!(sig.as_str(), "USR1" | "USR2") {
                return Err(format!(
                    "its `[quiet.drain] {k} = \"{sig}\"` is not USR1 or USR2, the only signals \
                     cauldron sends as a drain request"
                ));
            }
        }
        if self.start == self.cancel {
            return Err("its `[quiet.drain]` start and cancel are the same signal".into());
        }
        if self.cap_secs == 0 {
            return Err("its `[quiet.drain] cap_secs` is 0".into());
        }
        Ok(())
    }

    fn read(self, v: &serde_json::Value) -> DrainRead {
        let at = v.pointer(&self.since);
        let turn_ids = self
            .turn_ids
            .as_deref()
            .and_then(|p| v.pointer(p))
            .and_then(serde_json::Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect());
        DrainRead {
            served: at.is_some_and(|x| x.is_null() || x.is_u64()),
            since: at.and_then(serde_json::Value::as_u64),
            running: v.pointer(&self.running).and_then(serde_json::Value::as_u64),
            turn_ids,
            stanza: self,
            declared: None,
        }
    }
}

// ══ A PIN'S OWN DRAIN TERMS (R9.C7f, gap C-7, E18-15/E18-17) ═══════════════
//
// The drain obeys the RUNNING build's `[quiet.drain]`, and that is the build
// being replaced. Prod threads' running build declares `at_cap = "extend"`,
// which has no upper bound, so the pin that installs a build declaring
// `handoff` would block every new turn behind any long turn still running
// (E18-15: 2h43m). `cauldron pin <svc> <ver> --at-cap handoff --drain-cap
// <secs>` replaces either value FOR THAT PIN ONLY. It travels the channel the
// pin itself does — `env.toml`, `[services.<svc>.drain]`, stamped with the
// version it was given for — and the HOLDER applies it, in `tend`'s re-pin
// drain, so a pin the holder reads later under a different version (a newer
// pin, a rollback) never inherits it. Absent flags write nothing, and the
// declaration governs exactly as before.

/// The largest `--drain-cap` a pin may carry: one hour. The cap bounds how
/// long a drain blocks intake; a pin that wants more than an hour wants
/// `extend`, and should say so.
pub(crate) const MAX_PIN_DRAIN_CAP_SECS: u64 = 3600;

/// Why a pin's drain override was refused. Typed so the CLI refuses BEFORE it
/// writes the pin (no drain can open on a refused override) and the holder
/// names the same fault for one written by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PinDrainError {
    /// `--drain-cap 0`: a drain that is capped before it begins.
    CapZero,
    /// `--drain-cap` over [`MAX_PIN_DRAIN_CAP_SECS`].
    CapOverMax(u64),
    /// `--drain-cap` that is not a whole number of seconds.
    CapNotSeconds(String),
    /// `--at-cap` that is neither `handoff` nor `extend`.
    AtCapUnknown(String),
}

impl std::fmt::Display for PinDrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PinDrainError::CapZero => write!(
                f,
                "`--drain-cap 0` is refused: the cap is 1..={MAX_PIN_DRAIN_CAP_SECS} seconds"
            ),
            PinDrainError::CapOverMax(n) => write!(
                f,
                "`--drain-cap {n}` is refused: the cap is 1..={MAX_PIN_DRAIN_CAP_SECS} seconds \
                 (a drain that should wait longer than that wants `--at-cap extend`)"
            ),
            PinDrainError::CapNotSeconds(v) => write!(
                f,
                "`--drain-cap {v}` is refused: not a whole number of seconds \
                 (1..={MAX_PIN_DRAIN_CAP_SECS})"
            ),
            PinDrainError::AtCapUnknown(v) => {
                write!(f, "`--at-cap {v}` is refused: it is `handoff` or `extend`")
            }
        }
    }
}

impl std::error::Error for PinDrainError {}

/// `--at-cap <word>`.
pub(crate) fn parse_at_cap(v: &str) -> std::result::Result<AtCap, PinDrainError> {
    match v {
        "handoff" => Ok(AtCap::Handoff),
        "extend" => Ok(AtCap::Extend),
        other => Err(PinDrainError::AtCapUnknown(other.to_string())),
    }
}

/// `--drain-cap <secs>`: 1..=[`MAX_PIN_DRAIN_CAP_SECS`].
pub(crate) fn parse_drain_cap(v: &str) -> std::result::Result<u64, PinDrainError> {
    let n: u64 = v.parse().map_err(|_| PinDrainError::CapNotSeconds(v.to_string()))?;
    check_drain_cap(n)
}

fn check_drain_cap(n: u64) -> std::result::Result<u64, PinDrainError> {
    match n {
        0 => Err(PinDrainError::CapZero),
        n if n > MAX_PIN_DRAIN_CAP_SECS => Err(PinDrainError::CapOverMax(n)),
        n => Ok(n),
    }
}

/// A pin's drain override, checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DrainOverride {
    pub at_cap: Option<AtCap>,
    pub cap_secs: Option<u64>,
}

/// The override `env.toml` carries for `service`'s CURRENT pin, if any.
/// `Ok(None)`: none, or one given with another pin (so not this one's).
pub(crate) fn pin_drain_override(
    m: &Manifest,
    service: &str,
) -> std::result::Result<Option<DrainOverride>, PinDrainError> {
    let Some(p) = m.services.get(service) else { return Ok(None) };
    let Some(o) = p.drain.as_ref().filter(|o| o.for_version == p.version) else { return Ok(None) };
    let at_cap = o.at_cap.as_deref().map(parse_at_cap).transpose()?;
    let cap_secs = o.cap_secs.map(check_drain_cap).transpose()?;
    if at_cap.is_none() && cap_secs.is_none() {
        return Ok(None);
    }
    Ok(Some(DrainOverride { at_cap, cap_secs }))
}

/// **R9.C7f2 (C-7, E19-8): `service`'s pin `pin` has COMPLETED** — the
/// holder restarted the running build onto it, or launched it (up or failed)
/// with nothing running, or (R9.C7f3) found it already running — so the
/// drain override given with that pin is spent
/// and is removed from `env.toml`. Re-read rather than the pass's snapshot,
/// and cleared only while it still belongs to `pin` (a newer `pin` may have
/// written its own since). A failure to read or write is said, never fatal:
/// the override is then still bound to its pin and only `set_version` drops
/// it.
/// **R9.C7f5 (C-7, inspect R9.C7f4 low 2): the longest the holder waits for
/// the env.toml writers' lock.** Every holder write to env.toml (`pin_completed`,
/// auto-rollback, good-recording) takes the lock through
/// [`Manifest::lock_within`] with this bound, and each is retried on a later
/// pass, so a `cauldron pin` stopped while holding the lock delays a pass by at
/// most this per write and never stalls supervision. The holder's READS of
/// env.toml take no lock at all: the save is an atomic rename, so a reader sees
/// the old file or the new one, never a torn one.
pub(crate) const HOLDER_ENV_LOCK_BOUND: Duration = Duration::from_millis(500);

pub(crate) fn pin_completed(env: &Environment, service: &str, pin: &str) {
    let path = env.manifest();
    // R9.C7f4: the load-clear-save runs under the env.toml writers' lock, so
    // a `cauldron pin` landing meanwhile is not overwritten (ledger R9.C7f3-2).
    // R9.C7f5: never an unbounded wait — a writer stuck holding the lock
    // costs this pass at most HOLDER_ENV_LOCK_BOUND. The override then stays,
    // still bound to `pin`, and the next pass (which sees the pin running and
    // its override still there) clears it.
    let _env_toml = match Manifest::lock_within(&path, HOLDER_ENV_LOCK_BOUND) {
        Ok(l) => l,
        Err(e) => {
            warn_at!("`{service}`'s pin {pin} completed but env.toml could not be locked to clear its drain override: {e:#}");
            return;
        }
    };
    let mut m = match Manifest::load(&path) {
        Ok(m) => m,
        Err(e) => {
            warn_at!("`{service}`'s pin {pin} completed but env.toml could not be read to clear its drain override: {e:#}");
            return;
        }
    };
    if !m.clear_pin_drain(service, pin) {
        return;
    }
    match m.save(&path) {
        Ok(()) => println!(
            "cauldron: `{service}`'s pin {pin} has completed, so the drain terms it carried \
             are cleared from env.toml; a later drain is under the running build's own `[quiet.drain]`"
        ),
        Err(e) => {
            warn_at!("`{service}`'s pin {pin} completed but its drain override could not be cleared: {e:#}");
        }
    }
}

/// Holder side: an override in `env.toml` that does not check (written by
/// hand) is IGNORED, said once per service, and the running declaration
/// governs — the drain the operator would have had with no override at all.
static OVERRIDE_REFUSED: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

fn pin_drain_override_or_warn(m: &Manifest, service: &str) -> Option<DrainOverride> {
    match pin_drain_override(m, service) {
        Ok(o) => o,
        Err(e) => {
            let said = OVERRIDE_REFUSED
                .lock()
                .map(|mut s| s.insert(service.to_string(), e.to_string()) != Some(e.to_string()))
                .unwrap_or(true);
            if said {
                warn_at!(
                    "`{service}`'s pin carries a drain override in env.toml that is refused ({e}); \
                     IGNORING it, so the running build's own `[quiet.drain]` governs the drain"
                );
            }
            None
        }
    }
}

impl DrainRead {
    /// This read with a pin's override applied: each value the override
    /// carries replaces the declaration's, and [`DrainRead::declared`]
    /// remembers what the declaration said (so `status` can say both).
    pub(crate) fn with_override(mut self, o: Option<DrainOverride>) -> DrainRead {
        let Some(o) = o else { return self };
        self.declared = Some((self.stanza.at_cap, self.stanza.cap_secs));
        if let Some(a) = o.at_cap {
            self.stanza.at_cap = a;
        }
        if let Some(c) = o.cap_secs {
            self.stanza.cap_secs = c;
        }
        self
    }
}

/// What one read said about the drain.
#[derive(Debug, Clone)]
pub(crate) struct DrainRead {
    pub stanza: DrainStanza,
    /// The running process serves the `since` key at all (null or a number):
    /// it has the drain door. False for a build that predates it.
    pub served: bool,
    /// When the drain began (epoch seconds), if one is in progress.
    pub since: Option<u64>,
    /// Turns running, as the service counts them.
    pub running: Option<u64>,
    /// Which turns those are, when the stanza names `turn_ids` and the
    /// service serves them (R9.C7b). Informational only.
    pub turn_ids: Option<Vec<String>>,
    /// R9.C7f: `Some((at_cap, cap_secs))` the RUNNING DECLARATION says, when
    /// a pin's override replaced them in `stanza`; `None` when it governs.
    pub declared: Option<(AtCap, u64)>,
}

impl DrainRead {
    /// When the cap fires, if a drain is in progress.
    pub(crate) fn cap_at(&self) -> Option<u64> {
        self.since.map(|s| s.saturating_add(self.stanza.cap_secs))
    }
}

/// One read of a service's restart gate: its quiet signal and its drain.
#[derive(Debug, Clone)]
pub(crate) struct GateRead {
    pub quiet: QuietRead,
    pub drain: Option<DrainRead>,
}

impl GateRead {
    fn only(quiet: QuietRead) -> Self {
        GateRead { quiet, drain: None }
    }
}

enum DrainStep {
    Wait,
    HandOff,
}

/// A re-pin found the service busy and able to drain: ask, wait, or (at the
/// cap) decide.
#[allow(clippy::too_many_arguments)]
fn drain_step(
    env: &Environment,
    policy: &Policy,
    name: &str,
    seam: &seams::Seam,
    running: &str,
    want: &str,
    read: &str,
    d: &DrainRead,
) -> DrainStep {
    let n = d.running.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
    let cap = d.stanza.cap_secs;
    drain_watch(name);
    let Some(since) = d.since else {
        // Repeated every pass until the service says it is draining: the
        // request is idempotent, and a lost signal must not stall the deploy.
        crate::signal(seam.pid, &d.stanza.start);
        quiet_say_once(name, format!("drain {want}"), || {
            println!(
                "cauldron: `{name}` is running version {running} and `{}` now pins {want}, but \
                 it is busy ({read}). Asked it to DRAIN (SIG{} to pid {}): it starts no new turn \
                 and queues new prompts, its {n} running turn(s) finish, and then it is \
                 restarted onto the pin — or at the cap, {cap}s after the drain began, {}.{}",
                env.name,
                d.stanza.start,
                seam.pid,
                at_cap_words(d.stanza.at_cap),
                override_words(d)
            )
        });
        return DrainStep::Wait;
    };
    let cap_at = since.saturating_add(cap);
    if crate::secs_since_epoch() < cap_at {
        quiet_say_once(name, format!("draining {want} {since}"), || {
            println!(
                "cauldron: `{name}` is draining since {}, {n} running, cap at {} — re-pin to \
                 {want} waits for the running turns (re-read every {}s)",
                crate::jobs::utc_iso(since),
                crate::jobs::utc_iso(cap_at),
                policy.poll_secs
            )
        });
        return DrainStep::Wait;
    }
    match d.stanza.at_cap {
        AtCap::Handoff => {
            quiet_forget(name);
            drain_unwatch(name);
            warn_at!(
                "`{name}` reached its drain cap ({cap}s; draining since {}) with {n} turn(s) \
                 still running. Restarting it onto {want} and HANDING THOSE TURNS OFF: each \
                 live child keeps running in its own process group and the new process \
                 re-adopts it from its durable row. Nothing is killed by this restart; a \
                 child that writes stderr after it dies of SIGPIPE and is retried (R9.L2).{}",
                crate::jobs::utc_iso(since),
                override_words(d)
            );
            DrainStep::HandOff
        }
        AtCap::Extend => {
            quiet_say_once(name, format!("extend {want} {since}"), || {
                warn_at!(
                    "`{name}` reached its drain cap ({cap}s; draining since {}) with {n} \
                     turn(s) still running, and declares `at_cap = \"extend\"`: NOT restarting \
                     it and not killing them. The restart onto {want} follows when they finish. \
                     `cauldron restart {name}` waits for them too (it drains); only `cauldron \
                     stop {name}` ends it sooner, and that {KILLS_TURNS}.{}",
                    crate::jobs::utc_iso(since),
                    override_words(d)
                );
            });
            DrainStep::Wait
        }
    }
}

// ══ A RESTART THAT DRAINS (R9.C7d, gap C-7) ════════════════════════════════
//
// C7c's drain covered the RE-PIN. Two other paths still took threads down with
// turns running, and each turn whose child wrote stderr after its daemon was
// gone died of SIGPIPE with its prompt lost (an internal report):
//
// * `cauldron restart <svc>` (`Ask::Restart`), the path every wait message
//   offered as "now regardless" (finding F-A);
// * the relaunch of a service whose health check answers 503 (R9.C6's stale
//   store), which stopped it and launched it again.
//
// Both now go through the same door, under the same two witnesses (the RUNNING
// build declares `[quiet.drain]`, and its own answer carries `since`): a busy
// service is asked to drain and is restarted when its running set reaches
// zero. A build without the door is restarted exactly as before.
//
// AT THE CAP the two differ, deliberately:
// * a requested restart obeys `at_cap` (`extend`, the default, keeps waiting
//   and never kills; `handoff` restarts and the new process re-adopts);
// * an UNHEALTHY service is restarted at the cap whatever `at_cap` says. It is
//   wedged on its store; an unbounded wait there is an indefinite outage of the
//   very service the relaunch exists to recover, and turns that cannot write
//   their results may never end. The cap bounds the drain, then the C6
//   relaunch goes ahead, handing the turns still running off as `handoff`
//   does (ledger R9.C7d-1).
//
// Stop and crash cannot drain and are left alone.

/// Services whose restart is waiting for a drain, and why (`requested` or
/// `down`). In memory: a holder that restarts mid-drain forgets the restart,
/// and its first look at the service cancels the drain (`drain_watch_due`),
/// so nothing is left draining for a restart nobody remembers.
static DRAIN_RESTART: Mutex<BTreeMap<String, &'static str>> = Mutex::new(BTreeMap::new());

fn drain_restart_pending(service: &str) -> Option<&'static str> {
    DRAIN_RESTART.lock().ok().and_then(|m| m.get(service).copied())
}

fn drain_restart_forget(service: &str) {
    if let Ok(mut m) = DRAIN_RESTART.lock() {
        m.remove(service);
    }
}

/// What to do with a restart, read off the service's restart gate.
#[derive(Debug, PartialEq, Eq)]
enum RestartGate {
    /// Restart on this pass.
    Now,
    /// A drain is under way (or was just asked for): not on this pass.
    Wait,
}

/// The words for what a restart costs when the service cannot drain: every
/// turn running at the stop is killed, and its prompt is lost if its child
/// writes stderr afterwards.
pub(crate) const KILLS_TURNS: &str = "KILLS every turn still running: a turn's child that writes \
     stderr after its daemon is gone dies of SIGPIPE, and that turn's prompt is lost";

/// **SHOULD THIS RESTART HAPPEN NOW, OR WAIT FOR A DRAIN?** For `cauldron
/// restart` (`why = requested`) and the unhealthy relaunch (`why = down`,
/// `unhealthy = true`, which reads the gate off a 503 answer too).
fn restart_gate(
    env: &Environment,
    policy: &Policy,
    name: &str,
    seam: &seams::Seam,
    why: &'static str,
    unhealthy: bool,
) -> RestartGate {
    let gate = if unhealthy { read_gate_unhealthy(env, seam) } else { read_gate(env, seam) };
    let what = if unhealthy {
        format!("the relaunch of the unhealthy `{name}`")
    } else {
        format!("the requested restart of `{name}`")
    };
    let Some(d) = gate.drain.filter(|d| d.served) else {
        // NO DOOR: today's behaviour, and said plainly when it costs turns.
        drain_restart_forget(name);
        quiet_forget(name);
        let _ = match &gate.quiet {
            QuietRead::Busy(read) => warn_at!(
                "{what}: it is busy ({read}) and its running build has no drain door \
                 (`[quiet.drain]` and a `since` in its answer), so it is restarted NOW, which \
                 {KILLS_TURNS}"
            ),
            QuietRead::Unreadable(why) if !unhealthy => warn_at!(
                "{what}: whether turns are running cannot be read ({why}), so it is \
                 restarted NOW; if any are, this {KILLS_TURNS}"
            ),
            _ => false,
        };
        return RestartGate::Now;
    };
    let idle = matches!(gate.quiet, QuietRead::Quiet(_)) || d.running == Some(0);
    if idle {
        let was = DRAIN_RESTART.lock().ok().and_then(|mut m| m.remove(name));
        quiet_forget(name);
        drain_unwatch(name);
        if was.is_some() {
            println!(
                "cauldron: `{name}` has DRAINED for {what} (draining since {}): no turn \
                 is running, so it is restarted now and the new process starts the prompts \
                 queued during the drain",
                d.since.map(crate::jobs::utc_iso).unwrap_or_else(|| "?".into())
            );
        }
        return RestartGate::Now;
    }
    if let Ok(mut m) = DRAIN_RESTART.lock() {
        m.insert(name.to_string(), why);
    }
    drain_watch(name);
    let n = d.running.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
    let cap = d.stanza.cap_secs;
    let at_cap = if unhealthy { AtCap::Handoff } else { d.stanza.at_cap };
    let Some(since) = d.since else {
        // Repeated every pass until it says it is draining: idempotent.
        crate::signal(seam.pid, &d.stanza.start);
        quiet_say_once(name, format!("restart-drain {why}"), || {
            println!(
                "cauldron: {what}: it is busy ({n} running turn(s)). Asked it to DRAIN \
                 (SIG{} to pid {}): it starts no new turn and queues new prompts, its running \
                 turns finish, and then it is restarted — or at the cap, {cap}s after the drain \
                 began, {}.",
                d.stanza.start,
                seam.pid,
                if unhealthy {
                    "it is relaunched anyway (it is unhealthy) and the turns still running are \
                     handed off to the new process"
                } else {
                    at_cap_words(at_cap)
                }
            )
        });
        return RestartGate::Wait;
    };
    let cap_at = since.saturating_add(cap);
    if crate::secs_since_epoch() < cap_at {
        quiet_say_once(name, format!("restart-draining {why} {since}"), || {
            println!(
                "cauldron: `{name}` is draining since {}, {n} running, cap at {} — {what} \
                 waits for the running turns (re-read every {}s)",
                crate::jobs::utc_iso(since),
                crate::jobs::utc_iso(cap_at),
                policy.poll_secs
            )
        });
        return RestartGate::Wait;
    }
    match at_cap {
        AtCap::Handoff => {
            drain_restart_forget(name);
            quiet_forget(name);
            drain_unwatch(name);
            warn_at!(
                "`{name}` reached its drain cap ({cap}s; draining since {}) with {n} turn(s) \
                 still running. {}: HANDING THOSE TURNS OFF — each live child keeps running in \
                 its own process group and the new process re-adopts it from its durable row. \
                 Nothing is killed by this restart; a child that writes stderr after it dies of \
                 SIGPIPE and is retried (R9.L2).",
                crate::jobs::utc_iso(since),
                if unhealthy {
                    "It is UNHEALTHY, so it is relaunched now whatever `at_cap` says"
                } else {
                    "Restarting it as requested"
                }
            );
            RestartGate::Now
        }
        AtCap::Extend => {
            quiet_say_once(name, format!("restart-extend {since}"), || {
                warn_at!(
                    "`{name}` reached its drain cap ({cap}s; draining since {}) with {n} turn(s) \
                     still running, and declares `at_cap = \"extend\"`: the requested restart \
                     waits for them and kills none. Nothing restarts it sooner without killing \
                     them: `cauldron stop {name}` would, and that {KILLS_TURNS}.",
                    crate::jobs::utc_iso(since)
                );
            });
            RestartGate::Wait
        }
    }
}

/// R9.C7f: ` These drain terms are this pin's override …` when a pin's
/// `--at-cap`/`--drain-cap` replaced the running declaration's, else empty.
pub(crate) fn override_words(d: &DrainRead) -> String {
    match d.declared {
        None => String::new(),
        Some((a, c)) => format!(
            " These drain terms (at_cap = \"{}\", cap {}s) are OVERRIDDEN by this pin; the \
             running build declares at_cap = \"{}\", cap {}s.",
            at_cap_name(d.stanza.at_cap),
            d.stanza.cap_secs,
            at_cap_name(a),
            c
        ),
    }
}

/// `handoff` / `extend`, as declared.
pub(crate) fn at_cap_name(a: AtCap) -> &'static str {
    match a {
        AtCap::Handoff => "handoff",
        AtCap::Extend => "extend",
    }
}

pub(crate) fn at_cap_words(a: AtCap) -> &'static str {
    match a {
        AtCap::Handoff => {
            "the turns still running are handed off to the new process, which re-adopts them"
        }
        AtCap::Extend => "the wait is extended for the turns still running; none is killed",
    }
}

/// The pin is the running build again: cancel a drain in progress.
fn cancel_drain_if_draining(env: &Environment, name: &str, seam: &seams::Seam) {
    let g = read_gate(env, seam);
    match g.drain.filter(|d| d.served) {
        Some(d) if d.since.is_some() => {
            crate::signal(seam.pid, &d.stanza.cancel);
            let since = d.since.unwrap_or_default();
            quiet_say_once(name, format!("cancel {since}"), || {
                println!(
                    "cauldron: `{name}` was DRAINING (since {}) for a re-pin that is no longer \
                     wanted: `{}` pins the running version {} again. Cancelled the drain (SIG{} \
                     to pid {}); its queued prompts start now.",
                    crate::jobs::utc_iso(since),
                    env.name,
                    seam.version,
                    d.stanza.cancel,
                    seam.pid
                )
            });
        }
        // Not draining (any more), or no door: nothing to watch.
        _ => drain_unwatch(name),
    }
}

/// Services this daemon asked to drain (or found draining), and services it
/// has looked at once since it started. A drain outlives the holder that asked
/// for it, so a new holder looks once at every service before trusting that it
/// has nothing to cancel.
static DRAIN_ASKED: Mutex<std::collections::BTreeSet<String>> =
    Mutex::new(std::collections::BTreeSet::new());
static DRAIN_LOOKED: Mutex<std::collections::BTreeSet<String>> =
    Mutex::new(std::collections::BTreeSet::new());

fn drain_watch(service: &str) {
    if let Ok(mut s) = DRAIN_ASKED.lock() {
        s.insert(service.to_string());
    }
}

fn drain_unwatch(service: &str) {
    if let Ok(mut s) = DRAIN_ASKED.lock() {
        s.remove(service);
    }
}

/// Should this pass look for a drain to cancel?
fn drain_watch_due(service: &str) -> bool {
    let asked = DRAIN_ASKED.lock().map(|s| s.contains(service)).unwrap_or(false);
    let first = DRAIN_LOOKED.lock().map(|mut s| s.insert(service.to_string())).unwrap_or(false);
    asked || first
}

#[cfg(test)]
mod quiet_tests {
    use super::{quiet_stanza, read_quiet_from, QuietRead};

    #[test]
    fn no_quiet_stanza_is_undeclared_and_a_present_one_parses() {
        assert!(matches!(quiet_stanza("name = \"x\"\n"), Ok(None)));
        let s = quiet_stanza("name = \"x\"\n[quiet]\npath = \"/health\"\npointer = \"/a/b\"\n").unwrap().unwrap();
        assert_eq!((s.path.as_str(), s.pointer.as_str()), ("/health", "/a/b"));
        // Opposed: a [quiet] missing its pointer is NOT undeclared.
        assert!(quiet_stanza("name = \"x\"\n[quiet]\npath = \"/health\"\n").is_err());
        assert!(matches!(read_quiet_from("name = \"x\"\n", "127.0.0.1:1"), QuietRead::Undeclared));
    }
}

#[cfg(test)]
mod backoff_reset_tests {
    //! The STABLE RESET of the restart backoff, on an injected clock.
    //!
    //! R5.S1's inspector broke the reset predicate (`&& true`, so it reset on
    //! every live pass) and every test stayed green: the subprocess backoff test
    //! kills its service before one poll sees it live, so the reset branch was
    //! never entered, and every fixture set `stable_after_secs = 600`. An
    //! over-eager reset is exactly the crash loop at poll rate the backoff
    //! exists to end, and nothing would have gone red.
    //!
    //! So the decision takes its clock as an argument and these tests hand it
    //! instants: the 60-second property is proven in microseconds, and the
    //! period outlasted (doctrine 4) is `stable_after_secs` itself, crossed on
    //! both sides of the boundary — 59 s does NOT reset, 60 s does.
    use super::{Child, Policy};
    use std::time::{Duration, Instant};

    /// The shipped defaults: base 2 s, max 300 s, stable after 60 s.
    fn policy() -> Policy {
        let p = Policy::default();
        assert_eq!((p.backoff_base_secs, p.backoff_max_secs, p.stable_after_secs), (2, 300, 60), "defaults moved; re-read these tests");
        p
    }

    /// A child that has crash-looped three times, the last launch at `t`. The
    /// next wait has climbed 2 → 4 → 8.
    fn crash_looped(p: &Policy, t0: Instant) -> (Child, Instant) {
        let mut c = Child::new(false);
        assert_eq!(c.launched(p, t0), 2);
        let t1 = t0 + Duration::from_secs(2);
        assert_eq!(c.backoff_remaining(t0 + Duration::from_secs(1)), Some(Duration::from_secs(1)), "a launch inside its backoff was allowed");
        assert_eq!(c.backoff_remaining(t1), None);
        assert_eq!(c.launched(p, t1), 4);
        let t2 = t1 + Duration::from_secs(4);
        assert_eq!(c.launched(p, t2), 8);
        assert_eq!(c.launches, 3);
        (c, t2)
    }

    #[test]
    fn up_for_stable_after_60s_resets_the_backoff_to_its_2s_base() {
        let p = policy();
        let (mut c, last) = crash_looped(&p, Instant::now());
        // Seen live, at exactly stable_after_secs since the last launch.
        assert!(c.observed_live(&p, last + Duration::from_secs(60)), "60 s up did not reset the backoff");
        assert_eq!((c.launches, c.not_before), (0, None));
        // It dies: the relaunch is IMMEDIATE (no backoff left to wait out) …
        let died = last + Duration::from_secs(61);
        assert_eq!(c.backoff_remaining(died), None, "a reset service still waited before relaunch");
        // … and the wait after THAT launch is back to the 2 s base, not 16 s.
        assert_eq!(c.launched(&p, died), 2, "the backoff did not return to its base after a stable run");
    }

    #[test]
    fn up_for_59s_is_not_stable_and_the_backoff_keeps_climbing() {
        let p = policy();
        let (mut c, last) = crash_looped(&p, Instant::now());
        // Seen live on every poll up to one second short of stable.
        for s in [1, 4, 30, 59] {
            assert!(!c.observed_live(&p, last + Duration::from_secs(s)), "the backoff reset after only {s}s up");
        }
        assert_eq!(c.launches, 3);
        // It dies at 59 s: the next launch waits 16 s, the climb continued.
        assert_eq!(c.launched(&p, last + Duration::from_secs(59)), 16, "the climb was reset by a run shorter than stable_after_secs");
    }

    #[test]
    fn a_reset_clears_a_pending_wait_longer_than_stable_after() {
        // Past the 6th launch the wait (64 s, then 128 s, 256 s) is LONGER
        // than stable_after_secs. A service that stays up 60 s and then dies
        // must relaunch now, not sit out the rest of a wait its crash loop
        // earned before it recovered.
        let p = policy();
        let mut c = Child::new(false);
        let mut t = Instant::now();
        for _ in 0..8 {
            let w = c.launched(&p, t);
            t += Duration::from_secs(w);
        }
        let last = c.launched_at.unwrap();
        assert_eq!(c.not_before, Some(last + Duration::from_secs(256)), "the climb did not reach a wait longer than 60 s");
        assert!(c.observed_live(&p, last + Duration::from_secs(60)));
        assert_eq!(c.backoff_remaining(last + Duration::from_secs(61)), None, "a stable service still sat out its old backoff");
    }

    #[test]
    fn a_child_that_never_launched_has_nothing_to_reset() {
        // A re-attached child: live, launches = 0. Not a reset, and not a
        // message claiming one.
        let p = policy();
        let mut c = Child::new(true);
        assert!(!c.observed_live(&p, Instant::now()));
    }
}

#[cfg(test)]
mod crash_loop_tests {
    //! R9.C3 (gap C-3): a launch that comes up and dies before
    //! `stable_after_secs` is a failed launch, counted toward rollback and
    //! park; only a run of `stable_after_secs` clears the count. The period
    //! outlasted (doctrine 4) is `stable_after_secs` = 60 (the shipped
    //! default), crossed on both sides: 59 s up does not prove, 60 s does.
    use super::{Child, Policy};

    fn policy() -> Policy {
        let p = Policy::default();
        assert_eq!(p.stable_after_secs, 60, "default moved; re-read these tests");
        p
    }

    #[test]
    fn a_launch_that_binds_then_dies_inside_stable_after_60s_counts_toward_rollback() {
        let p = policy();
        let mut c = Child::new(false);
        for n in 1..=3u32 {
            c.came_up();
            assert!(!c.proven_by(&p, 20), "20 s up read as stable");
            assert!(!c.proven_by(&p, 59), "59 s up read as stable");
            assert_eq!(c.start_failures, n - 1, "a live pass before stable cleared the count");
            assert_eq!(c.died_unproven(), Some(n), "death {n} after 59 s up was not counted");
            assert_eq!(c.died_unproven(), None, "one death was counted twice");
        }
        assert_eq!(c.start_failures, 3);
    }

    #[test]
    fn a_launch_up_past_stable_after_60s_clears_the_count_and_its_later_death_is_not_counted() {
        let p = policy();
        let mut c = Child::new(false);
        c.start_failures = 2;
        c.came_up();
        assert!(!c.proven_by(&p, 59));
        assert_eq!(c.start_failures, 2, "a live pass before stable cleared the count");
        assert!(c.proven_by(&p, 60), "60 s up did not read as stable");
        assert_eq!(c.start_failures, 0, "a stable run did not clear the count");
        assert_eq!(c.died_unproven(), None, "a death after a stable run was counted as unstable");
        assert_eq!(c.start_failures, 0);
    }

    #[test]
    fn a_reattached_child_seen_live_inside_stable_after_60s_clears_its_count_as_before() {
        // Not launched by this loop, so nothing to prove: unchanged behaviour.
        let p = policy();
        let mut c = Child::new(true);
        c.start_failures = 2;
        assert!(!c.proven_by(&p, 5));
        assert_eq!(c.start_failures, 0);
        assert_eq!(c.died_unproven(), None);
    }
}

/// **R9.C7f5 (C-7, inspect R9.C7f4 low 2): THE HOLDER NEVER WAITS UNBOUNDEDLY
/// ON THE env.toml LOCK.** A stuck writer holds `.env.toml.lock` (another open
/// file description, so the flock genuinely conflicts); the holder's writes
/// return within `HOLDER_ENV_LOCK_BOUND` (500 ms), leave env.toml as it was,
/// and do their work once the writer lets go.
#[cfg(test)]
mod c7f5_holder_lock_tests {
    use super::{pin_completed, HOLDER_ENV_LOCK_BOUND};
    use env::decl::{Manifest, PinDrain};
    use env::paths::Source;
    use env::Environment;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn scratch_env() -> Environment {
        let root = std::env::temp_dir().join(format!(
            "cauldron-c7f5-holder-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".cauldron")), "scratch must not be ~/.cauldron");
        let data_root = root.join("data").join("c7f5");
        std::fs::create_dir_all(&data_root).unwrap();
        let env = Environment {
            root,
            root_source: Source::CauldronHome,
            name: "c7f5".into(),
            name_source: Source::Flag,
            data_root,
        };
        let mut m = Manifest::default();
        m.set_version("s0", "v1");
        m.services.get_mut("s0").unwrap().drain =
            Some(PinDrain { for_version: "v1".into(), at_cap: None, cap_secs: Some(30) });
        m.save(&env.manifest()).unwrap();
        env
    }

    fn override_left(env: &Environment) -> bool {
        Manifest::load(&env.manifest()).unwrap().services["s0"].drain.is_some()
    }

    /// Runs `f` on a thread and returns how long it took, or panics if it has
    /// not returned in 5 s — so a blocking (unbounded) lock shows as a red,
    /// not a hung suite.
    fn timed<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> (T, Duration) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let out = f();
            let _ = tx.send((out, t0.elapsed()));
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the holder's env.toml write did not return within 5 s while a writer held the lock")
    }

    #[test]
    fn c7f5_c_holder_writes_return_within_500ms_bound_while_a_stuck_writer_holds_the_lock() {
        assert_eq!(HOLDER_ENV_LOCK_BOUND, Duration::from_millis(500), "the test name states the bound");
        let env = scratch_env();
        let stuck = Manifest::lock(&env.manifest()).unwrap();

        // pin_completed: bounded, and leaves the override for the next pass.
        let e2 = env.clone();
        let ((), took) = timed(move || pin_completed(&e2, "s0", "v1"));
        assert!(took >= HOLDER_ENV_LOCK_BOUND, "returned before its bound, so the lock was not contended: {took:?}");
        assert!(took < HOLDER_ENV_LOCK_BOUND + Duration::from_millis(1000), "waited past its bound: {took:?}");
        assert!(override_left(&env), "cleared the override without the lock");

        // record_good_if_stable (the holder's good-recording): bounded, an error.
        let e2 = env.clone();
        let (r, took) = timed(move || crate::record_good_if_stable(&e2, "s0", "v1", 100, 10).map_err(|e| format!("{e:#}")));
        assert!(took >= HOLDER_ENV_LOCK_BOUND && took < HOLDER_ENV_LOCK_BOUND + Duration::from_millis(1000), "{took:?}");
        let err = r.expect_err("recorded good without the lock");
        assert!(err.contains("gave up after 500ms"), "{err}");

        // Opposing: once the writer lets go, the same calls do their work.
        drop(stuck);
        let e2 = env.clone();
        let ((), took) = timed(move || pin_completed(&e2, "s0", "v1"));
        assert!(took < Duration::from_millis(400), "a free lock still waited: {took:?}");
        assert!(!override_left(&env), "the spent override was not cleared with the lock free");
        assert!(crate::record_good_if_stable(&env, "s0", "v1", 100, 10).unwrap());
        assert_eq!(Manifest::load(&env.manifest()).unwrap().services["s0"].good.first().map(String::as_str), Some("v1"));
        let _ = std::fs::remove_dir_all(&env.root);
    }
}
