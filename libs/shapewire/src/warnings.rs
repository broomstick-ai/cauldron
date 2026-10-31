//! **The warnings a producer owes cauldron, counted by CALL SITE.**
//!
//! Serving a warnings shape is part of what it means to be a cauldron service:
//! `env::seams::ServiceEnv::register` takes an `env::warnings::Attestation`,
//! which only `env::warnings::attest_warnings!` can mint, and that only for a
//! type implementing `env::warnings::ServesWarnings`. The CONTRACT (the
//! `Export` type, the two supervisor words, the attestation) is `env`'s. What
//! is here is the half every producer would otherwise write again: the counter
//! the `ServesWarnings::warnings` body reads.
//!
//! This is `services/tts/src/seam.rs`'s counter made a library type, with one
//! deliberate change: tts keeps ONE process-global ledger, so its own unit
//! test can only read deltas. Here a [`Counter`] is a value. A producer keeps
//! one in a `static` ([`COUNTER`] is offered); a test makes its own and reads
//! absolute numbers.
//!
//! # The rules, carried from tts
//!
//! * Counted by CALL SITE (`file`, `line`), not by message: one defect with
//!   eighteen message strings is one kind, not eighteen.
//! * `warn` and `error` COUNTED APART.
//! * The durable record (`<warnings dir>/warnings.jsonl`) is attached lazily,
//!   and a failed write to it is a COUNTED FIELD (`sink_errors`), never a log
//!   line — a failed write to the warnings record reported through the warning
//!   path is the original defect with recursion added.
//!
//! # No `tracing-subscriber`
//!
//! A subscriber is the BINARY's choice (see `Cargo.toml`), so this module
//! offers [`Counter::record_event`], which takes the `tracing::Event` a layer is
//! handed. The producer's own four-line `Layer` calls it:
//!
//! ```ignore
//! impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for MyLayer {
//!     fn on_event(&self, e: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
//!         shapewire::warnings::COUNTER.record_event(e);
//!     }
//! }
//! ```

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use env::warnings::{Export, Kind};

/// How many call sites an export carries. tts's figure.
pub const TOP_KINDS: usize = 5;

/// The durable record's file name, under the directory [`Counter::attach_sink`]
/// is given.
pub const SINK_FILE: &str = "warnings.jsonl";

/// **THE DISK BOUND (DG-3): 4 MiB live, one rotated `.1`, 8 MiB total.**
///
/// The same discipline as threads' own warnings record
/// (`services/threads/src/warnings.rs`, `WARNINGS_MAX_BYTES`), and read off
/// [`crate::ledger::LEDGER_MAX_BYTES`] rather than re-chosen, so there is one
/// number and not two. A write that would carry the live file past this is
/// preceded by a rename of the live file to `<sink>.1`, replacing whatever `.1`
/// held — the `.1`-rename pattern of `ledger.rs`'s rotation.
pub const SINK_MAX_BYTES: u64 = crate::ledger::LEDGER_MAX_BYTES;

/// Append one line to a size-bounded record, rotating first when the line would
/// carry the live file past `max_bytes`: rename `path` to `path.1` (dropping the
/// old `.1`), then append to a fresh `path`. A rename and never a
/// copy-and-truncate, so no reader ever sees half a file, and the newest line
/// is always in the live file. The total on disk stays under `2 * max_bytes`
/// (plus one line).
pub fn append_bounded(path: &Path, max_bytes: u64, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let held = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let incoming = line.len() as u64 + 1;
    if held > 0 && held + incoming > max_bytes {
        let mut rotated = path.as_os_str().to_os_string();
        rotated.push(".1");
        std::fs::rename(path, PathBuf::from(rotated))?;
    }
    writeln!(std::fs::OpenOptions::new().create(true).append(true).open(path)?, "{line}")
}

/// A label is the first message a site wrote, truncated to this many chars.
pub const LABEL_CHARS: usize = 160;

/// The two levels that count. Anything below WARN is not a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Warn,
    Error,
}

