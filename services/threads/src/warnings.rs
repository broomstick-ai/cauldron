//! # The daemon counts its own warnings, and says so out loud
//!
//! ## Why this module exists, and the number that forced it
//!
//! `~/.aui/threads-daemon.log` carried
//!
//! ```text
//! WARN thread lock released by Drop — a turn path exited without releasing
//! ```
//!
//! **29,644 times.** The warning was correct every single time. Nobody ever saw
//! one, because its only consumer was a 47 MB file with 165,281 `INFO` lines in
//! it. The operator's ruling, which is this module's whole specification:
//!
//! > *"reporting itself means it needs to send a warning to its consumer until
//! > we have centralized cauldron warning aggregation."*
//!
//! A producer that reports a problem into a log reports it to **nobody**. So the
//! count travels two ways from here, and the operator asked for both rather than
//! either:
//!
//! * **to the consumer**, as [`crate::shapes::warnings`] — a shape, because that
//!   is how a consumer learns a fact on this producer;
//! * **to disk**, as a warnings-ONLY record under `~/.cauldron/threads.warnings/`.
//!
//! ## Why a `tracing` layer and not 100 edited call sites
//!
//! There are over a hundred `warn!` and `error!` statements in this crate. The
//! alternative to a layer is a counter bump beside every one of them, which is a
//! hundred chances to forget one — and a counter that silently misses a call site
//! is worse than no counter, because it reads as *nothing is wrong*.
//!
//! A [`Layer`] sits under the subscriber and sees **every event that passes the
//! env filter**, including every one written before this file existed and every
//! one written after it, with no call site touched and none to keep in sync.
//! The cost of that choice is stated honestly in two places below: the filter
//! caveat, and the reentrancy hazard.
//!
//! **THE FILTER CAVEAT.** This layer counts what the subscriber DELIVERS. The
//! `EnvFilter` runs first, so `RUST_LOG=error` would hide `WARN` from this
//! counter exactly as it hides it from the log. That is the honest behaviour —
//! the counter agrees with the file beside it — but it means the count is
//! *"warnings this daemon emitted under its current filter"*, never *"warnings
//! this daemon would have emitted"*. The default filter is `info`, which passes
//! both levels.
//!
//! ## The reentrancy hazard, which is real and had to be designed against
//!
//! This layer runs INSIDE `warn!`. Its sink writes a file. `shapewire`'s
//! `ledger.rs` — the discipline this sink copies — reports its own write failures
//! with `tracing::warn!`, and that pattern here would be an **infinite
//! recursion**: a failed write warns, the warning re-enters the layer, the layer
//! writes, the write fails, …
//!
//! So two guards, and the second exists because the first is not enough:
//!
//! 1. **The sink never logs.** A write failure increments [`Tally::sink_errors`]
//!    and is carried to the consumer ON THE SHAPE. This is strictly better than
//!    a log line anyway, and for exactly this module's reason: a disk failure
//!    reported into the log it is failing to supplement is the original defect,
//!    recursive.
//! 2. **A thread-local guard** makes the sink a no-op while it is already inside
//!    itself on this thread. Guard 1 covers the code this module owns; guard 2
//!    covers anything the standard library, the allocator or a future edit does
//!    that guard 1 cannot see.
//!
//! **GUARD 2 IS DEFENCE IN DEPTH AND IS CURRENTLY UNREACHABLE, WHICH IS SAID HERE
//! RATHER THAN DISCOVERED LATER.** While guard 1 holds, nothing inside
//! [`write_record`] logs: `serde_json` and `std::fs` do not, and the record it
//! serialises holds captured `String`s, never a caller's `Debug` impl. So there is
//! no path from the sink back into the layer, and the first version of the test
//! for this claimed otherwise — it raised a warning from a `Debug` impl and passed
//! WITH THE GUARD DELETED, because that re-entry happens during field capture in
//! `on_event`, before `write_record` is ever reached, and terminates at depth 2 on
//! its own. A test that cannot fail is worse than no test, so it was replaced by
//! two that can:
//!
//! * `the_sink_refuses_to_re_enter_itself_on_the_same_thread` drives
//!   [`write_record`] directly with the flag already set — the only way to reach
//!   guard 2 at all — and is red when the early return is deleted;
//! * `the_sink_never_reports_its_own_failure_through_tracing` is the guard on
//!   guard 1, and it is the one that would actually catch the hazard coming back,
//!   because the hazard arrives as somebody adding a `tracing::warn!` to the write
//!   path.
//!
//! **AND `tracing` ITSELF ALREADY REFUSES RE-ENTRANT EVENTS — MEASURED, NOT
//! ASSUMED.** The `Debug`-impl probe was kept one revision longer as a test that a
//! warning raised while rendering another warning's field is counted. It is not:
//! the count came back `1`, not `2`. `tracing-core`'s dispatcher sets its
//! thread-local `can_enter` false for the duration of a `Subscriber::event` call,
//! so an event raised from inside one is dropped before any layer sees it.
//!
//! Two consequences, both worth writing down:
//!
//! * guard 2 is unreachable through a THIRD mechanism, upstream of both of ours,
//!   which is why it is defence in depth and nothing more;
//! * **a warning emitted while a warning is being rendered is silently lost, by
//!   `tracing`, everywhere in this daemon** — not by this module, not fixable here,
//!   and not counted. Reported rather than worked around: it is upstream policy,
//!   the events in question are pathological, and a workaround would mean
//!   bypassing the dispatcher.
//!
//! The test that asserted otherwise was deleted rather than weakened to match,
//! because a test whose whole content is *"the library we depend on behaves as it
//! behaves"* proves nothing about this code.
//!
//! ## Counts since BOOT, and why `counts_since` is not optional
//!
//! The counters are process-global and start at zero when the process does.
//! Since-ever would need persistence, a merge across restarts, and a decision
//! about what the `execv` self-update (which keeps the pid) means — all of which
//! is a different feature. Since-boot is simple and true.
//!
//! But *"a count that silently resets is a lie about 29,644"*, so the reset has
//! to be VISIBLE. The shape publishes `counts_since` on `shared`, and that number
//! is the DAEMON's start, deliberately not the shape's `epoch.init_at`: the shape
//! is `Lazy` and its epoch re-inits every time the last subscriber leaves, while
//! these counters keep climbing straight through. A consumer that read the epoch
//! as the counting window would under-report by every warning emitted while it
//! was not watching.
//!
//! ## The kind of a warning is its CALL SITE, not its message text
//!
//! A kind is `(target, line)`. Three reasons, and the third is measured:
//!
//! * it allocates nothing on the hot path — both halves are already `&'static`
//!   data on the event's metadata;
//! * it is stable under rewording, so a kind's history is not split in half the
//!   day somebody improves a sentence;
//! * **message text would shatter one kind into dozens.** The live log holds
//!   `update: A RESTART HAS BEEN PENDING FOR 10 MINUTES…`, `…FOR 15 MINUTES…`,
//!   `…FOR 85 MINUTES…` — one escalating `error!`, one call site, and **18
//!   distinct message strings** across 53 records. Keyed by text that is 18
//!   kinds and the top-N is all one defect; keyed by call site it is one kind
//!   with a count of 53, which is the sentence an operator wanted.
//!
//! The message is captured anyway, ONCE per call site, as the kind's human
//! label. The first one seen wins; a later rewording does not rewrite history.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

// ─────────────────────────────────────────────────────────────────────────────
//  WHERE THE DISK RECORD GOES
// ─────────────────────────────────────────────────────────────────────────────

/// **`~/.cauldron/threads.warnings/`** — `<service>.<kind>`, flat and dotted.
///
/// The operator's layout ruling for `~/.cauldron/`: a directory is named
/// `<service>.<kind>` so that `threads.*` shows everything one service owns and
/// `*.warnings` shows one kind across every service. Nothing is named after a
/// mechanism, which is why this is not `threads.tracing` or `threads.log`.
///
/// **Not `~/.aui/`**, where the 47 MB file that failed lives. These are the
/// DAEMON's warnings; `~/.aui/` is the AUI's home and the wrong owner. A reader
/// asking *"what has `threads` complained about"* should not have to know which
/// unrelated program happens to capture its stderr.
///
/// `shapewire::ledger::LEDGER_DIR` (`chassis`) is the pre-ruling name and a
/// rename of it is queued separately; this lands correct so it never needs moving
/// twice.
pub const WARNINGS_DIR: &str = "threads.warnings";

/// The file inside it. The directory already carries the service and the kind, so
/// the file name says only what the lines are.
pub const WARNINGS_FILE: &str = "warnings.jsonl";

/// **HOW MANY KINDS THE SHAPE PUBLISHES.** Five.
///
/// A bare total is enough to annoy him, which was the stated goal — but a total
/// that never breaks down leaves him reading the log to learn anything, which is
/// the thing being fixed. Five is the smallest number that answers *"what is it
/// mostly?"* against the real inventory: over the last seven quiet days the live
/// log holds **five** distinct `WARN` kinds in total, so five is the whole
/// steady-state truth, and during the burst the top one was 91% of everything.
///
/// The cost is stated in [`Tally::kinds`].
pub const TOP_KINDS: usize = 5;

/// **THE DISK BOUND — 4 MiB live, one rotated generation, 8 MiB total.**
///
/// Read straight off `shapewire::ledger` rather than re-chosen, because a second
/// number here would be a second policy to get wrong, and the operator asked for
/// that file's discipline specifically rather than a second rotation.
pub const WARNINGS_MAX_BYTES: u64 = shapewire::ledger::LEDGER_MAX_BYTES;

/// **ERRORS HAVE THEIR OWN FILE (WARN-1 D-E, census §4–§6.5).** `errors.jsonl`,
/// beside `warnings.jsonl` in the same directory. Until WARN-1 both levels
/// shared ONE rotation, so a warning storm (~5,000 lines/min in prod on
/// 2026-10-28) rotated every one of the daemon's 17 errors out of the only
/// record of them within minutes. A warning is never written here, so no
/// number of warnings can evict an error line.
pub const ERRORS_FILE: &str = "errors.jsonl";

