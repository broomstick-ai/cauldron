//! **The reads**: `regions`, `region`, `nodes`, `search` and `get`, each
//! through connections that cannot write (the pool, and the spine's commit
//! order), each answering one JSON document, bounded by a limit with a
//! default and by an optional per-field `max_chars`; what was cut is listed
//! in `truncated`, and a list cut by its limit says how many `more` there
//! are.
//!
//! # Superseded nodes
//!
//! A node is SUPERSEDED when another node carries a `supersedes` edge to it
//! (the grammar's computed state: "read from an inbound `supersedes`"). An
//! update is a supersede, so without this every reader would see the stale
//! and the current version side by side. `get` always answers the node and
//! adds `superseded_by: [ids]`; `nodes`, `search` and a region's members
//! leave superseded nodes out unless [`ReadOpts::include_superseded`], and
//! each says how many it left out (`superseded_hidden`). With the option, a
//! superseded node is listed with its `superseded_by`.
//!
//! # Unreviewed records
//!
//! A record a ripple wrote (its creating transaction's initiator is of class
//! `ripple`) is UNREVIEWED until its `meta.approved` is set, which only the
//! owner's side can do (the spine's flag rule refuses it to every ripple).
//! Every read shows such a record, marked `unreviewed: true`; every other
//! record carries no such key, so a store with no ripple record reads
//! exactly as before. `nodes` and `search` take [`ReadOpts::review`]:
//! `pending` keeps only unreviewed records, `reviewed` only the rest.
//!
//! A record a merge carried from another cauldron (`crate::merge`) where a
//! ripple wrote it and nobody had decided it is unreviewed here too
//! (`crate::provenance`): a merge never reviews a record. `get` says of
//! every merged record where it came from (`merged`).
//!
//! # Rejected records
//!
//! A record a ripple wrote that the owner REJECTED (`meta.rejected` set, by
//! `cauldron ripples reject` at a terminal) is no longer unreviewed, and
//! `nodes`, `search` and a region's members leave it out unless
//! [`ReadOpts::include_rejected`], saying how many they left out
//! (`rejected_hidden`, present only when there are any). `get` always
//! answers it, marked `rejected: true`, so its cite to the dump and the
//! decision stay readable. Nothing is deleted.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use spine::pool::{NodeId, Pool, QualifiedNodeId};
use spine::schema_def::PoolName;

use crate::cut::Cut;
use crate::target::Target;
use crate::{refuse, Code, Error, Result};

/// The soup's one pool (the engine's `SOUP_POOL`).
pub const DEFAULT_POOL: &str = "ws";

/// Default bounds.
pub const NODES_LIMIT: usize = 50;
pub const SEARCH_LIMIT: usize = 20;
pub const MEMBERS_LIMIT: usize = 50;
pub const SNIPPET_CHARS: usize = 160;

const REGION_FLAGS: [&str; 5] = ["retired", "provisional", "deferred_by", "thresholds", "wake_when"];

/// The options a read takes. Every field is optional; each read uses the
/// ones it names and ignores the rest.
#[derive(Debug, Clone, Default)]
pub struct ReadOpts {
    /// The pool, already checked with [`checked_pool`]; `ws` when `None`.
    pub pool: Option<String>,
    /// Cut any longer text field to this many chars.
    pub max_chars: Option<usize>,
    /// How many list items (`nodes`, `search` results, `region` members).
    pub limit: Option<usize>,
    /// `region`: how many levels of parents and children.
    pub depth: Option<usize>,
    /// `nodes`, `search`: keep a region's direct members.
    pub region: Option<String>,
    /// `nodes`, `search`: one node type.
    pub node_type: Option<String>,
    /// `nodes`: one state.
    pub state: Option<String>,
    /// `nodes`, `search`, `region`: list superseded nodes too (they are
    /// left out, and counted, by default).
    pub include_superseded: bool,
    /// `nodes`, `search`: `pending` (only unreviewed records) or
    /// `reviewed` (only the others); checked with [`checked_review`].
    pub review: Option<String>,
    /// `nodes`, `search`, `region`: list rejected records too (they are
    /// left out, and counted, by default).
    pub include_rejected: bool,
    /// `get`: add each record's `edges_in`, the edges that arrive at it
    /// from any pool (opt in: a read adds it, and a write drops it).
    pub edges_in: bool,
}

impl ReadOpts {
    pub fn pool_name(&self) -> PoolName {
        PoolName::new(self.pool.as_deref().unwrap_or(DEFAULT_POOL))
    }
}

/// A pool names a FILE (`<soup>/<pool>.db`), so it is held to the pool name
/// grammar (`[a-z][a-z0-9-]*`, `spine` reserved) before it goes anywhere near
/// a path: a slash, `..` or an absolute path would otherwise open a file
/// outside the soup. Nothing is guessed or sanitised.
pub fn checked_pool(name: &str) -> Result<String> {
    let p = PoolName::new(name);
    if !p.is_well_formed() {
        refuse!(
            Code::Usage,
            "REFUSED: `--pool {name}` is not a pool name. A pool name is [a-z][a-z0-9-]* (a lowercase \
             letter, then lowercase letters, digits or `-`); it names a file inside the soup, so a path \
             is never accepted. Nothing was read or written."
        );
    }
    if p.is_reserved() {
        refuse!(Code::Usage, "REFUSED: `--pool {name}` names the spine's own file, not a pool. Nothing was read or written.");
    }
    Ok(name.to_string())
}

fn store(err: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, err.to_string())
}

/// The review filter's values: `pending`, `reviewed`, and who decided
/// (`by-tty`: approved or rejected by a person at a terminal; `by-agent`:
/// by an agent the owner's `owner_acts` choice allowed).
pub const REVIEW_VALUES: [&str; 4] = ["pending", "reviewed", "by-tty", "by-agent"];

/// A review filter, checked.
pub fn checked_review(v: &str) -> Result<String> {
    if REVIEW_VALUES.contains(&v) {
        return Ok(v.to_string());
    }
    refuse!(
        Code::Usage,
        "the review filter is `pending` (only records a ripple wrote that nobody approved yet), `reviewed` (every other \
         record), `by-tty` (only records approved or rejected by a person at a terminal) or `by-agent` (only records \
         approved or rejected by an agent the owner allowed), not `{v}`"
    )
}

