//! **The soup's corpus**: the regions and nodes of the cauldron's whole
//! knowledge corpus, and the MANY-TO-MANY map between regions and clusters.
//!
//! The shapes are the v5 workspace protocol's (`core:workspaces-v5`). An
//! environment has ONE workspace — this corpus — and it carries a
//! `schema_version`; a REGION is a topic with a `handle`
//! and a `boundary`, nested by a `narrows` DAG (multi-parent, so regions
//! overlap); a NODE attests one or more regions with `in_region` edges, and a
//! region holds no member list — its members are JOINED from the nodes. A
//! region may have been made under an older schema, and says so.
//!
//! The soup is ONE soup per cauldron: every region and node id is unique in it,
//! so `[region:R-3]` and `[node:IS-12]` name one thing.
//!
//! A cluster CLAIMS a region by listing it (`"regions": [...]` on the cluster in the
//! seed). Any cluster may claim any region, siblings included, and a region may be
//! claimed by many clusters. A region no cluster claims is an ORPHAN: no cluster's
//! Soup tab reaches it, so Config lists it. Claims are
//! stored once, on the cluster; every reverse view (who claims this region) is
//! computed here and never stored.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use serde::Deserialize;

use crate::cauldron::model::{Forest, ClusterId};

// ── the data file ───────────────────────────────────────────────────────────

/// The workspace schema a corpus is on when its seed does not say.
pub const CURRENT_SCHEMA: u32 = 5;

fn current_schema() -> u32 {
    CURRENT_SCHEMA
}

/// The environment's one workspace.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusSpec {
    #[serde(default = "current_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub regions: Vec<RegionSpec>,
    #[serde(default)]
    pub nodes: Vec<NodeSpec>,
}

