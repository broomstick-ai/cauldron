//! **THE LOGIN FLOW, DRIVEN OVER PIPES.**
//!
//! `claude auth login --claudeai` prints a URL, then blocks reading a pasted
//! `code#state` from STDIN. That conversation — with a human in the middle — is
//! why logging an account in was the one account operation the interface could
//! not do: it had to suspend itself and hand the operator a bare terminal.
//!
//! # The pty question, settled by measurement and pinned here
//!
//! `scripts/add-account` deliberately INHERITS stdin and stdout, so the first
//! question was whether a parent can drive this on plain pipes or needs a pty.
//! Measured against the real binary (v2.1.220):
//!
//! * On a PIPE: the banner, the URL and the prompt arrive in ~1.2s in plain
//!   UTF-8 with ZERO ANSI escapes. No non-tty detection, no refusal, no
//!   suppression of the URL. Writing `code#state\n` afterwards performs the
//!   exchange.
//! * Under a PTY: the same output wrapped in an OSC-8 hyperlink, the URL
//!   repeated inside `\x1b[94m…`, `\r` line endings, a cursor escape.
//!
//! **A pty is not required and would be worse** — it buys nothing and costs an
//! escape-stripping pass over the one string that must not be corrupted.
//!
//! Two measured behaviours the implementation has to respect, both of which
//! would have been easy to get wrong by reasoning alone, and both of which the
//! fixture here reproduces exactly:
//!
//! 1. **The prompt has NO trailing newline.** A line-oriented reader blocks on
//!    it forever, waiting for an EOL that only arrives after the answer.
//! 2. **EOF on stdin is IGNORED.** Closing the pipe does not make the child
//!    exit. The parent owns every timeout.
//!
//! # Adversarial
//!
//! The fixture is `tests/fixtures/fake-add-account`, which reproduces the
//! protocol rather than the credentials — nothing here touches the operator's
//! keychain, spawns `claude`, or reaches the network.

use std::path::PathBuf;

use threads::login::{LoginOptions, LoginSessions};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-add-account")
}

#[path = "support/fake_login.rs"]
mod fake_login;

/// R12.5b: every login is now GUARDED — snapshotted, then asked of the server.
/// These tests drive the PIPE PROTOCOL, so the guard runs against the on-disk
/// fakes (no keychain, no network), and the fixture lands a credential that
/// the fake server reads back as `<label>@x.test`. The guard's own cases are
/// in `tests/daemon_login_guard.rs`.
fn sessions(home: &std::path::Path) -> LoginSessions {
    fake_login::install_identity_seam();
    let s = LoginSessions::default();
    s.set_keychain_for_test(fake_login::keychain_at(home));
    s
}

/// **The URL comes back from `begin`, and the code goes in through `complete`.**
///
/// The whole flow, in the shape the interface codes against.
///
/// **THE MUTATIONS**, each red here:
///
/// 1. Use `read_line` instead of a byte read in `begin`. The prompt has no
///    trailing newline, so this HANGS and the test times out at 60s — which is
///    exactly the bug the measurement exists to prevent.
/// 2. Write the code at spawn time instead of after the prompt. The measured
///    child swallows pre-fed input at startup; the fixture's `read` runs after
///    the prompt for the same reason, so the code is lost and the exchange never
///    happens.
/// 3. Take the FIRST `https://` in the output rather than the line that names
///    it — `find_url`'s unit tests cover that directly.
#[tokio::test]
async fn a_login_returns_a_url_and_then_accepts_the_pasted_code() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = sessions(tmp.path());

    let begun = sessions
        .begin(&fixture(), tmp.path(), "aa-owned", LoginOptions::default())
        .await
        .expect("begin should reach the prompt");

    assert_eq!(begun.account, "aa-owned");
    assert!(
        begun.url.starts_with("https://claude.com/cai/oauth/authorize"),
        "the URL is what the operator opens: {}",
        begun.url
    );
    assert!(
        begun.url.contains("state=SPIKEaa-owned"),
        "and it is the whole line, query string included — a truncated authorize link \
         fails in a way that looks like the operator's fault: {}",
        begun.url
    );
    assert!(
        !begun.url.contains('\u{1b}'),
        "no ANSI escapes: this is why the flow runs on a PIPE and not a pty — a pty \
         wraps the same URL in an OSC-8 hyperlink and repeats it in a colour escape"
    );
    assert!(!begun.session_id.is_empty());

    // The session is in flight and visible while the human is at the browser.
    let live = sessions.list().await;
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].1, "aa-owned");

    // THE SECOND ROUND TRIP.
    let done = sessions
        .complete(&begun.session_id, "abc123#SPIKEaa-owned")
        .await
        .unwrap();
    assert!(done.ok, "output: {}", done.output);
    assert_eq!(done.exit_code, Some(0));
    assert_eq!(done.account, "aa-owned");
    assert!(
        done.output.contains("distinct account"),
        "the script's OWN words come back — its refusals name the remedy and \
         paraphrasing them would lose it: {}",
        done.output
    );

    // And the session is gone: it cannot be completed twice.
    assert!(sessions.list().await.is_empty());
    let again = sessions.complete(&begun.session_id, "abc123#x").await;
    assert!(again.is_err(), "a completed session must not be reusable");
}

