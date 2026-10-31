//! # The MCP door — a SECOND CARRIER for methods that already have a home
//!
//! `docs/managed-subagents.md` §8.1: *"Needs: a streamable-HTTP `POST /mcp`
//! door on the chassis, JSON-RPC 2.0, `initialize` / `notifications/initialized`
//! / `ping` / `tools/list` / `tools/call`, everything else `-32601`. … the MCP
//! door is a second CARRIER for methods that already have a home, not a second
//! producer."* And the operator's one carve-out from the standing no-MCP rule:
//! *"even the sub-agent should be an MCP tool call. And to be honest, I hate
//! MCP, but I don't want to use Bash for our built-in tools."*
//!
//! So this module is the carrier and nothing else. It speaks JSON-RPC over one
//! HTTP route, authenticates every call with a run token ([`crate::token`]),
//! and hands the producer two questions — *what tools do you expose* and *run
//! this one for this caller* — through [`McpTools`]. **It knows no tool by
//! name.** The façade that binds a tool to a method path is the producer's
//! (`threads::agents`), and the rule that makes that binding safe is written
//! there, where someone adding a tool will read it.
//!
//! ## What is here, and what a producer still owns
//!
//! | here | the producer's |
//! |---|---|
//! | the route, `POST /mcp` (GET and DELETE are 405) | — |
//! | the content negotiation the reference server does: `Accept` must name both `application/json` and `text/event-stream` (406), `Content-Type` must be JSON (415) | — |
//! | `mcp-protocol-version` checked against [`PROTOCOL_VERSIONS`] (400) | — |
//! | the run token read off `Authorization: Bearer` and handed to [`McpTools::verify`] (401) | the key, and the validity window |
//! | JSON-RPC framing, `initialize`'s version negotiation, the session id, `ping`, `-32601` | — |
//! | `tools/list` → [`McpTools::tools`]; `tools/call` → [`McpTools::call`], with a typed refusal rendered where the model can read it | the tools, their schemas, and every act behind them |
//!
//! ## Why there is no SSE stream and no session registry
//!
//! The streamable-HTTP transport allows a server to answer a POST with
//! `text/event-stream` and to serve a standalone `GET` stream for
//! server-initiated messages. This door does neither: every reply is one JSON
//! body, and `GET /mcp` is 405, which the spec permits for a server that
//! *"does not offer an SSE stream"*. The reason is rule 2.2 of
//! `docs/shapes.md` one layer up — a subscription is a SHAPE, and a push the
//! agent would receive over MCP would be a second transport for facts the
//! shapes already carry. An agent that wants to watch a thread subscribes to
//! `threads/log`; `agents_log` exists because an MCP call cannot hold a
//! subscription, and it must not become one.
//!
//! A session id is minted on `initialize` and echoed, because a conforming
//! client stores and resends it; it is NOT enforced, because the token already
//! names the caller on every call and a second identity beside it is the
//! two-spellings hazard with a different hat on. `DELETE /mcp` (session
//! termination) is therefore 405 too.
//!
//! ## Rule 2.7 on this door: a refusal reaches the caller WHERE IT CAN SEE IT
//!
//! A tool-level refusal is a `tools/call` RESULT with `isError: true`, the
//! code and message in `structuredContent.refused`, and the same in the text
//! block — never a JSON-RPC error. The distinction is who reads it: a JSON-RPC
//! error is handled by the client's transport and may never reach the model;
//! a result does. The one exception is a tool that does not exist, which the
//! spec names a protocol error (`-32602`), and which is a caller's typo rather
//! than an act's answer.
//!
//! ## Feature placement
//!
//! The protocol core — [`McpTools`], [`ToolDef`], [`parse`], [`answer`] — is
//! in the always-compiled floor and takes no axum: it is testable with a
//! fake, and a consumer that wants to SPEAK this protocol (a face that talks
//! to the daemon as an agent would) reads these types. The HTTP handlers are
//! behind `server` with the router that mounts them.

use std::future::Future;
use std::pin::Pin;

use serde::Serialize;
use serde_json::{json, Value};

use crate::envelope::{codes, Identity, Refusal};

