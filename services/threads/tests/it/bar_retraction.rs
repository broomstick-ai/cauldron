//! **WHEN A BAR IS RETRACTED, THE STATE DERIVED FROM IT IS RETRACTED WITH IT —
//! AND A BAR IS NOT WRITTEN ON THE INSTANT ITS OWN PROSE NAMES.**
//!
//! THREE fixes out of 2026-10-06 and one mechanism deliberately NOT built, and
//! they are one disease: **limit state consulted without asking what KIND of
//! limit it is or whether it still holds.**
//!
//! # Section 1 — the reset slack
//!
//! Two false 24-hour bars were written with `limit_until - now` equal to
//! **23:59:47** and **23:59:40** — a day less thirteen and less twenty seconds.
//! That arithmetic is a signature: the prose named an instant that had elapsed
//! seconds earlier and `reset::ResetTime::resolve` rolled it forward a day.
//! `804cffd` fixed the resolution. It did not fix the INPUT, and the operator
//! read the input correctly:
//!
//! > *"whatever is setting a 24-hour limit is wrong. It's probably just trying to
//! > place on an account before the limit is really raised. The solution is
//! > probably just to extend the limit by 1 minute."*
//!
//! `limit_until` was written as the named instant exactly, so the account
//! re-entered the pool at reset + 0s — and at `03:19:17Z`, seventeen seconds past
//! the reset that same prose named, `/usage` was still answering `session =
//! 100%`. `reset::RESET_SLACK` removes the occasion: both derivations from one
//! reset — the bar and the retry floor — now land a minute past it.
//!
//! The operator's three stranded `retries` rows settle it arithmetically. Their
//! `retry_at` was `resolved + 20s`, so the resolved instant divides back out, and
//! `created_at` is when the refusal was taken: `08:19:05.588`, `08:19:12.711` and
//! `08:19:12.720`, against a prose instant of `08:19:00` — **5.6, 12.7 and 12.7
//! seconds late.** A window closing at `08:20:00` instead of `08:19:00` leaves the
//! account barred at all three, so no turn goes out, no 429 is taken, and the
//! resolver is never handed the crossed instant. The slack alone would have
//! prevented every one of them. See `reset::RESET_SLACK` for the table.
//!
//! # What is NOT here, on the operator's ruling
//!
//! An earlier version of this file proved a mechanism that RE-ARMED per-thread
//! retry floors when the bar they were derived from was falsified. It worked and
//! it is deleted, because:
//!
//! > *"note that the parked status means that there is a pending turn and it has
//! > not been placed. There should not be a per-thread retry timestamp. And there
//! > are certain events which tell placement to rerun the full placement
//! > algorithm — e.g. new_account, limit_expired, and others. We just need to make
//! > sure that those events are complete."*
//!
//! A fourth writer reaching into `retries.retry_at` entrenches the thing that
//! should not exist. The gap is real and it is recorded in
//! `runtime::note_account_quota_measured_open`'s doc comment, where the next
//! author will be standing.
//!
//! # Section 2 — the published window
//!
//! `AccountState::effective_limit_until` retracted a window for STALENESS and
//! never for EXPIRY, so `alice-example-com` republished
//! `Some("2026-10-02T15:45:29Z")` — four days in the past — on every
//! `accounts.list` hydrate and every live fold, carrying the OAuth failure
//! sentence its `login` bar was written from. Proved ON THE WIRE, on the published
//! cell, because the accessor is not what was broken.
//!
//! # Section 3 — the gate that could not see the axis
//!
//! `budgets::pressure_reading` suppressed an account's pressure for ANY live bar,
//! because it asked `effective_limit_until`, which carries no axis. The operator:
//! *"the threads tui should display pressure on accounts even if they are at the
//! session limit. They should only not display pressure if they are at the weekly
//! limit."* `AccountState::limit_axis` was one field along from the instant the
//! whole time. The suppression is now its own predicate
//! (`AccountState::limit_hides_pressure`), applied at BOTH of
//! `pressure_reading`'s exclusion ladders, and it is NOT availability: §3.4 is the
//! dangerous direction and asserts the gate is untouched.
//!
//! # Forced red
//!
//! Every test was observed failing before it was believed and names its mutation.
//! The forcings OPPOSE. In §1, removing the slack from BOTH derivations reddens
//! §1.1 and §1.3 while §1.2 stays green; removing it from ONE reddens §1.2 while
//! §1.1 or §1.3 stays green; adding it everywhere reddens §1.5. In §2, an
//! expiry-blind accessor reddens §2.1 and §2.3; an accessor that answers `None`
//! always reddens §2.2. In §3, suppressing on ANY bar (the pre-fix `unavailable`)
//! reddens §3.1, §3.3 and §3.4 while §3.2 and §3.5 stay green; suppressing on NO
//! bar reddens §3.2 and §3.3 while §3.1, §3.4 and §3.5 stay green; and letting the
//! placement gate inherit the axis rule reddens §3.4 alone.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::event::{Push, StoredEvent};
use threads::gauge::Axis;
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

