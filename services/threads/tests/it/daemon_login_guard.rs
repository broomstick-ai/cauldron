//! **R12.5b — THE DAEMON'S OWN LOGIN PATH GETS THE CROSSED-LOGIN GUARD, AND
//! EVERY DAEMON DOOR THAT CLEARS A LOGIN FLAG GOES THROUGH THE RE-CHECK.**
//!
//! `accounts/login/begin` spawns `add-account` directly — never through
//! `claude-accounts login` — so R12.5's script guard never ran on it, and
//! `accounts/login/{id}/complete` then cleared the sign-in set, the login bar
//! and `dead_since` on `add-account`'s exit status alone. So did the
//! `accounts/{account}/login_restored` arm, which any WS client may call.
//!
//! Every case here goes through the REAL WS server (`chassis::serve`) and the
//! REAL arms, with `tests/fixtures/fake-add-account` as the script. The
//! keychain is a directory of files and the identity server reads the email out
//! of the slot's item (`tests/support/fake_login.rs`): no real keychain, no
//! real `~/.claude.json`, no network, no real slot.
//!
//! Forcings and their red runs: an internal report §5.

#[path = "support/fake_login.rs"]
mod fake_login;
#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use threads::accounts::SlotHealth;
use threads::{build_manager, Config, ThreadManager};

use fake_login::{hits, items_with_prefix, keychain_at, land, service_of, KC};

const DIED: &str = "2026-01-01T00:00:00Z";
const LATER: &str = "2099-01-01T00:00:00Z";

struct World {
    tmp: tempfile::TempDir,
    m: ThreadManager,
    addr: SocketAddr,
    /// Per label, the slot item that is the DEAD credential (R12.5c): while the
    /// slot holds exactly these bytes its probe reads `Expired`, otherwise
    /// `Live`. See [`World::dead_credential`].
    dead: Arc<std::sync::Mutex<BTreeMap<String, Vec<u8>>>>,
}