/// Who approved or rejected a record (`meta.approved.by`, else
/// `meta.rejected.by`): `agent` when it says so, else `tty` (a decision
/// written before decisions said who, `by: "owner"`, was made at a
/// terminal: nothing else could make one). `None` for a record nobody
/// decided.
pub fn decided_by(fields: &BTreeMap<String, spine::pool::FieldValue>) -> Option<&'static str> {
    let meta = fields.get("meta")?;
    let flag = |k: &str| meta.value.get(k).filter(|a| !a.is_null() && **a != Value::Bool(false));
    let d = flag("approved").or_else(|| flag("rejected"))?;
    Some(if d.get("by").and_then(Value::as_str) == Some("agent") { "agent" } else { "tty" })
}

/// The hashes of the landed transactions a RIPPLE initiated, read from the
/// spine through a connection that cannot write. A spine with no such row
/// (every store written before ripples) answers an empty set.
pub fn ripple_commits(c: &env::Cauldron) -> Result<std::collections::HashSet<String>> {
    let conn = spine::offline::read_only_connection(&c.soup().join(spine::spine_db::SPINE_FILE))
        .map_err(|err| store(format!("reading cauldron `{}`'s spine: {err}", c.name)))?;
    let mut stmt = conn
        .prepare("SELECT hash FROM transactions WHERE decided = 1 AND json_extract(initiator, '$.class') = 'ripple'")
        .map_err(store)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(store)?;
    rows.collect::<std::result::Result<_, _>>().map_err(store)
}

/// Does a node's `meta` carry an approval (`meta.approved` present and
/// neither `false` nor `null`)?
pub fn approved(fields: &BTreeMap<String, spine::pool::FieldValue>) -> bool {
    fields
        .get("meta")
        .and_then(|m| m.value.get("approved"))
        .is_some_and(|a| !a.is_null() && *a != Value::Bool(false))
}

/// Does a node's `meta` carry a rejection (`meta.rejected` present and
/// neither `false` nor `null`)? Only the owner's `cauldron ripples reject`,
/// at a terminal, writes one.
pub fn rejected(fields: &BTreeMap<String, spine::pool::FieldValue>) -> bool {
    fields
        .get("meta")
        .and_then(|m| m.value.get("rejected"))
        .is_some_and(|a| !a.is_null() && *a != Value::Bool(false))
}

/// **A record's review mark**, the one test `get` and its `edges_in` use:
/// `rejected` for a record the owner rejected, `unreviewed` for one a ripple
/// wrote (or a merge carried from a ripple, `provenance::Writers`) that the
/// owner has not approved, else none.
fn review_mark(n: &spine::pool::NodeRecord, writers: &crate::provenance::Writers, created: Option<&str>) -> Option<&'static str> {
    if rejected(&n.fields) {
        return Some("rejected");
    }
    let meta = n.fields.get("meta").map(|f| f.value.clone()).unwrap_or(Value::Null);
    (writers.ripple_written(created, &meta) && !approved(&n.fields)).then_some("unreviewed")
}

/// The pool, through a connection that cannot write.
fn read_pool(c: &env::Cauldron, pool: &PoolName) -> Result<Pool> {
    let file = Pool::path_in(&c.soup(), pool);
    if !file.exists() {
        // A registered user pool whose file is out says so, and what to do.
        if let Some(g) = crate::grammars::missing(c)?.into_iter().find(|g| &g.pool == pool) {
            return Err(Error::new(Code::NotFound, format!("REFUSED: {}", crate::grammars::missing_note(c, &g)))
                .with_detail(json!({"reason": "pool-file-missing", "pool": pool.as_str()})));
        }
    }
    Pool::open_read_only(&file).map_err(|err| store(format!("reading cauldron `{}`'s pool `{pool}`: {err}", c.name)))
}

/// An edge's far end as the published shape writes it: the bare id in the
/// same pool, `<ID>@<pool>` in another.
pub fn target_text(pool: &PoolName, dst: &QualifiedNodeId) -> String {
    if &dst.pool == pool {
        dst.id.0.clone()
    } else {
        format!("{}@{}", dst.id.0, dst.pool)
    }
}

// ------------------------------------------------------------------ regions

/// Every region: id, handle, boundary, flags, the regions it narrows or
/// relates to, and how many nodes sit in it.
pub fn regions(t: &Target, o: &ReadOpts) -> Result<Value> {
    let (e, c) = (&t.env, &t.cauldron);
    let pool = o.pool_name();
    let handle = read_pool(c, &pool)?;
    let snap = handle.snapshot().map_err(store)?;
    let mut out = Vec::new();
    let mut cut = Cut::new(o.max_chars);
    // The region locks held now (`region_lock`): a region a writer holds is
    // marked `locked`; an ancestor of one (held `link`: writes beside it
    // land, a change of its header or parents is refused) is marked
    // `locked_below`.
    let now = env::region_locks::now_ms();
    let locks = env::region_locks::load(c);
    for r in snap.nodes_of("region", None).map_err(store)? {
        let field = |k: &str| r.fields.get(k).map(|f| f.value.clone());
        let text = |cut: &mut Cut, k: &str| r.fields.get(k).map(|f| cut.value(&r.id.0, k, &f.value));
        let mut flags = serde_json::Map::new();
        for k in REGION_FLAGS {
            if let Some(v) = field(k) {
                flags.insert(k.to_string(), v);
            }
        }
        let edges = snap.edges_from(&r.id, None).map_err(store)?;
        let of = |kind: &str| -> Vec<String> {
            edges.iter().filter(|x| x.kind == kind).map(|x| target_text(&pool, &x.dst)).collect()
        };
        let members = snap
            .edges_to(&QualifiedNodeId { pool: pool.clone(), id: r.id.clone() }, Some("in_region"))
            .map_err(store)?
            .len();
        out.push(json!({
            "id": r.id.0,
            "handle": text(&mut cut, "handle"),
            "boundary": text(&mut cut, "boundary"),
            "flags": flags,
            "narrows": of("narrows"),
            "relates": of("relates"),
            "members": members,
        }));
        if let Ok(t) = &locks {
            if let Some(h) = env::region_locks::holder_of(t, now, pool.as_str(), &r.id.0) {
                let last = out.len() - 1;
                out[last]["locked"] = json!({"holder": h.holder, "purpose": h.purpose, "since": h.since, "until": h.until});
            } else if let Some(h) = t.live(now).into_iter().find(|x| x.pool == pool.as_str() && x.region == r.id.0 && x.mode == env::region_locks::LINK) {
                let last = out.len() - 1;
                out[last]["locked_below"] = json!({"holder": h.holder, "purpose": h.purpose, "since": h.since, "until": h.until});
            }
        }
    }
    let mut doc = json!({"env": e.name, "cauldron": c.name, "pool": pool.as_str(), "regions": out});
    if let Err(err) = &locks {
        doc["region_locks_error"] = json!(format!("{err:#}"));
    }
    crate::merge::note_pending(&t.cauldron, &mut doc);
    cut.report(&mut doc);
    Ok(doc)
}

