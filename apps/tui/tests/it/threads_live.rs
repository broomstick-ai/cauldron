//! **REL-6 — THE THREADS TAB, REAL.** The cauldron face's Threads tab reads
//! the threads daemon through the face's one connection
//! (`tui::cauldron::live`), on the chat face's seam, mirror and store.
//!
//! Every test here drives a REAL socket: `fake::Fake` on an ephemeral
//! loopback port, speaking the chassis door and pushing real shape frames
//! (the producer's frame shapes, as `src/mirror/fixtures.rs` records them),
//! with a scratch HOME. The helpers refuse his live ports and his real HOME
//! (G8) before anything is dialled.
//!
//! Guards (its validation plan): G1 real rows · G2 no Projects band · G3 send goes to the
//! daemon · G4 the render thread never waits · G5 daemon down · G6 the rename
//! · G7 one connection · G8 prod ports refused.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use serde_json::{json, Value};
use tui::cauldron::app::{App, CTab};
use tui::cauldron::live::{self, Live};
use tui::cauldron::render_to_text;
use tui::cauldron::store::seed_dir;
use tui::cauldron::threads::{self, Block, TFocus};

use crate::fake::{Conn, Fake};

const AT: &str = "2026-10-30T11:00:00Z";
const A: &str = "th-pinned-aaaa";
const B: &str = "th-recent-bbbb";
const A_NAME: &str = "the pinned thread from the index";
const B_NAME: &str = "the recent thread the daemon named";
/// Not a scratch directory (`store::in_scratch_dir` hides those from the
/// recents); nothing reads it — it is a string on a row.
const B_CWD: &str = "/Users/fixture/projects/bravo";

// ── the harness ───────────────────────────────────────────────────────────

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "threads-live-{tag}-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// A scratch HOME: one registered project whose index pins A.
fn home() -> PathBuf {
    let home = scratch("home");
    let root = home.join("work").join("alpha");
    std::fs::create_dir_all(root.join(".mind")).unwrap();
    std::fs::write(
        root.join(".mind").join("threads.json"),
        format!(
            r#"{{"threads":[{{"id":"{A}","session_id":"{A}","summary":"{A_NAME}","pinned":true,
                "last_turn_at":"2026-10-29T00:00:00Z","turn_count":2}}]}}"#
        ),
    )
    .unwrap();
    let mut reg = projects::Registry::empty();
    reg.register(&root, None).expect("registered");
    reg.save(&home).expect("saved");
    home
}

/// **G8: THE HELPERS REFUSE HIS LIVE PORTS AND HIS REAL HOME** before
/// anything is dialled.
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

/// The cauldron face on the Threads tab, its one connection pointed at
/// `port`, with HOME `home`.
fn face(port: u16, home: &Path) -> App {
    checked(port, home).expect("a test face is pointed at a fake, with a scratch HOME");
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env_in(&scratch("root")), seed_dir(), false).expect("the face opens");
    a.set_ctab(CTab::Threads);
    let live = Live::start_pointed(port, home.to_string_lossy().into_owned()).expect("pointed at the fake");
    a.attach_live(live);
    a
}

/// Tick the face (drain, settle, project) until `done`, for at most 5 s.
fn pump(a: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let t0 = Instant::now();
    loop {
        threads::tick(a);
        if done(a) {
            return;
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "timed out waiting for {what}:\n{}", screen(a));
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Tick the face for `d`, whatever arrives.
fn idle(a: &mut App, d: Duration) {
    let t0 = Instant::now();
    while t0.elapsed() < d {
        threads::tick(a);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 40).unwrap().join("\n")
}

fn key(a: &mut App, code: KeyCode) {
    a.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        key(a, KeyCode::Char(c));
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

/// The `send`s the fake saw, as `(thread, text)`.
fn sends(f: &Fake) -> Vec<(String, String)> {
    f.log()
        .into_iter()
        .filter(|r| r.leaf() == "send")
        .map(|r| (r.subject().unwrap_or_default().to_string(), r.params["text"].as_str().unwrap_or_default().to_string()))
        .collect()
}

// ── the producer's frames (src/mirror/fixtures.rs, with the wire's `type`) ─

fn known() -> Value {
    json!({"state": "known", "complete": true, "folded_at": AT})
}

fn export(shape: &str, key: &str, row: Value, cells: Value) -> Value {
    json!({
        "type": "shape", "shape": shape, "key": key, "op": "read", "as_of": AT,
        "epoch": {"init_at": AT, "reinit": 0, "lost": 0, "state": "known"},
        "cells": cells, "row": row,
    })
}

/// `threads/recent-updates`, newest first.
fn recent(ids: &[&str]) -> Value {
    let rows: Vec<Value> = ids.iter().map(|id| json!({"id": id, "at": AT, "unread": 0, "seen_seq": 0})).collect();
    export("threads/recent-updates", "fleet", json!({"pool": "fleet", "threads": rows}), json!({"threads": known()}))
}

/// One `threads` row: his app's thread, named by the daemon.
fn thread(id: &str, name: &str, cwd: &str, state: &str) -> Value {
    let cells: serde_json::Map<String, Value> = ["header", "settings", "account", "status", "status_transitions", "queued", "last_message_id", "seen_seq", "unread", "newest", "liveness", "app"]
        .iter()
        .map(|c| (c.to_string(), known()))
        .collect();
    export(
        "threads",
        id,
        json!({
            "thread_id": id,
            "header": {"cwd": cwd, "model": "sonnet", "created_at": AT, "updated_at": AT},
            "settings": {"app_id": "tui"},
            "account": null,
            "status": {"state": state, "executing": state == "active", "detail": null},
            "status_transitions": [],
            "queued": 0, "last_message_id": null, "seen_seq": 0, "unread": 0,
            "newest": {"seq": 2, "message_id": null, "kind": "assistant_text", "ts": AT},
            "body": null, "compaction": null,
            "liveness": {"busy": state == "active", "parent_running": false, "running_subagents": []},
            "app": {"summary_updated": {"summary": name, "by": "tui", "ts": AT}},
        }),
        Value::Object(cells),
    )
}

fn item(seq: u64, kind: &str, event: Value) -> Value {
    let mut ev = event;
    ev["kind"] = json!(kind);
    json!({"seq": seq, "kind": kind, "ts": AT, "payload": {"here": ev}})
}

/// A `split` item: the message's `text` lifted out behind the resource door.
fn split(seq: u64, kind: &str, id: &str, bytes: u64) -> Value {
    json!({"seq": seq, "kind": kind, "ts": AT,
        "payload": {"split": {"event": {"kind": kind}, "text": {"id": id, "bytes": bytes, "transports": ["ws"]}}}})
}

fn log(id: &str, items: Vec<Value>) -> Value {
    export("threads/log", id, json!({"thread_id": id, "events": items, "retained": 0}), json!({"events": known()}))
}

/// B's transcript: his question and the assistant's answer.
fn b_log() -> Value {
    log(B, vec![item(1, "user_message", json!({"text": "what is on the board"})), item(2, "assistant_text", json!({"text": "two open questions"}))])
}

/// A connection serving A and B (A pinned by the index, B in recents).
fn serving() -> Conn {
    Conn::default()
        .respond("send", json!({"accepted": true, "message_id": 3}))
        .push(recent(&[B, A]))
        .push(thread(A, A_NAME, B_CWD, "idle"))
        .push(thread(B, B_NAME, B_CWD, "idle"))
        .push(b_log())
}

/// The list's rows under band `band` (the lines after its band line, to the
/// next blank line).
fn band_rows(s: &str, band: &str) -> Vec<String> {
    let lines: Vec<&str> = s.lines().collect();
    let Some(at) = lines.iter().position(|l| l.contains(&format!(" {band} ")) && (l.contains('▾') || l.contains('▸'))) else {
        return Vec::new();
    };
    lines[at + 1..].iter().take_while(|l| l.trim_matches(|c| c == '│' || c == ' ').len() > 0).map(|l| l.to_string()).collect()
}

fn open_b(a: &mut App) {
    pump(a, "B on the list", |a| a.threads.rows.iter().any(|t| t.id == B));
    a.threads.open_thread(B);
    threads::settle(a);
    pump(a, "B's transcript", |a| a.threads.blocks().iter().any(|b| matches!(b, Block::Assistant { text, .. } if text.contains("two open questions"))));
}

// ── G1 G2: real rows, no Projects band ────────────────────────────────────

/// **G1**: with the fake serving A (pinned in the scratch root's index) and
/// B (a recent thread of his app), Pinned holds A and Recent holds B — off
/// the socket, through the chat face's store. **G2**: no line says
/// `Projects`; Pinned and Recent are there.
#[test]
fn g1_g2_the_list_is_the_daemons_pinned_and_recent_threads_with_no_projects_band() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![serving()]));
    let home = home();
    let mut a = face(fake.port, &home);
    pump(&mut a, "both threads", |a| a.threads.rows.iter().any(|t| t.id == B) && a.threads.rows.iter().any(|t| t.id == A));
    let s = screen(&mut a);
    // G2, and its opposition: the bands that ARE there.
    assert!(!s.lines().any(|l| l.contains("Projects")), "a Projects band:\n{s}");
    assert!(s.contains(" Pinned ") && s.contains(" Recent "), "no Pinned or no Recent band:\n{s}");
    // G1.
    let pinned = band_rows(&s, "Pinned");
    let recent = band_rows(&s, "Recent");
    assert!(pinned.iter().any(|l| l.contains(A_NAME)), "A is not under Pinned:\n{s}");
    assert!(!pinned.iter().any(|l| l.contains(B_NAME)), "B is under Pinned:\n{s}");
    assert!(recent.iter().any(|l| l.contains(B_NAME)), "B is not under Recent:\n{s}");
    assert!(!recent.iter().any(|l| l.contains(A_NAME)), "A is listed twice:\n{s}");
    // Never `absent`, never `?`, never the id (consumer §8, TC-02).
    assert!(!s.contains("absent") && !s.contains(" ? ") && !s.contains(B), "{s}");
}

