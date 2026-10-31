//! SQLite persistence (WAL): keys, renders, memberships, locks, counters.
//!
//! One `Connection` behind a mutex, WAL and a busy timeout on open, and the
//! schema applied on every open. The substrate crate also carried an empty
//! `MIGRATIONS` array and a rebuild of `cache_keys` for stores older than
//! count mode; the array never held anything and production's store was
//! rebuilt long ago, so both are gone and an older store is now REFUSED by
//! name at open instead of being silently half-usable.
//!
//! # One invariant every deletion here keeps
//!
//! **A membership never outlives its render.** `key_stats` and
//! `lru_candidates` both JOIN `renders`, so an `entries` row whose render is
//! gone is in no count, is never an eviction candidate, and lives forever —
//! while still poisoning `MIN(last_used_at)`, the one aggregate that does not
//! join. Production carried 1,923 such rows (48% of `content`) before this was
//! enforced. So every function that deletes renders deletes the memberships
//! pointing at them in the same transaction.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::keys::KeyConfig;

const SCHEMA_SQL: &str = include_str!("../schema.sql");

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// A render: the bytes.
#[derive(Debug, Clone)]
pub struct RenderRow {
    pub hash: String,
    pub module: String,
    pub module_fingerprint: String,
    pub state: String,
    pub rel_path: Option<String>,
    pub bytes: u64,
    pub format: String,
    pub synth_ms: Option<u64>,
    pub fail_code: Option<String>,
    pub fail_reason: Option<String>,
    pub fail_count: u32,
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// A key's membership in a render.
#[derive(Debug, Clone)]
pub struct EntryRow {
    pub key_name: String,
    pub hash: String,
    pub registered_at: String,
    pub last_used_at: String,
    pub use_count: u64,
}

/// Per-key aggregate for `tts.status`. `bytes` counts warm renders only.
#[derive(Debug, Clone, Default)]
pub struct KeyStats {
    pub entries: u64,
    pub bytes: u64,
    pub warm: u64,
    pub queued: u64,
    pub synthesizing: u64,
    pub failed: u64,
    pub oldest_used_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Counters {
    pub synth_total: u64,
    pub evicted_total: u64,
    pub fail_total: u64,
    pub bytes_written: u64,
}

/// The four counters, by name. The SQL is chosen from this enum, so a
/// counter name never reaches a statement as text.
#[derive(Debug, Clone, Copy)]
pub enum Counter {
    SynthTotal,
    EvictedTotal,
    FailTotal,
    BytesWritten,
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref())?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Self::init(conn)
    }