/// The errors file's live bound: **1 MiB**, with [`ERRORS_GENERATIONS`]
/// generations kept in total (`errors.jsonl` plus `.1`, `.2`, `.3`), 4 MiB.
pub const ERRORS_MAX_BYTES: u64 = 1 << 20;

/// How many errors-file generations exist, the live one included.
pub const ERRORS_GENERATIONS: u32 = 4;

/// **ONE LINE PER EPISODE, A SUMMARY EVERY 60 s (WARN-1 D-E, census §6.5).**
/// A `WARN` with the same call site and the same message as one already
/// written within this window is not written again: it is counted, and the
/// episode writes ONE summary line carrying `repeats: n` at most once per
/// window while it lasts, and once more when it ends. The count on the shape
/// stays exact; the file's volume survives as the sum of `repeats`.
pub const EPISODE_WINDOW: Duration = Duration::from_secs(60);

/// **THE WRITE-RATE CAP (WARN-1 D-E, census §6.5): 100 lines per second per
/// call site.** Lines past it in the same second are not written; they are
/// counted and become ONE summary line (`suppressed: n`). That bounds a storm
/// of DISTINCT messages, which [`EPISODE_WINDOW`] cannot.
pub const RATE_CAP_PER_SEC: u32 = 100;

/// **THE `last_errors` RING (WARN-1 D-F):** the last 32 error records, on the
/// shape, so a face can name an error without reading a file.
pub const LAST_ERRORS: usize = 32;

/// A `last_errors` message or field value longer than this is cut (on a char
/// boundary): a label for a face, not the record — the record is the file.
pub const ERROR_TEXT_MAX: usize = 512;

/// Open episodes are bounded; past this the least recently seen one is settled
/// (its summary written) and dropped.
const MAX_EPISODES: usize = 1024;

/// An episode is keyed by call site and message; the message part of the key
/// is cut to this many bytes.
const EPISODE_KEY_MAX: usize = 256;

/// The sink's write policy. [`SinkPolicy::DEFAULT`] is production; a test
/// shrinks the bounds and the windows rather than writing megabytes or
/// sleeping a minute (doctrine 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkPolicy {
    /// `warnings.jsonl`'s live bound; one `.1` generation is kept.
    pub warnings_max_bytes: u64,
    /// `errors.jsonl`'s live bound.
    pub errors_max_bytes: u64,
    /// Errors-file generations, the live one included.
    pub errors_generations: u32,
    /// See [`EPISODE_WINDOW`].
    pub episode_window: Duration,
    /// See [`RATE_CAP_PER_SEC`].
    pub rate_cap_per_sec: u32,
}

impl SinkPolicy {
    pub const DEFAULT: SinkPolicy = SinkPolicy {
        warnings_max_bytes: WARNINGS_MAX_BYTES,
        errors_max_bytes: ERRORS_MAX_BYTES,
        errors_generations: ERRORS_GENERATIONS,
        episode_window: EPISODE_WINDOW,
        rate_cap_per_sec: RATE_CAP_PER_SEC,
    };
}

impl Default for SinkPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// `errors.jsonl` beside a warnings file.
pub fn errors_path_beside(warnings: &Path) -> PathBuf {
    warnings.with_file_name(ERRORS_FILE)
}

/// `~/.cauldron/threads.warnings/warnings.jsonl`, honouring `$CAULDRON_HOME`.
///
/// **The override is load-bearing and not a test convenience.** A sibling proved
/// today that the chassis ledger appended a dev daemon's records to the production
/// file *under production's own name*. `crate::consumers::cauldron_home` is the
/// one function that resolves this, so a dev daemon and every `cargo test` that
/// builds a manager land somewhere else by construction rather than by
/// remembering to.
///
/// `None` when neither `$CAULDRON_HOME` nor `$HOME` is set. The honest
/// consequence is that the disk half does not attach; the counter and the shape
/// are unaffected, and `sink_attached` on the shape says so rather than leaving a
/// reader to assume a file exists.
///
/// **PER ENVIRONMENT, except `prod` (R6.S3).** The root-level directory above is
/// shared by every environment in a cauldron root, so before R6.S3 a threads
/// daemon hosted in a second environment (`cauldron --env dev daemon`) appended
/// its warnings to PRODUCTION's file — measured as the one byte a side-by-side
/// environment wrote outside its own data root. A non-prod environment now
/// writes `data/<env>/threads/warnings/warnings.jsonl`, the decided
/// `data/<env>/<service>/<kind>/` layout, beside its consumer ledger. `prod`
/// keeps the root-level path byte for byte: the default environment's paths do
/// not move. An unusable environment name attaches no disk record at all
/// rather than guessing which environment's file to write.
pub fn warnings_path() -> Option<PathBuf> {
    warnings_path_for(
        &crate::consumers::cauldron_home()?,
        crate::consumers::cauldron_env().as_deref(),
    )
}

