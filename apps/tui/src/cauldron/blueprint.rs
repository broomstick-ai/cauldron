//! **Repos and their blueprints** (design 0071): the repos registered in the
//! cauldron, each with its blueprint — the per-repo design graph whose node
//! grammar is `core:blueprint`'s (`PREFIX-slug-8hex` ids; six core types and
//! the secondary ones).
//!
//! A cluster CLAIMS a repo the way it claims a region: a list on the cluster,
//! many-to-many, each claim with an optional FOCUS (blueprint ids). A repo no
//! cluster claims is UNCLAIMED and listed on Settings. Claims are stored once, on
//! the cluster; every reverse view — who claims a repo, what owns a resource,
//! who consumes an interface, who is working on an entry — is computed here
//! and never stored. The blueprint stores no back-edges.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::cauldron::model::{Forest, ClusterId};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlueprintFile {
    #[serde(default, rename = "_note")]
    pub note: String,
    #[serde(default)]
    pub repos: Vec<RepoSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worktree {
    pub name: String,
    pub branch: String,
    pub head: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoSpec {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub head: String,
    #[serde(default)]
    pub worktrees: Vec<Worktree>,
    #[serde(default)]
    pub registered_at: String,
    pub blueprint: BlueprintSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlueprintSpec {
    pub schema_version: u32,
    #[serde(default)]
    pub adopted: String,
    #[serde(default)]
    pub nodes: Vec<BpNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BpEdge {
    pub kind: String,
    pub to: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BpNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub state: String,
    #[serde(default)]
    pub built: bool,
    pub handle: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub part_of: Option<String>,
    #[serde(default)]
    pub edges: Vec<BpEdge>,
    #[serde(default)]
    pub meta: serde_json::Value,
}

impl BpNode {
    /// `MOD-tui` for `MOD-tui-4f1c9a02`: the id without its hash suffix.
    pub fn short(&self) -> &str {
        short_id(&self.id)
    }

    pub fn meta_str(&self, k: &str) -> Option<&str> {
        self.meta.get(k).and_then(|v| v.as_str())
    }

    pub fn meta_u64(&self, k: &str) -> Option<u64> {
        self.meta.get(k).and_then(|v| v.as_u64())
    }

    /// The ids this node's `kind` edges point at.
    pub fn out<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.edges.iter().filter(move |e| e.kind == kind).map(|e| e.to.as_str())
    }

    /// Secondary nodes that are live: an open violation, active drag, an open
    /// contradiction, an undescribed migration.
    pub fn nags(&self) -> bool {
        matches!(
            (self.kind.as_str(), self.state.as_str()),
            ("violation", "open") | ("contradiction", "open") | ("drag", "active") | ("migration", "undescribed")
        )
    }

    /// The glyph a secondary node carries.
    pub fn glyph(&self) -> &'static str {
        match (self.kind.as_str(), self.state.as_str()) {
            ("violation", _) => "●",
            ("drag", _) => "◇",
            ("contradiction", _) => "⚡",
            ("migration", "undescribed") => "◌",
            ("capability", "live") => "✓",
            ("capability", "stubbed") => "◦",
            ("capability", "parked") => "◆",
            ("capability", _) => "✗",
            _ => " ",
        }
    }
}

/// An id without its 8-hex suffix.
pub fn short_id(id: &str) -> &str {
    match id.rsplit_once('-') {
        Some((head, tail)) if tail.len() == 8 && tail.chars().all(|c| c.is_ascii_hexdigit()) => head,
        _ => id,
    }
}

pub type RepoId = usize;

#[derive(Debug)]
pub struct Repo {
    pub name: String,
    pub path: String,
    /// The branch and head, as the seed records them; `None` for a repo
    /// registered here (this face reads no git — nothing has looked).
    pub branch: String,
    pub head: String,
    pub worktrees: Vec<Worktree>,
    /// The blueprint's schema version; `None` while the repo has no blueprint
    /// (registered here, and not adopted yet).
    pub schema_version: Option<u32>,
    pub adopted: String,
    pub nodes: Vec<BpNode>,
    pub registered_at: String,
    /// Registered in this face (`+ register repo`), not by the seed.
    pub local: bool,
}

impl Repo {
    /// `bp v1`, or `no blueprint` before one is adopted.
    pub fn bp_word(&self) -> String {
        match self.schema_version {
            Some(v) => format!("bp v{v}"),
            None => "no blueprint".into(),
        }
    }

    /// `main @ 8299dcc1`, or `branch not read` for a repo registered here.
    pub fn at_word(&self) -> String {
        if self.local {
            "branch not read".into()
        } else {
            format!("{} @ {}", self.branch, self.head)
        }
    }

    pub fn find(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    pub fn of_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = usize> + 'a {
        (0..self.nodes.len()).filter(move |&i| self.nodes[i].kind == kind)
    }

    pub fn count(&self, kind: &str) -> usize {
        self.of_kind(kind).count()
    }

    /// Live secondary nodes of `kind` (see [`BpNode::nags`]).
    pub fn live(&self, kind: &str) -> Vec<usize> {
        self.of_kind(kind).filter(|&i| self.nodes[i].nags()).collect()
    }

    /// Definitions linked over definitions, across the modules: the
    /// blueprint's coverage word is LINKED (`core:blueprint` §6).
    pub fn linked_pct(&self) -> Option<u64> {
        let (mut d, mut l) = (0, 0);
        for i in self.of_kind("module") {
            d += self.nodes[i].meta_u64("definitions").unwrap_or(0);
            l += self.nodes[i].meta_u64("linked").unwrap_or(0);
        }
        (d > 0).then(|| (l * 100 + d / 2) / d)
    }

    /// The top-level modules (no `part_of`).
    pub fn top_modules(&self) -> Vec<usize> {
        self.of_kind("module").filter(|&i| self.nodes[i].part_of.is_none()).collect()
    }

    /// The sub-modules of module `m`.
    pub fn submodules(&self, m: usize) -> Vec<usize> {
        let id = &self.nodes[m].id;
        self.of_kind("module").filter(|&i| self.nodes[i].part_of.as_deref() == Some(id)).collect()
    }

    /// The interface fronting module `m`.
    pub fn interface_of(&self, m: usize) -> Option<usize> {
        let id = &self.nodes[m].id;
        self.of_kind("interface").find(|&i| self.nodes[i].out("fronts").any(|t| t == id))
    }

    /// What `kind` edges from anywhere point at node `n` — the computed
    /// inverse (an owner, a consumer, the secondary nodes that involve it).
    pub fn inbound(&self, n: usize, kind: &str) -> Vec<usize> {
        let id = &self.nodes[n].id;
        (0..self.nodes.len()).filter(|&i| self.nodes[i].out(kind).any(|t| t == id)).collect()
    }

    /// Node ids an edge list names, resolved.
    pub fn resolve<'a>(&self, ids: impl Iterator<Item = &'a str>) -> Vec<usize> {
        ids.filter_map(|t| self.find(t)).collect()
    }

    /// Migrations, newest first; undescribed first within a day.
    pub fn migrations(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.of_kind("migration").collect();
        v.sort_by(|&a, &b| {
            let (x, y) = (&self.nodes[a], &self.nodes[b]);
            y.meta_str("at")
                .cmp(&x.meta_str("at"))
                .then((y.state == "undescribed").cmp(&(x.state == "undescribed")))
        });
        v
    }

    /// The live secondary nodes that involve node `n` or anything inside it
    /// (its owned entries and its interface, for a module).
    pub fn nags_touching(&self, n: usize) -> Vec<usize> {
        let mut scope = vec![n];
        if self.nodes[n].kind == "module" {
            scope.extend(self.resolve(self.nodes[n].out("owns")));
            scope.extend(self.interface_of(n));
        }
        let ids: Vec<&str> = scope.iter().map(|&i| self.nodes[i].id.as_str()).collect();
        (0..self.nodes.len())
            .filter(|&i| self.nodes[i].nags() && self.nodes[i].kind != "migration")
            .filter(|&i| self.nodes[i].out("involves").any(|t| ids.contains(&t)))
            .collect()
    }
}

