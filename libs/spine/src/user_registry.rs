//! **The spine's record of the user grammars a soup has registered**
//! (issue #28): which user pools exist, the exact grammar each was pinned
//! to, and what the owner has accepted about each.
//!
//! A pool file is the pool's own record of its grammar, but a file can be
//! edited, swapped, restored from an older backup or moved out. The spine is
//! the soup's one authority on what was written, so it also records, at
//! `add_pool`, each user pool's name, kind, version and digest. At every
//! open, a user pool file is judged against that row (`user_grammar::refusal`):
//!
//! - a file whose pin is not the registered grammar (any hand edit, a kind
//!   edited to a word with no hyphen included) is refused;
//! - a pool file the registry does not name, other than the built-in `ws`
//!   and `cauldron`, is refused (so no edit can move a file from the user
//!   rules to the built-in ones);
//! - a registered pool whose file is missing is NOT a refusal of the soup:
//!   the rest opens, the pool's types are refused by name, and its kind,
//!   types and prefixes stay taken ([`registered_grammars`]), so no other pool
//!   can mint its ids again;
//! - a part the spine landed in the pool that the owner accepted as lost
//!   (`cauldron soup grammar rebind`, after a file was restored from an older
//!   copy) is left out of the binding ([`forgiven`]), and the ids those parts
//!   minted are kept ([`lost_ids`]) so they are never written again.
//!
//! The tables are made only when a soup first registers a user grammar
//! (`CREATE TABLE IF NOT EXISTS`), so a soup with none is byte-for-byte what
//! it was. An older release ignores them: it opens the spine by its schema
//! version, which they do not move.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::Result;
use crate::schema_def::PoolGrammar;

/// The registry's tables.
pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS user_pools (
  pool       TEXT PRIMARY KEY,
  kind       TEXT NOT NULL,
  version    INTEGER NOT NULL,
  digest     TEXT NOT NULL,
  grammar    TEXT NOT NULL CHECK (json_valid(grammar)),
  state      TEXT NOT NULL CHECK (state IN ('live', 'removed')),
  added_at   TEXT NOT NULL,
  changed_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS user_pool_forgiven (
  pool    TEXT NOT NULL,
  tx_hash TEXT NOT NULL,
  part    INTEGER NOT NULL,
  at      TEXT NOT NULL,
  by      TEXT NOT NULL,
  PRIMARY KEY (pool, tx_hash, part)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS user_pool_lost_ids (
  pool TEXT NOT NULL,
  id   TEXT NOT NULL,
  at   TEXT NOT NULL,
  PRIMARY KEY (pool, id)
) WITHOUT ROWID;
"#;

/// One registered user pool.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub pool: String,
    pub kind: String,
    pub version: u32,
    pub digest: String,
    /// The form of `digest` (`user_grammar::DIGEST_INPUT_VERSION` when it was
    /// written); `None` for a row written before the form was recorded.
    pub digest_input: Option<u32>,
    pub grammar: PoolGrammar,
    /// `live` or `removed`.
    pub state: String,
}

impl Row {
    pub fn is_live(&self) -> bool {
        self.state == "live"
    }
}

fn now(conn: &Connection) -> Result<String> {
    Ok(conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| r.get(0))?)
}

fn has_digest_input(conn: &Connection) -> Result<bool> {
    let n: i64 = conn.query_row("SELECT count(*) FROM pragma_table_info('user_pools') WHERE name = 'digest_input'", [], |r| r.get(0))?;
    Ok(n > 0)
}

fn has_registry(conn: &Connection) -> Result<bool> {
    let n: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'user_pools'", [], |r| r.get(0))?;
    Ok(n > 0)
}

