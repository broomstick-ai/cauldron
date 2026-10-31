//! Ledger unit tests: append/replay with timestamps, kind filters, message-id
//! retrieval (G20), queue lifecycle (Q4), persisted retries (Q5/G10), running
//! turns (G1), usage ledger (Q11), and import watermarks (Q8).

use serde_json::json;
use threads::event::{SendIntent, StoredEvent, ThreadEvent};
use threads::store::{now_rfc3339, BudgetRow, RetryRow, RunningTurnRow, Store, ThreadRow};

fn thread_row(id: &str) -> ThreadRow {
    ThreadRow {
        id: id.into(),
        account: None,
        cwd: Some("/tmp".into()),
        model: Some("sonnet".into()),
        settings_json: "{}".into(),
        last_seq: 0,
        next_message_id: 1,
        last_compaction_message_id: 0,
        needs_restate: false,
        created_at: "2026-07-27T00:00:00Z".into(),
        updated_at: "2026-07-27T00:00:00Z".into(),
    }
}

fn ev(thread: &str, seq: u64, message_id: Option<u64>, event: ThreadEvent) -> StoredEvent {
    StoredEvent {
        thread_id: thread.into(),
        seq,
        message_id,
        ts: format!("2026-07-27T00:00:{:02}Z", seq),
        event,
    }
}

fn user(text: &str) -> ThreadEvent {
    ThreadEvent::UserMessage {
        text: text.into(),
        intent: SendIntent::Immediate,
        metadata: json!({"wav_path": "/captures/a.wav"}),
    }
}

fn assistant(text: &str) -> ThreadEvent {
    ThreadEvent::AssistantText {
        text: text.into(),
        parent_tool_use_id: None,
        api_message_id: Some("msg_1".into()),
    }
}

#[test]
fn history_returns_timestamps_and_filters_kinds() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_thread(&thread_row("t1")).unwrap();
    s.append_event(&ev("t1", 1, Some(0), user("hello"))).unwrap();
    s.append_event(&ev("t1", 2, None, ThreadEvent::TurnStarted { attempt: 1, message_ids: vec![0] }))
        .unwrap();
    s.append_event(&ev("t1", 3, Some(1), assistant("hi"))).unwrap();

    // G5: timestamps come back on replay.
    let all = s.list_events("t1", 0, 100, None).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].ts, "2026-07-27T00:00:01Z");
    assert_eq!(all[0].message_id, Some(0));

    // after_seq is exclusive (CCW-compatible cursor).
    let after = s.list_events("t1", 1, 100, None).unwrap();
    assert_eq!(after.first().unwrap().seq, 2);

    // Kind filter: only messages.
    let kinds = vec!["user_message".to_string(), "assistant_text".to_string()];
    let msgs = s.list_events("t1", 0, 100, Some(&kinds)).unwrap();
    assert_eq!(msgs.len(), 2);

    // Idempotent append: same (thread, seq) is a no-op.
    s.append_event(&ev("t1", 1, Some(0), user("DUPLICATE"))).unwrap();
    let all2 = s.list_events("t1", 0, 100, None).unwrap();
    assert_eq!(all2.len(), 3);
    match &all2[0].event {
        ThreadEvent::UserMessage { text, metadata, .. } => {
            assert_eq!(text, "hello"); // original preserved
            assert_eq!(metadata["wav_path"], "/captures/a.wav"); // G18 metadata round-trips
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn message_id_retrieval_modes() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_thread(&thread_row("t2")).unwrap();
    for i in 0..10u64 {
        let e = if i % 2 == 0 { user(&format!("u{i}")) } else { assistant(&format!("a{i}")) };
        s.append_event(&ev("t2", i + 1, Some(i), e)).unwrap();
    }
    // Non-addressable rows must never surface in message queries.
    s.append_event(&ev("t2", 11, None, ThreadEvent::TurnEnded { exit_code: Some(0) }))
        .unwrap();

    // `--ids` (`Store::events_by_message_ids`) was tested here. It went with
    // `threads/{id}/messages`, its only caller, at R2.S3a.

    // --from/--to with kind filter
    let kinds = vec!["user_message".to_string()];
    let got = s
        .events_by_range("t2", Some(3), Some(8), Some(&kinds), None)
        .unwrap();
    let ids: Vec<u64> = got.iter().filter_map(|e| e.message_id).collect();
    assert_eq!(ids, vec![4, 6, 8]);

    // --tail
    let got = s.events_by_range("t2", None, None, None, Some(3)).unwrap();
    let ids: Vec<u64> = got.iter().filter_map(|e| e.message_id).collect();
    assert_eq!(ids, vec![7, 8, 9]);

    assert_eq!(s.max_message_id("t2").unwrap(), 9);
    assert_eq!(s.max_seq("t2").unwrap(), 11);
}

#[test]
fn queue_lifecycle() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_thread(&thread_row("t3")).unwrap();
    s.enqueue("t3", 0, "first", "immediate", &serde_json::Value::Null, &[]).unwrap();
    s.enqueue("t3", 1, "second", "turn_end", &serde_json::Value::Null, &[]).unwrap();
    s.enqueue("t3", 2, "third", "immediate_bare", &serde_json::Value::Null, &[]).unwrap();

    let waiting = s.waiting_queue("t3").unwrap();
    assert_eq!(waiting.len(), 3);
    assert_eq!(waiting[0].text, "first");
    assert_eq!(waiting[1].intent, "turn_end");
    assert_eq!(waiting[2].intent, "immediate_bare");

    assert_eq!(s.threads_with_waiting_queue().unwrap(), vec!["t3".to_string()]);

    // Cancel by message id (the AUI queued-menu affordance).
    assert!(s.cancel_queued("t3", 1).unwrap());
    assert!(!s.cancel_queued("t3", 1).unwrap()); // already gone

    let ids: Vec<i64> = s.waiting_queue("t3").unwrap().iter().map(|q| q.id).collect();
    s.mark_delivered(&ids).unwrap();
    assert!(s.waiting_queue("t3").unwrap().is_empty());
}

