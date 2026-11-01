//! His msg 235, the look and Config (task 0191; checklist 0188): each test
//! names the checklist ids it proves — V1–V8 (the visual grammar), C1–C8
//! (Config's structure), S3's look and S4 (an account's pressure). The
//! toggle's cells, the default/revert grammar, a row's modal and the
//! detected models are driven in `tests/it/settings.rs`; this file holds what
//! is not there. Hermetic: a scratch root, CAULDRON_TYPE_ONE=off.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Hit, Modal};
use tui::cauldron::menu::{Bound, CfgTab, Entry, Nav, SPage, SETTINGS_ROW};
use tui::cauldron::settings::{self, Ctx, Val, DEFS};
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::seed_dir;
use tui::cauldron::{render_bg_at, render_to_text};
use tui_render::palette;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "msg235b6-{}-{}-{}",
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

fn open_in(root: &Path, name: &str) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    crate::live_pool::seeded(App::open(env_in(root, name), seed_dir(), true).expect("seed loads"))
}

fn open() -> App {
    open_in(&scratch_root(), "prod")
}

/// The face over the fixture seed with NO stand-in daemon behind it yet.
fn open_plain() -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(&scratch_root(), "prod"), seed_dir(), true).expect("seed loads")
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

const W: u16 = 120;
const H: u16 = 80;

fn screen(a: &mut App) -> String {
    render_to_text(a, W, H).unwrap().join("\n")
}

fn config(a: &mut App, path: &str) {
    a.set_ctab(CTab::Settings);
    a.focus = tui::cauldron::app::Focus::Content;
    a.snav = Nav::cauldron();
    a.rows[SETTINGS_ROW] = 0;
    a.open_settings_path(path).unwrap_or_else(|e| panic!("{path}: {e}"));
}

/// The (row, column) on screen where `needle` first appears.
fn at(s: &str, needle: &str) -> (usize, usize) {
    for (y, l) in s.lines().enumerate() {
        if let Some(b) = l.find(needle) {
            return (y, l[..b].chars().count());
        }
    }
    panic!("`{needle}` not on screen:\n{s}");
}

fn press(a: &mut App, x: u16, y: u16) {
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE }));
}

// ── V1 ────────────────────────────────────────────────────────────────────

/// V1 (msg 235: "you need a two space prefix whether it's the item is a
/// band header or just an item"): what a band holds — a setting, a table's
/// row, a button — begins two columns in from the band's title; a band
/// inside it puts its chevron (or its dot) where its title's text begins,
/// so its own title is two in too.
#[test]
fn v1_a_bands_contents_sit_two_columns_in_from_its_title() {
    let mut a = open();
    config(&mut a, "providers/claude_code");
    let s = screen(&mut a);
    let (cy, title) = at(&s, "Claude Code");
    // Below the band's title: the shared defaults stand flat above it.
    let below: String = s.lines().skip(cy).collect::<Vec<_>>().join("\n");
    assert_eq!(at(&below, "session threshold").1, title + 2, "a setting\n{s}");
    assert_eq!(at(&s, "+ add account").1, title + 3, "a button: its words, one in from its own edge\n{s}");
    assert_eq!(at(&s, "account   ").1, title + 3, "the table's column names, one in from its rows' edge\n{s}");
    assert_eq!(at(&s, "• Models").1, title, "a band inside it: its dot under the title's text\n{s}");
    assert_eq!(at(&s, "haiku").1, at(&s, "Models").1 + 2, "and ITS contents two in from its title\n{s}");
    // At the top of a tab nothing contains a row: it stands with the bands.
    config(&mut a, "cauldron");
    let s = screen(&mut a);
    assert_eq!(at(&s, "cauldron enabled").1, at(&s, "▸ Accounts").1, "{s}");
}

// ── V2 V3 ─────────────────────────────────────────────────────────────────