/// **THE PROTOCOL VERSIONS THIS DOOR SPEAKS**, newest first.
///
/// `2025-06-18` is the revision this door was built against (JSON-RPC
/// batching removed, `MCP-Protocol-Version` header introduced); `2025-03-26`
/// is the first streamable-HTTP revision and differs on nothing this door
/// does. The installed `claude` 2.1.280 proposes `2025-11-25` on `initialize`
/// and accepts any of `["2025-11-25","2025-06-18","2025-03-26","2024-11-05",
/// "2024-10-07"]` in the answer (read out of its bundle — the
/// `@modelcontextprotocol/sdk` `Client.connect` it embeds throws *"Server's
/// protocol version is not supported"* otherwise). A proposal this door does
/// not know is answered with [`DEFAULT_PROTOCOL_VERSION`], exactly as the
/// reference server does: `zG.includes(t) ? t : LATEST`.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26"];

/// The version answered when the client's proposal is not one this door
/// knows. The newest this door speaks.
pub const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

/// The session header, as the SDK spells it (lowercase on the client, since
/// HTTP headers are case-insensitive).
pub const SESSION_HEADER: &str = "mcp-session-id";

/// The protocol-version header a client sends after `initialize`.
pub const PROTOCOL_HEADER: &str = "mcp-protocol-version";

/// JSON-RPC's own error codes, the three this door uses plus the SDK's
/// server-side `-32000` for transport-level refusals.
pub mod rpc {
    pub const PARSE: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL: i64 = -32603;
    /// What the reference server answers for a transport-level refusal — a
    /// bad `Accept`, an unsupported version. Kept so a client reading our
    /// refusals sees the same code it would from the SDK's own server.
    pub const SERVER: i64 = -32000;
    /// This door's own: the token did not verify.
    pub const UNAUTHORIZED: i64 = -32001;
}

/// One tool, as `tools/list` returns it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// Who is calling: the verified claims of the run token.
pub type Caller = crate::token::Claims;

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// **WHAT A PRODUCER HANDS THE MCP DOOR.** Three questions, none of which the
/// chassis can answer itself.
pub trait McpTools: Send + Sync {
    /// Verify the `Authorization` header's value — `Bearer <run token>` — and
    /// say who the caller is. `None` means the header was absent.
    ///
    /// The producer owns this because it owns the KEY and the VALIDITY WINDOW:
    /// the chassis can check a signature only against a key it is handed, and
    /// *"void the moment its turn ends"* is a fact about the producer's turns.
    /// Refuse with [`codes::UNAUTHORIZED`]; the door answers 401.
    fn verify(&self, authorization: Option<&str>) -> Result<Caller, Refusal>;

    /// The surface, with full input schemas. Answered to `tools/list`.
    fn tools(&self) -> Vec<ToolDef>;

    /// Run one tool for one caller. A refusal reaches the model as an
    /// `isError` result; a [`codes::UNKNOWN_METHOD`] refusal is the one case
    /// mapped to a protocol error, because an unknown tool name is the
    /// caller's typo and the spec says `-32602`.
    fn call<'a>(
        &'a self,
        caller: &'a Caller,
        name: &'a str,
        arguments: Value,
    ) -> BoxFut<'a, Result<Value, Refusal>>;
}

/// What one POST body turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// Has an `id`; owed a response.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// No `id`; owed `202 Accepted` and no body.
    Notification { method: String, params: Value },
}

impl Incoming {
    pub fn method(&self) -> &str {
        match self {
            Incoming::Request { method, .. } | Incoming::Notification { method, .. } => method,
        }
    }
}

/// A refusal at the framing layer: an HTTP status and a JSON-RPC error body.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub status: u16,
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(status: u16, code: i64, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    /// The body, with a null id — the framing layer has no request id to
    /// correlate to, or refused before reading one.
    pub fn body(&self) -> Value {
        error_body(Value::Null, self.code, &self.message, None)
    }
}

