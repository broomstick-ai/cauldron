//! Pool schema definitions — a pool schema definition is real machinery
//! here (the fixtures' particulars are illustrative).
//!
//! Each pool carries a PINNED grammar naming its KIND, its node types, their
//! fields and state vocabularies, its machine-maintained count cells, and
//! its edge kinds with endpoint types — and the door validates
//! every part of a statement against the touched pool's OWN grammar (the
//! version-blind rule assumes exactly this). The production schema system is
//! NOT in scope; this is the minimal definition mechanism the fixtures need.
//!
//! # The cross-pool grammar
//!
//! Every grammar declares a KIND (`workspace`, `blueprint`, ...); two pools may
//! share a kind. An edge kind's far endpoint is a SET of permitted endpoints,
//! each either LOCAL (a node type in this pool only) or a REACH — `{pool kind,
//! node type}` — the SOURCE pool's outbound declaration of what it may point
//! at across the cauldron. A reach is BY KIND, never by pool name: the source
//! cannot enumerate every other workspace by name (an issue in one workspace
//! blocked by something in ANOTHER workspace), and a reach by kind covers
//! every pool of that kind, the declaring pool included.
//!
//! Validating a cross-pool edge at the door is then a LOOKUP: the cauldron
//! already holds every pool's pinned grammar, so the validator consults the
//! FAR pool's own pin — its kind, its declared node types, the far node's
//! actual type — and records the far pin's version on the compiled part
//! forever. This is not a
//! breach of version-blindness: the spine still shares no vocabulary
//! across pools; it reads the far pool's own pin. The projected `spine` namespace is a pool
//! KIND too (`spine`), with a CODE-DEFINED grammar ([`crate::project`]) —
//! which is what gives the lookup something to look up for a ghost target.

use crate::delta::{CompiledPart, Delta};
use crate::error::{Result, SpineError};
use crate::pool::{NodeId, QualifiedNodeId};
use crate::witness::Witness;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A pool's name. Also the pool's file name inside the cauldron directory
/// (`<name>.db`), so it is constrained to
/// `[a-z][a-z0-9-]*` and the name `spine` is RESERVED for the projected
/// transaction namespace and for `spine.db` itself — creating a
/// pool named `spine` refuses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PoolName(pub String);

impl PoolName {
    pub fn new(name: impl Into<String>) -> PoolName {
        PoolName(name.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `[a-z][a-z0-9-]*` — the file-name constraint stated in this type's
    /// own docs, made checkable. Does NOT judge the reserved name; see
    /// [`PoolName::is_reserved`].
    pub fn is_well_formed(&self) -> bool {
        well_formed_name(&self.0)
    }

    /// `spine` is reserved for the projected transaction namespace
    /// and for `spine.db` itself — creating a pool with this
    /// name refuses.
    pub fn is_reserved(&self) -> bool {
        self.0 == crate::project::PROJECTED_POOL
    }
}

impl std::fmt::Display for PoolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A pool's KIND — what a reach names (e.g. a workspace may declare
/// that it can write certain edge types to the blueprints). Well-formed like
/// a pool name (`[a-z][a-z0-9-]*`); two pools may share a kind; the kind
/// `spine` is reserved for the projected namespace, whose grammar is code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PoolKind(pub String);

impl PoolKind {
    pub fn new(kind: impl Into<String>) -> PoolKind {
        PoolKind(kind.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_well_formed(&self) -> bool {
        well_formed_name(&self.0)
    }
    /// The projected namespace's kind; no real pool may declare it.
    pub fn is_reserved(&self) -> bool {
        self.0 == crate::project::PROJECTED_KIND
    }
}

impl std::fmt::Display for PoolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn well_formed_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The direction of an edge slice or count cell relative to its node:
/// `Out` = edges leaving the node, `In` = edges arriving at it (a slice is
/// the same-type edges at a node in ONE direction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Out,
    In,
}

/// One pool's pinned grammar (the "pool's pinned version").
///
/// Serialized (canonical JSON) into the pool file at create; the pin moves
/// only by migration machinery that is out of scope here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolGrammar {
    pub pool: PoolName,
    /// The pool's KIND — what another pool's reach names. Required,
    /// no serde default: a pinned grammar that declares no kind cannot be
    /// reached across and cannot be judged as a far endpoint, and there are no
    /// pinned pools older than this field to keep parsing.
    pub kind: PoolKind,
    /// The pinned schema version — what every node's machine-owned stamp is
    /// copied from at creation and what each
    /// transaction part records as validated-under.
    pub version: u32,
    pub node_types: BTreeMap<String, NodeTypeDef>,
    pub edge_kinds: BTreeMap<String, EdgeKindDef>,
}

/// One node type: its fields, its state vocabulary, its count cells.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeTypeDef {
    /// Declared fields by name. A statement writing an undeclared field is a
    /// grammar violation witness.
    pub fields: BTreeMap<String, FieldDef>,
    /// The state vocabulary. Empty means the type carries no `state` — a
    /// statement addressing `state` on it refuses. A non-empty vocabulary is
    /// closed: values outside it refuse.
    pub states: Vec<String>,
    /// Machine-maintained count cells, keyed by cell name (grammar-declared;
    /// increments derived mechanically inside the
    /// per-pool landing transaction by commutative UPDATE — never a
    /// caller-spoken delta, never read-modify-write).
    pub count_cells: BTreeMap<String, CountCellDef>,
    /// The id PREFIX every node of this type carries (ids are type-prefixed
    /// serial per pool). `Some("M")`
    /// means every minted id is `M-<rest>` with a non-empty rest; a CREATE
    /// whose id lacks it refuses `id_prefix_mismatch`. A `Some`
    /// type's id is minted by the SPINE: the compiler writes the provisional
    /// `M-~<hex>` (`ladle::compile::provisional_serial_id`) and the decided
    /// flip rewrites it to the serial `M-<n>` (`spine_db::assign_serials`).
    /// The prefix check judges the PREFIX only, so a provisional `M-~<hex>`
    /// carries it; at a POOL LANDING (`FarSide::Landing`) the validator then
    /// refuses any provisional id `provisional_id_at_landing`, since
    /// the flip must have rewritten it first. `None` leaves the id
    /// shape unjudged, and such a type's id is still the compiler's hash mint
    /// `n<16 hex>` (`ladle::compile::mint_node_id` — the fixtures; the
    /// prefixed path sits beside that rule, it does not retire it). Unique
    /// across a grammar's types:
    /// the prefix IS the type's name on the wire. Serde default so grammars
    /// pinned before this field parse.
    #[serde(default)]
    pub id_prefix: Option<String>,
    /// Edges a node of this type must carry AT MINT, by edge kind, with the
    /// minimum count the same part must carry leaving the minted node
    /// (e.g. `serves` on a milestone, `in_region` on every
    /// content node, "at least two candidates" as `rejected_by: 2`). Judged
    /// over the part being written only — the edges are in the part or they
    /// are not. A shortfall refuses with
    /// [`Witness::RequiredEdgeMissing`] naming the kind, the minimum and the
    /// count found. Serde default as above.
    #[serde(default)]
    pub required_edges_at_mint: BTreeMap<String, u32>,
    /// ONE-OF required-at-mint (the regionless mint): a node
    /// of this type must be minted satisfying AT LEAST ONE of these
    /// alternatives IN FULL, beside everything [`Self::required_edges_at_mint`]
    /// demands (which is an AND and stands on its own). v6's content types:
    /// `[{in_region: 1}, {rejected_by: 2; field searched}]`. Judged over the
    /// part being written only, exactly as the AND form: edges are counted
    /// as they SURVIVE the part (adds and removes applied in delta order —
    /// which closes the add-then-remove hole), and a field counts
    /// when the CREATE carries it or the same part SETs it whole. A part
    /// that meets none refuses with the shortfall of the alternative it came
    /// CLOSEST to (see [`MintAlternative`]). Empty (the default, and every
    /// grammar older than this field) demands nothing; serde default and omitted when
    /// empty, so a grammar pinned before it serializes byte-identically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_at_mint_any_of: Vec<MintAlternative>,
}

/// One alternative of [`NodeTypeDef::required_at_mint_any_of`]: minimum
/// counts of edge kinds leaving the minted node, plus node fields that must
/// be present, ALL of which the part must meet for this alternative to hold.
///
/// When no alternative holds, the refusal names the alternative the part
/// came closest to — the most PROGRESS, counted as `min(found, minimum)`
/// summed over its edge kinds plus one per present field; ties go to the
/// FIRST declared — and within it the first shortfall: an edge kind short
/// refuses [`Witness::RequiredEdgeMissing`] (kind, minimum, found, by
/// value), else a missing field refuses `missing_required_field`. So a mint
/// carrying nothing names the first alternative (`in_region`, 1, 0), and a
/// mint carrying one `rejected_by` and `searched` names `rejected_by` (2, 1)
/// — the alternative it was attempting.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MintAlternative {
    /// Edge kind -> minimum surviving count leaving the minted node.
    #[serde(default)]
    pub edges: BTreeMap<String, u32>,
    /// Node fields the minted node must carry.
    #[serde(default)]
    pub fields: BTreeSet<String>,
}

/// One declared field: whether it is required at mint, what KIND of JSON
/// value it holds, and — for a text field — the character cap and floor
/// the door holds it to. Value typing was once
/// deliberately absent (the fixtures do not need it); v6 needs a handle
/// of at most 235 characters and a body of at most 10,000, and the cap law
/// says those are HARD by one character and refused with measured, cap and
/// overage. Field values are JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldDef {
    /// Must be present at node creation.
    pub required: bool,
    /// The JSON shape the value must have. [`FieldKind::Any`] (the default,
    /// and what every grammar older than field kinds means) judges nothing.
    #[serde(default)]
    pub kind: FieldKind,
    /// The character CAP: a value of MORE than this many characters refuses
    /// [`Witness::OverCap`]. Exactly the cap is accepted — "hard by one
    /// character". Legal only with [`FieldKind::Text`]: a cap is a count of
    /// characters, and only text has them (`check_wellformed` refuses a cap
    /// on any other kind rather than inventing a meter for it).
    #[serde(default)]
    pub max_chars: Option<u64>,
    /// The character FLOOR: a value of FEWER than this many characters
    /// refuses [`Witness::UnderFloor`]. Exactly the floor is accepted. Text
    /// only, as above.
    #[serde(default)]
    pub min_chars: Option<u64>,
}

impl FieldDef {
    /// A required field of any kind, uncapped — the original shape.
    pub fn required() -> FieldDef {
        FieldDef {
            required: true,
            ..FieldDef::default()
        }
    }
    /// An optional field of any kind, uncapped.
    pub fn optional() -> FieldDef {
        FieldDef::default()
    }
    /// A text field (required or not) held to `max_chars` characters.
    pub fn text(required: bool, max_chars: u64) -> FieldDef {
        FieldDef {
            required,
            kind: FieldKind::Text,
            max_chars: Some(max_chars),
            min_chars: None,
        }
    }
}

