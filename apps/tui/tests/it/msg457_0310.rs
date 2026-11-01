//! Task 0310 (8e4fca10 msg 457, the five UI landings he settled on): the
//! bucket's copy at its bottom right, drawn unlike a reference chip; the root
//! tabs Cauldron | Threads | Status | Config; the environment alone at the top
//! left; the top box's legend `surface`; and Status › Ledger, the whole
//! history of execution — every dump a band, newest first, the ripples it ran
//! nested inside it, each one explored by its type. Real seed files, real
//! events, real frames, scratch roots — and the adversarial pass: no old name
//! on screen, the Ledger's nesting and keys, the bucket never losing a word.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Focus, Hit, Modal, Outbox, KEYMAP};
use tui::cauldron::bus::BusPage;
use tui::cauldron::status::{SAct, StatusTab};
use tui::cauldron::store::seed_dir;
use tui::cauldron::{render_bg_at, render_fg_at, render_to_text};

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0310-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path, name: &str) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: name.into(), name_source: Source::Default, data_root: root.join("data").join(name) }
}

fn app() -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(&scratch_root(), "prod"), seed_dir(), true).expect("opens")
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn press(a: &mut App, code: KeyCode) {
    a.handle(key(code));
}

fn click(a: &mut App, x: u16, y: u16) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
    }
}

const W: u16 = 130;
const H: u16 = 50;

fn rows(a: &mut App) -> Vec<String> {
    render_to_text(a, W, H).unwrap()
}

fn screen(a: &mut App) -> String {
    rows(a).join("\n")
}

/// The cell column of `needle` on `row`.
fn col(row: &str, needle: &str) -> u16 {
    let at = row.find(needle).unwrap_or_else(|| panic!("`{needle}` not on {row:?}"));
    row[..at].chars().count() as u16
}

// ── 1: the bucket's copy ────────────────────────────────────────────────────

/// At the bottom RIGHT, set into the border, and never in the chip style: a
/// chip is the code colour on the code ground; the copy is bold ink on no
/// ground at all.
#[test]
fn item1_the_copy_is_bottom_right_and_unlike_a_chip() {
    let mut a = app();
    a.focus_input();
    a.type_text("see ");
    a.handle(Event::Paste("one\ntwo\nthree\nfour".into()));
    let r = rows(&mut a);
    let bottom = r.len() - 1;
    assert!(r[bottom].ends_with("┤ ⧉ copy ├╯"), "{:?}", r[bottom]);
    assert!(!r[bottom].starts_with("╰ ⧉"), "not at the left any more: {:?}", r[bottom]);
    // The chip in the text, and the copy's word, side by side.
    let chip_row = r.iter().position(|l| l.contains("[paste ")).expect("a chip in the bucket");
    let chip_x = col(&r[chip_row], "[paste ") + 1;
    let copy_x = col(&r[bottom], "copy");
    let chip_bg = render_bg_at(&mut a, W, H, chip_x, chip_row as u16).unwrap();
    let copy_bg = render_bg_at(&mut a, W, H, copy_x, bottom as u16).unwrap();
    let bar_bg = render_bg_at(&mut a, W, H, 2, bottom as u16).unwrap();
    assert_eq!(chip_bg, tui_render::palette::c().code_bg, "the chip is on the code ground");
    assert_ne!(copy_bg, chip_bg, "the copy is not on a chip's ground");
    assert_eq!(copy_bg, bar_bg, "it is on the border's own ground");
    let chip_fg = render_fg_at(&mut a, W, H, chip_x, chip_row as u16).unwrap();
    let copy_fg = render_fg_at(&mut a, W, H, copy_x, bottom as u16).unwrap();
    assert_ne!(copy_fg, chip_fg, "nor in a chip's colour");
    // Its tees are the border's colour, so it reads as part of the frame.
    let tee_x = col(&r[bottom], "┤");
    let edge_fg = render_fg_at(&mut a, W, H, 1, bottom as u16).unwrap();
    assert_eq!(render_fg_at(&mut a, W, H, tee_x, bottom as u16).unwrap(), edge_fg);
    // One click on it copies the whole bucket, chip as its text, and the
    // bucket keeps every word.
    let before = a.composer.text();
    a.outbox.clear();
    click(&mut a, copy_x, bottom as u16);
    assert_eq!(a.outbox, vec![Outbox::Copy("see one\ntwo\nthree\nfour".into())]);
    assert_eq!(a.composer.text(), before, "nothing in the bucket is lost");
}

