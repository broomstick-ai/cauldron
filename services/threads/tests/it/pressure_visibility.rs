//! **PRESSURE VISIBILITY — an over-share account that is QUIET leaves the
//! scale, and one that is still RUNNING does not.**
//!
//! The operator's ruling, 2026-09-22, in full:
//!
//! > *"I think it's fine as is — don't clamp, just detect when weekly > share
//! > and running == 0. The limit was passed because the last thread to run on
//! > it didn't get evicted early, which is known behavior and not an issue for
//! > now. But if those conditions I've stated are true you can assume that
//! > placement will not place there again, and you can just remove the pressure
//! > from view instead of clamping it. If it went over I do want to see
//! > negative pressure while the last thread is still running. That's honest."*
//!
//! Two conditions, ANDed, and the second one is the one most likely to be
//! optimised away by someone who reads only the first sentence. These tests
//! exist in pairs for that reason: for every "it is hidden", there is a
//! neighbour asserting "and here it is NOT".

use threads::budgets::{Budgets, UnrankedReason};
use threads::gauge::Axis;
use threads::store::Store;

/// A pool where every account carries one weekly reading: `(name, pct,
/// fraction of the window still to run)`.
fn budgets_with(accounts: &[(&str, f64, f64)], now: chrono::DateTime<chrono::Utc>) -> Budgets {
    let b = Budgets::new(Store::open_in_memory().unwrap());
    let window = Axis::WeeklyAll.window_secs() as f64;
    for (name, pct, frac_left) in accounts {
        b.observe(
            name,
            Axis::WeeklyAll,
            *pct,
            Some((now + chrono::Duration::seconds((window * frac_left) as i64)).to_rfc3339()),
            "usage_poll",
        )
        .unwrap();
    }
    b
}

/// **PUT A LIVE EXECUTION ON AN ACCOUNT** — the operator's `executing > 0`.
///
/// Two rows, because `Budgets::nothing_running_on` needs both: a
/// `running_turns` row whose pid EXISTS (this test process stands in for the
/// spawned child, since the predicate refuses to count a turn whose process is
/// gone), and a `thread_accounts` row, because `running_turns` carries no
/// account column and an unattributable turn counts against every account at
/// once.
fn set_running_on(b: &Budgets, thread_id: &str, account: &str) {
    b.store_ref()
        .upsert_running_turn(&threads::store::RunningTurnRow {
            thread_id: thread_id.into(),
            pid: std::process::id() as i64,
            spawn_cookie: "cookie".into(),
            output_path: "/dev/null".into(),
            byte_offset: 0,
            started_at: threads::store::now_rfc3339(),
            prompt: None,
            message_ids: vec![],
            attempt: 1,
        })
        .unwrap();
    b.store_ref()
        .record_thread_account(thread_id, account)
        .unwrap();
}