/// **G1's opposition**: the fake serving NO threads (and a HOME with no
/// index) leaves every band empty — not one row of `data/threads.json`, the
/// fake data this tab used to draw, appears.
#[test]
fn g1_with_no_threads_served_the_bands_are_empty_and_no_fake_row_appears() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![Conn::default().push(recent(&[]))]));
    let empty_home = scratch("empty-home");
    let mut a = face(fake.port, &empty_home);
    pump(&mut a, "the link", |a| a.live.is_up());
    idle(&mut a, Duration::from_millis(150));
    let s = screen(&mut a);
    assert!(a.threads.rows.is_empty(), "{:?}", a.threads.rows);
    // The bands are there (so `band_rows` below is reading real bands, not
    // finding nothing because there is no band to find), and empty.
    assert!(s.contains("▾ Pinned") && s.contains("▾ Recent"), "{s}");
    assert!(band_rows(&s, "Pinned").is_empty() && band_rows(&s, "Recent").is_empty(), "{s}");
    let seed: Value = serde_json::from_str(&std::fs::read_to_string(seed_dir().join("threads.json")).unwrap()).unwrap();
    let names: Vec<&str> = seed["threads"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.len() > 3, "the old fake data has rows to look for: {names:?}");
    for n in names {
        assert!(!s.contains(&n[..n.len().min(24)]), "a row of data/threads.json is on screen: {n}\n{s}");
    }
}

// ── G3: send goes to the daemon ───────────────────────────────────────────

/// **G3**: Enter in the message box makes the fake receive exactly one
/// `threads/B/send` carrying the text; the transcript shows the message
/// only when the fake LOGS it (its echo), and the box empties on the
/// daemon's accept.
#[test]
fn g3_a_send_goes_to_the_daemon_and_shows_when_the_daemon_logs_it() {
    const TEXT: &str = "please look at REL-6";
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![serving().echo_sends()]));
    let home = home();
    let mut a = face(fake.port, &home);
    open_b(&mut a);
    a.threads.focus = Some(TFocus::Input);
    typed(&mut a, TEXT);
    key(&mut a, KeyCode::Enter);
    // No local echo: before the daemon logs it, the transcript lacks it.
    assert!(!a.threads.blocks().iter().any(|b| matches!(b, Block::User { text, .. } if text == TEXT)), "echoed locally: {:?}", a.threads.blocks());
    rt.block_on(fake.until("the send", |f| !sends(f).is_empty()));
    pump(&mut a, "the daemon's record of the send", |a| a.threads.blocks().iter().any(|b| matches!(b, Block::User { text, .. } if text == TEXT)));
    assert_eq!(sends(&fake), vec![(B.to_string(), TEXT.to_string())], "exactly one send, of that text, into B");
    pump(&mut a, "the accept", |a| a.threads.composer_ref().is_some_and(|c| c.text().is_empty()));
}

/// **G3's opposition**: with the fake NOT logging the send, the message is
/// not in the transcript after the draft debounce (100 ms) plus one frame
/// (16 ms) — nor after five times that.
#[test]
fn g3_a_send_the_daemon_never_logs_is_not_shown_after_the_100ms_debounce_and_a_16ms_frame() {
    const TEXT: &str = "an unlogged message";
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![serving()]));
    let home = home();
    let mut a = face(fake.port, &home);
    open_b(&mut a);
    a.threads.focus = Some(TFocus::Input);
    typed(&mut a, TEXT);
    key(&mut a, KeyCode::Enter);
    rt.block_on(fake.until("the send", |f| !sends(f).is_empty()));
    idle(&mut a, (threads::DRAFT_DEBOUNCE + tui::app::FRAME_INTERVAL) * 5);
    assert_eq!(sends(&fake).len(), 1);
    assert!(!a.threads.blocks().iter().any(|b| matches!(b, Block::User { text, .. } if text == TEXT)), "shown though the daemon never logged it: {:?}", a.threads.blocks());
}

// ── G4: the render thread never waits ─────────────────────────────────────

/// A thread whose newest message is behind the resource door.
const C: &str = "th-slow-body-cccc";
const C_NAME: &str = "a thread with a slow body";
const BODY: &str = "the words behind the door";

fn slow(hang: bool) -> Conn {
    let mut c = Conn::default()
        .push(recent(&[C]))
        .push(thread(C, C_NAME, B_CWD, "idle"))
        .push(log(C, vec![item(1, "user_message", json!({"text": "hello"})), split(2, "assistant_text", "res-body-1", BODY.len() as u64)]));
    if hang {
        c = c.hang("read");
    } else {
        c = c.respond("read", json!({"base64": b64(BODY.as_bytes()), "bytes": BODY.len()}));
    }
    c
}

fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for ch in bytes.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= ch.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// **G4**: with the daemon never serving a body, opening the thread and
/// every key after it hand the loop back — key, drain, project — inside the
/// 16 ms frame budget; the body reads as loading meanwhile. The PAINT is
/// drawn but not timed: its budget is the chat face's release frame tests,
/// and a debug paint under a parallel suite measured 30 ms (post-merge run
/// 1) — a measure of the machine, not of a wait. What this times is whether
/// the loop's own work WAITS on the daemon; the forcing that awaits the body
/// on the loop measured 30 s.
#[test]
fn g4_a_body_the_daemon_never_serves_never_holds_the_loop_past_16ms() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![slow(true)]));
    let home = home();
    let mut a = face(fake.port, &home);
    pump(&mut a, "C on the list", |a| a.threads.rows.iter().any(|t| t.id == C));
    let t0 = Instant::now();
    a.threads.open_thread(C);
    threads::settle(&mut a);
    threads::tick(&mut a);
    let open = t0.elapsed();
    screen(&mut a);
    assert!(open < tui::app::FRAME_INTERVAL, "opening the thread held the loop for {open:?} while a body was owed");
    pump(&mut a, "C drawn with its body loading", |a| a.threads.blocks().iter().any(|b| matches!(b, Block::Assistant { text, .. } if text.contains("loading"))));
    let mut worst = open;
    for k in [KeyCode::Up, KeyCode::Down, KeyCode::Up, KeyCode::Down, KeyCode::Up, KeyCode::Down] {
        let t = Instant::now();
        key(&mut a, k);
        threads::tick(&mut a);
        worst = worst.max(t.elapsed());
        screen(&mut a);
    }
    assert!(worst < tui::app::FRAME_INTERVAL, "the loop's work took {worst:?} (open {open:?}) while a body was owed");
    assert!(!a.threads.blocks().iter().any(|b| matches!(b, Block::Assistant { text, .. } if text == BODY)), "the hung body arrived?");
}

/// **G4's opposition**: the daemon serving the body, it fills in.
#[test]
fn g4_a_body_the_daemon_serves_fills_in() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![slow(false)]));
    let home = home();
    let mut a = face(fake.port, &home);
    pump(&mut a, "C on the list", |a| a.threads.rows.iter().any(|t| t.id == C));
    a.threads.open_thread(C);
    threads::settle(&mut a);
    pump(&mut a, "the body", |a| a.threads.blocks().iter().any(|b| matches!(b, Block::Assistant { text, .. } if text == BODY)));
}

// ── G5: the daemon down ───────────────────────────────────────────────────

/// **G5**: the daemon goes down — the rows stay drawn, the box says it
/// cannot send and refuses Enter, the draft is kept; the daemon comes back on
/// the same port — the legend clears, the held keys are subscribed again (a
/// second connection subscribes `threads/log` for B), and the draft is NOT
/// sent by itself.
#[test]
fn g5_the_daemon_down_keeps_the_rows_blocks_the_send_and_resubscribes_on_return() {
    const DRAFT: &str = "typed while threads was down";
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![serving()]));
    let port = fake.port;
    let home = home();
    let mut a = face(port, &home);
    open_b(&mut a);
    a.threads.focus = Some(TFocus::Input);
    let before = a.threads.rows.clone();
    assert!(!before.is_empty());

    fake.stop();
    pump(&mut a, "the link to go down", |a| a.live.is_down());
    let s = screen(&mut a);
    assert_eq!(a.threads.rows, before, "the rows changed when the daemon went down");
    assert!(s.contains(B_NAME), "B's name is gone:\n{s}");
    assert!(s.contains(live::SEND_BLOCKED), "no legend on the box:\n{s}");
    assert!(a.threads.blocks().iter().any(|b| matches!(b, Block::Assistant { text, .. } if text.contains("two open questions"))), "the transcript blanked");
    typed(&mut a, DRAFT);
    key(&mut a, KeyCode::Enter);
    assert_eq!(a.threads.composer_ref().map(|c| c.text()), Some(DRAFT.to_string()), "the draft was not kept");
    assert!(a.error.as_deref().is_some_and(|e| e.contains("threads is down")), "{:?}", a.error);

    // Back, on the same port.
    let back = rt.block_on(Fake::start_on(port, vec![serving()]));
    pump(&mut a, "the link to come back", |a| a.live.is_up());
    rt.block_on(back.until("B's log subscribed again", |f| f.log().iter().any(|r| r.method == "shapes/threads/log/subscribe" && r.params.to_string().contains(B))));
    pump(&mut a, "the legend to clear", |a| a.threads.link == threads::Link::Up);
    let s = screen(&mut a);
    assert!(!s.contains(live::SEND_BLOCKED), "the legend outlived the outage:\n{s}");
    idle(&mut a, Duration::from_millis(300));
    assert!(sends(&back).is_empty() && sends(&fake).is_empty(), "a draft was sent by itself: {:?} {:?}", sends(&fake), sends(&back));
    assert_eq!(a.threads.composer_ref().map(|c| c.text()), Some(DRAFT.to_string()));
}

