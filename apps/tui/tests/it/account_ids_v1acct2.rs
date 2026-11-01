//! **V1-ACCT2 — the V1-ACCT-I findings, closed.**
//!
//! - **N1** one account with values under two OLD ids: a documented
//!   precedence decides (the pre-salt digest id, then the raw key, then the
//!   plain slug; a value under the id now beats them all), and he is told.
//! - **N2** two accounts that shared one old slug: the value goes to BOTH,
//!   whatever the roster's order, and he is told.
//! - **N3** the id's digest is keyed by a per-install secret stored beside
//!   the settings, created on first use: a guessed address cannot be checked
//!   against an id or a tag; ids written by the pre-salt face still migrate.
//! - **N4** `TUI_LOG=trace` never writes a raw wire frame.
//! - **N5** daemon prose on Tokens and the pool board passes the masker.
//!
//! Every address here is a made-up `example.com` one. Nothing here starts a
//! face, dials a port or touches a live store: settings are opened over
//! scratch roots, and shapes are folded by hand.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use env::paths::Source;
use env::Environment;
use serde_json::json;
use tui::cauldron::menu::{account_id, CLAUDE_CODE};
use tui::cauldron::settings::{def, Ctx, Settings, Val};
use tui::cauldron::store::{global_dir, soup_dir};
use tui::pool::model::{digest_hex, install_key, key_digest, project, unsalted_digest_hex, Fold, ACCOUNTS, NON_IDLE, SUMMARY, THREADS};

use crate::account_ids_v1acct::old_digest_id;

const CAROL: &str = "carol@example.com";
/// Two addresses whose plain slug is ONE string: `c-l-example-com`.
const DOT: &str = "c.l@example.com";
const DASH: &str = "c-l@example.com";

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "acct-ids2-{tag}-{}-{}-{}",
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

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Settings over a fresh scratch root whose global file is `global_toml`.
fn opened(tag: &str, global_toml: &str) -> (Settings, PathBuf) {
    let root = scratch(tag);
    let env = env_in(&root);
    let dir = soup_dir(&env).unwrap();
    let global = global_dir(&env);
    let gpath = global.join("settings.toml");
    write(&gpath, global_toml);
    (Settings::open(&dir, &global, "prod", None, None, true, BTreeMap::new()).expect("opens"), gpath)
}

fn share(s: &Settings, id: &str) -> Val {
    s.get(def("pools.{pool}.accounts.{account}.share"), &Ctx::account(CLAUDE_CODE, id)).val
}

fn enabled(s: &Settings, id: &str) -> Val {
    s.get(def("pools.{pool}.accounts.{account}.enabled"), &Ctx::account(CLAUDE_CODE, id)).val
}

fn slug(key: &str) -> String {
    key.to_lowercase().replace(['@', '.'], "-")
}

// ── N1: one account, two old ids ──────────────────────────────────────────

/// **N1**: carol has a share under her plain slug (25) AND under her raw key
/// (60). The raw key names exactly one account, so it wins; the slug's value
/// is dropped — and he is TOLD, masked. With the pre-salt digest id present
/// too (40), that newest form wins over both. Neither depends on the order
/// the old ids sit in the file.
#[test]
fn n1_two_old_ids_for_one_account_resolve_by_the_documented_precedence_and_say_so() {
    for (tag, file) in [
        ("slug-then-raw", format!("[pools.claude_code.accounts.{}]\nshare = 25\n\n[pools.claude_code.accounts.\"{CAROL}\"]\nshare = 60\n", slug(CAROL))),
        ("raw-then-slug", format!("[pools.claude_code.accounts.\"{CAROL}\"]\nshare = 60\n\n[pools.claude_code.accounts.{}]\nshare = 25\n", slug(CAROL))),
    ] {
        let (mut s, gpath) = opened(tag, &file);
        s.adopt_account_ids(CLAUDE_CODE, &[CAROL.into()]).expect("adopts");
        assert_eq!(share(&s, &account_id(CAROL)), Val::Int(60), "{tag}: the raw key's value wins over the plain slug's");
        let notes = s.adoption_notes.join("; ");
        assert!(notes.contains("more than one old id"), "{tag}: the dropped value is said: {notes:?}");
        assert!(!notes.contains(CAROL) && !notes.contains("carol"), "{tag}: a note names an address: {notes}");
        let g = read(&gpath);
        assert!(!g.contains(&slug(CAROL)) && !g.contains(CAROL), "{tag}: an old id is left:\n{g}");
    }
    let (mut s, _) = opened(
        "three",
        &format!(
            "[pools.claude_code.accounts.{}]\nshare = 25\n\n[pools.claude_code.accounts.\"{CAROL}\"]\nshare = 60\n\n[pools.claude_code.accounts.{}]\nshare = 40\n",
            slug(CAROL),
            old_digest_id(CAROL)
        ),
    );
    s.adopt_account_ids(CLAUDE_CODE, &[CAROL.into()]).unwrap();
    assert_eq!(share(&s, &account_id(CAROL)), Val::Int(40), "the pre-salt digest id is the newest form a face wrote: it wins");
}

