//! Every item of the operator's msg-165 review (8e4fca10 #165, task 0147),
//! driven headlessly through `App::handle` — keys and mouse — and drawn to
//! ratatui's TestBackend, against a scratch cauldron root (never
//! `~/.cauldron`). One test per item, named for what he asked.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, Band, CTab, Focus, HaBand, Hit, Item, Modal, DBand, Outbox, QBand, KTab};
use tui::cauldron::status::StatusTab;
use tui::cauldron::menu::{Entry, SPage, SETTINGS_ROW};
use tui::cauldron::render_to_text;
use tui::cauldron::store::{seed_dir, soup_dir};

// ── fixtures ────────────────────────────────────────────────────────────────

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "review165-{}-{}-{}",
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

fn open_in(root: &Path) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(root, "prod"), seed_dir(), true).expect("seed loads")
}

fn open() -> (App, PathBuf) {
    let root = scratch_root();
    (open_in(&root), root)
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn press(a: &mut App, keys: &[KeyCode]) {
    for &k in keys {
        a.handle(key(k));
    }
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(key(KeyCode::Char(c)));
    }
}

fn screen_at(a: &mut App, w: u16, h: u16) -> Vec<String> {
    render_to_text(a, w, h).unwrap()
}

fn screen(a: &mut App) -> String {
    screen_at(a, 120, 70).join("\n")
}

/// Click the centre of the first region recorded for `hit` in the last frame.
fn click_hit(a: &mut App, hit: Hit) {
    screen(a);
    let r = a.hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {hit:?} on screen"));
    click(a, r.x, r.y);
}

fn click(a: &mut App, x: u16, y: u16) {
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE }));
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE }));
}

fn file(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(soup_dir(&env_in(root, "prod")).unwrap().join(rel)).unwrap_or_default()
}

/// A file of the GLOBAL settings (msg 186): the token sources, the
/// accounts added, the flip log — one copy for every cauldron.
fn global_file(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join("data").join("shared").join("tui").join(rel)).unwrap_or_default()
}

fn select_item(a: &mut App, it: Item) {
    a.item = a.items().iter().position(|x| *x == it).unwrap_or_else(|| panic!("{it:?} is not on the page: {:?}", a.items()));
    a.focus = Focus::Content;
}

fn cluster(a: &mut App, key: &str) {
    let g = a.forest.find(key).unwrap();
    a.select(g);
}

// ── CAULDRON LEVEL ──────────────────────────────────────────────────────────

#[test]
fn the_top_left_is_the_word_cauldron_and_its_name_as_a_dropdown_and_no_switch() {
    let (mut a, root) = open();
    let top = screen_at(&mut a, 120, 30)[0].clone();
    // msg 186: the switch left the top bar for Config's first row.
    // msg 235: the plain product name, then the cauldrons dropdown;
    // 8e4fca10 msg 457: only the environment, no product name.
    assert!(top.starts_with("  prod  ▾"), "{top}");
    assert!(!top.contains('●'), "{top}");
    // ^T asks first, and the answer is kept.
    a.handle(ctrl('t'));
    assert!(matches!(&a.modal, Some(Modal::Confirm { title, .. }) if title == "Disable the prod cauldron?"), "{:?}", a.modal);
    a.handle(key(KeyCode::Enter));
    assert!(file(&root, "settings.toml").contains("[cauldron]\nenabled = false"), "{}", file(&root, "settings.toml"));
    a.handle(ctrl('t'));
    assert!(matches!(&a.modal, Some(Modal::Confirm { title, .. }) if title == "Enable the prod cauldron again?"));
    a.handle(key(KeyCode::Esc));
    assert!(file(&root, "settings.toml").contains("enabled = false"), "Esc keeps it disabled");
    // There is no pause on a cluster any more: the only one is this.
    a.set_ctab(CTab::Config);
    assert!(!screen(&mut a).contains("paused"));
    assert!(tui::cauldron::settings::DEFS.iter().all(|d| d.key != "cluster.paused"));
}

#[test]
fn the_cauldrons_dropdown_hangs_under_the_name_left_aligned_untitled_and_says_which_are_on() {
    let (mut a, root) = open();
    // A second cauldron, switched off in its own settings.
    let dev = env_in(&root, "dev");
    std::fs::create_dir_all(&dev.data_root).unwrap();
    let dir = soup_dir(&dev).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("settings.toml"), "[cauldron]\nenabled = false\n").unwrap();
    click_hit(&mut a, Hit::Env);
    assert!(a.dropdown.is_some() && a.modal.is_none(), "a dropdown, not a modal");
    let rows = screen_at(&mut a, 120, 30);
    let name_col = rows[0].find(" prod ").unwrap();
    let box_col = rows[1].chars().enumerate().skip(1).find(|(_, c)| *c == '╭').map(|(i, _)| i).expect("the dropdown's corner (rounded, task 0209 D18)");
    assert_eq!(box_col, rows[0][..name_col].chars().count(), "left edges aligned:\n{}\n{}", rows[0], rows[1]);
    let all = rows.join("\n");
    assert!(!all.to_lowercase().contains("environment"), "no title, no `environments`");
    let dev_row = rows.iter().find(|r| r.contains(" dev ")).unwrap();
    // In the switch's own words (msg 235: "enabled. Or disabled").
    assert!(dev_row.contains("disabled"), "{dev_row}");
    let prod_row = rows.iter().find(|r| r.contains("│ prod") || r.contains(" prod   ")).unwrap();
    assert!(prod_row.contains("enabled") && !prod_row.contains("disabled"), "{prod_row}");
    // At least 20 columns of name.
    let inner = rows[2].chars().filter(|c| *c != ' ').count();
    assert!(rows[1].chars().filter(|c| *c == '─').count() >= 20 + 4, "{} {inner}", rows[1]);
    // `+` is the last row, and makes one.
    press(&mut a, &[KeyCode::End, KeyCode::Enter]);
    assert!(matches!(a.modal, Some(Modal::NewCauldron { .. })) && a.dropdown.is_none());
    a.close_modal();
    // A click outside closes it and touches nothing.
    a.open_cauldrons();
    click(&mut a, 100, 20);
    assert!(a.dropdown.is_none() && a.ctab == CTab::Cauldron);
}