// ── G6: the rename ────────────────────────────────────────────────────────

/// **G6**: the tab is called Threads; `threads` names it, `agents` is its
/// old name — refused typed, resolved stored; `cauldron` still opens.
#[test]
fn g6_the_tab_is_threads_and_agents_is_its_old_name() {
    assert_eq!(CTab::ENV.map(CTab::label), ["Cauldron", "Threads", "Status", "Permissions", "Settings"]);
    assert_eq!(CTab::parse("threads"), Some(CTab::Threads));
    assert_eq!(CTab::parse("agents"), None, "agents is not a tab's name");
    assert_eq!(CTab::renamed("agents"), Some(CTab::Threads));
    assert_eq!(CTab::stored("agents"), Some(CTab::Threads), "his stored `agents` is the Threads tab");
    assert_eq!(CTab::stored("Threads"), Some(CTab::Threads));
    assert_eq!(CTab::parse("cauldron"), Some(CTab::Cauldron));
}

// ── G7: one connection ────────────────────────────────────────────────────

/// **G7**: Threads, then Status, then Threads again: the fake is connected
/// to ONCE (one welcome). G5 is the counter's opposition: its daemon
/// restart is a second connection, which it waits for.
#[test]
fn g7_leaving_and_returning_to_the_tab_dials_once() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![serving()]));
    let home = home();
    let mut a = face(fake.port, &home);
    pump(&mut a, "B", |a| a.threads.rows.iter().any(|t| t.id == B));
    for t in [CTab::Status, CTab::Threads, CTab::Settings, CTab::Threads] {
        a.set_ctab(t);
        a.start_live();
        screen(&mut a);
        idle(&mut a, Duration::from_millis(30));
    }
    open_b(&mut a);
    assert_eq!(fake.connections(), 1, "{:#?}", fake.log());
}

// ── G8: his live ports and HOME are refused ───────────────────────────────

/// **G8**: the pointed connection and the test helpers refuse 3651, 3653,
/// 3753 and 9010 and the real HOME — before anything is dialled; a fake's
/// port and a scratch HOME are accepted and connect.
#[test]
fn g8_his_live_ports_and_real_home_are_refused_and_a_fake_is_not() {
    let scratch_home = scratch("g8");
    for port in live::PROD_PORTS {
        let r = Live::start_pointed(port, scratch_home.to_string_lossy().into_owned());
        let why = r.err().unwrap_or_else(|| panic!("port {port} was dialled"));
        assert!(why.contains("refused"), "{why}");
        assert!(checked(port, &scratch_home).is_err());
    }
    if let Some(real) = dirs::home_dir() {
        assert!(checked(40_000, &real).unwrap_err().contains("real HOME"));
    }
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![Conn::default()]));
    assert!(checked(fake.port, &scratch_home).is_ok());
    let l = Live::start_pointed(fake.port, scratch_home.to_string_lossy().into_owned()).expect("a fake's port");
    assert!(l.is_started());
    rt.block_on(fake.until_connections(1));
}

// ── REL-6a: no unpointed door to his daemon (REL-6-I F1, F2) ───────────────

/// A scratch cauldron root whose prod declaration puts threads at `port`.
/// What an UNPOINTED face would resolve to — so a fallback to the
/// environment's own daemon reaches a fake this test owns, never his.
///
/// MERGE-V1i: the root also HAS a `prod` environment (`data/prod/`). On
/// rel/halloween `prod` was exempt from `confirm_environment`, so a dump of
/// an environment-less root still opened `prod`; V1.2 (Q-F1) removed that
/// exemption and sends a root with NO environment to the first-run screen,
/// which has no Threads tab to refuse or draw. These tests are about a
/// Threads dump of an environment that exists, so the fixture now says so.
fn root_declaring(port: u16) -> PathBuf {
    let root = scratch("decl");
    std::fs::create_dir_all(root.join("services")).unwrap();
    std::fs::create_dir_all(root.join("data").join("prod")).unwrap();
    std::fs::write(
        root.join("services").join("threads.toml"),
        format!(
            "name = \"threads\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
             [launch]\naddr = \"127.0.0.1:{port}\"\n\
             [build]\nartifact = \"/usr/bin/true\"\nbin = \"true\"\n"
        ),
    )
    .unwrap();
    root
}

/// **REL-6a F1**: an App no launch opened a door for — every in-process
/// test's App — dials NOTHING when its Threads connection is started, not
/// even the daemon its environment declares. Waited 1 s for a dial that, when
/// it happens, happens at once (the seam connects on spawn; its backoff only
/// follows a failure). The sentinel first proves a fallback WOULD reach the
/// fake: the environment resolves to it.
#[test]
fn rel6a_f1_a_face_no_launch_opened_dials_nothing_not_even_its_environments_daemon_1s() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![serving(), serving()]));
    let root = root_declaring(fake.port);
    let _unset = tui::mirror::ports_unset();
    // No `CAULDRON_TYPE_ONE=off` here (REL-6ab-I F3): it is not an isolation
    // signal any more, and nothing this test does reaches type-one.
    let mut a = App::open(env_in(&root), seed_dir(), false).expect("the face opens");
    assert_eq!(
        live::Endpoint::resolve(&a.env, None, None).map(|e| e.port()),
        Ok(fake.port),
        "sentinel: the environment must resolve to the fake, or a fallback would go unseen"
    );
    a.set_ctab(CTab::Threads);
    a.start_live();
    std::thread::sleep(Duration::from_secs(1));
    assert!(!a.live.is_started(), "a face no launch opened started a connection");
    assert_eq!(fake.connections(), 0, "the environment's daemon was dialled: {:#?}", fake.log());
    let why = a.live.why_not.clone().unwrap_or_default();
    assert!(why.contains("no threads endpoint"), "{why}");
    let s = screen(&mut a);
    assert!(s.contains("no threads endpoint"), "the tab says why it has no daemon:\n{s}");
}

fn interactive_and_isolated() -> [Option<&'static str>; 2] {
    [None, Some("a headless --dump")]
}

/// **REL-6a F1, the resolution**: an isolated face (test, dump, drive) with
/// no explicit port is refused, and is refused each of his live ports
/// explicit or resolved; his interactive face resolves the environment's own
/// daemon — 3653 on prod — and may be pointed at 3653. Junk is refused in
/// both (the seam reads it as 3653). Every arm has its opposite.
#[test]
fn rel6a_f1_an_isolated_face_needs_an_explicit_port_that_is_not_his_and_his_own_face_still_reaches_prod() {
    let prod_like = env_in(&root_declaring(3653));
    let elsewhere = env_in(&root_declaring(47_611));
    // Interactive: the environment's own daemon, prod's included.
    assert_eq!(live::Endpoint::resolve(&prod_like, None, None).map(|e| e.port()), Ok(3653));
    assert_eq!(live::Endpoint::resolve(&elsewhere, None, None).map(|e| e.port()), Ok(47_611));
    for p in live::PROD_PORTS {
        assert_eq!(live::Endpoint::resolve(&elsewhere, None, Some(&p.to_string())).map(|e| e.port()), Ok(p), "his face pointed at {p}");
    }
    // Isolated: no fallback, whatever the environment says.
    for env in [&prod_like, &elsewhere] {
        let why = live::Endpoint::resolve(env, Some("a headless --dump"), None).unwrap_err();
        assert!(why.contains("refused") && why.contains("TUI_THREADS_CHASSIS_PORT"), "{why}");
    }
    for p in live::PROD_PORTS {
        let why = live::Endpoint::resolve(&elsewhere, Some("a unit test"), Some(&p.to_string())).unwrap_err();
        assert!(why.contains(&format!("port {p}")), "{why}");
    }
    assert_eq!(live::Endpoint::resolve(&prod_like, Some("a headless --dump"), Some("47612")).map(|e| e.port()), Ok(47_612));
    for iso in interactive_and_isolated() {
        for junk in ["abc", "0", "70000"] {
            assert!(live::Endpoint::resolve(&elsewhere, iso, Some(junk)).is_err(), "`{junk}` accepted ({iso:?})");
        }
    }
}