/// **THE −157.38 LEAVES THE BOARD ONCE THE ACCOUNT IS QUIET — AND EVERY
/// READING STAYS.**
///
/// The fixture is the operator's own: an account 10 points past a 0.9 share,
/// 427 seconds from its reset, which is the exact pair that produced the
/// −157.38 he reported. Nothing is executing on it, so the two conditions both
/// hold and the number is removed from view.
///
/// **UNRANKED IS NOT UNLISTED.** `accounts.pressure`'s contract is explicit
/// that an account which silently vanishes from this payload is how he loses
/// track of one, so the row keeps its share, its observed percent, its observed
/// instant and its reset — everything the reading gathered on the way — and
/// loses only the rate.
///
/// **THE MUTATION.** Delete the `settled_overage` arm from the tail ladder of
/// `pressure_reading` and this goes red with the divergent number back in the
/// table.
#[test]
fn an_over_share_account_with_nothing_running_leaves_the_scale() {
    let now = chrono::Utc::now();
    let window = Axis::WeeklyAll.window_secs() as f64;
    let b = budgets_with(
        &[
            ("owner-account", 19.0, 0.8),
            ("cc1", 9.0, 0.8),
            ("cc2", 46.0, 0.8),
            ("gmail", 100.0, 427.0 / window),
        ],
        now,
    );
    b.store_ref().set_pool_share("gmail", 0.9, None).unwrap();

    let r = b.pressure_reading("gmail", now);
    assert_eq!(
        r.pressure, None,
        "over its lent share with nothing running: the number describes nothing \
         anyone can act on, so it is removed from view — got {:?}",
        r.pressure
    );
    assert_eq!(
        r.unranked_reason,
        Some(UnrankedReason::OverShareSettled),
        "and it says WHY, the same way `disabled` and `unavailable` do"
    );
    assert!(r.over_share, "the FACT is still reported, on its own field");

    // EVERY READING INTACT.
    assert_eq!(r.share, 0.9, "the share he lent is still there");
    assert_eq!(r.observed_pct, Some(100.0));
    assert!(r.observed_at.is_some(), "when it was measured");
    assert!(r.resets_at.is_some(), "and when the window rolls");
    assert!(!r.disabled, "he did not disable it");
    assert!(!r.unavailable, "and nothing barred it");

    // STILL ON THE PAYLOAD, and the other three rank 1..3 between themselves.
    let table = b.pressure_table(
        &["owner-account", "cc1", "cc2", "gmail"].map(String::from),
        now,
    );
    let row = table
        .accounts
        .iter()
        .find(|r| r.account == "gmail")
        .expect("STILL LISTED: an account that vanishes from this payload is how \
                 he loses track of one");
    assert!(!row.ranked);
    assert_eq!(row.pressure, None, "NULL, never 0.0");
    assert_eq!(row.rank, None);
    assert_eq!(row.unranked_reason, Some("over_share_settled"));
    assert!(row.over_share);
    assert_eq!(table.ranked_count, 3);
    let mut ranks: Vec<usize> = table.accounts.iter().filter_map(|r| r.rank).collect();
    ranks.sort_unstable();
    assert_eq!(ranks, vec![1, 2, 3], "the healthy three have the scale to themselves");
}

/// **AND WITH A THREAD STILL RUNNING THERE, THE NEGATIVE NUMBER STAYS. THIS IS
/// THE ONE THAT MUST NOT BE SIMPLIFIED AWAY.**
///
/// > *"If it went over I do want to see negative pressure while the last thread
/// > is still running. That's honest."*
///
/// The overage exists PRECISELY BECAUSE that thread was not evicted early —
/// which the operator named as known behaviour he is not fixing today. Hiding
/// the number while the thread runs would hide a live fact about a turn that is
/// still burning share.
///
/// The fixture is identical to the one above in every respect except the
/// running turn, so the difference this asserts can only be the second
/// condition.
///
/// **THE MUTATION.** Replace `settled_overage` with a bare `out.over_share` in
/// `pressure_reading` — the cheap version, which passes the test above — and
/// this goes red.
#[test]
fn an_over_share_account_with_a_thread_still_running_keeps_its_negative_pressure() {
    let now = chrono::Utc::now();
    let window = Axis::WeeklyAll.window_secs() as f64;
    let b = budgets_with(&[("gmail", 100.0, 427.0 / window)], now);
    b.store_ref().set_pool_share("gmail", 0.9, None).unwrap();

    // The very thread the operator described: the last one to run on the
    // account, not evicted early, which is why the account went over at all.
    set_running_on(&b, "t-the-one-that-did-not-get-evicted", "gmail");

    let r = b.pressure_reading("gmail", now);
    let v = r
        .pressure
        .expect("THE NUMBER IS TRUE WHILE THE THREAD RUNS and he asked to see it");
    assert!(
        v < -100.0,
        "the honest negative, not a clamp and not a hole: got {v}"
    );
    assert_eq!(r.unranked_reason, None, "it is RANKED");
    assert!(
        r.over_share,
        "and the overage is still reported as a fact beside the number, which is \
         what lets a reader say why the number is negative"
    );

    // AND THE MOMENT IT GOES QUIET, the same account with the same readings
    // loses the number. Proved on ONE fixture so nothing else can explain the
    // difference.
    b.store_ref()
        .clear_running_turn("t-the-one-that-did-not-get-evicted")
        .unwrap();
    let quiet = b.pressure_reading("gmail", now);
    assert_eq!(quiet.pressure, None);
    assert_eq!(
        quiet.unranked_reason,
        Some(UnrankedReason::OverShareSettled)
    );
}

