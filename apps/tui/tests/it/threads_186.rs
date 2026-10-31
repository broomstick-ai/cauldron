//! THE THREADS TAB — his msg-186 mode (task 0167, checklist 0163 M1–M3)
//! made a tab by msg 235 (task 0190, N1 N2): the top left is the product's
//! name, plain, and Threads is the second cauldron tab; on it is the chat
//! face he uses now, rebuilt on its own data (`data/threads.json`) in this
//! face's visual system, in a box like every tab's; in a thread its first
//! line says the thread's name; it has no bucket and no sidebar. Each test
//! names the checklist ids it proves. Hermetic: a scratch root,
//! CAULDRON_TYPE_ONE=off.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Hit, Modal};
use tui::cauldron::store::seed_dir;
use tui::cauldron::threads::{Block, TFocus};
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
    (App::open(env_in(&root, "prod"), seed_dir(), true).expect("seed loads"), root)
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

/// Open the thread `id` from the list with keys: select it, →.
fn open_thread(a: &mut App, id: &str) {
    a.set_ctab(CTab::Agents);
    let at = a.threads.seed.threads.iter().position(|t| t.id == id).unwrap();
    a.threads.open_thread(at);
}

// ── N1 N2: the name, and the tab ──────────────────────────────────────────

/// N1 (msg 235: "Let's turn it back to just the name of the product,
/// cauldron"): the top left is `Cauldron`, plain — no button, no background
/// of its own, and a click on it does nothing — then the cauldrons dropdown.
/// 8e4fca10 msg 457 took the name away: the environment's dropdown is the
/// top left, and Threads is called Agents.
/// N2: Threads is a cauldron TAB, the second; ⇧Tab, F2 and a click reach it,
/// and it keeps where he was in it. ⌥T is not bound any more.
#[test]
fn n1_n2_the_top_left_is_the_name_and_threads_is_a_tab() {
    let (mut a, _r) = open();
    let s = screen(&mut a);
    let top = s.lines().next().unwrap();
    // 8e4fca10 msg 457: no product name — the environment is the top left.
    assert!(top.starts_with("  prod  ▾"), "{top:?}");
    assert!(top.trim_end().ends_with("Cauldron | Agents | Status | Config"), "{top:?}");
    let bar = render_bg_at(&mut a, 120, 60, 40, 0).unwrap();
    assert_eq!(render_bg_at(&mut a, 120, 60, 0, 0).unwrap(), bar, "the margin is the bar's own background");
    press_at(&mut a, 3, 0);
    assert!(a.dropdown.is_some(), "a click at the top left is the cauldrons dropdown");
    a.dropdown = None;
    // A click on the tab, F2, ⇧Tab.
    press(&mut a, Hit::CTab(CTab::Agents));
    assert_eq!(a.ctab, CTab::Agents);
    a.handle(Event::Key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)));
    assert_eq!(a.ctab, CTab::Cauldron);
    a.handle(Event::Key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)));
    assert_eq!(a.ctab, CTab::Agents);
    a.handle(Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)));
    assert_eq!(a.ctab, CTab::Status);
    // It keeps where he was: a thread opened stays open across tabs.
    open_thread(&mut a, "8e4fca10");
    a.set_ctab(CTab::Config);
    a.set_ctab(CTab::Agents);
    assert_eq!(a.threads.open, Some(0));
    // ⌥T does nothing now — in the bucket it is not text either.
    a.set_ctab(CTab::Cauldron);
    a.handle(alt('t'));
    assert_eq!(a.ctab, CTab::Cauldron);
    assert!(!tui::cauldron::app::KEYMAP.iter().any(|(k, _)| k.contains("⌥T")));
}

// ── M2: the threads face ──────────────────────────────────────────────────