/// [`warnings_path`] with its two inputs explicit, so the rule is testable
/// without mutating the process environment. `env` is `None` when the name was
/// unusable.
pub fn warnings_path_for(home: &std::path::Path, env: Option<&str>) -> Option<PathBuf> {
    match env? {
        crate::consumers::DEFAULT_ENV => Some(home.join(WARNINGS_DIR).join(WARNINGS_FILE)),
        name => Some(
            home.join(crate::consumers::DATA_DIR)
                .join(name)
                .join("threads")
                .join("warnings")
                .join(WARNINGS_FILE),
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  WHAT A CONSUMER IS TOLD
// ─────────────────────────────────────────────────────────────────────────────

/// One kind of warning: a call site, its count, and the first message it wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Kind {
    /// `threads::runtime` — the module that warned.
    pub target: String,
    /// The source line. With `target`, this IS the kind — see the module doc.
    pub line: u32,
    /// `warn` or `error`. A call site has one level; it is carried so a reader of
    /// the breakdown can tell a defect from a condition without re-deriving it
    /// from the totals.
    pub level: &'static str,
    /// How many times this call site has fired since boot.
    pub count: u64,
    /// The FIRST message this call site wrote, as its label. Truncated to
    /// [`Kind::MESSAGE_MAX`] so one `warn!` with a 4 KB payload cannot set the
    /// size of every export.
    pub message: String,
}

impl Kind {
    /// A label, not a payload. Long enough for every real first clause in the
    /// live log; short enough that five of them are bounded at ~1 KB.
    pub const MESSAGE_MAX: usize = 160;
}

/// **THE WHOLE FACT, as a consumer receives it.**
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Tally {
    /// `WARN` events delivered since boot.
    pub warn: u64,
    /// `ERROR` events delivered since boot.
    ///
    /// **Counted apart, never folded into [`Tally::warn`].** An error is a
    /// different statement from a warning, and summing them in the producer would
    /// be the collapse `docs/shape-design-guide.md` rule 8 names: one number
    /// where two distinct facts exist, with no way back to *40 warnings* from *40
    /// errors*. A face that has room for only one number sums them itself
    /// ([`Tally::total`]) and can say which it did.
    pub error: u64,
    /// The [`TOP_KINDS`] busiest call sites, **descending by count**, ties broken
    /// by `(target, line)` so the order is deterministic and a consumer diffing
    /// two frames sees a change only when a count actually moved — **plus every
    /// `error` kind, whatever its rank** (WARN-1 D-F). The cut applies to warns
    /// only: on 2026-10-28 prod held 17 errors and the top five were all warns,
    /// so the shape could not say where a single error came from.
    ///
    /// **WHAT IT COSTS, stated rather than discovered.** Three things. (1) The hot
    /// path takes a mutex and a `BTreeMap` lookup instead of one atomic add — at
    /// the burst rate measured in the live log (0.6 events/second) that is
    /// nothing, and the map is bounded by the number of `warn!` statements in the
    /// crate, not by traffic. (2) The export grows by about 1 KB. (3) **This cell,
    /// not the totals, sets the frame rate**: a re-rank dirties the row even in
    /// the rare case where `warn` and `error` are unchanged. The deterministic
    /// tie-break is what keeps that from being a frame per sample forever.
    pub kinds: Vec<Kind>,
    /// **HOW MANY DISTINCT CALL SITES HAVE FIRED — the denominator [`Tally::kinds`]
    /// is a numerator of.**
    ///
    /// Without it, `kinds` truncated to [`TOP_KINDS`] is a silent cap, and
    /// *"these are the 5 kinds there are"* and *"these are 5 of 47"* arrive
    /// identical. `docs/shape-design-guide.md` step 8 says a read that hits its
    /// cap answers `Partial` and never `Complete`; a cell has no `Partial`, so the
    /// honest form is to carry the whole number beside the truncated list and let
    /// the consumer see the cap bite.
    ///
    /// **This is the one number on this shape that is NOT a count of warnings**,
    /// and it is the reason no ceiling here can drop one: `warn`, `error` and
    /// `sink_errors` are exact and uncapped, the map is bounded by the number of
    /// `warn!` statements in the crate rather than by traffic, and the only thing
    /// [`TOP_KINDS`] drops is EVIDENCE about which sites dominate — which this
    /// field makes visible rather than invisible.
    pub kinds_total: u64,
    /// Writes to the disk record that FAILED since boot.
    ///
    /// **This is why the sink does not log.** A disk failure reported into the log
    /// that the disk record exists to supplement is the original defect,
    /// recursive. It is reported to the CONSUMER instead, which is this module's
    /// entire thesis applied to its own failure.
    pub sink_errors: u64,
    /// Whether a disk record is attached at all. `false` means no path could be
    /// resolved, and is NOT the same statement as `sink_errors == 0`.
    pub sink_attached: bool,
    /// **THE LAST [`LAST_ERRORS`] ERROR RECORDS, oldest first (WARN-1 D-F).**
    /// Since boot, like the counts; the ring is in memory and resets with the
    /// process.
    pub last_errors: Vec<ErrorRecord>,
}

/// One `ERROR` event, as the `last_errors` ring holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorRecord {
    /// RFC 3339, when it was recorded.
    pub at: String,
    pub target: String,
    pub line: u32,
    /// Cut at [`ERROR_TEXT_MAX`].
    pub message: String,
    /// The event's own fields, each string value cut at [`ERROR_TEXT_MAX`].
    /// Named `event_fields`, not `fields`: the seam generator spells a cell's
    /// field list as a const `FIELDS`, and a field of that name collides.
    pub event_fields: Value,
}

impl Tally {
    /// `warn + error` — the ONE number a face shows when it has room for one.
    ///
    /// A method rather than a cell, because it is derived and a stored copy is a
    /// second way to know a thing (`docs/shapes.md` §2.4). The split stays on the
    /// wire; this is the face's convenience.
    pub fn total(&self) -> u64 {
        self.warn + self.error
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  THE COUNTER
// ─────────────────────────────────────────────────────────────────────────────

/// A call site: `(target, line)`. See the module doc for why not the message.
type CallSite = (&'static str, u32);

struct KindRow {
    count: u64,
    level: &'static str,
    message: String,
}

struct Sink {
    path: PathBuf,
    errors_path: PathBuf,
    policy: SinkPolicy,
}

/// An open run of identical warnings: one call site, one message.
struct Episode {
    level: &'static str,
    /// RFC 3339 of the line that opened it.
    started_at: String,
    last_seen: Instant,
    last_flush: Instant,
    /// Occurrences counted and not yet written as a summary.
    repeats: u64,
}

/// One call site's write-rate window.
struct Rate {
    level: &'static str,
    window_start: Instant,
    written: u32,
    suppressed: u64,
}

const RATE_WINDOW: Duration = Duration::from_secs(1);

struct State {
    kinds: BTreeMap<CallSite, KindRow>,
    sink: Option<Sink>,
    sink_errors: u64,
    episodes: BTreeMap<(CallSite, String), Episode>,
    rates: BTreeMap<CallSite, Rate>,
    last_errors: VecDeque<ErrorRecord>,
    last_sweep: Option<Instant>,
    /// A frozen clock for the unit tests (doctrine 4: a 60 s window is
    /// outlasted by moving this, not by sleeping).
    #[cfg(test)]
    test_now: Option<Instant>,
}

impl State {
    const fn new() -> Self {
        Self {
            kinds: BTreeMap::new(),
            sink: None,
            sink_errors: 0,
            episodes: BTreeMap::new(),
            rates: BTreeMap::new(),
            last_errors: VecDeque::new(),
            last_sweep: None,
            #[cfg(test)]
            test_now: None,
        }
    }

    #[cfg(test)]
    fn now(&self) -> Instant {
        self.test_now.unwrap_or_else(Instant::now)
    }

    #[cfg(not(test))]
    fn now(&self) -> Instant {
        Instant::now()
    }

    /// Write one line to the file its LEVEL owns: an error to `errors.jsonl`,
    /// anything else to `warnings.jsonl`. A failure is counted, never logged.
    fn write(&mut self, level: &'static str, record: &Value) {
        let Some(sink) = self.sink.as_ref() else { return };
        let out = if level == LEVEL_ERROR {
            write_record_gens(&sink.errors_path, sink.policy.errors_max_bytes, sink.policy.errors_generations, record)
        } else {
            write_record(&sink.path, sink.policy.warnings_max_bytes, record)
        };
        if out.is_err() {
            self.sink_errors += 1;
        }
    }

    /// **THE RATE GATE.** Whether one more line from `site` may be written in
    /// the current one-second window. A window that closed owing suppressed
    /// lines writes its summary first.
    fn admit(&mut self, site: CallSite, level: &'static str, now: Instant) -> bool {
        let Some(cap) = self.sink.as_ref().map(|s| s.policy.rate_cap_per_sec) else { return false };
        let r = self.rates.entry(site).or_insert(Rate { level, window_start: now, written: 0, suppressed: 0 });
        let mut owed = None;
        if now.duration_since(r.window_start) >= RATE_WINDOW {
            if r.suppressed > 0 {
                owed = Some((r.suppressed, r.window_start));
            }
            *r = Rate { level, window_start: now, written: 0, suppressed: 0 };
        }
        let ok = if r.written < cap {
            r.written += 1;
            true
        } else {
            r.suppressed += 1;
            false
        };
        if let Some((n, _)) = owed {
            self.write(level, &rate_summary(site, level, n, cap));
        }
        ok
    }

    /// Settle what is due: an episode whose window has passed writes its
    /// `repeats` summary, an ended episode that owes nothing is dropped, and a
    /// closed rate window writes its `suppressed` summary. `force` settles
    /// everything now (shutdown, and the agreement tests).
    fn flush_due(&mut self, now: Instant, force: bool) {
        let Some(policy) = self.sink.as_ref().map(|s| s.policy) else { return };
        let window = policy.episode_window;
        let mut out: Vec<(&'static str, Value)> = Vec::new();
        self.episodes.retain(|(site, msg), ep| {
            if ep.repeats > 0 && (force || now.duration_since(ep.last_flush) >= window) {
                out.push((ep.level, episode_summary(*site, ep.level, msg, &ep.started_at, ep.repeats)));
                ep.repeats = 0;
                ep.last_flush = now;
            }
            !(ep.repeats == 0 && (force || now.duration_since(ep.last_seen) > window))
        });
        for (site, r) in self.rates.iter_mut() {
            if r.suppressed > 0 && (force || now.duration_since(r.window_start) >= RATE_WINDOW) {
                out.push((r.level, rate_summary(*site, r.level, r.suppressed, policy.rate_cap_per_sec)));
                r.suppressed = 0;
            }
        }
        self.rates.retain(|_, r| r.suppressed > 0 || now.duration_since(r.window_start) < RATE_WINDOW);
        for (level, rec) in out {
            self.write(level, &rec);
        }
    }

    /// One `WARN`/`ERROR` event's disk half: episode, rate cap, then the line.
    fn sink_event(&mut self, site: CallSite, level: &'static str, at: &str, message: &str, record: &Value) {
        let Some(policy) = self.sink.as_ref().map(|s| s.policy) else { return };
        let now = self.now();
        if self.last_sweep.is_none_or(|t| now.duration_since(t) >= RATE_WINDOW) {
            self.flush_due(now, false);
            self.last_sweep = Some(now);
        }
        if level == LEVEL_ERROR {
            // Every error is its own line: errors are rare and each is
            // evidence. Only the rate cap applies, so an error storm is
            // bounded too.
            if self.admit(site, level, now) {
                self.write(level, record);
            }
            return;
        }
        let key = (site, truncate_on_char_boundary(message, EPISODE_KEY_MAX));
        let window = policy.episode_window;
        // Inside an open episode: count it; write the summary when one is due.
        let mut due: Option<Value> = None;
        let mut ended: Option<Value> = None;
        let mut inside = false;
        if let Some(ep) = self.episodes.get_mut(&key) {
            if now.duration_since(ep.last_seen) <= window {
                inside = true;
                ep.repeats += 1;
                ep.last_seen = now;
                if now.duration_since(ep.last_flush) >= window {
                    due = Some(episode_summary(site, level, &key.1, &ep.started_at, ep.repeats));
                    ep.repeats = 0;
                    ep.last_flush = now;
                }
            } else if ep.repeats > 0 {
                ended = Some(episode_summary(site, level, &key.1, &ep.started_at, ep.repeats));
            }
        }
        if inside {
            if let Some(rec) = due {
                self.write(level, &rec);
            }
            return;
        }
        // A NEW episode: settle the old one, then the start line, rate-capped.
        self.episodes.remove(&key);
        if let Some(rec) = ended {
            self.write(level, &rec);
        }
        if !self.admit(site, level, now) {
            return;
        }
        self.write(level, record);
        if self.episodes.len() >= MAX_EPISODES {
            let oldest = self.episodes.iter().min_by_key(|(_, e)| e.last_seen).map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                if let Some(e) = self.episodes.remove(&k) {
                    if e.repeats > 0 {
                        self.write(e.level, &episode_summary(k.0, e.level, &k.1, &e.started_at, e.repeats));
                    }
                }
            }
        }
        self.episodes.insert(key, Episode { level, started_at: at.to_string(), last_seen: now, last_flush: now, repeats: 0 });
    }
}

/// The line an episode writes for the occurrences it did not write.
fn episode_summary(site: CallSite, level: &'static str, message: &str, started_at: &str, repeats: u64) -> Value {
    json!({
        "at": crate::store::now_rfc3339(),
        "level": level,
        "target": site.0,
        "line": site.1,
        "message": message,
        "repeats": repeats,
        "episode_started": started_at,
    })
}

/// The line a closed rate window writes for the lines it refused.
fn rate_summary(site: CallSite, level: &'static str, suppressed: u64, cap: u32) -> Value {
    json!({
        "at": crate::store::now_rfc3339(),
        "level": level,
        "target": site.0,
        "line": site.1,
        "message": format!("write-rate cap: {suppressed} lines from this call site were not written (cap {cap}/s)"),
        "suppressed": suppressed,
        "rate_cap_per_sec": cap,
    })
}

/// A `last_errors` entry's fields: string values cut at [`ERROR_TEXT_MAX`].
fn cut_fields(fields: &Map<String, Value>) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|(k, v)| {
                let v = match v {
                    Value::String(s) => Value::String(truncate_on_char_boundary(s, ERROR_TEXT_MAX)),
                    other => other.clone(),
                };
                (k.clone(), v)
            })
            .collect(),
    )
}

/// **THE PROCESS-WIDE COUNTER.**
///
/// A `static` and not a field on `ThreadManager`, for one reason that is not
/// convenience: the thing doing the counting is a `tracing` layer installed on
/// the global subscriber before any manager exists, and `warn!` is called from
/// code that holds no manager — `main`, `update`, `store`, a `spawn_blocking`
/// closure. A counter reachable only through a manager would miss exactly the
/// early-boot and background warnings most likely to go unseen.
///
/// The totals are atomics so the shape's sampler reads them without taking the
/// lock the warn path uses.
pub struct Counter {
    warn: AtomicU64,
    error: AtomicU64,
    state: Mutex<State>,
}

/// The one instance.
pub static COUNTER: Counter = Counter::new();

thread_local! {
    /// **GUARD 2.** See the module doc's reentrancy section.
    static IN_SINK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Counter {
    const fn new() -> Self {
        Self {
            warn: AtomicU64::new(0),
            error: AtomicU64::new(0),
            state: Mutex::new(State::new()),
        }
    }

    /// Attach the disk record. Idempotent; the last call wins.
    ///
    /// **LAZY, like the ledger's.** The directory is not created and no file is
    /// opened until the first warning, so a daemon that never warns — and every
    /// unit test that merely installs the layer — writes nothing at all.
    ///
    /// `path` is `warnings.jsonl`; errors go to [`ERRORS_FILE`] beside it
    /// ([`errors_path_beside`]) under the default [`SinkPolicy`].
    pub fn attach_sink(&self, path: PathBuf, max_bytes: u64) {
        self.attach_sink_with(path, SinkPolicy { warnings_max_bytes: max_bytes, ..SinkPolicy::DEFAULT });
    }

    /// [`Counter::attach_sink`] with every bound and window named.
    pub fn attach_sink_with(&self, path: PathBuf, policy: SinkPolicy) {
        if let Ok(mut st) = self.state.lock() {
            let errors_path = errors_path_beside(&path);
            st.sink = Some(Sink { path, errors_path, policy });
            st.episodes.clear();
            st.rates.clear();
        }
    }

    /// **SETTLE EVERY OPEN EPISODE AND RATE WINDOW NOW**: write each one's
    /// summary line whatever its window. After this the disk agrees with the
    /// count: lines + Σ`repeats` + Σ`suppressed` = events written since attach.
    pub fn flush(&self) {
        if let Ok(mut st) = self.state.lock() {
            let now = st.now();
            st.flush_due(now, true);
        }
    }

    /// **READ THE WHOLE TALLY.** What the sampler and `threads/status` both call,
    /// so the shape and the legacy door cannot disagree about the same instant.
    pub fn tally(&self) -> Tally {
        let warn = self.warn.load(Ordering::Relaxed);
        let error = self.error.load(Ordering::Relaxed);
        let Ok(mut st) = self.state.lock() else {
            // A poisoned lock means a panic happened while counting. The totals
            // are still true (they are atomics), so report them rather than
            // nothing, and report an EMPTY breakdown rather than inventing one.
            return Tally {
                warn,
                error,
                kinds: Vec::new(),
                // Zero because the map is what was unreadable, exactly as the
                // breakdown is empty rather than invented. A reader sees an empty
                // list with a zero denominator, which is "I could not look", not
                // "there is nothing".
                kinds_total: 0,
                sink_errors: 0,
                sink_attached: false,
                last_errors: Vec::new(),
            };
        };
        let mut kinds: Vec<Kind> = st
            .kinds
            .iter()
            .map(|(&(target, line), row)| Kind {
                target: target.to_string(),
                line,
                level: row.level,
                count: row.count,
                message: row.message.clone(),
            })
            .collect();
        // Descending by count; `(target, line)` breaks ties, so the order is a
        // function of the data and not of the map's iteration luck.
        kinds.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.target.cmp(&b.target))
                .then_with(|| a.line.cmp(&b.line))
        });
        // **BEFORE the cut**, which is the whole point of the field.
        let kinds_total = kinds.len() as u64;
        // The top TOP_KINDS, and EVERY error kind past them (WARN-1 D-F): the
        // cut is for warns. The order is still descending by count.
        kinds = kinds
            .into_iter()
            .enumerate()
            .filter(|(i, k)| *i < TOP_KINDS || k.level == LEVEL_ERROR)
            .map(|(_, k)| k)
            .collect();
        // The sampler reads this once a second: settle any summary now due,
        // so an episode that went quiet is written without waiting for its
        // call site to fire again.
        if st.sink.is_some() {
            let now = st.now();
            st.flush_due(now, false);
        }
        Tally {
            warn,
            error,
            kinds,
            kinds_total,
            sink_errors: st.sink_errors,
            sink_attached: st.sink.is_some(),
            last_errors: st.last_errors.iter().cloned().collect(),
        }
    }

    /// Count one event and write it to disk. The layer's whole body.
    fn record(&self, site: CallSite, level: &'static str, file: Option<&str>, cap: Capture) {
        match level {
            "error" => self.error.fetch_add(1, Ordering::Relaxed),
            _ => self.warn.fetch_add(1, Ordering::Relaxed),
        };
        let Ok(mut st) = self.state.lock() else { return };
        let entry = st.kinds.entry(site).or_insert_with(|| KindRow {
            count: 0,
            level,
            // The FIRST message wins — see the module doc. Truncated on a char
            // boundary, because a `warn!` carrying a multi-byte character at byte
            // 160 would otherwise panic inside the logging path.
            message: truncate_on_char_boundary(&cap.message, Kind::MESSAGE_MAX),
        });
        entry.count += 1;
        // The level is read off the metadata every time rather than trusted from
        // the first insert, so a call site that changed level across a rebuild
        // cannot keep reporting the old one.
        entry.level = level;
        let at = crate::store::now_rfc3339();
        if level == LEVEL_ERROR {
            st.last_errors.push_back(ErrorRecord {
                at: at.clone(),
                target: site.0.to_string(),
                line: site.1,
                message: truncate_on_char_boundary(&cap.message, ERROR_TEXT_MAX),
                event_fields: cut_fields(&cap.fields),
            });
            while st.last_errors.len() > LAST_ERRORS {
                st.last_errors.pop_front();
            }
        }
        if st.sink.is_none() {
            return;
        }
        let record = json!({
            "at": at,
            "level": level,
            "target": site.0,
            "file": file,
            "line": site.1,
            "message": cap.message,
            "fields": Value::Object(cap.fields),
        });
        let message = record["message"].as_str().unwrap_or_default().to_string();
        st.sink_event(site, level, &at, &message, &record);
    }

    /// Test-only reset. **Not `pub`** beyond the crate: a counter an outside
    /// caller can zero is a counter whose history is deniable, which is the
    /// property this whole module exists to remove.
    #[cfg(test)]
    pub(crate) fn reset_for_test(&self) {
        self.warn.store(0, Ordering::Relaxed);
        self.error.store(0, Ordering::Relaxed);
        if let Ok(mut st) = self.state.lock() {
            st.kinds.clear();
            st.sink = None;
            st.sink_errors = 0;
            st.episodes.clear();
            st.rates.clear();
            st.last_errors.clear();
            st.last_sweep = None;
            st.test_now = None;
        }
    }

    /// Test-only: freeze (or with `None` release) the sink's clock.
    #[cfg(test)]
    pub(crate) fn set_now_for_test(&self, t: Option<Instant>) {
        if let Ok(mut st) = self.state.lock() {
            st.test_now = t;
        }
    }
}

