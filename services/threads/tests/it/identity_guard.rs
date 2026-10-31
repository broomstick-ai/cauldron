//! R12.1 — `accounts::AccountId` and `accounts::IdentityGuard`
//! (cauldron `docs/ACCOUNT-EMAIL-ID.md` §2.1, §3, §5 row R12.1).
//!
//! Every fixture is a temp dir and a FAKE identity source / fake keychain
//! metadata. No real slot, keychain item or server is touched: the one test
//! that drives the real `ServerProfileSource` request path points it at a
//! loopback listener with a fake token reader, so the production keychain
//! reader (CON-28 tripwire) is never reached.
//!
//! Guards (each with its opposing arm, doctrine 1–2):
//! * G1a crossed — keyed `a@x.test`, source says `b@x.test` ⇒ `mismatch`;
//!   the same slot with a matching source ⇒ `verified` (so neither a
//!   constant-`verified` nor a constant-`mismatch` guard passes).
//! * G1b stale file — `.claude.json` says `a@`, source says `b@` ⇒ `mismatch`;
//!   and the reverse (file says `b@`, source says `a@`) ⇒ `verified` (a
//!   file-only guard fails both).
//! * G1c offline — source unreachable ⇒ `unreadable`, never `verified`, even
//!   when the cached file names the right email; the real request path
//!   against a closed port ⇒ `unreadable`, against a live loopback server ⇒
//!   `verified`.
//! * G1d normalisation — `A@X.test` = `a@x.test`; `a+1@x.test` ≠ `a@x.test`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use threads::accounts::{
    keychain_account, keychain_service_for, parse_keychain_mdat, parse_profile_response,
    AccessToken, AccountId, IdentityGuard, IdentitySource, IdentityState, IdentityVerdict,
    KeychainMetadata, ServerIdentity, ServerProfileSource,
};

// ── fixtures ────────────────────────────────────────────────────────────────

/// A fake trusted read: answers whatever the test put in it, and counts calls.
#[derive(Clone)]
struct FakeSource {
    answer: Arc<Mutex<Result<ServerIdentity, String>>>,
    calls: Arc<AtomicUsize>,
}

