//! V1.1 (release plan R-V1 item 1; product spec §12): the face's two tab
//! bars. *"Global, on top: Config, with TUI, devices, providers and accounts.
//! Environment, below: Cauldron, Threads, Status, Permissions."* — *"two tab
//! bars like on top of each other ... different colors"*. Each guard reads
//! the bars from what the frame RECORDS (the click targets `draw` pushes)
//! and the colours from the rendered buffer, never from the screen's text
//! alone, and each carries its opposition (testing doctrine 2).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use tui::cauldron::app::{App, Bar, CTab, Hit};
use tui::cauldron::menu::{CfgTab, SPage};
use tui::cauldron::store::seed_dir;
use tui::cauldron::{render_bg_at, render_fg_at, render_to_text};

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "two-bars-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path, name: &str) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: name.into(), name_source: Source::Default, data_root: root.join("data").join(name) }
}

fn open(root: &Path) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(root, "prod"), seed_dir(), true).expect("opens")
}

fn app() -> App {
    open(&scratch_root())
}

const W: u16 = 120;
const H: u16 = 40;

/// The root tabs drawn on row `y` at width `w`, left to right, with their
/// click targets: parsed from the hits the frame recorded.
fn bar(a: &mut App, w: u16, y: u16) -> Vec<(CTab, Rect)> {
    render_to_text(a, w, H).unwrap();
    let mut v: Vec<(CTab, Rect)> = a.hits.iter().filter_map(|(r, h)| match h {
        Hit::CTab(t) if r.y == y => Some((*t, *r)),
        _ => None,
    }).collect();
    v.sort_by_key(|(_, r)| r.x);
    v
}

fn labels(v: &[(CTab, Rect)]) -> Vec<&'static str> {
    v.iter().map(|(t, _)| t.label()).collect()
}

/// The tabs lit on row `y`: a lit tab is drawn in ink, every other one muted
/// (`view::draw_tabs`). Read from the buffer at each tab's first cell.
fn lit(a: &mut App, y: u16) -> Vec<CTab> {
    let tabs = bar(a, W, y);
    let ink = tui_render::palette::c().ink;
    tabs.into_iter().filter(|(_, r)| render_fg_at(a, W, H, r.x, r.y).unwrap() == ink).map(|(t, _)| t).collect()
}

// ── G1: two bars, the right members ─────────────────────────────────────────

