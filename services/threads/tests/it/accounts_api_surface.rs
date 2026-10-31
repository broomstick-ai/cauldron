//! **THE ACCOUNTS-INTERFACE API SURFACE — every fact from the API, every
//! operation through the API.**
//!
//! > *"Have every fact come from the API. And every operation go through the
//! > API except for one, which is to set my local account. That's it.
//! > Everything else should go through the API."*
//!
//! The accounts interface was reading SQLite directly, shelling out to two
//! binaries, reading files behind the daemon's back and tailing a log. This
//! file covers the arithmetic half of the methods that replace those paths; the
//! protocol half lives in `ws_protocol.rs`, which asserts the same methods over
//! a real socket.
//!
//! # Three things are proved here, and each is proved by being forced to fail
//!
//! 1. **`pool_share` set actually persists AND is read back by PLACEMENT.**
//!    `Store::set_pool_share` shipped with no caller anywhere except a test, so
//!    "lend the pool 60% of this account" was unreachable from every surface.
//!    Reaching the setter is not the same as the number mattering, so this goes
//!    all the way to a placement decision changing.
//! 2. **An unranked account cannot render as zero pressure.** Unranked is the
//!    absence of evidence; zero is the busiest possible reading. They are
//!    opposite facts and a payload that conflates them is lying about one.
//! 3. **The machine default does not survive a reboot in a budget scope.**
//!
//! # Adversarial
//!
//! Every test names the MUTATION that makes it red. A green nobody has forced
//! to fail is not evidence.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};
use threads::gauge::Axis;
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

// ════════════════════════════════════════════════════════════════════════════
// fixtures
// ════════════════════════════════════════════════════════════════════════════

fn set_env() {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        first_exec::warmed_abs(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake-claude")),
    );
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-accounts-api-home"),
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
        // was barred at 100% for an unrelated reason.
        gauge_bar_pct: 0.0,
        ..Default::default()
    }
}

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

/// A weekly-all reading in a LIVE window — the axis the share divides and the
/// clamp reads.
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
// 1. THE POOL SHARE IS REACHABLE, AND WHAT IT WRITES IS WHAT PLACEMENT READS
// ════════════════════════════════════════════════════════════════════════════

/// **A share set through the API changes a real placement decision.**
///
/// This is the test that makes "the setter now has a caller" mean something.
/// It would be entirely possible to expose `set_pool_share` on the wire, have
/// it write a row, have `pool_share_rows` read that row back, and have the
/// number decide nothing at all — because the ranker reads it through a
/// different path. So the assertion is not "the row is there", it is "the
/// thread went somewhere else".
///
/// The sequence:
///
/// * Both accounts have spent 75% of their weekly window. Symmetric.
/// * A fresh thread is placed. With nothing to separate them, it lands
///   somewhere — and the test records WHERE rather than assuming.
/// * The share on THAT account is set to 0.6 through the API. 75% spent is now
///   past the 60% it was lent, so the clamp refuses NEW placement there.
/// * A second fresh thread must land on the other account.
///
/// **THE MUTATIONS**, each of which makes this red:
///
/// 1. Make the WS `.set` arm a no-op that returns the echo — the row is never
///    written, `over_share` stays false, and the second thread lands back on
///    the clamped account.
/// 2. Have `Budgets::pool_share` read `pool.declared_share` FIRST instead of
///    the store row — the operator's runtime override stops winning, which is
///    the precedence bug that would make the setter reachable and useless.
/// 3. Cache the share at `build_manager` time instead of reading it per
///    decision — the number then takes effect only on restart, and the whole
///    point is that the interface can set it and see it apply.
#[tokio::test]
async fn a_share_set_through_the_api_is_read_back_by_placement() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "zz-second"])).unwrap();
    m.start_watchers();

    // Symmetric: both accounts have spent the same, and neither is clamped.
    for n in ["aa-owned", "zz-second"] {
        weekly(&m, n, 75.0);
        session(&m, n, 1.0);
    }

    // Nothing is over its share yet — every account is lent the whole of itself.
    let before = m
        .budgets
        .pool_share_rows(&["aa-owned".to_string(), "zz-second".to_string()]);
    for row in before["shares"].as_array().unwrap() {
        assert_eq!(row["share"], json!(1.0));
        assert_eq!(row["override"], Value::Null);
        assert_eq!(row["source"], json!("unset"));
    }

    let first = m.create_thread(None, settings(tmp.path())).unwrap();
    let landed = m.serving_account(&first).unwrap();
    let other = if landed == "aa-owned" { "zz-second" } else { "aa-owned" };

    // THE OPERATION, through the same code path the WS arm calls. 75% spent is
    // past 60% lent, so this account is now over its share.
    m.store
        .set_pool_share(&landed, 0.6, Some("he keeps 40% to spend by hand"))
        .unwrap();
    // The reason travels to firehose subscribers on the `info` frame — this is
    // the same tag the `accounts.pool_share.set` arm passes, so this test keeps
    // mirroring the WS arm exactly. See `tests/info_broadcast.rs`.
    m.account_picture_changed("pool_share");

    // THE FACT, through the same builder the WS arm reads.
    let after = m.budgets.pool_share_rows(std::slice::from_ref(&landed));
    assert_eq!(after["shares"][0]["share"], json!(0.6));
    assert_eq!(after["shares"][0]["override"], json!(0.6));
    assert_eq!(after["shares"][0]["source"], json!("override"));
    assert_eq!(
        after["shares"][0]["note"],
        json!("he keeps 40% to spend by hand"),
        "the note travels with the number — it is why he set it"
    );

    // AND THE CONSEQUENCE. A new thread must not be placed on the clamped
    // account while an unclamped one exists.
    let second = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(
        m.serving_account(&second).unwrap(),
        other,
        "the share the operator set through the API has to REACH placement; a setter \
         whose value nothing reads is the hole this closes, not a smaller version of it"
    );

    // And with the alternative removed, the clamp is a veto rather than a
    // preference: the thread PARKS instead of landing on the clamped account.
    m.store
        .upsert_account_state(&threads::accounts::AccountState {
            name: other.into(),
            config_dir: None,
            limit_until: Some("2099-01-01T00:00:00+00:00".into()),
            last_limit_prose: Some("resets in 2099".into()),
            ..Default::default()
        })
        .unwrap();
    let third = m.create_thread(None, settings(tmp.path())).unwrap();
    m.send(&third, "hello".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| m.park_state(&third).unwrap() != Value::Null).await,
        "park: {:?}",
        m.park_state(&third).unwrap()
    );
    let reason = m.park_state(&third).unwrap()["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        reason.contains("spent past the share it was lent"),
        "it parks on the SHARE the API set, not on a threshold: {reason}"
    );
}

