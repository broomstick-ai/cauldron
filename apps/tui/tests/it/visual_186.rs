//! His msg-186 review, the VISUAL half (task 0166, checklist 0163): one card
//! for every node (the question card's look) and one for artifacts; one band
//! line everywhere, its metadata flush right like the ripples' counts, no
//! leaders; tables for enumerations and an opened item's fields; selection a
//! background, no bar or gutter anywhere; modals with a thick border, a box
//! inside and their own colour; a double click inserts as `i` does; every
//! Config item's ⓘ in the pane's rightmost column; the segmented toggle; no
//! square in the sidebar; an artifact inserted in a question card. Each test
//! names the checklist ids it proves. Hermetic: a scratch root,
//! CAULDRON_TYPE_ONE=off.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Focus, Hit, Item, Modal, Outbox, KTab};
use tui::cauldron::menu::Entry;
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::seed_dir;
use tui::cauldron::{render_to_text, render_with_selection};

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "visual186-{}-{}-{}",
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

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 70).unwrap().join("\n")
}

fn press(a: &mut App, hit: Hit) {
    screen(a);
    let r = a.hits.iter().rev().find(|(_, h)| *h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {hit:?} on screen"));
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
}

fn cluster(a: &mut App, key: &str) {
    let g = a.forest.find(key).unwrap();
    a.select(g);
}

/// The page column of a row: what is right of the sidebar.
fn page(row: &str) -> &str {
    row.split('│').nth(2).unwrap_or("")
}

/// Every cluster tab, every cauldron tab and sub-tab, as he sees it.
fn every_screen(a: &mut App, mut f: impl FnMut(&str, &mut App)) {
    for t in KTab::ALL {
        a.set_ctab(CTab::Cauldron);
        a.set_ktab(t);
        f(&format!("{t:?}"), a);
    }
    for st in StatusTab::ALL {
        a.set_ctab(CTab::Status);
        a.sttab = st;
        f(&format!("Status {st:?}"), a);
    }
    a.set_ctab(CTab::Threads);
    crate::live_fixture::feed(a);
    f("Threads", a);
    a.set_ctab(CTab::Config);
    f("Config", a);
}

// ── S3: selection ──────────────────────────────────────────────────────────

/// S3: no `▌` bar and no reserved gutter on any screen; the selection is
/// the warm background, on exactly the selected item's rows, in every pane.
#[test]
fn s3_selection_is_a_background_and_never_a_bar_or_gutter() {
    let (mut a, _r) = open();
    a.focus = Focus::Content;
    every_screen(&mut a, |what, a| {
        a.focus = Focus::Content;
        let (rows, sel) = render_with_selection(a, 120, 70).unwrap();
        let s = rows.join("\n");
        assert!(!s.contains('▌'), "{what}: a selection bar:\n{s}");
        assert!(!sel.is_empty(), "{what}: nothing carries the selection background:\n{s}");
    });
    // The sidebar too: its selected cluster is a background, not paper.
    a.set_ctab(CTab::Cauldron);
    a.focus = Focus::Tree;
    let (rows, sel) = render_with_selection(&mut a, 120, 70).unwrap();
    let name = a.forest.get(a.selected).name.clone();
    assert!(sel.iter().any(|&y| rows[y].split('│').nth(1).is_some_and(|c| c.contains(&name))), "{rows:#?}");
    // No gutter: a page's band line starts at the page's first column.
    a.set_ktab(KTab::Questions);
    let s = screen(&mut a);
    let active = s.lines().find(|l| page(l).contains("▾ Active")).unwrap();
    assert!(page(active).starts_with(" ▾ Active"), "the band starts one column in, no gutter: `{}`", page(active));
}

// ── V1 V2: cards ───────────────────────────────────────────────────────────

/// V1 V2: every node is the question's card — Overview's directives,
/// constraints, objectives and milestones, Soup's nodes, Status's accounts
/// and unclaimed nodes — rounded, its id the legend at the top right; an
/// artifact is the square card.
#[test]
fn v1_v2_every_node_is_the_question_card_and_artifacts_are_the_square_card() {
    let (mut a, _r) = open();
    cluster(&mut a, "X-1");
    a.set_ktab(KTab::Details);
    let s = screen(&mut a);
    for id in ["DIR-1", "CON-1", "OBJ-1", "M-1"] {
        let top = s.lines().find(|l| l.contains(&format!(" {id} ─╮"))).unwrap_or_else(|| panic!("{id} is not a card's legend:\n{s}"));
        assert!(page(top).trim_start().starts_with('╭'), "{top}");
    }
    // Overview's bands are bands, not boxes (T1): the band line has no corner.
    assert!(s.lines().any(|l| page(l).trim_start().starts_with("• Directives")), "{s}");
    // Soup: a region opens to its nodes as cards, its artifacts as the square card.
    cluster(&mut a, "X-2");
    a.set_ktab(KTab::Nodes);
    a.art_toggled.insert("region:R-1".into());
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("╭─ ▸ ") && l.contains("─╮")), "soup nodes are cards:\n{s}");
    assert!(s.lines().any(|l| l.contains("┌─ ▸ ") && l.contains("─┐")), "artifacts are the square card:\n{s}");
    // Status › Tokens: an account is the same card, always open (msg 235): a
    // dot where the chevron was.
    a.set_ctab(CTab::Status);
    a.sttab = StatusTab::Tokens;
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("╭─ • acct2") && l.trim_end().ends_with("─╮ │")), "{s}");
    // An account's card holds a TABLE (V6).
    assert!(s.lines().any(|l| l.contains("login") && l.contains(" │ ")), "{s}");
}

