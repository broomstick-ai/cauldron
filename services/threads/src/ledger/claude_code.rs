//! # The Claude Code adapter — wire line → event, ledger → native file, native file → what it carries
//!
//! `docs/proto-ledger.md` §1, §3, §6. The one adapter that exists. Three
//! functions and nothing stateful:
//!
//! * [`translate`] — one `stream-json` line → exactly one [`Outcome`]. Stateless,
//!   so the archive parser and a live ingest call the same thing.
//! * [`render_to`] — a whole [`Ledger`] → the native session file, in full, at a
//!   path, atomically. A pure function of the ledger and a [`RenderEnv`].
//! * [`read_native`] — a native session file → the conversation chain it
//!   carries and a count of everything else, for the round-trip test and for
//!   reconcile.
//!
//! ## The dialect finding, stated because the design overstated it
//!
//! `docs/proto-ledger.md` §1 says projection into Claude Code is *write
//! `native.line` back out — byte-faithful by construction*. Measured against
//! the corpus, that is one respelling short of true: the WIRE spells
//! `session_id`, `request_id`, `tool_use_result`, `compact_metadata`,
//! `logical_parent_uuid` and the compaction metadata's inner keys in
//! snake_case; the FILE spells every one of them camelCase (`sessionId`,
//! `requestId`, `toolUseResult`, `compactMetadata.preservedMessages.uuids`).
//! [`wire_to_file`] and [`file_to_wire`] are that respelling, symmetric, and
//! `the_respelling_is_symmetric` proves it on a real compaction line. The
//! values are untouched; the chain fields (`parentUuid`, `isSidechain`, the
//! environment stamps) are added. That is the whole distance between "the
//! line" and "the file line".

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::event::{Draft, Kind, Ledger, LedgerEvent};

/// The name this adapter writes into `Native.runtime`.
pub const RUNTIME: &str = "claude_code";

/// How a compaction summary announces itself, on the wire and in the file, in
/// every Claude Code version in the archive. The file carries no
/// `isCompactSummary` flag in the versions measured (0 of 26 native copies).
pub const SUMMARY_PREFIX: &str = "This session is being continued from a previous conversation";

/// **EPHEMERA** — lines that describe the progress of producing an event, not
/// an event. Counted by the census, never stored. `docs/proto-ledger.md` §1.
pub const EPHEMERA: &[&str] = &[
    "stream_event",
    "system/thinking_tokens",
    "system/task_progress",
    "system/status",
    "tool_progress",
    "rate_limit_event",
];

/// What one wire line becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// One or more events (an `assistant` line with two blocks is two; a
    /// `result` line is a `turn_ended` and a `usage`).
    Events(Vec<Draft>),
    /// An ephemeral line, with its label from [`EPHEMERA`].
    Ephemeral(&'static str),
    /// Not a line this adapter can place: no `type`, or not an object.
    Malformed(String),
}