/// **REL-6a2: the isolation signals** — a dump, a unit test, a cargo test
/// harness and the test-only marker `CAULDRON_ISOLATED=1` each isolate; their
/// absence is his face. Opposition (REL-6ab-I F2): `CAULDRON_OPENER=none` and
/// `CAULDRON_TYPE_ONE=off`, which he may set for their own reasons, do NOT.
#[test]
fn rel6a2_the_isolation_signals_are_the_marker_and_the_harness_and_his_own_switches_are_not() {
    let none = |_: &str| None::<String>;
    assert_eq!(live::isolation_from(false, false, false, none), None);
    assert!(live::isolation_from(true, false, false, none).is_some());
    assert!(live::isolation_from(false, true, false, none).is_some());
    assert!(live::isolation_from(false, false, true, none).is_some());
    let var = |k: &'static str, v: &'static str| move |q: &str| (q == k).then(|| v.to_string());
    assert_eq!(live::ISOLATED_ENV, "CAULDRON_ISOLATED");
    assert!(live::isolation_from(false, false, false, var("CAULDRON_ISOLATED", "1")).is_some());
    for not in ["0", "", "yes"] {
        assert_eq!(live::isolation_from(false, false, false, var("CAULDRON_ISOLATED", not)), None, "`{not}`");
    }
    assert_eq!(live::isolation_from(false, false, false, var("CAULDRON_OPENER", "none")), None, "OPENER=none disabled his tab");
    assert_eq!(live::isolation_from(false, false, false, var("CAULDRON_TYPE_ONE", "off")), None, "TYPE_ONE=off disabled his tab");
}

/// **REL-6a2: what a cargo test harness is.** Sentinel first: THIS process
/// (the `it` binary) is one — so the predicate can fire on the real thing.
/// Opposition: his installed face, the built `tui` and a hashed name outside
/// `deps/` are not.
#[test]
fn rel6a2_a_test_harness_is_a_hashed_binary_in_deps_and_his_face_is_not() {
    let me = std::env::current_exe().unwrap();
    assert!(live::exe_is_test_harness(&me), "this `it` process is not seen as a test harness: {}", me.display());
    assert!(live::isolation(false).is_some(), "an in-process `it` test is not isolated");
    let yes = ["/x/slots/c-1/debug/deps/it-0123456789abcdef", concat!("/t/debug/deps/", "tui-fedcba9876543210")];
    let no = [
        concat!("/Users/someone/.cauldron/bin/", "tui"),
        concat!("/x/target/debug/", "tui"),
        concat!("/x/target/release/", "tui"),
        concat!("/x/debug/", "tui-0123456789abcdef"),
        "/x/debug/deps/it-0123456789abcde",
        "/x/debug/deps/it-0123456789abcdeg",
        "/x/debug/deps/libtui-0123456789abcdef.rlib",
    ];
    for p in yes {
        assert!(live::exe_is_test_harness(Path::new(p)), "{p}");
    }
    for p in no {
        assert!(!live::exe_is_test_harness(Path::new(p)), "{p}");
    }
}

/// The message a caught panic carried.
fn panic_text(e: Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
}

/// **REL-6a2: THE CONNECTION CONSTRUCTORS REFUSE HIS PORTS IN AN ISOLATED
/// PROCESS** — `mirror::spawn` (every threads connection in the crate) with
/// the lever unset (= 3653), and `pool::client::spawn` at :3653. Called with
/// NO tokio runtime on purpose: past the guard the first thing either does
/// is `tokio::spawn`, which panics before any socket exists — so a missing
/// guard shows as tokio's panic, never as a dial. Opposition: pointed at
/// port 1, the same calls pass the guard (and reach tokio's panic).
#[test]
fn rel6a2_the_connection_constructors_refuse_his_ports_when_isolated_and_dial_nothing() {
    assert!(live::isolation(false).is_some(), "sentinel: this process must be isolated");
    {
        let _unset = tui::mirror::ports_unset();
        let e = std::panic::catch_unwind(|| drop(tui::mirror::spawn())).expect_err("spawn returned");
        let msg = panic_text(e);
        assert!(msg.contains("refused") && msg.contains("port 3653"), "mirror::spawn unpointed was not refused: {msg}");
    }
    let e = std::panic::catch_unwind(|| drop(tui::mirror::spawn_dead())).expect_err("no runtime, so it must panic");
    let msg = panic_text(e);
    assert!(!msg.contains("refused"), "port 1 was refused: {msg}");
    for p in live::PROD_PORTS {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let e = std::panic::catch_unwind(|| drop(tui::pool::client::spawn(format!("ws://127.0.0.1:{p}/ws"), tx))).expect_err("pool spawn returned");
        let msg = panic_text(e);
        assert!(msg.contains("refused") && msg.contains(&format!("port {p}")), "pool :{p} was not refused: {msg}");
    }
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let e = std::panic::catch_unwind(|| drop(tui::pool::client::spawn("ws://127.0.0.1:1/ws".into(), tx))).expect_err("no runtime");
    assert!(!panic_text(e).contains("refused"), "pool :1 was refused");
    assert_eq!(tui::pool::client::url_port("ws://127.0.0.1:3653/ws"), Some(3653));
    // REL-6a5: no port is the scheme's default (80), read by a real URL
    // parser — never "no port, so nothing to refuse" (REL-6a3-I B2).
    assert_eq!(tui::pool::client::url_port("ws://127.0.0.1/ws"), Some(80));
    assert_eq!(tui::pool::client::url_port("ws://127.0.0.1:9010?x"), Some(9010));
}

/// The pty driver: `python3 -c PTY_PY <secs> <program> <args…>` runs the
/// program under a fresh pty (110x30) for at most `secs`, then SIGKILLs it;
/// it prints what the program wrote and exits with its code (99: killed).
const PTY_PY: &str = r#"
import os, pty, sys, time, select, signal, fcntl, termios, struct
secs = float(sys.argv[1])
pid, fd = pty.fork()
if pid == 0:
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 110, 0, 0))
    os.execv(sys.argv[2], sys.argv[2:])
out = b''
end = time.time() + secs
status = None
while True:
    r, _, _ = select.select([fd], [], [], 0.1)
    if r:
        try:
            d = os.read(fd, 65536)
        except OSError:
            d = b''
        out += d
    p, st = os.waitpid(pid, os.WNOHANG)
    if p:
        status = st
        break
    if time.time() > end:
        os.kill(pid, signal.SIGKILL)
        # REL-6a3 (as REL-1.S1c's build_stamp fix): KEEP READING while it
        # dies. A killed face's exit closes its tty, which on macOS waits for
        # unread output to drain; with nobody reading the master a blocking
        # waitpid hung the full suite. Drain until reaped (bounded), then close
        # the master, which releases any drain left.
        stop = time.time() + 10
        done = 0
        while done == 0 and time.time() < stop:
            r, _, _ = select.select([fd], [], [], 0.05)
            if r:
                try:
                    out += os.read(fd, 1 << 20)
                except OSError:
                    pass
            done, _ = os.waitpid(pid, os.WNOHANG)
        os.close(fd)
        if done == 0:
            os.waitpid(pid, 0)
        break
sys.stdout.buffer.write(out)
sys.stdout.flush()
if status is None:
    sys.exit(99)
sys.exit(os.waitstatus_to_exitcode(status) & 0xff)
"#;

