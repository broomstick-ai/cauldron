//! **`soup_changes`**: what landed in a cauldron since a cursor, read
//! through connections that cannot write (never through a writer thread).
//!
//! There are two streams, each a spine's committed-commit counter: the
//! soup's, and the dumps store's (`soup_door::dumps`). Every write that
//! lands bumps its spine's counter by one and stamps its transaction row
//! with the new value (`committed_at`). A cursor
//! (`v1.<soup instance>.<counter>.<dumps counter>`, see [`crate::cursor`])
//! names a point in both; the changes after it are the rows whose
//! `committed_at` is above it, oldest first, in `changes` (the soup) and
//! `dumps` (the dumps store).
//!
//! Each soup change says what the write did, from the pool files' own `*_tx`
//! columns: `create` (it made nodes; `ids` are those nodes), `link` (it made
//! edges between nodes that already existed; `ids` are the edges' sources),
//! `unlink` (it removed edges and did nothing else: a released claim; `ids`
//! are the edges' sources, read from the write's own recorded deltas, since
//! a removed edge is no longer in its pool) or `update` (it changed nodes it
//! did not make). Every pool of the soup is read, the cluster pool
//! included, so a cluster's creation and a claim are changes like any
//! other, on the one counter every pool's writes share. At most
//! [`IDS_PER_CHANGE`] ids are listed per change; `count` is how many there
//! were. Each dump entry is `{seq, at, op: "dump", id, kind, bytes, state}`
//! (ids and sizes, never text).
//!
//! A merge (`crate::merge`) lands its records in at most three soup writes
//! (its main write; its ripple part, the records a ripple wrote that nobody
//! decided; and the edges from the first's records to the second's), each a
//! change of an existing op (`create`, or `link`), and each carries `merge`:
//! `{id, part: main|ripple|links, from: {cauldron_id}, counts: {<type>: n},
//! edges}` (plus `resync` when `ids` could not list every record). The dumps
//! it carried are `dump` entries, one per dump, as every dump is its own
//! write.
//!
//! Refusals: a cursor that is not this door's form, or that belongs to
//! another cauldron (or to one removed and made again under the same name),
//! is `cursor_unknown`; one whose counter is above its stream's is
//! `cursor_stale`. With no `since`, the answer is the current cursor and no
//! changes: never a dump of the soup.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::cursor;
use crate::target::Target;
use crate::{Code, Error};

/// The refusals of this module are soup-door errors; a door hands them back
/// by their code (`cursor_unknown`, `cursor_stale`, `store`, `usage`).
type Refusal = Error;

/// How many changes one call answers when it does not say.
pub const DEFAULT_LIMIT: usize = 100;
/// The most one call answers, whatever it asks.
pub const MAX_LIMIT: usize = 1000;
/// The most ids listed for one change.
pub const IDS_PER_CHANGE: usize = 50;

/// The hint a cut list carries: how to read on from where it stopped.
pub const NEXT: &str = "more changes landed after these: call soup_changes again with `since` set to this answer's `cursor`";

fn store(t: &Target, e: &dyn std::fmt::Display) -> Refusal {
    Error::new(Code::Store, format!("reading cauldron `{}`'s change stream: {e}", t.cauldron.name))
}

fn unknown(t: &Target, since: &str, why: &str) -> Refusal {
    Error::new(
        Code::CursorUnknown,
        format!(
            "`{since}` is not a cursor of cauldron `{}` as it stands ({why}). Nothing was read. Call soup_changes \
             without `since` (or soup_status) for the current cursor, and read the soup itself for what is \
             already there.",
            t.cauldron.name
        ),
    )
}