/// **A DEAD PID IS NOT A RUNNING THREAD.**
///
/// A `running_turns` row whose process is gone is a leftover that `recover()`
/// clears at the next daemon start. If one counted as "still executing", an
/// account would keep a divergent negative pressure on the board forever —
/// which is the defect this change is fixing, arriving by a different door.
///
/// **THE MUTATION.** Delete the `pid_alive` guard in `unsettled_since` and this
/// goes red.
#[test]
fn a_leftover_running_turn_whose_process_is_gone_does_not_keep_the_reading_alive() {
    let now = chrono::Utc::now();
    let window = Axis::WeeklyAll.window_secs() as f64;
    let b = budgets_with(&[("gmail", 100.0, 427.0 / window)], now);
    b.store_ref().set_pool_share("gmail", 0.9, None).unwrap();
    b.store_ref()
        .upsert_running_turn(&threads::store::RunningTurnRow {
            thread_id: "t-zombie".into(),
            // A pid that cannot be alive. `recover()` would clear this row; a
            // reader that trusted it would think the account is still busy.
            pid: i64::from(u32::MAX),
            spawn_cookie: "cookie".into(),
            output_path: "/dev/null".into(),
            byte_offset: 0,
            started_at: threads::store::now_rfc3339(),
            prompt: None,
            message_ids: vec![],
            attempt: 1,
        })
        .unwrap();
    b.store_ref()
        .record_thread_account("t-zombie", "gmail")
        .unwrap();

    let r = b.pressure_reading("gmail", now);
    assert_eq!(
        r.unranked_reason,
        Some(UnrankedReason::OverShareSettled),
        "a row for a process that does not exist is not work in flight"
    );
}

/// **EXACTLY AT THE SHARE IS NOT PAST IT**, and the reading there is a real
/// zero rather than a hole.
///
/// The condition is `weekly > share`, strictly — his words. At exactly the
/// share the rate is exactly `0.0`: "this account has spent precisely what it
/// was lent, with the window still to run". Nothing about that diverges and
/// nothing about it is unactionable, so it stays on the scale.
///
/// This is deliberately NOT `Budgets::over_share`, which is `>=` because it
/// gates ADMISSION, where exactly-at-the-share must already refuse. Two
/// questions, two boundaries.
///
/// **THE MUTATION.** Change `consumed > lent` to `consumed >= lent` in
/// `pressure_reading` and this goes red.
#[test]
fn exactly_at_the_lent_share_is_still_ranked_at_zero() {
    let now = chrono::Utc::now();
    let b = budgets_with(&[("edge", 90.0, 0.5)], now);
    b.store_ref().set_pool_share("edge", 0.9, None).unwrap();

    let r = b.pressure_reading("edge", now);
    assert!(
        !r.over_share,
        "90% of a 0.9 share is AT the share, not past it"
    );
    assert_eq!(
        r.pressure,
        Some(0.0),
        "and zero pressure is the busiest honest reading there is, not an absence"
    );
    assert_eq!(r.unranked_reason, None);

    // One point past it, and the same account is over and quiet.
    b.observe(
        "edge",
        Axis::WeeklyAll,
        91.0,
        Some((now + chrono::Duration::seconds((Axis::WeeklyAll.window_secs() as f64 * 0.5) as i64)).to_rfc3339()),
        "usage_poll",
    )
    .unwrap();
    let over = b.pressure_reading("edge", now);
    assert!(over.over_share);
    assert_eq!(over.unranked_reason, Some(UnrankedReason::OverShareSettled));
}

