//! # `cauldron` — the project registry
//!
//! One small JSON document, `~/.cauldron/projects.json`, is the list of his
//! projects: an ordered array of `{name, path, registered_at}`. This crate owns
//! its schema, its locked atomic write, its bootstrap from the `~/minds` relic,
//! the synthesized `miscellaneous` bucket, and every refusal sentence either
//! face speaks about a project. Design: `0071-report-design-cauldron-registry`.
//!
//! ## THE ROOT IS A PARAMETER
//!
//! Every function that touches the filesystem takes `home: &Path`. There is
//! exactly ONE call to `dirs::home_dir()` in the crate — [`home_dir`] — and it
//! sits at the edge for a binary's `main` to call once. Nothing deeper ever
//! reaches for it.
//!
//! That is not a style preference. `~/.cauldron` is HIS directory, it already
//! holds `plugins/`, and the AUI mints the real registry on its own first boot
//! after he publishes it. A crate that resolved `$HOME` inside `save()` could
//! only be tested by writing into that directory — so it would not be tested,
//! and the first exercise of the write path would be the one that mattered.
//! With the root as a parameter every test here runs against a `tempfile`
//! root and the real registry is minted exactly once, by him, later.
//!
//! ## What is in the file and what is not
//!
//! In: `version`, an optional `bootstrap` provenance block, and the ordered
//! `projects` array. **Array position IS the order** (§5) — there is no rank
//! column, because the document is always rewritten whole and a rank would be
//! a second copy of the array index that could disagree with it.
//!
//! Not in: the archive set (stays in `~/.aui/archived-projects.json`), model
//! overrides (`settings.json`), and the bucket (§4 — synthesized, see
//! [`BUCKET`]).
//!
//! ## The two write protocols, and why there are two
//!
//! [`Registry::update`] mutates an EXISTING file: take the advisory lock,
//! load, apply, write-temp/fsync/rename, unlock. The lock serializes writers;
//! the rename makes every reader see the old document or the new one.
//!
//! [`Registry::bootstrap_if_absent`] CREATES the file, and takes no lock at
//! all. Creation is `hard_link(temp, projects.json)`, which fails with
//! `EEXIST` atomically — so the first process to reach that line wins and
//! every loser deletes its temp and reloads the winner. A lock could not do
//! this job: two processes that both find the file absent would both derive,
//! and the second would overwrite the first's document with its own.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod refusal;
mod snapshot;

pub use refusal::*;
pub use snapshot::{Snapshot, SnapshotStatus};

// ──────────────────────────────────────────────────────────────────────────
//  Constants and paths
// ──────────────────────────────────────────────────────────────────────────

/// **The bucket's one canonical spelling**, on the wire, in the file, and in
/// every sentence: `miscellaneous`, his spelling.
///
/// `0071` §4: the bucket is SYNTHESIZED, not registered. The file never
/// contains it, so an empty registry still has it, no write can remove or move
/// it, and no writer has to remember to preserve it. The alternative — a
/// flagged entry in the array — can be deleted or reordered by a hand edit and
/// is absent from a registry whose bootstrap died before writing it.
///
/// The AUI spells it `_miscellaneous` internally (a phone node key, three
/// mirrored constants) and maps at ONE boundary; that is a recorded drag, not
/// this crate's business. A name that normalizes onto this one is REFUSED at
/// registration — see [`is_reserved_name`].
pub const BUCKET: &str = "miscellaneous";

/// The schema version this crate writes and the only one it reads.
/// A reader that meets a version it does not know REFUSES in a sentence rather
/// than guessing; that is the whole job of the field.
pub const VERSION: u32 = 1;

/// Temps older than this are strays from a crashed run and are swept.
/// Sixty seconds is far longer than a 4 KB write and far shorter than the gap
/// between two boots, so it cannot sweep a live writer's temp.
const STRAY_TEMP_AGE: Duration = Duration::from_secs(60);

/// `~/.cauldron` — his directory; it already holds `plugins/`.
pub fn cauldron_dir(home: &Path) -> PathBuf {
    home.join(".cauldron")
}