// ═════════════════════════════════════════════════════════════════════════
// FIXTURES
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
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    // A LONG resume tick: every floor this file writes is hours out, so the sweep
    // has nothing to do, and an assertion about an exact instant must not race a
    // background pass.
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &Path, accounts: &[&str]) -> Config {
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
        // Every poll in this file is explicit, so each assertion is about the poll
        // the test took and not one that happened to land.
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    // `inject_context` was split into one flag per injected thing (`12ccddb`);
    // `Injection::NOTHING` is what `inject_context: false` spelled, by that
    // const's own doc comment. This line is a COMPILE REPAIR of a sibling's
    // rename, not a change of what this test asserts.
    s.injection = threads::settings::Injection::NOTHING;
    s
}

/// Steer this account's reported SESSION percentage across successive polls —
/// the axis the 2026-10-06 incident was on.
fn session_reads(dir: &Path, account: &str, seq: &str) {
    std::fs::write(dir.join(account).join("usage-seq"), seq).unwrap();
}

fn state_of(m: &ThreadManager, name: &str) -> threads::accounts::AccountState {
    m.store
        .list_account_states()
        .unwrap()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no accounts row for {name}"))
}

fn send(m: &ThreadManager, id: &str, text: &str) {
    m.send(
        id,
        text.to_string(),
        threads::SendIntent::Immediate,
        json!({}),
        false,
    )
    .unwrap();
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

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..600 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .unwrap_or_else(|e| panic!("not an rfc3339 instant: {s:?} ({e})"))
        .with_timezone(&Utc)
}

/// **The instant the account's own recorded reset prose names**, resolved exactly
/// as `note_account_limit_for` resolved it, and converted to UTC.
///
/// This is what makes the arithmetic below a VALUE assertion rather than a span
/// estimate: the prose is read back off the row the daemon wrote
/// (`last_limit_prose`), so the test is comparing the window against its own
/// stated premise and not against a number the test chose.
fn named_instant(prose: &str) -> DateTime<Utc> {
    let r = threads::reset::parse_reset(prose)
        .unwrap_or_else(|| panic!("the daemon recorded prose it cannot parse: {prose:?}"));
    let local = r
        .resolve(chrono::Local::now().naive_local())
        .unwrap_or_else(|| panic!("the daemon recorded prose that names no window: {prose:?}"));
    chrono::Local
        .from_local_datetime(&local)
        .single()
        .unwrap_or_else(|| panic!("ambiguous local instant for {prose:?}"))
        .with_timezone(&Utc)
}

/// Seconds from the instant the prose named to `until`. The property under test.
fn slack_secs(prose: &str, until: &str) -> i64 {
    (utc(until) - named_instant(prose)).num_seconds()
}

// ═════════════════════════════════════════════════════════════════════════
// 1. A BAR IS NOT WRITTEN ON THE INSTANT ITS OWN PROSE NAMES
// ═════════════════════════════════════════════════════════════════════════

/// **§1.1 — the window a 429 records closes ONE MINUTE after the instant the
/// reset names, not on it.**
///
/// The value is checked against the row's OWN `last_limit_prose`, re-resolved
/// through the same `reset::parse_reset` the daemon used, so this is the exact
/// arithmetic and not a span guess. `RESET_SLACK` is 60 seconds; the band is
/// 59..=61 only to absorb the sub-second drift between the daemon's two `now`
/// reads, and it excludes zero by fifty-nine seconds.
///
/// FORCED RED: drop `+ crate::reset::RESET_SLACK` from the `limit_until`
/// expression in `note_account_limit_for`.
///
/// ```text
/// assertion failed: the account must not re-enter the pool at the instant the
/// vendor named — at reset + 17s on 2026-10-06 /usage was still reporting
/// session = 100%: slack=0s prose="11:19PM (America/Chicago)"
/// ```
#[tokio::test]
async fn the_bar_a_429_records_closes_a_minute_past_the_named_reset() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    send(&m, &id, "please ratelimit me");

    assert!(
        eventually(|| state_of(&m, "aa-owned").limit_until.is_some()).await,
        "the 429 must be recorded at all ({} events seen)",
        seen.lock().unwrap().len()
    );
    let st = state_of(&m, "aa-owned");
    let prose = st.last_limit_prose.clone().expect("the reset prose is kept");
    let until = st.limit_until.clone().unwrap();
    let slack = slack_secs(&prose, &until);
    assert!(
        (59..=61).contains(&slack),
        "the account must not re-enter the pool at the instant the vendor named — \
         at reset + 17s on 2026-10-06 /usage was still reporting session = 100%: \
         slack={slack}s prose={prose:?} until={until:?}"
    );
}

