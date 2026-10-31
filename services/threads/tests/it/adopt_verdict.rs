//! **L-2: A TURN ADOPTED AFTER A RESTART LANDS WITH THE VERDICT `run_turn`
//! WOULD HAVE GIVEN IT.**
//!
//! GAP-AUDIT L-2. A restart finds a `running_turns` row whose child is still
//! alive and re-adopts it (`adopt_turn`): it tails the child's output file
//! until the pid dies. Before this fix the adopted ending never reached the
//! verdict-to-outcome match `run_turn` uses, so:
//! * a 529 in the adopted tail was lost — no retry, no `turn_failed`, the
//!   operator's message unanswered;
//! * a successful adopted retry left the stale retry row behind, and the same
//!   prompt ran (and was paid for) again;
//! * a credit error in the adopted tail never barred the account.
//!
//! The restart is REAL as far as the manager can tell: the rows are written by
//! one manager, it is dropped, and a second one is built on the same data dir
//! and runs `recover`. The adopted child is a real process this test spawned
//! (a `sleep`), so `pid_alive` answers from the OS; the stream it "wrote" is a
//! file prepared before the restart, exactly as a real child's output file is.
//!
//! The tests are a set in which no constant implementation passes all of them
//! (doctrine 2): "always retry" fails the success pair, "never retry" fails
//! the 529 test, "always bar" fails the 529 and success tests' bar checks,
//! "never bar" fails the credit test.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use threads::event::{StoredEvent, ThreadEvent};
use threads::settings::ThreadSettings;
use threads::store::{RetryRow, RunningTurnRow};
use threads::{build_manager, Config, ThreadManager};

const WORDS: &str = "the operator's words, carried across a restart";
const MSG: u64 = 7;
const ACCOUNT: &str = "solo";

/// The CLI's own ending after it burned its internal retries against a 529:
/// the overload as a synthetic assistant message, then a result that calls
/// itself a success and flags itself an error (fixture `errorresult`).
const OVERLOAD_TEXT: &str = "API Error: 529 Overloaded. This is a server-side issue, usually \
                             temporary — try again in a moment.";
const CREDIT_TEXT: &str = "Credit balance is too low";

fn init_line() -> String {
    r#"{"type":"system","subtype":"init","session_id":"fake-sess","model":"claude-fable-5"}"#
        .to_string()
}

fn synthetic_text(text: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {
            "model": "<synthetic>",
            "content": [{"type": "text", "text": text}],
            "usage": {"input_tokens": 0, "output_tokens": 0}
        }
    })
    .to_string()
}

fn errored_result() -> String {
    r#"{"type":"result","subtype":"success","is_error":true,"total_cost_usd":0,"num_turns":1,"duration_ms":50,"stop_reason":"stop_sequence","modelUsage":{},"session_id":"fake-sess"}"#.to_string()
}

fn clean_result() -> String {
    r#"{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.01,"num_turns":1,"duration_ms":50,"stop_reason":"end_turn","modelUsage":{},"session_id":"fake-sess"}"#.to_string()
}

fn set_env() {
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake);
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &Path) -> Config {
    let mut map = BTreeMap::new();
    let cd = dir.join(ACCOUNT);
    std::fs::create_dir_all(&cd).unwrap();
    map.insert(ACCOUNT.to_string(), cd.to_string_lossy().to_string());
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts: map,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s.account = threads::settings::AccountChoice::Named(ACCOUNT.into());
    s
}

/// The adopted child: a real process, alive at `recover`, gone ~`ms` later.
/// Reaped by a helper thread so its pid really disappears (no zombie), and
/// killed on drop so a red run never leaks it.
///
/// **Kill only what you started (R9.L2b, inspect finding 3).** The pid is
/// signalled only while the child is still UNREAPED, i.e. while the pid is
/// still ours: the helper reaps (`try_wait`) and the drop kills under ONE
/// lock, so the drop either sees the child already reaped and sends nothing,
/// or kills a child that cannot have been reaped yet (a zombie's pid is never
/// recycled). Before this, the drop sent SIGKILL to a pid the helper had
/// released ~1.2 s earlier — on a busy machine, possibly someone else's.
///
/// **Waiting for the reap is a LATCH, not a clock (R9.L2c, inspect F-2).**
/// Whoever reaps the child — the helper or [`Orphan::release`] — sets `reaped`
/// and notifies the condvar under the same lock, and [`Orphan::wait_reaped`]
/// blocks on that condvar. A test that needs "the helper has reaped it" waits
/// for the helper's own act, however late the helper is scheduled, instead of
/// assuming the helper's 20 ms poll has already run.
struct Orphan {
    pid: i64,
    child: std::sync::Arc<(std::sync::Mutex<OrphanChild>, std::sync::Condvar)>,
}

