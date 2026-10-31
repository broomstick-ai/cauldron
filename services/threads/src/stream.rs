//! stream-json line parser: one claude `--output-format stream-json` line →
//! zero or more [`ThreadEvent`]s.
//!
//! Ported from CCW `stream.rs` — deliberately **stateless**: it maps a single
//! parsed JSON `Value` to the events it implies; the runtime owns the stateful
//! interpretation (liveness accounting, retry classification, message-id
//! assignment). One upgrade over CCW: `parse_assistant` now reads the
//! Anthropic `message.id` into `AssistantText.api_message_id` (G4 — CCW
//! dropped the only intrinsic per-message identifier the stream carries).
//!
//! Every `type` discriminator has an arm; anything else becomes
//! [`ThreadEvent::Unknown`] so nothing is dropped.
//!
//! # Reading the wire, not the docs
//!
//! Two families of key in this module were written from an older stream-json
//! shape and read null against every line the CLI emits today:
//!
//! * **Casing drift** — `toolUseResult` → `tool_use_result`, and
//!   `compactMetadata.preTokens` → `compact_metadata.pre_tokens`. Both old
//!   spellings occur zero times in the 371,702-line capture under
//!   `lib/threads/turns/`. Handled by [`get_either`], which accepts BOTH rather
//!   than swapping one for the other.
//! * **A key that was never there** — `toolUseResult.toolUseId` does not exist
//!   on any record, in any spelling. The id lives on the sibling `tool_result`
//!   block; see [`first_tool_result_id`].
//!
//! The `system/task_*` family (`task_started`, `task_updated`,
//! `task_notification`, `task_progress`, `background_tasks_changed`) is parsed
//! here for the first time; it was previously the largest single population of
//! `Unknown` rows in the ledger. Its shapes are derived from the capture, and
//! every field is optional unless the capture shows it on every record.

use serde_json::Value;

use crate::event::{BackgroundTask, ThreadEvent};

/// Read the first of several spellings of a key that is present.
///
/// **THE CASING DRIFT.** The CLI's stream-json envelope has moved from
/// camelCase to snake_case for some keys and not others, at a date the capture
/// cannot pin down: the daemon only started writing turn files on 2026-07-30,
/// while the ledger goes back to 2026-04-22. Across the 371,702 captured lines
/// `"toolUseResult"` appears **0** times and `"tool_use_result"` **331**;
/// `"compactMetadata"` **0** and `"compact_metadata"` **15**. Reading only the
/// old spelling silently produced null — that is how `Liveness.outstanding`
/// stayed empty forever and how all ten `compacted` ledger rows lost their
/// trigger and token count.
///
/// This reads EITHER spelling rather than swapping one for the other. The
/// corpus cannot say when the rename happened, so it also cannot say that no
/// live thread still speaks the old one; a reader that accepts both is correct
/// on both sides of a boundary we cannot locate. It costs one failed hash
/// lookup.
///
/// Note the drift is not uniform, which is why this is per-key and not a
/// blanket normalization: the OUTER key `tool_use_result` is snake_case while
/// every key INSIDE it (`agentId`, `isAsync`, `outputFile`) is still camelCase,
/// whereas `compact_metadata` went snake_case inside and out (`pre_tokens`).
fn get_either<'a>(v: &'a Value, spellings: &[&str]) -> Option<&'a Value> {
    spellings.iter().find_map(|k| v.get(*k))
}