#[test]
fn retry_rows_survive_and_clear() {
    let s = Store::open_in_memory().unwrap();
    let row = RetryRow {
        thread_id: "t4".into(),
        prompt: "[#3 2026-07-27 10:00:00]\nplease".into(),
        message_ids: vec![3],
        attempt: 2,
        kind: "usage_limit".into(),
        reason: "rate limit (429)".into(),
        account: Some("acct-a".into()),
        created_at: String::new(),
    };
    s.upsert_retry(&row).unwrap();
    let got = s.get_retry("t4").unwrap().unwrap();
    assert_eq!(got.prompt, row.prompt);
    assert_eq!(got.message_ids, vec![3]);
    assert_eq!(got.attempt, 2);

    // Upsert replaces (one pending retry per thread).
    s.upsert_retry(&RetryRow { attempt: 3, ..row.clone() }).unwrap();
    assert_eq!(s.get_retry("t4").unwrap().unwrap().attempt, 3);
    assert_eq!(s.list_retries().unwrap().len(), 1);

    s.clear_retry("t4").unwrap();
    assert!(s.get_retry("t4").unwrap().is_none());
}

/// `running_turns` round-trips, and the thread row carries a `last_seq`
/// WATERMARK and nothing else.
///
/// This test used to be `running_turns_and_busy_sweep` and also exercised
/// `sweep_stale_busy` — the boot-time repair for the `threads.status` column.
/// Both the column and the sweep are deleted (`store::DROPPED_COLUMNS`): the
/// answer is derived on demand by `ThreadManager::thread_states`, so there is
/// no cache to go stale and nothing for a boot sweep to repair. The half of the
/// old write that was real state — the watermark — is asserted here;
/// `the_status_column_is_gone` below asserts the other half stayed dead.
#[test]
fn running_turns_and_the_last_seq_watermark() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_thread(&thread_row("t5")).unwrap();
    s.bump_last_seq("t5", 7, None).unwrap();
    assert_eq!(s.get_thread("t5").unwrap().unwrap().last_seq, 7);

    // ── AND THE RECENCY STAMP IS A PASSENGER THAT ONLY A USER MESSAGE CARRIES ─
    //
    // `last_user_input_at` rides this same statement, written only when the
    // append that called it was a `user_message` — *"ordered on last user input,
    // not... any other events, including renames"*. Both directions are asserted,
    // and the `None` one is the load-bearing half: the statement rewrites the
    // column on EVERY append (one statement rather than two, for a write that
    // runs 2.46 M times), so a `CASE WHEN` whose sense was backwards would quietly
    // blank the stamp on the next non-user event.
    assert_eq!(
        s.last_user_input_at("t5").unwrap(),
        None,
        "a non-user append must not invent a recency stamp"
    );
    s.bump_last_seq("t5", 8, Some("2026-10-05T01:02:03Z")).unwrap();
    assert_eq!(
        s.last_user_input_at("t5").unwrap().as_deref(),
        Some("2026-10-05T01:02:03Z"),
        "a `user_message` append writes it"
    );
    s.bump_last_seq("t5", 9, None).unwrap();
    assert_eq!(
        s.last_user_input_at("t5").unwrap().as_deref(),
        Some("2026-10-05T01:02:03Z"),
        "and the next NON-user append leaves it exactly where it was — this is the \
         assertion a one-statement write can get backwards"
    );
    assert_eq!(s.get_thread("t5").unwrap().unwrap().last_seq, 9);

    s.upsert_running_turn(&RunningTurnRow {
        thread_id: "t5".into(),
        pid: 4242,
        spawn_cookie: "cookie".into(),
        output_path: "/tmp/x.jsonl".into(),
        byte_offset: 0,
        started_at: "2026-07-27T00:00:00Z".into(),
        prompt: None,
        message_ids: vec![],
        attempt: 1,
    })
    .unwrap();
    s.update_turn_offset("t5", 512).unwrap();
    let rows = s.list_running_turns().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].byte_offset, 512);

    s.clear_running_turn("t5").unwrap();
    assert!(s.list_running_turns().unwrap().is_empty());
}