struct OrphanChild {
    proc: std::process::Child,
    /// Set by whichever side reaped it: its exit status.
    reaped: Option<std::process::ExitStatus>,
}

/// What [`Orphan::release`] did.
#[derive(Debug, PartialEq, Eq)]
enum Released {
    /// The child was still ours (unreaped): it was SIGKILLed and reaped.
    Killed,
    /// The helper had already reaped it: no signal was sent.
    AlreadyReaped,
}

impl Orphan {
    fn spawn(ms: u64) -> Orphan {
        let secs = format!("{:.3}", ms as f64 / 1000.0);
        let proc = std::process::Command::new("sleep")
            .arg(secs)
            .spawn()
            .unwrap();
        Self::reaped_by_helper(proc)
    }

    fn reaped_by_helper(proc: std::process::Child) -> Orphan {
        let pid = proc.id() as i64;
        let child = std::sync::Arc::new((
            std::sync::Mutex::new(OrphanChild { proc, reaped: None }),
            std::sync::Condvar::new(),
        ));
        let helper = std::sync::Arc::clone(&child);
        std::thread::spawn(move || loop {
            {
                let (lock, reaped_cv) = &*helper;
                let mut c = lock.lock().unwrap();
                if c.reaped.is_some() {
                    return;
                }
                if let Ok(Some(status)) = c.proc.try_wait() {
                    c.reaped = Some(status);
                    reaped_cv.notify_all();
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        });
        Orphan { pid, child }
    }

    /// **The adopted child, ended by a LATCH, not a clock (R9.F2i).** A `cat`
    /// reading a pipe this fixture holds: alive for exactly as long as the
    /// test keeps the write end open, however slowly a loaded machine gets
    /// from spawn to `recover`, and ending ON ITS OWN (EOF, exit 0, no
    /// signal) when [`Orphan::let_it_end`] drops it. `Orphan::spawn(1200)`
    /// raced the fixture's own setup (two `build_manager`s and the ledger
    /// writes) against a 1.2 s `sleep`: when setup lost, `recover` found the
    /// pid dead and never adopted it.
    fn spawn_gated() -> Orphan {
        let proc = std::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        Self::reaped_by_helper(proc)
    }

    /// Close the gated child's stdin: it reads EOF and exits 0 by itself.
    fn let_it_end(&self) {
        let mut c = self.child.0.lock().unwrap();
        assert!(c.reaped.is_none(), "sentinel: the gated child is still alive when its gate opens");
        drop(c.proc.stdin.take().expect("a gated child (spawn_gated)"));
    }

    /// Has the helper reaped the child (its pid is gone)?
    fn is_reaped(&self) -> bool {
        self.child.0.lock().unwrap().reaped.is_some()
    }

    /// Block until the child has been reaped, woken by the reaper itself.
    /// `limit` is a HANG guard only — the wait ends on the reap, never on the
    /// clock, so it does not race the helper's poll. Returns whether it was
    /// reaped.
    fn wait_reaped(&self, limit: Duration) -> bool {
        let (lock, reaped_cv) = &*self.child;
        let c = lock.lock().unwrap();
        let (c, _) = reaped_cv
            .wait_timeout_while(c, limit, |c| c.reaped.is_none())
            .unwrap();
        c.reaped.is_some()
    }

    /// End the child if, and only if, it is still ours. Idempotent.
    fn release(&self) -> Released {
        let (lock, reaped_cv) = &*self.child;
        let mut c = lock.lock().unwrap();
        if c.reaped.is_some() {
            return Released::AlreadyReaped;
        }
        // Unreaped under the lock: the pid cannot have been recycled.
        let _ = c.proc.kill();
        let status = c.proc.wait().ok();
        c.reaped = status;
        reaped_cv.notify_all();
        Released::Killed
    }

    /// The exit status, once reaped.
    fn status(&self) -> Option<std::process::ExitStatus> {
        self.child.0.lock().unwrap().reaped
    }
}

impl Drop for Orphan {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// Where the previous daemon was when it was restarted: message `MSG` queued
/// and delivered, a `turn_started` naming it (attempt `attempt`), a live child
/// still running it, and that child's output file holding `lines`. The first
/// `seen` of those lines were already read by the previous daemon, so the row's
/// `byte_offset` points past them. `stale_retry` seeds the retry row that a
/// retry attempt leaves behind while it runs (it is the row that FIRED it).
/// Returns the thread id and a manager built FRESH on the same data dir.
fn previous_daemon_restarted_mid_turn(
    dir: &Path,
    attempt: u32,
    lines: &[String],
    seen: usize,
    stale_retry: bool,
    child: &Orphan,
) -> (String, ThreadManager) {
    let output = dir.join("adopted.jsonl");
    let mut body = String::new();
    let mut offset = 0u64;
    for (i, l) in lines.iter().enumerate() {
        body.push_str(l);
        body.push('\n');
        if i + 1 == seen {
            offset = body.len() as u64;
        }
    }
    std::fs::write(&output, body).unwrap();
    let id = {
        let m1 = build_manager(config_at(dir)).unwrap();
        let id = m1.create_thread(None, settings(dir)).unwrap();
        let qid = m1
            .store
            .enqueue(&id, MSG, WORDS, "immediate", &serde_json::Value::Null, &[])
            .unwrap();
        m1.store.mark_delivered(&[qid]).unwrap();
        let seq = m1.store.max_seq(&id).unwrap() + 1;
        m1.store
            .append_event(&StoredEvent {
                thread_id: id.clone(),
                seq,
                message_id: None,
                ts: threads::store::now_rfc3339(),
                event: ThreadEvent::TurnStarted {
                    attempt,
                    message_ids: vec![MSG],
                },
            })
            .unwrap();
        if stale_retry {
            m1.store
                .upsert_retry(&RetryRow {
                    thread_id: id.clone(),
                    prompt: WORDS.to_string(),
                    message_ids: vec![MSG],
                    attempt,
                    kind: "overloaded".into(),
                    reason: "the attempt the restart caught was itself a retry".into(),
                    account: Some(ACCOUNT.into()),
                    created_at: threads::store::now_rfc3339(),
                })
                .unwrap();
        }
        m1.store
            .upsert_running_turn(&RunningTurnRow {
                thread_id: id.clone(),
                pid: child.pid,
                spawn_cookie: "cookie".into(),
                output_path: output.to_string_lossy().to_string(),
                byte_offset: offset,
                started_at: threads::store::now_rfc3339(),
                prompt: Some(WORDS.to_string()),
                message_ids: vec![MSG],
                attempt,
            })
            .unwrap();
        id
    };
    let m2 = build_manager(config_at(dir)).unwrap();
    (id, m2)
}

fn ledger_kinds(m: &ThreadManager, id: &str) -> Vec<String> {
    m.store
        .list_events(id, 0, 10_000, None)
        .unwrap()
        .iter()
        .map(|e| e.event.kind().to_string())
        .collect()
}

/// Waits for the adopted turn to END AND BE FINISHED: `turn_ended` on the
/// ledger and the `adopt` execution lock gone from the store. That lock is
/// taken by `recover` before the adopter starts and released only in
/// `finish_turn`, which runs AFTER the verdict has landed (synchronously), so
/// every assertion after this reads a settled outcome. (A first draft waited
/// on the thread no longer reading `running`; that flips at
/// `process_exited`, BEFORE the verdict, and a forced run caught it mid-way:
/// the bar written, the retry row not yet.)
async fn adopted_turn_finished(m: &ThreadManager, id: &str, child: &Orphan) {
    let adopt_lock_held = || {
        m.store
            .list_thread_locks()
            .unwrap()
            .iter()
            .any(|l| l.thread_id == id && l.entry == "adopt")
    };
    // R9.F2i: the child is GATED, so it is provably alive here and the adopt
    // lock cannot have been released yet. The old `|| turn_ended` escape is
    // gone: with a 1.2 s clock-bound child a loaded machine could reach
    // `recover` after the child had died, `recover` then took the
    // `turn_interrupted` arm (no adopt lock, no `turn_ended`), and this
    // sentinel was red 1 in 3 on base 7ef22e73.
    assert!(
        adopt_lock_held(),
        "sentinel: recover adopted the live child and took the adopt lock: ledger = {:?}",
        ledger_kinds(m, id)
    );
    // Only now may the adopted child end — on its own (EOF on stdin, exit 0).
    child.let_it_end();
    for _ in 0..800 {
        let ended = ledger_kinds(m, id).iter().any(|k| k == "turn_ended");
        if ended && !adopt_lock_held() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "the adopted turn never finished: ledger = {:?}, state = {:?}",
        ledger_kinds(m, id),
        m.thread_state(id).map(|s| s.state)
    );
}

fn credit_barred(m: &ThreadManager) -> bool {
    m.store
        .list_account_states()
        .unwrap()
        .into_iter()
        .any(|s| s.name == ACCOUNT && s.limit_axis.as_deref() == Some("credit"))
}

fn turn_starts(kinds: &[String]) -> usize {
    kinds.iter().filter(|k| *k == "turn_started").count()
}

/// **The adopter must not kill the turn it adopted (R9.L2d, L-2; R9.L2c
/// inspect G-1).** Adoption means "tail it until it ends", so a gated child
/// the product adopted must end ON ITS OWN: exit code 0, no signal.
///
/// This replaces `release() == AlreadyReaped` after `wait_reaped`, which was
/// `x == x` (both operands the fixture's own `reaped`, just established by
/// the wait) and stayed green when the product SIGKILLed the adopted child
/// (inspect forcing FK, 6/6 green). The status read here is the OS's answer
/// for the child, recorded by whichever side reaped it, so it differs
/// between "ended on its own" (`code() == Some(0)`) and "killed"
/// (`code() == None`, `signal() == Some(9)`) — the pair test
/// `an_orphan_is_killed_only_while_its_pid_is_still_ours` is the sentinel
/// that the fixture records both shapes. The fixture-hygiene property the old
/// line named ("the drop does not signal a reaped pid") is guarded there.
///
/// Waits on the reaper's own latch, never on the clock (R9.L2c F-2): the
/// product's `pid_alive` counts a zombie as dead, so the turn can finish
/// before the helper reaps. `30 s` is a hang guard only.
fn the_adopter_left_its_child_to_end_on_its_own(child: &Orphan) {
    use std::os::unix::process::ExitStatusExt;
    assert!(
        tokio::task::block_in_place(|| child.wait_reaped(Duration::from_secs(30))),
        "the adopted child (gated `cat`) was never reaped by the fixture's helper"
    );
    let status = child.status();
    assert_eq!(
        status.map(|s| (s.code(), s.signal())),
        Some((Some(0), None)),
        "L-2: the adopter must tail its child until it ends, not end it — the adopted \
         child did not exit on its own: {status:?}"
    );
}

/// **(a) An adopted child that ends with a 529 goes on the transient ladder,
/// once, at `attempt + 1`.** Red at base: no retry row, the message is lost.
#[tokio::test(flavor = "multi_thread")]
async fn an_adopted_child_that_ends_with_a_529_is_retried_once_on_the_transient_ladder() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let child = Orphan::spawn_gated();
    let lines = vec![init_line(), synthetic_text(OVERLOAD_TEXT), errored_result()];
    let (id, m) = previous_daemon_restarted_mid_turn(tmp.path(), 1, &lines, 1, false, &child);

    m.recover().await;
    adopted_turn_finished(&m, &id, &child).await;

    // The kind run_turn would have put it on: the SAME classifier, read here
    // rather than hard-coded, so this test follows the classifier.
    let verdict = threads::retry::classify_errored_result("success", Some(OVERLOAD_TEXT));
    let (want_kind, _) = threads::retry::ladder_for(&verdict);
    assert!(
        want_kind.kind_str() != "credit_balance" && want_kind.kind_str() != "claude_error",
        "sentinel: a 529 must classify as a transient, got {}",
        want_kind.kind_str()
    );
    // The REASON is what tells a classified 529 from any other death (R9.L2b,
    // inspect finding 1): `Overload` and `ErrorDuringExecution` share the
    // wire tag "transient", so `retry.kind` alone is `x == x` between the
    // two. The sentinel proves the two operands differ: the classifier's
    // reason names the overload, and the generic death's reason does not.
    let want_reason = match &verdict {
        threads::retry::RetryDecision::Retry { reason, .. } => reason.clone(),
        other => panic!("sentinel: a 529 must classify as a Retry, got {other:?}"),
    };
    let generic = threads::retry::classify_errored_result("success", None);
    let generic_reason = match &generic {
        threads::retry::RetryDecision::Retry { reason, .. } => reason.clone(),
        other => panic!("sentinel: an unnamed error must classify as a Retry, got {other:?}"),
    };
    assert!(
        want_reason.contains("overload") && !generic_reason.contains("overload"),
        "sentinel: the 529 reason ({want_reason:?}) must name the overload and a generic \
         death's ({generic_reason:?}) must not"
    );
    let kinds = ledger_kinds(&m, &id);
    let retry = m.store.get_retry(&id).unwrap().unwrap_or_else(|| {
        panic!(
            "L-2: a 529 in an adopted turn must land on the transient ladder; no retry row. \
             thread state = {:?}, ledger = {kinds:?}",
            m.thread_state(&id).map(|s| s.state)
        )
    });
    assert_eq!(
        retry.kind,
        want_kind.kind_str(),
        "the ladder run_turn would pick"
    );
    assert_eq!(
        retry.reason, want_reason,
        "the classifier's verdict, not a generic death (both are kind \"transient\")"
    );
    assert_eq!(retry.prompt, WORDS, "the prompt is carried verbatim");
    assert_eq!(retry.message_ids, vec![MSG], "with the delivery it answers");
    assert_eq!(
        retry.attempt, 2,
        "once, continuing the adopted attempt's ladder"
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "retry_scheduled").count(),
        1,
        "scheduled exactly once: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|k| k == "turn_failed"),
        "not given up: {kinds:?}"
    );
    assert_eq!(
        turn_starts(&kinds),
        1,
        "scheduled, not re-run inline: {kinds:?}"
    );
    assert!(
        !credit_barred(&m),
        "a 529 is not a credit error — nothing is barred"
    );
    assert!(
        m.store.failed_deliveries(&id).unwrap().is_empty(),
        "the delivery is still live"
    );
    the_adopter_left_its_child_to_end_on_its_own(&child);
}

/// **(b) An adopted RETRY that ends with success is not run again.** Its
/// stale retry row is cleared, exactly as `run_turn` clears it on success.
/// Red at base: the row survives and the same paid prompt would run again.
#[tokio::test(flavor = "multi_thread")]
async fn an_adopted_child_that_ends_with_success_is_not_re_run() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let child = Orphan::spawn_gated();
    let lines = vec![init_line(), synthetic_text("done."), clean_result()];
    let (id, m) = previous_daemon_restarted_mid_turn(tmp.path(), 2, &lines, 1, true, &child);
    assert!(
        m.store.get_retry(&id).unwrap().is_some(),
        "sentinel: the stale retry row is in place before the restart"
    );

