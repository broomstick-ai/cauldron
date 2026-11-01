//! **V1.5 — STATUS › TOKENS, REAL.** Claude Code's band draws the daemon's
//! account pool off the cauldron face's ONE connection (`cauldron::live`):
//! the standing `accounts` and `threads/non-idle` mirrors the seam already
//! holds, folded by the pool board's own `pool::model::project`.
//!
//! Every test drives a REAL socket — `fake::Fake` on an ephemeral loopback
//! port, pushing the producer's frame shapes — with a scratch HOME; the
//! helpers refuse his live ports and his real HOME before anything dials.
//!
//! Guards (its validation plan): G1 real accounts · G2 pressure verbatim · G3 meters from
//! `gauge` · G4 no gauge-history · G5 one connection · G6 loading, never
//! absent · G7 recheck_failed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use env::paths::Source;
use env::Environment;
use serde_json::{json, Value};
use tui::cauldron::app::{App, CTab};
use tui::cauldron::live::{self, Live};
use tui::cauldron::render_to_text;
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::seed_dir;
use tui::cauldron::threads;

use crate::fake::{Conn, Fake};

const AT: &str = "2026-10-31T11:00:00Z";
/// Far enough ahead that a reset never passes during a run.
const LATER: &str = "2030-01-01T00:00:00Z";

// ── the harness ───────────────────────────────────────────────────────────

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "tokens-live-{tag}-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn checked(port: u16, home: &Path) -> Result<(), String> {
    if live::PROD_PORTS.contains(&port) {
        return Err(format!("refused: port {port} is one of his live daemons"));
    }
    if dirs::home_dir().is_some_and(|h| h == home) {
        return Err(format!("refused: {} is his real HOME", home.display()));
    }
    Ok(())
}

fn env_in(root: &Path) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") }
}

/// The cauldron face on `tab`, its one connection pointed at `port`. Opened
/// over the FIXTURE seed on purpose: its `global.json` carries three made-up
/// accounts, so a Tokens that still read the seed would show them.
fn face_on(port: u16, tab: CTab) -> App {
    let home = scratch("home");
    checked(port, &home).expect("a test face is pointed at a fake, with a scratch HOME");
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env_in(&scratch("root")), seed_dir(), false).expect("the face opens");
    a.set_ctab(tab);
    a.set_sttab(StatusTab::Tokens);
    let live = Live::start_pointed(port, home.to_string_lossy().into_owned()).expect("pointed at the fake");
    a.attach_live(live);
    a
}

fn face(port: u16) -> App {
    face_on(port, CTab::Status)
}

/// **A BACKSTOP, NOT A VERDICT** (V1-ACCT3, V1-ACCT2-I on `f1`). [`pump`]
/// returns the moment the face shows what it waits for: that event is the
/// wait. This bound only stops a test that would otherwise hang. At 5 s it
/// measured the scheduler: `f1` failed at load ~24 with the face honestly
/// `connecting to the threads daemon…`, two channel hops from `Connected`.
/// It sits above the seam's own `CONNECT_TIMEOUT` and first backoff, so
/// a run that needs a retry still finishes inside it.
const PUMP_BACKSTOP: Duration = Duration::from_secs(30);

