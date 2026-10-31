//! **Where the soup face keeps what he types, per ENVIRONMENT.**
//!
//! An environment is a cauldron environment, resolved by the `env` crate —
//! the same resolution every `cauldron` verb uses (`--env`, `CAULDRON_ENV`,
//! `ENV`, else `prod`; the root is `CAULDRON_HOME`, else `~/.cauldron`). An
//! environment exists because its data root `data/<env>/` exists; this face
//! keeps its state inside that root, in the decided `data/<env>/<owner>/<kind>/`
//! shape, as owner `tui`, kind `soup`:
//!
//! ```text
//! <root>/data/<env>/tui/soup/bucket.json   the cauldron's ONE BUCKET being written (dirty state)
//! <root>/data/<env>/tui/soup/bucket/artifacts/<id>.txt   its paste chips' bodies (see `dump`)
//! <root>/data/<env>/tui/soup/dumps.jsonl   every dump sent, append-only: its notes, tags and chips
//! <root>/data/<env>/tui/soup/dumps.lock    held only for the length of one append
//!
//! (A bucket per cluster, `drafts/<cluster>.json` with its chips under
//! `drafts/<cluster>/artifacts/`, was design 0138's layout; one per
//! environment, `draft.json`, the layout before it; `sends.jsonl` the log's
//! old name. Their texts are adopted into the one bucket, in that order,
//! with their chips' bodies — and again whenever an older face still running
//! writes more into them (`adopted.json` records what was taken in; see
//! [`readopt`]). Nothing old is ever deleted, by adoption or by GC. `sends.jsonl` is renamed to `dumps.jsonl` the first time
//! a face may write.)
//! <root>/data/<env>/tui/soup/adopted.json     what the bucket took in from each old bucket file (see [`Adopted`])
//! <root>/data/<env>/tui/soup/clusters.json     the clusters made here (the `+` on Clusters, or beside one), seeded
//! <root>/data/<env>/tui/soup/ripples/           the ripples' runs recorded here (see `ripples`)
//! <root>/data/<env>/tui/soup/layout.json    the face's layout: the sidebar's share of the width, the bucket's height
//! <root>/data/<env>/tui/soup/settings.toml  the cauldron's settings (and accounts.json, flips.jsonl; see `settings`)
//! <root>/data/<env>/tui/soup/repos.json.bak  this cauldron's own repo registry from before msg 235, adopted into the global one
//! <root>/data/<env>/tui/soup/supervisions.jsonl   what was asked of the Mesh's services here (see `supervise`)
//! <root>/data/<env>/tui/soup/tickets.json   the human actions' steps done and fields submitted here (see `tickets`)
//! <root>/data/<env>/tui/soup/seed/clusters.json   a cauldron's OWN seed (see [`own_seed`]): wins over the repo's
//! <root>/data/<env>/tui/soup/seed/artifacts.json  its own artifacts seed, likewise
//! <root>/data/<env>/tui/soup/seed/ripples.json    its own ripple policy seed, likewise
//! <root>/data/shared/tui/repos.json             the repos registered (and seed repos unregistered) for EVERY cauldron (see `blueprint`)
//! <root>/data/shared/tui/dev_names.json           DEV ONLY: titles renamed on screen (see [`DevNames`])
//! ```
//!
//! **The bucket is shared live by every face open on one environment, and no
//! face's words are ever lost to another's (0220 E2).** It is written whole on
//! every edit — to a uniquely named temp file in the same directory, then
//! `rename`d over `bucket.json`, which is atomic on one filesystem, so a reader
//! sees the old bucket or the new one and never half of each. Each write
//! carries a fresh NONCE. A write is a read-MERGE-write under `bucket.lock`
//! (an `flock`, dropped by the OS if the face dies): when the file holds a
//! nonce this face did not write or read, another face wrote since, and the
//! text written is the THREE-WAY MERGE ([`merge3`]) of the text both started
//! from, this face's text and the other's — never this face's text over the
//! other's. A face polls the file and merges any foreign write into its own
//! text the same way. No write is ever torn and none ever replaces another.
//!
//! **Sends are appended under a lock** (`env::lock::with_lock`, the seam
//! table's own lock), one JSON line per send written with a single
//! `write_all`. A reader parses only up to the last newline, so a line still
//! being written is never read half-way; every face tails the file and shows a
//! new send in the logs of the clusters it tagged.
//!
//! **An environment reads only its OWN data (REL-3.S2, PRODUCT-SPEC §17).**
//! Its soup is what was made in it, plus the OWN seed it carries under
//! `seed/` in this directory, if any: `cauldron-build`'s is written by
//! `soup::demo::install` when it is created (the CLI's `env create` and this
//! face's `+` both), and no other environment carries one, so a fresh one is
//! empty. The repo ships NO seed: the fixtures the tests open
//! ([`seed_dir`], `tests/fixtures/seed/`) are reached only by a face opened
//! with `--seeds` or by a test, never by a face on a real environment
//! ([`no_seeds`]).

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use env::Environment;
use serde::{Deserialize, Serialize};

use crate::cauldron::model::Soup;
use crate::cauldron::tags;

/// The owner directory under the environment's data root.
pub const OWNER: &str = "tui";
/// The kind directory under the owner.
pub const KIND: &str = "soup";

/// `<data_root>/tui/soup`.
pub fn soup_dir(env: &Environment) -> Result<PathBuf> {
    Ok(env.service_data(OWNER)?.join(KIND))
}

/// `<root>/data/shared/tui`: what every cauldron of the root shares — the
/// GLOBAL settings (8e4fca10 msg 186: the token sources, set once for every
/// cauldron), the accounts added, the flip log, and the global token board.
pub fn global_dir(env: &Environment) -> PathBuf {
    env.shared_root().join(OWNER)
}

/// TEST FIXTURES ONLY: this crate's fixture seed (`tests/fixtures/seed/`,
/// synthetic, the shape the old repo `data/` had). A face on a real
/// environment never reads it (REL-3.S2): `tui --cauldron` opens with
/// [`no_seeds`], and only an explicit `--seeds <dir>` or a test opens a
/// fixture.
pub fn seed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/seed")
}

/// What a face on a REAL environment passes as its seeds (REL-3.S2): a
/// directory under the cauldron root that nothing ever creates, so every
/// fixture fallback (`<seeds>/clusters.json`, `<seeds>/global.json`,
/// `<seeds>/threads.json`, ...) finds nothing and the environment shows only
/// its own data and its own seed (`own_seed`).
pub fn no_seeds(env: &Environment) -> PathBuf {
    env.root.join("tui-no-seeds")
}

/// The seed file for environment `name`, if it has one.
pub fn seed_path(seeds: &Path, name: &str) -> Option<PathBuf> {
    let own = seeds.join("soup").join(format!("{name}.json"));
    if own.is_file() {
        return Some(own);
    }
    if name != "prod" {
        return None;
    }
    let prod = seeds.join("clusters.json");
    prod.is_file().then_some(prod)
}

/// The directory of a cauldron's OWN seed: `<soup dir>/seed`.
pub fn own_seed_dir(dir: &Path) -> PathBuf {
    dir.join("seed")
}

/// The name an own seed's clusters file had before msg 235: refused (see
/// [`load_soup`]).
pub const LEGACY_SEED: &str = "surfaces.json";

/// A file of the cauldron's OWN seed (`<soup dir>/seed/<file>`), if it has
/// one. It wins over the repo's seed for the same kind: the repo's seeds are
/// fixtures, a hand-curated cauldron's data is its own.
pub fn own_seed(dir: &Path, file: &str) -> Option<PathBuf> {
    let p = own_seed_dir(dir).join(file);
    p.is_file().then_some(p)
}

/// The soup for environment `name`: its own seed if it carries one, else the
/// repo's seed (or an empty one), plus the clusters created in it, which
/// `dir` holds, each under its parent.
///
/// An own seed written before msg 235 (`seed/surfaces.json`: slug ids,
/// serials per cluster) is REFUSED, loudly: its writer writes
/// `seed/clusters.json` now, and a stale one read in its place, or skipped
/// for the repo's seed, would show him a soup that is not his.
pub fn load_soup(seeds: &Path, name: &str, dir: &Path) -> Result<Soup> {
    load_soup_rooted(seeds, name, dir, false)
}

/// [`load_soup`], every cluster under the WHOLE CAULDRON when `whole` (the
/// sidebar's `contents below` on, b964d571 msg 23).
pub fn load_soup_rooted(seeds: &Path, name: &str, dir: &Path, whole: bool) -> Result<Soup> {
    if let (Some(p), None) = (own_seed(dir, LEGACY_SEED), own_seed(dir, "clusters.json")) {
        anyhow::bail!("{} is a seed from before msg 235 (slug ids, serials per cluster): this face reads `clusters.json` beside it, with minted ids and soup-wide serials; re-run its writer", p.display());
    }
    let created = CreatedClusters::new(dir).specs()?;
    let mut soup = match own_seed(dir, "clusters.json").or_else(|| seed_path(seeds, name)) {
        Some(p) => Soup::load_rooted(&p, created, whole.then_some(name))?,
        None => Soup::empty_rooted(name, created, whole)?,
    };
    // The token-source board a seed carries (`global.json`): the cauldron's
    // OWN seed's first, else a fixture's (REL-3.S2: never the repo's into
    // every environment).
    if let Some(board) = crate::cauldron::dash::GlobalBoard::load_for(seeds, dir)? {
        board.apply(&mut soup.dash);
    }
    // What was asked of the mesh's services from this face (msg 403):
    // replayed over the seed's readings, so the face shows what it did.
    let records = crate::cauldron::supervise::load(dir)?;
    crate::cauldron::supervise::apply(&mut soup.dash.mesh, &mut soup.dash.threads, &records);
    soup.dash.supervisions = records;
    Ok(soup)
}

/// Write `value` to `path` whole and atomically, as JSON.
fn write_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    write_text_atomic(path, &serde_json::to_string_pretty(value)?)
}

