//! Full-import + delta-reconciliation tests (Q8) against a synthetic AUI
//! project (`.mind/threads.json` + `.mind/threads/<id>.jsonl`).
//!
//! The second half of this file pins the three IMPORT GAPS the migration
//! readiness dry run measured against the operator's real data: an imported
//! thread came out on no account, in the wrong working directory, and with a
//! session id shared by three projects silently merged. Each test here fails
//! without its fix.

use std::path::{Path, PathBuf};

use serde_json::json;
use threads::accounts::{Account, Registry, DEFAULT_ACCOUNT};
use threads::event::ThreadEvent;
use threads::import::{
    claude_project_slug, import_minds_root, import_project, AccountStore, ImportContext,
};
use threads::store::Store;

const AUI_THREAD_ID: &str = "aaaaaaaa-1111-2222-3333-444444444444";
const SESSION_ID: &str = "bbbbbbbb-5555-6666-7777-888888888888";

fn write_project(root: &std::path::Path, jsonl_lines: &[&str]) {
    write_thread(root, AUI_THREAD_ID, SESSION_ID, jsonl_lines);
}

/// One header + its event file inside `root`'s `.mind`, appending to whatever
/// index is already there.
fn write_thread(root: &Path, aui_id: &str, session_id: &str, jsonl_lines: &[&str]) {
    let mind = root.join(".mind");
    std::fs::create_dir_all(mind.join("threads")).unwrap();
    let index_path = mind.join("threads.json");
    let mut threads: Vec<serde_json::Value> = std::fs::read_to_string(&index_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("threads").and_then(|t| t.as_array()).cloned())
        .unwrap_or_default();
    threads.retain(|t| t.get("id").and_then(|v| v.as_str()) != Some(aui_id));
    threads.push(json!({
        "id": aui_id,
        "session_id": session_id,
        "summary": "test thread",
        "created_at": "2026-07-01T00:00:00Z",
        "last_turn_at": "2026-07-27T00:00:00Z",
        "turn_count": 2
    }));
    std::fs::write(&index_path, json!({ "threads": threads }).to_string()).unwrap();
    std::fs::write(
        mind.join("threads").join(format!("{aui_id}.jsonl")),
        jsonl_lines.join("\n") + "\n",
    )
    .unwrap();
}