/// M2: no bucket and no clusters on the Threads tab, the cauldron tabs above
/// it as on every tab; the list is bands (Pinned · Recent · Projects ·
/// Archived) of thread rows in one table, in a box like every tab's.
#[test]
fn m2_the_threads_tab_has_no_bucket_and_its_list_is_bands() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Agents);
    let s = screen(&mut a);
    for gone in [" bucket ", "Clusters", " soup ", " surface "] {
        assert!(!s.contains(gone), "`{gone}` is on the Agents screen:\n{s}");
    }
    assert!(s.lines().nth(1).unwrap().starts_with('╭') && s.lines().last().unwrap().starts_with('╰'), "a box from line two to the last:\n{s}");
    for band in ["▾ Pinned", "▾ Recent", "▾ Projects", "▾ harness", "▸ Archived"] {
        assert!(s.contains(band), "no band `{band}`:\n{s}");
    }
    // The metadata is the band line's, flush right.
    let pinned = s.lines().find(|l| l.contains("▾ Pinned")).map(unbox).unwrap();
    assert!(pinned.ends_with("2 threads · ● 1 active · • 12 unread"), "{pinned:?}");
    // V6: thread rows at one level are one table — their columns line up,
    // in every band at that level (a project's rows are a level deeper).
    let col = |l: &str, w: &str| l.find(w).map(|b| l[..b].chars().count());
    for depth in [5usize, 7] {
        let rows: Vec<&str> = s.lines().map(unbox).filter(|l| l.len() - l.trim_start().len() == depth - 1 && (l.contains(" idle ") || l.contains("● active"))).collect();
        let states: Vec<usize> = rows.iter().filter_map(|l| col(l, "idle").or(col(l, "● active"))).collect();
        assert!(states.len() > 5 && states.windows(2).all(|w| w[0] == w[1]), "depth {depth}: the state column does not line up: {states:?}\n{s}");
    }
    // ← closes a band, → opens it; → on an open band steps to its first row.
    keys(&mut a, &[KeyCode::Left]);
    assert!(screen(&mut a).contains("▸ Pinned"));
    keys(&mut a, &[KeyCode::Right, KeyCode::Right]);
    assert_eq!(a.threads.list_item, 1);
    // ← on a row climbs to its band.
    keys(&mut a, &[KeyCode::Left]);
    assert_eq!(a.threads.list_item, 0);
    // Archived opens.
    keys(&mut a, &[KeyCode::End, KeyCode::Right]);
    assert!(screen(&mut a).contains("Axiom curation"));
}

/// M2: → on a thread opens it — the transcript (the assistant under `claude
/// · time · took`, his words in the node card with their id as its legend,
/// the tool lane, the compaction rule), the message box, the subagents.
#[test]
fn m2_a_thread_is_the_transcript_the_message_box_and_the_subagents() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Agents);
    keys(&mut a, &[KeyCode::Down, KeyCode::Right]);
    assert_eq!(a.threads.open, Some(0));
    a.threads.chat_scroll = 1000;
    let s = render_to_text(&mut a, 120, 90).unwrap().join("\n");
    assert!(s.contains("claude · 06:44 · 12m"), "{s}");
    let card = s.lines().find(|l| l.contains("#186")).map(unbox).expect("his message's card");
    assert!(card.trim_start().starts_with("╭─ 06:42") && card.trim_end().ends_with("#186 ─╮"), "{card:?}");
    // His card is pushed right: a margin of ten at this width.
    assert!(card.len() - card.trim_start().len() >= 10, "{card:?}");
    assert!(s.contains("⚙ Read docs/navigation.md") && s.contains("721 lines"), "{s}");
    assert!(s.contains("◇ build and test · r9/globs-iteration-2") && s.contains("214 calls"));
    assert!(s.contains("⚠ retry #2 at 07:31 · overloaded"));
    // No `── TITLE ──` rule anywhere (msg 235: "Eradicate it. From the
    // entire application"): a compaction is a muted line, the message box's
    // head a band on the strip's background.
    assert!(s.contains("⋯ compacted (auto) · swallowed #141–#179") && !s.contains("── compacted"), "{s}");
    assert!(s.contains("Message · Cauldron interface — msg 186 round ") && !s.contains("msg 186 round ─"), "{s}");
    let band = s.lines().find(|l| l.contains("▾ Subagents")).map(unbox).expect("the subagent band");
    assert!(band.ends_with("▶ 1 running · ✓ 4 done · ✕ 1 failed"), "{band:?}");
    // Row one of the tree is the thread itself; a child is nested under its
    // parent with rails; the figures he asked for are on the row.
    assert!(s.contains("Cauldron interface — msg 186 round") && s.contains("this thread"));
    let child = s.lines().find(|l| l.contains("Explore · study the harness tui")).unwrap();
    assert!(child.contains("│ └ "), "{child:?}");
    let b1 = s.lines().find(|l| l.contains("├ core:builder · B1 threads mode")).unwrap();
    for want in ["running", "14m", "312k tok", "88 calls"] {
        assert!(b1.contains(want), "{want} missing: {b1:?}");
    }
}