/// **SORT ONE BODY INTO A REQUEST OR A NOTIFICATION, OR REFUSE IT.**
///
/// A JSON array is refused: batching was removed in `2025-06-18`, and a door
/// that accepted one would have to answer N results in one body, which is the
/// one shape the `application/json` reply cannot carry without becoming a
/// stream. A response object (`result`/`error` with no `method`) is refused
/// too: this door initiates nothing, so there is nothing a client could be
/// responding to.
pub fn parse(body: &[u8]) -> Result<Incoming, RpcError> {
    let raw: Value = serde_json::from_slice(body)
        .map_err(|e| RpcError::new(400, rpc::PARSE, format!("parse error: {e}")))?;
    if raw.is_array() {
        return Err(RpcError::new(
            400,
            rpc::INVALID_REQUEST,
            "JSON-RPC batching is not supported on this door: it was removed in protocol \
             2025-06-18, and one request per POST is the only shape a JSON reply can answer",
        ));
    }
    let Some(obj) = raw.as_object() else {
        return Err(RpcError::new(
            400,
            rpc::INVALID_REQUEST,
            "a JSON-RPC message is an object",
        ));
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(RpcError::new(
            400,
            rpc::INVALID_REQUEST,
            "`jsonrpc` must be \"2.0\"",
        ));
    }
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return Err(RpcError::new(
            400,
            rpc::INVALID_REQUEST,
            "no `method`: this door accepts requests and notifications, never responses \
             — it initiates nothing a client could be answering",
        ));
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    match obj.get("id") {
        None | Some(Value::Null) => Ok(Incoming::Notification {
            method: method.to_string(),
            params,
        }),
        Some(id) if id.is_string() || id.is_number() => Ok(Incoming::Request {
            id: id.clone(),
            method: method.to_string(),
            params,
        }),
        Some(_) => Err(RpcError::new(
            400,
            rpc::INVALID_REQUEST,
            "`id` must be a string or a number",
        )),
    }
}

/// **WHICH VERSION TO ANSWER.** The client's proposal when this door knows
/// it, else [`DEFAULT_PROTOCOL_VERSION`] — the reference server's rule.
pub fn negotiate(proposed: Option<&str>) -> &'static str {
    match proposed {
        Some(p) => PROTOCOL_VERSIONS
            .iter()
            .copied()
            .find(|v| *v == p)
            .unwrap_or(DEFAULT_PROTOCOL_VERSION),
        None => DEFAULT_PROTOCOL_VERSION,
    }
}

/// Is this a version this door speaks? For the request header after
/// `initialize`.
pub fn supports(version: &str) -> bool {
    PROTOCOL_VERSIONS.contains(&version)
}

/// The reply to one POST, transport-agnostic.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub status: u16,
    /// `None` on `202 Accepted`.
    pub body: Option<Value>,
    /// Set on `initialize` only: the session id to echo in [`SESSION_HEADER`].
    pub session: Option<String>,
}

impl Reply {
    fn ok(id: Value, result: Value) -> Self {
        Self {
            status: 200,
            body: Some(result_body(id, result)),
            session: None,
        }
    }
    fn error(id: Value, code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            status: 200,
            body: Some(error_body(id, code, message, data)),
            session: None,
        }
    }
    fn accepted() -> Self {
        Self {
            status: 202,
            body: None,
            session: None,
        }
    }
}

