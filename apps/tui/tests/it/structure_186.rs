//! His msg-186 review, the STRUCTURE half (task 0165, checklist 0163): the
//! cauldron's switch moved into Config and asks first (BREWING / BANKED);
//! Work is CONTENTS; aggregate ripples are SPLASHES; the dump area is the
//! BUCKET (DUMP is the verb), with a copy button, ⌥A and ⌥M; Overview is
//! bands; Soup and Artifacts are one tab; Questions has an ASSUMED band;
//! clusters carry no budget; Status and Config have sub-tabs (msg 235 took
//! System, dictation and the cluster's Config away); the
//! token sources are GLOBAL. Each test names the checklist ids it proves.
//! Hermetic: a scratch root, CAULDRON_TYPE_ONE=off.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, Band, CTab, Focus, Hit, Item, Modal, DBand, Outbox, QBand, KTab};
use tui::cauldron::menu::{Bound, CfgTab, Entry, SPage, SETTINGS_ROW};
use tui::cauldron::render_to_text;
use tui::cauldron::settings::{Ctx, Val};
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::{global_dir, seed_dir, soup_dir};

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "structure186-{}-{}-{}",
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

fn open_env(root: &Path, name: &str) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(root, name), seed_dir(), true).expect("seed loads")
}

fn open() -> (App, PathBuf) {
    let root = scratch_root();
    (open_env(&root, "prod"), root)
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn alt(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT))
}

fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(key(KeyCode::Char(c)));
    }
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 70).unwrap().join("\n")
}

fn click_hit(a: &mut App, hit: Hit) {
    screen(a);
    let r = a.hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {hit:?} on screen"));
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
}

fn line_with<'a>(s: &'a str, needle: &str) -> &'a str {
    s.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("no line with `{needle}`:\n{s}"))
}

// ── A1 A2 A3 A4 ──────────────────────────────────────────────────────────────

/// A1 A2 A3 A4: no switch in the top bar; the FIRST item of Config (its
/// first tab, this cauldron's) is the cauldron enabled or disabled, on by default; flipping
/// it — by Space, Enter, →, a click, or ^T — asks first, in the words chosen.
#[test]
fn the_cauldrons_switch_is_its_tabs_first_row_enabled_or_disabled_and_asks_first() {
    let (mut a, root) = open_env_fresh();
    let top = render_to_text(&mut a, 120, 30).unwrap()[0].clone();
    assert!(top.starts_with("  prod  ▾") && !top.contains('●') && !top.contains(" on "), "A1: {top}");
    assert!(!a.hits.iter().any(|(_, h)| matches!(h, Hit::Strip(_))), "no strip on Surface");
    // THIS cauldron's tab (msg 235: named for it; b964d571 msg 23: the first), and its first
    // row (under the strip) is the switch.
    a.handle(key(KeyCode::F(4)));
    a.set_cfg_tab(CfgTab::Cauldron);
    let first = a.selectable().into_iter().nth(1).unwrap();
    assert!(matches!(&first, Entry::Set(b) if b.key == "cauldron.enabled"), "A2: {first:?}");
    assert!(a.enabled(), "A2: active by default");
    let s = screen(&mut a);
    // The segmented toggle (msg 235: "cauldron enabled ... two options
    // enabled. Or disabled"), the one in force lit.
    assert!(s.lines().any(|l| l.contains("cauldron enabled") && l.contains(" enabled  disabled ")), "A4: {s}");
    let lit = |a: &App| a.toggle_of(&Bound::new("cauldron.enabled", Ctx::default())).selected;
    assert_eq!(lit(&a), 0, "enabled in force");
    // Every way of flipping it asks.
    for k in [KeyCode::Char(' '), KeyCode::Enter, KeyCode::Right] {
        a.rows[SETTINGS_ROW] = 1;
        a.handle(key(k));
        let Some(Modal::Confirm { title, body, .. }) = a.modal.clone() else { panic!("{k:?}: {:?}", a.modal) };
        assert_eq!(title, "Disable the prod cauldron?", "A3 A4");
        assert_eq!(body, vec!["Are you sure you want to pause prod?".to_string()], "A3");
        // Esc (or ←) keeps it enabled.
        a.handle(key(KeyCode::Esc));
        assert!(a.enabled());
    }
    // Enter does it: disabled, and the cauldrons dropdown says so too.
    a.rows[SETTINGS_ROW] = 1;
    a.handle(key(KeyCode::Char(' ')));
    let s = screen(&mut a);
    assert!(s.contains("┏ Disable the prod cauldron? ") && s.contains("Are you sure you want to pause prod?"), "{s}");
    a.handle(key(KeyCode::Enter));
    assert!(!a.enabled());
    assert!(file(&root, "settings.toml").contains("[cauldron]\nenabled = false"));
    assert_eq!(lit(&a), 1, "disabled in force");
    assert!(a.notice.as_deref() == Some("prod is disabled"), "{:?}", a.notice);
    a.open_cauldrons();
    let s = screen(&mut a);
    assert!(line_with(&s, "│ prod").contains("disabled"), "{s}");
    a.dropdown = None;
    // ^T, from anywhere, asks the other way.
    a.handle(key(KeyCode::F(1)));
    a.handle(ctrl('t'));
    let Some(Modal::Confirm { title, body, .. }) = a.modal.clone() else { panic!("{:?}", a.modal) };
    assert_eq!((title.as_str(), body[0].as_str()), ("Enable the prod cauldron again?", "Are you sure you want to resume prod?"));
    a.handle(key(KeyCode::Right));
    assert!(a.enabled());
}