#[test]
fn four_cauldron_tabs_with_no_numbers() {
    let (mut a, _r) = open();
    let top = screen_at(&mut a, 120, 30)[0].clone();
    // msg 186: "Let's change the work tab to be called contents".
    // msg 235, renamed by 8e4fca10 msg 457: Cauldron · Agents · Status · Config.
    assert!(top.ends_with("Cauldron | Agents | Status | Config"), "{top}");
    let tabs = &top[top.find("Cauldron |").unwrap()..];
    assert!(!tabs.chars().any(|c| c.is_ascii_digit()), "{tabs}");
    for (i, t) in CTab::ALL.into_iter().enumerate() {
        a.handle(key(KeyCode::F(i as u8 + 1)));
        assert_eq!(a.ctab, t);
    }
}

#[test]
fn status_shows_the_mesh_with_services_nested_token_sources_as_bands_and_running_parked() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Status);
    let s = screen(&mut a);
    // msg 235: Mesh · Tokens · Soup · Bus, and the strip carries no metadata.
    let head = s.lines().find(|l| l.contains("Mesh")).unwrap();
    assert!(head.contains("Mesh") && head.contains("Tokens") && head.contains("Soup") && head.contains("Bus"), "{head}");
    assert!(!head.contains("running") && !head.contains("parked"), "{head}");
    // Running and parked are the Bus's executions, as ONE aggregate each.
    let (r, p, _) = tui::cauldron::status::counts(&a);
    a.set_sttab(StatusTab::Bus);
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("Running") && l.contains(&r.to_string())), "{s}");
    assert!(s.lines().any(|l| l.contains("Parked") && l.contains(&p.to_string())), "{s}");
    // A service's band header, nested under its device: status, uptime,
    // warnings, errors.
    a.set_sttab(StatusTab::Mesh);
    let s = screen(&mut a);
    let tally = s.lines().find(|l| l.contains("▸ tally")).unwrap();
    assert!(tally.contains("failed") && tally.contains("⚠ 3") && tally.contains("✕ 2"), "{tally}");
    // Per token source and per account.
    a.set_sttab(StatusTab::Tokens);
    let s = screen(&mut a);
    let cc = s.lines().find(|l| l.contains("Claude Code")).unwrap();
    assert!(cc.contains("▶ 8 runs"), "{cc}");
    // A run on an inference endpoint counts on its endpoints' band (design 0199).
    assert!(s.lines().any(|l| l.contains("Inference endpoints") && l.contains("▶ 1 run")), "{s}");
    // The threads board's layout is gone.
    assert!(!s.contains("THREADS") && !s.contains("┌ 1 ·") && !s.contains("NEXT BACK"));
    // Inside a service: a way to copy its log's link, never the log.
    a.set_sttab(StatusTab::Mesh);
    let v = tui::cauldron::status::view(&a, 120);
    a.rows[0] = v.items.iter().position(|i| i.key == "svc:example-mac/threads").unwrap();
    press(&mut a, &[KeyCode::Right, KeyCode::Down]);
    let s = screen(&mut a);
    assert!(s.contains("copy log link"), "{s}");
    press(&mut a, &[KeyCode::Enter]);
    assert_eq!(a.outbox, vec![Outbox::Copy("/Users/demo/.cauldron/data/prod/logs/threads.log".into())]);
    // An account is a card with its own metadata, ALWAYS open (msg 235:
    // "each one of the cards for the accounts they can just be expanded they
    // don't ever have to be collapsed"): a dot where the chevron was.
    a.set_sttab(StatusTab::Tokens);
    let v = tui::cauldron::status::view(&a, 120);
    a.rows[0] = v.items.iter().position(|i| i.key == "acct:claude_code/acct2").unwrap();
    let s = screen(&mut a);
    let acct = s.lines().find(|l| l.contains("• acct2")).unwrap();
    assert!(acct.contains("weekly 88%") && acct.contains("AT LIMIT · back 1h 05m"), "{acct}");
    // Its card is a TABLE of its fields (msg 186, task 0166 V6); its pressure
    // is its own tokens left over its time left (msg 235): (1 − 0.88) over
    // 49h / 168h.
    assert!(s.contains("pressure      │ 0.41x") && s.contains("login         │ 4d"), "{s}");
    press(&mut a, &[KeyCode::Right]);
    assert!(screen(&mut a).contains("pressure      │ 0.41x"), "→ does not close it");
    // ← climbs to its source.
    press(&mut a, &[KeyCode::Left]);
    let v = tui::cauldron::status::view(&a, 120);
    assert_eq!(v.items[a.rows[0]].key, "tok:claude_code");
}

#[test]
fn there_is_no_system_tab_and_no_dreaming() {
    let (mut a, _r) = open();
    assert_eq!(CTab::ALL.len(), 4);
    for t in CTab::ALL {
        a.set_ctab(t);
        let s = screen(&mut a);
        assert!(!s.contains("System"), "{t:?}:\n{s}");
    }
    assert!(tui::cauldron::settings::ROLES.iter().all(|r| r.id != "dream"));
}


