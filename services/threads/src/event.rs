//! `ThreadEvent` — the ledger taxonomy the threads daemon persists and streams.
//!
//! Supersedes CCW's `CcwEvent` with the operator's locked contract
//! (design doc §10–§12):
//!
//! - **`UserMessage` carries the FULL prompt text** (Q3: "it should 100% store
//!   the entire chat history"), plus an opaque per-message `metadata` object
//!   (§11: "we have to be able to attach arbitrary metadata to every message" —
//!   e.g. AUI's audio start/end timestamps).
//! - **Message ids are a separate, small per-thread ordinal** carried on the
//!   [`StoredEvent`] envelope (not inside payloads) — the spoken id (oq-msg-id-
//!   format: "keep a small per-thread message ordinal as the operator-facing
//!   ID; store the seq alongside it as the durable join key").
//! - **Liveness is NOT an event.** CCW persisted a heartbeat row every second
//!   (G6); here liveness is a structurally distinct transient frame
//!   ([`Push::Liveness`]) that can never reach the ledger.
//! - **`Custom`** is the application-defined event arm (§11: "consumers can log
//!   their own event types into a thread's ledger").
//! - **`Compacted` records the pre-compaction message-id range** (§12) so an
//!   agent can recover state after an uncontrolled compaction.
//! - **Terminal failures always emit `TurnFailed`** (G8: CCW's attempt
//!   exhaustion emitted nothing).
//!
//! Wire form: internally tagged on `kind`, snake_case, with an `Unknown` arm so
//! nothing is ever dropped (ported from CCW).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How a sent message wants to be delivered (Q4 — the two send intents).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SendIntent {
    /// Default: deliver as soon as no parent turn is in flight — the natural
    /// Claude Code queued-turn behavior. Never interrupts a running turn
    /// mid-flight; CAN start a new turn while async subagents from the prior
    /// turn are still running.
    #[default]
    Immediate,
    /// Deliver only when the agent finishes its current work — parent result
    /// arrived AND every async subagent has gone quiet.
    ///
    /// SEAM (§12 open nuance): with per-turn `-p` spawns there is no mid-turn
    /// pickup. If threads later holds a persistent stream-json session per
    /// active turn, `Immediate` becomes "inject at Claude Code's natural
    /// boundaries" and this intent keeps its wait-for-idle meaning; the wire
    /// contract does not change.
    TurnEnd,
}

impl SendIntent {
    pub fn as_str(&self) -> &'static str {
        match self {
            SendIntent::Immediate => "immediate",
            SendIntent::TurnEnd => "turn_end",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "immediate" => Some(SendIntent::Immediate),
            "turn_end" => Some(SendIntent::TurnEnd),
            _ => None,
        }
    }
}

/// **THE TWO RECORDS of a queued message** (R10.M1, L-8 / E21-4;
/// `docs/MESSAGING-ORCHESTRATION.md` §5.1, §6.2).
///
/// *Sent* is immutable and lives on the ledger as `Custom` events named here.
/// *Whether to send* is one current word on the queue row, `queue.dispatch`,
/// and every change to it is also one of these events, so the row can be
/// rebuilt from the ledger. Until PL-8 these are ledger-A `Custom` events; the
/// protoledger writer (R10.M7) maps each one 1:1 to `app{type, data}`.
pub mod delivery {
    /// An attempt opened on a batch: `{attempt, message_ids, transport,
    /// resend_of}`. Written in the SAME transaction that moves the rows to
    /// `attempted`, and before any byte reaches the process.
    pub const ATTEMPTED: &str = "cauldron.threads/delivery_attempted";
    /// An attempt ended without delivering: `{attempt, message_ids, reason,
    /// dispatch_now}`. `reason` is one of [`REASONS`].
    pub const CANCELLED: &str = "cauldron.threads/delivery_cancelled";
    /// The operator's explicit withdraw: `{message_ids, by, dispatch_now:
    /// "never"}`. The text stays on the ledger and on the row.
    pub const WITHDRAWN: &str = "cauldron.threads/message_withdrawn";
    /// A withdrawn or failed row put back in the dispatcher's view:
    /// `{message_ids, by, dispatch_now}`. Not one of §6.2's three names — §5.1
    /// requires every change to `dispatch` to be a ledger event, and a resend
    /// is one.
    pub const RESENT: &str = "cauldron.threads/message_resent";
    /// **A delivery RE-ENTERED its process** (R13.1 C) — a retry, a re-fired
    /// retry after a restart (L-1), a resumed park, or an adopted turn handed
    /// back to the ladder: `{attempt, message_ids, why, delivered_ids,
    /// resent_ids, withheld_ids, sent, resumed_session, evidence}`. `sent` is
    /// `"continue"` when every id was already delivered and the resumed session
    /// got only a continue note (the original text withheld), `"prompt"` when
    /// the original text went out because it had not been delivered (or the
    /// session could not be resumed). `why` is the newest event that cut the
    /// delivery (`{kind, seq, reason}`). Written after the stdin write, so it
    /// records what was actually sent; `write_error` is set when that failed.
    pub const RESUMED: &str = "cauldron.threads/delivery_resumed";

    /// Every `delivery_cancelled.reason` word (§6.2).
    pub const REASONS: &[&str] = &[
        "operator_cancel",
        "interrupt_mail",
        "restart",
        "turn_failed",
        "retries_exhausted",
    ];

    /// The `queue.dispatch` vocabulary, closed.
    pub const EAGER: &str = "eager";
    pub const TURN_END: &str = "turn_end";
    pub const RIDE: &str = "ride";
    pub const NEVER: &str = "never";
    pub const DISPATCH: &[&str] = &[EAGER, TURN_END, RIDE, NEVER];

    /// The dispatch word a freshly queued row gets from its send intent. The
    /// wire word `immediate` (and its `_bare` form) is `eager`; `turn_end`
    /// stays `turn_end`.
    pub fn dispatch_for_intent(intent: &str) -> &'static str {
        if intent.starts_with("turn_end") {
            TURN_END
        } else {
            EAGER
        }
    }
}

/// One entry of a `system/background_tasks_changed` roster.
///
/// Uniform across all 22,210 captured entries (`task_id`, `task_type`,
/// `description`), but every field is optional anyway: this is a roster the CLI
/// owns and may extend, and a roster entry we cannot fully read is still worth
/// more than a dropped one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BackgroundTask {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// `local_agent` | `local_bash`, free-form for the same reason as
    /// [`ThreadEvent::TaskStarted::task_type`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// **THE NOTICE KIND for an expired login.** Named as a constant because two
/// modules must agree on it and a third must subscribe to it: `runtime` claims
/// the daily window under this key, `store::claim_notice` keys the durable row
/// on it, and the AUDIO INTERFACE subscribes to the event of the same name.
///
/// The wire `kind` on the event itself is `account_login_expired_notice`, which
/// is what a client filters on.
pub const LOGIN_EXPIRED_NOTICE: &str = "account_login_expired_notice";

