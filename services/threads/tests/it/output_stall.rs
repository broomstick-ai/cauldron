//! # R9.L6 (GAP-AUDIT L-6) — a `claude` child that HANGS is noticed by its silence
//!
//! The tail loop always knew how far the child had written (`offset`) and
//! never knew WHEN it last wrote. A child that wrote its init line and then
//! hung without exiting therefore read as an active turn forever: the
//! heartbeat kept broadcasting liveness, nothing ever tore it down, and every
//! message queued behind it waited for a manual cancel.
//!
//! The fix stamps the last-output instant in the tail loop and, after
//! `THREADS_OUTPUT_STALL_MS` of silence (default 20 minutes), tears the child
//! down through the SAME teardown the other four kill flags use and hands the
//! turn to the existing retry ladder — a `turn_killed` naming the silence, then
//! a `retry_scheduled`, never a silent drop.
//!
//! Every test here shrinks the silence bound to **1500 ms** (injectable, the
//! longest period on this path besides the 20 ms tail poll and the 150 ms
//! resume tick) and names that period in its own name (doctrine 4). The three
//! tests oppose one another (doctrine 2):
//!   1. writes once, then hangs            → torn down and retried;
//!   2. keeps writing, every gap < 1500 ms,
//!      for ~5× the bound in total          → NOT torn down;
//!   3. parent goes silent for 5× the bound
//!      while a native async agent it
//!      launched is still outstanding        → NOT torn down (the documented
//!                                             quiet-but-alive case).
//! A constant "never tear down" fails (1); a constant "tear down on any
//! silence" or "tear down after a fixed wall-clock" fails (2) and (3).
//!
//! The child is a purpose-written stub script — never a real `claude` turn —
//! and the manager runs on a tempdir data dir with `THREADS_CLAUDE_HOME` on a
//! scratch dir, so nothing here touches the operator's daemon or `~/.claude`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::json;
use threads::budgets::{BudgetConfig, TripAction};
use threads::event::{Push, StoredEvent, ThreadEvent};
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

/// The shrunk silence bound every test runs under.
const STALL_MS: u64 = 1500;

/// The async-agent hold every test runs under (R9.L6b): 8x the stall bound,
/// set explicitly so the 8 s quiet-but-alive parent of test 3 sits a third
/// under it and the hung agent of the hold test is torn down well inside the
/// 30 s polling window.
const HOLD_MS: u64 = 12_000;

const STUB: &str = r#"#!/usr/bin/env bash
set -u
# The zero-cost `/usage` probe (R9.L6b's adopted limit-kill test): argv, no
# stdin, answered before `cat`. A `usage-weekly` file in the account's own
# config dir is the weekly percentage, as in `tests/fixtures/fake-claude`.
for arg in "$@"; do
  if [[ "$arg" == "/usage" ]]; then
    reset="$(date -v+2H '+%l:%M%p' 2>/dev/null || date -d '+2 hours' '+%l:%M%p')"
    reset="${reset# }"
    weekly=34
    if [[ -n "${CLAUDE_CONFIG_DIR:-}" && -f "${CLAUDE_CONFIG_DIR}/usage-weekly" ]]; then
      weekly="$(cat "${CLAUDE_CONFIG_DIR}/usage-weekly")"
    fi
    cat <<USAGE
{"type":"result","subtype":"success","is_error":false,"num_turns":0,"total_cost_usd":0,"result":"Current session: 12% used \u00b7 resets ${reset} (America/Chicago)\nCurrent week (all models): ${weekly}% used \u00b7 resets ${reset} (America/Chicago)\nCurrent week (Fable): 56% used \u00b7 resets ${reset} (America/Chicago)","session_id":"fake-usage-sess"}
USAGE
    exit 0
  fi
done
PROMPT="$(cat)"
STATE_DIR="${FAKE_STATE_DIR:-/tmp}"
emit_init() {
  echo '{"type":"system","subtype":"init","session_id":"fake-sess","cwd":"/tmp","model":"claude-fable-5","tools":["Bash","Read"],"mcp_servers":[],"permissionMode":"bypassPermissions","slash_commands":["/usage"],"apiKeySource":"none","agents":[]}'
}
emit_text() {
  echo '{"type":"assistant","message":{"id":"msg_fake_01","model":"claude-fable-5","content":[{"type":"text","text":"Hello from the stall stub."}],"usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}'
}
emit_result() {
  echo '{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.01,"num_turns":1,"duration_ms":50,"stop_reason":"end_turn","modelUsage":{"claude-fable-5":{"inputTokens":105,"outputTokens":25,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"costUSD":0.01}},"session_id":"fake-sess"}'
}
if [[ "$PROMPT" == *"stallonce"* ]]; then
  # The FIRST spawn is told `--session-id`; the retry of a thread that has
  # spawned once is told `--resume`. Keyed on argv rather than on a marker
  # file: the spawned child cannot be relied on to write outside its stdout
  # here (measured — a marker under the test's tempdir never appeared).
  if [[ " $* " != *" --resume "* ]]; then
    # Writes ONCE, then hangs without exiting — the L-6 specimen.
    emit_init
    exec sleep 120
  fi
  emit_init; emit_text; emit_result; exit 0
elif [[ "$PROMPT" == *"resultlinger"* ]]; then
  # R9.L6c (inspect F1): the verdict ARRIVES — a clean `result success` — and
  # then the child lingers without exiting (a shutdown hook or an MCP server
  # that will not die). The turn is finished and paid for; only the process
  # is left. Every spawn does this, so a retry would hang again — counted.
  # It answers SIGTERM with exit 143, as a CLI with a signal handler does, so
  # the non-zero-exit door is exercised as well as the stall verdict.
  emit_init; emit_text; emit_result
  trap 'exit 143' TERM
  sleep 120 &
  wait
  exit 0
elif [[ "$PROMPT" == *"resultthenmore"* ]]; then
  # Paired with `resultlinger`: a result, then the parent STARTS AGAIN (a new
  # assistant line) and hangs mid-burst. Ruling E17-8: the verdict already
  # arrived, so this is NOT retried either — the cut-off output is recorded.
  # The `--resume` branch exists so that a retry, if one ever came, would land
  # clean and be counted rather than hang again.
  if [[ " $* " != *" --resume "* ]]; then
    emit_init; emit_text; emit_result; emit_text
    exec sleep 120
  fi
  emit_init; emit_text; emit_result; exit 0
elif [[ "$PROMPT" == *"slowwriter"* ]]; then
  # Alive and talking: a line every 150 ms for ~8.3 s (over 5x the bound),
  # every single gap a tenth of it.
  emit_init
  for i in $(seq 1 55); do
    sleep 0.15
    emit_text
  done
  emit_result
  exit 0
elif [[ "$PROMPT" == *"asynchung"* ]]; then
  # R9.L6b: the parent launches a native async agent whose
  # `task_notification` NEVER arrives, then hangs. Without a bound on the
  # hold it is held forever. The `--resume` branch lands a retry clean.
  if [[ " $* " != *" --resume "* ]]; then
    emit_init
    echo '{"type":"assistant","message":{"id":"msg_fake_03","model":"claude-fable-5","content":[{"type":"tool_use","id":"toolu_agent9","name":"Agent","input":{"description":"never reports","subagent_type":"general-purpose","run_in_background":true,"prompt":"go"}}],"usage":{"input_tokens":5,"output_tokens":5,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}'
    echo '{"type":"user","tool_use_result":{"isAsync":true,"status":"async_launched","agentId":"agent-hung-9","outputFile":"/dev/null","canReadOutputFile":true},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_agent9","content":"launched"}]}}'
    exec sleep 120
  fi
  emit_init; emit_text; emit_result; exit 0
elif [[ "$PROMPT" == *"asyncquiet"* ]]; then
  OUT="$STATE_DIR/threads-stall-agent-$(printf '%s' "$PROMPT" | cksum | cut -d' ' -f1).jsonl"
  echo '{"type":"assistant","message":{"model":"claude-fable-5","content":[{"type":"text","text":"working"}]}}' > "$OUT"
  emit_init
  emit_text
  echo '{"type":"assistant","message":{"id":"msg_fake_02","model":"claude-fable-5","content":[{"type":"tool_use","id":"toolu_agent1","name":"Agent","input":{"description":"do work","subagent_type":"general-purpose","run_in_background":true,"prompt":"go"}}],"usage":{"input_tokens":5,"output_tokens":5,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}'
  printf '{"type":"user","tool_use_result":{"isAsync":true,"status":"async_launched","agentId":"agent-abc","outputFile":"%s","canReadOutputFile":true},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_agent1","content":"launched"}]}}\n' "$OUT"
  emit_result
  # The parent has said everything it will say; the native async agent it
  # launched keeps the process alive, writing nothing to the parent's stream,
  # for 8 s (over 5x the bound). This is the quiet-but-alive case.
  sleep 8
  exit 0