fn pump(a: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let t0 = Instant::now();
    loop {
        threads::tick(a);
        if done(a) {
            return;
        }
        assert!(t0.elapsed() < PUMP_BACKSTOP, "timed out after {:?} waiting for {what}:\n{}", t0.elapsed(), screen(a));
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn idle(a: &mut App, d: Duration) {
    let t0 = Instant::now();
    while t0.elapsed() < d {
        threads::tick(a);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 80).unwrap().join("\n")
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

/// The pool is in: known, every named account has its row, and `n` cards.
fn cards_in(n: usize) -> impl Fn(&App) -> bool {
    move |a: &App| a.live.is_up() && a.live.pool().is_some_and(|p| p.known && p.loading.is_empty() && p.board.cards.len() == n)
}

/// The Claude Code band's lines: from its band line to the next band at the
/// left margin (Codex's).
fn claude_band(s: &str) -> Vec<String> {
    let lines: Vec<&str> = s.lines().collect();
    let Some(at) = lines.iter().position(|l| l.contains("Claude Code") && (l.contains('▾') || l.contains('▸'))) else {
        return Vec::new();
    };
    let end = lines[at + 1..].iter().position(|l| l.contains("Codex")).map_or(lines.len(), |e| at + 1 + e);
    lines[at..end].iter().map(|l| l.to_string()).collect()
}

/// The lines of account `name`'s card: its header to the card's last line.
fn card(s: &str, name: &str) -> Vec<String> {
    let band = claude_band(s);
    let Some(at) = band.iter().position(|l| l.contains(&format!("• {name}"))) else {
        return Vec::new();
    };
    let mut out = vec![band[at].clone()];
    for l in &band[at + 1..] {
        out.push(l.clone());
        if l.contains('╰') || l.contains('└') {
            break;
        }
    }
    out
}

/// The body row labelled `label` — never the card's HEADER line, whose
/// metadata also carries `session 23%` (a G3 probe reading the header passed
/// with the axes swapped: measured, forcing batch A).
fn row_of<'a>(card: &'a [String], label: &str) -> &'a str {
    card.iter().skip(1).find(|l| l.split_whitespace().any(|w| w == label)).map(String::as_str).unwrap_or_else(|| panic!("no `{label}` row in:\n{}", card.join("\n")))
}

// ── the producer's frames (`lib/threads/src/shapes/accounts.rs::AccountRow`) ──

fn known() -> Value {
    json!({"state": "known", "complete": true, "folded_at": AT})
}

fn epoch() -> Value {
    json!({"init_at": AT, "reinit": 0, "lost": 0, "state": "known"})
}

/// One `accounts` export: `axes` as `(axis, observed_pct)`, `pressure` the
/// cell's whole value, `credential` likewise.
fn account(name: &str, axes: &[(&str, f64)], pressure: Value, credential: Value) -> Value {
    let gauge: serde_json::Map<String, Value> =
        axes.iter().map(|(ax, pct)| (ax.to_string(), json!({"observed_pct": pct, "resets_at": LATER, "confidence": 1.0}))).collect();
    let cells: serde_json::Map<String, Value> =
        ["registered", "share", "disabled", "gauge", "limit", "health", "credential", "pressure"].iter().map(|c| (c.to_string(), known())).collect();
    json!({
        "type": "shape", "shape": "accounts", "key": name, "op": "read", "as_of": AT, "epoch": epoch(),
        "cells": cells,
        "row": {
            "account": name, "registered": true,
            "share": {"share": 0.5},
            "disabled": {"disabled": false},
            "gauge": gauge,
            "limit": {"until": null},
            "health": {"dead_since": null},
            "credential": credential,
            "pressure": pressure,
        },
    })
}

fn live_login() -> Value {
    json!({"slot_state": "live", "is_live_login": false})
}

/// The membership frame: the pool's accounts, and nothing else.
fn members(keys: &[&str]) -> Value {
    json!({"type": "shape", "shape": "accounts", "op": "read", "as_of": AT, "epoch": epoch(),
        "members": {"state": "known", "keys": keys, "complete": true}})
}

/// `threads/non-idle`: `(id, account, executing)`.
fn non_idle(placed: &[(&str, &str, bool)]) -> Value {
    let threads: Vec<Value> = placed
        .iter()
        .map(|(id, acct, ex)| json!({"id": id, "account": acct, "state": if *ex { "active" } else { "parked" }, "executing": ex, "queued": 0, "since": AT}))
        .collect();
    json!({"type": "shape", "shape": "threads/non-idle", "key": "fleet", "op": "read", "as_of": AT, "epoch": epoch(),
        "cells": {"threads": known()}, "row": {"pool": "fleet", "threads": threads}})
}

const ALPHA_AXES: &[(&str, f64)] = &[("session", 23.0), ("weekly-all", 41.0), ("weekly-fable", 12.0)];
/// No `weekly-fable` reading: the fable meter is UNKNOWN (G3's opposition).
const BETA_AXES: &[(&str, f64)] = &[("session", 67.0), ("weekly-all", 88.0)];

fn alpha(axes: &[(&str, f64)]) -> Value {
    account("alpha", axes, json!({"pressure": 1.7, "share": 0.5}), live_login())
}

fn beta(credential: Value) -> Value {
    account("beta", BETA_AXES, json!({"pressure": null, "infinite": true, "share": 0.5}), credential)
}

/// alpha and beta, alpha running one thread.
fn two() -> Conn {
    Conn::default().push(members(&["alpha", "beta"])).push(alpha(ALPHA_AXES)).push(beta(live_login())).push(non_idle(&[("t1", "alpha", true)]))
}

/// The fixture seed's made-up accounts — what a Tokens reading the seed
/// would draw. Sentinel: there are some, so their absence is a finding.
fn seed_names(a: &App) -> Vec<String> {
    let names: Vec<String> = a.dash.threads.accounts.iter().map(|x| x.label.clone()).collect();
    assert!(names.len() >= 2, "sentinel: the fixture seed carries accounts to look for: {names:?}");
    names
}

// ── G1: real accounts ─────────────────────────────────────────────────────

/// **G1**: the fake serves `alpha` and `beta`; Tokens shows exactly those two
/// cards and none of the seed's.
#[test]
fn g1_tokens_shows_exactly_the_daemons_two_accounts_and_none_of_the_seeds() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![two()]));
    let mut a = face(fake.port);
    pump(&mut a, "two cards", cards_in(2));
    let s = screen(&mut a);
    let band = claude_band(&s);
    let cards: Vec<&String> = band.iter().filter(|l| l.contains("• ")).collect();
    assert_eq!(cards.len(), 2, "exactly two cards:\n{}", band.join("\n"));
    assert!(!card(&s, "alpha").is_empty() && !card(&s, "beta").is_empty(), "{s}");
    for n in seed_names(&a) {
        assert!(!s.contains(&n), "the seed's account `{n}` is on Tokens:\n{s}");
    }
}

