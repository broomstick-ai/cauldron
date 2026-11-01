//! **V1-ACCT — one account masker, stable storage ids, a redacted login code.**
//!
//! Reconciles V1.6 (`accounts::masked`, `a…@e….com`) with V1-CLEAN1/2
//! (`pool::model::shown_name`, `a•••@e•••`, digest ids):
//!
//! - **A1** one masker: `shown_name` is idempotent, `masked_text` routes prose
//!   through it, and two addresses that mask the same are told apart by a
//!   short tag off their id's digest — and only when they are twins.
//! - **A2** storage: a value stored under an account's OLD id (the slug of its
//!   address, or its raw key) is moved to its digest id once, the file is
//!   rewritten, and two twins keep distinct ids and distinct settings.
//! - **A3** the V1-CLEAN2-I measured case, through the face: `enabled = false`
//!   under the old slug id was ignored (`2 of 2 accounts open`); it applies.
//! - **A4** `Modal::Login`'s `{:?}` never carries the code.
//!
//! Every address here is a made-up `example.com` one. The face is pointed at
//! the in-process fake (`tests/it/fake`) on an ephemeral loopback port, never
//! at one of his daemons, over a scratch root and HOME.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use env::paths::Source;
use env::Environment;
use serde_json::{json, Value};
use tui::cauldron::accounts::{LoginCode, LoginStage};
use tui::cauldron::app::{App, CTab, Modal};
use tui::cauldron::live::{self, Live};
use tui::cauldron::menu::{account_id, legacy_account_ids, CLAUDE_CODE};
use tui::cauldron::render_to_text;
use tui::cauldron::settings::{def, Ctx, Settings, Val};
use tui::cauldron::store::{global_dir, seed_dir, soup_dir};
use tui::cauldron::threads;
use tui::pool::model::{digest_hex, masked_text, shown_among, shown_name};

use crate::fake::{Conn, Fake};

const AT: &str = "2026-10-31T11:00:00Z";
/// Twins: both mask as `c•••@e•••`.
const CAROL: &str = "carol@example.com";
const CORA: &str = "cora@example.com";
/// Not a twin of either.
const DANA: &str = "dana@example.com";

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "acct-ids-{tag}-{}-{}-{}",
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

// ── A1: one masker ────────────────────────────────────────────────────────

/// **A1**: the one masker masks once (a drawn name passed again is itself),
/// prose goes through it, and twins — and only twins — carry a tag taken
/// from their id's digest and nothing more of the address.
#[test]
fn a1_one_masker_once_and_twins_told_apart_by_their_ids_digest() {
    assert_eq!(shown_name(CAROL), "c•••@e•••");
    assert_eq!(shown_name(CORA), shown_name(CAROL), "sentinel: the two are twins");
    for k in [CAROL, CORA, DANA, "work-slot"] {
        assert_eq!(shown_name(&shown_name(k)), shown_name(k), "masking twice changes nothing: {k}");
    }
    let keys = [CAROL, CORA, DANA];
    let (a, b, d) = (shown_among(CAROL, keys), shown_among(CORA, keys), shown_among(DANA, keys));
    assert_ne!(a, b, "twins drawn the same: {a}");
    assert_eq!(a, format!("c•••@e••• ·{}", &digest_hex(CAROL)[..3]), "the tag is the head of the id's digest");
    assert!(account_id(CAROL).ends_with(&digest_hex(CAROL)), "sentinel: the id carries the same digest: {}", account_id(CAROL));
    assert_eq!(d, shown_name(DANA), "a name with no twin carries no tag");
    assert_eq!(shown_among(CAROL, [CAROL, DANA]), shown_name(CAROL), "no twin on THIS roster, no tag");
    assert_eq!(shown_among("work-slot", ["work-slot", "work-slot2"]), "work-slot", "a name that is not an address is drawn as it is");
    for s in [&a, &b, &d] {
        for part in ["carol", "cora", "dana", "example"] {
            assert!(!s.contains(part), "{s} carries `{part}`");
        }
    }
    let said = masked_text("refused: accounts/carol@example.com/disable (cora@example.com).");
    assert!(!said.contains("carol") && !said.contains("cora@"), "{said}");
    assert!(said.contains("(c•••@e•••)."), "{said}");
}