/// Write `text` to `path` whole and atomically: a uniquely named temp file
/// in the same directory, then `rename` over the real one.
pub fn write_text_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let tmp = dir.join(format!(".{}.{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("file"), uuid::Uuid::new_v4()));
    {
        let mut fh = std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        fh.write_all(text.as_bytes())?;
        fh.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(serde_json::from_str(&s).with_context(|| format!("parse {}", path.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

// ── what other faces wrote ──────────────────────────────────────────────────

/// A STAT signature over a set of files and directories: every file's path,
/// length and modification time, a directory walked whole. Every writer here
/// replaces a file by `rename`, so a changed file always shows a new
/// signature; a face compares the signature on each quiet tick and re-reads
/// exactly the part of its state another face changed, the way it polls the
/// draft. (Its own writes change the signature too; it re-reads those, which
/// is harmless, and keeps where he is either way.)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sig(Vec<(PathBuf, u64, Option<std::time::SystemTime>)>);

impl Sig {
    /// The signature of `paths` now. A path that does not exist contributes
    /// nothing, so a file appearing or vanishing is a change too.
    pub fn of(paths: &[PathBuf]) -> Sig {
        fn walk(p: &Path, out: &mut Vec<(PathBuf, u64, Option<std::time::SystemTime>)>) {
            let Ok(md) = std::fs::metadata(p) else { return };
            if md.is_dir() {
                let Ok(rd) = std::fs::read_dir(p) else { return };
                let mut kids: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
                kids.sort();
                for k in kids {
                    // A writer's temp file is not yet a change.
                    if k.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.') && n.ends_with(".tmp")) {
                        continue;
                    }
                    walk(&k, out);
                }
            } else {
                out.push((p.to_path_buf(), md.len(), md.modified().ok()));
            }
        }
        let mut out = Vec::new();
        for p in paths {
            walk(p, &mut out);
        }
        Sig(out)
    }
}

/// The parts of a face's state other faces on the environment can change,
/// each with the signature it was last read at.
#[derive(Debug, Clone, Default)]
pub struct Watch {
    /// The clusters made here and the repos registered here: the soup.
    pub soup: Sig,
    /// The settings files and the accounts and pools added here.
    pub settings: Sig,
    /// The ripple policy, its working variant and its runs.
    pub ripples: Sig,
    /// The human actions' steps done and fields submitted.
    pub tickets: Sig,
    /// The old buckets an older face may still be writing (0217 B1).
    pub legacy: Sig,
}

/// What each part of [`Watch`] covers in the soup directory `dir` — and, for
/// the repo registry, which every cauldron shares, the global directory.
pub fn soup_paths(dir: &Path, global: &Path) -> Vec<PathBuf> {
    vec![dir.join("clusters.json"), global.join(crate::cauldron::blueprint::REGISTRY_FILE), dir.join(crate::cauldron::blueprint::REGISTRY_FILE), own_seed_dir(dir), crate::cauldron::supervise::path(dir)]
}
/// The settings files a face watches: this cauldron's (`dir`) and the
/// global ones (`global`), which another cauldron's face may write.
pub fn settings_paths(dir: &Path, global: &Path) -> Vec<PathBuf> {
    vec![dir.join("settings.toml"), global.join("settings.toml"), global.join("accounts.json"), global.join("flips.jsonl")]
}
pub fn ripples_paths(dir: &Path) -> Vec<PathBuf> {
    vec![dir.join("ripples")]
}
pub fn tickets_paths(dir: &Path) -> Vec<PathBuf> {
    vec![dir.join("tickets.json")]
}

impl Watch {
    pub fn now(dir: &Path, global: &Path) -> Watch {
        Watch {
            soup: Sig::of(&soup_paths(dir, global)),
            settings: Sig::of(&settings_paths(dir, global)),
            ripples: Sig::of(&ripples_paths(dir)),
            tickets: Sig::of(&tickets_paths(dir)),
            legacy: Sig::of(&legacy_paths(dir)),
        }
    }
}

// ── clusters made here ─────────────────────────────────────────────────────────

/// One cluster made here: with the `+` on Clusters (a ROOT, after the
/// seed's roots) or as a CHILD of a cluster, with the `+` beside it (msg 112). It carries
/// his BOUNDARIES — what belongs in it, in his words, which is its
/// description — and what the seeding ripple claimed and linked for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreatedCluster {
    pub id: String,
    pub name: String,
    pub created_at: String,
    /// The cluster it was made under; none is a root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub boundaries: String,
    /// Regions the seeding claimed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<String>,
    /// Repos the seeding claimed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    /// Questions and tasks of other clusters the seeding linked, by soup id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    /// The seeding run: its id, line and when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeded_by: Option<SeededBy>,
    /// The UNOWNED cluster (msg 235: "if we delete a cluster and now there's
    /// unowned regions, we can create an unowned cluster"): made only when a
    /// deletion leaves regions no cluster claims, and given each of them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unowned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SeededBy {
    pub run: String,
    pub line: String,
    pub at: String,
    /// The seeding is still running (its call has not answered yet): the
    /// cluster is open and its claims land when it does.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

/// What a seeding claimed and linked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Claims {
    pub regions: Vec<String>,
    pub repos: Vec<String>,
    pub links: Vec<String>,
}

impl CreatedCluster {
    /// The cluster as the soup loads it from soup dir `dir`, and the
    /// cluster it goes under. A pending seeding whose face is gone loads as
    /// `interrupted` ([`CreatedCluster::seed_state`]), never `running`.
    pub fn spec(&self, dir: &Path) -> (Option<String>, crate::cauldron::model::ClusterSpec) {
        use crate::cauldron::model::{Background, RepoClaim, ClusterSpec};
        let mut g = ClusterSpec::bare(&self.id, &self.name);
        g.made_here = true;
        g.description = self.boundaries.clone();
        g.regions = self.regions.clone();
        g.repos = self.repos.iter().map(|r| RepoClaim { name: r.clone(), focus: Vec::new() }).collect();
        g.links = self.links.clone();
        if let Some(s) = &self.seeded_by {
            let (state, line) = match self.seed_state(dir, chrono::Utc::now()) {
                SeedState::Interrupted(why) => ("interrupted", format!("interrupted · {why} · reseed or clear it below")),
                SeedState::Running => ("running", s.line.clone()),
                _ => ("done", s.line.clone()),
            };
            g.ripples.push(Background { kind: "seed".into(), line, when: s.at.clone(), state: state.into(), cite: None, run: Some(s.run.clone()) });
        }
        (self.parent.clone(), g)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatedFile {
    clusters: Vec<CreatedCluster>,
}

/// The clusters made in one environment, in `clusters.json`.
pub struct CreatedClusters {
    pub path: PathBuf,
}

impl CreatedClusters {
    pub fn new(dir: &Path) -> Self {
        CreatedClusters { path: dir.join("clusters.json") }
    }

    /// The soup dir the file is in.
    pub fn dir(&self) -> PathBuf {
        self.path.parent().map(Path::to_path_buf).unwrap_or_default()
    }

    pub fn list(&self) -> Result<Vec<CreatedCluster>> {
        Ok(read_json::<CreatedFile>(&self.path)?.unwrap_or_default().clusters)
    }

    /// Every cluster made here, with the cluster each goes under, in the
    /// order they were made (a move can put a child before its parent; the
    /// soup attaches them in whatever order resolves).
    pub fn specs(&self) -> Result<Vec<(Option<String>, crate::cauldron::model::ClusterSpec)>> {
        let dir = self.dir();
        Ok(self.list()?.iter().map(|c| c.spec(&dir)).collect())
    }

    /// Record a new cluster. `taken` says whether an id is already a cluster of the
    /// soup; it is asked again INSIDE the lock, against the file as it is
    /// then, so two faces cannot both create one id.
    pub fn add(&self, cluster: CreatedCluster, taken: impl Fn(&str) -> bool) -> Result<()> {
        env::lock::with_lock(&self.path, || {
            let mut file = read_json::<CreatedFile>(&self.path)?.unwrap_or_default();
            if taken(&cluster.id) || file.clusters.iter().any(|g| g.id == cluster.id) {
                anyhow::bail!("a cluster `{}` already exists", cluster.id);
            }
            file.clusters.push(cluster);
            write_atomic(&self.path, &file)
        })
    }
}

impl CreatedClusters {
    /// Change the file under its lock, from the file as it is inside the lock.
    fn update(&self, f: impl FnOnce(&mut Vec<CreatedCluster>) -> Result<()>) -> Result<()> {
        env::lock::with_lock(&self.path, || {
            let mut file = read_json::<CreatedFile>(&self.path)?.unwrap_or_default();
            f(&mut file.clusters)?;
            write_atomic(&self.path, &file)
        })
    }

    /// A seeding of cluster `id` answered: its claims and its run (none: it
    /// did not run, and the "seeding" mark goes).
    pub fn seeded(&self, id: &str, claims: Claims, by: Option<SeededBy>) -> Result<()> {
        self.update(|list| {
            let s = list.iter_mut().find(|s| s.id == id).with_context(|| format!("`{id}` is gone (deleted while it was seeding)"))?;
            s.regions = claims.regions;
            s.repos = claims.repos;
            s.links = claims.links;
            s.seeded_by = by;
            Ok(())
        })
    }

    /// Rename cluster `id`. Its id stays: it is minted, never read off its
    /// name.
    pub fn rename(&self, id: &str, name: &str) -> Result<()> {
        self.update(|list| {
            let s = list.iter_mut().find(|s| s.id == id).with_context(|| format!("`{id}` was not made here"))?;
            s.name = name.to_string();
            Ok(())
        })
    }

    /// Put cluster `id` under `parent` (none: at the root), after the
    /// parent's other children. Refused for a cluster not made here, a
    /// parent that is not in the soup (`exists` says), and a parent that is
    /// the cluster itself or below it.
    pub fn set_parent(&self, id: &str, parent: Option<String>, exists: impl Fn(&str) -> bool) -> Result<()> {
        self.update(|list| {
            let i = list.iter().position(|s| s.id == id).with_context(|| format!("`{id}` was not made here"))?;
            if let Some(p) = &parent {
                if !exists(p) {
                    anyhow::bail!("there is no cluster `{p}`");
                }
                // Walk up from the new parent: meeting `id` means a cycle.
                let mut cur = Some(p.clone());
                while let Some(c) = cur {
                    if c == id {
                        anyhow::bail!("a cluster cannot go under itself or a cluster below it");
                    }
                    cur = list.iter().find(|s| s.id == c).and_then(|s| s.parent.clone());
                }
            }
            let mut s = list.remove(i);
            s.parent = parent;
            // Last among its new siblings.
            list.push(s);
            Ok(())
        })
    }

    /// Delete cluster `id` and every cluster made under it. Returns the ids
    /// that went, `id` first.
    pub fn remove(&self, id: &str) -> Result<Vec<String>> {
        let mut gone = Vec::new();
        self.update(|list| {
            if !list.iter().any(|s| s.id == id) {
                anyhow::bail!("`{id}` was not made here");
            }
            gone.push(id.to_string());
            let mut i = 0;
            while i < gone.len() {
                let p = gone[i].clone();
                gone.extend(list.iter().filter(|s| s.parent.as_deref() == Some(p.as_str())).map(|s| s.id.clone()));
                i += 1;
            }
            list.retain(|s| !gone.contains(&s.id));
            Ok(())
        })?;
        Ok(gone)
    }

    /// Give the UNOWNED cluster `regions` (msg 235): it is made at the root
    /// the first time a deletion leaves a region no cluster claims, and each
    /// later one adds to it. Returns its id.
    pub fn unowned(&self, regions: &[String], mint: impl Fn(&[CreatedCluster]) -> String) -> Result<String> {
        let mut id = String::new();
        self.update(|list| {
            match list.iter_mut().find(|s| s.unowned) {
                Some(s) => {
                    for r in regions {
                        if !s.regions.contains(r) {
                            s.regions.push(r.clone());
                        }
                    }
                    id = s.id.clone();
                }
                None => {
                    id = mint(list);
                    list.push(CreatedCluster {
                        id: id.clone(),
                        name: "Unowned".into(),
                        created_at: chrono::Utc::now().to_rfc3339(),
                        parent: None,
                        boundaries: "The regions no cluster claims since the cluster that claimed them was deleted.".into(),
                        regions: regions.to_vec(),
                        repos: Vec::new(),
                        links: Vec::new(),
                        seeded_by: None,
                        unowned: true,
                    });
                }
            }
            Ok(())
        })?;
        Ok(id)
    }
}

// ── who is seeding (IS-32) ──────────────────────────────────────────────────
//
// `SeededBy.pending` is cleared only by the face that started the seeding, so
// a face that dies mid-seed used to leave its cluster `seeding` forever. WHO is
// seeding is recorded OUTSIDE `clusters.json`, because `SeededBy` is
// `deny_unknown_fields` and an older face still running would refuse a file
// carrying a new field (the IS-19/IS-28 class): one lock file per cluster,
// `<soup dir>/seeding/<cluster id>.lock`, which an older face never reads.
//
// **Liveness is the KERNEL's answer, never a pid** (the reasoning is
// `env::hold`'s: a pid is a recycled integer, and a pidfile outlives a
// SIGKILL). The seeding face holds an EXCLUSIVE `flock` on the file for the
// whole of the seeding, taken BEFORE `pending` is written and dropped only
// AFTER its answer is written; the OS drops it when the face dies by any
// means. Any face asks with a SHARED try-lock: refused means the owner is
// alive; granted means nobody holds it, so the owner is gone. The pid, start
// time and run written into the file AFTER the lock is taken are a NOTE for
// the screen, read only once the kernel has decided — no decision reads them,
// so a reused pid cannot keep an orphan looking alive.
//
// A pending seed with NO lock file was started by an older face (one
// built before this lock existed): it is taken as interrupted once it is older
// than [`ORPHAN_AFTER`], ten minutes — well past the type-one call's own
// timeout (90 s by default, `CAULDRON_TYPE_ONE_TIMEOUT_S`), after which that
// older face would have written its answer had it lived.
//
// The lock files are never deleted: unlinking a lock file another face may
// have just opened lets two faces each hold a lock on a different inode.

/// The directory of the seeding lock files, under the soup dir.
pub const SEED_LOCK_DIR: &str = "seeding";

/// A pending seed no face has claimed for this long is interrupted.
pub const ORPHAN_AFTER: chrono::Duration = chrono::Duration::minutes(10);

/// `<soup dir>/seeding/<cluster id>.lock`.
pub fn seed_lock_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(SEED_LOCK_DIR).join(format!("{id}.lock"))
}

/// What the seeding face writes into its lock file, for the screen only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SeedNote {
    pub run: String,
    pub pid: u32,
    pub started: String,
}

/// The seeding of one cluster, held by this face: the kernel lock on its lock
/// file. Dropping it (or the face dying) lets any face see the seeding as
/// interrupted unless its answer was written.
#[derive(Debug)]
pub struct SeedLock {
    _file: std::fs::File,
    pub note: SeedNote,
}

impl SeedLock {
    /// Take the seeding of cluster `id` for run `run`. `Ok(None)` when
    /// another face holds it (a live seeding, or another face reseeding at
    /// this moment). A shared probe holds the lock for one syscall, so a
    /// refusal is tried again for a moment before it is believed.
    pub fn take(dir: &Path, id: &str, run: &str) -> Result<Option<SeedLock>> {
        let path = seed_lock_path(dir, id);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).with_context(|| format!("creating {}", p.display()))?;
        }
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).with_context(|| format!("opening {}", path.display()))?;
        let t0 = std::time::Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if t0.elapsed() < std::time::Duration::from_millis(200) => std::thread::sleep(std::time::Duration::from_millis(5)),
                Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                Err(std::fs::TryLockError::Error(e)) => return Err(e).with_context(|| format!("locking {}", path.display())),
            }
        }
        let note = SeedNote { run: run.to_string(), pid: std::process::id(), started: chrono::Utc::now().to_rfc3339() };
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(serde_json::to_string(&note)?.as_bytes())?;
        file.sync_data().ok();
        Ok(Some(SeedLock { _file: file, note }))
    }
}

