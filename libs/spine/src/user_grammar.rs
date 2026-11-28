//! **User-defined grammars** ("ontologies"): a grammar file a person or an
//! agent writes, judged before it can become a pool of a cauldron.
//!
//! A cauldron already holds several grammars side by side, each a pool
//! pinned to `kind@version` ([`crate::schema_def::PoolGrammar`]); the soup is
//! made with `workspace@6` (pool `ws`) and `cauldron@1` (pool `cauldron`).
//! This module is the one door through which a grammar that is NOT built in
//! may join them. It does three things:
//!
//! 1. [`parse_file`]: the file is untrusted input. Its size and nesting
//!    depth are judged before any parse, then it is parsed STRICTLY: a key
//!    this build does not know is refused by name, never ignored, so a file
//!    written for a later format can never be half-read.
//! 2. [`check_user`]: the rules a user grammar must meet on its own and
//!    against the pins the cauldron already holds (every rule is a named
//!    [`Problem`]; every problem is reported, not just the first).
//! 3. [`digest`]: the grammar's identity beside `kind@version`.
//!
//! What it does NOT do: it adds no field, column or file to the store. A pool
//! made from a user grammar is pinned with exactly the `PoolGrammar` a
//! built-in pool is pinned with, so a binary that predates this module opens
//! it, validates writes into it under its pin, and reads it, like any other
//! pool. That is why the user format may use only constructs the pinned
//! grammar already has: a construct an older binary would not know (a
//! pattern, an enum field kind) would be ignored by it, and needs a store
//! fence first.

use crate::schema_def::{EdgeKindDef, FieldDef, FieldKind, MintAlternative, NodeTypeDef, PoolGrammar, PoolKind, PoolName, TypeRef};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The one file format this build reads, named in the file's `format` key.
pub const FORMAT: &str = "cauldron-grammar/1";

/// The rule a pool name whose file is gone, with writes recorded under it
/// in the spine, is refused by (it is never made again as an empty pool).
pub const POOL_HAS_HISTORY: &str = "pool_name_has_history";

/// The largest file judged at all, in bytes (refused before parsing).
pub const MAX_FILE_BYTES: usize = 256 * 1024;
/// The deepest nesting of objects and arrays a file may have. The format
/// itself is at most six deep; two levels of slack, nothing more.
pub const MAX_DEPTH: usize = 8;

/// Limits on what one grammar declares.
pub const MAX_NODE_TYPES: usize = 64;
pub const MAX_EDGE_KINDS: usize = 64;
pub const MAX_FIELDS: usize = 48;
pub const MAX_STATES: usize = 32;
pub const MAX_EDGE_FIELDS: usize = 16;
pub const MAX_ENDPOINTS: usize = 16;
pub const MAX_ALTERNATIVES: usize = 8;
pub const MAX_MIN_COUNT: u32 = 16;
pub const MAX_CHARS_CAP: u64 = 10_000;
pub const MAX_VERSION: u32 = 9_999;
/// How many user grammars one cauldron may hold.
pub const MAX_USER_POOLS: usize = 8;

/// The display contract: every user type carries `handle`, text of 1 to at
/// most this many characters (what reads, search and the change feed print).
pub const HANDLE_CAP: u64 = crate::grammar_v6::HANDLE_CAP;
/// `body`, when declared, is text of at most this many characters.
pub const BODY_CAP: u64 = crate::grammar_v6::BODY_CAP;

/// Names a user grammar may not give a field: the machine's own, `state`
/// (the state vocabulary's), and the keys a read adds to a record.
pub const RESERVED_FIELDS: &[&str] = &[
    "id", "rev", "schema_version", "created_tx", "updated_tx", "sealed", "state", "type", "edges", "superseded_by",
    "unreviewed", "rejected", "claims", "regions", "pool", "provenance", "label",
];
/// Names a user grammar may not give an edge field: an edge record's own keys.
/// An edge record's own keys, which an edge field never takes: reads print
/// an edge as `{kind, to, note, <fields>}` and an arriving one (`edges_in`)
/// as `{from, from_pool, kind, note, rejected, unreviewed, <fields>}`, so a
/// field of one of these names would read as the edge's position, its note,
/// or its source's review mark.
pub const RESERVED_EDGE_FIELDS: &[&str] = &["kind", "to", "from", "from_pool", "note", "rejected", "unreviewed"];
/// Built-in edge kinds a user grammar may never declare, local or not:
/// readers treat them as region membership and cluster claims by name.
pub const NEVER_EDGE_NAMES: &[&str] = &["in_region", "claims"];
/// Pool names held for the soup's own pools and the transaction namespace.
pub const RESERVED_POOLS: &[&str] = &["ws", "cauldron", "dumps", "spine"];

/// One rule a grammar file broke: the rule's stable name, where in the file
/// (a `/`-separated path, `""` for the whole file), and what was found.
/// Suites match on `rule`; `detail` is prose.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Problem {
    pub rule: &'static str,
    pub at: String,
    pub detail: String,
}

fn problem(rule: &'static str, at: impl Into<String>, detail: impl Into<String>) -> Problem {
    Problem { rule, at: at.into(), detail: detail.into() }
}

/// At most 80 characters of something the file said, for a message.
fn quoted(s: &str) -> String {
    let mut out: String = s.chars().take(80).collect();
    if s.chars().count() > 80 {
        out.push('…');
    }
    format!("{out:?}")
}

// --------------------------------------------------------------- the file