/// A claude conversation on disk, filed exactly the way Claude Code files one:
/// `<store>/projects/<slug of cwd>/<session id>.jsonl`, every line stamped with
/// the `cwd` it ran in. `dir_cwd` decides the DIRECTORY, `recorded_cwd` decides
/// what the lines claim — normally the same path.
fn plant_transcript(
    store_root: &Path,
    dir_cwd: &str,
    recorded_cwd: &str,
    session_id: &str,
    lines: usize,
) -> PathBuf {
    let dir = store_root
        .join("projects")
        .join(claude_project_slug(dir_cwd));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session_id}.jsonl"));
    let body: String = (0..lines)
        .map(|i| {
            json!({
                "type": "user", "sessionId": session_id, "cwd": recorded_cwd,
                "message": {"role": "user", "content": format!("line {i}")},
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&path, body).unwrap();
    path
}

/// A context over one or more fake account stores, machine default first.
fn ctx_over(stores: &[(&str, &Path)]) -> ImportContext {
    ImportContext {
        stores: stores
            .iter()
            .map(|(name, root)| AccountStore {
                name: (*name).to_string(),
                root: root.to_path_buf(),
            })
            .collect(),
        default_account: stores.first().map(|(n, _)| (*n).to_string()),
    }
}

fn fixture_lines() -> Vec<&'static str> {
    vec![
        r#"{"kind":"user","idx":0,"ts":"2026-07-01T00:01:00Z","text":"hello agent","wav_path":"/captures/one.wav"}"#,
        r#"{"kind":"assistant_speech","idx":1,"ts":"2026-07-01T00:02:00Z","text":"hello operator"}"#,
        r#"{"kind":"summary_updated","idx":2,"ts":"2026-07-01T00:02:30Z","summary":"greetings"}"#,
        r#"{"kind":"failed_input","idx":3,"ts":"2026-07-01T00:03:00Z","text":"broken","error":"usage limit","error_kind":"usage_limit","retry_at":"2026-07-01T05:00:00Z"}"#,
        "this line is not json",
        r#"{"kind":"thread_compacted","idx":5,"ts":"2026-07-01T00:04:00Z","trigger":"manual","instructions":"focus"}"#,
    ]
}

#[test]
fn full_import_preserves_ids_text_and_vocabulary() {
    let tmp = tempfile::tempdir().unwrap();
    write_project(tmp.path(), &fixture_lines());
    let store = Store::open_in_memory().unwrap();

    let report = import_project(&store, tmp.path(), &ImportContext::default()).unwrap();
    assert_eq!(report.threads_seen, 1);
    assert_eq!(report.threads_created, 1);
    assert_eq!(report.events_imported, 6);

    // The ledger thread is keyed by the CLAUDE SESSION uuid (Q2 join key).
    let row = store.get_thread(SESSION_ID).unwrap().unwrap();
    assert_eq!(row.cwd.as_deref(), Some(tmp.path().to_str().unwrap()));
    // The AUI header is preserved under settings metadata.
    let settings: serde_json::Value = serde_json::from_str(&row.settings_json).unwrap();
    assert_eq!(settings["metadata"]["aui_import"]["id"], AUI_THREAD_ID);

    let events = store.list_events(SESSION_ID, 0, 100, None).unwrap();
    assert_eq!(events.len(), 6);

    // Line i → seq i+1, message_id i — ids preserved verbatim, ts preserved.
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[0].message_id, Some(0));
    assert_eq!(events[0].ts, "2026-07-01T00:01:00Z");
    match &events[0].event {
        ThreadEvent::UserMessage { text, metadata, .. } => {
            assert_eq!(text, "hello agent");
            assert_eq!(metadata["wav_path"], "/captures/one.wav");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(&events[1].event, ThreadEvent::AssistantText { text, .. } if text == "hello operator"));

    // Non-native kinds keep their own vocabulary + full payload (§11).
    match &events[2].event {
        ThreadEvent::Custom { event_type, data } => {
            assert_eq!(event_type, "summary_updated");
            assert_eq!(data["summary"], "greetings");
        }
        other => panic!("unexpected {other:?}"),
    }
    match &events[3].event {
        ThreadEvent::Custom { event_type, data } => {
            assert_eq!(event_type, "failed_input");
            assert_eq!(data["error_kind"], "usage_limit");
        }
        other => panic!("unexpected {other:?}"),
    }
    // A malformed line still occupies its slot so later ids never shift.
    match &events[4].event {
        ThreadEvent::Custom { event_type, .. } => assert_eq!(event_type, "unparsed_line"),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(events[5].message_id, Some(5));

    // Counters continue past the imported tail.
    let row = store.get_thread(SESSION_ID).unwrap().unwrap();
    assert_eq!(row.next_message_id, 6);
    assert_eq!(row.last_seq, 6);

    // ── AND THE RECENCY STAMP COMES OUT OF THE IMPORTED ROWS ─────────────────
    //
    // An import appends events WITHOUT calling `bump_last_seq` per event — it
    // bumps once at the end — so there is no per-append site for the recency
    // stamp to ride and `import_project` carries the newest `user_message` ts out
    // of the loop itself. Without that, a freshly imported thread would land with
    // `last_user_input_at` NULL and hold no place in `threads/recent-updates`
    // even though his words are in the rows just written; the one-time backfill
    // cannot cover it either, because that runs at OPEN and an import happens
    // afterwards.
    //
    // It is the `user` line's own ts (idx 0, 00:01:00) and not the newest line's
    // (idx 5, 00:04:00), so a stamp taken from the wrong row is visible here.
    assert_eq!(
        store.last_user_input_at(SESSION_ID).unwrap().as_deref(),
        Some("2026-07-01T00:01:00Z"),
        "an imported thread's recency stamp is the newest USER line it imported"
    );

    // Retrieval by preserved id works (G20 over imported history).
    // (By range since R2.S3a, which retired the by-ids read with its only
    // caller, `threads/{id}/messages`.)
    let got = store
        .events_by_range(SESSION_ID, Some(1), Some(1), None, None)
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].message_id, Some(1));
}

