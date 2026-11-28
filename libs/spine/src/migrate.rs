//! The migration driver — validate-then-commit, through the ONE door
//! (the door's validate mode; the proving rules).
//!
//! # What this module is, and the three things it deliberately is NOT
//!
//! It is a DRIVER: it takes an abstract iterator of [`SourceItem`]s, dry-runs
//! every one of them through the door's validate mode against the
//! DESTINATION's own pinned grammars, writes a disposition per item into a
//! [`Manifest`] while nothing is at stake, and then lands the accepted set
//! atomically under ONE attempt identifier through the same door.
//!
//! 1. **It is METHOD-INDIFFERENT**. The METHOD is REGENERATION, and
//!    the regeneration ADAPTER lives beside the driver, not inside it:
//!    [`v5`] reads a v5 regions pool and yields [`SourceItem`]s, and
//!    [`import_v5_pool`] is the entry point that feeds them here. The
//!    driver itself still cannot tell which method produced its items, and
//!    nothing in this crate knows any workspace path: the caller names
//!    the pool directory.
//! 2. **It never decides pass or fail**: an agent NEVER decides pass or
//!    fail — it produces material. Every public
//!    entry here returns MATERIAL — a manifest, a landing record, a
//!    structured refusal value. The deterministic checkers in
//!    [`crate::checkers`] are what assert.
//! 3. **It never compares old to new.** The old-equals-new assertion is
//!    BARRED by design: there is no machine verification that new
//!    matches old. There is no helper here that could be composed
//!    into one, and the source is never re-read after the run.
//!
//! # The four migration rules, each built and each flagged
//!
//! The code says so at each site.
//!
//! - **A dangling source edge imports with an explicit unresolved marker**
//!   rather than starving the accounting →
//!   [`Resolution::UnresolvedEndpoint`]. See that type's docs for the
//!   collision this ran into: the one door CANNOT land an edge whose far
//!   endpoint does not exist, so the marker lands in the manifest and the
//!   destination gets nothing. Reported as a disagreement, not absorbed.
//! - **A re-run against a populated destination REFUSES unless explicitly
//!   told otherwise** → [`RunOutcome::RefusedPopulatedDestination`], checked
//!   before any dry run, lifted only by
//!   [`MigrationPlan::allow_populated_destination`].
//! - **A lossy transform is never silent** → it is a
//!   [`NamedTransform`] recorded in the manifest, or the item is REJECTED.
//!   The adapter names its own transforms (it is the half that knows the
//!   source format); the driver records them and lets the door's grammar
//!   validation reject every loss nobody named.
//! - **Provenance is initiator AGENT, instrument the IMPORT TOOL at its
//!   version** → [`import_tool`]. Attribution is harness-supplied
//!   ([`MigrationPlan::new`] takes the initiator; nothing here self-reports).

use crate::envelope::{
    Bindings, Envelope, Initiator, Instrument, Mode, Provenance, TxHash, ENVELOPE_FORMAT_VERSION,
};
use crate::error::{Result, SpineError};
use crate::pool::QualifiedNodeId;
use crate::schema_def::PoolName;
use crate::witness::Witness;
use crate::{Cauldron, Outcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------
// the instrument: the import tool at its version
// ---------------------------------------------------------------------------

/// The import tool's instrument NAME. Deliberately NOT `spine-door`: there
/// are two instruments in this build — the statement door at its
/// version; the import tool at its version for migration freight — and an
/// audit that cannot separate migration freight from ordinary writes has lost
/// the only thing the second name was for.
pub const IMPORT_TOOL: &str = "spine-import";

/// This build's import tool at its version (the instrument is versioned
/// machinery, carried once per transaction row).
pub fn import_tool() -> Instrument {
    Instrument {
        name: IMPORT_TOOL.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

// ---------------------------------------------------------------------------
// the abstract source — the driver's ONLY window onto the corpus
// ---------------------------------------------------------------------------

/// One item of the source corpus, as the driver sees it.
///
/// THIS TYPE IS THE METHOD-INDIFFERENCE SEAM. A REPLAY
/// adapter (feeding the historical message stream through the writer with a
/// historical-import flag) and a REGENERATION adapter (reading the pool files
/// as they stand) both produce exactly this, and the driver cannot tell which
/// one it is talking to. It carries no file path, no message identifier and
/// no ordering assumption beyond the iterator's own order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "item", rename_all = "snake_case")]
pub enum SourceItem {
    Node(SourceNode),
    Edge(SourceEdge),
    /// A node the DESTINATION already holds, named so source edges can point
    /// at it (an outside agent's records sit in regions the soup already
    /// has). Never created; see [`ExistingNode`].
    Existing(ExistingNode),
}

/// A node ALREADY IN THE DESTINATION that source edges point at (the
/// published-record import, [`records`]). It is never created and never
/// changed: the dry run and the landing bind it with `MATCH (b: pool/type
/// { id: $b_id })` and pin it with `EXPECT COUNT(b) = 1`, so a node that is
/// gone by the time the statement runs refuses the whole statement instead
/// of silently dropping the edge (an empty match lands nothing).
/// The v5 adapter never yields one, so the regeneration import is unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExistingNode {
    /// The item's identity in the SOURCE, as edges name it.
    pub source_id: String,
    pub pool: PoolName,
    /// Its type AS STORED (read from the destination by the adapter): a
    /// MATCH head names its type.
    pub node_type: String,
    /// Its identifier in the destination pool.
    pub id: String,
}

/// One node to import.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceNode {
    /// The item's identity IN THE SOURCE — opaque to the driver, used only
    /// for accounting and for edge endpoint resolution. It is never the
    /// destination identifier BY ITSELF: identifiers are minted by the store
    /// (`ladle::compile::mint_node_id`) unless the item carries one in
    /// [`SourceNode::carried_id`], and then the door judges it.
    pub source_id: String,
    pub pool: PoolName,
    pub node_type: String,
    pub state: Option<String>,
    /// A SERIAL id the source already holds (`Q-65`), carried through the
    /// import-only `IMPORT` statement form:
    /// the node clause is `IMPORT (b: pool/type { id: $b_id, ... })` when
    /// this is `Some`, an ordinary `CREATE` when `None`. The door judges it —
    /// the type's prefix, a `[1-9][0-9]*` serial, absence from the pool —
    /// and admits it only from an `Agent`. It is the ONLY carrier of an id:
    /// `fields` never holds `id`.
    #[serde(default)]
    pub carried_id: Option<String>,
    pub fields: BTreeMap<String, Value>,
    /// Transforms the ADAPTER applied on the way here, each NAMED (a lossy
    /// transform is never silent).
    pub transforms: Vec<NamedTransform>,
}

/// One edge to import. Both endpoints are named by SOURCE identity; the
/// driver resolves them against the item set (which is what makes a dangling
/// endpoint detectable at all).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceEdge {
    pub source_id: String,
    /// The pool whose grammar owns this edge kind — the pool the edge is
    /// STORED in, which for a cross-pool kind is the near end's pool
    /// (the grammar's `catalogues`).
    pub pool: PoolName,
    pub kind: String,
    /// `source_id` of the near endpoint.
    pub src: String,
    /// `source_id` of the far endpoint.
    pub dst: String,
    /// The edge record's free-text NOTE (the `edges.note` column, judged by
    /// no grammar rule) — the clause's `NOTE $e` bound to a STRING. A v5
    /// edge's `admits`/`sentence`/`note`/`label` travel here.
    #[serde(default)]
    pub note: Option<String>,
    /// The edge's DECLARED fields — the clause's `NOTE $e` bound to
    /// an OBJECT, judged by the kind's field table (`shows {cite, why}`).
    /// One carrier per clause: an edge with both `note` and `fields` is an
    /// adapter defect, refused before anything is at stake.
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
    pub transforms: Vec<NamedTransform>,
}

