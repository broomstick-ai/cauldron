//! **THE RUNTIME SLOT PROBE CAN RETURN `CloneOf` AND `Stranded`, AND A CLONED
//! SLOT IS NEVER PLACED ON (R9.A2, gap A-2).**
//!
//! The probe placement reads called `classify_slot(&facts, None, None, …)`: no
//! manifest credential mode and no peer hashes, so the two clone states were
//! unreachable from it. `login_stranded` never fired in production, and a slot
//! whose secret is byte-identical to another store's — possibly the operator's
//! own login — was placed on, refreshed the shared token, and logged its twin
//! out.
//!
//! These tests keep the PRODUCTION probe (peer table, manifest read,
//! classification) and replace only the keychain read under it
//! (`set_credential_reader_for_test`), so the clone states are reached the way
//! they are reached in production. The ranking is rigged toward `aa-bad`, so
//! nothing but the availability predicate keeps placement off it; each refusal
//! is paired with a counterpart in which the same rig DOES place on `aa-bad`.
//!
//! Doctrine 10: which real accounts in the operator's manifest sit in the
//! clone / stranded regimes is NOT measured here (no live reads in this step).
//!
//! R9.A2b adds two rules (CON-28 and A2's inspect F1): a DISABLED slot is never
//! read at all — not at boot, not on the recheck, not as anyone's clone peer —
//! and of two pool slots holding one session only the COPY is barred; the
//! original stays placeable.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use threads::accounts::{credential_facts_from_blob, CredentialFacts};
use threads::gauge::Axis;
use threads::{build_manager, Config, ThreadManager, ThreadSettings};

const BAD: &str = "aa-bad";
const TWIN: &str = "mm-twin";
const GOOD: &str = "zz-good";

fn set_env() {
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake);
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// A pool of `names`, each a signed-in-looking slot dir (it has a
/// `.claude.json`, so the probe asks the credential reader about it).
fn config_at(dir: &Path, names: &[&str]) -> Config {
    let mut accounts = BTreeMap::new();
    for name in names {
        let cd = dir.join(name);
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
fn declare_mode(dir: &Path, name: &str, mode: &str) {
    declare_mode_at(dir, name, mode, "2026-07-29T18:59:00Z");
}

/// [`declare_mode`] with the slot's `captured_at` (when it was established).
fn declare_mode_at(dir: &Path, name: &str, mode: &str, captured_at: &str) {
    let cd = dir.join(name);
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

/// A healthy-looking credential blob, distinct per `token` — and IDENTICAL for
/// one token however many times it is built.
///
/// R9.A2b: `secret_sha8` hashes the WHOLE blob, dates included, and this used
/// to stamp each call with its own `Utc::now()`. Two stores built for one
/// "shared" token a millisecond apart then hashed differently, so the clone
/// tests went red at random (measured at 690cfd0d: 2 of 5 runs of the binary,
/// 1 of 8 runs of `two_slots_…` alone). One clock reading per process now.
fn facts(token: &str) -> CredentialFacts {
    static NOW: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    let now = *NOW.get_or_init(|| chrono::Utc::now().timestamp_millis());
    let blob = serde_json::json!({"claudeAiOauth": {
        "accessToken": format!("at-{token}"),
        "refreshToken": format!("rt-{token}"),
        "expiresAt": now + 3_600_000,
        "refreshTokenExpiresAt": now + 20 * 86_400_000_i64,
        "scopes": ["user:inference"],
        "subscriptionType": "max",
    }})
    .to_string();
    let f = credential_facts_from_blob(&blob);
    assert!(f.secret_sha8.is_some() && f.refresh_token_present, "fixture: a usable grant");
    f
}

/// Install a credential reader: `stores` maps a slot NAME (or `None` for the
/// machine default) to the token its store holds. Any other store is absent.
fn reader(m: &ThreadManager, dir: &Path, stores: &[(Option<&str>, &str)]) {
    let map: BTreeMap<Option<String>, CredentialFacts> = stores
        .iter()
        .map(|(name, token)| {
            (
                name.map(|n| dir.join(n).to_string_lossy().to_string()),
                facts(token),
            )
        })
        .collect();
    m.set_credential_reader_for_test(Arc::new(move |d: Option<&str>| {
        map.get(&d.map(str::to_string)).cloned().unwrap_or_default()
    }));
}

/// Rig the ranker toward `aa-bad`, then `mm-twin`, then `zz-good`.
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

/// Boot's one reading of every slot, then one fresh placement.
fn placed_on(m: &ThreadManager, cwd: &Path) -> String {
    m.seed_sign_in_needed();
    let id = m.create_thread(None, settings(cwd)).unwrap();
    m.serving_account(&id).unwrap()
}

// ── CloneOf ─────────────────────────────────────────────────────────────────

/// Two pool slots hold one session (byte-identical secrets): both read
/// `CloneOf`, both are unavailable with `login_clone-of`, and placement goes to
/// the independent login even though the ranking prefers the clones.
///
/// FORCED RED without the fix: the probe classified with no peers, so both
/// read `Live`, and the rigged ranking placed the thread on `aa-bad`.
#[tokio::test]
async fn two_slots_sharing_one_credential_read_clone_of_and_are_never_placed_on() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [BAD, TWIN, GOOD];
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    reader(&m, tmp.path(), &[(Some(BAD), "shared"), (Some(TWIN), "shared"), (Some(GOOD), "own")]);
    rig(&m, &names);

    let account = placed_on(&m, tmp.path());
    let why = m.account_unavailability();
    assert_eq!(why.get(BAD), Some(&vec!["login_clone-of".to_string()]), "{why:?}");
    assert_eq!(why.get(TWIN), Some(&vec!["login_clone-of".to_string()]), "{why:?}");
    assert_eq!(why.get(GOOD), None, "the independent login stays available: {why:?}");
    assert_eq!(account, GOOD, "placement refuses both copies of the shared session");
}

/// The operator's own case: a pool slot holds a byte-identical copy of the
/// MACHINE DEFAULT store. It reads `CloneOf { of: "the live login" }` and is
/// never placed on — a turn there would log the operator's own login out.
#[tokio::test]
async fn a_slot_cloned_from_the_live_login_is_never_placed_on() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [BAD, GOOD];
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    reader(&m, tmp.path(), &[(None, "operator"), (Some(BAD), "operator"), (Some(GOOD), "own")]);
    rig(&m, &names);

    let account = placed_on(&m, tmp.path());
    let why = m.account_unavailability();
    assert_eq!(why.get(BAD), Some(&vec!["login_clone-of".to_string()]), "{why:?}");
    assert_eq!(account, GOOD);
}

/// PAIRED: the same pool and the same rig, every store distinct. Nobody is a
/// clone, nobody is unavailable, and the rig DOES place on `aa-bad` — so the
/// refusals above are the predicate's, not the rig's or a constant's.
#[tokio::test]
async fn distinct_slots_are_all_live_and_placeable() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [BAD, TWIN, GOOD];
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    reader(
        &m,
        tmp.path(),
        &[(None, "operator"), (Some(BAD), "a"), (Some(TWIN), "b"), (Some(GOOD), "c")],
    );
    rig(&m, &names);

    let account = placed_on(&m, tmp.path());
    assert!(m.account_unavailability().is_empty(), "{:?}", m.account_unavailability());
    assert_eq!(account, BAD, "fixture: the rig places on aa-bad when it is available");
}

