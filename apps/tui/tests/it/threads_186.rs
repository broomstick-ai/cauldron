//! THE THREADS TAB — his msg-186 mode (task 0167, checklist 0163 M1–M3)
//! made a tab by msg 235 (task 0190, N1 N2): the top left is the product's
//! name, plain, and Threads is the second cauldron tab; on it is the chat
//! face he uses now, in this face's visual system, in a box like every
//! tab's; in a thread its first line says the thread's name; it has no
//! bucket and no sidebar. REL-6 made it REAL: it draws the chat face's store,
//! which these tests feed through the store's own doors (`live_fixture`) —
//! the socket is `threads_live.rs`'s. Each test names the checklist ids it
//! proves. Hermetic: a scratch root, CAULDRON_TYPE_ONE=off.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Hit, Modal};
use tui::cauldron::store::seed_dir;
use tui::cauldron::threads::{Block, LItem, TFocus};

use crate::live_fixture::{self as fx, P, P_NAME, R1, R1_NAME, R2_NAME, X_NAME, K1};
use tui::cauldron::{render_bg_at, render_to_text, render_with_selection};

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "threads186-{}-{}-{}",
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

fn open() -> (App, PathBuf) {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let root = scratch_root();
    let mut a = App::open(env_in(&root, "prod"), seed_dir(), true).expect("seed loads");
    fx::feed(&mut a);
    (a, root)
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn alt(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT))
}

fn keys(a: &mut App, ks: &[KeyCode]) {
    for k in ks {
        a.handle(key(*k));
    }
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(key(KeyCode::Char(c)));
    }
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 60).unwrap().join("\n")
}

/// A row inside the tab's box: its `│` borders and the column of margin
/// each side taken off.
fn unbox(l: &str) -> &str {
    let l = l.strip_prefix('│').unwrap_or(l);
    let l = l.trim_end().strip_suffix('│').unwrap_or(l);
    l.strip_prefix(' ').unwrap_or(l).trim_end()
}

fn press_at(a: &mut App, x: u16, y: u16) {
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE }));
}

fn press(a: &mut App, hit: Hit) {
    screen(a);
    let r = a.hits.iter().rev().find(|(_, h)| *h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {hit:?} on screen"));
    press_at(a, r.x, r.y);
}

/// Open the thread `id`, its tail loaded as the daemon would hand it.
fn open_thread(a: &mut App, id: &str) {
    a.set_ctab(CTab::Threads);
    fx::open(a, id);
}

// ── N1 N2: the name, and the tab ──────────────────────────────────────────

/// N1 (msg 235): the environment's dropdown is the top left (msg 457), and
/// the tabs are Cauldron | Threads | Status | Config (REL-6).
/// N2: Threads is a cauldron TAB, the second; ⇧Tab, F2 and a click reach it,
/// and it keeps where he was in it. ⌥T is not bound any more.
#[test]
fn n1_n2_the_top_left_is_the_name_and_threads_is_a_tab() {
    let (mut a, _r) = open();
    let s = screen(&mut a);
    let top = s.lines().next().unwrap();
    assert!(top.starts_with("  prod  ▾"), "{top:?}");
    assert!(top.trim_end().ends_with("Cauldron | Threads | Status | Config"), "{top:?}");
    let bar = render_bg_at(&mut a, 120, 60, 40, 0).unwrap();
    assert_eq!(render_bg_at(&mut a, 120, 60, 0, 0).unwrap(), bar, "the margin is the bar's own background");
    press_at(&mut a, 3, 0);
    assert!(a.dropdown.is_some(), "a click at the top left is the cauldrons dropdown");
    a.dropdown = None;
    press(&mut a, Hit::CTab(CTab::Threads));
    assert_eq!(a.ctab, CTab::Threads);
    a.handle(Event::Key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)));
    assert_eq!(a.ctab, CTab::Cauldron);
    a.handle(Event::Key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)));
    assert_eq!(a.ctab, CTab::Threads);
    a.handle(Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)));
    assert_eq!(a.ctab, CTab::Status);
    // It keeps where he was: a thread opened stays open across tabs.
    open_thread(&mut a, P);
    a.set_ctab(CTab::Config);
    a.set_ctab(CTab::Threads);
    assert_eq!(a.threads.open.as_deref(), Some(P));
    a.set_ctab(CTab::Cauldron);
    a.handle(alt('t'));
    assert_eq!(a.ctab, CTab::Cauldron);
    assert!(!tui::cauldron::app::KEYMAP.iter().any(|(k, _)| k.contains("⌥T")));
}

