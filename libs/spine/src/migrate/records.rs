//! **The PUBLISHED-RECORD import**: records in the shape published at
//! `schema/soup/workspace-v6/` — `{id, type, state?, <fields>, edges: [{kind,
//! to, note?, <declared edge fields>}]}` — turned into [`SourceItem`]s and
//! judged by the SAME driver the v5 regeneration uses ([`super::dry_run`],
//! [`super::land`]): every record dry-run through the door's validate mode
//! against the destination's own pinned grammar, then the whole set landed as
//! ONE statement under ONE attempt.
//!
//! What this adapter adds, and why each piece is here rather than in the
//! driver:
//!
//! - **All or nothing.** The v5 import lands the accepted subset (a
//!   migration accounts for its losses). An outside writer's batch is one
//!   tranche: [`import_records`] lands only when EVERY record is accepted,
//!   and otherwise lands nothing and says why per record.
//! - **References to nodes the soup already holds.** An edge whose `to` is
//!   not in the batch is looked up in the destination and becomes an
//!   [`SourceItem::Existing`] endpoint (bound by MATCH, pinned by EXPECT),
//!   so a record can sit in a region the soup already has. An id the
//!   destination does not hold refuses the record (`unknown-target`). A bare
//!   `to` names a record of the batch when one carries that id; `<ID>@<pool>`
//!   always names a node already held, even in the destination pool itself.
//! - **Re-minting.** With `remint`, every incoming id is a batch-local label:
//!   each record is given the next free serial for its type's prefix in the
//!   target pool (read from the spine's own serial counter, the number a
//!   CREATE would draw), references between batch records and `[[label]]`
//!   links in bodies are rewritten, and the new id is carried through the
//!   `IMPORT` form, so the landing is still one statement. Without it the
//!   ids are carried as written and a collision refuses the import.
//! - **No label shadows an existing id.** Under `remint` a batch label that
//!   equals an id the destination pool already holds is refused
//!   ([`REFUSE_LABEL_SHADOWS`]), and so, all or nothing, is the import.
//!   Otherwise a bare reference meant for the existing node would resolve to
//!   the batch record and land pointing at its new id, with nothing to say
//!   the writer meant otherwise. With the rule, a bare id that is not a
//!   label always names the node the soup holds, and `<ID>@<pool>` stays the
//!   explicit form.
//! - **One viewer per role** (a `LEFT_OUT` rule, kept here for this
//!   adapter's own output as the v5 adapter keeps it for its own).
//! - **Records routed by type.** A batch may hold records of more than one
//!   pool's grammar: [`RecordsOptions::routes`] names, per type, the pool its
//!   records go to (the soup routes `cluster` to its `cauldron` pool), and
//!   every other type goes to [`RecordsOptions::pool`]. The batch is still
//!   ONE statement, so a cluster, a region and the cluster's claim of that
//!   region land together or not at all. A bare reference from a routed
//!   record names a record of the batch first, then a node of its own pool,
//!   then a node of the home pool (`opts.pool`), so a cluster's `claims` edge
//!   may name a held region as `R-3`; last, a node of the user grammar's
//!   pool whose type carries the id's prefix (a user grammar's prefixes are
//!   used once per cauldron), so a `film` may name a held `book` as `BK-1`.
//! - **Citations.** `meta` is open, so the grammar never looks inside it;
//!   a `meta.cites` entry of an unknown kind, or without its kind's keys
//!   ([`crate::grammar_v6::CITE_KINDS`]), refuses its record here
//!   ([`REFUSE_BAD_CITE`]), in a dry run and a real import alike.

use super::{
    dry_run, land, Disposition, ExistingNode, Landing, Manifest, MigrationPlan, Rejection,
    Resolution, SourceEdge, SourceItem, SourceNode, MARKER_ENDPOINT_REJECTED,
};
use crate::error::Result;
use crate::pool::NodeId;
use crate::schema_def::PoolName;
use crate::witness::Witness;
use crate::Cauldron;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The keys of a record the adapter consumes itself; every other top-level
/// key is a field, verbatim, and the grammar judges it.
const RECORD_KEYS: [&str; 4] = ["id", "type", "state", "edges"];
/// The keys of an edge the adapter consumes; every other key is a declared
/// edge field (`shows {cite, why}`), judged by the kind's field table.
const EDGE_KEYS: [&str; 3] = ["kind", "to", "note"];