// ── Stranded ────────────────────────────────────────────────────────────────

/// A CAPTURE (`credential_mode = "keychain-cloned"`) whose secret no longer
/// matches any store: its twin rotated the token away. It reads `Stranded`, is
/// unavailable with `login_stranded`, and is never placed on.
///
/// FORCED RED without the fix: the probe never read the manifest, so the slot
/// read `Live` (its dates are fine) and won the rigged ranking.
#[tokio::test]
async fn a_stranded_capture_reads_stranded_and_is_never_placed_on() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [BAD, GOOD];
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    declare_mode(tmp.path(), BAD, threads::accounts::CLONED_CREDENTIAL_MODE);
    reader(&m, tmp.path(), &[(None, "rotated"), (Some(BAD), "stale"), (Some(GOOD), "own")]);
    rig(&m, &names);

    let account = placed_on(&m, tmp.path());
    let why = m.account_unavailability();
    assert_eq!(why.get(BAD), Some(&vec!["login_stranded".to_string()]), "{why:?}");
    assert_eq!(why.get(GOOD), None, "{why:?}");
    assert_eq!(account, GOOD);
}

/// PAIRED: the same slot declaring its OWN login is `Live` and placeable — the
/// refusal above is the declared capture, not the manifest's mere presence.
#[tokio::test]
async fn a_slot_declaring_its_own_login_is_live_and_placeable() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [BAD, GOOD];
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    declare_mode(tmp.path(), BAD, threads::accounts::LOGIN_CREDENTIAL_MODE);
    reader(&m, tmp.path(), &[(None, "rotated"), (Some(BAD), "stale"), (Some(GOOD), "own")]);
    rig(&m, &names);

    let account = placed_on(&m, tmp.path());
    assert!(m.account_unavailability().is_empty(), "{:?}", m.account_unavailability());
    assert_eq!(account, BAD);
}