elif [[ "$PROMPT" == *"cancelrace"* ]]; then
  # R9.L6d (ledger R9.L6b-1): a child that IGNORES SIGTERM, so a limit kill
  # and a cancel can both land inside one kill-grace window before the
  # SIGKILL ends it. The `--resume` branch lands a re-run clean, so a re-run
  # (the defect) is counted, not hung.
  if [[ " $* " != *" --resume "* ]]; then
    emit_init; emit_text
    trap '' TERM
    # Keeps talking (a gap far under the 1500 ms bound), so no stall can be
    # what ends it.
    while :; do sleep 0.2; emit_text; done
  fi
  emit_init; emit_text; emit_result; exit 0
elif [[ "$PROMPT" == *"budgetrace"* ]]; then
  # R9.L6e (R9.L6d inspect F1): the budget twin of `cancelrace`. IGNORES
  # SIGTERM, and streams one message priced far past a half-cent cash budget
  # (5M fable input tokens), so a budget kill fires mid-turn and the operator's
  # cancel can land inside the same kill-grace window before the SIGKILL.
  if [[ " $* " != *" --resume "* ]]; then
    emit_init
    trap '' TERM
    echo '{"type":"assistant","message":{"id":"msg_fake_burn","model":"claude-fable-5","content":[{"type":"text","text":"burning"}],"usage":{"input_tokens":5000000,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}'
    while :; do sleep 0.2; emit_text; done
  fi
  emit_init; emit_text; emit_result; exit 0
elif [[ "$PROMPT" == *"carried across a restart"* ]]; then
  # The adopted tests' words (`ADOPTED_WORDS`), when a RETRY of an adopted turn
  # spawns: says whether the words arrived verbatim, so a test can read that
  # off the stream instead of off a retry row that may already have fired.
  emit_init
  if [[ "$PROMPT" == "the operator's words, carried across a restart" ]]; then
    echo '{"type":"assistant","message":{"id":"msg_fake_06","model":"claude-fable-5","content":[{"type":"text","text":"prompt-received: verbatim"}],"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}'
  else
    echo '{"type":"assistant","message":{"id":"msg_fake_06","model":"claude-fable-5","content":[{"type":"text","text":"prompt-received: altered"}],"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}'
  fi
  emit_result; exit 0
else
  emit_init; emit_text; emit_result; exit 0
fi
"#;

fn scratch() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().unwrap()).path()
}

fn stub() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let p = scratch().join("stall-claude");
        std::fs::write(&p, STUB).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        // WARM IT ONCE, synchronously, before any test spawns it. The first
        // exec of a freshly written script on macOS is held for seconds while
        // the system scans it (measured: two first spawns each silent past a
        // 1.5 s bound, the third clean) — which the stall check correctly
        // reads as silence. The tests measure the daemon, not that scan.
        let out = std::process::Command::new(&p)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "the stub must run: {out:?}");
        p
    })
    .clone()
}

fn set_env() {
    // Identical values in every test, so the process-global env cannot race.
    std::env::set_var("THREADS_CLAUDE_BIN", stub());
    std::env::set_var("THREADS_OUTPUT_STALL_MS", STALL_MS.to_string());
    std::env::set_var("THREADS_ASYNC_HOLD_MS", HOLD_MS.to_string());
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "200");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("THREADS_KILL_GRACE_MS", "300");
    std::env::set_var("FAKE_STATE_DIR", scratch());
    std::env::set_var("THREADS_CLAUDE_HOME", scratch().join("claude-home"));
}

fn config_at(dir: &Path) -> Config {
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        dir.join("aa-owned").to_string_lossy().to_string(),
    );
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

fn watch(m: &ThreadManager) -> Arc<Mutex<Vec<StoredEvent>>> {
    let mut rx = m.subscribe();
    let seen = Arc::new(Mutex::new(Vec::<StoredEvent>::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Event(ev) = p {
                out.lock().unwrap().push(ev);
            }
        }
    });
    seen
}

fn kinds(seen: &Arc<Mutex<Vec<StoredEvent>>>) -> Vec<&'static str> {
    seen.lock().unwrap().iter().map(|e| e.event.kind()).collect()
}

fn count(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|e| e.event.kind() == kind)
        .count()
}

/// Turns that ended with a clean exit 0.
fn clean_ends(seen: &Arc<Mutex<Vec<StoredEvent>>>) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e.event, ThreadEvent::TurnEnded { exit_code: Some(0) }))
        .count()
}

/// Polls for up to 30 s — 20x the bound, and far short of the stub's 120 s
/// hang, so a base without the fix cannot pass by the child exiting on its own.
async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..1200 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

fn unique(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{tag} {}-{nanos}", std::process::id())
}

/// **The specimen.** The child writes its init line and hangs. After the
/// 1500 ms silence bound it must be torn down — a `turn_killed` that names the
/// silence — and the turn handed to the retry ladder (`retry_scheduled`, kind
/// `transient`), whose next attempt runs clean. At base nothing fires for the
/// whole 30 s window: the turn just reads as active.
#[tokio::test]
async fn a_child_that_writes_once_then_hangs_is_torn_down_after_the_1500ms_silence_bound_and_retried(
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    // Boot as the daemon does: the retry ladder's pool cadence and placement
    // wakes are armed here, and without them a scheduled retry never fires.
    m.recover().await;
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    m.send(
        &id,
        unique("stallonce — the operator's real words"),
        Default::default(),
        json!({}),
        false,
    )
    .unwrap();

    assert!(
        eventually(|| count(&seen, "retry_scheduled") >= 1).await,
        "a hung child must be torn down and retried, not left active forever: {:?}",
        kinds(&seen)
    );
    let (killed, retry_kind) = {
        let ev = seen.lock().unwrap();
        let killed: Vec<String> = ev
            .iter()
            .filter_map(|e| match &e.event {
                ThreadEvent::TurnKilled { reason, budget_key, .. } => {
                    assert!(budget_key.is_none(), "a silence is not a budget trip");
                    Some(reason.clone())
                }
                _ => None,
            })
            .collect();
        let kind = ev.iter().find_map(|e| match &e.event {
            ThreadEvent::RetryScheduled { error_kind, .. } => Some(error_kind.clone()),
            _ => None,
        });
        (killed, kind)
    };
    assert_eq!(killed.len(), 1, "exactly one teardown: {killed:?}");
    assert!(
        killed[0].contains("no output"),
        "the ledger must say WHY it was torn down: {:?}",
        killed[0]
    );
    assert_eq!(retry_kind.as_deref(), Some("transient"));

    // The ladder's next rung runs the same words and they land.
    assert!(
        eventually(|| clean_ends(&seen) >= 1).await,
        "the retried turn must complete: {:?}",
        seen.lock().unwrap().iter().map(|e| format!("{:?}", e.event)).collect::<Vec<_>>()
    );
}

/// **Paired (doctrine 2): alive and talking is never a stall.** Every gap is
/// 150 ms, under the 1500 ms bound; the turn as a whole runs ~8.3 s, over five
/// times the bound, so a check on total wall-clock (rather than on SILENCE)
/// would tear it down.
#[tokio::test]
async fn a_child_writing_every_150ms_outlasting_the_1500ms_silence_bound_five_times_is_not_torn_down(
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    // Boot as the daemon does: the retry ladder's pool cadence and placement
    // wakes are armed here, and without them a scheduled retry never fires.
    m.recover().await;
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    m.send(&id, unique("slowwriter"), Default::default(), json!({}), false)
        .unwrap();

    assert!(
        eventually(|| clean_ends(&seen) >= 1).await,
        "the slow writer must finish on its own: {:?}",
        kinds(&seen)
    );
    assert_eq!(count(&seen, "turn_killed"), 0, "{:?}", kinds(&seen));
    assert_eq!(count(&seen, "retry_scheduled"), 0, "{:?}", kinds(&seen));
}