/// A refusal the adapter makes before the door sees the record.
pub const REFUSE_SHAPE: &str = "record-shape";
/// Two records in one batch name the same id.
pub const REFUSE_DUPLICATE: &str = "duplicate-id-in-batch";
/// An edge names an id neither the batch nor the destination holds.
pub const REFUSE_UNKNOWN_TARGET: &str = "unknown-target";
/// An edge names `<ID>@<pool>` for a pool this cauldron does not have.
pub const REFUSE_UNKNOWN_POOL: &str = "unknown-pool";
/// `--remint` needs the type's id prefix, and the type has none here.
pub const REFUSE_UNKNOWN_TYPE: &str = "unknown-type";
/// A viewer for a role the destination (or the batch) already has.
pub const REFUSE_VIEWER_ROLE: &str = "viewer-role-taken";
/// An edge points at a record of the batch that was itself refused.
pub const REFUSE_TARGET_REFUSED: &str = "target-refused";
/// Under `remint`, a batch label equals an id the destination already holds.
pub const REFUSE_LABEL_SHADOWS: &str = "label-shadows-existing";
/// A `meta.cites` entry of an unknown kind, or without its kind's keys
/// ([`crate::grammar_v6::CITE_KINDS`]).
pub const REFUSE_BAD_CITE: &str = "bad-cite";

/// How to run an import.
#[derive(Debug, Clone)]
pub struct RecordsOptions {
    /// The destination pool (the soup's is `ws`): the home pool, where a
    /// record of every type not in [`RecordsOptions::routes`] goes.
    pub pool: PoolName,
    /// Node type -> the pool its records go to instead of the home pool.
    /// Empty: every record goes to the home pool.
    pub routes: BTreeMap<String, PoolName>,
    /// Treat incoming ids as batch-local and assign fresh serials.
    pub remint: bool,
    /// Land when every record is accepted. `false` is the dry run.
    pub commit: bool,
}

/// One reason a record was refused.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecordReason {
    pub code: String,
    pub detail: String,
    /// Which of the record's `edges` the reason is about, when it is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge: Option<usize>,
    /// The door's structured witnesses, verbatim, when the grammar refused.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub witnesses: Vec<Witness>,
}

/// One record's disposition.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecordDisposition {
    /// The id as the record wrote it (its batch label under `remint`).
    pub id: String,
    /// The id it lands (or would land) under, when that differs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub node_type: Option<String>,
    /// `accepted` or `refused`.
    pub disposition: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<RecordReason>,
}

/// What one import produced. A value in every arm; the caller decides what
/// to print and how to exit.
#[derive(Debug, Clone)]
pub struct RecordsImport {
    pub records: Vec<RecordDisposition>,
    /// Old id -> new id, under `remint` (predicted on a dry run).
    pub id_map: BTreeMap<String, String>,
    /// The driver's manifest, when the door was reached.
    pub manifest: Option<Manifest>,
    /// `None` on a dry run and when a refusal withheld the landing.
    pub landing: Option<Landing>,
}

impl RecordsImport {
    pub fn all_accepted(&self) -> bool {
        self.records.iter().all(|r| r.disposition == ACCEPTED)
    }
}

const ACCEPTED: &str = "accepted";
const REFUSED: &str = "refused";

/// A record the adapter could read.
struct Parsed {
    label: String,
    node_type: String,
    state: Option<String>,
    fields: BTreeMap<String, Value>,
    edges: Vec<ParsedEdge>,
}

struct ParsedEdge {
    kind: String,
    to: String,
    note: Option<String>,
    fields: BTreeMap<String, Value>,
}

/// Where an edge's far end resolved.
enum Target {
    Batch(String),
    Existing { pool: PoolName, id: String, node_type: String },
}