/// A fresh prod: no settings file of its own yet (the seed reads in place).
fn open_env_fresh() -> (App, PathBuf) {
    open()
}

fn file(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(soup_dir(&env_in(root, "prod")).unwrap().join(rel)).unwrap_or_default()
}

// ── N1 N2 N3 ────────────────────────────────────────────────────────────────

/// N1: the tabs are Contents · Status · System · Config, and `work` still
/// parses as Contents. N2: an aggregate ripple is a SPLASH, wherever the UI
/// names its kind. N3: the input area is the BUCKET.
#[test]
fn contents_splash_and_bucket_are_the_words_on_screen() {
    let (mut a, _r) = open();
    let s = screen(&mut a);
    // msg 235: the root tabs are Surface · Threads · Status · Config;
    // 8e4fca10 msg 457 renamed them Cauldron · Agents · Status · Config.
    // b964d571 msg 23: a `|` parts the menu items.
    assert!(s.lines().next().unwrap().ends_with("Cauldron | Agents | Status | Config"), "N1");
    assert!(!s.contains("Contents ·") && !s.contains("System"));
    assert_eq!(CTab::parse("cauldron"), Some(CTab::Cauldron));
    assert_eq!(CTab::parse("surface"), None, "renamed at the source (msg 457), no alias");
    assert_eq!(CTab::parse("contents"), None, "renamed at the source, no alias");
    assert_eq!(CTab::Cauldron.label(), "Cauldron");
    // The Cauldron tab is two boxes: `surface` above (msg 457), `bucket` below.
    assert!(s.lines().nth(1).unwrap().starts_with("╭") && s.lines().nth(1).unwrap().contains(" surface "), "{s}");
    assert!(!s.lines().nth(1).unwrap().contains(" soup "), "{s}");
    assert!(s.lines().any(|l| l.starts_with("╭") && l.contains(" bucket ")), "{s}");
    // Its copy at its bottom right (8e4fca10 msg 457).
    assert!(s.lines().last().unwrap().ends_with("┤ ⧉ copy ├╯"), "{s}");
    assert!(!s.contains("Dump · ") && !s.contains("Bucket · "), "{s}");
    // DUMP is the verb: Enter dumps the bucket, and the record is a dump.
    a.focus_input();
    typed(&mut a, "a thought");
    a.handle(key(KeyCode::Enter));
    assert!(a.notice.as_deref().unwrap_or("").starts_with("dump "), "{:?}", a.notice);
    // N2: the Bus's ripples, read-only (msg 235), say splash.
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Bus);
    a.stnav.insert("ripples".into());
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains(" dump ") && l.contains("splash")), "{s}");
    assert!(!s.lines().any(|l| l.contains(" today ") && l.contains(" shell ")), "no ripple's kind says shell: {s}");
    assert_eq!(tui::cauldron::ripples::Kind::Shell.label(), "splash");
    let k: tui::cauldron::ripples::Kind = serde_json::from_str("\"splash\"").unwrap();
    assert_eq!(k, tui::cauldron::ripples::Kind::Shell, "the stored word may be splash too");
}

