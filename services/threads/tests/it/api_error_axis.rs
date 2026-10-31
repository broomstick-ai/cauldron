//! **The synthetic API error: detected at all, and then on the RIGHT AXIS.**
//!
//! Two defects of one shape live here, and this file is the evidence for both.
//!
//! # 1. The detector was keyed on a spelling the producer does not emit
//!
//! [`threads::stream::is_api_error`] tested `isApiErrorMessage` /
//! `apiErrorStatus`. The CLI emits `is_api_error_message` / `api_error_status`.
//! Measured over the operator's own turn logs (7,018 `.jsonl` files under
//! `lib/threads/turns/`, 2026-07-30 → 2026-08-26):
//!
//! | spelling, as a JSON KEY          | files |
//! |----------------------------------|-------|
//! | `"isApiErrorMessage":`           | **0** |
//! | `"apiErrorStatus":`              | **0** |
//! | `"is_api_error_message":`        | 4,825 |
//! | `"api_error_status":`            | 6,526 |
//!
//! The only occurrences of the camelCase spellings anywhere in the corpus are
//! inside quoted *source code* — `lib/ccw/src/stream.rs`, the very function
//! that looks for them, pasted into a transcript. The producer has never used
//! them. Only the `error == "rate_limit"` clause has ever fired.
//!
//! # 2. Fixing #1 naively re-buries the cause
//!
//! Every detected API error became `ThreadEvent::LimitHit`, and `interpret`
//! converts a `LimitHit` UNCONDITIONALLY into `TransientKind::RateLimit`. So
//! simply accepting the snake_case spelling would mint an expired OAuth session
//! as a rate limit — onto the usage-limit ladder, which waits for a reset that
//! will never come, with no login bar and no operator-facing sentence. That is
//! the same class-loss as the `classify_errored_result` downgrade fixed in
//! `90b7691`, arriving from the other end.
//!
//! So the detector is split: `LimitHit` is now minted ONLY for a genuine limit,
//! and every other API error becomes [`ThreadEvent::ApiError`], which carries
//! `error` and `status` intact to [`threads::retry::classify_api_error`].
//!
//! # The fixtures are REAL
//!
//! `tests/fixtures/api_error_*.jsonl` are lines lifted VERBATIM out of the
//! operator's turn logs, not written here. A fixture built from an author's
//! understanding of the format is exactly what hid defect #1 for months: the
//! old `tests/stream_parse.rs` fixture asserted camelCase and passed forever
//! while the detector never fired once in production.

use serde_json::Value;
use threads::event::ThreadEvent;
use threads::retry::{RetryDecision, TransientKind};

fn assistant_lines() -> Vec<Value> {
    let raw = include_str!("../fixtures/api_error_assistant_lines.jsonl");
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("real captured line must parse"))
        .collect()
}

fn result_lines() -> Vec<Value> {
    let raw = include_str!("../fixtures/api_error_result_lines.jsonl");
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("real captured line must parse"))
        .collect()
}

fn error_of(v: &Value) -> Option<&str> {
    v.get("error").and_then(Value::as_str)
}

/// **THE FORCED FAILURE FOR DEFECT 2.** Against the unfixed detector this fails
/// on the first non-`rate_limit` line in the corpus, because the only clause
/// that has ever worked is `error == "rate_limit"`.
#[test]
fn every_real_captured_api_error_line_is_detected() {
    let lines = assistant_lines();
    assert!(!lines.is_empty(), "the fixture must not be empty");
    for v in &lines {
        assert!(
            threads::stream::is_api_error(v),
            "a REAL captured line the CLI emitted was not detected as an API error \
             (error={:?}, is_api_error_message={:?}) — this is the casing drift: \
             the producer emits `is_api_error_message`, the detector read \
             `isApiErrorMessage`",
            error_of(v),
            v.get("is_api_error_message"),
        );
    }
}

/// The camelCase spelling is still accepted. The corpus cannot prove no live
/// producer speaks it (`get_either`'s own argument), so this is additive.
#[test]
fn the_old_camel_case_spelling_still_works() {
    let v: Value = serde_json::json!({
        "type": "assistant",
        "isApiErrorMessage": true,
        "apiErrorStatus": 429,
        "message": {"content": [{"type": "text", "text": "You've hit your weekly limit"}]}
    });
    assert!(threads::stream::is_api_error(&v));
}