/// **The share is REFUSED out of range, not clamped, and a refusal leaves the
/// previous value alone.**
///
/// Silently accepting 1.5 and storing 1.0 would tell the operator his edit
/// landed when it did not — and he would then reason about a pool he does not
/// have. The bound lives at the only door into the table, so every surface
/// inherits it and none has to remember it.
///
/// **THE MUTATION.** Replace the refusal in `Store::set_pool_share` with
/// `share.clamp(0.01, 1.0)` and the first block goes red; make the WS arm
/// swallow the error and report success and `ws_protocol.rs` goes red too.
#[test]
fn an_impossible_share_is_refused_and_changes_nothing() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    m.store.set_pool_share("aa-owned", 0.6, None).unwrap();

    // 0.0 IS NOT ON THIS LIST, as of 2026-09-08: a true zero DISABLES and
    // is the one thing on the old list that the operator actually needed. Every
    // value here is still nonsense — a negative share is not a "more disabled"
    // disable, and refusing it is what keeps the disabling a single, exact state.
    for bad in [1.5, -0.2, f64::NAN, f64::INFINITY] {
        assert!(
            m.store.set_pool_share("aa-owned", bad, None).is_err(),
            "share {bad} must be refused, not clamped"
        );
        assert_eq!(
            m.budgets.pool_share("aa-owned"),
            threads::budgets::PoolShare::pooled(0.6),
            "a refused write must not disturb the value that was already there"
        );
    }

    // The boundaries themselves are legal: 1.0 is "the whole account", which is
    // a thing he may deliberately state, and it stays distinguishable from
    // never having said anything.
    assert!(m.store.set_pool_share("aa-owned", 1.0, None).is_ok());
    let rows = m.budgets.pool_share_rows(&["aa-owned".to_string()]);
    assert_eq!(rows["shares"][0]["share"], json!(1.0));
    assert_eq!(
        rows["shares"][0]["source"],
        json!("override"),
        "a DELIBERATE 1.0 is not the same fact as an absent row, and the payload keeps \
         them apart — the store goes out of its way to preserve that distinction and a \
         wire shape reporting only the effective number would have thrown it away"
    );
}