impl World {
    /// `accounts`: `(label, Some(address it is signed in as now))`.
    async fn new(accounts: &[(&str, Option<&str>)]) -> World {
        keychain::install();
        fake_login::install_identity_seam();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let kc = keychain_at(home);
        std::fs::create_dir_all(home.join("scripts")).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-add-account"),
            home.join("scripts/add-account"),
        )
        .unwrap();
        let mut map = BTreeMap::new();
        for (label, now) in accounts {
            let slot = home.join("accounts").join(label);
            std::fs::create_dir_all(&slot).unwrap();
            if let Some(email) = now {
                land(home, &slot, email, &format!("old-{label}"));
            }
            map.insert(label.to_string(), slot.to_string_lossy().to_string());
        }
        let m = build_manager(Config {
            data_dir: Some(home.to_string_lossy().to_string()),
            accounts: map,
            calibrate: false,
            edge_poll: false,
            probe_interval_secs: 0,
            ..Default::default()
        })
        .unwrap();
        // R12.5c (de-flake g1/g4): the probe reads the slot's ITEM. It used to
        // read `Live` for every slot, so the dead login `stale_flags` writes sat
        // on a slot the probe called healthy — and any placement pass
        // (`notice_expired_limits` -> `recheck_sign_in_needed`, R9.A3: a DEAD
        // login is read at once, then every 30 s) revived it in the background.
        // Under load that pass landed between the login and the assertion.
        let dead: Arc<std::sync::Mutex<BTreeMap<String, Vec<u8>>>> = Arc::default();
        let (d, kc_dir) = (dead.clone(), home.join(KC));
        m.set_slot_probe_for_test(Arc::new(move |l: &str, dir: Option<&str>| {
            let item = dir.and_then(|dir| std::fs::read(kc_dir.join(service_of(Path::new(dir)))).ok());
            match (d.lock().unwrap().get(l), item) {
                (Some(dead), Some(now)) if *dead == now => SlotHealth::Expired,
                _ => SlotHealth::Live,
            }
        }));
        m.logins.set_keychain_for_test(kc);
        let (addr, h) = threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        std::mem::forget(h);
        World { tmp, m, addr, dead }
    }

    fn home(&self) -> &Path {
        self.tmp.path()
    }
    fn slot(&self, label: &str) -> PathBuf {
        self.home().join("accounts").join(label)
    }
    /// The slot's keychain item, as bytes (the fake's file).
    fn item(&self, label: &str) -> Option<Vec<u8>> {
        std::fs::read(self.home().join(KC).join(service_of(&self.slot(label)))).ok()
    }
    fn file(&self, label: &str, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.slot(label).join(name)).ok()
    }
    /// Everything a revert must put back, byte for byte.
    fn picture(&self, label: &str) -> Vec<Option<Vec<u8>>> {
        vec![
            self.item(label),
            self.file(label, ".claude.json"),
            self.file(label, "login-hint"),
            self.file(label, "manifest.toml"),
        ]
    }
    fn words(&self, label: &str) -> Vec<String> {
        self.m.account_unavailable_reasons(label).unwrap_or_default()
    }
    /// A dead login and a live login bar, both written long ago.
    fn stale_flags(&self, label: &str) {
        self.m.store.set_account_health(label, Some((DIED, "OAuth token has expired"))).unwrap();
        self.m
            .store
            .set_account_limit(label, Some((LATER, Some("401"), Some("login"), None)))
            .unwrap();
        let w = self.words(label);
        assert!(
            w.contains(&"login_dead".to_string()) && w.contains(&"auth_failed".to_string()),
            "fixture: {label} is dead and login-barred: {w:?}"
        );
    }
    /// The credential in `label`'s slot NOW is the one that died: its probe
    /// reads `Expired` for as long as the slot holds it (R12.5c). So the
    /// background recheck can never revive the login on this credential, and a
    /// revert that puts it back keeps it dead — as the real slot would.
    fn dead_credential(&self, label: &str) {
        let item = self.item(label).expect("fixture: the slot holds a credential");
        self.dead.lock().unwrap().insert(label.to_string(), item);
    }
    fn offline(&self) {
        std::fs::write(self.home().join(KC).join("OFFLINE"), "").unwrap();
    }

    /// One WS request through the real server; the success payload.
    async fn call(&self, method: &str, params: Value) -> Value {
        let (ws, _) = connect_async(format!("ws://{}/ws", self.addr)).await.unwrap();
        let (mut wr, mut rd) = ws.split();
        wr.send(Message::Text(
            json!({"type":"request","id":"r1","method":method,"params":params}).to_string(),
        ))
        .await
        .unwrap();
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(60), rd.next())
                .await
                .expect("timed out waiting for the response")
                .expect("socket closed")
                .expect("socket error");
            let Message::Text(t) = msg else { continue };
            let f: Value = serde_json::from_str(&t).unwrap();
            if f["type"] == "response" && f["correlate"] == "r1" {
                assert_eq!(f["outcome"]["type"], "success", "{method}: {f}");
                return f["outcome"]["payload"].clone();
            }
        }
    }

    /// The daemon's own login, both halves, as the interface drives it.
    async fn daemon_login(&self, account: &str, email: Option<&str>) -> Value {
        let mut p = json!({ "account": account });
        if let Some(e) = email {
            p["email"] = json!(e);
        }
        let begun = self.call("accounts/login/begin", p).await;
        let sid = begun["session_id"].as_str().expect("a session id").to_string();
        self.call(&format!("accounts/login/{sid}/complete"), json!({ "code": "the-code#the-state" }))
            .await
    }
}

fn refused(out: &Value) -> String {
    out["refused"].as_str().unwrap_or("").to_string()
}

// ── G1: a CROSSED daemon login is refused and reverted ──────────────────────

/// The consent page for `cross-a` (signed in as a@x.test, flagged dead and
/// login-barred) is answered as b@x.test. The server — not the file — says b@.
///
/// **Forced red** (report §5 F1): `judge` returning `Verified` without the read
/// (today's path: the script's exit 0 is the verdict) ⇒ `ok: true`, b@'s grant
/// stays in a@'s slot, and the stale flags are cleared unread.
#[tokio::test]
async fn g1_a_crossed_daemon_login_is_refused_and_reverted_byte_for_byte() {
    let w = World::new(&[("cross-a", Some("a@x.test"))]).await;
    w.stale_flags("cross-a");
    w.dead_credential("cross-a");
    let before = w.picture("cross-a");
    let asked = hits(w.home());

    let out = w.daemon_login("cross-a", Some("a@x.test")).await;

    assert_eq!(out["ok"], false, "{out}");
    assert!(refused(&out).contains("wanted a@x.test, got b@x.test"), "{out}");
    assert!(hits(w.home()) > asked, "the SERVER was asked (hits {asked} -> {})", hits(w.home()));
    assert_eq!(w.picture("cross-a"), before, "the slot's item and files are back, byte for byte");
    let retired = items_with_prefix(w.home(), "Claude Code-retired.");
    assert_eq!(retired.len(), 1, "b@'s grant is PARKED, not destroyed: {retired:?}");
    let parked = std::fs::read_to_string(w.home().join(KC).join(&retired[0])).unwrap();
    assert!(parked.starts_with("tok|b@x.test|"), "the parked item is b@'s grant");
    assert!(items_with_prefix(w.home(), "Claude Code-prelogin").is_empty(), "no scratch left");
    assert!(out["recheck"].is_null(), "a refused login re-checks (and clears) nothing: {out}");
    // R12.5c: the background recheck this assertion used to race, run NOW.
    w.m.notice_expired_limits();
    let words = w.words("cross-a");
    assert!(
        words.contains(&"login_dead".to_string()) && words.contains(&"auth_failed".to_string()),
        "the stale flags stay: {words:?}"
    );
}