// ── CONFIG ──────────────────────────────────────────────────────────────────

#[test]
fn config_is_collapsible_bands_with_no_breadcrumb() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Config);
    a.set_cfg_tab(tui::cauldron::menu::CfgTab::Cauldron);
    let s = screen(&mut a);
    assert!(!s.contains(" › "), "no breadcrumb:\n{s}");
    assert!(s.contains("▸ Directories") && s.contains("▸ Accounts") && s.contains("▸ Roles"), "{s}");
    // → opens a band where it is; ← closes it (the switch, opportunistic
    // and the sidebar come first: 0214 A8).
    press(&mut a, &[KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Right]);
    let s = screen(&mut a);
    assert!(s.contains("▾ Accounts") && s.contains("Claude Code") && s.contains("sam-broomstick"), "{s}");
    // → on an open band steps onto its first row; ← climbs back to it.
    // Its first row is the CLAUDE CODE section, itself a band (audit 0177
    // V3): → again steps onto its first account; ← climbs back to the
    // section, closes it, and climbs to Accounts.
    press(&mut a, &[KeyCode::Right]);
    assert!(matches!(a.settings_entry(), Some(Entry::Sub { page: SPage::Section(..), label, .. }) if label == "Claude Code"));
    press(&mut a, &[KeyCode::Right]);
    assert!(matches!(a.settings_entry(), Some(Entry::Row(SPage::UseAccount(..)))));
    press(&mut a, &[KeyCode::Left]);
    assert!(matches!(a.settings_entry(), Some(Entry::Sub { page: SPage::Section(..), .. })));
    press(&mut a, &[KeyCode::Left, KeyCode::Left]);
    assert!(matches!(a.settings_entry(), Some(Entry::Sub { page: SPage::Use, .. })));
    press(&mut a, &[KeyCode::Left]);
    assert!(screen(&mut a).contains("▸ Accounts"));
}

#[test]
fn paths_are_the_root_and_the_weights_path_and_both_are_editable() {
    let (mut a, root) = open();
    a.set_ctab(CTab::Config);
    // The two paths are the machine's (task 0209 D8): Devices › Storage ›
    // Directories; this cauldron's Directories holds every root its files live
    // under — what the Artifacts tab's FILE SYSTEM band was.
    a.open_settings_path("devices/storage").unwrap();
    let s = screen(&mut a);
    // (A default the row has no room for is said on the line under it,
    // 0217 B13.)
    let rows: Vec<&str> = s.lines().skip_while(|l| !l.contains("▾ Directories")).skip(1).filter(|l| l.contains("cauldron root") || l.contains("weights path")).take(2).collect();
    assert!(rows[0].contains("cauldron root") && rows[0].contains("review165-"), "{s}");
    assert!(rows[1].contains("weights path") && rows[1].contains("$root/data/shared/weights"), "{}", rows[1]);
    assert!(!s.contains("data root") && !s.contains("sends.jsonl"), "the other paths are gone");
    assert!(a.select_setting("paths.weights"));
    press(&mut a, &[KeyCode::Enter]);
    a.handle(ctrl('u'));
    typed(&mut a, "~/.weights");
    press(&mut a, &[KeyCode::Enter]);
    assert!(global_file(&root, "settings.toml").contains("weights = \"~/.weights\""), "{}", global_file(&root, "settings.toml"));
    a.open_settings_path("directories").unwrap();
    let s = screen(&mut a);
    let roots: Vec<&str> = s.lines().skip_while(|l| !l.contains("▾ Roots")).skip(1).take_while(|l| !l.contains("▸ Local")).collect();
    assert!(roots.iter().any(|l| l.contains("cauldron home")) && roots.iter().any(|l| l.contains(" repo ")) && roots.iter().any(|l| l.contains("directory")), "{roots:?}");
}

#[test]
fn a_source_is_its_accounts_table_then_its_settings_and_a_row_switch_works() {
    let (mut a, root) = open();
    a.set_ctab(CTab::Config);
    a.open_settings_path("providers/claude_code").unwrap();
    let s = screen(&mut a);
    let t = s.find("sam-broomstick").expect("the accounts' table");
    // Its settings: below its band's title (the shared defaults stand flat
    // at the head of Providers, task 0209 D4).
    let band = s.find("Claude Code").expect("its band");
    let w = band + s[band..].find("session threshold").expect("its settings");
    assert!(t < w, "the accounts' table above the settings");
    assert!(!s.contains("pool enabled"), "{s}");
    let inside = a.page_entries(&SPage::Pool("claude_code".into()));
    assert!(!inside.iter().any(|e| matches!(e, Entry::Sub { .. })), "flat, no Defaults or Accounts band: {inside:?}");
    // No planned sources (msg 235, design 0199): the Anthropic API and
    // Gemini are inference endpoint templates now.
    assert!(!s.contains("planned"), "{s}");
    assert!(s.contains("+ add account"));
    // The row's switch works on the row: Space steps acct2's `enabled`.
    let at = a.selectable().iter().position(|e| *e == Entry::Row(SPage::Account("claude_code".into(), "acct2".into()))).unwrap();
    a.rows[SETTINGS_ROW] = at;
    press(&mut a, &[KeyCode::Char(' ')]);
    assert!(global_file(&root, "settings.toml").contains("[pools.claude_code.accounts.acct2]\nenabled = true"), "{}", global_file(&root, "settings.toml"));
    // Enter opens its modal: its thresholds and caps.
    press(&mut a, &[KeyCode::Enter]);
    let s = screen(&mut a);
    for k in ["session threshold", "weekly threshold", "fable threshold", "session cap", "weekly cap", "fable cap", "cash threshold"] {
        assert!(s.contains(k), "{k}:\n{s}");
    }
}