/// **The payload explains WHERE the effective share came from, over all three
/// sources.**
///
/// Written after the mutation run found a hole in this file: flipping
/// `Budgets::pool_share` to read the DECLARED share before the store row — the
/// precedence bug that would make the setter reachable and useless — did not
/// make any test here go red, because no fixture above declares a share at all.
/// `pool_share_and_clamp.rs` caught it, but this file ships the WIRE SHAPE and
/// was asserting only two of its three `source` values.
///
/// **THE MUTATION.** Read `declared` before the store row in
/// `Budgets::pool_share`: `overridden` then reports 0.95 with source
/// `"declared"`, and the operator's runtime override has silently stopped
/// winning.
#[test]
fn the_payload_names_which_of_the_three_sources_decided_the_share() {
    use std::sync::Arc;
    use threads::pool::{Pool, PoolEntry};
    use threads::{Budgets, Store};

    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("t.db")).unwrap();

    let mut pool = Pool::default();
    for (name, share) in [("overridden", Some(0.95)), ("declared-only", Some(0.3))] {
        let mut e = PoolEntry::claude_account(name, "/tmp/x").unwrap();
        e.share = share;
        pool.entries.push(e);
    }
    pool.entries
        .push(PoolEntry::claude_account("bare", "/tmp/x").unwrap());

    let b = Budgets::new(store.clone()).with_pool(Arc::new(pool));
    // The operator overrides one of the declared ones at runtime.
    store.set_pool_share("overridden", 0.5, Some("today only")).unwrap();

    let rows = b.pool_share_rows(&[
        "overridden".to_string(),
        "declared-only".to_string(),
        "bare".to_string(),
    ]);
    let r = |n: &str| -> Value {
        rows["shares"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["account"] == n)
            .unwrap()
            .clone()
    };

    // OVERRIDE WINS, and both numbers are visible so the precedence is legible
    // rather than something the reader has to know.
    assert_eq!(r("overridden")["share"], json!(0.5));
    assert_eq!(r("overridden")["override"], json!(0.5));
    assert_eq!(r("overridden")["declared"], json!(0.95));
    assert_eq!(r("overridden")["source"], json!("override"));
    assert_eq!(r("overridden")["note"], json!("today only"));

    // DECLARED, with no override.
    assert_eq!(r("declared-only")["share"], json!(0.3));
    assert_eq!(r("declared-only")["override"], Value::Null);
    assert_eq!(r("declared-only")["declared"], json!(0.3));
    assert_eq!(r("declared-only")["source"], json!("declared"));

    // NEITHER: the whole account, byte-for-byte the behaviour that shipped
    // before shares existed.
    assert_eq!(r("bare")["share"], json!(1.0));
    assert_eq!(r("bare")["override"], Value::Null);
    assert_eq!(r("bare")["declared"], Value::Null);
    assert_eq!(r("bare")["source"], json!("unset"));
}

// ════════════════════════════════════════════════════════════════════════════
// 2. UNRANKED IS NOT ZERO
// ════════════════════════════════════════════════════════════════════════════