// ── B1 B2 ───────────────────────────────────────────────────────────────────

/// B1: the bucket's bottom edge carries `⧉ copy` (b964d571 msg 23); a click on it, ⌥A, or iTerm's
/// `å` copies the WHOLE bucket, each chip as its text (a chip means nothing
/// outside this face). Ctrl-A stays line-start in the bucket. msg 235: there
/// is no dictation; ⌥M is nothing, and never types an `m`.
#[test]
fn the_bucket_copies_whole_and_there_is_no_dictation() {
    let (mut a, _r) = open();
    a.focus_input();
    typed(&mut a, "first");
    a.handle(Event::Paste("one\ntwo\nthree\nfour".into()));
    let text = a.composer.text();
    assert!(text.contains("[paste "), "a chip: {text}");
    a.outbox.clear();
    click_hit(&mut a, Hit::BucketCopy);
    assert_eq!(a.outbox, vec![Outbox::Copy("firstone\ntwo\nthree\nfour".into())], "B1: the chip's body, not the chip");
    a.outbox.clear();
    a.handle(alt('a'));
    assert_eq!(a.outbox.len(), 1, "B1: ⌥A");
    assert!(a.composer.text() == text, "nothing typed by ⌥A");
    // iTerm's `å` copies from the page; in the bucket it is a letter he
    // typed (0220 E10: a Nordic keyboard's å must never be swallowed).
    a.focus = Focus::Tree;
    a.handle(key(KeyCode::Char('å')));
    assert_eq!(a.outbox.len(), 2, "B1: `å` from the page");
    assert!(a.composer.text() == text, "nothing typed by `å` on the page");
    a.focus_input();
    // From the page too, and from another cauldron tab.
    a.focus = Focus::Tree;
    a.set_ctab(CTab::Status);
    a.outbox.clear();
    a.handle(alt('a'));
    assert_eq!(a.outbox.len(), 1);
    // ^A is line start, as in the harness composer.
    a.set_ctab(CTab::Cauldron);
    a.focus_input();
    a.handle(ctrl('a'));
    assert_eq!(a.composer.text(), text);
    // An empty bucket says so and copies nothing.
    a.composer = tui_render::compose::Composer::new();
    a.outbox.clear();
    a.handle(alt('a'));
    assert!(a.outbox.is_empty() && a.notice.as_deref().unwrap_or("").contains("empty"));
    // msg 235: no dictation. ⌥M in the bucket types nothing.
    a.focus_input();
    a.handle(alt('m'));
    assert_eq!(a.composer.text(), "", "⌥M is a chord, never the letter");
    assert!(a.modal.is_none());
    // The palette names ⌥A, and no dictation key.
    let keys: Vec<&str> = tui::cauldron::app::KEYMAP.iter().map(|(k, _)| *k).collect();
    assert!(keys.iter().any(|k| k.contains("⌥A")) && !keys.iter().any(|k| k.contains("⌥M")));
    assert!(!tui::cauldron::app::KEYMAP.iter().any(|(k, w)| k.contains("d (Work)") || k.contains("^V") || w.to_lowercase().contains("dictat")));
}

// ── T1 T6 ───────────────────────────────────────────────────────────────────

