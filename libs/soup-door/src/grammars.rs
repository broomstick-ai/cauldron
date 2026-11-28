//! **A cauldron's grammars, and the user grammars it may gain**
//! (`cauldron soup grammar check | add | list | show`).
//!
//! A soup is made with two pools, `ws` (`workspace@6`) and `cauldron`
//! (`cauldron@1`). A user grammar (an "ontology": its kind always has a
//! hyphen) joins as a third pool by [`add`], an owner act the command line
//! gates (`owner_acts`), after [`spine::user_grammar::parse_file`] and
//! [`spine::user_grammar::check_user`] found nothing wrong with the file.
//!
//! Once it is there, a record of one of its types is written like any other:
//! [`routes`] sends it to its pool by type (types are unique across a
//! cauldron), from `cauldron soup import` and from `soup_put` alike, and the
//! reads find it by type (`nodes`, `search`) and by id (`get`).

use std::collections::BTreeMap;

use serde_json::{json, Value};
use spine::pool::Pool;
use spine::schema_def::{PoolGrammar, PoolName};
use spine::user_grammar::{self as ug, Problem};

use crate::target::Target;
use crate::{refuse, Code, Error, Result};

fn store(err: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, err.to_string())
}

/// Every pool of the cauldron's soup and its pinned grammar, read from the
/// pool files themselves (no writer is needed, and none is taken), and
/// judged: a user pool file `grammar add` would refuse, or one not bound to
/// this soup's spine, is refused by name with the way out
/// ([`spine::user_grammar::refusal`]), so no read routes by it.
pub fn pins(c: &env::Cauldron) -> Result<Vec<PoolGrammar>> {
    let found = pins_as_found(c)?;
    let refs: Vec<&PoolGrammar> = found.iter().collect();
    if let Some(why) = ug::refusal(&c.soup(), &refs).map_err(|e| store(format!("checking cauldron `{}`'s pool files: {e}", c.name)))? {
        refuse!(Code::Store, "REFUSED: cauldron `{}`'s soup: {why}", c.name);
    }
    Ok(found)
}

/// [`pins`] as the files hold them, unjudged (for `grammar list`, which
/// shows what is wrong rather than refusing).
pub fn pins_as_found(c: &env::Cauldron) -> Result<Vec<PoolGrammar>> {
    let soup = c.soup();
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir(&soup) else { return Ok(out) };
    let mut files: Vec<std::path::PathBuf> = dir.filter_map(|e| e.ok().map(|e| e.path())).collect();
    files.sort();
    for f in files {
        if f.extension().and_then(|e| e.to_str()) != Some("db") || f.file_name().and_then(|n| n.to_str()) == Some(spine::spine_db::SPINE_FILE) {
            continue;
        }
        let pool = Pool::open_read_only(&f).map_err(|e| store(format!("reading cauldron `{}`'s soup: {}", c.name, ug::unreadable(&f, e))))?;
        // A pool's file is `<pool>.db`, as the spine's open requires: a copy
        // under another name (`reading-list-backup.db`) is not read as the
        // pool it holds.
        if Pool::path_in(&soup, &pool.grammar().pool) != f {
            refuse!(
                Code::Store,
                "REFUSED: cauldron `{}`'s soup holds `{}`, a file of pool `{}` under another name (its file is `{}.db`): a copy \
                 of a pool file left in the soup directory. Move it out of the soup directory; nothing reads or writes the soup's \
                 user pools while it is there",
                c.name,
                f.display(),
                pool.grammar().pool,
                pool.grammar().pool
            );
        }
        out.push(pool.grammar().clone());
    }
    Ok(out)
}

/// The spine's registry of this soup's user pools (`spine::user_registry`),
/// read through a connection that cannot write; empty when it has none.
pub fn registry(c: &env::Cauldron) -> Result<BTreeMap<String, spine::user_registry::Row>> {
    let spine_file = c.soup().join(spine::spine_db::SPINE_FILE);
    if !spine_file.exists() {
        return Ok(BTreeMap::new());
    }
    let conn = spine::offline::read_only_connection(&spine_file).map_err(|e| store(format!("reading cauldron `{}`'s spine: {e}", c.name)))?;
    Ok(spine::user_registry::read(&conn).map_err(|e| store(format!("reading cauldron `{}`'s spine: {e}", c.name)))?.unwrap_or_default())
}

