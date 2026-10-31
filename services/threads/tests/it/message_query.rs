//! **The citation query — `Store::query_messages` and its spelling** (INTENT
//! 57, msg #1438).
//!
//! *"The reason why there's a message ID in the messages is so that you can
//! save on your token consumption by telling the curator specifically which
//! messages you're referring to with some optional prose… I do not want you
//! writing prose. That's the opposite of what I want. It's tripling the amount
//! of information required. First I say it and then you say it and then it
//! hears it."*
//!
//! So a caller hands a sub-agent `#3` or `#2..#5` and the sub-agent READS
//! them.
//!
//! **R2.S3c (R4.S9, HOLDS R9) retired the `threads/{thread_id}/query`
//! REQUEST** — its consumers (harness `thread-messages`, `v5core.py`) read
//! message PAGES through the resource door now (`tests/message_pages.rs`). What
//! the request answered with is still served there, built from the same two
//! pieces, so these tests pin those pieces directly: the store's selection
//! (`Store::query_messages` — ids, range, tail, kinds, cursor, limit) and the
//! one spelling of a cited message (`server::cited_message_json`). The arm's
//! own policy (messages-only default kinds, the limit clamp) went with the arm;
//! the retirement itself is pinned in `ws_protocol.rs`.
//!
//! The ledger is SEEDED directly through the store — no claude is spawned.

use serde_json::{json, Value};
use threads::event::{StoredEvent, ThreadEvent};
use threads::settings::ThreadSettings;
use threads::store::{MessagePage, MessageQuery};
use threads::{build_manager, server, Config, ThreadManager};

fn msgs_kinds() -> Vec<String> {
    server::MESSAGE_KINDS.iter().map(|s| s.to_string()).collect()
}

fn q<'a>(kinds: Option<&'a [String]>, limit: usize) -> MessageQuery<'a> {
    MessageQuery { ids: None, from: None, to: None, tail: None, kinds, cursor: None, limit }
}

fn run(m: &ThreadManager, id: &str, query: &MessageQuery) -> (MessagePage, Vec<Value>) {
    let page = m.store.query_messages(id, query).unwrap();
    let cited = page.events.iter().map(|e| server::cited_message_json(e, true, 0)).collect();
    (page, cited)
}

fn ids_of(cited: &[Value]) -> Vec<u64> {
    cited.iter().map(|m| m["message_id"].as_u64().unwrap()).collect()
}

/// A daemon over a seeded ledger: ten alternating user/assistant messages
/// (#1..#10), one non-addressable `rate_limit_event`, and one addressable
/// category-(a) `custom` at #11.
fn seeded() -> (ThreadManager, tempfile::TempDir, String) {
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
    let tmp = tempfile::tempdir().unwrap();
    // CON-27: the machine `default` account is registered on every boot but is
    // never PLACEABLE — threads only runs on logins that carry their own config
    // dir. A fixture that registers nothing therefore has nowhere to put a
    // thread and `create_thread` refuses outright. So this ledger needs one
    // account threads actually OWNS; `aa-owned` sorts first and so plays the
    // same role in ranking that `default` used to play by accident.
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        tmp.path().join("aa-owned").to_string_lossy().to_string(),
    );
    let m = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();
    let mut s = ThreadSettings::new(tmp.path().to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    let id = m.create_thread(None, s).unwrap();

    let mut seq = 1_000u64;
    let mut push = |ev: ThreadEvent, message_id: Option<u64>| {
        seq += 1;
        m.store
            .append_event(&StoredEvent {
                thread_id: id.clone(),
                seq,
                message_id,
                ts: format!("2026-08-01T10:{:02}:00Z", seq % 60),
                event: ev,
            })
            .unwrap();
    };
    for n in 1..=10u64 {
        if n % 2 == 1 {
            push(
                ThreadEvent::UserMessage {
                    text: format!("operator message {n}"),
                    intent: Default::default(),
                    metadata: json!({"wav": format!("/tmp/{n}.wav")}),
                },
                Some(n),
            );
        } else {
            push(
                ThreadEvent::AssistantText {
                    text: format!("agent reply {n}"),
                    parent_tool_use_id: None,
                    api_message_id: None,
                },
                Some(n),
            );
        }
    }
    push(
        ThreadEvent::RateLimitEvent {
            rate_limit_type: "seven_day".into(),
            utilization: 0.73,
            resets_at: Some(1_785_607_200),
            resets_at_rfc3339: None,
            status: None,
            is_using_overage: false,
        },
        None,
    );
    push(
        ThreadEvent::Custom {
            event_type: "summary_updated".into(),
            data: json!({"summary": "AUI bookkeeping"}),
        },
        Some(11),
    );

    (m, tmp, id)
}

