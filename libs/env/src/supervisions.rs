//! **THE SUPERVISOR'S OWN RECORD OF WHAT IT DID TO A SERVICE.**
//!
//! # The question this exists to answer
//!
//! Three proofs carried `eq "the live daemon is the same process, never
//! restarted"`, comparing `pgrep -x threads` before and after a run of eight
//! to twelve minutes. That guard is **wrong in both directions**, and this
//! module is the thing that replaces it rather than patching it.
//!
//! **It goes red because the supervisor did its job.** Now that cauldron hosts
//! `threads` with `auto_restart` and `escalate = true`, a legitimate re-pin
//! inside the window restarts the service, the pid set changes, and the
//! assertion fails. Not hypothetical: `tts` went 38752 -> 80756 at 11:11:44 on
//! 2026-10-09, launched from
//! `~/.cauldron/data/shared/builds/tts/2303409776b0-dev/tts`, absorbing a
//! sibling's commit. A proof that goes red when the product works trains its
//! reader to discount the red.
//!
//! **And it goes GREEN when the service really did restart.** `threads`
//! self-updates by `execv`, which replaces the process image while keeping the
//! pid, the ppid and the reported start time. So "did this process restart" is
//! not answerable from the process table at all — which is the deeper reason
//! the guard cannot work as written, and the reason this is not a bug to be
//! fixed by comparing something else about the process.
//!
//! A pid comparison also cannot distinguish *the proof restarted it* from *the
//! supervisor re-pinned it*, because the observable is the same. The remedy
//! has to be on cauldron's side: **the supervisor knows every restart it
//! performed, and until now it did not report it in a form a proof can read.**
//! This is that form.
//!
//! # Why it is not `restarts.jsonl`
//!
//! `restarts.jsonl` already exists and it is NOT this. It is written by each
//! SERVICE into its own store — `~/.cauldron/data/prod/threads/store/` has
//! 2,832 lines of it, `tts` 157 — and holds `boot` and `last_good` events with
//! the binary path, a code-signing identity and a pid. It is the service's own
//! self-update ledger and it is useful, but it cannot answer this question:
//!
//! * it has no notion of WHY, because the service does not know why it was
//!   started;
//! * it is written by the service, so a restart the supervisor performed that
//!   never reached a successful boot leaves no line at all — which is exactly
//!   the case a supervisor is for;
//! * it lives in the service's data directory, so it is the testimony of the
//!   party being asked about rather than of the party that acted;
//! * a service with no store writes none.
//!
//! So this is a second file on purpose, named differently on purpose, and it
//! is the SUPERVISOR's. One line per act of supervision, in the environment's
//! own data root, beside `seams.json` and `env.toml`.
//!
//! # What it keys on, and the boot token
//!
//! **Not the pid.** Pid liveness as evidence is the defect class this codebase
//! rejects — staleness is proven by a handshake carrying a boot token, never
//! by `kill -0` (see [`crate::seams::probe`]) — and a record keyed on a pid
//! inherits the same flaw. A pid is reused across reboots; a service was found
//! unsupervised for sixteen days because a pidfile's recorded pid had been
//! reused. And under `execv` the pid does not change across a real restart, so
//! a pid-keyed record could not even EXPRESS the restarts it exists to report.
//!
//! So an instance is identified by its **boot token** — [`crate::Seam`]'s
//! `boot_token`, minted per start by whoever starts the service, handed to it
//! in its environment and echoed back on `HELLO`. It is already this crate's
//! identity proof and it is already the thing [`crate::seams::probe`] refuses
//! `live` without.
//!
//! **The pid is recorded too, and the record says what it is for.** The token
//! is EMPTY for a hosted binary that cannot echo one — which today is
//! `threads` and `tts`, the two services that matter — so a token-only record
//! would carry nothing for them. The pid is therefore kept as a handle a human
//! can put into `ps`, and the ledger's own `boot` field is what a reader
//! compares. Neither is the evidence that a restart happened:
//!
//! **THE EVIDENCE IS THE RECORD ITSELF.** A reader does not observe the
//! process twice and infer a restart; it asks the only party that knows. That
//! inversion is the whole design, and it is what makes the honest assertion
//! writable: *no restart of this service that this proof caused*, rather than
//! *no restart at all*.
//!
//! # And an absent ledger is not an empty one
//!
//! [`Ledger::exists`] is carried separately from [`Ledger::records`] and every
//! reader must look at it. A file that is not there reads as zero records, and
//! "zero records" would then mean both *the supervisor restarted nothing* and
//! *no supervisor has ever written here* — a check that cannot run, reporting
//! as a result. The two are kept apart in the type, in the JSON, and in what
//! `cauldron supervisions` prints.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The file name, inside the environment's data root.
pub const FILE: &str = "supervisions.jsonl";

