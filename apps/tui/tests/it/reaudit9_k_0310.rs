//! Task 0310's addendum (8e4fca10 mail #78): the defects 0305 found in the
//! bucket's click paths, K1–K4. K1 is input loss and goes first: click A,
//! click B (B replaces A), double click C brought A back and dropped B. Each
//! is proved by its exact sequence on real frames, and again with a second
//! face typing into the same bucket.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Hit, Item, KTab};
use tui::cauldron::render_to_text;
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::seed_dir;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0310k-{}-{}-{}",
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

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
    }
}

fn mouse(a: &mut App, x: u16, y: u16) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
    }
}

fn press_hit(a: &mut App, hit: Hit) {
    let _ = render_to_text(a, 140, 160).unwrap();
    let r = a.hits.iter().rev().find(|(_, h)| *h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {hit:?} on screen"));
    mouse(a, r.x, r.y);
}

/// One single click on page item `i`, never read as half of a double.
fn click(a: &mut App, i: usize) {
    a.last_click = None;
    press_hit(a, Hit::Item(i));
}

/// A double click on item `i`: two presses with no slower click before.
fn double(a: &mut App, i: usize) {
    a.last_click = None;
    press_hit(a, Hit::Item(i));
    let _ = render_to_text(a, 140, 160).unwrap();
    a.last_click = Some((std::time::Instant::now(), Hit::Item(i)));
    let r = a.hits.iter().rev().find(|(_, h)| *h == Hit::Item(i)).map(|(r, _)| *r).unwrap();
    mouse(a, r.x, r.y);
}

/// X-1's three questions, on its Questions tab.
fn three_questions(a: &mut App) -> [usize; 3] {
    let x1 = a.forest.find("X-1").unwrap();
    a.select(x1);
    a.set_ktab(KTab::Questions);
    let qs: Vec<usize> = a.items().iter().enumerate().filter(|(_, i)| matches!(i, Item::Question { .. })).map(|(i, _)| i).collect();
    [qs[0], qs[1], qs[2]]
}

fn tag_of(a: &App, i: usize) -> String {
    a.tag_for(a.items()[i]).unwrap().render()
}

/// A click in the bucket on the first character of `needle` as drawn.
fn click_in_bucket(a: &mut App, needle: &str) {
    a.set_ctab(CTab::Cauldron);
    a.last_click = None;
    let s = render_to_text(a, 140, 160).unwrap();
    // Only the bucket's text rows: the notice on its border may name it too.
    let (r, _) = a.input_at;
    let y = (r.y as usize..(r.y + r.height) as usize).rev().find(|&y| s[y].contains(needle)).unwrap_or_else(|| panic!("`{needle}` not in the bucket:\n{}", s.join("\n")));
    let x = s[y][..s[y].find(needle).unwrap()].chars().count() as u16 + 1;
    mouse(a, x, y as u16);
}

fn caret(a: &App) -> usize {
    tui::cauldron::dump::caret_offset(&a.composer)
}

// ── K1 ─────────────────────────────────────────────────────────────────────

/// 0305 K1, its exact sequence: click A, click B (which replaces A), double
/// click C. B is cleared, as msg 23 asks, and A — already replaced — never
/// comes back.
#[test]
fn k1_click_a_click_b_double_c_keeps_neither_and_never_brings_a_back() {
    let mut a = open_at(&scratch_root());
    let [qa, qb, qc] = three_questions(&mut a);
    let (ta, tb, tc) = (tag_of(&a, qa), tag_of(&a, qb), tag_of(&a, qc));
    a.type_text("go ");
    click(&mut a, qa);
    assert_eq!(a.composer.text(), format!("go {ta} "));
    click(&mut a, qb);
    assert_eq!(a.composer.text(), format!("go {tb} "), "B replaces A (msg 23)");
    double(&mut a, qc);
    let t = a.composer.text();
    assert_eq!(t, format!("go\n\n> {tc}"), "B cleared, C quoted");
    assert!(!t.contains(&ta), "A never comes back: {t:?}");
    // On disk, the same.
    let _ = render_to_text(&mut a, 140, 160).unwrap();
    a.sync();
    assert_eq!(a.composer.text(), format!("go\n\n> {tc}"));
    // A reference he TYPED, replaced by a click, still comes back (J2).
    let mut b = open_at(&scratch_root());
    let [qa, qb, _] = three_questions(&mut b);
    let tb = tag_of(&b, qb);
    b.type_text("x [DIR-3]");
    click(&mut b, qa);
    double(&mut b, qb);
    assert_eq!(b.composer.text(), format!("x [DIR-3]\n\n> {tb}"));
    // A longer chain: four single clicks, then a double — only the typed
    // text and the quote are left.
    let mut c = open_at(&scratch_root());
    let [qa, qb, qc] = three_questions(&mut c);
    let tc = tag_of(&c, qc);
    c.type_text("chain ");
    for q in [qa, qb, qa, qb] {
        click(&mut c, q);
    }
    double(&mut c, qc);
    assert_eq!(c.composer.text(), format!("chain\n\n> {tc}"));
}

/// K1 with a second face typing into the same bucket between the clicks:
/// every word it typed stays, B is cleared, A never comes back.
#[test]
fn k1_with_a_second_face_typing() {
    let root = scratch_root();
    let mut f = open_at(&root);
    let mut g = open_at(&root);
    let [qa, qb, qc] = three_questions(&mut f);
    let (ta, tb, tc) = (tag_of(&f, qa), tag_of(&f, qb), tag_of(&f, qc));
    f.type_text("go ");
    click(&mut f, qa);
    // g types at the start, between A and B.
    g.sync();
    g.focus_input();
    tui::cauldron::dump::set_caret(&mut g.composer, 0);
    typed(&mut g, "hey ");
    f.sync();
    click(&mut f, qb);
    // And again, at the end, between B and the double.
    g.sync();
    let end = g.composer.text().len();
    tui::cauldron::dump::set_caret(&mut g.composer, end);
    typed(&mut g, "tail");
    f.sync();
    assert!(f.composer.text().contains(&tb) && !f.composer.text().contains(&ta), "{:?}", f.composer.text());
    double(&mut f, qc);
    for _ in 0..2 {
        f.sync();
        g.sync();
    }
    let t = f.composer.text();
    assert!(t.starts_with("hey go") && t.contains("tail") && t.contains(&format!("> {tc}")), "{t:?}");
    assert!(!t.contains(&ta), "A never comes back: {t:?}");
    assert!(!t.contains(&tb), "B is cleared: {t:?}");
    assert_eq!(t.matches("hey").count(), 1, "{t:?}");
    assert_eq!(t.matches("tail").count(), 1, "{t:?}");
    assert_eq!(f.composer.text(), g.composer.text(), "the faces converge");
}

// ── K2 ─────────────────────────────────────────────────────────────────────

/// 0305 K2: navigate by `[Q-1]`; another face types before it; the next page
/// click goes BESIDE it (J7), not in its place. Once he types himself, msg
/// 23's replace holds again.
#[test]
fn k2_the_reference_he_went_by_survives_another_faces_typing() {
    let root = scratch_root();
    let mut f = open_at(&root);
    let mut g = open_at(&root);
    let [q1, q2, _] = three_questions(&mut f);
    let (t1, t2) = (tag_of(&f, q1), tag_of(&f, q2));
    f.type_text("see ");
    click(&mut f, q1);
    assert_eq!(f.composer.text(), format!("see {t1} "));
    click_in_bucket(&mut f, &t1);
    assert!(f.notice.as_deref().is_some_and(|n| n.starts_with(&t1)), "{:?}", f.notice);
    // g types `hey ` at the start.
    g.sync();
    g.focus_input();
    tui::cauldron::dump::set_caret(&mut g.composer, 0);
    typed(&mut g, "hey ");
    f.sync();
    assert_eq!(f.composer.text(), format!("hey see {t1} "));
    let _ = three_questions(&mut f);
    click(&mut f, q2);
    assert_eq!(f.composer.text(), format!("hey see {t1}  {t2} "), "beside it, not in its place");
    // He went by it again and then typed himself: the next click replaces
    // the reference at his caret, as msg 23 says.
    click_in_bucket(&mut f, &t1);
    f.focus_input();
    typed(&mut f, "x");
    let _ = three_questions(&mut f);
    let before = f.composer.text();
    click(&mut f, q2);
    assert_ne!(f.composer.text(), before);
    assert!(!f.composer.text().contains(&format!("{t1}x  {t2}  {t2}")), "{:?}", f.composer.text());
}

// ── K3 ─────────────────────────────────────────────────────────────────────

/// 0305 K3: after a click on a long-form reference the caret is after it
/// as WRITTEN, so what he types next never breaks it.
#[test]
fn k3_a_long_form_reference_keeps_the_caret_outside_it() {
    for (written, short) in [("[cluster:X-2]", "[X-2]"), ("[node:IS-1]", "[IS-1]")] {
        let mut a = open_at(&scratch_root());
        a.focus_input();
        a.type_text(&format!("go {written} now"));
        click_in_bucket(&mut a, written);
        assert_eq!(caret(&a), format!("go {written}").len(), "{written}: the caret is after it");
        a.set_ctab(CTab::Cauldron);
        a.focus_input();
        typed(&mut a, "ZZ");
        assert_eq!(a.composer.text(), format!("go {written}ZZ now"), "{written} stays whole");
        assert_ne!(written, short);
    }
}

// ── K4 ─────────────────────────────────────────────────────────────────────

/// 0305 K4: a hand-typed cluster or dump reference goes to it; it is never
/// said to be nowhere.
#[test]
fn k4_typed_cluster_and_dump_references_go_to_them() {
    let mut a = open_at(&scratch_root());
    a.focus_input();
    a.type_text("see [X-1] and [cluster:X-2] and [dump:11] ");
    let text = a.composer.text();
    for (needle, cluster) in [("[X-1]", "X-1"), ("[cluster:X-2]", "X-2")] {
        click_in_bucket(&mut a, needle);
        let n = a.notice.clone().unwrap_or_default();
        assert!(!n.contains("nowhere"), "{needle}: {n}");
        assert_eq!((a.ctab, a.ktab), (CTab::Cauldron, KTab::Details), "{needle}: {n}");
        assert_eq!(a.forest.get(a.selected).key, cluster, "{needle}: {n}");
        assert!(n.contains("Details"), "{needle}: {n}");
    }
    click_in_bucket(&mut a, "[dump:11]");
    let n = a.notice.clone().unwrap_or_default();
    assert!(!n.contains("nowhere"), "{n}");
    assert_eq!((a.ctab, a.sttab), (CTab::Status, StatusTab::Ledger), "{n}");
    let v = tui::cauldron::status::view(&a, 120);
    assert_eq!(v.items[a.rows[0]].key, "ld:11", "{n}");
    assert_eq!(a.composer.text(), text, "navigation never changes the bucket");
    // A dump that does not exist is still said to be nowhere.
    let mut b = open_at(&scratch_root());
    b.focus_input();
    b.type_text("see [dump:99] ");
    click_in_bucket(&mut b, "[dump:99]");
    assert!(b.notice.as_deref().is_some_and(|n| n.contains("nowhere")), "{:?}", b.notice);
}