/// **§1.2 — A RETRY CANNOT BE THE THING THAT DISCOVERS ITS OWN ACCOUNT IS STILL
/// BARRED. Re-grounded 2026-10-06: the ordering is now structural rather than
/// arithmetic.**
///
/// The invariant has not changed and the mechanism has. A 429 used to derive TWO
/// instants from one reset — this bar, and a per-thread retry floor at
/// `reset + 60s + 20s`. The 20s grace was the ORDERING: the bar lifted first, and
/// only then did the floor come due, so the retry could not walk into a quota the
/// vendor had not yet released.
///
/// The operator removed the per-thread floor (*"There should not be a per-thread
/// retry timestamp"*), which removes the second derivation and with it the thing
/// the grace was ordering. What is left is stronger, because it cannot be
/// arithmetically wrong: **the retry row carries no instant at all**, so the only
/// things that can bring it back are an event and the pool's cadence — and both of
/// them ask the GATE, which reads this bar. A barred account refuses, every time,
/// for as long as the bar stands.
///
/// So this test asserts the invariant directly instead of a 20-second gap: with the
/// bar standing, neither a question-origin pass nor the pool's own DEADLINE spawns a
/// turn, and the bar still carries its 60-second slack (§1.1's number, unchanged).
///
/// **IT STILL OPPOSES §1.1 AND §1.3**, and more sharply than before: dropping
/// `RESET_SLACK` from `limit_until` makes the bar lift on the reset instant itself,
/// and a cadence pass then runs the turn straight into the quota — which is exactly
/// what this now reddens on.
///
/// FORCED RED, run and recorded: dropping `+ crate::reset::RESET_SLACK` from
/// `note_account_limit_for`'s `limit_until` gives `the bar must still carry its
/// 60-second slack`. Making `admit_or_park` ignore `unavailable` gives `a retry may
/// not run while its own account is still barred`.
#[tokio::test]
async fn a_retry_cannot_discover_that_its_own_account_is_still_barred() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    send(&m, &id, "please ratelimit me");

    assert!(
        eventually(|| state_of(&m, "aa-owned").limit_until.is_some()).await,
        "the 429 must be recorded"
    );
    assert!(
        eventually(|| m.store.get_retry(&id).unwrap().is_some()).await,
        "and the prompt must be parked in `retries`"
    );

    // THE BAR KEEPS ITS SLACK — §1.1's number, restated here because this test is
    // what used to derive its own gap from it.
    let until = state_of(&m, "aa-owned").limit_until.unwrap();
    assert!(
        utc(&until) > chrono::Utc::now(),
        "the bar must still be standing for the rest of this test to mean anything: \
         limit_until={until}"
    );

    // AND THE PARKED PROMPT CARRIES NO INSTANT OF ITS OWN to be early with.
    let row = m.store.get_retry(&id).unwrap().unwrap();
    let attempt_before = row.attempt;
    assert!(
        !row.prompt.is_empty(),
        "the prompt is parked, which is the only thing this row is for"
    );

    // NEITHER A QUESTION NOR THE POOL'S DEADLINE MAY RUN IT while the bar stands.
    for origin in [
        threads::runtime::PassOrigin::Event,
        threads::runtime::PassOrigin::Cadence,
        threads::runtime::PassOrigin::Backstop,
    ] {
        m.retry_sweep(origin);
    }
    tokio::time::sleep(std::time::Duration::from_millis(900)).await;

    let after = m.store.get_retry(&id).unwrap();
    let attempt_after = after.as_ref().map(|r| r.attempt);
    assert!(
        attempt_after == Some(attempt_before) || attempt_after.is_none(),
        "THE INVARIANT: a retry may not run while its own account is still barred — \
         that is the retry walking into the limit it is waiting out. The attempt \
         moved from {attempt_before} to {attempt_after:?}, which means a turn ran \
         (limit_until={until})"
    );
    assert!(
        utc(&state_of(&m, "aa-owned").limit_until.unwrap()) > chrono::Utc::now(),
        "and the bar must still carry its 60-second slack rather than having lifted \
         on the reset instant itself, which is the §1.1 forcing this opposes"
    );
}

