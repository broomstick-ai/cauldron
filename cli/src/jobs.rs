//! **SCHEDULED JOBS, run by the daemon that holds the environment.** (R5.S3)
//!
//! H179 ruling 1 says cauldron hosts ALL services and restarts any that are
//! down. `soup-backup` is a service that is not a daemon: launchd runs one
//! short pass every 300 s (a per-user launchd job, `StartInterval 300`,
//! `RunAtLoad`, `KeepAlive false`). Until this module a `kind = "job"` could
//! only be REGISTERED, because cauldron had no scheduler.
//!
//! A job is pinned like any service — presence is the pin — and the daemon's
//! pass calls [`tend`] for it instead of the liveness loop. Each pass of the
//! job is forked into its own process group with the same environment a
//! hosted daemon gets (`CAULDRON_*` plus the declaration's `[env]`, expanded),
//! and every start and every end is written to `supervisions` (`run` /
//! `ran`). The last outcome is kept in `data/<env>/jobs.json`, which is what
//! `status` reads.
//!
//! **Restart, for a job.** A pass that FAILS — non-zero, killed by a signal,
//! or past `timeout_secs` — is re-run after `retry_secs` when the
//! declaration says `restart = "always"` and the environment's
//! `auto_restart` is on; otherwise it waits for its next interval. Killing a
//! running pass is therefore how a job's restart is exercised, and the
//! record of it is a `ran` naming the signal followed by a `run` whose `why`
//! is `retry`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use env::decl::{Declaration, Manifest, Supervisor as Policy};
use env::hosting::Restart;
use env::supervisions::{self, Act, Why};
use env::Environment;
use serde::{Deserialize, Serialize};

/// The file `status` reads, inside the environment's data root.
pub(crate) const FILE: &str = "jobs.json";

/// What the daemon keeps in memory per job.
struct Live {
    child: Option<std::process::Child>,
    started: Option<Instant>,
    /// When the next pass is due (R5.S4b: wall-clock aware, see [`Sched`]).
    sched: Sched,
}

// ═══════════════════════════════════ WHEN A PASS IS DUE (R5.S4b) ════
//
// **THE DEFECT THIS REPLACES (Deploy4-3).** `next_due` was a
// `std::time::Instant` fixed at the holder's first `tend`. On macOS `Instant`
// is CLOCK_UPTIME_RAW, which STOPS while the machine sleeps, so an
// `at = "09:30"` job armed at 22:36 for "10h54m from now" fired at 09:30 PLUS
// however long the Mac slept. launchd's `StartCalendarInterval` fires on wake.
//
// **THE SEMANTICS NOW.**
// - A CALENDAR job (`at = "HH:MM"`) has SLOTS: every local day at HH:MM. Each
//   poll reads the WALL clock and the local time of day afresh, computes the
//   most recent slot at or before now, and is due iff that slot is newer than
//   the last slot a pass was started for. So a slot missed while asleep runs
//   ONCE on wake (however many days were slept: only the most recent slot is
//   ever considered — launchd's coalescing), and a clock stepped BACKWARD
//   never re-runs a slot (the most recent slot moves back, not forward). The
//   last slot is persisted in `jobs.json` (`last_slot`), so a holder restart
//   neither loses a slot that came due while it was down (it runs once on the
//   next poll) nor doubles one already started (it is recorded at spawn).
//   "Newer" means more than HALF A DAY newer: slots are 24 h ± 1 h (DST)
//   apart, so the margin never swallows a real slot (see [`SLOT_MARGIN`]).
//   JOBS-DST: a slot is a LOCAL DATE at HH:MM resolved to an instant
//   ([`resolve_local`]); an ambiguous HH:MM (fall-back) is its FIRST
//   occurrence, a nonexistent one (spring-forward) is moved forward by the
//   gap. Wall-clock seconds are never added to or taken from a local time.
// - An INTERVAL job (`every_secs`) and every RETRY wait on a [`Deadline`]:
//   due when EITHER the monotonic clock OR the wall clock says the wait has
//   passed. Sleep advances the wall clock, so a sleep no longer delays it; a
//   backward wall step leaves the monotonic half to fire it on time, so it
//   never runs early or twice because of one. (Decision recorded in the
//   ledger as R5.S4b-1: the brief allowed interval jobs to stay monotonic;
//   either-clock costs nothing and makes a long interval sleep-proof too.)

/// The clock readings a schedule depends on. Injectable so a test can move
/// the wall clock while the monotonic one stands still — exactly what a
/// macOS sleep looks like from inside the process — and can fix the LOCAL
/// ZONE, so no slot test depends on the machine's `TZ` (JOBS-DST).
pub(crate) trait Clock {
    /// Monotonic, paused in sleep on macOS (CLOCK_UPTIME_RAW).
    fn mono(&self) -> Instant;
    /// Wall clock, unix seconds. Advances through sleep; may step either way.
    fn wall(&self) -> u64;
    /// **JOBS-DST.** The local zone's offset from UTC, seconds EAST, in force
    /// AT wall time `t` (unix seconds) — so a DST change between now and `t`
    /// is seen. `None` if the local zone cannot be read. Every local-time
    /// answer the scheduler needs is derived from this one reading (see
    /// [`resolve_local`]); nothing adds wall-clock seconds to a local time.
    fn offset_at(&self, t: u64) -> Option<i64>;
}

/// The real clocks.
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn mono(&self) -> Instant {
        Instant::now()
    }
    fn wall(&self) -> u64 {
        test_now().unwrap_or_else(epoch)
    }
    fn offset_at(&self, t: u64) -> Option<i64> {
        local_offset_at(t)
    }
}

/// **JOBS-DST: THE INJECTED "NOW" OF AN ISOLATED PROCESS.** An isolated
/// `cauldron` (the test-only marker, or a cargo test harness —
/// [`env::egress::process_isolation`]) reads its scheduling wall clock from
/// `CAULDRON_TEST_NOW` (unix seconds) when that is set, so an integration
/// test can ask `status` for a calendar job's slot at a fixed instant —
/// across a DST change — instead of racing the real clock. His own process
/// is never isolated, so it never reads the variable. The local zone is
/// injected the ordinary way: `TZ`, which [`local_offset_at`] honours.
fn test_now() -> Option<u64> {
    static NOW: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *NOW.get_or_init(|| {
        env::egress::process_isolation()?;
        std::env::var("CAULDRON_TEST_NOW").ok()?.trim().parse().ok()
    })
}

/// The machine's local zone offset at `t` (seconds east of UTC), through
/// chrono's `Local` — the zone `TZ` names, else `/etc/localtime` — so no
/// `/bin/date` is spawned per reading and no `unsafe` `localtime_r` is needed.
fn local_offset_at(t: u64) -> Option<i64> {
    use chrono::{Offset, TimeZone};
    let utc = chrono::DateTime::from_timestamp(i64::try_from(t).ok()?, 0)?.naive_utc();
    Some(i64::from(chrono::Local.offset_from_utc_datetime(&utc).fix().local_minus_utc()))
}

/// Due when EITHER clock reaches its half.
#[derive(Debug, Clone, Copy)]
struct Deadline {
    mono: Instant,
    wall: u64,
}

impl Deadline {
    fn after(c: &dyn Clock, secs: u64) -> Self {
        Deadline { mono: c.mono() + Duration::from_secs(secs), wall: c.wall().saturating_add(secs) }
    }
    fn passed(&self, c: &dyn Clock) -> bool {
        c.mono() >= self.mono || c.wall() >= self.wall
    }
}

#[derive(Debug, Clone, Copy)]
enum Next {
    /// Never scheduled in this holder's life (or reset by a refusal).
    Unset,
    /// Due at a deadline (an interval job, or any retry).
    At(Deadline),
    /// A calendar job between passes: due when a newer slot arrives.
    Slot,
}

/// Half a day. Two distinct calendar slots are 23 h to 25 h apart (a DST
/// day is 23 or 25 h long), so "newer by more than this" never swallows a
/// real slot. Since JOBS-DST the slots themselves are exact; the margin
/// still absorbs (a) a `last_slot` persisted by the pre-JOBS-DST arithmetic,
/// up to an hour off, and (b) the machine's ZONE changing under a running
/// holder (the offset moves with the wall clock unchanged), which would
/// otherwise make the slot just run look an hour newer.
const SLOT_MARGIN: u64 = 43_200;

const DAY: i64 = 86_400;

/// The LOCAL calendar date (days since 1970-01-01, local) at wall time `t`.
fn local_day(c: &dyn Clock, t: u64) -> Option<i64> {
    Some((i64::try_from(t).ok()? + c.offset_at(t)?).div_euclid(DAY))
}

/// **JOBS-DST: THE ONE PLACE A LOCAL TIME BECOMES AN INSTANT.** Local date
/// `day` (days since 1970-01-01) at `target` seconds after local midnight,
/// as unix seconds, under the zone rules in force then.
///
/// The offsets a day either side are the only candidates (no zone changes
/// offset twice within two days); a candidate is real when the zone, read at
/// the instant it names, agrees with the offset it was built from.
///
/// **POLICY** (RFC 5545 §3.3.5, and what POSIX `mktime` with `tm_isdst = -1`,
/// Python's `fold=0` and Temporal's `"compatible"` all do):
/// - **AMBIGUOUS** (the hour a fall-back repeats, 01:xx on 2026-11-01 in
///   America/Los_Angeles): the FIRST occurrence (01:30 PDT, not 01:30 PST).
///   A slot runs once that day; the repeat of the hour is not a new slot.
/// - **NONEXISTENT** (the hour a spring-forward skips, 02:xx on 2026-03-08):
///   read through the offset in force BEFORE the gap, i.e. moved forward by
///   the gap's length (02:30 runs at 03:30 PDT). The day's slot is never
///   skipped, and it lands after every earlier slot.
fn resolve_local(c: &dyn Clock, day: i64, target: u64) -> Option<u64> {
    let naive = day.checked_mul(DAY)?.checked_add(i64::try_from(target).ok()?)?;
    let at = |off: i64| u64::try_from(naive - off).ok();
    // A day either side of every candidate (offsets are within ±14 h).
    let before = c.offset_at(u64::try_from(naive - DAY).ok()?)?;
    let after = c.offset_at(u64::try_from(naive + DAY).ok()?)?;
    let real = |off: i64| at(off).filter(|&t| c.offset_at(t) == Some(off));
    match (real(before), real(after)) {
        (Some(a), Some(b)) => Some(a.min(b)), // ambiguous (or no change): the first
        (Some(t), None) | (None, Some(t)) => Some(t),
        (None, None) => at(before), // nonexistent: shifted forward by the gap
    }
}

/// The calendar slot (`target` seconds after local midnight) at or before
/// wall time `t`, with the local date it belongs to.
fn slot_at_or_before(c: &dyn Clock, target: u64, t: u64) -> Option<(i64, u64)> {
    let today = local_day(c, t)?;
    // Today's slot, else yesterday's. (Two days back only if a gap pushed
    // yesterday's late slot past midnight and past `t`.)
    for day in [today, today - 1, today - 2] {
        let s = resolve_local(c, day, target)?;
        if s <= t {
            return Some((day, s));
        }
    }
    None
}

/// The most recent calendar slot (`target` seconds after local midnight) at
/// or before now, as unix seconds. `None` if local time cannot be read.
///
/// JOBS-DST: computed in LOCAL CALENDAR terms — today's local date, at
/// `target`, resolved to an instant ([`resolve_local`]) — never by
/// subtracting wall-clock seconds from now. The old arithmetic (now minus
/// the local time since the slot) assumed the last 24 h held 86 400 s, and
/// so put yesterday's slot an hour off from 01:00 PST to 22:59 PST on a
/// fall-back day, and from 03:00 PDT on a spring-forward day to 00:59 the
/// next.
fn most_recent_slot(c: &dyn Clock, target: u64) -> Option<u64> {
    slot_at_or_before(c, target, c.wall()).map(|(_, s)| s)
}

/// **WHEN THE NEXT CALENDAR PASS IS DUE (R5.S5).** The ONE function both
/// [`Sched::due`] ("is a pass due now?") and `status` ("when is the next
/// one?") read, so the time `status` prints cannot drift from the time the
/// holder acts on (Deploy5-2: it had to be derived from this file by hand).
///
/// Unix seconds of the slot the next pass is due at. It is `<= now` exactly
/// when a pass is due now — that slot is `most_recent_slot`, newer than
/// `last` by more than [`SLOT_MARGIN`] — and otherwise it is the first later
/// slot that will be. `last` is the slot the last pass was started for;
/// `None` means armed with nothing recorded, so the slot already passed is
/// not owed (as in `due`). `None` back: local time unreadable.
///
/// JOBS-DST: each later slot is its own local date at `target`, resolved
/// through the zone rules in force on that date, so a slot past a DST change
/// is exactly where the holder will see it come due (the old version placed
/// it 24 h on and corrected it with a `/bin/date` per candidate).
fn next_slot(c: &dyn Clock, target: u64, last: Option<u64>) -> Option<u64> {
    let (day, slot) = slot_at_or_before(c, target, c.wall())?;
    let floor = last.unwrap_or(slot).saturating_add(SLOT_MARGIN);
    if slot > floor {
        return Some(slot);
    }
    // Bounded: a `last` years ahead (a clock stepped far back) ends in `None`.
    for k in 1..=800 {
        let t = resolve_local(c, day + k, target)?;
        if t > floor {
            return Some(t);
        }
    }
    None
}