/// **PRECEDENCE: A BAR BEATS A SETTLED OVERAGE, AND A DISABLE BEATS BOTH.**
///
/// All three can be true of one account at once, and exactly one word reaches
/// the reader. The order is `Disabled` > `Unavailable` > `OverShareSettled`:
///
/// * `Disabled` is his own act and the only one he can reverse, so it is the
///   reason he can act on.
/// * `Unavailable` is the stronger of the remaining two — "cannot serve a turn
///   at all, until this instant" implies "will not be placed on again this
///   window" — and it carries `limit_until`, a WHEN, where a settled overage
///   has only a WHY.
///
/// **NO FACT IS DISCARDED.** Precedence decides which single word is SHOWN;
/// every boolean on the row stays true.
///
/// **THE MUTATION.** Reorder the tail ladder in `pressure_reading` so
/// `settled_overage` is tested before `unavailable`, and the first block goes
/// red.
#[test]
fn a_bar_outranks_a_settled_overage_and_a_disable_outranks_the_bar() {
    let now = chrono::Utc::now();
    let window = Axis::WeeklyAll.window_secs() as f64;
    let b = budgets_with(&[("gmail", 100.0, 427.0 / window)], now);
    b.store_ref().set_pool_share("gmail", 0.9, None).unwrap();
    let until = (now + chrono::Duration::minutes(13)).to_rfc3339();
    b.store_ref()
        .set_account_limit(
            "gmail",
            Some((&until, Some("weekly limit"), Some("weekly_all"), None)),
        )
        .unwrap();

    let barred = b.pressure_reading("gmail", now);
    assert_eq!(
        barred.unranked_reason,
        Some(UnrankedReason::Unavailable),
        "both are true; the bar is the one that says WHEN, so it is the one shown"
    );
    assert!(
        barred.over_share,
        "AND THE OTHER FACT IS NOT DISCARDED — precedence is about which word is \
         shown, never about throwing a reading away"
    );
    assert!(barred.unavailable);
    assert!(
        barred.limit_until.is_some(),
        "which is the whole reason the bar wins: it can say until when"
    );

    // …and the disable takes it from the bar.
    b.store_ref()
        .set_account_disabled("gmail", true, Some("test"))
        .unwrap();
    let disabled = b.pressure_reading("gmail", now);
    assert_eq!(
        disabled.unranked_reason,
        Some(UnrankedReason::Disabled),
        "the reason he can ACT on beats the two he can only wait out"
    );
    assert!(disabled.disabled);
    assert!(disabled.unavailable, "and the bar is still on the row");
}

/// **THIS IS A VISIBILITY CHANGE, NOT A CLAMP — the two things it must not
/// have touched, pinned.**
///
/// 1. **The window floor stays.** `PRESSURE_MIN_WINDOW_FRACTION` is what keeps
///    the rate finite; it is why the observed `−157.38` corresponded exactly to
///    427 s and why the worst case is near `−1120` rather than `−∞`. A second
///    clamp was explicitly rejected, and so was removing this one.
/// 2. **The symmetric POSITIVE case is left exactly as it was.** An untouched
///    account a minute from its reset reads roughly `+10,080`. The operator was
///    shown that number and accepted it — *"cute"* — and it is not a lie: it is
///    what "burn a full week's share in sixty seconds" evaluates to. Nothing
///    here may quietly fix it.
///
/// **THE MUTATION.** Widen the new rule to cover accounts that are UNDER their
/// share (drop the `out.over_share` condition, keeping only the quiet one) and
/// the positive leg goes red — which is what a careless "unrank near the reset"
/// generalisation would do.
#[test]
fn the_floor_is_untouched_and_the_positive_case_is_deliberately_left_alone() {
    let now = chrono::Utc::now();
    let window = Axis::WeeklyAll.window_secs() as f64;
    // One minute left: the floor's own value, so this is the worst case on both
    // sides of zero.
    let at_the_floor = 60.0 / window;

    // ── THE NEGATIVE SIDE, at the floor, with a thread running so the reading
    //    is reported at all.
    let b = budgets_with(&[("over", 100.0, at_the_floor)], now);
    b.store_ref().set_pool_share("over", 0.9, None).unwrap();
    set_running_on(&b, "t-live", "over");
    let v = b.pressure_reading("over", now).pressure.unwrap();
    assert!(
        v.is_finite() && v < -1000.0 && v > -1200.0,
        "the FLOOR still bounds the divergence at about −1120 — no second clamp \
         was added and the existing one was not removed: got {v}"
    );

    // ── THE POSITIVE SIDE. Untouched account, one minute to its reset, nothing
    //    running on it. Every condition of the new rule except the overage.
    let b2 = budgets_with(&[("fresh", 0.0, at_the_floor)], now);
    b2.store_ref().set_pool_share("fresh", 1.0, None).unwrap();
    // R9.PR1 (operator ruling r42) SUPERSEDES the accepted +10,080: inside the
    // final minute an account with room left reads ∞ — still ranked, still
    // unmoderated, and now said as what it is.
    let r = b2.pressure_reading("fresh", now);
    assert_eq!(
        r.value,
        Some(threads::budgets::PressureValue::Infinite),
        "an untouched account a minute from its reset is ∞ and still ranked: {r:?}"
    );
    assert!(r.pressure.is_none(), "∞ is not carried as a number: {r:?}");
    assert!(!r.over_share, "it is under its share, which is why nothing applies");
    assert_eq!(r.unranked_reason, None);
}