#[test]
fn an_endpoints_key_is_masked_typed_and_shown() {
    // Design 0199: a key is never shown — its prefix, dots, its last four.
    let (mut a, root) = open();
    a.set_ctab(CTab::Config);
    a.open_settings_path("providers/endpoints/ep-2").unwrap();
    // name, protocol, url, key: the fourth row of its modal.
    press(&mut a, &[KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Enter]);
    typed(&mut a, "sk-or-v1-NEWKEY0000000000000000beef");
    let s = screen(&mut a);
    assert!(!s.contains("NEWKEY") && s.contains("•••••••••••••••"), "masked while typed:\n{s}");
    press(&mut a, &[KeyCode::Enter]);
    let s = screen(&mut a);
    let row = s.lines().find(|l| l.contains(" key ") && l.contains("set")).unwrap_or_else(|| panic!("{s}"));
    assert!(row.contains("sk-or-v1-••••••••••••••••beef") && !row.contains("NEWKEY"), "{row}");
    assert!(global_file(&root, "settings.toml").contains("sk-or-v1-NEWKEY"), "a global endpoint's key is global");
}

#[test]
fn a_row_says_what_its_default_is_never_an_override() {
    // msg 235: "we should actually just say what the default is ... So don't
    // even show that it's an override. Just say what the default is."
    let (mut a, _r) = open();
    a.set_ctab(CTab::Config);
    a.open_settings_path("providers/claude_code/acct2").unwrap();
    let s = screen(&mut a);
    assert!(!s.contains("inherited ·") && !s.contains("override") && !s.contains("↑pool"), "{s}");
    let weekly = s.lines().find(|l| l.contains("weekly threshold") && l.contains("inherits")).unwrap();
    assert!(weekly.contains("80 %"), "{weekly}");
    let session = s.lines().find(|l| l.contains("session threshold") && l.contains("inherits")).unwrap();
    assert!(session.contains("inherits off"), "{session}");
}

#[test]
fn max_concurrent_zero_is_zero_and_minus_one_is_unlimited() {
    let (mut a, root) = open();
    a.set_ctab(CTab::Config);
    a.open_settings_path("providers/claude_code").unwrap();
    let at = a.selectable().iter().position(|e| matches!(e, Entry::Set(b) if b.key == "pools.{pool}.max_concurrent")).unwrap();
    a.rows[SETTINGS_ROW] = at;
    assert!(screen(&mut a).lines().any(|l| l.contains("max concurrent") && l.contains("unlimited")));
    press(&mut a, &[KeyCode::Enter]);
    a.handle(ctrl('u'));
    typed(&mut a, "0");
    press(&mut a, &[KeyCode::Enter]);
    let row = screen(&mut a).lines().find(|l| l.contains("max concurrent")).unwrap().to_string();
    // Its ⓘ is in the rightmost column (msg 186); the value follows the label.
    assert!(row.trim_end_matches(['│', ' ']).ends_with('ⓘ') && row.split("max concurrent").nth(1).unwrap().trim_start().starts_with("0 "), "zero is zero: {row}");
    assert!(global_file(&root, "settings.toml").contains("max_concurrent = 0"));
    // `-` from 0 is -1, which is unlimited — its default, and kept as his
    // choice (0250, 0225 F1).
    press(&mut a, &[KeyCode::Char('-')]);
    assert!(screen(&mut a).lines().any(|l| l.contains("max concurrent") && l.contains("unlimited")));
    assert!(global_file(&root, "settings.toml").contains("max_concurrent = -1"), "{}", global_file(&root, "settings.toml"));
}

#[test]
fn every_setting_has_an_info_icon_whose_modal_is_the_only_place_its_explanation_is() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Config);
    a.open_settings_path("storage").unwrap();
    let s = screen(&mut a);
    // Every SETTING row; a fact (`warnings cap`) is no setting.
    let rows: Vec<&str> = s.lines().filter(|l| (l.contains("retention") || l.contains(" cap ")) && !l.contains("warnings cap") && !l.contains("▾") && !l.contains("▸")).collect();
    assert!(!rows.is_empty() && rows.iter().all(|r| r.contains("ⓘ")), "{rows:?}");
    let text = tui::cauldron::settings::help(tui::cauldron::settings::def("storage.gc.dry_run"));
    assert!(!s.contains(&text[..20]), "no explanation on the page");
    // A click on an ⓘ opens it; a click outside closes it.
    screen(&mut a);
    let tip = a.hits.iter().find(|(_, h)| matches!(h, Hit::Tip(_))).map(|(r, h)| (*r, *h)).unwrap();
    click(&mut a, tip.0.x, tip.0.y);
    let Some(Modal::Help { text, .. }) = a.modal.clone() else { panic!("no help: {:?}", a.modal) };
    assert!(screen(&mut a).contains(&text[..20]), "the modal shows it");
    click(&mut a, 1, 60);
    assert!(a.modal.is_none());
    // `h` on a row does the same.
    press(&mut a, &[KeyCode::Down, KeyCode::Char('h')]);
    assert!(matches!(a.modal, Some(Modal::Help { .. })));
}