/// One ledger event. Everything persisted to a thread's event log is one of
/// these; the envelope ([`StoredEvent`]) carries `seq` / `message_id` / `ts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThreadEvent {
    // ── thread lifecycle ────────────────────────────────────────────────
    /// The thread was created; the first event on any thread stream.
    ThreadStarted {
        thread_id: String,
        account: String,
        cwd: String,
        model: String,
    },
    /// The operator (or another client) said something. FULL text, always
    /// (Q3). `metadata` is the caller's opaque blob (wav path, audio times,
    /// anchor ids — the daemon never interprets it, only stores and returns
    /// it). Persisted at ACCEPT time — the ledger records what was said when
    /// it was said; delivery timing is the queue's business.
    UserMessage {
        text: String,
        #[serde(default)]
        intent: SendIntent,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        metadata: Value,
    },
    /// A turn began: claude is being spawned for the listed message ids.
    /// `attempt` > 1 marks a retry of the same delivery.
    TurnStarted {
        attempt: u32,
        /// The per-thread message ids delivered in this turn's prompt.
        message_ids: Vec<u64>,
    },
    /// The turn's claude process exited. Replaces CCW's misleadingly-named
    /// per-turn `SessionEnded`. `exit_code: None` = unknown (e.g. a re-adopted
    /// orphan whose exit status we could not reap).
    TurnEnded { exit_code: Option<i32> },
    /// **A turn was cut off before it could finish, and the ledger says by
    /// WHAT.**
    ///
    /// Two things reach this variant, and a consumer has to be able to tell
    /// them apart:
    ///
    /// * `daemon_restart` — the G1 recovery event. A turn was found dead on
    ///   daemon boot and could not be re-adopted, or a dispatch claimed
    ///   messages and died before its turn started. Nobody asked for it; it is
    ///   a fault, and AUI reports it as one.
    /// * `operator` — the operator asked for it, through `threads.cancel`.
    ///   Nothing failed. AUI lands this as an ordinary item in the thread's
    ///   event list, with no error tone and no spoken error.
    ///
    /// `trigger` is a value of [`InterruptTrigger`] — the ONE interrupt
    /// vocabulary this daemon has, shared with `thread_system_interrupted` and
    /// `ThreadInterruptRow`, deliberately not a second one. It is
    /// `serde(default)` so ledger rows written before the field existed still
    /// parse; every one of those is a daemon-restart interrupt, and an empty
    /// trigger is read as "not the operator" everywhere.
    TurnInterrupted {
        reason: String,
        #[serde(default)]
        trigger: String,
    },
    /// A terminal, non-retried failure (G8: exhaustion always leaves a marker).
    /// `error_kind` is the stable programmatic tag AUI already speaks:
    /// `usage_limit` | `credit_balance` | `transient` | `claude_error`.
    TurnFailed { error_kind: String, message: String },

    // ── system events (ported from CCW) ─────────────────────────────────
    /// `type:"system", subtype:"init"` — the full tool/model/mcp roster.
    SystemInit {
        #[serde(default)]
        claude_session_id: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        tools: Vec<String>,
        #[serde(default)]
        mcp_servers: Value,
        #[serde(default)]
        permission_mode: Option<String>,
        #[serde(default)]
        slash_commands: Vec<String>,
        #[serde(default)]
        api_key_source: Option<String>,
        #[serde(default)]
        agents: Value,
    },
    /// A compaction happened (`system/compact_boundary`). Beyond CCW's shape,
    /// carries the pre-compaction message-id RANGE (§12: "so an agent can
    /// recover state by exploring specific prior messages after an
    /// uncontrolled compaction"). `None` when unknown (imported history).
    Compacted {
        trigger: String,
        #[serde(default)]
        pre_tokens: Option<u64>,
        /// `[first, last]` message ids summarized away by this compaction.
        #[serde(default)]
        message_id_range: Option<(u64, u64)>,
    },
    /// The daemon injected context into the next prompt (post-compaction
    /// restatement, plugin paths, thread identity). Persisted so the ledger
    /// shows exactly what the agent was told and when.
    ContextInjected { text: String },

    // ── assistant content (ported from CCW) ─────────────────────────────
    /// A complete assistant text block. `api_message_id` is the Anthropic
    /// `message.id` CCW dropped (G4).
    AssistantText {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_message_id: Option<String>,
    },
    /// An extended-thinking block.
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
    },
    /// A `tool_use` block.
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
    },
    /// A `tool_result` fed back in (full content, not a preview).
    ToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(default)]
        is_error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
    },
    /// Per-assistant-message usage (streams before the turn aggregate).
    ///
    /// `api_message_id` is the Anthropic `message.id` this usage belongs to,
    /// and it is what makes the line DEDUPABLE. Claude emits one `assistant`
    /// line per CONTENT BLOCK — a thinking block and a text block of the same
    /// message arrive as two lines — and every one of them repeats the same
    /// `message.usage` payload byte for byte (observed 2026-07-29 as events
    /// 7/9 and 16/18 of a single haiku turn). Charging each line double-counted
    /// the turn: 22,462 true tokens metered as 44,862.
    ///
    /// The parser is stateless by design, so it cannot drop the repeat itself;
    /// it carries the identity that lets the runtime's per-turn ingest do it.
    MessageUsage {
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
        /// The Anthropic `message.id`. `None` when the stream omitted it — an
        /// unidentifiable usage line is charged rather than dropped, because
        /// under-charging is the dangerous direction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_message_id: Option<String>,
    },

    // ── subagents (ported from CCW) ─────────────────────────────────────
    /// A `Agent`/`Task` **tool call** was emitted by the assistant. This is the
    /// EARLIEST observation of a subagent — it fires at tool-call time, before
    /// the CLI has assigned the agent an id.
    ///
    /// Not subsumed by [`ThreadEvent::TaskStarted`]: `run_in_background` is
    /// visible only here (the `system/task_started` line does not carry it),
    /// and `parent_tool_use_id` makes this record a self-sufficient index for
    /// the depth-resolving join described on [`ThreadEvent::TaskNotification`].
    /// `runtime::interpret` has no arm for it — it is ledger-only today.
    AgentSpawn {
        tool_use_id: String,
        #[serde(default)]
        subagent_type: Option<String>,
        #[serde(default)]
        description: Option<String>,
        run_in_background: bool,
        /// The tool_use id of the agent that made this call — `None` at the
        /// top level. Half of the parent link; the other half is `tool_use_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
    },
    AgentLaunched {
        tool_use_id: String,
        agent_id: String,
        is_async: bool,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_file: Option<String>,
    },
    /// A tracked async (native, in-process) agent can no longer be running,
    /// because the `claude` process it ran in has EXITED.
    ///
    /// **Since R4.S2 this is a machine fact, not a heuristic.** It used to be
    /// emitted by `runtime::sweep_subagents` when the agent's live-transcript
    /// file stopped changing for `THREADS_SUBAGENT_IDLE_MS` — roughly nine
    /// spurious firings per real completion; that sweep and its poll are
    /// deleted. It is now emitted by `ThreadManager::process_exited` for each
    /// agent still listed when the turn's process exits. It says the agent is
    /// OVER, not how it ended: the CLI's own [`ThreadEvent::TaskNotification`]
    /// carries `status`, and is what normally clears the listing first.
    AgentCompleted { agent_id: String },
    AgentListingDelta { raw: Value },

    // ── task lifecycle (`system/task_*`) ────────────────────────────────
    //
    // The CLI narrates every backgrounded unit of work — subagents AND
    // background Bash — on the parent's stream as `type:"system"` lines. Before
    // these variants existed every one of them fell to `Unknown`: 84,358
    // `system/task_progress`, 7,069 `system/task_started` and 7,049
    // `system/task_notification` rows sat in the ledger as opaque blobs.
    //
    // NONE of these lines carries `parent_tool_use_id` — verified zero across
    // 371,702 captured lines. Depth attribution is a JOIN, and this parser is
    // stateless by contract, so the variants carry the join KEYS and nothing
    // more. See [`ThreadEvent::TaskNotification`] for the join itself.
    /// `system/task_started` — a backgrounded unit of work began.
    ///
    /// `task_type` splits two populations that look identical on the wire:
    /// `local_agent` (1,520 of 7,501 captured; carries `subagent_type` and
    /// `prompt`) and `local_bash` (5,981; carries neither). Background Bash is
    /// therefore observable and distinguishable for the first time.
    ///
    /// `task_id` is the SAME identifier as [`ThreadEvent::AgentLaunched`]'s
    /// `agent_id` — verified: all 836 captured `agentId`s appear as a
    /// `task_started.task_id`. That equality is what lets liveness accounting
    /// close on a real completion instead of on a file mtime.
    TaskStarted {
        task_id: String,
        /// The `tool_use` block that launched it — the link back into the
        /// assistant record, and thus into the parent chain. Absent on 5 of
        /// 7,494 captured notifications, so optional here too.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Present only for `task_type: "local_agent"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_type: Option<String>,
        /// `local_agent` | `local_bash` — kept as a free string, not an enum:
        /// the CLI adds task types without warning and an unknown one must
        /// survive the trip rather than fail the parse.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_type: Option<String>,
        /// The launch instruction, verbatim. Present only for `local_agent`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// `system/task_updated` — a status patch against a live task.
    ///
    /// Carries NO `tool_use_id`; `task_id` is the only handle. The `patch`
    /// object is genuinely partial — of 2,407 captured, 2,095 were
    /// `{status,end_time}`, 183 were `{is_backgrounded}` with no status at all,
    /// and 129 added `error`. Every field is therefore optional, and the raw
    /// `patch` rides along so a shape we have not seen is not lost.
    TaskUpdated {
        task_id: String,
        /// `completed` | `failed` | `killed` — absent on an `is_backgrounded`
        /// patch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end_time: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_backgrounded: Option<bool>,
        /// The patch verbatim — the tolerance hatch for unseen fields.
        #[serde(default, skip_serializing_if = "Value::is_null")]
        patch: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// `system/task_notification` — **the terminal signal**: this task is over
    /// and here is its result.
    ///
    /// This is the event the daemon has been missing. It is the CLI's own word
    /// on completion (`status`: `completed` | `failed` | `stopped`), it names
    /// the output file, it carries the result summary, and it reports the
    /// task's total spend. Nine hundred and ninety-nine times out of a thousand
    /// it arrives on the parent's stream DURING the launching turn — 802 of 805
    /// captured async launches were notified inside the same turn file.
    ///
    /// **The attribution join** (performed by a stateful consumer, never here):
    /// `tool_use_id` → the assistant record that emitted that `tool_use.id` →
    /// that record's `parent_tool_use_id` → look it up in the `TaskStarted`
    /// index by `tool_use_id` → the parent's `task_id`. Recurse for depth.
    /// [`ThreadEvent::ToolUse`] and [`ThreadEvent::AgentSpawn`] both carry the
    /// `(id, parent_tool_use_id)` pair the middle step needs, so every key the
    /// join wants is already on the wire.
    TaskNotification {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        /// `completed` | `failed` | `stopped`. Optional because a status we
        /// cannot read must not be silently reported as a success.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_file: Option<String>,
        /// The result text the parent will act on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        /// Present on 1,370 of 7,494 captured notifications.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_uses: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// `system/task_progress` — a live heartbeat for a running task.
    ///
    /// Highest-volume line the CLI emits (88,511 of 371,702 captured, 24%).
    /// It was ALREADY being persisted, as `Unknown`, so typing it costs no
    /// ledger volume and buys the only per-subagent liveness signal that is not
    /// a file mtime: a real heartbeat, from the CLI, naming the tool the agent
    /// is running and its spend so far.
    TaskProgress {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        /// The current activity line, e.g. `"Reading skills/SKILL.md"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_uses: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// `system/background_tasks_changed` — the CLI's full roster of currently
    /// backgrounded tasks, re-broadcast on every change.
    ///
    /// A LEVEL signal, where the other three are edges: it says what is running
    /// now rather than what just happened, so a consumer that missed an edge can
    /// resynchronize from it. The array is legitimately empty (402 of 4,046
    /// captured) — that is the "everything finished" broadcast, not a parse
    /// failure.
    BackgroundTasksChanged {
        tasks: Vec<BackgroundTask>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },

    // ── raw passthrough (nothing dropped) ───────────────────────────────
    StreamEvent { raw: Value },
    Unknown { raw: Value },

    // ── turn result / telemetry ─────────────────────────────────────────
    ResultTurn {
        subtype: String,
        is_error: bool,
        #[serde(default)]
        total_cost_usd: Option<f64>,
        #[serde(default)]
        num_turns: Option<u64>,
        #[serde(default)]
        duration_ms: Option<u64>,
        #[serde(default)]
        stop_reason: Option<String>,
        #[serde(default)]
        model_usage: Value,
        /// The `result` line's `structured_output`, when the turn ran with
        /// `--json-schema` and the model called the tool (R11.J2). ABSENT from
        /// the line, not null, when there is none (R11.J1 PROBE-NOTE). Only a
        /// candidate: see [`judge_structured`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        structured_output: Option<Value>,
    },

    // ── governance / retry meta-events ──────────────────────────────────
    /// A rate/usage limit was hit (surfaced the moment it streams).
    LimitHit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pool: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset: Option<String>,
        #[serde(default)]
        status: Option<u64>,
        #[serde(default)]
        text: String,
    },
    /// **A synthetic API error that is NOT a usage limit — the class that had
    /// nowhere to land.**
    ///
    /// The CLI emits one record shape for every API-level failure: an assistant
    /// message with `is_api_error_message: true`, an `error` naming the cause,
    /// and sometimes an `api_error_status`. Until 2026-08-27 the parser turned
    /// every one it recognized into [`ThreadEvent::LimitHit`], and the runtime
    /// converts a `LimitHit` into `TransientKind::RateLimit` unconditionally.
    /// A single variant therefore could not represent an expired login, an
    /// overload and a rejected prompt without lying about two of them.
    ///
    /// This is the other half of the split. `LimitHit` keeps the cases that
    /// really are limits — it is what feeds the gauge, the pool axis and the
    /// reset prose — and everything else arrives here with its cause intact,
    /// for `retry::classify_api_error` to put on the right ladder.
    ///
    /// Measured over the operator's turn logs (7,018 files, 2026-07-30 →
    /// 2026-08-26): 158 `server_error`, 6 `invalid_request` and 4
    /// `authentication_failed` records — 168 in total — none of which ever
    /// produced a typed API-error event. They fell through the broken detector
    /// into the ordinary text path and reached the ledger as plain
    /// `AssistantText`, indistinguishable from something the model said.
    ///
    /// That accident is also what limited the damage: `classify_errored_result`
    /// reads the last assistant text, so a refusal whose PROSE the classifier
    /// recognized still got a verdict. The 27 records that arrived on turns
    /// whose `result` reported `is_error: false` had no such rescue and were
    /// classified by nothing at all.
    ApiError {
        /// The CLI's own name for the cause — `server_error`,
        /// `invalid_request`, `authentication_failed`. `None` when the record
        /// carried only a status.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// The HTTP status, when one was given. Read from `api_error_status`
        /// AND `apiErrorStatus` — see `stream::get_either` on the casing drift.
        #[serde(default)]
        status: Option<u64>,
        /// The refusal prose, verbatim. It is the fallback diagnosis when the
        /// named cause is one this crate has never seen.
        #[serde(default)]
        text: String,
    },
    /// The `rate_limit_event` stream line, first-classed (DEC-15 layer 1).
    /// Real claude 2.1.220 pushes this inline on every turn; it used to fall
    /// through to `Unknown` and be lost to the gauge. `utilization` is kept
    /// VERBATIM as the 0-1 fraction claude sends (the `/usage` prose is
    /// 0-100 — the normalization every consumer must handle); `resets_at` is
    /// the raw epoch seconds, with `resets_at_rfc3339` resolved alongside it.
    RateLimitEvent {
        rate_limit_type: String,
        utilization: f64,
        #[serde(default)]
        resets_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resets_at_rfc3339: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        is_using_overage: bool,
    },

    // ── budgets (§15) ───────────────────────────────────────────────────
    /// A budget meter exhausted (DEC-16: either meter trips it). Pushed as a
    /// first-class thread event so a tripped budget rides AUI's existing
    /// event stream (DEC-17) — AUI keeps no meter of its own (DEC-7).
    BudgetTripped {
        budget_key: String,
        /// `session` | `weekly_all`.
        axis: String,
        consumed_pct: f64,
        limit_pct: f64,
        /// `kill` | `finish` (DEC-13).
        action: String,
    },
    /// The turn was PARKED, not dropped (CON-4). `reason` is queryable prose;
    /// `cause` is the programmatic tag (`threshold` | `budget` | `internal` |
    /// `login` | every other [`crate::budgets::ParkCause`]).
    ///
    /// `login` is the odd one and is worth naming here: it is not a budget
    /// decision at all. It means the thread's account resolves to NO LOGIN
    /// DIRECTORY, so running the turn would leave `CLAUDE_CONFIG_DIR` unset —
    /// i.e. spawn it against the operator's own terminal login. The turn is
    /// refused rather than served on the wrong account; see
    /// `ThreadManager::spawn_login` and `docs/spawn-config-dir-guard.md`.
    TurnParked {
        cause: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_key: Option<String>,
        #[serde(default)]
        message_ids: Vec<u64>,
    },
    /// **The budgets module itself failed** (round-7 item 1). Enforcement fails
    /// CLOSED: on an internal error nothing runs except threads whose budgeting
    /// is explicitly turned off, so this event is the loud surface of a
    /// condition that would otherwise look like an inexplicably stuck daemon.
    /// `phase` says where it happened (`admission` | `charge` | `trip_check`),
    /// `outcome` says what the daemon did about it (`parked` | `bypassed` |
    /// `logged`). The operator's remedy path is `budgeting_off` on a thread.
    BudgetsFailed {
        phase: String,
        error: String,
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_key: Option<String>,
    },
    /// A parked turn became admissible again (usage reset, or a config edit)
    /// and is being delivered.
    TurnResumed {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_key: Option<String>,
        #[serde(default)]
        message_ids: Vec<u64>,
    },
    /// An in-flight execution was torn down by a budget trip (DEC-13/DIR-19).
    /// `strategy` names the teardown used, so M-9 can compare strategies from
    /// the ledger alone.
    TurnKilled {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_key: Option<String>,
        strategy: String,
    },

    /// A retry was scheduled with a PERSISTED deadline (Q5/G10: retries are
    /// owned by threads and survive a daemon restart). Replaces CCW's
    /// in-memory-only `RetryStarted`.
    RetryScheduled {
        error_kind: String,
        reason: String,
        attempt: u32,
        /// RFC3339 UTC instant the retry fires.
        retry_at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset_prose: Option<String>,
    },
    /// **A usage-limit retry has stopped looking like waiting for a reset.**
    ///
    /// The deliberate twin of `update.rs`'s `starving` ledger line, for the
    /// deliberate twin of its failure: a mechanism that is technically still
    /// working, indefinitely, with nothing loud enough to notice. There a
    /// restart deferred for an hour and left one line; here a retry reached
    /// `attempt = 924` on one delivery and left only `retry_scheduled` rows,
    /// which look identical at attempt 3 and attempt 900.
    ///
    /// Emitted while the retry is still ARMED — this is not the give-up, it is
    /// the warning that precedes it. `give_up_at` says where the ceiling is, so
    /// the row answers "how bad is this" without the reader having to know the
    /// policy.
    RetryStarving {
        error_kind: String,
        reason: String,
        /// Attempts spent on this ONE delivery so far. Every one of them was a
        /// real `claude` process that ran and was refused.
        attempt: u32,
        /// The ceiling this retry will be given up at.
        give_up_at: u32,
        /// The login this retry is floored behind — what it is starving ON, in
        /// the same sense `update.rs` reports the turns a restart starves
        /// behind.
        account: String,
        /// RFC3339 UTC instant the next attempt is due.
        retry_at: String,
        /// RFC3339 UTC instant the FIRST attempt of this delivery was armed.
        /// Survives every re-arm, so it measures the ladder rather than the rung.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<String>,
    },
    /// **A LOGIN HAS EXPIRED AND WORK IS PILING UP BEHIND IT — say so, once a
    /// day, out loud.**
    ///
    /// The operator rejected the ticket-filing alarm this replaces:
    ///
    /// > *"I don't really need an alarm... Sounds like it's just going to nag
    /// > me, and I don't want to be nagged. Maybe just, like, if there's an
    /// > attempt to place on a thread, you can notify me in the audio user
    /// > interface, but I don't want to be notified more than once per day."*
    ///
    /// So this is not an alarm and not a ticket. It is one event on the stream
    /// the audio interface already subscribes to, emitted when a placement
    /// actually fails on an expired credential — never on a timer, never on a
    /// schedule, and never more than once per day per login.
    ///
    /// **The rate limit is enforced in THREADS, in `store::claim_notice`, not
    /// in the client.** A limit held per-connection is not a limit (a second
    /// listener doubles it) and a limit held in memory is not one either (this
    /// daemon restarts itself on every new binary, and an expired login is a
    /// state only he can leave, so an in-memory window would re-open on every
    /// boot — the daemon would nag him precisely because it was trying not to).
    ///
    /// `suppressed_since` is why the rate limit does not lose information: a
    /// login expired for a day may have blocked forty placements, and the one
    /// notice that gets through says so rather than presenting a persistent
    /// failure as a single event.
    ///
    /// `speakable` is a complete sentence, synthesised here, for an interface
    /// whose job is to READ IT ALOUD and which must not have to assemble prose
    /// from fields. Never vendor text: a credential error's own wording is the
    /// thing this crate scrubs off transcripts, and it is not going to be piped
    /// to a speaker instead.
    AccountLoginExpiredNotice {
        /// The account whose placement failed.
        account: String,
        /// Every label of the same login — all of them are barred together, and
        /// naming one of five would understate the damage.
        labels: Vec<String>,
        /// RFC3339 UTC: when the bar lifts on its own for one probing turn.
        /// NOT a promise that the login works again then.
        bar_until: String,
        /// Placement failures on this login swallowed since the last notice.
        suppressed_since: u64,
        /// One sentence, ready to speak.
        speakable: String,
    },
    /// The thread switched accounts (usage-limit failover per retry policy).
    AccountSwitched {
        from: String,
        to: String,
        reason: String,
    },
    /// The thread's settings blob changed mid-thread (Q9/Q10 audit trail).
    SettingsChanged { patch: Value },
    /// The effective plugin set changed between turns (hot-reload observation;
    /// §12: updated skills reach the next turn immediately).
    PluginsChanged {
        added: Vec<String>,
        removed: Vec<String>,
        modified: Vec<String>,
    },

    // ── threads' own system events — DEC-55 category (b) ────────────────
    /// **A HARD BREAK interrupted this thread** (DEC-56). The one event the
    /// service owes the thread because "it can't help it": the conversation
    /// was cut off to move it, so it is told so.
    ///
    /// `trigger` is the programmatic tag naming WHICH gate fired —
    /// `session_limit` | `weekly_limit` | `fable_limit` | `threshold` |
    /// `hard_budget` (see [`InterruptTrigger`]). `switched` is DEC-55's
    /// literal ask: "a flag indicating whether a switchover was done".
    /// `detail` is one short factual clause naming the specific budget or
    /// threshold, always SYNTHESIZED by the service (`limit_detail`) and never
    /// vendor prose.
    ///
    /// **NONE OF `trigger`, `detail`, `switched` OR `to_account` REACHES THE
    /// MODEL.** They are ledger fields. DEC-56's permission to render the cause
    /// was superseded on 2026-08-17 (see [`InterruptTrigger`] and
    /// [`crate::context::render_model_notice`]); the model is told only THAT it
    /// was interrupted and WHEN. CON-22 still forbids a reset instant anywhere
    /// in this record, and [`crate::context::guard_injection`] is still the
    /// structural backstop.
    ThreadSystemInterrupted {
        trigger: String,
        #[serde(default)]
        switched: bool,
        #[serde(default)]
        detail: String,
        /// RFC3339 UTC instant the interruption happened.
        at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_account: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_account: Option<String>,
    },
    /// The interrupted thread was automatically resumed.
    ///
    /// This used to be described as "WHY, WHEN, and the ELAPSED duration". The
    /// WHY is gone from the model's copy as of the 2026-08-17 supersession of
    /// DEC-56: what is rendered is THAT it was interrupted and resumed, WHEN,
    /// and — the load-bearing part, which stays — the ELAPSED duration,
    /// computed by the system because a model cannot reliably subtract two
    /// timestamps. `trigger`, `detail`, `switched` and `to_account` remain on
    /// the record for the ledger, and are not rendered.
    ThreadSystemResumed {
        trigger: String,
        #[serde(default)]
        detail: String,
        interrupted_at: String,
        resumed_at: String,
        elapsed_seconds: i64,
        /// The elapsed figure already rendered in days/hours/minutes/seconds.
        elapsed: String,
        #[serde(default)]
        switched: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_account: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_account: Option<String>,
    },
    /// A turn was REQUESTED but could not be STARTED, and has now started
    /// (DEC-57). `interrupted` is ALWAYS false and is carried explicitly:
    /// nothing was interrupted, it never began, and setting the flag true here
    /// would make a model distrust a state that is in fact intact.
    ThreadSystemQueued {
        /// RFC3339 UTC — when the operator's message was accepted.
        submitted_at: String,
        /// RFC3339 UTC — when the turn actually started.
        queued_until: String,
        queued_for_seconds: i64,
        /// The wait already rendered in days/hours/minutes/seconds.
        queued_for: String,
        /// DEC-57: always `false`.
        #[serde(default)]
        interrupted: bool,
        #[serde(default)]
        message_ids: Vec<u64>,
    },

    // ── application-defined events (§11) ────────────────────────────────
    /// A client-appended custom event. `event_type` is the consumer's own
    /// vocabulary (e.g. AUI's `summary_updated`, `failed_input`); `data` is
    /// opaque. Addressable customs get a `message_id` on the envelope.
    ///
    /// **DEC-55 category (a).** This is the application's own bookkeeping and
    /// it is [`Audience::LedgerOnly`] by construction — it can never reach a
    /// model's context, no matter which client writes it or what it contains.
    Custom { event_type: String, data: Value },
}

/// WHICH gate fired on a hard break — **a LEDGER fact, and only a ledger
/// fact.**
///
/// This doc comment used to read: *"Naming the gate is PERMITTED (DEC-56)
/// because it is a fact about a past event."* **That permission is SUPERSEDED
/// as of 2026-08-17 and must not be restored from this comment.** The operator:
///
/// > *"I no longer want any usage limits to be told to the agents at all… we
/// > shouldn't ever give the model the ability to see what its resource limit
/// > is."*
///
/// The distinction DEC-55/56 drew — history may be told, prediction may not —
/// was the right line for the RESET TIME and is still the right line for the
/// reset time (CON-22, unchanged and unweakened). It was the wrong line for the
/// TRIGGER, because a model told "weekly_limit" does not treat it as history:
/// it plans against the window it infers behind it. On 2026-08-14 one
/// downgraded itself to a cheaper model for three days on exactly that
/// inference.
///
/// So this enum keeps every variant and keeps being recorded on every
/// `thread_system_interrupted` row — the operator and his tooling need it — and
/// [`crate::context::render_model_notice`] no longer renders it into context.
/// If a metacognitive system is ever built that is meant to balance its own
/// resources, THAT is the change that reopens this, and it reopens it at the
/// renderer, deliberately, not by a comment here going stale.
/// **ONE vocabulary, not two.** The five limit/budget triggers below are the
/// gates that fire on a HARD BREAK and are recorded on
/// `thread_system_interrupted` / `ThreadInterruptRow`. [`Operator`] and
/// [`DaemonRestart`] name the two things that cut a TURN off
/// (`turn_interrupted`) and are the same vocabulary deliberately: a second enum
/// for "why was this interrupted" would have to be kept in step with this one
/// forever, and every consumer would have to learn which of the two it was
/// reading.
///
/// [`Operator`]: InterruptTrigger::Operator
/// [`DaemonRestart`]: InterruptTrigger::DaemonRestart
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptTrigger {
    SessionLimit,
    WeeklyLimit,
    FableLimit,
    Threshold,
    HardBudget,
    /// The operator asked for it (`threads.cancel`). **Not a fault** — the one
    /// trigger in this enum that names an intention rather than a gate.
    Operator,
    /// The daemon died or restarted with a turn in flight and the execution
    /// could not be re-adopted (G1).
    DaemonRestart,
    /// **An automatic compaction happened and this thread is not permitted to
    /// resume through one** (`ThreadSettings::resume_after_compaction`, false by
    /// default).
    ///
    /// Like [`Operator`], this names an INTENTION rather than a gate — it is the
    /// operator's standing instruction, applied at the moment it becomes
    /// relevant, and the second `true` in `retry::ends_the_delivery`. Nothing
    /// failed and nothing refused: the turn did work, the conversation was
    /// compacted, and the thread was stopped there deliberately rather than
    /// allowed to carry on from a summary it does not accurately remember.
    ///
    /// It is a THIRD case in this enum rather than a reuse of `Operator`
    /// precisely so a reader can tell "he cancelled this" from "it hit a
    /// compaction" without consulting the reason prose.
    ///
    /// [`Operator`]: InterruptTrigger::Operator
    Compaction,
}

impl InterruptTrigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            InterruptTrigger::SessionLimit => "session_limit",
            InterruptTrigger::WeeklyLimit => "weekly_limit",
            InterruptTrigger::FableLimit => "fable_limit",
            InterruptTrigger::Threshold => "threshold",
            InterruptTrigger::HardBudget => "hard_budget",
            InterruptTrigger::Operator => "operator",
            InterruptTrigger::DaemonRestart => "daemon_restart",
            InterruptTrigger::Compaction => "compaction",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "session_limit" => Some(InterruptTrigger::SessionLimit),
            "weekly_limit" => Some(InterruptTrigger::WeeklyLimit),
            "fable_limit" => Some(InterruptTrigger::FableLimit),
            "threshold" => Some(InterruptTrigger::Threshold),
            "hard_budget" => Some(InterruptTrigger::HardBudget),
            "operator" => Some(InterruptTrigger::Operator),
            "daemon_restart" => Some(InterruptTrigger::DaemonRestart),
            "compaction" => Some(InterruptTrigger::Compaction),
            _ => None,
        }
    }
}

