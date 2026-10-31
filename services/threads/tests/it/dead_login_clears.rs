//! **R9.A3 (GAP-AUDIT A-3, CON-28): A LOGIN PROVEN GOOD AGAIN LEAVES
//! `login_dead` WITHOUT A COMPLETED TURN.**
//!
//! `dead_since` (the `login_dead` reason in `account_unavailability`) used to
//! clear only on a completed turn — which the spawn guard now forbids on a dead
//! login — or a reported login. So the hint "`claude-accounts login <label>`
//! fixes it" was false for a login re-authenticated by any other route. Now a
//! probe of the slot that reads LIVE lifts it: in the slow recheck
//! (`notice_expired_limits` → `recheck_sign_in_needed`) and in the credential
//! sample (`sample_credentials` → `reconcile_sign_in_needed`), both through the
//! sign-in set's one owner (`apply_sign_in`).
//!
//! Every slot is read through the injectable `slot_probe`, never the keychain,
//! and every read is counted per label so "not read" is paired with "read".
//! `cc2` is a CON-28 fixture: disabled, never probed, never revived, never
//! given a login hint.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::accounts::SlotHealth;
use threads::event::Push;
use threads::{build_manager, Config, ThreadManager};

const DEAD: &str = "aa-dead";
const GOOD: &str = "zz-good";
const CON28: &str = "cc2";
/// Long before any reading this test takes.
const DIED: &str = "2026-01-01T00:00:00Z";
const WHY: &str = "OAuth token has expired. Please run /login";