/// `~/.cauldron/projects.json` — the registry.
pub fn registry_path(home: &Path) -> PathBuf {
    cauldron_dir(home).join("projects.json")
}

/// `~/.cauldron/projects.lock` — an empty file, advisory exclusive, held only
/// across a read-modify-write.
pub fn lock_path(home: &Path) -> PathBuf {
    cauldron_dir(home).join("projects.lock")
}

/// `~/minds` — **the relic**. Read exactly once per machine, by the bootstrap.
/// Never written by this crate or any new code (`0071` §7).
pub fn minds_root(home: &Path) -> PathBuf {
    home.join("minds")
}

/// `~/.aui/miscellaneous` — the bucket's root, and it stays there: 61 daemon
/// threads have that cwd and the whole `.mind/threads/` tree lives under it, so
/// moving it would be a migration with no ask behind it (`0071` §4).
pub fn bucket_root(home: &Path) -> PathBuf {
    home.join(".aui").join(BUCKET)
}

/// **The crate's only `$HOME` lookup.** Call it once, in `main`, and pass the
/// result down. See the module note "THE ROOT IS A PARAMETER".
pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

// ──────────────────────────────────────────────────────────────────────────
//  The document
// ──────────────────────────────────────────────────────────────────────────

/// One registered project. `name` is the key; `path` is what it resolves to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The KEY. Stored, never derived from `path`: the bootstrap must keep
    /// `broomstick-splash` for a target named `…-redesign`.
    pub name: String,
    /// Absolute and canonical as of registration.
    pub path: PathBuf,
    /// RFC 3339. The one field a human can use to reconstruct what happened.
    pub registered_at: DateTime<Utc>,
}

/// Where the list came from, for the human who opens the file when something
/// is wrong. **Never consulted for idempotence** (`0071` §6) — a marker inside
/// a file can be inside a truncated file; the trigger is the file's absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bootstrap {
    pub from: PathBuf,
    pub at: DateTime<Utc>,
    pub by: String,
}

/// The whole document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
    /// Absent on a machine that never had `~/minds`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap: Option<Bootstrap>,
    projects: Vec<Entry>,
}

/// A row as a face draws it: the registered entries in registry order, then
/// the bucket, always last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub path: PathBuf,
    /// The synthesized bucket. Drawn italic; never archivable, removable or
    /// counted in the band header.
    pub bucket: bool,
    /// The registered directory is not there any more. **The row stays**
    /// (`0071` §3): a checkout that moved is a fact to show him, not a reason
    /// to silently shorten his list. Always `false` for the bucket.
    pub missing: bool,
}

/// Which of the two sources a view came from. The TUI draws the difference in
/// its header (`registry not created yet`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    File,
    DerivedFromMinds,
}

/// What [`Registry::bootstrap_if_absent`] did.
///
/// **THREE OUTCOMES, NOT TWO — a deliberate deviation from `0071` §6.** The
/// design distinguishes "created" from "already present" because that is what
/// the AUI needs to decide whether to speak its one-time startup sentence.
/// `LostRace` is split out of `AlreadyPresent` for a different reason: it is
/// the only way a test can tell "I never raced anybody" from "I raced and
/// lost", and without it the concurrency proof would have to INFER contention
/// from timing instead of witnessing it. A caller that does not care treats
/// the two alike — [`Bootstrapped::was_created`] is the only question the AUI
/// asks.
#[derive(Debug)]
pub enum Bootstrapped {
    /// This process wrote the file. Say the one-time startup sentence.
    Created(Registry),
    /// The file was there before this call derived anything. A stat and a
    /// return: `~/minds` was never read.
    AlreadyPresent(Registry),
    /// This process derived, wrote its temp, and LOST the `hard_link` — another
    /// process created the file first. The temp is deleted and the winner's
    /// document is what came back. Nothing of this process's reached the file.
    LostRace(Registry),
}

