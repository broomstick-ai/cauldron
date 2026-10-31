//! **THE POOL SHARE KEYS ON THE ACCOUNT NAME, AND THE SHARE BOUNDARY IS BOTH A
//! PRESSURE TERM AND A VETO.**
//!
//! Two rulings from 2026-08-27, both corrections to what shipped the day before.
//!
//! # 1. The key is the NAME, not the login
//!
//! > *"I think it's actually better to key on the account name instead of on the
//! > login slot, because the login slot is subject to change."*
//!
//! Yesterday's `account_pool_shares` keyed on a login fingerprint, and reported
//! as a vindication that the operator's `default` label and his Gmail label
//! resolve to one login. He rejected the framing: the collision was not evidence
//! for identity-keying, it was evidence that `default` should never have been
//! visible to the pool. And a login slot MOVES — `add-account --force` re-points
//! a label at a different login as a routine operation — so a share keyed on the
//! fingerprint is silently lost or reassigned by a re-login.
//!
//! # 2. The share is a VETO as well as a denominator
//!
//! > *"if all the other accounts are blocked, we don't want that account to even
//! > be available for placement, right? So overage we've determined is okay
//! > while the current workload is finishing. But then it needs to be like hard
//! > clamped — not available for placement as well. So it needs to have an
//! > effect on pressure, but it also needs to have an effect on whether
//! > placement is possible."*
//!
//! Negative pressure past the share is *necessary but not sufficient*: a
//! discouragement loses to an empty field, and "all the other accounts are
//! blocked" is exactly the case where a ranking term has nothing left to lose
//! to. So the boundary gets a second effect.
//!
//! **The asymmetry is the ruling, and it is not "kill the account".** Work
//! already running on an over-share account finishes. What stops is NEW
//! placement onto it.
//!
//! This is NOT the per-account veto the operator struck (*"I don't know why
//! you're going to need a per account veto. We don't need that."*). That was
//! about THRESHOLDS, and no per-account threshold has been reintroduced —
//! nothing in this file reads one.
//!
//! # Adversarial
//!
//! Every test here names the MUTATION that makes it red, because a green nobody
//! has forced to fail is not evidence. Survivors are written down in the test
//! that failed to catch them.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;
use threads::budgets::PoolShare;
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
        std::env::temp_dir().join("threads-share-tests-home"),
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
        // The gauge's own bar OFF, so nothing in this file can pass because an
        // account was barred at 100% for an unrelated reason. The clamp has to
        // be the thing doing the work.
        gauge_bar_pct: 0.0,
        ..Default::default()
    }
}

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

/// Record a weekly-all reading — the axis the share divides and the clamp reads.
fn weekly(m: &ThreadManager, account: &str, pct: f64) {
    m.budgets
        .observe(
            account,
            Axis::WeeklyAll,
            pct,
            // A live window, so `pool_pressure` has a usable denominator and the
            // reading is not discarded as stale.
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
// 1. THE KEY IS THE NAME
// ════════════════════════════════════════════════════════════════════════════

/// **The share follows the NAME across a login change.**
///
/// The simulation is the real operation: `add-account --force` logs a label in
/// again, so its identity fingerprint changes while the label does not. Under
/// the old key the share was attached to the OLD fingerprint and the account
/// silently reverted to 1.0 — the whole account offered to the pool, which is
/// the opposite of what the operator set.
///
/// **THE MUTATION.** Re-introduce the fingerprint resolver — key
/// `Budgets::pool_share` on `login_identity(account)` instead of on `account` —
/// and the second assertion goes red, reporting 1.0.
#[tokio::test]
async fn the_share_keys_on_the_name_and_survives_a_login_change() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    // The label is signed into one login…
    m.store
        .set_account_identity("aa-owned", Some("/tmp/aa-owned"), Some("fp-before.1111"))
        .unwrap();
    m.store
        .set_pool_share("aa-owned", 0.6, Some("he keeps 40% to spend by hand"))
        .unwrap();
    assert_eq!(m.budgets.pool_share("aa-owned"), PoolShare::pooled(0.6));

    // …and then he re-logs it in. Same label, different credential, different
    // fingerprint. `add-account --force` does exactly this.
    m.store
        .set_account_identity("aa-owned", Some("/tmp/aa-owned"), Some("fp-after.2222"))
        .unwrap();

    assert_eq!(
        m.budgets.pool_share("aa-owned"),
        PoolShare::pooled(0.6),
        "the share is a statement about what the operator lends the pool, not about a \
         credential — 'the login slot is subject to change'"
    );

    // And it is stored under the name, readably.
    let rows = m.store.list_pool_shares().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "aa-owned");
    assert_eq!(rows[0].1, 0.6);
}