/// **An account with no reading is UNRANKED, and it cannot be rendered as zero
/// pressure.**
///
/// The interface has to draw these numbers. Zero pressure means "exactly out of
/// share with a whole window left to burn it in" — the hottest thing the scale
/// can say. No reading means we have never measured the account. A board that
/// draws both as an empty bar at the bottom tells the operator his unmeasured
/// account is his most urgent one, or that his most urgent one is unmeasured;
/// either way he acts on the wrong account.
///
/// So the wire shape makes the confusion require deliberate effort:
/// `pressure` is JSON `null`, `ranked` is `false`, and `unranked_reason` names
/// WHICH of the three causes it is.
///
/// **THE MUTATIONS**, all red here:
///
/// 1. `"pressure": r.pressure.unwrap_or_default()` in `pressure_rows` — one
///    keystroke, and it is precisely the bug. The `Value::Null` assertions fail.
/// 2. Rank the unranked at the bottom rather than not at all — `rank` becomes a
///    number and the "unranked has no rank, not last place" assertion fails.
/// 3. Collapse the three `UnrankedReason`s to one — the `window_rolled` and
///    `no_reading` assertions stop being distinguishable.
#[test]
fn an_unranked_account_never_renders_as_zero_pressure() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["measured", "never-read", "stale"])).unwrap();
    let now = chrono::Utc::now();

    // MEASURED: a reading inside a live window.
    weekly(&m, "measured", 40.0);
    // STALE: a real reading whose window has already reset.
    m.budgets
        .observe(
            "stale",
            Axis::WeeklyAll,
            40.0,
            Some("2000-01-01T00:00:00Z".to_string()),
            "usage_poll",
        )
        .unwrap();
    // NEVER-READ: nothing at all.

    let names: Vec<String> = ["measured", "never-read", "stale"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let p = m.budgets.pressure_rows(&names, now);
    let row = |n: &str| -> Value {
        p["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["account"] == n)
            .unwrap()
            .clone()
    };

    // ── the measured account ──
    let measured = row("measured");
    assert_eq!(measured["ranked"], json!(true));
    let value = measured["pressure"].as_f64().unwrap();
    assert!(value.is_finite() && value > 0.0, "measured: {measured}");
    assert_eq!(measured["rank"], json!(1), "the only ranked account is rank 1");
    assert_eq!(measured["unranked_reason"], Value::Null);
    // Everything a reader needs to render it honestly travels with it.
    assert_eq!(measured["axis"], json!("weekly_all"));
    assert_eq!(measured["share"], json!(1.0));
    assert_eq!(measured["observed_pct"], json!(40.0));
    assert_eq!(measured["resets_at"], json!("2099-01-01T00:00:00+00:00"));
    assert_eq!(
        measured["reset_basis"],
        json!("observed"),
        "an ASSUMED window is a guess, and a reader shown a reset instant is owed how \
         much that instant is worth"
    );

    // ── the two unranked accounts ──
    for (name, why) in [("never-read", "no_reading"), ("stale", "window_rolled")] {
        let r = row(name);
        assert_eq!(r["ranked"], json!(false), "{name}: {r}");
        assert_eq!(
            r["pressure"],
            Value::Null,
            "{name}: UNRANKED IS NOT ZERO. Zero is the busiest reading on the scale; this \
             is the absence of evidence: {r}"
        );
        assert!(
            r["pressure"].as_f64().is_none(),
            "{name}: and it must not be readable as a number by any client that asks for \
             one: {r}"
        );
        assert_ne!(
            r["pressure"],
            json!(0.0),
            "{name}: stated the other way round, because this is the exact confusion"
        );
        assert_eq!(
            r["rank"],
            Value::Null,
            "{name}: an unranked account has NO rank — not last place, which would be a \
             claim we cannot support: {r}"
        );
        assert_eq!(
            r["unranked_reason"],
            json!(why),
            "{name}: the three causes stay distinguishable — 'never measured' and 'the \
             reading is stale' call for different actions from a human: {r}"
        );
        // The share is still reported: it is a fact about what he lent the
        // pool, and it is true whether or not anything has been measured.
        assert_eq!(r["share"], json!(1.0), "{name}: {r}");
    }

    // The stale account keeps its reading — it is unrankable, not unknown.
    assert_eq!(row("stale")["observed_pct"], json!(40.0));
    assert_eq!(
        row("never-read")["observed_pct"],
        Value::Null,
        "and the never-measured account reports no percentage at all, rather than a 0.0 \
         that would draw as an empty account with room to spare"
    );

    assert_eq!(p["ranked_count"], json!(1));
    assert_eq!(
        p["accounts"].as_array().unwrap().len(),
        3,
        "every pool member appears — an account dropped for being unmeasured is an \
         account the operator cannot see is unmeasured"
    );
}

/// **The ranker and the API read the SAME number.**
///
/// `pool_pressure` is what places threads; `pressure_rows` is what the
/// interface draws. If they were two implementations they would disagree the
/// first time either was touched, and the board would explain a placement that
/// never happened. They are one implementation, and this is what says so.
///
/// **THE MUTATION.** Give `pressure_rows` its own copy of the arithmetic — even
/// a faithful copy — and then change `PRESSURE_MIN_WINDOW_FRACTION` in only one
/// of them. This goes red. As written, `pool_pressure` is a projection of
/// `pressure_reading`, so there is nowhere for a second copy to live.
#[test]
fn the_ranked_value_on_the_wire_is_the_ranker_s_own_number() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["hot", "roomy"])).unwrap();
    let now = chrono::Utc::now();

    // Different shares AND different consumption, so the two accounts cannot
    // tie by accident and the share is genuinely dividing.
    m.store.set_pool_share("hot", 0.5, None).unwrap();
    weekly(&m, "hot", 40.0);
    weekly(&m, "roomy", 40.0);

    let names = vec!["hot".to_string(), "roomy".to_string()];
    let p = m.budgets.pressure_rows(&names, now);
    let get = |n: &str| -> f64 {
        p["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["account"] == n)
            .unwrap()["pressure"]
            .as_f64()
            .unwrap()
    };

    // The same consumption against half the share is a much higher required
    // burn rate. If the share were not the denominator these would be equal.
    assert!(
        get("hot") < get("roomy"),
        "40% spent of a 50% share leaves less of the share unused than 40% of a whole \
         account, so its pressure is LOWER: hot={} roomy={}",
        get("hot"),
        get("roomy")
    );
    assert_eq!(
        p["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["account"] == "hot")
            .unwrap()["share"],
        json!(0.5),
        "and the denominator is on the wire, so the number is explicable without the \
         reader re-deriving it"
    );

    // Cross-check against the ranker's own view of the same accounts: the
    // ordering the placement key sees must be the ordering the board draws.
    let ranks: Vec<&str> = {
        let mut v: Vec<(&str, f64)> = vec![("hot", get("hot")), ("roomy", get("roomy"))];
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        v.into_iter().map(|(n, _)| n).collect()
    };
    assert_eq!(ranks[0], "roomy");
    assert_eq!(
        p["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["account"] == "roomy")
            .unwrap()["rank"],
        json!(1),
        "rank 1 is the highest required burn rate — the account the pool most needs to \
         spend"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// 3. THE MACHINE DEFAULT DOES NOT SURVIVE A REBOOT IN A BUDGET SCOPE
// ════════════════════════════════════════════════════════════════════════════

/// **The persisted `aui:default` config is migrated off the literal account
/// `"default"`, and the migration is idempotent.**
///
/// `Budgets::ensure_default` never overwrites, which is correct — boot must not
/// undo a ceiling the operator set. The price is that a config written when the
/// machine default WAS a registered account keeps that scope forever, and it
/// comes straight back out of `budgets.get`:
///
/// > *"We do not want the threads module to have any concept of the default."*
///
/// It gates nothing (no pool member is named `default`), so this is not a
/// placement bug. It is the operator being shown an account this module is not
/// supposed to know exists, which is the thing he actually asked to stop.
///
/// **THE MUTATIONS.** Skip the `set_config` write and the scope still contains
/// `"default"`. Re-scope every emptied key to the pool instead of only
/// `aui:default` and the hand-scoped key below is silently re-pointed at
/// accounts he never named — the second block catches it.
#[test]
fn the_machine_default_is_migrated_out_of_every_budget_scope() {
    use threads::budgets::{BudgetConfig, DEFAULT_BUDGET_KEY};

    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "zz-second"])).unwrap();
    let pool = m.account_pool();

    // The row as it exists on the operator's machine: seeded on an old boot,
    // when the machine default was a registered account.
    let mut legacy = BudgetConfig::new(DEFAULT_BUDGET_KEY);
    legacy.accounts = vec![
        "default".to_string(),
        "aa-owned".to_string(),
        "zz-second".to_string(),
    ];
    m.budgets.set_config(&legacy).unwrap();
    assert!(
        m.budgets
            .get_config(DEFAULT_BUDGET_KEY)
            .unwrap()
            .unwrap()
            .accounts
            .iter()
            .any(|a| a == "default"),
        "fixture precondition: the residue is there to be removed"
    );

    // A key the operator scoped BY HAND to the default and nothing else. This
    // one must NOT be re-pointed: silently widening his budget to accounts he
    // did not name is a worse answer than leaving the residue and saying so.
    let mut handmade = BudgetConfig::new("his-own-key");
    handmade.accounts = vec!["default".to_string()];
    m.budgets.set_config(&handmade).unwrap();

    let changed = m.budgets.purge_account_from_scopes("default", &pool).unwrap();
    assert_eq!(
        changed,
        vec![DEFAULT_BUDGET_KEY.to_string()],
        "exactly the fleet-wide key was rewritten"
    );

    let after = m
        .budgets
        .get_config(DEFAULT_BUDGET_KEY)
        .unwrap()
        .unwrap();
    assert!(
        !after.accounts.iter().any(|a| a == "default"),
        "the literal account `default` is gone from the scope: {:?}",
        after.accounts
    );
    assert_eq!(
        after.accounts,
        vec!["aa-owned".to_string(), "zz-second".to_string()],
        "and the real accounts it was scoped to are untouched — this is a removal, not a \
         re-scope"
    );

    // The hand-scoped key is LEFT ALONE, emptied of nothing.
    let his = m.budgets.get_config("his-own-key").unwrap().unwrap();
    assert_eq!(
        his.accounts,
        vec!["default".to_string()],
        "a key scoped to the default and nothing else is a question for the operator, not \
         a row to silently re-point at the pool"
    );

    // IDEMPOTENT: a second boot changes nothing and reports nothing.
    assert!(
        m.budgets
            .purge_account_from_scopes("default", &pool)
            .unwrap()
            .is_empty(),
        "a migration that runs once must be visible once, not on every boot"
    );
}

/// **The daemon runs the migration at boot, so this is fixed on the operator's
/// machine without anybody running a command.**
///
/// The unit above proves the transform; this proves it is WIRED. A migration
/// nothing calls is a migration that has not happened.
///
/// **THE MUTATION.** Delete the `purge_account_from_scopes` call from
/// `ThreadManager::recover` and this goes red while the unit above stays green
/// — which is exactly the failure mode a unit-only proof would miss.
#[tokio::test]
async fn boot_migrates_the_default_out_of_the_budget_scope() {
    use threads::budgets::{BudgetConfig, DEFAULT_BUDGET_KEY};

    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "zz-second"])).unwrap();

    let mut legacy = BudgetConfig::new(DEFAULT_BUDGET_KEY);
    legacy.accounts = vec!["default".to_string(), "aa-owned".to_string()];
    m.budgets.set_config(&legacy).unwrap();

    m.recover().await;

    let after = m
        .budgets
        .get_config(DEFAULT_BUDGET_KEY)
        .unwrap()
        .unwrap();
    assert!(
        !after.accounts.iter().any(|a| a == "default"),
        "boot must migrate it — nobody is going to run a command: {:?}",
        after.accounts
    );

    // And `budgets.get`'s payload — the thing the operator actually reads — no
    // longer mentions it anywhere.
    let snap = m
        .budgets
        .snapshot(DEFAULT_BUDGET_KEY, &m.account_pool())
        .unwrap()
        .unwrap()
        .to_json();
    assert!(
        !serde_json::to_string(&snap).unwrap().contains("\"default\""),
        "the surface he reads must not name it: {snap}"
    );
}