impl FakeSource {
    fn says(email: &str) -> Self {
        Self {
            answer: Arc::new(Mutex::new(Ok(ServerIdentity {
                email: email.to_string(),
                account_uuid: Some("uuid-fake".into()),
            }))),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn unreachable() -> Self {
        let s = Self::says("unused@x.test");
        *s.answer.lock().unwrap() = Err("connect: network is unreachable".into());
        s
    }
    fn set(&self, email: &str) {
        *self.answer.lock().unwrap() = Ok(ServerIdentity {
            email: email.into(),
            account_uuid: None,
        });
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl IdentitySource for FakeSource {
    fn authenticated_identity(&self, _config_dir: Option<&str>) -> Result<ServerIdentity, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answer.lock().unwrap().clone()
    }
}

/// A fake keychain-metadata reader: one mdat per service.
#[derive(Clone, Default)]
struct FakeKeychain {
    mdat: Arc<Mutex<std::collections::BTreeMap<String, String>>>,
}

impl FakeKeychain {
    fn set(&self, service: &str, mdat: &str) {
        self.mdat.lock().unwrap().insert(service.into(), mdat.into());
    }
}

impl KeychainMetadata for FakeKeychain {
    fn mdat(&self, service: &str, _account: &str) -> Option<String> {
        self.mdat.lock().unwrap().get(service).cloned()
    }
}

/// A temp slot dir whose `.claude.json` names `file_email`.
struct Slot {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
}

impl Slot {
    fn new(file_email: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("slot");
        std::fs::create_dir_all(&dir).unwrap();
        let s = Self { _tmp: tmp, dir };
        s.write_file_email(file_email);
        s
    }
    fn write_file_email(&self, email: &str) {
        let v = serde_json::json!({
            "oauthAccount": {
                "accountUuid": "uuid-file",
                "emailAddress": email,
                "organizationUuid": "org-file",
            }
        });
        std::fs::write(self.dir.join(".claude.json"), v.to_string()).unwrap();
    }
    fn config_dir(&self) -> &str {
        self.dir.to_str().unwrap()
    }
    fn service(&self) -> String {
        keychain_service_for(Some(self.config_dir()))
    }
}

fn guard(source: &FakeSource, kc: &FakeKeychain, home: &Path) -> IdentityGuard {
    IdentityGuard::new(Box::new(source.clone()), Box::new(kc.clone()), home.to_path_buf())
}

fn id(s: &str) -> AccountId {
    AccountId::parse(s).unwrap()
}

// ── G1a crossed ─────────────────────────────────────────────────────────────

#[test]
fn g1a_crossed_login_is_mismatch_and_matching_login_is_verified() {
    let slot = Slot::new("a@x.test");
    let kc = FakeKeychain::default();
    kc.set(&slot.service(), "20261025004103Z");
    let a = id("a@x.test");

    // Crossed: the account is keyed a@, its credential authenticates as b@.
    let crossed = FakeSource::says("b@x.test");
    let v = guard(&crossed, &kc, slot.dir.parent().unwrap()).verify(&a, Some(slot.config_dir()));
    assert_eq!(v.identity_state, IdentityState::Mismatch, "{v:?}");
    assert!(!v.is_placeable());
    assert_eq!(
        v.identity_reason.as_deref(),
        Some("credentials authenticate as b@x.test, not a@x.test")
    );
    assert_eq!(v.binding, None, "a mismatch records no binding");
    assert_eq!(crossed.calls(), 1);

    // Opposing arm: the same slot, a matching credential ⇒ verified.
    let good = FakeSource::says("a@x.test");
    let v = guard(&good, &kc, slot.dir.parent().unwrap()).verify(&a, Some(slot.config_dir()));
    assert_eq!(v.identity_state, IdentityState::Verified, "{v:?}");
    assert!(v.is_placeable());
    assert_eq!(v.identity_reason, None);
    let b = v.binding.expect("verified carries its binding");
    assert_eq!(b.keychain_mdat.as_deref(), Some("20261025004103Z"));
    assert!(b.oauth_account_sha8.is_some());
}

// ── G1b stale file ──────────────────────────────────────────────────────────

#[test]
fn g1b_stale_file_cannot_pass_a_crossed_login_nor_fail_a_good_one() {
    let kc = FakeKeychain::default();
    let a = id("a@x.test");

    // The cached file agrees with the account; the server does not.
    let stale = Slot::new("a@x.test");
    let src = FakeSource::says("b@x.test");
    let v = guard(&src, &kc, stale.dir.parent().unwrap()).verify(&a, Some(stale.config_dir()));
    assert_eq!(v.identity_state, IdentityState::Mismatch, "{v:?}");
    assert!(v.identity_reason.unwrap().contains("authenticate as b@x.test"));

    // Reverse: the cached file is stale the other way; the server agrees.
    let stale_other = Slot::new("b@x.test");
    let src = FakeSource::says("a@x.test");
    let v = guard(&src, &kc, stale_other.dir.parent().unwrap())
        .verify(&a, Some(stale_other.config_dir()));
    assert_eq!(v.identity_state, IdentityState::Verified, "{v:?}");
}

// ── G1c offline ─────────────────────────────────────────────────────────────

#[test]
fn g1c_unreachable_source_is_unreadable_never_verified() {
    // The cached file names the right email: an answer-from-cache would pass.
    let slot = Slot::new("a@x.test");
    let kc = FakeKeychain::default();
    let a = id("a@x.test");

    let offline = FakeSource::unreachable();
    let v = guard(&offline, &kc, slot.dir.parent().unwrap()).verify(&a, Some(slot.config_dir()));
    assert_eq!(v.identity_state, IdentityState::Unreadable, "{v:?}");
    assert!(!v.is_placeable());
    let reason = v.identity_reason.unwrap();
    assert!(reason.contains("unreadable") && reason.contains("network is unreachable"), "{reason}");

    // Opposing arm: the same source back online ⇒ verified.
    offline.set("a@x.test");
    let v = guard(&offline, &kc, slot.dir.parent().unwrap()).verify(&a, Some(slot.config_dir()));
    assert_eq!(v.identity_state, IdentityState::Verified, "{v:?}");
}

/// A one-shot loopback HTTP server: records the request, answers `status` +
/// `body`. Returns the base URL and the recorded request.
fn one_shot_server(status: &str, body: &str) -> (String, Arc<Mutex<String>>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(String::new()));
    let seen2 = seen.clone();
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let h = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = s.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        *seen2.lock().unwrap() = String::from_utf8_lossy(&buf).to_string();
        s.write_all(resp.as_bytes()).unwrap();
    });
    (url, seen, h)
}