/// **Two accounts that share ONE login get their own shares.**
///
/// The case the old key could not express at all: under a fingerprint key these
/// two rows are one row, so setting one silently overwrites the other. This is
/// the live shape on the operator's machine — labels that collapse to one login
/// — and it is the reason the collision he was shown was not a vindication.
///
/// **THE MUTATION.** Key on the fingerprint and this goes red: both accounts
/// report 0.25, because the second `set` overwrote the first.
#[tokio::test]
async fn two_labels_of_one_login_hold_two_independent_shares() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "zz-alias"])).unwrap();

    for n in ["aa-owned", "zz-alias"] {
        m.store
            .set_account_identity(n, Some("/tmp/x"), Some("one.login"))
            .unwrap();
    }
    m.store.set_pool_share("aa-owned", 0.9, None).unwrap();
    m.store.set_pool_share("zz-alias", 0.25, None).unwrap();

    assert_eq!(m.budgets.pool_share("aa-owned"), PoolShare::pooled(0.9));
    assert_eq!(m.budgets.pool_share("zz-alias"), PoolShare::pooled(0.25));
}

/// **Absent configuration is 1.0, and the store OVERRIDES the blob.**
///
/// The precedence is stated in exactly one place (`Budgets::pool_share`) and
/// asserted in exactly one place, here. Absent both is the whole account, which
/// is byte-for-byte the behaviour that shipped before shares existed — the
/// standing "no backfill, no shim" requirement.
#[test]
fn absent_is_one_and_the_store_row_wins_over_the_declared_share() {
    use std::sync::Arc;
    use threads::pool::{Pool, PoolEntry};
    use threads::{Budgets, Store};

    let store = Store::open_in_memory().unwrap();

    // No pool, no rows: 1.0.
    let bare = Budgets::new(store.clone());
    assert_eq!(
        bare.pool_share("anything"),
        PoolShare::pooled(1.0),
        "absent means the whole account"
    );

    // A DECLARED share in the blob.
    let mut declared = PoolEntry::claude_account("aa-owned", "/tmp/aa-owned").unwrap();
    declared.share = Some(0.5);
    let pool = Arc::new(Pool {
        entries: vec![declared],
    });
    let b = Budgets::new(store.clone()).with_pool(pool);
    assert_eq!(b.pool_share("aa-owned"), PoolShare::pooled(0.5), "the blob declares it");
    assert_eq!(b.pool_share("not-in-the-pool"), PoolShare::pooled(1.0));

    // …overridden at runtime by the operator.
    store.set_pool_share("aa-owned", 0.8, Some("lending more today")).unwrap();
    assert_eq!(
        b.pool_share("aa-owned"),
        PoolShare::pooled(0.8),
        "the store row is the operator's override and wins over the declaration"
    );
}