impl Level {
    /// The word on the wire (`Kind::level`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    /// `None` for anything that is not a warning.
    pub fn of(l: &tracing::Level) -> Option<Level> {
        match *l {
            tracing::Level::ERROR => Some(Level::Error),
            tracing::Level::WARN => Some(Level::Warn),
            _ => None,
        }
    }
}

struct Site {
    count: u64,
    level: Level,
    message: String,
}

struct Ledger {
    kinds: BTreeMap<(String, u32), Site>,
    sink: Option<PathBuf>,
    sink_max_bytes: u64,
    sink_errors: u64,
}

/// One producer's warnings since its process started.
pub struct Counter {
    warn: AtomicU64,
    error: AtomicU64,
    ledger: Mutex<Ledger>,
}

/// The process's counter, for a producer that wants exactly one.
pub static COUNTER: Counter = Counter::new();

impl Default for Counter {
    fn default() -> Self {
        Self::new()
    }
}

impl Counter {
    pub const fn new() -> Self {
        Counter {
            warn: AtomicU64::new(0),
            error: AtomicU64::new(0),
            ledger: Mutex::new(Ledger { kinds: BTreeMap::new(), sink: None, sink_max_bytes: SINK_MAX_BYTES, sink_errors: 0 }),
        }
    }

    /// Attach the durable record. Lazy: nothing is opened until the first
    /// warning.
    pub fn attach_sink(&self, warnings_dir: &Path) {
        self.attach_sink_bounded(warnings_dir, SINK_MAX_BYTES);
    }

    /// [`Counter::attach_sink`] with the live-file bound named — a test's
    /// small threshold; production uses [`SINK_MAX_BYTES`].
    pub fn attach_sink_bounded(&self, warnings_dir: &Path, max_bytes: u64) {
        if let Ok(mut l) = self.ledger.lock() {
            l.sink = Some(warnings_dir.join(SINK_FILE));
            l.sink_max_bytes = max_bytes;
        }
    }

    /// Count one event at a call site, and append it to the durable record.
    pub fn record(&self, level: Level, file: &str, line: u32, message: &str) {
        match level {
            Level::Error => self.error.fetch_add(1, Ordering::Relaxed),
            Level::Warn => self.warn.fetch_add(1, Ordering::Relaxed),
        };
        let Ok(mut l) = self.ledger.lock() else { return };
        l.kinds
            .entry((file.to_string(), line))
            .or_insert_with(|| Site { count: 0, level, message: message.chars().take(LABEL_CHARS).collect() })
            .count += 1;
        let Some(path) = l.sink.clone() else { return };
        let rec = serde_json::json!({
            "at": chrono::Utc::now().timestamp(),
            "level": level.as_str(),
            "target": file,
            "line": line,
            "message": message,
        });
        let wrote = append_bounded(&path, l.sink_max_bytes, &rec.to_string());
        // COUNTED, NEVER LOGGED. See the module header.
        if wrote.is_err() {
            l.sink_errors += 1;
        }
    }

    /// Count a `tracing` event, keyed by the call site its macro captured. A
    /// no-op below WARN, so a layer may hand it every event.
    pub fn record_event(&self, event: &tracing::Event<'_>) {
        let meta = event.metadata();
        let Some(level) = Level::of(meta.level()) else { return };
        let mut msg = Message(String::new());
        event.record(&mut msg);
        self.record(level, meta.file().unwrap_or(meta.target()), meta.line().unwrap_or(0), &msg.0);
    }

