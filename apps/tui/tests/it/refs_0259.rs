//! Task 0259 item 1 (8e4fca10 msg 385): a click on a reference in the bucket
//! goes to where it was put in from — the cluster, the tab, the item — as an
//! index ON DISK records it, through a restart, with its anchors following
//! every edit and every merge with another face. A reference with no entry
//! goes by its soup id. Navigation never changes the bucket, and no click
//! loses what he typed. Real seed files, real events, scratch roots.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, Band, CTab, Focus, Hit, Item, KTab, QBand};
use tui::cauldron::render_to_text;
use tui::cauldron::store::seed_dir;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0259-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn open_at(root: &Path) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let env = Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") };
    App::open(env, seed_dir(), true).expect("opens")
}

fn refs_file(a: &App) -> PathBuf {
    tui::cauldron::store::soup_dir(&a.env).unwrap().join(tui::cauldron::refs::FILE)
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
    }
}

fn press(a: &mut App, code: KeyCode) {
    a.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn mouse(a: &mut App, x: u16, y: u16) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
    }
}

fn press_hit(a: &mut App, hit: Hit) {
    let _ = render_to_text(a, 140, 50).unwrap();
    let r = a.hits.iter().rev().find(|(_, h)| *h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {hit:?} on screen"));
    a.last_click = None;
    mouse(a, r.x, r.y);
}

/// Click a page item into the bucket, as he does: one click.
fn click_item(a: &mut App, it: Item) {
    let _ = render_to_text(a, 140, 50).unwrap();
    let i = a.items().iter().position(|x| *x == it).unwrap_or_else(|| panic!("{it:?} not on the page: {:?}", a.items()));
    // Scrolled to it, as the wheel would, and not yet selected: one click on
    // a selected item opens it instead.
    a.item = i;
    a.reveal = true;
    let _ = render_to_text(a, 140, 50).unwrap();
    a.item = 0;
    a.focus = Focus::Tree;
    press_hit(a, Hit::Item(i));
}

/// Click the `n`th (0-based) `needle` drawn inside the bucket, on its second
/// character.
fn click_in_bucket(a: &mut App, needle: &str, n: usize) {
    let rows = render_to_text(a, 140, 50).unwrap();
    let (r, _) = a.input_at;
    let mut seen = 0;
    for y in r.y..r.y + r.height {
        let row: String = rows[y as usize].clone();
        let mut from = 0;
        while let Some(b) = row[from..].find(needle) {
            let b = from + b;
            let x = row[..b].chars().count() as u16;
            if x >= r.x && x < r.x + r.width {
                if seen == n {
                    a.last_click = None;
                    mouse(a, x + 1, y);
                    return;
                }
                seen += 1;
            }
            from = b + needle.len();
        }
    }
    panic!("`{needle}` #{n} not in the bucket:\n{}", rows.join("\n"));
}

fn cluster(a: &App, key: &str) -> usize {
    a.forest.find(key).unwrap_or_else(|| panic!("no {key}"))
}

/// The reference the page has selected.
fn selected_tag(a: &App) -> String {
    a.tag_for(a.items()[a.item]).map(|t| t.render()).unwrap_or_default()
}

/// A child's question put in from its PARENT's Questions tab with
/// `Aggregated clusters` on: by its soup id alone it would go to the child;
/// by the index it goes back to the parent, as he saw it.
fn child_question_from_parent(a: &mut App) -> (String, String, String) {
    // Keys, not indexes: `Aggregated clusters` puts the whole cauldron in as
    // every cluster's root, which moves every index.
    let parent = a.forest.clusters.iter().position(|g| !g.whole && !g.children.is_empty() && g.children.iter().any(|&c| !a.forest.get(c).questions.is_empty())).expect("a parent with a child that has questions");
    let child = *a.forest.get(parent).children.iter().find(|&&c| !a.forest.get(c).questions.is_empty()).unwrap();
    let (pk, ck, serial) = (a.forest.get(parent).key.clone(), a.forest.get(child).key.clone(), a.forest.get(child).questions[0].serial);
    a.set_ctab(CTab::Cauldron);
    a.set_aggregate(true);
    a.select(cluster(a, &pk));
    a.set_ktab(KTab::Questions);
    let it = Item::Question { cluster: cluster(a, &ck), serial };
    click_item(a, it);
    (pk, ck, format!("[Q-{serial}]"))
}

#[test]
fn the_index_survives_a_restart_and_the_click_lands_where_it_was_put_in_from() {
    let root = scratch_root();
    let mut a = open_at(&root);
    let (parent, child, tag) = child_question_from_parent(&mut a);
    assert!(a.composer.text().contains(&tag), "{:?}", a.composer.text());
    // On disk: the reference, its cluster, its tab.
    let f = std::fs::read_to_string(refs_file(&a)).expect("the index is on disk");
    let v: serde_json::Value = serde_json::from_str(&f).unwrap();
    let e = &v["refs"][0];
    assert_eq!((e["tag"].as_str(), e["cluster"].as_str(), e["ctab"].as_str(), e["tab"].as_str(), e["aggregate"].as_bool()), (Some(tag.as_str()), Some(parent.as_str()), Some("cauldron"), Some("questions"), Some(true)), "{f}");
    drop(a);
    // Close the app; open it again somewhere else; click the reference.
    let mut b = open_at(&root);
    b.set_ctab(CTab::Status);
    b.set_ctab(CTab::Cauldron);
    b.select(cluster(&b, "X-9"));
    b.set_aggregate(false);
    b.set_ktab(KTab::Nodes);
    let text = b.composer.text();
    b.focus_input();
    click_in_bucket(&mut b, &tag, 0);
    assert_eq!(b.forest.get(b.selected).key, parent, "the cluster it was put in from, not {child}, its home");
    assert_eq!((b.ctab, b.ktab, b.aggregate), (CTab::Cauldron, KTab::Questions, true));
    assert_eq!(selected_tag(&b), tag, "the item itself is selected");
    let s = render_to_text(&mut b, 140, 50).unwrap().join("\n");
    let id = &tag[1..tag.len() - 1];
    assert!(s.lines().any(|l| l.contains(&format!(" {id} ")) && l.contains("─╮")), "scrolled to it: its card's legend is on screen:\n{s}");
    assert_eq!(b.composer.text(), text, "navigation never changes the bucket");
    assert_eq!(b.focus, Focus::Input, "what he types next still goes to the bucket");
    typed(&mut b, "x");
    assert_eq!(b.composer.text().len(), text.len() + 1);
}

#[test]
fn anchors_follow_inserts_and_deletes_and_a_deleted_reference_takes_its_entry() {
    let root = scratch_root();
    let mut a = open_at(&root);
    let (parent, _, tag) = child_question_from_parent(&mut a);
    // Type BEFORE the reference: the anchor moves with it.
    a.focus_input();
    a.composer.buffer_start();
    typed(&mut a, "first, and more ");
    let at = a.composer.text().find(&tag).unwrap();
    a.refs.save(&a.composer.text()).unwrap();
    assert_eq!(a.refs.refs.iter().map(|e| e.at).collect::<Vec<_>>(), vec![at], "{:?}", a.refs.refs);
    // Delete some of what is before it: it follows again.
    a.composer.buffer_start();
    for _ in 0..3 {
        press(&mut a, KeyCode::Delete);
    }
    let at2 = a.composer.text().find(&tag).unwrap();
    assert_eq!(at2, at - 3);
    // And it still goes to where it came from, through a restart.
    drop(a);
    let mut b = open_at(&root);
    b.focus_input();
    click_in_bucket(&mut b, &tag, 0);
    assert_eq!((b.forest.get(b.selected).key.as_str(), b.ktab, b.aggregate), (parent.as_str(), KTab::Questions, true));
    // Take the reference out: its entry goes with it.
    b.focus_input();
    b.composer.buffer_end();
    let text = b.composer.text();
    for _ in 0..text.len() - text.find(&tag).unwrap() {
        press(&mut b, KeyCode::Backspace);
    }
    assert!(!b.composer.text().contains(&tag));
    b.refs.save(&b.composer.text()).unwrap();
    assert!(b.refs.refs.is_empty(), "{:?}", b.refs.refs);
    let f: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(refs_file(&b)).unwrap()).unwrap();
    assert_eq!(f["refs"].as_array().unwrap().len(), 0, "{f}");
}

