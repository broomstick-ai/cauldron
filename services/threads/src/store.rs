//! SQLite persistence — the thread ledger of record.
//!
//! Ported from CCW `store.rs` (WAL on open, `Arc<Mutex<Connection>>`, embedded
//! schema applied on every open, idempotent event append) and extended with
//! the system-of-record surfaces the operator locked in §10–§12: message-id
//! retrieval (G20), the send queue (Q4), persisted retries (Q5/G10), running
//! turns for re-adoption (G1), tracked accounts (§10), the usage ledger (Q11),
//! and import watermarks (Q8).

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

use crate::accounts::AccountState;
use crate::error::{Error, Result};
use crate::event::{StoredEvent, ThreadEvent};

/// The messaging-network rows (R4.S3). A child module so it shares this
/// store's one write connection and read pool; the rules are `crate::networks`.
pub mod networks;

const SCHEMA_SQL: &str = include_str!("../schema.sql");

/// Idempotent column additions for databases created before a column existed.
/// Applied after the schema batch on every open; a duplicate-column error is
/// the normal outcome and is swallowed (see [`Store::apply_schema`]).
const MIGRATIONS: &[&str] = &[
    // Round-7 item 4: the weekly-fable BUDGET (it was threshold-only).
    "ALTER TABLE budgets ADD COLUMN fable_pct REAL",
    // Round-7 item 5: the CASH axis, in dollars, pooled across accounts.
    "ALTER TABLE budgets ADD COLUMN cash_usd REAL",
    // Round-7 item 6: explicit account scope — never defaulted to "all".
    "ALTER TABLE budgets ADD COLUMN accounts_json TEXT NOT NULL DEFAULT '[]'",
    // Overlap-window calibration (msg #620): the execution's own start
    // instant, so a usage row also names an (started_at, ts) interval.
    "ALTER TABLE usage_ledger ADD COLUMN started_at TEXT",
    // Q-34 (msg #751): a budget key is DISABLED, never deleted. NULL = live.
    "ALTER TABLE budgets ADD COLUMN disabled_at TEXT",
    // Account IDENTITY and HEALTH (account-selection-design §3.1). `identity`
    // is an opaque hash of the login behind the label, so two labels for ONE
    // login stop counting as two accounts; `dead_since`/`dead_reason` record an
    // authentication failure no retry can fix, which takes the account out of
    // selection and failover while leaving it visible in `accounts status`.
    "ALTER TABLE accounts ADD COLUMN identity TEXT",
    "ALTER TABLE accounts ADD COLUMN dead_since TEXT",
    "ALTER TABLE accounts ADD COLUMN dead_reason TEXT",
    // The DOUBLE-COUNT marker. Rows already on disk when this column arrives
    // were written by the cumulative-snapshot writer and are inflated by an
    // unknown factor, so they default to rev 0 and every derived number skips
    // them. History is MARKED, never rewritten — see `schema.sql`.
    "ALTER TABLE usage_ledger ADD COLUMN accounting_rev INTEGER NOT NULL DEFAULT 0",
    // INTENT 65 / DIR-56: the per-message metadata block threads composes
    // itself. `last_meta_date` is the day last STATED to a thread (the date is
    // elided until it rolls over); `queue.metadata_json` carries the caller's
    // blob to delivery, which is where the AUI-supplied audio start instant —
    // and so the duration — is read from.
    "ALTER TABLE threads ADD COLUMN last_meta_date TEXT",
    "ALTER TABLE queue ADD COLUMN metadata_json TEXT",
    // **INTERIM, and owned by the projection work that will delete it.** The
    // context id last STATED to a thread, so that turning `inject_context_id` on
    // mid-context injects the line on the next post instead of waiting for the
    // next compaction. Once per-message metadata is rebuilt at turn start as a
    // PROJECTION, nothing needs to remember what was stated and this column goes
    // with the catch-up in `ThreadManager::build_prompt`.
    "ALTER TABLE threads ADD COLUMN last_context_id TEXT",
    // INTENT 66b: the header DECLARATION an application sent with this
    // message, already validated at accept time. It rides its own column and
    // NOT `metadata_json` on purpose — `metadata_json` is the caller's opaque
    // store-and-return blob, and mixing a structure threads reads and renders
    // into the blob it promises never to read would make DEC-55's invariant
    // ("category (a) is invisible by construction") a thing you have to check
    // rather than a thing that is true.
    "ALTER TABLE queue ADD COLUMN headers_json TEXT",
    // THE FAILURE'S SUBJECT. Added to attribute a retry floor to the login that
    // refused rather than to the thread; the floor itself is gone (see
    // `DROPPED_COLUMNS`) and the attribution outlived it, because a starving
    // delivery still has to be able to name what it is piling up behind.
    "ALTER TABLE retries ADD COLUMN account TEXT",
    // The limit's SUBJECT and its QUOTA. `limit_until` was being read as "this
    // LABEL is barred"; it is really "the login this label pointed at when the
    // 429 arrived is barred". Stamping the identity is what lets a re-pointed
    // label (`~/.claude.json` after a `/login`) stop carrying a limit that
    // belongs to a login it no longer names. `limit_axis` records which quota
    // said no, which the 429 path previously threw away.
    "ALTER TABLE accounts ADD COLUMN limit_identity TEXT",
    "ALTER TABLE accounts ADD COLUMN limit_axis TEXT",
    // THE ADOPTED TURN READS THE PROMPT BACK. `adopt_turn` held a pid and an
    // output file and not the operator's words, so it declined to honour a
    // usage-limit kill and dropped the prompt on a budget kill (CON-4). The
    // words were never actually at risk — they are on the `events` ledger as
    // `user_message` and in `queue.text`, neither of which this crate ever
    // deletes from — so this is a MISSING SELECT, not a data-loss fix. Storing
    // them beside the row the adopter already reads is what makes them reachable
    // from the one place that needs them.
    //
    // `prompt` and `message_ids_json` are NULLABLE deliberately: a row written
    // by the daemon that is running right now must read back as None, and
    // `adopt_turn` must decline exactly as it does today for it.
    "ALTER TABLE running_turns ADD COLUMN prompt TEXT",
    "ALTER TABLE running_turns ADD COLUMN message_ids_json TEXT",
    "ALTER TABLE running_turns ADD COLUMN attempt INTEGER NOT NULL DEFAULT 1",
    // RETIRED 2026-09-20, kept because the migration below reads it once.
    //
    // It held the share a disable DESTROYED, so an enable could put it back.
    // Nothing destroys a share any more: `account_disabled` carries the
    // inclusion decision and `pool_shares.share` is untouched by it, so there
    // is no number to remember and no reader left. The ALTER stays so that a
    // database created between 2026-09-08 and 2026-09-20 still HAS the column
    // for `Store::migrate_share_zero_disable` to recover shares out of; a
    // database created fresh gets it from `schema.sql` and it is NULL forever.
    "ALTER TABLE pool_shares ADD COLUMN prior_share REAL",
    // **THE RECENCY ORDER'S OWN COLUMN.** Operator directive, verbatim: *"the
    // recents need to be ordered on last user input, not on when they were
    // created or any other events, including renames."* `threads.updated_at`
    // fails all three exclusions and was what `recent_threads` ordered by, so
    // the index re-sorted on every append and on every rename. See `schema.sql`
    // for what the column means and `Store::backfill_last_user_input_at` for how
    // rows already on disk get theirs.
    "ALTER TABLE threads ADD COLUMN last_user_input_at TEXT",
];

/// Columns REMOVED from a table that may still carry them on disk. Applied
/// after [`MIGRATIONS`] on every open; "no such column" is the normal outcome
/// from the second open onward and is swallowed (see [`Store::apply_schema`]).
///
/// **`threads.status` — THE DELETED CACHE.** It was a fourth-hand copy of an
/// answer nine other places already derived, and it was WRONG: two arena
/// threads reported `busy` for twenty-five hours after completing. It is not
/// deleted because it drifted; it is deleted because a copy that three writers
/// and four repair mechanisms could not keep correct is a second source of
/// truth, and the operator's rule is that two people asking the same question
/// must never be able to get two different answers.
///
/// Nothing dispatch-critical ever read it — `lock_turn`, `maybe_dispatch` and
/// `admit_or_park` all derive. Its only readers were `thread_json`,
/// `activity_by_account`'s `retrying`/`idle` counters, and AUI's legacy
/// `threads.list` fallback; all three now read
/// [`crate::runtime::ThreadManager::thread_states`].
///
/// **Do not re-add it, or anything shaped like it.** If a status read ever
/// looks expensive, batch the derivation (that is what `thread_states` is —
/// five fleet-wide reads for the whole fleet); do not cache its answer.
/// **`retries.retry_at` — THE PER-THREAD RETRY TIMESTAMP.** It was a FLOOR: the
/// earliest instant it was worth asking the account that refused. The operator
/// ruled it out of existence:
///
/// > *"There should not be a per-thread retry timestamp."*
/// > *"if any thread is parked then the pool needs a retry, not the thread. Any
/// > parked threads get checked at any opportunity."*
///
/// So a row in `retries` is now simply a PARKED PROMPT, and when it is
/// reconsidered is a POOL-level question: [`crate::runtime::PoolCadence`] holds
/// one cadence for the whole pool, and every placement event reconsiders
/// everything. Nothing per-row is read to decide eligibility, which is why the
/// column is gone rather than merely unread — a timestamp nothing reads is still
/// a timestamp somebody will read.
///
/// **Do not re-add it.** The ladder is not lost with it: the rung is
/// `retry::ladder_secs(kind, attempt + pool_barren)`, a pure function of two
/// columns that are still here and one pool counter.
const DROPPED_COLUMNS: &[&str] = &[
    "ALTER TABLE threads DROP COLUMN status",
    "ALTER TABLE retries DROP COLUMN retry_at",
];

/// The accounting shape rows written by THIS build carry: one row per
/// `result_turn` per model, holding the DELTA over that execution's previous
/// snapshot. Rows stamped with anything lower were written by the
/// cumulative-snapshot writer and are not summable.
pub const ACCOUNTING_REV: i64 = 1;

/// A filesystem path as the path part of an SQLite `file:` URI: the three
/// characters the URI grammar gives meaning to (`%`, `?`, `#`) are
/// percent-encoded, everything else passes through.
fn uri_escape_path(path: &Path) -> String {
    let mut out = String::new();
    for c in path.to_string_lossy().chars() {
        match c {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3f"),
            '#' => out.push_str("%23"),
            c => out.push(c),
        }
    }
    out
}

/// **TEST SEAM** (R9.C7h; `test-seams` builds only): `THREADS_MIGRATION_FAULT=
/// sleep:<ms>` stands in for a slow store migration — it is announced through
/// `notice` like a real one and then holds `Store::open` for that long;
/// `quiet-sleep:<ms>` (R9.C7l) holds it as long WITHOUT a notice — a slow
/// open no migration announces; `=hang` never returns. Production builds do not compile it.
#[cfg(any(test, feature = "test-seams"))]
fn migration_fault_seam(notice: &dyn Fn(&'static str)) {
    match std::env::var("THREADS_MIGRATION_FAULT").ok().as_deref() {
        Some("hang") => {
            notice("test seam (hang)");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
        Some(v) => {
            if let Some(ms) = v.strip_prefix("sleep:").and_then(|n| n.trim().parse::<u64>().ok()) {
                notice("test seam (sleep)");
                std::thread::sleep(std::time::Duration::from_millis(ms));
            } else if let Some(ms) =
                v.strip_prefix("quiet-sleep:").and_then(|n| n.trim().parse::<u64>().ok())
            {
                // R9.C7l: a slow store open that is NOT announced — the
                // DEPLOY-9k shape (an unconditional schema pass on a big
                // store), which gets no `migrating` answer.
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
        }
        None => {}
    }
}

fn is_duplicate_column(e: &rusqlite::Error) -> bool {
    e.to_string().contains("duplicate column name")
}

/// A `DROP COLUMN` for a column that is already gone. The success case from the
/// second open onward, and the ONLY case on a database created by the current
/// `schema.sql`.
fn is_missing_column(e: &rusqlite::Error) -> bool {
    e.to_string().contains("no such column")
}

/// **The crate's one door to a timestamp that gets PERSISTED — and clause 1 of
/// the clock seam.**
///
/// It reads [`crate::clock::now`], not `chrono::Utc::now()`, so every one of its
/// callers is on the seam for free, wherever it lives. That is what makes the
/// seam partial without rotting: a new site that needs a timestamp to store
/// calls this and has no decision to make. See `src/clock.rs` for the rule and
/// for why this matters — `usage_ledger.started_at` and `.ts` are two reads of
/// it, and the execution window between them is load-bearing.
pub fn now_rfc3339() -> String {
    crate::clock::now().to_rfc3339()
}

// ── the shape-mismatch surface ───────────────────────────────────────────────

/// How many ledger rows this process could not deserialize and had to degrade.
///
/// A shape mismatch must never be SILENT. This counter is the cheap, always-on
/// half of that rule: it is queryable over `daemon.status`, so "is the daemon
/// reading rows it does not understand, and how many" is answerable without
/// trawling a log file.
static UNREADABLE_LEDGER_ROWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The `kind` column values already reported loudly, so repeats can drop to
/// DEBUG. Keyed by KIND rather than by row: a thousand rows of one unknown kind
/// is one piece of news, and drowning the log is how the last shape mismatch
/// hid for a month.
static WARNED_KINDS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Total ledger rows degraded to [`ThreadEvent::Unknown`] since this process
/// started. `0` means every row read so far was understood.
pub fn unreadable_ledger_rows() -> u64 {
    UNREADABLE_LEDGER_ROWS.load(std::sync::atomic::Ordering::Relaxed)
}

// ── the cost meter ───────────────────────────────────────────────────────────

/// **CONNECTION ACQUISITIONS — WHICH IS NOT THE SAME AS STATEMENTS, AND THIS
/// DOC USED TO SAY IT WAS.**
///
/// Incremented once per [`Store::conn`] and once per [`Store::read`]: one round
/// trip through the serialized writer mutex, or one checkout from the read pool.
/// That is exactly the cost it was built to measure — *"the thing that made
/// status expensive was never wall time, it was N round trips through ONE
/// serialized `Mutex<Connection>`"* — and it measures it correctly.
///
/// **What it does NOT measure is SQL statements.** This doc claimed *"every
/// store method but two runs EXACTLY ONE SQL statement, so this is the statement
/// count"*, and that was false for ELEVEN methods, not two (found 2026-10-02
/// while measuring `summary`'s hydrate, which was two full scans of `events`
/// behind one trip — ~3.5 s each cold — while both this doc and `summary.rs`
/// claimed one statement). A method that runs two statements on one connection
/// adds ONE here, so the counter is a FLOOR on statements and an exact count of
/// round trips.
///
/// The eleven are pinned by `tests/db_trips_metric.rs`, which scrapes this file
/// so the claim cannot drift again, and they are: `apply_schema` (3, at open
/// only), `migrate_share_zero_disable` and `rekey_pool_shares_on_name` (2 each,
/// at open only), `query_messages`, `stranded_deliveries`, `reselect_failed`,
/// `record_turn_usage`, `set_account_disabled`, `account_tokens_between` (2
/// each) and `claim_notice` (4). `mark_seen` is the doc's old second exception
/// and is fine: it acquires twice for two statements, so it is 1:1.
///
/// **What the bounds built on it therefore prove.** Every `db_trips()` test in
/// `tests/` measures ROUND TRIPS exactly. Where it says "statements" it is
/// proven only for methods outside the eleven:
///
/// * `derivation_cost.rs` (5 for `thread_states`), `thread_hydrate_cost.rs`
///   (11 per key), `hydrate_cost.rs` and `recent_hydrate_cost.rs` touch no
///   method in the eleven, so their numbers are statement counts as well as trip
///   counts — but by coincidence of which methods they call, not by anything the
///   counter guarantees.
/// * `budget_meter_cost.rs` (59) reaches `record_turn_usage`'s path, which is one
///   of the eleven, so its FLATNESS property holds and its absolute "59
///   statements" phrasing is unproven. The flatness is what the test is for and
///   a constant undercount cannot hide a per-account regression, which is the
///   assertion that matters there.
///
/// Process-global and free (a relaxed add on a path that is about to take a
/// mutex anyway).
static DB_TRIPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Trips to the database since this process started. See [`DB_TRIPS`].
pub fn db_trips() -> u64 {
    DB_TRIPS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reset the counter — tests only, so one case cannot see another's tally.
#[doc(hidden)]
pub fn reset_unreadable_ledger_rows() {
    UNREADABLE_LEDGER_ROWS.store(0, std::sync::atomic::Ordering::Relaxed);
    *WARNED_KINDS.lock().unwrap() = None;
}

/// Wrap a payload this build cannot parse, preserving it verbatim and counting
/// it. See [`Store::rows_to_events`] for why this degrades rather than fails.
fn degrade_unreadable_row(
    thread_id: &str,
    seq: u64,
    payload: &str,
    err: &serde_json::Error,
) -> ThreadEvent {
    UNREADABLE_LEDGER_ROWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Keep the payload as STRUCTURE when it is still valid JSON (the ordinary
    // case: a well-formed envelope naming a `kind` this build predates), and as
    // a string only when the bytes are not JSON at all. Either way nothing is
    // discarded, so a later build can re-read the same row.
    let raw: Value = match serde_json::from_str::<Value>(payload) {
        Ok(v) => v,
        Err(_) => Value::String(payload.to_string()),
    };
    let kind = raw
        .get("kind")
        .and_then(|k| k.as_str())
        .unwrap_or("<no kind>")
        .to_string();

    let first_of_its_kind = {
        let mut guard = WARNED_KINDS.lock().unwrap();
        guard.get_or_insert_with(HashSet::new).insert(kind.clone())
    };
    if first_of_its_kind {
        tracing::warn!(
            thread = %thread_id,
            seq,
            kind = %kind,
            error = %err,
            total = unreadable_ledger_rows(),
            "ledger row in a shape this build cannot read — kept verbatim as `unknown`, \
             history still served; further rows of this kind log at debug"
        );
    } else {
        tracing::debug!(
            thread = %thread_id, seq, kind = %kind, error = %err,
            "another unreadable ledger row of a kind already reported"
        );
    }
    ThreadEvent::Unknown { raw }
}

/// How long [`Store::ping`] waits for SQLite's write lock while it holds the
/// write mutex (R9.C6b). Everything in-process that needs the write connection
/// waits at most this long behind a heartbeat, against the connection's 5 s.
pub const PING_BUSY_MS: i64 = 250;

/// The store handle. Cheap to clone (wraps `Arc`s).
///
/// **TWO DOORS, AND THE DIFFERENCE IS THE WHOLE POINT.** [`Store::conn`] is the
/// single WRITE connection behind one mutex, exactly as it always was;
/// [`Store::read`] is a pool of READ-ONLY connections that never touches that
/// mutex at all. Everything else about the store is unchanged.
///
/// Why the split exists: on 2026-09-11 a single `SELECT DISTINCT` over a
/// 479,230-row ledger took the whole daemon out of service — a live sample of
/// pid 1337 caught four of ten tokio workers parked in `Store::conn`'s
/// `Mutex::lock` and a fifth holding it inside that query. The query was fixed
/// (6ac979d); the STRUCTURE that turned one slow read into a process-wide
/// outage was not. With one connection behind one mutex, every read is
/// serialized against every other read AND against every write, so the
/// daemon's worst read is also its availability.
///
/// SQLite in WAL mode has no such constraint: readers do not block the writer
/// and the writer does not block readers. The single mutex was imposing a
/// limit the database itself does not have. See [`ReadPool`].
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    /// `None` for an in-memory store, where a second connection would open a
    /// DIFFERENT empty database rather than a second view of this one. Those
    /// stores fall back to the write connection and behave exactly as before.
    reads: Option<Arc<ReadPool>>,
    /// The database file, `None` in memory.
    ///
    /// Held here rather than read back off the connection, because the two
    /// callers that want it — [`Self::wal_bytes`] and
    /// [`Self::checkpoint_for_shutdown`] — are exactly the callers that must
    /// work when the write mutex is WEDGED. `wal_bytes` used to take that
    /// mutex just to ask the connection its own path, which made a
    /// best-effort observability call as available as the thing it was
    /// observing, and put a lock acquisition on the shutdown path one line
    /// above the one that was already the problem.
    path: Option<PathBuf>,
}

/// How many read connections one store will ever open.
///
/// Eight against the daemon's ten runtime workers: enough that the hot paths
/// (the budget meter, the fleet derivation, history) never queue behind each
/// other, and small enough that the per-connection page cache stays a rounding
/// error. It is a CEILING, not an allocation — connections open lazily, so a
/// store that is only ever read from one thread opens exactly one.
const READ_POOL_MAX: usize = 8;

/// **THE READ-ONLY CONNECTION POOL.**
///
/// Read-only by SQLite's own flag, not by convention. That is deliberate and it
/// is the safety net for the one mistake this design can make: routing a
/// statement that writes through [`Store::read`]. Such a statement fails with
/// `SQLITE_READONLY` — loudly, at the first call, under the test suite — rather
/// than quietly landing on a second connection and breaking an atomicity
/// assumption somewhere else.
///
/// **It does not weaken any atomicity that existed.** Every store method is one
/// autocommit statement, or a transaction opened and closed inside that one
/// method on the write connection. Nothing held the mutex ACROSS two store
/// calls, so no read-modify-write sequence was ever protected by it; moving the
/// reads elsewhere takes nothing away.
///
/// **What it costs.** A live reader can make `PRAGMA wal_checkpoint(TRUNCATE)`
/// return busy, which with a single connection was impossible — see
/// [`Store::checkpoint`], where that trade is taken deliberately and reported.
struct ReadPool {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
    /// How many connections exist, idle or checked out. Never exceeds
    /// [`READ_POOL_MAX`].
    opened: AtomicUsize,
    /// Signalled when a connection returns to `idle`.
    returned: Condvar,
}

impl ReadPool {
    fn open_reader(path: &Path) -> Result<Connection> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )?;
        // The same 5s ceiling the writer carries. A reader needs it for the
        // brief WAL-index locks a checkpoint takes.
        conn.pragma_update(None, "busy_timeout", 5000)?;
        Ok(conn)
    }

    /// Take a connection: an idle one, else a newly opened one, else wait for
    /// one to come back. The wait is the ONLY place a reader can block on
    /// another reader, and it takes `READ_POOL_MAX` concurrent slow reads to
    /// reach it — against one, before.
    fn checkout(&self) -> Result<Connection> {
        let mut idle = self
            .idle
            .lock()
            .map_err(|_| Error::Internal("store read pool poisoned".into()))?;
        loop {
            if let Some(c) = idle.pop() {
                return Ok(c);
            }
            // Claim the slot BEFORE dropping the lock, so two threads racing
            // here cannot both decide they are the last one allowed to open.
            if self.opened.load(std::sync::atomic::Ordering::Relaxed) < READ_POOL_MAX {
                self.opened
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                drop(idle);
                return Self::open_reader(&self.path).inspect_err(|_| {
                    self.opened
                        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                });
            }
            idle = self
                .returned
                .wait(idle)
                .map_err(|_| Error::Internal("store read pool poisoned".into()))?;
        }
    }

    fn checkin(&self, conn: Connection) {
        if let Ok(mut idle) = self.idle.lock() {
            idle.push(conn);
            self.returned.notify_one();
        }
        // A poisoned pool drops the connection instead of returning it. The
        // slot is not reclaimed, which is the conservative direction: the pool
        // shrinks rather than handing out a connection nobody is tracking.
    }
}

/// A connection borrowed for READING — either from the pool or, for an
/// in-memory store, the write connection itself. Derefs to [`Connection`] so
/// every call site reads exactly as it did before.
pub struct ReadConn<'a> {
    pooled: Option<(Arc<ReadPool>, Option<Connection>)>,
    writer: Option<MutexGuard<'a, Connection>>,
}

impl Deref for ReadConn<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match (&self.pooled, &self.writer) {
            (Some((_, Some(c))), _) => c,
            (_, Some(g)) => g,
            // Unreachable: exactly one of the two is always populated, and the
            // `None` inside `pooled` only appears during `Drop`.
            _ => unreachable!("a ReadConn always holds a connection"),
        }
    }
}

impl Drop for ReadConn<'_> {
    fn drop(&mut self) {
        if let Some((pool, conn)) = self.pooled.take() {
            if let Some(c) = conn {
                pool.checkin(c);
            }
        }
    }
}

/// A persisted thread row.
///
/// **There is no `status` field, on purpose.** Whether a thread is running is
/// DERIVED, once, by [`crate::runtime::ThreadManager::thread_states`] — see
/// [`DROPPED_COLUMNS`] for what the column was and why keeping it was the bug.
/// A thread row is durable configuration and counters; it is not a liveness
/// report, and nothing should be able to read one out of it.
/// One row of `events`, as [`Store::newest_event`] reads it.
///
/// A struct and not a 5-tuple: `ts` and `payload_json` are both `String`, so a
/// positional swap of the last two would compile and the shape would publish a
/// timestamp's byte count as a message body's size.
#[derive(Debug, Clone)]
pub struct NewestEventRow {
    pub seq: u64,
    pub message_id: Option<u64>,
    pub kind: String,
    pub ts: String,
    /// The event's payload, exactly as the column holds it. Unparsed — what
    /// counts as a BODY is `crate::shapes::body`'s question, not the store's.
    pub payload_json: String,
}

#[derive(Debug, Clone)]
pub struct ThreadRow {
    pub id: String,
    pub account: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub settings_json: String,
    pub last_seq: u64,
    pub next_message_id: u64,
    pub last_compaction_message_id: u64,
    pub needs_restate: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// A pending INTERRUPTION owed to a thread (DEC-56): the flag, and at what
/// time. Present means "this thread was cut off by a hard break and has not
/// yet been told"; absent is the normal state and is also, by DEC-57, the
/// answer for a turn that merely QUEUED.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadInterruptRow {
    pub thread_id: String,
    /// `session_limit` | `weekly_limit` | `fable_limit` | `threshold` |
    /// `hard_budget` (see `crate::event::InterruptTrigger`).
    pub trigger: String,
    /// One short factual clause naming the specific budget or threshold.
    pub detail: String,
    /// DEC-55's flag: did a switchover actually happen.
    pub switched: bool,
    pub from_account: Option<String>,
    pub to_account: Option<String>,
    /// RFC3339 UTC.
    pub interrupted_at: String,
}

/// Selection for [`Store::query_messages`] — the citation query's shape.
#[derive(Debug, Clone, Default)]
pub struct MessageQuery<'a> {
    /// Exact message ids. Wins over `from`/`to` when present.
    pub ids: Option<&'a [u64]>,
    /// Inclusive message-id range bounds.
    pub from: Option<u64>,
    pub to: Option<u64>,
    /// The last N matching rows.
    pub tail: Option<usize>,
    /// Event kinds to include. `None` means "whatever the caller resolved" —
    /// the store does not carry a default; the WS layer owns messages-only.
    pub kinds: Option<&'a [String]>,
    /// Resume strictly after this message id.
    pub cursor: Option<u64>,
    /// Page size, already capped by the caller.
    pub limit: usize,
}

/// One page of [`Store::query_messages`].
#[derive(Debug, Clone, Default)]
pub struct MessagePage {
    pub events: Vec<StoredEvent>,
    /// How many rows matched BEFORE the page limit — so a caller citing a
    /// range knows whether it got all of it.
    pub total_matched: usize,
    pub next_cursor: Option<u64>,
    pub has_more: bool,
}

/// A queue row (Q4).
#[derive(Debug, Clone)]
pub struct QueueRow {
    pub id: i64,
    pub thread_id: String,
    pub message_id: u64,
    pub text: String,
    pub intent: String,
    pub state: String,
    pub created_at: String,
    /// The caller's opaque per-message blob, carried from `threads.send` to
    /// delivery. `Value::Null` when none was supplied. Exactly ONE declared
    /// path out of it reaches the model — see [`crate::context`] — and the rest
    /// stays invisible by construction (DEC-55 category (a)).
    pub metadata: Value,
    /// The header declarations accepted for this message (INTENT 66b), stored
    /// post-validation so delivery renders without re-judging anything.
    pub headers: Vec<crate::context::HeaderDeclaration>,
    /// WHETHER TO SEND (R10.M1): `eager | turn_end | ride | never` — see
    /// [`crate::event::delivery`]. `never` rows are invisible to every
    /// dispatcher read.
    pub dispatch: String,
    /// How many `delivery_attempted` events have named this row.
    pub attempts: u32,
}

/// **The current delivery's clock** — a row of `delivery_clocks`. Both instants
/// are BIRTHS (when something began), never a deadline; see `schema.sql`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveryClock {
    /// When the delivery's FIRST attempt began. A retry never moves it.
    pub turn_started_at: String,
    /// The `kind_str` of the current run of consecutive same-class failures.
    pub streak_kind: Option<String>,
    /// When that run began — what a wall-clock retry budget is measured from.
    pub streak_since: Option<String>,
}

/// **A PARKED PROMPT (Q5/G10) — and nothing about WHEN.**
///
/// This row used to carry a floor (`retry_at`) that decided, per thread, when the
/// retry was eligible. It does not any more, on the operator's ruling: *"There
/// should not be a per-thread retry timestamp"*, and *"if any thread is parked
/// then the pool needs a retry, not the thread."* So this struct is the DURABLE
/// HOME OF A PROMPT while its thread waits, plus the attribution a starving
/// delivery needs, and the question "may it run now" is answered by the admission
/// gate every single time the pool is reconsidered —
/// [`crate::runtime::ThreadManager::retries_to_reconsider`] has no filter at all.
#[derive(Debug, Clone, Default)]
pub struct RetryRow {
    pub thread_id: String,
    pub prompt: String,
    pub message_ids: Vec<u64>,
    /// **SPAWNS THAT RAN AND WERE REFUSED** — unchanged by the move to a pool
    /// cadence, and deliberately kept. It is per-thread ATTRIBUTION (a delivery
    /// refused thirty times is information, and
    /// [`crate::event::ThreadEvent::RetryStarving`] is built on it), it still
    /// drives the per-class attempt CEILINGS, and it is one of the two inputs to
    /// the pool's cadence rung — `retry::ladder_secs(kind, attempt + barren)`.
    pub attempt: u32,
    pub kind: String,
    pub reason: String,
    /// The account that produced the failure. `None` on rows written before the
    /// column existed; `recover` backfills those at boot so a starving retry can
    /// always name the login it is piling up behind.
    pub account: Option<String>,
    /// When this row was FIRST written — **read-only**. The store stamps it on
    /// insert and `upsert_retry`'s `DO UPDATE` list deliberately omits it, so it
    /// survives every re-arm; whatever a caller puts here is ignored on write.
    /// It is the only honest answer to "how long has this delivery been waiting",
    /// and with the floor gone it is also the only instant this row carries.
    pub created_at: String,
}