/// **G1's opposition**: the fake serving ZERO accounts (a complete, empty
/// membership) shows zero cards — and still none of the seed's.
#[test]
fn g1_zero_accounts_served_draws_zero_cards_and_no_seed_account() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![Conn::default().push(members(&[]))]));
    let mut a = face(fake.port);
    pump(&mut a, "the empty pool", |a| a.live.pool().is_some_and(|p| p.known));
    let s = screen(&mut a);
    let band = claude_band(&s);
    assert!(!band.is_empty(), "sentinel: the Claude Code band is drawn:\n{s}");
    assert!(!band.iter().any(|l| l.contains("• ")), "a card with no account served:\n{}", band.join("\n"));
    assert!(band.iter().any(|l| l.contains("holds no accounts")), "{}", band.join("\n"));
    for n in seed_names(&a) {
        assert!(!s.contains(&n), "the seed's account `{n}` is on Tokens:\n{s}");
    }
}

// ── G2: pressure verbatim ─────────────────────────────────────────────────

fn pressure_row(s: &str, name: &str) -> String {
    row_of(&card(s, name), "pressure").to_string()
}

/// **G2**: `alpha.pressure = 1.7`, `beta.pressure = {infinite: true}`; the
/// cards read `1.7` and `∞`.
#[test]
fn g2_the_cards_read_the_daemons_pressure_1_7_and_infinity() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![two()]));
    let mut a = face(fake.port);
    pump(&mut a, "two cards", cards_in(2));
    let s = screen(&mut a);
    let (pa, pb) = (pressure_row(&s, "alpha"), pressure_row(&s, "beta"));
    assert!(pa.split_whitespace().any(|w| w == "1.7"), "alpha: {pa}");
    assert!(pb.split_whitespace().any(|w| w == "∞"), "beta: {pb}");
}

/// **G2's opposition**: changing ONLY alpha's gauge (its weekly from 41 % to
/// 95 %, its `pressure` cell untouched) does not change the printed pressure
/// — while the weekly meter it would be worked out from did change.
#[test]
fn g2_changing_only_the_gauge_does_not_change_the_printed_pressure() {
    let rt = rt();
    let moved: &[(&str, f64)] = &[("session", 23.0), ("weekly-all", 95.0), ("weekly-fable", 12.0)];
    let mut seen = Vec::new();
    for axes in [ALPHA_AXES, moved] {
        let fake = rt.block_on(Fake::start(vec![Conn::default().push(members(&["alpha"])).push(alpha(axes))]));
        let mut a = face(fake.port);
        pump(&mut a, "alpha", cards_in(1));
        let s = screen(&mut a);
        seen.push((pressure_row(&s, "alpha"), row_of(&card(&s, "alpha"), "weekly").to_string()));
    }
    assert_ne!(seen[0].1, seen[1].1, "sentinel: the weekly meter moved");
    assert!(seen[1].1.contains("95%"), "{:?}", seen[1]);
    assert_eq!(seen[0].0, seen[1].0, "the printed pressure moved with the gauge");
}

// ── G3: meters from `gauge` ───────────────────────────────────────────────

/// **G3**: each card's session, weekly and fable meters are the fake's
/// `gauge` axes — two distinct readings per card.
#[test]
fn g3_session_weekly_and_fable_are_the_gauge_axes() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![two()]));
    let mut a = face(fake.port);
    pump(&mut a, "two cards", cards_in(2));
    let s = screen(&mut a);
    let (ca, cb) = (card(&s, "alpha"), card(&s, "beta"));
    for (c, label, pct) in [(&ca, "session", "23%"), (&ca, "weekly", "41%"), (&ca, "fable", "12%"), (&cb, "session", "67%"), (&cb, "weekly", "88%")] {
        let r = row_of(c, label);
        assert!(r.contains(pct), "{label} is not {pct}: {r}\n{}", c.join("\n"));
    }
}