/// **THE AUDIENCE SPLIT (DEC-55).** Every event is RECORDED; only some are
/// ever REPLAYED into the model's context, and which is which is a property of
/// the event KIND — not a filter applied at one call site that the next call
/// site forgets.
///
/// This is the whole design: an account-level usage-limit error became a fact
/// a model believed about itself because a thread's event stream was one
/// undifferentiated record and everything in it was a candidate for context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// Recorded, queryable by clients, **never rendered into model context.**
    /// The default for anything not deliberately promoted.
    LedgerOnly,
    /// DEC-55 category (b) — threads' own system events, the only ones that
    /// may ever reach the model, and only through
    /// [`crate::context::assemble_model_notices`].
    Model,
}

impl Audience {
    pub fn as_str(&self) -> &'static str {
        match self {
            Audience::LedgerOnly => "ledger_only",
            Audience::Model => "model",
        }
    }
    pub fn reaches_model(&self) -> bool {
        matches!(self, Audience::Model)
    }
}

/// The kinds that MAY be replayed into model context — the whole of category
/// (b), listed once so a test can pin it and a reader can audit it in a
/// glance. Everything else in [`ThreadEvent::kind`] is `LedgerOnly`.
///
/// **A kind being listed here does NOT mean its FIELDS reach the model.** The
/// list gates the EVENT; [`crate::context::render_model_notice`] gates the
/// CONTENT, and since the 2026-08-17 supersession of DEC-56 it renders no
/// cause at all for the two interrupt kinds — no trigger, no detail, no
/// account, no switchover flag. Adding a kind here is therefore not sufficient
/// to surface anything, and removing a field from a struct is not the way to
/// stop surfacing it. Two gates, on purpose.
pub const MODEL_AUDIENCE_KINDS: &[&str] = &[
    "thread_system_interrupted",
    "thread_system_queued",
    "thread_system_resumed",
];

