//! **TAKING AN ACCOUNT OUT OF THE POOL — the MANAGER half.**
//!
//! # 2026-09-20: this is no longer a share of zero
//!
//! Everything below shipped on 2026-09-08 as *"a true 0% share takes an account
//! out of placement selection"*, and the operator's 2026-09-08 words quoted
//! further down are still the reason the EXCLUSION exists. What changed is
//! where the state lives. He separated the two questions the share column had
//! been answering:
//!
//! > *"I want disabled to be independent of share. Whether to include it in the
//! > pool supersedes how much should be made available to the pool."*
//!
//! So membership is `account_disabled` (its own table, its own row), allocation
//! stays `pool_shares.share`, and every test in this file now spells the disable
//! `set_account_disabled(.., true, ..)` rather than `set_pool_share(.., 0.0, ..)`.
//! The PLACEMENT claims are unchanged and are asserted against the same ground;
//! what is new is that each one now also holds with a real, non-zero share
//! sitting beside the disable, which is the case the old model could not
//! represent at all.
//!
//! The original framing, kept because it is why the exclusion is an exclusion:
//!
//! The operator, 2026-09-08, on an account of his that is no longer a paid
//! subscription:
//!
//! > *"One of these accounts is no longer a paid account. It's just free, so
//! > it's probably not even working… I tried changing the share to like 0.001%,
//! > but I think that just means that there's still a non-zero pressure on it…
//! > the simplest is just to be able to set a true 0% share on an account just
//! > to disable usage on it, and then we should just like not consider it for
//! > placement. It should be out of the placement selection. That's the
//! > cleanest."*
//!
//! He diagnosed it correctly and the diagnosis is the design. Pressure is a
//! RATE and the share is its DENOMINATOR, so a small share does not produce a
//! small pressure — `budgets::tests::a_point_oh_oh_one_percent_share_does_not_
//! reduce_pressure_it_explodes_it` pins that arithmetic. There is no number
//! that means "off", so "off" is not a number: it is an EXCLUSION from the
//! candidate set, applied before ranking.
//!
//! # What this file covers, and what it does not
//!
//! The RANKING half — that a disabled account is absent from the field whether it
//! would have won or lost, and that the arithmetic never sees the zero — lives
//! in `budgets.rs`'s own test module, next to the code, with the mutations
//! recorded. This file is the MANAGER half, and it exists for the four claims
//! that can only be made against a running manager:
//!
//! 1. a thread already sitting on an account that gets disabled MOVES OFF BY
//!    ITSELF, because the stickiness gate is gone and every turn re-ranks;
//! 2. a thread with nowhere to go is NOT STRANDED — it keeps running where it
//!    is, and disabling never becomes an eviction;
//! 3. **DISABLED IS NOT BARRED**, and the two are distinguishable in the DATA —
//!    different tables, different wire fields, different park causes;
//! 4. the other accounts' shares are UNCHANGED by one being zeroed, because a
//!    share is a fraction OF ITS OWN ACCOUNT and there is no pool total to
//!    re-normalise.
//!
//! # Adversarial
//!
//! Every test names the MUTATION that makes it red. A green nobody has forced
//! to fail is not evidence.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;
use threads::gauge::Axis;
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

// ════════════════════════════════════════════════════════════════════════════
// fixtures — deliberately the same shape as `pool_share_and_clamp.rs`, so the
// disable and the clamp are exercised against identical ground and any difference
// in outcome is a difference in the mechanism rather than in the harness.
// ════════════════════════════════════════════════════════════════════════════

fn set_env() {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        first_exec::warmed_abs(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake-claude")),
    );
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-disable-tests-home"),
    );
}

fn config_at(dir: &Path, extra: &[&str]) -> Config {
    let mut accounts = BTreeMap::new();
    for name in extra {
        accounts.insert(name.to_string(), dir.join(name).to_string_lossy().to_string());
    }
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        // The gauge's own bar OFF, so nothing here can pass because an account
        // was barred at 100% for an unrelated reason. DISABLING has to be the
        // thing doing the work — and telling a disable apart from a bar is half
        // of what this file is for.
        gauge_bar_pct: 0.0,
        ..Default::default()
    }
}

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

