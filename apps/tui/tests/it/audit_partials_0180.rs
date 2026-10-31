//! The msg-186 audit's partials (audit 0177; task 0180), closed at the
//! source: ONE band grammar on every tab — no `── TITLE ──` divider rule
//! anywhere, the Engine's directive rows without a leader, one band case
//! (V3, SR4); an ⓘ in the rightmost column of EVERY Config row, bands,
//! read-only and action rows included, each with an explanation (V7); and
//! enough fake soup that every cluster's Soup tab shows real examples on its
//! own, not only in aggregate (T4). Hermetic: a scratch root,
//! CAULDRON_TYPE_ONE=off.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tui::cauldron::app::{App, CTab, Focus, Hit, Modal, KTab};
use tui::cauldron::menu::{CfgTab, Entry, SPage};
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::seed_dir;
use tui::cauldron::render_to_text;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "partials180-{}-{}-{}",
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
    render_to_text(a, 130, 200).unwrap().join("\n")
}

fn cluster(a: &mut App, key: &str) {
    let g = a.forest.find(key).unwrap();
    a.select(g);
}

/// What is inside the frame's border, right of the sidebar when there is one.
fn page(row: &str) -> String {
    let inner = row.trim_start_matches('│').trim_end_matches('│');
    let cols: Vec<&str> = row.split('│').collect();
    if cols.len() > 3 { cols[2..cols.len() - 1].join("│") } else { inner.to_string() }
}

/// Open every band of a Config tab (sections are open already).
fn open_all(a: &mut App) {
    for _ in 0..12 {
        let closed: Vec<_> = a.entries().into_iter().filter(|f| f.open == Some(false)).filter_map(|f| f.entry.band()).collect();
        if closed.is_empty() {
            break;
        }
        for b in closed {
            a.snav.set_open(b, true);
        }
    }
}

/// Every screen the audit named a divider on, and every other tab.
fn every_screen(a: &mut App, mut f: impl FnMut(&str, &mut App)) {
    for g in ["X-1", "X-2", "X-6", "X-9"] {
        cluster(a, g);
        for agg in [false, true] {
            a.aggregate = agg;
            for t in KTab::ALL {
                a.set_ctab(CTab::Cauldron);
                a.set_ktab(t);
                f(&format!("{g} {t:?} aggregate={agg}"), a);
            }
        }
        a.aggregate = false;
    }
    for st in StatusTab::ALL {
        a.set_ctab(CTab::Status);
        a.set_sttab(st);
        f(&format!("Status {st:?}"), a);
    }
    for t in CfgTab::ALL {
        a.set_ctab(CTab::Config);
        a.set_cfg_tab(t);
        open_all(a);
        f(&format!("Config {t:?}"), a);
        // Every kind of a table's row, opened in its modal (msg 235).
        let mut kinds: Vec<std::mem::Discriminant<SPage>> = Vec::new();
        let rows: Vec<SPage> = a.selectable().into_iter().filter_map(|e| if let Entry::Row(p) = e { Some(p) } else { None }).collect();
        for p in rows {
            if kinds.contains(&std::mem::discriminant(&p)) {
                continue;
            }
            kinds.push(std::mem::discriminant(&p));
            a.open_row(p.clone());
            f(&format!("Config {t:?} modal {p:?}"), a);
            a.modal = None;
        }
    }
}

// ── V3 / SR4: one band grammar ─────────────────────────────────────────────

/// V3 SR4: no screen draws a `── TITLE ──` divider rule — no row of a page
/// starts with `── ` or ends with ` ──` — and every band's name is in the one
/// band case (never ALL CAPS: `Services`, `User input`, `Roots`).
#[test]
fn v3_sr4_no_divider_rule_and_one_band_case_on_every_tab() {
    let (mut a, _r) = open();
    let mut bands = 0;
    every_screen(&mut a, |what, a| {
        let s = screen(a);
        for l in s.lines().skip(1) {
            let p = page(l);
            let t = p.trim();
            assert!(!t.starts_with("── ") && !t.ends_with(" ──"), "{what}: a divider rule `{t}`\n{s}");
            for chev in ["▾ ", "▸ "] {
                if let Some(rest) = t.strip_prefix(chev) {
                    bands += 1;
                    let word: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
                    let caps = word.chars().count() > 3 && word.chars().all(|c| c.is_ascii_uppercase());
                    assert!(!caps, "{what}: band `{word}` is not in the band case: `{t}`");
                    // Nor half of it (`TaskS`): no capital after a small letter.
                    let mixed = word.chars().zip(word.chars().skip(1)).any(|(x, y)| x.is_ascii_lowercase() && y.is_ascii_uppercase()) && !word.starts_with("Open");
                    assert!(!mixed, "{what}: band `{word}` is not in the band case: `{t}`");
                }
            }
        }
    });
    assert!(bands > 200, "only {bands} band lines seen");
}

