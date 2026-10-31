//! The cauldron Config tab and the cluster Config tab — collapsible bands
//! over every setting (8e4fca10 msg 165) — driven headlessly by real key and
//! mouse events through `App::handle`, drawn to ratatui's TestBackend,
//! against real files under a scratch cauldron root (never `~/.cauldron`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use tui::cauldron::app::{App, CTab, Focus, Hit, Modal, KTab};
use tui::cauldron::menu::{Bound, Entry, Nav, SPage, SETTINGS_ROW};
use tui::cauldron::render_to_text;
use tui::cauldron::settings::{Ctx, Val, DEFS};
use tui::cauldron::store::{seed_dir, soup_dir};
use tui::cauldron::toggle::{Toggle, WIDTH};

// ── fixtures ────────────────────────────────────────────────────────────────

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "settings-test-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path, name: &str) -> Environment {
    Environment {
        root: root.to_path_buf(),
        root_source: Source::CauldronHome,
        name: "prod".into(),
        name_source: Source::Default,
        data_root: root.join("data").join("prod"),
    }
    .for_name(name)
    .unwrap()
}

fn open(root: &Path, name: &str) -> App {
    // Hermetic: no test here may make a billed type-one call, whatever is on
    // PATH (a cluster made here seeds; tests/it/seed_live.rs is the live proof).
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(root, name), seed_dir(), true).expect("seed loads")
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn alt(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::ALT))
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(key(KeyCode::Char(c)));
    }
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 80).unwrap().join("\n")
}

/// Runs of spaces as one: columns are layout, not content.
fn c(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The screen row holding `needle`, trimmed of the frame, its columns
/// collapsed to single spaces.
fn row(a: &mut App, needle: &str) -> String {
    let s = screen(a);
    let found = s.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("`{needle}` not on screen:\n{s}"));
    c(found.trim_matches(|ch| ch == '│' || ch == ' '))
}

/// Assert a row holding `needle` contains `want` (columns collapsed).
fn assert_row(a: &mut App, needle: &str, want: &str) {
    let s = screen(a);
    let rows: Vec<String> = s.lines().filter(|l| l.contains(needle)).map(c).collect();
    assert!(!rows.is_empty(), "`{needle}` not on screen:\n{s}");
    assert!(rows.iter().any(|r| r.contains(&c(want))), "no `{needle}` row has `{}`: {rows:?}\n{s}", c(want));
}

/// What a selectable entry is called on its row.
fn label_of(e: &Entry) -> String {
    match e {
        Entry::Set(b) if b.key == "pools.{pool}.model.{model}" => b.ctx.model.clone(),
        Entry::Set(b) | Entry::Run(b) => b.def().label.to_string(),
        Entry::Row(p) => p.title(),
        Entry::Sub { label, .. } | Entry::Do { label, .. } | Entry::Ref { label, .. } => label.clone(),
        Entry::Add(tui::cauldron::menu::Add::Account(_)) => "+ add account".into(),
        Entry::Add(tui::cauldron::menu::Add::Item(_)) => "+ add".into(),
        Entry::Item { text, .. } => text.clone(),
        _ => String::new(),
    }
}

/// Select the FIRST Config row labelled `label`, as the bands are open:
/// Home, then ↓ until it is.
fn down_to(a: &mut App, label: &str) {
    a.handle(key(KeyCode::Home));
    for _ in 0..200 {
        if a.settings_entry().is_some_and(|e| label_of(&e) == label) {
            return;
        }
        a.handle(key(KeyCode::Down));
    }
    let shown: Vec<String> = a.selectable().iter().map(label_of).collect();
    panic!("no row `{label}` among {shown:?}");
}

/// The cauldron's Config with every band closed, then `path` opened.
fn config(a: &mut App, path: &str) {
    a.set_ctab(CTab::Config);
    a.snav = Nav::cauldron();
    a.rows[SETTINGS_ROW] = 0;
    if !path.is_empty() {
        a.open_settings_path(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    }
}

fn file(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(soup_dir(&env_in(root, "prod")).unwrap().join(rel)).unwrap_or_default()
}

/// A file of the GLOBAL settings (msg 186): the token sources, the
/// accounts added, the flip log — one copy for every cauldron.
fn global_file(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join("data").join("shared").join("tui").join(rel)).unwrap_or_default()
}

fn acct(id: &str) -> SPage {
    SPage::Account("claude_code".into(), id.into())
}

// ── the toggle switch ───────────────────────────────────────────────────────

#[test]
fn the_toggle_is_one_contiguous_control_with_the_option_in_force_lit() {
    // msg 235: "The whole toggle, all of the options in a toggle should have
    // a different background color to indicate that they're all one
    // contiguous toggle ... each of the items has one space of margin on the
    // left and the right and whichever one is selected has a different
    // background color ... just remove those dots".
    let mut term = Terminal::new(TestBackend::new(20, 3)).unwrap();
    term.draw(|f| {
        f.render_widget(Toggle::new(true), ratatui::layout::Rect::new(0, 0, 20, 1));
        f.render_widget(Toggle::new(false), ratatui::layout::Rect::new(0, 1, 20, 1));
        f.render_widget(Toggle::tri(None), ratatui::layout::Rect::new(0, 2, 20, 1));
    })
    .unwrap();
    let buf = term.backend().buffer().clone();
    let line = |y: u16| (0..20).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>();
    assert_eq!(line(0).trim_end(), " on  off");
    assert_eq!(line(1).trim_end(), " on  off");
    assert_eq!(line(2).trim_end(), " inherit  on  off", "inherit · on · off (msg 235)");
    assert!(!(0..3).any(|y| line(y).contains('●') || line(y).contains('○')), "no dots");
    // ONE background under every cell of it, the lit option's apart.
    let bg = |x: u16, y: u16| buf[(x, y)].bg;
    for x in 0..WIDTH as u16 {
        assert_ne!(bg(x, 0), tui_render::palette::c().surface, "cell {x} of the toggle is on the toggle's background");
    }
    assert_eq!(bg(0, 0), tui_render::palette::c().code_bg, "on: ` on ` lit");
    assert_eq!(bg(4, 0), tui_render::palette::c().toggle_bg, "on: ` off ` on the toggle's own background");
    assert_eq!((bg(0, 1), bg(4, 1)), (tui_render::palette::c().toggle_bg, tui_render::palette::c().code_bg), "off: ` off ` lit");
    assert!(buf[(1, 0)].modifier.contains(ratatui::style::Modifier::BOLD), "the option in force is bold");
    assert_eq!(buf[(1, 0)].fg, tui_render::palette::c().ink, "and bright");
    assert_eq!(bg(0, 2), tui_render::palette::c().code_bg, "inherit in force");
    // One width whatever the state, so the column after it lines up.
    assert_eq!((Toggle::new(true).width(), Toggle::new(false).width()), (WIDTH, WIDTH));
    assert_eq!(Toggle::tri(Some(true)).width(), Toggle::tri(None).width());
    // Each option's click hotspot is its own cells.
    assert_eq!(Toggle::tri(None).hotspots(), vec![(0, 9), (9, 4), (13, 5)]);
}

#[test]
fn every_setting_that_inherits_is_tri_state_and_the_rest_two() {
    // msg 235: "if there's an inherent option on any setting in any config
    // menu ... we're going to have inherit, on, and off".
    let mut a = open(&scratch_root(), "prod");
    a.handle(key(KeyCode::F(4)));
    config(&mut a, "providers/claude_code");
    assert_row(&mut a, "opportunistic", "opportunistic inherit on off");
    assert_row(&mut a, "failover", "failover on off default on");
    for d in DEFS.iter().filter(|d| d.ty == tui::cauldron::settings::Ty::Toggle) {
        let bd = tui::cauldron::menu::Bound::new(d.key, tui::cauldron::settings::Ctx::account("claude_code", "acct2"));
        let n = a.toggle_of(&bd).options.len();
        assert_eq!(n, if d.inherits.is_some() { 3 } else { 2 }, "{}", d.key);
    }
    // The cauldron's own switch says what it does (msg 235: "cauldron
    // enabled ... two options enabled. Or disabled").
    config(&mut a, "cauldron");
    assert_row(&mut a, "cauldron enabled", "cauldron enabled enabled disabled");
}

// ── the default, said; the revert glyph ─────────────────────────────────────

#[test]
fn a_row_says_its_default_and_reverts_only_while_it_differs() {
    // msg 235: "we should actually just say what the default is ... don't
    // even show that it's an override" and "you could have a little revert
    // ASCII character appear to the right of it. And then if you click on
    // that revert button, then it will revert to the default."
    let root = scratch_root();
    let mut a = open(&root, "prod");
    a.handle(key(KeyCode::F(4)));
    config(&mut a, "devices/storage");
    assert_row(&mut a, "archive before delete", "archive before delete on off default off");
    assert!(!row(&mut a, "archive before delete").contains('↺'));
    down_to(&mut a, "archive before delete");
    a.handle(key(KeyCode::Char(' ')));
    assert_row(&mut a, "archive before delete", "default off ↺");
    let s = screen(&mut a);
    assert!(!s.contains("override") && !s.contains("● ") && !s.contains("inherited ·"), "no override marker:\n{s}");
    // Back to off by hand: the default again — no ↺ — and kept as his
    // choice (0250, 0225 F1).
    a.handle(key(KeyCode::Char(' ')));
    assert!(!row(&mut a, "archive before delete").contains('↺'), "{}", row(&mut a, "archive before delete"));
    assert!(global_file(&root, "settings.toml").contains("archive_before_delete = false"), "{}", global_file(&root, "settings.toml"));
    // A number: set, then the ↺ CLICKED puts it back.
    down_to(&mut a, "threads cap");
    a.handle(key(KeyCode::Char('+')));
    assert_row(&mut a, "threads cap", "threads cap 11 GB default 10 GB ↺");
    let i = a.rows[SETTINGS_ROW];
    render_to_text(&mut a, 120, 80).unwrap();
    let (r, _) = *a.hits.iter().find(|(_, h)| *h == Hit::Revert(i as u16)).expect("the ↺ is a click target");
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
    assert_row(&mut a, "threads cap", "threads cap 10 GB default 10 GB");
    assert!(!row(&mut a, "threads cap").contains('↺'));
    // A setting that inherits says what inheriting gives.
    config(&mut a, "providers/claude_code");
    assert_row(&mut a, "weekly threshold", "weekly threshold 80 % default 80 %");
}

