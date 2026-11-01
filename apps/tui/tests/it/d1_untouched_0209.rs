//! Task 0209 D1 (8e4fca10 msg 235 audit, 0200 D1): opening a cauldron writes
//! NOTHING into his settings files. The seed's FAKE inference endpoints are
//! READ beneath his files, never copied into them; no role is rebound to one;
//! his retired `[pools.openrouter]` keys are never deleted. Hermetic: a
//! scratch root holding his CURRENT file shapes (embedded below, and his real
//! files' text when they exist — read, never opened as a root).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use tui::cauldron::app::App;
use tui::cauldron::menu::Bound;
use tui::cauldron::render_to_text;
use tui::cauldron::settings::{Ctx, Val};
use tui::cauldron::store::seed_dir;

/// His global file as it stood on 2026-10-26 (before any endpoint build).
const HIS_GLOBAL: &str = "[pools.claude_code]\nnew_accounts_enabled = false\n\n[pools.claude_code.accounts.acct2]\nshare = 30\n\n[pools.claude_code.accounts.acct3]\nenabled = false\nnote = \"out of the pool, 3d ago · card expired\"\nshare = 20\n\n[pools.claude_code.accounts.sam-broomstick]\nenabled = true\nmax_concurrent = -1\nshare = 50\n\n[pools.claude_code.accounts.sam-broomstick.budget.session]\nthreshold_pct = \"off\"\n\n[pools.claude_code.budget.session]\nthreshold_pct = \"off\"\n\n[pools.claude_code.opportunistic]\nenabled = false\n\n[pools.openrouter]\nmodels = [\"jev\"]\nprovider = \"openrouter\"\n";
/// His prod file, the same day.
const HIS_PROD: &str = "[cauldron]\nenabled = true\n\n[identity]\nlive_login = \"claude_code/sam-broomstick\"\n\n[supervisor]\nauto_restart = true\n";

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0209-{}-{}-{}",
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
    // V1.6: Claude Code's accounts are the daemon's; the fixture stands in.
    crate::live_pool::seeded(App::open(env_in(root, "prod"), seed_dir(), true).expect("opens"))
}

/// A scratch root holding `global` and `prod` where his live in his root.
fn root_with(global: &str, prod: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch_root();
    let g = root.join("data/shared/tui/settings.toml");
    let c = root.join("data/prod/tui/soup/settings.toml");
    for (p, t) in [(&g, global), (&c, prod)] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, t).unwrap();
    }
    (root, g, c)
}

/// His file shapes: the embedded copies of his files.
fn shapes() -> Vec<(String, String)> {
    // Never his live files (0225 F4): the embedded copies only.
    vec![(HIS_GLOBAL.to_string(), HIS_PROD.to_string())]
}

#[test]
fn d1_opening_a_cauldron_leaves_his_settings_files_byte_identical() {
    for (global, prod) in shapes() {
        let (root, g, c) = root_with(&global, &prod);
        let mut a = open(&root);
        // Walk the faces that read the endpoints, as he would.
        for _ in 0..2 {
            render_to_text(&mut a, 120, 60).unwrap();
        }
        drop(a);
        let b = open(&root);
        drop(b);
        assert_eq!(std::fs::read_to_string(&g).unwrap(), global, "the global file was written by an open");
        assert_eq!(std::fs::read_to_string(&c).unwrap(), prod, "prod's file was written by an open");
    }
}

/// The fake endpoints are still SHOWN — read from the seed beneath his file —
/// and a role is not bound to one in his cauldron.
#[test]
fn d1_the_seed_endpoints_are_read_not_written_and_no_role_is_rebound() {
    let (root, _, _) = root_with(HIS_GLOBAL, HIS_PROD);
    let a = open(&root);
    assert_eq!(a.settings.endpoint_ids(false), ["ep-1", "ep-2", "ep-3", "ep-4", "ep-5"]);
    assert_eq!(a.settings.endpoint_ids(true), ["ep-6"]);
    assert!(!a.sis_set(&Bound::new("roles.{role}.endpoint", Ctx::role("type_one.secondary"))), "no role bound to a fake endpoint in his cauldron");
}