/// The file, as read: strict at every level (`deny_unknown_fields`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDoc {
    // Judged before this parse (`parse_file`); kept so the key is known.
    #[allow(dead_code)]
    format: String,
    #[serde(default)]
    pool: Option<String>,
    kind: String,
    version: u32,
    node_types: BTreeMap<String, FileNodeType>,
    #[serde(default)]
    edge_kinds: BTreeMap<String, FileEdgeKind>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileNodeType {
    fields: BTreeMap<String, FileField>,
    #[serde(default)]
    states: Vec<String>,
    #[serde(default)]
    count_cells: BTreeMap<String, Value>,
    #[serde(default)]
    id_prefix: Option<String>,
    #[serde(default)]
    required_edges_at_mint: BTreeMap<String, u32>,
    #[serde(default)]
    required_at_mint_any_of: Vec<FileAlternative>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileAlternative {
    #[serde(default)]
    edges: BTreeMap<String, u32>,
    #[serde(default)]
    fields: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileField {
    #[serde(default)]
    required: bool,
    #[serde(default)]
    kind: FieldKind,
    #[serde(default)]
    max_chars: Option<u64>,
    #[serde(default)]
    min_chars: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEdgeKind {
    from: Vec<FileTypeRef>,
    to: Vec<FileTypeRef>,
    #[serde(default)]
    at_most_one_per_from: bool,
    #[serde(default)]
    at_most_one_per_to: bool,
    #[serde(default)]
    fields: BTreeMap<String, FileField>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case", deny_unknown_fields)]
enum FileTypeRef {
    Local { node_type: String },
    Reach { kind: String, node_type: String },
    SameAsSource,
}

impl FileField {
    fn into_def(self) -> FieldDef {
        FieldDef { required: self.required, kind: self.kind, max_chars: self.max_chars, min_chars: self.min_chars }
    }
}

impl FileTypeRef {
    fn into_ref(self) -> TypeRef {
        match self {
            FileTypeRef::Local { node_type } => TypeRef::Local { node_type },
            FileTypeRef::Reach { kind, node_type } => TypeRef::Reach { kind: PoolKind(kind), node_type },
            FileTypeRef::SameAsSource => TypeRef::SameAsSource,
        }
    }
}

/// A parsed user grammar file: the grammar it pins (its pool is the file's
/// `pool`, else the kind), and whether the file named the pool. Counts the
/// file declared that a set would collapse (a repeated endpoint) are kept
/// for [`check_user`].
#[derive(Debug, Clone)]
pub struct UserGrammar {
    pub grammar: PoolGrammar,
    pub pool_named: bool,
    /// Per type, `count_cells` the file declared (refused by the check).
    count_cells: BTreeMap<String, usize>,
    /// Per edge kind, how many `to` entries the file listed.
    endpoints: BTreeMap<String, usize>,
}

impl UserGrammar {
    /// A pinned grammar, as [`check_user`] judges it (what
    /// [`crate::Cauldron::add_pool`] does with the grammar it is handed).
    pub fn from_pin(g: &PoolGrammar) -> UserGrammar {
        UserGrammar {
            grammar: g.clone(),
            pool_named: true,
            count_cells: g.node_types.iter().map(|(t, d)| (t.clone(), d.count_cells.len())).collect(),
            endpoints: g.edge_kinds.iter().map(|(k, e)| (k.clone(), e.to.len())).collect(),
        }
    }
}

const DUPLICATE: &str = "duplicate key";

/// A JSON value read refusing a key named twice in one object (serde_json
/// keeps the last one silently).
struct Unique(Value);

impl<'de> serde::Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Unique, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_bool<E>(self, b: bool) -> Result<Unique, E> {
                Ok(Unique(Value::Bool(b)))
            }
            fn visit_i64<E>(self, n: i64) -> Result<Unique, E> {
                Ok(Unique(json!(n)))
            }
            fn visit_u64<E>(self, n: u64) -> Result<Unique, E> {
                Ok(Unique(json!(n)))
            }
            fn visit_f64<E>(self, n: f64) -> Result<Unique, E> {
                Ok(Unique(json!(n)))
            }
            fn visit_str<E>(self, s: &str) -> Result<Unique, E> {
                Ok(Unique(Value::String(s.to_string())))
            }
            fn visit_string<E>(self, s: String) -> Result<Unique, E> {
                Ok(Unique(Value::String(s)))
            }
            fn visit_unit<E>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_none<E>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut v = Vec::new();
                while let Some(Unique(x)) = a.next_element()? {
                    v.push(x);
                }
                Ok(Unique(Value::Array(v)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut m = serde_json::Map::new();
                while let Some(k) = a.next_key::<String>()? {
                    if m.contains_key(&k) {
                        return Err(serde::de::Error::custom(format!("{DUPLICATE} `{}`", k.chars().take(80).collect::<String>())));
                    }
                    let Unique(x) = a.next_value()?;
                    m.insert(k, x);
                }
                Ok(Unique(Value::Object(m)))
            }
        }
        d.deserialize_any(V)
    }
}

/// The deepest nesting of `{`/`[` in `bytes`, strings skipped. Counted on
/// the raw bytes, so a hostile file is measured without being parsed.
fn depth_of(bytes: &[u8]) -> usize {
    let (mut depth, mut max, mut in_str, mut esc) = (0usize, 0usize, false, false);
    for &b in bytes {
        if in_str {
            match (esc, b) {
                (true, _) => esc = false,
                (false, b'\\') => esc = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' | b'[' => {
                depth += 1;
                max = max.max(depth);
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// **Read a grammar file.** Size, then depth, then a strict parse; a file
/// that fails any of these answers exactly one [`Problem`]. `pool` (the
/// command's `--pool`) names the pool when given, and must agree with a
/// `pool` the file names.
pub fn parse_file(bytes: &[u8], pool: Option<&str>) -> Result<UserGrammar, Vec<Problem>> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(vec![problem(
            "file_too_large",
            "",
            format!("the file is {} bytes; a grammar file is at most {MAX_FILE_BYTES} bytes", bytes.len()),
        )]);
    }
    let depth = depth_of(bytes);
    if depth > MAX_DEPTH {
        return Err(vec![problem(
            "too_deep",
            "",
            format!("the file nests {depth} levels of objects and arrays; a grammar file nests at most {MAX_DEPTH}"),
        )]);
    }
    // A UTF-8 byte order mark (some editors write one) is not JSON, and is
    // not the file's content: it is skipped.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let value: Value = match serde_json::from_slice::<Unique>(bytes) {
        Ok(Unique(v)) => v,
        Err(e) if e.to_string().starts_with(DUPLICATE) => {
            return Err(vec![problem("duplicate_key", "", format!("the file names a key twice in one object ({e}); a JSON reader would keep one of the two silently, so the file is refused"))])
        }
        Err(e) => return Err(vec![problem("not_json", "", format!("the file is not JSON: {e}"))]),
    };
    // The format word first, by itself: a file for a later format is refused
    // as that, not as whatever key of it this build does not know.
    match value.get("format") {
        Some(Value::String(f)) if f == FORMAT => {}
        Some(Value::String(f)) => {
            return Err(vec![problem(
                "wrong_format",
                "format",
                format!("the file says format {}; this build reads only `{FORMAT}`", quoted(f)),
            )])
        }
        Some(_) => return Err(vec![problem("wrong_format", "format", format!("`format` must be the string `{FORMAT}`"))]),
        None => {
            return Err(vec![problem(
                "wrong_format",
                "format",
                format!("the file has no `format`; a grammar file says `\"format\": \"{FORMAT}\"`"),
            )])
        }
    }
    let doc: FileDoc = serde_json::from_value(value)
        .map_err(|e| vec![problem("unknown_or_malformed_key", "", format!("the file does not have the grammar file's shape: {e}"))])?;
    let pool_named = pool.is_some() || doc.pool.is_some();
    let name = match (pool, doc.pool.as_deref()) {
        (Some(a), Some(b)) if a != b => {
            return Err(vec![problem(
                "pool_disagrees",
                "pool",
                format!("the file names pool {} and the command names pool {}; name it once", quoted(b), quoted(a)),
            )])
        }
        (Some(a), _) => a.to_string(),
        (None, Some(b)) => b.to_string(),
        (None, None) => doc.kind.clone(),
    };
    let mut count_cells = BTreeMap::new();
    let mut endpoints = BTreeMap::new();
    let node_types = doc
        .node_types
        .into_iter()
        .map(|(t, d)| {
            count_cells.insert(t.clone(), d.count_cells.len());
            let def = NodeTypeDef {
                fields: d.fields.into_iter().map(|(k, f)| (k, f.into_def())).collect(),
                states: d.states,
                count_cells: BTreeMap::new(),
                id_prefix: d.id_prefix,
                required_edges_at_mint: d.required_edges_at_mint,
                required_at_mint_any_of: d
                    .required_at_mint_any_of
                    .into_iter()
                    .map(|a| MintAlternative { edges: a.edges, fields: a.fields })
                    .collect(),
            };
            (t, def)
        })
        .collect();
    let edge_kinds = doc
        .edge_kinds
        .into_iter()
        .map(|(k, e)| {
            endpoints.insert(k.clone(), e.to.len());
            let def = EdgeKindDef {
                from: e.from.into_iter().map(FileTypeRef::into_ref).collect(),
                to: e.to.into_iter().map(FileTypeRef::into_ref).collect(),
                at_most_one_per_from: e.at_most_one_per_from,
                at_most_one_per_to: e.at_most_one_per_to,
                fields: e.fields.into_iter().map(|(k, f)| (k, f.into_def())).collect(),
            };
            (k, def)
        })
        .collect();
    Ok(UserGrammar {
        grammar: PoolGrammar { pool: PoolName(name), kind: PoolKind(doc.kind), version: doc.version, node_types, edge_kinds },
        pool_named,
        count_cells,
        endpoints,
    })
}

/// A pinned grammar as a grammar FILE (what `grammar show` prints and what a
/// user may start from): the pin with `format` added.
pub fn to_file(g: &PoolGrammar) -> Value {
    let mut v = serde_json::to_value(g).expect("a grammar serializes");
    v["format"] = json!(FORMAT);
    v
}

// ------------------------------------------------------------- the rules

/// Is `kind` a user kind? Every user kind has a hyphen in it, and no
/// built-in kind ever will, so the two can never collide.
pub fn is_user_kind(kind: &str) -> bool {
    kind.contains('-')
}

/// The grammars built into this release, whatever the cauldron holds: the
/// names they use are taken for every cauldron.
pub fn built_ins() -> Vec<PoolGrammar> {
    vec![
        crate::grammar_v6::workspace_v6("ws"),
        crate::grammar_cauldron::cauldron_v1(crate::grammar_cauldron::POOL),
        crate::grammar_dump::dumps_v2(crate::grammar_dump::POOL),
    ]
}

/// The built-in `workspace` types a user edge may reach: the region and the
/// ten content types (not `viewer`).
pub fn workspace_reach_targets() -> BTreeSet<String> {
    crate::grammar_v6::workspace_v6("ws").node_types.into_keys().filter(|t| t != "viewer").collect()
}

fn kind_name_ok(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && !s.ends_with('-')
        && !s.contains("--")
}

fn ident_ok(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b.len() <= 40 && b[0].is_ascii_lowercase() && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

fn state_ok(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

fn prefix_ok(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b.len() <= 6 && b[0].is_ascii_uppercase() && b.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// **Judge a user grammar** on its own and against `pins`, the grammars the
/// cauldron holds now (none, for a check without a cauldron). Every problem
/// found is answered; an empty list is a grammar `add_pool` takes.
pub fn check_user(u: &UserGrammar, pins: &[&PoolGrammar]) -> Vec<Problem> {
    let g = &u.grammar;
    let mut out = Vec::new();
    let mut p = |rule, at: String, detail: String| out.push(problem(rule, at, detail));

    // The kind and the pool.
    if !kind_name_ok(g.kind.as_str()) {
        p("name_charset", "kind".into(), format!("kind {} is not [a-z][a-z0-9-], at most 32 characters", quoted(g.kind.as_str())));
    } else if !is_user_kind(g.kind.as_str()) {
        p(
            "kind_needs_hyphen",
            "kind".into(),
            format!(
                "kind `{}` has no hyphen; a user kind always has one (`reading-list`, not `reading`), so it can never \
                 be a built-in kind's name, now or later",
                g.kind
            ),
        );
    }
    let pool = g.pool.as_str();
    if !kind_name_ok(pool) {
        p("name_charset", "pool".into(), format!("pool {} is not [a-z][a-z0-9-], at most 32 characters", quoted(pool)));
    } else if (RESERVED_POOLS.contains(&pool) || !pool.contains('-')) && (u.pool_named || is_user_kind(g.kind.as_str())) {
        // (A pool named after a kind with no hyphen is that kind's problem,
        // said once above.)
        p("pool_reserved", "pool".into(), format!("pool `{pool}` is reserved or has no hyphen; a user pool's name has a hyphen, like its kind"));
    }
    if g.version == 0 || g.version > MAX_VERSION {
        p("version_range", "version".into(), format!("version {} is not 1 to {MAX_VERSION}", g.version));
    }

    // What the cauldron and this release already use.
    let built = built_ins();
    let mut taken_types: BTreeMap<String, String> = BTreeMap::new();
    let mut taken_prefixes: BTreeMap<String, String> = BTreeMap::new();
    for b in &built {
        for (t, d) in &b.node_types {
            taken_types.insert(t.clone(), format!("built-in `{}@{}`", b.kind, b.version));
            if let Some(x) = &d.id_prefix {
                taken_prefixes.insert(x.clone(), format!("built-in `{}@{}` type `{t}`", b.kind, b.version));
            }
        }
    }
    // Retired built-in prefixes, kept off so an old id never reads as new.
    for x in ["DIR", "CON", "D", "IS", "DEC", "Q", "T", "M", "C", "HO", "R", "SUB", "V", "CL", "DM"] {
        taken_prefixes.entry(x.to_string()).or_insert_with(|| "a built-in grammar".into());
    }
    let user_pins: Vec<&&PoolGrammar> = pins.iter().filter(|x| is_user_kind(x.kind.as_str()) && x.pool != g.pool).collect();
    for x in pins {
        if x.pool == g.pool {
            continue;
        }
        for (t, d) in &x.node_types {
            taken_types.entry(t.clone()).or_insert_with(|| format!("pool `{}` (`{}@{}`)", x.pool, x.kind, x.version));
            if let Some(pre) = &d.id_prefix {
                taken_prefixes.entry(pre.clone()).or_insert_with(|| format!("pool `{}` type `{t}`", x.pool));
            }
        }
        if x.kind == g.kind {
            p(
                "kind_registered",
                "kind".into(),
                format!(
                    "this cauldron already holds kind `{}` as pool `{}` (`{}@{}`); one pool per user kind in this release \
                     (moving to a new version is a later step)",
                    g.kind, x.pool, x.kind, x.version
                ),
            );
        }
    }
    if pins.iter().any(|x| x.pool == g.pool && **x != *g) {
        let x = pins.iter().find(|x| x.pool == g.pool).expect("found above");
        if x.kind == g.kind && x.version == g.version {
            p(
                "pool_in_use",
                "pool".into(),
                format!(
                    "this cauldron already has pool `{pool}`, pinned to `{}@{}` with other content: the pinned grammar's digest \
                     is {}, this file's is {}. A pinned grammar is never replaced, and a new version of a kind is not built in \
                     this release (`cauldron soup grammar show {pool}` prints the pinned one)",
                    x.kind,
                    x.version,
                    digest(x),
                    digest(g)
                ),
            );
        } else {
            p("pool_in_use", "pool".into(), format!("this cauldron already has pool `{pool}`, pinned to `{}@{}`", x.kind, x.version));
        }
    }
    if user_pins.len() >= MAX_USER_POOLS && !pins.iter().any(|x| x.pool == g.pool) {
        p("too_many_user_pools", "".into(), format!("this cauldron holds {} user grammars; at most {MAX_USER_POOLS}", user_pins.len()));
    }

    // Node types.
    if g.node_types.is_empty() {
        p("no_types", "node_types".into(), "a grammar declares at least one node type".into());
    }
    if g.node_types.len() > MAX_NODE_TYPES {
        p("too_many", "node_types".into(), format!("{} node types; at most {MAX_NODE_TYPES}", g.node_types.len()));
    }
    let mut own_prefixes: BTreeMap<&str, &str> = BTreeMap::new();
    for (t, d) in &g.node_types {
        let at = format!("node_types/{t}");
        if !ident_ok(t) {
            p("name_charset", at.clone(), format!("type name {} is not [a-z][a-z0-9_], at most 40 characters", quoted(t)));
            continue;
        }
        if let Some(by) = taken_types.get(t) {
            p("type_name_taken", at.clone(), format!("type `{t}` is already a type of {by}; a record goes to its pool by type, so a type name is used once per cauldron"));
        }
        match &d.id_prefix {
            None => p("prefix_missing", format!("{at}/id_prefix"), format!("type `{t}` declares no id_prefix; every user type has one (ids are `<prefix>-<n>`)")),
            Some(x) if !prefix_ok(x) => p("prefix_charset", format!("{at}/id_prefix"), format!("id_prefix {} is not [A-Z][A-Z0-9], at most 6 characters", quoted(x))),
            Some(x) => {
                if let Some(by) = taken_prefixes.get(x) {
                    p("prefix_taken", format!("{at}/id_prefix"), format!("id_prefix `{x}` is already used by {by}; an id names no pool, so a prefix is used once per cauldron"));
                }
                if let Some(other) = own_prefixes.insert(x, t) {
                    p("prefix_taken", format!("{at}/id_prefix"), format!("id_prefix `{x}` is also type `{other}`'s"));
                }
            }
        }
        if u.count_cells.get(t).copied().unwrap_or(0) > 0 {
            p("count_cells_unsupported", format!("{at}/count_cells"), "a user grammar declares no count cells in this release".into());
        }
        if d.fields.len() > MAX_FIELDS {
            p("too_many", format!("{at}/fields"), format!("{} fields; at most {MAX_FIELDS}", d.fields.len()));
        }
        if d.states.len() > MAX_STATES {
            p("too_many", format!("{at}/states"), format!("{} states; at most {MAX_STATES}", d.states.len()));
        }
        for s in &d.states {
            if !state_ok(s) {
                p("name_charset", format!("{at}/states"), format!("state {} is not [a-z][a-z0-9_-], at most 40 characters", quoted(s)));
            }
        }
        for (f, def) in &d.fields {
            let fat = format!("{at}/fields/{f}");
            if !ident_ok(f) {
                p("name_charset", fat, format!("field name {} is not [a-z][a-z0-9_], at most 40 characters", quoted(f)));
                continue;
            }
            if RESERVED_FIELDS.contains(&f.as_str()) {
                p("field_reserved", fat.clone(), format!("`{f}` is a name the store or a read uses; choose another"));
            }
            field_limits(&mut p, &fat, def);
        }
        // The display contract.
        match d.fields.get("handle") {
            Some(h) if h.required && h.kind == FieldKind::Text && h.min_chars.unwrap_or(0) >= 1 && h.max_chars.is_some_and(|c| c <= HANDLE_CAP) => {}
            _ => p(
                "handle_required",
                format!("{at}/fields/handle"),
                format!(
                    "every user type declares `handle` as required text with min_chars of at least 1 and max_chars of at \
                     most {HANDLE_CAP}: reads, search and the change feed show a record by its handle"
                ),
            ),
        }
        if let Some(b) = d.fields.get("body") {
            if b.kind != FieldKind::Text || b.max_chars.is_none_or(|c| c > BODY_CAP) {
                p("body_shape", format!("{at}/fields/body"), format!("`body`, when declared, is text with max_chars of at most {BODY_CAP}"));
            }
        }
        if let Some(m) = d.fields.get("meta") {
            if m.kind != FieldKind::Object {
                p("meta_shape", format!("{at}/fields/meta"), "`meta`, when declared, has kind `object` (it carries `cites`)".into());
            }
        }
        for (k, n) in &d.required_edges_at_mint {
            if *n > MAX_MIN_COUNT {
                p("too_many", format!("{at}/required_edges_at_mint/{k}"), format!("a minimum of {n}; at most {MAX_MIN_COUNT}"));
            }
        }
        if d.required_at_mint_any_of.len() > MAX_ALTERNATIVES {
            p("too_many", format!("{at}/required_at_mint_any_of"), format!("{} alternatives; at most {MAX_ALTERNATIVES}", d.required_at_mint_any_of.len()));
        }
        for a in &d.required_at_mint_any_of {
            for (k, n) in &a.edges {
                if *n > MAX_MIN_COUNT {
                    p("too_many", format!("{at}/required_at_mint_any_of"), format!("edge `{k}` minimum {n}; at most {MAX_MIN_COUNT}"));
                }
            }
        }
    }

    // Edge kinds.
    if g.edge_kinds.len() > MAX_EDGE_KINDS {
        p("too_many", "edge_kinds".into(), format!("{} edge kinds; at most {MAX_EDGE_KINDS}", g.edge_kinds.len()));
    }
    let builtin_edges: BTreeSet<String> = built.iter().flat_map(|b| b.edge_kinds.keys().cloned()).collect();
    let reach_ws = workspace_reach_targets();
    for (k, e) in &g.edge_kinds {
        let at = format!("edge_kinds/{k}");
        if !ident_ok(k) {
            p("name_charset", at, format!("edge kind name {} is not [a-z][a-z0-9_], at most 40 characters", quoted(k)));
            continue;
        }
        if u.endpoints.get(k).copied().unwrap_or(0) > MAX_ENDPOINTS || e.from.len() > MAX_ENDPOINTS {
            p("too_many", at.clone(), format!("more than {MAX_ENDPOINTS} endpoints"));
        }
        if e.fields.len() > MAX_EDGE_FIELDS {
            p("too_many", format!("{at}/fields"), format!("{} edge fields; at most {MAX_EDGE_FIELDS}", e.fields.len()));
        }
        for (f, def) in &e.fields {
            let fat = format!("{at}/fields/{f}");
            if !ident_ok(f) {
                p("name_charset", fat, format!("edge field name {} is not [a-z][a-z0-9_], at most 40 characters", quoted(f)));
                continue;
            }
            if RESERVED_EDGE_FIELDS.contains(&f.as_str()) {
                p("field_reserved", fat.clone(), format!("`{f}` is an edge record's own key"));
            }
            field_limits(&mut p, &fat, def);
        }
        let leaves_pool = e.to.iter().any(|t| matches!(t, TypeRef::Reach { kind, .. } if kind != &g.kind));
        if NEVER_EDGE_NAMES.contains(&k.as_str()) {
            p("edge_name_reserved", at.clone(), format!("`{k}` is a built-in edge kind that readers treat as region membership or a cluster's claim; a user grammar never declares it"));
        } else if builtin_edges.contains(k) && leaves_pool {
            p("edge_name_reserved", at.clone(), format!("`{k}` is a built-in edge kind's name; a user edge of that name stays inside its own kind"));
        }
        for t in &e.to {
            let TypeRef::Reach { kind, node_type } = t else { continue };
            let tat = format!("{at}/to");
            if kind == &g.kind {
                if !g.node_types.contains_key(node_type) {
                    p("reach_unknown_type", tat, format!("kind `{kind}` (this grammar) has no type `{node_type}`"));
                }
            } else if kind.as_str() == crate::grammar_v6::KIND {
                if !reach_ws.contains(node_type) {
                    p("reach_not_allowed", tat, format!("a user edge may reach `workspace` types {} only, not `{node_type}`", reach_ws.iter().cloned().collect::<Vec<_>>().join(", ")));
                }
            } else if !is_user_kind(kind.as_str()) {
                p("reach_not_allowed", tat, format!("a user edge may reach its own kind, `workspace`, or another user kind this cauldron holds; not built-in kind `{kind}`"));
            } else {
                match user_pins.iter().find(|x| &x.kind == kind) {
                    None => p("reach_unknown_kind", tat, format!("this cauldron holds no kind `{kind}`; a reach names a kind already registered")),
                    Some(x) if !x.node_types.contains_key(node_type) => {
                        p("reach_unknown_type", tat, format!("kind `{kind}` (pool `{}`) has no type `{node_type}`", x.pool))
                    }
                    Some(_) => {}
                }
            }
        }
    }

    // Last, the spine's own well-formedness, which every pinned grammar meets.
    if out.is_empty() {
        if let Err(e) = g.check_wellformed() {
            // The witnesses say what is wrong; the error's own text only
            // counts them.
            let detail = match &e {
                crate::error::SpineError::Refused { witnesses } => witnesses
                    .iter()
                    .map(|w| match w {
                        crate::witness::Witness::GrammarViolation { message, .. } => message.clone(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
                other => other.to_string(),
            };
            out.push(problem("wellformed", "", detail));
        }
    }
    out
}

fn field_limits(p: &mut impl FnMut(&'static str, String, String), at: &str, def: &FieldDef) {
    if def.max_chars.is_some_and(|c| c > MAX_CHARS_CAP) {
        p("cap_too_large", format!("{at}/max_chars"), format!("max_chars {}; at most {MAX_CHARS_CAP}", def.max_chars.unwrap_or(0)));
    }
    if def.min_chars.is_some_and(|c| c > MAX_CHARS_CAP) {
        p("cap_too_large", format!("{at}/min_chars"), format!("min_chars {}; at most {MAX_CHARS_CAP}", def.min_chars.unwrap_or(0)));
    }
}

// --------------------------------------------------------------- identity

/// The rule a prefix another pool has minted ids under is refused by.
pub const PREFIX_HAS_HISTORY: &str = "prefix_has_history";

/// What the spine's history refuses of `g` in the soup at `dir`: an id
/// prefix another pool has minted or carried ids under
/// ([`PREFIX_HAS_HISTORY`]: its ids would be minted again from 1, even
/// after that pool's file was moved out).
pub fn history_problems(dir: &std::path::Path, g: &PoolGrammar) -> crate::error::Result<Vec<Problem>> {
    let conn = crate::offline::read_only_connection(&dir.join(crate::spine_db::SPINE_FILE))?;
    let mut out = Vec::new();
    for (t, d) in &g.node_types {
        let Some(pre) = &d.id_prefix else { continue };
        if let Some(other) = crate::user_registry::prefix_minted_elsewhere(&conn, pre, g.pool.as_str())? {
            out.push(problem(
                PREFIX_HAS_HISTORY,
                format!("node_types/{t}/id_prefix"),
                format!(
                    "pool `{other}` of this soup has minted ids under prefix `{pre}` (its file may be moved out, but its records \
                     and their ids exist): a prefix never moves to another pool, so no id is ever minted twice. Choose another \
                     prefix, or put `{other}`'s file back"
                ),
            ));
        }
    }
    Ok(out)
}

/// The built-in pools a soup is made with; every other pool of a soup
/// that has registered a user grammar must be in its registry.
pub const BUILT_IN_POOLS: [&str; 2] = ["ws", "cauldron"];

/// What to do about a user pool file [`refusal`] refuses: said in one
/// place, for the spine's open and the soup's reads alike.
pub fn way_out(file: &std::path::Path) -> String {
    let pool = file.file_stem().and_then(|n| n.to_str()).unwrap_or("<pool>");
    format!(
        "Until it is dealt with, every write to this soup is refused, and so is every read that reaches a user grammar's \
         pool (a read of the workspace's own records still answers). Put back the file this cauldron wrote; a copy older \
         than the pool's last write is taken only by `cauldron soup grammar rebind {pool} --env <env> --cauldron <name>` \
         (it names the writes the copy lacks, and with `--yes` records them as lost), or restore the whole soup directory \
         from one backup. Or move `{}` out of the soup directory, keeping it: the rest of the soup then opens, the pool's \
         records cannot be read until it is back, and its kind, types and id prefixes stay registered, so no other pool \
         mints its ids. A user pool file is never edited, copied in or removed by hand",
        file.display()
    )
}

/// A pool file that will not open, as an error: a user pool's (its name
/// has a hyphen) says the way out, since its owner added the file and the
/// whole soup waits on it; a built-in pool's is the error as it was.
pub fn unreadable(file: &std::path::Path, e: crate::error::SpineError) -> crate::error::SpineError {
    let user = file.file_stem().and_then(|n| n.to_str()).is_some_and(|n| n.contains('-'));
    if !user {
        return e;
    }
    crate::error::SpineError::hard(format!("user pool file `{}` cannot be read ({e}). {}.", file.display(), way_out(file)))
}

/// **Why the soup in `dir` may not be opened with these pins** (read from
/// its pool files), or `None`.
///
/// Once a soup has registered a user grammar (`crate::user_registry`):
/// - `ws` and `cauldron` may not declare a type or id prefix a registered
///   user pool declares (that built-in file is the one blamed);
/// - every other pool file must be a LIVE registered pool whose pin is
///   exactly the registered grammar (same digest): a hand edit of any field,
///   a kind edited to a word with no hyphen, a pool file the registry does
///   not name, and a removed pool's file put back are all refused;
/// - each such file must be bound to this spine (`offline::binding`), less
///   the parts the owner accepted as lost.
///
/// A registered pool whose file is missing is not a refusal here (see
/// `crate::user_registry`). A soup that never registered one through the
/// registry is judged as before ([`judge_pins`]), and a user pool file in it
/// is refused with the command that registers it (`rebind`).
pub fn refusal(dir: &std::path::Path, pins: &[&PoolGrammar]) -> crate::error::Result<Option<String>> {
    let conn = crate::offline::read_only_connection(&dir.join(crate::spine_db::SPINE_FILE))?;
    let file_of = |pool: &str| dir.join(format!("{pool}.db"));
    let Some(reg) = crate::user_registry::read(&conn)? else {
        if let Some((pool, p)) = judge_pins(pins).into_iter().next() {
            let file = file_of(pool.as_str());
            return Ok(Some(format!(
                "pool file `{}` holds a grammar `cauldron soup grammar add` would refuse ({} at `{}`: {}), so it was changed by \
                 hand or copied in. {}.",
                file.display(),
                p.rule,
                p.at,
                p.detail,
                way_out(&file)
            )));
        }
        if let Some(g) = pins.iter().find(|g| is_user_kind(g.kind.as_str())) {
            return Ok(Some(format!(
                "pool file `{}` holds user grammar `{}@{}`, but this soup's spine has no record of registering it (an earlier \
                 build of this release did not keep one, or the file was copied in). If it is this soup's own pool, record it: \
                 `cauldron soup grammar rebind {} --env <env> --cauldron <name>` judges it as `grammar add` would and binds \
                 it to this soup",
                file_of(g.pool.as_str()).display(),
                g.kind,
                g.version,
                g.pool
            )));
        }
        return Ok(None);
    };
    let live: Vec<&crate::user_registry::Row> = reg.values().filter(|r| r.is_live()).collect();
    for g in pins {
        let pool = g.pool.as_str();
        let file = file_of(pool);
        if BUILT_IN_POOLS.contains(&pool) {
            for r in &live {
                let clash = g.node_types.iter().find_map(|(t, d)| {
                    if r.grammar.node_types.contains_key(t) {
                        return Some(format!("type `{t}`"));
                    }
                    let p = d.id_prefix.as_ref()?;
                    r.grammar.node_types.values().any(|u| u.id_prefix.as_ref() == Some(p)).then(|| format!("id prefix `{p}`"))
                });
                if let Some(what) = clash {
                    return Ok(Some(format!(
                        "built-in pool file `{}` declares {what}, which user pool `{}` (`{}@{}`) registered: the built-in pool's pin \
                         was edited by hand. Put back this cauldron's own `{pool}.db`, or restore the whole soup directory from \
                         one backup",
                        file.display(),
                        r.pool,
                        r.kind,
                        r.version
                    )));
                }
            }
            continue;
        }
        let Some(row) = reg.get(pool) else {
            return Ok(Some(format!(
                "pool file `{}` is not a pool this soup registered (its pin says `{}@{}`): a pool other than `ws` and `cauldron` \
                 joins a soup only by `cauldron soup grammar add`, so this file was copied in or its pin edited by hand. {}.",
                file.display(),
                g.kind,
                g.version,
                way_out(&file)
            )));
        };
        if !row.is_live() {
            return Ok(Some(format!(
                "pool file `{}` is the file of user pool `{pool}`, which was removed (`cauldron soup grammar remove`); a removed \
                 pool's file is not put back. Move it out of the soup directory; `cauldron soup grammar add` makes the pool \
                 again, empty",
                file.display()
            )));
        }
        match crate::user_registry::pin_matches(row, g) {
            crate::user_registry::PinMatch::Current | crate::user_registry::PinMatch::EarlierForm => {}
            crate::user_registry::PinMatch::NewerForm(v) => {
                return Ok(Some(format!(
                    "this soup's record of user pool `{pool}` was written by a newer release (digest form {v}; this release knows \
                     up to {DIGEST_INPUT_VERSION}), so this release cannot judge pool file `{}`. Open the soup with that release",
                    file.display()
                )))
            }
            crate::user_registry::PinMatch::Differs(what) => {
                return Ok(Some(format!(
                    "pool file `{}` holds a grammar that is not the one this soup recorded when pool `{pool}` was added ({what}). A \
                     pinned grammar never changes, so the file's pin was changed after it was written, or the file is another \
                     pool's. {}.",
                    file.display(),
                    way_out(&file)
                )))
            }
        }
        if let Some(why) = crate::offline::binding(dir, pool)?.why(pool) {
            return Ok(Some(format!("pool file `{}` is not this soup's own: {why}.", file.display())));
        }
    }
    Ok(None)
}

/// **The pins a soup holds, judged as `cauldron soup grammar add` judged
/// them**, every time they are read from the pool files: each user
/// grammar's pin by [`check_user`] against all the others, and every pin of
/// one of this release's built-in kinds (`workspace`, `cauldron`, `dumps`)
/// sitting under a user pool's name (a hyphen) beside the pool that holds
/// that kind already: a soup holds each built-in kind once, so the second
/// is a user pool's pin edited to claim it. (A lone built-in pool under any
/// name, as the spine's own tests make, and a pin of any other kind with no
/// hyphen, are not a user grammar's and are left as they were.) A pool
/// file is the pool's own record, so a hand edit or a file copied in is
/// caught here, by the same rules, rather than routed (a user pin declaring
/// a built-in type would take that type's records; one claiming a built-in
/// kind would hide its own). Empty: every pin is one `add` would take.
pub fn judge_pins(pins: &[&PoolGrammar]) -> Vec<(PoolName, Problem)> {
    let mut out = Vec::new();
    for g in pins {
        if is_user_kind(g.kind.as_str()) {
            out.extend(check_user(&UserGrammar::from_pin(g), pins).into_iter().map(|p| (g.pool.clone(), p)));
        } else if g.pool.as_str().contains('-')
            && built_ins().iter().any(|b| b.kind == g.kind)
            && pins.iter().any(|o| o.pool != g.pool && o.kind == g.kind)
        {
            out.push((
                g.pool.clone(),
                problem(
                    "builtin_kind_in_user_pool",
                    "kind",
                    format!(
                        "pool `{}` has a user pool's name (it has a hyphen), but its pin claims the built-in kind `{}`, which \
                         another pool of this soup holds; a built-in grammar is pinned only to the pool the cauldron is made with",
                        g.pool, g.kind
                    ),
                ),
            ));
        }
    }
    out
}

/// **A grammar's digest**: `sha256:` and the hex of the SHA-256 of its
/// canonical JSON (keys sorted, no spaces), the pool's name left out (one
/// grammar, whatever a cauldron calls its pool). Computed from the pin, so
/// nothing is stored beside it.
pub fn digest(g: &PoolGrammar) -> String {
    use sha2::Digest;
    let bytes = serde_json::to_vec(&digest_input(g)).expect("a value serializes");
    let sum = sha2::Sha256::digest(&bytes);
    let mut out = String::from("sha256:");
    for b in sum {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The digest form before [`DIGEST_INPUT_VERSION`] 1: the SHA-256 of the
/// grammar structs' own serialization, the pool's name left out (what
/// development builds of this release recorded before the form was fixed).
/// Kept so a registry row written in it is recognised and migrated, never
/// refused; it is computed from today's structs, which serialize as they
/// did then.
pub fn digest_v0(g: &PoolGrammar) -> String {
    use sha2::Digest;
    let mut v = serde_json::to_value(g).expect("a grammar serializes");
    if let Some(m) = v.as_object_mut() {
        m.remove("pool");
    }
    let sum = sha2::Sha256::digest(serde_json::to_vec(&v).expect("a value serializes"));
    let mut out = String::from("sha256:");
    for b in sum {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Every digest form this release knows, oldest first, with its version:
/// a registry row in any of them is judged (and migrated to the newest).
/// A later form is added here, never in place of an earlier one.
pub fn digest_forms() -> Vec<(u32, fn(&PoolGrammar) -> String)> {
    vec![(0, digest_v0 as fn(&PoolGrammar) -> String), (DIGEST_INPUT_VERSION, digest as fn(&PoolGrammar) -> String)]
}

/// What differs between the grammar a pool was registered with and a file's
/// pin, in words (the first few differences).
pub fn what_differs(registered: &PoolGrammar, pin: &PoolGrammar) -> String {
    let mut out = Vec::new();
    if registered.kind != pin.kind || registered.version != pin.version {
        out.push(format!("registered `{}@{}`, the file says `{}@{}`", registered.kind, registered.version, pin.kind, pin.version));
    }
    for t in registered.node_types.keys().filter(|t| !pin.node_types.contains_key(*t)) {
        out.push(format!("type `{t}` is gone"));
    }
    for t in pin.node_types.keys().filter(|t| !registered.node_types.contains_key(*t)) {
        out.push(format!("type `{t}` was added"));
    }
    for (t, d) in &pin.node_types {
        if let Some(r) = registered.node_types.get(t) {
            if r != d {
                out.push(format!("type `{t}` differs"));
            }
        }
    }
    for e in registered.edge_kinds.keys().filter(|e| !pin.edge_kinds.contains_key(*e)) {
        out.push(format!("edge kind `{e}` is gone"));
    }
    for e in pin.edge_kinds.keys().filter(|e| !registered.edge_kinds.contains_key(*e)) {
        out.push(format!("edge kind `{e}` was added"));
    }
    for (e, d) in &pin.edge_kinds {
        if let Some(r) = registered.edge_kinds.get(e) {
            if r != d {
                out.push(format!("edge kind `{e}` differs"));
            }
        }
    }
    if out.is_empty() {
        out.push("the grammar's content differs".into());
    }
    out.truncate(4);
    out.join("; ")
}

/// The version of [`digest_input`]'s shape, carried inside it.
pub const DIGEST_INPUT_VERSION: u32 = 1;

/// **What [`digest`] hashes**: a canonical JSON written field by field
/// (version [`DIGEST_INPUT_VERSION`]), never the grammar structs' own
/// serialization. The spine's registry keeps every user pool's digest and
/// compares it at every open, so a digest that moved when a later release
/// added a field to `PoolGrammar`, `NodeTypeDef`, `FieldDef` or
/// `EdgeKindDef` would refuse every user pool as "edited by hand". Every
/// struct is destructured with no `..`, so adding a field does not compile
/// until it is placed here on purpose (and then the version moves, with a
/// migration of the registered digests). The pool's name is left out: it
/// names a pool, not a grammar. A test pins the example grammar's digest.
pub fn digest_input(g: &PoolGrammar) -> Value {
    use crate::schema_def::{CountCellDef, Direction, EdgeKindDef, FieldDef, NodeTypeDef};
    let PoolGrammar { pool: _, kind, version, node_types, edge_kinds } = g;
    let field = |f: &FieldDef| {
        let FieldDef { required, kind, max_chars, min_chars } = f;
        json!({"required": required, "kind": serde_json::to_value(kind).expect("a field kind serializes"), "max_chars": max_chars, "min_chars": min_chars})
    };
    let type_ref = |t: &TypeRef| match t {
        TypeRef::Local { node_type } => json!({"at": "local", "node_type": node_type}),
        TypeRef::Reach { kind, node_type } => json!({"at": "reach", "kind": kind.as_str(), "node_type": node_type}),
        TypeRef::SameAsSource => json!({"at": "same_as_source"}),
    };
    let types: serde_json::Map<String, Value> = node_types
        .iter()
        .map(|(name, d)| {
            let NodeTypeDef { fields, states, count_cells, id_prefix, required_edges_at_mint, required_at_mint_any_of } = d;
            let cells: serde_json::Map<String, Value> = count_cells
                .iter()
                .map(|(c, def)| {
                    let CountCellDef { edge_kind, direction } = def;
                    let dir = match direction {
                        Direction::Out => "out",
                        Direction::In => "in",
                    };
                    (c.clone(), json!({"edge_kind": edge_kind, "direction": dir}))
                })
                .collect();
            let any_of: Vec<Value> = required_at_mint_any_of
                .iter()
                .map(|a| {
                    let MintAlternative { edges, fields } = a;
                    json!({"edges": edges, "fields": fields})
                })
                .collect();
            let fields: serde_json::Map<String, Value> = fields.iter().map(|(f, def)| (f.clone(), field(def))).collect();
            (
                name.clone(),
                json!({"fields": fields, "states": states, "count_cells": cells, "id_prefix": id_prefix,
                       "required_edges_at_mint": required_edges_at_mint, "required_at_mint_any_of": any_of}),
            )
        })
        .collect();
    let edges: serde_json::Map<String, Value> = edge_kinds
        .iter()
        .map(|(name, e)| {
            let EdgeKindDef { from, to, at_most_one_per_from, at_most_one_per_to, fields } = e;
            let fields: serde_json::Map<String, Value> = fields.iter().map(|(f, def)| (f.clone(), field(def))).collect();
            (
                name.clone(),
                json!({"from": from.iter().map(type_ref).collect::<Vec<_>>(), "to": to.iter().map(type_ref).collect::<Vec<_>>(),
                       "at_most_one_per_from": at_most_one_per_from, "at_most_one_per_to": at_most_one_per_to, "fields": fields}),
            )
        })
        .collect();
    json!({"digest_input": DIGEST_INPUT_VERSION, "kind": kind.as_str(), "version": version, "node_types": types, "edge_kinds": edges})
}

/// `builtin` or `user`, for a listing.
pub fn origin(g: &PoolGrammar) -> &'static str {
    if is_user_kind(g.kind.as_str()) {
        "user"
    } else {
        "builtin"
    }
}

// ------------------------------------------------------------ meta-schema

/// **The grammar file's JSON Schema** (2020-12), generated from the same
/// limits and charsets [`check_user`] holds a file to. It describes the
/// file's structure and names; the rules that need the cauldron (a name
/// already taken) and the display contract's cross-field rules are the
/// check's alone. Published as `schema/soup/grammar.schema.json`.
pub fn meta_schema() -> Value {
    let ident = json!({"type": "string", "pattern": "^[a-z][a-z0-9_]{0,39}$"});
    let field = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "required": {"type": "boolean"},
            "kind": {"enum": ["any", "text", "integer", "boolean", "object"]},
            "max_chars": {"type": ["integer", "null"], "minimum": 0, "maximum": MAX_CHARS_CAP},
            "min_chars": {"type": ["integer", "null"], "minimum": 0, "maximum": MAX_CHARS_CAP}
        }
    });
    let type_ref = json!({
        "oneOf": [
            {"type": "object", "additionalProperties": false, "required": ["at", "node_type"],
             "properties": {"at": {"const": "local"}, "node_type": {"$ref": "#/$defs/ident"}}},
            {"type": "object", "additionalProperties": false, "required": ["at", "kind", "node_type"],
             "properties": {"at": {"const": "reach"}, "kind": {"$ref": "#/$defs/kind"}, "node_type": {"$ref": "#/$defs/ident"}}},
            {"type": "object", "additionalProperties": false, "required": ["at"],
             "properties": {"at": {"const": "same_as_source"}}}
        ]
    });
    let counts = json!({"type": "object", "propertyNames": {"$ref": "#/$defs/ident"},
                        "additionalProperties": {"type": "integer", "minimum": 0, "maximum": MAX_MIN_COUNT}});
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://github.com/broomstick-ai/cauldron/schema/soup/grammar.schema.json",
        "title": format!("A user-defined grammar file ({FORMAT})"),
        "description": "A grammar a cauldron can register as a new pool (`cauldron soup grammar check | add`). This schema \
                        describes the file's shape and names. `cauldron soup grammar check` also judges what needs the \
                        cauldron (names, kinds and prefixes already taken; reach targets), the reach rules (which \
                        built-in types an edge may reach, and edge names reserved to the built-in grammars) and \
                        that a pool's id prefixes and type names are once per cauldron. The display contract (every \
                        type's `handle`, `$defs/handle`) is here. Generated from \
                        libs/spine/src/user_grammar.rs; do not edit by hand.",
        "type": "object",
        "additionalProperties": false,
        "required": ["format", "kind", "version", "node_types"],
        "properties": {
            "format": {"const": FORMAT},
            "pool": {"$ref": "#/$defs/kind"},
            "kind": {"$ref": "#/$defs/kind", "description": "A user kind always has a hyphen."},
            "version": {"type": "integer", "minimum": 1, "maximum": MAX_VERSION},
            "node_types": {
                "type": "object", "minProperties": 1, "maxProperties": MAX_NODE_TYPES,
                "propertyNames": {"$ref": "#/$defs/ident"},
                "additionalProperties": {"$ref": "#/$defs/node_type"}
            },
            "edge_kinds": {
                "type": "object", "maxProperties": MAX_EDGE_KINDS,
                "propertyNames": {"$ref": "#/$defs/ident"},
                "additionalProperties": {"$ref": "#/$defs/edge_kind"}
            }
        },
        "$defs": {
            "ident": ident,
            "kind": {"type": "string", "pattern": "^[a-z][a-z0-9]*(-[a-z0-9]+)+$", "maxLength": 32},
            "field": field,
            "handle": {
                "description": "The display contract: every type's `handle` is required text, at least 1 character and at most 235.",
                "type": "object",
                "additionalProperties": false,
                "required": ["required", "kind", "min_chars", "max_chars"],
                "properties": {
                    "required": {"const": true},
                    "kind": {"const": "text"},
                    "min_chars": {"type": "integer", "minimum": 1, "maximum": HANDLE_CAP},
                    "max_chars": {"type": "integer", "minimum": 1, "maximum": HANDLE_CAP}
                }
            },
            "type_ref": type_ref,
            "counts": counts,
            "node_type": {
                "type": "object",
                "additionalProperties": false,
                "required": ["fields", "id_prefix"],
                "properties": {
                    "fields": {"type": "object", "required": ["handle"], "maxProperties": MAX_FIELDS,
                               "propertyNames": {"$ref": "#/$defs/ident"},
                               "properties": {"handle": {"$ref": "#/$defs/handle"}},
                               "additionalProperties": {"$ref": "#/$defs/field"}},
                    "states": {"type": "array", "maxItems": MAX_STATES,
                               "items": {"type": "string", "pattern": "^[a-z][a-z0-9_-]{0,39}$"}},
                    "count_cells": {"type": "object", "maxProperties": 0},
                    "id_prefix": {"type": "string", "pattern": "^[A-Z][A-Z0-9]{0,5}$"},
                    "required_edges_at_mint": {"$ref": "#/$defs/counts"},
                    "required_at_mint_any_of": {
                        "type": "array", "maxItems": MAX_ALTERNATIVES,
                        "items": {"type": "object", "additionalProperties": false, "properties": {
                            "edges": {"$ref": "#/$defs/counts"},
                            "fields": {"type": "array", "items": {"$ref": "#/$defs/ident"}}
                        }}
                    }
                }
            },
            "edge_kind": {
                "type": "object",
                "additionalProperties": false,
                "required": ["from", "to"],
                "properties": {
                    "from": {"type": "array", "minItems": 1, "maxItems": MAX_ENDPOINTS, "items": {"$ref": "#/$defs/type_ref"}},
                    "to": {"type": "array", "minItems": 1, "maxItems": MAX_ENDPOINTS, "items": {"$ref": "#/$defs/type_ref"}},
                    "at_most_one_per_from": {"type": "boolean"},
                    "at_most_one_per_to": {"type": "boolean"},
                    "fields": {"type": "object", "maxProperties": MAX_EDGE_FIELDS,
                               "propertyNames": {"$ref": "#/$defs/ident"},
                               "additionalProperties": {"$ref": "#/$defs/field"}}
                }
            }
        }
    })
}

/// The meta-schema as published: pretty JSON with a final newline.
pub fn meta_schema_text() -> String {
    let mut s = serde_json::to_string_pretty(&meta_schema()).expect("a value serializes");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests;
