//! **WHY THE HIGHER-PRESSURE ACCOUNT LOSES EVERY PLACEMENT, WITH HIS OWN
//! NUMBERS.**
//!
//! The operator, 2026-09-12 (thread `657a9a0d`, msg #541): every thread he had
//! started for hours landed on `owner@example.com` while `owner@example.ai`
//! carried the higher pressure — the two accounts at **exactly the same weekly
//! consumption**, example's window resetting in **14 hours** and gmail's in
//! **1 day 21 hours**. A shorter window is a larger multiplier on pressure, so
//! example should have been taking all of it.
//!
//! **His arithmetic is right and the daemon agrees with it.** Read live off the
//! running daemon at 2026-09-12T03:56:47Z, `accounts.pressure` returned
//! example `2.6300` at rank 1 and gmail `0.6673` at rank 2. The pressure
//! term is not broken, is not stale, and is not excluding anything.
//!
//! **Pressure is criterion THREE and the decision is over at criterion ONE.**
//! `Budgets::place` narrows on expected deaths first (`DEATHS_DEADBAND`, a
//! hundredth of one thread), then the landing cap, and only then pressure.
//! `narrow_min` REMOVES every candidate outside the deaths band, so an account
//! that loses there is gone from `front` before the pressure line is reached.
//!
//! With his live readings the deaths gap is **1.03 whole threads** — a hundred
//! times the deadband — and it runs the other way from pressure:
//!
//! | account    | share | weekly_all | fable | reach (min axis) | deaths | pressure |
//! |------------|-------|------------|-------|------------------|--------|----------|
//! | example | 1.00  | 78%        | 51%   |  50,649,098 tok  |  4.65  |  2.635   |
//! | gmail      | 0.95  | 78%        | 46%   |  62,505,192 tok  |  3.62  |  0.628   |
//!
//! The binding axis on BOTH accounts is `weekly_fable` — the axis pressure does
//! not look at, since `pool_pressure` reads `weekly_all` alone. So the fleet is
//! placed by an axis the rank is blind to, and the rank never gets a vote.
//!
//! **The threshold is the turn size, and it is measurable.** `expected_deaths`
//! is `threads_at_risk · exp(−reach ÷ tokens_per_turn)`. When a turn is small
//! against the reach both exponentials underflow to zero, the deaths tie inside
//! the deadband, and pressure decides — which is exactly what happened at
//! 2026-09-11T22:25:59Z, the ONE moment in the window when the ranker moved a
//! thread ONTO example, on a thread whose demand estimate was 7.7M tokens.
//! His coordinator threads run 20–60M tokens a turn with a dozen subagents, so
//! for them the exponential is in its sensitive region and deaths always win.
//!
//! These tests PIN THAT BEHAVIOUR rather than judging it. Whether deaths or
//! pressure should own this decision is the operator's call, not this file's.
//! What this file guarantees is that the answer stops being invisible: change
//! the layering and these tests speak up.
//!
//! Every test drives the real crate against an in-memory store. No claude
//! process is spawned, no login is read, and nothing here touches the
//! operator's database or the running daemon.

use threads::budgets::{Budgets, Demand, PlacementRequest, Reach, Thresholds};
use threads::gauge::Axis;
use threads::store::{EstimateRow, Store};

const BROOM: &str = "owner-account";
const GMAIL: &str = "owner-gmail";

// ── harness ─────────────────────────────────────────────────────────────────

fn svc() -> (Budgets, Store) {
    let store = Store::open_in_memory().unwrap();
    (Budgets::new(store.clone()), store)
}

fn placeable(name: &str) -> threads::accounts::Placeable {
    threads::pool::PoolEntry::claude_account(name, &format!("/tmp/threads-pressure-vote/{name}"))
        .expect("an entry that names its login directory is a pool member")
        .placeable()
}

fn pool(names: &[&str]) -> Vec<threads::accounts::Placeable> {
    names.iter().copied().map(placeable).collect()
}

fn place(
    b: &Budgets,
    th: &Thresholds,
    candidates: &[threads::accounts::Placeable],
    d: Demand,
) -> Option<String> {
    b.place(&PlacementRequest {
        thresholds: th,
        candidates,
        prefer: None,
        sessions: &[],
        demand: d,
    })
    .unwrap()
    .map(|p| p.name().to_string())
}

/// Inject a calibration directly, fully displacing the prior, so the reach
/// these tests reason about is the number they name.
fn calibrate(store: &Store, account: &str, axis: Axis, tokens_per_pct: f64) {
    store
        .upsert_estimate(&EstimateRow {
            account: account.into(),
            axis: axis.as_str().into(),
            tokens_per_percent: tokens_per_pct,
            sample_count: 32,
            last_obs_id: 0,
            updated_at: threads::store::now_rfc3339(),
        })
        .unwrap();
}

