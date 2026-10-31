//! Stream-json → ThreadEvent mapping tests (ported from CCW's, plus the G4
//! `api_message_id` upgrade).

use serde_json::json;
use threads::event::ThreadEvent;
use threads::stream::{parse_line, parse_raw_line};

#[test]
fn assistant_text_carries_api_message_id() {
    // G4: CCW dropped message.id; threads keeps it.
    let v = json!({
        "type": "assistant",
        "message": {
            "id": "msg_abc123",
            "model": "claude-fable-5",
            "content": [{"type": "text", "text": "hello"}],
            "usage": {"input_tokens": 10, "output_tokens": 2}
        }
    });
    let evs = parse_line(&v);
    match &evs[0] {
        ThreadEvent::AssistantText {
            text,
            api_message_id,
            parent_tool_use_id,
        } => {
            assert_eq!(text, "hello");
            assert_eq!(api_message_id.as_deref(), Some("msg_abc123"));
            assert!(parent_tool_use_id.is_none());
        }
        other => panic!("unexpected {other:?}"),
    }
    // Usage still extracted alongside.
    assert!(matches!(evs[1], ThreadEvent::MessageUsage { input_tokens: 10, .. }));
}

/// The parser half of the double-charge fix (observed live 2026-07-29).
///
/// Claude emits one `assistant` line per CONTENT BLOCK, and every line of a
/// multi-block message repeats that message's `usage` payload byte for byte.
/// The parser is stateless, so it cannot drop the repeat — its job is to carry
/// the `message.id` that makes the repeat RECOGNIZABLE, which is what lets the
/// runtime charge each message exactly once.
#[test]
fn repeated_usage_lines_are_identifiable_by_message_id() {
    // Verbatim shape of events 7/9 from the 2026-07-29 run: a thinking block
    // and a text block of ONE message, each carrying the same usage.
    let usage = json!({
        "input_tokens": 9,
        "output_tokens": 4,
        "cache_read_input_tokens": 22418,
        "cache_creation_input_tokens": 69
    });
    let thinking = json!({
        "type": "assistant",
        "message": {
            "id": "msg_011CdWtD1c2T9vF2XVVoPdWq",
            "model": "claude-haiku-4-5-20251001",
            "content": [{"type": "thinking", "thinking": "…"}],
            "usage": usage.clone()
        }
    });
    let text = json!({
        "type": "assistant",
        "message": {
            "id": "msg_011CdWtD1c2T9vF2XVVoPdWq",
            "model": "claude-haiku-4-5-20251001",
            "content": [{"type": "text", "text": "ok"}],
            "usage": usage
        }
    });

    let usage_of = |v: &serde_json::Value| -> (Option<String>, u64) {
        parse_line(v)
            .into_iter()
            .find_map(|e| match e {
                ThreadEvent::MessageUsage {
                    api_message_id,
                    input_tokens,
                    ..
                } => Some((api_message_id, input_tokens)),
                _ => None,
            })
            .expect("every assistant line with usage yields a MessageUsage")
    };

    let (a_id, a_tokens) = usage_of(&thinking);
    let (b_id, b_tokens) = usage_of(&text);
    // Both lines really do report the same spend…
    assert_eq!(a_tokens, b_tokens);
    // …and both name the same message, so the second is a repeat, not a
    // second charge.
    assert_eq!(a_id.as_deref(), Some("msg_011CdWtD1c2T9vF2XVVoPdWq"));
    assert_eq!(a_id, b_id);

    // A stream that omits `message.id` still parses; such a line cannot be
    // deduped and is charged, because under-charging is the dangerous
    // direction and the turn epilogue reconciles either way.
    let anonymous = json!({
        "type": "assistant",
        "message": {"model": "m", "content": [{"type": "text", "text": "x"}],
                    "usage": {"input_tokens": 1}}
    });
    assert_eq!(usage_of(&anonymous).0, None);
}