pub fn result_body(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error_body(id: Value, code: i64, message: impl Into<String>, data: Option<Value>) -> Value {
    let mut err = json!({ "code": code, "message": message.into() });
    if let Some(d) = data {
        err["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": err })
}

/// **ANSWER ONE MESSAGE FOR ONE VERIFIED CALLER.**
///
/// `mint_session` is called on `initialize` and nowhere else; it is a closure
/// because the id's randomness (`uuid`) is behind `server` and this function
/// is in the floor.
pub async fn answer(
    tools: &dyn McpTools,
    identity: &Identity,
    caller: &Caller,
    incoming: Incoming,
    mint_session: impl FnOnce() -> String,
) -> Reply {
    let (id, method, params) = match incoming {
        // ── NOTIFICATIONS: 202, no body, whatever the method ─────────────
        //
        // `notifications/initialized` is the one a client sends; any other
        // notification is accepted and ignored, which is what the spec says
        // to do with a notification one does not handle. Refusing it would
        // be answering a message that asked for no answer.
        Incoming::Notification { .. } => return Reply::accepted(),
        Incoming::Request { id, method, params } => (id, method, params),
    };
    match method.as_str() {
        "initialize" => {
            let proposed = params.get("protocolVersion").and_then(Value::as_str);
            let version = negotiate(proposed);
            let mut r = Reply::ok(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": identity.daemon, "version": identity.version },
                    "instructions": format!(
                        "You are thread {}. Every tool here is a façade over one method path on \
                         this daemon; a command acknowledges and the fact arrives on a shape, a \
                         read answers. A refusal is a tool result with isError and a typed code — \
                         read it; it is never an absence.",
                        caller.thread_id
                    ),
                }),
            );
            r.session = Some(mint_session());
            r
        }
        "ping" => Reply::ok(id, json!({})),
        "tools/list" => Reply::ok(id, json!({ "tools": tools.tools() })),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Reply::error(
                    id,
                    rpc::INVALID_PARAMS,
                    "tools/call needs `name` (a string) and `arguments` (an object)",
                    None,
                );
            };
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            match tools.call(caller, name, arguments).await {
                Ok(payload) => Reply::ok(id, tool_result(payload)),
                // **THE SPEC'S ONE PROTOCOL ERROR ON A CALL: NO SUCH TOOL.**
                Err(r) if r.code == codes::UNKNOWN_METHOD => Reply::error(
                    id,
                    rpc::INVALID_PARAMS,
                    format!("unknown tool: {}", r.message),
                    Some(json!({ "code": r.code })),
                ),
                // **EVERY OTHER REFUSAL IS A RESULT THE MODEL READS** — rule
                // 2.7's *"where the caller can see it, never as an absence"*.
                Err(r) => Reply::ok(id, tool_refused(&r)),
            }
        }
        other => Reply::error(
            id,
            rpc::METHOD_NOT_FOUND,
            format!(
                "method not found: {other}. This door answers initialize, ping, tools/list and \
                 tools/call; notifications are accepted; there is no SSE stream, no resources \
                 and no prompts here — facts are shapes, read them on /ws"
            ),
            None,
        ),
    }
}

/// A successful `tools/call` result: the payload as text for the model and as
/// `structuredContent` for a client that reads structure.
pub fn tool_result(payload: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": payload.to_string() }],
        "structuredContent": payload,
        "isError": false,
    })
}