/// **The bound is refused, not clamped.** Silently storing 1.0 for a
/// hand-written 1.5 would tell him his edit landed when it did not.
///
/// **0.0 MOVED OUT OF THIS LIST ON 2026-09-08 AND BACK INTO IT ON 2026-09-20.**
/// Zero was refused originally because it would make the pressure denominator
/// zero; it was then admitted as the DISABLING; the operator has now separated
/// the two questions — *"I want disabled to be independent of share"* — so the
/// state lives in `account_disabled` and this column is a pure quantity again.
/// The bound is `(0.0, 1.0]`, and the refusal NAMES `accounts.disable` so that
/// anyone still reaching for the 2026-09-08 spelling is told where the act went.
#[test]
fn a_share_outside_the_unit_interval_is_refused_at_the_door() {
    let store = threads::Store::open_in_memory().unwrap();
    for bad in [-0.1, -0.0001, 1.5, f64::NAN, f64::INFINITY] {
        assert!(
            store.set_pool_share("aa-owned", bad, None).is_err(),
            "share {bad} must be refused, not clamped"
        );
    }
    assert!(store.set_pool_share("aa-owned", 1.0, None).is_ok());
    assert!(store.set_pool_share("aa-owned", 0.01, None).is_ok());
    let err = store
        .set_pool_share("aa-owned", 0.0, None)
        .expect_err("0 is not a quantity and is no longer a state either");
    assert!(
        format!("{err}").contains("accounts/aa-owned/disable"),
        "the refusal has to say where the act went: {err}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// 2. THE CONFIG BLOB
// ════════════════════════════════════════════════════════════════════════════

/// **A pool is a flat list of typed resource entries, round-tripping through the
/// JSON blob the operator described.**
///
/// *"For each pool of resources that you can draw from, you have a JSON blob
/// indicating… what type it is. This is a cloud account. What percentage of the
/// pool are we giving it access to? … You just have them all flat beside each
/// other."*
#[test]
fn the_pool_is_a_flat_list_of_typed_entries_in_one_blob() {
    use threads::pool::Pool;

    let blob = r#"{
      "entries": [
        {
          "name": "alice-example-com",
          "type": "claude_account",
          "login_dir": "/Users/testuser/code/substrate-interface-layer/lib/threads/accounts/alice-example-com",
          "share": 0.95,
          "note": "the other 5% is mine to spend by hand"
        },
        {
          "name": "owner-gmail",
          "type": "claude_account",
          "login_dir": "/Users/testuser/code/substrate-interface-layer/lib/threads/accounts/owner-gmail"
        }
      ]
    }"#;

    let pool = Pool::from_json(blob).unwrap();
    assert_eq!(
        pool.names(),
        vec![
            "alice-example-com".to_string(),
            "owner-gmail".to_string()
        ]
    );
    assert_eq!(pool.declared_share("alice-example-com"), Some(0.95));
    assert_eq!(
        pool.declared_share("owner-gmail"),
        None,
        "an absent share reads as 1.0 downstream, and stays distinguishable from a \
         deliberate 1.0 here"
    );
    // The login location is an explicit PROPERTY of the entry, not a convention
    // recovered from somewhere else.
    assert_eq!(
        pool.login_dir("alice-example-com"),
        Some("/Users/testuser/code/substrate-interface-layer/lib/threads/accounts/alice-example-com")
    );
    assert_eq!(
        pool.get("alice-example-com").unwrap().resource.type_name(),
        "claude_account"
    );

    // It round-trips: what the daemon renders, it can read back.
    let again = Pool::from_json(&pool.to_json().unwrap()).unwrap();
    assert_eq!(again, pool);
}

/// **A blob that cannot mean what it says is refused, loudly.**
#[test]
fn a_duplicate_name_or_an_impossible_share_is_refused() {
    use threads::pool::Pool;

    let dup = r#"{"entries":[
        {"name":"a","type":"claude_account","login_dir":"/x"},
        {"name":"a","type":"claude_account","login_dir":"/y"}]}"#;
    let e = Pool::from_json(dup).unwrap_err().to_string();
    assert!(e.contains("twice"), "the name is the KEY: {e}");

    let bad_share = r#"{"entries":[
        {"name":"a","type":"claude_account","login_dir":"/x","share":1.5}]}"#;
    assert!(Pool::from_json(bad_share).is_err());

    // An unknown TYPE is refused rather than silently treated as a Claude
    // account — which is what makes adding the second variant safe.
    let unknown = r#"{"entries":[{"name":"a","type":"open_router","credit_key":"sk-x"}]}"#;
    assert!(
        Pool::from_json(unknown).is_err(),
        "an unimplemented resource type must not be mistaken for an implemented one"
    );
}

/// **An absent `pool.json` is today's behaviour**, derived from the registry —
/// the standing "no backfill required" constraint.
#[test]
fn an_absent_config_derives_the_pool_from_the_registry() {
    use threads::accounts::Registry;
    use threads::pool::Pool;

    let tmp = tempfile::tempdir().unwrap();
    let mut reg = Registry::default();
    reg.add("aa-owned", Some("/tmp/aa-owned".into()));
    reg.add("default", None);

    let pool = Pool::load(&tmp.path().join("pool.json"), &reg).unwrap();
    assert_eq!(pool.names(), vec!["aa-owned".to_string()]);
    assert_eq!(pool.declared_share("aa-owned"), None, "which reads as 1.0");

    // And a file that exists but does NOT parse is an error, never a silent
    // fallback to the derivation: running on a different pool than the one he
    // wrote is how a config becomes untrustworthy.
    let path = tmp.path().join("pool.json");
    std::fs::write(&path, "{ not json").unwrap();
    assert!(Pool::load(&path, &reg).is_err());
}

