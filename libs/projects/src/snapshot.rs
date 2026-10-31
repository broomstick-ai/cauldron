//! **A stat-validated cache of the registry.**
//!
//! `0071` §8: the AUI's `Config` gains one of these and `project_root` goes
//! through it. The point is that the AUI holds the list in memory but must not
//! hold a STALE list — a registration made by a future `cauldron register`
//! CLI, or by another face, has to be visible to the next call without a
//! restart. So every `get()` is a `stat`, and a re-parse happens only when the
//! file's mtime or length has moved.
//!
//! The cheap-clone shape (`Arc<Mutex<..>>`) is deliberate: `Config` is cloned
//! into tasks, and two clones must share one cache or the stat storm is per
//! clone.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::{registry_path, LoadError, Registry};

/// What the last `get()` found. The AUI speaks the difference: an absent
/// registry is a machine that has not bootstrapped; an unreadable one is a
/// warning plus a REFUSAL of every write until it parses (`0071` §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotStatus {
    /// Never stat'd yet.
    Unknown,
    /// No file. The in-memory list is empty and that is correct.
    Absent,
    Loaded,
    /// Present and this crate will not act on it. Carries the sentence.
    Unreadable(String),
}

#[derive(Debug)]
struct Inner {
    path: PathBuf,
    /// `None` for an `in_memory` snapshot, which never stats anything.
    watched: bool,
    stamp: Option<(SystemTime, u64)>,
    value: Arc<Registry>,
    status: SnapshotStatus,
}

/// A cheaply cloneable handle onto one cached parse of the registry.
#[derive(Debug, Clone)]
pub struct Snapshot(Arc<Mutex<Inner>>);

impl Snapshot {
    /// Watch the registry under `home`. Does not read it yet; the first
    /// [`Snapshot::get`] does.
    pub fn new(home: &Path) -> Snapshot {
        Snapshot(Arc::new(Mutex::new(Inner {
            path: registry_path(home),
            watched: true,
            stamp: None,
            value: Arc::new(Registry::empty()),
            status: SnapshotStatus::Unknown,
        })))
    }

    /// A snapshot over a registry that is not on disk — the test fixture
    /// `0071` §8 asks for, replacing `Config { minds_root: scratch }`.
    /// It never stats and never changes.
    pub fn in_memory(registry: Registry) -> Snapshot {
        Snapshot(Arc::new(Mutex::new(Inner {
            path: PathBuf::new(),
            watched: false,
            stamp: None,
            value: Arc::new(registry),
            status: SnapshotStatus::Loaded,
        })))
    }

    /// **The read every caller uses.** One `stat`; a re-parse only if the file
    /// moved. An absent or unreadable file yields the last good value, or an
    /// empty registry if there never was one — and [`Snapshot::status`] says
    /// which, because "empty" and "unreadable" must not look the same to a
    /// caller deciding whether to refuse a write.
    pub fn get(&self) -> Arc<Registry> {
        self.refresh();
        self.0.lock().expect("snapshot mutex").value.clone()
    }

    /// The same read, but an unreadable file is an error rather than a stale
    /// value. The AUI's write path uses this one.
    pub fn get_checked(&self) -> Result<Arc<Registry>, LoadError> {
        let value = self.get();
        let status = self.status();
        match status {
            SnapshotStatus::Unreadable(s) => Err(LoadError::Io(std::io::Error::other(s))),
            _ => Ok(value),
        }
    }

    pub fn status(&self) -> SnapshotStatus {
        self.0.lock().expect("snapshot mutex").status.clone()
    }

    fn refresh(&self) {
        let mut inner = self.0.lock().expect("snapshot mutex");
        if !inner.watched {
            return;
        }
        let stamp = std::fs::metadata(&inner.path)
            .ok()
            .and_then(|m| m.modified().ok().map(|t| (t, m.len())));
        match stamp {
            None => {
                inner.stamp = None;
                inner.value = Arc::new(Registry::empty());
                inner.status = SnapshotStatus::Absent;
            }
            Some(s) => {
                if inner.stamp == Some(s) && matches!(inner.status, SnapshotStatus::Loaded) {
                    return;
                }
                let text = std::fs::read_to_string(&inner.path);
                match text.map_err(LoadError::Io).and_then(|t| Registry::from_json(&t)) {
                    Ok(r) => {
                        inner.stamp = Some(s);
                        inner.value = Arc::new(r);
                        inner.status = SnapshotStatus::Loaded;
                    }
                    Err(e) => {
                        // Keep the last good value; say loudly that it is stale.
                        inner.stamp = Some(s);
                        inner.status = SnapshotStatus::Unreadable(e.sentence());
                    }
                }
            }
        }
    }
}