// ── R9.A2b: only the COPY of a pool clone pair is barred ────────────────────

/// Two pool slots hold one session. `aa-bad` declares its OWN login, `mm-twin`
/// declares it was CAPTURED: `mm-twin` copied `aa-bad`. Only the copy reads
/// `CloneOf` and is barred; the original stays placeable, and the rig (which
/// prefers `aa-bad`) places on it.
///
/// FORCED RED without the fix: `classify_slot` saw the twin on both sides, so
/// `aa-bad` read `login_clone-of` too and placement went to `zz-good`.
#[tokio::test]
async fn the_original_of_a_clone_pair_stays_placeable_and_only_the_copy_is_barred() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [BAD, TWIN, GOOD];
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    declare_mode(tmp.path(), BAD, threads::accounts::LOGIN_CREDENTIAL_MODE);
    declare_mode(tmp.path(), TWIN, threads::accounts::CLONED_CREDENTIAL_MODE);
    reader(&m, tmp.path(), &[(Some(BAD), "shared"), (Some(TWIN), "shared"), (Some(GOOD), "own")]);
    rig(&m, &names);

    let account = placed_on(&m, tmp.path());
    let why = m.account_unavailability();
    assert_eq!(why.get(BAD), None, "the original stays available: {why:?}");
    assert_eq!(why.get(TWIN), Some(&vec!["login_clone-of".to_string()]), "{why:?}");
    assert_eq!(account, BAD, "placement uses the original");
}

/// PAIRED, mirrored: the SAME pair with the declarations swapped — now
/// `aa-bad` is the capture. It is barred and the original `mm-twin` is placed
/// on. So the rule follows the declaration, not the label or the rank.
#[tokio::test]
async fn the_copy_of_a_clone_pair_is_barred_whichever_label_it_holds() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [BAD, TWIN, GOOD];
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    declare_mode(tmp.path(), BAD, threads::accounts::CLONED_CREDENTIAL_MODE);
    declare_mode(tmp.path(), TWIN, threads::accounts::LOGIN_CREDENTIAL_MODE);
    reader(&m, tmp.path(), &[(Some(BAD), "shared"), (Some(TWIN), "shared"), (Some(GOOD), "own")]);
    rig(&m, &names);

    let account = placed_on(&m, tmp.path());
    let why = m.account_unavailability();
    assert_eq!(why.get(BAD), Some(&vec!["login_clone-of".to_string()]), "{why:?}");
    assert_eq!(why.get(TWIN), None, "{why:?}");
    assert_eq!(account, TWIN);
}

/// Neither declares a capture (both own-login): the EARLIER-established slot
/// (`captured_at`) is the original. `mm-twin` was set up first, so `aa-bad`
/// is the copy.
#[tokio::test]
async fn with_no_capture_declared_the_earlier_established_slot_is_the_original() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [BAD, TWIN, GOOD];
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    let own = threads::accounts::LOGIN_CREDENTIAL_MODE;
    declare_mode_at(tmp.path(), BAD, own, "2026-09-01T00:00:00Z");
    declare_mode_at(tmp.path(), TWIN, own, "2026-08-01T00:00:00Z");
    reader(&m, tmp.path(), &[(Some(BAD), "shared"), (Some(TWIN), "shared"), (Some(GOOD), "own")]);
    rig(&m, &names);

    let account = placed_on(&m, tmp.path());
    let why = m.account_unavailability();
    assert_eq!(why.get(BAD), Some(&vec!["login_clone-of".to_string()]), "{why:?}");
    assert_eq!(why.get(TWIN), None, "{why:?}");
    assert_eq!(account, TWIN);
}

// ── R9.A2b / CON-28: a DISABLED slot is never checked ───────────────────────

/// The CON-28 population, as fixtures: unpaid, disabled, ruled unavailable.
const CON28_DISABLED: [&str; 2] = ["cc2", "owner-account"];

type Reads = Arc<std::sync::Mutex<BTreeMap<Option<String>, usize>>>;

/// [`reader`], counting every read per store (by config dir).
fn counting_reader(m: &ThreadManager, dir: &Path, stores: &[(Option<&str>, &str)]) -> Reads {
    let map: BTreeMap<Option<String>, CredentialFacts> = stores
        .iter()
        .map(|(name, token)| {
            (
                name.map(|n| dir.join(n).to_string_lossy().to_string()),
                facts(token),
            )
        })
        .collect();
    let reads: Reads = Arc::default();
    let r = reads.clone();
    m.set_credential_reader_for_test(Arc::new(move |d: Option<&str>| {
        let key = d.map(str::to_string);
        *r.lock().unwrap().entry(key.clone()).or_default() += 1;
        map.get(&key).cloned().unwrap_or_default()
    }));
    reads
}