/// **G3's opposition**: beta has no `weekly-fable` reading — its fable meter
/// draws as unknown, never as 0.
#[test]
fn g3_a_missing_fable_axis_draws_unknown_not_zero() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![two()]));
    let mut a = face(fake.port);
    pump(&mut a, "two cards", cards_in(2));
    let s = screen(&mut a);
    let r = row_of(&card(&s, "beta"), "fable").to_string();
    assert!(r.contains("no reading") && r.contains('—'), "{r}");
    assert!(!r.contains("0%"), "the missing axis reads 0: {r}");
}

// ── G4: no gauge-history ──────────────────────────────────────────────────

/// **G4**: opening Tokens and waiting one debounce (100 ms) plus one frame
/// (16 ms) — then five times that — no `accounts/gauge-history` subscribe was
/// sent. Opposition in the same run: the `accounts` subscribe WAS logged, so
/// the log can see subscribes.
#[test]
fn g4_tokens_never_subscribes_gauge_history_past_the_100ms_debounce_and_a_16ms_frame() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![two()]));
    let mut a = face(fake.port);
    pump(&mut a, "two cards", cards_in(2));
    screen(&mut a);
    idle(&mut a, (threads::DRAFT_DEBOUNCE + tui::app::FRAME_INTERVAL) * 5);
    screen(&mut a);
    let methods: Vec<String> = fake.log().into_iter().map(|r| r.method).collect();
    assert!(methods.iter().any(|m| m == "shapes/accounts/subscribe"), "the log saw no accounts subscribe: {methods:#?}");
    assert!(!methods.iter().any(|m| m.contains("gauge-history")), "gauge-history was subscribed: {methods:#?}");
}

// ── G5: one connection ────────────────────────────────────────────────────

/// **G5**: Threads, then Tokens: the fake sees exactly ONE connection
/// (one `welcome`) and Tokens fills off it.
#[test]
fn g5_threads_then_tokens_is_one_connection() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![two()]));
    let mut a = face_on(fake.port, CTab::Threads);
    a.start_live();
    screen(&mut a);
    idle(&mut a, Duration::from_millis(50));
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Tokens);
    assert!(a.shows_live(), "Tokens reads the daemon");
    a.start_live();
    pump(&mut a, "two cards", cards_in(2));
    screen(&mut a);
    idle(&mut a, Duration::from_millis(100));
    assert_eq!(fake.connections(), 1, "{:#?}", fake.log());
}

/// **G5's opposition**: the daemon dropping the connection makes the face
/// connect again — a second connection the same count sees.
#[test]
fn g5_a_dropped_connection_is_counted_as_a_second() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![two()]));
    let mut a = face(fake.port);
    pump(&mut a, "two cards", cards_in(2));
    assert_eq!(fake.connections(), 1);
    fake.close_current();
    rt.block_on(fake.until_connections(2));
    pump(&mut a, "the pool back", cards_in(2));
    assert_eq!(fake.connections(), 2);
}

// ── G6: loading, never absent ─────────────────────────────────────────────

/// **G6**: before the first `accounts` export — the daemon has named alpha
/// and beta and sent no row — the cards draw LOADING: no line of the band
/// reads `absent`, and no number reads `0`.
#[test]
fn g6_before_the_first_export_the_cards_are_loading_never_absent_or_zero() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![Conn::default().push(members(&["alpha", "beta"]))]));
    let mut a = face(fake.port);
    pump(&mut a, "the names", |a| a.live.pool().is_some_and(|p| p.loading.len() == 2));
    let s = screen(&mut a);
    let band = claude_band(&s);
    assert_eq!(band.iter().filter(|l| l.contains("• ")).count(), 2, "{}", band.join("\n"));
    assert!(band.iter().filter(|l| l.contains("loading")).count() >= 2, "{}", band.join("\n"));
    for l in &band {
        assert!(!l.contains("absent"), "{l}");
        assert!(!l.split(|c: char| !c.is_ascii_digit() && c != '%').any(|w| w == "0" || w == "0%"), "a 0 before the export: {l}");
    }
}