// ── A2: storage ids, read-migrated ────────────────────────────────────────

/// The old id, as a pre-CLEAN2 face wrote it (`store::slug` of the address:
/// lower-cased, every run of other characters one `-`). Computed HERE, not
/// through the code under test, so a forcing of `legacy_account_ids` reaches
/// the assertions on behaviour rather than this helper.
fn old_slug(key: &str) -> String {
    key.to_lowercase().replace(['@', '.'], "-")
}

/// The id a V1-CLEAN2..V1-ACCT face wrote: the masked slug and the UNSALTED
/// FNV-1a digest's low 24 bits. Computed HERE, independently of the code.
pub(crate) fn old_digest_id(key: &str) -> String {
    let h = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    let (l, d) = key.split_once('@').unwrap();
    format!("{}-{}-{:06x}", &l[..1], &d[..1], h & 0xff_ffff)
}

/// The code's legacy ids, in precedence order (V1-ACCT2, V1-ACCT-I N1): the
/// pre-salt digest id, the raw key, the plain slug — and none for a key that
/// is not an address.
#[test]
fn a2_the_legacy_ids_are_the_old_digest_id_the_raw_key_and_the_plain_slug_in_that_order() {
    assert_eq!(legacy_account_ids(CAROL), vec![old_digest_id(CAROL), CAROL.to_string(), old_slug(CAROL)]);
    assert!(legacy_account_ids("work-slot").is_empty());
}