/// **Cite by id.** The row carries everything a citing caller needs and
/// nothing it has to retype.
#[test]
fn query_by_ids_returns_what_a_citation_needs() {
    let (m, _tmp, id) = seeded();
    let k = msgs_kinds();
    let (page, msgs) = run(&m, &id, &MessageQuery { ids: Some(&[3, 4]), ..q(Some(&k), 50) });

    assert_eq!(msgs.len(), 2);
    assert_eq!(page.total_matched, 2);
    assert!(!page.has_more);
    assert_eq!(page.next_cursor, None);

    assert_eq!(msgs[0]["message_id"], 3);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["kind"], "user_message");
    assert_eq!(msgs[0]["text"], "operator message 3");
    assert_eq!(msgs[0]["audience"], "ledger_only");
    assert!(msgs[0]["seq"].is_u64());
    assert!(msgs[0]["ts"].as_str().unwrap().starts_with("2026-08-01"));
    assert_eq!(msgs[0]["metadata"]["wav"], "/tmp/3.wav");
    assert_eq!(msgs[0]["chars"], 18);
    assert_eq!(msgs[0]["truncated"], false);

    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["text"], "agent reply 4");
    // An assistant row carries no `metadata` key at all.
    assert!(msgs[1].get("metadata").is_none());
}

/// **Cite by range** — `#2..#5`, inclusive on both ends, in id order.
#[test]
fn query_by_range_is_inclusive_and_ordered() {
    let (m, _tmp, id) = seeded();
    let k = msgs_kinds();
    let (page, msgs) = run(&m, &id, &MessageQuery { from: Some(2), to: Some(5), ..q(Some(&k), 50) });
    assert_eq!(ids_of(&msgs), vec![2, 3, 4, 5]);
    assert_eq!(page.total_matched, 4);
}

/// **Kinds select.** The category-(a) `custom` event and the
/// `rate_limit_event` are RECORDED and REACHABLE — the audience split governs
/// model context, not what a client may read — but only when named; `None`
/// is every ADDRESSABLE event.
#[test]
fn kinds_select_and_the_non_addressable_event_is_never_cited() {
    let (m, _tmp, id) = seeded();
    let k = msgs_kinds();
    let (page, msgs) = run(&m, &id, &q(Some(&k), 50));
    assert_eq!(page.total_matched, 10, "the ten messages, and only those");
    assert!(msgs.iter().all(|m| m["kind"] == "user_message" || m["kind"] == "assistant_text"));

    let custom = vec!["custom".to_string()];
    let (_, msgs) = run(&m, &id, &q(Some(&custom), 50));
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["message_id"], 11);
    assert_eq!(msgs[0]["role"], "application");
    assert_eq!(
        msgs[0]["audience"], "ledger_only",
        "every row says whether a model could ever see it"
    );

    let (page, _) = run(&m, &id, &q(None, 50));
    assert_eq!(page.total_matched, 11, "the non-addressable event is not cited");
}

/// **A page size and a cursor**, so a range query is bounded and resumable.
#[test]
fn query_pages_and_resumes() {
    let (m, _tmp, id) = seeded();
    let k = msgs_kinds();
    let (page, msgs) = run(&m, &id, &q(Some(&k), 4));
    assert_eq!(msgs.len(), 4);
    assert_eq!(page.total_matched, 10, "how much a caller did NOT get");
    assert!(page.has_more);
    assert_eq!(page.next_cursor, Some(4));

    let (_, msgs) = run(&m, &id, &MessageQuery { cursor: page.next_cursor, ..q(Some(&k), 4) });
    assert_eq!(ids_of(&msgs), vec![5, 6, 7, 8]);
}

/// `tail` for "the last N", and `max_chars` / `text:false` in the spelling so
/// a caller can survey a range before pulling its bodies.
#[test]
fn tail_truncation_and_metadata_only_modes() {
    let (m, _tmp, id) = seeded();
    let k = msgs_kinds();
    let (_, msgs) = run(&m, &id, &MessageQuery { tail: Some(3), ..q(Some(&k), 50) });
    assert_eq!(ids_of(&msgs), vec![8, 9, 10], "the last three, still in id order");

    let (page, _) = run(&m, &id, &MessageQuery { ids: Some(&[1]), ..q(Some(&k), 50) });
    let m0 = server::cited_message_json(&page.events[0], true, 8);
    assert_eq!(m0["text"], "operator");
    assert_eq!(m0["truncated"], true);
    assert_eq!(m0["chars"], 18, "the FULL length is always reported");

    let m0 = server::cited_message_json(&page.events[0], false, 0);
    assert!(m0.get("text").is_none());
    assert_eq!(m0["chars"], 18);
}

/// An unknown thread is empty, not an error; an empty `ids` list is empty.
#[test]
fn empty_selections_are_empty_not_errors() {
    let (m, _tmp, id) = seeded();
    let k = msgs_kinds();
    let (page, _) = run(&m, "no-such-thread", &q(Some(&k), 50));
    assert_eq!(page.events.len(), 0);
    assert_eq!(page.total_matched, 0);

    let (page, _) = run(&m, &id, &MessageQuery { ids: Some(&[]), ..q(Some(&k), 50) });
    assert_eq!(page.events.len(), 0);
}