/// **EVERY LEDGER KIND, CLOSED** — the words [`ThreadEvent::kind`] can answer,
/// in the match's own order. The `threads/log` shape declares its rows' `kind`
/// as this vocabulary (`Shape::CELLS`) and the `threads` shape its `newest.kind`,
/// so a consumer that compares a row against a word is checked against the
/// producer's list and not against its own memory of it.
///
/// `tests/cell_manifest.rs::the_ledger_kind_vocabulary_is_the_match` pins this
/// list against the match arms, so adding a variant without adding its word is
/// a red test rather than a word consumers cannot see coming.
pub const KINDS: &[&str] = &[
    "thread_started",
    "user_message",
    "turn_started",
    "turn_ended",
    "turn_interrupted",
    "turn_failed",
    "system_init",
    "compacted",
    "context_injected",
    "assistant_text",
    "thinking",
    "tool_use",
    "tool_result",
    "message_usage",
    "agent_spawn",
    "agent_launched",
    "agent_completed",
    "agent_listing_delta",
    "task_started",
    "task_updated",
    "task_notification",
    "task_progress",
    "background_tasks_changed",
    "stream_event",
    "unknown",
    "result_turn",
    "limit_hit",
    "api_error",
    "rate_limit_event",
    "budget_tripped",
    "turn_parked",
    "budgets_failed",
    "turn_resumed",
    "turn_killed",
    "retry_scheduled",
    "retry_starving",
    "account_login_expired_notice",
    "account_switched",
    "settings_changed",
    "plugins_changed",
    "thread_system_interrupted",
    "thread_system_resumed",
    "thread_system_queued",
    "custom",
];

