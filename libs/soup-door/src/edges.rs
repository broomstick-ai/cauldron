//! **Edges and citations of records the soup already holds**: adding edges
//! (`soup_link`, `cauldron soup link`), removing them (`soup_unlink`,
//! `cauldron soup unlink`), and adding or removing one citation of a
//! record's `meta.cites` (`soup_cite`, `cauldron soup cite`).
//!
//! Each act is judged first and then written as ONE statement through the
//! handle it is given: the `mcp` service's writer handle, or one this module
//! opens for a direct write from the command line ([`direct`]). The handle
//! carries the region guard, so a write into a region a ripple holds is
//! refused `region_locked`, for a removal as for an addition.
//!
//! **What a removal may not take away.** Records are written whole, and the
//! grammar requires some edges of a record at mint; a removal that would
//! leave a record without them would leave it as no import could write it.
//! So these are refused, each by name, and nothing of the call is written:
//!
//! - `required-edge`: the edge is one of the record's last required edges:
//!   a content record keeps at least one `in_region` (or, when it has none,
//!   its `rejected_by` edges and `searched` field: at least
//!   [`spine::grammar_v6::REJECTED_BY_MIN`]), and a milestone keeps a
//!   `serves`;
//! - `supersedes-is-history`: a `supersedes` edge says one record replaced
//!   another; removing it would make the old record read as current again.
//!   Write a new record instead. The one way out of damage no write makes
//!   any more: a `supersedes` edge from a record to itself, or one on a
//!   cycle of `supersedes` edges, may be removed (it records no
//!   replacement, and it hides every record on it);
//! - `claims-is-soup-claim`: a cluster's `claims` edge is released with
//!   `soup_claim` (`release: true`), never here;
//! - `unknown-node`, `unknown-kind`, as for a link.
//!
//! **What an addition may not make.** Each edge kind of the grammar was
//! checked for a meaning that needs the moment a record is made:
//!
//! - `supersedes-at-creation`: `supersedes` says the record it leaves was
//!   written to replace the one it reaches, so it is written only WITH that
//!   new record (a put or import batch, where the grammar already holds it
//!   to the same type); `soup_link` never adds one;
//! - `edge-loop`: a `supersedes` or `narrows` edge from a record to itself,
//!   or one that would close a cycle of its kind (`narrows` is acyclic;
//!   `supersedes` is history, and a cycle of it would hide every record on
//!   it). Refused here and, through [`refuse_edge_loops`], in every put and
//!   import batch;
//! - every other kind (`in_region`, `rejected_by`, `depends_on`, `blocks`,
//!   `derived_from`, `resolved_by`, `serves`, `relates`, `ref`, `shows`) is a
//!   relation that holds or not whenever it is written, and is added here.
//!
//! A removal of an edge that is not there changes nothing and says so
//! (`not-linked`). A removal lands in the change feed as `unlink`. In the
//! same way, adding an edge the soup already holds (same from, kind and to)
//! writes nothing and says so (`already-linked`; its note stays as it was),
//! as `soup_claim` answers `already-claimed`.
//!
//! **Citations.** `meta.cites` is changed one entry at a time, by a `SET` of
//! that one key: no other `meta` key (the owner's review flags, a ripple's
//! provenance, `imported_from`) can be named, set or cleared here. An added
//! citation is judged by the same table as an import's
//! ([`spine::grammar_v6::cite_problem`]). On a record a ripple wrote, a
//! `dump` citation is part of the review trail (the review queue groups the
//! record by it, and approving or rejecting `--dump` selects by it), so one
//! is neither added nor removed there: `dump-cite-is-provenance`. Other
//! citations of such a record change as on any record.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance, ENVELOPE_FORMAT_VERSION};
use spine::pool::{NodeId, Pool, QualifiedNodeId};
use spine::schema_def::PoolName;

use crate::clusters::Writer;
use crate::reads::DEFAULT_POOL;
use crate::target::Target;
use crate::{refuse, Code, Error, Result};

/// The citation kind that names the dump a record came from.
const DUMP_CITE: &str = "dump";

/// The refusal reason for a change to a ripple-written record's `dump`
/// citation.
pub const DUMP_CITE_IS_PROVENANCE: &str = "dump-cite-is-provenance";

/// One edge to add or remove.
#[derive(Debug, Clone)]
pub struct Link {
    pub from: String,
    pub kind: String,
    pub to: String,
    /// The edge's note (adding only).
    pub note: Option<String>,
}

fn store(err: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, err.to_string())
}

fn ws() -> PoolName {
    PoolName::new(DEFAULT_POOL)
}

