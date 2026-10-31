//! **R9.T7 (GAP-AUDIT T-7, CON-28): A SLOT THAT DECLARES A KEYCHAIN LOGIN AND
//! HAS NONE IS UNAVAILABLE, SO NO TURN IS PLACED ON IT TO LEARN THAT BY A 401.**
//!
//! `SlotHealth::NoLogin` is deliberately placeable: a plaintext install and
//! every test fixture also have no keychain item, and absence of evidence is
//! not a fault. But a slot whose `manifest.toml` SAYS its login lives in the
//! keychain (`own-login`, `keychain-cloned`) and whose store holds nothing —
//! or whose dir has no `.claude.json` — is not that ambiguity: its next turn
//! fails to authenticate. It now reads `NoLoginDeclared`, word `no-login`, and
//! placement bars it as `login_no-login`; the board (`collect_slot_reports`)
//! classifies it through the same function and draws the same verdict.
//!
//! Fixture slots only, in temp dirs. Two keychain readers are replaced, both
//! counting per store: the runtime PROBE's (`set_credential_reader_for_test`)
//! and the board INVENTORY's (`set_keychain_reader_for_test`, process-wide —
//! each test's slots live in their own temp dir, so their services never mix).
//! Nothing reads the real keychain.
//!
//! The ranking is rigged toward `aa-bad`, so nothing but the availability
//! predicate keeps placement off it; the refusals are paired with the
//! no-manifest and `plaintext-file` slots on which the SAME rig DOES place.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use threads::accounts::{
    credential_facts_from_blob, keychain_service_for, CredentialFacts, SlotHealth, SlotReport,
    CLONED_CREDENTIAL_MODE, LOGIN_CREDENTIAL_MODE,
};
use threads::gauge::Axis;
use threads::{build_manager, Config, ThreadManager, ThreadSettings};

const BAD: &str = "aa-bad";
const GOOD: &str = "zz-good";
/// The CON-28 population, as fixtures.
const CON28: [&str; 2] = ["cc2", "owner-account"];

// ── fixtures ────────────────────────────────────────────────────────────────

fn set_env() {
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude");
    std::env::set_var("THREADS_CLAUDE_BIN", fake);
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
    install_inventory_reader();
}

/// Slot dirs live where the board's inventory enumerates them.
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

/// Write the slot's `manifest.toml` declaring `credential_mode`.
fn declare(dir: &Path, name: &str, mode: &str) {
    let cd = slot_dir(dir, name);
    std::fs::write(
        cd.join("manifest.toml"),
        format!(
            "label = \"{name}\"\ncaptured_at = \"2026-07-29T18:59:00Z\"\n\
             source_config_dir = \"/nowhere\"\nsource_config_json = \"/nowhere/.claude.json\"\n\
             spawn_env = \"CLAUDE_CONFIG_DIR={d}\"\ncredential_mode = \"{mode}\"\n\
             keychain_service = \"Claude Code-credentials-x\"\nkeychain_account = \"me\"\n",
            d = cd.display()
        ),
    )
    .unwrap();
    assert_eq!(
        threads::accounts::manifest_credential_mode(&cd.to_string_lossy()).as_deref(),
        Some(mode),
        "fixture: the manifest parses and declares {mode}"
    );
}

/// A usable grant, identical for one token however often it is built.
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

/// The process-wide fixture keychain: service → facts, every read counted.
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

/// Put `token`'s grant in each named slot's store. Any other store is EMPTY.
fn stock(dir: &Path, stores: &[(&str, &str)]) {
    let mut k = keychain().lock().unwrap();
    for (name, token) in stores {
        k.facts.insert(service(dir, name), facts(token));
    }
}

fn inv_reads(dir: &Path, name: &str) -> usize {
    keychain().lock().unwrap().reads.get(&service(dir, name)).copied().unwrap_or(0)
}

type ProbeReads = Arc<Mutex<BTreeMap<Option<String>, usize>>>;

/// The runtime probe's `read_facts`, over the SAME fixture stores, counted.
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

/// Rig the ranker toward `names[0]`, then the next, …
fn rig(m: &ThreadManager, names: &[&str]) {
    for (i, a) in names.iter().enumerate() {
        let pct = 20.0 * i as f64;
        m.budgets
            .observe(a, Axis::WeeklyAll, pct, Some("2099-01-01T00:00:00Z".into()), "usage_poll")
            .unwrap();
        m.budgets.observe(a, Axis::Session, pct, None, "usage_poll").unwrap();
    }
}

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