impl ThreadEvent {
    /// The variant tag (matches serialized `kind`) — the ledger's cheap-filter
    /// column.
    pub fn kind(&self) -> &'static str {
        match self {
            ThreadEvent::ThreadStarted { .. } => "thread_started",
            ThreadEvent::UserMessage { .. } => "user_message",
            ThreadEvent::TurnStarted { .. } => "turn_started",
            ThreadEvent::TurnEnded { .. } => "turn_ended",
            ThreadEvent::TurnInterrupted { .. } => "turn_interrupted",
            ThreadEvent::TurnFailed { .. } => "turn_failed",
            ThreadEvent::SystemInit { .. } => "system_init",
            ThreadEvent::Compacted { .. } => "compacted",
            ThreadEvent::ContextInjected { .. } => "context_injected",
            ThreadEvent::AssistantText { .. } => "assistant_text",
            ThreadEvent::Thinking { .. } => "thinking",
            ThreadEvent::ToolUse { .. } => "tool_use",
            ThreadEvent::ToolResult { .. } => "tool_result",
            ThreadEvent::MessageUsage { .. } => "message_usage",
            ThreadEvent::AgentSpawn { .. } => "agent_spawn",
            ThreadEvent::AgentLaunched { .. } => "agent_launched",
            ThreadEvent::AgentCompleted { .. } => "agent_completed",
            ThreadEvent::AgentListingDelta { .. } => "agent_listing_delta",
            ThreadEvent::TaskStarted { .. } => "task_started",
            ThreadEvent::TaskUpdated { .. } => "task_updated",
            ThreadEvent::TaskNotification { .. } => "task_notification",
            ThreadEvent::TaskProgress { .. } => "task_progress",
            ThreadEvent::BackgroundTasksChanged { .. } => "background_tasks_changed",
            ThreadEvent::StreamEvent { .. } => "stream_event",
            ThreadEvent::Unknown { .. } => "unknown",
            ThreadEvent::ResultTurn { .. } => "result_turn",
            ThreadEvent::LimitHit { .. } => "limit_hit",
            ThreadEvent::ApiError { .. } => "api_error",
            ThreadEvent::RateLimitEvent { .. } => "rate_limit_event",
            ThreadEvent::BudgetTripped { .. } => "budget_tripped",
            ThreadEvent::TurnParked { .. } => "turn_parked",
            ThreadEvent::BudgetsFailed { .. } => "budgets_failed",
            ThreadEvent::TurnResumed { .. } => "turn_resumed",
            ThreadEvent::TurnKilled { .. } => "turn_killed",
            ThreadEvent::RetryScheduled { .. } => "retry_scheduled",
            ThreadEvent::RetryStarving { .. } => "retry_starving",
            ThreadEvent::AccountLoginExpiredNotice { .. } => "account_login_expired_notice",
            ThreadEvent::AccountSwitched { .. } => "account_switched",
            ThreadEvent::SettingsChanged { .. } => "settings_changed",
            ThreadEvent::PluginsChanged { .. } => "plugins_changed",
            ThreadEvent::ThreadSystemInterrupted { .. } => "thread_system_interrupted",
            ThreadEvent::ThreadSystemResumed { .. } => "thread_system_resumed",
            ThreadEvent::ThreadSystemQueued { .. } => "thread_system_queued",
            ThreadEvent::Custom { .. } => "custom",
        }
    }

    /// **DID THE OPERATOR PUT WORDS INTO THIS THREAD?** — the ONE classifier
    /// behind thread RECENCY, and the reason it is a method here rather than a
    /// `matches!` at each site.
    ///
    /// Operator directive, verbatim: *"the recents need to be ordered on last
    /// user input, not on when they were created or any other events, including
    /// renames."* That order has two halves which must never be able to
    /// disagree — the live fold in [`crate::shapes::recent`], which reorders the
    /// index, and `threads.last_user_input_at`, the column
    /// [`crate::store::Store::recent_threads`] hydrates that order back out of.
    /// Both ask this question, and because they ask it through one function
    /// there is no arrangement of this codebase in which one of them counts an
    /// event the other does not.
    ///
    /// **It is `user_message` and nothing else.** Not `turn_started`, which is
    /// the daemon acting on what he said; not `context_injected`, which is the
    /// daemon speaking FOR him; not `thread_started`, which is creation and is
    /// named in the directive's own exclusions. The matching durable filter is
    /// `kind = 'user_message'`, which is [`Self::kind`]'s string for this
    /// variant — the one place the two spellings meet.
    pub fn is_user_message(&self) -> bool {
        matches!(self, ThreadEvent::UserMessage { .. })
    }

    /// **THE PER-KIND REPLAY AUDIT (DEC-55).** Whether this event may ever be
    /// rendered into the model's context.
    ///
    /// Deliberately an EXHAUSTIVE match with no `_` arm: a new variant does not
    /// compile until someone decides, in this table, whether a model may see
    /// it. The default when in doubt is [`Audience::LedgerOnly`].
    ///
    /// The audit, kind by kind:
    ///
    /// * **Category (b) — replayed.** `thread_system_interrupted`,
    ///   `thread_system_resumed`, `thread_system_queued`. The service could not
    ///   avoid interrupting or delaying the conversation and owes it an
    ///   account.
    /// * **Category (a) — never.** `custom` is the application's own metadata
    ///   ("none of that should land in the thread"); invisible by construction.
    /// * **Operational facts about the OPERATOR'S ACCOUNTS — never.**
    ///   `rate_limit_event` (38 of them in one live thread), `limit_hit`,
    ///   `retry_scheduled` (carries `reset_prose`), `budget_tripped`,
    ///   `budgets_failed`, `turn_parked`, `turn_resumed`, `turn_killed`,
    ///   `account_switched`. These are billing and plumbing; a model given them
    ///   reasons about a resource it does not control and cannot observe. The
    ///   category-(b) events above are the sanctioned, CON-22-safe restatement
    ///   of the only part of them that is the thread's business.
    /// * **Already in the transcript — never replayed.** `user_message`,
    ///   `assistant_text`, `thinking`, `tool_use`, `tool_result`,
    ///   `context_injected`. The model has these; replaying them would double
    ///   them.
    /// * **Mechanics with no meaning to a model — never.** `thread_started`,
    ///   `turn_started`, `turn_ended`, `turn_interrupted`, `turn_failed`,
    ///   `system_init`, `compacted`, `message_usage`, `result_turn`,
    ///   `agent_spawn`, `agent_launched`, `agent_completed`,
    ///   `agent_listing_delta`, `stream_event`, `unknown`, `settings_changed`,
    ///   `plugins_changed`, and the whole task lifecycle — `task_started`,
    ///   `task_updated`, `task_notification`, `task_progress`,
    ///   `background_tasks_changed`.
    ///
    /// Two kinds are worth their own line because they LOOK replayable and are
    /// not. `turn_interrupted` is the daemon-restart orphan marker, a fact
    /// about the daemon rather than a budget event — DEC-56's protocol emits
    /// `thread_system_interrupted` instead. `context_injected` records what was
    /// already said to the model; replaying it would say it twice.
    pub fn audience(&self) -> Audience {
        match self {
            // ── category (b): threads' own system events ────────────────
            ThreadEvent::ThreadSystemInterrupted { .. }
            | ThreadEvent::ThreadSystemResumed { .. }
            | ThreadEvent::ThreadSystemQueued { .. } => Audience::Model,

            // ── category (a): application bookkeeping ───────────────────
            ThreadEvent::Custom { .. } => Audience::LedgerOnly,

            // ── operator-account facts: the leak channel, closed ────────
            ThreadEvent::RateLimitEvent { .. }
            | ThreadEvent::LimitHit { .. }
            // Names the operator's account trouble — an expired login, an
            // overloaded service, a refused request — and he is the only one
            // who can act on any of it. Same rule as `limit_hit`: he reads it,
            // the thread never hears it.
            | ThreadEvent::ApiError { .. }
            | ThreadEvent::RetryScheduled { .. }
            // Names a login and a limit window it is stuck behind, so it is an
            // operator-account fact by the same rule as `retry_scheduled` — and
            // for the same reason it must never reach the model, which cannot
            // act on it and would only reason about a resource it does not
            // control. The operator reads it; the thread never hears it.
            | ThreadEvent::RetryStarving { .. }
            // Names the operator's OWN credential and asks him to act on it.
            // Same rule as `retry_scheduled`: he reads it, the thread never
            // hears it. A model told its account's login expired would reason
            // about a resource it does not control and cannot fix.
            | ThreadEvent::AccountLoginExpiredNotice { .. }
            | ThreadEvent::BudgetTripped { .. }
            | ThreadEvent::BudgetsFailed { .. }
            | ThreadEvent::TurnParked { .. }
            | ThreadEvent::TurnResumed { .. }
            | ThreadEvent::TurnKilled { .. }
            | ThreadEvent::AccountSwitched { .. } => Audience::LedgerOnly,

            // ── already in the transcript ───────────────────────────────
            ThreadEvent::UserMessage { .. }
            | ThreadEvent::AssistantText { .. }
            | ThreadEvent::Thinking { .. }
            | ThreadEvent::ToolUse { .. }
            | ThreadEvent::ToolResult { .. }
            | ThreadEvent::ContextInjected { .. } => Audience::LedgerOnly,

            // ── mechanics ───────────────────────────────────────────────
            ThreadEvent::ThreadStarted { .. }
            | ThreadEvent::TurnStarted { .. }
            | ThreadEvent::TurnEnded { .. }
            | ThreadEvent::TurnInterrupted { .. }
            | ThreadEvent::TurnFailed { .. }
            | ThreadEvent::SystemInit { .. }
            | ThreadEvent::Compacted { .. }
            | ThreadEvent::MessageUsage { .. }
            | ThreadEvent::ResultTurn { .. }
            | ThreadEvent::AgentSpawn { .. }
            | ThreadEvent::AgentLaunched { .. }
            | ThreadEvent::AgentCompleted { .. }
            | ThreadEvent::AgentListingDelta { .. }
            // The task lifecycle is the CLI's narration of work the model
            // ITSELF launched and is already told about in-band (the `Agent`
            // tool's own result). Replaying it would tell the model twice, in a
            // second voice, about its own subagents — and `task_notification`
            // carries the full result summary, so a replay would also duplicate
            // whole agent reports into context. LedgerOnly, all five.
            | ThreadEvent::TaskStarted { .. }
            | ThreadEvent::TaskUpdated { .. }
            | ThreadEvent::TaskNotification { .. }
            | ThreadEvent::TaskProgress { .. }
            | ThreadEvent::BackgroundTasksChanged { .. }
            | ThreadEvent::StreamEvent { .. }
            | ThreadEvent::Unknown { .. }
            | ThreadEvent::SettingsChanged { .. }
            | ThreadEvent::PluginsChanged { .. } => Audience::LedgerOnly,
        }
    }
}

/// The audience of a bare kind STRING — the same table, reachable from the
/// ledger's cheap-filter column without deserializing a payload.
///
/// Unknown kinds answer [`Audience::LedgerOnly`]: a kind nobody has audited is
/// not a kind a model may read.
pub fn audience_of_kind(kind: &str) -> Audience {
    if MODEL_AUDIENCE_KINDS.contains(&kind) {
        Audience::Model
    } else {
        Audience::LedgerOnly
    }
}

/// The persisted envelope: one ledger row. `seq` is the dense per-thread
/// cursor (replay/join key); `message_id` is the small spoken ordinal, present
/// only on addressable events; `ts` is RFC3339 UTC (G5: history replays WITH
/// timestamps).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredEvent {
    pub thread_id: String,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<u64>,
    pub ts: String,
    pub event: ThreadEvent,
}