/// V2 V3: every switch on Config is the one segmented control, and a
/// setting that inherits is inherit · on · off everywhere it is drawn — on
/// its row and on a table's row.
#[test]
fn v2_v3_every_switch_is_one_background_and_an_inheriting_one_is_tri_state() {
    let mut a = open();
    config(&mut a, "providers/claude_code");
    let s = screen(&mut a);
    let (y, x) = at(&s, " inherit  on  off ");
    for dx in 0..18 {
        let bg = render_bg_at(&mut a, W, H, (x + dx) as u16, y as u16).unwrap();
        assert!(bg == tui_render::palette::c().toggle_bg || bg == tui_render::palette::c().code_bg, "cell {dx}: {bg:?}");
    }
    for d in DEFS.iter().filter(|d| d.ty == settings::Ty::Toggle && d.inherits.is_some()) {
        let bd = Bound::new(d.key, Ctx::account("claude_code", "acct2"));
        assert!(tui::cauldron::menu::is_tri(&bd), "{}", d.key);
        assert_eq!(a.toggle_of(&bd).options, ["inherit", "on", "off"], "{}", d.key);
    }
}

// ── V4 ────────────────────────────────────────────────────────────────────

/// V4: a value set back to its default by hand reads as the default again —
/// not "overriding the default" (msg 235: "even if you change it from off to
/// on and then back to off, it still says that it's overriding") — so no ↺
/// shows. It is still kept as his choice (0250, 0225 F1).
#[test]
fn v4_a_value_put_back_to_its_default_is_the_default() {
    let root = scratch_root();
    let mut a = open_in(&root, "prod");
    let bd = Bound::new("storage.threads.cap_gb", Ctx::default());
    a.sset(&bd, Val::Int(11)).unwrap();
    assert!(a.reverts(&bd));
    a.sset(&bd, Val::Int(10)).unwrap();
    assert!(!a.reverts(&bd), "reads as the default");
    assert!(a.sis_set(&bd), "kept as his choice (0225 F1)");
    // A setting that inherits: its inherited value typed in is inheriting.
    let wk = Bound::new("pools.{pool}.accounts.{account}.budget.weekly.threshold_pct", Ctx::account("claude_code", "acct2"));
    a.sset(&wk, Val::Int(80)).unwrap();
    assert!(!a.sis_set(&wk), "80 is what it inherits");
    assert_eq!(a.default_note(&wk), "inherits 80 %");
    assert_eq!(a.default_note(&bd), "default 10 GB");
}

// ── V5 ────────────────────────────────────────────────────────────────────

/// V5 (msg 235: "I want it to look like a button. So make it look like a
/// button and not like a whole row that you can click on"): `+ add account`
/// selected lights the BUTTON — the rest of its row stays the page's — and
/// a click beside it does not press it; a click on it does.
#[test]
fn v5_an_add_action_is_a_button_not_a_row() {
    let mut a = open();
    config(&mut a, "providers/claude_code");
    let i = a.selectable().iter().position(|e| matches!(e, Entry::Add(_))).unwrap();
    a.rows[SETTINGS_ROW] = i;
    let s = screen(&mut a);
    let (y, x) = at(&s, "+ add account");
    assert_eq!(render_bg_at(&mut a, W, H, x as u16, y as u16).unwrap(), tui_render::palette::c().button_sel_bg, "the button lights");
    let beyond = (x + 20) as u16;
    assert_eq!(render_bg_at(&mut a, W, H, beyond, y as u16).unwrap(), palette::c().surface, "its row does not:\n{s}");
    // Beside it: nothing.
    a.last_click = None;
    press(&mut a, beyond, y as u16);
    a.last_click = None;
    press(&mut a, beyond, y as u16);
    assert!(a.modal.is_none(), "a click beside a button never presses it: {:?}", a.modal);
    // On it: pressed, at once.
    screen(&mut a);
    press(&mut a, x as u16, y as u16);
    assert!(matches!(a.modal, Some(Modal::AddAccount { .. })), "{:?}", a.modal);
    // Every action on Config is one — run now among them.
    assert!(Entry::Run(Bound::new("storage.gc.run_now", Ctx::default())).is_button());
}

// ── V6 ────────────────────────────────────────────────────────────────────