/// The counters `since` names (the soup's, the dumps store's), checked
/// against this cauldron and its heads. A cursor with no dumps part (one
/// written before dumps existed) is read as dumps counter 0.
pub fn parse_since(t: &Target, since: &str, head: u64, dumps_head: u64) -> Result<(u64, u64), Refusal> {
    let parts: Vec<&str> = since.trim().split('.').collect();
    let (inst, n, d) = match parts.as_slice() {
        ["v1", inst, n] => (*inst, *n, None),
        ["v1", inst, n, d] => (*inst, *n, Some(*d)),
        _ => return Err(unknown(t, since, "a cursor has the form v1.<instance>.<counter>.<dumps counter>")),
    };
    if inst != cursor::instance(&t.cauldron) {
        return Err(unknown(
            t,
            since,
            "it was answered by another cauldron, or by one of the same name that has since been removed and made again",
        ));
    }
    let n: u64 = n.parse().map_err(|_| unknown(t, since, "its counter is not a number"))?;
    let d: u64 = match d {
        None => 0,
        Some(d) => d.parse().map_err(|_| unknown(t, since, "its dumps counter is not a number"))?,
    };
    if n > head || d > dumps_head {
        return Err(Error::new(
            Code::CursorStale,
            format!(
                "`{since}` is ahead of cauldron `{}`, whose cursor is `{}`: it has not seen that many \
                 writes. Nothing was read. Call soup_changes without `since` for the current cursor.",
                t.cauldron.name,
                cursor::cursor(&t.cauldron, head, dumps_head)
            ),
        ));
    }
    Ok((n, d))
}

/// One transaction row of the stream.
struct Row {
    counter: u64,
    hash: String,
    at: String,
    by: Option<String>,
    /// The initiator's `dispatch`, when it has one (a merge's carries its
    /// mark: `crate::provenance::parse_mark`).
    dispatch: Option<String>,
}

/// The answer: `{env, cauldron, since, cursor, head, changes, dumps,
/// more}` (+ `next` when a list stopped short). `cursor` is where to read
/// on from.
pub fn changes(t: &Target, since: Option<&str>, limit: Option<usize>) -> Result<Value, Refusal> {
    let soup = t.soup();
    let head = cursor::counter_offline(&soup).map_err(|e| store(t, &e))?;
    let dumps_head = crate::dumps::counter(&t.cauldron).map_err(|e| store(t, &e))?;
    let head_cursor = cursor::cursor(&t.cauldron, head, dumps_head);
    let Some(since) = since else {
        return Ok(json!({
            "env": t.env.name,
            "cauldron": t.cauldron.name,
            "since": Value::Null,
            "cursor": head_cursor,
            "head": head_cursor,
            "changes": [],
            "dumps": [],
            "more": false,
            "note": "no `since`: this is the current cursor only; pass it as `since` later to read what landed after it",
        }));
    };
    let (from, dumps_from) = parse_since(t, since, head, dumps_head)?;
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let rows = rows_after(&soup, from, head, limit + 1).map_err(|e| store(t, &e))?;
    let more_soup = rows.len() > limit;
    let rows = &rows[..rows.len().min(limit)];
    let pools = pool_files(&soup).map_err(|e| store(t, &e))?;
    let mut list = Vec::new();
    for r in rows {
        list.push(change(&soup, &pools, r).map_err(|e| store(t, &e))?);
    }
    let mut dumps = crate::dumps::landed_after(&t.cauldron, dumps_from, dumps_head, limit).map_err(|e| store(t, &e))?;
    let more_dumps = dumps.len() > limit;
    dumps.truncate(limit);
    let soup_at = match (more_soup, rows.last()) {
        (true, Some(last)) => last.counter,
        _ => head,
    };
    let dumps_at = match (more_dumps, dumps.last().and_then(|d| d["seq"].as_u64())) {
        (true, Some(last)) => last,
        _ => dumps_head,
    };
    let more = more_soup || more_dumps;
    let mut doc = json!({
        "env": t.env.name,
        "cauldron": t.cauldron.name,
        "since": since,
        "cursor": cursor::cursor(&t.cauldron, soup_at, dumps_at),
        "head": head_cursor,
        "changes": list,
        "dumps": dumps,
        "more": more,
    });
    if more {
        doc["next"] = json!(NEXT);
    }
    Ok(doc)
}

