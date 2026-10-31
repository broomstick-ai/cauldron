//! Task 0205 (8e4fca10 msg 235): B6's two partials closed. C1 — a LIGHT
//! palette in the shared render crate, and Config › TUI › theme switching it
//! live. C2 — the repo registry is GLOBAL: one registry every cauldron sees, a
//! cauldron's clusters still claiming from it, each cauldron's own registry
//! adopted into it at the source (kept as `repos.json.bak`). The palette's
//! contrast is checked in `libs/tui-render/tests/golden.rs`
//! (`light_palette_*`). Hermetic: a scratch root, CAULDRON_TYPE_ONE=off.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use ratatui::Terminal;
use tui::cauldron::app::{App, CTab, Focus, Modal};
use tui::cauldron::blueprint::{Registered, Registry, RegistryFile};
use tui::cauldron::menu::{Bound, CfgTab, Entry, SPage, SETTINGS_ROW};
use tui::cauldron::settings::{Ctx, Val};
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::{global_dir, seed_dir, soup_dir};
use tui::cauldron::{render_bg_at, render_to_text};
use tui_render::palette::{self, Theme, DARK, LIGHT};

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0205-{}-{}-{}",
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

fn open_with(root: &Path, name: &str, persist: bool) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(root, name), seed_dir(), persist).expect("seed loads")
}

fn open_in(root: &Path, name: &str) -> App {
    open_with(root, name, true)
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(key(KeyCode::Char(c)));
    }
}

const W: u16 = 120;
const H: u16 = 80;

fn config(a: &mut App, path: &str) {
    a.set_ctab(CTab::Config);
    a.focus = Focus::Content;
    a.snav = tui::cauldron::menu::Nav::cauldron();
    a.rows[SETTINGS_ROW] = 0;
    a.open_settings_path(path).unwrap();
}

fn select_row(a: &mut App, pick: impl Fn(&Entry) -> bool) {
    let i = a.selectable().iter().position(pick).unwrap_or_else(|| panic!("no such row: {:?}", a.selectable()));
    a.rows[SETTINGS_ROW] = i;
}

fn select_do(a: &mut App, label: &str) {
    select_row(a, |e| matches!(e, Entry::Do { label: l, .. } if l == label));
}

/// Every cell's foreground and background of one frame, counted.
fn colours(a: &mut App) -> HashMap<Color, usize> {
    let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
    term.draw(|f| tui::cauldron::view::draw(f, a)).unwrap();
    let buf = term.backend().buffer();
    let mut out = HashMap::new();
    for y in 0..H {
        for x in 0..W {
            *out.entry(buf[(x, y)].fg).or_default() += 1;
            *out.entry(buf[(x, y)].bg).or_default() += 1;
        }
    }
    out
}

/// What one frame draws that is not a colour of `want`'s palette: a role of
/// the other palette, or any colour no palette names.
fn strays(seen: &HashMap<Color, usize>, want: &palette::Colors) -> Vec<Color> {
    let ok: Vec<Color> = want.roles().iter().map(|r| r.1).collect();
    seen.keys().copied().filter(|c| !ok.contains(c)).collect()
}

fn set_theme(a: &mut App, t: &str) {
    a.sset(&Bound::new("ui.theme", Ctx::default()), Val::Str(t.into())).unwrap();
}

// ── C1: a light palette, switched live ──────────────────────────────────────