/// The live registered user pools whose files are not in the soup.
pub fn missing(c: &env::Cauldron) -> Result<Vec<PoolGrammar>> {
    let soup = c.soup();
    Ok(registry(c)?.into_values().filter(|r| r.is_live() && !Pool::path_in(&soup, &r.grammar.pool).exists()).map(|r| r.grammar).collect())
}

/// What to say about a registered pool whose file is missing.
pub fn missing_note(c: &env::Cauldron, g: &PoolGrammar) -> String {
    let file = Pool::path_in(&c.soup(), &g.pool);
    format!(
        "user pool `{}` (`{}@{}`) is registered in cauldron `{}`'s soup, but its file `{}` is not there (moved out). Its records \
         cannot be read or written until it is back: put the file back (a copy older than the pool's last write is taken by \
         `cauldron soup grammar rebind {}`). Its kind, types and id prefixes stay registered meanwhile, so no other pool mints \
         its ids",
        g.pool,
        g.kind,
        g.version,
        c.name,
        file.display(),
        g.pool
    )
}

/// When `node_type` is a type of a registered user pool whose file is out,
/// the refusal that says so (reads by type, and writes, of its records).
pub fn refuse_missing_type(c: &env::Cauldron, node_type: &str) -> Result<()> {
    if let Some(g) = missing(c)?.into_iter().find(|g| g.node_types.contains_key(node_type)) {
        return Err(Error::new(Code::NotFound, format!("REFUSED: type `{node_type}` lives in a pool whose file is out: {}", missing_note(c, &g)))
            .with_detail(json!({"reason": "pool-file-missing", "pool": g.pool.as_str(), "type": node_type})));
    }
    Ok(())
}

/// The user grammars among [`pins`]./// The user grammars among [`pins`].
pub fn user_pins(c: &env::Cauldron) -> Result<Vec<PoolGrammar>> {
    Ok(pins(c)?.into_iter().filter(|g| ug::is_user_kind(g.kind.as_str())).collect())
}

/// The pool a record of `node_type` goes to when it is a user type of this
/// cauldron (`None`: a built-in type, or no such type).
pub fn pool_of_type(c: &env::Cauldron, node_type: &str) -> Result<Option<PoolName>> {
    if is_built_in_type(node_type) {
        return Ok(None);
    }
    Ok(user_pins(c)?.into_iter().find(|g| g.node_types.contains_key(node_type)).map(|g| g.pool))
}

/// **The routes a record write uses**, from the handle's pins: `cluster` to
/// the cluster pool, every type of a user grammar to its pool, and every
/// other type to `ws` (the batch's home).
pub fn routes(handle: &spine::Cauldron) -> BTreeMap<String, PoolName> {
    let mut r = crate::clusters::routes();
    for pool in handle.pools() {
        let Some(g) = handle.grammar(pool) else { continue };
        if ug::is_user_kind(g.kind.as_str()) {
            for t in g.node_types.keys() {
                // Never a built-in type's route (the open refuses such a
                // pin; this holds even without it).
                if !is_built_in_type(t) {
                    r.insert(t.clone(), pool.clone());
                }
            }
        }
    }
    r
}

/// Is `t` a type of a built-in grammar of this release?
pub fn is_built_in_type(t: &str) -> bool {
    ug::built_ins().iter().any(|g| g.node_types.contains_key(t))
}

/// The grammar a record of `node_type` is judged by, through `handle`: a
/// user type's pool's pin, else the built-in one
/// ([`crate::clusters::grammar_of_type`]).
pub fn grammar_of_type(handle: &spine::Cauldron, node_type: &str) -> Option<PoolGrammar> {
    if is_built_in_type(node_type) {
        return crate::clusters::grammar_of_type(node_type);
    }
    for pool in handle.pools() {
        if let Some(g) = handle.grammar(pool) {
            if ug::is_user_kind(g.kind.as_str()) && g.node_types.contains_key(node_type) {
                return Some(g.clone());
            }
        }
    }
    crate::clusters::grammar_of_type(node_type)
}

/// One pool, as listed.
pub fn listed(g: &PoolGrammar, nodes: Option<usize>) -> Value {
    let mut v = json!({
        "pool": g.pool.as_str(),
        "kind": g.kind.as_str(),
        "version": g.version,
        "schema": format!("{}@{}", g.kind, g.version),
        "origin": ug::origin(g),
        "types": g.node_types.keys().collect::<Vec<_>>(),
        "prefixes": g.node_types.iter().filter_map(|(t, d)| d.id_prefix.as_ref().map(|p| (t.clone(), json!(p)))).collect::<serde_json::Map<_, _>>(),
    });
    if ug::is_user_kind(g.kind.as_str()) {
        v["digest"] = json!(ug::digest(g));
    }
    if let Some(n) = nodes {
        v["nodes"] = json!(n);
    }
    v
}

