//! # `diag.rs` — **THE TRANSCRIPT'S FLIGHT RECORDER.**
//!
//! One line per thing that changes what the transcript can show, written to a
//! file so an agent can read what the face was told and what it did with it:
//!
//! | category | what it records |
//! |---|---|
//! | `shape` | every hold / watch / unwatch of a shape key, every change in what the face's own mirror says about a held `threads/log` key (unhydrated → known → dead), and every shape-read refusal with the shape, the key and the reason |
//! | `wake` | every seam wake-up the projection ran on |
//! | `frame` | every ledger row the projection handed the store, with its kind, `message_id`, whether its bytes were behind the resource door, and the text length it arrived with |
//! | `body` | every elided body fetched through the resource door, and every failure to fetch one |
//! | `send` | every send issued, and its ack or refusal |
//! | `store` | every row INSERTED, REPLACED or REMOVED from the open thread's retained rows, with the reason |
//! | `view` | every transcript block that APPEARED, CHANGED or DISAPPEARED between two projections, with the reason it can name |
//! | `cancel` | every interrupt/cancel the face sends (`SEND`, before the request leaves) and the daemon's answer (`REPLY`, ok or error, with latency), the two joined by one `rid=` (TC-46) |
//!
//! **Removals are the point.** The symptom this file was built for is a row
//! that appears and then vanishes, so every path that takes a row out of the
//! store or a block out of the view says so here, with what it was.
//!
//! ## Where it goes
//!
//! `$TUI_DIAG` names the file; `TUI_DIAG=off` turns it off. Unset, the live face
//! writes `~/.tui/diag.log` and the headless runner writes the path it is given.
//! It is SEPARATE from `~/.tui/log` because that log is `tracing`'s and is
//! filtered at `info`; this one is unfiltered by design and must not be lost to
//! a log level.
//!
//! Nothing is written until [`init`] is called, so a test that never calls it
//! writes nothing and pays one atomic load per call site.

use std::fmt;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// The size past which [`init`] rotates the file.
pub const ROTATE_AT: u64 = 64 * 1024 * 1024;

static ON: AtomicBool = AtomicBool::new(false);
static OUT: OnceLock<Mutex<Sink>> = OnceLock::new();
/// How many [`Tap`]s are live. A tap turns recording on for as long as it is
/// held, whether or not a file was ever opened.
static TAPPED: AtomicUsize = AtomicUsize::new(0);
static TAPS: Mutex<Vec<Arc<Mutex<Vec<String>>>>> = Mutex::new(Vec::new());

/// **BUFFERED, AND FLUSHED OFF THE CALLER'S THREAD** (THREAD-LOAD-LATENCY F8).
/// It was a `LineWriter`: one `write(2)` per line, under one global mutex,
/// from the UI loop, the forward task and every transcript task — and opening
/// a 4,000-row thread writes a line per row. Now a line is a memcpy into a
/// buffer, a flusher thread writes it out every [`FLUSH_EVERY`], and the same
/// thread rotates the file when it passes [`ROTATE_AT`] (it used to rotate only
/// at start, so a face that ran for days grew it without bound — 128 MB seen).
/// A panic flushes too (`flush_now`), so the last lines before a crash land.
struct Sink {
    w: BufWriter<std::fs::File>,
    path: PathBuf,
    bytes: u64,
}

/// How often buffered lines reach the file.
pub const FLUSH_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// Where the recorder writes, resolved from `$TUI_DIAG` and a fallback.
/// `None` means it is turned off.
///
/// **UNDER ISOLATION, `TUI_DIAG=<path>` IS HONOURED ONLY UNDER THE ISOLATED
/// HOME** (REL-6a4b, REL-6a4-I N1): an isolated process (a test, a headless
/// run, the marker) given a path anywhere else — his home, inside it, a
/// relative path, a sibling scratch dir — refuses it: the recorder is OFF,
/// and the refusal is printed to stderr. `off` / `0` / unset are unchanged.
pub fn path_for(fallback: PathBuf) -> Option<PathBuf> {
    let var = std::env::var("TUI_DIAG").ok();
    let iso = crate::cauldron::live::isolation(false).map(|why| (why, crate::cauldron::live::face_home()));
    match path_for_from(var.as_deref(), fallback, iso.as_ref().map(|(w, h)| (w.as_str(), h.as_deref()))) {
        Ok(p) => p,
        Err(why) => {
            eprintln!("tui: {why}");
            tracing::warn!("{why}");
            None
        }
    }
}

