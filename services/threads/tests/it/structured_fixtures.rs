//! R11.J1 — the CLI's `--json-schema` contract, pinned against LIVE captures.
//!
//! Every `.jsonl` under `tests/fixtures/structured/` is the verbatim stdout of
//! one live `claude -p --output-format stream-json --verbose --model haiku
//! --json-schema '<schema>'` run on claude 2.1.280 (2026-10-20). Nothing was
//! edited; see `tests/fixtures/structured/PROBE-NOTE.md` for the exact argv,
//! the prompts and what each capture settles. The fixtures were written by the
//! CLI, never by this test, so no assertion here can witness its own write.
//!
//! Two kinds of assertion live here and are kept apart on purpose:
//!
//! * **wire facts** — what the CLI emits (read off the raw JSON). These hold
//!   until the CLI changes, and J2's parser is built on them.
//! * **today's parse** — what `stream.rs` / `retry.rs` make of those lines on
//!   THIS commit. J2 is expected to change two of them (`ResultTurn` gains
//!   `structured_output`; `error_max_structured_output_retries` stops being
//!   fatal) and each such assertion says so where it stands.

use serde_json::{json, Value};
use threads::event::ThreadEvent;
use threads::retry::{classify_errored_result, classify_result, is_schema_unmet_result, RetryDecision};
use threads::stream::parse_raw_line;

const FIRST: &str = include_str!("../fixtures/structured/first-turn-answers.jsonl");
const RESUME: &str = include_str!("../fixtures/structured/resume-turn-answers.jsonl");
const RESUME_PLAIN: &str = include_str!("../fixtures/structured/resume-turn-no-schema.jsonl");
const REFUSES: &str = include_str!("../fixtures/structured/refuses-tool.jsonl");
const EXHAUSTED: &str = include_str!("../fixtures/structured/invalid-retries-exhausted.jsonl");
const FIXED: &str = include_str!("../fixtures/structured/invalid-then-fixed.jsonl");
const BAD_SCHEMA_STDERR: &str = include_str!("../fixtures/structured/bad-schema.stderr");

const ALL: [(&str, &str); 6] = [
    ("first-turn-answers", FIRST),
    ("resume-turn-answers", RESUME),
    ("resume-turn-no-schema", RESUME_PLAIN),
    ("refuses-tool", REFUSES),
    ("invalid-retries-exhausted", EXHAUSTED),
    ("invalid-then-fixed", FIXED),
];

fn lines(f: &str) -> Vec<Value> {
    f.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("fixture line is valid JSON"))
        .collect()
}

fn events(f: &str) -> Vec<ThreadEvent> {
    f.lines().filter(|l| !l.trim().is_empty()).flat_map(parse_raw_line).collect()
}

/// The ONE raw `result` line. Panics (fails) on zero or several.
fn result_line(f: &str) -> Value {
    let rs: Vec<Value> = lines(f)
        .into_iter()
        .filter(|v| v.get("type").and_then(Value::as_str) == Some("result"))
        .collect();
    assert_eq!(rs.len(), 1, "a single-turn capture has exactly one result line");
    rs.into_iter().next().unwrap()
}

fn init_tools(f: &str) -> Vec<String> {
    let init = lines(f)
        .into_iter()
        .find(|v| v["type"] == "system" && v["subtype"] == "init")
        .expect("capture starts with system/init");
    init["tools"]
        .as_array()
        .expect("init carries a tools array")
        .iter()
        .filter_map(|t| t.as_str().map(str::to_string))
        .collect()
}

fn session(f: &str) -> String {
    result_line(f)["session_id"].as_str().unwrap().to_string()
}

/// Today's parse of the result line: (subtype, is_error).
fn parsed_result(f: &str) -> (String, bool) {
    let rs: Vec<(String, bool)> = events(f)
        .into_iter()
        .filter_map(|e| match e {
            ThreadEvent::ResultTurn { subtype, is_error, .. } => Some((subtype, is_error)),
            _ => None,
        })
        .collect();
    assert_eq!(rs.len(), 1, "exactly one ResultTurn per capture");
    rs.into_iter().next().unwrap()
}