#[test]
fn reconcile_imports_only_the_delta() {
    let tmp = tempfile::tempdir().unwrap();
    write_project(tmp.path(), &fixture_lines());
    let store = Store::open_in_memory().unwrap();

    import_project(&store, tmp.path(), &ImportContext::default()).unwrap();

    // Post-cutover: AUI appended two more lines to the JSONL.
    let mut lines = fixture_lines();
    lines.push(r#"{"kind":"user","idx":6,"ts":"2026-07-27T09:00:00Z","text":"new after snapshot"}"#);
    lines.push(r#"{"kind":"assistant_speech","idx":7,"ts":"2026-07-27T09:01:00Z","text":"caught up"}"#);
    write_project(tmp.path(), &lines);

    let report = import_project(&store, tmp.path(), &ImportContext::default()).unwrap();
    assert_eq!(report.events_imported, 2, "only the delta");
    assert_eq!(report.events_skipped_watermark, 6);
    assert_eq!(report.threads_created, 0, "thread row not recreated");

    let events = store.list_events(SESSION_ID, 0, 100, None).unwrap();
    assert_eq!(events.len(), 8);
    assert_eq!(events[6].message_id, Some(6));
    assert!(matches!(&events[6].event, ThreadEvent::UserMessage { text, .. } if text == "new after snapshot"));

    // ── THE DELTA ADVANCES THE RECENCY STAMP ────────────────────────────────
    assert_eq!(
        store.last_user_input_at(SESSION_ID).unwrap().as_deref(),
        Some("2026-07-27T09:00:00Z"),
        "the delta's own `user` line is the new last user input"
    );

    // Idempotence: a third run is a no-op.
    let report = import_project(&store, tmp.path(), &ImportContext::default()).unwrap();
    assert_eq!(report.events_imported, 0);

    // ── AND A RUN THAT IMPORTS NOTHING MUST NOT BLANK IT ────────────────────
    //
    // The third run still reaches `bump_last_seq`, because `line_count > 0` is
    // about the FILE and not about the delta — and it reaches it with `None`,
    // every line having been skipped by the watermark. One statement writes both
    // the watermark and the stamp, so a `CASE WHEN` whose sense was backwards
    // would erase his last message here, on a no-op.
    assert_eq!(
        store.last_user_input_at(SESSION_ID).unwrap().as_deref(),
        Some("2026-07-27T09:00:00Z"),
        "a reconcile that imports nothing must not erase the recency stamp"
    );
}

#[test]
fn minds_root_scan_imports_each_project() {
    let tmp = tempfile::tempdir().unwrap();
    let p1 = tmp.path().join("project-one");
    std::fs::create_dir_all(&p1).unwrap();
    write_project(&p1, &fixture_lines()[..2].to_vec());
    // A dir without .mind is skipped quietly.
    std::fs::create_dir_all(tmp.path().join("not-a-project")).unwrap();

    let store = Store::open_in_memory().unwrap();
    let report = import_minds_root(&store, tmp.path(), &ImportContext::default()).unwrap();
    assert_eq!(report.threads_seen, 1);
    assert_eq!(report.events_imported, 2);
}

// ──────────────────────────────────────────────────────────────────────────
//  GAP 2 — the account
// ──────────────────────────────────────────────────────────────────────────

/// Every imported row used to come out with `account: NULL` and no
/// `thread_accounts` history, so `reresolve_account(Auto, None)` fell through
/// to real placement and round-robined the thread onto whichever login had
/// room — which is never guaranteed to be the login that HOLDS its
/// conversation. The importer now places the thread where its conversation
/// actually is, and records the same answer in `thread_accounts` so the
/// admission gate and placement cannot disagree.
#[test]
fn an_imported_thread_is_placed_on_the_account_that_holds_its_conversation() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let cwd = tmp.path().join("workdir");
    std::fs::create_dir_all(&cwd).unwrap();
    write_project(&project, &fixture_lines());

    let machine = tmp.path().join("store-default");
    let other = tmp.path().join("store-other");
    std::fs::create_dir_all(other.join("projects")).unwrap();
    plant_transcript(
        &other,
        cwd.to_str().unwrap(),
        cwd.to_str().unwrap(),
        SESSION_ID,
        4,
    );

    let store = Store::open_in_memory().unwrap();
    let ctx = ctx_over(&[("default", &machine), ("second-login", &other)]);
    let report = import_project(&store, &project, &ctx).unwrap();

    assert_eq!(report.threads_with_conversation, 1);
    assert_eq!(report.placed.get("second-login"), Some(&1));

    let row = store.get_thread(SESSION_ID).unwrap().unwrap();
    assert_eq!(
        row.account.as_deref(),
        Some("second-login"),
        "the thread must be placed on the login whose store holds its conversation, \
         not left NULL for the round-robin"
    );
    assert_eq!(
        store.thread_accounts(SESSION_ID).unwrap(),
        vec!["second-login".to_string()],
        "the continuity gate must see the same conversation placement does"
    );
}

/// A thread whose conversation is gone (claude retention) has no evidence to
/// derive an account from — it goes to the machine default, where the AUI
/// direct path has always run it, and gets NO `thread_accounts` row, because
/// it genuinely has no conversation anywhere.
#[test]
fn a_thread_with_no_conversation_goes_to_the_machine_default_with_no_history() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    write_project(&project, &fixture_lines());
    let machine = tmp.path().join("store-default");
    std::fs::create_dir_all(machine.join("projects")).unwrap();

    let store = Store::open_in_memory().unwrap();
    let ctx = ctx_over(&[("default", &machine)]);
    let report = import_project(&store, &project, &ctx).unwrap();

    assert_eq!(report.threads_with_conversation, 0);
    let row = store.get_thread(SESSION_ID).unwrap().unwrap();
    // `ctx_over` names the store "default" explicitly, so this asserts the
    // FALLBACK mechanism (a thread with no conversation takes the context's
    // account) and not that the machine default is preferred — which
    // `the_import_context_neither_searches_nor_places_on_the_machine_default`
    // now forbids.
    assert_eq!(row.account.as_deref(), Some("default"));
    assert!(
        store.thread_accounts(SESSION_ID).unwrap().is_empty(),
        "no conversation anywhere means no established account history"
    );
}