    m.recover().await;
    adopted_turn_finished(&m, &id, &child).await;

    let kinds = ledger_kinds(&m, &id);
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "L-2: a successful adopted turn must clear its retry row, or the same prompt runs \
         again: {kinds:?}"
    );
    assert!(!kinds.iter().any(|k| k == "retry_scheduled"), "{kinds:?}");
    assert!(!kinds.iter().any(|k| k == "turn_failed"), "{kinds:?}");
    assert_eq!(turn_starts(&kinds), 1, "not re-run: {kinds:?}");
    assert!(!credit_barred(&m), "a success bars nothing");
    the_adopter_left_its_child_to_end_on_its_own(&child);
}

/// **(b, paired) The clean result arrived BEFORE the restart** — the previous
/// daemon read it, then the child lingered (a hook, an MCP server) and the
/// restart caught it. The adopted tail is empty. E17-8: once a clean result
/// has arrived the turn never retries, so an ending with no NEW verdict must
/// not be read as a death and re-run.
#[tokio::test(flavor = "multi_thread")]
async fn an_adopted_child_whose_result_arrived_before_the_restart_is_not_re_run() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let child = Orphan::spawn_gated();
    let lines = vec![init_line(), synthetic_text("done."), clean_result()];
    let (id, m) = previous_daemon_restarted_mid_turn(tmp.path(), 2, &lines, 3, true, &child);

    m.recover().await;
    adopted_turn_finished(&m, &id, &child).await;

    let kinds = ledger_kinds(&m, &id);
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "the result before the restart is the verdict: no retry row: {kinds:?}"
    );
    assert!(!kinds.iter().any(|k| k == "retry_scheduled"), "{kinds:?}");
    assert!(!kinds.iter().any(|k| k == "turn_failed"), "{kinds:?}");
    assert_eq!(turn_starts(&kinds), 1, "not re-run: {kinds:?}");
    the_adopter_left_its_child_to_end_on_its_own(&child);
}