/// Who holds the seeding of a cluster, as the kernel says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedOwner {
    /// A face holds it: the seeding is live (the note, if readable).
    Live(Option<SeedNote>),
    /// Nobody holds it: whoever wrote the note is gone.
    Gone(Option<SeedNote>),
    /// There is no lock file: no face built with this lock ever seeded it.
    Never,
}

/// Ask the kernel who holds the seeding of cluster `id`. Never creates the
/// file and never writes it.
pub fn seed_owner(dir: &Path, id: &str) -> SeedOwner {
    let path = seed_lock_path(dir, id);
    let Ok(file) = OpenOptions::new().read(true).open(&path) else { return SeedOwner::Never };
    let held = matches!(file.try_lock_shared(), Err(std::fs::TryLockError::WouldBlock));
    drop(file);
    // The note is read only now, after the kernel has decided.
    let note = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<SeedNote>(&t).ok());
    if held { SeedOwner::Live(note) } else { SeedOwner::Gone(note) }
}

/// Where a cluster's seeding stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedState {
    /// It was never seeded, or the mark was cleared.
    None,
    /// Its answer was written.
    Done,
    /// A live face is seeding it.
    Running,
    /// Pending, and the face seeding it is gone: why, in words.
    Interrupted(String),
}

impl CreatedCluster {
    /// Where this cluster's seeding stands, in soup dir `dir`, at `now`.
    pub fn seed_state(&self, dir: &Path, now: chrono::DateTime<chrono::Utc>) -> SeedState {
        let Some(s) = &self.seeded_by else { return SeedState::None };
        if !s.pending {
            return SeedState::Done;
        }
        let gone = |n: &SeedNote| format!("the face seeding it (pid {}, since {}) is gone", n.pid, local_hm(&n.started));
        match seed_owner(dir, &self.id) {
            // Whoever holds it — this run's face, or a face reseeding it this
            // moment — is alive.
            SeedOwner::Live(_) => SeedState::Running,
            // Nobody holds it. A face writes its note before it writes
            // `pending` (on create and on reseed), and an older face only
            // seeds at creation, before any face could have taken the lock:
            // so whoever wrote this file is the last face seeding it, and it
            // is gone.
            SeedOwner::Gone(Some(n)) => SeedState::Interrupted(gone(&n)),
            SeedOwner::Gone(None) => SeedState::Interrupted("the face seeding it is gone".into()),
            // No lock file: an older face started it.
            SeedOwner::Never => {
                let made = chrono::DateTime::parse_from_rfc3339(&self.created_at).map(|t| t.with_timezone(&chrono::Utc)).ok();
                match made {
                    Some(t) if now - t < ORPHAN_AFTER => SeedState::Running,
                    _ => SeedState::Interrupted(format!("no face has claimed it for over {} min", ORPHAN_AFTER.num_minutes())),
                }
            }
        }
    }
}

fn local_hm(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339).map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_else(|_| rfc3339.to_string())
}

impl CreatedClusters {
    /// Take over cluster `id`'s INTERRUPTED seeding, whose pending run is
    /// `orphan`, as run `by` (pending). The caller holds `by`'s [`SeedLock`],
    /// so no live face is seeding it; inside the file's lock the cluster
    /// must still be pending as `orphan`, so of two faces reseeding at once
    /// exactly one wins and the other is told.
    pub fn reseed(&self, id: &str, orphan: &str, by: SeededBy, held: &SeedLock) -> Result<()> {
        anyhow::ensure!(held.note.run == by.run, "the seeding lock held is for run {}, not {}", held.note.run, by.run);
        self.update(|list| {
            let s = list.iter_mut().find(|s| s.id == id).with_context(|| format!("`{id}` was not made here"))?;
            match &s.seeded_by {
                Some(b) if b.pending && b.run == orphan => {}
                _ => anyhow::bail!("its seeding is no longer the interrupted one (another face reseeded or cleared it)"),
            }
            s.seeded_by = Some(by);
            Ok(())
        })
    }

    /// Clear cluster `id`'s INTERRUPTED seeding mark (pending run `orphan`):
    /// it stays with no claims and no seeding. The caller holds the seeding
    /// lock, so no live face is seeding it.
    pub fn clear_seed(&self, id: &str, orphan: &str, _held: &SeedLock) -> Result<()> {
        self.update(|list| {
            let s = list.iter_mut().find(|s| s.id == id).with_context(|| format!("`{id}` was not made here"))?;
            match &s.seeded_by {
                Some(b) if b.pending && b.run == orphan => {}
                _ => anyhow::bail!("its seeding is no longer the interrupted one (another face reseeded or cleared it)"),
            }
            s.seeded_by = None;
            Ok(())
        })
    }
}

/// The next MINTED cluster id (msg 235: "use a minted ID, like cluster-2,
/// like C-2 ... Use an X"): `X-<n>`, one past the highest `X-` number among
/// `keys`, which are every cluster's — the seed's and those made here.
pub fn mint_id<'a>(keys: impl IntoIterator<Item = &'a str>) -> String {
    let n = keys.into_iter().filter_map(|k| k.strip_prefix("X-")?.parse::<u32>().ok()).max().unwrap_or(0);
    format!("X-{}", n + 1)
}

/// The next minted cluster id for `forest` (and what is made here).
pub fn mint_cluster_id(forest: &crate::cauldron::model::Forest) -> String {
    // REL-3.S2d: a cluster made here and skipped (its parent is gone) is
    // still on disk, so its id is still taken.
    mint_id(forest.clusters.iter().map(|g| g.key.as_str()).chain(forest.skipped.iter().map(String::as_str)))
}

/// A name as a SLUG: lower case, every run of anything but a letter or a
/// digit one `-`, trimmed of `-`. Empty when the name has no letter or
/// digit at all. An account's id, a pool's and a repo's are slugs of their
/// names; a cluster's never is — it is minted ([`mint_id`]).
pub fn slug(name: &str) -> String {
    let mut id = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            id.push(c);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    id.trim_end_matches('-').to_string()
}

// ── the layout ──────────────────────────────────────────────────────────────

/// The face's layout, kept per environment beside the draft.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    /// The sidebar's share of the cluster page's width, in thousandths. `None`
    /// until the divider is first dragged: the default layout.
    #[serde(default)]
    pub sidebar_permille: Option<u16>,
    /// The bucket's text rows, once its divider was dragged (msg 235). `None`:
    /// as many as it holds, three to six.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket_rows: Option<u16>,
}

pub struct LayoutStore {
    pub path: PathBuf,
    writable: bool,
}

impl LayoutStore {
    pub fn new(dir: &Path, writable: bool) -> Self {
        LayoutStore { path: dir.join("layout.json"), writable }
    }

    pub fn load(&self) -> Result<Layout> {
        Ok(read_json(&self.path)?.unwrap_or_default())
    }

    pub fn save(&self, layout: &Layout) -> Result<()> {
        if !self.writable {
            return Ok(());
        }
        write_atomic(&self.path, layout)
    }
}

/// Every environment that exists in the root, plus `current` (which may not
/// have been written to yet). An environment exists only once it has been
/// created — by an explicit "new", or by `cauldron` itself — never by being
/// listed here or switched to.
pub fn list_envs(current: &Environment) -> Result<Vec<String>> {
    current.environments_including_self()
}

/// The cauldrons of `current`'s root whose clusters claim repo `repo` —
/// `current`'s own read from `here`, every other's from its soup. That is
/// every cauldron that exists, AND every one the repo seeds (`seeds`) — prod,
/// and each `<seeds>/soup/<env>.json` — since a seeded cauldron's clusters claim
/// their repos the moment it is first opened. A cauldron whose soup cannot be
/// read is counted as claiming it: the registry is global, and taking a repo
/// away from a cauldron that cannot be checked could leave it unable to load.
pub fn repo_claimants(current: &Environment, seeds: &Path, repo: &str, here: &crate::cauldron::model::Forest) -> Result<Vec<String>> {
    let claims = |f: &crate::cauldron::model::Forest| f.clusters.iter().any(|g| g.repos.iter().any(|c| c.name == repo));
    let mut names = list_envs(current)?;
    let seeded = std::fs::read_dir(seeds.join("soup")).into_iter().flatten().flatten().filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").filter(|n| !n.contains('.')).map(str::to_string));
    names.extend(seeded.chain(seed_path(seeds, "prod").map(|_| "prod".to_string())));
    names.sort();
    names.dedup();
    let mut out = Vec::new();
    for name in names {
        let yes = if name == current.name {
            claims(here)
        } else {
            current.for_name(&name).and_then(|env| soup_dir(&env)).and_then(|dir| load_soup(seeds, &name, &dir)).map_or(true, |soup| claims(&soup.forest))
        };
        if yes {
            out.push(name);
        }
    }
    Ok(out)
}

/// The repos every cauldron sees (the global registry), with `forest`'s
/// claims resolved — adopting this cauldron's own registry from before msg
/// 235 first, once ([`crate::cauldron::blueprint::adopt_legacy`]).
pub fn load_repos(env: &Environment, seeds: &Path, forest: &crate::cauldron::model::Forest, persist: bool) -> Result<(crate::cauldron::blueprint::Repos, crate::cauldron::blueprint::Adopted)> {
    use crate::cauldron::blueprint::{adopt_legacy, read_seed, Repos};
    let dir = soup_dir(env)?;
    let seed = read_seed(seeds)?;
    let claimed = |name: &str| repo_claimants(env, seeds, name, forest).map_or(true, |v| !v.is_empty());
    let (reg, adopted) = adopt_legacy(&dir, &global_dir(env), &seed, persist, &claimed)?;
    Ok((Repos::build_with(seed, &reg, forest)?, adopted))
}

/// Create environment `name` in `current`'s root: its data root and the
/// directories every environment has. Refused when it already exists.
pub fn create_env(current: &Environment, name: &str) -> Result<Environment> {
    let env = current.for_name(name)?;
    if current.environments()?.iter().any(|n| n == name) {
        anyhow::bail!("`{name}` already exists");
    }
    env.ensure_dirs()?;
    // REL-3.S2: the curated environment is seeded here exactly as the CLI's
    // `env create` seeds it; every other name starts empty
    // (`soup::demo::seed_env` refuses any other).
    if name == ::soup::demo::ENV {
        ::soup::demo::seed_env(&env).context("seeding the curated environment")?;
    }
    Ok(env)
}

// ── the draft ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftFile {
    /// Fresh per write. A face adopts any draft whose nonce it did not write
    /// or already read.
    pub nonce: String,
    /// `pid@host` style label of the face that wrote it, for diagnosis only.
    pub writer: String,
    pub updated_at: String,
    pub text: String,
    /// Written by a face of this build or later. A write without it is an
    /// older face's, whose base has to be found ([`merge3_unmarked`]).
    #[serde(default)]
    pub merges: bool,
}