fn str_either(v: &Value, spellings: &[&str]) -> Option<String> {
    get_either(v, spellings)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// **True when an event is the synthetic API-error record — in EITHER spelling.**
///
/// # The detector that never fired
///
/// This function used to read `isApiErrorMessage` and `apiErrorStatus`. It sits
/// directly beneath [`get_either`], whose comment documents the casing drift in
/// detail and was written for exactly this hazard — and it did not use it.
///
/// Measured over the operator's turn logs (7,018 `.jsonl` files, 2026-07-30 →
/// 2026-08-26), counting each spelling as a JSON **key**:
///
/// | key                        | files |
/// |----------------------------|-------|
/// | `"isApiErrorMessage":`     | **0** |
/// | `"apiErrorStatus":`        | **0** |
/// | `"is_api_error_message":`  | 4,825 |
/// | `"api_error_status":`      | 6,526 |
///
/// The camelCase spellings occur in the corpus only inside quoted *source
/// code* — `lib/ccw/src/stream.rs`, this very function, pasted into a
/// transcript. The producer has never emitted them. So two of the three clauses
/// were dead from the first line of capture, and `error == "rate_limit"` was
/// carrying the whole detector alone.
///
/// What that hid: 158 `server_error`, 6 `invalid_request` and 4
/// `authentication_failed` records — 168 in all — none of which ever produced a
/// typed API-error event. They fell through into the ordinary text path and
/// reached the ledger as plain `AssistantText`.
///
/// Most were rescued downstream BY ACCIDENT: `classify_errored_result` reads
/// the last assistant text, so a refusal whose prose the classifier happened to
/// recognize still got a verdict. That accident is why this was survivable and
/// why it stayed invisible. It does not cover the 27 records that arrived on
/// turns whose `result` reported `is_error: false` — nothing classified those —
/// and it throws away the HTTP status on every one of them.
///
/// # Both spellings, deliberately
///
/// The corpus cannot say when the producer renamed these keys, so it cannot say
/// no live producer still speaks the old one. Accepting both is correct on
/// either side of a boundary we cannot locate, and it costs one failed hash
/// lookup — [`get_either`]'s own argument, applied where it belonged.
///
/// # This is only HALF the fix
///
/// Detecting more is dangerous on its own, because every detected error used to
/// become a `LimitHit` and the runtime reads every `LimitHit` as a rate limit.
/// See [`is_rate_limit_api_error`] and `retry::classify_api_error`.
pub fn is_api_error(v: &Value) -> bool {
    get_either(v, &["is_api_error_message", "isApiErrorMessage"])
        .and_then(Value::as_bool)
        == Some(true)
        || v.get("error").and_then(Value::as_str) == Some("rate_limit")
        || get_either(v, &["api_error_status", "apiErrorStatus"]).is_some()
}

/// **Of the API errors, the ones that are actually usage limits.**
///
/// The axis. `LimitHit` carries the gauge observation, the pool and the reset
/// prose, and the runtime turns it into `TransientKind::RateLimit` with no
/// guard — so it must be minted ONLY when the refusal really is a limit.
/// Everything else becomes [`crate::event::ThreadEvent::ApiError`].
///
/// Both of the real limit shapes are here. The assistant record names the cause
/// (`error: "rate_limit"`, 4,845 records) and the `result` record names only the
/// status (`api_error_status: 429`, 4,742 records); neither carries the other.
pub fn is_rate_limit_api_error(v: &Value) -> bool {
    v.get("error").and_then(Value::as_str) == Some("rate_limit")
        || api_error_status(v) == Some(429)
}

/// The HTTP status of a synthetic API error, in either spelling.
pub fn api_error_status(v: &Value) -> Option<u64> {
    get_either(v, &["api_error_status", "apiErrorStatus"]).and_then(Value::as_u64)
}

fn u64_at(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Flatten a `message.content` (array of blocks or a bare string) to text.
fn flatten_text(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => {
            let mut buf = String::new();
            for it in items {
                if let Some(t) = it.get("text").and_then(Value::as_str) {
                    if !buf.is_empty() {
                        buf.push(' ');
                    }
                    buf.push_str(t);
                }
            }
            buf
        }
        _ => String::new(),
    }
}

/// Parse one stream-json line (already JSON-decoded) into thread events.
pub fn parse_line(v: &Value) -> Vec<ThreadEvent> {
    let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
    let parent_tool_use_id = str_at(v, "parent_tool_use_id");

    match ty {
        "system" => parse_system(v),
        "assistant" => parse_assistant(v, parent_tool_use_id),
        "user" => parse_user(v, parent_tool_use_id),
        "result" => vec![parse_result(v)],
        "stream_event" => vec![ThreadEvent::StreamEvent { raw: v.clone() }],
        "agent_listing_delta" => vec![ThreadEvent::AgentListingDelta { raw: v.clone() }],
        "rate_limit_event" => vec![parse_rate_limit_event(v)],
        _ => vec![ThreadEvent::Unknown { raw: v.clone() }],
    }
}

/// `{"type":"rate_limit_event","rate_limit_info":{status, resetsAt,
/// rateLimitType, utilization, isUsingOverage}}` — DEC-15's free push gauge,
/// observed verbatim in the 2026-07-27 burn-in and previously swallowed as
/// `Unknown` because this arm did not exist. A line missing `rate_limit_info`
/// stays `Unknown` rather than being coerced into a fabricated reading.
fn parse_rate_limit_event(v: &Value) -> ThreadEvent {
    let Some(info) = v.get("rate_limit_info") else {
        return ThreadEvent::Unknown { raw: v.clone() };
    };
    let Some(utilization) = info.get("utilization").and_then(Value::as_f64) else {
        return ThreadEvent::Unknown { raw: v.clone() };
    };
    let resets_at = info.get("resetsAt").and_then(Value::as_i64);
    ThreadEvent::RateLimitEvent {
        rate_limit_type: str_at(info, "rateLimitType").unwrap_or_default(),
        utilization,
        resets_at,
        resets_at_rfc3339: resets_at.and_then(crate::gauge::epoch_to_rfc3339),
        status: str_at(info, "status"),
        is_using_overage: info
            .get("isUsingOverage")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// One window of a `rate_limit_event`'s `unifiedWindows` map (A-4).
/// `utilization` is the vendor's 0-1 fraction, verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct PushWindow {
    /// The map key as claude sends it (`five_hour`, `seven_day`, …).
    pub name: String,
    pub utilization: f64,
    /// Epoch seconds.
    pub resets_at: Option<i64>,
}

/// **What a `rate_limit_event` says about the ACCOUNT, as opposed to what it
/// says about the gauge** (gap A-4).
///
/// The typed [`ThreadEvent::RateLimitEvent`] carries one window and two flags,
/// and the runtime's arm read only the window. Worse, the shape claude has
/// emitted since at least 2026-09-28 has NO top-level `utilization` at all —
/// it carries `unifiedWindows` instead — so [`parse_rate_limit_event`] keeps
/// that line as `Unknown` (deliberately: it will not fabricate a reading) and
/// the line never reached the runtime as anything it read. Verbatim, from the
/// ledger (`libs/shapewire/seam/conformance/one_thread.jsonl`):
///
/// ```text
/// {"isUsingOverage":false,"overageDisabledReason":"org_level_disabled",
///  "overageStatus":"rejected","rateLimitType":"five_hour","resetsAt":1790631000,
///  "status":"allowed","unifiedWindows":{"five_hour":{"resetsAt":1790631000,
///  "utilization":0.2},"seven_day":{"resetsAt":1791151200,"utilization":0.03}}}
/// ```
///
/// This reads either shape — the WHOLE line, typed or not — so the ledger and
/// the TUI keep exactly the events they had and only the runtime learns more.
/// Note `overageStatus: "rejected"` in that specimen: it means overage is NOT
/// AVAILABLE to the account, and is not a refusal of the request. Only the
/// top-level `status` is.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitPush {
    pub rate_limit_type: String,
    pub status: Option<String>,
    pub is_using_overage: bool,
    /// Top-level `resetsAt` — the reset of `rate_limit_type`'s window.
    pub resets_at: Option<i64>,
    /// Every window in `unifiedWindows`, in key order. Empty for the older
    /// single-window shape.
    pub windows: Vec<PushWindow>,
}

impl RateLimitPush {
    /// Read a raw stream line. `None` unless it is a `rate_limit_event` with a
    /// `rate_limit_info` object.
    pub fn from_line(v: &Value) -> Option<Self> {
        if v.get("type").and_then(Value::as_str) != Some("rate_limit_event") {
            return None;
        }
        let info = v.get("rate_limit_info")?.as_object()?;
        let windows = info
            .get("unifiedWindows")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(name, w)| {
                        Some(PushWindow {
                            name: name.clone(),
                            utilization: w.get("utilization")?.as_f64()?,
                            resets_at: w.get("resetsAt").and_then(Value::as_i64),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(RateLimitPush {
            rate_limit_type: info
                .get("rateLimitType")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            status: info.get("status").and_then(Value::as_str).map(str::to_string),
            is_using_overage: info
                .get("isUsingOverage")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            resets_at: info.get("resetsAt").and_then(Value::as_i64),
            windows,
        })
    }

    /// Does this line bar the account for placement? Paid overage, or a
    /// request the vendor refused. `overageStatus` is NOT consulted.
    pub fn bars(&self) -> bool {
        self.is_using_overage || self.status.as_deref() == Some("rejected")
    }

    /// When the bar should end, epoch seconds: the LATEST reset among windows
    /// the line reports as exhausted (utilization ≥ 1) — the account is on
    /// overage BECAUSE of those, so it stays on overage until the last of them
    /// resets — else the top-level `resetsAt`, else nothing.
    pub fn bar_reset(&self) -> Option<i64> {
        self.windows
            .iter()
            .filter(|w| w.utilization >= 1.0)
            .filter_map(|w| w.resets_at)
            .max()
            .or(self.resets_at)
    }
}

fn parse_system(v: &Value) -> Vec<ThreadEvent> {
    match v.get("subtype").and_then(Value::as_str) {
        Some("init") => vec![ThreadEvent::SystemInit {
            claude_session_id: str_at(v, "session_id"),
            cwd: str_at(v, "cwd"),
            model: str_at(v, "model"),
            tools: v
                .get("tools")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            mcp_servers: v.get("mcp_servers").cloned().unwrap_or(Value::Null),
            permission_mode: str_at(v, "permissionMode"),
            slash_commands: v
                .get("slash_commands")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            api_key_source: str_at(v, "apiKeySource"),
            agents: v.get("agents").cloned().unwrap_or(Value::Null),
        }],
        Some("compact_boundary") => {
            // Both spellings, OUTER and INNER — the drift went all the way down
            // here (`compact_metadata` / `pre_tokens`), unlike
            // `tool_use_result`, whose interior is still camelCase. Reading only
            // `compactMetadata.preTokens` is why every captured compaction
            // recorded `trigger: "unknown"` and a null token count.
            let meta = get_either(v, &["compact_metadata", "compactMetadata"]);
            vec![ThreadEvent::Compacted {
                trigger: meta
                    .and_then(|m| m.get("trigger"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                pre_tokens: meta
                    .and_then(|m| get_either(m, &["pre_tokens", "preTokens"]))
                    .and_then(Value::as_u64),
                // The runtime fills the message-id range (§12) before emit —
                // the stateless parser cannot know it.
                message_id_range: None,
            }]
        }
        Some("task_started") => vec![parse_task_started(v)],
        Some("task_updated") => vec![parse_task_updated(v)],
        Some("task_notification") => vec![parse_task_notification(v)],
        Some("task_progress") => vec![parse_task_progress(v)],
        Some("background_tasks_changed") => vec![parse_background_tasks_changed(v)],
        _ => vec![ThreadEvent::Unknown { raw: v.clone() }],
    }
}

// ── the task lifecycle ──────────────────────────────────────────────────
//
// Five `system` subtypes the CLI has been emitting all along and this parser
// dropped into `Unknown`. Shapes were derived from the 371,702-line capture
// under `lib/threads/turns/`, not from documentation; every field below is
// optional unless the capture shows it on 100% of records, and every arm
// ignores keys it does not recognize so a CLI that adds one does not regress
// this parser.
//
// `task_id` is the one exception — it is present on every captured record of
// all four keyed subtypes, and it is the join key the whole lifecycle hangs
// off. A record without it is not a task record we can use, so it stays
// `Unknown` rather than being admitted with an empty id that would silently
// collide with every other empty id.
//
// NOTHING here carries `parent_tool_use_id` (verified: zero occurrences on
// `type:"system"` lines across the capture). Depth attribution is a join over
// `tool_use_id`, and this module is stateless by contract — see
// `ThreadEvent::TaskNotification` for the join and where it belongs.

/// The `task_id` every keyed task subtype must have, or `None` to fall through
/// to `Unknown`.
fn task_id_of(v: &Value) -> Option<String> {
    v.get("task_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `{total_tokens, tool_uses, duration_ms}` — the same object on both
/// `task_notification` and `task_progress`. Absent entirely on 6,124 of 7,494
/// captured notifications, so a missing block is normal, not an error.
fn task_usage(v: &Value) -> (Option<u64>, Option<u64>, Option<u64>) {
    let Some(u) = v.get("usage") else {
        return (None, None, None);
    };
    (
        u.get("total_tokens").and_then(Value::as_u64),
        u.get("tool_uses").and_then(Value::as_u64),
        u.get("duration_ms").and_then(Value::as_u64),
    )
}

fn parse_task_started(v: &Value) -> ThreadEvent {
    let Some(task_id) = task_id_of(v) else {
        return ThreadEvent::Unknown { raw: v.clone() };
    };
    ThreadEvent::TaskStarted {
        task_id,
        tool_use_id: str_at(v, "tool_use_id"),
        description: str_at(v, "description"),
        // Only `local_agent` records carry these two (1,520 of 7,501).
        subagent_type: str_at(v, "subagent_type"),
        task_type: str_at(v, "task_type"),
        prompt: str_at(v, "prompt"),
        session_id: str_at(v, "session_id"),
    }
}

fn parse_task_updated(v: &Value) -> ThreadEvent {
    let Some(task_id) = task_id_of(v) else {
        return ThreadEvent::Unknown { raw: v.clone() };
    };
    let patch = v.get("patch");
    ThreadEvent::TaskUpdated {
        task_id,
        status: patch.and_then(|p| str_at(p, "status")),
        end_time: patch.and_then(|p| p.get("end_time")).and_then(Value::as_i64),
        error: patch.and_then(|p| str_at(p, "error")),
        is_backgrounded: patch
            .and_then(|p| p.get("is_backgrounded"))
            .and_then(Value::as_bool),
        // The patch is partial BY DESIGN and its key set is not closed (three
        // distinct shapes in 2,407 captured records). Carrying it verbatim is
        // what makes the typed fields above an optimization rather than a lossy
        // filter.
        patch: patch.cloned().unwrap_or(Value::Null),
        session_id: str_at(v, "session_id"),
    }
}

fn parse_task_notification(v: &Value) -> ThreadEvent {
    let Some(task_id) = task_id_of(v) else {
        return ThreadEvent::Unknown { raw: v.clone() };
    };
    let (total_tokens, tool_uses, duration_ms) = task_usage(v);
    ThreadEvent::TaskNotification {
        task_id,
        // Absent on 5 of 7,494 captured records — rare, but it happens, and a
        // notification without a tool link is still a completion.
        tool_use_id: str_at(v, "tool_use_id"),
        status: str_at(v, "status"),
        output_file: str_at(v, "output_file"),
        summary: str_at(v, "summary"),
        total_tokens,
        tool_uses,
        duration_ms,
        session_id: str_at(v, "session_id"),
    }
}

fn parse_task_progress(v: &Value) -> ThreadEvent {
    let Some(task_id) = task_id_of(v) else {
        return ThreadEvent::Unknown { raw: v.clone() };
    };
    let (total_tokens, tool_uses, duration_ms) = task_usage(v);
    ThreadEvent::TaskProgress {
        task_id,
        tool_use_id: str_at(v, "tool_use_id"),
        description: str_at(v, "description"),
        subagent_type: str_at(v, "subagent_type"),
        last_tool_name: str_at(v, "last_tool_name"),
        total_tokens,
        tool_uses,
        duration_ms,
        session_id: str_at(v, "session_id"),
    }
}

fn parse_background_tasks_changed(v: &Value) -> ThreadEvent {
    // A MISSING `tasks` key and an EMPTY one are different facts: empty is the
    // CLI saying "nothing is running now" (402 of 4,046 captured), which is a
    // real level reading. A line with no `tasks` at all is not a roster and
    // must not be reported as an empty one.
    let Some(tasks) = v.get("tasks").and_then(Value::as_array) else {
        return ThreadEvent::Unknown { raw: v.clone() };
    };
    ThreadEvent::BackgroundTasksChanged {
        tasks: tasks
            .iter()
            .map(|t| BackgroundTask {
                task_id: str_at(t, "task_id"),
                task_type: str_at(t, "task_type"),
                description: str_at(t, "description"),
            })
            .collect(),
        session_id: str_at(v, "session_id"),
    }
}

fn parse_assistant(v: &Value, parent: Option<String>) -> Vec<ThreadEvent> {
    // The synthetic API error arrives as an assistant event.
    //
    // THE SPLIT (2026-08-27). This used to mint a `LimitHit` for everything it
    // detected, and `runtime::interpret` reads every `LimitHit` as a rate limit
    // with no guard at all. That was only survivable while the detector was
    // broken in a way that let nothing but rate limits through; repairing the
    // detector without repairing this would have put an expired OAuth session
    // on the usage-limit ladder, waiting for a reset that does not exist.
    //
    // So the limit keeps `LimitHit` — the gauge, the pool axis and the reset
    // prose all hang off it — and every other cause gets `ApiError`, which
    // carries `error` and `status` to `retry::classify_api_error`.
    if is_api_error(v) {
        let text = v.get("message").map(flatten_text).unwrap_or_default();
        let status = api_error_status(v);
        if is_rate_limit_api_error(v) {
            let reset = crate::reset::parse_limit_reset(&text).map(|r| r.raw);
            return vec![ThreadEvent::LimitHit {
                // DEC-15 layer 3: `pool` used to be hardcoded `None` — the
                // information was sitting unused in `text`. It is still `None`
                // when the prose says nothing recognizable; we classify, we do
                // not guess.
                pool: crate::gauge::classify_limit_pool(&text).map(|a| a.as_str().to_string()),
                reset,
                status,
                text,
            }];
        }
        return vec![ThreadEvent::ApiError {
            error: str_at(v, "error"),
            status,
            text,
        }];
    }

    let mut out = Vec::new();
    let Some(msg) = v.get("message") else {
        return vec![ThreadEvent::Unknown { raw: v.clone() }];
    };
    let api_message_id = str_at(msg, "id");

    if let Some(blocks) = msg.get("content").and_then(Value::as_array) {
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = b.get("text").and_then(Value::as_str) {
                        out.push(ThreadEvent::AssistantText {
                            text: t.to_string(),
                            parent_tool_use_id: parent.clone(),
                            api_message_id: api_message_id.clone(),
                        });
                    }
                }
                Some("thinking") => {
                    if let Some(t) = b.get("thinking").and_then(Value::as_str) {
                        out.push(ThreadEvent::Thinking {
                            text: t.to_string(),
                            parent_tool_use_id: parent.clone(),
                        });
                    }
                }
                Some("tool_use") => {
                    let name = b.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    let id = b.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                    let input = b.get("input").cloned().unwrap_or(Value::Null);
                    // Subagent spawn: `Agent` (v2.1.201+) or legacy `Task`.
                    if name == "Agent" || name == "Task" {
                        out.push(ThreadEvent::AgentSpawn {
                            tool_use_id: id.clone(),
                            subagent_type: str_at(&input, "subagent_type"),
                            description: str_at(&input, "description"),
                            run_in_background: input
                                .get("run_in_background")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            // The other half of the parent link. With it, this
                            // one event is a complete index row for the depth
                            // join (`ThreadEvent::TaskNotification`): it names
                            // the tool_use id a notification will quote AND the
                            // tool_use id of whoever made the call.
                            parent_tool_use_id: parent.clone(),
                        });
                    }
                    out.push(ThreadEvent::ToolUse {
                        id,
                        name,
                        input,
                        parent_tool_use_id: parent.clone(),
                    });
                }
                _ => {}
            }
        }
    }

    // Per-message usage (before the turn aggregate).
    //
    // Claude emits one `assistant` line per content block, and EVERY line of a
    // multi-block message repeats the same `message.usage` payload. Carrying
    // `message.id` through is what lets the runtime charge each message's usage
    // exactly once — see `ThreadEvent::MessageUsage`. Dropping the repeat here
    // is not possible: this parser sees one line at a time and holds no state.
    if let Some(u) = msg.get("usage") {
        out.push(ThreadEvent::MessageUsage {
            model: str_at(msg, "model").unwrap_or_else(|| "unknown".into()),
            input_tokens: u64_at(u, "input_tokens"),
            output_tokens: u64_at(u, "output_tokens"),
            cache_read_tokens: u64_at(u, "cache_read_input_tokens"),
            cache_creation_tokens: u64_at(u, "cache_creation_input_tokens"),
            api_message_id: api_message_id.clone(),
        });
    }

    if out.is_empty() {
        out.push(ThreadEvent::Unknown { raw: v.clone() });
    }
    out
}

/// The `tool_use_id` of the first `tool_result` block on a user record.
///
/// The tool-use result object does **not** carry the id: `toolUseId` appears
/// **zero** times inside `tool_use_result` across the whole capture, on any of
/// the 836 records that have an `agentId`. It is a sibling — it lives on the
/// `message.content[]` `tool_result` block, and on every captured launch record
/// there is exactly one such block. Reading it from the result object produced
/// an EMPTY `AgentLaunched.tool_use_id`, which would have made every launch
/// look like the same launch to anything that keys on it.
fn first_tool_result_id(v: &Value) -> Option<String> {
    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)?
        .iter()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .and_then(|b| str_at(b, "tool_use_id"))
}

fn parse_user(v: &Value, parent: Option<String>) -> Vec<ThreadEvent> {
    let mut out = Vec::new();

    // The rich tool-use result distinguishes sync-completed from async-launched
    // Agent results — THE liveness-critical shape, and the one the casing drift
    // hid. `tool_use_result` is what the CLI emits today; `toolUseResult` is the
    // spelling this parser was written against and the one every earlier thread
    // may still be speaking. See `get_either`.
    //
    // Note the interior is still camelCase (`agentId`, `isAsync`, `outputFile`)
    // — only the envelope key moved, so these reads are deliberately NOT
    // spelling-tolerant. Guessing at a snake_case interior nobody has observed
    // would be inventing a shape, not accepting one.
    if let Some(tur) = get_either(v, &["tool_use_result", "toolUseResult"]) {
        let status = str_at(tur, "status").unwrap_or_default();
        let is_async = tur.get("isAsync").and_then(Value::as_bool).unwrap_or(false);
        if let Some(agent_id) = str_at(tur, "agentId") {
            out.push(ThreadEvent::AgentLaunched {
                // `toolUseId` first for the shape this was written against;
                // the sibling `tool_result` block is where it actually is.
                tool_use_id: str_either(tur, &["toolUseId", "tool_use_id"])
                    .or_else(|| first_tool_result_id(v))
                    .unwrap_or_default(),
                agent_id,
                is_async,
                status,
                output_file: str_at(tur, "outputFile"),
            });
        }
    }

    if let Some(blocks) = v.get("message").and_then(|m| m.get("content")).and_then(Value::as_array) {
        for b in blocks {
            if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                out.push(ThreadEvent::ToolResult {
                    tool_use_id: b
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    content: b.get("content").cloned().unwrap_or(Value::Null),
                    is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    parent_tool_use_id: parent.clone(),
                });
            }
        }
    }

    if out.is_empty() {
        out.push(ThreadEvent::Unknown { raw: v.clone() });
    }
    out
}

fn parse_result(v: &Value) -> ThreadEvent {
    let subtype = str_at(v, "subtype").unwrap_or_default();
    // R11.J2: `error_max_structured_output_retries` is the CLI flagging a
    // missing SHAPE, not a failed turn — the work ran. It is read as an ok
    // turn whose structured output is unmet (`retry::is_schema_unmet_result`,
    // docs/STRUCTURED-OUTPUT.md §3.3). The subtype is kept verbatim.
    let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false)
        && !crate::retry::is_schema_unmet_result(&subtype);
    ThreadEvent::ResultTurn {
        subtype,
        is_error,
        total_cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
        num_turns: v.get("num_turns").and_then(Value::as_u64),
        duration_ms: v.get("duration_ms").and_then(Value::as_u64),
        stop_reason: str_at(v, "stop_reason"),
        model_usage: v.get("modelUsage").cloned().unwrap_or(Value::Null),
        structured_output: v.get("structured_output").filter(|x| !x.is_null()).cloned(),
    }
}

/// Parse a raw stream-json line (string). Returns the events, or a single
/// [`ThreadEvent::Unknown`] wrapping the raw text when the line is not valid
/// JSON (never dropped).
pub fn parse_raw_line(line: &str) -> Vec<ThreadEvent> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Value>(line) {
        Ok(v) => parse_line(&v),
        Err(_) => vec![ThreadEvent::Unknown {
            raw: Value::String(line.to_string()),
        }],
    }
}