impl SourceItem {
    pub fn source_id(&self) -> &str {
        match self {
            SourceItem::Node(n) => &n.source_id,
            SourceItem::Edge(e) => &e.source_id,
            SourceItem::Existing(x) => &x.source_id,
        }
    }

    pub fn transforms(&self) -> &[NamedTransform] {
        match self {
            SourceItem::Node(n) => &n.transforms,
            SourceItem::Edge(e) => &e.transforms,
            SourceItem::Existing(_) => &[],
        }
    }

    pub fn pool(&self) -> &PoolName {
        match self {
            SourceItem::Node(n) => &n.pool,
            SourceItem::Edge(e) => &e.pool,
            SourceItem::Existing(x) => &x.pool,
        }
    }
}

/// A transform the adapter applied, NAMED (a lossy transform is never
/// silent — it is a named transform in the manifest or a rejection). The driver never invents one and never applies one; it
/// records what the adapter declared and lets the destination grammar refuse
/// every loss nobody declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedTransform {
    /// A stable name, e.g. `"drop-undeclared-field"`.
    pub name: String,
    /// What it did, concretely, e.g. `"summary"`.
    pub detail: String,
    /// Did it lose information? A lossless rename is recorded too, but only
    /// a lossy one is what the never-silent rule is about.
    pub lossy: bool,
}

// ---------------------------------------------------------------------------
// the plan
// ---------------------------------------------------------------------------

/// What the caller fixes before a run. Provenance is HARNESS-SUPPLIED: the driver takes the initiator, never
/// invents it, and never lets the source corpus speak it.
#[derive(Debug, Clone)]
pub struct MigrationPlan {
    /// The default: **initiator AGENT** for the migration run (the
    /// migration run's provenance is initiator agent, instrument the
    /// import tool at its version). The caller may hand a
    /// `core-mechanical` initiator instead and the manifest records which was
    /// used, because a default is not a licence to refuse the other.
    pub initiator: Initiator,
    /// The import tool at its version. Defaulted to
    /// [`import_tool`]; overridable so a real adapter can name itself.
    pub instrument: Instrument,
    /// A re-run against a populated destination
    /// REFUSES unless explicitly told otherwise. This IS the "otherwise".
    pub allow_populated_destination: bool,
    /// Rides into the attempt's hash identity so two
    /// deliberate runs of the same corpus are two identities rather than one
    /// deduplicated one.
    pub idempotency_nonce: Option<String>,
}

impl MigrationPlan {
    /// A plan with the defaults: the import tool as instrument, a
    /// populated destination refused, no nonce.
    pub fn new(initiator: Initiator) -> MigrationPlan {
        MigrationPlan {
            initiator,
            instrument: import_tool(),
            allow_populated_destination: false,
            idempotency_nonce: None,
        }
    }

    fn check(&self) -> Result<()> {
        if let Initiator::External = self.initiator {
            return Err(SpineError::usage(
                "migrate: the `external` initiator is out of scope this build; the \
                 enum value exists, the import path does not accept it",
            ));
        }
        if self.instrument.name.trim().is_empty() || self.instrument.version.trim().is_empty() {
            return Err(SpineError::usage(
                "migrate: the instrument carries a name and a version",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// the manifest — the material the run produces before anything is at stake
// ---------------------------------------------------------------------------

/// The MANIFEST: one disposition per source item, written while
/// NOTHING IS AT STAKE, plus the attempt stamp once the accepted set landed.
///
/// It is the driver's primary product and the checkers' primary input. It
/// asserts nothing itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub initiator: Initiator,
    pub instrument: Instrument,
    /// How many items the source iterator yielded. Recorded SEPARATELY from
    /// `entries.len()` on purpose: total accounting is the claim
    /// that these two agree, and a checker cannot check a number it derives
    /// from the thing it is checking.
    pub source_count: usize,
    /// One entry per source item, in the order the iterator yielded them.
    pub entries: Vec<ManifestEntry>,
    /// The ONE attempt identifier the accepted set landed under,
    /// `None` until (and unless) the commit phase runs.
    pub attempt: Option<AttemptStamp>,
}

impl Manifest {
    pub fn imported(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.disposition, Disposition::Imported { .. }))
            .count()
    }

    pub fn rejected(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.disposition, Disposition::Rejected { .. }))
            .count()
    }

    /// The entries that actually go into the landing statement: imported AND
    /// completely resolved. An unresolved-endpoint import is IMPORTED for
    /// accounting and LANDS NOTHING — see [`Resolution::UnresolvedEndpoint`].
    pub fn landable(&self) -> Vec<&ManifestEntry> {
        self.entries.iter().filter(|e| e.lands()).collect()
    }

    pub fn entry(&self, source_id: &str) -> Option<&ManifestEntry> {
        self.entries.iter().find(|e| e.source_id == source_id)
    }
}

/// One item's disposition — "import-as-X or rejected-because-Y".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub source_id: String,
    pub disposition: Disposition,
}

impl ManifestEntry {
    /// Does this entry contribute to the landing statement?
    pub fn lands(&self) -> bool {
        matches!(
            &self.disposition,
            Disposition::Imported {
                resolution: Resolution::Complete,
                ..
            }
        )
    }
}

/// Import-as-X, or rejected-because-Y. Exactly two arms, so total accounting
/// (imported plus rejected-with-reason equals source count, no silent
/// drop) is a partition by construction rather than by discipline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum Disposition {
    Imported {
        pool: PoolName,
        shape: ImportShape,
        resolution: Resolution,
        transforms: Vec<NamedTransform>,
    },
    Rejected {
        reason: Rejection,
    },
}

/// What the item imports AS ("import-as-X").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "as", rename_all = "snake_case")]
pub enum ImportShape {
    Node {
        node_type: String,
    },
    Edge {
        kind: String,
        src: String,
        dst: String,
    },
    /// A reference to a node the destination already holds
    /// ([`SourceItem::Existing`]): "imported" only in the accounting sense —
    /// it is bound, never created.
    Existing {
        node_type: String,
        id: String,
    },
}

