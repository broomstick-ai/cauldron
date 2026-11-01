//! His msg 235, the structure (task 0190; checklist 0188): what the other
//! suites do not already hold — I2 (switching cauldron keeps where he is),
//! N10 (the DEV-only rename of a title, by a double click), N9 (a
//! reference carries no cluster) and S3's structure (no TOKENS band). Each test names the checklist ids it
//! proves. Hermetic: a scratch root, CAULDRON_TYPE_ONE=off.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, Band, CTab, Hit, Modal, Title, KTab};
use tui::cauldron::render_to_text;
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::seed_dir;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "msg235-{}-{}-{}",
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

fn open(root: &Path) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    crate::live_pool::seeded(App::open(env_in(root, "prod"), seed_dir(), true).expect("seed loads"))
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 50).unwrap().join("\n")
}

/// Press the left button on the first region recorded for `hit`.
fn click(a: &mut App, hit: Hit) {
    screen(a);
    let r = a.hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {hit:?} on screen"));
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
}

fn double_click(a: &mut App, hit: Hit) {
    a.last_click = None;
    click(a, hit);
    click(a, hit);
}

/// I2: switching cauldron changes the data and nothing of where he is — the
/// tab, the cluster tab, the Status sub-tab, the bands he opened and shut.
#[test]
fn i2_switching_cauldron_keeps_the_tab_and_every_bands_expansion() {
    let root = scratch_root();
    std::fs::create_dir_all(root.join("data").join("dev")).unwrap();
    let mut a = open(&root);
    // A cluster page: Questions with its Active band shut (Details' bands
    // never shut since 8e4fca10 msg 385).
    a.set_ktab(KTab::Questions);
    let scope = Band::Questions(tui::cauldron::app::QBand::Active);
    assert!(a.band_open(scope));
    a.bands.insert(scope);
    assert!(!a.band_open(scope));
    // Status › Bus with Triggers opened.
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Bus);
    a.stnav.insert("triggers".into());
    assert!(tui::cauldron::status::is_open(&a, "triggers"));
    a.switch_env("dev").unwrap();
    assert_eq!(a.env.name, "dev", "the data changed");
    assert_eq!((a.ctab, a.sttab), (CTab::Status, StatusTab::Bus), "the tab and the sub-tab stayed");
    assert!(tui::cauldron::status::is_open(&a, "triggers"), "the band he opened is still open");
    assert!(screen(&mut a).lines().any(|l| l.contains("▾ Triggers")));
    a.set_ctab(CTab::Cauldron);
    assert_eq!(a.ktab, KTab::Questions);
    assert!(!a.band_open(scope), "the band he shut is still shut");
    // And back again (Status's own bands reset when he LEFT Status, rule
    // 10, which is not the switch).
    a.switch_env("prod").unwrap();
    assert!(!a.band_open(scope));
    // By the dropdown, as he does it: the same.
    a.set_ctab(CTab::Status);
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)));
    let dev = a.dropdown.as_ref().unwrap().entries.iter().position(|e| e.0 == "dev").unwrap();
    a.handle(key(KeyCode::Home));
    for _ in 0..dev {
        a.handle(key(KeyCode::Down));
    }
    a.handle(key(KeyCode::Enter));
    assert_eq!((a.env.name.as_str(), a.ctab, a.sttab), ("dev", CTab::Status, StatusTab::Bus));
}

// The switch the code names as dev-only is on in this build.
const _: () = assert!(tui::cauldron::designs::DEV_RENAME);