/// **THE COLUMN STAYS DELETED.**
///
/// `threads.status` was a cached copy of an answer nine other places derived,
/// and it reported two completed threads as `busy` for twenty-five hours. It is
/// gone from `schema.sql` and dropped from existing databases by
/// `store::DROPPED_COLUMNS`. This asserts the schema a fresh open produces, so
/// that re-adding the column — by hand, or by a merge that resurrects the old
/// CREATE TABLE — fails here rather than quietly reintroducing a second source
/// of truth.
#[test]
fn the_status_column_is_gone() {
    let s = Store::open_in_memory().unwrap();
    // A plain read of the column must be a hard SQL error, not NULL.
    assert!(
        s.execute_sql("SELECT status FROM threads").is_err(),
        "the `threads.status` cache is back — see store::DROPPED_COLUMNS for \
         why it must not be, and batch the derivation instead"
    );
    // And the columns that ARE real state still read.
    s.execute_sql("SELECT id, last_seq, account FROM threads").unwrap();
}

/// A database created BEFORE the column was deleted opens cleanly and comes out
/// the other side without it — the `DROP COLUMN` migration actually runs.
///
/// The reverse direction is what makes this worth a test: `apply_schema`
/// swallows "no such column" so that a fresh database is not an error, and a
/// swallow that is too broad would also swallow a migration that never fired.
#[test]
fn an_old_database_loses_its_status_column_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    {
        // Hand-build the pre-change table, status column and all.
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE threads (
                 id TEXT PRIMARY KEY,
                 status TEXT NOT NULL DEFAULT 'idle',
                 account TEXT, cwd TEXT, model TEXT,
                 settings_json TEXT NOT NULL,
                 last_seq INTEGER NOT NULL DEFAULT 0,
                 next_message_id INTEGER NOT NULL DEFAULT 1,
                 last_compaction_message_id INTEGER NOT NULL DEFAULT 0,
                 needs_restate INTEGER NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             INSERT INTO threads (id, status, settings_json, created_at, updated_at)
             VALUES ('legacy', 'busy', '{}', 'then', 'then');",
        )
        .unwrap();
    }
    let s = Store::open(&path).unwrap();
    assert!(
        s.execute_sql("SELECT status FROM threads").is_err(),
        "opening an old database left the stale `busy` cache in place"
    );
    // The ROW survived the drop — this is a column removal, not a data purge.
    assert!(s.get_thread("legacy").unwrap().is_some());
}

#[test]
fn usage_ledger_from_model_usage() {
    let s = Store::open_in_memory().unwrap();
    let mu = json!({
        "claude-fable-5": {"inputTokens": 100, "outputTokens": 50, "cacheReadInputTokens": 10, "cacheCreationInputTokens": 5, "costUSD": 0.25},
        "claude-haiku-4-5": {"inputTokens": 10, "outputTokens": 5, "costUSD": 0.01},
    });
    assert_eq!(
        s.record_turn_usage("t6", Some("default"), &mu, None)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        s.record_turn_usage("t7", Some("work"), &mu, None)
            .unwrap()
            .len(),
        2
    );

    let by_thread = s.spend_summary(false).unwrap();
    assert_eq!(by_thread.len(), 2);
    let t6 = by_thread.iter().find(|r| r.key == "t6").unwrap();
    assert_eq!(t6.total_tokens, 100 + 50 + 10 + 5 + 10 + 5);
    assert!((t6.cost_usd - 0.26).abs() < 1e-9);

    let by_account = s.spend_summary(true).unwrap();
    assert!(by_account.iter().any(|r| r.key == "work"));
}

/// A `result_turn`'s `modelUsage` is the CUMULATIVE per-model total for the
/// whole claude process, and a turn with subagents emits one result per
/// subagent — so the same running total arrives many times over.
///
/// This is the shape measured on the live store on 2026-07-31: one turn
/// recorded SEVENTEEN times, `ledger.query` reading $2,845 for a thread whose
/// true spend was about $199. Recording each snapshot verbatim is what did it.
#[test]
fn a_repeated_cumulative_snapshot_is_booked_exactly_once() {
    let s = Store::open_in_memory().unwrap();
    let snapshot = json!({
        "claude-fable-5": {"inputTokens": 4000, "outputTokens": 800,
                           "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0,
                           "costUSD": 4.0},
    });

    // Seventeen results, one execution, one identical payload — the live shape.
    let mut booked = 0usize;
    for _ in 0..17 {
        booked += s
            .record_turn_usage("t-dup", Some("default"), &snapshot, Some("2026-07-31T01:56:10Z"))
            .unwrap()
            .len();
    }
    assert_eq!(booked, 1, "only the first snapshot moves the ledger");

    let row = s.spend_summary(false).unwrap();
    let t = row.iter().find(|r| r.key == "t-dup").unwrap();
    assert_eq!(t.total_tokens, 4800, "the turn is worth 4,800 tokens, once");
    assert!(
        (t.cost_usd - 4.0).abs() < 1e-9,
        "the turn is worth $4.00, once — got {}",
        t.cost_usd
    );
    // …and one EXECUTION, not seventeen. `turns` counting rows was half of why
    // the inflated number looked plausible.
    assert_eq!(t.turns, 1);
}