/// After a whole-reply bound cut a list: the cursor's part for that list is
/// moved back to the last item kept, so reading on from it misses nothing.
pub fn after_budget(t: &Target, doc: &mut Value) {
    let dropped = |k: &str| doc["truncated"]["dropped_from"][k].as_u64().unwrap_or(0);
    let (soup_cut, dumps_cut) = (dropped("changes"), dropped("dumps"));
    if soup_cut == 0 && dumps_cut == 0 {
        return;
    }
    let Some(cur) = doc["cursor"].as_str().map(str::to_string) else { return };
    let parts: Vec<&str> = cur.split('.').collect();
    let (Some(s), Some(d)) = (parts.get(2).and_then(|x| x.parse::<u64>().ok()), parts.get(3).and_then(|x| x.parse::<u64>().ok())) else {
        return;
    };
    let last = |k: &str| doc[k].as_array().and_then(|c| c.last()).and_then(|c| c["seq"].as_u64());
    let s = if soup_cut > 0 { last("changes").unwrap_or(s.min(parse_from(doc))) } else { s };
    let d = if dumps_cut > 0 { last("dumps").unwrap_or(d.min(parse_dumps_from(doc))) } else { d };
    doc["cursor"] = json!(cursor::cursor(&t.cauldron, s, d));
    doc["more"] = json!(true);
    doc["next"] = json!(NEXT);
}