/// A change he makes writes only what he changed: his retired openrouter
/// keys stay, the held global keys move to the global file before prod's
/// file drops them, and no fake endpoint is written along with it.
#[test]
fn d1_a_change_keeps_his_openrouter_keys_and_moves_held_keys_without_loss() {
    let (root, g, c) = root_with(HIS_GLOBAL, HIS_PROD);
    let mut a = open(&root);
    a.sset(&Bound::new("cauldron.opportunistic", Ctx::default()), Val::Bool(false)).unwrap();
    let gt = std::fs::read_to_string(&g).unwrap();
    let ct = std::fs::read_to_string(&c).unwrap();
    assert!(gt.contains("[pools.openrouter]") && gt.contains("models = [\"jev\"]") && gt.contains("provider = \"openrouter\""), "{gt}");
    assert!(gt.contains("live_login = \"claude_code/sam-broomstick\"") && gt.contains("auto_restart = true"), "held keys moved, not lost: {gt}");
    assert!(!gt.contains("endpoints") && !gt.contains("FAKE"), "no fake endpoint written: {gt}");
    assert!(!ct.contains("endpoints") && !ct.contains("roles") && !ct.contains("cauldron.use") && ct.contains("opportunistic = false"), "{ct}");
    // Removing a fake endpoint records only its removal; a new one mints
    // past the seed's ids.
    a.settings.remove_endpoint(false, "ep-2").unwrap();
    let gt = std::fs::read_to_string(&g).unwrap();
    assert!(gt.contains("[endpoints.ep-2]\nremoved = true") && !gt.contains("FAKE"), "{gt}");
    let b = open(&root);
    assert_eq!(b.settings.endpoint_ids(false), ["ep-1", "ep-3", "ep-4", "ep-5"]);
}

/// D2: his old per-cluster bucket's `> [cauldron-ripples/Q-1]` comes into
/// the one bucket as the SAME question's soup id — Q-15, not Q-1 (which is
/// another cluster's) — and an unknown old reference is left as it was.
#[test]
fn d2_adopted_old_references_are_rewritten_to_their_soup_ids() {
    let root = scratch_root();
    let dir = root.join("data/prod/tui/soup");
    std::fs::create_dir_all(dir.join("drafts")).unwrap();
    let draft = |t: &str| format!("{{\"nonce\":\"n\",\"writer\":\"w\",\"updated_at\":\"2026-10-25T05:41:44+00:00\",\"text\":{}}}", serde_json::to_string(t).unwrap());
    std::fs::write(dir.join("drafts/cauldron-ripples.json"), draft("> [cauldron-ripples/Q-1]")).unwrap();
    std::fs::write(dir.join("drafts/cauldron.json"), draft("see [cauldron/Q-2.opt1], [surface:broomstick-platform], [broomstick-platform/HA-1.step2] and [nowhere/Q-9]")).unwrap();
    let a = open(&root);
    let t = a.composer.text();
    assert!(t.contains("> [Q-15]") && !t.contains("cauldron-ripples/"), "{t}");
    assert!(t.contains("see [Q-2.opt1], [X-8], [HA-5.step2] and [nowhere/Q-9]"), "{t}");
    assert!(a.notice.as_deref().is_none_or(|n| !n.contains("in the bucket now")), "no caption: {:?}", a.notice);
    assert_eq!(tui::cauldron::store::rewrite_legacy_refs("[harness-workspaces/Q-4]"), "[Q-21]");
}

/// The held keys' move merges into the global file as it is on disk: a
/// value another face wrote since this one opened is kept.
#[test]
fn d1_the_deferred_move_merges_with_what_another_face_wrote() {
    let (root, g, c) = root_with(HIS_GLOBAL, HIS_PROD);
    let mut a = open(&root);
    let mut b = open(&root);
    b.sset(&Bound::new("machine.cpu_max_pct", Ctx::default()), Val::Int(61)).unwrap();
    a.sset(&Bound::new("cauldron.opportunistic", Ctx::default()), Val::Bool(false)).unwrap();
    let gt = std::fs::read_to_string(&g).unwrap();
    assert!(gt.contains("cpu_max_pct = 61") && gt.contains("live_login = \"claude_code/sam-broomstick\"") && gt.contains("[pools.openrouter]"), "{gt}");
    assert!(!std::fs::read_to_string(&c).unwrap().contains("live_login"));
}

// ── step 2: the rest of the 0200 audit's defects ────────────────────────────

fn config(a: &mut App, path: &str) {
    a.set_ctab(tui::cauldron::app::CTab::Settings);
    a.snav = tui::cauldron::menu::Nav::cauldron();
    a.rows[tui::cauldron::menu::SETTINGS_ROW] = 0;
    a.open_settings_path(path).unwrap_or_else(|e| panic!("{path}: {e}"));
}