/// What flows down the firehose to subscribers: persisted ledger events, or
/// transient liveness beats that structurally CANNOT be persisted (G6).
#[derive(Debug, Clone)]
pub enum Push {
    Event(StoredEvent),
    Liveness {
        thread_id: String,
        busy: bool,
        parent_running: bool,
        running_subagents: Vec<String>,
        // ── `account` IS RETIRED FROM THIS FRAME (S3) ──────────────────
        //
        // The beat carried `account: Option<String>` for exactly one consumer:
        // the account board's `live.py::_note_account`, which used it to place
        // a running thread without a `threads.get`. That whole client-side
        // fold is deleted — the `accounts` SHAPE puts a thread's row under the
        // account it was placed on, so the placement is the key and there is
        // no lookup left to short-circuit.
        //
        // VERIFIED BEFORE REMOVAL, not assumed. The three consumers of this
        // frame are AUI (`harness/apps/aui/src/threads_client.rs`, which parses
        // it into `DaemonPush::Liveness { thread_id, busy }` and reads nothing
        // else), the terminal face (`harness/apps/tui`, whose `TaggedMsg`
        // carries `thread_id`/`busy`/`parent_running`/`running_subagents` and
        // whose store answers `Applied::default()` for it — the frame is
        // discarded outright), and the board. None reads `account`.
        //
        // **`Push::Status.account` IS LOAD-BEARING AND STAYS.** It is not the
        // same field twice: `Status` is EDGE-TRIGGERED, the edge is the lock
        // being taken or released, and that edge is where placement is
        // DECIDED. The shape folds it. A 1 Hz beat repeating a placement
        // nobody reads is transport noise; the edge is the fact.
    },
    /// A per-thread STATUS transition (msg #1213: "status isn't making it
    /// whatsoever here to AUI"). Transient like liveness — never in the ledger
    /// — but unlike liveness it is edge-triggered, not a 1Hz beat, and it
    /// carries the two facts the operator asked to hear hands-free: is this
    /// thread still working, and is a message of mine waiting behind it.
    ///
    /// `state` is the spoken vocabulary the operator named — `active` /
    /// `queued` / `idle`, plus `parked` and `retrying` for the two states that
    /// are neither working nor free.
    /// Progress on a long ACCOUNT operation — a login or a rotation. Transient
    /// by construction, for the reasons the retired legacy `ServerMsg::Progress`
    /// frame gave (R2.S4): it replaces a client that tailed a log file. A
    /// consumer reads it as a `narration` line.
    ///
    /// It carries no `thread_id` because it belongs to no thread, which is why
    /// it is a `Push` in its own right rather than a `ThreadEvent`. It is
    /// delivered to FIREHOSE subscribers only — a per-thread subscription
    /// cannot express interest in it.
    Progress {
        op: String,
        id: String,
        account: Option<String>,
        phase: String,
        detail: Option<String>,
        done: bool,
        ok: Option<bool>,
    },
    Status {
        thread_id: String,
        state: String,
        /// An execution holds this thread's lock right now.
        executing: bool,
        /// Waiting queue rows — messages accepted and not yet delivered.
        queued: u64,
        /// Addressable messages above the client's seen cursor.
        unread: u64,
        /// The park reason, when `state == "parked"`.
        detail: Option<String>,
        /// **WHICH ACCOUNT THIS THREAD IS ON — the placement fact.**
        ///
        /// The operator, on the account board's per-account running counts:
        /// *"I have to click refresh to get my counts of active running on each
        /// account to update in the UI. that seems to contradict and needs to be
        /// resolved."* It contradicted because neither of the two live signals
        /// that a thread had started said WHERE, so a client that wanted "how
        /// many are running on alice-example-com" had to issue a `threads.get`
        /// per newly running thread, and a client that did not bother re-read
        /// the whole board on a timer instead. Both of those are now deleted
        /// from the board, and THIS FIELD IS WHY: the `accounts` shape folds it,
        /// so a thread's row moves to the account it was placed on at the
        /// instant it is placed.
        ///
        /// **THIS IS A CARRIED VALUE, NOT AN ANNOUNCEMENT, AND THAT IS
        /// DELIBERATE** — the one exception worth stating beside [`Push::Info`]'s
        /// "it announces, it does not carry" rule. `Info` refuses to carry a
        /// value because it would be a second copy of a QUERY'S SCHEMA that
        /// could drift from the query. This is not a copy of anything: status
        /// has no query behind it on this path, it is transient by
        /// construction, and the account is an attribute OF THE EDGE — which
        /// thread, what state, and where.
        ///
        /// **IT IS THE EDGE THAT MAKES IT LOAD-BEARING**, which is why the same
        /// field was retired from `Liveness` (see there) and kept here. The edge
        /// that says "this thread just took the lock" is precisely the instant a
        /// per-account running count changes. A 1 Hz beat repeating the same
        /// placement adds nothing a fold can use.
        ///
        /// `Option`, and absent means *we do not know*: a thread whose in-memory
        /// account string is empty (never placed, or a durable row we could not
        /// read) reports nothing rather than an empty string that a client would
        /// have to special-case. An honest absence is cheaper for every consumer
        /// than a guessed value.
        account: Option<String>,
    },
    /// **A SERVICE IS GOING DOWN, OR IS BACK.** Transient, like `Progress`, and
    /// belonging to no thread — so like `Progress` it is delivered to FIREHOSE
    /// subscribers and nobody else, since a per-thread subscription cannot
    /// express interest in a fact about the daemon itself.
    ///
    /// See [`crate::update::LifecycleNote`] for why this is shaped as a generic
    /// service announcement rather than as a threads-specific frame.
    Lifecycle(crate::update::LifecycleNote),
    /// **AN INFORMATION UPDATE THE DAEMON JUST MADE — the announcement that
    /// something a client is DISPLAYING is no longer what the client is
    /// displaying.**
    ///
    /// The operator's ask, verbatim: *"All information updates should get
    /// relayed out as a broadcast and picked up by my text user interface."*
    /// Before this frame existed the daemon broadcast only facts about THREADS
    /// — events, liveness, status — plus progress on an operation a client had
    /// itself started. Everything the account board actually shows (usage
    /// readings, login health, pool shares, placement inputs) changed in
    /// silence, so a client could only learn of it by asking again on a timer.
    /// That is precisely the regression he reported: *"the only updates I'm
    /// getting are when I trigger a refresh."*
    ///
    /// **IT ANNOUNCES, IT DOES NOT CARRY.** The frame names the DOMAIN that
    /// moved and why; it does not ship the new value. Two reasons, and both are
    /// load-bearing:
    ///
    /// * **A payload here would be a SECOND COPY of a query's schema**, on a
    ///   channel with no version negotiation, drifting from the query it
    ///   duplicates. `Progress` exists because "a log line is not an API"; a
    ///   parallel, push-only restatement of `gauge.status` is not one either.
    /// * **An announcement SELF-HEALS and a payload does not.** The client
    ///   re-reads through the ordinary query, so a coalesced burst, a dropped
    ///   frame and a concurrent poll all converge on the same answer. A client
    ///   reconciling pushed values against polled ones would have two sources
    ///   for one fact and no rule for which wins.
    ///
    /// Transient and FIREHOSE-ONLY, by the same rule as `Progress` and
    /// `Lifecycle`: it belongs to no thread, and a per-thread subscription
    /// cannot express interest in "the account picture moved".
    ///
    /// `domain` is what to re-read (`accounts` | `gauge`); `reason` is the
    /// machine tag for what moved. `detail` is one human sentence and is the
    /// only free-form field — a client switches on `domain` and `reason` and
    /// never parses prose.
    Info {
        /// The family of facts that changed — what a client re-reads.
        domain: String,
        /// The machine-readable cause, e.g. `usage_poll`, `rate_limit_event`,
        /// `login_restored`, `pool_share`, `limit_recorded`.
        reason: String,
        /// The login this is about, when it is about one.
        account: Option<String>,
        /// One human sentence, for a log or a wire dump. Not an API.
        detail: Option<String>,
        /// RFC3339 UTC.
        at: String,
    },
}

/// **A PRODUCER FACT — for shapes only, and it can never reach the legacy
/// wire.**
///
/// This is a SEPARATE ENUM from [`Push`], and that separation is the single
/// most load-bearing decision in the shape design. If these were `Push`
/// variants, `server::drive`'s exhaustive match over `Push`
/// (`server.rs:371-434`) would gain arms, `ServerMsg` would gain frames, and
/// "the legacy wire is byte-identical" would be a rule somebody has to
/// remember rather than a property of the type system. As a distinct enum,
/// [`crate::shape::Hub::emit_fact`] has NO expressible path to
/// `firehose.send`, and the three real consumers
/// (`harness/apps/tui/src/proto.rs`, `harness/apps/aui/src/threads_client.rs`,
/// `scripts/threads-app/threads_tui/api.py` — all of which send
/// `{"type":"subscribe","threads":null}` and nothing else) cannot observe that
/// any of this exists.
///
/// **Why these facts did not already exist.** Every one of them is something
/// the daemon ALREADY knew and announced only as an
/// [`Push::Info`] — "the account picture moved, go re-read". That
/// announcement is right for a client with a query behind it and useless to a
/// fold, which needs the VALUE. Each variant below is paired with the
/// `account_picture_changed_for` call that already fires at the same site, so
/// the two never disagree: the announcement keeps working for old clients and
/// the fact feeds the shape.
///
/// Each variant carries the SAME row its hydrate response carries, so the live
/// path and the read path feed one cell through one type. Where that row is a
/// `serde_json::Value` it is built by the ONE builder both the WS arm and the
/// CLI already use (`budgets::gauge_axis_row`, `budgets::pool_share_row`), so
/// the fact cannot drift from the query.
#[derive(Debug, Clone)]
pub enum Fact {
    /// P1 — a fresh `/usage` or stream-event reading was RECORDED (never
    /// merely attempted). `row` is exactly the per-axis object
    /// `budgets.gauge_state` renders.
    GaugeObserved {
        account: String,
        axis: String,
        row: serde_json::Value,
    },
    /// P2 / P9 — a limit window was written for this login.
    LimitRecorded {
        account: String,
        until: String,
        axis: Option<String>,
        prose: Option<String>,
    },
    /// P2 / P8 — the window was falsified or the identity changed under it.
    LimitCleared { account: String },
    /// P3 / P10 — a declared or overridden pool share moved. `row` is one
    /// element of `budgets.pool_share_rows`' `shares` array.
    PoolShare {
        account: String,
        row: serde_json::Value,
    },
    /// **P16 — the operator took an account OUT of the pool, or put it back.**
    ///
    /// Its own variant rather than a field on [`Fact::PoolShare`], because it is
    /// its own fact. Until 2026-09-20 "disabled" WAS `share == 0`, so the share
    /// fact carried it implicitly and a consumer had to know that a particular
    /// number in a quantity field was a different statement. The operator ruled
    /// them apart — *"I want disabled to be independent of share. Whether to
    /// include it in the pool supersedes how much should be made available to
    /// the pool."* — and a fact that is independent in the model and folded into
    /// another one on the wire is only half separated.
    ///
    /// The two still travel together (`accounts.disable` emits this AND a
    /// `PoolShare`, because a disable changes the pressure row), but they arrive
    /// as two statements, land in two cells, and a consumer branches on this one.
    ///
    /// `disabled_at` and `note` are `None` exactly when `disabled` is false:
    /// enabling DELETES the row, so there is no instant and no prose left to
    /// carry, and emitting the stale ones would be the last state pretending to
    /// be the current one.
    AccountDisabled {
        account: String,
        disabled: bool,
        disabled_at: Option<String>,
        note: Option<String>,
    },
    /// P4 — a login was proven dead, or proved itself alive again.
    LoginHealth {
        account: String,
        healthy: bool,
        dead_since: Option<String>,
        dead_reason: Option<String>,
    },
    /// P6 — the credential behind a slot changed. Boxed because `SlotReport` is
    /// by far the largest payload here and every other variant would pay for
    /// it.
    CredentialChanged {
        account: String,
        slot: Box<crate::accounts::SlotReport>,
    },
    /// P5 — the roster gained a login.
    AccountRegistered {
        name: String,
        login_dir: Option<String>,
    },
    /// P5 — the roster lost one.
    AccountRemoved { name: String },
    /// P15 — pressure is PRODUCED, not derived by a consumer. `row` is one
    /// element of `budgets.pressure_rows`' `accounts` array; `as_of` is the
    /// `now` that table was computed at.
    Pressure {
        account: String,
        row: serde_json::Value,
        as_of: String,
    },