/// Whether the item's references all resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "resolution", rename_all = "snake_case")]
pub enum Resolution {
    /// Everything resolved; the item is in the landing statement.
    Complete,
    /// **The dangling-edge rule, and this build's one open
    /// collision with it.** The rule: a dangling SOURCE edge imports with an
    /// explicit unresolved marker rather than starving the accounting.
    ///
    /// The accounting half is delivered exactly: the item is IMPORTED, never
    /// dropped, and the marker names which endpoint failed to resolve and
    /// why. The DESTINATION half cannot be delivered through the one door,
    /// and this is a fact about the door, not a shortcut: a `CREATE` edge
    /// clause takes BOUND endpoints, an unresolvable endpoint binds nothing,
    /// and a clause over an empty binding lands nothing (the empty-match
    /// rule). The only door-legal way to leave a mark in the
    /// destination would be to MINT A PLACEHOLDER NODE the source never had —
    /// fabricated destination content, in a migration whose own rule says
    /// the old files are never ground truth and nothing may be invented from
    /// them.
    ///
    /// So: the marker is durable in the MANIFEST, which is material the run
    /// produces and keeps, and nothing lands. Reported as a disagreement with
    /// the rule's literal words, NOT resolved here and NOT absorbed.
    UnresolvedEndpoint {
        endpoint: EndpointSide,
        /// The source identity that did not resolve.
        source_ref: String,
        /// Why it did not resolve — one of [`MARKER_ABSENT_FROM_SOURCE`] or
        /// [`MARKER_ENDPOINT_REJECTED`].
        marker: String,
    },
}

/// Which end of an edge failed to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointSide {
    Src,
    Dst,
}

/// The endpoint names a source identity the corpus never yielded.
pub const MARKER_ABSENT_FROM_SOURCE: &str = "endpoint-absent-from-source";
/// The endpoint IS in the corpus but was itself rejected, so it will not
/// exist in the destination.
pub const MARKER_ENDPOINT_REJECTED: &str = "endpoint-rejected";

/// Rejected-because-Y, structured (prose cannot be
/// asserted on, so the reason carries a frozen CODE and, when the door
/// refused, the door's own witnesses verbatim).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rejection {
    pub code: String,
    pub detail: String,
    /// The door's structured witnesses, when the dry run refused. Empty when the driver rejected before the door saw
    /// the item.
    pub witnesses: Vec<Witness>,
}

/// The destination grammar refused the item's dry run.
pub const REJECT_GRAMMAR: &str = "refused-by-destination-grammar";
/// The item names an endpoint pool this cauldron does not have.
pub const REJECT_UNKNOWN_POOL: &str = "unknown-destination-pool";
/// An edge item whose endpoint is a source item that is itself an edge.
pub const REJECT_ENDPOINT_NOT_A_NODE: &str = "endpoint-is-not-a-node";
/// An [`SourceItem::Existing`] the destination does not hold (or holds under
/// another type).
pub const REJECT_UNKNOWN_DESTINATION_NODE: &str = "unknown-destination-node";

/// The ONE attempt identifier the accepted set landed under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptStamp {
    /// The transaction hash identity, 64 lowercase hex characters.
    pub tx: String,
    /// The spine sequence number the row was issued.
    pub seq: i64,
}

// ---------------------------------------------------------------------------
// what a run produces
// ---------------------------------------------------------------------------

/// What [`run`] produces. A VALUE in every arm — the driver decides nothing
/// (the proving rules); a refusal is material too.
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    /// A re-run against a populated
    /// destination REFUSES. Nothing was dry-run, nothing was submitted.
    ///
    /// INTERIOR CHOICE (recorded): this is a VALUE, not a
    /// [`Witness`]-carrying refusal. The witness shapes are the STATEMENT
    /// door's contract; the import tool refusing to point itself at a
    /// populated destination is not a statement refusal, and minting a
    /// witness variant for it would invent contract surface a later release
    /// inherits. It is structured, so suites assert on it structurally
    /// (the same precedent as the blank-provenance case).
    RefusedPopulatedDestination { populated: Vec<PopulatedPool> },
    /// The run happened: manifest plus what the landing did.
    Ran(MigrationRun),
}