/// T1: Overview is collapsible bands, every one open at first, each closing
/// and opening by the band grammar, its items inside it. T6: Questions'
/// second band is ASSUMED, always there, its questions answered by an
/// assumption that stands.
#[test]
fn overview_is_bands_and_questions_have_an_assumed_band_second() {
    let (mut a, _r) = open();
    a.set_ktab(KTab::Details);
    let items = a.items();
    let bands: Vec<Item> = items.iter().copied().filter(|i| matches!(i, Item::Band(_))).collect();
    let want = [DBand::Scope, DBand::LastDump, DBand::Directives, DBand::Constraints, DBand::Objectives, DBand::Milestones, DBand::Cluster].map(|b| Item::Band(Band::Details(b)));
    assert_eq!(bands, want, "T1");
    let s = screen(&mut a);
    // T1: the bands are lines, not boxes; the nodes in them are cards (V1).
    assert!(s.lines().any(|l| l.split('│').nth(2).is_some_and(|p| p.trim_start().starts_with("• Directives"))), "T1: bands");
    assert!(s.lines().any(|l| l.contains(" DIR-1 ─╮")), "V1: a directive is a card");
    a.focus = Focus::Content;
    a.item = items.iter().position(|i| *i == Item::Band(Band::Details(DBand::Milestones))).unwrap();
    // 8e4fca10 msg 385: the band never closes; ← goes up, not shut.
    a.handle(key(KeyCode::Left));
    assert!(a.band_open(Band::Details(DBand::Milestones)));
    assert!(screen(&mut a).contains(" M-1 ─╮"));
    // T6.
    a.set_ktab(KTab::Questions);
    let order: Vec<Item> = a.items().into_iter().filter(|i| matches!(i, Item::Band(Band::Questions(_)))).collect();
    assert_eq!(order[0], Item::Band(Band::Questions(QBand::Active)));
    assert_eq!(order[1], Item::Band(Band::Questions(QBand::Assumed)), "T6: second");
    let page = |a: &App| tui::cauldron::pages::page(a, 110).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
    let s = page(&a);
    assert!(has_band(&s, "▸ Assumed", 1), "{s}");
    a.focus = Focus::Content;
    a.item = a.items().iter().position(|i| *i == Item::Band(Band::Questions(QBand::Assumed))).unwrap();
    a.handle(key(KeyCode::Right));
    // The card's text as one run, wrapped where its column ends (two in
    // from the band's title, 0211 R4).
    let s = page(&a);
    let flat = s.lines().map(|l| l.trim().trim_matches('│').trim()).collect::<Vec<_>>().join(" ");
    assert!(flat.contains("Do clusters carry budgets of their own?") && s.contains("Assumed") && flat.contains("Assumed until"), "{s}");
    // Shown even where nothing is assumed.
    a.select(a.forest.find("X-9").unwrap());
    a.set_ktab(KTab::Questions);
    assert!(has_band(&page(&a), "Assumed", 0), "{}", page(&a));
}

// ── T2 T3 T4 T5 ─────────────────────────────────────────────────────────────

/// T2 T3: Soup and Artifacts are one tab, its bands REGIONS · ARTIFACTS ·
/// REPOS · DIRECTORIES, no FILE SYSTEM; regions are bands opening to their
/// nodes. T4: expanding shows real examples on many clusters. T5: an
/// artifact is a soup node wrapping a file, in its regions.
#[test]
fn soup_holds_regions_artifacts_repos_and_directories_with_examples_everywhere() {
    let (mut a, _r) = open();
    assert_eq!(KTab::ALL, [KTab::Questions, KTab::Actions, KTab::Tasks, KTab::Nodes, KTab::Status, KTab::Details], "T2 (msg 235: no Config; 8e4fca10 msg 385: Questions and Actions, then Tasks, and Details last)");
    let mut with_artifacts = 0;
    let mut with_dirs = 0;
    for key in ["X-1", "X-2", "X-6", "X-7", "X-8", "X-10", "X-11", "X-3"] {
        a.select(a.forest.find(key).unwrap());
        a.set_ktab(KTab::Nodes);
        let p = tui::cauldron::pages::page(&a, 110);
        let s: Vec<String> = p.lines.iter().map(|l| l.to_string()).collect();
        let s = s.join("\n");
        let at = |t: &str| s.lines().position(|l| l.trim_start().starts_with(&format!("▾ {t}")) || l.trim_start().starts_with(&format!("▸ {t}"))).unwrap_or_else(|| panic!("{key} {t}:\n{s}"));
        assert!(at("Regions") < at("Artifacts") && at("Artifacts") < at("Repos") && at("Repos") < at("Directories"), "{key}: {s}");
        assert!(!s.contains("FILE SYSTEM"), "{key}");
        // Its noun count (msg 403): `35 artifacts`.
        let n = |band: &str| s.lines().nth(at(band)).unwrap().split_whitespace().rev().nth(1).and_then(|w| w.parse::<usize>().ok()).unwrap_or(0);
        with_artifacts += usize::from(n("Artifacts") > 0);
        with_dirs += usize::from(n("Directories") > 0);
    }
    assert!(with_artifacts >= 6 && with_dirs >= 5, "T4: artifacts on {with_artifacts}, directories on {with_dirs} clusters");
    assert!(a.arts.directories.len() >= 7 && a.arts.artifacts.len() >= 45, "T4");
    // T5: every artifact sits in regions, and opens as a node.
    assert!(a.arts.artifacts.iter().all(|x| !x.regions.is_empty()), "T5");
    a.select(a.forest.find("X-11").unwrap());
    a.set_ktab(KTab::Nodes);
    a.focus = Focus::Content;
    let r31 = Item::Region(a.corpus.find_region("R-31").unwrap());
    a.item = a.items().iter().position(|i| *i == r31).unwrap();
    a.handle(key(KeyCode::Right));
    let s = screen(&mut a);
    assert!(s.contains("0002-report-fable-convergence"), "T5: an artifact among R-31's nodes: {s}");
    let art = Item::Art(tui::cauldron::artpage::hash("art:ART-207"));
    a.item = a.items().iter().position(|i| *i == art).unwrap();
    a.handle(key(KeyCode::Right));
    // Its details are a table (task 0166 V6): what it is, where it is.
    let page: String = tui::cauldron::pages::page(&a, 110).lines.iter().map(|l| l.to_string() + "\n").collect();
    assert!(page.contains("node     │ soup node ART-207 · wraps a file") && page.contains("regions  │ R-31"), "{page}");
    // A directory band lists what is really on disk.
    a.art_toggled.insert("directories".into());
    let dir = format!("dirroot:{}", a.arts.directories.iter().find(|d| d.claimed_by.iter().any(|c| c == "X-11")).unwrap().path);
    a.art_toggled.insert(dir);
    assert!(screen(&mut a).contains("artifacts/"), "the blueprint workspace's artifacts directory");
}