fn envelope(statement: String, bindings: Bindings, w: &Writer, nonce: &str) -> Envelope {
    Envelope {
        statement,
        bindings,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent { thread: w.thread.clone(), dispatch: w.dispatch.clone() },
            instrument: Instrument { name: "cauldron-soup-door".into(), version: env!("CARGO_PKG_VERSION").into() },
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(format!(
            "{nonce}:{}:{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        )),
    }
}

/// One statement over `links`: a MATCH (pinned to exactly one node) per
/// distinct endpoint, then `verb` per link. Values ride as parameters; the
/// type and kind names spliced in are the grammar's own.
fn statement(links: &[&Link], types: &BTreeMap<String, At>, verb: &str) -> (String, Bindings) {
    let mut binding: BTreeMap<&str, String> = BTreeMap::new();
    let (mut lines, mut expects, mut acts) = (Vec::new(), Vec::new(), Vec::new());
    let mut params = Bindings::new();
    for l in links {
        for end in [&l.from, &l.to] {
            if !binding.contains_key(end.as_str()) {
                let b = format!("n{}", binding.len());
                let at = &types[end];
                lines.push(format!("MATCH ({b}: {}/{} {{ id: $id_{b} }})", at.pool.as_str(), at.ty));
                expects.push(format!("EXPECT COUNT({b}) = 1"));
                params.insert(format!("id_{b}"), json!(end));
                binding.insert(end, b);
            }
        }
    }
    for (i, l) in links.iter().enumerate() {
        let mut line = format!("{verb} ({}) -[{}]-> ({})", binding[l.from.as_str()], l.kind, binding[l.to.as_str()]);
        if let Some(n) = l.note.as_ref().filter(|_| verb == "CREATE") {
            line.push_str(&format!(" NOTE $note_{i}"));
            params.insert(format!("note_{i}"), json!(n));
        }
        acts.push(line);
    }
    (lines.into_iter().chain(expects).chain(acts).collect::<Vec<_>>().join("\n"), params)
}

/// Where a record the soup holds is: its pool and its type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct At {
    pub pool: PoolName,
    pub ty: String,
}

impl At {
    fn is_user(&self) -> bool {
        self.pool.as_str() != DEFAULT_POOL && self.pool.as_str() != crate::clusters::POOL
    }

    fn qualified(&self, id: &str) -> QualifiedNodeId {
        QualifiedNodeId { pool: self.pool.clone(), id: NodeId(id.to_string()) }
    }
}

/// The user grammars' pools `handle` holds, by name.
fn user_pools(handle: &spine::Cauldron) -> Vec<PoolName> {
    handle.pools().filter(|p| handle.grammar(p).is_some_and(|g| spine::user_grammar::is_user_kind(g.kind.as_str()))).cloned().collect()
}

fn read_pool(c: &env::Cauldron, pool: &PoolName) -> Result<Pool> {
    Pool::open_read_only(&Pool::path_in(&c.soup(), pool)).map_err(|err| store(format!("reading pool `{pool}` of cauldron `{}`'s soup: {err}", c.name)))
}

/// **Where each of `ids` is**: in `ws`, else in one of the user grammars'
/// pools (`spine::user_grammar`; a record id names one pool, since an id
/// prefix is used once per cauldron). An id in neither is absent.
pub fn locate(handle: &spine::Cauldron, c: &env::Cauldron, ids: &[&String]) -> Result<BTreeMap<String, At>> {
    let mut found = BTreeMap::new();
    let mut pools = vec![ws()];
    pools.extend(user_pools(handle));
    for pool in pools {
        let left: Vec<&&String> = ids.iter().filter(|id| !found.contains_key(id.as_str())).collect();
        if left.is_empty() {
            break;
        }
        let p = read_pool(c, &pool)?;
        let snap = p.snapshot().map_err(store)?;
        for id in left {
            if let Some(n) = snap.node(&NodeId((*id).clone())).map_err(store)? {
                found.insert((*id).clone(), At { pool: pool.clone(), ty: n.node_type });
            }
        }
    }
    Ok(found)
}

/// `<ID>@<pool>`, as reads print an edge's target: the id, and the pool it
/// names. A plain id names no pool.
pub fn split_end(end: &str) -> (String, Option<String>) {
    match end.rsplit_once('@') {
        Some((id, pool)) if !id.is_empty() && PoolName::new(pool).is_well_formed() => (id.to_string(), Some(pool.to_string())),
        _ => (end.to_string(), None),
    }
}

/// `links` with every `<ID>@<pool>` end made a plain id, and the pool each
/// such end named (by link index and end: 0 from, 1 to).
fn plain(links: &[Link]) -> (Vec<Link>, BTreeMap<(usize, u8), String>) {
    let mut named = BTreeMap::new();
    let out = links
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let (from, fp) = split_end(&l.from);
            let (to, tp) = split_end(&l.to);
            if let Some(p) = fp {
                named.insert((i, 0), p);
            }
            if let Some(p) = tp {
                named.insert((i, 1), p);
            }
            Link { from, kind: l.kind.clone(), to, note: l.note.clone() }
        })
        .collect();
    (out, named)
}

/// The record ids of `links` that the soup does not hold, as refusals, and
/// where those it does are. An end written `<ID>@<pool>` must be in that
/// pool.
fn endpoints(handle: &spine::Cauldron, links: &[Link], c: &env::Cauldron) -> Result<(BTreeMap<String, At>, BTreeMap<usize, Value>, Vec<Link>)> {
    let (links_plain, named) = plain(links);
    let links = &links_plain[..];
    let ids: Vec<&String> = links.iter().flat_map(|l| [&l.from, &l.to]).collect();
    let types = locate(handle, c, &ids)?;
    let mut why = BTreeMap::new();
    for ((i, end), want) in &named {
        let l = &links[*i];
        let id = if *end == 0 { &l.from } else { &l.to };
        if let Some(at) = types.get(id) {
            if at.pool.as_str() != want {
                why.insert(*i, json!({"reason": "wrong-pool", "detail": format!("`{id}@{want}` names pool `{want}`, but `{id}` is a record of pool `{}`", at.pool)}));
            }
        }
    }
    for (i, l) in links.iter().enumerate() {
        for end in [&l.from, &l.to] {
            if !why.contains_key(&i) && !types.contains_key(end) {
                // An id of a registered user pool whose file is out: say so.
                let out = handle.missing_user_pools().unwrap_or_default().into_iter().find(|g| {
                    let named = named.iter().any(|((j, _), p)| *j == i && p == g.pool.as_str());
                    named || g.node_types.values().any(|d| d.id_prefix.as_ref().is_some_and(|p| end.starts_with(&format!("{p}-"))))
                });
                match out {
                    Some(g) => why.insert(i, json!({"reason": "pool-file-missing", "pool": g.pool.as_str(), "detail": crate::grammars::missing_note(c, &g)})),
                    None => why.insert(i, json!({"reason": "unknown-node", "detail": format!("`{end}` is not a record of cauldron `{}` (in `ws` or a user grammar's pool)", c.name)})),
                };
            }
        }
        if why.contains_key(&i) {
            continue;
        }
        let (from, to) = (&types[&l.from], &types[&l.to]);
        // An edge leaves its source's own grammar. A built-in grammar
        // declares no edge into a user kind (it was written before any
        // user grammar), so no edge from a workspace record reaches one.
        if !from.is_user() && to.is_user() {
            why.insert(i, json!({"reason": "no-edge-into-a-user-kind",
                "detail": format!("`{}` is a `{}` of pool `{}` and `{}` a `{}` of user pool `{}`: an edge is declared by its source's grammar, and a built-in grammar declares no edge into a user kind, so no `{}` edge leaves `{}` for `{}`. An edge the other way is written when `{}`'s grammar declares it (`cauldron soup grammar show {}`)",
                    l.from, from.ty, from.pool, l.to, to.ty, to.pool, l.kind, l.from, l.to, to.pool, to.pool)}));
        }
    }
    Ok((types, why, links_plain))
}

