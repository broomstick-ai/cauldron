//! **AN ACCOUNT ADDED TO THE POOL MUST BECOME REACHABLE BY THREADS THAT
//! ALREADY EXIST.**
//!
//! The operator, 2026-09-18, on `bob-example-com` — a login he had been using
//! by hand in claude-code for a day:
//!
//! > *"I've been using the account directly in claude-code but it doesn't have
//! > history in threads so it's not getting selected. So if an account is added
//! > to the pool, there's a condition where it can never be used. So I need that
//! > fixed."*
//!
//! He is describing TWO gates, stacked, and each one alone is sufficient to make
//! a new account permanently unreachable. This file pins both.
//!
//! # Gate one: THE SCOPE, which is the one that actually fired
//!
//! Reconstructed from his own ledger. At **2026-09-18T19:34:57Z** thread
//! `bcccfa79-e3c4-4423-8850-7c46c7d3e5cb` parked for continuity and failed over,
//! and its `account_switched` event records the whole candidate field verbatim:
//!
//! ```text
//! continuity on alice-example-com: waiting for an admissible account this
//! thread has history on (history: ["alice-example-com"]; admissible now:
//! [Placeable("owner-account")]; blocked: alice-example-com:
//! weekly_all at 100% ≥ threshold 95%; owner-gmail: weekly_all at 100% ≥
//! threshold 95%) — failing over to owner-account and carrying the
//! conversation (msg #1028)
//! ```
//!
//! `bob-example-com` is in neither list. Not admissible, not blocked —
//! **absent**, because `aui:default`'s persisted scope was written on the boot
//! that created it and still read
//! `["owner-account","owner-gmail","alice-example-com"]`. The
//! thread was ported onto an account at **91% weekly** (four points from its
//! own 95% threshold) while a measured, ranked, **1%** account sat outside the
//! scope. `Budgets::ensure_default` never overwrites, so the scope it wrote once
//! is the scope it keeps — and an account added afterwards never joins it.
//!
//! # Gate two: THE HISTORY INTERSECTION, which would have fired next
//!
//! Even in scope, the re-rank branch of `Budgets::admit` built its field as the
//! thread's own `thread_accounts` history INTERSECTED with the placeable
//! targets. An account with no history rows matched nothing, so it was removed
//! before a single criterion was computed for it. You need history to be
//! ranked; you only get history by being placed. For a thread that already
//! exists the loop never opens.
//!
//! It is not continuity that filter was protecting.
//! `ThreadManager::move_thread` PORTS the conversation to whatever target it is
//! given and then sets `resumable` from the port's own outcome — so a
//! destination that already appears in `thread_accounts` buys nothing a port
//! does not also buy. The cache asset is `momentum`, it is five minutes wide,
//! and it already lives inside `place`.
//!
//! Every test here drives the real crate against an in-memory store. No claude
//! process is spawned, no login is read, and nothing touches the operator's
//! database or the running daemon.

use threads::accounts::Placeable;
use threads::budgets::{Admission, AdmissionRequest, Budgets, Demand, ParkCause, Thresholds};
use threads::gauge::Axis;
use threads::store::{EstimateRow, Store};

const BROOM: &str = "owner-account";
const GMAIL: &str = "owner-gmail";
const CC1: &str = "alice-example-com";
const CC2: &str = "bob-example-com";

// ── harness ─────────────────────────────────────────────────────────────────

fn placeable(name: &str) -> Placeable {
    threads::pool::PoolEntry::claude_account(name, &format!("/tmp/threads-new-account/{name}"))
        .expect("an entry that names its login directory is a pool member")
        .placeable()
}

fn pool(names: &[&str]) -> Vec<Placeable> {
    names.iter().copied().map(placeable).collect()
}

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

/// The thresholds every AUI thread carries — `{"weekly": 95.0}`, read off the
/// three live threads on 2026-09-18.
fn aui_thresholds() -> Thresholds {
    Thresholds {
        session: None,
        fable: None,
        weekly: Some(95.0),
        cash: None,
    }
}

/// A coordinator-shaped turn. The operator's own threads run 20–60M tokens with
/// a dozen subagents (see `placement_pressure_never_votes.rs`), which is the
/// regime where `expected_deaths` is in its sensitive region and an account four
/// points from its ceiling is measurably worse than an empty one.
fn coordinator_demand() -> Demand {
    Demand {
        tokens_per_turn: 20_000_000.0,
        subagents: 12.0,
    }
}