/// **A2**: a global `settings.toml`, a cauldron `settings.toml` and an
/// `accounts.json` keyed by OLD ids (the email slug, and one raw key) →
/// after adoption every value applies to the right account under its digest
/// id, both files are rewritten with digest ids and none of the old, and the
/// two twins keep distinct ids and distinct settings. A second adoption
/// writes nothing.
#[test]
fn a2_settings_under_old_ids_move_to_the_digest_ids_once_and_twins_stay_apart() {
    let root = scratch("a2");
    let env = env_in(&root);
    let dir = soup_dir(&env).unwrap();
    let global = global_dir(&env);
    let (carol_old, cora_old) = (old_slug(CAROL), old_slug(CORA));
    let gpath = global.join("settings.toml");
    write(
        &gpath,
        &format!(
            "# his comment survives\n[pools.claude_code.accounts.{carol_old}]\nenabled = false\nshare = 25\n\n[pools.claude_code.accounts.{cora_old}]\nshare = 70\n\n[pools.claude_code.accounts.\"{DANA}\"]\nnote = \"raw key\"\n"
        ),
    );
    let cpath = dir.join("settings.toml");
    write(&cpath, &format!("[cauldron.accounts.claude_code.{carol_old}]\nallowed = false\n\n[identity]\nlive_login = \"claude_code/{cora_old}\"\n"));
    let apath = global.join("accounts.json");
    write(&apath, &serde_json::to_string_pretty(&json!({"accounts": [], "renamed": {format!("claude_code/{cora_old}"): "Work"}})).unwrap());

    let mut s = Settings::open(&dir, &global, "prod", None, None, true, BTreeMap::new()).expect("opens");
    let (carol, cora, dana) = (account_id(CAROL), account_id(CORA), account_id(DANA));
    assert_ne!(carol, cora, "twins keep distinct ids");
    let get = |s: &Settings, k: &str, id: &str| s.get(def(k), &Ctx::account(CLAUDE_CODE, id)).val;
    // Before: what he set is under ids nothing reads (the measured orphaning).
    assert_eq!(get(&s, "pools.{pool}.accounts.{account}.enabled", &carol), Val::Bool(true), "sentinel: before adoption the switch is orphaned");
    assert_eq!(get(&s, "pools.{pool}.accounts.{account}.enabled", &carol_old), Val::Bool(false), "sentinel: and it IS on disk, under the old id");

    let moved = s.adopt_account_ids(CLAUDE_CODE, &[CAROL.into(), CORA.into(), DANA.into()]).expect("adopts");
    assert!(moved >= 7, "every entry moved: {moved}");

    // After: each value applies to the right account.
    assert_eq!(get(&s, "pools.{pool}.accounts.{account}.enabled", &carol), Val::Bool(false));
    assert_eq!(get(&s, "pools.{pool}.accounts.{account}.enabled", &cora), Val::Bool(true), "the twin's switch is its own");
    assert_eq!(get(&s, "pools.{pool}.accounts.{account}.share", &carol), Val::Int(25));
    assert_eq!(get(&s, "pools.{pool}.accounts.{account}.share", &cora), Val::Int(70), "twins keep distinct settings");
    assert_eq!(get(&s, "pools.{pool}.accounts.{account}.note", &dana), Val::Str("raw key".into()), "a raw-key id is found too");
    assert_eq!(get(&s, "cauldron.accounts.{pool}.{account}.allowed", &carol), Val::Bool(false), "this cauldron's own switch moved");
    assert_eq!(s.live_login(), Some(format!("claude_code/{cora}")), "the live-login pointer moved");
    assert_eq!(s.accounts_file.renamed.get(&format!("claude_code/{cora}")).map(String::as_str), Some("Work"), "the rename moved");

    // The files say the same, with no old id left and his comment kept.
    let (g, c, a) = (read(&gpath), read(&cpath), read(&apath));
    for (what, text) in [("global", &g), ("cauldron", &c), ("accounts.json", &a)] {
        for old in [carol_old.as_str(), cora_old.as_str(), DANA] {
            // An emptied table that carries his comment keeps its header (the
            // file's rule, 0220 E3: his comments survive a save) — but holds
            // no value: the next line after it is blank, a header, or the end.
            let lines: Vec<&str> = text.lines().collect();
            for (i, l) in lines.iter().enumerate().filter(|(_, l)| l.contains(old)) {
                let next = lines.get(i + 1).map_or("", |n| n.trim());
                let commented = i > 0 && lines[i - 1].trim_start().starts_with('#');
                assert!(l.trim_start().starts_with('[') && commented && (next.is_empty() || next.starts_with('[')), "{what} still holds a value under `{old}`:\n{text}");
            }
        }
    }
    assert!(g.contains(&carol) && g.contains(&cora) && g.contains(&dana) && g.contains("# his comment survives"), "{g}");
    // `identity.` is a GLOBAL key: the pointer he kept in this cauldron's
    // file is adopted into the global one, under the new id.
    assert!(c.contains(&carol), "{c}");
    assert!(g.contains(&format!("live_login = \"claude_code/{cora}\"")), "{g}");
    assert!(a.contains(&cora), "{a}");

    // Reopened from disk: the values are read under the new ids.
    let s2 = Settings::open(&dir, &global, "prod", None, None, true, BTreeMap::new()).expect("reopens");
    assert_eq!(get(&s2, "pools.{pool}.accounts.{account}.share", &cora), Val::Int(70));
    assert_eq!(get(&s2, "pools.{pool}.accounts.{account}.enabled", &carol), Val::Bool(false));

    // Write-through ONCE: adopting again moves and writes nothing.
    let before = (std::fs::metadata(&gpath).unwrap().modified().unwrap(), g.clone(), c.clone(), a.clone());
    let mut s3 = s2;
    assert_eq!(s3.adopt_account_ids(CLAUDE_CODE, &[CAROL.into(), CORA.into(), DANA.into()]).unwrap(), 0);
    assert_eq!((std::fs::metadata(&gpath).unwrap().modified().unwrap(), read(&gpath), read(&cpath), read(&apath)), before, "a second adoption rewrote a file");
}