/// **(b, opposing) An adopted child that ends with NO verdict at all** — no
/// result before or after the restart — died mid-execution. It goes on the
/// ladder (the class an unclassified mid-execution death gets), so "an empty
/// adopted ending is a success" fails here.
#[tokio::test(flavor = "multi_thread")]
async fn an_adopted_child_that_ends_with_no_result_at_all_is_retried() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let child = Orphan::spawn_gated();
    let lines = vec![init_line(), synthetic_text("working on it…")];
    let (id, m) = previous_daemon_restarted_mid_turn(tmp.path(), 1, &lines, 1, false, &child);

    m.recover().await;
    adopted_turn_finished(&m, &id, &child).await;

    let kinds = ledger_kinds(&m, &id);
    let retry = m.store.get_retry(&id).unwrap().unwrap_or_else(|| {
        panic!("L-2: an adopted child that died with no verdict must be retried: {kinds:?}")
    });
    assert_eq!(retry.attempt, 2);
    assert_eq!(
        retry.kind,
        threads::retry::TransientKind::ErrorDuringExecution.kind_str(),
        "the class an unclassified mid-execution death gets"
    );
    the_adopter_left_its_child_to_end_on_its_own(&child);
}

/// **(c) An adopted child that ends with a credit error bars the account.**
/// Red at base: nothing is barred, and the message is lost.
#[tokio::test(flavor = "multi_thread")]
async fn an_adopted_child_that_ends_with_a_credit_error_bars_the_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let child = Orphan::spawn_gated();
    let lines = vec![init_line(), synthetic_text(CREDIT_TEXT), errored_result()];
    let (id, m) = previous_daemon_restarted_mid_turn(tmp.path(), 1, &lines, 1, false, &child);
    assert!(
        !credit_barred(&m),
        "sentinel: not barred before the restart"
    );

    m.recover().await;
    adopted_turn_finished(&m, &id, &child).await;

    let kinds = ledger_kinds(&m, &id);
    assert!(
        credit_barred(&m),
        "L-2: a credit error in an adopted turn must bar the account: accounts = {:?}, \
         ledger = {kinds:?}",
        m.store
            .list_account_states()
            .unwrap()
            .into_iter()
            .map(|s| (s.name, s.limit_axis))
            .collect::<Vec<_>>()
    );
    let retry = m.store.get_retry(&id).unwrap();
    assert_eq!(
        retry.as_ref().map(|r| r.kind.as_str()),
        Some("credit_balance"),
        "handed back on the credit ladder, as run_turn does: {kinds:?}"
    );
    the_adopter_left_its_child_to_end_on_its_own(&child);
}