fn unknown_kind(grammar: &spine::schema_def::PoolGrammar, kind: &str) -> Option<Value> {
    (!grammar.edge_kinds.contains_key(kind)).then(|| {
        let kinds: Vec<&String> = grammar.edge_kinds.keys().collect();
        json!({"reason": "unknown-kind", "detail": format!("`{kind}` is not an edge kind of pool `{}`'s grammar (`{}@{}`)", grammar.pool, grammar.kind, grammar.version), "kinds": kinds})
    })
}

/// The grammar of the pool a link's source is in.
fn source_grammar<'h>(handle: &'h spine::Cauldron, at: &At) -> Result<&'h spine::schema_def::PoolGrammar> {
    handle.grammar(&at.pool).ok_or_else(|| store(format!("the soup has no pool `{}`", at.pool)))
}

/// The edge kinds whose graph may hold no cycle and no self-edge.
pub const ACYCLIC_KINDS: [&str; 2] = ["supersedes", "narrows"];

/// The edge kind added only with the record it leaves.
pub const AT_CREATION_KINDS: [&str; 1] = ["supersedes"];

/// Does `from` reach `to` by `kind` edges of the soup, plus `extra` edges
/// (from, to) of the same kind? (Breadth first; every node once.)
fn reaches(snap: &spine::pool::Snapshot<'_>, pool: &PoolName, kind: &str, from: &str, to: &str, extra: &[(String, String)]) -> Result<bool> {
    let mut seen = BTreeSet::new();
    let mut queue = vec![from.to_string()];
    while let Some(n) = queue.pop() {
        if n == to {
            return Ok(true);
        }
        if !seen.insert(n.clone()) {
            continue;
        }
        for e in snap.edges_from(&NodeId(n.clone()), Some(kind)).map_err(store)? {
            if &e.dst.pool == pool {
                queue.push(e.dst.id.0.clone());
            }
        }
        queue.extend(extra.iter().filter(|(f, _)| *f == n).map(|(_, t)| t.clone()));
    }
    Ok(false)
}

fn loop_refusal(l: &Link) -> Value {
    let what = if l.kind == "narrows" { "`narrows` is acyclic" } else { "`supersedes` is history, and a loop of it would hide every record on it" };
    if l.from == l.to {
        json!({"reason": "edge-loop", "detail": format!("`{}` -[{}]-> `{}` is an edge from a record to itself: {what}", l.from, l.kind, l.to)})
    } else {
        json!({"reason": "edge-loop", "detail": format!("`{}` -[{}]-> `{}` would close a cycle (`{}` already reaches `{}` by `{}` edges): {what}", l.from, l.kind, l.to, l.to, l.from, l.kind)})
    }
}

/// **No batch writes a self-edge or a cycle** of an [`ACYCLIC_KINDS`] kind.
/// A batch only creates records, and an edge only leaves a record of its
/// batch, so a cycle can only run through the batch's own records (no
/// record the soup holds gains an edge into a new one): the check is on
/// the batch alone, before anything is judged or written, as a long note
/// is. Records are named by `id` (an id, or a label under remint).
pub fn refuse_edge_loops(records: &[Value]) -> Result<()> {
    for kind in ACYCLIC_KINDS {
        let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (i, r) in records.iter().enumerate() {
            let id = r.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("#{i}"));
            for e in r.get("edges").and_then(Value::as_array).into_iter().flatten() {
                if e.get("kind").and_then(Value::as_str) != Some(kind) {
                    continue;
                }
                let Some(to) = e.get("to").and_then(Value::as_str) else { continue };
                if to == id {
                    return Err(Error::new(
                        Code::Usage,
                        format!(
                            "record `{id}` carries a `{kind}` edge to itself; {}. Nothing was written",
                            if kind == "narrows" { "`narrows` is acyclic" } else { "a record never supersedes itself (a new record supersedes an older one)" }
                        ),
                    )
                    .with_detail(json!({"reason": "edge-loop", "record": id, "kind": kind})));
                }
                graph.entry(id.clone()).or_default().push(to.to_string());
            }
        }
        // A cycle among the batch's records: depth first, three colours.
        fn visit(n: &str, g: &BTreeMap<String, Vec<String>>, state: &mut BTreeMap<String, u8>, path: &mut Vec<String>) -> Option<Vec<String>> {
            match state.get(n).copied() {
                Some(1) => {
                    let at = path.iter().position(|p| p == n).unwrap_or(0);
                    let mut cyc = path[at..].to_vec();
                    cyc.push(n.to_string());
                    return Some(cyc);
                }
                Some(2) => return None,
                _ => {}
            }
            state.insert(n.to_string(), 1);
            path.push(n.to_string());
            for t in g.get(n).into_iter().flatten() {
                if let Some(c) = visit(t, g, state, path) {
                    return Some(c);
                }
            }
            path.pop();
            state.insert(n.to_string(), 2);
            None
        }
        let mut state = BTreeMap::new();
        for n in graph.keys() {
            if let Some(cyc) = visit(n, &graph, &mut state, &mut Vec::new()) {
                return Err(Error::new(
                    Code::Usage,
                    format!(
                        "the batch's `{kind}` edges make a cycle ({}); {}. Nothing was written",
                        cyc.join(" -> "),
                        if kind == "narrows" { "`narrows` is acyclic" } else { "`supersedes` is history, and a cycle of it would hide every record on it" }
                    ),
                )
                .with_detail(json!({"reason": "edge-loop", "kind": kind, "cycle": cyc})));
            }
        }
    }
    Ok(())
}