/// **G6, before ANYTHING**: connected, nothing exported — the band says it
/// is loading the pool, not that it is empty.
#[test]
fn g6_before_any_frame_the_band_says_loading_not_empty() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![Conn::default()]));
    let mut a = face(fake.port);
    pump(&mut a, "the link", |a| a.live.is_up());
    idle(&mut a, Duration::from_millis(150));
    let s = screen(&mut a);
    let band = claude_band(&s).join("\n");
    assert!(band.contains("loading the account pool"), "{band}");
    assert!(!band.contains("holds no accounts") && !band.contains("0 of"), "{band}");
}

/// **G6's opposition**: after the export, the numbers show.
#[test]
fn g6_after_the_export_the_numbers_show() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![two()]));
    let mut a = face(fake.port);
    // Wait on BOTH events this asserts (V1-ACCT2): the `accounts` cards, and
    // the `threads/non-idle` push after them that homes t1 on alpha — the
    // `running` row's source. Waiting on the cards alone raced that push.
    pump(&mut a, "two cards and alpha's running thread", |a| {
        cards_in(2)(a) && a.live.pool().is_some_and(|p| p.board.cards.iter().any(|c| c.label == "alpha" && !c.homed.is_empty()))
    });
    let s = screen(&mut a);
    let band = claude_band(&s).join("\n");
    assert!(!band.contains("loading"), "{band}");
    // V1-CLEAN1 (V1.5-I F2): `A && B || C` let any `2 of 2` pass without
    // the 23 %. Both are required now, and the count is exact: alpha and
    // beta are both enabled, logged in and held by no `limit` — both open.
    assert!(band.contains("23%"), "{band}");
    assert!(band.contains("2 of 2 accounts open"), "{band}");
    assert!(row_of(&card(&s, "alpha"), "running").contains('1'), "{band}");
}

// ── G7: recheck_failed ────────────────────────────────────────────────────

/// **G7**: the fake marks `beta.credential` `{state: recheck_failed, op,
/// reason}`; beta's card reads `recheck failed: <reason>` and is drawn
/// UNAVAILABLE. Opposition in the same frame: alpha, unmarked, reads its real
/// credential state.
#[test]
fn g7_a_recheck_failed_credential_reads_its_reason_and_the_account_is_unavailable() {
    const WHY: &str = "token refresh refused";
    let marked = json!({"state": "recheck_failed", "op": "recheck", "op_id": 7, "started_at": AT, "failed_at": AT, "reason": WHY});
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![Conn::default().push(members(&["alpha", "beta"])).push(alpha(ALPHA_AXES)).push(beta(marked))]));
    let mut a = face(fake.port);
    pump(&mut a, "two cards", cards_in(2));
    let s = screen(&mut a);
    let cb = card(&s, "beta");
    assert!(row_of(&cb, "login").contains(&format!("recheck failed: {WHY}")), "{}", cb.join("\n"));
    assert!(cb[0].contains("UNAVAILABLE"), "beta is not drawn unavailable: {}", cb[0]);
    let ca = card(&s, "alpha");
    assert!(row_of(&ca, "login").contains("login ok"), "{}", ca.join("\n"));
    assert!(!ca[0].contains("UNAVAILABLE"), "{}", ca[0]);
    let band = claude_band(&s).join("\n");
    assert!(band.contains("1 of 2 accounts open"), "beta counted open:\n{band}");
}

// ── no connection at all ──────────────────────────────────────────────────

/// A face no launch opened a door for dials nothing for Tokens either, and
/// the band says why rather than drawing an empty pool.
#[test]
fn tokens_with_no_door_dials_nothing_and_says_why() {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env_in(&scratch("root")), seed_dir(), false).expect("the face opens");
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Tokens);
    a.start_live();
    assert!(!a.live.is_started());
    let s = screen(&mut a);
    let band = claude_band(&s).join("\n");
    assert!(band.contains("no threads endpoint") && band.contains("not connected"), "{band}");
    assert!(!band.contains("• "), "{band}");
}

/// **A LINK THAT NEVER CAME UP IS NOT `loading`.** Pointed at port 1, where
/// nothing listens, the band says the daemon is down (or still connecting) —
/// a refused or failed dial once read `loading the account pool…` forever
/// (measured: V1.5's first frame, whose dial was refused). Opposition: G6's
/// `before_any_frame` test, link UP, reads `loading`.
#[test]
fn tokens_with_the_link_down_says_so_not_loading() {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env_in(&scratch("root")), seed_dir(), false).expect("the face opens");
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Tokens);
    let live = Live::start_pointed(1, scratch("home").to_string_lossy().into_owned()).expect("port 1 is admitted");
    a.attach_live(live);
    pump(&mut a, "the dial to fail", |a| a.live.is_down());
    let band = claude_band(&screen(&mut a)).join("\n");
    assert!(band.contains("threads is down"), "{band}");
    assert!(!band.contains("loading"), "{band}");
}