#[test]
fn a_click_on_an_option_picks_it() {
    let root = scratch_root();
    let mut a = open(&root, "prod");
    a.handle(key(KeyCode::F(4)));
    config(&mut a, "providers/claude_code");
    // The source's own (the shared default of the same name stands flat at
    // the head of Providers, task 0209 D4).
    assert!(a.select_setting("pools.{pool}.opportunistic.enabled"));
    let i = a.rows[SETTINGS_ROW];
    let opt = |a: &mut App, o: u8| {
        render_to_text(a, 120, 80).unwrap();
        let (r, _) = *a.hits.iter().find(|(_, h)| *h == Hit::Opt { row: i as u16, cell: 0, opt: o }).expect("the option is a click target");
        a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + 1, row: r.y, modifiers: KeyModifiers::NONE }));
    };
    opt(&mut a, 2);
    assert!(global_file(&root, "settings.toml").contains("[pools.claude_code.opportunistic]\nenabled = false"), "{}", global_file(&root, "settings.toml"));
    assert_row(&mut a, "opportunistic", "↺");
    opt(&mut a, 1);
    assert!(global_file(&root, "settings.toml").contains("[pools.claude_code.opportunistic]\nenabled = true"), "on is a choice apart from inherit");
    opt(&mut a, 0);
    assert!(!global_file(&root, "settings.toml").contains("[pools.claude_code.opportunistic]"), "inherit clears it");
    // Space steps inherit → on → off → inherit.
    for want in ["enabled = true", "enabled = false"] {
        a.handle(key(KeyCode::Char(' ')));
        assert!(global_file(&root, "settings.toml").contains(want), "{want}");
    }
    a.handle(key(KeyCode::Char(' ')));
    assert!(!global_file(&root, "settings.toml").contains("[pools.claude_code.opportunistic]"));
}

// ── a source's settings → a new account inherits them ───────────────────────

#[test]
fn a_source_is_its_accounts_table_its_settings_flat_and_its_models() {
    let root = scratch_root();
    let mut a = open(&root, "prod");
    a.handle(key(KeyCode::F(4)));
    // Config › Providers ▾ Claude Code, by keys: → on the strip to
    // Providers (the cauldron's tab first, b964d571 msg 23), → opens a band.
    a.rows[SETTINGS_ROW] = 0;
    a.handle(key(KeyCode::Right));
    a.handle(key(KeyCode::Right));
    a.handle(key(KeyCode::Right));
    assert_eq!(a.cfg_tab, tui::cauldron::menu::CfgTab::Providers);
    down_to(&mut a, "Claude Code");
    a.handle(key(KeyCode::Right));
    assert!(a.snav.is_open(&SPage::Pool("claude_code".into())));
    // msg 235: "first just show me a table of all of my accounts, and then
    // just show me a flattened list of all of the settings ... the exception
    // being models".
    let inside: Vec<Entry> = a.page_entries(&SPage::Pool("claude_code".into()));
    assert!(matches!(&inside[0], Entry::Row(p) if *p == acct("sam-broomstick")), "{inside:?}");
    assert!(inside.iter().all(|e| !matches!(e, Entry::Sub { label, .. } if label == "Defaults" || label == "Accounts" || label == "Opportunistic mode")), "no Defaults, Accounts or Opportunistic band");
    assert!(inside.iter().any(|e| matches!(e, Entry::Fixed(t, _) if t == "Models")));
    assert_row(&mut a, "account", "account enabled weekly threshold opportunistic");
    assert_row(&mut a, "sam-broomstick", "sam-broomstick inherit on off 80 % inherit on off ›");
    // prod's own DEFAULT of 80 (F41: in code, not seed data): no ↺.
    assert!(!row(&mut a, "weekly threshold").contains('↺'));
    assert!(a.select_setting("pools.{pool}.budget.weekly.threshold_pct"));
    a.handle(key(KeyCode::Enter));
    assert_eq!(a.field.as_ref().map(|f| f.text.as_str()), Some("80"), "the field starts at the value");
    a.handle(key(KeyCode::Backspace));
    a.handle(key(KeyCode::Backspace));
    typed(&mut a, "70");
    a.handle(key(KeyCode::Enter));
    assert!(a.field.is_none());
    assert_row(&mut a, "weekly threshold", "weekly threshold 70 % default 80 % ↺");
    assert!(global_file(&root, "settings.toml").contains("[pools.claude_code.budget.weekly]\nthreshold_pct = 70"), "{}", global_file(&root, "settings.toml"));
    // Every account's row follows.
    assert_row(&mut a, "acct2", "70 %");

    // A new account joins the source: written with no settings of its own.
    down_to(&mut a, "+ add account");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(a.modal, Some(Modal::AddAccount { .. })), "{:?}", a.modal);
    assert!(screen(&mut a).contains("Add an account to Claude Code"));
    typed(&mut a, "cc9");
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert!(matches!(a.settings_entry(), Some(Entry::Row(page)) if page == acct("cc9")), "the new account's row is selected");
    assert_row(&mut a, "cc9", "cc9 inherit on off 70 % inherit on off ›");
    assert!(global_file(&root, "accounts.json").contains("\"id\": \"cc9\""));
    assert!(!global_file(&root, "settings.toml").contains("cc9"), "a new account writes no settings");

    // Change the source's value again: the new account follows.
    assert!(a.select_setting("pools.{pool}.budget.weekly.threshold_pct"));
    a.handle(key(KeyCode::Char('+')));
    assert_row(&mut a, "cc9", "75 %");

    // The source's accounts switched off: every account inheriting it is.
    down_to(&mut a, "accounts enabled");
    a.handle(key(KeyCode::Char(' ')));
    assert!(!a.account_on("claude_code", "cc9"));
    assert!(a.account_on("claude_code", "sam-broomstick") == a.account_on("claude_code", "sam-broomstick"));
    down_to(&mut a, "+ add account");
    a.handle(key(KeyCode::Enter));
    typed(&mut a, "cc9");
    a.handle(key(KeyCode::Enter));
    let Some(Modal::AddAccount { error, .. }) = &a.modal else { panic!("{:?}", a.modal) };
    assert!(error.as_deref().unwrap_or("").contains("already in Claude Code"), "{error:?}");
}

#[test]
fn a_subscription_sources_models_are_detected_and_switched_never_added() {
    // msg 235: "we never add models to cloud code ... We just detect the
    // models ... we could have a toggle on each one of the models ... by
    // default, each one of them will be on" — an always-expanded band.
    let root = scratch_root();
    let mut a = open(&root, "prod");
    a.handle(key(KeyCode::F(4)));
    config(&mut a, "providers/claude_code");
    assert_row(&mut a, "Models", "• Models 4 of 4 on");
    for m in ["haiku", "sonnet", "opus", "fable"] {
        assert_row(&mut a, m, &format!("{m} on off default on"));
    }
    let s = screen(&mut a);
    assert!(!s.contains("add model") && !s.contains("▸ Models") && !s.contains("▾ Models"), "{s}");
    // The Models head is not a row: nothing selects or closes it.
    assert!(!a.selectable().iter().any(|e| matches!(e, Entry::Fixed(..))));
    down_to(&mut a, "opus");
    a.handle(key(KeyCode::Char(' ')));
    assert!(global_file(&root, "settings.toml").contains("[pools.claude_code.model]\nopus = false"), "{}", global_file(&root, "settings.toml"));
    assert_row(&mut a, "Models", "3 of 4 on");
    assert!(!a.pool_models("claude_code").contains(&"opus".to_string()), "a role is no longer offered it");
    // Codex gets the same (msg 235: "make similar changes").
    config(&mut a, "providers/codex");
    assert_row(&mut a, "Models", "• Models 3 of 3 on");
    assert_row(&mut a, "account", "account enabled weekly threshold opportunistic");
}

// ── an account: a row, its modal ─────────────────────────────────────────────

// ── an account override, and clearing it ────────────────────────────────────