// ---------------------------------------------------------------------- get

/// Those nodes as records in the published shape, with their outgoing
/// edges. Ids the pool does not hold are listed as `missing`.
pub fn get(t: &Target, ids: &[String], o: &ReadOpts) -> Result<Value> {
    // `<ID>@<pool>`, as an edge target reads: that pool's record, named
    // outright (a pool the soup does not hold, or an id it does not hold
    // there, is `missing` as asked).
    if o.pool.is_none() && ids.iter().any(|i| i.contains('@')) {
        let (named, plain): (Vec<&String>, Vec<&String>) = ids.iter().partition(|i| i.contains('@'));
        let plain: Vec<String> = plain.into_iter().cloned().collect();
        let mut doc = get(t, &plain, o)?;
        let held: Vec<PoolName> = crate::grammars::pins(&t.cauldron)?.into_iter().map(|g| g.pool).collect();
        let (mut records, mut missing) = (Vec::new(), Vec::new());
        for asked in named {
            let (id, p) = asked.rsplit_once('@').unwrap_or((asked, ""));
            if !PoolName::new(p).is_well_formed() || !held.contains(&PoolName::new(p)) {
                missing.push(json!(asked));
                continue;
            }
            // The soup's own pool and the cluster pool are read as a plain id
            // reads them (a cluster with its `claims`); the record must be in
            // the pool named.
            let builtin = p == DEFAULT_POOL || p == crate::clusters::POOL;
            let o2 = if builtin {
                ReadOpts { max_chars: o.max_chars, edges_in: o.edges_in, ..ReadOpts::default() }
            } else {
                ReadOpts { pool: Some(p.to_string()), max_chars: o.max_chars, edges_in: o.edges_in, ..ReadOpts::default() }
            };
            let found = get(t, &[id.to_string()], &o2)?;
            let r = found["records"].as_array().and_then(|a| a.first()).cloned().filter(|r| {
                !builtin || (r.get("pool").is_none() && ((r["type"] == crate::clusters::POOL_TYPE) == (p == crate::clusters::POOL)))
            });
            match r {
                Some(mut r) => {
                    if !builtin {
                        r["pool"] = json!(p);
                    }
                    records.push(r);
                }
                None => missing.push(json!(asked)),
            }
        }
        let mut users: Vec<String> = doc["user_pools"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        users.extend(records.iter().filter_map(|r| r["pool"].as_str().map(String::from)));
        users.sort();
        users.dedup();
        if !users.is_empty() {
            doc["user_pools"] = json!(users);
        }
        if let Some(a) = doc["records"].as_array_mut() {
            a.extend(records);
        }
        say_cluster_pool(&mut doc, "records");
        if !missing.is_empty() {
            let mut m = doc["missing"].as_array().cloned().unwrap_or_default();
            m.extend(missing);
            doc["missing"] = json!(m);
        }
        return Ok(doc);
    }
    let (e, c) = (&t.env, &t.cauldron);
    let pool = o.pool_name();
    let handle = read_pool(c, &pool)?;
    let snap = handle.snapshot().map_err(store)?;
    let writers = crate::provenance::Writers::of(c)?;
    let mut records = Vec::new();
    let mut missing = Vec::new();
    let mut cut = Cut::new(o.max_chars);
    for id in ids {
        let Some(n) = snap.node(&NodeId(id.clone())).map_err(store)? else {
            missing.push(id.clone());
            continue;
        };
        let mut rec = serde_json::Map::new();
        rec.insert("id".into(), json!(n.id.0));
        rec.insert("type".into(), json!(n.node_type));
        if let Some(s) = &n.state {
            rec.insert("state".into(), json!(s));
        }
        for (k, f) in &n.fields {
            rec.insert(k.clone(), cut.value(&n.id.0, k, &f.value));
        }
        let mut edges = Vec::new();
        for (j, x) in snap.edges_from(&n.id, None).map_err(store)?.into_iter().enumerate() {
            let mut m = serde_json::Map::new();
            m.insert("kind".into(), json!(x.kind));
            m.insert("to".into(), json!(target_text(&pool, &x.dst)));
            if let Some(note) = &x.note {
                m.insert("note".into(), json!(crate::notes::shown(&mut cut, &n.id.0, &format!("edges/{j}/note"), note)));
            }
            let fields: BTreeMap<String, Value> = x.fields.clone();
            for (k, v) in fields {
                m.insert(k, v);
            }
            edges.push(Value::Object(m));
        }
        if !edges.is_empty() {
            rec.insert("edges".into(), Value::Array(edges));
        }
        let by = superseded_by(&snap, &pool, &n.id)?;
        if !by.is_empty() {
            rec.insert("superseded_by".into(), json!(by));
        }
        if o.edges_in {
            let (list, total) = edges_in(c, &pool, &n.id, &writers)?;
            rec.insert("edges_in".into(), json!(list));
            if total > list.len() {
                rec.insert("edges_in_total".into(), json!(total));
            }
        }
        let created = snap.created_tx(&n.id).map_err(store)?.map(|h| h.to_hex());
        let meta = n.fields.get("meta").map(|f| f.value.clone()).unwrap_or(Value::Null);
        if let Some(mark) = review_mark(&n, &writers, created.as_deref()) {
            rec.insert(mark.into(), json!(true));
        }
        // A record a merge carried says so, and from where
        // (`crate::merge`); its `meta.merged_from` has the rest.
        if let Some(m) = writers.merge_of(created.as_deref(), &meta) {
            let from = &meta["merged_from"];
            rec.insert(
                "merged".into(),
                json!({"merge": m.merge, "at": m.at, "from": {"cauldron": from["cauldron"], "cauldron_id": from["cauldron_id"], "id": from["id"]}}),
            );
        }
        records.push(Value::Object(rec));
    }
    // An id the workspace pool does not hold may be a cluster's.
    if pool.as_str() == DEFAULT_POOL && !missing.is_empty() {
        let (clusters, still) = crate::clusters::get(c, &missing, &mut cut)?;
        records.extend(clusters);
        missing = still;
    }
    // Or a user grammar's record: each such record carries its `pool`.
    let mut from_user_pools = Vec::new();
    if pool.as_str() == DEFAULT_POOL && !missing.is_empty() {
        for g in crate::grammars::user_pins(c)? {
            if missing.is_empty() {
                break;
            }
            let ids = std::mem::take(&mut missing);
            let o2 = ReadOpts { pool: Some(g.pool.as_str().to_string()), max_chars: o.max_chars, edges_in: o.edges_in, ..ReadOpts::default() };
            let found = get(t, &ids, &o2)?;
            for mut r in found["records"].as_array().cloned().unwrap_or_default() {
                r["pool"] = json!(g.pool.as_str());
                records.push(r);
                from_user_pools.push(g.pool.as_str().to_string());
            }
            missing = found["missing"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        }
    }
    // An id whose prefix is a type's of a registered user pool whose file is
    // out: still missing, and the answer says why.
    let mut out_pools = Vec::new();
    if pool.as_str() == DEFAULT_POOL && !missing.is_empty() {
        for g in crate::grammars::missing(c)? {
            let held: Vec<&String> = missing
                .iter()
                .filter(|id| g.node_types.values().any(|d| d.id_prefix.as_ref().is_some_and(|p| id.starts_with(&format!("{p}-")))))
                .collect();
            if !held.is_empty() {
                out_pools.push(json!({"pool": g.pool.as_str(), "ids": held, "why": crate::grammars::missing_note(c, &g)}));
            }
        }
    }
    let mut doc = json!({"env": e.name, "cauldron": c.name, "pool": pool.as_str(), "records": records});
    if !out_pools.is_empty() {
        doc["pool_files_missing"] = json!(out_pools);
    }
    if !from_user_pools.is_empty() {
        from_user_pools.sort();
        from_user_pools.dedup();
        doc["user_pools"] = json!(from_user_pools);
    }
    say_cluster_pool(&mut doc, "records");
    if !missing.is_empty() {
        doc["missing"] = json!(missing);
    }
    crate::merge::note_pending(&t.cauldron, &mut doc);
    cut.report(&mut doc);
    Ok(doc)
}

/// An answer whose `pool` is the workspace pool and whose `list` holds a
/// cluster says where that came from: `cluster_pool: "cauldron"` (`pool`
/// keeps naming the workspace pool, as before clusters).
fn say_cluster_pool(doc: &mut Value, list: &str) {
    if doc[list].as_array().is_some_and(|a| a.iter().any(|r| r["type"] == crate::clusters::POOL_TYPE)) {
        doc["cluster_pool"] = json!(crate::clusters::POOL);
    }
}

// ---------------------------------------------------- the catalog of nodes

/// One node as the listing reads carry it.
struct Entry {
    id: String,
    node_type: String,
    state: Option<String>,
    handle: String,
    body: String,
    regions: Vec<String>,
    /// The committed-commit counter at the commit that created it; `None`
    /// when the spine has no landed row for it (not expected; sorted first).
    order: Option<i64>,
    /// The nodes whose `supersedes` edge points at it, by id; empty for a
    /// node nothing supersedes.
    superseded_by: Vec<String>,
    /// A ripple wrote it and nobody approved or rejected it yet.
    unreviewed: bool,
    /// The owner rejected it (`meta.rejected`).
    rejected: bool,
    /// Who approved or rejected it ([`decided_by`]).
    decided_by: Option<&'static str>,
}

/// Every node of the pool, newest first: by the commit that created it
/// (the spine's order, never a timestamp); nodes of one write by id prefix,
/// then serial, highest first. Read through connections that cannot write.
fn catalog(c: &env::Cauldron, pool: &PoolName, snap: &spine::pool::Snapshot<'_>, types: &[String]) -> Result<Vec<Entry>> {
    let order = spine::offline::commit_order(&c.soup())
        .map_err(|err| store(format!("reading cauldron `{}`'s commit order: {err}", c.name)))?;
    let writers = crate::provenance::Writers::of(c)?;
    let mut out = Vec::new();
    for t in types {
        for n in snap.nodes_of(t, None).map_err(store)? {
            let text = |k: &str| n.fields.get(k).and_then(|f| f.value.as_str()).unwrap_or("").to_string();
            let regions = snap
                .edges_from(&n.id, Some("in_region"))
                .map_err(store)?
                .iter()
                .map(|x| target_text(pool, &x.dst))
                .collect();
            let created = snap.created_tx(&n.id).map_err(store)?;
            let superseded_by = superseded_by(snap, pool, &n.id)?;
            let rejected = rejected(&n.fields);
            let meta = n.fields.get("meta").map(|f| f.value.clone()).unwrap_or(Value::Null);
            let unreviewed = !rejected && writers.ripple_written(created.as_ref().map(|h| h.to_hex()).as_deref(), &meta) && !approved(&n.fields);
            out.push(Entry {
                id: n.id.0.clone(),
                node_type: n.node_type.clone(),
                state: n.state.clone(),
                handle: text("handle"),
                body: text("body"),
                regions,
                order: created.and_then(|h| order.get(&h.to_hex()).copied()),
                superseded_by,
                unreviewed,
                rejected,
                decided_by: decided_by(&n.fields),
            });
        }
    }
    out.sort_by(|a, b| {
        let newest = |e: &Entry| e.order.map_or(i64::MAX, |o| o);
        newest(b).cmp(&newest(a)).then_with(|| prefix(&a.id).cmp(prefix(&b.id))).then_with(|| serial(&b.id).cmp(&serial(&a.id))).then_with(|| a.id.cmp(&b.id))
    });
    Ok(out)
}

/// A listed node with its `state`, when it has one: a region and a viewer
/// carry no state at all, so they are listed without the key (never
/// `state: null`), as `get` prints them. A superseded node (listed only
/// when asked for) carries its `superseded_by`.
fn stated(mut item: Value, n: &Entry) -> Value {
    if let Some(m) = item.as_object_mut() {
        if let Some(s) = &n.state {
            m.insert("state".into(), json!(s));
        }
        if !n.superseded_by.is_empty() {
            m.insert("superseded_by".into(), json!(n.superseded_by));
        }
        if n.unreviewed {
            m.insert("unreviewed".into(), json!(true));
        }
        if n.rejected {
            m.insert("rejected".into(), json!(true));
        }
        if let Some(by) = n.decided_by {
            m.insert("decided_by".into(), json!(by));
        }
    }
    item
}

/// Does `n` pass the review filter `review` (none passes everything)?
fn reviewed_as(review: Option<&str>, n: &Entry) -> bool {
    match review {
        Some("pending") => n.unreviewed,
        Some("reviewed") => !n.unreviewed,
        Some("by-tty") => n.decided_by == Some("tty"),
        Some("by-agent") => n.decided_by == Some("agent"),
        _ => true,
    }
}

/// The ids of the nodes whose `supersedes` edge points at `id`, sorted.
/// How many arriving edges `get` lists per record (`edges_in_total` says
/// how many there are when there are more).
pub const EDGES_IN_LIMIT: usize = 500;

/// **The edges that arrive at `id` of `pool`**, from every pool of the soup
/// (an edge is stored with its source, so each pool file is asked):
/// `{from, kind, from_pool?, note?, <edge fields>}`, by kind and source, at
/// most [`EDGES_IN_LIMIT`]; and how many there are. An edge names one
/// version of a record: after a supersede, edges into the old version stay
/// on it (records never change), so a reader follows `superseded_by`.
fn edges_in(c: &env::Cauldron, pool: &PoolName, id: &NodeId, writers: &crate::provenance::Writers) -> Result<(Vec<Value>, usize)> {
    let dst = QualifiedNodeId { pool: pool.clone(), id: id.clone() };
    let mut out = Vec::new();
    let mut total = 0;
    // The pools every read of a user pool accepts (`grammars::pins`: each
    // file judged against the spine's registry and bound to it, and each at
    // its own `<pool>.db`), never the soup directory's files as found.
    for g in crate::grammars::pins(c)? {
        let src_pool = g.pool.clone();
        let p = read_pool(c, &src_pool)?;
        let snap = p.snapshot().map_err(store)?;
        for x in snap.edges_to(&dst, None).map_err(store)? {
            total += 1;
            if out.len() >= EDGES_IN_LIMIT {
                continue;
            }
            // The edge's fields first, then its position over them: what
            // the entry names as the source is where the edge is stored,
            // whatever a field says (a user grammar may not declare these
            // names, `user_grammar::RESERVED_EDGE_FIELDS`; a built-in one
            // does not).
            let mut m = serde_json::Map::new();
            for (k, v) in x.fields {
                m.insert(k, v);
            }
            // The review marks are the source's, set below from its record
            // only, never carried over from a field.
            for k in ["from_pool", "rejected", "unreviewed"] {
                m.remove(k);
            }
            m.insert("from".into(), json!(x.src.0));
            m.insert("kind".into(), json!(x.kind));
            if &src_pool != pool {
                m.insert("from_pool".into(), json!(src_pool.as_str()));
            }
            if let Some(n) = &x.note {
                m.insert("note".into(), json!(n));
            }
            // The source's review state, marked as a read of the source
            // marks it, so a reply holding an unreviewed ripple's note
            // carries the warning (`mcp` attaches `unreviewed_note`).
            if let Some(src) = snap.node(&x.src).map_err(store)? {
                let created = snap.created_tx(&src.id).map_err(store)?.map(|h| h.to_hex());
                if let Some(mark) = review_mark(&src, writers, created.as_deref()) {
                    m.insert(mark.into(), json!(true));
                }
            }
            out.push(Value::Object(m));
        }
    }
    Ok((out, total))
}

fn superseded_by(snap: &spine::pool::Snapshot<'_>, pool: &PoolName, id: &NodeId) -> Result<Vec<String>> {
    let mut by: Vec<String> = snap
        .edges_to(&QualifiedNodeId { pool: pool.clone(), id: id.clone() }, Some("supersedes"))
        .map_err(store)?
        .into_iter()
        .map(|x| x.src.0)
        .collect();
    by.sort_by(|a, b| prefix(a).cmp(prefix(b)).then(serial(a).cmp(&serial(b))).then(a.cmp(b)));
    by.dedup();
    Ok(by)
}

/// `list` without its superseded nodes, unless `keep`; and how many were
/// left out.
fn unsuperseded<T>(list: Vec<T>, keep: bool, entry: impl Fn(&T) -> &Entry) -> (Vec<T>, usize) {
    if keep {
        return (list, 0);
    }
    let before = list.len();
    let kept: Vec<T> = list.into_iter().filter(|x| entry(x).superseded_by.is_empty()).collect();
    let hidden = before - kept.len();
    (kept, hidden)
}

/// `list` without its rejected records, unless `keep`; and how many were
/// left out.
fn unrejected<T>(list: Vec<T>, keep: bool, entry: impl Fn(&T) -> &Entry) -> (Vec<T>, usize) {
    if keep {
        return (list, 0);
    }
    let before = list.len();
    let kept: Vec<T> = list.into_iter().filter(|x| !entry(x).rejected).collect();
    let hidden = before - kept.len();
    (kept, hidden)
}

/// Say how many rejected records a listing left out, when it left any.
fn say_rejected(doc: &mut Value, hidden: usize) {
    if hidden > 0 {
        doc["rejected_hidden"] = json!(hidden);
    }
}

/// The prefix of `<PREFIX>-<n>`, the whole id for anything else.
pub fn prefix(id: &str) -> &str {
    id.rsplit_once('-').map_or(id, |(p, _)| p)
}

/// The serial of `<PREFIX>-<n>`, 0 for anything else.
pub fn serial(id: &str) -> u64 {
    id.rsplit_once('-').and_then(|(_, n)| n.parse().ok()).unwrap_or(0)
}

/// The node types to read: `asked` (checked against the pool's grammar), or
/// every type the grammar declares.
fn types_of(pool: &Pool, asked: Option<&str>) -> Result<Vec<String>> {
    let all: Vec<String> = pool.grammar().node_types.keys().cloned().collect();
    match asked {
        None => Ok(all),
        Some(t) if all.iter().any(|x| x == t) => Ok(vec![t.to_string()]),
        Some(t) => refuse!(Code::Usage, "pool `{}` has no node type `{t}` (it has: {})", pool.name(), all.join(", ")),
    }
}

/// A region filter, checked: it must name a region the pool holds.
fn region_filter(snap: &spine::pool::Snapshot<'_>, asked: Option<&str>) -> Result<Option<String>> {
    let Some(r) = asked else { return Ok(None) };
    match snap.node(&NodeId(r.to_string())).map_err(store)? {
        Some(n) if n.node_type == "region" => Ok(Some(r.to_string())),
        Some(n) => refuse!(Code::Usage, "`{r}` is a {}, not a region", n.node_type),
        None => refuse!(Code::NotFound, "there is no region `{r}`; `cauldron soup regions` lists them"),
    }
}

/// A list cut to `limit`, with how many were left out.
fn bounded<T>(mut v: Vec<T>, limit: usize) -> (Vec<T>, usize) {
    let more = v.len().saturating_sub(limit);
    v.truncate(limit);
    (v, more)
}

// -------------------------------------------------------------------- nodes

/// Nodes as `{id, type, state, handle, regions}`, newest first, filtered by
/// region, type and state, at most `limit` (default [`NODES_LIMIT`]).
/// A read aimed at the soup's own pool whose `type` is a user grammar's type
/// reads that type's pool instead.
fn user_pool_for_type(t: &Target, o: &ReadOpts) -> Result<Option<PoolName>> {
    if o.pool.is_some() {
        return Ok(None);
    }
    match o.node_type.as_deref() {
        Some(ty) => {
            crate::grammars::refuse_missing_type(&t.cauldron, ty)?;
            crate::grammars::pool_of_type(&t.cauldron, ty)
        }
        None => Ok(None),
    }
}

/// A region filter on a user grammar's pool is refused: user kinds have no
/// region membership in this release (`in_region` and `claims` are never a
/// user edge), so no user record is in a region to be filtered by.
fn refuse_region_on_user_pool(t: &Target, pool: &PoolName, o: &ReadOpts) -> Result<()> {
    let Some(r) = o.region.as_deref() else { return Ok(()) };
    if let Some(g) = crate::grammars::user_pins(&t.cauldron)?.into_iter().find(|g| &g.pool == pool) {
        let what = o.node_type.as_deref().map(|ty| format!("a `{ty}`")).unwrap_or_else(|| "a record".into());
        return Err(Error::new(
            Code::Usage,
            format!(
                "{what} of user pool `{pool}` (`{}@{}`) is in no region: user kinds have no region membership in this release \
                 (`in_region` is never a user grammar's edge), so `region` cannot filter them. An edge from a user record to \
                 a region (such as `about`) is a plain link: `soup get <id>` shows it. Read without `region`",
                g.kind, g.version
            ),
        )
        .with_detail(json!({"reason": "no-region-membership", "pool": pool.as_str(), "region": r})));
    }
    Ok(())
}

pub fn nodes(t: &Target, o: &ReadOpts) -> Result<Value> {
    if let Some(r) = &o.review {
        checked_review(r)?;
    }
    let (e, c) = (&t.env, &t.cauldron);
    let pool = user_pool_for_type(t, o)?.unwrap_or_else(|| o.pool_name());
    refuse_region_on_user_pool(t, &pool, o)?;
    // `--type cluster` lists the clusters of the soup's cluster pool.
    if pool.as_str() == DEFAULT_POOL && o.node_type.as_deref() == Some(spine::grammar_cauldron::CLUSTER) {
        return crate::clusters::nodes(t, o.region.as_deref(), o.state.as_deref(), o.limit.unwrap_or(NODES_LIMIT), o.max_chars);
    }
    let handle = read_pool(c, &pool)?;
    let snap = handle.snapshot().map_err(store)?;
    let types = types_of(&handle, o.node_type.as_deref())?;
    let region = region_filter(&snap, o.region.as_deref())?;
    let all: Vec<Entry> = catalog(c, &pool, &snap, &types)?
        .into_iter()
        .filter(|n| o.state.as_deref().is_none_or(|s| n.state.as_deref() == Some(s)))
        .filter(|n| region.as_ref().is_none_or(|r| n.regions.contains(r)))
        .filter(|n| reviewed_as(o.review.as_deref(), n))
        .collect();
    let (all, hidden) = unsuperseded(all, o.include_superseded, |n| n);
    let (all, rejected_hidden) = unrejected(all, o.include_rejected, |n| n);
    let total = all.len();
    let (shown, more) = bounded(all, o.limit.unwrap_or(NODES_LIMIT));
    let mut cut = Cut::new(o.max_chars);
    let list: Vec<Value> = shown
        .iter()
        .map(|n| {
            stated(
                json!({
                    "id": n.id,
                    "type": n.node_type,
                    "handle": cut.value(&n.id, "handle", &json!(n.handle)),
                    "regions": n.regions,
                }),
                n,
            )
        })
        .collect();
    let mut doc = json!({"env": e.name, "cauldron": c.name, "pool": pool.as_str(), "total": total, "superseded_hidden": hidden, "nodes": list});
    say_rejected(&mut doc, rejected_hidden);
    if more > 0 {
        doc["more"] = json!(more);
    }
    crate::merge::note_pending(&t.cauldron, &mut doc);
    cut.report(&mut doc);
    Ok(doc)
}

// ------------------------------------------------------------------- search

/// Lower-case one char to one char, so char positions in the folded text
/// are positions in the original.
fn fold(s: &str) -> Vec<char> {
    s.chars().map(|ch| ch.to_lowercase().next().unwrap_or(ch)).collect()
}

/// Where `term` first occurs in `hay`, as a char index.
fn find(hay: &[char], term: &[char]) -> Option<usize> {
    if term.is_empty() || term.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - term.len()).find(|&i| hay[i..i + term.len()] == *term)
}

/// The query's terms: its words (runs of letters and digits), folded,
/// without repeats, in order.
fn terms(query: &str) -> Vec<Vec<char>> {
    let mut out: Vec<Vec<char>> = Vec::new();
    for w in query.split(|ch: char| !ch.is_alphanumeric()).filter(|w| !w.is_empty()) {
        let t = fold(w);
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// The terms a search for `query` looks for; none means the query has no
/// word to search for.
pub fn search_terms(query: &str) -> Vec<String> {
    terms(query).iter().map(|t| t.iter().collect()).collect()
}

/// `max` chars of `text` around char `at`, with `…` where it was cut.
fn snippet(text: &str, at: usize, max: usize) -> (String, bool) {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return (text.to_string(), false);
    }
    let start = at.saturating_sub(max / 4).min(chars.len() - max);
    let mut s: String = chars[start..start + max].iter().collect();
    if start > 0 {
        s.insert(0, '…');
    }
    if start + max < chars.len() {
        s.push('…');
    }
    (s, true)
}

/// Lexical search over handles and bodies (and, in the soup's own pool, over
/// cluster handles and boundaries): every word of the query a term,
/// found case-insensitively as a substring; ranked by how many terms match,
/// then handle hits over body hits, then newest. At most `limit` (default
/// [`SEARCH_LIMIT`]); the snippet is `max_chars` long (default
/// [`SNIPPET_CHARS`]) around the first hit.
pub fn search(t: &Target, query: &str, o: &ReadOpts) -> Result<Value> {
    let wanted = terms(query);
    if wanted.is_empty() {
        refuse!(Code::Usage, "a search needs a query with at least one word");
    }
    if let Some(r) = &o.review {
        checked_review(r)?;
    }
    let (e, c) = (&t.env, &t.cauldron);
    let pool = user_pool_for_type(t, o)?.unwrap_or_else(|| o.pool_name());
    refuse_region_on_user_pool(t, &pool, o)?;
    let handle = read_pool(c, &pool)?;
    let snap = handle.snapshot().map_err(store)?;
    // Clusters are searched beside the workspace's nodes (or alone, with
    // `type: cluster`): their handles and boundaries.
    let with_clusters = pool.as_str() == DEFAULT_POOL && o.node_type.as_deref().is_none_or(|t| t == spine::grammar_cauldron::CLUSTER);
    let only_clusters = with_clusters && o.node_type.is_some();
    let types = if only_clusters { vec![] } else { types_of(&handle, o.node_type.as_deref())? };
    let region = region_filter(&snap, o.region.as_deref())?;
    let phrase: Vec<char> = wanted.join(&' ');
    // (matched terms, score, catalog position, entry, first body hit)
    let mut hits = Vec::new();
    for (pos, n) in catalog(c, &pool, &snap, &types)?.into_iter().enumerate() {
        if region.as_ref().is_some_and(|r| !n.regions.contains(r)) || !reviewed_as(o.review.as_deref(), &n) {
            continue;
        }
        let (h, b) = (fold(&n.handle), fold(&n.body));
        let (mut matched, mut score, mut first) = (0usize, 0usize, None::<usize>);
        for t in &wanted {
            let in_h = find(&h, t).is_some();
            let in_b = find(&b, t);
            if in_h || in_b.is_some() {
                matched += 1;
            }
            score += 3 * usize::from(in_h) + usize::from(in_b.is_some());
            if let Some(at) = in_b {
                first = Some(first.map_or(at, |f: usize| f.min(at)));
            }
        }
        if matched == 0 {
            continue;
        }
        if wanted.len() > 1 && (find(&h, &phrase).is_some() || find(&b, &phrase).is_some()) {
            score += 2;
        }
        hits.push((matched, 10 * matched + score, pos, n, first));
    }
    // Most terms matched, then the score, then newest (catalog order).
    hits.sort_by(|x, y| y.0.cmp(&x.0).then(y.1.cmp(&x.1)).then(x.2.cmp(&y.2)));
    let (hits, hidden) = unsuperseded(hits, o.include_superseded, |h| &h.3);
    let (hits, rejected_hidden) = unrejected(hits, o.include_rejected, |h| &h.3);
    // One ranked list of both: a workspace node or a cluster.
    enum Found {
        Node(Box<Entry>, Option<usize>),
        Cluster(crate::clusters::Listed),
    }
    let mut ranked: Vec<(usize, usize, usize, usize, Found)> =
        hits.into_iter().map(|(m, sc, pos, n, first)| (m, sc, 0, pos, Found::Node(Box::new(n), first))).collect();
    if with_clusters {
        for (m, sc, pos, k) in crate::clusters::search(c, &wanted, &phrase, region.as_deref())? {
            ranked.push((m, sc, 1, pos, Found::Cluster(k)));
        }
        ranked.sort_by(|x, y| y.0.cmp(&x.0).then(y.1.cmp(&x.1)).then(x.2.cmp(&y.2)).then(x.3.cmp(&y.3)));
    }
    let total = ranked.len();
    let (shown, more) = bounded(ranked, o.limit.unwrap_or(SEARCH_LIMIT));
    let max = o.max_chars.unwrap_or(SNIPPET_CHARS);
    let mut cut = Cut::new(o.max_chars);
    let fold_find = |text: &str| -> usize {
        let h = fold(text);
        wanted.iter().filter_map(|t| find(&h, t)).min().unwrap_or(0)
    };
    let list: Vec<Value> = shown
        .into_iter()
        .map(|(_, score, _, _, found)| match found {
            Found::Node(n, first) => {
                let (snip, partial) = snippet(&n.body, first.unwrap_or(0), max);
                if partial {
                    cut.note(json!({"id": n.id, "field": "body", "chars": n.body.chars().count(), "as": "snippet"}));
                }
                stated(
                    json!({
                        "id": n.id,
                        "type": n.node_type,
                        "handle": cut.value(&n.id, "handle", &json!(n.handle)),
                        "regions": n.regions,
                        "score": score,
                        "snippet": snip,
                    }),
                    &n,
                )
            }
            Found::Cluster(k) => {
                // A cluster's snippet is cut from its boundary.
                let (snip, partial) = snippet(&k.boundary, fold_find(&k.boundary), max);
                if partial {
                    cut.note(json!({"id": k.id, "field": "boundary", "chars": k.boundary.chars().count(), "as": "snippet"}));
                }
                json!({
                    "id": k.id,
                    "type": spine::grammar_cauldron::CLUSTER,
                    "handle": cut.value(&k.id, "handle", &json!(k.handle)),
                    "regions": k.regions,
                    "score": score,
                    "snippet": snip,
                })
            }
        })
        .collect();
    let mut doc = json!({
        "env": e.name, "cauldron": c.name, "pool": pool.as_str(), "query": query,
        "terms": wanted.iter().map(|t| t.iter().collect::<String>()).collect::<Vec<_>>(),
        "total": total, "superseded_hidden": hidden, "results": list,
    });
    say_cluster_pool(&mut doc, "results");
    say_rejected(&mut doc, rejected_hidden);
    if more > 0 {
        doc["more"] = json!(more);
    }
    crate::merge::note_pending(&t.cauldron, &mut doc);
    cut.report(&mut doc);
    Ok(doc)
}

// ------------------------------------------------------------------- region

/// One region: its record, the regions it narrows (parents) and that narrow
/// it (children) to `depth` levels (default 1), the regions it relates to
/// either way, and its direct members (id, type, state, handle), newest
/// first, at most `limit` (default [`MEMBERS_LIMIT`]).
pub fn region(t: &Target, id: &str, o: &ReadOpts) -> Result<Value> {
    let (e, c) = (&t.env, &t.cauldron);
    let pool = o.pool_name();
    let handle = read_pool(c, &pool)?;
    let snap = handle.snapshot().map_err(store)?;
    let id = region_filter(&snap, Some(id))?.expect("asked");
    let err = |e: spine::error::SpineError| store(e);
    let mut cut = Cut::new(o.max_chars);
    let depth = o.depth.unwrap_or(1);
    let qualified = |id: &str| QualifiedNodeId { pool: pool.clone(), id: NodeId(id.to_string()) };
    let members_of = |id: &str| -> Result<usize> { Ok(snap.edges_to(&qualified(id), Some("in_region")).map_err(err)?.len()) };

    // A neighbouring region, short: id, handle, member count.
    let brief = |cut: &mut Cut, rid: &str, extra: Option<usize>| -> Result<Value> {
        let h = snap
            .node(&NodeId(rid.to_string()))
            .map_err(err)?
            .and_then(|n| n.fields.get("handle").map(|f| f.value.clone()))
            .unwrap_or(Value::Null);
        let mut v = json!({"id": rid, "handle": cut.value(rid, "handle", &h), "members": members_of(rid)?});
        if let Some(d) = extra {
            v["depth"] = json!(d);
        }
        Ok(v)
    };

    // Up the narrows graph (parents) and down it (children), breadth first,
    // each region once, to `depth` levels.
    let walk = |cut: &mut Cut, up: bool| -> Result<Vec<Value>> {
        let mut seen = std::collections::BTreeSet::from([id.clone()]);
        let mut frontier = vec![id.clone()];
        let mut out = Vec::new();
        for level in 1..=depth {
            let mut next = Vec::new();
            for r in &frontier {
                let mut far: Vec<String> = if up {
                    snap.edges_from(&NodeId(r.clone()), Some("narrows")).map_err(err)?.iter().map(|x| target_text(&pool, &x.dst)).collect()
                } else {
                    snap.edges_to(&qualified(r), Some("narrows")).map_err(err)?.iter().map(|x| x.src.0.clone()).collect()
                };
                far.sort();
                for f in far {
                    if seen.insert(f.clone()) {
                        out.push(brief(cut, &f, Some(level))?);
                        next.push(f);
                    }
                }
            }
            frontier = next;
        }
        Ok(out)
    };
    let parents = walk(&mut cut, true)?;
    let children = walk(&mut cut, false)?;

    let mut relates: Vec<String> = snap
        .edges_from(&NodeId(id.clone()), Some("relates"))
        .map_err(err)?
        .iter()
        .map(|x| target_text(&pool, &x.dst))
        .chain(snap.edges_to(&qualified(&id), Some("relates")).map_err(err)?.iter().map(|x| x.src.0.clone()))
        .collect();
    relates.sort();
    relates.dedup();
    let relates = relates.iter().map(|r| brief(&mut cut, r, None)).collect::<Result<Vec<_>>>()?;

    let r = snap.node(&NodeId(id.clone())).map_err(err)?.expect("checked above");
    let mut flags = serde_json::Map::new();
    for k in REGION_FLAGS {
        if let Some(f) = r.fields.get(k) {
            flags.insert(k.to_string(), f.value.clone());
        }
    }
    let field = |cut: &mut Cut, k: &str| r.fields.get(k).map(|f| cut.value(&id, k, &f.value));
    let record = json!({
        "id": id,
        "handle": field(&mut cut, "handle"),
        "boundary": field(&mut cut, "boundary"),
        "flags": flags,
    });

    let types = types_of(&handle, None)?;
    let members: Vec<Entry> = catalog(c, &pool, &snap, &types)?.into_iter().filter(|n| n.regions.contains(&id)).collect();
    let (members, hidden) = unsuperseded(members, o.include_superseded, |n| n);
    let (members, rejected_hidden) = unrejected(members, o.include_rejected, |n| n);
    let total = members.len();
    let (shown, more) = bounded(members, o.limit.unwrap_or(MEMBERS_LIMIT));
    let members: Vec<Value> = shown
        .iter()
        .map(|n| stated(json!({"id": n.id, "type": n.node_type, "handle": cut.value(&n.id, "handle", &json!(n.handle))}), n))
        .collect();
    // The clusters that claim it (the cluster pool's reverse edges).
    let clusters = if pool.as_str() == DEFAULT_POOL { crate::clusters::claimants(c, &id, &mut cut)? } else { vec![] };
    let mut doc = json!({
        "env": e.name, "cauldron": c.name, "pool": pool.as_str(),
        "region": record, "parents": parents, "children": children, "relates": relates,
        "members_total": total, "superseded_hidden": hidden, "members": members,
        "clusters": clusters,
    });
    say_rejected(&mut doc, rejected_hidden);
    if more > 0 {
        doc["more"] = json!(more);
    }
    crate::merge::note_pending(&t.cauldron, &mut doc);
    cut.report(&mut doc);
    Ok(doc)
}
