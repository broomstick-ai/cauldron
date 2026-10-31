//! **A COMPACTION ENDS THE TURN. IT DOES NOT LAUNCH ONE.**
//!
//! The operator, 2026-08-28, in his own words:
//!
//! > *"I no longer want automatic compactions. Like an automatic compaction on
//! > its own is actually fine. What I really don't want is resume after
//! > compaction. I want to have a flag when we're running a thread, when we're
//! > submitting a thread job into the pool. I want to have a flag called resume
//! > after compaction, which defaults to false… because it makes such
//! > catastrophic errors after every compaction, every single time. I really
//! > need to give it a preamble to let it know, okay, here's what you're doing,
//! > here's the mistakes you're going to make, don't make those mistakes. I
//! > just want it to stop. I want it to end. I want the turn to end. It should
//! > not keep going on its own."*
//!
//! Read precisely, that is three separate claims, and each one is a test here:
//!
//! * The compaction itself is FINE and still happens. Nothing in this file
//!   prevents one, and `the_compaction_itself_still_happens_and_the_work_before_it_survives`
//!   pins that the boundary is recorded and the pre-compaction work is on the
//!   ledger exactly as before.
//! * What stops is the thread's own MOMENTUM afterwards — the turn ends at the
//!   boundary rather than carrying on into the confused post-compaction work
//!   that has cost him repeatedly.
//! * It is a FLAG, and `true` still carries on, so the branch is real rather
//!   than a behaviour change wearing a flag's name.
//!
//! And one rule that governs all of it and is his paramount one: **his words
//! are never lost and never silently fail to reach the thread.** A turn that
//! ends early must therefore not eat the messages queued behind it, which is
//! `messages_queued_behind_a_stopped_turn_are_not_consumed_by_it` — the
//! durability case, and the one that must not regress.
//!
//! Every test drives the manager in-process against the FAKE claude fixture
//! (`tests/fixtures/fake-claude`, mode `compactturn`). No real claude turn is
//! ever spawned; the manager gets an explicit `data_dir` under a tempdir and
//! `THREADS_CLAUDE_HOME` points transcript storage at a scratch dir, so nothing
//! here touches the operator's own `~/.claude`, the running daemon, or the
//! keychain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use threads::event::{Push, StoredEvent, ThreadEvent};
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env() {
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "200");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
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

/// Settings with `resume_after_compaction` left at ITS OWN DEFAULT. Nothing
/// here sets the field, deliberately: the point of the change is that the
/// stopping behaviour is what a caller gets by forgetting, so a test that set
/// it explicitly would prove the branch works and say nothing about the
/// default.
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

fn saw(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> bool {
    seen.lock().unwrap().iter().any(|e| e.event.kind() == kind)
}

fn count(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|e| e.event.kind() == kind)
        .count()
}

/// Every scrap of assistant text the thread produced, in order.
fn assistant_text(seen: &Arc<Mutex<Vec<StoredEvent>>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            ThreadEvent::AssistantText { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The `message_ids` named by each `TurnStarted`, in order — i.e. which of his
/// messages each spawned turn actually claimed.
fn turn_batches(seen: &Arc<Mutex<Vec<StoredEvent>>>) -> Vec<Vec<u64>> {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            ThreadEvent::TurnStarted { message_ids, .. } => Some(message_ids.clone()),
            _ => None,
        })
        .collect()
}

