//! **What a ripple may write, and what it already wrote.**
//!
//! The first ripple turns one dump into new records. Before the door lands
//! its batch, [`check_batch`] holds it to this stage's limits, which are
//! machinery and not advice:
//!
//! - NEW RECORDS ONLY: every record is minted under a fresh id (the batch's
//!   ids are batch-local labels), so nothing already in the soup is edited,
//!   closed or superseded; a `supersedes` edge is refused;
//! - only the types in [`TYPES`] (no `directive`, which needs the owner's
//!   `operator`; no `region`; no `milestone`, `handoff` or `viewer`);
//! - every record sits in at least one region the soup already holds
//!   (`in_region`), and links only with `ref` and `derived_from`, to a node
//!   the soup holds or to another record of the batch, never to itself;
//! - each label names one record, and no label equals an id the soup holds
//!   (`duplicate-id-in-batch`, `label-shadows-existing`, as the record
//!   import names them);
//! - no `provenance` key (who wrote it is the door's to say) and no
//!   `meta.approved` or `meta.rejected` (a ripple never reviews; the spine
//!   refuses both too);
//! - no cluster and no claim: a ripple writes neither in this release
//!   ([`NO_CLUSTERS`]);
//! - at most [`MAX_RECORDS`] records.
//!
//! [`land`] then writes the batch as ONE statement of `CREATE`s (never the
//! record import's `IMPORT` form, which only an agent may use) under the
//! ripple initiator the door minted.
//!
//! [`run_landed`] answers which records a run already landed, read from the
//! spine's own record of who wrote what, so the same run is never landed
//! twice.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use spine::pool::{NodeId, Pool};
use spine::schema_def::PoolName;

use crate::reads::DEFAULT_POOL;
use crate::{Code, Error, Result};

/// The node types a ripple may mint in this stage.
pub const TYPES: [&str; 7] = ["idea", "issue", "question", "decision", "constraint", "task", "danger"];

/// The edge kinds a ripple's record may carry.
pub const EDGES: [&str; 3] = ["in_region", "ref", "derived_from"];

/// Why a ripple's batch may hold no cluster and no claim, said whenever it
/// holds one. Clusters and their claims are written by a session
/// (`soup_put`, `soup_claim`) or at a terminal (`cauldron soup import |
/// claim`); a later release may let a ripple propose them.
pub const NO_CLUSTERS: &str = "a ripple writes no cluster and no claim in this release: clusters and their claims are written by a session (soup_put, soup_claim) or at a terminal (cauldron soup import, cauldron soup claim)";

/// The most records one batch may hold.
pub const MAX_RECORDS: usize = 25;

/// The keys a record may carry at its top level (the published shape's,
/// less what is the door's to set).
const KEYS: [&str; 7] = ["id", "type", "state", "handle", "body", "meta", "edges"];

fn store(e: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, e.to_string())
}