fn truncate_on_char_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// **SUPERSEDED IN PART BY WARN-1 D-E (census §6.5):** identical warnings are
/// now one line per EPISODE plus a `repeats: n` summary at most every
/// [`EPISODE_WINDOW`] ([`State::sink_event`]). The operator's "size shows
/// volume" rule survives as the `repeats` sum, and the agreement test is the
/// sum form: lines + Σ`repeats` + Σ`suppressed` = count. The text below is the
/// original reasoning, kept because the rotation half of it still holds.
///
/// **ONE LINE PER WARNING, and no run-compression.** Two reasons, both the
/// operator's own.
///
/// `shapewire`'s ledger compresses a run of identical records into one line with a
/// count, and copying that here was the obvious move. It is wrong here:
///
/// * **The file's SIZE is the intended signal.** The instruction was that *"the
///   volume is obvious from the file size alone and no grep is needed to discover
///   a problem exists"*. Compressing 26,960 identical lines into one line saying
///   `repeats: 26960` hides precisely the volume it is supposed to show, in a
///   300-byte file.
/// * **It makes the agreement test unprovable in the form asked for.** The
///   obligation is *"emit N warnings, assert the count is N and the file holds N
///   lines"*. Under compression N warnings are one line and the test becomes a
///   sum over parsed records — a weaker check of a more complex invariant.
///
/// Bounded by rotation instead, which is what bounds it in the ledger too.
///
/// **Rename, never copy-and-truncate** — one syscall regardless of size, and no
/// instant at which a reader sees half a file. `shapewire::ledger` argues it at
/// length; this is the same policy against the same constant.
///
/// **Returns `Err` and NEVER logs.** The caller counts the failure; see the module
/// doc.
fn write_record(path: &Path, max_bytes: u64, record: &Value) -> std::io::Result<()> {
    write_record_gens(path, max_bytes, 1, record)
}

/// [`write_record`] keeping `generations` files in total (`path`, `.1` …
/// `.{generations-1}`); `generations <= 1` keeps the live file and `.1`, as the
/// warnings file always has.
fn write_record_gens(path: &Path, max_bytes: u64, generations: u32, record: &Value) -> std::io::Result<()> {
    IN_SINK.with(|g| {
        if g.get() {
            // Already inside the sink on this thread. Guard 2: do nothing at all
            // rather than recurse. Reported as success because there is no new
            // failure here — the outer call is the one that will report.
            return Ok(());
        }
        g.set(true);
        let out = (|| -> std::io::Result<()> {
            let mut line = serde_json::to_string(record)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            line.push('\n');
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            if len + line.len() as u64 > max_bytes {
                let gen = |n: u32| {
                    let mut p = path.to_path_buf().into_os_string();
                    p.push(format!(".{n}"));
                    PathBuf::from(p)
                };
                // Oldest first: `.{g-2}` -> `.{g-1}` … `.1` -> `.2`; the
                // oldest generation is overwritten by the rename into it.
                let last = generations.max(2) - 1;
                for n in (1..last).rev() {
                    let from = gen(n);
                    if from.exists() {
                        std::fs::rename(&from, gen(n + 1))?;
                    }
                }
                std::fs::rename(path, gen(1))?;
            }
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            f.write_all(line.as_bytes())
        })();
        g.set(false);
        out
    })
}

// ─────────────────────────────────────────────────────────────────────────────
//  THE LAYER
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Capture {
    message: String,
    fields: Map<String, Value>,
}