// ── V1-CLEAN1: a title is never an email; AT LIMIT is the daemon's ────────

/// An `accounts` export with its `limit` cell held until `until`.
fn limited(mut v: Value, until: &str) -> Value {
    v["row"]["limit"] = json!({"until": until});
    v
}

/// Made-up addresses (example.com only), their local parts and first domain
/// labels distinctive enough that ANY drawn form of them — raw, slugged,
/// cropped, re-cased — is found by [`leaks`]. `B` is mixed case with
/// punctuation in its local part, so a slug or a lower-casing is caught too.
const ADDR: &str = "zqxwvu@jkplm.example.com";
const ADDR_B: &str = "Hv.Kqzj+t@wrlfd.example.com";
/// Added by hand on Config › Accounts — the account list's own path.
const ADDR_C: &str = "Pxqrnw@tbvyk.example.com";

/// Every part of `addr` beyond each side's FIRST letter that `s` shows. The
/// screen and the address are both cut to lower-case letters and digits, so
/// `qmzrtb-vxnhwk-example`, `QMZRTB` and `qmzrtb@…` are one finding; each
/// side is searched in every 3-character window after its first letter. The
/// domain's own first label is searched that way; its `example.com` tail is
/// searched whole (its windows, `com` and `amp`, are English).
fn leaks(s: &str, addr: &str) -> Vec<String> {
    let norm = |x: &str| x.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
    let flat = norm(s);
    let (local, domain) = addr.split_once('@').expect("an address");
    let (label, rest) = domain.split_once('.').expect("a dotted domain");
    let mut found = Vec::new();
    for side in [local, label] {
        let tail: Vec<char> = norm(side).chars().skip(1).collect();
        for w in tail.windows(3) {
            let w: String = w.iter().collect();
            if flat.contains(&w) {
                found.push(w);
            }
        }
    }
    if flat.contains(&norm(&format!("{label}.{rest}"))[1..]) || s.to_ascii_lowercase().contains(&rest.to_ascii_lowercase()) {
        found.push(rest.to_string());
    }
    found
}

/// The masked form each address must be drawn in.
fn masked(addr: &str) -> String {
    tui::pool::model::shown_name(addr)
}

/// The face on `tab` over a scratch root that PERSISTS (Config's add path
/// writes `accounts.json` there, and nowhere else).
fn face_persisting(port: u16) -> App {
    let home = scratch("home");
    checked(port, &home).expect("a test face is pointed at a fake, with a scratch HOME");
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env_in(&scratch("root")), seed_dir(), true).expect("the face opens");
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Tokens);
    a.attach_live(Live::start_pointed(port, home.to_string_lossy().into_owned()).expect("pointed at the fake"));
    a
}

fn key(code: ratatui::crossterm::event::KeyCode) -> ratatui::crossterm::event::Event {
    ratatui::crossterm::event::Event::Key(ratatui::crossterm::event::KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::NONE))
}

