//! **V1.6 — Config › Accounts, real and global, toggled per cauldron.**
//!
//! The face is pointed at a FAKE threads chassis on an ephemeral loopback
//! port (`tests/it/fake`), never at one of his daemons (3651, 3653, 3753,
//! 9010), with a scratch HOME and a scratch cauldron root. Every account key
//! here is a made-up `example.com` address, so the masking is exercised on
//! the shape real keys have (R12.2 keys the pool by email) and no real
//! address exists anywhere in this file.
//!
//! - **G1** the roster is the daemon's (and never the seed's `global.json`).
//! - **G2** the global default is on and configurable per account; a cauldron
//!   that set its own keeps it (inherit vs explicit is real).
//! - **G3** *in the pool* is the daemon's: one `disable` goes out, and the
//!   row moves only when the daemon publishes `disabled`.
//! - **G4** the login round trip: `begin` → URL → `complete` with the code →
//!   the account appears only when the shape adds it; Esc sends `cancel`.
//! - **G5** the code is never printed (frames, the flight recorder) — and it
//!   did reach the daemon.
//! - **G6** a face is never pointed at his ports, and the stand-in daemon's
//!   login is refused and adds nothing.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use serde_json::{json, Value};
use tui::cauldron::accounts::{LoginStage, Roster};
use tui::pool::model::{masked_text, shown_name as masked};
use tui::cauldron::app::{App, CTab, Modal};
use tui::cauldron::live::{self, Live};
use tui::cauldron::manage::Act;
use tui::cauldron::menu::{account_id, Bound, SPage, CLAUDE_CODE};
use tui::cauldron::render_to_text;
use tui::cauldron::settings::{Ctx, Prov, Val};
use tui::cauldron::store::seed_dir;
use tui::cauldron::threads;

use crate::fake::{Conn, Fake};

const AT: &str = "2026-10-31T11:00:00Z";
const ALPHA: &str = "alpha@example.com";
const BETA: &str = "beta@example.com";
const GAMMA: &str = "gamma@example.com";
const SENTINEL: &str = "SENTINEL-CODE-123";
/// The longest period on the path from a frame to the screen: the seam's
/// 100 ms debounce plus one 16 ms frame. A "nothing moved" assertion waits
/// past it, five times over.
const OUTLAST: Duration = Duration::from_millis((100 + 16) * 5);

// ── the harness ───────────────────────────────────────────────────────────

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "accounts-live-{tag}-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path, name: &str) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") }
        .for_name(name)
        .unwrap()
}

/// The cauldron face on Config › Accounts, Claude Code's band open, its one
/// connection pointed at `port`. Opened over the FIXTURE seed on purpose: its
/// `global.json` carries three made-up accounts, so a roster that still read
/// the seed would show them.
fn face(port: u16) -> App {
    assert!(!live::PROD_PORTS.contains(&port), "a test face is pointed at a fake, never at {port}");
    let home = scratch("home");
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env_in(&scratch("root"), "prod"), seed_dir(), false).expect("the face opens");
    a.set_ctab(CTab::Accounts);
    a.snav.set_open(SPage::Pool(CLAUDE_CODE.into()), true);
    let live = Live::start_pointed(port, home.to_string_lossy().into_owned()).expect("pointed at the fake");
    a.attach_live(live);
    a
}