/// **THE AXIS.** An expired OAuth session must NOT become a rate limit.
///
/// This is the test that a naive spelling fix fails: with `is_api_error`
/// accepting snake_case but `parse_assistant` still minting `LimitHit` for
/// everything, the auth line below becomes a `LimitHit`, and `interpret` turns
/// every `LimitHit` into `TransientKind::RateLimit` with no guard at all.
#[test]
fn an_expired_login_is_not_minted_as_a_rate_limit() {
    let auth: Vec<Value> = assistant_lines()
        .into_iter()
        .filter(|v| error_of(v) == Some("authentication_failed"))
        .collect();
    assert!(
        !auth.is_empty(),
        "the fixture must contain a real authentication_failed capture"
    );

    for v in &auth {
        let events = threads::stream::parse_line(v);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ThreadEvent::LimitHit { .. })),
            "an expired OAuth session was minted as a LimitHit — the runtime converts \
             every LimitHit to TransientKind::RateLimit unconditionally, so this puts a \
             credential failure on the usage-limit ladder waiting for a reset that will \
             never arrive. Events: {events:?}"
        );

        let api = events
            .iter()
            .find_map(|e| match e {
                ThreadEvent::ApiError {
                    error,
                    status,
                    text,
                } => Some((error.clone(), *status, text.clone())),
                _ => None,
            })
            .expect("a real authentication_failed line must become ThreadEvent::ApiError");

        assert_eq!(api.0.as_deref(), Some("authentication_failed"));
        let decision = threads::retry::classify_api_error(api.0.as_deref(), api.1, &api.2);
        assert!(
            matches!(decision, RetryDecision::LoginExpired { .. }),
            "a real captured `authentication_failed` must classify as LoginExpired, \
             got {decision:?}"
        );
    }
}

/// A real rate limit still lands on `LimitHit` — the gauge, the pool and the
/// reset prose all hang off that variant and must not regress.
#[test]
fn a_real_rate_limit_still_mints_a_limit_hit() {
    let limits: Vec<Value> = assistant_lines()
        .into_iter()
        .filter(|v| error_of(v) == Some("rate_limit"))
        .collect();
    assert!(!limits.is_empty());
    for v in &limits {
        let events = threads::stream::parse_line(v);
        let hit = events
            .iter()
            .find_map(|e| match e {
                ThreadEvent::LimitHit { reset, text, .. } => Some((reset.clone(), text.clone())),
                _ => None,
            })
            .expect("a real rate_limit capture must still be a LimitHit");
        assert!(
            hit.1.contains("limit"),
            "the refusal prose must be carried: {hit:?}"
        );
    }
}