/// **The quiet-but-alive case the bound deliberately does not cover.** A native
/// async agent runs INSIDE the `claude` process and writes nothing to the
/// parent's stream; while one is outstanding the silence clock is held, so a
/// parent quiet for 8 s (over five times the 1500 ms bound) is not torn down.
#[tokio::test]
async fn a_parent_silent_for_five_times_the_1500ms_bound_with_an_async_agent_outstanding_is_not_torn_down(
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    // Boot as the daemon does: the retry ladder's pool cadence and placement
    // wakes are armed here, and without them a scheduled retry never fires.
    m.recover().await;
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    m.send(&id, unique("asyncquiet"), Default::default(), json!({}), false)
        .unwrap();

    assert!(
        eventually(|| clean_ends(&seen) >= 1).await,
        "the parent must run out its async agent and exit on its own: {:?}",
        kinds(&seen)
    );
    assert_eq!(count(&seen, "turn_killed"), 0, "{:?}", kinds(&seen));
    assert_eq!(count(&seen, "retry_scheduled"), 0, "{:?}", kinds(&seen));
}

/// **R9.L6c (inspect F1): a verdict that has ARRIVED is never re-run.** The
/// child streams a clean `result success` and then lingers without exiting.
/// The stall check still reaps it — one `turn_killed` — but the turn lands
/// with the verdict it already sent: no `retry_scheduled`, no second
/// `turn_started`, no retry row, no open delivery clock, and the thread idle.
///
/// After the reap the test waits a further 4500 ms — three times the 1500 ms
/// bound, thirty times the 150 ms resume tick, and past the transient
/// ladder's first two rungs (0 s, 0 s) — so a retry that was going to be
/// scheduled has had every period on its path to appear. At 82e022f9 the
/// events read `result_turn, turn_killed, turn_ended, retry_scheduled,
/// turn_started` (the inspector's repro).
#[tokio::test]
async fn a_child_that_sent_its_result_then_lingers_past_the_1500ms_silence_bound_is_reaped_and_not_retried_for_4500ms_after(
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    // Boot as the daemon does: without the pool cadence armed a scheduled
    // retry never fires, and this test must be able to SEE one if it comes.
    m.recover().await;
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    m.send(&id, unique("resultlinger"), Default::default(), json!({}), false)
        .unwrap();

    assert!(
        eventually(|| count(&seen, "turn_ended") >= 1).await,
        "the lingering child must still be reaped past the bound: {:?}",
        kinds(&seen)
    );
    // Outlast the bound three times over after the reap (see the doc above).
    tokio::time::sleep(Duration::from_millis(3 * STALL_MS)).await;

    let k = kinds(&seen);
    assert_eq!(count(&seen, "result_turn"), 1, "the stub's verdict arrived: {k:?}");
    assert_eq!(count(&seen, "turn_killed"), 1, "the linger is reaped once: {k:?}");
    assert_eq!(
        count(&seen, "retry_scheduled"),
        0,
        "a turn whose result already arrived must not be re-run: {k:?}"
    );
    assert_eq!(count(&seen, "turn_started"), 1, "no second execution: {k:?}");
    // The stub answered our SIGTERM with 143, so the non-zero-exit door was
    // reached too — and it must not have laddered the turn either.
    let ended: Vec<Option<i32>> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            ThreadEvent::TurnEnded { exit_code } => Some(*exit_code),
            _ => None,
        })
        .collect();
    assert_eq!(ended, vec![Some(143)], "the reaped child exited 143: {k:?}");
    let reason = seen
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match &e.event {
            ThreadEvent::TurnKilled { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        !reason.contains("retry"),
        "the ledger must not say a delivered turn went to the ladder: {reason:?}"
    );
    // Since R10.M1 every dispatch writes one `custom` of its own
    // (`cauldron.threads/delivery_attempted`), so the bare kind is not the
    // cut-off record; name the one this test is about.
    let cut_off = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(&e.event,
            ThreadEvent::Custom { event_type, .. } if event_type == "post_result_output_truncated"))
        .count();
    assert_eq!(cut_off, 0, "nothing followed the result, so nothing was cut off: {k:?}");
    assert!(m.store.get_retry(&id).unwrap().is_none(), "no retry row");
    assert!(
        m.store.delivery_clock(&id).unwrap().is_none(),
        "the delivery ended as a success"
    );
    let st = m.thread_status(&id).unwrap();
    assert_eq!(st["state"], "idle", "the thread lands idle: {st}");
}

/// **R13.5 (overrides E17-8): a TOP-LEVEL write AFTER a clean result re-opens
/// the turn, so a hang there is laddered.** The child sends a result, writes
/// a further top-level assistant line (the parent woken again), then hangs.
/// The stall reaps it once; its LAST word was not a clean result, so the turn
/// goes on the ladder (`retry_scheduled`, a second `turn_started`) and the
/// stub's `--resume` branch lands the retry clean. Nothing is recorded as
/// `post_result_output_truncated` — the output was not cut off unretried.
/// Opposes G1 (`resultlinger`: the result IS the last word → reaped, no retry)
/// and `tests/last_word_stall.rs`.
#[tokio::test]
async fn a_child_that_writes_again_after_its_result_then_hangs_past_the_1500ms_silence_bound_is_laddered_and_its_retry_lands_clean(
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    m.recover().await;
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    m.send(&id, unique("resultthenmore"), Default::default(), json!({}), false)
        .unwrap();

    assert!(
        eventually(|| count(&seen, "turn_killed") >= 1).await,
        "the hung child must be reaped past the bound: {:?}",
        kinds(&seen)
    );
    assert!(
        eventually(|| count(&seen, "retry_scheduled") >= 1).await,
        "a turn re-opened after its result goes on the ladder (R13.5): {:?}",
        kinds(&seen)
    );
    assert!(
        eventually(|| clean_ends(&seen) >= 1).await,
        "the retried turn completes: {:?}",
        all_events(&seen)
    );
    let k = kinds(&seen);
    assert_eq!(count(&seen, "turn_killed"), 1, "reaped once: {k:?}");
    assert_eq!(count(&seen, "turn_started"), 2, "one retry: {k:?}");
    let truncated = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(&e.event,
            ThreadEvent::Custom { event_type, .. } if event_type == "post_result_output_truncated"))
        .count();
    assert_eq!(truncated, 0, "a laddered turn is not 'cut off, not retried': {k:?}");
}

// ═════════════════════════════════════════════════════════════════════════
// R9.L6b — THE SAME CHECK ON A RE-ADOPTED TURN, AND A BOUND ON THE HOLD
// ═════════════════════════════════════════════════════════════════════════
//
// A daemon restart re-adopts a live child (`adopt_turn`) from its
// `running_turns` row and tails its output file from the stored offset. These
// tests stage that exactly as `tests/adopted_turn_landing.rs` does: one
// manager writes the row naming a REAL process this test spawned (its own
// process-group leader, as `run_once` makes every child, so the production
// `killpg` teardown reaches it) and then runs `recover`, which adopts it. The
// child's "stream" is the output file, prepared or appended to by the test.
//
// Opposition (doctrine 2): A (silent adopted child → torn down, retried) vs E
// (adopted child that keeps writing → never torn down); B (adopted child that
// DELIVERED then hung → reaped, no retry) vs A; C (async agent outstanding
// past the hold → torn down) vs test 3 above and F (held within the bound → not
// torn down); D (adopted limit kill → admission moves it and it runs now) is
// opposed by `adopted_turn_landing.rs`'s one-account ladder test.

/// The words a re-adopted turn was running. No stub keyword in them, so any
/// re-run of them takes the stub's clean default branch.
const ADOPTED_WORDS: &str = "the operator's words, carried across a restart";
const ADOPTED_MSG: u64 = 7;

/// A stand-in for the previous daemon's still-running child: `bash -c
/// <script>` as its own process-group leader, reaped by a helper thread so
/// its pid really disappears (no zombie). Killed (the whole group) on drop
/// ONLY if it has not been reaped, so a recycled pid is never signalled.
struct Adoptee {
    pid: i64,
    reaped: Arc<std::sync::atomic::AtomicBool>,
}

