//! **What the Nodes tab's ARTIFACTS, REPOS and DIRECTORIES bands read** (design 0146; msg 186 merged the Artifacts tab into Soup, msg 235 renamed Soup Contents, 8e4fca10 msg 387 Contents Nodes): the soup's WORKTREE and
//! ARTIFACT nodes, the plain DIRECTORIES a cluster owns, and the disk under
//! all of them. `pages` draws the tab; this is its data and the computed
//! readings over it, never stored twice:
//!
//! - a WORKTREE (`WT-n`, the repo schema, his seam ruling) is what the
//!   machine writes from git — repo, path, branch, head, base, ahead,
//!   behind, dirty, present, last commit. Its STATE is computed: held by a
//!   running task → active; its directory gone → missing; nothing main
//!   lacks → merged; else idle;
//! - an ARTIFACT (`ART-n`, a SOUP node wrapping a file — `designs::ARTIFACT_HOME`,
//!   his answer in msg 186 — in the regions its `regions` name) is
//!   what a maker minted with INTENT (handle, kind, location, maker, what it
//!   is for) and what the machine VERIFIED (sha256, bytes, when). Its state
//!   is computed from the two and the disk; where it sits (a worktree, a
//!   repo) is computed from its path. File bytes never enter a node.
//!
//! The seed is `<seeds>/artifacts.json` (prod) or `<seeds>/soup/<env>.artifacts.json`.
//! A face that reads git (the live loop: [`read_worktrees`], off the UI
//! thread) replaces a repo's seeded worktrees with what git says.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSeed {
    #[serde(default, rename = "_note")]
    pub note: String,
    #[serde(default)]
    pub directories: Vec<Directory>,
    #[serde(default)]
    pub worktrees: Vec<Worktree>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
}

/// A plain directory the registry holds — a repo without git — and the
/// clusters that claim it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Directory {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub claimed_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub sha: String,
    pub at: String,
    pub subject: String,
}

/// A WORKTREE node: machine-written from git.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Worktree {
    pub id: String,
    pub repo: String,
    pub path: String,
    /// None: detached.
    pub branch: Option<String>,
    pub head: String,
    #[serde(default)]
    pub base: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Changed files.
    pub dirty: u32,
    pub present: bool,
    #[serde(default)]
    pub last_commit: Option<Commit>,
}

impl Worktree {
    /// The directory's name: what a row shows and copies by.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// A worktree's state, computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WtState {
    Active,
    Idle,
    Merged,
    Missing,
}

impl WtState {
    pub const ALL: [WtState; 4] = [WtState::Active, WtState::Idle, WtState::Merged, WtState::Missing];
    pub fn word(self) -> &'static str {
        match self {
            WtState::Active => "ACTIVE",
            WtState::Idle => "IDLE",
            WtState::Merged => "MERGED",
            WtState::Missing => "MISSING",
        }
    }
    pub fn glyph(self) -> &'static str {
        match self {
            WtState::Active => "▶",
            WtState::Idle => "○",
            WtState::Merged => "✓",
            WtState::Missing => "✕",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Verified {
    pub sha256: String,
    pub bytes: u64,
    pub at: String,
}

/// An ARTIFACT node: intent, then verified fact.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub id: String,
    pub handle: String,
    /// report | task | page | deck | doc | data | log | … (open vocabulary)
    pub kind: String,
    /// The intended path: the one stored fact about place.
    pub location: String,
    /// A run, a ripple, or the operator; a task as its soup id (`T-3`).
    pub made_by: String,
    /// The node it is for, by its soup id (`T-3`).
    #[serde(rename = "for")]
    pub for_: String,
    pub created: String,
    #[serde(default)]
    pub verified: Option<Verified>,
    #[serde(default)]
    pub supersedes: Option<String>,
    /// The soup regions it sits in (8e4fca10 msg 186: an artifact is "a node
    /// that wraps around like a file" — a soup node like any other).
    #[serde(default)]
    pub regions: Vec<String>,
}

impl Artifact {
    /// The file's name without its extension: what a row shows.
    pub fn name(&self) -> &str {
        let f = self.location.rsplit('/').next().unwrap_or(&self.location);
        f.rsplit_once('.').map_or(f, |(n, _)| n)
    }
    /// The cluster its `for` node is on: the one that holds it.
    pub fn cluster(&self, forest: &crate::cauldron::model::Forest) -> Option<crate::cauldron::model::ClusterId> {
        let id = self.for_.split('.').next().unwrap_or(&self.for_);
        crate::cauldron::model::Local::parse(id).and_then(|l| forest.owner(l))
    }
}

/// An artifact's state, computed (0146 §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtState {
    /// Intent and no fact, and its maker still running.
    BeingWritten,
    Present,
    /// The file changed after the machine verified it.
    Stale,
    Missing,
}

impl ArtState {
    pub fn glyph(self) -> &'static str {
        match self {
            ArtState::BeingWritten => "▶",
            ArtState::Present => "◈",
            ArtState::Stale => "◇",
            ArtState::Missing => "✕",
        }
    }
}

/// A band name for an artifact kind, in the band case: `Reports`, `Tasks`, `Decks`.
pub fn kind_band(kind: &str) -> String {
    let k = crate::cauldron::look::sentence(kind);
    if k.ends_with('s') {
        k
    } else {
        crate::cauldron::look::plural_of(&k)
    }
}