/// **THE MACHINE DEFAULT IS NEITHER SEARCHED FIRST NOR PLACED ON.**
///
/// This test asserted the exact opposite until 2026-08-27, and what it was
/// protecting was the last path in the crate that could still CREATE `default`
/// rows — long after placement stopped knowing the concept. `from_registry`
/// hoisted the machine default to the front of the search, so `~/.claude` — the
/// operator's own terminal login store — was scanned first for every session
/// id; a hit there set the thread's account to `"default"`, and that string was
/// written to `threads.account` and embedded in `settings_json`. It then
/// PREFERRED `"default"` as the fallback for any thread whose conversation could
/// not be located at all.
///
/// > *"We do not want the threads module to have any concept of the default."*
///
/// The old justification — "it is the store the direct path writes" — was true
/// and is no longer a reason: a thread placed there could never be SERVED,
/// because nothing named `default` is a pool member. Writing it recorded a
/// location that is not a location.
///
/// **THE MUTATION.** Restore either `names.sort_by_key(|n| n != DEFAULT_ACCOUNT)`
/// or the `if reg.get(DEFAULT_ACCOUNT).is_some()` fallback, and this goes red on
/// the corresponding assertion.
#[test]
fn the_import_context_neither_searches_nor_places_on_the_machine_default() {
    let mut reg = Registry::default();
    reg.ensure_default();
    reg.accounts.insert(
        "aaa-first-alphabetically".into(),
        Account {
            config_dir: Some("/tmp/aaa".into()),
        },
    );
    let ctx = ImportContext::from_registry(&reg);

    assert!(
        !ctx.stores.iter().any(|s| s.name == DEFAULT_ACCOUNT),
        "the operator's own terminal store is not a place this importer looks: {:?}",
        ctx.stores.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    assert_eq!(
        ctx.stores[0].name, "aaa-first-alphabetically",
        "plain alphabetical, nothing hoisted"
    );
    assert_eq!(
        ctx.default_account.as_deref(),
        Some("aaa-first-alphabetically"),
        "a thread with no located conversation goes to the first REAL account — the only \
         answer placement can act on"
    );
}

// ──────────────────────────────────────────────────────────────────────────
//  GAP 3 — the working directory
// ──────────────────────────────────────────────────────────────────────────

/// `claude --resume` resolves inside `projects/<slug of the cwd>/`. The
/// importer used to write the PROJECT ROOT as the cwd, and on this machine not
/// one of the twenty-three surviving conversations is filed under its project
/// root — they are all under the harness slug, because the AUI direct path
/// never set a working directory and inherited the ui-server's. The cwd is now
/// read out of the conversation itself.
#[test]
fn an_imported_threads_cwd_is_derived_from_its_conversation_not_the_project_root() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("some-project");
    std::fs::create_dir_all(&project).unwrap();
    write_project(&project, &fixture_lines());
    // The conversation ran somewhere else entirely — the ui-server's cwd.
    let real_cwd = tmp.path().join("elsewhere").join("harness");
    std::fs::create_dir_all(&real_cwd).unwrap();
    let real_cwd = real_cwd.to_str().unwrap().to_string();

    let machine = tmp.path().join("store-default");
    let planted = plant_transcript(&machine, &real_cwd, &real_cwd, SESSION_ID, 7);

    let store = Store::open_in_memory().unwrap();
    let ctx = ctx_over(&[("default", &machine)]);
    import_project(&store, &project, &ctx).unwrap();

    let row = store.get_thread(SESSION_ID).unwrap().unwrap();
    assert_eq!(
        row.cwd.as_deref(),
        Some(real_cwd.as_str()),
        "the cwd must come from the conversation, not from the project root"
    );
    let settings: serde_json::Value = serde_json::from_str(&row.settings_json).unwrap();
    assert_eq!(settings["cwd"], real_cwd, "the spawn reads settings.cwd");
    assert_eq!(settings["metadata"]["aui_cwd_source"], "transcript");

    // The property that actually matters: the daemon's next spawn will look in
    // exactly the directory the conversation is in.
    let resolved = machine
        .join("projects")
        .join(claude_project_slug(&row.cwd.unwrap()))
        .join(format!("{SESSION_ID}.jsonl"));
    assert_eq!(resolved, planted, "--resume must resolve to the real file");
    assert!(resolved.exists());
}