    // ── THE THREAD FACTS (2026-09-26) ───────────────────────────────────
    //
    // **The first facts on this producer that are not about an account**, which
    // is why [`Fact::account`] had to stop claiming that every fact has one.
    //
    // Each of the five exists because a DURABLE THREAD WRITE had no live signal
    // carrying its value. `Push::Status` covers the derived state and nothing
    // else: the header, the settings blob, the placement column, the seen cursor
    // and the compaction marker are all written and then announced by nothing,
    // or announced in a form a fold cannot use. A shape folding only what exists
    // today would hold cells that quietly stop moving, which is
    // `docs/shapes.md` §2.5's deliberately-uncovered case arriving as five
    // concrete instances.
    //
    // Every one is emitted AFTER the store write and gated on
    // `hub.any_tracking()`, and every one carries the SAME row type its hydrate
    // response carries — `docs/shapes.md` §4.3 rule 3: one type, two paths, one
    // cell.
    /// **The header, after `upsert_thread` or `update_thread_settings`.**
    ///
    /// `ThreadStarted` is enough to name a new thread and not enough to FILL it:
    /// it carries `{thread_id, account, cwd, model}` and no `created_at`,
    /// `updated_at`. A fold may not read the store to get them, so the producer
    /// carries them.
    ThreadHeader {
        thread_id: String,
        row: crate::shapes::threads::HeaderRow,
    },
    /// **The settings BLOB — never the patch.**
    ///
    /// `ThreadEvent::SettingsChanged` carries `patch: Value`, which is what
    /// CHANGED and not what the settings ARE. A `Register<Value>` fed a patch
    /// would hold a patch. This carries `shapes::threads::settings_of`'s output,
    /// which is the same expression `thread_json` puts on the wire — see
    /// `ThreadCells::settings` for why that identity is load-bearing.
    ThreadSettings {
        thread_id: String,
        settings: serde_json::Value,
    },
    /// **The placement column, which moved in silence at two sites.**
    ///
    /// `persist_thread_account` is the one funnel for the `threads.account`
    /// write, and it had no emit at all: a failover persisted the column and a
    /// rehydration corrected it, and neither said so. `None` means the daemon
    /// genuinely does not know, exactly as `Push::Status.account` reasons.
    ThreadPlacement {
        thread_id: String,
        account: Option<String>,
    },
    /// **The seen cursor, and the exact unread count as of that cursor.**
    ///
    /// `Push::Status` is emitted after `mark_seen` but carries `unread`, not
    /// `seen_seq` — and adding a field to `ServerMsg::Status` would be changing
    /// an existing endpoint, which this work's constraint forbids.
    ///
    /// `unread` rides along because it is the only moment the fold cannot work it
    /// out: when the cursor JUMPS, how many addressable events the jump swallowed
    /// is a question about the log, and the write site has just read the answer
    /// anyway. See `ThreadCells::unread` for the three writers of that cell.
    ThreadSeen {
        thread_id: String,
        seen_seq: u64,
        unread: u64,
    },
    /// **The compaction marker, after `update_thread_compaction`.**
    ///
    /// Both durable compaction writes emitted nothing of their own. The `range`
    /// travels on the row rather than being folded out of
    /// `ThreadEvent::Compacted`, so the cell has exactly one writer.
    ThreadCompaction {
        thread_id: String,
        row: crate::shapes::threads::CompactionRow,
    },
    /// **THE UNDELIVERED QUEUE, AFTER EVERY WRITE THAT MOVES IT.**
    ///
    /// Carries the ROWS and not a count, because the count already travels:
    /// `Push::Status.queued` is emitted on the same paths and
    /// `crate::shapes::threads`' `queued` cell folds it. This is the one input
    /// that carries queue CONTENTS, and it exists because a fold cannot read the
    /// store — `Shape::folds` is handed a `Tx`, never the producer — so a cell
    /// holding what is in the queue has to be told.
    ///
    /// **It is the WHOLE undelivered set, not a delta.** Every one of the six
    /// writes that can move the queue (enqueue, deliver, restore, fail,
    /// re-select, cancel) changes membership in a different direction, and a
    /// per-write delta would need five shapes of event to say what one re-read
    /// says once. `queue` is a LEVEL (see `ThreadCells::queue`), so the honest
    /// input is the level.
    ThreadQueue {
        thread_id: String,
        rows: Vec<crate::shapes::threads::QueueEntry>,
    },

    /// **A PARENT'S MANAGED CHILDREN, AS A LEVEL** (`docs/TUI-SUBAGENTS.md`
    /// §4.1). `crate::shapes::threads`' `children` cell folds it. Emitted by
    /// `crate::managed` after a spawn's commit point and after a child's report
    /// lands — the two writes that move the parent's `child{..}` events — and
    /// built by the same `children_row` the hydrate uses. Whole value, not a
    /// delta, for `ThreadQueue`'s reason.
    ThreadChildren {
        thread_id: String,
        row: crate::shapes::threads::ChildrenRow,
    },

    /// **A THREAD'S STATUS ROLLED UP OVER ITS WHOLE SUBTREE, AS A LEVEL**
    /// (R9.TS1). `crate::shapes::threads`' `rolled` cell folds it. Emitted by
    /// `ThreadManager::emit_rolled` for a thread AND every ancestor whenever
    /// that thread's status edge fires or its children move — the only two
    /// things that can change an answer that depends on a whole subtree. Built
    /// by `crate::shapes::threads::rolled_rows`, the same builder the hydrate
    /// uses. Whole value, not a delta.
    ThreadRolled {
        thread_id: String,
        row: crate::shapes::threads::RolledRow,
    },

    /// **A THREAD'S IN-PROCESS (Task / Agent tool) SUBAGENTS, AS A LEVEL**
    /// (R9.TS2). `crate::shapes::threads`' `subagents` cell folds it. Emitted
    /// by `ThreadManager` whenever its in-memory roster changes — a Task call,
    /// the CLI naming its task, the CLI's own end word, the launching call's
    /// `tool_result`, or the `claude` process exiting. The whole map keyed by
    /// launching `tool_use_id`, not a delta.
    ThreadSubagents {
        thread_id: String,
        rows: std::collections::BTreeMap<String, crate::shapes::threads::SubagentRow>,
    },

    /// **A MANAGED CHILD ACTED** (TC-44): `child_id`, a child of `thread_id`,
    /// appended an event at `at` (RFC3339, the event's own `ts`).
    /// `crate::shapes::threads`' `children` cell moves that run's
    /// `last_activity_at`. Emitted by `ThreadManager` on the append path at
    /// most once per wall-clock second per child — an activity tick, not a
    /// lineage change, so it is a delta on one run and not the whole level.
    ThreadChildActivity {
        thread_id: String,
        child_id: String,
        at: String,
    },

    // ── THE LINK BEAT (2026-09-28) ──────────────────────────────────────
    /// **THE HEARTBEAT, AND IT IS A `Fact` SO THAT IT TRAVELS THE FRAME PATH.**
    ///
    /// The one input on this producer that is about NOTHING — no account, no
    /// thread, no write. Its entire content is that it arrived, and it earns its
    /// place only because of the route it takes to arrive: emitted by the beat
    /// task, into the shape's FIFO, through the fold, out of the drain, onto the
    /// connection's own frame channel. A consumer that receives one has been told
    /// that every link in that chain moved, which is precisely what a websocket
    /// ping cannot tell it — see [`crate::shapes::heartbeat`] for the full
    /// argument and for what a ping DOES answer.
    ///
    /// A `Fact` rather than a [`Push`] for the usual construction reason: a
    /// `Fact` has no expressible route to the legacy firehose, so adding a 1 Hz
    /// input cannot put 1 Hz of traffic on a wire nobody asked to beat.
    ///
    /// `seq` is the PRODUCER's monotone count of beats made, and the delivery
    /// gate does not renumber it. Under `beat: n` a consumer sees gaps and the
    /// gaps are its own ask, not loss — `crate::shape::Beat` §6.
     /// **THE PRODUCER'S COUNT OF ITS OWN WARNINGS WAS SAMPLED.**
    ///
    /// Its own variant and emphatically NOT a field on [`Fact::Beat`], although
    /// riding the beat would have cost no new fact, no new timer and no new match
    /// arm anywhere. A beat means *frames are flowing*; this means *the daemon has
    /// complained N times*. One event meaning two things is the collapse
    /// `docs/shape-design-guide.md` rule 8 calls THE recurring bug, and a consumer
    /// of `heartbeat` would then have had to parse a payload it has no interest in
    /// to get the liveness signal it does want.
    ///
    /// **It carries the tally rather than a nudge to go and read one.** The fold is
    /// then pure — it needs no access to the process-global counter and is testable
    /// by handing it a [`crate::warnings::Tally`] — and the stamp on the input is
    /// the instant the numbers were true, which is what lets a consumer derive a
    /// RATE by diffing two frames. A nudge would have made the fold read a global
    /// at an instant the frame does not name.
    WarningsSampled {
        tally: crate::warnings::Tally,
    },
   Beat { seq: u64 },
}