#[test]
fn a_row_opens_its_modal_at_one_click_and_its_settings_work_there() {
    // msg 235: "account specific settings would be rows that you expand into
    // a modal with the settings ... that's what we should do for every
    // configurable row in anything that I call a table".
    let root = scratch_root();
    let mut a = open(&root, "prod");
    a.handle(key(KeyCode::F(4)));
    config(&mut a, "providers/claude_code");
    let i = a.selectable().iter().position(|e| *e == Entry::Row(acct("acct2"))).unwrap();
    render_to_text(&mut a, 120, 80).unwrap();
    let (r, _) = *a.hits.iter().find(|(_, h)| *h == Hit::Row(i)).expect("acct2's row on screen");
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + 2, row: r.y, modifiers: KeyModifiers::NONE }));
    assert!(matches!(&a.modal, Some(Modal::Settings { page, .. }) if *page == acct("acct2")), "{:?}", a.modal);
    let s = screen(&mut a);
    assert!(s.contains("┏ Claude Code · acct2 "), "{s}");
    assert_row(&mut a, "weekly threshold", "weekly threshold 80 % default 80 %");
    // ↓ to it, Enter types, Enter keeps.
    while a.modal_entry().is_some_and(|e| !matches!(e, Entry::Set(b) if b.key == "pools.{pool}.accounts.{account}.budget.weekly.threshold_pct")) {
        a.handle(key(KeyCode::Down));
    }
    a.handle(key(KeyCode::Enter));
    assert!(a.field.is_some());
    a.handle(key(KeyCode::Backspace));
    a.handle(key(KeyCode::Backspace));
    typed(&mut a, "60");
    a.handle(key(KeyCode::Enter));
    assert!(a.field.is_none() && matches!(a.modal, Some(Modal::Settings { .. })), "the modal stays");
    assert_row(&mut a, "weekly threshold", "weekly threshold 60 % inherits 80 % ↺");
    assert!(global_file(&root, "settings.toml").contains("[pools.claude_code.accounts.acct2.budget.weekly]\nthreshold_pct = 60"));
    // `x` reverts it.
    a.handle(key(KeyCode::Char('x')));
    assert_row(&mut a, "weekly threshold", "weekly threshold 80 % default 80 %");
    assert!(!global_file(&root, "settings.toml").contains("acct2.budget"), "{}", global_file(&root, "settings.toml"));
    // A value out of range is refused on its row, and the field stays open.
    a.handle(key(KeyCode::Enter));
    typed(&mut a, "0");
    a.handle(key(KeyCode::Enter));
    assert!(a.field.is_some());
    assert!(screen(&mut a).contains("✗ a percent 0–100, or off"), "{}", screen(&mut a));
    a.handle(key(KeyCode::Esc));
    // A picker asked from the modal comes back to it.
    a.handle(key(KeyCode::Home));
    while a.modal_entry().is_some_and(|e| !matches!(e, Entry::Set(b) if b.key == "pools.{pool}.accounts.{account}.share")) {
        a.handle(key(KeyCode::Down));
    }
    a.handle(key(KeyCode::Char('-')));
    assert_row(&mut a, "share", "25 % default 100 % ↺");
    a.handle(key(KeyCode::Char('h')));
    assert!(matches!(&a.modal, Some(Modal::Help { title, .. }) if title == "share"), "{:?}", a.modal);
    a.handle(key(KeyCode::Esc));
    assert!(matches!(&a.modal, Some(Modal::Settings { page, .. }) if *page == acct("acct2")), "back to the row's modal: {:?}", a.modal);
    // The modal's switches work: a click on `off` of its own switch.
    render_to_text(&mut a, 120, 80).unwrap();
    let (r, _) = *a.hits.iter().find(|(_, h)| *h == Hit::MOpt { row: 0, cell: 0, opt: 2 }).expect("account enabled's `off`");
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + 1, row: r.y, modifiers: KeyModifiers::NONE }));
    // A save edits the file as it is (0220 E3), so a new key goes after the
    // table's others.
    let g = global_file(&root, "settings.toml");
    let acct2 = g.split("[pools.claude_code.accounts.acct2]\n").nth(1).and_then(|t| t.split("\n\n").next()).unwrap_or_default();
    assert!(acct2.lines().any(|l| l == "enabled = false") && acct2.lines().any(|l| l == "share = 25"), "{g}");
    // Esc closes it; the row is still selected.
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none());
    assert_eq!(a.settings_entry(), Some(Entry::Row(acct("acct2"))));
    // Enter opens it again.
    a.handle(key(KeyCode::Enter));
    assert!(matches!(&a.modal, Some(Modal::Settings { .. })));
}

#[test]
fn a_rows_switch_works_on_the_row() {
    // msg 235: "you've got the on-off in the account row, so you can't
    // actually change it. It's just an illusion" — now it is a switch.
    let root = scratch_root();
    let mut a = open(&root, "prod");
    a.handle(key(KeyCode::F(4)));
    config(&mut a, "cauldron/accounts");
    let i = a.selectable().iter().position(|e| matches!(e, Entry::Row(SPage::UseAccount(_, x)) if x == "acct2")).unwrap();
    render_to_text(&mut a, 120, 80).unwrap();
    // `used here`'s `off` (a tri-state now, b964d571 msg 23: inherit · on ·
    // off), then `opportunistic`'s `off`.
    for (cell, opt) in [(0u8, 2u8), (1, 2)] {
        let (r, _) = *a.hits.iter().find(|(_, h)| *h == Hit::Opt { row: i as u16, cell, opt }).expect("a switch's option on the row");
        a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + 1, row: r.y, modifiers: KeyModifiers::NONE }));
        render_to_text(&mut a, 120, 80).unwrap();
    }
    assert!(a.modal.is_none(), "a switch on a row is not the row: {:?}", a.modal);
    assert!(!a.account_allowed("claude_code", "acct2"));
    let f = file(&root, "settings.toml");
    assert!(f.contains("[cauldron.accounts.claude_code.acct2]\nallowed = false\nopportunistic = false"), "{f}");
    // Space on the row steps its first switch.
    a.rows[SETTINGS_ROW] = i;
    a.handle(key(KeyCode::Char(' ')));
    assert!(a.account_allowed("claude_code", "acct2"));
}

// ── a cluster override ─────────────────────────────────────────────────────────

/// msg 235: a cluster has no settings — no Config tab, no cluster key —
/// and no cluster key is in the taxonomy.
#[test]
fn a_cluster_has_no_settings() {
    assert!(DEFS.iter().all(|d| !d.key.starts_with("cluster.") && !d.key.starts_with("budgets.cluster_default")));
    assert_eq!(KTab::parse("config"), None);
}

// ── persistence ─────────────────────────────────────────────────────────────

#[test]
fn settings_survive_a_restart_and_stay_in_their_environment() {
    // RAM max defaults to 75% of THIS machine's RAM (F44).
    let ram = tui::cauldron::settings::machine_ram_gb().map_or(48, |g| g * 3 / 4);
    let root = scratch_root();
    std::fs::create_dir_all(root.join("data/dev")).unwrap();
    {
        let mut a = open(&root, "prod");
        a.handle(key(KeyCode::F(4)));
        a.open_settings_path("machine").unwrap();
        down_to(&mut a, "RAM max");
        a.handle(key(KeyCode::Char('+')));
        // A role is a row of its table: its modal opens, its model first.
        a.open_settings_path("roles/curator").unwrap();
        assert!(matches!(&a.modal, Some(Modal::Settings { page: SPage::Role(r), .. }) if r == "curator"), "{:?}", a.modal);
        a.handle(key(KeyCode::Enter));
        let Some(Modal::Picker { options, .. }) = &a.modal else { panic!("{:?}", a.modal) };
        let opus = options.iter().position(|o| o == "opus").unwrap();
        a.handle(key(KeyCode::Home));
        for _ in 0..opus {
            a.handle(key(KeyCode::Down));
        }
        a.handle(key(KeyCode::Right));
        assert!(matches!(&a.modal, Some(Modal::Settings { .. })), "the pick goes back to the role's modal: {:?}", a.modal);
    }
    let mut a = open(&root, "prod");
    a.handle(key(KeyCode::F(4)));
    a.open_settings_path("machine").unwrap();
    assert_row(&mut a, "RAM max", &format!("RAM max {} GB default {ram} GB", ram + 1));
    assert!(row(&mut a, "RAM max").contains('↺'));
    a.open_settings_path("roles").unwrap();
    assert_row(&mut a, "curator", "curator opus none Claude Code ›");

    // dev: the machine is GLOBAL (msg 235: "this machine ... all that stuff
    // belongs in global") — dev reads the same value; a role is THIS
    // cauldron's own, so dev's curator is its default.
    let mut d = open(&root, "dev");
    d.handle(key(KeyCode::F(4)));
    d.open_settings_path("machine").unwrap();
    assert_row(&mut d, "RAM max", &format!("RAM max {} GB", ram + 1));
    d.open_settings_path("roles").unwrap();
    assert_row(&mut d, "curator", "curator sonnet none Claude Code ›");
    // The token sources are GLOBAL (msg 186): dev reads the same ones.
    d.open_settings_path("providers/claude_code").unwrap();
    assert_row(&mut d, "weekly threshold", "weekly threshold 80 % default 80 %");
    assert!(!root.join("data/dev/tui/soup/settings.toml").exists());

    // The headless dump reads it all and writes nothing.
    let before = global_file(&root, "settings.toml");
    let mut h = App::open(env_in(&root, "prod"), seed_dir(), false).unwrap();
    h.handle(key(KeyCode::F(4)));
    h.open_settings_path("machine").unwrap();
    down_to(&mut h, "RAM max");
    h.handle(key(KeyCode::Char('+')));
    assert_row(&mut h, "RAM max", &format!("RAM max {} GB", ram + 2));
    assert_eq!(global_file(&root, "settings.toml"), before);
}

// ── navigation (docs/navigation.md) ─────────────────────────────────────────