impl Adoptee {
    fn spawn(script: &str) -> Adoptee {
        use std::os::unix::process::CommandExt;
        let mut c = std::process::Command::new("bash")
            .arg("-c")
            .arg(script)
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = c.id() as i64;
        let reaped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = reaped.clone();
        std::thread::spawn(move || {
            let _ = c.wait();
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        Adoptee { pid, reaped }
    }
    fn gone(&self) -> bool {
        self.reaped.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for Adoptee {
    fn drop(&mut self) {
        if !self.gone() {
            unsafe {
                libc_kill(-(self.pid as i32), 9);
            }
        }
    }
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

fn init_line() -> String {
    r#"{"type":"system","subtype":"init","session_id":"fake-sess","model":"claude-fable-5"}"#
        .to_string()
}

fn text_line(text: &str) -> String {
    json!({"type":"assistant","message":{"model":"claude-fable-5","content":[{"type":"text","text":text}]}})
        .to_string()
}

fn clean_result_line() -> String {
    r#"{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.01,"num_turns":1,"duration_ms":50,"stop_reason":"end_turn","modelUsage":{},"session_id":"fake-sess"}"#.to_string()
}

/// The two lines a native async agent's launch puts on the parent's stream.
fn async_launch_lines(agent_id: &str) -> Vec<String> {
    vec![
        r#"{"type":"assistant","message":{"id":"msg_fake_05","model":"claude-fable-5","content":[{"type":"tool_use","id":"toolu_agent5","name":"Agent","input":{"description":"do work","subagent_type":"general-purpose","run_in_background":true,"prompt":"go"}}]}}"#.to_string(),
        json!({"type":"user","tool_use_result":{"isAsync":true,"status":"async_launched","agentId":agent_id,"outputFile":"/dev/null","canReadOutputFile":true},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_agent5","content":"launched"}]}}).to_string(),
    ]
}

/// Accounts this daemon owns, each with its own config dir.
fn config_with(dir: &Path, accounts: &[&str]) -> Config {
    let mut map = BTreeMap::new();
    for name in accounts {
        let cd = dir.join(name);
        std::fs::create_dir_all(&cd).unwrap();
        map.insert(name.to_string(), cd.to_string_lossy().to_string());
    }
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts: map,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

/// Where the previous daemon was when it was restarted: message
/// `ADOPTED_MSG` queued and delivered, and a `running_turns` row (attempt
/// `attempt`, with its prompt) naming `child` and `output`, whose first
/// `prefix` bytes that daemon had already read.
fn record_in_flight(
    m: &ThreadManager,
    id: &str,
    child: &Adoptee,
    output: &Path,
    prefix: u64,
    attempt: u32,
) {
    let qid = m
        .store
        .enqueue(id, ADOPTED_MSG, ADOPTED_WORDS, "immediate", &serde_json::Value::Null, &[])
        .unwrap();
    m.store.mark_delivered(&[qid]).unwrap();
    // The `turn_started` that previous daemon wrote. Without it `recover`
    // reads the delivered message as a dispatch that died BEFORE its turn
    // started and re-queues it — a second execution of the same words that
    // no adopted landing caused (measured: it was the first red of test (b)).
    let seq = m.store.max_seq(id).unwrap() + 1;
    m.store
        .append_event(&StoredEvent {
            thread_id: id.to_string(),
            seq,
            message_id: None,
            ts: threads::store::now_rfc3339(),
            event: ThreadEvent::TurnStarted {
                attempt,
                message_ids: vec![ADOPTED_MSG],
            },
        })
        .unwrap();
    m.store
        .upsert_running_turn(&threads::store::RunningTurnRow {
            thread_id: id.to_string(),
            pid: child.pid,
            spawn_cookie: "cookie".into(),
            output_path: output.to_string_lossy().to_string(),
            byte_offset: prefix,
            started_at: threads::store::now_rfc3339(),
            prompt: Some(ADOPTED_WORDS.to_string()),
            message_ids: vec![ADOPTED_MSG],
            attempt,
        })
        .unwrap();
}

/// Write `lines` to `path`; return the byte offset after the first `seen`.
fn write_stream(path: &Path, lines: &[String], seen: usize) -> u64 {
    let mut body = String::new();
    let mut offset = 0u64;
    for (i, l) in lines.iter().enumerate() {
        body.push_str(l);
        body.push('\n');
        if i + 1 == seen {
            offset = body.len() as u64;
        }
    }
    std::fs::write(path, body).unwrap();
    offset
}

fn killed_reasons(seen: &Arc<Mutex<Vec<StoredEvent>>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            ThreadEvent::TurnKilled { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

fn all_events(seen: &Arc<Mutex<Vec<StoredEvent>>>) -> Vec<String> {
    seen.lock().unwrap().iter().map(|e| format!("{:?}", e.event)).collect()
}

/// **(a) A re-adopted child that is SILENT past the 1500 ms bound is torn
/// down and retried**, exactly as a fresh child is. The child (a 120 s
/// `sleep`) wrote its init line before the restart and nothing after. At the
/// base `adopt_turn` has no stall check: nothing fires in the 30 s window and
/// the thread reads as busy until the child dies on its own.
#[tokio::test(flavor = "multi_thread")]
async fn a_re_adopted_child_silent_past_the_1500ms_bound_is_torn_down_and_retried() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["aa-owned"])).unwrap();
    let seen = watch(&m);
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let child = Adoptee::spawn("exec sleep 120");
    let out = tmp.path().join("adopted.jsonl");
    let prefix = write_stream(&out, &[init_line()], 1);
    record_in_flight(&m, &id, &child, &out, prefix, 1);

    m.recover().await;
    assert!(
        eventually(|| count(&seen, "retry_scheduled") >= 1).await,
        "R9.L6b: a hung re-adopted child must be torn down and retried: {:?}",
        kinds(&seen)
    );
    let killed = killed_reasons(&seen);
    assert_eq!(killed.len(), 1, "exactly one teardown: {killed:?}");
    assert!(killed[0].contains("no output"), "says why: {:?}", killed[0]);
    assert_eq!(
        count(&seen, "turn_interrupted"),
        0,
        "sentinel: the delivery was not re-queued behind the adopted turn: {:?}",
        kinds(&seen)
    );
    // R9.L6d (L6b inspect finding 4): these facts used to be asserted only
    // `if let Some(r) = get_retry(..)` — and the transient ladder's first rung
    // is 0 s, so the row had usually fired and gone, and the assertions did
    // not run. They are read now from what cannot vanish: the ledger's
    // `retry_scheduled` and `turn_started`, and the words the retry's own
    // spawn received (the stub's `carried across a restart` branch).
    let sched = seen.lock().unwrap().iter().find_map(|e| match &e.event {
        ThreadEvent::RetryScheduled { error_kind, attempt, .. } => Some((error_kind.clone(), *attempt)),
        _ => None,
    });
    assert_eq!(
        sched,
        Some(("transient".to_string(), 2)),
        "the transient ladder, continuing the adopted attempt's ladder"
    );
    assert!(
        eventually(|| child.gone()).await,
        "the teardown reached the re-adopted child's process group"
    );
    // The ladder's next rung runs the same words and they land.
    assert!(
        eventually(|| clean_ends(&seen) >= 1).await,
        "the retried turn must complete: {:?}",
        all_events(&seen)
    );
    let started: Vec<(u32, Vec<u64>)> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            ThreadEvent::TurnStarted { attempt, message_ids } => Some((*attempt, message_ids.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(started, vec![(2, vec![ADOPTED_MSG])], "the retry re-ran the adopted delivery");
    let texts: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            ThreadEvent::AssistantText { text, .. } if text.starts_with("prompt-received") => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["prompt-received: verbatim".to_string()], "the prompt carried verbatim");
}

/// **(b) E17-8 on the adopted path: a re-adopted child that sent a clean
/// result and then hung is reaped and lands ONCE — never retried.** The
/// result arrives in the adopted tail (after the restart's offset); the
/// child then sleeps. Waits 4500 ms (3x the 1500 ms bound, past the transient
/// ladder's 0 s first rungs and 30 resume ticks) after the reap so a retry
/// that was coming has had every period on its path to appear.
#[tokio::test(flavor = "multi_thread")]
async fn a_re_adopted_child_that_sent_its_result_then_hung_past_the_1500ms_bound_lands_once_and_is_not_retried_for_4500ms_after(
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["aa-owned"])).unwrap();
    let seen = watch(&m);
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let child = Adoptee::spawn("exec sleep 120");
    let out = tmp.path().join("adopted.jsonl");
    let prefix = write_stream(
        &out,
        &[init_line(), text_line("the work"), clean_result_line()],
        1,
    );
    record_in_flight(&m, &id, &child, &out, prefix, 1);

    m.recover().await;
    assert!(
        eventually(|| count(&seen, "turn_ended") >= 1).await,
        "the lingering re-adopted child must still be reaped past the bound: {:?}",
        kinds(&seen)
    );
    tokio::time::sleep(Duration::from_millis(3 * STALL_MS)).await;

    let k = kinds(&seen);
    assert_eq!(count(&seen, "result_turn"), 1, "the verdict arrived in the tail: {k:?}");
    assert_eq!(count(&seen, "turn_killed"), 1, "reaped once: {k:?}");
    assert_eq!(
        count(&seen, "retry_scheduled"),
        0,
        "a re-adopted turn whose result arrived must not be re-run (E17-8): {k:?}"
    );
    assert_eq!(count(&seen, "turn_started"), 0, "no new execution: {k:?}");
    assert_eq!(count(&seen, "turn_interrupted"), 0, "sentinel: nothing re-queued: {k:?}");
    assert!(m.store.get_retry(&id).unwrap().is_none(), "no retry row");
    assert!(child.gone(), "the child was reaped");
    let st = m.thread_status(&id).unwrap();
    assert_eq!(st["state"], "idle", "the thread lands idle: {st}");
}

/// **(c) An async agent outstanding past the 12000 ms hold is torn down, its
/// id named.** The parent launches a native async agent whose
/// `task_notification` never comes and hangs. The hold is what kept test 3's
/// quiet parent alive; past `THREADS_ASYNC_HOLD_MS` it gives way. Red at the
/// base: the hold is unbounded, nothing fires in the 30 s window.
#[tokio::test(flavor = "multi_thread")]
async fn an_async_agent_outstanding_past_the_12000ms_hold_is_torn_down_naming_the_agent_and_retried(
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();
    m.recover().await;
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    let sent = std::time::Instant::now();
    m.send(&id, unique("asynchung"), Default::default(), json!({}), false)
        .unwrap();

    assert!(
        eventually(|| count(&seen, "turn_killed") >= 1).await,
        "R9.L6b: a hold whose agent never reports must not hold forever: {:?}",
        kinds(&seen)
    );
    let after = sent.elapsed();
    assert!(
        after >= Duration::from_millis(HOLD_MS),
        "the hold was honoured for its full {HOLD_MS} ms before the teardown, not cut at \
         the {STALL_MS} ms silence bound: torn down after {after:?}"
    );
    let killed = killed_reasons(&seen);
    assert_eq!(killed.len(), 1, "{killed:?}");
    assert!(
        killed[0].contains("agent-hung-9"),
        "the teardown names the agent still outstanding: {:?}",
        killed[0]
    );
    assert!(
        eventually(|| count(&seen, "retry_scheduled") >= 1).await,
        "and the turn goes on the ladder: {:?}",
        kinds(&seen)
    );
    assert!(
        eventually(|| clean_ends(&seen) >= 1).await,
        "the retried turn completes: {:?}",
        all_events(&seen)
    );
}

/// **(d) An adopted limit kill goes through `land_failed_verdict` — the same
/// admission `run_turn`'s 429 branch asks — so placement can move the turn
/// to an open account and run it NOW.** Two accounts, the thread placed on
/// `hot` (AUTO, not pinned); the adopted child keeps writing (so no stall can
/// fire) until the gauge reads `hot` exhausted and the limit kill tears it
/// down. The old branch landed straight on the usage-limit ladder: a
/// `retry_scheduled` on `hot`, waiting for a reset two hours out.
#[tokio::test(flavor = "multi_thread")]
async fn an_adopted_limit_kill_goes_through_admission_and_runs_now_on_the_open_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["hot", "cool"])).unwrap();
    m.budgets
        .observe("hot", threads::gauge::Axis::Session, 1.0, None, "usage_poll")
        .unwrap();
    m.budgets
        .observe("cool", threads::gauge::Axis::Session, 99.0, None, "usage_poll")
        .unwrap();
    let s = settings(tmp.path());
    assert!(s.limit_kill_mid_turn, "sentinel: the interrupt is on");
    let id = m.create_thread(None, s).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "hot", "sentinel: placed on hot");
    let seen = watch(&m);
    let out = tmp.path().join("adopted.jsonl");
    let prefix = write_stream(&out, &[init_line()], 1);
    let child = Adoptee::spawn(&format!(
        "while :; do echo '{}' >> '{}'; sleep 0.2; done",
        text_line("still working"),
        out.display()
    ));
    record_in_flight(&m, &id, &child, &out, prefix, 2);

    m.recover().await;
    assert!(eventually(|| m.is_locked(&id)).await, "sentinel: re-adopted");
    std::fs::write(tmp.path().join("hot").join("usage-weekly"), "100").unwrap();
    m.probe_accounts_once(true).await.unwrap();

    assert!(
        eventually(|| count(&seen, "turn_killed") >= 1).await,
        "the adopted turn was torn down by the limit kill: {:?}",
        kinds(&seen)
    );
    assert!(
        killed_reasons(&seen)[0].contains("usage limit"),
        "by the limit kill, not a stall: {:?}",
        killed_reasons(&seen)
    );
    assert!(
        eventually(|| clean_ends(&seen) >= 1).await,
        "admission moved it and it ran now: {:?} (account {:?}, retry {:?})",
        all_events(&seen),
        m.serving_account(&id),
        m.store.get_retry(&id).unwrap().map(|r| (r.kind, r.attempt))
    );
    assert_eq!(m.serving_account(&id).unwrap(), "cool", "placement moved it");
    assert_eq!(
        count(&seen, "retry_scheduled"),
        0,
        "it did not wait on the usage-limit ladder: {:?}",
        kinds(&seen)
    );
    let attempts: Vec<u32> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            ThreadEvent::TurnStarted { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .collect();
    assert_eq!(attempts, vec![3], "re-run once, at the adopted attempt + 1");
    assert!(
        count(&seen, "thread_system_interrupted") >= 1,
        "DEC-56: the thread is flagged, as on the run_once path: {:?}",
        kinds(&seen)
    );
}

/// **Paired with (a): a re-adopted child that keeps writing is never torn
/// down.** A line every 150 ms for ~8.3 s (over 5x the 1500 ms bound), then a
/// clean result and exit 0.
#[tokio::test(flavor = "multi_thread")]
async fn a_re_adopted_child_writing_every_150ms_for_five_times_the_1500ms_bound_is_never_torn_down(
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["aa-owned"])).unwrap();
    let seen = watch(&m);
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let out = tmp.path().join("adopted.jsonl");
    let prefix = write_stream(&out, &[init_line()], 1);
    let child = Adoptee::spawn(&format!(
        "for i in $(seq 1 55); do sleep 0.15; echo '{t}' >> '{o}'; done; echo '{r}' >> '{o}'",
        t = text_line("still working"),
        r = clean_result_line(),
        o = out.display()
    ));
    record_in_flight(&m, &id, &child, &out, prefix, 1);

    m.recover().await;
    assert!(
        eventually(|| count(&seen, "turn_ended") >= 1).await,
        "the writer finishes on its own: {:?}",
        kinds(&seen)
    );
    assert_eq!(count(&seen, "turn_killed"), 0, "{:?}", kinds(&seen));
    assert_eq!(count(&seen, "retry_scheduled"), 0, "{:?}", kinds(&seen));
    assert_eq!(count(&seen, "result_turn"), 1, "its result landed: {:?}", kinds(&seen));
    assert!(m.store.get_retry(&id).unwrap().is_none(), "no retry row");
}

/// **Paired with (c), on the adopted path: a parent holding an async agent it
/// launched BEFORE the restart is held, within the bound.** The launch is in
/// the prefix the previous daemon read; the parent is then silent for 8 s
/// (over 5x the 1500 ms silence bound, under the 12000 ms hold) and exits.
/// Without rebuilding `outstanding` from that prefix the adopter would not
/// know the agent exists and would tear the parent down at 1500 ms.
#[tokio::test(flavor = "multi_thread")]
async fn a_re_adopted_parent_holding_an_async_agent_launched_before_the_restart_is_not_torn_down_while_silent_8s(
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["aa-owned"])).unwrap();
    let seen = watch(&m);
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let out = tmp.path().join("adopted.jsonl");
    let mut lines = vec![init_line()];
    lines.extend(async_launch_lines("agent-before-restart"));
    lines.push(clean_result_line());
    let n = lines.len();
    let prefix = write_stream(&out, &lines, n);
    let child = Adoptee::spawn("sleep 8");
    record_in_flight(&m, &id, &child, &out, prefix, 1);

    m.recover().await;
    assert!(
        eventually(|| count(&seen, "turn_ended") >= 1).await,
        "the parent runs out its agent and exits: {:?}",
        kinds(&seen)
    );
    assert_eq!(
        count(&seen, "turn_killed"),
        0,
        "held by the agent launched before the restart: {:?}",
        killed_reasons(&seen)
    );
    assert!(child.gone(), "it exited on its own");
}