pub struct DraftStore {
    pub path: PathBuf,
    /// The nonce of the bucket this face last wrote or adopted.
    last: Option<String>,
    /// That bucket's text: what this face's text and another face's write
    /// both started from, the base of every [`merge3`].
    base: String,
    /// The buckets this face has written or adopted lately, newest last: an
    /// older face's stale write is some one of them plus what it typed, and
    /// is merged from that one ([`merge3_unmarked`]).
    seen: Vec<String>,
    /// The text of the last unmarked write this face merged: an older face
    /// typing on before its tick types on from there.
    prev_unmarked: Option<String>,
    /// Per character of `base`, whether THIS face typed it: a write without
    /// the `merges` mark that takes such text out may be stale
    /// ([`merge3_unmarked`]).
    own: Vec<bool>,
    writable: bool,
}

/// How many recent buckets a face keeps to place an older face's write.
const SEEN_KEPT: usize = 64;

fn writer_label() -> String {
    format!("tui --cauldron pid {}", std::process::id())
}

/// The repo seed's old cluster slugs and their minted ids (8e4fca10 msg
/// 235), in the old seed's pre-order (c62b7eb5 `data/surfaces.json` →
/// `data/clusters.json`).
const LEGACY_CLUSTERS: &[(&str, &str)] = &[
    ("cauldron", "X-1"),
    ("cauldron-interface", "X-2"),
    ("cauldron-ripples", "X-3"),
    ("cauldron-ripples-dream", "X-4"),
    ("cauldron-curation", "X-5"),
    ("broomstick", "X-6"),
    ("broomstick-landing", "X-7"),
    ("broomstick-platform", "X-8"),
    ("harness", "X-9"),
    ("harness-tui", "X-10"),
    ("harness-workspaces", "X-11"),
];

/// Every item of the old seed, `<slug>/<id>`, and its id in the one soup:
/// matched by content against `<seeds>/clusters.json` (the same matching that
/// re-keyed tickets.json in task 0203; the two lowest scores, Q-15 and Q-21,
/// checked by hand: the text differs only by the surface → cluster rename).
const LEGACY_ITEMS: &[(&str, &str)] = &[
    ("cauldron/Q-1", "Q-1"),
    ("cauldron/Q-2", "Q-2"),
    ("cauldron/Q-3", "Q-3"),
    ("cauldron/T-1", "T-1"),
    ("cauldron/T-2", "T-2"),
    ("cauldron/T-3", "T-3"),
    ("cauldron/T-4", "T-4"),
    ("cauldron/T-5", "T-5"),
    ("cauldron/T-6", "T-6"),
    ("cauldron/T-8", "T-8"),
    ("cauldron/HA-1", "HA-1"),
    ("cauldron/HA-2", "HA-2"),
    ("cauldron/M-1", "M-1"),
    ("cauldron/M-2", "M-2"),
    ("cauldron/M-3", "M-3"),
    ("cauldron/M-4", "M-4"),
    ("cauldron/M-5", "M-5"),
    ("cauldron/OBJ-1", "OBJ-1"),
    ("cauldron/OBJ-2", "OBJ-2"),
    ("cauldron/OBJ-3", "OBJ-3"),
    ("cauldron-interface/Q-1", "Q-11"),
    ("cauldron-interface/Q-2", "Q-12"),
    ("cauldron-interface/T-1", "T-9"),
    ("cauldron-interface/T-2", "T-10"),
    ("cauldron-interface/T-3", "T-11"),
    ("cauldron-interface/T-4", "T-12"),
    ("cauldron-interface/M-1", "M-6"),
    ("cauldron-interface/M-2", "M-7"),
    ("cauldron-interface/OBJ-1", "OBJ-4"),
    ("cauldron-ripples/Q-1", "Q-15"),
    ("cauldron-ripples/T-1", "T-13"),
    ("cauldron-ripples/T-2", "T-14"),
    ("cauldron-ripples/HA-1", "HA-4"),
    ("cauldron-ripples/M-1", "M-8"),
    ("cauldron-ripples/OBJ-1", "OBJ-5"),
    ("cauldron-ripples-dream/Q-1", "Q-16"),
    ("cauldron-ripples-dream/T-1", "T-15"),
    ("cauldron-ripples-dream/M-1", "M-9"),
    ("cauldron-ripples-dream/OBJ-1", "OBJ-6"),
    ("cauldron-curation/Q-1", "Q-17"),
    ("cauldron-curation/T-1", "T-16"),
    ("cauldron-curation/M-1", "M-10"),
    ("cauldron-curation/OBJ-1", "OBJ-7"),
    ("broomstick/Q-7", "Q-7"),
    ("broomstick/T-1", "T-17"),
    ("broomstick/T-2", "T-18"),
    ("broomstick/M-1", "M-11"),
    ("broomstick/M-2", "M-12"),
    ("broomstick/OBJ-1", "OBJ-8"),
    ("broomstick-landing/Q-1", "Q-18"),
    ("broomstick-landing/T-1", "T-19"),
    ("broomstick-landing/T-2", "T-20"),
    ("broomstick-landing/M-1", "M-13"),
    ("broomstick-landing/OBJ-1", "OBJ-9"),
    ("broomstick-platform/Q-1", "Q-20"),
    ("broomstick-platform/T-1", "T-21"),
    ("broomstick-platform/T-2", "T-22"),
    ("broomstick-platform/HA-1", "HA-5"),
    ("broomstick-platform/HA-2", "HA-6"),
    ("broomstick-platform/HA-3", "HA-3"),
    ("broomstick-platform/M-1", "M-14"),
    ("broomstick-platform/OBJ-1", "OBJ-10"),
    ("harness/T-1", "T-23"),
    ("harness/T-2", "T-24"),
    ("harness/M-1", "M-15"),
    ("harness/OBJ-1", "OBJ-11"),
    ("harness-tui/T-1", "T-25"),
    ("harness-tui/T-2", "T-26"),
    ("harness-tui/T-3", "T-27"),
    ("harness-tui/M-1", "M-16"),
    ("harness-tui/M-2", "M-17"),
    ("harness-tui/M-3", "M-18"),
    ("harness-tui/OBJ-1", "OBJ-12"),
    ("harness-workspaces/Q-4", "Q-21"),
    ("harness-workspaces/T-1", "T-28"),
    ("harness-workspaces/HA-1", "HA-7"),
    ("harness-workspaces/M-1", "M-19"),
    ("harness-workspaces/OBJ-1", "OBJ-13"),
    ("cauldron/Q-4", "Q-4"),
    ("cauldron/Q-5", "Q-5"),
    ("cauldron/Q-9", "Q-9"),
    ("cauldron/Q-8", "Q-8"),
    ("cauldron/Q-10", "Q-10"),
    ("cauldron-interface/Q-9", "Q-13"),
    ("cauldron-interface/Q-10", "Q-14"),
    ("broomstick-landing/Q-4", "Q-19"),
];

/// The DESIGN cauldron's old surface slugs (its seed before msg 235,
/// `make_seed.py`'s variable names) and their minted ids, in its pre-order
/// (0217 B3: D2's rewrite reached prod only). No slug here is one of
/// [`LEGACY_CLUSTERS`], so one table serves both cauldrons.
const LEGACY_DESIGN_CLUSTERS: &[(&str, &str)] = &[
    ("system", "X-2"),
    ("bucket", "X-3"),
    ("voice", "X-4"),
    ("attention", "X-5"),
    ("units", "X-6"),
    ("config", "X-7"),
    ("screen", "X-8"),
];

/// The design seed's old items whose soup id is KNOWN: `system` is the first
/// cluster under the root, and the root holds no question, so its serials
/// were kept ("a cluster earlier in the tree keeps it", make_seed.py). The
/// other surfaces' serials moved, by an order nothing recorded, so theirs
/// are left as written rather than guessed.
const LEGACY_DESIGN_ITEMS: &[(&str, &str)] = &[
    ("system/Q-1", "Q-1"),
    ("system/Q-2", "Q-2"),
    ("system/Q-3", "Q-3"),
    ("system/Q-4", "Q-4"),
    ("system/Q-5", "Q-5"),
];

/// The DEV cauldron's old cluster slugs (its seed was `scratch` until
/// c64c1a35, then `sandbox`, then minted ids, 9e3257d2) and their ids, and
/// its items: matched by content (Q-1 asks about the nightly refresh, Q-2
/// whether a dev ripple may write). 0220 E7: an older face wrote
/// `[scratch/Q-1]` into the dev bucket after the one bucket existed.
const LEGACY_DEV_CLUSTERS: &[(&str, &str)] = &[("scratch", "X-1"), ("scratch-ripples", "X-2"), ("sandbox", "X-1"), ("sandbox-ripples", "X-2")];
const LEGACY_DEV_ITEMS: &[(&str, &str)] = &[("scratch/Q-1", "Q-1"), ("scratch-ripples/Q-1", "Q-2"), ("sandbox/Q-1", "Q-1"), ("sandbox-ripples/Q-1", "Q-2")];

/// A setting reference as the one cauldron names it: the old per-surface
/// `[setting:<slug>/surface.<key>]` is the cauldron's `<key>` now (a surface's
/// settings became its cauldron's, msg 235), and a RENAMED key is its new
/// name (`pools.claude_code.new_accounts_enabled` →
/// `….accounts_enabled`). `None` when nothing changes.
fn rewrite_setting_ref(key: &str) -> Option<String> {
    let bare = match key.split_once('/') {
        Some((slug, rest)) if LEGACY_DESIGN_CLUSTERS.iter().chain(LEGACY_CLUSTERS).chain(LEGACY_DEV_CLUSTERS).any(|(o, _)| *o == slug) => rest.strip_prefix("surface.").unwrap_or(rest),
        Some(_) => return None,
        None => key,
    };
    let new = crate::cauldron::settings::renamed(bare).unwrap_or_else(|| bare.to_string());
    (new != key).then_some(new)
}

/// `text` with every old reference rewritten to the one soup's (task 0209
/// D2, 0217 B3 for the design cauldron): `[cauldron-ripples/Q-1.opt2]` →
/// `[Q-15.opt2]`, `[system/Q-1]` → `[Q-1]`, `[surface:cauldron]` →
/// `[X-1]`, `[setting:system/surface.roles]` → `[setting:roles]`. One the
/// tables do not know is left as it was: never guessed, because a bare
/// serial would name a different item.
pub fn rewrite_legacy_refs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        let Some(close) = tail.find(']') else {
            out.push_str(tail);
            return out;
        };
        let inner = &tail[1..close];
        let new = if let Some(slug) = inner.strip_prefix("surface:") {
            LEGACY_CLUSTERS.iter().chain(LEGACY_DESIGN_CLUSTERS).chain(LEGACY_DEV_CLUSTERS).find(|(o, _)| *o == slug).map(|(_, n)| crate::cauldron::tags::Tag::thing("cluster", n).render())
        } else if let Some(key) = inner.strip_prefix("setting:") {
            rewrite_setting_ref(key).map(|k| format!("[setting:{k}]"))
        } else {
            inner.split_once('/').and_then(|(slug, item)| {
                let (id, sub) = item.split_once('.').map_or((item, None), |(i, s)| (i, Some(s)));
                let key = format!("{slug}/{id}");
                let (_, n) = LEGACY_ITEMS.iter().chain(LEGACY_DESIGN_ITEMS).chain(LEGACY_DEV_ITEMS).find(|(o, _)| *o == key)?;
                Some(match sub {
                    Some(s) => format!("[{n}.{s}]"),
                    None => format!("[{n}]"),
                })
            })
        };
        match new {
            Some(n) => out.push_str(&n),
            None => out.push_str(&tail[..=close]),
        }
        rest = &tail[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Every bucket file the soup directory `dir` holds from before there was
/// ONE (msg 235): each cluster's (`drafts/<cluster>.json`, design 0138),
/// then the environment's one before that (`draft.json`).
pub fn legacy_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir.join("drafts"))
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect())
        .unwrap_or_default();
    paths.sort();
    paths.push(dir.join("draft.json"));
    paths
}