/// The other half of the semantics: within ONE execution the cumulative total
/// GROWS between bursts of results. Recording only the final result would drop
/// everything the earlier bursts had already added — so the correct shape is a
/// delta, and the deltas must SUM to the last snapshot.
#[test]
fn deltas_of_one_execution_sum_to_the_final_cumulative_total() {
    let s = Store::open_in_memory().unwrap();
    let epoch = Some("2026-07-31T02:42:47Z");
    let cum = |input: u64, cost: f64| {
        json!({"claude-fable-5": {"inputTokens": input, "outputTokens": 0,
                                  "cacheReadInputTokens": 0,
                                  "cacheCreationInputTokens": 0, "costUSD": cost}})
    };

    let a = s.record_turn_usage("t-grow", Some("default"), &cum(1_000, 1.0), epoch).unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].tokens, 1_000);
    // A repeat of the same snapshot adds nothing…
    assert!(s
        .record_turn_usage("t-grow", Some("default"), &cum(1_000, 1.0), epoch)
        .unwrap()
        .is_empty());
    // …and the grown snapshot adds only what grew.
    let b = s.record_turn_usage("t-grow", Some("default"), &cum(4_000, 4.0), epoch).unwrap();
    assert_eq!(b[0].tokens, 3_000, "only the increment");
    assert!((b[0].cost_usd - 3.0).abs() < 1e-9);

    let t = s.spend_summary(false).unwrap();
    let t = t.iter().find(|r| r.key == "t-grow").unwrap();
    assert_eq!(t.total_tokens, 4_000, "the ledger sums to the final total");
    assert!((t.cost_usd - 4.0).abs() < 1e-9);
}

/// Results can arrive out of order, and an execution can DIE between them.
/// Neither may corrupt the ledger: a stale snapshot contributes zero (never a
/// negative row, never a second copy), and the NEXT execution — whose counters
/// restart at zero — is booked in full rather than swallowed by the dead one's
/// high-water mark.
#[test]
fn an_out_of_order_result_adds_nothing_and_a_new_execution_starts_fresh() {
    let s = Store::open_in_memory().unwrap();
    let cum = |input: u64, cost: f64| {
        json!({"claude-fable-5": {"inputTokens": input, "outputTokens": 0,
                                  "cacheReadInputTokens": 0,
                                  "cacheCreationInputTokens": 0, "costUSD": cost}})
    };
    let first = Some("2026-07-31T01:00:00Z");

    s.record_turn_usage("t-oo", Some("default"), &cum(5_000, 5.0), first).unwrap();
    // A LATE, smaller snapshot from earlier in the same execution.
    let late = s.record_turn_usage("t-oo", Some("default"), &cum(2_000, 2.0), first).unwrap();
    assert!(late.is_empty(), "a stale snapshot must not hand spend back");
    // An EMPTY modelUsage — what a torn-down turn really emits — is "no
    // information", not "the turn cost nothing".
    assert!(s
        .record_turn_usage("t-oo", Some("default"), &json!({}), first)
        .unwrap()
        .is_empty());

    // The turn died there. The retry is a NEW claude process: its counters
    // start at zero, and its spend is real.
    let second = Some("2026-07-31T01:05:00Z");
    let retry = s.record_turn_usage("t-oo", Some("default"), &cum(1_500, 1.5), second).unwrap();
    assert_eq!(retry[0].tokens, 1_500, "a new execution is booked in full");

    let t = s.spend_summary(false).unwrap();
    let t = t.iter().find(|r| r.key == "t-oo").unwrap();
    assert_eq!(t.total_tokens, 6_500);
    assert!((t.cost_usd - 6.5).abs() < 1e-9);
    assert_eq!(t.turns, 2, "two executions");
}

/// Pre-fix rows are MARKED, not rewritten. They stay on disk as the evidence
/// they are, they are named in every query that touches them, and they are kept
/// out of every total — including the calibration pair, where an inflated token
/// count estimates the account limit too high and a percentage budget then
/// fails to trip at all.
#[test]
fn pre_fix_rows_are_excluded_from_totals_and_reported_beside_them() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.db");

    // A database from the pre-fix daemon: the column does not exist yet, and
    // the rows in it are cumulative snapshots recorded once per result.
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE usage_ledger (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 thread_id TEXT NOT NULL, account TEXT, model TEXT NOT NULL,
                 input_tokens INTEGER NOT NULL DEFAULT 0,
                 output_tokens INTEGER NOT NULL DEFAULT 0,
                 cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                 cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                 cost_usd REAL NOT NULL DEFAULT 0,
                 ts TEXT NOT NULL, started_at TEXT);",
        )
        .unwrap();
        for _ in 0..17 {
            c.execute(
                "INSERT INTO usage_ledger
                    (thread_id, account, model, input_tokens, cost_usd, ts, started_at)
                 VALUES ('old', 'default', 'claude-opus-5', 4000, 146.7365,
                         '2026-07-31T02:42:39Z', '2026-07-31T01:56:10Z')",
                [],
            )
            .unwrap();
        }
    }

    let s = Store::open(&path).unwrap();
    // The new turn writes a delta row through the fixed path.
    s.record_turn_usage(
        "new",
        Some("default"),
        &json!({"claude-opus-5": {"inputTokens": 100, "outputTokens": 0,
                                  "cacheReadInputTokens": 0,
                                  "cacheCreationInputTokens": 0, "costUSD": 0.5}}),
        Some("2026-07-31T18:11:22Z"),
    )
    .unwrap();

    let by_account = s.spend_summary(true).unwrap();
    let a = by_account.iter().find(|r| r.key == "default").unwrap();
    assert!(
        (a.cost_usd - 0.5).abs() < 1e-9,
        "the total counts only trustworthy rows, got {}",
        a.cost_usd
    );
    assert_eq!(a.legacy_rows, 17, "and says how many it did not count");
    assert!(
        (a.legacy_cost_usd - 17.0 * 146.7365).abs() < 1e-3,
        "and what those rows claim, so the operator can see the scale"
    );
    // The old rows are still there — marked, not deleted.
    let kept: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM usage_ledger", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, 18);

    // Calibration sees only the trustworthy row. An inflated token count would
    // estimate the account's limit high, which is the direction where a
    // percentage budget silently never trips.
    let tokens = s
        .account_tokens_between("default", "2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z", None)
        .unwrap();
    assert_eq!(tokens, 100);
    let intervals = s
        .execution_intervals("default", "2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z")
        .unwrap();
    assert_eq!(intervals.len(), 1, "no interval is built from a pre-fix row");
}

