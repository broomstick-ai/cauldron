//! R11.J2 — parse and record (`docs/STRUCTURED-OUTPUT.md` §3.3–§3.4, §5).
//!
//! PROVES: the live fixtures (R11.J1, `tests/fixtures/structured/`, written by
//! the CLI and never by this test) yield `ok` / `unmet` records, and an invalid
//! value is never `ok`.
//!
//! The schemas below are the ones the probe used (PROBE-NOTE.md): `A` is
//! `{verdict ∈ {merge, block}, count: integer}`, `A10` adds `count >= 10` (the
//! `invalid-*` captures), `B` is the resume capture's `{word, vowels}`.
//! The expected statuses are written out by hand per capture, never computed
//! with the validator under test.

use serde_json::{json, Value};
use threads::event::{
    judge_structured, schema_sha, Audience, StructuredOutputRecord, StructuredStatus, ThreadEvent,
    STRUCTURED_OUTPUT_EVENT, UNPARSED_TOOL_INPUT,
};
use threads::ledger::claude_code::{translate, Outcome as Wire};
use threads::managed::TurnTerminal;
use threads::stream::parse_raw_line;

const FIRST: &str = include_str!("../fixtures/structured/first-turn-answers.jsonl");
const RESUME: &str = include_str!("../fixtures/structured/resume-turn-answers.jsonl");
const RESUME_PLAIN: &str = include_str!("../fixtures/structured/resume-turn-no-schema.jsonl");
const REFUSES: &str = include_str!("../fixtures/structured/refuses-tool.jsonl");
const EXHAUSTED: &str = include_str!("../fixtures/structured/invalid-retries-exhausted.jsonl");
const FIXED: &str = include_str!("../fixtures/structured/invalid-then-fixed.jsonl");

fn schema_a() -> Value {
    json!({"type": "object",
           "properties": {"verdict": {"enum": ["merge", "block"]}, "count": {"type": "integer"}},
           "required": ["verdict", "count"], "additionalProperties": false})
}
fn schema_a10() -> Value {
    json!({"type": "object",
           "properties": {"verdict": {"enum": ["merge", "block"]}, "count": {"type": "integer", "minimum": 10}},
           "required": ["verdict", "count"], "additionalProperties": false})
}
fn schema_b() -> Value {
    json!({"type": "object",
           "properties": {"word": {"type": "string"}, "vowels": {"type": "integer"}},
           "required": ["word", "vowels"]})
}

fn events(f: &str) -> Vec<ThreadEvent> {
    f.lines().filter(|l| !l.trim().is_empty()).flat_map(parse_raw_line).collect()
}

fn terminal_of(evs: &[ThreadEvent]) -> TurnTerminal {
    let mut t = TurnTerminal::default();
    for e in evs {
        t.observe(e);
    }
    t
}

fn record(f: &str, schema: &Value) -> StructuredOutputRecord {
    terminal_of(&events(f))
        .structured_record(schema, Some("t1".into()), Some(3), 1)
        .expect("an ok turn always gets a record")
}

fn so_inputs(f: &str) -> Vec<Value> {
    events(f)
        .into_iter()
        .filter_map(|e| match e {
            ThreadEvent::ToolUse { name, input, .. } if name == "StructuredOutput" => Some(input),
            _ => None,
        })
        .collect()
}

// ── the proves line ──────────────────────────────────────────────────────────

/// Every schema-bearing capture, with its hand-written expected verdict.
#[test]
fn the_live_fixtures_yield_ok_and_unmet_records() {
    let r = record(FIRST, &schema_a());
    assert_eq!(r.status, StructuredStatus::Ok, "{r:?}");
    assert_eq!(r.value, Some(json!({"verdict": "merge", "count": 5})));
    assert!(r.errors.is_empty());

    let r = record(RESUME, &schema_b());
    assert_eq!(r.status, StructuredStatus::Ok, "{r:?}");
    assert_eq!(r.value, Some(json!({"word": "apple", "vowels": 2})));

    let r = record(FIXED, &schema_a10());
    assert_eq!(r.status, StructuredStatus::Ok, "{r:?}");
    assert_eq!(r.value, Some(json!({"verdict": "merge", "count": 10})), "the corrected value wins");

    // success WITHOUT value (the refusal): unmet, nothing to keep.
    let r = record(REFUSES, &schema_a());
    assert_eq!(r.status, StructuredStatus::Unmet, "{r:?}");
    assert_eq!(r.value, None);
    assert_eq!(r.errors.len(), 1);

    // retries exhausted: the TURN is ok (re-classified), the record is unmet,
    // and the invalid last call is kept as the value with its errors.
    let r = record(EXHAUSTED, &schema_a10());
    assert_eq!(r.status, StructuredStatus::Unmet, "{r:?}");
    assert_eq!(r.value, Some(json!({"verdict": "maybe", "count": 5})));
    assert!(r.errors.iter().any(|e| e.starts_with("/verdict")), "{:?}", r.errors);
    assert!(r.errors.iter().any(|e| e.starts_with("/count")), "{:?}", r.errors);

    // the plain resume turn offers nothing at all.
    assert_eq!(terminal_of(&events(RESUME_PLAIN)).structured_candidate(), None);
}