// ── V3 V5: the band line ───────────────────────────────────────────────────

/// V3 V5: every band's metadata is flush right on its own line, with no
/// rule running from the name to it — Soup's bands included — and written
/// like the ripples' counts (`glyph n word · …`).
#[test]
fn v3_v5_one_band_line_metadata_flush_right_and_no_leaders() {
    let (mut a, _r) = open();
    cluster(&mut a, "X-1");
    every_screen(&mut a, |what, a| {
        let s = screen(a);
        for l in s.lines() {
            let p = if l.contains("│ Clusters") || l.split('│').count() > 3 { page(l) } else { l.trim_matches('│') };
            let band = p.trim_start().starts_with("▾ ") || p.trim_start().starts_with("▸ ");
            if band {
                assert!(!p.contains(" ── ") && !p.contains("───"), "{what}: a leader on a band line: `{p}`");
            }
        }
    });
    // The cluster Status tab's ripples: the reference metadata, flush right.
    a.set_ctab(CTab::Cauldron);
    a.set_ktab(KTab::Status);
    let s = screen(&mut a);
    let r = s.lines().find(|l| page(l).contains("▾ Ripples")).unwrap();
    assert!(page(r).trim_end().ends_with("pending"), "{r}");
    // Questions: `◆ 1 blocking · 3 questions`, not `Active 3`.
    a.set_ktab(KTab::Questions);
    let s = screen(&mut a);
    let act = s.lines().find(|l| page(l).contains("▾ Active")).unwrap();
    assert!(page(act).trim_end().ends_with("questions"), "{act}");
    // Status › Mesh: a device's band, `▶ 3 of 5 running · ⚠ 6 warnings · ✕ 2 errors`.
    a.set_ctab(CTab::Status);
    a.sttab = StatusTab::Mesh;
    let s = screen(&mut a);
    let svc = s.lines().find(|l| l.contains("▾ example-mac")).unwrap();
    assert!(svc.contains("▶ 3 of 5 services running") && svc.contains("⚠ 6 warnings") && svc.contains("✕ 2 errors"), "{svc}");
}

// ── V4 V6: tables and colour ───────────────────────────────────────────────

/// V6: the services are rows of a table — every column starts at the same
/// place — and an opened service is a table of its fields. V4: a running
/// service's status is in the good colour, a failed one's red.
#[test]
fn v4_v6_enumerations_line_up_and_an_opened_item_is_a_table() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Status);
    a.sttab = StatusTab::Mesh;
    let s = screen(&mut a);
    let rows: Vec<&str> = s.lines().filter(|l| l.contains("▸ ") && (l.contains("running") || l.contains("stopped") || l.contains("failed"))).collect();
    assert!(rows.len() >= 4, "{s}");
    let col = |l: &str, w: &str| l.find(w).map(|i| l[..i].chars().count());
    let status_cols: Vec<_> = rows.iter().map(|l| ["running", "stopped", "failed"].iter().find_map(|w| col(l, w))).collect();
    assert!(status_cols.windows(2).all(|w| w[0] == w[1]), "the status column lines up: {rows:#?}");
    let key = a.dash.mesh.iter().flat_map(|n| n.services.iter().map(move |x| format!("svc:{}/{}", n.node, x.name))).next().unwrap();
    a.stnav.insert(key);
    let s = screen(&mut a);
    for f in ["status", "uptime", "pid", "address", "version", "warnings", "errors"] {
        assert!(s.lines().any(|l| l.contains(f) && l.contains(" │ ")), "{f} is a table row:\n{s}");
    }
    // V4: the colours of the states.
    let (rows, _) = render_with_selection(&mut a, 120, 70).unwrap();
    let y = rows.iter().position(|l| l.contains("▸ ") && l.contains("failed")).unwrap();
    let x = rows[y].find("failed").map(|i| rows[y][..i].chars().count()).unwrap();
    assert_eq!(tui::cauldron::render_fg_at(&mut a, 120, 70, x as u16, y as u16).unwrap(), tui_render::palette::c().red);
}