impl RunOutcome {
    /// The run, or a loud failure — for callers that already established the
    /// destination is empty.
    pub fn expect_ran(self) -> Result<MigrationRun> {
        match self {
            RunOutcome::Ran(r) => Ok(r),
            RunOutcome::RefusedPopulatedDestination { populated } => Err(SpineError::usage(
                format!(
                    "migrate: the destination is already populated ({}); a re-run refuses unless \
                     the plan explicitly allows it",
                    populated
                        .iter()
                        .map(|p| format!("{}: {} node(s)", p.pool, p.nodes))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }
}

/// One populated destination pool, as the re-run refusal names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopulatedPool {
    pub pool: PoolName,
    pub nodes: usize,
}

/// The material one run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct MigrationRun {
    pub manifest: Manifest,
    pub landing: Landing,
}

/// What the commit phase did. Every arm is a RECORD; none is a verdict.
#[derive(Debug, Clone, PartialEq)]
pub enum Landing {
    /// Nothing was landable, so no envelope was built and no row exists. A
    /// commit of nothing is not a commit (a statement that does nothing is
    /// not a statement worth a row, whereas an empty MATCH is still a
    /// recorded request).
    NothingAccepted,
    /// The accepted set landed as ONE commit on ONE spine row.
    Committed {
        tx: TxHash,
        seq: i64,
        /// Source identity -> the identifier the store minted for it.
        landed: BTreeMap<String, QualifiedNodeId>,
        /// True when the identity had already landed and the door replayed
        /// the ORIGINAL outcome (exactly-once).
        deduplicated: bool,
    },
    /// The door refused the WHOLE landing statement with witnesses. Per-item
    /// validation does not compose to set-level validity (two items can each
    /// be legal and be illegal together — a scalar slice is the sharp case),
    /// so this arm is REAL and is reported rather than retried: nothing
    /// landed anywhere (refusal is whole).
    Refused { witnesses: Vec<Witness> },
}

impl Landing {
    pub fn attempt(&self) -> Option<(&TxHash, i64)> {
        match self {
            Landing::Committed { tx, seq, .. } => Some((tx, *seq)),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// the regeneration import
// ---------------------------------------------------------------------------

pub mod records;
pub mod v5;

/// Import one v5 REGIONS pool (`<pool_dir>/nodes/*.yaml` plus
/// `<pool_dir>/manifest.yaml`'s `viewers`) into `cauldron`, BY REGENERATION
/// [`v5::read_pool`] turns the files as they stand
/// into [`SourceItem`]s and [`run`] dry-runs and lands them through the ONE
/// door as one attempt. Every v5 node lands under its OWN id through the
/// `IMPORT` statement form; every viewer role is a store-minted `V-n` with
/// its `shows` edges; every field v6 has no home for rides the node's
/// `meta.imported_from` record; every edge's v5 text rides its `note`. The
/// destination pool is the directory's BASENAME and must already be pinned
/// to `grammar_v6::workspace_v6(<pool>)` — creating the cauldron is the
/// caller's.
///
/// It replaces `import_live_pools`, which refused until the import method
/// was chosen; the method is regeneration.
///
/// Gates, before anything is dry-run:
/// - the plan's initiator must be `Agent` (the import's initiator is the
///   agent-driven writer that holds the pool). Any other is [`SpineError::Usage`]. [`run`] itself stays
///   initiator-indifferent; this is the adapter's own gate, and the door's
///   `import_not_permitted` is the second one.
/// - with [`MigrationPlan::allow_populated_destination`], a destination that
///   already holds a `viewer` whose `role` this import would mint again is
///   [`SpineError::Usage`] naming the role (one viewer per role — a
///   workspace constraint, `grammar_v6::LEFT_OUT`; the adapter enforces it
///   for its own output). Without it, [`run`]'s populated-destination gate
///   answers first.
///
/// NOT CHECKED HERE (the SIXTH migration property): faithfulness of
/// MEANING is judgment, reported and flagged as judgment, never a gate. The
/// five mechanical checkers cannot carry it, and fixtures make it vacuous,
/// so whoever runs a live import judges it and writes that judgment down
/// beside the manifest.
pub fn import_v5_pool(
    cauldron: &Cauldron,
    plan: &MigrationPlan,
    pool_dir: &std::path::Path,
) -> Result<RunOutcome> {
    if !matches!(plan.initiator, Initiator::Agent { .. }) {
        return Err(SpineError::usage(format!(
            "migrate::import_v5_pool: the import's initiator is `Agent` (a v5 pool is \
             written only by an agent's tool, so its import is an agent's act); this plan's initiator is `{}`",
            initiator_kind(&plan.initiator)
        )));
    }
    let source = v5::read_pool(pool_dir)?;
    if plan.allow_populated_destination {
        v5::refuse_viewer_collision(cauldron, &source)?;
    }
    run(cauldron, plan, source.items)
}

fn initiator_kind(initiator: &Initiator) -> &'static str {
    match initiator {
        Initiator::Agent { .. } => "agent",
        Initiator::CoreMechanical { .. } => "core_mechanical",
        Initiator::Ripple { kind, .. } => match kind {
            crate::envelope::RippleKind::TypeOne => "type_one",
            crate::envelope::RippleKind::TypeTwo => "type_two",
            crate::envelope::RippleKind::TypeThree => "type_three",
        },
        Initiator::External => "external",
    }
}

// ---------------------------------------------------------------------------
// the run — validate-then-commit
// ---------------------------------------------------------------------------

/// Run one import: the populated-destination gate, the per-item dry run
/// through the door's VALIDATE mode, and the one-attempt landing of the
/// accepted set. Pass one of the dry run carries each node's
/// REQUIRED-AT-MINT edges (see
/// [`dry_run`]), so a node the grammar requires to mint with an edge is
/// judged as the landing will mint it.
///
/// Returns MATERIAL in every case it can. `Err` is reserved for the driver
/// being MISUSED (a broken plan, a source that names one identity twice) and
/// for physics — never for "the migration went badly", which is the
/// checkers' business and not this function's.
pub fn run(
    cauldron: &Cauldron,
    plan: &MigrationPlan,
    items: impl IntoIterator<Item = SourceItem>,
) -> Result<RunOutcome> {
    plan.check()?;
    let items: Vec<SourceItem> = items.into_iter().collect();
    check_source_identities(&items)?;

    // GATE FIRST, before anything is dry-run: a refusal is recoverable, a
    // silent replace is not.
    if !plan.allow_populated_destination {
        let populated = destination_population(cauldron)?;
        if !populated.is_empty() {
            return Ok(RunOutcome::RefusedPopulatedDestination { populated });
        }
    }

    let manifest = dry_run(cauldron, plan, &items)?;
    let (manifest, landing) = land(cauldron, plan, &items, manifest)?;
    Ok(RunOutcome::Ran(MigrationRun { manifest, landing }))
}

/// The VALIDATE half of validate-then-commit (the door's validate
/// mode — "one validator, two callers"). Every item is taken through the
/// DESTINATION grammar as a dry run and given a disposition while NOTHING IS
/// AT STAKE. Nothing lands and no spine row is written here.
///
/// Two passes, deliberately: nodes first, because an edge's disposition
/// depends on whether its endpoints were accepted, and a one-pass driver
/// would have to guess for any edge that arrived before its endpoints.
///
/// **Pass one carries each node's REQUIRED-AT-MINT edges**. A grammar can require a type to
/// mint WITH edges (`required_edges_at_mint`, the AND form, and
/// `required_at_mint_any_of`, the one-of form — `workspace_v6`: a content
/// node mints with an `in_region`, or with `rejected_by` + `searched`), and
/// the landing is ONE statement in which those edges exist. A node dry-run
/// alone would disagree with the landing it predicts. So each node is
/// dry-run TOGETHER WITH its outgoing source edges whose kind the grammar
/// names in either requirement of its type, toward endpoints ALREADY
/// ACCEPTED in this pass (each re-created with its own required closure,
/// the same way). Nodes are judged in dependency order — a node after the
/// targets of its required-kind edges, so a type with no requirement (a
/// region) goes first; a cycle is broken in source order with what is
/// accepted so far. A node whose surviving required edges fall short is
/// rejected by the grammar's own `required_edge_missing` — a correct
/// disposition. Only the REQUIRED kinds ride along: a non-required edge's
/// fault stays on that edge's own item. The requirement is read from the
/// destination grammar, never from any import method, so the driver stays
/// method-indifferent.
pub fn dry_run(
    cauldron: &Cauldron,
    plan: &MigrationPlan,
    items: &[SourceItem],
) -> Result<Manifest> {
    let mut entries: Vec<Option<ManifestEntry>> = vec![None; items.len()];
    let ix = Index::of(cauldron, items);
    let mut accepted_nodes: BTreeSet<&str> = BTreeSet::new();
    let mut judged: BTreeSet<&str> = BTreeSet::new();

    // ---- pass zero: every EXISTING item is looked up in the destination ----
    // It is accepted when the destination holds that id under that type; an
    // edge toward it is then judged like an edge toward an accepted node.
    for (i, item) in items.iter().enumerate() {
        let SourceItem::Existing(x) = item else {
            continue;
        };
        judged.insert(x.source_id.as_str());
        let held = if cauldron.grammar(&x.pool).is_none() {
            None
        } else {
            let pool = cauldron.open_pool(&x.pool)?;
            let snapshot = pool.snapshot()?;
            snapshot
                .node(&crate::pool::NodeId(x.id.clone()))?
                .map(|n| n.node_type)
        };
        entries[i] = Some(match held {
            Some(t) if t == x.node_type => {
                accepted_nodes.insert(x.source_id.as_str());
                ManifestEntry {
                    source_id: x.source_id.clone(),
                    disposition: Disposition::Imported {
                        pool: x.pool.clone(),
                        shape: ImportShape::Existing {
                            node_type: x.node_type.clone(),
                            id: x.id.clone(),
                        },
                        resolution: Resolution::Complete,
                        transforms: Vec::new(),
                    },
                }
            }
            other => reject(
                &x.source_id,
                REJECT_UNKNOWN_DESTINATION_NODE,
                match other {
                    Some(t) => format!(
                        "pool `{}` holds `{}` as a `{t}`, not a `{}`",
                        x.pool, x.id, x.node_type
                    ),
                    None => format!("pool `{}` holds no node `{}`", x.pool, x.id),
                },
                Vec::new(),
            ),
        });
    }

    // ---- pass one: every NODE item, with its required-at-mint edges ----
    let mut pending: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i, SourceItem::Node(_)))
        .map(|(n, _)| n)
        .collect();
    while !pending.is_empty() {
        let mut next: Vec<usize> = Vec::new();
        let mut progressed = false;
        for &i in &pending {
            let SourceItem::Node(node) = &items[i] else {
                unreachable!("pending holds node items only")
            };
            let ready = ix.required_out(node).iter().all(|e| {
                e.dst == node.source_id
                    || !ix.nodes.contains_key(e.dst.as_str())
                    || judged.contains(e.dst.as_str())
            });
            if !ready {
                next.push(i);
                continue;
            }
            progressed = true;
            judge_node(cauldron, plan, &ix, node, &mut accepted_nodes, &mut judged, &mut entries, i)?;
        }
        if !progressed {
            // A cycle among required-kind edges: judge the first remaining
            // node in source order with what is accepted so far.
            let i = next.remove(0);
            let SourceItem::Node(node) = &items[i] else {
                unreachable!("pending holds node items only")
            };
            judge_node(cauldron, plan, &ix, node, &mut accepted_nodes, &mut judged, &mut entries, i)?;
        }
        pending = next;
    }

    // ---- pass two: every EDGE item, with its endpoints, through validate ----
    for (i, item) in items.iter().enumerate() {
        let SourceItem::Edge(edge) = item else {
            continue;
        };
        if cauldron.grammar(&edge.pool).is_none() {
            entries[i] = Some(reject(
                &edge.source_id,
                REJECT_UNKNOWN_POOL,
                format!("this cauldron has no pool `{}`", edge.pool),
                Vec::new(),
            ));
            continue;
        }
        // An endpoint that names another EDGE is an adapter defect, not a
        // dangling reference: it resolved, to the wrong kind of thing. It is
        // rejected by its own code rather than dressed as unresolved.
        if let Some(bad) = [edge.src.as_str(), edge.dst.as_str()]
            .into_iter()
            .find(|e| !ix.is_node(e) && ix.edge_ids.contains(e))
        {
            entries[i] = Some(reject(
                &edge.source_id,
                REJECT_ENDPOINT_NOT_A_NODE,
                format!("endpoint `{bad}` names a source item that is not a node"),
                Vec::new(),
            ));
            continue;
        }
        // The dangling-edge rule, applied at BOTH ends.
        if let Some(unresolved) = unresolved_endpoint(edge, &ix, &accepted_nodes) {
            entries[i] = Some(ManifestEntry {
                source_id: edge.source_id.clone(),
                disposition: Disposition::Imported {
                    pool: edge.pool.clone(),
                    shape: edge_shape(edge),
                    resolution: unresolved,
                    transforms: edge.transforms.clone(),
                },
            });
            continue;
        }
        // The dry run re-creates both endpoints — each with its required
        // closure, exactly as pass one accepted it — so the edge kind is
        // judged against real endpoint TYPES (schema_def::validate_delta) and
        // the endpoints mint as the landing will mint them.
        let mut st = DryStatement::default();
        let a = st.node(&ix, edge.src.as_str(), &accepted_nodes);
        let b = st.node(&ix, edge.dst.as_str(), &accepted_nodes);
        if !st.edges.contains(edge.source_id.as_str()) {
            st.edge(&a, &b, edge);
        }
        let (text, params) = st.finish();
        entries[i] = Some(match validate(cauldron, plan, &text, params) {
            Ok(()) => ManifestEntry {
                source_id: edge.source_id.clone(),
                disposition: Disposition::Imported {
                    pool: edge.pool.clone(),
                    shape: edge_shape(edge),
                    resolution: Resolution::Complete,
                    transforms: edge.transforms.clone(),
                },
            },
            Err(SpineError::Refused { witnesses }) => reject(
                &edge.source_id,
                REJECT_GRAMMAR,
                format!(
                    "the destination grammar of pool `{}` refused this edge's dry run",
                    edge.pool
                ),
                witnesses,
            ),
            Err(other) => return Err(other),
        });
    }

    // Every slot is filled, or the driver dropped an item silently — which is
    // exactly what total accounting exists to catch, so it fails HERE and
    // loudly rather than producing a manifest a checker has to catch it in.
    let mut out = Vec::with_capacity(items.len());
    for (i, e) in entries.into_iter().enumerate() {
        out.push(e.ok_or_else(|| {
            SpineError::hard(format!(
                "migrate: source item {i} (`{}`) got no disposition; the driver would have \
                 dropped it silently",
                items[i].source_id()
            ))
        })?);
    }

    Ok(Manifest {
        initiator: plan.initiator.clone(),
        instrument: plan.instrument.clone(),
        source_count: items.len(),
        entries: out,
        attempt: None,
    })
}

/// Pass one's verdict on ONE node: its dry run with its required-at-mint
/// closure (see [`dry_run`]).
#[allow(clippy::too_many_arguments)]
fn judge_node<'a>(
    cauldron: &Cauldron,
    plan: &MigrationPlan,
    ix: &Index<'a>,
    node: &'a SourceNode,
    accepted: &mut BTreeSet<&'a str>,
    judged: &mut BTreeSet<&'a str>,
    entries: &mut [Option<ManifestEntry>],
    i: usize,
) -> Result<()> {
    judged.insert(node.source_id.as_str());
    if cauldron.grammar(&node.pool).is_none() {
        entries[i] = Some(reject(
            &node.source_id,
            REJECT_UNKNOWN_POOL,
            format!("this cauldron has no pool `{}`", node.pool),
            Vec::new(),
        ));
        return Ok(());
    }
    let mut st = DryStatement::default();
    st.node(ix, node.source_id.as_str(), accepted);
    let (text, params) = st.finish();
    let entry = match validate(cauldron, plan, &text, params) {
        Ok(()) => ManifestEntry {
            source_id: node.source_id.clone(),
            disposition: Disposition::Imported {
                pool: node.pool.clone(),
                shape: ImportShape::Node {
                    node_type: node.node_type.clone(),
                },
                resolution: Resolution::Complete,
                transforms: node.transforms.clone(),
            },
        },
        Err(SpineError::Refused { witnesses }) => reject(
            &node.source_id,
            REJECT_GRAMMAR,
            format!(
                "the destination grammar of pool `{}` refused this item's dry run",
                node.pool
            ),
            witnesses,
        ),
        Err(other) => return Err(other),
    };
    if entry.lands() {
        accepted.insert(node.source_id.as_str());
    }
    entries[i] = Some(entry);
    Ok(())
}

/// The corpus, indexed once for the dry run.
struct Index<'a> {
    nodes: BTreeMap<&'a str, &'a SourceNode>,
    /// The destination nodes the source names ([`SourceItem::Existing`]).
    existing: BTreeMap<&'a str, &'a ExistingNode>,
    edge_ids: BTreeSet<&'a str>,
    /// Edge items by near endpoint, in source order.
    out: BTreeMap<&'a str, Vec<&'a SourceEdge>>,
    /// (pool, node type) -> the edge kinds its grammar requires at mint, in
    /// EITHER form (AND or one-of).
    required: BTreeMap<(PoolName, String), BTreeSet<String>>,
}