/// Every row, by pool name; `None` when the soup never registered a user
/// grammar (the tables do not exist).
pub fn read(conn: &Connection) -> Result<Option<BTreeMap<String, Row>>> {
    if !has_registry(conn)? {
        return Ok(None);
    }
    let mut out = BTreeMap::new();
    let sql = if has_digest_input(conn)? {
        "SELECT pool, kind, version, digest, grammar, state, digest_input FROM user_pools"
    } else {
        "SELECT pool, kind, version, digest, grammar, state, NULL FROM user_pools"
    };
    let mut q = conn.prepare(sql)?;
    let rows = q.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<i64>>(6)?,
        ))
    })?;
    for row in rows {
        let (pool, kind, version, digest, grammar, state, digest_input) = row?;
        let digest_input = digest_input.and_then(|v| u32::try_from(v).ok());
        let grammar: PoolGrammar = serde_json::from_str(&grammar).map_err(|e| {
            crate::error::SpineError::hard(format!(
                "the spine's record of user pool `{pool}` (table `user_pools` in `spine.db`) will not parse: {e}. It was \
                 edited by hand; restore the whole soup directory (spine and every pool) from one backup"
            ))
        })?;
        out.insert(pool.clone(), Row { pool, kind, version: u32::try_from(version).unwrap_or(0), digest, digest_input, grammar, state });
    }
    Ok(Some(out))
}

/// Record `g` as a live user pool (made, or made again after a removal).
pub fn register(conn: &Connection, g: &PoolGrammar) -> Result<()> {
    conn.execute_batch(DDL)?;
    ensure_digest_input(conn)?;
    let at = now(conn)?;
    conn.execute(
        "INSERT INTO user_pools (pool, kind, version, digest, grammar, state, added_at, changed_at, digest_input)
         VALUES (?1, ?2, ?3, ?4, ?5, 'live', ?6, ?6, ?7)
         ON CONFLICT (pool) DO UPDATE SET kind = excluded.kind, version = excluded.version, digest = excluded.digest,
           grammar = excluded.grammar, state = 'live', changed_at = excluded.changed_at, digest_input = excluded.digest_input",
        params![
            g.pool.as_str(),
            g.kind.as_str(),
            g.version,
            crate::user_grammar::digest(g),
            serde_json::to_string(g).map_err(|e| crate::error::SpineError::hard(e.to_string()))?,
            at,
            crate::user_grammar::DIGEST_INPUT_VERSION
        ],
    )?;
    Ok(())
}

/// The `digest_input` column, added to a registry made before it (nullable:
/// a row without it records no form, and is judged by every form this
/// release knows).
fn ensure_digest_input(conn: &Connection) -> Result<()> {
    if !has_digest_input(conn)? {
        conn.execute_batch("ALTER TABLE user_pools ADD COLUMN digest_input INTEGER")?;
    }
    Ok(())
}

/// How a pool file's pin stands against its registry row.
#[derive(Debug, Clone, PartialEq)]
pub enum PinMatch {
    /// The row's digest is the pin's digest in this release's form.
    Current,
    /// The pin is the registered grammar, but the row's digest is of an
    /// earlier form (or none is recorded): it is migrated on the next open
    /// for writing ([`migrate_digests`]).
    EarlierForm,
    /// The row was written by a release whose digest form this one does not
    /// know: it cannot judge the pin.
    NewerForm(u32),
    /// The file's pin is not the grammar recorded when the pool was added;
    /// what differs, in words.
    Differs(String),
}

/// **Judge a pool file's pin against its registry row**, by every digest
/// form this release knows ([`crate::user_grammar::digest_forms`]) and, for a
/// row whose form is earlier or unknown, by the grammar the row itself
/// records. A digest form that changes is therefore a migration, never a
/// refusal.
pub fn pin_matches(row: &Row, pin: &PoolGrammar) -> PinMatch {
    let current = crate::user_grammar::DIGEST_INPUT_VERSION;
    if let Some(v) = row.digest_input.filter(|v| *v > current) {
        return PinMatch::NewerForm(v);
    }
    if row.digest == crate::user_grammar::digest(pin) && row.digest_input == Some(current) {
        return PinMatch::Current;
    }
    let earlier = crate::user_grammar::digest_forms().into_iter().any(|(_, f)| f(pin) == row.digest);
    let mut recorded = row.grammar.clone();
    recorded.pool = pin.pool.clone();
    if earlier || recorded == *pin {
        return PinMatch::EarlierForm;
    }
    PinMatch::Differs(crate::user_grammar::what_differs(&row.grammar, pin))
}