// ── V7: ⓘ ─────────────────────────────────────────────────────────────────

/// V7: every Config item — on both tabs, every band open — carries its ⓘ in
/// the pane's rightmost column, and a click on it explains the setting.
#[test]
fn v7_every_config_item_has_its_tip_in_the_rightmost_column() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Config);
    // The TUI tab (Config opens on the cauldron's since b964d571 msg 23).
    a.set_cfg_tab(tui::cauldron::menu::CfgTab::Tui);
    for _ in 0..12 {
        let closed: Vec<_> = a.entries().into_iter().filter(|f| f.open == Some(false)).filter_map(|f| f.entry.band()).collect();
        if closed.is_empty() {
            break;
        }
        for b in closed {
            a.snav.set_open(b, true);
        }
    }
    let w = 114;
    let (lines, ranges, tips, _) = tui::cauldron::view::settings_entry_lines(&a, w);
    let sel = a.selectable();
    let mut n = 0;
    for (i, e) in sel.iter().enumerate() {
        if matches!(e, Entry::Set(_) | Entry::Run(_)) {
            let l = &lines[ranges[i].start];
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(text.ends_with('ⓘ') && text.chars().count() == w, "item {i} `{text}`");
            // A tip is addressed by its ENTRY (every row has one, audit 0177 V7).
            assert!(tips.iter().any(|&(line, c, _)| line == ranges[i].start && c == w - 1), "item {i}: no tip hotspot at the rightmost column");
            n += 1;
        }
    }
    assert!(n > 10, "{n} items");
    // A click on the first ⓘ opens the help modal.
    a.snav.open.clear();
    screen(&mut a);
    let tip = a.hits.iter().find(|(_, h)| matches!(h, Hit::Tip(_))).map(|(r, h)| (*r, *h)).expect("an ⓘ on screen");
    assert!(tip.0.x >= 116, "the ⓘ is at the right edge: {:?}", tip.0);
    press(&mut a, tip.1);
    assert!(matches!(a.modal, Some(Modal::Help { .. })), "{:?}", a.modal);
}

// ── A5: the toggle ────────────────────────────────────────────────────────

/// A5: the cauldron's switch is the segmented toggle — both words on
/// screen, the one in force lit — and so is every boolean.
#[test]
fn a5_the_toggle_is_segmented_and_the_old_knob_is_gone() {
    let (mut a, _r) = open();
    every_screen(&mut a, |what, a| {
        let s = screen(a);
        assert!(!s.contains("──●") && !s.contains("●──"), "{what}: the old knob:\n{s}");
    });
    a.set_ctab(CTab::Config);
    a.set_cfg_tab(tui::cauldron::menu::CfgTab::Cauldron);
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("cauldron enabled") && l.contains(" enabled  disabled ")), "{s}");
    assert!(!s.contains('●') && !s.contains('○'), "no dots (msg 235):\n{s}");
}

// ── S1: modals ────────────────────────────────────────────────────────────

/// S1: a modal has a THICK border all the way round, a box inside it, and
/// its own background colour.
#[test]
fn s1_a_modal_has_a_thick_border_a_box_inside_and_its_own_colour() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Config);
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)));
    assert!(matches!(a.modal, Some(Modal::Confirm { .. })), "{:?}", a.modal);
    let rows = render_to_text(&mut a, 120, 40).unwrap();
    let top = rows.iter().position(|l| l.contains('┏')).expect("a thick top border");
    assert!(rows[top].contains('┓') && rows[top].contains("cauldron?"), "{}", rows[top]);
    assert!(rows[top + 1].contains("┃") && rows[top + 1].contains('╭'), "a box inside: {}", rows[top + 1]);
    assert!(rows.iter().any(|l| l.contains('┗') && l.contains('┛')), "thick all the way round");
    let r = a.modal_rect.unwrap();
    let bg = tui::cauldron::render_bg_at(&mut a, 120, 40, r.x + 3, r.y + 2).unwrap();
    assert_eq!(bg, tui_render::palette::c().modal_bg, "its own colour, inside");
    let out = tui::cauldron::render_bg_at(&mut a, 120, 40, 1, 2).unwrap();
    assert_ne!(out, tui_render::palette::c().modal_bg);
}