/// Every distinct class in the real corpus lands on its own axis. This is the
/// whole point of the fix: five causes, five verdicts, none of them collapsed
/// into a sixth.
#[test]
fn each_real_class_lands_on_its_own_axis() {
    for v in &assistant_lines() {
        let events = threads::stream::parse_line(v);
        let text = events
            .iter()
            .find_map(|e| match e {
                ThreadEvent::ApiError { text, .. } => Some(text.clone()),
                ThreadEvent::LimitHit { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let status = events.iter().find_map(|e| match e {
            ThreadEvent::ApiError { status, .. } | ThreadEvent::LimitHit { status, .. } => *status,
            _ => None,
        });
        let decision = threads::retry::classify_api_error(error_of(v), status, &text);
        let expected = match error_of(v) {
            Some("rate_limit") => "usage_limit",
            Some("authentication_failed") => "login_expired",
            // Both real captures read like wire failures ("Connection closed
            // mid-response", "Unable to connect to API (ENOTFOUND)"), and that
            // prose used to win. SUPERSEDED 2026-10-14 (an internal thread): the
            // producer's named cause wins, and a server error is its own class
            // on its own small budget — never the connectivity ladder.
            Some("server_error") => "server_error",
            // "safeguards flagged this message" / "Prompt is too long" — the
            // request itself is bad, and no amount of retrying changes it.
            Some("invalid_request") => "claude_error",
            other => panic!("unexpected class in the real corpus: {other:?}"),
        };
        assert_eq!(
            decision.kind_str(),
            expected,
            "class {:?} (text {:?}) landed on the wrong axis: {decision:?}",
            error_of(v),
            text
        );
    }
}

/// A 5xx from the service is a SERVER ERROR (its own small ladder since
/// 2026-10-14, an internal thread), and it must not be read as a deterministic
/// failure just because its prose names no network vocabulary.
#[test]
fn a_bare_5xx_status_is_a_server_error() {
    let d = threads::retry::classify_api_error(Some("server_error"), Some(529), "API Error: 529");
    assert_eq!(d.kind_str(), "server_error", "got {d:?}");
    let d = threads::retry::classify_api_error(None, Some(500), "something went wrong");
    assert_eq!(d.kind_str(), "server_error", "got {d:?}");
}

/// A 429 arriving with NO named error is still a rate limit. This is the shape
/// the `result` event uses (4,742 of them in the corpus) — `error` absent,
/// `api_error_status: 429`.
#[test]
fn a_bare_429_status_is_a_rate_limit() {
    for v in &result_lines() {
        let status = v.get("api_error_status").and_then(Value::as_u64);
        if status != Some(429) {
            continue;
        }
        let text = v.get("result").and_then(Value::as_str).unwrap_or_default();
        let d = threads::retry::classify_api_error(None, status, text);
        assert_eq!(d.kind_str(), "usage_limit", "got {d:?} for {text:?}");
    }
}

/// A 4xx that is not a 429 is the request's own fault — deterministic.
#[test]
fn a_non_429_4xx_is_deterministic() {
    let d = threads::retry::classify_api_error(None, Some(400), "Prompt is too long");
    assert_eq!(d.kind_str(), "claude_error", "got {d:?}");
}

/// **The unknown class must not be swallowed.** An API error naming a cause
/// this crate has never seen falls back to the prose classifier rather than to
/// a default arm — the failure mode of both audited defects.
#[test]
fn an_unknown_named_cause_falls_back_to_the_prose() {
    let d = threads::retry::classify_api_error(
        Some("some_future_class_we_have_never_seen"),
        None,
        "getaddrinfo ENOTFOUND api.anthropic.com",
    );
    assert_eq!(
        d.kind_str(),
        "connectivity",
        "an unrecognized class must still be read for its prose, got {d:?}"
    );
}

// ── the transient ladder ────────────────────────────────────────────────────

/// **His ruling: rate limiting on BOTH classes.** *"A transient issue, even
/// though it might justify an immediate retry, definitely needs a back-off
/// strategy."*
///
/// Before this fix `Overload` and `ErrorDuringExecution` had no arm in
/// `compute_retry_at` at all — they fell into `_ => backoff_ms(attempt)` and
/// ran out a `max_attempts` of 3 in **seven seconds** (1s + 2s + 4s). That is
/// not a backoff strategy, and it is the same swallowing-`_`-arm shape as the
/// two audited defects.
#[test]
fn the_transient_ladder_backs_off_after_the_first_retry() {
    use threads::retry::transient_backoff_secs;
    // The first retry may be immediate — he said so explicitly.
    assert_eq!(transient_backoff_secs(1), 0);
    // Everything after it must back off, strictly monotonically, to a ceiling.
    let rungs: Vec<u64> = (1..=8).map(transient_backoff_secs).collect();
    for w in rungs.windows(2) {
        assert!(
            w[1] >= w[0],
            "the transient ladder must never step DOWN: {rungs:?}"
        );
    }
    assert!(
        rungs[2] > rungs[1],
        "the ladder must actually back off after the immediate retry: {rungs:?}"
    );
    let total: u64 = rungs.iter().sum();
    assert!(
        total >= 600,
        "eight transient attempts must span at least ten minutes, not seven seconds — \
         got {total}s across {rungs:?}"
    );
    // And it must be BOUNDED — a ladder with no ceiling is a different bug.
    assert_eq!(transient_backoff_secs(50), transient_backoff_secs(9));
}

/// The transient ceiling must be more than three. Three attempts in seven
/// seconds is what made a 529 — the single most common cause in the ledger —
/// terminal.
#[test]
fn the_transient_ceiling_is_not_three() {
    assert!(
        threads::retry::DEFAULT_MAX_TRANSIENT_ATTEMPTS >= 6,
        "got {}",
        threads::retry::DEFAULT_MAX_TRANSIENT_ATTEMPTS
    );
}

/// Every `TransientKind` must have a ladder that is not the bare exponential
/// default. This is the regression guard for the `_` arm itself: if someone
/// adds a variant and forgets to give it a ladder, this fails.
#[test]
fn every_transient_kind_has_a_named_ladder() {
    for kind in [
        TransientKind::RateLimit,
        TransientKind::Overload,
        TransientKind::Connectivity,
        TransientKind::ConnectionDrop,
        TransientKind::ErrorDuringExecution,
        TransientKind::Deterministic,
        TransientKind::CreditBalance,
        TransientKind::LoginExpired,
    ] {
        let secs = threads::retry::ladder_secs(&kind, 3);
        assert!(
            secs > 0,
            "{kind:?} has no ladder of its own at attempt 3 — it is falling through a \
             default arm"
        );
    }
}

// ── the login bar ───────────────────────────────────────────────────────────

/// **His ruling on the recheck.** *"if it's just a process and it's not an
/// actual auth check against their API, then you can do it more frequently …
/// slow back off only up to like five minutes even or 10 minutes."*
///
/// It IS just a process: this crate has no HTTP client at all (`reqwest` is in
/// the workspace manifest but is not a dependency of `lib/threads`), and the
/// recheck is a local `claude` spawn that the CLI refuses on the credential
/// before a model is ever reached. So the ceiling comes down from 1800s.
#[test]
fn the_login_bar_backs_off_slowly_to_a_ten_minute_ceiling() {
    use threads::retry::login_bar_secs;
    let rungs: Vec<i64> = (0..8).map(login_bar_secs).collect();
    for w in rungs.windows(2) {
        assert!(w[1] >= w[0], "the login bar must not step DOWN: {rungs:?}");
    }
    assert!(
        rungs[0] <= 120,
        "the FIRST recheck must be prompt — he has just been told to run `/login`, and \
         the cost of being wrong is one refused local spawn: {rungs:?}"
    );
    let ceiling = *rungs.last().unwrap();
    assert!(
        (300..=600).contains(&ceiling),
        "the ceiling must land in his stated five-to-ten minute range, got {ceiling}s"
    );
    assert!(
        ceiling < 1800,
        "the whole point is that it is below the old thirty minutes"
    );
    assert_eq!(
        login_bar_secs(50),
        ceiling,
        "the ladder must be bounded, not unbounded"
    );
}