/// **A2's opposition**: a value already under the new id is his newer
/// choice and wins over the old id's; a key that is not an address has no
/// legacy id and nothing of it moves.
#[test]
fn a2_a_value_under_the_new_id_wins_and_a_non_address_never_moves() {
    let root = scratch("a2b");
    let env = env_in(&root);
    let dir = soup_dir(&env).unwrap();
    let global = global_dir(&env);
    let carol = account_id(CAROL);
    let gpath = global.join("settings.toml");
    write(&gpath, &format!("[pools.claude_code.accounts.{}]\nshare = 10\n\n[pools.claude_code.accounts.{carol}]\nshare = 90\n\n[pools.claude_code.accounts.work-slot]\nshare = 33\n", old_slug(CAROL)));
    let mut s = Settings::open(&dir, &global, "prod", None, None, true, BTreeMap::new()).unwrap();
    assert!(legacy_account_ids("work-slot").is_empty(), "a non-address keeps its id");
    s.adopt_account_ids(CLAUDE_CODE, &[CAROL.into(), "work-slot".into()]).unwrap();
    let get = |id: &str| s.get(def("pools.{pool}.accounts.{account}.share"), &Ctx::account(CLAUDE_CODE, id)).val;
    assert_eq!(get(&carol), Val::Int(90), "the newer value under the new id won");
    assert_eq!(get("work-slot"), Val::Int(33));
    let g = read(&gpath);
    assert!(!g.contains(&old_slug(CAROL)) && g.contains("work-slot"), "{g}");
}

// ── A3: the measured case, through the face ───────────────────────────────

fn known() -> Value {
    json!({"state": "known", "complete": true, "folded_at": AT})
}

fn epoch() -> Value {
    json!({"init_at": AT, "reinit": 0, "lost": 0, "state": "known"})
}

fn account(key: &str) -> Value {
    let cells: serde_json::Map<String, Value> =
        ["registered", "share", "disabled", "gauge", "limit", "health", "credential", "pressure"].iter().map(|c| (c.to_string(), known())).collect();
    json!({
        "type": "shape", "shape": "accounts", "key": key, "op": "read", "as_of": AT, "epoch": epoch(),
        "cells": cells,
        "row": {
            "account": key, "registered": true,
            "share": {"share": 0.5},
            "disabled": {"disabled": false},
            "gauge": {}, "limit": {"until": null}, "health": {"dead_since": null},
            "credential": {"slot_state": "live", "is_live_login": false},
            "pressure": {"pressure": 1.2, "share": 0.5},
        },
    })
}

fn members(keys: &[&str]) -> Value {
    json!({"type": "shape", "shape": "accounts", "op": "read", "as_of": AT, "epoch": epoch(),
        "members": {"state": "known", "keys": keys, "complete": true}})
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 80).unwrap().join("\n")
}

