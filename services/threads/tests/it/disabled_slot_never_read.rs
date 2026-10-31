//! **R9.A2c (A-2, CON-28): NO CODE PATH READS, PROBES OR RE-CHECKS A DISABLED
//! ACCOUNT SLOT, AND `sign_in_needed` HAS EXACTLY ONE WRITER.**
//!
//! `cc2` and `owner-account` are unpaid, disabled and ruled
//! UNAVAILABLE. Every test here uses FIXTURE slots of those names in a temp
//! data dir; nothing reads the real keychain. Two readers are replaced:
//!
//! * the INVENTORY's keychain read (`set_keychain_reader_for_test`) — under
//!   `collect_slot_reports`, i.e. the board's `LoginSlots` hydrate the sampler
//!   runs, `health --json`, rotation's plan (the script executes the
//!   `rotation` verdicts `health --json` carries) and delete's lookup;
//! * the runtime PROBE's keychain read (`set_credential_reader_for_test`) —
//!   under `seed` / auth-failure / sampler reconcile / recheck.
//!
//! Each counts its reads per keychain service, so every "a disabled slot is
//! read 0 times" is paired with "the enabled slot in the same call IS read":
//! the zero is the disable's, not a path that read nothing at all.
//!
//! The inventory reader is process-wide, so each test's slots live in their own
//! temp dir and are therefore their own keychain services; counts never mix.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use threads::accounts::{
    credential_facts_from_blob, keychain_service_for, CredentialFacts, SlotHealth,
    CLONED_CREDENTIAL_MODE, LOGIN_CREDENTIAL_MODE,
};
use threads::{build_manager, Config, ThreadManager};

/// The CON-28 population, as fixtures.
const CON28: [&str; 2] = ["cc2", "owner-account"];
const BAD: &str = "aa-bad";
const GOOD: &str = "zz-good";

// ── fixtures ────────────────────────────────────────────────────────────────

fn set_env() {
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude");
    std::env::set_var("THREADS_CLAUDE_BIN", fake);
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
    // The inventory resolves the machine default's `.claude.json` under HOME;
    // point it at scratch so not even that file of the operator's is opened.
    let home = std::env::temp_dir().join("threads-tests-a2c-home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    install_inventory_reader();
}

/// Slot dirs live where the inventory enumerates them: `<data>/accounts/<n>`.
fn slot_dir(dir: &Path, name: &str) -> PathBuf {
    dir.join("accounts").join(name)
}

fn config_at(dir: &Path, names: &[&str]) -> Config {
    let mut accounts = BTreeMap::new();
    for name in names {
        let cd = slot_dir(dir, name);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join(".claude.json"), "{}").unwrap();
        accounts.insert(name.to_string(), cd.to_string_lossy().to_string());
    }
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn declare(dir: &Path, name: &str, mode: &str, captured_at: &str) {
    let cd = slot_dir(dir, name);
    std::fs::write(
        cd.join("manifest.toml"),
        format!(
            "label = \"{name}\"\ncaptured_at = \"{captured_at}\"\n\
             source_config_dir = \"/nowhere\"\nsource_config_json = \"/nowhere/.claude.json\"\n\
             spawn_env = \"CLAUDE_CONFIG_DIR={d}\"\ncredential_mode = \"{mode}\"\n\
             keychain_service = \"Claude Code-credentials-x\"\nkeychain_account = \"me\"\n",
            d = cd.display()
        ),
    )
    .unwrap();
}

/// A usable grant, identical for one token however often it is built (one
/// clock reading per process — see `slot_probe_clone_stranded::facts`).
fn facts(token: &str) -> CredentialFacts {
    static NOW: OnceLock<i64> = OnceLock::new();
    let now = *NOW.get_or_init(|| chrono::Utc::now().timestamp_millis());
    let f = credential_facts_from_blob(
        &serde_json::json!({"claudeAiOauth": {
            "accessToken": format!("at-{token}"),
            "refreshToken": format!("rt-{token}"),
            "expiresAt": now + 3_600_000,
            "refreshTokenExpiresAt": now + 20 * 86_400_000_i64,
            "scopes": ["user:inference"],
            "subscriptionType": "max",
        }})
        .to_string(),
    );
    assert!(f.secret_sha8.is_some() && f.refresh_token_present, "fixture: a usable grant");
    f
}

/// A WIPED store: the entry exists, its refresh token is blank.
fn wiped() -> CredentialFacts {
    let f = credential_facts_from_blob(
        &serde_json::json!({"claudeAiOauth": {
            "accessToken": "at-w", "refreshToken": "", "expiresAt": 1,
        }})
        .to_string(),
    );
    assert!(f.secret_sha8.is_some() && !f.refresh_token_present, "fixture: wiped");
    f
}

/// The process-wide fixture keychain the inventory reads: service → facts,
/// and every read counted per service.
#[derive(Default)]
struct Keychain {
    facts: BTreeMap<String, CredentialFacts>,
    reads: BTreeMap<String, usize>,
}

fn keychain() -> &'static Mutex<Keychain> {
    static K: OnceLock<Mutex<Keychain>> = OnceLock::new();
    K.get_or_init(Default::default)
}