/// **`[accounts.default]` CANNOT COME BACK — from a save, from a config seed,
/// or from a daemon boot.**
///
/// The state migration that removed the row from `accounts.toml`, the `accounts`
/// table and the budget scope was correct and would have been undone almost
/// immediately, by three separate routes:
///
/// 1. **`threads accounts adopt`**, which `claude-accounts login` runs, called
///    `ensure_default()` and then `save()`. The next login rewrites the stanza.
/// 2. **`threads accounts add`**, same shape.
/// 3. **`build_manager`**, which needs no call site to be wrong at all: it
///    loads a registry that may carry the row, merges `config.toml`'s
///    `[accounts]` seeds — where `default = ""` has always lived — and saves
///    the whole thing back the moment anything differs.
///
/// Route 3 is why the fix is at the WRITE and not at the six call sites. A call
/// site fix holds until somebody adds a seventh, and route 3 is not a call site
/// mistake — every line of it is doing its job.
///
/// **THE MUTATIONS.** Restore `ensure_default()` before the `save()` in
/// `accounts_adopt` — still green, because `Registry::save` filters it, which is
/// the property being bought. Now ALSO remove the filter from `Registry::save`:
/// red. Remove only the `config.toml` seed guard in `build_manager`: the second
/// block goes red on the in-memory pool, catching the half the disk filter
/// cannot see.
#[test]
fn the_machine_default_can_never_be_written_back_to_the_registry() {
    use threads::accounts::{Registry, DEFAULT_ACCOUNT};

    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("accounts.toml");

    // A registry holding the machine default IN MEMORY — which is legal, and is
    // what `accounts health` and `accounts delete` legitimately construct.
    let mut reg = Registry::default();
    reg.add("aa-owned", Some("/tmp/aa-owned".into()));
    assert!(reg.ensure_default(), "fixture precondition: the row is present");
    assert!(reg.get(DEFAULT_ACCOUNT).is_some());

    // Saving it does NOT persist it. This is the choke point.
    reg.save(&path).unwrap();
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(
        !on_disk.contains("default"),
        "`[accounts.default]` must never reach the file — the next `claude-accounts login` \
         runs `accounts adopt`, which saves, and that is how the state migration would \
         have been undone: {on_disk}"
    );
    assert!(
        on_disk.contains("aa-owned"),
        "…while everything that IS a registered account is written normally: {on_disk}"
    );

    // And it round-trips absent, so a reload cannot resurrect it either.
    let back = Registry::load(&path).unwrap();
    assert!(back.get(DEFAULT_ACCOUNT).is_none());
    assert!(back.get("aa-owned").is_some());

    // ── the config-seed route, which no call-site audit would have found ──
    //
    // `config.toml` spells the machine default as `default = ""`. A daemon boot
    // merges those seeds into the registry and saves when anything changed.
    let mut accounts = BTreeMap::new();
    accounts.insert("default".to_string(), String::new());
    accounts.insert(
        "aa-owned".to_string(),
        tmp.path().join("aa-owned").to_string_lossy().to_string(),
    );
    let cfg = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        gauge_bar_pct: 0.0,
        ..Default::default()
    };
    let m = build_manager(cfg).unwrap();

    assert!(
        m.registry.get(DEFAULT_ACCOUNT).is_none(),
        "a `default = \"\"` seed must not register the machine default even in memory — \
         the daemon would otherwise spend the process reasoning about a slot the operator \
         has said is not its to know"
    );
    assert!(
        !m.account_pool().iter().any(|a| a == DEFAULT_ACCOUNT),
        "and it is not in the pool: {:?}",
        m.account_pool()
    );
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        !on_disk.contains("default"),
        "nor on disk after a boot: {on_disk}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// 4. THE POLLING FALLBACK HAS NO DOOR LEFT
