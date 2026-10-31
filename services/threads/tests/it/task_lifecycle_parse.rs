//! **Parses REAL captured stream-json, not hand-written JSON.**
//!
//! Every line in `tests/fixtures/task_lifecycle.jsonl` was copied verbatim out
//! of `lib/threads/turns/` — the daemon's own capture of live `claude
//! --output-format stream-json` output. Nothing was edited, reformatted or
//! trimmed; the file was assembled by selecting whole lines, so that this suite
//! fails when the parser disagrees with what the CLI actually emits rather than
//! when it disagrees with what someone assumed it emits. It is copied in only
//! so the test is hermetic — `turns/` is 846 MB and is not a test input.
//!
//! Why these tests exist at all: two families of key in `stream.rs` were
//! written against an older wire shape and had been reading `null` against
//! every line the CLI emits.
//!
//! * `toolUseResult` → `tool_use_result`. Zero occurrences of the old spelling
//!   in 371,702 captured lines. This is the bug that kept `Liveness.outstanding`
//!   empty forever, which kept `busy()` from ever counting a subagent, which
//!   kept `AgentCompleted` from ever being emitted, which left 537,322 ledger
//!   events containing exactly zero `agent_launched` rows.
//! * `compactMetadata.preTokens` → `compact_metadata.pre_tokens`. Same story;
//!   all ten `compacted` ledger rows read `trigger: "unknown"` and a null token
//!   count.
//!
//! And one key that was never there in any spelling: `toolUseResult.toolUseId`.
//!
//! The fixture's first eleven lines are ONE REAL THREE-LEVEL AGENT CHAIN,
//! captured live: a top-level `general-purpose` agent launched a second
//! `general-purpose` agent, which launched an `Explore` agent. It is kept whole
//! because the attribution join is the thing most likely to be got wrong, and a
//! chain of depth three is the shallowest one that can tell a correct recursive
//! join from a lucky one-hop.

use std::collections::HashMap;

use serde_json::{json, Value};
use threads::event::ThreadEvent;
use threads::stream::{parse_line, parse_raw_line};

const FIXTURE: &str = include_str!("../fixtures/task_lifecycle.jsonl");

fn fixture_lines() -> Vec<Value> {
    FIXTURE
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("fixture line is valid JSON"))
        .collect()
}

/// Every event the fixture produces, in order.
fn fixture_events() -> Vec<ThreadEvent> {
    FIXTURE
        .lines()
        .filter(|l| !l.trim().is_empty())
        .flat_map(parse_raw_line)
        .collect()
}

/// The fixture is real and finite; find the one line matching a predicate.
fn line_where(pred: impl Fn(&Value) -> bool) -> Value {
    fixture_lines()
        .into_iter()
        .find(|v| pred(v))
        .expect("fixture is missing a line this test needs")
}

// ── change 1: the casing bug ────────────────────────────────────────────

/// The whole consequence chain in one assertion: a REAL `tool_use_result`
/// record must yield a REAL `AgentLaunched`.
///
/// Before the fix this line produced a single `ThreadEvent::Unknown` for the
/// launch (the `tool_result` block still parsed) and `outstanding` stayed empty.
#[test]
fn real_snake_case_tool_use_result_yields_agent_launched() {
    let v = line_where(|v| {
        v.get("tool_use_result")
            .and_then(|t| t.get("status"))
            .and_then(Value::as_str)
            == Some("async_launched")
    });
    // Guard the premise: this really is the new spelling and not the old one.
    assert!(v.get("tool_use_result").is_some(), "fixture premise");
    assert!(
        v.get("toolUseResult").is_none(),
        "the CLI does not emit the camelCase spelling any more"
    );

    let evs = parse_line(&v);
    let launched = evs
        .iter()
        .find(|e| matches!(e, ThreadEvent::AgentLaunched { .. }))
        .expect("an async launch must produce AgentLaunched");

    let ThreadEvent::AgentLaunched {
        tool_use_id,
        agent_id,
        is_async,
        status,
        output_file,
    } = launched
    else {
        unreachable!()
    };

    assert_eq!(status, "async_launched");
    assert!(is_async, "isAsync is still camelCase INSIDE the result object");
    assert!(!agent_id.is_empty());
    assert!(
        output_file.as_deref().is_some_and(|p| p.ends_with(".output")),
        "outputFile is the mtime heartbeat sweep_subagents watches: {output_file:?}"
    );

    // THE SECOND BUG, and the one a pure casing swap would have left in place:
    // `toolUseResult.toolUseId` does not exist on any captured record, in any
    // spelling. Fixing only the envelope key would have populated `outstanding`
    // with launches whose `tool_use_id` was the empty string — every launch
    // indistinguishable from every other.
    assert!(
        v["tool_use_result"].get("toolUseId").is_none(),
        "the id is NOT in the result object"
    );
    assert!(
        !tool_use_id.is_empty(),
        "tool_use_id must be recovered from the sibling tool_result block"
    );
    let sibling = v["message"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "tool_result")
        .unwrap()["tool_use_id"]
        .as_str()
        .unwrap();
    assert_eq!(tool_use_id, sibling);
}