// ════════════════════════════════════════════════════════════════════════════
// 3. THE HARD CLAMP
// ════════════════════════════════════════════════════════════════════════════

/// **An over-share account is refused NEW placement even when it is the only
/// candidate left.**
///
/// This is the exact case the operator named — *"if all the other accounts are
/// blocked, we don't want that account to even be available for placement"* —
/// and it is the case that negative pressure cannot cover, because a ranking
/// term with nothing to lose to still wins.
///
/// The thread here is UNGATED: no budget key, no thresholds. That is every AUI
/// thread, and it is the configuration in which the clamp is easiest to get
/// wrong, because `admit_or_park`'s escape hatch runs a parked ungated thread
/// anyway.
///
/// **THE MUTATIONS.** Three, all of which make this red:
///   1. delete the `self.over_share(...)` filter from `admit`'s `targets` loop;
///   2. delete the `!clamped.is_empty()` park in the fresh-thread branch (the
///      thread then admits in place, on the clamped account);
///   3. drop `&& cause != ParkCause::Share` from the ungated escape hatch in
///      `admit_or_park` — the clamp is then downgraded to a warning for every
///      AUI thread, i.e. it becomes the pressure term it was ruled insufficient
///      to replace.
#[tokio::test]
async fn an_over_share_account_refuses_new_placement_as_the_last_candidate() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "zz-second"])).unwrap();
    m.start_watchers();

    // He lends 60% of `aa-owned`, and it has spent 75% — past the boundary.
    // `zz-second` is lent everything and has spent nothing.
    m.store.set_pool_share("aa-owned", 0.6, None).unwrap();
    weekly(&m, "aa-owned", 75.0);
    weekly(&m, "zz-second", 1.0);
    session(&m, "aa-owned", 1.0);
    session(&m, "zz-second", 1.0);

    // With a free account beside it, a new thread simply goes to the free one.
    let a = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(
        m.serving_account(&a).unwrap(),
        "zz-second",
        "the clamped account is not a target while anything else is"
    );

    // NOW REMOVE THE ALTERNATIVE. `zz-second` takes a limit window, so the
    // clamped account is the only candidate left — the operator's exact case.
    m.store
        .upsert_account_state(&threads::accounts::AccountState {
            name: "zz-second".into(),
            config_dir: None,
            limit_until: Some("2099-01-01T00:00:00+00:00".into()),
            last_limit_prose: Some("resets in 2099".into()),
            ..Default::default()
        })
        .unwrap();

    let b = m.create_thread(None, settings(tmp.path())).unwrap();
    m.send(&b, "hello".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();

    assert!(
        eventually(|| m.park_state(&b).unwrap() != serde_json::Value::Null).await,
        "with every unclamped account blocked the thread PARKS — the share is a veto, \
         not only a discouragement. park: {:?}",
        m.park_state(&b).unwrap()
    );
    let park = m.park_state(&b).unwrap();
    let reason = park["reason"].as_str().unwrap();
    assert!(
        reason.contains("spent past the share it was lent"),
        "it parks on the SHARE, not on a threshold — a threshold park sends him to raise \
         a ceiling, which is not what he would need to do here: {park}"
    );
    assert!(
        reason.contains("aa-owned"),
        "the park must name the clamped account: {park}"
    );
    assert!(
        !reason.contains("over a threshold"),
        "and it must not be reported as a threshold park: {park}"
    );
}