/// **Import published records into `cauldron`'s pool `opts.pool`** — dry run
/// every one through the door, and with `opts.commit` land them as ONE
/// statement when, and only when, every record is accepted.
///
/// `Err` is reserved for misuse and physics (a plan the driver refuses, a
/// store that will not answer); a refused record is a value.
pub fn import_records(
    cauldron: &Cauldron,
    plan: &MigrationPlan,
    records: &[Value],
    opts: &RecordsOptions,
) -> Result<RecordsImport> {
    plan.check()?;
    let home = &opts.pool;
    let grammar = cauldron.grammar(home).cloned().ok_or_else(|| {
        crate::error::SpineError::usage(format!("this cauldron has no pool `{home}`"))
    })?;
    // The pool a record of `node_type` goes to.
    let pool_of = |node_type: &str| -> PoolName { opts.routes.get(node_type).cloned().unwrap_or_else(|| home.clone()) };

    // ---- read every record; refusals of shape are made here ----
    let mut out: Vec<RecordDisposition> = Vec::with_capacity(records.len());
    let mut parsed: Vec<Option<Parsed>> = Vec::with_capacity(records.len());
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (i, raw) in records.iter().enumerate() {
        let label = raw
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("#{i}"));
        let node_type = raw.get("type").and_then(Value::as_str).map(str::to_string);
        let mut d = RecordDisposition {
            id: label.clone(),
            new_id: None,
            node_type: node_type.clone(),
            disposition: ACCEPTED,
            reasons: Vec::new(),
        };
        match parse(raw) {
            Ok(p) => {
                let pool = pool_of(&p.node_type);
                let routed = cauldron.grammar(&pool).cloned();
                if routed.is_none() {
                    refuse(
                        &mut d,
                        REFUSE_UNKNOWN_POOL,
                        format!("a `{}` record goes to pool `{pool}`, which this cauldron does not have", p.node_type),
                        None,
                    );
                    parsed.push(None);
                } else if let Some((j, why)) = edge_keys(&p, routed.as_ref().unwrap_or(&grammar), &pool) {
                    refuse(&mut d, REFUSE_SHAPE, why, Some(j));
                    parsed.push(None);
                } else if let Some(why) = p.fields.get(META).and_then(crate::grammar_v6::cite_problem) {
                    refuse(&mut d, REFUSE_BAD_CITE, format!("`{}`: {why}", p.label), None);
                    parsed.push(None);
                } else if !seen.insert(p.label.clone()) {
                    refuse(&mut d, REFUSE_DUPLICATE, format!("`{}` is named by two records of this batch", p.label), None);
                    parsed.push(None);
                } else {
                    parsed.push(Some(p));
                }
            }
            Err(why) => {
                refuse(&mut d, REFUSE_SHAPE, why, None);
                parsed.push(None);
            }
        }
        out.push(d);
    }
    let labels: BTreeSet<String> = parsed.iter().flatten().map(|p| p.label.clone()).collect();

    // Every lookup of a held node goes through one handle per pool, opened
    // once for the whole batch (not one connection per record).
    let mut handles: BTreeMap<PoolName, crate::pool::Pool> = BTreeMap::new();
    let mut held_node = |pool: &PoolName, id: &str| -> Result<Option<crate::pool::NodeRecord>> {
        if !handles.contains_key(pool) {
            handles.insert(pool.clone(), cauldron.open_pool(pool)?);
        }
        let snapshot = handles[pool].snapshot()?;
        snapshot.node(&NodeId(id.to_string()))
    };

    // ---- ids: carried as written, or re-minted from the serial counters ----
    let mut id_map: BTreeMap<String, String> = BTreeMap::new();
    let mut new_id: BTreeMap<String, String> = BTreeMap::new();
    if opts.remint {
        // A label equal to a held id would capture every bare reference
        // meant for the held node: refused, naming the node it would hide.
        for (i, p) in parsed.iter().enumerate() {
            let Some(p) = p else { continue };
            let own = pool_of(&p.node_type);
            // A routed record's bare references also reach the home pool,
            // so its label may shadow a node of either.
            let mut pools = vec![own.clone()];
            if own != *home {
                pools.push(home.clone());
            }
            for pool in &pools {
                let Some(held) = held_node(pool, &p.label)? else { continue };
                let what = held
                    .fields
                    .get("handle")
                    .and_then(|f| f.value.as_str())
                    .map(|h| format!(" ({}: \"{h}\")", held.node_type))
                    .unwrap_or_else(|| format!(" ({})", held.node_type));
                refuse(
                    &mut out[i],
                    REFUSE_LABEL_SHADOWS,
                    format!(
                        "under --remint `{}` is a batch label, and pool `{pool}` already holds a node `{}`{what}; a \
                         bare reference to `{}` could mean either. Give this record a label that is not an id \
                         (e.g. `new-1`); a reference to the existing node stays `{}` or `{}@{pool}`",
                        p.label, p.label, p.label, p.label, p.label
                    ),
                    None,
                );
                break;
            }
        }
        let mut next_of: BTreeMap<PoolName, BTreeMap<String, u64>> = BTreeMap::new();
        for (i, p) in parsed.iter().enumerate() {
            let Some(p) = p else { continue };
            if out[i].disposition == REFUSED {
                continue;
            }
            let pool = pool_of(&p.node_type);
            if !next_of.contains_key(&pool) {
                let counters = cauldron.serial_counters(&pool)?.into_iter().map(|(k, v)| (k, v + 1)).collect();
                next_of.insert(pool.clone(), counters);
            }
            let next = next_of.get_mut(&pool).expect("inserted above");
            let prefix = cauldron.grammar(&pool).and_then(|g| g.node_types.get(&p.node_type)).and_then(|t| t.id_prefix.clone());
            let Some(prefix) = prefix else {
                refuse(
                    &mut out[i],
                    REFUSE_UNKNOWN_TYPE,
                    format!("pool `{pool}` has no type `{}` with an id prefix to mint under", p.node_type),
                    None,
                );
                continue;
            };
            let n = next.entry(prefix.clone()).or_insert(1);
            let id = format!("{prefix}-{n}");
            *n += 1;
            id_map.insert(p.label.clone(), id.clone());
            new_id.insert(p.label.clone(), id.clone());
            out[i].new_id = Some(id);
        }
    } else {
        for p in parsed.iter().flatten() {
            new_id.insert(p.label.clone(), p.label.clone());
        }
    }

    // ---- far ends: the batch first, then the destination ----
    let mut existing: BTreeMap<String, ExistingNode> = BTreeMap::new();
    let mut targets: Vec<Vec<Option<Target>>> = Vec::with_capacity(parsed.len());
    for (i, p) in parsed.iter().enumerate() {
        let mut row = Vec::new();
        if let Some(p) = p {
            let pool = pool_of(&p.node_type);
            for (j, e) in p.edges.iter().enumerate() {
                let (id, at) = match e.to.split_once('@') {
                    Some((id, p2)) => (id.to_string(), Some(PoolName::new(p2))),
                    None => (e.to.clone(), None),
                };
                let mut target_pool = at.clone().unwrap_or_else(|| pool.clone());
                // A bare id names the batch first; `<ID>@<pool>` always names
                // a node the destination already holds (even this pool's).
                if at.is_none() && labels.contains(&id) {
                    row.push(Some(Target::Batch(id)));
                    continue;
                }
                if cauldron.grammar(&target_pool).is_none() {
                    refuse(
                        &mut out[i],
                        REFUSE_UNKNOWN_POOL,
                        format!("edge {j} ({}) names pool `{target_pool}`, which this cauldron does not have", e.kind),
                        Some(j),
                    );
                    row.push(None);
                    continue;
                }
                let mut held_in = |pool: &PoolName| -> Result<Option<String>> { Ok(held_node(pool, &id)?.map(|n| n.node_type)) };
                let mut held = held_in(&target_pool)?;
                // A routed record's bare reference falls back to the home pool.
                let mut looked_in = vec![target_pool.clone()];
                if held.is_none() && at.is_none() && pool != *home {
                    held = held_in(home)?;
                    looked_in.push(home.clone());
                    if held.is_some() {
                        target_pool = home.clone();
                    }
                }
                // A bare reference into a USER grammar's pool (a kind with a
                // hyphen) is found by its id prefix: a user grammar's
                // prefixes are used once per cauldron
                // (`user_grammar::check_user`), so the prefix names the pool.
                if held.is_none() && at.is_none() {
                    let pre = id.split_once('-').map(|(p, _)| p).unwrap_or("");
                    let owner = cauldron.pools().find(|p| {
                        !looked_in.contains(p)
                            && cauldron.grammar(p).is_some_and(|g| {
                                crate::user_grammar::is_user_kind(g.kind.as_str())
                                    && g.node_types.values().any(|d| d.id_prefix.as_deref() == Some(pre))
                            })
                    });
                    if let Some(p) = owner.cloned() {
                        held = held_in(&p)?;
                        looked_in.push(p.clone());
                        if held.is_some() {
                            target_pool = p;
                        }
                    }
                }
                match held {
                    Some(node_type) => row.push(Some(Target::Existing { pool: target_pool, id, node_type })),
                    None => {
                        let pools = looked_in.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(" nor of pool ");
                        refuse(
                            &mut out[i],
                            REFUSE_UNKNOWN_TARGET,
                            format!(
                                "edge {j} ({}) points at `{}`, which is neither a record of this batch nor a node of pool {pools}",
                                e.kind, e.to
                            ),
                            Some(j),
                        );
                        row.push(None);
                    }
                }
            }
        }
        targets.push(row);
    }

    // The lookups are done: their connections close before anything is
    // judged or landed.
    drop(held_node);
    drop(handles);

    // ---- one viewer per role ----
    let mut roles: BTreeMap<String, String> = BTreeMap::new();
    {
        let handle = cauldron.open_pool(home)?;
        let snapshot = handle.snapshot()?;
        for v in snapshot.nodes_of(VIEWER, None)? {
            if let Some(role) = v.fields.get(VIEWER_ROLE).and_then(|f| f.value.as_str()) {
                roles.insert(role.to_string(), v.id.0.clone());
            }
        }
    }
    for (i, p) in parsed.iter().enumerate() {
        let Some(p) = p else { continue };
        if p.node_type != VIEWER {
            continue;
        }
        let Some(role) = p.fields.get(VIEWER_ROLE).and_then(Value::as_str) else { continue };
        if let Some(held) = roles.get(role) {
            refuse(
                &mut out[i],
                REFUSE_VIEWER_ROLE,
                format!("role `{role}` already has viewer `{held}`; one viewer per role"),
                None,
            );
        } else {
            roles.insert(role.to_string(), p.label.clone());
        }
    }

    // ---- a record pointing at a refused record of the batch is refused too
    //      (to a fixpoint: the refusal travels along chains) ----
    let index_of: BTreeMap<String, usize> = parsed
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.as_ref().map(|p| (p.label.clone(), i)))
        .collect();
    loop {
        let mut changed = false;
        for i in 0..parsed.len() {
            if out[i].disposition == REFUSED {
                continue;
            }
            for (j, t) in targets[i].iter().enumerate() {
                let Some(Target::Batch(label)) = t else { continue };
                if out[index_of[label]].disposition == REFUSED {
                    refuse(
                        &mut out[i],
                        REFUSE_TARGET_REFUSED,
                        format!("edge {j} points at `{label}`, which this batch refuses"),
                        Some(j),
                    );
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // ---- source items, for every record the adapter did not refuse ----
    let mut items: Vec<SourceItem> = Vec::new();
    let mut edge_owner: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (i, p) in parsed.iter().enumerate() {
        let Some(p) = p else { continue };
        if out[i].disposition == REFUSED {
            continue;
        }
        let mut fields = p.fields.clone();
        if opts.remint {
            if let Some(Value::String(body)) = fields.get(BODY) {
                fields.insert(BODY.to_string(), Value::String(relink(body, &id_map)));
            }
        }
        let pool = pool_of(&p.node_type);
        items.push(SourceItem::Node(SourceNode {
            source_id: p.label.clone(),
            pool: pool.clone(),
            node_type: p.node_type.clone(),
            state: p.state.clone(),
            carried_id: Some(new_id[&p.label].clone()),
            fields,
            transforms: Vec::new(),
        }));
        for (j, e) in p.edges.iter().enumerate() {
            let dst = match &targets[i][j] {
                Some(Target::Batch(label)) => label.clone(),
                Some(Target::Existing { pool: tp, id, node_type }) => {
                    let sid = format!("{id}@{tp}");
                    existing.entry(sid.clone()).or_insert_with(|| ExistingNode {
                        source_id: sid.clone(),
                        pool: tp.clone(),
                        node_type: node_type.clone(),
                        id: id.clone(),
                    });
                    sid
                }
                None => unreachable!("an unresolved edge refused its record above"),
            };
            let sid = format!("{}#edges[{j}]", p.label);
            edge_owner.insert(sid.clone(), (i, j));
            items.push(SourceItem::Edge(SourceEdge {
                source_id: sid,
                pool: pool.clone(),
                kind: e.kind.clone(),
                src: p.label.clone(),
                dst,
                note: e.note.clone(),
                fields: e.fields.clone(),
                transforms: Vec::new(),
            }));
        }
    }
    items.extend(existing.into_values().map(SourceItem::Existing));

    if items.is_empty() {
        return Ok(RecordsImport { records: out, id_map, manifest: None, landing: None });
    }

    // ---- the door's judgment: the whole batch at once first (ONE
    // validate of the landing statement itself); item by item only when
    // that refuses, to say which record and why. (Never a commit as the
    // judge: a commit the door refuses late leaves an aborted row in the
    // spine, and a refused import writes nothing.) ----
    let adapter_accepted = out.iter().all(|r| r.disposition == ACCEPTED);
    let whole = if adapter_accepted { super::validate_whole(cauldron, plan, &items)? } else { None };
    let manifest = match whole {
        Some(m) => m,
        None => dry_run(cauldron, plan, &items)?,
    };
    let index: BTreeMap<&str, usize> = parsed
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.as_ref().map(|p| (p.label.as_str(), i)))
        .collect();
    for entry in &manifest.entries {
        let (i, edge) = match edge_owner.get(&entry.source_id) {
            Some(&(i, j)) => (i, Some(j)),
            None => match index.get(entry.source_id.as_str()) {
                Some(&i) => (i, None),
                // An existing endpoint: it was found when the adapter looked,
                // so a refusal here is a race; it refuses the records whose
                // edges point at it, through their edge entries below.
                None => continue,
            },
        };
        match &entry.disposition {
            Disposition::Rejected { reason } => {
                let Rejection { code, detail, witnesses } = reason;
                out[i].reasons.push(RecordReason {
                    code: code.clone(),
                    detail: detail.clone(),
                    edge,
                    witnesses: witnesses.clone(),
                });
                out[i].disposition = REFUSED;
            }
            Disposition::Imported { resolution: Resolution::UnresolvedEndpoint { source_ref, marker, .. }, .. } => {
                // The record's own node was refused: its reason is already
                // there, and its edges failing with it says nothing more.
                if parsed[i].as_ref().is_some_and(|p| &p.label == source_ref) {
                    continue;
                }
                let code = if marker == MARKER_ENDPOINT_REJECTED { REFUSE_TARGET_REFUSED } else { REFUSE_UNKNOWN_TARGET };
                out[i].reasons.push(RecordReason {
                    code: code.to_string(),
                    detail: format!("this edge's endpoint `{source_ref}` will not land ({marker})"),
                    edge,
                    witnesses: Vec::new(),
                });
                out[i].disposition = REFUSED;
            }
            Disposition::Imported { .. } => {}
        }
    }

    let mut result = RecordsImport { records: out, id_map, manifest: Some(manifest), landing: None };
    if !opts.commit || !result.all_accepted() {
        return Ok(result);
    }
    // ---- every record accepted: ONE statement, ONE attempt ----
    let manifest = result.manifest.take().expect("set above");
    let (manifest, landing) = land(cauldron, plan, &items, manifest)?;
    result.manifest = Some(manifest);
    result.landing = Some(landing);
    Ok(result)
}

const VIEWER: &str = "viewer";
const VIEWER_ROLE: &str = "role";
const BODY: &str = "body";
const META: &str = "meta";

fn refuse(d: &mut RecordDisposition, code: &str, detail: String, edge: Option<usize>) {
    d.disposition = REFUSED;
    d.reasons.push(RecordReason { code: code.to_string(), detail, edge, witnesses: Vec::new() });
}

/// Read one record in the published shape. Only the SHAPE is judged here
/// (an object, a string `id` and `type`, a list of edges each with `kind`
/// and `to`); every value is the grammar's to judge at the door.
fn parse(raw: &Value) -> std::result::Result<Parsed, String> {
    let obj = raw.as_object().ok_or("a record is an object")?;
    let label = match obj.get("id") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
        _ => return Err("a record carries a non-empty string `id`".into()),
    };
    if label.contains('@') || label.contains('#') {
        return Err(format!("`{label}`: an id never contains `@` or `#`"));
    }
    let node_type = match obj.get("type") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err(format!("`{label}` carries no string `type`")),
    };
    let state = match obj.get("state") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(format!("`{label}`: `state` is one word, a string")),
    };
    let mut edges = Vec::new();
    match obj.get("edges") {
        None | Some(Value::Null) => {}
        Some(Value::Array(list)) => {
            for (j, e) in list.iter().enumerate() {
                let eo = e.as_object().ok_or_else(|| format!("`{label}` edge {j} is not an object"))?;
                let kind = eo.get("kind").and_then(Value::as_str).filter(|s| !s.is_empty());
                let to = eo.get("to").and_then(Value::as_str).filter(|s| !s.is_empty());
                let (Some(kind), Some(to)) = (kind, to) else {
                    return Err(format!("`{label}` edge {j} needs a string `kind` and a string `to`"));
                };
                let note = match eo.get("note") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(_) => return Err(format!("`{label}` edge {j}: `note` is free text, a string")),
                };
                let fields: BTreeMap<String, Value> = eo
                    .iter()
                    .filter(|(k, _)| !EDGE_KEYS.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                // A `note` beside other keys is judged against the grammar's
                // edge table in [`edge_keys`], which can name the key.
                edges.push(ParsedEdge { kind: kind.to_string(), to: to.to_string(), note, fields });
            }
        }
        Some(_) => return Err(format!("`{label}`: `edges` is a list")),
    }
    let fields = obj
        .iter()
        .filter(|(k, _)| !RECORD_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Ok(Parsed { label, node_type, state, fields, edges })
}

