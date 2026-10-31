//! # The account model, after the 2026-09-20 separation
//!
//! Three questions about one account that used to be two, and were never three:
//!
//! | question | where it lives | remedy when it says no |
//! |---|---|---|
//! | **IS it in the pool?** (membership) | `account_disabled` | he reverses it |
//! | **HOW MUCH of it is lent?** (allocation) | `pool_shares.share` | he changes it |
//! | **CAN it serve a turn now?** (availability) | `accounts.limit_until` | he waits |
//!
//! Membership and allocation were ONE COLUMN until 2026-09-20 — disabled WAS
//! `share == 0` — and the operator separated them:
//!
//! > *"I want disabled to be independent of share. Whether to include it in the
//! > pool supersedes how much should be made available to the pool."*
//!
//! Availability was known to the daemon all along and never reached the ranker,
//! which is what produced the `−49.63` he reported on the dashboard.
//!
//! This file covers the three claims that need a STORE or a CONFIG rather than
//! a `Budgets` in memory: the migration off the old model, the idle probe
//! cadence, and the barred account leaving the pressure scale while staying on
//! the payload. The rest lives beside the code in `budgets.rs`, in
//! `tests/account_disable.rs` (the manager half) and in `tests/shapes.rs` (the
//! fold half).
//!
//! Every test names the MUTATION that makes it red.

use serde_json::json;
use threads::gauge::Axis;
use threads::Store;

// ════════════════════════════════════════════════════════════════════════════
// 1. THE MIGRATION OFF `share == 0`
// ════════════════════════════════════════════════════════════════════════════

/// **EVERY ACCOUNT THE OLD MODEL DISABLED IS STILL DISABLED, AND HAS ITS SHARE
/// BACK.**
///
/// This is the only test in the suite that can fail on the operator's own
/// machine and nowhere else, because it is about the bytes already on his disk.
/// Between 2026-09-08 and 2026-09-20 a disable OVERWROTE `pool_shares.share`
/// with zero and stashed the displaced value in `prior_share`. After the
/// separation a zero in that column is meaningless, so the migration has to do
/// two things and both are silent failures if it does not:
///
/// * **carry the membership across.** An account that comes back into his pool
///   during an upgrade is the worst outcome available here — he took it out
///   because it is not a paid subscription, and nothing would tell him it was
///   serving turns again.
/// * **put the share back**, spending `prior_share` one last time. This is
///   `Budgets::restore_share`'s four-rung ladder executed in SQL; the two rungs
///   that had no memo to spend are expressed as DELETING the row, which restores
///   the declared/default fall-through instead of leaving a zero in a column
///   that now refuses zeroes.
///
/// **THE MUTATION.** Delete the `INSERT OR IGNORE INTO account_disabled` line
/// from `Store::migrate_share_zero_disable` and both accounts come back into the
/// pool. Delete the `UPDATE ... SET share = prior_share` line and the first one
/// comes back at the whole login instead of the 0.6 he lent.
#[test]
fn the_migration_carries_the_old_share_zero_disable_across_with_its_share() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.db");

    // A database in the OLD shape, written the way the 2026-09-08 code wrote it:
    // the zero over the share, the displaced value in the memo.
    {
        let store = Store::open(&path).unwrap();
        store.set_pool_share("lent-then-off", 0.6, Some("he lent 60%")).unwrap();
        store.set_pool_share("never-set-then-off", 1.0, None).unwrap();
        store.set_pool_share("still-live", 0.8, Some("untouched")).unwrap();
        store
            .execute_sql(
                "UPDATE pool_shares SET share = 0.0, prior_share = 0.6, note = 'not paid'
                     WHERE account = 'lent-then-off';
                 UPDATE pool_shares SET share = 0.0, prior_share = NULL
                     WHERE account = 'never-set-then-off';",
            )
            .unwrap();
    }

    // Re-opening runs the migration.
    let store = Store::open(&path).unwrap();

    // ── BOTH ARE STILL OUT OF THE POOL.
    for a in ["lent-then-off", "never-set-then-off"] {
        assert!(
            store.account_disabled_at(a).unwrap().is_some(),
            "{a} was disabled under the old model and must still be disabled"
        );
    }
    assert_eq!(
        store.account_disabled_at("still-live").unwrap(),
        None,
        "and an account that was never disabled must not be swept up"
    );
    // His words came across with the state.
    assert_eq!(
        store.account_disabled_note("lent-then-off").unwrap().as_deref(),
        Some("not paid")
    );

    // ── AND THE SHARES ARE BACK.
    assert_eq!(
        store.pool_share("lent-then-off").unwrap(),
        Some(0.6),
        "the memo was spent: this is the 0.6 the old disable destroyed"
    );
    assert_eq!(
        store.pool_share("never-set-then-off").unwrap(),
        None,
        "no memo to spend, so the ROW GOES — which is what restores the \
         declared/default fall-through the old rungs 2 and 3 hand-rolled"
    );
    assert_eq!(store.pool_share("still-live").unwrap(), Some(0.8));

    // ── NO ZERO SURVIVES ANYWHERE, which is the invariant the setter now
    // enforces at the door and the migration has to establish for old rows.
    for (a, s, _) in store.list_pool_shares().unwrap() {
        assert!(s > 0.0, "{a} still carries a share of {s} after the migration");
    }

    // ── IDEMPOTENT. A second open must not re-date anything or touch a share.
    let at = store.account_disabled_at("lent-then-off").unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.account_disabled_at("lent-then-off").unwrap(), at);
    assert_eq!(store.pool_share("lent-then-off").unwrap(), Some(0.6));
}

