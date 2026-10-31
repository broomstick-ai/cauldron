//! PER-PERIOD LIMIT-ESTIMATE HISTORY — the period BOUNDARY, specifically.
//!
//! `limit_estimates` holds one row per (account, axis) and overwrites it
//! forever, so it can say what capacity IS and never what it WAS. The
//! operator asked for the history beside it, and asked for it bucketed by the
//! limit's own reset window rather than sampled on a timer:
//!
//!   "every usage limit that we have, maybe you update our estimate for that
//!    usage limit in that period, be it session limit, be it weekly limit, be
//!    it Fable weekly limit. And then every time the update changes for that
//!    period, update it. And then when the period is over, just leave it and
//!    move forward and start recording the next one."
//!
//! That is two behaviours and exactly one seam between them, which is what
//! this file exercises:
//!
//!   1. an update INSIDE a live window MUTATES the open record, and
//!   2. the first update AFTER that window closes STARTS A NEW ONE, leaving
//!      the closed record frozen as that period's final estimate.
//!
//! Plus the two identity rules the records live or die by: the bucket is
//! keyed by the LOGIN, not by the label pointing at it, and the pointer label
//! `default` is never recorded at all.

use chrono::{Duration, Utc};

use threads::budgets::Budgets;
use threads::gauge::Axis;
use threads::store::{ObservationRow, PeriodEstimateRow, Store};

/// A store plus a `Budgets` over the SAME connection, so the test can both
/// drive the recorder and read the rows it wrote.
fn svc() -> (Store, Budgets) {
    let store = Store::open_in_memory().expect("in-memory store");
    (store.clone(), Budgets::new(store))
}

/// Register `name` as a real account carrying login fingerprint `identity`.
fn register(store: &Store, name: &str, identity: &str) {
    store
        .set_account_identity(name, Some(&format!("/tmp/threads-test/{name}")), Some(identity))
        .expect("registering an account identity");
}

/// Record the gauge reading that puts `(account, axis)` inside the window
/// ending at `resets_at`. This is the ONLY thing that tells the recorder which
/// period it is in — the recorder never invents one.
fn observe_in_window(store: &Store, account: &str, axis: Axis, pct: f64, resets_at: &str) {
    store
        .record_observation(&ObservationRow {
            id: 0,
            account: account.to_string(),
            axis: axis.as_str().to_string(),
            pct,
            resets_at: Some(resets_at.to_string()),
            source: "usage_poll".into(),
            observed_at: threads::store::now_rfc3339(),
        })
        .expect("recording an observation");
}

fn periods(store: &Store, identity: &str, axis: Axis) -> Vec<PeriodEstimateRow> {
    store
        .list_period_estimates()
        .expect("listing period estimates")
        .into_iter()
        .filter(|p| p.identity == identity && p.axis == axis.as_str())
        .collect()
}

/// **THE PERIOD BOUNDARY.** One window's worth of updates collapses into ONE
/// record that keeps moving; the next window opens a SECOND record and leaves
/// the first exactly as the window closed it.
///
/// Both halves matter and they fail in opposite directions. Without the
/// in-place update this is a timer sample — a row per poll, and no notion of a
/// period at all. Without the boundary this is a single row per (login, axis)
/// that overwrites forever, which is precisely the `limit_estimates` defect
/// this table exists to fix, reproduced one table over.
#[test]
fn a_live_window_updates_in_place_and_a_closed_one_starts_a_new_record() {
    let (store, b) = svc();
    register(&store, "acct-a", "fp-a");
    let now = Utc::now();

    // ── window 1: opens the bucket ───────────────────────────────────────
    let w1 = (now + Duration::hours(1)).to_rfc3339();
    observe_in_window(&store, "acct-a", Axis::Session, 4.0, &w1);
    b.record_period_estimate("acct-a", Axis::Session, 100_000.0, 1, now)
        .expect("recording inside a live window");

    // ── still window 1: the reset is re-reported 45 SECONDS off, which is
    // how a real gauge behaves (it reports to the minute). That is jitter,
    // not a rollover, and it must not open a second bucket.
    let w1_jittered = (now + Duration::hours(1) + Duration::seconds(45)).to_rfc3339();
    observe_in_window(&store, "acct-a", Axis::Session, 9.0, &w1_jittered);
    b.record_period_estimate("acct-a", Axis::Session, 120_000.0, 2, now)
        .expect("a second estimate in the same window");

    let live = periods(&store, "fp-a", Axis::Session);
    assert_eq!(
        live.len(),
        1,
        "two updates inside ONE window are one record, not two: {live:#?}"
    );
    assert_eq!(
        live[0].period_end, w1,
        "an in-place update keeps the bucket's ORIGINAL window instant"
    );
    assert_eq!(live[0].tokens_per_percent, 120_000.0, "the record carries the LATEST estimate");
    assert_eq!(live[0].limit_tokens, 12_000_000.0);
    assert_eq!(live[0].sample_count, 2);
    assert_eq!(live[0].updates, 2, "both writes landed on the same record");
    let frozen_first_recorded = live[0].first_recorded_at.clone();

    // ── window 2: the session pool is five hours, so the reset advancing by
    // five hours is a genuine rollover — the period is OVER.
    let w2 = (now + Duration::hours(6)).to_rfc3339();
    observe_in_window(&store, "acct-a", Axis::Session, 3.0, &w2);
    b.record_period_estimate("acct-a", Axis::Session, 150_000.0, 3, now)
        .expect("the first estimate of the next window");

    let all = periods(&store, "fp-a", Axis::Session);
    assert_eq!(
        all.len(),
        2,
        "the first update after the window closed must START A NEW RECORD, \
         not overwrite the closed one: {all:#?}"
    );

    // The closed period is FROZEN: its final estimate is what it held when
    // the window ran out, and nothing since has touched it.
    let closed = all.iter().find(|p| p.period_end == w1).expect("window 1's record survives");
    assert_eq!(
        closed.tokens_per_percent, 120_000.0,
        "a closed period keeps the estimate the window ended on"
    );
    assert_eq!(closed.updates, 2, "a frozen record takes no further writes");
    assert_eq!(closed.first_recorded_at, frozen_first_recorded);

    // The new period starts clean.
    let open = all.iter().find(|p| p.period_end == w2).expect("window 2's record exists");
    assert_eq!(open.tokens_per_percent, 150_000.0);
    assert_eq!(open.updates, 1, "a fresh period has been written exactly once");
    assert_eq!(open.sample_count, 3);
}

