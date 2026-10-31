//! **R9.A2d (A-2, CON-28; A2c inspect-2 F5): THE TWO DELETE DOORS THEMSELVES
//! DELETE A DISABLED ACCOUNT, AND THE WS DOOR READS NOTHING OF IT DOING SO.**
//!
//! `disabled_slot_never_read.rs`'s delete guards call `accounts::delete_lookup`
//! directly, so an ARM that stops using it (reverted to the pre-r2
//! `collect_slot_reports` lookup, which has no row for a disabled slot and so
//! answers "there is no account") left the whole suite green — measured by
//! the A2c inspector (`forceArm2`). These guards go through the doors:
//!
//! 1. **WS** — `accounts/{account}/delete` over a real `chassis::serve`
//!    socket, with the process-wide FIXTURE inventory reader installed and its
//!    reads counted per keychain service. A disabled account is deleted
//!    (success, slot dir gone) and its store is read 0 times; the enabled
//!    account's store IS read by the same door (so the zero is the disable's,
//!    not a door that reads nothing); a name that is no account is still
//!    refused (so the door is not a constant "yes").
//! 2. **CLI** — `threads accounts delete <name>`, the REAL binary against a
//!    fixture data dir (`--config`), a scratch `HOME` and a fixture `USER`.
//!    A disabled account is deleted (exit 0, slot dir gone); a missing one is
//!    refused (exit 2, "there is no account"); an enabled one is deleted.
//!    **What the CLI guard cannot see: the READ COUNT.** The binary has no
//!    keychain seam (`set_keychain_reader_for_test` is in-process only, and the
//!    production reader is `/usr/bin/security` by absolute path), so a CLI arm
//!    that read the disabled store but still deleted it would pass here. Both
//!    arms share `delete_lookup`, whose zero-read property the WS guard and
//!    `disabled_slot_never_read.rs` assert in-process.
//!
//! **Keychain hygiene.** Every slot is a temp dir, so its keychain service is
//! `Claude Code-credentials-<hash of a temp path>` — never one of the
//! operator's. `USER` is set to a fixture name, so the keychain ACCOUNT every
//! `security` call names (`keychain_account()`) matches no real item: the
//! WS/CLI `delete_account(.., Auto)` existence check, and the CLI's inventory
//! read of the machine-default service, find nothing and read no secret.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::accounts::{credential_facts_from_blob, keychain_service_for, CredentialFacts};
use threads::{build_manager, Config, ThreadManager};
use tokio_tungstenite::tungstenite::Message;

/// The CON-28 population, as fixtures.
const CON28: [&str; 2] = ["cc2", "owner-account"];
const GOOD: &str = "zz-good";
/// A keychain account name no real item is stored under.
const FIXTURE_USER: &str = "threads-a2d-fixture-user";

// ── fixtures ────────────────────────────────────────────────────────────────

fn set_env() {
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude");
    std::env::set_var("THREADS_CLAUDE_BIN", fake);
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
    std::env::set_var("HOME", scratch_home());
    std::env::set_var("USER", FIXTURE_USER);
    install_inventory_reader();
}

fn scratch_home() -> PathBuf {
    let home = std::env::temp_dir().join("threads-tests-a2d-home");
    std::fs::create_dir_all(&home).unwrap();
    home
}

fn slot_dir(dir: &Path, name: &str) -> PathBuf {
    dir.join("accounts").join(name)
}

fn make_slots(dir: &Path, names: &[&str]) -> BTreeMap<String, String> {
    let mut accounts = BTreeMap::new();
    for name in names {
        let cd = slot_dir(dir, name);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join(".claude.json"), "{}").unwrap();
        accounts.insert(name.to_string(), cd.to_string_lossy().to_string());
    }
    accounts
}