/// The JSON shape a declared field's value must have ("field kinds").
/// Judged on every whole-field write — a CREATE's initial value and a SET
/// whose path is the bare field — and a dotted SET into a field of any kind
/// but `Any` or `Object` refuses `wrong_field_kind`, because addressing
/// through a value that has no keys would either destroy it (`not_an_object`
/// at the landing) or, when the field is absent, mint an object inside a
/// field declared to hold text. The kinds are JSON's own shapes and
/// nothing more: a syntax rule over the text (links, clause words) is the
/// schema author's constraint, not the door's grammar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// Any JSON value; nothing judged (the default).
    #[default]
    Any,
    /// A JSON string. The only kind a character cap or floor may bind.
    Text,
    /// A JSON integer (`i64`); a fraction or a numeric string refuses.
    Integer,
    /// A JSON boolean.
    Boolean,
    /// A JSON object. Dotted SETs may address into it.
    Object,
}

impl FieldKind {
    /// The kind's name as the grammar spells it.
    pub fn name(self) -> &'static str {
        match self {
            FieldKind::Any => "any",
            FieldKind::Text => "text",
            FieldKind::Integer => "integer",
            FieldKind::Boolean => "boolean",
            FieldKind::Object => "object",
        }
    }

    /// Does `value` have this shape?
    pub fn admits(self, value: &serde_json::Value) -> bool {
        match self {
            FieldKind::Any => true,
            FieldKind::Text => value.is_string(),
            FieldKind::Integer => value.is_i64(),
            FieldKind::Boolean => value.is_boolean(),
            FieldKind::Object => value.is_object(),
        }
    }

    /// What a value of this shape is called in a refusal's detail.
    fn describe(value: &serde_json::Value) -> &'static str {
        use serde_json::Value;
        match value {
            Value::Null => "null",
            Value::Bool(_) => "a boolean",
            Value::Number(n) if n.is_i64() => "an integer",
            Value::Number(_) => "a non-integer number",
            Value::String(_) => "a string",
            Value::Array(_) => "an array",
            Value::Object(_) => "an object",
        }
    }

    /// May a dotted SET address INTO a field of this kind?
    pub fn addressable(self) -> bool {
        matches!(self, FieldKind::Any | FieldKind::Object)
    }
}

/// The character count the cap law meters — CHARACTERS, never bytes (bytes
/// would charge three for one accented letter). One home, so
/// the cap and the floor cannot disagree about what they measured.
pub fn measure_chars(s: &str) -> u64 {
    s.chars().count() as u64
}

/// One count cell: counts edges of `edge_kind` in `direction` at nodes of
/// the declaring type. E.g. fixture-alpha's
/// `author.docs_written` counts `wrote` edges outgoing from the author.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CountCellDef {
    pub edge_kind: String,
    pub direction: Direction,
}

/// One edge kind: its source type (always LOCAL — the source is the node
/// whose record the edge hangs off), the SET of far endpoints it may arrive
/// at (local types and reaches), and the at-most-one slice
/// constraints that make a slice a SCALAR (link
/// mode forbidden, membership changes are slice writes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeKindDef {
    /// The SET of node types an edge of this kind may leave. Non-empty, and
    /// every entry LOCAL: the source is the node whose record the edge hangs
    /// off, so it is always in the pool that stores the edge. A set rather
    /// than one type, because v6's `supersedes` leaves any
    /// content type and `depends_on` leaves a task or a milestone — one
    /// kind, several sources.
    pub from: BTreeSet<TypeRef>,
    /// The permitted far endpoints. Non-empty. LOCAL entries mean this pool
    /// only; a REACH covers every pool of its kind, this pool included; a
    /// [`TypeRef::SameAsSource`] entry means a local node of whatever type
    /// the SOURCE node has (`supersedes`: "node -> node, same type").
    pub to: BTreeSet<TypeRef>,
    /// At most one edge of this kind may LEAVE any one `from` node.
    pub at_most_one_per_from: bool,
    /// At most one edge of this kind may ARRIVE at any one `to` node —
    /// fixture-alpha's `current_of` ("at most one per author") sets this.
    /// Never legal beside a reach: that slice lives in another pool.
    pub at_most_one_per_to: bool,
    /// The FIELDS an edge of this kind carries (e.g. `admits` on `in_region`, `note` on `supersedes`,
    /// `rejects`/`rev` on `rejected_by`, `cite`/`why` on `shows`, the
    /// `sentence` a `ref` keeps). The SAME [`FieldDef`] a node field uses —
    /// kind, HARD character cap and floor, required or not — judged by the
    /// same function, so an edge field and a node field cannot disagree
    /// about what a cap is. An edge create naming a field not declared here
    /// refuses `undeclared_edge_field`; one missing a required field refuses
    /// `missing_required_edge_field`; a cap or floor breach refuses with
    /// [`crate::witness::Witness::EdgeFieldOverCap`] /
    /// [`crate::witness::Witness::EdgeFieldUnderFloor`], by value. Empty (the
    /// default, and every grammar older than edge fields) declares none. Serde default and
    /// omitted when empty, so a grammar pinned before this field parses and
    /// serializes byte-identically.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, FieldDef>,
}

impl EdgeKindDef {
    /// A purely local edge kind: one `from` and one local `to`, no
    /// constraints.
    pub fn local(from: impl Into<String>, to: impl Into<String>) -> EdgeKindDef {
        EdgeKindDef {
            from: BTreeSet::from([TypeRef::local(from)]),
            to: BTreeSet::from([TypeRef::local(to)]),
            at_most_one_per_from: false,
            at_most_one_per_to: false,
            fields: BTreeMap::new(),
        }
    }

    /// An edge kind leaving one `from`, whose far endpoints are exactly
    /// `to`, no constraints.
    pub fn reaching(from: impl Into<String>, to: impl IntoIterator<Item = TypeRef>) -> EdgeKindDef {
        EdgeKindDef {
            from: BTreeSet::from([TypeRef::local(from)]),
            to: to.into_iter().collect(),
            at_most_one_per_from: false,
            at_most_one_per_to: false,
            fields: BTreeMap::new(),
        }
    }

    /// An edge kind leaving ANY of `from` (local types), arriving at
    /// exactly `to`, no constraints (the multi-source shape).
    pub fn between(
        from: impl IntoIterator<Item = impl Into<String>>,
        to: impl IntoIterator<Item = TypeRef>,
    ) -> EdgeKindDef {
        EdgeKindDef {
            from: from.into_iter().map(TypeRef::local).collect(),
            to: to.into_iter().collect(),
            at_most_one_per_from: false,
            at_most_one_per_to: false,
            fields: BTreeMap::new(),
        }
    }

    /// May an edge of this kind leave a local node of `node_type`?
    pub fn leaves(&self, node_type: &str) -> bool {
        self.from.contains(&TypeRef::local(node_type))
    }

    /// Every reach this kind declares, as `(kind, node type)`.
    pub fn reaches(&self) -> impl Iterator<Item = (&PoolKind, &str)> {
        self.to.iter().filter_map(|t| match t {
            TypeRef::Local { .. } | TypeRef::SameAsSource => None,
            TypeRef::Reach { kind, node_type } => Some((kind, node_type.as_str())),
        })
    }

    /// Does this kind declare ANY reach — may it ever point out of its pool?
    pub fn reaches_across(&self) -> bool {
        self.reaches().next().is_some()
    }

    /// Is a far endpoint of `node_type` in a pool of `far_kind` permitted,
    /// for an edge leaving a node of `src_type`? `local` says whether that
    /// far pool is the declaring pool itself: a LOCAL entry permits it only
    /// then; a REACH of the same kind permits it either way ("the declaring
    /// pool included"); a SAME-AS-SOURCE entry permits a local node whose
    /// type is exactly `src_type`.
    pub fn permits(&self, far_kind: &PoolKind, node_type: &str, local: bool, src_type: &str) -> bool {
        self.to.iter().any(|t| match t {
            TypeRef::Local { node_type: nt } => local && nt == node_type,
            TypeRef::Reach { kind, node_type: nt } => kind == far_kind && nt == node_type,
            TypeRef::SameAsSource => local && node_type == src_type,
        })
    }
}

/// One permitted endpoint of an edge kind: a node type in the
/// declaring pool, or a REACH into every pool of a kind. The old
/// `foreign(<pool name>, type)` form is gone — reach is by kind, never by
/// pool name, because the source cannot enumerate every other workspace by
/// name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum TypeRef {
    /// A node type in the declaring pool only.
    Local { node_type: String },
    /// A node type in ANY pool of `kind` — the declaring pool included.
    Reach { kind: PoolKind, node_type: String },
    /// A LOCAL node of the same type as the edge's SOURCE node (v6
    /// `supersedes`: "node -> node, same type"). Legal in `to` only — an
    /// edge's source has no source to be the same as.
    SameAsSource,
}

impl TypeRef {
    /// A type in the declaring pool.
    pub fn local(node_type: impl Into<String>) -> TypeRef {
        TypeRef::Local {
            node_type: node_type.into(),
        }
    }
    /// A type in every pool of a kind.
    pub fn reach(kind: impl Into<String>, node_type: impl Into<String>) -> TypeRef {
        TypeRef::Reach {
            kind: PoolKind::new(kind),
            node_type: node_type.into(),
        }
    }
    /// The node type this endpoint names — `None` for
    /// [`TypeRef::SameAsSource`], which names none of its own.
    pub fn node_type(&self) -> Option<&str> {
        match self {
            TypeRef::Local { node_type } | TypeRef::Reach { node_type, .. } => Some(node_type),
            TypeRef::SameAsSource => None,
        }
    }
    pub fn is_local(&self) -> bool {
        matches!(self, TypeRef::Local { .. })
    }
}

// ---------------------------------------------------------------------------
// Grammar validation — per-pool and
// VERSION-PINNED, with the far side of a cross-pool edge consulted by LOOKUP.
//
// The version-blind rule stands: each part validates under its OWN
// pool's pinned grammar, and no vocabulary is shared across pools. In
// addition, a cross-pool edge consults the FAR pool's own pin — through
// `FarSide`, at the door — for the far pool's kind, its declared types and the
// far node's actual type, and the far pin's version is recorded on the part.
// ---------------------------------------------------------------------------

/// The field names the machinery owns; no pen, agent or human, sets them
/// (for the schema-version stamp: a fact an agent could edit is
/// not a fact). A delta addressing any of these refuses.
///
/// `sealed` is the seal flag: set only by the `SEAL` statement form
/// (`delta::Delta::Seal`), never by a SET or a CREATE field, never cleared.
pub const MACHINE_OWNED_FIELDS: &[&str] =
    &["id", "rev", "schema_version", "created_tx", "updated_tx", "sealed"];

/// The field a seal freezes: the field named `body`, in every pool —
/// the same name the resource rule keys on (a field named `body` is a
/// resource). Once a node is sealed, any SET whose target routes
/// (`pool::set_target`) to this field — whole or dotted — refuses
/// `sealed_body`; its state, its other fields and its edges stay writable,
/// and correction is by superseding (a new node plus the edges the grammar
/// allows). A spine rule rather than per-type grammar data: it reads the
/// node's own machine-owned flag, which no grammar can declare.
pub const SEALED_BODY_FIELD: &str = "body";