/// A window that has rolled on ONE axis has not rolled on the others. The
/// buckets are per (login, axis, window) and the boundaries are independent —
/// the session pool turns over roughly every five hours while a weekly pool
/// sits still through dozens of those.
#[test]
fn each_axis_carries_its_own_period_boundary() {
    let (store, b) = svc();
    register(&store, "acct-a", "fp-a");
    let now = Utc::now();

    let weekly = (now + Duration::days(3)).to_rfc3339();
    observe_in_window(&store, "acct-a", Axis::WeeklyAll, 20.0, &weekly);
    b.record_period_estimate("acct-a", Axis::WeeklyAll, 20_000_000.0, 5, now)
        .unwrap();

    // Two whole session windows come and go underneath it.
    for (hours, tpp) in [(1i64, 3_000_000.0), (6, 3_100_000.0), (11, 3_200_000.0)] {
        let r = (now + Duration::hours(hours)).to_rfc3339();
        observe_in_window(&store, "acct-a", Axis::Session, 5.0, &r);
        b.record_period_estimate("acct-a", Axis::Session, tpp, 1, now)
            .unwrap();
    }

    assert_eq!(
        periods(&store, "fp-a", Axis::Session).len(),
        3,
        "three session windows are three records"
    );
    let w = periods(&store, "fp-a", Axis::WeeklyAll);
    assert_eq!(w.len(), 1, "the weekly window never rolled: {w:#?}");
    assert_eq!(w[0].tokens_per_percent, 20_000_000.0);
}

/// **THE POINTER IS NOT AN ACCOUNT — and it is kept out by not being a pool
/// member, not by a name check.**
///
/// `default` names whichever login the operator has currently made live on his
/// own machine; recording history under it produces a series that silently
/// re-attributes itself the next time he runs `/login`. The live database
/// carries the proof: a `default` row frozen since 2026-08-03 whose fingerprint
/// now names an account whose own row disagrees with it by 42%.
///
/// **This test used to call `record_period_estimate("default", …)` and assert
/// it returned `None`**, because the function opened with
/// `if account == POINTER_LABEL { return Ok(None) }`. That guard is DELETED —
/// 2026-08-27, the operator: *"I do not want default even in the purview of the
/// system. Why does it even see that there is a default?"* — so calling it with
/// that literal string now records, and asserting otherwise would be asserting
/// a check he ordered removed.
///
/// The guarantee did not weaken; it got wider. The old guard covered exactly
/// ONE writer. Five siblings — `observe`, `gauge_state`, `set_account_identity`,
/// `ensure_default` and the ledger — had no such check and happily recorded
/// under `default` the whole time it stood there looking like an invariant.
/// What replaces it covers all six: the accounts that reach any of them come
/// from the POOL, and the machine default is not a pool member. So this test
/// pins the property at the place it is now true.
///
/// **THE MUTATION.** Point `ThreadManager::account_pool` back at
/// `self.registry.names()` and this goes red on the first assertion.
#[tokio::test]
async fn the_default_pointer_never_reaches_the_recorder() {
    // A registry shaped exactly like the operator's own `accounts.toml`: real
    // accounts with their own login directories, plus the machine-default slot.
    let mut reg = threads::accounts::Registry::default();
    reg.add("acct-a", Some("/tmp/threads-periods/acct-a".into()));
    reg.add("default", None);

    let pool = threads::pool::Pool::from_registry(&reg);
    assert!(
        !pool.contains("default"),
        "the recorder is only ever handed pool names: {:?}",
        pool.names()
    );

    // …and the surfaces that feed it, on a real manager, all agree.
    let tmp = tempfile::tempdir().unwrap();
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "acct-a".to_string(),
        tmp.path().join("acct-a").to_string_lossy().to_string(),
    );
    let m = threads::build_manager(threads::Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();

    for (what, names) in [
        ("account_pool", m.account_pool()),
        ("poll_targets", m.poll_targets()),
        (
            "placement_pool",
            m.placement_pool().iter().map(|p| p.name().to_string()).collect(),
        ),
    ] {
        assert!(
            !names.contains(&"default".to_string()),
            "{what} must not carry the machine default: {names:?}"
        );
    }
}