fn refused_links(answers: Vec<Value>, mut doc: Value, total: usize) -> Error {
    let n = answers.iter().filter(|a| a["status"] == "refused").count();
    doc["links"] = json!(answers);
    doc["outcome"] = json!("refused");
    Error::new(Code::LinksRefused, format!("{n} of {total} link(s) were refused, so none was written (all or nothing); `detail.links` says why."))
        .with_detail(doc)
}

// ------------------------------------------------------------------- link

/// **Add edges** between records the soup holds, in ONE write: each link is
/// judged by the grammar first (a dry run of it alone), and if any is
/// refused nothing lands (`links_refused`, each link's reason in
/// `detail.links`). Answers `{links: [{from, kind, to, status: linked}],
/// outcome: linked, landed}`.
pub fn link(handle: &spine::Cauldron, t: &Target, links: &[Link], w: &Writer) -> Result<Value> {
    let c = &t.cauldron;
    for l in links {
        if let Some(n) = &l.note {
            crate::notes::check(n, &format!("the link `{}` -[{}]-> `{}`", l.from, l.kind, l.to))?;
        }
    }
    let (types, mut why, links_plain) = endpoints(handle, links, c)?;
    let links = &links_plain[..];
    // An edge the soup already holds is answered `already-linked` and not
    // written again (the spine would treat it as a no-op, but the write
    // would still take a change entry pointing at nothing new).
    let mut present = vec![false; links.len()];
    for (i, l) in links.iter().enumerate() {
        if !why.contains_key(&i) {
            let (from, to) = (&types[&l.from], &types[&l.to]);
            let p = read_pool(c, &from.pool)?;
            let snap = p.snapshot().map_err(store)?;
            let dst = to.qualified(&l.to);
            present[i] = snap.edges_from(&NodeId(l.from.clone()), Some(&l.kind)).map_err(store)?.iter().any(|e| e.dst == dst);
        }
    }
    // Added only with the record it leaves; never a self-edge or a cycle
    // (module docs). The cycle check counts the call's other new edges.
    let new_edges: Vec<(usize, String, String, String)> = links
        .iter()
        .enumerate()
        .filter(|(i, _)| !why.contains_key(i) && !present[*i])
        .map(|(i, l)| (i, l.kind.clone(), l.from.clone(), l.to.clone()))
        .collect();
    for (i, l) in links.iter().enumerate() {
        if why.contains_key(&i) {
            continue;
        }
        if AT_CREATION_KINDS.contains(&l.kind.as_str()) {
            why.insert(i, json!({"reason": "supersedes-at-creation",
                "detail": format!("a `supersedes` edge is written only with the record that supersedes: put a NEW record of the same type as `{}` carrying `{{\"kind\": \"supersedes\", \"to\": \"{}\", \"note\": …}}` (soup_put, or an import). soup_link never adds one, so no record can be marked replaced after the fact", l.to, l.to)}));
            continue;
        }
        // A cycle runs within one pool: an edge between pools goes from a
        // later grammar to an earlier one, never back.
        let (from, to) = (&types[&l.from], &types[&l.to]);
        if ACYCLIC_KINDS.contains(&l.kind.as_str()) && !present[i] && from.pool == to.pool {
            let others: Vec<(String, String)> =
                new_edges.iter().filter(|(j, k, _, _)| *j != i && *k == l.kind).map(|(_, _, f, t)| (f.clone(), t.clone())).collect();
            let p = read_pool(c, &from.pool)?;
            let snap = p.snapshot().map_err(store)?;
            if l.from == l.to || reaches(&snap, &from.pool, &l.kind, &l.to, &l.from, &others)? {
                why.insert(i, loop_refusal(l));
            }
        }
    }
    let mut answers = Vec::new();
    for (i, l) in links.iter().enumerate() {
        if !why.contains_key(&i) {
            if let Some(u) = unknown_kind(source_grammar(handle, &types[&l.from])?, &l.kind) {
                why.insert(i, u);
            }
        }
        if !why.contains_key(&i) && !present[i] {
            let (s, b) = statement(&[l], &types, "CREATE");
            let mut env = envelope(s, b, w, "soup-link");
            env.mode = Mode::Validate;
            match handle.submit(env) {
                Ok(_) => {}
                Err(spine::error::SpineError::Refused { witnesses }) => {
                    why.insert(i, json!({"reason": "door-refused", "witnesses": witnesses}));
                }
                Err(e) => return Err(store(format!("judging a link: {e}"))),
            }
        }
        answers.push(answer(l, &types, why.remove(&i), if present[i] { "already-linked" } else { "accepted" }));
    }
    let doc = json!({"env": t.env.name, "cauldron": c.name, "pool": DEFAULT_POOL});
    let mut doc = with_pools(doc, links, &types);
    if answers.iter().any(|a| a["status"] == "refused") {
        return Err(refused_links(answers, doc, links.len()));
    }
    // Each new edge once, even when the call names it twice.
    let mut seen = BTreeSet::new();
    let take: Vec<&Link> = links
        .iter()
        .enumerate()
        .filter(|(i, l)| !present[*i] && seen.insert((l.from.clone(), l.kind.clone(), l.to.clone())))
        .map(|(_, l)| l)
        .collect();
    if take.is_empty() {
        doc["links"] = json!(answers);
        doc["outcome"] = json!("already-linked");
        doc["written"] = json!(false);
        return Ok(doc);
    }
    let (s, b) = statement(&take, &types, "CREATE");
    match handle.submit(envelope(s, b, w, "soup-link")) {
        Ok(spine::Outcome::Committed { tx, seq, deduplicated, .. }) => {
            for a in &mut answers {
                if a["status"] == "accepted" {
                    a["status"] = json!("linked");
                }
            }
            doc["links"] = json!(answers);
            doc["outcome"] = json!("linked");
            doc["written"] = json!(true);
            doc["landed"] = json!({"tx": tx.to_hex(), "seq": seq, "deduplicated": deduplicated});
            Ok(doc)
        }
        Ok(other) => Err(store(format!("the link statement was not committed: {other:?}"))),
        Err(spine::error::SpineError::Refused { witnesses }) => {
            doc["links"] = json!(answers);
            doc["outcome"] = json!("refused");
            doc["witnesses"] = json!(witnesses);
            Err(Error::new(Code::LinksRefused, "every link passed alone and the whole set was refused; nothing was written").with_detail(doc))
        }
        Err(e @ spine::error::SpineError::RegionLocked { .. }) => Err(crate::region_lock::spine_error(e)),
        Err(e) => Err(store(format!("writing the links: {e}"))),
    }
}