/// The FROZEN machine tags a grammar refusal carries. `Witness::GrammarViolation`
/// (owned by the witness module, whose shape is frozen) has one prose
/// `message` field and no code field, so the tag is carried as the message's
/// `"<tag>: <detail>"` PREFIX and read back with [`grammar_fault_tag`].
/// Suites assert on the tag, never on the detail — assert on the
/// STRUCTURE, never the prose.
///
/// A `code` field on the witness would be cleaner
/// than a prefix convention. Not taken, because `Witness` is a
/// frozen surface.
///
/// The cross-pool grammar retired `foreign_endpoint_pool` (reach is by kind, not by pool
/// name) and added the four cross-pool tags at the end.
pub const GRAMMAR_FAULTS: &[&str] = &[
    "wrong_pool",
    "version_mismatch",
    "unknown_node_type",
    "unknown_node",
    "duplicate_node_id",
    "undeclared_field",
    "machine_owned_field",
    "machine_owned_cell",
    "missing_required_field",
    "unknown_state",
    "state_not_declared",
    // A dotted SET addresses through a value that is not an object (a
    // scalar, an array): satisfying it would DESTROY that value, which is
    // data loss behind a `Committed` reply — refused at compile against the
    // matched record and again at the landing.
    "not_an_object",
    "unknown_edge_kind",
    "wrong_endpoint_type",
    "at_most_one_violated",
    "malformed_grammar",
    "empty_path",
    // ---- cross-pool ----
    // The SOURCE declares no reach covering the far node's pool kind and
    // actual type.
    "reach_undeclared",
    // The TARGET's own pinned grammar declares no node type of the name a
    // reach toward its kind uses — the source's declaration is stale against
    // the far pool's pin.
    "foreign_type_unknown",
    // This cauldron does not hold that pool. ONE fact, ONE tag, TWO
    // producers:
    // - THE DOOR, at head resolution (`ladle::compile::resolve_grammars`,
    //   reached by `claim_footprint` at both `Cauldron::submit` and
    //   `Cauldron::read`, and again by `compile`/`claims_for`): a pattern or
    //   CREATE head naming a pool the cauldron lacks. Stamped with the pool
    //   the statement NAMED. This is the only producer a statement through
    //   the door can reach, because a delta's far pool is always some head's
    //   pool and heads resolve first.
    // - THE VALIDATOR, for a direct `validate_part` caller
    //   (`PoolGrammar::validate_reach`): a cross-pool edge whose far pool the
    //   `FarSide::Door` lookup does not answer. Stamped with the SOURCE
    //   part's pool (the grammar whose declaration was being judged); the
    //   detail names the far pool.
    "foreign_pool_unknown",
    // A pool landing met a cross-pool edge whose part carries no record of
    // the far pin it validated under — the part did not come through the
    // door's far check (the landing has no window onto other files).
    "far_unvalidated",
    // ---- the v6 grammar additions ----
    // A text field written over its declared cap. Carried as
    // `Witness::OverCap` — measured, cap and overage by value — and never
    // as prose.
    "over_cap",
    // A text field written under its declared floor: `Witness::UnderFloor`.
    "under_floor",
    // A value whose JSON shape is not the field's declared kind, or a dotted
    // SET into a field whose kind has no keys to address.
    "wrong_field_kind",
    // A node minted with fewer edges of a kind than its type requires at
    // mint: `Witness::RequiredEdgeMissing`, by value.
    "required_edge_missing",
    // A CREATE whose id does not carry its type's declared prefix.
    "id_prefix_mismatch",
    // An IMPORT whose carried id is not a SERIAL `<prefix>-<n>` (n decimal,
    // 1 or above, unpadded): the provisional `-~` form, `Q-0`, `Q-007`,
    // `Q-abc`, `Q-` (it never admits the
    // `-~` provisional form). Prose-shaped, like `provisional_id_at_landing`.
    "carried_id_not_serial",
    // ---- edge fields ----
    // An edge create carrying a field its kind does not declare.
    "undeclared_edge_field",
    // An edge create missing a field its kind declares required.
    "missing_required_edge_field",
    // A create of an edge whose (src, kind, dst) already exists — stored, or
    // earlier in the same part — with DIFFERENT fields. Edge identity is the
    // triple and edges are a set: the identical create is a no-op;
    // a differing one is neither a no-op nor an overwrite, it refuses, and
    // the first edge's fields stand (keeping the first).
    "edge_fields_differ",
    // ---- provisional ids ----
    // A CREATE reaching a pool LANDING whose id is still the door's
    // provisional `<P>-~hex`: the decided flip never assigned its serial.
    "provisional_id_at_landing",
    // ---- the seal ----
    // A SET into the body (`SEALED_BODY_FIELD`) of a node whose `sealed`
    // flag is set — refused at the door (compile, against the matched
    // record and the statement's own earlier SEALs) and again at the
    // landing. The witness names the node; correction is by superseding.
    "sealed_body",
];

/// The STRUCTURED half of a grammar fault — the numbers a cap-law refusal
/// carries (measured, cap, overage). A fault with `Prose` is
/// witnessed as [`Witness::GrammarViolation`] with the tag in its message;
/// the other shapes are witnessed by their own variants, so a suite asserts
/// on the numbers by value and never parses them out of a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaultShape {
    Prose,
    OverCap {
        node_type: String,
        field: String,
        measured: u64,
        cap: u64,
    },
    UnderFloor {
        node_type: String,
        field: String,
        measured: u64,
        floor: u64,
    },
    RequiredEdge {
        node_type: String,
        edge_kind: String,
        minimum: u32,
        found: u32,
    },
    /// An EDGE field over its cap: the same law, owned by an edge
    /// kind rather than a node type.
    EdgeOverCap {
        edge_kind: String,
        field: String,
        measured: u64,
        cap: u64,
    },
    /// An EDGE field under its floor.
    EdgeUnderFloor {
        edge_kind: String,
        field: String,
        measured: u64,
        floor: u64,
    },
}

/// One grammar fault: a frozen machine tag plus human detail, optionally
/// the pool whose grammar REFUSED when that is not the pool being validated
/// (`unknown_node` for a far node is stamped with the FAR pool),
/// and the structured measurements when the fault is a cap-law one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarFault {
    pub tag: &'static str,
    pub detail: String,
    /// Overrides the pool the witness is stamped with; `None` = the caller's.
    pub pool: Option<PoolName>,
    /// The numbers, for the shapes that carry them.
    pub shape: FaultShape,
}

impl GrammarFault {
    pub fn new(tag: &'static str, detail: impl Into<String>) -> GrammarFault {
        debug_assert!(
            GRAMMAR_FAULTS.contains(&tag),
            "grammar fault tags are frozen in GRAMMAR_FAULTS"
        );
        GrammarFault {
            tag,
            detail: detail.into(),
            pool: None,
            shape: FaultShape::Prose,
        }
    }

    /// A text field over its cap: `measured > cap`, so the overage the
    /// witness carries is at least one.
    pub fn over_cap(node_type: &str, field: &str, measured: u64, cap: u64) -> GrammarFault {
        GrammarFault {
            tag: "over_cap",
            detail: format!(
                "`{node_type}.{field}` is {measured} characters; the cap is {cap}; over by {}",
                measured.saturating_sub(cap)
            ),
            pool: None,
            shape: FaultShape::OverCap {
                node_type: node_type.to_string(),
                field: field.to_string(),
                measured,
                cap,
            },
        }
    }

    /// A text field under its floor: `measured < floor`.
    pub fn under_floor(node_type: &str, field: &str, measured: u64, floor: u64) -> GrammarFault {
        GrammarFault {
            tag: "under_floor",
            detail: format!(
                "`{node_type}.{field}` is {measured} characters; the floor is {floor}; short by {}",
                floor.saturating_sub(measured)
            ),
            pool: None,
            shape: FaultShape::UnderFloor {
                node_type: node_type.to_string(),
                field: field.to_string(),
                measured,
                floor,
            },
        }
    }

    /// A node minted with `found` edges of `edge_kind` leaving it when its
    /// type requires at least `minimum`.
    pub fn required_edge(node_type: &str, edge_kind: &str, minimum: u32, found: u32) -> GrammarFault {
        GrammarFault {
            tag: "required_edge_missing",
            detail: format!(
                "a `{node_type}` must be minted with at least {minimum} `{edge_kind}` edge(s) leaving it in the same part; this part carries {found}"
            ),
            pool: None,
            shape: FaultShape::RequiredEdge {
                node_type: node_type.to_string(),
                edge_kind: edge_kind.to_string(),
                minimum,
                found,
            },
        }
    }

    /// The cap-law shapes re-owned by an EDGE kind: a node-field
    /// cap or floor fault from [`PoolGrammar::check_field_value`], judged
    /// over an edge field, carries the edge kind in place of a node type.
    fn on_edge(self) -> GrammarFault {
        let shape = match self.shape {
            FaultShape::OverCap {
                node_type,
                field,
                measured,
                cap,
            } => FaultShape::EdgeOverCap {
                edge_kind: node_type,
                field,
                measured,
                cap,
            },
            FaultShape::UnderFloor {
                node_type,
                field,
                measured,
                floor,
            } => FaultShape::EdgeUnderFloor {
                edge_kind: node_type,
                field,
                measured,
                floor,
            },
            other => other,
        };
        GrammarFault { shape, ..self }
    }

    /// Stamp this fault with the pool whose grammar refused (the far pool of
    /// a cross-pool edge), overriding the validating pool.
    pub fn in_pool(mut self, pool: &PoolName) -> GrammarFault {
        self.pool = Some(pool.clone());
        self
    }

    /// The structured witness (the per-pool poisoned variants
    /// assert on `pool` to prove validation is genuinely per-pool). A
    /// cap-law shape is its own variant, numbers by value.
    pub fn witness(&self, pool: &PoolName, part: Option<u32>) -> Witness {
        let pool = self.pool.clone().unwrap_or_else(|| pool.clone());
        match &self.shape {
            FaultShape::Prose => Witness::GrammarViolation {
                pool,
                part,
                message: format!("{}: {}", self.tag, self.detail),
            },
            FaultShape::OverCap {
                node_type,
                field,
                measured,
                cap,
            } => Witness::OverCap {
                pool,
                part,
                node_type: node_type.clone(),
                field: field.clone(),
                measured: *measured,
                cap: *cap,
                overage: measured.saturating_sub(*cap),
            },
            FaultShape::UnderFloor {
                node_type,
                field,
                measured,
                floor,
            } => Witness::UnderFloor {
                pool,
                part,
                node_type: node_type.clone(),
                field: field.clone(),
                measured: *measured,
                floor: *floor,
                shortfall: floor.saturating_sub(*measured),
            },
            FaultShape::RequiredEdge {
                node_type,
                edge_kind,
                minimum,
                found,
            } => Witness::RequiredEdgeMissing {
                pool,
                part,
                node_type: node_type.clone(),
                edge_kind: edge_kind.clone(),
                minimum: *minimum,
                found: *found,
            },
            FaultShape::EdgeOverCap {
                edge_kind,
                field,
                measured,
                cap,
            } => Witness::EdgeFieldOverCap {
                pool,
                part,
                edge_kind: edge_kind.clone(),
                field: field.clone(),
                measured: *measured,
                cap: *cap,
                overage: measured.saturating_sub(*cap),
            },
            FaultShape::EdgeUnderFloor {
                edge_kind,
                field,
                measured,
                floor,
            } => Witness::EdgeFieldUnderFloor {
                pool,
                part,
                edge_kind: edge_kind.clone(),
                field: field.clone(),
                measured: *measured,
                floor: *floor,
                shortfall: floor.saturating_sub(*measured),
            },
        }
    }