/// V3: the sections the audit named are bands now — on Config and Status
/// (msg 235 took System; its sections are the Bus's bands) — each a band
/// line with its chevron.
#[test]
fn v3_the_named_sections_are_band_lines() {
    let (mut a, _r) = open();
    let seen = |a: &mut App, want: &[&str]| {
        let s = screen(a);
        for w in want {
            assert!(s.lines().any(|l| page(l).trim_start().starts_with(&format!("▾ {w}"))), "no band `▾ {w}`:\n{s}");
        }
    };
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Bus);
    seen(&mut a, &["Events", "Executions", "Running"]);
    assert!(screen(&mut a).lines().any(|l| page(l).trim_start().starts_with("▸ Triggers")));
    a.set_sttab(StatusTab::Mesh);
    seen(&mut a, &["example-mac"]);
    a.set_sttab(StatusTab::Soup);
    seen(&mut a, &["Unclaimed regions", "Unclaimed repos"]);
    a.set_ctab(CTab::Config);
    a.set_cfg_tab(CfgTab::Cauldron);
    a.open_settings_path("cauldron/directories").unwrap();
    seen(&mut a, &["Roots"]);
    a.set_cfg_tab(CfgTab::Providers);
    a.open_settings_path("providers/claude_code").unwrap();
    let s = screen(&mut a);
    assert!(!s.contains("CLAUDE CODE"), "{s}");
    // The cluster's Overview: CLUSTER is a band at its foot (msg 235).
    a.set_ctab(CTab::Cauldron);
    cluster(&mut a, "X-1");
    a.set_ktab(KTab::Details);
    // 8e4fca10 msg 385: Details' bands are always expanded — a dot, never a chevron.
    let s = screen(&mut a);
    assert!(s.lines().any(|l| page(l).trim_start().starts_with("• Cluster")), "no band `• Cluster`:\n{s}");
}

/// V3: the Engine's directive rows carry no leader to their cite: the
/// directive's id, its words, and the cite flush right with spaces between.
#[test]
fn v3_engine_directive_rows_have_no_leader() {
    let (mut a, _r) = open();
    cluster(&mut a, "X-1");
    a.set_ktab(KTab::Tasks);
    let s = screen(&mut a);
    let row = s.lines().find(|l| page(l).contains("DIR-1")).unwrap_or_else(|| panic!("{s}"));
    let p = page(row);
    assert!(!p.contains('─'), "a leader on the directive row: `{p}`");
    assert!(p.trim_end().ends_with("8e4fca10#1"), "the cite is flush right: `{p}`");
    // Under aggregate each cluster's part is a band of the Engine, not a rule.
    a.aggregate = true;
    let s = screen(&mut a);
    assert!(s.lines().any(|l| page(l).trim_start().starts_with("▾ Interface")), "{s}");
}