#[test]
fn import_watermarks_roundtrip() {
    let s = Store::open_in_memory().unwrap();
    assert_eq!(s.get_watermark("proj|abc").unwrap(), None);
    s.set_watermark("proj|abc", "sess-1", 42).unwrap();
    assert_eq!(s.get_watermark("proj|abc").unwrap(), Some(42));
    s.set_watermark("proj|abc", "sess-1", 50).unwrap();
    assert_eq!(s.get_watermark("proj|abc").unwrap(), Some(50));
}

#[test]
fn a_pre_round_seven_database_is_migrated_in_place() {
    // The budgets table gained three columns in round 7 (fable_pct, cash_usd,
    // accounts_json). `CREATE TABLE IF NOT EXISTS` cannot add a column, so the
    // store ships them twice — in the CREATE for fresh databases and as
    // idempotent ALTERs for existing ones. An operator's live `threads.db`
    // takes the second path, so it is the one that has to be proven.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.db");

    // Stand up a database with the OLD budgets shape and a row already in it.
    let store = Store::open(&path).unwrap();
    store
        .execute_sql(
            "DROP TABLE budgets;
             CREATE TABLE budgets (
                key TEXT PRIMARY KEY, parent_key TEXT, session_pct REAL,
                weekly_pct REAL, on_trip TEXT NOT NULL DEFAULT 'kill',
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             INSERT INTO budgets (key, session_pct, on_trip, created_at, updated_at)
                VALUES ('legacy', 20.0, 'kill', '2026-07-27T00:00:00Z', '2026-07-27T00:00:00Z');",
        )
        .unwrap();
    drop(store);

    // Re-opening migrates it, twice over — the second open must be a no-op.
    let store = Store::open(&path).unwrap();
    let store2 = Store::open(&path).unwrap();
    drop(store2);

    let row = store.get_budget("legacy").unwrap().expect("the row survived");
    assert_eq!(row.session_pct, Some(20.0));
    // An existing key keeps its EXPLICIT kill setting: the new default-finish
    // applies to keys created from now on, it does not rewrite history.
    assert_eq!(row.on_trip, "kill");
    // The new columns exist and default to "nothing configured" — crucially,
    // an un-migrated key is scoped to NO accounts rather than silently to all.
    assert_eq!(row.fable_pct, None);
    assert_eq!(row.cash_usd, None);
    assert!(row.accounts.is_empty());
    // Q-34's column arrived the same way, and an existing key is of course
    // LIVE, not disabled — a migration must never quietly deny a key.
    assert_eq!(row.disabled_at, None);

    // And the migrated table is writable on the new shape.
    let mut updated = row.clone();
    updated.accounts = vec!["default".into()];
    updated.cash_usd = Some(20.0);
    store.upsert_budget(&updated).unwrap();
    let back = store.get_budget("legacy").unwrap().unwrap();
    assert_eq!(back.accounts, vec!["default".to_string()]);
    assert_eq!(back.cash_usd, Some(20.0));
}

#[test]
fn a_budget_key_is_disabled_and_re_enabled_in_place_and_never_deleted() {
    // Q-34 (msg #751), at the storage layer: disabling touches ONE column, so
    // every row of accounting the key carries stays exactly where it was. There
    // is no delete on this table at all — the store has no method for it.
    let store = Store::open_in_memory().unwrap();
    store
        .upsert_budget(&BudgetRow {
            key: "k".into(),
            parent_key: None,
            session_pct: Some(20.0),
            weekly_pct: None,
            fable_pct: None,
            cash_usd: Some(5.0),
            accounts: vec!["default".into()],
            on_trip: "finish".into(),
            disabled_at: None,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
        })
        .unwrap();
    store
        .record_charge(&threads::store::ChargeRow {
            budget_key: "k".into(),
            thread_id: "t1".into(),
            account: "default".into(),
            axis: "session".into(),
            tokens: 1_000,
            cost_usd: 0.25,
            ts: now_rfc3339(),
        })
        .unwrap();

    assert!(store.set_budget_disabled("k", Some("2026-07-29T00:00:00Z")).unwrap());
    let row = store.get_budget("k").unwrap().unwrap();
    assert_eq!(row.disabled_at.as_deref(), Some("2026-07-29T00:00:00Z"));
    // Nothing else moved, and the ledger is untouched.
    assert_eq!(row.session_pct, Some(20.0));
    assert_eq!(row.cash_usd, Some(5.0));
    assert_eq!(row.accounts, vec!["default".to_string()]);
    assert_eq!(store.charge_accounts("k").unwrap(), vec!["default"]);
    assert_eq!(
        store.charged_tokens("k", "session", "default", "2000-01-01T00:00:00Z").unwrap(),
        1_000
    );

    // Enabling is the exact inverse, and the key is still listed throughout.
    assert!(store.set_budget_disabled("k", None).unwrap());
    assert_eq!(store.get_budget("k").unwrap().unwrap().disabled_at, None);
    assert_eq!(store.list_budgets().unwrap().len(), 1);
    assert!(store.any_budget_configured().unwrap());
    // A key that does not exist cannot be disabled into existence.
    assert!(!store.set_budget_disabled("nope", Some("now")).unwrap());
}

#[test]
fn thread_account_history_is_recorded_and_deduplicated() {
    // Round-7 item 7's substrate: the gate needs to know where a thread's
    // conversation lives before it can refuse to abandon it.
    let store = Store::open_in_memory().unwrap();
    assert!(store.thread_accounts("t1").unwrap().is_empty());
    store.record_thread_account("t1", "default").unwrap();
    store.record_thread_account("t1", "default").unwrap();
    store.record_thread_account("t1", "second").unwrap();
    let mut got = store.thread_accounts("t1").unwrap();
    got.sort();
    assert_eq!(got, vec!["default".to_string(), "second".into()]);
    assert!(store.thread_accounts("t2").unwrap().is_empty());
}

// ── the shape-mismatch rule: an unreadable row degrades, it does not erase ──
//
// `threads.history` is the single call every recovery path funnels through —
// the AUI's outstanding-turn sweep, the reconnect backfill, and the lag
// recovery a client runs when the daemon's fan-out overflows. It used to
// deserialize each row with `?`, so one payload this build could not read
// failed the whole page and every one of those recoveries came back EMPTY,
// which is indistinguishable from "nothing was missed".

/// The degraded-row counter is process-wide, so the cases that READ it must not
/// run alongside the cases that FEED it. Every test below that inserts an
/// unreadable payload takes this lock first.
static SHAPE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn shape_guard() -> std::sync::MutexGuard<'static, ()> {
    let g = SHAPE.lock().unwrap_or_else(|e| e.into_inner());
    threads::store::reset_unreadable_ledger_rows();
    g
}