/// **HIS FOUR ACCOUNTS AT 19:34:57Z**, transcribed from `usage_observations`
/// and `limit_estimates` in `threads.db`. Reset instants are expressed as
/// offsets from `now` so the fixture keeps meaning the same thing tomorrow:
/// cc1's window had ~2d2h left, gmail's ~2d5h, broomstick's ~22h, cc2's ~6d7h.
///
/// `bob-example-com` has **no `limit_estimates` row on any axis** — that is
/// the live state, and it is deliberate here. A day-old account is uncalibrated
/// by construction, so a fix that only works for a calibrated account would not
/// be a fix for the case he reported.
fn his_pool_at_1934() -> (Budgets, Store) {
    let store = Store::open_in_memory().unwrap();
    let b = Budgets::new(store.clone());
    let now = chrono::Utc::now();
    let cc1_reset = (now + chrono::Duration::minutes(2 * 24 * 60 + 2 * 60)).to_rfc3339();
    let gmail_reset = (now + chrono::Duration::minutes(2 * 24 * 60 + 5 * 60)).to_rfc3339();
    let broom_reset = (now + chrono::Duration::minutes(22 * 60 + 25)).to_rfc3339();
    let cc2_reset = (now + chrono::Duration::minutes(6 * 24 * 60 + 7 * 60 + 25)).to_rfc3339();

    // cc1: weekly_all 100%, inside a limit window until 2026-09-20T22:00.
    b.observe(CC1, Axis::Session, 0.0, None, "usage_poll").unwrap();
    b.observe(CC1, Axis::WeeklyAll, 100.0, Some(cc1_reset.clone()), "usage_poll").unwrap();
    b.observe(CC1, Axis::WeeklyFable, 98.0, Some(cc1_reset), "usage_poll").unwrap();
    calibrate(&store, CC1, Axis::Session, 3_178_154.044_085_95);
    calibrate(&store, CC1, Axis::WeeklyAll, 23_576_846.396_200_9);
    calibrate(&store, CC1, Axis::WeeklyFable, 1_531_402.531_926_74);

    // owner-gmail: weekly_all 100%.
    store.set_pool_share(GMAIL, 0.9, None).unwrap();
    b.observe(GMAIL, Axis::Session, 0.0, None, "usage_poll").unwrap();
    b.observe(GMAIL, Axis::WeeklyAll, 100.0, Some(gmail_reset.clone()), "usage_poll").unwrap();
    b.observe(GMAIL, Axis::WeeklyFable, 63.0, Some(gmail_reset), "usage_poll").unwrap();
    calibrate(&store, GMAIL, Axis::Session, 2_456_873.946_423_13);
    calibrate(&store, GMAIL, Axis::WeeklyAll, 15_892_580.022_805);
    calibrate(&store, GMAIL, Axis::WeeklyFable, 1_176_253.441_090_28);

    // owner-account: weekly_all 91% — four points from the threshold,
    // and the account the failover actually chose.
    b.observe(BROOM, Axis::Session, 7.0, None, "usage_poll").unwrap();
    b.observe(BROOM, Axis::WeeklyAll, 91.0, Some(broom_reset.clone()), "usage_poll").unwrap();
    b.observe(BROOM, Axis::WeeklyFable, 80.0, Some(broom_reset), "usage_poll").unwrap();
    calibrate(&store, BROOM, Axis::Session, 3_213_298.586_432_56);
    calibrate(&store, BROOM, Axis::WeeklyAll, 21_053_052.708_811_7);
    calibrate(&store, BROOM, Axis::WeeklyFable, 1_043_425.391_035_88);

    // bob-example-com: weekly_all 1%, ranked first on pressure, NO calibration.
    b.observe(CC2, Axis::Session, 1.0, None, "usage_poll").unwrap();
    b.observe(CC2, Axis::WeeklyAll, 1.0, Some(cc2_reset.clone()), "usage_poll").unwrap();
    b.observe(CC2, Axis::WeeklyFable, 0.0, Some(cc2_reset), "usage_poll").unwrap();

    (b, store)
}

fn admit_for<'a>(
    b: &Budgets,
    accounts: &'a [Placeable],
    current: &'a str,
    history: &'a [String],
    key: Option<&'a str>,
) -> Admission {
    b.admit(&AdmissionRequest {
        budget_key: key,
        thresholds: &aui_thresholds(),
        accounts,
        current_account: current,
        pinned: false,
        history_accounts: history,
        unavailable: &[],
        unhealthy: &[],
        demand: coordinator_demand(),
    })
    .unwrap()
}