/// V3: a section band closes and opens like every band — ← on it closes it
/// and its rows go; → opens it again — on Status › Bus and Config.
#[test]
fn v3_a_section_band_closes_and_opens() {
    let (mut a, _r) = open();
    // Status › Bus: `Events` holds the event types.
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Bus);
    a.focus = Focus::Content;
    let at = tui::cauldron::status::view(&a, 120).items.iter().position(|i| i.key == "events").expect("the Events band is a row");
    a.rows[0] = at;
    assert!(screen(&mut a).contains("dump.received"), "open: its rows show");
    a.handle(key(KeyCode::Left));
    let s = screen(&mut a);
    assert!(s.contains("▸ Events") && !s.contains("dump.received"), "← closes it:\n{s}");
    a.handle(key(KeyCode::Right));
    assert!(screen(&mut a).contains("dump.received"), "→ opens it again");
    // Config › Cauldron › Directories › Roots.
    a.set_ctab(CTab::Config);
    a.set_cfg_tab(CfgTab::Cauldron);
    a.open_settings_path("cauldron/directories").unwrap();
    let i = a.selectable().iter().position(|e| matches!(e, Entry::Sub { label, .. } if label == "Roots")).expect("Roots is a row");
    a.rows[tui::cauldron::menu::SETTINGS_ROW] = i;
    let n = a.selectable().len();
    a.handle(key(KeyCode::Left));
    assert!(screen(&mut a).contains("▸ Roots"));
    assert!(a.selectable().len() < n, "its roots are no longer rows");
    a.handle(key(KeyCode::Right));
    assert_eq!(a.selectable().len(), n);
}

// ── V7: ⓘ on every Config row ──────────────────────────────────────────────

/// V7: every row of every Config tab — the cauldron's two (a cluster has no
/// Config since msg 235), every band open — ends in the ⓘ at the pane's rightmost column, has a tip
/// hotspot there, and has an explanation: bands (an account's own on/off band
/// included), read-only facts, actions, items, quoted things. A click on a band's ⓘ, and `h` on a read-only row, open it.
#[test]
fn v7_every_config_row_has_an_explained_tip() {
    let (mut a, _r) = open();
    let w = 114;
    let mut kinds = std::collections::BTreeSet::new();
    for t in CfgTab::ALL {
        a.set_ctab(CTab::Config);
        a.set_cfg_tab(t);
        open_all(&mut a);
        let entries = a.entries();
        check(&a, t, &entries, w, &mut kinds);
    }
    for k in ["Sub", "Set", "Fact", "Do", "Ref", "Add", "Row", "Fixed"] {
        assert!(kinds.contains(k), "no {k} row was checked: {kinds:?}");
    }
    // A click on a table row's ⓘ explains it (and does not open it).
    a.set_ctab(CTab::Config);
    a.set_cfg_tab(CfgTab::Providers);
    a.snav.open.clear();
    a.open_settings_path("providers/claude_code").unwrap();
    let ix = a.entries().iter().position(|f| matches!(&f.entry, Entry::Row(_))).expect("an account's row");
    screen(&mut a);
    a.handle(Event::Mouse(tip_click(&a, ix)));
    match &a.modal {
        Some(Modal::Help { text, .. }) => assert!(text.contains("A click opens its settings"), "{text}"),
        m => panic!("no help: {m:?}"),
    }
    a.modal = None;
    // `h` on a read-only row explains it.
    a.set_cfg_tab(CfgTab::Cauldron);
    a.snav.open.clear();
    a.open_settings_path("cauldron/endpoints").unwrap();
    a.focus = Focus::Content;
    let i = a.selectable().iter().position(|e| matches!(e, Entry::Do { .. } | Entry::Sub { .. } | Entry::Set(_))).unwrap();
    a.rows[tui::cauldron::menu::SETTINGS_ROW] = i;
    a.handle(key(KeyCode::Char('h')));
    assert!(matches!(a.modal, Some(Modal::Help { .. })), "{:?}", a.modal);
}

fn tip_click(a: &App, ix: usize) -> ratatui::crossterm::event::MouseEvent {
    let r = a.hits.iter().find(|(_, h)| *h == Hit::Tip(ix)).map(|(r, _)| *r).expect("the band's ⓘ is on screen");
    ratatui::crossterm::event::MouseEvent { kind: ratatui::crossterm::event::MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left), column: r.x, row: r.y, modifiers: KeyModifiers::NONE }
}