/// A sync-completed Agent result is NOT an async launch: `interpret` keys the
/// liveness insert on `is_async && status != "completed"`, so getting this
/// wrong would make `outstanding` fill with agents that had already finished.
#[test]
fn real_sync_completed_agent_result_is_not_async() {
    let v = line_where(|v| {
        v.get("tool_use_result")
            .is_some_and(|t| t.get("agentId").is_some() && t["status"] == "completed")
    });
    let evs = parse_line(&v);
    let ThreadEvent::AgentLaunched {
        is_async, status, ..
    } = evs
        .iter()
        .find(|e| matches!(e, ThreadEvent::AgentLaunched { .. }))
        .expect("still an AgentLaunched, just a finished one")
    else {
        unreachable!()
    };
    assert!(!is_async, "no isAsync key at all on the sync shape");
    assert_eq!(status, "completed");
}

/// The compaction drift went all the way down: envelope AND interior.
#[test]
fn real_compact_boundary_reads_trigger_and_pre_tokens() {
    let v = line_where(|v| v["subtype"] == "compact_boundary");
    assert!(v.get("compact_metadata").is_some(), "fixture premise");
    assert!(v["compact_metadata"].get("preTokens").is_none());

    let evs = parse_line(&v);
    let ThreadEvent::Compacted {
        trigger, pre_tokens, ..
    } = &evs[0]
    else {
        panic!("expected Compacted, got {:?}", evs[0])
    };
    assert!(
        trigger == "auto" || trigger == "manual",
        "every captured compaction recorded `unknown` before the fix: {trigger}"
    );
    assert!(
        pre_tokens.is_some_and(|t| t > 0),
        "pre_tokens read null on all ten ledger rows before the fix"
    );
}

/// **The reason we accept both spellings instead of swapping one for the
/// other.** The capture cannot date the rename — the daemon only began writing
/// turn files on 2026-07-30 while the ledger goes back to 2026-04-22 — so it
/// cannot rule out a live thread still speaking camelCase. A reader that
/// handles either is correct on both sides of a boundary nobody can locate.
#[test]
fn legacy_camel_case_spellings_still_parse() {
    let camel = json!({
        "type": "user",
        "toolUseResult": {
            "agentId": "a19856ee9986244ad",
            "isAsync": true,
            "status": "async_launched",
            "outputFile": "/tmp/x.output"
        },
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_legacy", "content": "ok"}
        ]}
    });
    let evs = parse_line(&camel);
    assert!(
        evs.iter().any(|e| matches!(
            e,
            ThreadEvent::AgentLaunched { agent_id, tool_use_id, .. }
                if agent_id == "a19856ee9986244ad" && tool_use_id == "toolu_legacy"
        )),
        "the old spelling must not regress: {evs:?}"
    );

    let camel_compact = json!({
        "type": "system",
        "subtype": "compact_boundary",
        "compactMetadata": {"trigger": "manual", "preTokens": 251239}
    });
    assert!(matches!(
        &parse_line(&camel_compact)[0],
        ThreadEvent::Compacted { trigger, pre_tokens: Some(251239), .. } if trigger == "manual"
    ));
}