/// Continuity outranks the migration: a conversation whose recorded cwd does
/// not slug to its own directory cannot be proven resumable, so the thread is
/// left alone on the direct path rather than imported and quietly restarted.
#[test]
fn a_conversation_that_cannot_be_proven_resumable_is_left_on_the_direct_path() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    write_project(&project, &fixture_lines());

    let machine = tmp.path().join("store-default");
    // Filed under one path's slug, but the lines claim a different cwd.
    plant_transcript(
        &machine,
        "/tmp/where-it-is",
        "/tmp/where-it-claims",
        SESSION_ID,
        3,
    );

    let store = Store::open_in_memory().unwrap();
    let ctx = ctx_over(&[("default", &machine)]);
    let report = import_project(&store, &project, &ctx).unwrap();

    assert_eq!(report.threads_created, 0);
    assert_eq!(report.events_imported, 0);
    assert_eq!(report.skipped_unsafe.len(), 1);
    assert!(report.skipped_unsafe[0].contains(SESSION_ID));
    assert!(
        store.get_thread(SESSION_ID).unwrap().is_none(),
        "an unprovable thread must not exist on the daemon at all — the AUI \
         continuity gate is what keeps it on the direct path"
    );
}

#[test]
fn the_slug_matches_claude_codes_own_project_directory_naming() {
    assert_eq!(
        claude_project_slug("/Users/testuser/code/harness"),
        "-Users-testuser-code-harness"
    );
    assert_eq!(
        claude_project_slug("/Users/testuser/.aui/mythcellaneous"),
        "-Users-testuser--aui-mythcellaneous"
    );
    assert_eq!(
        claude_project_slug("/Users/testuser/code/broomstick/workshops/c2w7 - landscape"),
        "-Users-testuser-code-broomstick-workshops-c2w7---landscape"
    );
}

// ──────────────────────────────────────────────────────────────────────────
//  GAP 4 — one session id, several project indexes
// ──────────────────────────────────────────────────────────────────────────