/// The soup counter `since` named (0 when unreadable): where to read on
/// from when a cut left no change in the list.
fn parse_from(doc: &Value) -> u64 {
    doc["since"].as_str().and_then(|s| s.split('.').nth(2)).and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn parse_dumps_from(doc: &Value) -> u64 {
    doc["since"].as_str().and_then(|s| s.split('.').nth(3)).and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn rows_after(soup: &Path, from: u64, head: u64, n: usize) -> Result<Vec<Row>, String> {
    let conn = spine::offline::read_only_connection(&soup.join(spine::spine_db::SPINE_FILE)).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT committed_at, hash, ts, initiator FROM transactions
             WHERE committed_at IS NOT NULL AND committed_at > ?1 AND committed_at <= ?2
             ORDER BY committed_at LIMIT ?3",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite_params(from, head, n), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (counter, hash, at, initiator) = row.map_err(|e| e.to_string())?;
        let v = serde_json::from_str::<Value>(&initiator).ok();
        let by = v.as_ref().and_then(|v| v.get("thread").and_then(Value::as_str).map(str::to_string));
        // A merge's mark ends its `dispatch` (or, for its ripple part, its
        // ripple initiator's `step_id`).
        let dispatch = v.as_ref().and_then(|v| v.get("dispatch").or_else(|| v.get("step_id")).and_then(Value::as_str).map(str::to_string));
        out.push(Row { counter: counter.max(0) as u64, hash, at, by, dispatch });
    }
    Ok(out)
}

fn rusqlite_params(from: u64, head: u64, n: usize) -> [i64; 3] {
    [from as i64, head as i64, n as i64]
}

/// Every pool file of the soup (`<pool>.db`, not `spine.db`), by name.
fn pool_files(soup: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(soup).map_err(|e| format!("listing {}: {e}", soup.display()))? {
        let p = entry.map_err(|e| e.to_string())?.path();
        let stem = p.file_stem().and_then(|n| n.to_str()).unwrap_or("");
        if p.extension().and_then(|x| x.to_str()) == Some("db") && stem != "spine" {
            out.push(p);
        }
    }
    out.sort();
    Ok(out)
}

/// The sources of the edges a landed write added (`removed: false`) or
/// removed (`true`), from its row's recorded payload: `(pool, src)`. What a
/// write did to edges that are no longer there (a claim since released) is
/// read here, since the pool holds only the edges that stand.
fn payload_edges(soup: &Path, hash: &str, removed: bool) -> Result<Vec<(String, String)>, String> {
    let conn = spine::offline::read_only_connection(&soup.join(spine::spine_db::SPINE_FILE)).map_err(|e| e.to_string())?;
    let payload: String = conn
        .query_row("SELECT payload FROM transactions WHERE hash = ?1 AND decided = 1", [hash], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let p: spine::delta::CompiledPayload = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for part in &p.parts {
        for d in &part.deltas {
            match d {
                spine::delta::Delta::RemoveEdge { src, .. } if removed => out.push((part.pool.as_str().to_string(), src.0.clone())),
                spine::delta::Delta::AddEdge { src, .. } if !removed => out.push((part.pool.as_str().to_string(), src.0.clone())),
                _ => {}
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// The nodes a landed write set a field of (or sealed), from its row's
/// recorded payload: `(pool, node)`. Read here rather than from the pool's
/// `updated_tx` column, which names only a node's LATEST write: a node
/// changed again later (a second citation, an approval then a citation)
/// would otherwise leave this write's entry with no ids.
fn payload_set_nodes(soup: &Path, hash: &str) -> Result<Vec<(String, String)>, String> {
    let conn = spine::offline::read_only_connection(&soup.join(spine::spine_db::SPINE_FILE)).map_err(|e| e.to_string())?;
    let payload: String = conn
        .query_row("SELECT payload FROM transactions WHERE hash = ?1 AND decided = 1", [hash], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let p: spine::delta::CompiledPayload = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
    let mut out: Vec<(String, String)> = Vec::new();
    let mut created: BTreeSet<(String, String)> = BTreeSet::new();
    for part in &p.parts {
        for d in &part.deltas {
            let pool = part.pool.as_str().to_string();
            match d {
                spine::delta::Delta::CreateNode { id, .. } => {
                    created.insert((pool, id.0.clone()));
                }
                spine::delta::Delta::SetField { node, .. } | spine::delta::Delta::Seal { node } => {
                    let k = (pool, node.0.clone());
                    if !out.contains(&k) {
                        out.push(k);
                    }
                }
                _ => {}
            }
        }
    }
    out.retain(|k| !created.contains(k));
    Ok(out)
}

/// A write that left no node and no standing edge of its own: `link` when
/// it added edges (since removed), else `unlink` when it removed some.
fn edge_op(soup: &Path, hash: &str) -> Result<Option<(&'static str, Vec<(String, String)>)>, String> {
    let added = payload_edges(soup, hash, false)?;
    if !added.is_empty() {
        return Ok(Some(("link", added)));
    }
    let removed = payload_edges(soup, hash, true)?;
    Ok((!removed.is_empty()).then_some(("unlink", removed)))
}

fn change(soup: &Path, pools: &[std::path::PathBuf], r: &Row) -> Result<Value, String> {
    let mut created: Vec<Value> = Vec::new();
    let mut linked: BTreeSet<(usize, String)> = BTreeSet::new();
    let mut updated: Vec<Value> = Vec::new();
    let mut edges = 0usize;
    for (pi, p) in pools.iter().enumerate() {
        let conn = spine::offline::read_only_connection(p).map_err(|e| e.to_string())?;
        let node = |sql: &str| -> Result<Vec<Value>, String> {
            let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([&r.hash], |row| {
                    Ok(json!({"id": row.get::<_, String>(0)?, "type": row.get::<_, String>(1)?, "handle": handle(row.get::<_, Option<String>>(2)?)}))
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        };
        created.extend(node(
            "SELECT n.id, n.node_type, f.value FROM nodes n LEFT JOIN node_fields f ON f.node_id = n.id AND f.field = 'handle'
             WHERE n.created_tx = ?1 ORDER BY n.rowid",
        )?);
        let mut stmt = conn.prepare("SELECT src FROM edges WHERE created_tx = ?1").map_err(|e| e.to_string())?;
        for src in stmt.query_map([&r.hash], |row| row.get::<_, String>(0)).map_err(|e| e.to_string())? {
            edges += 1;
            linked.insert((pi, src.map_err(|e| e.to_string())?));
        }
    }
    // The nodes it changed, from its own recorded deltas (every write it
    // made, whatever wrote those nodes since).
    if created.is_empty() && linked.is_empty() {
        for (pool, id) in payload_set_nodes(soup, &r.hash)? {
            let file = soup.join(format!("{pool}.db"));
            let conn = spine::offline::read_only_connection(&file).map_err(|e| e.to_string())?;
            let found = conn
                .query_row(
                    "SELECT n.node_type, f.value FROM nodes n LEFT JOIN node_fields f ON f.node_id = n.id AND f.field = 'handle' WHERE n.id = ?1",
                    [&id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .ok();
            let (ty, h) = found.unwrap_or_default();
            updated.push(json!({"id": id, "type": ty, "handle": handle(h)}));
        }
    }
    let (op, ids): (&str, Vec<Value>) = if !created.is_empty() {
        ("create", created)
    } else if !linked.is_empty() {
        let mut v = Vec::new();
        for (pi, src) in &linked {
            let conn = spine::offline::read_only_connection(&pools[*pi]).map_err(|e| e.to_string())?;
            let found = conn
                .query_row(
                    "SELECT n.node_type, f.value FROM nodes n LEFT JOIN node_fields f ON f.node_id = n.id AND f.field = 'handle' WHERE n.id = ?1",
                    [src],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .ok();
            let (ty, h) = found.unwrap_or_default();
            v.push(json!({"id": src, "type": ty, "handle": handle(h)}));
        }
        ("link", v)
    } else if let Some((op, srcs)) = if updated.is_empty() { edge_op(soup, &r.hash)? } else { None } {
        let mut v = Vec::new();
        for (pool, src) in srcs {
            let file = soup.join(format!("{pool}.db"));
            let conn = spine::offline::read_only_connection(&file).map_err(|e| e.to_string())?;
            let found = conn
                .query_row(
                    "SELECT n.node_type, f.value FROM nodes n LEFT JOIN node_fields f ON f.node_id = n.id AND f.field = 'handle' WHERE n.id = ?1",
                    [&src],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .ok();
            let (ty, h) = found.unwrap_or_default();
            v.push(json!({"id": src, "type": ty, "handle": handle(h)}));
        }
        (op, v)
    } else {
        ("update", updated)
    };
    let count = ids.len();
    let mut c = Map::new();
    c.insert("seq".into(), json!(r.counter));
    c.insert("at".into(), json!(r.at));
    c.insert("op".into(), json!(op));
    c.insert("tx".into(), json!(r.hash));
    if let Some(by) = &r.by {
        c.insert("by".into(), json!(by));
    }
    c.insert("count".into(), json!(count));
    if edges > 0 {
        c.insert("edges".into(), json!(edges));
    }
    // A merge (`crate::merge`) is one `create` change, which says it is one:
    // which merge, from which cauldron, and how many records of each type.
    let mark = r.dispatch.as_deref().and_then(|d| {
        crate::provenance::parse_mark(d)
            .map(|(m, src, _, _)| ("main".to_string(), m, src))
            .or_else(|| crate::provenance::parse_part(d).map(|(part, m, src, _, _)| (part, m, src)))
    });
    if let Some((part, merge, source_id)) = mark {
        let mut by_type: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        for x in &ids {
            *by_type.entry(x["type"].as_str().unwrap_or("").to_string()).or_default() += 1;
        }
        let mut m = json!({"id": merge, "part": part, "from": {"cauldron_id": source_id}, "counts": by_type, "edges": edges});
        if count > IDS_PER_CHANGE {
            m["resync"] = json!(format!(
                "this merge created {count} records and `ids` lists the first {IDS_PER_CHANGE}: read the soup again \
                 (soup_regions, soup_nodes) to see them all"
            ));
        }
        c.insert("merge".into(), m);
    }
    c.insert("ids".into(), Value::Array(ids.into_iter().take(IDS_PER_CHANGE).collect()));
    Ok(Value::Object(c))
}

/// A `handle` field's stored JSON text, as the string it holds.
fn handle(stored: Option<String>) -> Value {
    stored.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null)
}

// ----------------------------------------------------------------- waiting

/// The longest a `soup_changes` call (or `cauldron soup changes --wait`) may
/// wait for a change, in seconds.
pub const WAIT_MAX_SECS: u64 = 60;

/// What a waiting call sleeps on: a generation number that moves whenever
/// something may have landed, and a way to sleep until it moves (or `max`
/// passes).
pub trait Wake: Sync {
    fn generation(&self) -> u64;
    fn wait_past(&self, generation: u64, max: std::time::Duration);
}

/// A [`Wake`] a writer bumps after every write it lands, so every call
/// waiting on it wakes at once (a condition variable: a waiter sleeps, it
/// never spins).
#[derive(Default)]
pub struct Pulse {
    generation: std::sync::Mutex<u64>,
    moved: std::sync::Condvar,
}

impl Pulse {
    /// Something may have landed: wake every waiter.
    pub fn bump(&self) {
        let mut g = self.generation.lock().unwrap_or_else(|p| p.into_inner());
        *g = g.wrapping_add(1);
        self.moved.notify_all();
    }
}

impl Wake for Pulse {
    fn generation(&self) -> u64 {
        *self.generation.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn wait_past(&self, generation: u64, max: std::time::Duration) {
        let g = self.generation.lock().unwrap_or_else(|p| p.into_inner());
        let _ = self.moved.wait_timeout_while(g, max, |now| *now == generation);
    }
}

/// A [`Wake`] that nothing bumps: each wait is a plain sleep of `max`. What a
/// process that writes nothing itself uses (the command line).
pub struct Sleep;

impl Wake for Sleep {
    fn generation(&self) -> u64 {
        0
    }
    fn wait_past(&self, _: u64, max: std::time::Duration) {
        std::thread::sleep(max);
    }
}

/// **[`changes`] that waits**: answers as soon as anything lands after
/// `since` (a soup write or a dump), or when `wait` has passed with nothing
/// (then `changes` is empty and `cursor` is `since`'s point, unchanged).
/// Between looks it sleeps on `wake`, for at most `look` at a time, so a
/// write `wake` is never told about (another process's) is still seen
/// within `look`. `allowed` runs before every look after the first: a
/// refusal there (the grant was revoked, say) ends the wait with that
/// refusal. Nothing is held while it waits: no writer, no lock, no open
/// store. The answer carries `waited: {asked_secs, ms, ended: "landed" |
/// "timeout"}`. `wait` is at most [`WAIT_MAX_SECS`].
pub fn changes_waiting(
    t: &Target,
    since: &str,
    limit: Option<usize>,
    wait: std::time::Duration,
    wake: &dyn Wake,
    look: std::time::Duration,
    allowed: &mut dyn FnMut() -> Result<(), Refusal>,
) -> Result<Value, Refusal> {
    if wait > std::time::Duration::from_secs(WAIT_MAX_SECS) {
        return Err(Error::new(Code::Usage, format!("a change wait is 0 to {WAIT_MAX_SECS} seconds")));
    }
    let start = std::time::Instant::now();
    let deadline = start + wait;
    let mut first = true;
    loop {
        if !first {
            allowed()?;
        }
        first = false;
        // The generation is read BEFORE the store, so a write that lands
        // between the look and the sleep moves it and the sleep returns.
        let g = wake.generation();
        let mut doc = changes(t, Some(since), limit)?;
        let landed = doc["changes"].as_array().is_some_and(|a| !a.is_empty()) || doc["dumps"].as_array().is_some_and(|a| !a.is_empty());
        let now = std::time::Instant::now();
        if landed || now >= deadline {
            doc["waited"] = json!({
                "asked_secs": wait.as_secs(),
                "ms": now.duration_since(start).as_millis() as u64,
                "ended": if landed { "landed" } else { "timeout" },
            });
            return Ok(doc);
        }
        wake.wait_past(g, (deadline - now).min(look));
    }
}