// ── change 2: the task lifecycle ────────────────────────────────────────

/// The headline: nothing in the fixture falls through to `Unknown` any more.
///
/// Before this change the five `system/task_*` subtypes were the largest
/// population of `unknown` rows in the ledger — 84,358 `task_progress`, 7,069
/// `task_started`, 7,049 `task_notification`.
#[test]
fn no_captured_line_falls_through_to_unknown() {
    let unknown: Vec<_> = fixture_events()
        .into_iter()
        .filter(|e| matches!(e, ThreadEvent::Unknown { .. }))
        .collect();
    assert!(
        unknown.is_empty(),
        "{} captured line(s) still land in Unknown: {unknown:#?}",
        unknown.len()
    );
}

/// `local_agent` and `local_bash` are the same subtype on the wire and must
/// stay distinguishable: background Bash is observable for the first time.
#[test]
fn task_started_separates_agents_from_background_bash() {
    let mut agents = 0;
    let mut bash = 0;
    for e in fixture_events() {
        let ThreadEvent::TaskStarted {
            task_type,
            subagent_type,
            prompt,
            tool_use_id,
            description,
            task_id,
            ..
        } = &e
        else {
            continue;
        };
        assert!(!task_id.is_empty(), "task_id is the join key; never empty");
        assert!(tool_use_id.is_some(), "captured starts all carry the tool link");
        assert!(description.is_some());
        match task_type.as_deref() {
            Some("local_agent") => {
                agents += 1;
                assert!(subagent_type.is_some(), "agents name their type");
                assert!(prompt.is_some(), "agents carry their launch instruction");
            }
            Some("local_bash") => {
                bash += 1;
                // 5,981 of 7,501 captured starts look like this. Treating the
                // absence as a parse failure would have thrown away every
                // background Bash observation.
                assert!(subagent_type.is_none());
                assert!(prompt.is_none());
            }
            other => panic!("unexpected task_type {other:?}"),
        }
    }
    assert!(agents >= 3 && bash >= 1, "fixture covers both populations");
}

/// The terminal signal, with and without its optional parts.
#[test]
fn task_notification_carries_status_result_and_spend() {
    let notifications: Vec<_> = fixture_events()
        .into_iter()
        .filter(|e| matches!(e, ThreadEvent::TaskNotification { .. }))
        .collect();
    assert!(notifications.len() >= 5);

    let mut with_usage = 0;
    let mut without_usage = 0;
    let mut without_tool_link = 0;
    for e in &notifications {
        let ThreadEvent::TaskNotification {
            task_id,
            tool_use_id,
            status,
            output_file,
            summary,
            total_tokens,
            tool_uses,
            duration_ms,
            ..
        } = e
        else {
            unreachable!()
        };
        assert!(!task_id.is_empty());
        assert!(
            matches!(
                status.as_deref(),
                Some("completed") | Some("failed") | Some("stopped")
            ),
            "unexpected status {status:?}"
        );
        assert!(output_file.is_some());
        assert!(summary.is_some(), "the summary IS the result the parent acts on");
        if total_tokens.is_some() {
            with_usage += 1;
            assert!(tool_uses.is_some() && duration_ms.is_some());
        } else {
            without_usage += 1;
            assert!(tool_uses.is_none() && duration_ms.is_none());
        }
        if tool_use_id.is_none() {
            without_tool_link += 1;
        }
    }
    // Both populations are real: 1,370 of 7,494 captured notifications carry a
    // usage block and 6,124 do not. A parser that required it would drop 82% of
    // all completions.
    assert!(with_usage >= 1 && without_usage >= 1);
    // And 5 of 7,494 carry no tool link at all — rare, but a completion is a
    // completion.
    assert!(without_tool_link >= 1);
}