#[test]
fn the_tui_tab_has_the_two_preserve_switches_and_leaving_resets_unless_one_is_on() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Config);
    a.set_cfg_tab(tui::cauldron::menu::CfgTab::Tui);
    let s = screen(&mut a);
    for k in ["preserve Config expansion", "preserve Status expansion"] {
        assert!(s.lines().any(|l| l.contains(k) && l.contains("on  off") && l.contains("default off")), "{k}:\n{s}");
    }
    // Off: away and back, every band is closed.
    a.open_settings_path("tui/attention").unwrap();
    a.set_ctab(CTab::Cauldron);
    a.set_ctab(CTab::Config);
    assert!(a.snav.open.is_empty());
    // On: kept.
    a.open_settings_path("tui/attention").unwrap();
    let at = a.selectable().iter().position(|e| matches!(e, Entry::Set(b) if b.key == "ui.preserve.config")).unwrap();
    a.rows[SETTINGS_ROW] = at;
    press(&mut a, &[KeyCode::Char(' ')]);
    a.set_ctab(CTab::Status);
    a.set_ctab(CTab::Config);
    assert!(a.snav.open.contains(&SPage::Attention));
    // Status's bands reset the same way.
    a.set_ctab(CTab::Status);
    a.stnav.insert("devices".into());
    a.set_ctab(CTab::Cauldron);
    assert!(a.stnav.is_empty());
}

#[test]
fn a_retired_setting_in_his_file_is_unused_said_and_never_deleted() {
    let root = scratch_root();
    let dir = soup_dir(&env_in(&root, "prod")).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    // His prod file held exactly this per-pool switch (msg 165 retired it).
    std::fs::write(dir.join("settings.toml"), "[pools.claude_code]\nenabled = true\n\n[pools.claude_code.accounts.acct2]\nshare = 30\n").unwrap();
    let mut a = open_in(&root);
    // Read and unused, never said: it is kept as he wrote it (0211 R2).
    assert!(a.notice.is_none(), "{:?}", a.notice);
    assert!(a.settings.retired().iter().any(|k| k.contains("pools.claude_code.enabled")), "{:?}", a.settings.retired());
    // The token sources are global now (msg 186): the share he set moves to
    // the global file, silently, and leaves his cauldron's.
    // His first change writes the file (an open writes nothing).
    a.sset(&tui::cauldron::menu::Bound::new("cauldron.opportunistic", tui::cauldron::settings::Ctx::default()), tui::cauldron::settings::Val::Bool(false)).unwrap();
    // What he wrote is never deleted (task 0209 D1): the retired switch is
    // written back as it was, and read as nothing.
    let f = std::fs::read_to_string(dir.join("settings.toml")).unwrap();
    assert!(f.contains("[pools.claude_code]\nenabled = true") && !f.contains("share"), "{f}");
    let g = global_file(&root, "settings.toml");
    assert!(g.contains("share = 30"), "{g}");
}

// ── WORK ────────────────────────────────────────────────────────────────────

#[test]
fn the_sidebar_header_says_clusters_with_a_plus_and_every_row_has_its_plus() {
    let (mut a, _r) = open();
    let rows = screen_at(&mut a, 120, 30);
    assert!(rows[2].starts_with("│ Clusters") && rows[2].contains("+ │"), "{}", rows[2]);
    assert!(rows[3].starts_with("│───"), "a line under the header: {}", rows[3]);
    assert!(rows[4].contains("Cauldron") && rows[4].contains("+ │"), "{}", rows[4]);
    assert!(!rows.join("\n").contains("+ new cluster"));
    // The header's `+` makes a ROOT cluster; a row's makes one under it.
    click_hit(&mut a, Hit::AddRoot);
    assert!(matches!(&a.modal, Some(Modal::NewCluster { parent: None, .. })));
    a.close_modal();
    let g = a.forest.find("X-6").unwrap();
    click_hit(&mut a, Hit::AddChild(g));
    assert!(matches!(&a.modal, Some(Modal::NewCluster { parent: Some(p), .. }) if p == "X-6"));
    a.close_modal();
    // By key: ↑ from the first cluster is the header's `+`.
    a.select(0);
    a.focus = Focus::Tree;
    press(&mut a, &[KeyCode::Up]);
    assert!(a.on_add_root);
    press(&mut a, &[KeyCode::Char('+')]);
    assert!(matches!(&a.modal, Some(Modal::NewCluster { parent: None, .. })));
}

#[test]
fn cluster_tabs_are_six_and_only_questions_and_actions_carry_a_number() {
    let (mut a, _r) = open();
    let s = screen(&mut a);
    let tabs = s.lines().find(|l| l.contains(" | Actions ")).unwrap();
    let (nq, na) = (a.active_questions(), a.active_actions());
    // msg 235: no Config. 8e4fca10 msg 385: Human flattened into Questions
    // and Actions, each with its count; msg 387: Contents is Nodes.
    assert!(tabs.contains(&format!("Questions {nq} | Actions {na} | Tasks | Nodes | Status | Details")) && !tabs.contains("Config"), "{tabs}");
    assert_eq!(tabs.matches(char::is_numeric).count(), nq.to_string().len() + na.to_string().len(), "{tabs}");
    assert_eq!(KTab::ALL.len(), 6);
    assert_eq!(KTab::parse("nodes"), Some(KTab::Nodes));
    assert_eq!(KTab::parse("contents"), None, "8e4fca10 msg 387: Contents is Nodes, renamed at the source, no alias");
    assert!(!tabs.contains("Threads"), "{tabs}");
    // msg 235: no rule under the strip; the body starts on the next line.
    let i = s.lines().position(|l| l.contains(" | Actions ")).unwrap();
    let next = s.lines().nth(i + 1).unwrap();
    assert!(!next.split('│').nth(2).unwrap().contains("──────────"), "{next}");
}