/// Every frame drawn while pumping, so G5 can read them all.
fn pump(a: &mut App, frames: &mut Vec<String>, what: &str, done: impl Fn(&App) -> bool) {
    let t0 = Instant::now();
    loop {
        threads::tick(a);
        frames.push(screen(a));
        if done(a) {
            return;
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "timed out waiting for {what}:\n{}", screen(a));
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn idle(a: &mut App, frames: &mut Vec<String>, d: Duration) {
    let t0 = Instant::now();
    while t0.elapsed() < d {
        threads::tick(a);
        frames.push(screen(a));
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 80).unwrap().join("\n")
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(key(KeyCode::Char(c)));
    }
}

/// The roster is in, with exactly `n` keys.
fn roster_of(n: usize) -> impl Fn(&App) -> bool {
    move |a: &App| matches!(a.roster(), Roster::Known(k) if k.len() == n && a.live.pool().is_some_and(|p| p.loading.is_empty()))
}

/// The screen line of `key`'s row (its masked name), as words.
fn row_words(s: &str, key: &str) -> Vec<String> {
    let m = masked(key);
    s.lines().find(|l| l.contains(&m)).map(|l| l.split_whitespace().map(str::to_string).collect()).unwrap_or_else(|| panic!("no row for {m}:\n{s}"))
}

fn login_stage(a: &App) -> Option<LoginStage> {
    match &a.modal {
        Some(Modal::Login { stage, .. }) => Some(stage.clone()),
        _ => None,
    }
}

// ── the producer's frames (`services/threads/src/shapes/accounts.rs`) ─────

fn known() -> Value {
    json!({"state": "known", "complete": true, "folded_at": AT})
}

fn epoch() -> Value {
    json!({"init_at": AT, "reinit": 0, "lost": 0, "state": "known"})
}

/// One `accounts` export; `disabled` is the daemon's machine-wide switch.
fn account(key: &str, disabled: bool) -> Value {
    let cells: serde_json::Map<String, Value> =
        ["registered", "share", "disabled", "gauge", "limit", "health", "credential", "pressure"].iter().map(|c| (c.to_string(), known())).collect();
    json!({
        "type": "shape", "shape": "accounts", "key": key, "op": "read", "as_of": AT, "epoch": epoch(),
        "cells": cells,
        "row": {
            "account": key, "registered": true,
            "share": {"share": 0.5},
            "disabled": {"disabled": disabled},
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

/// alpha and beta, both in the pool.
fn two() -> Conn {
    Conn::default().push(members(&[ALPHA, BETA])).push(account(ALPHA, false)).push(account(BETA, false))
}

fn seed_names(a: &App) -> Vec<String> {
    let names: Vec<String> = a.dash.threads.accounts.iter().map(|x| x.label.clone()).collect();
    assert!(names.len() >= 2, "sentinel: the fixture seed carries accounts to look for: {names:?}");
    names
}

// ── the pure parts ────────────────────────────────────────────────────────

/// V1-ACCT: ONE masker. What V1.6 drew as `a…@e….com` is now
/// `pool::model::shown_name`'s `a•••@e•••`, and prose from the daemon passes
/// the same masker (`masked_text`).
#[test]
fn an_address_is_drawn_masked_and_a_slot_name_as_it_is() {
    assert_eq!(masked(ALPHA), "a•••@e•••");
    assert_eq!(masked("work-slot"), "work-slot", "a name with no @ is not an address");
    let said = masked_text("refused: the login landed on gamma@example.com, not (beta@example.com).");
    assert!(!said.contains("gamma@example.com") && !said.contains("beta@example.com"), "{said}");
    assert!(said.contains("g•••@e•••") && said.contains("(b•••@e•••)."), "{said}");
    assert_eq!(masked_text("no address here"), "no address here");
}

/// The seam's in-pool paths ARE the pool board's (`pool::action`): one way to
/// say each act.
#[test]
fn the_in_pool_switch_says_what_the_pool_board_says() {
    use tui::pool::action::{Action, Command};
    use tui::seam::commands::methods;
    for (on, dialed) in [(true, methods::ACCOUNT_ENABLE.at(BETA)), (false, methods::ACCOUNT_DISABLE.at(BETA))] {
        let act = if on { Action::Enable(BETA.into()) } else { Action::Disable(BETA.into()) };
        let Command::Ws { method, .. } = act.command() else { panic!("an in-pool act is a daemon request") };
        assert_eq!(dialed.path(), method);
    }
    assert_ne!(methods::ACCOUNT_ENABLE.at(BETA).path(), methods::ACCOUNT_DISABLE.at(BETA).path(), "sentinel: the two paths differ");
}

// ── G1: the roster is the daemon's ────────────────────────────────────────

/// **G1**: the fake serves alpha and beta; Config › Accounts lists exactly
/// those two, masked, and none of the seed's.
#[test]
fn g1_accounts_lists_exactly_the_daemons_two_and_none_of_the_seeds() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![two()]));
    let mut a = face(fake.port);
    let mut frames = Vec::new();
    pump(&mut a, &mut frames, "two accounts", roster_of(2));
    let rows: Vec<String> = a.accounts(CLAUDE_CODE).into_iter().map(|x| x.label).collect();
    assert_eq!(rows, vec![masked(ALPHA), masked(BETA)]);
    let s = screen(&mut a);
    assert!(s.contains(&masked(ALPHA)) && s.contains(&masked(BETA)), "{s}");
    for n in seed_names(&a) {
        assert!(!s.contains(&n), "the seed's account `{n}` is on Config › Accounts:\n{s}");
    }
    for f in &frames {
        assert!(!f.contains(ALPHA) && !f.contains(BETA), "an address drawn whole:\n{f}");
    }
}

/// **G1's opposition**: the fake serving NO accounts (a complete, empty
/// membership) lists none — and still none of the seed's.
#[test]
fn g1_zero_accounts_served_lists_none_and_no_seed_account() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![Conn::default().push(members(&[]))]));
    let mut a = face(fake.port);
    let mut frames = Vec::new();
    pump(&mut a, &mut frames, "an empty pool", |a| a.live.pool().is_some_and(|p| p.known));
    assert!(a.accounts(CLAUDE_CODE).is_empty(), "{:?}", a.accounts(CLAUDE_CODE));
    let s = screen(&mut a);
    for n in seed_names(&a) {
        assert!(!s.contains(&n), "the seed's account `{n}` is listed with an empty pool:\n{s}");
    }
}

