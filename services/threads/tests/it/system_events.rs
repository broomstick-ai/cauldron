//! **DEC-55 / DEC-56 (SUPERSEDED) / DEC-57 / CON-22** — the audience split, the
//! interruption protocol, the queued-turn protocol, and the hard prohibition.
//!
//! The failure every test here answers is concrete and already happened: a
//! model resumed on a DIFFERENT account read an account-level usage-limit
//! error out of its own thread history and concluded it was blocked until
//! Sunday. It was wrong twice over — the account it was told about was not the
//! account it was running on, and the reset time was never any of its business.
//!
//! # DEC-56 WAS SUPERSEDED ON 2026-08-17, AND THESE TESTS MOVED WITH IT
//!
//! DEC-56 permitted the daemon to name the CAUSE of an interruption in the
//! model's own context — `Trigger: weekly_limit.` / `What fired: …` / `Moved to
//! another account: yes.` Several tests below used to ASSERT that those lines
//! were present. The operator withdrew the permission:
//!
//! > *"On decision 56, I remember what I was thinking at that time, and we have
//! > to supersede that. I no longer want any usage limits to be told to the
//! > agents at all… I am increasingly moving towards this belief that there's
//! > only exactly one way to solve a problem. And downgrading to a different
//! > agent is not it. So we shouldn't ever give the model the ability to see
//! > what its resource limit is."*
//!
//! Those tests were not deleted — a deleted test is a contract nobody is
//! checking. They were INVERTED: each now asserts the absence of exactly the
//! string it used to require, so the old behaviour cannot come back quietly.
//! The permitted set is now exhaustive: THAT it was interrupted, THAT it
//! resumed, WHEN, and HOW LONG AGO.
//!
//! **CON-22's other half is untouched.** A model is still never told it has no
//! usage limit, and never told when a limit resets; `guard_injection` and every
//! test of it stand exactly as they were.
//!
//! Every test drives fixtures only. No real claude turn is ever spawned, every
//! manager gets an explicit `data_dir` under a tempdir so `~/.threads` cannot
//! come into existence, and `THREADS_CLAUDE_HOME` is pointed at scratch so
//! nothing can read or write the operator's own login.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use serde_json::Value;
use threads::context::{
    assemble_model_notices, con22_violation, format_elapsed, guard_injection, render_model_notice,
    system_notice, NOTICE_KIND_QUEUED, NOTICE_KIND_RESUMED, NOTICE_TAG,
};
use threads::event::{
    audience_of_kind, Audience, InterruptTrigger, Push, StoredEvent, ThreadEvent,
    MODEL_AUDIENCE_KINDS,
};
use threads::settings::ThreadSettings;
use threads::store::ThreadInterruptRow;
use threads::{build_manager, Config, ThreadManager};

// ═════════════════════════════════════════════════════════════════════════
// 1. THE AUDIENCE SPLIT (DEC-55)
// ═════════════════════════════════════════════════════════════════════════

/// **Category (a) can never reach a model's context** — not by a filter
/// someone remembered to write, but by the event's own kind.
///
/// "Our application, like AUI, will want to just drop its own metadata into
/// the thread. But none of that should like land in the thread… the only thing
/// that should change is maybe like the event ID."
#[test]
fn a_category_a_application_event_never_reaches_model_context() {
    let app = ThreadEvent::Custom {
        event_type: "summary_updated".into(),
        data: serde_json::json!({"summary": "AUI's own bookkeeping"}),
    };
    assert_eq!(app.audience(), Audience::LedgerOnly);
    assert!(!app.audience().reaches_model());
    // There is no rendering of it at all — the split is enforced at the
    // renderer, not downstream of it.
    assert!(render_model_notice(&app).is_none());
    // And handing it straight to the assembler changes nothing.
    assert!(assemble_model_notices(&[app], Utc::now()).is_none());
}

