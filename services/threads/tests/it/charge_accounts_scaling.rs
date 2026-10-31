//! `Store::charge_accounts` must not get slower as the charge ledger grows.
//!
//! This is a regression test for a real hang. `budget_charges` is an
//! append-only ledger that nothing prunes, and `charge_accounts` is asked
//! "which accounts has this key drawn from?" on EVERY token-usage line the CLI
//! streams — `ThreadManager::charge_budget` -> `check_trip_midturn` ->
//! `Budgets::check_trip` -> `snapshot` -> `metered_accounts` -> here. It used
//! to answer by walking every charge row for the key and sorting them through
//! a temp b-tree, and because `budget_key` is one value in practice, "every row
//! for the key" was the whole table.
//!
//! On the operator's own daemon on 2026-09-11 that was 479,230 rows, and
//! because the walk ran while holding the store's single `Mutex<Connection>`,
//! it stalled every other task in the process: a live `sample(1)` of the
//! running daemon caught four of ten tokio workers parked in `Store::conn`'s
//! `Mutex::lock` behind this one query, and `/health` — which does nothing but
//! return a fixed struct — took over fifteen seconds to answer. The daemon was
//! up, listening, and accepting connections the whole time. It just could not
//! reply.
//!
//! So the property under test is not "fast", it is **flat**: the answer depends
//! on how many accounts exist, never on how many charges have been recorded.
//! The test compares a small ledger against one 500x larger and asserts the
//! cost barely moves. Before the fix the large case was roughly three orders of
//! magnitude slower; after it, the two are indistinguishable.
//!
//! Timing is compared as a RATIO of the best of several runs rather than
//! against an absolute bound, so the test does not encode this machine's disk
//! speed — and best-of-N is used because the failure this guards against was
//! itself discovered on a box under heavy I/O contention, where single wall
//! clock readings for identical queries ranged from 0.9s to 97s.

use std::time::{Duration, Instant};
use threads::store::{ChargeRow, Store};

const ACCOUNTS: [&str; 3] = ["acct-alpha", "acct-beta", "acct-gamma"];
const KEY: &str = "default";

fn seed(store: &Store, rows: usize) {
    for i in 0..rows {
        store
            .record_charge(&ChargeRow {
                budget_key: KEY.into(),
                thread_id: format!("thread-{}", i % 64),
                account: ACCOUNTS[i % ACCOUNTS.len()].into(),
                axis: if i % 2 == 0 { "session" } else { "weekly_all" }.into(),
                tokens: i as i64,
                cost_usd: 0.0,
                ts: format!("2026-09-{:02}T00:00:00Z", 1 + (i % 9)),
            })
            .unwrap();
    }
}

/// Best of `n` — the minimum is the run least disturbed by unrelated I/O.
fn best_of(n: usize, store: &Store) -> Duration {
    (0..n)
        .map(|_| {
            let t = Instant::now();
            let got = store.charge_accounts(KEY).unwrap();
            assert_eq!(got.len(), ACCOUNTS.len());
            t.elapsed()
        })
        .min()
        .unwrap()
}

#[test]
fn charge_accounts_returns_the_distinct_accounts_in_order() {
    let store = Store::open_in_memory().unwrap();
    seed(&store, 500);

    let mut expected: Vec<String> = ACCOUNTS.iter().map(|a| a.to_string()).collect();
    expected.sort();
    assert_eq!(store.charge_accounts(KEY).unwrap(), expected);

    // A key with no charges is empty, not an error — `metered_accounts` folds
    // the result into a list it already has and must be able to get nothing.
    assert!(store.charge_accounts("never-charged").unwrap().is_empty());

    // Charges under another key are not this key's accounts.
    store
        .record_charge(&ChargeRow {
            budget_key: "other".into(),
            thread_id: "t".into(),
            account: "acct-zeta".into(),
            axis: "session".into(),
            tokens: 1,
            cost_usd: 0.0,
            ts: "2026-09-11T00:00:00Z".into(),
        })
        .unwrap();
    assert_eq!(store.charge_accounts(KEY).unwrap(), expected);
    assert_eq!(store.charge_accounts("other").unwrap(), vec!["acct-zeta"]);
}

#[test]
fn charge_accounts_does_not_slow_down_as_the_ledger_grows() {
    let small = Store::open_in_memory().unwrap();
    seed(&small, 400);
    let small_t = best_of(5, &small);

    let large = Store::open_in_memory().unwrap();
    seed(&large, 200_000);
    let large_t = best_of(5, &large);

    // Both ledgers hold the same three accounts; only the number of charges
    // differs, by 500x. A lookup whose cost is O(distinct accounts) cannot
    // notice that. A lookup whose cost is O(rows) notices it by a factor of
    // roughly 500.
    //
    // The bar is 20x, which is 25x of headroom under the real regression and
    // far more than timer granularity or a noisy disk can manufacture.
    let ratio = large_t.as_secs_f64() / small_t.as_secs_f64().max(1e-9);
    assert!(
        ratio < 20.0,
        "charge_accounts scales with the size of the charge ledger: \
         {} rows took {small_t:?}, {} rows took {large_t:?} ({ratio:.1}x). \
         It must be O(distinct accounts), not O(rows) — this is the query \
         that parked the daemon's tokio workers behind the store mutex.",
        400,
        200_000,
    );
}