/// V6 (msg 235: "rows ... you could make that the border color of the
/// cells ... instead of, you know, having, like, three colors involved"):
/// a table's row stands on ONE background, what parts it from the next is
/// the app's own background, its columns are left-aligned under their
/// names, a `›` ends it, and a click on it opens its modal.
#[test]
fn v6_a_table_is_rows_on_one_colour_parted_by_the_apps_with_a_chevron() {
    let mut a = open();
    config(&mut a, "providers/claude_code");
    a.rows[SETTINGS_ROW] = 0;
    let s = screen(&mut a);
    let (head, hx) = at(&s, "account   ");
    // V1.6: the daemon's order (its keys): acct2, acct3, sam-broomstick.
    let (r1, x1) = at(&s, "acct2 ");
    let (r2, x2) = at(&s, "acct3 ");
    assert_eq!((x1, x2), (hx, hx), "the names left-aligned under `account`\n{s}");
    assert_eq!(r1, head + 1);
    assert_eq!(r2, r1 + 2, "one row's edge between two rows");
    // Every column starts where its name does.
    let line = |y: usize| s.lines().nth(y).unwrap().to_string();
    let col = line(head).find("weekly threshold").unwrap();
    assert_eq!(line(r1).find("80 %").unwrap(), col, "the value under its column's name\n{s}");
    assert!(line(r1).contains('›') && line(r2).contains('›'), "{s}");
    // Two colours: the row's, and the app's between rows (the switches on
    // it are controls, on their own).
    let chev = line(r2).chars().position(|c| c == '›').unwrap();
    for x in [x2, chev - 1, chev] {
        assert_eq!(render_bg_at(&mut a, W, H, x as u16, r2 as u16).unwrap(), tui_render::palette::c().row_bg, "column {x}");
    }
    let edge = render_bg_at(&mut a, W, H, (x2 + 30) as u16, (r2 + 1) as u16).unwrap();
    assert_eq!(edge, palette::c().surface, "the edge under a row is the app's background");
    assert!(line(r2 + 1).contains('▀'), "its top half the row's: {}", line(r2 + 1));
    assert_eq!(render_bg_at(&mut a, W, H, 3, (r2 + 1) as u16).unwrap(), palette::c().surface);
    // One click on the row: its modal, with every setting it has.
    let i = a.selectable().iter().position(|e| *e == Entry::Row(SPage::Account("claude_code".into(), "acct2".into()))).unwrap();
    screen(&mut a);
    let (r, _) = *a.hits.iter().find(|(_, h)| *h == Hit::Row(i)).unwrap();
    press(&mut a, r.x + 2, r.y);
    let Some(Modal::Settings { page, .. }) = a.modal.clone() else { panic!("{:?}", a.modal) };
    assert!(a.modal_selectable(&page).len() > 10, "all its settings");
    // Selected, the row is a background, not a bar or a new shape.
    a.modal = None;
    let s = screen(&mut a);
    assert!(!s.contains('▌'));
    let (y, x) = at(&s, "acct2 ");
    assert_eq!(render_bg_at(&mut a, W, H, x as u16, y as u16).unwrap(), tui_render::palette::c().sel_bg);
}

// ── V7 ────────────────────────────────────────────────────────────────────

/// V7 (msg 235: "two dashed lines and then the title and then dashed lines
/// all the way to the end ... Eradicate it. From the entire application"):
/// no `── TITLE ──` on any Config tab or modal, the Threads tab, or the
/// Tasks strip — no line holds a run of `─` beside words, inside the page.
#[test]
fn v7_no_titled_rule_anywhere() {
    let mut a = open();
    let check = |what: &str, s: &str| {
        for l in s.lines().skip(2) {
            let inner = l.trim_matches(|c| c == '│' || c == ' ' || c == '╭' || c == '╮' || c == '╰' || c == '╯');
            let rule = inner.contains("── ") && inner.chars().any(|c| c.is_alphabetic()) && !inner.contains('╭') && !inner.contains('┌') && !inner.contains('┏') && !inner.contains('┗') && !inner.contains('╰');
            assert!(!rule, "{what}: `{inner}`\n{s}");
        }
    };
    for t in CfgTab::ALL {
        a.set_ctab(CTab::Settings);
        a.set_cfg_tab(t);
        for _ in 0..12 {
            let closed: Vec<_> = a.entries().into_iter().filter(|f| f.open == Some(false)).filter_map(|f| f.entry.band()).collect();
            if closed.is_empty() {
                break;
            }
            for b in closed {
                a.snav.set_open(b, true);
            }
        }
        let s = render_to_text(&mut a, W, 200).unwrap().join("\n");
        check(&format!("Config › {t:?}"), &s);
    }
    a.set_ctab(CTab::Threads);
    crate::live_fixture::feed(&mut a);
    for i in crate::live_fixture::LISTED {
        crate::live_fixture::open(&mut a, i);
        a.threads.chat_scroll = 1000;
        let s = render_to_text(&mut a, W, 120).unwrap().join("\n");
        assert!(!s.contains("── compacted") && !s.lines().any(|l| l.contains("Message ·") && l.contains("─")), "Threads #{i}:\n{s}");
    }
}