// R9.F2b: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;

/// **The fixture kills only a child it still owns (R9.L2b, inspect finding
/// 3).** A pair, so neither constant passes: "always kill" (the old drop)
/// fails the reaped half, "never kill" fails the live half.
#[test]
fn an_orphan_is_killed_only_while_its_pid_is_still_ours() {
    // Live half: a child that would outlive the test by a minute is still
    // unreaped, so release SIGKILLs it — and it really dies of that signal.
    let live = Orphan::spawn(60_000);
    assert!(!live.is_reaped(), "sentinel: a 60 s sleep is still running");
    assert_eq!(live.release(), Released::Killed, "an unreaped orphan is killed");
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        live.status().and_then(|s| s.signal()),
        Some(9),
        "it died of the SIGKILL, not of its own accord"
    );

    // Reaped half: a 50 ms child, waited out until the helper has reaped it.
    let gone = Orphan::spawn(50);
    assert!(
        gone.wait_reaped(Duration::from_secs(10)),
        "the helper never reaped a 50 ms sleep"
    );
    assert_eq!(gone.status().map(|s| s.code()), Some(Some(0)), "it exited on its own");
    assert_eq!(
        gone.release(),
        Released::AlreadyReaped,
        "a reaped orphan's pid is no longer ours: no signal is sent"
    );
}