fn fake_token(expires_at_ms: Option<i64>) -> threads::accounts::AccessTokenReader {
    Box::new(move |_dir: Option<&str>| Ok(AccessToken::new("fake-token-r12-1".into(), expires_at_ms)))
}

fn real_path_guard(src: ServerProfileSource, home: &Path) -> IdentityGuard {
    IdentityGuard::new(Box::new(src), Box::new(FakeKeychain::default()), home.to_path_buf())
}

#[test]
fn g1c_real_request_path_offline_is_unreadable_and_online_is_verified() {
    let slot = Slot::new("a@x.test");
    let home = slot.dir.parent().unwrap();
    let a = id("a@x.test");

    // Offline: a port nothing listens on. The file still says a@.
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let mut src = ServerProfileSource::with_token_reader(closed, fake_token(None));
    src.timeout_secs = 5;
    let v = real_path_guard(src, home).verify(&a, Some(slot.config_dir()));
    assert_eq!(v.identity_state, IdentityState::Unreadable, "{v:?}");
    let reason = v.identity_reason.unwrap();
    assert!(reason.contains("profile request failed"), "{reason}");

    // Online: the server answers the profile, with a differently-cased email.
    let (url, seen, h) = one_shot_server(
        "200 OK",
        r#"{"account":{"uuid":"u-1","email":"A@X.test"},"organization":{"uuid":"o-1"}}"#,
    );
    let v = real_path_guard(ServerProfileSource::with_token_reader(url, fake_token(None)), home)
        .verify(&a, Some(slot.config_dir()));
    h.join().unwrap();
    assert_eq!(v.identity_state, IdentityState::Verified, "{v:?}");
    let req = seen.lock().unwrap().clone();
    assert!(req.starts_with("GET /api/oauth/profile HTTP/1.1\r\n"), "{req}");
    assert!(req.contains("\r\nAuthorization: Bearer fake-token-r12-1\r\n"), "{req}");

    // Online but crossed: the real path reports the mismatch.
    let (url, _seen, h) = one_shot_server("200 OK", r#"{"account":{"email":"b@x.test"}}"#);
    let v = real_path_guard(ServerProfileSource::with_token_reader(url, fake_token(None)), home)
        .verify(&a, Some(slot.config_dir()));
    h.join().unwrap();
    assert_eq!(v.identity_state, IdentityState::Mismatch, "{v:?}");

    // A rejected token is unreadable, not a pass.
    let (url, _seen, h) = one_shot_server("401 Unauthorized", r#"{"error":"invalid"}"#);
    let v = real_path_guard(ServerProfileSource::with_token_reader(url, fake_token(None)), home)
        .verify(&a, Some(slot.config_dir()));
    h.join().unwrap();
    assert_eq!(v.identity_state, IdentityState::Unreadable, "{v:?}");
    assert!(v.identity_reason.unwrap().contains("HTTP 401"));

    // R12.10 (R12.1 inspect): the token never reaches the reason. The old probe
    // asserted it on the OFFLINE path, where curl under `-sS` has nothing to
    // echo, so it could not fail. Here the server REFLECTS the request's
    // Authorization header into its answer — the shape that could carry the
    // token into a reason — on a 2xx that is not JSON and on a 5xx. Forced red
    // by quoting the body in either error (R12.10 report §5).
    for status in ["200 OK", "502 Bad Gateway"] {
        let (url, _seen, h) =
            one_shot_server(status, "upstream saw: Authorization: Bearer fake-token-r12-1");
        let v = real_path_guard(ServerProfileSource::with_token_reader(url, fake_token(None)), home)
            .verify(&a, Some(slot.config_dir()));
        h.join().unwrap();
        assert_eq!(v.identity_state, IdentityState::Unreadable, "{status}: {v:?}");
        let reason = v.identity_reason.unwrap();
        assert!(!reason.contains("fake-token-r12-1"), "{status}: the token leaked into the reason: {reason}");
    }
}

#[test]
fn g1c_expired_token_is_unreadable_without_contacting_the_server() {
    let slot = Slot::new("a@x.test");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let v = real_path_guard(
        ServerProfileSource::with_token_reader(url, fake_token(Some(1_000))),
        slot.dir.parent().unwrap(),
    )
    .verify(&id("a@x.test"), Some(slot.config_dir()));
    assert_eq!(v.identity_state, IdentityState::Unreadable, "{v:?}");
    assert!(v.identity_reason.unwrap().contains("expired"));
    // The call has returned, so a connection it made would be in the backlog.
    match listener.accept() {
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("an expired token reached the server: {other:?}"),
    }
}

// ── G1d normalisation ───────────────────────────────────────────────────────

#[test]
fn g1d_case_folds_and_plus_addressing_does_not() {
    assert_eq!(id("A@X.test"), id("a@x.test"));
    assert_eq!(id("  Owner.Name@Example.AI ").as_str(), "owner.name@example.ai");
    assert_ne!(id("a+1@x.test"), id("a@x.test"));
    assert_eq!(id("a+1@x.test").as_str(), "a+1@x.test");
    assert_ne!(id("s.h@x.test"), id("sh@x.test"), "dots are not stripped");
    for bad in ["", "a", "a@", "@x.test", "a@x", "a@@x.test", "a@b@x.test", "a b@x.test", "a@.test", "a@x."] {
        assert!(AccountId::parse(bad).is_err(), "{bad:?} parsed");
    }
    // serde routes through parse, both ways.
    let back: AccountId = serde_json::from_str("\"A@X.test\"").unwrap();
    assert_eq!(back.as_str(), "a@x.test");
    assert!(serde_json::from_str::<AccountId>("\"not-an-email\"").is_err());
    assert_eq!(serde_json::to_string(&id("a@x.test")).unwrap(), "\"a@x.test\"");
}

#[test]
fn g1d_guard_compares_normalised_emails() {
    let slot = Slot::new("a@x.test");
    let kc = FakeKeychain::default();
    let home = slot.dir.parent().unwrap();
    let v = guard(&FakeSource::says("A@X.Test"), &kc, home).verify(&id("a@x.test"), Some(slot.config_dir()));
    assert_eq!(v.identity_state, IdentityState::Verified, "{v:?}");
    let v = guard(&FakeSource::says("a+1@x.test"), &kc, home).verify(&id("a@x.test"), Some(slot.config_dir()));
    assert_eq!(v.identity_state, IdentityState::Mismatch, "{v:?}");
    // A server answer that is not an email is unreadable, not a match.
    let v = guard(&FakeSource::says("garbage"), &kc, home).verify(&id("a@x.test"), Some(slot.config_dir()));
    assert_eq!(v.identity_state, IdentityState::Unreadable, "{v:?}");
}

// ── the binding ─────────────────────────────────────────────────────────────

#[test]
fn binding_holds_without_a_server_call_and_voids_on_either_change() {
    let slot = Slot::new("a@x.test");
    let kc = FakeKeychain::default();
    kc.set(&slot.service(), "20261025004103Z");
    let src = FakeSource::says("a@x.test");
    let g = guard(&src, &kc, slot.dir.parent().unwrap());
    let a = id("a@x.test");
    let v = g.verify(&a, Some(slot.config_dir()));
    assert_eq!(v.identity_state, IdentityState::Verified);
    assert_eq!(src.calls(), 1);

    // Unchanged: still verified, and the server was not asked.
    let same = g.check_binding(&v, Some(slot.config_dir()));
    assert_eq!(same, v);
    assert_eq!(src.calls(), 1);

    // The keychain item was rewritten (a /login): void.
    kc.set(&slot.service(), "20261026120000Z");
    let k = g.check_binding(&v, Some(slot.config_dir()));
    assert_eq!(k.identity_state, IdentityState::Unverified, "{k:?}");
    assert!(!k.is_placeable());
    assert!(k.identity_reason.unwrap().contains("keychain item modified"));
    kc.set(&slot.service(), "20261025004103Z");
    assert_eq!(g.check_binding(&v, Some(slot.config_dir())), v, "restored binding holds again");

    // The cached profile was rewritten: void.
    slot.write_file_email("b@x.test");
    let f = g.check_binding(&v, Some(slot.config_dir()));
    assert_eq!(f.identity_state, IdentityState::Unverified, "{f:?}");
    assert!(f.identity_reason.unwrap().contains("oauthAccount changed"));
    assert_eq!(src.calls(), 1, "the cheap check never asks the server");

    // Only a trusted read clears it — here, a crossed one.
    src.set("b@x.test");
    let r = g.verify(&a, Some(slot.config_dir()));
    assert_eq!(r.identity_state, IdentityState::Mismatch);
}

#[test]
fn a_non_verified_verdict_is_never_promoted_by_the_cheap_check() {
    let slot = Slot::new("a@x.test");
    let kc = FakeKeychain::default();
    let g = guard(&FakeSource::says("a@x.test"), &kc, slot.dir.parent().unwrap());
    let fresh = IdentityVerdict::unverified(id("a@x.test"));
    assert_eq!(fresh.identity_state, IdentityState::Unverified);
    assert_eq!(g.check_binding(&fresh, Some(slot.config_dir())), fresh);
}

#[test]
fn the_machine_default_binds_through_home() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"a@x.test"}}"#,
    )
    .unwrap();
    let kc = FakeKeychain::default();
    kc.set(&keychain_service_for(None), "20261001000000Z");
    let g = guard(&FakeSource::says("a@x.test"), &kc, tmp.path());
    let b = g.binding_of(None);
    assert_eq!(b.keychain_mdat.as_deref(), Some("20261001000000Z"));
    assert!(b.oauth_account_sha8.is_some());
    assert_eq!(keychain_account().is_empty(), false);
}