/// **Work already running on an over-share account is NOT disturbed.**
///
/// The other half of the ruling, and the half that makes it not "kill the
/// account": *"overage we've determined is okay while the current workload is
/// finishing."*
///
/// Mechanically the clamp removes the account from the TARGET set and leaves it
/// in the CLEARED set, which is the same shape a dead login already had. So a
/// thread with a conversation there is still `on_admissible` and the gate's
/// DEC-26 branch returns "stay".
///
/// **THE MUTATION.** Apply the clamp to `clear` instead of to `targets` — the
/// one-line "simplification" that turns a placement veto into an eviction — and
/// this goes red: the thread is moved off an account it has history on, and its
/// conversation restarts.
#[tokio::test]
async fn work_already_running_on_an_over_share_account_is_not_disturbed() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "zz-second"])).unwrap();
    m.start_watchers();

    // Both accounts are free and well inside their shares to begin with.
    weekly(&m, "aa-owned", 1.0);
    weekly(&m, "zz-second", 1.0);
    session(&m, "aa-owned", 1.0);
    session(&m, "zz-second", 1.0);

    // Pin a thread onto `aa-owned` and give it a real conversation there.
    let mut s = settings(tmp.path());
    s.account = threads::settings::AccountChoice::Named("aa-owned".into());
    let id = m.create_thread(None, s).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "aa-owned");
    m.send(&id, "first".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| m
            .store
            .thread_accounts(&id)
            .unwrap()
            .contains(&"aa-owned".to_string()))
        .await,
        "fixture: the thread must hold a conversation on the account before it is clamped"
    );

    // NOW CLAMP IT: he lends 60%, it has spent 75%.
    m.store.set_pool_share("aa-owned", 0.6, None).unwrap();
    weekly(&m, "aa-owned", 75.0);

    // The account is genuinely over its share…
    assert!(
        m.budgets.pool_share("aa-owned").lent().unwrap() < 0.75,
        "fixture: the account must actually be past its share"
    );

    // …and the thread carries on there. Another turn runs, in place.
    m.send(&id, "second".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| m.store.get_thread(&id).unwrap().unwrap().account
            == Some("aa-owned".to_string())
            && m.park_state(&id).unwrap() == serde_json::Value::Null)
        .await,
        "a thread already running on an over-share account finishes there — the clamp \
         stops NEW placement, it does not evict. account={:?} park={:?}",
        m.serving_account(&id).unwrap(),
        m.park_state(&id).unwrap()
    );

    // And the conversation was never ported away, which is what an eviction
    // would have shown up as.
    assert_eq!(
        m.store.thread_accounts(&id).unwrap(),
        vec!["aa-owned".to_string()],
        "no second account was ever established for this thread"
    );
}

/// **A fresh thread is placed AWAY from an over-share account while anything
/// else is free — and the pressure term alone would not have done it.**
///
/// The distinction being pinned: at 75% spent of a 60% share the account reads
/// NEGATIVE pressure, which sorts it last. That is the *discouragement* the
/// operator called necessary but not sufficient. This test asserts the clamp is
/// what removes it, by making the alternative one that pressure would rank
/// WORSE: `zz-second` is at 90% of a full share, so it is the account an honest
/// ranking prefers to leave alone.
///
/// **THE MUTATION.** Delete the `over_share` filter and this goes red —
/// placement takes `aa-owned`, because 90% spent on a full share is a smaller
/// required burn rate than the clamped account's.
#[tokio::test]
async fn the_clamp_and_not_the_ranking_is_what_moves_the_thread() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned", "zz-second"])).unwrap();

    m.store.set_pool_share("aa-owned", 0.6, None).unwrap();
    weekly(&m, "aa-owned", 75.0); // past a 60% share
    weekly(&m, "zz-second", 90.0); // deep into a full share, but inside it
    session(&m, "aa-owned", 1.0);
    session(&m, "zz-second", 1.0);

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "zz-second",
        "the clamped account is not a candidate at all, whatever the ranking thinks of \
         the alternative"
    );
}

/// **Unknown is not over.** A gauge that has never reported must not bar an
/// account — that direction is CON-5, and it is also the only direction in which
/// a never-polled account does not lock the whole fleet out on boot.
///
/// **THE MUTATION.** Make `over_share`'s `None` arm return `true` and this goes
/// red immediately; so does most of the rest of the suite, which is the point.
#[tokio::test]
async fn an_unmeasured_account_is_never_clamped() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    // A tiny share and NO weekly reading at all.
    m.store.set_pool_share("aa-owned", 0.01, None).unwrap();

    // NOT `serving_account`: `auto_place` falls back to pool order when
    // placement returns nothing, so a clamped-everything pool still SHOWS the
    // account on the thread. That fallback masked this mutation on the first
    // pass and the assertion below is what actually catches it — the turn has
    // to RUN.
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    m.start_watchers();
    m.send(&id, "hello".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(
        eventually(|| {
            let p = m.park_state(&id).unwrap();
            p == serde_json::Value::Null
                && m.store.get_thread(&id).unwrap().is_some()
        })
        .await,
        "absence of evidence is not evidence of overage — an unmeasured account must \
         still take work. park: {:?}",
        m.park_state(&id).unwrap()
    );
    assert!(
        !m.park_state(&id)
            .unwrap()
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .contains("share"),
        "and never on the strength of a share it has no reading for"
    );
}