/// One link's answer: `{from, kind, to, status, refused?}`, plus where each
/// end is (`from_pool`, `to_pool`) when either is in a user grammar's pool.
fn answer(l: &Link, types: &BTreeMap<String, At>, refused: Option<Value>, ok: &str) -> Value {
    let mut a = json!({"from": l.from, "kind": l.kind, "to": l.to});
    if let (Some(f), Some(t)) = (types.get(&l.from), types.get(&l.to)) {
        if f.is_user() || t.is_user() {
            a["from_pool"] = json!(f.pool.as_str());
            a["to_pool"] = json!(t.pool.as_str());
        }
    }
    match refused {
        Some(r) => {
            a["status"] = json!("refused");
            a["refused"] = r;
        }
        None => a["status"] = json!(ok),
    }
    a
}

/// `doc` with `pools`: every pool a link's source is in, when one is a user
/// grammar's (`pool` stays the soup's own, `ws`, as before).
fn with_pools(mut doc: Value, links: &[Link], types: &BTreeMap<String, At>) -> Value {
    let pools: BTreeSet<&str> = links.iter().filter_map(|l| types.get(&l.from)).map(|a| a.pool.as_str()).collect();
    if pools.iter().any(|p| *p != DEFAULT_POOL) {
        doc["pools"] = json!(pools);
    }
    doc
}

// ----------------------------------------------------------------- unlink

/// Would `node` (of pool `pool`, judged by its pin `grammar`) still have
/// every edge its type requires at mint, with `gone` (kind, target)
/// removed? `None` when it would; else the sentence that says what it would
/// lack. A workspace record is held to workspace@6's table
/// ([`spine::grammar_v6::required_edges`], [`spine::grammar_v6::mint_alternatives`]),
/// a user grammar's record to its type's `required_edges_at_mint`.
fn still_minted(
    snap: &spine::pool::Snapshot<'_>,
    pool: &PoolName,
    grammar: &spine::schema_def::PoolGrammar,
    node: &spine::pool::NodeRecord,
    gone: &BTreeSet<(String, QualifiedNodeId)>,
) -> Result<Option<String>> {
    let edges = snap.edges_from(&node.id, None).map_err(store)?;
    let mut left: BTreeMap<String, u32> = BTreeMap::new();
    for e in &edges {
        if !gone.contains(&(e.kind.clone(), e.dst.clone())) {
            *left.entry(e.kind.clone()).or_default() += 1;
        }
    }
    let has = |k: &str| left.get(k).copied().unwrap_or(0);
    let (required, alts) = if pool.as_str() == DEFAULT_POOL {
        (spine::grammar_v6::required_edges(&node.node_type), spine::grammar_v6::mint_alternatives(&node.node_type))
    } else {
        match grammar.node_types.get(&node.node_type) {
            Some(d) => (d.required_edges_at_mint.clone(), d.required_at_mint_any_of.clone()),
            None => (BTreeMap::new(), vec![]),
        }
    };
    for (k, n) in required {
        if has(&k) < n {
            return Ok(Some(format!("a `{}` keeps at least {n} `{k}` edge(s)", node.node_type)));
        }
    }
    if alts.is_empty() {
        return Ok(None);
    }
    let met = alts.iter().any(|a| a.edges.iter().all(|(k, n)| has(k) >= *n) && a.fields.iter().all(|f| node.fields.contains_key(f)));
    if met {
        return Ok(None);
    }
    Ok(Some(format!(
        "a `{}` keeps at least one `in_region` (or, with none, at least {} `rejected_by` and its `searched` field)",
        node.node_type,
        spine::grammar_v6::REJECTED_BY_MIN
    )))
}