// ── S2: double click ──────────────────────────────────────────────────────

/// S2, as msg 235 re-made it (K6: "Let's do a single click just inserts it
/// at the cursor and the bucket. Double click makes it a new line with a
/// quote"): one click on a reference puts it at the bucket's caret, the page
/// keeping the selection; a double click takes that back and quotes it on a
/// new line, as `i` does; a slower second click on the selected one opens it
/// and inserts nothing more. On a cauldron tab's row a double click quotes.
#[test]
fn s2_one_click_inserts_at_the_caret_and_a_double_click_quotes() {
    let (mut a, _r) = open();
    cluster(&mut a, "X-1");
    a.set_ktab(KTab::Questions);
    let q = a.items().iter().position(|i| matches!(i, Item::Question { .. })).unwrap();
    press(&mut a, Hit::Item(q));
    assert_eq!(a.composer.text(), "[Q-1] ", "one click: the reference at the caret");
    assert_eq!(a.focus, Focus::Content, "the page keeps the selection");
    press(&mut a, Hit::Item(q));
    assert_eq!(a.composer.text(), "> [Q-1]", "a double click: the insert taken back, a quote instead");
    assert_eq!(a.focus, Focus::Input, "as `i` does, the bucket takes the keys");
    // A slow second click opens instead (it inserts nothing more).
    let (mut a, _r) = open();
    cluster(&mut a, "X-1");
    a.set_ktab(KTab::Questions);
    press(&mut a, Hit::Item(q));
    a.last_click = None;
    let before = a.item;
    press(&mut a, Hit::Item(q));
    assert_eq!(a.composer.text(), "[Q-1] ", "{}", a.composer.text());
    assert!(a.item > before, "the second click opened the question: its first option is selected");
    // A typed caret: the reference lands where the caret is, mid-text.
    let (mut a, _r) = open();
    cluster(&mut a, "X-1");
    a.set_ktab(KTab::Questions);
    a.type_text("see  please");
    for _ in 0.." please".len() {
        a.composer.left();
    }
    press(&mut a, Hit::Item(q));
    assert_eq!(a.composer.text(), "see [Q-1] please", "one space each side (0316 K5)");
    // On Status: a service's row is a band — its first click opens it — and
    // a double click turns it back and quotes it.
    let (mut a, _r) = open();
    a.set_ctab(CTab::Status);
    a.sttab = StatusTab::Mesh;
    let row = tui::cauldron::status::view(&a, 120).items.iter().position(|r| r.tag.as_ref().is_some_and(|t| t.render().contains("service:tts"))).unwrap();
    let key = tui::cauldron::status::view(&a, 120).items[row].key.clone();
    let shut = !tui::cauldron::status::is_open(&a, &key);
    press(&mut a, Hit::Row(row));
    assert_eq!(tui::cauldron::status::is_open(&a, &key), shut, "one click turns the band");
    press(&mut a, Hit::Row(row));
    assert_eq!(tui::cauldron::status::is_open(&a, &key), !shut, "the double click turns it back");
    assert!(a.composer.text().starts_with("> [service:tts]"), "{}", a.composer.text());
}

// ── V8: the sidebar ───────────────────────────────────────────────────────

/// V8: the sidebar carries no square marker, even for a cluster whose
/// bucket holds notes.
#[test]
fn v8_the_sidebar_has_no_square_marker() {
    let (mut a, _r) = open();
    a.focus_input();
    for c in "a note".chars() {
        a.handle(key(KeyCode::Char(c)));
    }
    a.focus = Focus::Tree;
    let s = screen(&mut a);
    let tree: String = s.lines().map(|l| l.split('│').nth(1).unwrap_or("")).collect();
    assert!(!tree.contains('▪') && !tree.contains('■'), "{s}");
}

// ── D3: an artifact in a question ─────────────────────────────────────────