/// The seed for environment `env`: the cauldron's own (`<dir>/seed/artifacts.json`,
/// see `store::own_seed`) if it carries one, else the repo's.
pub fn load(seeds: &Path, env: &str, dir: &Path) -> Result<ArtifactSeed> {
    let own = seeds.join("soup").join(format!("{env}.artifacts.json"));
    let p = if let Some(mine) = crate::cauldron::store::own_seed(dir, "artifacts.json") {
        mine
    } else if own.is_file() {
        own
    } else if env == "prod" {
        seeds.join("artifacts.json")
    } else {
        return Ok(ArtifactSeed::default());
    };
    if !p.is_file() {
        return Ok(ArtifactSeed::default());
    }
    let text = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))
}

/// The state of an artifact given whether its maker is running.
pub fn art_state(a: &Artifact, maker_running: bool) -> ArtState {
    let md = std::fs::metadata(&a.location).ok();
    match (&a.verified, md) {
        (None, _) if maker_running => ArtState::BeingWritten,
        (None, _) => ArtState::Missing,
        (Some(_), None) => ArtState::Missing,
        (Some(v), Some(md)) => {
            let verified = chrono::DateTime::parse_from_rfc3339(&v.at).ok().map(|t| t.timestamp());
            let mtime = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
            match (verified, mtime) {
                (Some(v), Some(m)) if m > v + 1 => ArtState::Stale,
                _ => ArtState::Present,
            }
        }
    }
}

// ── the disk ───────────────────────────────────────────────────────────────

/// One entry of a directory, as a FILES row shows it.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub dir: bool,
    /// Bytes of a file; entries of a directory.
    pub size: u64,
    /// Seconds since it changed.
    pub age_s: u64,
}

/// `path`'s entries, directories first, each group by name; hidden ones
/// left out. Read once per open directory per frame, never the whole tree.
pub fn list_dir(path: &str) -> Vec<DirEntry> {
    let Ok(rd) = std::fs::read_dir(path) else { return Vec::new() };
    let now = std::time::SystemTime::now();
    let mut out: Vec<DirEntry> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                return None;
            }
            let md = e.metadata().ok()?;
            let dir = md.is_dir();
            let size = if dir { std::fs::read_dir(e.path()).map_or(0, |r| r.count() as u64) } else { md.len() };
            let age_s = md.modified().ok().and_then(|m| now.duration_since(m).ok()).map_or(0, |d| d.as_secs());
            Some(DirEntry { name, path: e.path().display().to_string(), dir, size, age_s })
        })
        .collect();
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.name.cmp(&b.name)));
    out
}

/// `2m`, `3h`, `4d`, `2w`.
pub fn short_age(s: u64) -> String {
    match s {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s if s < 14 * 86_400 => format!("{}d", s / 86_400),
        s => format!("{}w", s / (7 * 86_400)),
    }
}

/// `412 B`, `1.2 KB`, `188 KB`, `4.1 MB`.
pub fn size_word(b: u64) -> String {
    match b {
        b if b < 1024 => format!("{b} B"),
        b if b < 10 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b if b < 1024 * 1024 => format!("{} KB", b / 1024),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

// ── git, for a face that reads it ──────────────────────────────────────────

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").arg("-C").arg(cwd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Every LINKED worktree of the repo at `repo_path` (the primary checkout
/// is the repo itself, never a worktree), read from git: `worktree list
/// --porcelain`, then per worktree its merge base with `main`, how far
/// ahead and behind it is, its changed files and its last commit. Slow on a
/// repo with many worktrees: the live loop runs it OFF the UI thread and
/// the frame shows the last result. None when `repo_path` is not a repo.
pub fn read_worktrees(repo: &str, repo_path: &str, main: &str) -> Option<Vec<Worktree>> {
    let root = PathBuf::from(repo_path);
    let list = git(&root, &["worktree", "list", "--porcelain"])?;
    let mut out = Vec::new();
    for (n, block) in list.split("\n\n").enumerate().skip(1) {
        let mut path = None;
        let mut head = None;
        let mut branch = None;
        for l in block.lines() {
            if let Some(p) = l.strip_prefix("worktree ") {
                path = Some(p.to_string());
            } else if let Some(h) = l.strip_prefix("HEAD ") {
                head = Some(h.to_string());
            } else if let Some(b) = l.strip_prefix("branch ") {
                branch = Some(b.trim_start_matches("refs/heads/").to_string());
            }
        }
        let (Some(path), Some(head)) = (path, head) else { continue };
        let present = Path::new(&path).is_dir();
        let base = git(&root, &["merge-base", main, &head]);
        let (behind, ahead) = git(&root, &["rev-list", "--left-right", "--count", &format!("{main}...{head}")])
            .and_then(|s| {
                let mut it = s.split_whitespace().map(|x| x.parse::<u32>().ok());
                Some((it.next()??, it.next()??))
            })
            .unwrap_or((0, 0));
        let dirty = if present { git(Path::new(&path), &["status", "--porcelain"]).map_or(0, |s| s.lines().filter(|l| !l.trim().is_empty()).count() as u32) } else { 0 };
        let last_commit = git(&root, &["log", "-1", "--format=%H%x09%ct%x09%s", &head]).and_then(|s| {
            let mut it = s.splitn(3, '\t');
            let sha = it.next()?.to_string();
            let ct: i64 = it.next()?.parse().ok()?;
            let at = chrono::DateTime::from_timestamp(ct, 0)?.to_rfc3339();
            Some(Commit { sha, at, subject: it.next().unwrap_or("").to_string() })
        });
        out.push(Worktree { id: format!("WT-{n}"), repo: repo.to_string(), path, branch, head, base, ahead, behind, dirty, present, last_commit });
    }
    Some(out)
}