    /// An in-memory store, for tests.
    pub fn open_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        let existing: Option<String> = conn
            .query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='cache_keys'", [], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(sql) = existing {
            if !sql.contains("lru_count") {
                return Err(Error::Store(
                    "this store predates count-mode keys: its `cache_keys` CHECK constraint \
                     refuses `lru_count`, so the pinned-count and active-count keys could never \
                     be projected into it. It was written by a substrate tts older than \
                     2026-09; rebuild the table (or start from an empty store) before pointing \
                     this daemon at it."
                        .into(),
                ));
            }
        }
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| Error::Internal("store mutex poisoned".into()))
    }

    /// Run `f` against the connection. For tests that need to look at, or
    /// deliberately damage, rows no public method exposes.
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        Ok(f(&*self.lock()?)?)
    }

    // ── cache keys ─────────────────────────────────────────────────────────

    /// Project the loaded keys (each with its module fingerprint) into
    /// `cache_keys`, returning the names that changed. A key the file no
    /// longer names is DISABLED, never deleted: deleting a gigabyte because
    /// six lines of TOML went away is the inelegance the operator named.
    pub fn project_keys(&self, keys: &[(KeyConfig, String)]) -> Result<Vec<String>> {
        type Row = (String, Option<i64>, Option<i64>, String, String, String, i64, i64, i64);
        let now = now_rfc3339();
        let c = self.lock()?;
        let mut changed = Vec::new();
        for (k, fingerprint) in keys {
            let cfg = k.module_config_json().to_string();
            let wanted: Row = (
                k.mode.as_str().into(),
                k.max_bytes.map(|b| b as i64),
                k.max_entries.map(|n| n as i64),
                k.queueing.as_str().into(),
                k.module.clone(),
                cfg.clone(),
                k.priority,
                k.min_age_secs,
                k.enabled as i64,
            );
            let prior: Option<Row> = c
                .query_row(
                    "SELECT mode, max_bytes, max_entries, queueing, module, module_config, priority, \
                     min_age_secs, enabled FROM cache_keys WHERE name = ?1",
                    params![k.name],
                    |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?))
                    },
                )
                .optional()?;
            if prior.as_ref() != Some(&wanted) {
                changed.push(k.name.clone());
            }
            c.execute(
                "INSERT INTO cache_keys (name, mode, max_bytes, max_entries, warn_bytes, refuse_bytes, \
                    queueing, module, module_config, module_fingerprint, priority, min_age_secs, \
                    enabled, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) \
                 ON CONFLICT(name) DO UPDATE SET mode=?2, max_bytes=?3, max_entries=?4, \
                    warn_bytes=?5, refuse_bytes=?6, queueing=?7, module=?8, module_config=?9, \
                    module_fingerprint=?10, priority=?11, min_age_secs=?12, enabled=?13, updated_at=?14",
                params![
                    k.name,
                    k.mode.as_str(),
                    k.max_bytes.map(|b| b as i64),
                    k.max_entries.map(|n| n as i64),
                    k.warn_bytes.map(|b| b as i64),
                    k.refuse_bytes.map(|b| b as i64),
                    k.queueing.as_str(),
                    k.module,
                    cfg,
                    fingerprint,
                    k.priority,
                    k.min_age_secs,
                    k.enabled as i64,
                    now,
                ],
            )?;
            c.execute("INSERT OR IGNORE INTO key_counters (key_name) VALUES (?1)", params![k.name])?;
        }
        let live: Vec<String> = c
            .prepare("SELECT name FROM cache_keys WHERE enabled = 1")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for name in live {
            if !keys.iter().any(|(k, _)| k.name == name) {
                c.execute("UPDATE cache_keys SET enabled = 0, updated_at = ?2 WHERE name = ?1", params![name, now])?;
                changed.push(name);
            }
        }
        Ok(changed)
    }

    /// Every projected key, as JSON: the `tts.keys` payload.
    pub fn list_key_rows(&self) -> Result<Vec<Value>> {
        let c = self.lock()?;
        let mut stmt = c.prepare(
            "SELECT name, mode, max_bytes, max_entries, warn_bytes, refuse_bytes, queueing, module, \
             module_config, module_fingerprint, priority, min_age_secs, enabled FROM cache_keys ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            let cfg: String = r.get(8)?;
            Ok(serde_json::json!({
                "key": r.get::<_, String>(0)?,
                "mode": r.get::<_, String>(1)?,
                "max_bytes": r.get::<_, Option<i64>>(2)?,
                "max_entries": r.get::<_, Option<i64>>(3)?,
                "warn_bytes": r.get::<_, Option<i64>>(4)?,
                "refuse_bytes": r.get::<_, Option<i64>>(5)?,
                "queueing": r.get::<_, String>(6)?,
                "module": r.get::<_, String>(7)?,
                "module_config": serde_json::from_str::<Value>(&cfg).unwrap_or(Value::Null),
                "module_fingerprint": r.get::<_, String>(9)?,
                "priority": r.get::<_, i64>(10)?,
                "min_age_secs": r.get::<_, i64>(11)?,
                "enabled": r.get::<_, i64>(12)? != 0,
            }))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ── renders ────────────────────────────────────────────────────────────

    pub fn get_render(&self, hash: &str) -> Result<Option<RenderRow>> {
        Ok(self
            .lock()?
            .query_row(
                "SELECT hash, module, module_fingerprint, state, rel_path, bytes, format, synth_ms, \
                 fail_code, fail_reason, fail_count, created_at, completed_at FROM renders WHERE hash = ?1",
                params![hash],
                |r| {
                    Ok(RenderRow {
                        hash: r.get(0)?,
                        module: r.get(1)?,
                        module_fingerprint: r.get(2)?,
                        state: r.get(3)?,
                        rel_path: r.get(4)?,
                        bytes: r.get::<_, i64>(5)? as u64,
                        format: r.get(6)?,
                        synth_ms: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
                        fail_code: r.get(8)?,
                        fail_reason: r.get(9)?,
                        fail_count: r.get::<_, i64>(10)? as u32,
                        created_at: r.get(11)?,
                        completed_at: r.get(12)?,
                    })
                },
            )
            .optional()?)
    }

    /// Put a render in `queued` if it is unknown, and REVIVE it if its last
    /// attempt failed; return the state that now holds.
    ///
    /// "Never let a sentence block outright, for a technical failure… if it's
    /// being requested again, then it needs to be fixed." Only a `failed` row
    /// is touched, so `warm` keeps its bytes and an in-flight `synthesizing`
    /// row is never pulled out from under its job. The `fail_*` columns are
    /// left as the record of the last attempt.
    pub fn ensure_render(&self, hash: &str, module: &str, fingerprint: &str, char_len: usize, format: &str) -> Result<String> {
        let c = self.lock()?;
        c.execute(
            "INSERT INTO renders (hash, module, module_fingerprint, state, char_len, format, created_at) \
             VALUES (?1,?2,?3,'queued',?4,?5,?6) \
             ON CONFLICT(hash) DO UPDATE SET state='queued', module=?2, module_fingerprint=?3, \
                char_len=?4, format=?5, completed_at=NULL WHERE renders.state = 'failed'",
            params![hash, module, fingerprint, char_len as i64, format, now_rfc3339()],
        )?;
        Ok(c.query_row("SELECT state FROM renders WHERE hash = ?1", params![hash], |r| r.get(0))?)
    }

    pub fn set_render_state(&self, hash: &str, state: &str) -> Result<()> {
        self.lock()?.execute("UPDATE renders SET state = ?2 WHERE hash = ?1", params![hash, state])?;
        Ok(())
    }

    pub fn mark_warm(&self, hash: &str, rel_path: &str, bytes: u64, format: &str, synth_ms: u64) -> Result<()> {
        self.lock()?.execute(
            "UPDATE renders SET state='warm', rel_path=?2, bytes=?3, format=?4, synth_ms=?5, \
             fail_code=NULL, fail_reason=NULL, completed_at=?6 WHERE hash = ?1",
            params![hash, rel_path, bytes as i64, format, synth_ms as i64, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Record that the LAST attempt failed. A diagnostic, never a gate:
    /// `fail_count` accumulates so a struggling text is visible without that
    /// fact ever silencing it.
    pub fn mark_failed(&self, hash: &str, code: &str, reason: &str) -> Result<()> {
        self.lock()?.execute(
            "UPDATE renders SET state='failed', fail_code=?2, fail_reason=?3, fail_count = fail_count + 1, \
             completed_at=?4 WHERE hash = ?1",
            params![hash, code, reason, now_rfc3339()],
        )?;
        Ok(())
    }

    /// The importer's insert: an existing file adopted as a warm render.
    #[allow(clippy::too_many_arguments)]
    pub fn adopt_render(
        &self,
        hash: &str,
        module: &str,
        fingerprint: &str,
        rel_path: &str,
        bytes: u64,
        format: &str,
        created_at: &str,
    ) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO renders (hash, module, module_fingerprint, state, rel_path, bytes, format, \
                created_at, completed_at) VALUES (?1,?2,?3,'warm',?4,?5,?6,?7,?7) \
             ON CONFLICT(hash) DO UPDATE SET state='warm', rel_path=?4, bytes=?5, format=?6",
            params![hash, module, fingerprint, rel_path, bytes as i64, format, created_at],
        )?;
        Ok(())
    }

    // ── memberships ────────────────────────────────────────────────────────

    /// Register a hash into a key. Idempotent, and also a touch: registering
    /// is a statement of intent to use.
    pub fn register_entry(&self, key: &str, hash: &str) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO entries (key_name, hash, registered_at, last_used_at, use_count) VALUES (?1,?2,?3,?3,0) \
             ON CONFLICT(key_name, hash) DO UPDATE SET last_used_at = ?3",
            params![key, hash, now_rfc3339()],
        )?;
        Ok(())
    }

    /// The importer's variant: recency seeded from the source file's mtime,
    /// so the face's existing LRU order carries over.
    pub fn register_entry_at(&self, key: &str, hash: &str, at: &str) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO entries (key_name, hash, registered_at, last_used_at, use_count) VALUES (?1,?2,?3,?3,0) \
             ON CONFLICT(key_name, hash) DO NOTHING",
            params![key, hash, at],
        )?;
        Ok(())
    }

    /// A USE: recency and `use_count`. Returns how many moved and which hashes
    /// the key did not know.
    pub fn touch(&self, key: &str, hashes: &[String]) -> Result<(u64, Vec<String>)> {
        let now = now_rfc3339();
        let c = self.lock()?;
        let (mut touched, mut unknown) = (0u64, Vec::new());
        for h in hashes {
            let n = c.execute(
                "UPDATE entries SET last_used_at = ?3, use_count = use_count + 1 WHERE key_name = ?1 AND hash = ?2",
                params![key, h, now],
            )?;
            if n == 0 {
                unknown.push(h.clone());
            } else {
                touched += 1;
            }
        }
        Ok((touched, unknown))
    }

    /// The load-bearing half of a PUT: move recency for every EXISTING
    /// membership in `hashes`, in one transaction, without bumping
    /// `use_count` (warming is intent, not a play) and without creating
    /// anything (so a refused PUT leaves no row behind).
    ///
    /// "It should update the timestamp of that item because in the least
    /// recently used cache, that would be the action that indicates that it's
    /// still being used."
    pub fn touch_recency(&self, key: &str, hashes: &[String]) -> Result<u64> {
        if hashes.is_empty() {
            return Ok(0);
        }
        let now = now_rfc3339();
        let mut c = self.lock()?;
        let tx = c.transaction()?;
        let mut moved = 0u64;
        {
            let mut stmt = tx.prepare_cached("UPDATE entries SET last_used_at = ?3 WHERE key_name = ?1 AND hash = ?2")?;
            for h in hashes {
                moved += stmt.execute(params![key, h, now])? as u64;
            }
        }
        tx.commit()?;
        Ok(moved)
    }

    pub fn has_entry(&self, key: &str, hash: &str) -> Result<bool> {
        Ok(self.entry(key, hash)?.is_some())
    }

    pub fn entry(&self, key: &str, hash: &str) -> Result<Option<EntryRow>> {
        Ok(self
            .lock()?
            .query_row(
                "SELECT key_name, hash, registered_at, last_used_at, use_count FROM entries \
                 WHERE key_name = ?1 AND hash = ?2",
                params![key, hash],
                |r| {
                    Ok(EntryRow {
                        key_name: r.get(0)?,
                        hash: r.get(1)?,
                        registered_at: r.get(2)?,
                        last_used_at: r.get(3)?,
                        use_count: r.get::<_, i64>(4)? as u64,
                    })
                },
            )
            .optional()?)
    }

    pub fn key_stats(&self, key: &str) -> Result<KeyStats> {
        let c = self.lock()?;
        let mut s = KeyStats::default();
        let mut stmt = c.prepare(
            "SELECT r.state, COUNT(*), COALESCE(SUM(r.bytes),0) FROM entries e JOIN renders r ON r.hash = e.hash \
             WHERE e.key_name = ?1 GROUP BY r.state",
        )?;
        let rows = stmt.query_map(params![key], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)? as u64))
        })?;
        for row in rows {
            let (state, n, bytes) = row?;
            s.entries += n;
            match state.as_str() {
                "warm" => {
                    s.warm = n;
                    s.bytes += bytes;
                }
                "queued" => s.queued = n,
                "synthesizing" => s.synthesizing = n,
                "failed" => s.failed = n,
                _ => {}
            }
        }
        drop(stmt);
        s.oldest_used_at =
            c.query_row("SELECT MIN(last_used_at) FROM entries WHERE key_name = ?1", params![key], |r| r.get(0))?;
        Ok(s)
    }

    /// Warm bytes across ALL keys, each render counted once however many keys
    /// share it. Keep-warm bytes count: never a victim, never invisible.
    pub fn total_warm_bytes(&self) -> Result<u64> {
        Ok(self
            .lock()?
            .query_row("SELECT COALESCE(SUM(bytes),0) FROM renders WHERE state='warm'", [], |r| r.get::<_, i64>(0))?
            as u64)
    }

    /// Eviction candidates for `key`, least recently used first, skipping
    /// anything used within `min_age_secs`.
    pub fn lru_candidates(&self, key: &str, min_age_secs: i64) -> Result<Vec<(String, u64)>> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(min_age_secs)).to_rfc3339();
        let c = self.lock()?;
        let mut stmt = c.prepare(
            "SELECT e.hash, r.bytes FROM entries e JOIN renders r ON r.hash = e.hash \
             WHERE e.key_name = ?1 AND r.state = 'warm' AND e.last_used_at < ?2 ORDER BY e.last_used_at ASC",
        )?;
        let rows = stmt.query_map(params![key, cutoff], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Drop one membership. If no key still holds the render, the render goes
    /// too and its file is queued for unlinking — all in one transaction, and
    /// the unlink happens after the commit. Returns the bytes freed (0 while
    /// another key still holds the render).
    pub fn drop_entry(&self, key: &str, hash: &str) -> Result<u64> {
        let mut c = self.lock()?;
        let tx = c.transaction()?;
        if tx.execute("DELETE FROM entries WHERE key_name = ?1 AND hash = ?2", params![key, hash])? == 0 {
            return Ok(0);
        }
        let remaining: i64 = tx.query_row("SELECT COUNT(*) FROM entries WHERE hash = ?1", params![hash], |r| r.get(0))?;
        let mut freed = 0;
        if remaining == 0 {
            let row: Option<(Option<String>, i64)> = tx
                .query_row("SELECT rel_path, bytes FROM renders WHERE hash = ?1", params![hash], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            if let Some((rel, bytes)) = row {
                if let Some(rel) = rel {
                    tx.execute(
                        "INSERT OR IGNORE INTO pending_unlink (rel_path, bytes, queued_at) VALUES (?1,?2,?3)",
                        params![rel, bytes, now_rfc3339()],
                    )?;
                }
                freed = bytes as u64;
            }
            tx.execute("DELETE FROM renders WHERE hash = ?1", params![hash])?;
        }
        tx.commit()?;
        Ok(freed)
    }

    pub fn take_pending_unlink(&self) -> Result<Vec<(String, u64)>> {
        let c = self.lock()?;
        let mut stmt = c.prepare("SELECT rel_path, bytes FROM pending_unlink")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn clear_pending_unlink(&self, rel: &str) -> Result<()> {
        self.lock()?.execute("DELETE FROM pending_unlink WHERE rel_path = ?1", params![rel])?;
        Ok(())
    }

    // ── cross-process single-flight ────────────────────────────────────────

    /// Claim the synthesis of `hash`; the primary key is the exclusion. A lock
    /// held by a dead pid or a previous boot is stolen with a compare-and-swap,
    /// so a synthesis interrupted by a restart is reclaimable.
    pub fn claim_synth_lock(&self, hash: &str, boot_id: &str, pid: u32, lane: &str) -> Result<bool> {
        let now = now_rfc3339();
        let c = self.lock()?;
        if c.execute(
            "INSERT INTO synth_locks (hash, boot_id, daemon_pid, lane, acquired_at) VALUES (?1,?2,?3,?4,?5) \
             ON CONFLICT(hash) DO NOTHING",
            params![hash, boot_id, pid as i64, lane, now],
        )? == 1
        {
            return Ok(true);
        }
        let (holder_boot, holder_pid): (String, i64) = c.query_row(
            "SELECT boot_id, daemon_pid FROM synth_locks WHERE hash = ?1",
            params![hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if holder_boot == boot_id && pid_alive(holder_pid as u32) {
            return Ok(false);
        }
        Ok(c.execute(
            "UPDATE synth_locks SET boot_id=?2, daemon_pid=?3, lane=?4, acquired_at=?5 \
             WHERE hash = ?1 AND boot_id = ?6 AND daemon_pid = ?7",
            params![hash, boot_id, pid as i64, lane, now, holder_boot, holder_pid],
        )? == 1)
    }

    pub fn release_synth_lock(&self, hash: &str) -> Result<()> {
        self.lock()?.execute("DELETE FROM synth_locks WHERE hash = ?1", params![hash])?;
        Ok(())
    }

    // ── boot recovery: nothing a dead process stranded survives a boot ─────

    /// Every lock from another boot is stale by definition.
    pub fn clear_locks_not_from(&self, boot_id: &str) -> Result<usize> {
        Ok(self.lock()?.execute("DELETE FROM synth_locks WHERE boot_id != ?1", params![boot_id])?)
    }

    /// `synthesizing` left by a dead daemon becomes `queued`, so the sweep
    /// below collects it.
    pub fn requeue_orphaned_synthesizing(&self) -> Result<usize> {
        Ok(self.lock()?.execute("UPDATE renders SET state='queued' WHERE state='synthesizing'", [])?)
    }

    /// Delete every render in `state`, with its memberships. Called at boot
    /// for `queued` — the queue is in memory, so after a boot nothing intends
    /// to synthesize a `queued` row (production once held 2,284 of them, all
    /// claiming a synthesis was coming) — and for `failed`, because a failure
    /// recorded by a process that no longer exists is not evidence about this
    /// one. Neither owns bytes; `key_counters` keeps the aggregate.
    pub fn clear_stranded(&self, state: &str) -> Result<usize> {
        let mut c = self.lock()?;
        let tx = c.transaction()?;
        tx.execute("DELETE FROM entries WHERE hash IN (SELECT hash FROM renders WHERE state = ?1)", params![state])?;
        let n = tx.execute("DELETE FROM renders WHERE state = ?1", params![state])?;
        tx.commit()?;
        Ok(n)
    }

    /// Abandon a render that has produced NO audio — cancelled or evicted
    /// before it finished. The state guard is the safety: `queued` and
    /// `synthesizing` own nothing on disk (the backend renames its `.part` into
    /// place only on success), and a render that went `warm` under us is left
    /// alone with every membership intact. Memberships go hash-wide, because
    /// the render is gone for every key at once.
    pub fn clear_unfinished_render(&self, hash: &str) -> Result<usize> {
        let mut c = self.lock()?;
        let tx = c.transaction()?;
        let n = tx.execute("DELETE FROM renders WHERE hash = ?1 AND state IN ('queued','synthesizing')", params![hash])?;
        if n > 0 {
            tx.execute("DELETE FROM entries WHERE hash = ?1", params![hash])?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Every `(hash, rel_path)` the store believes is on disk.
    pub fn warm_paths(&self) -> Result<HashMap<String, String>> {
        let c = self.lock()?;
        let mut stmt = c.prepare("SELECT hash, rel_path FROM renders WHERE state='warm' AND rel_path IS NOT NULL")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A warm render whose file has vanished: forget the render and its
    /// memberships, so the next use re-synthesizes from the client's text.
    pub fn forget_missing_render(&self, hash: &str) -> Result<()> {
        let mut c = self.lock()?;
        let tx = c.transaction()?;
        tx.execute("DELETE FROM entries WHERE hash = ?1", params![hash])?;
        tx.execute("DELETE FROM renders WHERE hash = ?1", params![hash])?;
        tx.commit()?;
        Ok(())
    }

    // ── counters ───────────────────────────────────────────────────────────

    pub fn bump(&self, key: &str, counter: Counter, by: i64) -> Result<()> {
        let sql = match counter {
            Counter::SynthTotal => "UPDATE key_counters SET synth_total = synth_total + ?2 WHERE key_name = ?1",
            Counter::EvictedTotal => "UPDATE key_counters SET evicted_total = evicted_total + ?2 WHERE key_name = ?1",
            Counter::FailTotal => "UPDATE key_counters SET fail_total = fail_total + ?2 WHERE key_name = ?1",
            Counter::BytesWritten => "UPDATE key_counters SET bytes_written = bytes_written + ?2 WHERE key_name = ?1",
        };
        let c = self.lock()?;
        c.execute("INSERT OR IGNORE INTO key_counters (key_name) VALUES (?1)", params![key])?;
        c.execute(sql, params![key, by])?;
        Ok(())
    }

    pub fn counters(&self, key: &str) -> Result<Counters> {
        Ok(self
            .lock()?
            .query_row(
                "SELECT synth_total, evicted_total, fail_total, bytes_written FROM key_counters WHERE key_name = ?1",
                params![key],
                |r| {
                    Ok(Counters {
                        synth_total: r.get::<_, i64>(0)? as u64,
                        evicted_total: r.get::<_, i64>(1)? as u64,
                        fail_total: r.get::<_, i64>(2)? as u64,
                        bytes_written: r.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }
}

/// `kill(pid, 0)`: is the process there?
pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only asks the kernel whether the
    // pid exists and may be signalled.
    pid != 0 && unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(s: &Store, key: &str, hash: &str, state: &str) {
        s.ensure_render(hash, "system", "Samantha", 10, "aiff").unwrap();
        if state != "queued" {
            s.set_render_state(hash, state).unwrap();
        }
        s.register_entry(key, hash).unwrap();
    }

    fn memberships(s: &Store) -> Vec<String> {
        s.with_conn(|c| {
            c.prepare("SELECT hash FROM entries ORDER BY key_name, hash")?.query_map([], |r| r.get(0))?.collect()
        })
        .unwrap()
    }

    /// Memberships whose render does not exist — the count every deletion
    /// path must leave at zero.
    fn dangling(s: &Store) -> i64 {
        s.with_conn(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM entries e LEFT JOIN renders r ON r.hash = e.hash WHERE r.hash IS NULL",
                [],
                |r| r.get(0),
            )
        })
        .unwrap()
    }

    /// Why a dangling membership is a leak and not untidiness: no count sees
    /// it, no eviction can take it, and it still sets the key's reported age.
    /// The ghost is written by hand because the code can no longer make one.
    #[test]
    fn a_dangling_membership_is_invisible_to_every_policy_that_could_reclaim_it() {
        let s = Store::open_memory().unwrap();
        seed(&s, "content", "aaa", "warm");
        s.with_conn(|c| {
            c.execute(
                "INSERT INTO entries VALUES ('content','ghost','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z',0)",
                [],
            )
        })
        .unwrap();
        let st = s.key_stats("content").unwrap();
        assert_eq!(st.entries, 1);
        let cands = s.lru_candidates("content", -5).unwrap();
        assert!(cands.iter().any(|(h, _)| h == "aaa"), "the candidate query must return something");
        assert!(cands.iter().all(|(h, _)| h != "ghost"));
        assert_eq!(st.oldest_used_at.as_deref(), Some("2020-01-01T00:00:00Z"));
    }

    #[test]
    fn the_boot_sweeps_take_their_memberships_and_nothing_else() {
        let s = Store::open_memory().unwrap();
        seed(&s, "content", "stranded", "queued");
        seed(&s, "content", "stalefail", "failed");
        seed(&s, "content", "keeper", "warm");
        s.register_entry("interface", "keeper").unwrap();
        assert_eq!(s.clear_stranded("queued").unwrap(), 1);
        assert_eq!(s.clear_stranded("failed").unwrap(), 1);
        assert_eq!(memberships(&s), ["keeper", "keeper"]);
        assert_eq!(dangling(&s), 0);
    }

    #[test]
    fn abandoning_an_unfinished_render_drops_every_keys_membership_but_a_warm_one_is_kept() {
        let s = Store::open_memory().unwrap();
        seed(&s, "content", "inflight", "synthesizing");
        s.register_entry("interface", "inflight").unwrap();
        seed(&s, "content", "won", "warm");
        s.register_entry("interface", "won").unwrap();
        assert_eq!(s.clear_unfinished_render("inflight").unwrap(), 1);
        assert_eq!(s.clear_unfinished_render("won").unwrap(), 0, "the race was lost; the audio stays");
        assert_eq!(memberships(&s), ["won", "won"]);
        assert_eq!(dangling(&s), 0);
    }

    #[test]
    fn forgetting_a_missing_render_takes_its_memberships_with_it() {
        let s = Store::open_memory().unwrap();
        seed(&s, "content", "gone", "warm");
        s.register_entry("interface", "gone").unwrap();
        s.forget_missing_render("gone").unwrap();
        assert!(memberships(&s).is_empty());
        assert_eq!(dangling(&s), 0);
    }

    /// A failed row is a record, not a gate: asking again revives it, and
    /// the record of the failure survives the revival.
    #[test]
    fn a_failed_render_is_revived_by_the_next_request_with_its_history_intact() {
        let s = Store::open_memory().unwrap();
        s.ensure_render("h", "system", "Samantha", 1, "aiff").unwrap();
        s.mark_failed("h", "synthesis_failed", "boom").unwrap();
        assert_eq!(s.ensure_render("h", "system", "Samantha", 1, "aiff").unwrap(), "queued");
        let r = s.get_render("h").unwrap().unwrap();
        assert_eq!((r.fail_count, r.fail_code.as_deref()), (1, Some("synthesis_failed")));
        s.set_render_state("h", "synthesizing").unwrap();
        assert_eq!(s.ensure_render("h", "system", "Samantha", 1, "aiff").unwrap(), "synthesizing");
    }

    /// The lock is stolen only from a dead holder or another boot.
    #[test]
    fn a_synth_lock_is_respected_while_its_holder_lives_and_stolen_after() {
        let s = Store::open_memory().unwrap();
        let me = std::process::id();
        assert!(s.claim_synth_lock("h", "boot-a", me, "warm").unwrap());
        assert!(!s.claim_synth_lock("h", "boot-a", me, "warm").unwrap(), "a live holder in this boot");
        assert!(s.claim_synth_lock("h", "boot-b", me, "warm").unwrap(), "a previous boot's lock");
        s.with_conn(|c| c.execute("UPDATE synth_locks SET daemon_pid = 999999", [])).unwrap();
        assert!(s.claim_synth_lock("h", "boot-b", me, "warm").unwrap(), "a dead holder");
    }

    #[test]
    fn a_store_older_than_count_mode_is_refused_by_name() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cache_keys (name TEXT PRIMARY KEY, mode TEXT NOT NULL CHECK (mode IN ('keep_warm','lru')))",
        )
        .unwrap();
        let e = Store::init(c).err().expect("refused").to_string();
        assert!(e.contains("predates count-mode"), "{e}");
    }
}