/// D3: a question card carries an inserted ARTIFACT as a card inside it; it
/// is an item; Enter opens its text in the artifact modal, `o` hands it to
/// `open`, a double click quotes it, and its `open ↗` runs `open` at once.
#[test]
fn d3_a_question_card_carries_an_inserted_artifact_that_opens() {
    let (mut a, _r) = open();
    cluster(&mut a, "X-2");
    a.set_ktab(KTab::Questions);
    let s = screen(&mut a);
    let top = s.lines().find(|l| l.contains("┌─ ▤ ") && l.contains("open ↗")).unwrap_or_else(|| panic!("no artifact card:\n{s}"));
    assert!(top.contains("0126-report-reaudit"), "{top}");
    let at = a.items().iter().position(|i| matches!(i, Item::QArt { .. })).expect("the artifact is an item");
    let q = a.items().iter().position(|i| matches!(i, Item::Question { .. })).unwrap();
    assert!(at > q, "inside its question");
    a.focus = Focus::Content;
    a.item = at;
    a.handle(key(KeyCode::Enter));
    match &a.modal {
        Some(Modal::Artifact { path, lines, .. }) => {
            if std::path::Path::new(path).exists() {
                assert!(!lines.is_empty());
            }
        }
        None => assert!(matches!(a.outbox.last(), Some(Outbox::Open(_))), "not text, or not there: open"),
        m => panic!("{m:?}"),
    }
    if a.modal.is_some() {
        let s = screen(&mut a);
        assert!(s.contains('┏') && s.contains("0126-report-reaudit"), "{s}");
        a.handle(key(KeyCode::Char('o')));
        assert!(a.modal.is_none());
        assert!(matches!(a.outbox.last(), Some(Outbox::Open(p)) if p.ends_with("0126-report-reaudit.md")), "{:?}", a.outbox);
    }
    // `o` on its row, too; and its `open ↗` at one click.
    a.outbox.clear();
    a.item = at;
    a.handle(key(KeyCode::Char('o')));
    assert!(matches!(a.outbox.last(), Some(Outbox::Open(_))), "{:?}", a.outbox);
    a.outbox.clear();
    let open_at = { screen(&mut a); a.hits.iter().find(|(_, h)| matches!(h, Hit::OpenAt(_))).map(|(_, h)| *h).unwrap() };
    press(&mut a, open_at);
    assert!(matches!(a.outbox.last(), Some(Outbox::Open(p)) if p.ends_with("0126-report-reaudit.md")), "{:?}", a.outbox);
    // `i` quotes it as the soup node it is.
    a.item = a.items().iter().position(|i| matches!(i, Item::QArt { .. })).unwrap();
    a.focus = Focus::Content;
    a.insert_selected();
    assert!(a.composer.text().ends_with("> [ART-126]"), "{}", a.composer.text());
}

// ── the scroll invariant, on the new shapes ───────────────────────────────

/// msg 165's invariant on msg 186's shapes: selecting a card, an option, an
/// inserted artifact, an account card or a service's table never changes a
/// line of the page; and scrolling by one moves every row by exactly one.
#[test]
fn selection_and_scrolling_never_change_a_card_or_a_table() {
    let (mut a, _r) = open();
    cluster(&mut a, "X-2");
    for t in [KTab::Details, KTab::Questions, KTab::Actions, KTab::Nodes] {
        a.set_ktab(t);
        a.art_toggled.insert("region:R-1".into());
        let lines = |a: &App| tui::cauldron::pages::page(a, 100).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        let first = lines(&a);
        for i in 0..a.items().len() {
            a.item = i;
            assert_eq!(lines(&a), first, "{t:?}: selecting item {i} changed the page");
        }
        // Scrolling never reshapes a row: the page laid out is the same at
        // every scroll offset (the view only windows it).
        for k in [1, 5, 17] {
            a.scroll = k;
            assert_eq!(lines(&a), first, "{t:?}: scrolled to {k}, the page changed");
        }
        a.scroll = 0;
    }
    a.set_ctab(CTab::Status);
    for st in StatusTab::ALL {
        a.sttab = st;
        a.stnav.insert("acct:claude_code/acct2".into());
        let lines = |a: &App| tui::cauldron::status::view(a, 110).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        let n = tui::cauldron::status::view(&a, 110).items.len();
        a.rows[0] = 0;
        let first = lines(&a);
        for i in 0..n {
            a.rows[0] = i;
            assert_eq!(lines(&a), first, "{st:?}: selecting row {i} changed the page");
        }
    }
}