/// `task_updated` patches are genuinely partial, in three captured shapes.
#[test]
fn task_updated_tolerates_every_captured_patch_shape() {
    let updates: Vec<_> = fixture_events()
        .into_iter()
        .filter(|e| matches!(e, ThreadEvent::TaskUpdated { .. }))
        .collect();
    assert_eq!(updates.len(), 3, "one of each captured shape");

    let mut saw_plain = false;
    let mut saw_error = false;
    let mut saw_backgrounded = false;
    for e in &updates {
        let ThreadEvent::TaskUpdated {
            task_id,
            status,
            end_time,
            error,
            is_backgrounded,
            patch,
            ..
        } = e
        else {
            unreachable!()
        };
        assert!(!task_id.is_empty());
        // The raw patch always rides along — the tolerance hatch for a shape
        // the capture has not seen.
        assert!(patch.is_object(), "patch kept verbatim");
        if is_backgrounded.is_some() {
            saw_backgrounded = true;
            // 183 of 2,407 captured patches are exactly this: no status at all.
            // Defaulting a missing status to "completed" here would have
            // reported 183 live tasks as finished.
            assert!(status.is_none() && end_time.is_none());
        } else if error.is_some() {
            saw_error = true;
            assert_eq!(status.as_deref(), Some("failed"));
            assert!(end_time.is_some());
        } else {
            saw_plain = true;
            assert!(status.is_some() && end_time.is_some());
        }
    }
    assert!(saw_plain && saw_error && saw_backgrounded);
}

/// `task_progress` — the highest-volume line the CLI emits, and the only
/// per-subagent heartbeat that is not a file mtime.
#[test]
fn task_progress_is_a_real_heartbeat() {
    let e = fixture_events()
        .into_iter()
        .find(|e| matches!(e, ThreadEvent::TaskProgress { .. }))
        .expect("fixture has a progress line");
    let ThreadEvent::TaskProgress {
        task_id,
        tool_use_id,
        description,
        subagent_type,
        last_tool_name,
        total_tokens,
        tool_uses,
        duration_ms,
        ..
    } = e
    else {
        unreachable!()
    };
    assert!(!task_id.is_empty());
    assert!(tool_use_id.is_some());
    assert!(description.is_some(), "the current activity line");
    assert!(subagent_type.is_some());
    assert!(last_tool_name.is_some(), "what the agent is running right now");
    assert!(total_tokens.is_some() && tool_uses.is_some() && duration_ms.is_some());
}

/// An empty roster is a reading, not a failure — 402 of 4,046 captured
/// `background_tasks_changed` lines carry `tasks: []`, which is the CLI saying
/// "nothing is running". A line with no `tasks` key at all is a different fact
/// and stays `Unknown`.
#[test]
fn background_tasks_changed_distinguishes_empty_from_absent() {
    let rosters: Vec<_> = fixture_events()
        .into_iter()
        .filter_map(|e| match e {
            ThreadEvent::BackgroundTasksChanged { tasks, .. } => Some(tasks),
            _ => None,
        })
        .collect();
    assert_eq!(rosters.len(), 2);
    let (empty, full): (Vec<_>, Vec<_>) = rosters.into_iter().partition(|t| t.is_empty());
    assert_eq!(empty.len(), 1, "the everything-finished broadcast");
    assert_eq!(full.len(), 1);
    for t in &full[0] {
        assert!(t.task_id.is_some());
        assert!(t.task_type.is_some());
        assert!(t.description.is_some());
    }

    let absent = json!({"type": "system", "subtype": "background_tasks_changed"});
    assert!(matches!(&parse_line(&absent)[0], ThreadEvent::Unknown { .. }));
}

// ── attribution ─────────────────────────────────────────────────────────