/// Before the pool is in, the band says why — never an empty table.
#[test]
fn g1_before_the_export_the_band_says_it_is_waiting() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![Conn::default()]));
    let mut a = face(fake.port);
    let mut frames = Vec::new();
    pump(&mut a, &mut frames, "the link", |a| a.live.is_up());
    let s = screen(&mut a);
    assert!(s.contains("loading the account pool"), "{s}");
}

// ── G2: the default is on, and configurable ───────────────────────────────

/// A face on environment `name` under `root` whose pool is alpha and beta,
/// folded by hand (no socket: G2 is about settings, not the wire).
fn env_face(root: &Path, name: &str) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env_in(root, name), seed_dir(), true).expect("the face opens");
    let mut live = Live::detached(String::new());
    for f in [members(&[ALPHA, BETA]), account(ALPHA, false), account(BETA, false)] {
        live.fold_by_hand(&f);
    }
    live.store.link = tui_render::LinkState::Connected;
    a.attach_live(live);
    a
}

fn allowed(key: &str) -> Bound {
    Bound::new("cauldron.accounts.{pool}.{account}.allowed", Ctx::account(CLAUDE_CODE, &account_id(key)))
}

fn global_default(key: &str) -> Bound {
    Bound::new("pools.{pool}.accounts.{account}.enabled", Ctx::account(CLAUDE_CODE, &account_id(key)))
}

/// The `use in this cauldron` row of `key`'s modal on the environment's
/// Settings tab, as words.
fn use_here_row(a: &mut App, key: &str) -> String {
    a.set_ctab(CTab::Settings);
    a.open_row(SPage::UseAccount(CLAUDE_CODE.into(), account_id(key)));
    let s = screen(a);
    let row = s.lines().find(|l| l.contains("use in this caul")).unwrap_or_else(|| panic!("no `use in this cauldron` row:\n{s}")).split_whitespace().collect::<Vec<_>>().join(" ");
    a.modal = None;
    row
}