/// An in-flight turn's process identity (G1) — and, since 2026-08-17, what it
/// is answering.
#[derive(Debug, Clone)]
pub struct RunningTurnRow {
    pub thread_id: String,
    pub pid: i64,
    pub spawn_cookie: String,
    pub output_path: String,
    pub byte_offset: u64,
    pub started_at: String,
    /// The operator's prompt, verbatim.
    ///
    /// `None` means "this row was written by a daemon that did not store it" —
    /// never "there was no prompt". `adopt_turn` treats `None` as its old,
    /// declining self, so the gap closes on the first restart after this build
    /// lands instead of needing a migration of live rows.
    pub prompt: Option<String>,
    /// The deliveries this execution answers. Empty on a pre-column row, for the
    /// same reason.
    pub message_ids: Vec<u64>,
    /// Which attempt of the retry ladder this execution is. `1` on a
    /// pre-column row (the column's DEFAULT), which is the honest floor: it can
    /// only make a teardown's ladder more patient, never less.
    pub attempt: u32,
}

/// The one-row read behind [`Store::get_running_turn`]. Kept beside
/// [`running_turn_from_row`] so the column ORDER and the reader cannot drift.
const RUNNING_TURN_COLS_SQL: &str =
    "SELECT thread_id, pid, spawn_cookie, output_path, byte_offset, started_at,
            prompt, message_ids_json, attempt
     FROM running_turns WHERE thread_id = ?1";

/// The single deserializer for a `running_turns` row.
///
/// `message_ids_json` degrades to an empty vec rather than failing the read: a
/// row whose id list we cannot parse is still a row naming a LIVE CHILD, and
/// refusing to read it would mean refusing to re-adopt or bury that child at
/// boot. The same reasoning as `RetryRow`'s reader, which does this too.
fn running_turn_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunningTurnRow> {
    let ids_json: Option<String> = r.get(7)?;
    Ok(RunningTurnRow {
        thread_id: r.get(0)?,
        pid: r.get(1)?,
        spawn_cookie: r.get(2)?,
        output_path: r.get(3)?,
        byte_offset: r.get::<_, i64>(4)? as u64,
        started_at: r.get(5)?,
        prompt: r.get(6)?,
        message_ids: ids_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        attempt: r.get::<_, i64>(8).unwrap_or(1).max(1) as u32,
    })
}

/// One INTERACTIVE claude session attached to a thread by `cauldron-claude`
/// (SH-A1, docs/SHIMS.md §A.4). Not a [`RunningTurnRow`]: the daemon did not
/// spawn this process, has no stdout file for it, and must never kill it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedSessionRow {
    /// The attach's spawn cookie — the row's identity. Unique: a second insert
    /// under the same cookie is refused.
    pub cookie: String,
    pub thread_id: String,
    /// The pool account the session runs on (its `CLAUDE_CONFIG_DIR`).
    pub account: String,
    /// The `cauldron-claude` wrapper process.
    pub wrapper_pid: i64,
    /// The interactive claude process. `None` until the SessionStart hook.
    pub claude_pid: Option<i64>,
    /// The native transcript file. `None` until the SessionStart hook.
    pub transcript_path: Option<String>,
    /// How far the Stop-hook transcript sync has read.
    pub byte_offset: u64,
    pub started_at: String,
}

/// ONE column list for every reader of `attached_sessions`, in
/// [`attached_session_from_row`]'s order, so a future column cannot be added
/// to one reader and forgotten in another.
const ATTACHED_SESSION_COLS: &str =
    "cookie, thread_id, account, wrapper_pid, claude_pid, transcript_path, byte_offset, started_at";

fn attached_session_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AttachedSessionRow> {
    Ok(AttachedSessionRow {
        cookie: r.get(0)?,
        thread_id: r.get(1)?,
        account: r.get(2)?,
        wrapper_pid: r.get(3)?,
        claude_pid: r.get(4)?,
        transcript_path: r.get(5)?,
        byte_offset: r.get::<_, i64>(6)? as u64,
        started_at: r.get(7)?,
    })
}

/// A held thread execution lock (msg #1213). One row per LOCKED thread; the
/// absence of a row is the only "unlocked".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadLockRow {
    pub thread_id: String,
    /// The daemon PROCESS that holds it (`ThreadManager::boot_id`).
    pub boot_id: String,
    /// That process's pid — the reclaim rule's only input.
    pub daemon_pid: i64,
    /// dispatch | parked | retry | adopt.
    pub entry: String,
    pub acquired_at: String,
}

/// The outcome of [`Store::try_claim_thread`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// The row is yours. Release it with [`Store::release_thread_lock`].
    Taken,
    /// Somebody else's row is in the way; here it is.
    Held(ThreadLockRow),
    /// The conflicting row disappeared between the insert and the read — a
    /// holder released in the gap. Retry the claim.
    Vanished,
}

fn map_lock(r: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadLockRow> {
    Ok(ThreadLockRow {
        thread_id: r.get(0)?,
        boot_id: r.get(1)?,
        daemon_pid: r.get(2)?,
        entry: r.get(3)?,
        acquired_at: r.get(4)?,
    })
}

fn read_lock(conn: &Connection, thread_id: &str) -> Result<Option<ThreadLockRow>> {
    Ok(conn
        .query_row(
            "SELECT thread_id, boot_id, daemon_pid, entry, acquired_at
               FROM thread_locks WHERE thread_id = ?1",
            params![thread_id],
            map_lock,
        )
        .optional()?)
}

/// One per-model spend row parsed from a turn result (Q11).
#[derive(Debug, Clone, Default)]
pub struct UsageRow {
    pub thread_id: String,
    pub account: Option<String>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cost_usd: f64,
    /// RFC3339 UTC instant the EXECUTION that produced this spend started.
    /// `None` when unknown (rows written before this column existed, or a
    /// caller with no running-turn record to read it from) — the overlap-
    /// window calibrator (msg #620) then simply sees no interval here rather
    /// than guessing one.
    pub started_at: Option<String>,
}

/// One budget key's persisted config (§15.1/§15.2; DEC-4/DEC-11/DEC-13, as
/// amended by the operator's round-7 corrections).
#[derive(Debug, Clone)]
pub struct BudgetRow {
    pub key: String,
    /// RESERVED for §14's nested workflow budgets — never read by v1 (DEC-14).
    pub parent_key: Option<String>,
    pub session_pct: Option<f64>,
    pub weekly_pct: Option<f64>,
    /// The weekly-FABLE percentage budget (round-7 item 4).
    pub fable_pct: Option<f64>,
    /// The CASH budget in DOLLARS, pooled across accounts (round-7 item 5).
    pub cash_usd: Option<f64>,
    /// Which accounts this key applies to. NEVER defaulted to "all" by the
    /// crate (round-7 item 6) — an empty scope is refused at write time.
    pub accounts: Vec<String>,
    /// `kill` | `finish` (round-7 item 3: default FINISH, kill is a toggle).
    pub on_trip: String,
    /// RFC3339 instant this key was DISABLED, or `None` while it is live
    /// (Q-34, msg #751). A key is never deleted, so this column is the whole
    /// of the "stop using this key" mechanism: while it is set, every
    /// admission that names the key is DENIED and the turn parks, and every
    /// row of its accounting stays exactly where it was. The instant itself is
    /// kept (rather than a bare flag) so "when did we stop spending on this?"
    /// is answerable from the config alone.
    pub disabled_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One charge against a budget key, in tokens, on one account and one axis.
///
/// `tokens` and `cost_usd` are SIGNED. A meter is the sum of its rows over the
/// current window, so a negative row is how a provisional over-charge is
/// undone: the turn epilogue reconciles the live per-message estimate against
/// the authoritative `result_turn` totals by writing the DIFFERENCE, which is
/// negative whenever the estimate ran ahead. Without that, an over-charge was
/// permanent — `tokens.saturating_sub(already)` clamped to zero and the meter
/// simply stayed wrong (the 2× reading observed 2026-07-29).
#[derive(Debug, Clone)]
pub struct ChargeRow {
    pub budget_key: String,
    pub thread_id: String,
    pub account: String,
    pub axis: String,
    pub tokens: i64,
    pub cost_usd: f64,
    pub ts: String,
}

/// One raw gauge sample (DEC-15). `pct` is normalized to 0-100 whatever layer
/// produced it.
#[derive(Debug, Clone)]
pub struct ObservationRow {
    pub id: i64,
    pub account: String,
    pub axis: String,
    pub pct: f64,
    /// RFC3339 UTC instant this window resets, when known.
    pub resets_at: Option<String>,
    pub source: String,
    pub observed_at: String,
}

/// The continuous per-(account, axis) limit estimate (DEC-12).
#[derive(Debug, Clone)]
pub struct EstimateRow {
    pub account: String,
    pub axis: String,
    pub tokens_per_percent: f64,
    pub sample_count: u64,
    /// The newest observation already folded into the estimate (so one
    /// observation pair is never counted twice).
    pub last_obs_id: i64,
    pub updated_at: String,
}

/// ONE PERIOD BUCKET of the limit estimate: what capacity looked like for one
/// LOGIN, on one axis, across one reset window (`limit_estimate_periods`).
///
/// Live while its window is: rewritten in place on every estimate change, and
/// frozen the moment the window rolls simply by never being written again.
///
/// Keyed by [`Self::identity`] — the login — never by the label. See the
/// table's comment in `schema.sql` for why: the label `default` is a POINTER
/// at whichever login the operator has made current, and history keyed by a
/// pointer re-attributes itself without saying so.
#[derive(Debug, Clone)]
pub struct PeriodEstimateRow {
    /// `accounts.identity` (an opaque login fingerprint), or `label:<name>`
    /// when this label has no readable identity.
    pub identity: String,
    pub axis: String,
    /// The window's reset instant, RFC3339 UTC.
    pub period_end: String,
    /// The registry label this login answered to at the last write. Read-only
    /// colour: two labels for one login share ONE bucket, deliberately.
    pub account_label: String,
    pub config_dir: Option<String>,
    pub tokens_per_percent: f64,
    /// `tokens_per_percent * 100` — the whole pool.
    pub limit_tokens: f64,
    pub sample_count: u64,
    /// How many times this row was written while its window was live.
    pub updates: u64,
    pub first_recorded_at: String,
    pub updated_at: String,
}

/// One budget KILL read back from the ledger (R9.L4) — see
/// [`Store::budget_kill_latches`].
#[derive(Debug, Clone)]
pub struct KillLatchRow {
    pub thread_id: String,
    pub budget_key: String,
    pub axis: String,
    pub limit: f64,
    /// The `budget_tripped` event's own `ts`.
    pub killed_at: String,
    /// The operator cancelled on the killed thread after the kill
    /// (`turn_interrupted` with `trigger: operator`, later in its sequence).
    pub cancelled_after: bool,
}

/// A turn killed by a budget trip, with its prompt preserved (CON-4).
#[derive(Debug, Clone)]
pub struct ParkedTurnRow {
    pub thread_id: String,
    pub prompt: String,
    pub message_ids: Vec<u64>,
    pub attempt: u32,
    pub reason: String,
    pub budget_key: Option<String>,
    pub parked_at: String,
}

/// An aggregated spend line (read by the budget calibration; `ledger/query`, its wire read, was retired at R2.S3a).
///
/// The headline numbers cover TRUSTWORTHY rows only (`accounting_rev >=
/// ACCOUNTING_REV`). The `legacy_*` fields carry the pre-fix rows alongside,
/// unsummed and unmixed, so a caller can say "and there are N older rows whose
/// totals are inflated" instead of silently either dropping or believing them.
#[derive(Debug, Clone)]
pub struct SpendSummary {
    pub key: String,
    pub total_tokens: u64,
    pub cost_usd: f64,
    pub turns: u64,
    /// Rows written by the cumulative-snapshot writer (`accounting_rev` 0).
    pub legacy_rows: u64,
    /// What those rows claim, which is an over-statement of unknown size.
    pub legacy_cost_usd: f64,
}

/// One model's INCREMENT on a single `result_turn`, as booked on the ledger.
/// The runtime charges the budget meter from exactly these numbers, so the
/// ledger and the meter cannot drift apart.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageDelta {
    pub model: String,
    pub tokens: u64,
    pub cost_usd: f64,
}

/// What one `PRAGMA wal_checkpoint` actually did.
///
/// SQLite answers a checkpoint with three numbers and we used to throw all
/// three away. That is how a WAL reached 87 MB over three days without one
/// line anywhere saying so: the checkpoint was running, it was declining to
/// finish, and declining silently. The numbers are the whole point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointReport {
    /// Non-zero when SQLite could not get the lock it needed and gave up
    /// WITHOUT reclaiming everything. The field that was invisible.
    pub busy: i64,
    /// Frames in the WAL when the checkpoint started.
    pub log_frames: i64,
    /// Frames it actually folded back. `checkpointed < log` means the WAL
    /// still holds frames nobody has written into the main database.
    pub checkpointed_frames: i64,
    /// The `-wal` file's size on disk, before and after.
    ///
    /// Worth recording separately from the frame counts, because they answer
    /// different questions and only one of them was ever the problem here.
    /// A PASSIVE checkpoint reclaims frames but resets the write cursor
    /// in place — it does NOT shrink the file, so the `-wal` stays at its
    /// high-water mark for the daemon's whole uptime. TRUNCATE is the only
    /// mode that moves these numbers.
    pub wal_bytes_before: u64,
    pub wal_bytes_after: u64,
}

/// **WHAT A STORE PROMISES ABOUT A CRASH** — rule 2.7's `durable` stage, as
/// `docs/shapes.md` 2.7 asks a producer to declare it.
///
/// Three states, and the ONE that does not exist is the point: there is no
/// variant meaning *"durable, probably"*. A store either syncs on every commit,
/// or has a barrier that syncs on demand, or promises nothing — and the third is
/// a REFUSAL, carried as a sentence a caller can be shown rather than as a
/// weaker `Ok`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// `synchronous` ≥ `FULL`. Every commit already fsynced; a barrier has
    /// nothing to do.
    ///
    /// **This is the accident, not the design.** `docs/shapes.md` 2.7: *"today
    /// every command pays durability whether it asked or not — an accident of
    /// one producer's store, and this rule says the opposite."* The variant
    /// exists so the accident is NAMED where the code can see it, rather than
    /// being a sentence in a doc comment nothing reads.
    PerCommit { level: i64 },
    /// `synchronous = NORMAL`. Commits do not sync; the checkpoint does, so
    /// [`Store::durability_barrier`] is where the cost lands and a caller that
    /// never calls it never pays.
    AtBarrier,
    /// **NOTHING IS PROMISED, AND HERE IS WHY** — an in-memory database,
    /// `synchronous=OFF`, or a connection that will not answer. The barrier
    /// refuses with this sentence.
    None { why: &'static str },
}

/// **WHAT ONE BARRIER DID.** Returned to the producer, never to a consumer:
/// rule 2.7's reply carries the STAGE and no store internals (*"ack should just
/// inform the general milestones, not protocol-specific details"*), so these
/// numbers exist for a log line and for a test, and `shapewire::command`
/// throws every one of them away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Barrier {
    /// There was nothing to do: every commit before this call had already
    /// fsynced, because this store runs `synchronous` at `level`.
    AlreadyDurable { level: i64 },
    /// The log was folded into the database, which is the sync point at
    /// `synchronous=NORMAL`.
    Checkpointed(CheckpointReport),
}

impl Barrier {
    /// Did this barrier actually have to do anything? A log line's question, and
    /// the one a test asks to tell "the checkpoint ran" from "the store was
    /// paying all along".
    pub fn did_work(&self) -> bool {
        matches!(self, Self::Checkpointed(_))
    }
}

/// Which door the shutdown checkpoint got through, and what it cost.
///
/// The rung is part of the answer, not decoration: "the WAL was folded back"
/// and "the WAL was folded back after the store mutex refused to come free for
/// two and a half seconds" are the same outcome and completely different news.
/// The second one is a daemon whose store was wedged at the moment it was asked
/// to stop, which is the state that produced two SIGTERMs and a SIGKILL on
/// 2026-09-11 — and under the old code it was invisible, because the shutdown
/// checkpoint simply never returned.
#[derive(Debug, Clone)]
pub enum ShutdownCheckpoint {
    /// The write mutex was free, or came free inside the budget.
    OnPrimary {
        waited: std::time::Duration,
        report: CheckpointReport,
    },
    /// The mutex did not come free. A connection was opened for the purpose and
    /// the checkpoint ran through it.
    OnItsOwnConnection {
        waited: std::time::Duration,
        report: CheckpointReport,
    },
    /// Neither door opened inside the budget. The daemon exits anyway; the next
    /// boot recovers the WAL, which is what WAL recovery is for.
    Refused {
        waited: std::time::Duration,
        error: String,
    },
}

impl ShutdownCheckpoint {
    pub fn report(&self) -> Option<&CheckpointReport> {
        match self {
            Self::OnPrimary { report, .. } | Self::OnItsOwnConnection { report, .. } => Some(report),
            Self::Refused { .. } => None,
        }
    }

    /// How long the shutdown path spent waiting for a way in.
    pub fn waited(&self) -> std::time::Duration {
        match self {
            Self::OnPrimary { waited, .. }
            | Self::OnItsOwnConnection { waited, .. }
            | Self::Refused { waited, .. } => *waited,
        }
    }

    /// The rung, for a log line.
    pub fn door(&self) -> &'static str {
        match self {
            Self::OnPrimary { .. } => "primary connection",
            Self::OnItsOwnConnection { .. } => "its own connection (the store mutex was held)",
            Self::Refused { .. } => "none",
        }
    }
}

impl CheckpointReport {
    /// Did this checkpoint fold back everything that was there to fold?
    ///
    /// A `false` here is the condition that used to be silent, and is the
    /// thing worth logging loudly rather than the raw numbers.
    pub fn fully_reclaimed(&self) -> bool {
        self.busy == 0 && self.checkpointed_frames >= self.log_frames
    }
}