// ════════════════════════════════════════════════════════════════════════════
// 2. THE IDLE GAUGE CADENCE
// ════════════════════════════════════════════════════════════════════════════

/// **THE IDLE SWEEP IS 1×/HOUR AND THE BUSY SWEEP IS UNCHANGED.**
///
/// The operator:
///
/// > *"the daemon should only be reading balances on accounts that are active
/// > on the accelerated schedule. If there isn't any placement you can move it
/// > to 1x/hour so we don't get rate limited."*
///
/// The first half was already true — `probe_interval_busy_secs` polls only
/// logins with a turn or a subagent in flight — so the BUSY number must not
/// move: it is the bound on "how long can the daemon keep placing onto an
/// exhausted account", and loosening it to save round trips would trade a
/// safety mechanism for a cost the busy leg does not incur.
///
/// The whole-pool leg is the one spending round trips on accounts nothing is
/// happening on, four times an hour, forever.
///
/// **`0` KEEPS ITS MEANING** on both knobs, and both stay configurable: a value
/// in `config.toml` is honoured, and only the DEFAULT moved.
///
/// **THE MUTATION.** Put `default_probe_interval()` back to 900 and the first
/// assertion goes red; change `default_probe_interval_busy()` and the second
/// does.
#[test]
fn the_idle_probe_default_is_one_sweep_an_hour_and_the_busy_one_is_untouched() {
    let c = threads::Config::default();
    assert_eq!(
        c.probe_interval_secs, 3600,
        "the whole-pool sweep is 1x/hour: an idle account's reading does not \
         move, and four round trips an hour per login to learn that is exactly \
         the rate-limit exposure he named"
    );
    assert_eq!(
        c.probe_interval_busy_secs, 120,
        "THE ACCELERATED SCHEDULE IS UNCHANGED. It polls only logins with work \
         in flight and it is the detection bound for an exhausted account; this \
         change must not have touched it"
    );
    assert!(
        c.probe_interval_secs > c.probe_interval_busy_secs,
        "the idle leg must never be tighter than the busy one, or the split has \
         collapsed"
    );

    // BOTH KNOBS STAY CONFIGURABLE, and `0` still means off — the value the
    // whole test suite sets to keep timers out of its way.
    let toml = "probe_interval_secs = 900\nprobe_interval_busy_secs = 0\n";
    let parsed: threads::Config = toml::from_str(toml).unwrap();
    assert_eq!(parsed.probe_interval_secs, 900, "a configured value wins");
    assert_eq!(parsed.probe_interval_busy_secs, 0, "and 0 is still 0");
}

// ════════════════════════════════════════════════════════════════════════════
// 3. A BARRED ACCOUNT LEAVES THE SCALE AND STAYS ON THE PAYLOAD
// ════════════════════════════════════════════════════════════════════════════