impl<'a> Index<'a> {
    fn of(cauldron: &Cauldron, items: &'a [SourceItem]) -> Index<'a> {
        let mut ix = Index {
            nodes: BTreeMap::new(),
            existing: BTreeMap::new(),
            edge_ids: BTreeSet::new(),
            out: BTreeMap::new(),
            required: BTreeMap::new(),
        };
        for item in items {
            match item {
                SourceItem::Node(n) => {
                    ix.nodes.insert(n.source_id.as_str(), n);
                    let key = (n.pool.clone(), n.node_type.clone());
                    if !ix.required.contains_key(&key) {
                        let kinds = cauldron
                            .grammar(&n.pool)
                            .and_then(|g| g.node_types.get(&n.node_type))
                            .map(|t| {
                                t.required_edges_at_mint
                                    .keys()
                                    .cloned()
                                    .chain(
                                        t.required_at_mint_any_of
                                            .iter()
                                            .flat_map(|alt| alt.edges.keys().cloned()),
                                    )
                                    .collect()
                            })
                            .unwrap_or_default();
                        ix.required.insert(key, kinds);
                    }
                }
                SourceItem::Edge(e) => {
                    ix.edge_ids.insert(e.source_id.as_str());
                    ix.out.entry(e.src.as_str()).or_default().push(e);
                }
                SourceItem::Existing(x) => {
                    ix.existing.insert(x.source_id.as_str(), x);
                }
            }
        }
        ix
    }

    /// Does `id` name a node item, created or existing?
    fn is_node(&self, id: &str) -> bool {
        self.nodes.contains_key(id) || self.existing.contains_key(id)
    }

    /// The node's outgoing edge items of a kind its grammar requires at mint.
    fn required_out(&self, node: &SourceNode) -> Vec<&'a SourceEdge> {
        let Some(kinds) = self
            .required
            .get(&(node.pool.clone(), node.node_type.clone()))
        else {
            return Vec::new();
        };
        self.out
            .get(node.source_id.as_str())
            .map(|v| v.iter().copied().filter(|e| kinds.contains(&e.kind)).collect())
            .unwrap_or_default()
    }
}

