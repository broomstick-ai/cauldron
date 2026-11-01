//! **V1-ACCT3 — the V1-ACCT2-I non-blocking notes, closed.**
//!
//! - **N-a** a process bound to install A that opens install B holding its
//!   OWN key: refused (`OtherInstall`), B opens read-only with a notice, A's
//!   key is never written into B's ids and B's key file is untouched.
//! - **N-b** a key file that is corrupt, unreadable or LOST (missing where
//!   one was made): never rewritten, never silently replaced; the settings
//!   open read-only with a notice.
//! - **N-c** digests drawn before the first bind, under the temporary key:
//!   the install's key is adopted and the bind says `Redrew`.
//! - **The connect timeout**: a daemon that takes the TCP connection and
//!   never answers the WebSocket upgrade leaves the face `Connecting` only
//!   for `CONNECT_TIMEOUT`, then `Down` with that said, then retried.
//!
//! No case falls back to an unsalted id. Every address is a made-up
//! `example.com` one, every key here is generated or derived in the test.
//! Settings are opened over scratch roots; the socket is a loopback listener
//! on an ephemeral port. No face, prod port or live store is touched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use tui::cauldron::menu::{account_id, CLAUDE_CODE};
use tui::cauldron::settings::{def, Ctx, Settings, Val};
use tui::cauldron::store::{global_dir, soup_dir};
use tui::pool::model::install_key::{self, Bind, KeyError, KeyState};
use tui::pool::model::unsalted_digest_hex;

const CAROL: &str = "carol@example.com";

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "acct3-{tag}-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") }
}

/// `(cauldron dir, global dir)` of a fresh scratch install.
fn install(tag: &str) -> (PathBuf, PathBuf) {
    let env = env_in(&scratch(tag));
    (soup_dir(&env).unwrap(), global_dir(&env))
}

fn open(dir: &Path, global: &Path) -> Settings {
    Settings::open(dir, global, "prod", None, None, true, BTreeMap::new()).expect("opens")
}

/// A key that is NOT `key`: every hex digit moved by one.
fn other_key(key: &str) -> String {
    key.trim().chars().map(|c| std::char::from_digit((c.to_digit(16).unwrap() + 1) % 16, 16).unwrap()).collect()
}

/// A fresh, never-bound key's text, made by a throwaway state.
fn fresh_key_text() -> String {
    let d = scratch("mint");
    KeyState::new().bind(&d, true).unwrap();
    std::fs::read_to_string(d.join(install_key::FILE)).unwrap()
}

const ENABLED: &str = "pools.{pool}.accounts.{account}.enabled";

/// The key-problem notice, as the user reads it (V1-ACCT5). Written out here, not
/// called from the crate, so a change of wording fails these tests.
fn notice(problem: &str, unread: bool) -> String {
    let ro = format!("{problem}: account settings are read-only in this face, and a change made here is not saved");
    if !unread {
        return ro;
    }
    format!("{ro}. Settings saved for an email-address account under this install's key are not applied in this session (a switch you set may show its default); a named account's settings still apply")
}

/// The id `label` (an address) has under the key written in `key_text`:
/// the slug part of [`account_id`] with the digest drawn under that key.
fn id_under(label: &str, key_text: &str) -> String {
    use std::hash::Hasher;
    let t = key_text.trim();
    let (k0, k1) = (u64::from_str_radix(&t[..16], 16).unwrap(), u64::from_str_radix(&t[16..], 16).unwrap());
    #[allow(deprecated)]
    let mut h = std::hash::SipHasher::new_with_keys(k0, k1);
    h.write(label.as_bytes());
    let id = account_id(label);
    format!("{}{:06x}", &id[..id.len() - 6], h.finish() & 0xff_ffff)
}