fn budgets_with(accounts: &[(&str, f64, f64)], now: chrono::DateTime<chrono::Utc>) -> threads::budgets::Budgets {
    let b = threads::budgets::Budgets::new(Store::open_in_memory().unwrap());
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

/// **PUT A LIVE EXECUTION ON AN ACCOUNT.** Two rows, because
/// `Budgets::nothing_running_on` needs both: a `running_turns` row whose pid
/// exists (this test process stands in for the child — the predicate refuses to
/// count a turn whose process is gone), and a `thread_accounts` row, because
/// `running_turns` carries no account column and an unattributable turn counts
/// against every account at once.
fn set_running_on(b: &threads::budgets::Budgets, thread_id: &str, account: &str) {
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
    b.store_ref().record_thread_account(thread_id, account).unwrap();
}

/// **THE −49.63 IS OFF THE SCALE, AND THE ACCOUNT IS STILL ON THE BOARD.**
///
/// Reported by the operator on 2026-09-20 against the live daemon:
///
/// > *"I'm seeing pressure of -49.63x on the threads dashboard for an account
/// > that is at its limit for the week. Let's exclude it from the scaled
/// > pressure mechanism while it's unavailable. It makes the others not make
/// > sense because it distorts the scale so much."*
///
/// Nothing was arithmetically wrong. `owner-gmail` was at 100% against a
/// 0.9 share with minutes to its reset, so the numerator was negative and the
/// denominator shrinking, and the required burn rate diverged — it read −56.66
/// when this was re-measured a few minutes later. The damage was that it was
/// still `ranked: true` and therefore INSIDE the scale it was destroying: three
/// healthy accounts between 0.92 and 0.99 are indistinguishable on any relative
/// display that also has to fit −56.
///
/// The fixture reproduces that pool exactly, to one decimal place, so a reader
/// can check the claim rather than take it.
///
/// **UNRANKED IS NOT UNLISTED.** The `accounts.pressure` contract is explicit
/// that an account which silently vanishes from this payload is how he loses
/// track of one, so the barred account keeps every reading it has and gains
/// `limit_until` saying when the bar lifts.
///
/// **THE MUTATION.** Delete the `out.excluded_over(unavailable.then_some(..))`
/// line at the tail of `pressure_reading` and the first block goes red with the
/// divergent number back in the table and the three healthy ranks shifted.
#[test]
fn an_account_inside_a_limit_window_is_unranked_but_still_listed() {
    let now = chrono::Utc::now();
    // The live pool, reproduced: three healthy accounts clustered high, and one
    // at 100% on a 0.9 share with its window nearly over.
    let b = budgets_with(
        &[
            ("owner-account", 19.0, 0.8),
            ("cc1", 9.0, 0.8),
            ("cc2", 46.0, 0.8),
            ("owner-gmail", 100.0, 0.001),
        ],
        now,
    );
    b.store_ref().set_pool_share("owner-gmail", 0.9, None).unwrap();

    // ── THE FIXTURE ALSO NEEDS A LIVE TURN, SINCE 2026-09-22 ──
    //
    // This account is at 100% against a 0.9 share, which is to say it is OVER
    // ITS LENT SHARE — and an over-share account with nothing running on it now
    // leaves the scale on its own (`UnrankedReason::OverShareSettled`). That is
    // a SECOND, independent reason to unrank exactly this row, and without a
    // running turn the fixture check below would be measuring it instead of the
    // bar this test is about.
    //
    // The thread is the one the operator described in the ruling that added
    // that rule: the last one to run on the account, not evicted early, which
    // is why the account is past its share at all. With it in place the
    // un-barred reading is the −49.63 he actually reported, which is what makes
    // the fixture check mean something.
    set_running_on(&b, "t-overrun", "owner-gmail");

    // FIXTURE CHECK: without the bar this account really does produce the
    // divergent number the operator saw. If this stops being true the test
    // below is asserting nothing.
    let unbarred = b.pressure_reading("owner-gmail", now);
    let v = unbarred.pressure.expect("fixture: it is ranked while un-barred");
    assert!(
        v < -40.0,
        "fixture: the reported pool produced {v}, not the large negative the \
         operator reported — re-point this test"
    );

    // ── THE BAR, as the daemon records it: an observed limit window, in the
    // future, on the account's own row.
    let until = (now + chrono::Duration::minutes(13)).to_rfc3339();
    b.store_ref()
        .set_account_limit("owner-gmail", Some((&until, Some("weekly limit"), Some("weekly_all"), None)))
        .unwrap();

    let table = b.pressure_table(
        &["owner-account", "cc1", "cc2", "owner-gmail"].map(String::from),
        now,
    );
    let row = table
        .accounts
        .iter()
        .find(|r| r.account == "owner-gmail")
        .expect("STILL LISTED: unranked is not unlisted, and an account that \
                 vanishes from this payload is how he loses track of one");

    // ── OUT OF THE SCALE.
    assert!(!row.ranked, "{row:?}");
    assert_eq!(row.pressure, None, "null, never a number and never 0.0: {row:?}");
    assert_eq!(row.rank, None, "unranked means NO rank, not last place: {row:?}");
    assert_eq!(row.unranked_reason, Some("unavailable"), "{row:?}");
    assert!(row.unavailable, "{row:?}");
    assert_eq!(
        row.limit_until.as_deref(),
        Some(until.as_str()),
        "WHEN, not only WHY — the reason is actionable only with the instant: {row:?}"
    );

    // ── AND EVERY READING IT HAD IS STILL THERE.
    assert_eq!(row.observed_pct, Some(100.0), "{row:?}");
    assert!(row.observed_at.is_some(), "{row:?}");
    assert!(row.resets_at.is_some(), "{row:?}");
    assert_eq!(row.share, 0.9, "{row:?}");
    assert!(!row.disabled, "a bar is NOT a disable: {row:?}");

    // ── THE OTHER THREE ARE UNDISTURBED, and their ranks are 1..3 rather than
    // 1..4 with a hole. This is the half the operator actually asked for: the
    // scale is theirs again.
    assert_eq!(table.ranked_count, 3, "{table:?}");
    let mut ranks: Vec<usize> = table
        .accounts
        .iter()
        .filter(|r| r.account != "owner-gmail")
        .map(|r| r.rank.expect("a healthy account is ranked"))
        .collect();
    ranks.sort();
    assert_eq!(ranks, vec![1, 2, 3], "{table:?}");
    for r in table.accounts.iter().filter(|r| r.account != "owner-gmail") {
        let p = r.pressure.unwrap();
        assert!(
            p > 0.0 && p < 2.0,
            "the surviving scale is the tight band it should be: {r:?}"
        );
    }
}

/// **A STALE LIMIT IS NOT A BAR, and a lapsed one is not either.**
///
/// `effective_limit_until` and not the raw column, because a `limit_until`
/// stamped against a login this label no longer points at belongs to a
/// different account — that is what stranded a thread for five and a half hours
/// on a login answering turns at 29% on 2026-08-02. Reading the raw column here
/// would have re-created that defect on a new surface.
///
/// **THE MUTATION.** Read `st.limit_until` instead of
/// `st.effective_limit_until()` in `pressure_reading` and the stale case goes
/// red. Delete the `.filter(|t| t > now)` and the lapsed case does.
#[test]
fn a_stale_or_lapsed_limit_window_does_not_unrank_an_account() {
    let now = chrono::Utc::now();
    let b = budgets_with(&[("acct", 40.0, 0.5)], now);

    // STALE: the window was observed on a DIFFERENT login than the one this
    // label points at now. Two known, differing fingerprints falsify it.
    let future = (now + chrono::Duration::hours(2)).to_rfc3339();
    b.store_ref()
        .set_account_limit("acct", Some((&future, Some("429"), Some("weekly_all"), Some("observed-on-A"))))
        .unwrap();
    b.store_ref()
        .set_account_identity("acct", None, Some("points-at-B"))
        .unwrap();
    let r = b.pressure_reading("acct", now);
    assert!(
        !r.unavailable,
        "a limit belonging to a login this label no longer names is not a bar"
    );
    assert_eq!(r.limit_until, None);
    assert!(r.pressure.is_some(), "and it is ranked normally: {r:?}");

    // LAPSED: attributable, but already over.
    let past = (now - chrono::Duration::hours(1)).to_rfc3339();
    b.store_ref()
        .set_account_limit("acct", Some((&past, Some("429"), Some("weekly_all"), Some("points-at-B"))))
        .unwrap();
    let r = b.pressure_reading("acct", now);
    assert!(!r.unavailable, "a window that has already passed bars nothing: {r:?}");
    assert!(r.pressure.is_some(), "{r:?}");
}

/// **DISABLED DOMINATES UNAVAILABLE, and the wire keeps both facts.**
///
/// Two true statements about one account, and only one can be the `reason`. The
/// operator's own act wins, because it is the one he can act on and because a
/// bar is irrelevant to an account nothing will be placed on anyway. But the
/// BOOLEANS both stay true on the row, so an interface can say "disabled, and
/// also at its limit" if it wants to — the precedence is about which single
/// word is shown, never about discarding a fact.
///
/// **THE MUTATION.** Swap the two arms of the `excluded` ladder in
/// `pressure_reading` and the SECOND block goes red.
///
/// **THE FIRST FORM OF THIS TEST PASSED UNDER THAT MUTATION**, and it is
/// recorded because a green that fails to fail under its own mutation is worse
/// than no test. It had only the first block: an account with a full set of
/// readings. That account reaches the `lent()` guard, which passes a HARDCODED
/// `Disabled` rather than the `excluded` ladder — deliberately, so that deleting
/// the guard stays visible — so the ladder's order was never consulted and the
/// swap changed nothing. The ladder is only reached by the FIVE EARLY EXITS, so
/// the second block gives the account nothing to measure and exits through one.
#[test]
fn a_disabled_account_that_is_also_barred_reports_disabled() {
    let now = chrono::Utc::now();
    let b = budgets_with(&[("acct", 100.0, 0.001)], now);
    b.store_ref().set_pool_share("acct", 0.5, None).unwrap();
    b.store_ref().set_account_disabled("acct", true, None).unwrap();
    let until = (now + chrono::Duration::hours(2)).to_rfc3339();
    b.store_ref()
        .set_account_limit("acct", Some((&until, Some("429"), Some("weekly_all"), None)))
        .unwrap();

    let r = b.pressure_reading("acct", now);
    assert_eq!(
        r.unranked_reason,
        Some(threads::budgets::UnrankedReason::Disabled),
        "his own act is the reason he can act on: {r:?}"
    );
    // AND NEITHER FACT IS DISCARDED. Three independent answers on one row.
    assert!(r.disabled, "membership: {r:?}");
    assert!(r.unavailable, "availability: {r:?}");
    assert_eq!(r.share, 0.5, "allocation: {r:?}");
    assert_eq!(r.pressure, None);

    // On the wire, through the same builder both surfaces use.
    let rows = b.pressure_rows(&["acct".to_string()], now);
    let w = &rows["accounts"][0];
    assert_eq!(w["unranked_reason"], json!("disabled"));
    assert_eq!(w["disabled"], json!(true));
    assert_eq!(w["unavailable"], json!(true));
    assert_eq!(w["share"], json!(0.5));
    assert!(w["pressure"].is_null());

    // ── THE BLOCK THAT ACTUALLY EXERCISES THE LADDER ──
    //
    // No observation at all, so this exits through `NoReading` — one of the five
    // early exits, which are the only paths that consult `excluded`. Both
    // standing exclusions still have to beat the measurement reason, and
    // `disabled` still has to beat `unavailable`.
    let b2 = threads::budgets::Budgets::new(Store::open_in_memory().unwrap());
    b2.store_ref().set_account_disabled("silent", true, None).unwrap();
    b2.store_ref()
        .set_account_limit("silent", Some((&until, Some("429"), Some("weekly_all"), None)))
        .unwrap();
    let r = b2.pressure_reading("silent", now);
    assert_eq!(
        r.unranked_reason,
        Some(threads::budgets::UnrankedReason::Disabled),
        "his own act outranks the bar AND outranks `no_reading`, which would \
         send him to poll a gauge on an account he took out of the pool: {r:?}"
    );
    assert!(r.disabled && r.unavailable, "{r:?}");
    assert_eq!(r.observed_pct, None, "fixture: it exits through NoReading");

    // And with the disable lifted, the bar is what the early exit reports —
    // still not `no_reading`.
    b2.store_ref().set_account_disabled("silent", false, None).unwrap();
    let r = b2.pressure_reading("silent", now);
    assert_eq!(
        r.unranked_reason,
        Some(threads::budgets::UnrankedReason::Unavailable),
        "{r:?}"
    );
}