/// **G2**: a new environment reads both accounts ON, inherited. The global
/// default turned OFF for beta → a NEW environment reads beta off
/// (inherited) and alpha still on. **Opposition**: an environment that set
/// beta ON itself keeps it after the global flip.
#[test]
fn g2_the_default_is_on_and_off_for_one_account_flows_to_a_new_env_but_not_an_explicit_one() {
    let root = scratch("g2");
    let mut first = env_face(&root, "first");
    for k in [ALPHA, BETA] {
        let r = first.sget(&allowed(k));
        assert_eq!((r.val.clone(), matches!(r.prov, Prov::Inherited(_) | Prov::Default)), (Val::Bool(true), true), "{k} in a new env: {r:?}");
        let row = use_here_row(&mut first, k);
        assert!(row.contains("inherits on") && !row.contains('↺'), "{k}: {row}");
    }
    // The first cauldron says, itself, that beta is used here.
    first.sset(&allowed(BETA), Val::Bool(true)).expect("set here");
    // Turn beta's global default off — the Accounts tab's `on in new cauldrons`.
    first.sset(&global_default(BETA), Val::Bool(false)).expect("set globally");
    assert_eq!(global_default(BETA).def().label, "on in new cauldrons");

    let mut second = env_face(&root, "second");
    let b = second.sget(&allowed(BETA));
    assert_eq!(b.val, Val::Bool(false), "beta in a NEW env after the flip: {b:?}");
    assert!(matches!(b.prov, Prov::Inherited(_)), "inherited, not set: {b:?}");
    assert!(use_here_row(&mut second, BETA).contains("inherits off"));
    let al = second.sget(&allowed(ALPHA));
    assert_eq!(al.val, Val::Bool(true), "alpha is untouched: {al:?}");

    // Opposition: the explicit ON outlives the flip.
    let mut first = env_face(&root, "first");
    let b = first.sget(&allowed(BETA));
    assert_eq!((b.val, b.prov), (Val::Bool(true), Prov::Here), "an explicit on is kept");
    assert!(use_here_row(&mut first, BETA).contains('↺'), "a value set here can be reverted");
}

// ── G3: in the pool is the daemon's ───────────────────────────────────────

fn in_pool_flip(a: &mut App, key: &str) {
    a.start_act(Act::InPool { key: key.into(), on: false });
    assert!(matches!(a.modal, Some(Modal::Confirm { .. })), "it asks first: {:?}", a.modal);
    a.handle(key_enter());
}

fn key_enter() -> Event {
    key(KeyCode::Enter)
}

fn sent(fake: &Fake, leaf: &str) -> Vec<String> {
    fake.log().into_iter().filter(|r| r.leaf() == leaf).map(|r| r.method).collect()
}

/// **G3**: taking beta out of the pool sends exactly ONE
/// `accounts/beta…/disable` (on the fake's own log), and the row reads `out`
/// once the fake publishes `disabled`.
#[test]
fn g3_out_of_the_pool_sends_one_disable_and_the_row_follows_the_daemon() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![two().then("disable", account(BETA, true))]));
    let mut a = face(fake.port);
    let mut frames = Vec::new();
    pump(&mut a, &mut frames, "two accounts", roster_of(2));
    assert!(row_words(&screen(&mut a), BETA).iter().any(|w| w == "in"), "beta starts in the pool");
    in_pool_flip(&mut a, BETA);
    pump(&mut a, &mut frames, "beta out", |a| a.in_pool(BETA) == Some(false));
    assert_eq!(sent(&fake, "disable"), vec![format!("accounts/{BETA}/disable")]);
    assert!(sent(&fake, "enable").is_empty());
    assert!(row_words(&screen(&mut a), BETA).iter().any(|w| w == "out"), "{}", screen(&mut a));
    assert!(row_words(&screen(&mut a), ALPHA).iter().any(|w| w == "in"), "alpha is untouched");
    assert_eq!(a.sget(&global_default(BETA)).val, Val::Bool(true), "the local default was not touched by the pool switch");
}