/// M2: the regions are chat ↔ message box ↔ subagents (rule 3); → opens a
/// tool call's details in place as a table, ← closes it; → on a subagent's
/// launch shows its transcript; ← climbs to the thread, then to the list.
#[test]
fn m2_the_arrows_cross_the_regions_open_in_place_and_climb_back() {
    let (mut a, _r) = open();
    open_thread(&mut a, "8e4fca10");
    assert_eq!(a.threads.focus(), TFocus::Chat);
    // The newest block is selected; ↓ goes on into the message box, and on
    // into the subagents.
    keys(&mut a, &[KeyCode::Down]);
    assert_eq!(a.threads.focus(), TFocus::Input);
    keys(&mut a, &[KeyCode::Down]);
    assert_eq!(a.threads.focus(), TFocus::Agents);
    keys(&mut a, &[KeyCode::Up, KeyCode::Up]);
    assert_eq!(a.threads.focus(), TFocus::Input);
    keys(&mut a, &[KeyCode::Up]);
    assert_eq!(a.threads.focus(), TFocus::Chat);
    // The tool call: its details are a table.
    let tool = a.threads.blocks().iter().position(|b| matches!(b, Block::Tool { text, .. } if text == "Read docs/navigation.md")).unwrap();
    a.threads.chat_item = tool;
    keys(&mut a, &[KeyCode::Right]);
    let s = screen(&mut a);
    assert!(s.contains("▾ ⚙ Read docs/navigation.md"), "{s}");
    assert!(s.lines().map(unbox).any(|l| l.trim_start().starts_with("path") && l.contains(" │ ~/code/cauldron-wt-globs2/docs/navigation.md")), "{s}");
    keys(&mut a, &[KeyCode::Left]);
    assert!(screen(&mut a).contains("▸ ⚙ Read docs/navigation.md"));
    assert_eq!(a.threads.open, Some(0), "← on an opened call left the thread");
    // A subagent's launch: its transcript, and the header says whose.
    let launch = a.threads.blocks().iter().position(|b| matches!(b, Block::Agent { agent } if agent == "a4")).unwrap();
    a.threads.chat_item = launch;
    keys(&mut a, &[KeyCode::Enter]);
    assert_eq!(a.threads.subject.as_deref(), Some("a4"));
    let s = screen(&mut a);
    assert!(s.lines().nth(2).unwrap().contains("Cauldron interface — msg 186 round › core:builder · B1 threads mode"), "its first line says whose transcript:\n{s}");
    assert!(s.contains("Studying the harness tui first"), "{s}");
    // A subagent takes no messages: there is no message box.
    assert!(!s.contains("Message ·"), "{s}");
    keys(&mut a, &[KeyCode::Home, KeyCode::Left]);
    assert_eq!(a.threads.subject, None);
    keys(&mut a, &[KeyCode::Left]);
    assert_eq!(a.threads.open, None);
    assert_eq!(a.threads.focus(), TFocus::List);
    // The list is back on the row of the thread he left.
    let s = render_with_selection(&mut a, 120, 60).unwrap();
    let row = s.1.first().copied().unwrap();
    assert!(s.0[row].contains("Cauldron interface — msg 186 round"), "{:?}", s.0[row]);
}

/// M2: the subagent tree picks whose transcript is shown (its row one is the
/// thread); Enter on a row, or a second click on it.
#[test]
fn m2_the_subagent_tree_picks_the_transcript() {
    let (mut a, _r) = open();
    open_thread(&mut a, "8e4fca10");
    a.threads.focus = Some(TFocus::Agents);
    a.threads.agent_row = 3; // B3
    keys(&mut a, &[KeyCode::Enter]);
    assert_eq!(a.threads.subject.as_deref(), Some("a2"));
    assert!(screen(&mut a).contains("Committed faaecbcd"));
    // A click selects the thread's row, a second click opens it.
    press(&mut a, Hit::TAgent(1));
    assert_eq!(a.threads.subject.as_deref(), Some("a2"));
    press(&mut a, Hit::TAgent(1));
    assert_eq!(a.threads.subject, None);
    // ← on the band's line shuts it; → opens it again.
    a.threads.agent_row = 0;
    keys(&mut a, &[KeyCode::Left]);
    assert!(screen(&mut a).contains("▸ Subagents"));
    keys(&mut a, &[KeyCode::Right]);
    assert!(screen(&mut a).contains("▾ Subagents"));
}