/// The record survives the label. Two registry labels for ONE login share one
/// set of usage limits, so they are one account's history — and a login that
/// gets RENAMED does not restart its series.
#[test]
fn records_are_keyed_by_the_login_not_the_label() {
    let (store, b) = svc();
    register(&store, "acct-a", "fp-shared");
    register(&store, "acct-a-alias", "fp-shared");
    let now = Utc::now();
    let w = (now + Duration::hours(1)).to_rfc3339();

    observe_in_window(&store, "acct-a", Axis::Session, 4.0, &w);
    b.record_period_estimate("acct-a", Axis::Session, 100_000.0, 1, now)
        .unwrap();
    observe_in_window(&store, "acct-a-alias", Axis::Session, 9.0, &w);
    b.record_period_estimate("acct-a-alias", Axis::Session, 110_000.0, 2, now)
        .unwrap();

    let rows = periods(&store, "fp-shared", Axis::Session);
    assert_eq!(
        rows.len(),
        1,
        "two labels for one login are ONE account's history: {rows:#?}"
    );
    assert_eq!(rows[0].tokens_per_percent, 110_000.0);
    assert_eq!(rows[0].updates, 2);
    assert_eq!(
        rows[0].account_label, "acct-a-alias",
        "the label rides along as colour, refreshed to whatever wrote last"
    );
}

/// A window we cannot NAME is not a window. With no reset instant in view the
/// recorder declines rather than bucketing under a fabricated boundary — the
/// same rule the calibrator already applies to an unknown reset.
#[test]
fn no_reset_instant_means_no_period_and_nothing_is_recorded() {
    let (store, b) = svc();
    register(&store, "acct-a", "fp-a");
    let now = Utc::now();

    // An observation with no reset instant at all (the `/usage` prose did not
    // parse, or the layer that pushed it never carried one).
    store
        .record_observation(&ObservationRow {
            id: 0,
            account: "acct-a".into(),
            axis: Axis::Session.as_str().into(),
            pct: 4.0,
            resets_at: None,
            source: "usage_poll".into(),
            observed_at: threads::store::now_rfc3339(),
        })
        .unwrap();

    assert_eq!(
        b.record_period_estimate("acct-a", Axis::Session, 100_000.0, 1, now)
            .unwrap(),
        None
    );
    assert!(store.list_period_estimates().unwrap().is_empty());
    // …and with no observation whatsoever, the same.
    assert_eq!(
        b.record_period_estimate("acct-a", Axis::WeeklyFable, 900_000.0, 1, now)
            .unwrap(),
        None
    );
}

/// An account with NO readable login fingerprint still gets a history — under
/// a key that is honest about being a label, so a fingerprint arriving later
/// cannot be confused with one that was measured all along.
#[test]
fn an_account_with_no_readable_identity_falls_back_to_a_marked_label_key() {
    let (store, b) = svc();
    store
        .set_account_identity("acct-nofp", Some("/tmp/threads-test/acct-nofp"), None)
        .unwrap();
    let now = Utc::now();
    let w = (now + Duration::hours(1)).to_rfc3339();
    observe_in_window(&store, "acct-nofp", Axis::Session, 4.0, &w);
    b.record_period_estimate("acct-nofp", Axis::Session, 100_000.0, 1, now)
        .unwrap();

    let rows = store.list_period_estimates().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].identity, "label:acct-nofp",
        "an unknown login is keyed as a LABEL and says so — a bare name here \
         would be indistinguishable from a fingerprint"
    );
}