// ── M2: the threads face ──────────────────────────────────────────────────

/// M2: no bucket and no clusters on the Threads tab; the list is bands
/// (Pinned · Recent · Archived — no Projects, REL-6) of thread rows in one
/// table, in a box like every tab's.
#[test]
fn m2_the_threads_tab_has_no_bucket_and_its_list_is_bands() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Threads);
    let s = screen(&mut a);
    for gone in [" bucket ", "Clusters", " soup ", " surface ", "Projects"] {
        assert!(!s.contains(gone), "`{gone}` is on the Threads screen:\n{s}");
    }
    assert!(s.lines().nth(1).unwrap().starts_with('╭') && s.lines().last().unwrap().starts_with('╰'), "a box from line two to the last:\n{s}");
    for band in ["▾ Pinned", "▾ Recent", "▸ Archived"] {
        assert!(s.contains(band), "no band `{band}`:\n{s}");
    }
    // The metadata is the band line's, flush right: the daemon's counts.
    let pinned = s.lines().find(|l| l.contains("▾ Pinned")).map(unbox).unwrap();
    assert!(pinned.ends_with("1 thread · ● 1 active · • 2 unread"), "{pinned:?}");
    let recent = s.lines().find(|l| l.contains("▾ Recent")).map(unbox).unwrap();
    assert!(recent.ends_with("2 threads · ● 1 active · • 1 unread"), "{recent:?}");
    // The daemon's words, verbatim: `active (+1)` for the pinned thread whose
    // subagent runs.
    assert!(s.lines().any(|l| l.contains(P_NAME) && l.contains("● active (+1)")), "{s}");
    // V6: thread rows are one table — their state columns line up.
    let col = |l: &str, w: &str| l.find(w).map(|b| l[..b].chars().count());
    let states: Vec<usize> = s.lines().filter(|l| l.contains(P_NAME) || l.contains(R1_NAME) || l.contains(R2_NAME)).filter_map(|l| col(l, "idle").or(col(l, "● active"))).collect();
    assert!(states.len() == 3 && states.windows(2).all(|w| w[0] == w[1]), "the state column does not line up: {states:?}\n{s}");
    // ← closes a band, → opens it; → on an open band steps to its first row.
    keys(&mut a, &[KeyCode::Left]);
    assert!(screen(&mut a).contains("▸ Pinned"));
    keys(&mut a, &[KeyCode::Right, KeyCode::Right]);
    assert_eq!(a.threads.list_item, 1);
    keys(&mut a, &[KeyCode::Left]);
    assert_eq!(a.threads.list_item, 0);
    // Archived opens, on the closed thread.
    assert!(!screen(&mut a).contains(X_NAME));
    keys(&mut a, &[KeyCode::End, KeyCode::Right]);
    assert!(screen(&mut a).contains(X_NAME));
}

/// M2: → on a thread opens it — the transcript (the assistant under
/// `claude · time`, his words in the node card, the tool lane), the message
/// box, the subagents (the daemon's managed children).
#[test]
fn m2_a_thread_is_the_transcript_the_message_box_and_the_subagents() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Threads);
    keys(&mut a, &[KeyCode::Down, KeyCode::Right]);
    assert_eq!(a.threads.open.as_deref(), Some(P));
    fx::transcript(&mut a);
    let s = render_to_text(&mut a, 120, 60).unwrap().join("\n");
    assert!(s.lines().any(|l| l.contains("claude · ")), "{s}");
    assert!(s.contains("Two open questions, both acted on."), "{s}");
    let card = s.lines().find(|l| l.contains(&format!("what is the state of {P}?"))).expect("his message");
    assert!(card.contains('│'), "his words are in a card: {card:?}");
    assert!(s.lines().any(|l| l.contains("Read")), "no tool lane:\n{s}");
    assert!(s.contains(&format!("Message · {P_NAME}")), "{s}");
    let band = s.lines().find(|l| l.contains("▾ Subagents")).map(unbox).expect("the subagent band");
    assert!(band.ends_with("▶ 1 running · ✓ 1 idle"), "{band:?}");
    assert!(s.contains("this thread"));
    let k1 = s.lines().find(|l| l.contains("carve region R7")).expect("K1's row");
    assert!(k1.contains("├ ") && k1.contains("active"), "{k1:?}");
}