/// The daemon does not trust the CLI. The SAME events from the first capture
/// — whose result line carries a value the CLI accepted — are `unmet` against
/// a schema the value breaks, and `ok` against the one it meets.
#[test]
fn an_invalid_value_is_never_ok_even_when_the_result_line_carries_it() {
    let evs = events(FIRST);
    let t = terminal_of(&evs);
    let strict = t.structured_record(&schema_a10(), None, None, 1).unwrap();
    assert_eq!(strict.status, StructuredStatus::Unmet, "{strict:?}");
    assert!(strict.errors[0].starts_with("/count"), "{:?}", strict.errors);
    let loose = t.structured_record(&schema_a(), None, None, 1).unwrap();
    assert_eq!(loose.status, StructuredStatus::Ok, "{loose:?}");
}

/// Every value the CLI REFUSED on the wire is `unmet` here too, and the one it
/// accepted is `ok` — per call, across both invalid captures (seven calls).
#[test]
fn every_refused_call_is_unmet_and_only_the_corrected_one_is_ok() {
    let fixed = so_inputs(FIXED);
    assert_eq!(fixed.len(), 2);
    assert_eq!(judge_structured(&schema_a10(), Some(&fixed[0])).0, StructuredStatus::Unmet);
    assert_eq!(judge_structured(&schema_a10(), Some(&fixed[1])).0, StructuredStatus::Ok);
    let exhausted = so_inputs(EXHAUSTED);
    assert_eq!(exhausted.len(), 5);
    for (i, v) in exhausted.iter().enumerate() {
        assert_eq!(judge_structured(&schema_a10(), Some(v)).0, StructuredStatus::Unmet, "call {i}: {v}");
    }
}

/// `{"__unparsedToolInput": …}` is unmet even under a schema it would satisfy
/// (`{"type":"object"}` accepts any object); a parsed object under the same
/// schema is ok.
#[test]
fn an_unparsed_tool_input_is_unmet_under_a_schema_it_would_satisfy() {
    let any_object = json!({"type": "object"});
    let unparsed = so_inputs(EXHAUSTED)
        .into_iter()
        .find(|v| v.get(UNPARSED_TOOL_INPUT).is_some())
        .expect("the exhausted capture has an unparsed call");
    let (s, e) = judge_structured(&any_object, Some(&unparsed));
    assert_eq!(s, StructuredStatus::Unmet, "{e:?}");
    assert_eq!(judge_structured(&any_object, Some(&json!({"a": 1}))).0, StructuredStatus::Ok);
}

/// The fallback: with the result line's value REMOVED, the last top-level
/// StructuredOutput input is the candidate (FIXED: the corrected, valid one —
/// not the first, invalid one). With both present and different, the result
/// line wins.
#[test]
fn the_fallback_is_the_last_call_and_the_result_line_outranks_it() {
    let stripped: Vec<ThreadEvent> = FIXED
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if v["type"] == "result" {
                v.as_object_mut().unwrap().remove("structured_output");
            }
            v.to_string()
        })
        .flat_map(|l| parse_raw_line(&l))
        .collect();
    assert!(stripped
        .iter()
        .any(|e| matches!(e, ThreadEvent::ResultTurn { structured_output: None, .. })));
    let t = terminal_of(&stripped);
    assert_eq!(t.structured_candidate(), Some(&json!({"verdict": "merge", "count": 10})));
    assert_eq!(t.structured_record(&schema_a10(), None, None, 1).unwrap().status, StructuredStatus::Ok);

    // both present and different: a LATER call with another value is
    // injected right after the real one; the result line's value still wins.
    let mut evs = events(FIRST);
    let real = evs
        .iter()
        .position(|e| matches!(e, ThreadEvent::ToolUse { name, .. } if name == "StructuredOutput"))
        .unwrap();
    evs.insert(
        real + 1,
        ThreadEvent::ToolUse {
            id: "x".into(),
            name: "StructuredOutput".into(),
            input: json!({"verdict": "block", "count": 1}),
            parent_tool_use_id: None,
        },
    );
    let t = terminal_of(&evs);
    assert_eq!(t.structured_candidate(), Some(&json!({"verdict": "merge", "count": 5})));

    // a subagent's StructuredOutput call is not the turn's answer.
    let sub = vec![ThreadEvent::ToolUse {
        id: "s".into(),
        name: "StructuredOutput".into(),
        input: json!({"verdict": "merge", "count": 5}),
        parent_tool_use_id: Some("toolu_parent".into()),
    }];
    assert_eq!(terminal_of(&sub).structured_candidate(), None);
}