/// What the last event said sits at the bottom LEFT now and never covers
/// the copy, however long, at any width.
#[test]
fn item1_a_long_notice_never_covers_the_copy_at_any_width() {
    let mut a = app();
    a.focus_input();
    a.type_text("kept");
    for w in [40u16, 60, 90, 130] {
        a.notice = Some("a very long notice ".repeat(20));
        let r = render_to_text(&mut a, w, 30).unwrap();
        let bottom = r.len() - 1;
        assert!(r[bottom].ends_with("┤ ⧉ copy ├╯"), "{w}: {:?}", r[bottom]);
        assert!(r[bottom].starts_with("╰ a very"), "{w}: at the left: {:?}", r[bottom]);
        let hit = a.hits.iter().find(|(_, h)| *h == Hit::BucketCopy).map(|(r, _)| *r).unwrap();
        assert_eq!(hit.x, col(&r[bottom], "┤"), "{w}: the hit is where it is drawn");
        a.outbox.clear();
        click(&mut a, hit.x + 3, hit.y);
        assert!(matches!(a.outbox.last(), Some(Outbox::Copy(t)) if t == "kept"), "{w}: {:?}", a.outbox);
    }
    assert_eq!(a.composer.text(), "kept");
}

// ── 2: the environment bar's Cauldron | Threads | Status ───────────────────

/// REL-6 named the second tab Threads again (the release plan's environment
/// bar); msg 457's `Agents` is now the old name, as `Surface` is. V1.1 made
/// Config the global bar over it (two_bars.rs holds that): the environment
/// bar is row 1.
#[test]
fn item2_the_root_tabs_are_cauldron_threads_status_config_everywhere() {
    assert_eq!(CTab::ENV.map(CTab::label), ["Cauldron", "Threads", "Status", "Permissions", "Settings"]);
    let mut a = app();
    // Every tab, Threads included (V1-CLEAN1, V1.1-I §3: V1.1 had dropped
    // it from the sweep; this face has no daemon behind it, so its Threads
    // page draws without dialling).
    let mut swept = Vec::new();
    for t in CTab::ALL {
        swept.push(t);
        a.set_ctab(t);
        let s = screen(&mut a);
        let env_bar = s.lines().nth(1).unwrap();
        assert!(env_bar.ends_with("Cauldron | Threads | Status | Permissions | Settings"), "{t:?}: {env_bar}");
        for old in ["Surface", "Agents"] {
            assert!(!s.contains(old), "{t:?}: `{old}` is on screen:\n{s}");
        }
    }
    assert!(swept.contains(&CTab::Threads) && swept.len() == 9, "{swept:?}");
    // The help palette names them by their new names, and only so.
    let help: String = KEYMAP.iter().map(|(k, w)| format!("{k} {w}\n")).collect();
    assert!(help.contains("⇧Tab · F1 … F9 next tab: the environment bar's, then the global bar's"), "{help}");
    for old in ["Surface", "Agents"] {
        assert!(!help.contains(old), "`{old}` in the palette:\n{help}");
    }
    a.modal = Some(Modal::Palette { scroll: 0 });
    let s = render_to_text(&mut a, 160, 90).unwrap().join("\n");
    assert!(!s.contains("Surface") && !s.contains("Agents"), "{s}");
}