fn interrupt_triggers(seen: &Arc<Mutex<Vec<StoredEvent>>>) -> Vec<(String, String)> {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            ThreadEvent::TurnInterrupted { reason, trigger } => {
                Some((trigger.clone(), reason.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Every prompt the fake CLI received, in order — the fixture appends each one
/// verbatim, NUL-separated, when the prompt carries `promptfile=<path>`.
fn captured(path: &std::path::Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    // R9.F2: only NUL-TERMINATED records are prompts. The fixture appends each
    // one with `printf '%s\0'`, and a long prompt reaches the file in more than
    // one write(2); a read landing between them used to count the torn head,
    // which has no terminator yet, as a whole prompt (`context_id` red in the
    // full suite, green alone). Everything after the last NUL is still in flight.
    let raw = raw.rsplit_once('\0').map(|(done, _in_flight)| done).unwrap_or("");
    raw.split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..400 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

/// The post-compaction line the fixture emits 1.5s after the boundary. Its
/// presence on the ledger IS the thread carrying on under its own momentum.
const AFTER: &str = "AFTER THE COMPACTION";

// ── 1. THE DEFAULT: THE TURN ENDS AT THE BOUNDARY ───────────────────────────

/// **With the flag at its default, a compaction ends the turn — and the ending
/// is a recorded, legible, non-failure terminal state.**
///
/// Forced to fail against the pre-change code, this asserted nothing subtle:
/// the fixture's `AFTER THE COMPACTION` line landed on the ledger every time,
/// because nothing in the crate interrupted a turn on a compaction. Compaction
/// was pure bookkeeping and the child kept streaming.
#[tokio::test]
async fn a_compaction_ends_the_turn_and_the_thread_does_not_carry_on() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    m.send(
        &id,
        "compactturn please — this is the operator's real turn".into(),
        Default::default(),
        json!({}),
        false,
    )
    .unwrap();

    // The turn reaches a terminal state.
    assert!(
        eventually(|| saw(&seen, "turn_ended")).await,
        "the turn must reach a terminal state, saw {:?}",
        kinds(&seen)
    );

    // ── THE POINT. The thread does not carry on past the boundary.
    //
    // Given a moment longer than the fixture's own 1.5s momentum window, so
    // this cannot pass merely by reading the ledger too early.
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let texts = assistant_text(&seen);
    assert!(
        !texts.iter().any(|t| t.contains(AFTER)),
        "the thread kept going after the compaction — that is the whole failure \
         this flag exists to stop. Assistant text on the ledger: {texts:?}"
    );

    // ── THE ENDING IS TELLABLE FROM A CRASH.
    //
    // It lands as `turn_interrupted` with trigger `compaction`: the daemon's ONE
    // interrupt vocabulary, carrying a cause that names an intention rather than
    // a fault. A reader that already distinguishes `operator` from
    // `daemon_restart` gets this third case for free and in the same field.
    let triggers = interrupt_triggers(&seen);
    assert_eq!(
        triggers.len(),
        1,
        "exactly one interrupt should be recorded, got {triggers:?}"
    );
    assert_eq!(
        triggers[0].0, "compaction",
        "the ledger must say WHAT ended the turn, got {triggers:?}"
    );
    assert!(
        triggers[0].1.contains("compaction"),
        "the reason must read as prose a human can act on, got {triggers:?}"
    );

    // NOT a failure and NOT a kill. Both of those are how a reader recognises
    // something going wrong, and nothing went wrong here.
    assert!(
        !saw(&seen, "turn_failed"),
        "a clean ending is not a failure: {:?}",
        kinds(&seen)
    );
    assert!(
        !saw(&seen, "turn_killed"),
        "a clean ending is not a kill: {:?}",
        kinds(&seen)
    );

    // ── AND IT SPAWNS NOTHING FURTHER.
    assert_eq!(
        count(&seen, "turn_started"),
        1,
        "the delivery ENDS here; nothing may re-spawn it: {:?}",
        kinds(&seen)
    );
    assert!(
        !saw(&seen, "retry_scheduled"),
        "an ending the operator asked for is not a failure to retry: {:?}",
        kinds(&seen)
    );
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "no retry row may be left behind holding this delivery"
    );
}

// ── 2. THE OTHER BRANCH IS REAL ─────────────────────────────────────────────

/// **With `resume_after_compaction: true`, the thread carries on.**
///
/// Without this the change would be indistinguishable from simply killing
/// every turn that compacts, and the flag would be decoration. Forced to fail
/// by inverting it to the default — it then reproduces test 1 and the
/// `AFTER THE COMPACTION` assertion below fails.
#[tokio::test]
async fn with_resume_after_compaction_the_thread_carries_on() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();

    let mut s = settings(tmp.path());
    s.resume_after_compaction = true;
    let id = m.create_thread(None, s).unwrap();
    let seen = watch(&m);
    m.send(
        &id,
        "compactturn please — carry on".into(),
        Default::default(),
        json!({}),
        false,
    )
    .unwrap();

    assert!(
        eventually(|| assistant_text(&seen).iter().any(|t| t.contains(AFTER))).await,
        "with the flag ON the thread must keep going past the compaction — \
         otherwise the branch is not real. Assistant text: {:?}, events: {:?}",
        assistant_text(&seen),
        kinds(&seen)
    );

    assert!(
        eventually(|| saw(&seen, "turn_ended")).await,
        "and it still ends normally, of its own accord: {:?}",
        kinds(&seen)
    );

    // Nothing interrupted it: this branch is the old behaviour, untouched.
    assert!(
        interrupt_triggers(&seen).is_empty(),
        "a resuming thread is never interrupted by the compaction: {:?}",
        interrupt_triggers(&seen)
    );
}

// ── 3. THE DURABILITY CASE ──────────────────────────────────────────────────

/// **A turn that ends early does not eat the messages queued behind it.**
///
/// His paramount rule, and the one regression that would matter most: a message
/// sent while the compacting turn was still running must not be consumed by
/// that turn's early ending. It waits, and a LATER turn delivers it.
///
/// The assertion is made on `TurnStarted.message_ids` rather than on the queue
/// table, because that is the ledger's own record of which of his messages each
/// spawned turn actually claimed — the same field `Store::stranded_deliveries`
/// reasons about.
#[tokio::test]
async fn messages_queued_behind_a_stopped_turn_are_not_consumed_by_it() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);

    let first = m
        .send(
            &id,
            "compactturn please — the turn that will end early".into(),
            Default::default(),
            json!({}),
            false,
        )
        .unwrap();

    // Wait until the turn is genuinely STREAMING — not merely started — and
    // then speak again. This is him talking while the thread is working, which
    // is exactly when it is easiest to lose a message.
    //
    // Waiting for the assistant text rather than for `turn_started` is
    // load-bearing, and it is the difference between this test proving
    // something and proving nothing. The fixture emits its text, then holds for
    // 600ms, and only then compacts. Synchronising on the text therefore puts
    // this message on the queue while the turn is alive and BEFORE the
    // compaction is seen — the one ordering in which a turn ending at the
    // boundary could swallow it. Synchronising on `turn_started` instead loses
    // that race: the boundary can arrive first, the queue is empty when the
    // ending runs, and a version that swallows the queue passes anyway.
    assert!(
        eventually(|| assistant_text(&seen)
            .iter()
            .any(|t| t.contains("Hello from fake claude")))
        .await,
        "the first turn must be streaming before he speaks again: {:?}",
        kinds(&seen)
    );
    assert!(
        !saw(&seen, "compacted"),
        "…and it must not have compacted yet, or this test is not exercising \
         the window it exists for: {:?}",
        kinds(&seen)
    );
    let second = m
        .send(
            &id,
            "the words he said while it was compacting".into(),
            Default::default(),
            json!({}),
            false,
        )
        .unwrap();
    assert_ne!(first, second, "two distinct messages");

    // The second message is delivered — by a SECOND turn, not the first.
    assert!(
        eventually(|| turn_batches(&seen)
            .iter()
            .any(|b| b.contains(&second)))
        .await,
        "his second message must reach the thread. Turn batches: {:?}, events: {:?}",
        turn_batches(&seen),
        kinds(&seen)
    );

    let batches = turn_batches(&seen);
    assert!(
        batches.len() >= 2,
        "the queued message belongs to a LATER turn, not the one that stopped: {batches:?}"
    );
    assert!(
        batches[0].contains(&first) && !batches[0].contains(&second),
        "the turn that ended early must claim ONLY the message it was started \
         for — anything else is his words being swallowed by a turn that \
         stopped before it could act on them: {batches:?}"
    );
    assert!(
        batches[1..].iter().any(|b| b.contains(&second)),
        "and the queued message is claimed by a later turn: {batches:?}"
    );

    // Nothing was dropped or failed on the way.
    assert!(
        !saw(&seen, "turn_failed"),
        "no message may be lost to a failure here: {:?}",
        kinds(&seen)
    );
}