/// M2: the message box has the composer's grammar; Enter sends — his
/// message lands with the next id, held here, said so.
#[test]
fn m2_the_message_box_sends_into_the_thread() {
    let (mut a, _r) = open();
    open_thread(&mut a, "8e4fca10");
    a.handle(key(KeyCode::Esc));
    assert_eq!(a.threads.focus(), TFocus::Input);
    typed(&mut a, "looks right");
    // ⌥Enter is a new line, not a send.
    a.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)));
    typed(&mut a, "ship it");
    assert_eq!(a.threads.composer().text(), "looks right\nship it");
    // `q` is text here, not quit.
    typed(&mut a, " q");
    assert!(!a.quit);
    a.handle(key(KeyCode::Enter));
    assert!(a.threads.composer().text().is_empty());
    let s = render_to_text(&mut a, 120, 60).unwrap().join("\n");
    let card = s.lines().find(|l| l.contains("#187")).expect("the sent message's card");
    assert!(card.contains("╭─"), "{card:?}");
    assert!(s.contains("ship it q"), "{s}");
    assert!(s.contains("◇ held in this face · no daemon behind the Agents tab"), "{s}");
    // A paste lands in the message box, and is refused elsewhere.
    a.handle(Event::Paste("pasted".into()));
    assert_eq!(a.threads.composer().text(), "pasted");
    a.threads.focus = Some(TFocus::Chat);
    a.handle(Event::Paste("nope".into()));
    assert_eq!(a.threads.composer().text(), "pasted");
    // ⌥↑ goes to the previous message he sent.
    a.threads.chat_item = a.threads.blocks().len() - 1;
    a.handle(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)));
    assert!(matches!(&a.threads.blocks()[a.threads.chat_item], Block::User { id, .. } if id == "187"));
    a.handle(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)));
    assert!(matches!(&a.threads.blocks()[a.threads.chat_item], Block::User { id, .. } if id == "186"));
}

/// M2 (rule 9, the wheel) and I1: a batch of calls is a BAND — one click
/// opens it and one closes it (msg 235); a list's band too; a click selects
/// a thread and a second opens it; the wheel scrolls the transcript; `?`
/// opens the palette over the Threads tab.
#[test]
fn m2_i1_the_mouse_and_the_palette() {
    let (mut a, _r) = open();
    open_thread(&mut a, "8e4fca10");
    let tool = a.threads.blocks().iter().position(|b| matches!(b, Block::Group { .. })).unwrap();
    press(&mut a, Hit::TChat(tool as u16));
    assert_eq!(a.threads.chat_item, tool);
    assert!(a.threads.block_open(tool), "one click opens the band");
    assert!(screen(&mut a).contains("⚙ Bash cargo clippy -p tui"));
    press(&mut a, Hit::TChat(tool as u16));
    assert!(!a.threads.block_open(tool), "one more closes it");
    press(&mut a, Hit::TChat(tool as u16));
    assert!(a.threads.block_open(tool));
    // The wheel.
    screen(&mut a);
    let r = a.hits.iter().find(|(_, h)| matches!(h, Hit::TChat(_))).unwrap().0;
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::ScrollUp, column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
    assert_eq!(a.threads.chat_scroll, 3);
    a.handle(key(KeyCode::Char('?')));
    assert!(matches!(a.modal, Some(Modal::Palette { .. })));
    a.handle(key(KeyCode::Esc));
    // On the list: a band closes at one click, and opens at one.
    a.threads.close_thread();
    screen(&mut a);
    let pinned = a.threads.list(118).items.iter().position(|(_, it)| matches!(it, tui::cauldron::threads::LItem::Band(k) if k == "pinned")).unwrap();
    press(&mut a, Hit::TList(pinned as u16));
    assert!(screen(&mut a).contains("▸ Pinned"));
    press(&mut a, Hit::TList(pinned as u16));
    assert!(screen(&mut a).contains("▾ Pinned"));
    // A thread opens at ONE click (0214 A5).
    let row = a.threads.list(118).items.iter().position(|(_, it)| *it == tui::cauldron::threads::LItem::Thread(4)).unwrap();
    press(&mut a, Hit::TList(row as u16));
    assert_eq!(a.threads.open, Some(4));
}

// ── M3: the thread's first line ───────────────────────────────────────────

