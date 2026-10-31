//! Retry classification tests: the CCW+AUI merged classifier (G9 credit
//! balance, AUI's substring set) and the §9.4 backoff fix.

use threads::retry::{
    backoff_ms, classify_errored_result, classify_message, classify_result,
    usage_limit_backoff_secs, RetryDecision, TransientKind,
};

#[test]
fn credit_balance_is_first_class_and_never_retried() {
    // G9: AUI speaks a specific message for this and does not retry.
    match classify_message("Your credit balance is too low") {
        RetryDecision::CreditBalance { .. } => {}
        other => panic!("expected CreditBalance, got {other:?}"),
    }
    // Ordering matters: credit balance wins even when "limit" appears too.
    match classify_message("credit balance exhausted; usage limit reached") {
        RetryDecision::CreditBalance { .. } => {}
        other => panic!("expected CreditBalance, got {other:?}"),
    }
}

#[test]
fn rate_limits_and_aui_patterns() {
    for msg in [
        "You have hit your session limit resets 2:40am (America/Chicago)",
        "usage limit reached",
        "429 too many requests",
        "rate_limit",
    ] {
        match classify_message(msg) {
            RetryDecision::Retry {
                kind: TransientKind::RateLimit,
                ..
            } => {}
            other => panic!("{msg:?} → {other:?}"),
        }
    }
    // AUI's transient patterns that CCW lacked.
    for msg in ["API Error: 502 Bad Gateway", "service temporarily unavailable"] {
        match classify_message(msg) {
            RetryDecision::Retry {
                kind: TransientKind::Overload,
                ..
            } => {}
            other => panic!("{msg:?} → {other:?}"),
        }
    }
    // Wire-level failures now land in `Connectivity` rather than
    // `ConnectionDrop` (intent item 73). The class was renamed because it grew
    // a POLICY of its own — its own attempt ceiling, its own capped ladder, its
    // own `"connectivity"` tag, and no failover — and because the old name
    // described only the failures that mention a connection, which is the
    // narrow reading that let a DNS failure kill a thread. `is_connectivity()`
    // is the predicate everything branches on, so it is what this asserts;
    // `ConnectionDrop` still deserializes for retry rows written before the
    // change. Full coverage of the vocabulary lives in
    // `tests/network_resilience.rs`.
    for msg in ["request timed out", "connection reset by peer"] {
        match classify_message(msg) {
            RetryDecision::Retry { kind, .. } if kind.is_connectivity() => {}
            other => panic!("{msg:?} → {other:?}"),
        }
    }
    assert!(TransientKind::ConnectionDrop.is_connectivity());
}

#[test]
fn expired_resume_restarts_fresh_and_unknown_is_fatal() {
    assert!(matches!(
        classify_message("No conversation found with session ID abc"),
        RetryDecision::RestartFresh { .. }
    ));
    assert!(matches!(
        classify_message("some flag parse explosion"),
        RetryDecision::Fatal { .. }
    ));
}

#[test]
fn result_subtypes() {
    assert!(matches!(
        classify_result("error_during_execution"),
        RetryDecision::Retry {
            kind: TransientKind::ErrorDuringExecution,
            ..
        }
    ));
    assert!(matches!(
        classify_result("error_max_turns"),
        RetryDecision::Fatal { .. }
    ));
}

// ════════════════════════════════════════════════════════════════════════════
// `subtype: "success"` WITH `is_error: true` — the contradiction that ate 46
// operator turns
// ════════════════════════════════════════════════════════════════════════════

/// **A turn is never thrown away under the words "result subtype success".**
///
/// The defect, stated as the one thing that must not happen. Between
/// 2026-08-03 and 2026-08-18 the live ledger recorded 46 `turn_failed` events
/// whose entire message was `result subtype success` — a fatal `claude_error`
/// synthesized from a subtype that says the run ended normally. The operator's
/// prompt went with it: `Fatal` clears the retry row, so nothing is left
/// holding his words.
#[test]
fn a_flagged_success_result_is_never_fatal() {
    for text in [
        Some("API Error: 529 Overloaded. This is a server-side issue, usually temporary."),
        Some("API Error: Unable to connect to API (ENOTFOUND)"),
        Some("API Error: Connection closed mid-response. The response above may be incomplete."),
        Some("API Error: 500 Internal server error."),
        // The two cases with no diagnosis at all: a text we do not recognize,
        // and no text whatsoever. These are the ones a "when in doubt, fail"
        // rule would still destroy, and they must not be destroyed either.
        Some("something nobody has ever seen before"),
        None,
    ] {
        let d = classify_errored_result("success", text);
        assert!(
            !matches!(d, RetryDecision::Fatal { .. }),
            "a self-contradictory result must not be fatal — the operator's turn is \
             held on the retry row, not discarded. text={text:?} got={d:?}"
        );
        assert!(
            !format!("{d:?}").contains("result subtype success"),
            "the tautology that named nothing must be gone: {d:?}"
        );
    }
}