/// D5: at 80 columns a table row keeps every option of its switches and
/// its `›`; a text column narrows instead.
#[test]
fn d5_a_table_row_keeps_its_switches_and_chevron_at_80_columns() {
    let mut a = open(&scratch_root());
    config(&mut a, "providers/claude_code");
    let s = render_to_text(&mut a, 80, 140).unwrap();
    // Its name narrows (`sam-b…`, beside V1.6's `in the pool`) so its
    // switches do not.
    let row = s.iter().find(|l| l.contains("sam-b") && l.contains('›')).unwrap_or_else(|| panic!("{}", s.join("\n")));
    assert_eq!(row.matches("inherit  on  off").count(), 2, "{row}");
    assert!(row.contains('›'), "{row}");
    let head = s.iter().find(|l| l.contains("on in new cauldrons") && l.contains("opportunistic")).unwrap();
    assert!(!head.contains("…o") && head.contains("opportunistic"), "a narrowed column name keeps its gap: {head}");
}

/// D6: Codex carries no Fable setting; Claude Code still does.
#[test]
fn d6_a_source_without_fable_has_no_fable_settings() {
    use tui::cauldron::menu::{Entry, SPage};
    let a = open(&scratch_root());
    let fable = |p: &SPage| a.page_entries(p).iter().any(|e| matches!(e, Entry::Set(b) if b.key.contains(".fable.")));
    assert!(!fable(&SPage::Pool("codex".into())), "Codex");
    assert!(fable(&SPage::Pool("claude_code".into())), "Claude Code");
    let acct = a.accounts("codex").into_iter().next().map(|x| x.id);
    if let Some(id) = acct {
        assert!(!fable(&SPage::Account("codex".into(), id)), "a Codex account");
    }
}

/// D7: an Overview card starts two columns in from its band's title.
#[test]
fn d7_overview_cards_sit_two_columns_in_from_the_band_title() {
    let mut a = open(&scratch_root());
    a.set_ktab(tui::cauldron::app::KTab::Details);
    let s = render_to_text(&mut a, 140, 120).unwrap();
    let (y, l) = s.iter().enumerate().find(|(_, l)| l.contains("• Directives")).unwrap_or_else(|| panic!("{}", s.join("\n")));
    let title = l[..l.find("Directives").unwrap()].chars().count();
    let card = &s[y + 1];
    let corner = card.chars().enumerate().skip(title.saturating_sub(4)).find(|(_, c)| *c == '╭').map(|(i, _)| i).unwrap_or_else(|| panic!("{card}"));
    assert_eq!(corner, title + 2, "\n{l}\n{card}");
}

/// D10: an account's modal leaves no ⓘ of the page beside it, at any
/// width (it showed from 118 columns up).
#[test]
fn d10_a_row_modal_covers_the_page_beside_it() {
    let mut a = open(&scratch_root());
    config(&mut a, "providers/claude_code/acct2");
    for w in [80u16, 110, 118, 120, 140] {
        let s = render_to_text(&mut a, w, 40).unwrap();
        assert!(s.iter().any(|l| l.contains('┃')), "the modal is up at {w}");
        for l in s.iter().filter(|l| l.contains('┃')) {
            let after = &l[l.rfind('┃').unwrap() + '┃'.len_utf8()..];
            assert!(!after.contains('ⓘ'), "{w}: {l}");
        }
    }
}

/// D11: a choice is said in words.
#[test]
fn d11_a_choice_value_is_said_in_words() {
    use tui::cauldron::settings::{def, show};
    assert_eq!(show(def("attention.autodecide"), &Val::Str("three_ways_floor".into())), "three ways floor");
    assert_eq!(show(def("storage.threads.eviction_order"), &Val::Str("lru".into())), "least recently used");
    assert_eq!(show(def("pools.{pool}.selection"), &Val::Str("min_risk".into())), "min risk");
}

/// D16: a directive and a constraint are bare soup items, like a question;
/// the old `[node:DIR-1]` still reads.
#[test]
fn d16_directives_and_constraints_are_bare_items() {
    use tui::cauldron::tags::{scan, Tag};
    let found: Vec<Tag> = scan("[DIR-1] [CON-2] [node:DIR-3]").into_iter().map(|(_, t)| t).collect();
    assert_eq!(found, [Tag::item("DIR-1"), Tag::item("CON-2"), Tag::thing("node", "DIR-3")]);
}

/// D19: after Status' `config ▸` opens a row's modal, F1 goes to Surface.
#[test]
fn d19_an_f_key_leaves_a_rows_modal_for_its_tab() {
    use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut a = open(&scratch_root());
    a.open_endpoint_settings(false, "ep-1");
    assert!(a.modal.is_some());
    a.handle(Event::Key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)));
    assert!(a.modal.is_none());
    assert_eq!(a.ctab, tui::cauldron::app::CTab::Cauldron);
}