/// **Hold `records` to this stage's limits** against the soup of `c`.
/// Answers every problem found (empty: the batch may be judged by the
/// grammar), each `{record, problem}`, and `code` for the two the record
/// import names too: a label used twice (`duplicate-id-in-batch`) and a
/// label equal to an id the cauldron holds (`label-shadows-existing`).
pub fn check_batch(c: &env::Cauldron, records: &[Value]) -> Result<Vec<Value>> {
    use spine::migrate::records::{REFUSE_DUPLICATE, REFUSE_LABEL_SHADOWS};
    let mut problems = Vec::new();
    let mut named: Vec<Value> = Vec::new();
    let mut say = |record: &str, problem: String| problems.push(json!({"record": record, "problem": problem}));
    if records.is_empty() {
        say("", "the batch holds no record".into());
    }
    if records.len() > MAX_RECORDS {
        say("", format!("the batch holds {} records; a ripple may write at most {MAX_RECORDS} at once", records.len()));
    }
    let pool = PoolName::new(DEFAULT_POOL);
    let handle = Pool::open_read_only(&Pool::path_in(&c.soup(), &pool)).map_err(store)?;
    let snap = handle.snapshot().map_err(store)?;
    let labels: BTreeSet<&str> = records.iter().filter_map(|r| r.get("id").and_then(Value::as_str)).collect();
    // Each label names one record of the batch, and none names a node the
    // cauldron already holds: the refusals the record import makes, by the
    // same names. A repeated label would land two records that every report
    // keyed by label shows as one; a label equal to a held id would turn a
    // reference meant for that node into one to the new record.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for l in records.iter().filter_map(|r| r.get("id").and_then(Value::as_str)) {
        if !seen.insert(l) {
            named.push(json!({"record": l, "code": REFUSE_DUPLICATE, "problem": format!("`{l}` is named by two records of this batch; each label names one record")}));
        } else if snap.node(&NodeId(l.to_string())).map_err(store)?.is_some() {
            named.push(json!({"record": l, "code": REFUSE_LABEL_SHADOWS, "problem": format!("the label `{l}` equals an id the cauldron already holds; a label must not shadow a held node")}));
        }
    }
    for (i, r) in records.iter().enumerate() {
        let label = r.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("#{i}"));
        let Some(obj) = r.as_object() else {
            say(&label, "a record is a JSON object".into());
            continue;
        };
        if r.get("id").and_then(Value::as_str).is_none_or(|s| s.trim().is_empty()) {
            say(&label, "a record needs an `id`, a label of this batch".into());
        }
        if let Err(e) = crate::notes::refuse_long_notes(std::slice::from_ref(r)) {
            say(&label, e.message);
        }
        for k in obj.keys() {
            if !KEYS.contains(&k.as_str()) {
                say(&label, format!("`{k}` is not a key a ripple's record may carry (it may carry {})", KEYS.join(", ")));
            }
        }
        match r.get("type").and_then(Value::as_str) {
            Some(t) if TYPES.contains(&t) => {}
            Some(spine::grammar_cauldron::CLUSTER) => say(&label, format!("`cluster`: {NO_CLUSTERS}")),
            Some(t) => say(&label, format!("a ripple may not write a `{t}`; it writes only {}", TYPES.join(", "))),
            None => say(&label, "a record needs a `type`".into()),
        }
        if r.get("meta").is_some_and(|m| !m.is_object()) {
            say(&label, "`meta` is an object".into());
        }
        // The same checks every record write makes (`review::review_flag`,
        // `review::merge_provenance`).
        if let Some(k) = crate::review::review_flag(r) {
            say(&label, format!("a ripple never sets or clears a review flag (`{k}`): its records stay unreviewed until the owner approves or rejects them"));
        }
        if let Some(k) = crate::review::merge_provenance(r) {
            say(&label, format!("a ripple never writes `{k}`: where a record came from by a merge is written only by `cauldron merge`"));
        }
        let edges = match r.get("edges") {
            None => vec![],
            Some(Value::Array(e)) => e.clone(),
            Some(_) => {
                say(&label, "`edges` is a list".into());
                vec![]
            }
        };
        let mut in_region = 0;
        for e in &edges {
            let kind = e.get("kind").and_then(Value::as_str).unwrap_or("");
            let to = e.get("to").and_then(Value::as_str).unwrap_or("");
            if let Some(k) = e.as_object().and_then(|o| o.keys().find(|k| !["kind", "to", "note"].contains(&k.as_str()))) {
                say(&label, format!("an edge carries `kind`, `to` and an optional `note`, not `{k}`"));
            }
            if e.get("note").is_some_and(|n| !n.is_string()) {
                say(&label, "an edge's `note` is a string".into());
            }
            if kind == spine::grammar_cauldron::CLAIMS {
                say(&label, format!("a `claims` edge: {NO_CLUSTERS}"));
                continue;
            }
            if !EDGES.contains(&kind) {
                say(&label, format!("a ripple's record may not carry a `{kind}` edge; it carries only {}", EDGES.join(", ")));
                continue;
            }
            if r.get("id").and_then(Value::as_str) == Some(to) {
                say(&label, format!("a record may not `{kind}` itself"));
                continue;
            }
            let existing = snap.node(&NodeId(to.to_string())).map_err(store)?;
            if kind == "in_region" {
                match existing {
                    Some(n) if n.node_type == "region" => in_region += 1,
                    _ => say(&label, format!("`in_region` must name a region the cauldron already holds; `{to}` is not one")),
                }
            } else if existing.is_none() && !labels.contains(to) {
                say(&label, format!("`{kind}` names `{to}`, which is neither a node of the cauldron nor a record of this batch"));
            } else if existing.as_ref().is_some_and(|n| n.node_type == "region") {
                say(&label, format!("`{kind}` may not name a region (`{to}`); use `in_region`"));
            }
        }
        if in_region == 0 {
            say(&label, "every record sits in at least one region the cauldron holds (an `in_region` edge)".into());
        }
    }
    named.extend(problems);
    Ok(named)
}

