//! A LOCAL, TOLERANT MIRROR of the threads daemon's wire protocol.
//!
//! **Not a dependency on the `threads` crate** (`DEC-10`). `apps/aui`'s
//! `threads_client.rs` made the same call and for the same two reasons: a path
//! dependency would couple every harness build to the state of a
//! `substrate-interface-layer` checkout that is not in this repo and not in
//! this repo's lockfile, and — the load-bearing half — the daemon's own docs
//! promise new event kinds *without warning*. A mirror that refuses to parse an
//! upgraded daemon is a face that goes dark on the day the daemon improves, and
//! `DIR-4` ("give him ALL of the events") makes a silent drop a defect rather
//! than a degradation.
//!
//! So tolerance is the contract, and it is enforced at **two** levels, because
//! the wire nests two tagged enums:
//!
//! 1. **The frame** (`{"type": …}`). An unrecognized `type` becomes
//!    [`ServerMsg::Unrecognized`] carrying the raw JSON. Nothing about an
//!    unknown frame can kill the session.
//! 2. **The ledger event** (`{"kind": …}` inside an `event` frame). An
//!    unrecognized `kind` becomes [`ThreadEvent::Untyped`], which carries the
//!    kind string and the raw payload and **renders**.
//!
//! **Why serde alone does not give this.** `#[serde(tag = "…")]` is not
//! tolerant: an internally-tagged enum with no matching variant is a hard
//! error, and `#[serde(other)]` only accepts a *unit* variant, so it cannot
//! keep the payload. Both levels therefore parse to a [`Value`] first and fall
//! back explicitly. That fallback also catches a known kind whose SHAPE drifted
//! — which is the same class of event (the daemon changed under us) and wants
//! the same answer (render it, say what we could not read, never drop it).
//!
//! **[`Untyped`] is not a synonym for "unknown".** [`KNOWN_KINDS`] is the
//! daemon's own `ThreadEvent::kind()` table transcribed verbatim, so the face
//! can tell "a kind this daemon documents that this build has not specialized
//! yet" apart from "a kind nothing here has ever heard of". Those two are
//! different facts about the world and honesty rule 1 says they must not render
//! alike.

//! # **WHAT IS UNREACHABLE IN HERE NOW, NAMED RATHER THAN LEFT TO BE FOUND**
//!
//! This file is a tolerant mirror of the daemon's wire, and the SOCKET that
//! spoke that wire is gone (`transport.rs` is vocabulary only now; the face
//! reads `crate::seam`). So a large part of this file has no producer and no
//! caller, and that is residue rather than a design:
//!
//! | what | state |
//! |---|---|
//! | [`ThreadEvent`], `KNOWN_KINDS`, `ThreadEvent::parse`/`kind` | **LIVE and load-bearing.** `threads/log` rows carry the ledger event byte-identically, so every classifier above this is unchanged |
//! | [`ServerMsg::Event`] | **LIVE.** `mirror::Projection::rows` builds it and `store::apply_frame` consumes it, exactly as the firehose did |
//! | `auth_frame` | **DEAD, and its only caller is the test that tests it** (`the_auth_frame_is_what_the_daemon_reads`). `seam::link` speaks the chassis door and sends no legacy frame at all. Kept this pass only because deleting it was not in R2.S2b's mandate; see that step's report. (`subscribe_firehose_frame` is DELETED: R2.S4 retired the firehose and the chassis refuses the frame `malformed`. `request_frame` is DELETED: R2.S2b, zero call sites in the checkout and no test at all.) |
//! | `TaggedMsg` and its `From` conversion | **DEAD.** It parsed frames off a socket this crate no longer opens |
//! | `ServerMsg::{Welcome, AuthAck, Response, SubAck, Lagged}` | **DEAD**, and they never reached `store.rs` even before — the old transport's header said so |
//! | `ServerMsg::{Status, Progress, Lifecycle, Info, Liveness, Unrecognized}` | **DEAD, with their facts covered elsewhere.** `Status`'s four fields are a strict subset of what `mirror::status_row` projects; the three narration channels arrive as `TransportEvent::Note` with byte-identical text; `Liveness` was already `Applied::default()` — the TUI never read it |
//!
//! **It is not deleted in this arc, and the reason is a trade worth stating.**
//! Deleting the dead `ServerMsg` variants means deleting `store::apply_frame`'s
//! arms for them and the `store.rs` tests that construct them — and *"`store.rs`
//! has zero lines changed"* is the strongest evidence this migration is a mirror
//! and not a rewrite. The residue cannot reach a socket; the parity claim can
//! only be made once. So the cleanup is a separate pass, and this table is what
//! stops it being a discovery.
use serde::Deserialize;
use serde_json::{json, Value};

/// The proto version this mirror was written against (`protocol.rs::PROTO`).
///
/// A mismatch is LOGGED AND TOLERATED, never fatal: `PROTO` bumps on a breaking
/// *envelope* change, and refusing to run would trade a partly-working face for
/// no face at all. The banner is the right place for that to surface.
pub const PROTO: u16 = 1;

// ────────────────────────────────────────────────────────────────────────────
//  client → daemon
// ────────────────────────────────────────────────────────────────────────────

/// `{"type":"auth","secret":"…"}` — required first when the daemon has a
/// secret configured; acked as a no-op when it does not.
pub fn auth_frame(secret: &str) -> String {
    json!({ "type": "auth", "secret": secret }).to_string()
}

// **`subscribe_firehose_frame` IS DELETED (R2.S9).** It built
// `{"type":"subscribe","threads":null}`, and R2.S4 retired the firehose on the
// producer: the `:3651` listener is gone and the chassis refuses that frame
// `malformed`. Nothing called it; a builder for a frame the producer refuses is
// an invitation to send one.

// ────────────────────────────────────────────────────────────────────────────
//  daemon → client
// ────────────────────────────────────────────────────────────────────────────

/// Where one thread's replay must resume after a fan-out gap.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LagCursor {
    pub thread_id: String,
    /// The highest seq this CONNECTION was actually sent for the thread.
    pub last_seq: u64,
}

/// The success / error / promise switch. `v1` never emits `Promise`; the arm is
/// mirrored so an upgraded daemon that does emit one does not become a parse
/// failure.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    Success { payload: Value },
    Error { error: String },
    Promise { promise: String },
}