/// One cluster's claim, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub repo: RepoId,
    /// Node indexes in the repo.
    pub focus: Vec<usize>,
}

/// A repo on a cluster's Artifacts tab, with the clusters in scope it came through
/// and the union of their focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterRepo {
    pub repo: RepoId,
    pub via: Vec<ClusterId>,
    pub focus: Vec<usize>,
}

#[derive(Debug, Default)]
pub struct Repos {
    pub repos: Vec<Repo>,
    /// Per cluster, its claims in the order it lists them.
    pub claims: Vec<Vec<Claim>>,
    /// Per repo, the clusters that claim it, in tree order.
    pub claimants: Vec<Vec<ClusterId>>,
    /// Names the registry unregisters that the seed does not register (a
    /// seed this environment no longer reads, REL-3.S2b): kept in the file,
    /// skipped here, said by the face.
    pub stale_unregistered: Vec<String>,
    /// A cluster's links that name a repo, or a node of one, no registered
    /// repo holds (a seed this environment no longer reads, REL-3.S2c): a
    /// repo claim, a claim's focus, a task's or milestone's edge. Kept in the
    /// cluster's file, skipped here, said by the face.
    pub stale_links: Vec<String>,
    /// A repo's own blueprint node whose edge or `part_of` names a node not
    /// in that blueprint (REL-3.S2d): the edge is skipped here, kept in the
    /// file, and counted in the face's stale notice.
    pub stale_refs: Vec<String>,
}