/// The landed transactions run `run` wrote (a ripple initiator carrying
/// that run id), by hash, read from the soup's spine through a connection
/// that cannot write.
pub fn run_landed(c: &env::Cauldron, run: &str) -> Result<Vec<String>> {
    let conn = spine::offline::read_only_connection(&c.soup().join(spine::spine_db::SPINE_FILE)).map_err(store)?;
    let mut stmt = conn
        .prepare(
            "SELECT hash FROM transactions WHERE decided = 1 AND json_extract(initiator, '$.class') = 'ripple' \
             AND json_extract(initiator, '$.run_id') = ?1 ORDER BY seq",
        )
        .map_err(store)?;
    let rows = stmt.query_map([run], |r| r.get::<_, String>(0)).map_err(store)?;
    rows.collect::<std::result::Result<_, _>>().map_err(store)
}

/// The ids of the nodes those transactions created, sorted by type prefix
/// then serial.
pub fn created_by(c: &env::Cauldron, txs: &[String]) -> Result<Vec<String>> {
    if txs.is_empty() {
        return Ok(vec![]);
    }
    let pool = PoolName::new(DEFAULT_POOL);
    let handle = Pool::open_read_only(&Pool::path_in(&c.soup(), &pool)).map_err(store)?;
    let snap = handle.snapshot().map_err(store)?;
    let mut out = Vec::new();
    for t in handle.grammar().node_types.keys() {
        for n in snap.nodes_of(t, None).map_err(store)? {
            if snap.created_tx(&n.id).map_err(store)?.is_some_and(|h| txs.contains(&h.to_hex())) {
                out.push(n.id.0);
            }
        }
    }
    out.sort_by(|a, b| crate::reads::prefix(a).cmp(crate::reads::prefix(b)).then(crate::reads::serial(a).cmp(&crate::reads::serial(b))));
    Ok(out)
}

/// What [`land`] came to.
#[derive(Debug, Clone)]
pub enum Landed {
    /// Judged and, in commit mode, landed: label -> minted id, and the
    /// transaction (commit mode only).
    Ok { ids: std::collections::BTreeMap<String, String>, tx: Option<String> },
    /// The soup's grammar refused it; nothing landed.
    Refused { witnesses: Value },
}