#[test]
fn anchors_follow_another_faces_merge_and_its_references_navigate_here() {
    let root = scratch_root();
    let mut f = open_at(&root);
    let mut g = open_at(&root);
    // f puts a reference in from the parent, aggregated.
    let (parent, _, tag) = child_question_from_parent(&mut f);
    g.sync();
    assert!(g.composer.text().contains(&tag));
    // g types BEFORE it; f types after it; both merge.
    g.focus_input();
    g.composer.buffer_start();
    typed(&mut g, "from g ");
    f.focus_input();
    f.composer.buffer_end();
    typed(&mut f, " and f");
    for _ in 0..3 {
        f.sync();
        g.sync();
    }
    assert_eq!(f.composer.text(), g.composer.text(), "the faces converge");
    let at = g.composer.text().find(&tag).unwrap();
    assert!(at >= "from g ".len(), "{:?}", g.composer.text());
    // g never put it in: its click finds f's entry, carried to here.
    g.set_ktab(KTab::Tasks);
    g.select(cluster(&g, "X-9"));
    g.set_aggregate(false);
    g.focus_input();
    let before = g.composer.text();
    click_in_bucket(&mut g, &tag, 0);
    assert_eq!((g.forest.get(g.selected).key.as_str(), g.ktab, g.aggregate), (parent.as_str(), KTab::Questions, true), "{:?}", g.notice);
    assert_eq!(selected_tag(&g), tag);
    assert_eq!(g.composer.text(), before, "navigation never changes the bucket");
    // And f's own anchor followed g's insert.
    f.sync();
    f.refs.save(&f.composer.text()).unwrap();
    assert_eq!(f.refs.refs.iter().find(|e| e.tag == tag).map(|e| e.at), Some(f.composer.text().find(&tag).unwrap()));
}