/// Typed, an old name is REFUSED by name; stored (a reference's origin, a
/// setting), it is read as the tab it always was.
#[test]
fn item2_old_names_are_refused_on_the_flag_and_kept_in_his_data() {
    assert_eq!(CTab::parse("surface"), None);
    assert_eq!(CTab::parse("agents"), None);
    assert_eq!(CTab::parse("cauldron"), Some(CTab::Cauldron));
    assert_eq!(CTab::parse("threads"), Some(CTab::Threads));
    assert_eq!(CTab::stored("surface"), Some(CTab::Cauldron));
    assert_eq!(CTab::stored("Agents"), Some(CTab::Threads));
    for (old, now) in [("surface", "cauldron"), ("agents", "threads")] {
        let e = tui::cauldron::cli::main(vec!["--dump".into(), "--tab".into(), old.into()]).unwrap_err().to_string();
        assert!(e.contains(&format!("called {now} now")), "{old}: {e}");
    }
    // A settings file from before: its tab is the same tab, renamed.
    for (old, now) in [("surface", "cauldron"), ("agents", "threads")] {
        let st = tui::cauldron::settings::Store::parse(&format!("[ui]\ndefault_tab = \"{old}\"\n"), "test").unwrap();
        assert!(matches!(st.vals.get("ui.default_tab"), Some(tui::cauldron::settings::Val::Str(v)) if v == now), "{old}: {:?}", st.vals.get("ui.default_tab"));
        assert!(st.warnings.is_empty() && st.retired.is_empty(), "{:?} {:?}", st.warnings, st.retired);
    }
    assert_eq!(tui::cauldron::settings::def("ui.default_tab").default, "cauldron");
}

// ── 3: the environment alone at the top left ───────────────────────────────

#[test]
fn item3_the_top_left_is_the_environment_and_no_product_name() {
    let mut a = app();
    // V1.1: the environment bar holds five tabs now, so its name is whole
    // from 60 columns (V1.1 spec item 7); narrower, the name crops, never a
    // tab (view.rs's d8 sweep holds that).
    for w in [60u16, 80, 130, 200] {
        let top = render_to_text(&mut a, w, 30).unwrap()[1].clone();
        let tabs_at = top.find("Cauldron |").or_else(|| top.find("Cauldron|")).unwrap_or(top.len());
        assert!(!top[..tabs_at].contains("Cauldron"), "{w}: no product name: {top:?}");
        assert!(top.trim_start().starts_with("prod  ▾"), "{w}: the environment leads: {top:?}");
        // Its dropdown is where it is drawn, and still opens.
        let env = a.hits.iter().find(|(_, h)| *h == Hit::Env).map(|(r, _)| *r).unwrap();
        assert_eq!(env.x, col(&top, " prod"), "{w}: {top:?}");
        click(&mut a, env.x + 2, env.y);
        assert!(a.dropdown.is_some(), "{w}: the cauldrons dropdown opens");
        a.dropdown = None;
    }
}

// ── 4: the legend is `surface` ──────────────────────────────────────────────

#[test]
fn item4_the_top_box_says_surface_and_the_bottom_bucket() {
    let mut a = app();
    let r = rows(&mut a);
    assert!(r[2].starts_with('╭') && r[2].trim_end().ends_with(" surface ╮"), "{:?}", r[2]);
    assert!(!r.join("\n").contains(" soup ╮"), "no legend says soup");
    assert!(r.iter().any(|l| l.starts_with('╭') && l.trim_end().ends_with(" bucket ╮")));
    // Its DEV rename reaches it by its new word.
    assert_eq!(tui::cauldron::app::Title::Surface.word(), "surface");
}

// ── 5: Status › Ledger ─────────────────────────────────────────────────────

fn ledger(a: &mut App) -> Vec<String> {
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Ledger);
    a.focus = Focus::Content;
    rows(a)
}

/// The item row whose key is `key`, selected.
fn select_key(a: &mut App, key: &str) {
    let v = tui::cauldron::status::view(a, 120);
    a.rows[0] = v.items.iter().position(|i| i.key == key).unwrap_or_else(|| panic!("no row {key}: {:?}", v.items.iter().map(|i| i.key.clone()).collect::<Vec<_>>()));
}

fn selected_key(a: &App) -> String {
    tui::cauldron::status::view(a, 120).items[a.rows[0]].key.clone()
}