/// M3: in a thread its first line is the thread's name, its state and
/// project, and what is unread elsewhere — opening a thread reads it; on the
/// list there is no header line (the bands carry the counts: msg 235, "Don't
/// have any metadata in the tab header").
#[test]
fn m3_a_threads_first_line_names_it() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Agents);
    let first = |a: &mut App| unbox(&render_to_text(a, 120, 30).unwrap()[2]).to_string();
    assert!(first(&mut a).contains("▾ Pinned"), "the list starts at once");
    keys(&mut a, &[KeyCode::Down, KeyCode::Down, KeyCode::Right]);
    assert_eq!(first(&mut a), "The threads daemon wire — living notes · idle · harness · 798 unread elsewhere");
    keys(&mut a, &[KeyCode::Left]);
    open_thread(&mut a, "c0ffee11");
    assert!(first(&mut a).contains("Polymarket backtest — the fee model · idle · orchard · 497 unread elsewhere"), "{}", first(&mut a));
}

// ── the visual system, held ───────────────────────────────────────────────

/// S3 on the Threads tab: the selection is a background — on the list, a
/// transcript block, a subagent row — dimmer where the keys are not; no bar.
#[test]
fn the_selection_in_threads_mode_is_a_background() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Agents);
    let (rows, sel) = render_with_selection(&mut a, 120, 60).unwrap();
    assert_eq!(sel.len(), 1, "{sel:?}");
    assert!(rows[sel[0]].contains("▾ Pinned"));
    assert!(rows.iter().all(|r| !r.contains('▌')));
    open_thread(&mut a, "8e4fca10");
    a.threads.chat_item = a.threads.blocks().iter().position(|b| matches!(b, Block::User { id, .. } if id == "186")).unwrap();
    a.threads.reveal = true;
    let (rows, sel) = render_with_selection(&mut a, 120, 60).unwrap();
    // His whole card is on the background, and the subagent row too (dim).
    let card: Vec<&String> = sel.iter().map(|&y| &rows[y]).collect();
    assert!(card.iter().any(|r| r.contains("#186")) && card.iter().any(|r| r.contains("╰")), "{card:?}");
    assert!(card.iter().any(|r| r.contains("this thread")), "the tree's cursor has no background: {card:?}");
    let y = rows.iter().position(|r| r.contains("#186")).unwrap() as u16;
    assert_eq!(render_bg_at(&mut a, 120, 60, 60, y).unwrap(), tui_render::palette::c().sel_bg);
    let t = rows.iter().position(|r| r.contains("this thread")).unwrap() as u16;
    assert_eq!(render_bg_at(&mut a, 120, 60, 4, t).unwrap(), tui_render::palette::c().sel_bg_dim);
}

/// msg 165's scroll invariant on the Threads tab: selecting any row of the
/// list or any block of a transcript never changes a line of the page.
#[test]
fn selection_and_scrolling_never_change_the_threads_pages() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Agents);
    let list = |a: &App| a.threads.list(110).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
    let first = list(&a);
    for i in 0..a.threads.list(110).items.len() {
        a.threads.list_item = i;
        a.threads.list_scroll = i;
        assert_eq!(list(&a), first, "selecting row {i} changed the list");
    }
    for t in 0..a.threads.seed.threads.len() {
        a.threads.open_thread(t);
        for subject in std::iter::once(None).chain(a.threads.thread().unwrap().agents.iter().map(|x| Some(x.id.clone())).collect::<Vec<_>>()) {
            a.threads.set_subject(subject);
            let chat = |a: &App| a.threads.chat(110).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
            let tree = |a: &App| a.threads.agents(110).iter().map(|l| l.to_string()).collect::<Vec<_>>();
            let first = chat(&a);
            let ftree = tree(&a);
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
    a.set_ctab(CTab::Agents);
    for w in [60u16, 80, 100, 120, 160] {
        for s in [0usize, 1000] {
            a.threads.close_thread();
            a.threads.list_scroll = s;
            for l in render_to_text(&mut a, w, 30).unwrap() {
                assert!(l.chars().count() <= w as usize, "{w}: {l:?}");
            }
            open_thread(&mut a, "8e4fca10");
            a.threads.chat_scroll = s;
            for l in render_to_text(&mut a, w, 30).unwrap() {
                assert!(l.chars().count() <= w as usize, "{w}: {l:?}");
            }
        }
    }
}