// ── R9.L6d ───────────────────────────────────────────────────────────────

fn task_line(subtype: &str, task_id: &str) -> String {
    json!({"type":"system","subtype":subtype,"task_id":task_id,"status":"completed"}).to_string()
}

/// **R9.L6d (L6b inspect I2): an agent launched AND notified before the
/// restart does not hold the adopted parent.** Both lines are in the prefix
/// the previous daemon read; the parent is then silent. `outstanding_in_prefix`
/// must clear the agent on its `task_notification`, so the parent is torn
/// down as a plain stall at the 1500 ms bound — not held to the 12000 ms
/// hold. Paired with the launched-before-the-restart hold test above, which
/// is the same prefix WITHOUT the notification and is NOT torn down.
#[tokio::test(flavor = "multi_thread")]
async fn a_re_adopted_parent_whose_agent_was_notified_inside_the_prefix_is_torn_down_at_the_1500ms_bound_not_held_12000ms(
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["aa-owned"])).unwrap();
    let seen = watch(&m);
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let out = tmp.path().join("adopted.jsonl");
    let mut lines = vec![init_line()];
    lines.extend(async_launch_lines("agent-done"));
    lines.push(task_line("task_notification", "agent-done"));
    let n = lines.len();
    let prefix = write_stream(&out, &lines, n);
    let child = Adoptee::spawn("exec sleep 120");
    record_in_flight(&m, &id, &child, &out, prefix, 1);

    let adopted = std::time::Instant::now();
    m.recover().await;
    assert!(
        eventually(|| count(&seen, "turn_killed") >= 1).await,
        "{:?}",
        kinds(&seen)
    );
    let after = adopted.elapsed();
    let k = killed_reasons(&seen);
    assert!(
        k[0].contains("no output") && !k[0].contains("agent-done"),
        "a plain stall, naming no outstanding agent: {k:?}"
    );
    assert!(
        after < Duration::from_millis(HOLD_MS),
        "torn down at the {STALL_MS} ms bound, not held to the {HOLD_MS} ms hold: {after:?}"
    );
}