// ── T7 ──────────────────────────────────────────────────────────────────────

/// T7, msg 235: a cluster has no settings at all — no Config tab, no
/// cluster key in the taxonomy, and a cluster settings file is never read.
#[test]
fn clusters_have_no_settings_and_no_config_tab() {
    let root = scratch_root();
    let dir = soup_dir(&env_in(&root, "prod")).unwrap().join("clusters").join("X-9");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("settings.toml"), "[cluster.attention]\nquiet = true\n").unwrap();
    let mut a = open_env(&root, "prod");
    assert!(tui::cauldron::settings::DEFS.iter().all(|d| !d.key.starts_with("cluster.")));
    assert_eq!(KTab::parse("config"), None);
    a.select(a.forest.find("X-9").unwrap());
    let s = screen(&mut a);
    let strip = line_with(&s, " | Actions ");
    assert!(!strip.contains("Config"), "{strip}");
}

// ── C1 C2 C3 C5 ─────────────────────────────────────────────────────────────

/// C1, msg 235: Status's sub-tabs are Tokens · Mesh · Soup · Bus (b964d571
/// msg 23 put Tokens first); the Bus
/// holds the executions (RUNNING · PARKED · PENDING). C2: Status › Soup holds
/// the unclaimed regions and repos as bands. The strip is each tab's first
/// row: → steps forward, ← back, a click picks.
#[test]
fn status_has_sub_tabs_and_the_strip_follows_the_band_grammar() {
    let (mut a, _r) = open();
    a.handle(key(KeyCode::F(3)));
    assert_eq!(a.ctab, CTab::Status);
    a.focus = Focus::Content;
    // Tokens first, and Status opens on it (b964d571 msg 23) — until the
    // Ledger went before it (8e4fca10 msg 457).
    assert_eq!(a.sttab, StatusTab::Ledger);
    let mut seen = Vec::new();
    for _ in 0..4 {
        a.rows[0] = 0;
        a.handle(key(KeyCode::Right));
        seen.push(a.sttab);
    }
    assert_eq!(seen, [StatusTab::Tokens, StatusTab::Mesh, StatusTab::Soup, StatusTab::Bus], "C1");
    a.handle(key(KeyCode::Right));
    assert_eq!((a.sttab, a.rows[0]), (StatusTab::Bus, 1), "→ on the last steps into its rows");
    let s = screen(&mut a);
    assert!(s.contains("▾ Running") && s.contains("Parked") && s.contains("Pending") && !s.contains("▾ Devices"), "C1: {s}");
    a.rows[0] = 0;
    a.handle(key(KeyCode::Left));
    assert_eq!(a.sttab, StatusTab::Soup);
    let s = screen(&mut a);
    assert!(s.contains("▾ Unclaimed regions") && s.contains("▾ Unclaimed repos") && s.contains("▸ R-9") && s.contains("▸ orchard"), "C2: {s}");
    click_hit(&mut a, Hit::Strip(4));
    assert_eq!(a.sttab, StatusTab::Bus);
    // The sub-tab stays when he leaves and comes back.
    a.handle(key(KeyCode::F(1)));
    a.handle(key(KeyCode::F(3)));
    assert_eq!(a.sttab, StatusTab::Bus);
}