/// Rewrite every live row whose pin matched in an earlier digest form to
/// this release's form (called by an open for writing, after the pins were
/// judged). Answers the pools migrated.
pub fn migrate_digests(conn: &Connection, pins: &[&PoolGrammar]) -> Result<Vec<String>> {
    let Some(reg) = read(conn)? else { return Ok(vec![]) };
    let mut done = Vec::new();
    for g in pins {
        let Some(row) = reg.get(g.pool.as_str()).filter(|r| r.is_live()) else { continue };
        if pin_matches(row, g) == PinMatch::EarlierForm {
            ensure_digest_input(conn)?;
            conn.execute(
                "UPDATE user_pools SET digest = ?2, digest_input = ?3, changed_at = ?4 WHERE pool = ?1",
                params![row.pool, crate::user_grammar::digest(g), crate::user_grammar::DIGEST_INPUT_VERSION, now(conn)?],
            )?;
            done.push(row.pool.clone());
        }
    }
    Ok(done)
}

/// Mark `pool` removed (its row stays: the name's history).
pub fn mark_removed(conn: &Connection, pool: &str) -> Result<()> {
    conn.execute("UPDATE user_pools SET state = 'removed', changed_at = ?2 WHERE pool = ?1", params![pool, now(conn)?])?;
    Ok(())
}

/// The parts of `pool` the owner accepted as lost.
pub fn forgiven(conn: &Connection, pool: &str) -> Result<BTreeSet<(String, i64)>> {
    if !has_registry(conn)? {
        return Ok(BTreeSet::new());
    }
    let mut q = conn.prepare("SELECT tx_hash, part FROM user_pool_forgiven WHERE pool = ?1")?;
    let rows = q.query_map([pool], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = BTreeSet::new();
    for r in rows {
        out.insert(r?);
    }
    Ok(out)
}

/// Accept `parts` of `pool` as lost, and keep the ids they minted.
pub fn forgive(conn: &mut Connection, pool: &str, parts: &[(String, i64)], ids: &[String], by: &str) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(DDL)?;
    let at = now(&tx)?;
    for (h, p) in parts {
        tx.execute(
            "INSERT OR IGNORE INTO user_pool_forgiven (pool, tx_hash, part, at, by) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![pool, h, p, at, by],
        )?;
    }
    for id in ids {
        tx.execute("INSERT OR IGNORE INTO user_pool_lost_ids (pool, id, at) VALUES (?1, ?2, ?3)", params![pool, id, at])?;
    }
    tx.commit()?;
    Ok(())
}

/// The ids of `pool`'s records lost with forgiven parts (never written again).
pub fn lost_ids(conn: &Connection, pool: &str) -> Result<BTreeSet<String>> {
    if !has_registry(conn)? {
        return Ok(BTreeSet::new());
    }
    let mut q = conn.prepare("SELECT id FROM user_pool_lost_ids WHERE pool = ?1")?;
    let rows = q.query_map([pool], |r| r.get::<_, String>(0))?;
    let mut out = BTreeSet::new();
    for r in rows {
        out.insert(r?);
    }
    Ok(out)
}

/// The grammars of every LIVE registered pool, whether or not its file is
/// in the soup: what a new grammar is judged against besides the files
/// (kinds, types and prefixes stay taken while a pool's file is out).
pub fn registered_grammars(conn: &Connection) -> Result<Vec<PoolGrammar>> {
    Ok(read(conn)?.unwrap_or_default().into_values().filter(Row::is_live).map(|r| r.grammar).collect())
}