/// **G3's opposition**: with the fake NOT publishing, the row stays `in`
/// past the 100 ms debounce plus one 16 ms frame (×5) — the ack is not the
/// fact.
#[test]
fn g3_without_the_daemons_frame_the_row_stays_in_past_the_100ms_debounce_and_a_16ms_frame() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![two()]));
    let mut a = face(fake.port);
    let mut frames = Vec::new();
    pump(&mut a, &mut frames, "two accounts", roster_of(2));
    in_pool_flip(&mut a, BETA);
    rt.block_on(fake.until("the disable", |f| f.log().iter().any(|r| r.leaf() == "disable")));
    idle(&mut a, &mut frames, OUTLAST);
    assert_eq!(a.in_pool(BETA), Some(true));
    assert!(row_words(&screen(&mut a), BETA).iter().any(|w| w == "in"), "{}", screen(&mut a));
}

// ── G4: the login round trip ──────────────────────────────────────────────

/// The fake for a login: `begin` mints `s-1` and an address; `complete`
/// answers ok; `then` frames after `complete` (the daemon adding gamma).
fn login_conn(publish: bool) -> Conn {
    let mut c = two()
        .respond("begin", json!({"session_id": "s-1", "account": "gamma", "url": "https://login.example.com/oauth?state=abc", "next": "accounts/login/{session_id}/complete", "expires_in_secs": 600}))
        .respond("complete", json!({"account": "gamma", "ok": true, "exit_code": 0, "identity": GAMMA, "refused": null, "recheck": null, "reopened": [], "output": format!("signed in as {GAMMA}")}));
    if publish {
        c = c.then("complete", members(&[ALPHA, BETA, GAMMA])).then("complete", account(GAMMA, false));
    }
    c
}

/// `+ add account` on Claude Code: named, then Enter.
fn add_account(a: &mut App, name: &str) {
    a.modal = Some(Modal::AddAccount { pool: CLAUDE_CODE.into(), name: String::new(), error: None });
    typed(a, name);
    a.handle(key_enter());
}

fn awaiting(a: &App) -> bool {
    matches!(login_stage(a), Some(LoginStage::AwaitCode { .. }))
}

/// **G4 + G5**: `+ add account` → `begin` (the modal shows the address);
/// the code → `complete`; gamma is listed only once the fake adds it to the
/// shape — never on the ack. The code is in no frame and no flight-recorder
/// line, and it DID reach the daemon.
#[test]
fn g4_g5_a_login_round_trip_lists_the_account_only_from_the_shape_and_never_prints_the_code() {
    let tap = tui::diag::tap();
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![login_conn(true)]));
    let mut a = face(fake.port);
    let mut frames = Vec::new();
    pump(&mut a, &mut frames, "two accounts", roster_of(2));
    add_account(&mut a, "gamma");
    pump(&mut a, &mut frames, "the login's address", awaiting);
    let s = screen(&mut a);
    assert!(s.contains("https://login.example.com/oauth?state=abc"), "the address to sign in at:\n{s}");
    assert_eq!(fake.log().iter().find(|r| r.leaf() == "begin").map(|r| r.params["account"].clone()), Some(json!("gamma")));
    // The ack of `begin` added nothing.
    assert_eq!(a.roster_keys().len(), 2, "an account listed before the daemon listed it");
    typed(&mut a, SENTINEL);
    frames.push(screen(&mut a));
    a.handle(key_enter());
    pump(&mut a, &mut frames, "gamma listed", roster_of(3));
    pump(&mut a, &mut frames, "the modal done", |a| matches!(login_stage(a), Some(LoginStage::Done { ok: true, .. })));
    assert!(a.accounts(CLAUDE_CODE).iter().any(|x| x.key.as_deref() == Some(GAMMA)));
    idle(&mut a, &mut frames, Duration::from_millis(50));
    // G5: the value went where it should — and nowhere else.
    let complete = fake.log().into_iter().find(|r| r.leaf() == "complete").expect("complete was sent");
    assert_eq!(complete.method, "accounts/login/s-1/complete");
    assert_eq!(complete.params["code"], json!(SENTINEL), "the code reached the daemon");
    assert!(frames.len() >= 5, "sentinel: frames were captured ({})", frames.len());
    assert!(frames.iter().any(|f| f.contains("•••••")), "sentinel: the typed code was drawn, as dots");
    for f in &frames {
        assert!(!f.contains(SENTINEL), "the code was drawn:\n{f}");
        assert!(!f.contains(GAMMA), "an address drawn whole:\n{f}");
    }
    let lines = tap.lines();
    assert!(lines.iter().all(|l| !l.contains(SENTINEL)), "the code is in the flight recorder: {lines:?}");
    assert!(!format!("{:?}", tui::acts::AccountVerb::LoginComplete { session_id: "s".into(), code: SENTINEL.into() }).contains(SENTINEL), "a Debug of the verb prints the code");
}