/// A light frame is drawn in the light palette and NOTHING else, on every
/// top tab, every Status tab, every Config tab and a row's modal: no dark
/// colour survives anywhere, and the light surface is what most of the
/// screen is.
#[test]
fn c1_a_light_frame_is_drawn_in_the_light_palette_everywhere() {
    let mut a = open_in(&scratch_root(), "prod");
    set_theme(&mut a, "light");
    let mut frames: Vec<(String, HashMap<Color, usize>)> = Vec::new();
    crate::live_fixture::feed(&mut a);
    for t in [CTab::Cauldron, CTab::Threads] {
        a.set_ctab(t);
        frames.push((format!("{t:?}"), colours(&mut a)));
    }
    a.set_ctab(CTab::Status);
    for st in StatusTab::ALL {
        a.set_sttab(st);
        frames.push((format!("status {st:?}"), colours(&mut a)));
    }
    for t in CfgTab::ALL {
        a.set_ctab(CTab::Config);
        a.set_cfg_tab(t);
        frames.push((format!("config {t:?}"), colours(&mut a)));
    }
    config(&mut a, "providers/claude_code/acct2");
    assert!(a.modal.is_some(), "the row's modal is open");
    frames.push(("a row's modal".into(), colours(&mut a)));
    a.modal = None;
    config(&mut a, "devices/repos");
    frames.push(("devices › repos".into(), colours(&mut a)));
    for (what, seen) in &frames {
        assert!(strays(seen, &LIGHT).is_empty(), "{what}: drawn in colours that are not the light palette's: {:?}", strays(seen, &LIGHT));
        let surface = seen.get(&LIGHT.surface).copied().unwrap_or(0);
        assert!(surface * 2 > (W as usize) * (H as usize), "{what}: the light surface is not most of the background ({surface})");
        assert!(seen.contains_key(&LIGHT.ink) || seen.contains_key(&LIGHT.ink2), "{what}: no ink");
        assert!(!seen.contains_key(&Color::Reset), "{what}: a cell left in the terminal's own colours");
    }
    // The light frame really is different: not one dark role is drawn.
    for (what, seen) in &frames {
        for (role, c) in DARK.roles() {
            if !LIGHT.roles().iter().any(|r| r.1 == c) {
                assert!(!seen.contains_key(&c), "{what}: the dark `{role}` is drawn");
            }
        }
    }
    // And the dark frame is still the dark palette and nothing else.
    set_theme(&mut a, "dark");
    a.set_ctab(CTab::Cauldron);
    let seen = colours(&mut a);
    assert!(strays(&seen, &DARK).is_empty(), "dark: {:?}", strays(&seen, &DARK));
}

/// The theme is a row on Config › TUI, first, drawn as a two-word switch;
/// Space on it switches the whole face at once — the very next frame — and
/// it is a GLOBAL setting, so every cauldron opens in it.
#[test]
fn c1_the_theme_switch_on_config_tui_switches_the_face_live() {
    let root = scratch_root();
    let mut a = open_in(&root, "prod");
    config(&mut a, "tui");
    let s = render_to_text(&mut a, W, H).unwrap().join("\n");
    let row = s.lines().find(|l| l.contains("theme")).unwrap_or_else(|| panic!("no theme row:\n{s}"));
    assert!(row.contains(" dark ") && row.contains(" light ") && row.contains("default dark"), "{row}");
    assert!(s.find("theme").unwrap() < s.find("opens on").unwrap(), "the theme comes first:\n{s}");
    let corner = |a: &mut App| render_bg_at(a, W, H, W - 1, H - 1).unwrap();
    assert_eq!(corner(&mut a), DARK.surface);
    select_row(&mut a, |e| matches!(e, Entry::Set(b) if b.key == "ui.theme"));
    a.handle(key(KeyCode::Char(' ')));
    assert_eq!(a.theme(), Theme::Light);
    assert_eq!(corner(&mut a), LIGHT.surface, "the next frame is light");
    // Every cauldron opens light: the setting is global.
    let mut b = open_in(&root, "dev");
    assert_eq!(b.theme(), Theme::Light);
    assert_eq!(corner(&mut b), LIGHT.surface);
    // Space again: dark, the default, which reverts it.
    a.handle(key(KeyCode::Char(' ')));
    assert_eq!(a.theme(), Theme::Dark);
    assert_eq!(corner(&mut a), DARK.surface);
    assert!(!a.reverts(&Bound::new("ui.theme", Ctx::default())), "dark is the default again");
}

// ── C2: one repo registry, every cauldron's ─────────────────────────────────

fn register(a: &mut App, path: &str) {
    config(a, "devices/repos");
    select_do(a, "+ register repo");
    a.handle(key(KeyCode::Enter));
    typed(a, path);
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none(), "{:?}", a.modal);
}