#[test]
fn settings_bands_navigate_by_the_one_philosophy() {
    let root = scratch_root();
    let mut a = open(&root, "prod");
    a.handle(key(KeyCode::F(4)));
    a.set_cfg_tab(tui::cauldron::menu::CfgTab::Devices);
    // No breadcrumb: every band header is on the page, closed.
    let s = screen(&mut a);
    assert!(!s.contains(" › "), "{s}");
    assert!(a.snav.open.is_empty());
    // Rule 4, as bands: → expands (the header stays selected), → again steps
    // in, ← climbs to the header, ← collapses, ← on a closed top band stays.
    down_to(&mut a, "Storage & retention");
    let at = a.rows[SETTINGS_ROW];
    assert_row(&mut a, "Storage & retention", "▸ Storage & retention");
    a.handle(key(KeyCode::Right));
    assert!(a.snav.is_open(&SPage::Storage));
    assert_eq!(a.rows[SETTINGS_ROW], at);
    assert_row(&mut a, "Storage & retention", "▾ Storage & retention");
    a.handle(key(KeyCode::Right));
    assert_eq!(a.rows[SETTINGS_ROW], at + 1, "→ on an open band steps to its first row");
    // Its first row is Garbage collection, store-wide (0211 R8); then its
    // THREADS section, a band like any other (audit 0177 V3): open, → steps
    // into it and ← climbs back to its header.
    assert_row(&mut a, "Garbage collection", "▸ Garbage collection");
    a.handle(key(KeyCode::Down));
    assert_row(&mut a, "Threads", "▾ Threads");
    a.handle(key(KeyCode::Right));
    assert_eq!(a.rows[SETTINGS_ROW], at + 3, "→ on an open section steps to its first row");
    a.handle(key(KeyCode::Left));
    assert_eq!(a.rows[SETTINGS_ROW], at + 2, "← climbs to the band's header");
    a.handle(key(KeyCode::Left));
    assert_row(&mut a, "Threads", "▸ Threads");
    a.handle(key(KeyCode::Left));
    assert_eq!(a.rows[SETTINGS_ROW], at, "← on a closed section climbs to its page's band");
    a.handle(key(KeyCode::Right));
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::Right));
    assert_row(&mut a, "Threads", "▾ Threads");
    a.handle(key(KeyCode::Left));
    a.handle(key(KeyCode::Left));
    assert!(a.snav.is_open(&SPage::Storage));
    a.handle(key(KeyCode::Left));
    assert!(!a.snav.is_open(&SPage::Storage), "← collapses it");
    a.handle(key(KeyCode::Left));
    assert_eq!((a.ctab, a.rows[SETTINGS_ROW]), (CTab::Config, at), "← never leaves the tab");

    // Rule 6: ⌥↓ / ⌥↑ jump between the sections of what is open.
    a.handle(key(KeyCode::Right));
    a.handle(alt(KeyCode::Down));
    assert!(matches!(a.settings_entry(), Some(Entry::Sub { label, .. }) if label == "Threads"));
    a.handle(alt(KeyCode::Down));
    assert!(matches!(a.settings_entry(), Some(Entry::Sub { label, .. }) if label == "Logs"));
    a.handle(alt(KeyCode::Up));
    a.handle(alt(KeyCode::Up));
    assert_eq!(a.rows[SETTINGS_ROW], at);

    // Rule 2: letters are commands outside the dump; nothing is typed.
    a.handle(key(KeyCode::Char('x')));
    a.handle(key(KeyCode::Char('+')));
    assert!(a.composer.is_blank());

    // Rule 3: the dump is not on Config (msg 165): ↓ at the last row stays.
    a.handle(key(KeyCode::End));
    let last = a.rows[SETTINGS_ROW];
    a.handle(key(KeyCode::Down));
    assert_eq!((a.focus, a.rows[SETTINGS_ROW]), (Focus::Content, last));
    // Rule 7: Esc does nothing here, and never quits.
    a.handle(key(KeyCode::Esc));
    assert_eq!((a.ctab, a.focus), (CTab::Config, Focus::Content));
    assert!(!a.quit);
    // `a` goes to the dump, on Work; coming back, the bands are reset
    // (ui.preserve.config is off by default).
    a.handle(key(KeyCode::Char('a')));
    assert_eq!((a.ctab, a.focus), (CTab::Cauldron, Focus::Input));
    a.handle(key(KeyCode::F(4)));
    assert!(a.snav.open.is_empty() && a.rows[SETTINGS_ROW] == 0, "{:?}", a.snav);

    // A toggle row: Space, Enter and → each flip it.
    a.open_settings_path("storage").unwrap();
    down_to(&mut a, "archive before delete");
    let here = a.rows[SETTINGS_ROW];
    let archive = tui::cauldron::menu::Bound::new("storage.threads.archive_before_delete", Default::default());
    for (k, on) in [(KeyCode::Char(' '), true), (KeyCode::Enter, false), (KeyCode::Right, true)] {
        a.handle(key(k));
        assert_eq!(a.sget(&archive).val.as_bool(), on, "{k:?}");
    }
    assert_eq!(a.rows[SETTINGS_ROW], here, "flipping does not move");

    // Space on an account's row steps its switch without opening it:
    // acct3's `off` (the seed's) → `inherit`, which is on.
    a.open_settings_path("providers/claude_code").unwrap();
    down_to(&mut a, "acct3");
    assert!(!a.account_on("claude_code", "acct3"));
    a.handle(key(KeyCode::Char(' ')));
    assert!(a.modal.is_none());
    assert!(a.account_on("claude_code", "acct3"));

    // Rule 8: a picker takes every key — ↑↓ move, ← escapes, → chooses.
    a.open_settings_path("providers/claude_code").unwrap();
    down_to(&mut a, "selection");
    a.handle(key(KeyCode::Enter));
    let Some(Modal::Picker { selected, options, .. }) = &a.modal else { panic!("{:?}", a.modal) };
    assert_eq!((options[*selected].as_str(), options.len()), ("min_risk", 3));
    assert!(screen(&mut a).contains("┏ selection "));
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::Left));
    assert!(a.modal.is_none());
    assert_row(&mut a, "selection", "min risk");
    a.handle(key(KeyCode::Right));
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::Right));
    assert!(a.modal.is_none());
    assert_row(&mut a, "selection", "selection round robin default min risk ↺");

    // A value field, in a row's modal: Esc leaves it and KEEPS what was
    // typed, and the modal stays.
    a.open_settings_path("providers/claude_code/acct2").unwrap();
    while a.modal_entry().is_some_and(|e| !matches!(e, Entry::Set(b) if b.key == "pools.{pool}.accounts.{account}.share")) {
        a.handle(key(KeyCode::Down));
    }
    a.handle(key(KeyCode::Enter));
    a.handle(key(KeyCode::Backspace));
    a.handle(key(KeyCode::Backspace));
    typed(&mut a, "40");
    a.handle(key(KeyCode::Esc));
    assert!(a.field.is_none());
    assert!(matches!(a.modal, Some(Modal::Settings { .. })), "the Esc left the field, not the modal");
    assert_row(&mut a, "share", "40 %");
    // ↓ out of a field keeps it and moves.
    a.handle(key(KeyCode::Enter));
    a.handle(key(KeyCode::Backspace));
    typed(&mut a, "5");
    a.handle(key(KeyCode::Down));
    assert_row(&mut a, "share", "45 %");
    assert!(matches!(a.modal_entry(), Some(Entry::Set(b)) if b.key == "pools.{pool}.accounts.{account}.max_concurrent"));

    // `h` explains the selected setting, in a modal (msg 165: the only place
    // the explanation is); closing it goes back to the row's modal.
    a.handle(key(KeyCode::Char('h')));
    assert!(matches!(&a.modal, Some(Modal::Help { title, .. }) if title == "max concurrent"), "{:?}", a.modal);
    a.handle(key(KeyCode::Esc));
    assert!(matches!(a.modal, Some(Modal::Settings { .. })), "{:?}", a.modal);
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none());

    // I1 (msg 235): a band toggles on ONE click; a second click toggles it back.
    let codex = SPage::Pool("codex".into());
    let i = a.selectable().iter().position(|e| matches!(e, Entry::Sub { page, .. } if *page == codex)).unwrap();
    render_to_text(&mut a, 120, 160).unwrap();
    let (r, _) = *a.hits.iter().find(|(_, h)| *h == Hit::Row(i)).expect("Codex's row on screen");
    let click = |a: &mut App| {
        a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + 3, row: r.y, modifiers: KeyModifiers::NONE }));
    };
    click(&mut a);
    assert_eq!(a.rows[SETTINGS_ROW], i);
    assert!(a.snav.is_open(&codex), "one click expands it");
    a.last_click = None;
    click(&mut a);
    assert!(!a.snav.is_open(&codex), "the next click collapses it");

    // The cluster tab: ← at its top closes its open band, and ← again hands
    // back to the sidebar.
    a.set_ctab(CTab::Cauldron);
    a.focus = Focus::Tree;
    a.handle(key(KeyCode::Tab));
    while a.ktab != KTab::Status {
        a.handle(key(KeyCode::Tab));
    }
    a.handle(key(KeyCode::Right));
    assert_eq!(a.focus, Focus::Content);
    a.handle(key(KeyCode::Left));
    assert_eq!(a.focus, Focus::Content, "the first ← closes the band");
    a.handle(key(KeyCode::Left));
    assert_eq!(a.focus, Focus::Tree);
}

// ── completeness ────────────────────────────────────────────────────────────