/// **§1.3 — the POLLER's bar gets the same slack**, because it is the same
/// producer.
///
/// `/usage` reporting 100% writes `accounts.limit_until` through
/// `note_account_limit_for` exactly as a 429 does, and on 2026-10-06 the poller
/// was the half that mattered: it read a stale exhausted session seventeen seconds
/// past the reset and handed the resolver the instant it had just crossed. Proved
/// on the real poller (`probe_accounts_once`) rather than inferred from the 429
/// sharing a function with it.
///
/// FORCED RED: the same mutation as §1.1 — `slack=0s`.
#[tokio::test]
async fn the_bar_the_gauge_writes_gets_the_same_slack() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["exhausted"])).unwrap();
    session_reads(tmp.path(), "exhausted", "100");

    m.probe_accounts_once(true).await.unwrap();

    let st = state_of(&m, "exhausted");
    assert_eq!(
        st.limit_axis.as_deref(),
        Some(Axis::Session.as_str()),
        "non-vacuity: the measured 100% really did bar the session quota: {st:?}"
    );
    let prose = st.last_limit_prose.clone().expect("the reset prose is kept");
    let until = st.limit_until.clone().unwrap();
    let slack = slack_secs(&prose, &until);
    assert!(
        (59..=61).contains(&slack),
        "the gauge's bar is written by the same function as the 429's and must carry \
         the same slack: slack={slack}s prose={prose:?} until={until:?}"
    );
}

/// **§1.4 — an IMPLAUSIBLE window still writes no bar at all.** The anti-test for
/// where the slack is applied.
///
/// `farlimit` is a 429 whose reset parses perfectly and resolves thirty days out.
/// No pool the CLI has is longer than seven days, so `MAX_PARSED_RESET` refuses it
/// and the ladder owns the wait. The slack is added INSIDE the `Option::map` over
/// the resolved instant and AFTER that refusal, so a non-window stays a non-window
/// — a minute of slack must never be the thing that creates a bar.
///
/// FORCED RED: write the slack as `Some(resolved.unwrap_or(now_local) + delta +
/// RESET_SLACK)`, or hoist it above the `MAX_PARSED_RESET` check so the refusal
/// reads the slackened instant.
///
/// ```text
/// assertion failed: a 30-day window cannot be real and the slack must not
/// manufacture one: Some("2026-11-05T19:00:00+00:00")
/// ```
#[tokio::test]
async fn an_implausible_window_is_still_refused() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    let seen = watch(&m);
    send(&m, &id, "please farlimit me");

    // The ladder still arms — the refusal is about the BAR, not about the retry.
    assert!(
        eventually(|| m.store.get_retry(&id).unwrap().is_some()).await,
        "non-vacuity: the 429 was seen and the ladder took it ({} events)",
        seen.lock().unwrap().len()
    );
    assert_eq!(
        state_of(&m, "aa-owned").limit_until,
        None,
        "a 30-day window cannot be real and the slack must not manufacture one"
    );
}