#[test]
fn item5_the_ledger_is_the_first_status_tab_and_status_opens_on_it() {
    assert_eq!(StatusTab::ALL[0], StatusTab::Ledger);
    assert_eq!(StatusTab::ALL[1], StatusTab::Tokens, "Tokens and the rest keep their order behind it");
    let mut a = app();
    a.set_ctab(CTab::Status);
    assert_eq!(a.sttab, StatusTab::Ledger);
    let r = rows(&mut a);
    assert!(r[3].trim_start_matches(['│', ' ']).starts_with("Ledger | Tokens | Mesh | Soup | Bus"), "{:?}", r[3]);
    assert_eq!(StatusTab::parse("ledger"), Some(StatusTab::Ledger));
}

/// Every dump an EVENT BLOCK (V1.4: "styled as an event flow like a Claude
/// Code thread ... NOT the expandable bands"), newest first, a noun count on
/// its first line and its words under it; the newest open, the rest shut;
/// under `⎿`, the dump ripple, its PHASES, and the ripples each phase ran,
/// nested as the Tasks graph draws, each with its type in a circle, its
/// state, start and duration. (Was `item5_dumps_are_bands_...`: the band
/// look became the block look; the order, the counts and the rails stand.)
#[test]
fn item5_dumps_are_event_blocks_newest_first_and_their_ripples_nest() {
    let mut a = app();
    let r = ledger(&mut a);
    let s = r.join("\n");
    let at = |t: &str| s.find(t).unwrap_or_else(|| panic!("{t}:\n{s}"));
    assert!(at("● Dump 11") < at("● Dump 10") && at("● Dump 10") < at("● Dump 9") && at("● Dump 9") < at("● Dump 8"), "newest first:\n{s}");
    let hi = r.iter().position(|l| l.contains("● Dump 11")).unwrap();
    assert!(r[hi].contains("✓ done") && r[hi].contains("7 ripples"), "{}", r[hi]);
    assert!(r[hi + 1].contains("Let's call them dumps, not batches.") && !r[hi].contains("Let's call them"), "the words under the head:\n{s}");
    let d9 = r.iter().position(|l| l.contains("● Dump 9")).unwrap();
    assert!(r[d9].contains("✕ failed") && r[d9].contains("4 ripples") && r[d9].contains("yesterday"), "{}", r[d9]);
    assert!(r[d9 + 2].contains("⎿ 4 ripples · 5 phases"), "shut, one ⎿ line says what ran:\n{s}");
    // The rails: the dump ripple, its phases, the ripple each phase called;
    // apply runs two authors.
    let row = |name: &str, nth: usize| r.iter().filter(|l| l.contains(name)).nth(nth).cloned().unwrap_or_else(|| panic!("{name}:\n{s}"));
    assert!(row("③ dump", 0).contains("⎿ ✓ ③ dump"), "{}", row("③ dump", 0));
    assert!(row("✓ load_dump", 0).contains("├─✓ load_dump"), "{}", row("✓ load_dump", 0));
    assert!(row("② fragment", 0).contains("│ ╰─✓ ② fragment") && row("② fragment", 0).contains("done"), "{}", row("② fragment", 0));
    assert!(row("① route", 0).contains("│ ╰─✓ ① route"));
    assert!(row("③ apply", 0).contains("│ ╰─✓ ③ apply") && row("③ apply", 0).contains("done"));
    assert!(row("② author", 0).contains("│   ├─✓ ② author"), "{}", row("② author", 0));
    assert!(row("② author", 1).contains("│   ╰─✓ ② author"), "{}", row("② author", 1));
    assert!(row("◇ commit", 0).contains("╰─◇ commit"), "the last phase closes the rail: {}", row("◇ commit", 0));
    assert!(row("② consequence", 0).contains("✓ ② consequence"));
    // Start and duration in their own columns, lined up.
    for n in ["② fragment", "① route", "✓ load_dump"] {
        let l = row(n, 0);
        assert_eq!(col(&l, "done"), col(&row("② fragment", 0), "done"), "{n}: the state column lines up");
    }
    assert!(row("② fragment", 0).trim_end_matches('│').trim_end().ends_with("4.1s"), "{}", row("② fragment", 0));
    assert!(row("① route", 0).trim_end_matches('│').trim_end().ends_with("900ms"));
}