/// A turn that failed, or never produced a result, has no answer to judge:
/// no record. The same schema on an ok turn gets one.
#[test]
fn a_failed_or_unfinished_turn_gets_no_record() {
    let failed = parse_raw_line(
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"structured_output":{"verdict":"merge","count":5}}"#,
    );
    assert!(terminal_of(&failed).structured_record(&schema_a(), None, None, 1).is_none());
    let unfinished: Vec<ThreadEvent> =
        events(FIRST).into_iter().filter(|e| !matches!(e, ThreadEvent::ResultTurn { .. })).collect();
    assert!(terminal_of(&unfinished).structured_record(&schema_a(), None, None, 1).is_none());
    assert!(terminal_of(&events(FIRST)).structured_record(&schema_a(), None, None, 1).is_some());
}

/// The record is ONE `Custom` fact of type `cauldron.threads/structured_output`,
/// ledger-only, and reads back to itself; any other Custom is not a record.
#[test]
fn the_record_is_a_ledger_only_custom_fact_that_reads_back() {
    let r = record(EXHAUSTED, &schema_a10());
    let ev = r.to_event();
    match &ev {
        ThreadEvent::Custom { event_type, data } => {
            assert_eq!(event_type, STRUCTURED_OUTPUT_EVENT);
            assert_eq!(data["status"], "unmet");
            assert_eq!(data["for_message"], 3);
            assert_eq!(data["turn"], "t1");
            assert_eq!(data["attempt"], 1);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(ev.audience(), Audience::LedgerOnly);
    assert_eq!(StructuredOutputRecord::from_event(&ev), Some(r.clone()));
    let other = ThreadEvent::Custom { event_type: "something/else".into(), data: ev_data(&ev) };
    assert_eq!(StructuredOutputRecord::from_event(&other), None);
    // the schema name: stable for one schema, different for another.
    assert_eq!(r.schema_sha, schema_sha(&schema_a10()));
    assert_ne!(schema_sha(&schema_a()), schema_sha(&schema_a10()));
    assert_eq!(r.schema_sha.len(), 64);
}

fn ev_data(ev: &ThreadEvent) -> Value {
    match ev {
        ThreadEvent::Custom { data, .. } => data.clone(),
        _ => Value::Null,
    }
}

/// `errors[≤5]`: seven violations are recorded as five; three as three.
#[test]
fn at_most_five_errors_are_recorded() {
    let props: serde_json::Map<String, Value> =
        (1..=7).map(|i| (format!("p{i}"), json!({"type": "integer"}))).collect();
    let schema = json!({"type": "object", "properties": props});
    let seven: Value = (1..=7).map(|i| (format!("p{i}"), json!("x"))).collect::<serde_json::Map<_, _>>().into();
    let three: Value = (1..=3).map(|i| (format!("p{i}"), json!("x"))).collect::<serde_json::Map<_, _>>().into();
    assert_eq!(judge_structured(&schema, Some(&seven)).1.len(), 5);
    assert_eq!(judge_structured(&schema, Some(&three)).1.len(), 3);
}

/// The ledger adapter writes the exhausted result as `outcome: ok` (re-
/// classified); the same line with a real ceiling subtype stays `failed`.
#[test]
fn the_ledger_adapter_reads_retries_exhausted_as_ok_and_a_real_ceiling_as_failed() {
    let line: Value = EXHAUSTED
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["type"] == "result")
        .unwrap();
    let outcome_of = |v: &Value| match translate(v, Some("t1")) {
        Wire::Events(d) => d[0].body["outcome"].clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(outcome_of(&line), "ok");
    let mut ceiling = line.clone();
    ceiling["subtype"] = json!("error_max_turns");
    assert_eq!(outcome_of(&ceiling), "failed");
}