impl Visit for Capture {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        if field.name() == "message" {
            self.message = rendered;
        } else {
            self.fields
                .insert(field.name().to_string(), Value::String(rendered));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .insert(field.name().to_string(), Value::String(value.to_string()));
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.insert(field.name().to_string(), json!(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.insert(field.name().to_string(), json!(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.insert(field.name().to_string(), json!(value));
    }
}

/// **THE LAYER THAT MAKES EVERY `warn!` IN THIS CRATE COUNTABLE.**
///
/// Install it beside the `fmt` layer, never instead of it: the log file is still
/// the full record and this does not replace it. See this module's doc for why a
/// layer and not call sites.
/// `warn`, the level of a condition a reader should know about.
pub const LEVEL_WARN: &str = "warn";
/// `error`, the level of a defect.
pub const LEVEL_ERROR: &str = "error";

/// **THE CLOSED VOCABULARY OF [`Kind::level`], AND THE ONLY SPELLING OF IT.**
///
/// Declared here rather than in the shape so the words a consumer may compare
/// against and the words [`WarningCounter::on_event`] can actually produce are
/// one list (`docs/shape-design-guide.md` rule 7). `shapes/warnings.rs` declares
/// the cell's `Enum` from this const, so a third level could not reach the wire
/// without appearing here first.
pub const LEVELS: &[&str] = &[LEVEL_WARN, LEVEL_ERROR];

pub struct WarningCounter;

impl<S: Subscriber> Layer<S> for WarningCounter {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let md = event.metadata();
        let level = match *md.level() {
            Level::WARN => LEVEL_WARN,
            Level::ERROR => LEVEL_ERROR,
            // **INFO, DEBUG and TRACE ARE NOT COUNTED AND NOT WRITTEN.** The file
            // this feature exists because of failed precisely by carrying all
            // five levels: 165,281 `INFO` lines around 32,989 `WARN` ones. A
            // record a reader has to filter is the record we already have.
            _ => return,
        };
        if IN_SINK.with(|g| g.get()) {
            return;
        }
        let mut cap = Capture::default();
        event.record(&mut cap);
        COUNTER.record((md.target(), md.line().unwrap_or(0)), level, md.file(), cap);
    }
}

/// Resolve the path and attach the sink, once, at boot.
///
/// Separate from [`WarningCounter`] so the layer can be installed in a test
/// without a file anywhere, and so a daemon with no resolvable `$HOME` still
/// counts.
pub fn attach_disk_record() {
    if let Some(p) = warnings_path() {
        COUNTER.attach_sink(p, WARNINGS_MAX_BYTES);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  PROOFS
// ─────────────────────────────────────────────────────────────────────────────
//
// **EVERY TEST HERE WAS FORCED RED** by the mutation named in its own docstring,
// and the red text is quoted there. A green nobody has made fail is a decoration.
//
// They are SERIALISED by [`tests::GATE`], because [`COUNTER`] is a process-global
// and `cargo test` runs a binary's tests on many threads. Two tests resetting one
// static concurrently would be flaky in a way that reads as a real defect.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use tracing_subscriber::layer::SubscriberExt;

    /// Serialises every test that touches [`COUNTER`]. Not a lock the production
    /// path takes, and not an ordering requirement of the design.
    static GATE: StdMutex<()> = StdMutex::new(());

    /// Take [`GATE`], **recovering from poisoning rather than propagating it**.
    ///
    /// Learned the hard way one revision ago: one test failed its assertion while
    /// holding this lock and the other nine reported
    /// `called Result::unwrap() on an Err value: PoisonError` — nine red tests, one
    /// real defect, and the real one buried. A test harness that turns one failure
    /// into ten is actively misleading about which line is wrong.
    fn gate() -> std::sync::MutexGuard<'static, ()> {
        GATE.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Run `f` with the counting layer installed on THIS thread only, so a test
    /// never installs a global subscriber and tests cannot steal each other's
    /// events.
    fn counting<R>(f: impl FnOnce() -> R) -> R {
        let sub = tracing_subscriber::registry().with(WarningCounter);
        tracing::subscriber::with_default(sub, f)
    }

    /// How many lines the disk record holds. `0` when it does not exist, which is
    /// a real and distinct state from "exists and is empty".
    fn lines(path: &Path) -> usize {
        std::fs::read_to_string(path)
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    /// Events a record file accounts for: 1 per line, plus each summary's
    /// `repeats` and `suppressed` (WARN-1 D-E).
    fn disk_sum(path: &Path) -> u64 {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let v: Value = serde_json::from_str(l).expect("every line is JSON");
                if let Some(r) = v["repeats"].as_u64() {
                    r
                } else if let Some(n) = v["suppressed"].as_u64() {
                    n
                } else {
                    1
                }
            })
            .sum()
    }

    /// Every JSON line of every file in `dir`.
    fn all_lines(dir: &Path) -> Vec<Value> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            out.extend(text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()));
        }
        out
    }

    fn file_lines(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect()
    }

    // ── WARN-1 base-compatible proofs (begin) ──
    //
    // These use only API that existed at efbb02ee, so they were run RED
    // against efbb02ee's `warnings.rs` (reports: an internal report).

    /// **D-E: A WARNING STORM AT TWO SITES CANNOT EVICT AN ERROR LINE (two
    /// error sites).** The warnings file is bounded tiny (600 bytes) and
    /// stormed with 160 distinct lines from two call sites, so it rotates many
    /// times. Sentinel: the storm's first line IS gone from every file — the
    /// eviction was real. Claim: both errors are still on disk, in
    /// `errors.jsonl`, and no warning is in that file.
    #[test]
    fn a_warning_storm_at_two_sites_cannot_evict_an_error_line() {
        let _g = gate();
        COUNTER.reset_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(WARNINGS_FILE);
        COUNTER.attach_sink(path.clone(), 600);
        counting(|| {
            tracing::error!("deliberate error one");
            tracing::error!("deliberate error two");
            for i in 0..80 {
                tracing::warn!("storm a {}", i);
                tracing::warn!("storm b {}", i);
            }
        });
        let every = all_lines(tmp.path());
        let has = |m: &str| every.iter().any(|v| v["message"] == m);
        assert!(!has("storm a 0") && !has("storm b 0"), "sentinel: the storm did not evict anything, so this proves nothing");
        assert!(has("storm a 79") && has("storm b 79"), "sentinel: the storm's newest lines are on disk");
        assert!(
            has("deliberate error one") && has("deliberate error two"),
            "a warning storm evicted an error line: {} lines left on disk",
            every.len()
        );
        let errs = file_lines(&tmp.path().join("errors.jsonl"));
        assert_eq!(errs.len(), 2, "both errors, in errors.jsonl: {errs:?}");
        assert!(errs.iter().all(|v| v["level"] == "error"), "a warning reached errors.jsonl: {errs:?}");
    }

    /// **D-F: AN ERROR KIND PAST THE TOP-FIVE CUT IS STILL IN `kinds`.** Six
    /// warn sites (7..2) and one error site fired once, the smallest count.
    /// Opposed: the sixth warn site IS still cut, so the cut is for warns and
    /// not removed.
    #[test]
    fn an_error_kind_past_the_top_five_is_still_in_kinds() {
        let _g = gate();
        COUNTER.reset_for_test();
        counting(|| {
            for _ in 0..7 {
                tracing::warn!("site a");
            }
            for _ in 0..6 {
                tracing::warn!("site b");
            }
            for _ in 0..5 {
                tracing::warn!("site c");
            }
            for _ in 0..4 {
                tracing::warn!("site d");
            }
            for _ in 0..3 {
                tracing::warn!("site e");
            }
            for _ in 0..2 {
                tracing::warn!("site f");
            }
            tracing::error!("the one error");
        });
        let t = COUNTER.tally();
        let errors: Vec<&Kind> = t.kinds.iter().filter(|k| k.level == "error").collect();
        assert_eq!(errors.len(), 1, "the error kind was cut by the top-5: {:?}", t.kinds);
        assert_eq!(errors[0].message, "the one error");
        let warns: Vec<u64> = t.kinds.iter().filter(|k| k.level == "warn").map(|k| k.count).collect();
        assert_eq!(warns, vec![7, 6, 5, 4, 3], "the warn cut still holds: site f (2) is dropped");
        assert_eq!(t.kinds_total, 7);
    }

    /// **D-E: IDENTICAL WARNINGS AT TWO SITES ARE ONE LINE EACH INSIDE THE
    /// EPISODE WINDOW.** 50 identical at each of two call sites, written well
    /// inside 60 s. Opposed: the two sites are NOT merged into one line.
    #[test]
    fn identical_warnings_at_two_sites_write_one_line_each_inside_the_window() {
        let _g = gate();
        COUNTER.reset_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(WARNINGS_FILE);
        COUNTER.attach_sink(path.clone(), WARNINGS_MAX_BYTES);
        counting(|| {
            for _ in 0..50 {
                tracing::warn!("episode site one");
                tracing::warn!("episode site two");
            }
        });
        let ls = file_lines(&path);
        let one = ls.iter().filter(|v| v["message"] == "episode site one" && v.get("repeats").is_none()).count();
        let two = ls.iter().filter(|v| v["message"] == "episode site two" && v.get("repeats").is_none()).count();
        assert_eq!((one, two), (1, 1), "one start line per site inside the window, got {one} and {two}");
        assert_eq!(COUNTER.tally().warn, 100, "the COUNT stays exact");
    }