fn node_count(c: &env::Cauldron, pool: &PoolName) -> Option<usize> {
    let p = Pool::open_read_only(&Pool::path_in(&c.soup(), pool)).ok()?;
    let snap = p.snapshot().ok()?;
    let mut n = 0;
    for t in p.grammar().node_types.keys() {
        n += snap.nodes_of(t, None).ok()?.len();
    }
    Some(n)
}

/// **The pools of a cauldron**, built-in and user, with their node counts.
/// A pool file that cannot be read at all is listed with `unreadable` (its
/// error) rather than failing the list; one the reads refuse
/// ([`pins`]) is listed with its `problems` and the whole list carries
/// `refused`, the sentence every read and write of the soup answers.
pub fn list(t: &Target) -> Result<Value> {
    let c = &t.cauldron;
    let soup = c.soup();
    let mut pools = Vec::new();
    let mut found = Vec::new();
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&soup).map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
    files.sort();
    for f in files {
        if f.extension().and_then(|e| e.to_str()) != Some("db") || f.file_name().and_then(|n| n.to_str()) == Some(spine::spine_db::SPINE_FILE) {
            continue;
        }
        match Pool::open_read_only(&f) {
            Ok(p) => found.push(p.grammar().clone()),
            Err(e) => pools.push(json!({"file": f.file_name().and_then(|n| n.to_str()), "unreadable": e.to_string(),
                "way_out": ug::way_out(&f)})),
        }
    }
    let reg = registry(c)?;
    let spine_conn = spine::offline::read_only_connection(&soup.join(spine::spine_db::SPINE_FILE)).ok();
    for g in &found {
        let mut v = listed(g, node_count(c, &g.pool));
        if let Ok(Some(why)) = ug::refusal(&soup, &[g]) {
            v["problems"] = json!([{"rule": "refused", "at": "", "detail": why}]);
        }
        // An id `rebind` recorded as lost that the pool holds again: an older
        // release (0.1.19 and before) knows no such list, and its `soup import
        // --pool` can write one. Named, so the owner knows that id now names
        // a new record (and an edge listed `dangling` then reaches it).
        if let Some(conn) = &spine_conn {
            let lost = spine::user_registry::lost_ids(conn, g.pool.as_str()).unwrap_or_default();
            if !lost.is_empty() {
                if let Ok(p) = Pool::open_read_only(&Pool::path_in(&soup, &g.pool)) {
                    if let Ok(snap) = p.snapshot() {
                        let again: Vec<&String> = lost.iter().filter(|id| snap.node(&spine::pool::NodeId((*id).clone())).ok().flatten().is_some()).collect();
                        if !again.is_empty() {
                            v["lost_ids_written_again"] = json!(again);
                            v["lost_ids_note"] = json!("ids `rebind` recorded as lost that the pool holds again: an older release's `soup import --pool` does not know the list and wrote them; each now names a new record");
                        }
                    }
                }
            }
        }
        pools.push(v);
    }
    for r in reg.values().filter(|r| r.is_live() && !found.iter().any(|g| g.pool.as_str() == r.pool)) {
        let mut v = listed(&r.grammar, None);
        v["missing"] = json!(true);
        v["way_out"] = json!(missing_note(c, &r.grammar));
        pools.push(v);
    }
    let mut doc = json!({"env": t.env.name, "cauldron": c.name, "pools": pools});
    let removed: Vec<&str> = reg.values().filter(|r| !r.is_live()).map(|r| r.pool.as_str()).collect();
    if !removed.is_empty() {
        doc["removed"] = json!(removed);
    }
    if let Err(e) = pins(c) {
        doc["refused"] = json!(e.message);
    }
    Ok(doc)
}

/// One pool's grammar as a grammar file.
pub fn show(t: &Target, pool: &str) -> Result<Value> {
    let c = &t.cauldron;
    let Some(g) = pins_as_found(c)?.into_iter().find(|g| g.pool.as_str() == pool) else {
        refuse!(Code::NotFound, "cauldron `{}` has no pool `{pool}` (`cauldron soup grammar list`)", c.name);
    };
    let mut doc = json!({"env": t.env.name, "cauldron": c.name, "pool": pool, "origin": ug::origin(&g), "grammar": ug::to_file(&g)});
    if ug::is_user_kind(g.kind.as_str()) {
        doc["digest"] = json!(ug::digest(&g));
    }
    Ok(doc)
}