/// One dry-run statement under construction: node heads bound once each,
/// with their required-at-mint closure, and edge clauses bound once each.
#[derive(Default)]
struct DryStatement<'a> {
    /// MATCH heads of existing nodes, then their EXPECT pins: the fixed
    /// clause order puts both before every mutation.
    matches: Vec<String>,
    expects: Vec<String>,
    clauses: Vec<String>,
    params: Bindings,
    bound: BTreeMap<&'a str, String>,
    edges: BTreeSet<&'a str>,
}

impl<'a> DryStatement<'a> {
    /// Bind source node `id` (once), then its required-kind outgoing edges
    /// toward ACCEPTED endpoints, each endpoint bound the same way.
    fn node(&mut self, ix: &Index<'a>, id: &'a str, accepted: &BTreeSet<&'a str>) -> String {
        if let Some(b) = self.bound.get(id) {
            return b.clone();
        }
        let binding = format!("a{}", self.bound.len());
        if let Some(x) = ix.existing.get(id) {
            let (m, e, p) = existing_clauses(&binding, x);
            self.matches.push(m);
            self.expects.push(e);
            self.params.extend(p);
            self.bound.insert(id, binding.clone());
            return binding;
        }
        let node = ix.nodes[id];
        let (text, p) = node_clause(&binding, node);
        self.clauses.push(text);
        self.params.extend(p);
        self.bound.insert(id, binding.clone());
        for edge in ix.required_out(node) {
            if !accepted.contains(edge.dst.as_str()) || self.edges.contains(edge.source_id.as_str())
            {
                continue;
            }
            let dst = self.node(ix, edge.dst.as_str(), accepted);
            self.edge(&binding, &dst, edge);
        }
        binding
    }

    fn edge(&mut self, src: &str, dst: &str, edge: &'a SourceEdge) {
        let param = format!("e{}", self.edges.len());
        let (text, p) = edge_clause(src, dst, edge, &param);
        self.clauses.push(text);
        self.params.extend(p);
        self.edges.insert(edge.source_id.as_str());
    }

    fn finish(self) -> (String, Bindings) {
        let text: Vec<String> = self
            .matches
            .into_iter()
            .chain(self.expects)
            .chain(self.clauses)
            .collect();
        (text.join("\n"), self.params)
    }
}

/// The landing statement of `items`' landable set (`landable`): existing
/// destination nodes bound by MATCH and pinned by EXPECT, every node minted,
/// every edge created, and every minted identifier RETURNed. Answers the
/// statement, its bindings, and each landable node's (source identity,
/// binding) in binding order; `None` when nothing would be minted (an empty
/// set, or existing nodes only).
type LandingStatement<'a> = (String, Bindings, Vec<(&'a str, String)>);

fn landing_statement<'a>(items: &'a [SourceItem], landable: &BTreeSet<&str>) -> Result<Option<LandingStatement<'a>>> {
    // Bindings are assigned in SOURCE ORDER over the landable nodes; the edge
    // clauses then reference them. `binding_of` is the only place a source
    // identity meets a statement binding.
    let mut binding_of: BTreeMap<&'a str, String> = BTreeMap::new();
    let mut clauses: Vec<String> = Vec::new();
    let mut params = Bindings::new();
    // Existing destination nodes are bound by MATCH and pinned by EXPECT,
    // ahead of every mutation (the fixed clause order), as `m<k>`.
    let mut matches: Vec<String> = Vec::new();
    let mut expects: Vec<String> = Vec::new();
    let mut existing_of: BTreeMap<&str, String> = BTreeMap::new();
    for item in items.iter() {
        let SourceItem::Existing(x) = item else {
            continue;
        };
        if !landable.contains(x.source_id.as_str()) {
            continue;
        }
        let binding = format!("m{}", existing_of.len());
        let (m, e, p) = existing_clauses(&binding, x);
        matches.push(m);
        expects.push(e);
        params.extend(p);
        existing_of.insert(x.source_id.as_str(), binding);
    }
    for item in items.iter() {
        let SourceItem::Node(node) = item else {
            continue;
        };
        if !landable.contains(node.source_id.as_str()) {
            continue;
        }
        let binding = format!("n{}", binding_of.len());
        let (text, p) = node_clause(&binding, node);
        clauses.push(text);
        params.extend(p);
        binding_of.insert(node.source_id.as_str(), binding);
    }
    let mut edge_ordinal = 0usize;
    for item in items.iter() {
        let SourceItem::Edge(edge) = item else {
            continue;
        };
        if !landable.contains(edge.source_id.as_str()) {
            continue;
        }
        let bound = |id: &str| binding_of.get(id).or_else(|| existing_of.get(id));
        let (Some(src), Some(dst)) = (bound(edge.src.as_str()), bound(edge.dst.as_str())) else {
            return Err(SpineError::hard(format!(
                "migrate: edge `{}` was judged landable but an endpoint has no binding; the dry \
                 run and the landing disagree",
                edge.source_id
            )));
        };
        let (text, p) = edge_clause(src, dst, edge, &format!("e{edge_ordinal}"));
        edge_ordinal += 1;
        clauses.push(text);
        params.extend(p);
    }

    // RETURN every minted identifier, un-aliased: the default column name of
    // an un-aliased item is its source text (`n0.id`), which needs no alias
    // namespace and cannot collide with a binding name.
    if clauses.is_empty() {
        // Only existing nodes were landable: binding them mutates nothing.
        return Ok(None);
    }
    let mut ordered: Vec<(&'a str, String)> = binding_of.into_iter().collect();
    ordered.sort_by_key(|(_, b)| binding_ordinal(b));
    let returns: Vec<String> = ordered.iter().map(|(_, b)| format!("{b}.id")).collect();
    if !returns.is_empty() {
        clauses.push(format!("RETURN {}", returns.join(", ")));
    }
    let statement: String = matches
        .into_iter()
        .chain(expects)
        .chain(clauses)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Some((statement, params, ordered)))
}