fn weekly(m: &ThreadManager, account: &str, pct: f64) {
    m.budgets
        .observe(
            account,
            Axis::WeeklyAll,
            pct,
            Some("2099-01-01T00:00:00Z".to_string()),
            "usage_poll",
        )
        .unwrap();
}

fn session(m: &ThreadManager, account: &str, pct: f64) {
    m.budgets
        .observe(account, Axis::Session, pct, None, "usage_poll")
        .unwrap();
}

/// A settings patch that says ONE thing: where this thread runs. Everything
/// else is absent, which matters — a patch that does not mention the account is
/// not permission to move the thread, and a helper that set unrelated fields
/// would be exercising `reresolve_account` instead of the disable.
fn account_patch(choice: threads::settings::AccountChoice) -> threads::settings::SettingsPatch {
    threads::settings::SettingsPatch {
        account: Some(choice),
        ..Default::default()
    }
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if f() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    f()
}

// ════════════════════════════════════════════════════════════════════════════
// 1. THREADS ALREADY PLACED THERE
// ════════════════════════════════════════════════════════════════════════════

/// **A live thread MOVES OFF a disabled account by itself — verified, not
/// assumed.**
///
/// The stickiness gate was removed on this board (`budgets::admit`: *"every
/// single time we start a new turn… it needs to go through placement every
/// single time"*), so the claim "a live thread will re-rank and leave" follows
/// from that removal rather than from anything the disable does. A claim that
/// follows from someone else's change is exactly the kind this board has been
/// burned by, so it is asserted here against a running manager rather than
/// reasoned about.
///
/// The thread is given a conversation on BOTH accounts first, because a thread
/// with history moves only to an account it already holds a session on —
/// continuity outranks throughput, and without that setup this test would be
/// asserting the continuity park instead of the move.
///
/// **THE MUTATION.** Re-introduce the stickiness gate (`else if on_admissible {
/// None }` ahead of the re-rank branch in `admit`) and this goes red: the thread
/// stays on the disabled account forever, and the disable silently applies to new
/// threads only.
#[tokio::test]
async fn a_live_thread_moves_off_an_account_the_operator_disables() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-disabled", "zz-second"])).unwrap();
    m.start_watchers();

    // THE RANKING IS RIGGED TOWARD THE ACCOUNT THAT WILL BE DISABLED, and it has
    // to be. If the two accounts merely TIED, the winner would be settled by the
    // persistent round-robin cursor — and a fixture that depends on a cursor
    // position asserts something different every time the file is reordered.
    // Untouched against 80% consumed is a difference every criterion agrees on.
    weekly(&m, "aa-disabled", 0.0);
    session(&m, "aa-disabled", 0.0);
    weekly(&m, "zz-second", 80.0);
    session(&m, "zz-second", 80.0);

    // Establish a conversation on `zz-second` FIRST…
    let mut s = settings(tmp.path());
    s.account = threads::settings::AccountChoice::Named("zz-second".into());
    let id = m.create_thread(None, s).unwrap();
    m.send(&id, "first".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| m
            .store
            .thread_accounts(&id)
            .unwrap()
            .contains(&"zz-second".to_string()))
        .await,
        "fixture: the thread needs a session on the second account to move TO — \
         continuity outranks throughput, so a thread with no history there would \
         park instead of moving, which is a different test"
    );

    // …and then on `aa-disabled`, where it will be sitting when the disable lands.
    m.update_settings(
        &id,
        account_patch(threads::settings::AccountChoice::Named("aa-disabled".into())),
    )
    .unwrap();
    m.send(&id, "second".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| m
            .store
            .thread_accounts(&id)
            .unwrap()
            .contains(&"aa-disabled".to_string()))
        .await,
        "fixture: and a conversation on the account about to be disabled"
    );

    // Hand it back to the RANKER. A NAMED account is a PIN, and a pin is never
    // re-placed — that is a separate, deliberate rule, and asserting the disable
    // against a pinned thread would be asserting the wrong thing entirely.
    //
    // This patch itself re-places the thread (a patch that names the account IS
    // permission to move it), which is the second reason the fixture is rigged:
    // it has to land back on `aa-disabled`, and it does because every criterion
    // prefers it.
    m.update_settings(&id, account_patch(threads::settings::AccountChoice::Auto))
        .unwrap();
    assert!(
        eventually(|| m.store.get_thread(&id).unwrap().unwrap().account
            == Some("aa-disabled".to_string()))
        .await,
        "fixture: the thread must be sitting on the account when it is disabled, \
         and be there by the RANKER's choice rather than by a pin. account={:?}",
        m.serving_account(&id).unwrap()
    );

    // ── THE DISABLE, and it is his own hand on the control ──
    m.store
        .set_account_disabled("aa-disabled", true, Some("no longer a paid account"))
        .unwrap();
    assert!(m.budgets.is_disabled("aa-disabled"));

    m.send(&id, "third".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| m.store.get_thread(&id).unwrap().unwrap().account
            == Some("zz-second".to_string()))
        .await,
        "a live thread re-ranks every turn and leaves a disabled account BY \
         ITSELF — and it leaves ONTO THE WORSE-RANKED ACCOUNT, which is what \
         makes this the exclusion moving it and not the ranking. account={:?} \
         park={:?}",
        m.serving_account(&id).unwrap(),
        m.park_state(&id).unwrap()
    );
}