/// An edge that carries a `note` beside other keys, judged against the
/// pool's edge table: the first such edge's index and the reason, or `None`.
///
/// The soup stores a `note` OR declared fields on an edge, never both, so
/// such an edge is refused here (the driver would refuse it as misuse). The
/// reason names what is actually wrong: a key the kind does not declare
/// (`weight` on `in_region`, which declares no fields), or a kind the pool
/// does not have; only an edge whose every other key IS declared (`shows`
/// with `cite` and `why`) is told that a note and declared fields do not go
/// together. An edge without a note goes to the door, which names an
/// undeclared key itself (`undeclared_edge_field`).
fn edge_keys(p: &Parsed, grammar: &crate::schema_def::PoolGrammar, pool: &PoolName) -> Option<(usize, String)> {
    let label = &p.label;
    for (j, e) in p.edges.iter().enumerate() {
        if e.note.is_none() || e.fields.is_empty() {
            continue;
        }
        let kind = &e.kind;
        let Some(def) = grammar.edge_kinds.get(kind) else {
            return Some((j, format!("`{label}` edge {j}: `{kind}` is not an edge kind of pool `{pool}`")));
        };
        let unknown: Vec<&str> = e.fields.keys().map(String::as_str).filter(|k| !def.fields.contains_key(*k)).collect();
        if let Some(first) = unknown.first() {
            let declared = if def.fields.is_empty() {
                "it declares none".to_string()
            } else {
                format!("it declares {}", def.fields.keys().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", "))
            };
            let what = if unknown.len() == 1 {
                format!("`{first}` is not a field of `{kind}` edges")
            } else {
                format!("{} are not fields of `{kind}` edges", unknown.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", "))
            };
            return Some((
                j,
                format!("`{label}` edge {j} ({kind}): {what} ({declared}); an edge carries `kind`, `to`, and a `note` or its kind's fields"),
            ));
        }
        return Some((
            j,
            format!("`{label}` edge {j} ({kind}) carries both `note` and declared fields; the soup stores one or the other on an edge"),
        ));
    }
    None
}