// ── gate one: the scope ─────────────────────────────────────────────────────

/// **THE 19:34 DECISION, REPRODUCED.** With `aui:default` scoped to the three
/// accounts that existed when the key was written, the gate cannot see `cc2` at
/// all — and the thread is carried onto the 91% account.
///
/// This test asserts the BUG, so that the reconstruction in
/// `docs/placement-new-account-fix.md` is executable rather than narrated. It
/// passes before the fix and after it: the stale scope is still a stale scope,
/// what changes is that boot no longer leaves one behind.
#[test]
fn a_stale_scope_hides_the_new_account_from_the_whole_admission() {
    let (b, _store) = his_pool_at_1934();
    let mut cfg = threads::budgets::BudgetConfig::new("aui:default");
    cfg.accounts = vec![BROOM.into(), GMAIL.into(), CC1.into()];
    b.set_config(&cfg).unwrap();

    let accounts = pool(&[BROOM, GMAIL, CC1, CC2]);
    let history = vec![CC1.to_string()];
    // The first ask: the thread's only history is cc1, and cc1 is over.
    let park = admit_for(&b, &accounts, CC1, &history, Some("aui:default"));
    let Admission::Park { cause, reason, .. } = &park else {
        panic!("cc1 is at 100%: the thread cannot stay there — got {park:?}");
    };
    assert_eq!(*cause, ParkCause::Continuity, "{reason}");
    assert!(
        !reason.contains(CC2),
        "THE SILENCE IS THE DEFECT: cc2 is neither admissible nor blocked, it is \
         absent — and the park text the operator reads never mentions it: {reason}"
    );

    // The second ask is `ThreadManager::failover_target`: same request, history
    // cleared. The escape hatch works exactly as designed and still cannot
    // reach cc2, because scope is applied above it.
    let over = admit_for(&b, &accounts, CC1, &[], Some("aui:default"));
    assert_eq!(
        over,
        Admission::Admit {
            switch_to: Some(placeable(BROOM))
        },
        "with history cleared the ranker still only sees the scoped three, so it \
         ports the conversation onto the 91% account"
    );
}

/// **THE FIX, GATE ONE.** `ensure_default` must reconcile the fleet-wide key's
/// scope to the CURRENT pool, not only seed it once.
///
/// Its own docstring already rules that this key's correct scope is every
/// registered account — *"The DEFAULT key is the one case where every
/// registered account is the right scope"* — and `purge_account_from_scopes`
/// applies the identical statement to a row written before the pool existed.
/// The defect is only that the statement was made once, at creation, and never
/// again.
#[test]
fn the_fleet_wide_default_scope_takes_in_an_account_added_after_it_was_created() {
    let store = Store::open_in_memory().unwrap();
    let b = Budgets::new(store);

    assert!(
        b.ensure_default(&[BROOM.into(), GMAIL.into(), CC1.into()])
            .unwrap(),
        "the first call seeds the key"
    );
    // The operator adds a fourth login and the daemon reboots onto a wider pool.
    let widened = b
        .ensure_default(&[BROOM.into(), GMAIL.into(), CC1.into(), CC2.into()])
        .unwrap();
    assert!(widened, "a pool that grew must re-scope the fleet-wide key");

    let cfg = b.get_config("aui:default").unwrap().expect("the key exists");
    assert!(
        cfg.accounts.iter().any(|a| a == CC2),
        "an account in the pool and not in the fleet-wide scope can never be \
         placed on by any keyed thread: {:?}",
        cfg.accounts
    );
    assert_eq!(cfg.accounts.len(), 4, "no duplicates: {:?}", cfg.accounts);
    assert!(
        !b.ensure_default(&[BROOM.into(), GMAIL.into(), CC1.into(), CC2.into()])
            .unwrap(),
        "idempotent: a pool that did not change rewrites nothing"
    );
}