/// A repo registered once, in one cauldron, is registered in every
/// cauldron — one opened after it, one with no seed of its own, and a face
/// already open on another — and it is kept once, in the global directory.
#[test]
fn c2_a_repo_registered_once_appears_in_every_cauldron() {
    let root = scratch_root();
    let mut dev = open_in(&root, "dev");
    let mut a = open_in(&root, "prod");
    register(&mut a, "/tmp/somewhere/Side_Project");
    assert!(a.repos.find_repo("side-project").is_some());
    let reg = Registry::global(&global_dir(&a.env)).read().unwrap();
    assert_eq!(reg.registered.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["side-project"]);
    let own = soup_dir(&a.env).unwrap().join("repos.json");
    assert!(!own.exists() && !own.with_extension("json.bak").exists(), "nothing is kept per cauldron, not even for a moment");
    // A face already open on dev sees it on its next tick.
    dev.sync();
    assert!(dev.repos.find_repo("side-project").is_some(), "the dev face already open sees it");
    for name in ["dev", "scratch1"] {
        let b = open_in(&root, name);
        assert!(b.repos.find_repo("side-project").is_some(), "{name} sees it");
        // The seed's repos are every cauldron's too.
        for r in ["harness", "cauldron", "broomstick", "orchard"] {
            assert!(b.repos.find_repo(r).is_some(), "{name} sees {r}");
        }
        assert_eq!(b.repos.repos.len(), a.repos.repos.len(), "{name}: one registry");
    }
    // Shown on Devices › Repos of another cauldron.
    let mut b = open_in(&root, "scratch1");
    config(&mut b, "devices/repos");
    let s = render_to_text(&mut b, W, H).unwrap().join("\n");
    assert!(s.lines().any(|l| l.contains("side-project") && l.contains("/tmp/somewhere/Side_Project")), "{s}");
    // And not on the cauldron's own tab any more.
    config(&mut b, "cauldron");
    let s = render_to_text(&mut b, W, H).unwrap().join("\n");
    assert!(!s.contains("▸ Repos"), "{s}");
}

/// A cauldron still CLAIMS repos, on its clusters, as before: the registry is
/// shared, the claims are not. And a repo another cauldron claims cannot be
/// unregistered from this one — the registry is shared, so that would take
/// it from the other.
#[test]
fn c2_claims_stay_a_cauldrons_and_a_repo_claimed_elsewhere_stays_registered() {
    let root = scratch_root();
    let prod = open_in(&root, "prod");
    let h = prod.repos.find_repo("harness").unwrap();
    assert!(!prod.repos.claimants[h].is_empty(), "prod's clusters claim harness");
    let mut b = open_in(&root, "scratch1");
    let hb = b.repos.find_repo("harness").unwrap();
    assert!(b.repos.claimants[hb].is_empty(), "scratch1's clusters do not");
    assert!(b.repos.unclaimed().contains(&hb));
    config(&mut b, "devices/repos");
    select_row(&mut b, |e| matches!(e, Entry::Sub { page: SPage::Repo(n), .. } if n == "harness"));
    b.handle(key(KeyCode::Right));
    select_do(&mut b, "unregister");
    b.handle(key(KeyCode::Enter));
    b.handle(key(KeyCode::Enter));
    assert!(matches!(&b.modal, Some(Modal::Confirm { error: Some(e), .. }) if e.contains("the prod cauldron claims it")), "{:?}", b.modal);
    b.handle(key(KeyCode::Esc));
    assert!(b.repos.find_repo("harness").is_some());
    // One nobody claims goes, for every cauldron.
    register(&mut b, "/tmp/nobodys");
    config(&mut b, "devices/repos");
    select_row(&mut b, |e| matches!(e, Entry::Sub { page: SPage::Repo(n), .. } if n == "nobodys"));
    b.handle(key(KeyCode::Right));
    select_do(&mut b, "unregister");
    b.handle(key(KeyCode::Enter));
    b.handle(key(KeyCode::Enter));
    assert!(b.repos.find_repo("nobodys").is_none(), "{:?}", b.modal);
    assert!(open_in(&root, "prod").repos.find_repo("nobodys").is_none());
}