// ── G2 (opposition to G1): a MATCHING login passes; its last act re-checks ──

/// **Forced red** (report §5 F2): `judge` refusing everything (constant
/// refuse) ⇒ `ok: false` here. F3: the re-check skipped after a passed login ⇒
/// the stale flags survive.
#[tokio::test]
async fn g2_a_matching_daemon_login_passes_and_its_last_act_clears_the_stale_flags() {
    let w = World::new(&[("a2", Some("a2@x.test"))]).await;
    w.stale_flags("a2");
    let old_item = w.item("a2");

    let out = w.daemon_login("a2", Some("a2@x.test")).await;

    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["identity"], "a2@x.test", "{out}");
    assert!(out["refused"].is_null(), "{out}");
    assert_ne!(w.item("a2"), old_item, "the new grant is in the slot");
    assert!(items_with_prefix(w.home(), "Claude Code-retired.").is_empty());
    assert!(items_with_prefix(w.home(), "Claude Code-prelogin").is_empty(), "scratch dropped");
    assert_eq!(out["recheck"]["identity_state"], "verified", "{out}");
    assert_eq!(w.words("a2"), Vec::<String>::new(), "no stale flag survives a verified login");
    assert_eq!(out["reopened"], json!(["a2"]), "{out}");
}

// ── G3: unreadable ⇒ refused and reverted, never a pass ─────────────────────

/// **Forced red** (report §5 F4): an unreadable answer taken as a pass ⇒
/// `ok: true`.
#[tokio::test]
async fn g3_an_unreadable_identity_is_refused_and_reverted() {
    let w = World::new(&[("a3", Some("a3@x.test"))]).await;
    let before = w.picture("a3");
    w.offline();

    let out = w.daemon_login("a3", Some("a3@x.test")).await;

    assert_eq!(out["ok"], false, "{out}");
    assert!(refused(&out).contains("wanted a3@x.test, got <unreadable"), "{out}");
    assert_eq!(w.picture("a3"), before, "reverted byte for byte");
}

// ── G4: exit 0 with nothing landed where it is checked ⇒ refused ────────────

#[tokio::test]
async fn g4_a_login_that_lands_nowhere_checked_is_refused_not_passed() {
    let w = World::new(&[("elsewhere-a", Some("ea@x.test"))]).await;
    w.stale_flags("elsewhere-a");
    w.dead_credential("elsewhere-a");
    let before = w.picture("elsewhere-a");

    let out = w.daemon_login("elsewhere-a", Some("ea@x.test")).await;

    assert_eq!(out["exit_code"], 0, "the script claimed success: {out}");
    assert_eq!(out["ok"], false, "{out}");
    assert!(refused(&out).contains("landed where it was not checked"), "{out}");
    assert_eq!(w.picture("elsewhere-a"), before);
    // R12.5c: the background recheck this assertion used to race, run NOW.
    w.m.notice_expired_limits();
    assert!(w.words("elsewhere-a").contains(&"login_dead".to_string()), "{:?}", w.words("elsewhere-a"));
}

// ── G5: a crossed credential written before a FAILED exit is still reverted ─

#[tokio::test]
async fn g5_a_crossed_credential_landed_before_a_non_zero_exit_is_still_reverted() {
    let w = World::new(&[("failafter-a", Some("fa@x.test"))]).await;
    let before = w.picture("failafter-a");

    let out = w.daemon_login("failafter-a", Some("fa@x.test")).await;

    assert_eq!(out["exit_code"], 1, "{out}");
    assert_eq!(out["ok"], false, "{out}");
    assert!(refused(&out).contains("wanted fa@x.test, got b@x.test"), "{out}");
    assert_eq!(w.picture("failafter-a"), before, "reverted although the script failed");
}

// ── G6 (N ≥ 2): two logins in flight at once, one crossed, one matching ─────