/// **SUPERSEDED BY H179 (2026-10-16): a thread whose whole conversation is on
/// an account he disables MOVES — conversation carried — before its next turn.**
///
/// This test used to pin the opposite ("work already running on a disabled
/// account finishes there … a thread whose only admissible home is the disabled
/// account stays there and RUNS"). The operator ruled it out after cc2 was
/// tried for a turn while the dashboard showed it unusable: *disabled* is one of
/// the signals that "should be interpreted as account not available", and an
/// unavailable account is never tried, on any path. A turn ALREADY EXECUTING
/// there is not torn down; the NEXT turn is failed over by the admission gate
/// (`account_unavailability` feeds `barred_accounts`), carrying the transcript,
/// exactly as a usage-limit window does.
///
/// **THE MUTATION.** Drop the `account_unavailability` keys from
/// `barred_accounts` and this goes red: the thread stays and runs on the
/// disabled account.
#[tokio::test]
async fn a_thread_with_history_only_on_the_disabled_account_moves_before_its_next_turn() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-disabled", "zz-second"])).unwrap();
    m.start_watchers();

    // Rigged toward `aa-disabled` so a plain `Auto` creation lands there with NO
    // settings patch at all. That matters here: a patch naming an account
    // re-places the thread, so a fixture that moved it by hand would be
    // measuring `reresolve_account` rather than the admission gate.
    weekly(&m, "aa-disabled", 0.0);
    session(&m, "aa-disabled", 0.0);
    weekly(&m, "zz-second", 80.0);
    session(&m, "zz-second", 80.0);

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "aa-disabled",
        "fixture: the ranker must put it here, unpinned"
    );
    m.send(&id, "first".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| m
            .store
            .thread_accounts(&id)
            .unwrap()
            .contains(&"aa-disabled".to_string()))
        .await,
        "fixture: one account of history"
    );
    assert_eq!(
        m.store.thread_accounts(&id).unwrap(),
        vec!["aa-disabled".to_string()],
        "fixture: AND ONLY ONE — the thread must have nowhere else it could go \
         without losing its conversation, or this is not a test about stranding"
    );

    m.store.set_account_disabled("aa-disabled", true, None).unwrap();
    assert!(m.budgets.is_disabled("aa-disabled"));

    m.send(&id, "second".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| m.store.get_thread(&id).unwrap().unwrap().account
            == Some("zz-second".to_string())
            && m.store.thread_accounts(&id).unwrap().contains(&"zz-second".to_string())
            && m.park_state(&id).unwrap() == serde_json::Value::Null)
        .await,
        "a disabled account is not available (H179): the next turn is failed over, \
         conversation carried, and runs on the account that is. account={:?} park={:?}",
        m.serving_account(&id).unwrap(),
        m.park_state(&id).unwrap()
    );
}