impl Bootstrapped {
    pub fn registry(&self) -> &Registry {
        match self {
            Bootstrapped::Created(r)
            | Bootstrapped::AlreadyPresent(r)
            | Bootstrapped::LostRace(r) => r,
        }
    }
    /// The one question a caller asks: do I speak the startup sentence?
    pub fn was_created(&self) -> bool {
        matches!(self, Bootstrapped::Created(_))
    }
    /// This process derived and then lost the atomic creation. Proof material.
    pub fn lost_race(&self) -> bool {
        matches!(self, Bootstrapped::LostRace(_))
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  Errors
// ──────────────────────────────────────────────────────────────────────────

/// The file is present and this crate will not act on it.
///
/// **An unreadable registry is NOT fail-open** (`0071` §2). The archive set
/// fails open because "empty" there means "hide nothing"; here "empty" would
/// mean "he has no projects", which is the dangerous direction — and a writer
/// that fell open to empty would then SAVE that emptiness over his list. So a
/// bad file is an error, the bytes are never touched on read, and
/// [`Registry::update`] refuses every write until it parses.
#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Parse(serde_json::Error),
    UnknownVersion(u32),
    DuplicateName(String),
    DuplicatePath(PathBuf),
    /// The bucket's name found inside the array — a hand edit, or a writer
    /// that did not exist when this was written.
    BucketRegistered,
}

impl LoadError {
    /// A sentence for him, not a debug dump.
    pub fn sentence(&self) -> String {
        match self {
            LoadError::Io(e) => format!("I could not read your project registry: {e}."),
            LoadError::Parse(e) => {
                format!("Your project registry is not valid JSON: {e}.")
            }
            LoadError::UnknownVersion(v) => format!(
                "Your project registry is version {v} and I only know version {VERSION}."
            ),
            LoadError::DuplicateName(n) => {
                format!("Your project registry lists `{n}` twice.")
            }
            LoadError::DuplicatePath(p) => format!(
                "Your project registry lists `{}` under two names.",
                p.display()
            ),
            LoadError::BucketRegistered => format!(
                "Your project registry contains `{BUCKET}`, which is the bucket and not a project."
            ),
        }
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.sentence())
    }
}

impl std::error::Error for LoadError {}

/// A write that did not happen.
#[derive(Debug)]
pub enum UpdateError {
    /// The file is there and unreadable. **Nothing was written** — the crate
    /// cannot clobber what it could not read.
    Load(LoadError),
    /// The closure refused. Nothing was written; the sentence is his.
    Refused(Refusal),
    Io(io::Error),
}

impl UpdateError {
    pub fn sentence(&self) -> String {
        match self {
            UpdateError::Load(e) => e.sentence(),
            UpdateError::Refused(r) => r.sentence().to_string(),
            UpdateError::Io(e) => format!("I could not write your project registry: {e}."),
        }
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.sentence())
    }
}

impl std::error::Error for UpdateError {}

impl From<io::Error> for UpdateError {
    fn from(e: io::Error) -> Self {
        UpdateError::Io(e)
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  Name rules
// ──────────────────────────────────────────────────────────────────────────

/// **Is this name a spelling of the bucket?**
///
/// Deliberately BROADER than exact comparison and deliberately NOT the AUI's
/// `normalize_project_name`. `0071` §3 requires `_miscellaneous` and
/// `Miscellaneous` to be refused as well as `miscellaneous`, and this crate
/// does not own the normalizer (see [`Registry::register`]'s note). So the test
/// is a collision-class one: lowercase, drop every non-alphanumeric, compare.
///
/// It over-reserves a handful of odd spellings (`mis-cellaneous`). Reserving
/// them costs nothing; letting one through costs him a second row in his list
/// named after his bucket.
pub fn is_reserved_name(name: &str) -> bool {
    let squashed: String = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect();
    squashed == BUCKET
}

/// **What a project name may not be**, returning the objection as a clause for
/// [`unusable_name`], or `None` if the name is usable.
///
/// This is SAFETY, not naming policy. A project name is used as a filesystem
/// component in `~/.aui/replicas/<project>/`, as a settings key, and as a
/// phone node key. A name containing a path separator or spelled `..` would
/// make one of those escape its directory. Everything else — spaces, unicode,
/// underscores — is allowed, because `type_3` must stay `type_3` and he names
/// his own things.
pub fn name_objection(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("it is empty");
    }
    if name.contains('/') || name.contains('\\') {
        return Some("a name cannot contain a path separator");
    }
    if name == "." || name == ".." {
        return Some("that is a directory, not a name");
    }
    if name.starts_with('.') {
        return Some("a name cannot start with a dot");
    }
    if name.chars().any(|c| c.is_control()) {
        return Some("it contains a control character");
    }
    None
}

// ──────────────────────────────────────────────────────────────────────────
//  Registry — reading
// ──────────────────────────────────────────────────────────────────────────

impl Registry {
    /// An empty registry with no provenance. Not written anywhere by itself.
    pub fn empty() -> Registry {
        Registry {
            version: VERSION,
            bootstrap: None,
            projects: Vec::new(),
        }
    }

