-- threads persistence schema (SQLite, WAL mode set on open).
-- Append-only: add new tables / columns at the bottom. Applied on every open
-- via execute_batch (IF NOT EXISTS everywhere).

-- One row per thread. `id` == the claude session uuid — caller-supplied or
-- minted (Q2), which is what lets AUI's existing threads keep their sessions.
-- `settings_json` is the live ThreadSettings blob (Q9), mutable mid-thread.
--
-- THERE IS NO `status` COLUMN, and adding one back is a regression. Whether a
-- thread is running is DERIVED — once, fleet-wide, by
-- `ThreadManager::thread_states` — from `thread_locks`, `parked_turns`,
-- `retries`, `queue` and the in-memory park latch. The column that used to sit
-- on the second line here was a cache of that answer written by three separate
-- paths and repaired by four more, and it still reported two completed threads
-- as `busy` for twenty-five hours. See `DROPPED_COLUMNS` in `store.rs`; the
-- `ALTER TABLE threads DROP COLUMN status` that removes it from databases
-- created before this change lives there.
CREATE TABLE IF NOT EXISTS threads (
    id               TEXT PRIMARY KEY,
    account          TEXT,
    cwd              TEXT,
    model            TEXT,
    settings_json    TEXT NOT NULL,
    last_seq         INTEGER NOT NULL DEFAULT 0,
    -- The next small per-thread message ordinal (the spoken id).
    next_message_id  INTEGER NOT NULL DEFAULT 1,
    -- The highest message id swallowed by the last compaction (0 = none) —
    -- lets the next Compacted event carry its pre-compaction id range (§12).
    last_compaction_message_id INTEGER NOT NULL DEFAULT 0,
    -- Nonzero when the next turn must carry the post-compaction restatement.
    needs_restate    INTEGER NOT NULL DEFAULT 0,
    -- The calendar day (YYYY-MM-DD, local) last STATED in a per-message
    -- metadata block. NULL = never stated. The date is re-stated only when the
    -- day rolls over (INTENT 65's token thrift), so this must survive a daemon
    -- restart — and the daemon now restarts itself on its own binary change.
    last_meta_date   TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    -- **WHEN HE LAST PUT WORDS INTO THIS THREAD** — the `ts` of the newest
    -- `user_message` event on it, and the column `Store::recent_threads` orders
    -- the `threads/recent-updates` index by.
    --
    -- Operator directive, verbatim: *"the recents need to be ordered on last
    -- user input, not on when they were created or any other events, including
    -- renames."* `updated_at` on the line above fails every one of those three
    -- exclusions — `bump_last_seq` advances it on EVERY ledger append (58% of
    -- which are `stream_event`), `update_thread_settings` advances it on a
    -- rename, and `upsert_thread` stamps it at creation. It is still the right
    -- column for "when did anything about this row change", which is what
    -- `list_threads` wants; it is the wrong one for recency, and ordering the
    -- index by it was the defect this column exists to remove.
    --
    -- NULL means HE HAS NEVER SPOKEN HERE, and that is a real state rather than
    -- a missing value: an order on last user input has no position to offer a
    -- thread with no last user input, so such a thread holds no place in the
    -- index at all. `Store::recent_threads` filters it out, and the live fold
    -- never admits one either (it moves a thread only on a `user_message`), so
    -- the two halves of the order agree by construction rather than by care.
    --
    -- Written by exactly one statement, `bump_last_seq`, and only when the
    -- append that calls it was a `user_message` — see `ThreadEvent::is_user_message`.
    -- Databases created before this column get it from `MIGRATIONS` and are
    -- BACKFILLED once by `Store::backfill_last_user_input_at`.
    last_user_input_at TEXT
);

-- The ledger: every persisted ThreadEvent, densely sequenced per thread.
-- `message_id` is the small addressable ordinal (NULL on non-addressable
-- events); `ts` is returned by history (G5). Liveness beats NEVER land here
-- (G6 — they are a distinct transient frame type).
CREATE TABLE IF NOT EXISTS events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id    TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    message_id   INTEGER,
    kind         TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    ts           TEXT NOT NULL,
    UNIQUE (thread_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_events_thread_seq  ON events(thread_id, seq);
CREATE INDEX IF NOT EXISTS idx_events_thread_msg  ON events(thread_id, message_id);
CREATE INDEX IF NOT EXISTS idx_events_thread_kind ON events(thread_id, kind);

-- The thread-side send queue (Q4). A row references the UserMessage already
-- persisted in the ledger (the ledger records what was said when it was said;
-- the queue only owns delivery timing).
CREATE TABLE IF NOT EXISTS queue (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id    TEXT NOT NULL,
    message_id   INTEGER NOT NULL,
    text         TEXT NOT NULL,
    intent       TEXT NOT NULL,              -- immediate | turn_end
    -- waiting | delivered | cancelled | failed
    --
    -- `failed` (2026-08-25) is the state that keeps the operator's words when a
    -- retry ladder runs out. It is deliberately a state NO dispatcher selects:
    -- `waiting_queue` and `threads_with_waiting_queue` both filter on
    -- 'waiting', so a failed row is held rather than re-fired, and moving it
    -- back to 'waiting' is an explicit act (`Store::reselect_failed`). Before
    -- it existed, an exhausted ladder left its rows on 'delivered' forever —
    -- identical to a delivery that succeeded — and the prompt's only other copy
    -- was DELETEd by `clear_retry`. That was a silent drop of something he
    -- said, and it is the one outcome this module is not allowed to produce.
    state        TEXT NOT NULL DEFAULT 'waiting',
    created_at   TEXT NOT NULL,
    delivered_at TEXT,
    -- The caller's opaque per-message metadata blob, carried to delivery. ONE
    -- declared path out of it reaches the model (`aui.audio_started_at`, which
    -- yields the duration and `aui.thread_event_idx`, which yields the idx
    -- line); the rest stays invisible, per DEC-55 category (a).
    metadata_json TEXT,
    -- The header DECLARATIONS the sender supplied, validated at accept time
    -- (INTENT 66b). A SEPARATE column from `metadata_json`, deliberately:
    -- structure threads reads and renders must never share a channel with the
    -- blob threads promises never to read.
    headers_json TEXT
);
CREATE INDEX IF NOT EXISTS idx_queue_thread_state ON queue(thread_id, state);

-- Persisted retry deadlines (Q5/G10): a daemon restart re-arms these instead
-- of losing the turn. One pending retry per thread. The prompt is stored so
-- the retry re-sends the exact delivery (CCW's full-prompt retry semantics —
-- the prompt of a failed spawn is NOT reliably in claude's session, G23).
CREATE TABLE IF NOT EXISTS retries (
    thread_id        TEXT PRIMARY KEY,
    prompt           TEXT NOT NULL,
    message_ids_json TEXT NOT NULL,
    attempt          INTEGER NOT NULL,
    kind             TEXT NOT NULL,
    reason           TEXT NOT NULL,
    -- **THERE IS NO `retry_at` HERE, AND ITS ABSENCE IS THE POINT.** The column
    -- existed and was dropped (see `store::DROPPED_COLUMNS`) on the operator's
    -- ruling: *"There should not be a per-thread retry timestamp... if any thread
    -- is parked then the pool needs a retry, not the thread. Any parked threads
    -- get checked at any opportunity."* A row in this table is simply a PARKED
    -- PROMPT. What decides when it is reconsidered is not in this table at all:
    -- it is the pool's own cadence (`runtime::PoolCadence`) plus every event that
    -- rings the placement bell. Do not re-add a per-row instant.
    --
    -- The account that produced the failure, so a starving retry can name the
    -- login it is piling up behind. NULL for rows written before the column
    -- existed; `recover` backfills those at boot.
    account          TEXT,
    created_at       TEXT NOT NULL
);

-- **THE DELIVERY'S OWN CLOCK — two BIRTH instants, never a deadline.**
--
-- One row per thread, describing the delivery currently being attempted:
--   * `turn_started_at` — when its FIRST attempt began. A retry must not reset
--     it (thread 60f0380d, 2026-10-14: forty retries over 2½ days each looked
--     freshly started, so the thread read as busy rather than stuck).
--   * `streak_kind` / `streak_since` — the failure class of the CURRENT run of
--     consecutive same-class failures, and when that run began. This is what a
--     wall-clock retry budget is measured against (`retry::connectivity_exhausted`,
--     `retry::server_error_exhausted`), because an attempt COUNT does not bound
--     time: in 60f0380d each attempt ran ~3 minutes of real subagent work.
--
-- Kept apart from `retries` on purpose: a retry row is cleared when a refused
-- retry is handed over to `parked_turns`, and a clock living on that row would
-- restart its budget at every hand-over. This one is keyed by the delivery's
-- message ids and is only replaced when a DIFFERENT delivery begins
-- (`Store::begin_delivery_attempt`). Neither instant is "when to retry next" —
-- the no-per-thread-retry-timestamp ruling on `retries` is untouched.
CREATE TABLE IF NOT EXISTS delivery_clocks (
    thread_id        TEXT PRIMARY KEY,
    message_ids_json TEXT NOT NULL,
    turn_started_at  TEXT NOT NULL,
    streak_kind      TEXT,
    streak_since     TEXT
);

-- One row per in-flight turn (G1): child pid + spawn cookie + the durable
-- stdout file + how far the daemon has parsed it. On boot, still-running
-- children are re-adopted by tailing `output_path` from `byte_offset`;
-- dead ones become a `turn_interrupted` recovery event.
CREATE TABLE IF NOT EXISTS running_turns (
    thread_id    TEXT PRIMARY KEY,
    pid          INTEGER NOT NULL,
    spawn_cookie TEXT NOT NULL,
    output_path  TEXT NOT NULL,
    byte_offset  INTEGER NOT NULL DEFAULT 0,
    started_at   TEXT NOT NULL,
    -- WHAT THE OPERATOR SAID, so a re-adopted turn can be torn down and
    -- re-delivered instead of declining to act. `adopt_turn` used to refuse to
    -- honour a usage-limit kill because it "does not hold the prompt"; it holds
    -- it now. NULLABLE ON PURPOSE: a row written by an older daemon reads back
    -- as None, and `adopt_turn` falls back to exactly its old behaviour for it,
    -- which makes the limitation a one-restart transient instead of a contract.
    prompt          TEXT,
    -- The deliveries this execution is answering, for the `parked_turns` /
    -- `retries` row a teardown writes. Same JSON shape as everywhere else.
    message_ids_json TEXT,
    -- Which attempt this is, so a teardown's retry row continues the ladder
    -- rather than restarting it at 1 and re-buying the whole ceiling.
    attempt          INTEGER NOT NULL DEFAULT 1
);

-- Tracked per-account limit state (multi-account scaffolding, §10). Updated
-- from observed limit_hit events; `limit_until` is RFC3339 UTC.
CREATE TABLE IF NOT EXISTS accounts (
    name             TEXT PRIMARY KEY,
    config_dir       TEXT,
    limit_until      TEXT,
    last_limit_prose TEXT,
    updated_at       TEXT,
    -- The LOGIN this label points at, as an opaque hash of its `oauthAccount`
    -- identity (`accounts::identity_fingerprint`). Two labels carrying the same
    -- value are two names for ONE account: they share one set of usage limits,
    -- so failing over between them re-hits the limit we ran from and any
    -- "N accounts means N×100% capacity" claim over them is false. NULL = the
    -- config dir held no readable identity; absence never counts as a match.
    identity         TEXT,
    -- Set when a turn on this account failed in a way that PROVES the login is
    -- dead (`accounts::is_dead_login_error` — an OAuth session that cannot be
    -- refreshed needs a human `/login`, not a retry). While it is set the
    -- account is excluded from selection and from failover, and stays fully
    -- visible in `accounts status`. Cleared by a successful turn.
    dead_since       TEXT,
    dead_reason      TEXT,
    -- The LOGIN `limit_until` was actually observed on, stamped from `identity`
    -- at the moment the 429 was recorded.
    --
    -- A limit is a fact about a LOGIN; `name` is only a label pointing at one.
    -- A label can be re-pointed — `default` has no config dir, so it is whatever
    -- `~/.claude.json` currently holds, and a `/login` swaps the login under it
    -- without the daemon being told. When that happens the label keeps a limit
    -- that belongs to a login it no longer names, and because `with_aliases`
    -- widens a bar across every label of the (new) login, the stale bar is
    -- promoted into a bar on a completely different, healthy account. That is
    -- the 2026-08-02 incident exactly: `default` carried Login B's Aug-1 weekly
    -- limit, was re-logged into Login A at ~17:00Z, and the boot at 19:23:39Z
    -- refreshed `identity` — barring Login A, which was at 29% and serving
    -- turns, until the foreign clock ran out at 01:00Z.
    --
    -- So a `limit_until` is honoured only while this column still equals
    -- `identity`. NULL on either side means "we cannot tell", and a limit we
    -- cannot attribute is left BARRING: absence of evidence never un-bars.
    limit_identity   TEXT,
    -- Which quota the 429 named (`gauge::Axis` — session | weekly_all |
    -- weekly_fable), or NULL when the refusal prose named none. Recorded for
    -- visibility, not yet for scoping: the bar it annotates is still
    -- account-wide. See ISSUE-2 in the account-limit diagnosis report.
    limit_axis       TEXT
);

-- Per-turn spend rows (Q11): written from every result_turn's modelUsage, so
-- per-thread and per-account spend are one GROUP BY away. Supersedes CCW's
-- never-written invocations/calibrations machinery.
--
-- `started_at` (added for the overlap-window calibration method, msg #620) is
-- the EXECUTION's own start instant — `ts` was always its end. A row with
-- `started_at` present is one endpoint of an "our own execution ran during
-- [started_at, ts]" interval; NULL (rows written before this column existed)
-- simply contributes no interval, never a guessed one.
--
-- `accounting_rev` is the SHAPE the row was written in, and it exists because
-- history cannot be trusted and must not be rewritten:
--
--   0 — CUMULATIVE-SNAPSHOT rows. A `result` line's `modelUsage` is the running
--       per-model total for the WHOLE claude process, and every result of a
--       turn (one per subagent, plus the parent's) repeats or extends it. The
--       pre-fix writer stored each snapshot verbatim, so a turn with N results
--       was recorded ~N times over. Measured live 2026-07-31: one turn recorded
--       17 times, one thread reading $2,845 against a true ~$199. These rows
--       are INFLATED BY AN UNKNOWN FACTOR and are excluded from every derived
--       number; they are kept, not deleted, because they are the evidence.
--   1 — DELTA rows (this build). Each row is the increment over the previous
--       snapshot of the same execution, so the rows of a turn SUM to what the
--       API actually billed for that turn.
CREATE TABLE IF NOT EXISTS usage_ledger (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id             TEXT NOT NULL,
    account               TEXT,
    model                 TEXT NOT NULL,
    input_tokens          INTEGER NOT NULL DEFAULT 0,
    output_tokens         INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd              REAL NOT NULL DEFAULT 0,
    ts                    TEXT NOT NULL,
    started_at            TEXT,
    accounting_rev        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_ledger_thread  ON usage_ledger(thread_id);
CREATE INDEX IF NOT EXISTS idx_ledger_account ON usage_ledger(account);

-- The CUMULATIVE per-model usage high-water mark of the execution currently
-- running on a thread — the baseline every `result_turn` delta is taken
-- against.
--
-- Durable rather than in-memory because a daemon restart RE-ADOPTS a running
-- turn (`adopt_turn`) and keeps tailing its output: an in-memory baseline
-- would be lost across the restart and the next result line, which carries the
-- whole running total, would be booked a second time in full.
--
-- `epoch` is the execution's `started_at`. A result arriving under a different
-- epoch means a new claude process, whose counters start at zero again, so the
-- watermark resets rather than producing a negative delta.
CREATE TABLE IF NOT EXISTS turn_usage_watermarks (
    thread_id             TEXT NOT NULL,
    model                 TEXT NOT NULL,
    epoch                 TEXT NOT NULL,
    input_tokens          INTEGER NOT NULL DEFAULT 0,
    output_tokens         INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd              REAL NOT NULL DEFAULT 0,
    updated_at            TEXT NOT NULL,
    PRIMARY KEY (thread_id, model)
);

-- Import watermarks (Q8): per imported AUI thread, how many JSONL lines have
-- been ingested — the cursor the post-cutover delta reconciliation resumes
-- from. `source` is "<project_root>|<aui_thread_id>".
CREATE TABLE IF NOT EXISTS import_watermarks (
    source      TEXT PRIMARY KEY,
    thread_id   TEXT NOT NULL,
    line_count  INTEGER NOT NULL,
    imported_at TEXT NOT NULL
);

-- ─────────────────────────────────────────────────────────────────────────────
-- Budgets module (§15 of the CCW-migration design doc; DEC-4 … DEC-19).
-- Everything below is owned by `budgets.rs` / `gauge.rs`.
-- ─────────────────────────────────────────────────────────────────────────────

-- One row per BUDGET KEY (DEC-4/DEC-14). The key namespace is independent of
-- thread ids; several threads may point at one key; a key outlives its
-- threads. `parent_key` is RESERVED for §14's nested workflow budgets — v1
-- never reads it (DEC-14: flat keys now, hierarchy reserved).
CREATE TABLE IF NOT EXISTS budgets (
    key          TEXT PRIMARY KEY,
    parent_key   TEXT,
    -- Percent of ONE ACCOUNT's session limit this key may consume, pooled
    -- across accounts (DEC-5). NULL = no limit on this meter.
    session_pct  REAL,
    -- Percent of ONE ACCOUNT's weekly (all-models) limit. NULL = no limit.
    weekly_pct   REAL,
    -- Percent of ONE ACCOUNT's weekly FABLE limit (round-7 item 4: budgets
    -- exist on all three percentage axes, not two). NULL = no limit.
    fable_pct    REAL,
    -- The CASH axis (round-7 item 5): dollars, POOLED across accounts —
    -- $20 means $20 total, not $20 per account. NULL = no limit.
    cash_usd     REAL,
    -- Which accounts this key applies to, as a JSON array (round-7 item 6).
    -- The crate NEVER defaults to all accounts: an empty array is rejected at
    -- write time. A client (AUI) may CHOOSE to pass every account; that is the
    -- client's default, not the crate's.
    accounts_json TEXT NOT NULL DEFAULT '[]',
    -- 'kill' | 'finish' on trip. DEFAULT FINISH (round-7 item 3 supersedes
    -- DEC-13's default-kill; kill remains a per-budget toggle).
    on_trip      TEXT NOT NULL DEFAULT 'finish',
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    -- When this key was DISABLED, or NULL while it is live (Q-34, msg #751:
    -- "I don't think we should delete a budget key. I think we should disable
    -- a budget key so we can still query and see information about the
    -- budget… So no deleting"). A disabled key DENIES every admission that
    -- names it — the turn parks, CON-4 — while every row of its accounting
    -- stays exactly where it is. There is no DELETE on this table.
    disabled_at  TEXT
);

-- Every charge against a budget key, in TOKENS, stamped with the account it
-- was drawn from (DEC-5: a budget's balance is account-agnostic, every charge
-- is account-specific) and the axis meter it feeds (DEC-16: two independent
-- meters). Charges are recorded against the key that was active AT CHARGE
-- TIME, so re-pointing a thread (DEC-14) never moves history.
--
-- The denominator (the per-account token limit) is NOT stored here: it is
-- re-estimated continuously (DEC-12), so the meter re-bases without ever
-- double-charging.
CREATE TABLE IF NOT EXISTS budget_charges (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    budget_key  TEXT NOT NULL,
    thread_id   TEXT NOT NULL,
    account     TEXT NOT NULL,
    axis        TEXT NOT NULL,          -- session | weekly_all
    tokens      INTEGER NOT NULL,
    cost_usd    REAL NOT NULL DEFAULT 0,
    ts          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_charges_key_axis ON budget_charges(budget_key, axis, account, ts);
-- The metering path asks "which accounts has this key ever drawn from?" on
-- EVERY streamed token-usage event (charge_budget -> check_trip_midturn ->
-- snapshot -> metered_accounts -> Store::charge_accounts). `account` is the
-- THIRD column of idx_charges_key_axis, so that index cannot seek to it, and
-- the question used to be answered by walking every charge row for the key.
-- `budget_key` is effectively a constant in practice (one key, 479k rows on
-- the operator's own daemon by 2026-09-11), so that walk was the whole table,
-- per streamed line, under the store's single connection mutex — which is how
-- a daemon that is still running stops answering. This index puts `account`
-- second so `Store::charge_accounts` can SKIP between distinct accounts
-- instead of scanning them, making the cost O(distinct accounts) rather than
-- O(rows ever charged). See `Store::charge_accounts`.
CREATE INDEX IF NOT EXISTS idx_charges_key_account ON budget_charges(budget_key, account);

-- Raw gauge samples (DEC-15). One row per observed percent, whatever layer it
-- came from. `resets_at` is RFC3339 UTC (epoch seconds from the stream line,
-- resolved prose from /usage and 429s) and is what makes a window rollover
-- detectable: when it advances, the meter's window advanced.
CREATE TABLE IF NOT EXISTS usage_observations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account     TEXT NOT NULL,
    axis        TEXT NOT NULL,          -- session | weekly_all | weekly_fable
    pct         REAL NOT NULL,          -- normalized 0-100
    resets_at   TEXT,
    source      TEXT NOT NULL,          -- usage_poll | rate_limit_event | limit_hit
    observed_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_obs_account_axis ON usage_observations(account, axis, id);

-- The continuous limit estimate (DEC-12), per account per axis: how many
-- tokens one percent of that axis costs, as a running mean over real
-- calibration samples. A missing row means UNCALIBRATED — the axis reports
-- `calibrating` and gates nothing (CON-5 / CCW INTENT #202).
CREATE TABLE IF NOT EXISTS limit_estimates (
    account            TEXT NOT NULL,
    axis               TEXT NOT NULL,
    tokens_per_percent REAL NOT NULL,
    sample_count       INTEGER NOT NULL,
    -- The observation id the last sample was paired against (so the same
    -- pair is never counted twice).
    last_obs_id        INTEGER NOT NULL DEFAULT 0,
    updated_at         TEXT NOT NULL,
    PRIMARY KEY (account, axis)
);

-- PER-PERIOD LIMIT-ESTIMATE HISTORY. The table above holds ONE row per
-- (account, axis), overwritten forever, so it can say what capacity is and
-- never what it WAS. This table is that history, and it exists to answer a
-- PURCHASING question the operator asked directly: "I want to snapshot this
-- over time so we can see if this is consistent on a per account basis,
-- because it might affect how I allocate money."
--
-- It is NOT a timer sample. It is bucketed by the real reset window, per his
-- design: "every usage limit that we have, maybe you update our estimate for
-- that usage limit in that period, be it session limit, be it weekly limit,
-- be it Fable weekly limit. And then every time the update changes for that
-- period, update it. And then when the period is over, just leave it and move
-- forward and start recording the next one."
--
-- So: one row per (login, axis, window). While the window is live the row is
-- UPDATED IN PLACE on every estimate change; when the window rolls the row is
-- simply never touched again and becomes that period's final estimate.
--
-- ── WHY THE KEY IS THE LOGIN AND NOT THE ACCOUNT LABEL ──────────────────
--
-- `limit_estimates.account` is a LABEL, and a label can be re-pointed. The
-- live database proves the cost: it carries a `default` row, and `default` is
-- not an account at all — it is a pointer at whichever login the operator has
-- made current in `~/.claude.json` for his own manual invocations. That row
-- has not moved since 2026-08-03 while `default`'s fingerprint now resolves
-- to `owner-gmail`, whose own row disagrees with it by 42% on weekly
-- capacity. Two rows, one login, two different answers.
--
-- History keyed by a pointer is history that silently re-attributes itself.
-- So this table is keyed by `identity` — the opaque login fingerprint
-- (`accounts.identity`) — with the label carried ALONGSIDE it for reading.
-- `default` is never recorded here at all: the daemon knows all three real
-- accounts and has no business holding a concept of the operator's current
-- default (CON-27).
--
-- WHAT IS NOT HERE, AND WHY: the account CLASS (business-email vs
-- personal-email), which is the axis the purchasing decision actually turns
-- on. The daemon cannot derive it — `identity_fingerprint` hashes the email
-- and never keeps the address, so no email domain exists anywhere in this
-- process. `identity` plus `account_label` plus `config_dir` are the stable
-- identity we DO hold, and they are recorded so the classification can be
-- applied later by hand rather than being lost now.
CREATE TABLE IF NOT EXISTS limit_estimate_periods (
    -- The LOGIN. `accounts.identity` when it is readable, else the literal
    -- `label:<name>` — a synthetic key that is honest about being a label,
    -- and never collides with a real fingerprint (which has no colon).
    identity           TEXT NOT NULL,
    axis               TEXT NOT NULL,   -- session | weekly_all | weekly_fable
    -- The window's RESET INSTANT, RFC3339 UTC, taken from the newest gauge
    -- observation for this (account, axis) — the same reset instant
    -- `budgets::window_start` meters against and `gauge::same_window`
    -- compares. This crate has exactly one notion of a window and this is it.
    period_end         TEXT NOT NULL,
    -- The registry label this login answered to when the row was last
    -- written. Diagnostic and human-readable; NEVER part of the key.
    account_label      TEXT NOT NULL,
    config_dir         TEXT,
    tokens_per_percent REAL NOT NULL,
    -- `tokens_per_percent * 100` — the whole pool, denormalized because it is
    -- the number a human comparing accounts actually wants to read.
    limit_tokens       REAL NOT NULL,
    sample_count       INTEGER NOT NULL,
    -- How many times this row was updated in place while its window was live.
    -- 1 means the estimate never moved after the window opened.
    updates            INTEGER NOT NULL DEFAULT 1,
    first_recorded_at  TEXT NOT NULL,
    updated_at         TEXT NOT NULL,
    PRIMARY KEY (identity, axis, period_end)
);
CREATE INDEX IF NOT EXISTS idx_estimate_periods_axis
    ON limit_estimate_periods(identity, axis, period_end DESC);

-- **WHAT SHARE OF ONE LOGIN IS GIVEN TO THE POOL MANAGER.**
--
-- The operator does not hand the daemon whole accounts. He hands it a SHARE of
-- each one and keeps the remainder to spend by hand:
--
--   "it should be considered like the capacity given to the system. It needs to
--    change the denominator somewhere so that it's measuring pressure against
--    the 95% and really fully trying to exploit the 95%, but not calculating
--    pressure as though there's 100%. Because then if I only gave 10% of an
--    account, well, then there's going to be disproportionate pressure of 10x
--    on that 10% account when really, the amount available to it is only the
--    10% to begin with."
--
-- So this number is a DENOMINATOR, not a ceiling. It divides, and what it
-- divides is `budgets::pool_pressure`. Read the failure case above carefully:
-- the bug it names is what happens when the share is applied as a NUMERATOR
-- (scaling the unused budget up) instead of as the denominator (normalising it
-- against what was actually offered). An untouched 10% account and an untouched
-- 100% account are BOTH exactly on pace, and must both score 1.0.
--
-- KEYED BY THE ACCOUNT NAME, and this is a CORRECTION to the table that
-- shipped on 2026-08-26 keyed on the login fingerprint.
--
-- That table was keyed on the login because `limit_estimate_periods` had been
-- burned by label-keying, and its author reported as a vindication that the
-- operator's `default` label and his Gmail label resolve to one login. He
-- rejected the whole framing (2026-08-27):
--
--   "I do not want default even in the purview of the system. Why does it even
--    see that there is a default? ... And I think it's actually better to key
--    on the account name instead of on the login slot, because the login slot
--    is subject to change."
--
-- Two rulings, and the second is this table's. The LOGIN SLOT IS SUBJECT TO
-- CHANGE — a label re-pointed at a different login is a normal operation
-- (`add-account --force` does exactly that) — so keying the share on the
-- fingerprint means a re-login silently loses or reassigns the share the
-- operator set. The NAME is the stable thing he manages, and the share is a
-- statement about what he is willing to lend the pool, not about a credential.
--
-- The `default`/Gmail collision was never evidence for identity-keying either.
-- It was evidence that `default` should not have been visible to the pool at
-- all, which is the first ruling: `default` is not a pool member, so no row
-- here can ever be about it.
--
-- AN ABSENT ROW MEANS 1.0 — the whole account is offered, which is exactly the
-- behaviour every account had before shares existed. No backfill, no shim, no
-- default row to seed. `pool.json` may also DECLARE a share; the row here is
-- the operator's runtime override and WINS. `budgets::pool_share` is the single
-- place that resolves the two.
--
-- THE SHARE BOUNDARY HAS TWO EFFECTS, ruled 2026-08-27, and this is the change
-- from yesterday. It is still a DENOMINATOR — it divides `pool_pressure`, and
-- overage past it still reads NEGATIVE rather than clamping, so how far past it
-- went stays visible. It is ALSO a VETO on new placement:
--
--   "if all the other accounts are blocked, we don't want that account to even
--    be available for placement, right? So overage we've determined is okay
--    while the current workload is finishing. But then it needs to be like hard
--    clamped — not available for placement as well. So it needs to have an
--    effect on pressure, but it also needs to have an effect on whether
--    placement is possible."
--
-- The asymmetry is exact and is implemented in `budgets::admit`: work already
-- running on an over-share account runs to completion and a thread with history
-- there is not moved; what stops is NEW placement onto it. Same shape as a dead
-- login (`AdmissionRequest::unhealthy`) — removed from the TARGET set, left in
-- the CLEARED set. This is NOT the per-account threshold veto the operator
-- struck ("I don't know why you're going to need a per account veto"); that was
-- about thresholds, and there is still no per-account threshold here.
CREATE TABLE IF NOT EXISTS pool_shares (
    -- THE ACCOUNT NAME — `pool.json`'s `name`, the label the operator manages.
    -- Stable across a re-login, which is the entire reason it is the key.
    account    TEXT PRIMARY KEY,
    -- The fraction of this resource offered to the pool manager, in (0.0, 1.0].
    -- A FRACTION, never a token count and never a percentage point, because
    -- "percentages are the only thing that's comparable" and a token count
    -- would need a denominator this table does not have.
    --
    -- ZERO IS REFUSED AGAIN, AND THAT IS THE 2026-09-20 CORRECTION.
    --
    -- Between 2026-09-08 and 2026-09-20 a stored `0.0` here WAS the disable:
    -- one column answered both "is this account in the pool" and "how much of
    -- it may the pool use". The operator separated them:
    --
    --   "I want disabled to be independent of share. Whether to include it in
    --    the pool supersedes how much should be made available to the pool."
    --
    -- Inclusion now lives in `account_disabled` (its own table, its own row,
    -- its own instant). This column is once more a pure QUANTITY: what he lends
    -- the pool, which does not change because he took the account out of it and
    -- does not have to be reconstructed when he puts it back. That is what
    -- retired `prior_share` below.
    share      REAL NOT NULL,
    -- Free text for why this share is what it is, so a number set by hand a
    -- month ago is still explicable.
    note       TEXT,
    -- RETIRED 2026-09-20, AND KEPT ONLY SO THE MIGRATION CAN READ IT ONCE.
    --
    -- It held the share a disable DESTROYED, so an enable could put it back.
    -- Nothing destroys a share any more — `account_disabled` carries the
    -- inclusion decision and `share` is untouched by it — so there is nothing
    -- left to remember and no reader. `Store::migrate_share_zero_disable` reads
    -- this column exactly once per database, to recover the shares the old
    -- model overwrote with zero, and `Store::set_pool_share` NULLs it on every
    -- write thereafter. SQLite cannot drop a column without rebuilding the
    -- table, and rebuilding a table to delete a column nobody reads is a
    -- migration risk taken for tidiness; it stays, empty.
    prior_share REAL,
    updated_at TEXT NOT NULL
);

-- ============================================================================
-- IS THIS ACCOUNT IN THE POOL AT ALL — the operator's INCLUSION decision, and
-- deliberately not a number.
-- ============================================================================
--
-- 2026-09-20, in his own words:
--
--   "I want disabled to be independent of share. Whether to include it in the
--    pool supersedes how much should be made available to the pool."
--
-- Two questions, and they are not the same question:
--
--   * IS this account in the pool?      -> this table. A row means NO.
--   * HOW MUCH of it may the pool use?  -> `pool_shares.share`.
--
-- The first supersedes the second: an account with a row here is out of
-- placement selection whatever its share says, and its share keeps whatever
-- value it had while it sits here.
--
-- WHY THIS IS A TABLE AND NOT A COLUMN ON `pool_shares`. A disable must not
-- require a share row to exist, and must not MINT one. `pool_shares` has a
-- meaning beyond its value — the row's presence is what `pool_share_row`
-- reports as `source: "override"`, "the operator said this" as against "the
-- pool declares it" — so disabling an account that has never had a share set
-- would have had to invent an override he never typed, and the interface would
-- have shown him one. Presence-as-state in its own table has no such cost.
--
-- WHY THE PREVIOUS DESIGN WAS WRONG, recorded because it cost something real.
-- Disabled WAS `share == 0`. The capability shipped 2026-09-08 behind a magic
-- value in a number field; four days later the operator's own row for
-- `alice-example-com` still read `1.0e-05` — the 0.001% he had tried BEFORE
-- the feature existed — because reaching the capability meant knowing that a
-- particular number in a quantity field was a different act. A capability that
-- is live for four days and never found is a capability with no door.
CREATE TABLE IF NOT EXISTS account_disabled (
    -- THE ACCOUNT NAME, same key as `pool_shares` and for the same reason: the
    -- label is the stable thing the operator manages, and a login slot is not.
    account     TEXT PRIMARY KEY,
    -- PRESENCE IS THE STATE. There is no boolean column here on purpose: a
    -- `disabled INTEGER` would admit the row `(account, 0)`, which reads as
    -- "disabled: false" and is indistinguishable in every query from "not
    -- disabled" except by being a row. Enabling DELETES.
    --
    -- The instant is for the record, so a disable set weeks ago can still be
    -- dated. Nothing reads it to decide anything and nothing lifts a disable on
    -- a timer — this state never expires.
    disabled_at TEXT NOT NULL,
    -- The operator's own words about WHY, optional, absent means absent.
    note        TEXT
);

-- A turn that was KILLED mid-flight by a budget trip (DEC-13) with its prompt
-- preserved verbatim. CON-4: a trip parks the turn and NEVER discards the
-- operator's prompt. The resume watcher re-dispatches this row the moment the
-- thread becomes admissible again.
CREATE TABLE IF NOT EXISTS parked_turns (
    thread_id        TEXT PRIMARY KEY,
    prompt           TEXT NOT NULL,
    message_ids_json TEXT NOT NULL,
    attempt          INTEGER NOT NULL DEFAULT 1,
    reason           TEXT NOT NULL,
    budget_key       TEXT,
    parked_at        TEXT NOT NULL
);

-- Which accounts a thread has actually ESTABLISHED A CONVERSATION on (round-7
-- item 7: park for continuity). An account switch restarts the conversation
-- unless a session for this thread id already exists on the target account, so
-- the admission gate needs to know where a thread's history lives: it parks
-- rather than switching when no admissible account carries its history.
CREATE TABLE IF NOT EXISTS thread_accounts (
    thread_id  TEXT NOT NULL,
    account    TEXT NOT NULL,
    first_seen TEXT NOT NULL,
    last_seen  TEXT NOT NULL,
    PRIMARY KEY (thread_id, account)
);

-- THE THREAD EXECUTION LOCK (operator msg #1213: "It is impossible to run the
-- same thread two times. Impossible.").
--
-- `thread_id` is the PRIMARY KEY, so the INSERT that claims a thread IS the
-- mutual exclusion — a second claimant gets a UNIQUE violation from SQLite,
-- not a courtesy check it could skip. Nothing in the daemon starts an
-- execution without holding this row: dispatch, parked-turn redelivery, retry
-- fire and boot re-adoption all go through one acquire.
--
-- `boot_id` names the daemon PROCESS that holds it and `daemon_pid` is that
-- process's pid, which is what makes a crashed holder RECLAIMABLE rather than
-- a permanent deadlock: a claim whose daemon_pid is dead is stolen with a
-- boot_id-guarded UPDATE (compare-and-swap); a claim whose daemon_pid is
-- ALIVE is never stolen, ever — the second claimant waits.
CREATE TABLE IF NOT EXISTS thread_locks (
    thread_id   TEXT PRIMARY KEY,
    boot_id     TEXT NOT NULL,
    daemon_pid  INTEGER NOT NULL,
    -- Which entry point claimed it (dispatch | parked | retry | adopt) —
    -- diagnostic only, and what a refusal names in its log line.
    entry       TEXT NOT NULL,
    acquired_at TEXT NOT NULL
);

-- How far a client has been shown (msg #1213, second half: "status isn't
-- making it whatsoever here to AUI"). `last_seen_seq` is the ledger seq the
-- operator has actually heard through; `threads.status` reports the unread
-- count over it and `threads.seen` advances it.
CREATE TABLE IF NOT EXISTS thread_seen (
    thread_id     TEXT PRIMARY KEY,
    last_seen_seq INTEGER NOT NULL DEFAULT 0,
    updated_at    TEXT NOT NULL
);

-- DEC-56: THE INTERRUPTED FLAG AND ITS TIMESTAMP. One row per thread that a
-- HARD break cut off and that has not yet been told about it; the row is
-- deleted the moment the resume notice is actually delivered, so its presence
-- is exactly "this thread is owed an account of an interruption".
--
-- Note what is NOT here: nothing about when a limit resets. CON-22 — a model
-- is told WHICH gate stopped it and WHEN, never when its capacity returns.
-- The reset instant lives on the account row, where the model cannot reach it.
CREATE TABLE IF NOT EXISTS thread_interrupts (
    thread_id      TEXT PRIMARY KEY,
    -- session_limit | weekly_limit | fable_limit | threshold | hard_budget
    trigger_kind   TEXT NOT NULL,
    -- One short factual clause naming the specific budget or threshold.
    detail         TEXT NOT NULL DEFAULT '',
    -- DEC-55's flag: did a switchover actually happen.
    switched       INTEGER NOT NULL DEFAULT 0,
    from_account   TEXT,
    to_account     TEXT,
    interrupted_at TEXT NOT NULL
);

-- THE NOTICE LOG: what has already been SAID OUT LOUD, and when.
--
-- The operator rejected a ticket-filing alarm for expired logins:
--
--   "I don't really need an alarm... Sounds like it's just going to nag me,
--    and I don't want to be nagged. Maybe just, like, if there's an attempt to
--    place on a thread, you can notify me in the audio user interface, but I
--    don't want to be notified more than once per day."
--
-- So the rate limit lives HERE, in threads, and not in the client. Two reasons
-- it is durable rather than an in-memory map:
--
--   * a daemon restart must not re-open the window. An expired login is a state
--    only he can leave, so it persists across restarts by definition, and an
--    in-memory limiter would speak again on every boot — which for a daemon
--    that restarts itself on every new binary is a nagging machine.
--   * more than one client may be listening. A limit held per-connection is
--    not a limit at all.
--
-- Keyed on (kind, subject) so the daily budget is PER LOGIN: two expired
-- accounts are two different things to fix and he is told about both.
CREATE TABLE IF NOT EXISTS notice_log (
    kind     TEXT NOT NULL,
    subject  TEXT NOT NULL,
    -- RFC3339 UTC of the last time this notice was actually emitted.
    last_at  TEXT NOT NULL,
    -- How many times it has been SUPPRESSED since then, so the one notice that
    -- does get through can say "and it has come up 40 times since yesterday"
    -- rather than understating a persistent problem as a single event.
    suppressed INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (kind, subject)
);

-- ─────────────────────────────────────────────────────────────────────────────
-- Messaging networks (R4.S3; `docs/agent-networks.md`). Rules live in
-- `src/networks.rs`; these are rows only (`src/store/networks.rs`).
-- Membership is a mutable projection: every act that changes these rows is
-- ALSO a ledger event on the acting thread, so history is reconstructible from
-- `events` even though these tables are not append-only.
-- ─────────────────────────────────────────────────────────────────────────────

-- Lineage is NOT here: `parent_id` has ONE owner, R4.S2's `ThreadMeta` in the
-- managed ledgers (`src/managed.rs`), written once at a spawn's commit point.
-- Ancestors, subtree and every home network are derived from it.

-- One row per CREATED network, plus a row `home:<thread_id>` only once a home's
-- rules or flags have been configured (a home itself is derived, never stored).
CREATE TABLE IF NOT EXISTS networks (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    created_by  TEXT NOT NULL,        -- provenance only; confers nothing (N6)
    rules_json  TEXT NOT NULL,        -- {max_chars, history}
    created_at  TEXT NOT NULL
);

-- Per-member flags (agent-networks §1.2). `state` is `member` or `pending` (an
-- offer, §1.3). Under a `home:<t>` id a row is a flag OVERRIDE, never a
-- membership: the home's membership is the subtree.
CREATE TABLE IF NOT EXISTS network_members (
    network_id  TEXT NOT NULL,
    thread_id   TEXT NOT NULL,
    flags_json  TEXT NOT NULL,
    state       TEXT NOT NULL,
    joined_by   TEXT NOT NULL,
    joined_at   TEXT NOT NULL,
    PRIMARY KEY (network_id, thread_id)
);
CREATE INDEX IF NOT EXISTS idx_network_members_thread ON network_members(thread_id);

-- CORRESPONDENCE (§1.5): `holder` may address `target` over `network_id`
-- because `target` delivered mail to `holder` there. A route, never a roster
-- row, never authority, never a read.
CREATE TABLE IF NOT EXISTS network_routes (
    network_id  TEXT NOT NULL,
    holder      TEXT NOT NULL,
    target      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (network_id, holder, target)
);

-- Broadcast bodies, kept for `history: true` (§3.4): a joiner receives them.
CREATE TABLE IF NOT EXISTS network_broadcasts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    network_id  TEXT NOT NULL,
    sender      TEXT NOT NULL,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

-- Mail to a DORMANT member that is not `wakeable` to its sender: accepted and
-- durable, released into the recipient's queue ahead of the next mail that
-- does wake it, or by `networks::release_held` at a turn start (§4.2).
CREATE TABLE IF NOT EXISTS network_held_mail (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    recipient      TEXT NOT NULL,
    network_id     TEXT NOT NULL,
    sender         TEXT NOT NULL,
    text           TEXT NOT NULL,
    metadata_json  TEXT NOT NULL,
    created_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_network_held_mail_recipient ON network_held_mail(recipient);