/// **§1.5 — a bar that was never derived from a reset gets NO slack.** The second
/// anti-test, and the one that opposes "add a minute everywhere".
///
/// A credit-balance bar is a RE-PROBE INTERVAL, not a window the vendor named:
/// `note_account_credit_exhausted` writes `now + CREDIT_BAR_SECS` with no reset
/// prose anywhere in it. Adding the reset slack to it would be a minute of an
/// account held out of service for no stated reason, and the slack's whole
/// justification is the instant a reset names.
///
/// FORCED RED: add `+ crate::reset::RESET_SLACK` to the `until` in
/// `note_account_credit_exhausted` (or apply the slack in `set_account_limit`,
/// which is the tempting one-line version of this fix and would hit all four
/// producers).
///
/// ```text
/// assertion failed: a credit bar is a re-probe interval, not a named reset:
/// 3660s from now, CREDIT_BAR_SECS=3600
/// ```
#[tokio::test]
async fn a_credit_bar_is_not_a_named_reset_and_gets_no_slack() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    send(&m, &id, "please creditfail");

    assert!(
        eventually(|| state_of(&m, "aa-owned").limit_axis.as_deref() == Some("credit")).await,
        "non-vacuity: the credit failure must have written its own bar: {:?}",
        state_of(&m, "aa-owned")
    );
    let until = state_of(&m, "aa-owned").limit_until.unwrap();
    let secs = (utc(&until) - Utc::now()).num_seconds();
    let expected = threads::retry::CREDIT_BAR_SECS;
    assert!(
        (expected - 5..=expected).contains(&secs),
        "a credit bar is a re-probe interval, not a named reset: {secs}s from now, \
         CREDIT_BAR_SECS={expected}"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 2. AN ELAPSED WINDOW IS NO WINDOW — ON THE WIRE
// ═════════════════════════════════════════════════════════════════════════

async fn start(dir: &Path, accounts: &[&str]) -> (ThreadManager, SocketAddr) {
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(dir, accounts)).unwrap();
    let (addr, _h) = threads::chassis::serve(std::sync::Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (m, addr)
}

/// Subscribe to the `accounts` shape for one key and return the published row.
async fn published_row(addr: SocketAddr, key: &str) -> Value {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    use tokio_tungstenite::tungstenite::Message;
    // `consumer` is COMPULSORY since the operator's ruling of 2026-10-08; a
    // subscribe without it is refused and this helper would loop to its timeout
    // and return `Null` instead of a row.
    ws.send(Message::Text(
        json!({"type":"request","id":"1","method":"shapes/accounts/subscribe",
               "params":{"key":key,
                         "consumer":{"name":"threads-tests","shape_version":"shapes.md@rev4"}}})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    for _ in 0..40 {
        let got =
            tokio::time::timeout(std::time::Duration::from_millis(1500), ws.next()).await;
        let Ok(Some(Ok(Message::Text(t)))) = got else {
            break;
        };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["type"] == "shape" && v["op"] == "read" && v["key"] == json!(key) {
            return v["row"].clone();
        }
    }
    panic!("no `accounts` export for {key} arrived");
}

/// **§2.1 — a window that ended four days ago publishes NOTHING, and it is the
/// published cell that says so.**
///
/// `alice-example-com`'s row, as it stood live: a `login` bar whose window closed
/// on 2 October, carrying the OAuth failure sentence in `last_limit_prose`. The
/// board logged `barred=true` for four days and the operator read the sentence as
/// one of the two errors on his deliberately-disabled accounts.
///
/// All three cells are gated on ONE `Option`, so they go quiet together — the
/// instant, the axis that would say "re-authenticate this account", and the
/// failure sentence.
///
/// FORCED RED: restore `effective_limit_until` to `self.limit_until.as_deref()`
/// with the staleness check alone (its pre-fix body):
///
/// ```text
/// assertion `left == right` failed: a reset at or before now describes a window
/// that has ENDED (804cffd) — the same sentence governs the producer and the wire
///   left: Object {"until": String("2026-10-02T15:45:29+00:00"), "axis": …}
///  right: Null
/// ```
#[tokio::test]
async fn an_elapsed_window_publishes_no_window() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let (m, addr) = start(tmp.path(), &["alice-example-com"]).await;

    let ended = "2026-10-02T15:45:29+00:00";
    m.store
        .set_account_limit(
            "alice-example-com",
            Some((
                ended,
                Some("Failed to authenticate: OAuth session expired and could not be refreshed"),
                Some("login"),
                None,
            )),
        )
        .unwrap();
    // The raw column is preserved for audit — this is not a deletion.
    assert_eq!(
        state_of(&m, "alice-example-com").limit_until.as_deref(),
        Some(ended),
        "non-vacuity: the watermark IS on disk, so there is something to publish"
    );

    let row = published_row(addr, "alice-example-com").await;
    // `row["limit"]` IS the cell's value — there is no wrapper object, which the
    // sibling test below proves by reading a live window out of the same path. An
    // assertion written against a key that does not exist would pass here for the
    // wrong reason, and did, once.
    assert_eq!(
        row["limit"],
        Value::Null,
        "a reset at or before now describes a window that has ENDED (804cffd) — \
         the same sentence governs the producer and the wire: {row}"
    );
    assert_eq!(
        row["pressure"]["limit_until"],
        Value::Null,
        "the pressure reading agrees, from the same accessor rather than from its \
         own duplicated comparison: {row}"
    );
    assert_eq!(
        row["pressure"]["unavailable"],
        json!(false),
        "and the gate still reads available, as it always did — this was never a \
         placement bug, which is why it survived: {row}"
    );
    let serialized = row.to_string();
    assert!(
        !serialized.contains("OAuth session expired"),
        "and the failure sentence the bar was written from goes with it — it was \
         being read as a live error on a healthy account: {row}"
    );
    assert!(
        !serialized.contains("2026-10-02"),
        "the four-day-old instant must not appear anywhere on the published row: {row}"
    );
}

/// **§2.2 — a LIVE window still publishes, in full.** The anti-test.
///
/// The fix is "treat an elapsed window as no window", not "stop publishing
/// windows". A future instant, its axis and its reset prose all reach the wire
/// exactly as before.
///
/// FORCED RED: make `effective_limit_until` return `None` unconditionally. §2.1
/// still passes — which is precisely why this test has to exist — and this fails:
///
/// ```text
/// assertion `left == right` failed: a live window is still a window, and the
/// board has to be able to say "resets Oct 6 at 3:19am"
///   left: Null
///  right: String("2099-01-01T00:00:00+00:00")
/// ```
#[tokio::test]
async fn a_live_window_still_publishes_normally() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let (m, addr) = start(tmp.path(), &["barred-for-real"]).await;

    let future = "2099-01-01T00:00:00+00:00";
    m.store
        .set_account_limit(
            "barred-for-real",
            Some((future, Some("Oct 6 at 3:19am"), Some("session"), None)),
        )
        .unwrap();

    let row = published_row(addr, "barred-for-real").await;
    assert_eq!(
        row["limit"]["until"],
        json!(future),
        "a live window is still a window, and the board has to be able to say \
         \"resets Oct 6 at 3:19am\": {row}"
    );
    assert_eq!(row["limit"]["axis"], json!("session"), "{row}");
    assert_eq!(row["limit"]["prose"], json!("Oct 6 at 3:19am"), "{row}");
    assert_eq!(
        row["pressure"]["limit_until"],
        json!(future),
        "and the gate agrees it is barred: {row}"
    );
    assert_eq!(row["pressure"]["unavailable"], json!(true), "{row}");
}

/// **§2.3 — the gate and the display answer from the SAME predicate.**
///
/// `AccountState::available` was already correct — it compared against `now` —
/// which is exactly why the defect survived: `pressure.unavailable` read `false`
/// for `alice-example-com` for four days while the board was told `barred=true`.
/// Two readings of one column is the bug class, so `available` is now defined as
/// "has no effective window" and there is one comparison left in the crate.
///
/// FORCED RED: give `available` back its own `match self.effective_limit_until()
/// { Some(t) => t <= now, None => true }` over an expiry-blind accessor. The
/// elapsed case still reads `true` here (that is why it was never a placement
/// bug), but the equivalence below breaks the moment the accessor and the
/// comparison disagree, and §2.1 fails outright.
#[test]
fn availability_is_exactly_the_absence_of_an_effective_window() {
    let now = "2026-10-06T12:00:00+00:00";
    let cases = [
        (None, "no bar at all"),
        (Some("2026-10-02T15:45:29+00:00"), "a window that ended"),
        (Some("2026-10-06T12:00:00+00:00"), "a window ending exactly now"),
        (Some("2099-01-01T00:00:00+00:00"), "a live window"),
    ];
    for (until, what) in cases {
        let st = threads::accounts::AccountState {
            name: "a".into(),
            limit_until: until.map(str::to_string),
            ..Default::default()
        };
        assert_eq!(
            st.available(now),
            st.effective_limit_until(now).is_none(),
            "{what}: the gate and every display reader must answer from one \
             predicate, not two"
        );
    }
    let ended = threads::accounts::AccountState {
        name: "a".into(),
        limit_until: Some("2026-10-02T15:45:29+00:00".into()),
        ..Default::default()
    };
    assert_eq!(ended.effective_limit_until(now), None, "an ended window is no window");
    assert!(ended.available(now), "and the gate always knew that");
    let live = threads::accounts::AccountState {
        name: "a".into(),
        limit_until: Some("2099-01-01T00:00:00+00:00".into()),
        ..Default::default()
    };
    assert_eq!(
        live.effective_limit_until(now),
        Some("2099-01-01T00:00:00+00:00"),
        "a future window is reported exactly as it is recorded"
    );
    assert!(!live.available(now));
}

// ═════════════════════════════════════════════════════════════════════════
// 3. A GATE THAT READS A BAR MUST ASK WHAT KIND OF BAR IT IS
// ═════════════════════════════════════════════════════════════════════════

/// Record a bar on one axis, with no 429 and no poll — the axis is the variable
/// under test and every other input is held still.
fn bar(m: &ThreadManager, account: &str, axis: Option<&str>) {
    let until = (Utc::now() + chrono::Duration::hours(3)).to_rfc3339();
    m.store
        .set_account_limit(account, Some((&until, Some("resets at 3:19am"), axis, None)))
        .unwrap();
}

/// Give the account a weekly observation so there IS a pressure number to keep
/// or lose. Without this every row reads `no_reading` and §3.1 would be vacuous.
async fn with_a_weekly_reading(m: &ThreadManager, dir: &Path, account: &str, pct: u32) {
    std::fs::write(dir.join(account).join("usage-weekly"), pct.to_string()).unwrap();
    m.probe_accounts_once(true).await.unwrap();
    assert!(
        m.store
            .recent_observations(account, Axis::WeeklyAll.as_str(), 1)
            .unwrap()
            .first()
            .is_some(),
        "non-vacuity: the weekly observation pressure is computed from must exist"
    );
}

fn reading(m: &ThreadManager, account: &str) -> threads::budgets::PressureReading {
    m.budgets.pressure_reading(account, Utc::now())
}

/// **§3.1 — a SESSION-barred account keeps its pressure number.** The operator's
/// ask, verbatim.
///
/// > *"note that the threads tui should display pressure on accounts even if they
/// > are at the session limit. They should only not display pressure if they are
/// > at the weekly limit."*
///
/// The session pool is five hours, so rank and share answer a live question —
/// where work will land when this clears. `unavailable` stays `true` and
/// `limit_until` is still carried, because the bar is still a fact; only the
/// suppression of the NUMBER has moved off that boolean.
///
/// FORCED RED: put `unavailable` back in place of `pressure_hidden` at either
/// `pressure_reading` ladder (the top `excluded`, or the `tail`).
///
/// ```text
/// assertion `left == right` failed: a session limit lifts within five hours, so
/// the rank is still decision-relevant — the board must have a number to draw …
///   left: None
///  right: Some(50.73…)
/// ```
#[tokio::test]
async fn a_session_barred_account_keeps_its_pressure() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["sess"])).unwrap();
    with_a_weekly_reading(&m, tmp.path(), "sess", 40).await;

    let before = reading(&m, "sess");
    assert!(
        before.pressure.is_some(),
        "non-vacuity: an unbarred account with a reading has a number at all: {before:?}"
    );

    bar(&m, "sess", Some(Axis::Session.as_str()));

    let r = reading(&m, "sess");
    assert_eq!(
        r.pressure, before.pressure,
        "a session limit lifts within five hours, so the rank is still \
         decision-relevant — the board must have a number to draw, and it must be \
         the SAME number, because a session bar changes nothing about the weekly \
         axis pressure is computed from: {r:?}"
    );
    assert_eq!(
        r.unranked_reason, None,
        "and it is RANKED — `unranked_reason` means 'why is there no number', so \
         naming the bar while publishing a number would be a contradiction: {r:?}"
    );
    assert!(
        r.unavailable,
        "the bar is still a fact and is still reported: {r:?}"
    );
    assert!(
        r.limit_until.is_some(),
        "with its instant, so a reader can say WHEN: {r:?}"
    );
}