// ── the registry, one for every cauldron ────────────────────────────────────

/// A repo registered in this face (`Config › Devices › Repos › + register
/// repo`): a path and a name. FAKE in the sense every value here is: nothing
/// reads its git or adopts its blueprint, and the face says so on its page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Registered {
    pub name: String,
    pub path: String,
    pub registered_at: String,
}

/// `<root>/data/shared/tui/repos.json`: the repos registered, and the seed's
/// repos unregistered, for EVERY cauldron of the root (8e4fca10 msg 235:
/// "repos ... all that stuff belongs in global"). A cauldron still CLAIMS
/// repos, on its clusters, exactly as before; what it claims is drawn from
/// this one registry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegistryFile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registered: Vec<Registered>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unregistered: Vec<String>,
}

pub struct Registry {
    pub path: PathBuf,
}

/// The file name of the registry, global and (before msg 235) per cauldron.
pub const REGISTRY_FILE: &str = "repos.json";

impl Registry {
    /// THE registry: in the global directory `global` (`store::global_dir`).
    pub fn global(global: &Path) -> Registry {
        Registry { path: global.join(REGISTRY_FILE) }
    }

    /// A cauldron's OWN registry from before msg 235, `<soup dir>/repos.json`:
    /// read only to adopt it into the global one ([`adopt_legacy`]).
    pub fn legacy(dir: &Path) -> Registry {
        Registry { path: dir.join(REGISTRY_FILE) }
    }

    pub fn read(&self) -> Result<RegistryFile> {
        match std::fs::read_to_string(&self.path) {
            Ok(t) => serde_json::from_str(&t).with_context(|| format!("parse {}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RegistryFile::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", self.path.display())),
        }
    }

    /// Change the file under its lock, re-reading it inside the lock so two
    /// faces — of any cauldron — never lose each other's change.
    pub fn update(&self, f: impl FnOnce(&mut RegistryFile) -> Result<()>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        env::lock::with_lock(&self.path, || {
            let mut file = self.read()?;
            f(&mut file)?;
            crate::cauldron::store::write_text_atomic(&self.path, &serde_json::to_string_pretty(&file)?)
        })
    }
}

/// The blueprint seed — `<seeds>/blueprints.json` — is the GLOBAL registry's
/// seed, the same for every cauldron, as the registry is. (Before msg 235 it
/// was prod's alone, and `<seeds>/soup/<env>.blueprints.json` another's; dev's,
/// the only one, was a copy of prod's `cauldron` and is gone.)
pub fn seed_path(seeds: &Path) -> Option<PathBuf> {
    let p = seeds.join("blueprints.json");
    p.is_file().then_some(p)
}

/// The blueprint seed, or an empty one.
pub fn read_seed(seeds: &Path) -> Result<BlueprintFile> {
    match seed_path(seeds) {
        Some(p) => {
            let text = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
            serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))
        }
        None => Ok(BlueprintFile::default()),
    }
}

/// What adopting one cauldron's own registry did ([`merge_legacy`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Adopted {
    /// Repos it registered that are now registered globally.
    pub registered: Vec<String>,
    /// Seed repos it unregistered that are now unregistered globally.
    pub unregistered: Vec<String>,
    /// What could not be carried, each with why: a name registered globally
    /// at another path, or an unregistration of a repo some cauldron claims.
    pub kept_back: Vec<String>,
}