fn install_inventory_reader() {
    threads::accounts::set_keychain_reader_for_test(Some(Arc::new(|service: &str, _: &str| {
        let mut k = keychain().lock().unwrap();
        *k.reads.entry(service.to_string()).or_default() += 1;
        k.facts.get(service).cloned().unwrap_or_default()
    })));
}

fn service(dir: &Path, name: &str) -> String {
    keychain_service_for(Some(&slot_dir(dir, name).to_string_lossy()))
}

/// Put `token`'s grant in each named slot's fixture store.
fn stock(dir: &Path, stores: &[(&str, &str)]) {
    let mut k = keychain().lock().unwrap();
    for (name, token) in stores {
        k.facts.insert(service(dir, name), facts(token));
    }
}

/// Inventory reads of one slot's store so far.
fn inv_reads(dir: &Path, name: &str) -> usize {
    keychain().lock().unwrap().reads.get(&service(dir, name)).copied().unwrap_or(0)
}

type ProbeReads = Arc<Mutex<BTreeMap<Option<String>, usize>>>;

/// The runtime probe's reader, over the SAME fixture stores, counted per dir.
fn probe_reader(m: &ThreadManager) -> ProbeReads {
    let reads: ProbeReads = Arc::default();
    let r = reads.clone();
    m.set_credential_reader_for_test(Arc::new(move |d: Option<&str>| {
        *r.lock().unwrap().entry(d.map(str::to_string)).or_default() += 1;
        let svc = keychain_service_for(d);
        keychain().lock().unwrap().facts.get(&svc).cloned().unwrap_or_default()
    }));
    reads
}

fn probe_reads(reads: &ProbeReads, dir: &Path, name: &str) -> usize {
    let key = Some(slot_dir(dir, name).to_string_lossy().to_string());
    reads.lock().unwrap().get(&key).copied().unwrap_or(0)
}

/// Pool = CON-28 + one enabled slot; the CON-28 pair disabled in the store
/// the daemon and the CLI both read.
fn con28_pool(dir: &Path) -> ThreadManager {
    let names = [CON28[0], CON28[1], GOOD];
    let m = build_manager(config_at(dir, &names)).unwrap();
    for d in CON28 {
        m.store.set_account_disabled(d, true, Some("unpaid (CON-28)")).unwrap();
    }
    stock(dir, &[(CON28[0], "d0"), (CON28[1], "d1"), (GOOD, "own")]);
    m
}

// ── item 1: every path skips a disabled slot ────────────────────────────────

/// **The sampler's BOARD HYDRATE (A2b inspect-2 F1): with `accounts` TRACKED,
/// one `sample_credentials` tick reads the enabled slot's store and never a
/// disabled one** — neither through the board's `LoginSlots` hydrate
/// (`collect_slot_reports`) nor through the reconcile probe.
///
/// The A2b guards could not see this read: their hub did not track `accounts`,
/// so the hydrate half of the tick never ran, and their counting reader sat
/// under the probe only. Here the shape is subscribed (so `tracked` is true)
/// and the counting reader sits under the inventory.
#[tokio::test]
async fn one_sampler_tick_with_accounts_tracked_reads_the_enabled_slot_and_no_disabled_one() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = con28_pool(tmp.path());
    let probes = probe_reader(&m);
    // The shapes are registered by `start_watchers` (as `serve` and `recover`
    // do). Its credential sampler sleeps its 5-minute period before its first
    // tick, so the one tick below is the only one this test sees.
    m.start_watchers();
    let _frames = m.hub.connect(9101);
    m.hub.subscribe("accounts", None, 9101, None).unwrap();
    assert!(m.hub.tracked("accounts"), "fixture: the board is open");
    // The subscribe hydrates `LoginSlots` on the shape's own task. Let it
    // finish (reads stable for 500 ms) so the count below is the TICK's alone
    // and not the subscribe's landing late (doctrine 3).
    let mut last = usize::MAX;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let now = inv_reads(tmp.path(), GOOD);
        if now == last && now >= 1 {
            break;
        }
        last = now;
    }

    let good_before = inv_reads(tmp.path(), GOOD);
    assert!(good_before >= 1, "fixture: the subscribe's hydrate read the enabled slot");
    m.sample_credentials();

    assert!(
        inv_reads(tmp.path(), GOOD) > good_before,
        "the tick's board hydrate reads the enabled slot's store"
    );
    assert!(probe_reads(&probes, tmp.path(), GOOD) >= 1, "the tick's reconcile probes it");
    for d in CON28 {
        assert_eq!(inv_reads(tmp.path(), d), 0, "{d}: board hydrate read a disabled store");
        assert_eq!(probe_reads(&probes, tmp.path(), d), 0, "{d}: reconcile probed a disabled slot");
    }
}

