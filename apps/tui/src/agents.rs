//! **THE SUBAGENT AGGREGATION — one record per subagent, out of the parent's
//! own event stream.**
//!
//! > *I just want to see the subagents, and their subagents. It looks like a
//! > file tree. See if you can copy claude code's view. With token size, #
//! > calls, etc. I don't want to see them in the chat area, it takes up 90% of
//! > the chat area.*
//!
//! A `direct` subagent — every subagent that exists today — runs IN PROCESS
//! inside Claude Code. It has no thread of its own and no address: its events
//! arrive on the PARENT thread's stream, each carrying a `parent_tool_use_id`
//! naming the `Agent` tool call that launched it. So a subagent is not a thing
//! the daemon can be asked about; it is a thing this module derives.
//!
//! **THE FIVE FACTS THIS MODULE IS BUILT AROUND, every one of them measured
//! (`0080`) rather than reasoned:**
//!
//! 1. **`task_id` IS NOT UNIQUE.** A resumed agent reuses it with a different
//!    `tool_use_id` — 128 of 2,590. So *"a `task_started` with no later
//!    `task_notification`"* is an ILL-FORMED PREDICATE: at one instant it
//!    reported **13 subagents running where the CLI's roster said 1 and the
//!    operating system said 0**, twelve of them dead since September. Nothing
//!    here pairs edges; the identity is the TOOL CALL, and a `task_id` is
//!    resolved to a launch by its LATEST start.
//! 2. **RUNNING-NESS IS A LEVEL SIGNAL.** `background_tasks_changed` carries
//!    the CLI's entire roster of what is backgrounded right now, rebroadcast on
//!    every change. The newest one in the tail IS the truth; its age is not
//!    staleness.
//! 3. **`agent_completed` IS 99.8% PREMATURE, mean lead 21 MINUTES.** It is
//!    never read here. The terminal signal is `task_notification`.
//! 4. **TOKENS ARE AVAILABLE FOR SUBAGENTS 99.7% OF THE TIME.** The "usually
//!    absent" reading came from a bash-dominated denominator; a shell has no
//!    token count, correctly.
//! 5. **THERE IS NO PID PER SUBAGENT AND `ps` IS WRONG IN BOTH DIRECTIONS.**
//!    The live turn's `claude` had zero children while a subagent ran; another
//!    had five persistent shells while running nothing. The honest figure is
//!    BASH CALLS IN FLIGHT and it is named for exactly that.
//!
//! **THE SEAM.** [`SubagentClass`] is the whole of `direct`/`managed`: one
//! enum, one field, one naming convention (`direct_attribute` is the direct
//! class's attribution and says so in its name). `managed` — a subagent that
//! is a thread of its own, which nothing constructs today — costs one more
//! function here and a parent link in the daemon. No adapter, no registry, no
//! trait with one implementor.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::Value;
use tui_render::model::RowKey;
use tui_render::{SubagentClass, SubagentState};

use crate::proto::ThreadEvent;
use crate::store::LedgerRow;

/// **ONE SUBAGENT, AGGREGATED.** The record the tree draws a row from and the
/// transcript draws one tile from — the same record in two places, which is
/// what stops the two from disagreeing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentRow {
    /// **THE OPAQUE ID OF THE `Agent` TOOL CALL THAT LAUNCHED IT.** The
    /// identity, and deliberately not the `task_id`: a resumed agent reuses
    /// that one.
    pub id: String,
    pub class: SubagentClass,
    /// The launching call's own `parent_tool_use_id` — which subagent this one
    /// is nested under, `None` at the top level.
    pub parent: Option<String>,
    /// `"Explore · scout the raw list"` — the subagent type and the
    /// description it was launched with.
    pub label: String,
    pub state: SubagentState,
    /// **WHERE THE TILE SITS IN THE TRANSCRIPT: the LAUNCH seq.** The one
    /// position that never relocates. An aggregate at its LAST-EVENT position
    /// moves down the transcript on every beat, which is the relocation the
    /// block-count freeze cannot hold still.
    pub launched_at: RowKey,
    pub started: Option<DateTime<Utc>>,
    pub ended: Option<DateTime<Utc>>,
    pub tokens: Option<u64>,
    pub calls: Option<u64>,
    /// **BASH CALLS IN FLIGHT.** Never OS processes. See the module header.
    pub bash_in_flight: Option<u32>,
    pub agents_running: Option<u32>,
    /// The verbatim instruction this subagent was given — `Agent.input.prompt`
    /// for a DIRECT INVOCATION, `SendMessage.input.message` for a message. It
    /// is the first "user" turn of the subagent's own chat.
    pub invocation: Option<String>,
    /// Whether the launching call was `Agent` (a direct invocation) or
    /// `SendMessage` (a message to one already running). The tool NAME, not a
    /// heuristic.
    pub invocation_is_direct: bool,
}