/// **Remove edges** between records the soup holds, in ONE write (module
/// docs: what may not be removed). Every link is judged first; if any is
/// refused nothing is written (`links_refused`). A link whose edge is not
/// there is answered `not-linked` and changes nothing; when none of them is
/// there, nothing is written at all. Answers `{links: [{from, kind, to,
/// status: unlinked | not-linked}], outcome: unlinked | not-linked, written,
/// landed?}`.
pub fn unlink(handle: &spine::Cauldron, t: &Target, links: &[Link], w: &Writer) -> Result<Value> {
    let c = &t.cauldron;
    if links.iter().any(|l| l.note.is_some()) {
        refuse!(Code::Usage, "a removed edge takes no note: it goes, note and all. Nothing was written");
    }
    let (types, mut why, links_plain) = endpoints(handle, links, c)?;
    let links = &links_plain[..];
    let mut present = vec![false; links.len()];
    for (i, l) in links.iter().enumerate() {
        if l.kind == spine::grammar_cauldron::CLAIMS {
            why.insert(i, json!({"reason": "claims-is-soup-claim",
                "detail": "a cluster's claim is released with soup_claim (`release: true`) or `cauldron soup claim --release`, never here"}));
            continue;
        }
        if why.contains_key(&i) {
            continue;
        }
        let (from, to) = (&types[&l.from], &types[&l.to]);
        if let Some(u) = unknown_kind(source_grammar(handle, from)?, &l.kind) {
            why.insert(i, u);
            continue;
        }
        let p = read_pool(c, &from.pool)?;
        let snap = p.snapshot().map_err(store)?;
        if l.kind == "supersedes" {
            // The way out of damage no write makes any more: a self-edge,
            // or an edge on a cycle of `supersedes`, records no replacement
            // and hides every record on it, so it may go.
            let damaged = l.from == l.to || (from.pool == to.pool && reaches(&snap, &from.pool, "supersedes", &l.to, &l.from, &[])?);
            if !damaged {
                why.insert(i, json!({"reason": "supersedes-is-history",
                    "detail": format!("`{}` -[supersedes]-> `{}` records that one replaced the other; removing it would make `{}` read as current again. Write a new record instead. (Only a `supersedes` edge from a record to itself, or one on a cycle of `supersedes` edges, may be removed: neither records a replacement.)", l.from, l.to, l.to)}));
                continue;
            }
        }
        let dst = to.qualified(&l.to);
        present[i] = snap.edges_from(&NodeId(l.from.clone()), Some(&l.kind)).map_err(store)?.iter().any(|e| e.dst == dst);
    }
    // The required edges, judged per source record over every removal the
    // call makes from it.
    let mut gone: BTreeMap<String, BTreeSet<(String, QualifiedNodeId)>> = BTreeMap::new();
    for (i, l) in links.iter().enumerate() {
        if present[i] && !why.contains_key(&i) {
            gone.entry(l.from.clone()).or_default().insert((l.kind.clone(), types[&l.to].qualified(&l.to)));
        }
    }
    for (from, removed) in &gone {
        let at = &types[from];
        let grammar = source_grammar(handle, at)?;
        let p = read_pool(c, &at.pool)?;
        let snap = p.snapshot().map_err(store)?;
        let node = snap.node(&NodeId(from.clone())).map_err(store)?.expect("located above");
        // A record that already lacks a required edge (written before a
        // rule) is not held to it by a removal of something else.
        if still_minted(&snap, &at.pool, grammar, &node, &BTreeSet::new())?.is_some() {
            continue;
        }
        if let Some(lack) = still_minted(&snap, &at.pool, grammar, &node, removed)? {
            for (i, l) in links.iter().enumerate() {
                if &l.from == from && present[i] && !why.contains_key(&i) {
                    why.insert(i, json!({"reason": "required-edge",
                        "detail": format!("removing `{}` -[{}]-> `{}` would leave `{from}` without an edge its type requires: {lack}. Add the replacement edge first (soup_link), then remove this one", l.from, l.kind, l.to)}));
                }
            }
        }
    }
    let mut answers: Vec<Value> = Vec::new();
    for (i, l) in links.iter().enumerate() {
        answers.push(answer(l, &types, why.remove(&i), if present[i] { "accepted" } else { "not-linked" }));
    }
    let mut doc = with_pools(json!({"env": t.env.name, "cauldron": c.name, "pool": DEFAULT_POOL}), links, &types);
    if answers.iter().any(|a| a["status"] == "refused") {
        return Err(refused_links(answers, doc, links.len()));
    }
    // Each edge once, even when the call names it twice.
    let mut seen = BTreeSet::new();
    let take: Vec<&Link> = links
        .iter()
        .enumerate()
        .filter(|(i, l)| present[*i] && seen.insert((l.from.clone(), l.kind.clone(), l.to.clone())))
        .map(|(_, l)| l)
        .collect();
    if take.is_empty() {
        doc["links"] = json!(answers);
        doc["outcome"] = json!("not-linked");
        doc["written"] = json!(false);
        return Ok(doc);
    }
    let (s, b) = statement(&take, &types, "REMOVE");
    match handle.submit(envelope(s, b, w, "soup-unlink")) {
        Ok(spine::Outcome::Committed { tx, seq, deduplicated, .. }) => {
            for a in &mut answers {
                if a["status"] == "accepted" {
                    a["status"] = json!("unlinked");
                }
            }
            doc["links"] = json!(answers);
            doc["outcome"] = json!("unlinked");
            doc["written"] = json!(true);
            doc["landed"] = json!({"tx": tx.to_hex(), "seq": seq, "deduplicated": deduplicated});
            Ok(doc)
        }
        Ok(other) => Err(store(format!("the unlink statement was not committed: {other:?}"))),
        Err(spine::error::SpineError::Refused { witnesses }) => {
            doc["links"] = json!(answers);
            doc["outcome"] = json!("refused");
            doc["witnesses"] = json!(witnesses);
            Err(Error::new(Code::LinksRefused, "the soup refused removing these edges; nothing was written").with_detail(doc))
        }
        Err(e @ spine::error::SpineError::RegionLocked { .. }) => Err(crate::region_lock::spine_error(e)),
        Err(e) => Err(store(format!("removing the edges: {e}"))),
    }
}