/// **What the supervisor did.** A closed set, because an act nobody can spell
/// is an act nobody records.
pub struct Act;
impl Act {
    /// It forked the service and saw it listening.
    pub const LAUNCH: &'static str = "launch";
    /// It signalled the service and the address went quiet.
    pub const STOP: &'static str = "stop";
    /// It rewrote this environment's `env.toml`. Not a restart by itself: it
    /// is what EXPLAINS the launch on the next pass, and recording it is what
    /// makes `auto_rollback` readable as a sequence rather than as a version
    /// that changed for no stated reason.
    pub const REPIN: &'static str = "repin";

    // ── THE SUPERVISOR'S OWN LIFE, WHICH IS NOT A SERVICE'S ──────────────
    //
    // THREE CAUSES SHARE ONE AFTERMATH, and without these two acts only one
    // of the three is in the record. `com.broomstick.cauldron.plist` runs
    // `cauldron daemon` under `KeepAlive = true, ThrottleInterval = 10`, and
    // the daemon EXITS ON PURPOSE when the image at its launch path changes
    // (`daemon.rs`, "drop execv") so launchd starts the new bytes. So after
    // any of these, `threads` is running and the supervisor is running:
    //
    //   1. launchd restarted the supervisor after a SELF-UPDATE;
    //   2. launchd restarted the supervisor after a CRASH, or at login;
    //   3. the supervisor re-pinned a SERVICE and restarted it.
    //
    // (3) was already a `stop`+`launch` pair carrying `Why::REPIN`. (1) and
    // (2) were INFERRABLE ONLY — from `by.since` changing between two service
    // records — and where no service act straddles the boundary they left no
    // trace at all. An inference that is unavailable in exactly the quiet
    // window a proof runs in is not evidence.
    //
    // So each incarnation records that it took the hold, and the OUTGOING one
    // records that it is leaving because its image changed. The outgoing
    // incarnation is the only party that can honestly state that reason: a
    // fresh process does not know why it was started, and a supervisor that
    // guessed would be the flattering-answer failure this crate refuses
    // everywhere else. Which makes the three readable apart:
    //
    //   `exit/self_update` then `hold/started`  -> (1)
    //   `hold/started` with NO preceding exit   -> (2)
    //   `stop/repin` + `launch/repin`           -> (3)
    //
    /// A new incarnation took this environment's hold. `service` is the
    /// supervisor's own name, and `boot` is the boot token it minted for
    /// itself — the same identity a service's row carries, for the same
    /// reason.
    pub const HOLD: &'static str = "hold";
    /// This incarnation is exiting so launchd can start different bytes.
    pub const EXIT: &'static str = "exit";

    // ── R5.S3: A SCHEDULED JOB, which is a pass and not a process ──────────
    /// The supervisor started one pass of a `kind = "job"`.
    pub const RUN: &'static str = "run";
    /// A pass ended. `detail` carries its exit code or the signal that ended
    /// it; a killed pass followed by a `run`/`retry` is a job's restart.
    pub const RAN: &'static str = "ran";
    /// R5.S6: the supervisor DECLINED to start a pass it would otherwise
    /// have started — a `run_at_load` job found on a holder's first look
    /// that already ran within its period. `why` says why; `detail` carries
    /// the record it decided on and when the next pass is due.
    pub const SKIP: &'static str = "skip";
}

/// **WHY it did it.** The field the service's own `restarts.jsonl` cannot
/// have, and the one the guard turns on.
pub struct Why;
impl Why {
    /// This environment's pin changed and the running version is not it.
    pub const REPIN: &'static str = "repin";
    /// It failed `down_after` consecutive liveness checks.
    pub const DOWN: &'static str = "down";
    /// Someone asked, on the supervisor seam or at the CLI. **This is the
    /// value a proof looks for to find a restart IT caused.**
    pub const REQUESTED: &'static str = "requested";
    /// It is no longer pinned here, and an unpin is how a deliberate stop is
    /// expressed.
    pub const UNPINNED: &'static str = "unpinned";
    /// `auto_rollback` re-pinned this environment to the previous good
    /// version after `max_start_failures` failed starts.
    pub const ROLLBACK: &'static str = "rollback";