/// Run `argv` under a fresh pty (110x30) for at most `secs`, then SIGKILL it.
/// The environment is this process's, minus `remove`, plus `set`. Returns
/// (exit code, or None if it was killed; what it wrote).
fn in_pty(argv: &[String], set: &[(&str, String)], remove: &[&str], secs: f64) -> (Option<i32>, String) {
    let mut c = std::process::Command::new("python3");
    c.arg("-c").arg(PTY_PY).arg(secs.to_string()).args(argv);
    for k in remove {
        c.env_remove(k);
    }
    for (k, v) in set {
        c.env(k, v);
    }
    let out = c.output().expect("python3 runs");
    let code = out.status.code().filter(|c| *c != 99);
    (code, String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The isolation set for a child, except the marker (each arm names it).
fn child_env(root: &Path, home: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("HOME", home.display().to_string()),
        ("CAULDRON_HOME", root.display().to_string()),
        ("CAULDRON_OPENER", "none".into()),
        ("CAULDRON_TYPE_ONE", "off".into()),
        ("AUI_V2_URL", "ws://127.0.0.1:1/".into()),
        ("CAULDRON_BIN", "/usr/bin/false".into()),
    ]
}

const CHILD_UNSET: &[&str] =
    &["CAULDRON_ENV", "TMUX", "TUI_THREADS_URL", "TUI_THREADS_PORT", "TUI_THREADS_CHASSIS_PORT", "CAULDRON_ISOLATED"];

/// **NOT A TEST ON ITS OWN**: the in-process half of
/// `rel6a2_cli_main_in_process_without_dump_dials_nothing_1500ms`, which runs
/// it under a pty (so raw mode succeeds and `cli::main` reaches its loop,
/// the inspector's probe). It calls `tui::cauldron::cli::main` WITHOUT
/// `--dump` on the Threads tab, waits 1.5 s and exits the process.
#[test]
#[ignore = "run under a pty by rel6a2_cli_main_in_process_without_dump_dials_nothing_1500ms"]
fn rel6a2_inner_cli_main_in_process() {
    let Ok(port) = std::env::var("REL6A2_FAKE_PORT") else {
        panic!("run only by its outer test");
    };
    let root = PathBuf::from(std::env::var("CAULDRON_HOME").unwrap());
    // SENTINEL: the environment this launch resolves goes to the outer
    // test's fake, so a fallback dial would be SEEN there (never his port).
    assert_eq!(live::Endpoint::resolve(&env_in(&root), None, None).map(|e| e.port().to_string()), Ok(port));
    println!("REL6A2-ISOLATION={:?}", live::isolation(false));
    std::thread::spawn(|| {
        let r = tui::cauldron::cli::main(["--env", "prod", "--tab", "threads"].map(String::from).to_vec());
        println!("REL6A2-MAIN-RETURNED={r:?}");
    });
    std::thread::sleep(Duration::from_millis(1500));
    println!("REL6A2-INNER-DONE");
    std::process::exit(0);
}

/// **REL-6a2 B1: `cli::main` called in-process WITHOUT `--dump` dials
/// nothing** — under the marker, and (the structural half) without it, since
/// the `it` binary is a test harness. The environment declares a fake, so a
/// dial is seen and harmless; waited 1.5 s for a dial that, when it happens,
/// happens at once. Each arm checks the inner run reached `cli::main`'s loop
/// (it did not return) and named its isolation.
#[test]
fn rel6a2_cli_main_in_process_without_dump_dials_nothing_1500ms() {
    let rt = rt();
    let me = std::env::current_exe().unwrap().display().to_string();
    for (arm, marker) in [("marker", true), ("harness", false)] {
        let fake = rt.block_on(Fake::start(vec![serving(), serving()]));
        let root = root_declaring(fake.port);
        let home = home();
        let mut set = child_env(&root, &home);
        set.push(("REL6A2_FAKE_PORT", fake.port.to_string()));
        if marker {
            set.push(("CAULDRON_ISOLATED", "1".into()));
        }
        let argv: Vec<String> =
            [me.as_str(), "threads_live::rel6a2_inner_cli_main_in_process", "--exact", "--ignored", "--nocapture", "--test-threads=1"]
                .map(String::from)
                .to_vec();
        let (code, out) = in_pty(&argv, &set, CHILD_UNSET, 30.0);
        assert_eq!(code, Some(0), "[{arm}] the inner run failed:\n{out}");
        assert!(out.contains("REL6A2-INNER-DONE"), "[{arm}] the inner run never finished:\n{out}");
        assert!(!out.contains("REL6A2-MAIN-RETURNED"), "[{arm}] cli::main returned before its loop (no pty?):\n{out}");
        assert_eq!(fake.connections(), 0, "[{arm}] cli::main without --dump dialled the environment's daemon: {:#?}", fake.log());
        assert!(out.contains("REL6A2-ISOLATION=Some("), "[{arm}] the inner run is not isolated:\n{out}");
    }
}

/// **REL-6a2/REL-6a3: the BUILT face, interactive (no `--dump`), on the
/// Threads tab, under a pty for 2.5 s, started through `face::Face` (so it
/// carries `CAULDRON_ISOLATED=1`) with the threads lever UNSET and its
/// environment declaring a fake: it dials NOTHING — not even that fake.
/// Sentinel, and his own face: the same environment, resolved as his
/// interactive face resolves it (no isolation), lands on that fake — so a
/// fallback would have been seen. (The unmarked built face is no longer
/// launchable from a test at all: rule B gives every child the marker.)
#[test]
fn rel6a2_the_marker_isolates_the_built_interactive_face_and_its_absence_is_his_face_2500ms() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![serving(), serving()]));
    let root = root_declaring(fake.port);
    let home = home();
    assert_eq!(live::Endpoint::resolve(&env_in(&root), None, None).map(|e| e.port()), Ok(fake.port), "his face would reach the fake");
    let (code, out) = face_in_pty(&root, &home, &["--cauldron", "--env", "prod", "--tab", "threads"], fake.port, 2.5);
    assert_eq!(code, None, "the face exited instead of running:\n{out}");
    assert_eq!(fake.connections(), 0, "the marked face dialled its environment's daemon: {:#?}", fake.log());
}

/// `face_args` on the BUILT face under the pty driver, through `face::Face`
/// (the isolation set, `CAULDRON_ISOLATED=1`), with HOME `home`,
/// CAULDRON_HOME `root`, and the threads lever UNSET — allowed only because
/// `root` resolves to `fake` ([`crate::face::Face::unset_lever_resolving_to`]).
fn face_in_pty(root: &Path, home: &Path, face_args: &[&str], fake: u16, secs: f64) -> (Option<i32>, String) {
    let mut c = crate::face::Face::wrapped(&scratch("face"), "python3", &["-c", PTY_PY, &secs.to_string()]);
    c.args(face_args).env("HOME", home).env("CAULDRON_HOME", root).env_remove("CAULDRON_ENV").unset_lever_resolving_to(fake);
    let out = c.output().expect("python3 runs");
    let code = out.status.code().filter(|c| *c != 99);
    (code, String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The built face as a headless Threads dump, through `face::Face` (the
/// whole isolation set, `CAULDRON_ISOLATED=1`), HOME `home`, CAULDRON_HOME
/// `root`, and the chassis port as given — or, `None`, the lever unset, which
/// `Face` allows only once `root` is proved to resolve to `fake`.
fn dump_threads(root: &Path, home: &Path, port: Option<u16>, fake: u16) -> std::process::Output {
    let mut c = crate::face::Face::new(&scratch("face"));
    c.args(["--cauldron", "--dump", "--env", "prod", "--tab", "threads", "--size", "110x30", "--settle", "3000"])
        .env("HOME", home)
        .env("CAULDRON_HOME", root)
        .env_remove("CAULDRON_ENV");
    match port {
        Some(p) => {
            assert!(!live::PROD_PORTS.contains(&p), "a dump is never pointed at his port {p}");
            c.env("TUI_THREADS_CHASSIS_PORT", p.to_string()).env("TUI_THREADS_PORT", p.to_string());
        }
        None => {
            c.unset_lever_resolving_to(fake);
        }
    }
    c.output().expect("the face runs")
}

/// **REL-6a F2**: `--dump --tab threads` with `TUI_THREADS_CHASSIS_PORT`
/// unset REFUSES (rc ≠ 0, the reason on stderr) and dials nothing — not
/// even the daemon its environment declares, which is a fake here so that a
/// fallback is seen and harmless. Opposition: pointed at that same fake, the
/// dump connects and draws its threads, rc 0.
#[test]
fn rel6a_f2_a_threads_dump_with_no_port_refuses_and_dials_nothing_and_a_pointed_one_draws() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![serving(), serving()]));
    let root = root_declaring(fake.port);
    let home = home();
    // SENTINEL: the built face, launched exactly so, resolves this root's
    // threads to the fake — so a dump that fell back would dial the fake,
    // seen here, and never his daemon.
    let r = crate::face::Face::new(&scratch("face"))
        .arg("--resolve")
        .env("HOME", &home)
        .env("CAULDRON_HOME", &root)
        .env_remove("CAULDRON_ENV")
        .unset_for_offline_verb("TUI_THREADS_PORT")
        .unset_for_offline_verb("TUI_THREADS_CHASSIS_PORT")
        .output()
        .expect("--resolve runs");
    let said = String::from_utf8_lossy(&r.stdout);
    assert!(said.contains(&format!("port={}", fake.port)), "the scratch root does not resolve to the fake: {said}");
    let out = dump_threads(&root, &home, None, fake.port);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_ne!(out.status.code(), Some(0), "an unpointed Threads dump succeeded:\n{}", String::from_utf8_lossy(&out.stdout));
    assert!(err.contains("refused") && err.contains("TUI_THREADS_CHASSIS_PORT"), "stderr: {err}");
    assert_eq!(fake.connections(), 0, "the unpointed dump dialled the environment's daemon: {:#?}", fake.log());
    let out = dump_threads(&root, &home, Some(fake.port), fake.port);
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(fake.connections() >= 1);
    assert!(s.contains("Recent") && s.contains(B_NAME), "{s}");
}

// ── REL-6b: tool-call detail, and in-process (Task) subagents ──────────────

/// The tail of a Read's result: past the closed row's 400-character cap, so
/// it is on screen only when the call is opened.
const RESULT_TAIL: &str = "THE-END-OF-THE-READ-RESULT";
const SCOUT_PROMPT: &str = "read the raw list projection and report";
const SCOUT_SAYS: &str = "the projection is in store.rs";

/// B's transcript with a tool call and an in-process subagent, as the
/// daemon's log carries them: a `Read` and its long result (seq 2-3), an
/// `Agent` launch (4) whose task (5) does its own work (6, 7) — every row of
/// it marked with `parent_tool_use_id` — its progress and the running task
/// list (8, 9), then the thread's answer (10).
fn b_log_with_tools() -> Value {
    let long = format!("{}{RESULT_TAIL}", "line of the spec\n".repeat(40));
    log(
        B,
        vec![
            item(1, "user_message", json!({"text": "what is on the board"})),
            item(2, "tool_use", json!({"id": "tu-1", "name": "Read", "input": {"file_path": "/Users/fixture/briefs/spec.md"}})),
            item(3, "tool_result", json!({"tool_use_id": "tu-1", "is_error": false, "content": long})),
            item(4, "tool_use", json!({"id": "ag1", "name": "Agent", "input": {"subagent_type": "Explore", "description": "scout the raw list", "prompt": SCOUT_PROMPT}})),
            item(5, "task_started", json!({"task_id": "T1", "task_type": "local_agent", "tool_use_id": "ag1"})),
            item(6, "tool_use", json!({"id": "r1", "name": "Read", "input": {"file_path": "/x/y.rs"}, "parent_tool_use_id": "ag1"})),
            item(7, "assistant_text", json!({"text": SCOUT_SAYS, "parent_tool_use_id": "ag1"})),
            item(8, "task_progress", json!({"task_id": "T1", "total_tokens": 31000, "tool_uses": 12})),
            item(9, "background_tasks_changed", json!({"tasks": [{"task_id": "T1", "task_type": "local_agent"}]})),
            item(10, "assistant_text", json!({"text": "two open questions"})),
        ],
    )
}