// ── V8 ────────────────────────────────────────────────────────────────────

/// V8 (msg 235: "instead of it being a chevron, it could just be a dot ...
/// We'll call that an always expanded band"): MODELS is one — a dot, never
/// closed, nothing selects its head and ← never closes it.
#[test]
fn v8_an_always_expanded_band_has_a_dot_and_never_closes() {
    let mut a = open();
    config(&mut a, "providers/claude_code");
    let s = screen(&mut a);
    assert!(s.contains("• Models") && !s.contains("▾ Models") && !s.contains("▸ Models"), "{s}");
    let opus = a.selectable().iter().position(|e| matches!(e, Entry::Set(b) if b.ctx.model == "opus")).unwrap();
    a.rows[SETTINGS_ROW] = opus;
    a.handle(key(KeyCode::Left));
    assert!(screen(&mut a).contains("haiku"), "← from a model climbs past the dot, and Models stays open");
    assert!(matches!(a.settings_entry(), Some(Entry::Sub { page: SPage::Pool(_), .. })), "{:?}", a.settings_entry());
    // Status › Tokens: an account's card is always open, a dot on its border.
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Tokens);
    let s = screen(&mut a);
    assert!(s.contains("╭─ • sam-broomstick") && !s.contains("╭─ ▸ ") && !s.contains("╭─ ▾ "), "{s}");
}

// ── C1 C2 ─────────────────────────────────────────────────────────────────

/// C1 C2 (msg 235: "one for the TUI, one for the machine, or we'll call it
/// devices ... providers. And then finally at the end, we'll have one for
/// this cauldron"; "this machine, and storage and retention, permissions,
/// identity, attention and voice, mission services, repos, TUI, all that
/// stuff belongs in global"): five tabs, one named for the cauldron (the
/// first since b964d571 msg 23);
/// what moved to a global tab is kept ONCE, and every cauldron reads it.
#[test]
fn c1_c2_five_tabs_the_last_this_cauldron_and_the_global_ones_kept_once() {
    let root = scratch_root();
    let mut a = open_in(&root, "prod");
    a.set_ctab(CTab::Settings);
    let s = screen(&mut a);
    // V1.1: no sub-tab strip; the pages are root tabs on the two bars.
    assert!(s.lines().next().unwrap().ends_with("TUI | Devices | Providers | Accounts") && s.lines().nth(1).unwrap().ends_with("Permissions | Settings"), "{s}");
    let tab = |a: &App, p: SPage| CfgTab::ALL.into_iter().find(|t| a.page_entries(&t.root()).iter().any(|e| e.band() == Some(p.clone())));
    for (p, t) in [
        (SPage::Attention, CfgTab::Tui),
        (SPage::Machine, CfgTab::Devices),
        (SPage::Storage, CfgTab::Devices),
        (SPage::Mesh, CfgTab::Devices),
        (SPage::Pool("claude_code".into()), CfgTab::Accounts),
        (SPage::Identity, CfgTab::Permissions),
        (SPage::Use, CfgTab::Cauldron),
        (SPage::Roles, CfgTab::Cauldron),
    ] {
        assert_eq!(tab(&a, p.clone()), Some(t), "{p:?}");
    }
    assert!(a.page_entries(&CfgTab::Tui.root()).iter().any(|e| matches!(e, Entry::Set(b) if b.key == "ui.default_tab")), "TUI's own settings are on it, flat");
    assert!(a.page_entries(&CfgTab::Permissions.root()).iter().any(|e| matches!(e, Entry::Set(b) if b.key == "permissions.mode")));
    // Kept once: set in prod, read in dev.
    for (key, v) in [("machine.cpu_max_pct", Val::Int(70)), ("ui.aggregate_default", Val::Bool(true)), ("permissions.mode", Val::Str("plan".into())), ("notify.tts.enabled", Val::Bool(false))] {
        assert!(settings::is_global(key), "{key}");
        a.sset(&Bound::new(key, Ctx::default()), v.clone()).unwrap();
    }
    std::fs::create_dir_all(root.join("data/dev")).unwrap();
    let d = open_in(&root, "dev");
    assert_eq!(d.sget(&Bound::new("machine.cpu_max_pct", Ctx::default())).val, Val::Int(70));
    assert_eq!(d.sget(&Bound::new("permissions.mode", Ctx::default())).val, Val::Str("plan".into()));
    // THIS cauldron's stay its own.
    // The root of every cauldron and the weights path are the machine's
    // (task 0209 D8).
    assert!(settings::is_global("paths.root") && settings::is_global("paths.weights"));
    for k in ["cauldron.enabled", "cauldron.opportunistic", "local.enabled", "roles.{role}.model"] {
        assert!(!settings::is_global(k), "{k}");
    }
    // The repo registry is global since task 0205, and its trace roots with it.
    assert!(settings::is_global("repos.trace_roots"));
    // A cauldron file that held one before hands it over once: adopted when
    // the global file lacks it, and gone from the cauldron's file — on disk
    // at the first change he makes, never by the open (task 0209 D1).
    let root2 = scratch_root();
    let dir = tui::cauldron::store::soup_dir(&env_in(&root2, "prod")).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let before = "[machine]\ncpu_max_pct = 60\n\n[local]\nenabled = true\n";
    std::fs::write(dir.join("settings.toml"), before).unwrap();
    let mut b = open_in(&root2, "prod");
    // Moved without a caption: nothing he wrote changes (0211 R2).
    assert!(b.notice.is_none(), "{:?}", b.notice);
    assert_eq!(b.sget(&Bound::new("machine.cpu_max_pct", Ctx::default())).val, Val::Int(60));
    assert_eq!(std::fs::read_to_string(dir.join("settings.toml")).unwrap(), before, "the open writes nothing");
    b.sset(&Bound::new("cauldron.opportunistic", Ctx::default()), Val::Bool(false)).unwrap();
    let mine = std::fs::read_to_string(dir.join("settings.toml")).unwrap();
    assert!(!mine.contains("machine") && mine.contains("[local]"), "{mine}");
    let g = std::fs::read_to_string(tui::cauldron::store::global_dir(&b.env).join("settings.toml")).unwrap();
    assert!(g.contains("cpu_max_pct = 60"), "moved, not lost: {g}");
}