/// **R9.L6d decision (H179-3), regime 1 of 2: a LIVE agent whose
/// `task_progress` heartbeat reaches the parent's stream is never torn down**
/// — a line every 1 s for 16 s, past the 12000 ms hold, then the
/// notification and a clean result.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_agent_heartbeating_on_the_parent_stream_past_the_12000ms_hold_is_not_torn_down() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["aa-owned"])).unwrap();
    let seen = watch(&m);
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let out = tmp.path().join("adopted.jsonl");
    let mut lines = vec![init_line()];
    lines.extend(async_launch_lines("agent-live"));
    let n = lines.len();
    let prefix = write_stream(&out, &lines, n);
    let child = Adoptee::spawn(&format!(
        "for i in $(seq 1 16); do sleep 1; echo '{p}' >> '{o}'; done; echo '{t}' >> '{o}'; echo '{r}' >> '{o}'",
        p = task_line("task_progress", "agent-live"),
        t = task_line("task_notification", "agent-live"),
        r = clean_result_line(),
        o = out.display()
    ));
    record_in_flight(&m, &id, &child, &out, prefix, 1);
    m.recover().await;
    assert!(eventually(|| count(&seen, "turn_ended") >= 1).await, "{:?}", kinds(&seen));
    assert_eq!(count(&seen, "turn_killed"), 0, "live agent torn down: {:?}", killed_reasons(&seen));
    assert!(child.gone(), "it exited on its own");
}

/// **R9.L6d decision (H179-3), regime 2 of 2 — WHO IS IN THE DEGRADED
/// REGIME (doctrine 10): an agent that is alive but writes ONLY to its own
/// output file, nothing on the parent's stream, IS torn down at the 12000 ms
/// hold, its id named.** Accepted by the decision recorded on
/// `ASYNC_HOLD_FACTOR`; this test is the record of who that decision kills,
/// so a change to it is a change someone has to make on purpose.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_agent_writing_only_its_own_file_is_torn_down_at_the_12000ms_hold() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["aa-owned"])).unwrap();
    let seen = watch(&m);
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let out = tmp.path().join("adopted.jsonl");
    let side = tmp.path().join("agent-side.jsonl");
    let mut lines = vec![init_line()];
    lines.extend(async_launch_lines("agent-fileonly"));
    let n = lines.len();
    let prefix = write_stream(&out, &lines, n);
    let child = Adoptee::spawn(&format!(
        "for i in $(seq 1 20); do sleep 1; echo working >> '{s}'; done",
        s = side.display()
    ));
    record_in_flight(&m, &id, &child, &out, prefix, 1);
    let adopted = std::time::Instant::now();
    m.recover().await;
    assert!(eventually(|| count(&seen, "turn_killed") >= 1).await, "{:?}", kinds(&seen));
    let after = adopted.elapsed();
    let k = killed_reasons(&seen);
    assert!(k[0].contains("agent-fileonly"), "{k:?}");
    assert!(
        after >= Duration::from_millis(HOLD_MS),
        "held for the full {HOLD_MS} ms first: {after:?}"
    );
    assert!(
        std::fs::read_to_string(&side).unwrap().lines().count() >= 5,
        "sentinel: the agent really was alive and writing its own file"
    );
}

/// **R9.L6d (ledger R9.L6b-1): on the FRESH path a cancel that races a limit
/// kill wins — the delivery ends, it is not handed back to admission.** The
/// child ignores SIGTERM, so both teardown flags land inside one 300 ms kill
/// grace: the gauge bars `hot` (limit kill), then the operator cancels. Before
/// the fix `run_once` returned `limit_killed` first, `run_turn` took the 429
/// branch, admission moved the thread to `cool` and RAN IT AGAIN. Waits
/// 4500 ms after the turn ends (past the 300 ms grace, the 0 s first rungs and
/// 30 resume ticks) for a re-run that would be coming. Opposed by
/// `limit_detection.rs`'s `a_barred_account_interrupts_the_turn_running_on_it`
/// (a limit kill alone still re-runs on the open account) and by the plain
/// cancel tests.
#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_racing_a_limit_kill_on_the_fresh_path_ends_the_delivery_and_is_not_rerun_for_4500ms() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["hot", "cool"])).unwrap();
    m.budgets
        .observe("hot", threads::gauge::Axis::Session, 1.0, None, "usage_poll")
        .unwrap();
    m.budgets
        .observe("cool", threads::gauge::Axis::Session, 99.0, None, "usage_poll")
        .unwrap();
    let s = settings(tmp.path());
    assert!(s.limit_kill_mid_turn, "sentinel: the interrupt is on");
    let id = m.create_thread(None, s).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "hot", "sentinel: placed on hot");
    let seen = watch(&m);
    m.send(&id, unique("cancelrace"), Default::default(), json!({}), false)
        .unwrap();
    assert!(
        eventually(|| m.store.list_running_turns().unwrap().iter().any(|r| r.thread_id == id)).await,
        "sentinel: a real child is running"
    );
    assert!(
        eventually(|| count(&seen, "assistant_text") >= 1).await,
        "sentinel: the child is past its trap: {:?}",
        kinds(&seen)
    );
    std::fs::write(tmp.path().join("hot").join("usage-weekly"), "100").unwrap();
    m.probe_accounts_once(true).await.unwrap();
    m.cancel(&id).unwrap();

    assert!(
        eventually(|| count(&seen, "turn_ended") >= 1).await,
        "the SIGTERM-ignoring child is SIGKILLed after the grace: {:?}",
        kinds(&seen)
    );
    assert!(
        killed_reasons(&seen).iter().any(|r| r.contains("usage limit")),
        "sentinel: the limit kill really fired in this turn: {:?}",
        killed_reasons(&seen)
    );
    tokio::time::sleep(Duration::from_millis(3 * STALL_MS)).await;
    let k = kinds(&seen);
    let operator = seen.lock().unwrap().iter().any(|e| {
        matches!(&e.event, ThreadEvent::TurnInterrupted { trigger, .. } if trigger == "operator")
    });
    assert!(operator, "the turn ended as the operator's interrupt: {k:?}");
    assert_eq!(count(&seen, "turn_started"), 1, "not re-run: {k:?}");
    assert_eq!(count(&seen, "thread_system_interrupted"), 0, "not handed to admission: {k:?}");
    assert!(m.store.get_retry(&id).unwrap().is_none(), "no retry row");
    assert!(m.store.get_parked_turn(&id).unwrap().is_none(), "no parked row");
}