/// **`health` (`threads accounts health --json` and the `accounts.health` WS
/// arm — one builder): a disabled slot is not read, and still has its row,
/// built from config alone — state `disabled`, no secret, no dates.** A
/// disabled account staying on this list is `account_disable.rs`'s rule.
/// Paired: the enabled slot IS read and its row carries the reading (`live`).
#[tokio::test]
async fn health_reads_the_enabled_slot_and_rows_the_disabled_ones_unread() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let _m = con28_pool(tmp.path());
    let home = tempfile::tempdir().unwrap();

    let v = threads::accounts::health_json(tmp.path(), home.path(), 0).unwrap();

    let rows: BTreeMap<String, serde_json::Value> = v["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["label"].as_str().unwrap().to_string(), r.clone()))
        .collect();
    assert_eq!(rows[GOOD]["health"]["state"], "live", "{}", rows[GOOD]);
    assert!(rows[GOOD]["secret_sha8"].is_string(), "the enabled row carries its reading");
    assert!(inv_reads(tmp.path(), GOOD) >= 1, "the enabled slot is read");
    for d in CON28 {
        let row = rows.get(d).unwrap_or_else(|| panic!("{d} vanished from health: {v}"));
        assert_eq!(row["health"]["state"], "disabled", "{row}");
        assert!(row["secret_sha8"].is_null(), "{d} carries a reading: {row}");
        assert_eq!(inv_reads(tmp.path(), d), 0, "{d}: health read a disabled store");
    }
}

/// **Rotation's plan: `claude-accounts rotate` executes the `rotation` verdict
/// of each `health --json` row, so a disabled slot gets `skip` — never
/// `refresh`, and never "sign it in" (CON-28).** Paired: the enabled own-login
/// slot, in the same plan, IS planned for `refresh`. (With the skip removed a
/// disabled own-login slot is planned for `refresh` too — measured.)
#[tokio::test]
async fn rotation_plans_the_enabled_slot_and_skips_a_disabled_one_unread() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let _m = con28_pool(tmp.path());
    for n in [CON28[0], CON28[1], GOOD] {
        declare(tmp.path(), n, LOGIN_CREDENTIAL_MODE, "2026-07-29T18:59:00Z");
    }
    let home = tempfile::tempdir().unwrap();

    // Two hours on: the fixture's access token (1 h) has lapsed and its login
    // (20 d) has not, so an own-login slot's honest verdict is `refresh`.
    let now_ms = chrono::Utc::now().timestamp_millis() + 2 * 3_600_000;
    let v = threads::accounts::health_json(tmp.path(), home.path(), now_ms).unwrap();
    let plan: BTreeMap<String, (String, String)> = v["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["label"].as_str().unwrap().to_string(),
                (
                    r["rotation"]["action"].as_str().unwrap_or("?").to_string(),
                    r["rotation"]["reason"].as_str().unwrap_or("").to_string(),
                ),
            )
        })
        .collect();
    assert_eq!(plan.get(GOOD).map(|p| p.0.as_str()), Some("refresh"), "{plan:?}");
    for d in CON28 {
        let (action, reason) = plan.get(d).unwrap_or_else(|| panic!("{d} has no verdict: {plan:?}"));
        assert_eq!(action, "skip", "{d}: {plan:?}");
        assert!(reason.contains("disabled"), "{d}: {reason}");
        assert!(!reason.contains("sign"), "{d}: sign-in advice for a disabled account: {reason}");
        assert_eq!(inv_reads(tmp.path(), d), 0, "{d}: rotation's plan read a disabled store");
    }
}