/// Nesting to any depth: a type 3 inside a type 3 inside a type 3, under
/// the phase that called it.
#[test]
fn item5_sub_ripples_of_sub_ripples_nest_three_deep() {
    let mut a = app();
    ledger(&mut a);
    select_key(&mut a, "ld:10");
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    let r: Vec<&str> = s.lines().collect();
    let line = |t: &str| *r.iter().find(|l| l.contains(t)).unwrap_or_else(|| panic!("{t}:\n{s}"));
    assert!(line("③ dump  2 fragments · 2 nodes").contains("⎿ ✓ ③ dump"), "→ opens a shut dump:\n{s}");
    // Each level two columns in from the one that ran it, its connector
    // under its parent's glyph: dump › (phase apply) › apply ›
    // author_with_review › reviewer.
    let glyph = |t: &str| col(line(t), t) - 2;
    assert_eq!(glyph("✓ apply  2 items"), glyph("③ dump  2 fragments · 2 nodes"), "the phase's glyph is in the lane after the ripple's: {}", line("✓ apply  2 items"));
    assert_eq!(glyph("③ apply  2 items · 2 written"), glyph("③ dump  2 fragments · 2 nodes") + 4, "{}", line("③ apply  2 items · 2 written"));
    assert_eq!(glyph("③ author_with_review"), glyph("③ apply  2 items · 2 written") + 2, "{}", line("③ author_with_review"));
    assert_eq!(glyph("② reviewer"), glyph("③ author_with_review") + 2, "three deep: {}", line("② reviewer"));
    assert!(line("③ apply  2 items · 2 written").contains("│ ╰─✓ ③ apply"), "{}", line("③ apply  2 items · 2 written"));
    assert!(line("② author  drafted").contains("├─✓ ② author"), "{}", line("② author  drafted"));
    assert!(line("② reviewer").contains("╰─✓ ② reviewer"), "{}", line("② reviewer"));
    // apply's first author has a sibling below it, so apply's rail runs on.
    assert!(line("DEC-38: the copy").contains("├─✓ ② author"), "{}", line("DEC-38: the copy"));
    // The dump has eleven ripples, at every depth.
    assert!(line("● Dump 10").contains("11 ripples"), "{}", line("● Dump 10"));
}

/// → and ← on a ripple that ran others: open, step in, shut; ← on one that
/// ran nothing climbs to what ran it — a ripple, or the PHASE that called
/// it, and a phase to its ripple; ← on a dump's first ripple climbs to the
/// dump, and on the dump shuts it.
#[test]
fn item5_ripples_open_and_shut_and_climb_by_the_band_standard() {
    let mut a = app();
    ledger(&mut a);
    select_key(&mut a, "lx:11:0.2");
    assert!(screen(&mut a).contains("╰─✓ ② author"));
    // ← shuts it, and its row says how many it ran.
    press(&mut a, KeyCode::Left);
    let s = screen(&mut a);
    assert!(!s.contains("╰─✓ ② author") && s.contains("+2 ripples inside"), "{s}");
    assert_eq!(selected_key(&a), "lx:11:0.2", "the selection stays on it");
    // → opens it; → again steps to the first of what it ran.
    press(&mut a, KeyCode::Right);
    assert!(screen(&mut a).contains("╰─✓ ② author"));
    press(&mut a, KeyCode::Right);
    assert_eq!(selected_key(&a), "lx:11:0.2.0");
    // ← on one that ran nothing climbs to what ran it, then on up: the
    // phase that called apply, then the dump ripple whose phase it is.
    press(&mut a, KeyCode::Left);
    assert_eq!(selected_key(&a), "lx:11:0.2");
    press(&mut a, KeyCode::Left);
    press(&mut a, KeyCode::Left);
    assert_eq!(selected_key(&a), "lp:11:0:3", "a shut ripple climbs to the phase that called it");
    press(&mut a, KeyCode::Right);
    assert_eq!(selected_key(&a), "lx:11:0.2", "→ on a phase steps to what it called");
    select_key(&mut a, "lp:11:0:3");
    press(&mut a, KeyCode::Left);
    assert_eq!(selected_key(&a), "lx:11:0", "a phase climbs to its ripple");
    press(&mut a, KeyCode::Left);
    assert_eq!(selected_key(&a), "lx:11:0", "an open ripple shuts first");
    assert!(screen(&mut a).contains("+6 phases inside"), "{}", screen(&mut a));
    press(&mut a, KeyCode::Left);
    assert_eq!(selected_key(&a), "ld:11", "a shut top ripple climbs to its dump");
    press(&mut a, KeyCode::Left);
    let s = screen(&mut a);
    assert!(s.contains("⎿ 7 ripples · 6 phases") && !s.contains("③ dump  2 fragments · applying"), "← shuts the dump:\n{s}");
    // ↓ walks the rows, every one reachable.
    a.rows[0] = 0;
    for _ in 0..30 {
        press(&mut a, KeyCode::Down);
    }
    assert!(a.rows[0] > 3, "rows below the strip are reachable");
}

