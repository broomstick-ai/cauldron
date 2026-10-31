//! **Where each reference in the bucket came from** (8e4fca10 msg 385: "If
//! I click on a reference, why don't we navigate to its location in the
//! cluster it came from. you'll need an index on disk so if I close the app,
//! open it back up and click on a reference in the bucket it knows which
//! cluster put that reference there. also hard because span position will
//! change as the bucket changes. need dynamic reference insert location
//! recomputing. and it needs to be lightweight in rust of course.").
//!
//! ONE small file per cauldron, `<soup dir>/refs.json`, written whole and
//! atomically ([`store::write_text_atomic`]):
//!
//! ```text
//! { "version": 1,
//!   "text": "<the bucket text the anchors below are against>",
//!   "refs": [ { "id": "3f9a…", "tag": "[Q-1]", "at": 12,
//!               "cluster": "X-1", "ctab": "surface", "tab": "questions",
//!               "aggregate": false } ] }
//! ```
//!
//! `at` is the byte where the reference's `[` sits in `text`. Each entry is
//! one reference a click or `i` put in, with the cluster, the cauldron tab
//! and its sub-tab it was put in from.
//!
//! **The anchors follow the bucket through every change, whoever made it.**
//! Whenever the bucket's text moves from one string to another — a key he
//! typed, a paste, a click's insert, a send that empties it, another face's
//! write merged in three ways — [`follow`] carries every anchor across by the
//! character LCS of the two texts ([`store::lcs_pairs`], the merge's own):
//! an entry survives only if every character of its reference is kept, side
//! by side, in the new text, and lands where they did. A reference taken out,
//! or broken into, takes its entry with it. Because the file carries the text
//! its anchors are against, a face that reads another face's entries carries
//! them over to its own bucket the same way, so a reference one face put in
//! navigates from the other too.
//!
//! **It is user-derived state and it is never collected**: `gc` takes only
//! three named kinds of temp file, and this is none of them. A reference
//! with no entry — typed by hand, pasted, or put in by an older face — is
//! still a reference: the face resolves it by its soup id alone
//! ([`App::goto_ref`](crate::cauldron::app::App::goto_ref)).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cauldron::store;

/// The file's name in the cauldron's soup dir.
pub const FILE: &str = "refs.json";

/// Where a reference was put in from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// The cluster's minted key (`X-1`).
    pub cluster: String,
    /// The cauldron tab: `surface`, `status` or `config`.
    pub ctab: String,
    /// Its sub-tab: the cluster tab on Surface (`questions`, `nodes`, …),
    /// the Status or Config sub-tab otherwise.
    pub tab: String,
    /// Whether `Aggregated clusters` was on (an item of a cluster below).
    #[serde(default)]
    pub aggregate: bool,
}

/// One reference in the bucket and where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    /// The reference as written, `[Q-1]`.
    pub tag: String,
    /// The byte of its `[` in the text the entry is against.
    pub at: usize,
    #[serde(flatten)]
    pub origin: Origin,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RefsFile {
    version: u32,
    text: String,
    refs: Vec<Entry>,
}

/// Carry `refs`, anchored in `old`, over to `new`. An entry whose reference
/// is not kept whole and in one piece is dropped.
pub fn follow(old: &str, new: &str, refs: &mut Vec<Entry>) {
    if old == new || refs.is_empty() {
        return;
    }
    let (o, n): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    // Old char index → new char index, for every kept character.
    let mut map: Vec<Option<usize>> = vec![None; o.len()];
    for (i, j) in store::lcs_pairs(&o, &n) {
        map[i] = Some(j);
    }
    // Byte → char index of `old`, and char → byte of `new`.
    let char_of = |b: usize| old.get(..b).map(|s| s.chars().count());
    let new_bytes: Vec<usize> = new.char_indices().map(|(b, _)| b).chain(std::iter::once(new.len())).collect();
    refs.retain_mut(|e| {
        if old.get(e.at..).is_none_or(|s| !s.starts_with(&e.tag)) {
            return false;
        }
        let Some(c) = char_of(e.at) else { return false };
        let len = e.tag.chars().count();
        let Some(Some(first)) = map.get(c).copied() else { return false };
        if !(0..len).all(|k| map.get(c + k).copied().flatten() == Some(first + k)) {
            return false;
        }
        e.at = new_bytes[first];
        new[e.at..].starts_with(&e.tag)
    });
}

/// The face's index: the entries, the text they are anchored in, and the
/// file they live in.
#[derive(Debug)]
pub struct Index {
    path: PathBuf,
    writable: bool,
    /// The bucket text the entries are anchored in.
    text: String,
    pub refs: Vec<Entry>,
    /// The file as this face last read or wrote it.
    seen: Option<(SystemTime, u64)>,
    dirty: bool,
}