    /// **A registry that never touches a disk — for tests and for the AUI's
    /// `Config` fixtures.** `0071` §8 names it so the ~15 test constructions of
    /// `Config { minds_root: scratch }` have somewhere to land.
    pub fn in_memory<P: Into<PathBuf>>(entries: Vec<(&str, P)>) -> Registry {
        let now = Utc::now();
        Registry {
            version: VERSION,
            bootstrap: None,
            projects: entries
                .into_iter()
                .map(|(name, path)| Entry {
                    name: name.to_string(),
                    path: path.into(),
                    registered_at: now,
                })
                .collect(),
        }
    }

    /// The ordered entries. Array position is the rank.
    pub fn entries(&self) -> &[Entry] {
        &self.projects
    }

    /// Registered names in registry order. **The bucket is not among them** —
    /// it is not a project and is not counted in the band header (`0071` §12).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.projects.iter().map(|e| e.name.as_str())
    }

    pub fn len(&self) -> usize {
        self.projects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.projects.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.projects.iter().find(|e| e.name == name)
    }

    /// **The list as a face draws it**: registered entries in order, then the
    /// bucket, always last, always present.
    ///
    /// Takes `home` because the bucket's root is derived from it, and because
    /// `missing` is a statement about the filesystem. (`0071` §8 wrote this
    /// `view(&self)`; the bucket row cannot be built without a root, so the
    /// root is a parameter here as it is everywhere else.)
    pub fn view(&self, home: &Path) -> Vec<Row> {
        let mut rows: Vec<Row> = self
            .projects
            .iter()
            .map(|e| Row {
                name: e.name.clone(),
                path: e.path.clone(),
                bucket: false,
                missing: !e.path.is_dir(),
            })
            .collect();
        rows.push(Row {
            name: BUCKET.to_string(),
            path: bucket_root(home),
            bucket: true,
            // Never `missing`: the bucket's root is created on demand by the
            // face that writes into it, and an absent one is not a fact about
            // his projects.
            missing: false,
        });
        rows
    }

    /// **The one resolver.** Bucket → `~/.aui/miscellaneous`; a registered name
    /// → its path; anything else → `None`.
    ///
    /// The `Option` is the point. `0071` §8 and §11: an infallible lookup would
    /// need a value for an unknown name and there is no honest one —
    /// `ThreadIndex::save` creates `.mind/threads.json` under whatever root it
    /// is handed, so a made-up path is a silent write into a new tree. The
    /// `None` makes the compiler enumerate every site that assumed a name
    /// always resolves, which is exactly the assumption the registry retires.
    pub fn resolve(&self, home: &Path, name: &str) -> Option<PathBuf> {
        if name == BUCKET {
            return Some(bucket_root(home));
        }
        self.get(name).map(|e| e.path.clone())
    }