/// Enter or a click explores a ripple by its TYPE: a type 1 its input and
/// output; a type 2 its transcript; a type 3 its steps as a step function,
/// whose ripples are links down, and a link up to what ran it.
#[test]
fn item5_enter_explores_each_type() {
    let mut a = app();
    ledger(&mut a);
    // Type 1.
    select_key(&mut a, "lx:11:0.1");
    press(&mut a, KeyCode::Enter);
    match &a.modal {
        Some(Modal::Bus { stack }) => assert_eq!(stack.last().unwrap().0, BusPage::Exec { seq: 11, path: vec![0, 1] }),
        other => panic!("{other:?}"),
    }
    let s = render_to_text(&mut a, 140, 60).unwrap().join("\n");
    assert!(s.contains("dump 11 › dump › route") && s.contains("① type 1") && s.contains("• Input") && s.contains("• Output") && s.contains("fragment 2 → X-1 Cauldron"), "{s}");
    assert!(!s.contains("• Transcript") && !s.contains("• Steps"), "{s}");
    a.modal = None;
    // Type 2: an agent's transcript.
    select_key(&mut a, "lx:11:0.0");
    press(&mut a, KeyCode::Enter);
    let s = render_to_text(&mut a, 140, 60).unwrap().join("\n");
    assert!(s.contains("② type 2") && s.contains("• Transcript") && s.contains("3 turns") && s.contains("› you") && s.contains("● fragment") && s.contains("◦ system"), "{s}");
    a.modal = None;
    // A tool's turn names its call.
    select_key(&mut a, "lx:11:0.2.0");
    press(&mut a, KeyCode::Enter);
    let s = render_to_text(&mut a, 140, 60).unwrap().join("\n");
    assert!(s.contains("⚙ soup.read") && s.contains("⚙ soup.write"), "{s}");
    a.modal = None;
    // Type 3: a step function.
    select_key(&mut a, "lx:11:0");
    press(&mut a, KeyCode::Enter);
    let s = render_to_text(&mut a, 140, 60).unwrap().join("\n");
    assert!(s.contains("③ type 3") && !s.to_lowercase().contains("splash") && s.contains("• Steps") && s.contains("◉ start") && s.contains("◇ cascade") && s.contains("✓ done") && !s.contains("running") && !s.contains("so far"), "{s}");
    let at = |t: &str| s.find(t).unwrap_or_else(|| panic!("{t}:\n{s}"));
    assert!(at("load_dump") < at("✓ fragment ") && at("✓ fragment ") < at("✓ apply ") && at("✓ apply ") < at("◇ commit"), "in order:\n{s}");
    // Its links: the ripples its steps ran. Follow `apply`, then the author
    // it ran; ← walks back.
    let links = a.bus_laid(tui::cauldron::bus::BUS_WIDTH).unwrap().links;
    let apply = links.iter().position(|(_, p)| *p == BusPage::Exec { seq: 11, path: vec![0, 2] }).expect("apply is a link");
    a.bus_click(apply);
    let links = a.bus_laid(tui::cauldron::bus::BUS_WIDTH).unwrap().links;
    assert_eq!(links[0].1, BusPage::Exec { seq: 11, path: vec![0] }, "the first link is what ran it");
    let author = links.iter().position(|(_, p)| *p == BusPage::Exec { seq: 11, path: vec![0, 2, 1] }).expect("its author is a link");
    a.bus_click(author);
    let s = render_to_text(&mut a, 140, 60).unwrap().join("\n");
    assert!(s.contains("dump 11 › dump › apply › author") && !s.contains("still running"), "{s}");
    press(&mut a, KeyCode::Left);
    press(&mut a, KeyCode::Left);
    match &a.modal {
        Some(Modal::Bus { stack }) => assert_eq!(stack.len(), 1),
        other => panic!("{other:?}"),
    }
    press(&mut a, KeyCode::Left);
    assert!(a.modal.is_none(), "← on the first page closes it");
}