/// Every `StructuredOutput` tool_use as parsed today: (id, input).
fn so_calls(f: &str) -> Vec<(String, Value)> {
    events(f)
        .into_iter()
        .filter_map(|e| match e {
            ThreadEvent::ToolUse { id, name, input, .. } if name == "StructuredOutput" => {
                Some((id, input))
            }
            _ => None,
        })
        .collect()
}

/// The parsed `tool_result` for a tool_use id: (content-as-text, is_error).
fn result_for(f: &str, id: &str) -> (String, bool) {
    events(f)
        .into_iter()
        .find_map(|e| match e {
            ThreadEvent::ToolResult { tool_use_id, content, is_error, .. } if tool_use_id == id => {
                Some((content.as_str().map(str::to_string).unwrap_or_else(|| content.to_string()), is_error))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no tool_result for {id}"))
}

// ── the instrument: six captures, six different runs ─────────────────────────

/// Sentinel for doctrine 9: the six fixtures are six different things, so a
/// comparison between two of them is never `x == x`. Sessions: the two resume
/// captures share the first capture's session (that is what makes them
/// resumes); every other capture has its own.
#[test]
fn fixtures_are_six_distinct_live_runs() {
    for (i, (na, a)) in ALL.iter().enumerate() {
        for (nb, b) in ALL.iter().skip(i + 1) {
            assert_ne!(a, b, "{na} and {nb} are the same bytes");
            assert_ne!(
                result_line(a)["uuid"], result_line(b)["uuid"],
                "{na} and {nb} carry the same result uuid"
            );
        }
    }
    let first = session(FIRST);
    assert_eq!(session(RESUME), first, "resume-turn-answers resumed the first turn");
    assert_eq!(session(RESUME_PLAIN), first, "resume-turn-no-schema resumed the first turn");
    let mut others = vec![first, session(REFUSES), session(EXHAUSTED), session(FIXED)];
    others.sort();
    others.dedup();
    assert_eq!(others.len(), 4, "the fresh captures are four separate sessions");
}

// ── answers: first turn and --resume turn ────────────────────────────────────

/// A model that answers: the result line carries `structured_output`, and its
/// `result` text is that same object printed compact. The value equals the
/// input of the one StructuredOutput tool_use, whose tool_result is the CLI's
/// success string.
#[test]
fn first_turn_answer_carries_structured_output() {
    let r = result_line(FIRST);
    assert_eq!(r["subtype"], "success");
    assert_eq!(r["is_error"], false);
    assert_eq!(r["structured_output"], json!({"verdict": "merge", "count": 5}));
    let text: Value = serde_json::from_str(r["result"].as_str().unwrap()).unwrap();
    assert_eq!(text, r["structured_output"], "result text is the value, compact");

    let calls = so_calls(FIRST);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, r["structured_output"], "tool_use input == structured_output");
    assert_eq!(
        result_for(FIRST, &calls[0].0),
        ("Structured output provided successfully".to_string(), false)
    );
    assert!(init_tools(FIRST).iter().any(|t| t == "StructuredOutput"));
    // today's parse: the line is a plain success ResultTurn (J2 adds the value).
    assert_eq!(parsed_result(FIRST), ("success".into(), false));
}

/// `--resume` with a DIFFERENT schema works exactly like a first turn: the
/// tool is offered again and the value answers the new schema.
#[test]
fn resume_turn_with_schema_answers_the_new_schema() {
    let r = result_line(RESUME);
    assert_eq!(r["subtype"], "success");
    assert_eq!(r["structured_output"], json!({"word": "apple", "vowels": 2}));
    assert_ne!(r["structured_output"], result_line(FIRST)["structured_output"]);
    let calls = so_calls(RESUME);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, r["structured_output"]);
    assert!(init_tools(RESUME).iter().any(|t| t == "StructuredOutput"));
    assert_eq!(parsed_result(RESUME), ("success".into(), false));
}

/// A `--resume` turn WITHOUT `--json-schema`, on a session whose earlier turns
/// had one: the schema is not sticky. The tool is not offered, nothing calls
/// it, and the result line has no `structured_output` key at all (absent, not
/// null). This is what J3's "argv carries it on turn 1 and not on turn 2" rests
/// on.
#[test]
fn resume_without_schema_has_no_tool_and_no_key() {
    assert!(!init_tools(RESUME_PLAIN).iter().any(|t| t == "StructuredOutput"));
    assert!(so_calls(RESUME_PLAIN).is_empty());
    let r = result_line(RESUME_PLAIN);
    assert!(r.get("structured_output").is_none(), "key absent: {r}");
    assert_eq!(r["subtype"], "success");
    // counterpart: the same session's schema turn DID have the key.
    assert!(result_line(RESUME).get("structured_output").is_some());
}

// ── the settled question: success WITHOUT value ──────────────────────────────

/// A model that refuses the tool. 2.1.280 injects exactly one
/// `[structured-output-enforce]` user re-prompt, and when the model refuses
/// again the turn ends `success`, `is_error:false`, with NO `structured_output`
/// key. It does NOT end `error_max_structured_output_retries`. So "success"
/// alone never means "value present"; J2 must judge presence itself.
#[test]
fn refusal_ends_success_without_value_after_one_enforce() {
    let r = result_line(REFUSES);
    assert_eq!(r["subtype"], "success");
    assert_eq!(r["is_error"], false);
    assert!(r.get("structured_output").is_none(), "no value key on a refusal: {r}");
    assert!(so_calls(REFUSES).is_empty(), "the model never called the tool");
    assert!(init_tools(REFUSES).iter().any(|t| t == "StructuredOutput"), "it was offered");

    let enforce: Vec<Value> = lines(REFUSES)
        .into_iter()
        .filter(|v| v["type"] == "user" && v.to_string().contains("[structured-output-enforce]"))
        .collect();
    assert_eq!(enforce.len(), 1, "exactly one enforce re-prompt");
    // today's parse: a text-only user line has no tool_result, so it is kept
    // as Unknown (never dropped, never mistaken for a tool result).
    let parsed = parse_raw_line(&enforce[0].to_string());
    assert!(matches!(parsed.as_slice(), [ThreadEvent::Unknown { .. }]), "{parsed:?}");

    assert_eq!(parsed_result(REFUSES), ("success".into(), false));
    // counterpart: the answering capture has no enforce line.
    assert!(!FIRST.contains("[structured-output-enforce]"));
}

// ── invalid values ───────────────────────────────────────────────────────────

/// The CLI validates the tool input against the schema itself: an invalid
/// value comes back as an `is_error` tool_result naming the violations, and a
/// model that corrects itself ends `success` with only the CORRECTED value in
/// `structured_output`. The fallback J2 plans ("last StructuredOutput tool_use
/// input") equals the value here — but the FIRST call's input is invalid, so
/// any "first call" fallback would be wrong.
#[test]
fn invalid_value_is_rejected_by_cli_and_corrected_value_wins() {
    let calls = so_calls(FIXED);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1, json!({"verdict": "maybe", "count": 3}));
    let (text, err) = result_for(FIXED, &calls[0].0);
    assert!(err, "the invalid call's tool_result is_error");
    assert!(text.starts_with("Output does not match required schema:"), "{text}");
    assert_eq!(result_for(FIXED, &calls[1].0), ("Structured output provided successfully".into(), false));

    let r = result_line(FIXED);
    assert_eq!(r["subtype"], "success");
    assert_eq!(r["structured_output"], json!({"verdict": "merge", "count": 10}));
    assert_eq!(calls.last().unwrap().1, r["structured_output"]);
    assert_ne!(calls[0].1, r["structured_output"]);
    assert_eq!(parsed_result(FIXED), ("success".into(), false));
}

