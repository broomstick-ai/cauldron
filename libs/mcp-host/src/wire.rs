//! # The seam words — how a call reaches the host
//!
//! Four words on the supervisor's one-word line protocol (the same seam that
//! answers `STATUS`, `RESTART`, `STOP`). Each request is one line; each answer
//! is ONE JSON line.
//!
//! | request | answer |
//! |---|---|
//! | `MCP_TOOLS` | `{"tools":[{name,description,inputSchema,server}], "in_process":[{server,provider}]}` — the PROCESS servers' tools, answered from the declarations without waking anything |
//! | `MCP_CALL <json>` | `{"ok": <CallToolResult>}` or `{"refused": {"code","message"}}`; `<json>` is `{"tool","arguments","thread_id"?}` |
//! | `MCP_STATUS` | [`ToolHost::status`] |
//! | `MCP_SLEEP` | `{"slept": <server or null>}` |
//!
//! The caller is `services/threads/src/mcp_route.rs`, which speaks these
//! words without linking this crate; [`WORDS`] and the answer shapes are the
//! contract between the two, and each side's tests pin its half.

use serde_json::{json, Value};

use crate::decl::Residency;
use crate::host::ToolHost;

pub const TOOLS: &str = "MCP_TOOLS";
pub const CALL: &str = "MCP_CALL";
pub const STATUS: &str = "MCP_STATUS";
pub const SLEEP: &str = "MCP_SLEEP";
pub const WORDS: &[&str] = &[TOOLS, CALL, STATUS, SLEEP];

fn refused(code: &str, message: impl Into<String>) -> Value {
    json!({"refused": {"code": code, "message": message.into()}})
}

/// Answer one request. `None` when `verb` is not one of [`WORDS`].
///
/// **No outer lock** (R5.S2b): every connection shares one `&ToolHost`, and
/// the host locks only what one word needs. `MCP_TOOLS` reads the immutable
/// declarations and takes no lock at all, so it is answered while a tool is
/// wedged; an `MCP_CALL` for one tool waits on another only up to the host's
/// preempt bound.
pub fn answer(h: &ToolHost, verb: &str, rest: &str) -> Option<Value> {
    if !WORDS.contains(&verb) {
        return None;
    }
    Some(match verb {
        TOOLS => {
            let mut tools = Vec::new();
            let mut in_process = Vec::new();
            for d in h.decls() {
                match d.residency {
                    Residency::Process => tools.extend(d.tools.iter().map(|t| {
                        json!({"name": t.name, "description": t.description, "inputSchema": t.schema(), "server": d.name})
                    })),
                    Residency::InProcess => in_process.push(json!({"server": d.name, "provider": d.provider})),
                }
            }
            json!({"tools": tools, "in_process": in_process})
        }
        CALL => {
            let req: Value = match serde_json::from_str(rest) {
                Ok(v) => v,
                Err(e) => return Some(refused("malformed", format!("MCP_CALL takes one JSON object: {e}"))),
            };
            let Some(tool) = req.get("tool").and_then(Value::as_str) else {
                return Some(refused("malformed", "MCP_CALL needs `tool` (a string)"));
            };
            let args = req.get("arguments").cloned().unwrap_or(json!({}));
            match h.call(tool, args) {
                Ok(result) => json!({"ok": result}),
                Err(e) => refused(e.code(), e.message()),
            }
        }
        STATUS => h.status(),
        SLEEP => json!({"slept": h.sleep()}),
        _ => unreachable!("checked against WORDS above"),
    })
}