/// **Delete (`accounts/{account}/delete` and `threads accounts delete` share
/// `accounts::delete_lookup`): a DISABLED account CAN be deleted, and its
/// store is read 0 times doing it** (R9.A2c-r2, inspect F1 — CON-28 bars
/// reading a disabled slot, not removing it). The enabled slot in the same
/// lookup IS read. The delete itself runs with `KeychainMode::Skip` (fixture
/// slots only), and the slot's directory is gone afterwards.
#[tokio::test]
async fn deleting_a_disabled_account_succeeds_and_reads_nothing() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let _m = con28_pool(tmp.path());
    let home = tempfile::tempdir().unwrap();
    let mut registry = threads::accounts::Registry::default();
    registry.ensure_default();

    for d in CON28 {
        let why = threads::accounts::delete_lookup(&registry, tmp.path(), home.path(), d, false, 0);
        assert_eq!(why, None, "{d}: deleting a disabled account is refused");
    }
    assert!(inv_reads(tmp.path(), GOOD) >= 1, "the lookup reads the enabled slot");
    for d in CON28 {
        assert_eq!(inv_reads(tmp.path(), d), 0, "{d}: delete's lookup read a disabled store");
        let o = threads::accounts::delete_account(
            tmp.path(),
            d,
            threads::accounts::KeychainMode::Skip,
        )
        .unwrap();
        assert!(o.dir_removed, "{d}: its slot dir was not removed");
        assert!(!slot_dir(tmp.path(), d).exists(), "{d}: slot dir still on disk");
        assert_eq!(inv_reads(tmp.path(), d), 0, "{d}: deleting it read its store");
    }
}

/// PAIRED with the above: the lookup still REFUSES a name that is no account,
/// and still refuses (without `--force`) a DISABLED account that the
/// `set-live-login` pointer names — derived from the pointer file, unread.
#[tokio::test]
async fn delete_still_refuses_a_missing_account_and_a_disabled_live_login() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let _m = con28_pool(tmp.path());
    let home = tempfile::tempdir().unwrap();
    let mut registry = threads::accounts::Registry::default();
    registry.ensure_default();

    let why = threads::accounts::delete_lookup(&registry, tmp.path(), home.path(), "no-such", false, 0);
    assert!(
        why.as_deref().is_some_and(|w| w.contains("there is no account")),
        "a missing account must be refused: {why:?}"
    );

    threads::accounts::write_live_login_pointer(tmp.path(), CON28[0]).unwrap();
    let why = threads::accounts::delete_lookup(&registry, tmp.path(), home.path(), CON28[0], false, 0);
    assert!(
        why.as_deref().is_some_and(|w| w.contains("machine login is currently signed in as")),
        "the disabled live login must be refused without --force: {why:?}"
    );
    let forced = threads::accounts::delete_lookup(&registry, tmp.path(), home.path(), CON28[0], true, 0);
    assert_eq!(forced, None, "--force overrides the live-login guard");
    assert_eq!(inv_reads(tmp.path(), CON28[0]), 0, "deciding it read the disabled store");
}

/// **The gauge `/usage` poll: a disabled account is not a poll target, and a
/// probe naming it is refused before anything is spawned.** Paired: the
/// enabled account IS a target and IS polled (the fake `claude`'s `/usage`
/// records readings for it).
#[tokio::test]
async fn the_gauge_polls_the_enabled_account_and_never_a_disabled_one() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = con28_pool(tmp.path());

    let targets = m.poll_targets();
    assert_eq!(targets, vec![GOOD.to_string()], "poll targets");
    let n = m.probe_account_for_test(GOOD).await.expect("the enabled account is polled");
    assert!(n > 0, "fixture: the fake /usage recorded readings, got {n}");
    for d in CON28 {
        let err = m.probe_account_for_test(d).await.expect_err("a disabled account is polled");
        assert!(err.contains("disabled"), "{d}: {err}");
    }
}

// ── item 2: one writer, and a sample tick cannot undo a newer probe ─────────