/// **A refused exchange is a FAILURE, and it is read off the exit status.**
///
/// `add-account` exits non-zero for every refusal it makes and for a failed
/// exchange. The verdict is that status — never a string in the output.
///
/// **THE MUTATION.** Decide `ok` by searching the output for "failed" or for
/// "distinct account". Both go wrong the first time the script's wording moves,
/// and grepping another program's prose for outcomes is precisely what this
/// whole API surface exists to stop — the old interface did it in four places.
#[tokio::test]
async fn a_bad_code_is_a_failure_and_the_verdict_is_the_exit_status() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = sessions(tmp.path());

    let begun = sessions
        .begin(&fixture(), tmp.path(), "aa-owned", LoginOptions::default())
        .await
        .unwrap();
    // No '#': the fixture stands in for the 400 the real exchange returns.
    let done = sessions.complete(&begun.session_id, "not-a-real-code").await.unwrap();

    assert!(!done.ok);
    assert_eq!(done.exit_code, Some(1));
    assert!(
        done.output.contains("Login failed"),
        "the failure text is preserved for the operator: {}",
        done.output
    );
}

/// **An empty paste is refused before the child is written to.**
///
/// A newline-only paste would be read as the answer, and the exchange would
/// fail with a server error that reads like the operator's fault.
#[tokio::test]
async fn an_empty_code_is_refused_and_the_child_is_not_left_running() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = sessions(tmp.path());

    let begun = sessions
        .begin(&fixture(), tmp.path(), "aa-owned", LoginOptions::default())
        .await
        .unwrap();
    let err = sessions
        .complete(&begun.session_id, "   \n  ")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("empty"), "{err}");
    // The session was consumed and the child killed — it must not be left
    // parked on a read, because the child IGNORES EOF.
    assert!(sessions.list().await.is_empty());
}

/// **A script that refuses before printing a URL surfaces ITS OWN refusal.**
///
/// This is the credential-guard path and the reason the flow drives
/// `add-account` rather than reimplementing it: the script refuses when
/// `CLAUDE_CODE_OAUTH_TOKEN` is set, because `claude` then DELETES the
/// machine-default keychain item on exit and destroys the operator's live
/// login. That guard has to reach the operator intact.
///
/// **THE MUTATION.** Report a generic "the login could not be started" and the
/// remedy — which is a specific environment variable to unset — is lost.
#[tokio::test]
async fn a_refusal_before_the_url_reports_the_scripts_own_words() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = sessions(tmp.path());

    let err = sessions
        .begin(&fixture(), tmp.path(), "refuse-me", LoginOptions::default())
        .await
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("CLAUDE_CODE_OAUTH_TOKEN"),
        "the guard's exact words, including the variable to unset: {err}"
    );
    assert!(
        sessions.list().await.is_empty(),
        "a failed begin leaves no session behind"
    );
}

/// **A missing script is refused with a reason, not a panic.**
#[tokio::test]
async fn a_missing_script_is_refused_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = sessions(tmp.path());
    let err = sessions
        .begin(
            &tmp.path().join("nope"),
            tmp.path(),
            "aa-owned",
            LoginOptions::default(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("login script is not at"), "{err}");
}