/// Rewrite every `[[label]]` link whose label the batch re-minted, in one
/// pass (so a new id that happens to equal another label is never rewritten
/// twice). Anything else between brackets is left as written.
fn relink(text: &str, map: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("]]") {
            Some(end) => {
                let inner = &after[..end];
                out.push_str("[[");
                out.push_str(map.get(inner).map(String::as_str).unwrap_or(inner));
                out.push_str("]]");
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relink_rewrites_only_batch_labels_in_one_pass() {
        let map: BTreeMap<String, String> =
            [("IS-1".to_string(), "IS-2".to_string()), ("IS-2".to_string(), "IS-3".to_string())].into();
        assert_eq!(relink("see [[IS-1]] and [[IS-2]], not [[DEC-9]] or [[", &map), "see [[IS-2]] and [[IS-3]], not [[DEC-9]] or [[");
    }

    #[test]
    fn parse_refuses_shapes_and_keeps_fields_verbatim() {
        assert!(parse(&serde_json::json!([])).is_err());
        assert!(parse(&serde_json::json!({"type": "issue"})).is_err());
        assert!(parse(&serde_json::json!({"id": "IS-1", "type": "issue", "edges": [{"kind": "ref"}]})).is_err());
        // A note beside other keys parses; `edge_keys` judges it.
        assert!(parse(&serde_json::json!({"id": "V-1", "type": "viewer", "edges": [{"kind": "shows", "to": "IS-1", "note": "x", "why": "y"}]})).is_ok());
        let p = parse(&serde_json::json!({"id": "IS-1", "type": "issue", "state": "open", "handle": "h", "rev": 3})).unwrap();
        assert_eq!(p.fields.keys().collect::<Vec<_>>(), ["handle", "rev"]);
    }

    #[test]
    fn edge_keys_names_the_unknown_key_and_keeps_the_note_rule_for_declared_ones() {
        let g = crate::grammar_v6::workspace_v6("ws");
        let pool = PoolName::new("ws");
        let judge = |r: Value| edge_keys(&parse(&r).unwrap(), &g, &pool).map(|(_, why)| why);
        // Issue #18: `weight` on an in_region edge with a note.
        let why = judge(serde_json::json!({"id": "n1", "type": "decision",
            "edges": [{"kind": "in_region", "to": "R-1", "note": "x", "weight": 1}]})).unwrap();
        assert!(why.contains("`weight` is not a field of `in_region` edges") && why.contains("it declares none"), "{why}");
        assert!(!why.contains("both `note` and declared fields"), "{why}");
        // Two unknown keys are both named; an unknown kind is named.
        let why = judge(serde_json::json!({"id": "n1", "type": "decision",
            "edges": [{"kind": "ref", "to": "R-1", "note": "x", "weight": 1, "colour": "red"}]})).unwrap();
        assert!(why.contains("`colour`, `weight` are not fields of `ref` edges"), "{why}");
        let why = judge(serde_json::json!({"id": "n1", "type": "decision",
            "edges": [{"kind": "nope", "to": "R-1", "note": "x", "weight": 1}]})).unwrap();
        assert!(why.contains("`nope` is not an edge kind of pool `ws`"), "{why}");
        // A declared field beside a note keeps the note-or-fields reason.
        let why = judge(serde_json::json!({"id": "V-1", "type": "viewer",
            "edges": [{"kind": "shows", "to": "IS-1", "note": "x", "why": "y"}]})).unwrap();
        assert!(why.contains("carries both `note` and declared fields"), "{why}");
        // Opposed: a note alone, or fields alone, are the door's to judge.
        assert!(judge(serde_json::json!({"id": "n1", "type": "decision", "edges": [{"kind": "in_region", "to": "R-1", "note": "x"}]})).is_none());
        assert!(judge(serde_json::json!({"id": "n1", "type": "decision", "edges": [{"kind": "in_region", "to": "R-1", "weight": 1}]})).is_none());
    }
}