/// **A sampler tick whose read of `aa-bad` STARTED before a probe marked it
/// WIPED does not remove the label when it finishes** — the interleave that
/// MERGE-32 cp5 suspected behind `slot_probe_clone_stranded`'s full-suite red.
///
/// Deterministic: the tick's first read of `aa-bad` blocks on a channel; while
/// it is parked, the store is wiped and the boot probe (`seed_sign_in_needed`)
/// marks the slot; then the tick is released with its (older) `live` reading.
///
/// PAIRED in the same test: a tick that starts AFTER the mark and reads the
/// slot usable DOES remove the label — so the guard is ordering, not "a sample
/// never lifts".
#[tokio::test]
async fn a_sample_tick_that_started_before_a_probe_marked_the_slot_does_not_unmark_it() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &[BAD, GOOD])).unwrap();
    declare(tmp.path(), BAD, LOGIN_CREDENTIAL_MODE, "2026-07-29T18:59:00Z");
    stock(tmp.path(), &[(BAD, "bad"), (GOOD, "own")]);

    let bad_dir = slot_dir(tmp.path(), BAD).to_string_lossy().to_string();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let gate = Arc::new(Mutex::new(Some((entered_tx, release_rx))));
    let wiped_now = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let gate = gate.clone();
        let wiped_now = wiped_now.clone();
        let bad_dir = bad_dir.clone();
        m.set_credential_reader_for_test(Arc::new(move |d: Option<&str>| {
            if d == Some(bad_dir.as_str()) {
                // The FIRST read of aa-bad (the tick's) parks until released,
                // and returns what the store held when it STARTED: live.
                let parked = gate.lock().unwrap().take();
                if let Some((entered, release)) = parked {
                    let reading = facts("bad");
                    entered.send(()).unwrap();
                    release.recv().unwrap();
                    return reading;
                }
                if wiped_now.load(std::sync::atomic::Ordering::SeqCst) {
                    return wiped();
                }
                return facts("bad");
            }
            keychain().lock().unwrap().facts.get(&keychain_service_for(d)).cloned().unwrap_or_default()
        }));
    }

    let tick = {
        let m = m.clone();
        std::thread::spawn(move || m.sample_credentials())
    };
    entered_rx.recv().unwrap();
    // While the tick's read is in flight: the store is wiped, and the probe
    // (a newer reading) marks the slot.
    wiped_now.store(true, std::sync::atomic::Ordering::SeqCst);
    m.seed_sign_in_needed();
    assert_eq!(m.sign_in_needed_labels(), vec![BAD.to_string()], "fixture: the probe marked it");
    release_tx.send(()).unwrap();
    tick.join().unwrap();

    assert_eq!(
        m.sign_in_needed_labels(),
        vec![BAD.to_string()],
        "the tick's OLDER live reading removed the label the probe set"
    );
    assert_eq!(
        m.account_unavailability().get(BAD),
        Some(&vec!["login_wiped".to_string()])
    );

    // PAIRED: a tick that STARTS after the mark, reading the slot live again
    // (signed in), does lift it.
    wiped_now.store(false, std::sync::atomic::Ordering::SeqCst);
    m.sample_credentials();
    assert!(m.sign_in_needed_labels().is_empty(), "{:?}", m.sign_in_needed_labels());
    assert_eq!(m.account_unavailability().get(BAD), None);
}

// ── item 3: the board does not draw the ORIGINAL of a clone pair as a clone ──

/// **Two pool slots holding one session: the board (`collect_slot_reports`,
/// what the `LoginSlots` hydrate draws) shows the CAPTURE as `CloneOf` its
/// original, and the ORIGINAL as `Live`** — the rule the runtime probe already
/// applies (`is_the_copy`). PAIRED: when provenance cannot be told apart (no
/// manifests), both stay `CloneOf`, the safe side — so the original's `Live`
/// is the rule's, not a classifier that stopped detecting clones.
#[tokio::test]
async fn the_board_draws_only_the_copy_of_a_clone_pair_as_a_clone() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let orig = "aa-original";
    let copy = "mm-copy";
    let _m = build_manager(config_at(tmp.path(), &[orig, copy])).unwrap();
    stock(tmp.path(), &[(orig, "shared-a2c"), (copy, "shared-a2c")]);
    let home = tempfile::tempdir().unwrap();
    let mut registry = threads::accounts::Registry::default();
    registry.ensure_default();
    let health = |dir: &Path| -> BTreeMap<String, SlotHealth> {
        threads::accounts::collect_slot_reports(&registry, dir, home.path(), 0, 7.0)
            .into_iter()
            .map(|r| (r.label, r.health))
            .collect()
    };

    // Undecidable first (no manifests): both copies.
    let h = health(tmp.path());
    assert_eq!(h[orig], SlotHealth::CloneOf { of: copy.into() }, "{h:?}");
    assert_eq!(h[copy], SlotHealth::CloneOf { of: orig.into() }, "{h:?}");

    // Provenance declared: the original is its own login, the copy a capture.
    declare(tmp.path(), orig, LOGIN_CREDENTIAL_MODE, "2026-07-01T00:00:00Z");
    declare(tmp.path(), copy, CLONED_CREDENTIAL_MODE, "2026-07-29T18:59:00Z");
    let h = health(tmp.path());
    assert_eq!(h[orig], SlotHealth::Live, "the ORIGINAL is drawn as a clone: {h:?}");
    assert_eq!(h[copy], SlotHealth::CloneOf { of: orig.into() }, "{h:?}");
}