/// The slot a calendar job's last pass is taken to have covered, from the
/// persisted record, when a holder first looks (and when `status` asks):
/// the recorded `last_slot`; else a legacy record's `last_start` read back
/// to its slot; else — never run — the slot already passed (not owed).
fn seed_last_slot(c: &dyn Clock, target: u64, prev: Option<&Record>) -> Option<u64> {
    prev.and_then(|r| r.last_slot)
        .or_else(|| prev.and_then(|r| r.last_start).and_then(|t| legacy_slot(c, target, t)))
        .or_else(|| most_recent_slot(c, target))
}

/// The slot at or before an earlier wall time `t` (a legacy record's
/// `last_start`), in local calendar terms at `t`. A `t` in the future is not
/// trusted: the most recent slot stands in.
fn legacy_slot(c: &dyn Clock, target: u64, t: u64) -> Option<u64> {
    if t > c.wall() {
        return most_recent_slot(c, target);
    }
    slot_at_or_before(c, target, t).map(|(_, s)| s)
}

/// One job's schedule state: what is due next, and (calendar jobs) the slot
/// the last pass was started for.
#[derive(Debug, Clone)]
pub(crate) struct Sched {
    next: Next,
    /// Unix seconds of the calendar slot the last pass was STARTED for.
    last_slot: Option<u64>,
    /// Why the next run will start (`schedule` or `retry`).
    why: &'static str,
    /// R5.S6: set by the first look when a `run_at_load` pass was NOT
    /// started because the job ran within its period; `tend` takes it and
    /// records the `skip`.
    load_skip: Option<String>,
    /// R5.S7: set by the first look when the record says `running` but the
    /// pass it names is GONE (the holder that started it died mid-pass, and
    /// so did the pass, or its pid now names another process); `tend` takes
    /// it and records the `ran` / `orphaned` that no holder saw.
    load_orphan: Option<String>,
    /// R5.S7: how the first look asks whether a recorded pass is still the
    /// process it was. [`probe_pass`] in the daemon; injectable for tests.
    probe: fn(u32, u64) -> PassLife,
    /// R5.S7 inspector F1: the `(pid, last_start)` of a pass already found
    /// orphaned. If the re-run then fails to SPAWN, `next` stays `Unset` and
    /// the next poll looks at the same record again; this stops that look
    /// probing, and recording the orphan, once per poll.
    orphan_seen: Option<(u32, u64)>,
}

/// **R5.S7: IS THE PASS A `running` RECORD NAMES STILL RUNNING?** The answer
/// of [`probe_pass`], asked of `(pid, last_start)` from `jobs.json`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PassLife {
    /// That pid is alive AND it is the process that was started at
    /// `last_start` (its own start time agrees). The detail says what was seen.
    Alive(String),
    /// No process has that pid, or the one that has it started after the
    /// pass did (a RECYCLED pid). The detail says which.
    Gone(String),
    /// The question was not answered: `ps` did not run, printed something
    /// unparseable, or printed nothing without exiting 1 (or `kill -0` still
    /// reaches the pid, or `/bin/kill` could not be run). Treated as alive: a pass that might be running is
    /// not run twice.
    Unknown(String),
}

/// A process started after the recorded `last_start` by more than this is
/// not the pass the record names. The pass's own start is BEFORE
/// `last_start` (the holder reads the wall clock after `spawn` returns), and
/// `ps`'s elapsed time is whole seconds read a moment after ours, so the same
/// process reads at most ~1 s later than `last_start`; a pid freed by the
/// pass's death and handed out again within 2 s of its START would have to
/// wrap the whole pid space in that time.
const SAME_PASS_SLACK_SECS: u64 = 2;

/// Parse `ps -o etime=`: `[[dd-]hh:]mm:ss`.
fn parse_etime(t: &str) -> Option<u64> {
    let t = t.trim();
    let (days, rest) = match t.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, t),
    };
    let mut secs = 0u64;
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    for p in parts {
        secs = secs * 60 + p.parse::<u64>().ok()?;
    }
    Some(days * 86_400 + secs)
}

/// **R5.S7: the identity check.** `kill -0` alone answers "is SOME process
/// using this pid", which after the holder that recorded it died is not the
/// question: pids are recycled. So the process holding `pid` must also have
/// STARTED no later than the pass did — its start, `now - etime`, is
/// compared with the record's `last_start` (the same idea as SB-1's lock,
/// which compares `ps -o lstart=`; elapsed seconds avoid parsing a date).
pub(crate) fn probe_pass(pid: u32, last_start: u64) -> PassLife {
    probe_pass_with("/bin/ps", pid, last_start)
}

/// [`probe_pass`] with the `ps` binary named, so a test can hand it one that
/// fails (R5.S7 inspector F3).
fn probe_pass_with(ps: &str, pid: u32, last_start: u64) -> PassLife {
    // `now` is read BEFORE `ps` samples its own clock (inspector F2): any delay
    // between the two then makes `etime` larger and `started` EARLIER — the
    // safe direction (a live pass can only read as the pass), never later.
    let now = epoch();
    let out = match Command::new(ps)
        .args(["-o", "etime=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(e) => return PassLife::Unknown(format!("`{ps}` could not be run ({e})")),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        // `ps` prints nothing both for "no such pid" (exit 1) and when it
        // failed outright; only the first is an answer. Confirmed with
        // `kill -0` (inspector F3): a pid that still answers is NOT read as
        // gone on the strength of a question `ps` did not answer.
        let answers = Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success());
        return match (out.status.code(), answers) {
            (Some(1), Ok(false)) => PassLife::Gone(format!("no process has pid {pid}")),
            (code, answers) => PassLife::Unknown(format!(
                "`{ps}` printed nothing for pid {pid} (exit {code:?}) and `kill -0` says {answers:?}"
            )),
        };
    }
    let Some(etime) = parse_etime(&text) else {
        return PassLife::Unknown(format!("`ps -o etime=` printed `{}` for pid {pid}", text.trim()));
    };
    let started = now.saturating_sub(etime);
    if started > last_start + SAME_PASS_SLACK_SECS {
        PassLife::Gone(format!(
            "pid {pid} is alive but is ANOTHER process: it started at {started}, {}s after the pass did \
             ({last_start}) — a recycled pid",
            started - last_start
        ))
    } else {
        PassLife::Alive(format!("pid {pid} is alive and started at {started}, as the pass did ({last_start})"))
    }
}

impl Sched {
    pub(crate) fn new() -> Self {
        Sched {
            next: Next::Unset,
            last_slot: None,
            why: Why::SCHEDULE,
            load_skip: None,
            load_orphan: None,
            probe: probe_pass,
            orphan_seen: None,
        }
    }

    /// **R5.S6 (H179-1, Deploy5-4): IS THE LOAD RUN OWED?** `run_at_load`
    /// means "run when this job is first found pinned, or when it is
    /// overdue" — NOT "run on every holder start". Deploy 5 measured the
    /// difference: the new holder's first look ran soup-backup 14 s after the
    /// old holder's pass, and with F-J1 holders restart on every placed image.
    /// So an INTERVAL job whose persisted record (`jobs.json`) says its last
    /// pass STARTED less than `every_secs` ago is not run at load; it resumes
    /// from `last_start + every_secs`, exactly as a `run_at_load = false`
    /// job does. Returns the reason when it is skipped, `None` when it runs.
    ///
    /// It runs (never skipped) when: there is no record or no `last_start`
    /// (never run — a first pin, or only ever blocked/refused); the last pass
    /// FAILED (the old holder owed it a retry, which this holder cannot see
    /// the deadline of); the recorded start is in the future (a record the
    /// wall clock contradicts is not trusted); or it is a CALENDAR job (its
    /// slots are tracked by `last_slot`; unchanged here).
    ///
    /// **R5.S7 (R5.S6-1 c): a recent `running` record is ASKED, not
    /// trusted.** `running` on a holder's first look means the holder that
    /// started the pass exited before it ended. If the record carries the
    /// pass's pid, [`Sched::probe`] asks whether that pid is still the pass
    /// (alive AND started no later than `last_start`, so a recycled pid is
    /// not mistaken for it): still running → skipped, as before, because a
    /// second concurrent pass is worse than a deferral; GONE → the pass is
    /// UNFINISHED, its outcome unseen by any holder, so it is treated as a
    /// failed pass (run at load) and `self.load_orphan` carries the reason
    /// for the `ran` / `orphaned` row. A `running` record with NO pid (a
    /// format older than any holder that writes one) cannot be asked and
    /// keeps the R5.S6 behaviour: skipped.
    fn load_skip_reason(&mut self, s: &env::hosting::Schedule, c: &dyn Clock, prev: Option<&Record>) -> Option<String> {
        if Self::target(s).is_some() {
            return None;
        }
        let r = prev?;
        if r.state == "failed" {
            return None;
        }
        let last = r.last_start?;
        let now = c.wall();
        if last > now {
            return None;
        }
        let age = now - last;
        if age >= s.every_secs {
            return None;
        }
        let mut seen = String::new();
        if r.state == "running" {
            if r.pid.is_some_and(|pid| self.orphan_seen == Some((pid, last))) {
                return None; // already found orphaned and recorded: still owed a run
            }
            match r.pid.map(|pid| (pid, (self.probe)(pid, last))) {
                Some((pid, PassLife::Gone(why))) => {
                    self.orphan_seen = Some((pid, last));
                    self.load_orphan = Some(format!(
                        "the pass started {age}s ago (pid {pid}, jobs.json last_start {last}) is recorded \
                         `running`, but {why}. The holder that started it exited before it ended and no \
                         holder saw it end, so its outcome is unknown: it is treated as UNFINISHED, like a \
                         failed pass, and is not a recent run"
                    ));
                    return None;
                }
                Some((_, PassLife::Alive(d))) => seen = format!("; still running: {d}"),
                Some((_, PassLife::Unknown(d))) => {
                    seen = format!("; whether it is still running could not be checked ({d}), so it is taken to be")
                }
                None => seen = "; the record names no pid, so whether that pass is still running cannot be \
                                checked (a record older than R5.S7)"
                    .into(),
            }
        }
        Some(format!(
            "run_at_load pass NOT started: the last pass started {age}s ago (jobs.json last_start {last}, \
             state `{}`{seen}), less than its every_secs = {}. A holder restart is not a new load; the next \
             pass is due in {}s, {} s after that start",
            r.state,
            s.every_secs,
            s.every_secs - age,
            s.every_secs
        ))
    }

    fn target(s: &env::hosting::Schedule) -> Option<u64> {
        s.at.as_deref().and_then(|a| env::hosting::parse_at(a).ok())
    }

    /// **IS A PASS DUE NOW?** Called every poll with nothing in flight.
    /// `prev` is the persisted record (read once, at the holder's first look).
    pub(crate) fn due(&mut self, s: &env::hosting::Schedule, c: &dyn Clock, prev: Option<&Record>) -> bool {
        if let Next::Unset = self.next {
            let skip = if s.run_at_load { self.load_skip_reason(s, c, prev) } else { None };
            if s.run_at_load && skip.is_none() {
                // Seed the slot from the record so a load run does not count
                // as covering a slot it did not (the start re-records it).
                self.last_slot = self.last_slot.or(prev.and_then(|r| r.last_slot));
                self.why = Why::SCHEDULE;
                return true;
            }
            // A skipped load run falls through to the interval resume below
            // (`last_start + every_secs`), which is what makes the skip a
            // deferral and never a loss.
            self.load_skip = skip;
            match Self::target(s) {
                Some(target) => {
                    // Never run before: the slot that has already passed is
                    // not owed (launchd does not run a calendar job at load
                    // either); the next one is.
                    self.last_slot = seed_last_slot(c, target, prev);
                    self.next = Next::Slot;
                }
                None => {
                    // An interval job resumes from its last start, so a holder
                    // that was down past a due pass runs it once, now.
                    let wait = match prev.and_then(|r| r.last_start) {
                        Some(t) => t.saturating_add(s.every_secs).saturating_sub(c.wall()),
                        None => s.every_secs,
                    };
                    self.next = Next::At(Deadline::after(c, wait));
                }
            }
        }
        match self.next {
            Next::Unset => false,
            Next::At(d) => d.passed(c),
            Next::Slot => {
                let Some(target) = Self::target(s) else { return false };
                if self.last_slot.is_none() {
                    // Local time was unreadable when this was armed; the slot
                    // that has passed is not owed, the next one is. (Still
                    // unreadable: re-checked next poll.)
                    self.last_slot = most_recent_slot(c, target);
                    return false;
                }
                // R5.S5: due iff the next slot is not in the future — the
                // same function `status` prints the next slot from.
                next_slot(c, target, self.last_slot).is_some_and(|t| t <= c.wall())
            }
        }
    }