fn sig(path: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

fn read(path: &Path) -> Result<Option<RefsFile>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(serde_json::from_str(&s).with_context(|| format!("parse {}", path.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

impl Index {
    /// The index of the cauldron whose soup dir is `dir`, as on disk. A face
    /// that does not persist reads it and never writes it.
    pub fn open(dir: &Path, writable: bool) -> Result<Self> {
        let path = dir.join(FILE);
        let mut ix = Index { path, writable, text: String::new(), refs: Vec::new(), seen: None, dirty: false };
        ix.seen = sig(&ix.path);
        if let Some(f) = read(&ix.path)? {
            ix.text = f.text;
            ix.refs = f.refs;
        }
        Ok(ix)
    }

    /// An empty index on the file in `dir`, for one that cannot be read:
    /// the next entry written replaces it.
    pub fn fresh(dir: &Path, writable: bool) -> Self {
        Index { path: dir.join(FILE), writable, text: String::new(), refs: Vec::new(), seen: None, dirty: false }
    }

    /// An empty index that touches no file.
    pub fn none() -> Self {
        Index { path: PathBuf::new(), writable: false, text: String::new(), refs: Vec::new(), seen: None, dirty: false }
    }

    /// The text the entries are anchored in.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Carry every entry over to the bucket as it is `now`. The file needs
    /// writing only when an entry moved or went: until then its text and its
    /// anchors still agree with each other, which is all a reader needs.
    pub fn follow_to(&mut self, now: &str) {
        if self.text != now {
            let ats: Vec<usize> = self.refs.iter().map(|e| e.at).collect();
            follow(&self.text, now, &mut self.refs);
            self.dirty |= self.refs.iter().map(|e| e.at).ne(ats);
            self.text = now.to_string();
        }
    }

    /// Record that `tag` sits at byte `at` of `now`, put in from `origin`.
    /// An entry already at that byte is replaced.
    pub fn add(&mut self, now: &str, at: usize, tag: &str, origin: Origin) {
        self.follow_to(now);
        if !now.get(at..).is_some_and(|s| s.starts_with(tag)) {
            return;
        }
        self.refs.retain(|e| e.at != at);
        self.refs.push(Entry { id: uuid::Uuid::new_v4().simple().to_string(), tag: tag.to_string(), at, origin });
        self.refs.sort_by_key(|e| e.at);
        self.dirty = true;
    }

    /// The entry of the reference whose `[` is at byte `at` of `now`.
    pub fn at(&mut self, now: &str, at: usize) -> Option<Entry> {
        self.follow_to(now);
        self.refs.iter().find(|e| e.at == at).cloned()
    }

    /// Take in what another face wrote to the file since this face last
    /// read or wrote it: each entry it holds and this face does not, carried
    /// from its text to `now`. True when something came in.
    pub fn poll(&mut self, now: &str) -> Result<bool> {
        let s = sig(&self.path);
        if s == self.seen || self.path.as_os_str().is_empty() {
            return Ok(false);
        }
        self.seen = s;
        let Some(f) = read(&self.path)? else { return Ok(false) };
        self.follow_to(now);
        let mut theirs: Vec<Entry> = f.refs.into_iter().filter(|e| !self.refs.iter().any(|m| m.id == e.id)).collect();
        follow(&f.text, now, &mut theirs);
        theirs.retain(|e| !self.refs.iter().any(|m| m.at == e.at));
        let came = !theirs.is_empty();
        self.refs.extend(theirs);
        self.refs.sort_by_key(|e| e.at);
        self.dirty |= came;
        Ok(came)
    }

    /// Write the index if it changed, whole and atomically, after taking in
    /// what another face wrote meanwhile.
    pub fn save(&mut self, now: &str) -> Result<()> {
        self.follow_to(now);
        if self.writable {
            self.poll(now)?;
        }
        if !self.dirty || !self.writable {
            return Ok(());
        }
        let f = RefsFile { version: 1, text: self.text.clone(), refs: self.refs.clone() };
        store::write_text_atomic(&self.path, &serde_json::to_string(&f)?)?;
        self.seen = sig(&self.path);
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(tag: &str, at: usize) -> Entry {
        Entry { id: format!("{tag}@{at}"), tag: tag.into(), at, origin: Origin { cluster: "X-1".into(), ctab: "surface".into(), tab: "questions".into(), aggregate: false } }
    }

    #[test]
    fn an_anchor_follows_an_insert_before_it_and_stays_through_one_after() {
        let old = "see [Q-1] now";
        let mut r = vec![e("[Q-1]", 4)];
        follow(old, "and see [Q-1] now", &mut r);
        assert_eq!(r[0].at, 8);
        follow("and see [Q-1] now", "and see [Q-1] now, ok", &mut r);
        assert_eq!(r[0].at, 8);
    }

    #[test]
    fn a_deleted_or_broken_reference_drops_its_entry() {
        let mut r = vec![e("[Q-1]", 0), e("[T-2]", 6)];
        follow("[Q-1] [T-2]", "[Q-1] ", &mut r);
        assert_eq!(r.len(), 1);
        follow("[Q-1] ", "[Q-x1] ", &mut r);
        assert!(r.is_empty(), "a reference typed into is no longer the one put in");
    }

    #[test]
    fn two_same_references_keep_their_own_entries_through_an_edit_between_them() {
        let old = "[Q-1] a [Q-1]";
        let mut r = vec![e("[Q-1]", 0), e("[Q-1]", 8)];
        r[1].origin.cluster = "X-2".into();
        follow(old, "[Q-1] abc [Q-1]", &mut r);
        assert_eq!((r[0].at, r[1].at), (0, 10));
        assert_eq!(r[1].origin.cluster, "X-2");
    }

    #[test]
    fn multibyte_text_before_an_anchor() {
        let mut r = vec![e("[Q-1]", "→ ".len())];
        follow("→ [Q-1]", "→ é → [Q-1]", &mut r);
        assert_eq!(r[0].at, "→ é → ".len());
    }
}