    /// A supervisor incarnation started and took the hold. It does NOT say
    /// why launchd started it, because a fresh process cannot know: pair it
    /// with a preceding [`Why::SELF_UPDATE`] to read a self-update, and read
    /// its ABSENCE as a crash, a login or a first bootstrap.
    pub const STARTED: &'static str = "started";
    /// The image at this process's launch path is no longer the one this
    /// process is, so it is exiting for launchd to start the new bytes. Only
    /// the OUTGOING incarnation can state this.
    pub const SELF_UPDATE: &'static str = "self_update";
    /// R5.S3: a job's pass started because its interval came round (or at
    /// load, `run_at_load`); a `ran` carries this too.
    pub const SCHEDULE: &'static str = "schedule";
    /// R5.S3: a job's pass started early because the previous one FAILED and
    /// its declaration says `restart = "always"`.
    pub const RETRY: &'static str = "retry";
    /// R5.S6: a `skip` of a `run_at_load` pass, because the job's last
    /// recorded pass started less than one period ago (a holder restart is
    /// not a new load).
    pub const RECENT: &'static str = "recent";
    /// R5.S7: a `ran` that no holder OBSERVED. On a holder's first look the
    /// job's record said `running`, and the pass it names is gone (its pid is
    /// free, or now names a process that started after the pass did): the
    /// holder that started it exited mid-pass. Its outcome is unknown, so it
    /// is treated as unfinished and is not a recent run.
    pub const ORPHANED: &'static str = "orphaned";

    pub const ALL: &'static [&'static str] = &[
        Self::REPIN,
        Self::DOWN,
        Self::REQUESTED,
        Self::UNPINNED,
        Self::ROLLBACK,
        Self::STARTED,
        Self::SELF_UPDATE,
        Self::SCHEDULE,
        Self::RETRY,
        Self::RECENT,
        Self::ORPHANED,
    ];
}

/// **Who acted**, carried on every line so the record survives the
/// supervisor's own restart and still says which incarnation did what.
///
/// `root` is the ownership field and it is the one a proof scopes on. It is
/// the cauldron root this supervisor serves, so a record written by a daemon
/// in a proof sandbox carries that sandbox and the operator's carries
/// `/Users/testuser/.cauldron`. It is the same discrimination
/// `prove-warnings`' section 6 makes on `CAULDRON_HOME`, and for the same
/// reason: a process name confuses one root with another and a path does not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct By {
    pub pid: u32,
    /// The acting process's own start, or empty for a one-shot CLI that has no
    /// incarnation to name. Two records with the same `pid` and different
    /// `since` are two different processes, which is what a reused pid looks
    /// like from here.
    #[serde(default)]
    pub since: String,
    #[serde(default)]
    pub commit: String,
    pub root: String,
}

/// One act of supervision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Supervision {
    /// RFC3339, UTC, microseconds. Sortable and comparable as a STRING, which
    /// is what makes a window query a string comparison and not a date
    /// library.
    pub at: String,
    pub env: String,
    pub service: String,
    /// One of [`Act`]'s words.
    pub act: String,
    /// One of [`Why`]'s words.
    pub why: String,
    /// The version the act is ABOUT: launched onto, stopped off, or re-pinned
    /// to. Empty only where it is honestly unknown.
    #[serde(default)]
    pub version: String,
    /// The version it was on BEFORE, where the acting code knows it. Empty is
    /// "not known here", never "the same".
    #[serde(default)]
    pub was_version: String,
    /// **The instance identity.** [`crate::Seam::boot_token`]. Empty for a
    /// hosted binary that cannot echo one — recorded empty rather than
    /// omitted, because that emptiness is itself the fact that no stronger
    /// identity is available for this service.
    #[serde(default)]
    pub boot: String,
    /// A handle for `ps`, explicitly NOT the identity. See this module's head.
    pub pid: Option<u32>,
    pub by: By,
    #[serde(default)]
    pub detail: String,
}

/// Where the ledger for this environment lives.
pub fn path(data_root: &Path) -> PathBuf {
    data_root.join(FILE)
}

/// **RFC3339 UTC with microseconds, with no date dependency.**
///
/// Hand-rolled from the epoch because this crate has no chrono and a record
/// whose timestamp format drifts is a record a window query silently misses.
/// Civil-time conversion by the standard days-from-civil inverse; leap seconds
/// are not represented, which is true of the clock this reads as well.
pub fn now_rfc3339() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() as i64;
    let micros = d.subsec_micros();
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    // days-from-civil, inverted (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dd = doy - (153 * mp + 2) / 5 + 1;
    let mm = if mp < 10 { mp + 3 } else { mp - 9 };
    let yy = if mm <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}+00:00",
        yy,
        mm,
        dd,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        micros
    )
}

/// **Append one line.** `O_APPEND` and one `write_all`, so two processes
/// appending cannot interleave a line: a single `write` of less than
/// `PIPE_BUF` to an append-mode file is atomic, and that is the only mutual
/// exclusion this file needs because nothing ever rewrites it.
///
/// It is append-only on purpose. A ledger that is compacted, rotated or
/// rewritten by the party it is testimony about is not testimony.
pub fn append(data_root: &Path, s: &Supervision) -> Result<()> {
    let p = path(data_root);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut line = serde_json::to_string(s)?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .with_context(|| format!("opening {} to append", p.display()))?;
    f.write_all(line.as_bytes()).with_context(|| format!("appending to {}", p.display()))?;
    f.flush()?;
    Ok(())
}