    /// Read the file. `Ok(None)` when it is absent; `Err` when it is present
    /// and this crate will not act on it (see [`LoadError`]).
    pub fn load(home: &Path) -> Result<Option<Registry>, LoadError> {
        let path = registry_path(home);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(LoadError::Io(e)),
        };
        Registry::from_json(&text).map(Some)
    }

    /// Parse and VALIDATE. Exposed so a caller can check a candidate document
    /// without a file — and so the validation has exactly one definition.
    pub fn from_json(text: &str) -> Result<Registry, LoadError> {
        let reg: Registry = serde_json::from_str(text).map_err(LoadError::Parse)?;
        if reg.version != VERSION {
            return Err(LoadError::UnknownVersion(reg.version));
        }
        let mut names: BTreeSet<&str> = BTreeSet::new();
        let mut paths: BTreeSet<&Path> = BTreeSet::new();
        for e in &reg.projects {
            if e.name == BUCKET {
                return Err(LoadError::BucketRegistered);
            }
            if !names.insert(e.name.as_str()) {
                return Err(LoadError::DuplicateName(e.name.clone()));
            }
            if !paths.insert(e.path.as_path()) {
                return Err(LoadError::DuplicatePath(e.path.clone()));
            }
        }
        Ok(reg)
    }

    /// **The registry if it exists, otherwise today's `~/minds` list.**
    ///
    /// The coexistence window (`0071` §10): the TUI ships before the AUI mints
    /// the file, and in that window it must show byte-for-byte the list it
    /// showed yesterday. It gets that by calling the bootstrap's OWN derivation
    /// — one derivation, two uses, never two definitions.
    pub fn load_or_derive(home: &Path) -> Result<(Registry, Source), LoadError> {
        match Registry::load(home)? {
            Some(r) => Ok((r, Source::File)),
            None => Ok((
                Registry::derive_from_minds(home, "derived"),
                Source::DerivedFromMinds,
            )),
        }
    }

    /// **The derivation, and it writes nothing.**
    ///
    /// Every non-dot entry of `~/minds` whose FOLLOWED metadata is a directory
    /// — exactly `apps/tui/src/index.rs:175-198`'s and
    /// `apps/aui/src/state.rs:401`'s predicate, so a dangling link is skipped
    /// here as it is invisible there and the day the registry lands nothing
    /// appears that was not on his screen yesterday.
    ///
    /// The name is the basename **VERBATIM**. `type_3` must not become
    /// `type-3`: normalizing would re-key its settings override and its replica
    /// directory and detach nothing visibly until something failed.
    ///
    /// The path is the canonicalized symlink target (26 of his 32 point under
    /// `~/code`), or the entry itself for a real directory — none exist today,
    /// and if one ever does, the relic is load-bearing for it and the row says
    /// so honestly.
    pub fn derive_from_minds(home: &Path, by: &str) -> Registry {
        let root = minds_root(home);
        let now = Utc::now();
        let mut projects: Vec<Entry> = Vec::new();

        if let Ok(rd) = fs::read_dir(&root) {
            for ent in rd.flatten() {
                let Some(name) = ent.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let entry_path = ent.path();
                // TODAY'S PREDICATE, unchanged: follow, then ask if it is a
                // directory. `file_type()` would call `harness -> code/harness`
                // a symlink and skip all 32.
                match fs::metadata(&entry_path) {
                    Ok(md) if md.is_dir() => {}
                    _ => continue,
                }
                let is_link = fs::symlink_metadata(&entry_path)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false);
                let path = if is_link {
                    match fs::canonicalize(&entry_path) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(
                                entry = %entry_path.display(),
                                error = %e,
                                "minds entry could not be resolved; skipped"
                            );
                            continue;
                        }
                    }
                } else {
                    entry_path
                };
                projects.push(Entry {
                    name,
                    path,
                    registered_at: now,
                });
            }
        }

        // Byte order, as `discover_roots` sorts (`index.rs:190`).
        projects.sort_by(|a, b| a.name.cmp(&b.name));

        Registry {
            version: VERSION,
            bootstrap: Some(Bootstrap {
                from: root,
                at: now,
                by: by.to_string(),
            }),
            projects,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  Registry — mutating (in memory; nothing here touches a disk)
// ──────────────────────────────────────────────────────────────────────────

impl Registry {
    /// **Register an existing directory.**
    ///
    /// Validated in `0071` §8's order, refusing at the first failure: the path
    /// exists → is a directory → canonicalize → not already registered by path
    /// → name → not empty → not reserved → not taken. The entry lands LAST; he
    /// moves it if he wants it elsewhere.
    ///
    /// **NOTE ON NORMALIZATION — a deliberate deviation from `0071` §8.** The
    /// design says the derived name is `normalize_project_name(basename)`. That
    /// normalizer is D21a's ONE definition site
    /// (`libs/aui-state/src/server/mod.rs:377`) and it must stay one: a second
    /// copy here would let the name he heard confirmed differ from the name on
    /// disk, which is the exact failure D21a exists to prevent. This crate
    /// cannot call it either — `aui-state`'s `server` feature is off in the
    /// TUI's build. So the CALLER normalizes and passes `Some(name)`; `None`
    /// means "use the basename verbatim", which is what the bootstrap does and
    /// what an agent-facing `cauldron register <path>` wants. What this crate
    /// does own is [`name_objection`] — the safety floor no normalizer can be
    /// trusted to have applied.
    pub fn register(&mut self, path: &Path, name: Option<&str>) -> Result<&Entry, Refusal> {
        let meta = match fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return Err(nothing_at_path(path)),
        };
        if !meta.is_dir() {
            return Err(not_a_directory(path));
        }
        let canonical = fs::canonicalize(path).map_err(|_| nothing_at_path(path))?;
        if let Some(existing) = self.projects.iter().find(|e| e.path == canonical) {
            return Err(path_already_registered(&canonical, &existing.name));
        }
        let name: String = match name {
            Some(n) => n.to_string(),
            None => match canonical.file_name().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => return Err(empty_name()),
            },
        };
        if name.trim().is_empty() {
            return Err(empty_name());
        }
        if let Some(why) = name_objection(&name) {
            return Err(unusable_name(&name, why));
        }
        if is_reserved_name(&name) {
            return Err(name_reserved(&name));
        }
        if self.projects.iter().any(|e| e.name == name) {
            return Err(name_taken(&name));
        }
        self.projects.push(Entry {
            name,
            path: canonical,
            registered_at: Utc::now(),
        });
        Ok(self.projects.last().expect("just pushed"))
    }

    /// **De-register: removes the ENTRY and touches nothing else.**
    ///
    /// Not the directory, not `.mind/`, not `.mind/threads.json`, not the
    /// archive set, not `~/minds/<name>` (`0071` §3). The threads stay on disk
    /// and come back if he re-registers the same path under the same name.
    /// This function cannot do otherwise: it has no filesystem in it.
    pub fn remove(&mut self, name: &str) -> Result<Entry, Refusal> {
        if name == BUCKET {
            return Err(cannot_remove_bucket());
        }
        match self.projects.iter().position(|e| e.name == name) {
            Some(i) => Ok(self.projects.remove(i)),
            None => Err(no_such_project(name)),
        }
    }

    /// **Move `project` so it sits immediately before `before`; `None` means
    /// last.**
    ///
    /// The destination is a NEIGHBOUR'S NAME, never an index, so a face drawing
    /// a list that is a second old names a project and not a slot.
    ///
    /// **A `before` that is no longer in the list is REFUSED.** This is the
    /// defect `0071` §15 records in `move_pin`
    /// (`apps/aui/src/v2/mutate.rs:711`, `unwrap_or(next.len())`): a stale
    /// target there silently drops the moved row to the END, so the move looks
    /// like it worked and the row is not where he put it. Refusing hands the
    /// face a sentence and a reason to re-read. Validation happens BEFORE any
    /// mutation, so a refusal leaves the order untouched.
    pub fn move_before(&mut self, name: &str, before: Option<&str>) -> Result<(), Refusal> {
        let from = self
            .projects
            .iter()
            .position(|e| e.name == name)
            .ok_or_else(|| no_such_project(name))?;
        if let Some(b) = before {
            if b == name {
                return Err(move_before_self(name));
            }
            if !self.projects.iter().any(|e| e.name == b) {
                return Err(stale_before(b));
            }
        }
        let entry = self.projects.remove(from);
        let to = match before {
            None => self.projects.len(),
            Some(b) => self
                .projects
                .iter()
                .position(|e| e.name == b)
                .expect("checked above, and nothing ran between"),
        };
        self.projects.insert(to, entry);
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  Registry — writing
// ──────────────────────────────────────────────────────────────────────────

impl Registry {
    /// Serialize exactly as the file is written — pretty, one entry per line,
    /// trailing newline. Exposed so a test can compare bytes.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("registry is plain data");
        s.push('\n');
        s
    }

    /// **Write the whole document, atomically, under the lock.**
    ///
    /// Prefer [`Registry::update`] — `save` writes what the caller has in hand,
    /// so a caller holding a stale copy overwrites a change it never saw.
    /// `update` cannot do that, because it reloads inside the lock.
    pub fn save(&self, home: &Path) -> Result<(), UpdateError> {
        let dir = cauldron_dir(home);
        fs::create_dir_all(&dir)?;
        let mut lock = Lock::acquire(home)?;
        let _guard = lock.write()?;
        self.write_atomically(&dir)
    }

    /// The write half, with the lock already held.
    fn write_atomically(&self, dir: &Path) -> Result<(), UpdateError> {
        let tmp = temp_path(dir);
        {
            let mut f = fs::File::create(&tmp)?;
            io::Write::write_all(&mut f, self.to_json().as_bytes())?;
            // fsync BEFORE the rename. Without it the rename can be durable
            // while the bytes are not, and a power loss leaves the final name
            // pointing at a file of zeroes — a torn document reached without
            // anybody writing one.
            f.sync_all()?;
        }
        let final_path = dir.join("projects.json");
        if let Err(e) = fs::rename(&tmp, &final_path) {
            let _ = fs::remove_file(&tmp);
            return Err(UpdateError::Io(e));
        }
        // The directory entry itself, so the rename survives a power loss too.
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }

    /// **The mutation path: lock → load → apply → write → unlock.**
    ///
    /// Every write is a read-modify-write against the FILE, so an in-memory
    /// cache can never clobber a change it did not see, and two writers
    /// serialize on the lock rather than on luck.
    ///
    /// A refusal from `f` releases the lock and writes nothing. A file that is
    /// present and unreadable is [`UpdateError::Load`] and writes nothing
    /// either — the crate cannot clobber what it could not read (`0071` §2).
    /// A file that is ABSENT starts from an empty registry: that is the
    /// agent-facing `cauldron register` on a machine the AUI has not booted
    /// yet, and an empty document with his one project in it is correct.
    pub fn update<T, F>(home: &Path, f: F) -> Result<T, UpdateError>
    where
        F: FnOnce(&mut Registry) -> Result<T, Refusal>,
    {
        let dir = cauldron_dir(home);
        fs::create_dir_all(&dir)?;
        let mut lock = Lock::acquire(home)?;
        let _guard = lock.write()?;

        let mut reg = match Registry::load(home) {
            Ok(Some(r)) => r,
            Ok(None) => Registry::empty(),
            Err(e) => return Err(UpdateError::Load(e)),
        };
        let out = f(&mut reg).map_err(UpdateError::Refused)?;
        reg.write_atomically(&dir)?;
        Ok(out)
    }

    /// **Mint the registry if and only if it is absent.**
    ///
    /// The trigger is the file's ABSENCE and nothing else. The file comes into
    /// existence only by `hard_link` of a fully written, fsynced temp, so
    /// *exists* and *complete* are the same predicate: a second run is a `stat`
    /// and a return, and the only code path that reads `~/minds` sits behind
    /// that `stat`. The `bootstrap` block inside the document is provenance for
    /// humans and is never consulted here — a marker inside a file can be
    /// inside a truncated file.
    ///
    /// **No lock is taken.** `hard_link` fails with `EEXIST` atomically, so the
    /// first process to reach it wins and every loser deletes its temp and
    /// reloads the winner. A lock would not help: two processes that both saw
    /// the file absent would both derive, and the second would overwrite the
    /// first.
    pub fn bootstrap_if_absent(home: &Path, by: &str) -> Result<Bootstrapped, UpdateError> {
        let dir = cauldron_dir(home);
        let final_path = dir.join("projects.json");

        // 1. Present → a stat and a return.
        if final_path.exists() {
            return Ok(Bootstrapped::AlreadyPresent(load_existing(home)?));
        }

        fs::create_dir_all(&dir)?;

        // 2. Sweep strays from a crashed run.
        sweep_stray_temps(&dir, STRAY_TEMP_AGE);

        // 3. Derive. Reads `~/minds`; writes nothing.
        let reg = Registry::derive_from_minds(home, by);

        // 4. Fully written and fsynced, under a name nothing reads.
        let tmp = temp_path(&dir);
        {
            let mut f = fs::File::create(&tmp)?;
            io::Write::write_all(&mut f, reg.to_json().as_bytes())?;
            f.sync_all()?;
        }

        // 5. The one atomic instant.
        match fs::hard_link(&tmp, &final_path) {
            Ok(()) => {
                let _ = fs::remove_file(&tmp);
                if let Ok(d) = fs::File::open(&dir) {
                    let _ = d.sync_all();
                }
                Ok(Bootstrapped::Created(reg))
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // LOST, CLEANLY: delete our temp, read the winner's bytes, and
                // report that we wrote nothing.
                let _ = fs::remove_file(&tmp);
                Ok(Bootstrapped::LostRace(load_existing(home)?))
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(UpdateError::Io(e))
            }
        }
    }
}

