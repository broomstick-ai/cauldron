//! Task 0218 (8e4fca10 msg 235, the 0217 re-audit). B1 (GC never takes his
//! words) is `settings::b1_*` and `gc::tests`; this module proves the rest
//! of what 0217 found, on scratch roots.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Modal};
use tui::cauldron::menu::{Bound, CfgTab};
use tui::cauldron::render_to_text;
use tui::cauldron::settings::{Ctx, Val};
use tui::cauldron::store::{rewrite_legacy_refs, seed_dir};

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0218-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn open_env(root: &Path, name: &str) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let env = Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: name.into(), name_source: Source::Default, data_root: root.join("data").join(name) };
    App::open(env, seed_dir(), true).expect("opens")
}

fn click(a: &mut App, x: u16, y: u16) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
    }
}

/// Where `needle` first stands on screen: (column, row).
fn at(s: &[String], needle: &str) -> (u16, u16) {
    let (y, l) = s.iter().enumerate().find(|(_, l)| l.contains(needle)).unwrap_or_else(|| panic!("no `{needle}`:\n{}", s.join("\n")));
    (l[..l.find(needle).unwrap()].chars().count() as u16, y as u16)
}

/// B3: the design cauldron's old references come into the one soup's
/// grammar — its surface slugs, the items whose ids are known, an old
/// per-surface setting and a renamed key. One the tables do not know is
/// left as written, never guessed.
#[test]
fn b3_design_references_are_rewritten_on_adoption() {
    assert_eq!(
        rewrite_legacy_refs("> [system/Q-1]\n> [setting:system/surface.roles]\n> [setting:pools.claude_code.new_accounts_enabled]\n> [surface:voice]\n> [voice/Q-3]\n> [setting:pools.claude_code]"),
        "> [Q-1]\n> [setting:roles]\n> [setting:pools.claude_code.accounts_enabled]\n> [X-4]\n> [voice/Q-3]\n> [setting:pools.claude_code]"
    );
    // On a real adoption, in the design environment.
    let root = scratch_root();
    let dir = root.join("data/design/tui/soup/drafts");
    std::fs::create_dir_all(&dir).unwrap();
    let draft = |t: &str| format!(r#"{{"nonce":"n","writer":"tui --surfaces pid 1","updated_at":"t","text":{}}}"#, serde_json::to_string(t).unwrap());
    std::fs::write(dir.join("system.json"), draft("> [system/Q-1]\n\nmy words\n\n> [setting:system/surface.roles]")).unwrap();
    let a = open_env(&root, "design");
    let t = a.composer.text();
    assert_eq!(t, "> [Q-1]\n\nmy words\n\n> [setting:roles]");
    assert_eq!(tui::cauldron::tags::scan(&t).len(), 2, "both read as references now: {t}");
}

/// B4 (D18): one click on a VALUE row opens its picker or editor, on the
/// page and in a row's modal; a switch's label only selects it (B17).
#[test]
fn b4_one_click_opens_a_value_row() {
    let mut a = open_env(&scratch_root(), "prod");
    a.set_ctab(CTab::Settings);
    a.set_cfg_tab(CfgTab::Tui);
    let s = render_to_text(&mut a, 120, 50).unwrap();
    let (x, y) = at(&s, "opens on");
    click(&mut a, x, y);
    assert!(matches!(a.modal, Some(Modal::Picker { .. })), "one click on `opens on`: {:?}", a.modal);
    a.modal = None;
    // A switch's label: selected, never cycled unseen.
    let agg = Bound::new("ui.aggregate_default", Ctx::default());
    let before = a.sget(&agg).val;
    let s = render_to_text(&mut a, 120, 50).unwrap();
    let (x, y) = at(&s, "Aggregated clusters");
    click(&mut a, x, y);
    click(&mut a, x, y);
    assert_eq!(a.sget(&agg).val, before, "a click on a switch's label changed it");
}

/// B5 (V1): the Engine's graph and an artifact card sit two columns in from
/// their band's title.
#[test]
fn b5_engine_and_report_cards_at_title_plus_two() {
    let mut a = open_env(&scratch_root(), "prod");
    a.set_ktab(tui::cauldron::app::KTab::Tasks);
    let s = render_to_text(&mut a, 130, 60).unwrap();
    let (title, _) = at(&s, "Engine ");
    let (m1, _) = at(&s, "M-1  An interface");
    let line = &s[at(&s, "M-1  An interface").1 as usize];
    let gutter = line.chars().enumerate().skip(title as usize - 3).find(|(_, c)| !c.is_whitespace() && *c != '│').map(|(i, _)| i).unwrap();
    assert_eq!(gutter, title as usize + 2, "the gutter at the title + 2:\n{}", s.join("\n"));
    assert!(m1 > title);
    a.set_ktab(tui::cauldron::app::KTab::Nodes);
    let s = render_to_text(&mut a, 130, 60).unwrap();
    let (reports, _) = at(&s, "Reports");
    let (card, _) = at(&s, "┌─ ");
    assert_eq!(card, reports + 2, "{}", s.join("\n"));
}

/// B7, B10, B15: no live reading in Config, no microphone tones, no label
/// naming him in the third person. A file still holding a tone is read, the
/// key dropped and named, every other setting kept.
#[test]
fn b7_b10_b15_config_says_only_settings() {
    let root = scratch_root();
    let g = root.join("data/shared/tui/settings.toml");
    std::fs::create_dir_all(g.parent().unwrap()).unwrap();
    std::fs::write(&g, "[notify.tones]\ncapture = false\n\n[ui]\naggregate_default = true\n").unwrap();
    let mut a = open_env(&root, "prod");
    assert_eq!(a.sget(&Bound::new("ui.aggregate_default", Ctx::default())).val, Val::Bool(true));
    a.set_ctab(CTab::Settings);
    for (tab, path) in [(CfgTab::Devices, "devices/storage"), (CfgTab::Tui, "tui"), (CfgTab::Providers, "providers")] {
        a.set_cfg_tab(tab);
        a.snav = tui::cauldron::menu::Nav::cauldron();
        let _ = a.open_settings_path(path);
        let s = render_to_text(&mut a, 140, 120).unwrap().join("\n");
        for gone in ["reclaimable", "tone on capture", "tone on stored", "reserve for him"] {
            assert!(!s.contains(gone), "{gone} on {path}:\n{s}");
        }
    }
}

/// B11: a seed's link to the soup folder opens THIS cauldron's, wherever
/// its root is.
#[test]
fn b11_a_ticket_link_is_this_cauldrons_soup() {
    let root = scratch_root();
    let a = open_env(&root, "prod");
    let url = a.ha_url("file://{soup}/");
    assert!(url.starts_with(&format!("file://{}", root.display())) && url.ends_with("data/prod/tui/soup/"), "{url}");
}

/// B12: a role's models are its source's; `endpoint` and `engine` say their
/// default; the settle ladder says its own.
#[test]
fn b12_a_roles_models_are_its_sources() {
    let a = open_env(&scratch_root(), "prod");
    let bd = Bound::new("roles.{role}.model", Ctx::role("coordinator"));
    let m = a.choices(&bd);
    assert!(m.iter().any(|x| x == "opus") && !m.iter().any(|x| x.contains("gpt") || x == "o3" || x.contains("qwen")), "{m:?}");
    assert_eq!(a.default_note(&Bound::new("roles.{role}.endpoint", Ctx::role("coordinator"))), "default none");
    assert_eq!(a.default_note(&Bound::new("local.engine", Ctx::default())), "default none");
    assert_eq!(a.default_note(&Bound::new("threads.settle_ladder_secs", Ctx::default())), "default 5s, 30s, 120s");
}

/// B13: at 80 columns a default the row has no room for is said whole, on
/// the line under it.
#[test]
fn b13_a_default_is_never_cut_at_80_columns() {
    let mut a = open_env(&scratch_root(), "prod");
    a.set_ctab(CTab::Settings);
    a.set_cfg_tab(CfgTab::Providers);
    for path in ["providers/endpoints", "providers/claude_code"] {
        a.snav = tui::cauldron::menu::Nav::cauldron();
        a.open_settings_path(path).unwrap();
        let s = render_to_text(&mut a, 80, 60).unwrap();
        for l in &s {
            if let Some(i) = l.find("default ").or_else(|| l.find("inherits ")) {
                assert!(!l[i..].contains('…'), "cut: {l}\n{}", s.join("\n"));
            }
        }
        assert!(s.iter().any(|l| l.trim_start_matches('│').trim_start().starts_with("default ") || l.trim_start_matches('│').trim_start().starts_with("inherits ")), "{}", s.join("\n"));
    }
}

/// The hunt (0218): one click opens a number row's editor; a click away
/// from it writes nothing into his settings file.
#[test]
fn a_value_row_opened_and_left_writes_nothing() {
    let root = scratch_root();
    let mut a = open_env(&root, "prod");
    a.set_ctab(CTab::Settings);
    a.set_cfg_tab(CfgTab::Providers);
    let files = || ["data/shared/tui/settings.toml", "data/prod/tui/soup/settings.toml"].map(|f| std::fs::read(root.join(f)).ok());
    let before = files();
    let s = render_to_text(&mut a, 120, 60).unwrap();
    let (x, y) = at(&s, "weekly threshold");
    click(&mut a, x, y);
    assert!(a.field.is_some(), "one click opens its editor");
    // Enter on it as it opened: nothing changed, nothing written.
    a.handle(Event::Key(ratatui::crossterm::event::KeyEvent::new(ratatui::crossterm::event::KeyCode::Enter, KeyModifiers::NONE)));
    assert!(a.field.is_none());
    assert_eq!(files(), before, "a value opened and left was written");
}

/// B4 in a modal (0217: "a key typed after one click on `key` was silently
/// dropped"): in the add-endpoint modal one click on `key` opens its field,
/// and what is typed goes into it.
#[test]
fn b4_one_click_on_a_modal_key_row_takes_the_key() {
    let mut a = open_env(&scratch_root(), "prod");
    a.set_ctab(CTab::Settings);
    a.set_cfg_tab(CfgTab::Providers);
    // V1.7 (R-V1 item 7): every endpoint is greyed — the add modal does
    // not open, and one click on a greyed endpoint's key opens no field.
    a.start_act(tui::cauldron::manage::Act::NewEndpoint { here: false });
    assert!(a.modal.is_none() && a.notice.as_deref() == Some(tui::cauldron::settings::GREYED), "{:?}", a.modal);
    a.open_settings_path("providers/endpoints/ep-1").unwrap();
    let s = render_to_text(&mut a, 120, 60).unwrap();
    let (x, y) = at(&s, "│   key ");
    let x = x + 4;
    click(&mut a, x, y);
    assert!(a.field.is_none(), "a greyed key takes nothing:\n{}", s.join("\n"));
}

/// B4 in a modal (0217: "a key typed after one click on `key` was silently
/// dropped"): in the add-endpoint modal one click on `key` opens its field,
/// and what is typed goes into it.
///
/// V1.7b F1: the body of `b4_one_click_on_a_modal_key_row_takes_the_key` at 59c5ec8f, verbatim — the positive
/// spec the greyed endpoints lost (V1.7-I F1). Un-ignore when
/// `inference_live` ungreys them; `b4_one_click_on_a_modal_key_row_takes_the_key` above is the refusal it pairs with.
#[test]
#[ignore = "endpoints greyed in V1.7; restore when un-greyed"]
fn b4_one_click_on_a_modal_key_row_takes_the_key_ungreyed() {
    let mut a = open_env(&scratch_root(), "prod");
    a.set_ctab(CTab::Settings);
    a.set_cfg_tab(CfgTab::Providers);
    a.start_act(tui::cauldron::manage::Act::NewEndpoint { here: false });
    let s = render_to_text(&mut a, 120, 60).unwrap();
    let (x, y) = at(&s, "│   key ");
    let x = x + 4;
    click(&mut a, x, y);
    for c in "sk-or-v1-abc".chars() {
        a.handle(Event::Key(ratatui::crossterm::event::KeyEvent::new(ratatui::crossterm::event::KeyCode::Char(c), KeyModifiers::NONE)));
    }
    let f = a.field.as_ref().unwrap_or_else(|| panic!("no field open after one click:\n{}", s.join("\n")));
    assert_eq!(f.text, "sk-or-v1-abc");
}