impl Adopted {
    /// Said once, on the face that adopted it.
    pub fn say(&self, cauldron: &str) -> Option<String> {
        if self.registered.is_empty() && self.unregistered.is_empty() && self.kept_back.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if !self.registered.is_empty() {
            parts.push(format!("registered {}", self.registered.join(", ")));
        }
        if !self.unregistered.is_empty() {
            parts.push(format!("unregistered {}", self.unregistered.join(", ")));
        }
        let mut s = format!("the {cauldron} cauldron's repos are global now");
        if !parts.is_empty() {
            s.push_str(&format!(": {}", parts.join("; ")));
        }
        if !self.kept_back.is_empty() {
            s.push_str(&format!(" — not carried: {}", self.kept_back.join("; ")));
        }
        Some(s)
    }
}

/// Fold one cauldron's own registry `own` into the global one `global`, whose
/// seed is `seed`. A repo it registered joins the global registry, unless the
/// name is already there (at the same path: nothing to do; at another: the
/// global one stands). A seed repo it unregistered is unregistered for every
/// cauldron only when `claimed(name)` is false — unregistering a repo some
/// cauldron's cluster claims would leave that cauldron unable to load.
pub fn merge_legacy(global: &mut RegistryFile, own: &RegistryFile, seed: &BlueprintFile, claimed: &dyn Fn(&str) -> bool) -> Adopted {
    let mut out = Adopted::default();
    for r in &own.registered {
        let seeded = seed.repos.iter().find(|x| x.name == r.name && !global.unregistered.contains(&x.name)).map(|x| x.path.clone());
        let there = global.registered.iter().find(|x| x.name == r.name).map(|x| x.path.clone()).or(seeded);
        match there {
            Some(p) if p == r.path => {}
            Some(p) => out.kept_back.push(format!("`{}` at {} (registered at {p})", r.name, r.path)),
            None => {
                global.unregistered.retain(|u| *u != r.name);
                global.registered.push(r.clone());
                out.registered.push(r.name.clone());
            }
        }
    }
    for name in &own.unregistered {
        if global.unregistered.contains(name) || !seed.repos.iter().any(|x| x.name == *name) {
            continue;
        }
        if claimed(name) {
            out.kept_back.push(format!("unregistering `{name}` (a cauldron claims it)"));
            continue;
        }
        global.unregistered.push(name.clone());
        out.unregistered.push(name.clone());
    }
    out
}

/// Adopt cauldron `dir`'s own registry from before msg 235 into the global
/// one at `global`, AT THE SOURCE: merged and then the cauldron's file
/// renamed to `repos.json.bak` (`.bak.2`, … when one is there already), BOTH
/// under the global file's lock and with the cauldron's file re-read inside
/// it — so two faces opening the same cauldron at once adopt it exactly once.
/// With `persist` false (`--dump`) nothing is written and the merge is only
/// in memory: the registry returned is what would be in force.
pub fn adopt_legacy(dir: &Path, global: &Path, seed: &BlueprintFile, persist: bool, claimed: &dyn Fn(&str) -> bool) -> Result<(RegistryFile, Adopted)> {
    let own = Registry::legacy(dir);
    let reg = Registry::global(global);
    if !own.path.is_file() {
        return Ok((reg.read()?, Adopted::default()));
    }
    if !persist {
        let mut g = reg.read()?;
        let adopted = merge_legacy(&mut g, &own.read()?, seed, claimed);
        return Ok((g, adopted));
    }
    let mut adopted = Adopted::default();
    reg.update(|g| {
        // Another face of this cauldron may have adopted it while this one
        // waited for the lock.
        if !own.path.is_file() {
            return Ok(());
        }
        adopted = merge_legacy(g, &own.read()?, seed, claimed);
        let mut bak = own.path.with_extension("json.bak");
        let mut n = 2;
        while bak.exists() {
            bak = own.path.with_extension(format!("json.bak.{n}"));
            n += 1;
        }
        std::fs::rename(&own.path, &bak).with_context(|| format!("rename {} to {}", own.path.display(), bak.display()))
    })?;
    Ok((reg.read()?, adopted))
}