/// N10: DEV ONLY — a double click on a title (a tab, the sidebar's
/// `Clusters`, a box's legend) opens a rename modal; Enter keeps the new word
/// for every face on this machine; an empty one puts the word back. It
/// renames the word on screen and nothing else.
#[test]
fn n10_a_double_click_on_a_title_renames_it_on_screen_for_the_developer() {
    let root = scratch_root();
    let mut a = open(&root);
    // The soup box's legend.
    double_click(&mut a, Hit::Title(Title::Surface));
    assert!(matches!(&a.modal, Some(Modal::DevRename { key, name }) if key == "surface" && name == "surface"), "{:?}", a.modal);
    assert!(screen(&mut a).contains("Rename surface · dev only"));
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
    for c in "pot".chars() {
        a.handle(key(KeyCode::Char(c)));
    }
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none());
    let s = screen(&mut a);
    assert!(s.lines().nth(2).unwrap().contains(" pot "), "the legend says the new word: {s}");
    // Kept for every face on the machine, never in a cauldron's data.
    let file = root.join("data").join("shared").join("tui").join("dev_names.json");
    assert!(std::fs::read_to_string(&file).unwrap().contains("\"pot\""), "{}", file.display());
    let mut b = open(&root);
    assert!(screen(&mut b).lines().nth(2).unwrap().contains(" pot "));
    // A root tab, and the sidebar's title, too.
    double_click(&mut a, Hit::CTab(CTab::Threads));
    assert!(matches!(&a.modal, Some(Modal::DevRename { key, .. }) if key == "Threads"), "{:?}", a.modal);
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none(), "Esc keeps it as it was");
    // (Its first click picked the tab, as one click on a tab does.)
    assert_eq!(a.ctab, CTab::Threads);
    a.set_ctab(CTab::Cauldron);
    double_click(&mut a, Hit::Title(Title::Sidebar));
    assert!(matches!(&a.modal, Some(Modal::DevRename { key, .. }) if key == "Clusters"), "{:?}", a.modal);
    a.handle(key(KeyCode::Esc));
    // An empty name puts the code's word back.
    double_click(&mut a, Hit::Title(Title::Surface));
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
    a.handle(key(KeyCode::Enter));
    assert!(screen(&mut a).lines().nth(2).unwrap().contains(" surface "));
    // Only the word on screen: the tab's key and flag are what they were.
    assert_eq!(CTab::parse("cauldron"), Some(CTab::Cauldron));
}

/// N9: one soup, so a reference carries no cluster — `[Q-1]`, `[T-9]` — and a
/// cluster, when it must be named, is its minted id; a slug is no cluster.
#[test]
fn n9_a_reference_carries_no_cluster_and_a_cluster_is_its_minted_id() {
    let root = scratch_root();
    let mut a = open(&root);
    a.set_ktab(KTab::Questions);
    let q = tui::cauldron::app::Item::Question { cluster: 0, serial: 1 };
    assert_eq!(a.tag_for(q).unwrap().render(), "[Q-1]");
    let found = tui::cauldron::tags::scan("see [Q-1] and [cluster:X-2]");
    assert_eq!(found.len(), 2);
    assert_eq!(a.clusters_of(&found[0].1), vec!["X-1".to_string()], "Q-1 is Cauldron's: its owner, found in the one soup");
    assert_eq!(a.clusters_of(&found[1].1), vec!["X-2".to_string()]);
    assert!(tui::cauldron::tags::scan("[cluster:cauldron-interface]").is_empty(), "a slug is no cluster");
    assert!(tui::cauldron::tags::scan("[cauldron/Q-1]").is_empty(), "the old slugged reference reads as nothing");
    // Every serial is unique in the soup: the seed proves it by loading.
    let mut seen = std::collections::HashSet::new();
    for g in &a.forest.clusters {
        for q in &g.questions {
            assert!(seen.insert(format!("Q-{}", q.serial)), "Q-{} twice", q.serial);
        }
    }
}

/// S3, its structure (the look is B6's): Status › Tokens is one band per
/// token source at the top — no TOKENS band around them — each account a
/// card inside its source's band.
#[test]
fn s3_tokens_is_one_band_per_source_with_no_band_around_them() {
    let root = scratch_root();
    let mut a = open(&root);
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Tokens);
    let v = tui::cauldron::status::view(&a, 120);
    assert!(!v.items.iter().any(|i| i.key == "tokens"), "no TOKENS band");
    let sources: Vec<&str> = v.items.iter().filter(|i| i.key.starts_with("tok:")).map(|i| i.key.as_str()).collect();
    assert!(sources.contains(&"tok:claude_code") && sources.contains(&"tok:endpoints"), "{sources:?}");
    assert!(v.items.iter().filter(|i| i.key.starts_with("tok:")).all(|i| i.parent.is_none()), "each source is a top-level band");
    let acct = v.items.iter().find(|i| i.key == "acct:claude_code/acct2").unwrap();
    assert_eq!(acct.parent.as_deref(), Some("tok:claude_code"));
    let s = screen(&mut a);
    assert!(!s.lines().any(|l| l.contains("▾ Tokens") || l.contains("▸ Tokens")), "{s}");
    assert!(s.lines().nth(4).unwrap().contains("▾ Claude Code"), "the first source is the first band: {s}");
}