// ------------------------------------------------------------------- cite

/// Which citation a removal names.
#[derive(Debug, Clone)]
pub enum Which {
    /// Its place in `meta.cites`, from 0.
    Index(usize),
    /// The one entry holding every key and value of this object.
    Matching(serde_json::Map<String, Value>),
}

/// What a citation change asks.
#[derive(Debug, Clone)]
pub enum CiteOp {
    Add(Value),
    Remove(Which),
}

/// **Was record `id` of the soup's workspace pool written by a ripple?**
/// Then its `dump` citation is its review trail (the review queue groups
/// by it, `--dump` decides by it), and [`cite`] neither adds nor removes
/// one. A ripple's record a merge carried counts too, whichever of the
/// merge's writes it landed in (one whose decision was carried lands in the
/// agent-class main write): the same test the reads make. A cluster, or a
/// record the soup does not hold: no.
pub fn ripple_written(c: &env::Cauldron, id: &str) -> Result<bool> {
    if id.starts_with("CL-") {
        return Ok(false);
    }
    let p = Pool::open_read_only(&Pool::path_in(&c.soup(), &ws())).map_err(|err| store(format!("reading cauldron `{}`'s soup: {err}", c.name)))?;
    let snap = p.snapshot().map_err(store)?;
    let Some(node) = snap.node(&NodeId(id.to_string())).map_err(store)? else { return Ok(false) };
    match snap.created_tx(&NodeId(id.to_string())).map_err(store)? {
        Some(h) => {
            let meta = node.fields.get("meta").map(|f| f.value.clone()).unwrap_or(Value::Null);
            Ok(crate::provenance::Writers::of(c)?.ripple_written(Some(&h.to_hex()), &meta))
        }
        None => Ok(false),
    }
}