fn serving_tools() -> Conn {
    Conn::default()
        .push(recent(&[B, A]))
        .push(thread(A, A_NAME, B_CWD, "idle"))
        .push(thread(B, B_NAME, B_CWD, "idle"))
        .push(b_log_with_tools())
}

/// The block index of the `Read` of spec.md.
fn read_block(a: &App) -> usize {
    a.threads
        .blocks()
        .iter()
        .position(|b| matches!(b, Block::Tool { text, .. } if text.contains("spec.md")))
        .unwrap_or_else(|| panic!("no Read block: {:#?}", a.threads.blocks()))
}

/// Whether block `b` carries `what` in its text, whatever its kind.
fn says(b: &Block, what: &str) -> bool {
    match b {
        Block::User { text, .. } | Block::Assistant { text, .. } | Block::Tool { text, .. } | Block::Group { text, .. } | Block::Warn { text } | Block::Note { text } | Block::Compacted { text } => text.contains(what),
        Block::Agent { label, .. } => label.contains(what),
    }
}

/// The `threads/log` subscribes the fake saw, by key.
fn log_subscribes(f: &Fake) -> Vec<String> {
    f.log().into_iter().filter(|r| r.method == "shapes/threads/log/subscribe").map(|r| r.params.to_string()).collect()
}

/// **REL-6b G9 — a tool call opens in place.** Closed, the Read is one line
/// and its result's tail is nowhere on screen; → on it opens the chat face's
/// whole rendering (arguments and the full result), drawn under its line;
/// ← closes it again. Opposition: → on his message opens nothing (it has no
/// detail), and the transcript is the daemon's rows only.
#[test]
fn rel6b_g9_a_tool_call_opens_to_its_full_arguments_and_result_and_closes() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![serving_tools()]));
    let home = home();
    let mut a = face(fake.port, &home);
    pump(&mut a, "B on the list", |a| a.threads.rows.iter().any(|t| t.id == B));
    a.threads.open_thread(B);
    threads::settle(&mut a);
    pump(&mut a, "B's transcript", |a| a.threads.blocks().iter().any(|b| matches!(b, Block::Assistant { text, .. } if text.contains("two open questions"))));
    let i = read_block(&a);
    assert!(matches!(&a.threads.blocks()[i], Block::Tool { open: false, expandable: true, key: Some(_), .. }), "{:#?}", a.threads.blocks()[i]);
    let closed = render_to_text(&mut a, 120, 200).unwrap().join("\n");
    assert!(!closed.contains(RESULT_TAIL), "the result's tail shows while closed:\n{closed}");
    // → on the Read.
    a.threads.focus = Some(TFocus::Chat);
    a.threads.chat_item = i;
    key(&mut a, KeyCode::Right);
    let i = read_block(&a);
    assert!(matches!(&a.threads.blocks()[i], Block::Tool { open: true, .. }), "→ did not open it: {:#?}", a.threads.blocks()[i]);
    let open = render_to_text(&mut a, 120, 200).unwrap().join("\n");
    assert!(open.contains(RESULT_TAIL) && open.contains("spec.md"), "opened, the full result is not drawn:\n{open}");
    // ← closes it.
    a.threads.chat_item = i;
    key(&mut a, KeyCode::Left);
    assert!(a.threads.opened.is_empty(), "← did not close it");
    assert!(a.threads.open.is_some(), "← on an OPEN row closes the row, not the thread");
    let again = render_to_text(&mut a, 120, 200).unwrap().join("\n");
    assert!(!again.contains(RESULT_TAIL), "closed again, the tail is still drawn");
    // Opposition: his message has nothing behind it.
    let u = a.threads.blocks().iter().position(|b| matches!(b, Block::User { .. })).expect("his message");
    a.threads.chat_item = u;
    key(&mut a, KeyCode::Right);
    assert!(a.threads.opened.is_empty(), "→ on his message opened something");
}

/// **REL-6b G10 — an in-process subagent's transcript.** The thread's chat
/// holds the subagent's ONE tile and none of its rows; the tree lists it in
/// the daemon's vocabulary; → on the tile shows its own chat — the
/// invocation, marked DIRECT INVOCATION, and what it said — and none of the
/// thread's; ← goes back. All from the thread's one `threads/log`: no second
/// subscription, no second read path, the store still on B.
#[test]
fn rel6b_g10_a_task_subagents_transcript_is_its_own_rows_of_the_threads_log() {
    let rt = rt();
    let fake = rt.block_on(Fake::start(vec![serving_tools()]));
    let home = home();
    let mut a = face(fake.port, &home);
    pump(&mut a, "B on the list", |a| a.threads.rows.iter().any(|t| t.id == B));
    a.threads.open_thread(B);
    threads::settle(&mut a);
    pump(&mut a, "B's transcript", |a| a.threads.blocks().iter().any(|b| matches!(b, Block::Assistant { text, .. } if text.contains("two open questions"))));
    // The thread's own chat: the tile, not the subagent's rows.
    let tile = a.threads.blocks().iter().position(|b| matches!(b, Block::Agent { agent, .. } if agent == "ag1")).unwrap_or_else(|| panic!("no tile: {:#?}", a.threads.blocks()));
    assert!(matches!(&a.threads.blocks()[tile], Block::Agent { label, .. } if label.contains("scout the raw list")), "{:#?}", a.threads.blocks()[tile]);
    assert!(!a.threads.blocks().iter().any(|b| says(b, SCOUT_SAYS) || says(b, "/x/y.rs")), "the subagent's rows are in the thread's chat: {:#?}", a.threads.blocks());
    // The tree: the thread, then the subagent, by its label, in his words.
    let ag = a.threads.agent("ag1").expect("the Task subagent is in the tree");
    assert!(ag.label.contains("scout the raw list") && ag.state == "active", "{ag:?}");
    assert!(a.threads.tree().iter().any(|r| r.id.as_deref() == Some("ag1")));
    // → on the tile.
    a.threads.focus = Some(TFocus::Chat);
    a.threads.chat_item = tile;
    key(&mut a, KeyCode::Right);
    assert_eq!(a.threads.subject.as_deref(), Some("ag1"));
    let b = a.threads.blocks();
    assert!(matches!(b.first(), Some(Block::User { text, .. }) if text.contains(SCOUT_PROMPT)), "its chat opens on its invocation: {b:#?}");
    assert!(b.iter().any(|x| says(x, SCOUT_SAYS)), "its own reply: {b:#?}");
    assert!(b.iter().any(|x| says(x, "/x/y.rs")), "its own tool call: {b:#?}");
    assert!(!b.iter().any(|x| says(x, "two open questions") || says(x, "spec.md")), "the thread's rows in the subagent's chat: {b:#?}");
    let s = screen(&mut a);
    assert!(s.contains("DIRECT INVOCATION") || s.contains(SCOUT_PROMPT), "{s}");
    assert!(s.contains("scout the raw list"), "the header names whose chat this is:\n{s}");
    // The same one log: the store never left B, nothing else was asked for.
    idle(&mut a, Duration::from_millis(200));
    assert_eq!(a.live.store.open_id(), Some(B), "the store left the thread for a subagent");
    let subs = log_subscribes(&fake);
    assert!(!subs.is_empty() && subs.iter().all(|p| p.contains(B) && !p.contains("ag1")), "threads/log subscribes: {subs:#?}");
    assert!(!fake.log().iter().any(|r| r.method.contains("history")), "a history read");
    // ← back to the thread's chat.
    key(&mut a, KeyCode::Left);
    assert_eq!(a.threads.subject, None);
    assert!(a.threads.blocks().iter().any(|b| matches!(b, Block::Agent { .. })));
}

// ── REL-6a3: close the CLASS — no non-interactive or test-spawned face can
//    reach his daemons ─────────────────────────────────────────────────────

/// One row's verdict: `Err(text)` if the constructor refused (its panic or
/// error text), `Ok(text)` if it got past its guard (tokio's no-reactor panic,
/// or a value). Every call here runs with NO tokio runtime, so past the guard
/// the first thing a constructor does — `tokio::spawn` — panics before any
/// socket exists: a missing refusal shows as that panic, never as a dial.
fn verdict(f: impl FnOnce() -> Result<(), String>) -> Result<String, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(())) => Ok("returned".into()),
        Ok(Err(e)) => if e.contains("refused") { Err(e) } else { Ok(e) },
        Err(p) => {
            let t = panic_text(p);
            if t.contains("refused") { Err(t) } else { Ok(t) }
        }
    }
}

