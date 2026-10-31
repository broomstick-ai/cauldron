//! **R12.5c — A COMPLETED TURN LIFTS AN ACCOUNT'S LOGIN FLAGS ONLY THROUGH
//! R12.10's RE-CHECK.**
//!
//! `land_clean_verdict` (runtime.rs) used to treat every completed turn as
//! proof that the account's own login works: `note_login_health` revived
//! `dead_since` and `note_account_served` dropped the slot from the sign-in set
//! and cleared the login bar (`auth_failed`), all with no identity read. A slot
//! holding b@'s grant under a@'s label serves turns AS b@, so both clears fired
//! for a@ unread (R12.5b inspect, finding 1). Now, when the account carries a
//! login flag, the completion runs `accounts::recheck_flags(.., Turn)` first:
//! verified clears, anything else keeps.
//!
//! Every case drives a REAL turn (`tests/fixtures/fake-claude`, `holdturn`) on
//! one named account. The flags are written WHILE the turn runs (a flagged
//! account is never placed), so the turn's clean ending is the only completion
//! that can reach them. The identity server is `support/fake_login.rs`'s file
//! fake: no keychain, no network, no real slot.
//!
//! Doctrine 4: nothing here waits out a period. The sign-in reading is
//! `Expired` for the kept cases, so neither the 30 s recheck nor the sampler can
//! lift a flag behind the test's back, and the login bar is timed to 2099, so
//! only a completion or a re-check can lift it. Forcings: an internal report §5.

#[path = "support/fake_login.rs"]
mod fake_login;
#[path = "support/first_exec.rs"]
mod first_exec;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::accounts::SlotHealth;
use threads::budgets::Thresholds;
use threads::event::Push;
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

use fake_login::{hits, sign_in_verified, KC};

const DIED: &str = "2026-01-01T00:00:00Z";
const LATER: &str = "2099-01-01T00:00:00Z";

fn set_env() {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(first_exec::warmed("tests/fixtures/fake-claude"))
            .to_string_lossy()
            .to_string(),
    );
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

enum Slot {
    /// The slot's item authenticates as its recorded address.
    Verified,
    /// The slot's item is b@x.test's grant; its recorded address is still the label's.
    Crossed,
    /// The identity server cannot be reached.
    Unreadable,
}

struct World {
    tmp: tempfile::TempDir,
    m: ThreadManager,
    label: &'static str,
    reading: Arc<Mutex<SlotHealth>>,
    infos: Arc<Mutex<Vec<(String, String)>>>,
}

impl World {
    fn new(label: &'static str, slot: Slot) -> World {
        set_env();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("accounts").join(label);
        std::fs::create_dir_all(&dir).unwrap();
        let mut accounts = BTreeMap::new();
        accounts.insert(label.to_string(), dir.to_string_lossy().to_string());
        let m = build_manager(Config {
            data_dir: Some(tmp.path().to_string_lossy().to_string()),
            accounts,
            calibrate: false,
            edge_poll: false,
            probe_interval_secs: 0,
            ..Default::default()
        })
        .unwrap();
        let reading = Arc::new(Mutex::new(SlotHealth::Live));
        let r = reading.clone();
        m.set_slot_probe_for_test(Arc::new(move |_l: &str, _d: Option<&str>| r.lock().unwrap().clone()));
        sign_in_verified(&m, label, &format!("{label}@x.test"));
        let kc = tmp.path().join("accounts").join(KC);
        match slot {
            Slot::Verified => {}
            Slot::Crossed => std::fs::write(kc.join(fake_login::service_of(&dir)), "tok|b@x.test|x").unwrap(),
            Slot::Unreadable => std::fs::write(kc.join("OFFLINE"), "").unwrap(),
        }
        let infos: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let (out, mut rx) = (infos.clone(), m.subscribe());
        tokio::spawn(async move {
            while let Ok(p) = rx.recv().await {
                if let Push::Info { reason, account: Some(a), .. } = p {
                    out.lock().unwrap().push((reason, a));
                }
            }
        });
        World { tmp, m, label, reading, infos }
    }