/// **F1 (V1.5-I), WIDENED BY V1-CLEAN2 (V1-CLEAN1-I B1)**: after R12.2 the
/// `accounts` row key IS the account's email. EVERY surface that draws an
/// account is driven with made-up email-keyed accounts, and no screen shows
/// any part of either side of an address beyond its first letter:
///
/// - Status › Tokens, every card row and `config ▸` selected in turn;
/// - ENTER on each card's `config ▸` — Config › Accounts with the account's
///   modal open, titled `Claude Code · z•••@j•••` (B1 measured the slug
///   `qmzrtb-vxnhwk-example` there);
/// - Config › Accounts after an address is ADDED there: its row, the
///   `added …` notice, its modal, and its rename and remove prompts;
/// - the pool board (`tui pool` and its `--dump`, one renderer): card titles,
///   `machine login is …`, the WIPED warning, the disable confirm and the
///   share prompt;
/// - a note quoting the key back (a daemon's refusal, a tool's stderr).
///
/// Opposition in the same frames: `alpha`, not an address, is drawn as it is.
#[test]
fn f1_every_surface_that_draws_an_account_masks_an_address() {
    use ratatui::crossterm::event::KeyCode;
    use tui::cauldron::app::{Focus, Modal};
    use tui::cauldron::menu::SPage;
    let rt = rt();
    let mail = account(ADDR, ALPHA_AXES, json!({"pressure": 0.9, "share": 0.5}), live_login());
    let mail_b = account(ADDR_B, BETA_AXES, json!({"pressure": 0.4, "share": 0.5}), live_login());
    let fake = rt.block_on(Fake::start(vec![Conn::default().push(members(&[ADDR, ADDR_B, "alpha"])).push(mail).push(mail_b).push(alpha(ALPHA_AXES))]));
    let mut a = face_persisting(fake.port);
    pump(&mut a, "three cards", cards_in(3));
    assert!(a.live.pool().unwrap().board.cards.iter().any(|c| c.label == ADDR), "sentinel: the daemon's key IS the address");
    let mut screens: Vec<(String, String)> = Vec::new();
    fn shot(screens: &mut Vec<(String, String)>, a: &mut App, what: String) {
        let s = screen(a);
        screens.push((what, s));
    }

    // Status › Tokens: every card and every `config ▸`, selected in turn.
    shot(&mut screens, &mut a, "Status › Tokens".into());
    let band = claude_band(&screens[0].1).join("\n");
    assert!(band.contains(&format!("• {}", masked(ADDR))) && band.contains(&format!("• {}", masked(ADDR_B))), "the masked titles:\n{band}");
    assert!(band.contains("• alpha "), "a name that is not an address is drawn as it is:\n{band}");
    let v = tui::cauldron::status::view(&a, 120);
    let rows: Vec<(usize, String)> = v.items.iter().enumerate().filter(|(_, it)| it.key.starts_with("acct:") || it.key.starts_with("cfg:")).map(|(i, it)| (i, it.key.clone())).collect();
    assert!(rows.iter().filter(|(_, k)| k.starts_with("cfg:")).count() == 3, "sentinel: a `config ▸` per card: {rows:?}");
    for (i, k) in &rows {
        a.rows[0] = *i;
        shot(&mut screens, &mut a, format!("Status › Tokens, {k} selected"));
    }

    // ENTER on each card's `config ▸`: the account's page on Config.
    let mut entered = 0;
    for (i, k) in rows.iter().filter(|(_, k)| k.starts_with("cfg:")) {
        a.set_ctab(CTab::Status);
        a.set_sttab(StatusTab::Tokens);
        a.modal = None;
        a.rows[0] = *i;
        a.handle(key(KeyCode::Enter));
        let Some(Modal::Settings { page: SPage::Account(_, id), .. }) = a.modal.clone() else { panic!("Enter on {k} opened no account page: {:?}", a.modal) };
        let s = screen(&mut a);
        for addr in [ADDR, ADDR_B] {
            if account_id_of(addr) == id {
                let title = format!("Claude Code · {}", masked(addr));
                assert!(s.contains(&title), "Enter on {k}: the page is titled `{title}`:\n{s}");
                entered += 1;
            }
        }
        screens.push((format!("Enter on {k}"), s));
        a.handle(key(KeyCode::Esc));
        shot(&mut screens, &mut a, format!("Config › Accounts after Enter on {k}"));
    }
    assert_eq!(entered, 2, "sentinel: both address cards were entered");

    // Config › Accounts: `+ add account` on Claude Code. V1.6 (merged by
    // V1-ACCT): a Claude Code account is added by LOGGING IT IN through the
    // daemon, never recorded here — so the address is drawn in the login
    // modal's title, masked.
    a.set_ctab(CTab::Accounts);
    a.modal = Some(Modal::AddAccount { pool: "claude_code".into(), name: ADDR_C.into(), error: None });
    a.handle(key(KeyCode::Enter));
    assert!(matches!(&a.modal, Some(Modal::Login { title, .. }) if *title == format!("Log in {}", masked(ADDR_C))), "Claude Code's add is a masked login: {:?}", a.modal);
    shot(&mut screens, &mut a, "Config › Accounts, Claude Code's login modal".into());
    a.modal = None;
    // No other source takes an account added here in this product (Codex is
    // greyed: Claude Code is the only inference source), so CLEAN1's "added
    // here" surfaces are now a daemon account's: its page, row, and its
    // rename and remove prompts.
    let id_a = account_id_of(ADDR);
    assert!(a.accounts("claude_code").iter().any(|x| x.id == id_a), "sentinel: the daemon's account is listed");
    a.open_account_settings("claude_code", &id_a);
    let s = screen(&mut a);
    assert!(s.contains(&format!("Claude Code · {}", masked(ADDR))), "the account's page title:\n{s}");
    screens.push(("Config › Accounts, a daemon account's modal".into(), s));
    a.modal = None;
    a.focus = Focus::Content;
    shot(&mut screens, &mut a, "Config › Accounts, the daemon account's row".into());
    for act in [tui::cauldron::manage::Act::RenameAccount { pool: "claude_code".into(), id: id_a.clone() }, tui::cauldron::manage::Act::RemoveAccount { pool: "claude_code".into(), id: id_a.clone() }] {
        a.modal = None;
        a.start_act(act.clone());
        assert!(a.modal.is_some(), "{act:?} asked nothing");
        shot(&mut screens, &mut a, format!("{act:?}'s prompt"));
    }

    // The pool board, and its `--dump` (the same renderer), over the same keys.
    let mut pb = tui::pool::app::App::new("ws://127.0.0.1:0/ws", chrono::Utc::now());
    let mut wiped = account(ADDR_B, BETA_AXES, json!({"pressure": 0.4, "share": 0.5}), json!({"slot_state": "wiped", "is_live_login": false}));
    let mut first = account(ADDR, ALPHA_AXES, json!({"pressure": 0.9, "share": 0.5}), json!({"slot_state": "live", "is_live_login": true}));
    first["shared"] = json!({"warn_below_days": 7, "machine_login": {"hosted_by": ADDR, "unhosted": false}});
    wiped["shared"] = first["shared"].clone();
    for f in [first, wiped, alpha(ALPHA_AXES), non_idle(&[])] {
        assert!(pb.on_frame(&f), "frame not folded: {f}");
    }
    pb.link = tui::pool::app::LinkState::Live;
    let board = |pb: &tui::pool::app::App| tui::pool::render_to_text(pb, 120, 30).unwrap().join("\n");
    let s = board(&pb);
    assert!(s.contains(&format!("machine login is {}", masked(ADDR))) && s.contains(&format!("✕ {} login was WIPED", masked(ADDR_B))), "sentinel: the board names both:\n{s}");
    screens.push(("pool board".into(), s));
    for p in [tui::pool::app::Prompt::Confirm(tui::pool::action::Action::Disable(ADDR.into())), tui::pool::app::Prompt::Share { label: ADDR_B.into(), buf: "40".into() }] {
        pb.prompt = p;
        screens.push(("pool board prompt".into(), board(&pb)));
    }
    pb.prompt = tui::pool::app::Prompt::None;
    pb.set_note(tui::pool::model::masked_in(&format!("accounts/{ADDR}/disable refused: no account {ADDR}"), ADDR), tui::pool::model::Tone::Bad);
    screens.push(("pool board note".into(), board(&pb)));

    for (what, s) in &screens {
        for addr in [ADDR, ADDR_B, ADDR_C] {
            let l = leaks(s, addr);
            assert!(l.is_empty(), "{what}: {l:?} of `{}` is on screen:\n{s}", masked(addr));
        }
        assert!(s.matches('@').count() == s.matches("•••@").count(), "{what}: an `@` not in a masked name:\n{s}");
    }
}