/// What a check answers: `ok`, the grammar's identity, and every problem.
fn verdict(u: Option<&ug::UserGrammar>, problems: &[Problem], against: Option<&str>) -> Value {
    let mut v = json!({"ok": problems.is_empty(), "problems": problems});
    if let Some(u) = u {
        let g = &u.grammar;
        v["kind"] = json!(g.kind.as_str());
        v["version"] = json!(g.version);
        v["schema"] = json!(format!("{}@{}", g.kind, g.version));
        v["pool"] = json!(g.pool.as_str());
        v["types"] = json!(g.node_types.keys().collect::<Vec<_>>());
        v["digest"] = json!(ug::digest(g));
    }
    v["judged_against"] = json!(against.unwrap_or("this release's built-in grammars only (name a cauldron to judge against its pools too)"));
    v
}

/// **Judge a grammar file**, read only: on its own, and against the pools of
/// `t`'s cauldron when one is named.
pub fn check(t: Option<&Target>, bytes: &[u8], pool: Option<&str>) -> Result<Value> {
    let u = match ug::parse_file(bytes, pool) {
        Ok(u) => u,
        Err(ps) => return Ok(verdict(None, &ps, None)),
    };
    let (pins, against) = match t {
        Some(t) => (pins(&t.cauldron)?, Some(format!("the pools of cauldron `{}` (environment `{}`)", t.cauldron.name, t.env.name))),
        None => (vec![], None),
    };
    let mut judged = pins.clone();
    if let Some(t) = t {
        judged.extend(missing(&t.cauldron)?);
    }
    let refs: Vec<&PoolGrammar> = judged.iter().collect();
    let mut ps = ug::check_user(&u, &refs);
    if let Some(t) = t {
        ps.extend(history(&t.cauldron, &u.grammar, &pins)?);
        ps.extend(ug::history_problems(&t.cauldron.soup(), &u.grammar).map_err(|e| store(format!("reading cauldron `{}`'s spine: {e}", t.cauldron.name)))?);
    }
    let mut v = verdict(Some(&u), &ps, against.as_deref());
    if let Some(t) = t {
        if let Some(same) = pins.iter().find(|g| g.pool == u.grammar.pool && **g == u.grammar) {
            v["already"] = json!(format!("cauldron `{}` already holds pool `{}` pinned to exactly this grammar", t.cauldron.name, same.pool));
        }
    }
    Ok(v)
}

/// The [`ug::POOL_HAS_HISTORY`] problem, when `g`'s pool is not in the soup
/// but its spine records writes to a pool of that name.
fn history(c: &env::Cauldron, g: &PoolGrammar, pins: &[PoolGrammar]) -> Result<Vec<Problem>> {
    if pins.iter().any(|p| p.pool == g.pool) {
        return Ok(vec![]);
    }
    let had = spine::offline::pool_has_history(&c.soup(), g.pool.as_str()).map_err(|e| store(format!("reading cauldron `{}`'s spine: {e}", c.name)))?;
    Ok(if had {
        vec![Problem {
            rule: ug::POOL_HAS_HISTORY,
            at: "pool".into(),
            detail: format!(
                "this soup's spine records writes to a pool `{}` whose file is no longer in the soup, so it is not made again as \
                 a new, empty pool (its ids would be minted again, and edges other pools hold into its old records would point \
                 at new ones). Put back the pool file this cauldron wrote: a copy older than its last write is taken by `cauldron soup grammar rebind {}`, which records the records it lacks as lost (after which, if it holds no record, it can be removed and added again)",
                g.pool,
                g.pool
            ),
        }]
    } else {
        vec![]
    })
}

/// **Add a user grammar to the cauldron as a new pool.** The caller (the
/// command line) has asked the owner-acts gate first; `by` is who acted.
/// Taken under the soup's writer lock, with no soup daemon running; refused
/// `Locked` when another writer, the `mcp` service included, holds the soup
/// (the command line then sends it to the service, which adds it through
/// its own handle: [`add_on`]). Answers the pool as listed and whether it
/// was added (`false`: already there, pinned to exactly this grammar).
pub fn add(t: &Target, bytes: &[u8], pool: Option<&str>, by: &str) -> Result<Value> {
    let (e, c) = (&t.env, &t.cauldron);
    if let Err(ps) = ug::parse_file(bytes, pool) {
        return Ok(refused(t, verdict(None, &ps, None)));
    }
    crate::lock::refuse_live_soup(e)?;
    let _lock = crate::lock::take_writer_lock(c, "soup grammar add")?;
    let mut handle = spine::Cauldron::open(&c.soup(), spine::config::SpineConfig::default())
        .map_err(|err| store(format!("opening cauldron `{}`'s soup: {err}", c.name)))?;
    add_on(&mut handle, t, bytes, pool, by)
}