/// What a face watches for an OLDER face still writing the old buckets: the
/// old files and their chips' bodies.
pub fn legacy_paths(dir: &Path) -> Vec<PathBuf> {
    vec![dir.join("drafts"), dir.join("draft.json")]
}

/// The record of what the one bucket has taken in from each old bucket file
/// (0217 B1): the file's text as it was when adopted, keyed by its path under
/// the soup directory. An older face still running keeps writing the old
/// files after the one bucket exists, so adoption is not once: whatever an
/// old file gained since its recorded text comes in too. Nothing old is
/// ever deleted — not by adoption and not by garbage collection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adopted {
    #[serde(default)]
    pub files: std::collections::BTreeMap<String, String>,
    /// Per file, the last part of it that came into the bucket, as it went
    /// in: an older face typing on glues on while the bucket still ends with
    /// it (0252 G5).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub parts: std::collections::BTreeMap<String, String>,
}

pub const ADOPTED_FILE: &str = "adopted.json";

impl Adopted {
    pub fn load(dir: &Path) -> Adopted {
        read_json::<Adopted>(&dir.join(ADOPTED_FILE)).ok().flatten().unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        write_atomic(&dir.join(ADOPTED_FILE), self)
    }
}

/// An old text as the one bucket holds it: a dictation's chip (`[audio …]`,
/// retired by msg 235: "I actually don't want to support transcription
/// directly into the Cauldron") as a paste chip, its body kept, and every
/// old `[<slug>/<id>]` and `[surface:<slug>]` rewritten to its id in the one
/// soup ([`rewrite_legacy_refs`], task 0209 D2).
fn as_bucket_text(t: &str) -> String {
    rewrite_legacy_refs(t.trim_matches('\n').replace("[audio ", "[paste ").as_str())
}

/// What the old buckets in `dir` hold that the one bucket (`bucket`, its
/// text now) does not, and the [`Adopted`] record once it is taken in.
///
/// Per old file: nothing when its text is the one recorded; otherwise what
/// it GAINED since its record, judged BY POSITION (0252 G4): what it typed
/// on after the recorded text, or, if it was edited, what the edit put in
/// (its deletions are not applied: nothing is ever taken out of the bucket
/// here). A gain is new text whatever the bucket already holds elsewhere —
/// `the`, `ok`, one letter — and comes in unless the bucket already ENDS
/// with it (another face took the same write in first). A file with no
/// record comes in whole, unless the bucket (or an earlier part) already
/// holds it. Parts are joined by a blank line. With no bucket yet (`bucket`
/// empty, no record) this is the first adoption: every old text, in
/// [`legacy_files`]' order.
///
/// The `bool` says the text CONTINUES the bucket's last line (0225 F5, 0252
/// G5): an older face typing on in a file already taken in, while the bucket
/// still ends with what last came in from it (or, the first time, with the
/// recorded text's last line), so what it gained goes on that line as it was
/// typed — its spaces too — and not in a paragraph of its own per append.
pub fn readopt(dir: &Path, bucket: &str, record: &Adopted) -> (Option<String>, bool, Adopted) {
    let mut next = record.clone();
    let mut parts: Vec<String> = Vec::new();
    let mut glued = false;
    for p in legacy_files(dir) {
        let Some(raw) = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<DraftFile>(&t).ok()).map(|d| d.text) else { continue };
        let key = p.strip_prefix(dir).unwrap_or(&p).to_string_lossy().into_owned();
        let Some(was) = record.files.get(&key) else {
            // Never taken in: whole, unless it is here already.
            let part = as_bucket_text(&raw);
            let held = |t: &str| t.contains(part.trim());
            if !part.trim().is_empty() && !held(bucket) && !parts.iter().any(|x| held(x)) {
                next.parts.insert(key.clone(), part.clone());
                parts.push(part);
            }
            next.files.insert(key, raw);
            continue;
        };
        next.files.insert(key.clone(), raw.clone());
        if raw.trim_end() == was.trim_end() {
            continue;
        }
        let typed_on = raw.starts_with(was.trim_end()) && !was.trim().is_empty();
        let gain = if typed_on { &raw[was.trim_end().len()..] } else { one_edit(was, &raw).2 };
        let gain = rewrite_legacy_refs(&gain.replace("[audio ", "[paste "));
        if gain.trim().is_empty() && !typed_on {
            continue;
        }
        let tail = bucket.trim_end_matches('\n');
        let last_in = record.parts.get(&key).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let run = format!("{}{}", last_in.clone().unwrap_or_default(), gain.trim_end_matches('\n'));
        // Another face took this same write in first: the bucket ends with
        // the whole run already, this gain and all.
        if typed_on && last_in.is_some() && !gain.trim().is_empty() && tail.trim_end().ends_with(run.trim()) {
            next.parts.insert(key, run);
            continue;
        }
        let first_line = as_bucket_text(was);
        let first_line = first_line.trim_end().lines().last().unwrap_or("").trim();
        let continues = typed_on && parts.is_empty() && match &last_in {
            Some(l) => tail.trim_end().ends_with(l.as_str()),
            None => !first_line.is_empty() && tail.trim_end().ends_with(first_line),
        };
        if continues {
            // As typed: a space it typed is kept, so its next word is a word.
            next.parts.insert(key, run);
            parts.push(gain.trim_end_matches('\n').to_string());
            glued = true;
            continue;
        }
        // A part of its own starts at its word, unless another face put it in
        // just so already.
        let part = as_bucket_text(gain.trim_start());
        if !part.trim().is_empty() {
            next.parts.insert(key, part.clone());
            if !tail.ends_with(&format!("\n\n{part}")) {
                parts.push(part);
            }
        }
    }
    ((!parts.is_empty()).then(|| parts.join("\n\n")), glued, next)
}

impl DraftStore {
    /// The cauldron's one bucket, in the soup directory `dir`.
    pub fn new(dir: &Path, writable: bool) -> Self {
        DraftStore { path: dir.join("bucket.json"), last: None, base: String::new(), seen: Vec::new(), prev_unmarked: None, own: Vec::new(), writable }
    }

    /// The bucket on disk, every old `[<slug>/<id>]` in it read as its id
    /// in the one soup (0220 E7: an older face still writes slugs into the
    /// one bucket too, so the rewrite is on every read, not only on
    /// adoption). The file itself changes only at this face's next write.
    fn read(&self) -> Result<Option<DraftFile>> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => {
                let mut d: DraftFile = serde_json::from_str(&s).with_context(|| format!("parse {}", self.path.display()))?;
                d.text = rewrite_legacy_refs(&d.text);
                Ok(Some(d))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", self.path.display())),
        }
    }

    /// Whether the bucket is on disk.
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// The persisted bucket, adopted as this face's own.
    pub fn load(&mut self) -> Result<String> {
        let d = self.read()?;
        self.last = d.as_ref().map(|d| d.nonce.clone());
        let text = d.as_ref().map(|d| d.text.clone()).unwrap_or_default();
        self.set_base(text.clone(), None);
        Ok(text)
    }

    /// `text` is now the base: kept among the recent buckets too. `from`:
    /// where its characters came from, merged from the base before (`None`:
    /// none of it typed here).
    fn set_base(&mut self, text: String, from: Option<&[From]>) {
        self.own = match from {
            Some(from) => from.iter().map(|f| matches!(f, From::Mine) || matches!(f, From::Base(g) | From::Kept(g) if self.own.get(*g) == Some(&true))).collect(),
            None => vec![false; text.chars().count()],
        };
        if self.seen.last() != Some(&text) {
            self.seen.retain(|t| *t != text);
            self.seen.push(text.clone());
            if self.seen.len() > SEEN_KEPT {
                self.seen.remove(0);
            }
        }
        self.base = text;
    }

    /// Start a bucket that is not on disk yet from `text` (the seed's): it is
    /// the base another face starting from the same seed merges against.
    pub fn start_from(&mut self, text: &str) {
        if self.last.is_none() {
            self.set_base(text.to_string(), None);
        }
    }

    /// What another face wrote since this one last looked, merged into this
    /// face's text `mine` (caret at byte `caret`), if anything: the merged
    /// text and where the caret goes in it. The merge is written back when
    /// it holds anything the file does not, so the other face gets this
    /// face's words in turn.
    pub fn poll(&mut self, mine: &str, caret: usize) -> Result<Option<(String, usize)>> {
        let Some(d) = self.read()? else { return Ok(None) };
        if self.last.as_deref() == Some(d.nonce.as_str()) {
            return Ok(None);
        }
        self.save(mine, caret)
    }

    /// Write this face's text `text` (caret at byte `caret`) atomically. When
    /// another face wrote since this one last looked, what is written is the
    /// merge of the two, and it is returned with the caret's place in it:
    /// the face must take it as its text, or its next write would read the
    /// other face's words as deleted. `None`: `text` went to disk as it is.
    ///
    /// A face that does not persist writes nothing, and still merges what
    /// another face wrote.
    pub fn save(&mut self, text: &str, caret: usize) -> Result<Option<(String, usize)>> {
        let dir = self.path.parent().context("bucket path has no parent")?.to_path_buf();
        if self.writable {
            std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let _lock = if self.writable { Some(lock_file(&self.path.with_extension("lock"))?) } else { None };
        let disk = self.read()?;
        let foreign = disk.as_ref().filter(|d| self.last.as_deref() != Some(d.nonce.as_str()));
        let (out, moved, from) = match foreign {
            Some(d) => {
                let (m, here) = if d.merges {
                    (merge_chars(&self.base, text, &d.text, caret), true)
                } else {
                    let m = merge_unmarked(&self.seen, self.prev_unmarked.as_deref(), &self.base, &self.own, text, &d.text, caret);
                    self.prev_unmarked = Some(d.text.clone());
                    m
                };
                // A character an unmarked write holds has been seen by that
                // face, so it no longer counts as typed here; and merged from an
                // older bucket, which were typed here is not known.
                let from: Vec<From> = if here && d.merges { m.from } else { m.from.into_iter().map(|f| if matches!(f, From::Base(_)) { From::Theirs } else { f }).collect() };
                let (merged, at) = (m.text, m.at);
                if merged == d.text {
                    // Nothing here the file lacks: adopt it, write nothing.
                    self.last = Some(d.nonce.clone());
                    self.set_base(merged.clone(), Some(&from));
                    return Ok((merged != text).then_some((merged, at)));
                }
                let moved = (merged != text).then(|| (merged.clone(), at));
                (merged, moved, from)
            }
            None if disk.as_ref().is_some_and(|d| d.text == text) => return Ok(None),
            None => (text.to_string(), None, merge_chars(&self.base, text, &self.base, caret).from),
        };
        if !self.writable {
            if let Some(d) = foreign {
                self.last = Some(d.nonce.clone());
            }
            self.set_base(out, Some(&from));
            return Ok(moved);
        }
        let nonce = uuid::Uuid::new_v4().to_string();
        let file = DraftFile {
            nonce: nonce.clone(),
            writer: writer_label(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            text: out.clone(),
            merges: true,
        };
        let tmp = dir.join(format!(".bucket.{nonce}.tmp"));
        {
            let mut fh = std::fs::File::create(&tmp)
                .with_context(|| format!("create {}", tmp.display()))?;
            fh.write_all(serde_json::to_string(&file)?.as_bytes())?;
            fh.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), self.path.display()))?;
        self.last = Some(nonce);
        self.set_base(out, Some(&from));
        Ok(moved)
    }
}

/// An exclusive `flock` on `path` (created if missing), held until the file
/// is dropped. The OS releases it when the process dies, so a face killed
/// mid-write leaves no lock behind.
fn lock_file(path: &Path) -> Result<std::fs::File> {
    let fh = OpenOptions::new().create(true).truncate(false).write(true).open(path).with_context(|| format!("open {}", path.display()))?;
    fh.lock().with_context(|| format!("lock {}", path.display()))?;
    Ok(fh)
}