// ── 4. THE COMPACTION ITSELF IS UNTOUCHED ───────────────────────────────────

/// **The compaction still happens, and everything the turn produced before it
/// is still on the ledger.**
///
/// He was explicit that the compaction is fine. This pins that the change ends
/// a turn and does nothing else: the boundary is recorded with its swallowed
/// message-id range, and the work before it survives the ending.
#[tokio::test]
async fn the_compaction_itself_still_happens_and_the_work_before_it_survives() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    m.send(
        &id,
        "compactturn please".into(),
        Default::default(),
        json!({}),
        false,
    )
    .unwrap();

    assert!(
        eventually(|| saw(&seen, "compacted")).await,
        "the compaction is FINE and is still recorded: {:?}",
        kinds(&seen)
    );
    assert!(
        eventually(|| saw(&seen, "turn_ended")).await,
        "and the turn then ends: {:?}",
        kinds(&seen)
    );

    // The work done BEFORE the boundary is preserved.
    let texts = assistant_text(&seen);
    assert!(
        texts.iter().any(|t| t.contains("Hello from fake claude")),
        "the turn's pre-compaction work must survive the ending: {texts:?}"
    );

    // The boundary carries its range, so the next turn's restatement can name
    // the messages that were summarised away.
    let range = seen.lock().unwrap().iter().find_map(|e| match &e.event {
        ThreadEvent::Compacted {
            message_id_range, ..
        } => Some(*message_id_range),
        _ => None,
    });
    assert!(
        matches!(range, Some(Some(_))),
        "the compaction must still stamp its swallowed message-id range, got {range:?}"
    );

    // …and the thread is flagged to restate on the next turn, which is the
    // mechanism the ending composes with rather than replaces.
    let row = m.store.get_thread(&id).unwrap().unwrap();
    assert!(
        row.needs_restate,
        "the NEXT turn must still be told it has just been through a compaction"
    );
}

