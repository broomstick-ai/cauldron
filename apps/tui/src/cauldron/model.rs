//! The cluster model: the data file's shape, the arena it is loaded into, and the
//! rollups computed over it. **No status is stored** — every status on a page is
//! derived here from the tasks and questions underneath it.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{bail, Context};
use serde::Deserialize;

// ── the data file ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DataFile {
    /// The cluster tree. (Read under one key only: the pre-rename `globs`
    /// alias is retired, as is the prod seed's old name.)
    pub clusters: Vec<ClusterSpec>,
    /// The fake dashboards behind the Status and Config tabs. Absent means an empty cauldron, not an error.
    #[serde(default)]
    pub cauldron: crate::cauldron::dash::Dash,
    /// The soup's regions and nodes: the environment's one workspace. Absent
    /// means a soup with no regions yet.
    #[serde(default)]
    pub corpus: crate::cauldron::corpus::CorpusSpec,
    /// His messages, by `thread#msg`, that a trace quotes. (In the real
    /// wiring this is the daemon store, read with `thread-messages`.)
    #[serde(default)]
    pub messages: BTreeMap<String, Message>,
}

/// One operator message a trace quotes.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub date: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterSpec {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub questions: Vec<Question>,
    #[serde(default)]
    pub human_actions: Vec<HumanAction>,
    /// The regions this cluster CLAIMS as relevant to it — its memory in the
    /// soup. Many-to-many: any cluster may claim any region.
    #[serde(default)]
    pub regions: Vec<String>,
    /// The repos this cluster CLAIMS — the code it builds against — each with
    /// the blueprint entries it is focused on. Many-to-many, as regions are.
    #[serde(default)]
    pub repos: Vec<RepoClaim>,
    /// His directives that bind this cluster (and everything below it).
    #[serde(default)]
    pub directives: Vec<Directive>,
    /// His constraints on it: what it may not do, in his words.
    #[serde(default)]
    pub constraints: Vec<Constraint>,
    /// The agent-set end-states that decompose a directive in this cluster.
    #[serde(default)]
    pub objectives: Vec<Objective>,
    /// The increments of an objective, each with a done-condition.
    #[serde(default)]
    pub milestones: Vec<Milestone>,
    /// Questions already answered here.
    #[serde(default)]
    pub closed_questions: Vec<ClosedQuestion>,
    /// The ripples that moved through this cluster that are NOT task runs:
    /// dump, curation, dream, voice.
    #[serde(default)]
    pub ripples: Vec<Background>,
    /// Questions and tasks of OTHER clusters linked here (by the seeding
    /// ripple's `link_node`), by soup id (`Q-7`): shown on this cluster,
    /// owned by theirs.
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub children: Vec<ClusterSpec>,
    /// Made in this face (`store::CreatedCluster`), not by the seed.
    #[serde(skip)]
    pub made_here: bool,
    /// The WHOLE CAULDRON ([`ClusterSpec::whole`]): made by the face, never
    /// by a seed and never stored.
    #[serde(skip)]
    pub whole: bool,
}

/// A cluster's claim on a repo: the repo, and the blueprint entries it is about.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoClaim {
    pub name: String,
    #[serde(default)]
    pub focus: Vec<String>,
}

/// His sentence, verbatim, with the message it came from. Its id is a soup
/// id (`DIR-1`), unique across the cauldron.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Directive {
    pub id: String,
    pub text: String,
    /// `thread#msg`.
    pub cite: String,
    #[serde(default)]
    pub region: Option<String>,
}

/// One of his constraints: a limit he set on the cluster, verbatim, with
/// the message it came from. Operator-set, as a directive is (msg 165:
/// "operator-derived fields like directives and constraints").
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Constraint {
    pub id: String,
    pub text: String,
    /// `thread#msg`.
    #[serde(default)]
    pub cite: Option<String>,
}

/// What done looks like, in this cluster, for one directive.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Objective {
    pub serial: u32,
    pub title: String,
    /// The directive it serves: its own cluster's or an ancestor's.
    pub serves: String,
}

impl Objective {
    pub fn written_id(&self) -> String {
        format!("OBJ-{}", self.serial)
    }
}

/// An edge from a milestone or task into a blueprint: `builds` (the entries
/// the work derives from), `depends_on` (a migration it waits on), `clears`
/// (the drag it is meant to retire).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub kind: String,
    pub to: String,
}

/// One increment of an objective. Its STATE is never stored: it is computed
/// from the tasks it holds ([`Cluster::milestone_state`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Milestone {
    pub serial: u32,
    /// The done-condition.
    pub handle: String,
    /// The objective it serves (its serial, in this cluster).
    pub objective: u32,
    /// What it needs first: `M-<n>` or `T-<n>` in this cluster.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// The tasks it holds (it depends on them; they are listed under it).
    #[serde(default)]
    pub tasks: Vec<u32>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    /// The date the work on it began.
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

impl Milestone {
    pub fn written_id(&self) -> String {
        format!("M-{}", self.serial)
    }
}

/// A milestone's computed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsState {
    Done,
    Active,
    Pending,
    Blocked,
}

impl MsState {
    pub fn glyph(self) -> &'static str {
        match self {
            MsState::Done => "✓",
            MsState::Active => "◆",
            MsState::Pending => "◇",
            MsState::Blocked => "■",
        }
    }
}

/// Where a question that is no longer open went (msg 165: "complete,
/// active, archived, superseded").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum QStatus {
    /// Answered.
    #[default]
    Complete,
    /// Replaced by another question, or made moot.
    Superseded,
    /// Put away unanswered.
    Archived,
    /// Answered by an ASSUMPTION that stands until he says otherwise
    /// (8e4fca10 msg 186: "we want to support assumed questions, assumed
    /// answers to questions"): its band is the second, after Active.
    Assumed,
}

/// A question that is no longer open: answered, superseded or archived.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedQuestion {
    pub serial: u32,
    pub question: String,
    #[serde(default)]
    pub status: QStatus,
    /// His answer (complete), or why it went (superseded, archived).
    pub answer: String,
    /// The decision the answer became, or what replaced it.
    pub resolved_by: String,
    pub date: String,
}

/// A ripple that is not a task run.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Background {
    /// dump | curation | dream | voice
    pub kind: String,
    pub line: String,
    pub when: String,
    /// done | running | parked | failed
    pub state: String,
    /// The message a dump run took in.
    #[serde(default)]
    pub cite: Option<String>,
    /// The run's id (→ opens it in the bus modal).
    #[serde(default)]
    pub run: Option<String>,
}

impl ClusterSpec {
    /// A cluster with a name and nothing in it yet.
    pub fn bare(id: &str, name: &str) -> Self {
        ClusterSpec {
            id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            tasks: Vec::new(),
            questions: Vec::new(),
            human_actions: Vec::new(),
            regions: Vec::new(),
            repos: Vec::new(),
            directives: Vec::new(),
            constraints: Vec::new(),
            objectives: Vec::new(),
            milestones: Vec::new(),
            closed_questions: Vec::new(),
            ripples: Vec::new(),
            links: Vec::new(),
            children: Vec::new(),
            made_here: false,
            whole: false,
        }
    }