/// What a read returns. **`exists` is not derivable from `records`**, which is
/// the whole reason this is a struct.
#[derive(Debug, Clone, Serialize)]
pub struct Ledger {
    pub ledger: String,
    /// **Is there a ledger at all?** `false` with an empty `records` means
    /// NOTHING IS KNOWN, not that nothing happened. A caller that treats the
    /// two the same has written a check that cannot fail.
    pub exists: bool,
    /// Lines that were present and did not parse. Counted rather than
    /// discarded silently: a parse failure is the one way this file could
    /// under-report, so the number is carried where a reader sees it.
    pub unreadable: usize,
    pub records: Vec<Supervision>,
}

/// Narrow a read. Every field is optional and an absent field filters nothing.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Inclusive lower bound on `at`, as an RFC3339 string. Compared
    /// lexically, which is exact for the fixed-width UTC format
    /// [`now_rfc3339`] writes.
    pub since: Option<String>,
    pub service: Option<String>,
    pub why: Option<String>,
    /// Only records written by a supervisor serving this cauldron root. The
    /// ownership lens.
    pub root: Option<String>,
}

/// **Read the ledger.** A missing file is `exists: false`, never an error: an
/// environment no supervisor has ever acted in is a normal state and has no
/// ledger. Anything else that goes wrong IS an error, because a ledger that
/// cannot be read must not be reported as a ledger with nothing in it.
pub fn read(data_root: &Path, f: &Filter) -> Result<Ledger> {
    let p = path(data_root);
    if !p.exists() {
        return Ok(Ledger {
            ledger: p.display().to_string(),
            exists: false,
            unreadable: 0,
            records: Vec::new(),
        });
    }
    let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    let mut records = Vec::new();
    let mut unreadable = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Supervision>(line) {
            Ok(s) => {
                if let Some(w) = &f.since {
                    if s.at.as_str() < w.as_str() {
                        continue;
                    }
                }
                if let Some(x) = &f.service {
                    if &s.service != x {
                        continue;
                    }
                }
                if let Some(x) = &f.why {
                    if &s.why != x {
                        continue;
                    }
                }
                if let Some(x) = &f.root {
                    if &s.by.root != x {
                        continue;
                    }
                }
                records.push(s);
            }
            Err(_) => unreadable += 1,
        }
    }
    Ok(Ledger { ledger: p.display().to_string(), exists: true, unreadable, records })
}

// ════════════════════════════════════════════════════ WRITING ONE, BRIEFLY ══

/// **This process's incarnation**, set once at start by whoever has one.
///
/// It is a property of the PROCESS and not of the call, so it is held here
/// rather than threaded through every signature that might eventually record
/// something. The daemon sets it to its own `started_at`; a one-shot CLI never
/// sets it and its records carry an empty `since`, which is the honest answer
/// for a process with no incarnation to name.
static INCARNATION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Call once, early. A second call is ignored: an incarnation that changed
/// mid-process would make two records claim to be from different processes.
pub fn set_incarnation(since: &str) {
    let _ = INCARNATION.set(since.to_string());
}

/// A record about `service`, with the acting party already filled in. The
/// caller sets `version`, `was_version`, `boot`, `pid` and `detail` — every
/// one of which is honestly empty by default rather than guessed.
pub fn note(
    env: &crate::Environment,
    commit: &str,
    service: &str,
    act: &str,
    why: &str,
) -> Supervision {
    Supervision {
        at: now_rfc3339(),
        env: env.name.clone(),
        service: service.to_string(),
        act: act.to_string(),
        why: why.to_string(),
        version: String::new(),
        was_version: String::new(),
        boot: String::new(),
        pid: None,
        by: By {
            pid: std::process::id(),
            since: INCARNATION.get().cloned().unwrap_or_default(),
            commit: commit.to_string(),
            root: env.root.display().to_string(),
        },
        detail: String::new(),
    }
}

/// **The sentence to print when the ledger cannot be written.**
///
/// The act is never abandoned because the record failed — a supervisor that
/// refuses to restart a service because it cannot write a log file has
/// converted a logging fault into an outage. But it is never silent either:
/// an unrecorded restart is exactly the state this whole module exists to end,
/// so the failure is reported in the words that say what is now unprovable.
pub fn write_failure(service: &str, act: &str, data_root: &Path, e: &anyhow::Error) -> String {
    format!(
        "the `{act}` of `{service}` WAS PERFORMED but could not be recorded in {} ({e:#}). The \
         act stands; what is lost is the evidence of it, so for this window the supervisor \
         cannot prove what it did to `{service}` and a guard reading this ledger will see an \
         act it cannot account for.",
        path(data_root).display()
    )
}