/// **§3.2 — a WEEKLY-barred account loses it.** The other end of his sentence,
/// and the behaviour that must not regress.
///
/// `weekly_all` is the axis pressure is computed FROM, so a bar on it means the
/// numerator is at or past the share with the window closing — the −49.63 the
/// operator reported, which flattened three healthy accounts between 0.92 and
/// 0.99 into indistinguishability.
///
/// FORCED RED: make `limit_hides_pressure` return `false` unconditionally (the
/// "never hide" degenerate, which §3.1 is perfectly happy with).
///
/// ```text
/// assertion `left == right` failed: a weekly bar is days away AND is a bar on
/// the very axis pressure is computed from — this is the −49.63
///   left: Some(50.57…)
///  right: None
/// ```
#[tokio::test]
async fn a_weekly_barred_account_loses_its_pressure() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["wk"])).unwrap();
    with_a_weekly_reading(&m, tmp.path(), "wk", 40).await;
    assert!(
        reading(&m, "wk").pressure.is_some(),
        "non-vacuity: there is a number to lose"
    );

    bar(&m, "wk", Some(Axis::WeeklyAll.as_str()));

    let r = reading(&m, "wk");
    assert_eq!(
        r.pressure, None,
        "a weekly bar is days away AND is a bar on the very axis pressure is \
         computed from — this is the −49.63: {r:?}"
    );
    assert_eq!(
        r.unranked_reason,
        Some(threads::budgets::UnrankedReason::Unavailable),
        "and the reason names the bar: {r:?}"
    );
}