    /// THE WHOLE CAULDRON as a cluster (b964d571 msg 23: "If we do, then
    /// there should be a full cauldron root cluster appear as the parent of
    /// all of the other clusters. it's a convenience to be able to see the
    /// questions present in the whole soup"): `specs` under one root that
    /// holds nothing of its own, named for the cauldron. Its id, `X-0`, is
    /// one no cluster is ever minted (`store::mint_id` starts at 1).
    pub fn whole(env: &str, specs: Vec<ClusterSpec>) -> Self {
        ClusterSpec { children: specs, whole: true, ..ClusterSpec::bare(WHOLE_ID, &format!("{env} cauldron")) }
    }

    /// The cluster `key` in `specs` or below them.
    fn find_mut<'a>(specs: &'a mut [ClusterSpec], key: &str) -> Option<&'a mut ClusterSpec> {
        for s in specs {
            if s.id == key {
                return Some(s);
            }
            if let Some(x) = ClusterSpec::find_mut(&mut s.children, key) {
                return Some(x);
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Ready,
    Active,
    Done,
    Blocked,
}

impl TaskState {
    pub fn label(self) -> &'static str {
        match self {
            TaskState::Ready => "ready",
            TaskState::Active => "active",
            TaskState::Done => "done",
            TaskState::Blocked => "blocked",
        }
    }
    /// One glyph per state, for the DAG.
    pub fn glyph(self) -> &'static str {
        match self {
            TaskState::Ready => "○",
            TaskState::Active => "▶",
            TaskState::Done => "✓",
            TaskState::Blocked => "■",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// The permanent per-cluster serial: `T-<serial>`.
    pub serial: u32,
    pub title: String,
    pub state: TaskState,
    /// Serials of tasks IN THE SAME CLUSTER this one waits on.
    #[serde(default)]
    pub depends_on: Vec<u32>,
    /// What the task's ripple is doing right now, in a few words (fake). A
    /// running ripple without one says so; a parked one without one says what
    /// it waits on, derived from the DAG.
    #[serde(default)]
    pub doing: Option<String>,
    /// The account a running ripple is on (fake).
    #[serde(default)]
    pub account: Option<String>,
    /// How long the running ripple has been at it.
    #[serde(default)]
    pub elapsed: Option<String>,
    /// `2/3` when the run is a retry.
    #[serde(default)]
    pub retry: Option<String>,
    /// What holds it (a question, a human action, a contradiction): its
    /// state is `blocked` exactly when this is not empty.
    #[serde(default)]
    pub blocked_by: Vec<String>,
    /// The operator message it came from (`thread#msg`).
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    /// The report a done task delivered.
    #[serde(default)]
    pub report: Option<String>,
    /// The date the work on it began.
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// Tasks or milestones of OTHER clusters this one waits on, by soup id
    /// (`T-13`) — the cross-cluster dependencies the aggregate Engine draws.
    #[serde(default)]
    pub waits_on: Vec<String>,
    /// The admission gate REFUSED its run (FAKE, the threads daemon's
    /// `parked_turns` row): its cause, and the token sources it could run on.
    /// Only a ready task with nothing in its way can be parked (0142).
    #[serde(default)]
    pub parked: Option<Parked>,
    /// The worktree its run holds (FAKE: the task shell's `cut_worktree`
    /// step sets it, design 0146 §4.2): the Artifacts tab's ACTIVE band.
    #[serde(default)]
    pub worktree: Option<String>,
}

/// Why the gate refused a run, as the daemon records it (`budgets.rs`
/// ParkCause) and in the words a row shows (design 0142 §1).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Parked {
    /// threshold | budget | share | unavailable | continuity |
    /// account_disabled | disabled | scope | unknown_key | internal
    pub cause: String,
    /// The row's phrase: `weekly 80% on every account`, `back 1h 05m`.
    pub phrase: String,
    /// The token sources the run could have been placed on, at the moment of
    /// refusal (the proposed `parked_turns.eligible` column).
    #[serde(default)]
    pub eligible: Vec<String>,
    /// How long it has been parked: `41m`.
    pub since: String,
}

impl Parked {
    /// A cause that needs a fix, not a wait: drawn red.
    pub fn needs_config(&self) -> bool {
        matches!(self.cause.as_str(), "scope" | "unknown_key" | "internal")
    }
}

impl Task {
    pub fn written_id(&self) -> String {
        format!("T-{}", self.serial)
    }
}

/// Where a task's RIPPLE is: the run that carries the task out. A ripple is
/// not stored apart from its task — a task's state IS its ripple's state — so
/// the Status tabs' counts and their bands are readings of one list and
/// cannot disagree. Msg 165: "count active, count parked, count pending. Or
/// pending just means that it hasn't been actually started yet, but nothing
/// has prevented it from being started". Design 0142's one test: "if
/// nothing changes but the clock and the work ahead of it, will it start?"
/// Yes is pending; no is parked; work the GRAPH holds is neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RippleState {
    /// The task is active: its ripple is running.
    Running,
    /// The admission gate refused its run (a cause, the sources it could
    /// run on).
    Parked,
    /// Wanted, and nothing refused it: it waits its turn.
    Pending,
    /// Held in the graph — a blocker (■ Q-2), or a task it waits on not
    /// done (← T-3). Not one of the three words (0142 Q1): it is on the
    /// engine with its own glyph, and counted nowhere else.
    Waiting,
    Done,
}

/// One ripple: task `serial` of cluster `cluster`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ripple {
    pub cluster: ClusterId,
    pub serial: u32,
    pub state: RippleState,
}

/// `active · parked · pending · done` over a list of ripples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RippleCounts {
    pub active: usize,
    pub parked: usize,
    pub pending: usize,
    /// Held in the graph: none of the three words.
    pub waiting: usize,
    pub done: usize,
}

/// A ticket's status, in the human-action format's own words (SCHEMA.md
/// "Frontmatter"): open, resolved (he did it), draft (awaiting a value
/// before he can act), retired (he will never do it — a withdrawal, not a
/// completion).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HaStatus {
    Open,
    Draft,
    Resolved,
    Retired,
}

impl HaStatus {
    pub fn label(self) -> &'static str {
        match self {
            HaStatus::Open => "open",
            HaStatus::Draft => "draft",
            HaStatus::Resolved => "resolved",
            HaStatus::Retired => "retired",
        }
    }
}

/// A human-action ticket: something only he can do.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanAction {
    /// The permanent per-cluster serial: `HA-<serial>`.
    pub serial: u32,
    pub ask: String,
    pub status: HaStatus,
    pub filed: String,
    /// When it was resolved or retired.
    #[serde(default)]
    pub closed: Option<String>,
    /// What raised it: a task or a question of this cluster.
    #[serde(default)]
    pub raised_by: Option<String>,
    /// What waits on it.
    #[serde(default)]
    pub blocks: Vec<String>,
    #[serde(default)]
    pub cite: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    /// The ticket's body, in the human-action format: `##` sections are its
    /// steps, with `field`, `copy` and `open` fences in them (`tickets`).
    #[serde(default)]
    pub body: String,
}