/// The file is supposed to be there. A `None` here means it vanished between
/// the `stat` and the read, which is a read error, not an empty registry.
fn load_existing(home: &Path) -> Result<Registry, UpdateError> {
    match Registry::load(home) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(UpdateError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "the registry vanished while it was being read",
        ))),
        Err(e) => Err(UpdateError::Load(e)),
    }
}

/// `projects.json.tmp-<pid>-<nanos>` — in the same directory, so the rename and
/// the link are same-filesystem operations, and unique per process AND per call
/// so two writers never share a temp.
fn temp_path(dir: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("projects.json.tmp-{}-{}", std::process::id(), nanos))
}

/// **Strays are swept and never read.** A crash between the write and the link
/// leaves a temp and no registry; the next boot deletes it and bootstraps
/// again. Nothing ever reads a `tmp-` file — that is what makes a half-written
/// one harmless rather than dangerous.
fn sweep_stray_temps(dir: &Path, older_than: Duration) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    for ent in rd.flatten() {
        let name = ent.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("projects.json.tmp-") {
            continue;
        }
        let stale = ent
            .metadata()
            .and_then(|m| m.modified())
            .map(|m| now.duration_since(m).unwrap_or_default() >= older_than)
            .unwrap_or(true);
        if stale {
            match fs::remove_file(ent.path()) {
                Ok(()) => tracing::warn!(stray = %name, "swept a stray registry temp"),
                Err(e) => tracing::warn!(stray = %name, error = %e, "could not sweep stray"),
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  The lock
// ──────────────────────────────────────────────────────────────────────────

/// An exclusive advisory lock on `~/.cauldron/projects.lock`, held for the
/// duration of one read-modify-write.
///
/// The lock file is NOT the registry. Locking the registry itself would mean
/// opening it for write to take the lock, which would truncate-or-create the
/// very file whose absence is the bootstrap's trigger.
struct Lock {
    inner: fd_lock::RwLock<fs::File>,
}

impl Lock {
    fn acquire(home: &Path) -> Result<Lock, io::Error> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path(home))?;
        Ok(Lock {
            inner: fd_lock::RwLock::new(file),
        })
    }

    /// Blocks until the lock is ours.
    fn write(&mut self) -> Result<fd_lock::RwLockWriteGuard<'_, fs::File>, io::Error> {
        self.inner.write()
    }
}