/// **THE WHOLE BATCH AT ONCE**: every item accepted, as ONE statement through
/// the door's VALIDATE mode, the landing statement itself with the mode
/// flipped. When it validates, the landing it predicts is exactly the one
/// [`land`] would submit, so the per-item judgement ([`dry_run`], one
/// validate per node and per edge, each on fresh connections) has nothing
/// to add, and the manifest is every item imported, complete. `None` when
/// anything refuses it: the caller then judges item by item, which says
/// which item and why. Nothing lands, no row is written.
pub fn validate_whole(cauldron: &Cauldron, plan: &MigrationPlan, items: &[SourceItem]) -> Result<Option<Manifest>> {
    let Some(manifest) = accepted_manifest(cauldron, plan, items)? else { return Ok(None) };
    let all: BTreeSet<&str> = items.iter().map(SourceItem::source_id).collect();
    let Some((statement, params, _)) = landing_statement(items, &all)? else {
        return Ok(None);
    };
    match validate(cauldron, plan, &statement, params) {
        Ok(()) => Ok(Some(manifest)),
        // Refused, or anything else: the item-by-item judgement says what.
        Err(_) => Ok(None),
    }
}

/// The manifest that says every item is imported, complete: what
/// [`validate_whole`] answers when the whole batch validates. `None` when
/// an item names a pool this cauldron does not have.
pub(crate) fn accepted_manifest(cauldron: &Cauldron, plan: &MigrationPlan, items: &[SourceItem]) -> Result<Option<Manifest>> {
    plan.check()?;
    check_source_identities(items)?;
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let (pool, shape) = match item {
            SourceItem::Node(n) => (n.pool.clone(), ImportShape::Node { node_type: n.node_type.clone() }),
            SourceItem::Edge(e) => (e.pool.clone(), edge_shape(e)),
            SourceItem::Existing(x) => {
                (x.pool.clone(), ImportShape::Existing { node_type: x.node_type.clone(), id: x.id.clone() })
            }
        };
        if cauldron.grammar(&pool).is_none() {
            return Ok(None);
        }
        entries.push(ManifestEntry {
            source_id: item.source_id().to_string(),
            disposition: Disposition::Imported {
                pool,
                shape,
                resolution: Resolution::Complete,
                transforms: item.transforms().to_vec(),
            },
        });
    }
    Ok(Some(Manifest {
        initiator: plan.initiator.clone(),
        instrument: plan.instrument.clone(),
        source_count: items.len(),
        entries,
        attempt: None,
    }))
}

/// The COMMIT half: the accepted set, as ONE statement, through
/// the ONE door, under ONE attempt identifier. The commit never meets a
/// refusable item — every item in it already passed the same validator.
fn land(
    cauldron: &Cauldron,
    plan: &MigrationPlan,
    items: &[SourceItem],
    mut manifest: Manifest,
) -> Result<(Manifest, Landing)> {
    let landable: BTreeSet<&str> = manifest
        .landable()
        .into_iter()
        .map(|e| e.source_id.as_str())
        .collect();
    if landable.is_empty() {
        return Ok((manifest, Landing::NothingAccepted));
    }
    let Some((statement, params, ordered)) = landing_statement(items, &landable)? else {
        return Ok((manifest, Landing::NothingAccepted));
    };

    let envelope = Envelope {
        statement,
        bindings: params,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: plan.initiator.clone(),
            instrument: plan.instrument.clone(),
        },
        mode: Mode::Commit,
        idempotency_nonce: plan.idempotency_nonce.clone(),
    };

    match cauldron.submit(envelope) {
        Ok(Outcome::Committed {
            tx,
            seq,
            returned,
            deduplicated,
        }) => {
            let mut landed = BTreeMap::new();
            let row = returned.0.first();
            // Each source identity's item, looked up once (not searched for
            // per landed node: a batch of thousands would search thousands).
            let item_of: BTreeMap<&str, &SourceItem> = items.iter().map(|i| (i.source_id(), i)).collect();
            for (source_id, binding) in &ordered {
                let column = format!("{binding}.id");
                let id = row
                    .and_then(|r| r.get(&column))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        SpineError::hard(format!(
                            "migrate: the landing returned no identifier for `{source_id}` under \
                             `{column}`"
                        ))
                    })?;
                let pool = match *item_of.get(source_id).expect("a landed binding comes from a source item") {
                    SourceItem::Node(n) => n.pool.clone(),
                    SourceItem::Edge(e) => e.pool.clone(),
                    SourceItem::Existing(x) => x.pool.clone(),
                };
                landed.insert(
                    (*source_id).to_string(),
                    QualifiedNodeId {
                        pool,
                        id: crate::pool::NodeId(id.to_string()),
                    },
                );
            }
            manifest.attempt = Some(AttemptStamp {
                tx: tx.to_hex(),
                seq: seq.0,
            });
            Ok((
                manifest,
                Landing::Committed {
                    tx,
                    seq: seq.0,
                    landed,
                    deduplicated,
                },
            ))
        }
        Ok(Outcome::Validated { .. }) => Err(SpineError::hard(
            "migrate: the landing envelope was submitted in commit mode and came back validated",
        )),
        Err(SpineError::Refused { witnesses }) => Ok((manifest, Landing::Refused { witnesses })),
        Err(other) => Err(other),
    }
}

// ---------------------------------------------------------------------------
// interior
// ---------------------------------------------------------------------------

/// A source that names one identity twice is a BROKEN ADAPTER, and it is
/// refused before anything is at stake rather than dispositioned: two entries
/// under one identity would make the manifest's own accounting ambiguous
/// (which entry does an edge endpoint mean?), and a manifest whose accounting
/// is ambiguous is worse than no manifest.
fn check_source_identities(items: &[SourceItem]) -> Result<()> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut carried_ids: BTreeSet<(&PoolName, &str)> = BTreeSet::new();
    for item in items {
        let id = item.source_id();
        if id.trim().is_empty() {
            return Err(SpineError::usage(
                "migrate: a source item carries an empty source identity; the driver accounts by \
                 identity and cannot account for an anonymous item",
            ));
        }
        if !seen.insert(id) {
            return Err(SpineError::usage(format!(
                "migrate: the source yields `{id}` twice; one source identity is one item"
            )));
        }
        match item {
            SourceItem::Node(n) => {
                for reserved in ["id", "state"] {
                    if n.fields.contains_key(reserved) {
                        return Err(SpineError::usage(format!(
                            "migrate: source item `{id}` carries the reserved field `{reserved}`; \
                             `id` is store-minted or carried in `carried_id`, and `state` is the \
                             node's state"
                        )));
                    }
                }
                if let Some(carried) = &n.carried_id {
                    if !carried_ids.insert((&n.pool, carried.as_str())) {
                        return Err(SpineError::usage(format!(
                            "migrate: the source carries id `{carried}` into pool `{}` twice; one \
                             carried id is one node",
                            n.pool
                        )));
                    }
                }
            }
            SourceItem::Edge(e) => {
                if e.note.is_some() && !e.fields.is_empty() {
                    return Err(SpineError::usage(format!(
                        "migrate: source edge `{id}` carries both a `note` and `fields`; an edge \
                         clause's NOTE carries one or the other (a string is the note, an object \
                         the declared fields)"
                    )));
                }
            }
            SourceItem::Existing(_) => {}
        }
    }
    Ok(())
}