/// Write a row straight into the ledger with a payload the crate cannot parse.
fn insert_raw_payload(s: &Store, thread: &str, seq: u64, payload: &str) {
    let esc = payload.replace('\'', "''");
    s.execute_sql(&format!(
        "INSERT INTO events (thread_id, seq, message_id, kind, payload_json, ts)
         VALUES ('{thread}', {seq}, NULL, 'from_the_future', '{esc}',
                 '2026-07-27T00:00:{seq:02}Z')"
    ))
    .unwrap();
}

#[test]
fn one_unreadable_row_does_not_take_the_whole_history_page_with_it() {
    let _guard = shape_guard();
    let s = Store::open_in_memory().unwrap();
    s.upsert_thread(&thread_row("t1")).unwrap();
    s.append_event(&ev("t1", 1, Some(1), user("before"))).unwrap();
    // A kind written by a daemon newer than this binary.
    insert_raw_payload(
        &s,
        "t1",
        2,
        r#"{"kind":"from_the_future","note":"a shape this build predates"}"#,
    );
    s.append_event(&ev("t1", 3, Some(2), assistant("after"))).unwrap();

    // Without the fix this call is an Err and the operator hears nothing.
    let got = s.list_events("t1", 0, 100, None).unwrap();
    assert_eq!(got.len(), 3, "every row must still be served");
    assert_eq!(got[0].seq, 1);
    assert_eq!(got[1].seq, 2);
    assert_eq!(got[2].seq, 3);
    // The events either side of the bad row survive intact — the whole point is
    // that a reply sitting at seq 3 still reaches him.
    assert!(matches!(&got[2].event, ThreadEvent::AssistantText { text, .. } if text == "after"));
}

#[test]
fn an_unreadable_row_keeps_its_payload_verbatim() {
    let _guard = shape_guard();
    let s = Store::open_in_memory().unwrap();
    s.upsert_thread(&thread_row("t1")).unwrap();
    insert_raw_payload(
        &s,
        "t1",
        1,
        r#"{"kind":"from_the_future","detail":"keep me","n":7}"#,
    );
    let got = s.list_events("t1", 0, 100, None).unwrap();
    match &got[0].event {
        ThreadEvent::Unknown { raw } => {
            // Nothing invented, nothing dropped: a later build that understands
            // `from_the_future` can re-read this exact row.
            assert_eq!(raw.get("kind").unwrap(), "from_the_future");
            assert_eq!(raw.get("detail").unwrap(), "keep me");
            assert_eq!(raw.get("n").unwrap(), 7);
        }
        other => panic!("expected the payload kept as `unknown`, got {other:?}"),
    }
}