/// **AND IT NEVER TOUCHES A CEILING.** The never-overwrite rule exists because
/// *"boot must never quietly undo a limit he set"*. Widening a scope is not
/// undoing a limit, and the reconciliation must prove it does not become one.
#[test]
fn reconciling_the_scope_leaves_every_ceiling_and_the_disable_exactly_as_they_were() {
    let store = Store::open_in_memory().unwrap();
    let b = Budgets::new(store);
    b.ensure_default(&[BROOM.into(), GMAIL.into()]).unwrap();

    let mut cfg = b.get_config("aui:default").unwrap().unwrap();
    cfg.weekly_pct = Some(60.0);
    cfg.cash_usd = Some(25.0);
    cfg.on_trip = threads::budgets::TripAction::Kill;
    b.set_config(&cfg).unwrap();

    b.ensure_default(&[BROOM.into(), GMAIL.into(), CC2.into()])
        .unwrap();

    let after = b.get_config("aui:default").unwrap().unwrap();
    assert_eq!(after.weekly_pct, Some(60.0), "a ceiling he set survived boot");
    assert_eq!(after.cash_usd, Some(25.0));
    assert_eq!(after.on_trip, threads::budgets::TripAction::Kill);
    assert!(after.accounts.iter().any(|a| a == CC2));
}

/// **IT WIDENS ONLY.** A name in the persisted scope that is not in today's
/// pool is LEFT THERE. Dropping names is `purge_account_from_scopes`'s job, it
/// is told which name to drop by a caller that knows what the name means, and
/// this reconciliation must not quietly acquire that power — a pool read that
/// came back short for any reason would otherwise silently narrow the fleet.
#[test]
fn reconciling_the_scope_never_removes_a_name() {
    let store = Store::open_in_memory().unwrap();
    let b = Budgets::new(store);
    b.ensure_default(&[BROOM.into(), GMAIL.into(), CC1.into()])
        .unwrap();

    b.ensure_default(&[BROOM.into(), CC2.into()]).unwrap();

    let cfg = b.get_config("aui:default").unwrap().unwrap();
    for name in [BROOM, GMAIL, CC1, CC2] {
        assert!(
            cfg.accounts.iter().any(|a| a == name),
            "{name} left the scope: {:?}",
            cfg.accounts
        );
    }
}

/// **AND IT IS THE FLEET-WIDE KEY ALONE.** A key the operator scoped by hand is
/// a decision he is allowed to have made — *"narrowing a budget to a scope is a
/// thing the operator is allowed to mean"* — and the crate never guesses a
/// scope for one.
#[test]
fn a_hand_scoped_key_is_not_widened_by_the_pool_growing() {
    let store = Store::open_in_memory().unwrap();
    let b = Budgets::new(store);
    let mut mine = threads::budgets::BudgetConfig::new("ops:nightly");
    mine.accounts = vec![GMAIL.into()];
    b.set_config(&mine).unwrap();

    b.ensure_default(&[BROOM.into(), GMAIL.into(), CC2.into()])
        .unwrap();

    let after = b.get_config("ops:nightly").unwrap().unwrap();
    assert_eq!(
        after.accounts,
        vec![GMAIL.to_string()],
        "a budget the operator scoped by hand is his, not the pool's"
    );
}

// ── gate two: the history intersection ──────────────────────────────────────

/// **THE REGRESSION TEST THE OPERATOR ASKED FOR.** A pool account with ZERO
/// `thread_accounts` rows and the best reading in the pool must be reachable by
/// a thread that already exists.
///
/// The thread is running, legally, on `owner-account` at 91% against a
/// 95% threshold. It has history there and nowhere else. `cc2` is in the pool,
/// in scope, at 1%, and the thread has never touched it.
///
/// Before the fix the gate answers `Admit { switch_to: None }` — "stay" — every
/// turn, forever, because `cc2` is filtered out of the candidate field before
/// any criterion is computed for it.
#[test]
fn an_account_with_no_history_and_the_best_reading_is_reachable_by_an_existing_thread() {
    let (b, _store) = his_pool_at_1934();
    let accounts = pool(&[BROOM, CC2]);
    let history = vec![BROOM.to_string()];

    let decision = admit_for(&b, &accounts, BROOM, &history, None);

    assert_eq!(
        decision,
        Admission::Admit {
            switch_to: Some(placeable(CC2))
        },
        "a thread four points from its ceiling must be able to move onto an \
         empty account it has never run on — otherwise an account added to the \
         pool can never be used. Got {decision:?}"
    );
}

