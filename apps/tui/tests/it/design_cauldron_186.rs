//! The DESIGN cauldron (8e4fca10 msg 186, task 0168, checklist D1 D2 D4): "use
//! a cauldron to represent all these questions to me ... make a new cauldron
//! environment and just use it specifically for designing this ... you're
//! hand curating the data". A cauldron may carry its OWN seed in its own data
//! root (`<data_root>/tui/soup/seed/`): clusters, artifacts and ripple policy,
//! each winning over the repo's fixture of the same kind. Its question cards
//! carry inserted artifacts, real files a click opens. Hermetic (a scratch
//! root, CAULDRON_TYPE_ONE=off) except the last test, which READS the real
//! design cauldron on this machine when it exists and writes nothing there.

use crate::own_seed_fixture;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tui::cauldron::app::{App, CTab, Focus, Item, Modal, Outbox, KTab};
use tui::cauldron::model::QStatus;
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::seed_dir;
use tui::cauldron::render_to_text;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "design186-{}-{}-{}",
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

fn open(root: &Path, name: &str, persist: bool) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    crate::live_pool::seeded(App::open(env_in(root, name), seed_dir(), persist).expect("seed loads"))
}

/// The fixture design cauldron, named `design`, in a fresh root.
fn design() -> (App, PathBuf) {
    let root = scratch_root();
    own_seed_fixture::write(&root, "design");
    (open(&root, "design", false), root)
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 70).unwrap().join("\n")
}

fn tasks_of(a: &mut App, key: &str) {
    let g = a.forest.find(key).unwrap_or_else(|| panic!("no cluster {key}"));
    a.set_ctab(CTab::Cauldron);
    a.select(g);
    a.set_ktab(KTab::Questions);
}

// ── D1: a cauldron of its own, with its own data ───────────────────────────

/// D1: the own seed is the cauldron's soup, and it wins over the repo's: a
/// `prod` that carries one reads it instead of data/clusters.json, while
/// another cauldron of the same root that carries none still reads its repo
/// seed (dev's Sandbox).
#[test]
fn d1_a_cauldrons_own_seed_wins_over_the_repos() {
    let (a, root) = design();
    assert_eq!(a.forest.get(0).name, "Design");
    assert!(a.forest.find("X-2").is_some(), "the System cluster, minted X-2");
    assert!(a.forest.find("X-3").is_none(), "no repo fixture leaked in");
    own_seed_fixture::write(&root, "prod");
    let p = open(&root, "prod", false);
    assert_eq!(p.forest.get(0).name, "Design", "an own seed wins over the repo's prod seed too");
    let d = open(&root, "dev", false);
    assert_eq!(d.forest.get(0).name, "Sandbox", "a cauldron without one reads the repo's");
}

/// D1: "just have a couple of services, not too many": the services are the
/// cauldron's own, and Status › Mesh lists exactly them, under its device.
#[test]
fn d1_its_services_are_its_own() {
    let (mut a, _r) = design();
    a.set_ctab(CTab::Status);
    a.sttab = StatusTab::Mesh;
    let s = screen(&mut a);
    assert!(s.contains("▶ 2 of 2 services running"), "{s}");
    for name in ["cauldron daemon", "threads"] {
        assert!(s.lines().any(|l| l.contains(&format!("▸ {name}"))), "{name}:\n{s}");
    }
    for prod_only in ["tts", "tally", "echo"] {
        assert!(!s.lines().any(|l| l.contains(&format!("▸ {prod_only} "))), "{prod_only} is prod's:\n{s}");
    }
}

/// D1: its artifacts are its own (`seed/artifacts.json`): the repo's
/// (ART-126 and its worktrees) are not in it.
#[test]
fn d1_its_artifacts_are_its_own() {
    let (a, _r) = design();
    let ids: Vec<&str> = a.arts.artifacts.iter().map(|x| x.id.as_str()).collect();
    assert_eq!(ids, ["ART-1"]);
    assert!(a.arts.worktrees.is_empty());
    assert_eq!(a.arts.directories.len(), 1);
}