/// The id the face gives account `key` (what `config ▸` opens).
pub fn account_id_of(key: &str) -> String {
    tui::cauldron::menu::account_id(key)
}

/// **F5 (V1.5-I)**: AT LIMIT is the daemon's `limit` cell. `full` reads
/// 100 % on its session meter with NO `limit` held: it is not AT LIMIT and
/// it counts open. `held` reads well under every axis with `limit.until`
/// set: it IS AT LIMIT and is shut. A face deriving the state from the
/// gauge fails both halves.
#[test]
fn f5_at_limit_is_the_daemons_limit_cell_never_a_full_gauge() {
    let rt = rt();
    let full = account("full", &[("session", 100.0), ("weekly-all", 50.0)], json!({"pressure": 0.5, "share": 0.5}), live_login());
    let held = limited(account("held", &[("session", 10.0), ("weekly-all", 20.0)], json!({"pressure": 0.5, "share": 0.5}), live_login()), LATER);
    let fake = rt.block_on(Fake::start(vec![Conn::default().push(members(&["full", "held"])).push(full).push(held)]));
    let mut a = face(fake.port);
    pump(&mut a, "two cards", cards_in(2));
    let s = screen(&mut a);
    let cf = card(&s, "full");
    assert!(row_of(&cf, "session").contains("100%"), "sentinel: full's meter reads 100 %:\n{}", cf.join("\n"));
    assert!(!cf.iter().any(|l| l.contains("AT LIMIT") || l.contains("at the limit")), "a full meter drawn as a limit:\n{}", cf.join("\n"));
    let ch = card(&s, "held");
    assert!(ch[0].contains("AT LIMIT"), "the daemon's limit not drawn: {}", ch[0]);
    assert!(row_of(&ch, "limit").contains("back in"), "{}", ch.join("\n"));
    let band = claude_band(&s).join("\n");
    assert!(band.contains("1 of 2 accounts open"), "full is open, held is shut:\n{band}");
}