/// M2: the regions are chat ↔ message box ↔ subagents (rule 3); a subagent
/// row shows its transcript (it is a thread of its own, opened); ← climbs
/// to the thread, then to the list.
#[test]
fn m2_the_arrows_cross_the_regions_and_climb_back() {
    let (mut a, _r) = open();
    open_thread(&mut a, P);
    assert_eq!(a.threads.focus(), TFocus::Chat);
    keys(&mut a, &[KeyCode::Down]);
    assert_eq!(a.threads.focus(), TFocus::Input);
    keys(&mut a, &[KeyCode::Down]);
    assert_eq!(a.threads.focus(), TFocus::Agents);
    keys(&mut a, &[KeyCode::Up, KeyCode::Up]);
    assert_eq!(a.threads.focus(), TFocus::Input);
    keys(&mut a, &[KeyCode::Up]);
    assert_eq!(a.threads.focus(), TFocus::Chat);
    // A subagent's row: its transcript, and the header says whose.
    a.threads.focus = Some(TFocus::Agents);
    a.threads.agent_row = 2;
    keys(&mut a, &[KeyCode::Enter]);
    assert_eq!(a.threads.subject.as_deref(), Some(K1));
    assert_eq!(a.live.store.open_id(), Some(K1), "the subagent's own thread is opened");
    fx::transcript(&mut a);
    let s = screen(&mut a);
    assert!(s.lines().nth(2).unwrap().contains(&format!("{P_NAME} › builder · carve region R7")), "its first line says whose transcript:\n{s}");
    assert!(s.contains(&format!("what is the state of {K1}?")), "{s}");
    assert!(!s.contains("Message ·"), "a subagent takes no messages:\n{s}");
    a.threads.focus = Some(TFocus::Chat);
    keys(&mut a, &[KeyCode::Home, KeyCode::Left]);
    assert_eq!(a.threads.subject, None);
    keys(&mut a, &[KeyCode::Left]);
    assert_eq!(a.threads.open, None);
    assert_eq!(a.threads.focus(), TFocus::List);
    let s = render_with_selection(&mut a, 120, 60).unwrap();
    let row = s.1.first().copied().unwrap();
    assert!(s.0[row].contains(P_NAME), "{:?}", s.0[row]);
}

/// M2: the subagent tree picks whose transcript is shown (its row one is the
/// thread); Enter on a row, or a second click on it.
#[test]
fn m2_the_subagent_tree_picks_the_transcript() {
    let (mut a, _r) = open();
    open_thread(&mut a, P);
    a.threads.focus = Some(TFocus::Agents);
    a.threads.agent_row = 3; // K2
    keys(&mut a, &[KeyCode::Enter]);
    assert_eq!(a.threads.subject.as_deref(), Some(fx::K2));
    press(&mut a, Hit::TAgent(1));
    assert_eq!(a.threads.subject.as_deref(), Some(fx::K2));
    press(&mut a, Hit::TAgent(1));
    assert_eq!(a.threads.subject, None);
    assert_eq!(a.live.store.open_id(), Some(P));
    a.threads.agent_row = 0;
    keys(&mut a, &[KeyCode::Left]);
    assert!(screen(&mut a).contains("▸ Subagents"));
    keys(&mut a, &[KeyCode::Right]);
    assert!(screen(&mut a).contains("▾ Subagents"));
}

