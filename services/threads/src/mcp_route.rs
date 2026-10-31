//! # The `/mcp` façade routes declared tools THROUGH THE SUPERVISOR — R5.S2 (DEC-34)
//!
//! The operator's ruling (`#H135`): the cauldron supervisor owns ALL MCP
//! tools — declared and version-controlled, at most one awake at a time,
//! woken on demand when a tool is given to an active thread.
//!
//! This daemon's `/mcp` door ([`crate::chassis`]'s `McpTools` impl) serves two
//! kinds of tool, and this module is the boundary between them:
//!
//! * **Ours, in-process** — the `agents_*` façade ([`crate::agents`]). It is
//!   code in this crate, declared `residency = "in-process"` by the
//!   supervisor's `mcp/agents.toml`, and served right here with no process to
//!   wake. Calls for these NEVER leave this daemon.
//! * **Everything else the supervisor declares** — external tool servers.
//!   `tools/list` adds the supervisor's declared tools (`MCP_TOOLS`, answered
//!   from the declarations without waking anything), and a `tools/call` for
//!   one is forwarded to the supervisor as `MCP_CALL`. The supervisor wakes
//!   the declaring server — putting any other to sleep — and serves it. This
//!   daemon never spawns, holds or talks to a tool process itself.
//!
//! **What a turn sees is unchanged** (R4.S5): one `--mcp-config` naming this
//! daemon's own `/mcp` under the server name `threads`, with the run token as
//! bearer. The supervisor's tools appear on that same door, so the run-token
//! contract covers them with no second credential.
//!
//! ## Finding the supervisor
//!
//! `CAULDRON_SUPERVISOR_ADDR` when set (a throwaway proof, a test); otherwise
//! the `cauldron` row of the seam table at `CAULDRON_SEAMS`, which the
//! supervisor registers for itself and `launch_hosted` hands every hosted
//! service. Neither present means no supervisor: the door serves the
//! in-process tools only, exactly as before this step.
//!
//! ## Why this crate does not link `mcp-host`
//!
//! `threads` depends on no workspace crate but the chassis (`Cargo.toml`). So
//! the four seam words and their answer shapes are spoken here by hand; the
//! supervisor's half is `libs/mcp-host/src/wire.rs`, and each side's tests pin
//! its half (`tests/mcp_supervisor_route.rs` here).

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::{json, Value};
use shapewire::envelope::{codes, Refusal};
use shapewire::mcp::ToolDef;
use shapewire::token::Claims;

/// An explicit supervisor seam address, overriding the seam table.
pub const SUPERVISOR_ADDR_VAR: &str = "CAULDRON_SUPERVISOR_ADDR";
/// The seam-table row the supervisor registers for itself.
pub const SUPERVISOR_SERVICE: &str = "cauldron";

/// How long to wait for the supervisor to accept a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// `MCP_TOOLS` is asked on every `tools/list`, i.e. at the start of every
/// turn; it is answered from declarations and must be quick.
const LIST_TIMEOUT: Duration = Duration::from_secs(2);
/// Longer than the supervisor's worst single call (`mcp_host::host::Bounds::
/// worst_call`: up to 10 s waiting out ANOTHER busy tool before preempting it
/// (R5.S2b), a 10 s wake handshake, then the 120 s call budget = 140 s), so
/// the supervisor's typed refusal arrives before this side gives up.
const CALL_TIMEOUT: Duration = Duration::from_secs(150);

/// The supervisor's seam address, if this daemon runs under one.
pub fn supervisor_addr() -> Option<SocketAddr> {
    if let Ok(v) = std::env::var(SUPERVISOR_ADDR_VAR) {
        if !v.trim().is_empty() {
            return v.trim().parse().ok();
        }
    }
    let seams = std::env::var("CAULDRON_SEAMS").ok()?;
    let text = std::fs::read_to_string(seams).ok()?;
    let table: Value = serde_json::from_str(&text).ok()?;
    table["seams"][SUPERVISOR_SERVICE]["addr"].as_str()?.parse().ok()
}

/// One request line, one JSON answer line.
fn ask(addr: SocketAddr, line: &str, timeout: Duration) -> std::io::Result<Value> {
    let mut s = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(CONNECT_TIMEOUT))?;
    s.write_all(line.as_bytes())?;
    s.write_all(b"\n")?;
    s.flush()?;
    let mut answer = String::new();
    BufReader::new(s).read_line(&mut answer)?;
    serde_json::from_str(answer.trim()).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("the supervisor answered `{}`, which is not JSON: {e}", answer.trim()),
        )
    })
}