/// Is `prefix` a prefix the spine has minted or carried ids under, in a pool
/// other than `pool`? (Such a prefix is never taken by another pool: its
/// ids would be minted again from 1.)
pub fn prefix_minted_elsewhere(conn: &Connection, prefix: &str, pool: &str) -> Result<Option<String>> {
    let has: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'id_serials'", [], |r| r.get(0))?;
    if has == 0 {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT pool FROM id_serials WHERE prefix = ?1 AND pool != ?2 AND last > 0 ORDER BY pool LIMIT 1",
            params![prefix, pool],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

/// How many records a pool file holds (every type of its pin).
pub fn record_count(file: &std::path::Path) -> Result<usize> {
    let p = crate::pool::Pool::open_read_only(file)?;
    let snap = p.snapshot()?;
    let mut n = 0;
    for t in p.grammar().node_types.keys() {
        n += snap.nodes_of(t, None)?.len();
    }
    Ok(n)
}

/// The ids the parts `lost` created in `pool`, read from the spine's own
/// record of each write (the payload a decided row keeps carries the
/// serial ids).
fn ids_created(spine: &Connection, pool: &str, lost: &[(String, i64, i64, String)]) -> Result<Vec<String>> {
    Ok(creations(spine, pool, lost)?.into_iter().map(|(id, _)| id).collect())
}

/// The ids the parts `parts` created in `pool`, each with the hash of the
/// transaction that created it.
fn creations(spine: &Connection, pool: &str, parts: &[(String, i64, i64, String)]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for (hash, part, seq, _) in parts {
        let payload: String = spine.query_row("SELECT payload FROM transactions WHERE seq = ?1", [seq], |r| r.get(0))?;
        let Ok(p) = serde_json::from_str::<crate::delta::CompiledPayload>(&payload) else { continue };
        let Some(part) = usize::try_from(*part).ok().and_then(|i| p.parts.get(i)) else { continue };
        if part.pool.as_str() != pool {
            continue;
        }
        for d in &part.deltas {
            if let crate::delta::Delta::CreateNode { id, .. } = d {
                out.push((id.0.clone(), hash.clone()));
            }
        }
    }
    Ok(out)
}

/// **`cauldron soup grammar rebind <pool>`**: take the user pool file now
/// at `<dir>/<pool>.db` as the pool's own, when it is safe to.
///
/// Refused, writing nothing, unless every one of these holds:
/// - the file opens as a pool, passes SQLite's `quick_check`, and its pin is
///   the grammar the spine registered for the pool (same digest); for a pool
///   the spine never registered (made by an earlier build), its pin is a
///   user grammar `grammar add` would take against the other pools now;
/// - every part the file holds is one this soup's spine wrote to the pool
///   (nothing from another cauldron);
/// - no transaction touching the pool is still in flight.
///
/// What it then accepts: the parts the spine landed in the pool that the
/// file lacks (an older copy: a backup taken before them). The answer names
/// each (`seq`, `ts`, `tx`), the ids of the records they created
/// (`lost_ids`: never minted or written again by this release), and the
/// edges other pools hold into those records (`dangling`). With `apply`
/// (`--yes`) they are recorded (`user_pool_forgiven`, `user_pool_lost_ids`)
/// and the pool is registered if it was not; without it, nothing is written.
/// The caller holds the soup's writer lock.
pub fn rebind(dir: &std::path::Path, pool: &str, apply: bool, by: &str) -> Result<serde_json::Value> {
    use crate::error::SpineError;
    use serde_json::json;
    let file = dir.join(format!("{pool}.db"));
    if !file.is_file() {
        return Err(SpineError::not_found(format!("there is no pool file `{}`: put the copy to take in place first", file.display())));
    }
    let g = crate::pool::Pool::open_read_only(&file)
        .map_err(|e| SpineError::usage(format!("pool file `{}` cannot be read as a pool ({e}); rebind takes only a sound copy", file.display())))?
        .grammar()
        .clone();
    if g.pool.as_str() != pool {
        return Err(SpineError::usage(format!("pool file `{}` holds pool `{}`, not `{pool}`", file.display(), g.pool)));
    }
    let check: String = crate::offline::read_only_connection(&file)?.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if check != "ok" {
        return Err(SpineError::usage(format!("pool file `{}` fails SQLite's quick_check ({check}); rebind takes only a sound copy", file.display())));
    }
    let spine = crate::offline::read_only_connection(&dir.join(crate::spine_db::SPINE_FILE))?;
    let reg = read(&spine)?.unwrap_or_default();
    let adopt = match reg.get(pool) {
        Some(r) if !r.is_live() => {
            return Err(SpineError::usage(format!("user pool `{pool}` was removed; its file is not taken back (`cauldron soup grammar add` makes it again, empty)")))
        }
        Some(r) if matches!(pin_matches(r, &g), PinMatch::Differs(_) | PinMatch::NewerForm(_)) => {
            let why = match pin_matches(r, &g) {
                PinMatch::Differs(d) => d,
                PinMatch::NewerForm(v) => format!("its record was written by a newer release (digest form {v}), which this one cannot judge"),
                _ => String::new(),
            };
            return Err(SpineError::usage(format!(
                "pool file `{}` holds a pin that is not the grammar this soup registered for `{pool}` ({why}): rebind takes only \
                 a copy of the pool's own file",
                file.display()
            )))
        }
        Some(_) => false,
        None => {
            if !crate::user_grammar::is_user_kind(g.kind.as_str()) {
                return Err(SpineError::usage(format!("pool file `{}` does not hold a user grammar (`{}@{}`)", file.display(), g.kind, g.version)));
            }
            let mut others: Vec<PoolGrammar> = registered_grammars(&spine)?.into_iter().filter(|x| x.pool != g.pool).collect();
            for f in std::fs::read_dir(dir).map_err(|e| SpineError::hard(e.to_string()))?.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("db") || path == file || path.file_name().and_then(|n| n.to_str()) == Some(crate::spine_db::SPINE_FILE) {
                    continue;
                }
                if let Ok(p) = crate::pool::Pool::open_read_only(&path) {
                    if !others.iter().any(|x| x.pool == p.grammar().pool) {
                        others.push(p.grammar().clone());
                    }
                }
            }
            let refs: Vec<&PoolGrammar> = others.iter().collect();
            let mut ps = crate::user_grammar::check_user(&crate::user_grammar::UserGrammar::from_pin(&g), &refs);
            ps.extend(crate::user_grammar::history_problems(dir, &g)?);
            if let Some(p) = ps.first() {
                return Err(SpineError::usage(format!(
                    "pool file `{}` holds a grammar `grammar add` would refuse here ({} at `{}`: {}); it is not registered",
                    file.display(),
                    p.rule,
                    p.at,
                    p.detail
                )));
            }
            true
        }
    };
    let b = crate::offline::binding(dir, pool)?;
    if !b.foreign.is_empty() {
        return Err(SpineError::usage(format!("pool file `{}`: {}", file.display(), b.why(pool).unwrap_or_default())));
    }
    let in_flight: i64 = spine.query_row(
        "SELECT count(*) FROM transactions t JOIN tx_parts p ON p.seq = t.seq WHERE p.pool = ?1 AND t.cleared = 0",
        [pool],
        |r| r.get(0),
    )?;
    if in_flight > 0 {
        return Err(SpineError::usage(format!("{in_flight} write(s) touching pool `{pool}` are still in flight; run `cauldron soup recover` first")));
    }
    // Every record the file holds must be one this soup's spine created in
    // the pool: a file whose part log was emptied by hand still holds
    // another cauldron's records, and taking it would make their ids ours
    // (and every later mint of them a duplicate).
    let mut written: Vec<(String, i64, i64, String)> = Vec::new();
    {
        let mut q = spine.prepare(
            "SELECT t.hash, p.part, t.seq, t.ts FROM tx_parts p JOIN transactions t ON t.seq = p.seq WHERE p.pool = ?1 AND t.decided = 1",
        )?;
        let rows = q.query_map([pool], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?)))?;
        for r in rows {
            written.push(r?);
        }
    }
    // ...and each one the very write this spine recorded creating it: an id
    // alone is not enough, since another cauldron mints the same `BK-1`.
    let created: BTreeMap<String, String> = creations(&spine, pool, &written)?.into_iter().collect();
    let held: Vec<(String, String)> = {
        let c = crate::offline::read_only_connection(&file)?;
        let mut q = c.prepare("SELECT id, created_tx FROM nodes ORDER BY id")?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    if let Some((id, _)) = held.iter().find(|(id, tx)| created.get(id).is_some_and(|ours| ours != tx)) {
        return Err(SpineError::usage(format!(
            "pool file `{}` holds record `{id}`, but not the write this soup's spine recorded creating `{id}` in pool `{pool}` \
             (another transaction created the file's): it is another cauldron's file, whatever its write log says. Put back \
             this cauldron's own pool file, or restore the whole soup directory from one backup",
            file.display()
        )));
    }
    let strangers: Vec<&String> = held.iter().map(|(id, _)| id).filter(|id| !created.contains_key(*id)).collect();
    if let Some(first) = strangers.first() {
        return Err(SpineError::usage(format!(
            "pool file `{}` holds {} record(s) this soup's spine never created in pool `{pool}` (the first `{first}`): it is not \
             a copy of this pool's file, whatever its write log says. Put back this cauldron's own pool file, or restore the \
             whole soup directory from one backup",
            file.display(),
            strangers.len()
        )));
    }
    let lost_ids = ids_created(&spine, pool, &b.lost)?;
    // Edges other pools hold into the records the copy lacks.
    let mut dangling = Vec::new();
    if !lost_ids.is_empty() {
        for f in std::fs::read_dir(dir).map_err(|e| SpineError::hard(e.to_string()))?.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("db") || path == file || path.file_name().and_then(|n| n.to_str()) == Some(crate::spine_db::SPINE_FILE) {
                continue;
            }
            let Ok(c) = crate::offline::read_only_connection(&path) else { continue };
            let src_pool = path.file_stem().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let Ok(mut q) = c.prepare("SELECT src, kind, dst FROM edges WHERE dst_pool = ?1") else { continue };
            let rows = q.query_map([pool], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
            for row in rows {
                let (src, kind, dst) = row?;
                if lost_ids.contains(&dst) {
                    dangling.push(json!({"pool": src_pool, "from": src, "kind": kind, "to": format!("{dst}@{pool}")}));
                }
            }
        }
    }
    let writes: Vec<_> = b.lost.iter().map(|(h, p, seq, ts)| json!({"seq": seq, "part": p, "ts": ts, "tx": h})).collect();
    let mut doc = json!({
        "pool": pool,
        "schema": format!("{}@{}", g.kind, g.version),
        "digest": crate::user_grammar::digest(&g),
        "registered_before": !adopt,
        "lost_writes": writes,
        "lost_ids": lost_ids,
        "dangling": dangling,
    });
    if !apply {
        doc["outcome"] = json!(if b.lost.is_empty() && !adopt { "nothing-to-do" } else { "would-rebind" });
        return Ok(doc);
    }
    drop(spine);
    let mut rw = Connection::open(dir.join(crate::spine_db::SPINE_FILE))?;
    rw.busy_timeout(std::time::Duration::from_secs(5))?;
    if adopt {
        register(&rw, &g)?;
    }
    let parts: Vec<(String, i64)> = b.lost.iter().map(|(h, p, _, _)| (h.clone(), *p)).collect();
    forgive(&mut rw, pool, &parts, &lost_ids, by)?;
    doc["outcome"] = json!(if b.lost.is_empty() && !adopt { "nothing-to-do" } else { "rebound" });
    Ok(doc)
}