/// **R9.L6e (R9.L6d inspect F1): on the FRESH path a cancel that races a
/// BUDGET kill wins — the turn lands as the operator's interrupt, the prompt
/// is not parked.** The budget twin of the limit-race test above. A half-cent
/// cash budget that KILLS; the SIGTERM-ignoring child streams a message priced
/// far past it, so the budget kill fires (`turn_killed` naming the key) and the
/// child outlives the 300 ms kill grace; the test cancels as soon as it sees
/// that `turn_killed`, inside the grace. With the budget verdict above the
/// cancel in `run_once`, the turn returns `budget_killed`, `run_turn` parks the
/// prompt (CON-4: `turn_parked`, a `parked_turns` row) and no operator
/// interrupt is recorded. Waits 4500 ms after the turn ends (past the 300 ms
/// grace and 30 resume ticks) for a park or a re-run that would be coming.
/// Opposed by `budgets_e2e.rs`'s `a_cash_budget_trips_and_kills_during_the_execution`
/// (a budget kill alone still parks) and by the plain cancel tests.
#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_racing_a_budget_kill_on_the_fresh_path_lands_cancelled_not_parked_for_4500ms() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["aa-owned"])).unwrap();
    let mut cfg = BudgetConfig::new("racekill");
    cfg.cash_usd = Some(0.005);
    cfg.on_trip = TripAction::Kill;
    cfg.accounts = vec!["aa-owned".into()];
    m.budgets.set_config(&cfg).unwrap();
    let mut s = settings(tmp.path());
    s.budget_key = Some("racekill".into());
    let id = m.create_thread(None, s).unwrap();
    let seen = watch(&m);
    m.send(&id, unique("budgetrace"), Default::default(), json!({}), false)
        .unwrap();

    let budget_killed = |seen: &Arc<Mutex<Vec<StoredEvent>>>| {
        seen.lock().unwrap().iter().any(|e| {
            matches!(&e.event, ThreadEvent::TurnKilled { budget_key: Some(k), .. } if k == "racekill")
        })
    };
    assert!(
        eventually(|| budget_killed(&seen)).await,
        "sentinel: the budget kill really fired in this turn: {:?}",
        kinds(&seen)
    );
    assert_eq!(count(&seen, "turn_ended"), 0, "sentinel: the cancel lands inside the kill grace");
    m.cancel(&id).unwrap();

    assert!(
        eventually(|| count(&seen, "turn_ended") >= 1).await,
        "the SIGTERM-ignoring child is SIGKILLed after the grace: {:?}",
        kinds(&seen)
    );
    tokio::time::sleep(Duration::from_millis(3 * STALL_MS)).await;
    let k = kinds(&seen);
    let operator = seen.lock().unwrap().iter().any(|e| {
        matches!(&e.event, ThreadEvent::TurnInterrupted { trigger, .. } if trigger == "operator")
    });
    assert!(operator, "the turn ended as the operator's interrupt: {k:?}");
    assert_eq!(count(&seen, "turn_parked"), 0, "not parked: {k:?}");
    assert!(m.store.get_parked_turn(&id).unwrap().is_none(), "no parked row");
    assert_eq!(count(&seen, "turn_started"), 1, "not re-run: {k:?}");
    assert!(m.store.get_retry(&id).unwrap().is_none(), "no retry row");
}

/// **R9.L6e: the same race on the ADOPTED path — a cancel that lands while a
/// re-adopted turn's budget kill is pending lands as the operator's
/// interrupt, not parked.** `adopt_turn` swaps the budget flag in one poll and
/// the cancel in a later one; both are recorded. The adopted child ignores
/// SIGTERM (this path has no SIGKILL escalation for a budget kill) and exits
/// on its own about 3 s after adoption, so the cancel the test sends right
/// after the budget `turn_killed` is inside the turn. Waits 4500 ms after the
/// turn ends for a park or a re-run that would be coming. Opposed by
/// `adopted_turn_landing.rs`'s `an_adopted_turn_killed_by_a_budget_parks_its_prompt`
/// (a budget kill alone still parks on this path).
#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_after_an_adopted_turns_budget_kill_lands_cancelled_not_parked_for_4500ms() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["aa-owned"])).unwrap();
    let mut cfg = BudgetConfig::new("adoptrace");
    cfg.cash_usd = Some(0.005);
    cfg.on_trip = TripAction::Kill;
    cfg.accounts = vec!["aa-owned".into()];
    m.budgets.set_config(&cfg).unwrap();
    let mut s = settings(tmp.path());
    s.budget_key = Some("adoptrace".into());
    let id = m.create_thread(None, s).unwrap();
    let seen = watch(&m);
    let out = tmp.path().join("adopted.jsonl");
    let prefix = write_stream(&out, &[init_line()], 1);
    let child = Adoptee::spawn(&format!(
        "trap '' TERM; for i in $(seq 1 15); do echo '{}' >> '{}'; sleep 0.2; done; exit 1",
        text_line("still working"),
        out.display()
    ));
    record_in_flight(&m, &id, &child, &out, prefix, 1);
    m.recover().await;
    assert!(eventually(|| m.is_locked(&id)).await, "sentinel: re-adopted");
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&out).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"id":"msg_fake_burn","model":"claude-fable-5","content":[{{"type":"text","text":"burning"}}],"usage":{{"input_tokens":5000000,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}}}"#
        )
        .unwrap();
    }
    let budget_killed = |seen: &Arc<Mutex<Vec<StoredEvent>>>| {
        seen.lock().unwrap().iter().any(|e| {
            matches!(&e.event, ThreadEvent::TurnKilled { budget_key: Some(k), .. } if k == "adoptrace")
        })
    };
    assert!(
        eventually(|| budget_killed(&seen)).await,
        "sentinel: the budget kill really fired on the adopted turn: {:?}",
        kinds(&seen)
    );
    assert_eq!(count(&seen, "turn_ended"), 0, "sentinel: the cancel lands inside the turn");
    assert!(!child.gone(), "sentinel: the SIGTERM-ignoring child is still alive");
    m.cancel(&id).unwrap();

    assert!(
        eventually(|| count(&seen, "turn_ended") >= 1).await,
        "the adopted child exits: {:?}",
        kinds(&seen)
    );
    tokio::time::sleep(Duration::from_millis(3 * STALL_MS)).await;
    let k = kinds(&seen);
    let operator = seen.lock().unwrap().iter().any(|e| {
        matches!(&e.event, ThreadEvent::TurnInterrupted { trigger, .. } if trigger == "operator")
    });
    assert!(operator, "the turn ended as the operator's interrupt: {k:?}");
    assert_eq!(count(&seen, "turn_parked"), 0, "not parked: {k:?}");
    assert!(m.store.get_parked_turn(&id).unwrap().is_none(), "no parked row");
    assert_eq!(count(&seen, "turn_started"), 0, "not re-run: {k:?}");
}