#[test]
fn a_row_that_is_not_even_json_is_still_kept_and_still_counted() {
    let _guard = shape_guard();
    let s = Store::open_in_memory().unwrap();
    s.upsert_thread(&thread_row("t1")).unwrap();
    insert_raw_payload(&s, "t1", 1, "this is not json at all");
    let got = s.list_events("t1", 0, 100, None).unwrap();
    assert_eq!(got.len(), 1);
    match &got[0].event {
        ThreadEvent::Unknown { raw } => {
            assert_eq!(raw.as_str().unwrap(), "this is not json at all");
        }
        other => panic!("expected the bytes kept as `unknown`, got {other:?}"),
    }
    assert_eq!(threads::store::unreadable_ledger_rows(), 1);
}

#[test]
fn a_degraded_row_is_counted_so_the_mismatch_is_never_silent() {
    let _guard = shape_guard();
    let s = Store::open_in_memory().unwrap();
    s.upsert_thread(&thread_row("t1")).unwrap();
    assert_eq!(
        threads::store::unreadable_ledger_rows(),
        0,
        "a clean ledger must report zero"
    );
    for seq in 1..=3 {
        insert_raw_payload(&s, "t1", seq, r#"{"kind":"from_the_future"}"#);
    }
    s.list_events("t1", 0, 100, None).unwrap();
    assert_eq!(
        threads::store::unreadable_ledger_rows(),
        3,
        "every degraded row must be counted, not just the first"
    );
}

#[test]
fn a_degraded_row_can_never_reach_model_context() {
    // DEC-55 is not weakened by degrading. `unknown` is LedgerOnly by
    // construction, so a row nobody has audited cannot be promoted into a
    // model's context by failing to parse — which would be the worst possible
    // way to earn an audience.
    let _guard = shape_guard();
    let s = Store::open_in_memory().unwrap();
    s.upsert_thread(&thread_row("t1")).unwrap();
    // Even a payload claiming a kind that IS model-audience degrades to
    // ledger-only, because the degraded event is `unknown`, not the kind it
    // claimed to be.
    insert_raw_payload(
        &s,
        "t1",
        1,
        r#"{"kind":"thread_system_interrupted","trigger":"nonsense","at":42}"#,
    );
    let got = s.list_events("t1", 0, 100, None).unwrap();
    assert_eq!(
        got[0].event.audience(),
        threads::event::Audience::LedgerOnly,
        "a row that failed to parse must never be replayed to a model"
    );
}

/// **THE ONE-TIME BACKFILL FILLS `last_user_input_at` FROM THE LEDGER, ONCE, AND
/// LEAVES A NEVER-SPOKEN THREAD ALONE.**
///
/// A column added to a 5,421-row table is NULL on every row already on disk, and
/// NULL means *he has never spoken here* — which for 5,418 of those threads is
/// false. The alternative to filling it was to leave them NULL and have the
/// recency read fall back to `updated_at` for them, which is the defect kept alive
/// under a condition: every thread on disk would keep being ordered by
/// last-any-event forever, and the live fold and the hydrate would disagree for
/// exactly those threads. So `Store::apply_schema` runs the backfill, and this is
/// it.
///
/// Three claims, and the third is the one a careless backfill fails:
///
/// 1. a thread with `user_message` rows gets the NEWEST of THEIR stamps — not the
///    first, and not the newest event of any kind. The fixture puts a LATER
///    non-user event on the same thread precisely so a `MAX(ts)` without the
///    `kind` filter is caught;
/// 2. a thread with events but NO `user_message` stays NULL, because that is what
///    NULL means;
/// 3. it is IDEMPOTENT and does not CLOBBER — a second open must not overwrite a
///    stamp the live append path has written since. The guard is
///    `WHERE last_user_input_at IS NULL`, and this asserts it by writing a stamp
///    that DISAGREES with the ledger and re-opening.
///
/// The re-opens are real `Store::open` calls on one file, because `apply_schema`
/// is what runs the backfill and nothing else calls it. The first open is the
/// pre-column state by construction: `upsert_thread` and `append_event` never
/// touch this column, so a store built out of them alone has it NULL on every row
/// — which is exactly the state `MIGRATIONS`' `ALTER TABLE` leaves a database
/// created before the column in.
///
/// **THE MUTATION.** In `Store::backfill_last_user_input_at`, drop the
/// `WHERE last_user_input_at IS NULL` guard — the backfill that runs on every open
/// and overwrites what the append path wrote.
///
/// Forced red:
/// ```text
/// the backfill is a ONE-TIME fill and never a repair — a second open must not
/// overwrite what the append path has written since: Some("2026-03-03T00:00:00Z")
/// ```
#[test]
fn the_backfill_fills_last_user_input_at_once_from_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("backfill.db");

    // ── OPEN 1: A LEDGER THAT PREDATES THE COLUMN ────────────────────────────
    {
        let s = Store::open(&path).unwrap();
        s.upsert_thread(&thread_row("spoke")).unwrap();
        s.upsert_thread(&thread_row("silent")).unwrap();
        for (seq, ts) in [(1u64, "2026-01-01T00:00:00Z"), (2, "2026-03-03T00:00:00Z")] {
            s.append_event(&StoredEvent {
                thread_id: "spoke".into(),
                seq,
                message_id: Some(seq),
                ts: ts.into(),
                event: user(&format!("said {seq}")),
            })
            .unwrap();
        }
        // The NEWEST event on `spoke` is NOT a user message — a `MAX(ts)` without
        // the `kind` filter would pick this one up and be six months wrong.
        s.append_event(&StoredEvent {
            thread_id: "spoke".into(),
            seq: 3,
            message_id: None,
            ts: "2026-09-09T00:00:00Z".into(),
            event: ThreadEvent::Thinking {
                text: "thinking out loud".into(),
                parent_tool_use_id: None,
            },
        })
        .unwrap();
        // `silent` has events and no user message at all.
        s.append_event(&StoredEvent {
            thread_id: "silent".into(),
            seq: 1,
            message_id: None,
            ts: "2026-05-05T00:00:00Z".into(),
            event: ThreadEvent::Thinking {
                text: "nobody spoke here".into(),
                parent_tool_use_id: None,
            },
        })
        .unwrap();
        assert_eq!(
            s.last_user_input_at("spoke").unwrap(),
            None,
            "the fixture requires the pre-column state — `append_event` must not \
             be writing this column behind the backfill's back"
        );
    }

    // ── OPEN 2: WHERE THE BACKFILL RUNS ──────────────────────────────────────
    {
        let s = Store::open(&path).unwrap();
        assert_eq!(
            s.last_user_input_at("spoke").unwrap().as_deref(),
            Some("2026-03-03T00:00:00Z"),
            "the backfill takes the newest `user_message` stamp — not the first, \
             and not the newest event of any kind"
        );
        assert_eq!(
            s.last_user_input_at("silent").unwrap(),
            None,
            "a thread with events and no `user_message` stays NULL, because NULL \
             means he has never spoken here and that is true of this one"
        );
        // The append path now writes a stamp the ledger does not contain.
        s.bump_last_seq("spoke", 4, Some("2030-01-01T00:00:00Z"))
            .unwrap();
    }

    // ── OPEN 3: AND THE BACKFILL DOES NOT UNDO IT ────────────────────────────
    {
        let s = Store::open(&path).unwrap();
        let got = s.last_user_input_at("spoke").unwrap();
        assert_eq!(
            got.as_deref(),
            Some("2030-01-01T00:00:00Z"),
            "the backfill is a ONE-TIME fill and never a repair — a second open \
             must not overwrite what the append path has written since: {got:?}"
        );
    }
}