impl Repos {
    /// The seed's repos, less those unregistered globally, plus those
    /// registered globally — the same for every cauldron — with `forest`'s
    /// claims resolved against them.
    pub fn load(seeds: &Path, forest: &Forest, reg: &RegistryFile) -> Result<Self> {
        Repos::build_with(read_seed(seeds)?, reg, forest)
    }

    pub fn build(file: BlueprintFile, forest: &Forest) -> Result<Self> {
        Repos::build_with(file, &RegistryFile::default(), forest)
    }

    /// Load the repos and resolve every cluster's claims. Every id must be unique
    /// across the cauldron and every edge must land in its own repo.
    pub fn build_with(file: BlueprintFile, reg: &RegistryFile, forest: &Forest) -> Result<Self> {
        let mut out = Repos::default();
        let mut ids = std::collections::HashSet::new();
        for name in &reg.unregistered {
            if !file.repos.iter().any(|r| r.name == *name) {
                out.stale_unregistered.push(name.clone());
            }
        }
        let seeded = file.repos.into_iter().filter(|r| !reg.unregistered.contains(&r.name));
        let local = reg.registered.iter().map(|r| RepoSpec {
            name: r.name.clone(),
            path: r.path.clone(),
            branch: String::new(),
            head: String::new(),
            worktrees: Vec::new(),
            registered_at: r.registered_at.clone(),
            blueprint: BlueprintSpec { schema_version: 0, adopted: String::new(), nodes: Vec::new() },
        });
        let all: Vec<(RepoSpec, bool)> = seeded.map(|r| (r, false)).chain(local.map(|r| (r, true))).collect();
        for (r, local) in all {
            if r.name.is_empty() || !r.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
                bail!("repo name `{}` is not [a-z0-9-]+ (it is the inside of a [repo:…] tag)", r.name);
            }
            if out.repos.iter().any(|x| x.name == r.name) {
                bail!("repo `{}` is registered twice", r.name);
            }
            for n in &r.blueprint.nodes {
                if !ids.insert(n.id.clone()) {
                    bail!("blueprint id `{}` is used twice in the cauldron", n.id);
                }
            }
            let mut nodes = r.blueprint.nodes;
            let held: std::collections::HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
            for n in &mut nodes {
                let (repo, id) = (&r.name, n.id.clone());
                let stale = &mut out.stale_refs;
                n.edges.retain(|e| {
                    let ok = held.contains(&e.to);
                    if !ok {
                        stale.push(format!("repo `{repo}`: `{id}` points at `{}`, which is not in its blueprint", e.to));
                    }
                    ok
                });
                if n.part_of.as_ref().is_some_and(|p| !held.contains(p)) {
                    stale.push(format!("repo `{repo}`: `{id}` points at `{}`, which is not in its blueprint", n.part_of.take().unwrap_or_default()));
                }
            }
            let repo = Repo {
                name: r.name,
                path: r.path,
                branch: r.branch,
                head: r.head,
                worktrees: r.worktrees,
                schema_version: (!local).then_some(r.blueprint.schema_version),
                adopted: r.blueprint.adopted,
                nodes,
                registered_at: r.registered_at,
                local,
            };
            out.repos.push(repo);
        }
        out.claimants = vec![Vec::new(); out.repos.len()];
        out.claims = vec![Vec::new(); forest.clusters.len()];
        for (gi, g) in forest.clusters.iter().enumerate() {
            for c in &g.repos {
                let Some(ri) = out.repos.iter().position(|r| r.name == c.name) else {
                    out.stale_links.push(format!("cluster `{}` claims repo `{}`, which is not registered", g.key, c.name));
                    continue;
                };
                if out.claims[gi].iter().any(|x| x.repo == ri) {
                    bail!("cluster `{}` claims repo `{}` twice", g.key, c.name);
                }
                let mut focus = Vec::new();
                for f in &c.focus {
                    match out.repos[ri].find(f) {
                        Some(n) => focus.push(n),
                        None => out.stale_links.push(format!("cluster `{}` focuses `{f}`, which is not in repo `{}`", g.key, c.name)),
                    }
                }
                out.claims[gi].push(Claim { repo: ri, focus });
                out.claimants[ri].push(gi);
            }
        }
        for g in &forest.clusters {
            for e in g.tasks.iter().flat_map(|t| t.edges.iter()).chain(g.milestones.iter().flat_map(|m| m.edges.iter())) {
                if out.locate(&e.to).is_none() {
                    out.stale_links.push(format!("cluster `{}` has a `{}` edge to `{}`, which is in no registered blueprint", g.key, e.kind, e.to));
                }
            }
        }
        Ok(out)
    }

    /// The repo and node a blueprint id names.
    pub fn locate(&self, id: &str) -> Option<(RepoId, usize)> {
        self.repos.iter().enumerate().find_map(|(r, repo)| repo.find(id).map(|n| (r, n)))
    }

    pub fn find_repo(&self, name: &str) -> Option<RepoId> {
        self.repos.iter().position(|r| r.name == name)
    }

    /// Repos no cluster claims.
    pub fn unclaimed(&self) -> Vec<RepoId> {
        (0..self.repos.len()).filter(|&r| self.claimants[r].is_empty()).collect()
    }

    /// The repos on a page whose scope is `scope`: each once, with the clusters
    /// it came through and the union of their focus.
    pub fn cluster_repos(&self, scope: &[ClusterId]) -> Vec<ClusterRepo> {
        let mut out: Vec<ClusterRepo> = Vec::new();
        for &g in scope {
            for c in self.claims.get(g).map(Vec::as_slice).unwrap_or(&[]) {
                match out.iter_mut().find(|x| x.repo == c.repo) {
                    Some(x) => {
                        x.via.push(g);
                        for &f in &c.focus {
                            if !x.focus.contains(&f) {
                                x.focus.push(f);
                            }
                        }
                    }
                    None => out.push(ClusterRepo { repo: c.repo, via: vec![g], focus: c.focus.clone() }),
                }
            }
        }
        out
    }

    /// Whether a claim needs him: an open violation or contradiction touches
    /// its focus (or, with no focus, anything in the repo).
    pub fn alarming(&self, gr: &ClusterRepo) -> bool {
        let repo = &self.repos[gr.repo];
        let hot = |n: usize| {
            repo.nags_touching(n)
                .iter()
                .any(|&s| matches!(repo.nodes[s].kind.as_str(), "violation" | "contradiction"))
        };
        if gr.focus.is_empty() {
            (0..repo.nodes.len()).any(|i| repo.nodes[i].state == "open" && matches!(repo.nodes[i].kind.as_str(), "violation" | "contradiction"))
        } else {
            gr.focus.iter().any(|&f| hot(f))
        }
    }
}