    /// **D-E: A STORM OF DISTINCT MESSAGES IS CAPPED AT 100 LINES PER SECOND
    /// PER SITE, N = 2 SITES.** 300 distinct lines from each of two sites in
    /// a tight loop (measured: well under a second). Opposed: each site gets
    /// its OWN 100 — a global cap would give the second site nothing.
    #[test]
    fn a_storm_of_distinct_messages_is_capped_per_site_n2() {
        let _g = gate();
        COUNTER.reset_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(WARNINGS_FILE);
        COUNTER.attach_sink(path.clone(), WARNINGS_MAX_BYTES);
        let t0 = std::time::Instant::now();
        counting(|| {
            for i in 0..300 {
                tracing::warn!("rate a {}", i);
                tracing::warn!("rate b {}", i);
            }
        });
        let secs = t0.elapsed().as_secs() + 1;
        let ls = file_lines(&path);
        let site = |p: &str| ls.iter().filter(|v| v["message"].as_str().is_some_and(|m| m.starts_with(p))).count() as u64;
        let (a, b) = (site("rate a "), site("rate b "));
        assert!(a <= 100 * secs && b <= 100 * secs, "the per-site cap did not hold over {secs}s: a={a} b={b}");
        assert!(a >= 100 && b >= 100, "each site gets its own 100: a={a} b={b}");
    }

    // ── WARN-1 base-compatible proofs (end) ──

    /// **D-E: AN EPISODE WRITES ONE `repeats` SUMMARY PER WINDOW, AND ENDS.**
    /// On a frozen clock (doctrine 4: the 60 s window is outlasted by moving
    /// the clock). 10 at t=0 (one line, 9 repeats), 5 at t=30 (no summary
    /// yet), one at t=61 (the window's summary, 14, is settled before it is
    /// counted), the sampler's read at t=200 settles the last one and ends
    /// the episode, so one more at t=200 is a new start line.
    #[test]
    fn an_episode_writes_a_repeats_summary_per_window_and_the_sum_agrees() {
        let _g = gate();
        COUNTER.reset_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(WARNINGS_FILE);
        COUNTER.attach_sink(path.clone(), WARNINGS_MAX_BYTES);
        let t0 = Instant::now();
        let at = |s: u64| COUNTER.set_now_for_test(Some(t0 + Duration::from_secs(s)));
        let warn = |n: usize| counting(|| (0..n).for_each(|_| tracing::warn!("one standing condition")));
        at(0);
        warn(10);
        at(30);
        warn(5);
        assert_eq!(lines(&path), 1, "no summary before the window has passed");
        at(61);
        warn(1);
        let ls = file_lines(&path);
        assert_eq!(ls.len(), 2, "{ls:?}");
        assert_eq!(ls[1]["repeats"], 14, "the summary carries every unwritten occurrence: {ls:?}");
        assert!(ls[1]["episode_started"].is_string());
        at(200);
        let _ = COUNTER.tally(); // the sampler's read settles the quiet episode
        let ls = file_lines(&path);
        assert_eq!(ls.len(), 3, "{ls:?}");
        assert_eq!(ls[2]["repeats"], 1, "the last window's one repeat, settled by the read: {ls:?}");
        warn(1);
        let ls = file_lines(&path);
        assert_eq!(ls.len(), 4, "a warning after the episode ended is a new start line: {ls:?}");
        assert!(ls[3].get("repeats").is_none());
        COUNTER.flush();
        assert_eq!(disk_sum(&path), 17, "lines + repeats = the count");
        assert_eq!(COUNTER.tally().warn, 17);
        COUNTER.set_now_for_test(None);
    }

    /// **D-E: THE RATE CAP'S SUMMARY, AND THE NEXT SECOND ADMITS AGAIN.** On a
    /// frozen clock: 250 distinct at one site in one second → 100 lines; the
    /// next second's first line writes the `suppressed: 150` summary and is
    /// itself admitted. Forced red by making `admit` return `true` always.
    #[test]
    fn the_rate_cap_writes_a_suppressed_summary_and_reopens_next_second() {
        let _g = gate();
        COUNTER.reset_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(WARNINGS_FILE);
        COUNTER.attach_sink(path.clone(), WARNINGS_MAX_BYTES);
        let t0 = Instant::now();
        COUNTER.set_now_for_test(Some(t0));
        counting(|| (0..250).for_each(|i| tracing::warn!("capped {}", i)));
        assert_eq!(lines(&path), 100, "the cap is 100 lines per second per site");
        COUNTER.set_now_for_test(Some(t0 + Duration::from_millis(1500)));
        counting(|| tracing::warn!("capped next"));
        let ls = file_lines(&path);
        assert_eq!(ls.len(), 102, "{}", ls.len());
        assert_eq!(ls[100]["suppressed"], 150, "{:?}", ls[100]);
        assert_eq!(ls[101]["message"], "capped next");
        assert_eq!(disk_sum(&path), 251);
        COUNTER.set_now_for_test(None);
    }

    /// **D-E: ERRORS ROTATE IN THEIR OWN FOUR GENERATIONS.** Forced red by
    /// writing errors through `write_record` (one `.1` generation).
    #[test]
    fn errors_rotate_in_their_own_four_generations() {
        let _g = gate();
        COUNTER.reset_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(WARNINGS_FILE);
        COUNTER.attach_sink_with(path.clone(), SinkPolicy { errors_max_bytes: 400, ..SinkPolicy::DEFAULT });
        counting(|| (0..60).for_each(|i| tracing::error!("an error long enough to rotate a small file {}", i)));
        let e = tmp.path().join(ERRORS_FILE);
        for g in [".1", ".2", ".3"] {
            assert!(tmp.path().join(format!("{ERRORS_FILE}{g}")).exists(), "generation {g} missing");
        }
        assert!(!tmp.path().join(format!("{ERRORS_FILE}.4")).exists(), "a fifth generation exists");
        assert!(std::fs::metadata(&e).unwrap().len() <= 400);
        assert!(!path.exists(), "an error was written to warnings.jsonl");
    }

    /// **D-F: `last_errors` IS A RING OF THE LAST 32 ERRORS, AND NO WARN.**
    /// Forced red by removing the `pop_front` loop (the ring grows to 40).
    #[test]
    fn last_errors_is_a_ring_of_the_last_32_errors() {
        let _g = gate();
        COUNTER.reset_for_test();
        counting(|| {
            for i in 0..40 {
                tracing::error!(n = i, "ring error {}", i);
                tracing::warn!("a warn is never in the ring {}", i);
            }
        });
        let t = COUNTER.tally();
        assert_eq!(t.last_errors.len(), LAST_ERRORS);
        assert_eq!(t.last_errors[0].message, "ring error 8", "oldest first, the first eight dropped");
        assert_eq!(t.last_errors[31].message, "ring error 39");
        assert_eq!(t.last_errors[31].event_fields["n"], 39);
        assert!(t.last_errors.iter().all(|e| e.message.starts_with("ring error")));
        assert!(t.last_errors[0].line > 0 && t.last_errors[0].target.contains("warnings"));
    }

    /// **ZERO READS AS ZERO, AND ONE WARNING MAKES IT ONE.** Both directions, in
    /// one test, because each alone is satisfiable by a stub: a counter hardwired
    /// to 0 passes the first and a counter hardwired to 1 passes the second.
    ///
    /// **THE MUTATION.** In [`Counter::record`], delete the
    /// `self.warn.fetch_add(1, Ordering::Relaxed)` arm.
    ///
    /// Forced red:
    /// ```text
    /// one WARN through the layer must make the count exactly 1, not 0
    /// ```
    #[test]
    fn zero_reads_as_zero_and_one_warning_makes_it_one() {
        let _g = gate();
        COUNTER.reset_for_test();

        let before = COUNTER.tally();
        assert_eq!(before.warn, 0, "a daemon that has not warned reads zero");
        assert_eq!(before.error, 0, "and has not errored either");
        assert!(
            before.kinds.is_empty(),
            "with no breakdown, rather than a breakdown of nothing"
        );

        counting(|| tracing::warn!(probe = 1, "a deliberate test warning"));

        let after = COUNTER.tally();
        assert_eq!(
            after.warn, 1,
            "one WARN through the layer must make the count exactly 1, not {}",
            after.warn
        );
        assert_eq!(after.error, 0, "and must not be counted as an error");
        assert_eq!(after.total(), 1, "the one number a face shows");
    }

    /// **AN ERROR IS COUNTED APART FROM A WARNING.** The collapse rule 8 names:
    /// one number where two distinct facts exist.
    ///
    /// **THE MUTATION.** In [`Counter::record`], make the `match level` arm fall
    /// through so `error` increments `self.warn`.
    ///
    /// Forced red:
    /// ```text
    /// an ERROR is not a WARN: warn=1 error=0
    /// ```
    #[test]
    fn an_error_is_counted_apart_from_a_warning() {
        let _g = gate();
        COUNTER.reset_for_test();
        counting(|| {
            tracing::error!("a deliberate test error");
        });
        let t = COUNTER.tally();
        assert_eq!(
            (t.warn, t.error),
            (0, 1),
            "an ERROR is not a WARN: warn={} error={}",
            t.warn,
            t.error
        );
        assert_eq!(t.total(), 1, "but a face summing them sees one problem");
    }

    /// **THE COUNT AND THE FILE AGREE — N WARNINGS, COUNT N, N LINES.**
    ///
    /// The operator's own obligation: *"If the count says 40 and the file holds 12,
    /// the counter is a decoration."* This is the test that makes both halves
    /// trustworthy, and it is the reason the sink writes one line per warning
    /// rather than compressing runs (see [`write_record`]).
    ///
    /// **WARN-1 D-E changed the form, not the obligation:** 40 identical
    /// warnings are now one episode line plus a `repeats` summary, so the
    /// agreement is the SUM — lines + Σ`repeats` + Σ`suppressed` = count —
    /// read after [`Counter::flush`]. Forced red by deleting the
    /// `ep.repeats += 1;` line in `State::sink_event`:
    /// ```text
    /// the count and the disk record must agree: count=40 file sum=1
    /// ```
    /// **R6.S3: a second environment's warnings never land in prod's file.**
    /// Opposed: prod's path is asserted unchanged (a constant per-env path
    /// fails it) and two non-prod environments are asserted apart from prod
    /// AND from each other (the old root-level constant fails it).
    #[test]
    fn warnings_are_per_environment_and_prod_keeps_its_path() {
        let home = std::path::Path::new("/r/.cauldron");
        let prod = warnings_path_for(home, Some("prod")).unwrap();
        assert_eq!(prod, home.join("threads.warnings").join("warnings.jsonl"), "prod's path moved");
        let a = warnings_path_for(home, Some("r6s3-proof")).unwrap();
        let b = warnings_path_for(home, Some("dev")).unwrap();
        assert_eq!(a, home.join("data/r6s3-proof/threads/warnings/warnings.jsonl"));
        assert_ne!(a, prod, "a non-prod environment wrote prod's warnings file");
        assert_ne!(a, b, "two environments share one warnings file");
        assert_eq!(warnings_path_for(home, None), None, "an unusable env name must attach nothing");
    }