/// **The add itself, through `handle`**: the command line's own handle
/// ([`add`]), or the `mcp` service's writer handle when the service holds
/// the soup (its `GRAMMAR` seam verb), so the pool and its routes are the
/// handle's at once and no grant is touched. The file is judged again here,
/// against the pools the handle holds.
pub fn add_on(handle: &mut spine::Cauldron, t: &Target, bytes: &[u8], pool: Option<&str>, by: &str) -> Result<Value> {
    let (e, c) = (&t.env, &t.cauldron);
    let u = match ug::parse_file(bytes, pool) {
        Ok(u) => u,
        Err(ps) => return Ok(refused(t, verdict(None, &ps, None))),
    };
    let owned: Vec<PoolGrammar> = handle.pools().filter_map(|p| handle.grammar(p)).cloned().collect();
    let judged = handle.held_and_registered().map_err(|e| store(format!("reading cauldron `{}`'s spine: {e}", c.name)))?;
    let refs: Vec<&PoolGrammar> = judged.iter().collect();
    let mut ps = ug::check_user(&u, &refs);
    ps.extend(history(c, &u.grammar, &owned)?);
    ps.extend(ug::history_problems(&c.soup(), &u.grammar).map_err(|e| store(format!("reading cauldron `{}`'s spine: {e}", c.name)))?);
    if !ps.is_empty() {
        let against = format!("the pools of cauldron `{}` (environment `{}`)", c.name, e.name);
        return Ok(refused(t, verdict(Some(&u), &ps, Some(&against))));
    }
    let added = handle
        .add_pool(&u.grammar)
        .map_err(|err| store(format!("adding pool `{}` to cauldron `{}`'s soup: {err}", u.grammar.pool, c.name)))?;
    if added {
        env::private::tighten_tree(&c.soup())
            .map_err(|err| Error::caused(Code::Store, format!("making cauldron `{}`'s soup private to its owner", c.name), &err))?;
    }
    Ok(json!({
        "outcome": if added { "added" } else { "already-added" },
        "by": by,
        "env": e.name,
        "cauldron": c.name,
        "pool": listed(&u.grammar, None),
        "note": format!(
            "records of {} now go to pool `{}` by type, from `cauldron soup import` and from `soup_put`; a session's \
             `soup_get`, `soup_nodes` and `soup_search` find them by id and by type. A release before this one opens the \
             soup and reads its other pools as before, and refuses a record of these types as a type it does not know.",
            u.grammar.node_types.keys().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", "),
            u.grammar.pool
        ),
    }))
}

fn refused(t: &Target, mut verdict: Value) -> Value {
    verdict["outcome"] = json!("refused");
    verdict["env"] = json!(t.env.name);
    verdict["cauldron"] = json!(t.cauldron.name);
    verdict["note"] = json!("Nothing was written. Each problem names its rule and where in the file; `cauldron soup grammar check` judges a file without writing.");
    verdict
}

/// **`cauldron soup grammar remove <pool>`**, through `handle` (the
/// command line's, or the `mcp` service's writer handle): a user pool with
/// no record that nothing reaches, its file moved to `<soup>/.removed/`
/// (`spine::Cauldron::remove_user_pool`). `apply` false judges only.
pub fn remove_on(handle: &mut spine::Cauldron, t: &Target, pool: &str, apply: bool, by: &str) -> Result<Value> {
    let c = &t.cauldron;
    let mut v = handle
        .remove_user_pool(&PoolName::new(pool), apply)
        .map_err(|e| Error::new(if matches!(e, spine::error::SpineError::NotFound { .. }) { Code::NotFound } else { Code::Usage }, format!("REFUSED: {e}. Nothing was removed")))?;
    v["env"] = json!(t.env.name);
    v["cauldron"] = json!(c.name);
    v["by"] = json!(by);
    if !apply {
        v["note"] = json!("Nothing was moved: run it again with --yes to remove the pool.");
    }
    Ok(v)
}