/// **THE SUPERVISOR'S DECLARED TOOLS**, as `tools/list` entries, minus any
/// whose name `own` already uses (an in-process tool always wins its name).
/// Empty when there is no supervisor or it does not answer — a `tools/list`
/// never fails because the supervisor is away.
pub fn declared_tools(own: &[ToolDef]) -> Vec<ToolDef> {
    let Some(addr) = supervisor_addr() else { return Vec::new() };
    let answer = match ask(addr, "MCP_TOOLS", LIST_TIMEOUT) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%addr, "the supervisor did not list its MCP tools ({e}); this turn sees in-process tools only");
            return Vec::new();
        }
    };
    let Some(tools) = answer["tools"].as_array() else {
        tracing::warn!(%addr, %answer, "the supervisor's MCP_TOOLS answer has no `tools` array");
        return Vec::new();
    };
    tools
        .iter()
        .filter_map(|t| {
            let name = t["name"].as_str()?.to_string();
            if own.iter().any(|o| o.name == name) {
                tracing::warn!(tool = %name, "a supervisor-declared tool shadows an in-process one; the in-process tool keeps the name");
                return None;
            }
            Some(ToolDef {
                name,
                description: t["description"].as_str().unwrap_or("").to_string(),
                input_schema: t.get("inputSchema").cloned().unwrap_or_else(|| json!({"type": "object"})),
            })
        })
        .collect()
}

/// **FORWARD ONE `tools/call` TO THE SUPERVISOR.** `name` is not one of this
/// daemon's in-process tools (the caller checked). The supervisor wakes the
/// declaring server and serves it; its answer comes back as this call's
/// result, or as a typed refusal the model reads.
pub async fn call(caller: &Claims, name: &str, arguments: Value) -> Result<Value, Refusal> {
    let Some(addr) = supervisor_addr() else {
        // No supervisor: exactly the refusal an unknown name always earned
        // (`agents::call`), so the door answers -32602 as before.
        return Err(Refusal::new(codes::UNKNOWN_METHOD, format!("{name}; the surface is tools/list")));
    };
    let req = json!({"tool": name, "arguments": arguments, "thread_id": caller.thread_id, "run_id": caller.run_id});
    let line = format!("MCP_CALL {req}");
    let tool = name.to_string();
    let answer = tokio::task::spawn_blocking(move || ask(addr, &line, CALL_TIMEOUT))
        .await
        .map_err(|e| Refusal::new(codes::ERROR, format!("forwarding `{tool}` to the supervisor panicked: {e}")))?
        .map_err(|e| {
            Refusal::new(
                codes::ERROR,
                format!("the cauldron supervisor at {addr} did not serve `{tool}`: {e}"),
            )
        })?;
    from_answer(name, answer)
}

/// The supervisor's `MCP_CALL` answer → this door's payload or refusal.
pub fn from_answer(name: &str, answer: Value) -> Result<Value, Refusal> {
    if let Some(r) = answer.get("refused") {
        let code = r["code"].as_str().unwrap_or("");
        let message = r["message"].as_str().unwrap_or("").to_string();
        return Err(match code {
            "unknown_tool" => Refusal::new(codes::UNKNOWN_METHOD, format!("{name}; the surface is tools/list")),
            _ => Refusal::new(codes::ERROR, format!("the supervisor refused `{name}` ({code}): {message}")),
        });
    }
    let Some(result) = answer.get("ok") else {
        return Err(Refusal::new(codes::ERROR, format!("the supervisor's answer for `{name}` has neither `ok` nor `refused`: {answer}")));
    };
    // The tool server's own CallToolResult. An `isError` result is the TOOL's
    // refusal and reaches the model as one; otherwise its structured content
    // (or its content blocks) is this call's payload.
    let text = || {
        result["content"]
            .as_array()
            .map(|c| c.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    };
    if result["isError"] == json!(true) {
        return Err(Refusal::new(codes::ERROR, format!("`{name}` answered with an error: {}", text())));
    }
    Ok(match result.get("structuredContent") {
        Some(sc) if !sc.is_null() => sc.clone(),
        _ => json!({ "content": result.get("content").cloned().unwrap_or(Value::Null) }),
    })
}
