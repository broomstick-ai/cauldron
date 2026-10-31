//! A cross-process lock for read-modify-write of the seam table.
//!
//! Two services starting at once in one environment both have to land in one
//! `seams.json`, so the file needs a lock. `create_new` on an ordinary file is
//! atomic on every filesystem that matters, so that is the lock: the holder
//! writes its pid and the time, a waiter spins, and a lock older than
//! [`STALE_AFTER`] is broken with a message rather than waited on forever —
//! because the process that took it may have been SIGKILLed, which is exactly
//! the case this whole crate is built to survive.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

pub const WAIT: Duration = Duration::from_secs(5);
pub const STALE_AFTER: Duration = Duration::from_secs(30);

pub struct Guard(PathBuf);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Take the lock beside `target`, run `f`, release. Never holds it across a
/// network call or a spawn.
pub fn with_lock<T>(target: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock = target.with_extension("lock");
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let start = Instant::now();
    let _guard = loop {
        match OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(mut fh) => {
                let _ = writeln!(fh, "{} {}", std::process::id(), now());
                break Guard(lock.clone());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let age = std::fs::metadata(&lock)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|m| SystemTime::now().duration_since(m).ok())
                    .unwrap_or_default();
                if age > STALE_AFTER {
                    eprintln!(
                        "cauldron: breaking a stale seam lock {} ({}s old; its holder is gone)",
                        lock.display(),
                        age.as_secs()
                    );
                    let _ = std::fs::remove_file(&lock);
                    continue;
                }
                if start.elapsed() > WAIT {
                    let holder = std::fs::read_to_string(&lock).unwrap_or_default();
                    bail!(
                        "timed out after {}s waiting for the seam lock {} (held by `{}`)",
                        WAIT.as_secs(),
                        lock.display(),
                        holder.trim()
                    );
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e).with_context(|| format!("opening {}", lock.display())),
        }
    };
    f()
}