impl Fact {
    /// The name the burst warning prints, so an operator reading
    /// `tracing::warn!(shape, input_kind, emitted, "burst")` can tell which
    /// producer made a fold re-emit keys it did not change.
    pub fn kind(&self) -> &'static str {
        match self {
            Fact::GaugeObserved { .. } => "gauge_observed",
            Fact::LimitRecorded { .. } => "limit_recorded",
            Fact::LimitCleared { .. } => "limit_cleared",
            Fact::PoolShare { .. } => "pool_share",
            Fact::AccountDisabled { .. } => "account_disabled",
            Fact::LoginHealth { .. } => "login_health",
            Fact::CredentialChanged { .. } => "credential_changed",
            Fact::AccountRegistered { .. } => "account_registered",
            Fact::AccountRemoved { .. } => "account_removed",
            Fact::Pressure { .. } => "pressure",
            Fact::ThreadHeader { .. } => "thread_header",
            Fact::ThreadSettings { .. } => "thread_settings",
            Fact::ThreadPlacement { .. } => "thread_placement",
            Fact::ThreadSeen { .. } => "thread_seen",
            Fact::ThreadCompaction { .. } => "thread_compaction",
            Fact::ThreadQueue { .. } => "thread_queue",
            Fact::ThreadChildren { .. } => "thread_children",
            Fact::ThreadRolled { .. } => "thread_rolled",
            Fact::ThreadSubagents { .. } => "thread_subagents",
            Fact::ThreadChildActivity { .. } => "thread_child_activity",
            Fact::Beat { .. } => "beat",
            Fact::WarningsSampled { .. } => "warnings_sampled",
        }
    }

    /// **The account this fact is about, IF IT IS ABOUT ONE.**
    ///
    /// This returned `&str` and asserted structurally that every fact was about
    /// an account. The five thread facts break that assertion — a thread is not
    /// an account — so it returns `Option` now, and the structural claim it used
    /// to make is stated where it is still true: the `accounts` shape's fold
    /// resolves each account fact's own field and never calls this.
    ///
    /// It had ZERO callers in this crate when the thread facts were added
    /// (verified by grep over `lib/threads/src` and `lib/shapewire/src`),
    /// which is what made the signature free to change. The burst warning and
    /// `Input::kind` key off [`Fact::kind`] alone.
    pub fn account(&self) -> Option<&str> {
        match self {
            Fact::GaugeObserved { account, .. }
            | Fact::LimitRecorded { account, .. }
            | Fact::LimitCleared { account }
            | Fact::PoolShare { account, .. }
            | Fact::AccountDisabled { account, .. }
            | Fact::LoginHealth { account, .. }
            | Fact::CredentialChanged { account, .. }
            | Fact::Pressure { account, .. } => Some(account),
            Fact::AccountRegistered { name, .. } | Fact::AccountRemoved { name } => Some(name),
            Fact::ThreadHeader { .. }
            | Fact::ThreadSettings { .. }
            | Fact::ThreadPlacement { .. }
            | Fact::ThreadSeen { .. }
            | Fact::ThreadCompaction { .. }
            | Fact::ThreadQueue { .. }
            | Fact::ThreadChildren { .. }
            | Fact::ThreadRolled { .. }
            | Fact::ThreadSubagents { .. }
            | Fact::ThreadChildActivity { .. }
            // A beat is about the LINK. It is not about an account, and it is not
            // about a thread either — `crate::shapes::threads`' `liveness` cell
            // answers "is this thread's turn still working", which is a different
            // question with a different home.
            | Fact::Beat { .. }
            // The warning count is about the PRODUCER. Many of the warnings it
            // counts DO name an account, but the count is not about any one of
            // them, and attributing it to the first would be a fact about the
            // daemon filed under a login that happens to appear in it.
            | Fact::WarningsSampled { .. } => None,
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// STRUCTURED OUTPUT — the daemon's verdict on a schema-bearing turn
// (R11.J2, `docs/STRUCTURED-OUTPUT.md` §3.3–§3.4)
// ═════════════════════════════════════════════════════════════════════════════
//
// A turn that was asked for JSON (`--json-schema`) ends with ONE fact on the
// answering thread's ledger: a `Custom` event of type
// [`STRUCTURED_OUTPUT_EVENT`] whose `data` is a [`StructuredOutputRecord`].
// It is the DAEMON's judgment of what the CLI produced, so it is a separate
// event from `ResultTurn` (the CLI's own line) — Option 3.4-A.
//
// The record is never self-attested: the value is validated here against the
// schema the asker sent, with the `jsonschema` crate. The CLI's own check is
// not trusted (R11.J1 PROBE-NOTE: the fallback tool input can be invalid, and
// an unparseable call reaches the stream as `{"__unparsedToolInput": …}`).
//
// What this record may NEVER do (D3): change the turn's machine outcome, its
// routing, or whether anyone is woken. A turn that ended `ok` stays `ok`
// whether its record says `ok` or `unmet`.

/// The `Custom.event_type` of the structured-output fact.
pub const STRUCTURED_OUTPUT_EVENT: &str = "cauldron.threads/structured_output";

/// The tool the CLI adds when `--json-schema` is passed. Its last top-level
/// call is the fallback candidate when the `result` line carries no value.
pub const STRUCTURED_OUTPUT_TOOL: &str = "StructuredOutput";

/// The CLI's spelling of a tool call whose input was not parseable JSON
/// (seen four times in R11.J1's `invalid-retries-exhausted` capture).
pub const UNPARSED_TOOL_INPUT: &str = "__unparsedToolInput";

/// At most this many validation errors are recorded (§3.3: `errors[≤5]`).
pub const MAX_STRUCTURED_ERRORS: usize = 5;

/// `ok`: a value was present and valid against the asker's schema. `unmet`:
/// absent, unparseable, or invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredStatus {
    Ok,
    Unmet,
}

/// The `data` of a [`STRUCTURED_OUTPUT_EVENT`] fact (§3.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredOutputRecord {
    /// The turn id that produced it, when known.
    #[serde(default)]
    pub turn: Option<String>,
    /// The message the schema was attached to.
    #[serde(default)]
    pub for_message: Option<u64>,
    /// 1 for the first answer; each corrective turn (J4) adds one.
    pub attempt: u32,
    /// sha256 (hex) of the schema's compact serialization.
    pub schema_sha: String,
    pub status: StructuredStatus,
    /// The candidate value. Always present on `ok`; present on `unmet` when
    /// there was something to judge (an invalid value is kept, §3.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Why it is `unmet`, at most [`MAX_STRUCTURED_ERRORS`]. Empty on `ok`.
    #[serde(default)]
    pub errors: Vec<String>,
}

/// sha256 hex of `schema`'s compact JSON form — the stable name a record
/// carries for the schema it was judged against.
pub fn schema_sha(schema: &Value) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(schema.to_string().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Judge one candidate against `schema`. **The only path to `ok`** is a value
/// that is present, parsed, and passes the validator; everything else is
/// `unmet` with a reason.
pub fn judge_structured(schema: &Value, candidate: Option<&Value>) -> (StructuredStatus, Vec<String>) {
    let Some(value) = candidate else {
        return (
            StructuredStatus::Unmet,
            vec!["no structured output: the turn ended without a StructuredOutput value".into()],
        );
    };
    if value.get(UNPARSED_TOOL_INPUT).is_some() {
        return (
            StructuredStatus::Unmet,
            vec!["the StructuredOutput input was not parseable JSON".into()],
        );
    }
    let validator = match jsonschema::validator_for(schema) {
        Ok(v) => v,
        Err(e) => {
            return (
                StructuredStatus::Unmet,
                vec![format!("the requested schema does not compile: {e}")],
            )
        }
    };
    let errors: Vec<String> = validator
        .iter_errors(value)
        .take(MAX_STRUCTURED_ERRORS)
        .map(|e| {
            let at = e.instance_path().to_string();
            if at.is_empty() {
                e.to_string()
            } else {
                format!("{at}: {e}")
            }
        })
        .collect();
    if errors.is_empty() {
        (StructuredStatus::Ok, Vec::new())
    } else {
        (StructuredStatus::Unmet, errors)
    }
}

/// The candidate a turn offers: the `result` line's `structured_output` when
/// it has one, else the LAST top-level `StructuredOutput` tool_use input
/// (§3.3). Either way it is only a candidate until [`judge_structured`].
pub fn structured_candidate<'a>(
    from_result: Option<&'a Value>,
    last_tool_input: Option<&'a Value>,
) -> Option<&'a Value> {
    from_result.or(last_tool_input)
}

impl StructuredOutputRecord {
    /// Judge `candidate` against `schema` and build the record.
    pub fn judge(
        schema: &Value,
        candidate: Option<&Value>,
        turn: Option<String>,
        for_message: Option<u64>,
        attempt: u32,
    ) -> Self {
        let (status, errors) = judge_structured(schema, candidate);
        StructuredOutputRecord {
            turn,
            for_message,
            attempt,
            schema_sha: schema_sha(schema),
            status,
            value: candidate.cloned(),
            errors,
        }
    }

    /// The ledger fact: `Custom{event_type: STRUCTURED_OUTPUT_EVENT, data}`.
    pub fn to_event(&self) -> ThreadEvent {
        ThreadEvent::Custom {
            event_type: STRUCTURED_OUTPUT_EVENT.to_string(),
            data: serde_json::to_value(self).expect("a StructuredOutputRecord always serializes"),
        }
    }

    /// Read a record back out of a ledger event; `None` for any other event.
    pub fn from_event(ev: &ThreadEvent) -> Option<Self> {
        match ev {
            ThreadEvent::Custom { event_type, data } if event_type == STRUCTURED_OUTPUT_EVENT => {
                serde_json::from_value(data.clone()).ok()
            }
            _ => None,
        }
    }
}