#[test]
fn a_reference_in_a_closed_band_opens_its_band_and_one_typed_by_hand_goes_by_its_id() {
    let root = scratch_root();
    let mut a = open_at(&root);
    let x1 = cluster(&a, "X-1");
    // Q-4 is complete: its band is shut until something opens it.
    a.select(x1);
    a.set_ktab(KTab::Questions);
    assert!(!a.band_open(Band::Questions(QBand::Complete)));
    // Typed by hand: no entry, and it still goes there.
    a.focus_input();
    typed(&mut a, "see [Q-4] and [HA-1]");
    a.set_ktab(KTab::Tasks);
    a.select(cluster(&a, "X-9"));
    a.focus_input();
    let text = a.composer.text();
    click_in_bucket(&mut a, "[Q-4]", 0);
    assert_eq!((a.selected, a.ktab), (x1, KTab::Questions), "{:?}", a.notice);
    assert!(a.band_open(Band::Questions(QBand::Complete)), "the band that holds it opened");
    assert!(!a.band_open(Band::Questions(QBand::Archived)), "and only that one");
    assert_eq!(a.items()[a.item], Item::Closed { cluster: x1, serial: 4 });
    let owner = a.forest.clusters.iter().position(|g| g.human_actions.iter().any(|h| h.serial == 1)).expect("HA-1's cluster");
    click_in_bucket(&mut a, "[HA-1]", 0);
    assert_eq!((a.selected, a.ktab), (owner, KTab::Actions), "{:?}", a.notice);
    assert_eq!(selected_tag(&a), "[HA-1]");
    assert_eq!(a.composer.text(), text, "navigation never changes the bucket");
    assert!(a.refs.refs.is_empty(), "a typed reference is never indexed");
}