/// [`path_for`] over explicit inputs. `isolated` is `Some((why, isolated
/// home))` in an isolated process. `Err` is the refusal (recorder off).
pub fn path_for_from(var: Option<&str>, fallback: PathBuf, isolated: Option<(&str, Option<&std::path::Path>)>) -> Result<Option<PathBuf>, String> {
    let explicit = match var {
        Some(v) if v == "off" || v == "0" => return Ok(None),
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => return Ok(Some(fallback)),
    };
    let Some((why, home)) = isolated else { return Ok(Some(explicit)) };
    match home {
        Some(h) if explicit.is_absolute() && under(&explicit, h) => Ok(Some(explicit)),
        _ => Err(format!(
            "refused: TUI_DIAG={} is not under this isolated process's home ({}), and this process is isolated ({why}); \
             the flight recorder is off",
            explicit.display(),
            home.map(|h| h.display().to_string()).unwrap_or_else(|| "none".into())
        )),
    }
}

/// `p` is `home` or inside it, with no `..`, judged through the symlinks of
/// the nearest existing ancestor of each (`/var` is `/private/var`).
fn under(p: &std::path::Path, home: &std::path::Path) -> bool {
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return false;
    }
    let canon = |x: &std::path::Path| {
        let mut tail = Vec::new();
        let mut at = x;
        loop {
            if let Ok(c) = at.canonicalize() {
                return tail.iter().rev().fold(c, |acc: PathBuf, t| acc.join(t));
            }
            match (at.parent(), at.file_name()) {
                (Some(parent), Some(name)) => {
                    tail.push(name.to_os_string());
                    at = parent;
                }
                _ => return x.to_path_buf(),
            }
        }
    };
    canon(p).starts_with(canon(home))
}

/// Open the file (append) and start recording. Idempotent; a second call is
/// ignored. An unopenable file leaves the recorder off and says why on the
/// `tracing` log rather than failing the face.
pub fn init(path: &std::path::Path) {
    if OUT.get().is_some() {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // **BOUNDED.** One open of a long thread is about a megabyte (a line per
    // retained row), so the file is rotated once at start when it has grown
    // past [`ROTATE_AT`]; the previous generation is kept as `<path>.1`.
    if std::fs::metadata(path).map(|m| m.len() > ROTATE_AT).unwrap_or(false) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => {
            let bytes = f.metadata().map(|m| m.len()).unwrap_or(0);
            let _ = OUT.set(Mutex::new(Sink {
                w: BufWriter::with_capacity(256 * 1024, f),
                path: path.to_path_buf(),
                bytes,
            }));
            ON.store(true, Ordering::Release);
            let _ = std::thread::Builder::new().name("diag-flush".into()).spawn(|| loop {
                std::thread::sleep(FLUSH_EVERY);
                flush_now();
            });
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                flush_now();
                prev(info);
            }));
            line(
                "start",
                format_args!("pid={} path={}", std::process::id(), path.display()),
            );
        }
        Err(e) => tracing::warn!(path = %path.display(), %e, "diag: could not open"),
    }
}

/// Whether anything is being recorded. Call sites that would do work to build
/// a line check this first.
#[inline]
pub fn on() -> bool {
    ON.load(Ordering::Acquire) || TAPPED.load(Ordering::Acquire) > 0
}