impl HumanAction {
    pub fn written_id(&self) -> String {
        format!("HA-{}", self.serial)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    /// The permanent per-cluster serial: `Q-<serial>`, spoken "question <serial>".
    pub serial: u32,
    /// Fake ranking signal, 0-100.
    pub value: u8,
    #[serde(default)]
    pub blocking: bool,
    pub question: String,
    pub why: String,
    pub options: Vec<String>,
    pub recommendation: String,
    /// The operator message it came from.
    #[serde(default)]
    pub cite: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    /// The task or milestone the answer gates.
    #[serde(default)]
    pub gates: Option<String>,
    /// ARTIFACTS inserted in the question (msg 186: "your question card needs
    /// to have the ability to have an inserted artifact"): each an artifact's
    /// id (`ART-…`, from the soup's artifacts) or a file's path. Each is a
    /// card inside the question's card; Enter opens it in a modal, `o` hands
    /// it to `open`.
    #[serde(default)]
    pub artifacts: Vec<String>,
}

impl Question {
    pub fn written_id(&self) -> String {
        format!("Q-{}", self.serial)
    }

    /// An option's NAME (its text up to the first period) and the rest.
    pub fn option_parts(o: &str) -> (&str, &str) {
        match o.split_once(". ") {
            Some((n, rest)) => (n, rest),
            None => (o.trim_end_matches('.'), ""),
        }
    }

    /// The LEAN: the option the recommendation names (1-based), and the
    /// recommendation's argument after the name. The name is the longest
    /// option name the recommendation starts with.
    pub fn lean(&self) -> (Option<usize>, String) {
        let mut best: Option<(usize, usize)> = None;
        for (i, o) in self.options.iter().enumerate() {
            let (name, _) = Question::option_parts(o);
            if !name.is_empty() && self.recommendation.starts_with(name) && best.is_none_or(|(_, l)| name.len() > l) {
                best = Some((i + 1, name.len()));
            }
        }
        match best {
            Some((i, l)) => {
                let rest = self.recommendation[l..].trim_start_matches(['.', ',', ' ', '—', '-']).trim();
                (Some(i), rest.to_string())
            }
            None => (None, self.recommendation.clone()),
        }
    }

    /// The lean's name, as the collapsed question shows it.
    pub fn lean_name(&self) -> String {
        match self.lean() {
            (Some(i), _) => Question::option_parts(&self.options[i - 1]).0.to_string(),
            (None, r) => r.split(['.', ',']).next().unwrap_or("").trim().to_string(),
        }
    }
}

// ── the arena ───────────────────────────────────────────────────────────────

pub type ClusterId = usize;

/// The whole cauldron's id ([`ClusterSpec::whole`]).
pub const WHOLE_ID: &str = "X-0";

#[derive(Debug)]
pub struct Cluster {
    pub key: String,
    pub name: String,
    pub description: String,
    pub tasks: Vec<Task>,
    pub questions: Vec<Question>,
    pub human_actions: Vec<HumanAction>,
    /// The region ids this cluster claims, as the seed lists them; resolved by
    /// [`crate::cauldron::corpus::Corpus::build`].
    pub claims: Vec<String>,
    /// The repos it claims; resolved by [`crate::cauldron::blueprint::Repos::build`].
    pub repos: Vec<RepoClaim>,
    pub directives: Vec<Directive>,
    pub constraints: Vec<Constraint>,
    pub objectives: Vec<Objective>,
    pub milestones: Vec<Milestone>,
    pub closed_questions: Vec<ClosedQuestion>,
    pub background: Vec<Background>,
    /// Other clusters' questions and tasks linked here, by soup id; resolved
    /// (and checked) once the whole forest is loaded.
    pub links: Vec<String>,
    pub parent: Option<ClusterId>,
    pub children: Vec<ClusterId>,
    pub depth: usize,
    /// Made in this face: it may be renamed, moved and deleted here.
    pub made_here: bool,
    /// The whole cauldron, the root of every other cluster while the
    /// sidebar shows what is below ([`ClusterSpec::whole`]). It holds
    /// nothing of its own and is never renamed, moved or deleted, and
    /// nothing is ever made or moved UNDER it: under it is the root.
    pub whole: bool,
}

/// Every cluster, in DEPTH-FIRST PRE-ORDER — which is also the tree's display
/// order, so the tree pane is just this vector.
#[derive(Debug)]
pub struct Forest {
    pub clusters: Vec<Cluster>,
    /// REL-3.S2d: every reference the soup's data makes to an item that is
    /// not in it (a region, a cluster, a task, a milestone, a question, a
    /// human action, an objective, a directive), one line each. Such a
    /// reference never stops the face opening: it is skipped in the view
    /// (dropped from the in-memory copy, or left where no lookup lands) and
    /// counted in the face's one stale notice. Nothing is written: the file
    /// that holds it is kept byte for byte.
    pub stale: Vec<String>,
    /// REL-3.S2d: the clusters made here that were skipped because the
    /// cluster they were made under is not in the soup. Still on disk, so a
    /// new cluster's minted id steps past them.
    pub skipped: Vec<String>,
}

/// One environment's whole fake world: its clusters, its corpus of regions and
/// nodes, and its dashboards.
#[derive(Debug)]
pub struct Soup {
    pub forest: Forest,
    pub corpus: crate::cauldron::corpus::Corpus,
    pub dash: crate::cauldron::dash::Dash,
    pub messages: BTreeMap<String, Message>,
}

impl Soup {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Soup::load_with(path, Vec::new())
    }

    /// The seed at `path`, with `created` — clusters made in this environment
    /// — each under the cluster it was made under (after its last child), or
    /// after the roots when it was made at the root.
    pub fn load_with(path: &Path, created: Vec<(Option<String>, ClusterSpec)>) -> anyhow::Result<Self> {
        Soup::load_rooted(path, created, None)
    }

    /// [`Soup::load_with`], every cluster under the whole cauldron of
    /// `whole` (an environment's name) when it is given.
    pub fn load_rooted(path: &Path, created: Vec<(Option<String>, ClusterSpec)>, whole: Option<&str>) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read cluster data {}", path.display()))?;
        let mut file: DataFile = serde_json::from_str(&text)
            .with_context(|| format!("parse cluster data {}", path.display()))?;
        let orphans = attach(&mut file.clusters, created);
        if let Some(env) = whole {
            file.clusters = vec![ClusterSpec::whole(env, std::mem::take(&mut file.clusters))];
        }
        let mut soup = Soup::from_file(file).with_context(|| format!("cluster data {}", path.display()))?;
        soup.forest.take_orphans(orphans);
        Ok(soup)
    }

    pub fn from_file(file: DataFile) -> anyhow::Result<Self> {
        let mut forest = Forest::from_specs(file.clusters)?;
        let corpus = crate::cauldron::corpus::Corpus::build(file.corpus, &forest)?;
        // REL-3.S2d: an item citing a region the soup does not hold is shown
        // without the region (skipped in the view) and counted.
        let mut stale = Vec::new();
        for g in &mut forest.clusters {
            let key = g.key.clone();
            for r in g.directives.iter_mut().map(|d| &mut d.region)
                .chain(g.tasks.iter_mut().map(|t| &mut t.region))
                .chain(g.milestones.iter_mut().map(|m| &mut m.region))
                .chain(g.questions.iter_mut().map(|q| &mut q.region))
                .chain(g.human_actions.iter_mut().map(|h| &mut h.region))
            {
                if r.as_deref().is_some_and(|id| corpus.find_region(id).is_none()) {
                    stale.push(format!("cluster `{key}` cites region `{}`, which is not in the soup", r.take().unwrap_or_default()));
                }
            }
        }
        forest.stale.extend(stale);
        Ok(Soup { forest, corpus, dash: file.cauldron, messages: file.messages })
    }

    /// An environment with no seed: one empty cluster named after it, and an
    /// empty cauldron. Honest rather than borrowed — showing prod's fake soup
    /// under a new environment's name would say it holds work it does not.
    pub fn empty(env: &str) -> Self {
        Soup::empty_with(env, Vec::new()).expect("one cluster with no questions is valid")
    }

    /// [`Soup::empty`], with the clusters created in this environment after it.
    pub fn empty_with(env: &str, created: Vec<(Option<String>, ClusterSpec)>) -> anyhow::Result<Self> {
        Soup::empty_rooted(env, created, false)
    }

    /// [`Soup::empty_with`], under the whole cauldron when `whole`.
    pub fn empty_rooted(env: &str, created: Vec<(Option<String>, ClusterSpec)>, whole: bool) -> anyhow::Result<Self> {
        let mut specs = vec![ClusterSpec::bare("X-1", env)];
        let orphans = attach(&mut specs, created);
        if whole {
            specs = vec![ClusterSpec::whole(env, specs)];
        }
        let mut forest = Forest::from_specs(specs)?;
        forest.take_orphans(orphans);
        let corpus = crate::cauldron::corpus::Corpus::build(Default::default(), &forest)?;
        Ok(Soup { forest, corpus, dash: Default::default(), messages: BTreeMap::new() })
    }
}