fn check(a: &App, scope: CfgTab, entries: &[tui::cauldron::menu::Flat], w: usize, kinds: &mut std::collections::BTreeSet<&'static str>) {
    let (lines, _, tips, _) = tui::cauldron::view::settings_entry_lines(a, w);
    // The lines each entry begins on, in order (an entry's first line).
    let mut at = 0;
    let mut tipped = 0;
    for (ix, f) in entries.iter().enumerate() {
        let kind = match &f.entry {
            Entry::Strip | Entry::Gap | Entry::Para(_) => None,
            Entry::Sub { .. } => Some("Sub"),
            Entry::Row(_) => Some("Row"),
            Entry::Fixed(..) => Some("Fixed"),
            Entry::Set(_) | Entry::Run(_) => Some("Set"),
            Entry::Fact(..) => Some("Fact"),
            Entry::Do { .. } => Some("Do"),
            Entry::Ref { .. } => Some("Ref"),
            Entry::Add(_) => Some("Add"),
            Entry::Item { .. } => Some("Item"),
            Entry::Head(..) => Some("Head"),
        };
        let Some(kind) = kind else { continue };
        kinds.insert(kind);
        let &(line, col, _) = tips.iter().find(|t| t.2 == ix).unwrap_or_else(|| panic!("{scope:?} entry {ix} {:?}: no ⓘ", f.entry));
        assert!(line >= at, "{scope:?} entry {ix}: its ⓘ is on an earlier row");
        at = line;
        let text: String = lines[line].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with('ⓘ') && tui_render::compose::str_width(&text) == w && col == w - 1, "{scope:?} entry {ix} `{text}`");
        let (_, help) = a.entry_help(&f.entry).unwrap_or_else(|| panic!("{scope:?} entry {ix} {:?}: no explanation", f.entry));
        assert!(help.len() > 20, "{scope:?} entry {ix} {:?}: `{help}`", f.entry);
        tipped += 1;
    }
    assert!(tipped > 10, "{scope:?}: {tipped} rows");
}

// ── T4: every cluster's Soup has real examples ─────────────────────────────

/// T4: on its own (aggregate off), every cluster's Soup shows several
/// artifacts of more than one kind, at least one repo whose worktrees are
/// listed, and directories; and some cluster shows more than one repo.
#[test]
fn t4_every_cluster_soup_has_real_examples() {
    let (mut a, _r) = open();
    a.aggregate = false;
    let keys: Vec<String> = a.forest.clusters.iter().map(|x| x.key.clone()).collect();
    let mut many_repos = 0;
    for k in &keys {
        cluster(&mut a, k);
        let at = a.forest.find(k);
        let arts: Vec<_> = a.arts.artifacts.iter().filter(|x| x.cluster(&a.forest) == at).collect();
        let kinds: std::collections::BTreeSet<&str> = arts.iter().map(|x| x.kind.as_str()).collect();
        assert!(arts.len() >= 3 && kinds.len() >= 2, "{k}: {} artifacts of {kinds:?}", arts.len());
        let repos = a.cluster_repos();
        many_repos += usize::from(repos.len() > 1);
        assert!(
            repos.iter().any(|r| a.arts.worktrees.iter().any(|w| w.repo == a.repos.repos[r.repo].name)),
            "{k}: no repo with worktrees"
        );
        let dirs = a.arts.directories.iter().filter(|d| d.claimed_by.contains(k)).count();
        assert!(dirs >= 1, "{k}: no directories");
        // And the Soup tab shows them: the three bands, open, have rows.
        a.set_ctab(CTab::Cauldron);
        a.set_ktab(KTab::Nodes);
        for (band, key) in [("Artifacts", "created"), ("Repos", "repos"), ("Directories", "directories")] {
            if screen(&mut a).lines().any(|l| page(l).trim_start().starts_with(&format!("▸ {band}"))) {
                a.art_toggled.insert(key.into());
            }
        }
        let s = screen(&mut a);
        for band in ["Artifacts", "Repos", "Directories"] {
            let l = s.lines().find(|l| page(l).trim_start().starts_with(&format!("▾ {band}"))).unwrap_or_else(|| panic!("{k}: no open {band} band:\n{s}"));
            let n: String = page(l).trim_end().chars().rev().take_while(|c| !c.is_whitespace()).collect::<Vec<_>>().into_iter().rev().collect();
            assert!(!n.starts_with('0'), "{k}: {band} is empty: `{l}`");
        }
        a.art_toggled.clear();
    }
    assert!(many_repos >= 3, "only {many_repos} clusters show more than one repo");
}