/// The frames this build understands, plus the arm that keeps the ones it does
/// not.
#[derive(Debug, Clone)]
pub enum ServerMsg {
    Welcome {
        proto: u16,
        boot_id: String,
        auth_required: bool,
        daemon_version: String,
    },
    AuthAck {
        ok: bool,
    },
    Response {
        correlate: String,
        outcome: Outcome,
    },
    /// A pushed PERSISTED ledger event. The only frame that carries a `seq`,
    /// which is why it is the only one that can move a replay cursor.
    Event {
        thread_id: String,
        seq: u64,
        message_id: Option<u64>,
        ts: String,
        event: ThreadEvent,
    },
    /// 1 Hz busy beat. Transient, unpositioned, and explicitly IGNORED by lag
    /// recovery — the next beat repairs it.
    Liveness {
        thread_id: String,
        busy: bool,
        parent_running: bool,
        running_subagents: Vec<String>,
    },
    /// EDGE-TRIGGERED per-thread state. It does not repeat, which is why every
    /// gap in delivery has to be closed with a `threads/status` query.
    Status {
        thread_id: String,
        state: String,
        executing: bool,
        queued: u64,
        unread: u64,
        detail: Option<String>,
    },
    Progress {
        op: String,
        id: String,
        account: Option<String>,
        phase: String,
        detail: Option<String>,
        done: bool,
        ok: Option<bool>,
    },
    /// A supervised service going down or coming back. `boot_id` on `back_up`
    /// is the same value the connection's `Welcome` carried, so the two can be
    /// correlated.
    Lifecycle {
        service: String,
        /// `going_down` | `back_up`.
        phase: String,
        /// `new_binary` | `operator` | `fallback` | `unknown`.
        reason: String,
        detail: String,
        boot_id: String,
        at: String,
    },
    /// "What you are showing just changed — re-read `domain`." Carries no
    /// value, deliberately.
    Info {
        domain: String,
        reason: String,
        account: Option<String>,
        detail: Option<String>,
        at: String,
    },
    SubAck {
        firehose: bool,
        threads: Vec<String>,
    },
    /// THE FAN-OUT OVERFLOWED and events were dropped for this connection.
    Lagged {
        dropped: u64,
        cursors: Vec<LagCursor>,
        recover: String,
    },
    /// **A FRAME TYPE THIS BUILD HAS NEVER SEEN — kept, never dropped.**
    ///
    /// `why` is the parse error when the `type` WAS recognized and the body did
    /// not fit, which is the shape-drift case: it is the difference between
    /// "the daemon added a frame" and "the daemon changed a frame", and a
    /// debugger reading `~/.tui/log` needs to be told which.
    Unrecognized {
        frame_type: String,
        why: String,
        raw: Value,
    },
}

/// The tagged half — everything serde can resolve on its own.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TaggedMsg {
    Welcome {
        #[serde(default)]
        proto: u16,
        #[serde(default)]
        boot_id: String,
        #[serde(default)]
        auth_required: bool,
        #[serde(default)]
        daemon_version: String,
    },
    AuthAck {
        #[serde(default)]
        ok: bool,
    },
    Response {
        correlate: String,
        outcome: Outcome,
    },
    Event {
        thread_id: String,
        seq: u64,
        #[serde(default)]
        message_id: Option<u64>,
        #[serde(default)]
        ts: String,
        event: Value,
    },
    Liveness {
        thread_id: String,
        #[serde(default)]
        busy: bool,
        #[serde(default)]
        parent_running: bool,
        #[serde(default)]
        running_subagents: Vec<String>,
    },
    Status {
        thread_id: String,
        #[serde(default)]
        state: String,
        #[serde(default)]
        executing: bool,
        #[serde(default)]
        queued: u64,
        #[serde(default)]
        unread: u64,
        #[serde(default)]
        detail: Option<String>,
    },
    Progress {
        #[serde(default)]
        op: String,
        #[serde(default)]
        id: String,
        #[serde(default)]
        account: Option<String>,
        #[serde(default)]
        phase: String,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        done: bool,
        #[serde(default)]
        ok: Option<bool>,
    },
    Lifecycle {
        #[serde(default)]
        service: String,
        #[serde(default)]
        phase: String,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        detail: String,
        #[serde(default)]
        boot_id: String,
        #[serde(default)]
        at: String,
    },
    Info {
        #[serde(default)]
        domain: String,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        account: Option<String>,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        at: String,
    },
    SubAck {
        #[serde(default)]
        firehose: bool,
        #[serde(default)]
        threads: Vec<String>,
    },
    Lagged {
        #[serde(default)]
        dropped: u64,
        #[serde(default)]
        cursors: Vec<LagCursor>,
        #[serde(default)]
        recover: String,
    },
}

impl ServerMsg {
    /// Parse one text frame. **This function does not have a failure mode that
    /// reaches the caller as "drop it".**
    ///
    /// Malformed JSON is the one thing that cannot be kept structurally — there
    /// is no `Value` to carry — and it still comes back as an
    /// [`ServerMsg::Unrecognized`] holding the raw text as a JSON string, so it
    /// reaches the screen as a line rather than vanishing into a `continue`.
    pub fn parse(text: &str) -> ServerMsg {
        let v: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                return ServerMsg::Unrecognized {
                    frame_type: "<not json>".into(),
                    why: e.to_string(),
                    raw: Value::String(text.to_string()),
                }
            }
        };
        let frame_type = v
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("<no type>")
            .to_string();
        // `&Value` is a Deserializer, so the fallback arm still owns `v`.
        match TaggedMsg::deserialize(&v) {
            Ok(t) => t.into_server_msg(),
            Err(e) => ServerMsg::Unrecognized {
                frame_type,
                why: e.to_string(),
                raw: v,
            },
        }
    }

    /// A short label for a log line or a lane row.
    pub fn label(&self) -> &str {
        match self {
            ServerMsg::Welcome { .. } => "welcome",
            ServerMsg::AuthAck { .. } => "auth_ack",
            ServerMsg::Response { .. } => "response",
            ServerMsg::Event { .. } => "event",
            ServerMsg::Liveness { .. } => "liveness",
            ServerMsg::Status { .. } => "status",
            ServerMsg::Progress { .. } => "progress",
            ServerMsg::Lifecycle { .. } => "lifecycle",
            ServerMsg::Info { .. } => "info",
            ServerMsg::SubAck { .. } => "sub_ack",
            ServerMsg::Lagged { .. } => "lagged",
            ServerMsg::Unrecognized { frame_type, .. } => frame_type,
        }
    }
}