/// **The per-kind replay audit, pinned.** Every kind the taxonomy can emit is
/// walked; `audience()` must agree with `MODEL_AUDIENCE_KINDS` exactly, and
/// the default for anything not deliberately promoted is `LedgerOnly`.
///
/// `rate_limit_event` is called out by name because it is the leak channel:
/// an inventory of the coordinator's own live thread found it **38 times in
/// one conversation**.
#[test]
fn the_per_kind_replay_audit_is_pinned() {
    for ev in every_event_variant() {
        let kind = ev.kind();
        let expected = if MODEL_AUDIENCE_KINDS.contains(&kind) {
            Audience::Model
        } else {
            Audience::LedgerOnly
        };
        assert_eq!(
            ev.audience(),
            expected,
            "kind {kind:?} disagrees with MODEL_AUDIENCE_KINDS — decide it deliberately"
        );
        // The kind-string table and the variant table are the same table.
        assert_eq!(audience_of_kind(kind), ev.audience(), "kind {kind:?}");
    }

    // Exactly three kinds are replayable, and they are threads' OWN events.
    assert_eq!(
        MODEL_AUDIENCE_KINDS,
        &[
            "thread_system_interrupted",
            "thread_system_queued",
            "thread_system_resumed"
        ]
    );
    // The named offenders, spelled out so a future edit has to argue with them.
    for leaky in [
        "rate_limit_event",
        "limit_hit",
        "retry_scheduled",
        "budget_tripped",
        "turn_parked",
        "turn_resumed",
        "turn_killed",
        "account_switched",
        "custom",
        "system_init",
        "message_usage",
        "turn_started",
        "result_turn",
    ] {
        assert_eq!(
            audience_of_kind(leaky),
            Audience::LedgerOnly,
            "{leaky} must never be replayed into model context"
        );
    }
    // An unaudited kind is not a kind a model may read.
    assert_eq!(
        audience_of_kind("some_future_kind_nobody_has_audited"),
        Audience::LedgerOnly
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 2. CON-22 — THE HARD PROHIBITION
// ═════════════════════════════════════════════════════════════════════════

/// **A reset timestamp never reaches context.** This is the one that actually
/// leaked.
#[test]
fn a_reset_timestamp_never_reaches_context() {
    let now = Utc::now();
    let future = (now + ChronoDuration::days(3)).to_rfc3339();
    let text = format!(
        "You were interrupted.\nYour weekly limit resets at {future}.\nElapsed: 2 minutes."
    );
    let guarded = guard_injection(&text, now);
    assert!(!guarded.contains("resets at"));
    assert!(!guarded.contains(&future));
    assert!(guarded.contains("You were interrupted."));
    assert!(guarded.contains("Elapsed: 2 minutes."));
    assert!(con22_violation(&guarded, now).is_none());
}

/// The STRUCTURAL half of the rule, which is what closes kinds nobody has
/// written yet: a FUTURE instant is refused whatever prose surrounds it.
/// "The line is drawn between history and prediction."
#[test]
fn a_future_instant_is_refused_even_with_no_forbidden_phrase() {
    let now = Utc::now();
    let future = (now + ChronoDuration::hours(6)).to_rfc3339();
    let line = format!("Capacity window: {future}");
    assert!(guard_injection(&line, now).is_empty());
    assert!(con22_violation(&line, now).is_some());
}

/// …and HISTORY survives untouched, because DEC-56 REQUIRES telling the model
/// when it was interrupted.
#[test]
fn past_instants_survive_the_guard() {
    let now = Utc::now();
    let past = (now - ChronoDuration::days(4)).to_rfc3339();
    let line = format!("Interrupted at: {past}.");
    assert_eq!(guard_injection(&line, now), line);
    assert!(con22_violation(&line, now).is_none());
}

/// Both directions are forbidden. "A model told it is unlimited will spend
/// accordingly."
#[test]
fn the_model_is_never_told_it_has_no_limit() {
    let now = Utc::now();
    for claim in [
        "You have no usage limit on this account.",
        "This thread runs with unlimited capacity.",
        "Your quota remaining is 4,000,000 tokens.",
        "The account is available again at 9pm.",
    ] {
        assert!(
            guard_injection(claim, now).is_empty(),
            "not refused: {claim}"
        );
        assert!(con22_violation(claim, now).is_some(), "not flagged: {claim}");
    }
}

/// The guard runs on the ASSEMBLED block, so a violation smuggled in through
/// an event field is dropped before the block is ever returned.
#[test]
fn the_assembler_refuses_a_violation_carried_on_an_event_field() {
    let now = Utc::now();
    let reset = (now + ChronoDuration::days(2)).to_rfc3339();
    let ev = ThreadEvent::ThreadSystemResumed {
        trigger: "weekly_limit".into(),
        detail: format!("the weekly usage limit was hit; resets at {reset}"),
        interrupted_at: (now - ChronoDuration::hours(2)).to_rfc3339(),
        resumed_at: now.to_rfc3339(),
        elapsed_seconds: 7_200,
        elapsed: format_elapsed(7_200),
        switched: false,
        from_account: Some("default".into()),
        to_account: None,
    };
    let block = assemble_model_notices(&[ev], now).expect("a resume notice is owed");
    // WAS: `assert!(block.contains("weekly_limit"), "the trigger survives")`.
    // The DEC-56 supersession inverted it — the trigger is a ledger field now
    // and the renderer never emits it.
    assert!(
        !block.contains("weekly_limit"),
        "the TRIGGER must not survive into context: {block}"
    );
    assert!(
        !block.contains("the weekly usage limit was hit"),
        "…and neither must the detail carrying it: {block}"
    );
    assert!(block.contains("2 hours"), "the elapsed figure survives");
    assert!(!block.contains("resets at"));
    assert!(!block.contains(&reset));
    assert!(con22_violation(&block, now).is_none());
}

// ═════════════════════════════════════════════════════════════════════════
// 3. THE ENVELOPE (Q-38, exercised)
// ═════════════════════════════════════════════════════════════════════════

/// The tag is CONTRACT, and it is deliberately not the host harness's own.
#[test]
fn the_envelope_tag_is_pinned_and_cannot_be_confused_with_the_host_harness() {
    assert_eq!(NOTICE_TAG, "thread_system_notice");
    assert_eq!(NOTICE_KIND_RESUMED, "resumed_after_interrupt");
    assert_eq!(NOTICE_KIND_QUEUED, "queued_turn");

    let rendered = system_notice(NOTICE_KIND_RESUMED, "one short factual block");
    assert!(rendered.starts_with(
        "<thread_system_notice source=\"threads\" kind=\"resumed_after_interrupt\">"
    ));
    assert!(rendered.ends_with("</thread_system_notice>"));
    assert!(rendered.contains("  one short factual block"));
    // Claude Code injects `<system-reminder>` into the same context. A model
    // must never have to guess which of the two is speaking.
    assert!(!rendered.contains("system-reminder"));
    assert!(!rendered.contains("<system>"));
}

// ═════════════════════════════════════════════════════════════════════════
// 4. THE RESUME NOTICE (DEC-56)
// ═════════════════════════════════════════════════════════════════════════

/// "It resuming after one second is gonna be different than it resuming after
/// four days. So that's why we need to tell it the duration." Days, hours,
/// minutes AND seconds — computed by the system and handed over computed.
#[test]
fn elapsed_is_rendered_in_days_hours_minutes_and_seconds() {
    assert_eq!(format_elapsed(0), "0 seconds");
    assert_eq!(format_elapsed(1), "1 second");
    assert_eq!(format_elapsed(90), "1 minute, 30 seconds");
    assert_eq!(
        format_elapsed(4 * 86_400 + 3 * 3_600 + 2 * 60 + 1),
        "4 days, 3 hours, 2 minutes, 1 second"
    );
}

/// **The notice carries WHEN and ELAPSED — and NOT why.**
///
/// This test used to be `the_resume_notice_carries_why_when_and_elapsed` and
/// asserted `block.contains("session_limit")`, `block.contains("the 40% account
/// threshold for the session limit")` and `block.contains("Moved to another
/// account: yes.")`. All three are now asserted ABSENT (DEC-56 superseded,
/// 2026-08-17). Dropping the elapsed figure is still the failure the second
/// half exists for.
///
/// `Moved to another account: yes.` names no account and is still forbidden:
/// it tells a model its account was exhausted, which is the inference the
/// supersession closes.
#[test]
fn the_resume_notice_carries_when_and_elapsed_but_never_why() {
    let now = Utc::now();
    let interrupted = now - ChronoDuration::seconds(4 * 86_400 + 3_600);
    let ev = ThreadEvent::ThreadSystemResumed {
        trigger: "session_limit".into(),
        detail: "the 40% account threshold for the session limit was crossed".into(),
        interrupted_at: interrupted.to_rfc3339(),
        resumed_at: now.to_rfc3339(),
        elapsed_seconds: 4 * 86_400 + 3_600,
        elapsed: format_elapsed(4 * 86_400 + 3_600),
        switched: true,
        from_account: Some("default".into()),
        to_account: Some("zz-second".into()),
    };
    let block = assemble_model_notices(&[ev], now).unwrap();
    assert!(block.contains("kind=\"resumed_after_interrupt\""));

    // ── WHAT IS OWED ─────────────────────────────────────────────────────
    assert!(
        block.contains("You were interrupted and have now been resumed."),
        "THAT it happened"
    );
    assert!(block.contains("Interrupted at:"), "WHEN");
    assert!(
        block.contains("4 days, 1 hour, 0 minutes, 0 seconds"),
        "ELAPSED, already computed: {block}"
    );

    // ── WHAT IS FORBIDDEN (each line was asserted PRESENT before) ────────
    assert!(!block.contains("session_limit"), "the trigger: {block}");
    assert!(
        !block.contains("the 40% account threshold for the session limit"),
        "the detail: {block}"
    );
    assert!(!block.contains("What fired"), "the detail's label: {block}");
    assert!(!block.contains("Why:"), "the trigger's label: {block}");
    assert!(!block.contains("Trigger:"), "the trigger's other label: {block}");
    assert!(
        !block.contains("Moved to another account"),
        "the switchover flag — it names no account and still says 'exhausted': {block}"
    );
    assert!(!block.contains("zz-second"), "the account itself: {block}");
    assert!(!block.contains("default"), "…or the one it came from: {block}");
}

/// The interrupt notice obeys the same rule as the resume notice: WHEN, and
/// nothing about the cause.
#[test]
fn the_interrupt_notice_names_no_cause_either() {
    let now = Utc::now();
    let at = now - ChronoDuration::minutes(5);
    let ev = ThreadEvent::ThreadSystemInterrupted {
        trigger: "weekly_limit".into(),
        switched: true,
        detail: "the weekly usage limit was reached on the account this thread was running on"
            .into(),
        at: at.to_rfc3339(),
        from_account: Some("default".into()),
        to_account: Some("zz-second".into()),
    };
    let (_kind, body) = render_model_notice(&ev).expect("an interrupt is rendered");
    assert!(body.contains("This thread was interrupted at"), "WHAT and WHEN");
    for forbidden in [
        "weekly_limit",
        "Trigger:",
        "usage limit",
        "What fired",
        "Moved to another account",
        "zz-second",
    ] {
        assert!(!body.contains(forbidden), "{forbidden:?} leaked: {body}");
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 5. THE QUEUED-TURN PROTOCOL (DEC-57)
// ═════════════════════════════════════════════════════════════════════════

/// "The flag for it being interrupted would be set to false, because it wasn't
/// interrupted, it never started."
#[test]
fn a_queued_notice_reports_its_wait_with_interrupted_false() {
    let now = Utc::now();
    let submitted = now - ChronoDuration::minutes(41);
    let ev = ThreadEvent::ThreadSystemQueued {
        submitted_at: submitted.to_rfc3339(),
        queued_until: now.to_rfc3339(),
        queued_for_seconds: 41 * 60,
        queued_for: format_elapsed(41 * 60),
        interrupted: false,
        message_ids: vec![7, 8],
    };
    let block = assemble_model_notices(&[ev], now).unwrap();
    assert!(block.contains("kind=\"queued_turn\""));
    assert!(block.contains("Submitted at:"));
    assert!(block.contains("Queued until:"));
    assert!(block.contains("Queued for: 41 minutes, 0 seconds"));
    assert!(
        block.contains("Interrupted: no."),
        "DEC-57: nothing was interrupted — it never started"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 6. THE AMENDMENT (DEC-56, msg #1412) — HAND BACK, DO NOT RE-PLACE
// ═════════════════════════════════════════════════════════════════════════

/// **"You don't directly attempt to put it in another account. You put it back
/// to the balancer."**
///
/// A source guard, in the shape of the pinned WS-method inventory: it reads
/// `interrupt_thread`'s body out of `runtime.rs` and fails on sight of any
/// account-selection identifier. "A reviewer should treat any new
/// account-selection code under an interrupt branch as a defect on sight" —
/// this is that reviewer, running on every build.
#[test]
fn the_interrupt_path_contains_no_placement_logic() {
    let src = include_str!("../../src/runtime.rs");
    let start = src
        .find("    fn interrupt_thread(")
        .expect("interrupt_thread was renamed — re-point this test");
    let body = &src[start..];
    let end = body
        .find("fn classify_limit_trigger(")
        .expect("the item after interrupt_thread moved — re-point this test");
    let body = &body[..end];

    for banned in [
        "resolve_account",
        "selectable_pool",
        "account_pool",
        "switch_account",
        "move_thread",
        "failover",
        "PlacementRequest",
        "AdmissionRequest",
        "admit_or_park",
        "self.budgets",
        "self.registry",
        ".place(",
        ".admit(",
    ] {
        assert!(
            !body.contains(banned),
            "the interrupt path grew placement logic ({banned:?}). DEC-56 as amended: \
             flag the thread, emit the event, hand it back. If placement must differ on \
             an interrupt, that is an INPUT to Budgets::place / Budgets::admit."
        );
    }
    // …and it does the three things it IS allowed to do.
    assert!(body.contains("set_thread_interrupt"), "the flag");
    assert!(body.contains("interrupted_at"), "and its timestamp");
    assert!(body.contains("ThreadSystemInterrupted"), "the category-(b) event");
}

// ═════════════════════════════════════════════════════════════════════════
// 7. END-TO-END, AGAINST THE FAKE CLAUDE FIXTURE
// ═════════════════════════════════════════════════════════════════════════

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
    // NEVER the operator's own `~/.claude`.
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn manager_at(dir: &std::path::Path) -> ThreadManager {
    // CON-27: `default` is the operator's own terminal login — registered on
    // every boot by `ensure_default`, and never somewhere threads may PLACE a
    // thread, because it has no config dir of its own. A fixture that registers
    // nothing else has no placeable account at all and `create_thread` refuses.
    // So these end-to-end threads need one login threads OWNS. Exactly one, on
    // purpose: the hard-limit test below turns on there being nowhere to fail
    // over to. `aa-owned` sorts first, so it ranks where `default` used to.
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        dir.join("aa-owned").to_string_lossy().to_string(),
    );
    build_manager(Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap()
}

/// A thread that DOES take context injection — the notices are the point.
fn injecting(cwd: &std::path::Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::default();
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

fn first_of(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> Option<ThreadEvent> {
    seen.lock()
        .unwrap()
        .iter()
        .find(|e| e.event.kind() == kind)
        .map(|e| e.event.clone())
}

fn has_kind(seen: &Arc<Mutex<Vec<StoredEvent>>>, kind: &str) -> bool {
    seen.lock().unwrap().iter().any(|e| e.event.kind() == kind)
}

fn injected_texts(seen: &Arc<Mutex<Vec<StoredEvent>>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            ThreadEvent::ContextInjected { text } => Some(text.clone()),
            _ => None,
        })
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

/// **A hard limit break flags the thread and HANDS IT BACK.**
///
/// The fixture's `ratelimit` mode emits the real 429 shape, prose and all
/// ("You have hit your session limit resets 2:40am"). What must come out of it:
/// a `thread_system_interrupted` naming the gate, an interrupted flag with its
/// timestamp, NO account switch invented by the interrupt path, and the
/// EXISTING retry path continuing to own run-now-versus-wait.
#[tokio::test]
async fn a_hard_limit_break_flags_the_thread_and_hands_it_back() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = manager_at(tmp.path());
    let seen = watch(&m);
    let id = m.create_thread(None, injecting(tmp.path())).unwrap();
    m.send(&id, "ratelimit please".into(), Default::default(), Value::Null, false)
        .unwrap();

    assert!(
        eventually(|| has_kind(&seen, "thread_system_interrupted")).await,
        "a hard break owes the thread a category-(b) event"
    );
    let ev = first_of(&seen, "thread_system_interrupted").unwrap();
    let ThreadEvent::ThreadSystemInterrupted {
        trigger,
        switched,
        detail,
        at,
        ..
    } = ev
    else {
        panic!("wrong variant")
    };
    // The LEDGER still names the gate — that is the operator's record, and the
    // supersession moved the model's copy, not his. See
    // `context::render_model_notice`, and the end of this test for the proof
    // that none of it reached the model.
    assert_eq!(trigger, "session_limit", "the gate is NAMED on the ledger");
    assert!(!switched, "one registered login: no switchover happened");
    assert!(detail.contains("session usage limit"));
    assert!(chrono::DateTime::parse_from_rfc3339(&at).is_ok(), "…and WHEN");

    // The flag is DURABLE — a daemon restart still owes this thread its notice.
    let row = m.store.get_thread_interrupt(&id).unwrap().expect("flagged");
    assert_eq!(row.trigger, "session_limit");
    assert_eq!(row.interrupted_at, at);

    // THE AMENDMENT: the interrupt invented no placement. With one login there
    // was nowhere to go, and the interrupt path did not pretend otherwise.
    assert!(
        !has_kind(&seen, "account_switched"),
        "the interrupt path must not place the thread"
    );
    // …and the EXISTING path kept ownership of run-now-versus-wait.
    assert!(
        eventually(|| has_kind(&seen, "retry_scheduled")).await,
        "the thread is handed back, not dropped"
    );

    // CON-22, end to end: the fixture's prose carries a reset time. Nothing the
    // daemon injected may contain it.
    //
    // This was `!text.contains("2:40am")` against the clock time the fixture
    // used to hard-code. The fixture now COMPUTES its reset two hours ahead of
    // `now` (a fixed one resolves to no window once it has gone by, so it made
    // every 429 test clock-dependent), and against a computed reset that
    // literal would be vacuously green forever — a scrub assertion that cannot
    // fail is not a scrub assertion.
    //
    // So the subject is read back off the row the daemon ITSELF wrote, which is
    // the prose the daemon actually saw, and it is asserted PRESENT there
    // before it is asserted absent from the injections. If the recording path
    // breaks, this says so instead of going quiet.
    let prose = eventually(|| {
        m.store
            .list_account_states()
            .unwrap()
            .into_iter()
            .any(|s| s.name == "aa-owned" && s.last_limit_prose.is_some())
    })
    .await
    .then(|| {
        m.store
            .list_account_states()
            .unwrap()
            .into_iter()
            .find(|s| s.name == "aa-owned")
            .and_then(|s| s.last_limit_prose)
            .unwrap()
    })
    .expect("the 429's reset prose is recorded against the login that refused");
    assert!(
        prose.ends_with("(America/Chicago)"),
        "the recorded prose is the fixture's own reset descriptor: {prose}"
    );
    let clock = prose
        .split_whitespace()
        .next()
        .expect("the descriptor leads with the clock time")
        .to_string();
    for text in injected_texts(&seen) {
        assert!(
            con22_violation(&text, Utc::now()).is_none(),
            "injected context leaked future capacity: {text}"
        );
        assert!(
            !text.contains(&prose),
            "the reset descriptor the daemon recorded leaked into model context: {text}"
        );
        assert!(
            !text.contains(&clock),
            "the reset clock time {clock} leaked into model context: {text}"
        );
        // DEC-56 superseded: and nothing it injected may name the CAUSE
        // either, even though the ledger row asserted above names it.
        assert!(!text.contains("session_limit"), "the trigger leaked: {text}");
        assert!(!text.contains("usage limit"), "the detail leaked: {text}");
    }
}

/// **The automatic resume injects WHEN and ELAPSED — and no trigger** — inside
/// the envelope, on the next user message, with the reset prose scrubbed.
///
/// Was `an_automatic_resume_injects_why_when_and_elapsed`, whose central
/// assertion was `injected.contains("weekly_limit")`. That assertion is now
/// inverted (DEC-56 superseded, 2026-08-17): the interrupt row still CARRIES
/// `weekly_limit` — it is set explicitly below and read back off the ledger —
/// and the injected text must not contain it. That is the whole point of
/// driving this end to end rather than unit-testing the renderer: the value is
/// provably present upstream of the thing that must not emit it.
#[tokio::test]
async fn an_automatic_resume_injects_when_and_elapsed_but_names_no_trigger() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = manager_at(tmp.path());
    let seen = watch(&m);
    let id = m.create_thread(None, injecting(tmp.path())).unwrap();

    // A thread that was cut off four days ago — and whose recorded detail
    // carries exactly the prose that leaked, so the guard has something to do.
    let four_days_ago = Utc::now() - ChronoDuration::seconds(4 * 86_400 + 7_265);
    let reset = (Utc::now() + ChronoDuration::days(2)).to_rfc3339();
    m.store
        .set_thread_interrupt(&ThreadInterruptRow {
            thread_id: id.clone(),
            trigger: "weekly_limit".into(),
            detail: format!("the weekly usage limit was reached; resets at {reset}"),
            switched: false,
            from_account: Some("default".into()),
            to_account: None,
            interrupted_at: four_days_ago.to_rfc3339(),
        })
        .unwrap();

    m.send(&id, "carry on".into(), Default::default(), Value::Null, false)
        .unwrap();
    assert!(eventually(|| has_kind(&seen, "context_injected")).await);

    let injected = injected_texts(&seen).join("\n");
    assert!(
        injected.contains("<thread_system_notice source=\"threads\" kind=\"resumed_after_interrupt\">"),
        "the envelope: {injected}"
    );
    assert!(injected.contains("Interrupted at:"), "WHEN");
    assert!(
        injected.contains("4 days, 2 hours, 1 minute, 5 seconds"),
        "ELAPSED, computed by the system: {injected}"
    );
    // DEC-56 SUPERSEDED. Every one of these is on the interrupt row set above,
    // and none of them may be in the text.
    assert!(!injected.contains("weekly_limit"), "the trigger: {injected}");
    assert!(!injected.contains("usage limit"), "the detail: {injected}");
    assert!(!injected.contains("Why:"), "{injected}");
    assert!(!injected.contains("What fired"), "{injected}");
    assert!(!injected.contains("Moved to another account"), "{injected}");
    // CON-22, unweakened: the reset instant and its phrase are gone too.
    assert!(!injected.contains(&reset));
    assert!(!injected.contains("resets at"));
    assert!(con22_violation(&injected, Utc::now()).is_none());
    // The ledger kept what the model was not told.
    let row_trigger = first_of(&seen, "thread_system_resumed").and_then(|e| match e {
        ThreadEvent::ThreadSystemResumed { trigger, .. } => Some(trigger),
        _ => None,
    });
    assert_eq!(
        row_trigger.as_deref(),
        Some("weekly_limit"),
        "the RECORD still names the gate; only the model's copy does not"
    );

    // The event is on the record, and the flag is cleared once — a thread is
    // told about an interruption exactly one time.
    assert!(has_kind(&seen, "thread_system_resumed"));
    assert!(m.store.get_thread_interrupt(&id).unwrap().is_none());
}

/// **A queued turn reports its wait, with the interrupted flag FALSE.**
///
/// The second message queues behind a `slowturn`; its queue row is backdated
/// while it waits, which is the same thing a real budget park does over
/// forty minutes without making the test wait forty minutes.
#[tokio::test]
async fn a_queued_turn_reports_its_wait_and_is_not_interrupted() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = manager_at(tmp.path());
    let seen = watch(&m);
    let id = m.create_thread(None, injecting(tmp.path())).unwrap();

    m.send(&id, "slowturn first".into(), Default::default(), Value::Null, false)
        .unwrap();
    assert!(eventually(|| has_kind(&seen, "turn_started")).await);
    m.send(&id, "second thing".into(), Default::default(), Value::Null, false)
        .unwrap();
    let submitted = Utc::now() - ChronoDuration::minutes(41);
    m.store
        .execute_sql(&format!(
            "UPDATE queue SET created_at = '{}' WHERE state = 'waiting'",
            submitted.to_rfc3339()
        ))
        .unwrap();

    assert!(
        eventually(|| has_kind(&seen, "thread_system_queued")).await,
        "a gap between intent and delivery is owed a report"
    );
    let ThreadEvent::ThreadSystemQueued {
        interrupted,
        queued_for_seconds,
        queued_for,
        ..
    } = first_of(&seen, "thread_system_queued").unwrap()
    else {
        panic!("wrong variant")
    };
    assert!(!interrupted, "DEC-57: it was never interrupted, it never started");
    assert!(queued_for_seconds >= 41 * 60);
    assert!(queued_for.contains("41 minutes"), "{queued_for}");

    // …and nothing set the interrupted flag, which is the distinction the
    // whole decision exists to protect.
    assert!(m.store.get_thread_interrupt(&id).unwrap().is_none());

    let injected = injected_texts(&seen).join("\n");
    assert!(injected.contains("kind=\"queued_turn\""), "{injected}");
    assert!(injected.contains("Interrupted: no."));
}

// ═════════════════════════════════════════════════════════════════════════
// 8. THE OPERATOR'S OWN INTERRUPT (`threads.cancel`)
// ═════════════════════════════════════════════════════════════════════════

/// **A cancelled turn SAYS it was cancelled, and says who asked.**
///
/// Before this, a `threads.cancel` produced a bare `turn_ended` with no result
/// and no verdict behind it, so the only thing a client could do was wait out
/// its no-result grace window — 90 seconds in AUI — and then guess. The
/// operator pressed a button and heard nothing for a minute and a half.
///
/// The fixture's `holdturn` mode gives a turn that is DEFINITELY still running
/// (it waits on a release file that this test never creates), so the cancel has
/// something real to tear down.
#[tokio::test]
async fn an_operator_cancel_records_an_interrupt_naming_the_operator() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = manager_at(tmp.path());
    let seen = watch(&m);
    let id = m.create_thread(None, injecting(tmp.path())).unwrap();
    let release = tmp.path().join("never-released");
    m.send(
        &id,
        format!("holdturn release={}", release.display()),
        Default::default(),
        Value::Null,
        false,
    )
    .unwrap();

    assert!(
        eventually(|| has_kind(&seen, "turn_started")).await,
        "the turn has to be running before there is anything to interrupt"
    );
    m.cancel(&id).unwrap();

    assert!(
        eventually(|| has_kind(&seen, "turn_interrupted")).await,
        "a cancelled turn owes the ledger an interrupt event, not a silent \
         turn_ended the client has to time out on"
    );
    let ThreadEvent::TurnInterrupted { reason, trigger } =
        first_of(&seen, "turn_interrupted").unwrap()
    else {
        panic!("wrong variant")
    };
    assert_eq!(
        trigger, "operator",
        "an operator-requested cancel must be distinguishable from a \
         daemon-restart interrupt — that distinction is the whole feature"
    );
    assert!(reason.contains("operator"), "{reason}");
    assert!(
        InterruptTrigger::parse(&trigger).is_some(),
        "the trigger is a value of the ONE interrupt vocabulary, not a second one"
    );
    // It is a cancel, not a failure: nothing may report this as an error.
    assert!(!has_kind(&seen, "turn_failed"), "a cancel is not a failure");
    // …and the turn is genuinely over.
    assert!(eventually(|| has_kind(&seen, "turn_ended")).await);
}

/// **A pre-field ledger row still parses, and reads as NOT the operator.**
///
/// `trigger` is `serde(default)`, so every `turn_interrupted` written before it
/// existed comes back with an empty trigger. All of those are daemon-restart
/// interrupts, and the rule every consumer applies — operator, or not the
/// operator — has to give the right answer for them without any migration.
#[test]
fn an_old_turn_interrupted_row_parses_and_is_not_an_operator_cancel() {
    let old: ThreadEvent =
        serde_json::from_str(r#"{"kind":"turn_interrupted","reason":"daemon restarted"}"#)
            .expect("a row written before `trigger` existed must still parse");
    let ThreadEvent::TurnInterrupted { trigger, .. } = &old else {
        panic!("wrong variant")
    };
    assert_eq!(trigger, "", "an absent trigger defaults to empty");
    assert_ne!(trigger, "operator", "…and is therefore never read as a cancel");
    // It is still ledger-only: an interrupt is mechanics, and a model never
    // sees this kind whichever trigger it carries.
    assert_eq!(old.audience(), Audience::LedgerOnly);
}

/// **The re-adopted orphan RECORDS the cancellation, it does not merely act on
/// it — the asymmetry that motivated this feature.**
///
/// `adopt_turn` read `cancel_requested`, SIGTERM'd the process group, and said
/// nothing; `run_once` returned a `cancelled` outcome its caller could see. So
/// the identical operator action produced two different ledgers depending on
/// whether the daemon happened to have restarted since the turn began.
///
/// Source-scanned rather than driven: reaching `adopt_turn` needs a daemon to
/// die mid-turn and a second one to boot onto the surviving pid, and the
/// behaviour under test is an emit behind a turn lock with no return value to
/// assert on.
#[test]
fn both_cancel_paths_emit_the_same_interrupt() {
    let src = include_str!("../../src/runtime.rs");

    let adopt = src
        .find("    async fn adopt_turn(")
        .expect("adopt_turn was renamed — re-point this test");
    let adopt_body = &src[adopt..];
    let adopt_end = adopt_body
        .find("/// In-stream signals captured during one spawn")
        .expect("the item after adopt_turn moved — re-point this test");
    let adopt_body = &adopt_body[..adopt_end];

    assert!(
        adopt_body.contains("ThreadEvent::TurnInterrupted"),
        "a re-adopted orphan that the operator cancels must RECORD it, not just \
         signal the process group — that asymmetry with run_once is the original bug"
    );
    assert!(
        adopt_body.contains("InterruptTrigger::Operator"),
        "…and it must carry the same trigger run_once carries, so a client \
         cannot tell which loop tore the turn down"
    );

    let run_once = src
        .find("    async fn run_once(")
        .expect("run_once was renamed — re-point this test");
    let run_once_body = &src[run_once..];
    let run_once_end = run_once_body
        .find("    /// Parse one raw stream line into events")
        .expect("the item after run_once moved — re-point this test");
    // Anchored on `if cancelled &&` since 2026-08-25: the arm now consults
    // `retry::ends_the_delivery(InterruptTrigger::Operator)` rather than
    // short-circuiting on the bare flag, so that the rule "a user interruption
    // ends the delivery, nothing else does" is load-bearing at the one site that
    // enforces it instead of being stated in a function nothing calls. What this
    // test pins — that the arm RECORDS the interrupt and names the operator — is
    // unchanged.
    let cancelled_arm = run_once_body[..run_once_end]
        .split("if cancelled &&")
        .nth(1)
        .expect("run_once's cancelled arm moved — re-point this test");
    assert!(
        cancelled_arm.contains("ThreadEvent::TurnInterrupted"),
        "run_once's cancelled outcome must emit the interrupt"
    );
    assert!(
        cancelled_arm.contains("InterruptTrigger::Operator"),
        "…naming the operator"
    );
}

// ── one of every variant, for the audit ──────────────────────────────────

fn every_event_variant() -> Vec<ThreadEvent> {
    use ThreadEvent as E;
    let v = Value::Null;
    vec![
        E::ThreadStarted {
            thread_id: "t".into(),
            account: "a".into(),
            cwd: "/".into(),
            model: "m".into(),
        },
        E::UserMessage {
            text: "hi".into(),
            intent: Default::default(),
            metadata: v.clone(),
        },
        E::TurnStarted {
            attempt: 1,
            message_ids: vec![],
        },
        E::TurnEnded { exit_code: None },
        E::TurnInterrupted {
            reason: "r".into(),
            trigger: "operator".into(),
        },
        E::TurnFailed {
            error_kind: "k".into(),
            message: "m".into(),
        },
        E::SystemInit {
            claude_session_id: None,
            cwd: None,
            model: None,
            tools: vec![],
            mcp_servers: v.clone(),
            permission_mode: None,
            slash_commands: vec![],
            api_key_source: None,
            agents: v.clone(),
        },
        E::Compacted {
            trigger: "t".into(),
            pre_tokens: None,
            message_id_range: None,
        },
        E::ContextInjected { text: "t".into() },
        E::AssistantText {
            text: "t".into(),
            parent_tool_use_id: None,
            api_message_id: None,
        },
        E::Thinking {
            text: "t".into(),
            parent_tool_use_id: None,
        },
        E::ToolUse {
            id: "i".into(),
            name: "n".into(),
            input: v.clone(),
            parent_tool_use_id: None,
        },
        E::ToolResult {
            tool_use_id: "i".into(),
            content: v.clone(),
            is_error: false,
            parent_tool_use_id: None,
        },
        E::MessageUsage {
            model: "m".into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            api_message_id: None,
        },
        E::AgentSpawn {
            tool_use_id: "i".into(),
            subagent_type: None,
            description: None,
            run_in_background: false,
            parent_tool_use_id: None,
        },
        E::AgentLaunched {
            tool_use_id: "i".into(),
            agent_id: "a".into(),
            is_async: false,
            status: "s".into(),
            output_file: None,
        },
        E::AgentCompleted { agent_id: "a".into() },
        E::AgentListingDelta { raw: v.clone() },
        // The `system/task_*` lifecycle. All five are LedgerOnly: they are the
        // CLI narrating work the model itself launched and is already told
        // about in-band, and `task_notification` carries whole agent reports
        // that a replay would duplicate into context.
        E::TaskStarted {
            task_id: "t".into(),
            tool_use_id: None,
            description: None,
            subagent_type: None,
            task_type: None,
            prompt: None,
            session_id: None,
        },
        E::TaskUpdated {
            task_id: "t".into(),
            status: None,
            end_time: None,
            error: None,
            is_backgrounded: None,
            patch: v.clone(),
            session_id: None,
        },
        E::TaskNotification {
            task_id: "t".into(),
            tool_use_id: None,
            status: None,
            output_file: None,
            summary: None,
            total_tokens: None,
            tool_uses: None,
            duration_ms: None,
            session_id: None,
        },
        E::TaskProgress {
            task_id: "t".into(),
            tool_use_id: None,
            description: None,
            subagent_type: None,
            last_tool_name: None,
            total_tokens: None,
            tool_uses: None,
            duration_ms: None,
            session_id: None,
        },
        E::BackgroundTasksChanged {
            tasks: vec![],
            session_id: None,
        },
        E::StreamEvent { raw: v.clone() },
        E::Unknown { raw: v.clone() },
        E::ResultTurn {
            subtype: "s".into(),
            is_error: false,
            total_cost_usd: None,
            num_turns: None,
            duration_ms: None,
            stop_reason: None,
            model_usage: v.clone(),
            structured_output: None,
        },
        E::LimitHit {
            pool: None,
            reset: None,
            status: None,
            text: String::new(),
        },
        E::RateLimitEvent {
            rate_limit_type: "seven_day".into(),
            utilization: 0.73,
            resets_at: Some(1_785_607_200),
            resets_at_rfc3339: None,
            status: None,
            is_using_overage: false,
        },
        E::BudgetTripped {
            budget_key: "k".into(),
            axis: "session".into(),
            consumed_pct: 1.0,
            limit_pct: 1.0,
            action: "kill".into(),
        },
        E::TurnParked {
            cause: "c".into(),
            reason: "r".into(),
            budget_key: None,
            message_ids: vec![],
        },
        E::BudgetsFailed {
            phase: "p".into(),
            error: "e".into(),
            outcome: "o".into(),
            budget_key: None,
        },
        E::TurnResumed {
            reason: "r".into(),
            budget_key: None,
            message_ids: vec![],
        },
        E::TurnKilled {
            reason: "r".into(),
            budget_key: None,
            strategy: "s".into(),
        },
        E::RetryScheduled {
            error_kind: "k".into(),
            reason: "r".into(),
            attempt: 1,
            retry_at: "2026-08-01T00:00:00Z".into(),
            reset_prose: None,
        },
        E::AccountSwitched {
            from: "a".into(),
            to: "b".into(),
            reason: "r".into(),
        },
        E::SettingsChanged { patch: v.clone() },
        E::PluginsChanged {
            added: vec![],
            removed: vec![],
            modified: vec![],
        },
        E::ThreadSystemInterrupted {
            trigger: "session_limit".into(),
            switched: false,
            detail: String::new(),
            at: "2026-08-01T00:00:00Z".into(),
            from_account: None,
            to_account: None,
        },
        E::ThreadSystemResumed {
            trigger: "session_limit".into(),
            detail: String::new(),
            interrupted_at: "2026-08-01T00:00:00Z".into(),
            resumed_at: "2026-08-01T00:00:01Z".into(),
            elapsed_seconds: 1,
            elapsed: "1 second".into(),
            switched: false,
            from_account: None,
            to_account: None,
        },
        E::ThreadSystemQueued {
            submitted_at: "2026-08-01T00:00:00Z".into(),
            queued_until: "2026-08-01T00:00:01Z".into(),
            queued_for_seconds: 1,
            queued_for: "1 second".into(),
            interrupted: false,
            message_ids: vec![],
        },
        E::Custom {
            event_type: "summary_updated".into(),
            data: v,
        },
    ]
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