/// The one edit that turns `base` into `new`: the byte range of `base` it
/// replaces and what replaces it (common prefix and suffix kept, on char
/// boundaries).
fn one_edit<'a>(base: &str, new: &'a str) -> (usize, usize, &'a str) {
    let mut p = base.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    while !base.is_char_boundary(p) || !new.is_char_boundary(p) {
        p -= 1;
    }
    let room = base.len().min(new.len()) - p;
    let mut q = base.bytes().rev().zip(new.bytes().rev()).take(room).take_while(|(a, b)| a == b).count();
    while !base.is_char_boundary(base.len() - q) || !new.is_char_boundary(new.len() - q) {
        q -= 1;
    }
    (p, base.len() - q, &new[p..new.len() - q])
}

/// A write from a face that does not mark it `merges`: whose base to merge
/// it from ([`merge3`]). Two kinds of older face write that way. The one he
/// runs (b7c1cb42) is not blind: it merges too, under the same lock, so its
/// write is the file it replaced plus its own keystrokes — and when it
/// writes several times between two of this face's ticks, it is this face's
/// base plus all of them. So by default its base is THIS face's base (the
/// file this face last wrote or adopted), and the merge is exact: an in-word
/// edit is an edit, a deletion is a deletion (0252 G1, G2).
///
/// An older face still (before b7c1cb42) never merged: it adopted the file
/// on its tick and wrote its own text over it, so a write between the two is
/// STALE — some earlier bucket plus what it typed since — and against this
/// base it takes out what this face typed meanwhile, which it never saw. So
/// a write that takes out text THIS FACE TYPED and it never wrote back
/// (`own`, per base character) while typing text of its own, and is one
/// recent bucket
/// (`seen`, or its own previous write `prev`) plus an insertion this face
/// does not already hold there is merged from that
/// bucket instead, the cheapest, the newest of equals: what it typed comes in once and nothing of this face's goes
/// (0225 F2, 0252 G3). The same face saves on a keystroke that changes
/// nothing, writing its previous text again: a write equal to `prev` that
/// would change more than one character here changes nothing. (One is
/// b7c1cb42's Backspace over the letter this face typed since its last
/// write, and his face is the one that runs: it applies.)
///
/// A character this face typed stops counting once an unmarked write holds
/// it: that face has seen it. Where the two still cannot be told apart —
/// b7c1cb42 deleting, between two ticks here, text this face typed that it
/// has not written back yet, and typing on — only what it inserted comes
/// in: his deletion comes back, and nothing is lost.
/// How to merge an unmarked write ([`unmarked_base`]).
enum Unmarked<'a> {
    /// A keystroke that changed nothing: nothing to merge.
    Nothing,
    /// Merge it from this base, exactly.
    From(&'a str),
    /// Take in only what it inserted (from this base): what it took out may
    /// have been text it never saw.
    Inserts(&'a str),
}

fn unmarked_base<'a>(seen: &'a [String], prev: Option<&'a str>, base: &'a str, own: &[bool], mine: &str, theirs: &str) -> Unmarked<'a> {
    let (s, e, t) = one_edit(base, theirs);
    if prev == Some(theirs) && base[s..e].chars().count() + t.chars().count() > 1 {
        return Unmarked::Nothing;
    }
    let b: Vec<char> = base.chars().collect();
    let dt = char_diff(&b, &theirs.chars().collect::<Vec<_>>());
    // What it took out that this face typed and it has never written back.
    // Taken out with nothing typed is a deletion — Backspaces, a clear after
    // a send — since b7c1cb42 takes this face's text in on its tick without
    // writing; once it has written, all it holds is seen.
    let unseen_gone = dt.kept.iter().zip(own).filter(|(k, o)| **o && !**k).count();
    if unseen_gone == 0 || t.is_empty() {
        return Unmarked::From(base);
    }
    let m: Vec<char> = mine.chars().collect();
    let mut best: Option<(&str, usize)> = None;
    for c in prev.into_iter().chain(seen.iter().rev().map(String::as_str)).filter(|c| *c != base && *c != theirs) {
        let (s, e, t) = one_edit(c, theirs);
        if s != e || t.is_empty() || best.is_some_and(|b| t.len() >= b.1) {
            continue;
        }
        // Text this face already holds at that spot is not its insertion.
        let cc: Vec<char> = c.chars().collect();
        let here: String = char_diff(&cc, &m).ins[c[..s].chars().count()].iter().collect();
        if !here.contains(t) {
            best = Some((c, t.len()));
        }
    }
    best.map_or(Unmarked::Inserts(base), |b| Unmarked::From(b.0))
}

/// What b7c1cb42 MEANT to write, when `theirs` is what its merge wrote over
/// `file` (this face's base) where the two edits OVERLAPPED. That build
/// merges one edit per side, and where they overlap it keeps both versions
/// of the stretch (its doc: "text may come out twice"): when this face made
/// two edits apart between two of its syncs and he typed between them in the
/// older face, the stretch comes back twice. That build is the one his live
/// face runs until it restarts, and its code cannot change, so the write is
/// read here as what it meant: b7c1cb42's own text (`file`'s base, `b`, one
/// keystroke on) merged character by character with `file`.
///
/// Recognised only exactly: `theirs` is `file` with one stretch inserted, and
/// for one recent bucket `b` (the base it merged from) and one keystroke on
/// it (`x`: an insertion, or one character deleted), b7c1cb42's merge of `b`,
/// `x` and `file` IS `theirs`, and the stretch holds text of `b` beside what
/// it typed. A plain insertion it typed never matches: its stretch is only
/// what it typed.
fn b7c1cb42_meant<'a>(cands: impl Iterator<Item = &'a str>, file: &str, theirs: &str) -> Option<String> {
    let (p, e, ins) = one_edit(file, theirs);
    if p != e || ins.is_empty() {
        return None;
    }
    let l = ins.len();
    for b in cands.filter(|b| *b != file).take(16) {
        let (bs, be, _) = one_edit(b, file);
        // Where the stretch may have gone in: as far back as the insertion
        // can slide over equal text, never past where `b` and `file` part.
        let top = p.min(bs);
        for us in (top.saturating_sub(256)..=top).rev().filter(|i| file.is_char_boundary(*i) && theirs.is_char_boundary(*i + l)) {
            if theirs[us + l..] != file[us..] {
                continue;
            }
            let m = &theirs[us..us + l];
            for ue in (be..=b.len().min(be + 64)).filter(|i| b.is_char_boundary(*i)) {
                let x = format!("{}{m}{}", &b[..us], &b[ue..]);
                // One keystroke: an insertion, or one character deleted.
                let (xs, xe, xt) = one_edit(b, &x);
                if (xs != xe && !xt.is_empty()) || xt == m || b[xs..xe].chars().count() > 1 {
                    continue;
                }
                if merge3_b7c1cb42(b, &x, file) == (theirs.to_string(), true) {
                    return Some(merge_chars(b, &x, file, 0).text);
                }
            }
        }
    }
    None
}

/// b7c1cb42's `merge3`, as it was, kept only so [`b7c1cb42_meant`] can
/// recognise what it wrote. Never used to merge. The `bool`: the edits
/// overlapped and it kept BOTH versions of the stretch, which is the only
/// case in which it wrote text twice.
fn merge3_b7c1cb42(base: &str, mine: &str, theirs: &str) -> (String, bool) {
    if mine == base {
        return (theirs.to_string(), false);
    }
    if theirs == base {
        return (mine.to_string(), false);
    }
    let (as_, ae, ta) = one_edit(base, mine);
    let (bs, be, tb) = one_edit(base, theirs);
    if ae <= bs {
        (format!("{}{ta}{}{tb}{}", &base[..as_], &base[ae..bs], &base[be..]), false)
    } else if be <= as_ {
        (format!("{}{tb}{}{ta}{}", &base[..bs], &base[be..as_], &base[ae..]), false)
    } else {
        let (us, ue) = (as_.min(bs), ae.max(be));
        let m = format!("{}{ta}{}", &base[us..as_], &base[ae..ue]);
        let t = format!("{}{tb}{}", &base[us..bs], &base[be..ue]);
        let both = !t.is_empty() && t != m && !m.is_empty();
        let t = if both { t } else { String::new() };
        (format!("{}{m}{t}{}", &base[..us], &base[ue..]), both)
    }
}

/// [`merge3`] of a write without the `merges` mark, its base found by
/// [`unmarked_base`]. `own` marks which of `base`'s characters this face
/// typed.
pub fn merge3_unmarked(seen: &[String], prev: Option<&str>, base: &str, own: &[bool], mine: &str, theirs: &str, caret: usize) -> (String, usize) {
    let (m, _) = merge_unmarked(seen, prev, base, own, mine, theirs, caret);
    (m.text, m.at)
}

/// The merge, and whether it was merged from `base` itself.
fn merge_unmarked(seen: &[String], prev: Option<&str>, base: &str, own: &[bool], mine: &str, theirs: &str, caret: usize) -> (Merged, bool) {
    if let Some(meant) = b7c1cb42_meant(prev.into_iter().chain(seen.iter().rev().map(String::as_str)), base, theirs) {
        return (merge_chars(base, mine, &meant, caret), true);
    }
    match unmarked_base(seen, prev, base, own, mine, theirs) {
        Unmarked::Nothing => (merge_chars(base, mine, base, caret), true),
        Unmarked::From(from) => (merge_chars(from, mine, theirs, caret), std::ptr::eq(from, base)),
        Unmarked::Inserts(from) => (merge_inserts(from, mine, theirs, caret), true),
    }
}

/// The THREE-WAY MERGE of a bucket (0220 E2, 0252 G1), CHARACTER BY
/// CHARACTER: `base` is the text two faces both started from, `mine` this
/// face's text now (its caret at byte `caret`), `theirs` what the other face
/// wrote. Each side's change is its character diff from `base` (a longest
/// common subsequence): which base characters it kept and what it inserted
/// in each gap between them, so an edit inside a word is an edit inside that
/// word, wherever it is and however many there are.
///
/// The rule, which never loses and never doubles a character either side
/// typed: a base character stays only if NEITHER side deleted it (both
/// deleting it deletes it once); every character either side inserted is in
/// the result exactly once; two inserts in one gap both stay, mine first —
/// even the same letter typed in both, since each face typed it. Each face's
/// caret stays where it was typing and the other's text goes after it, so two
/// faces typing at one spot each write in one piece, never interleaved
/// letter by letter. Deterministic. Returns the merged text and the caret's
/// place in it.
pub fn merge3(base: &str, mine: &str, theirs: &str, caret: usize) -> (String, usize) {
    let m = merge_chars(base, mine, theirs, caret);
    (m.text, m.at)
}

/// [`merge_chars`], applying only theirs' insertions: a base character
/// theirs took out stays when mine kept it.
fn merge_inserts(base: &str, mine: &str, theirs: &str, caret: usize) -> Merged {
    let b: Vec<char> = base.chars().collect();
    let t: Vec<char> = theirs.chars().collect();
    let dt = char_diff(&b, &t);
    let theirs_kept: String = (0..=b.len()).flat_map(|g| dt.ins[g].iter().copied().chain(b.get(g).copied())).collect();
    let mut m = merge_chars(base, mine, &theirs_kept, caret);
    for f in m.from.iter_mut() {
        if let From::Base(g) = *f {
            if !dt.kept[g] {
                *f = From::Kept(g);
            }
        }
    }
    m
}

/// Where a merged character came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum From {
    /// The base's character at this index, kept by both.
    Base(usize),
    /// The base's character at this index, kept by mine though theirs took
    /// it out ([`merge_inserts`]).
    Kept(usize),
    Mine,
    Theirs,
}

struct Merged {
    text: String,
    at: usize,
    from: Vec<From>,
}

fn merge_chars(base: &str, mine: &str, theirs: &str, caret: usize) -> Merged {
    let b: Vec<char> = base.chars().collect();
    let (m, t): (Vec<char>, Vec<char>) = (mine.chars().collect(), theirs.chars().collect());
    let (dm, dt) = (char_diff(&b, &m), char_diff(&b, &t));
    let caret = (0..=caret.min(mine.len())).rev().find(|i| mine.is_char_boundary(*i)).unwrap_or(0);
    let ck = mine[..caret].chars().count();
    let mut out = String::with_capacity(mine.len().max(theirs.len()));
    let mut from = Vec::with_capacity(m.len().max(t.len()));
    let (mut oc, mut at) = (0usize, None);
    for g in 0..=b.len() {
        for &c in &dm.ins[g] {
            if oc == ck && at.is_none() {
                at = Some(out.len());
            }
            out.push(c);
            from.push(From::Mine);
            oc += 1;
        }
        // The caret before mine's next character: theirs' insert here goes after it.
        if oc == ck && at.is_none() {
            at = Some(out.len());
        }
        for &c in &dt.ins[g] {
            out.push(c);
            from.push(From::Theirs);
        }
        if g < b.len() && dm.kept[g] {
            oc += 1;
            if dt.kept[g] {
                out.push(b[g]);
                from.push(From::Base(g));
            }
        }
    }
    let at = at.unwrap_or(out.len());
    Merged { text: out, at, from }
}