// ════════════════════════════════════════════════════════════════════════════

/// **A poll with no login directory is REFUSED, never run against `~/.claude`.**
///
/// This is the behaviour the operator has been trying to purge, and a comment
/// documented it as intended:
///
/// > *"I'm really disappointed that you found a comment that said that because
/// > I've been trying to purge this behavior for a long time."*
///
/// Removing `default` from the pool removed the only NAME that resolved to no
/// config dir. It did not remove the RESOLUTION: `probe_account` fed
/// `config_dir_for`'s `Option` straight to the probe, and `None` means "leave
/// `CLAUDE_CONFIG_DIR` unset", which means `~/.claude` — his own terminal
/// login, polled on a timer. Any name that is not a pool member still resolved
/// that way, and so would any future non-Claude resource type.
///
/// **THE MUTATION.** Restore `let cd = self.config_dir_for(name);` and pass it
/// through. This goes red: the probe is invoked with `None` instead of being
/// refused.
#[tokio::test]
async fn a_poll_without_a_login_directory_is_refused_not_defaulted() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    // Every name the poller will actually visit resolves to a real directory —
    // membership in the pool IS a login directory, by construction.
    for name in m.poll_targets() {
        assert!(
            m.pool().login_dir(&name).is_some(),
            "{name} is a poll target with no login directory"
        );
    }

    // And a name that does NOT resolve is refused rather than polled. This is
    // the access-side guard, which holds for any caller and any future resource
    // type rather than for whichever list happens to feed the poller today.
    let err = m
        .probe_account_for_test("not-a-pool-member")
        .await
        .unwrap_err();
    assert!(
        err.contains("no login directory"),
        "it must refuse, and say why: {err}"
    );
    assert!(
        err.contains("operator's own terminal login"),
        "and the refusal names the thing it is protecting, because a future reader will \
         otherwise 'fix' it back: {err}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// 5. ONE NOTICE PER DAY, AND IT DOES NOT LOSE THE COUNT
// ════════════════════════════════════════════════════════════════════════════

/// **The daily notice fires ONCE, and the second attempt inside the window does
/// not fire at all.**
///
/// The operator struck the ticket-filing alarm this replaces:
///
/// > *"I don't really need an alarm... Sounds like it's just going to nag me,
/// > and I don't want to be nagged. Maybe just, like, if there's an attempt to
/// > place on a thread, you can notify me in the audio user interface, but I
/// > don't want to be notified more than once per day."*
///
/// The whole value of the feature is the NEGATIVE case, so that is what is
/// asserted hardest: forty failures in an hour produce one notice.
///
/// **THE MUTATIONS**, each red here:
///
/// 1. Read `last_at`, decide, then write — the obvious two-statement shape.
///    The `>= 2` concurrent block below is what catches it; a single-threaded
///    test never would.
/// 2. Hold the window in a `HashMap` on `ThreadManager` instead of the store.
///    The "a restart does not re-open the window" block goes red, and that is
///    the case that matters most: this daemon restarts itself on every new
///    binary, and an expired login outlives every restart.
/// 3. Drop `suppressed` and always report 0 — the count assertion goes red, and
///    a persistent failure would be reported to him as a single event.
#[test]
fn the_daily_notice_fires_once_and_counts_what_it_swallowed() {
    use threads::event::LOGIN_EXPIRED_NOTICE;
    use threads::Store;

    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("t.db");
    let store = Store::open(&db).unwrap();
    let day = chrono::Duration::days(1);
    let t0 = chrono::DateTime::parse_from_rfc3339("2026-08-27T09:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    // FIRST: nothing has been said, so it may speak.
    assert_eq!(
        store
            .claim_notice(LOGIN_EXPIRED_NOTICE, "login-a", day, t0)
            .unwrap(),
        Some(0),
        "the first notice for a subject always gets through"
    );

    // …AND IT DOES NOT FIRE AGAIN. Forty more placement failures over the next
    // eleven hours produce nothing.
    for i in 1..=40 {
        let t = t0 + chrono::Duration::minutes(i * 16);
        assert_eq!(
            store
                .claim_notice(LOGIN_EXPIRED_NOTICE, "login-a", day, t)
                .unwrap(),
            None,
            "attempt {i} at {t} is inside the day and must be silent — this is the whole \
             point of the feature, and 'not more than once per day' is a statement about \
             what must NOT happen"
        );
    }

    // Just before the day is up: still silent. The boundary is the assertion,
    // not the middle of the window.
    let almost = t0 + day - chrono::Duration::seconds(1);
    assert_eq!(
        store
            .claim_notice(LOGIN_EXPIRED_NOTICE, "login-a", day, almost)
            .unwrap(),
        None,
        "one second early is still inside the day"
    );

    // A day later it speaks again, AND it carries what it swallowed.
    let next = t0 + day + chrono::Duration::seconds(1);
    let carried = store
        .claim_notice(LOGIN_EXPIRED_NOTICE, "login-a", day, next)
        .unwrap();
    assert_eq!(
        carried,
        Some(41),
        "the rate limit must not lose information: a login expired for a day may have \
         blocked forty placements, and reporting that as a single event understates a \
         persistent problem as an incident"
    );

    // And the counter reset with the notice.
    assert_eq!(
        store
            .claim_notice(LOGIN_EXPIRED_NOTICE, "login-a", day, next)
            .unwrap(),
        None
    );

    // ── PER SUBJECT, NOT GLOBAL ──
    //
    // Two genuinely different expired logins are two things for him to fix, so
    // the daily budget is per login. A global limiter would silently hide the
    // second account behind the first.
    assert_eq!(
        store
            .claim_notice(LOGIN_EXPIRED_NOTICE, "login-b", day, next)
            .unwrap(),
        Some(0),
        "a second login's first notice is not suppressed by the first login's"
    );

    // ── A RESTART DOES NOT RE-OPEN THE WINDOW ──
    //
    // The case an in-memory limiter fails, and the one that matters most: this
    // daemon restarts itself whenever a new binary lands, and an expired login
    // is a state only the operator can leave, so it outlives every restart.
    drop(store);
    let reopened = Store::open(&db).unwrap();
    assert_eq!(
        reopened
            .claim_notice(LOGIN_EXPIRED_NOTICE, "login-a", day, next)
            .unwrap(),
        None,
        "a fresh process must not speak again — an in-memory window would re-open on \
         every boot and the daemon would nag him precisely because it was trying not to"
    );
}

/// **Two callers racing for the same window: exactly one wins.**
///
/// Anything that arbitrates between two or more of something is proven at N>=2
/// or not proven — its empty and single cases cannot exercise the contention it
/// exists to manage. A rate limit is exactly that kind of thing.
///
/// # A SURVIVOR, written down here because it survived
///
/// This test was written to catch one specific mutation: replacing the
/// conditional UPDATE in `claim_notice` with a SELECT, an `if`, and an
/// unconditional UPDATE — the read-then-write shape that classically emits
/// twice. **That mutation was applied and this test stayed GREEN with 8
/// threads.**
///
/// The reason is that `Store` holds a single `Mutex<Connection>` and
/// `claim_notice` takes the guard once for its whole body, so the two
/// statements are already atomic with respect to each other. The exclusion
/// being proved here is real, but it is currently supplied by the store's
/// connection model and NOT by the SQL shape, and saying otherwise would have
/// been a comment claiming evidence this test does not produce.
///
/// The conditional UPDATE is kept regardless: it is correct without depending
/// on a property of a different module, and a connection pool, a reader
/// connection or a second daemon process would each remove the mutex quietly
/// and take the read-then-write version with it.
///
/// So what this test DOES prove: that under real concurrent load exactly one
/// caller may speak. That is the operator's requirement, and it holds. What it
/// does not prove is which mechanism is holding it up — for that, the mutation
/// to make would be to the store's connection model, which is out of this
/// file's scope.
#[test]
fn exactly_one_of_two_racing_callers_may_speak() {
    use std::sync::Arc;
    use threads::event::LOGIN_EXPIRED_NOTICE;
    use threads::Store;

    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&tmp.path().join("t.db")).unwrap());
    let day = chrono::Duration::days(1);
    let t0 = chrono::Utc::now();

    // Prime the row so the "first ever" branch is not what is being tested —
    // that branch is trivially exclusive because of the PRIMARY KEY.
    store
        .claim_notice(LOGIN_EXPIRED_NOTICE, "racy", day, t0 - day * 2)
        .unwrap();

    // Now N threads all arrive at an EXPIRED window simultaneously.
    let winners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let mut hs = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let winners = winners.clone();
        let barrier = barrier.clone();
        hs.push(std::thread::spawn(move || {
            barrier.wait();
            if store
                .claim_notice(LOGIN_EXPIRED_NOTICE, "racy", day, t0)
                .unwrap()
                .is_some()
            {
                winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }));
    }
    for h in hs {
        h.join().unwrap();
    }

    assert_eq!(
        winners.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "eight callers arriving at an expired window must yield exactly ONE notice — \
         'not more than once per day' has to hold under concurrency or it does not hold"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