    #[test]
    fn the_count_and_the_disk_record_agree_line_for_line() {
        let _g = gate();
        COUNTER.reset_for_test();
        let tmp = tempfile::tempdir().unwrap();
        // A nested dir that does NOT exist, so the lazy `create_dir_all` is
        // exercised rather than assumed.
        let path = tmp.path().join(WARNINGS_DIR).join(WARNINGS_FILE);
        assert_eq!(lines(&path), 0, "nothing is written before the first warning");
        COUNTER.attach_sink(path.clone(), WARNINGS_MAX_BYTES);

        const N: u64 = 40;
        counting(|| {
            for i in 0..N {
                tracing::warn!(i, "a deliberate test warning");
            }
        });

        COUNTER.flush();
        let t = COUNTER.tally();
        let got = disk_sum(&path);
        assert_eq!(
            (t.warn, got),
            (N, N),
            "the count and the disk record must agree: count={} file sum={}",
            t.warn,
            got
        );
        assert_eq!(lines(&path), 2, "one episode line and one `repeats` summary, not {N} lines");
        assert_eq!(t.sink_errors, 0, "and no write failed");
        assert!(t.sink_attached, "and the record is attached");

        // Every line is parseable JSON carrying the level and the message — a
        // record a reader cannot parse is a record that failed.
        let text = std::fs::read_to_string(&path).unwrap();
        for line in text.lines() {
            let v: Value = serde_json::from_str(line).expect("every line is JSON");
            assert_eq!(v["level"], "warn", "{line}");
            if v.get("repeats").is_some() {
                assert_eq!(v["repeats"], N - 1, "{line}");
            }
            assert_eq!(v["message"], "a deliberate test warning", "{line}");
            assert!(v["at"].is_string(), "{line}");
            assert!(v["target"].is_string(), "{line}");
        }
    }

    /// **NOTHING BELOW `WARN` REACHES THE COUNTER OR THE FILE.**
    ///
    /// Requirement 1: *"A file that also carries info and debug is the file we
    /// already have and it failed. If a reader has to filter, we have rebuilt the
    /// problem."* The live log is 165,281 `INFO` lines around 32,989 `WARN` ones,
    /// which is the whole reason nobody saw 29,644 of anything.
    ///
    /// **THE MUTATION.** In [`WarningCounter::on_event`], change the catch-all
    /// `_ => return` to `_ => "info"`.
    ///
    /// Forced red:
    /// ```text
    /// INFO, DEBUG and TRACE are not warnings: counted 3, wrote 3 lines
    /// ```
    #[test]
    fn nothing_below_warn_reaches_the_counter_or_the_file() {
        let _g = gate();
        COUNTER.reset_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(WARNINGS_FILE);
        COUNTER.attach_sink(path.clone(), WARNINGS_MAX_BYTES);

        counting(|| {
            tracing::info!("an info line");
            tracing::debug!("a debug line");
            tracing::trace!("a trace line");
        });

        let t = COUNTER.tally();
        assert_eq!(
            (t.warn + t.error, lines(&path)),
            (0, 0),
            "INFO, DEBUG and TRACE are not warnings: counted {}, wrote {} lines",
            t.warn + t.error,
            lines(&path)
        );

        // **THE POSITIVE CONTROL.** Without it this test passes against a layer
        // that counts nothing at all, which is the vacuous green the whole suite
        // is written against.
        counting(|| tracing::warn!("a warning, which IS counted"));
        assert_eq!(
            (COUNTER.tally().warn, lines(&path)),
            (1, 1),
            "the same sink DOES take a WARN, so the silence above is selectivity \
             and not a broken layer"
        );
    }

    /// **A KIND IS A CALL SITE, SO MANY MESSAGES FROM ONE `warn!` ARE ONE KIND.**
    ///
    /// Drawn from the live log: `update: A RESTART HAS BEEN PENDING FOR 10
    /// MINUTES…` through `…FOR 85 MINUTES…` is ONE `error!` with 18 distinct
    /// message strings across 53 records. Keyed by text that is 18 kinds and the
    /// top-5 is all one defect; keyed by call site it is one kind with a count of
    /// 53.
    ///
    /// **THE MUTATION.** In [`WarningCounter::on_event`], key by the message
    /// instead: build the `Capture` first and pass `(cap.message.leak(), 0)`.
    ///
    /// Forced red:
    /// ```text
    /// one warn! statement is ONE kind however its message varies: got 6 kinds
    /// ```
    #[test]
    fn a_kind_is_a_call_site_so_a_varying_message_is_still_one_kind() {
        let _g = gate();
        COUNTER.reset_for_test();
        counting(|| {
            for n in [10, 15, 20, 25, 30, 85] {
                // ONE call site, six different rendered messages — the live log's
                // own shape.
                tracing::warn!("A RESTART HAS BEEN PENDING FOR {} MINUTES", n);
            }
        });
        let t = COUNTER.tally();
        assert_eq!(
            t.kinds.len(),
            1,
            "one warn! statement is ONE kind however its message varies: got {} kinds",
            t.kinds.len()
        );
        assert_eq!(t.kinds[0].count, 6, "with every occurrence on it");
        assert_eq!(
            t.kinds[0].message, "A RESTART HAS BEEN PENDING FOR 10 MINUTES",
            "labelled by the FIRST message seen, so a rewording does not rewrite \
             history"
        );
        assert_eq!(t.kinds[0].level, "warn");
    }

    /// **THE BREAKDOWN IS RANKED BY COUNT AND BOUNDED AT [`TOP_KINDS`].**
    ///
    /// The bound is the point: a breakdown that grew with the number of `warn!`
    /// statements in the crate would put ~100 entries on every export.
    ///
    /// **THE MUTATION.** Delete the top-`TOP_KINDS` cut (WARN-1: a `filter`, was `kinds.truncate`) in
    /// [`Counter::tally`].
    ///
    /// Forced red:
    /// ```text
    /// the breakdown is bounded at 5: got 7
    /// ```
    #[test]
    fn the_breakdown_is_ranked_by_count_and_bounded() {
        let _g = gate();
        COUNTER.reset_for_test();
        // Seven call sites, deliberately more than TOP_KINDS, with descending
        // counts so the ranking is observable rather than incidental.
        counting(|| {
            for _ in 0..7 {
                tracing::warn!("site a");
            }
            for _ in 0..6 {
                tracing::warn!("site b");
            }
            for _ in 0..5 {
                tracing::warn!("site c");
            }
            for _ in 0..4 {
                tracing::warn!("site d");
            }
            for _ in 0..3 {
                tracing::warn!("site e");
            }
            for _ in 0..2 {
                tracing::warn!("site f");
            }
            tracing::warn!("site g");
        });
        let t = COUNTER.tally();
        assert_eq!(
            t.kinds.len(),
            TOP_KINDS,
            "the breakdown is bounded at {}: got {}",
            TOP_KINDS,
            t.kinds.len()
        );
        let counts: Vec<u64> = t.kinds.iter().map(|k| k.count).collect();
        assert_eq!(
            counts,
            vec![7, 6, 5, 4, 3],
            "descending by count, and the two smallest kinds are the ones dropped"
        );
        assert_eq!(t.warn, 28, "while the TOTAL still counts every one of them");
    }

    /// **THE CAP ON THE BREAKDOWN IS VISIBLE, NOT SILENT** — [`Tally::kinds_total`]
    /// is the denominator the truncated [`Tally::kinds`] is a numerator of.
    ///
    /// This is the one place on this shape where information is dropped at a
    /// ceiling, and a shape whose whole job is to stop information going unread
    /// may not drop it quietly. Without the denominator, *"these are the 5 kinds
    /// there are"* and *"these are 5 of 47"* arrive as the same frame — and the
    /// breakdown is the half that says WHAT to fix, so a reader who cannot tell
    /// those apart cannot tell whether he has seen the problem.
    ///
    /// The totals are untouched by any cap ([`the_breakdown_is_ranked_by_count_and_bounded`]
    /// asserts `warn == 28` here), so no WARNING is ever dropped; what the cap
    /// drops is evidence, and this number is what makes the drop legible.
    ///
    /// **THE MUTATION.** Move `let kinds_total = kinds.len() as u64;` below the
    /// top-`TOP_KINDS` cut (WARN-1: a `filter`, was `kinds.truncate`) in [`Counter::tally`].
    ///
    /// Forced red:
    /// ```text
    /// seven call sites fired, so the denominator is 7 and not the capped 5: got 5
    /// ```
    #[test]
    fn the_truncated_breakdown_carries_the_whole_denominator() {
        let _g = gate();
        COUNTER.reset_for_test();
        // Seven distinct call sites again, two more than TOP_KINDS, so the cap
        // bites and the denominator has something to disagree with.
        counting(|| {
            tracing::warn!("site a");
            tracing::warn!("site b");
            tracing::warn!("site c");
            tracing::warn!("site d");
            tracing::warn!("site e");
            tracing::warn!("site f");
            tracing::warn!("site g");
        });
        let t = COUNTER.tally();
        // The positive control beside the claim: the cap really did bite here. A
        // fixture that fired five or fewer sites would pass this test with the
        // denominator computed on either side of the truncate.
        assert_eq!(
            t.kinds.len(),
            TOP_KINDS,
            "the positive control: the cap must actually bite, or this test cannot fail"
        );
        assert_eq!(
            t.kinds_total, 7,
            "seven call sites fired, so the denominator is 7 and not the capped {}: got {}",
            TOP_KINDS, t.kinds_total
        );
        assert!(
            t.kinds_total > t.kinds.len() as u64,
            "and the consumer can SEE the cap bit, which is the whole point"
        );
    }