/// A model that keeps sending an invalid value: after 5 attempts (one of them
/// after the enforce re-prompt) 2.1.280 ends
/// `error_max_structured_output_retries`, `is_error:true`,
/// `terminal_reason:"structured_output_retry_exhausted"`, no `result` text, no
/// `structured_output`, and the last CLI error in `errors[0]`. Unparseable
/// JSON reaches the stream as `{"__unparsedToolInput":{raw,len}}`. The LAST
/// tool_use input here is schema-INVALID — so J2's fallback must validate,
/// never trust, it.
#[test]
fn persistent_invalid_value_ends_max_structured_output_retries() {
    let r = result_line(EXHAUSTED);
    assert_eq!(r["subtype"], "error_max_structured_output_retries");
    assert_eq!(r["is_error"], true);
    assert_eq!(r["terminal_reason"], "structured_output_retry_exhausted");
    assert!(r.get("structured_output").is_none());
    assert!(r.get("result").is_none(), "no result text on this subtype: {r}");
    let e0 = r["errors"][0].as_str().unwrap();
    assert!(e0.starts_with("Failed to provide valid structured output after 5 attempts"), "{e0}");

    let calls = so_calls(EXHAUSTED);
    assert_eq!(calls.len(), 5, "five attempts on the wire");
    for (id, _) in &calls {
        assert!(result_for(EXHAUSTED, id).1, "every attempt was refused by the CLI");
    }
    assert!(calls.iter().any(|(_, i)| i.get("__unparsedToolInput").is_some()));
    assert_eq!(calls.last().unwrap().1, json!({"verdict": "maybe", "count": 5}), "last input is invalid");
    assert_eq!(lines(EXHAUSTED).iter().filter(|v| v.to_string().contains("[structured-output-enforce]")).count(), 1);

    // FLIPPED by R11.J2: the parse reads this subtype as an OK turn (the
    // work ran, only the shape is missing); its record says `unmet`
    // (`tests/structured_record.rs`). The subtype is kept verbatim.
    assert_eq!(parsed_result(EXHAUSTED), ("error_max_structured_output_retries".into(), false));
    assert!(is_schema_unmet_result("error_max_structured_output_retries"));
    // counterparts: a real ceiling stays a ceiling, and a plain transient is
    // still not fatal.
    assert!(!is_schema_unmet_result("error_max_turns"));
    let ceiling = classify_errored_result("error_max_turns", None);
    // Exact reason (J1 dac0a7be's pin, carried onto the real ceiling):
    // `contains("error_max_turns")` would also be satisfied by the catch-all
    // `other =>` arm ("result subtype error_max_turns").
    assert!(
        matches!(&ceiling, RetryDecision::Fatal { reason }
            if reason == "hit configured ceiling: error_max_turns"),
        "{ceiling:?}"
    );
    assert!(!matches!(classify_result("error_during_execution"), RetryDecision::Fatal { .. }));
}

/// A malformed schema never reaches the model: the CLI exits 1 before any
/// stream line (the capture's stdout was 0 bytes) and says why on stderr. With
/// no `result` line the daemon would see a bare non-zero exit, so J3's
/// pre-commit `bad_schema` check is the only place this can be named.
#[test]
fn bad_schema_is_a_stderr_exit_with_no_stream() {
    assert!(
        BAD_SCHEMA_STDERR.starts_with("Error: --json-schema is not a valid JSON Schema:"),
        "{BAD_SCHEMA_STDERR}"
    );
    // Exactly one Unknown carrying the raw line: `.all(Unknown)` would also be
    // true of `[]`, i.e. of a parser that silently DROPS non-JSON lines.
    let first = BAD_SCHEMA_STDERR.lines().next().unwrap();
    let parsed = parse_raw_line(first);
    assert!(
        matches!(parsed.as_slice(), [ThreadEvent::Unknown { raw }] if raw == &json!(first.trim())),
        "{parsed:?}"
    );
}