/// **THE LIVE SAMPLE**, transcribed from his store at 2026-09-12T03:56Z:
/// `usage_observations` (newest row per account per axis), `pool_shares`, and
/// `limit_estimates.tokens_per_percent`.
///
/// The two weekly reset instants are expressed as offsets from `now` so the
/// fixture keeps meaning the same thing tomorrow: example's window has 14
/// hours left, gmail's has 45, which is his "14 hours" against his "1 day 21
/// hours" to the minute.
fn his_two_accounts() -> (Budgets, Store) {
    let (b, store) = svc();
    let now = chrono::Utc::now();
    let broom_reset = (now + chrono::Duration::minutes(14 * 60 + 2)).to_rfc3339();
    let gmail_reset = (now + chrono::Duration::minutes(45 * 60 + 2)).to_rfc3339();

    // ── owner-account: NO `pool_shares` row at all, so its share is
    // `DEFAULT_POOL_SHARE` — the whole account. That is the live state; the
    // operator has never set a share on this label.
    b.observe(BROOM, Axis::Session, 0.0, None, "usage_poll").unwrap();
    b.observe(BROOM, Axis::WeeklyAll, 78.0, Some(broom_reset.clone()), "usage_poll").unwrap();
    b.observe(BROOM, Axis::WeeklyFable, 51.0, Some(broom_reset), "usage_poll").unwrap();
    calibrate(&store, BROOM, Axis::Session, 3_213_298.586_432_56);
    calibrate(&store, BROOM, Axis::WeeklyAll, 21_835_802.846_671_9);
    calibrate(&store, BROOM, Axis::WeeklyFable, 1_033_655.060_657_24);

    // ── owner-gmail: share 0.95, set by hand on 2026-09-02.
    store.set_pool_share(GMAIL, 0.95, None).unwrap();
    b.observe(GMAIL, Axis::Session, 23.0, None, "usage_poll").unwrap();
    b.observe(GMAIL, Axis::WeeklyAll, 78.0, Some(gmail_reset.clone()), "usage_poll").unwrap();
    b.observe(GMAIL, Axis::WeeklyFable, 46.0, Some(gmail_reset), "usage_poll").unwrap();
    calibrate(&store, GMAIL, Axis::Session, 2_456_873.946_423_13);
    calibrate(&store, GMAIL, Axis::WeeklyAll, 16_271_691.873_660_2);
    calibrate(&store, GMAIL, Axis::WeeklyFable, 1_157_503.550_980_74);

    (b, store)
}

/// The thresholds every AUI thread carries: `{"weekly": 95.0}` off
/// `aui:default`, nothing on session, nothing on fable.
fn aui_thresholds() -> Thresholds {
    Thresholds {
        session: None,
        fable: None,
        weekly: Some(95.0),
        cash: None,
    }
}

// ── his premise, confirmed against the real arithmetic ──────────────────────

/// **HE IS RIGHT ABOUT PRESSURE.** Example carries roughly four times
/// gmail's, and the gap is more than twenty times `PRESSURE_DEADBAND` — there
/// is no reading of the rank in which this is a tie.
#[test]
fn example_carries_far_more_pressure_than_gmail() {
    let (b, _store) = his_two_accounts();
    let now = chrono::Utc::now();
    let broom = b.pressure_reading(BROOM, now);
    let gmail = b.pressure_reading(GMAIL, now);

    let (bp, gp) = (
        broom.pressure.expect("example is ranked"),
        gmail.pressure.expect("gmail is ranked"),
    );
    // R9.PR1: example lends its whole account, so its cap is 0.95, not 1.0
    // — live 2.63 on the old formula is (0.17/0.95)/0.0837 ≈ 2.14 now. gmail's
    // 0.95 share is unaffected.
    assert!(
        bp > 2.0 && bp < 2.3,
        "example's pressure under the 0.95 cap is ~2.14; got {bp}"
    );
    assert!(
        gp > 0.55 && gp < 0.75,
        "gmail's live pressure was 0.667; got {gp}"
    );
    assert!(
        bp - gp > 1.4,
        "the rank is not close: {bp} vs {gp} — pressure alone would place \
         every turn on example"
    );
    assert!(
        broom.unranked_reason.is_none() && gmail.unranked_reason.is_none(),
        "neither account is excluded, barred, stale or disabled: {broom:?} / {gmail:?}"
    );
}

/// **AND THE BINDING AXIS IS ONE PRESSURE CANNOT SEE.** Reach is the min across
/// session, weekly-all and weekly-fable; on both accounts the minimum is the
/// FABLE axis, while `pool_pressure` divides the WEEKLY-ALL axis alone.
#[test]
fn the_tightest_axis_on_both_accounts_is_the_one_pressure_does_not_read() {
    let (b, _store) = his_two_accounts();
    let th = aui_thresholds();

    let broom = match b.reachable_tokens(BROOM, &th) {
        Reach::Known(t) => t,
        other => panic!("example is measured on every axis: {other:?}"),
    };
    let gmail = match b.reachable_tokens(GMAIL, &th) {
        Reach::Known(t) => t,
        other => panic!("gmail is measured on every axis: {other:?}"),
    };

    // 49 points of fable × 1,033,655 tokens/point, and 54 × 1,157,504.
    assert!(
        (broom - 50_649_098.0).abs() < 1.0,
        "example's reach is its fable axis: {broom}"
    );
    assert!(
        (gmail - 62_505_192.0).abs() < 1.0,
        "gmail's reach is its fable axis: {gmail}"
    );
    assert!(
        gmail > broom,
        "the account with the LOWER pressure has the LARGER reach — that is \
         the whole inversion: {gmail} > {broom}"
    );
}