// ── C3 ────────────────────────────────────────────────────────────────────

/// C3: this cauldron's first setting is `cauldron enabled`, enabled or
/// disabled; flipping it asks first and is kept in its own file.
#[test]
fn c3_the_first_setting_is_cauldron_enabled_enabled_or_disabled() {
    let root = scratch_root();
    let mut a = open_in(&root, "prod");
    config(&mut a, "cauldron");
    a.rows[SETTINGS_ROW] = 0;
    assert!(matches!(a.settings_entry(), Some(Entry::Set(b)) if b.key == "cauldron.enabled"));
    let s = screen(&mut a);
    let row = s.lines().find(|l| l.contains("cauldron enabled")).unwrap();
    // Its default said, as every row's is (V4; task 0209 D12).
    assert!(row.contains(" enabled  disabled ") && row.contains("default enabled"), "{row}");
    // A click on `disabled` asks; Enter does it.
    screen(&mut a);
    let (r, _) = *a.hits.iter().find(|(_, h)| *h == Hit::Opt { row: 0, cell: 0, opt: 1 }).expect("`disabled` is a click target");
    press(&mut a, r.x + 1, r.y);
    assert!(matches!(&a.modal, Some(Modal::Confirm { .. })), "{:?}", a.modal);
    a.handle(key(KeyCode::Enter));
    assert!(!a.enabled());
    let f = std::fs::read_to_string(tui::cauldron::store::soup_dir(&a.env).unwrap().join("settings.toml")).unwrap();
    assert!(f.contains("[cauldron]\nenabled = false"), "{f}");
}

// ── C4 ────────────────────────────────────────────────────────────────────

