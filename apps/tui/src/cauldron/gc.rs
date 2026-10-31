//! **Garbage collection of the face's OWN stores** (Config › Devices ›
//! Storage & retention › Garbage collection; 0214 A2: `run now` was a dead
//! control over a collector that did not exist).
//!
//! **It never deletes anything a person wrote** (0217 B1, HIGH: it once took
//! the old bucket files while an older face was still writing his words into
//! them). Never collected, whatever bucket adopted them or how old they are:
//! the bucket and its paste bodies, the old buckets (`draft.json`,
//! `drafts/`, their chips) — an older face's later writes are RE-ADOPTED into
//! the bucket instead (`store::readopt`) — the dumps (`dumps.jsonl`, and
//! `sends.jsonl` before it), tickets and the values given to them, settings
//! and their `.bak`s, the seed, the clusters made here, the ripples' runs,
//! the index of where each reference in the bucket came from (`refs.json`),
//! and anything outside the face's two dirs, `<root>/data/<env>/tui/soup/`
//! and `<root>/data/shared/tui/`. A temp file left by a write of any of
//! those may hold the newest copy of his words, so it stays too.
//!
//! What it may take is only what a face can make again:
//!
//! - **left-over temp files** of the layout and the dev names (`.layout.json.
//!   ….tmp`, `.dev_names.json.….tmp`) and of the old collector's dump trim,
//!   older than a minute (a live write lasts microseconds).
//!
//! None of those is a regenerable CACHE, so only a person takes them: `run
//! now` lists them and asks, and the automatic pass (the interval, disk
//! pressure) deletes NOTHING — it only says that there is something to
//! confirm. Dumps past `storage.tui.dumps_cap_mb` are said in the plan's
//! notes, and kept. A [`Plan`] is computed first, then [`Plan::apply`]
//! performs it after his confirm, or not at all on a dry run.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A temp file younger than this may be a write in flight.
const TMP_AGE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    /// A whole file to delete.
    File(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub what: What,
    pub bytes: u64,
    /// Which kind, in his words: `dumps over the cap`, `orphaned paste bodies`, …
    pub kind: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub items: Vec<Item>,
    /// What a pass sees and does not take, in his words (dumps over their cap).
    pub notes: Vec<String>,
}

/// The temp files a pass may take, by the file whose write left them: only
/// what a face makes again. Never a bucket's, a ticket's, a setting's, a
/// cluster's or a dump's — those may hold the newest copy of his words.
const REGENERABLE_TMP: [&str; 3] = [".layout.json.", ".dev_names.json.", ".dumps.gc."];

fn len(p: &Path) -> u64 {
    std::fs::metadata(p).map_or(0, |m| m.len())
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    out.sort();
    out
}