/// M2: the message box has the composer's grammar; Enter SENDS through the
/// connection — never a local echo (TC-15). With no daemon behind the
/// fixture the send is refused with the reason and his words are kept.
/// (The send reaching a daemon is `threads_live.rs` G3.)
#[test]
fn m2_the_message_box_sends_through_the_connection_and_never_echoes() {
    let (mut a, _r) = open();
    open_thread(&mut a, P);
    let before = a.threads.blocks().to_vec();
    a.handle(key(KeyCode::Esc));
    assert_eq!(a.threads.focus(), TFocus::Input);
    typed(&mut a, "looks right");
    a.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)));
    typed(&mut a, "ship it");
    assert_eq!(a.threads.composer().text(), "looks right\nship it");
    typed(&mut a, " q");
    assert!(!a.quit);
    a.handle(key(KeyCode::Enter));
    assert_eq!(a.threads.composer().text(), "looks right\nship it q", "the box kept his words");
    assert_eq!(a.threads.blocks(), &before[..], "a local echo");
    assert!(a.error.as_deref().is_some_and(|e| e.contains("not sent") || e.contains("threads is down")), "{:?}", a.error);
    // A paste lands in the message box, and is refused elsewhere.
    a.threads.composers.clear();
    a.handle(Event::Paste("pasted".into()));
    assert_eq!(a.threads.composer().text(), "pasted");
    a.threads.focus = Some(TFocus::Chat);
    a.handle(Event::Paste("nope".into()));
    assert_eq!(a.threads.composer().text(), "pasted");
    // ⌥↑ goes to the previous message he sent.
    a.threads.chat_item = a.threads.blocks().len() - 1;
    a.handle(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)));
    assert!(matches!(&a.threads.blocks()[a.threads.chat_item], Block::User { .. }));
}

/// M2 (rule 9, the wheel) and I1: a click selects a block; the wheel
/// scrolls the transcript; `?` opens the palette; a list band closes and
/// opens at one click; a thread opens at ONE click (0214 A5).
#[test]
fn m2_i1_the_mouse_and_the_palette() {
    let (mut a, _r) = open();
    open_thread(&mut a, P);
    press(&mut a, Hit::TChat(0));
    assert_eq!(a.threads.chat_item, 0);
    screen(&mut a);
    let r = a.hits.iter().find(|(_, h)| matches!(h, Hit::TChat(_))).unwrap().0;
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::ScrollUp, column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
    assert_eq!(a.threads.chat_scroll, 3);
    a.handle(key(KeyCode::Char('?')));
    assert!(matches!(a.modal, Some(Modal::Palette { .. })));
    a.handle(key(KeyCode::Esc));
    a.threads.close_thread();
    screen(&mut a);
    let pinned = a.threads.list(118).items.iter().position(|(_, it)| matches!(it, LItem::Band(k) if k == "pinned")).unwrap();
    press(&mut a, Hit::TList(pinned as u16));
    assert!(screen(&mut a).contains("▸ Pinned"));
    press(&mut a, Hit::TList(pinned as u16));
    assert!(screen(&mut a).contains("▾ Pinned"));
    let row = a.threads.list(118).items.iter().position(|(_, it)| *it == LItem::Thread(R1.into())).unwrap();
    press(&mut a, Hit::TList(row as u16));
    assert_eq!(a.threads.open.as_deref(), Some(R1));
}

// ── M3: the thread's first line ───────────────────────────────────────────

/// M3: in a thread its first line is the thread's name, the daemon's word
/// for it and its project, and what is unread elsewhere; on the list there
/// is no header line.
#[test]
fn m3_a_threads_first_line_names_it() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Threads);
    let first = |a: &mut App| unbox(&render_to_text(a, 120, 30).unwrap()[2]).to_string();
    assert!(first(&mut a).contains("▾ Pinned"), "the list starts at once");
    keys(&mut a, &[KeyCode::Down, KeyCode::Right]);
    assert_eq!(first(&mut a), format!("{P_NAME} · active (+1) · harness · 3 unread elsewhere"));
    keys(&mut a, &[KeyCode::Left]);
    open_thread(&mut a, R1);
    assert!(first(&mut a).starts_with(&format!("{R1_NAME} · active · harness")), "{}", first(&mut a));
}

// ── the visual system, held ───────────────────────────────────────────────