/// Put each made cluster under its parent, in the order they were made. A
/// cluster MOVED under one made after it waits for its parent: the list is
/// passed over until a pass places nothing. A cluster whose parent is never
/// in the soup (REL-3.S2d: a seed cluster this environment no longer reads)
/// is never moved to the root and never stops the face opening: it is
/// skipped, kept on disk, and returned (`(its id, the parent it names)`) to
/// be counted in the face's stale notice.
fn attach(specs: &mut Vec<ClusterSpec>, created: Vec<(Option<String>, ClusterSpec)>) -> Vec<(String, String)> {
    let mut pending = created;
    while !pending.is_empty() {
        let mut waiting = Vec::new();
        let before = pending.len();
        for (parent, spec) in pending {
            match &parent {
                None => specs.push(spec),
                Some(p) => match ClusterSpec::find_mut(specs, p) {
                    Some(x) => x.children.push(spec),
                    None => waiting.push((parent, spec)),
                },
            }
        }
        if waiting.len() == before {
            return waiting.into_iter().map(|(p, spec)| (spec.id, p.unwrap_or_default())).collect();
        }
        pending = waiting;
    }
    Vec::new()
}

impl Forest {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Ok(Soup::load(path)?.forest)
    }

    /// Record the clusters [`attach`] skipped (REL-3.S2d).
    fn take_orphans(&mut self, orphans: Vec<(String, String)>) {
        for (id, parent) in orphans {
            self.stale.push(format!("cluster `{id}` was made under `{parent}`, which is not in the soup"));
            self.skipped.push(id);
        }
    }

    pub fn from_specs(specs: Vec<ClusterSpec>) -> anyhow::Result<Self> {
        let mut forest = Forest { clusters: Vec::new(), stale: Vec::new(), skipped: Vec::new() };
        let mut keys = HashSet::new();
        let mut dirs = HashSet::new();
        for spec in specs {
            forest.insert(spec, None, 0, &mut keys, &mut dirs, &[])?;
        }
        if forest.clusters.is_empty() {
            bail!("cluster data holds no clusters");
        }
        // An item's id is the SOUP's (msg 235: "There's only one soup"): no
        // two clusters hold a question, a task, a human action, a milestone
        // or an objective of the same id, so `[Q-7]` names one thing.
        let mut seen: std::collections::HashMap<String, String> = Default::default();
        for g in &forest.clusters {
            let ids = g
                .questions
                .iter()
                .map(|q| q.written_id())
                .chain(g.closed_questions.iter().map(|c| format!("Q-{}", c.serial)))
                .chain(g.tasks.iter().map(Task::written_id))
                .chain(g.human_actions.iter().map(HumanAction::written_id))
                .chain(g.milestones.iter().map(Milestone::written_id))
                .chain(g.objectives.iter().map(Objective::written_id));
            for id in ids {
                if let Some(other) = seen.insert(id.clone(), g.key.clone()) {
                    if other != g.key {
                        bail!("`{id}` is in two clusters, `{other}` and `{}`: an id is the soup's, one per cauldron", g.key);
                    }
                }
            }
        }
        // REL-3.S2d: a wait or a link naming an item no cluster holds is
        // skipped (dropped from the in-memory copy) and counted; a wait or a
        // link of the wrong KIND, or a wait inside its own cluster, is bad
        // data, not a vanished item, and is still refused.
        let mut stale = Vec::new();
        let mut gone_waits: Vec<(usize, usize, String)> = Vec::new();
        let mut gone_links: Vec<(usize, String)> = Vec::new();
        for (gi, g) in forest.clusters.iter().enumerate() {
            for (ti, t) in g.tasks.iter().enumerate() {
                for w in &t.waits_on {
                    let Some(l) = Local::parse(w).filter(|l| matches!(l, Local::T(_) | Local::M(_))) else {
                        bail!("cluster `{}` T-{} waits on `{w}`, which is not a task or a milestone", g.key, t.serial)
                    };
                    let Some(o) = forest.owner(l) else {
                        stale.push(format!("cluster `{}` T-{} waits on `{w}`, which is in no cluster", g.key, t.serial));
                        gone_waits.push((gi, ti, w.clone()));
                        continue;
                    };
                    if o == gi {
                        bail!("cluster `{}` T-{} waits on `{w}` in its own cluster: that is `depends_on`", g.key, t.serial);
                    }
                }
            }
            for l in &g.links {
                let ok = match Local::parse(l) {
                    Some(q @ Local::Q(n)) => forest.owner(q).is_some_and(|o| forest.get(o).question(n).is_some()),
                    Some(t @ Local::T(_)) => forest.owner(t).is_some(),
                    _ => bail!("cluster `{}` links `{l}`, which is not an open question or a task of the soup", g.key),
                };
                if !ok {
                    stale.push(format!("cluster `{}` links `{l}`, which is not an open question or a task of the soup", g.key));
                    gone_links.push((gi, l.clone()));
                }
            }
        }
        forest.stale.extend(stale);
        for (gi, ti, w) in gone_waits {
            forest.clusters[gi].tasks[ti].waits_on.retain(|x| *x != w);
        }
        for (gi, l) in gone_links {
            forest.clusters[gi].links.retain(|x| *x != l);
        }
        Ok(forest)
    }

    /// The cluster that holds soup item `l` (its own; a link only shows it).
    pub fn owner(&self, l: Local) -> Option<ClusterId> {
        self.clusters.iter().position(|g| match l {
            Local::Q(n) => g.question(n).is_some() || g.closed_questions.iter().any(|c| c.serial == n),
            Local::T(n) => g.tasks.iter().any(|t| t.serial == n),
            Local::Ha(n) => g.human_action(n).is_some(),
            Local::M(n) => g.milestone(n).is_some(),
            Local::Obj(n) => g.objective(n).is_some(),
        })
    }

    /// Every cross-cluster wait inside `scope`'s clusters: (waiting cluster,
    /// its task, the cluster waited on, the task or milestone there). With
    /// `both`, only those whose two ends are both in `scope`.
    pub fn waits(&self, scope: &[ClusterId], both: bool) -> Vec<(ClusterId, u32, ClusterId, Local)> {
        let mut out = Vec::new();
        for &g in scope {
            for t in &self.clusters[g].tasks {
                for w in &t.waits_on {
                    let Some(l) = Local::parse(w) else { continue };
                    let Some(o) = self.owner(l) else { continue };
                    if !both || scope.contains(&o) {
                        out.push((g, t.serial, o, l));
                    }
                }
            }
        }
        out
    }

    /// The tasks of other clusters that wait on `local` (of cluster `g`).
    pub fn waited_by(&self, _g: ClusterId, local: Local) -> Vec<(ClusterId, u32)> {
        let mut out = Vec::new();
        for (o, s) in self.clusters.iter().enumerate() {
            for t in &s.tasks {
                if t.waits_on.iter().any(|w| Local::parse(w) == Some(local)) {
                    out.push((o, t.serial));
                }
            }
        }
        out
    }

    /// The questions linked to cluster `id` from other clusters: (owner, question).
    pub fn linked_questions(&self, id: ClusterId) -> Vec<(ClusterId, &Question)> {
        self.clusters[id]
            .links
            .iter()
            .filter_map(|l| match Local::parse(l)? {
                q @ Local::Q(n) => {
                    let o = self.owner(q)?;
                    Some((o, self.clusters[o].question(n)?))
                }
                _ => None,
            })
            .collect()
    }

    /// The tasks linked to cluster `id` from other clusters: (owner, task).
    pub fn linked_tasks(&self, id: ClusterId) -> Vec<(ClusterId, &Task)> {
        self.clusters[id]
            .links
            .iter()
            .filter_map(|l| match Local::parse(l)? {
                t @ Local::T(n) => {
                    let o = self.owner(t)?;
                    Some((o, self.task(o, n)?))
                }
                _ => None,
            })
            .collect()
    }

    fn insert(
        &mut self,
        spec: ClusterSpec,
        parent: Option<ClusterId>,
        depth: usize,
        keys: &mut HashSet<String>,
        dir_ids: &mut HashSet<String>,
        inherited: &[String],
    ) -> anyhow::Result<ClusterId> {
        if !keys.insert(spec.id.clone()) {
            bail!("duplicate cluster id `{}`", spec.id);
        }
        if !crate::cauldron::tags::is_cluster_id(&spec.id) {
            // A cluster's id is MINTED (msg 235: "Don't have a slug ID ... If
            // you want to use an ID, use a minted ID"): `[cluster:X-3]`.
            bail!("cluster id `{}` is not a minted `X-<n>`", spec.id);
        }
        let mut serials = HashSet::new();
        for q in &spec.questions {
            if !serials.insert(q.serial) {
                bail!("cluster `{}` reuses question serial {}", spec.id, q.serial);
            }
        }
        let mut serials = HashSet::new();
        for t in &spec.tasks {
            if !serials.insert(t.serial) {
                bail!("cluster `{}` reuses task serial {}", spec.id, t.serial);
            }
        }
        // REL-3.S2d: a dependency on a task not in the cluster is skipped
        // and counted, never a reason not to open.
        let mut spec = spec;
        for t in &mut spec.tasks {
            let (key, serial) = (&spec.id, t.serial);
            let stale = &mut self.stale;
            t.depends_on.retain(|d| {
                let ok = serials.contains(d);
                if !ok {
                    stale.push(format!("cluster `{key}` task T-{serial} depends on T-{d}, which is not in that cluster"));
                }
                ok
            });
        }
        if dag_layers(&spec.tasks).is_none() {
            bail!("cluster `{}`: its task dependencies form a cycle", spec.id);
        }
        let mut serials = HashSet::new();
        for h in &spec.human_actions {
            if !serials.insert(h.serial) {
                bail!("cluster `{}` reuses human-action serial {}", spec.id, h.serial);
            }
        }
        validate_plan(&mut spec, dir_ids, inherited, &mut self.stale)?;
        let mut visible: Vec<String> = inherited.to_vec();
        visible.extend(spec.directives.iter().map(|d| d.id.clone()));
        let id = self.clusters.len();
        self.clusters.push(Cluster {
            key: spec.id,
            name: spec.name,
            description: spec.description,
            tasks: spec.tasks,
            questions: spec.questions,
            human_actions: spec.human_actions,
            claims: spec.regions,
            repos: spec.repos,
            directives: spec.directives,
            constraints: spec.constraints,
            objectives: spec.objectives,
            milestones: spec.milestones,
            closed_questions: spec.closed_questions,
            background: spec.ripples,
            links: spec.links,
            parent,
            children: Vec::new(),
            depth,
            made_here: spec.made_here,
            whole: spec.whole,
        });
        for child in spec.children {
            let c = self.insert(child, Some(id), depth + 1, keys, dir_ids, &visible)?;
            self.clusters[id].children.push(c);
        }
        Ok(id)
    }

    pub fn get(&self, id: ClusterId) -> &Cluster {
        &self.clusters[id]
    }

    pub fn find(&self, key: &str) -> Option<ClusterId> {
        self.clusters.iter().position(|g| g.key == key)
    }

    /// `id` and every cluster below it, pre-order. The arena is pre-order, so a
    /// subtree is the contiguous run that starts at `id` and ends before the
    /// next cluster at `id`'s depth or shallower.
    pub fn subtree(&self, id: ClusterId) -> Vec<ClusterId> {
        let depth = self.clusters[id].depth;
        let end = self.clusters[id + 1..]
            .iter()
            .position(|g| g.depth <= depth)
            .map_or(self.clusters.len(), |p| id + 1 + p);
        (id..end).collect()
    }

    /// `Cauldron / Ripples / Dreaming`.
    /// The whole cauldron's cluster, while it is shown.
    pub fn whole(&self) -> Option<ClusterId> {
        self.clusters.first().filter(|g| g.whole).map(|_| 0)
    }

    /// The key of `id`'s parent as it is STORED: none at the root, and none
    /// under the whole cauldron, which is the root shown as a cluster.
    pub fn parent_key(&self, id: ClusterId) -> Option<String> {
        self.clusters[id].parent.filter(|&p| !self.clusters[p].whole).map(|p| self.clusters[p].key.clone())
    }

    /// The depth of the sidebar's SECTIONS (⌥↑ ⌥↓): its roots — under the
    /// whole cauldron, its children.
    pub fn section_depth(&self) -> usize {
        usize::from(self.whole().is_some())
    }

    pub fn path(&self, id: ClusterId) -> String {
        let mut names = vec![self.clusters[id].name.as_str()];
        let mut cur = self.clusters[id].parent.filter(|&p| !self.clusters[p].whole);
        while let Some(p) = cur {
            names.push(self.clusters[p].name.as_str());
            cur = self.clusters[p].parent.filter(|&p| !self.clusters[p].whole);
        }
        names.reverse();
        names.join(" / ")
    }

    /// The questions a page shows, RANKED: blocking first, then by value, then
    /// by permanent id. With `aggregate`, every descendant's questions join in,
    /// each still carrying the cluster it belongs to.
    pub fn questions(&self, id: ClusterId, aggregate: bool) -> Vec<(ClusterId, &Question)> {
        let scope = if aggregate { self.subtree(id) } else { vec![id] };
        let mut out: Vec<(ClusterId, &Question)> = scope
            .into_iter()
            .flat_map(|g| self.clusters[g].questions.iter().map(move |q| (g, q)))
            .collect();
        out.sort_by(|(ga, a), (gb, b)| {
            b.blocking
                .cmp(&a.blocking)
                .then(b.value.cmp(&a.value))
                .then(ga.cmp(gb))
                .then(a.serial.cmp(&b.serial))
        });
        out
    }

    /// Every ripple of `id` — and with `aggregate`, of the clusters below it —
    /// pre-order by cluster, then in task-serial order. The cluster header,
    /// the System tab and its count all read this, so all three are the
    /// cluster's own unless aggregate is on (8e4fca10 msgs 18 and 42, F5).
    pub fn ripples(&self, id: ClusterId, aggregate: bool) -> Vec<Ripple> {
        let mut out = Vec::new();
        let scope = if aggregate { self.subtree(id) } else { vec![id] };
        for g in scope {
            let mut tasks: Vec<&Task> = self.clusters[g].tasks.iter().collect();
            tasks.sort_by_key(|t| t.serial);
            let x = &self.clusters[g];
            out.extend(tasks.into_iter().map(|t| Ripple { cluster: g, serial: t.serial, state: x.task_ripple(t) }));
        }
        out
    }

    pub fn ripple_counts(&self, id: ClusterId, aggregate: bool) -> RippleCounts {
        let mut c = RippleCounts::default();
        for r in self.ripples(id, aggregate) {
            match r.state {
                RippleState::Running => c.active += 1,
                RippleState::Parked => c.parked += 1,
                RippleState::Pending => c.pending += 1,
                RippleState::Waiting => c.waiting += 1,
                RippleState::Done => c.done += 1,
            }
        }
        c
    }

    pub fn task(&self, cluster: ClusterId, serial: u32) -> Option<&Task> {
        self.clusters[cluster].tasks.iter().find(|t| t.serial == serial)
    }

    /// The rollup over the whole subtree. Always computed, never stored.
    pub fn rollup(&self, id: ClusterId) -> Rollup {
        let mut r = Rollup::default();
        for g in self.subtree(id) {
            let cluster = &self.clusters[g];
            for q in &cluster.questions {
                if g == id {
                    r.own_questions += 1;
                }
                r.open_questions += 1;
                if q.blocking {
                    r.blocking_questions += 1;
                }
            }
            r.open_human_actions += cluster.open_actions();
            for t in &cluster.tasks {
                match t.state {
                    TaskState::Ready => r.ready += 1,
                    TaskState::Active => r.active += 1,
                    TaskState::Done => r.done += 1,
                    TaskState::Blocked => r.blocked += 1,
                }
            }
        }
        r
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Rollup {
    pub open_human_actions: usize,
    pub own_questions: usize,
    pub open_questions: usize,
    pub blocking_questions: usize,
    pub ready: usize,
    pub active: usize,
    pub done: usize,
    pub blocked: usize,
}

/// A reference inside one cluster's plan: `M-2`, `T-4`, `Q-1`, `HA-1`, `OBJ-1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Local {
    M(u32),
    T(u32),
    Q(u32),
    Ha(u32),
    Obj(u32),
}