/// **The join, over a real three-level chain, using only what `stream.rs`
/// emits.**
///
/// `stream.rs` is stateless by contract, so it does not perform this — it emits
/// the keys and a stateful consumer does the walk. This test IS that consumer,
/// and its point is to prove the keys are sufficient. The walk:
///
/// `task_notification.tool_use_id` → the assistant record that emitted that
/// `tool_use.id` (`ToolUse` / `AgentSpawn` carry the pair) → that record's
/// `parent_tool_use_id` → look it up in the `TaskStarted` index by
/// `tool_use_id` → the parent's `task_id`. Recurse.
///
/// Note that NO `system/task_*` line carries `parent_tool_use_id` — verified,
/// zero occurrences across the capture. The link has to come from the assistant
/// side, which is exactly why `AgentSpawn` now carries it too.
#[test]
fn three_level_agent_chain_resolves_by_join() {
    let events = fixture_events();

    // Index 1: tool_use id → its parent's tool_use id. Built from AgentSpawn,
    // which now carries both halves.
    let mut parent_of_tool_use: HashMap<String, Option<String>> = HashMap::new();
    // Index 2: tool_use id → the task it started.
    let mut task_of_tool_use: HashMap<String, String> = HashMap::new();

    for e in &events {
        match e {
            ThreadEvent::AgentSpawn {
                tool_use_id,
                parent_tool_use_id,
                ..
            } => {
                parent_of_tool_use.insert(tool_use_id.clone(), parent_tool_use_id.clone());
            }
            ThreadEvent::TaskStarted {
                task_id,
                tool_use_id: Some(tu),
                ..
            } => {
                task_of_tool_use.insert(tu.clone(), task_id.clone());
            }
            _ => {}
        }
    }
    assert!(
        parent_of_tool_use.len() >= 3,
        "AgentSpawn must fire for every Agent tool call: {parent_of_tool_use:?}"
    );

    // Walk up from the DEEPEST notification in the captured chain: the
    // `Explore` agent `a19856ee9986244ad`, launched by tool_use
    // `toolu_01VtdhnvmWEvVdAJBMwZmUxo`.
    let deepest = events
        .iter()
        .find_map(|e| match e {
            ThreadEvent::TaskNotification {
                task_id,
                tool_use_id: Some(tu),
                ..
            } if task_id == "a19856ee9986244ad" => Some(tu.clone()),
            _ => None,
        })
        .expect("the Explore agent's notification");
    assert_eq!(deepest, "toolu_01VtdhnvmWEvVdAJBMwZmUxo");

    let mut ancestry = vec![task_of_tool_use[&deepest].clone()];
    let mut cursor = deepest;
    while let Some(Some(parent_tu)) = parent_of_tool_use.get(&cursor).cloned() {
        let parent_task = task_of_tool_use
            .get(&parent_tu)
            .unwrap_or_else(|| panic!("broken link at {parent_tu}"))
            .clone();
        ancestry.push(parent_task);
        cursor = parent_tu;
    }

    // Explore ← general-purpose ← general-purpose ← (top level).
    assert_eq!(
        ancestry,
        vec![
            "a19856ee9986244ad".to_string(),
            "a0e7b437ba74224b2".to_string(),
            "ad06fc554bdf962d8".to_string(),
        ],
        "the real captured chain, resolved by recursion"
    );
    // The walk terminated because the outermost agent's launching record has a
    // null `parent_tool_use_id` — a top-level call, not a broken link.
    assert!(parent_of_tool_use["toolu_01BGHDSeUsVGdsJMkvC26ErE"].is_none());
}

/// `AgentLaunched.agent_id` and `TaskStarted.task_id` are the SAME identifier.
///
/// This equality is what lets liveness accounting close on a real completion
/// (`task_notification`) instead of on a file mtime: `interpret` keys
/// `Liveness.outstanding` on `agent_id`, and the notification arrives carrying
/// `task_id`. All 836 captured `agentId`s appear as a `task_started.task_id`.
#[test]
fn agent_id_and_task_id_are_the_same_namespace() {
    let v = line_where(|v| {
        v.get("tool_use_result")
            .and_then(|t| t.get("status"))
            .and_then(Value::as_str)
            == Some("async_launched")
    });
    let ThreadEvent::AgentLaunched { agent_id, .. } = parse_line(&v)
        .into_iter()
        .find(|e| matches!(e, ThreadEvent::AgentLaunched { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    // It looks like a task id, not a uuid: the CLI's short task-id shape.
    assert!(!agent_id.contains('-') && agent_id.len() >= 9, "{agent_id}");

    // And within the fixture, the update for that very agent is keyed by it.
    let updated = fixture_events().into_iter().any(|e| {
        matches!(e, ThreadEvent::TaskUpdated { task_id, .. } if task_id == agent_id)
    });
    assert!(updated, "the launched agent's task_updated is keyed by the same id");
}