#[test]
fn overview_is_bands_his_and_the_agents_and_has_no_log() {
    let (mut a, _r) = open();
    a.focus_input();
    typed(&mut a, "a thought for the cauldron");
    press(&mut a, &[KeyCode::Enter]);
    a.focus = Focus::Tree;
    a.set_ktab(KTab::Details);
    let s = screen(&mut a);
    // msg 186: "I don't like the boxes ... use the collapsible band structure";
    // 8e4fca10 msg 385: always expanded, a dot for the chevron.
    for b in ["• Scope", "• Last dump", "• Directives", "• Constraints", "• Objectives", "• Milestones"] {
        assert!(s.contains(b), "{b}:\n{s}");
    }
    // The bands are lines, never boxes; what is in them are nodes, and a
    // node is a card (msg 186, task 0166 V1).
    for b in ["• Scope", "• Last dump", "• Directives"] {
        let l = s.lines().find(|l| l.contains(b)).unwrap();
        assert!(!l.contains('╭') && !l.contains('│') || l.split('│').nth(2).is_some_and(|p| p.trim_start().starts_with(b)), "a band is not a box: {l}");
    }
    assert!(s.lines().any(|l| l.contains("╭─ ") && l.contains(" CON-1 ─╮")), "a constraint is a card: {s}");
    let last = s.lines().find(|l| l.contains("• Last dump")).unwrap();
    assert!(last.contains("#"), "{last}");
    let when = s.lines().skip_while(|l| !l.contains("• Last dump")).nth(1).unwrap();
    assert!(when.contains("ago"), "{when}");
    assert!(s.contains("CON-1") && s.contains("No numbers on the cauldron's tabs"));
    // His and the agents' are kept apart.
    assert!(s.lines().any(|l| l.contains("• Directives") && l.contains("you")));
    assert!(s.lines().any(|l| l.contains("• Milestones") && l.contains("agents")));
    // A band never closes (8e4fca10 msg 385: "without expand ability"):
    // ← and a click leave it open; → steps into it.
    select_item(&mut a, Item::Band(Band::Details(DBand::Directives)));
    press(&mut a, &[KeyCode::Left]);
    assert!(screen(&mut a).contains("• Directives") && screen(&mut a).contains("DIR-1 ─╮"));
    assert!(a.band_open(Band::Details(DBand::Directives)));
    select_item(&mut a, Item::Band(Band::Details(DBand::Directives)));
    press(&mut a, &[KeyCode::Right]);
    assert!(matches!(a.items()[a.item], Item::Directive { .. }), "→ steps in: {:?}", a.items()[a.item]);
    assert!(!s.contains("log ▸"), "no log on Overview");
    // A constraint quotes as a soup node.
    select_item(&mut a, Item::Constraint { cluster: 0, ix: 0 });
    a.insert_selected();
    assert!(a.composer.text().ends_with("> [CON-1]"));
}

#[test]
fn questions_are_boxes_with_their_id_as_a_legend_context_options_recommendation() {
    let (mut a, _r) = open();
    a.set_ktab(KTab::Questions);
    let s = screen(&mut a);
    assert!(!s.contains("Human Actions"), "no sub-tab strip (8e4fca10 msg 385): {s}");
    let top = s.lines().find(|l| l.contains("◆ Q-1 ─╮")).expect("the id, top right of its border");
    assert!(top.contains("╭───"), "{top}");
    assert!(s.contains("Context") && s.contains("Options") && s.contains("Recommendation"));
    assert!(s.contains("1. ASK EVERYTHING. Every open question reaches you"), "{s}");
    assert!(!s.contains(" 90 ") && !s.contains("▸ Three ways"), "no score, never collapsed");
    assert!(!s.lines().any(|l| l.trim_start_matches('│').trim_start().starts_with('▏')), "no quote bar");
    // Status bands: active open, the rest closed until opened.
    let all: String = tui::cauldron::pages::page(&a, 110).lines.iter().map(|l| l.to_string() + "\n").collect();
    assert!(has_band(&all, "▾ Active", 3) && has_band(&all, "▸ Complete", 2) && has_band(&all, "▸ Superseded", 1) && has_band(&all, "▸ Archived", 1), "{all}");
    select_item(&mut a, Item::Band(Band::Questions(QBand::Superseded)));
    press(&mut a, &[KeyCode::Right]);
    let all: String = tui::cauldron::pages::page(&a, 110).lines.iter().map(|l| l.to_string() + "\n").collect();
    assert!(has_band(&all, "▾ Superseded", 1) && all.contains("Should the batch be called a batch?"), "{all}");
    // An option quotes as itself.
    select_item(&mut a, Item::Option { cluster: 0, serial: 1, opt: 4 });
    a.insert_selected();
    assert!(a.composer.text().ends_with("> [Q-1.opt4]"), "{}", a.composer.text());
}

/// 8e4fca10 msg 385: Questions and Actions are cluster tabs; a click on
/// either picks it, and neither page carries a sub-tab strip.
#[test]
fn questions_and_actions_are_cluster_tabs_picked_by_click() {
    let (mut a, _r) = open();
    a.set_ktab(KTab::Questions);
    a.focus = Focus::Content;
    click_hit(&mut a, Hit::KTab(KTab::Actions));
    assert_eq!(a.ktab, KTab::Actions);
    assert!(matches!(a.items().first(), Some(Item::Band(_))), "no sub-tab strip: {:?}", a.items().first());
    click_hit(&mut a, Hit::KTab(KTab::Questions));
    assert_eq!(a.ktab, KTab::Questions);
    assert!(matches!(a.items().first(), Some(Item::Band(_))), "no sub-tab strip: {:?}", a.items().first());
}