/// Every setting the design lists is in some band — walked from each root by
/// opening every band — and so is editable there.
#[test]
fn every_setting_in_the_design_is_in_a_band() {
    let mut a = open(&scratch_root(), "prod");
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    // An endpoint's add modal (design 0199 §3, §4), as each template draws
    // it, globally and here: its template and server are set only there.
    for here in [false, true] {
        for t in ["openrouter", "local"] {
            a.settings.draft_endpoint(here, t);
            let explicit = tui::cauldron::menu::ep_bound(here, tui::cauldron::settings::DRAFT_EP, "models");
            a.sset(&explicit, Val::Str("explicit".into())).unwrap();
            for e in a.page_entries(&SPage::Endpoint { here, id: tui::cauldron::settings::DRAFT_EP.into() }) {
                match e {
                    Entry::Set(b) => {
                        seen.insert(b.key);
                    }
                    Entry::Add(tui::cauldron::menu::Add::Item(b)) => {
                        seen.insert(b.key);
                    }
                    _ => {}
                }
            }
        }
    }
    a.settings.discard_draft();
    let mut pages = 0;
    for root in tui::cauldron::menu::CfgTab::ALL.map(|t| t.root()) {
        let mut todo = vec![root];
        let mut done: Vec<SPage> = Vec::new();
        while let Some(p) = todo.pop() {
            if done.contains(&p) {
                continue;
            }
            pages += 1;
            for e in a.page_entries(&p) {
                if let Some(band) = e.band() {
                    todo.push(band);
                }
                match e {
                    // A table's row: its settings are in its modal.
                    Entry::Row(p) => todo.push(p),
                    Entry::Set(b) | Entry::Run(b) => {
                        seen.insert(b.key);
                    }
                    // A list shown open in its band (TRACE ROOTS): its items
                    // and its `+ add` edit it there.
                    Entry::Add(tui::cauldron::menu::Add::Item(b)) | Entry::Item { list: b, .. } => {
                        seen.insert(b.key);
                    }
                    _ => {}
                }
            }
            done.push(p);
        }
    }
    // The cauldron's own switch is THIS cauldron's tab's FIRST row (msg 186,
    // msg 235: "the first setting `cauldron enabled`").
    assert!(matches!(a.page_entries(&tui::cauldron::menu::CfgTab::Cauldron.root()).first(), Some(Entry::Set(b)) if b.key == "cauldron.enabled"));
    // A cauldron's narrowed list of a global endpoint's models.
    a.sset(&Bound::new("cauldron.use.{ep}.narrow", Ctx::ep("ep-2")), Val::Bool(true)).unwrap();
    for e in a.page_entries(&SPage::UseEndpoint("ep-2".into())) {
        if let Entry::Add(tui::cauldron::menu::Add::Item(b)) = e {
            seen.insert(b.key);
        }
    }
    // Bookkeeping no row sets (the last endpoint id minted) is in no band.
    let missing: Vec<&str> = DEFS.iter().filter(|d| !d.hidden).map(|d| d.key).filter(|k| !seen.contains(k)).collect();
    assert!(missing.is_empty(), "not in any band: {missing:?}");
    assert!(pages > 30, "{pages}");
    let on = |a: &App| a.cauldron_on("prod");
    let was = on(&a);
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)));
    assert_eq!(on(&a), was, "^T asks first");
    a.handle(key(KeyCode::Enter));
    assert_ne!(on(&a), was, "^T, confirmed, flips cauldron.enabled");
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)));
    a.handle(key(KeyCode::Enter));
    // Every band fits a narrow terminal.
    for path in [
        "providers/claude_code",
        "providers/claude_code/acct2",
        "providers/endpoints/ep-1",
        "providers",
        "cauldron/accounts",
        "cauldron/accounts/acct2",
        "cauldron/endpoints/ep-1",
        "cauldron/directories",
        "cauldron/roles/type_one.secondary",
        "devices/storage/gc",
        "devices/mesh/supervisor",
        "tui/attention",
        "permissions/identity",
    ] {
        config(&mut a, path);
        for w in [60u16, 80, 110] {
            for r in render_to_text(&mut a, w, 40).unwrap() {
                assert!(r.chars().count() <= w as usize, "{path} at {w}: {r}");
            }
        }
    }
    // `run now` on a fresh cauldron: nothing of the face's is garbage, and
    // it says so rather than pretend a pass ran.
    a.modal = None;
    config(&mut a, "storage/gc");
    down_to(&mut a, "run now");
    a.handle(key(KeyCode::Enter));
    assert!(a.notice.as_deref().unwrap_or("").contains("nothing to reclaim"), "{:?}", a.notice);
}

/// Storage › GC (0214 A2): the page shows what a pass would reclaim from the
/// face's own files; `run now` asks first, with what it takes, and the
/// confirm performs it — on screen and on disk. A dry
/// run only says. And the notice a MOUSE press raises survives its release.
/// Since 0217 B1 a pass takes only a stale layout / dev-names temp file:
/// a paste nothing names and a bucket's half-written temp are his and stay.
#[test]
fn gc_run_now_reclaims_the_faces_own_garbage_after_a_confirm() {
    let root = scratch_root();
    let mut a = open(&root, "prod");
    let dir = root.join("data/prod/tui/soup");
    std::fs::create_dir_all(dir.join("bucket/artifacts")).unwrap();
    std::fs::write(dir.join("bucket/artifacts/dead1234.txt"), "a paste nothing names").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    let age = |p: &Path| std::fs::File::options().write(true).open(p).unwrap().set_modified(old).unwrap();
    let his_tmp = dir.join(".bucket.0000.tmp");
    std::fs::write(&his_tmp, "half a write").unwrap();
    age(&his_tmp);
    let stale = dir.join(".layout.json.0000.tmp");
    std::fs::write(&stale, "{\"sidebar\":1}").unwrap();
    age(&stale);
    config(&mut a, "storage/gc");
    assert_eq!(a.gc_plan().unwrap().summary(), "13 B · 1 item");
    assert!(!screen(&mut a).contains("reclaimable"), "no live reading in Config (0217 B7)");
    // A dry run says what it would take, and takes nothing.
    a.sset(&Bound::new("storage.gc.dry_run", Ctx::default()), Val::Bool(true)).unwrap();
    down_to(&mut a, "run now");
    a.handle(key(KeyCode::Enter));
    assert!(a.notice.as_deref().unwrap_or("").contains("dry run, nothing deleted"), "{:?}", a.notice);
    assert!(stale.exists());
    a.sset(&Bound::new("storage.gc.dry_run", Ctx::default()), Val::Bool(false)).unwrap();
    // The confirm, with what it takes, and its two buttons.
    a.handle(key(KeyCode::Enter));
    let Some(Modal::Confirm { title, body, .. }) = a.modal.clone() else { panic!("{:?}", a.modal) };
    assert_eq!(title, "Collect the garbage?");
    assert!(body.iter().any(|l| l.contains("left-over temp files: 1 file")) && body.iter().any(|l| l.contains("Nothing you wrote is ever touched")), "{body:?}");
    let s = screen(&mut a);
    assert!(s.contains(" collect ") && s.contains(" cancel "), "{s}");
    // By mouse: press and release on `collect`.
    let (yes, _) = *a.hits.iter().find(|(_, h)| *h == Hit::ConfirmButton(0)).expect("the collect button");
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: yes.x, row: yes.y, modifiers: KeyModifiers::NONE }));
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: yes.x, row: yes.y, modifiers: KeyModifiers::NONE }));
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert!(screen(&mut a).contains("garbage collection reclaimed 13 B"), "the notice outlives the release:\n{}", screen(&mut a));
    assert!(!stale.exists());
    assert!(his_tmp.exists() && dir.join("bucket/artifacts/dead1234.txt").exists(), "his paste and his half-written bucket stay");
    assert!(a.gc_plan().unwrap().is_empty());
}

/// 0217 B1 (HIGH): GC never deletes what a person wrote. An OLDER face keeps
/// writing the old bucket files after this face adopted them; that text
/// survives every GC path — `run now` and its confirm, a dry run, the
/// interval, disk pressure — and is RE-ADOPTED into the bucket, live and on
/// the next open, once. A bucket adopted by a build before the record
/// existed takes in what it is missing too.
#[test]
fn b1_gc_never_takes_his_words_and_an_older_faces_writes_are_readopted() {
    let root = scratch_root();
    let dir = root.join("data/prod/tui/soup");
    std::fs::create_dir_all(dir.join("drafts/system/artifacts")).unwrap();
    let draft = |t: &str| format!(r#"{{"nonce":"{}","writer":"tui --surfaces pid 9864","updated_at":"t","text":{}}}"#, uuid::Uuid::new_v4(), serde_json::to_string(t).unwrap());
    let (sys, env_draft) = (dir.join("drafts/system.json"), dir.join("draft.json"));
    std::fs::write(&sys, draft("first words")).unwrap();
    std::fs::write(&env_draft, draft("the env draft")).unwrap();
    let mut a = open(&root, "prod");
    assert_eq!(a.composer.text(), "first words\n\nthe env draft");
    assert!(dir.join("adopted.json").is_file(), "what was taken in is recorded");
    // The older face (pid 9864) types on, after adoption: an append to one
    // file, a rewrite of the other, a paste with its body.
    let appended = draft("first words and LATE WORDS ONE");
    let rewritten = draft("REWRITTEN TWO [paste dddd4444: \"body\" 4 chars]");
    std::fs::write(&sys, &appended).unwrap();
    std::fs::write(&env_draft, &rewritten).unwrap();
    std::fs::write(dir.join("drafts/system/artifacts/dddd4444.txt"), "body").unwrap();
    let old_files = |why: &str| {
        assert_eq!(std::fs::read_to_string(&sys).unwrap(), appended, "{why}: drafts/system.json");
        assert_eq!(std::fs::read_to_string(&env_draft).unwrap(), rewritten, "{why}: draft.json");
        assert!(dir.join("drafts/system/artifacts/dddd4444.txt").is_file(), "{why}: its chip");
    };
    // Nothing of it is in a plan.
    let plan = a.gc_plan().unwrap();
    assert!(plan.is_empty(), "{plan:#?}");
    // `run now`, with something else to take so the confirm is reached.
    let stale = dir.join(".layout.json.0001.tmp");
    std::fs::write(&stale, "{}").unwrap();
    std::fs::File::options().write(true).open(&stale).unwrap().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(600)).unwrap();
    config(&mut a, "storage/gc");
    a.sset(&Bound::new("storage.gc.dry_run", Ctx::default()), Val::Bool(true)).unwrap();
    down_to(&mut a, "run now");
    a.handle(key(KeyCode::Enter));
    old_files("dry run");
    a.sset(&Bound::new("storage.gc.dry_run", Ctx::default()), Val::Bool(false)).unwrap();
    a.handle(key(KeyCode::Enter));
    let Some(Modal::Confirm { body, .. }) = a.modal.clone() else { panic!("{:?}", a.modal) };
    assert!(!body.iter().any(|l| l.contains("adopted") || l.contains("draft")), "{body:?}");
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none() && !stale.exists(), "{:?}", a.modal);
    old_files("run now, confirmed");
    // The interval, then disk pressure: they delete NOTHING, not even the
    // temp file a confirm would take.
    std::fs::write(&stale, "{}").unwrap();
    std::fs::File::options().write(true).open(&stale).unwrap().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(600)).unwrap();
    // (The first quiet tick re-adopts what the older face wrote, and says so.)
    a.sync();
    assert_eq!(a.notice.as_deref(), Some("text an older face wrote came into the bucket"));
    old_files("re-adoption");
    a.gc_at = std::time::Instant::now() - std::time::Duration::from_secs(7 * 3600);
    a.sync();
    old_files("interval");
    assert!(stale.exists(), "an automatic pass deletes nothing");
    assert!(a.notice.as_deref().unwrap_or("").contains("nothing is deleted until you confirm"), "{:?}", a.notice);
    a.sset(&Bound::new("machine.disk.free_floor_gb", Ctx::default()), Val::Int(1_000_000)).unwrap();
    a.gc_looked = std::time::Instant::now() - std::time::Duration::from_secs(120);
    a.sync();
    old_files("disk pressure");
    assert!(stale.exists());
    // And the older face's words came into the bucket, live, once each.
    let t = a.composer.text();
    assert!(t.contains("LATE WORDS ONE") && t.contains("REWRITTEN TWO"), "{t}");
    let bucket = std::fs::read_to_string(dir.join("bucket.json")).unwrap();
    assert!(bucket.contains("LATE WORDS ONE") && bucket.contains("REWRITTEN TWO"), "{bucket}");
    assert_eq!(t.matches("first words").count(), 1, "{t}");
    assert_eq!(a.chips.read("dddd4444").as_deref(), Some("body"), "its chip came with it");
    // The next face finds them once, not adopted twice.
    let b = open(&root, "prod");
    assert_eq!(b.composer.text(), t);
    // While no new face runs, the older one writes again: the next open
    // takes it in.
    std::fs::write(&sys, draft("first words and LATE WORDS ONE and THREE")).unwrap();
    let c = open(&root, "prod");
    assert!(c.composer.text().ends_with("and THREE"), "{}", c.composer.text());
    assert_eq!(c.composer.text().matches("LATE WORDS ONE").count(), 1, "{}", c.composer.text());
    // A bucket adopted by the build before the record (c6c5afba): no
    // adopted.json, and the older face wrote on after it.
    let root = scratch_root();
    let dir = root.join("data/prod/tui/soup");
    std::fs::create_dir_all(dir.join("drafts")).unwrap();
    std::fs::write(dir.join("drafts/system.json"), draft("adopted then\n\nTYPED AFTER")).unwrap();
    std::fs::write(dir.join("bucket.json"), draft("adopted then\n\nand his edits in the bucket")).unwrap();
    let d = open(&root, "prod");
    let t = d.composer.text();
    assert!(t.starts_with("adopted then\n\nand his edits in the bucket") && t.contains("TYPED AFTER"), "{t}");
}