impl Store {
    /// Open a persistent store at `path` (WAL). Creates parent dirs + file.
    ///
    /// # `synchronous` IS LEFT ALONE, AND THAT IS A DECISION WITH NUMBERS BEHIND IT
    ///
    /// `docs/shapes.md` 2.7 names this store as an ACCIDENT — *"`store.rs` runs
    /// WAL at a compiled `synchronous=FULL`, so today every command pays
    /// durability whether it asked or not … and this rule says the opposite"* —
    /// and the obvious reading of the rule is that the pragma should come down
    /// to `NORMAL` now that there is a barrier to ask with. **It should not, and
    /// the next dispatch to read that sentence should read this one too.**
    /// Measured on this machine, 2026-10-02, 400 autocommit `append_event`s into
    /// a fresh WAL store, debug build, APFS:
    ///
    /// | `synchronous` | per commit | what a commit survives |
    /// |---|---|---|
    /// | `FULL` (today) | 0.141 ms | process death, kernel panic, power loss to `fsync` strength |
    /// | `NORMAL` | 0.087 ms | process death. **NOT** a kernel panic or power loss |
    /// | `OFF` | 0.072 ms | process death only, and the barrier refuses outright |
    ///
    /// **The whole saving is 54 µs a commit.** Against what it costs:
    /// `ThreadManager::emit` is *persist-then-broadcast* — `append_event` and
    /// `bump_last_seq` land BEFORE the hub fans the event out — so at `NORMAL` a
    /// kernel panic can leave the ledger missing a `UserMessage` that every
    /// subscriber has already received, that AUI has already spoken, and that a
    /// turn has already acted on. That is not "losing the last write"; it is the
    /// ledger disagreeing with what consumers observed, which is the
    /// second-source disease this whole document exists to delete. The
    /// relaxation would CREATE one to save 54 microseconds.
    ///
    /// **And the amortisation argument does not apply to this platform at this
    /// setting.** The expensive sync on macOS is `F_FULLFSYNC`, which flushes the
    /// device's own cache; SQLite uses it only under `PRAGMA fullfsync`, which is
    /// `0` here. Measured the same way: 0.116 ms a commit at `fullfsync=0`
    /// against **6.03 ms** at `fullfsync=1`, a factor of 52. *That* is the number
    /// a barrier would amortise — and `threads` does not pay it, has never paid
    /// it, and the barrier deliberately does not start: rule 2.7's `durable` is
    /// *"committed such that a crash cannot lose it"*, which is the guarantee
    /// this store already makes, and inventing a stronger fourth stage nobody
    /// has ruled on would be imposing one.
    ///
    /// **The one path where the per-commit cost is real** is
    /// `import::import_thread`, which calls `append_event` once per JSONL line
    /// in a loop with no transaction around it, so a 100,000-line import pays
    /// 100,000 fsyncs (~14 s, against ~9 s at `NORMAL`). The fix there is one
    /// transaction around that loop — one commit, one fsync, and faster than
    /// `NORMAL` is without it. It is not a reason to move a pragma that governs
    /// every other writer in the daemon.
    ///
    /// So the rule is satisfied by the half that was missing: the barrier exists
    /// ([`Self::durability_barrier`]), it is opt-in, it refuses rather than
    /// downgrading, and it declares its contract before it is asked. The pragma
    /// stays where it is. A producer that WANTS the `AtBarrier` trade gets it by
    /// setting `synchronous=NORMAL`; that path is built and tested
    /// (`a_barrier_puts_the_rows_in_the_database_file_and_not_only_in_the_log`)
    /// so the choice is one line and not a project.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_noticing(path, &|_| {})
    }

    /// [`Self::open`], telling `notice` — BEFORE the work starts — each time a
    /// schema/data migration is about to do real work on this store (R9.C7h,
    /// implements C-7). `run_daemon` passes a notice that flips `/health` to
    /// `migrating`; a store that needs nothing never calls it, so nothing is
    /// ever reported as migrating that is not. The argument names the step —
    /// a word for the operator, not a contract. (The `last_user_input_at`
    /// backfill is not announced: it runs, as a no-op scan, on every open.)
    pub fn open_noticing(path: impl AsRef<Path>, notice: &dyn Fn(&'static str)) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| Error::Store(format!("creating {}: {e}", parent.display())))?;
            }
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            path: Some(path.to_path_buf()),
            // The pool is built here but opens nothing yet. `apply_schema`
            // below is what creates the `-wal`/`-shm` pair a READ-ONLY
            // connection needs and cannot create for itself, so the readers
            // must not be opened before it has run.
            reads: Some(Arc::new(ReadPool {
                path: path.to_path_buf(),
                idle: Mutex::new(Vec::new()),
                opened: AtomicUsize::new(0),
                returned: Condvar::new(),
            })),
        };
        store.apply_schema(notice)?;
        Ok(store)
    }

    /// **Open an EXISTING store READ-ONLY — never create it, never migrate it**
    /// (R4.S4b.fix, HOLDS R25). For a CLI verb that only reads, such as
    /// `threads cutover`: [`Self::open`] creates a missing file (and its parent
    /// directories) and runs [`Self::apply_schema`], so a wrong data dir minted
    /// an empty store and a CLI newer than the running daemon migrated the live
    /// one. This does neither: a missing file is an error, the connection is
    /// `SQLITE_OPEN_READ_ONLY`, and no schema, migration or pragma that writes
    /// is run. Any write routed through the returned store fails `SQLITE_READONLY`.
    ///
    /// **WHICH read-only open, by whether `<db>-wal` exists.** Every store this
    /// module opens is WAL-mode, and a plain `mode=ro` connection to a WAL
    /// database whose `-wal`/`-shm` are absent cannot open it (it would need to
    /// create the `-shm`), while one whose `-wal` exists may still be written by
    /// a live daemon and must see that log.
    /// * `-wal` present → `mode=ro`: reads through the log, so a live daemon's
    ///   uncheckpointed rows are seen. Creates no file the run did not find.
    /// * `-wal` absent → `immutable=1`: no WAL connection is open (one would
    ///   hold a `-wal`), so the file is the whole database; SQLite opens it
    ///   with no locking and no side files. The window it accepts: a writer
    ///   that opens the store DURING this read is not seen — tolerable for a
    ///   one-row lookup, wrong for anything that holds the connection.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(Error::Store(format!(
                "no store at {} (refusing to create one)",
                path.display()
            )));
        }
        let mut wal = path.as_os_str().to_os_string();
        wal.push("-wal");
        let param = if Path::new(&wal).exists() {
            "mode=ro"
        } else {
            "immutable=1"
        };
        let uri = format!("file:{}?{param}", uri_escape_path(path));
        let conn = Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: Some(path.to_path_buf()),
            // Reads fall back to the one (read-only) connection.
            reads: None,
        })
    }

    /// Open an in-memory store (tests).
    ///
    /// No read pool: `:memory:` is per-connection, so a second connection would
    /// open a different, empty database rather than a second view of this one.
    /// Reads fall back to the write connection and this store behaves exactly
    /// as every store did before the pool existed.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            reads: None,
            path: None,
        };
        store.apply_schema(&|_| {})?;
        Ok(store)
    }

    /// **THE WRITE CONNECTION.** One, behind one mutex, serialized — which is
    /// what a single SQLite writer is anyway. Everything that changes the
    /// database comes through here.
    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        DB_TRIPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.conn
            .lock()
            .map_err(|_| Error::Internal("store mutex poisoned".into()))
    }

    /// **A READ CONNECTION**, from the pool where there is one.
    ///
    /// Use this for any method whose statements are pure `SELECT`. It counts
    /// towards [`db_trips`] exactly as [`Self::conn`] does, so the trip-count
    /// tests keep measuring the same thing: the number of round trips, which
    /// was always the cost that mattered, has not changed — only whether they
    /// queue behind each other.
    fn read(&self) -> Result<ReadConn<'_>> {
        DB_TRIPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(pool) = &self.reads {
            match pool.checkout() {
                Ok(conn) => {
                    return Ok(ReadConn {
                        pooled: Some((pool.clone(), Some(conn))),
                        writer: None,
                    })
                }
                // **A POOL THAT CANNOT OPEN A CONNECTION FALLS BACK TO THE
                // WRITER RATHER THAN FAILING THE READ.** Opening a second
                // connection is a new way for a read to fail that did not
                // exist before this pool — a descriptor limit, a `-shm` that
                // cannot be created, a filesystem gone read-only — and a
                // hardening change is not allowed to turn a working read into
                // a failed one. Degraded means "as slow as it was last week",
                // which is a state this daemon survived for months.
                Err(e) => tracing::warn!(
                    error = %e,
                    "could not take a pooled read connection; falling back to the write \
                     connection for this read. The store is serialized again until this \
                     clears."
                ),
            }
        }
        Ok(ReadConn {
            pooled: None,
            writer: Some(
                self.conn
                    .lock()
                    .map_err(|_| Error::Internal("store mutex poisoned".into()))?,
            ),
        })
    }

    /// Fold the write-ahead log back into the main database and truncate it.
    ///
    /// **This is the shutdown path's corruption mitigation, and it is worth
    /// being precise about what it does and does not buy**, because the risk it
    /// addresses is routinely overstated in both directions.
    ///
    /// `open()` sets `journal_mode=WAL` and leaves `synchronous` at its
    /// compiled default of `FULL`. That combination is already crash-safe by
    /// construction: a `SIGKILL` mid-write does not corrupt the database,
    /// because the next process to open it replays the WAL and recovers every
    /// committed transaction. So this is NOT what stands between us and a
    /// corrupt store — SQLite's own durability contract is.
    ///
    /// What it buys is the difference between a database that must be
    /// RECOVERED and one that is simply CLOSED. After a checkpoint the WAL is
    /// empty and `threads.db` is complete on its own, which means the next boot
    /// does no recovery, cannot be interrupted part-way through recovery (the
    /// one window where repeated hard kills genuinely compound), and leaves no
    /// `-wal`/`-shm` pair for a backup or a file copy to tear.
    ///
    /// It is called on the way out of [`crate::run_daemon`] and, since
    /// 2026-09-11, on an interval from [`crate::spawn_wal_keeper`].
    /// A failure is reported and never fatal: refusing to exit because the
    /// checkpoint failed would strand the port, which is the actual outage.
    ///
    /// **Why it also runs on an interval now.** Shutdown-only truncation means
    /// the `-wal` file's HIGH-WATER MARK is permanent for the daemon's uptime.
    /// Auto-checkpoint keeps the frame count near `wal_autocheckpoint` (1000
    /// pages, ~4 MB) but never shrinks the file, so a single bad episode — one
    /// long transaction, one import, one backup holding a read snapshot — grows
    /// the file and every WAL-index operation pays for that size until the next
    /// clean exit. Observed 2026-09-11: 87 MB after three days, health latency
    /// 2.1s against tts's 0.03s, and the daemon eventually too wedged to answer
    /// the SIGTERM that would have truncated it. The interval turns a permanent
    /// inflation into a bounded one.
    pub fn checkpoint(&self) -> Result<CheckpointReport> {
        // Measured before the lock so a wedged mutex cannot make us report a
        // stale "before" as though the checkpoint had shrunk something.
        let wal_bytes_before = self.wal_bytes();
        let conn = self.conn()?;
        // Returns one row — (busy, log_frames, checkpointed_frames). TRUNCATE
        // is the strongest mode: it blocks for readers to clear, writes every
        // frame back, and then zeroes the WAL file rather than leaving it
        // allocated for reuse.
        //
        // **THE STORE NOW HAS A READ POOL, AND THIS COMMENT USED TO SAY THAT
        // WOULD MATTER.** It does, and here is the whole of it: TRUNCATE waits
        // for readers to clear, so a pooled read in flight can make this call
        // wait and, past `busy_timeout`, come back with `busy != 0` having
        // reclaimed nothing. With one connection that was impossible.
        //
        // The trade is taken deliberately and it is not close. The reads are
        // indexed and sub-millisecond, so the wait is real but tiny; a
        // checkpoint that declines is not a failure (the WAL is folded on an
        // interval and again on the way out, and a `false` from
        // `fully_reclaimed` is logged loudly at both sites since 376207b); and
        // what the pool buys is that a slow read no longer takes the whole
        // daemon out of service, which is the outage this file's history is
        // made of. A checkpoint that occasionally has to try again is a much
        // smaller thing than a process that cannot answer `/health`.
        let report = Self::truncate(&conn, wal_bytes_before, || self.wal_bytes())?;
        drop(conn);
        Ok(report)
    }

    /// **THE SHUTDOWN CHECKPOINT, WITH A CEILING ON HOW LONG IT WILL WAIT.**
    ///
    /// `599bff3` gave the daemon a real clean shutdown on SIGTERM. It did not
    /// hold, and the reason was structural rather than a bug in it: the last
    /// thing a clean exit does is fold the WAL back, that runs
    /// [`Self::checkpoint`], and `checkpoint` wants the one mutex that is
    /// exactly what fails first. Measured on 2026-09-11 — SIGTERM at 17:24:56Z
    /// with 60s to drain, `rung2_failed`, a second SIGTERM, `rung3_failed`,
    /// SIGKILL at 17:26:45Z. 108 seconds, two SIGTERMs, no drain, with an 87 MB
    /// WAL that then survived into the next boot. A graceful shutdown that is
    /// only as available as the thing it is trying to recover from is not a
    /// graceful shutdown.
    ///
    /// **The coupling is what this removes, not the instance.** The read pool
    /// took the instance away — the 2026-09-11 lock-holder was a READ, and
    /// reads no longer touch this mutex at all. But the mutex still exists, a
    /// long WRITE can still hold it, and the WAL keeper's own checkpoint is
    /// `spawn_blocking` — `wal_keeper.abort()` cancels the task, not the
    /// blocking closure, so shutdown can still arrive while the keeper holds
    /// the connection. The next long lock-hold would re-break shutdown exactly
    /// as the last one did.
    ///
    /// So the path gets a LADDER instead of one door, and each rung is
    /// reported rather than inferred:
    ///
    /// 1. Wait up to HALF the budget for the write mutex. Got it — checkpoint
    ///    on the primary connection, which is today's behaviour, unchanged,
    ///    and is what happens every ordinary shutdown.
    /// 2. Still wedged — open a connection OF ITS OWN and checkpoint through
    ///    that, with `busy_timeout` set to what is left of the budget. SQLite
    ///    coordinates the two connections itself; the worst case is
    ///    `busy != 0` and nothing reclaimed, which is a report, not a hang.
    /// 3. Neither door opened. Say so, with the number of milliseconds spent,
    ///    and let the process exit.
    ///
    /// **What is bounded is WAITING FOR A LOCK, not the fold itself.** Once
    /// SQLite is writing frames back it is not interrupted, and it should not
    /// be: a half-folded WAL is the recovery case, and recovery is precisely
    /// what the checkpoint exists to spare the next boot. The defect was an
    /// unbounded WAIT, and that is what has a ceiling now.
    ///
    /// Never an error. A store that cannot be checkpointed is still a
    /// crash-safe store (WAL + `synchronous=FULL`); a daemon that will not exit
    /// is a held port, which is the actual outage.
    pub fn checkpoint_for_shutdown(&self, budget: std::time::Duration) -> ShutdownCheckpoint {
        let started = std::time::Instant::now();
        let wal_bytes_before = self.wal_bytes();
        let mutex_budget = budget / 2;

        // ── RUNG 1: the primary connection, if it comes free ─────────────
        loop {
            match self.conn.try_lock() {
                Ok(conn) => {
                    // R9.F2l: `waited` is the LOCK WAIT, so it is read the
                    // moment the lock is won — before the fold, exactly as
                    // rung 2 reads its own. Read after `truncate` it timed
                    // the disk as well, and the same field meant two
                    // different things in two arms of one enum.
                    let waited = started.elapsed();
                    DB_TRIPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let out = Self::truncate(&conn, wal_bytes_before, || self.wal_bytes());
                    return match out {
                        Ok(report) => ShutdownCheckpoint::OnPrimary { waited, report },
                        Err(e) => ShutdownCheckpoint::Refused {
                            waited,
                            error: e.to_string(),
                        },
                    };
                }
                // A poisoned mutex is a thread that panicked holding the
                // connection. There is nothing to wait for; go straight to the
                // second door.
                Err(std::sync::TryLockError::Poisoned(_)) => break,
                Err(std::sync::TryLockError::WouldBlock) => {
                    if started.elapsed() >= mutex_budget {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }

        // ── RUNG 2: a connection of its own ──────────────────────────────
        let waited = started.elapsed();
        let Some(path) = &self.path else {
            // In memory: there is no WAL and nothing to fold. Not a failure.
            return ShutdownCheckpoint::Refused {
                waited,
                error: "the store mutex was held and this store has no file to \
                        open a second connection to (in memory)"
                    .into(),
            };
        };
        // What is left of the budget, floored so a fully-spent budget still
        // gives SQLite a moment rather than an instant refusal.
        let remaining = budget
            .saturating_sub(waited)
            .max(std::time::Duration::from_millis(100));
        let opened = Connection::open(path).and_then(|c| {
            c.pragma_update(None, "busy_timeout", remaining.as_millis() as i64)?;
            Ok(c)
        });
        match opened {
            Ok(conn) => match Self::truncate(&conn, wal_bytes_before, || self.wal_bytes()) {
                Ok(report) => ShutdownCheckpoint::OnItsOwnConnection { waited, report },
                Err(e) => ShutdownCheckpoint::Refused {
                    waited: started.elapsed(),
                    error: e.to_string(),
                },
            },
            Err(e) => ShutdownCheckpoint::Refused {
                waited: started.elapsed(),
                error: e.to_string(),
            },
        }
    }

    /// The `PRAGMA wal_checkpoint(TRUNCATE)` itself, on whichever connection
    /// the caller got hold of. Shared by [`Self::checkpoint`] and
    /// [`Self::checkpoint_for_shutdown`] so the two can never drift on what a
    /// checkpoint IS — only on how they reached a connection.
    fn truncate(
        conn: &Connection,
        wal_bytes_before: u64,
        wal_bytes_after: impl Fn() -> u64,
    ) -> Result<CheckpointReport> {
        Self::wal_checkpoint(conn, "TRUNCATE", wal_bytes_before, wal_bytes_after)
    }

    /// `PRAGMA wal_checkpoint(<mode>)`, for the two callers that want two
    /// different modes for two different reasons — [`Self::truncate`] wants the
    /// `-wal` file SHRUNK, [`Self::durability_barrier`] wants the frames SYNCED
    /// and does not care about the file size.
    ///
    /// `mode` is a `&'static str` from this file and is interpolated into SQL,
    /// which is safe for exactly that reason and for no other: it must never
    /// become a parameter anything outside this module chooses. A `PRAGMA`
    /// argument cannot be bound, so there is no safer spelling available.
    fn wal_checkpoint(
        conn: &Connection,
        mode: &'static str,
        wal_bytes_before: u64,
        wal_bytes_after: impl Fn() -> u64,
    ) -> Result<CheckpointReport> {
        let (busy, log_frames, checkpointed_frames) =
            conn.query_row(&format!("PRAGMA wal_checkpoint({mode})"), [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        Ok(CheckpointReport {
            busy,
            log_frames,
            checkpointed_frames,
            wal_bytes_before,
            wal_bytes_after: wal_bytes_after(),
        })
    }

    /// Size of the `-wal` file on disk, or 0 if we cannot see it.
    ///
    /// Best-effort by design: an in-memory store has no path and a missing
    /// `-wal` is the ordinary state right after a successful TRUNCATE. Neither
    /// is worth failing a checkpoint over.
    pub fn wal_bytes(&self) -> u64 {
        // NO LOCK. This used to take the write mutex purely to ask the
        // connection its own path, which meant the one measurement that tells
        // you the store is in trouble was itself unavailable exactly when the
        // store was in trouble — and the WAL keeper calls it every tick.
        let Some(path) = &self.path else { return 0 };
        let mut wal = path.clone().into_os_string();
        wal.push("-wal");
        std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0)
    }

    // ── rule 2.7's third stage: durability, and the barrier that asks for it ─

    /// **WHAT `synchronous` THIS CONNECTION IS ACTUALLY RUNNING AT**, read off
    /// SQLite rather than asserted by a comment.
    ///
    /// `0` OFF, `1` NORMAL, `2` FULL, `3` EXTRA. [`Self::open`] sets
    /// `journal_mode` and deliberately never touches this, so the value is
    /// whatever SQLite was COMPILED with — and [`Self::checkpoint`]'s doc has
    /// claimed since it was written that the compiled default is `FULL` with
    /// nothing anywhere checking it. [`Self::durability`] branches on it, which
    /// makes the number load-bearing instead of documentary, and
    /// `the_compiled_synchronous_default_is_full` pins it.
    ///
    /// It reads the WRITE connection, which is the only one that matters: the
    /// [`ReadPool`]'s connections are opened `SQLITE_OPEN_READ_ONLY` and commit
    /// nothing, so their `synchronous` governs no durability at all.
    pub fn synchronous(&self) -> Result<i64> {
        let conn = self.conn()?;
        Ok(conn.query_row("PRAGMA synchronous", [], |r| r.get(0))?)
    }

    /// **COULD THIS DAEMON WRITE ITS STORE RIGHT NOW?** — the heartbeat check
    /// behind `/health` (R9.C6, [`crate::liveness`]).
    ///
    /// Takes the write mutex, then opens and rolls back a write transaction
    /// (`BEGIN IMMEDIATE` takes SQLite's write lock and writes nothing, so the
    /// WAL does not grow). It fails if the lock cannot be had within the
    /// connection's `busy_timeout`, and it never returns at all while the
    /// mutex is held by something that is stuck — which is the wedge it exists
    /// to see.
    ///
    /// Deliberately NOT through [`Self::conn`]: that counts a round trip in
    /// [`db_trips`], and a background check every two seconds would move the
    /// number the trip-count tests measure.
    ///
    /// A connection already inside a transaction (autocommit off) is reported
    /// live without a `BEGIN`: holding the mutex was the proof, and a nested
    /// `BEGIN` would fail for a reason that is not a wedge.
    ///
    /// **THE PING'S OWN WAIT IS SHORT: [`PING_BUSY_MS`], not the connection's
    /// 5 s** (R9.C6b). The ping holds the write mutex while it waits for
    /// SQLite's lock, and every in-process statement that goes through the
    /// write connection — writes, and reads that fall back to it — queues
    /// behind that mutex. With the connection's own 5 s `busy_timeout`, a
    /// long write by ANOTHER process (`threads import`) had the heartbeat
    /// holding the mutex ~5 s of every ~7 s. So the ping sets a short
    /// per-ping `busy_timeout`, and restores the connection's previous value
    /// before it releases the mutex, success or failure. It is a short WAIT,
    /// not a mutex-only ping, on purpose: a mutex-only ping cannot see a
    /// foreign process holding the write lock, and that is a store this daemon
    /// cannot write — `/health` must still go 503 once it has lasted past the
    /// stale bound (`tests/health_liveness.rs`). A failed ping only means
    /// "no beat this period"; it is the bound, not one ping, that says wedged.
    pub fn ping(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Internal("store mutex poisoned".into()))?;
        if !conn.is_autocommit() {
            return Ok(());
        }
        let previous: i64 = conn.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?;
        conn.pragma_update(None, "busy_timeout", PING_BUSY_MS)?;
        let pinged = conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;");
        // Restored before the mutex is released, whatever the ping said: every
        // other user of this connection keeps its 5 s.
        let restored = conn.pragma_update(None, "busy_timeout", previous);
        pinged?;
        restored?;
        Ok(())
    }

    /// **WHAT THIS STORE CAN PROMISE ABOUT A CRASH** — rule 2.7's `durable`
    /// stage, as the store itself answers it, and the thing
    /// [`Self::durability_barrier`] branches on.
    ///
    /// It is read from the live connection on every call rather than cached at
    /// `open`, because a cached capability is a second source for a fact the
    /// database already holds: one `PRAGMA synchronous=…` anywhere and the cache
    /// would be a promise the store no longer keeps.
    pub fn durability(&self) -> Durability {
        if self.path.is_none() {
            return Durability::None {
                why: "an in-memory database survives no crash at all — there is \
                      no file for anything to be committed to",
            };
        }
        match self.synchronous() {
            // A store that cannot be asked what it syncs has UNKNOWN
            // durability, and unknown is refused. It is the `Err` arm of a
            // `PRAGMA` read, so in practice it means a poisoned mutex or a
            // connection that is gone — neither of which is a thing to answer
            // `durable` over.
            Err(_) => Durability::None {
                why: "this store could not be asked what it syncs",
            },
            Ok(0) => Durability::None {
                why: "`synchronous=OFF`: SQLite issues no sync at all, so a \
                      checkpoint would copy pages nobody has promised are on \
                      the platter",
            },
            Ok(1) => Durability::AtBarrier,
            Ok(level) => Durability::PerCommit { level },
        }
    }

    /// **THE BARRIER — rule 2.7's third stage, done.**
    ///
    /// It returns only once everything this store has already committed will
    /// survive the machine losing power. The three outcomes are the three things
    /// a SQLite store can honestly be, and the interesting one is the middle:
    ///
    /// * `synchronous` ≥ `FULL` — **[`Barrier::AlreadyDurable`]**. Every commit
    ///   fsynced the write-ahead log on its way out, so there is no work to do
    ///   and this call is nearly free. That is TODAY's `threads`, and it is the
    ///   accident `docs/shapes.md` 2.7 names: the barrier has nothing to add
    ///   because the store never stopped paying.
    /// * `synchronous = NORMAL` — **[`Barrier::Checkpointed`]**. Ordinary commits
    ///   do not sync; *"the WAL file is synchronized before each checkpoint and
    ///   the database file is synchronized after each completed checkpoint"*
    ///   (SQLite, `PRAGMA synchronous`). So the checkpoint IS the durability
    ///   point, and a `FULL` checkpoint — every frame folded back, not merely the
    ///   ones a passive pass could reach — is the barrier. **A checkpoint that
    ///   comes back `busy` is REFUSED, not reported as success**: SQLite syncs
    ///   *before* a checkpoint, and a checkpoint that did not happen had no
    ///   before.
    /// * anything else — **refused**, through [`Durability::None`]. In memory, or
    ///   `synchronous=OFF`, or a connection that will not say. A producer that
    ///   cannot promise durability says so; it does not answer a weaker stage
    ///   under this stage's name.
    ///
    /// # Why `FULL` and not `TRUNCATE`
    ///
    /// [`Self::checkpoint`] uses `TRUNCATE`, which additionally zeroes the `-wal`
    /// file — a disk-space concern (the 87 MB episode in that method's doc) and
    /// not a durability one. `TRUNCATE` waits for every reader to clear, so on a
    /// store with eight live read connections it is the mode most likely to come
    /// back `busy`; a barrier is on a caller's critical path and must not inherit
    /// a wait it does not need. `FULL` folds every frame back, which is all the
    /// sync contract above requires.
    pub fn durability_barrier(&self) -> Result<Barrier> {
        match self.durability() {
            Durability::None { why } => Err(Error::Store(format!(
                "this store promises no durability: {why}"
            ))),
            Durability::PerCommit { level } => Ok(Barrier::AlreadyDurable { level }),
            Durability::AtBarrier => {
                let before = self.wal_bytes();
                let conn = self.conn()?;
                let report = Self::wal_checkpoint(&conn, "FULL", before, || self.wal_bytes())?;
                drop(conn);
                if report.busy != 0 || report.checkpointed_frames < report.log_frames {
                    return Err(Error::Store(format!(
                        "the checkpoint this barrier needs did not complete \
                         (busy={}, folded {} of {} frames), so nothing has been \
                         synced and this call will not claim it was",
                        report.busy, report.checkpointed_frames, report.log_frames
                    )));
                }
                Ok(Barrier::Checkpointed(report))
            }
        }
    }

    fn apply_schema(&self, notice: &dyn Fn(&'static str)) -> Result<()> {
        let conn = self.conn()?;
        // R9.C7h: a store created by THIS open has nothing to migrate — the
        // column adds below can still succeed on it (a column that ships only
        // as a migration), but that is creation, not a migration, and is not
        // announced. Only a store that existed before this open is.
        let existed: bool = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='threads'",
            [],
            |r| r.get::<_, i64>(0),
        )? > 0;
        let quiet = |_: &'static str| {};
        let notice: &dyn Fn(&'static str) = if existed { notice } else { &quiet };
        conn.execute_batch(SCHEMA_SQL)?;
        // R9.C7h test seam: a migration that takes as long as the test says.
        #[cfg(any(test, feature = "test-seams"))]
        migration_fault_seam(notice);
        // Column additions to tables that already exist on disk. `CREATE TABLE
        // IF NOT EXISTS` cannot add a column, so every new column ships twice:
        // in the CREATE above (fresh databases) and here (existing ones).
        // Re-running is expected and harmless — a duplicate column is the
        // success case on the second open.
        for sql in MIGRATIONS {
            match conn.execute(sql, []) {
                Ok(_) => notice("add columns"),
                Err(e) if is_duplicate_column(&e) => {}
                Err(e) => return Err(e.into()),
            }
        }
        // Column REMOVALS, same shape in the other direction: the column is
        // gone from the CREATE above (fresh databases) and dropped here
        // (existing ones). "No such column" is the success case on the second
        // open and on every fresh database.
        for sql in DROPPED_COLUMNS {
            match conn.execute(sql, []) {
                Ok(_) => notice("drop columns"),
                Err(e) if is_missing_column(&e) => {}
                Err(e) => return Err(e.into()),
            }
        }
        drop(conn);
        self.migrate_queue_two_records(notice)?;
        self.rekey_pool_shares_on_name(notice)?;
        self.migrate_share_zero_disable(notice)?;
        // The recency column's one-time fill. After the `ALTER` above, because it
        // writes the column that `ALTER` adds, and idempotent on its own NULL —
        // see the function for the cost argument against the 2.46 M-row ledger.
        self.backfill_last_user_input_at()?;
        Ok(())
    }

    /// **THE TWO RECORDS, on a database written before them** (R10.M1, L-8 /
    /// E21-4; `docs/MESSAGING-ORCHESTRATION.md` §10.2). Keeps every row:
    ///
    /// * `waiting` → dispatch `eager` (or `turn_end` when its intent was
    ///   `turn_end` — the wire word `immediate` IS `eager`, and a `turn_end` row
    ///   keeps the meaning it was queued with), attempts 0;
    /// * `delivered` / `failed` → state unchanged, attempts 1 (each was one
    ///   attempt; there is no attempt event for it, the ledger predates them);
    /// * `cancelled` → state `waiting`, dispatch `never`, and ONE
    ///   `message_withdrawn{by: "migration"}` event per thread naming its
    ///   rows. The row and its text stay; only the conflated word goes.
    ///
    /// The column add and the backfill run once (gated on the column being
    /// absent), in one transaction. The `cancelled` sweep runs on EVERY open
    /// and is idempotent on its own predicate, so a row an older binary
    /// cancels after this one migrated is still converted on the next open.
    /// Event seqs are `MAX(seq)+1` per thread: this runs inside `Store::open`,
    /// before any `ThreadState` has loaded its counter from the same MAX.
    fn migrate_queue_two_records(&self, notice: &dyn Fn(&'static str)) -> Result<()> {
        let mut conn = self.conn()?;
        let has_dispatch = {
            let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('queue')")?;
            let cols = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut found = false;
            for c in cols {
                if c? == "dispatch" {
                    found = true;
                }
            }
            found
        };
        if !has_dispatch {
            // The R10.M1 backfill: ~72 s on the 4.3 GB prod store (DEPLOY-9h).
            notice("queue two records");
        }
        let tx = conn.transaction()?;
        let mut backfilled = 0usize;
        if !has_dispatch {
            tx.execute(
                "ALTER TABLE queue ADD COLUMN dispatch TEXT NOT NULL DEFAULT 'eager'",
                [],
            )?;
            tx.execute(
                "ALTER TABLE queue ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
            backfilled += tx.execute(
                "UPDATE queue SET dispatch='turn_end' WHERE intent LIKE 'turn_end%'",
                [],
            )?;
            backfilled += tx.execute(
                "UPDATE queue SET attempts=1 WHERE state IN ('delivered', 'failed')",
                [],
            )?;
        }
        let cancelled: Vec<(i64, String, u64)> = {
            let mut stmt = tx.prepare(
                "SELECT id, thread_id, message_id FROM queue
                 WHERE state='cancelled' ORDER BY thread_id, id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?.max(0) as u64,
                ))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        if has_dispatch && !cancelled.is_empty() {
            notice("queue cancelled -> withdrawn");
        }
        let mut by_thread: std::collections::BTreeMap<String, Vec<(i64, u64)>> =
            std::collections::BTreeMap::new();
        for (row_id, thread_id, mid) in &cancelled {
            by_thread
                .entry(thread_id.clone())
                .or_default()
                .push((*row_id, *mid));
        }
        for (thread_id, rows) in &by_thread {
            let seq: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE thread_id = ?1",
                params![thread_id],
                |r| r.get(0),
            )?;
            let ev = StoredEvent {
                thread_id: thread_id.clone(),
                seq: seq.max(1) as u64,
                message_id: None,
                ts: now_rfc3339(),
                event: ThreadEvent::Custom {
                    event_type: crate::event::delivery::WITHDRAWN.to_string(),
                    data: serde_json::json!({
                        "message_ids": rows.iter().map(|(_, m)| *m).collect::<Vec<u64>>(),
                        "by": "migration",
                        "from_state": "cancelled",
                        "dispatch_now": crate::event::delivery::NEVER,
                    }),
                },
            };
            Self::insert_event_tx(&tx, &ev)?;
            for (row_id, _) in rows {
                tx.execute(
                    "UPDATE queue SET state='waiting', dispatch='never' WHERE id=?1 AND state='cancelled'",
                    params![row_id],
                )?;
            }
            tx.execute(
                "UPDATE threads SET last_seq=?2 WHERE id=?1 AND last_seq < ?2",
                params![thread_id, seq],
            )?;
        }
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_queue_thread_state_dispatch
                 ON queue(thread_id, state, dispatch)",
            [],
        )?;
        tx.commit()?;
        if !has_dispatch || !cancelled.is_empty() {
            tracing::info!(
                added_columns = !has_dispatch,
                backfilled,
                cancelled_to_withdrawn = cancelled.len(),
                threads = by_thread.len(),
                "queue migrated to the two records (dispatch + attempts)"
            );
        }
        Ok(())
    }

    /// **`pool_shares.share == 0` (the 2026-09-08 disable) → a row in
    /// `account_disabled` (the 2026-09-20 one), with the share PUT BACK.**
    ///
    /// The operator separated the two questions:
    ///
    /// > *"I want disabled to be independent of share. Whether to include it in
    /// > the pool supersedes how much should be made available to the pool."*
    ///
    /// Under the old model a disable OVERWROTE the share with zero and left a
    /// memo (`prior_share`) so an enable could reconstruct it. Under the new one
    /// the share is never touched, so every database on disk carries shares the
    /// old model destroyed and a memo of what they were. This migration spends
    /// that memo, once, and is the last reader `prior_share` ever has.
    ///
    /// **It is [`crate::budgets::Budgets::restore_share`]'s four rungs, executed
    /// in SQL and then deleted from the code.** For each row with `share = 0`:
    ///
    /// * `prior_share` is a usable share → that is what he had. Put it back and
    ///   clear the memo.
    /// * `prior_share` is NULL (disabled before the memo existed, or an account
    ///   with no share of its own to displace) → DELETE the `pool_shares` row
    ///   entirely. That is not data loss: the row's only content was the zero,
    ///   which is now meaningless, and deleting it is what restores the
    ///   precedence fall-through the old rungs 2 and 3 hand-rolled — the share
    ///   resolves to what `pool.json` declares, else the whole account. Keeping
    ///   the row would have left a zero in a column that now refuses zeroes.
    ///
    /// The `account_disabled` row is inserted for BOTH cases, because both were
    /// disabled accounts and the operator must not find an account quietly back
    /// in his pool after an upgrade. `updated_at` becomes `disabled_at` — the
    /// closest true instant on record; the note rides across unchanged.
    ///
    /// Idempotent: after it runs there are no `share = 0` rows left, so a second
    /// open matches nothing. `INSERT OR IGNORE` so a hand-made disable that
    /// already exists is never re-dated.
    fn migrate_share_zero_disable(&self, notice: &dyn Fn(&'static str)) -> Result<()> {
        let conn = self.conn()?;
        // Counted first so the log line can say whether anything happened at
        // all. A silent migration over a live store is one nobody can confirm
        // ran, and this one moves the operator's own pool membership.
        let pending: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pool_shares WHERE share = 0.0",
            [],
            |r| r.get(0),
        )?;
        if pending == 0 {
            return Ok(());
        }
        notice("share-zero disable");
        conn.execute_batch(
            "INSERT OR IGNORE INTO account_disabled (account, disabled_at, note)
                 SELECT account, updated_at, note FROM pool_shares WHERE share = 0.0;
             UPDATE pool_shares
                 SET share = prior_share, prior_share = NULL
                 WHERE share = 0.0
                   AND prior_share IS NOT NULL
                   AND prior_share > 0.0 AND prior_share <= 1.0;
             DELETE FROM pool_shares WHERE share = 0.0;",
        )?;
        tracing::info!(
            accounts = pending,
            "migrated the share-zero disable onto `account_disabled`; every account that was \
             disabled stays disabled and each one's share is back at what it was lent"
        );
        Ok(())
    }

    /// **`account_pool_shares` (keyed on the LOGIN) → `pool_shares` (keyed on
    /// the ACCOUNT NAME).**
    ///
    /// The operator's ruling, 2026-08-27: *"I think it's actually better to key
    /// on the account name instead of on the login slot, because the login slot
    /// is subject to change."* See `schema.sql` for the full reasoning.
    ///
    /// A real data migration rather than a column add, because the KEY changed
    /// and SQLite cannot re-key in place. The old table already carried
    /// `account_label` — written as a diagnostic, never as the key — and that
    /// column is exactly the new key, so no row is lost and nothing has to be
    /// guessed. `DO NOTHING` on conflict because two labels of one login could
    /// each have written a row under the same login key's `account_label` only
    /// by being written twice; the first wins and the second is a duplicate of
    /// a share the operator set for a name that already has one.
    ///
    /// Idempotent, and a no-op on every database that never had the old table —
    /// which on 2026-08-27 is EVERY database including the live one, because the
    /// daemon has not restarted since the old table was added. It is written
    /// anyway: the migration must be correct for the store the operator is
    /// actually carrying, and "it happened not to matter this time" is not a
    /// property a migration gets to rely on.
    fn rekey_pool_shares_on_name(&self, notice: &dyn Fn(&'static str)) -> Result<()> {
        let conn = self.conn()?;
        let had_old: bool = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='account_pool_shares'",
            [],
            |r| r.get::<_, i64>(0),
        )? > 0;
        if !had_old {
            return Ok(());
        }
        notice("pool shares by name");
        conn.execute_batch(
            "INSERT INTO pool_shares (account, share, note, updated_at)
                 SELECT account_label, share, note, updated_at FROM account_pool_shares
                 WHERE account_label <> ''
             ON CONFLICT(account) DO NOTHING;
             DROP TABLE account_pool_shares;",
        )?;
        tracing::info!("re-keyed the pool share table from the login onto the account name");
        Ok(())
    }

    /// Execute raw SQL against the store.
    ///
    /// Exists for TESTS and hand maintenance — the hardening suite uses it to
    /// simulate a corrupted/unavailable ledger and assert that admission fails
    /// CLOSED (round-7 item 1). Nothing in the daemon calls it.
    pub fn execute_sql(&self, sql: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(sql)?;
        Ok(())
    }

    // ── threads ──────────────────────────────────────────────────────────

    /// Insert (or refresh the settings of) a thread row.
    pub fn upsert_thread(&self, t: &ThreadRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO threads
                (id, account, cwd, model, settings_json, last_seq, next_message_id,
                 last_compaction_message_id, needs_restate, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                account=excluded.account, cwd=excluded.cwd, model=excluded.model,
                settings_json=excluded.settings_json, updated_at=excluded.updated_at",
            params![
                t.id,
                t.account,
                t.cwd,
                t.model,
                t.settings_json,
                t.last_seq as i64,
                t.next_message_id as i64,
                t.last_compaction_message_id as i64,
                t.needs_restate as i64,
                t.created_at,
                t.updated_at,
            ],
        )?;
        Ok(())
    }

    fn map_thread(r: &rusqlite::Row) -> rusqlite::Result<ThreadRow> {
        Ok(ThreadRow {
            id: r.get(0)?,
            account: r.get(1)?,
            cwd: r.get(2)?,
            model: r.get(3)?,
            settings_json: r.get(4)?,
            last_seq: r.get::<_, i64>(5)? as u64,
            next_message_id: r.get::<_, i64>(6)? as u64,
            last_compaction_message_id: r.get::<_, i64>(7)? as u64,
            needs_restate: r.get::<_, i64>(8)? != 0,
            created_at: r.get(9)?,
            updated_at: r.get(10)?,
        })
    }

    const THREAD_COLS: &'static str = "id, account, cwd, model, settings_json, last_seq, \
         next_message_id, last_compaction_message_id, needs_restate, created_at, updated_at";

    pub fn get_thread(&self, id: &str) -> Result<Option<ThreadRow>> {
        let conn = self.read()?;
        let sql = format!("SELECT {} FROM threads WHERE id = ?1", Self::THREAD_COLS);
        // PL-2d: only "no row" is `None`. Any other SQL failure (a locked
        // DB, a schema mismatch, I/O) is an error, never "no such thread".
        Ok(conn.query_row(&sql, params![id], Self::map_thread).optional()?)
    }

    pub fn list_threads(&self, limit: usize) -> Result<Vec<ThreadRow>> {
        let conn = self.read()?;
        let sql = format!(
            "SELECT {} FROM threads ORDER BY updated_at DESC LIMIT ?1",
            Self::THREAD_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![limit as i64], Self::map_thread)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// **The `last_seq` watermark for NAMED threads, in ONE statement.**
    ///
    /// It exists because the only caller that wanted it — the `accounts` shape's
    /// hydrate for its `threads` cell — was calling [`Self::get_thread`] once
    /// per thread inside a hydrate path, which is the N+1 `docs/shapes.md` 4.3
    /// forbids a producer to owe. `get_thread` reads eleven columns to reach one
    /// of them; this reads the one, for every id the caller names, on a single
    /// connection acquisition, so the read cost is FLAT in the number of ids.
    ///
    /// Ids that name no thread are simply absent from the map, exactly as
    /// [`crate::runtime::ThreadManager::thread_states`] leaves them absent.
    ///
    /// The caller must bound `ids` — every one becomes a bound parameter, and
    /// SQLite's `SQLITE_MAX_VARIABLE_NUMBER` is the ceiling. The one caller
    /// bounds it at [`crate::runtime::MAX_NON_IDLE_HYDRATE_ROWS`], which is
    /// chosen to sit under the most conservative build of that limit.
    pub fn last_seqs(&self, ids: &[String]) -> Result<HashMap<String, u64>> {
        if ids.is_empty() {
            // NO CONNECTION ACQUIRED. A caller with nothing to ask must not pay
            // a trip, and the trip-count test in `tests/hydrate_cost.rs` reads
            // this as the zero it is.
            return Ok(HashMap::new());
        }
        let conn = self.read()?;
        let placeholders = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT id, last_seq FROM threads WHERE id IN ({placeholders})");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// **EACH THREAD'S NEWEST EVENT TIME** (TC-44's recency key for a managed
    /// child): `id → ts` of the highest-`seq` event, one backwards seek on
    /// `idx_events_thread_seq` per id, all inside ONE statement per 500 ids. An
    /// id with no event (or no row) is absent from the map.
    pub fn newest_event_ts_many(&self, ids: &[String]) -> Result<HashMap<String, String>> {
        let mut out = HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let conn = self.read()?;
        for chunk in ids.chunks(500) {
            let placeholders = std::iter::repeat("?").take(chunk.len()).collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT t.id, (SELECT e.ts FROM events e WHERE e.thread_id = t.id ORDER BY e.seq DESC LIMIT 1)
                   FROM threads t WHERE t.id IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?;
            for row in rows {
                if let (id, Some(ts)) = row? {
                    out.insert(id, ts);
                }
            }
        }
        Ok(out)
    }

    /// Advance a thread's `last_seq` watermark (and its `updated_at`, which is
    /// what `list_threads` orders by — so a thread that just changed sorts to the
    /// top of a CONTENT page).
    ///
    /// This is the SURVIVING half of the old `update_thread_status`, which
    /// carried a status word and this watermark in one write. The status word is
    /// gone (see [`DROPPED_COLUMNS`]); the watermark is real durable state that
    /// nothing else records, so it kept its write and lost its passenger.
    ///
    /// # `user_input_at` — the RECENCY stamp, and why it rides this statement
    ///
    /// `Some(ts)` when the append that called this was a `user_message`, and
    /// `None` for every other append; the caller decides through
    /// [`crate::event::ThreadEvent::is_user_message`], which is also what the
    /// `threads/recent-updates` fold decides through, so the column and the live
    /// order cannot be fed by two different definitions of "he spoke".
    ///
    /// It is a passenger on THIS statement rather than a second `UPDATE`
    /// deliberately. Every ledger append runs this write — 2.46 M rows at ~50/s
    /// peak — and a second statement would double that, for a column that moves
    /// on 9,496 of those rows. The `CASE WHEN ?4 IS NULL` is what makes one
    /// statement serve both callers: a non-user append re-writes the column with
    /// its own current value, which is a no-op the row rewrite was already paying
    /// for.
    ///
    /// **`updated_at` is NOT redundant with it and is not being replaced.** They
    /// answer different questions — *when did anything about this row change*
    /// versus *when did HE last speak* — and the whole defect this column fixes
    /// was the recency index reading the first one for the second question.
    pub fn bump_last_seq(&self, id: &str, last_seq: u64, user_input_at: Option<&str>) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE threads
                SET last_seq=?2,
                    updated_at=?3,
                    last_user_input_at = CASE WHEN ?4 IS NULL
                                              THEN last_user_input_at
                                              ELSE ?4 END
              WHERE id=?1",
            params![id, last_seq as i64, now_rfc3339(), user_input_at],
        )?;
        Ok(())
    }

    /// **WHEN HE LAST SPOKE IN ONE THREAD**, read back off the column — `None`
    /// for a thread he has never spoken in, which is a real state and not a
    /// missing value (see `schema.sql`).
    ///
    /// A dedicated accessor and not a [`ThreadRow`] field, for `last_meta_date`'s
    /// reason: it is written on the append path and read by the recency order,
    /// and no row-shaped consumer wants it.
    pub fn last_user_input_at(&self, id: &str) -> Result<Option<String>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT last_user_input_at FROM threads WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// **THE ONE-TIME BACKFILL OF `threads.last_user_input_at`** — run after
    /// [`MIGRATIONS`] on every open, matching nothing from the second open on.
    ///
    /// # Why a backfill at all, rather than NULL-and-fall-back
    ///
    /// The alternative was to leave existing rows NULL and have the hydrate fall
    /// back to `updated_at` for them. That is the defect kept alive under a
    /// condition: every thread on disk today would keep being ordered by
    /// last-any-event, forever, and the only way to leave that state would be to
    /// speak in the thread. It would also make the live fold and the hydrate
    /// disagree for exactly those threads, which is the divergence the whole
    /// repair exists to close. So the column is filled, once, and NULL is left to
    /// mean only what `schema.sql` says it means: he has never spoken here.
    ///
    /// # Why this is affordable, stated against the ledger's size
    ///
    /// `events` is 2.46 M rows / 3.8 GB, and
    /// `crate::shapes::summary`'s module doc records a standing REFUSAL to add an
    /// index to it. This read needs none:
    ///
    /// ```text
    /// SCAN threads USING COVERING INDEX sqlite_autoindex_threads_1
    /// CORRELATED SCALAR SUBQUERY
    ///   SEARCH events USING INDEX idx_events_thread_kind (thread_id=? AND kind=?)
    /// ```
    ///
    /// It is 5,421 index SEARCHES on an index that already exists, one per
    /// thread, each over only that thread's `user_message` rows — 9,496 of them
    /// fleet-wide. **Measured on the production store at 1.35 s wall / 0.16 s
    /// CPU cold.** What it is deliberately NOT is a fleet-wide `GROUP BY` over
    /// `events`, which `idx_events_thread_kind` cannot serve on `kind` alone and
    /// which would be the scan that refusal is about.
    ///
    /// `MAX(ts)` is the newest stamp because `ts` is RFC3339 UTC written by
    /// [`now_rfc3339`], whose text order is its chronological order — the same
    /// property `recent_threads`' `ORDER BY` has always relied on.
    ///
    /// # Idempotence, and what the guard costs afterwards
    ///
    /// `WHERE last_user_input_at IS NULL` — so after one run the only rows it
    /// re-examines are the threads that genuinely have no `user_message` (three,
    /// of 5,421, on the production store). A fresh database matches nothing at
    /// all. There is no marker table: the column's own NULL is the marker, which
    /// is why the guard is on it rather than on a schema version.
    fn backfill_last_user_input_at(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE threads
                SET last_user_input_at = (
                      SELECT MAX(e.ts) FROM events e
                       WHERE e.thread_id = threads.id
                         AND e.kind = 'user_message')
              WHERE last_user_input_at IS NULL",
            [],
        )?;
        Ok(())
    }

    /// Persist the live settings blob (threads.update).
    pub fn update_thread_settings(
        &self,
        id: &str,
        settings_json: &str,
        account: Option<&str>,
        cwd: &str,
        model: &str,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE threads SET settings_json=?2, account=?3, cwd=?4, model=?5, updated_at=?6
             WHERE id=?1",
            params![id, settings_json, account, cwd, model, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Persist the message-id counter after an allocation.
    pub fn update_thread_counters(&self, id: &str, next_message_id: u64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE threads SET next_message_id=?2, updated_at=?3 WHERE id=?1",
            params![id, next_message_id as i64, now_rfc3339()],
        )?;
        Ok(())
    }

    /// The calendar day last STATED in a per-message metadata block, or `None`
    /// when this thread has never been told one (INTENT 65).
    ///
    /// A dedicated accessor rather than a [`ThreadRow`] field, deliberately:
    /// it is read and written on the delivery path only, and every other
    /// consumer of a thread row has no business with it.
    pub fn get_thread_meta_date(&self, id: &str) -> Result<Option<String>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT last_meta_date FROM threads WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Record the calendar day just stated, so the NEXT delivery knows whether
    /// the day has rolled over. Persisted rather than held in memory because
    /// the daemon restarts itself on binary change and must not restate the
    /// date merely because it was updated.
    pub fn set_thread_meta_date(&self, id: &str, date: Option<&str>) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE threads SET last_meta_date=?2, updated_at=?3 WHERE id=?1",
            params![id, date, now_rfc3339()],
        )?;
        Ok(())
    }

    /// **INTERIM — see [`MIGRATIONS`].** The context id last stated in a
    /// per-message metadata block, or `None` when this thread has never been
    /// told one. Read to decide whether the CURRENT context has already carried
    /// the id, which is what makes turning `inject_context_id` on mid-context
    /// take effect on the next post rather than at the next compaction.
    ///
    /// A dedicated accessor rather than a [`ThreadRow`] field, for
    /// `last_meta_date`'s reason: the delivery path is its only consumer.
    pub fn get_thread_context_id(&self, id: &str) -> Result<Option<String>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT last_context_id FROM threads WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Record the context id just stated. **INTERIM — see [`MIGRATIONS`].**
    pub fn set_thread_context_id(&self, id: &str, context_id: Option<&str>) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE threads SET last_context_id=?2, updated_at=?3 WHERE id=?1",
            params![id, context_id, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Persist compaction bookkeeping (§12): the restate flag and the highest
    /// message id the last compaction swallowed.
    pub fn update_thread_compaction(
        &self,
        id: &str,
        last_compaction_message_id: u64,
        needs_restate: bool,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE threads SET last_compaction_message_id=?2, needs_restate=?3, updated_at=?4
             WHERE id=?1",
            params![
                id,
                last_compaction_message_id as i64,
                needs_restate as i64,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    // `sweep_stale_busy` used to live here: a boot-time `UPDATE threads SET
    // status='idle'` that existed only to repair the cache that is now deleted.
    // It was the LAST of four repair mechanisms and the only one that ever
    // actually fired, which is another way of saying the other three did not
    // work. A derived answer needs no repair — deleting the column deleted the
    // sweep. Do not bring either back.

    // ── the event ledger ─────────────────────────────────────────────────

    /// Append one sequenced event (idempotent on `(thread_id, seq)`).
    pub fn append_event(&self, ev: &StoredEvent) -> Result<()> {
        let conn = self.conn()?;
        let payload = serde_json::to_string(&ev.event)?;
        conn.execute(
            "INSERT INTO events (thread_id, seq, message_id, kind, payload_json, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(thread_id, seq) DO NOTHING",
            params![
                ev.thread_id,
                ev.seq as i64,
                ev.message_id.map(|m| m as i64),
                ev.event.kind(),
                payload,
                ev.ts,
            ],
        )?;
        Ok(())
    }

    fn map_event(r: &rusqlite::Row) -> rusqlite::Result<(u64, Option<u64>, String, String)> {
        Ok((
            r.get::<_, i64>(0)? as u64,
            r.get::<_, Option<i64>>(1)?.map(|m| m as u64),
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    }

    /// Turn ledger rows into events, **degrading a row this build cannot parse
    /// instead of failing the whole page**.
    ///
    /// This is the read half of the shape-mismatch rule, and it is load-bearing
    /// well beyond tidiness. [`Self::list_events`] is `threads.history`, and
    /// `threads.history` is the ONE call every recovery path in the system goes
    /// through: the AUI's outstanding-turn sweep after a restart, the reconnect
    /// backfill, and the lag recovery a client is told to run when the daemon's
    /// fan-out overflows. It used to deserialize with `?`, so a SINGLE row this
    /// binary could not read — a payload written by a newer daemon, a
    /// hand-repaired row, a truncated write — failed the entire page, and every
    /// one of those recovery paths came back empty. Empty is indistinguishable
    /// from "nothing was missed". One unreadable row could therefore silence a
    /// whole thread's recovery, which is the exact failure the recovery machinery
    /// exists to prevent.
    ///
    /// So an unreadable payload becomes [`ThreadEvent::Unknown`] carrying the
    /// bytes VERBATIM, and the page is still returned. Three properties matter:
    ///
    /// * **Nothing is invented.** The raw payload is preserved exactly, so a
    ///   later build that understands the kind can re-read the same row.
    /// * **The audience split is preserved** (DEC-55). `Unknown` is
    ///   [`crate::event::Audience::LedgerOnly`] by construction, so a row that
    ///   degraded here can never be replayed into a model's context. Degrading
    ///   cannot promote anything.
    /// * **It is never silent.** Every degraded row increments
    ///   [`unreadable_ledger_rows`] and the first one of each kind logs at WARN.
    ///   Repeats drop to DEBUG because a state that has not changed is not news,
    ///   and 133,000 identical warnings is how the last one of these stayed
    ///   invisible for a month.
    fn rows_to_events(
        thread_id: &str,
        rows: Vec<(u64, Option<u64>, String, String)>,
    ) -> Result<Vec<StoredEvent>> {
        let mut out = Vec::with_capacity(rows.len());
        for (seq, message_id, ts, payload) in rows {
            let event = match serde_json::from_str::<ThreadEvent>(&payload) {
                Ok(ev) => ev,
                Err(e) => degrade_unreadable_row(thread_id, seq, &payload, &e),
            };
            out.push(StoredEvent {
                thread_id: thread_id.to_string(),
                seq,
                message_id,
                ts,
                event,
            });
        }
        Ok(out)
    }

    /// Paged history: events with `seq > after_seq`, ascending, capped at
    /// `limit`, optionally filtered to `kinds` (G6 mitigation for consumers
    /// that only want messages).
    pub fn list_events(
        &self,
        thread_id: &str,
        after_seq: u64,
        limit: usize,
        kinds: Option<&[String]>,
    ) -> Result<Vec<StoredEvent>> {
        let conn = self.read()?;
        let mut sql = String::from(
            "SELECT seq, message_id, ts, payload_json FROM events
             WHERE thread_id = ?1 AND seq > ?2",
        );
        if let Some(ks) = kinds {
            if !ks.is_empty() {
                let placeholders: Vec<String> =
                    (0..ks.len()).map(|i| format!("?{}", i + 4)).collect();
                sql.push_str(&format!(" AND kind IN ({})", placeholders.join(",")));
            }
        }
        sql.push_str(" ORDER BY seq ASC LIMIT ?3");
        let mut stmt = conn.prepare(&sql)?;
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(thread_id.to_string()),
            Box::new(after_seq as i64),
            Box::new(limit as i64),
        ];
        if let Some(ks) = kinds {
            for k in ks {
                params_vec.push(Box::new(k.clone()));
            }
        }
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
            |r| Self::map_event(r),
        )?;
        let rows: Vec<_> = rows.collect::<std::result::Result<_, _>>()?;
        Self::rows_to_events(thread_id, rows)
    }

    /// **THE NEWEST `limit` EVENTS OF ONE THREAD, OLDEST FIRST** — the live
    /// window's hydrate read, and the one statement
    /// `crate::shapes::log::ThreadTail` is.
    ///
    /// ONE seek plus a backward walk on `idx_events_thread_seq`, so it is flat in
    /// the thread's size: production's largest thread carries a 177,995-entry
    /// tail and this reads `limit` of them, never 177,995. That is the whole
    /// reason the window is a declared ceiling rather than *"the log"* — see
    /// [`crate::shapes::log::WINDOW`].
    ///
    /// **It is NOT [`Self::list_events`] with a cursor.** `list_events` is
    /// `seq > after_seq ASC` — a forward page from a position the CONSUMER holds,
    /// which is the paged read rule 2.5 is deliberately not engaged by. This has
    /// no cursor at all and cannot be given one: the answer is *the recent end*,
    /// and the recent end is a fact about the log rather than a position in it.
    ///
    /// **It takes NO `kinds` filter, deliberately.** A filter is a VIEW, and the
    /// shape carries facts; see [`crate::shapes::log`]'s module doc for the
    /// argument and for what it costs.
    pub fn tail_events(&self, thread_id: &str, limit: usize) -> Result<Vec<StoredEvent>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT seq, message_id, ts, payload_json FROM events
             WHERE thread_id = ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![thread_id, limit as i64], |r| {
            Self::map_event(r)
        })?;
        let mut rows: Vec<_> = rows.collect::<std::result::Result<_, _>>()?;
        // Read newest-first because that is what the index can do in one seek;
        // handed over OLDEST-first because that is the order a `Sequence` holds
        // and exports, so nothing reverses anything again downstream.
        rows.reverse();
        Self::rows_to_events(thread_id, rows)
    }

    /// **THE NEWEST SEQ OF ONE EVENT KIND IN ONE THREAD**, or `None` when the
    /// thread has none. One SEARCH on `idx_events_thread_kind`
    /// `(thread_id, kind)` over only that thread's rows of that kind — flat in
    /// the length of the run, which is the property a fixed-size tail lacked
    /// (`managed::run_ended`, H179: a 495-event child run pushed its own
    /// `user_message` out of a 400-event tail and was never reported).
    pub fn last_seq_of_kind(&self, thread_id: &str, kind: &str) -> Result<Option<u64>> {
        let conn = self.read()?;
        let v: Option<i64> = conn.query_row(
            "SELECT MAX(seq) FROM events WHERE thread_id = ?1 AND kind = ?2",
            params![thread_id, kind],
            |r| r.get(0),
        )?;
        Ok(v.map(|s| s.max(0) as u64))
    }

    /// **THE NEWEST EVENT OF ONE KIND IN ONE THREAD**, or `None`. One search
    /// on `idx_events_thread_kind` over that thread's rows of that kind, so it
    /// is flat in the length of the run — never a fixed-size tail. With
    /// `top_level`, a row whose payload names a `parent_tool_use_id` (a native
    /// subagent's line inside the turn) is skipped. Boot reconciliation reads a
    /// managed child's terminal state through this (`managed::terminal_from_store`,
    /// `docs/SUBAGENT-WAKE.md`).
    pub fn newest_of_kind(&self, thread_id: &str, kind: &str, top_level: bool) -> Result<Option<StoredEvent>> {
        let conn = self.read()?;
        let sql = if top_level {
            "SELECT seq, message_id, ts, payload_json FROM events
             WHERE thread_id = ?1 AND kind = ?2
               AND json_extract(payload_json, '$.parent_tool_use_id') IS NULL
             ORDER BY seq DESC LIMIT 1"
        } else {
            "SELECT seq, message_id, ts, payload_json FROM events
             WHERE thread_id = ?1 AND kind = ?2 ORDER BY seq DESC LIMIT 1"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![thread_id, kind], |r| Self::map_event(r))?;
        let rows: Vec<_> = rows.collect::<std::result::Result<_, _>>()?;
        Ok(Self::rows_to_events(thread_id, rows)?.pop())
    }

    /// **ONE PAGE OF A THREAD'S LOG, OLDEST FIRST, FROM A SEQ.** Every kind —
    /// the `threads/log` shape's row set, read as a page rather than a window:
    /// `seq >= from_seq`, at most `limit` rows, one index seek on
    /// `idx_events_thread_seq`. The `agents/{agent}/log` read (R4.S1) is the
    /// caller; it builds rows through `shapes::log::row_of` exactly as the
    /// shape's hydrate does, so a page here and a window there are the same
    /// rows.
    pub fn events_from(&self, thread_id: &str, from_seq: u64, limit: usize) -> Result<Vec<StoredEvent>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT seq, message_id, ts, payload_json FROM events
             WHERE thread_id = ?1 AND seq >= ?2 ORDER BY seq ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![thread_id, from_seq as i64, limit as i64],
            |r| Self::map_event(r),
        )?;
        let rows: Vec<_> = rows.collect::<std::result::Result<_, _>>()?;
        Self::rows_to_events(thread_id, rows)
    }

    /// Retrieve events by message-id range / kind filter / tail (G20 — the
    /// `--from/--to/--kinds/--tail` mirrors). `tail` wins when set: the last N
    /// addressable events.
    pub fn events_by_range(
        &self,
        thread_id: &str,
        from: Option<u64>,
        to: Option<u64>,
        kinds: Option<&[String]>,
        tail: Option<usize>,
    ) -> Result<Vec<StoredEvent>> {
        let conn = self.read()?;
        let mut clauses = vec!["thread_id = ?1".to_string(), "message_id IS NOT NULL".to_string()];
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(thread_id.to_string())];
        let mut next = 2;
        if let Some(f) = from {
            clauses.push(format!("message_id >= ?{next}"));
            params_vec.push(Box::new(f as i64));
            next += 1;
        }
        if let Some(t) = to {
            clauses.push(format!("message_id <= ?{next}"));
            params_vec.push(Box::new(t as i64));
            next += 1;
        }
        if let Some(ks) = kinds {
            if !ks.is_empty() {
                let ph: Vec<String> = ks
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", next + i))
                    .collect();
                clauses.push(format!("kind IN ({})", ph.join(",")));
                for k in ks {
                    params_vec.push(Box::new(k.clone()));
                }
            }
        }
        let base = format!(
            "SELECT seq, message_id, ts, payload_json FROM events WHERE {}",
            clauses.join(" AND ")
        );
        let sql = match tail {
            Some(n) => format!(
                "SELECT * FROM ({base} ORDER BY message_id DESC LIMIT {n}) ORDER BY message_id ASC"
            ),
            None => format!("{base} ORDER BY message_id ASC"),
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
            |r| Self::map_event(r),
        )?;
        let rows: Vec<_> = rows.collect::<std::result::Result<_, _>>()?;
        Self::rows_to_events(thread_id, rows)
    }

    // ── the citation query (INTENT 57) ───────────────────────────────────

    /// **THE CITATION QUERY.** One paged read behind `threads.query`, so a
    /// caller can hand a sub-agent `#1412` or `#1390..#1412` instead of a
    /// transcription of what was said.
    ///
    /// Selection is by MESSAGE ID (the spoken ordinal — the thing a caller
    /// actually cites), so rows without one are never returned. `ids` wins
    /// when present; otherwise `from`/`to`/`cursor` bound the range and `tail`
    /// takes the last N of whatever the other clauses left.
    ///
    /// Returns the page AND the total number of rows that matched before the
    /// limit, because a caller citing a range needs to know it did not get all
    /// of it.
    pub fn query_messages(&self, thread_id: &str, q: &MessageQuery<'_>) -> Result<MessagePage> {
        let conn = self.read()?;
        let mut clauses = vec![
            "thread_id = ?1".to_string(),
            "message_id IS NOT NULL".to_string(),
        ];
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(thread_id.to_string())];
        let mut next = 2;
        if let Some(ids) = q.ids {
            if ids.is_empty() {
                return Ok(MessagePage::default());
            }
            let ph: Vec<String> = (0..ids.len()).map(|i| format!("?{}", next + i)).collect();
            clauses.push(format!("message_id IN ({})", ph.join(",")));
            for id in ids {
                binds.push(Box::new(*id as i64));
            }
            next += ids.len();
        }
        if let Some(f) = q.from {
            clauses.push(format!("message_id >= ?{next}"));
            binds.push(Box::new(f as i64));
            next += 1;
        }
        if let Some(t) = q.to {
            clauses.push(format!("message_id <= ?{next}"));
            binds.push(Box::new(t as i64));
            next += 1;
        }
        if let Some(c) = q.cursor {
            clauses.push(format!("message_id > ?{next}"));
            binds.push(Box::new(c as i64));
            next += 1;
        }
        if let Some(ks) = q.kinds {
            if ks.is_empty() {
                return Ok(MessagePage::default());
            }
            let ph: Vec<String> = (0..ks.len()).map(|i| format!("?{}", next + i)).collect();
            clauses.push(format!("kind IN ({})", ph.join(",")));
            for k in ks {
                binds.push(Box::new(k.clone()));
            }
        }
        let where_sql = clauses.join(" AND ");
        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM events WHERE {where_sql}"),
            rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
            |r| r.get(0),
        )?;
        let base = format!(
            "SELECT seq, message_id, ts, payload_json FROM events WHERE {where_sql}"
        );
        // `limit + 1` is how `has_more` is answered without a second count of
        // the page itself.
        let probe = q.limit.saturating_add(1);
        let sql = match q.tail {
            Some(n) => format!(
                "SELECT * FROM ({base} ORDER BY message_id DESC LIMIT {}) ORDER BY message_id ASC",
                n.min(probe)
            ),
            None => format!("{base} ORDER BY message_id ASC LIMIT {probe}"),
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
            |r| Self::map_event(r),
        )?;
        let rows: Vec<_> = rows.collect::<std::result::Result<_, _>>()?;
        let mut events = Self::rows_to_events(thread_id, rows)?;
        let has_more = events.len() > q.limit;
        if has_more {
            events.truncate(q.limit);
        }
        let next_cursor = if has_more {
            events.last().and_then(|e| e.message_id)
        } else {
            None
        };
        Ok(MessagePage {
            events,
            total_matched: total.max(0) as usize,
            next_cursor,
            has_more,
        })
    }

    // ── the interrupted flag (DEC-56) ────────────────────────────────────

    /// Flag this thread INTERRUPTED, with the instant. Idempotent per thread:
    /// a second hard break before the first was ever reported replaces the
    /// row, because the thread is owed an account of the break that is still
    /// holding it, not of every break it has ever had.
    pub fn set_thread_interrupt(&self, row: &ThreadInterruptRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO thread_interrupts
                (thread_id, trigger_kind, detail, switched, from_account, to_account, interrupted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(thread_id) DO UPDATE SET
                trigger_kind=excluded.trigger_kind, detail=excluded.detail,
                switched=excluded.switched, from_account=excluded.from_account,
                to_account=excluded.to_account, interrupted_at=excluded.interrupted_at",
            params![
                row.thread_id,
                row.trigger,
                row.detail,
                row.switched as i64,
                row.from_account,
                row.to_account,
                row.interrupted_at,
            ],
        )?;
        Ok(())
    }

    /// The pending interruption for a thread — `None` when it is not
    /// interrupted (which is also DEC-57's answer for a merely QUEUED turn).
    pub fn get_thread_interrupt(&self, thread_id: &str) -> Result<Option<ThreadInterruptRow>> {
        let conn = self.read()?;
        let row = conn
            .query_row(
                "SELECT thread_id, trigger_kind, detail, switched, from_account,
                        to_account, interrupted_at
                 FROM thread_interrupts WHERE thread_id = ?1",
                params![thread_id],
                |r| {
                    Ok(ThreadInterruptRow {
                        thread_id: r.get(0)?,
                        trigger: r.get(1)?,
                        detail: r.get(2)?,
                        switched: r.get::<_, i64>(3)? != 0,
                        from_account: r.get(4)?,
                        to_account: r.get(5)?,
                        interrupted_at: r.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Clear the flag — called only once the thread has actually been TOLD.
    pub fn clear_thread_interrupt(&self, thread_id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM thread_interrupts WHERE thread_id = ?1",
            params![thread_id],
        )?;
        Ok(())
    }

    /// The highest seq recorded for a thread (0 if none) — resume watermark.
    pub fn max_seq(&self, thread_id: &str) -> Result<u64> {
        let conn = self.read()?;
        let seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE thread_id = ?1",
            params![thread_id],
            |r| r.get(0),
        )?;
        Ok(seq.max(0) as u64)
    }

    // ── THE SHAPE READS (2026-09-26) ─────────────────────────────────────
    //
    // Four reads added for `crate::shapes::threads` and
    // `crate::shapes::summary`. Every one is ONE statement and none of them
    // loops over ids — the `accounts` shape's repaired `list_threads(100_000)`
    // plus N+1 is the pattern these must not copy.

    /// **WHERE THE LOG'S HEAD IS — `{seq, message_id, kind, ts}`, plus the
    /// newest event's raw payload.**
    ///
    /// ONE seek: `idx_events_thread_seq` is `(thread_id, seq)`, so the descending
    /// limit-1 walks the index backwards from this thread's last entry and reads
    /// exactly one row. `None` for a thread with no events, which is a complete
    /// answer about the world and not a failure.
    ///
    /// **This is the authority for a thread's head, deliberately in preference to
    /// the `threads.last_seq` column**, which is a cache — `import.rs` inserts
    /// events without `bump_last_seq`, so the column can lag its own log.
    /// `docs/thread-shapes-plan.md` §3.4 asked the shape to pick one source; this
    /// is the pick.
    /// **AND IT CARRIES THE PAYLOAD, SINCE 2026-10-02.** The newest event's
    /// BODY is a resource (`shapes::body`), and its summary has to say how many
    /// bytes it is — which is a fact about the payload. Selecting one more
    /// column off the row this statement already reads keeps that at ONE db
    /// trip; a second `newest_body` query would have been a second statement on
    /// a path `tests/recent_hydrate_cost.rs` counts.
    pub fn newest_event(&self, thread_id: &str) -> Result<Option<NewestEventRow>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT seq, message_id, kind, ts, payload_json FROM events
                  WHERE thread_id = ?1 ORDER BY seq DESC LIMIT 1",
                params![thread_id],
                |r| {
                    Ok(NewestEventRow {
                        seq: r.get::<_, i64>(0)?.max(0) as u64,
                        message_id: r.get::<_, Option<i64>>(1)?.map(|m| m.max(0) as u64),
                        kind: r.get::<_, String>(2)?,
                        ts: r.get::<_, String>(3)?,
                        payload_json: r.get::<_, String>(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// **ONE EVENT BY ITS DURABLE KEY — what the resource door resolves an id
    /// to.**
    ///
    /// `(thread_id, seq)` is `UNIQUE` (`schema.sql`) and no `UPDATE events` or
    /// `DELETE FROM events` exists anywhere in this crate, so the row an id
    /// names is immutable by construction of the writer set. That is what makes
    /// a message body `Retention::Fixed` rather than a guess.
    ///
    /// The payload is returned RAW, as the column holds it. The caller decides
    /// what a body is; this does not parse.
    pub fn event_payload_at(&self, thread_id: &str, seq: u64) -> Result<Option<(String, String)>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT kind, payload_json FROM events WHERE thread_id = ?1 AND seq = ?2",
                params![thread_id, seq as i64],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?)
    }

    /// **ONE EVENT, BY `(thread_id, seq)`, AS A `StoredEvent`** — the resource
    /// door's read.
    ///
    /// It goes through [`Self::rows_to_events`] rather than handing back the raw
    /// column, so an unreadable payload is degraded to `Unknown { raw }` by the
    /// SAME code path every other read uses — one degrade site, one warning, one
    /// counter. That matters here specifically: the door's job is to hand over
    /// exactly the bytes a shape's summary said were there, and a second parse
    /// with its own error handling is a second answer to that question.
    pub fn event_at(&self, thread_id: &str, seq: u64) -> Result<Option<StoredEvent>> {
        let conn = self.read()?;
        let row = conn
            .query_row(
                "SELECT seq, message_id, ts, payload_json FROM events
                 WHERE thread_id = ?1 AND seq = ?2",
                params![thread_id, seq as i64],
                |r| Self::map_event(r),
            )
            .optional()?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Self::rows_to_events(thread_id, vec![row])?.into_iter().next())
    }

    /// **HOW MANY THREADS EXIST AT ALL.** One `COUNT(*)` over the primary-key
    /// index of a 5,417-row table, so a consumer holding three keys can tell
    /// whether that is the fleet or a window onto it.
    pub fn count_threads(&self) -> Result<u64> {
        let conn = self.read()?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM threads", [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    /// **EVERY THREAD ID THAT EXISTS, IN KEY ORDER** — the pool's membership.
    ///
    /// **ONE statement, ONE column, covered by the primary-key index**, and that
    /// is the whole affordability argument for `detail: keys`: at 5,417 threads
    /// this is ~200 KB of ids against the ~20 MB the rows would have been, and
    /// it never touches a row. It is deliberately NOT
    /// [`Self::list_threads`], which reads eleven columns and orders by
    /// `updated_at` — that is a page of CONTENT in recency order, a different
    /// question with a different cost.
    ///
    /// Bounded by the caller's `limit` so the answer cannot be larger than the
    /// ceiling the shape declares; the caller compares what it got against the
    /// limit to know whether the read was short, which is what makes the
    /// membership export's `complete` honest rather than assumed.
    /// **THE THREADS HE SPOKE IN MOST RECENTLY, NEWEST FIRST** — the durable
    /// order `crate::shapes::recent` hydrates its index from.
    ///
    /// # It orders on `last_user_input_at`, and that is a RULING
    ///
    /// Operator directive, verbatim: *"the recents need to be ordered on last
    /// user input, not on when they were created or any other events, including
    /// renames."*
    ///
    /// **This read ordered by `updated_at` until this change, and that was the
    /// defect.** `bump_last_seq` advances `updated_at` on every one of 2.46 M
    /// ledger appends, `update_thread_settings` advances it on a RENAME — the
    /// exclusion the directive names out loud — and `upsert_thread` stamps it at
    /// creation, which is the second. So the index this feeds re-sorted on
    /// last-any-event, and it did so at every re-initialisation even once the
    /// live fold had been fixed: a half-fix whose only symptom is that the order
    /// changes across a reconnect.
    ///
    /// # NULL is EXCLUDED, not sorted last
    ///
    /// A thread with no `last_user_input_at` is one he has never spoken in, and
    /// an order on last user input has no position to offer it. Any position
    /// invented for it — bottom, top, by `created_at` — would be an order on
    /// something the directive excludes, which is the thing being removed.
    ///
    /// Excluding it is also what makes the two halves of the order PROVABLY the
    /// same read: `crate::shapes::recent`'s fold moves a thread only on a
    /// `user_message`, so a never-spoken thread cannot enter the live order
    /// either, and `WHERE last_user_input_at IS NOT NULL` is that same rule
    /// spelled in SQL. A hydrate that padded the window with never-spoken threads
    /// would hand a reconnecting consumer entries the live fold would never have
    /// put there.
    ///
    /// **ONE statement, TWO columns, and it never touches a row.** It is
    /// deliberately not [`Self::list_threads`], which answers the same ORDER with
    /// eleven columns including the settings blob: the index carries ids and a
    /// position and nothing else, so reading a row to build it would be paying
    /// the 20 MB this whole design exists to avoid, and then throwing it away.
    ///
    /// It is also deliberately not [`Self::thread_ids`], which is the same table
    /// in KEY order — that is membership, a different question. See
    /// `docs/access-patterns.md` 3.4: *"Membership is EXISTENCE, in key order. It
    /// is not RECENCY."*
    ///
    /// The stamp is returned exactly as stored; the caller parses it and
    /// reports a row it cannot parse rather than inventing a stamp for it.
    ///
    /// **There is no index on `last_user_input_at`**, so this is a scan of the
    /// `threads` table plus a sort — 5,421 rows of two columns at measurement,
    /// exactly what the `updated_at` form cost. That is the reason the caller's
    /// `limit` is small and the reason this runs ONCE per epoch and never per
    /// key. **It is deliberately not a `GROUP BY` over `events`**, which is the
    /// shape this answer had before the column existed and which no index on that
    /// 2.46 M-row table serves — see `crate::shapes::summary`'s standing refusal.
    pub fn recent_threads(&self, limit: usize) -> Result<Vec<(String, String)>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT id, last_user_input_at FROM threads
              WHERE last_user_input_at IS NOT NULL
              ORDER BY last_user_input_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    pub fn thread_ids(&self, limit: usize) -> Result<Vec<String>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare("SELECT id FROM threads ORDER BY id LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// **THE NEWEST APPLICATION EVENT OF EACH TYPE ON ONE THREAD** — the opaque
    /// payloads `crate::shapes::threads`' `app` cell holds.
    ///
    /// ONE statement. The inner group-by is served by `idx_events_thread_kind`
    /// (`thread_id`, `kind`) and the join back is on the `(thread_id, seq)`
    /// unique index, so the cost is one index range plus one seek per event type
    /// — and the type set is the APPLICATION's vocabulary, which is seven in the
    /// production store.
    ///
    /// The payload is returned verbatim. This crate does not read it: what an
    /// `event_type` MEANS is application logic and lives in an
    /// application-specific shape.
    pub fn newest_custom_events(&self, thread_id: &str) -> Result<Vec<(String, Value)>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT e.payload_json
               FROM events e
               JOIN (SELECT json_extract(payload_json, '$.event_type') AS et,
                            MAX(seq) AS top
                       FROM events
                      WHERE thread_id = ?1 AND kind = 'custom'
                      GROUP BY et) newest
                 ON e.seq = newest.top
              WHERE e.thread_id = ?1 AND e.kind = 'custom'",
        )?;
        let rows = stmt.query_map(params![thread_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for payload in rows {
            let payload = payload?;
            let Ok(v) = serde_json::from_str::<Value>(&payload) else {
                continue;
            };
            // The ledger's `Custom` payload is `{kind: "custom", event_type, data}`.
            let Some(et) = v.get("event_type").and_then(Value::as_str) else {
                continue;
            };
            out.push((
                et.to_string(),
                v.get("data").cloned().unwrap_or(Value::Null),
            ));
        }
        Ok(out)
    }

    /// **THE MOST RECENTLY WRITTEN APPLICATION EVENT OF ONE TYPE, PER THREAD,
    /// FLEET-WIDE AND CAPPED** — what `crate::shapes::summary` hydrates from.
    ///
    /// Answers `(rows, total)`: the newest `limit` of them, newest first, AND how
    /// many threads have one at all, so the caller can report `Partial` rather
    /// than a short read that claims to be whole.
    ///
    /// ONE statement for the page and one for the total. `idx_events_thread_kind`
    /// is `(thread_id, kind)`, so a fleet-wide filter on `kind` alone cannot use
    /// it and this SCANS — measured at 0.85–0.94 s on the 2.46M-row production
    /// store, once per shape epoch. An index on `(kind, id)` would make it a
    /// seek; adding one is a migration on a 2.46 GB table and is deliberately out
    /// of scope here.
    ///
    /// `events.id` is the global `INTEGER PRIMARY KEY AUTOINCREMENT`, so `MAX(id)`
    /// per thread is "most recently written" fleet-wide with no clock involved.
    pub fn newest_app_events(
        &self,
        event_type: &str,
        limit: usize,
    ) -> Result<(Vec<(String, u64, String, Value)>, usize)> {
        let conn = self.read()?;
        // ── ONE STATEMENT, AND IT USED TO BE TWO FULL SCANS ──────────────────
        //
        // **THE DEFECT THIS FIXES (2026-10-02).** This method ran a
        // `SELECT COUNT(*) FROM (<the group-by>)` and then the group-by AGAIN to
        // get the page — two statements, each one a full scan of `events`. The
        // group-by filters on `kind` and a `json_extract` of the payload, and
        // `idx_events_thread_kind` is `(thread_id, kind)`, so a fleet-wide read
        // on `kind` alone cannot use it: BOTH statements scan the whole table.
        // Measured at ~3.5 s cold per statement on the 2.46M-row production
        // store, so the `summary` shape's init paid ~7 s where its own module
        // doc and `store.rs`'s cost-meter doc both claimed ONE statement.
        //
        // **`COUNT(*) OVER ()` is what collapses it.** The window is computed
        // over the whole inner result, so `total` is the number of threads that
        // have a name — exactly what the separate `COUNT(*)` answered — and the
        // outer `LIMIT` is applied after it. A window function also blocks
        // limit-pushdown into the subquery, which is what makes that true rather
        // than hopeful: SQLite cannot cut the partition short and then report its
        // size.
        //
        // `DB_TRIPS` never saw any of this, because it counts connection
        // ACQUISITIONS and this was always one — see the counter's own doc.
        let mut stmt = conn.prepare(
            "SELECT thread_id, seq, ts, payload_json, total FROM (
               SELECT e.id AS id, e.thread_id AS thread_id, e.seq AS seq,
                      e.ts AS ts, e.payload_json AS payload_json,
                      COUNT(*) OVER () AS total
                 FROM events e
                WHERE e.id IN (SELECT MAX(id) FROM events
                                WHERE kind = 'custom'
                                  AND json_extract(payload_json, '$.event_type') = ?1
                                GROUP BY thread_id)
             )
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![event_type, limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?.max(0) as u64,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        // **ZERO ROWS MEANS ZERO TOTAL, and it has to be said explicitly**: the
        // window function rides ON a row, so an empty result carries no `total`
        // at all. The old shape got this for free from a separate `COUNT(*)`;
        // this is the one thing the collapse has to restate.
        let mut total: i64 = 0;
        for row in rows {
            let (thread_id, seq, ts, payload, t) = row?;
            total = t;
            let data = serde_json::from_str::<Value>(&payload)
                .ok()
                .and_then(|v| v.get("data").cloned())
                .unwrap_or(Value::Null);
            out.push((thread_id, seq, ts, data));
        }
        Ok((out, total.max(0) as usize))
    }

    /// The highest message id assigned in a thread (0 if none).
    pub fn max_message_id(&self, thread_id: &str) -> Result<u64> {
        let conn = self.read()?;
        let id: i64 = conn.query_row(
            "SELECT COALESCE(MAX(message_id), 0) FROM events WHERE thread_id = ?1",
            params![thread_id],
            |r| r.get(0),
        )?;
        Ok(id.max(0) as u64)
    }

    // ── queue (Q4) ───────────────────────────────────────────────────────

    pub fn enqueue(
        &self,
        thread_id: &str,
        message_id: u64,
        text: &str,
        intent: &str,
        metadata: &Value,
        headers: &[crate::context::HeaderDeclaration],
    ) -> Result<i64> {
        self.enqueue_recorded(thread_id, message_id, text, intent, metadata, headers, None)
    }

    /// [`Self::enqueue`] with the `user_message` envelope that says it, in ONE
    /// transaction (R10.M1): the words reach the ledger exactly when they
    /// reach a row a dispatcher selects, never one without the other.
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_recorded(
        &self,
        thread_id: &str,
        message_id: u64,
        text: &str,
        intent: &str,
        metadata: &Value,
        headers: &[crate::context::HeaderDeclaration],
        appended: Option<&StoredEvent>,
    ) -> Result<i64> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        if let Some(ev) = appended {
            Self::insert_event_tx(&tx, ev)?;
        }
        // NULL rather than the string "null": an absent blob and an explicit
        // JSON null are the same thing to every reader, and NULL is the one
        // that costs nothing to store.
        let metadata_json = if metadata.is_null() {
            None
        } else {
            Some(metadata.to_string())
        };
        let headers_json = if headers.is_empty() {
            None
        } else {
            serde_json::to_string(headers).ok()
        };
        tx.execute(
            "INSERT INTO queue (thread_id, message_id, text, intent, state, created_at, metadata_json, headers_json, dispatch)
             VALUES (?1, ?2, ?3, ?4, 'waiting', ?5, ?6, ?7, ?8)",
            params![
                thread_id,
                message_id as i64,
                text,
                intent,
                now_rfc3339(),
                metadata_json,
                headers_json,
                crate::event::delivery::dispatch_for_intent(intent)
            ],
        )?;
        let row_id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(row_id)
    }

    fn map_queue(r: &rusqlite::Row) -> rusqlite::Result<QueueRow> {
        Ok(QueueRow {
            id: r.get(0)?,
            thread_id: r.get(1)?,
            message_id: r.get::<_, i64>(2)? as u64,
            text: r.get(3)?,
            intent: r.get(4)?,
            state: r.get(5)?,
            created_at: r.get(6)?,
            // Unparseable stored metadata degrades to "absent" rather than
            // failing the delivery: a malformed client blob must never cost the
            // operator his turn.
            metadata: r
                .get::<_, Option<String>>(7)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(Value::Null),
            // Same degrade-to-absent rule: an unreadable declaration costs the
            // header, never the turn. Rows written before this column existed
            // read as NULL and therefore as "no declared headers".
            headers: r
                .get::<_, Option<String>>(8)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            dispatch: r.get(9)?,
            attempts: r.get::<_, i64>(10)?.max(0) as u32,
        })
    }

    /// Waiting queue rows for a thread, oldest first.
    pub fn waiting_queue(&self, thread_id: &str) -> Result<Vec<QueueRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT id, thread_id, message_id, text, intent, state, created_at, metadata_json, headers_json, dispatch, attempts
             FROM queue WHERE thread_id = ?1 AND state = 'waiting' AND dispatch != 'never'
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![thread_id], Self::map_queue)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// How many waiting queue rows each thread holds — the WHOLE FLEET in one
    /// statement, for [`crate::runtime::ThreadManager::thread_states`].
    ///
    /// This replaces `waiting_queue(id).len()` per thread. It is a covered read:
    /// `idx_queue_thread_state` is `(thread_id, state)`, so the `WHERE
    /// state='waiting' GROUP BY thread_id` is answered from the index without
    /// touching a row, and its cost is proportional to the number of WAITING
    /// rows (usually zero), not to the number of threads.
    ///
    /// Threads with no waiting rows are simply absent; the caller reads a
    /// missing key as 0.
    pub fn waiting_queue_counts(&self) -> Result<HashMap<String, u64>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT thread_id, COUNT(*) FROM queue
             WHERE state = 'waiting' AND dispatch != 'never' GROUP BY thread_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// Threads that currently have waiting queue rows (boot re-dispatch).
    pub fn threads_with_waiting_queue(&self) -> Result<Vec<String>> {
        let conn = self.read()?;
        let mut stmt =
            conn.prepare(
            "SELECT DISTINCT thread_id FROM queue WHERE state = 'waiting' AND dispatch != 'never'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// **OPEN AN ATTEMPT on queue rows — `mark_delivered`, renamed for what it
    /// always was** (R10.M1, H17). The rows move `waiting → attempted` and their
    /// `attempts` counter rises; nothing here says they were DELIVERED, because
    /// at this instant no process exists. Delivered is derived from a later
    /// `turn_started` naming the message id ([`Self::delivered_message_ids`]),
    /// so there is no second write to lose.
    ///
    /// `attempt` is the `delivery_attempted` envelope, already sequenced by the
    /// caller. When given, it is inserted in the SAME transaction as the row
    /// moves, so no kill can leave a row `attempted` without its attempt on the
    /// ledger or an attempt on the ledger whose rows never moved. `None` is the
    /// bare row move (test residue and the legacy alias).
    pub fn mark_attempted(&self, row_ids: &[i64], attempt: Option<&StoredEvent>) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        if let Some(ev) = attempt {
            Self::insert_event_tx(&tx, ev)?;
        }
        for id in row_ids {
            tx.execute(
                "UPDATE queue SET state='attempted', attempts=attempts+1 WHERE id=?1",
                params![id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The pre-R10.M1 name of [`Self::mark_attempted`] (no attempt event),
    /// kept so callers outside R10.M1's touches compile unchanged. The rows go
    /// to `attempted`, NOT `delivered`.
    #[doc(hidden)]
    pub fn mark_delivered(&self, row_ids: &[i64]) -> Result<()> {
        self.mark_attempted(row_ids, None)
    }

    /// One ledger row, inside a caller's transaction. A plain `INSERT` — not
    /// [`Self::append_event`]'s `ON CONFLICT DO NOTHING` — because the whole
    /// point of the caller's transaction is that the event and the row move
    /// land together or not at all, and a silently skipped event would break
    /// that.
    fn insert_event_tx(tx: &rusqlite::Transaction<'_>, ev: &StoredEvent) -> Result<()> {
        let payload = serde_json::to_string(&ev.event)?;
        tx.execute(
            "INSERT INTO events (thread_id, seq, message_id, kind, payload_json, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                ev.thread_id,
                ev.seq as i64,
                ev.message_id.map(|m| m as i64),
                ev.event.kind(),
                payload,
                ev.ts,
            ],
        )?;
        Ok(())
    }

    /// **DELIVERED, DERIVED** (R10.M1, §5.3 rule 4): every message id a
    /// `turn_started` on this thread has named. A message is delivered iff it
    /// is in this set; nothing on the queue row says so.
    pub fn delivered_message_ids(&self, thread_id: &str) -> Result<HashSet<u64>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT payload_json FROM events WHERE thread_id = ?1 AND kind = 'turn_started'",
        )?;
        let rows = stmt.query_map(params![thread_id], |r| r.get::<_, String>(0))?;
        let mut out = HashSet::new();
        for row in rows {
            let Ok(v) = serde_json::from_str::<Value>(&row?) else {
                continue;
            };
            if let Some(ids) = v.get("message_ids").and_then(|m| m.as_array()) {
                out.extend(ids.iter().filter_map(|i| i.as_u64()));
            }
        }
        Ok(out)
    }

    /// The `cauldron.threads/*` delivery events on one thread, in ledger
    /// order, as `(seq, event_type, data)`. The sent record, read back.
    pub fn delivery_events(&self, thread_id: &str) -> Result<Vec<(u64, String, Value)>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT seq, payload_json FROM events
             WHERE thread_id = ?1 AND kind = 'custom' ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![thread_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, payload) = row?;
            let Ok(v) = serde_json::from_str::<Value>(&payload) else {
                continue;
            };
            let Some(t) = v.get("event_type").and_then(|t| t.as_str()) else {
                continue;
            };
            if !t.starts_with("cauldron.threads/") {
                continue;
            }
            out.push((
                seq.max(0) as u64,
                t.to_string(),
                v.get("data").cloned().unwrap_or(Value::Null),
            ));
        }
        Ok(out)
    }

    /// For each message id, the id of the LAST `delivery_attempted` that named
    /// it — the `resend_of` a new attempt carries (§6.2). Absent ids were
    /// never attempted under R10.M1 (or only as a legacy `delivered` row).
    pub fn last_attempt_ids(
        &self,
        thread_id: &str,
        message_ids: &[u64],
    ) -> Result<HashMap<u64, String>> {
        let want: HashSet<u64> = message_ids.iter().copied().collect();
        let mut out = HashMap::new();
        for (_, t, data) in self.delivery_events(thread_id)? {
            if t != crate::event::delivery::ATTEMPTED {
                continue;
            }
            let Some(att) = data.get("attempt").and_then(|a| a.as_str()) else {
                continue;
            };
            for mid in data
                .get("message_ids")
                .and_then(|m| m.as_array())
                .into_iter()
                .flatten()
                .filter_map(|i| i.as_u64())
            {
                if want.contains(&mid) {
                    out.insert(mid, att.to_string());
                }
            }
        }
        Ok(out)
    }

    /// Queue rows that a dispatch consumed and then dropped on the floor: marked
    /// `delivered`, but no turn was ever started for them.
    ///
    /// This closes the one hole in Q4's "a message is never lost" promise. The
    /// drain in `maybe_dispatch` takes the thread lock, marks its batch
    /// delivered, builds the prompt, and hands the rest to a DETACHED task —
    /// and only that task, several `.await`s later, spawns the child and writes
    /// the `running_turns` row. Die anywhere in that window and the durable
    /// residue is a set of `delivered` rows whose prompt existed solely as a
    /// `String` in a task that no longer exists. `waiting_queue` filters on
    /// `state='waiting'`, so those rows are never offered to anyone again:
    /// silent, permanent loss of something the operator said.
    ///
    /// The detection is EXACT, not heuristic, and rests on two facts that hold
    /// crate-wide:
    ///
    ///   1. `mark_delivered` has exactly ONE caller — the dispatch drain.
    ///   2. That drain's only continuation is `run_turn`, whose first act is to
    ///      emit `turn_started` naming the very message ids it was handed, and
    ///      that emit happens BEFORE the output file is created and before the
    ///      child is spawned.
    ///
    /// So `turn_started` naming a message id is a durable receipt that a child
    /// was about to receive it, and the absence of that receipt proves no child
    /// ever did. Neither `events` nor `queue` rows are ever deleted by this
    /// crate, so the absence cannot be an artifact of pruning.
    ///
    /// Deliberately conservative in the other direction: a message that DID get
    /// a `turn_started` is never returned here even if its turn later died, was
    /// killed, or was interrupted. That turn burned real tokens and may have
    /// done real work, and re-running it would duplicate an execution — which is
    /// a worse harm than a turn that visibly stopped. Only turns that provably
    /// never began are recoverable.
    pub fn stranded_deliveries(&self) -> Result<Vec<QueueRow>> {
        let conn = self.read()?;

        // Every message id any turn was ever started for, per thread.
        let mut started: HashMap<String, HashSet<u64>> = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT thread_id, payload_json FROM events WHERE kind='turn_started'")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (thread_id, payload) = row?;
                let Ok(v) = serde_json::from_str::<Value>(&payload) else {
                    continue;
                };
                let Some(ids) = v.get("message_ids").and_then(|m| m.as_array()) else {
                    continue;
                };
                let seen = started.entry(thread_id).or_default();
                for id in ids.iter().filter_map(|i| i.as_u64()) {
                    seen.insert(id);
                }
            }
        }

        let mut stmt = conn.prepare(
            "SELECT id, thread_id, message_id, text, intent, state, created_at, metadata_json, headers_json, dispatch, attempts
             FROM queue WHERE state IN ('attempted', 'delivered') ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], Self::map_queue)?;
        let mut out = Vec::new();
        for row in rows {
            let row = row?;
            let ran = started
                .get(&row.thread_id)
                .is_some_and(|ids| ids.contains(&row.message_id));
            if !ran {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// Return stranded rows to the waiting queue so the ordinary dispatch path
    /// picks them up again. Guarded on `state IN ('attempted','delivered')` (R10.M1) so a row that raced
    /// back into service is left alone, and done in ONE transaction so a batch
    /// is never half-restored. Returns how many rows actually moved.
    pub fn restore_deliveries(&self, row_ids: &[i64]) -> Result<usize> {
        self.restore_deliveries_recorded(row_ids, None)
    }

    /// [`Self::restore_deliveries`] with the `delivery_cancelled{restart}` that
    /// says WHY in the same transaction (R10.M1, §5.3 rule 5): the attempt
    /// ends on the ledger exactly when its rows go back to `waiting`, and the
    /// rows' `dispatch` is left as it was. The event is written only when a
    /// row actually moved, so a second boot cannot record a second
    /// cancellation of the same attempt.
    pub fn restore_deliveries_recorded(
        &self,
        row_ids: &[i64],
        cancelled: Option<&StoredEvent>,
    ) -> Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut moved = 0;
        for id in row_ids {
            moved += tx.execute(
                "UPDATE queue SET state='waiting', delivered_at=NULL
                 WHERE id=?1 AND state IN ('attempted', 'delivered')",
                params![id],
            )?;
        }
        if moved > 0 {
            if let Some(ev) = cancelled {
                Self::insert_event_tx(&tx, ev)?;
            }
        }
        tx.commit()?;
        Ok(moved)
    }

    /// **Mark a delivery FAILED — the state that keeps the operator's words when
    /// a retry ladder runs out.**
    ///
    /// # The hole this fills
    ///
    /// Until 2026-08-25 a turn that exhausted its ladder did two things:
    /// `clear_retry` (a plain `DELETE`, which destroys the last copy of the
    /// prompt any dispatcher will ever read) and `emit(TurnFailed)`. It touched
    /// `queue` not at all, so the rows stayed `delivered` forever —
    /// indistinguishable from a delivery that succeeded, invisible to
    /// `waiting_queue`, and excluded from `restore_deliveries` by construction
    /// (`stranded_deliveries` deliberately refuses anything that ever got a
    /// `turn_started`, and a failed turn always did).
    ///
    /// The net effect was that the operator's only remaining recourse was to
    /// re-type what he had said. That is precisely the outcome he has ruled out
    /// in the strongest terms available:
    ///
    /// > *"I would rather have the entire threads module burned to the ground
    /// > than violate this principle"* — his voice input is never lost and never
    /// > silently fails to reach the thread.
    ///
    /// # Why a THIRD state and not a restore
    ///
    /// Moving the rows back to `waiting` would preserve the words, but it would
    /// also hand them straight back to `maybe_dispatch`, which would re-run the
    /// turn that just failed, fail again, and restore again: a live-lock that
    /// spends tokens forever. `failed` is deliberately a state no dispatcher
    /// selects — the prompt is held, not re-fired, and moving it back to
    /// `waiting` is an explicit act (see [`Self::reselect_failed`]).
    ///
    /// Guarded on `state IN ('attempted','delivered')` (R10.M1) and done in one transaction, exactly like
    /// [`Self::restore_deliveries`]. Returns how many rows moved.
    pub fn fail_deliveries(&self, thread_id: &str, message_ids: &[u64]) -> Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut moved = 0;
        for mid in message_ids {
            moved += tx.execute(
                "UPDATE queue SET state='failed'
                 WHERE thread_id=?1 AND message_id=?2 AND state IN ('attempted', 'delivered')",
                params![thread_id, *mid as i64],
            )?;
        }
        tx.commit()?;
        Ok(moved)
    }

    /// **Put a failed delivery back in the queue — the RE-SELECT half.**
    ///
    /// The operator (or a client acting for him) says "send that again", and the
    /// exact text he spoke goes back to `waiting` for the ordinary dispatch path
    /// to pick up. Nothing is re-typed and nothing is reconstructed: `queue.text`
    /// is untouched by delivery and by failure alike, so what gets re-sent is
    /// verbatim what he said.
    ///
    /// `message_ids` empty means *every* failed row on the thread, which is the
    /// common case — a delivery is usually a batch and he wants all of it back.
    ///
    /// Guarded on `state='failed'` so this can never resurrect a cancelled row or
    /// double-send a live one. Idempotent: calling it twice moves rows the first
    /// time and nothing the second.
    pub fn reselect_failed(&self, thread_id: &str, message_ids: &[u64]) -> Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let moved = if message_ids.is_empty() {
            tx.execute(
                "UPDATE queue SET state='waiting', delivered_at=NULL
                 WHERE thread_id=?1 AND state='failed'",
                params![thread_id],
            )?
        } else {
            let mut n = 0;
            for mid in message_ids {
                n += tx.execute(
                    "UPDATE queue SET state='waiting', delivered_at=NULL
                     WHERE thread_id=?1 AND message_id=?2 AND state='failed'",
                    params![thread_id, *mid as i64],
                )?;
            }
            n
        };
        tx.commit()?;
        Ok(moved)
    }

    /// The failed-but-preserved deliveries on a thread, oldest first — what a
    /// client shows when it offers to re-send.
    pub fn failed_deliveries(&self, thread_id: &str) -> Result<Vec<QueueRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT id, thread_id, message_id, text, intent, state, created_at, metadata_json, headers_json, dispatch, attempts
             FROM queue WHERE thread_id = ?1 AND state = 'failed' ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![thread_id], Self::map_queue)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// **WITHDRAW a queued message — the operator's explicit act, and the only
    /// one that takes a row out of the dispatcher's view** (R10.M1, H18; was
    /// `cancel_queued`). `dispatch` becomes `never`; `state` and `text` are
    /// untouched, so the words stay on the row and on the ledger. Only a row
    /// with no open attempt (`waiting`) and not already withdrawn moves.
    ///
    /// `withdrawn` is the `message_withdrawn` envelope, sequenced by the
    /// caller, written in the same transaction and only when the row moved.
    pub fn queue_withdraw(
        &self,
        thread_id: &str,
        message_id: u64,
        withdrawn: Option<&StoredEvent>,
    ) -> Result<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE queue SET dispatch='never'
             WHERE thread_id=?1 AND message_id=?2 AND state='waiting' AND dispatch != 'never'",
            params![thread_id, message_id as i64],
        )?;
        if n > 0 {
            if let Some(ev) = withdrawn {
                Self::insert_event_tx(&tx, ev)?;
            }
        }
        tx.commit()?;
        Ok(n > 0)
    }

    /// The pre-R10.M1 name of [`Self::queue_withdraw`] (no event).
    #[doc(hidden)]
    pub fn cancel_queued(&self, thread_id: &str, message_id: u64) -> Result<bool> {
        self.queue_withdraw(thread_id, message_id, None)
    }

    /// **RESEND: put withdrawn or failed rows back in the dispatcher's view**
    /// (R10.M1). A row moves when it is withdrawn (`waiting` + `never`) or
    /// `failed`; it comes back `waiting` with the dispatch word its intent
    /// gives. Its text is untouched, so what goes out is verbatim what was
    /// said, and the next attempt's `resend_of` names the earlier one.
    /// `message_ids` empty means every such row on the thread. Returns the
    /// message ids that moved (for the caller's `message_resent` event).
    pub fn queue_resend(&self, thread_id: &str, message_ids: &[u64]) -> Result<Vec<u64>> {
        let conn = self.conn()?;
        // ONE statement: the selection, the move and the read-back of what
        // moved. An empty id list is `[]`, which selects every candidate.
        let ids = serde_json::to_string(message_ids)?;
        let mut stmt = conn.prepare(
            "UPDATE queue
                SET state='waiting', delivered_at=NULL,
                    dispatch = CASE WHEN intent LIKE 'turn_end%' THEN 'turn_end' ELSE 'eager' END
              WHERE thread_id=?1
                AND (state='failed' OR (state='waiting' AND dispatch='never'))
                AND (json_array_length(?2) = 0
                     OR message_id IN (SELECT value FROM json_each(?2)))
             RETURNING id, message_id",
        )?;
        let mut moved: Vec<(i64, u64)> = stmt
            .query_map(params![thread_id, ids], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
            })?
            .collect::<std::result::Result<_, _>>()?;
        // RETURNING's order is unspecified; queue order is the row id.
        moved.sort_unstable();
        Ok(moved.into_iter().map(|(_, m)| m).collect())
    }

    // ── R10.M2: an operator cancel DEFERS, never drops ──────────────────
    //
    // Kept in its own block, apart from R10.M1's queue code, so the two
    // merge cleanly.

    /// The message ids among `message_ids` whose rows [`Self::queue_ride`]
    /// would move: still owed (`attempted`, or `delivered` with no turn that
    /// finished them — the rows a retry ladder or a parked turn holds — or
    /// `waiting` and not already `ride` or `never`).
    pub fn rideable(&self, thread_id: &str, message_ids: &[u64]) -> Result<Vec<u64>> {
        let conn = self.read()?;
        let mut out = Vec::new();
        for mid in message_ids {
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM queue
                 WHERE thread_id=?1 AND message_id=?2
                   AND (state IN ('attempted', 'delivered')
                        OR (state='waiting' AND dispatch IN ('eager', 'turn_end')))",
                params![thread_id, *mid as i64],
                |r| r.get(0),
            )?;
            if n > 0 {
                out.push(*mid);
            }
        }
        Ok(out)
    }

    /// **Set rows back on the queue `ride`** (R10.M2, L-8): `state='waiting'`,
    /// `dispatch='ride'`, in ONE transaction with the ledger event that
    /// records it (`delivery_cancelled` or `dispatch_changed`), written only
    /// when a row moved. Moves exactly the rows [`Self::rideable`] names;
    /// `failed` and `never` rows are untouched — only the operator's explicit
    /// re-select or withdraw moves those. Returns how many rows moved.
    pub fn queue_ride(&self, thread_id: &str, message_ids: &[u64], recorded: Option<&StoredEvent>) -> Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut moved = 0;
        for mid in message_ids {
            moved += tx.execute(
                "UPDATE queue SET state='waiting', dispatch='ride', delivered_at=NULL
                 WHERE thread_id=?1 AND message_id=?2
                   AND (state IN ('attempted', 'delivered')
                        OR (state='waiting' AND dispatch IN ('eager', 'turn_end')))",
                params![thread_id, *mid as i64],
            )?;
        }
        if moved > 0 {
            if let Some(ev) = recorded {
                Self::insert_event_tx(&tx, ev)?;
            }
        }
        tx.commit()?;
        Ok(moved)
    }

    // ── retries (Q5/G10) ─────────────────────────────────────────────────

    pub fn upsert_retry(&self, r: &RetryRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO retries
                (thread_id, prompt, message_ids_json, attempt, kind, reason, account, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(thread_id) DO UPDATE SET
                prompt=excluded.prompt, message_ids_json=excluded.message_ids_json,
                attempt=excluded.attempt, kind=excluded.kind, reason=excluded.reason,
                account=excluded.account",
            params![
                r.thread_id,
                r.prompt,
                serde_json::to_string(&r.message_ids)?,
                r.attempt as i64,
                r.kind,
                r.reason,
                r.account,
                now_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_retry(&self, thread_id: &str) -> Result<Option<RetryRow>> {
        let conn = self.read()?;
        let row = conn
            .query_row(
                "SELECT thread_id, prompt, message_ids_json, attempt, kind, reason, account, created_at
                 FROM retries WHERE thread_id = ?1",
                params![thread_id],
                Self::map_retry,
            )
            .ok();
        Ok(row)
    }

    fn map_retry(r: &rusqlite::Row) -> rusqlite::Result<RetryRow> {
        let ids_json: String = r.get(2)?;
        Ok(RetryRow {
            thread_id: r.get(0)?,
            prompt: r.get(1)?,
            message_ids: serde_json::from_str(&ids_json).unwrap_or_default(),
            attempt: r.get::<_, i64>(3)? as u32,
            kind: r.get(4)?,
            reason: r.get(5)?,
            account: r.get(6)?,
            created_at: r.get(7)?,
        })
    }

    pub fn list_retries(&self) -> Result<Vec<RetryRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            // **ORDERED BY BIRTH, NOT BY A DEADLINE.** `ORDER BY retry_at ASC`
            // ranked parked prompts by a per-thread clock; with that clock gone
            // the only honest order is the one the operator's own messages
            // arrived in, which is what `created_at` records (it survives every
            // re-arm). The order is total — `thread_id` breaks same-millisecond
            // ties — so the candidate set a pass walks is reproducible.
            "SELECT thread_id, prompt, message_ids_json, attempt, kind, reason, account, created_at
             FROM retries ORDER BY created_at ASC, thread_id ASC",
        )?;
        let rows = stmt.query_map([], Self::map_retry)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// When this retry row was FIRST written, not when it was last re-armed.
    ///
    /// `upsert_retry`'s `DO UPDATE` list deliberately omits `created_at`, so the
    /// birth timestamp survives every re-arm. That makes it the only honest
    /// answer to "how long has this delivery been retrying" — `attempt` counts
    /// spawns, not time, and there is no longer any per-row instant at all to
    /// mistake for one. [`RetryRow::created_at`] carries the same value, so a
    /// caller that already has the row needs no second trip. The starvation
    /// escalation reports it for the same reason `update.rs` reports
    /// `pending_secs`: a count without a duration cannot tell a slow ladder from
    /// a fast loop.
    pub fn retry_created_at(&self, thread_id: &str) -> Result<Option<String>> {
        let conn = self.read()?;
        let v = conn
            .query_row(
                "SELECT created_at FROM retries WHERE thread_id = ?1",
                params![thread_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(v)
    }

    pub fn clear_retry(&self, thread_id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM retries WHERE thread_id = ?1", params![thread_id])?;
        Ok(())
    }

    // ── the delivery's own clock (see `delivery_clocks` in schema.sql) ────

    /// **An attempt of this delivery is starting. Return its clock, creating it
    /// only if this is a DIFFERENT delivery from the one on record.**
    ///
    /// "The same delivery" means the new message ids SHARE an id with the
    /// recorded ones — so a retry (same ids), a resumed park (same ids) and a
    /// delivery that has had a later message folded in (a superset) all keep the
    /// recorded `turn_started_at` and failure streak. Only a delivery with no id
    /// in common replaces the row, which is what makes a missed
    /// [`Self::end_delivery`] self-healing rather than a clock that leaks into
    /// the next turn.
    ///
    /// An EMPTY id list is never matched to anything (it names no delivery), so
    /// it always starts a fresh clock.
    pub fn begin_delivery_attempt(
        &self,
        thread_id: &str,
        message_ids: &[u64],
    ) -> Result<DeliveryClock> {
        let conn = self.conn()?;
        let existing: Option<(String, String, Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT message_ids_json, turn_started_at, streak_kind, streak_since
                 FROM delivery_clocks WHERE thread_id = ?1",
                params![thread_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let ids_json = serde_json::to_string(message_ids)?;
        if let Some((old_json, started, streak_kind, streak_since)) = existing {
            let old: Vec<u64> = serde_json::from_str(&old_json).unwrap_or_default();
            let same_delivery = message_ids.iter().any(|id| old.contains(id));
            if same_delivery {
                if old_json != ids_json {
                    conn.execute(
                        "UPDATE delivery_clocks SET message_ids_json = ?2 WHERE thread_id = ?1",
                        params![thread_id, ids_json],
                    )?;
                }
                return Ok(DeliveryClock {
                    turn_started_at: started,
                    streak_kind,
                    streak_since,
                });
            }
        }
        let now = now_rfc3339();
        conn.execute(
            "INSERT INTO delivery_clocks
                (thread_id, message_ids_json, turn_started_at, streak_kind, streak_since)
             VALUES (?1, ?2, ?3, NULL, NULL)
             ON CONFLICT(thread_id) DO UPDATE SET
                message_ids_json=excluded.message_ids_json,
                turn_started_at=excluded.turn_started_at,
                streak_kind=NULL, streak_since=NULL",
            params![thread_id, ids_json, now],
        )?;
        Ok(DeliveryClock {
            turn_started_at: now,
            streak_kind: None,
            streak_since: None,
        })
    }

    /// **An attempt of the current delivery failed with `kind`.** Returns when
    /// the current run of `kind` failures began.
    ///
    /// A failure of the SAME class as the recorded streak keeps its start; a
    /// failure of a different class starts a new streak now. So a delivery that
    /// spent an hour behind a usage limit and then lost the network starts its
    /// connectivity budget at the first network failure, not an hour ago — no
    /// class spends another's budget, in time exactly as in attempts.
    ///
    /// With no clock on record (a row lost, or a caller that never began one) a
    /// clock is created with the streak starting now, which fails in the
    /// PATIENT direction: one more budget, never zero.
    pub fn note_delivery_failure(&self, thread_id: &str, kind: &str) -> Result<String> {
        let conn = self.conn()?;
        let now = now_rfc3339();
        conn.execute(
            "INSERT INTO delivery_clocks
                (thread_id, message_ids_json, turn_started_at, streak_kind, streak_since)
             VALUES (?1, '[]', ?3, ?2, ?3)
             ON CONFLICT(thread_id) DO UPDATE SET
                streak_since = CASE
                    WHEN delivery_clocks.streak_kind IS ?2
                         AND delivery_clocks.streak_since IS NOT NULL
                    THEN delivery_clocks.streak_since ELSE ?3 END,
                streak_kind = ?2",
            params![thread_id, kind, now],
        )?;
        let since: String = conn.query_row(
            "SELECT streak_since FROM delivery_clocks WHERE thread_id = ?1",
            params![thread_id],
            |r| r.get(0),
        )?;
        Ok(since)
    }

    /// The clock on record for this thread's current delivery, if any.
    pub fn delivery_clock(&self, thread_id: &str) -> Result<Option<DeliveryClock>> {
        let conn = self.read()?;
        let v = conn
            .query_row(
                "SELECT turn_started_at, streak_kind, streak_since
                 FROM delivery_clocks WHERE thread_id = ?1",
                params![thread_id],
                |r| {
                    Ok(DeliveryClock {
                        turn_started_at: r.get(0)?,
                        streak_kind: r.get(1)?,
                        streak_since: r.get(2)?,
                    })
                },
            )
            .optional()?;
        Ok(v)
    }

    /// Every thread's recorded `turn_started_at`, for the non-idle roster.
    pub fn list_delivery_starts(&self) -> Result<Vec<(String, String)>> {
        let conn = self.read()?;
        let mut stmt =
            conn.prepare("SELECT thread_id, turn_started_at FROM delivery_clocks")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// **Seed a clock directly** — for a test that must stand a delivery at a
    /// point in its budget without driving half an hour of real attempts, and
    /// for nothing else. Overwrites whatever is recorded.
    pub fn put_delivery_clock(
        &self,
        thread_id: &str,
        message_ids: &[u64],
        clock: &DeliveryClock,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO delivery_clocks
                (thread_id, message_ids_json, turn_started_at, streak_kind, streak_since)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(thread_id) DO UPDATE SET
                message_ids_json=excluded.message_ids_json,
                turn_started_at=excluded.turn_started_at,
                streak_kind=excluded.streak_kind, streak_since=excluded.streak_since",
            params![
                thread_id,
                serde_json::to_string(message_ids)?,
                clock.turn_started_at,
                clock.streak_kind,
                clock.streak_since,
            ],
        )?;
        Ok(())
    }

    /// The delivery is over — succeeded, given up, or ended by an intention.
    pub fn end_delivery(&self, thread_id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM delivery_clocks WHERE thread_id = ?1",
            params![thread_id],
        )?;
        Ok(())
    }

    // ── running turns (G1) ───────────────────────────────────────────────
    //
    // ONE column list and ONE row reader for the two readers below, so a future
    // column cannot be added to `list_running_turns` and forgotten in
    // `get_running_turn` — which is how `adopt_turn` would silently go back to
    // seeing `prompt: None` on every row.

    pub fn upsert_running_turn(&self, r: &RunningTurnRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO running_turns
                (thread_id, pid, spawn_cookie, output_path, byte_offset, started_at,
                 prompt, message_ids_json, attempt)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(thread_id) DO UPDATE SET
                pid=excluded.pid, spawn_cookie=excluded.spawn_cookie,
                output_path=excluded.output_path, byte_offset=excluded.byte_offset,
                started_at=excluded.started_at, prompt=excluded.prompt,
                message_ids_json=excluded.message_ids_json, attempt=excluded.attempt",
            params![
                r.thread_id,
                r.pid,
                r.spawn_cookie,
                r.output_path,
                r.byte_offset as i64,
                r.started_at,
                r.prompt,
                serde_json::to_string(&r.message_ids)?,
                r.attempt,
            ],
        )?;
        Ok(())
    }

    pub fn update_turn_offset(&self, thread_id: &str, byte_offset: u64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE running_turns SET byte_offset=?2 WHERE thread_id=?1",
            params![thread_id, byte_offset as i64],
        )?;
        Ok(())
    }

    pub fn clear_running_turn(&self, thread_id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM running_turns WHERE thread_id = ?1",
            params![thread_id],
        )?;
        Ok(())
    }

    /// This thread's in-flight turn, if one is recorded — read at `ResultTurn`
    /// time (before `clear_running_turn` fires) to stamp the execution's own
    /// `started_at` onto its usage-ledger rows (overlap-window calibration,
    /// msg #620).
    pub fn get_running_turn(&self, thread_id: &str) -> Result<Option<RunningTurnRow>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                RUNNING_TURN_COLS_SQL,
                params![thread_id],
                running_turn_from_row,
            )
            .ok())
    }

    pub fn list_running_turns(&self) -> Result<Vec<RunningTurnRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT thread_id, pid, spawn_cookie, output_path, byte_offset, started_at,
                    prompt, message_ids_json, attempt
             FROM running_turns",
        )?;
        let rows = stmt.query_map([], running_turn_from_row)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    // ── attached interactive sessions (SH-A1, docs/SHIMS.md §A.4) ────────

    /// Record a new attached session. A plain `INSERT`: a row with this cookie
    /// already present is REFUSED (the primary key), never overwritten — two
    /// attaches minting one cookie is a bug to surface, not to merge.
    pub fn insert_attached_session(&self, r: &AttachedSessionRow) -> Result<()> {
        let conn = self.conn()?;
        let res = conn.execute(
            &format!(
                "INSERT INTO attached_sessions ({ATTACHED_SESSION_COLS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            ),
            params![
                r.cookie,
                r.thread_id,
                r.account,
                r.wrapper_pid,
                r.claude_pid,
                r.transcript_path,
                r.byte_offset as i64,
                r.started_at,
            ],
        );
        match res {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(Error::Store(format!(
                    "attached session cookie {:?} is already recorded; refusing a second insert",
                    r.cookie
                )))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// The attached session with this cookie, if any. A read error is an
    /// error, not `None`.
    pub fn get_attached_session(&self, cookie: &str) -> Result<Option<AttachedSessionRow>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                &format!("SELECT {ATTACHED_SESSION_COLS} FROM attached_sessions WHERE cookie = ?1"),
                params![cookie],
                attached_session_from_row,
            )
            .optional()?)
    }

    /// Every attached session on `thread_id`, oldest first (cookie breaks ties).
    pub fn list_attached_sessions(&self, thread_id: &str) -> Result<Vec<AttachedSessionRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {ATTACHED_SESSION_COLS} FROM attached_sessions
             WHERE thread_id = ?1 ORDER BY started_at, cookie"
        ))?;
        let rows = stmt.query_map(params![thread_id], attached_session_from_row)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// Every attached session on every thread, oldest first (cookie breaks
    /// ties) — the SH-A4 pid reaper's sweep.
    pub fn list_all_attached_sessions(&self) -> Result<Vec<AttachedSessionRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {ATTACHED_SESSION_COLS} FROM attached_sessions ORDER BY started_at, cookie"
        ))?;
        let rows = stmt.query_map([], attached_session_from_row)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// Advance the transcript-sync offset. `Ok(false)` when no row has this
    /// cookie (the session was already detached).
    pub fn update_attached_offset(&self, cookie: &str, byte_offset: u64) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE attached_sessions SET byte_offset = ?2 WHERE cookie = ?1",
            params![cookie, byte_offset as i64],
        )?;
        Ok(n > 0)
    }

    /// Record the process identities: the wrapper pid (a relaunch may replace
    /// it) and the claude pid once SessionStart reports it. `Ok(false)` when no
    /// row has this cookie.
    pub fn update_attached_pids(
        &self,
        cookie: &str,
        wrapper_pid: i64,
        claude_pid: Option<i64>,
    ) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE attached_sessions SET wrapper_pid = ?2, claude_pid = ?3 WHERE cookie = ?1",
            params![cookie, wrapper_pid, claude_pid],
        )?;
        Ok(n > 0)
    }

    /// Record the native transcript path SessionStart reports. `Ok(false)`
    /// when no row has this cookie.
    pub fn update_attached_transcript(&self, cookie: &str, transcript_path: &str) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE attached_sessions SET transcript_path = ?2 WHERE cookie = ?1",
            params![cookie, transcript_path],
        )?;
        Ok(n > 0)
    }

    /// Forget the session (detach). `Ok(false)` when no row had this cookie.
    pub fn delete_attached_session(&self, cookie: &str) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "DELETE FROM attached_sessions WHERE cookie = ?1",
            params![cookie],
        )?;
        Ok(n > 0)
    }

    // ── the thread execution lock (msg #1213) ────────────────────────────

    /// Try to CLAIM `thread_id` for this daemon.
    ///
    /// The whole mutual exclusion is the `thread_locks` primary key: this is a
    /// plain `INSERT`, and SQLite refuses the second one. There is deliberately
    /// no "check then insert" — a check-then-act would have a window, and the
    /// operator's directive has no room for a window.
    ///
    /// [`Claim::Taken`] means the row is yours. [`Claim::Held`] hands back the
    /// CURRENT holder so the caller can decide (in `runtime`, which is where
    /// pid liveness belongs) whether it is a live holder to wait for or a dead
    /// one to reclaim with [`Self::steal_thread_lock`].
    pub fn try_claim_thread(
        &self,
        thread_id: &str,
        boot_id: &str,
        daemon_pid: i64,
        entry: &str,
    ) -> Result<Claim> {
        let conn = self.conn()?;
        let n = conn.execute(
            "INSERT INTO thread_locks (thread_id, boot_id, daemon_pid, entry, acquired_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(thread_id) DO NOTHING",
            params![thread_id, boot_id, daemon_pid, entry, now_rfc3339()],
        )?;
        if n == 1 {
            return Ok(Claim::Taken);
        }
        match read_lock(&conn, thread_id)? {
            Some(row) => Ok(Claim::Held(row)),
            // The holder released between our INSERT and this read. Say so
            // rather than inventing a holder; the caller retries.
            None => Ok(Claim::Vanished),
        }
    }

    /// RECLAIM a lock from a named holder — compare-and-swap on `boot_id`.
    ///
    /// `true` only when the row still belonged to `expected_boot` at the
    /// instant of the write, so two daemons that both judge a holder dead
    /// cannot both win: the loser's `WHERE boot_id = expected` no longer
    /// matches. Never call this on a holder whose pid is alive.
    pub fn steal_thread_lock(
        &self,
        thread_id: &str,
        expected_boot: &str,
        boot_id: &str,
        daemon_pid: i64,
        entry: &str,
    ) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE thread_locks
                SET boot_id=?3, daemon_pid=?4, entry=?5, acquired_at=?6
              WHERE thread_id=?1 AND boot_id=?2",
            params![
                thread_id,
                expected_boot,
                boot_id,
                daemon_pid,
                entry,
                now_rfc3339()
            ],
        )?;
        Ok(n == 1)
    }

    /// Release — guarded by `boot_id`, so a process can only ever drop a lock
    /// it actually holds. `true` when a row was removed.
    pub fn release_thread_lock(&self, thread_id: &str, boot_id: &str) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "DELETE FROM thread_locks WHERE thread_id=?1 AND boot_id=?2",
            params![thread_id, boot_id],
        )?;
        Ok(n == 1)
    }

    pub fn get_thread_lock(&self, thread_id: &str) -> Result<Option<ThreadLockRow>> {
        let conn = self.read()?;
        read_lock(&conn, thread_id)
    }

    pub fn list_thread_locks(&self) -> Result<Vec<ThreadLockRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT thread_id, boot_id, daemon_pid, entry, acquired_at FROM thread_locks",
        )?;
        let rows = stmt.query_map([], map_lock)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    // ── the seen cursor (status to AUI, msg #1213 second half) ───────────

    /// The ledger seq a client has been shown through (0 = nothing).
    pub fn last_seen_seq(&self, thread_id: &str) -> Result<u64> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT last_seen_seq FROM thread_seen WHERE thread_id=?1",
                params![thread_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as u64)
    }

    /// Every thread's seen cursor in ONE statement, for
    /// [`crate::runtime::ThreadManager::thread_states`]. `thread_seen`'s primary
    /// key is `thread_id`, so this is a straight table scan of one small
    /// single-row-per-thread table — cheaper than N primary-key lookups through
    /// N acquisitions of the one serialized connection.
    ///
    /// Threads that have never been marked seen are absent; a missing key is 0,
    /// exactly as [`Self::last_seen_seq`] returns 0 for them.
    pub fn list_thread_seen(&self) -> Result<HashMap<String, u64>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare("SELECT thread_id, last_seen_seq FROM thread_seen")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// Advance the seen cursor. MONOTONIC — a late frame from a stale client
    /// can never un-read messages the operator has already heard.
    pub fn mark_seen(&self, thread_id: &str, seq: u64) -> Result<u64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO thread_seen (thread_id, last_seen_seq, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(thread_id) DO UPDATE SET
                last_seen_seq = MAX(thread_seen.last_seen_seq, excluded.last_seen_seq),
                updated_at    = excluded.updated_at",
            params![thread_id, seq as i64, now_rfc3339()],
        )?;
        drop(conn);
        self.last_seen_seq(thread_id)
    }

    /// **THE DURABLE VISIT ORDER** — `threads/recent-visits`' hydrate, in ONE
    /// statement: `(thread_id, visited_at, last_seen_seq)`, most recently
    /// visited first, at most `limit` rows.
    ///
    /// `thread_seen.updated_at` is the visit stamp and [`Self::mark_seen`] is its
    /// only writer: every call stamps it, including one whose `seq` does not
    /// advance the monotonic cursor, because a visit is the ACT of being shown
    /// the thread and not the cursor moving. `ThreadManager::mark_seen` is the
    /// one site that both writes this column and emits the `Fact::ThreadSeen`
    /// the live fold moves on — so the hydrate and the fold are fed by one act,
    /// which is where the ordering rule is spelled (see
    /// `crate::shapes::recent_visits` §"What the order is").
    ///
    /// The `JOIN threads` drops a cursor whose thread is gone: an index whose
    /// whole protocol is "subscribe to `threads` with these ids" must not hand
    /// out a key the pool cannot serve. One row per thread already (`thread_id`
    /// is the primary key), so there is nothing to aggregate. No index on
    /// `updated_at`: the table is one row per VISITED thread, not per event.
    pub fn recent_visits(&self, limit: usize) -> Result<Vec<(String, String, u64)>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT s.thread_id, s.updated_at, s.last_seen_seq
               FROM thread_seen s JOIN threads t ON t.id = s.thread_id
              ORDER BY s.updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?.max(0) as u64,
            ))
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// How many ADDRESSABLE messages sit above the seen cursor. Addressable
    /// (`message_id IS NOT NULL`) is the right unit because that is exactly
    /// what the operator can be read back — machinery events are not "unread".
    pub fn unread_count(&self, thread_id: &str, after_seq: u64) -> Result<u64> {
        let conn = self.read()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events
              WHERE thread_id=?1 AND seq > ?2 AND message_id IS NOT NULL",
            params![thread_id, after_seq as i64],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }

    // ── accounts (§10) ───────────────────────────────────────────────────

    pub fn upsert_account_state(&self, a: &AccountState) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO accounts
                (name, config_dir, limit_until, last_limit_prose,
                 limit_identity, limit_axis, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(name) DO UPDATE SET
                config_dir=excluded.config_dir, limit_until=excluded.limit_until,
                last_limit_prose=excluded.last_limit_prose,
                limit_identity=excluded.limit_identity, limit_axis=excluded.limit_axis,
                updated_at=excluded.updated_at",
            params![
                a.name,
                a.config_dir,
                a.limit_until,
                a.last_limit_prose,
                a.limit_identity,
                a.limit_axis,
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Record (or clear) a usage-limit window on ONE label, touching only the
    /// limit columns.
    ///
    /// A narrow UPDATE for the same reason [`Self::set_account_identity`] and
    /// [`Self::set_account_health`] are: the three writers of this table are
    /// column-disjoint, so a rate limit can never erase a fingerprint and a
    /// fingerprint refresh can never erase a limit.
    ///
    /// It also replaces the one production caller of
    /// [`Self::upsert_account_state`], which wrote `config_dir` too — so the 429
    /// path could only ever stamp the label the failing thread was on, since it
    /// had exactly one config dir to write. A limit belongs to a LOGIN, and
    /// `runtime::note_account_limit` now calls this once per label of that
    /// login; carrying no config dir is what makes that safe.
    ///
    /// `limit` is `Some((until, prose, axis, identity))` or `None` to clear.
    /// `identity` is the login the limit was OBSERVED on — see the
    /// `limit_identity` column comment in `schema.sql`.
    pub fn set_account_limit(
        &self,
        name: &str,
        limit: Option<(&str, Option<&str>, Option<&str>, Option<&str>)>,
    ) -> Result<()> {
        let conn = self.conn()?;
        let (until, prose, axis, identity) = match limit {
            Some((u, p, a, i)) => (Some(u), p, a, i),
            None => (None, None, None, None),
        };
        conn.execute(
            "INSERT INTO accounts
                (name, limit_until, last_limit_prose, limit_axis, limit_identity, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(name) DO UPDATE SET
                limit_until=excluded.limit_until,
                last_limit_prose=excluded.last_limit_prose,
                limit_axis=excluded.limit_axis,
                limit_identity=excluded.limit_identity,
                updated_at=excluded.updated_at",
            params![name, until, prose, axis, identity, now_rfc3339()],
        )?;
        Ok(())
    }

    /// **ONE account's tracked row**, or `None` when the account has never been
    /// recorded — which is not a fault: an account with no row has never hit a
    /// limit and never failed to authenticate, and absence of evidence is not a
    /// fault (`accounts::selectable`).
    ///
    /// Separate from [`Self::list_account_states`] because
    /// [`crate::budgets::Budgets::pressure_reading`] is on the RANKING path and
    /// is called once per candidate per placement: reading the whole table there
    /// would make one placement over a four-account pool four full scans to
    /// learn four single rows.
    pub fn account_state(&self, name: &str) -> Result<Option<AccountState>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT name, config_dir, limit_until, last_limit_prose, updated_at,
                        identity, dead_since, dead_reason, limit_identity, limit_axis
                 FROM accounts WHERE name=?1",
                params![name],
                |r| {
                    Ok(AccountState {
                        name: r.get(0)?,
                        config_dir: r.get(1)?,
                        limit_until: r.get(2)?,
                        last_limit_prose: r.get(3)?,
                        updated_at: r.get(4)?,
                        identity: r.get(5)?,
                        dead_since: r.get(6)?,
                        dead_reason: r.get(7)?,
                        limit_identity: r.get(8)?,
                        limit_axis: r.get(9)?,
                    })
                },
            )
            .ok())
    }

    pub fn list_account_states(&self) -> Result<Vec<AccountState>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT name, config_dir, limit_until, last_limit_prose, updated_at,
                    identity, dead_since, dead_reason, limit_identity, limit_axis
             FROM accounts ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(AccountState {
                name: r.get(0)?,
                config_dir: r.get(1)?,
                limit_until: r.get(2)?,
                last_limit_prose: r.get(3)?,
                updated_at: r.get(4)?,
                identity: r.get(5)?,
                dead_since: r.get(6)?,
                dead_reason: r.get(7)?,
                limit_identity: r.get(8)?,
                limit_axis: r.get(9)?,
            })
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// Record an account's login IDENTITY (an opaque hash — never a credential
    /// value). Written on daemon open for every registered account.
    ///
    /// A narrow UPDATE rather than a field on [`Self::upsert_account_state`] on
    /// purpose: that upsert is called from the 429 path with a freshly built
    /// limit-state row, and folding identity into it would let a rate limit
    /// silently erase the fingerprint (and vice versa).
    pub fn set_account_identity(
        &self,
        name: &str,
        config_dir: Option<&str>,
        identity: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO accounts (name, config_dir, identity, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET
                config_dir=excluded.config_dir, identity=excluded.identity,
                updated_at=excluded.updated_at",
            params![name, config_dir, identity, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Set or clear an account's DEAD flag. `Some((instant, reason))` marks the
    /// login dead (an auth failure no retry can fix); `None` clears it, which a
    /// successful turn on the account does.
    ///
    /// Like [`Self::set_account_identity`] this touches only its own columns, so
    /// health and limit state can never overwrite each other.
    pub fn set_account_health(
        &self,
        name: &str,
        dead: Option<(&str, &str)>,
    ) -> Result<()> {
        let conn = self.conn()?;
        let (since, reason) = match dead {
            Some((s, r)) => (Some(s), Some(r)),
            None => (None, None),
        };
        conn.execute(
            "INSERT INTO accounts (name, dead_since, dead_reason, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET
                dead_since=excluded.dead_since, dead_reason=excluded.dead_reason,
                updated_at=excluded.updated_at",
            params![name, since, reason, now_rfc3339()],
        )?;
        Ok(())
    }

    // ── usage ledger (Q11) ───────────────────────────────────────────────

    /// Insert one ledger row on an existing connection, stamped with the
    /// CURRENT accounting revision. Every writer goes through here, so a row
    /// can never be written without saying which shape it is in.
    fn insert_usage(conn: &Connection, u: &UsageRow, ts: &str) -> Result<()> {
        conn.execute(
            "INSERT INTO usage_ledger
                (thread_id, account, model, input_tokens, output_tokens,
                 cache_read_tokens, cache_creation_tokens, cost_usd, ts, started_at,
                 accounting_rev)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                u.thread_id,
                u.account,
                u.model,
                u.input_tokens as i64,
                u.output_tokens as i64,
                u.cache_read_tokens as i64,
                u.cache_creation_tokens as i64,
                u.cost_usd,
                ts,
                u.started_at,
                ACCOUNTING_REV,
            ],
        )?;
        Ok(())
    }

    pub fn record_usage(&self, u: &UsageRow) -> Result<()> {
        let conn = self.conn()?;
        Self::insert_usage(&conn, u, &now_rfc3339())?;
        Ok(())
    }

    /// Book a `result_turn`'s `modelUsage` on the ledger AS A DELTA, and hand
    /// back exactly what was booked so the budget meter can be charged from the
    /// same numbers.
    ///
    /// ## What `modelUsage` actually is (measured 2026-07-31 on the live store)
    ///
    /// It is the CUMULATIVE per-model total for the whole claude PROCESS — the
    /// parent agent and every subagent it spawned — as of the instant that
    /// `result` line was serialized. It is not a per-result figure and not a
    /// per-message one. Three observations from the real ledger pin this down:
    ///
    /// * a turn whose 17 subagent results were flushed in one 30 ms burst at
    ///   process teardown carried a BYTE-IDENTICAL `modelUsage` on all 17;
    /// * within ONE execution (`started_at` 02:42:47) the burst at 03:59 read
    ///   38,337,547 opus cache-read tokens and the burst at 04:11 read
    ///   53,569,874 — strictly greater, same process, so it accumulates across
    ///   the turn rather than resetting per result;
    /// * on the last line of every burst, `total_cost_usd` equals the sum of
    ///   `modelUsage[*].costUSD` to the cent ($146.7365, $41.0113, $52.3409),
    ///   which is what "cumulative total for this process" means.
    ///
    /// So neither "record every result" (17×) nor "record only the last one"
    /// (which would drop the 03:59 burst and then re-book it inside the 04:11
    /// figure) is right. The only correct shape is the DELTA against the
    /// previous snapshot of the same execution, which is what this does.
    ///
    /// The baseline is a durable per-(thread, model) HIGH-WATER MARK, taken
    /// field by field:
    ///
    /// * durable, because `adopt_turn` re-adopts a running turn after a daemon
    ///   restart and keeps reading its output — an in-memory baseline would be
    ///   lost and the next line's running total booked again in full;
    /// * high-water rather than last-seen, so results arriving OUT OF ORDER
    ///   (or a snapshot repeated late) can only ever contribute zero, never a
    ///   negative row and never a second copy;
    /// * scoped by `epoch` (the execution's `started_at`): a different epoch is
    ///   a different claude process whose counters restart at zero, so the
    ///   watermark resets instead of swallowing the new execution's spend.
    ///
    /// A model absent from this snapshot is treated as UNCHANGED, not as zero —
    /// `modelUsage: {}` (which a torn-down turn really does emit) must not look
    /// like the turn gave its tokens back.
    ///
    /// `started_at` is also carried onto every row for the overlap-window
    /// calibrator (msg #620). Returns the per-model deltas actually booked;
    /// models whose delta is zero produce no row and no entry.
    pub fn record_turn_usage(
        &self,
        thread_id: &str,
        account: Option<&str>,
        model_usage: &Value,
        started_at: Option<&str>,
    ) -> Result<Vec<UsageDelta>> {
        let Some(map) = model_usage.as_object() else {
            return Ok(Vec::new());
        };
        if map.is_empty() {
            return Ok(Vec::new());
        }
        let epoch = started_at.unwrap_or("");
        let now = now_rfc3339();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut out = Vec::new();

        for (model, u) in map {
            let cum = (
                u.get("inputTokens").and_then(Value::as_u64).unwrap_or(0),
                u.get("outputTokens").and_then(Value::as_u64).unwrap_or(0),
                u.get("cacheReadInputTokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                u.get("cacheCreationInputTokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                u.get("costUSD").and_then(Value::as_f64).unwrap_or(0.0),
            );

            // The baseline. A watermark from another execution is not a
            // baseline at all — that process's counters are gone.
            let prev: (u64, u64, u64, u64, f64) = tx
                .query_row(
                    "SELECT input_tokens, output_tokens, cache_read_tokens,
                            cache_creation_tokens, cost_usd
                     FROM turn_usage_watermarks
                     WHERE thread_id=?1 AND model=?2 AND epoch=?3",
                    params![thread_id, model, epoch],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?.max(0) as u64,
                            r.get::<_, i64>(1)?.max(0) as u64,
                            r.get::<_, i64>(2)?.max(0) as u64,
                            r.get::<_, i64>(3)?.max(0) as u64,
                            r.get::<_, f64>(4)?,
                        ))
                    },
                )
                .optional()?
                .unwrap_or((0, 0, 0, 0, 0.0));

            // High-water mark, field by field: an out-of-order or repeated
            // snapshot yields zero, never a give-back.
            let hi = (
                cum.0.max(prev.0),
                cum.1.max(prev.1),
                cum.2.max(prev.2),
                cum.3.max(prev.3),
                cum.4.max(prev.4),
            );
            let row = UsageRow {
                thread_id: thread_id.to_string(),
                account: account.map(str::to_string),
                model: model.clone(),
                input_tokens: hi.0 - prev.0,
                output_tokens: hi.1 - prev.1,
                cache_read_tokens: hi.2 - prev.2,
                cache_creation_tokens: hi.3 - prev.3,
                cost_usd: hi.4 - prev.4,
                started_at: started_at.map(str::to_string),
            };
            let tokens = row.input_tokens
                + row.output_tokens
                + row.cache_read_tokens
                + row.cache_creation_tokens;
            if tokens == 0 && row.cost_usd == 0.0 {
                continue; // nothing moved; no row, no charge
            }
            Self::insert_usage(&tx, &row, &now)?;
            tx.execute(
                "INSERT INTO turn_usage_watermarks
                    (thread_id, model, epoch, input_tokens, output_tokens,
                     cache_read_tokens, cache_creation_tokens, cost_usd, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(thread_id, model) DO UPDATE SET
                    epoch=excluded.epoch,
                    input_tokens=excluded.input_tokens,
                    output_tokens=excluded.output_tokens,
                    cache_read_tokens=excluded.cache_read_tokens,
                    cache_creation_tokens=excluded.cache_creation_tokens,
                    cost_usd=excluded.cost_usd,
                    updated_at=excluded.updated_at",
                params![
                    thread_id,
                    model,
                    epoch,
                    hi.0 as i64,
                    hi.1 as i64,
                    hi.2 as i64,
                    hi.3 as i64,
                    hi.4,
                    now,
                ],
            )?;
            out.push(UsageDelta {
                model: model.clone(),
                tokens,
                cost_usd: row.cost_usd,
            });
        }
        tx.commit()?;
        Ok(out)
    }

    /// Drop a thread's usage watermarks. Called when an execution is known to
    /// be over, so a stale baseline can never suppress the next turn's spend.
    pub fn clear_turn_usage_watermarks(&self, thread_id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM turn_usage_watermarks WHERE thread_id=?1",
            params![thread_id],
        )?;
        Ok(())
    }

    /// Aggregate spend grouped by thread or by account (the wire read `ledger/query` was retired at R2.S3a).
    ///
    /// The totals sum DELTA rows only. Pre-fix rows are reported beside them
    /// (`legacy_rows` / `legacy_cost_usd`) rather than added in or hidden —
    /// they over-state spend by a factor nobody can reconstruct, so folding
    /// them into a headline number would make every budget conversation wrong.
    ///
    /// `turns` counts EXECUTIONS, not rows: one turn writes one row per model
    /// per result, and calling seventeen of those "seventeen turns" was half of
    /// how the double-count read as plausible.
    pub fn spend_summary(&self, group_by_account: bool) -> Result<Vec<SpendSummary>> {
        let conn = self.read()?;
        let key = if group_by_account {
            "COALESCE(account, 'default')"
        } else {
            "thread_id"
        };
        let sql = format!(
            "SELECT {key} AS k,
                    COALESCE(SUM(CASE WHEN accounting_rev >= {rev} THEN
                        input_tokens + output_tokens + cache_read_tokens
                        + cache_creation_tokens ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN accounting_rev >= {rev} THEN cost_usd ELSE 0 END), 0),
                    COUNT(DISTINCT CASE WHEN accounting_rev >= {rev}
                        THEN COALESCE(started_at, ts) END),
                    COALESCE(SUM(CASE WHEN accounting_rev < {rev} THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN accounting_rev < {rev} THEN cost_usd ELSE 0 END), 0)
             FROM usage_ledger GROUP BY k ORDER BY 3 DESC",
            rev = ACCOUNTING_REV
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| {
            Ok(SpendSummary {
                key: r.get(0)?,
                total_tokens: r.get::<_, i64>(1)?.max(0) as u64,
                cost_usd: r.get(2)?,
                turns: r.get::<_, i64>(3)?.max(0) as u64,
                legacy_rows: r.get::<_, i64>(4)?.max(0) as u64,
                legacy_cost_usd: r.get(5)?,
            })
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    // ── budgets (§15: DEC-4 … DEC-19) ────────────────────────────────────

    pub fn upsert_budget(&self, b: &BudgetRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO budgets
                (key, parent_key, session_pct, weekly_pct, fable_pct, cash_usd,
                 accounts_json, on_trip, created_at, updated_at, disabled_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(key) DO UPDATE SET
                parent_key=excluded.parent_key, session_pct=excluded.session_pct,
                weekly_pct=excluded.weekly_pct, fable_pct=excluded.fable_pct,
                cash_usd=excluded.cash_usd, accounts_json=excluded.accounts_json,
                on_trip=excluded.on_trip, updated_at=excluded.updated_at,
                disabled_at=excluded.disabled_at",
            params![
                b.key,
                b.parent_key,
                b.session_pct,
                b.weekly_pct,
                b.fable_pct,
                b.cash_usd,
                serde_json::to_string(&b.accounts)?,
                b.on_trip,
                b.created_at,
                b.updated_at,
                b.disabled_at,
            ],
        )?;
        Ok(())
    }

    fn map_budget(r: &rusqlite::Row) -> rusqlite::Result<BudgetRow> {
        let accounts_json: String = r.get(6)?;
        Ok(BudgetRow {
            key: r.get(0)?,
            parent_key: r.get(1)?,
            session_pct: r.get(2)?,
            weekly_pct: r.get(3)?,
            fable_pct: r.get(4)?,
            cash_usd: r.get(5)?,
            accounts: serde_json::from_str(&accounts_json).unwrap_or_default(),
            on_trip: r.get(7)?,
            created_at: r.get(8)?,
            updated_at: r.get(9)?,
            disabled_at: r.get(10)?,
        })
    }

    const BUDGET_COLS: &'static str = "key, parent_key, session_pct, weekly_pct, fable_pct, \
         cash_usd, accounts_json, on_trip, created_at, updated_at, disabled_at";

    pub fn get_budget(&self, key: &str) -> Result<Option<BudgetRow>> {
        let conn = self.read()?;
        let sql = format!("SELECT {} FROM budgets WHERE key = ?1", Self::BUDGET_COLS);
        Ok(conn.query_row(&sql, params![key], Self::map_budget).ok())
    }

    pub fn list_budgets(&self) -> Result<Vec<BudgetRow>> {
        let conn = self.read()?;
        let sql = format!("SELECT {} FROM budgets ORDER BY key", Self::BUDGET_COLS);
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], Self::map_budget)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// DISABLE or re-enable a budget key (Q-34, msg #751) — `Some(instant)`
    /// disables, `None` re-enables. Returns false when there is no such key.
    ///
    /// There is deliberately **no delete**: the operator's answer was "I don't
    /// think we should delete a budget key. I think we should disable a budget
    /// key so we can still query and see information about the budget… So no
    /// deleting." This UPDATE is therefore the only lifecycle operation a key
    /// has after creation, and it touches exactly one column: every charge,
    /// observation, estimate and thread pointer survives untouched, which is
    /// what keeps the whole history queryable.
    ///
    /// It writes through the raw column rather than [`Self::upsert_budget`] on
    /// purpose: disabling must work on a key whose config would no longer pass
    /// validation (e.g. a pre-round-7 key with an empty account scope). The
    /// one thing you must always be able to do to a broken key is turn it off.
    pub fn set_budget_disabled(&self, key: &str, disabled_at: Option<&str>) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE budgets SET disabled_at = ?2, updated_at = ?3 WHERE key = ?1",
            params![key, disabled_at, now_rfc3339()],
        )?;
        Ok(n > 0)
    }

    /// Does any budget key exist at all? (Cheap gate for the `/usage` probe —
    /// a machine with no budgets never shells out.)
    pub fn any_budget_configured(&self) -> Result<bool> {
        let conn = self.read()?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM budgets", [], |r| r.get(0))?;
        Ok(n > 0)
    }

    // ── budget charges ───────────────────────────────────────────────────

    pub fn record_charge(&self, c: &ChargeRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO budget_charges
                (budget_key, thread_id, account, axis, tokens, cost_usd, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                c.budget_key,
                c.thread_id,
                c.account,
                c.axis,
                c.tokens,
                c.cost_usd,
                c.ts,
            ],
        )?;
        Ok(())
    }

    /// Tokens charged to `key` on `axis` from `account` since `since_ts`
    /// (the current window start). Charges outside the window are ignored —
    /// that IS the meter reset (DEC-16).
    pub fn charged_tokens(
        &self,
        key: &str,
        axis: &str,
        account: &str,
        since_ts: &str,
    ) -> Result<u64> {
        let conn = self.read()?;
        let n: i64 = conn.query_row(
            "SELECT COALESCE(SUM(tokens), 0) FROM budget_charges
             WHERE budget_key=?1 AND axis=?2 AND account=?3 AND ts >= ?4",
            params![key, axis, account, since_ts],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }

    /// Dollars charged to `key` on `axis` from `account` since `since_ts` —
    /// the CASH axis's meter (round-7 item 5). Dollars are the unit here, and
    /// they POOL across accounts exactly like the percentage meters do: a $20
    /// budget is $20 total, so the caller sums these across the key's scope.
    pub fn charged_dollars(
        &self,
        key: &str,
        axis: &str,
        account: &str,
        since_ts: &str,
    ) -> Result<f64> {
        let conn = self.read()?;
        let v: f64 = conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM budget_charges
             WHERE budget_key=?1 AND axis=?2 AND account=?3 AND ts >= ?4",
            params![key, axis, account, since_ts],
            |r| r.get(0),
        )?;
        Ok(if v.is_finite() && v > 0.0 { v } else { 0.0 })
    }

    /// Every account this key has ever drawn from (the per-account breakdown).
    ///
    /// **This runs on the hottest path in the daemon and it must not scan.**
    /// `charge_budget` fires on every token-usage line the CLI streams, and it
    /// calls `check_trip_midturn` -> `Budgets::snapshot` -> `metered_accounts`
    /// -> here. The obvious spelling — `SELECT DISTINCT account ... WHERE
    /// budget_key=?1` — walks every charge row for the key and sorts them in a
    /// temp b-tree to hand back a list that is three names long. `budget_key`
    /// is one value in practice, so "every row for the key" is the whole
    /// table: 479,230 rows on the operator's own daemon on 2026-09-11, growing
    /// forever because `budget_charges` is an append-only ledger that is never
    /// pruned. Worse, it ran holding the single `Mutex<Connection>` every other
    /// store call also needs, so one metering query parked the whole runtime —
    /// a live sample of the daemon that morning caught four of ten tokio
    /// workers blocked in `Store::conn`'s `Mutex::lock` behind exactly this
    /// query, with `/health` taking over fifteen seconds to answer.
    ///
    /// So this is a LOOSE INDEX SCAN instead. `min(account)` with an equality
    /// bound on `budget_key` is a single index seek against
    /// `idx_charges_key_account`, and each recursion step seeks past the
    /// account it just found rather than reading the rows in between. The cost
    /// is one seek per distinct account — O(accounts), which is a number that
    /// stays small — instead of O(rows ever charged), which is a number that
    /// only ever goes up. Measured on a 480,000-row ledger: 75.8s before,
    /// 0.001s after.
    ///
    /// `account` is `NOT NULL`, so a NULL from `min()` unambiguously means "no
    /// rows past here" and is the recursion's terminator. Results come back in
    /// ascending account order, which is what the old `ORDER BY account` gave.
    pub fn charge_accounts(&self, key: &str) -> Result<Vec<String>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "WITH RECURSIVE walk(account) AS (
                 SELECT min(account) FROM budget_charges WHERE budget_key=?1
               UNION ALL
                 SELECT (SELECT min(c.account) FROM budget_charges c
                          WHERE c.budget_key=?1 AND c.account > walk.account)
                 FROM walk WHERE walk.account IS NOT NULL
             )
             SELECT account FROM walk WHERE account IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![key], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    // ── gauge observations (DEC-15) ──────────────────────────────────────

    pub fn record_observation(&self, o: &ObservationRow) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO usage_observations (account, axis, pct, resets_at, source, observed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![o.account, o.axis, o.pct, o.resets_at, o.source, o.observed_at],
        )?;
        Ok(conn.last_insert_rowid())
    }

    fn map_observation(r: &rusqlite::Row) -> rusqlite::Result<ObservationRow> {
        Ok(ObservationRow {
            id: r.get(0)?,
            account: r.get(1)?,
            axis: r.get(2)?,
            pct: r.get(3)?,
            resets_at: r.get(4)?,
            source: r.get(5)?,
            observed_at: r.get(6)?,
        })
    }

    /// The most recent observations for `(account, axis)`, newest first.
    pub fn recent_observations(
        &self,
        account: &str,
        axis: &str,
        limit: usize,
    ) -> Result<Vec<ObservationRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT id, account, axis, pct, resets_at, source, observed_at
             FROM usage_observations WHERE account=?1 AND axis=?2
             ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![account, axis, limit as i64], Self::map_observation)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// **The observation SERIES for `(account, axis)` over a time window**,
    /// oldest first — what a history view and a sparkline are drawn from.
    ///
    /// This exists because the accounts interface was reading `usage_observations`
    /// out of `threads.db` DIRECTLY, opening the daemon's own database read-only
    /// while the daemon held a WAL on it, to draw its history screen. Everything
    /// that view knows — the axis vocabulary, the poll cadence, the schema —
    /// was duplicated client-side and would go silently wrong the moment any of
    /// the three moved.
    ///
    /// Ordered by `id`, not by `observed_at`: the timestamp is a string and
    /// two observations inside one poll can share it, so ordering on it would
    /// be unstable. `id` is the insertion order and is what the calibrator
    /// already walks.
    ///
    /// **THE CAP TAKES THE NEWEST ROWS IN THE WINDOW, AND THAT IS THE WHOLE
    /// POINT OF THE `DESC` BELOW.** It read `ORDER BY id ASC LIMIT ?5`, which
    /// kept the OLDEST rows: a busy account's week came back as its first
    /// hours, and a sparkline drawn from it showed a window that had already
    /// gone by while claiming to be the week. The cadence is what makes that
    /// reachable rather than theoretical — the busy probe interval is 120 s
    /// (`crate::config::default_probe_interval_busy`), so one axis can hold
    /// ~5,040 observations in a week against a default cap of 1,000.
    ///
    /// **The SQL orders `DESC` and the answer is reversed back to ASCENDING**,
    /// so this function's contract — *oldest first* — is unchanged and no
    /// caller's x-axis inverts. What moved is WHICH end the cap keeps, which is
    /// the only thing that was wrong. `recent_observations` above is `DESC` and
    /// stays `DESC` because newest-first IS its contract; the two are different
    /// questions and neither should be read for the other.
    ///
    /// `since` / `until` are RFC3339 strings compared as strings, which is
    /// correct for RFC3339 UTC and is how every other range query in this file
    /// compares them. `limit` is a hard cap so a client asking for a year of a
    /// two-minute poll cannot pull the table into memory.
    pub fn observations_between(
        &self,
        account: &str,
        axis: &str,
        since: &str,
        until: &str,
        limit: usize,
    ) -> Result<Vec<ObservationRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT id, account, axis, pct, resets_at, source, observed_at
             FROM usage_observations
             WHERE account=?1 AND axis=?2 AND observed_at >= ?3 AND observed_at <= ?4
             ORDER BY id DESC LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![account, axis, since, until, limit as i64],
            Self::map_observation,
        )?;
        let mut out: Vec<ObservationRow> = rows
            .collect::<std::result::Result<_, _>>()
            .map_err(Error::from)?;
        // The cap has already chosen the newest rows; this restores the
        // declared order. Reversing in Rust rather than wrapping the statement
        // in an ordered sub-SELECT keeps the index walk on `id` visible at the
        // top level, where the next reader of this query looks for it.
        out.reverse();
        Ok(out)
    }

    /// Observations for `(account, axis)` with `id >= since_id`, ASCENDING —
    /// the bracketing material the overlap-window calibrator walks (msg
    /// #620). `since_id` is inclusive so the last observation an estimate has
    /// already folded in stays in view: it is exactly the `obs_before` a NEW
    /// window brackets against.
    pub fn observations_from(
        &self,
        account: &str,
        axis: &str,
        since_id: i64,
    ) -> Result<Vec<ObservationRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT id, account, axis, pct, resets_at, source, observed_at
             FROM usage_observations WHERE account=?1 AND axis=?2 AND id >= ?3
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![account, axis, since_id], Self::map_observation)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    // ── continuous limit estimates (DEC-12) ──────────────────────────────

    pub fn upsert_estimate(&self, e: &EstimateRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO limit_estimates
                (account, axis, tokens_per_percent, sample_count, last_obs_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(account, axis) DO UPDATE SET
                tokens_per_percent=excluded.tokens_per_percent,
                sample_count=excluded.sample_count,
                last_obs_id=excluded.last_obs_id,
                updated_at=excluded.updated_at",
            params![
                e.account,
                e.axis,
                e.tokens_per_percent,
                e.sample_count as i64,
                e.last_obs_id,
                e.updated_at,
            ],
        )?;
        Ok(())
    }

    fn map_estimate(r: &rusqlite::Row) -> rusqlite::Result<EstimateRow> {
        Ok(EstimateRow {
            account: r.get(0)?,
            axis: r.get(1)?,
            tokens_per_percent: r.get(2)?,
            sample_count: r.get::<_, i64>(3)?.max(0) as u64,
            last_obs_id: r.get(4)?,
            updated_at: r.get(5)?,
        })
    }

    const ESTIMATE_COLS: &'static str =
        "account, axis, tokens_per_percent, sample_count, last_obs_id, updated_at";

    pub fn get_estimate(&self, account: &str, axis: &str) -> Result<Option<EstimateRow>> {
        let conn = self.read()?;
        let sql = format!(
            "SELECT {} FROM limit_estimates WHERE account=?1 AND axis=?2",
            Self::ESTIMATE_COLS
        );
        Ok(conn
            .query_row(&sql, params![account, axis], Self::map_estimate)
            .ok())
    }

    pub fn list_estimates(&self) -> Result<Vec<EstimateRow>> {
        let conn = self.read()?;
        let sql = format!(
            "SELECT {} FROM limit_estimates ORDER BY account, axis",
            Self::ESTIMATE_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], Self::map_estimate)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    // ── per-period estimate history (`limit_estimate_periods`) ───────────

    const PERIOD_COLS: &'static str = "identity, axis, period_end, account_label, config_dir, \
         tokens_per_percent, limit_tokens, sample_count, updates, first_recorded_at, updated_at";

    fn map_period(r: &rusqlite::Row) -> rusqlite::Result<PeriodEstimateRow> {
        Ok(PeriodEstimateRow {
            identity: r.get(0)?,
            axis: r.get(1)?,
            period_end: r.get(2)?,
            account_label: r.get(3)?,
            config_dir: r.get(4)?,
            tokens_per_percent: r.get(5)?,
            limit_tokens: r.get(6)?,
            sample_count: r.get::<_, i64>(7)?.max(0) as u64,
            updates: r.get::<_, i64>(8)?.max(0) as u64,
            first_recorded_at: r.get(9)?,
            updated_at: r.get(10)?,
        })
    }

    /// The NEWEST period row for `(identity, axis)` — the one the caller must
    /// compare its current window against to decide "same period, update in
    /// place" versus "the period closed, start a new one".
    ///
    /// Ordered by `period_end` rather than by `updated_at` on purpose: a row
    /// is frozen by never being written again, so write time stops tracking
    /// window order the moment a period closes.
    pub fn latest_period_estimate(
        &self,
        identity: &str,
        axis: &str,
    ) -> Result<Option<PeriodEstimateRow>> {
        let conn = self.read()?;
        let sql = format!(
            "SELECT {} FROM limit_estimate_periods WHERE identity=?1 AND axis=?2
             ORDER BY period_end DESC LIMIT 1",
            Self::PERIOD_COLS
        );
        Ok(conn
            .query_row(&sql, params![identity, axis], Self::map_period)
            .ok())
    }

    /// OPEN a period: insert the first estimate recorded inside a window.
    ///
    /// The `ON CONFLICT` branch is defensive, not the normal path — the caller
    /// decides period identity through `gauge::same_window`, and a re-open of
    /// a key that already exists must still count as an in-place update rather
    /// than an error or a silently dropped write.
    pub fn open_period_estimate(&self, p: &PeriodEstimateRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO limit_estimate_periods
                (identity, axis, period_end, account_label, config_dir,
                 tokens_per_percent, limit_tokens, sample_count, updates,
                 first_recorded_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(identity, axis, period_end) DO UPDATE SET
                account_label=excluded.account_label,
                config_dir=excluded.config_dir,
                tokens_per_percent=excluded.tokens_per_percent,
                limit_tokens=excluded.limit_tokens,
                sample_count=excluded.sample_count,
                updates=limit_estimate_periods.updates + 1,
                updated_at=excluded.updated_at",
            params![
                p.identity,
                p.axis,
                p.period_end,
                p.account_label,
                p.config_dir,
                p.tokens_per_percent,
                p.limit_tokens,
                p.sample_count as i64,
                p.updates as i64,
                p.first_recorded_at,
                p.updated_at,
            ],
        )?;
        Ok(())
    }

    /// UPDATE a period that is still live, IN PLACE. `period_end` names the
    /// row already open — it is never rewritten, so the bucket keeps the
    /// window instant it was opened with even when the gauge re-reports that
    /// same reset a minute off (`gauge::same_window`'s tolerance).
    ///
    /// `first_recorded_at` is untouched by construction: when this period
    /// first got an estimate is exactly the thing an in-place update must not
    /// move.
    pub fn update_period_estimate(&self, p: &PeriodEstimateRow) -> Result<usize> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE limit_estimate_periods SET
                account_label=?4, config_dir=?5, tokens_per_percent=?6,
                limit_tokens=?7, sample_count=?8, updates=updates + 1,
                updated_at=?9
             WHERE identity=?1 AND axis=?2 AND period_end=?3",
            params![
                p.identity,
                p.axis,
                p.period_end,
                p.account_label,
                p.config_dir,
                p.tokens_per_percent,
                p.limit_tokens,
                p.sample_count as i64,
                p.updated_at,
            ],
        )?;
        Ok(n)
    }

    /// Every period bucket, oldest window first within each (login, axis) —
    /// the history a human reads to compare accounts over time.
    pub fn list_period_estimates(&self) -> Result<Vec<PeriodEstimateRow>> {
        let conn = self.read()?;
        let sql = format!(
            "SELECT {} FROM limit_estimate_periods ORDER BY identity, axis, period_end",
            Self::PERIOD_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], Self::map_period)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    // ── the notice log: what has already been said out loud ──────────────

    /// **Claim the right to emit a notice, at most once per `every`.**
    ///
    /// Returns `Some(suppressed_since_last)` when the caller MAY speak — and
    /// records that it did, atomically, so two concurrent callers cannot both
    /// win. Returns `None` when the window is still open, having counted the
    /// suppression.
    ///
    /// The count comes back with the permission deliberately. A login that has
    /// been expired for a day may have blocked forty placements; a notice that
    /// says "this happened" understates that, and a notice that says "this
    /// happened, and 40 more times since yesterday" is the same one interruption
    /// carrying the whole truth. That is the difference between a rate limit and
    /// dropping information on the floor.
    ///
    /// # Why the rate limit is here and not in the client
    ///
    /// > *"I don't want to be notified more than once per day."*
    ///
    /// A limit held per-connection is not a limit — a second client listening
    /// doubles the notices. A limit held in memory is not a limit either: this
    /// daemon restarts itself whenever a new binary lands, and an expired login
    /// is a state only the operator can leave, so an in-memory window would
    /// re-open on every boot and the daemon would nag him precisely because it
    /// was trying not to.
    ///
    /// # The compare and the stamp are ONE statement, and what that is worth
    ///
    /// The window is arbitrated by the `WHERE` clause of the UPDATE itself
    /// rather than by a SELECT, an `if` and a later write.
    ///
    /// **Being honest about where the exclusion actually comes from:** today it
    /// comes from [`Store`]'s single `Mutex<Connection>`, which this function
    /// holds for its whole body — so the read-then-write shape would ALSO be
    /// correct, and `tests/accounts_api_surface.rs::
    /// exactly_one_of_two_racing_callers_may_speak` was written expecting to
    /// catch that mutation and did not. That is recorded rather than papered
    /// over.
    ///
    /// The conditional UPDATE is kept anyway, because it is correct WITHOUT
    /// depending on a property of a different module. The store's connection
    /// model is not this function's to assume: a connection pool, a reader
    /// connection, or a second daemon process would each remove the mutex that
    /// is silently doing the work, and the read-then-write version would start
    /// emitting twice with nothing in this file having changed. The one
    /// statement costs nothing and does not have that failure mode.
    pub fn claim_notice(
        &self,
        kind: &str,
        subject: &str,
        every: chrono::Duration,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<u64>> {
        let conn = self.conn()?;
        let cutoff = (now - every).to_rfc3339();
        let now_s = now.to_rfc3339();

        // THE FIRST NOTICE for this subject. `DO NOTHING`, not `DO UPDATE`, so
        // this statement can only ever CREATE — an existing row is left for the
        // conditional update below to arbitrate, and a first notice is never
        // mistaken for a window that has expired.
        conn.execute(
            "INSERT INTO notice_log (kind, subject, last_at, suppressed)
             VALUES (?1, ?2, ?3, 0)
             ON CONFLICT(kind, subject) DO NOTHING",
            params![kind, subject, now_s],
        )?;
        if conn.changes() > 0 {
            return Ok(Some(0));
        }

        // How many were swallowed since the last time this was spoken. Read
        // before the claim, on the same connection: it is INFORMATIONAL and
        // does not decide anything, so reading it outside the arbitration is
        // safe in a way that reading `last_at` there would not be.
        let suppressed: u64 = conn
            .query_row(
                "SELECT suppressed FROM notice_log WHERE kind=?1 AND subject=?2",
                params![kind, subject],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v.max(0) as u64)
            .unwrap_or(0);

        conn.execute(
            "UPDATE notice_log SET last_at = ?3, suppressed = 0
             WHERE kind = ?1 AND subject = ?2 AND last_at <= ?4",
            params![kind, subject, now_s, cutoff],
        )?;
        if conn.changes() > 0 {
            return Ok(Some(suppressed));
        }

        // The window is still open. Count the suppression so the notice that
        // eventually gets through can carry the whole truth.
        conn.execute(
            "UPDATE notice_log SET suppressed = suppressed + 1
             WHERE kind = ?1 AND subject = ?2",
            params![kind, subject],
        )?;
        Ok(None)
    }

    /// Every notice on record, for `daemon.status` and for a test to read
    /// without reaching into SQL.
    pub fn notice_log(&self) -> Result<Vec<(String, String, String, u64)>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT kind, subject, last_at, suppressed FROM notice_log ORDER BY kind, subject",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, i64>(3)?.max(0) as u64,
            ))
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    // ── the per-account pool share (`pool_shares`) ───────────────────────

    /// **The share of one ACCOUNT offered to the pool manager**, or `None` when
    /// the operator has never set one.
    ///
    /// Keyed on the account NAME, which is the 2026-08-27 correction: *"the
    /// login slot is subject to change."* A label re-pointed at a different
    /// login keeps the share the operator set for that label, because the share
    /// is a statement about what he is lending the pool and not about a
    /// credential.
    ///
    /// `None` is not an error and not a zero — it means the whole account is
    /// offered (1.0), which is precisely the behaviour every account had before
    /// this table existed. The caller ([`crate::budgets::Budgets::pool_share`])
    /// applies that default; this method reports only what is on disk, so an
    /// absent row and a deliberate `1.0` stay distinguishable to anyone
    /// auditing the store.
    pub fn pool_share(&self, account: &str) -> Result<Option<f64>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT share FROM pool_shares WHERE account=?1",
                params![account],
                |r| r.get::<_, f64>(0),
            )
            .ok())
    }

    /// The NOTE the operator left with a share, or `None` when there is no row
    /// or the row carries no note.
    ///
    /// Separate from [`Self::pool_share`] because that function is on the
    /// ranking path and is called per account per placement; the note is read
    /// only when a human is about to look at the number. Reading it there would
    /// have put a string allocation inside the ranker for nobody's benefit.
    pub fn pool_share_note(&self, account: &str) -> Result<Option<String>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT note FROM pool_shares WHERE account=?1",
                params![account],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten())
    }

    /// Set the share for one account. Upsert, because the operator revises this
    /// number by hand and a second `set` must move it rather than fail.
    ///
    /// **The bound is enforced HERE**, at the only door into the table, rather
    /// than trusted at every read: a share above 1.0 would claim more of an
    /// account than exists, and a negative one is not a quantity at all. Both
    /// are refused rather than clamped — silently accepting `1.5` and storing
    /// `1.0` would tell the operator his edit landed when it did not.
    ///
    /// # ZERO IS REFUSED, AND THAT IS THE 2026-09-20 CORRECTION
    ///
    /// Between 2026-09-08 and 2026-09-20 zero was admitted here and MEANT
    /// disabled. The operator overturned that:
    ///
    /// > *"I want disabled to be independent of share. Whether to include it in
    /// > the pool supersedes how much should be made available to the pool."*
    ///
    /// So inclusion moved to [`Self::set_account_disabled`] and its own table,
    /// and this column went back to being a pure QUANTITY — which has no zero,
    /// for the reason the bound originally had: a share of zero lent to the pool
    /// is not a small offer, it is the absence of one, and it is the denominator
    /// of [`crate::budgets::Budgets::pool_pressure`].
    ///
    /// **The refusal names the other door**, which is the entire point of having
    /// one. The defect this whole change is about is a capability reachable only
    /// by knowing that a magic value in a number field was a different act; an
    /// error that says "0 is not a share, call `accounts.disable`" is the
    /// cheapest possible place to say so, because it fires exactly when someone
    /// is reaching for the old spelling.
    ///
    /// # A disable no longer touches this column at all
    ///
    /// It used to overwrite the share with zero and stash the old value in
    /// `prior_share` so an enable could reconstruct it. Nothing is destroyed now,
    /// so nothing has to be remembered: an account sitting in `account_disabled`
    /// keeps whatever share it was lent, and enabling it is a DELETE from that
    /// table and no write here. `prior_share` is NULLed on every write below,
    /// which is how the retired column empties itself out of live databases.
    pub fn set_pool_share(&self, account: &str, share: f64, note: Option<&str>) -> Result<()> {
        if !share.is_finite() || share <= 0.0 || share > 1.0 {
            return Err(Error::Config(format!(
                "pool share must be a fraction in (0.0, 1.0] — got {share}. It is the portion of \
                 the resource offered to the pool manager (0.95 for 95%), never a percentage \
                 point and never a token count. A share of 0 is NOT how you take an account out \
                 of the pool: whether an account is in the pool at all is a separate fact with \
                 its own door (`accounts/{account}/disable`, `accounts/{account}/enable`), and it \
                 leaves this number exactly as you set it so you never have to remember it to come back."
            )));
        }
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO pool_shares (account, share, note, prior_share, updated_at)
             VALUES (?1, ?2, ?3, NULL, ?4)
             ON CONFLICT(account) DO UPDATE SET
                share=excluded.share,
                note=excluded.note,
                prior_share=NULL,
                updated_at=excluded.updated_at",
            params![account, share, note, now_rfc3339()],
        )?;
        Ok(())
    }

    /// **CLEAR the share override for one account — DELETE its row.** `true`
    /// when a row existed and is now gone; `false` when there was nothing to
    /// clear, which is NOT an error: "this account carries no override" is
    /// already true, and a clear is idempotent the way a `set` is.
    ///
    /// R2.S3g (2026-10-10). [`Self::set_pool_share`] was the only door into
    /// this table and it could only INSERT or move a row, so a key minted by an
    /// override alone — `aa-owned`, written by a test that reached the live
    /// daemon: no registration, no slot, no threads — could be created by one
    /// call and removed by none. The operator: *"The end state should clear
    /// aa-owned from my pool because its not real."* Deleting the row is that
    /// end state: absent is what `pool_share` already reports as "the operator
    /// has said nothing", and the `accounts` shape drops an unregistered key
    /// that nothing else holds.
    ///
    /// Reached over the wire as `accounts/{account}/pool_share_set` with
    /// `share: null` — present and null. An ABSENT `share` stays a malformed
    /// call; see that arm in `server.rs`.
    pub fn clear_pool_share(&self, account: &str) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute("DELETE FROM pool_shares WHERE account=?1", params![account])?;
        Ok(n > 0)
    }

    /// **IS THIS ACCOUNT OUT OF THE POOL — the instant the operator said so, or
    /// `None`.**
    ///
    /// Presence of the row IS the state; the instant is for the record and
    /// decides nothing. Nothing in this crate writes `account_disabled` on a
    /// timer and nothing reads the instant to expire it, so a disable cannot
    /// lift itself — the same property the share-zero disable had, kept
    /// deliberately.
    ///
    /// **A FAULT IS NOT A DISABLING**, and the direction matters: every caller
    /// goes through [`crate::budgets::Budgets::pool_share`], which degrades a
    /// read error to NOT disabled. A disable is something the operator SAID, so
    /// it may only ever come from a value actually read; turning an unreadable
    /// store into "out of the pool" would take an account out on the strength of
    /// a failure, which is CON-5 in a different costume.
    pub fn account_disabled_at(&self, account: &str) -> Result<Option<String>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT disabled_at FROM account_disabled WHERE account=?1",
                params![account],
                |r| r.get::<_, String>(0),
            )
            .ok())
    }

    /// The note the operator left with a disable, or `None`. Separate read from
    /// [`Self::account_disabled_at`] for the same reason `pool_share_note` is
    /// separate from `pool_share`: the state is what decides, the prose is what
    /// explains, and a caller that wants one must not be made to parse the other.
    pub fn account_disabled_note(&self, account: &str) -> Result<Option<String>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT note FROM account_disabled WHERE account=?1",
                params![account],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten())
    }

    /// **TAKE AN ACCOUNT OUT OF THE POOL, OR PUT IT BACK. The only writer of
    /// `account_disabled`.**
    ///
    /// `true` upserts the row; `false` DELETES it. Enabling deletes rather than
    /// flipping a column because presence is the state — see `schema.sql` for
    /// why a `disabled INTEGER` column would have admitted a row that means
    /// "not disabled" and is a row anyway.
    ///
    /// **It does not touch `pool_shares`, in either direction.** That is the
    /// whole separation: the share is what the operator lent, the row here is
    /// whether the pool may use it, and a round trip through disable and enable
    /// returns the account to exactly the weight it had with nothing
    /// reconstructed and nothing to remember. `tests/account_disable.rs` forces
    /// that by disabling an account on a 0.6 share and asserting 0.6 on the way
    /// out and on the way back.
    ///
    /// **Re-disabling is idempotent and does NOT re-date.** `ON CONFLICT DO
    /// NOTHING`: `disabled_at` is when he took it out, and a second press of the
    /// same key must not rewrite that history — the same lesson the old
    /// `prior_share` CASE had to learn when a second disable would overwrite the
    /// memo with the zero it was standing in for. The note is the exception: a
    /// second disable that CARRIES a note is him saying something new about why,
    /// and dropping it would silently discard his words.
    pub fn set_account_disabled(
        &self,
        account: &str,
        disabled: bool,
        note: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn()?;
        if disabled {
            conn.execute(
                "INSERT INTO account_disabled (account, disabled_at, note)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(account) DO UPDATE SET
                    note=COALESCE(excluded.note, account_disabled.note)",
                params![account, now_rfc3339(), note],
            )?;
        } else {
            conn.execute(
                "DELETE FROM account_disabled WHERE account=?1",
                params![account],
            )?;
        }
        Ok(())
    }

    /// Every account currently out of the pool, ordered by name — `(account,
    /// disabled_at, note)`. One read for the shape's hydrate handler, so a board
    /// coming up does not pay one query per account for a table that is usually
    /// empty.
    pub fn list_disabled_accounts(&self) -> Result<Vec<(String, String, Option<String>)>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT account, disabled_at, note FROM account_disabled ORDER BY account",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// Every share on record, for the operator to read back, ordered by the
    /// name — which is now both how he knows an account and what the row is
    /// keyed on.
    pub fn list_pool_shares(&self) -> Result<Vec<(String, f64, Option<String>)>> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare("SELECT account, share, note FROM pool_shares ORDER BY account")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    /// **When a turn last ENDED on this account** — the momentum clock.
    ///
    /// The prompt cache is warm for a few minutes after a turn and cold after
    /// that, so "was this account used just now" is the only thing that makes
    /// staying on it cheaper than moving. Read off `usage_ledger`, which
    /// already carries `(account, ts)` and already has an index on `account`.
    ///
    /// `accounting_rev = 1` only: the pre-fix cumulative-snapshot rows are
    /// excluded from every other derived number in this crate and there is no
    /// reason for the one criterion about RECENCY to be the exception.
    pub fn last_turn_end_on(&self, account: &str) -> Result<Option<String>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT MAX(ts) FROM usage_ledger WHERE account=?1 AND accounting_rev=1",
                params![account],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten())
    }

    /// Tokens consumed on `account` in `(from_ts, to_ts]` — the TOKEN side of
    /// a calibration pair, and the per-account consumption tracking DEC-12
    /// asks for. `model_filter` narrows to a model-scoped axis (the weekly
    /// fable pool must be paired with fable tokens only).
    ///
    /// Pre-fix rows are EXCLUDED. Feeding an inflated token count into
    /// `tokens_per_percent` estimates the account's limit far too high, which
    /// is the direction where a percentage budget silently fails to trip — the
    /// one failure mode calibration exists to prevent. Excluding them costs
    /// nothing real: no window had ever calibrated when this landed
    /// (`calibration_samples: 0` on every axis of every account).
    pub fn account_tokens_between(
        &self,
        account: &str,
        from_ts: &str,
        to_ts: &str,
        model_filter: Option<&str>,
    ) -> Result<u64> {
        let conn = self.read()?;
        let total: i64 = match model_filter {
            Some(m) => conn.query_row(
                &format!(
                    "SELECT COALESCE(SUM(input_tokens + output_tokens + cache_read_tokens
                                         + cache_creation_tokens), 0)
                     FROM usage_ledger
                     WHERE COALESCE(account,'default')=?1 AND ts > ?2 AND ts <= ?3
                       AND accounting_rev >= {ACCOUNTING_REV}
                       AND LOWER(model) LIKE ?4"
                ),
                params![account, from_ts, to_ts, format!("%{}%", m.to_lowercase())],
                |r| r.get(0),
            )?,
            None => conn.query_row(
                &format!(
                    "SELECT COALESCE(SUM(input_tokens + output_tokens + cache_read_tokens
                                         + cache_creation_tokens), 0)
                     FROM usage_ledger
                     WHERE COALESCE(account,'default')=?1 AND ts > ?2 AND ts <= ?3
                       AND accounting_rev >= {ACCOUNTING_REV}"
                ),
                params![account, from_ts, to_ts],
                |r| r.get(0),
            )?,
        };
        Ok(total.max(0) as u64)
    }

    /// Every (start, end) EXECUTION INTERVAL this daemon recorded for
    /// `account` whose end (`ts`) falls in `(from_ts, to_ts]` — the raw
    /// material for building OVERLAP WINDOWS (the operator's calibration
    /// method, msg #620: "find the beginning of the first one in the overlap
    /// and the end of the last one"). A `usage_ledger` row with no
    /// `started_at` (written before that column existed, or by a caller with
    /// no running-turn record) contributes no interval — never a guessed one.
    ///
    /// Pre-fix rows are excluded here too, and for the same reason as in
    /// [`Self::account_tokens_between`]: the two must agree about which rows
    /// exist, or a window would be built from intervals whose tokens the other
    /// half of the pair refuses to count, and the estimate would read zero.
    pub fn execution_intervals(
        &self,
        account: &str,
        from_ts: &str,
        to_ts: &str,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT started_at, ts FROM usage_ledger
             WHERE COALESCE(account,'default')=?1 AND started_at IS NOT NULL
               AND ts > ?2 AND ts <= ?3
               AND accounting_rev >= {ACCOUNTING_REV}
             ORDER BY started_at ASC"
        ))?;
        let rows = stmt.query_map(params![account, from_ts, to_ts], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<_, _>>().map_err(Error::from)
    }

    // ── the placement estimator (INTENT 83) ──────────────────────────────

    /// **What this thread's recent EXECUTIONS actually cost, in tokens, newest
    /// first** — the burn half of `budgets::Demand`.
    ///
    /// One row per execution, not per ledger row: a turn writes several delta
    /// rows (one per `result` line, which means one per subagent plus the
    /// parent), and it is the whole execution an account has to be able to
    /// carry. `started_at` is the execution's own identity, so grouping on it
    /// sums a turn back together; rows written before that column existed carry
    /// no execution identity and are skipped rather than counted as turns of
    /// one row each.
    ///
    /// `accounting_rev` is respected for the same reason every other derived
    /// number respects it — the pre-fix cumulative-snapshot rows are inflated
    /// by an unknown factor, and an estimate built on them would read a turn as
    /// seventeen times its true size.
    pub fn recent_turn_tokens(&self, thread_id: &str, limit: usize) -> Result<Vec<u64>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT SUM(input_tokens + output_tokens + cache_read_tokens
                        + cache_creation_tokens) AS total
             FROM usage_ledger
             WHERE thread_id = ?1 AND started_at IS NOT NULL
               AND accounting_rev >= {ACCOUNTING_REV}
             GROUP BY started_at
             ORDER BY started_at DESC
             LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![thread_id, limit as i64], |r| {
            Ok(r.get::<_, i64>(0).unwrap_or(0).max(0) as u64)
        })?;
        rows.collect::<std::result::Result<_, _>>().map_err(Error::from)
    }

    /// **How many SUBAGENTS this thread's recent turns spawned, newest first**
    /// — the fan-out half of `budgets::Demand`.
    ///
    /// Counted off the ledger the daemon already writes: `turn_started` opens a
    /// turn, every `agent_spawn` inside it is one subagent, and each subagent is
    /// a thread for the purposes of the death count (INTENT 83). A turn that
    /// spawned nothing contributes a genuine zero, which is the difference
    /// between "this thread does not fan out" and "we have never looked".
    ///
    /// The scan is bounded by `limit` turns' worth of events rather than by the
    /// whole ledger: it walks backwards from the newest event and stops as soon
    /// as it has seen `limit + 1` turn boundaries.
    pub fn recent_turn_fanout(&self, thread_id: &str, limit: usize) -> Result<Vec<u64>> {
        let conn = self.read()?;
        // A generous but bounded slice of the thread's tail. Turns interleave
        // with hundreds of other events, so the cap is on ROWS SCANNED and the
        // walk below is what turns them into per-turn counts.
        let scan = (limit.saturating_mul(400)).clamp(400, 20_000) as i64;
        let mut stmt = conn.prepare(
            "SELECT kind FROM (
                 SELECT seq, kind FROM events
                 WHERE thread_id = ?1 AND kind IN ('turn_started','agent_spawn')
                 ORDER BY seq DESC LIMIT ?2
             ) ORDER BY seq DESC",
        )?;
        let rows = stmt.query_map(params![thread_id, scan], |r| r.get::<_, String>(0))?;
        // Walking BACKWARDS, the spawns of a turn are seen before the
        // `turn_started` that opened them, so a count is closed by the boundary
        // rather than opened by it.
        let mut out: Vec<u64> = Vec::new();
        let mut spawns: u64 = 0;
        for kind in rows {
            match kind?.as_str() {
                "agent_spawn" => spawns += 1,
                _ => {
                    out.push(spawns);
                    spawns = 0;
                    if out.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    // ── budget-kill latches (R9.L4) ──────────────────────────────────────

    /// Every budget KILL the ledger holds: each `budget_tripped` event whose
    /// `action` is `kill`, oldest first, with whether the operator cancelled on
    /// that thread afterwards.
    ///
    /// READ-ONLY over the existing `events` ledger: the kill already writes
    /// this event (`check_trip_midturn`) and the cancel already writes its
    /// `turn_interrupted`, so the latch survives a restart without any new
    /// table, column or event kind. Walked per thread (`CROSS JOIN` fixes the
    /// join order) so each lookup rides `idx_events_thread_kind` instead of
    /// scanning the whole ledger. Called once per daemon life.
    pub fn budget_kill_latches(&self) -> Result<Vec<KillLatchRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT e.thread_id,
                    json_extract(e.payload_json, '$.budget_key'),
                    json_extract(e.payload_json, '$.axis'),
                    json_extract(e.payload_json, '$.limit_pct'),
                    e.ts,
                    COALESCE((SELECT MAX(i.seq) FROM events AS i
                              WHERE i.thread_id = e.thread_id
                                AND i.kind = 'turn_interrupted'
                                AND json_extract(i.payload_json, '$.trigger') = 'operator'),
                             -1) > e.seq
             FROM threads AS t CROSS JOIN events AS e
             WHERE e.thread_id = t.id
               AND e.kind = 'budget_tripped'
               AND json_extract(e.payload_json, '$.action') = 'kill'
             ORDER BY e.ts ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(KillLatchRow {
                thread_id: r.get(0)?,
                budget_key: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                axis: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                limit: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                killed_at: r.get(4)?,
                cancelled_after: r.get::<_, i64>(5)? != 0,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map(|v| v.into_iter().filter(|k| !k.budget_key.is_empty()).collect())
            .map_err(Error::from)
    }

    // ── parked turns (CON-4) ─────────────────────────────────────────────

    pub fn upsert_parked_turn(&self, p: &ParkedTurnRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO parked_turns
                (thread_id, prompt, message_ids_json, attempt, reason, budget_key, parked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(thread_id) DO UPDATE SET
                prompt=excluded.prompt, message_ids_json=excluded.message_ids_json,
                attempt=excluded.attempt, reason=excluded.reason,
                budget_key=excluded.budget_key, parked_at=excluded.parked_at",
            params![
                p.thread_id,
                p.prompt,
                serde_json::to_string(&p.message_ids)?,
                p.attempt as i64,
                p.reason,
                p.budget_key,
                p.parked_at,
            ],
        )?;
        Ok(())
    }

    fn map_parked(r: &rusqlite::Row) -> rusqlite::Result<ParkedTurnRow> {
        let ids_json: String = r.get(2)?;
        Ok(ParkedTurnRow {
            thread_id: r.get(0)?,
            prompt: r.get(1)?,
            message_ids: serde_json::from_str(&ids_json).unwrap_or_default(),
            attempt: r.get::<_, i64>(3)?.max(0) as u32,
            reason: r.get(4)?,
            budget_key: r.get(5)?,
            parked_at: r.get(6)?,
        })
    }

    const PARKED_COLS: &'static str =
        "thread_id, prompt, message_ids_json, attempt, reason, budget_key, parked_at";

    pub fn get_parked_turn(&self, thread_id: &str) -> Result<Option<ParkedTurnRow>> {
        let conn = self.read()?;
        let sql = format!(
            "SELECT {} FROM parked_turns WHERE thread_id = ?1",
            Self::PARKED_COLS
        );
        Ok(conn
            .query_row(&sql, params![thread_id], Self::map_parked)
            .ok())
    }

    pub fn list_parked_turns(&self) -> Result<Vec<ParkedTurnRow>> {
        let conn = self.read()?;
        let sql = format!(
            "SELECT {} FROM parked_turns ORDER BY parked_at ASC",
            Self::PARKED_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], Self::map_parked)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    pub fn clear_parked_turn(&self, thread_id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM parked_turns WHERE thread_id = ?1",
            params![thread_id],
        )?;
        Ok(())
    }

    // ── conversation history per account (round-7 item 7) ────────────────

    /// Record that `thread_id` has an established conversation on `account`.
    /// Called once a spawn on that account has actually produced a session, so
    /// the admission gate can tell "moving here resumes the conversation" from
    /// "moving here restarts it".
    pub fn record_thread_account(&self, thread_id: &str, account: &str) -> Result<()> {
        let conn = self.conn()?;
        let now = now_rfc3339();
        conn.execute(
            "INSERT INTO thread_accounts (thread_id, account, first_seen, last_seen)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(thread_id, account) DO UPDATE SET last_seen=excluded.last_seen",
            params![thread_id, account, now],
        )?;
        Ok(())
    }

    /// Every account this thread has conversation history on, most recent
    /// first (empty = a FRESH thread, free to start anywhere admissible).
    pub fn thread_accounts(&self, thread_id: &str) -> Result<Vec<String>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT account FROM thread_accounts WHERE thread_id=?1 ORDER BY last_seen DESC",
        )?;
        let rows = stmt.query_map(params![thread_id], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    // ── import watermarks (Q8) ───────────────────────────────────────────

    pub fn get_watermark(&self, source: &str) -> Result<Option<u64>> {
        let conn = self.read()?;
        let n: Option<i64> = conn
            .query_row(
                "SELECT line_count FROM import_watermarks WHERE source = ?1",
                params![source],
                |r| r.get(0),
            )
            .ok();
        Ok(n.map(|n| n.max(0) as u64))
    }

    pub fn set_watermark(&self, source: &str, thread_id: &str, line_count: u64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO import_watermarks (source, thread_id, line_count, imported_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(source) DO UPDATE SET
                line_count=excluded.line_count, imported_at=excluded.imported_at",
            params![source, thread_id, line_count as i64, now_rfc3339()],
        )?;
        Ok(())
    }
}

// ═════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod durability {
    //! **RULE 2.7's THIRD STAGE, AT THE STORE.**
    //!
    //! These tests live here and not in `tests/` for one reason: two of them
    //! have to move `PRAGMA synchronous`, and there is deliberately no public
    //! setter for it (see [`Store::open`]'s durability note). A module inside
    //! this file can reach `conn()`; nothing outside can, which is the point.

    use super::*;

    fn ev(thread: &str, seq: u64) -> crate::event::StoredEvent {
        crate::event::StoredEvent {
            thread_id: thread.to_string(),
            seq,
            message_id: Some(seq),
            ts: now_rfc3339(),
            event: crate::event::ThreadEvent::ContextInjected {
                text: format!("event {seq}"),
            },
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "threads-durability-{}-{tag}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("threads.db")
    }

    fn count(path: &Path) -> usize {
        let s = Store::open(path).unwrap();
        s.events_by_range("t", None, None, None, None).unwrap().len()
    }

    /// **THE COMPILED DEFAULT IS `FULL`, AND NOTHING CHECKED THAT UNTIL NOW.**
    ///
    /// [`Store::checkpoint`]'s doc has asserted it in prose since it was
    /// written, `docs/shapes.md` 2.7 builds an argument on it, and
    /// `docs/rules-audit.md` §4 cites it by line number — all of it resting on
    /// the `libsqlite3-sys` `bundled` build passing no `-DSQLITE_DEFAULT_*
    /// SYNCHRONOUS` flag. A vendored-sqlite bump that changed that default would
    /// silently turn every one of those claims false AND silently change which
    /// branch of [`Store::durability_barrier`] production takes.
    ///
    /// **Forced red** by adding `synchronous = 1` to [`Store::open`]'s pragmas:
    /// *``open()` sets journal_mode and nothing else; the compiled default must
    /// still be FULL — left: 1, right: 2*.
    #[test]
    fn the_compiled_synchronous_default_is_full() {
        let p = scratch("default");
        let s = Store::open(&p).unwrap();
        assert_eq!(
            s.synchronous().unwrap(),
            2,
            "`open()` sets journal_mode and nothing else; the compiled default \
             must still be FULL"
        );
        assert_eq!(s.durability(), Durability::PerCommit { level: 2 });
        assert!(
            !s.durability_barrier().unwrap().did_work(),
            "at FULL every commit already synced, so the barrier must say so \
             rather than check-pointing for appearances"
        );
    }

    /// **THE PROOF THAT IS NOT "THE CALL RETURNED": THE BYTES ARE IN THE
    /// DATABASE FILE, AND THEY WERE NOT BEFORE.**
    ///
    /// A test that asserts `durability_barrier()` returned `Ok` proves nothing —
    /// `Ok(())` is one line of Rust. So this one takes the observation a crash
    /// would take: it copies **only `threads.db`**, leaving the `-wal` and `-shm`
    /// behind, and opens the copy. Before the barrier that copy holds NOTHING,
    /// because at `synchronous=NORMAL` 50 committed events are frames in a log
    /// that no sync has promised and no checkpoint has folded. After the
    /// barrier it holds all 50.
    ///
    /// The before-copy is the control, and it is what makes the after-copy
    /// evidence instead of decoration: without it this test would pass against a
    /// barrier that did nothing, on a store that had already auto-checkpointed.
    ///
    /// **Forced red** by returning `Barrier::AlreadyDurable` from the
    /// `AtBarrier` arm — a barrier that answers and does nothing, which is the
    /// exact failure a `assert!(result.is_ok())` test could not see: it panics
    /// on `assert!(outcome.did_work())` with *AlreadyDurable { level: 1 }*, and
    /// with that assertion removed it fails the after-copy count `0` against
    /// `50`.
    #[test]
    fn a_barrier_puts_the_rows_in_the_database_file_and_not_only_in_the_log() {
        let p = scratch("file");
        let s = Store::open(&p).unwrap();
        // The one place in the crate that moves this pragma, and it moves it
        // for 50 writes in a temp directory. See `Store::open`.
        s.conn().unwrap().pragma_update(None, "synchronous", 1).unwrap();
        assert_eq!(s.durability(), Durability::AtBarrier);

        for i in 1..=50 {
            s.append_event(&ev("t", i)).unwrap();
        }

        let before = p.with_extension("before.db");
        std::fs::copy(&p, &before).unwrap();
        assert_eq!(
            count(&before),
            0,
            "the control: with the log left behind, the database file must be \
             empty — otherwise this test cannot tell a barrier from a no-op"
        );

        let outcome = s.durability_barrier().unwrap();
        assert!(outcome.did_work(), "{outcome:?}");

        let after = p.with_extension("after.db");
        std::fs::copy(&p, &after).unwrap();
        assert_eq!(
            count(&after),
            50,
            "after the barrier every event must be IN the database file — this \
             is the assertion that is about the data and not about the call"
        );
    }

    /// **A STORE THAT PROMISES NOTHING SAYS SO, AND THE BARRIER REFUSES.**
    ///
    /// Three ways to promise nothing and all three refuse, because the
    /// alternative — answering the weaker stage under this stage's name — is the
    /// silent downgrade rule 2.7 exists to forbid.
    ///
    /// **Forced red** by disabling the `path.is_none()` guard in
    /// [`Store::durability`], so an in-memory store reads the pragma off a
    /// connection that honestly answers `FULL` and claims `PerCommit`:
    /// *assertion failed: matches!(mem.durability(), Durability::None { .. })*.
    /// That is the whole hazard in one line — the pragma is right and the claim
    /// is still false, because there is no file.
    #[test]
    fn a_store_that_cannot_promise_durability_refuses_rather_than_downgrading() {
        let mem = Store::open_in_memory().unwrap();
        assert!(matches!(mem.durability(), Durability::None { .. }));
        let e = mem.durability_barrier().unwrap_err().to_string();
        assert!(
            e.contains("promises no durability") && e.contains("in-memory"),
            "the refusal must say WHY, since a caller can act on `in memory` \
             and cannot act on `no`: {e}"
        );

        let p = scratch("off");
        let s = Store::open(&p).unwrap();
        s.conn().unwrap().pragma_update(None, "synchronous", 0).unwrap();
        assert!(matches!(s.durability(), Durability::None { .. }));
        let e = s.durability_barrier().unwrap_err().to_string();
        assert!(
            e.contains("synchronous=OFF"),
            "a FILE-backed store can still promise nothing, and the reason is \
             not the file: {e}"
        );
    }

    /// **EVERY COMMIT SURVIVES A REOPEN THAT DOES NO CLEAN ANYTHING.**
    ///
    /// No checkpoint, no `checkpoint_for_shutdown`, no graceful exit — the
    /// store is dropped and a FRESH one is opened on the same path, which is the
    /// WAL-recovery path a boot after a hard kill takes. It is the weaker half
    /// of the proof above (the page cache is still warm, so it does not isolate
    /// the fsync) and it is the half that runs against PRODUCTION's `FULL`,
    /// where the barrier does no work and the survival is the store's own.
    ///
    /// **Forced red** by making `append_event` silently skip even `seq`s — the
    /// shape of every "the write did not land" defect: the reopened count fails
    /// *left: 13, right: 25*, so this test is pinned to HOW MANY survived and
    /// not to "something did".
    #[test]
    fn commits_survive_a_reopen_with_no_clean_shutdown_at_all() {
        let p = scratch("reopen");
        {
            let s = Store::open(&p).unwrap();
            assert_eq!(s.durability(), Durability::PerCommit { level: 2 });
            for i in 1..=25 {
                s.append_event(&ev("t", i)).unwrap();
            }
            let _ = s.durability_barrier().unwrap();
            // and nothing else: no checkpoint, no shutdown path.
        }
        assert_eq!(count(&p), 25);
    }
}