// ── 5. THE STOP DOES NOT OUTLIVE THE TURN THAT EARNED IT ────────────────────

/// **A compaction in one turn never ends the NEXT turn.**
///
/// `compaction_stop` lives on the thread, not on the execution, because the
/// interpreter and the supervisor are different loops. That makes staleness a
/// real hazard rather than a theoretical one: the final drain after the child
/// exits is read AFTER the supervisor's teardown checks, so a boundary arriving
/// in the turn's last breath raises the flag with no consumer left to meet it.
///
/// Left raised, it would end the next turn — one that never compacted — on its
/// first poll. From outside that looks like the daemon stopping threads at
/// random, which is a worse failure than the one this whole change removes.
///
/// Forced to fail by making the flag never clear — `swap(false, ..)` weakened
/// to `load(..)` in the supervisor AND the post-loop drain disabled. The second
/// turn is then interrupted with trigger `compaction` despite no compaction
/// occurring in it, three times over.
///
/// Disabling only ONE of those two leaves this test green, which is worth
/// recording rather than glossing: the in-loop consumption and the post-loop
/// drain each independently prevent the leak on this path, so this test pins
/// the PROPERTY (a stop never outlives its turn) and not either mechanism. The
/// narrow ordering only the post-loop drain covers — a boundary read by the
/// final drain after `try_wait`, or an `Err` break — is not reachable from a
/// fixture, so the drain is guarding a window this suite cannot construct.
#[tokio::test]
async fn a_compaction_stop_never_leaks_into_the_following_turn() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);

    // Turn one compacts in its last breath and exits in the same poll.
    m.send(
        &id,
        "compactexit please".into(),
        Default::default(),
        json!({}),
        false,
    )
    .unwrap();
    assert!(
        eventually(|| saw(&seen, "compacted") && saw(&seen, "turn_ended")).await,
        "the first turn must compact and end: {:?}",
        kinds(&seen)
    );

    // Turn two is an ordinary turn. It must run and finish untouched.
    let second = m
        .send(
            &id,
            "an ordinary second turn that never compacts".into(),
            Default::default(),
            json!({}),
            false,
        )
        .unwrap();

    assert!(
        eventually(|| turn_batches(&seen).iter().any(|b| b.contains(&second))).await,
        "his second message must be delivered: {:?}",
        kinds(&seen)
    );
    assert!(
        eventually(|| count(&seen, "turn_ended") >= 2).await,
        "the second turn must reach its own terminal state: {:?}",
        kinds(&seen)
    );

    // Exactly one compaction happened, so at most one turn may have been ended
    // by one. A second `compaction` interrupt here is the stale flag.
    assert_eq!(
        count(&seen, "compacted"),
        1,
        "only the first turn compacts: {:?}",
        kinds(&seen)
    );
    let triggers = interrupt_triggers(&seen);
    assert!(
        triggers.len() <= 1,
        "a compaction in turn one must not also end turn two — the stop flag \
         outlived the turn that earned it: {triggers:?}"
    );
}