fn facts(token: &str) -> CredentialFacts {
    static NOW: OnceLock<i64> = OnceLock::new();
    let now = *NOW.get_or_init(|| chrono::Utc::now().timestamp_millis());
    let f = credential_facts_from_blob(
        &json!({"claudeAiOauth": {
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

/// The process-wide fixture keychain the inventory reads, every read counted
/// per service.
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
    // R9.F2h: the doors' existence check asks the same fixture keychain, and
    // is NOT counted — it is metadata, not a read of the store's facts, so the
    // "a disabled store is read 0 times" counts below keep their meaning.
    threads::accounts::set_keychain_entry_exists_for_test(Some(Arc::new(|service: &str, _: &str| {
        keychain().lock().unwrap().facts.contains_key(service)
    })));
}

fn service(dir: &Path, name: &str) -> String {
    keychain_service_for(Some(&slot_dir(dir, name).to_string_lossy()))
}

fn inv_reads(dir: &Path, name: &str) -> usize {
    keychain().lock().unwrap().reads.get(&service(dir, name)).copied().unwrap_or(0)
}

/// Pool = CON-28 + one enabled slot, the CON-28 pair disabled in the store.
fn con28_pool(dir: &Path) -> ThreadManager {
    let names = [CON28[0], CON28[1], GOOD];
    let m = build_manager(Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts: make_slots(dir, &names),
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();
    for d in CON28 {
        m.store.set_account_disabled(d, true, Some("unpaid (CON-28)")).unwrap();
    }
    let mut k = keychain().lock().unwrap();
    for (name, token) in [(CON28[0], "d0"), (CON28[1], "d1"), (GOOD, "own")] {
        k.facts.insert(service(dir, name), facts(token));
    }
    drop(k);
    m
}

/// One request on a fresh connection; the outcome object.
async fn ws_request(addr: std::net::SocketAddr, method: &str) -> Value {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    let _welcome = ws.next().await;
    ws.send(Message::Text(
        json!({"type": "request", "id": "a2d", "method": method, "params": {}}).to_string(),
    ))
    .await
    .unwrap();
    loop {
        let m = tokio::time::timeout(std::time::Duration::from_secs(20), ws.next())
            .await
            .expect("timed out waiting for the response")
            .expect("socket closed")
            .expect("socket error");
        if let Message::Text(t) = m {
            let f: Value = serde_json::from_str(&t).unwrap();
            if f["type"] == "response" && f["correlate"] == "a2d" {
                return f["outcome"].clone();
            }
        }
    }
}

// ── 1. the WS door ──────────────────────────────────────────────────────────

/// **`accounts/{account}/delete` over a real socket: each DISABLED account is
/// deleted (success, its slot dir gone) and its store is read 0 times; the
/// ENABLED account's store IS read by the same door; a missing name is
/// refused.**
///
/// Forced red (an internal report): the WS arm alone reverted to
/// the pre-r2 `collect_slot_reports` lookup → `there is no account "cc2"`.
#[tokio::test]
async fn ws_delete_arm_deletes_a_disabled_account_unread_and_reads_the_enabled_one() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = con28_pool(tmp.path());
    let (addr, _h) = threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    // Opposing a constant "yes": a name that is no account is refused.
    let out = ws_request(addr, "accounts/no-such/delete").await;
    assert_eq!(out["type"], "error", "a missing account was deleted: {out}");
    assert!(out.to_string().contains("there is no account"), "{out}");

    for d in CON28 {
        let good_before = inv_reads(tmp.path(), GOOD);
        let out = ws_request(addr, &format!("accounts/{d}/delete")).await;
        assert_eq!(out["type"], "success", "WS: deleting disabled {d} was refused: {out}");
        assert_eq!(out["payload"]["label"], d, "{out}");
        assert_eq!(out["payload"]["dir_removed"], true, "{out}");
        assert!(!slot_dir(tmp.path(), d).exists(), "{d}: slot dir still on disk");
        // The SAME request read the enabled slot: the door's lookup ran its
        // inventory, and skipped only the disabled one.
        assert!(
            inv_reads(tmp.path(), GOOD) > good_before,
            "WS delete of {d}: the lookup did not read the enabled slot"
        );
        for d2 in CON28 {
            assert_eq!(inv_reads(tmp.path(), d2), 0, "{d2}: WS delete read a disabled store");
        }
    }

    // The enabled account through the same door: read, and deleted.
    let good_before = inv_reads(tmp.path(), GOOD);
    let out = ws_request(addr, &format!("accounts/{GOOD}/delete")).await;
    assert_eq!(out["type"], "success", "WS: deleting enabled {GOOD}: {out}");
    assert!(inv_reads(tmp.path(), GOOD) > good_before, "the enabled slot's store was not read");
    assert!(!slot_dir(tmp.path(), GOOD).exists(), "{GOOD}: slot dir still on disk");
}

// ── 2. the CLI door ─────────────────────────────────────────────────────────

fn cli_delete(cfg: &Path, label: &str) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_threads"))
        .args(["accounts", "delete", label, "--config"])
        .arg(cfg)
        .env_remove("THREADS_CONFIG")
        .env("HOME", scratch_home())
        .env("USER", FIXTURE_USER)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// **`threads accounts delete <disabled>`, the real binary on a fixture data
/// dir: exit 0 and the slot dir is gone.** Paired: a missing name exits 2
/// with "there is no account", and the enabled account is deleted too.
///
/// Forced red (an internal report): the CLI arm alone
/// (`main.rs::accounts_delete`) reverted to the pre-r2 `collect_slot_reports`
/// lookup → exit 2, `there is no account "cc2"`.
#[test]
fn cli_delete_arm_deletes_a_disabled_account() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    make_slots(&data, &[CON28[0], CON28[1], GOOD]);
    {
        let store = threads::Store::open(data.join("threads.db")).unwrap();
        for d in CON28 {
            store.set_account_disabled(d, true, Some("unpaid (CON-28)")).unwrap();
        }
    }
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, format!("data_dir = {:?}\n", data.display().to_string())).unwrap();

    let (rc, _, err) = cli_delete(&cfg, "no-such");
    assert_eq!(rc, 2, "a missing account must be refused with exit 2: {err}");
    assert!(err.contains("there is no account"), "{err}");

    for d in CON28 {
        let (rc, out, err) = cli_delete(&cfg, d);
        assert_eq!(rc, 0, "CLI: deleting disabled {d} was refused: {err}");
        assert!(out.contains(&format!("deleted {d}")), "{out}");
        assert!(!slot_dir(&data, d).exists(), "{d}: slot dir still on disk");
    }

    let (rc, out, err) = cli_delete(&cfg, GOOD);
    assert_eq!(rc, 0, "CLI: deleting enabled {GOOD}: {err}");
    assert!(out.contains(&format!("deleted {GOOD}")), "{out}");
    assert!(!slot_dir(&data, GOOD).exists(), "{GOOD}: slot dir still on disk");
}