/// A click on a ripple's row explores it at ONE click, as a bus row does; a
/// click on a dump's first line opens and shuts it.
#[test]
fn item5_a_click_explores_a_ripple_and_turns_a_dump() {
    let mut a = app();
    let r = ledger(&mut a);
    let y = r.iter().position(|l| l.contains("① route")).unwrap() as u16;
    click(&mut a, 20, y);
    assert!(matches!(&a.modal, Some(Modal::Bus { stack }) if stack[0].0 == BusPage::Exec { seq: 11, path: vec![0, 1] }), "{:?}", a.modal);
    a.modal = None;
    let r = rows(&mut a);
    let y = r.iter().position(|l| l.contains("● Dump 8")).unwrap() as u16;
    assert!(!r.join("\n").contains("DEC-35: Tokens first"), "shut before the click");
    click(&mut a, 10, y);
    let s = screen(&mut a);
    assert!(s.contains("● Dump 8") && s.contains("DEC-35: Tokens first"), "{s}");
}

/// A dump made here is on the ledger at once, at the top, with nothing run
/// for it — the face has no engine — and the bucket keeps nothing of it.
#[test]
fn item5_a_dump_made_here_is_on_top_with_nothing_run() {
    let mut a = app();
    a.focus_input();
    a.dump("a new thought for the ledger");
    assert!(a.composer.is_blank());
    let r = ledger(&mut a);
    let fi = r.iter().position(|l| l.contains("Dump ")).unwrap();
    assert!(r[fi].contains("● Dump 12") && r[fi].contains("0 ripples"), "{}", r[fi]);
    assert!(r[fi + 1].contains("a new thought for the ledger"), "its words under it: {}", r[fi + 1]);
    assert!(r[fi + 2].contains("⎿ ◇ nothing has run for it yet"), "{}", r.join("\n"));
    let d11 = r.iter().position(|l| l.contains("● Dump 11")).unwrap();
    assert!(r[d11 + 1..d11 + 4].iter().any(|l| l.contains("⎿ 7 ripples")), "the one before is shut now:\n{}", r.join("\n"));
}

/// Every row's act: a dump's block is a band (it opens and shuts); a phase
/// only shows, under its ripple; a ripple explores.
#[test]
fn item5_every_ledger_row_is_a_band_a_phase_or_a_ripple() {
    let a = {
        let mut a = app();
        ledger(&mut a);
        a
    };
    let v = tui::cauldron::status::view(&a, 120);
    let mut ripples = 0;
    let mut phases = 0;
    for it in v.items.iter().skip(1) {
        match &it.act {
            SAct::Band => assert!(it.key.starts_with("ld:"), "{}", it.key),
            SAct::Show => {
                phases += 1;
                let (seq, rest) = it.key.strip_prefix("lp:").and_then(|k| k.split_once(':')).unwrap_or_else(|| panic!("{}", it.key));
                let ripple = rest.rsplit_once(':').unwrap().0;
                assert_eq!(it.parent.as_deref(), Some(format!("lx:{seq}:{ripple}").as_str()), "{}: under its ripple", it.key);
            }
            SAct::Exec { seq, path, .. } => {
                ripples += 1;
                assert_eq!(it.key, format!("lx:{seq}:{}", tui::cauldron::ledger::path_key(path)));
            }
            other => panic!("{}: {other:?}", it.key),
        }
    }
    assert_eq!(ripples, 7, "dump 11's seven, the rest shut");
    assert_eq!(phases, 6, "dump 11's dump ripple's six steps");
}