// ── N2: one old slug, two accounts ────────────────────────────────────────

/// **N2**: `c.l@` and `c-l@` both had the plain slug `c-l-example-com`. A
/// switch, a share, a rename and the live-login pointer stored under it →
/// the switch, share and rename go to BOTH accounts, the pointer (which names
/// one) to the first label in byte order — the SAME result in either roster
/// order — and he is told.
#[test]
fn n2_an_old_slug_two_accounts_shared_goes_to_both_in_either_roster_order() {
    let old = slug(DOT);
    assert_eq!(old, slug(DASH), "sentinel: the two addresses share one plain slug");
    let mut results = Vec::new();
    for order in [[DOT, DASH], [DASH, DOT]] {
        let root = scratch("n2");
        let env = env_in(&root);
        let dir = soup_dir(&env).unwrap();
        let global = global_dir(&env);
        write(&global.join("settings.toml"), &format!("[pools.claude_code.accounts.{old}]\nenabled = false\nshare = 40\n\n[identity]\nlive_login = \"claude_code/{old}\"\n"));
        write(&global.join("accounts.json"), &serde_json::to_string_pretty(&json!({"accounts": [], "renamed": {format!("claude_code/{old}"): "Work"}})).unwrap());
        let mut s = Settings::open(&dir, &global, "prod", None, None, true, BTreeMap::new()).unwrap();
        s.adopt_account_ids(CLAUDE_CODE, &order.map(String::from)).unwrap();
        let per: Vec<(Val, Val, Option<String>)> = [DOT, DASH]
            .iter()
            .map(|k| (enabled(&s, &account_id(k)), share(&s, &account_id(k)), s.accounts_file.renamed.get(&format!("claude_code/{}", account_id(k))).cloned()))
            .collect();
        for (k, p) in [DOT, DASH].iter().zip(&per) {
            assert_eq!(p, &(Val::Bool(false), Val::Int(40), Some("Work".to_string())), "{order:?}: {k} lost the value its old id held");
        }
        assert_eq!(s.live_login(), Some(format!("claude_code/{}", account_id(DASH))), "{order:?}: the pointer goes to the first label in byte order");
        let notes = s.adoption_notes.join("; ");
        assert!(notes.contains("2 accounts"), "{order:?}: the shared id is said: {notes:?}");
        assert!(!notes.contains(DOT) && !notes.contains(DASH) && !notes.contains("c.l") && !notes.contains("c-l@"), "{order:?}: a note names an address: {notes}");
        let g = read(&global.join("settings.toml"));
        assert!(!g.contains(&old), "{order:?}: the shared old id is left:\n{g}");
        results.push((per, s.live_login()));
    }
    assert_eq!(results[0], results[1], "the roster's order decided something");
}

// ── N3: the digest is keyed per install ───────────────────────────────────

/// The digest computed HERE from a key file's text, independently of the
/// code: SipHash-2-4 under the file's two 64-bit halves.
fn keyed_hex(file: &str, key: &str) -> String {
    use std::hash::Hasher;
    let t = file.trim();
    let (k0, k1) = (u64::from_str_radix(&t[..16], 16).unwrap(), u64::from_str_radix(&t[16..], 16).unwrap());
    #[allow(deprecated)]
    let mut h = std::hash::SipHasher::new_with_keys(k0, k1);
    h.write(key.as_bytes());
    format!("{:06x}", h.finish() & 0xff_ffff)
}