/// Boot's one reading of every slot, then `n` fresh placements: the accounts
/// they landed on.
fn placements(m: &ThreadManager, cwd: &Path, n: usize) -> Vec<String> {
    m.seed_sign_in_needed();
    (0..n)
        .map(|_| {
            let id = m.create_thread(None, settings(cwd)).unwrap();
            m.serving_account(&id).unwrap()
        })
        .collect()
}

/// The board's row for `label`: the same inventory `health --json`, the
/// `accounts` shape's `LoginSlots` hydrate and rotation's plan read.
fn board_row(dir: &Path, label: &str) -> SlotReport {
    let home = tempfile::tempdir().unwrap();
    let registry = threads::accounts::Registry::default();
    let rows = threads::accounts::collect_slot_reports(&registry, dir, home.path(), 0, 7.0);
    rows.into_iter()
        .find(|r| r.label == label)
        .unwrap_or_else(|| panic!("{label} is not on the board"))
}

/// The placement reason the board's verdict implies — `login_<word>` when the
/// reading needs a sign-in, none otherwise. The board and placement agree iff
/// this equals placement's own `login_` reason.
fn board_reason(h: &SlotHealth) -> Option<String> {
    h.needs_sign_in().then(|| format!("login_{}", h.word()))
}

fn login_reasons(why: &BTreeMap<String, Vec<String>>, label: &str) -> Vec<String> {
    why.get(label)
        .map(|v| v.iter().filter(|r| r.starts_with("login_")).cloned().collect())
        .unwrap_or_default()
}

// ── (a) a declared keychain login with no item: unavailable, board agrees ──

/// **`own-login` and `keychain-cloned` manifests, EMPTY store: the slot is
/// unavailable as `login_no-login`, none of five placements lands on it even
/// though the rig prefers it, and the board draws the same word and verdict.**
///
/// FORCED RED by reverting the probe/classify change: both early returns gave
/// plain `NoLogin` (placeable), so the rig placed every thread on `aa-bad`.
#[tokio::test]
async fn a_declared_keychain_login_with_an_empty_store_is_unavailable_and_never_placed_on() {
    set_env();
    for mode in [LOGIN_CREDENTIAL_MODE, CLONED_CREDENTIAL_MODE] {
        let tmp = tempfile::tempdir().unwrap();
        let names = [BAD, GOOD];
        let m = build_manager(config_at(tmp.path(), &names)).unwrap();
        declare(tmp.path(), BAD, mode);
        stock(tmp.path(), &[(GOOD, "own")]);
        let probes = probe_reader(&m);
        rig(&m, &names);

        let placed = placements(&m, tmp.path(), 5);
        let why = m.account_unavailability();
        assert_eq!(why.get(BAD), Some(&vec!["login_no-login".to_string()]), "{mode}: {why:?}");
        assert_eq!(why.get(GOOD), None, "{mode}: the signed-in slot stays available: {why:?}");
        assert!(
            placed.iter().all(|a| a == GOOD),
            "{mode}: a turn was placed on the slot with no login: {placed:?}"
        );
        assert!(probe_reads(&probes, tmp.path(), BAD) >= 1, "{mode}: the probe read the slot");

        let row = board_row(tmp.path(), BAD);
        assert_eq!(
            row.health,
            SlotHealth::NoLoginDeclared { declared: mode.to_string() },
            "{mode}: board"
        );
        assert_eq!(row.health.word(), "no-login");
        assert_eq!(
            board_reason(&row.health),
            Some("login_no-login".to_string()),
            "{mode}: the board's verdict is placement's"
        );
        assert_eq!(serde_json::to_value(&row.health).unwrap()["state"], "no-login");
        assert!(inv_reads(tmp.path(), BAD) >= 1, "{mode}: the board read the store");
    }
}

/// **`own-login` manifest and a USABLE grant in the store, but no
/// `.claude.json` in the dir: unavailable as `login_no-login`, never placed
/// on, and the board — asking no store, exactly as the probe asks none —
/// draws the same.** FORCED RED by reverting: the probe's no-`.claude.json`
/// return was plain `NoLogin` (placeable), and the board read the grant and
/// drew `live`, so board and placement both put it in play.
#[tokio::test]
async fn a_declared_keychain_login_with_no_claude_json_is_unavailable_on_board_and_in_placement() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [BAD, GOOD];
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    declare(tmp.path(), BAD, LOGIN_CREDENTIAL_MODE);
    std::fs::remove_file(slot_dir(tmp.path(), BAD).join(".claude.json")).unwrap();
    stock(tmp.path(), &[(BAD, "orphan"), (GOOD, "own")]);
    let probes = probe_reader(&m);
    rig(&m, &names);

    let placed = placements(&m, tmp.path(), 5);
    let why = m.account_unavailability();
    assert_eq!(why.get(BAD), Some(&vec!["login_no-login".to_string()]), "{why:?}");
    assert!(placed.iter().all(|a| a == GOOD), "{placed:?}");
    // (Its store IS still read once — as `zz-good`'s clone PEER, which is the
    // A2 peer table's business and unchanged here; its OWN reading asks none.)
    let _ = probe_reads(&probes, tmp.path(), BAD);

    let row = board_row(tmp.path(), BAD);
    assert_eq!(row.health, SlotHealth::NoLoginDeclared { declared: LOGIN_CREDENTIAL_MODE.into() });
    assert_eq!(board_reason(&row.health), login_reasons(&why, BAD).first().cloned());
}