/// Who is working on a blueprint entry: a cluster's milestone or task whose
/// `builds` (or `clears`, or `depends_on`) edge names it — the back-edge trace,
/// computed by scanning, never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingOn {
    pub cluster: ClusterId,
    /// `M-2` / `T-4`.
    pub item: String,
    pub state: String,
    pub since: Option<String>,
    /// The running ripple carrying it out, if one is.
    pub ripple: Option<String>,
}

pub fn working_on(forest: &Forest, id: &str) -> Vec<WorkingOn> {
    let mut out = Vec::new();
    for (gi, g) in forest.clusters.iter().enumerate() {
        for m in &g.milestones {
            if m.edges.iter().any(|e| e.to == id) {
                let running = m
                    .tasks
                    .iter()
                    .filter_map(|&t| g.tasks.iter().find(|x| x.serial == t))
                    .find(|t| t.state == crate::cauldron::model::TaskState::Active)
                    .map(|t| t.written_id());
                let state = match g.milestone_state(m) {
                    crate::cauldron::model::MsState::Done => "done",
                    crate::cauldron::model::MsState::Active => "active",
                    crate::cauldron::model::MsState::Pending => "pending",
                    crate::cauldron::model::MsState::Blocked => "blocked",
                };
                out.push(WorkingOn { cluster: gi, item: m.written_id(), state: state.into(), since: m.since.clone(), ripple: running });
            }
        }
        for t in &g.tasks {
            if t.edges.iter().any(|e| e.to == id) {
                let ripple = (t.state == crate::cauldron::model::TaskState::Active).then(|| t.written_id());
                out.push(WorkingOn { cluster: gi, item: t.written_id(), state: t.state.label().into(), since: t.since.clone(), ripple });
            }
        }
    }
    out
}