/// **Add or remove one entry of a record's `meta.cites`**, by one `SET` of
/// that key and no other. Idempotent: adding a citation the record already
/// carries answers `already-cited`, removing one that matches none answers
/// `not-cited`, and neither writes. Refused as `not_found` for a record the
/// soup does not hold, `usage` for a citation the table refuses
/// (`bad-cite`), an index past the end, or a match that names more than one
/// entry. Answers `{id, op, outcome: cited | uncited | already-cited |
/// not-cited, written, cites, removed?, landed?}`.
pub fn cite(handle: &spine::Cauldron, t: &Target, id: &str, op: &CiteOp, w: &Writer) -> Result<Value> {
    let c = &t.cauldron;
    // `<ID>@<pool>`, as reads print it: that pool's record.
    let (plain_id, named) = split_end(id);
    let id = plain_id.as_str();
    if let Some(want) = &named {
        let at = if id.starts_with("CL-") { Some(crate::clusters::POOL.to_string()) } else { locate(handle, c, &[&id.to_string()])?.remove(id).map(|a| a.pool.as_str().to_string()) };
        match at {
            Some(p) if &p == want => {}
            Some(p) => refuse!(Code::Usage, "`{id}@{want}` names pool `{want}`, but `{id}` is a record of pool `{p}`. Nothing was written"),
            None => refuse!(Code::NotFound, "there is no record `{id}` in pool `{want}` of cauldron `{}`. Nothing was written", c.name),
        }
    }
    let is_cluster = id.starts_with("CL-");
    let mut pool = if is_cluster { PoolName::new(crate::clusters::POOL) } else { ws() };
    if is_cluster && !crate::clusters::has_pool(c) {
        refuse!(Code::NotFound, "there is no record `{id}` in cauldron `{}`. Nothing was written", c.name);
    }
    // Not in `ws`: a user grammar's record, when one of its pools holds it.
    if !is_cluster {
        if let Some(at) = locate(handle, c, &[&id.to_string()])?.remove(id) {
            pool = at.pool;
        }
    }
    let user = pool.as_str() != DEFAULT_POOL && !is_cluster;
    let p = Pool::open_read_only(&Pool::path_in(&c.soup(), &pool)).map_err(|err| store(format!("reading cauldron `{}`'s soup: {err}", c.name)))?;
    let snap = p.snapshot().map_err(store)?;
    let Some(node) = snap.node(&NodeId(id.to_string())).map_err(store)? else {
        refuse!(Code::NotFound, "there is no record `{id}` in cauldron `{}` (in `ws` or a user grammar's pool). Nothing was written", c.name);
    };
    // A citation is an entry of `meta.cites`: a user type that declares no
    // `meta` field carries none (its grammar's rule, not this door's).
    if user {
        let g = handle.grammar(&pool).ok_or_else(|| store(format!("the soup has no pool `{pool}`")))?;
        if !g.node_types.get(&node.node_type).is_some_and(|d| d.fields.contains_key("meta")) {
            return Err(Error::new(
                Code::Usage,
                format!(
                    "`{id}` (type `{}`, user pool `{pool}`): `{}@{}` declares no `meta` field on `{}`, so its records carry no \
                     citations (`meta.cites`). Nothing was written. A grammar that wants them declares `\"meta\": {{\"required\": \
                     false, \"kind\": \"object\"}}` on the type",
                    node.node_type, g.kind, g.version, node.node_type
                ),
            )
            .with_detail(json!({"reason": "no-meta-field", "pool": pool.as_str(), "type": node.node_type})));
        }
    }
    // A record a ripple wrote: its `dump` citation is part of its review
    // trail (the review queue groups by it, and `--dump` approves and
    // rejects by it), so it is neither added nor removed here. A ripple's
    // record a merge carried counts too, whichever of the merge's writes it
    // landed in (one whose decision was carried lands in the agent-class
    // main write): the same test the reads make.
    drop(snap);
    drop(p);
    let ripple_written = !is_cluster && ripple_written(c, id)?;
    let provenance = |entry: &Value| -> Result<()> {
        if ripple_written && entry.get("kind").and_then(Value::as_str) == Some(DUMP_CITE) {
            return Err(Error::new(
                Code::Usage,
                format!(
                    "`{id}` was written by a ripple, and its `dump` citation is part of its review trail: the review \
                     queue groups it by that dump and `--dump` approves or rejects by it. A `dump` citation of a record a \
                     ripple wrote is neither added nor removed (dump-cite-is-provenance). Nothing was written"
                ),
            )
            .with_detail(json!({"reason": DUMP_CITE_IS_PROVENANCE})));
        }
        Ok(())
    };
    let meta = node.fields.get("meta").map(|f| f.value.clone()).unwrap_or(Value::Null);
    let mut cites: Vec<Value> = meta.get("cites").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut doc = json!({"env": t.env.name, "cauldron": c.name, "id": id});
    if user {
        doc["pool"] = json!(pool.as_str());
    }
    let (outcome, removed) = match op {
        CiteOp::Add(v) => {
            doc["op"] = json!("add");
            if let Some(problem) = spine::grammar_v6::cite_problem(&json!({"cites": [v]})) {
                return Err(Error::new(Code::Usage, format!("{} (bad-cite). Nothing was written", problem.replacen("meta.cites[0]", "the citation", 1)))
                    .with_detail(json!({"reason": "bad-cite"})));
            }
            provenance(v)?;
            crate::review::refuse_merge_provenance("the citation", v)?;
            if cites.contains(v) {
                doc["outcome"] = json!("already-cited");
                doc["written"] = json!(false);
                doc["cites"] = json!(cites);
                return Ok(doc);
            }
            cites.push(v.clone());
            ("cited", None)
        }
        CiteOp::Remove(which) => {
            doc["op"] = json!("remove");
            let at = match which {
                Which::Index(i) if *i < cites.len() => Some(*i),
                Which::Index(i) => {
                    refuse!(Code::Usage, "`{id}` has {} citation(s), so there is no citation at index {i} (they count from 0). Nothing was written", cites.len())
                }
                Which::Matching(m) => {
                    let hits: Vec<usize> = cites
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| m.iter().all(|(k, v)| e.get(k) == Some(v)))
                        .map(|(i, _)| i)
                        .collect();
                    if hits.len() > 1 {
                        refuse!(Code::Usage, "the match names {} citations of `{id}` (at {:?}); name one by its index. Nothing was written", hits.len(), hits);
                    }
                    hits.first().copied()
                }
            };
            let Some(at) = at else {
                doc["outcome"] = json!("not-cited");
                doc["written"] = json!(false);
                doc["cites"] = json!(cites);
                return Ok(doc);
            };
            provenance(&cites[at])?;
            let gone = cites.remove(at);
            ("uncited", Some(gone))
        }
    };
    let ty = &node.node_type;
    let statement = format!(
        "MATCH (n: {}/{ty} {{ id: $id }})\nEXPECT COUNT(n) = 1\nSET n.meta.cites = $cites",
        pool.as_str()
    );
    let mut b = Bindings::new();
    b.insert("id".into(), json!(id));
    b.insert("cites".into(), json!(cites));
    match handle.submit(envelope(statement, b, w, "soup-cite")) {
        Ok(spine::Outcome::Committed { tx, seq, deduplicated, .. }) => {
            doc["outcome"] = json!(outcome);
            doc["written"] = json!(true);
            doc["cites"] = json!(cites);
            if let Some(r) = removed {
                doc["removed"] = r;
            }
            doc["landed"] = json!({"tx": tx.to_hex(), "seq": seq, "deduplicated": deduplicated});
            Ok(doc)
        }
        Ok(other) => Err(store(format!("the citation was not committed: {other:?}"))),
        Err(spine::error::SpineError::Refused { witnesses }) => Err(Error::new(
            Code::Usage,
            format!("the soup refused changing `{id}`'s citations; nothing was written"),
        )
        .with_detail(json!({"witnesses": witnesses}))),
        Err(e @ spine::error::SpineError::RegionLocked { .. }) => Err(crate::region_lock::spine_error(e)),
        Err(e) => Err(store(format!("writing the citation: {e}"))),
    }
}

// ----------------------------------------------------------------- direct

/// **One of the acts above, written directly** (no `mcp` service holds the
/// soup): the proofs an import makes (no live soup daemon, the writer lock,
/// at rest), the region guard, then `act` on a handle opened for it. A soup
/// the service holds is refused `locked`, and the command line sends the
/// act to the service instead.
pub fn direct(t: &Target, verb: &str, act: impl FnOnce(&spine::Cauldron) -> Result<Value>) -> Result<Value> {
    let c = &t.cauldron;
    crate::lock::refuse_live_soup(&t.env)?;
    let _lock = crate::lock::take_writer_lock(c, verb)?;
    let unrest = spine::offline::rest_check(&c.soup())
        .map_err(|err| store(format!("REFUSED: cannot read {}'s spine, so cannot prove it is at rest: {err}", c.name)))?;
    if !unrest.is_empty() {
        refuse!(
            Code::NotAtRest,
            "REFUSED: cauldron `{}`'s soup is not at rest ({}); run `cauldron soup recover --env {} --cauldron {}` first. Nothing was written.",
            c.name,
            unrest.join("; "),
            t.env.name,
            c.name
        );
    }
    let mut handle = spine::Cauldron::open(&c.soup(), spine::config::SpineConfig::default())
        .map_err(|err| store(format!("opening cauldron `{}`'s soup: {err}", c.name)))?;
    handle.set_region_guard(crate::region_lock::Guard::new(c));
    act(&handle)
}