/// **N3**: opening settings that may be written creates the install's key
/// BESIDE them (`<data>/shared/tui/install.key`, 128 bits, 0600); the id's
/// digest is the one that key makes, which is not the bare FNV a guess
/// could reproduce; and a pre-salt id migrates to it.
#[test]
fn n3_the_id_is_keyed_by_a_per_install_secret_beside_the_settings_and_old_ids_still_move() {
    let root = scratch("n3");
    let env = env_in(&root);
    let dir = soup_dir(&env).unwrap();
    let global = global_dir(&env);
    let kpath = global.join(install_key::FILE);
    assert!(!kpath.exists(), "sentinel: a fresh install has no key");
    let gpath = global.join("settings.toml");
    write(&gpath, &format!("[pools.claude_code.accounts.{}]\nenabled = false\n", old_digest_id(CAROL)));
    let mut s = Settings::open(&dir, &global, "prod", None, None, true, BTreeMap::new()).unwrap();

    let file = read(&kpath);
    assert!(file.trim().len() == 32 && file.trim().bytes().all(|b| b.is_ascii_hexdigit()), "the install key is 128 bits of hex: {:?}", file.len());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&kpath).unwrap().permissions().mode() & 0o777, 0o600, "the key is readable by its owner only");
    }
    // The id is the key's: what the code draws and stores is what the file
    // makes, and a guess that does not hold the file cannot make it.
    assert_eq!(digest_hex(CAROL), keyed_hex(&file, CAROL), "the digest is keyed by the file beside the settings");
    assert!(account_id(CAROL).ends_with(&keyed_hex(&file, CAROL)), "{}", account_id(CAROL));
    assert_ne!(digest_hex(CAROL), unsalted_digest_hex(CAROL), "the id is the bare digest a guess reproduces");
    assert_ne!(keyed_hex("0123456789abcdef0123456789abcdef", CAROL), digest_hex(CAROL), "another install's key makes another id");
    assert_eq!(key_digest(CAROL), key_digest(CAROL), "stable inside one install");

    // A pre-salt id (what the V1-ACCT face wrote) still moves.
    assert_eq!(enabled(&s, &account_id(CAROL)), Val::Bool(true), "sentinel: before adoption the switch is orphaned");
    s.adopt_account_ids(CLAUDE_CODE, &[CAROL.into()]).unwrap();
    assert_eq!(enabled(&s, &account_id(CAROL)), Val::Bool(false), "the pre-salt id's value applies under the keyed id");
    let g = read(&gpath);
    assert!(!g.contains(&old_digest_id(CAROL)) && g.contains(&account_id(CAROL)), "{g}");

    // Reopened (a restart of this install): the key is read, not remade.
    let s2 = Settings::open(&dir, &global, "prod", None, None, true, BTreeMap::new()).unwrap();
    assert_eq!(read(&kpath), file, "a second open rewrote the key");
    assert_eq!(enabled(&s2, &account_id(CAROL)), Val::Bool(false));
}

/// **N3's opposition**: a face that may not write (`--dump`) on a fresh
/// install creates no key file.
#[test]
fn n3_a_face_that_may_not_write_creates_no_key() {
    let root = scratch("n3-ro");
    let env = env_in(&root);
    let dir = soup_dir(&env).unwrap();
    let global = global_dir(&env);
    let _s = Settings::open(&dir, &global, "prod", None, None, false, BTreeMap::new()).unwrap();
    assert!(!global.join(install_key::FILE).exists(), "a read-only face wrote the install key");
}