/// **THE SAME, THROUGH THE REAL 19:34 FIELD, IN BOTH ITS STEPS.** Scope
/// reconciled, all four accounts, the thread's only history on a cc1 that is at
/// 100%.
///
/// The thread may not stay and holds no session on any target, so the gate still
/// parks it for continuity — and it MUST. That park is what routes the move
/// through `ThreadManager::failover_target`, whose `port_must_succeed` contract
/// is the difference between carrying a conversation and forking one (DEC-56).
/// The fix does not remove that step; it fixes what the second ask can see.
///
/// The second ask is failover's, reproduced exactly: the same request with
/// `history_accounts` emptied. At 19:34 it answered `owner-account` at 91%. It now
/// answers cc2 at 1% — the same account `accounts.pressure` had been reporting
/// at rank 1 the whole time.
#[test]
fn the_1934_thread_reaches_cc2_once_the_scope_holds_the_whole_pool() {
    let (b, _store) = his_pool_at_1934();
    // THE ROW AS IT STOOD ON HIS MACHINE: written on the boot that created the
    // key, three accounts long, and cc2 registered a day later.
    let mut stale = threads::budgets::BudgetConfig::new("aui:default");
    stale.accounts = vec![BROOM.into(), GMAIL.into(), CC1.into()];
    b.set_config(&stale).unwrap();
    // Boot, on a pool that has since grown. This is the whole of gate one.
    b.ensure_default(&[BROOM.into(), GMAIL.into(), CC1.into(), CC2.into()])
        .unwrap();

    let accounts = pool(&[BROOM, GMAIL, CC1, CC2]);
    let history = vec![CC1.to_string()];

    let first = admit_for(&b, &accounts, CC1, &history, Some("aui:default"));
    let Admission::Park { cause, reason, .. } = &first else {
        panic!("a thread that may not stay is failover's to move, not a switch_to: {first:?}");
    };
    assert_eq!(*cause, ParkCause::Continuity, "{reason}");
    assert!(
        reason.contains(CC2),
        "the park text must now NAME cc2 as admissible — at 19:34 it was absent \
         from both halves of this sentence and that silence was the defect: {reason}"
    );

    // `ThreadManager::failover_target`: the same ask, history cleared.
    let over = admit_for(&b, &accounts, CC1, &[], Some("aui:default"));
    assert_eq!(
        over,
        Admission::Admit {
            switch_to: Some(placeable(CC2))
        },
        "cc2 was measured, ranked first on pressure and 90 points below the \
         account the daemon actually chose. Got {over:?}"
    );
}

// ── and the churn it must NOT cause ─────────────────────────────────────────

/// **A SETTLED THREAD DOES NOT MOVE.** Opening the field is not licence to
/// thrash. Two healthy accounts that tie on every substantive criterion leave
/// the thread exactly where it is, because an account the thread holds no
/// session on may not win on a TIE-BREAK — only by beating every
/// session-bearing candidate on a measured criterion.
#[test]
fn a_newcomer_never_wins_a_tie_and_a_settled_thread_stays_put() {
    let store = Store::open_in_memory().unwrap();
    let b = Budgets::new(store.clone());
    let now = chrono::Utc::now();
    let reset = (now + chrono::Duration::hours(80)).to_rfc3339();
    // Identical readings, identical windows, identical calibration: nothing
    // measurable separates them at all.
    for name in [BROOM, CC2] {
        b.observe(name, Axis::Session, 10.0, None, "usage_poll").unwrap();
        b.observe(name, Axis::WeeklyAll, 20.0, Some(reset.clone()), "usage_poll").unwrap();
        b.observe(name, Axis::WeeklyFable, 15.0, Some(reset.clone()), "usage_poll").unwrap();
        calibrate(&store, name, Axis::Session, 3_000_000.0);
        calibrate(&store, name, Axis::WeeklyAll, 20_000_000.0);
        calibrate(&store, name, Axis::WeeklyFable, 1_000_000.0);
    }

    let accounts = pool(&[BROOM, CC2]);
    let history = vec![BROOM.to_string()];
    let decision = admit_for(&b, &accounts, BROOM, &history, None);

    assert_eq!(
        decision,
        Admission::Admit { switch_to: None },
        "nothing measurable separates the two accounts, so moving the thread \
         would buy a cold prompt cache for no gain. Got {decision:?}"
    );
}