/// **A FRESH thread is never created onto a disabled account while anything else
/// is placeable — including when the disabled account is the one the ranking
/// would otherwise prefer.**
///
/// Thread creation goes through `ThreadManager::auto_place`, which ranks
/// `selectable_pool()` DIRECTLY and never passes through `admit`. That second
/// door is why the exclusion lives in `place` rather than only in the admission
/// gate: an exclusion written into `admit` alone would be correct for every
/// turn and wrong for every creation.
///
/// **THE MUTATION.** Move the `is_disabled` skip out of `Budgets::place` and into
/// `admit`'s target loop only, and this goes red — the thread is CREATED on the
/// disabled account and only discovers the problem on its first turn.
#[tokio::test]
async fn a_fresh_thread_is_not_created_on_a_disabled_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-disabled", "zz-second"])).unwrap();

    // `aa-disabled` is untouched and `zz-second` is deep into its week, so the
    // ranking prefers `aa-disabled` on pressure AND it sorts first by name. Every
    // criterion that votes points at the account being disabled.
    weekly(&m, "aa-disabled", 0.0);
    weekly(&m, "zz-second", 80.0);
    session(&m, "aa-disabled", 0.0);
    session(&m, "zz-second", 80.0);

    // Fixture check: re-enabled, this is where a fresh thread lands.
    let control = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(
        m.serving_account(&control).unwrap(),
        "aa-disabled",
        "fixture: the account about to be disabled must be the one placement \
         actually prefers, or this test proves nothing"
    );

    m.store.set_account_disabled("aa-disabled", true, None).unwrap();
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "zz-second",
        "a disabled account is out of selection at CREATE time too, not only at \
         admission — `auto_place` never passes through the gate"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// 2. DISABLED IS NOT BARRED
// ════════════════════════════════════════════════════════════════════════════

/// **DISABLED IS NOT BARRED, and the two are distinguishable in the DATA — not
/// merely in a rendering.**
///
/// A bar is the system reporting a TEMPORARY FAILURE it observed: a 429 window
/// (`accounts.limit_until`), or a login that failed to authenticate
/// (`accounts.dead_since`). Both are written by the daemon, both are inferred
/// from an error, and both lift themselves — a limit on its reset, a dead flag
/// on the next successful turn.
///
/// Disabling is the operator's DELIBERATE CHOICE. It lives in a different table
/// (`account_disabled`), is written only by him, and nothing in this crate lifts it.
/// Conflating the two costs him in both directions: a bar reported as a disable
/// hides a real failure, and a disable reported as a bar tells him to wait for a
/// reset that will never come.
///
/// **THE MUTATION.** Implement the disable by writing `dead_since` on the account
/// row — the "simpler, we already have an exclusion mechanism" version — and
/// every assertion in the first block goes red at once.
#[test]
fn a_disabled_account_is_not_barred_and_the_data_says_so() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-disabled", "zz-second"])).unwrap();
    weekly(&m, "aa-disabled", 20.0);

    m.store.set_account_disabled("aa-disabled", true, Some("no longer paid")).unwrap();
    assert!(m.budgets.is_disabled("aa-disabled"));

    // ── THE ACCOUNT ROW IS UNTOUCHED. Nothing that means "the system found a
    // problem here" has been written.
    let states: std::collections::BTreeMap<String, threads::accounts::AccountState> = m
        .store
        .list_account_states()
        .unwrap()
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect();
    let row = states.get("aa-disabled").cloned().unwrap_or_default();
    assert_eq!(row.dead_since, None, "a disabled account is not a dead login");
    assert_eq!(row.dead_reason, None);
    assert_eq!(row.limit_until, None, "a disabled account is not a usage-limit window");
    assert!(row.healthy(), "a disabled account is HEALTHY — it simply is not in use");

    // ── AND THE TWO EXCLUSION SETS DO NOT CLAIM IT. These are the sets that
    // mean "barred": one for logins proven dead, one for accounts inside an
    // observed limit window. A disabled account belongs to neither and must not be
    // smuggled into either, because everything downstream — failover, the
    // health board, the retry ladder — reads them as evidence of failure.
    let names: Vec<String> = ["aa-disabled", "zz-second"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let now = chrono::Utc::now().to_rfc3339();
    assert!(
        threads::accounts::unhealthy_accounts(&names, &states).is_empty(),
        "a disabled account is not an unhealthy one"
    );
    assert!(
        threads::accounts::unavailable_accounts(&names, &states, &now).is_empty(),
        "a disabled account is not one that cannot serve a turn — it can, it is \
         just not being asked to"
    );

    // ── ON THE WIRE, THE DISTINCTION SURVIVES. The pressure payload says
    // `disabled`, and it says it as its own field rather than as a number the
    // caller has to interpret.
    let rows = m
        .budgets
        .pressure_rows(&names, chrono::Utc::now());
    let p = rows["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account"] == "aa-disabled")
        .expect("STILL VISIBLE: a disabled account is never dropped from a payload");
    assert_eq!(p["disabled"], json!(true));
    assert_eq!(p["unranked_reason"], json!("disabled"));
    assert!(
        p["pressure"].is_null(),
        "null, never 0 — zero pressure is the BUSIEST possible reading and is \
         the opposite fact"
    );
    assert_eq!(
        p["observed_pct"],
        json!(20.0),
        "with its readings intact: he asked to be able to SEE a disabled account"
    );

    // ── AND THE HEALTHY ACCOUNT IS UNMARKED, so none of the above is a
    // payload-wide default that would have passed for any input.
    let other = rows["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account"] == "zz-second")
        .unwrap();
    assert_eq!(other["disabled"], json!(false));
}