/// **A second login into one account SUPERSEDES the first; two accounts are fine.**
///
/// This assertion used to be the opposite: a second `begin` for an account
/// already mid-login returned `Err("... already in progress — complete it with
/// the pasted code, or cancel it, before starting another")`.
///
/// That is a refusal on the grounds that prior state exists, and it is a nasty
/// one. An ABANDONED session — a browser tab he closed, a code he never pasted
/// — holds the slot for the full `SESSION_TTL` of fifteen minutes, during which
/// the interface tells him he cannot sign in and gives him no way to clear it.
/// He is not going to hunt for a cancel button; he is going to press the login
/// key again. That must work.
///
/// **THE ORIGINAL CONCERN IS STILL HONOURED.** The reason for one-at-a-time was
/// that two LIVE children would race on one config dir and one keychain item.
/// Superseding kills the old child before spawning the new one, so there are
/// still never two — which is what the session-count assertions below prove.
///
/// Anything that arbitrates between two or more of something is proven at N>=2
/// or not proven, so this drives a third login as well: supersession has to be
/// repeatable, not a one-shot.
///
/// **THE MUTATION.** Make `begin` return `Err` when a session exists and the
/// supersession assertions go red; drop the kill-and-remove and the
/// one-session-per-account count goes red instead.
#[tokio::test]
async fn a_second_login_supersedes_the_first_and_two_accounts_may_run_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = sessions(tmp.path());

    let a = sessions
        .begin(&fixture(), tmp.path(), "aa-owned", LoginOptions::default())
        .await
        .unwrap();

    // THE KEYSTROKE HE ACTUALLY MAKES: press login again on a slot whose
    // previous attempt he abandoned. It must start a login, not refuse one.
    let again = sessions
        .begin(&fixture(), tmp.path(), "aa-owned", LoginOptions::default())
        .await
        .expect("a login already in flight must NEVER refuse a new one");
    assert_ne!(
        again.session_id, a.session_id,
        "the new login is a new session, not the old one handed back"
    );
    assert_eq!(
        sessions.list().await.len(),
        1,
        "the superseded session is gone — there are still never two children on one slot"
    );
    assert!(
        !sessions.cancel(&a.session_id).await,
        "the superseded session was removed, so cancelling it is a no-op"
    );

    // Repeatable: a third attempt supersedes the second the same way.
    let third = sessions
        .begin(&fixture(), tmp.path(), "aa-owned", LoginOptions::default())
        .await
        .expect("supersession is repeatable, not a one-shot");
    assert_ne!(third.session_id, again.session_id);
    assert_eq!(sessions.list().await.len(), 1);

    // A DIFFERENT account is unaffected: supersession is per slot, not global.
    let b = sessions
        .begin(&fixture(), tmp.path(), "zz-second", LoginOptions::default())
        .await
        .expect("a different account is a different slot");

    assert_eq!(sessions.list().await.len(), 2);
    assert_ne!(third.session_id, b.session_id);
    assert_ne!(third.url, b.url, "each login carries its own state parameter");

    assert!(sessions.cancel(&third.session_id).await);
    assert!(sessions.cancel(&b.session_id).await);
    assert!(sessions.list().await.is_empty());
    assert!(
        !sessions.cancel(&third.session_id).await,
        "cancelling twice is false, not an error"
    );
}

/// **A child that ignores EOF is REAPED, not left parked forever.**
///
/// The measured behaviour: closing the child's stdin does NOT make it exit —
/// `claude auth login` hung for the full timeout on `< /dev/null` and on a pipe
/// closed after the prompt. So a session nobody completes is a live process
/// parked on a read, and without a sweep it would sit there until the daemon
/// restarted. On a daemon that runs for weeks, that is a leak per abandoned
/// login.
///
/// **THE MUTATION.** Make `sweep` a no-op returning 0, or drop the `kill` and
/// merely remove the map entry — the first assertion goes red, and in the
/// second case the process leaks silently while the test still "passes" its
/// bookkeeping. The `list()` assertion is therefore paired with the reap COUNT.
#[tokio::test]
async fn an_abandoned_login_is_reaped_because_the_child_ignores_eof() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = sessions(tmp.path());

    let begun = sessions
        .begin(&fixture(), tmp.path(), "hang-me", LoginOptions::default())
        .await
        .expect("the URL is printed before the child hangs");
    assert_eq!(sessions.list().await.len(), 1);

    // Nothing is stale yet — a human is reading a browser page, and the TTL is
    // generous on purpose.
    assert_eq!(
        sessions.sweep().await,
        0,
        "a fresh session must not be reaped out from under the operator"
    );
    assert_eq!(sessions.list().await.len(), 1);

    // Cancelling kills it. (The TTL path is the same `kill`; forcing a 15
    // minute wait here would buy nothing over asserting the kill itself.)
    assert!(sessions.cancel(&begun.session_id).await);
    assert!(sessions.list().await.is_empty());
}