#[test]
fn sidechain_text_keeps_parent_id() {
    let v = json!({
        "type": "assistant",
        "parent_tool_use_id": "toolu_parent",
        "message": {"content": [{"type": "text", "text": "sub"}]}
    });
    match &parse_line(&v)[0] {
        ThreadEvent::AssistantText {
            parent_tool_use_id, ..
        } => assert_eq!(parent_tool_use_id.as_deref(), Some("toolu_parent")),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn synthetic_429_becomes_limit_hit_with_reset() {
    let v = json!({
        "type": "assistant",
        "isApiErrorMessage": true,
        "apiErrorStatus": 429,
        "message": {"model": "<synthetic>", "content": [
            {"type": "text", "text": "You have hit your session limit resets 2:40am (America/Chicago)"}
        ]}
    });
    match &parse_line(&v)[0] {
        ThreadEvent::LimitHit { reset, status, .. } => {
            assert_eq!(*status, Some(429));
            assert_eq!(reset.as_deref(), Some("2:40am (America/Chicago)"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn compact_boundary_parses_with_empty_range() {
    let v = json!({
        "type": "system", "subtype": "compact_boundary",
        "compactMetadata": {"trigger": "auto", "preTokens": 155000}
    });
    match &parse_line(&v)[0] {
        ThreadEvent::Compacted {
            trigger,
            pre_tokens,
            message_id_range,
        } => {
            assert_eq!(trigger, "auto");
            assert_eq!(*pre_tokens, Some(155000));
            // The runtime stamps the range (§12); the parser leaves it None.
            assert!(message_id_range.is_none());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn agent_spawn_and_async_launch() {
    let spawn = json!({
        "type": "assistant",
        "message": {"content": [{"type": "tool_use", "id": "toolu_1", "name": "Agent",
            "input": {"description": "d", "subagent_type": "explore", "run_in_background": true}}]}
    });
    let evs = parse_line(&spawn);
    assert!(matches!(&evs[0], ThreadEvent::AgentSpawn { run_in_background: true, .. }));
    assert!(matches!(&evs[1], ThreadEvent::ToolUse { .. }));

    let launched = json!({
        "type": "user",
        "toolUseResult": {"isAsync": true, "status": "async_launched", "agentId": "ag-1",
            "toolUseId": "toolu_1", "outputFile": "/tmp/out.jsonl"},
        "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}]}
    });
    let evs = parse_line(&launched);
    assert!(matches!(&evs[0], ThreadEvent::AgentLaunched { is_async: true, .. }));
    assert!(matches!(&evs[1], ThreadEvent::ToolResult { .. }));
}

#[test]
fn nothing_is_dropped() {
    // Unknown type → Unknown{raw}; non-JSON → Unknown{raw string}.
    assert!(matches!(
        &parse_line(&json!({"type": "brand_new_thing"}))[0],
        ThreadEvent::Unknown { .. }
    ));
    assert!(matches!(
        &parse_raw_line("not json at all")[0],
        ThreadEvent::Unknown { .. }
    ));
    assert!(parse_raw_line("   ").is_empty());
}

#[test]
fn rate_limit_event_is_no_longer_swallowed_as_unknown() {
    // DEC-15 layer 1. VERBATIM from the 2026-07-27 burn-in
    // (`gauge-scout-report` §1): real claude 2.1.220 pushes this line inline
    // on every turn and `parse_line` had no arm for it, so the only free
    // live gauge in the system was being persisted as `Unknown`.
    let raw = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1785607200,"rateLimitType":"seven_day","utilization":0.73,"isUsingOverage":false},"uuid":"6d6d03fb-0d63-454f-bf75-17d8ff74398e","session_id":"b0410a1c-7e6f-4b2e-9a1d-3c5e8f2a6d91"}"#;
    let evs = parse_raw_line(raw);
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        ThreadEvent::RateLimitEvent {
            rate_limit_type,
            utilization,
            resets_at,
            resets_at_rfc3339,
            status,
            is_using_overage,
        } => {
            assert_eq!(rate_limit_type, "seven_day");
            // 0-1 fraction, kept as claude sends it (the /usage prose is
            // 0-100 — normalization happens once, in the gauge).
            assert_eq!(*utilization, 0.73);
            assert_eq!(*resets_at, Some(1785607200));
            assert!(resets_at_rfc3339.as_deref().unwrap().starts_with("2026-"));
            assert_eq!(status.as_deref(), Some("allowed_warning"));
            assert!(!is_using_overage);
        }
        other => panic!("unexpected {other:?}"),
    }
    // A malformed line is still never coerced into a fabricated reading.
    assert!(matches!(
        &parse_line(&json!({"type": "rate_limit_event"}))[0],
        ThreadEvent::Unknown { .. }
    ));
}

#[test]
fn limit_hit_now_names_its_pool() {
    // DEC-15 layer 3: `pool` was hardcoded `None` — the answer was sitting
    // unused in `text` the whole time.
    let v = json!({
        "type": "assistant",
        "isApiErrorMessage": true,
        "apiErrorStatus": 429,
        "message": {"content": [{"type": "text",
            "text": "You've hit your session limit · resets 2:40am (America/Chicago)"}]}
    });
    match &parse_line(&v)[0] {
        ThreadEvent::LimitHit { pool, reset, .. } => {
            assert_eq!(pool.as_deref(), Some("session"));
            assert!(reset.as_deref().unwrap().contains("2:40am"));
        }
        other => panic!("unexpected {other:?}"),
    }

    // Prose that names nothing recognizable stays honestly unattributed — and
    // as of 2026-08-27 it is not a LIMIT at all.
    //
    // THIS ASSERTION USED TO EXPECT `LimitHit`, and it was wrong in the way
    // this whole file was wrong: an API error carrying no `error: "rate_limit"`
    // and no 429 is not evidence of a usage limit, and calling it one put every
    // non-limit failure — an expired login, a 529, a rejected prompt — on the
    // usage-limit ladder. See `tests/api_error_axis.rs`.
    //
    // Note also what the fixture above is: `isApiErrorMessage` / `apiErrorStatus`,
    // camelCase, a spelling that appears as a JSON key **zero** times in the
    // 7,018 captured turn files. This test passed for months while the detector
    // it covers never fired once in production, because it modelled a door the
    // real producer does not use. The camelCase case is KEPT — the parser still
    // accepts both spellings — but the real-capture coverage now lives in
    // `tests/api_error_axis.rs`, whose fixtures are lifted verbatim out of the
    // operator's own logs.
    let v = json!({
        "type": "assistant",
        "isApiErrorMessage": true,
        "message": {"content": [{"type": "text", "text": "something went wrong"}]}
    });
    match &parse_line(&v)[0] {
        ThreadEvent::ApiError { error, status, text } => {
            assert_eq!(*error, None);
            assert_eq!(*status, None);
            assert_eq!(text, "something went wrong");
        }
        other => panic!("unexpected {other:?}"),
    }
}
