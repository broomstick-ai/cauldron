-- The schema of production's tts store, read 2026-10-10 off a read-only
-- `.backup` copy of ~/.cauldron/data/prod/tts/store/tts.db (live writer: tts
-- pid 80756, 2303409776b0-dev). Schema only: no row of his data is here.
CREATE TABLE renders (
    hash               TEXT PRIMARY KEY,   -- sha256_hex(norm(text)|fingerprint)
    module             TEXT NOT NULL,
    module_fingerprint TEXT NOT NULL,
    state              TEXT NOT NULL CHECK (state IN ('queued','synthesizing','warm','failed')),
    rel_path           TEXT,               -- relative to <data_dir>/audio
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
CREATE INDEX renders_state ON renders(state);
CREATE TABLE entries (
    key_name      TEXT NOT NULL,
    hash          TEXT NOT NULL,
    registered_at TEXT NOT NULL,
    last_used_at  TEXT NOT NULL,
    use_count     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (key_name, hash)
);
CREATE INDEX entries_lru ON entries(key_name, last_used_at);
CREATE TABLE synth_locks (
    hash        TEXT PRIMARY KEY,
    boot_id     TEXT NOT NULL,
    daemon_pid  INTEGER NOT NULL,
    lane        TEXT NOT NULL,
    acquired_at TEXT NOT NULL
);
CREATE TABLE pending_unlink (
    rel_path   TEXT PRIMARY KEY,
    bytes      INTEGER NOT NULL,
    queued_at  TEXT NOT NULL
);
CREATE TABLE key_counters (
    key_name       TEXT PRIMARY KEY,
    synth_total    INTEGER NOT NULL DEFAULT 0,
    evicted_total  INTEGER NOT NULL DEFAULT 0,
    fail_total     INTEGER NOT NULL DEFAULT 0,
    bytes_written  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS "cache_keys" (
                 name               TEXT PRIMARY KEY,
                 mode               TEXT NOT NULL CHECK (mode IN ('keep_warm','lru','lru_count')),
                 max_bytes          INTEGER,
                 max_entries        INTEGER,
                 warn_bytes         INTEGER,
                 refuse_bytes       INTEGER,
                 queueing           TEXT NOT NULL CHECK (queueing IN ('fifo','lifo')),
                 module             TEXT NOT NULL,
                 module_config      TEXT NOT NULL,
                 module_fingerprint TEXT NOT NULL,
                 priority           INTEGER NOT NULL DEFAULT 50,
                 min_age_secs       INTEGER NOT NULL DEFAULT 120,
                 enabled            INTEGER NOT NULL DEFAULT 1,
                 updated_at         TEXT NOT NULL,
                 CHECK ((mode = 'lru') = (max_bytes IS NOT NULL)),
                 CHECK ((mode = 'lru_count') = (max_entries IS NOT NULL)),
                 CHECK (max_entries IS NULL OR max_entries >= 1)
             );