/// **THE RECENCY READ ORDERS ON `last_user_input_at`, AND LEAVES OUT THE THREADS
/// HE HAS NEVER SPOKEN IN** — the durable half of the order, at the store.
///
/// `shapes/recent.rs`' tests prove this on the wire; this one proves it on the
/// statement, because the two halves of the order are only provably the same read
/// if each is pinned where it lives. The fixture makes `updated_at` order and
/// `last_user_input_at` order DISAGREE, which is the only arrangement that can
/// tell them apart: the thread he spoke in LONGEST ago is the one most recently
/// touched by anything else.
///
/// **THE MUTATION.** In `Store::recent_threads`, order by `updated_at` again.
///
/// Forced red:
/// ```text
/// the recency read orders on LAST USER INPUT, so the thread with the newest
/// `updated_at` is not the one at the front: ["busy", "quiet"]
/// ```
#[test]
fn the_recency_read_orders_on_last_user_input_and_skips_the_unspoken() {
    let s = Store::open_in_memory().unwrap();
    for id in ["busy", "quiet", "unspoken"] {
        s.upsert_thread(&thread_row(id)).unwrap();
    }
    // He spoke in `busy` long ago and in `quiet` recently…
    s.bump_last_seq("busy", 1, Some("2026-01-01T00:00:00Z")).unwrap();
    s.bump_last_seq("quiet", 1, Some("2026-09-09T00:00:00Z")).unwrap();
    // …and then `busy` did a great deal of something else, which is what advances
    // `updated_at` and what this read must not be influenced by.
    for seq in 2..=20u64 {
        s.bump_last_seq("busy", seq, None).unwrap();
    }

    let got: Vec<String> = s
        .recent_threads(32)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        got,
        vec!["quiet".to_string(), "busy".to_string()],
        "the recency read orders on LAST USER INPUT, so the thread with the newest \
         `updated_at` is not the one at the front, and the thread he has never \
         spoken in is not in the answer at all: {got:?}"
    );

    // And the stamp it returns is the one it ordered by — not `updated_at` wearing
    // a different column's name.
    let stamps: Vec<String> = s
        .recent_threads(32)
        .unwrap()
        .into_iter()
        .map(|(_, at)| at)
        .collect();
    assert_eq!(
        stamps,
        vec![
            "2026-09-09T00:00:00Z".to_string(),
            "2026-01-01T00:00:00Z".to_string()
        ],
        "and each row carries the stamp it was ordered by: {stamps:?}"
    );
}