    /// A pass was started now: a calendar job records the slot it covers.
    pub(crate) fn started(&mut self, s: &env::hosting::Schedule, c: &dyn Clock) {
        if let Some(target) = Self::target(s) {
            if let Some(slot) = most_recent_slot(c, target) {
                self.last_slot = Some(self.last_slot.map_or(slot, |l| l.max(slot)));
            }
        }
        self.next = Next::Slot; // placeholder while in flight; `ended` sets it
    }

    /// A pass ended; returns the seconds until the next one (for the record).
    pub(crate) fn ended(&mut self, s: &env::hosting::Schedule, c: &dyn Clock, retry: bool, elapsed_secs: u64) -> u64 {
        if retry {
            self.next = Next::At(Deadline::after(c, s.retry_secs));
            self.why = Why::RETRY;
            return s.retry_secs;
        }
        self.why = Why::SCHEDULE;
        match Self::target(s) {
            Some(target) => {
                self.next = Next::Slot;
                let now = c.wall();
                next_slot(c, target, None).map_or(3600, |t| t.saturating_sub(now).max(1))
            }
            None => {
                let wait = s.every_secs.saturating_sub(elapsed_secs).max(1);
                self.next = Next::At(Deadline::after(c, wait));
                wait
            }
        }
    }

    /// Forget the schedule (a refused job is re-armed from scratch).
    pub(crate) fn reset(&mut self) {
        self.next = Next::Unset;
    }
}

static JOBS: Mutex<BTreeMap<String, Live>> = Mutex::new(BTreeMap::new());

/// One job's last known state, as `status` shows it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Record {
    /// `running` | `ok` | `failed` | `blocked` | `refused` | `waiting`
    pub state: String,
    pub version: String,
    pub pid: Option<u32>,
    /// Unix seconds.
    pub last_start: Option<u64>,
    pub last_end: Option<u64>,
    pub last_rc: Option<i32>,
    pub last_signal: Option<i32>,
    /// Seconds from `last_end` (or now) until the next run.
    pub next_in_secs: Option<u64>,
    pub detail: String,
    /// R5.S4b: unix seconds of the calendar slot (`at = "HH:MM"`) the last
    /// pass was STARTED for. Kept across a holder restart so a due slot is
    /// neither lost nor run twice. `None` for an interval job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_slot: Option<u64>,
}

fn epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn path(env: &Environment) -> PathBuf {
    env.data_root.join(FILE)
}