/// C4 (msg 235: "a collapsible header for Claude and then have a table
/// inside of it"; "on/off and weekly threshold and opportunistic mode. So
/// all three of those. And then when you click on the row, you go to modal
/// with all the settings"): this cauldron's accounts, a table per source,
/// its switches live, a row's modal this cauldron's settings of it.
#[test]
fn c4_this_cauldrons_accounts_are_a_table_with_live_switches_and_a_modal() {
    let root = scratch_root();
    let mut a = open_in(&root, "prod");
    config(&mut a, "cauldron/accounts");
    let s = screen(&mut a);
    assert!(s.contains("▾ Claude Code") && s.contains("▾ Codex"), "{s}");
    assert!(s.lines().any(|l| l.contains("account") && l.contains("allowed here") && l.contains("weekly threshold") && l.contains("opportunistic")), "{s}");
    let p = SPage::UseAccount("claude_code".into(), "acct2".into());
    let cells = a.row_cells(&p);
    assert_eq!(cells.len(), 3);
    a.open_row(p.clone());
    let keys: Vec<&str> = a.modal_selectable(&p).iter().filter_map(|e| if let Entry::Set(b) = e { Some(b.key) } else { None }).collect();
    assert_eq!(keys, ["cauldron.accounts.{pool}.{account}.allowed", "cauldron.accounts.{pool}.{account}.weekly_threshold_pct", "cauldron.accounts.{pool}.{account}.opportunistic"]);
    // Its weekly threshold inherits the account's own, set on Providers.
    let wk = Bound::new("cauldron.accounts.{pool}.{account}.weekly_threshold_pct", Ctx::account("claude_code", "acct2"));
    assert_eq!(a.sget(&wk).val, Val::Int(80));
    a.sset(&Bound::new("pools.{pool}.accounts.{account}.budget.weekly.threshold_pct", Ctx::account("claude_code", "acct2")), Val::Int(60)).unwrap();
    assert_eq!(a.sget(&wk).val, Val::Int(60), "it follows the account");
    a.sset(&wk, Val::Int(50)).unwrap();
    let f = std::fs::read_to_string(tui::cauldron::store::soup_dir(&a.env).unwrap().join("settings.toml")).unwrap();
    assert!(f.contains("weekly_threshold_pct = 50"), "this cauldron's own: {f}");
    assert!(screen(&mut a).contains("┏ acct2 · prod cauldron "), "{}", screen(&mut a));
}

// ── C5 C8 ─────────────────────────────────────────────────────────────────

/// C5 C8: Providers › Claude Code (and Codex, the same) is its accounts'
/// table, then its settings flat — no Defaults band, no `+ add model`, no
/// `provider` (it is what it is named) — then its detected MODELS.
#[test]
fn c5_c8_a_subscription_source_is_flat_and_codex_is_the_same() {
    let a = open();
    for p in ["claude_code", "codex"] {
        let e = a.page_entries(&SPage::Pool(p.into()));
        assert!(matches!(e.first(), Some(Entry::Row(SPage::Account(..)))), "{p}: {e:?}");
        assert!(e.iter().any(|x| matches!(x, Entry::Add(_))));
        assert!(!e.iter().any(|x| matches!(x, Entry::Sub { .. })), "{p}: flat");
        assert!(!e.iter().any(|x| matches!(x, Entry::Set(b) if b.key == "pools.{pool}.provider" || b.key == "pools.{pool}.models")), "{p}");
        let models: Vec<String> = e.iter().filter_map(|x| if let Entry::Set(b) = x { (b.key == "pools.{pool}.model.{model}").then(|| b.ctx.model.clone()) } else { None }).collect();
        assert_eq!(models, a.detected_models(p), "{p}");
        assert!(!models.is_empty());
    }
    // V1.6: the same table, Claude Code's with the daemon's `in the pool`
    // beside its name.
    let mut codex = a.row_head(&SPage::Account("codex".into(), "sam-codex".into()));
    codex.insert(1, "in the pool");
    assert_eq!(codex, a.row_head(&SPage::Account("claude_code".into(), "acct2".into())));
}

// ── C6 ────────────────────────────────────────────────────────────────────