// ════════════════════════════════════════════════════════════════════════════
// 3. THE REMAINDER
// ════════════════════════════════════════════════════════════════════════════

/// **ZEROING ONE SHARE CHANGES NOTHING ABOUT WHAT THE OTHERS MEAN — because a
/// share is a fraction OF ITS OWN ACCOUNT and there is no pool total.**
///
/// This is the question "what does the code do with the remainder today, and is
/// that right?", answered as a test rather than as a claim.
///
/// **WHAT IT DOES: nothing, and nothing is correct here.** A share answers *"how
/// much of THIS account am I lending the pool?"* — `pool_share` reads one row
/// per account, `pool_pressure` divides that account's own consumption by that
/// account's own share, and no function anywhere sums the shares or normalises
/// against a total. Two accounts at 0.95 are not "190% of a pool"; they are two
/// logins each offering 95% of itself. So there IS no remainder to redistribute,
/// and the sum was never a quantity that meant anything.
///
/// **WHY THAT IS RIGHT, and not merely convenient.** If shares were slices of a
/// pool total, disabling one account would silently RE-PRICE every other account's
/// pressure — a change he did not ask for, applied to accounts he did not touch,
/// with the number he set for each of them quietly meaning something new. The
/// operator's stated model is the per-account one (*"if I only gave 10% of an
/// account… the amount available to it is only the 10% to begin with"*), and it
/// is the model that makes disabling one account a local act.
///
/// **THE MUTATION.** Normalise shares against their sum anywhere on the read
/// path and this goes red immediately: `zz-second`'s pressure moves when an
/// account it has nothing to do with is disabled.
#[test]
fn disabling_one_account_does_not_re_price_the_others() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-disabled", "zz-second", "mm-third"])).unwrap();
    let now = chrono::Utc::now();

    for a in ["aa-disabled", "zz-second", "mm-third"] {
        weekly(&m, a, 30.0);
    }
    m.store.set_pool_share("aa-disabled", 0.5, None).unwrap();
    m.store.set_pool_share("zz-second", 0.95, None).unwrap();
    m.store.set_pool_share("mm-third", 0.4, None).unwrap();

    let before: Vec<(f64, f64)> = ["zz-second", "mm-third"]
        .iter()
        .map(|a| {
            let r = m.budgets.pressure_reading(a, now);
            (r.share, r.pressure.unwrap())
        })
        .collect();
    // Fixture: the shares do NOT sum to 1, and never did. If some future change
    // starts treating them as slices of a total, it has to reckon with the fact
    // that the live pool does not add up to one — the operator's own machine
    // runs 0.95 on one account with others unset at 1.0.
    assert!(
        0.5 + 0.95 + 0.4 > 1.0,
        "fixture: these are per-account fractions, not slices of a pool"
    );

    m.store.set_account_disabled("aa-disabled", true, None).unwrap();

    let after: Vec<(f64, f64)> = ["zz-second", "mm-third"]
        .iter()
        .map(|a| {
            let r = m.budgets.pressure_reading(a, now);
            (r.share, r.pressure.unwrap())
        })
        .collect();
    assert_eq!(
        before, after,
        "disabling one account must not move another account's share OR its \
         pressure by so much as a bit — a share is a statement about ONE \
         account, and re-pricing the others would be a change he never asked for"
    );

    // And the disabled one is the only thing that moved.
    let r = m.budgets.pressure_reading("aa-disabled", now);
    assert!(r.disabled);
    assert_eq!(r.pressure, None);
}