/// **REL-6a3 §4: EVERY CONNECTION CONSTRUCTOR × ISOLATED, TABLE-DRIVEN.**
/// This process is isolated (a cargo test harness — sentinel first). Each
/// row is one constructor at one of his ports (or unpointed, which IS one of
/// his ports: the seam's default 3653, the AUI's 9010) and must REFUSE, naming
/// the port; each row's opposition is the same constructor at port 1, which
/// must get PAST its guard. Removing any one refusal turns its rows red.
#[test]
fn rel6a3_every_connection_constructor_refuses_his_ports_when_isolated() {
    assert!(live::isolation(false).is_some(), "sentinel: this process must be isolated");
    assert_eq!(tui::aui::endpoint_from(None), "ws://127.0.0.1:9010/", "sentinel: the AUI's unpointed default is his 9010");
    assert!(std::env::var("AUI_V2_URL").is_err(), "sentinel: this process leaves the AUI unpointed, so `Config::live()` IS 9010");
    type Row = (String, u16, Box<dyn FnOnce() -> Result<(), String>>);
    let threads_at = |p: u16| -> Box<dyn FnOnce() -> Result<(), String>> {
        Box::new(move || {
            let _held = tui::mirror::ports_unset();
            if p != 3653 {
                std::env::set_var("TUI_THREADS_CHASSIS_PORT", p.to_string());
                std::env::set_var("TUI_THREADS_PORT", p.to_string());
            }
            let r = std::panic::catch_unwind(|| drop(tui::mirror::spawn()));
            std::env::remove_var("TUI_THREADS_CHASSIS_PORT");
            std::env::remove_var("TUI_THREADS_PORT");
            if let Err(e) = r {
                std::panic::resume_unwind(e);
            }
            Ok(())
        })
    };
    let mut rows: Vec<Row> = Vec::new();
    // threads, unpointed: the seam's default.
    rows.push(("threads mirror::spawn, unpointed".into(), 3653, threads_at(3653)));
    for p in live::PROD_PORTS.into_iter().chain([1]) {
        if p != 3653 {
            rows.push((format!("threads mirror::spawn :{p}"), p, threads_at(p)));
        }
        rows.push((format!("cauldron Live::start_pointed :{p}"), p, Box::new(move || Live::start_pointed(p, String::new()).map(drop))));
        rows.push((
            format!("pool::client::spawn :{p}"),
            p,
            Box::new(move || {
                let (tx, _rx) = tokio::sync::mpsc::channel(1);
                drop(tui::pool::client::spawn(format!("ws://127.0.0.1:{p}/ws"), tx));
                Ok(())
            }),
        ));
        rows.push((
            format!("aui::spawn pinned :{p}"),
            p,
            Box::new(move || {
                drop(tui::aui::spawn(tui::aui::Config::pinned(format!("ws://127.0.0.1:{p}/"))));
                Ok(())
            }),
        ));
        rows.push((
            format!("cauldron Endpoint::resolve, isolated, :{p}"),
            p,
            Box::new(move || live::Endpoint::resolve(&env_in(&scratch("root")), Some("a cargo test harness"), Some(&p.to_string())).map(drop)),
        ));
    }
    rows.push(("aui::spawn unpointed (Config::live)".into(), 9010, Box::new(|| {
        drop(tui::aui::spawn(tui::aui::Config::live()));
        Ok(())
    })));
    rows.push(("cauldron Endpoint::resolve, isolated, unpointed".into(), 0, Box::new(|| {
        live::Endpoint::resolve(&env_in(&root_declaring(47_613)), Some("a cargo test harness"), None).map(drop)
    })));
    let mut wrong = Vec::new();
    let n = rows.len();
    for (label, port, f) in rows {
        let v = verdict(f);
        let ok = match (port, &v) {
            (1, Ok(_)) => true,
            (1, Err(_)) => false,
            (0, Err(e)) => e.contains("TUI_THREADS_CHASSIS_PORT"),
            (p, Err(e)) => e.contains(&format!("port {p}")),
            (_, Ok(_)) => false,
        };
        if !ok {
            wrong.push(format!("{label}: {v:?}"));
        }
    }
    assert!(n >= 27, "the table shrank to {n} rows");
    assert!(wrong.is_empty(), "constructors that did not refuse his port (or refused port 1):\n  {}", wrong.join("\n  "));
}

/// The non-interactive modes of the face, as launched.
const NON_INTERACTIVE: &[(&str, &[&str])] = &[
    ("tui --headless", &["--headless", "--seconds", "1"]),
    ("tui pool --dump", &["pool", "--dump", "--seconds", "1"]),
    ("tui --cauldron --dump", &["--cauldron", "--dump", "--env", "prod", "--tab", "threads", "--settle", "1500"]),
    ("tui (no terminal)", &[]),
    ("tui pool (no terminal)", &["pool"]),
    ("tui --cauldron (no terminal)", &["--cauldron", "--env", "prod", "--tab", "threads"]),
];

/// **REL-6a3 §4, the other column: EVERY NON-INTERACTIVE MODE × UNPOINTED,
/// and RULE A's gate × his ports, table-driven.** In-process: the gate
/// refuses every mode unset, blank, junk, 0 and each of his ports, and passes
/// a test port. Built face: each mode launched with the lever UNSET in an
/// environment that declares a fake (proved by `--resolve`, so a fallback
/// would be SEEN there) exits non-zero, names the refusal, and dials nothing;
/// opposition — the three modes that run without a terminal, pointed at that
/// fake, DO reach it, and no pointed launch prints rule A's refusal.
#[test]
fn rel6a3_every_non_interactive_mode_needs_an_explicit_port_that_is_not_his_and_dials_nothing_without_one() {
    for (mode, _) in NON_INTERACTIVE {
        for bad in [None, Some(""), Some("junk"), Some("0")] {
            assert!(live::require_explicit_endpoint(mode, bad).is_err(), "{mode}: {bad:?} accepted");
        }
        for p in live::PROD_PORTS {
            let why = live::require_explicit_endpoint(mode, Some(&p.to_string())).unwrap_err();
            assert!(why.contains(&format!("port {p}")), "{mode}: {why}");
        }
        assert_eq!(live::require_explicit_endpoint(mode, Some("47612")), Ok(47_612), "{mode}: a test port was refused");
    }
    let rt = rt();
    let home = home();
    let mut wrong = Vec::new();
    for (mode, args) in NON_INTERACTIVE {
        // Refused: unset, resolving to a fake.
        let fake = rt.block_on(Fake::start(vec![serving(), serving()]));
        let root = root_declaring(fake.port);
        let mut c = crate::face::Face::new(&scratch("face"));
        c.args(*args).env("HOME", &home).env("CAULDRON_HOME", &root).env_remove("CAULDRON_ENV").unset_lever_resolving_to(fake.port);
        let out = c.output().expect("the face runs");
        let err = String::from_utf8_lossy(&out.stderr);
        std::thread::sleep(Duration::from_millis(300));
        if out.status.code() == Some(0) {
            wrong.push(format!("[{mode}] unpointed, it exited 0"));
        }
        if !(err.contains("refused") && err.contains("TUI_THREADS_CHASSIS_PORT")) {
            wrong.push(format!("[{mode}] unpointed, no refusal on stderr: {}", err.lines().last().unwrap_or("")));
        }
        if fake.connections() != 0 {
            wrong.push(format!("[{mode}] unpointed, it dialled the environment's daemon ({} connections)", fake.connections()));
        }
        // Opposition: pointed at a fake.
        let fake = rt.block_on(Fake::start(vec![serving(), serving()]));
        let mut c = crate::face::Face::new(&scratch("face"));
        c.args(*args)
            .env("HOME", &home)
            .env("CAULDRON_HOME", &root)
            .env_remove("CAULDRON_ENV")
            .env("TUI_THREADS_CHASSIS_PORT", fake.port.to_string())
            .env("TUI_THREADS_PORT", fake.port.to_string());
        let out = c.output().expect("the face runs");
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("non-interactive") {
            wrong.push(format!("[{mode}] pointed at a test daemon, rule A still refused: {err}"));
        }
        if !mode.contains("no terminal") && (out.status.code() != Some(0) || fake.connections() == 0) {
            wrong.push(format!("[{mode}] pointed: rc {:?}, {} connections, stderr {err}", out.status.code(), fake.connections()));
        }
    }
    assert!(wrong.is_empty(), "non-interactive launches that were not refused (or a pointed one that was):\n  {}", wrong.join("\n  "));
}

/// **REL-6a3 RULE B, run: THIS TEST BINARY'S CHILDREN INHERIT THE MARKER.**
/// Sentinel: the load-time initializer RAN in this process and found it
/// isolated (so the marker is ours, not merely inherited from whoever ran
/// cargo). Then a raw child — not through `face::Face` — sees
/// `CAULDRON_ISOLATED=1`. Opposition: the same child with the marker removed
/// sees nothing, so the probe can tell the two apart.
#[test]
fn rel6a3_rule_b_every_child_of_an_isolated_test_binary_inherits_the_marker() {
    assert!(test_isolation::ran(), "the load-time initializer did not run in this test binary");
    assert_eq!(std::env::var(live::ISOLATED_ENV).as_deref(), Ok("1"));
    let child = |remove: bool| {
        let mut c = std::process::Command::new("/bin/sh");
        c.args(["-c", "printf '[%s]' \"$CAULDRON_ISOLATED\""]);
        if remove {
            c.env_remove(live::ISOLATED_ENV);
        }
        String::from_utf8(c.output().unwrap().stdout).unwrap()
    };
    assert_eq!(child(false), "[1]", "a raw child did not inherit the marker");
    assert_eq!(child(true), "[]", "opposition: the probe cannot see the marker's absence");
}

// ── REL-6d: the Threads tab follows a cauldron switch (ledger REL-5.S3-1) ──

const ALPHA_T: &str = "th-alpha-1111";
const BRAVO_T: &str = "th-bravo-2222";
const ALPHA_NAME: &str = "the thread only alpha's daemon serves";
const BRAVO_NAME: &str = "the thread only bravo's daemon serves";

/// One connection of an environment's own daemon: one thread, in recents.
fn serving_one(id: &str, name: &str) -> Conn {
    Conn::default().push(recent(&[id])).push(thread(id, name, B_CWD, "idle"))
}