// ── N4: the log filter caps the wire ──────────────────────────────────────

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// **N4**: under `TUI_LOG=trace` (and `tungstenite=trace`, spelled out), the
/// face's filter drops every `trace` line of tungstenite, tokio-tungstenite
/// and the seam — the lines that carry a frame body — and keeps their
/// `debug`, and every other target's `trace`.
#[test]
fn n4_tui_log_trace_never_writes_a_raw_frame() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer;
    const BODY: &str = "FRAME-BODY-carol@example.com";
    for spec in ["trace", "trace,tungstenite=trace,tui::seam=trace"] {
        let buf = Buf::default();
        let w = buf.clone();
        let sub = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer().with_writer(move || w.clone()).with_ansi(false).with_filter(tui::log_filter::face_filter(Some(spec))),
        );
        tracing::subscriber::with_default(sub, || {
            tracing::trace!(target: "tungstenite::protocol", "Received message {BODY}");
            tracing::trace!(target: "tokio_tungstenite", "{BODY}");
            tracing::trace!(target: "tui::seam::link", "{BODY}");
            tracing::trace!(target: "tui::transport", "{BODY}");
            tracing::debug!(target: "tungstenite::protocol", "SENTINEL-debug-kept");
            tracing::trace!(target: "tui::cauldron::app", "SENTINEL-trace-kept");
            tracing::trace!(target: "tungstenite_lookalike", "SENTINEL-prefix-kept");
        });
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(out.contains("SENTINEL-debug-kept") && out.contains("SENTINEL-trace-kept") && out.contains("SENTINEL-prefix-kept"), "{spec}: sentinel: the capture worked:\n{out}");
        assert!(!out.contains("FRAME-BODY"), "{spec}: a raw frame was logged:\n{out}");
    }
}

// ── N5: daemon prose is masked ────────────────────────────────────────────

/// **N5**: an account row whose limit prose, health reason and unranked
/// reason quote addresses, a dead shape's reason, and a recheck-failed mark:
/// every one reaches a card, the board and Tokens through the masker.
#[test]
fn n5_daemon_prose_on_tokens_and_the_pool_board_is_masked() {
    let mut accounts = Fold::new(ACCOUNTS);
    accounts.on_frame(&json!({
        "type": "shape", "shape": "accounts", "key": CAROL, "op": "read",
        "row": {
            "account": CAROL, "registered": true,
            "share": {"share": 0.5}, "disabled": {"disabled": false}, "gauge": {},
            "limit": {"until": "2099-01-01T00:00:00Z", "prose": format!("{CAROL} is at its weekly limit")},
            "health": {"dead_since": "2026-10-31T10:00:00Z", "dead_reason": format!("probe for {CAROL} timed out")},
            "credential": {"slot_state": "live"},
            "pressure": {"pressure": null, "unranked_reason": format!("{CAROL} has no gauge")},
        },
    }));
    accounts.dead = Some(format!("the accounts task died reading {CAROL}"));
    let board = project(&accounts, &Fold::new(NON_IDLE), &Fold::new(THREADS), &Fold::new(SUMMARY), chrono::Utc::now());
    let c = board.cards.iter().find(|c| c.label == CAROL).expect("the card");
    let said = [c.limit_prose.clone(), c.not_answering.clone(), c.unranked_reason.clone(), board.dead.clone()];
    for (what, s) in ["limit prose", "dead reason", "unranked reason", "shape death"].iter().zip(&said) {
        let s = s.as_deref().unwrap_or_else(|| panic!("sentinel: {what} reached the card: {said:?}"));
        assert!(s.contains("c•••@e•••"), "sentinel: {what} kept its words, masked: {s}");
        assert!(!s.contains(CAROL) && !s.contains("carol"), "{what} is drawn raw: {s}");
    }
    let mut dead = Fold::new(ACCOUNTS);
    dead.on_frame(&json!({"type": "shape", "shape": "accounts", "op": "read", "epoch": {"state": "dead", "dead_reason": format!("lost {CAROL}")}}));
    assert_eq!(dead.dead.as_deref(), Some("lost c•••@e•••"), "the pool board's own fold masks a death notice");
    let mk = tui::cauldron::live::Mark::of(&json!({"state": "recheck_failed", "op": "credential", "reason": format!("signed in as {CAROL}, not the account"), "started_at": "x", "failed_at": "y"})).expect("a mark");
    let w = mk.words();
    assert!(w.starts_with("recheck failed: signed in as c•••@e•••") && !w.contains("carol"), "{w}");
}