#[test]
fn a_human_action_is_a_box_of_steps_that_hold_his_hand() {
    let (mut a, root) = open();
    cluster(&mut a, "X-8");
    a.set_ktab(KTab::Actions);
    let g = a.selected;
    let s = screen(&mut a);
    let head = s.lines().find(|l| l.contains("HA-5 ─╮")).unwrap();
    assert!(head.contains("● open") && head.contains("d ·") && head.contains("0/2 steps"), "{head}");
    assert!(s.contains("Step 1 — create the project") && s.contains("↗ Open Supabase") && s.contains("[ done ]"));
    assert!(s.lines().any(|l| l.contains("◐ draft")), "a draft:\n{s}");
    // 0217 B2: a field takes no value here (no destination exists, and a
    // box that said `✓ submitted` while dropping the key was a lie): it says
    // where to give it, and the step is marked done by hand.
    assert!(s.contains("not taken here — give it in the human-actions app") && !s.contains("Enter submits"), "{s}");
    let parts: Vec<Item> = a.items().into_iter().filter(|i| matches!(i, Item::HaPart { serial: 5, .. })).collect();
    let done2 = *parts.last().unwrap();
    let field = parts[parts.len() - 2];
    select_item(&mut a, field);
    press(&mut a, &[KeyCode::Enter]);
    assert!(a.notice.as_deref().is_some_and(|n| n.contains("not taken here")), "{:?}", a.notice);
    typed(&mut a, "eyJhbGciOi-secret");
    press(&mut a, &[KeyCode::Enter]);
    let t = std::fs::read_to_string(root.join("data/prod/tui/soup/tickets.json")).unwrap_or_default();
    assert!(!t.contains("SUPABASE_ANON_KEY") && !t.contains("eyJhbG"), "{t}");
    assert!(!screen(&mut a).contains("✓ submitted"));
    select_item(&mut a, done2);
    press(&mut a, &[KeyCode::Enter]);
    let s = screen(&mut a);
    assert!(s.lines().find(|l| l.contains("HA-5 ─╮")).expect(&s).contains("1/2 steps"), "{s}");
    // A link opens; nothing sets the action's status.
    let open1 = parts[0];
    select_item(&mut a, open1);
    press(&mut a, &[KeyCode::Enter]);
    assert_eq!(a.outbox.last(), Some(&Outbox::Open("https://supabase.com/dashboard/new".into())));
    assert_eq!(a.forest.get(g).human_action(5).unwrap().status, tui::cauldron::model::HaStatus::Open);
    // A step quotes as itself; bands: active open, done and archived apart.
    a.insert_selected();
    assert!(a.composer.text().ends_with("> [HA-5.step1]"), "{}", a.composer.text());
    a.focus = Focus::Content;
    assert!(has_band(&screen(&mut a), "▸ Archived", 1));
    assert!(a.items().contains(&Item::Band(Band::Actions(HaBand::Archived))));
}

#[test]
fn a_draft_human_action_offers_nothing_to_do_yet() {
    let (mut a, root) = open();
    cluster(&mut a, "X-8");
    a.set_ktab(KTab::Actions);
    let field = a.items().into_iter().find(|i| matches!(i, Item::HaPart { serial: 3, .. })).unwrap();
    // Its open part (a link) still opens; its field does not take a value.
    let parts: Vec<Item> = a.items().into_iter().filter(|i| matches!(i, Item::HaPart { serial: 3, .. })).collect();
    select_item(&mut a, parts[1]);
    press(&mut a, &[KeyCode::Enter]);
    assert!(a.notice.as_deref().is_some_and(|n| n.contains("draft")), "{:?}", a.notice);
    let _ = field;
    assert!(!file(&root, "tickets.json").contains("HA-3"));
}

#[test]
fn soup_is_the_regions_and_artifacts_copy_and_open_a_path() {
    let (mut a, _r) = open();
    a.set_ktab(KTab::Nodes);
    assert!(a.items().iter().any(|i| matches!(i, Item::Region(_))));
    a.set_ktab(KTab::Nodes);
    let r = a.items().into_iter().find(|i| matches!(i, Item::Repo(_))).unwrap();
    let Item::Repo(rid) = r else { unreachable!() };
    let path = a.repos.repos[rid].path.clone();
    select_item(&mut a, r);
    press(&mut a, &[KeyCode::Char('c'), KeyCode::Char('o')]);
    assert_eq!(a.outbox, vec![Outbox::Copy(path.clone()), Outbox::Open(path.clone())]);
    // A click on the path copies it; on the icon beside it, opens it.
    a.outbox.clear();
    screen(&mut a);
    let ix = a.hotspot_paths.iter().position(|p| *p == path).expect("the repo's path is a click target") as u16;
    click_hit(&mut a, Hit::CopyAt(ix));
    screen(&mut a);
    let ix = a.hotspot_paths.iter().position(|p| *p == path).unwrap() as u16;
    click_hit(&mut a, Hit::OpenAt(ix));
    assert_eq!(a.outbox, vec![Outbox::Copy(path.clone()), Outbox::Open(path)]);
    assert!(screen(&mut a).contains("↗"));
}