/// **AND A SESSION-BEARING ACCOUNT STILL OUTRANKS A COLD ONE ON A TIE.** The
/// thread is on `cc1`, which is over its threshold, so it must move. Both
/// `owner-account` (history) and `cc2` (none) are clear and tie on every criterion.
/// It goes to the one it already has a conversation on.
#[test]
fn between_two_tied_targets_the_one_with_a_session_wins() {
    let store = Store::open_in_memory().unwrap();
    let b = Budgets::new(store.clone());
    let now = chrono::Utc::now();
    let reset = (now + chrono::Duration::hours(80)).to_rfc3339();
    for name in [BROOM, CC2] {
        b.observe(name, Axis::Session, 10.0, None, "usage_poll").unwrap();
        b.observe(name, Axis::WeeklyAll, 20.0, Some(reset.clone()), "usage_poll").unwrap();
        b.observe(name, Axis::WeeklyFable, 15.0, Some(reset.clone()), "usage_poll").unwrap();
        calibrate(&store, name, Axis::Session, 3_000_000.0);
        calibrate(&store, name, Axis::WeeklyAll, 20_000_000.0);
        calibrate(&store, name, Axis::WeeklyFable, 1_000_000.0);
    }
    b.observe(CC1, Axis::WeeklyAll, 100.0, Some(reset), "usage_poll").unwrap();

    let accounts = pool(&[BROOM, CC1, CC2]);
    let history = vec![CC1.to_string(), BROOM.to_string()];
    let decision = admit_for(&b, &accounts, CC1, &history, None);

    assert_eq!(
        decision,
        Admission::Admit {
            switch_to: Some(placeable(BROOM))
        },
        "continuity decides a tie; it must not decide a measured difference. \
         Got {decision:?}"
    );
}

/// **CON-27 SURVIVES THE WIDENING.** The field the ranker sees is still built
/// by FILTERING the pool the caller handed in, so nothing the caller did not
/// name can come back out — including the machine default, which is not
/// expressible as a `Placeable` at all.
#[test]
fn the_widened_field_can_still_only_name_an_account_the_caller_supplied() {
    let (b, _store) = his_pool_at_1934();
    // cc2 is in the STORE (it has readings) but NOT in the pool handed in.
    let accounts = pool(&[BROOM]);
    let history = vec![BROOM.to_string()];
    let decision = admit_for(&b, &accounts, BROOM, &history, None);

    assert_eq!(
        decision,
        Admission::Admit { switch_to: None },
        "an account with readings but no seat in the pool is not a destination. \
         Got {decision:?}"
    );
}

/// **AND THE WIDENING STOPS AT THE "FINISHING OUT" STATE.** A thread whose
/// current account is admissible but is not a placement TARGET — a dead login, a
/// DISABLED account, or one past the share it was lent — keeps the field it had.
///
/// The operator's rule for all three: *"Overage we've determined is okay while
/// the current workload is finishing. But then it needs to be hard clamped — not
/// available for placement as well."* Work already running there finishes; what
/// stops is NEW placement onto it. A widened field would have turned that into
/// an eviction the moment he set a share to zero, and a disable — unlike a
/// clamp — has no weekly reset coming to undo it.
///
/// This is deliberately NOT a case the new-account fix reaches into. It pins
/// the GATE alone, handed an `unavailable` list that omits the disabled
/// account. H179: the runtime no longer hands it one — a disabled account is in
/// `account_unavailability`, so through the whole manager the thread is failed
/// over before its next turn
/// (`account_disable.rs::a_thread_with_history_only_on_the_disabled_account_
/// moves_before_its_next_turn`).
#[test]
fn a_thread_finishing_out_on_a_disabled_account_is_not_evicted_onto_a_newcomer() {
    let store = Store::open_in_memory().unwrap();
    let b = Budgets::new(store.clone());
    let now = chrono::Utc::now();
    let reset = (now + chrono::Duration::hours(80)).to_rfc3339();
    // `cc2` is healthy and empty; `broom` is where the thread lives and is the
    // account he has just switched off.
    for name in [BROOM, CC2] {
        b.observe(name, Axis::Session, 5.0, None, "usage_poll").unwrap();
        b.observe(name, Axis::WeeklyAll, 10.0, Some(reset.clone()), "usage_poll").unwrap();
        b.observe(name, Axis::WeeklyFable, 5.0, Some(reset.clone()), "usage_poll").unwrap();
        calibrate(&store, name, Axis::Session, 3_000_000.0);
        calibrate(&store, name, Axis::WeeklyAll, 20_000_000.0);
        calibrate(&store, name, Axis::WeeklyFable, 1_000_000.0);
    }
    store.set_account_disabled(BROOM, true, None).unwrap();
    assert!(b.is_disabled(BROOM), "fixture: the account is switched off");

    let accounts = pool(&[BROOM, CC2]);
    let history = vec![BROOM.to_string()];
    let decision = admit_for(&b, &accounts, BROOM, &history, None);

    assert_eq!(
        decision,
        Admission::Admit { switch_to: None },
        "disabling an account stops NEW placement onto it; it does not evict the \
         work already there. Got {decision:?}"
    );
}