// ── pure parsers ────────────────────────────────────────────────────────────

#[test]
fn mdat_is_parsed_from_security_attribute_listing() {
    let listing = "keychain: \"/Users/u/Library/Keychains/login.keychain-db\"\n\
version: 512\n\
class: \"genp\"\n\
attributes:\n    0x00000007 <blob>=\"Claude Code-credentials-d5660ea9\"\n    \"acct\"<blob>=\"u\"\n    \"cdat\"<timedate>=0x32303236303931353139313735315A00  \"20260915191751Z\\000\"\n    \"mdat\"<timedate>=0x32303236313032353030343130335A00  \"20261025004103Z\\000\"\n    \"svce\"<blob>=\"Claude Code-credentials-d5660ea9\"\n";
    assert_eq!(parse_keychain_mdat(listing).as_deref(), Some("20261025004103Z"));
    assert_eq!(parse_keychain_mdat("attributes:\n    \"mdat\"<timedate>=<NULL>\n"), None);
    assert_eq!(parse_keychain_mdat(""), None);
}

#[test]
fn profile_response_needs_account_email() {
    let ok = parse_profile_response(r#"{"account":{"uuid":"u","email":"a@x.test"}}"#).unwrap();
    assert_eq!(ok.email, "a@x.test");
    assert_eq!(ok.account_uuid.as_deref(), Some("u"));
    assert!(parse_profile_response(r#"{"account":{"uuid":"u"}}"#).is_err());
    assert!(parse_profile_response(r#"{"email":"a@x.test"}"#).is_err());
    assert!(parse_profile_response("<html>").is_err());
}

#[test]
fn identity_state_serialises_as_the_shape_spells_it() {
    for (s, w) in [
        (IdentityState::Verified, "verified"),
        (IdentityState::Mismatch, "mismatch"),
        (IdentityState::Unreadable, "unreadable"),
        (IdentityState::Unverified, "unverified"),
    ] {
        assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{w}\""));
        assert_eq!(s.as_str(), w);
    }
}

#[test]
fn access_token_debug_never_prints_the_secret() {
    let t = AccessToken::new("sk-secret-value".into(), Some(5));
    let d = format!("{t:?}");
    assert!(!d.contains("sk-secret-value") && d.contains("redacted"), "{d}");
    let blob = r#"{"claudeAiOauth":{"accessToken":"at-1","expiresAt":42}}"#;
    let t = threads::accounts::access_token_from_blob(blob).unwrap();
    assert_eq!(t.expires_at_ms, Some(42));
    // R12.10 (R12.1 inspect): the error never quotes the blob. The old probe fed
    // a blob whose token was EMPTY, so there was no secret in the input to leak
    // and it could not fail. Every blob here CARRIES a secret and still fails;
    // forced red by quoting the blob in the not-JSON error (R12.10 report §5).
    for bad in [
        r#"{"claudeAiOauth":{"accessToken":"sk-leak-r12-10","expiresAt":"#,
        r#"{"other":{"accessToken":"sk-leak-r12-10"}}"#,
        r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"sk-leak-r12-10"}}"#,
    ] {
        let e = threads::accounts::access_token_from_blob(bad).unwrap_err();
        assert!(!e.contains("sk-leak-r12-10"), "error quotes the blob: {e}");
    }
}