// ── Status reads what Config says ───────────────────────────────────────────

#[test]
fn the_status_tokens_band_shows_what_the_settings_say() {
    let mut a = open(&scratch_root(), "prod");
    a.handle(key(KeyCode::F(3)));
    assert_eq!(a.ctab, CTab::Status);
    a.set_sttab(tui::cauldron::status::StatusTab::Tokens);
    // acct3 is switched off in its settings: Status says so on its band,
    // as its one state word (design 0142).
    assert_row(&mut a, "acct3", "DISABLED");
    assert!(!row(&mut a, "sam-broomstick").contains("DISABLED"), "{}", row(&mut a, "sam-broomstick"));
    // Its `config ▸` opens its row's modal on Config › Providers.
    let v = tui::cauldron::status::view(&a, 120);
    a.rows[0] = v.items.iter().position(|i| i.key == "cfg:claude_code/acct3").expect("config ▸ under acct3");
    a.handle(key(KeyCode::Enter));
    assert_eq!(a.ctab, CTab::Config);
    assert_eq!(a.settings_entry(), Some(Entry::Row(acct("acct3"))));
    assert!(matches!(&a.modal, Some(Modal::Settings { page, .. }) if *page == acct("acct3")), "{:?}", a.modal);
    // Switched on there (its `off` steps to `inherit`, which is on), Status
    // follows.
    a.handle(key(KeyCode::Char(' ')));
    a.handle(key(KeyCode::Esc));
    a.handle(key(KeyCode::F(3)));
    // On, its login is what stops it.
    assert_row(&mut a, "acct3", "LOGIN EXPIRED");
}

// ── C9: inference endpoints (msg 235; design 0199; task 0192) ──────────────

fn cfg(a: &mut App, path: &str) {
    a.set_ctab(CTab::Config);
    a.focus = Focus::Content;
    a.snav = Nav::cauldron();
    a.rows[SETTINGS_ROW] = 0;
    a.modal = None;
    a.open_settings_path(path).unwrap_or_else(|e| panic!("{path}: {e}"));
}

/// Put the open row modal's selection on its entry matching `f`.
fn msel(a: &mut App, f: impl Fn(&Entry) -> bool) {
    let Some(Modal::Settings { page, .. }) = a.modal.clone() else { panic!("no row modal: {:?}", a.modal) };
    let i = a.modal_selectable(&page).iter().position(f).unwrap_or_else(|| panic!("not in {page:?}'s modal"));
    if let Some(Modal::Settings { sel, .. }) = &mut a.modal {
        *sel = i;
    }
}

fn is_set(field: &'static str) -> impl Fn(&Entry) -> bool {
    move |e| matches!(e, Entry::Set(b) if b.key.ends_with(&format!(".{field}")))
}

fn ep(here: bool, id: &str, field: &str) -> Bound {
    tui::cauldron::menu::ep_bound(here, id, field)
}

fn global_text(a: &App) -> String {
    std::fs::read_to_string(a.settings.global_path()).unwrap_or_default()
}

/// C9 (msg 235: "this whole open router section should just be like
/// basically like a list of like API keys ... we can call it like inference
/// endpoints"): Providers has no OpenRouter source and nothing planned; it
/// has INFERENCE ENDPOINTS, a table — name, url, models, budget, its switch,
/// `›` — then `+ add endpoint` as a button, then the defaults flat. No
/// spend on it (C6).
#[test]
fn c9_providers_holds_the_endpoints_table_and_no_openrouter_source() {
    let mut a = open(&scratch_root(), "prod");
    cfg(&mut a, "providers");
    let s = screen(&mut a);
    assert!(!s.contains("OpenRouter") && !s.contains("planned") && !s.contains("add token source"), "{s}");
    assert_row(&mut a, "▸ Inference endpoints", "5 endpoints · 4 on");
    cfg(&mut a, "providers/endpoints");
    let s = screen(&mut a);
    assert_row(&mut a, "endpoint", "endpoint url models budget enabled");
    assert_row(&mut a, "jev ", "jev openrouter.ai/api/v1 anthropic/claude-sonnet-4.5 $100 / month on off ›");
    assert_row(&mut a, "anthropic ", "anthropic api.anthropic.com/v1 claude-opus-5-5 +1 $200 / month");
    assert_row(&mut a, "gemini ", "generativelanguage.google… any $20 / week");
    assert_row(&mut a, "studio-box ", "198.51.100.5:11434/v1 any off");
    // The rows are a table (msg 235 V6): its column names once, each row its
    // own row, a button after them, then the defaults.
    let rows: Vec<Entry> = a.selectable().into_iter().filter(|e| matches!(e, Entry::Row(SPage::Endpoint { .. }))).collect();
    assert_eq!(rows.len(), 5);
    assert!(a.selectable().iter().any(|e| matches!(e, Entry::Do { label, .. } if label == "+ add endpoint") && e.is_button()));
    for k in ["budget period", "threshold", "at threshold", "max concurrent"] {
        assert!(s.contains(k), "{k}: {s}");
    }
    assert_row(&mut a, "at threshold", "park new park all default park new");
    for w in ["spent", "$12.40", "resets"] {
        assert!(!s.contains(w), "{w} is Status's: {s}");
    }
    // `i` quotes an endpoint as itself.
    let at = a.selectable().iter().position(|e| *e == Entry::Row(SPage::Endpoint { here: false, id: "ep-2".into() })).unwrap();
    assert_eq!(a.setting_tag(at).unwrap().render(), "[endpoint:ep-2]");
}