/// Forty-six session ids appear in more than one project's index (a copied
/// project). The importer keyed by session id and processed project by project,
/// so the LAST project read won the cwd and the metadata, and the other copies'
/// events were dropped on the `(thread_id, seq)` conflict without a word. Now
/// the whole minds root is resolved before anything is written: the richest
/// source wins deterministically and every source is recorded.
#[test]
fn a_session_id_in_two_projects_is_imported_once_from_the_richer_source() {
    let tmp = tempfile::tempdir().unwrap();
    let thin = tmp.path().join("aaa-thin-copy");
    let rich = tmp.path().join("zzz-rich-original");
    std::fs::create_dir_all(&thin).unwrap();
    std::fs::create_dir_all(&rich).unwrap();

    // The same conversation, indexed by both projects under the same AUI id —
    // one of them has kept going.
    let short = fixture_lines()[..2].to_vec();
    let long = fixture_lines();
    write_thread(&thin, AUI_THREAD_ID, SESSION_ID, &short);
    write_thread(&rich, AUI_THREAD_ID, SESSION_ID, &long);

    let store = Store::open_in_memory().unwrap();
    let report = import_minds_root(&store, tmp.path(), &ImportContext::default()).unwrap();

    assert_eq!(report.threads_seen, 2, "two headers");
    assert_eq!(report.threads_distinct, 1, "one conversation");
    assert_eq!(report.collisions_resolved, 1);
    assert_eq!(report.threads_created, 1);
    assert_eq!(
        report.events_imported,
        long.len(),
        "the richer source's events, whole — not a silent merge of both"
    );

    let row = store.get_thread(SESSION_ID).unwrap().unwrap();
    assert_eq!(
        row.cwd.as_deref(),
        Some(rich.to_str().unwrap()),
        "the richer source wins deterministically, not whichever was read last"
    );
    let settings: serde_json::Value = serde_json::from_str(&row.settings_json).unwrap();
    let sources = settings["metadata"]["aui_import_sources"]
        .as_array()
        .expect("every colliding source is recorded");
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["project"], "zzz-rich-original");
    assert_eq!(sources[1]["project"], "aaa-thin-copy");
    assert!(
        sources[0]["event_file_bytes"].as_u64() > sources[1]["event_file_bytes"].as_u64(),
        "the recorded collision says WHY this source won"
    );
}

/// **His compaction reason is recorded, and is never something the model reads**
/// (harness INTENT 79b).
///
/// AUI grew a second compaction event so the operator can hear the difference
/// between a thread he compacted himself and one the context overflowed: a
/// `thread_compaction_started` carrying the voice reason he sends with it,
/// followed later by a bare `thread_compacted`. The reason is his own words
/// about his own work, and the daemon has no business feeding it back into a
/// prompt.
///
/// That safety is not a rule anyone had to write for this kind — it falls out
/// of the importer's catch-all, which turns every kind it does not translate
/// into `Custom`, and of `audience()`, which answers `LedgerOnly` for `Custom`.
/// This test pins that the new kind actually lands there rather than being
/// special-cased into the model's context by some later well-meaning arm, and
/// that its text survives verbatim for the ledger.
///
/// Because it never reaches `assemble_model_notices`, it never reaches
/// `guard_injection` either — CON-22 is untouched by this event, which is the
/// correct relationship: an event that cannot become context cannot leak
/// capacity.
#[test]
fn an_aui_compaction_start_is_ledger_only_and_keeps_his_reason() {
    use threads::event::{audience_of_kind, Audience};

    let tmp = tempfile::tempdir().unwrap();
    let mut lines = fixture_lines();
    lines.push(
        r#"{"kind":"thread_compaction_started","idx":6,"ts":"2026-07-01T00:03:50Z","reason":"keep the API decisions"}"#,
    );
    write_project(tmp.path(), &lines);
    let store = Store::open_in_memory().unwrap();
    import_project(&store, tmp.path(), &ImportContext::default()).unwrap();

    let events = store.list_events(SESSION_ID, 0, 100, None).unwrap();
    let started = events
        .iter()
        .find(|e| {
            matches!(&e.event, ThreadEvent::Custom { event_type, .. }
                if event_type == "thread_compaction_started")
        })
        .expect("the start event kept its own vocabulary through import");

    // It is recorded in full — the reason is the whole point of the event.
    assert!(
        matches!(&started.event, ThreadEvent::Custom { data, .. }
            if data["reason"] == "keep the API decisions"),
        "the operator's compaction reason was lost on import"
    );

    // And it can never become context.
    assert_eq!(started.event.audience(), Audience::LedgerOnly);
    assert_eq!(audience_of_kind("thread_compaction_started"), Audience::LedgerOnly);
    assert!(!started.event.audience().reaches_model());
}
