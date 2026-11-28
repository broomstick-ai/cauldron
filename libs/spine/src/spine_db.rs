//! The pending-transactions record — the spine proper (ONE central database
//! for the whole cauldron; prefer "the spine" over "core database" in code
//! and prose).
//!
//! The commit point of the whole design is the ROW FLIP here, never the
//! storage engine (cross-file behavior is VERIFIED by the atomicity tests
//! rather than inherited as a claim). Recovery consults the row, never the
//! pool files.
//!
//! Rows are kept FOREVER; this crate ships no pruning path and no DELETE on
//! `transactions`, by design (the spine file only grows, accepted for this
//! build). An aborted transaction still leaves its row, landed flags false —
//! the abort is a recorded fact (nobody "cleans up" aborted rows).
//!
//! The spine file is the one storage-layer serialization point (every
//! transaction writes it twice — the honest price of crash safety). It is
//! the first number to measure; measurements are reported as data, and its
//! sharding escape is deliberately undesigned (a known residual).

use crate::delta::CompiledPayload;
use crate::envelope::{Envelope, TxHash};
use crate::error::{Result, SpineError};
use crate::schema_def::PoolName;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The spine file's name inside the cauldron directory.
pub const SPINE_FILE: &str = "spine.db";

/// The spine DDL (the pending row, one column per field). The lock tables
/// ([`crate::locks::LOCKS_DDL`]) are created alongside at cauldron create.
///
/// Row-design room left deliberately, built on nothing: a
/// future trigger release adds per-match "attempted" bookkeeping and
/// failure events as ordinary appends — many rows per causal chain, never
/// one transaction per chain. The outbound-request journal's future home is
/// this database; no table for it exists yet.
/// # Dedup keys on "did this LAND", not on "was this attempted"
///
/// The `hash` column was once `UNIQUE`. That made one identity mean one ROW,
/// which collided head-on with the restart rule (a restarted statement
/// RESTARTS with a fresh sequence number) and with the sequence number
/// BEING the pending row's rowid: a second attempt at the same ask could have
/// neither a second row nor a rewritten payload. It also meant an ABORTED
/// attempt blocked its own caller from ever re-submitting.
///
/// So dedup keys on "did this already land", the exactly-once question
/// itself. The column carries an `attempt` ordinal and the
/// uniqueness moves into TWO PARTIAL INDEXES, each with a different job:
///
/// - `idx_tx_live` — at most one UN-CLEARED row per hash. An un-cleared row
///   is an attempt IN FLIGHT, and two in-flight attempts at one ask would
///   race to land it. This is the N >= 2 arbitration point: concurrent
///   same-hash submitters contend on this index inside one IMMEDIATE
///   transaction, exactly one wins the insert, and every loser is handed the
///   winner's row.
/// - `idx_tx_committed` — at most one DECIDED row per hash, FOREVER. The
///   decided flip is the commit point, so this is exactly-once
///   made structural rather than merely enforced by the door: even a caller
///   who bypassed [`SpineDb::append_or_existing`] could never land the same
///   identity twice.
///
/// An ABORTED attempt (decided = 0, cleared = 1) is in neither index, so it
/// blocks nothing and a retry proceeds as a fresh attempt with a fresh
/// sequence number. Rows are still never deleted and aborted rows are still
/// kept forever — what changed is only what uniqueness means.
pub const SPINE_DDL: &str = r#"
CREATE TABLE transactions (
  seq              INTEGER PRIMARY KEY AUTOINCREMENT,
  hash             TEXT NOT NULL,
  attempt          INTEGER NOT NULL DEFAULT 1,
  ts               TEXT NOT NULL,
  envelope_version INTEGER NOT NULL,
  initiator        TEXT NOT NULL CHECK (json_valid(initiator)),
  instrument       TEXT NOT NULL CHECK (json_valid(instrument)),
  statement        TEXT NOT NULL,
  bindings         TEXT NOT NULL CHECK (json_valid(bindings)),
  payload          TEXT NOT NULL CHECK (json_valid(payload)),
  result           TEXT CHECK (result IS NULL OR json_valid(result)),
  decided          INTEGER NOT NULL DEFAULT 0,
  cleared          INTEGER NOT NULL DEFAULT 0,
  -- 1 when RECOVERY rolled this row forward and computed its RETURN
  -- projection at the cleared flip. An
  -- audit fact only: `result` holds a real projection either way, so
  -- "was this recovered" is read from THIS column and never inferred from
  -- the shape of `result`.
  recovered        INTEGER NOT NULL DEFAULT 0,
  -- THE THIRD EXPOSURE (the validated read's window check): the
  -- committed-commit counter's value AT this commit — set in
  -- the SAME spine transaction as the LAST part's landed flip and the
  -- counter increment (`mark_part_landed`), never a second write. NULL
  -- until then, forever NULL for an aborted attempt. It is what lets a
  -- validated read ask "WHICH commits landed in my window (c_start,
  -- c_end]", which a scalar counter cannot answer.
  committed_at     INTEGER,
  -- The pools this commit TOUCHES for invalidation purposes: the parts
  -- carrying at least one delta, as a JSON array of pool names in part
  -- order. A premise-only part (zero deltas) is still a recorded part in
  -- `tx_parts` with its validated-under stamp, but it changed no state a
  -- reader could have straddled, so it is absent here. Written at append,
  -- immutable after.
  touched          TEXT NOT NULL CHECK (json_valid(touched)),
  -- THE TRANCHE MARK: set on a tranche's ANCHOR row
  -- (`tranche_ordinal` NULL with `tranche_id` set) and on each
  -- reserved child row. The ORDINARY recovery pass skips every row whose
  -- `tranche_id` is set. `tranche_index` and
  -- `tranche_of` are written at reservation so the publisher builds a
  -- child's event from the row alone.
  tranche_id       TEXT,
  tranche_attempt  INTEGER,
  tranche_ordinal  INTEGER,
  tranche_index    INTEGER,
  tranche_of       INTEGER
);
-- The third exposure's index: a validated read's window query ranges
-- over `committed_at` and nothing else; aborted rows (NULL) stay out.
CREATE INDEX idx_tx_committed_at ON transactions(committed_at)
  WHERE committed_at IS NOT NULL;

-- At most ONE attempt at a hash may be in flight at a time (see the DDL's
-- own docs). This index IS the concurrent-submission arbiter.
CREATE UNIQUE INDEX idx_tx_live ON transactions(hash) WHERE cleared = 0;
-- At most ONE attempt at a hash may ever be DECIDED. Exactly-once, made
-- structural: the decided flip is the commit point.
CREATE UNIQUE INDEX idx_tx_committed ON transactions(hash) WHERE decided = 1;
-- Attempt lookup by identity, now that a hash may name several rows.
CREATE INDEX idx_tx_hash ON transactions(hash, attempt);

CREATE TABLE tx_parts (
  seq            INTEGER NOT NULL,
  part           INTEGER NOT NULL,
  pool           TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  landed         INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (seq, part)
) WITHOUT ROWID;
CREATE INDEX idx_tx_parts_pool ON tx_parts(pool, landed);