/// C9: a row's switch works on the row (msg 235: no illusions) and its
/// modal holds every row of the endpoint — the key masked, its template a
/// fact, its headers and models editable lines, its budget and period, what
/// enforces it, its management key — and nothing secret reaches the screen.
#[test]
fn c9_an_endpoints_row_switches_and_its_modal_holds_every_row() {
    let mut a = open(&scratch_root(), "prod");
    cfg(&mut a, "providers/endpoints");
    let gem = SPage::Endpoint { here: false, id: "ep-4".into() };
    a.rows[SETTINGS_ROW] = a.selectable().iter().position(|e| *e == Entry::Row(gem.clone())).unwrap();
    assert!(!a.sget(&ep(false, "ep-4", "enabled")).val.as_bool());
    a.handle(key(KeyCode::Char(' ')));
    assert!(a.sget(&ep(false, "ep-4", "enabled")).val.as_bool(), "Space switched gemini on, on its row");
    // The seed's `enabled = false` is read, never deleted: his `on` is kept
    // as his choice over it (0250, 0225 F1), and nothing of the seed is
    // written (task 0209 D1).
    assert!(global_text(&a).contains("[endpoints.ep-4]\nenabled = true") && !global_text(&a).contains("FAKE"), "{}", global_text(&a));
    // Its modal.
    cfg(&mut a, "providers/endpoints/ep-1");
    assert!(matches!(&a.modal, Some(Modal::Settings { page: SPage::Endpoint { here: false, id }, .. }) if id == "ep-1"));
    let s = screen(&mut a);
    assert!(s.contains("Endpoint · jev · global"), "{s}");
    for (k, v) in [
        ("template", "OpenRouter"),
        ("url", "https://openrouter.ai/api/v1 default openrouter.ai/api/v1"),
        ("key", "sk-or-v1-••••••••••••••••c81e set"),
        ("headers", "1 header"),
        ("budget", "$100 default off ↺"),
        ("budget period", "daily weekly monthly none inherits monthly"),
        ("enforced by", "openrouter · cauldron meter"),
        ("max concurrent", "4 inherits unlimited ↺"),
        ("management key", "sk-or-v1-••••••••••••••••4f9c set"),
        ("used by", "type one · secondary"),
    ] {
        assert_row(&mut a, &format!(" {k} "), &format!("{k} {v}"));
    }
    assert!(s.contains("· X-OpenRouter-Title: Cauldron") && s.contains("· anthropic/claude-sonnet-4.5"), "{s}");
    assert!(s.contains("+ add header") && s.contains("+ add model") && s.contains("remove endpoint"), "{s}");
    assert!(!s.contains("FAKE0000"), "no key on screen: {s}");
    // A model is added — its exact name, checked for shape only — and
    // taken off with x.
    msel(&mut a, |e| matches!(e, Entry::Add(tui::cauldron::menu::Add::Item(b)) if b.key.ends_with("model_list")));
    a.handle(key(KeyCode::Enter));
    typed(&mut a, "not a slug");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(&a.modal, Some(Modal::AddItem { error: Some(e), .. }) if e.contains("vendor/model")), "{:?}", a.modal);
    a.handle(key(KeyCode::Esc));
    msel(&mut a, |e| matches!(e, Entry::Add(tui::cauldron::menu::Add::Item(b)) if b.key.ends_with("model_list")));
    a.handle(key(KeyCode::Enter));
    typed(&mut a, "deepseek/deepseek-v4.1-flash-1017");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(a.modal, Some(Modal::Settings { .. })), "back in the endpoint's modal: {:?}", a.modal);
    assert_eq!(a.sget(&ep(false, "ep-1", "model_list")).val, Val::List(vec!["anthropic/claude-sonnet-4.5".into(), "deepseek/deepseek-v4.1-flash-1017".into()]));
    msel(&mut a, |e| matches!(e, Entry::Item { text, .. } if text == "anthropic/claude-sonnet-4.5"));
    // x asks on the row; a second x takes it off (0220 E4).
    a.handle(key(KeyCode::Char('x')));
    assert_eq!(a.sget(&ep(false, "ep-1", "model_list")).val.as_list().len(), 2, "one x only asks");
    a.handle(key(KeyCode::Char('x')));
    assert_eq!(a.sget(&ep(false, "ep-1", "model_list")).val, Val::List(vec!["deepseek/deepseek-v4.1-flash-1017".into()]));
    // A key is never reverted or cleared away: x on it does nothing.
    msel(&mut a, is_set("key"));
    a.handle(key(KeyCode::Char('x')));
    assert!(a.sis_set(&ep(false, "ep-1", "key")));
    // A click on a strip's option picks it.
    msel(&mut a, is_set("budget_period"));
    screen(&mut a);
    let Some(Modal::Settings { sel, .. }) = a.modal.clone() else { unreachable!() };
    let r = a.hits.iter().find(|(_, h)| *h == Hit::MOpt { row: sel as u16, cell: 0, opt: 1 }).map(|(r, _)| *r).expect("the strip's `weekly` hotspot");
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
    assert_eq!(a.sget(&ep(false, "ep-1", "budget_period")).val, Val::Str("weekly".into()));
    assert!(a.ep_dollars(false, "ep-1", &Val::Int(100)).ends_with("/ week"));
}

/// C9 (msg 235: "when you're creating a new one we can have a modal where
/// it's like you're selecting like open router ... you can select like a
/// local one"): `+ add endpoint` opens the add modal over a template strip;
/// the template fills every row not typed into and keeps every row typed;
/// it adds only once its template has what it needs; it is never written
/// before it is added, and Esc forgets it.
#[test]
fn c9_an_endpoint_is_added_from_a_template_and_forgotten_on_esc() {
    let mut a = open(&scratch_root(), "prod");
    cfg(&mut a, "providers/endpoints");
    a.rows[SETTINGS_ROW] = a.selectable().iter().position(|e| matches!(e, Entry::Do { label, .. } if label == "+ add endpoint")).unwrap();
    a.handle(key(KeyCode::Enter));
    let s = screen(&mut a);
    assert!(s.contains("New endpoint · global"), "{s}");
    assert_row(&mut a, " template ", "template OpenRouter Anthropic API OpenAI Gemini Local server Custom");
    assert_row(&mut a, "add endpoint ", "add endpoint needs a key");
    // Gemini, picked on the strip: its url and name.
    let t = ep(false, "ep-0", "template");
    a.set_option(&t, 3);
    assert_eq!(a.sget(&ep(false, "ep-0", "url")).val, Val::Str("https://generativelanguage.googleapis.com/v1beta/openai/".into()));
    assert_eq!(a.ep_name(false, "ep-0"), "gemini");
    // A key pasted, then OpenAI: the url moves, the typed key stays.
    msel(&mut a, is_set("key"));
    a.handle(key(KeyCode::Enter));
    a.handle(Event::Paste("sk-FAKE-test-0000000000000000000000001234".into()));
    a.handle(key(KeyCode::Enter));
    assert!(!screen(&mut a).contains("FAKE-test"), "the pasted key is masked");
    // This face's own save set off a re-read: the endpoint being added is in
    // no file, and lives on in the face.
    a.reload_settings().unwrap();
    assert!(a.sis_set(&ep(false, "ep-0", "key")) && a.sis_set(&ep(false, "ep-0", "template")));
    // A url is a url.
    let u = ep(false, "ep-0", "url");
    assert!(tui::cauldron::settings::parse_input(u.def().ty, "openrouter.ai").is_err());
    assert!(tui::cauldron::settings::parse_input(u.def().ty, "http://198.51.100.5:11434/v1").is_ok());
    a.set_option(&t, 2);
    assert_eq!(a.sget(&ep(false, "ep-0", "url")).val, Val::Str("https://api.openai.com/v1".into()));
    assert!(a.sis_set(&ep(false, "ep-0", "key")), "the key he typed stays");
    // Nothing is written while it is being added.
    assert!(!global_text(&a).contains("ep-0") && !global_text(&a).contains("FAKE-test"), "{}", global_text(&a));
    // Added: the next id of the one series, the table's last row, written.
    msel(&mut a, |e| matches!(e, Entry::Do { label, .. } if label == "add endpoint"));
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert_eq!(a.settings.endpoint_ids(false).last().map(String::as_str), Some("ep-7"));
    assert!(matches!(a.settings_entry(), Some(Entry::Row(SPage::Endpoint { id, .. })) if id == "ep-7"), "the modal closes onto its row");
    let g = global_text(&a);
    assert!(g.contains("[endpoints.ep-7]") && g.contains("template = \"openai\"") && g.contains("last_id = 7") && !g.contains("ep-0"), "{g}");
    // Esc forgets one being added.
    a.start_act(tui::cauldron::manage::Act::NewEndpoint { here: false });
    a.set_option(&t, 5);
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none());
    assert!(!a.sis_set(&t), "forgotten");
    assert_eq!(a.settings.endpoint_ids(false).len(), 6);
}

/// C9 (design 0199 §4): the local template has a server strip that fills its
/// url and its key rule — Ollama's client needs a key its server ignores —
/// prices its tokens, and takes no headers.
#[test]
fn c9_a_local_endpoint_takes_its_url_and_key_from_its_server() {
    let mut a = open(&scratch_root(), "prod");
    a.start_act(tui::cauldron::manage::Act::NewEndpoint { here: true });
    a.set_option(&ep(true, "ep-0", "template"), 4);
    let s = screen(&mut a);
    assert!(s.contains("New endpoint · prod cauldron"), "{s}");
    assert_row(&mut a, " server ", "server Ollama llama.cpp vLLM");
    assert_row(&mut a, " key ", "key ollama");
    assert_row(&mut a, " price in / 1M ", "price in / 1M $0 default $0");
    assert!(!s.contains(" headers ") && !s.contains("management key"), "{s}");
    assert_eq!(a.draft_missing(true), None, "ollama's stand-in key satisfies it");
    a.set_option(&ep(true, "ep-0", "server"), 2);
    assert_eq!(a.sget(&ep(true, "ep-0", "url")).val, Val::Str("http://localhost:8000/v1".into()));
    assert_eq!(a.sget(&ep(true, "ep-0", "key")).val, Val::Unset, "vLLM's key is optional");
    // A price takes cents.
    let p = ep(true, "ep-0", "price_out");
    assert!(a.sset(&p, tui::cauldron::settings::parse_input(p.def().ty, "0.15").unwrap()).is_ok());
    assert_eq!(a.value_text(&p), "$0.15");
    assert!(tui::cauldron::settings::parse_input(p.def().ty, "-1").is_err());
}