// ── 6. THE ENDING COMPOSES WITH THE RESTATEMENT ─────────────────────────────

/// **The turn the operator starts next is TOLD it has been through a
/// compaction.**
///
/// This is the half of his ask that is easy to lose sight of. He does not want
/// silence — he wants the thread stopped so that *he* can frame what comes
/// next, and the framing the daemon already owes it is the post-compaction
/// restatement: the thread id, the working path, and the range of message ids
/// the compaction swallowed, so the agent can go read what it no longer
/// remembers.
///
/// The ending must therefore ARM that restatement and not consume it. It does,
/// because both live in the same `Compacted` arm: the bookkeeping runs first
/// and unconditionally, and only then does the flag decide whether to stop.
/// This test reads the actual bytes the next turn's model received.
///
/// FORCED RED: clearing `needs_restate` on the stop, or moving the
/// `update_thread_compaction` call inside the `if resume` branch, leaves the
/// second prompt with no restatement and fails the assertion below. (The
/// mutation that skips the bookkeeping entirely is the one recorded on
/// `the_compaction_itself_still_happens_and_the_work_before_it_survives`.)
#[tokio::test]
async fn the_turn_after_the_ending_is_told_what_was_compacted_away() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();

    // `inject_context` ON — this test is about what the model is told, so the
    // preamble machinery has to be live. (The other tests turn it off to keep
    // their prompts free of noise.)
    let mut s = ThreadSettings::new(tmp.path().to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::default();
    let id = m.create_thread(None, s).unwrap();
    let seen = watch(&m);

    let cap = tmp.path().join("prompts.txt");
    let capture_arg = format!("promptfile={}", cap.display());

    m.send(
        &id,
        format!("compactturn please {capture_arg}"),
        Default::default(),
        json!({}),
        false,
    )
    .unwrap();
    assert!(
        eventually(|| saw(&seen, "compacted") && saw(&seen, "turn_ended")).await,
        "the first turn must compact and end: {:?}",
        kinds(&seen)
    );

    // He comes back and says the next thing himself.
    m.send(
        &id,
        format!("now do the next thing {capture_arg}"),
        Default::default(),
        json!({}),
        false,
    )
    .unwrap();
    assert!(
        eventually(|| captured(&cap).len() >= 2).await,
        "the second turn must actually reach the model, captured {:?}",
        captured(&cap).len()
    );

    let prompts = captured(&cap);
    let second = &prompts[1];
    assert!(
        second.contains("Context restatement"),
        "the turn after the ending must be told it has just been through a \
         compaction — that is the preamble he asked for, and without it the \
         ending is just silence. Second prompt:\n{second}"
    );
    assert!(
        second.contains(&id),
        "…and it carries the thread id:\n{second}"
    );
    assert!(
        second.contains(&*tmp.path().to_string_lossy()),
        "…and the working path:\n{second}"
    );
    assert!(
        second.contains("message ids"),
        "…and the range of message ids the compaction swallowed:\n{second}"
    );

    // The FIRST prompt has no restatement: nothing had been compacted yet.
    assert!(
        !prompts[0].contains("Context restatement"),
        "the first turn had nothing to restate:\n{}",
        prompts[0]
    );
}