/// D1: its ripple policy may be its own too: with the repo's catalog and no
/// runs, nothing is running and no ripple ran today (Status › Bus).
#[test]
fn d1_its_ripple_policy_may_be_its_own() {
    let root = scratch_root();
    own_seed_fixture::write(&root, "design");
    let mut before = open(&root, "design", false);
    before.set_ctab(CTab::Status);
    before.sttab = StatusTab::Bus;
    assert!(screen(&mut before).contains("▶ 1 running"), "the repo's fixture run, before");
    own_seed_fixture::write_quiet_ripples(&root, "design", &seed_dir());
    let mut a = open(&root, "design", false);
    a.set_ctab(CTab::Status);
    a.sttab = StatusTab::Bus;
    let s = screen(&mut a);
    assert!(s.contains("▶ 0 running"), "{s}");
    assert!(s.contains("0 runs today"), "{s}");
}

/// D1: the own seed is live — an edit to it (a new question, a changed
/// artifact) reaches a face already open on the cauldron at its next quiet
/// tick, keeping where he is.
#[test]
fn d1_an_edit_to_the_own_seed_reaches_an_open_face() {
    let root = scratch_root();
    own_seed_fixture::write(&root, "design");
    let mut a = open(&root, "design", true);
    tasks_of(&mut a, "X-2");
    assert_eq!(a.forest.get(a.selected).questions.len(), 2);
    let p = own_seed_fixture::seed_dir(&root, "design").join("clusters.json");
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    v["clusters"][0]["children"][0]["questions"].as_array_mut().unwrap().push(serde_json::json!({
        "serial": 9, "value": 10, "question": "A question added while the face was open?", "why": "w", "options": ["YES."], "recommendation": "Yes."
    }));
    std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let a_path = own_seed_fixture::seed_dir(&root, "design").join("artifacts.json");
    let arts = std::fs::read_to_string(&a_path).unwrap().replace("System root, scheme C", "System root, scheme C, revised");
    std::fs::write(&a_path, arts).unwrap();
    a.sync();
    assert_eq!(a.forest.get(a.selected).key, "X-2", "kept where he is");
    assert_eq!(a.forest.get(a.selected).questions.len(), 3);
    assert!(screen(&mut a).contains("A question added while the face was open?"));
    assert_eq!(a.arts.artifacts[0].handle, "System root, scheme C, revised");
}

// ── D2 / D4: the questions, as cards, with the design attached ─────────────

/// D2: a question is a card with his question, its Context, its Options and
/// its Recommendation; an assumed one sits in the Assumed band.
#[test]
fn d2_the_questions_are_cards_and_the_assumed_band_holds_the_assumptions() {
    let (mut a, _r) = design();
    tasks_of(&mut a, "X-2");
    let s = screen(&mut a);
    for part in ["How should the System tab's top level be organized?", "Context", "Options", "Recommendation", "◆ Q-1"] {
        assert!(s.contains(part), "{part}:\n{s}");
    }
    assert!(s.contains("▸ Assumed") || s.contains("▾ Assumed"), "{s}");
    let g = a.selected;
    assert!(a.forest.get(g).closed_questions.iter().any(|c| c.status == QStatus::Assumed));
}

/// D4: the System question carries the design artifacts inside its card —
/// by id (the soup node wrapping the file) and by path — and Enter opens a
/// mockup's text in the artifact modal, whole, wide lines included; `o`
/// hands the file to `open`.
#[test]
fn d4_the_system_question_carries_its_mockups_and_one_opens() {
    let (mut a, _r) = design();
    tasks_of(&mut a, "X-2");
    let s = screen(&mut a);
    let cards: Vec<&str> = s.lines().filter(|l| l.contains("┌─ ▤ ") && l.contains("open ↗")).collect();
    assert_eq!(cards.len(), 2, "{s}");
    assert!(cards[0].contains("system-root-C") && cards[1].contains("system-flow-2"), "{cards:?}");
    assert!(s.contains("ART-1 · "), "the id card names its soup node:\n{s}");
    let arts: Vec<usize> = a.items().iter().enumerate().filter(|(_, i)| matches!(i, Item::QArt { .. })).map(|(n, _)| n).collect();
    assert_eq!(arts.len(), 2);
    a.focus = Focus::Content;
    a.item = arts[0];
    a.handle(key(KeyCode::Enter));
    match &a.modal {
        Some(Modal::Artifact { lines, .. }) => {
            assert_eq!(lines[0], "SYSTEM ROOT, SCHEME C");
            assert!(lines.iter().any(|l| l == own_seed_fixture::WIDE), "the wide mockup line is whole: {lines:?}");
        }
        m => panic!("{m:?}"),
    }
    let s = screen(&mut a);
    assert!(s.contains('┏') && s.contains("SYSTEM ROOT, SCHEME C"), "{s}");
    // The modal is as wide as the mockup: at 130 columns its 120-column line
    // is whole; at 100 it crops, because the screen cannot hold it.
    let wide = render_to_text(&mut a, 130, 60).unwrap().join("\n");
    assert!(wide.contains(own_seed_fixture::WIDE), "{wide}");
    let narrow = render_to_text(&mut a, 100, 60).unwrap().join("\n");
    assert!(!narrow.contains(own_seed_fixture::WIDE) && narrow.contains("SYSTEM ROOT, SCHEME C"), "{narrow}");
    a.handle(key(KeyCode::Char('o')));
    assert!(a.modal.is_none());
    assert!(matches!(a.outbox.last(), Some(Outbox::Open(p)) if p.ends_with("system-root-C.txt")), "{:?}", a.outbox);
}