/// A refused `tools/call`, rendered where the model reads it.
pub fn tool_refused(r: &Refusal) -> Value {
    json!({
        "content": [{ "type": "text", "text": format!("refused ({}): {}", r.code, r.message) }],
        "structuredContent": { "refused": { "code": r.code, "message": r.message } },
        "isError": true,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// The HTTP half — behind `server`, with the router that mounts it
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "server")]
pub mod http {
    //! `POST /mcp`, and the 405 for everything else on that path.

    use super::*;
    use crate::server::{Chassis, Producer, AUTHORIZATION};
    use axum::{
        body::Bytes,
        extract::State,
        http::{header, HeaderMap, HeaderValue, StatusCode},
        response::{IntoResponse, Response},
    };

    /// The route this door is mounted on.
    pub const PATH: &str = "/mcp";

    fn json_response(status: u16, body: Value, extra: &[(&'static str, String)]) -> Response {
        let mut r = (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            [(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))],
            body.to_string(),
        )
            .into_response();
        for (k, v) in extra {
            if let Ok(hv) = HeaderValue::from_str(v) {
                r.headers_mut().insert(*k, hv);
            }
        }
        r
    }

    fn refuse(status: u16, code: i64, message: impl Into<String>) -> Response {
        json_response(status, RpcError::new(status, code, message).body(), &[])
    }

    /// **THE DOOR.**
    ///
    /// The order of the checks is the reference server's, with the token
    /// between the transport checks and the body: a caller that asked wrongly
    /// is told so for free, an unauthenticated caller is told so before any
    /// body is parsed, and only a verified caller's body is read.
    pub async fn post<P: Producer>(
        State(c): State<Chassis<P>>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        let producer = c.producer();
        // ── 1. IS THERE A DOOR AT ALL ────────────────────────────────────
        //
        // `unsupported` and 501, on the resource door's reasoning: mounted
        // on every chassis so that a producer with no tools is
        // distinguishable from a chassis with no route.
        let Some(tools) = producer.mcp() else {
            return refuse(
                501,
                rpc::SERVER,
                format!(
                    "{}: this producer exposes no tools on {PATH}; the route exists on \
                     every chassis so that this answer is reachable",
                    codes::UNSUPPORTED
                ),
            );
        };
        // ── 2. THE TRANSPORT CHECKS THE REFERENCE SERVER MAKES ───────────
        let accept = headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !accept.contains("application/json") || !accept.contains("text/event-stream") {
            return refuse(
                406,
                rpc::SERVER,
                "Not Acceptable: Client must accept both application/json and text/event-stream",
            );
        }
        let content_type = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !content_type.contains("application/json") {
            return refuse(
                415,
                rpc::SERVER,
                "Unsupported Media Type: Content-Type must be application/json",
            );
        }
        if let Some(v) = headers.get(PROTOCOL_HEADER).and_then(|v| v.to_str().ok()) {
            if !supports(v) {
                return refuse(
                    400,
                    rpc::SERVER,
                    format!(
                        "Bad Request: Unsupported protocol version: {v} (supported versions: {})",
                        PROTOCOL_VERSIONS.join(", ")
                    ),
                );
            }
        }
        // ── 3. WHO IS CALLING ────────────────────────────────────────────
        let auth = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok());
        let caller = match tools.verify(auth) {
            Ok(c) => c,
            Err(r) => {
                let status = if r.code == codes::UNAUTHORIZED { 401 } else { 500 };
                let body = error_body(
                    Value::Null,
                    rpc::UNAUTHORIZED,
                    format!("{}: {}", r.code, r.message),
                    Some(json!({ "code": r.code })),
                );
                return json_response(
                    status,
                    body,
                    &[(
                        "www-authenticate",
                        format!("Bearer realm=\"{}\"", producer.identity().daemon),
                    )],
                );
            }
        };
        // ── 4. THE MESSAGE ───────────────────────────────────────────────
        let incoming = match parse(&body) {
            Ok(i) => i,
            Err(e) => return json_response(e.status, e.body(), &[]),
        };
        let reply = answer(tools, producer.identity(), &caller, incoming, || {
            uuid::Uuid::new_v4().simple().to_string()
        })
        .await;
        match reply.body {
            None => StatusCode::from_u16(reply.status)
                .unwrap_or(StatusCode::ACCEPTED)
                .into_response(),
            Some(body) => {
                let extra: Vec<(&'static str, String)> = reply
                    .session
                    .into_iter()
                    .map(|s| (SESSION_HEADER, s))
                    .collect();
                json_response(reply.status, body, &extra)
            }
        }
    }

    /// `GET /mcp` and `DELETE /mcp`: this door offers no SSE stream and no
    /// session termination, and says so the way the reference server does.
    pub async fn not_allowed() -> Response {
        let mut r = json_response(
            405,
            error_body(Value::Null, rpc::SERVER, "Method not allowed.", None),
            &[],
        );
        r.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        r
    }
}

// ═════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Claims;

    /// A producer with two tools, one of which refuses.
    struct Two;
    impl McpTools for Two {
        fn verify(&self, authorization: Option<&str>) -> Result<Caller, Refusal> {
            match authorization {
                Some("Bearer ok") => Ok(caller()),
                _ => Err(Refusal::new(codes::UNAUTHORIZED, "no")),
            }
        }
        fn tools(&self) -> Vec<ToolDef> {
            vec![
                ToolDef {
                    name: "agents_log".into(),
                    description: "rows".into(),
                    input_schema: json!({"type":"object","properties":{"agent":{"type":"string"}},"required":["agent"]}),
                },
                ToolDef {
                    name: "agents_spawn".into(),
                    description: "refused".into(),
                    input_schema: json!({"type":"object"}),
                },
            ]
        }
        fn call<'a>(
            &'a self,
            caller: &'a Caller,
            name: &'a str,
            arguments: Value,
        ) -> BoxFut<'a, Result<Value, Refusal>> {
            Box::pin(async move {
                match name {
                    "agents_log" => Ok(json!({ "agent": caller.thread_id, "args": arguments })),
                    "agents_spawn" => Err(Refusal::new(
                        codes::NOT_AVAILABLE_YET,
                        "`agents/spawn` is on the surface and refused",
                    )),
                    other => Err(Refusal::new(codes::UNKNOWN_METHOD, other.to_string())),
                }
            })
        }
    }

    fn caller() -> Claims {
        Claims {
            thread_id: "t-a".into(),
            run_id: "r".into(),
            root: "t-a".into(),
            depth: 0,
            turn: "c".into(),
        }
    }

    fn identity() -> Identity {
        Identity {
            daemon: "threads".into(),
            version: "0.1.0".into(),
            proto: 1,
            boot_id: "b".into(),
        }
    }

    fn req(id: u64, method: &str, params: Value) -> Incoming {
        Incoming::Request {
            id: json!(id),
            method: method.into(),
            params,
        }
    }

    async fn ask(incoming: Incoming) -> Reply {
        answer(&Two, &identity(), &caller(), incoming, || "sess-1".into()).await
    }

    // ── framing ──────────────────────────────────────────────────────────

    /// **A BATCH IS REFUSED, A RESPONSE IS REFUSED, AND THE TWO LEGAL SHAPES
    /// ARE SORTED BY `id`.**
    ///
    /// **Forced red** by making `parse` take the first element of an array:
    /// the batch assertion fails with `Ok(Request { id: 1, … })`.
    #[test]
    fn parse_sorts_requests_from_notifications_and_refuses_batches_and_responses() {
        let r = parse(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        assert_eq!(r, req(1, "ping", Value::Null));
        let n = parse(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        assert_eq!(
            n,
            Incoming::Notification {
                method: "notifications/initialized".into(),
                params: Value::Null
            }
        );
        // A null id is a notification too (JSON-RPC 2.0 §4.1).
        assert!(matches!(
            parse(br#"{"jsonrpc":"2.0","id":null,"method":"x"}"#).unwrap(),
            Incoming::Notification { .. }
        ));
        let batch = parse(br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).unwrap_err();
        assert_eq!((batch.status, batch.code), (400, rpc::INVALID_REQUEST));
        assert!(batch.message.contains("batching"), "{}", batch.message);
        let resp = parse(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#).unwrap_err();
        assert_eq!(resp.code, rpc::INVALID_REQUEST);
        assert!(resp.message.contains("initiates nothing"), "{}", resp.message);
        let garbage = parse(b"not json").unwrap_err();
        assert_eq!((garbage.status, garbage.code), (400, rpc::PARSE));
        let v1 = parse(br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#).unwrap_err();
        assert_eq!(v1.code, rpc::INVALID_REQUEST);
        let bad_id = parse(br#"{"jsonrpc":"2.0","id":{},"method":"ping"}"#).unwrap_err();
        assert_eq!(bad_id.code, rpc::INVALID_REQUEST);
        // The error body correlates to nothing.
        assert_eq!(batch.body()["id"], Value::Null);
        assert_eq!(batch.body()["error"]["code"], json!(rpc::INVALID_REQUEST));
    }

    /// The reference server's rule: echo a known proposal, answer the default
    /// for an unknown one. The installed client proposes `2025-11-25`.
    #[test]
    fn negotiation_echoes_a_known_version_and_answers_the_default_otherwise() {
        assert_eq!(negotiate(Some("2025-06-18")), "2025-06-18");
        assert_eq!(negotiate(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate(Some("2025-11-25")), DEFAULT_PROTOCOL_VERSION);
        assert_eq!(negotiate(Some("1999-01-01")), DEFAULT_PROTOCOL_VERSION);
        assert_eq!(negotiate(None), DEFAULT_PROTOCOL_VERSION);
        assert!(supports("2025-06-18") && !supports("2025-11-25"));
        assert_eq!(PROTOCOL_VERSIONS[0], DEFAULT_PROTOCOL_VERSION, "the default is the newest");
    }

    // ── the five methods ─────────────────────────────────────────────────

    #[tokio::test]
    async fn initialize_answers_the_version_the_capabilities_and_a_session() {
        let r = ask(req(
            0,
            "initialize",
            json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"claude-code","version":"2.1.280"}}),
        ))
        .await;
        assert_eq!(r.status, 200);
        assert_eq!(r.session.as_deref(), Some("sess-1"));
        let body = r.body.unwrap();
        assert_eq!(body["id"], json!(0));
        assert_eq!(body["result"]["protocolVersion"], json!("2025-06-18"));
        assert_eq!(body["result"]["capabilities"], json!({ "tools": {} }));
        assert_eq!(body["result"]["serverInfo"]["name"], json!("threads"));
        assert!(body["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("thread t-a"));
    }

    /// A notification is `202` with no body, whatever it is.
    #[tokio::test]
    async fn a_notification_is_accepted_with_no_body() {
        let r = ask(Incoming::Notification {
            method: "notifications/initialized".into(),
            params: Value::Null,
        })
        .await;
        assert_eq!(r, Reply::accepted());
        let r = ask(Incoming::Notification {
            method: "notifications/whatever".into(),
            params: json!({}),
        })
        .await;
        assert_eq!(r.status, 202);
    }

    #[tokio::test]
    async fn ping_and_tools_list_answer() {
        let r = ask(req(1, "ping", Value::Null)).await;
        assert_eq!(r.body.unwrap()["result"], json!({}));
        let r = ask(req(2, "tools/list", json!({}))).await;
        let tools = r.body.unwrap()["result"]["tools"].clone();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["agents_log", "agents_spawn"]);
        assert!(tools[0].get("inputSchema").is_some(), "the schema rides as `inputSchema`");
    }

    /// **A REFUSAL IS A RESULT THE MODEL READS, NOT A JSON-RPC ERROR** — and
    /// an unknown tool is the one protocol error.
    ///
    /// **Forced red** by mapping every refusal to `Reply::error`: the
    /// `isError` assertion fails with `null` because the body has no `result`.
    #[tokio::test]
    async fn a_tool_refusal_is_a_result_with_is_error_and_an_unknown_tool_is_a_protocol_error() {
        let r = ask(req(3, "tools/call", json!({"name":"agents_spawn","arguments":{"task":"x"}}))).await;
        let body = r.body.unwrap();
        assert_eq!(body["result"]["isError"], json!(true), "{body}");
        assert_eq!(
            body["result"]["structuredContent"]["refused"]["code"],
            json!("not_available_yet")
        );
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("refused (not_available_yet): "), "{text}");
        assert!(body.get("error").is_none(), "not a protocol error: {body}");

        let r = ask(req(4, "tools/call", json!({"name":"nope","arguments":{}}))).await;
        let body = r.body.unwrap();
        assert_eq!(body["error"]["code"], json!(rpc::INVALID_PARAMS), "{body}");
        assert!(body["error"]["message"].as_str().unwrap().starts_with("unknown tool: "));

        let r = ask(req(5, "tools/call", json!({"arguments":{}}))).await;
        assert_eq!(r.body.unwrap()["error"]["code"], json!(rpc::INVALID_PARAMS));
    }

    #[tokio::test]
    async fn a_successful_call_carries_the_payload_as_text_and_as_structure() {
        let r = ask(req(6, "tools/call", json!({"name":"agents_log","arguments":{"agent":"t-a"}}))).await;
        let result = r.body.unwrap()["result"].clone();
        assert_eq!(result["isError"], json!(false));
        assert_eq!(result["structuredContent"]["agent"], json!("t-a"));
        assert_eq!(result["structuredContent"]["args"]["agent"], json!("t-a"));
        let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, result["structuredContent"], "the text IS the structure");
    }

    /// Everything else is `-32601`, and the message says where facts live.
    #[tokio::test]
    async fn every_other_method_is_method_not_found() {
        for m in ["resources/list", "prompts/list", "completion/complete", "logging/setLevel", "shapes/list"] {
            let r = ask(req(7, m, json!({}))).await;
            let body = r.body.unwrap();
            assert_eq!(body["error"]["code"], json!(rpc::METHOD_NOT_FOUND), "{m}: {body}");
            assert!(body["error"]["message"].as_str().unwrap().contains("/ws"), "{m}");
            assert_eq!(body["id"], json!(7), "correlated to the request");
        }
    }
}
