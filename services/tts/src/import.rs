//! `tts.import` — adopt an existing cache directory, by hash, by hardlink.
//!
//! The face's cache holds `<64-hex>.aiff` files whose names are exactly this
//! service's hash (see [`crate::hash`]): THE FILENAME IS THE HASH. So adoption
//! is a directory walk and a `link(2)` — no re-synthesis, no copied byte.
//!
//! * Hardlink, not copy (`copy` exists for a cross-filesystem case).
//! * `last_used_at` is seeded from the file's mtime, which the face has kept
//!   as its LRU key all along, so the recency order carries over.
//! * NOTHING is ever deleted from the source. `would_delete` is always 0.
//! * A dry run (the default on the wire) stats and never opens, so it does
//!   not perturb the mtimes it depends on.
//!
//! THE VOICE TRIPWIRE: the operator's cache is keyed on "Samantha". A
//! `module_config` whose fingerprint disagrees with the key's is refused,
//! because adopting under another voice would orphan every file silently.
//!
//! The substrate binary also exposed this as `tts import`; the websocket
//! method is now the one door.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::hash;
use crate::manager::{mtime_rfc3339, TtsManager};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    Hardlink,
    Copy,
}

impl ImportMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ImportMode::Hardlink => "hardlink",
            ImportMode::Copy => "copy",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [ImportMode::Hardlink, ImportMode::Copy].into_iter().find(|m| m.as_str() == s)
    }
}

/// A leading `~/` becomes `$HOME/`.
pub fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(p),
    }
}

/// Adopt `from_dir` into `key`.
pub fn import(m: &TtsManager, key: &str, from_dir: &Path, module_config: Option<Value>, mode: ImportMode, dry_run: bool) -> Result<Value> {
    let rk = m.key(key)?;
    if let Some(cfg) = &module_config {
        let asked = rk.module.fingerprint(cfg);
        if asked != rk.fingerprint {
            return Err(Error::bad_params(format!(
                "module_config fingerprint {asked:?} does not match key {key}'s fingerprint {:?} — adopting \
                 under a different voice would orphan every file in {} rather than import it",
                rk.fingerprint,
                from_dir.display()
            )));
        }
    }
    if !from_dir.is_dir() {
        return Err(Error::bad_params(format!("from_dir {} is not a directory", from_dir.display())));
    }
    let (mut scanned, mut adopted, mut present, mut bad_name, mut empty, mut bytes) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut errors = Vec::new();
    for e in std::fs::read_dir(from_dir)?.flatten() {
        scanned += 1;
        let name = e.file_name().to_string_lossy().to_string();
        // ONLY `<64-hex>.aiff`: in-flight temps and strays are skipped.
        let Some(h) = name.strip_suffix(".aiff").filter(|s| hash::is_hash(s)).map(str::to_string) else {
            bad_name += 1;
            continue;
        };
        let Ok(meta) = e.metadata() else {
            bad_name += 1;
            continue;
        };
        if !meta.is_file() || meta.len() == 0 {
            empty += 1;
            continue;
        }
        if m.store.get_render(&h)?.is_some() && m.store.has_entry(key, &h)? {
            present += 1;
            continue;
        }
        if !dry_run {
            let rel = hash::rel_path(&h, rk.ext);
            let dest = m.abs_path(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if !dest.exists() {
                let placed = match mode {
                    ImportMode::Hardlink => std::fs::hard_link(e.path(), &dest),
                    ImportMode::Copy => std::fs::copy(e.path(), &dest).map(|_| ()),
                };
                if let Err(err) = placed {
                    errors.push(format!("{name}: {err}"));
                    continue;
                }
            }
            let at = mtime_rfc3339(&meta);
            m.store.adopt_render(&h, rk.module.name(), &rk.fingerprint, &rel, meta.len(), rk.ext, &at)?;
            m.store.register_entry_at(key, &h, &at)?;
        }
        adopted += 1;
        bytes += meta.len();
    }
    Ok(json!({
        "key": key, "from_dir": from_dir.to_string_lossy(), "mode": mode.as_str(), "dry_run": dry_run,
        "fingerprint": rk.fingerprint, "scanned": scanned, "adopted": adopted, "already_present": present,
        "skipped": bad_name + empty, "skipped_name": bad_name, "skipped_empty": empty, "bytes": bytes,
        "errors": errors, "would_delete": 0,
    }))
}