/// **Judge (`commit` false) or land a checked batch** as ONE statement
/// under `initiator`: every record a `CREATE` (so each is minted under a
/// fresh id), its edges `CREATE`d beside it, every existing endpoint pinned
/// with `EXPECT COUNT(..) = 1`. Values ride as parameters; the only names
/// spliced into the text are the grammar's own (type, edge kind) and the
/// three fields a record may carry. Call [`check_batch`] first.
pub fn land(c: &spine::Cauldron, at: &env::Cauldron, records: &[Value], initiator: spine::envelope::Initiator, commit: bool) -> Result<Landed> {
    use spine::envelope::{Envelope, Instrument, Mode, Provenance, ENVELOPE_FORMAT_VERSION};
    let mut lines: Vec<String> = Vec::new();
    let mut expects: Vec<String> = Vec::new();
    let mut creates: Vec<String> = Vec::new();
    let mut edges: Vec<String> = Vec::new();
    let mut params: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    let mut label_of: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (i, r) in records.iter().enumerate() {
        label_of.insert(r["id"].as_str().unwrap_or_default().to_string(), format!("n{i}"));
    }
    let mut existing: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let pool = PoolName::new(DEFAULT_POOL);
    let handle = Pool::open_read_only(&Pool::path_in(&at.soup(), &pool)).map_err(store)?;
    let snap = handle.snapshot().map_err(store)?;
    for (i, r) in records.iter().enumerate() {
        let ty = r["type"].as_str().unwrap_or_default();
        if !TYPES.contains(&ty) {
            return Err(Error::new(Code::Usage, format!("`{ty}` is not a type a ripple may write")));
        }
        let mut fields = Vec::new();
        for k in ["state", "handle", "body", "meta"] {
            if let Some(v) = r.get(k).filter(|v| !v.is_null()) {
                fields.push(format!("{k}: ${k}_{i}"));
                params.insert(format!("{k}_{i}"), v.clone());
            }
        }
        creates.push(format!("CREATE (n{i}: {DEFAULT_POOL}/{ty} {{ {} }})", fields.join(", ")));
        for (j, e) in r.get("edges").and_then(Value::as_array).into_iter().flatten().enumerate() {
            let kind = e["kind"].as_str().unwrap_or_default();
            if !EDGES.contains(&kind) {
                return Err(Error::new(Code::Usage, format!("`{kind}` is not an edge a ripple may write")));
            }
            let to = e["to"].as_str().unwrap_or_default();
            let far = match label_of.get(to) {
                Some(b) => b.clone(),
                None => {
                    if !existing.contains_key(to) {
                        let Some(n) = snap.node(&NodeId(to.to_string())).map_err(store)? else {
                            return Err(Error::new(Code::Usage, format!("`{to}` is not a node of the cauldron")));
                        };
                        let b = format!("e{}", existing.len());
                        lines.push(format!("MATCH ({b}: {DEFAULT_POOL}/{} {{ id: $id_{b} }})", n.node_type));
                        expects.push(format!("EXPECT COUNT({b}) = 1"));
                        params.insert(format!("id_{b}"), json!(to));
                        existing.insert(to.to_string(), b);
                    }
                    existing[to].clone()
                }
            };
            let mut line = format!("CREATE (n{i}) -[{kind}]-> ({far})");
            if let Some(note) = e.get("note").and_then(Value::as_str) {
                line.push_str(&format!(" NOTE $note_{i}_{j}"));
                params.insert(format!("note_{i}_{j}"), json!(note));
            }
            edges.push(line);
        }
    }
    let ret = format!("RETURN {}", (0..records.len()).map(|i| format!("n{i}.id")).collect::<Vec<_>>().join(", "));
    let statement = lines.into_iter().chain(expects).chain(creates).chain(edges).chain(std::iter::once(ret)).collect::<Vec<_>>().join("\n");
    let env = Envelope {
        statement,
        bindings: params,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance { initiator, instrument: Instrument { name: "cauldron-mcp".into(), version: env!("CARGO_PKG_VERSION").into() } },
        mode: if commit { Mode::Commit } else { Mode::Validate },
        idempotency_nonce: Some(format!(
            "ripple-put:{}:{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        )),
    };
    let ids_of = |returned: &spine::Returned| -> std::collections::BTreeMap<String, String> {
        let row = returned.0.first();
        label_of
            .iter()
            .filter_map(|(label, b)| row.and_then(|r| r.get(&format!("{b}.id"))).and_then(Value::as_str).map(|id| (label.clone(), id.to_string())))
            .collect()
    };
    match c.submit(env) {
        Ok(spine::Outcome::Committed { tx, returned, .. }) => Ok(Landed::Ok { ids: ids_of(&returned), tx: Some(tx.to_hex()) }),
        Ok(spine::Outcome::Validated { .. }) if !commit => Ok(Landed::Ok { ids: Default::default(), tx: None }),
        Ok(other) => Err(store(format!("the batch came back {other:?}"))),
        Err(spine::error::SpineError::Refused { witnesses }) => Ok(Landed::Refused { witnesses: serde_json::to_value(witnesses).unwrap_or(Value::Null) }),
        Err(e @ spine::error::SpineError::RegionLocked { .. }) => Err(crate::region_lock::spine_error(e)),
        Err(e) => Err(store(format!("writing the batch: {e}"))),
    }
}