fn legacy(name: &str, path: &str) -> Registered {
    Registered { name: name.into(), path: path.into(), registered_at: "2026-10-01T00:00:00Z".into() }
}

fn write_legacy(root: &Path, env: &str, f: &RegistryFile) -> PathBuf {
    let dir = soup_dir(&env_in(root, env)).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("repos.json");
    std::fs::write(&p, serde_json::to_string_pretty(f).unwrap()).unwrap();
    p
}

/// A cauldron's own registry from before is adopted into the global one AT
/// THE SOURCE, once: what it registered joins it, a name the global registry
/// already holds at another path stays as it is, an unregistration of a repo
/// another cauldron claims is not carried, one nobody claims is; its file is
/// kept as `repos.json.bak`, and it is said once.
#[test]
fn c2_a_cauldrons_own_registry_is_adopted_into_the_global_one_with_a_bak() {
    let root = scratch_root();
    drop(open_in(&root, "prod"));
    let own = RegistryFile {
        registered: vec![legacy("side", "/tmp/side"), legacy("harness", "/elsewhere/harness")],
        unregistered: vec!["harness".into(), "broomstick".into()],
    };
    // broomstick: does prod claim it? Unregistering it is carried only if not.
    let prod_claims_broomstick = {
        let p = open_in(&root, "prod");
        let r = p.repos.find_repo("broomstick").unwrap();
        !p.repos.claimants[r].is_empty()
    };
    let path = write_legacy(&root, "scratch1", &own);
    let text = std::fs::read_to_string(&path).unwrap();
    let a = open_in(&root, "scratch1");
    assert!(!path.exists(), "the cauldron's own file is gone");
    assert_eq!(std::fs::read_to_string(path.with_extension("json.bak")).unwrap(), text, "kept, verbatim, as .bak");
    let reg = Registry::global(&global_dir(&a.env)).read().unwrap();
    assert!(reg.registered.iter().any(|r| r.name == "side" && r.path == "/tmp/side"));
    assert!(!reg.registered.iter().any(|r| r.name == "harness"), "the seed's harness stands");
    assert!(!reg.unregistered.contains(&"harness".to_string()), "prod claims harness: not unregistered");
    assert_eq!(reg.unregistered.contains(&"broomstick".to_string()), !prod_claims_broomstick);
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("the scratch1 cauldron's repos are global now") && said.contains("registered side") && said.contains("not carried") && said.contains("`harness` at /elsewhere/harness"), "{said}");
    // Every cauldron sees what it registered; harness is still the seed's.
    let p = open_in(&root, "prod");
    let h = p.repos.find_repo("harness").unwrap();
    assert!(p.repos.find_repo("side").is_some() && p.repos.repos[h].path == "/Users/demo/code/harness");
    // Once: the next open adopts nothing and says nothing of it.
    let again = open_in(&root, "scratch1");
    assert!(!again.notice.clone().unwrap_or_default().contains("repos are global"), "{:?}", again.notice);
    // A second legacy file (written by an old binary) is adopted too, its
    // .bak beside the first rather than over it.
    write_legacy(&root, "scratch1", &RegistryFile { registered: vec![legacy("late", "/tmp/late")], unregistered: vec![] });
    let b = open_in(&root, "scratch1");
    assert!(b.repos.find_repo("late").is_some());
    assert!(path.with_extension("json.bak.2").exists() && path.with_extension("json.bak").exists());
}

/// `--dump` writes nothing: a cauldron's own registry is read into what it
/// shows, and left where it is.
#[test]
fn c2_dump_adopts_in_memory_only() {
    let root = scratch_root();
    let path = write_legacy(&root, "scratch1", &RegistryFile { registered: vec![legacy("side", "/tmp/side")], unregistered: vec![] });
    let a = open_with(&root, "scratch1", false);
    assert!(a.repos.find_repo("side").is_some());
    assert!(path.exists() && !path.with_extension("json.bak").exists());
    assert!(!Registry::global(&global_dir(&a.env)).path.exists());
    assert!(!a.notice.clone().unwrap_or_default().contains("repos are global"), "nothing adopted, nothing said: {:?}", a.notice);
}