/// What one `task_started` said, once its `task_id` has been resolved to the
/// launch it belongs to.
#[derive(Debug, Clone, Default)]
struct TaskFacts {
    tool_use_id: Option<String>,
    task_type: Option<String>,
    tokens: Option<u64>,
    calls: Option<u64>,
    /// `task_notification`'s arrival — the ONE terminal signal.
    ended: Option<DateTime<Utc>>,
    ended_ok: bool,
}

/// **THE DIRECT CLASS'S ATTRIBUTION** — every claude-code subagent in this
/// tail, with its four figures, in LAUNCH order.
///
/// The name carries the seam: this is what `direct` costs. `managed` would be
/// a sibling function reading the daemon's parent link, and the two would meet
/// at [`SubagentRow`] — which is why the class is a field on the record rather
/// than a shape of it.
pub fn direct_attribute(rows: &[(&str, &LedgerRow)], now: DateTime<Utc>) -> Vec<SubagentRow> {
    let _ = now;
    // ── 1. THE LAUNCHES. A subagent IS an `Agent` (or `SendMessage`) call. ──
    let mut launches: Vec<SubagentRow> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    // Every tool call in the tail, so a Bash call can be attributed and a
    // result can close it.
    let mut bash_open: HashMap<String, String> = HashMap::new(); // bash tool_use id -> owning subagent
    let mut results: HashSet<String> = HashSet::new();
    let mut calls_seen: HashMap<String, u64> = HashMap::new();

    for (thread_id, r) in rows {
        match &r.event {
            ThreadEvent::ToolUse {
                id,
                name,
                input,
                parent_tool_use_id,
            } => {
                if let Some(owner) = parent_tool_use_id {
                    *calls_seen.entry(owner.clone()).or_insert(0) += 1;
                    if name == "Bash" {
                        bash_open.insert(id.clone(), owner.clone());
                    }
                }
                let direct = name == "Agent";
                if !direct && name != "SendMessage" {
                    continue;
                }
                let key: RowKey = (thread_id.to_string(), r.seq);
                by_id.insert(id.clone(), launches.len());
                launches.push(SubagentRow {
                    id: id.clone(),
                    class: SubagentClass::Direct,
                    parent: parent_tool_use_id.clone(),
                    label: label_of(input, direct),
                    state: SubagentState::Unknown,
                    launched_at: key,
                    started: r.ts,
                    ended: None,
                    tokens: None,
                    calls: None,
                    bash_in_flight: None,
                    agents_running: None,
                    invocation: invocation_of(input, direct),
                    invocation_is_direct: direct,
                });
            }
            ThreadEvent::ToolResult { tool_use_id, .. } => {
                results.insert(tool_use_id.clone());
            }
            _ => {}
        }
    }
    if launches.is_empty() {
        return launches;
    }

    // ── 2. THE TASKS, PAIRED BY LATEST START. ─────────────────────────────
    //
    // `task_id` is not unique. A later `task_started` on a seen id is a RESUME
    // and it supersedes the earlier one wholesale — its own `tool_use_id` is
    // the launch the figures now belong to. Pairing any other way both
    // resurrects the dead and loses the live, measured.
    let mut tasks: HashMap<String, TaskFacts> = HashMap::new();
    for (_, r) in rows {
        match &r.event {
            ThreadEvent::TaskStarted {
                task_id,
                task_type,
                tool_use_id,
                ..
            } => {
                let f = tasks.entry(task_id.clone()).or_default();
                *f = TaskFacts {
                    tool_use_id: tool_use_id.clone(),
                    task_type: task_type.clone(),
                    ..TaskFacts::default()
                };
            }
            ThreadEvent::TaskProgress {
                task_id,
                total_tokens,
                tool_uses,
                ..
            } => {
                let f = tasks.entry(task_id.clone()).or_default();
                if total_tokens.is_some() {
                    f.tokens = *total_tokens;
                }
                if tool_uses.is_some() {
                    f.calls = *tool_uses;
                }
            }
            ThreadEvent::TaskNotification {
                task_id,
                status,
                total_tokens,
                ..
            } => {
                let f = tasks.entry(task_id.clone()).or_default();
                // **THE ONE TERMINAL SIGNAL.** `agent_completed` is not read
                // anywhere in this module; see the header for the measurement.
                f.ended = r.ts.or(Some(r.received));
                f.ended_ok = !matches!(status.as_deref(), Some("error") | Some("failed"));
                if total_tokens.is_some() {
                    f.tokens = *total_tokens;
                }
            }
            _ => {}
        }
    }

    // ── 3. THE ROSTER — the LEVEL signal, newest wins. ────────────────────
    let roster = latest_roster(rows);
    // Which launches the roster says are running, and which backgrounded
    // shells belong to which launch.
    let mut running: HashSet<String> = HashSet::new();
    let mut bash_running: HashMap<String, u32> = HashMap::new();
    for task_id in &roster {
        let Some(f) = tasks.get(task_id) else { continue };
        let Some(tu) = f.tool_use_id.as_ref() else {
            continue;
        };
        match f.task_type.as_deref() {
            Some("local_bash") => {
                // A backgrounded shell. It is attributed to the subagent that
                // called it, through the Bash tool_use's own parent.
                if let Some(owner) = bash_open.get(tu) {
                    *bash_running.entry(owner.clone()).or_insert(0) += 1;
                }
            }
            _ => {
                running.insert(tu.clone());
            }
        }
    }

    // ── 4. FOLD THE TASK FACTS ONTO THEIR LAUNCHES. ───────────────────────
    for f in tasks.values() {
        let Some(tu) = f.tool_use_id.as_ref() else {
            continue;
        };
        let Some(&i) = by_id.get(tu) else { continue };
        let row = &mut launches[i];
        if f.tokens.is_some() {
            row.tokens = f.tokens;
        }
        if f.calls.is_some() {
            row.calls = f.calls;
        }
        if f.ended.is_some() {
            row.ended = f.ended;
            row.state = if f.ended_ok {
                SubagentState::Done
            } else {
                SubagentState::Failed
            };
        }
    }

    // ── 5. THE LIVE FIGURES. ──────────────────────────────────────────────
    for row in launches.iter_mut() {
        // **THE ROSTER OVERRULES.** A task that is on it is running, whatever
        // an earlier notification for a reused id said.
        if running.contains(&row.id) {
            row.state = SubagentState::Running;
            row.ended = None;
        }
        // Fallback for calls: what its trail actually shows. A count of rows
        // we hold is a true statement about what we hold.
        if row.calls.is_none() {
            row.calls = calls_seen.get(&row.id).copied();
        }
    }
    // Bash in flight: a `Bash` call of this subagent's with no result yet,
    // UNION the roster's backgrounded shells attributed to it. A set, because
    // a backgrounded shell is both.
    let mut bash: HashMap<String, HashSet<String>> = HashMap::new();
    for (bid, owner) in &bash_open {
        if !results.contains(bid) {
            bash.entry(owner.clone()).or_default().insert(bid.clone());
        }
    }
    for (owner, n) in &bash_running {
        let e = bash.entry(owner.clone()).or_default();
        for k in 0..*n {
            e.insert(format!("\u{0}roster{k}"));
        }
    }
    // Children running, from the roster and never edge-derived.
    let mut kids: HashMap<String, u32> = HashMap::new();
    for row in &launches {
        if let Some(p) = row.parent.as_ref() {
            if row.state == SubagentState::Running {
                *kids.entry(p.clone()).or_insert(0) += 1;
            }
        }
    }
    for row in launches.iter_mut() {
        row.bash_in_flight = bash.get(&row.id).map(|s| s.len() as u32);
        row.agents_running = kids.get(&row.id).copied();
    }
    launches
}

