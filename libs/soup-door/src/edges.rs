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

fn read_ws(c: &env::Cauldron) -> Result<Pool> {
    Pool::open_read_only(&Pool::path_in(&c.soup(), &ws()))
        .map_err(|err| store(format!("reading cauldron `{}`'s soup: {err}", c.name)))
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
fn statement(links: &[&Link], types: &BTreeMap<String, String>, verb: &str) -> (String, Bindings) {
    let mut binding: BTreeMap<&str, String> = BTreeMap::new();
    let (mut lines, mut expects, mut acts) = (Vec::new(), Vec::new(), Vec::new());
    let mut params = Bindings::new();
    for l in links {
        for end in [&l.from, &l.to] {
            if !binding.contains_key(end.as_str()) {
                let b = format!("n{}", binding.len());
                lines.push(format!("MATCH ({b}: {DEFAULT_POOL}/{} {{ id: $id_{b} }})", types[end]));
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

/// The record ids of `links` that the soup does not hold, as refusals, and
/// the types of those it does.
fn endpoints(snap: &spine::pool::Snapshot<'_>, links: &[Link], c: &env::Cauldron) -> Result<(BTreeMap<String, String>, BTreeMap<usize, Value>)> {
    let mut types = BTreeMap::new();
    let mut why = BTreeMap::new();
    for (i, l) in links.iter().enumerate() {
        for end in [&l.from, &l.to] {
            if why.contains_key(&i) || types.contains_key(end) {
                continue;
            }
            match snap.node(&NodeId(end.clone())).map_err(store)? {
                Some(n) => {
                    types.insert(end.clone(), n.node_type);
                }
                None => {
                    why.insert(i, json!({"reason": "unknown-node", "detail": format!("`{end}` is not a node in cauldron `{}`", c.name)}));
                }
            }
        }
    }
    Ok((types, why))
}

fn unknown_kind(grammar: &spine::schema_def::PoolGrammar, kind: &str) -> Option<Value> {
    (!grammar.edge_kinds.contains_key(kind)).then(|| {
        let kinds: Vec<&String> = grammar.edge_kinds.keys().collect();
        json!({"reason": "unknown-kind", "detail": format!("`{kind}` is not an edge kind of this soup's grammar"), "kinds": kinds})
    })
}

/// The edge kinds whose graph may hold no cycle and no self-edge.
pub const ACYCLIC_KINDS: [&str; 2] = ["supersedes", "narrows"];

/// The edge kind added only with the record it leaves.
pub const AT_CREATION_KINDS: [&str; 1] = ["supersedes"];

/// Does `from` reach `to` by `kind` edges of the soup, plus `extra` edges
/// (from, to) of the same kind? (Breadth first; every node once.)
fn reaches(snap: &spine::pool::Snapshot<'_>, kind: &str, from: &str, to: &str, extra: &[(String, String)]) -> Result<bool> {
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
            if e.dst.pool == ws() {
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
    let pool = ws();
    let grammar = handle.grammar(&pool).ok_or_else(|| store("the soup has no `ws` pool"))?.clone();
    for l in links {
        if let Some(n) = &l.note {
            crate::notes::check(n, &format!("the link `{}` -[{}]-> `{}`", l.from, l.kind, l.to))?;
        }
    }
    let p = read_ws(c)?;
    let snap = p.snapshot().map_err(store)?;
    let (types, mut why) = endpoints(&snap, links, c)?;
    // An edge the soup already holds is answered `already-linked` and not
    // written again (the spine would treat it as a no-op, but the write
    // would still take a change entry pointing at nothing new).
    let mut present = vec![false; links.len()];
    for (i, l) in links.iter().enumerate() {
        if !why.contains_key(&i) {
            let dst = QualifiedNodeId { pool: pool.clone(), id: NodeId(l.to.clone()) };
            present[i] = snap.edges_from(&NodeId(l.from.clone()), Some(&l.kind)).map_err(store)?.iter().any(|e| e.dst == dst);
        }
    }
    drop(snap);
    drop(p);
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
        if ACYCLIC_KINDS.contains(&l.kind.as_str()) && !present[i] {
            let others: Vec<(String, String)> =
                new_edges.iter().filter(|(j, k, _, _)| *j != i && *k == l.kind).map(|(_, _, f, t)| (f.clone(), t.clone())).collect();
            let p = read_ws(c)?;
            let snap = p.snapshot().map_err(store)?;
            if l.from == l.to || reaches(&snap, &l.kind, &l.to, &l.from, &others)? {
                why.insert(i, loop_refusal(l));
            }
        }
    }
    let mut answers = Vec::new();
    for (i, l) in links.iter().enumerate() {
        if !why.contains_key(&i) {
            if let Some(u) = unknown_kind(&grammar, &l.kind) {
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
        let mut a = json!({"from": l.from, "kind": l.kind, "to": l.to});
        match why.remove(&i) {
            Some(r) => {
                a["status"] = json!("refused");
                a["refused"] = r;
            }
            None => a["status"] = json!(if present[i] { "already-linked" } else { "accepted" }),
        }
        answers.push(a);
    }
    let doc = json!({"env": t.env.name, "cauldron": c.name, "pool": pool.as_str()});
    if answers.iter().any(|a| a["status"] == "refused") {
        return Err(refused_links(answers, doc, links.len()));
    }
    let mut doc = doc;
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

// ----------------------------------------------------------------- unlink

/// Would `node` still have every edge the grammar requires of its type at
/// mint, with `gone` (kind, target) removed? `None` when it would; else the
/// sentence that says what it would lack.
fn still_minted(snap: &spine::pool::Snapshot<'_>, node: &spine::pool::NodeRecord, gone: &BTreeSet<(String, String)>) -> Result<Option<String>> {
    let edges = snap.edges_from(&node.id, None).map_err(store)?;
    let mut left: BTreeMap<String, u32> = BTreeMap::new();
    for e in &edges {
        if !gone.contains(&(e.kind.clone(), e.dst.id.0.clone())) || e.dst.pool != ws() {
            *left.entry(e.kind.clone()).or_default() += 1;
        }
    }
    let has = |k: &str| left.get(k).copied().unwrap_or(0);
    for (k, n) in spine::grammar_v6::required_edges(&node.node_type) {
        if has(&k) < n {
            return Ok(Some(format!("a `{}` keeps at least {n} `{k}` edge(s)", node.node_type)));
        }
    }
    let alts = spine::grammar_v6::mint_alternatives(&node.node_type);
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
    let pool = ws();
    let grammar = handle.grammar(&pool).ok_or_else(|| store("the soup has no `ws` pool"))?.clone();
    if links.iter().any(|l| l.note.is_some()) {
        refuse!(Code::Usage, "a removed edge takes no note: it goes, note and all. Nothing was written");
    }
    let p = read_ws(c)?;
    let snap = p.snapshot().map_err(store)?;
    let (types, mut why) = endpoints(&snap, links, c)?;
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
        if let Some(u) = unknown_kind(&grammar, &l.kind) {
            why.insert(i, u);
            continue;
        }
        if l.kind == "supersedes" {
            // The way out of damage no write makes any more: a self-edge,
            // or an edge on a cycle of `supersedes`, records no replacement
            // and hides every record on it, so it may go.
            let damaged = l.from == l.to || reaches(&snap, "supersedes", &l.to, &l.from, &[])?;
            if !damaged {
                why.insert(i, json!({"reason": "supersedes-is-history",
                    "detail": format!("`{}` -[supersedes]-> `{}` records that one replaced the other; removing it would make `{}` read as current again. Write a new record instead. (Only a `supersedes` edge from a record to itself, or one on a cycle of `supersedes` edges, may be removed: neither records a replacement.)", l.from, l.to, l.to)}));
                continue;
            }
        }
        let dst = QualifiedNodeId { pool: pool.clone(), id: NodeId(l.to.clone()) };
        present[i] = snap.edges_from(&NodeId(l.from.clone()), Some(&l.kind)).map_err(store)?.iter().any(|e| e.dst == dst);
    }
    // The required edges, judged per source record over every removal the
    // call makes from it.
    let mut gone: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    for (i, l) in links.iter().enumerate() {
        if present[i] && !why.contains_key(&i) {
            gone.entry(l.from.clone()).or_default().insert((l.kind.clone(), l.to.clone()));
        }
    }
    for (from, removed) in &gone {
        let node = snap.node(&NodeId(from.clone())).map_err(store)?.expect("checked above");
        // A record that already lacks a required edge (written before a
        // rule) is not held to it by a removal of something else.
        if still_minted(&snap, &node, &BTreeSet::new())?.is_some() {
            continue;
        }
        if let Some(lack) = still_minted(&snap, &node, removed)? {
            for (i, l) in links.iter().enumerate() {
                if &l.from == from && present[i] && !why.contains_key(&i) {
                    why.insert(i, json!({"reason": "required-edge",
                        "detail": format!("removing `{}` -[{}]-> `{}` would leave `{from}` without an edge its type requires: {lack}. Add the replacement edge first (soup_link), then remove this one", l.from, l.kind, l.to)}));
                }
            }
        }
    }
    drop(snap);
    drop(p);
    let mut answers: Vec<Value> = Vec::new();
    for (i, l) in links.iter().enumerate() {
        let mut a = json!({"from": l.from, "kind": l.kind, "to": l.to});
        match why.remove(&i) {
            Some(r) => {
                a["status"] = json!("refused");
                a["refused"] = r;
            }
            None => a["status"] = json!(if present[i] { "accepted" } else { "not-linked" }),
        }
        answers.push(a);
    }
    let mut doc = json!({"env": t.env.name, "cauldron": c.name, "pool": pool.as_str()});
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
    let is_cluster = id.starts_with("CL-");
    let pool = if is_cluster { PoolName::new(crate::clusters::POOL) } else { ws() };
    if is_cluster && !crate::clusters::has_pool(c) {
        refuse!(Code::NotFound, "there is no record `{id}` in cauldron `{}`. Nothing was written", c.name);
    }
    let p = Pool::open_read_only(&Pool::path_in(&c.soup(), &pool)).map_err(|err| store(format!("reading cauldron `{}`'s soup: {err}", c.name)))?;
    let snap = p.snapshot().map_err(store)?;
    let Some(node) = snap.node(&NodeId(id.to_string())).map_err(store)? else {
        refuse!(Code::NotFound, "there is no record `{id}` in cauldron `{}`. Nothing was written", c.name);
    };
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