/// A scratch cauldron root with two environments, `alpha` and `bravo`, each
/// declaring its OWN threads daemon (`[services.threads] addr` in its
/// `env.toml`). The shared declaration points at the dead port, so nothing
/// falls through to it.
fn root_with_two(alpha: u16, bravo: u16) -> PathBuf {
    for p in [alpha, bravo] {
        assert!(!live::PROD_PORTS.contains(&p) && p != 0, "a scratch environment never declares {p}");
    }
    let root = scratch("two-envs");
    std::fs::create_dir_all(root.join("services")).unwrap();
    std::fs::write(
        root.join("services").join("threads.toml"),
        "name = \"threads\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
         [launch]\naddr = \"127.0.0.1:1\"\n[build]\nartifact = \"/usr/bin/true\"\nbin = \"true\"\n",
    )
    .unwrap();
    for (name, port) in [("alpha", alpha), ("bravo", bravo)] {
        let d = root.join("data").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("env.toml"), format!("[services.threads]\nversion = \"latest\"\naddr = \"127.0.0.1:{port}\"\n")).unwrap();
    }
    root
}

/// The built face as a headless Threads dump on `alpha`, pointed at
/// `alpha_port`, that switches cauldron to `bravo` before it prints its
/// frame: F1 (Cauldron), ^E ↓ ⏎ in the cauldrons dropdown, F2 (Threads).
fn dump_switching(root: &Path, home: &Path, alpha_port: u16) -> std::process::Output {
    assert!(!live::PROD_PORTS.contains(&alpha_port));
    let mut c = crate::face::Face::new(&scratch("face"));
    c.args(["--cauldron", "--dump", "--env", "alpha", "--tab", "threads", "--size", "110x30", "--settle", "3000", "--keys", "f1 ctrl-e down enter f2"])
        .env("HOME", home)
        .env("CAULDRON_HOME", root)
        .env_remove("CAULDRON_ENV")
        .env("TUI_THREADS_CHASSIS_PORT", alpha_port.to_string())
        .env("TUI_THREADS_PORT", alpha_port.to_string());
    c.output().expect("the face runs")
}

/// **REL-6d: SWITCHING CAULDRON, THE THREADS TAB FOLLOWS.** Two scratch
/// environments, each served by its own fake daemon on a port this test
/// bound and GRANTED (so `face::Face` hands it to the face). The face opens
/// on `alpha`'s Threads tab, pointed at alpha's daemon, then switches to
/// `bravo` in the face: the tab shows bravo's thread, served by bravo's own
/// daemon, and not alpha's. Before the fix it read "no threads endpoint was
/// chosen for this face" (REL-5.S3-1).
///
/// The egress gate, same switch: when bravo declares a scratch port this
/// process never granted, the switched face REFUSES it, says so, and that
/// listener is never dialled.
#[test]
fn rel6d_switching_cauldron_the_threads_tab_shows_the_new_environments_threads() {
    let rt = rt();
    let alpha = rt.block_on(Fake::start(vec![serving_one(ALPHA_T, ALPHA_NAME), serving_one(ALPHA_T, ALPHA_NAME)]));
    // REL-6f G2: bravo holds every connection, before its handshake, until
    // the face has closed every alpha connection — it waits on that EVENT
    // (alpha's server side seeing the face's EOF), not on two clocks. The
    // bound is a failure bound only, and under the dump's 3000 ms settle, so
    // a face that keeps alpha open until it exits (a leaked runtime) is
    // caught while it is still running.
    const SWITCH_CLOSES_ALPHA_WITHIN: Duration = Duration::from_millis(1500);
    let bravo = rt.block_on(Fake::start_gated(
        vec![serving_one(BRAVO_T, BRAVO_NAME), serving_one(BRAVO_T, BRAVO_NAME)],
        alpha.all_closed_gate(SWITCH_CLOSES_ALPHA_WITHIN),
    ));
    assert_ne!(alpha.port, bravo.port);
    let root = root_with_two(alpha.port, bravo.port);
    let home = home();
    let out = dump_switching(&root, &home, alpha.port);
    let s = String::from_utf8_lossy(&out.stdout);
    // SENTINEL: the launch really read alpha's daemon first.
    assert!(alpha.connections() >= 1, "the launch never reached alpha's daemon: {:#?}", alpha.log());
    // REL-6e F1, REL-6f G2: the switch TORE DOWN alpha's connection while the
    // face was still running — bravo's gate opened on alpha's closes, and no
    // bravo connection waited out the bound.
    assert_eq!(
        bravo.gate_timed_out(),
        0,
        "alpha's connection outlived the switch: bravo was dialled and alpha was still open {SWITCH_CLOSES_ALPHA_WITHIN:?} later (after the face exited: {} of {} alpha connections closed)",
        alpha.peer_closed(),
        alpha.connections()
    );
    assert!(bravo.gate_opened() >= 1, "sentinel: bravo was dialled and its gate opened on alpha's closes: {:#?}", bravo.log());
    assert_eq!(out.status.code(), Some(0), "stderr: {}\n{s}", String::from_utf8_lossy(&out.stderr));
    assert!(!s.contains("no threads endpoint"), "the switch dropped the threads door:\n{s}");
    assert!(s.contains(BRAVO_NAME), "the Threads tab does not show bravo's thread after the switch:\n{s}");
    assert!(!s.contains(ALPHA_NAME), "the Threads tab still shows alpha's thread after the switch:\n{s}");
    assert!(bravo.connections() >= 1, "bravo's own daemon was never dialled: {:#?}", bravo.log());

    // The gate: bravo on a scratch port nobody granted. A bare listener, never
    // granted, counts every dial.
    let ungranted = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let up = ungranted.local_addr().unwrap().port();
    // MERGE-V1j: REL-6a8 replaced `granted_ports()` (a port set) with
    // `granted()` (bound listeners, each `addr:port@pid`).
    assert!(!env::egress::granted().iter().any(|g| g.addr.port() == up), "sentinel: the bare listener is not granted");
    ungranted.set_nonblocking(true).unwrap();
    let alpha2 = rt.block_on(Fake::start(vec![serving_one(ALPHA_T, ALPHA_NAME), serving_one(ALPHA_T, ALPHA_NAME)]));
    let root = root_with_two(alpha2.port, up);
    let out = dump_switching(&root, &home, alpha2.port);
    let s = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "stderr: {}\n{s}", String::from_utf8_lossy(&out.stderr));
    assert!(alpha2.connections() >= 1, "sentinel: the launch reached alpha's daemon");
    // The refusal names the port: REL-6a6 wrote `port N`; REL-6a8's gate
    // (`admit_url_with`) names the address, `127.0.0.1:N` (MERGE-V1j).
    assert!(
        s.contains("no threads daemon") && s.contains("refused") && (s.contains(&format!("port {up}")) || s.contains(&format!("127.0.0.1:{up}"))),
        "the switched face did not say why it has no daemon:\n{s}"
    );
    assert!(!s.contains(ALPHA_NAME), "alpha's thread was carried into bravo:\n{s}");
    assert!(matches!(ungranted.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock), "the ungranted port was dialled");
}

// ── REL-6f G1: `reached`, through a real link ─────────────────────────────

/// **REL-6f G1: "DOWN" SAYS WHAT THE LINK ACTUALLY SAW.** The tab's
/// `reached` comes from the connection (`live.was_reached()`, a welcome's
/// `boot_id`), through a real seam link on a port this process bound and
/// granted:
///
/// - a scratch fake that welcomed the face, then went down: the line says
///   "showing what it last said";
/// - a scratch listener (bound and granted here, REL-6a8) that takes every
///   TCP dial and closes it before any welcome: the line says "it has not
///   answered yet", and never "last said".
#[test]
fn rel6f_g1_down_after_a_welcome_shows_what_it_last_said_and_down_before_one_says_it_has_not_answered() {
    const LAST_SAID: &str = "threads is down · showing what it last said";
    const NOT_ANSWERED: &str = "threads is down · it has not answered yet";
    let rt = rt();
    let home = home();

    // Reached: welcomed, then down.
    let fake = rt.block_on(Fake::start(vec![serving()]));
    let mut a = face(fake.port, &home);
    pump(&mut a, "the link to come up", |a| a.threads.link == threads::Link::Up && !a.threads.rows.is_empty());
    fake.stop();
    pump(&mut a, "the link to go down", |a| a.threads.link == threads::Link::Down);
    let s = screen(&mut a);
    assert!(s.contains(LAST_SAID) && !s.contains("not answered"), "a daemon that welcomed the face:\n{s}");
    drop(a);

    // Never reached: a listener this process bound and granted, which closes
    // every dial before the handshake.
    let mute = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = mute.local_addr().unwrap();
    env::egress::grant_addr(addr).expect("grant the scratch listener");
    let dials = std::sync::Arc::new(AtomicUsize::new(0));
    let d = dials.clone();
    std::thread::spawn(move || {
        for sock in mute.incoming() {
            d.fetch_add(1, Ordering::SeqCst);
            drop(sock);
        }
    });
    let mut a = face(addr.port(), &home);
    pump(&mut a, "the link to go down", |a| a.threads.link == threads::Link::Down);
    assert!(dials.load(Ordering::SeqCst) >= 1, "sentinel: the face dialled the scratch listener");
    assert!(!a.live.was_reached(), "sentinel: no welcome was ever sent");
    let s = screen(&mut a);
    assert!(s.contains(NOT_ANSWERED) && !s.contains("last said"), "a daemon that never answered:\n{s}");
}