/// The read-only proof shared by every failing case: the face carries on,
/// a switch flipped lives in this face only, the settings file is not
/// written, the notice says why, and the id is never the unsalted digest.
fn assert_fails_safe(s: &mut Settings, global: &Path, what: &str, unread: bool) {
    let problem = s.key_problem.clone().unwrap_or_else(|| panic!("{what}: the settings opened writable over an unusable key"));
    let notes = s.adoption_notes.join("; ");
    assert!(notes.contains("read-only") && notes.contains(&problem), "{what}: he is not told: {notes:?}");
    // V1-ACCT5 (V1-ACCT4-I B1): the notice is exactly one of two, and claims
    // saved settings go unapplied only when this process never bound the
    // install (it digests under another key).
    assert!(s.adoption_notes.contains(&notice(&problem, unread)), "{what}: the notice is not the {} one: {notes:?}", if unread { "unread" } else { "read-only" });
    // V1-ACCT4 (V1-ACCT3-I F1): a refused write names the key, never `--dump`.
    for (verb, err) in [
        ("created", s.add_account(CLAUDE_CODE, "Dave", |_| false).expect_err("an add in a key-read-only face is refused").to_string()),
        ("added", s.add_endpoint(false).expect_err("an endpoint add in a key-read-only face is refused").to_string()),
    ] {
        assert!(!err.contains("--dump"), "{what}: the refusal blames --dump: {err}");
        assert_eq!(err, format!("nothing is {verb}: account settings are read-only in this face because {problem}"), "{what}");
    }
    let gpath = global.join("settings.toml");
    let before = std::fs::read(&gpath).ok();
    let id = account_id(CAROL);
    s.set(def(ENABLED), &Ctx::account(CLAUDE_CODE, &id), Val::Bool(false)).expect("a change in this face still works");
    assert_eq!(s.get(def(ENABLED), &Ctx::account(CLAUDE_CODE, &id)).val, Val::Bool(false), "{what}: the change does not show in this face");
    assert_eq!(std::fs::read(&gpath).ok(), before, "{what}: the settings file was written under a key no later open can reproduce");
    assert!(!id.ends_with(&unsalted_digest_hex(CAROL)), "{what}: the id fell back to the unsalted digest: {id}");
}

/// **V1-ACCT4 opposition (F1)**: a face opened read-only (`--dump`) over a
/// good key still says `--dump` when it refuses, and has no key problem.
#[test]
fn acct4_a_dump_open_over_a_good_key_still_names_dump() {
    let (d, g) = install("acct4-dump");
    let _ = open(&d, &g);
    let mut ro = Settings::open(&d, &g, "prod", None, None, false, BTreeMap::new()).unwrap();
    assert!(ro.key_problem.is_none(), "{:?}", ro.key_problem);
    let err = ro.add_account(CLAUDE_CODE, "Dave", |_| false).expect_err("a --dump add is refused").to_string();
    assert_eq!(err, "nothing is created by --dump");
}

// ── V1-ACCT5: what the notice claims is true ─────────────────────────────

const WORK: &str = "Work";