// ── the answer: criterion one ends it ───────────────────────────────────────

/// **THE OBSERVATION, REPRODUCED.** His real demand estimate for thread
/// `657a9a0d` at 03:47Z was 47,479,395 tokens a turn over a 12.52-subagent
/// fan-out. Against that demand the deaths gap is a whole thread, `narrow_min`
/// drops example, and gmail takes the turn — with pressure four to one the
/// other way and never consulted.
#[test]
fn his_coordinator_thread_lands_on_the_lower_pressure_account() {
    let (b, _store) = his_two_accounts();
    let th = aui_thresholds();
    let real = Demand {
        tokens_per_turn: 47_479_395.0,
        subagents: 12.52,
    };

    let broom_deaths = real.expected_deaths(b.reachable_tokens(BROOM, &th));
    let gmail_deaths = real.expected_deaths(b.reachable_tokens(GMAIL, &th));
    assert!(
        broom_deaths - gmail_deaths > 1.0,
        "the deaths gap is a whole thread and it favours gmail: \
         example {broom_deaths} vs gmail {gmail_deaths}"
    );
    // `DEATHS_DEADBAND` is 0.01 — a hundredth of one thread. Nothing about a
    // gap this size is absorbable.
    assert!(
        broom_deaths - gmail_deaths > 100.0 * 0.01,
        "and it is a hundred deadbands wide, so criterion one is decisive"
    );

    assert_eq!(
        place(&b, &th, &pool(&[BROOM, GMAIL]), real).as_deref(),
        Some(GMAIL),
        "THE BUG HE REPORTED: the turn goes to the account with a quarter of \
         the pressure, because expected deaths is criterion one and pressure \
         is criterion three"
    );
}

/// **THE CONTROL, AND IT IS THE PROOF.** Change ONE thing — the size of the
/// turn — and the same two accounts, at the same readings, place the other way.
///
/// A small turn makes both exponentials underflow, the deaths tie inside the
/// deadband, `narrow_min` removes nobody, and the pressure line finally runs
/// and picks example. This is the natural experiment his own store already
/// performed at 2026-09-11T22:25:59Z, when a 7.7M-token thread was moved ONTO
/// example while every larger thread was going the other way.
///
/// It is also what rules out every "something is excluding example"
/// explanation: nothing excludes an account that wins this very call.
#[test]
fn the_same_two_accounts_place_the_other_way_for_a_small_turn() {
    let (b, _store) = his_two_accounts();
    let th = aui_thresholds();
    let small = Demand {
        tokens_per_turn: 1_000_000.0,
        subagents: 0.0,
    };

    let broom_deaths = small.expected_deaths(b.reachable_tokens(BROOM, &th));
    let gmail_deaths = small.expected_deaths(b.reachable_tokens(GMAIL, &th));
    assert!(
        (broom_deaths - gmail_deaths).abs() < 0.01,
        "a small turn ties on deaths, which is what lets the rank speak: \
         {broom_deaths} vs {gmail_deaths}"
    );

    assert_eq!(
        place(&b, &th, &pool(&[BROOM, GMAIL]), small).as_deref(),
        Some(BROOM),
        "with deaths tied, pressure decides and it decides for example — \
         the criterion works, it is simply never reached for his real threads"
    );
}

/// **WHERE THE LINE ACTUALLY SITS**, so the finding is a number rather than an
/// adjective: pressure stops voting once a thread's turn grows past roughly
/// eight million tokens against this pair of accounts. His coordinator threads
/// run six times that.
#[test]
fn pressure_stops_voting_somewhere_around_eight_million_tokens_a_turn() {
    let (b, _store) = his_two_accounts();
    let th = aui_thresholds();
    let for_turn = |tokens: f64| {
        place(
            &b,
            &th,
            &pool(&[BROOM, GMAIL]),
            Demand {
                tokens_per_turn: tokens,
                // One thread, no fan-out: the LEAST discriminating case there
                // is. Any real fan-out multiplies the gap and moves this
                // boundary further down, never up.
                subagents: 0.0,
            },
        )
    };
    assert_eq!(
        for_turn(5_000_000.0).as_deref(),
        Some(BROOM),
        "below the line the rank is reached and the high-pressure account wins"
    );
    assert_eq!(
        for_turn(20_000_000.0).as_deref(),
        Some(GMAIL),
        "above it the objective is decisive and the rank is dead weight"
    );
}