// ── 7. THE FLAG IS SETTABLE AND READABLE WHERE SETTINGS ARE ─────────────────

/// **The flag defaults to false, survives a round trip, and is patchable
/// live.**
///
/// The default is asserted against a settings blob that does not mention the
/// field at all — which is every thread stored before this change, and every
/// caller that does not know the flag exists. Forced to fail by flipping the
/// expectation: with a `default_true`-style default the first assertion fails,
/// which is what "defaults to false" has to mean in practice.
#[tokio::test]
async fn the_flag_defaults_to_false_and_is_patchable() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path())).unwrap();

    // A blob written before the field existed — absent means the new default.
    let legacy = serde_json::json!({ "cwd": "/tmp", "model": "fable" });
    let parsed: ThreadSettings = serde_json::from_value(legacy).unwrap();
    assert!(
        !parsed.resume_after_compaction,
        "the NEW behaviour is the default; continuing is the opt-in"
    );
    assert!(
        !ThreadSettings::new("/tmp", "fable").resume_after_compaction,
        "and the constructor agrees with serde"
    );

    // It round-trips through the store…
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let row = m.store.get_thread(&id).unwrap().unwrap();
    let stored: ThreadSettings = serde_json::from_str(&row.settings_json).unwrap();
    assert!(!stored.resume_after_compaction);

    // …and it is VISIBLE on the wire, not merely present in the struct.
    //
    // `server::thread_json` hands `settings_json` back verbatim on
    // `threads.get` and `threads.list`, so the stored blob IS the wire shape.
    // The field carries no `skip_serializing_if`, deliberately: a flag that
    // governs whether a thread stops has to be readable as a fact rather than
    // inferred from an absence, and `false` is exactly the value a
    // skip-if-default attribute would have hidden.
    let wire: serde_json::Value = serde_json::from_str(&row.settings_json).unwrap();
    assert_eq!(
        wire.get("resume_after_compaction"),
        Some(&json!(false)),
        "the flag must be readable where a thread's settings are read, got {wire}"
    );

    // …and it is live-patchable over `threads.update`, like every other
    // per-thread flag. (`limit_kill_mid_turn` shipped promising this and not
    // doing it; the promise is worth a test.)
    let patch = serde_json::from_value(json!({ "resume_after_compaction": true })).unwrap();
    let (applied, _) = m.update_settings(&id, patch).unwrap();
    assert_eq!(
        applied.get("resume_after_compaction"),
        Some(&json!(true)),
        "the patch must be reported in the SettingsChanged audit payload, got {applied}"
    );
    let row = m.store.get_thread(&id).unwrap().unwrap();
    let stored: ThreadSettings = serde_json::from_str(&row.settings_json).unwrap();
    assert!(
        stored.resume_after_compaction,
        "the patch must actually persist"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