/// C6: no reading is on Config — no share total, pressure, flip, spend,
/// subscription, branch or claim — on any tab or any row's modal. (The
/// LEGENDS sweep in `cauldron_render` holds the full list; this is the
/// account's modal, by name.)
#[test]
fn c6_no_status_on_config() {
    let mut a = open();
    config(&mut a, "providers/claude_code/sam-broomstick");
    let s = screen(&mut a);
    let inside: String = s.lines().filter(|l| l.contains('┃')).collect::<Vec<_>>().join("\n");
    for w in ["pressure", "share total", "last flip", "subscription", "login │", "config dir", "R ", "reading"] {
        assert!(!inside.contains(w), "`{w}`:\n{inside}");
    }
    // Where they went: Status › Tokens, on the account's card.
    a.modal = None;
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Tokens);
    let s = screen(&mut a);
    for w in ["pressure", "subscription", "login", "opportunistic"] {
        assert!(s.contains(w), "{w}:\n{s}");
    }
}

// ── C7 ────────────────────────────────────────────────────────────────────

/// C7 (msg 235: "the whole opportunistic mode submenu basically goes away
/// and you just have a setting of whether to enable opportunistic mode on
/// each individual account. And we're going to default to true"): the
/// submenu's settings are retired at the source; what stays is the global
/// default (on), a source's and each account's (tri-state), and THIS
/// cauldron's, whether it takes advantage of them.
#[test]
fn c7_opportunistic_is_a_per_account_switch_and_a_cauldron_setting() {
    for k in ["opportunistic.margin_pct", "opportunistic.lift_thresholds", "opportunistic.recompute_every", "pools.{pool}.opportunistic.margin_pct", "pools.{pool}.opportunistic.prefer_in_placement"] {
        assert!(DEFS.iter().all(|d| d.key != k), "{k} is gone");
        assert!(settings::RETIRED.contains(&k), "{k} is read and dropped from an old file");
    }
    let d = settings::def("opportunistic.enabled");
    assert_eq!((d.default, d.inherits), ("on", None));
    assert_eq!(settings::def("pools.{pool}.accounts.{account}.opportunistic.enabled").inherits, Some("pools.{pool}.opportunistic.enabled"));
    assert_eq!(settings::def("cauldron.accounts.{pool}.{account}.opportunistic").inherits, Some("cauldron.opportunistic"));
    assert_eq!(settings::def("cauldron.opportunistic").default, "on");
    // His old file, with the submenu's values: read, dropped, said.
    let root = scratch_root();
    let g = root.join("data/shared/tui");
    std::fs::create_dir_all(&g).unwrap();
    std::fs::write(g.join("settings.toml"), "[opportunistic]\nenabled = true\nmargin_pct = 20\n\n[pools.claude_code.opportunistic]\nlift_thresholds = false\n").unwrap();
    let a = open_in(&root, "prod");
    // Read, unused and kept as he wrote it: nothing to say (0211 R2).
    assert!(a.notice.is_none(), "{:?}", a.notice);
    assert!(a.settings.retired().iter().any(|k| k.contains("opportunistic.margin_pct")), "{:?}", a.settings.retired());
    // No Opportunistic band anywhere on Config.
    let mut a = a;
    for t in CfgTab::ALL {
        a.set_ctab(CTab::Settings);
        a.set_cfg_tab(t);
        assert!(!a.page_entries(&t.root()).iter().any(|e| matches!(e, Entry::Sub { label, .. } if label.contains("pportunistic"))), "{t:?}");
    }
    // His renamed key reads under its new name, its value kept.
    let root = scratch_root();
    let g = root.join("data/shared/tui");
    std::fs::create_dir_all(&g).unwrap();
    std::fs::write(g.join("settings.toml"), "[pools.claude_code]\nnew_accounts_enabled = false\n").unwrap();
    let a = open_in(&root, "prod");
    assert_eq!(a.sget(&Bound::new("pools.{pool}.accounts_enabled", Ctx::pool("claude_code"))).val, Val::Bool(false));
    assert!(!a.account_on("claude_code", "acct2"), "an account inheriting it is off, as it was");
}

// ── S3 S4 ─────────────────────────────────────────────────────────────────