/// **G4's opposition (the ack is not the row)**: `complete` answers ok and
/// the fake does NOT add gamma — gamma is never listed, past the 100 ms
/// debounce and a 16 ms frame.
#[test]
fn g4_a_signed_in_ack_without_the_shape_lists_nothing_past_the_100ms_debounce_and_a_16ms_frame() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![login_conn(false)]));
    let mut a = face(fake.port);
    let mut frames = Vec::new();
    pump(&mut a, &mut frames, "two accounts", roster_of(2));
    add_account(&mut a, "gamma");
    pump(&mut a, &mut frames, "the login's address", awaiting);
    typed(&mut a, "some-code");
    a.handle(key_enter());
    pump(&mut a, &mut frames, "the modal done", |a| matches!(login_stage(a), Some(LoginStage::Done { ok: true, .. })));
    idle(&mut a, &mut frames, OUTLAST);
    assert_eq!(a.roster_keys().len(), 2, "{:?}", a.roster_keys());
}

/// **G4: Esc mid-flow sends `cancel`** for the daemon's session, and no
/// `complete`.
#[test]
fn g4_esc_while_the_login_waits_for_the_code_cancels_it_at_the_daemon() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![login_conn(true)]));
    let mut a = face(fake.port);
    let mut frames = Vec::new();
    pump(&mut a, &mut frames, "two accounts", roster_of(2));
    a.start_act(Act::LogIn { key: BETA.into() });
    pump(&mut a, &mut frames, "the login's address", awaiting);
    assert_eq!(fake.log().iter().find(|r| r.leaf() == "begin").map(|r| r.params["account"].clone()), Some(json!(BETA)), "an account's `log in` begins for its own key");
    typed(&mut a, "half");
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none());
    rt.block_on(fake.until("the cancel", |f| f.log().iter().any(|r| r.leaf() == "cancel")));
    assert_eq!(sent(&fake, "cancel"), vec!["accounts/login/s-1/cancel".to_string()]);
    assert!(sent(&fake, "complete").is_empty(), "nothing was completed");
}

/// **G4's opposition (a refusal)**: the daemon refusing `begin` is shown in
/// the modal, masked, and adds nothing.
#[test]
fn g4_a_refused_begin_is_shown_and_adds_nothing() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![two().refuse("begin", "refused", &format!("no slot for {GAMMA}"))]));
    let mut a = face(fake.port);
    let mut frames = Vec::new();
    pump(&mut a, &mut frames, "two accounts", roster_of(2));
    add_account(&mut a, "gamma");
    pump(&mut a, &mut frames, "the refusal", |a| matches!(login_stage(a), Some(LoginStage::Done { ok: false, .. })));
    let s = screen(&mut a);
    assert!(s.contains("refused the login") && s.contains(&masked(GAMMA)) && !s.contains(GAMMA), "{s}");
    idle(&mut a, &mut frames, OUTLAST);
    assert_eq!(a.roster_keys().len(), 2);
    assert!(sent(&fake, "complete").is_empty() && sent(&fake, "cancel").is_empty());
}