/// Every destination pool that already holds nodes. Reads the DESTINATION
/// only — the source is never opened by anything in this crate.
fn destination_population(cauldron: &Cauldron) -> Result<Vec<PopulatedPool>> {
    let names: Vec<PoolName> = cauldron.pools().cloned().collect();
    let mut out = Vec::new();
    for name in names {
        let grammar = cauldron.grammar(&name).cloned().ok_or_else(|| {
            SpineError::hard(format!("cauldron lists pool `{name}` with no pinned grammar"))
        })?;
        let pool = cauldron.open_pool(&name)?;
        let snapshot = pool.snapshot()?;
        let mut nodes = 0usize;
        for node_type in grammar.node_types.keys() {
            nodes += snapshot.nodes_of(node_type, None)?.len();
        }
        if nodes > 0 {
            out.push(PopulatedPool { pool: name, nodes });
        }
    }
    Ok(out)
}

/// One item through the door's VALIDATE mode (the SAME statement
/// with the mode flipped — one validator, two callers). Nothing lands, no row
/// is written, no lock is taken.
fn validate(
    cauldron: &Cauldron,
    plan: &MigrationPlan,
    statement: &str,
    bindings: Bindings,
) -> Result<()> {
    let envelope = Envelope {
        statement: statement.to_string(),
        bindings,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: plan.initiator.clone(),
            instrument: plan.instrument.clone(),
        },
        mode: Mode::Validate,
        // The dry run deliberately carries NO nonce: `mode` is outside the
        // hash identity, so a nonce here would be the landing's identity too.
        idempotency_nonce: None,
    };
    match cauldron.submit(envelope)? {
        Outcome::Validated { .. } => Ok(()),
        // Unreachable by the door's contract: a Validate-mode envelope is
        // answered `Validated` or refused, never `Committed` — the door reads
        // the mode BEFORE it consults the record (exec phase 4). This arm
        // used to be the driver's own defence against a door that answered
        // `Committed { deduplicated: true }` for a dry run of a landed
        // identity; the door is fixed at the source and the
        // arm stays only because the match is exhaustive. Reaching it is a
        // crate defect, named as one.
        Outcome::Committed { tx, .. } => Err(SpineError::hard(format!(
            "SPINE INTERNAL INVARIANT VIOLATED: the door answered a VALIDATE-mode dry run with \
             commit `{tx}`; a dry run is never a commit (exec phase 4 reads the mode before the \
             record)"
        ))),
    }
}

/// One node clause plus its parameters: `IMPORT (binding: pool/type { id:
/// $binding_id, ... })` when the item carries an id (the import-only form),
/// `CREATE (binding: pool/type { ... })` otherwise. Every value
/// travels as a `$param`: values are NEVER spliced into statement text,
/// which is also why this function needs no escaping and
/// cannot be made to inject.
fn node_clause(binding: &str, node: &SourceNode) -> (String, Bindings) {
    let mut params = Bindings::new();
    let mut fields: Vec<String> = Vec::new();
    if let Some(id) = &node.carried_id {
        let name = format!("{binding}_id");
        params.insert(name.clone(), Value::String(id.clone()));
        fields.push(format!("id: ${name}"));
    }
    if let Some(state) = &node.state {
        let name = format!("{binding}_state");
        params.insert(name.clone(), Value::String(state.clone()));
        fields.push(format!("state: ${name}"));
    }
    for (field, value) in &node.fields {
        let name = format!("{binding}_{field}");
        params.insert(name.clone(), value.clone());
        fields.push(format!("{field}: ${name}"));
    }
    let map = if fields.is_empty() {
        String::new()
    } else {
        format!(" {{ {} }}", fields.join(", "))
    };
    let keyword = if node.carried_id.is_some() { "IMPORT" } else { "CREATE" };
    (
        format!(
            "{keyword} ({binding}: {}/{}{map})",
            node.pool, node.node_type
        ),
        params,
    )
}

/// The MATCH head and the EXPECT pin that bind one existing destination
/// node ([`ExistingNode`]): `MATCH (b: pool/type { id: $b_id })` and
/// `EXPECT COUNT(b) = 1`. The id travels as a parameter, never spliced.
fn existing_clauses(binding: &str, x: &ExistingNode) -> (String, String, Bindings) {
    let mut params = Bindings::new();
    let name = format!("{binding}_id");
    params.insert(name.clone(), Value::String(x.id.clone()));
    (
        format!("MATCH ({binding}: {}/{} {{ id: ${name} }})", x.pool, x.node_type),
        format!("EXPECT COUNT({binding}) = 1"),
        params,
    )
}

/// One edge clause plus its parameter: `CREATE (src) -[kind]-> (dst)`, with
/// `NOTE $param` bound to the edge's free-text note (a string) or to its
/// declared fields (an object), and nothing when it has neither.
fn edge_clause(src: &str, dst: &str, edge: &SourceEdge, param: &str) -> (String, Bindings) {
    let mut params = Bindings::new();
    let note = if let Some(note) = &edge.note {
        params.insert(param.to_string(), Value::String(note.clone()));
        format!(" NOTE ${param}")
    } else if !edge.fields.is_empty() {
        params.insert(
            param.to_string(),
            Value::Object(edge.fields.clone().into_iter().collect()),
        );
        format!(" NOTE ${param}")
    } else {
        String::new()
    };
    (format!("CREATE ({src}) -[{}]-> ({dst}){note}", edge.kind), params)
}

fn edge_shape(edge: &SourceEdge) -> ImportShape {
    ImportShape::Edge {
        kind: edge.kind.clone(),
        src: edge.src.clone(),
        dst: edge.dst.clone(),
    }
}

fn reject(
    source_id: &str,
    code: &str,
    detail: String,
    witnesses: Vec<Witness>,
) -> ManifestEntry {
    ManifestEntry {
        source_id: source_id.to_string(),
        disposition: Disposition::Rejected {
            reason: Rejection {
                code: code.to_string(),
                detail,
                witnesses,
            },
        },
    }
}

/// The dangling-edge rule, both ends, near end first (deterministic:
/// an edge with two bad ends reports the near one, always).
fn unresolved_endpoint(
    edge: &SourceEdge,
    ix: &Index<'_>,
    accepted: &BTreeSet<&str>,
) -> Option<Resolution> {
    for (side, source_ref) in [
        (EndpointSide::Src, edge.src.as_str()),
        (EndpointSide::Dst, edge.dst.as_str()),
    ] {
        if !ix.is_node(source_ref) {
            return Some(Resolution::UnresolvedEndpoint {
                endpoint: side,
                source_ref: source_ref.to_string(),
                marker: MARKER_ABSENT_FROM_SOURCE.to_string(),
            });
        }
        if !accepted.contains(source_ref) {
            return Some(Resolution::UnresolvedEndpoint {
                endpoint: side,
                source_ref: source_ref.to_string(),
                marker: MARKER_ENDPOINT_REJECTED.to_string(),
            });
        }
    }
    None
}

/// `n12` -> 12. Bindings are minted by this module and are always `n<digits>`;
/// anything else sorts last rather than panicking.
fn binding_ordinal(binding: &str) -> usize {
    binding
        .strip_prefix('n')
        .and_then(|d| d.parse::<usize>().ok())
        .unwrap_or(usize::MAX)
}