/// **V1-ACCT5, the claim holds**: install B with its own key holds, under
/// B's key, Carol's switch off, and a named account's switch off. Opened in
/// a process bound to A: the notice says an email-address account's saved
/// settings are not applied — and Carol's switch reads its default — and
/// that a named account's still apply — and Work's switch reads off.
#[test]
fn acct5_over_another_key_an_address_accounts_settings_go_unapplied_and_a_named_ones_apply() {
    let (da, ga) = install("acct5-a");
    let _a = open(&da, &ga);
    let ka = std::fs::read_to_string(ga.join(install_key::FILE)).expect("A has a key");
    assert_eq!(id_under(CAROL, &ka), account_id(CAROL), "sentinel: the test draws ids as the face does");
    let kb = other_key(&ka);
    let carol_b = id_under(CAROL, &kb);
    assert_ne!(carol_b, account_id(CAROL), "sentinel: B's key gives Carol another id");
    let work = account_id(WORK);
    assert_eq!(work, "work", "sentinel: a named account's id is its slug");

    // B's settings, written by a face that may write, then moved under B.
    let (dw, gw) = install("acct5-writer");
    let mut w = open(&dw, &gw);
    for id in [&carol_b, &work] {
        w.set(def(ENABLED), &Ctx::account(CLAUDE_CODE, id), Val::Bool(false)).unwrap();
    }
    let (db, gb) = install("acct5-b");
    std::fs::create_dir_all(&gb).unwrap();
    std::fs::copy(gw.join("settings.toml"), gb.join("settings.toml")).expect("the writer saved");
    std::fs::write(gb.join(install_key::FILE), &kb).unwrap();

    let b = open(&db, &gb);
    let problem = b.key_problem.clone().expect("B opens read-only");
    assert!(b.adoption_notes.contains(&notice(&problem, true)), "{:?}", b.adoption_notes);
    let (df, gf) = install("acct5-fresh");
    let fresh = open(&df, &gf);
    let default = fresh.get(def(ENABLED), &Ctx::account(CLAUDE_CODE, &account_id(CAROL))).val;
    assert_eq!(default, Val::Bool(true), "sentinel: the switch's default is on");
    assert_eq!(b.get(def(ENABLED), &Ctx::account(CLAUDE_CODE, &account_id(CAROL))).val, default, "Carol's saved switch was applied: the notice says it is not");
    assert_eq!(b.get(def(ENABLED), &Ctx::account(CLAUDE_CODE, &work)).val, Val::Bool(false), "the named account's saved switch is not applied: the notice says it is");
}

/// **V1-ACCT5, a re-read after the key went bad**: the process bound this
/// install, so its ids are still the saved ones. The notice says only that
/// the face is read-only, and Carol's saved switch is applied.
#[test]
fn acct5_a_reread_after_the_key_is_corrupted_still_applies_and_does_not_say_otherwise() {
    let (d, g) = install("acct5-reread");
    let mut s = open(&d, &g);
    s.set(def(ENABLED), &Ctx::account(CLAUDE_CODE, &account_id(CAROL)), Val::Bool(false)).unwrap();
    std::fs::write(g.join(install_key::FILE), "this is not a key\n").unwrap();
    let r = open(&d, &g);
    let problem = r.key_problem.clone().expect("the re-read is read-only");
    assert!(r.adoption_notes.contains(&notice(&problem, false)), "{:?}", r.adoption_notes);
    assert!(!r.adoption_notes.iter().any(|n| n.contains("not applied")), "{:?}", r.adoption_notes);
    assert_eq!(r.get(def(ENABLED), &Ctx::account(CLAUDE_CODE, &account_id(CAROL))).val, Val::Bool(false), "Carol's saved switch is not applied");
}