/// **V1-ACCT2 B1 (V1-ACCT-I B1)**: the account pool a frame draws moves only
/// at a tick. The fake publishes gamma in answer to `complete`; the test
/// waits on THAT EVENT — gamma in the seam's mirror, folded on the
/// connection's thread — never on time, and does not tick. Until it does,
/// every read the face makes (the roster, the rows drawn, `dash_rows`) sees
/// the two-account pool; one frame drawn then holds `view.rs`'s "drawn rows
/// and App::dash_rows disagree" check; the tick brings gamma in.
#[test]
fn b1_a_frame_reads_one_pool_while_the_mirror_moves_under_it() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![login_conn(true)]));
    let mut a = face(fake.port);
    let mut frames = Vec::new();
    pump(&mut a, &mut frames, "two accounts", roster_of(2));
    add_account(&mut a, "gamma");
    pump(&mut a, &mut frames, "the login's address", awaiting);
    let rows = a.dash_rows().len();
    typed(&mut a, "some-code");
    a.handle(key_enter());
    let t0 = Instant::now();
    while !a.live.mirror_pool().is_some_and(|p| p.board.cards.iter().any(|c| c.label == GAMMA)) {
        assert!(t0.elapsed() < Duration::from_secs(5), "the fake never published gamma");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(a.roster_keys().len(), 2, "the pool moved between two ticks: {:?}", a.roster_keys());
    assert_eq!(a.dash_rows().len(), rows, "the rows moved between two ticks");
    let _ = screen(&mut a);
    assert_eq!(a.dash_rows().len(), rows);
    threads::tick(&mut a);
    assert_eq!(a.roster_keys().len(), 3, "the tick brings gamma in");
    assert_eq!(a.dash_rows().len(), rows + 1, "and its row");
}

// ── G6: the stand-in daemon's login is inert; his ports are refused ───────

/// **G6**: a face is pointed at a stand-in daemon only — the helper refuses
/// each of his ports — and a stand-in that refuses every login step (as
/// `examples/fake_daemon.rs` does for `cauldron-build`) leaves the login
/// refused, nothing listed, and no request anywhere but the stand-in.
#[test]
fn g6_a_face_is_never_pointed_at_his_ports_and_the_stand_ins_login_is_refused() {
    for p in live::PROD_PORTS {
        let r = Live::start_pointed(p, scratch("home").to_string_lossy().into_owned());
        assert!(r.as_ref().err().is_some_and(|e| e.contains("refused")), "port {p} was not refused");
    }
    let rt = rt();
    let inert = "this is a stand-in daemon: logging in is inert here, and no account is reached";
    let fake = rt.block_on(Fake::start(vec![two().refuse("begin", "refused", inert).refuse("complete", "refused", inert).refuse("cancel", "refused", inert)]));
    assert!(!live::PROD_PORTS.contains(&fake.port), "sentinel: the stand-in is not on one of his ports");
    let mut a = face(fake.port);
    let mut frames = Vec::new();
    pump(&mut a, &mut frames, "two accounts", roster_of(2));
    a.start_act(Act::LogIn { key: ALPHA.into() });
    pump(&mut a, &mut frames, "the refusal", |a| matches!(login_stage(a), Some(LoginStage::Done { ok: false, .. })));
    assert!(screen(&mut a).contains("logging in is inert"), "{}", screen(&mut a));
    assert_eq!(a.roster_keys().len(), 2);
    assert_eq!(fake.connections(), 1, "one connection, to the stand-in");
}