CREATE TABLE counters (
  singleton         INTEGER PRIMARY KEY CHECK (singleton = 1),
  committed_commits INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE spine_state (
  singleton     INTEGER PRIMARY KEY CHECK (singleton = 1),
  halted        INTEGER NOT NULL DEFAULT 0,
  halted_reason TEXT,
  -- The SPINE's own schema version — NOT the pool grammar
  -- version `tx_parts.schema_version` records. 0 = a spine from before the
  -- tranche record; see `SPINE_SCHEMA_VERSION`.
  spine_schema_version INTEGER NOT NULL DEFAULT 0
);

-- THE QUIESCERS (the quiesce hook). A quiesce has an OWNER: one row per outstanding quiesce, keyed by
-- a nonce the quiescing handle holds, and the cauldron is quiesced while ANY
-- row exists. It used to be one boolean on `spine_state` with no owner and
-- no count — the first `resume()` lifted a quiesce a second caller still
-- held, and a `resume()` from anyone lifted one mid-drain invisibly.
CREATE TABLE quiescers (
  nonce  TEXT PRIMARY KEY,
  holder TEXT NOT NULL,
  since  TEXT NOT NULL
) WITHOUT ROWID;
"#;

/// THE SPINE SCHEMA VERSION this binary writes and knows.
///
/// Named `spine_schema_version` everywhere (the column on `spine_state`, this
/// constant) so it cannot be confused with `tx_parts.schema_version`, which is
/// the POOL GRAMMAR version a part validated under and has nothing to do with
/// the spine file's own shape.
///
/// - `0` — a spine from before the tranche record (no column at all reads as
///   0). [`SpineDb::fence_and_upgrade`] upgrades it in place to `1`.
/// - `1` — the tranche record: the five `tranche_*` columns on
///   `transactions`, the `tranches` and `tranche_children` tables.
///
/// A spine whose version is HIGHER than this is refused at open, by name
/// ([`crate::error::SpineError::SpineSchemaNewer`]): an older binary would not
/// know an anchor row from any other un-decided row, and its ordinary
/// recovery pass would abort it under a live tranche.
pub const SPINE_SCHEMA_VERSION: u32 = 1;

/// The tranche record, `IF NOT EXISTS` so the same text both
/// creates a fresh spine and upgrades a version-0 one in place.
///
/// - `tranches` — one row per ATTEMPT `(id, attempt)`. At most one attempt per
///   `id` is non-terminal at a time (`idx_tranches_live`). `seq` is the anchor
///   row's sequence number — the queue order, never the clock. Every
///   time column is milliseconds since the Unix epoch on the daemon's clock.
///   `key_hash` is the hex SHA-256 of the holder's 32 key bytes; the
///   key itself is never stored.
/// - `tranche_children` — one row per staged child. The
///   unique `(tranche_id, tranche_attempt, hash)` index is the dedup
///   against the tranche's own children. `state` is deliberately not
///   CHECK-constrained: later steps add states (`reserved`, `landed`) and a
///   CHECK cannot be widened in place.
pub const TRANCHE_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS tranches (
  id              TEXT NOT NULL,
  attempt         INTEGER NOT NULL CHECK (attempt >= 1),
  seq             INTEGER NOT NULL UNIQUE,
  state           TEXT NOT NULL CHECK (state IN
                    ('queued','open','staging','landing','landed','rolled-back','failed','abandoned')),
  lock_mode       TEXT NOT NULL CHECK (lock_mode IN ('whole','touched')),
  holder_service  TEXT NOT NULL,
  key_hash        TEXT NOT NULL,
  regions         TEXT NOT NULL CHECK (json_valid(regions)),
  began_at        INTEGER NOT NULL,
  last_heartbeat  INTEGER NOT NULL,
  deadline        INTEGER NOT NULL,
  head_since      INTEGER,
  draining        INTEGER NOT NULL DEFAULT 0,
  decided         INTEGER NOT NULL DEFAULT 0,
  renumbered      TEXT CHECK (renumbered IS NULL OR json_valid(renumbered)),
  landed_children INTEGER NOT NULL DEFAULT 0,
  landing_error   TEXT CHECK (landing_error IS NULL OR json_valid(landing_error)),
  ended_at        INTEGER,
  reason          TEXT,
  refusal         TEXT CHECK (refusal IS NULL OR json_valid(refusal)),
  staged          INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (id, attempt)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tranches_live ON tranches(id)
  WHERE state IN ('queued','open','staging','landing');
CREATE TABLE IF NOT EXISTS tranche_children (
  tranche_id      TEXT NOT NULL,
  tranche_attempt INTEGER NOT NULL,
  ordinal         INTEGER NOT NULL CHECK (ordinal >= 0),
  hash            TEXT NOT NULL,
  envelope        TEXT NOT NULL CHECK (json_valid(envelope)),
  payload         TEXT NOT NULL CHECK (json_valid(payload)),
  bound           TEXT CHECK (bound IS NULL OR json_valid(bound)),
  state           TEXT NOT NULL,
  seq             INTEGER,
  PRIMARY KEY (tranche_id, tranche_attempt, ordinal)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tranche_children_hash
  ON tranche_children(tranche_id, tranche_attempt, hash);
CREATE INDEX IF NOT EXISTS idx_tx_tranche ON transactions(tranche_id, tranche_attempt)
  WHERE tranche_id IS NOT NULL;
"#;

/// The five `transactions` columns the tranche record adds, for the in-place
/// upgrade of a version-0 spine (`ALTER TABLE … ADD COLUMN`, one each).
const TRANCHE_TX_COLUMNS: [&str; 5] = [
    "tranche_id TEXT",
    "tranche_attempt INTEGER",
    "tranche_ordinal INTEGER",
    "tranche_index INTEGER",
    "tranche_of INTEGER",
];

/// THE SERIAL COUNTERS: one row per (pool, id prefix), `last` the
/// highest serial the spine has minted for it. A node type whose grammar
/// sets `id_prefix` gets its identifiers as `<prefix>-<n>`, and `n` comes
/// from here, advanced inside the DECIDED FLIP's own transaction — the
/// commit point — together with the rewrite of the row's
/// payload and lock rows from the provisional ids to the serials
/// ([`crate::delta::PROVISIONAL_MARK`]). So a number is consumed exactly
/// when a commit is made: an attempt refused or restarted before the flip
/// consumes nothing (no gap), two flips serialize on the spine's write lock
/// (no duplicate), nothing ever decrements `last` (no reuse after a node is
/// gone), and the counter is a row in the spine file, so it survives a
/// reopen and a recovery. Keyed per POOL as well as per prefix: the rule is
/// a type-prefixed serial per pool, and two pools pinning one grammar each
/// count their own `T-1`.
///
/// `IF NOT EXISTS`, and run again by the flip before it first reads a
/// counter, so a spine created before this table existed gains it on the
/// first serial mint rather than refusing it.
pub const ID_SERIALS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS id_serials (
  pool   TEXT NOT NULL,
  prefix TEXT NOT NULL,
  last   INTEGER NOT NULL CHECK (last >= 0),
  PRIMARY KEY (pool, prefix)
) WITHOUT ROWID;
"#;

/// A transaction's sequence number — the pending row's AUTOINCREMENT rowid,
/// issued at open. Simultaneously the lock-queue order, the clearing order
/// among dependents, and the serialization order the deadlock argument
/// needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Seq(pub i64);

/// One outstanding quiesce as the `quiescers` table records it (the quiesce
/// hook): the nonce its token carries, who took it, and since when. What a
/// quiesced refusal names and what `resume_all` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quiescer {
    pub nonce: String,
    /// The taker as it described itself at `quiesce()` — `pid <n>` for the
    /// sanctioned hook, `by-hand` for the test-support staging call.
    pub holder: String,
    /// ISO-8601 UTC, the spine's clock at the moment the quiescer was
    /// recorded.
    pub since: String,
}

/// Read every outstanding quiescer off a connection or a transaction (the
/// append gate reads inside its own IMMEDIATE transaction), oldest first.
pub(crate) fn quiescers_in(conn: &Connection) -> Result<Vec<Quiescer>> {
    let mut stmt = conn.prepare("SELECT nonce, holder, since FROM quiescers ORDER BY since, nonce")?;
    let rows = stmt.query_map([], |r| {
        Ok(Quiescer {
            nonce: r.get(0)?,
            holder: r.get(1)?,
            since: r.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

impl std::fmt::Display for Seq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A handle on the spine database. Connection private, no accessor — no
/// transaction handle escapes the crate (shared-contract idiom).
pub struct SpineDb {
    conn: Connection,
    /// The HOLD this connection's handle owns ([`crate::Cauldron::hold`]):
    /// the exact `halted_reason` it set, which its own writes pass and
    /// nobody else's do. `None` for every other handle.
    hold: Option<String>,
    /// Whether this connection's next decided flip also lifts the hold, in
    /// the same transaction ([`crate::Cauldron::lift_hold_at_next_commit`]).
    lift_hold: bool,
}

/// The pragmas every spine connection runs under. WAL and a 5s busy
/// timeout are soup's inherited idiom; `synchronous = FULL` is a deliberate
/// step UP from soup's NORMAL for this one file, because the row flip here
/// is THE commit point of the whole design — the one record
/// that must never claim less than what the pools hold.
fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    Ok(())
}

/// The ISO-8601 UTC timestamp with a trailing `Z`, sourced from SQLite so
/// every stamp comes from one clock and one formatter (soup idiom,
/// inherited). Wall clock, RECORDED, never used for ordering — ordering is
/// the sequence number, always.
fn now(conn: &Connection) -> Result<String> {
    Ok(conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
        r.get(0)
    })?)
}

impl SpineDb {
    /// Create `spine.db` with [`SPINE_DDL`] plus the lock tables, WAL mode.
    /// Refuses an existing file.
    pub fn create(path: &Path) -> Result<SpineDb> {
        if path.exists() {
            return Err(SpineError::usage(format!(
                "refusing to create spine database over an existing file: {}",
                path.display()
            )));
        }
        let conn = Connection::open(path)?;
        apply_pragmas(&conn)?;
        // One batch transaction: a spine that opens is a spine with every
        // table and both singleton rows (soup idiom: nothing lazy).
        conn.execute_batch(&format!(
            "BEGIN;\n{SPINE_DDL}\n{ID_SERIALS_DDL}\n{}\n{TRANCHE_DDL}\n\
             INSERT INTO counters (singleton, committed_commits) VALUES (1, 0);\n\
             INSERT INTO spine_state (singleton, halted, halted_reason, spine_schema_version)\n\
             VALUES (1, 0, NULL, {SPINE_SCHEMA_VERSION});\n\
             COMMIT;",
            crate::locks::LOCKS_DDL
        ))?;
        Ok(SpineDb { conn, hold: None, lift_hold: false })
    }

    /// Open an existing spine file.
    pub fn open(path: &Path) -> Result<SpineDb> {
        if !path.exists() {
            return Err(SpineError::not_found(format!(
                "no spine database at {}",
                path.display()
            )));
        }
        let conn = Connection::open(path)?;
        apply_pragmas(&conn)?;
        // A file that is not a spine is reported loudly, never treated as
        // an empty spine (soup idiom: a store that will not answer is never
        // an empty store).
        let has_table: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'transactions'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if has_table.is_none() {
            return Err(SpineError::hard(format!(
                "{} exists but is not a spine database (no transactions table)",
                path.display()
            )));
        }
        Ok(SpineDb { conn, hold: None, lift_hold: false })
    }

    /// THE SCHEMA FENCE, run by every `Cauldron` open
    /// before anything reads a row: read the spine's
    /// [`SPINE_SCHEMA_VERSION`]-numbered shape and
    ///
    /// - a spine NEWER than `binary_version` → refused by name,
    ///   [`SpineError::SpineSchemaNewer`] — nothing written;
    /// - a spine OLDER (a version-0 spine from before the tranche record) →
    ///   upgraded in place to `binary_version`'s shape, in one IMMEDIATE
    ///   transaction (two processes opening at once serialize, and the second
    ///   finds the work done);
    /// - equal → nothing.
    ///
    /// `binary_version` is [`SPINE_SCHEMA_VERSION`] on every production path;
    /// only the test-support "older binary" simulation passes another
    /// (`Cauldron::open_as_older_binary`).
    pub(crate) fn fence_and_upgrade(&mut self, binary_version: u32) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let has_version_col = column_exists(&tx, "spine_state", "spine_schema_version")?;
        let found: u32 = if has_version_col {
            tx.query_row(
                "SELECT spine_schema_version FROM spine_state WHERE singleton = 1",
                [],
                |r| r.get::<_, i64>(0),
            )? as u32
        } else {
            0
        };
        if found > binary_version {
            return Err(SpineError::SpineSchemaNewer {
                spine_version: found,
                binary_version,
            });
        }
        if found == binary_version {
            return Ok(());
        }
        // found < binary_version: the only older shape is 0, and the only
        // newer one this binary knows is 1.
        if !has_version_col {
            tx.execute_batch(
                "ALTER TABLE spine_state ADD COLUMN spine_schema_version INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        for col in TRANCHE_TX_COLUMNS {
            let name = col.split(' ').next().expect("a column spec names its column");
            if !column_exists(&tx, "transactions", name)? {
                tx.execute_batch(&format!("ALTER TABLE transactions ADD COLUMN {col};"))?;
            }
        }
        tx.execute_batch(TRANCHE_DDL)?;
        tx.execute(
            "UPDATE spine_state SET spine_schema_version = ?1 WHERE singleton = 1",
            params![binary_version],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The interior connection, for the lock tables that live in this same
    /// file. Crate-private — no transaction handle
    /// escapes the crate (shared-contract idiom).
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// The interior connection, mutably — for a crate-interior module that
    /// opens its own IMMEDIATE transaction across several tables (the
    /// tranche record). Crate-private, as [`Self::conn`].
    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// The per-statement busy wait, from configuration
    /// (`SpineConfig::busy_timeout`). `apply_pragmas` sets the
    /// inherited 5s at open; the cauldron overrides it with its own number
    /// on every handle it opens.
    pub(crate) fn set_busy_timeout(&self, timeout: std::time::Duration) -> Result<()> {
        self.conn.busy_timeout(timeout)?;
        Ok(())
    }

    // ---- row lifecycle (the stage vocabulary these flips traverse is
    //      recovery's — `crate::recover::Stage`) ----
    //
    // VISIBILITY (the one-door law): every lifecycle flip below is `pub` ONLY under the
    // `test-support` feature — the crate's own dev-dependency enables it
    // for the test suites, which stage crash states with these directly; a
    // production consumer sees them `pub(crate)` and writes through
    // [`crate::Cauldron::submit`] alone. A caller that could flip `decided`
    // or land parts by hand is a second write path — a defect by
    // construction. Behavior is identical in both
    // builds; only reachability changes. Each `*_interior` carries the real
    // contract docs; the cfg pairs are mechanical.

    #[cfg(feature = "test-support")]
    pub fn append_pending(&mut self, envelope: &Envelope, hash: &TxHash, payload: &CompiledPayload) -> Result<Seq> {
        self.append_pending_interior(envelope, hash, payload)
    }
    // (`append_pending` has NO `pub(crate)` twin: nothing in the crate
    // calls it — the door goes through `append_or_existing` — so without
    // `test-support` the by-hand append simply does not exist,
    // rather than existing dead.)
    #[cfg(feature = "test-support")]
    pub fn append_or_existing(&mut self, envelope: &Envelope, hash: &TxHash, payload: &CompiledPayload) -> Result<AppendOutcome> {
        self.append_or_existing_interior(envelope, hash, payload, AppendGate::Commit)
    }
    #[cfg(not(feature = "test-support"))]
    pub(crate) fn append_or_existing(&mut self, envelope: &Envelope, hash: &TxHash, payload: &CompiledPayload) -> Result<AppendOutcome> {
        self.append_or_existing_interior(envelope, hash, payload, AppendGate::Commit)
    }

    /// The ESCALATED READ's row append: the same exactly-once append as
    /// [`Self::append_or_existing`], under the HALT gate (a row is a spine
    /// write, and a spine that physics refuses to write cannot issue a
    /// sequence number) but NOT the QUIESCE gate. Quiesce is "no new commit
    /// begins; in-flight commits finish"; an escalating reader
    /// appends a row only to be issued a sequence number — it never decides,
    /// never enters `in_landing`, never moves the committed-commit counter —
    /// so it is not a commit, and a backup taken on quiesce's assurance can
    /// never contain it half-landed. Refusing it here used to FAIL every
    /// cross-pool read that straddled an in-flight commit under a quiesce,
    /// in milliseconds, for a read. Crate-interior: only
    /// `read::escalated*` appends this way.
    /// `pub` under `test-support`: the lock tests stage a storm of
    /// escalated-read rows — real read rows under real `read` grants — and
    /// a read's row is the one row that passes the tranche gate's checkpoint
    /// 2 (readers are never blocked by the gate).
    #[cfg(feature = "test-support")]
    pub fn append_read_row(&mut self, envelope: &Envelope, hash: &TxHash, payload: &CompiledPayload) -> Result<AppendOutcome> {
        self.append_or_existing_interior(envelope, hash, payload, AppendGate::Read)
    }
    #[cfg(not(feature = "test-support"))]
    pub(crate) fn append_read_row(&mut self, envelope: &Envelope, hash: &TxHash, payload: &CompiledPayload) -> Result<AppendOutcome> {
        self.append_or_existing_interior(envelope, hash, payload, AppendGate::Read)
    }
    // (`mark_decided` has NO `pub(crate)` twin: the door decides
    // through `try_mark_decided`, so nothing in the crate called the twin and
    // it existed dead behind a compiler warning. Without `test-support` the
    // by-hand flip simply does not exist, like `append_pending` above.)
    #[cfg(feature = "test-support")]
    pub fn mark_decided(&mut self, seq: Seq) -> Result<()> {
        self.mark_decided_interior(seq)
    }
    #[cfg(feature = "test-support")]
    pub fn mark_part_landed(&mut self, seq: Seq, part: u32) -> Result<()> {
        self.mark_part_landed_interior(seq, part)
    }
    #[cfg(not(feature = "test-support"))]
    pub(crate) fn mark_part_landed(&mut self, seq: Seq, part: u32) -> Result<()> {
        self.mark_part_landed_interior(seq, part)
    }
    #[cfg(feature = "test-support")]
    pub fn mark_cleared(&mut self, seq: Seq, result: &serde_json::Value) -> Result<bool> {
        self.mark_cleared_interior(seq, result)
    }
    #[cfg(not(feature = "test-support"))]
    pub(crate) fn mark_cleared(&mut self, seq: Seq, result: &serde_json::Value) -> Result<bool> {
        self.mark_cleared_interior(seq, result)
    }
    #[cfg(feature = "test-support")]
    pub fn mark_aborted_if_undecided(&mut self, seq: Seq, result: &serde_json::Value) -> Result<bool> {
        self.mark_aborted_if_undecided_interior(seq, result)
    }
    #[cfg(not(feature = "test-support"))]
    pub(crate) fn mark_aborted_if_undecided(&mut self, seq: Seq, result: &serde_json::Value) -> Result<bool> {
        self.mark_aborted_if_undecided_interior(seq, result)
    }

    /// The door's first and only unfailable step: append the full pending
    /// row — statement, bindings, provenance, stamps, AND the compiled
    /// payload — durably, BEFORE any pool landing begins. Issues the
    /// sequence number. One part row per touched pool, each stamped with the
    /// schema version it validated under. When physics refuses this append,
    /// the caller halts fail-closed (see [`crate::recover::halt`]).
    ///
    /// A duplicate hash identity is detected HERE, in the same atomic step
    /// as the insert, and refused with a [`SpineError::Usage`] naming the
    /// existing row. Nothing is appended twice.
    ///
    /// This signature is frozen and it cannot carry a
    /// row back, so THE exactly-once door is [`Self::append_or_existing`],
    /// which this method delegates to: it returns the ORIGINAL row — and
    /// with it the original outcome in `result` — in one call. The IDENTITY
    /// of that row (its seq) is decided atomically with the dedup test; the
    /// row's CONTENT is then read after the transaction closes, so it may be
    /// FRESHER than what the dedup test saw — which is benign, because the
    /// row's lifecycle is monotonic (cleared never un-clears, decided never
    /// un-decides) and a fresher state of the same row is a truer answer,
    /// never a different row. Callers
    /// wanting the dedup reply call that; callers that only ever open fresh
    /// transactions call this.
    ///
    /// A halted spine refuses the append fail-closed (no new
    /// commit begins), before touching anything.
    #[cfg(feature = "test-support")]
    fn append_pending_interior(
        &mut self,
        envelope: &Envelope,
        hash: &TxHash,
        payload: &CompiledPayload,
    ) -> Result<Seq> {
        match self.append_or_existing_interior(envelope, hash, payload, AppendGate::Commit)? {
            AppendOutcome::Appended(seq) => Ok(seq),
            AppendOutcome::Duplicate(row) => Err(SpineError::usage(format!(
                "duplicate hash identity {}: transaction already recorded at seq {}; \
                 exactly-once returns the ORIGINAL outcome via append_or_existing, never a \
                 second append",
                hash.to_hex(),
                row.seq
            ))),
        }
    }

    /// THE exactly-once append: append the pending row, or —
    /// if this hash identity is already recorded — append NOTHING and hand
    /// back the ORIGINAL row, whose `result` column carries the original
    /// outcome.
    ///
    /// The duplicate check and the insert share ONE `IMMEDIATE` transaction,
    /// so two processes submitting the same envelope at the same instant
    /// cannot both append: one appends, the other reads the row the first
    /// one wrote. "Did this already land" is then answerable from the
    /// returned row alone, with no new machinery.
    ///
    /// Everything the pending row records goes in here, at open, before any
    /// pool landing:
    /// the hash, the wall-clock timestamp (recorded, never an ordering key),
    /// the envelope format version, initiator and instrument, the
    /// statement text and its bindings, and the COMPILED DELTAS — the full
    /// change payload, durable before the first landing, which is what makes
    /// roll-forward possible at all. One part row per touched
    /// pool, each stamped with the schema version that part validated under.
    fn append_or_existing_interior(
        &mut self,
        envelope: &Envelope,
        hash: &TxHash,
        payload: &CompiledPayload,
        gate: AppendGate,
    ) -> Result<AppendOutcome> {
        let initiator = to_json(&envelope.provenance.initiator)?;
        let instrument = to_json(&envelope.provenance.instrument)?;
        let bindings = to_json(&envelope.bindings)?;
        let payload_json = to_json(payload)?;
        let touched = to_json(&touched_pools_of(payload))?;
        let hold = self.hold.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // The halt and quiesce gates, INSIDE the append's own IMMEDIATE
        // transaction — not only at the door's phase 1. The door's early gate
        // is a courtesy refusal; THIS one is the guarantee. Checked outside
        // the transaction (as the halt once was, and the quiesce never was),
        // a door call that passed the gate before the flag flipped could
        // append AFTER `quiesce()` observed the spine drained and returned —
        // and a whole-cauldron backup taken on that assurance would contain
        // the half-landed commit the hook exists to prevent.
        // Inside the IMMEDIATE transaction the two writes serialize: either
        // this append commits first and the drain loop sees the row and
        // waits for it, or the flag commits first and this append refuses.
        // (Regression tests in door_smoke.)
        {
            let (halted, reason, quiesced): (i64, Option<String>, i64) = tx.query_row(
                "SELECT halted, halted_reason, (SELECT COUNT(*) FROM quiescers)
                 FROM spine_state WHERE singleton = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            if halted != 0 && !(hold.is_some() && hold.as_deref() == reason.as_deref()) {
                return Err(SpineError::Halted {
                    reason: reason
                        .unwrap_or_else(|| "halted with no recorded reason".to_string()),
                });
            }
            // The quiesce gate is a gate on COMMITS. An escalated read's row
            // (`AppendGate::Read`) passes it — see `append_read_row`. The
            // refusal NAMES every quiescer, holder and since: read
            // inside this same transaction, so what it names is exactly
            // what refused it.
            if quiesced != 0 && gate == AppendGate::Commit {
                let standing = quiescers_in(&tx)?;
                return Err(crate::recover::quiesced_busy(&standing));
            }
        }
        // THE DEDUP TEST, in this order. It asks "did this
        // already land, or is it landing", never "was this ever attempted".
        //
        // 1. A DECIDED row is the commit itself — landed or landing. It is
        //    the original outcome and it is final, forever.
        // 2. An UN-CLEARED row is another attempt IN FLIGHT. Its outcome does
        //    not exist yet; the caller is handed the row and waits.
        // 3. Anything else means every prior attempt ABORTED, and an abort
        //    landed nothing — so this is a FRESH attempt with a fresh
        //    sequence number, which is what the restart rule needs.
        let existing: Option<i64> = tx
            .query_row(
                "SELECT seq FROM transactions WHERE hash = ?1 AND decided = 1",
                params![hash.to_hex()],
                |r| r.get(0),
            )
            .optional()?
            .or(tx
                .query_row(
                    "SELECT seq FROM transactions WHERE hash = ?1 AND cleared = 0",
                    params![hash.to_hex()],
                    |r| r.get(0),
                )
                .optional()?);
        if let Some(seq) = existing {
            // Nothing is appended. The original row IS the reply.
            drop(tx);
            return Ok(AppendOutcome::Duplicate(Box::new(self.read_row(Seq(seq))?)));
        }
        // CHECKPOINT 2 of the tranche gate —
        // THE GUARANTEE for new rows, inside this same immediate transaction
        // and before the insert: a writer that passed the door's phase 1
        // before a `whole` tranche raised the gate is refused `busy
        // tranche_busy / locked` here with no row. A read's row
        // (`AppendGate::Read`, the escalated read) passes — readers are
        // never blocked by the gate. Only the ordinary door reaches
        // this function: the anchor is a direct insert in `tranche_begin`
        // and the lander's reservations are its own, so the
        // tranche's own rows never meet it.
        if gate == AppendGate::Commit {
            if let Some(holder) = crate::tranche::gate_up(&tx)? {
                return Err(crate::tranche::locked_busy(&holder));
            }
        }
        // The attempt ordinal: one past the highest this identity has used,
        // so the row set reads as attempt 1, attempt 2, ... in order.
        let attempt: i64 = tx.query_row(
            "SELECT COALESCE(MAX(attempt), 0) + 1 FROM transactions WHERE hash = ?1",
            params![hash.to_hex()],
            |r| r.get(0),
        )?;
        let ts = now(&tx)?;
        let inserted = tx.execute(
            "INSERT INTO transactions
               (hash, attempt, ts, envelope_version, initiator, instrument, statement,
                bindings, payload, touched)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                hash.to_hex(),
                attempt,
                ts,
                envelope.format_version,
                initiator,
                instrument,
                envelope.statement,
                bindings,
                payload_json,
                touched,
            ],
        );
        // THE BELT UNDER THE DEDUP TEST, and it must not read as PHYSICS.
        // `idx_tx_live` / `idx_tx_committed` refuse a second in-flight or a
        // second decided attempt at one identity. The checks above mean a
        // correct caller never reaches this, but "it cannot happen" is
        // exactly the reasoning that makes the response matter: a constraint
        // violation is a LOGIC fault, and letting it fall through as
        // `SpineError::Hard` would make the door's fail-closed path halt the
        // whole cauldron over a bug it could simply refuse. A mutation sweep
        // produced precisely that halt.
        if let Err(rusqlite::Error::SqliteFailure(f, _)) = &inserted {
            if f.code == rusqlite::ErrorCode::ConstraintViolation {
                return Err(SpineError::usage(format!(
                    "refusing a second attempt at hash identity {} beside one that is already \
                     in flight or already decided; exactly-once admits one live attempt and one \
                     landing per identity",
                    hash.to_hex()
                )));
            }
        }
        inserted?;
        let seq = tx.last_insert_rowid();
        for (i, part) in payload.parts.iter().enumerate() {
            tx.execute(
                "INSERT INTO tx_parts (seq, part, pool, schema_version, landed)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                params![seq, i as i64, part.pool.as_str(), part.schema_version],
            )?;
        }
        tx.commit()?;
        Ok(AppendOutcome::Appended(Seq(seq)))
    }

    /// Flip the commit-decided marker: after validation and lock
    /// acquisition succeed, before the first pool landing. What recovery
    /// reads to choose abort versus roll-forward.
    ///
    /// LIFECYCLE GUARDS (with the attempt-aware record). Two, both
    /// consequences of one identity now naming several attempt rows:
    ///
    /// - **At most one attempt per identity may EVER be decided.** The
    ///   decided flip is the commit point, so this is
    ///   exactly-once made structural: `idx_tx_committed` refuses the second
    ///   one at the storage layer, and this check refuses it BY NAME one
    ///   layer up rather than as a bare constraint violation. Nothing on the
    ///   door path can reach it — [`Self::append_or_existing`] dedups a
    ///   decided identity long before — which is exactly why it is here: it
    ///   is the net under the derivation, for a caller that bypassed the
    ///   dedup door.
    /// - **A CLEARED row is never decided afterwards.** A cleared-and-
    ///   undecided row is an ABORTED attempt; deciding one would
    ///   resurrect an abandoned attempt into the commit index and claim a
    ///   landing for a payload nothing ever applied.
    ///
    /// The read and the flip share one `IMMEDIATE` transaction, so two
    /// processes racing the same identity cannot both pass the check.
    fn mark_decided_interior(&mut self, seq: Seq) -> Result<()> {
        let lift = if self.lift_hold { self.hold.clone() } else { None };
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row: Option<(String, i64)> = tx
            .query_row(
                "SELECT hash, cleared FROM transactions WHERE seq = ?1",
                params![seq.0],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((hex, cleared)) = row else {
            return Err(SpineError::not_found(format!(
                "mark_decided: no transaction row at seq {seq}"
            )));
        };
        if cleared != 0 {
            return Err(SpineError::usage(format!(
                "mark_decided: seq {seq} is already cleared; a cleared-and-undecided row is an \
                 ABORTED attempt and an abort is never decided afterwards"
            )));
        }
        let already: Option<i64> = tx
            .query_row(
                "SELECT seq FROM transactions WHERE hash = ?1 AND decided = 1 AND seq <> ?2",
                params![hex, seq.0],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(other) = already {
            return Err(SpineError::usage(format!(
                "mark_decided: hash identity {hex} is already decided at seq {other}; at most one \
                 attempt per identity may ever be decided — the decided flip IS the commit point \
                 (exactly-once)"
            )));
        }
        // THE SERIAL MINT, in this same transaction: every
        // provisional `<prefix>-~...` the payload carries becomes
        // `<prefix>-<n>` from the (pool, prefix) counter, and the row's
        // payload and lock rows are rewritten to match before the flip
        // commits. See `ID_SERIALS_DDL` for why here and nowhere earlier.
        assign_serials(&tx, seq)?;
        tx.execute(
            "UPDATE transactions SET decided = 1 WHERE seq = ?1",
            params![seq.0],
        )?;
        // THE HOLD, lifted by the commit that ends it: in this same
        // transaction as the decided flip, so there is no moment at which
        // the hold is gone and its commit is not (`Cauldron::hold`).
        {
            if let Some(hold) = &lift {
                tx.execute(
                    "UPDATE spine_state SET halted = 0, halted_reason = NULL WHERE singleton = 1 AND halted = 1 AND halted_reason = ?1",
                    params![hold],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// THE DOOR's decided flip:
    /// the same guarded flip as [`Self::mark_decided`], answering `false`
    /// instead of refusing when the row is ALREADY CLEARED — which, for an
    /// un-decided row, means a `recover()` running beside the live door
    /// aborted its attempt under it. That is not the caller's misuse of the
    /// API and it is not a refusal of the statement; it is a lost race the
    /// door answers by RESTARTING with a fresh row (the restart
    /// shape). `mark_decided`'s named `Usage` refusal stays for a by-hand
    /// caller; the door never reaches it.
    pub(crate) fn try_mark_decided(&mut self, seq: Seq) -> Result<bool> {
        let cleared: Option<i64> = self
            .conn
            .query_row(
                "SELECT cleared FROM transactions WHERE seq = ?1",
                params![seq.0],
                |r| r.get(0),
            )
            .optional()?;
        match cleared {
            None => Err(SpineError::not_found(format!(
                "try_mark_decided: no transaction row at seq {seq}"
            ))),
            Some(c) if c != 0 => Ok(false),
            Some(_) => {
                // The read above and the flip are two statements; the flip's
                // own IMMEDIATE transaction re-reads `cleared` under the
                // write lock, so an abort landing between the two is seen
                // there and answered the same way rather than refused.
                match self.mark_decided_interior(seq) {
                    Ok(()) => Ok(true),
                    Err(SpineError::Usage { .. }) => {
                        let now: i64 = self.conn.query_row(
                            "SELECT cleared FROM transactions WHERE seq = ?1",
                            params![seq.0],
                            |r| r.get(0),
                        )?;
                        if now != 0 {
                            Ok(false)
                        } else {
                            Err(SpineError::hard(format!(
                                "try_mark_decided: seq {seq} refused the decided flip while \
                                 un-cleared; another attempt at its identity is already decided"
                            )))
                        }
                    }
                    Err(e) => Err(e),
                }
            }
        }
    }

    /// Is this row decided? Interior helper for the lifecycle guards below.
    fn is_decided(tx: &rusqlite::Transaction<'_>, seq: Seq) -> Result<Option<bool>> {
        let d: Option<i64> = tx
            .query_row(
                "SELECT decided FROM transactions WHERE seq = ?1",
                params![seq.0],
                |r| r.get(0),
            )
            .optional()?;
        Ok(d.map(|d| d != 0))
    }

    /// Record one part landed. When this is the commit's LAST part, the
    /// same spine transaction increments the committed-commit counter —
    /// that coupling is the exposure's contract (the counter
    /// increments only when a commit's last part has landed).
    ///
    /// Idempotent for recovery (run it twice, same result): a
    /// part already landed is detected and NOT re-flipped, so the counter
    /// can never increment twice for one commit.
    ///
    /// LIFECYCLE GUARD: a part
    /// may not land on an UNDECIDED row. The decided marker is flipped
    /// "before the first pool landing" and it is what recovery
    /// reads to choose abort versus roll-forward — a part landed
    /// under an undecided row would make recovery abort a transaction whose
    /// effects exist. The flips are a state machine; out-of-order flips are
    /// loud, never silent.
    fn mark_part_landed_interior(&mut self, seq: Seq, part: u32) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        match Self::is_decided(&tx, seq)? {
            None => {
                return Err(SpineError::not_found(format!(
                    "mark_part_landed: no transaction row at seq {seq}"
                )));
            }
            Some(false) => {
                return Err(SpineError::usage(format!(
                    "mark_part_landed: seq {seq} is not decided; the commit-decided marker is \
                     flipped BEFORE the first pool landing"
                )));
            }
            Some(true) => {}
        }
        let landed: Option<i64> = tx
            .query_row(
                "SELECT landed FROM tx_parts WHERE seq = ?1 AND part = ?2",
                params![seq.0, part as i64],
                |r| r.get(0),
            )
            .optional()?;
        match landed {
            None => {
                return Err(SpineError::not_found(format!(
                    "mark_part_landed: no part {part} for seq {seq}"
                )));
            }
            Some(l) if l != 0 => {
                // Already landed: recovery re-applying is detected, not
                // repeated; the counter stays untouched.
                tx.commit()?;
                return Ok(());
            }
            Some(_) => {}
        }
        tx.execute(
            "UPDATE tx_parts SET landed = 1 WHERE seq = ?1 AND part = ?2",
            params![seq.0, part as i64],
        )?;
        let unlanded: i64 = tx.query_row(
            "SELECT COUNT(*) FROM tx_parts WHERE seq = ?1 AND landed = 0",
            params![seq.0],
            |r| r.get(0),
        )?;
        if unlanded == 0 {
            // The commit's LAST part just landed: increment the
            // committed-commit counter atomically with this flip — and
            // stamp the value it reached onto THIS row, in the same
            // transaction (the third exposure). The stamp is read
            // back from the counter AFTER the increment, so "committed at
            // N" means exactly "the counter read N once this commit was
            // counted": a reader whose window is (c_start, c_end] finds
            // this commit if and only if its counter reads straddled it.
            tx.execute(
                "UPDATE counters SET committed_commits = committed_commits + 1
                 WHERE singleton = 1",
                [],
            )?;
            let reached: i64 = tx.query_row(
                "SELECT committed_commits FROM counters WHERE singleton = 1",
                [],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE transactions SET committed_at = ?2 WHERE seq = ?1",
                params![seq.0, reached],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Flip the row to cleared and store the outcome's RETURN projection in
    /// `result` (so a duplicate submission can return the ORIGINAL outcome
    /// — exactly-once). The caller releases locks at exactly
    /// this flip — and ONLY behind a flip it WON (below).
    ///
    /// LIFECYCLE GUARD: a
    /// DECIDED row may be cleared only when EVERY part has landed — a
    /// transaction's locks release when every part in every pool has landed
    /// and the spine flips the row to cleared. Clearing a
    /// decided row early would lie to both exposures at once: it would drop
    /// the transaction out of `in_landing` while a pool part is still
    /// missing, and it would release locks over a window that must
    /// be invisible. An UNDECIDED row may always be cleared — that is the
    /// ABORT path (nothing landed, by the per-pool atomic step), and the row
    /// remains forever as the record of the attempt.
    ///
    /// THE CLEARED-FLIP ARBITER. The flip is CONDITIONAL — `WHERE seq = ? AND cleared = 0` —
    /// and answers whether it happened. `true`: this caller cleared the row;
    /// its `result` and `recovered` are the record, and the caller releases
    /// the row's locks. `false`: the row was ALREADY cleared by the other
    /// party that can finish a decided row (the live door, or a `recover()`
    /// running beside it); NOTHING was written, the first flip's stored
    /// outcome stands, its audit mark stands, and the caller must NOT
    /// release locks it did not arbitrate — it reads the stored outcome and
    /// returns it as the original. It used to be a bare UPDATE, so a door
    /// resuming after recovery had rolled its row forward re-projected
    /// RETURN under nobody's lock, overwrote the stored result with a foreign
    /// value, and wrote `recovered = 0` over recovery's mark. Same shape as
    /// the abort arbiter (`mark_aborted_if_undecided`), applied
    /// to the other arm.
    fn mark_cleared_interior(&mut self, seq: Seq, result: &serde_json::Value) -> Result<bool> {
        self.mark_cleared_with(seq, result, false)
    }

    /// RECOVERY's cleared flip: the same
    /// guarded, ARBITRATED flip as [`Self::mark_cleared`] — every part
    /// landed, or refuse; `false` when the live door cleared first — storing
    /// the RETURN projection recovery computed, and setting the `recovered`
    /// audit mark in the same atomic step. Crate-interior: only the
    /// roll-forward path clears a row this way.
    pub(crate) fn mark_cleared_recovered(
        &mut self,
        seq: Seq,
        result: &serde_json::Value,
    ) -> Result<bool> {
        self.mark_cleared_with(seq, result, true)
    }

    fn mark_cleared_with(
        &mut self,
        seq: Seq,
        result: &serde_json::Value,
        recovered: bool,
    ) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        match Self::is_decided(&tx, seq)? {
            None => {
                return Err(SpineError::not_found(format!(
                    "mark_cleared: no transaction row at seq {seq}"
                )));
            }
            Some(true) => {
                let unlanded: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM tx_parts WHERE seq = ?1 AND landed = 0",
                    params![seq.0],
                    |r| r.get(0),
                )?;
                if unlanded != 0 {
                    return Err(SpineError::usage(format!(
                        "mark_cleared: seq {seq} is decided with {unlanded} part(s) not yet \
                         landed; the cleared flip comes only when every part in every pool has \
                         landed"
                    )));
                }
            }
            Some(false) => {}
        }
        let changed = tx.execute(
            "UPDATE transactions SET cleared = 1, result = ?2, recovered = ?3
             WHERE seq = ?1 AND cleared = 0",
            params![seq.0, to_json(result)?, recovered as i64],
        )?;
        tx.commit()?;
        Ok(changed == 1)
    }

    /// The ABORT arbiter: flip this row to cleared IF AND ONLY
    /// IF it is still un-decided and un-cleared, in one atomic step, and say
    /// whether the flip happened. `true` — the abort won; the caller may now
    /// release the row's locks, because no landing can ever begin (a cleared
    /// row is never decided afterwards, [`Self::mark_decided`]'s guard).
    /// `false` — someone decided (or already cleared) this row between the
    /// caller's read and this call, and NOTHING was changed: the caller must
    /// NOT release locks it no longer arbitrates.
    ///
    /// Why this exists: recovery's abort path
    /// used to release the row's locks FIRST and only then flip the row,
    /// with no atomic decided-check anywhere in between — so a `recover()`
    /// racing a live door could read a row un-decided, strip the locks the
    /// door was landing under after the door's decided flip, and leak the
    /// half-landed window to lock-takers (the release rule broken
    /// exactly where it is load-bearing). The abort is now this single
    /// conditional UPDATE, and lock release follows only a won abort.
    fn mark_aborted_if_undecided_interior(
        &mut self,
        seq: Seq,
        result: &serde_json::Value,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE transactions SET cleared = 1, result = ?2
             WHERE seq = ?1 AND decided = 0 AND cleared = 0",
            params![seq.0, to_json(result)?],
        )?;
        Ok(changed == 1)
    }

    /// Look a transaction up by hash identity — the deduplication read
    /// ("did this already land" is answerable from the record
    /// with no new machinery).
    /// One identity may now name SEVERAL rows — one per attempt (see the DDL
    /// docs). THE AUTHORITATIVE ONE, in this order: the DECIDED row if there
    /// is one (at most one can ever exist, and it is the commit); else the
    /// UN-CLEARED row if there is one (at most one at a time, and it is the
    /// attempt in flight); else the LATEST attempt, which is the most recent
    /// abort. Every caller that asks "what happened to this identity" wants
    /// exactly that row, and the ordering is what makes the answer stable.
    fn authoritative_seq(&self, hash: &TxHash) -> Result<Option<i64>> {
        let hex = hash.to_hex();
        for sql in [
            "SELECT seq FROM transactions WHERE hash = ?1 AND decided = 1",
            "SELECT seq FROM transactions WHERE hash = ?1 AND cleared = 0",
            "SELECT seq FROM transactions WHERE hash = ?1 ORDER BY attempt DESC LIMIT 1",
        ] {
            if let Some(seq) = self
                .conn
                .query_row(sql, params![hex], |r| r.get::<_, i64>(0))
                .optional()?
            {
                return Ok(Some(seq));
            }
        }
        Ok(None)
    }

    /// Every row this identity has ever produced, in attempt order — the
    /// aborted attempts and, if it got there, the commit. The audit view of a
    /// restarted transaction (an aborted transaction still leaves
    /// a row, and the abort is itself a recorded fact).
    pub fn attempts_of(&self, hash: &TxHash) -> Result<Vec<TxRow>> {
        let seqs: Vec<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT seq FROM transactions WHERE hash = ?1 ORDER BY attempt")?;
            let rows = stmt.query_map(params![hash.to_hex()], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        seqs.into_iter().map(|s| self.read_row(Seq(s))).collect()
    }

    /// One row by its sequence number. The door needs this to abort the exact
    /// attempt it opened: with several attempts per identity, "the row for
    /// this hash" is no longer the same question as "the row I appended".
    pub fn row_at(&self, seq: Seq) -> Result<TxRow> {
        self.read_row(seq)
    }

    /// Look a transaction up by hash identity — the deduplication read
    /// ("did this already land" is answerable from the record
    /// with no new machinery). Returns [`Self::authoritative_seq`]'s row: the
    /// decided one if there is one, else the un-cleared one, else the latest
    /// abandoned attempt. `None` only when the identity has no row at all.
    ///
    /// The CALLER decides what an aborted row means — `exec::dedup_outcome`
    /// reads it as "no verdict yet, proceed with a fresh attempt". This
    /// method reports the record; it does not interpret it.
    pub fn find_by_hash(&self, hash: &TxHash) -> Result<Option<TxRow>> {
        match self.authoritative_seq(hash)? {
            None => Ok(None),
            Some(s) => Ok(Some(self.read_row(Seq(s))?)),
        }
    }

    /// The live door's OTHER direction: the non-terminal
    /// tranche, if any, holding `hash` as a STAGED child — `(T, attempt)`.
    /// The ordinary door's dedup phase answers `busy` for it, naming the
    /// tranche as the holder, exactly as it does for an un-cleared row: the
    /// step is in flight, inside the tranche. A child of a STORED (terminal)
    /// tranche is not a live identity and is not answered here.
    pub fn staged_child_of_live_tranche(&self, hash: &TxHash) -> Result<Option<(String, u32)>> {
        let found: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT c.tranche_id, c.tranche_attempt
                 FROM tranche_children c
                 JOIN tranches t ON t.id = c.tranche_id AND t.attempt = c.tranche_attempt
                 WHERE c.hash = ?1 AND t.state IN ('queued','open','staging','landing')
                 LIMIT 1",
                params![hash.to_hex()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(found.map(|(id, n)| (id, n as u32)))
    }

    /// Every row not yet cleared, in sequence order — recovery's worklist
    /// and the clearing-order surface.
    pub fn pending(&self) -> Result<Vec<TxRow>> {
        let seqs: Vec<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT seq FROM transactions WHERE cleared = 0 ORDER BY seq")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        seqs.into_iter().map(|s| self.read_row(Seq(s))).collect()
    }

    // ---- the two exposures (facts the spine already owns, required only
    //      to be cheaply readable; the validated read is built on these —
    //      plus the THIRD, `commits_touching`, below, without which its
    //      window check cannot be read strictly) ----

    /// The committed-commit counter: increments only when a commit's LAST
    /// part has landed.
    pub fn committed_commit_counter(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT committed_commits FROM counters WHERE singleton = 1",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// One commit's touched-pool set — an exposure, read as the
    /// RECORDED part set: every pool a part validated under, delta-bearing
    /// or not (recovery needs all of them). The invalidation-facing set —
    /// delta-bearing parts only — is `TxRow::touched` / `InLanding::touched`
    /// and is what `commits_touching` filters on.
    pub fn touched_pools(&self, hash: &TxHash) -> Result<Vec<PoolName>> {
        let Some(seq) = self.authoritative_seq(hash)? else {
            return Err(SpineError::not_found(format!(
                "touched_pools: no transaction with hash {}",
                hash.to_hex()
            )));
        };
        self.pools_of(Seq(seq))
    }

    /// The set of transactions currently in landing state (decided, not
    /// cleared), each with its recorded part pools AND its touched set —
    /// queryable by pool (`pool: None` = all). The load-bearing half of
    /// read validation.
    ///
    /// "Queryable by pool" filters on the TOUCHED set: the
    /// question a reader asks is "could this landing change what I see in
    /// pool P", and a premise-only part cannot. The same definition the
    /// third exposure uses — one rule, two consumers.
    pub fn in_landing(&self, pool: Option<&PoolName>) -> Result<Vec<InLanding>> {
        let landing: Vec<(i64, String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT seq, hash, touched FROM transactions
                 WHERE decided = 1 AND cleared = 0 ORDER BY seq",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut out = Vec::new();
        for (seq, hex, touched) in landing {
            let touched: Vec<PoolName> = from_json(&touched, Seq(seq), "touched")?;
            if let Some(filter) = pool {
                if !touched.contains(filter) {
                    continue;
                }
            }
            let pools = self.pools_of(Seq(seq))?;
            let hash = TxHash::from_hex(&hex).ok_or_else(|| {
                SpineError::hard(format!(
                    "transaction row seq {seq} carries an unparseable hash: {hex}"
                ))
            })?;
            out.push(InLanding {
                hash,
                seq: Seq(seq),
                pools,
                touched,
            });
        }
        Ok(out)
    }

    // ---- the THIRD exposure ----

    /// The commits whose committed-at value lies in `(c_start, c_end]` and
    /// which TOUCH at least one of `pools` — in committed-at order.
    ///
    /// This exists because the validated read's window check ("no commit in
    /// the window touched YOUR pools") is unimplementable from the other two
    /// exposures: the counter says how many commits landed, never which. The
    /// spine already owns the fact (every row records its parts' pools; the
    /// counter increments in the same spine transaction as the last part's
    /// landed flip), and owned facts need only be cheaply
    /// readable. ONE indexed query: the range over `idx_tx_committed_at`,
    /// with the pool membership test on the row's own `touched` set.
    ///
    /// `pools` is the READER's pool set; a pool the cauldron does not hold
    /// simply matches nothing. `c_start >= c_end` is the empty window.
    pub fn commits_touching(
        &self,
        pools: &[PoolName],
        c_start: u64,
        c_end: u64,
    ) -> Result<Vec<CommittedCommit>> {
        if c_start >= c_end || pools.is_empty() {
            return Ok(Vec::new());
        }
        let wanted = to_json(&pools)?;
        let mut stmt = self.conn.prepare(
            "SELECT t.seq, t.hash, t.committed_at FROM transactions t
             WHERE t.committed_at > ?1 AND t.committed_at <= ?2
               AND EXISTS (SELECT 1 FROM json_each(t.touched) je
                           WHERE je.value IN (SELECT value FROM json_each(?3)))
             ORDER BY t.committed_at",
        )?;
        let rows = stmt.query_map(params![c_start as i64, c_end as i64, wanted], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, hex, at) = row?;
            let hash = TxHash::from_hex(&hex).ok_or_else(|| {
                SpineError::hard(format!(
                    "transaction row seq {seq} carries an unparseable hash: {hex}"
                ))
            })?;
            out.push(CommittedCommit {
                seq: Seq(seq),
                hash,
                committed_at: at as u64,
            });
        }
        Ok(out)
    }

    // ---- fail-closed state and quiesce ----

    /// The halted reason, if the spine is in the fail-closed halted state.
    /// Durable and
    /// cross-process — a halted spine stays halted until repaired.
    pub fn halted(&self) -> Result<Option<String>> {
        let (halted, reason): (i64, Option<String>) = self.conn.query_row(
            "SELECT halted, halted_reason FROM spine_state WHERE singleton = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if halted == 0 || self.owns_hold(reason.as_deref()) {
            return Ok(None);
        }
        Ok(Some(reason.unwrap_or_else(|| {
            "halted with no recorded reason".to_string()
        })))
    }

    fn owns_hold(&self, reason: Option<&str>) -> bool {
        self.hold.is_some() && self.hold.as_deref() == reason
    }

    /// Install the hold this connection's handle owns, and whether its next
    /// decided flip lifts it ([`crate::Cauldron::hold`]).
    pub(crate) fn with_hold(mut self, hold: Option<String>, lift: bool) -> SpineDb {
        self.hold = hold;
        self.lift_hold = lift;
        self
    }

    /// The raw halted state (`Some(reason)` when halted), whoever's it is.
    pub(crate) fn halted_raw(&self) -> Result<Option<String>> {
        let (halted, reason): (i64, Option<String>) = self.conn.query_row(
            "SELECT halted, halted_reason FROM spine_state WHERE singleton = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((halted != 0).then(|| reason.unwrap_or_default()))
    }

    /// Set the hold: halted with `reason`, in one IMMEDIATE transaction,
    /// only when the spine is not halted already (or already holds exactly
    /// `reason`). Answers whether it is now held with `reason`.
    pub(crate) fn set_hold(&mut self, reason: &str) -> Result<bool> {
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (halted, current): (i64, Option<String>) =
            tx.query_row("SELECT halted, halted_reason FROM spine_state WHERE singleton = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        if halted != 0 {
            return Ok(current.as_deref() == Some(reason));
        }
        tx.execute("UPDATE spine_state SET halted = 1, halted_reason = ?1 WHERE singleton = 1", params![reason])?;
        tx.commit()?;
        Ok(true)
    }

    /// Lift the hold `reason`, only if it is the one set. Answers whether it
    /// was set (and so is now lifted).
    pub(crate) fn clear_hold(&mut self, reason: &str) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE spine_state SET halted = 0, halted_reason = NULL WHERE singleton = 1 AND halted = 1 AND halted_reason = ?1",
            params![reason],
        )?;
        Ok(n > 0)
    }

    /// Enter the fail-closed halt: effectively read-only, announced
    /// best-effort, no retry-forever, no silent degradation.
    ///
    /// VISIBILITY (the one-door law): `pub` only under
    /// `test-support`; a consumer halts nothing by hand.
    #[cfg(feature = "test-support")]
    pub fn set_halted(&mut self, reason: &str) -> Result<()> {
        self.set_halted_interior(reason)
    }
    #[cfg(not(feature = "test-support"))]
    pub(crate) fn set_halted(&mut self, reason: &str) -> Result<()> {
        self.set_halted_interior(reason)
    }

    fn set_halted_interior(&mut self, reason: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE spine_state SET halted = 1, halted_reason = ?1 WHERE singleton = 1",
            params![reason],
        )?;
        Ok(())
    }

    /// A QUIESCE BY HAND (test-support only): `true` records an anonymous
    /// quiescer nobody's handle holds; `false` lifts EVERY quiescer — the
    /// out-of-band repair for a quiescer whose process died holding its
    /// nonce, and the only way to lift a by-hand one. The sanctioned hook is
    /// `Cauldron::quiesce`/`resume`, which owns what it lifts.
    ///
    /// VISIBILITY (the one-door law): `pub` only under
    /// `test-support`; crate-private otherwise, and nothing
    /// in the crate calls it.
    #[cfg(feature = "test-support")]
    pub fn set_quiesced(&mut self, quiesced: bool) -> Result<()> {
        self.set_quiesced_interior(quiesced)
    }
    #[cfg(not(feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn set_quiesced(&mut self, quiesced: bool) -> Result<()> {
        self.set_quiesced_interior(quiesced)
    }

    fn set_quiesced_interior(&mut self, quiesced: bool) -> Result<()> {
        if quiesced {
            let nonce = format!(
                "by-hand:{}:{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            self.add_quiescer(&nonce, "by-hand")?;
        } else {
            self.conn.execute("DELETE FROM quiescers", [])?;
        }
        Ok(())
    }

    /// Record one outstanding quiesce under `nonce`.
    /// From this statement's commit no new commit's append passes the gate.
    pub(crate) fn add_quiescer(&mut self, nonce: &str, holder: &str) -> Result<()> {
        let since = now(&self.conn)?;
        self.conn.execute(
            "INSERT INTO quiescers (nonce, holder, since) VALUES (?1, ?2, ?3)",
            params![nonce, holder, since],
        )?;
        Ok(())
    }

    /// Lift the quiesce recorded under `nonce`; `false` when no such
    /// quiescer is outstanding (nothing was lifted).
    pub(crate) fn remove_quiescer(&mut self, nonce: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM quiescers WHERE nonce = ?1", params![nonce])?;
        Ok(n == 1)
    }

    /// Is the quiesce recorded under `nonce` still outstanding? The drain
    /// loop re-checks its own before it answers `Ok`.
    pub(crate) fn holds_quiescer(&self, nonce: &str) -> Result<bool> {
        let hit: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM quiescers WHERE nonce = ?1",
                params![nonce],
                |r| r.get(0),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    /// How many quiesces are outstanding; the cauldron is quiesced while
    /// this is above zero.
    pub(crate) fn quiescer_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM quiescers", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Every outstanding quiescer — nonce, holder, since — oldest first.
    /// What a quiesced refusal names and what `resume_all` reports.
    pub(crate) fn quiescers(&self) -> Result<Vec<Quiescer>> {
        quiescers_in(&self.conn)
    }

    /// Lift EVERY outstanding quiescer in one atomic step and hand back
    /// exactly the rows that stood — listed and deleted inside one IMMEDIATE
    /// transaction, so a quiesce recorded between the two cannot be lifted
    /// unreported or reported unlifted. The by-hand repair's one write
    /// (`recover::resume_all`); nothing else in the crate calls it.
    pub(crate) fn take_all_quiescers(&mut self) -> Result<Vec<Quiescer>> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let standing = quiescers_in(&tx)?;
        tx.execute("DELETE FROM quiescers", [])?;
        tx.commit()?;
        Ok(standing)
    }

    // ---- interior readers ----

    /// One row's touched pools, in part order (one part per pool by
    /// construction, so no dedup is needed).
    fn pools_of(&self, seq: Seq) -> Result<Vec<PoolName>> {
        let mut stmt = self
            .conn
            .prepare("SELECT pool FROM tx_parts WHERE seq = ?1 ORDER BY part")?;
        let rows = stmt.query_map(params![seq.0], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for p in rows {
            out.push(PoolName::new(p?));
        }
        Ok(out)
    }

    /// Read one full row plus its parts. A row that will not parse is a
    /// loud [`SpineError::Hard`], never a silently partial record.
    fn read_row(&self, seq: Seq) -> Result<TxRow> {
        #[allow(clippy::type_complexity)]
        let (hex, attempt, ts, envelope_version, initiator, instrument, statement, bindings, payload, result, decided, cleared, recovered, committed_at, touched): (
            String,
            i64,
            String,
            i64,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            i64,
            i64,
            i64,
            Option<i64>,
            String,
        ) = self.conn.query_row(
            "SELECT hash, attempt, ts, envelope_version, initiator, instrument, statement,
                    bindings, payload, result, decided, cleared, recovered, committed_at,
                    touched
             FROM transactions WHERE seq = ?1",
            params![seq.0],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                    r.get(13)?,
                    r.get(14)?,
                ))
            },
        )?;
        let hash = TxHash::from_hex(&hex).ok_or_else(|| {
            SpineError::hard(format!(
                "transaction row seq {seq} carries an unparseable hash: {hex}"
            ))
        })?;
        let parts: Vec<PartRow> = {
            let mut stmt = self.conn.prepare(
                "SELECT part, pool, schema_version, landed FROM tx_parts
                 WHERE seq = ?1 ORDER BY part",
            )?;
            let rows = stmt.query_map(params![seq.0], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (part, pool, schema_version, landed) = row?;
                out.push(PartRow {
                    part: part as u32,
                    pool: PoolName::new(pool),
                    schema_version: schema_version as u32,
                    landed: landed != 0,
                });
            }
            out
        };
        let tranche = self.conn.query_row(
            "SELECT tranche_id, tranche_attempt, tranche_ordinal, tranche_index, tranche_of
             FROM transactions WHERE seq = ?1",
            params![seq.0],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            },
        )?;
        let tranche = match tranche {
            (None, ..) => None,
            (Some(id), Some(attempt), ordinal, index, of) => Some(TrancheMark {
                id,
                attempt: attempt as u32,
                ordinal: ordinal.map(|n| n as u32),
                index: index.map(|n| n as u32),
                of: of.map(|n| n as u32),
            }),
            (Some(id), None, ..) => {
                return Err(SpineError::hard(format!(
                    "transaction row seq {seq} carries tranche_id `{id}` and no tranche_attempt"
                )))
            }
        };
        Ok(TxRow {
            seq,
            hash,
            attempt: attempt as u32,
            timestamp: ts,
            envelope_version: envelope_version as u32,
            initiator: from_json(&initiator, seq, "initiator")?,
            instrument: from_json(&instrument, seq, "instrument")?,
            statement,
            bindings: from_json(&bindings, seq, "bindings")?,
            payload: from_json(&payload, seq, "payload")?,
            result: match result {
                None => None,
                Some(r) => Some(from_json(&r, seq, "result")?),
            },
            decided: decided != 0,
            cleared: cleared != 0,
            recovered: recovered != 0,
            committed_at: committed_at.map(|n| n as u64),
            touched: from_json(&touched, seq, "touched")?,
            parts,
            tranche,
        })
    }
}

/// Does `table` carry a column named `column`? (`PRAGMA table_info`.)
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for n in names {
        if n? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The pools a payload TOUCHES for invalidation purposes: every
/// part carrying at least one delta, in part order, deduplicated. The ONE
/// definition — written into the row at append, read by `in_landing` and
/// by the third exposure. A premise-only part is excluded here and nowhere
/// else: `tx_parts` still records it with its validated-under stamp.
/// The serial mint, inside the decided flip's transaction (see
/// [`ID_SERIALS_DDL`]). Reads the row's payload, numbers its provisional
/// ids in [`CompiledPayload::provisional_mints`] order from the (pool,
/// prefix) counters, advances the counters, and writes back the rewritten
/// payload and the row's lock-table resource keys (a provisional id is a
/// fixed-width hash text, so a key naming one names it whole). A payload
/// with no provisional id is left byte-for-byte untouched. Before any of
/// that, every CARRIED serial ([`CompiledPayload::carried_serials`], an
/// IMPORT's id) advances its counter to `max(last, n)`.
fn assign_serials(tx: &rusqlite::Transaction<'_>, seq: Seq) -> Result<()> {
    let text: String = tx.query_row(
        "SELECT payload FROM transactions WHERE seq = ?1",
        params![seq.0],
        |r| r.get(0),
    )?;
    let mut payload: CompiledPayload = from_json(&text, seq, "payload")?;
    let map = mint_serials(tx, std::slice::from_ref(&payload), seq)?;
    if map.is_empty() {
        return Ok(());
    }
    for (provisional, serial) in &map {
        tx.execute(
            "UPDATE locks SET resource = replace(resource, ?1, ?2)
             WHERE seq = ?3 AND instr(resource, ?1) > 0",
            params![provisional.id.0, serial.0, seq.0],
        )?;
    }
    payload.assign_ids(&map);
    tx.execute(
        "UPDATE transactions SET payload = ?1 WHERE seq = ?2",
        params![to_json(&payload)?, seq.0],
    )?;
    Ok(())
}

/// THE ONE SERIAL MINT over a SET of payloads (serial numbering generalised
/// to a set of payloads through one map). Inside the caller's transaction — the
/// ordinary door's decided flip ([`assign_serials`], one payload) or the
/// tranche's decision (`exec::land`, every landing child in ordinal order):
///
/// 1. every CARRIED serial of every payload advances its (pool, prefix)
///    counter to `max(last, n)` — before any
///    provisional is numbered, so the serials drawn below land ABOVE every
///    carried id of their prefix;
/// 2. every PROVISIONAL mint, in `payloads` order and within a payload in
///    [`CompiledPayload::provisional_mints`] order, draws `last + 1` from
///    its counter and advances it.
///
/// Returns ONE map, provisional → serial, over the whole set; the caller
/// rewrites its payloads (`assign_ids`), bound identities and lock-row keys
/// through it. Empty when nothing is minted (the counters are then never
/// read). Nothing is written to `transactions` here: which rows carry which
/// payload is the caller's.
pub(crate) fn mint_serials(
    tx: &rusqlite::Transaction<'_>,
    payloads: &[CompiledPayload],
    seq: Seq,
) -> Result<std::collections::BTreeMap<crate::pool::QualifiedNodeId, crate::pool::NodeId>> {
    let mut map = std::collections::BTreeMap::new();
    let mints: Vec<_> = payloads.iter().flat_map(|p| p.provisional_mints()).collect();
    let carried: Vec<_> = payloads.iter().flat_map(|p| p.carried_serials()).collect();
    if mints.is_empty() && carried.is_empty() {
        return Ok(map);
    }
    tx.execute_batch(ID_SERIALS_DDL)?;
    // CARRIED serials FIRST: every id an IMPORT
    // landed advances its (pool, prefix) counter to `max(last, n)` — never
    // down — in this same transaction, so the provisionals numbered below
    // land ABOVE every carried id of their prefix, and a refused or aborted
    // import advances nothing (the flip is the commit point). A
    // carried id is already its serial: no payload or lock rewrite for it.
    for (pool, prefix, n) in &carried {
        let n = i64::try_from(*n).map_err(|_| {
            SpineError::hard(format!(
                "transaction row seq {seq}: carried serial {prefix}-{n} exceeds the counter's range"
            ))
        })?;
        tx.execute(
            "INSERT INTO id_serials (pool, prefix, last) VALUES (?1, ?2, ?3)
             ON CONFLICT (pool, prefix) DO UPDATE SET last = max(last, excluded.last)",
            params![pool.as_str(), prefix, n],
        )?;
    }
    for (pool, prefix, provisional) in mints {
        let key = crate::pool::QualifiedNodeId {
            pool: pool.clone(),
            id: provisional,
        };
        // One provisional named by two payloads (a later child's reference
        // to an earlier child's mint) is ONE node: numbered once.
        if map.contains_key(&key) {
            continue;
        }
        let last: i64 = tx
            .query_row(
                "SELECT last FROM id_serials WHERE pool = ?1 AND prefix = ?2",
                params![pool.as_str(), prefix],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let next = last + 1;
        tx.execute(
            "INSERT INTO id_serials (pool, prefix, last) VALUES (?1, ?2, ?3)
             ON CONFLICT (pool, prefix) DO UPDATE SET last = excluded.last",
            params![pool.as_str(), prefix, next],
        )?;
        map.insert(key, crate::delta::serial_node_id(&prefix, next as u64));
    }
    Ok(map)
}

fn touched_pools_of(payload: &CompiledPayload) -> Vec<PoolName> {
    let mut out: Vec<PoolName> = Vec::new();
    for part in &payload.parts {
        if !part.deltas.is_empty() && !out.contains(&part.pool) {
            out.push(part.pool.clone());
        }
    }
    out
}

/// Canonical JSON out (compact `serde_json`; maps in these types are
/// `BTreeMap`s, so keys are sorted — the same canonical form the hash
/// identity uses).
fn to_json<T: Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|e| SpineError::hard(format!("serialization failure: {e}")))
}

/// JSON back in; a stored column that will not parse is loud and names its
/// row and column (soup idiom: a store that will not answer is never an
/// empty store).
fn from_json<T: for<'de> Deserialize<'de>>(s: &str, seq: Seq, col: &str) -> Result<T> {
    serde_json::from_str(s).map_err(|e| {
        SpineError::hard(format!(
            "transaction row seq {seq}: stored {col} will not parse: {e}"
        ))
    })
}

/// Which gates an append passes through: a COMMIT's row is
/// refused by the halt gate and by the quiesce gate; an escalated READ's row
/// by the halt gate alone — it exists to be issued a sequence number and is
/// never a commit (see [`SpineDb::append_read_row`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppendGate {
    Commit,
    Read,
}

/// What an exactly-once append found. Returned by
/// [`SpineDb::append_or_existing`]; an ADDITION beside the frozen
/// `append_pending -> Result<Seq>`, a shape that cannot carry the original
/// row back.
#[derive(Debug)]
pub enum AppendOutcome {
    /// A fresh transaction: the row was appended, and this is the sequence
    /// number the spine's own insertion order issued it.
    Appended(Seq),
    /// This hash identity was ALREADY recorded. Nothing was appended; this
    /// is the original row, and `result` is the original outcome (`None`
    /// while that transaction is still in flight — the honest answer, never
    /// a fabricated one).
    Duplicate(Box<TxRow>),
}

/// One transaction row, read back whole — self-describing forever (what
/// was ASKED recorded beside what was done).
#[derive(Debug, Clone)]
pub struct TxRow {
    pub seq: Seq,
    pub hash: TxHash,
    /// Which ATTEMPT at this hash identity this row is, from 1. The restart
    /// rule restarts a statement whose data moved
    /// with a FRESH sequence number, and the abandoned attempt's row is
    /// recorded aborted; this ordinal is what makes a hash's row set read as
    /// the history of one ask rather than as a collision.
    pub attempt: u32,
    /// ISO-8601 UTC. A recorded column; NOT the ordering key.
    pub timestamp: String,
    pub envelope_version: u32,
    pub initiator: crate::envelope::Initiator,
    pub instrument: crate::envelope::Instrument,
    pub statement: String,
    pub bindings: crate::envelope::Bindings,
    pub payload: CompiledPayload,
    /// The stored RETURN projection of a cleared commit, for dedup replies.
    pub result: Option<serde_json::Value>,
    pub decided: bool,
    pub cleared: bool,
    /// True when RECOVERY rolled this row forward and computed its RETURN
    /// projection at the cleared flip. `result` is a real
    /// projection either way; this is the audit fact, kept apart from it.
    pub recovered: bool,
    /// The committed-commit counter's value at this commit — stamped in the
    /// same spine transaction as the last part's landed flip (the third
    /// exposure). `None` until then; `None` forever for an abort.
    pub committed_at: Option<u64>,
    /// The pools this commit TOUCHES for invalidation: its delta-bearing
    /// parts' pools, in part order. A premise-only part is in `parts` (with
    /// its stamp) and not here.
    pub touched: Vec<PoolName>,
    pub parts: Vec<PartRow>,
    /// The row's tranche columns, `None` for every ordinary row.
    /// Carried here so the publisher's row-to-event function needs no second
    /// read.
    pub tranche: Option<TrancheMark>,
}

/// A row's five tranche columns. `ordinal` is `None` on the
/// tranche's ANCHOR row and `Some` on a reserved child row; `index`
/// and `of` are a landing child's position among the children that land and
/// their count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrancheMark {
    pub id: String,
    pub attempt: u32,
    pub ordinal: Option<u32>,
    pub index: Option<u32>,
    pub of: Option<u32>,
}

impl TrancheMark {
    /// The anchor row: a tranche id with no child ordinal.
    pub fn is_anchor(&self) -> bool {
        self.ordinal.is_none()
    }
}

/// One part's bookkeeping row (per-part landed flag, pool,
/// validated-under stamp).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartRow {
    pub part: u32,
    pub pool: PoolName,
    pub schema_version: u32,
    pub landed: bool,
}

/// One in-landing transaction as the validated read consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InLanding {
    pub hash: TxHash,
    pub seq: Seq,
    /// Every recorded part's pool, in part order — the row's own record.
    pub pools: Vec<PoolName>,
    /// The pools this landing TOUCHES (delta-bearing parts only) — what the
    /// validated read's window check tests against, and what
    /// `in_landing(Some(p))` filters on.
    pub touched: Vec<PoolName>,
}

/// One commit as the THIRD exposure reports it: a commit that landed at
/// `committed_at` — the counter's value once it was counted — and touched at
/// least one of the pools asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedCommit {
    pub seq: Seq,
    pub hash: TxHash,
    pub committed_at: u64,
}