/// What one side did to `base`: per base character whether it kept it, and
/// per gap (before base character `g`, `base.len()` the end) what it
/// inserted there.
struct CharDiff {
    kept: Vec<bool>,
    ins: Vec<Vec<char>>,
}

fn char_diff(base: &[char], new: &[char]) -> CharDiff {
    let mut kept = vec![false; base.len()];
    let mut ins = vec![Vec::new(); base.len() + 1];
    let pairs = lcs_pairs(base, new);
    let mut j = 0;
    for &(bi, nj) in pairs.iter().chain(std::iter::once(&(base.len(), new.len()))) {
        // What `new` holds before its next kept character goes in that gap,
        // after whatever base characters it dropped there.
        ins[bi].extend_from_slice(&new[j..nj]);
        if bi < base.len() {
            kept[bi] = true;
        }
        j = nj + 1;
    }
    CharDiff { kept, ins }
}

/// A longest common subsequence of `a` and `b`, as matched index pairs in
/// order: the shared prefix and suffix taken directly, the middle by
/// Hirschberg's divide and conquer (linear space, so a long bucket costs
/// time and never a table of its square).
pub(crate) fn lcs_pairs<T: PartialEq>(a: &[T], b: &[T]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    hirschberg(a, b, 0, 0, &mut out);
    out
}

fn hirschberg<T: PartialEq>(a: &[T], b: &[T], ao: usize, bo: usize, out: &mut Vec<(usize, usize)>) {
    let p = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    for i in 0..p {
        out.push((ao + i, bo + i));
    }
    let (a2, b2) = (&a[p..], &b[p..]);
    let q = a2.iter().rev().zip(b2.iter().rev()).take_while(|(x, y)| x == y).count();
    let (a3, b3) = (&a2[..a2.len() - q], &b2[..b2.len() - q]);
    let (ao3, bo3) = (ao + p, bo + p);
    if a3.is_empty() || b3.is_empty() {
    } else if a3.len() == 1 {
        if let Some(j) = b3.iter().position(|y| *y == a3[0]) {
            out.push((ao3, bo3 + j));
        }
    } else {
        let mid = a3.len() / 2;
        let l = lcs_row(&a3[..mid], b3, false);
        let r = lcs_row(&a3[mid..], b3, true);
        let split = (0..=b3.len()).max_by_key(|&j| (l[j] + r[b3.len() - j], std::cmp::Reverse(j))).unwrap_or(0);
        hirschberg(&a3[..mid], &b3[..split], ao3, bo3, out);
        hirschberg(&a3[mid..], &b3[split..], ao3 + mid, bo3 + split, out);
    }
    let (ae, be) = (a.len() - q, b.len() - q);
    for i in 0..q {
        out.push((ao + ae + i, bo + be + i));
    }
}

/// The last row of the LCS-length table of `a` against every prefix of `b`
/// (of both reversed when `rev`, so entry j is against b's last j).
fn lcs_row<T: PartialEq>(a: &[T], b: &[T], rev: bool) -> Vec<usize> {
    let n = b.len();
    let mut prev = vec![0usize; n + 1];
    let mut cur = vec![0usize; n + 1];
    let ix = |len: usize, i: usize| if rev { len - 1 - i } else { i };
    for i in 0..a.len() {
        for j in 0..n {
            cur[j + 1] = if a[ix(a.len(), i)] == b[ix(n, j)] { prev[j] + 1 } else { cur[j].max(prev[j + 1]) };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev
}

// ── the sends ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Send {
    /// Per-environment sequence, 1-based: "send three" means one thing.
    pub seq: u64,
    pub at: String,
    pub env: String,
    /// The cluster he was looking at when it was sent.
    pub from_cluster: String,
    pub text: String,
    /// Every tag in `text`, rendered, in order of appearance.
    pub tags: Vec<String>,
    /// The clusters this send lands on: the clusters its tags point into (a
    /// region or node tag points into the clusters that claim it). None: it
    /// goes into the soup, where the dump ripple routes it (msg 235: "we
    /// don't need to dump into a specific zone").
    pub targets: Vec<String>,
    /// Its NOTES — the text cut at its blank lines, each with its anchors
    /// and its chips (design 0138 §4.5): `dump 12, note 2` is note 2 here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<crate::cauldron::dump::Note>,
    /// Its chips' bodies, by id (`paste:b21c`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub artifacts: std::collections::BTreeMap<String, crate::cauldron::dump::ArtifactRef>,
}

pub struct SendLog {
    pub path: PathBuf,
    /// Bytes of the file already parsed.
    read_to: u64,
    pub sends: Vec<Send>,
    writable: bool,
}

/// The seed's dumps for an environment (`<seeds>/dumps.json` for prod,
/// `<seeds>/soup/<env>.dumps.json` otherwise), FAKE: dumps already sent, and
/// the bucket being written (used only while the cauldron has no bucket of
/// its own on disk). An `at` of `-2h` is that long before now.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DumpSeed {
    #[serde(default, rename = "_note")]
    pub note: String,
    #[serde(default)]
    pub dumps: Vec<Send>,
    #[serde(default)]
    pub draft: String,
}

impl DumpSeed {
    pub fn load(seeds: &Path, env: &str) -> Result<DumpSeed> {
        let own = seeds.join("soup").join(format!("{env}.dumps.json"));
        let p = if own.is_file() { own } else if env == "prod" { seeds.join("dumps.json") } else { return Ok(DumpSeed::default()) };
        let Some(mut d) = read_json::<DumpSeed>(&p)? else { return Ok(DumpSeed::default()) };
        for s in &mut d.dumps {
            if let Some(rel) = s.at.strip_prefix('-') {
                let (n, unit) = rel.split_at(rel.len().saturating_sub(1));
                let n: i64 = n.parse().with_context(|| format!("{}: `at` {}", p.display(), s.at))?;
                let secs = match unit {
                    "m" => 60,
                    "h" => 3600,
                    "d" => 86400,
                    _ => 1,
                } * n;
                s.at = (chrono::Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339();
            }
        }
        Ok(d)
    }
}

impl SendLog {
    /// Put the seed's dumps first: a dump sent here is numbered after them.
    pub fn seed(&mut self, dumps: Vec<Send>) {
        let mut all = dumps;
        all.append(&mut self.sends);
        self.sends = all;
    }

    pub fn new(dir: &Path, writable: bool) -> Self {
        let path = dir.join("dumps.jsonl");
        let old = dir.join("sends.jsonl");
        // Before design 0138 the log was `sends.jsonl`: renamed once, by the
        // first face that may write; read in place by one that may not.
        let path = if !path.exists() && old.exists() {
            if writable && std::fs::rename(&old, &path).is_ok() {
                path
            } else {
                old
            }
        } else {
            path
        };
        SendLog { path, read_to: 0, sends: Vec::new(), writable }
    }

    /// Take into `dumps.jsonl` every dump an OLDER face appended to
    /// `sends.jsonl` after the rename (0220 E14: it recreates the old file and
    /// appends there, and a dump left only there reached no face and no
    /// ripple). Each whole line past what was taken comes in renumbered, under
    /// the log's lock; how far `sends.jsonl` was taken is kept beside it
    /// (`sends.jsonl.taken`). `sends.jsonl` itself is never changed. Returns
    /// how many came in, and the lines that could not be read (left where
    /// they are).
    pub fn adopt_old_log(&mut self) -> Result<(usize, usize)> {
        let Some(dir) = self.path.parent().map(Path::to_path_buf) else { return Ok((0, 0)) };
        let old = dir.join("sends.jsonl");
        if !self.writable || self.path == old || !old.is_file() {
            return Ok((0, 0));
        }
        let mark = dir.join("sends.jsonl.taken");
        let read_mark = || std::fs::read_to_string(&mark).ok().and_then(|t| t.trim().parse::<u64>().ok()).unwrap_or(0);
        let len = std::fs::metadata(&old).map(|m| m.len()).unwrap_or(0);
        if read_mark() == len {
            return Ok((0, 0));
        }
        let path = self.path.clone();
        let out = env::lock::with_lock(&path, || {
            let mut taken = read_mark();
            let mut buf = Vec::new();
            {
                let mut fh = std::fs::File::open(&old).with_context(|| format!("open {}", old.display()))?;
                if taken > fh.metadata()?.len() {
                    // A different file under the old name: read it whole.
                    taken = 0;
                }
                fh.seek(SeekFrom::Start(taken))?;
                fh.read_to_end(&mut buf)?;
            }
            let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else { return Ok((0, 0)) };
            let complete = &buf[..=last_nl];
            self.poll()?;
            let mut seq = self.sends.iter().map(|s| s.seq).max().unwrap_or(0);
            let mut text = String::new();
            let (mut n, mut bad) = (0, 0);
            for line in complete.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
                match serde_json::from_slice::<Send>(line) {
                    Ok(mut send) => {
                        seq += 1;
                        send.seq = seq;
                        text.push_str(&serde_json::to_string(&send)?);
                        text.push('\n');
                        n += 1;
                    }
                    Err(_) => bad += 1,
                }
            }
            if !text.is_empty() {
                let mut fh = OpenOptions::new().create(true).append(true).open(&path).with_context(|| format!("open {}", path.display()))?;
                fh.write_all(text.as_bytes())?;
                fh.sync_all()?;
            }
            write_text_atomic(&mark, &(taken + complete.len() as u64).to_string())?;
            Ok((n, bad))
        })?;
        self.poll()?;
        Ok(out)
    }

    /// Read whatever has been appended since the last call. Returns how many
    /// new sends arrived. A trailing partial line is left for next time.
    pub fn poll(&mut self) -> Result<usize> {
        let mut fh = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e).with_context(|| format!("open {}", self.path.display())),
        };
        let len = fh.metadata()?.len();
        if len <= self.read_to {
            return Ok(0);
        }
        fh.seek(SeekFrom::Start(self.read_to))?;
        let mut buf = Vec::new();
        fh.read_to_end(&mut buf)?;
        let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else { return Ok(0) };
        let complete = &buf[..=last_nl];
        let mut n = 0;
        for line in complete.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let send: Send = serde_json::from_slice(line)
                .with_context(|| format!("parse a line of {}", self.path.display()))?;
            self.sends.push(send);
            n += 1;
        }
        self.read_to += complete.len() as u64;
        Ok(n)
    }

    /// Append one send. Returns it with its sequence number. The sequence is
    /// taken INSIDE the lock from the file itself, so two faces sending at once
    /// get two different numbers.
    ///
    /// `clusters_of` says which clusters a tag lands on: a cluster tag its own cluster, a
    /// region tag the clusters that claim the region, a node tag the clusters that
    /// claim any region it attests. The caller owns that map; this log only
    /// records the answer.
    pub fn append(
        &mut self,
        env: &str,
        from_cluster: &str,
        text: &str,
        artifacts: std::collections::BTreeMap<String, crate::cauldron::dump::ArtifactRef>,
        clusters_of: impl Fn(&tags::Tag) -> Vec<String>,
    ) -> Result<Send> {
        let found = tags::scan(text);
        let tag_strings: Vec<String> = found.iter().map(|(_, t)| t.render()).collect();
        let mut targets: Vec<String> = Vec::new();
        for (_, t) in &found {
            for g in clusters_of(t) {
                if !targets.contains(&g) {
                    targets.push(g);
                }
            }
        }
        let mut send = Send {
            seq: 0,
            at: chrono::Utc::now().to_rfc3339(),
            env: env.to_string(),
            from_cluster: from_cluster.to_string(),
            text: text.to_string(),
            tags: tag_strings,
            targets,
            notes: crate::cauldron::dump::note_records(text),
            artifacts,
        };
        if !self.writable {
            // Headless: numbered after what is already known, never written.
            send.seq = self.sends.iter().map(|s| s.seq).max().map_or(1, |n| n + 1);
            self.sends.push(send.clone());
            return Ok(send);
        }
        let dir = self.path.parent().context("sends path has no parent")?;
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let path = self.path.clone();
        env::lock::with_lock(&path, || {
            // Catch up first, so the sequence is the file's (and the
            // seed's), not this face's.
            self.poll()?;
            send.seq = self.sends.iter().map(|s| s.seq).max().map_or(1, |n| n + 1);
            let mut line = serde_json::to_string(&send)?;
            line.push('\n');
            let mut fh = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("open {}", path.display()))?;
            fh.write_all(line.as_bytes())?;
            fh.sync_all()?;
            Ok(())
        })?;
        // Read our own line back through the one reader.
        self.poll()?;
        Ok(send)
    }

    /// Sends that landed on cluster `key`, oldest first.
    pub fn for_cluster<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Send> + 'a {
        self.sends.iter().filter(move |s| s.targets.iter().any(|t| t == key))
    }
}