// ── (b)/(c) paired: no keychain declaration keeps today's placeable NoLogin ─

/// **PAIRED (b): no manifest, empty store → plain `NoLogin`, available, and
/// the SAME rig places every thread on it.** So the refusal above is the
/// declaration's, not the empty store's, the rig's or a constant's.
#[tokio::test]
async fn a_slot_with_no_manifest_and_an_empty_store_stays_placeable() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [BAD, GOOD];
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    stock(tmp.path(), &[(GOOD, "own")]);
    let _probes = probe_reader(&m);
    rig(&m, &names);

    let placed = placements(&m, tmp.path(), 3);
    let why = m.account_unavailability();
    assert!(why.is_empty(), "{why:?}");
    assert!(placed.iter().all(|a| a == BAD), "fixture: the rig places on aa-bad: {placed:?}");

    let row = board_row(tmp.path(), BAD);
    assert_eq!(row.health, SlotHealth::NoLogin);
    assert_eq!(board_reason(&row.health), None);
}

/// **PAIRED (c): a `plaintext-file` (and a `manual-login-required`) manifest,
/// empty store → plain `NoLogin`, available, placed on.** The credential of a
/// plaintext slot is not in the keychain, so an empty keychain says nothing.
#[tokio::test]
async fn a_plaintext_or_manual_manifest_with_an_empty_store_stays_placeable() {
    set_env();
    for mode in ["plaintext-file", "manual-login-required"] {
        let tmp = tempfile::tempdir().unwrap();
        let names = [BAD, GOOD];
        let m = build_manager(config_at(tmp.path(), &names)).unwrap();
        declare(tmp.path(), BAD, mode);
        stock(tmp.path(), &[(GOOD, "own")]);
        let _probes = probe_reader(&m);
        rig(&m, &names);

        let placed = placements(&m, tmp.path(), 3);
        let why = m.account_unavailability();
        assert!(why.is_empty(), "{mode}: {why:?}");
        assert!(placed.iter().all(|a| a == BAD), "{mode}: {placed:?}");

        let row = board_row(tmp.path(), BAD);
        assert_eq!(row.health, SlotHealth::NoLogin, "{mode}");
        assert_eq!(board_reason(&row.health), None, "{mode}");
    }
}

// ── (d) CON-28: a disabled slot is not read, whatever its manifest says ────

/// **(d) `cc2` / `owner-account` as DISABLED fixture slots with
/// `own-login` manifests and empty stores: 0 `read_facts` calls and 0 board
/// keychain reads for either, no `login_` reason (only `disabled`), and never
/// placed on.** Paired: the enabled slot in the same pool IS read by both.
#[tokio::test]
async fn a_disabled_slot_with_an_own_login_manifest_is_never_read() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [CON28[0], CON28[1], GOOD];
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    for d in CON28 {
        declare(tmp.path(), d, LOGIN_CREDENTIAL_MODE);
        m.store.set_account_disabled(d, true, Some("unpaid (CON-28)")).unwrap();
    }
    stock(tmp.path(), &[(GOOD, "own")]);
    let probes = probe_reader(&m);
    rig(&m, &names);

    let placed = placements(&m, tmp.path(), 3);
    m.reconcile_sign_in_needed();
    let _ = threads::accounts::health_json(tmp.path(), tempfile::tempdir().unwrap().path(), 0)
        .unwrap();
    let why = m.account_unavailability();

    assert!(placed.iter().all(|a| a == GOOD), "{placed:?}");
    assert!(probe_reads(&probes, tmp.path(), GOOD) >= 1, "the enabled slot is probed");
    assert!(inv_reads(tmp.path(), GOOD) >= 1, "the enabled slot is on the board, read");
    for d in CON28 {
        assert_eq!(probe_reads(&probes, tmp.path(), d), 0, "{d}: read_facts on a disabled slot");
        assert_eq!(inv_reads(tmp.path(), d), 0, "{d}: the board read a disabled store");
        assert_eq!(why.get(d), Some(&vec!["disabled".to_string()]), "{d}: {why:?}");
    }
}