// ── C4 C6 ───────────────────────────────────────────────────────────────────

/// C4: Config is global tabs and THIS cauldron's. The global ones hold the
/// token sources every cauldron shares (Claude Code accounts, Codex) and the
/// inference endpoints (design 0199), kept ONCE under data/shared; the
/// cauldron's holds the switch first, which accounts THIS cauldron uses,
/// which endpoints it uses with each one's METERED CAP (C6), then its own
/// settings, Directories among them. A cauldron file holding a global key
/// hands it over once.
#[test]
fn config_is_global_and_cauldron_and_the_token_sources_are_kept_once() {
    let root = scratch_root();
    // His prod file held token-source keys, as the real one does.
    let dir = soup_dir(&env_in(&root, "prod")).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("settings.toml"), "[cauldron]\nenabled = true\n\n[pools.claude_code.accounts.acct2]\nshare = 30\n").unwrap();
    let mut a = open_env(&root, "prod");
    // Moved without a caption: nothing he wrote changes (0211 R2).
    assert!(a.notice.is_none(), "{:?}", a.notice);
    // On disk at his first change, never by the open (task 0209 D1).
    assert!(!global_dir(&a.env).join("settings.toml").exists(), "the open writes nothing");
    a.sset(&Bound::new("cauldron.opportunistic", Ctx::default()), Val::Bool(false)).unwrap();
    let g = std::fs::read_to_string(global_dir(&a.env).join("settings.toml")).unwrap();
    assert!(g.contains("[pools.claude_code.accounts.acct2]\nshare = 30"), "adopted: {g}");
    assert!(!std::fs::read_to_string(dir.join("settings.toml")).unwrap().contains("pools."), "and gone from the cauldron's file");
    // THIS cauldron's tab: first (b964d571 msg 23), named for it (msg 235).
    a.handle(key(KeyCode::F(4)));
    a.set_cfg_tab(CfgTab::Cauldron);
    let s = screen(&mut a);
    assert!(line_with(&s, " prod cauldron | TUI | Devices | Providers | Permissions ").contains("prod cauldron"), "{s}");
    let labels: Vec<String> = a.selectable().iter().filter_map(|e| match e {
        Entry::Sub { label, .. } => Some(label.clone()),
        Entry::Set(b) => Some(b.key.to_string()),
        _ => None,
    }).collect();
    // The sidebar is this cauldron's (its layout.json; 0214 A8).
    assert_eq!(&labels[..6], ["cauldron.enabled", "cauldron.opportunistic", "ui.sidebar_ratio", "Accounts", "Inference endpoints", "Directories"], "{labels:?}");
    a.open_settings_path("cauldron/endpoints").unwrap();
    let s = screen(&mut a);
    for row in ["metered cap", "use here", "jev", "scratch-vllm", "+ add endpoint"] {
        assert!(s.contains(row), "C4 C6 {row}: {s}");
    }
    // A role is bound to one of the endpoints this cauldron uses.
    assert_eq!(a.choices(&Bound::new("roles.{role}.endpoint", Ctx::role("type_one.secondary")))[0], "none");
    // The Providers tab: the sources, the endpoints, the defaults — no
    // OpenRouter source and nothing planned (design 0199).
    a.set_cfg_tab(CfgTab::Providers);
    let s = screen(&mut a);
    for row in ["▸ Claude Code", "▸ Codex", "▸ Inference endpoints"] {
        assert!(s.contains(row), "{row}: {s}");
    }
    // The defaults flat at its head, no band of them (task 0209 D4).
    assert!(!s.contains("▸ Defaults") && !s.contains("▾ Defaults") && s.contains("weekly threshold"), "{s}");
    for gone in ["OpenRouter", "planned", "+ add token source"] {
        assert!(!s.contains(gone), "{gone}: {s}");
    }
    assert!(!s.contains("▸ Directories") && !s.contains("▸ Roles"), "nothing of one cauldron's on Providers: {s}");
    a.open_settings_path("providers/endpoints").unwrap();
    let s = screen(&mut a);
    assert!(s.contains("▾ Inference endpoints") && s.contains("general") && s.contains("+ add endpoint"), "{s}");
    // Another cauldron reads the same global values, and its own switches.
    std::fs::create_dir_all(root.join("data/dev")).unwrap();
    let mut d = open_env(&root, "dev");
    let share = Bound::new("pools.{pool}.accounts.{account}.share", Ctx::account("claude_code", "acct2"));
    assert_eq!(d.sget(&share).val, Val::Int(30));
    a.sset(&share, Val::Int(35)).unwrap();
    d.sync();
    assert_eq!(d.sget(&share).val, Val::Int(35));
    let mine = Bound::new("cauldron.use.{ep}.metered_cap_usd", Ctx::ep("ep-1"));
    a.sset(&mine, Val::Int(40)).unwrap();
    d.sync();
    assert_eq!(d.sget(&mine).val, Val::Off, "a cauldron's metered cap is its own");
    // Which accounts a cauldron uses: Status says so on an account not used here.
    let use2 = Bound::new("cauldron.accounts.{pool}.{account}.allowed", Ctx::account("claude_code", "sam-broomstick"));
    a.sset(&use2, Val::Bool(false)).unwrap();
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Tokens);
    assert!(line_with(&screen(&mut a), "• sam-broomstick").contains("NOT USED HERE"));
    d.set_ctab(CTab::Status);
    d.set_sttab(StatusTab::Tokens);
    assert!(!line_with(&screen(&mut d), "• sam-broomstick").contains("NOT USED HERE"));
}

