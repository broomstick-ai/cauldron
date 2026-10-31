-- The tts store. Applied with `execute_batch` on every open; `IF NOT EXISTS`
-- throughout, so opening production's existing store changes nothing in it.
--
-- There is no `text` column anywhere: "we don't even need the text itself.
-- All we need is the hash of the text."

-- A cache KEY: a named configuration, projected from tts.toml on every load so
-- joins and constraints work against real rows.
CREATE TABLE IF NOT EXISTS cache_keys (
    name               TEXT PRIMARY KEY,
    mode               TEXT NOT NULL CHECK (mode IN ('keep_warm','lru','lru_count')),
    max_bytes          INTEGER,
    max_entries        INTEGER,
    warn_bytes         INTEGER,
    refuse_bytes       INTEGER,
    queueing           TEXT NOT NULL CHECK (queueing IN ('fifo','lifo')),
    module             TEXT NOT NULL,
    module_config      TEXT NOT NULL,   -- canonical JSON
    module_fingerprint TEXT NOT NULL,   -- what enters the hash
    -- The floor of the published scale (src/priority.rs). Production's table
    -- says DEFAULT 50, from a since-retired scale; nothing relies on the
    -- default, because every projection writes `priority` explicitly.
    priority           INTEGER NOT NULL DEFAULT 0,
    min_age_secs       INTEGER NOT NULL DEFAULT 120,
    enabled            INTEGER NOT NULL DEFAULT 1,
    updated_at         TEXT NOT NULL,
    -- lru <=> max_bytes and lru_count <=> max_entries, enforced by the database.
    CHECK ((mode = 'lru') = (max_bytes IS NOT NULL)),
    CHECK ((mode = 'lru_count') = (max_entries IS NOT NULL)),
    CHECK (max_entries IS NULL OR max_entries >= 1)
);

-- The BYTES: one row per audio file, shared by every key that wants it.
-- `failed` and the fail_* columns are a record of the LAST attempt, never a
-- gate: a request for a failed hash always re-synthesizes, and no failed row
-- survives a boot.
CREATE TABLE IF NOT EXISTS renders (
    hash               TEXT PRIMARY KEY,
    module             TEXT NOT NULL,
    module_fingerprint TEXT NOT NULL,
    state              TEXT NOT NULL CHECK (state IN ('queued','synthesizing','warm','failed')),
    rel_path           TEXT,               -- relative to audio/
    bytes              INTEGER NOT NULL DEFAULT 0,
    format             TEXT NOT NULL DEFAULT 'aiff',
    char_len           INTEGER NOT NULL DEFAULT 0,
    synth_ms           INTEGER,
    fail_code          TEXT,
    fail_reason        TEXT,
    fail_count         INTEGER NOT NULL DEFAULT 0,
    created_at         TEXT NOT NULL,
    completed_at       TEXT
);
CREATE INDEX IF NOT EXISTS renders_state ON renders(state);

-- MEMBERSHIP: which key wants which render, with that key's recency. Two
-- tables so keys sharing a module share the bytes.
CREATE TABLE IF NOT EXISTS entries (
    key_name      TEXT NOT NULL,
    hash          TEXT NOT NULL,
    registered_at TEXT NOT NULL,
    last_used_at  TEXT NOT NULL,
    use_count     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (key_name, hash)
);
CREATE INDEX IF NOT EXISTS entries_lru ON entries(key_name, last_used_at);

-- Cross-process single-flight. The exclusion IS the primary key, claimed with
-- INSERT ... ON CONFLICT DO NOTHING.
CREATE TABLE IF NOT EXISTS synth_locks (
    hash        TEXT PRIMARY KEY,
    boot_id     TEXT NOT NULL,
    daemon_pid  INTEGER NOT NULL,
    lane        TEXT NOT NULL,
    acquired_at TEXT NOT NULL
);

-- Rows are committed away FIRST and bytes unlinked SECOND, so the failure
-- mode is an orphaned file (swept later), never a row naming a missing file.
CREATE TABLE IF NOT EXISTS pending_unlink (
    rel_path   TEXT PRIMARY KEY,
    bytes      INTEGER NOT NULL,
    queued_at  TEXT NOT NULL
);

-- Monotonic per-key counters: durable observability without an event ledger.
CREATE TABLE IF NOT EXISTS key_counters (
    key_name       TEXT PRIMARY KEY,
    synth_total    INTEGER NOT NULL DEFAULT 0,
    evicted_total  INTEGER NOT NULL DEFAULT 0,
    fail_total     INTEGER NOT NULL DEFAULT 0,
    bytes_written  INTEGER NOT NULL DEFAULT 0
);