fn reads_of(reads: &Reads, dir: &Path, name: &str) -> usize {
    let key = Some(dir.join(name).to_string_lossy().to_string());
    reads.lock().unwrap().get(&key).copied().unwrap_or(0)
}

/// **ZERO keychain reads of a disabled slot across MORE THAN ONE sign-in
/// recheck period** — the production period is 30 s (`SIGN_IN_RECHECK`); here
/// it is injected at 50 ms and outlasted three times over (doctrine 4).
///
/// Each disabled slot is a stranded capture holding a usable grant, so the
/// moment ANY gate lets it through — the boot seed, the periodic recheck, or
/// the N+1 peer read another slot's probe does — it is read and counted.
///
/// PAIRED in the same run: `aa-bad`, an ENABLED stranded capture, IS re-read
/// on every period that elapses, so the zero above is the disable's, not a
/// recheck that never ran.
#[tokio::test]
async fn a_disabled_slot_is_never_read_across_three_50ms_sign_in_recheck_periods() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [CON28_DISABLED[0], CON28_DISABLED[1], BAD, GOOD];
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    for d in CON28_DISABLED {
        m.store.set_account_disabled(d, true, Some("unpaid (CON-28)")).unwrap();
        declare_mode(tmp.path(), d, threads::accounts::CLONED_CREDENTIAL_MODE);
    }
    declare_mode(tmp.path(), BAD, threads::accounts::CLONED_CREDENTIAL_MODE);
    let reads = counting_reader(
        &m,
        tmp.path(),
        &[
            (None, "operator"),
            (Some(CON28_DISABLED[0]), "d0"),
            (Some(CON28_DISABLED[1]), "d1"),
            (Some(BAD), "stale"),
            (Some(GOOD), "own"),
        ],
    );
    m.set_sign_in_recheck_for_test(std::time::Duration::from_millis(50));

    m.seed_sign_in_needed();
    let after_seed = reads_of(&reads, tmp.path(), BAD);
    assert!(after_seed >= 1, "fixture: boot reads the enabled slot");
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(120));
        m.notice_expired_limits();
    }

    for d in CON28_DISABLED {
        assert_eq!(reads_of(&reads, tmp.path(), d), 0, "{d} was read: {:?}", reads.lock().unwrap());
    }
    assert_eq!(
        reads_of(&reads, tmp.path(), BAD),
        after_seed + 3,
        "the enabled stranded slot is re-read once per elapsed period: {:?}",
        reads.lock().unwrap()
    );
    let why = m.account_unavailability();
    for d in CON28_DISABLED {
        assert_eq!(why.get(d), Some(&vec!["disabled".to_string()]), "{why:?}");
    }
    assert_eq!(why.get(BAD), Some(&vec!["login_stranded".to_string()]), "{why:?}");
}

/// A slot marked BEFORE it was disabled leaves the recheck unread: the next
/// due tick drops it from the set without a keychain read, and from then on it
/// is `disabled` from config alone.
#[tokio::test]
async fn a_slot_disabled_after_it_was_marked_is_dropped_unread_after_one_50ms_recheck_period() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [CON28_DISABLED[0], GOOD];
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    declare_mode(tmp.path(), CON28_DISABLED[0], threads::accounts::CLONED_CREDENTIAL_MODE);
    let reads = counting_reader(
        &m,
        tmp.path(),
        &[(None, "operator"), (Some(CON28_DISABLED[0]), "d0"), (Some(GOOD), "own")],
    );
    m.set_sign_in_recheck_for_test(std::time::Duration::from_millis(50));
    m.seed_sign_in_needed();
    assert_eq!(m.sign_in_needed_labels(), vec![CON28_DISABLED[0].to_string()]);
    let before = reads_of(&reads, tmp.path(), CON28_DISABLED[0]);
    assert!(before >= 1, "fixture: an enabled stranded slot is read at boot");

    m.store.set_account_disabled(CON28_DISABLED[0], true, None).unwrap();
    for _ in 0..2 {
        std::thread::sleep(std::time::Duration::from_millis(120));
        m.notice_expired_limits();
    }
    assert_eq!(reads_of(&reads, tmp.path(), CON28_DISABLED[0]), before, "re-read after disable");
    assert!(m.sign_in_needed_labels().is_empty(), "{:?}", m.sign_in_needed_labels());
    assert_eq!(
        m.account_unavailability().get(CON28_DISABLED[0]),
        Some(&vec!["disabled".to_string()])
    );
}