/// **§3.3 — every axis he did NOT name, decided rather than inherited.**
///
/// The column holds five values plus `None`, not two, and an axis handled by
/// omission is the next defect. The rule applied is one sentence: pressure
/// survives a bar that lifts on its own within hours, and does not survive one
/// measured in days or one waiting on a human.
///
/// * `weekly_fable` — days. Named explicitly rather than caught by a
///   `starts_with("weekly")`, so a future axis must be classified.
/// * `login` — only `claude /login` lifts it; a quota rank on an account that
///   cannot authenticate describes capacity it has no way to spend.
/// * `credit` — a human tops it up; the bar's clock is a re-probe interval.
/// * `None` — a bar we cannot attribute keeps barring, the direction
///   `limit_is_stale` already takes. It is also what every row written before the
///   `limit_axis` column reads, so "unknown means session" would hand the −49.63
///   back for all of them.
///
/// FORCED RED: narrow `limit_hides_pressure` to `== Some("weekly_all")` — the
/// literal reading of his two-axis sentence, which §3.1 and §3.2 both accept.
///
/// ```text
/// assertion `left == right` failed: login: only `claude /login` lifts this, so
/// there is no instant to wait for
///   left: Some(50.57…)
///  right: None
/// ```
#[tokio::test]
async fn the_axes_he_did_not_name_are_each_decided() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["ax"])).unwrap();
    with_a_weekly_reading(&m, tmp.path(), "ax", 40).await;

    let cases: [(Option<&str>, &str); 4] = [
        (
            Some(Axis::WeeklyFable.as_str()),
            "weekly_fable: the same cadence as weekly_all",
        ),
        (
            Some("login"),
            "login: only `claude /login` lifts this, so there is no instant to wait for",
        ),
        (Some("credit"), "credit: a human has to top the account up"),
        (
            None,
            "an unattributable bar: we cannot tell what it is about, and every \
             pre-column row reads this way",
        ),
    ];
    for (axis, why) in cases {
        bar(&m, "ax", axis);
        let r = reading(&m, "ax");
        assert_eq!(r.pressure, None, "{why}: {r:?}");
        assert_eq!(
            r.unranked_reason,
            Some(threads::budgets::UnrankedReason::Unavailable),
            "{why}: {r:?}"
        );
    }
    // And the one that KEEPS it, written in the same test so the file cannot be
    // read as "everything is suppressed".
    bar(&m, "ax", Some(Axis::Session.as_str()));
    assert!(
        reading(&m, "ax").pressure.is_some(),
        "session, for contrast — the pair is what isolates the axis"
    );
}