impl Default for CorpusSpec {
    fn default() -> Self {
        CorpusSpec { schema_version: CURRENT_SCHEMA, regions: Vec::new(), nodes: Vec::new() }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionSpec {
    pub id: String,
    pub handle: String,
    pub boundary: String,
    /// The schema the region was made under; absent means the corpus's.
    #[serde(default)]
    pub schema_version: Option<u32>,
    /// The regions this one narrows (its parents in the `narrows` DAG).
    #[serde(default)]
    pub narrows: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSpec {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub handle: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub body: String,
    /// The regions this node attests. A v5 node attests at least one.
    pub in_region: Vec<String>,
    /// The operator message it cites (`thread#msg`).
    #[serde(default)]
    pub cite: Option<String>,
}

// ── the arena ───────────────────────────────────────────────────────────────

pub type RegionId = usize;
pub type NodeId = usize;

#[derive(Debug)]
pub struct Region {
    pub id: String,
    pub handle: String,
    pub boundary: String,
    pub schema_version: u32,
    pub narrows: Vec<RegionId>,
}

impl Region {
    /// Its handle as a band shows it: in NORMAL case (8e4fca10 msg 403, Q11
    /// "handle it is"). A region's handle leads with its name in capitals —
    /// `PLATFORM DEPLOY - the signed service…` — which reads loud beside
    /// every other band; the band shows `Platform deploy - the signed
    /// service…`. The data is untouched (search still reads it as written).
    pub fn shown_handle(&self) -> String {
        normal_case(&self.handle)
    }
}

/// Acronyms a capitalised name keeps in capitals.
const ACRONYMS: [&str; 14] = ["AI", "AUI", "API", "CLI", "DAG", "GC", "HTTP", "LLM", "MCP", "STT", "TTS", "TUI", "UI", "URL"];

/// `handle` with its capitalised lead — the words before its first ` - `, or
/// the whole of it when it has none — in normal case: each all-capitals word
/// lower case (an acronym kept), then its first letter capital. A lead with
/// a lower-case word in it (`THE v5 REGIONS PROTOCOL`) is still a lead: only
/// its all-capitals words change. A handle that does not lead in capitals is
/// returned as written.
pub fn normal_case(handle: &str) -> String {
    let (head, tail) = match handle.split_once(" - ") {
        Some((h, t)) => (h, Some(t)),
        None => (handle, None),
    };
    let caps = |w: &str| w.chars().any(char::is_alphabetic) && !w.chars().any(char::is_lowercase);
    if !head.split_whitespace().next().is_some_and(caps) {
        return handle.to_string();
    }
    let words: Vec<String> = head
        .split(' ')
        .map(|w| if caps(w) && !ACRONYMS.contains(&w.trim_matches(|c: char| !c.is_alphanumeric())) { w.to_lowercase() } else { w.to_string() })
        .collect();
    let mut out = words.join(" ");
    if let Some(f) = out.chars().next() {
        out = f.to_uppercase().chain(out.chars().skip(1)).collect();
    }
    match tail {
        Some(t) => format!("{out} - {t}"),
        None => out,
    }
}

#[cfg(test)]
mod normal_case_tests {
    use super::normal_case;

    #[test]
    fn the_capitalised_lead_reads_in_normal_case() {
        assert_eq!(normal_case("PLATFORM DEPLOY - the signed service"), "Platform deploy - the signed service");
        assert_eq!(normal_case("THE v5 REGIONS PROTOCOL - regions, attestation"), "The v5 regions protocol - regions, attestation");
        assert_eq!(normal_case("THE DUMP-ANYTHING LINE - one input"), "The dump-anything line - one input");
        assert_eq!(normal_case("THE TUI FACE - its tabs"), "The TUI face - its tabs");
        assert_eq!(normal_case("RIPPLES"), "Ripples");
        // A tail keeps its capitals; a handle that does not lead in capitals is as written.
        assert_eq!(normal_case("LANDING - the PUBLIC page"), "Landing - the PUBLIC page");
        assert_eq!(normal_case("Mesh devices - machines"), "Mesh devices - machines");
        assert_eq!(normal_case(""), "");
    }
}

#[derive(Debug)]
pub struct Node {
    pub id: String,
    pub kind: String,
    pub handle: String,
    pub state: String,
    pub body: String,
    pub in_region: Vec<RegionId>,
    pub cite: Option<String>,
}

#[derive(Debug, Default)]
pub struct Corpus {
    /// The workspace schema the environment runs.
    pub schema_version: u32,
    pub regions: Vec<Region>,
    pub nodes: Vec<Node>,
    /// Per region, the clusters that claim it, in tree order.
    pub claimants: Vec<Vec<ClusterId>>,
    /// Per cluster, the regions it claims, in the order it lists them.
    pub claims: Vec<Vec<RegionId>>,
    /// Per region, its direct members, in seed order.
    pub members: Vec<Vec<NodeId>>,
    /// REL-3.S2d: every reference to a region the soup does not hold (a
    /// region narrowing it, a node attesting it, a cluster claiming it), one
    /// line each. Skipped in the view, counted in the face's stale notice,
    /// never a reason not to open; nothing is written.
    pub stale: Vec<String>,
}

/// One region on a cluster's Regions page, with the clusters in the page's scope it
/// came from (the cluster itself, or the sub-clusters that claim it when
/// aggregating).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterRegion {
    pub region: RegionId,
    pub via: Vec<ClusterId>,
}

/// One search hit: a region, and the member nodes that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub region: RegionId,
    pub nodes: Vec<NodeId>,
}

fn is_id(s: &str) -> bool {
    // The id is the inside of a tag (`[region:R-3]`), so it obeys the tag's
    // name grammar.
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl Corpus {
    /// Load the corpus and resolve every cluster's claims against it. Every id
    /// must be unique across the whole soup and every reference must land.
    pub fn build(spec: CorpusSpec, forest: &Forest) -> Result<Self> {
        let mut c = Corpus { schema_version: spec.schema_version, ..Corpus::default() };
        let mut region_ix: HashMap<String, RegionId> = HashMap::new();
        let mut node_ids: HashSet<String> = HashSet::new();
        // Regions first, so a node may attest a region listed after it.
        let mut pending_narrows = Vec::new();
        for r in spec.regions {
            if !is_id(&r.id) {
                bail!("region id `{}` is not [A-Za-z0-9-_.]+", r.id);
            }
            if region_ix.insert(r.id.clone(), c.regions.len()).is_some() {
                bail!("region id `{}` is used twice in the soup", r.id);
            }
            if r.schema_version.is_some_and(|v| v > spec.schema_version) {
                bail!("region `{}` claims a schema newer than its workspace's v{}", r.id, spec.schema_version);
            }
            pending_narrows.push(r.narrows);
            c.regions.push(Region {
                id: r.id,
                handle: r.handle,
                boundary: r.boundary,
                schema_version: r.schema_version.unwrap_or(spec.schema_version),
                narrows: Vec::new(),
            });
        }
        // REL-3.S2d: a region that is not in the soup is skipped where it is
        // named and counted (`stale`), never a reason not to open.
        let mut stale = Vec::new();
        let mut find = |id: &str, what: &str, from: &str| -> Option<RegionId> {
            let got = region_ix.get(id).copied();
            if got.is_none() {
                stale.push(format!("{what} `{from}` names region `{id}`, which is not in the soup"));
            }
            got
        };
        for (ri, parents) in pending_narrows.into_iter().enumerate() {
            let from = c.regions[ri].id.clone();
            for p in parents {
                let Some(pi) = find(&p, "region", &from) else { continue };
                if pi == ri {
                    bail!("region `{from}` narrows itself");
                }
                c.regions[ri].narrows.push(pi);
            }
        }
        if let Some(r) = c.narrows_cycle() {
            bail!("the narrows DAG has a cycle through region `{}`", c.regions[r].id);
        }
        c.members = vec![Vec::new(); c.regions.len()];
        for n in spec.nodes {
            if !is_id(&n.id) {
                bail!("node id `{}` is not [A-Za-z0-9-_.]+", n.id);
            }
            if !node_ids.insert(n.id.clone()) {
                bail!("node id `{}` is used twice in the soup", n.id);
            }
            if n.in_region.is_empty() {
                bail!("node `{}` attests no region (every node attests at least one)", n.id);
            }
            let ni = c.nodes.len();
            let mut regions = Vec::new();
            for r in &n.in_region {
                let Some(ri) = find(r, "node", &n.id) else { continue };
                if !regions.contains(&ri) {
                    regions.push(ri);
                    c.members[ri].push(ni);
                }
            }
            c.nodes.push(Node {
                id: n.id,
                kind: n.kind,
                handle: n.handle,
                state: n.state,
                body: n.body,
                in_region: regions,
                cite: n.cite,
            });
        }
        c.claimants = vec![Vec::new(); c.regions.len()];
        c.claims = vec![Vec::new(); forest.clusters.len()];
        for (gi, g) in forest.clusters.iter().enumerate() {
            for r in &g.claims {
                let Some(ri) = find(r, "cluster", &g.key) else { continue };
                if c.claims[gi].contains(&ri) {
                    bail!("cluster `{}` claims region `{r}` twice", g.key);
                }
                c.claims[gi].push(ri);
                c.claimants[ri].push(gi);
            }
        }
        c.stale = stale;
        Ok(c)
    }

    fn narrows_cycle(&self) -> Option<RegionId> {
        // 0 unvisited, 1 on the stack, 2 done.
        fn visit(c: &Corpus, r: RegionId, mark: &mut [u8]) -> Option<RegionId> {
            match mark[r] {
                1 => return Some(r),
                2 => return None,
                _ => {}
            }
            mark[r] = 1;
            for &p in &c.regions[r].narrows {
                if let Some(x) = visit(c, p, mark) {
                    return Some(x);
                }
            }
            mark[r] = 2;
            None
        }
        let mut mark = vec![0u8; self.regions.len()];
        (0..self.regions.len()).find_map(|r| visit(self, r, &mut mark))
    }

    pub fn find_region(&self, id: &str) -> Option<RegionId> {
        self.regions.iter().position(|r| r.id == id)
    }

    pub fn find_node(&self, id: &str) -> Option<NodeId> {
        self.nodes.iter().position(|n| n.id == id)
    }

    /// The regions that narrow `r` (its children in the DAG).
    pub fn narrower(&self, r: RegionId) -> Vec<RegionId> {
        (0..self.regions.len()).filter(|&x| self.regions[x].narrows.contains(&r)).collect()
    }

    /// Regions no cluster claims: undiscoverable from the Cluster tab.
    pub fn orphans(&self) -> Vec<RegionId> {
        (0..self.regions.len()).filter(|&r| self.claimants[r].is_empty()).collect()
    }

    /// The regions on cluster `id`'s page: its own claims, or — aggregating — the
    /// claims of every cluster in `scope` (its subtree), each listed ONCE with
    /// every cluster in the scope that claims it.
    pub fn cluster_regions(&self, scope: &[ClusterId]) -> Vec<ClusterRegion> {
        let mut out: Vec<ClusterRegion> = Vec::new();
        for &g in scope {
            for &r in self.claims.get(g).map(Vec::as_slice).unwrap_or(&[]) {
                match out.iter_mut().find(|x| x.region == r) {
                    Some(x) => x.via.push(g),
                    None => out.push(ClusterRegion { region: r, via: vec![g] }),
                }
            }
        }
        out
    }

    /// Search the soup. A region is a hit when its id, handle or boundary
    /// contains `query` (any case) or when any of its members does (id, type,
    /// handle, state or body); the hit lists the members that matched. Hits
    /// are in seed order. An empty query matches every region and no node.
    pub fn search(&self, query: &str) -> Vec<Hit> {
        let q = query.trim().to_lowercase();
        let has = |s: &str| s.to_lowercase().contains(&q);
        (0..self.regions.len())
            .filter_map(|r| {
                if q.is_empty() {
                    return Some(Hit { region: r, nodes: Vec::new() });
                }
                let reg = &self.regions[r];
                let nodes: Vec<NodeId> = self.members[r]
                    .iter()
                    .copied()
                    .filter(|&n| {
                        let n = &self.nodes[n];
                        has(&n.id) || has(&n.kind) || has(&n.handle) || has(&n.state) || has(&n.body)
                    })
                    .collect();
                let own = has(&reg.id) || has(&reg.handle) || has(&reg.boundary);
                (own || !nodes.is_empty()).then_some(Hit { region: r, nodes })
            })
            .collect()
    }
}
