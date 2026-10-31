//! **WHAT ONE MID-TURN TRIP CHECK COSTS THE STORE, COUNTED.**
//!
//! `ThreadManager::charge_budget` runs `check_trip_midturn` on EVERY
//! token-usage line the CLI streams, and that reaches `Budgets::check_trip` ->
//! `snapshot` -> one `meter()` per axis -> a loop over the metered accounts.
//! With four axes and three accounts that was **83 separate round trips through
//! the store, per streamed line, per stream**.
//!
//! Thirty-six of those eighty-three were the SAME statement. `meter()` wanted
//! three things off one row — where the window starts, when the gauge says the
//! pool resets, and the raw observed value — and fetched that row three times
//! to get them: once inside `window_start`, once inline for `resets_at`, once
//! through `observed_value`. Identical `SELECT … ORDER BY observed_at DESC
//! LIMIT 1`, per account, per axis, three times over.
//!
//! Collapsing them is not a speed/correctness trade. Three reads of one row
//! inside one snapshot can only ever agree — and on the occasion they do NOT,
//! because a concurrent observation landed between them, the snapshot is
//! internally inconsistent, which is a defect and not a feature.
//!
//! **The count, not the wall time.** The thing that made this dangerous was
//! never milliseconds — every one of these statements is indexed and
//! sub-millisecond — it was N round trips through a store that used to
//! serialize all of them behind one mutex. Wall time on a warm page cache hides
//! that completely; the count does not. Same reasoning as
//! `tests/derivation_cost.rs`, and the same reason this is its own test binary:
//! `store::db_trips()` is process-global, so a test running beside this one
//! would land its statements inside the measurement window.

use threads::budgets::Budgets;
use threads::gauge::Axis;
use threads::store::{ObservationRow, Store};
use threads::BudgetConfig;

const ACCOUNTS: [&str; 3] = ["acct-alpha", "acct-beta", "acct-gamma"];
const KEY: &str = "metered";

/// The cost of one `check_trip` over three accounts and four axes.
///
/// One `get_budget`, one `charge_accounts`, then per axis (4) per account (3):
/// the newest observation, the charged tokens, the charged dollars, the stored
/// estimate, and — where the account's own measurement has not yet displaced
/// the prior — a fleet scan of estimates. The fable axis additionally borrows
/// the weekly-all reset when it has none of its own.
///
/// It was 83 before the triple read was collapsed. If this number RISES,
/// something has put another statement inside a per-account, per-axis loop that
/// runs on every streamed line, and that is the shape this file exists to catch.
const EXPECTED_TRIPS: u64 = 59;

fn seeded() -> (tempfile::TempDir, Budgets, Vec<String>) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("threads.db")).unwrap();
    let budgets = Budgets::new(store.clone());
    let accounts: Vec<String> = ACCOUNTS.iter().map(|a| a.to_string()).collect();

    let mut cfg = BudgetConfig::new(KEY);
    cfg.accounts = accounts.clone();
    cfg.session_pct = Some(90.0);
    cfg.weekly_pct = Some(90.0);
    cfg.fable_pct = Some(90.0);
    cfg.cash_usd = Some(100.0);
    budgets.set_config(&cfg).unwrap();

    for a in &accounts {
        budgets
            .charge(KEY, "thread-1", a, "claude-sonnet-4", 1_000, 0.01)
            .unwrap();
        for axis in Axis::ALL {
            store
                .record_observation(&ObservationRow {
                    id: 0,
                    account: a.clone(),
                    axis: axis.as_str().to_string(),
                    pct: 10.0,
                    resets_at: None,
                    source: "test".into(),
                    observed_at: threads::store::now_rfc3339(),
                })
                .unwrap();
        }
    }
    (tmp, budgets, accounts)
}

#[test]
fn a_mid_turn_trip_check_reads_each_accounts_gauge_once_per_axis() {
    let (_tmp, budgets, accounts) = seeded();

    // Warm: the first call through any path can pay for lazily-opened
    // connections and schema statements that are not part of the steady state.
    let _ = budgets.check_trip(KEY, &accounts).unwrap();

    let before = threads::store::db_trips();
    let trip = budgets.check_trip(KEY, &accounts).unwrap();
    let cost = threads::store::db_trips() - before;
    assert!(trip.is_none(), "10% against a 90% ceiling must not trip");

    assert_eq!(
        cost, EXPECTED_TRIPS,
        "one mid-turn trip check costs {cost} store round trips, not \
         {EXPECTED_TRIPS}. This runs on EVERY streamed token-usage line: a \
         statement added inside the per-account, per-axis loop is multiplied by \
         twelve and then by every line of every stream."
    );

    // `snapshot` is what `check_trip` is made of, so the two must cost the same
    // — a divergence would mean the trip check had grown a reading of its own.
    let before = threads::store::db_trips();
    let snap = budgets.snapshot(KEY, &accounts).unwrap().unwrap();
    assert_eq!(threads::store::db_trips() - before, EXPECTED_TRIPS);
    assert_eq!(snap.metered_accounts.len(), ACCOUNTS.len());

    // THE PROPERTY, stated so it survives a change to the constant: the meter
    // reads each (account, axis) gauge row ONCE. Four axes x three accounts is
    // twelve; it used to be thirty-six.
    //
    // Measured as the DELTA a single extra account adds, which isolates the
    // per-account cost from the fixed overhead and cannot be satisfied by
    // moving work out of the loop into a constant.
    let two: Vec<String> = accounts[..2].to_vec();
    let before = threads::store::db_trips();
    let _ = budgets.snapshot(KEY, &two).unwrap();
    let cost_2 = threads::store::db_trips() - before;
    let per_account = cost - cost_2;
    assert!(
        per_account <= 8,
        "each additional metered account costs {per_account} statements per \
         snapshot (four axes). Three identical gauge reads per axis would make \
         that at least twelve."
    );

    // And the collapse did not change a single thing the meter REPORTS.
    the_meter_still_reports_what_it_reported(&budgets, &accounts);
}

/// **ONE TEST, ON PURPOSE.** `db_trips()` is process-global, so a second
/// `#[test]` in this binary would run beside the measurement and land its own
/// statements inside the window — which it did, and the count moved from 59 to
/// 64 to 73 across three runs before the two were merged.
fn the_meter_still_reports_what_it_reported(budgets: &Budgets, accounts: &[String]) {
    // The three readings that used to be three statements: the window
    // placement, the reset instant and the observed value. Each account was
    // observed at 10% with no reset instant, so the assertions are exact.
    let snap = budgets.snapshot(KEY, &accounts).unwrap().unwrap();
    for axis in Axis::ALL {
        let m = snap.meter(axis);
        assert_eq!(m.accounts.len(), ACCOUNTS.len());
        for row in &m.accounts {
            assert_eq!(
                row.observed,
                Some(10.0),
                "the observed value must still come off the newest observation"
            );
            assert_eq!(
                row.window_resets_at, None,
                "an observation with no reset instant must still report none"
            );
            // `window_start` is the public path and reads the row for itself;
            // the meter now reads it once and passes it down. The two must
            // agree to the second, or the collapse changed the window.
            let via_public = budgets
                .window_start(&row.account, axis, chrono::Utc::now())
                .to_rfc3339();
            assert_eq!(
                via_public[..17],
                row.window_start[..17],
                "the window the meter places must equal the window \
                 `window_start` places for the same account and axis"
            );
        }
    }
}
