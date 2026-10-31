//! **DG-2 — SERVICE AND DAEMON LOGS ROTATE, SO NO LOG GROWS WITHOUT BOUND.**
//!
//! Every writer of `data/<env>/logs/<svc>.log` (a hosted daemon's stdout and
//! stderr, a started service's, a scheduled job pass's) and of the holder's
//! `<process>.launchd.log` used to open its file `create+append` and never
//! rotate it; `threads.log` reached 144 MiB at about 14 MB/day (census §3).
//!
//! This is the one helper every one of those writers calls, and it is the
//! `.1`-rename pattern of the shapewire subscriber ledger
//! (`libs/shapewire/src/ledger.rs`, `Journal::rotate_for` and
//! `previous_generation_path`) generalised from one generation to
//! [`LOG_GENERATIONS`]:
//!
//! - the generation suffix is appended to the WHOLE name (`threads.log` →
//!   `threads.log.1`), so a log and its generations sort together;
//! - rotation is a RENAME and never a copy-and-truncate. These logs are the
//!   stdout of a child that holds the file open for its whole life. A rename
//!   leaves that child writing, uninterrupted and in order, to the inode it
//!   already holds (now `.1`); the next OPEN of the path — the next start,
//!   restart or job pass — creates a fresh file. A truncate in place would
//!   instead discard what was rotated out from under a live writer and leave
//!   the writer's `O_APPEND` offset writing into the "new" file.
//!
//! cli does not link shapewire (its manifest carries `env`, `mcp-host` and
//! no other workspace crate), so the pattern is reproduced here rather than
//! imported: one helper for the cli's four writers, not four copies.
//!
//! No dependency beyond `std`: the integration tests include this file by
//! `#[path]`, because the cli has no library target.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// The size past which a log is rotated when it is next opened: 64 MiB.
///
/// At the measured `threads.log` average (about 20 MB/day: 343 MB written
/// 2026-10-09 → 2026-10-27, DG-2 inspect; the rate is bursty) that is about
/// 3.4 days per generation for the busiest service; every other log is a
/// small fraction of it. Ledger DG-2-1 (executor ruling, round 44).
pub const LOG_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// How many rotated generations are kept beside the live file (`.1`, `.2`,
/// `.3`): 3 × 64 MiB ≈ 201 MB ≈ 10 days of `threads.log` at about 20 MB/day.
///
/// Ledger DG-2-1 (executor ruling, round 44). Total on disk per log is bounded at
/// `(1 + LOG_GENERATIONS) × LOG_MAX_BYTES` plus whatever a running writer
/// appends past the bound before its next open (rotation happens at open; a
/// long-lived daemon is not rotated under itself).
pub const LOG_GENERATIONS: usize = 3;

/// `x.log` → `x.log.<n>`. The suffix is appended to the whole name, as
/// shapewire's `previous_generation_path` does.
pub fn generation_path(path: &Path, n: usize) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// **Rotate `path` by rename if it is larger than `max_bytes`.** Returns
/// whether it rotated.
///
/// `.{generations-1}` → `.{generations}` (replacing it, which is how the
/// oldest generation is dropped), …, `.1` → `.2`, then the live file → `.1`.
/// Any generation numbered beyond `generations` — left by an earlier, larger
/// policy — is removed. A missing file, or one at or under the bound, is left
/// exactly as it is. `generations` below 1 is treated as 1: a rotation that
/// kept nothing would be a delete.
pub fn rotate_if_over(path: &Path, max_bytes: u64, generations: usize) -> io::Result<bool> {
    let held = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if held <= max_bytes {
        return Ok(false);
    }
    let generations = generations.max(1);
    for n in (1..generations).rev() {
        let from = generation_path(path, n);
        if from.exists() {
            std::fs::rename(&from, generation_path(path, n + 1))?;
        }
    }
    std::fs::rename(path, generation_path(path, 1))?;
    // Beyond the cap: what an earlier policy left. Contiguous by
    // construction, so the sweep stops at the first gap.
    let mut n = generations + 1;
    loop {
        let stale = generation_path(path, n);
        match std::fs::remove_file(&stale) {
            Ok(()) => n += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// **Open a service log for appending, rotating it first if it is past the
/// bound.** The one open every cli log writer uses.
///
/// A failed rotation is reported on stderr and does NOT fail the open: a
/// service whose log could not be rotated still starts, writing to the file
/// it would have written to before DG-2.
pub fn open_log(path: &Path) -> io::Result<File> {
    rotate_quietly(path);
    OpenOptions::new().create(true).append(true).open(path)
}

/// [`rotate_if_over`] at the cli's bound and generation count, with a failure
/// reported and swallowed. Used directly for a log the cli does not open
/// itself (the launchd log, which launchd opens).
pub fn rotate_quietly(path: &Path) -> bool {
    match rotate_if_over(path, LOG_MAX_BYTES, LOG_GENERATIONS) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cauldron: could not rotate {} ({e}); appending to it as it is", path.display());
            false
        }
    }
}