#[test]
fn status_is_the_ripples_and_the_engine_as_bands_counting_running_parked_pending() {
    let (mut a, _r) = open();
    a.set_ktab(KTab::Status);
    let s = screen(&mut a);
    let (r, p, pe) = tui::cauldron::status::cluster_counts(&a, a.selected, false);
    let rip = s.lines().find(|l| l.contains("▾ Ripples")).unwrap();
    assert!(rip.contains(&format!("▶ {r} running · ◆ {p} parked · ◇ {pe} pending")), "{rip}");
    assert!(s.contains("▾ Running") && s.contains("▸ Parked") && s.contains("▸ Pending") && s.contains("log ▸"));
    // The Engine is the Tasks tab's (b964d571 msg 23), no longer Status's.
    assert!(!s.contains("▾ Engine") && !s.contains("DIR-1"), "{s}");
    a.set_ktab(KTab::Tasks);
    let s = screen(&mut a);
    assert!(s.contains("▾ Engine") && s.contains("DIR-1"), "{s}");
    select_item(&mut a, Item::Band(Band::Engine));
    press(&mut a, &[KeyCode::Left]);
    assert!(!screen(&mut a).contains("DIR-1"), "the engine band closes");
}

#[test]
fn the_dump_is_only_on_work_takes_no_free_keys_and_a_reference_deletes_whole() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Status);
    let s = screen(&mut a);
    assert!(!s.contains("Bucket · "), "no bucket off Nodes");
    let before = a.composer.text();
    a.handle(Event::Paste("stray".into()));
    assert_eq!(a.composer.text(), before, "a paste is not captured");
    // On Work, outside the dump, a letter is a command, not text.
    a.set_ctab(CTab::Cauldron);
    a.focus = Focus::Content;
    typed(&mut a, "zzz");
    assert_eq!(a.composer.text(), before);
    // msg 235: the Surface tab's bottom box, legend `bucket`.
    assert!(screen(&mut a).lines().any(|l| l.contains("╮") && l.contains(" bucket ")), "the bucket's legend:\n{}", screen(&mut a));
    // `a` opens it; a reference goes whole.
    typed(&mut a, "a");
    assert_eq!(a.focus, Focus::Input);
    typed(&mut a, "see [Q-1] x");
    press(&mut a, &[KeyCode::Backspace, KeyCode::Backspace, KeyCode::Backspace]);
    assert_eq!(a.composer.text(), "see ", "Backspace after a reference took it whole");
    typed(&mut a, "[Q-2]");
    press(&mut a, &[KeyCode::Home, KeyCode::Right, KeyCode::Right, KeyCode::Right, KeyCode::Right, KeyCode::Delete]);
    assert_eq!(a.composer.text(), "see ", "Delete before a reference took it whole");
    // From another tab, `i` quotes into the dump and stays.
    a.set_ctab(CTab::Config);
    a.set_cfg_tab(tui::cauldron::menu::CfgTab::Cauldron);
    press(&mut a, &[KeyCode::Down, KeyCode::Char('i')]);
    assert_eq!(a.ctab, CTab::Config);
    assert!(a.composer.text().contains("> [setting:cauldron.enabled]"), "{}", a.composer.text());
    // And it is said, though the bucket is not on this tab.
    assert!(screen(&mut a).contains("quoted [setting:cauldron.enabled] into the bucket"), "{}", screen(&mut a));
}

#[test]
fn scrolling_never_changes_a_rows_shape() {
    let (mut a, _r) = open();
    // Every cluster tab, every selection: the page is the same lines.
    for t in KTab::ALL {
        a.set_ktab(t);
        let lines = |a: &App| tui::cauldron::pages::page(a, 100).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        let n = a.items().len();
        a.item = 0;
        let first = lines(&a);
        for i in 0..n {
            a.item = i;
            assert_eq!(lines(&a), first, "{t:?}: selecting item {i} changed the page");
        }
    }
    // Every Status tab: no line appears under the selected row.
    a.set_ctab(CTab::Status);
    for t in StatusTab::ALL {
        a.set_sttab(t);
        let lines = |a: &App| tui::cauldron::status::view(a, 100).lines.len();
        let n = tui::cauldron::status::view(&a, 100).items.len();
        a.rows[0] = 0;
        let first = lines(&a);
        for i in 0..n {
            a.rows[0] = i;
            assert_eq!(lines(&a), first, "Status {t:?}: selecting row {i} changed the page");
        }
    }
}

/// msg 165 took the Threads TAB and its data out of the cauldron face, and that
/// stays so: no `[thread:…]` tag, no threads in the cluster seed, no tab key
/// for it. msg 186 brought threads back as a MODE (the top-left button, its
/// own data in `data/threads.json`, task 0167), so the palette now names the
/// mode — and only the mode.
#[test]
fn threads_and_their_data_are_gone() {
    let threads: Vec<_> = tui::cauldron::app::KEYMAP.iter().filter(|(k, w)| k.to_lowercase().contains("thread") || w.to_lowercase().contains("thread")).collect();
    assert!(threads.iter().all(|(k, w)| k.contains("Threads") || k.contains("a thread") || w.contains("Threads") || w.contains("a thread")), "{threads:?}");
    assert!(!tui::cauldron::app::KEYMAP.iter().any(|(k, _)| k.contains("⌥9")));
    assert!(tui::cauldron::tags::scan("[thread:8e4fca10]").is_empty());
    let seed = std::fs::read_to_string(seed_dir().join("clusters.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&seed).unwrap();
    assert!(v.get("threads").is_none() && v["cauldron"]["threads"].get("threads").is_none());
}

/// Whether `s` has the band line `head` whose metadata counts `n` (msg 186:
/// the count is the band's metadata, flush right — `▾ Active … 3 questions`).
#[allow(dead_code)]
fn has_band(s: &str, head: &str, n: usize) -> bool {
    s.lines().any(|l| {
        let Some(at) = l.find(head) else { return false };
        let rest = l[at + head.len()..].trim_end_matches(['│', ' ']);
        rest.starts_with(' ') && (rest.contains(&format!(" {n} ")) || rest.ends_with(&format!(" {n}")))
    })
}