    /// The whole-statement refusal carrying that witness (refusal is
    /// WHOLE; nothing lands anywhere).
    pub fn refusal(&self, pool: &PoolName, part: Option<u32>) -> SpineError {
        SpineError::refuse(self.witness(pool, part))
    }
}

/// Read the frozen machine tag back off a grammar witness: the message
/// prefix of a `GrammarViolation`, or the fixed tag of one of the
/// structured cap-law variants (`over_cap`, `under_floor`,
/// `required_edge_missing`). `None` for any other witness variant or an
/// untagged message.
pub fn grammar_fault_tag(w: &Witness) -> Option<&'static str> {
    match w {
        Witness::GrammarViolation { message, .. } => {
            let head = message.split(':').next()?;
            GRAMMAR_FAULTS.iter().copied().find(|t| *t == head)
        }
        Witness::OverCap { .. } | Witness::EdgeFieldOverCap { .. } => Some("over_cap"),
        Witness::UnderFloor { .. } | Witness::EdgeFieldUnderFloor { .. } => Some("under_floor"),
        Witness::RequiredEdgeMissing { .. } => Some("required_edge_missing"),
        _ => None,
    }
}

/// What a delta-validation pass may consult about nodes that already exist in
/// the pool: a node identifier to its stored node type. Nodes CREATEd by the
/// same part are resolved from the part itself and never reach this.
///
/// It is a closure, not a store handle, deliberately: grammar validation stays
/// pure and runs identically at compile time (against the match set) and at
/// landing time (against the pool's own `nodes` table).
pub type TypeResolver<'a> = dyn Fn(&NodeId) -> Option<String> + 'a;

/// The FAR side of a cross-pool edge, as the one validator may consult it.
/// Two callers, two windows:
pub enum FarSide<'a> {
    /// At the DOOR: the cauldron answers for every pool it holds — the far
    /// pool's pinned grammar (kind, version, declared types) and the far
    /// node's ACTUAL type as the far pool resolves it (matched or minted).
    /// The projected namespace answers through its code-defined grammar
    /// whether or not `grammar` knows it.
    Door {
        grammar: &'a dyn Fn(&PoolName) -> Option<PoolGrammar>,
        node_type: &'a dyn Fn(&QualifiedNodeId) -> Option<String>,
        /// Does ANY pool of this kind, at its pinned version, declare this
        /// node type? A reach is by KIND and "covers every pool of that
        /// kind", and two same-kind pools may carry different type sets (the
        /// fixtures pin ws-a at 3 and ws-b at 5 deliberately) — so a
        /// declaration written ahead of an older pool is judged against the
        /// KIND, not against the one pool the edge happens to aim at.
        /// The projected `spine` kind answers
        /// through its code-defined grammar inside the validator.
        declared_by_kind: &'a dyn Fn(&PoolKind, &str) -> bool,
    },
    /// At a POOL LANDING: one pool's landing transaction has no window onto
    /// another file (no ATTACH). The far half is not
    /// re-judged; instead the part must CARRY the door's record of the far
    /// pin it validated under (`CompiledPart::far_versions`), and a
    /// cross-pool edge without one refuses `far_unvalidated`.
    Landing,
}

/// Which prefixes the id-prefix rule ([`PoolGrammar::id_prefix_fault`])
/// admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdPrefixRule {
    /// The type's own `id_prefix` only — a `CREATE`'s caller id (compile's
    /// `caller_id_against_prefix`): a CREATE's id is a MINT, and the store
    /// mints under `id_prefix` alone.
    MintOnly,
    /// The type's own `id_prefix`, or its LEGACY import prefix
    /// (`grammar_v6::LEGACY_IMPORT_PREFIXES`) — every id inside a PART, at
    /// the door and at the landing alike. Sound because the only producer
    /// of a CreateNode id that is not a store mint is the `IMPORT` form
    /// (`emit_create_node` refuses every caller `id`, and the store mints
    /// under `id_prefix` alone), so a legacy prefix in a part was CARRIED
    /// through IMPORT by construction — the landing needs no second record
    /// of it, and the delta vocabulary does not grow.
    OrLegacyImport,
}

/// A node whose id was CARRIED (an `IMPORT`'s), not minted, as a
/// part can tell: a PREFIXED type whose id is not the door's provisional
/// placeholder. At the door every mint of a prefixed type is provisional
/// (`<P>-~hex`, the flip numbers it), so a non-provisional id there is a
/// carried one; at the landing (post-flip) a mint is serial too, but a
/// part that relied on this at the landing passed the same judgment at the
/// door first.
/// One part's edges and whole-field writes, indexed by node: what the
/// required-at-mint checks read for each minted node.
struct PartIndex<'a> {
    /// (source, kind) -> each add (`true`) or remove (`false`) of an edge of
    /// that kind leaving that source, toward its far end, in delta order.
    edges: std::collections::HashMap<(&'a NodeId, &'a str), Vec<(bool, &'a QualifiedNodeId)>>,
    /// Node -> the top-level fields its CREATE carries or the part SETs whole.
    fields: std::collections::HashMap<&'a NodeId, BTreeSet<&'a str>>,
}

impl<'a> PartIndex<'a> {
    fn of(part: &'a CompiledPart) -> PartIndex<'a> {
        let mut ix = PartIndex { edges: Default::default(), fields: Default::default() };
        for d in &part.deltas {
            match d {
                Delta::AddEdge { src, kind, dst, .. } => ix.edges.entry((src, kind.as_str())).or_default().push((true, dst)),
                Delta::RemoveEdge { src, kind, dst } => ix.edges.entry((src, kind.as_str())).or_default().push((false, dst)),
                Delta::CreateNode { id, fields, .. } => ix.fields.entry(id).or_default().extend(fields.keys().map(String::as_str)),
                Delta::SetField { node, path, .. } if path.len() == 1 => {
                    ix.fields.entry(node).or_default().insert(path[0].as_str());
                }
                _ => {}
            }
        }
        ix
    }
}

fn carried_in_part(tdef: &NodeTypeDef, id: &NodeId) -> bool {
    tdef.id_prefix.is_some() && crate::delta::provisional_prefix(id).is_none()
}

/// The import ISSUE record's key inside `meta.imported_from`
/// (`migrate::v5::RECORD_KEY`): one entry
/// per v6 required-at-mint edge the v5 source NEVER carried —
/// `{kind: "required_edge_missing", edge_kind, min, had: 0}`.
pub const IMPORT_ISSUES_KEY: &str = "issues";
/// The issue entry's `kind` for a never-had required-at-mint edge.
pub const ISSUE_REQUIRED_EDGE_MISSING: &str = "required_edge_missing";

/// Does the minted node `id` of `part` DECLARE, in its own
/// `meta.imported_from.issues`, that its source never had a `kind` edge?
fn declares_never_had(part: &CompiledPart, id: &NodeId, kind: &str) -> bool {
    part.deltas.iter().any(|d| match d {
        Delta::CreateNode { id: n, fields, .. } if n == id => fields
            .get("meta")
            .and_then(|m| m.get(crate::migrate::v5::RECORD_KEY))
            .and_then(|r| r.get(IMPORT_ISSUES_KEY))
            .and_then(|v| v.as_array())
            .is_some_and(|issues| {
                issues.iter().any(|i| {
                    i.get("kind").and_then(|k| k.as_str()) == Some(ISSUE_REQUIRED_EDGE_MISSING)
                        && i.get("edge_kind").and_then(|k| k.as_str()) == Some(kind)
                        && i.get("had").and_then(|h| h.as_u64()) == Some(0)
                })
            }),
        _ => false,
    })
}

impl PoolGrammar {
    pub fn node_type_def(&self, name: &str) -> Option<&NodeTypeDef> {
        self.node_types.get(name)
    }

    /// The ONE id-prefix rule (with its legacy import arm): `None` when `id`
    /// carries `<prefix>-<rest>` (rest non-empty) for the type's own
    /// `id_prefix` — or, under [`IdPrefixRule::OrLegacyImport`], for its
    /// legacy import prefix — else the `id_prefix_mismatch` fault naming the
    /// prefix the type MINTS. A type with no prefix (or not declared) judges
    /// nothing here.
    pub fn id_prefix_fault(&self, node_type: &str, id: &NodeId, rule: IdPrefixRule) -> Option<GrammarFault> {
        let p = self.node_types.get(node_type)?.id_prefix.as_deref()?;
        let carries = |p: &str| {
            id.0.strip_prefix(p)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|rest| !rest.is_empty())
        };
        let legacy = match rule {
            IdPrefixRule::MintOnly => None,
            IdPrefixRule::OrLegacyImport => {
                crate::grammar_v6::legacy_import_prefix(self.kind.as_str(), node_type)
            }
        };
        if carries(p) || legacy.is_some_and(carries) {
            return None;
        }
        Some(GrammarFault::new(
            "id_prefix_mismatch",
            format!("node type `{node_type}` mints ids `{p}-<n>`; `{id}` does not carry that prefix"),
        ))
    }

    pub fn edge_kind_def(&self, name: &str) -> Option<&EdgeKindDef> {
        self.edge_kinds.get(name)
    }

    /// Is the slice of `edge_kind` edges in `direction` a SCALAR — at most one
    /// edge (link mode forbidden on it, every membership change is
    /// a slice WRITE, reassignment serializes). `Out` reads the per-`from`
    /// constraint, `In` the per-`to` one.
    pub fn is_scalar_slice(&self, edge_kind: &str, direction: Direction) -> bool {
        match self.edge_kinds.get(edge_kind) {
            None => false,
            Some(k) => match direction {
                Direction::Out => k.at_most_one_per_from,
                Direction::In => k.at_most_one_per_to,
            },
        }
    }

    /// The count cells that an edge of `edge_kind` touches at the node on the
    /// `direction` end, given that node's type (the cells
    /// are GRAMMAR-DECLARED and increments are derived MECHANICALLY here —
    /// never a caller-spoken delta). `Out` = cells on the edge's source node,
    /// `In` = cells on its destination node.
    pub fn count_cells_for(
        &self,
        node_type: &str,
        edge_kind: &str,
        direction: Direction,
    ) -> Vec<&str> {
        match self.node_types.get(node_type) {
            None => Vec::new(),
            Some(t) => t
                .count_cells
                .iter()
                .filter(|(_, c)| c.edge_kind == edge_kind && c.direction == direction)
                .map(|(name, _)| name.as_str())
                .collect(),
        }
    }