    /// **THE RECORD ROTATES RATHER THAN GROWING WITHOUT BOUND.**
    ///
    /// Requirement 3: *"An unbounded warnings file on a machine at 19 GiB free is
    /// its own incident."* Proven at a tiny `max_bytes` rather than at 4 MiB,
    /// because forcing this one red at the production constant would mean writing
    /// 4 MiB — `shapewire::ledger`'s own reasoning for making the threshold a
    /// field.
    ///
    /// **THE MUTATION.** Delete the `std::fs::rename` block in [`write_record`].
    ///
    /// Forced red:
    /// ```text
    /// the live file is bounded by the threshold: 2104 bytes > 600
    /// ```
    #[test]
    fn the_record_rotates_rather_than_growing_without_bound() {
        let _g = gate();
        COUNTER.reset_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(WARNINGS_FILE);
        const MAX: u64 = 600;
        COUNTER.attach_sink(path.clone(), MAX);

        // DISTINCT messages (WARN-1: identical ones are one episode line).
        counting(|| {
            for i in 0..40 {
                tracing::warn!("a warning long enough to cross a small threshold {}", i);
            }
        });

        let live = std::fs::metadata(&path).unwrap().len();
        assert!(
            live <= MAX,
            "the live file is bounded by the threshold: {live} bytes > {MAX}"
        );
        let rotated = tmp.path().join(format!("{WARNINGS_FILE}.1"));
        assert!(
            rotated.exists(),
            "and the previous generation is kept rather than discarded"
        );
        let rot = std::fs::metadata(&rotated).unwrap().len();
        assert!(
            live + rot <= 2 * MAX,
            "with the TOTAL bounded at twice the threshold: {live} + {rot}"
        );
        // The count is unaffected by rotation: the file is a record, not the
        // counter. 40 warnings counted even though the file can no longer show
        // all 40 — which is exactly why the count travels separately.
        assert_eq!(
            COUNTER.tally().warn,
            40,
            "rotation bounds the FILE and never the COUNT"
        );
    }

    /// **A WRITE FAILURE NEVER FAILS THE THING THAT WARNED, AND IS REPORTED TO THE
    /// CONSUMER RATHER THAN TO THE LOG.**
    ///
    /// Requirement 4, plus the reentrancy design. The failure is forced by pointing
    /// the sink at a path whose PARENT is a regular file, so `create_dir_all`
    /// cannot succeed however the directory is spelled.
    ///
    /// **THE MUTATION.** In [`Counter::record`], replace the
    /// `if write_record(..).is_err() { st.sink_errors += 1; }` with
    /// `write_record(..).unwrap();`.
    ///
    /// Forced red: the test panics inside `tracing::warn!` with
    /// ```text
    /// called `Result::unwrap()` on an `Err` value:
    /// Os { code: 17, kind: AlreadyExists, message: "File exists" }
    /// ```
    /// (`create_dir_all` reports `AlreadyExists` when a path component is a
    /// regular file, not `NotADirectory` — the red text is what it actually said,
    /// not what was predicted.)
    #[test]
    fn a_write_failure_is_counted_and_never_propagates() {
        let _g = gate();
        COUNTER.reset_for_test();
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("not-a-dir");
        std::fs::write(&blocker, b"I am a file").unwrap();
        COUNTER.attach_sink(blocker.join(WARNINGS_FILE), WARNINGS_MAX_BYTES);

        // If this panicked or propagated, the test would not reach the assertions.
        counting(|| tracing::warn!("a warning whose record cannot be written"));

        let t = COUNTER.tally();
        assert_eq!(
            t.warn, 1,
            "the warning is still COUNTED when its record cannot be written — the \
             disk half failing must not take the live half with it"
        );
        assert_eq!(
            t.sink_errors, 1,
            "and the failure is carried to the CONSUMER, because it cannot be \
             logged without recursing"
        );
        assert!(
            t.sink_attached,
            "attached-and-failing is a different state from never-attached"
        );
    }

    /// **THE SINK REFUSES TO RE-ENTER ITSELF ON THE SAME THREAD** — guard 2,
    /// tested through the only door that reaches it.
    ///
    /// White-box deliberately: it sets the thread-local and calls
    /// [`write_record`]. The earlier version of this test went through
    /// `tracing::warn!` with a `Debug` impl that warns, and **passed with the
    /// guard deleted** — that re-entry lands in `on_event`'s field capture, not in
    /// the sink, and unwinds at depth 2 by itself. See the module doc.
    ///
    /// **THE MUTATION.** Delete the `if g.get() { return Ok(()); }` early return in
    /// [`write_record`].
    ///
    /// Forced red:
    /// ```text
    /// a re-entrant write must do NOTHING, not append: 1 lines
    /// ```
    #[test]
    fn the_sink_refuses_to_re_enter_itself_on_the_same_thread() {
        let _g = gate();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(WARNINGS_FILE);

        // The POSITIVE CONTROL first: with the flag clear, this same call writes.
        // Without it the assertion below passes against a `write_record` that
        // never writes anything at all.
        write_record(&path, WARNINGS_MAX_BYTES, &json!({"probe": "outer"})).unwrap();
        assert_eq!(lines(&path), 1, "the control write landed");

        IN_SINK.with(|g| g.set(true));
        let out = write_record(&path, WARNINGS_MAX_BYTES, &json!({"probe": "nested"}));
        IN_SINK.with(|g| g.set(false));

        assert!(out.is_ok(), "re-entry is not an error, it is a no-op");
        assert_eq!(
            lines(&path),
            1,
            "a re-entrant write must do NOTHING, not append: {} lines",
            lines(&path)
        );
    }

    /// **THE SINK NEVER REPORTS ITS OWN FAILURE THROUGH `tracing`** — guard 1, and
    /// the guard that would actually catch the hazard returning.
    ///
    /// `shapewire::ledger::write_line` ends with
    /// `tracing::warn!(.., "consumer ledger: write failed")`. That is correct there
    /// and fatal here: this write path runs INSIDE `warn!`. The hazard comes back
    /// the day somebody copies that line, so the ban is on the source text.
    ///
    /// **IT DOES NOT WITNESS ITSELF.** The scan covers `warnings.rs` ONLY UP TO the
    /// `mod tests` line, so this test's own prose — which necessarily contains the
    /// banned strings — is outside the scanned region. Proven by the assertion that
    /// the banned text IS present in the file as a whole.
    ///
    /// **IT IS NOT VACUOUS**, and the positive control is the real file whose
    /// pattern is being refused: the same scanner run over
    /// `lib/shapewire/src/ledger.rs` must FIND a logging call. A ban on something
    /// absent is a sentence that can never fail.
    ///
    /// **THE MUTATION.** Add `tracing::warn!("sink: write failed");` to
    /// [`Counter::record`]'s error arm.
    ///
    /// Forced red:
    /// ```text
    /// the sink must never log -- it runs inside warn!. Found in warnings.rs
    /// (pre-test region): ["tracing::warn!(\"sink: write failed\");"]
    /// ```
    #[test]
    fn the_sink_never_reports_its_own_failure_through_tracing() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/warnings.rs");
        let whole = std::fs::read_to_string(&src).expect("the module reads back");

        // Exclude the test module: it is allowed to talk about logging, and a scan
        // that read its own docstrings would be unfalsifiable.
        let cut = whole
            .find("\nmod tests {")
            .or_else(|| whole.find("\n#[cfg(test)]\nmod tests {"))
            .unwrap_or_else(|| panic!("the test module marker must exist to be excluded"));
        let production = &whole[..cut];

        let scan = |text: &str| -> Vec<String> {
            text.lines()
                .map(str::trim)
                .filter(|l| !l.starts_with("//"))
                .filter(|l| {
                    l.contains("tracing::warn!")
                        || l.contains("tracing::error!")
                        || l.contains("tracing::info!")
                })
                .map(str::to_string)
                .collect()
        };

        let found = scan(production);
        assert!(
            found.is_empty(),
            "the sink must never log -- it runs inside warn!. Found in warnings.rs \
             (pre-test region): {found:?}"
        );

        // ── NON-VACUITY. The scanner must be able to find the thing. ──
        let ledger = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../libs/shapewire/src/ledger.rs");
        let ledger_src = std::fs::read_to_string(&ledger).expect("the ledger reads back");
        let control = scan(&ledger_src);
        assert!(
            !control.is_empty(),
            "NON-VACUITY FAILED: the same scanner found no logging call in \
             shapewire/src/ledger.rs, so the ban above proves nothing"
        );

        // And the scanner would find it in THIS file too, if it were in the
        // scanned region: the strings exist in the excluded part.
        assert!(
            !scan(&whole).is_empty(),
            "NON-VACUITY FAILED: the banned text is absent from warnings.rs \
             entirely, so the exclusion is doing the work rather than the code"
        );
    }

    /// **A MESSAGE IS TRUNCATED ON A CHARACTER BOUNDARY, NOT A BYTE ONE.**
    ///
    /// A `warn!` carrying a multi-byte character astride [`Kind::MESSAGE_MAX`]
    /// would otherwise panic inside the logging path, which is the worst possible
    /// place for this module to panic.
    ///
    /// **THE MUTATION.** Replace [`truncate_on_char_boundary`]'s body with
    /// `s[..max.min(s.len())].to_string()`.
    ///
    /// Forced red: a panic rather than an assertion:
    /// ```text
    /// byte index 160 is not a char boundary; it is inside 'é' (bytes 159..161)
    /// ```
    #[test]
    fn a_long_message_is_truncated_on_a_char_boundary() {
        let _g = gate();
        // 159 ASCII bytes then a 2-byte char straddling byte 160.
        let s = format!("{}é{}", "a".repeat(159), "b".repeat(50));
        assert!(!s.is_char_boundary(Kind::MESSAGE_MAX), "the fixture straddles");
        let got = truncate_on_char_boundary(&s, Kind::MESSAGE_MAX);
        assert_eq!(got.len(), 159, "it backs up to the boundary: {}", got.len());
        assert!(got.is_char_boundary(got.len()));
    }
}