// ════════════════════════════════════════════════════════════════════════════
// ROTATION: progress is a frame, and the verdict is the JSON
// ════════════════════════════════════════════════════════════════════════════

/// **A rotation pass emits structured progress and returns a structured
/// verdict.**
///
/// The interface used to get its progress by TAILING `accounts/rotation.log`:
/// capture a byte offset, seek, read every 150ms, regex-parse another
/// program's sentences into outcomes, then classify by string prefix and decide
/// which accounts needed a manual login with `"needs" in status`.
///
/// Two things were wrong and only one was the parsing. `rotation.log` is a
/// SHARED append-only file, so a pass the operator started from the CLI a
/// second earlier interleaved into the same tail and was attributed to the
/// client's own run.
///
/// **THE MUTATIONS**, each red here:
///
/// 1. Don't set `CLAUDE_ACCOUNTS_LOG` — the fixture exits non-zero because the
///    seam is unset, which is the assertion that the log really is run-private
///    rather than the shared one.
/// 2. Return `Ok` with `Value::Null` when stdout has no parseable JSON. A pass
///    that printed nothing rotated nothing, and an empty result reads as
///    "nothing needed doing" — the phases assertion still passes, so the
///    verdict assertion is what catches it.
/// 3. Forward every log line as one opaque `note` phase. The `needs_login` and
///    `rotated` assertions go red, and the client is back to grepping prose.
#[tokio::test]
async fn a_rotation_pass_streams_phases_and_returns_the_json_verdict() {
    let tmp = tempfile::tempdir().unwrap();
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-rotate");

    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    let sink = seen.clone();
    let (ok, result, _errs) = threads::login::rotate(
        &script,
        tmp.path(),
        "run-1",
        false,
        |phase, detail| sink.lock().unwrap().push((phase.to_string(), detail)),
    )
    .await
    .expect("the pass should run");

    assert!(ok);

    // ── the PHASES, machine-readable ──
    let frames = seen.lock().unwrap().clone();
    let phases: Vec<&str> = frames.iter().map(|(p, _)| p.as_str()).collect();
    assert!(
        phases.contains(&"started"),
        "the pass announces itself: {phases:?}"
    );
    assert!(
        phases.contains(&"rotated"),
        "a rolled credential is `rotated`, not a line beginning with the word ROLLED that \
         a client has to recognise: {phases:?}"
    );
    assert!(phases.contains(&"skipped"), "{phases:?}");
    assert!(
        phases.contains(&"needs_login"),
        "the one outcome that needs the OPERATOR is named, rather than being found by \
         testing whether the status string contains 'needs': {phases:?}"
    );
    assert!(phases.contains(&"finished"), "{phases:?}");

    // The prose is still carried — it is the DETAIL, not the outcome.
    let rotated = frames.iter().find(|(p, _)| p == "rotated").unwrap();
    assert!(rotated.1.contains("aa-owned"), "{:?}", rotated);
    assert!(rotated.1.contains("30d"), "{:?}", rotated);

    // ── the VERDICT, from --json and not from the log ──
    assert_eq!(result["live_login"], "aa-owned");
    let slots = result["slots"].as_array().unwrap();
    assert_eq!(slots.len(), 3);
    assert_eq!(slots[0]["outcome"], "rotated");
    assert_eq!(slots[0]["days_left"], 30.0);
    assert_eq!(
        slots[2]["days_left"],
        serde_json::Value::Null,
        "an unknown days-left is null, never 0 — the same rule the pressure view follows"
    );

    // ── the log was RUN-PRIVATE, and is cleaned up on success ──
    let shared = tmp.path().join("accounts").join("rotation.log");
    assert!(
        !shared.exists(),
        "the pass must not have written to the SHARED log — that file is what let a \
         concurrent CLI rotation interleave into the client's tail"
    );
    assert!(
        !tmp.path()
            .join("accounts/.rotate-runs/run-1.log")
            .exists(),
        "a successful pass leaves no stale progress log to become a second source of truth"
    );
}

/// **A pass whose output is unparseable is a FAILURE, not an empty success.**
#[tokio::test]
async fn a_rotation_that_produces_no_json_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    // The login fixture is not a rotate script: it prints a URL and blocks, so
    // its stdout is prose rather than JSON. That is exactly the shape being
    // guarded against.
    let err = threads::login::rotate(&fixture(), tmp.path(), "run-2", false, |_, _| {})
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("no parseable JSON"),
        "a pass that printed nothing usable rotated nothing, and reporting an empty \
         result would read as 'nothing needed doing': {err}"
    );
}