/// msg 165's invariant on the design cauldron: selecting any card, option or
/// inserted artifact never changes a line of the page, and scrolling never
/// reshapes a row.
#[test]
fn selection_and_scrolling_never_change_the_design_cauldrons_pages() {
    let (mut a, _r) = design();
    for key in ["X-1", "X-2"] {
        let g = a.forest.find(key).unwrap();
        a.select(g);
        for t in [KTab::Details, KTab::Questions, KTab::Actions, KTab::Nodes] {
            a.set_ktab(t);
            let lines = |a: &App| tui::cauldron::pages::page(a, 100).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
            let first = lines(&a);
            for i in 0..a.items().len() {
                a.item = i;
                assert_eq!(lines(&a), first, "{key} {t:?}: selecting item {i} changed the page");
            }
            for k in [1, 5, 17] {
                a.scroll = k;
                assert_eq!(lines(&a), first, "{key} {t:?}: scrolled to {k}, the page changed");
            }
            a.scroll = 0;
        }
    }
}

// ── the real design cauldron, read where it lives ─────────────────────────

/// The design cauldron on THIS machine (`~/.cauldron/data/design`, made by
/// task 0168), copied into a scratch root and read: every open question is a
/// whole card (question, context, options, recommendation); Bus's three
/// carry the design, and every attached artifact is a real file whose bytes
/// match what its node says. Skips, saying so, where it has not been made.
#[test]
fn the_design_cauldron_on_this_machine_is_whole() {
    let Some(real) = own_seed_fixture::real_design_seed() else {
        eprintln!("skipped: no design cauldron on this machine");
        return;
    };
    let root = scratch_root();
    own_seed_fixture::copy_real_design_seed(&real, &root, "design");
    let mut a = open(&root, "design", false);
    let mut open_questions = 0;
    for g in 0..a.forest.clusters.len() {
        for q in &a.forest.get(g).questions {
            open_questions += 1;
            assert!(!q.why.trim().is_empty() && !q.recommendation.trim().is_empty() && q.options.len() >= 2, "{} Q-{} is not a whole card", a.forest.get(g).key, q.serial);
            for art in &q.artifacts {
                let path = match a.arts.artifacts.iter().find(|x| &x.id == art) {
                    Some(x) => {
                        let bytes = std::fs::read(&x.location).unwrap_or_else(|e| panic!("{art}: {}: {e}", x.location));
                        if let Some(v) = &x.verified {
                            assert_eq!(bytes.len() as u64, v.bytes, "{art} changed since it was verified");
                        }
                        x.location.clone()
                    }
                    None => art.clone(),
                };
                assert!(Path::new(&path).is_file(), "{art}: {path}");
            }
        }
    }
    assert!(open_questions >= 15, "{open_questions}");
    let system = a.forest.clusters.iter().position(|g| g.name.starts_with("Bus")).expect("the Bus cluster (System before msg 235)");
    let qs = &a.forest.get(system).questions;
    for serial in [1, 2, 3] {
        let q = qs.iter().find(|q| q.serial == serial).unwrap();
        assert!(q.blocking && !q.artifacts.is_empty(), "System Q-{serial} carries the design");
    }
    let key = a.forest.get(system).key.clone();
    tasks_of(&mut a, &key);
    let s = screen(&mut a);
    // 0223 B9: the design files are named for the Bus, not the System tab.
    assert!(s.contains("bus-root-C-entries-library-execution") && !s.contains("system-root"), "{s}");
}