    /// The export, as `env`'s own type: what `ServesWarnings::warnings`
    /// returns. `counts_since` is THIS PROCESS's start, never the shape's
    /// epoch, so a restart reads as a restart and not as a drop to zero.
    pub fn export(&self, producer: &str, shape: &str, counts_since: &str) -> Export {
        let (mut kinds, sink_errors, sink_attached) = match self.ledger.lock() {
            Ok(l) => (
                l.kinds
                    .iter()
                    .map(|((target, line), s)| Kind {
                        target: target.clone(),
                        line: *line,
                        level: s.level.as_str().into(),
                        count: s.count,
                        message: s.message.clone(),
                    })
                    .collect::<Vec<_>>(),
                l.sink_errors,
                l.sink.is_some(),
            ),
            // A panic while counting: the atomics are still true; invent no
            // breakdown.
            Err(_) => (Vec::new(), 0, false),
        };
        kinds.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.target.cmp(&b.target)).then(a.line.cmp(&b.line)));
        kinds.truncate(TOP_KINDS);
        Export {
            shape: shape.into(),
            shape_version: env::warnings::SHAPE_VERSION,
            producer: producer.into(),
            counts_since: counts_since.into(),
            warn: self.warn.load(Ordering::Relaxed),
            error: self.error.load(Ordering::Relaxed),
            sink_errors,
            sink_attached,
            kinds,
        }
    }
}

struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counted by call site, warn and error APART, top-N by count, the label is
    /// the FIRST message — read as absolute numbers off a counter of its own.
    #[test]
    fn warnings_are_counted_by_call_site_and_level_apart() {
        let c = Counter::new();
        let zero = c.export("p", "warnings", "0");
        assert_eq!((zero.warn, zero.error, zero.kinds.len()), (0, 0, 0), "a fresh counter is empty");
        for i in 0..3 {
            c.record(Level::Warn, "src/x.rs", 7, &format!("text {i}, the same site"));
        }
        c.record(Level::Error, "src/y.rs", 9, "an error");
        let e = c.export("p", "warnings", "0");
        assert_eq!((e.warn, e.error), (3, 1), "warn and error are counted apart");
        assert_eq!(e.kinds.len(), 2, "three messages at one site are ONE kind: {:?}", e.kinds);
        let x = &e.kinds[0];
        assert_eq!((x.target.as_str(), x.line, x.count), ("src/x.rs", 7, 3), "ordered by count");
        assert_eq!((x.level.as_str(), x.message.as_str()), ("warn", "text 0, the same site"));
        assert_eq!(e.kinds[1].level, "error");
        assert_eq!((e.producer.as_str(), e.counts_since.as_str()), ("p", "0"));
        assert!(!e.sink_attached);
    }

    #[test]
    fn only_the_top_kinds_are_exported_but_every_event_is_counted() {
        let c = Counter::new();
        for site in 0..(TOP_KINDS as u32 + 3) {
            for _ in 0..=site {
                c.record(Level::Warn, "src/many.rs", site, "m");
            }
        }
        let e = c.export("p", "warnings", "0");
        assert_eq!(e.kinds.len(), TOP_KINDS);
        let n = TOP_KINDS as u64 + 3;
        assert_eq!(e.warn, n * (n + 1) / 2, "the totals are not truncated with the list");
        assert_eq!(e.kinds[0].line, TOP_KINDS as u32 + 2, "the busiest site leads");
    }

    /// The durable record is written when attached — and a write that fails is
    /// a COUNTED field, paired with the write that succeeds.
    #[test]
    fn the_sink_is_written_and_a_failed_write_is_counted_not_logged() {
        let dir = tempfile::tempdir().unwrap();
        let good = Counter::new();
        good.attach_sink(&dir.path().join("warnings"));
        good.record(Level::Error, "src/a.rs", 1, "durable");
        let text = std::fs::read_to_string(dir.path().join("warnings").join(SINK_FILE)).unwrap();
        assert!(text.contains("\"durable\"") && text.contains("\"error\""), "{text}");
        let e = good.export("p", "warnings", "0");
        assert_eq!((e.sink_attached, e.sink_errors), (true, 0));

        // A sink under a regular FILE cannot be created.
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let bad = Counter::new();
        bad.attach_sink(&blocker.join("warnings"));
        bad.record(Level::Warn, "src/b.rs", 2, "lost");
        bad.record(Level::Warn, "src/b.rs", 2, "lost");
        let e = bad.export("p", "warnings", "0");
        assert_eq!((e.warn, e.sink_errors), (2, 2), "both failed writes are counted and both events still are");
    }

    /// **DG-3: the sink rotates past its bound, and a reader still sees the
    /// newest warning.** Opposed by the under-the-bound case: the same writes
    /// against a bound they never reach leave NO `.1`, so an implementation
    /// that rotates on every write (or never) fails one arm or the other.
    #[test]
    fn the_sink_rotates_past_its_bound_and_the_newest_warning_is_in_the_live_file() {
        const MAX: u64 = 600;
        let dir = tempfile::tempdir().unwrap();
        let c = Counter::new();
        c.attach_sink_bounded(&dir.path().join("warnings"), MAX);
        for i in 0..40 {
            c.record(Level::Warn, "src/r.rs", 3, &format!("a warning long enough to cross a small bound #{i}"));
        }
        let live = dir.path().join("warnings").join(SINK_FILE);
        let rotated = dir.path().join("warnings").join(format!("{SINK_FILE}.1"));
        let text = std::fs::read_to_string(&live).unwrap();
        let last: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(
            last["message"], "a warning long enough to cross a small bound #39",
            "a reader of the live file sees the NEWEST warning"
        );
        let live_len = std::fs::metadata(&live).unwrap().len();
        assert!(live_len <= MAX, "the live file is bounded: {live_len} > {MAX}");
        assert!(rotated.exists(), "the previous generation is kept as `.1`");
        let rot_len = std::fs::metadata(&rotated).unwrap().len();
        assert!(live_len + rot_len <= 2 * MAX, "total bounded at twice the bound: {live_len} + {rot_len}");
        assert_eq!(c.export("p", "warnings", "0").warn, 40, "rotation bounds the file, never the count");

        // OPPOSING: under the bound, no rotation.
        let dir2 = tempfile::tempdir().unwrap();
        let under = Counter::new();
        under.attach_sink_bounded(&dir2.path().join("warnings"), 1024 * 1024);
        for i in 0..40 {
            under.record(Level::Warn, "src/r.rs", 3, &format!("a warning long enough to cross a small bound #{i}"));
        }
        assert!(
            !dir2.path().join("warnings").join(format!("{SINK_FILE}.1")).exists(),
            "under the bound nothing rotates"
        );
        let all = std::fs::read_to_string(dir2.path().join("warnings").join(SINK_FILE)).unwrap();
        assert_eq!(all.lines().count(), 40, "and every line is still in the live file");
    }

    #[test]
    fn the_production_bound_is_threads_4_mib_live_8_mib_total() {
        assert_eq!(SINK_MAX_BYTES, 4 * 1024 * 1024);
        assert_eq!(SINK_MAX_BYTES, crate::ledger::LEDGER_MAX_BYTES);
        let c = Counter::new();
        c.attach_sink(Path::new("/nonexistent-dg3"));
        assert_eq!(c.ledger.lock().unwrap().sink_max_bytes, SINK_MAX_BYTES, "attach_sink applies the bound");
    }

    /// The `tracing` route: WARN and ERROR counted at the macro's call site,
    /// INFO not counted — the same subscriber, so the INFO arm is reachable.
    #[test]
    fn a_tracing_event_is_counted_at_its_call_site_and_info_is_not() {
        use tracing_subscriber::layer::SubscriberExt;
        static C: Counter = Counter::new();
        struct L;
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for L {
            fn on_event(&self, e: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
                C.record_event(e);
            }
        }
        let sub = tracing_subscriber::registry().with(L);
        tracing::subscriber::with_default(sub, || {
            tracing::info!("not a warning");
            tracing::warn!("a warning");
            tracing::error!("an error");
        });
        let e = C.export("p", "warnings", "0");
        assert_eq!((e.warn, e.error), (1, 1), "{:?}", e.kinds);
        assert!(e.kinds.iter().all(|k| k.target.ends_with("warnings.rs")), "{:?}", e.kinds);
        assert!(e.kinds.iter().any(|k| k.message == "a warning"), "{:?}", e.kinds);
    }
}