// ── the dev names ───────────────────────────────────────────────────────────

/// DEV ONLY — NOT FOR THE PRODUCTION VERSION. 8e4fca10 msg 235: "why don't
/// you give me the ability to try just renaming things too. I want to be
/// able to change the title of the sidebar and the tab names ... Just if I
/// like double click on one of these things, just let me just edit it in a
/// modal. And it should say in the code, this should not make it into the
/// production version. This is just for the developer."
///
/// The titles he renamed on screen, by the word the code writes, kept ONCE
/// for every cauldron (`<root>/data/shared/tui/dev_names.json`) and re-read
/// when another face writes it. Remove it, its modal
/// (`app::Modal::DevRename`) and [`crate::cauldron::designs::DEV_RENAME`]
/// before the face ships.
pub struct DevNames {
    pub path: PathBuf,
    pub names: std::collections::BTreeMap<String, String>,
    sig: Sig,
    writable: bool,
}

impl DevNames {
    pub fn load(global: &Path, writable: bool) -> Result<DevNames> {
        let path = global.join("dev_names.json");
        let names = read_json(&path)?.unwrap_or_default();
        let sig = Sig::of(std::slice::from_ref(&path));
        Ok(DevNames { path, names, sig, writable })
    }

    /// The word title `key` is shown as, if he renamed it.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.names.get(key).map(String::as_str)
    }

    /// Show title `key` as `name` (empty: as the code writes it again).
    pub fn set(&mut self, key: &str, name: &str) -> Result<()> {
        if name.is_empty() || name == key {
            self.names.remove(key);
        } else {
            self.names.insert(key.to_string(), name.to_string());
        }
        if self.writable {
            write_atomic(&self.path, &self.names)?;
            self.sig = Sig::of(std::slice::from_ref(&self.path));
        }
        Ok(())
    }

    /// Re-read the names when another face wrote them.
    pub fn poll(&mut self) -> Result<()> {
        let now = Sig::of(std::slice::from_ref(&self.path));
        if now != self.sig {
            self.names = read_json(&self.path)?.unwrap_or_default();
            self.sig = now;
        }
        Ok(())
    }
}

#[cfg(test)]
mod seed_orphan_tests {
    //! IS-32: a seeding whose face died is detected by the kernel lock and
    //! recovered, never shown `running` forever.
    use super::*;

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("seed-orphan-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pending(id: &str, run: &str, made: chrono::DateTime<chrono::Utc>) -> CreatedCluster {
        CreatedCluster {
            id: id.into(),
            name: format!("{id} name"),
            created_at: made.to_rfc3339(),
            parent: None,
            boundaries: "dreaming".into(),
            regions: Vec::new(),
            repos: Vec::new(),
            links: Vec::new(),
            seeded_by: Some(SeededBy { run: run.into(), line: "seeding · asking local matcher over 3 candidates".into(), at: "12:00".into(), pending: true }),
            unowned: false,
        }
    }

    #[test]
    fn a_held_seeding_is_running_and_once_its_holder_lets_go_it_is_interrupted() {
        let dir = scratch();
        let c = pending("X-1", "r-a", chrono::Utc::now());
        let now = chrono::Utc::now();
        let lock = SeedLock::take(&dir, "X-1", "r-a").unwrap().expect("free");
        assert_eq!(c.seed_state(&dir, now), SeedState::Running);
        assert!(matches!(seed_owner(&dir, "X-1"), SeedOwner::Live(Some(ref n)) if n.run == "r-a"));
        drop(lock);
        // A process another test forks in this moment shares the
        // descriptor until its exec closes it (O_CLOEXEC): that window is
        // bounded, so wait it out — up to 2 s — rather than read once.
        let t0 = std::time::Instant::now();
        while matches!(seed_owner(&dir, "X-1"), SeedOwner::Live(_)) && t0.elapsed() < std::time::Duration::from_secs(2) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        match c.seed_state(&dir, now) {
            SeedState::Interrupted(why) => assert!(why.contains("is gone"), "{why}"),
            other => panic!("{other:?}"),
        }
        // The ripple row says so, never `running`.
        let (_, spec) = c.spec(&dir);
        assert_eq!(spec.ripples[0].state, "interrupted");
        assert!(spec.ripples[0].line.starts_with("interrupted · "), "{}", spec.ripples[0].line);
        // A landed seeding is done whoever holds what.
        let mut landed = c.clone();
        landed.seeded_by.as_mut().unwrap().pending = false;
        assert_eq!(landed.seed_state(&dir, now), SeedState::Done);
        assert_eq!(landed.spec(&dir).1.ripples[0].state, "done");
    }

    #[test]
    fn a_seeding_whose_face_was_sigkilled_is_interrupted() {
        // A real other process takes the lock (perl's flock is the same
        // flock(2) std's try_lock uses on macOS), is SIGKILLed, and the
        // kernel lets go.
        let dir = scratch();
        let path = seed_lock_path(&dir, "X-2");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(&SeedNote { run: "r-k".into(), pid: 1, started: chrono::Utc::now().to_rfc3339() }).unwrap()).unwrap();
        let ready = dir.join("ready");
        let mut child = std::process::Command::new("perl")
            .args(["-e", r#"use Fcntl ':flock'; open(F, '<', $ARGV[0]) or die; flock(F, LOCK_EX) or die; open(R, '>', $ARGV[1]); close R; sleep 600"#])
            .arg(&path)
            .arg(&ready)
            .spawn()
            .unwrap();
        let t0 = std::time::Instant::now();
        while !ready.exists() {
            assert!(t0.elapsed() < std::time::Duration::from_secs(10), "perl never took the lock");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let c = pending("X-2", "r-k", chrono::Utc::now());
        assert_eq!(c.seed_state(&dir, chrono::Utc::now()), SeedState::Running, "a live other process holds it");
        assert!(SeedLock::take(&dir, "X-2", "r-other").unwrap().is_none(), "a live seeding cannot be taken over");
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(matches!(c.seed_state(&dir, chrono::Utc::now()), SeedState::Interrupted(_)));
        assert!(SeedLock::take(&dir, "X-2", "r-new").unwrap().is_some(), "once its face is dead it can be taken");
    }

    #[test]
    fn a_reused_pid_never_keeps_an_orphan_alive() {
        // The note names a pid that IS alive (this process's own) and nobody
        // holds the lock: a pid check would call it running.
        let dir = scratch();
        let path = seed_lock_path(&dir, "X-3");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(&SeedNote { run: "r-p".into(), pid: std::process::id(), started: chrono::Utc::now().to_rfc3339() }).unwrap()).unwrap();
        let c = pending("X-3", "r-p", chrono::Utc::now());
        match c.seed_state(&dir, chrono::Utc::now()) {
            SeedState::Interrupted(why) => assert!(why.contains(&format!("pid {}", std::process::id())), "{why}"),
            other => panic!("a live pid in the note kept it alive: {other:?}"),
        }
    }

    #[test]
    fn an_older_faces_seeding_with_no_lock_is_running_until_ten_minutes_then_interrupted() {
        let dir = scratch();
        let made = chrono::Utc::now();
        let c = pending("X-4", "r-old", made);
        assert_eq!(seed_owner(&dir, "X-4"), SeedOwner::Never);
        assert_eq!(c.seed_state(&dir, made + chrono::Duration::minutes(9)), SeedState::Running);
        match c.seed_state(&dir, made + chrono::Duration::minutes(11)) {
            SeedState::Interrupted(why) => assert!(why.contains("over 10 min"), "{why}"),
            other => panic!("{other:?}"),
        }
        // Probing never creates the file.
        assert!(!seed_lock_path(&dir, "X-4").exists());
    }

    #[test]
    fn of_two_faces_reseeding_at_once_exactly_one_wins() {
        for round in 0..20 {
            let dir = scratch();
            let file = CreatedClusters::new(&dir);
            file.add(pending("X-5", "r-dead", chrono::Utc::now()), |_| false).unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let go = |run: &'static str| {
                let (dir, barrier) = (dir.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let Some(lock) = SeedLock::take(&dir, "X-5", run).unwrap() else { return Err("held".to_string()) };
                    let by = SeededBy { run: run.into(), line: "seeding · asking local matcher over 3 candidates".into(), at: "12:01".into(), pending: true };
                    let r = CreatedClusters::new(&dir).reseed("X-5", "r-dead", by, &lock).map_err(|e| format!("{e:#}"));
                    // Hold the lock a moment, as a seeding would.
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    r
                })
            };
            let (a, b) = (go("r-a"), go("r-b"));
            let (a, b) = (a.join().unwrap(), b.join().unwrap());
            assert_eq!([a.is_ok(), b.is_ok()].iter().filter(|w| **w).count(), 1, "round {round}: {a:?} / {b:?}");
            let won = file.list().unwrap()[0].seeded_by.clone().unwrap();
            assert!(won.pending && (won.run == "r-a" || won.run == "r-b"), "{won:?}");
            assert_eq!(a.is_ok(), won.run == "r-a");
        }
    }

    #[test]
    fn a_second_reseed_after_the_first_let_go_is_refused_by_its_run() {
        let dir = scratch();
        let file = CreatedClusters::new(&dir);
        file.add(pending("X-6", "r-dead", chrono::Utc::now()), |_| false).unwrap();
        let by = |run: &str| SeededBy { run: run.into(), line: "l".into(), at: "12:02".into(), pending: true };
        let l1 = SeedLock::take(&dir, "X-6", "r-1").unwrap().unwrap();
        file.reseed("X-6", "r-dead", by("r-1"), &l1).unwrap();
        drop(l1);
        let l2 = SeedLock::take(&dir, "X-6", "r-2").unwrap().unwrap();
        let e = file.reseed("X-6", "r-dead", by("r-2"), &l2).unwrap_err();
        assert!(format!("{e:#}").contains("no longer the interrupted one"), "{e:#}");
        assert!(file.clear_seed("X-6", "r-dead", &l2).is_err(), "clear is refused the same way");
        assert_eq!(file.list().unwrap()[0].seeded_by.as_ref().unwrap().run, "r-1");
        // Clearing the CURRENT run's mark works and leaves no seeding.
        file.clear_seed("X-6", "r-1", &l2).unwrap();
        assert_eq!(file.list().unwrap()[0].seeded_by, None);
    }

    #[test]
    fn the_file_a_reseed_writes_is_one_an_older_face_reads() {
        // `SeededBy` and `CreatedCluster` are unchanged by IS-32 and both are
        // deny_unknown_fields: the written file must carry exactly the keys
        // an older face knows.
        let dir = scratch();
        let file = CreatedClusters::new(&dir);
        file.add(pending("X-7", "r-dead", chrono::Utc::now()), |_| false).unwrap();
        let l = SeedLock::take(&dir, "X-7", "r-n").unwrap().unwrap();
        file.reseed("X-7", "r-dead", SeededBy { run: "r-n".into(), line: "l".into(), at: "12:03".into(), pending: true }, &l).unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file.path).unwrap()).unwrap();
        let keys: Vec<&String> = v["clusters"][0]["seeded_by"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["at", "line", "pending", "run"]);
        // Nothing about the lock goes into the soup's own files.
        assert!(!std::fs::read_to_string(&file.path).unwrap().contains("pid"));
    }
}