/// **Doctrine 10: name the population.** The labels the probe never reads are
/// EXACTLY the disabled set — `cc2` and `owner-account` — and every
/// enabled label (the operator's other real labels, as fixtures) IS read. So
/// the skip path is neither empty nor wider than the disable.
#[tokio::test]
async fn the_skip_path_is_exactly_the_disabled_set_cc2_and_owner_account() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let enabled = ["alice-example-com", "owner-gmail"];
    let names = [CON28_DISABLED[0], CON28_DISABLED[1], enabled[0], enabled[1]];
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    for d in CON28_DISABLED {
        m.store.set_account_disabled(d, true, Some("unpaid (CON-28)")).unwrap();
    }
    let stores: Vec<(Option<&str>, &str)> =
        std::iter::once((None, "operator")).chain(names.iter().map(|n| (Some(*n), *n))).collect();
    let reads = counting_reader(&m, tmp.path(), &stores);

    m.seed_sign_in_needed();

    let disabled: Vec<&str> = names.iter().copied().filter(|n| m.budgets.is_disabled(n)).collect();
    assert_eq!(disabled, CON28_DISABLED.to_vec(), "fixture: the disabled set");
    let unread: Vec<&str> =
        names.iter().copied().filter(|n| reads_of(&reads, tmp.path(), n) == 0).collect();
    assert_eq!(unread, CON28_DISABLED.to_vec(), "the skip path is the disabled set: {:?}", reads);
    for e in enabled {
        assert!(reads_of(&reads, tmp.path(), e) >= 1, "{e} must be read");
    }
}

/// **R9.A2b-r (E17-16): a DISABLED slot passes through a credential-sampler
/// tick (`sample_credentials` → `reconcile_sign_in_needed`, A-1) with ZERO
/// keychain reads and ends WITHOUT a sign-in label** — a disabled slot shows
/// only `disabled` (CON-28: no label that reads as "sign in to it").
///
/// `cc2` is marked stranded while enabled, then disabled: the tick drops its
/// label unread. `owner-account` is disabled from the start: no label
/// appears. PAIRED in the same tick: the enabled stranded `aa-bad` IS read and
/// KEEPS its label, so the drop is the disable's, not a tick that clears all.
#[tokio::test]
async fn a_disabled_slot_passes_a_sampler_tick_unread_and_ends_without_its_sign_in_label() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let names = [CON28_DISABLED[0], CON28_DISABLED[1], BAD, GOOD];
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config_at(tmp.path(), &names)).unwrap();
    m.store.set_account_disabled(CON28_DISABLED[1], true, Some("unpaid (CON-28)")).unwrap();
    for n in [CON28_DISABLED[0], CON28_DISABLED[1], BAD] {
        declare_mode(tmp.path(), n, threads::accounts::CLONED_CREDENTIAL_MODE);
    }
    let reads = counting_reader(
        &m,
        tmp.path(),
        &[
            (None, "operator"),
            (Some(CON28_DISABLED[0]), "d0"),
            (Some(CON28_DISABLED[1]), "d1"),
            (Some(BAD), "stale"),
            (Some(GOOD), "own"),
        ],
    );
    m.seed_sign_in_needed();
    assert_eq!(
        m.sign_in_needed_labels(),
        vec![BAD.to_string(), CON28_DISABLED[0].to_string()],
        "fixture: cc2 (still enabled) and aa-bad are marked stranded"
    );
    m.store.set_account_disabled(CON28_DISABLED[0], true, Some("unpaid (CON-28)")).unwrap();
    let before: Vec<usize> = names.iter().map(|n| reads_of(&reads, tmp.path(), n)).collect();

    m.sample_credentials();

    for d in CON28_DISABLED {
        let i = names.iter().position(|n| *n == d).unwrap();
        assert_eq!(
            reads_of(&reads, tmp.path(), d),
            before[i],
            "{d} was read by the sampler tick: {:?}",
            reads.lock().unwrap()
        );
    }
    assert!(
        reads_of(&reads, tmp.path(), BAD) > before[2],
        "the enabled slot is read by the tick: {:?}",
        reads.lock().unwrap()
    );
    assert_eq!(
        m.sign_in_needed_labels(),
        vec![BAD.to_string()],
        "cc2's label dropped, owner-account not added, enabled aa-bad kept"
    );
    let why = m.account_unavailability();
    for d in CON28_DISABLED {
        assert_eq!(why.get(d), Some(&vec!["disabled".to_string()]), "{why:?}");
    }
    assert_eq!(why.get(BAD), Some(&vec!["login_stranded".to_string()]), "{why:?}");
}

// R9.F2b: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