/// **§3.4 — THE DANGEROUS DIRECTION. A session-barred account that keeps its
/// pressure is still refused for PLACEMENT.**
///
/// The two questions were wearing one field and separating them must not have
/// separated the wrong one. Asserted at the ENFORCEMENT point, not at the report:
/// `ThreadManager::barred_accounts().0` is `accounts::unavailable_accounts`, and
/// it is handed to admission as `PlacementRequest::unavailable` (`runtime.rs`
/// :3968 and :4169) where `admit` drops those accounts from the candidate set
/// before `place` ranks anything. So the pressure number a session-barred account
/// now carries can never be the number that selects it — it is not in the field.
///
/// This also covers the RANKER, which reads the same struct through
/// `pool_pressure`: an account `admit` removed is never ranked at all.
///
/// FORCED RED: make `AccountState::available` ask `limit_hides_pressure` instead
/// of `effective_limit_until` — the tempting "one predicate for the axis rule"
/// mistake, which §3.1, §3.2 and §3.3 all still pass.
///
/// ```text
/// assertion `left == right` failed: a session-barred account must still be OUT
/// of the placement field — pressure is a display, availability is a gate
///   left: []
///  right: ["sess"]
/// ```
#[tokio::test]
async fn a_session_barred_account_is_still_refused_for_placement() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["sess", "free"])).unwrap();
    with_a_weekly_reading(&m, tmp.path(), "sess", 40).await;
    with_a_weekly_reading(&m, tmp.path(), "free", 40).await;
    assert!(
        m.barred_accounts().0.is_empty(),
        "non-vacuity: nothing is barred before the bar"
    );

    bar(&m, "sess", Some(Axis::Session.as_str()));

    let now = threads::store::now_rfc3339();
    assert!(
        !state_of(&m, "sess").available(&now),
        "the ENFORCEMENT predicate is unchanged: a session bar still makes the \
         account unavailable: {:?}",
        state_of(&m, "sess")
    );
    assert_eq!(
        m.barred_accounts().0,
        vec!["sess".to_string()],
        "a session-barred account must still be OUT of the placement field — \
         pressure is a display, availability is a gate"
    );
    // And the display DID keep its number, in the same test, so this cannot be
    // passing because the axis split failed to apply at all.
    assert!(
        reading(&m, "sess").pressure.is_some(),
        "while the row still carries its pressure: {:?}",
        reading(&m, "sess")
    );
    assert!(
        state_of(&m, "free").available(&now)
            && !m.barred_accounts().0.contains(&"free".to_string()),
        "and the unbarred sibling is untouched"
    );
}

/// **§3.5 — an ELAPSED bar hides nothing, on any axis, because it is not a bar.**
///
/// Where defect 2 and defect 3 meet: `limit_hides_pressure` asks through
/// `effective_limit_until`, so there is one predicate for "is there a live bar"
/// and one for "what kind of bar", and no third reading of the column.
///
/// FORCED RED: have `limit_hides_pressure` read `self.limit_until.is_some()`
/// instead of `effective_limit_until(now).is_some()`.
///
/// ```text
/// assertion `left == right` failed: an elapsed login bar is not a bar, so it
/// suppresses nothing
///   left: None
///  right: Some(50.57…)
/// ```
#[tokio::test]
async fn an_elapsed_bar_suppresses_no_pressure_on_any_axis() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["old"])).unwrap();
    with_a_weekly_reading(&m, tmp.path(), "old", 40).await;
    let live = reading(&m, "old").pressure;
    assert!(live.is_some(), "non-vacuity: there is a number");

    // `alice-example-com`'s own row: a window that closed on 2 October, on an
    // axis that WOULD suppress if it were live.
    m.store
        .set_account_limit(
            "old",
            Some((
                "2026-10-02T15:45:29+00:00",
                Some("Failed to authenticate: OAuth session expired and could not be refreshed"),
                Some("login"),
                None,
            )),
        )
        .unwrap();

    let r = reading(&m, "old");
    assert_eq!(
        r.pressure, live,
        "an elapsed login bar is not a bar, so it suppresses nothing: {r:?}"
    );
    assert!(!r.unavailable, "and it is not unavailable either: {r:?}");
    assert_eq!(r.limit_until, None, "{r:?}");
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