    fn kc_home(&self) -> PathBuf {
        self.tmp.path().join("accounts")
    }
    fn words(&self) -> Vec<String> {
        self.m.account_unavailable_reasons(self.label).unwrap_or_default()
    }
    fn announced(&self, reason: &str) -> bool {
        self.infos.lock().unwrap().iter().any(|(r, a)| r == reason && a == self.label)
    }

    /// Start a held turn on the label; returns (thread id, release file).
    async fn hold_a_turn(&self) -> (String, PathBuf) {
        let mut s = ThreadSettings::new(self.tmp.path().to_string_lossy().to_string(), "fable".to_string());
        s.injection = threads::settings::Injection::NOTHING;
        s.account = threads::settings::AccountChoice::Named(self.label.into());
        s.thresholds = Thresholds::ungated();
        let id = self.m.create_thread(None, s).unwrap();
        let release = self.tmp.path().join("release");
        self.m
            .send(
                &id,
                format!("holdturn release={}", release.display()),
                threads::SendIntent::Immediate,
                serde_json::json!({}),
                false,
            )
            .unwrap();
        let m = self.m.clone();
        let idc = id.clone();
        assert!(
            eventually(move || m.store.get_running_turn(&idc).unwrap().is_some()).await,
            "the held turn runs on {}",
            self.label
        );
        (id, release)
    }

    /// The flags a login fault leaves, written while the turn runs: a dead
    /// login, a login bar (`auth_failed`, timed to 2099) and an EXPIRED reading
    /// in the sign-in set.
    fn flag_it(&self) {
        self.flag_with_bar("login");
    }
    /// `axis`: `login` (the login bar, `auth_failed`) or a quota axis
    /// (`weekly`), which only the completion's own clear can lift.
    fn flag_with_bar(&self, axis: &str) {
        let l = self.label;
        self.m.store.set_account_health(l, Some((DIED, "OAuth token has expired"))).unwrap();
        self.m.store.set_account_limit(l, Some((LATER, Some("401"), Some(axis), None))).unwrap();
        *self.reading.lock().unwrap() = SlotHealth::Expired;
        self.m.seed_sign_in_needed();
        let w = self.words();
        let bar = if axis == "login" { "auth_failed" } else { "login_dead" };
        for want in ["login_dead", bar, "login_expired"] {
            assert!(w.contains(&want.to_string()), "fixture: {l} carries {want}: {w:?}");
        }
    }