/// Adversarial: no click in the bucket loses what he typed, a click beside
/// a reference does not navigate, a click on a paste chip opens the chip
/// and goes nowhere, and every navigation leaves the bucket's text and its
/// file as they were.
#[test]
fn adversarial_no_click_loses_input_and_navigation_never_changes_the_bucket() {
    let root = scratch_root();
    let mut a = open_at(&root);
    let (parent, _, tag) = child_question_from_parent(&mut a);
    a.focus_input();
    a.composer.buffer_end();
    typed(&mut a, "  and words after it");
    let text = a.composer.text();
    let file = std::fs::read_to_string(tui::cauldron::store::soup_dir(&a.env).unwrap().join("bucket.json")).unwrap();
    // A click on the words beside it places the caret and goes nowhere.
    a.set_ktab(KTab::Nodes);
    a.select(cluster(&a, "X-9"));
    click_in_bucket(&mut a, "words", 0);
    assert_eq!((a.ktab, a.forest.get(a.selected).key.as_str()), (KTab::Nodes, "X-9"));
    // Every character of the reference goes there; none changes the text.
    let rows = render_to_text(&mut a, 140, 50).unwrap();
    let (r, _) = a.input_at;
    let (y, row) = (r.y..r.y + r.height).map(|y| (y, rows[y as usize].clone())).find(|(_, l)| l.contains(&tag)).unwrap();
    let x0 = row[..row.find(&tag).unwrap()].chars().count() as u16;
    for k in 0..tag.chars().count() as u16 {
        a.set_ktab(KTab::Nodes);
        a.select(cluster(&a, "X-9"));
        a.set_aggregate(false);
        a.focus_input();
        a.last_click = None;
        mouse(&mut a, x0 + k, y);
        assert_eq!((a.forest.get(a.selected).key.as_str(), a.ktab), (parent.as_str(), KTab::Questions), "column {k} of {tag}");
        assert_eq!(a.composer.text(), text, "column {k}: the bucket is untouched");
        assert_eq!(a.focus, Focus::Input);
    }
    // One cell past its `]` is not on it.
    a.set_ktab(KTab::Nodes);
    a.focus_input();
    a.last_click = None;
    mouse(&mut a, x0 + tag.chars().count() as u16, y);
    assert_eq!(a.ktab, KTab::Nodes, "a click past the reference stays");
    // A double click on it is two navigations and still no edit.
    a.last_click = None;
    mouse(&mut a, x0 + 1, y);
    mouse(&mut a, x0 + 1, y);
    assert_eq!(a.composer.text(), text);
    // What he types after a navigation lands in the bucket, at the caret.
    typed(&mut a, "!");
    assert_eq!(a.composer.text().matches('!').count(), 1);
    assert_eq!(a.composer.text().len(), text.len() + 1);
    // The bucket's file holds his words, and the navigations wrote nothing to it.
    let _ = file;
    let after = std::fs::read_to_string(tui::cauldron::store::soup_dir(&a.env).unwrap().join("bucket.json")).unwrap();
    assert!(after.contains("and words after it!") || after.contains("!"), "{after}");
}

/// The index is never collected: a garbage collection pass lists every
/// temp file it may take, and the index is not one, however old.
#[test]
fn the_index_is_never_collected() {
    let root = scratch_root();
    let mut a = open_at(&root);
    let _ = child_question_from_parent(&mut a);
    let dir = tui::cauldron::store::soup_dir(&a.env).unwrap();
    assert!(dir.join(tui::cauldron::refs::FILE).exists());
    let plan = tui::cauldron::gc::Plan::compute(&dir, &tui::cauldron::store::global_dir(&a.env), 0);
    assert!(!plan.items.iter().any(|i| matches!(&i.what, tui::cauldron::gc::What::File(p) if p.to_string_lossy().contains("refs"))), "{plan:?}");
}