impl Plan {
    /// What a pass would collect now, in the face's soup dir `dir` and the
    /// shared face dir `global`, with dumps capped at `cap_bytes` (said,
    /// never trimmed).
    pub fn compute(dir: &Path, global: &Path, cap_bytes: u64) -> Plan {
        let mut items = Vec::new();
        let mut notes = Vec::new();
        let dumps = len(&dir.join("dumps.jsonl"));
        if dumps > cap_bytes {
            notes.push(format!("the dumps take {}, over the {} cap — they are what was sent, and are kept", size(dumps), size(cap_bytes)));
        }
        let now = SystemTime::now();
        for d in [dir.to_path_buf(), global.to_path_buf()] {
            for p in files_in(&d) {
                let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let old = std::fs::metadata(&p).and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).unwrap_or_default() >= TMP_AGE);
                if n.ends_with(".tmp") && REGENERABLE_TMP.iter().any(|k| n.starts_with(k)) && old {
                    items.push(Item { bytes: len(&p), what: What::File(p), kind: "left-over temp files" });
                }
            }
        }
        Plan { items, notes }
    }

    pub fn bytes(&self) -> u64 {
        self.items.iter().map(|i| i.bytes).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// One line per kind: `orphaned paste bodies: 3 files, 1.2 KB`.
    pub fn lines(&self) -> Vec<String> {
        let mut kinds: Vec<&'static str> = Vec::new();
        for i in &self.items {
            if !kinds.contains(&i.kind) {
                kinds.push(i.kind);
            }
        }
        kinds
            .into_iter()
            .map(|k| {
                let of: Vec<&Item> = self.items.iter().filter(|i| i.kind == k).collect();
                let bytes: u64 = of.iter().map(|i| i.bytes).sum();
                let count = format!("{} file{}", of.len(), if of.len() == 1 { "" } else { "s" });
                format!("{k}: {count}, {}", size(bytes))
            })
            .collect()
    }

    /// `1.2 KB · 4 files`, or `nothing`.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "nothing".into();
        }
        let n = self.items.len();
        format!("{} · {n} item{}", size(self.bytes()), if n == 1 { "" } else { "s" })
    }

    /// Perform the plan. Returns the bytes reclaimed; a part that fails is
    /// skipped and named in the error, the rest still done.
    pub fn apply(&self) -> Result<u64, (u64, String)> {
        let mut done = 0u64;
        let mut failed: Vec<String> = Vec::new();
        for i in &self.items {
            let What::File(p) = &i.what;
            let r = std::fs::remove_file(p).map_err(|e| format!("{}: {e}", p.display()));
            match r {
                Ok(()) => done += i.bytes,
                Err(e) => failed.push(e),
            }
        }
        if failed.is_empty() { Ok(done) } else { Err((done, failed.join("; "))) }
    }
}

/// Bytes in his units.
pub fn size(b: u64) -> String {
    match b {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("gc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn age(p: &Path) {
        let old = SystemTime::now() - Duration::from_secs(600);
        std::fs::File::options().write(true).open(p).unwrap().set_modified(old).unwrap();
    }

    /// 0217 B1: nothing a person wrote is ever in a plan — the bucket, its
    /// pastes (named or not), the old buckets and their chips, the dumps
    /// (over their cap or not), the old log, tickets, settings, and a temp
    /// file any of those left. Only a layout's or dev names' stale temp goes.
    #[test]
    fn a_pass_never_takes_his_words() {
        let root = scratch();
        let (dir, global) = (root.join("soup"), root.join("shared"));
        std::fs::create_dir_all(dir.join("bucket/artifacts")).unwrap();
        std::fs::create_dir_all(dir.join("drafts/X-1/artifacts")).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        let his = [
            "bucket.json", "bucket/artifacts/aaaa1111.txt", "bucket/artifacts/bbbb2222.txt", "drafts/X-1.json",
            "drafts/X-1/artifacts/cccc3333.txt", "draft.json", "dumps.jsonl", "sends.jsonl", "tickets.json",
            "settings.toml", "settings.toml.bak", "clusters.json", ".bucket.0000.tmp", ".tickets.json.1.tmp",
            ".settings.toml.1.tmp", ".clusters.json.1.tmp", ".draft.json.1.tmp",
        ];
        for f in his {
            std::fs::write(dir.join(f), "his words ".repeat(50)).unwrap();
            age(&dir.join(f));
        }
        let stale = dir.join(".layout.json.1.tmp");
        std::fs::write(&stale, "{}").unwrap();
        age(&stale);
        std::fs::write(global.join(".dev_names.json.1.tmp"), "{}").unwrap();
        let plan = Plan::compute(&dir, &global, 10);
        assert_eq!(plan.items.iter().map(|i| &i.what).collect::<Vec<_>>(), [&What::File(stale.clone())], "{plan:#?}");
        assert!(plan.notes[0].contains("over the 10 B cap") && plan.notes[0].contains("kept"), "{:?}", plan.notes);
        assert_eq!(plan.apply(), Ok(plan.bytes()));
        for f in his {
            assert!(dir.join(f).exists(), "{f} was taken");
        }
        assert!(!stale.exists() && global.join(".dev_names.json.1.tmp").exists(), "a young temp file stays");
        assert!(Plan::compute(&dir, &global, 10).is_empty(), "a second pass finds nothing");
        std::fs::remove_dir_all(root).unwrap();
    }
}