impl Local {
    pub fn parse(s: &str) -> Option<Self> {
        let (p, n) = s.split_once('-')?;
        let n: u32 = n.parse().ok()?;
        Some(match p {
            "M" => Local::M(n),
            "T" => Local::T(n),
            "Q" => Local::Q(n),
            "HA" => Local::Ha(n),
            "OBJ" => Local::Obj(n),
            _ => return None,
        })
    }
}

/// An id as the soup writes it: `PREFIX-rest`, `[A-Za-z0-9-_.]`.
fn is_soup_id(s: &str) -> bool {
    s.contains('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The plan of one cluster must hang together: every objective serves a
/// directive it can see, every milestone an objective of its cluster, every task
/// sits under at most one milestone, every reference lands, the milestones
/// form no cycle, and a task is `blocked` exactly when something blocks it.
///
/// REL-3.S2d: a reference to an item that is not there (a directive no
/// ancestor holds, an objective, a task, a milestone, a question or a human
/// action not in the cluster) is NOT refused: it is dropped from the
/// in-memory copy, or left where every lookup of it lands on nothing, and
/// one line goes to `stale` for the face's notice. What is still refused is
/// data that is malformed rather than dangling: a bad or reused id, a cycle,
/// a self-dependency, a reference of the wrong kind, a task under two
/// milestones, the blocked invariant.
fn validate_plan(spec: &mut ClusterSpec, dir_ids: &mut HashSet<String>, inherited: &[String], stale: &mut Vec<String>) -> anyhow::Result<()> {
    let g = spec.id.clone();
    for d in &spec.directives {
        if !d.id.starts_with("DIR-") || !is_soup_id(&d.id) {
            bail!("cluster `{g}`: directive id `{}` is not DIR-<n>", d.id);
        }
        if !dir_ids.insert(d.id.clone()) {
            bail!("directive `{}` is declared twice in the soup", d.id);
        }
    }
    for c in &spec.constraints {
        if !c.id.starts_with("CON-") || !is_soup_id(&c.id) {
            bail!("cluster `{g}`: constraint id `{}` is not CON-<n>", c.id);
        }
        if !dir_ids.insert(c.id.clone()) {
            bail!("constraint `{}` is declared twice in the soup", c.id);
        }
    }
    let sees = |id: &str| inherited.iter().any(|x| x == id) || spec.directives.iter().any(|d| d.id == id);
    let mut serials = HashSet::new();
    for o in &spec.objectives {
        if !serials.insert(o.serial) {
            bail!("cluster `{g}` reuses objective serial {}", o.serial);
        }
        if !sees(&o.serves) {
            // Kept: every view finds an objective's directive by lookup, so
            // one serving no directive here is under none of them.
            stale.push(format!("cluster `{g}`: OBJ-{} serves `{}`, a directive neither it nor an ancestor holds", o.serial, o.serves));
        }
    }
    let tasks: HashSet<u32> = spec.tasks.iter().map(|t| t.serial).collect();
    let task = |n: u32| tasks.contains(&n);
    let mut serials = HashSet::new();
    let mut held = HashSet::new();
    for m in &mut spec.milestones {
        if !serials.insert(m.serial) {
            bail!("cluster `{g}` reuses milestone serial {}", m.serial);
        }
        if !spec.objectives.iter().any(|o| o.serial == m.objective) {
            // Kept: a milestone is drawn under its objective, found by lookup.
            stale.push(format!("cluster `{g}`: M-{} serves OBJ-{}, which is not in that cluster", m.serial, m.objective));
        }
        let ms = m.serial;
        let mut kept = Vec::new();
        for t in std::mem::take(&mut m.tasks) {
            if !task(t) {
                stale.push(format!("cluster `{g}`: M-{ms} holds T-{t}, which is not in that cluster"));
                continue;
            }
            if !held.insert(t) {
                bail!("cluster `{g}`: T-{t} sits under two milestones");
            }
            kept.push(t);
        }
        m.tasks = kept;
    }
    let milestones: HashSet<u32> = spec.milestones.iter().map(|m| m.serial).collect();
    let questions: HashSet<u32> = spec.questions.iter().map(|q| q.serial).collect();
    for m in &mut spec.milestones {
        let ms = m.serial;
        let mut kept = Vec::new();
        for d in std::mem::take(&mut m.depends_on) {
            let there = match Local::parse(&d) {
                Some(Local::M(n)) if n == ms => bail!("cluster `{g}`: M-{ms} depends on `{d}`, which is not a milestone, task or question of that cluster"),
                Some(Local::M(n)) => milestones.contains(&n),
                Some(Local::T(n)) => task(n),
                Some(Local::Q(n)) => questions.contains(&n),
                _ => bail!("cluster `{g}`: M-{ms} depends on `{d}`, which is not a milestone, task or question of that cluster"),
            };
            if there {
                kept.push(d);
            } else {
                stale.push(format!("cluster `{g}`: M-{ms} depends on `{d}`, which is not a milestone, task or question of that cluster"));
            }
        }
        m.depends_on = kept;
    }
    if milestone_order(&spec.milestones).is_none() {
        bail!("cluster `{g}`: its milestone dependencies form a cycle");
    }
    for t in &spec.tasks {
        if (t.state == TaskState::Blocked) != !t.blocked_by.is_empty() {
            bail!("cluster `{g}`: T-{} is `{}` but blocked by {:?} — a task is blocked exactly when something blocks it", t.serial, t.state.label(), t.blocked_by);
        }
    }
    // `Some(true)` there, `Some(false)` a local item not in the cluster
    // (skipped and counted), `None` not an id at all (refused).
    let has: HashSet<(u8, u32)> = spec.questions.iter().map(|q| (0, q.serial))
        .chain(spec.human_actions.iter().map(|h| (1, h.serial)))
        .chain(tasks.iter().map(|&t| (2, t)))
        .chain(milestones.iter().map(|&m| (3, m)))
        .chain(spec.objectives.iter().map(|o| (4, o.serial)))
        .collect();
    let local_ok = |s: &str| -> Option<bool> {
        match Local::parse(s) {
            Some(Local::Q(n)) => Some(has.contains(&(0, n))),
            Some(Local::Ha(n)) => Some(has.contains(&(1, n))),
            Some(Local::T(n)) => Some(has.contains(&(2, n))),
            Some(Local::M(n)) => Some(has.contains(&(3, n))),
            Some(Local::Obj(n)) => Some(has.contains(&(4, n))),
            // A soup or blueprint id (IS-12, CONTRA-…): checked where it lives.
            None => is_soup_id(s).then_some(true),
        }
    };
    let mut keep = |what: String, r: &str| -> anyhow::Result<bool> {
        match local_ok(r) {
            Some(true) => Ok(true),
            Some(false) => {
                stale.push(format!("cluster `{g}`: {what} names `{r}`, which is not in that cluster"));
                Ok(false)
            }
            None => bail!("cluster `{g}`: {what} names `{r}`, which is not in that cluster"),
        }
    };
    for t in &mut spec.tasks {
        let what = format!("T-{}", t.serial);
        let mut kept = Vec::new();
        for r in std::mem::take(&mut t.blocked_by) {
            if keep(what.clone(), &r)? {
                kept.push(r);
            }
        }
        t.blocked_by = kept;
    }
    for h in &mut spec.human_actions {
        let what = format!("HA-{}", h.serial);
        let mut kept = Vec::new();
        for r in std::mem::take(&mut h.blocks) {
            if keep(what.clone(), &r)? {
                kept.push(r);
            }
        }
        h.blocks = kept;
        if let Some(r) = h.raised_by.take() {
            if keep(what.clone(), &r)? {
                h.raised_by = Some(r);
            }
        }
    }
    for q in &mut spec.questions {
        if let Some(r) = q.gates.take() {
            if keep(format!("Q-{}", q.serial), &r)? {
                q.gates = Some(r);
            }
        }
    }
    let mut serials = HashSet::new();
    for c in &spec.closed_questions {
        if !serials.insert(c.serial) || spec.questions.iter().any(|q| q.serial == c.serial) {
            bail!("cluster `{g}` reuses question serial {} for a closed question", c.serial);
        }
    }
    Ok(())
}

/// The milestones' indexes in dependency order (a milestone after every
/// milestone it depends on), ties by serial. `None` on a cycle.
pub fn milestone_order(ms: &[Milestone]) -> Option<Vec<usize>> {
    let deps = |m: &Milestone| -> Vec<usize> {
        m.depends_on
            .iter()
            .filter_map(|d| match Local::parse(d) {
                Some(Local::M(n)) => ms.iter().position(|x| x.serial == n),
                _ => None,
            })
            .collect()
    };
    let mut order: Vec<usize> = (0..ms.len()).collect();
    order.sort_by_key(|&i| ms[i].serial);
    let mut out = Vec::new();
    let mut done = vec![false; ms.len()];
    while out.len() < ms.len() {
        let next = order.iter().copied().find(|&i| !done[i] && deps(&ms[i]).iter().all(|&d| done[d]))?;
        done[next] = true;
        out.push(next);
    }
    Some(out)
}

impl Cluster {
    pub fn milestone(&self, serial: u32) -> Option<&Milestone> {
        self.milestones.iter().find(|m| m.serial == serial)
    }

    pub fn objective(&self, serial: u32) -> Option<&Objective> {
        self.objectives.iter().find(|o| o.serial == serial)
    }

    pub fn question(&self, serial: u32) -> Option<&Question> {
        self.questions.iter().find(|q| q.serial == serial)
    }

    pub fn human_action(&self, serial: u32) -> Option<&HumanAction> {
        self.human_actions.iter().find(|h| h.serial == serial)
    }

    /// The milestone that holds task `serial`.
    pub fn milestone_of(&self, serial: u32) -> Option<&Milestone> {
        self.milestones.iter().find(|m| m.tasks.contains(&serial))
    }

    /// A milestone's state, from its tasks: all done → done; any running →
    /// active; any blocked → blocked; else pending.
    pub fn milestone_state(&self, m: &Milestone) -> MsState {
        let states: Vec<TaskState> =
            m.tasks.iter().filter_map(|&t| self.tasks.iter().find(|x| x.serial == t)).map(|t| t.state).collect();
        if !states.is_empty() && states.iter().all(|s| *s == TaskState::Done) {
            MsState::Done
        } else if states.contains(&TaskState::Active) {
            MsState::Active
        } else if states.contains(&TaskState::Blocked) {
            MsState::Blocked
        } else {
            MsState::Pending
        }
    }

    /// `(done, of)` tasks of a milestone.
    pub fn milestone_progress(&self, m: &Milestone) -> (usize, usize) {
        let done = m.tasks.iter().filter(|&&t| self.tasks.iter().any(|x| x.serial == t && x.state == TaskState::Done)).count();
        (done, m.tasks.len())
    }

    /// `(done, of)` milestones of an objective.
    pub fn objective_progress(&self, serial: u32) -> (usize, usize) {
        let ms: Vec<&Milestone> = self.milestones.iter().filter(|m| m.objective == serial).collect();
        (ms.iter().filter(|m| self.milestone_state(m) == MsState::Done).count(), ms.len())
    }

    /// Active human actions: open, or drafted and waiting on a value.
    pub fn open_actions(&self) -> usize {
        self.human_actions.iter().filter(|h| matches!(h.status, HaStatus::Open | HaStatus::Draft)).count()
    }

    /// Where task `t`'s ripple is: running while the task is active; parked
    /// while the admission gate refuses it; pending while it is wanted and
    /// nothing refused it; WAITING while the graph holds it (a blocker, or a
    /// task it waits on that is not done) — which is parked too only under
    /// the old reading (`designs::PARKED_IS_GATE_REFUSED_ONLY`).
    pub fn task_ripple(&self, t: &Task) -> RippleState {
        let held = || if crate::cauldron::designs::PARKED_IS_GATE_REFUSED_ONLY { RippleState::Waiting } else { RippleState::Parked };
        match t.state {
            TaskState::Active => RippleState::Running,
            TaskState::Done => RippleState::Done,
            TaskState::Blocked => held(),
            TaskState::Ready => {
                let waits = t.depends_on.iter().any(|d| self.tasks.iter().any(|o| o.serial == *d && o.state != TaskState::Done));
                if waits || !t.blocked_by.is_empty() {
                    held()
                } else if t.parked.is_some() {
                    RippleState::Parked
                } else {
                    RippleState::Pending
                }
            }
        }
    }
}

impl Forest {
    /// Directive `id` and the cluster that holds it.
    pub fn directive(&self, id: &str) -> Option<(ClusterId, &Directive)> {
        self.clusters.iter().enumerate().find_map(|(g, x)| x.directives.iter().find(|d| d.id == id).map(|d| (g, d)))
    }

    /// The directives that bind cluster `id`: its ancestors', outermost first,
    /// then its own — each with the cluster that holds it.
    pub fn binding_directives(&self, id: ClusterId) -> Vec<(ClusterId, usize)> {
        let mut chain = vec![id];
        let mut cur = self.clusters[id].parent;
        while let Some(p) = cur {
            chain.push(p);
            cur = self.clusters[p].parent;
        }
        chain.reverse();
        chain.into_iter().flat_map(|g| (0..self.clusters[g].directives.len()).map(move |i| (g, i))).collect()
    }
}

/// Tasks grouped into LAYERS: layer 0 depends on nothing, layer n on
/// something in layer n-1 at the latest. Each layer lists indexes into
/// `tasks`, in serial order. `None` when the dependencies form a cycle.
pub fn dag_layers(tasks: &[Task]) -> Option<Vec<Vec<usize>>> {
    let index = |serial: u32| tasks.iter().position(|t| t.serial == serial);
    let mut layer: Vec<Option<usize>> = vec![None; tasks.len()];
    // Longest-path layering by repeated relaxation; a pass that settles
    // nothing while something is unsettled means a cycle.
    let mut settled = 0;
    while settled < tasks.len() {
        let mut progressed = false;
        for (i, t) in tasks.iter().enumerate() {
            if layer[i].is_some() {
                continue;
            }
            let deps: Option<Vec<usize>> = t
                .depends_on
                .iter()
                .map(|d| index(*d).and_then(|j| layer[j]))
                .collect();
            if let Some(deps) = deps {
                layer[i] = Some(deps.into_iter().max().map_or(0, |m| m + 1));
                settled += 1;
                progressed = true;
            }
        }
        if !progressed {
            return None;
        }
    }
    let depth = layer.iter().flatten().max().map_or(0, |m| m + 1);
    let mut out = vec![Vec::new(); depth];
    let mut order: Vec<usize> = (0..tasks.len()).collect();
    order.sort_by_key(|&i| tasks[i].serial);
    for i in order {
        out[layer[i].expect("all settled")].push(i);
    }
    Some(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Headline {
    Blocked,
    Active,
    Waiting,
    Ready,
    Done,
    Empty,
}

impl Headline {
    pub fn label(self) -> &'static str {
        match self {
            Headline::Blocked => "BLOCKED",
            Headline::Active => "ACTIVE",
            Headline::Waiting => "WAITING ON ANSWERS",
            Headline::Ready => "READY",
            Headline::Done => "DONE",
            Headline::Empty => "EMPTY",
        }
    }
}

impl Rollup {
    pub fn tasks(&self) -> usize {
        self.ready + self.active + self.done + self.blocked
    }

    /// Tasks not yet running and not done: ready (queued) plus blocked.
    pub fn pending(&self) -> usize {
        self.ready + self.blocked
    }

    /// One word for the whole subtree, in priority order: anything blocked
    /// wins, then work in flight, then questions waiting on him, then work
    /// ready to start, then done.
    pub fn headline(&self) -> Headline {
        if self.blocking_questions > 0 || self.blocked > 0 {
            Headline::Blocked
        } else if self.active > 0 {
            Headline::Active
        } else if self.open_questions > 0 {
            Headline::Waiting
        } else if self.ready > 0 {
            Headline::Ready
        } else if self.done > 0 {
            Headline::Done
        } else {
            Headline::Empty
        }
    }
}

// ── spoken ids ──────────────────────────────────────────────────────────────

/// `41` -> `forty-one`. Serials are small by design (per cluster), but this is
/// total over `u32` so no serial renders as a placeholder.
pub fn number_words(n: u32) -> String {
    const ONES: [&str; 20] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen",
        "eighteen", "nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    fn below_thousand(n: u32) -> String {
        let (h, r) = (n / 100, n % 100);
        let rest = if r < 20 {
            ONES[r as usize].to_string()
        } else if r.is_multiple_of(10) {
            TENS[(r / 10) as usize].to_string()
        } else {
            format!("{}-{}", TENS[(r / 10) as usize], ONES[(r % 10) as usize])
        };
        match (h, r) {
            (0, _) => rest,
            (h, 0) => format!("{} hundred", ONES[h as usize]),
            (h, _) => format!("{} hundred {}", ONES[h as usize], rest),
        }
    }
    if n < 1000 {
        return below_thousand(n);
    }
    let mut parts = Vec::new();
    for (scale, name) in [(1_000_000_000, "billion"), (1_000_000, "million"), (1_000, "thousand")] {
        if !(n / scale).is_multiple_of(1000) {
            parts.push(format!("{} {name}", below_thousand(n / scale % 1000)));
        }
    }
    if !n.is_multiple_of(1000) {
        parts.push(below_thousand(n % 1000));
    }
    parts.join(" ")
}

/// "question forty-one", or "Broomstick question seven" when the question
/// belongs to a cluster other than the one being viewed.
pub fn spoken_id(q: &Question, owner: &Cluster, viewing_owner: bool) -> String {
    let base = format!("question {}", number_words(q.serial));
    if viewing_owner {
        base
    } else {
        format!("{} {base}", owner.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        assert_eq!(number_words(0), "zero");
        assert_eq!(number_words(7), "seven");
        assert_eq!(number_words(15), "fifteen");
        assert_eq!(number_words(41), "forty-one");
        assert_eq!(number_words(50), "fifty");
        assert_eq!(number_words(214), "two hundred fourteen");
        assert_eq!(number_words(300), "three hundred");
        assert_eq!(number_words(1_005), "one thousand five");
        assert_eq!(number_words(2_000_000), "two million");
    }
}