// ── R9.L4b (r37 inspect F-1): THE KILL LATCH ON THE ADOPTED PATH ──────────
//
// The same race as the test above, staged so the meter RECONCILES back under
// the limit afterwards — the only state in which the kill latch, and nothing
// else, can hold the thread. The adopted child ignores SIGTERM, keeps writing
// for ~3 s, then flushes a `result` whose authoritative `costUSD` is far below
// the estimate that tripped the budget (the shape of `fake-claude`'s
// `killcheap`) and exits. Waits are 4500 ms: 30 of the 150 ms resume ticks.

/// The adopted child's closing `result`: $0.0001 on the model the burn line
/// was priced on, so the signed reconciliation hands the estimate back.
fn cheap_result_line() -> String {
    r#"{"type":"result","subtype":"error_during_execution","is_error":true,"total_cost_usd":0.0001,"num_turns":2,"duration_ms":50,"stop_reason":null,"modelUsage":{"claude-fable-5":{"inputTokens":10,"outputTokens":1,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"costUSD":0.0001}},"session_id":"fake-sess"}"#.to_string()
}

fn count_on(seen: &Arc<Mutex<Vec<StoredEvent>>>, id: &str, kind: &str) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|e| e.thread_id == id && e.event.kind() == kind)
        .count()
}

fn consumed(m: &ThreadManager, key: &str) -> f64 {
    let snap = m.budgets.snapshot(key, &m.account_pool()).unwrap().unwrap().to_json();
    snap["cash"]["consumed_usd"].as_f64().unwrap()
}

/// Stage a re-adopted turn on `key` (a $0.005 KILL budget) whose budget kill
/// fires; if `cancel`, the operator cancels right after the `turn_killed`,
/// inside the turn. Returns once the turn has ended and the meter has
/// reconciled under the limit. The child's 3 s of writing keeps the stall
/// check quiet.
async fn adopted_budget_kill(
    m: &ThreadManager,
    tmp: &Path,
    id: &str,
    key: &str,
    seen: &Arc<Mutex<Vec<StoredEvent>>>,
    cancel: bool,
) {
    let out = tmp.join(format!("adopted-{key}.jsonl"));
    let prefix = write_stream(&out, &[init_line()], 1);
    let child = Adoptee::spawn(&format!(
        "trap '' TERM; for i in $(seq 1 15); do echo '{}' >> '{}'; sleep 0.2; done; echo '{}' >> '{}'; exit 1",
        text_line("still working"),
        out.display(),
        cheap_result_line(),
        out.display()
    ));
    record_in_flight(m, id, &child, &out, prefix, 1);
    m.recover().await;
    assert!(eventually(|| m.is_locked(id)).await, "sentinel: re-adopted");
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&out).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"id":"msg_fake_burn","model":"claude-fable-5","content":[{{"type":"text","text":"burning"}}],"usage":{{"input_tokens":5000000,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}}}"#
        )
        .unwrap();
    }
    let budget_killed = || {
        seen.lock().unwrap().iter().any(|e| {
            e.thread_id == id
                && matches!(&e.event, ThreadEvent::TurnKilled { budget_key: Some(k), .. } if k == key)
        })
    };
    assert!(
        eventually(budget_killed).await,
        "sentinel: the budget kill really fired on the adopted turn: {:?}",
        kinds(seen)
    );
    assert_eq!(count_on(seen, id, "turn_ended"), 0, "sentinel: still inside the turn");
    assert!(!child.gone(), "sentinel: the SIGTERM-ignoring child is still alive");
    if cancel {
        m.cancel(id).unwrap();
    }
    assert!(
        eventually(|| count_on(seen, id, "turn_ended") >= 1 && child.gone()).await,
        "the adopted child exits: {:?}",
        kinds(seen)
    );
    assert!(
        eventually(|| consumed(m, key) < 0.005).await,
        "sentinel: the cheap result reconciled the meter under the limit (got {}), so only \
         the latch can hold anything",
        consumed(m, key)
    );
}

/// **R9.L4b on the ADOPTED path (r37 F-1, F-2): a cancel that beats a
/// re-adopted turn's budget kill releases that thread's OWN latch — its next
/// message runs in this daemon life — and a SIBLING on the same key stays
/// held across 4500 ms (30 resume ticks).** Red with `adopt_turn`'s
/// `if cancelled { release_own_kill_latch }` removed (the thread's message is
/// held by the kill he cancelled); red for the sibling if the release cleared
/// every latch. Opposed by the next test (no cancel: the adopted kill holds).
#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_that_beats_an_adopted_turns_budget_kill_releases_its_own_latch_and_holds_the_sibling_for_4500ms(
) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["aa-owned"])).unwrap();
    let mut cfg = BudgetConfig::new("adoptlatch");
    cfg.cash_usd = Some(0.005);
    cfg.on_trip = TripAction::Kill;
    cfg.accounts = vec!["aa-owned".into()];
    m.budgets.set_config(&cfg).unwrap();
    let mut s = settings(tmp.path());
    s.budget_key = Some("adoptlatch".into());
    let id = m.create_thread(None, s.clone()).unwrap();
    let sibling = m.create_thread(None, s).unwrap();
    let seen = watch(&m);
    adopted_budget_kill(&m, tmp.path(), &id, "adoptlatch", &seen, true).await;
    let k = kinds(&seen);
    assert!(
        seen.lock().unwrap().iter().any(|e| e.thread_id == id
            && matches!(&e.event, ThreadEvent::TurnInterrupted { trigger, .. } if trigger == "operator")),
        "sentinel: the cancel won (operator interrupt): {k:?}"
    );
    assert!(m.store.get_parked_turn(&id).unwrap().is_none(), "sentinel: nothing parked");

    m.send(&sibling, "hello from the sibling".into(), Default::default(), json!({}), false)
        .unwrap();
    m.send(&id, "after the cancel that beat the adopted kill".into(), Default::default(), json!({}), false)
        .unwrap();
    assert!(
        eventually(|| count_on(&seen, &id, "turn_started") >= 1).await,
        "the cancel released the adopted thread's own latch — its next message runs: {:?}",
        kinds(&seen)
    );
    tokio::time::sleep(Duration::from_millis(3 * STALL_MS)).await;
    assert_eq!(
        count_on(&seen, &sibling, "turn_started"),
        0,
        "the sibling on the killed key stays held — the cancel released only its own thread"
    );
    let ps = m.park_state(&sibling).unwrap();
    assert!(
        ps["reason"].as_str().is_some_and(|r| r.contains("adoptlatch") && r.contains(&id)),
        "the sibling is held by THIS kill's latch: {ps}"
    );
}

/// **Opposing guard on the adopted path: a re-adopted turn's budget kill with
/// NO cancel still holds the thread's own latch** — the parked prompt is not
/// redelivered across 4500 ms (30 resume ticks) with the meter under the
/// limit; then the limit is raised and it runs, so the hold was the latch.
/// Red if the adopted path released the latch whenever a killed turn ends.
#[tokio::test(flavor = "multi_thread")]
async fn an_adopted_turns_budget_kill_with_no_cancel_holds_its_own_latch_for_4500ms() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_with(tmp.path(), &["aa-owned"])).unwrap();
    let mut cfg = BudgetConfig::new("adopthold");
    cfg.cash_usd = Some(0.005);
    cfg.on_trip = TripAction::Kill;
    cfg.accounts = vec!["aa-owned".into()];
    m.budgets.set_config(&cfg).unwrap();
    let mut s = settings(tmp.path());
    s.budget_key = Some("adopthold".into());
    let id = m.create_thread(None, s).unwrap();
    let seen = watch(&m);
    adopted_budget_kill(&m, tmp.path(), &id, "adopthold", &seen, false).await;
    assert!(m.store.get_parked_turn(&id).unwrap().is_some(), "sentinel: the kill parked the prompt");
    tokio::time::sleep(Duration::from_millis(3 * STALL_MS)).await;
    assert_eq!(
        count_on(&seen, &id, "turn_started"),
        0,
        "an adopted budget kill with no cancel holds the thread: {:?}",
        kinds(&seen)
    );
    assert!(m.store.get_parked_turn(&id).unwrap().is_some(), "its parked row is still there");

    cfg.cash_usd = Some(500.0);
    m.budgets.set_config(&cfg).unwrap();
    assert!(
        eventually(|| count_on(&seen, &id, "turn_started") >= 1).await,
        "raising the limit releases it: {:?}",
        kinds(&seen)
    );
}