/// **V1-ACCT5, `Write`**: no key and no mark, and none can be written. No id
/// was ever drawn under a key here, so the notice claims nothing goes
/// unapplied, and a named account's saved switch applies.
#[cfg(unix)]
#[test]
fn acct5_a_key_that_cannot_be_written_says_only_read_only() {
    use std::os::unix::fs::PermissionsExt;
    let (dw, gw) = install("acct5-w-writer");
    let mut w = open(&dw, &gw);
    w.set(def(ENABLED), &Ctx::account(CLAUDE_CODE, &account_id(WORK)), Val::Bool(false)).unwrap();
    let (d, g) = install("acct5-w");
    std::fs::create_dir_all(&g).unwrap();
    std::fs::copy(gw.join("settings.toml"), g.join("settings.toml")).unwrap();
    std::fs::set_permissions(&g, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(g.join("probe"), "").is_ok() {
        eprintln!("skipped: this user writes a 555 directory (root)");
        return;
    }
    let s = open(&d, &g);
    std::fs::set_permissions(&g, std::fs::Permissions::from_mode(0o755)).unwrap();
    let problem = s.key_problem.clone().expect("opens read-only");
    assert!(problem.contains("could not be written"), "{problem}");
    assert!(s.adoption_notes.contains(&notice(&problem, false)), "{:?}", s.adoption_notes);
    assert_eq!(s.get(def(ENABLED), &Ctx::account(CLAUDE_CODE, &account_id(WORK))).val, Val::Bool(false), "the named account's saved switch is not applied");
}

// ── N-a: another install's key ───────────────────────────────────────────

/// **N-a, the rule**: bound to A, a bind of B (its own key) is refused, the
/// process keeps A's key, and B's file is left as it was. A fresh B (no key)
/// is still given A's key, as before.
#[test]
fn na_a_second_install_with_its_own_key_is_refused_and_keeps_both_keys() {
    let mut st = KeyState::new();
    let a = scratch("na-a");
    assert_eq!(st.bind(&a, true).unwrap(), Bind::Created);
    let ka = std::fs::read_to_string(a.join(install_key::FILE)).unwrap();
    let k_before = st.get();

    let b = scratch("na-b");
    let kb = other_key(&ka);
    std::fs::write(b.join(install_key::FILE), &kb).unwrap();
    let r = st.bind(&b, true);
    assert!(matches!(r, Err(KeyError::OtherInstall(_))), "a second install's key was taken silently: {r:?}");
    assert_eq!(st.get(), k_before, "the process's key changed under the ids it drew");
    assert_eq!(std::fs::read_to_string(b.join(install_key::FILE)).unwrap(), kb, "B's key file was rewritten");

    // Opposition: B reopened in A's process with A's own key is fine, and a
    // fresh C is given A's key.
    assert_eq!(st.bind(&a, true).unwrap(), Bind::Read);
    let c = scratch("na-c");
    assert_eq!(st.bind(&c, true).unwrap(), Bind::Created);
    assert_eq!(std::fs::read_to_string(c.join(install_key::FILE)).unwrap(), ka, "a fresh install is given this process's key");
}

/// **N-a, through the settings**: the process opens A, then B with its own
/// key → B is read-only with a notice; nothing is written into B.
#[test]
fn na_settings_over_another_installs_key_open_read_only_and_say_so() {
    let (da, ga) = install("na-sa");
    let _a = open(&da, &ga);
    let ka = std::fs::read_to_string(ga.join(install_key::FILE)).expect("A has a key");
    let (db, gb) = install("na-sb");
    std::fs::create_dir_all(&gb).unwrap();
    let kb = other_key(&ka);
    std::fs::write(gb.join(install_key::FILE), &kb).unwrap();
    std::fs::write(gb.join("settings.toml"), "# B's own\n").unwrap();
    let mut b = open(&db, &gb);
    assert!(b.key_problem.as_deref().is_some_and(|p| p.contains("another install")), "{:?}", b.key_problem);
    assert_fails_safe(&mut b, &gb, "N-a", true);
    assert_eq!(std::fs::read_to_string(gb.join(install_key::FILE)).unwrap(), kb, "B's key was rewritten");
}

// ── N-b: corrupt, unreadable, lost ───────────────────────────────────────

/// **N-b, corrupt**: a key file that is not a key is never rewritten and
/// never replaced by a random key that would be written into ids.
#[test]
fn nb_a_corrupt_key_is_kept_and_the_settings_open_read_only() {
    let (d, g) = install("nb-corrupt");
    std::fs::create_dir_all(&g).unwrap();
    let junk = "this is not a key\n";
    std::fs::write(g.join(install_key::FILE), junk).unwrap();
    std::fs::write(g.join("settings.toml"), "# his\n").unwrap();
    let mut s = open(&d, &g);
    assert!(s.key_problem.as_deref().is_some_and(|p| p.contains("is not a key")), "{:?}", s.key_problem);
    assert_fails_safe(&mut s, &g, "N-b corrupt", true);
    assert_eq!(std::fs::read_to_string(g.join(install_key::FILE)).unwrap(), junk, "the corrupt key was rewritten");
    // KeyState: refused, and the state is unchanged.
    let mut st = KeyState::new();
    let k0 = st.get();
    assert!(matches!(st.bind(&g, true), Err(KeyError::Corrupt(_))));
    assert_eq!(st.get(), k0);
}

/// **N-b, unreadable** (wrong permissions): the same, and the file's mode is
/// not touched.
#[cfg(unix)]
#[test]
fn nb_an_unreadable_key_is_kept_and_the_settings_open_read_only() {
    use std::os::unix::fs::PermissionsExt;
    let (d, g) = install("nb-unread");
    std::fs::create_dir_all(&g).unwrap();
    let k = g.join(install_key::FILE);
    std::fs::write(&k, fresh_key_text()).unwrap();
    std::fs::write(g.join("settings.toml"), "# his\n").unwrap();
    std::fs::set_permissions(&k, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&k).is_ok() {
        eprintln!("skipped: this user reads a 000 file (root)");
        return;
    }
    let mut s = open(&d, &g);
    assert!(s.key_problem.as_deref().is_some_and(|p| p.contains("cannot be read")), "{:?}", s.key_problem);
    assert_fails_safe(&mut s, &g, "N-b unreadable", true);
    assert_eq!(std::fs::metadata(&k).unwrap().permissions().mode() & 0o777, 0, "the key's mode was changed");
    std::fs::set_permissions(&k, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// **N-b, lost**: once a key was made (the marker beside it), a missing key
/// is LOST — not made again, so ids are never silently changed — and the
/// settings open read-only. Opposition: a fresh install (no marker) creates
/// its key and marker; a pre-marker install with a key gets the marker.
#[test]
fn nb_a_lost_key_is_not_silently_replaced() {
    let (d, g) = install("nb-lost");
    let _s = open(&d, &g);
    let k = g.join(install_key::FILE);
    let m = g.join(install_key::MARKER);
    assert!(k.exists() && m.exists(), "sentinel: a fresh install made its key and marked it");
    let marker_text = std::fs::read_to_string(&m).unwrap();
    assert!(!marker_text.contains(std::fs::read_to_string(&k).unwrap().trim()), "the marker carries key material");
    std::fs::remove_file(&k).unwrap();
    let mut s = open(&d, &g);
    assert!(s.key_problem.as_deref().is_some_and(|p| p.contains("missing although one was made")), "{:?}", s.key_problem);
    assert!(!k.exists(), "a lost key was silently replaced by a new one");
    assert_fails_safe(&mut s, &g, "N-b lost", false);

    // A read-only open says the same and makes nothing.
    let ro = Settings::open(&d, &g, "prod", None, None, false, BTreeMap::new()).unwrap();
    assert!(ro.key_problem.is_some() && !k.exists());

    // KeyState: refused, unchanged.
    let mut st = KeyState::new();
    let k0 = st.get();
    assert!(matches!(st.bind(&g, true), Err(KeyError::Lost(_))));
    assert_eq!(st.get(), k0);

    // Opposition: a V1-ACCT2 install (key, no marker) is marked on open.
    let old = scratch("nb-premarker");
    std::fs::write(old.join(install_key::FILE), fresh_key_text()).unwrap();
    assert_eq!(KeyState::new().bind(&old, true).unwrap(), Bind::Read);
    assert!(old.join(install_key::MARKER).exists(), "an existing key was not marked");
    // …but not by a face that may not write.
    let old2 = scratch("nb-premarker-ro");
    std::fs::write(old2.join(install_key::FILE), fresh_key_text()).unwrap();
    assert_eq!(KeyState::new().bind(&old2, false).unwrap(), Bind::Read);
    assert!(!old2.join(install_key::MARKER).exists());
}

// ── N-c: digests drawn before the bind ───────────────────────────────────

/// **N-c**: a digest drawn under the temporary key, then a bind of an
/// install with its own key → the install's key is adopted and the bind
/// says `Redrew`. Oppositions: nothing drawn → `Read`; drawn, then a fresh
/// install → `Created` with the drawn key, so those ids hold.
#[test]
fn nc_a_digest_drawn_before_the_bind_is_said_to_change() {
    let text = fresh_key_text();
    let disk = (u64::from_str_radix(&text.trim()[..16], 16).unwrap(), u64::from_str_radix(&text.trim()[16..], 16).unwrap());
    let d = scratch("nc");
    std::fs::write(d.join(install_key::FILE), &text).unwrap();

    let mut st = KeyState::new();
    let tmp = st.get();
    assert_ne!(tmp, disk, "sentinel: the temporary key is not the install's");
    assert_eq!(st.bind(&d, true).unwrap(), Bind::Redrew, "ids drawn before the bind changed silently");
    assert_eq!(st.get(), disk, "the install's key is the one used after");

    let mut quiet = KeyState::new();
    assert_eq!(quiet.bind(&d, true).unwrap(), Bind::Read, "nothing was drawn: nothing to say");

    let mut kept = KeyState::new();
    let drawn = kept.get();
    let fresh = scratch("nc-fresh");
    assert_eq!(kept.bind(&fresh, true).unwrap(), Bind::Created);
    assert_eq!(kept.get(), drawn, "a fresh install was not given the key the ids were drawn under");
}

// ── the connect timeout ──────────────────────────────────────────────────

/// **A DAEMON THAT ACCEPTS AND NEVER ANSWERS**: the face says `Connecting`
/// while it waits, `Down` once `CONNECT_TIMEOUT` passes — with the reason —
/// and then tries again. Before, `connect_async` had no bound and the face
/// sat in `Connecting` for ever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_that_never_answers_the_handshake_is_down_after_the_connect_timeout_then_retried() {
    use std::time::{Duration, Instant};
    use tui::seam::link::CONNECT_TIMEOUT;
    use tui::transport::TransportEvent;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    assert!(!tui::cauldron::live::PROD_PORTS.contains(&port), "an ephemeral port is one of his live daemons");
    // Admitted by the egress gate as this process's own scratch listener.
    env::egress::grant_addr(listener.local_addr().unwrap()).expect("grant the mute listener");
    let accepted = std::sync::Arc::new(AtomicUsize::new(0));
    let n = accepted.clone();
    // Accept every connection, hold it, and never write a byte.
    let mute = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = listener.accept().await {
            n.fetch_add(1, Ordering::SeqCst);
            held.push(s);
        }
    });

    let (_handle, mut trx, _ports) = tui::mirror::spawn_at(port);
    // FIXED, never derived from CONNECT_TIMEOUT: a bound that scaled with
    // the constant would wait out an unbounded connect too (forced red, V1-ACCT3).
    let backstop = Duration::from_secs(30);
    assert!(CONNECT_TIMEOUT + Duration::from_secs(10) <= backstop, "the timeout under test outgrew the test's backstop");
    let t0 = Instant::now();
    let mut seen = Vec::new();
    let mut down_at = None;
    loop {
        let left = backstop.saturating_sub(t0.elapsed());
        assert!(!left.is_zero(), "no Down within {backstop:?} of a mute daemon: still {:?}", seen.last());
        let Ok(Some(ev)) = tokio::time::timeout(left, trx.recv()).await else { continue };
        match ev {
            TransportEvent::Connecting { attempt } => {
                seen.push(format!("connecting {attempt}"));
                if down_at.is_some() && attempt >= 2 {
                    break;
                }
            }
            TransportEvent::Disconnected { reason, .. } => {
                assert!(reason.contains("did not answer its handshake"), "the reason does not say why: {reason}");
                seen.push("down".into());
                down_at.get_or_insert(t0.elapsed());
            }
            _ => {}
        }
    }
    let down_at = down_at.unwrap();
    assert!(accepted.load(Ordering::SeqCst) >= 1, "sentinel: the TCP connection was taken, so it was the handshake that hung");
    assert!(down_at + Duration::from_millis(250) >= CONNECT_TIMEOUT, "Down before the timeout ({down_at:?}): not the timeout that fired");
    // The face starts `Connecting` and the mirror emits only on a change, so
    // `connecting 1` may or may not be an event; what must hold is that
    // nothing but `connecting` was said before the timeout's `Down`.
    let before: Vec<&String> = seen.iter().take_while(|e| *e != "down").collect();
    assert!(before.iter().all(|e| e.starts_with("connecting")), "the wait was drawn as something other than connecting: {seen:?}");
    mute.abort();
}