/// Write one line: `<utc> <pid> <category> <message>`.
///
/// The SAME line goes to the file and to every live [`Tap`], so a test that
/// reads a tap reads exactly what `~/.tui/diag.log` would have received.
pub fn line(cat: &str, msg: fmt::Arguments<'_>) {
    if !on() {
        return;
    }
    let text = format!(
        "{} {} {:<5} {}",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        std::process::id(),
        cat,
        msg
    );
    if TAPPED.load(Ordering::Acquire) > 0 {
        for t in TAPS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            t.lock().unwrap_or_else(|e| e.into_inner()).push(text.clone());
        }
    }
    if let Some(out) = OUT.get() {
        let mut g = out.lock().unwrap_or_else(|e| e.into_inner());
        g.bytes += text.len() as u64 + 1;
        let _ = g.w.write_all(text.as_bytes());
        let _ = g.w.write_all(b"\n");
    }
}

/// **AN IN-MEMORY COPY OF EVERY LINE, FOR AS LONG AS IT IS HELD.**
///
/// The recorder's file is process-global and opened once, so a test cannot
/// point it somewhere of its own without racing every other test in the
/// binary. A tap is the test's door instead: [`tap`] starts one, every line
/// written while it lives is copied into it as well as to the file (if any),
/// and dropping it stops the copy. Lines from other tests running at the same
/// time land in it too, so a reader filters on something it alone wrote (a
/// thread id).
pub struct Tap(Arc<Mutex<Vec<String>>>);

/// Start a [`Tap`].
pub fn tap() -> Tap {
    let buf = Arc::new(Mutex::new(Vec::new()));
    TAPS.lock().unwrap_or_else(|e| e.into_inner()).push(buf.clone());
    TAPPED.fetch_add(1, Ordering::AcqRel);
    Tap(buf)
}

impl Tap {
    /// Every line captured so far, in the order written.
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        TAPS.lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|t| !Arc::ptr_eq(t, &self.0));
        TAPPED.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Write out whatever is buffered, and rotate the file once it has passed
/// [`ROTATE_AT`]. Called by the flusher thread and on panic.
pub fn flush_now() {
    let Some(out) = OUT.get() else { return };
    let mut g = out.lock().unwrap_or_else(|e| e.into_inner());
    let _ = g.w.flush();
    if g.bytes > ROTATE_AT {
        let path = g.path.clone();
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            g.w = BufWriter::with_capacity(256 * 1024, f);
            g.bytes = 0;
        }
    }
}

/// `diag!("store", "insert seq={seq}")` — a line, built only when recording.
#[macro_export]
macro_rules! diag {
    ($cat:expr, $($arg:tt)*) => {
        if $crate::diag::on() {
            $crate::diag::line($cat, format_args!($($arg)*));
        }
    };
}

/// A short, one-line, char-safe preview of some text for a log line.
pub fn preview(s: &str, n: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(n)
        .collect();
    if s.chars().count() > n {
        format!("{flat:?}…")
    } else {
        format!("{flat:?}")
    }
}

#[cfg(test)]
mod tests {
    /// **A LINE IS A MEMCPY, AND EVERY LINE STILL LANDS** (THREAD-LOAD-LATENCY
    /// F8). Twenty thousand lines from four threads: all of them are in the
    /// file after `flush_now`, none torn, and writing them took no `write(2)`
    /// per line — measured here as the time per line, printed for the doc.
    /// The ONLY test in this crate that calls `init` (the recorder is global).
    #[test]
    fn buffered_lines_all_land_and_cost_no_syscall_each() {
        let dir = std::env::temp_dir().join(format!("tui-diag-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("diag.log");
        let _ = std::fs::remove_file(&path);
        super::init(&path);
        let t0 = std::time::Instant::now();
        let hs: Vec<_> = (0..4)
            .map(|t| {
                std::thread::spawn(move || {
                    for i in 0..5_000 {
                        crate::diag!("test", "thread={t} line={i} {}", "x".repeat(80));
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let per_line_ns = t0.elapsed().as_nanos() / 20_000;
        super::flush_now();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| l.contains(" test  thread=")).collect();
        assert_eq!(lines.len(), 20_000, "lines were lost");
        assert!(lines.iter().all(|l| l.ends_with(&"x".repeat(80))), "a line was torn");
        eprintln!("DIAG per_line_ns={per_line_ns}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