/// `type/subtype` for the census, or `type` alone.
pub fn label(v: &Value) -> String {
    let t = v.get("type").and_then(Value::as_str).unwrap_or("?");
    match v.get("subtype").and_then(Value::as_str) {
        Some(s) => format!("{t}/{s}"),
        None => t.to_string(),
    }
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

/// The id a block beyond the first takes: the line's uuid with the block
/// index — one line, one uuid, but one event per block.
pub fn block_id(uuid: &str, index: usize) -> String {
    if index == 0 {
        uuid.to_string()
    } else {
        format!("{uuid}#{index}")
    }
}

/// **ONE WIRE LINE → ONE OUTCOME.** `turn` is the cookie of the capture the
/// line came from. The line's `uuid` becomes the id; its `timestamp`, where it
/// has one, the `ts` (the caller carries the last seen one forward otherwise);
/// its `parent_tool_use_id` the `within`.
pub fn translate(v: &Value, turn: Option<&str>) -> Outcome {
    if !v.is_object() {
        return Outcome::Malformed("not a JSON object".into());
    }
    let Some(ty) = s(v, "type") else {
        return Outcome::Malformed("no `type`".into());
    };
    let lbl = label(v);
    if let Some(e) = EPHEMERA.iter().find(|e| **e == lbl || **e == ty) {
        return Outcome::Ephemeral(e);
    }
    let uuid = s(v, "uuid");
    let ts = s(v, "timestamp").unwrap_or_default();
    let within = v.get("parent_tool_use_id").and_then(Value::as_str).map(str::to_string);
    let draft = |kind: Kind, body: Value, idx: usize| -> Draft {
        Draft {
            id: uuid.as_deref().map(|u| block_id(u, idx)),
            ts: ts.clone(),
            turn: turn.map(str::to_string),
            within: within.clone(),
            kind: Some(kind),
            body,
            native: None,
        }
        .native(RUNTIME, v.clone())
    };
    let one = |kind: Kind, body: Value| Outcome::Events(vec![draft(kind, body, 0)]);

    match ty.as_str() {
        "system" => match v.get("subtype").and_then(Value::as_str).unwrap_or("") {
            "init" => one(
                Kind::System,
                json!({
                    "what": "init",
                    "model": s(v, "model"),
                    "cwd": s(v, "cwd"),
                    "version": s(v, "claude_code_version"),
                    "tools": v.get("tools").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
                }),
            ),
            "compact_boundary" => {
                let m = v.get("compact_metadata").cloned().unwrap_or(Value::Null);
                one(
                    Kind::Compaction,
                    json!({
                        "phase": "boundary",
                        "trigger": m.get("trigger").cloned().unwrap_or(Value::Null),
                        "pre_tokens": m.get("pre_tokens").cloned().unwrap_or(Value::Null),
                    }),
                )
            }
            "task_started" => one(
                Kind::Child,
                json!({
                    "phase": "started",
                    "child_id": s(v, "task_id"),
                    "tool_use_id": s(v, "tool_use_id"),
                    "detail": {"description": s(v, "description"), "subagent_type": s(v, "subagent_type")},
                }),
            ),
            "task_notification" => one(
                Kind::Child,
                json!({
                    "phase": "finished",
                    "child_id": s(v, "task_id"),
                    "outcome": s(v, "status"),
                    "detail": {"summary": s(v, "summary")},
                }),
            ),
            _ => one(Kind::Native, json!({})),
        },
        "assistant" => {
            if v.get("is_api_error_message").and_then(Value::as_bool) == Some(true) {
                return one(
                    Kind::System,
                    json!({"what": "api_error", "detail": v.get("error").cloned().unwrap_or(Value::Null)}),
                );
            }
            let msg = v.get("message").cloned().unwrap_or(Value::Null);
            let api_id = s(&msg, "id");
            let blocks = msg.get("content").and_then(Value::as_array).cloned().unwrap_or_default();
            if blocks.is_empty() {
                return one(Kind::Native, json!({}));
            }
            let mut out = Vec::with_capacity(blocks.len());
            for (i, b) in blocks.iter().enumerate() {
                let (kind, body) = match b.get("type").and_then(Value::as_str) {
                    Some("text") => (
                        Kind::Assistant,
                        json!({"text": b.get("text").cloned().unwrap_or(Value::Null), "api_message_id": api_id}),
                    ),
                    Some("thinking") => (
                        Kind::Thinking,
                        json!({"text": b.get("thinking").cloned().unwrap_or(Value::Null)}),
                    ),
                    Some("tool_use") => (
                        Kind::ToolUse,
                        json!({"id": s(b, "id"), "name": s(b, "name"), "input": b.get("input").cloned().unwrap_or(Value::Null)}),
                    ),
                    _ => (Kind::Native, json!({})),
                };
                out.push(draft(kind, body, i));
            }
            Outcome::Events(out)
        }
        "user" => {
            let msg = v.get("message").cloned().unwrap_or(Value::Null);
            let blocks: Vec<Value> = match msg.get("content") {
                Some(Value::Array(a)) => a.clone(),
                Some(Value::String(t)) => vec![json!({"type": "text", "text": t})],
                _ => vec![],
            };
            if blocks.is_empty() {
                return one(Kind::Native, json!({}));
            }
            let mut out = Vec::with_capacity(blocks.len());
            for (i, b) in blocks.iter().enumerate() {
                let (kind, body) = match b.get("type").and_then(Value::as_str) {
                    Some("tool_result") => (
                        Kind::ToolResult,
                        json!({
                            "tool_use_id": s(b, "tool_use_id"),
                            "content": b.get("content").cloned().unwrap_or(Value::Null),
                            "is_error": b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                        }),
                    ),
                    Some("text") => {
                        let text = b.get("text").and_then(Value::as_str).unwrap_or("");
                        if text.starts_with(SUMMARY_PREFIX) {
                            (Kind::Compaction, json!({"phase": "summary", "summary_text": text}))
                        } else {
                            (Kind::Context, json!({"text": text}))
                        }
                    }
                    other => (
                        // A `document` or `image` block: a user turn the API saw, with
                        // no text. The line carries it; the renderer emits it from there.
                        Kind::User,
                        json!({"text": null, "detail": {"block": other}}),
                    ),
                };
                out.push(draft(kind, body, i));
            }
            Outcome::Events(out)
        }
        "result" => {
            // R11.J2: a run that exhausted its StructuredOutput retries did its
            // work; only the shape is missing (`retry::is_schema_unmet_result`).
            let failed = v.get("is_error").and_then(Value::as_bool).unwrap_or(false)
                && !crate::retry::is_schema_unmet_result(v.get("subtype").and_then(Value::as_str).unwrap_or(""));
            let u = v.get("usage").cloned().unwrap_or(Value::Null);
            let model = v
                .get("modelUsage")
                .and_then(Value::as_object)
                .and_then(|m| m.keys().next().cloned());
            Outcome::Events(vec![
                draft(
                    Kind::TurnEnded,
                    json!({
                        "outcome": if failed { "failed" } else { "ok" },
                        "detail": {
                            "subtype": s(v, "subtype"),
                            "duration_ms": v.get("duration_ms").cloned().unwrap_or(Value::Null),
                            "num_turns": v.get("num_turns").cloned().unwrap_or(Value::Null),
                            "total_cost_usd": v.get("total_cost_usd").cloned().unwrap_or(Value::Null),
                        },
                    }),
                    0,
                ),
                Draft {
                    id: uuid.as_deref().map(|u| format!("{u}#usage")),
                    ..draft(
                        Kind::Usage,
                        json!({
                            "model": model,
                            "input_tokens": u.get("input_tokens").cloned().unwrap_or(Value::Null),
                            "output_tokens": u.get("output_tokens").cloned().unwrap_or(Value::Null),
                            "cache_read_tokens": u.get("cache_read_input_tokens").cloned().unwrap_or(Value::Null),
                            "cache_creation_tokens": u.get("cache_creation_input_tokens").cloned().unwrap_or(Value::Null),
                        }),
                        0,
                    )
                },
            ])
        }
        _ => one(Kind::Native, json!({})),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The respelling between the wire dialect and the file dialect
// ─────────────────────────────────────────────────────────────────────────────

const TOP_LEVEL: &[(&str, &str)] = &[
    ("session_id", "sessionId"),
    ("request_id", "requestId"),
    ("tool_use_result", "toolUseResult"),
    ("compact_metadata", "compactMetadata"),
    ("logical_parent_uuid", "logicalParentUuid"),
];

fn snake_to_camel(k: &str) -> String {
    let mut out = String::with_capacity(k.len());
    let mut up = false;
    for c in k.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn camel_to_snake(k: &str) -> String {
    let mut out = String::with_capacity(k.len() + 4);
    for c in k.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn respell_keys(v: &Value, f: &dyn Fn(&str) -> String) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (f(k), respell_keys(v, f))).collect()),
        Value::Array(a) => Value::Array(a.iter().map(|v| respell_keys(v, f)).collect()),
        other => other.clone(),
    }
}

/// A wire line's keys in the file's spelling. Top-level: the five renames;
/// `compact_metadata`'s subtree: every key camelCased. Everything else as is.
pub fn wire_to_file(line: &Value) -> Value {
    let Some(m) = line.as_object() else { return line.clone() };
    let mut out = Map::with_capacity(m.len());
    for (k, v) in m {
        let nk = TOP_LEVEL.iter().find(|(w, _)| w == k).map(|(_, f)| *f).unwrap_or(k.as_str());
        let nv = if k == "compact_metadata" { respell_keys(v, &snake_to_camel) } else { v.clone() };
        out.insert(nk.to_string(), nv);
    }
    Value::Object(out)
}

/// The inverse of [`wire_to_file`].
pub fn file_to_wire(line: &Value) -> Value {
    let Some(m) = line.as_object() else { return line.clone() };
    let mut out = Map::with_capacity(m.len());
    for (k, v) in m {
        let nk = TOP_LEVEL.iter().find(|(_, f)| f == k).map(|(w, _)| *w).unwrap_or(k.as_str());
        let nv = if k == "compactMetadata" { respell_keys(v, &camel_to_snake) } else { v.clone() };
        out.insert(nk.to_string(), nv);
    }
    Value::Object(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Projection
// ─────────────────────────────────────────────────────────────────────────────

/// What the renderer stamps on every line that the ledger does not hold: the
/// runtime's environment fields, which its loader reads but never validates.
#[derive(Debug, Clone)]
pub struct RenderEnv {
    pub cwd: String,
    pub version: String,
    pub git_branch: Option<String>,
}

/// **THE NAMED LOSSES** a render can meet, `docs/proto-ledger.md` §3. The
/// `projected` event records them by these names, and the round-trip test
/// refuses any loss that is not one of them.
pub const LOSSES: &[&str] = &[
    // An event inside a child agent's run (`within` set): the sidecar's, not the main chain's.
    "subagent_interior",
    // A kind that is not a conversation line (turn_started, turn_ended, usage, child, system, native).
    "not_a_conversation_line",
    // A block beyond the first of one wire line: emitted once, on the line of block 0.
    "folded_block",
    // A `user` event whose text is unrecoverable (null) — the archive case with no native copy.
    "prompt_text_unrecoverable",
];

/// What a render did. `digest` is sha256 of the bytes written.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub lines: usize,
    pub bytes: u64,
    pub digest: String,
    pub losses: BTreeMap<&'static str, usize>,
}

fn stamp(line: &mut Map<String, Value>, ledger: &Ledger, env: &RenderEnv, e: &LedgerEvent, parent: Option<&str>) {
    line.insert("parentUuid".into(), parent.map(|p| json!(p)).unwrap_or(Value::Null));
    line.insert("isSidechain".into(), json!(false));
    line.insert("userType".into(), json!("external"));
    line.insert("cwd".into(), json!(env.cwd));
    line.insert("sessionId".into(), json!(ledger.thread_id()));
    line.insert("version".into(), json!(env.version));
    line.insert("gitBranch".into(), env.git_branch.as_ref().map(|b| json!(b)).unwrap_or(Value::Null));
    line.insert("uuid".into(), json!(e.id));
    line.insert("timestamp".into(), json!(e.ts));
}

/// Build the file's lines in order, with the chain fields set. Pure.
pub fn render_lines(ledger: &Ledger, env: &RenderEnv) -> (Vec<Value>, BTreeMap<&'static str, usize>) {
    let mut lines = Vec::new();
    let mut losses: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut prev: Option<String> = None;
    let lose = |what: &'static str, losses: &mut BTreeMap<&'static str, usize>| {
        *losses.entry(what).or_insert(0) += 1;
    };
    for e in ledger.events() {
        if e.within.is_some() {
            lose("subagent_interior", &mut losses);
            continue;
        }
        if e.id.contains('#') {
            lose("folded_block", &mut losses);
            continue;
        }
        let from_native = |key: &str| -> Value {
            e.native
                .as_ref()
                .and_then(|n| n.line.get(key).cloned())
                .unwrap_or(Value::Null)
        };
        let mut line = Map::new();
        match e.kind {
            Kind::User => {
                line.insert("type".into(), json!("user"));
                match (from_native("message"), e.body.get("text").and_then(Value::as_str)) {
                    (Value::Null, None) => {
                        lose("prompt_text_unrecoverable", &mut losses);
                        continue;
                    }
                    (Value::Null, Some(text)) => {
                        line.insert("message".into(), json!({"role": "user", "content": text}));
                    }
                    (msg, _) => {
                        line.insert("message".into(), msg);
                    }
                }
            }
            Kind::Context => {
                line.insert("type".into(), json!("user"));
                line.insert("isMeta".into(), json!(true));
                let msg = match from_native("message") {
                    Value::Null => json!({"role": "user", "content": [{"type": "text", "text": e.body.get("text").cloned().unwrap_or(Value::Null)}]}),
                    m => m,
                };
                line.insert("message".into(), msg);
            }
            Kind::Assistant | Kind::Thinking | Kind::ToolUse => {
                line.insert("type".into(), json!("assistant"));
                let msg = match from_native("message") {
                    Value::Null => json!({"role": "assistant", "content": [assistant_block(e)]}),
                    m => m,
                };
                line.insert("message".into(), msg);
                if let Value::String(r) = from_native("request_id") {
                    line.insert("requestId".into(), json!(r));
                }
            }
            Kind::ToolResult => {
                line.insert("type".into(), json!("user"));
                let msg = match from_native("message") {
                    Value::Null => json!({"role": "user", "content": [{
                        "type": "tool_result",
                        "tool_use_id": e.body.get("tool_use_id").cloned().unwrap_or(Value::Null),
                        "content": e.body.get("content").cloned().unwrap_or(Value::Null),
                        "is_error": e.body.get("is_error").cloned().unwrap_or(json!(false)),
                    }]}),
                    m => m,
                };
                line.insert("message".into(), msg);
                if let r @ Value::Object(_) = from_native("tool_use_result") {
                    line.insert("toolUseResult".into(), r);
                }
            }
            Kind::Compaction => {
                if e.body.get("phase").and_then(Value::as_str) == Some("boundary") {
                    line.insert("type".into(), json!("system"));
                    line.insert("subtype".into(), json!("compact_boundary"));
                    line.insert("content".into(), json!("Conversation compacted"));
                    line.insert("level".into(), json!("info"));
                    line.insert(
                        "compactMetadata".into(),
                        respell_keys(&from_native("compact_metadata"), &snake_to_camel),
                    );
                    line.insert(
                        "logicalParentUuid".into(),
                        prev.as_ref().map(|p| json!(p)).unwrap_or(Value::Null),
                    );
                    stamp(&mut line, ledger, env, e, None);
                    lines.push(Value::Object(line));
                    prev = Some(e.id.clone());
                    continue;
                }
                line.insert("type".into(), json!("user"));
                let text = e.body.get("summary_text").cloned().unwrap_or(Value::Null);
                line.insert("message".into(), json!({"role": "user", "content": text}));
            }
            Kind::TurnStarted | Kind::TurnEnded | Kind::Usage | Kind::Child | Kind::System | Kind::Native => {
                lose("not_a_conversation_line", &mut losses);
                continue;
            }
        }
        stamp(&mut line, ledger, env, e, prev.as_deref());
        lines.push(Value::Object(line));
        prev = Some(e.id.clone());
    }
    (lines, losses)
}

fn assistant_block(e: &LedgerEvent) -> Value {
    match e.kind {
        Kind::Thinking => json!({"type": "thinking", "thinking": e.body.get("text").cloned().unwrap_or(Value::Null), "signature": ""}),
        Kind::ToolUse => json!({"type": "tool_use", "id": e.body.get("id"), "name": e.body.get("name"), "input": e.body.get("input")}),
        _ => json!({"type": "text", "text": e.body.get("text").cloned().unwrap_or(Value::Null)}),
    }
}

/// **RENDER THE LEDGER INTO THE NATIVE FILE, IN FULL, ATOMICALLY.** Compact
/// JSON, one line each, written to a dot-prefixed sibling and renamed over
/// the target, so a crash leaves either the old file or the new one.
pub fn render_to(ledger: &Ledger, env: &RenderEnv, path: &Path) -> std::io::Result<Rendered> {
    let (lines, losses) = render_lines(ledger, env);
    let mut buf = Vec::new();
    for l in &lines {
        serde_json::to_writer(&mut buf, l)?;
        buf.push(b'\n');
    }
    let digest = format!("{:x}", Sha256::digest(&buf));
    let tmp = path.with_file_name(format!(
        ".{}.rendering",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("session")
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&buf)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(Rendered {
        lines: lines.len(),
        bytes: buf.len() as u64,
        digest,
        losses,
    })
}

/// **RECORD WHAT A RENDER DID**, beside the turn it precedes: bytes, digest and
/// every loss met, under its name. The daemon that adopts this appends it at
/// turn start, after `render_to` and before the spawn — `ledger::turn` is that
/// daemon path, and it passes the turn cookie so the record carries `turn`
/// like every other event observed in that turn. `None` is the archive and
/// test form.
pub fn record_projected(ledger: &mut Ledger, rendered: &Rendered, now_ts: &str, turn: Option<&str>) {
    ledger
        .append(
            Draft::new(
                Kind::System,
                now_ts,
                json!({
                    "what": "projected",
                    "detail": {
                        "runtime": RUNTIME,
                        "lines": rendered.lines,
                        "bytes": rendered.bytes,
                        "digest": rendered.digest,
                        "losses": rendered.losses,
                    },
                }),
            )
            .in_turn(turn),
        )
        .expect("a minted id is never a replay");
}

/// **THE NATIVE LINE TYPES A RENDER DOES NOT REPRODUCE** — the side state and
/// the file-only records of `docs/proto-ledger.md` §3's table. The real-corpus
/// round-trip refuses a native file whose non-conversation lines include a
/// type not listed here, so a new one is named the day it appears.
pub const NATIVE_NOT_PROJECTED: &[&str] = &[
    "attachment",
    "queue-operation",
    "last-prompt",
    "ai-title",
    "atis-latch",
    "cost-state",
    "permission-mode",
    "pr-link",
    "continued-in",
    "bridge-session",
    "mode",
    "file-history-snapshot",
    "file-history-delta",
    // The runtime's own record of an API error (the wire's `assistant` line with
    // `is_api_error_message`, which the ledger holds as `system{api_error}`).
    // Not sent to the API; the daemon's scrub already blanks it in place today.
    "system/api_error",
    // An assistant line the runtime synthesised and never emitted on the wire:
    // `model: "<synthetic>"` — the `isApiErrorMessage` text (`<scrubbed>`) and
    // the "No response requested." filler after a turn with no assistant
    // output. Measured: 58 of 4,440 conversation lines across 26 native copies.
    "assistant/synthetic",
];

/// The non-conversation line types of a native file that are NOT in
/// [`NATIVE_NOT_PROJECTED`] — a new side-state record Claude Code started
/// writing, which must be named before it is accepted as a loss.
pub fn unlisted_native_types(other: &BTreeMap<String, usize>) -> Vec<String> {
    other
        .keys()
        .filter(|t| !NATIVE_NOT_PROJECTED.contains(&t.as_str()))
        .cloned()
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Reading a native file back
// ─────────────────────────────────────────────────────────────────────────────

/// One conversation line of a native file.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainLine {
    pub uuid: String,
    pub parent_uuid: Option<String>,
    /// `user` | `assistant` | `compact_boundary`
    pub kind: String,
    pub is_meta: bool,
    pub is_tool_result: bool,
    pub is_summary: bool,
    pub timestamp: Option<String>,
}

/// What a native file carries: the chain, and a count of every other line type.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NativeFile {
    pub chain: Vec<ChainLine>,
    pub other: BTreeMap<String, usize>,
    pub malformed: usize,
    pub bytes: u64,
}

/// Classify one native line.
pub fn classify_native(v: &Value) -> Result<ChainLine, String> {
    let ty = v.get("type").and_then(Value::as_str).ok_or("no type")?;
    let synthetic = v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
        || v.get("message").and_then(|m| m.get("model")).and_then(Value::as_str) == Some("<synthetic>");
    let kind = match (ty, v.get("subtype").and_then(Value::as_str)) {
        ("assistant", _) if synthetic => return Err("assistant/synthetic".to_string()),
        ("user", _) | ("assistant", _) => ty.to_string(),
        ("system", Some("compact_boundary")) => "compact_boundary".to_string(),
        ("system", Some(sub)) => return Err(format!("system/{sub}")),
        _ => return Err(ty.to_string()),
    };
    let uuid = s(v, "uuid").ok_or_else(|| format!("{ty} without uuid"))?;
    let content = v.get("message").and_then(|m| m.get("content"));
    let is_tool_result = content
        .and_then(Value::as_array)
        .map(|a| a.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")))
        .unwrap_or(false);
    let text = match content {
        Some(Value::String(t)) => t.as_str(),
        Some(Value::Array(a)) => a.first().and_then(|b| b.get("text")).and_then(Value::as_str).unwrap_or(""),
        _ => "",
    };
    Ok(ChainLine {
        uuid,
        parent_uuid: s(v, "parentUuid"),
        kind,
        is_meta: v.get("isMeta").and_then(Value::as_bool).unwrap_or(false),
        is_tool_result,
        is_summary: ty == "user" && text.starts_with(SUMMARY_PREFIX),
        timestamp: s(v, "timestamp"),
    })
}

/// Read a native session file from byte `from`.
pub fn read_native_from(path: &Path, from: u64) -> std::io::Result<NativeFile> {
    let bytes = std::fs::read(path)?;
    let mut out = NativeFile {
        bytes: bytes.len() as u64,
        ..Default::default()
    };
    let start = (from as usize).min(bytes.len());
    for raw in bytes[start..].split(|b| *b == b'\n') {
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(raw) else {
            out.malformed += 1;
            continue;
        };
        match classify_native(&v) {
            Ok(c) => out.chain.push(c),
            Err(other) => *out.other.entry(other).or_insert(0) += 1,
        }
    }
    Ok(out)
}

pub fn read_native(path: &Path) -> std::io::Result<NativeFile> {
    read_native_from(path, 0)
}

/// **THE CHAIN IS WHAT THE LOADER WALKS.** Every line's `parentUuid` is the
/// previous line's uuid, except the first and every compaction boundary,
/// whose parent is null. Answers the first break, or `None` when intact.
pub fn chain_break(chain: &[ChainLine]) -> Option<(usize, String)> {
    let mut prev: Option<&str> = None;
    for (i, l) in chain.iter().enumerate() {
        let expect = if i == 0 || l.kind == "compact_boundary" { None } else { prev };
        if l.parent_uuid.as_deref() != expect {
            return Some((i, format!("line {i} ({}) has parentUuid {:?}, expected {:?}", l.uuid, l.parent_uuid, expect)));
        }
        prev = Some(&l.uuid);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::event::ThreadMeta;

    fn meta() -> ThreadMeta {
        ThreadMeta::root("t", RUNTIME, "2026-10-08T00:00:00Z")
    }

    fn kinds(o: &Outcome) -> Vec<Kind> {
        match o {
            Outcome::Events(d) => d.iter().map(|d| d.kind.unwrap()).collect(),
            other => panic!("expected events, got {other:?}"),
        }
    }

    /// **A KNOWN LINE LANDS TYPED; AN UNKNOWN ONE LANDS `native`; EPHEMERA ARE
    /// NAMED; A LINE WITH NO TYPE IS MALFORMED.** The positive sentinel for the
    /// census: a translator that typed nothing, or that swallowed the unknown,
    /// is red here.
    ///
    /// **THE MUTATION.** In `translate`, make the `"assistant"` arm fall through
    /// to `_`.
    ///
    /// Forced red:
    /// ```text
    /// an assistant text block lands as `assistant`
    /// ```
    #[test]
    fn known_lines_are_typed_unknown_lines_are_native_ephemera_are_named() {
        let a = json!({"type": "assistant", "uuid": "u1", "timestamp": "2026-10-08T00:00:01Z", "parent_tool_use_id": null,
            "message": {"id": "msg_1", "content": [{"type": "thinking", "thinking": "hm", "signature": "sig"}, {"type": "text", "text": "hi"}]}});
        let o = translate(&a, Some("cookie"));
        assert_eq!(kinds(&o), vec![Kind::Thinking, Kind::Assistant], "an assistant text block lands as `assistant`");
        let Outcome::Events(d) = o else { unreachable!() };
        assert_eq!(d[0].id.as_deref(), Some("u1"));
        assert_eq!(d[1].id.as_deref(), Some("u1#1"), "the second block of one line takes the line's uuid plus its index");
        assert_eq!(d[1].turn.as_deref(), Some("cookie"));
        assert_eq!(d[1].body["api_message_id"], "msg_1");
        assert_eq!(d[0].native.as_ref().unwrap().line, a, "the whole line rides along");

        let unknown = json!({"type": "something_new", "uuid": "u2"});
        assert_eq!(kinds(&translate(&unknown, None)), vec![Kind::Native], "an unknown type is kept as `native`");
        let unknown_sub = json!({"type": "system", "subtype": "vcs_state_changed", "uuid": "u3"});
        assert_eq!(kinds(&translate(&unknown_sub, None)), vec![Kind::Native]);

        assert_eq!(translate(&json!({"type": "stream_event", "uuid": "u4"}), None), Outcome::Ephemeral("stream_event"));
        assert_eq!(
            translate(&json!({"type": "system", "subtype": "thinking_tokens", "uuid": "u5"}), None),
            Outcome::Ephemeral("system/thinking_tokens")
        );
        assert!(matches!(translate(&json!({"uuid": "u6"}), None), Outcome::Malformed(_)));
        assert!(matches!(translate(&json!("a string"), None), Outcome::Malformed(_)));

        let within = json!({"type": "user", "uuid": "u7", "parent_tool_use_id": "toolu_9",
            "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}]}});
        let Outcome::Events(d) = translate(&within, None) else { panic!() };
        assert_eq!(d[0].kind, Some(Kind::ToolResult));
        assert_eq!(d[0].within.as_deref(), Some("toolu_9"), "a line inside a child agent's run carries `within`");

        let summary = json!({"type": "user", "uuid": "u8", "message": {"content": [{"type": "text", "text": format!("{SUMMARY_PREFIX} that ran out")}]}});
        let Outcome::Events(d) = translate(&summary, None) else { panic!() };
        assert_eq!((d[0].kind, d[0].body["phase"].as_str()), (Some(Kind::Compaction), Some("summary")));
        let skill = json!({"type": "user", "uuid": "u9", "message": {"content": [{"type": "text", "text": "Base directory for this skill: x"}]}});
        assert_eq!(kinds(&translate(&skill, None)), vec![Kind::Context]);
        let pdf = json!({"type": "user", "uuid": "u11", "message": {"content": [{"type": "document", "source": {"media_type": "application/pdf"}}]}});
        let Outcome::Events(d) = translate(&pdf, None) else { panic!() };
        assert_eq!((d[0].kind, d[0].body["text"].is_null(), d[0].body["detail"]["block"].as_str()), (Some(Kind::User), true, Some("document")),
            "a document block is a user turn the API saw, with no text");

        let result = json!({"type": "result", "subtype": "success", "uuid": "u10", "is_error": false, "duration_ms": 5, "num_turns": 1,
            "usage": {"input_tokens": 1, "output_tokens": 2, "cache_read_input_tokens": 3, "cache_creation_input_tokens": 4}, "modelUsage": {"claude-x": {}}});
        let Outcome::Events(d) = translate(&result, None) else { panic!() };
        assert_eq!(d.iter().map(|d| d.kind.unwrap()).collect::<Vec<_>>(), vec![Kind::TurnEnded, Kind::Usage]);
        assert_eq!(d[1].id.as_deref(), Some("u10#usage"));
        assert_eq!(d[1].body["model"], "claude-x");
        assert_eq!(d[0].body["outcome"], "ok");
    }

    /// **THE RESPELLING IS SYMMETRIC**, on the real shape of a compaction line:
    /// `file_to_wire(wire_to_file(x)) == x`, and the file spelling is the one
    /// the loader reads (`compactMetadata.preservedMessages.uuids`).
    ///
    /// **THE MUTATION.** In `wire_to_file`, drop the `compact_metadata` subtree
    /// respelling (`let nv = v.clone()`).
    ///
    /// Forced red:
    /// ```text
    /// the file spelling is camelCase down the compaction metadata
    /// ```
    #[test]
    fn the_respelling_is_symmetric() {
        let wire = json!({"type": "system", "subtype": "compact_boundary", "uuid": "b", "session_id": "t",
            "logical_parent_uuid": "p", "compact_metadata": {"trigger": "auto", "pre_tokens": 1,
            "preserved_messages": {"anchor_uuid": "a", "uuids": ["x"]}}});
        let file = wire_to_file(&wire);
        assert_eq!(file["sessionId"], "t");
        assert_eq!(file["logicalParentUuid"], "p");
        assert_eq!(
            file["compactMetadata"]["preservedMessages"]["uuids"],
            json!(["x"]),
            "the file spelling is camelCase down the compaction metadata"
        );
        assert_eq!(file["compactMetadata"]["preTokens"], 1);
        assert!(file.get("compact_metadata").is_none());
        assert_eq!(file_to_wire(&file), wire, "and back");
        let user = json!({"type": "user", "uuid": "u", "tool_use_result": {"stdout": "x", "persistedOutputPath": "/p"}});
        let f = wire_to_file(&user);
        assert_eq!(f["toolUseResult"]["persistedOutputPath"], "/p", "inner keys of tool_use_result are untouched");
        assert_eq!(file_to_wire(&f), user);
    }

    /// **A RENDER IS A CHAIN THE LOADER CAN WALK, AND EVERY LOSS IS NAMED.**
    /// Build a ledger with every kind, render it, read it back: the
    /// conversation lines are the conversation events in order, under their
    /// own ids, chained; the compaction boundary restarts the chain; the rest
    /// is counted under a name in `LOSSES`.
    ///
    /// **THE MUTATION.** In `render_lines`, pass `prev.as_deref()` to `stamp`
    /// for the boundary too (never restart the chain).
    ///
    /// Forced red:
    /// ```text
    /// a compaction boundary restarts the chain
    /// ```
    #[test]
    fn a_render_is_a_walkable_chain_with_named_losses() {
        let mut l = Ledger::new(meta());
        let ts = |n: u32| format!("2026-10-08T00:00:{n:02}Z");
        let add = |l: &mut Ledger, k: Kind, id: &str, n: u32, body: Value| {
            l.append(Draft::new(k, ts(n), body).with_id(id)).unwrap();
        };
        add(&mut l, Kind::System, "s0", 0, json!({"what": "init"}));
        add(&mut l, Kind::TurnStarted, "t0", 0, json!({"attempt": 1}));
        add(&mut l, Kind::User, "u1", 1, json!({"text": "hello"}));
        add(&mut l, Kind::Context, "c1", 2, json!({"text": "skill text"}));
        add(&mut l, Kind::Thinking, "a1", 3, json!({"text": "hm"}));
        add(&mut l, Kind::Assistant, "a1#1", 3, json!({"text": "hi"}));
        add(&mut l, Kind::ToolUse, "a2", 4, json!({"id": "toolu_1", "name": "Read", "input": {}}));
        add(&mut l, Kind::ToolResult, "r1", 5, json!({"tool_use_id": "toolu_1", "content": "ok", "is_error": false}));
        l.append(Draft::new(Kind::Assistant, ts(6), json!({"text": "inner"})).with_id("in1").within(Some("toolu_1".into()))).unwrap();
        add(&mut l, Kind::Compaction, "b1", 7, json!({"phase": "boundary", "trigger": "auto"}));
        add(&mut l, Kind::Compaction, "b2", 8, json!({"phase": "summary", "summary_text": format!("{SUMMARY_PREFIX}…")}));
        add(&mut l, Kind::Assistant, "a3", 9, json!({"text": "after"}));
        add(&mut l, Kind::TurnEnded, "e1", 10, json!({"outcome": "ok"}));
        add(&mut l, Kind::Usage, "e1#usage", 10, json!({"model": "m"}));
        add(&mut l, Kind::Child, "k1", 10, json!({"phase": "finished", "child_id": "x"}));
        add(&mut l, Kind::Native, "n1", 10, json!({}));
        add(&mut l, Kind::User, "u2", 11, json!({"text": null}));
        l.append(Draft::new(Kind::User, ts(12), json!({"text": null, "detail": {"block": "document"}})).with_id("u3")
            .native(RUNTIME, json!({"type": "user", "uuid": "u3", "message": {"role": "user", "content": [{"type": "document", "source": {}}]}}))).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let env = RenderEnv { cwd: "/w".into(), version: "2.1.280".into(), git_branch: None };
        let r = render_to(&l, &env, &path).unwrap();
        let back = read_native(&path).unwrap();
        assert_eq!(back.malformed, 0);
        assert!(back.other.is_empty(), "a render emits conversation lines only: {:?}", back.other);
        let ids: Vec<&str> = back.chain.iter().map(|c| c.uuid.as_str()).collect();
        assert_eq!(ids, vec!["u1", "c1", "a1", "a2", "r1", "b1", "b2", "a3", "u3"], "the conversation events, in order, under their ids");
        assert_eq!(r.lines, 9);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.lines().last().unwrap().contains("\"type\":\"document\""), "a user turn with no text renders the runtime's own message");
        let boundary = &back.chain[5];
        assert_eq!((boundary.kind.as_str(), boundary.parent_uuid.as_deref()), ("compact_boundary", None), "a compaction boundary restarts the chain");
        assert_eq!(chain_break(&back.chain), None, "the chain is walkable");
        assert_eq!(back.chain[6].parent_uuid.as_deref(), Some("b1"));
        assert!(back.chain[1].is_meta && back.chain[6].is_summary && back.chain[4].is_tool_result);
        let mut want = BTreeMap::new();
        want.insert("subagent_interior", 1);
        want.insert("folded_block", 2);
        want.insert("not_a_conversation_line", 5);
        want.insert("prompt_text_unrecoverable", 1);
        assert_eq!(r.losses, want, "every loss is counted under its name");
        for k in r.losses.keys() {
            assert!(LOSSES.contains(k), "{k} is not a declared loss");
        }
        // Deterministic: the same ledger renders the same bytes.
        let r2 = render_to(&l, &env, &dir.path().join("t2.jsonl")).unwrap();
        assert_eq!((r.digest.clone(), r.bytes), (r2.digest, r2.bytes), "a render is a pure function of the ledger");
        // Compact JSON, as the loader's fallback scan requires.
        assert!(text.contains("\"type\":\"user\"") && !text.contains("\"type\": \"user\""));
        // The chain reader sees a break when there is one.
        let mut broken = back.chain.clone();
        broken[2].parent_uuid = Some("nope".into());
        assert!(chain_break(&broken).is_some());
        // A runtime-synthesised assistant line and the file's api_error record
        // are non-conversation lines under names the loss list carries.
        let synth = json!({"type": "assistant", "uuid": "s", "message": {"model": "<synthetic>", "content": [{"type": "text", "text": "No response requested."}]}});
        assert_eq!(classify_native(&synth), Err("assistant/synthetic".to_string()));
        let apierr = json!({"type": "system", "subtype": "api_error", "uuid": "e"});
        assert_eq!(classify_native(&apierr), Err("system/api_error".to_string()));
        assert!(NATIVE_NOT_PROJECTED.contains(&"assistant/synthetic") && NATIVE_NOT_PROJECTED.contains(&"system/api_error"));
        // And the loss list refuses a side-state type it has not named.
        let mut other = BTreeMap::new();
        other.insert("queue-operation".to_string(), 2);
        assert!(unlisted_native_types(&other).is_empty());
        other.insert("zzz-new-record".to_string(), 1);
        assert_eq!(unlisted_native_types(&other), vec!["zzz-new-record"], "an unnamed side-state type is refused, not accepted as loss");
    }
}