fn set_env() {
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &Path, names: &[&str]) -> Config {
    let mut accounts = BTreeMap::new();
    for name in names {
        let cd = dir.join("accounts").join(name);
        std::fs::create_dir_all(&cd).unwrap();
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

type Slots = Arc<Mutex<BTreeMap<String, SlotHealth>>>;
type Reads = Arc<Mutex<BTreeMap<String, usize>>>;

/// Every slot reads what `slots` says (Live by default); reads counted.
fn probe(m: &ThreadManager) -> (Slots, Reads) {
    let slots: Slots = Arc::default();
    let reads: Reads = Arc::default();
    let (s, r) = (slots.clone(), reads.clone());
    m.set_slot_probe_for_test(Arc::new(move |label: &str, _dir: Option<&str>| {
        *r.lock().unwrap().entry(label.to_string()).or_default() += 1;
        s.lock().unwrap().get(label).cloned().unwrap_or(SlotHealth::Live)
    }));
    (slots, reads)
}

fn reads_of(reads: &Reads, label: &str) -> usize {
    reads.lock().unwrap().get(label).copied().unwrap_or(0)
}

fn dead_since(m: &ThreadManager, label: &str) -> Option<String> {
    m.store
        .list_account_states()
        .unwrap()
        .into_iter()
        .find(|s| s.name == label)
        .and_then(|s| s.dead_since)
}

fn kill(m: &ThreadManager, label: &str, at: &str) {
    m.store.set_account_health(label, Some((at, WHY))).unwrap();
    assert!(
        m.account_unavailable_reasons(label)
            .is_some_and(|w| w.iter().any(|w| w == "login_dead")),
        "fixture: {label} is unavailable as login_dead"
    );
}

/// Every `accounts` info reason announced for `label` so far.
fn announced(rx: &mut tokio::sync::broadcast::Receiver<Push>, label: &str) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(p) = rx.try_recv() {
        if let Push::Info { domain, reason, account, .. } = p {
            if domain == "accounts" && account.as_deref() == Some(label) {
                out.push(reason);
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
enum Path_ {
    /// The slow recheck on the bar-expiry tick.
    Recheck,
    /// One credential-sample tick.
    Sample,
}

fn tick(m: &ThreadManager, path: Path_) {
    match path {
        Path_::Recheck => {
            m.notice_expired_limits();
        }
        Path_::Sample => m.sample_credentials(),
    }
}

// ── (a) a Live reading lifts it, on either path ─────────────────────────────

async fn live_reading_revives(path: Path_) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &[DEAD, GOOD])).unwrap();
    let (_slots, reads) = probe(&m);
    kill(&m, DEAD, DIED);
    let mut rx = m.subscribe();

    tick(&m, path);

    assert!(reads_of(&reads, DEAD) >= 1, "{path:?}: the dead slot was read");
    assert_eq!(dead_since(&m, DEAD), None, "{path:?}: a Live reading lifts dead_since");
    assert_eq!(
        m.account_unavailable_reasons(DEAD),
        None,
        "{path:?}: the revived login is available again"
    );
    let (barred, unhealthy) = m.barred_accounts();
    assert!(
        !barred.contains(&DEAD.to_string()) && !unhealthy.contains(&DEAD.to_string()),
        "{path:?}: placement no longer bars it: {barred:?} / {unhealthy:?}"
    );
    let said = announced(&mut rx, DEAD);
    assert!(
        said.iter().any(|r| r == "login_revived"),
        "{path:?}: `login_revived` announced for {DEAD}; got {said:?}"
    );
}

/// **Within ONE recheck** — no turn, no reported login, no sample.
///
/// FORCED RED (recorded in the R9.A3 report): with the clear in
/// `apply_sign_in` removed, `dead_since` stays `Some("2026-01-01T00:00:00Z")`.
#[tokio::test]
async fn a_dead_login_whose_slot_reads_live_is_revived_within_one_recheck() {
    live_reading_revives(Path_::Recheck).await;
}

/// **Within ONE sample tick**, through the same owner.
#[tokio::test]
async fn a_dead_login_whose_slot_reads_live_is_revived_within_one_sample_tick() {
    live_reading_revives(Path_::Sample).await;
}

// ── (b) paired: a reading that still needs a sign-in lifts nothing ──────────

/// Expired and Wiped, on both paths: `dead_since` stays, the login stays
/// unavailable, `login_revived` is never said, and the park clause names the
/// sign-in that WILL fix it (the positive half of (c)'s "no hint").
#[tokio::test]
async fn a_dead_login_whose_slot_still_needs_a_sign_in_stays_dead() {
    set_env();
    for health in [SlotHealth::Expired, SlotHealth::Wiped] {
        for path in [Path_::Recheck, Path_::Sample] {
            let tmp = tempfile::tempdir().unwrap();
            let m = build_manager(config_at(tmp.path(), &[DEAD, GOOD])).unwrap();
            let (slots, reads) = probe(&m);
            slots.lock().unwrap().insert(DEAD.into(), health.clone());
            kill(&m, DEAD, DIED);
            let mut rx = m.subscribe();

            tick(&m, path);

            let case = format!("{health:?} via {path:?}");
            assert!(reads_of(&reads, DEAD) >= 1, "{case}: the dead slot was read");
            assert_eq!(dead_since(&m, DEAD).as_deref(), Some(DIED), "{case}: dead_since stays");
            let why = m.account_unavailable_reasons(DEAD).unwrap_or_default();
            assert!(why.iter().any(|w| w == "login_dead"), "{case}: still login_dead: {why:?}");
            assert!(
                !announced(&mut rx, DEAD).iter().any(|r| r == "login_revived"),
                "{case}: nothing revived"
            );
            let note = m.sign_in_note();
            assert!(
                note.contains(&format!("`claude-accounts login {DEAD}`"))
                    && note.contains("returns within one recheck/sample"),
                "{case}: the park clause names the sign-in that fixes it: {note}"
            );
        }
    }
}

/// **A Live reading that STARTED before the login was marked dead lifts
/// nothing.** The auth failure that wrote `dead_since` is newer evidence than a
/// keychain read already in flight. Modelled by a death stamped after "now".
#[tokio::test]
async fn a_live_reading_older_than_the_death_lifts_nothing() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &[DEAD, GOOD])).unwrap();
    let (_slots, reads) = probe(&m);
    let later = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    kill(&m, DEAD, &later);

    tick(&m, Path_::Recheck);
    tick(&m, Path_::Sample);

    assert!(reads_of(&reads, DEAD) >= 2, "the dead slot was read on both paths");
    assert_eq!(dead_since(&m, DEAD), Some(later), "a stale Live reading lifts nothing");
}

/// **The recheck reads a dead slot once per period, not once per tick**
/// (doctrine 4: the period outlasted is the injectable `SIGN_IN_RECHECK`,
/// shrunk to 50 ms; the default 30 s is outlasted by not sleeping at all).
#[tokio::test]
async fn the_recheck_reads_a_dead_slot_once_per_30s_period() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &[DEAD, GOOD])).unwrap();
    let (slots, reads) = probe(&m);
    // A dead login whose slot reads Live but whose reading is stale keeps it
    // dead and NOT in the sign-in set, so only the dead-login read reaches it.
    slots.lock().unwrap().insert(DEAD.into(), SlotHealth::Live);
    let later = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    kill(&m, DEAD, &later);

    tick(&m, Path_::Recheck);
    tick(&m, Path_::Recheck);
    assert_eq!(reads_of(&reads, DEAD), 1, "two ticks inside one period: one read");

    m.set_sign_in_recheck_for_test(Duration::from_millis(50));
    tokio::time::sleep(Duration::from_millis(120)).await;
    tick(&m, Path_::Recheck);
    assert_eq!(reads_of(&reads, DEAD), 2, "past the period: read again");
}

// ── (b2) R9.A3b: a RELAPSING login is read at most once per period ─────────