pub(crate) fn load(env: &Environment) -> BTreeMap<String, Record> {
    std::fs::read_to_string(path(env))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Put `r` in `all`, carrying the previous record's `last_slot` forward: the
/// last slot outlives every state change (`ran`, `blocked`, `refused`); only
/// a START moves it (R5.S4b).
fn merge_one(all: &mut BTreeMap<String, Record>, name: &str, mut r: Record) {
    if r.last_slot.is_none() {
        r.last_slot = all.get(name).and_then(|p| p.last_slot);
    }
    all.insert(name.to_string(), r);
}

fn save_one(env: &Environment, name: &str, r: Record) {
    let mut all = load(env);
    merge_one(&mut all, name, r);
    if let Ok(mut t) = serde_json::to_string_pretty(&all) {
        t.push('\n');
        let p = path(env);
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// The declaration the PINNED build of `name` runs under, if it resolves.
pub(crate) fn pinned_decl(env: &Environment, m: &Manifest, name: &str) -> Option<crate::Launchable> {
    crate::resolve_launch(env, m, name).ok()
}

/// Is `name`, as pinned here, a scheduled job?
pub(crate) fn is_job(env: &Environment, m: &Manifest, name: &str) -> bool {
    pinned_decl(env, m, name).map(|l| l.decl.kind == "job").unwrap_or(false)
}

/// Does the declaration of the pinned build allow a restart? Unresolvable
/// means yes — the default — so a declaration fault never silently turns
/// supervision off; the launch path reports the fault itself.
pub(crate) fn restart_allowed(env: &Environment, m: &Manifest, name: &str) -> bool {
    pinned_decl(env, m, name).map(|l| l.decl.restart != Restart::NEVER).unwrap_or(true)
}

/// The environment a job's pass (or its health check) runs with: the same
/// `CAULDRON_*` set a hosted daemon gets, then the declaration's `[env]`.
fn command(env: &Environment, l: &crate::Launchable, args: &[String]) -> Result<(Command, PathBuf)> {
    let d = &l.decl;
    let binary = env.build_dir(&d.name, &l.label).join(&d.build()?.bin);
    if !binary.exists() {
        bail!("version `{}` of `{}` has no binary at {}", l.label, d.name, binary.display());
    }
    let data_dir = env.service_data(&d.name)?;
    std::fs::create_dir_all(&data_dir)?;
    let mut cmd = Command::new(&binary);
    cmd.args(args)
        .current_dir(&data_dir)
        .env("CAULDRON_SERVICE", &d.name)
        .env("CAULDRON_ENV", &env.name)
        .env("CAULDRON_HOME", &env.root)
        .env("CAULDRON_DATA_ROOT", &env.data_root)
        .env("CAULDRON_SERVICE_DATA_DIR", &data_dir)
        .env("CAULDRON_VERSION", &l.label)
        .env("CAULDRON_COMMIT", &l.rec.commit)
        .env("CAULDRON_DIRTY", if l.rec.dirty { "1" } else { "0" })
        .env("CAULDRON_BINARY", &binary)
        .env_remove("ENV")
        .stdin(Stdio::null());
    for (k, v) in &d.env {
        cmd.env(k, crate::expand(v, &data_dir));
    }
    Ok((cmd, data_dir))
}

/// `t` (unix seconds) as UTC ISO-8601, `2026-10-16T14:30:00Z`. Pure
/// arithmetic (days-from-civil inverted), no zone database.
pub(crate) fn utc_iso(t: u64) -> String {
    let (days, sod) = ((t / 86_400) as i64, t % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", sod / 3600, sod / 60 % 60, sod % 60)
}

/// `t` as LOCAL ISO-8601 with its offset and zone name, under the zone rules
/// in force at `t`: `2026-10-16T09:30:00-05:00 CDT`. `None` if `/bin/date`
/// cannot be read.
pub(crate) fn local_iso(t: u64) -> Option<String> {
    let out = Command::new("/bin/date").arg("-r").arg(t.to_string()).arg("+%Y-%m-%dT%H:%M:%S %z %Z").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.split_whitespace();
    let (dt, z, name) = (it.next()?, it.next()?, it.next().unwrap_or(""));
    if z.len() != 5 {
        return None;
    }
    Some(format!("{dt}{}:{} {name}", &z[..3], &z[3..]).trim_end().to_string())
}

/// **A CALENDAR JOB'S NEXT SLOT, as `status` shows it (R5.S5, Deploy5-2).**
#[derive(Debug, Clone, Serialize)]
pub(crate) struct NextSlot {
    /// Unix seconds.
    pub unix: u64,
    /// UTC ISO-8601.
    pub utc: String,
    /// Local ISO-8601 with offset and zone name; `null` if unreadable.
    pub local: Option<String>,
    /// The slot is already due: the holder starts it on its next poll (or
    /// it is past and no holder is polling).
    pub due: bool,
}

/// The next slot of a calendar schedule given its persisted record, through
/// [`seed_last_slot`] and [`next_slot`] — the holder's own functions, read
/// exactly as its first look reads them. `None` for an interval job.
fn next_slot_of(s: &env::hosting::Schedule, rec: Option<&Record>, c: &dyn Clock) -> Option<u64> {
    let target = Sched::target(s)?;
    next_slot(c, target, seed_last_slot(c, target, rec))
}

fn next_slot_view(s: &env::hosting::Schedule, rec: Option<&Record>, c: &dyn Clock) -> Option<NextSlot> {
    let unix = next_slot_of(s, rec, c)?;
    Some(NextSlot { unix, utc: utc_iso(unix), local: local_iso(unix), due: unix <= c.wall() })
}

/// The first `requires` path that does not exist, expanded.
fn missing_requirement(env: &Environment, d: &Declaration) -> Option<String> {
    let s = d.schedule.as_ref()?;
    let data_dir = env.service_data(&d.name).ok()?;
    s.requires
        .iter()
        .map(|r| crate::expand(r, &data_dir))
        .find(|p| !std::path::Path::new(p).exists())
}

/// **ONE JOB, ONE DAEMON PASS.** Reap a finished run, kill an overdue one,
/// start a due one.
pub(crate) fn tend(env: &Environment, m: &Manifest, policy: &Policy, name: &str) -> Result<()> {
    tend_with(env, m, policy, name, &SystemClock)
}

/// [`tend`] on a given clock.
fn tend_with(env: &Environment, m: &Manifest, policy: &Policy, name: &str, clock: &dyn Clock) -> Result<()> {
    let l = crate::resolve_launch(env, m, name)?;
    let d = l.decl.clone();
    let s = d
        .schedule
        .clone()
        .with_context(|| format!("`{name}` is a job with no `[schedule]`"))?;
    let now = clock.mono();
    let mut jobs = JOBS.lock().map_err(|_| anyhow::anyhow!("poisoned job table"))?;
    let live = jobs.entry(name.to_string()).or_insert_with(|| Live { child: None, started: None, sched: Sched::new() });

    // ── 1. A RUN IN FLIGHT: finished? overdue? ───────────────────────────
    if let Some(child) = live.child.as_mut() {
        let pid = child.id();
        let elapsed = live.started.map(|t| now.saturating_duration_since(t)).unwrap_or_default();
        let mut status = child.try_wait()?;
        let mut timed_out = false;
        if status.is_none() && elapsed > Duration::from_secs(s.timeout_secs) {
            timed_out = true;
            crate::signal(pid, &d.stop_signal);
            std::thread::sleep(Duration::from_millis(300));
            crate::signal(pid, "KILL");
            status = Some(child.wait()?);
        }
        let Some(st) = status else { return Ok(()) };
        use std::os::unix::process::ExitStatusExt;
        let rc = st.code();
        let sig = st.signal();
        let ok = st.success() && !timed_out;
        let retry = !ok && policy.auto_restart && d.restart != Restart::NEVER;
        let wait = live.sched.ended(&s, clock, retry, elapsed.as_secs());
        live.child = None;
        let ended = match (rc, sig) {
            (Some(c), _) => format!("exit {c}"),
            (None, Some(sg)) => format!("killed by signal {sg}"),
            _ => "ended".into(),
        };
        let detail = format!(
            "pass of `{name}` (pid {pid}) {ended}{} after {}s; next pass in {wait}s ({})",
            if timed_out { format!(", past its {}s timeout", s.timeout_secs) } else { String::new() },
            elapsed.as_secs(),
            if retry {
                "RETRY: it failed and `restart = \"always\"`"
            } else if ok {
                "on schedule"
            } else if d.restart == Restart::NEVER {
                "on schedule, NOT retried: `restart = \"never\"`"
            } else {
                "on schedule, NOT retried: this environment's `auto_restart = false`"
            }
        );
        println!("cauldron: {detail}");
        let mut r = supervisions::note(env, crate::SELF_COMMIT, name, Act::RAN, Why::SCHEDULE);
        r.version = l.label.clone();
        r.pid = Some(pid);
        r.detail = detail.clone();
        if let Err(e) = supervisions::append(&env.data_root, &r) {
            eprintln!("cauldron: {}", supervisions::write_failure(name, Act::RAN, &env.data_root, &e));
        }
        save_one(
            env,
            name,
            Record {
                state: if ok { "ok" } else { "failed" }.into(),
                version: l.label.clone(),
                pid: None,
                last_start: Some(clock.wall().saturating_sub(elapsed.as_secs())),
                last_end: Some(clock.wall()),
                last_rc: rc,
                last_signal: sig,
                next_in_secs: Some(wait),
                detail,
                last_slot: live.sched.last_slot,
            },
        );
        return Ok(());
    }

    // ── 2. A PENDING HANDOVER IS NOT RUN (R5.S3b). The PINNED build's
    // declaration still carries `[migration]`: its launcher (launchd) may
    // still run this job, and two schedulers is two passes. `pin` checks only
    // the SHIPPED file, so building with `[migration]`, deleting it, and
    // pinning the earlier label reached here (measured by the R5.S3
    // inspector). This is the job twin of `launch_for_daemon`'s check.
    // Re-checked every pass; said once, kept in `status`.
    if let Some(mg) = &d.migration {
        let detail = mg.refusal(name);
        // R5.S3d-1 (R5.S7): "said once" PER PINNED LABEL. Comparing only the
        // recorded state kept a refusal of a DIFFERENT label silent when the
        // record already said `refused` (a re-pin to another `[migration]`
        // label, or refused A -> clean -> refused B with no pass between), so
        // the log line names the label and is said again when the label
        // changes. NOT covered: refused A -> clean -> the SAME label A with no
        // pass between — the record cannot tell that apart from "still A".
        let prev = load(env).get(name).map(|r| (r.state.clone(), r.version.clone()));
        if prev.as_ref().map(|(st, v)| (st.as_str(), v.as_str())) != Some(("refused", l.label.as_str())) {
            eprintln!("cauldron: pinned version `{}` of `{name}`: {detail}", l.label);
        }
        live.sched.reset();
        save_one(
            env,
            name,
            Record { state: "refused".into(), version: l.label.clone(), detail, ..Default::default() },
        );
        return Ok(());
    }

    // ── 3. NOTHING IN FLIGHT: is one due? ────────────────────────────────
    // Re-derived EVERY poll from the wall clock (R5.S4b), so a slot that came
    // due while the machine slept runs on the first poll after wake.
    let prev = if matches!(live.sched.next, Next::Unset) { load(env).get(name).cloned() } else { None };
    let due = live.sched.due(&s, clock, prev.as_ref());
    if let Some(reason) = live.sched.load_orphan.take() {
        // R5.S7: the end of a pass no holder saw. Recorded as the `ran` it
        // is owed, so every `run` in `supervisions` still has its end, and
        // `why = orphaned` says the outcome is inferred, not observed.
        let detail = format!("`{name}`: {reason}");
        println!("cauldron: {detail}");
        let mut r = supervisions::note(env, crate::SELF_COMMIT, name, Act::RAN, Why::ORPHANED);
        r.version = prev.as_ref().map(|p| p.version.clone()).unwrap_or_else(|| l.label.clone());
        r.pid = prev.as_ref().and_then(|p| p.pid);
        r.detail = detail.clone();
        if let Err(e) = supervisions::append(&env.data_root, &r) {
            eprintln!("cauldron: {}", supervisions::write_failure(name, Act::RAN, &env.data_root, &e));
        }
        // And `jobs.json` stops saying `running` for a pass that is gone
        // (inspector F1): if the re-run below fails to start, `status` reads
        // the unfinished pass as `failed`, with the reason, not as running.
        if let Some(p) = prev.as_ref() {
            save_one(env, name, Record { state: "failed".into(), pid: None, detail, ..p.clone() });
        }
    }
    if let Some(reason) = live.sched.load_skip.take() {
        // R5.S6: the skip is an act of supervision and is recorded as one,
        // so `supervisions` answers "why did the new holder not run it".
        let detail = format!("`{name}`: {reason}");
        println!("cauldron: {detail}");
        let mut r = supervisions::note(env, crate::SELF_COMMIT, name, Act::SKIP, Why::RECENT);
        r.version = l.label.clone();
        r.detail = detail;
        if let Err(e) = supervisions::append(&env.data_root, &r) {
            eprintln!("cauldron: {}", supervisions::write_failure(name, Act::SKIP, &env.data_root, &e));
        }
    }
    if !due {
        return Ok(());
    }
    if let Some(missing) = missing_requirement(env, &d) {
        let detail = format!(
            "`{name}` is BLOCKED: its `[schedule] requires` names {missing}, which does not \
             exist. No pass was started, so it cannot fall back to a default that names some \
             other environment's data. Create it and the next pass runs."
        );
        // Re-checked EVERY poll (a file-exists test is cheap), so the pass
        // starts within one poll of the requirement appearing. Nothing here
        // touches the schedule, and that is what does it: this branch is
        // reached only when the pass is due, and nothing but a START moves a
        // due schedule, so the next poll is due again as it stands. (R5.S3c: an explicit `next_due = Some(now)` here was a
        // no-op — deleting it stayed green — and was removed.) The defect
        // this replaced DEFERRED the re-check up to 30 s, which the R5.S3
        // final run measured as a 40 s test timeout; the guard is the
        // latency bound in `hosts_all`'s blocked-job test, which any
        // deferral written here turns red (R5.S3b inspector, M2b, ~26 s).
        let prev = load(env).get(name).map(|r| r.state.clone());
        if prev.as_deref() != Some("blocked") {
            eprintln!("cauldron: {detail}");
        }
        save_one(
            env,
            name,
            Record { state: "blocked".into(), version: l.label.clone(), detail, ..Default::default() },
        );
        return Ok(());
    }
    let why = live.sched.why;
    let (mut cmd, _) = command(env, &l, &s.args)?;
    let log = env.log(name)?;
    std::fs::create_dir_all(log.parent().unwrap())?;
    // DG-2: rotated by rename past 64 MiB, 3 generations kept, before it is opened (logrotate.rs).
    let out = crate::logrotate::open_log(&log)?;
    let err = out.try_clone()?;
    cmd.stdout(Stdio::from(out)).stderr(Stdio::from(err));
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    let child = cmd.spawn().with_context(|| format!("spawning a pass of `{name}`"))?;
    let pid = child.id();
    live.child = Some(child);
    live.started = Some(now);
    live.sched.started(&s, clock);
    let last_slot = live.sched.last_slot;
    let detail = format!(
        "started a pass of `{name}` (pid {pid}, version {}, args {:?}); log {}",
        l.label,
        s.args,
        log.display()
    );
    println!("cauldron: {detail}");
    let mut r = supervisions::note(env, crate::SELF_COMMIT, name, Act::RUN, why);
    r.version = l.label.clone();
    r.pid = Some(pid);
    r.detail = detail.clone();
    if let Err(e) = supervisions::append(&env.data_root, &r) {
        eprintln!("cauldron: {}", supervisions::write_failure(name, Act::RUN, &env.data_root, &e));
    }
    save_one(
        env,
        name,
        Record {
            state: "running".into(),
            version: l.label.clone(),
            pid: Some(pid),
            last_start: Some(clock.wall()),
            detail,
            last_slot,
            ..Default::default()
        },
    );
    Ok(())
}

/// Kill and forget any running job that is no longer pinned (an unpin is a
/// deliberate stop, for a job as for a daemon).
pub(crate) fn sweep(env: &Environment, pinned: &[String]) {
    let Ok(mut jobs) = JOBS.lock() else { return };
    let gone: Vec<String> = jobs.keys().filter(|k| !pinned.contains(k)).cloned().collect();
    for name in gone {
        if let Some(mut live) = jobs.remove(&name) {
            if let Some(mut c) = live.child.take() {
                let pid = c.id();
                crate::signal(pid, "TERM");
                std::thread::sleep(Duration::from_millis(300));
                crate::signal(pid, "KILL");
                let _ = c.wait();
                let mut r = supervisions::note(env, crate::SELF_COMMIT, &name, Act::STOP, Why::UNPINNED);
                r.pid = Some(pid);
                r.detail = format!("`{name}` is no longer pinned; its running pass (pid {pid}) was stopped");
                let _ = supervisions::append(&env.data_root, &r);
            }
        }
    }
}

/// **A JOB'S HEALTH, ASKED NOW.** Runs the image with `health_args` (bounded
/// at 15 s). `None` when the declaration names no health check.
pub(crate) fn health(env: &Environment, l: &crate::Launchable) -> Option<(bool, String)> {
    let s = l.decl.schedule.as_ref()?;
    if s.health_args.is_empty() {
        return None;
    }
    // THE HEALTH CHECK OBEYS `requires` TOO. Measured in the R5.S3 proof: with
    // the env's config missing, `soup-backup status` ran on its DEFAULTS and
    // reported on the operator's real ~/.soup-backups — another environment's
    // data — read-only, but the wrong answer. A blocked job is not asked.
    if let Some(missing) = missing_requirement(env, &l.decl) {
        return Some((false, format!("not asked: `requires` names {missing}, which does not exist")));
    }
    let run = || -> Result<(bool, String)> {
        let (mut cmd, _) = command(env, l, &s.health_args)?;
        let mut c = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let t0 = Instant::now();
        loop {
            if let Some(st) = c.try_wait()? {
                let out = c.wait_with_output()?;
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                let first = text.lines().find(|x| !x.trim().is_empty()).unwrap_or("").to_string();
                return Ok((st.success(), format!("`{:?}` -> {st}: {first}", s.health_args)));
            }
            if t0.elapsed() > Duration::from_secs(15) {
                let _ = c.kill();
                let _ = c.wait();
                return Ok((false, format!("`{:?}` did not answer in 15s", s.health_args)));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    Some(run().unwrap_or_else(|e| (false, format!("health check could not run: {e:#}"))))
}

/// `status`'s row for a HOSTED job (one with no `[migration]`). For a
/// CALENDAR job (`at = "HH:MM"`) it also sets `next` and appends the next
/// slot to `detail` (R5.S5); an interval job's `next` stays `None` — its
/// record's own `next pass in Ns` already says when.
pub(crate) fn overlay(
    env: &Environment,
    m: &Manifest,
    d: &Declaration,
    state: &mut String,
    detail: &mut String,
    pid: &mut Option<u32>,
    next: &mut Option<NextSlot>,
) {
    overlay_on(env, m, d, state, detail, pid, next, &SystemClock)
}

/// [`overlay`] on a given clock.
#[allow(clippy::too_many_arguments)]
fn overlay_on(
    env: &Environment,
    m: &Manifest,
    d: &Declaration,
    state: &mut String,
    detail: &mut String,
    pid: &mut Option<u32>,
    next: &mut Option<NextSlot>,
    clock: &dyn Clock,
) {
    if d.kind != "job" || d.migration.is_some() || d.schedule.is_none() {
        return;
    }
    if m.pin(&d.name).is_none() {
        return; // not run here; examine()'s "not pinned" row stands
    }
    let pinned = pinned_decl(env, m, &d.name);
    let mut rec = load(env).get(&d.name).cloned();
    // **A REFUSAL IS A PROPERTY OF THE PIN, NOT OF THE RECORD (R5.S3d).** The
    // holder writes `refused` into `jobs.json`, and only a later PASS rewrites
    // it; a job with `run_at_load = false` (the alarm) is not due right after
    // a re-pin, so after the runbook's remedy — rebuild, pin the NEW clean
    // label — `status` kept reading `job:refused`, with the OLD label's text,
    // until the next scheduled pass (R5.S3c inspector, 2/2). So the refusal
    // is decided here from the PINNED build's declaration, exactly the
    // predicate `tend` §2 refuses on: a pinned build that carries
    // `[migration]` reads `job:refused` at once (even before the holder has
    // seen the pin), and a recorded refusal under a pin that is clean is of
    // an EARLIER pin and is not shown. Unresolvable pin: the record stands.
    let mut note = String::new();
    if let Some(l) = &pinned {
        if let Some(mg) = &l.decl.migration {
            *state = "job:refused".into();
            *pid = None;
            *detail = mg.refusal(&d.name);
            return;
        }
        if let Some(r) = rec.as_ref().filter(|r| r.state == "refused") {
            note = format!(
                " (the refusal recorded for version `{}` was of an earlier pin; version `{}` is \
                 pinned now and carries no `[migration]`)",
                r.version, l.label
            );
            rec = None;
        }
    }
    let base = match &rec {
        None => "job:never-run".to_string(),
        Some(r) => format!("job:{}", r.state),
    };
    // A REFUSED job's health check is not run (R5.S3c, R5.S3b inspector
    // finding 3): the holder refused to run anything out of that pinned build
    // (its declaration still says another launcher runs it), so `status` does
    // not execute a command out of it either — and a failing one would read
    // `job:unhealthy` and HIDE `job:refused` and its remedy. A blocked job's
    // check is skipped inside `health()` (its `requires` is missing).
    let health = if base == "job:refused" {
        None
    } else {
        pinned.as_ref().and_then(|l| health(env, l))
    };
    *state = match &health {
        _ if base == "job:blocked" || base == "job:refused" => base,
        Some((false, _)) => "job:unhealthy".into(),
        _ => base,
    };
    *pid = rec.as_ref().and_then(|r| r.pid);
    // The schedule the holder tends is the PINNED build's (`tend` resolves
    // the launch); the source declaration's only if the pin cannot resolve.
    let sched = pinned.as_ref().and_then(|l| l.decl.schedule.clone()).or_else(|| d.schedule.clone());
    *next = sched.as_ref().and_then(|s| next_slot_view(s, rec.as_ref(), clock));
    let slot_note = match next.as_ref() {
        Some(n) => format!(
            " | next slot {} ({}){}",
            n.utc,
            n.local.as_deref().unwrap_or("local time unreadable"),
            if n.due { " — DUE NOW: the holder starts it on its next poll" } else { "" }
        ),
        None => String::new(),
    };
    *detail = format!(
        "{}{note}{slot_note}{}",
        rec.map(|r| r.detail).unwrap_or_else(|| "pinned here, but the holder has not run it yet (no daemon, or not yet due)".into()),
        match health {
            Some((ok, h)) => format!(" | health {}: {h}", if ok { "OK" } else { "FAILING" }),
            None => String::new(),
        }
    );
}

/// **WAIT FOR A HOSTED CHILD'S HEALTH URL TO ANSWER 200** — the `probe =
/// "http"` half of a launch. Fails if the child exits or the budget runs out.
pub(crate) fn wait_healthy(url: &str, child: &mut std::process::Child, budget: Duration) -> Result<()> {
    let t0 = Instant::now();
    loop {
        let last = match env::hosting::http_health(url, Duration::from_millis(500)) {
            Ok(200) => return Ok(()),
            Ok(code) => format!("answered {code}"),
            Err(e) => format!("{e:#}"),
        };
        if let Some(st) = child.try_wait()? {
            bail!("the child exited ({st}) before its health check {url} answered 200 (last: {last})");
        }
        if t0.elapsed() > budget {
            bail!("its health check {url} did not answer 200 within {budget:?} (last: {last})");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// R5.S4b — the schedule on an INJECTED clock. A macOS sleep, seen from
/// inside the holder, is the wall clock moving while the monotonic one
/// (CLOCK_UPTIME_RAW) stands still; [`Fake::sleep`] is exactly that. Each
/// test's first assertion is a sentinel that the two clocks it compares are
/// genuinely different things (doctrine 9).
#[cfg(test)]
mod wall_clock_schedule {
    use super::*;
    use env::hosting::Schedule;
    use std::cell::Cell;

    const DAY: u64 = 86_400;
    /// Local midnight of "day 0", in a UTC-5 zone (CDT).
    const D0: u64 = 20_300 * DAY + 5 * 3600;

    fn hm(h: u64, m: u64) -> u64 {
        h * 3600 + m * 60
    }

    /// JOBS-DST: the fake's LOCAL ZONE, injected — no slot test reads the
    /// machine's `TZ`.
    #[derive(Debug, Clone, Copy)]
    enum Zone {
        /// One offset, always (seconds east of UTC): a zone with no DST.
        Fixed(i64),
        /// `before` until wall time `from`, then `after` (R5.S5: a DST
        /// change ahead of now).
        Shift { before: i64, from: u64, after: i64 },
        /// America/Los_Angeles under the US rule in force since 2007: PDT
        /// (-7 h) from 02:00 PST on the second Sunday of March to 02:00 PDT
        /// on the first Sunday of November, PST (-8 h) otherwise. 2026:
        /// 2026-03-08T10:00:00Z and 2026-11-01T09:00:00Z.
        UsPacific,
    }

    impl Zone {
        fn offset_at(self, t: u64) -> i64 {
            match self {
                Zone::Fixed(off) => off,
                Zone::Shift { before, from, after } => {
                    if t >= from {
                        after
                    } else {
                        before
                    }
                }
                Zone::UsPacific => {
                    use chrono::{Datelike, NaiveDate, Weekday};
                    let y = chrono::DateTime::from_timestamp(t as i64, 0).unwrap().year();
                    let utc = |m, n, h| {
                        let d = NaiveDate::from_weekday_of_month_opt(y, m, Weekday::Sun, n).unwrap();
                        d.and_hms_opt(h, 0, 0).unwrap().and_utc().timestamp() as u64
                    };
                    if (utc(3, 2, 10)..utc(11, 1, 9)).contains(&t) {
                        -7 * 3600
                    } else {
                        -8 * 3600
                    }
                }
            }
        }
    }

    /// The local zone of every R5.S4b/R5.S5 test: UTC-5 (CDT), no DST.
    const CDT: Zone = Zone::Fixed(-5 * 3600);

    struct Fake {
        base: Instant,
        mono: Cell<u64>,
        wall: Cell<u64>,
        /// The local zone (a zone change on the machine replaces it).
        zone: Cell<Zone>,
    }

    impl Fake {
        /// Local time `t` seconds after day 0's midnight, in [`CDT`].
        fn at(t: u64) -> Self {
            Fake::utc(D0 + t, CDT)
        }
        /// Wall time `t` (unix seconds) in `zone`.
        fn utc(t: u64, zone: Zone) -> Self {
            Fake { base: Instant::now(), mono: Cell::new(0), wall: Cell::new(t), zone: Cell::new(zone) }
        }
        /// Time passes with the machine awake: both clocks move.
        fn awake(&self, s: u64) {
            self.mono.set(self.mono.get() + s);
            self.wall.set(self.wall.get() + s);
        }
        /// The machine sleeps: only the wall clock moves.
        fn sleep(&self, s: u64) {
            self.wall.set(self.wall.get() + s);
        }
        /// Sleep until local time `t` after day 0's midnight.
        fn sleep_until(&self, t: u64) {
            self.sleep(D0 + t - self.wall.get());
        }
        fn step_wall_back(&self, s: u64) {
            self.wall.set(self.wall.get() - s);
        }
        fn local(&self) -> u64 {
            self.wall.get() - D0
        }
        /// Seconds after local midnight now.
        fn sod(&self) -> Option<u64> {
            let t = self.wall.get();
            Some((t as i64 + self.offset_at(t)?).rem_euclid(DAY as i64) as u64)
        }
    }

    impl Clock for Fake {
        fn mono(&self) -> Instant {
            self.base + Duration::from_secs(self.mono.get())
        }
        fn wall(&self) -> u64 {
            self.wall.get()
        }
        fn offset_at(&self, t: u64) -> Option<i64> {
            Some(self.zone.get().offset_at(t))
        }
    }

    fn sched(at: Option<&str>, every: u64, run_at_load: bool) -> Schedule {
        Schedule {
            every_secs: every,
            at: at.map(str::to_string),
            args: vec![],
            health_args: vec![],
            requires: vec![],
            retry_secs: 300,
            timeout_secs: 300,
            run_at_load,
        }
    }

    fn alarm() -> Schedule {
        sched(Some("09:30"), 0, false)
    }

    /// Poll every `step` seconds, awake, for `total` seconds; how many polls
    /// found a pass due (each due one is started and ended ok, as `tend`
    /// would, so a double-run counts twice).
    fn polls(sc: &mut Sched, s: &Schedule, c: &Fake, step: u64, total: u64) -> u32 {
        let mut n = 0;
        let mut t = 0;
        while t < total {
            c.awake(step);
            t += step;
            if sc.due(s, c, None) {
                n += 1;
                sc.started(s, c);
                sc.ended(s, c, false, 0);
            }
        }
        n
    }

    #[test]
    fn sentinel_the_fake_sleep_moves_the_wall_clock_and_not_the_monotonic_one() {
        let c = Fake::at(hm(22, 36));
        let (m0, w0) = (c.mono(), c.wall());
        c.sleep(3600);
        assert_eq!(c.mono(), m0, "sleep must not move the monotonic clock");
        assert_eq!(c.wall(), w0 + 3600);
        c.awake(60);
        assert_eq!(c.mono(), m0 + Duration::from_secs(60));
        assert_eq!(c.sod(), Some(hm(23, 37)), "local time of day is read off the wall clock");
    }

    /// The Deploy-4 case: armed 22:36, the Mac sleeps 23:00–08:00, so by
    /// 09:30 local the monotonic clock has advanced ~2 h of the ~10 h 54 m it
    /// was armed for. Due on the first poll at 09:30, not 9 h later.
    #[test]
    fn a_calendar_slot_is_due_on_the_wall_clock_after_an_overnight_sleep() {
        let s = alarm();
        let c = Fake::at(hm(22, 36));
        let mut sc = Sched::new();
        assert!(!sc.due(&s, &c, None), "armed at 22:36: the 09:30 slot already passed is not owed");
        c.awake(hm(0, 24));
        c.sleep_until(DAY + hm(8, 0));
        assert!(c.mono() < c.base + Duration::from_secs(hm(1, 0)), "sentinel: ~24 min of uptime only");
        c.awake(hm(1, 29) + 59);
        assert!(!sc.due(&s, &c, None), "09:29:59 is not yet 09:30 (not early)");
        c.awake(1);
        assert_eq!(c.sod(), Some(hm(9, 30)));
        assert!(sc.due(&s, &c, None), "09:30:00 local, after a 9 h sleep: the slot is due NOW");
    }

    /// The brief's case: the wall clock jumps past the due time while the
    /// monotonic clock does not move at all — due on the very next poll.
    #[test]
    fn a_wall_jump_past_the_slot_with_no_uptime_runs_on_the_next_poll() {
        let s = alarm();
        let c = Fake::at(hm(22, 36));
        let mut sc = Sched::new();
        assert!(!sc.due(&s, &c, None));
        c.sleep_until(DAY + hm(10, 15));
        assert_eq!(c.mono(), c.base, "sentinel: no uptime passed");
        assert!(sc.due(&s, &c, None), "woke at 10:15 after the 09:30 slot: due on this poll");
    }

    #[test]
    fn a_jump_of_n_days_runs_exactly_once_and_the_next_real_slot_still_runs() {
        let s = alarm();
        let c = Fake::at(hm(22, 36));
        let mut sc = Sched::new();
        assert!(!sc.due(&s, &c, None));
        c.sleep_until(5 * DAY + hm(10, 0));
        assert!(sc.due(&s, &c, None), "five slots slept through: due on wake");
        sc.started(&s, &c);
        sc.ended(&s, &c, false, 2);
        // Every 30 s, awake, until 09:29 the next day: never due again — the
        // four other missed slots are coalesced, as launchd does.
        assert_eq!(polls(&mut sc, &s, &c, 30, DAY - hm(0, 31)), 0, "a missed slot ran more than once");
        assert_eq!(c.local(), 6 * DAY + hm(9, 29));
        // Opposed: the margin must not swallow the next REAL slot.
        assert_eq!(polls(&mut sc, &s, &c, 30, hm(0, 2)), 1, "the next day's 09:30 did not run");
    }

    #[test]
    fn a_backward_wall_step_does_not_rerun_a_slot() {
        let s = alarm();
        let c = Fake::at(hm(9, 29));
        let mut sc = Sched::new();
        assert!(!sc.due(&s, &c, None));
        assert_eq!(polls(&mut sc, &s, &c, 30, 90), 1, "the 09:30 slot ran once");
        c.step_wall_back(hm(2, 0));
        assert_eq!(c.sod(), Some(hm(7, 30) + 30));
        // Back through 09:30 again, awake, polling every 30 s: not re-run.
        assert_eq!(polls(&mut sc, &s, &c, 30, hm(3, 0)), 0, "a backward step re-ran the 09:30 slot");
        // Opposed: the next day's slot still runs.
        let left = DAY + hm(9, 31) - c.local();
        assert_eq!(polls(&mut sc, &s, &c, 30, left), 1, "after a backward step the next slot was lost");
    }

    /// A zone or DST change moves local time by an hour with the wall clock
    /// unchanged: the slot just run must not be counted as a new one. The
    /// FALL-BACK direction is the one that bites: local time returns to 09:30
    /// an hour of wall time after the slot ran, so the slot arithmetic sees a
    /// slot one hour newer (R5.S4b forcing M2: margin 0 survived the
    /// spring-forward direction alone).
    #[test]
    fn a_dst_shift_either_way_right_after_a_pass_does_not_run_the_slot_again() {
        for shift in [-3600i64, 3600] {
            let s = alarm();
            let c = Fake::at(hm(9, 29));
            let mut sc = Sched::new();
            assert!(!sc.due(&s, &c, None));
            assert_eq!(polls(&mut sc, &s, &c, 30, 90), 1);
            c.zone.set(Zone::Fixed(-5 * 3600 + shift)); // the machine's zone changes
            assert_eq!(polls(&mut sc, &s, &c, 30, hm(3, 0)), 0, "a {shift} s DST shift re-ran the slot");
            // Opposed: the next day's slot (local) still runs.
            assert_eq!(polls(&mut sc, &s, &c, 30, hm(24, 0)), 1, "after a {shift} s shift the next slot was lost");
        }
    }

    /// Persisted `last_slot`, through JSON, as `jobs.json` carries it.
    fn roundtrip(all: &BTreeMap<String, Record>) -> BTreeMap<String, Record> {
        serde_json::from_str(&serde_json::to_string(all).unwrap()).unwrap()
    }

    #[test]
    fn a_holder_restart_after_a_pass_does_not_run_the_slot_twice() {
        let s = alarm();
        let c = Fake::at(hm(9, 29));
        let mut a = Sched::new();
        assert!(!a.due(&s, &c, None));
        c.awake(60);
        assert!(a.due(&s, &c, None));
        a.started(&s, &c);
        let mut all = BTreeMap::new();
        merge_one(&mut all, "alarm", Record { state: "running".into(), last_start: Some(c.wall()), last_slot: a.last_slot, ..Default::default() });
        // The `ran` record carries no slot; the merge must keep it.
        merge_one(&mut all, "alarm", Record { state: "ok".into(), last_start: Some(c.wall()), ..Default::default() });
        let all = roundtrip(&all);
        assert!(all["alarm"].last_slot.is_some(), "the `ran` record dropped the slot");
        // The holder restarts at 09:45 the same day.
        c.awake(hm(0, 15));
        let mut b = Sched::new();
        let prev = all.get("alarm");
        assert!(!b.due(&s, &c, prev), "the restarted holder ran today's slot a second time");
        assert_eq!(polls(&mut b, &s, &c, 60, hm(23, 0)), 0);
        // Opposed: tomorrow's slot is still run by the restarted holder.
        assert_eq!(polls(&mut b, &s, &c, 60, hm(1, 0)), 1);
    }

    #[test]
    fn a_holder_down_across_a_slot_runs_it_once_when_it_comes_back() {
        let s = alarm();
        let c = Fake::at(hm(9, 29));
        let mut a = Sched::new();
        assert!(!a.due(&s, &c, None));
        assert_eq!(polls(&mut a, &s, &c, 30, 90), 1, "day 0's slot");
        let mut all = BTreeMap::new();
        merge_one(&mut all, "alarm", Record { state: "ok".into(), last_slot: a.last_slot, ..Default::default() });
        let all = roundtrip(&all);
        // The holder dies at 23:00 and comes back at 09:45 the next day
        // (a reboot: new monotonic origin, nothing in memory).
        let c = Fake::at(DAY + hm(9, 45));
        let mut b = Sched::new();
        assert!(b.due(&s, &c, all.get("alarm")), "the slot that came due while the holder was down was lost");
        b.started(&s, &c);
        b.ended(&s, &c, false, 1);
        assert_eq!(polls(&mut b, &s, &c, 60, hm(23, 0)), 0, "and run only once");
    }

    /// A record written by the PRE-R5.S4b holder has `last_start` and no
    /// `last_slot` (Deploy 5 meets exactly that): it is read as the slot it
    /// started in.
    #[test]
    fn a_legacy_record_without_a_slot_is_read_from_its_last_start() {
        let s = alarm();
        let c = Fake::at(hm(22, 0));
        let legacy = Record { state: "ok".into(), last_start: Some(D0 + hm(9, 30) + 5), ..Default::default() };
        let mut b = Sched::new();
        assert!(!b.due(&s, &c, Some(&legacy)), "today's slot already ran under the old holder");
        assert_eq!(polls(&mut b, &s, &c, 60, hm(11, 29)), 0);
        assert_eq!(polls(&mut b, &s, &c, 60, hm(0, 2)), 1, "tomorrow's slot");
        // And yesterday's start, holder replaced after today's slot: owed now.
        let c = Fake::at(DAY + hm(10, 0));
        let mut b = Sched::new();
        assert!(b.due(&s, &c, Some(&legacy)), "today's slot was never run and was lost");
    }

    #[test]
    fn an_interval_is_due_after_a_sleep_and_not_early_after_a_backward_step() {
        let s = sched(None, 300, false);
        let c = Fake::at(hm(12, 0));
        let mut sc = Sched::new();
        assert!(!sc.due(&s, &c, None));
        c.sleep(301);
        assert_eq!(c.mono(), c.base, "sentinel: no uptime passed");
        assert!(sc.due(&s, &c, None), "a 300 s interval slept through is due on wake");
        sc.started(&s, &c);
        sc.ended(&s, &c, false, 0);
        c.step_wall_back(3600);
        c.awake(299);
        assert!(!sc.due(&s, &c, None), "not early");
        c.awake(1);
        assert!(sc.due(&s, &c, None), "the monotonic half fires it on time after a backward wall step");
    }

    #[test]
    fn a_retry_is_due_after_a_sleep_and_says_retry() {
        let s = alarm();
        let c = Fake::at(hm(9, 30));
        let mut sc = Sched::new();
        sc.due(&s, &c, None);
        sc.started(&s, &c);
        assert_eq!(sc.ended(&s, &c, true, 3), 300);
        c.sleep(299);
        assert!(!sc.due(&s, &c, None), "not early");
        c.sleep(1);
        assert!(sc.due(&s, &c, None), "a retry slept through is due on wake");
        assert_eq!(sc.why, Why::RETRY);
    }

    #[test]
    fn run_at_load_runs_a_calendar_job_now_and_still_runs_todays_slot() {
        let s = sched(Some("09:30"), 0, true);
        let c = Fake::at(hm(8, 0));
        let mut sc = Sched::new();
        assert!(sc.due(&s, &c, None), "run_at_load");
        sc.started(&s, &c);
        sc.ended(&s, &c, false, 1);
        assert_eq!(polls(&mut sc, &s, &c, 60, hm(1, 29)), 0);
        assert_eq!(polls(&mut sc, &s, &c, 60, hm(0, 2)), 1, "the load run is not today's 09:30 slot");
    }

    // ═══════════════════════ R5.S5: the next slot `status` prints ═══════

    /// Poll every second from now until `until` (wall), awake, never starting
    /// a pass: the wall time of the FIRST poll that found one due.
    fn first_due(sc: &mut Sched, s: &Schedule, c: &Fake, until: u64) -> Option<u64> {
        while c.wall() < until {
            if sc.due(s, c, None) {
                return Some(c.wall());
            }
            c.awake(1);
        }
        sc.due(s, c, None).then(|| c.wall())
    }

    /// The next slot `status` computes from the persisted record IS the
    /// instant the holder's own `due` first says yes — measured by polling
    /// the holder's `Sched` every second, not argued from the code. Both
    /// sides of 09:30: at 09:31 after today's pass the answer is TOMORROW's
    /// 09:30 (a wrong "today 09:30" would be in the past and read due); at
    /// 09:29 with yesterday's pass it is TODAY's (a wrong "tomorrow" would be
    /// a day late). The two answers are asserted to differ before anything
    /// else (doctrine 9: the operands must be shown capable of differing).
    #[test]
    fn the_next_slot_status_prints_is_the_instant_the_scheduler_first_finds_it_due() {
        let s = alarm();
        let today = D0 + hm(9, 30); // 09:30 local on day 0
        let tomorrow = today + DAY;
        assert_ne!(today, tomorrow, "sentinel");
        for (now, last, want, why) in [
            (hm(9, 31), today, tomorrow, "09:31 after today's pass: tomorrow's slot"),
            (hm(9, 29), today - DAY, today, "09:29 after yesterday's pass: today's slot"),
        ] {
            let c = Fake::at(now);
            let rec = Record { last_slot: Some(last), ..Record::default() };
            let got = next_slot_of(&s, Some(&rec), &c).expect("a calendar job has a next slot");
            assert_eq!(got, want, "{why}: status's next slot is {} s off", got as i64 - want as i64);
            assert!(got > c.wall(), "{why}: not due yet, so the slot is in the future");
            assert!(!next_slot_view(&s, Some(&rec), &c).unwrap().due);
            // The holder, armed from the same record, first finds it due AT
            // that instant: not a second before, not a poll after.
            let mut sc = Sched::new();
            assert!(!sc.due(&s, &c, Some(&rec)), "{why}: due early");
            assert_eq!(first_due(&mut sc, &s, &c, want + 5), Some(want), "{why}: the scheduler disagrees");
        }
    }

    /// Never run (no record) — the alarm's state after Deploy 5: the slot
    /// already passed is not owed, so the next slot is TOMORROW's 09:30, and
    /// the holder armed at the same moment first finds it due exactly then.
    #[test]
    fn a_never_run_calendar_job_reports_the_next_slot_not_the_one_already_passed() {
        let s = alarm();
        let c = Fake::at(hm(23, 35));
        let want = D0 + DAY + hm(9, 30);
        let got = next_slot_of(&s, None, &c).unwrap();
        assert_eq!(got, want, "off by {} s", got as i64 - want as i64);
        assert_ne!(got, D0 + hm(9, 30), "the passed slot is not owed");
        let mut sc = Sched::new();
        assert_eq!(first_due(&mut sc, &s, &c, want + 5), Some(want));
    }

    /// A slot missed (the holder down, or the record from days ago) reads DUE,
    /// at the most recent slot — and the holder agrees on its first poll.
    #[test]
    fn a_missed_slot_reads_due_now_and_the_holder_runs_it_on_the_first_poll() {
        let s = alarm();
        let c = Fake::at(3 * DAY + hm(11, 0));
        let rec = Record { last_slot: Some(D0 + hm(9, 30)), ..Record::default() };
        let v = next_slot_view(&s, Some(&rec), &c).unwrap();
        assert!(v.due, "three slots missed: due now");
        assert_eq!(v.unix, D0 + 3 * DAY + hm(9, 30), "the most recent slot, not the oldest missed one");
        let mut sc = Sched::new();
        assert!(sc.due(&s, &c, Some(&rec)), "the holder disagrees: not due");
        // Opposed: the same record, an hour before today's slot, is not due.
        let c = Fake::at(3 * DAY + hm(8, 30));
        let rec = Record { last_slot: Some(D0 + 2 * DAY + hm(9, 30)), ..Record::default() };
        assert!(!next_slot_view(&s, Some(&rec), &c).unwrap().due);
    }

    /// **The DST edge.** Overnight, local time shifts an hour; the holder's
    /// slot arithmetic reads local time afresh on each poll, so it first finds
    /// the 09:30 slot due at 09:30 under the NEW offset. `status` must print
    /// that instant, not 24 h after the last slot — the two differ by the
    /// shift, and the poll shows which one the holder acts on.
    #[test]
    fn across_a_dst_change_the_next_slot_is_where_the_holder_will_see_it() {
        for (new_off, label) in [(-6 * 3600i64, "fall back"), (-4 * 3600, "spring forward")] {
            let s = alarm();
            let c = Fake::at(hm(22, 0));
            // 02:00 local tomorrow (old offset) the zone changes.
            c.zone.set(Zone::Shift { before: -5 * 3600, from: D0 + DAY + hm(2, 0), after: new_off });
            let naive = D0 + DAY + hm(9, 30);
            let want = (naive as i64 + (-5 * 3600 - new_off)) as u64;
            assert_ne!(naive, want, "sentinel: the shift moves the slot");
            let got = next_slot_of(&s, None, &c).unwrap();
            assert_eq!(got, want, "{label}: status says {} s off the holder", got as i64 - want as i64);
            let mut sc = Sched::new();
            assert_eq!(first_due(&mut sc, &s, &c, naive.max(want) + 5), Some(want), "{label}: the holder fired elsewhere");
        }
    }

    /// `due` and `next_slot` agree with the original predicate
    /// (`most_recent_slot > last + SLOT_MARGIN`) at every minute of two days,
    /// for last slots on either side of now — the refactor changed no answer.
    #[test]
    fn due_through_next_slot_matches_the_slot_margin_predicate_everywhere() {
        let target = hm(9, 30);
        let mut checked = (0, 0);
        for minute in 0..(2 * 24 * 60) {
            let c = Fake::at(minute * 60);
            for last in [D0 - DAY + target, D0 + target, D0 + DAY + target, D0 + 2 * DAY + target] {
                let old = most_recent_slot(&c, target).unwrap() > last + SLOT_MARGIN;
                let new = next_slot(&c, target, Some(last)).is_some_and(|t| t <= c.wall());
                assert_eq!(new, old, "minute {minute}, last {last}");
                checked.1 += usize::from(old);
                checked.0 += 1;
            }
        }
        assert!(checked.1 > 0 && checked.1 < checked.0, "sentinel: both answers occur ({checked:?})");
    }

    // ── JOBS-DST: slots in LOCAL CALENDAR terms, over both 2026 US ─────────
    // transitions (America/Los_Angeles: 2026-03-08 spring-forward, 2026-11-01
    // fall-back) and a zone with no DST (America/Phoenix, -7 h all year).
    //
    // THE ORACLE IS LITERAL. Each row is a zone, a target `at`, the first of
    // five consecutive LOCAL dates (days since 1970-01-01), and that `at`'s
    // instant on each of the five — written down from Python's `zoneinfo`
    // (`datetime(y, m, d, h, mi, tzinfo=ZoneInfo(zone))`, `fold=0`, which is
    // the policy [`resolve_local`] documents) when this table was authored,
    // never computed at test time and never by this crate. Neither the product
    // code nor `/bin/date` produces an expected value here.
    //
    // Every LA row's middle date is the transition day. The rows cover every
    // hour boundary of the night of each change, plus the ambiguous (01:xx on
    // Nov 1) and nonexistent (02:xx on Mar 8) cases and the far side of the
    // day (12:00, 23:00, 23:59), where "yesterday's slot" was wrong.
    #[rustfmt::skip]
    const SLOTS: &[(Zone, &str, &str, i64, [u64; 5])] = &[
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "00:00", 20518, [1772784000, 1772870400, 1772956800, 1773039600, 1773126000]), // 06T08:00Z, 07T08:00Z, 08T08:00Z, 09T07:00Z, 10T07:00Z
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "00:30", 20518, [1772785800, 1772872200, 1772958600, 1773041400, 1773127800]), // 06T08:30Z, 07T08:30Z, 08T08:30Z, 09T07:30Z, 10T07:30Z
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "01:00", 20518, [1772787600, 1772874000, 1772960400, 1773043200, 1773129600]), // 06T09:00Z, 07T09:00Z, 08T09:00Z, 09T08:00Z, 10T08:00Z
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "01:30", 20518, [1772789400, 1772875800, 1772962200, 1773045000, 1773131400]), // 06T09:30Z, 07T09:30Z, 08T09:30Z, 09T08:30Z, 10T08:30Z
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "01:59", 20518, [1772791140, 1772877540, 1772963940, 1773046740, 1773133140]), // 06T09:59Z, 07T09:59Z, 08T09:59Z, 09T08:59Z, 10T08:59Z
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "02:00", 20518, [1772791200, 1772877600, 1772964000, 1773046800, 1773133200]), // 06T10:00Z, 07T10:00Z, 08T10:00Z, 09T09:00Z, 10T09:00Z
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "02:30", 20518, [1772793000, 1772879400, 1772965800, 1773048600, 1773135000]), // 06T10:30Z, 07T10:30Z, 08T10:30Z, 09T09:30Z, 10T09:30Z
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "02:59", 20518, [1772794740, 1772881140, 1772967540, 1773050340, 1773136740]), // 06T10:59Z, 07T10:59Z, 08T10:59Z, 09T09:59Z, 10T09:59Z
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "03:00", 20518, [1772794800, 1772881200, 1772964000, 1773050400, 1773136800]), // 06T11:00Z, 07T11:00Z, 08T10:00Z, 09T10:00Z, 10T10:00Z
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "03:15", 20518, [1772795700, 1772882100, 1772964900, 1773051300, 1773137700]), // 06T11:15Z, 07T11:15Z, 08T10:15Z, 09T10:15Z, 10T10:15Z
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "12:00", 20518, [1772827200, 1772913600, 1772996400, 1773082800, 1773169200]), // 06T20:00Z, 07T20:00Z, 08T19:00Z, 09T19:00Z, 10T19:00Z
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "23:00", 20518, [1772866800, 1772953200, 1773036000, 1773122400, 1773208800]), // 07T07:00Z, 08T07:00Z, 09T06:00Z, 10T06:00Z, 11T06:00Z
        (Zone::UsPacific, "LA spring-forward (Mar 8)", "23:59", 20518, [1772870340, 1772956740, 1773039540, 1773125940, 1773212340]), // 07T07:59Z, 08T07:59Z, 09T06:59Z, 10T06:59Z, 11T06:59Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "00:00", 20756, [1793343600, 1793430000, 1793516400, 1793606400, 1793692800]), // 30T07:00Z, 31T07:00Z, 01T07:00Z, 02T08:00Z, 03T08:00Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "00:30", 20756, [1793345400, 1793431800, 1793518200, 1793608200, 1793694600]), // 30T07:30Z, 31T07:30Z, 01T07:30Z, 02T08:30Z, 03T08:30Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "01:00", 20756, [1793347200, 1793433600, 1793520000, 1793610000, 1793696400]), // 30T08:00Z, 31T08:00Z, 01T08:00Z, 02T09:00Z, 03T09:00Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "01:30", 20756, [1793349000, 1793435400, 1793521800, 1793611800, 1793698200]), // 30T08:30Z, 31T08:30Z, 01T08:30Z, 02T09:30Z, 03T09:30Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "01:59", 20756, [1793350740, 1793437140, 1793523540, 1793613540, 1793699940]), // 30T08:59Z, 31T08:59Z, 01T08:59Z, 02T09:59Z, 03T09:59Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "02:00", 20756, [1793350800, 1793437200, 1793527200, 1793613600, 1793700000]), // 30T09:00Z, 31T09:00Z, 01T10:00Z, 02T10:00Z, 03T10:00Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "02:30", 20756, [1793352600, 1793439000, 1793529000, 1793615400, 1793701800]), // 30T09:30Z, 31T09:30Z, 01T10:30Z, 02T10:30Z, 03T10:30Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "02:59", 20756, [1793354340, 1793440740, 1793530740, 1793617140, 1793703540]), // 30T09:59Z, 31T09:59Z, 01T10:59Z, 02T10:59Z, 03T10:59Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "03:00", 20756, [1793354400, 1793440800, 1793530800, 1793617200, 1793703600]), // 30T10:00Z, 31T10:00Z, 01T11:00Z, 02T11:00Z, 03T11:00Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "03:15", 20756, [1793355300, 1793441700, 1793531700, 1793618100, 1793704500]), // 30T10:15Z, 31T10:15Z, 01T11:15Z, 02T11:15Z, 03T11:15Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "12:00", 20756, [1793386800, 1793473200, 1793563200, 1793649600, 1793736000]), // 30T19:00Z, 31T19:00Z, 01T20:00Z, 02T20:00Z, 03T20:00Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "23:00", 20756, [1793426400, 1793512800, 1793602800, 1793689200, 1793775600]), // 31T06:00Z, 01T06:00Z, 02T07:00Z, 03T07:00Z, 04T07:00Z
        (Zone::UsPacific, "LA fall-back (Nov 1)", "23:59", 20756, [1793429940, 1793516340, 1793606340, 1793692740, 1793779140]), // 31T06:59Z, 01T06:59Z, 02T07:59Z, 03T07:59Z, 04T07:59Z
        (Zone::Fixed(-7 * 3600), "Phoenix (no DST), Mar", "01:30", 20518, [1772785800, 1772872200, 1772958600, 1773045000, 1773131400]), // 06T08:30Z, 07T08:30Z, 08T08:30Z, 09T08:30Z, 10T08:30Z
        (Zone::Fixed(-7 * 3600), "Phoenix (no DST), Mar", "02:30", 20518, [1772789400, 1772875800, 1772962200, 1773048600, 1773135000]), // 06T09:30Z, 07T09:30Z, 08T09:30Z, 09T09:30Z, 10T09:30Z
        (Zone::Fixed(-7 * 3600), "Phoenix (no DST), Mar", "23:00", 20518, [1772863200, 1772949600, 1773036000, 1773122400, 1773208800]), // 07T06:00Z, 08T06:00Z, 09T06:00Z, 10T06:00Z, 11T06:00Z
        (Zone::Fixed(-7 * 3600), "Phoenix (no DST), Nov", "01:30", 20756, [1793349000, 1793435400, 1793521800, 1793608200, 1793694600]), // 30T08:30Z, 31T08:30Z, 01T08:30Z, 02T08:30Z, 03T08:30Z
        (Zone::Fixed(-7 * 3600), "Phoenix (no DST), Nov", "02:30", 20756, [1793352600, 1793439000, 1793525400, 1793611800, 1793698200]), // 30T09:30Z, 31T09:30Z, 01T09:30Z, 02T09:30Z, 03T09:30Z
        (Zone::Fixed(-7 * 3600), "Phoenix (no DST), Nov", "23:00", 20756, [1793426400, 1793512800, 1793599200, 1793685600, 1793772000]), // 31T06:00Z, 01T06:00Z, 02T06:00Z, 03T06:00Z, 04T06:00Z
    ];

    /// The named cases, as ISO strings a reader can check by eye: an
    /// ambiguous local time is its FIRST occurrence, a nonexistent one is
    /// moved forward by the gap, and an ordinary one is untouched.
    #[test]
    fn jobs_dst_ambiguous_and_nonexistent_local_times_follow_the_documented_policy() {
        let c = Fake::utc(1_793_000_000, Zone::UsPacific);
        let (mar8, nov1, nov2) = (20_520, 20_758, 20_759);
        for (day, at, want, why) in [
            (nov1, hm(1, 30), "2026-11-01T08:30:00Z", "ambiguous 01:30 → the first, 01:30 PDT (not 09:30Z, 01:30 PST)"),
            (nov1, hm(1, 0), "2026-11-01T08:00:00Z", "ambiguous 01:00 → 01:00 PDT"),
            (nov1, hm(1, 59), "2026-11-01T08:59:00Z", "ambiguous 01:59 → 01:59 PDT"),
            (nov1, hm(2, 0), "2026-11-01T10:00:00Z", "02:00 on fall-back day happens once, PST"),
            (nov1, hm(0, 59), "2026-11-01T07:59:00Z", "00:59 PDT, before the change"),
            (nov2, hm(1, 30), "2026-11-02T09:30:00Z", "the day after: 01:30 PST, 25 h after the slot before"),
            (mar8, hm(2, 0), "2026-03-08T10:00:00Z", "nonexistent 02:00 → 03:00 PDT"),
            (mar8, hm(2, 30), "2026-03-08T10:30:00Z", "nonexistent 02:30 → 03:30 PDT (shifted by the 1 h gap)"),
            (mar8, hm(2, 59), "2026-03-08T10:59:00Z", "nonexistent 02:59 → 03:59 PDT"),
            (mar8, hm(1, 59), "2026-03-08T09:59:00Z", "01:59 PST, before the gap"),
            (mar8, hm(3, 0), "2026-03-08T10:00:00Z", "03:00 PDT, the first instant after the gap"),
        ] {
            assert_eq!(utc_iso(resolve_local(&c, day, at).unwrap()), want, "{why}");
        }
        // Sentinel: the fake zone really changes at the 2026 instants.
        let z = Zone::UsPacific;
        assert_eq!((z.offset_at(1_772_963_999), z.offset_at(1_772_964_000)), (-8 * 3600, -7 * 3600));
        assert_eq!((z.offset_at(1_793_523_599), z.offset_at(1_793_523_600)), (-7 * 3600, -8 * 3600));
    }

    /// Each row's five slots, resolved from their local dates, are the
    /// literal instants — and at EVERY quarter hour from the first slot to
    /// the last (four days, through the whole night of the change), the most
    /// recent slot is the latest literal at or before now and the next slot
    /// is the earliest after it. Forced red (JOBS-DST report): with the old
    /// wall-clock arithmetic restored this fails on both LA transitions.
    #[test]
    fn jobs_dst_the_most_recent_and_next_slot_at_every_quarter_hour_across_both_2026_transitions() {
        let mut checked = 0;
        for &(zone, label, at, day0, slots) in SLOTS {
            let target = env::hosting::parse_at(at).unwrap();
            assert!(slots.windows(2).all(|w| (82_800..=90_000).contains(&(w[1] - w[0]))), "{label} {at}: table");
            let c = Fake::utc(slots[0], zone);
            for (k, &want) in slots.iter().enumerate() {
                let got = resolve_local(&c, day0 + k as i64, target).unwrap();
                assert_eq!(got, want, "{label} {at}, local date +{k}: {} not {}", utc_iso(got), utc_iso(want));
            }
            let mut now = slots[0];
            while now < slots[4] {
                let c = Fake::utc(now, zone);
                let recent = *slots.iter().rev().find(|&&s| s <= now).unwrap();
                let next = *slots.iter().find(|&&s| s > now).unwrap();
                let got = most_recent_slot(&c, target).unwrap();
                assert_eq!(got, recent, "{label} {at} at {}: most recent slot {} not {}", utc_iso(now), utc_iso(got), utc_iso(recent));
                let got = next_slot(&c, target, None).unwrap();
                assert_eq!(got, next, "{label} {at} at {}: next slot {} not {}", utc_iso(now), utc_iso(got), utc_iso(next));
                // `status`'s view from a record three slots back: due, at the most recent.
                let rec = Record { last_slot: Some(recent - 3 * DAY), ..Record::default() };
                // (`next_slot_of` is `next_slot_view` without its `/bin/date` local string.)
                let got = next_slot_of(&sched(Some(at), 0, false), Some(&rec), &c).unwrap();
                assert_eq!(got, recent, "{label} {at} at {}: status's due slot {}", utc_iso(now), utc_iso(got));
                checked += 1;
                now += 900;
            }
        }
        assert!(checked > 10_000, "sentinel: {checked} instants checked");
    }

    /// The HOLDER over the same table: armed just after the first slot and
    /// polled every minute, awake, for four days, it starts exactly one pass
    /// per local date, each AT that date's literal slot — the ambiguous hour
    /// is not run twice and the skipped hour is not lost.
    #[test]
    fn jobs_dst_the_holder_runs_each_local_date_once_at_its_literal_slot_through_both_transitions() {
        for &(zone, label, at, _, slots) in SLOTS {
            let s = sched(Some(at), 0, false);
            let c = Fake::utc(slots[0] + 60, zone);
            let mut sc = Sched::new();
            let mut fired = vec![];
            while c.wall() <= slots[4] + 60 {
                if sc.due(&s, &c, None) {
                    fired.push(c.wall());
                    sc.started(&s, &c);
                    let wait = sc.ended(&s, &c, false, 0);
                    let next = slots.iter().find(|&&t| t > c.wall()).copied();
                    if let Some(next) = next {
                        assert_eq!(c.wall() + wait, next, "{label} {at}: the recorded next_in_secs");
                    }
                }
                c.awake(60);
            }
            assert_eq!(fired, slots[1..].to_vec(), "{label} {at}: passes started at the wrong instants");
        }
    }

    #[test]
    fn utc_iso_formats_known_instants() {
        assert_eq!(utc_iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_iso(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(utc_iso(1_792_161_000), "2026-10-16T14:30:00Z");
        assert_eq!(utc_iso(1_798_761_599), "2026-12-31T23:59:59Z");
    }

    // ── R5.S6: a holder restart is not a new load ─────────────────────────
    // soup-backup's shape: `every_secs = 300`, `run_at_load = true`. The
    // three opposed cases the brief names, plus the failed-pass exception.

    fn soup() -> Schedule {
        sched(None, 300, true)
    }

    fn rec(state: &str, last_start: u64) -> Record {
        Record { state: state.into(), last_start: Some(last_start), ..Default::default() }
    }

    /// Deploy 5's case: the old holder started a pass 14 s before the new
    /// holder's first look. Not run at load; run at `last_start + 300`, not
    /// a second early and not lost.
    #[test]
    fn r5s6_a_recent_pass_skips_the_load_run_and_resumes_at_last_start_plus_every() {
        let s = soup();
        let c = Fake::at(hm(12, 0));
        let prev = rec("ok", c.wall() - 14);
        let mut sc = Sched::new();
        assert!(!sc.due(&s, &c, Some(&prev)), "a pass 14 s old was re-run at load");
        let why = sc.load_skip.take().expect("the skip carries a reason for `supervisions`");
        assert!(why.contains("14s ago") && why.contains("every_secs = 300"), "{why}");
        c.awake(300 - 14 - 1);
        assert!(!sc.due(&s, &c, None), "due before last_start + 300");
        assert!(sc.load_skip.is_none(), "the skip is recorded once, not every poll");
        c.awake(1);
        assert!(sc.due(&s, &c, None), "the deferred pass was lost at last_start + 300");
    }

    /// A pass that started a full period ago (or more) is overdue: run at once.
    #[test]
    fn r5s6_a_stale_pass_runs_at_load() {
        let s = soup();
        for age in [300, 301, 7200] {
            let c = Fake::at(hm(12, 0));
            let mut sc = Sched::new();
            assert!(sc.due(&s, &c, Some(&rec("ok", c.wall() - age))), "a pass {age} s old was not run at load");
            assert!(sc.load_skip.is_none(), "a run at load recorded a skip");
        }
    }

    /// Never skip a job that has never run: no record, or one with no start
    /// (only ever blocked or refused).
    #[test]
    fn r5s6_a_job_with_no_recorded_run_runs_at_load() {
        let s = soup();
        let c = Fake::at(hm(12, 0));
        let mut sc = Sched::new();
        assert!(sc.due(&s, &c, None), "no record");
        assert!(sc.load_skip.is_none());
        for state in ["blocked", "refused"] {
            let mut sc = Sched::new();
            let r = Record { state: state.into(), ..Default::default() };
            assert!(sc.due(&s, &c, Some(&r)), "a `{state}` record with no last_start");
            assert!(sc.load_skip.is_none());
        }
    }

    /// A recent pass that FAILED was owed a retry by the old holder: run.
    /// And a start the wall clock puts in the future is not trusted: run.
    #[test]
    fn r5s6_a_recent_failed_pass_or_a_future_start_runs_at_load() {
        let s = soup();
        let c = Fake::at(hm(12, 0));
        let mut sc = Sched::new();
        assert!(sc.due(&s, &c, Some(&rec("failed", c.wall() - 14))), "a failed pass's retry was skipped");
        let mut sc = Sched::new();
        assert!(sc.due(&s, &c, Some(&rec("ok", c.wall() + 60))), "a start in the future suppressed the load run");
        // Opposed: the same recent start, state ok and running, IS skipped.
        for state in ["ok", "running"] {
            let mut sc = Sched::new();
            assert!(!sc.due(&s, &c, Some(&rec(state, c.wall() - 14))), "`{state}` 14 s ago was not skipped");
        }
    }

    /// `run_at_load = false` is untouched: it never runs at load and never
    /// records a skip, whatever the record says.
    #[test]
    fn r5s6_run_at_load_false_records_no_skip() {
        let s = sched(None, 300, false);
        let c = Fake::at(hm(12, 0));
        let mut sc = Sched::new();
        assert!(!sc.due(&s, &c, Some(&rec("ok", c.wall() - 14))));
        assert!(sc.load_skip.is_none(), "a skip recorded for a job that never runs at load");
    }

    // ── R5.S7: a `running` record whose holder died ───────────────────────
    // (R5.S6-1 c) The record is ASKED: alive and the same process → skipped;
    // gone or recycled → unfinished, run at load; no pid → R5.S6's skip.

    fn running(pid: Option<u32>, last_start: u64) -> Record {
        Record { state: "running".into(), pid, last_start: Some(last_start), ..Default::default() }
    }

    /// (a) The pass is still running: skipped, deferred to `last_start +
    /// 300` exactly as R5.S6 does, never a second concurrent pass. The probe
    /// is asked with the record's own pid and start.
    #[test]
    fn r5s7_a_running_record_whose_pass_is_alive_is_skipped() {
        let s = soup();
        let c = Fake::at(hm(12, 0));
        let mut sc = Sched::new();
        sc.probe = |pid, start| {
            assert_eq!((pid, start), (4242, Fake::at(hm(12, 0)).wall() - 14), "probe asked of the wrong pass");
            PassLife::Alive("pid 4242 is alive and started with the pass".into())
        };
        assert!(!sc.due(&s, &c, Some(&running(Some(4242), c.wall() - 14))), "a live pass was run a second time");
        let why = sc.load_skip.take().expect("a skip");
        assert!(why.contains("still running: pid 4242 is alive"), "{why}");
        assert!(sc.load_orphan.is_none(), "a live pass was recorded as orphaned");
        c.awake(300 - 14 - 1);
        assert!(!sc.due(&s, &c, None), "due before last_start + 300");
        c.awake(1);
        assert!(sc.due(&s, &c, None), "the deferred pass was lost");
    }

    /// (b), and (c) at the decision: the pass is gone (dead, or its pid now
    /// names another process) — it is unfinished, so it runs at load and the
    /// first look hands `tend` the orphan's `ran` to record. Opposed to (a):
    /// the same record, only the probe's answer differs.
    #[test]
    fn r5s7_a_running_record_whose_pass_is_gone_runs_at_load_and_says_why() {
        let s = soup();
        let c = Fake::at(hm(12, 0));
        for (probe, word) in [
            ((|_, _| PassLife::Gone("no process has pid 4242".into())) as fn(u32, u64) -> PassLife, "no process"),
            (|_, _| PassLife::Gone("pid 4242 is alive but is ANOTHER process".into()), "ANOTHER process"),
        ] {
            let mut sc = Sched::new();
            sc.probe = probe;
            assert!(sc.due(&s, &c, Some(&running(Some(4242), c.wall() - 14))), "{word}: a dead pass counted as recent");
            assert!(sc.load_skip.is_none(), "{word}: a run at load recorded a skip");
            let o = sc.load_orphan.take().expect("the orphan's `ran` reason");
            assert!(o.contains(word) && o.contains("UNFINISHED") && o.contains("pid 4242"), "{o}");
        }
    }

    /// (d) A `running` record with no pid cannot be asked: R5.S6's skip
    /// stands, and the probe is never called. A probe that cannot answer is
    /// read as alive (skip), never as dead.
    #[test]
    fn r5s7_a_running_record_with_no_pid_or_an_unanswered_probe_keeps_the_skip() {
        let s = soup();
        let c = Fake::at(hm(12, 0));
        let mut sc = Sched::new();
        sc.probe = |_, _| panic!("the probe was asked about a record with no pid");
        assert!(!sc.due(&s, &c, Some(&running(None, c.wall() - 14))), "a pid-less running record was run");
        let why = sc.load_skip.take().expect("a skip");
        assert!(why.contains("names no pid"), "{why}");
        assert!(sc.load_orphan.is_none());
        let mut sc = Sched::new();
        sc.probe = |_, _| PassLife::Unknown("`/bin/ps` could not be run".into());
        assert!(!sc.due(&s, &c, Some(&running(Some(4242), c.wall() - 14))), "an unanswered probe read as dead");
        assert!(sc.load_orphan.is_none());
        // Only `running` is asked: an `ok` record's pid is not probed.
        let mut sc = Sched::new();
        sc.probe = |_, _| panic!("an `ok` record was probed");
        let r = Record { pid: Some(4242), ..rec("ok", c.wall() - 14) };
        assert!(!sc.due(&s, &c, Some(&r)));
    }

    #[test]
    fn r5s7_parse_etime_reads_every_ps_shape() {
        assert_eq!(parse_etime("   00:07\n"), Some(7));
        assert_eq!(parse_etime("01:02:03"), Some(3723));
        assert_eq!(parse_etime("2-01:02:03"), Some(2 * 86_400 + 3723));
        assert_eq!(parse_etime(""), None);
        assert_eq!(parse_etime("7"), None);
    }

    /// The REAL identity check, on real processes (c): a live child probed
    /// with its own start is Alive; the SAME live pid probed with a start 60 s
    /// earlier — a record of a pass that began before this process existed,
    /// which is what a recycled pid looks like — is Gone; and after the child
    /// is reaped its pid is Gone.
    #[test]
    fn r5s7_probe_pass_tells_the_pass_from_a_recycled_pid_and_from_a_dead_one() {
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let start = epoch();
        let alive = probe_pass(pid, start);
        assert!(matches!(alive, PassLife::Alive(_)), "the pass itself was not recognised: {alive:?}");
        let recycled = probe_pass(pid, start - 60);
        assert!(
            matches!(&recycled, PassLife::Gone(d) if d.contains("recycled")),
            "a process that started 60 s after the recorded pass was taken for it: {recycled:?}"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        let dead = probe_pass(pid, start);
        assert!(matches!(&dead, PassLife::Gone(d) if d.contains("no process")), "a reaped pid read as alive: {dead:?}");
    }

    /// Inspector F1: a pass found orphaned whose re-run then failed to spawn
    /// (no `started`, so `next` stays `Unset`) is not probed and recorded
    /// again on the next poll — but it is still due.
    #[test]
    fn r5s7_an_orphan_is_recorded_once_even_if_its_rerun_never_starts() {
        let s = soup();
        let c = Fake::at(hm(12, 0));
        let prev = running(Some(4242), c.wall() - 14);
        let mut sc = Sched::new();
        sc.probe = |_, _| PassLife::Gone("no process has pid 4242".into());
        assert!(sc.due(&s, &c, Some(&prev)));
        assert!(sc.load_orphan.take().is_some(), "premise: the first look records the orphan");
        sc.probe = |_, _| panic!("the same orphan was probed again");
        c.awake(1);
        assert!(sc.due(&s, &c, Some(&prev)), "the orphan's run is still owed");
        assert!(sc.load_orphan.is_none(), "the orphan was recorded a second time");
        // Opposed: a DIFFERENT pass in the record is asked.
        sc.probe = |_, _| PassLife::Alive("pid 5151".into());
        assert!(!sc.due(&s, &c, Some(&running(Some(5151), c.wall() - 3))), "a new live pass was not asked");
    }

    /// Inspector F3: a `ps` that prints nothing because it FAILED does not
    /// make a live pass read as gone.
    #[test]
    fn r5s7_a_failing_ps_does_not_read_a_live_pass_as_gone() {
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let r = probe_pass_with("/usr/bin/false", pid, epoch());
        assert!(matches!(r, PassLife::Unknown(_)), "a failed `ps` read a live pass as: {r:?}");
        child.kill().unwrap();
        child.wait().unwrap();
        // Opposed: the real `ps` on the reaped pid is an answer.
        let r = probe_pass_with("/bin/ps", pid, epoch());
        assert!(matches!(&r, PassLife::Gone(d) if d.contains("no process")), "{r:?}");
    }
}