// ── the scroll invariant, on every new page ─────────────────────────────────

/// Scrolling never changes a row's shape (msg 165): on Overview's bands, the
/// merged Soup, every Status, System and Config sub-tab.
#[test]
fn no_row_grows_when_selected_on_the_new_pages() {
    let (mut a, _r) = open();
    for t in [KTab::Details, KTab::Nodes] {
        a.set_ktab(t);
        let lines = |a: &App| tui::cauldron::pages::page(a, 100).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
        let first = lines(&a);
        for i in 0..a.items().len() {
            a.item = i;
            assert_eq!(lines(&a), first, "{t:?}: selecting item {i} changed the page");
        }
    }
    a.set_ctab(CTab::Status);
    for t in StatusTab::ALL {
        a.set_sttab(t);
        let first = tui::cauldron::status::view(&a, 100).lines.len();
        for i in 0..a.dash_rows().len() {
            a.rows[0] = i;
            assert_eq!(tui::cauldron::status::view(&a, 100).lines.len(), first, "Status › {}: row {i}", t.label());
        }
    }
    // Config: every tab with every band open, and every row's modal — the
    // TEXT of every line, not only how many (msg 235: a table's row and a
    // button draw their own selection, as a background, never a shape).
    a.set_ctab(CTab::Config);
    let text = |l: Vec<ratatui::text::Line<'static>>| l.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    for t in CfgTab::ALL {
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
        a.rows[SETTINGS_ROW] = 0;
        let first = text(tui::cauldron::view::settings_entry_lines(&a, 100).0);
        let rows: Vec<Entry> = a.selectable();
        for i in 0..rows.len() {
            a.rows[SETTINGS_ROW] = i;
            assert_eq!(text(tui::cauldron::view::settings_entry_lines(&a, 100).0), first, "Config › {}: row {i}", t.label());
        }
        // Every row's modal, and every `+ add endpoint`'s (design 0199 §3).
        let mut pages: Vec<SPage> = Vec::new();
        for e in rows {
            match e {
                Entry::Row(p) => pages.push(p),
                Entry::Do { act: tui::cauldron::manage::Act::NewEndpoint { here }, .. } => {
                    a.settings.draft_endpoint(here, "local");
                    pages.push(SPage::Endpoint { here, id: "ep-0".into() });
                }
                _ => {}
            }
        }
        for p in pages {
            let n = a.modal_selectable(&p).len();
            let first = text(tui::cauldron::view::modal_entry_lines(&a, &p, 0, 80).0);
            for i in 0..n {
                assert_eq!(text(tui::cauldron::view::modal_entry_lines(&a, &p, i, 80).0), first, "{p:?}: row {i}");
            }
        }
    }
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