/// C9 (design 0199 §5, §6): THIS cauldron's table is every endpoint it can
/// see — the global ones with a tri-state `use here` and its metered cap,
/// its own with a plain switch — and a global one's modal sets only what a
/// cauldron may: use here (never on where Providers has it off), its url,
/// a NARROWER model list, its metered cap, threshold, max concurrent.
#[test]
fn c9_this_cauldron_uses_global_endpoints_and_registers_its_own() {
    let mut a = open(&scratch_root(), "prod");
    cfg(&mut a, "cauldron/endpoints");
    assert_row(&mut a, "▾ Inference endpoints", "6 endpoints · 4 used here · 1 added here");
    assert_row(&mut a, "endpoint ", "endpoint scope url metered cap use here");
    assert_row(&mut a, "jev ", "jev global openrouter.ai/api/v1 $60 / month inherit on off ›");
    assert_row(&mut a, "scratch-vllm ", "scratch-vllm here localhost:8000/v1 off on off ›");
    // Gemini is off on Providers: `on` here is refused, said on its row,
    // and Space steps past it.
    let gem = Bound::new("cauldron.use.{ep}.enabled", Ctx::ep("ep-4"));
    a.set_option(&gem, 1);
    assert_eq!(a.toggle_of(&gem).selected, 0, "still inherit");
    assert_row(&mut a, "✗", "✗ disabled in Providers");
    a.rows[SETTINGS_ROW] = a.selectable().iter().position(|e| *e == Entry::Row(SPage::UseEndpoint("ep-4".into()))).unwrap();
    a.handle(key(KeyCode::Char(' ')));
    assert_eq!(a.toggle_of(&gem).selected, 2, "inherit → off");
    // `x` on a global row is refused here; on its own row it asks first.
    a.handle(key(KeyCode::Char('x')));
    assert!(a.notice.as_deref().is_some_and(|n| n.contains("remove it in Providers")), "{:?}", a.notice);
    // jev's modal here.
    cfg(&mut a, "cauldron/endpoints/ep-1");
    let s = screen(&mut a);
    assert!(s.contains("Endpoint · jev · prod cauldron · global"), "{s}");
    assert_row(&mut a, " metered cap ", "metered cap $60 / month default off ↺");
    assert_row(&mut a, " url ", "url https://openrouter.ai/api/v1 inherits openrouter.ai/api/v1");
    assert_row(&mut a, " key ", "key sk-or-v1-••••••••••••••••c81e");
    assert_row(&mut a, " budget ", "budget $100 / month · openrouter");
    assert!(s.contains("open in Providers") && !s.contains("management key") && !s.contains("remove endpoint"), "{s}");
    // Narrowing starts from the key's list and only takes away.
    let narrow = Bound::new("cauldron.use.{ep}.narrow", Ctx::ep("ep-1"));
    a.set_option(&narrow, 1);
    assert_eq!(a.ep_models_here(false, "ep-1"), ["anthropic/claude-sonnet-4.5"]);
    msel(&mut a, |e| matches!(e, Entry::Item { .. }));
    a.handle(key(KeyCode::Char('x')));
    a.handle(key(KeyCode::Char('x')));
    assert!(a.ep_models_here(false, "ep-1").is_empty());
    msel(&mut a, |e| matches!(e, Entry::Add(_)));
    a.handle(key(KeyCode::Enter));
    match &a.modal {
        Some(Modal::Picker { options, .. }) => assert_eq!(options, &["anthropic/claude-sonnet-4.5"], "only the key's own"),
        m => panic!("{m:?}"),
    }
    a.handle(key(KeyCode::Enter));
    assert_eq!(a.ep_models_here(false, "ep-1"), ["anthropic/claude-sonnet-4.5"]);
    a.set_option(&narrow, 0);
    assert!(!a.sis_set(&Bound::new("cauldron.use.{ep}.model_list", Ctx::ep("ep-1"))), "inherit forgets the narrowed list");
    // open in Providers: the endpoint's own modal there.
    msel(&mut a, |e| matches!(e, Entry::Do { label, .. } if label == "open in Providers"));
    a.handle(key(KeyCode::Enter));
    assert_eq!(a.cfg_tab, tui::cauldron::menu::CfgTab::Providers);
    assert!(matches!(&a.modal, Some(Modal::Settings { page: SPage::Endpoint { here: false, id }, .. }) if id == "ep-1"), "{:?}", a.modal);
    // Its own endpoint: removed, asked first; its settings and nothing else go.
    cfg(&mut a, "cauldron/endpoints");
    a.rows[SETTINGS_ROW] = a.selectable().iter().position(|e| *e == Entry::Row(SPage::Endpoint { here: true, id: "ep-6".into() })).unwrap();
    a.handle(key(KeyCode::Char('x')));
    assert!(matches!(a.modal, Some(Modal::Confirm { .. })), "{:?}", a.modal);
    a.handle(key(KeyCode::Enter));
    assert!(a.settings.endpoint_ids(true).is_empty());
    assert_eq!(a.settings.endpoint_ids(false).len(), 5);
    // Another cauldron sees the global ones, not prod's own, and its own caps.
    std::fs::create_dir_all(a.env.root.join("data/dev")).unwrap();
    let d = open(&a.env.root, "dev");
    assert_eq!(d.seen_endpoints().len(), 5);
    assert_eq!(d.sget(&Bound::new("cauldron.use.{ep}.metered_cap_usd", Ctx::ep("ep-1"))).val, Val::Off);
}

/// C9 (design 0199 D8): a metered role is bound to an endpoint; the Roles
/// table's source column names it, its picker offers the endpoints used
/// here, and a model picker offers the endpoints' models.
#[test]
fn c9_a_role_is_bound_to_an_endpoint() {
    let mut a = open(&scratch_root(), "prod");
    cfg(&mut a, "cauldron/roles");
    // A role is named in words (0214 A9).
    assert_row(&mut a, "type one · secondary", "type one · secondary jev none jev ›");
    let r = Bound::new("roles.{role}.endpoint", Ctx::role("writer"));
    let refs = a.choices(&r);
    assert_eq!(refs[0], "none");
    assert!(refs.iter().any(|x| x == "jev · openrouter.ai/api/v1") && !refs.iter().any(|x| x.starts_with("general")) && !refs.iter().any(|x| x.starts_with("gemini")), "only the endpoints used here: {refs:?}");
    cfg(&mut a, "cauldron/roles/writer");
    msel(&mut a, is_set("endpoint"));
    a.handle(key(KeyCode::Enter));
    let Some(Modal::Picker { options, .. }) = a.modal.clone() else { panic!("{:?}", a.modal) };
    let i = options.iter().position(|o| o.starts_with("studio-box")).unwrap();
    for _ in 0..i {
        a.handle(key(KeyCode::Down));
    }
    a.handle(key(KeyCode::Enter));
    assert_eq!(a.sget(&r).val, Val::Str("ep-5".into()), "kept as its id");
    assert_eq!(a.value_text(&r), "studio-box", "shown by its name");
    let models = a.choices(&Bound::new("roles.{role}.model", Ctx::role("writer")));
    assert!(models.iter().any(|m| m == "qwen3:32b") && models.iter().any(|m| m == "anthropic/claude-sonnet-4.5"), "{models:?}");
}

/// C9: the OpenRouter source is retired at the source — every key of it in
/// an old file is read, never used, and named, and nothing else is lost —
/// and a file that has never minted an endpoint READS the seed's endpoints
/// beneath it, never taking them into itself (task 0209 D1).
#[test]
fn c9_openrouter_keys_are_retired_and_his_files_read_the_seed_endpoints() {
    let s = tui::cauldron::settings::Store::parse(
        "[pools.openrouter]\nmodels = [\"jev\"]\nprovider = \"openrouter\"\n[pools.openrouter.budget.cash]\ncap_usd_month = 40\n[cauldron.openrouter]\nkey = \"jev-key\"\nmetered_cap_usd = 30\n[pools.claude_code.accounts.acct2]\nshare = 30\n",
        "t",
    )
    .unwrap();
    assert_eq!(s.retired.len(), 5, "{:?}", s.retired);
    assert_eq!(s.vals.len(), 1, "every setting that still stands is kept: {:?}", s.vals);
    let t = s.to_text().unwrap();
    assert!(t.contains("[pools.openrouter]") && t.contains("cap_usd_month = 40") && t.contains("key = \"jev-key\""), "a retired key is never deleted: {t}");
    // His files: a global one and a prod one, written before endpoints.
    let root = scratch_root();
    let probe = open(&root, "prod");
    let (gdir, dir) = (probe.settings.global_dir.clone(), probe.settings.dir.clone());
    drop(probe);
    std::fs::create_dir_all(&gdir).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let (g0, c0) = ("[pools.openrouter]\nmodels = [\"jev\"]\n[pools.claude_code.accounts.acct2]\nshare = 30\n", "[cauldron]\nenabled = true\n");
    std::fs::write(gdir.join("settings.toml"), g0).unwrap();
    std::fs::write(dir.join("settings.toml"), c0).unwrap();
    let mut a = open(&root, "prod");
    assert_eq!(global_text(&a), g0, "opening writes nothing");
    assert_eq!(std::fs::read_to_string(dir.join("settings.toml")).unwrap(), c0, "opening writes nothing");
    assert_eq!(a.settings.endpoint_ids(false).len(), 5);
    assert_eq!(a.settings.endpoint_ids(true), ["ep-6"]);
    // Removed, it stays removed: its removal is all that is written.
    a.settings.remove_endpoint(false, "ep-2").unwrap();
    let b = open(&root, "prod");
    assert!(!b.settings.endpoint_ids(false).contains(&"ep-2".to_string()));
    assert_eq!(b.settings.endpoint_ids(true), ["ep-6"]);
    assert!(!global_text(&b).contains("FAKE"), "{}", global_text(&b));
}

/// C9 + C6 (design 0199 D6): what a key spent is Status's — one always-open
/// card per endpoint, its spend of its budget, what is left, when it resets,
/// what THIS cauldron drew against its metered cap — and `config ▸` opens
/// the endpoint's modal on Config.
#[test]
fn c9_status_shows_each_endpoints_spend_and_opens_its_config() {
    let mut a = open(&scratch_root(), "prod");
    a.set_ctab(CTab::Status);
    a.set_sttab(tui::cauldron::status::StatusTab::Tokens);
    let s = render_to_text(&mut a, 120, 140).unwrap().join("\n");
    assert!(!s.contains("OpenRouter"), "{s}");
    assert!(s.lines().any(|l| l.contains("▾ Inference endpoints") && l.contains("6 endpoints") && l.contains("4 used here")), "{s}");
    assert!(s.lines().any(|l| l.contains("• jev ") && l.contains("$12.40 of $100 / month")), "{s}");
    assert!(s.lines().any(|l| l.contains("this cauldron") && l.contains("$9.10 of $60 / month metered cap")), "{s}");
    assert!(s.lines().any(|l| l.contains("• general ") && l.contains("NOT USED HERE")), "{s}");
    assert!(s.lines().any(|l| l.contains("• gemini ") && l.contains("DISABLED")), "{s}");
    let v = tui::cauldron::status::view(&a, 120);
    a.rows[0] = v.items.iter().position(|i| i.key == "cfg:ep/ep-1").unwrap();
    a.focus = Focus::Content;
    a.handle(key(KeyCode::Enter));
    assert_eq!(a.ctab, CTab::Config);
    assert!(matches!(&a.modal, Some(Modal::Settings { page: SPage::Endpoint { here: false, id }, .. }) if id == "ep-1"), "{:?}", a.modal);
}