/// **The cause is read off the stream's last words, so each failure gets the
/// ladder built for it.**
///
/// A 529 is the service answering under load (overload ladder); ENOTFOUND and
/// a dropped connection are the network being gone (connectivity ladder, forty
/// attempts, item 73). Classifying by subtype could not tell them apart
/// because the subtype does not know.
#[test]
fn a_flagged_success_result_is_classified_by_what_the_stream_actually_said() {
    match classify_errored_result("success", Some("API Error: 529 Overloaded")) {
        RetryDecision::Retry { kind, .. } => assert_eq!(kind, TransientKind::Overload),
        other => panic!("a 529 is an overload: {other:?}"),
    }
    for net in [
        "API Error: Unable to connect to API (ENOTFOUND)",
        "API Error: Connection closed mid-response.",
    ] {
        match classify_errored_result("success", Some(net)) {
            RetryDecision::Retry { kind, .. } => assert!(
                kind.is_connectivity(),
                "{net:?} is the network being gone, got {kind:?}"
            ),
            other => panic!("{net:?} must be retried: {other:?}"),
        }
    }
    // A credit balance keeps its own class even here — G9 is never retried.
    assert!(matches!(
        classify_errored_result("success", Some("Your credit balance is too low")),
        RetryDecision::CreditBalance { .. }
    ));
}

/// **Whatever the CLI said is carried into the reason, so the failure names
/// itself.**
///
/// `Prompt is too long` was one of the 46. Retried and then reported, the
/// operator can see what happened; reported as "result subtype success" he
/// could not.
#[test]
fn the_reason_quotes_the_clis_own_last_words() {
    let d = classify_errored_result("success", Some("Prompt is too long"));
    let reason = match &d {
        RetryDecision::Retry { reason, .. } => reason.clone(),
        other => panic!("must not be terminal: {other:?}"),
    };
    assert!(
        reason.contains("Prompt is too long"),
        "the operator must be told what actually failed, got {reason:?}"
    );
}

/// The anti-test: every OTHER subtype is classified exactly as before. This
/// change is about the one subtype that carries no information, and it must
/// not become a blanket "retry everything".
#[test]
fn every_other_subtype_is_unchanged_by_the_success_case() {
    assert!(matches!(
        classify_errored_result("error_max_turns", Some("API Error: 529 Overloaded")),
        RetryDecision::Fatal { .. }
    ));
    assert!(matches!(
        classify_errored_result("error_max_budget_usd", None),
        RetryDecision::Fatal { .. }
    ));
    assert!(matches!(
        classify_errored_result("error_during_execution", None),
        RetryDecision::Retry {
            kind: TransientKind::ErrorDuringExecution,
            ..
        }
    ));
    // An unknown subtype IS still a diagnosis of sorts — it names itself — and
    // stays terminal.
    assert!(matches!(
        classify_errored_result("error_something_new", None),
        RetryDecision::Fatal { .. }
    ));
}

#[test]
fn backoff_actually_escalates_with_attempt() {
    // §9.4: CCW computed this ladder then called it with a literal 1.
    // The runtime now passes the real attempt; assert the ladder is real.
    assert_eq!(backoff_ms(1), 1000);
    assert_eq!(backoff_ms(2), 2000);
    assert_eq!(backoff_ms(3), 4000);
    assert_eq!(backoff_ms(6), 30_000); // capped
    assert_eq!(backoff_ms(60), 30_000); // still capped
    assert!(backoff_ms(3) > backoff_ms(1), "the ladder must escalate");
}

#[test]
fn usage_limit_ladder_matches_aui() {
    // AUI's 30/60/120/300-capped indefinite ladder.
    assert_eq!(usage_limit_backoff_secs(1), 30);
    assert_eq!(usage_limit_backoff_secs(2), 60);
    assert_eq!(usage_limit_backoff_secs(3), 120);
    assert_eq!(usage_limit_backoff_secs(4), 300);
    assert_eq!(usage_limit_backoff_secs(99), 300);
}

#[test]
fn error_kind_tags_match_aui_vocabulary() {
    assert_eq!(TransientKind::RateLimit.kind_str(), "usage_limit");
    assert_eq!(TransientKind::Overload.kind_str(), "transient");
    assert_eq!(
        RetryDecision::CreditBalance { reason: String::new() }.kind_str(),
        "credit_balance"
    );
    assert_eq!(
        RetryDecision::Fatal { reason: String::new() }.kind_str(),
        "claude_error"
    );
}