/// **THE NEWEST `background_tasks_changed` IN THE TAIL, AS A SET OF TASK IDS.**
///
/// It is a LEVEL: the CLI rebroadcasts its whole roster on every change, and
/// the empty array is the legitimate "everything finished" broadcast rather
/// than a gap. Its AGE IS NOT STALENESS — measured at 25 minutes old and
/// correct.
///
/// **A ROSTER OLDER THAN ITS THREAD'S LAST `turn_ended` IS VOID** (R10.H1,
/// TC-13; an internal report D2). A daemon thread's `claude` lives
/// for one turn: when the turn ends, everything that roster listed died with
/// it. A CLI that died after a non-empty roster never broadcasts the empty
/// one, and "the roster overrules" would then pin those agents to Running
/// forever. So the newest roster counts only while no `turn_ended` of the SAME
/// thread follows it; otherwise the roster is empty (each agent falls back to
/// its own terminal signal, or `no end seen`).
fn latest_roster(rows: &[(&str, &LedgerRow)]) -> Vec<String> {
    let mut ended: HashSet<&str> = HashSet::new();
    for (thread_id, r) in rows.iter().rev() {
        match &r.event {
            ThreadEvent::TurnEnded { .. } => {
                ended.insert(*thread_id);
            }
            ThreadEvent::BackgroundTasksChanged { tasks } => {
                if ended.contains(thread_id) {
                    return Vec::new();
                }
                return tasks
                    .iter()
                    .filter_map(|t| t.get("task_id").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }
    Vec::new()
}

/// `"Explore · scout the raw list"`. The subagent's type and what it was asked
/// for, which is how Claude Code's own view names one.
fn label_of(input: &Value, direct: bool) -> String {
    let kind = input
        .get("subagent_type")
        .and_then(Value::as_str)
        .map(str::to_string);
    let what = input
        .get("description")
        .and_then(Value::as_str)
        .or_else(|| input.get("content").and_then(Value::as_str))
        .map(str::to_string);
    let what = what.map(|s| crate::store::flat(&s));
    match (kind, what) {
        (Some(k), Some(w)) => format!("{k} · {w}"),
        (Some(k), None) => k,
        (None, Some(w)) => w,
        (None, None) => {
            if direct {
                "agent".into()
            } else {
                "message".into()
            }
        }
    }
}

/// **THE DIRECT INVOCATION.** His hedge — *"If it's possible to detect"* —
/// resolves to YES and it is the TOOL NAME, not an inference: `Agent` carries
/// `input.prompt`, `SendMessage` carries `input.message`.
fn invocation_of(input: &Value, direct: bool) -> Option<String> {
    let k = if direct { "prompt" } else { "message" };
    input
        .get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(seq: u64, event: ThreadEvent) -> LedgerRow {
        LedgerRow { seq, message_id: None, ts: None, received: Utc::now(), event }
    }

    /// Two direct agents launched in thread `t`, both on the newest roster.
    fn launched() -> Vec<LedgerRow> {
        let mut v = Vec::new();
        for (i, (tu, task)) in [("ag1", "T1"), ("ag2", "T2")].iter().enumerate() {
            let s = (i as u64) * 2;
            v.push(row(s + 1, ThreadEvent::ToolUse {
                id: tu.to_string(),
                name: "Agent".into(),
                input: json!({"subagent_type": "Explore", "description": "scout"}),
                parent_tool_use_id: None,
            }));
            v.push(row(s + 2, ThreadEvent::TaskStarted {
                task_id: task.to_string(),
                description: None,
                subagent_type: None,
                task_type: Some("local_agent".into()),
                tool_use_id: Some(tu.to_string()),
            }));
        }
        v.push(row(5, ThreadEvent::BackgroundTasksChanged {
            tasks: vec![json!({"task_id": "T1", "task_type": "local_agent"}), json!({"task_id": "T2", "task_type": "local_agent"})],
        }));
        v
    }

    fn states(rows: &[LedgerRow], thread_of: impl Fn(u64) -> &'static str) -> Vec<SubagentState> {
        let tagged: Vec<(&str, &LedgerRow)> = rows.iter().map(|r| (thread_of(r.seq), r)).collect();
        direct_attribute(&tagged, Utc::now()).into_iter().map(|r| r.state).collect()
    }

    /// **R10.H1 (TC-13, HEALTH D2) — A ROSTER OLDER THAN ITS THREAD'S LAST
    /// `turn_ended` IS VOID; A FRESH ONE IS KEPT. N = 2 agents.**
    ///
    /// Opposing arms: the same roster with no `turn_ended` after it is Running
    /// for both (a constant "void every roster" fails); a `turn_ended` of a
    /// DIFFERENT thread does not void it (a "void on any turn_ended" fails);
    /// a roster broadcast after the `turn_ended` (a resumed turn) counts again.
    ///
    /// FORCED: delete the `ended.contains` void in `latest_roster` and the
    /// stale arm reads Running, Running.
    #[test]
    fn a_roster_older_than_the_threads_last_turn_ended_is_void_and_a_fresh_one_is_kept() {
        let fresh = launched();
        assert_eq!(states(&fresh, |_| "t"), vec![SubagentState::Running, SubagentState::Running], "fresh roster");

        let mut stale = launched();
        stale.push(row(6, ThreadEvent::TurnEnded { exit_code: Some(143) }));
        assert_eq!(
            states(&stale, |_| "t"),
            vec![SubagentState::Unknown, SubagentState::Unknown],
            "a roster from before the turn ended pinned its agents Running"
        );

        // Another thread's turn end does not void THIS thread's roster.
        assert_eq!(
            states(&stale, |s| if s == 6 { "other" } else { "t" }),
            vec![SubagentState::Running, SubagentState::Running],
            "another thread's turn_ended voided this roster"
        );

        // A roster broadcast after the turn end (a resumed turn) counts again.
        let mut resumed = stale.clone();
        resumed.push(row(7, ThreadEvent::BackgroundTasksChanged {
            tasks: vec![json!({"task_id": "T2", "task_type": "local_agent"})],
        }));
        assert_eq!(states(&resumed, |_| "t"), vec![SubagentState::Unknown, SubagentState::Running]);
    }
}