/// Revive DEAD on one recheck, let one more tick pass while it is alive, then
/// re-kill it (the next turn 401s: Live store, server still rejecting).
fn revive_then_relapse(m: &ThreadManager, reads: &Reads) {
    kill(m, DEAD, DIED);
    tick(m, Path_::Recheck);
    assert_eq!(dead_since(m, DEAD), None, "fixture: revived on the first recheck");
    assert_eq!(reads_of(reads, DEAD), 1, "fixture: one read so far");
    tick(m, Path_::Recheck); // a resume tick while it is alive
    std::thread::sleep(Duration::from_millis(5));
    kill(m, DEAD, &chrono::Utc::now().to_rfc3339());
    std::thread::sleep(Duration::from_millis(5));
}

/// **The 30 s bound holds across a revival (inspect R9.A3 F1, ledger R9.A3-1).**
/// The default 30 s `SIGN_IN_RECHECK` is outlasted by not sleeping at all
/// (doctrine 4): every tick here lands inside one period, so the re-killed
/// login is NOT read again and stays dead.
///
/// FORCED RED (R9.A3b report): with the recheck's memory dropped on revival
/// (`seen.retain(|l, _| dead.contains(l))`), reads = 2 and it is revived again.
#[tokio::test]
async fn a_relapsed_login_is_not_reread_inside_one_30s_period_after_an_alive_tick() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &[DEAD, GOOD])).unwrap();
    let (_slots, reads) = probe(&m);
    revive_then_relapse(&m, &reads);

    tick(&m, Path_::Recheck);
    tick(&m, Path_::Recheck);

    assert_eq!(reads_of(&reads, DEAD), 1, "inside one 30 s period: read once, revival included");
    assert!(dead_since(&m, DEAD).is_some(), "not re-read, so not revived again");
}

/// **Paired: once the period has passed, the relapsed login IS read again**
/// (and its Live reading revives it), so "never re-read" cannot pass the
/// guard above. Period shrunk to 50 ms and outlasted with 120 ms.
#[tokio::test]
async fn a_relapsed_login_is_reread_once_its_period_has_passed() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &[DEAD, GOOD])).unwrap();
    m.set_sign_in_recheck_for_test(Duration::from_millis(50));
    let (_slots, reads) = probe(&m);
    revive_then_relapse(&m, &reads);
    tokio::time::sleep(Duration::from_millis(120)).await;

    tick(&m, Path_::Recheck);

    assert_eq!(reads_of(&reads, DEAD), 2, "past the period: read again");
    assert_eq!(dead_since(&m, DEAD), None, "and revived again");
}

// ── (c) CON-28: a DISABLED dead login is never read and never revived ───────

/// `cc2` disabled, carrying `dead_since`, its fixture slot reading Live: one
/// recheck and one sample read it 0 times (and DO read the enabled slot), it
/// stays unavailable, and the park clause gives it no login hint.
///
/// FORCED RED (recorded in the R9.A3 report): with the disabled skip in
/// `probe_slot` removed, `cc2`'s read count is non-zero.
#[tokio::test]
async fn a_disabled_dead_login_is_never_read_revived_or_hinted() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build_manager(config_at(tmp.path(), &[CON28, GOOD])).unwrap();
    m.store
        .set_account_disabled(CON28, true, Some("unpaid (CON-28)"))
        .unwrap();
    let (_slots, reads) = probe(&m);
    kill(&m, CON28, DIED);
    // R9.A3b (inspect F2): GOOD is dead too, so the positive sentinel runs on
    // the SAME path as the ban — the recheck's dead-login read, alone.
    kill(&m, GOOD, DIED);

    tick(&m, Path_::Recheck);
    assert_eq!(reads_of(&reads, CON28), 0, "the recheck never reads a disabled dead slot");
    assert_eq!(
        reads_of(&reads, GOOD),
        1,
        "the recheck alone DOES read the enabled dead slot in the same tick"
    );
    assert_eq!(dead_since(&m, GOOD), None, "and its Live reading revives it");

    tick(&m, Path_::Sample);

    assert_eq!(reads_of(&reads, CON28), 0, "a disabled slot is never read");
    assert!(reads_of(&reads, GOOD) >= 2, "the enabled slot in the sample tick IS read");
    assert_eq!(dead_since(&m, CON28).as_deref(), Some(DIED), "never revived");
    let why = m.account_unavailable_reasons(CON28).unwrap_or_default();
    assert!(
        why.iter().any(|w| w == "disabled"),
        "stays unavailable as disabled: {why:?}"
    );
    let note = m.sign_in_note();
    assert!(note.contains(&format!("{CON28}: not available")), "named: {note}");
    assert!(
        !note.contains("claude-accounts login"),
        "a disabled account gets no login hint: {note}"
    );
}