impl TaggedMsg {
    fn into_server_msg(self) -> ServerMsg {
        match self {
            TaggedMsg::Welcome {
                proto,
                boot_id,
                auth_required,
                daemon_version,
            } => ServerMsg::Welcome {
                proto,
                boot_id,
                auth_required,
                daemon_version,
            },
            TaggedMsg::AuthAck { ok } => ServerMsg::AuthAck { ok },
            TaggedMsg::Response { correlate, outcome } => {
                ServerMsg::Response { correlate, outcome }
            }
            TaggedMsg::Event {
                thread_id,
                seq,
                message_id,
                ts,
                event,
            } => ServerMsg::Event {
                thread_id,
                seq,
                message_id,
                ts,
                event: ThreadEvent::parse(event),
            },
            TaggedMsg::Liveness {
                thread_id,
                busy,
                parent_running,
                running_subagents,
            } => ServerMsg::Liveness {
                thread_id,
                busy,
                parent_running,
                running_subagents,
            },
            TaggedMsg::Status {
                thread_id,
                state,
                executing,
                queued,
                unread,
                detail,
            } => ServerMsg::Status {
                thread_id,
                state,
                executing,
                queued,
                unread,
                detail,
            },
            TaggedMsg::Progress {
                op,
                id,
                account,
                phase,
                detail,
                done,
                ok,
            } => ServerMsg::Progress {
                op,
                id,
                account,
                phase,
                detail,
                done,
                ok,
            },
            TaggedMsg::Lifecycle {
                service,
                phase,
                reason,
                detail,
                boot_id,
                at,
            } => ServerMsg::Lifecycle {
                service,
                phase,
                reason,
                detail,
                boot_id,
                at,
            },
            TaggedMsg::Info {
                domain,
                reason,
                account,
                detail,
                at,
            } => ServerMsg::Info {
                domain,
                reason,
                account,
                detail,
                at,
            },
            TaggedMsg::SubAck { firehose, threads } => ServerMsg::SubAck { firehose, threads },
            TaggedMsg::Lagged {
                dropped,
                cursors,
                recover,
            } => ServerMsg::Lagged {
                dropped,
                cursors,
                recover,
            },
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  the ledger event
// ────────────────────────────────────────────────────────────────────────────

/// **THE DAEMON'S COMPLETE LEDGER VOCABULARY**, transcribed verbatim from
/// `event.rs::ThreadEvent::kind()`.
///
/// It exists so [`ThreadEvent::Untyped`] can tell two different facts apart: a
/// kind the daemon documents and this build has not specialized, versus a kind
/// that is in NO vocabulary either side knows. The first is a gap in this
/// client; the second is a daemon that moved. They render differently because
/// they are different, and a list that goes stale fails SAFE — a newly added
/// daemon kind simply reads as "unknown", which is exactly what it is from
/// here.
pub const KNOWN_KINDS: &[&str] = &[
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

/// One ledger event.
///
/// **A mirror that types what it RENDERS, with a total fallback under it.**
///
/// Step 2 typed thirteen kinds on purpose: typing more would have been
/// speculative work against a rendering vocabulary it did not own. Step 3 owns
/// it (`DIR-4` — "give me all of the events coming from threads"), so the
/// variants here are now every kind the event lane says something specific
/// about. The rest still arrive as [`ThreadEvent::Untyped`] with their payload
/// intact, and that arm is still the one that has to be right: the daemon adds
/// kinds without warning and `DIR-4` makes silence the defect.
///
/// **EVERY FIELD IS `#[serde(default)]`, deliberately.** `0031` left an unknown
/// naming a payload shape it never observed live, and the instruction that came
/// back was to parse tolerantly — *a field that moved must render, not break*.
/// With a required field, one renamed key upstream turns a whole kind into a
/// shape-drift row; with defaults, it turns into the same row missing one fact,
/// which is both more useful and more honest.
#[derive(Debug, Clone)]
pub enum ThreadEvent {
    ThreadStarted {
        account: String,
        cwd: String,
        model: String,
    },
    UserMessage {
        text: String,
        /// **THE METADATA THE DAEMON STORES AND NEVER READS**, verbatim.
        ///
        /// The threads daemon's own `user_message` carries an opaque
        /// per-message `metadata` object (`lib/threads/src/event.rs`: *"we have
        /// to be able to attach arbitrary metadata to every message"*), and
        /// this face was DROPPING it on ingest. He asked to see it — *"There's
        /// also some metadata we attach to user inputs… we need a way to toggle
        /// viewing this"* — so it is kept.
        ///
        /// A `Value`, not a typed struct: the whole point of the field on the
        /// wire is that its shape is the APPLICATION's and the daemon has no
        /// opinion about it. This face has none either; it flattens it for
        /// display and asserts nothing.
        metadata: Value,
        /// `immediate`, `queued`, … — the threads system's own field, and the
        /// one piece of "threads system metadata" that is not inside
        /// [`ThreadEvent::UserMessage::metadata`].
        intent: Option<String>,
    },
    AssistantText {
        text: String,
        /// `Some` means this came from INSIDE a subagent, not from the thread's
        /// own assistant — the daemon's own attribution, and the reason a
        /// sidechain reply is not boxed as the thread talking.
        parent_tool_use_id: Option<String>,
    },
    Thinking {
        text: String,
    },
    ContextInjected {
        text: String,
    },
    SystemInit {
        model: Option<String>,
        cwd: Option<String>,
        /// **THE TOOL ROSTER**, which is big — `0047` flagged this kind as
        /// EXPANDABLE for exactly that reason: one line collapsed, the whole
        /// list when he asks.
        tools: Vec<String>,
        permission_mode: Option<String>,
    },
    ToolUse {
        /// **THE JOIN KEY.** `tool_result` carries it back as `tool_use_id`,
        /// and it is what lets one call and its result be ONE row (`DEC-33`).
        /// It is a hash and it never reaches the one-line face — the tool's
        /// NAME is the human thing (`0051` item 5).
        id: String,
        name: String,
        input: Value,
        /// `Some` means a SUBAGENT made this call.
        parent_tool_use_id: Option<String>,
    },
    ToolResult {
        /// Which [`ThreadEvent::ToolUse`] this answers. The merge is keyed on
        /// it; an empty string means the daemon did not say, and an unkeyed
        /// result cannot be merged and stands alone.
        tool_use_id: String,
        is_error: bool,
        content: Value,
    },
    MessageUsage {
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
    },
    AgentSpawn {
        subagent_type: Option<String>,
        description: Option<String>,
        run_in_background: bool,
    },
    AgentLaunched {
        agent_id: String,
        is_async: bool,
        status: String,
    },
    /// **A HEURISTIC THAT FIRES 21 MINUTES EARLY ON AVERAGE — MEASURED, and
    /// the documentation it shipped with was wrong by two orders of
    /// magnitude.**
    ///
    /// Both this comment and the daemon's own (`event.rs:288-296`) said "~9
    /// spurious firings per real completion". The real figure, over 1,120 rows
    /// of the live ledger (`0080`): **1,118 of them (99.8%) are followed by a
    /// real `task_notification` for the same id, mean lead 1,258 SECONDS,
    /// 96.8% more than a minute early, maximum 8.1 hours.** The live case
    /// fired 58 seconds into a task that then ran 93 more minutes.
    ///
    /// So it is not "usually a bit early": it is **almost never a completion**.
    /// Rendered, never load-bearing. `task_notification` is the true terminal
    /// signal, and nothing — not a duration, not a state word, not a tree row
    /// — may be derived from this kind.
    AgentCompleted {
        agent_id: String,
    },
    TaskStarted {
        task_id: String,
        description: Option<String>,
        subagent_type: Option<String>,
        /// `local_agent` or `local_bash`. See the wire struct.
        task_type: Option<String>,
        /// The `tool_use` that launched this task. See the wire struct.
        tool_use_id: Option<String>,
    },
    TaskUpdated {
        task_id: String,
        status: Option<String>,
        error: Option<String>,
    },
    TaskProgress {
        task_id: String,
        description: Option<String>,
        subagent_type: Option<String>,
        last_tool_name: Option<String>,
        total_tokens: Option<u64>,
        tool_uses: Option<u64>,
        duration_ms: Option<u64>,
    },
    TaskNotification {
        task_id: String,
        status: Option<String>,
        summary: Option<String>,
        total_tokens: Option<u64>,
        duration_ms: Option<u64>,
    },
    TurnStarted {
        attempt: u32,
    },
    TurnEnded {
        exit_code: Option<i32>,
    },
    TurnInterrupted {
        reason: String,
        trigger: String,
    },
    TurnFailed {
        error_kind: String,
        message: String,
    },
    TurnParked {
        cause: String,
        reason: String,
    },
    TurnResumed {
        reason: String,
    },
    TurnKilled {
        reason: String,
        strategy: String,
    },
    /// **THE ONE HONEST USAGE FRAME ON THE WIRE** (`0047` D4).
    ///
    /// `message_usage` carries the API's `message_start` PLACEHOLDER — measured
    /// output {5,3,8,3} against a true {1133,2034,394,850} for the same four
    /// messages — so it is not a usage number at all. This one is the per-TURN
    /// aggregate the daemon persists: final, per model, with cost. Reading the
    /// right producer instead of correcting the wrong one is what
    /// correctness-at-the-source means here.
    ResultTurn {
        subtype: String,
        is_error: bool,
        /// The turn's total, in dollars.
        total_cost_usd: Option<f64>,
        num_turns: Option<u64>,
        duration_ms: Option<u64>,
        stop_reason: Option<String>,
        /// `{model: {inputTokens, outputTokens, cacheCreationInputTokens,
        /// cacheReadInputTokens, thinkingTokens, costUSD, contextWindow}}`.
        /// Held as [`Value`] rather than typed: it is keyed by MODEL NAME, a
        /// set that grows without warning, and the renderer reads it by key.
        model_usage: Value,
    },
    Compacted {
        trigger: String,
        pre_tokens: Option<u64>,
        message_id_range: Option<(u64, u64)>,
    },
    /// `background_tasks_changed` — **THE SECOND `unknown`-SHAPED OFFENDER**
    /// (`0047` D5). 7,681 ledger rows at an average 996-byte payload, rendered
    /// as a clipped JSON blob because the kind was in [`KNOWN_KINDS`] and not
    /// in `TYPED_KINDS`. Typed here so it can say `background tasks · N
    /// running` instead.
    BackgroundTasksChanged {
        tasks: Vec<Value>,
    },
    /// **`unknown` IS A REAL DAEMON KIND** (`0047` D5) — `ThreadEvent::Unknown
    /// { raw }`, the daemon's own catch-all for SDK lines it does not model,
    /// and the second most common kind in the ledger at 236,384 rows.
    ///
    /// It was landing in [`ThreadEvent::Untyped`] and rendering `unknown ·
    /// {json}`, which is the row he ruled must never exist. Typing it here is
    /// what lets `store::shape` SPLIT it by `raw.type`/`raw.subtype` — nine
    /// live subtypes, some machinery to drop and some content to render.
    Unknown {
        raw: Value,
    },
    LimitHit {
        pool: Option<String>,
        reset: Option<String>,
        text: String,
    },
    ApiError {
        error: Option<String>,
        status: Option<u64>,
        text: String,
    },
    RateLimitEvent {
        rate_limit_type: String,
        utilization: f64,
        status: Option<String>,
    },
    BudgetTripped {
        budget_key: String,
        axis: String,
        consumed_pct: f64,
        limit_pct: f64,
        action: String,
    },
    BudgetsFailed {
        phase: String,
        error: String,
        outcome: String,
    },
    RetryScheduled {
        error_kind: String,
        reason: String,
        attempt: u32,
        retry_at: String,
    },
    RetryStarving {
        error_kind: String,
        attempt: u32,
        give_up_at: u32,
        account: String,
    },
    AccountLoginExpiredNotice {
        account: String,
        bar_until: String,
    },
    AccountSwitched {
        from: String,
        to: String,
        reason: String,
    },
    SettingsChanged {
        patch: Value,
    },
    PluginsChanged {
        added: Vec<String>,
        removed: Vec<String>,
        modified: Vec<String>,
    },
    ThreadSystemInterrupted {
        trigger: String,
        detail: String,
    },
    ThreadSystemResumed {
        trigger: String,
        elapsed: String,
    },
    ThreadSystemQueued {
        queued_for: String,
    },
    /// The raw SDK delta stream. It is the SAME WORDS `assistant_text` already
    /// carries, one fragment at a time, and on a busy thread it is most of the
    /// ledger — which is why it has its own filter lane and its own coalescing
    /// rule rather than a special case in the renderer.
    StreamEvent {
        raw: Value,
    },
    Custom {
        event_type: String,
        data: Value,
    },
    /// **Anything else, and it RENDERS.** `in_daemon_vocabulary` is
    /// [`KNOWN_KINDS`] membership; `why` is populated only when the kind WAS
    /// one we type and its body did not fit.
    Untyped {
        kind: String,
        in_daemon_vocabulary: bool,
        why: Option<String>,
        raw: Value,
    },
}

/// The serde half. Every field defaults; see [`ThreadEvent`] for why.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TaggedEvent {
    ThreadStarted {
        #[serde(default)]
        account: String,
        #[serde(default)]
        cwd: String,
        #[serde(default)]
        model: String,
    },
    UserMessage {
        #[serde(default)]
        text: String,
        #[serde(default)]
        metadata: Value,
        #[serde(default)]
        intent: Option<String>,
    },
    AssistantText {
        #[serde(default)]
        text: String,
        #[serde(default)]
        parent_tool_use_id: Option<String>,
    },
    Thinking {
        #[serde(default)]
        text: String,
    },
    ContextInjected {
        #[serde(default)]
        text: String,
    },
    SystemInit {
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        tools: Vec<String>,
        #[serde(default)]
        permission_mode: Option<String>,
    },
    ToolUse {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        input: Value,
        #[serde(default)]
        parent_tool_use_id: Option<String>,
    },
    ToolResult {
        #[serde(default)]
        tool_use_id: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        content: Value,
    },
    MessageUsage {
        #[serde(default)]
        model: String,
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
        #[serde(default)]
        cache_read_tokens: u64,
        #[serde(default)]
        cache_creation_tokens: u64,
    },
    AgentSpawn {
        #[serde(default)]
        subagent_type: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        run_in_background: bool,
    },
    AgentLaunched {
        #[serde(default)]
        agent_id: String,
        #[serde(default)]
        is_async: bool,
        #[serde(default)]
        status: String,
    },
    AgentCompleted {
        #[serde(default)]
        agent_id: String,
    },
    TaskStarted {
        #[serde(default)]
        task_id: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        subagent_type: Option<String>,
        /// **`local_agent` OR `local_bash`** — the one field that tells a
        /// subagent from a background shell on a `task_started`. Restored per
        /// `0080`: without it the face cannot split the two, and every
        /// per-`task_type` figure on the wire is unreadable.
        #[serde(default)]
        task_type: Option<String>,
        /// **THE LINK FROM A TASK TO THE TOOL CALL THAT LAUNCHED IT.** It is
        /// what attributes a backgrounded shell to the subagent that ran it,
        /// and what pairs a `task_id` — which is NOT unique, 128 of 2,590
        /// were started more than once — with the one launch it belongs to.
        #[serde(default)]
        tool_use_id: Option<String>,
    },
    TaskUpdated {
        #[serde(default)]
        task_id: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        error: Option<String>,
    },
    TaskProgress {
        #[serde(default)]
        task_id: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        subagent_type: Option<String>,
        #[serde(default)]
        last_tool_name: Option<String>,
        #[serde(default)]
        total_tokens: Option<u64>,
        #[serde(default)]
        tool_uses: Option<u64>,
        #[serde(default)]
        duration_ms: Option<u64>,
    },
    TaskNotification {
        #[serde(default)]
        task_id: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        total_tokens: Option<u64>,
        #[serde(default)]
        duration_ms: Option<u64>,
    },
    TurnStarted {
        #[serde(default)]
        attempt: u32,
    },
    TurnEnded {
        #[serde(default)]
        exit_code: Option<i32>,
    },
    TurnInterrupted {
        #[serde(default)]
        reason: String,
        #[serde(default)]
        trigger: String,
    },
    TurnFailed {
        #[serde(default)]
        error_kind: String,
        #[serde(default)]
        message: String,
    },
    TurnParked {
        #[serde(default)]
        cause: String,
        #[serde(default)]
        reason: String,
    },
    TurnResumed {
        #[serde(default)]
        reason: String,
    },
    TurnKilled {
        #[serde(default)]
        reason: String,
        #[serde(default)]
        strategy: String,
    },
    ResultTurn {
        #[serde(default)]
        subtype: String,
        #[serde(default)]
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
    },
    BackgroundTasksChanged {
        #[serde(default)]
        tasks: Vec<Value>,
    },
    Unknown {
        #[serde(default)]
        raw: Value,
    },
    Compacted {
        #[serde(default)]
        trigger: String,
        #[serde(default)]
        pre_tokens: Option<u64>,
        #[serde(default)]
        message_id_range: Option<(u64, u64)>,
    },
    LimitHit {
        #[serde(default)]
        pool: Option<String>,
        #[serde(default)]
        reset: Option<String>,
        #[serde(default)]
        text: String,
    },
    ApiError {
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        status: Option<u64>,
        #[serde(default)]
        text: String,
    },
    RateLimitEvent {
        #[serde(default)]
        rate_limit_type: String,
        #[serde(default)]
        utilization: f64,
        #[serde(default)]
        status: Option<String>,
    },
    BudgetTripped {
        #[serde(default)]
        budget_key: String,
        #[serde(default)]
        axis: String,
        #[serde(default)]
        consumed_pct: f64,
        #[serde(default)]
        limit_pct: f64,
        #[serde(default)]
        action: String,
    },
    BudgetsFailed {
        #[serde(default)]
        phase: String,
        #[serde(default)]
        error: String,
        #[serde(default)]
        outcome: String,
    },
    RetryScheduled {
        #[serde(default)]
        error_kind: String,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        attempt: u32,
        #[serde(default)]
        retry_at: String,
    },
    RetryStarving {
        #[serde(default)]
        error_kind: String,
        #[serde(default)]
        attempt: u32,
        #[serde(default)]
        give_up_at: u32,
        #[serde(default)]
        account: String,
    },
    AccountLoginExpiredNotice {
        #[serde(default)]
        account: String,
        #[serde(default)]
        bar_until: String,
    },
    AccountSwitched {
        #[serde(default)]
        from: String,
        #[serde(default)]
        to: String,
        #[serde(default)]
        reason: String,
    },
    SettingsChanged {
        #[serde(default)]
        patch: Value,
    },
    PluginsChanged {
        #[serde(default)]
        added: Vec<String>,
        #[serde(default)]
        removed: Vec<String>,
        #[serde(default)]
        modified: Vec<String>,
    },
    ThreadSystemInterrupted {
        #[serde(default)]
        trigger: String,
        #[serde(default)]
        detail: String,
    },
    ThreadSystemResumed {
        #[serde(default)]
        trigger: String,
        #[serde(default)]
        elapsed: String,
    },
    ThreadSystemQueued {
        #[serde(default)]
        queued_for: String,
    },
    StreamEvent {
        #[serde(default)]
        raw: Value,
    },
    Custom {
        #[serde(default)]
        event_type: String,
        #[serde(default)]
        data: Value,
    },
}

/// The kinds [`TaggedEvent`] claims. A kind in here that fails to parse is
/// SHAPE DRIFT and gets a `why`; a kind outside it was simply never specialized.
///
/// It is DERIVED from the table below rather than written twice — a second
/// hand-maintained list of the same thing is a list that goes stale, and
/// `a_typed_kind_is_never_reported_as_unspecialized` is the test that would
/// have caught it.
const TYPED_KINDS: &[&str] = &[
    "thread_started",
    "user_message",
    "assistant_text",
    "thinking",
    "context_injected",
    "system_init",
    "tool_use",
    "tool_result",
    "message_usage",
    "agent_spawn",
    "agent_launched",
    "agent_completed",
    "task_started",
    "task_updated",
    "task_progress",
    "task_notification",
    "turn_started",
    "turn_ended",
    "turn_interrupted",
    "turn_failed",
    "turn_parked",
    "turn_resumed",
    "turn_killed",
    "result_turn",
    "compacted",
    "limit_hit",
    "api_error",
    "rate_limit_event",
    "budget_tripped",
    "budgets_failed",
    "retry_scheduled",
    "retry_starving",
    "account_login_expired_notice",
    "account_switched",
    "settings_changed",
    "plugins_changed",
    "thread_system_interrupted",
    "thread_system_resumed",
    "thread_system_queued",
    "stream_event",
    "custom",
    // `0051`: both were in KNOWN_KINDS and NOT here, which is precisely why
    // they rendered `<kind> · {json}`. `unknown` is the daemon's own catch-all
    // kind (236,384 rows) and is now SPLIT by subtype in `store::shape`;
    // `background_tasks_changed` (7,681 rows, 996-byte payloads) is now a
    // one-liner. See `0047` D5.
    "background_tasks_changed",
    "unknown",
];

impl ThreadEvent {
    pub fn parse(v: Value) -> ThreadEvent {
        let kind = v
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("<no kind>")
            .to_string();
        match TaggedEvent::deserialize(&v) {
            Ok(t) => t.into_event(),
            Err(e) => {
                let why = TYPED_KINDS.contains(&kind.as_str()).then(|| e.to_string());
                ThreadEvent::Untyped {
                    in_daemon_vocabulary: KNOWN_KINDS.contains(&kind.as_str()),
                    kind,
                    why,
                    raw: v,
                }
            }
        }
    }

    /// **THE WORDS, for the three kinds whose body IS text** — the producer's
    /// own set (`user_message`, `assistant_text`, `thinking`), which is also
    /// the set whose text it lifts behind the resource door when it is large.
    pub fn body_text(&self) -> Option<&str> {
        match self {
            ThreadEvent::UserMessage { text, .. }
            | ThreadEvent::AssistantText { text, .. }
            | ThreadEvent::Thinking { text } => Some(text),
            _ => None,
        }
    }

    /// The wire `kind` of this event, whatever arm it landed in.
    pub fn kind(&self) -> &str {
        match self {
            ThreadEvent::ThreadStarted { .. } => "thread_started",
            ThreadEvent::UserMessage { .. } => "user_message",
            ThreadEvent::AssistantText { .. } => "assistant_text",
            ThreadEvent::Thinking { .. } => "thinking",
            ThreadEvent::ContextInjected { .. } => "context_injected",
            ThreadEvent::SystemInit { .. } => "system_init",
            ThreadEvent::ToolUse { .. } => "tool_use",
            ThreadEvent::ToolResult { .. } => "tool_result",
            ThreadEvent::MessageUsage { .. } => "message_usage",
            ThreadEvent::AgentSpawn { .. } => "agent_spawn",
            ThreadEvent::AgentLaunched { .. } => "agent_launched",
            ThreadEvent::AgentCompleted { .. } => "agent_completed",
            ThreadEvent::TaskStarted { .. } => "task_started",
            ThreadEvent::TaskUpdated { .. } => "task_updated",
            ThreadEvent::TaskProgress { .. } => "task_progress",
            ThreadEvent::TaskNotification { .. } => "task_notification",
            ThreadEvent::TurnStarted { .. } => "turn_started",
            ThreadEvent::TurnEnded { .. } => "turn_ended",
            ThreadEvent::TurnInterrupted { .. } => "turn_interrupted",
            ThreadEvent::TurnFailed { .. } => "turn_failed",
            ThreadEvent::TurnParked { .. } => "turn_parked",
            ThreadEvent::TurnResumed { .. } => "turn_resumed",
            ThreadEvent::TurnKilled { .. } => "turn_killed",
            ThreadEvent::ResultTurn { .. } => "result_turn",
            ThreadEvent::BackgroundTasksChanged { .. } => "background_tasks_changed",
            ThreadEvent::Unknown { .. } => "unknown",
            ThreadEvent::Compacted { .. } => "compacted",
            ThreadEvent::LimitHit { .. } => "limit_hit",
            ThreadEvent::ApiError { .. } => "api_error",
            ThreadEvent::RateLimitEvent { .. } => "rate_limit_event",
            ThreadEvent::BudgetTripped { .. } => "budget_tripped",
            ThreadEvent::BudgetsFailed { .. } => "budgets_failed",
            ThreadEvent::RetryScheduled { .. } => "retry_scheduled",
            ThreadEvent::RetryStarving { .. } => "retry_starving",
            ThreadEvent::AccountLoginExpiredNotice { .. } => "account_login_expired_notice",
            ThreadEvent::AccountSwitched { .. } => "account_switched",
            ThreadEvent::SettingsChanged { .. } => "settings_changed",
            ThreadEvent::PluginsChanged { .. } => "plugins_changed",
            ThreadEvent::ThreadSystemInterrupted { .. } => "thread_system_interrupted",
            ThreadEvent::ThreadSystemResumed { .. } => "thread_system_resumed",
            ThreadEvent::ThreadSystemQueued { .. } => "thread_system_queued",
            ThreadEvent::StreamEvent { .. } => "stream_event",
            ThreadEvent::Custom { .. } => "custom",
            ThreadEvent::Untyped { kind, .. } => kind,
        }
    }
}

impl TaggedEvent {
    fn into_event(self) -> ThreadEvent {
        use TaggedEvent as T;
        match self {
            T::ThreadStarted {
                account,
                cwd,
                model,
            } => ThreadEvent::ThreadStarted {
                account,
                cwd,
                model,
            },
            T::UserMessage {
                text,
                metadata,
                intent,
            } => ThreadEvent::UserMessage {
                text,
                metadata,
                intent,
            },
            T::AssistantText {
                text,
                parent_tool_use_id,
            } => ThreadEvent::AssistantText {
                text,
                parent_tool_use_id,
            },
            T::Thinking { text } => ThreadEvent::Thinking { text },
            T::ContextInjected { text } => ThreadEvent::ContextInjected { text },
            T::SystemInit {
                model,
                cwd,
                tools,
                permission_mode,
            } => ThreadEvent::SystemInit {
                model,
                cwd,
                tools,
                permission_mode,
            },
            T::ToolUse {
                id,
                name,
                input,
                parent_tool_use_id,
            } => ThreadEvent::ToolUse {
                id,
                name,
                input,
                parent_tool_use_id,
            },
            T::ToolResult {
                tool_use_id,
                is_error,
                content,
            } => ThreadEvent::ToolResult {
                tool_use_id,
                is_error,
                content,
            },
            T::MessageUsage {
                model,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
            } => ThreadEvent::MessageUsage {
                model,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
            },
            T::AgentSpawn {
                subagent_type,
                description,
                run_in_background,
            } => ThreadEvent::AgentSpawn {
                subagent_type,
                description,
                run_in_background,
            },
            T::AgentLaunched {
                agent_id,
                is_async,
                status,
            } => ThreadEvent::AgentLaunched {
                agent_id,
                is_async,
                status,
            },
            T::AgentCompleted { agent_id } => ThreadEvent::AgentCompleted { agent_id },
            T::TaskStarted {
                task_id,
                description,
                subagent_type,
                task_type,
                tool_use_id,
            } => ThreadEvent::TaskStarted {
                task_id,
                description,
                subagent_type,
                task_type,
                tool_use_id,
            },
            T::TaskUpdated {
                task_id,
                status,
                error,
            } => ThreadEvent::TaskUpdated {
                task_id,
                status,
                error,
            },
            T::TaskProgress {
                task_id,
                description,
                subagent_type,
                last_tool_name,
                total_tokens,
                tool_uses,
                duration_ms,
            } => ThreadEvent::TaskProgress {
                task_id,
                description,
                subagent_type,
                last_tool_name,
                total_tokens,
                tool_uses,
                duration_ms,
            },
            T::TaskNotification {
                task_id,
                status,
                summary,
                total_tokens,
                duration_ms,
            } => ThreadEvent::TaskNotification {
                task_id,
                status,
                summary,
                total_tokens,
                duration_ms,
            },
            T::TurnStarted { attempt } => ThreadEvent::TurnStarted { attempt },
            T::TurnEnded { exit_code } => ThreadEvent::TurnEnded { exit_code },
            T::TurnInterrupted { reason, trigger } => {
                ThreadEvent::TurnInterrupted { reason, trigger }
            }
            T::TurnFailed {
                error_kind,
                message,
            } => ThreadEvent::TurnFailed {
                error_kind,
                message,
            },
            T::TurnParked { cause, reason } => ThreadEvent::TurnParked { cause, reason },
            T::TurnResumed { reason } => ThreadEvent::TurnResumed { reason },
            T::TurnKilled { reason, strategy } => ThreadEvent::TurnKilled { reason, strategy },
            T::ResultTurn {
                subtype,
                is_error,
                total_cost_usd,
                num_turns,
                duration_ms,
                stop_reason,
                model_usage,
            } => ThreadEvent::ResultTurn {
                subtype,
                is_error,
                total_cost_usd,
                num_turns,
                duration_ms,
                stop_reason,
                model_usage,
            },
            T::BackgroundTasksChanged { tasks } => ThreadEvent::BackgroundTasksChanged { tasks },
            T::Unknown { raw } => ThreadEvent::Unknown { raw },
            T::Compacted {
                trigger,
                pre_tokens,
                message_id_range,
            } => ThreadEvent::Compacted {
                trigger,
                pre_tokens,
                message_id_range,
            },
            T::LimitHit { pool, reset, text } => ThreadEvent::LimitHit { pool, reset, text },
            T::ApiError {
                error,
                status,
                text,
            } => ThreadEvent::ApiError {
                error,
                status,
                text,
            },
            T::RateLimitEvent {
                rate_limit_type,
                utilization,
                status,
            } => ThreadEvent::RateLimitEvent {
                rate_limit_type,
                utilization,
                status,
            },
            T::BudgetTripped {
                budget_key,
                axis,
                consumed_pct,
                limit_pct,
                action,
            } => ThreadEvent::BudgetTripped {
                budget_key,
                axis,
                consumed_pct,
                limit_pct,
                action,
            },
            T::BudgetsFailed {
                phase,
                error,
                outcome,
            } => ThreadEvent::BudgetsFailed {
                phase,
                error,
                outcome,
            },
            T::RetryScheduled {
                error_kind,
                reason,
                attempt,
                retry_at,
            } => ThreadEvent::RetryScheduled {
                error_kind,
                reason,
                attempt,
                retry_at,
            },
            T::RetryStarving {
                error_kind,
                attempt,
                give_up_at,
                account,
            } => ThreadEvent::RetryStarving {
                error_kind,
                attempt,
                give_up_at,
                account,
            },
            T::AccountLoginExpiredNotice { account, bar_until } => {
                ThreadEvent::AccountLoginExpiredNotice { account, bar_until }
            }
            T::AccountSwitched { from, to, reason } => {
                ThreadEvent::AccountSwitched { from, to, reason }
            }
            T::SettingsChanged { patch } => ThreadEvent::SettingsChanged { patch },
            T::PluginsChanged {
                added,
                removed,
                modified,
            } => ThreadEvent::PluginsChanged {
                added,
                removed,
                modified,
            },
            T::ThreadSystemInterrupted { trigger, detail } => {
                ThreadEvent::ThreadSystemInterrupted { trigger, detail }
            }
            T::ThreadSystemResumed { trigger, elapsed } => {
                ThreadEvent::ThreadSystemResumed { trigger, elapsed }
            }
            T::ThreadSystemQueued { queued_for } => ThreadEvent::ThreadSystemQueued { queued_for },
            T::StreamEvent { raw } => ThreadEvent::StreamEvent { raw },
            T::Custom { event_type, data } => ThreadEvent::Custom { event_type, data },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_auth_frame_is_what_the_daemon_reads() {
        // The firehose half of this test went with `subscribe_firehose_frame`
        // (R2.S4 retired the firehose; R2.S9 deleted the builder).
        let v: Value = serde_json::from_str(&auth_frame("s3cr3t")).unwrap();
        assert_eq!(v["type"], "auth");
        assert_eq!(v["secret"], "s3cr3t");
    }

    #[test]
    fn welcome_carries_the_three_things_reconnect_turns_on() {
        let m = ServerMsg::parse(
            r#"{"type":"welcome","proto":1,"daemon":"threads","daemon_version":"0.1.0",
                "boot_id":"abc","auth_required":false}"#,
        );
        match m {
            ServerMsg::Welcome {
                proto,
                boot_id,
                auth_required,
                ..
            } => {
                assert_eq!(proto, PROTO);
                assert_eq!(boot_id, "abc");
                assert!(!auth_required);
            }
            other => panic!("welcome did not parse: {other:?}"),
        }
    }

    #[test]
    fn lagged_keeps_every_cursor_because_each_one_is_a_replay_instruction() {
        let m = ServerMsg::parse(
            r#"{"type":"lagged","dropped":17,"recover":"…","cursors":[
                 {"thread_id":"a","last_seq":9},{"thread_id":"b","last_seq":4}]}"#,
        );
        match m {
            ServerMsg::Lagged {
                dropped, cursors, ..
            } => {
                assert_eq!(dropped, 17);
                assert_eq!(cursors.len(), 2);
                assert_eq!(cursors[1].last_seq, 4);
            }
            other => panic!("lagged did not parse: {other:?}"),
        }
    }

    #[test]
    fn an_unrecognized_frame_type_is_kept_whole_and_never_fatal() {
        let m = ServerMsg::parse(r#"{"type":"quantum_flux","payload":{"n":1}}"#);
        match m {
            ServerMsg::Unrecognized {
                frame_type, raw, ..
            } => {
                assert_eq!(frame_type, "quantum_flux");
                assert_eq!(raw["payload"]["n"], 1, "the payload must survive");
            }
            other => panic!("an unknown frame must not resolve to {other:?}"),
        }
    }

    #[test]
    fn even_malformed_json_comes_back_as_a_frame_rather_than_a_drop() {
        let m = ServerMsg::parse("{not json");
        assert!(matches!(m, ServerMsg::Unrecognized { .. }));
    }

    #[test]
    fn an_unknown_event_kind_renders_as_unknown_and_keeps_its_payload() {
        let m = ServerMsg::parse(
            r#"{"type":"event","thread_id":"t","seq":3,"ts":"now",
                "event":{"kind":"neutrino_detected","energy":42}}"#,
        );
        let ServerMsg::Event { event, seq, .. } = m else {
            panic!("the envelope must still parse when the payload does not")
        };
        assert_eq!(seq, 3, "an unreadable payload must not cost us the seq");
        match event {
            ThreadEvent::Untyped {
                kind,
                in_daemon_vocabulary,
                raw,
                why,
            } => {
                assert_eq!(kind, "neutrino_detected");
                assert!(
                    !in_daemon_vocabulary,
                    "a kind nothing has heard of must not read as a merely-unspecialized one"
                );
                assert_eq!(why, None, "this is a new kind, not shape drift");
                assert_eq!(raw["energy"], 42);
            }
            other => panic!("an unknown kind must not resolve to {other:?}"),
        }
    }

    #[test]
    fn a_daemon_kind_we_have_not_specialized_is_not_reported_as_unknown() {
        // `agent_listing_delta` is real and documented; this build simply does
        // not project it yet. Saying "unknown" would be a lie about the daemon.
        //
        // It used to be `plugins_changed`, which step 3 then typed — and that
        // is the right way for this test to go red: the fixture has to be a
        // kind that is still unspecialized, so the list it is testing against
        // is the list that has to be maintained.
        let ev = ThreadEvent::parse(serde_json::json!({"kind":"agent_listing_delta","raw":{}}));
        match ev {
            ThreadEvent::Untyped {
                in_daemon_vocabulary,
                kind,
                ..
            } => {
                assert!(in_daemon_vocabulary, "{kind} is in the daemon's own table");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn shape_drift_on_a_kind_we_do_type_is_kept_and_named_as_drift() {
        // `tool_use.input` became a string where we expect a Value — a Value
        // accepts that, so force the drift on a REQUIRED tag instead: a
        // `compacted` whose `trigger` is an object.
        let ev = ThreadEvent::parse(serde_json::json!({"kind":"compacted","trigger":{"x":1}}));
        match ev {
            ThreadEvent::Untyped {
                kind,
                in_daemon_vocabulary,
                why,
                raw,
            } => {
                assert_eq!(kind, "compacted");
                assert!(in_daemon_vocabulary);
                assert!(
                    why.is_some(),
                    "drift on a typed kind must say WHY it fell back"
                );
                assert_eq!(raw["trigger"]["x"], 1, "the payload must survive drift too");
            }
            other => panic!("drift must fall back, not resolve to {other:?}"),
        }
    }

    #[test]
    fn the_typed_kinds_are_all_in_the_daemons_own_vocabulary() {
        // Guards the transcription: a typo in `TYPED_KINDS` would otherwise be
        // invisible until a live frame took the wrong arm.
        for k in TYPED_KINDS {
            assert!(
                KNOWN_KINDS.contains(k),
                "{k} is typed here but is not a kind the daemon emits"
            );
        }
        assert_eq!(KNOWN_KINDS.len(), 44, "the daemon's table has 44 kinds");
    }
}