#[test]
fn g1_row_0_is_the_global_bar_and_row_1_the_environment_bar() {
    let mut a = app();
    let top = bar(&mut a, W, 0);
    let env = bar(&mut a, W, 1);
    assert_eq!(labels(&top), ["TUI", "Devices", "Providers", "Accounts"], "row 0");
    assert_eq!(labels(&env), ["Cauldron", "Threads", "Status", "Permissions", "Settings"], "row 1");
    // Opposition: Status is on the environment bar and NOT the global one.
    assert!(!labels(&top).contains(&"Status") && labels(&env).contains(&"Status"));
    // The dropdown leads row 1, before its first tab; row 0 has none.
    let drop = a.hits.iter().find(|(_, h)| *h == Hit::Env).map(|(r, _)| *r).expect("the dropdown is drawn");
    assert_eq!(drop.y, 1, "the dropdown is on the environment bar");
    assert!(drop.right() <= env[0].1.x, "the dropdown is at its left: {drop:?} {:?}", env[0].1);
    let rows = render_to_text(&mut a, W, H).unwrap();
    assert!(rows[1].trim_start().starts_with("prod  ▾"), "{:?}", rows[1]);
    // Every tab is on the bar CTab::bar says, and nowhere else.
    for t in CTab::ALL {
        let y = if t.bar() == Bar::Global { 0 } else { 1 };
        assert_eq!(a.hits.iter().filter(|(_, h)| *h == Hit::CTab(t)).map(|(r, _)| r.y).collect::<Vec<_>>(), vec![y], "{t:?}");
    }
    // No caption names a bar; no `Config` tab.
    assert!(!rows[0].contains("Config") && !rows[1].contains("Config") && !rows[0].contains("Global"), "{:?}\n{:?}", rows[0], rows[1]);
    // A click on each tab shows it.
    for t in CTab::ALL {
        if t == CTab::Threads {
            continue; // its page dials the daemon; its hit is checked above
        }
        let r = a.hits.iter().find(|(_, h)| *h == Hit::CTab(t)).map(|(r, _)| *r).unwrap();
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
            a.handle(Event::Mouse(MouseEvent { kind, column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
        }
        assert_eq!(a.ctab, t, "a click on {}", t.label());
        render_to_text(&mut a, W, H).unwrap();
    }
}

// ── G2: the bars are two colours ────────────────────────────────────────────

#[test]
fn g2_the_two_bars_are_on_different_grounds() {
    let mut a = app();
    let top = bar(&mut a, W, 0);
    let env = bar(&mut a, W, 1);
    let bg = |a: &mut App, x: u16, y: u16| render_bg_at(a, W, H, x, y).unwrap();
    // A tab's own cell, and the bar's empty left, on each row.
    let (g_tab, g_left) = (bg(&mut a, top[1].1.x, 0), bg(&mut a, 0, 0));
    let (e_tab, e_gap) = (bg(&mut a, env[1].1.x, 1), bg(&mut a, env[0].1.x - 2, 1));
    assert_ne!(g_tab, e_tab, "the global bar and the environment bar differ");
    // Opposition: within ONE bar two cells share its colour, so the probe
    // compares bars, not any two cells.
    assert_eq!(g_tab, g_left, "row 0 is one ground");
    assert_eq!(e_tab, e_gap, "row 1 is one ground (outside the dropdown)");
    // And those are the palette roles, not ad-hoc colours.
    assert_eq!(g_tab, tui::cauldron::look::global_bar_bg());
    assert_eq!(e_tab, tui::cauldron::look::env_bar_bg());
    assert_ne!(g_tab, tui_render::palette::c().surface, "the bar is not the page's ground");
}

// ── G3: one lit tab across both bars ────────────────────────────────────────

#[test]
fn g3_exactly_one_tab_is_lit_in_the_two_bars() {
    let mut a = app();
    a.set_ctab(CTab::Accounts);
    let (g, e) = (lit(&mut a, 0), lit(&mut a, 1));
    assert_eq!(e, Vec::<CTab>::new(), "on Accounts, nothing on the environment bar is lit");
    assert_eq!(g, [CTab::Accounts], "Accounts alone is lit");
    a.set_ctab(CTab::Status);
    let (g, e) = (lit(&mut a, 0), lit(&mut a, 1));
    assert_eq!(g, Vec::<CTab>::new(), "on Status, nothing on the global bar is lit");
    assert_eq!(e, [CTab::Status], "Status alone is lit");
    // Every tab: exactly one lit, and it is the one shown.
    for t in CTab::ALL.into_iter().filter(|t| *t != CTab::Threads) {
        a.set_ctab(t);
        let mut both = lit(&mut a, 0);
        both.extend(lit(&mut a, 1));
        assert_eq!(both, [t], "{t:?}");
    }
}

// ── G4: switching environment keeps the tab and its bands ──────────────────

#[test]
fn g4_an_environment_switch_keeps_a_global_tab_and_its_open_band() {
    let mut a = app();
    a.set_ctab(CTab::Accounts);
    let band = SPage::Pool("claude_code".into());
    assert!(!a.snav.is_open(&band), "sentinel: shut by default");
    a.snav.set_open(band.clone(), true);
    a.switch_env("dev").unwrap();
    // Opposition: the switch happened — the dropdown names the other one.
    let rows = render_to_text(&mut a, W, H).unwrap();
    assert!(rows[1].trim_start().starts_with("dev  ▾"), "{:?}", rows[1]);
    assert_eq!(a.env.name, "dev");
    assert_eq!((a.ctab, a.cfg_tab), (CTab::Accounts, CfgTab::Accounts), "the tab stayed");
    assert!(a.snav.is_open(&band), "the band stayed open");
    assert_eq!(lit(&mut a, 0), [CTab::Accounts]);
    // An environment tab is kept as well.
    a.set_ctab(CTab::Status);
    a.switch_env("prod").unwrap();
    assert_eq!(a.ctab, CTab::Status);
}

// ── G5: the flags ───────────────────────────────────────────────────────────

/// The BUILT face's one frame, `--cauldron --dump` plus `args`, in `root`.
fn dump(root: &Path, args: &[&str]) -> (i32, String, String) {
    let mut c = crate::face::Face::new(&root.with_extension("face"));
    c.arg("--cauldron")
        .arg("--dump")
        .args(["--env", "prod", "--seeds"])
        .arg(seed_dir())
        .args(["--size", "120x40"])
        .args(args)
        .env("HOME", root)
        .env("CAULDRON_HOME", root)
        .env_remove("CAULDRON_ENV")
        .env_remove("CAULDRON_ENV_SOURCE")
        .env_remove("ENV")
        .stdin(Stdio::null());
    let o = c.output().expect("spawn tui");
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

#[test]
fn g5_tab_names_every_tab_config_tab_still_maps_and_config_is_refused() {
    let root = scratch_root();
    drop(open(&root)); // prod exists
    // --tab accounts: the accounts' sources, and none of Providers' own.
    let (rc, acc, err) = dump(&root, &["--tab", "accounts"]);
    assert_eq!(rc, 0, "{err}");
    assert!(acc.contains("Claude Code") && !acc.contains("Inference endpoints"), "{acc}");
    // --config-tab providers (deprecated, one release): Providers' page.
    let (rc, prov, err) = dump(&root, &["--config-tab", "providers"]);
    assert_eq!(rc, 0, "{err}");
    assert!(prov.contains("Inference endpoints") && !prov.contains("▸ Claude Code"), "{prov}");
    // --config-tab cauldron: this cauldron's page, which is Settings now.
    let (rc, set, err) = dump(&root, &["--config-tab", "cauldron"]);
    assert_eq!(rc, 0, "{err}");
    assert!(set.contains("cauldron enabled"), "{set}");
    let (_, set2, _) = dump(&root, &["--tab", "settings"]);
    assert_eq!(set, set2, "--config-tab cauldron is --tab settings");
    // --tab config is refused, naming the new tabs.
    let (rc, _, err) = dump(&root, &["--tab", "config"]);
    assert_ne!(rc, 0);
    for t in ["tui", "devices", "providers", "accounts", "settings"] {
        assert!(err.contains(t), "`--tab config` names {t}: {err}");
    }
    // Opposition: an unknown tab is refused too, and a known one is not.
    let (rc, _, err) = dump(&root, &["--tab", "foo"]);
    assert!(rc != 0 && err.contains("no tab `foo`"), "{rc} {err}");
    let (rc, _, err) = dump(&root, &["--tab", "tui"]);
    assert_eq!(rc, 0, "{err}");
    // USAGE lists --config-tab as deprecated.
    let (_, out, err) = {
        let mut c = crate::face::Face::new(&root.with_extension("face-help"));
        c.args(["--cauldron", "--help"]).env("HOME", &root).env("CAULDRON_HOME", &root).stdin(Stdio::null());
        let o = c.output().unwrap();
        (o.status.code(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
    };
    let help = format!("{out}{err}");
    assert!(help.contains("--config-tab   DEPRECATED"), "{help}");
    // Stored names keep resolving: `config` was Config's first page.
    assert_eq!(CTab::stored("config"), Some(CTab::Settings));
    assert_eq!(CTab::parse("config"), None);
}

// ── G6: both bars fit, one line each ────────────────────────────────────────

#[test]
fn g6_each_bar_is_one_line_within_the_width_at_120_80_and_60() {
    let mut a = app();
    for w in [120u16, 80, 60] {
        let rows = render_to_text(&mut a, w, H).unwrap();
        for (y, want) in [(0u16, CTab::GLOBAL.to_vec()), (1, CTab::ENV.to_vec())] {
            let tabs = bar(&mut a, w, y);
            assert_eq!(tabs.iter().map(|(t, _)| *t).collect::<Vec<_>>(), want, "{w}: row {y}");
            for (t, r) in &tabs {
                assert_eq!(r.width as usize, t.label().chars().count(), "{w}: {t:?} is whole: {:?}", rows[y as usize]);
                assert!(r.right() <= w, "{w}: {t:?} is inside the width");
                assert!(rows[y as usize].contains(t.label()), "{w}: {t:?} on its row: {:?}", rows[y as usize]);
            }
            // One line, never wrapped or misplaced (V1-CLEAN1, V1.1-I §5: the
            // old `chars().count() <= w` could not fail — render_to_text trims
            // every row to the width): under each tab's click target is that
            // tab's whole label, cell for cell.
            for (t, r) in &tabs {
                let drawn: String = rows[y as usize].chars().skip(r.x as usize).take(r.width as usize).collect();
                assert_eq!(drawn, t.label(), "{w}: {t:?}'s target is over its label: {:?}", rows[y as usize]);
            }
        }
        // No root-tab target landed on any row but the two bars'.
        let stray: Vec<_> = a.hits.iter().filter(|(r, h)| matches!(h, Hit::CTab(_)) && r.y > 1).collect();
        assert!(stray.is_empty(), "{w}: a root tab spilled off its bar: {stray:?}");
        // Row 2 is the page, not a bar's spill.
        assert!(!rows[2].contains("Accounts") && !rows[2].contains("Permissions"), "{w}: {:?}", rows[2]);
    }
    // Opposition: at 120 the full, roomy labels show (` | `), at 60 the
    // environment bar is tight (`|`).
    let rows = render_to_text(&mut a, 120, H).unwrap();
    assert!(rows[0].ends_with("TUI | Devices | Providers | Accounts"), "{:?}", rows[0]);
    assert!(rows[1].ends_with("Cauldron | Threads | Status | Permissions | Settings"), "{:?}", rows[1]);
    let rows = render_to_text(&mut a, 60, H).unwrap();
    assert!(rows[1].ends_with("Cauldron|Threads|Status|Permissions|Settings"), "{:?}", rows[1]);
}

// ── G7: no numbers on the root tabs ─────────────────────────────────────────

#[test]
fn g7_no_root_tab_carries_a_number() {
    let mut a = app();
    let tabs: Vec<(CTab, Rect)> = [bar(&mut a, W, 0), bar(&mut a, W, 1)].concat();
    assert_eq!(tabs.len(), 9);
    let rows = render_to_text(&mut a, W, H).unwrap();
    for (t, r) in &tabs {
        let drawn: String = rows[r.y as usize].chars().skip(r.x as usize).take(r.width as usize).collect();
        assert_eq!(drawn, t.label(), "{t:?}");
        assert!(!drawn.chars().any(|c| c.is_ascii_digit()), "{t:?}: {drawn:?}");
    }
    // Opposition: the probe reads the root bars only — the cluster tab bar
    // under them still carries its count.
    let x1 = a.forest.find("X-1").unwrap();
    a.select(x1);
    let n = a.active_questions();
    assert!(n > 0, "sentinel: the seed waits on him");
    let rows = render_to_text(&mut a, W, H).unwrap();
    assert!(rows.iter().skip(2).any(|l| l.contains(&format!("Questions {n}"))), "{}", rows.join("\n"));
}

// ── navigation ──────────────────────────────────────────────────────────────

#[test]
fn shift_tab_cycles_the_environment_bar_then_the_global_and_opt_shift_tab_crosses() {
    let mut a = app();
    let key = |c: KeyCode, m: KeyModifiers| Event::Key(KeyEvent::new(c, m));
    // From Status, so the walk never shows Threads (it dials the daemon).
    a.set_ctab(CTab::Status);
    let mut seen = vec![a.ctab];
    for _ in 0..6 {
        a.handle(key(KeyCode::BackTab, KeyModifiers::SHIFT));
        seen.push(a.ctab);
    }
    assert_eq!(seen, [CTab::Status, CTab::Permissions, CTab::Settings, CTab::Tui, CTab::Devices, CTab::Providers, CTab::Accounts]);
    a.handle(key(KeyCode::BackTab, KeyModifiers::SHIFT));
    assert_eq!(a.ctab, CTab::Cauldron, "wraps to the environment bar's first");
    // F1 … F9 in the same order.
    a.handle(key(KeyCode::F(9), KeyModifiers::NONE));
    assert_eq!(a.ctab, CTab::Accounts);
    a.handle(key(KeyCode::F(5), KeyModifiers::NONE));
    assert_eq!(a.ctab, CTab::Settings);
    // ⌥⇧Tab: to the other bar, the tab last shown there — and back.
    a.handle(key(KeyCode::BackTab, KeyModifiers::SHIFT | KeyModifiers::ALT));
    assert_eq!(a.ctab, CTab::Accounts, "the global tab last shown");
    a.handle(key(KeyCode::BackTab, KeyModifiers::SHIFT | KeyModifiers::ALT));
    assert_eq!(a.ctab, CTab::Settings, "and back");
    a.set_ctab(CTab::Devices);
    a.handle(key(KeyCode::BackTab, KeyModifiers::SHIFT | KeyModifiers::ALT));
    assert_eq!(a.ctab, CTab::Settings);
}

/// Moving between two settings pages is not leaving them (it was one tab,
/// Config); leaving for Status resets them, as leaving Config did.
#[test]
fn bands_survive_a_move_between_pages_and_reset_on_leaving_them() {
    let mut a = app();
    a.set_ctab(CTab::Accounts);
    let band = SPage::Pool("claude_code".into());
    a.snav.set_open(band.clone(), true);
    a.set_ctab(CTab::Tui);
    a.set_ctab(CTab::Accounts);
    assert!(a.snav.is_open(&band), "kept across settings pages");
    a.set_ctab(CTab::Status);
    a.set_ctab(CTab::Accounts);
    assert!(!a.snav.is_open(&band), "reset on leaving them (ui.preserve.config is off)");
}

/// Accounts holds the sources' tables Providers held; Providers keeps its
/// defaults and its endpoints.
#[test]
fn accounts_holds_the_sources_and_providers_the_endpoints() {
    let a = app();
    let bands = |p: CfgTab| a.page_entries(&p.root()).into_iter().filter_map(|e| e.band()).collect::<Vec<_>>();
    let acc = bands(CfgTab::Accounts);
    let prov = bands(CfgTab::Providers);
    assert!(acc.contains(&SPage::Pool("claude_code".into())), "{acc:?}");
    assert!(!prov.iter().any(|p| matches!(p, SPage::Pool(_))), "{prov:?}");
    assert!(prov.contains(&SPage::Endpoints) && !acc.contains(&SPage::Endpoints));
}