/// S3 on the Threads tab: the selection is a background — on the list, a
/// transcript block, a subagent row — dimmer where the keys are not; no bar.
#[test]
fn the_selection_in_threads_mode_is_a_background() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Threads);
    let (rows, sel) = render_with_selection(&mut a, 120, 60).unwrap();
    assert_eq!(sel.len(), 1, "{sel:?}");
    assert!(rows[sel[0]].contains("▾ Pinned"));
    assert!(rows.iter().all(|r| !r.contains('▌')));
    open_thread(&mut a, P);
    a.threads.chat_item = a.threads.blocks().iter().position(|b| matches!(b, Block::User { .. })).unwrap();
    a.threads.reveal = true;
    let (rows, sel) = render_with_selection(&mut a, 120, 60).unwrap();
    let card: Vec<&String> = sel.iter().map(|&y| &rows[y]).collect();
    assert!(card.iter().any(|r| r.contains("what is the state of")) && card.iter().any(|r| r.contains("╰")), "{card:?}");
    assert!(card.iter().any(|r| r.contains("this thread")), "the tree's cursor has no background: {card:?}");
    let y = rows.iter().position(|r| r.contains("what is the state of")).unwrap() as u16;
    assert_eq!(render_bg_at(&mut a, 120, 60, 60, y).unwrap(), tui_render::palette::c().sel_bg);
    let t = rows.iter().position(|r| r.contains("this thread")).unwrap() as u16;
    assert_eq!(render_bg_at(&mut a, 120, 60, 4, t).unwrap(), tui_render::palette::c().sel_bg_dim);
}

/// msg 165's scroll invariant on the Threads tab: selecting any row of the
/// list or any block of a transcript never changes a line of the page.
#[test]
fn selection_and_scrolling_never_change_the_threads_pages() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Threads);
    let list = |a: &App| a.threads.list(110).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
    let first = list(&a);
    let n = a.threads.list(110).items.len();
    assert!(n >= 6, "the bands and the fixture's rows: {n}");
    for i in 0..n {
        a.threads.list_item = i;
        a.threads.list_scroll = i;
        assert_eq!(list(&a), first, "selecting row {i} changed the list");
    }
    for t in fx::LISTED {
        open_thread(&mut a, t);
        let subjects: Vec<Option<String>> = std::iter::once(None).chain(a.threads.agent_rows.iter().map(|x| Some(x.id.clone()))).collect();
        for subject in subjects {
            a.threads.set_subject(subject);
            tui::cauldron::threads::settle(&mut a);
            fx::transcript(&mut a);
            let chat = |a: &App| a.threads.chat(110).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
            let tree = |a: &App| a.threads.agents(110).iter().map(|l| l.to_string()).collect::<Vec<_>>();
            let first = chat(&a);
            let ftree = tree(&a);
            assert!(a.threads.blocks().len() >= 3, "{t}: the tail is loaded");
            for i in 0..a.threads.blocks().len() {
                a.threads.chat_item = i;
                a.threads.chat_scroll = i;
                assert_eq!(chat(&a), first, "thread {t}: selecting block {i} changed the transcript");
            }
            for r in 0..=a.threads.tree().len() {
                a.threads.agent_row = r;
                assert_eq!(tree(&a), ftree, "thread {t}: the tree's cursor on row {r} changed the tree");
            }
        }
    }
}

/// The shape law, at every width he uses: no line is wider than the frame,
/// on the list or in a thread, at the top or scrolled.
#[test]
fn the_threads_tab_fits_every_width() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Threads);
    for w in [60u16, 80, 100, 120, 160] {
        for s in [0usize, 1000] {
            a.threads.close_thread();
            a.threads.list_scroll = s;
            for l in render_to_text(&mut a, w, 30).unwrap() {
                assert!(l.chars().count() <= w as usize, "{w}: {l:?}");
            }
            open_thread(&mut a, P);
            a.threads.chat_scroll = s;
            for l in render_to_text(&mut a, w, 30).unwrap() {
                assert!(l.chars().count() <= w as usize, "{w}: {l:?}");
            }
        }
    }
}