/// **THE UNGATED THREAD — the clamp is not downgraded by the escape hatch.**
///
/// `admit_or_park` runs a parked thread anyway when it is UNGATED (no budget
/// key, no enforceable threshold), because *"a placement pass may not invent a
/// refusal for a thread that carries nothing to refuse"*. The share is not an
/// invented refusal: the operator set it, per account, precisely so the account
/// stops taking work.
///
/// Getting to this branch takes deliberate setup, and that is worth recording.
/// A clamped account needs a weekly READING, and a weekly reading plus the
/// fleet-wide default weekly threshold makes a thread GATED — so on the ordinary
/// AUI path the Share park never reaches the hatch at all. This thread clears
/// its thresholds explicitly, which is the only way in.
///
/// **THE MUTATION.** Drop `&& cause != ParkCause::Share` from the hatch in
/// `admit_or_park`. Without this test that mutation SURVIVES — it did, on the
/// first mutation pass, because every other clamp test is gated by construction.
#[tokio::test]
async fn the_clamp_is_not_downgraded_for_an_ungated_thread() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    m.start_watchers();

    m.store.set_pool_share("aa-owned", 0.5, None).unwrap();
    weekly(&m, "aa-owned", 80.0);

    // No budget key and NO thresholds at all — the ungated shape.
    let mut s = settings(tmp.path());
    s.thresholds = threads::Thresholds::ungated();
    let id = m.create_thread(None, s).unwrap();
    m.send(&id, "hello".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();

    assert!(
        eventually(|| m.park_state(&id).unwrap() != serde_json::Value::Null).await,
        "an ungated thread is not exempt from the share the operator lent — the escape \
         hatch must not swallow this park, or the clamp is the pressure term it was \
         ruled insufficient to replace"
    );
    assert!(
        m.park_state(&id).unwrap()["reason"]
            .as_str()
            .unwrap()
            .contains("spent past the share it was lent"),
        "park: {:?}",
        m.park_state(&id).unwrap()
    );
}

/// **With no share configured, nothing is clamped** — the standing constraint
/// that absent configuration means today's behaviour.
///
/// At the default share of 1.0 the boundary sits at a weekly reading of 100%,
/// which is an exhausted account and which `config.gauge_bar_pct` already bars
/// by default. This test runs with that bar switched OFF, so it isolates the
/// clamp: at 99% of an unconfigured account, placement is unchanged.
#[tokio::test]
async fn an_unconfigured_pool_clamps_nothing() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();

    weekly(&m, "aa-owned", 99.0);
    session(&m, "aa-owned", 99.0);

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "aa-owned",
        "no share row and no declared share means 1.0, and 99% is inside 100%"
    );
    assert!(m.store.list_pool_shares().unwrap().is_empty(), "no backfill");
}

/// **The clamp is not a per-account THRESHOLD veto, and the distinction is
/// checkable.**
///
/// The operator struck a per-account threshold: *"I don't know why you're going
/// to need a per account veto. We don't need that. We just need a per account
/// capacity, like percent offered to the pool."* The clamp is the CAPACITY he
/// asked for, given a second effect — so it must read the share and nothing
/// else. This asserts a clamped account with NO thresholds anywhere still
/// clamps, and that the park it produces is not spelled `threshold`.
#[tokio::test]
async fn the_clamp_reads_the_share_and_never_a_threshold() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &["aa-owned"])).unwrap();
    m.start_watchers();

    m.store.set_pool_share("aa-owned", 0.5, None).unwrap();
    weekly(&m, "aa-owned", 60.0);

    // No budget key, no thresholds, nothing that could be called a ceiling.
    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    m.send(&id, "hello".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();

    assert!(
        eventually(|| m.park_state(&id).unwrap() != serde_json::Value::Null).await,
        "the share alone refuses the placement"
    );
    let park = m.park_state(&id).unwrap();
    assert!(
        park["reason"]
            .as_str()
            .unwrap()
            .contains("spent past the share it was lent"),
        "with no threshold anywhere on the machine, the share alone produced the \
         refusal: {park}"
    );

    // The source guard: nothing in the clamp consults a threshold.
    let src = include_str!("../../src/budgets.rs");
    let start = src.find("    fn over_share(").expect("over_share was renamed");
    let body = &src[start..start + src[start..].find("\n    }").unwrap()];
    for banned in ["threshold", "Thresholds"] {
        assert!(
            !body.contains(banned),
            "the clamp must read the SHARE and nothing else; found {banned:?} in \
             `over_share`. The operator struck the per-account threshold veto and it \
             must not come back through this door."
        );
    }
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