/// S4 (msg 235: "tokens remaining ratio over time ... you want it to be
/// bigger when there are more tokens available, and you want it to be
/// bigger when there's less time available"): the SEED's pressure model
/// (`dash::Account::pressure`, what Config's seed rows still read) is its
/// own, deterministic — (1 − weekly used) ÷ (hours to its weekly reset ÷
/// 168) — and never relative to the other accounts. Status no longer works
/// pressure out at all (V1.5): the test below holds that.
#[test]
fn s4_the_seeds_pressure_is_tokens_left_over_time_left() {
    use tui::cauldron::dash::{Account, Axis};
    let acct = |pct: u32, resets: &str| Account {
        number: 1,
        label: "x".into(),
        login: "1d".into(),
        badges: Vec::new(),
        axes: vec![Axis { axis: "weekly".into(), pct, resets: resets.into() }],
        subscription: None,
    };
    let p = |pct, r| acct(pct, r).pressure().unwrap();
    assert!((p(50, "3d 12h") - 1.0).abs() < 1e-9, "half spent, half the week left: on pace");
    assert!((p(41, "3d 4h") - 0.59 / (76.0 / 168.0)).abs() < 1e-9);
    assert!(p(20, "1d") > 1.0, "much left, little time: above 1");
    assert!(p(90, "6d") < 1.0, "spent fast: below 1");
    assert!(p(50, "1d") > p(50, "2d"), "less time, more pressure");
    assert!(p(30, "2d") > p(60, "2d"), "more tokens, more pressure");
    assert_eq!(acct(10, "0m").pressure(), None, "no time left: no reading");
    assert_eq!(Account { axes: Vec::new(), ..acct(1, "1d") }.pressure(), None);
}

/// S4 on Status (V1.5; V1-CLEAN1, V1.5-I F3): each card prints the DAEMON's
/// `pressure` cell, never the seed's tokens-over-time. The stand-in daemon
/// is made to say numbers the seed's own reading would NOT give (sam-broomstick
/// 7.25, acct2 0.07 — the seed works out 1.3 and 0.41), so a face that
/// computed pressure, or read the seed, cannot pass: the two sources differ
/// by construction (asserted first, as the sentinel).
#[test]
fn s4_status_prints_the_daemons_pressure_cell_not_the_seeds_reading() {
    let said = |k: &str| match k {
        "sam-broomstick" => Some(7.25),
        "acct2" => Some(0.07),
        _ => None,
    };
    let mut a = open_plain();
    let seeds: Vec<(String, Option<f64>)> = a.dash.threads.accounts.iter().map(|x| (x.label.clone(), x.pressure())).collect();
    for (k, p) in &seeds {
        if let (Some(d), Some(p)) = (said(k), p) {
            assert!((d - p).abs() > 0.1, "sentinel: the daemon's {k} ({d}) differs from the seed's ({p})");
        }
    }
    assert_eq!(seeds.iter().filter(|(k, p)| said(k).is_some() && p.is_some()).count(), 2, "sentinel: both seed readings exist: {seeds:?}");
    crate::live_pool::seed_live_with(&mut a, |f| {
        if let Some(d) = f["key"].as_str().and_then(said) {
            f["row"]["pressure"]["pressure"] = serde_json::json!(d);
        }
    });
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Tokens);
    let s = screen(&mut a);
    let pressures: Vec<&str> = s.lines().filter(|l| l.contains("pressure") && l.contains('│')).flat_map(|l| l.split_whitespace()).collect();
    for w in ["7.25", "0.07"] {
        assert!(pressures.contains(&w), "the daemon's {w} is not printed:\n{s}");
    }
    for w in ["1.3", "0.41"] {
        assert!(!pressures.contains(&w), "the seed's {w} is printed:\n{s}");
    }
}

/// S3 (look): one band per token source, each account's card always open
/// (a dot on its border, no chevron), and nothing selects it closed: → and
/// a click on it do not shut it.
#[test]
fn s3_account_cards_are_always_open() {
    let mut a = open();
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Tokens);
    let v = tui::cauldron::status::view(&a, 120);
    let i = v.items.iter().position(|it| it.key == "acct:claude_code/acct2").unwrap();
    a.rows[0] = i;
    for k in [KeyCode::Right, KeyCode::Left, KeyCode::Enter] {
        a.rows[0] = i;
        a.handle(key(k));
        assert!(screen(&mut a).contains("login         │ login ok · expires in 4d"), "{k:?} leaves it open");
    }
    a.rows[0] = i;
    screen(&mut a);
    let (r, _) = *a.hits.iter().find(|(_, h)| *h == Hit::Row(i)).unwrap();
    a.last_click = None;
    press(&mut a, r.x + 4, r.y);
    assert!(screen(&mut a).contains("login         │ login ok · expires in 4d"), "a click leaves it open");
}