    /// Release the turn and wait until its completion has been landed (the
    /// running-turn row is dropped in `finish_turn`, after `land_clean_verdict`).
    async fn complete(&self, id: &str, release: &Path) {
        std::fs::write(release, "").unwrap();
        let m = self.m.clone();
        let idc = id.to_string();
        assert!(
            eventually(move || m.store.get_running_turn(&idc).unwrap().is_none()).await,
            "the turn completed"
        );
    }
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..800 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

/// **A turn that completes on a CROSSED slot clears nothing.** The slot holds
/// b@'s grant under `tx`'s label: before R12.5c the clean ending revived
/// `login_dead`, dropped `login_expired` from the sign-in set and lifted the
/// login bar, on b@'s success.
///
/// **Forced red** (report §5 T1): `completed_turn_may_lift_flags` returning
/// `true` without the re-check (the pre-R12.5c path).
#[tokio::test]
async fn t1_a_turn_completed_on_a_crossed_slot_keeps_every_login_flag() {
    let w = World::new("tx", Slot::Crossed);
    let (id, release) = w.hold_a_turn().await;
    w.flag_it();
    let asked = hits(&w.kc_home());

    w.complete(&id, &release).await;

    let words = w.words();
    for kept in ["login_dead", "auth_failed", "login_expired"] {
        assert!(words.contains(&kept.to_string()), "{kept} survives a crossed turn: {words:?}");
    }
    assert!(hits(&w.kc_home()) > asked, "the completion ASKED the server (hits {asked})");
    assert!(w.announced("flags_kept"), "the re-check said why it kept: {:?}", w.infos.lock().unwrap());
}

/// **Unreadable is not a pass.** Same turn, the server cannot be reached.
#[tokio::test]
async fn t2_a_turn_completed_while_the_identity_is_unreadable_keeps_every_login_flag() {
    let w = World::new("tu", Slot::Unreadable);
    let (id, release) = w.hold_a_turn().await;
    w.flag_it();

    w.complete(&id, &release).await;

    let words = w.words();
    for kept in ["login_dead", "auth_failed", "login_expired"] {
        assert!(words.contains(&kept.to_string()), "{kept} survives an unreadable turn: {words:?}");
    }
    assert!(w.announced("flags_kept"), "{:?}", w.infos.lock().unwrap());
}

/// **Opposition: a turn on a VERIFIED slot still clears them.** The slot reads
/// Live again by the time the turn ends; the login bar (2099) can only be
/// lifted by the completion, so a constant "keep" fails here.
///
/// **Forced red** (report §5 T3k): the constant-keep completion (no re-check,
/// no clear) leaves the 2099 login bar.
#[tokio::test]
async fn t3_a_turn_completed_on_a_verified_slot_clears_the_login_flags() {
    let w = World::new("tv", Slot::Verified);
    let (id, release) = w.hold_a_turn().await;
    w.flag_it();
    *w.reading.lock().unwrap() = SlotHealth::Live;
    let asked = hits(&w.kc_home());

    w.complete(&id, &release).await;

    assert!(hits(&w.kc_home()) > asked, "the completion asked the server");
    assert_eq!(w.words(), Vec::<String>::new(), "a verified completed turn leaves no login flag");
}

/// **No flag, no read.** A completed turn on an unflagged account does not ask
/// the server — the re-check is not a per-turn network call.
///
/// **Forced red** (report §5 T4): the re-check run on every completion.
#[tokio::test]
async fn t4_a_turn_completed_on_an_unflagged_account_does_not_ask_the_server() {
    let w = World::new("tn", Slot::Verified);
    let (id, release) = w.hold_a_turn().await;
    let asked = hits(&w.kc_home());

    w.complete(&id, &release).await;

    assert_eq!(hits(&w.kc_home()), asked, "no login flag ⇒ no identity read");
    assert_eq!(w.words(), Vec::<String>::new());
}

/// **The verdict, not only the re-check, decides the completion's own clears.**
/// A verified re-check lifts the login flags itself; what it leaves to the
/// completion is a QUOTA window (`weekly`, 2099) — falsified by a turn that
/// served on this account's own credential (A-4), so it must still clear.
///
/// **Forced red** (report §5 T3): the gate returning `false` after the
/// re-check — the login flags still clear (t3 stays green under it), the
/// quota window does not.
#[tokio::test]
async fn t5_a_verified_completion_still_falsifies_a_quota_window() {
    let w = World::new("tw", Slot::Verified);
    let (id, release) = w.hold_a_turn().await;
    w.flag_with_bar("weekly");
    let row = w.m.store.list_account_states().unwrap().into_iter().find(|s| s.name == "tw").unwrap();
    assert_eq!(row.limit_until.as_deref(), Some(LATER), "fixture: the quota window is recorded");
    *w.reading.lock().unwrap() = SlotHealth::Live;

    w.complete(&id, &release).await;

    assert_eq!(w.words(), Vec::<String>::new(), "a verified completed turn leaves no login flag");
    // A usage window is not an availability WORD (it is a window, not a
    // fault), so it is read off the row itself.
    let row = w.m.store.list_account_states().unwrap().into_iter().find(|s| s.name == "tw").unwrap();
    assert_eq!(row.limit_until, None, "the completion falsified the quota window: {row:?}");
}