/// Two snapshots are alive together (two parked copies), so the guard must
/// keep them apart: the crossed login on `cross-a` is reverted to a@'s grant
/// and NOT to bb's, and `bb`'s login passes with its own grant.
#[tokio::test]
async fn g6_two_accounts_in_flight_a_crossing_on_one_never_touches_the_other() {
    let w = World::new(&[("cross-a", Some("a@x.test")), ("bb", Some("bb@x.test"))]).await;
    let a_before = w.picture("cross-a");
    let b_old = w.item("bb");

    let a = w.call("accounts/login/begin", json!({"account":"cross-a","email":"a@x.test"})).await;
    let b = w.call("accounts/login/begin", json!({"account":"bb","email":"bb@x.test"})).await;
    assert_eq!(
        items_with_prefix(w.home(), "Claude Code-prelogin").len(),
        2,
        "both previous grants are parked, under two names"
    );
    let code = json!({ "code": "c#s" });
    let a_out = w
        .call(&format!("accounts/login/{}/complete", a["session_id"].as_str().unwrap()), code.clone())
        .await;
    assert_eq!(w.item("bb"), b_old, "the crossing on cross-a left bb's slot alone");
    let b_out = w
        .call(&format!("accounts/login/{}/complete", b["session_id"].as_str().unwrap()), code)
        .await;

    assert_eq!(a_out["ok"], false, "{a_out}");
    assert!(refused(&a_out).contains("wanted a@x.test, got b@x.test"), "{a_out}");
    assert_eq!(w.picture("cross-a"), a_before, "cross-a is back to a@'s own grant");
    assert_eq!(b_out["ok"], true, "{b_out}");
    assert_eq!(b_out["identity"], "bb@x.test");
    assert_ne!(w.item("bb"), b_old, "bb holds its new grant");
    assert!(items_with_prefix(w.home(), "Claude Code-prelogin").is_empty(), "both settled");
}

// ── F: the `login_restored` arm clears only through the re-check ────────────

/// The second door (R12.10 inspect (e)): any WS client could report a login
/// and have every flag cleared on its word.
///
/// **Forced red** (report §5 F5): `note_login_restored_checked` skipping the
/// re-check (the bypass) ⇒ the crossed case below clears `login_dead` and the
/// bar unread.
#[tokio::test]
async fn f1_a_reported_login_on_a_verified_slot_clears_the_stale_flags() {
    let w = World::new(&[("aa", Some("aa@x.test"))]).await;
    w.stale_flags("aa");
    let asked = hits(w.home());

    let out = w.call("accounts/aa/login_restored", json!({})).await;

    assert!(hits(w.home()) > asked, "the re-check asked the server");
    assert_eq!(out["recheck"]["identity_state"], "verified", "{out}");
    assert_eq!(out["reopened"], json!(["aa"]), "{out}");
    assert_eq!(w.words("aa"), Vec::<String>::new());
}

#[tokio::test]
async fn f2_a_reported_login_on_a_crossed_slot_clears_nothing() {
    let w = World::new(&[("aa", Some("aa@x.test")), ("zz", Some("zz@x.test"))]).await;
    w.stale_flags("aa");
    w.stale_flags("zz");
    // aa's slot holds b@'s grant; its recorded address is still aa@.
    std::fs::write(w.home().join(KC).join(service_of(&w.slot("aa"))), "tok|b@x.test|x").unwrap();

    let out = w.call("accounts/aa/login_restored", json!({})).await;

    assert_eq!(out["recheck"]["identity_state"], "mismatch", "{out}");
    assert_eq!(out["reopened"], json!([]), "{out}");
    let words = w.words("aa");
    assert!(
        words.contains(&"login_dead".to_string()) && words.contains(&"auth_failed".to_string()),
        "kept: {words:?}"
    );
    // N ≥ 2: the other account's flags are not this report's to clear either.
    assert!(w.words("zz").contains(&"auth_failed".to_string()), "{:?}", w.words("zz"));
}

#[tokio::test]
async fn f3_a_reported_login_that_cannot_be_read_clears_nothing() {
    let w = World::new(&[("aa", Some("aa@x.test"))]).await;
    w.stale_flags("aa");
    w.offline();

    let out = w.call("accounts/aa/login_restored", json!({})).await;

    assert_eq!(out["recheck"]["identity_state"], "unreadable", "{out}");
    assert_eq!(out["reopened"], json!([]), "{out}");
    assert!(w.words("aa").contains(&"login_dead".to_string()), "{:?}", w.words("aa"));
}
