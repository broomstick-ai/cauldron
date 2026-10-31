//! # A fake keychain and a fake identity server, both on disk (R12.5b)
//!
//! The daemon's login guard snapshots, judges and reverts a slot through
//! [`threads::login::SlotKeychain`] and asks R12.1's `IdentityGuard` who the
//! slot's credential authenticates as. A test must touch neither the real
//! keychain nor the network, and the CHILD process (`tests/fixtures/fake-add-account`)
//! has to be able to "land" a credential the daemon then reads. So both fakes
//! live in one directory, `<home>/fake-keychain/`:
//!
//! * one FILE per keychain item, named by its service, holding
//!   `tok|<email>|<nonce>` — the fixture writes it exactly as `claude` would
//!   write the slot's item;
//! * the server answers with the `<email>` inside the item the slot DERIVES
//!   (`keychain_service_for(<slot dir>)`), so it answers for the credential,
//!   never for `.claude.json` — and a revert that puts the old item back
//!   changes its answer, as the real server's would;
//! * `<home>/fake-keychain/OFFLINE` makes every read fail (unreadable);
//! * every read appends a line to `<home>/fake-keychain/hits`, so "the server
//!   was asked" is measured per test, not inferred.
//!
//! The server fake is STATELESS — it finds `fake-keychain/` by walking up from
//! the slot dir — so installing it process-wide from many concurrent tests is
//! race-free. Included with `#[path = "support/fake_login.rs"] mod fake_login;`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};

use threads::accounts::{
    keychain_service_for, set_identity_guard_for_test, IdentityGuard, IdentitySource,
    KeychainMetadata, ServerIdentity,
};
use threads::login::SlotKeychain;

pub const KC: &str = "fake-keychain";

/// The fake keychain: one file per item under `dir`.
pub struct FileKeychain {
    pub dir: PathBuf,
}

fn sha8_bytes(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b).iter().take(4).map(|b| format!("{b:02x}")).collect()
}

impl FileKeychain {
    fn path(&self, service: &str) -> PathBuf {
        self.dir.join(service)
    }
}

impl SlotKeychain for FileKeychain {
    fn secret_sha8(&self, service: &str) -> Option<String> {
        let mut b = std::fs::read(self.path(service)).ok()?;
        b.retain(|c| *c != b'\n');
        Some(sha8_bytes(&b))
    }
    fn copy(&self, from: &str, to: &str) -> bool {
        let Ok(b) = std::fs::read(self.path(from)) else { return false };
        std::fs::write(self.path(to), &b).is_ok() && self.secret_sha8(to) == self.secret_sha8(from)
    }
    fn delete(&self, service: &str) -> bool {
        let _ = std::fs::remove_file(self.path(service));
        !self.path(service).exists()
    }
}

/// The fake server: answers with the email inside the slot's item.
pub struct FileServer;

fn kc_dir_above(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().map(|a| a.join(KC)).find(|p| p.is_dir())
}

impl IdentitySource for FileServer {
    fn authenticated_identity(&self, config_dir: Option<&str>) -> Result<ServerIdentity, String> {
        let dir = config_dir.ok_or("the fake server has no machine default")?;
        let kc = kc_dir_above(Path::new(dir)).ok_or("no fake-keychain above this slot")?;
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(kc.join("hits")) {
            let _ = writeln!(f, "{dir}");
        }
        if kc.join("OFFLINE").exists() {
            return Err("could not reach the server (fake: offline)".into());
        }
        let tok = std::fs::read_to_string(kc.join(keychain_service_for(Some(dir))))
            .map_err(|_| "no credential in this slot".to_string())?;
        let email = tok.split('|').nth(1).ok_or("malformed fake token")?;
        Ok(ServerIdentity { email: email.to_string(), account_uuid: None })
    }
}

struct ConstMdat;
impl KeychainMetadata for ConstMdat {
    fn mdat(&self, _s: &str, _a: &str) -> Option<String> {
        Some("20261025000000Z".into())
    }
}

/// Install the fake server as the process's trusted read. Idempotent.
pub fn install_identity_seam() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        set_identity_guard_for_test(Some(Arc::new(IdentityGuard::new(
            Box::new(FileServer),
            Box::new(ConstMdat),
            PathBuf::from("/nonexistent-fake-home"),
        ))));
    });
}

/// Make `<home>/fake-keychain/` and return the keychain over it.
pub fn keychain_at(home: &Path) -> Arc<FileKeychain> {
    let dir = home.join(KC);
    std::fs::create_dir_all(&dir).unwrap();
    Arc::new(FileKeychain { dir })
}

/// The service a slot dir derives.
pub fn service_of(slot: &Path) -> String {
    keychain_service_for(Some(&slot.to_string_lossy()))
}

/// Sign `slot` in as `email` the way a completed login leaves it: the item,
/// the `.claude.json` and the `login-hint`. `nonce` makes two grants differ.
pub fn land(home: &Path, slot: &Path, email: &str, nonce: &str) {
    std::fs::create_dir_all(slot).unwrap();
    std::fs::write(home.join(KC).join(service_of(slot)), format!("tok|{email}|{nonce}")).unwrap();
    std::fs::write(
        slot.join(".claude.json"),
        format!("{{\"oauthAccount\":{{\"emailAddress\":\"{email}\"}}}}"),
    )
    .unwrap();
    std::fs::write(slot.join("login-hint"), format!("{email}\n")).unwrap();
}

/// How many times the fake server was asked, in this home.
pub fn hits(home: &Path) -> usize {
    std::fs::read_to_string(home.join(KC).join("hits"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// The item files under `<home>/fake-keychain/` whose name starts with `prefix`.
pub fn items_with_prefix(home: &Path, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(home.join(KC))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with(prefix))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// **A VERIFIED SLOT, for a test that reports a login** (`note_login_restored`
/// now clears only after the re-check's trusted read says `verified`). Gives
/// `label`'s slot an item the fake server reads as `email` and a matching
/// `login-hint` (the recorded address), with the fake keychain in the slot's
/// PARENT dir. No `.claude.json` is written, so the manager's default slot
/// probe never reaches a keychain reader. Installs the identity seam.
pub fn sign_in_verified(m: &threads::ThreadManager, label: &str, email: &str) {
    install_identity_seam();
    let dir = m.pool().login_dir(label).expect("the label has a slot dir").to_string();
    let slot = PathBuf::from(&dir);
    let parent = slot.parent().expect("a slot dir has a parent").to_path_buf();
    keychain_at(&parent);
    std::fs::create_dir_all(&slot).unwrap();
    std::fs::write(parent.join(KC).join(service_of(&slot)), format!("tok|{email}|test")).unwrap();
    std::fs::write(slot.join("login-hint"), format!("{email}\n")).unwrap();
}