// ════════════════════════════════════════════════════════════════════════════
// 4. THE FULL ROUND TRIP — DISABLE, GONE FROM PLACEMENT, ENABLE, BACK
//
// The operator, msg #531: *"I don't want it to be deleted, though. I still want
// to see it there as one of my accounts, because if I turn it back on, I'm
// going to keep using the same login. So the account's not destroyed. It's just
// not a paid account anymore."*
// ════════════════════════════════════════════════════════════════════════════

/// **DISABLE → OUT OF PLACEMENT → ENABLE → SELECTED AGAIN, ON THE SAME LOGIN.**
///
/// Proven at N >= 2, because placement ARBITRATES: the same account is the one
/// that would have WON in the first pool and the one that would have LOST in the
/// second, and the disabling removes it from both. A single pool where it happens
/// to lose anyway proves nothing — the exclusion and the ranking would be
/// indistinguishable.
///
/// The reversal half is what separates this from `budgets.rs`'s exclusion tests.
/// It asserts the three things he said mattered, in the order he said them: the
/// account is still THERE (registry and `accounts.health`), the LOGIN is
/// untouched (same config dir, same file, byte for byte), and it is placeable
/// again — at the share it had, not at some default.
///
/// **THE MUTATION.** Have the enable path write `DEFAULT_POOL_SHARE` instead of
/// `Budgets::restore_share` and the final share assertion goes red: the account
/// returns at the whole login instead of the 60% he lent, silently re-weighting
/// a pool he only meant to switch back on.
///
/// **THE SECOND MUTATION.** Implement the disabling by removing the account from
/// the registry — the "just delete it" shortcut — and the visibility assertions
/// go red before placement is even consulted.
#[test]
fn the_round_trip_leaves_the_login_untouched_and_the_account_placeable_again() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &["aa-off", "zz-second"])).unwrap();
    let now = chrono::Utc::now();

    // A CREDENTIAL FILE on the disabled account's login dir, so "the login is not
    // destroyed" is checked against a real artifact rather than asserted.
    let login_dir = tmp.path().join("aa-off");
    std::fs::create_dir_all(&login_dir).unwrap();
    let cred = login_dir.join(".credentials.json");
    std::fs::write(&cred, r#"{"token":"do-not-touch"}"#).unwrap();

    m.store.set_pool_share("aa-off", 0.6, Some("he lent 60%")).unwrap();

    // ── N >= 2, POOL A: `aa-off` is the account that WOULD HAVE WON. It is
    // barely used and its sibling is nearly spent, so any ranking puts it first.
    weekly(&m, "aa-off", 1.0);
    weekly(&m, "zz-second", 90.0);
    let pool = m.selectable_pool();
    let won = m
        .budgets
        .place(&threads::budgets::PlacementRequest {
            thresholds: &Default::default(),
            candidates: &pool,
            prefer: None,
            sessions: &[],
            demand: Default::default(),
        })
        .unwrap()
        .expect("some account is placeable");
    assert_eq!(
        won.name(),
        "aa-off",
        "fixture: this pool must be one the account WINS, or the exclusion below \
         proves nothing"
    );

    // ── DISABLE ──
    assert_eq!(m.budgets.pool_share("aa-off").share(), 0.6, "what he lent it");
    m.store.set_account_disabled("aa-off", true, None).unwrap();
    assert!(m.budgets.is_disabled("aa-off"));
    assert_eq!(
        m.budgets.pool_share("aa-off").share(),
        0.6,
        "and STILL what he lent it. There is no number to remember here since \
         2026-09-20, because there is no number the disable took away."
    );
    assert_eq!(
        m.budgets.pool_share("aa-off").lent(),
        None,
        "out of the pool all the same — `lent` is the availability, `share` is \
         the quantity, and this is the one assertion that needs both"
    );

    // GONE FROM PLACEMENT in the pool it would have won.
    let after = m
        .budgets
        .place(&threads::budgets::PlacementRequest {
            thresholds: &Default::default(),
            candidates: &pool,
            prefer: None,
            sessions: &[],
            demand: Default::default(),
        })
        .unwrap();
    assert_eq!(
        after.map(|p| p.name().to_string()),
        Some("zz-second".to_string()),
        "the account he disabled must not be selected even when it is the best \
         candidate on every measure"
    );

    // POOL B: the case where it would have LOST anyway. Same call, same
    // exclusion — so the outcome above is the exclusion and not the ranking.
    weekly(&m, "aa-off", 99.0);
    weekly(&m, "zz-second", 1.0);
    let after_b = m
        .budgets
        .place(&threads::budgets::PlacementRequest {
            thresholds: &Default::default(),
            candidates: &pool,
            prefer: None,
            sessions: &[],
            demand: Default::default(),
        })
        .unwrap();
    assert_eq!(after_b.map(|p| p.name().to_string()), Some("zz-second".to_string()));

    // ── STILL THERE. Not deleted, in the registry and on `accounts.health`.
    assert!(
        m.account_pool().iter().any(|a| a == "aa-off"),
        "a disabled account is still one of his accounts"
    );
    let health = threads::accounts::health_json(
        tmp.path(),
        tmp.path(),
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap();
    let slots = health["slots"].as_array().unwrap();
    assert!(
        slots.iter().any(|s| s["label"] == "aa-off"),
        "DISAPPEARING FROM THE LIST IS THE FAILURE: accounts.health must still \
         carry a disabled account — {health}"
    );

    // ── THE LOGIN IS UNTOUCHED. Same file, same bytes.
    assert!(cred.exists(), "disabling must never delete a credential file");
    assert_eq!(
        std::fs::read_to_string(&cred).unwrap(),
        r#"{"token":"do-not-touch"}"#,
        "disabling must not rewrite the login either"
    );

    // ── ENABLE, AND NO NUMBER IS SUPPLIED BY ANYBODY ──
    //
    // This call used to read `set_pool_share("aa-off", restore_share("aa-off"))`
    // — an enable that had to go and FETCH the share back. It does not any more,
    // and the absence of that argument is the change.
    m.store.set_account_disabled("aa-off", false, None).unwrap();
    assert!(!m.budgets.is_disabled("aa-off"));
    assert_eq!(
        m.budgets.pool_share("aa-off"),
        threads::budgets::PoolShare::pooled(0.6),
        "it comes back at the share it never stopped having — re-enabling is \
         not a re-weighting, and now it cannot be one"
    );

    // AND IT IS SELECTED AGAIN, on the same login, in the pool it used to win.
    weekly(&m, "aa-off", 1.0);
    weekly(&m, "zz-second", 90.0);
    let back = m
        .budgets
        .place(&threads::budgets::PlacementRequest {
            thresholds: &Default::default(),
            candidates: &pool,
            prefer: None,
            sessions: &[],
            demand: Default::default(),
        })
        .unwrap()
        .expect("an enabled account is placeable again");
    assert_eq!(back.name(), "aa-off");
    assert!(
        m.budgets.pressure_reading("aa-off", now).disabled == false,
        "and nothing anywhere still calls it disabled"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