/// **A3** (V1-CLEAN2-I B1, measured there): `enabled = false` written under
/// carol's OLD slug id. The face opens over that file, the daemon serves the
/// twins carol and cora, and the stored switch applies to carol — Status ›
/// Tokens reads `1 of 2 accounts open` — and to her alone; the file is
/// rewritten with her digest id. Every frame masks both, and tells them apart.
#[test]
fn a3_enabled_false_under_the_old_slug_id_applies_after_the_roster_lands() {
    let root = scratch("a3");
    let env = env_in(&root);
    let gpath = global_dir(&env).join("settings.toml");
    write(&gpath, &format!("[pools.claude_code.accounts.{}]\nenabled = false\n", old_slug(CAROL)));

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let fake = rt.block_on(Fake::start(vec![Conn::default().push(members(&[CAROL, CORA])).push(account(CAROL)).push(account(CORA))]));
    assert!(!live::PROD_PORTS.contains(&fake.port), "a test face is pointed at a fake");
    let home = scratch("home");
    assert!(dirs::home_dir().map_or(true, |h| h != home), "a scratch HOME");
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env, seed_dir(), true).expect("the face opens");
    a.set_ctab(CTab::Status);
    a.set_sttab(tui::cauldron::status::StatusTab::Tokens);
    a.attach_live(Live::start_pointed(fake.port, home.to_string_lossy().into_owned()).expect("pointed at the fake"));
    let t0 = Instant::now();
    let mut frames = Vec::new();
    while !a.live.pool().is_some_and(|p| p.board.cards.len() == 2) {
        threads::tick(&mut a);
        frames.push(screen(&mut a));
        assert!(t0.elapsed() < Duration::from_secs(5), "timed out waiting for the two cards:\n{}", screen(&mut a));
        std::thread::sleep(Duration::from_millis(5));
    }
    threads::tick(&mut a);
    let s = screen(&mut a);
    frames.push(s.clone());

    assert!(!a.account_on(CLAUDE_CODE, &account_id(CAROL)), "carol's stored switch is ignored:\n{s}");
    assert!(a.account_on(CLAUDE_CODE, &account_id(CORA)), "the twin's switch is its own");
    assert!(s.contains("1 of 2 accounts open"), "Tokens reads the switch:\n{s}");
    let g = read(&gpath);
    assert!(g.contains(&account_id(CAROL)) && !g.contains(&old_slug(CAROL)), "the file is rewritten with the digest id:\n{g}");

    // Twins on screen: masked, and told apart — on Tokens and on Config.
    let (tc, to) = (shown_among(CAROL, [CAROL, CORA]), shown_among(CORA, [CAROL, CORA]));
    assert!(s.contains(&tc) && s.contains(&to), "both twins, told apart, on Tokens:\n{s}");
    a.set_ctab(CTab::Accounts);
    a.snav.set_open(tui::cauldron::menu::SPage::Pool(CLAUDE_CODE.into()), true);
    let rows: Vec<String> = a.accounts(CLAUDE_CODE).into_iter().map(|x| x.label).collect();
    assert_eq!(rows, vec![tc.clone(), to.clone()], "Config › Accounts rows");
    assert_eq!(a.account_name(CLAUDE_CODE, &account_id(CORA)), to, "a page title is masked once and keeps its tag");
    a.start_act(tui::cauldron::manage::Act::InPool { key: CORA.into(), on: false });
    let Some(Modal::Confirm { title, body, .. }) = a.modal.clone() else { panic!("no confirm: {:?}", a.modal) };
    assert!(title.contains(&to) && body[0].contains(&to), "the in-pool confirm names which twin: {title} / {body:?}");
    frames.push(screen(&mut a));
    for f in &frames {
        for addr in [CAROL, CORA] {
            assert!(!f.contains(addr) && !f.contains(&addr[..addr.find('@').unwrap()]), "an address drawn:\n{f}");
        }
    }
}

// ── A4: the login code ────────────────────────────────────────────────────

/// **A4**: `{:?}` of a login modal holding a code never prints the code —
/// and the code IS in it (it derefs to it), and `{:?}` does print the
/// modal's other fields.
#[test]
fn a4_the_login_modals_debug_never_carries_the_code() {
    const CODE: &str = "SENTINEL-CODE-9q8w7e";
    let mut code = LoginCode::default();
    code.push_str(CODE);
    let m = Modal::Login { title: "Log in c•••@e•••".into(), stage: LoginStage::AwaitCode { session_id: "s-1".into(), url: "https://example.com/login".into() }, code, tag: 7 };
    let dbg = format!("{m:?}");
    let pretty = format!("{m:#?}");
    let Modal::Login { code, .. } = &m else { unreachable!() };
    assert_eq!(code.as_str(), CODE, "sentinel: the modal holds the code");
    assert!(dbg.contains("s-1") && dbg.contains("Log in"), "sentinel: Debug prints the modal's fields: {dbg}");
    for d in [&dbg, &pretty] {
        assert!(!d.contains(CODE) && !d.contains("9q8w7e"), "the code is printed: {d}");
        assert!(d.contains("redacted"), "{d}");
    }
}