/// [`remove_on`] on a handle of its own: under the writer lock, with no
/// soup daemon (refused `Locked` when the `mcp` service holds the soup).
pub fn remove(t: &Target, pool: &str, apply: bool, by: &str) -> Result<Value> {
    let c = &t.cauldron;
    crate::lock::refuse_live_soup(&t.env)?;
    let _lock = crate::lock::take_writer_lock(c, "soup grammar remove")?;
    let mut handle = spine::Cauldron::open(&c.soup(), spine::config::SpineConfig::default())
        .map_err(|err| store(format!("opening cauldron `{}`'s soup: {err}", c.name)))?;
    remove_on(&mut handle, t, pool, apply, by)
}

/// **`cauldron soup grammar rebind <pool>`**: take the pool file now in the
/// soup as the pool's own when it is safe to (`spine::user_registry::rebind`:
/// the registered grammar, sound, nothing foreign, nothing in flight),
/// accepting as lost the writes an older copy lacks. Under the writer lock,
/// with no soup daemon and no `mcp` service holding the soup (which opened
/// it before the copy was put back). `apply` false judges only.
pub fn rebind(t: &Target, pool: &str, apply: bool, by: &str) -> Result<Value> {
    let c = &t.cauldron;
    crate::lock::refuse_live_soup(&t.env)?;
    let _lock = crate::lock::take_writer_lock(c, "soup grammar rebind")?;
    let mut v = spine::user_registry::rebind(&c.soup(), pool, apply, by).map_err(|e| {
        Error::new(if matches!(e, spine::error::SpineError::NotFound { .. }) { Code::NotFound } else { Code::Usage }, format!("REFUSED: {e}. Nothing was written"))
    })?;
    v["env"] = json!(t.env.name);
    v["cauldron"] = json!(c.name);
    v["by"] = json!(by);
    if !apply {
        v["note"] = json!(
            "Nothing was written. With --yes, the writes above are recorded as lost (their records are gone from the soup, \
             their ids are never minted or written again, and the edges listed in `dangling` point at nothing), and the pool \
             file is taken as the pool's own."
        );
    }
    Ok(v)
}

/// **Before a record write, through `handle`**: a record of a type of a
/// registered user pool whose file is out is refused by name (its pool
/// cannot be written), and so is a record whose explicit id (no remint) is
/// one a `rebind` recorded as lost (a lost id is never written again).
pub fn refuse_missing_or_lost(handle: &spine::Cauldron, c: &env::Cauldron, records: &[Value], remint: bool) -> Result<()> {
    let missing = handle.missing_user_pools().map_err(|e| store(format!("reading cauldron `{}`'s spine: {e}", c.name)))?;
    for r in records {
        let Some(ty) = r.get("type").and_then(Value::as_str) else { continue };
        if let Some(g) = missing.iter().find(|g| g.node_types.contains_key(ty)) {
            return Err(Error::new(Code::NotFound, format!("REFUSED: a record of type `{ty}`, whose pool's file is out: {}. Nothing was written", missing_note(c, g)))
                .with_detail(json!({"reason": "pool-file-missing", "pool": g.pool.as_str(), "type": ty})));
        }
    }
    if remint {
        return Ok(());
    }
    let reg = registry(c)?;
    if reg.is_empty() {
        return Ok(());
    }
    let spine_file = c.soup().join(spine::spine_db::SPINE_FILE);
    let conn = spine::offline::read_only_connection(&spine_file).map_err(|e| store(format!("reading cauldron `{}`'s spine: {e}", c.name)))?;
    for row in reg.values() {
        let lost = spine::user_registry::lost_ids(&conn, &row.pool).map_err(|e| store(e.to_string()))?;
        if lost.is_empty() {
            continue;
        }
        for r in records {
            let (Some(ty), Some(id)) = (r.get("type").and_then(Value::as_str), r.get("id").and_then(Value::as_str)) else { continue };
            if row.grammar.node_types.contains_key(ty) && lost.contains(id) {
                return Err(Error::new(
                    Code::Usage,
                    format!(
                        "REFUSED: `{id}` was a record of user pool `{}` lost when an older copy of its file was taken \
                         (`grammar rebind`); this release never writes a lost id again. Give the record a label and remint. Nothing was written",
                        row.pool
                    ),
                )
                .with_detail(json!({"reason": "id-lost", "pool": row.pool, "id": id})));
            }
        }
    }
    Ok(())
}