    /// The grammar's own coherence — run once, at pool create, before the pin
    /// is written. A grammar that names types it does not declare, or declares
    /// a count cell over an edge kind that does not exist, or puts an
    /// at-most-one constraint on a REACH endpoint (where this pool cannot see
    /// the slice, let alone hold it still), or reaches toward the projected
    /// kind with anything but its one node type, is malformed and is refused
    /// rather than pinned.
    pub fn check_wellformed(&self) -> Result<()> {
        let bad = |detail: String| -> SpineError {
            GrammarFault::new("malformed_grammar", detail).refusal(&self.pool, None)
        };
        if !self.pool.is_well_formed() {
            return Err(bad(format!("pool name `{}` is not [a-z][a-z0-9-]*", self.pool)));
        }
        if self.pool.is_reserved() {
            return Err(bad(format!(
                "`{}` is the reserved projected namespace",
                self.pool
            )));
        }
        if !self.kind.is_well_formed() {
            return Err(bad(format!("pool kind `{}` is not [a-z][a-z0-9-]*", self.kind)));
        }
        if self.kind.is_reserved() {
            return Err(bad(format!(
                "kind `{}` is the projected namespace's own; its grammar is code",
                self.kind
            )));
        }
        if self.version == 0 {
            return Err(bad("pinned schema version must be >= 1".to_string()));
        }
        let mut seen_prefixes: BTreeMap<&str, &str> = BTreeMap::new();
        for (tname, tdef) in &self.node_types {
            for (f, fdef) in &tdef.fields {
                if MACHINE_OWNED_FIELDS.contains(&f.as_str()) || f == "state" {
                    return Err(bad(format!(
                        "node type `{tname}` declares the machine-owned or reserved field `{f}`"
                    )));
                }
                if tdef.count_cells.contains_key(f) {
                    return Err(bad(format!(
                        "node type `{tname}` declares `{f}` as both a field and a count cell"
                    )));
                }
                // A character cap or floor is a count of characters, and
                // only text has them: a cap on an integer or an
                // object would need a meter this grammar does not define.
                if (fdef.max_chars.is_some() || fdef.min_chars.is_some())
                    && fdef.kind != FieldKind::Text
                {
                    return Err(bad(format!(
                        "field `{tname}.{f}` carries a character cap or floor but is kind `{}`; caps bind text only",
                        fdef.kind.name()
                    )));
                }
                if let (Some(lo), Some(hi)) = (fdef.min_chars, fdef.max_chars) {
                    if lo > hi {
                        return Err(bad(format!(
                            "field `{tname}.{f}` has floor {lo} above cap {hi}; nothing could satisfy it"
                        )));
                    }
                }
            }
            // The id prefix: non-empty, no `-` of its own (the `-` is the
            // separator the id shape puts after it), and unique across the
            // grammar — a prefix names ONE type.
            if let Some(p) = &tdef.id_prefix {
                if p.is_empty() || p.contains('-') {
                    return Err(bad(format!(
                        "node type `{tname}` declares the id prefix `{p}`; a prefix is non-empty and carries no `-`"
                    )));
                }
                if let Some(other) = seen_prefixes.insert(p.as_str(), tname.as_str()) {
                    return Err(bad(format!(
                        "node types `{other}` and `{tname}` both declare the id prefix `{p}`; a prefix names one type"
                    )));
                }
            }
            // Required-at-mint edges: a declared kind, one this type may
            // leave, with a minimum of at least one (zero would require
            // nothing and read as coverage).
            for (kname, minimum) in &tdef.required_edges_at_mint {
                let Some(kind) = self.edge_kinds.get(kname) else {
                    return Err(bad(format!(
                        "node type `{tname}` requires unknown edge kind `{kname}` at mint"
                    )));
                };
                if !kind.leaves(tname) {
                    return Err(bad(format!(
                        "node type `{tname}` requires `{kname}` at mint, but that kind does not leave a `{tname}`"
                    )));
                }
                if *minimum == 0 {
                    return Err(bad(format!(
                        "node type `{tname}` requires zero `{kname}` edges at mint; a minimum is at least one"
                    )));
                }
            }
            // One-of required-at-mint: every alternative demands
            // something, names only declared kinds this type leaves (with a
            // minimum of at least one) and only fields this type declares.
            for (i, alt) in tdef.required_at_mint_any_of.iter().enumerate() {
                if alt.edges.is_empty() && alt.fields.is_empty() {
                    return Err(bad(format!(
                        "node type `{tname}`'s one-of alternative {i} demands nothing; it would admit every mint"
                    )));
                }
                for (kname, minimum) in &alt.edges {
                    let Some(kind) = self.edge_kinds.get(kname) else {
                        return Err(bad(format!(
                            "node type `{tname}`'s one-of alternative {i} requires unknown edge kind `{kname}`"
                        )));
                    };
                    if !kind.leaves(tname) {
                        return Err(bad(format!(
                            "node type `{tname}`'s one-of alternative {i} requires `{kname}`, which does not leave a `{tname}`"
                        )));
                    }
                    if *minimum == 0 {
                        return Err(bad(format!(
                            "node type `{tname}`'s one-of alternative {i} requires zero `{kname}` edges; a minimum is at least one"
                        )));
                    }
                }
                for f in &alt.fields {
                    if !tdef.fields.contains_key(f) {
                        return Err(bad(format!(
                            "node type `{tname}`'s one-of alternative {i} requires undeclared field `{f}`"
                        )));
                    }
                }
            }
            let mut seen_states = BTreeSet::new();
            for s in &tdef.states {
                if !seen_states.insert(s) {
                    return Err(bad(format!(
                        "node type `{tname}` repeats the state `{s}` in its vocabulary"
                    )));
                }
            }
            for (cname, cell) in &tdef.count_cells {
                let Some(kind) = self.edge_kinds.get(&cell.edge_kind) else {
                    return Err(bad(format!(
                        "count cell `{tname}.{cname}` counts unknown edge kind `{}`",
                        cell.edge_kind
                    )));
                };
                // A cell counts edges at a LOCAL endpoint of the declaring
                // type. `Out` is the source (always local; the type must be
                // one the kind leaves); `In` needs the kind's far endpoint
                // set to be exactly {local tname}.
                let ok = match cell.direction {
                    Direction::Out => kind.leaves(tname),
                    Direction::In => {
                        kind.to.len() == 1 && kind.to.contains(&TypeRef::local(tname.as_str()))
                    }
                };
                if !ok {
                    return Err(bad(format!(
                        "count cell `{tname}.{cname}` counts `{}` edges in a direction whose endpoint is not exactly a local `{tname}`",
                        cell.edge_kind
                    )));
                }
            }
        }
        for (kname, kind) in &self.edge_kinds {
            // The SOURCE of an edge is the node whose record the edge hangs
            // off; it is always local to the pool that stores the edge.
            if kind.from.is_empty() {
                return Err(bad(format!(
                    "edge kind `{kname}` leaves no node type at all"
                )));
            }
            for f in &kind.from {
                let TypeRef::Local { node_type: from } = f else {
                    return Err(bad(format!(
                        "edge kind `{kname}` declares a reach or same-as-source as a `from` endpoint; the source of an edge is always a local type"
                    )));
                };
                if !self.node_types.contains_key(from) {
                    return Err(bad(format!(
                        "edge kind `{kname}` leaves unknown node type `{from}`"
                    )));
                }
            }
            if kind.to.is_empty() {
                return Err(bad(format!(
                    "edge kind `{kname}` permits no far endpoint at all"
                )));
            }
            // Edge fields: the node-field rules, unchanged — a cap
            // or floor binds text only, and a floor above its cap admits
            // nothing.
            for (f, fdef) in &kind.fields {
                if f.is_empty() {
                    return Err(bad(format!("edge kind `{kname}` declares a field with no name")));
                }
                if (fdef.max_chars.is_some() || fdef.min_chars.is_some())
                    && fdef.kind != FieldKind::Text
                {
                    return Err(bad(format!(
                        "edge field `{kname}.{f}` carries a character cap or floor but is kind `{}`; caps bind text only",
                        fdef.kind.name()
                    )));
                }
                if let (Some(lo), Some(hi)) = (fdef.min_chars, fdef.max_chars) {
                    if lo > hi {
                        return Err(bad(format!(
                            "edge field `{kname}.{f}` has floor {lo} above cap {hi}; nothing could satisfy it"
                        )));
                    }
                }
            }
            for t in &kind.to {
                match t {
                    TypeRef::SameAsSource => {}
                    TypeRef::Local { node_type } => {
                        if !self.node_types.contains_key(node_type) {
                            return Err(bad(format!(
                                "edge kind `{kname}` arrives at unknown node type `{node_type}`"
                            )));
                        }
                    }
                    TypeRef::Reach { kind: far, node_type } => {
                        if !far.is_well_formed() {
                            return Err(bad(format!(
                                "edge kind `{kname}` reaches toward malformed kind `{far}`"
                            )));
                        }
                        if far.is_reserved() && node_type != crate::project::TXN_NODE_TYPE {
                            return Err(bad(format!(
                                "edge kind `{kname}` reaches toward the projected kind `{far}` with type `{node_type}`; the projection has exactly one node type, `{}`",
                                crate::project::TXN_NODE_TYPE
                            )));
                        }
                        if kind.at_most_one_per_to {
                            return Err(bad(format!(
                                "edge kind `{kname}` puts at-most-one on a REACH endpoint ({far}/{node_type}); that slice lives in another pool and this pool cannot hold it still"
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Validate ONE compiled part against this pool's pinned grammar — the
    /// whole per-pool, version-pinned check (the door validates
    /// every part of a statement against the touched pool's own grammar).
    ///
    /// `part_ordinal` is carried into the witness so the poisoned variants
    /// can name which part failed. `known` resolves the node type of rows that
    /// already exist in this pool; nodes CREATEd by this part resolve from the
    /// part itself. `far` is the window onto OTHER pools for cross-pool edges
    /// (see [`FarSide`]).
    ///
    /// This does NOT check at-most-one occupancy (that is stored state, not
    /// grammar — [`crate::pool::Pool::apply_part`] enforces it inside the
    /// landing transaction).
    pub fn validate_part(
        &self,
        part: &CompiledPart,
        part_ordinal: Option<u32>,
        known: &TypeResolver<'_>,
        far: &FarSide<'_>,
    ) -> Result<()> {
        let fail = |f: GrammarFault| -> SpineError { f.refusal(&self.pool, part_ordinal) };

        if part.pool != self.pool {
            return Err(fail(GrammarFault::new(
                "wrong_pool",
                format!(
                    "part addresses pool `{}` but this grammar pins `{}`",
                    part.pool, self.pool
                ),
            )));
        }
        // VERSION-PINNED: the part's validated-under stamp is what the row
        // records forever, so it must be the pin it actually
        // validated against — never a neighbour pool's number.
        if part.schema_version != self.version {
            return Err(fail(GrammarFault::new(
                "version_mismatch",
                format!(
                    "part stamped validated-under {} but pool `{}` is pinned at {}",
                    part.schema_version, self.pool, self.version
                ),
            )));
        }

        // Types minted by this part itself, so ordering inside a part works.
        let mut minted: BTreeMap<&NodeId, &str> = BTreeMap::new();
        for d in &part.deltas {
            if let Delta::CreateNode { id, node_type, .. } = d {
                minted.insert(id, node_type.as_str());
            }
        }
        let type_of = |id: &NodeId| -> Option<String> {
            minted
                .get(id)
                .map(|t| t.to_string())
                .or_else(|| known(id))
        };

        for d in &part.deltas {
            self.validate_delta(d, &type_of, known, far, &part.far_versions)
                .map_err(|f| fail(f))?;
        }

        // The part's edges by (source, kind) and its whole-field writes by
        // node, indexed once: the checks below ask about every minted node,
        // and a scan of the whole part per node would grow with the square
        // of the part.
        let ix = PartIndex::of(part);

        // REQUIRED-AT-MINT EDGES: a part-level fact, judged after
        // every delta has passed on its own — the minted node and the edges
        // leaving it are in THIS part or they are nowhere, because an edge
        // hangs off its source and lands in the source's pool. Nothing
        // outside the part is read.
        for d in &part.deltas {
            let Delta::CreateNode { id, node_type, .. } = d else {
                continue;
            };
            let Some(tdef) = self.node_types.get(node_type) else {
                continue; // refused above as `unknown_node_type`
            };
            for (kname, minimum) in &tdef.required_edges_at_mint {
                // What LANDS, not what the part says. An edge is keyed
                // (src, kind, dst) and the landing stores one row per key,
                // so two CREATE clauses toward the same target are ONE edge,
                // and a REMOVE of that key later in the same
                // part takes it away again (an add-only
                // count let `CREATE n; CREATE (n)-[serves]->(b); REMOVE
                // (n)-[serves]->(b)` mint a note with zero `serves`). So the
                // part's adds AND removes for (minted id, kind) are applied
                // IN DELTA ORDER over a set keyed on dst — the shape of
                // `exec::projected_occupancy` — and the survivors counted.
                // The minted node has no edges outside this part, so the
                // set starts empty.
                let found = Self::surviving_edges(&ix, id, kname);
                if found < *minimum {
                    // THE IMPORT ISSUE: a CARRIED
                    // node whose v5 source never had this edge — it says so
                    // in `meta.imported_from.issues`, and the part indeed
                    // carries none — lands with the gap recorded rather
                    // than refused. A CREATE's node is a provisional mint at
                    // the door, so it never qualifies; a node with ANY such
                    // edge is judged exactly as before.
                    let waived = found == 0
                        && carried_in_part(tdef, id)
                        && declares_never_had(part, id, kname);
                    if !waived {
                        return Err(fail(GrammarFault::required_edge(
                            node_type, kname, *minimum, found,
                        )));
                    }
                }
            }
            if !tdef.required_at_mint_any_of.is_empty() {
                if let Some(f) = Self::one_of_shortfall(&ix, id, node_type, tdef) {
                    // The one-of's import issue: declared under ANY of its
                    // alternatives' edge kinds, and waived only when the
                    // part carries NO edge of ANY alternative's kinds.
                    let kinds: BTreeSet<&str> = tdef
                        .required_at_mint_any_of
                        .iter()
                        .flat_map(|a| a.edges.keys().map(String::as_str))
                        .collect();
                    let waived = carried_in_part(tdef, id)
                        && kinds.iter().all(|k| Self::surviving_edges(&ix, id, k) == 0)
                        && kinds.iter().any(|k| declares_never_had(part, id, k));
                    if !waived {
                        return Err(fail(f));
                    }
                }
            }
        }
        Ok(())
    }

    /// The edges of `kind` leaving `id` that SURVIVE `part`: its adds and
    /// removes applied in delta order over a set keyed on the target (the
    /// closes the add-then-remove hole). The minted node has no
    /// edges outside the part, so the set starts empty.
    fn surviving_edges(ix: &PartIndex<'_>, id: &NodeId, kind: &str) -> u32 {
        let mut survivors: BTreeSet<&QualifiedNodeId> = BTreeSet::new();
        for (add, dst) in ix.edges.get(&(id, kind)).map(Vec::as_slice).unwrap_or(&[]) {
            if *add {
                survivors.insert(dst);
            } else {
                survivors.remove(dst);
            }
        }
        u32::try_from(survivors.len()).unwrap_or(u32::MAX)
    }

    /// Judge [`NodeTypeDef::required_at_mint_any_of`] for the node `id`
    /// minted by `part`: `None` when some alternative holds in full, else the
    /// fault of the closest alternative (rule on [`MintAlternative`]).
    fn one_of_shortfall(
        ix: &PartIndex<'_>,
        id: &NodeId,
        node_type: &str,
        tdef: &NodeTypeDef,
    ) -> Option<GrammarFault> {
        // A field is present when the CREATE carries it or the same part
        // SETs it whole; no delta removes a field, so presence only grows.
        let has_field = |f: &str| ix.fields.get(id).is_some_and(|fs| fs.contains(f));
        let mut best: Option<(u64, GrammarFault)> = None;
        for alt in &tdef.required_at_mint_any_of {
            let mut progress: u64 = 0;
            let mut shortfall: Option<GrammarFault> = None;
            for (kname, minimum) in &alt.edges {
                let found = Self::surviving_edges(ix, id, kname);
                progress += u64::from(found.min(*minimum));
                if found < *minimum && shortfall.is_none() {
                    shortfall = Some(GrammarFault::required_edge(node_type, kname, *minimum, found));
                }
            }
            for f in &alt.fields {
                if has_field(f) {
                    progress += 1;
                } else if shortfall.is_none() {
                    shortfall = Some(GrammarFault::new(
                        "missing_required_field",
                        format!(
                            "a `{node_type}` minted under the alternative requiring {:?} must carry field `{f}`",
                            alt.edges
                        ),
                    ));
                }
            }
            let Some(fault) = shortfall else {
                return None; // this alternative holds in full
            };
            if best.as_ref().map_or(true, |(p, _)| progress > *p) {
                best = Some((progress, fault));
            }
        }
        best.map(|(_, f)| f)
    }

    /// One whole-field value against its declaration: the kind,
    /// then the cap and floor for text. ONE function for the CREATE's
    /// initial values and a bare-field SET, so the two writes cannot
    /// disagree about what a field accepts.
    fn check_field_value(
        &self,
        node_type: &str,
        field: &str,
        fdef: &FieldDef,
        value: &serde_json::Value,
    ) -> std::result::Result<(), GrammarFault> {
        if !fdef.kind.admits(value) {
            return Err(GrammarFault::new(
                "wrong_field_kind",
                format!(
                    "`{node_type}.{field}` is kind `{}`; the value written is {}",
                    fdef.kind.name(),
                    FieldKind::describe(value)
                ),
            ));
        }
        if let Some(s) = value.as_str() {
            let measured = measure_chars(s);
            if let Some(cap) = fdef.max_chars {
                if measured > cap {
                    return Err(GrammarFault::over_cap(node_type, field, measured, cap));
                }
            }
            if let Some(floor) = fdef.min_chars {
                if measured < floor {
                    return Err(GrammarFault::under_floor(node_type, field, measured, floor));
                }
            }
        }
        Ok(())
    }

    /// An edge create's fields against its kind's declaration:
    /// every field named must be declared, every declared-required field
    /// present, and every value judged by [`Self::check_field_value`] — the
    /// node fields' own function — with a cap-law fault re-owned by the
    /// edge kind.
    fn check_edge_fields(
        &self,
        kind: &str,
        kdef: &EdgeKindDef,
        fields: &BTreeMap<String, serde_json::Value>,
    ) -> std::result::Result<(), GrammarFault> {
        for (f, value) in fields {
            let Some(fdef) = kdef.fields.get(f) else {
                return Err(GrammarFault::new(
                    "undeclared_edge_field",
                    format!("edge kind `{kind}` declares no field `{f}`"),
                ));
            };
            self.check_field_value(kind, f, fdef, value)
                .map_err(GrammarFault::on_edge)?;
        }
        for (f, fdef) in &kdef.fields {
            if fdef.required && !fields.contains_key(f) {
                return Err(GrammarFault::new(
                    "missing_required_edge_field",
                    format!("edge kind `{kind}` requires field `{f}` on every edge"),
                ));
            }
        }
        Ok(())
    }

    /// One delta against this grammar. Returns the raw fault so the caller
    /// decides which pool/part it is stamped with (a far-pool fault carries
    /// its own stamp — see [`GrammarFault::in_pool`]).
    fn validate_delta(
        &self,
        delta: &Delta,
        type_of: &dyn Fn(&NodeId) -> Option<String>,
        known: &TypeResolver<'_>,
        far: &FarSide<'_>,
        far_versions: &BTreeMap<PoolName, u32>,
    ) -> std::result::Result<(), GrammarFault> {
        match delta {
            Delta::CreateNode {
                id,
                node_type,
                state,
                fields,
            } => {
                if known(id).is_some() {
                    return Err(GrammarFault::new(
                        "duplicate_node_id",
                        format!("node `{id}` already exists in pool `{}`", self.pool),
                    ));
                }
                let Some(tdef) = self.node_types.get(node_type) else {
                    return Err(GrammarFault::new(
                        "unknown_node_type",
                        format!("pool `{}` declares no node type `{node_type}`", self.pool),
                    ));
                };
                match (state, tdef.states.is_empty()) {
                    (Some(s), true) => {
                        return Err(GrammarFault::new(
                            "state_not_declared",
                            format!("node type `{node_type}` carries no state vocabulary; `{s}` was addressed"),
                        ))
                    }
                    (Some(s), false) if !tdef.states.contains(s) => {
                        return Err(GrammarFault::new(
                            "unknown_state",
                            format!("`{s}` is outside `{node_type}`'s closed state vocabulary"),
                        ))
                    }
                    _ => {}
                }
                // The id prefix: `<prefix>-<rest>`, rest non-empty.
                // The serial shape of the rest is judged elsewhere; this judges the
                // prefix and nothing more. A PART may carry the type's legacy
                // import prefix (`IdPrefixRule::OrLegacyImport`):
                // nothing but `IMPORT` puts a non-minted id into a part.
                if let Some(f) = self.id_prefix_fault(node_type, id, IdPrefixRule::OrLegacyImport) {
                    return Err(f);
                }
                // A PROVISIONAL id (`<P>-~hex`, `delta::PROVISIONAL_MARK`) is
                // the door's placeholder; the decided flip rewrites every one
                // to its serial (`CompiledPayload::assign_ids`) before any
                // pool lands. One reaching a LANDING never went through the
                // flip, and landing it would store a node under an id no
                // serial counter accounts for. The door validates the
                // pre-flip payload, so this judges the landing only — and
                // AFTER the prefix rule, so a wrong prefix still answers
                // `id_prefix_mismatch` (`caller_id_against_prefix` reads it).
                if matches!(far, FarSide::Landing) && crate::delta::provisional_prefix(id).is_some() {
                    return Err(GrammarFault::new(
                        "provisional_id_at_landing",
                        format!(
                            "node `{id}` reached the landing with a provisional id; the decided flip assigns its serial before any pool lands"
                        ),
                    ));
                }
                for (f, value) in fields {
                    self.check_field_name(node_type, tdef, f)?;
                    // Declared, by the check above.
                    if let Some(fdef) = tdef.fields.get(f) {
                        self.check_field_value(node_type, f, fdef, value)?;
                    }
                }
                for (f, fdef) in &tdef.fields {
                    if fdef.required && !fields.contains_key(f) {
                        return Err(GrammarFault::new(
                            "missing_required_field",
                            format!("node type `{node_type}` requires field `{f}` at creation"),
                        ));
                    }
                }
                Ok(())
            }
            Delta::SetField { node, path, value } => {
                let Some(first) = path.first() else {
                    return Err(GrammarFault::new(
                        "empty_path",
                        format!("SET on node `{node}` carries an empty field path"),
                    ));
                };
                let node_type = self.resolve_type(node, type_of)?;
                let tdef = self
                    .node_types
                    .get(&node_type)
                    .ok_or_else(|| GrammarFault::new(
                        "unknown_node_type",
                        format!("stored node `{node}` has type `{node_type}`, which pool `{}` does not declare", self.pool),
                    ))?;
                if first == "state" {
                    if path.len() != 1 {
                        return Err(GrammarFault::new(
                            "undeclared_field",
                            format!("`state` is a scalar; `{}` addresses into it", path.join(".")),
                        ));
                    }
                    if tdef.states.is_empty() {
                        return Err(GrammarFault::new(
                            "state_not_declared",
                            format!("node type `{node_type}` carries no state vocabulary"),
                        ));
                    }
                    let Some(s) = value.as_str() else {
                        return Err(GrammarFault::new(
                            "unknown_state",
                            format!("state must be a string; got `{value}`"),
                        ));
                    };
                    if !tdef.states.contains(&s.to_string()) {
                        return Err(GrammarFault::new(
                            "unknown_state",
                            format!("`{s}` is outside `{node_type}`'s closed state vocabulary"),
                        ));
                    }
                    return Ok(());
                }
                self.check_field_name(&node_type, tdef, first)?;
                // Declared, by the check above.
                let Some(fdef) = tdef.fields.get(first) else {
                    return Ok(());
                };
                if path.len() == 1 {
                    // A whole-field write: the kind, the cap, the floor.
                    return self.check_field_value(&node_type, first, fdef, value);
                }
                // A dotted SET addresses THROUGH the field's value. Only a
                // kind that has keys can be addressed into; a text field
                // written this way would be destroyed (`not_an_object` at
                // the landing) or, when absent, minted as an object inside
                // a field declared to hold text.
                if !fdef.kind.addressable() {
                    return Err(GrammarFault::new(
                        "wrong_field_kind",
                        format!(
                            "`{}` addresses into `{node_type}.{first}`, which is kind `{}` and has no keys",
                            path.join("."),
                            fdef.kind.name()
                        ),
                    ));
                }
                Ok(())
            }
            Delta::Seal { node } => {
                // The flag is machine-owned and takes no value: the only
                // grammar fact is that the node exists in this pool.
                self.resolve_type(node, type_of)?;
                Ok(())
            }
            Delta::AddEdge {
                src, kind, dst, ..
            }
            | Delta::RemoveEdge { src, kind, dst } => {
                let Some(kdef) = self.edge_kinds.get(kind) else {
                    return Err(GrammarFault::new(
                        "unknown_edge_kind",
                        format!("pool `{}` declares no edge kind `{kind}`", self.pool),
                    ));
                };
                if let Delta::AddEdge { fields, .. } = delta {
                    self.check_edge_fields(kind, kdef, fields)?;
                }
                let src_type = self.resolve_type(src, type_of)?;
                if !kdef.leaves(&src_type) {
                    return Err(GrammarFault::new(
                        "wrong_endpoint_type",
                        format!(
                            "edge kind `{kind}` leaves {} but node `{src}` is a `{src_type}`",
                            describe_sources(kdef)
                        ),
                    ));
                }
                if dst.pool == self.pool {
                    // LOCAL far endpoint: the type IS this pool's business.
                    // Permitted by a LOCAL entry, by a reach toward this
                    // pool's own kind ("the declaring pool included"), or by
                    // SAME-AS-SOURCE when the far node's type is the source's.
                    let dst_type = self.resolve_type(&dst.id, type_of)?;
                    if !kdef.permits(&self.kind, &dst_type, true, &src_type) {
                        return Err(GrammarFault::new(
                            "wrong_endpoint_type",
                            format!(
                                "edge kind `{kind}` arrives at {} but node `{}` is a local `{dst_type}`",
                                describe_endpoints(kdef),
                                dst.id
                            ),
                        ));
                    }
                    return Ok(());
                }
                self.validate_reach(kind, kdef, dst, far, far_versions)
            }
        }
    }

    /// The cross-pool half of edge validation: the SOURCE's
    /// outbound declaration, then the TARGET's own pinned grammar, by lookup.
    fn validate_reach(
        &self,
        kind: &str,
        kdef: &EdgeKindDef,
        dst: &QualifiedNodeId,
        far: &FarSide<'_>,
        far_versions: &BTreeMap<PoolName, u32>,
    ) -> std::result::Result<(), GrammarFault> {
        // LOCAL still means this pool only: a kind with no reach at all can
        // never point out of its pool, whatever the far pool is.
        if !kdef.reaches_across() {
            return Err(GrammarFault::new(
                "reach_undeclared",
                format!(
                    "edge kind `{kind}` arrives at {} in this pool only; `{dst}` is in pool `{}`",
                    describe_endpoints(kdef),
                    dst.pool
                ),
            ));
        }
        let (grammar, node_type, declared_by_kind) = match far {
            FarSide::Landing => {
                // No window onto another file at a landing. The
                // door's record on the part is what the landing asks for.
                return match far_versions.get(&dst.pool) {
                    Some(_) => Ok(()),
                    None => Err(GrammarFault::new(
                        "far_unvalidated",
                        format!(
                            "edge kind `{kind}` points into pool `{}` but the part records no far pin it validated under; this part did not come through the door's cross-pool check",
                            dst.pool
                        ),
                    )),
                };
            }
            FarSide::Door {
                grammar,
                node_type,
                declared_by_kind,
            } => (grammar, node_type, declared_by_kind),
        };

        // PART ONE — the other pool's grammar is already known: look it up.
        // The projected namespace has a code-defined grammar (the origin
        // of these nodes is code).
        let far_grammar = if crate::project::is_projected(&dst.pool) {
            Some(crate::project::projected_grammar())
        } else {
            (grammar)(&dst.pool)
        };
        let Some(far_grammar) = far_grammar else {
            return Err(GrammarFault::new(
                "foreign_pool_unknown",
                format!(
                    "edge kind `{kind}` points into pool `{}`, which this cauldron does not hold",
                    dst.pool
                ),
            ));
        };
        // The far node must EXIST there, as the far pool resolves it — a
        // refusal stamped with the FAR pool, whose grammar is the one refusing.
        let Some(far_type) = (node_type)(dst) else {
            return Err(GrammarFault::new(
                "unknown_node",
                format!("pool `{}` holds no node `{}`", dst.pool, dst.id),
            )
            .in_pool(&dst.pool));
        };
        // PART TWO — against the SOURCE's declaration, by the far pool's KIND.
        let toward_kind: Vec<&str> = kdef
            .reaches()
            .filter(|(k, _)| **k == far_grammar.kind)
            .map(|(_, t)| t)
            .collect();
        if toward_kind.is_empty() {
            return Err(GrammarFault::new(
                "reach_undeclared",
                format!(
                    "edge kind `{kind}` declares no reach toward kind `{}`; `{dst}` is a `{far_type}` in pool `{}` of that kind",
                    far_grammar.kind, dst.pool
                ),
            ));
        }
        // Against the pins of the far KIND: every type this reach names
        // toward that kind must be one SOME pool of that kind actually
        // declares at its pinned version — "check the schema version on
        // it". A declaration stale against every pin of the kind refuses
        // here, loudly, rather than only when the dead type is aimed at
        // (an interior choice). What it is NOT judged against is the
        // ONE far pool the edge aims at: a reach "covers every pool of that kind", and two same-kind
        // pools may carry different type sets, so a declaration written
        // ahead of the older pool — naming a type only the newer one has —
        // is a forward-compatible declaration, not a stale one, and an edge
        // aimed at a type the older pool DOES declare lands. The far pool
        // itself is consulted first (it is of the kind), then the kind.
        for t in &toward_kind {
            if !far_grammar.node_types.contains_key(*t) && !(declared_by_kind)(&far_grammar.kind, t) {
                return Err(GrammarFault::new(
                    "foreign_type_unknown",
                    format!(
                        "edge kind `{kind}` reaches toward `{}/{t}` but no pool of kind `{}` declares a node type `{t}` at its pinned version (`{}` is pinned at {})",
                        far_grammar.kind, far_grammar.kind, dst.pool, far_grammar.version
                    ),
                ));
            }
        }
        // The far node's ACTUAL type must be one the reach names.
        if !toward_kind.iter().any(|t| *t == far_type) {
            return Err(GrammarFault::new(
                "reach_undeclared",
                format!(
                    "edge kind `{kind}` reaches toward kind `{}` types [{}]; `{dst}` in pool `{}` is a `{far_type}`",
                    far_grammar.kind,
                    toward_kind.join(", "),
                    dst.pool
                ),
            ));
        }
        Ok(())
    }

    fn resolve_type(
        &self,
        id: &NodeId,
        type_of: &dyn Fn(&NodeId) -> Option<String>,
    ) -> std::result::Result<String, GrammarFault> {
        type_of(id).ok_or_else(|| {
            GrammarFault::new(
                "unknown_node",
                format!("pool `{}` holds no node `{id}`", self.pool),
            )
        })
    }

    fn check_field_name(
        &self,
        node_type: &str,
        tdef: &NodeTypeDef,
        field: &str,
    ) -> std::result::Result<(), GrammarFault> {
        if MACHINE_OWNED_FIELDS.contains(&field) {
            return Err(GrammarFault::new(
                "machine_owned_field",
                format!("`{field}` is machine-owned; no pen ever sets it"),
            ));
        }
        if tdef.count_cells.contains_key(field) {
            return Err(GrammarFault::new(
                "machine_owned_cell",
                format!(
                    "`{node_type}.{field}` is a machine-maintained count cell; it is derived inside the landing transaction, never spoken"
                ),
            ));
        }
        if !tdef.fields.contains_key(field) {
            return Err(GrammarFault::new(
                "undeclared_field",
                format!("node type `{node_type}` declares no field `{field}`"),
            ));
        }
        Ok(())
    }
}

/// The far endpoints of an edge kind, for refusal detail.
fn describe_endpoints(kdef: &EdgeKindDef) -> String {
    let parts: Vec<String> = kdef
        .to
        .iter()
        .map(|t| match t {
            TypeRef::Local { node_type } => format!("local `{node_type}`"),
            TypeRef::Reach { kind, node_type } => format!("reach `{kind}/{node_type}`"),
            TypeRef::SameAsSource => "a local node of the source's own type".to_string(),
        })
        .collect();
    parts.join(" | ")
}

/// The source types of an edge kind, for refusal detail.
fn describe_sources(kdef: &EdgeKindDef) -> String {
    let parts: Vec<String> = kdef
        .from
        .iter()
        .map(|t| match t.node_type() {
            Some(nt) => format!("`{nt}`"),
            None => "<malformed source>".to_string(),
        })
        .collect();
    parts.join(" | ")
}

/// The test fixture grammars — TEST FIXTURES, NOT THE REAL
/// SCHEMAS (which applies to the workspace and
/// blueprint fixtures exactly as to alpha and beta). They exist so the spine
/// can be proven against genuinely different grammars; do not derive
/// production shapes from them, do not name production things after them, do
/// not let a fixture shape harden into a production default. Their
/// particulars are illustrative; their existence is required.
/// Constructed data, not behavior — shared here so every suite exercises ONE
/// alpha, ONE beta, ONE workspace shape and ONE blueprint shape.
pub mod fixtures {
    use super::*;

    /// The two workspace instances and the blueprint of the cross-pool
    /// fixtures — the worked cases made real across
    /// pools: an issue in one workspace blocked by an issue in ANOTHER, and
    /// a milestone depending on a blueprint's migration or release.
    pub const WS_A: &str = "fixture-ws-a";
    pub const WS_B: &str = "fixture-ws-b";
    pub const BP: &str = "fixture-bp";

    /// Fixture schema ALPHA (pool `fixture-alpha`, kind `alpha`), pinned
    /// version 1 — shaped so the write-side machinery is exercised.
    pub fn fixture_alpha() -> PoolGrammar {
        let mut node_types = BTreeMap::new();
        node_types.insert(
            "doc".to_string(),
            NodeTypeDef {
                fields: BTreeMap::from([("title".to_string(), FieldDef::required())]),
                states: vec![
                    "draft".to_string(),
                    "published".to_string(),
                    "superseded".to_string(),
                ],
                count_cells: BTreeMap::new(),
                ..Default::default()
            },
        );
        node_types.insert(
            "author".to_string(),
            NodeTypeDef {
                fields: BTreeMap::from([("name".to_string(), FieldDef::required())]),
                states: vec![],
                // The commutative-increment path exercised, not asserted:
                // docs_written counts outgoing `wrote` edges.
                count_cells: BTreeMap::from([(
                    "docs_written".to_string(),
                    CountCellDef {
                        edge_kind: "wrote".to_string(),
                        direction: Direction::Out,
                    },
                )]),
                ..Default::default()
            },
        );
        let mut edge_kinds = BTreeMap::new();
        edge_kinds.insert("wrote".to_string(), EdgeKindDef::local("author", "doc"));
        edge_kinds.insert("supersedes".to_string(), EdgeKindDef::local("doc", "doc"));
        // The unique slice (scalar-slice reassignment): at most
        // one `current_of` edge may arrive at any one author.
        edge_kinds.insert(
            "current_of".to_string(),
            EdgeKindDef {
                at_most_one_per_to: true,
                ..EdgeKindDef::local("doc", "author")
            },
        );
        PoolGrammar {
            pool: PoolName::new("fixture-alpha"),
            kind: PoolKind::new("alpha"),
            version: 1,
            node_types,
            edge_kinds,
        }
    }

    /// Fixture schema BETA (pool `fixture-beta`, kind `beta`), pinned
    /// version 2 — DELIBERATELY a different version number than alpha, so
    /// the spine's version-blindness and per-part validated-under stamps are
    /// exercised by every cross-pool test. Shares no node type
    /// and no edge kind with alpha.
    pub fn fixture_beta() -> PoolGrammar {
        let mut node_types = BTreeMap::new();
        node_types.insert(
            "shelf".to_string(),
            NodeTypeDef {
                fields: BTreeMap::from([
                    ("label".to_string(), FieldDef::required()),
                    ("capacity".to_string(), FieldDef::optional()),
                ]),
                states: vec![],
                count_cells: BTreeMap::new(),
                ..Default::default()
            },
        );
        node_types.insert(
            "ticket".to_string(),
            NodeTypeDef {
                fields: BTreeMap::new(),
                states: vec!["open".to_string(), "closed".to_string()],
                count_cells: BTreeMap::new(),
                ..Default::default()
            },
        );
        let mut edge_kinds = BTreeMap::new();
        edge_kinds.insert("holds".to_string(), EdgeKindDef::local("shelf", "ticket"));
        // THE cross-pool edge kind: a REACH toward kind `alpha`, type `doc`
        // (reach by kind, never by pool name), because the
        // workspace-points-at-blueprint seam is what this stands in for.
        edge_kinds.insert(
            "catalogues".to_string(),
            EdgeKindDef::reaching("ticket", [TypeRef::reach("alpha", "doc")]),
        );
        PoolGrammar {
            pool: PoolName::new("fixture-beta"),
            kind: PoolKind::new("beta"),
            version: 2,
            node_types,
            edge_kinds,
        }
    }

    /// The default pin of the fixture workspace grammar — `fixture-ws-a`'s,
    /// and any hand-built workspace instance's that names no other.
    pub const WORKSPACE_PIN: u32 = 3;

    /// `fixture-ws-b`'s pin in [`cross_pool_grammars`] — DELIBERATELY not
    /// [`WORKSPACE_PIN`]. Two pools of ONE
    /// kind can sit at DIFFERENT schema versions: two real workspaces share a
    /// grammar SHAPE, but one may have been migrated ahead of the other, and
    /// each is pinned at its own version (the pin is per pool,
    /// never per kind). The far pin a cross-pool edge records must be the FAR
    /// pool's own, and with the two workspaces pinned alike a build that
    /// recorded the SOURCE's pin instead would have passed
    /// case (a) — the one about ANOTHER workspace — unnoticed.
    pub const WS_B_PIN: u32 = 5;

    /// Fixture WORKSPACE grammar (kind `workspace`) at the default pin,
    /// [`WORKSPACE_PIN`] — [`fixture_workspace_at`] with that version. The
    /// signature every existing caller uses.
    pub fn fixture_workspace(name: &str) -> PoolGrammar {
        fixture_workspace_at(name, WORKSPACE_PIN)
    }

    /// Fixture WORKSPACE grammar (kind `workspace`), pinned at `version` —
    /// one shape, instantiated as `fixture-ws-a` and `fixture-ws-b` so
    /// case (a) — an issue blocked by an issue in ANOTHER
    /// workspace — is real across two pools of one kind. The version is a
    /// parameter because two pools of one kind may be pinned differently
    /// (see [`WS_B_PIN`]): the shape is the kind's, the pin is the pool's.
    /// TEST FIXTURE, NOT THE PRODUCTION WORKSPACE SCHEMA.
    ///
    /// - `issue` (field `title`; states open/closed), `milestone` (field
    ///   `title`; states active/done).
    /// - `blocked_by`: issue -> reach {workspace, issue} — covers this
    ///   workspace's own issues too (one relationship, one declaration).
    /// - `depends_on`: milestone -> reach {blueprint, migration} and reach
    ///   {blueprint, release} — case (b). The blueprint's `module` is
    ///   deliberately NOT reachable.
    /// - `landed_by`: issue -> reach {spine, txn} — the ghost-edge case:
    ///   provenance toward the projected namespace.
    pub fn fixture_workspace_at(name: &str, version: u32) -> PoolGrammar {
        let mut node_types = BTreeMap::new();
        node_types.insert(
            "issue".to_string(),
            NodeTypeDef {
                fields: BTreeMap::from([("title".to_string(), FieldDef::required())]),
                states: vec!["open".to_string(), "closed".to_string()],
                count_cells: BTreeMap::new(),
                ..Default::default()
            },
        );
        node_types.insert(
            "milestone".to_string(),
            NodeTypeDef {
                fields: BTreeMap::from([("title".to_string(), FieldDef::required())]),
                states: vec!["active".to_string(), "done".to_string()],
                count_cells: BTreeMap::new(),
                ..Default::default()
            },
        );
        let mut edge_kinds = BTreeMap::new();
        edge_kinds.insert(
            "blocked_by".to_string(),
            EdgeKindDef::reaching("issue", [TypeRef::reach("workspace", "issue")]),
        );
        edge_kinds.insert(
            "depends_on".to_string(),
            EdgeKindDef::reaching(
                "milestone",
                [
                    TypeRef::reach("blueprint", "migration"),
                    TypeRef::reach("blueprint", "release"),
                ],
            ),
        );
        edge_kinds.insert(
            "landed_by".to_string(),
            EdgeKindDef::reaching(
                "issue",
                [TypeRef::reach(
                    crate::project::PROJECTED_KIND,
                    crate::project::TXN_NODE_TYPE,
                )],
            ),
        );
        PoolGrammar {
            pool: PoolName::new(name),
            kind: PoolKind::new("workspace"),
            version,
            node_types,
            edge_kinds,
        }
    }

    /// Fixture BLUEPRINT grammar (pool `fixture-bp`, kind `blueprint`),
    /// pinned version 4. Node types `migration`, `release`, and `module` —
    /// the last deliberately UNREACHABLE from the workspace fixture so a
    /// negative case exists. TEST FIXTURE, NOT THE PRODUCTION BLUEPRINT
    /// SCHEMA.
    pub fn fixture_blueprint() -> PoolGrammar {
        let plain = |states: &[&str]| NodeTypeDef {
            fields: BTreeMap::from([("name".to_string(), FieldDef::required())]),
            states: states.iter().map(|s| s.to_string()).collect(),
            count_cells: BTreeMap::new(),
            ..Default::default()
        };
        let mut node_types = BTreeMap::new();
        node_types.insert("migration".to_string(), plain(&["planned", "applied"]));
        node_types.insert("release".to_string(), plain(&["cut", "shipped"]));
        node_types.insert("module".to_string(), plain(&[]));
        PoolGrammar {
            pool: PoolName::new(BP),
            kind: PoolKind::new("blueprint"),
            version: 4,
            node_types,
            edge_kinds: BTreeMap::new(),
        }
    }

    /// The three pools the cross-pool suite creates together: two workspace
    /// instances plus one blueprint — at FOUR-WAY distinct pins with alpha
    /// and beta: ws-a at [`WORKSPACE_PIN`] (3), ws-b at [`WS_B_PIN`] (5), the
    /// blueprint at 4. The two workspaces differ ON PURPOSE: see
    /// [`WS_B_PIN`].
    pub fn cross_pool_grammars() -> Vec<PoolGrammar> {
        vec![
            fixture_workspace_at(WS_A, WORKSPACE_PIN),
            fixture_workspace_at(WS_B, WS_B_PIN),
            fixture_blueprint(),
        ]
    }
}
