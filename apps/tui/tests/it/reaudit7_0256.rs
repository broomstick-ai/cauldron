//! Task 0256's second half (8e4fca10 msg 235, the 0255 re-audit): H1 a
//! double click never deletes what another face typed, H2 what he types after
//! a click goes to the bucket, H3 the whole cauldron's Overview holds real
//! values, H4 the help and palette follow msg 23, H5 a replacing click says
//! so. Real seed files, real events, scratch roots.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, Focus, Hit, Item, KTab};
use tui::cauldron::render_to_text;
use tui::cauldron::store::seed_dir;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0256b-{}-{}-{}",
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

fn key(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(key(c));
    }
}

fn press_hit(a: &mut App, hit: Hit) {
    let _ = render_to_text(a, 140, 50).unwrap();
    let r = a.hits.iter().rev().find(|(_, h)| *h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {hit:?} on screen"));
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
    }
}

/// Two questions' item indexes on X-1's Human tab, the page holding them.
fn two_questions(a: &mut App) -> (usize, usize) {
    let x1 = a.forest.find("X-1").unwrap();
    a.select(x1);
    a.set_ktab(KTab::Questions);
    let qs: Vec<usize> = a.items().iter().enumerate().filter(|(_, i)| matches!(i, Item::Question { .. })).map(|(i, _)| i).collect();
    (qs[0], qs[1])
}

fn tag_of(a: &App, i: usize) -> String {
    a.tag_for(a.items()[i]).unwrap().render()
}

// ── H2 ─────────────────────────────────────────────────────────────────────

#[test]
fn h2_what_he_types_after_a_click_goes_to_the_bucket() {
    let mut a = open_at(&scratch_root());
    let (q1, q2) = two_questions(&mut a);
    let (t1, t2) = (tag_of(&a, q1), tag_of(&a, q2));
    a.type_text("start ");
    press_hit(&mut a, Hit::Item(q1));
    assert_eq!(a.focus, Focus::Content, "the click keeps the page's selection");
    // 0255's own words: `quick note` quit the face, `i` quoted, `a` was eaten.
    typed(&mut a, " quick note abc");
    assert!(!a.quit, "q never quits mid-sentence");
    assert_eq!(a.focus, Focus::Input);
    assert_eq!(a.composer.text(), format!("start {t1}  quick note abc"));
    // The space he typed keeps it: the next click goes beside it.
    press_hit(&mut a, Hit::Item(q2));
    assert_eq!(a.composer.text(), format!("start {t1}  quick note abc {t2} "));
    // A space typed right after a click (msg 23's `separate by a space`).
    let mut b = open_at(&scratch_root());
    let (q1, q2) = two_questions(&mut b);
    press_hit(&mut b, Hit::Item(q1));
    typed(&mut b, " ");
    press_hit(&mut b, Hit::Item(q2));
    assert_eq!(b.composer.text(), format!("{t1}  {t2} "), "both kept");
    // Without a click first, page keys are still page keys.
    let mut c = open_at(&scratch_root());
    let _ = two_questions(&mut c);
    c.focus = Focus::Content;
    typed(&mut c, "q");
    assert!(c.quit, "a bare q on the page still quits");
}

// ── H5 ─────────────────────────────────────────────────────────────────────

#[test]
fn h5_a_click_that_replaces_says_what_it_replaced() {
    let mut a = open_at(&scratch_root());
    let (q1, q2) = two_questions(&mut a);
    let (t1, t2) = (tag_of(&a, q1), tag_of(&a, q2));
    press_hit(&mut a, Hit::Item(q1));
    assert_eq!(a.notice.as_deref(), Some(format!("{t1} at the bucket's caret").as_str()));
    press_hit(&mut a, Hit::Item(q2));
    assert_eq!(a.notice.as_deref(), Some(format!("replaced {t1} with {t2} at the bucket's caret").as_str()));
}

// ── H1 ─────────────────────────────────────────────────────────────────────

/// The other face types while this one double-clicks: before the first
/// press (that press's save merges it in), between the presses, and after.
/// Not one of its characters goes.
#[test]
fn h1_a_double_click_never_deletes_what_another_face_typed() {
    let root = scratch_root();
    let mut f = open_at(&root);
    let mut g = open_at(&root);
    let (q1, _) = two_questions(&mut f);
    let t1 = tag_of(&f, q1);
    f.type_text("start");
    g.sync();
    g.focus_input();
    g.composer = tui_render::compose::Composer::from_text(&g.composer.text());
    let end = |g: &mut App| {
        for _ in 0..4 {
            g.handle(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        }
        g.handle(Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)));
    };
    let words = ["one", "two", "three", "four", "five", "six"];
    for (r, w) in words.iter().enumerate() {
        end(&mut g);
        f.last_click = None;
        match r % 3 {
            0 => {
                typed(&mut g, &format!(" {w}"));
                press_hit(&mut f, Hit::Item(q1));
                press_hit(&mut f, Hit::Item(q1));
            }
            1 => {
                press_hit(&mut f, Hit::Item(q1));
                typed(&mut g, &format!(" {w}"));
                press_hit(&mut f, Hit::Item(q1));
            }
            _ => {
                press_hit(&mut f, Hit::Item(q1));
                press_hit(&mut f, Hit::Item(q1));
                typed(&mut g, &format!(" {w}"));
            }
        }
        f.sync();
        g.sync();
    }
    for _ in 0..2 {
        f.sync();
        g.sync();
    }
    let (x, y) = (f.composer.text(), g.composer.text());
    assert_eq!(x, y, "the faces converge");
    for w in words {
        assert_eq!(x.matches(w).count(), 1, "{w} in {x:?}");
    }
    assert!(x.starts_with("start"), "{x:?}");
    // Each double click quotes; a quote is his and the next double click
    // leaves it standing (0258 J2).
    assert!(x.contains(&format!("> {t1}")), "{x:?}");
}

/// The double click's undo, at the store: the other face's text merged in by
/// the first press's own save stays.
#[test]
fn h1_the_undo_takes_back_only_its_own_edit() {
    let root = scratch_root();
    let mut f = open_at(&root);
    let mut g = open_at(&root);
    let (q1, _) = two_questions(&mut f);
    f.type_text("abc");
    g.sync();
    g.focus_input();
    g.handle(Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)));
    typed(&mut g, "XYZ");
    f.last_click = None;
    press_hit(&mut f, Hit::Item(q1));
    assert!(f.composer.text().contains("XYZ"), "the first press's save merged g's text in: {:?}", f.composer.text());
    press_hit(&mut f, Hit::Item(q1));
    let t = f.composer.text();
    assert!(t.contains("XYZ"), "H1: {t:?}");
    assert!(t.starts_with("abc"), "{t:?}");
}

// ── H3 ─────────────────────────────────────────────────────────────────────

#[test]
fn h3_the_whole_cauldrons_overview_holds_real_values() {
    let mut a = open_at(&scratch_root());
    a.set_aggregate(true);
    let w = a.forest.whole().expect("the whole cauldron");
    a.select(w);
    a.set_ktab(KTab::Details);
    let r = render_to_text(&mut a, 140, 120).unwrap().join("\n");
    assert!(r.contains("Everything in the prod cauldron"), "{r}");
    let line = |name: &str| r.lines().find(|l| l.contains(&format!("• {name}"))).unwrap_or_else(|| panic!("no {name} band:\n{r}")).to_string();
    let n = |l: &str, unit: &str| -> usize {
        let s = l.split('│').nth(2).unwrap_or(l);
        s.split_whitespace().rev().collect::<Vec<_>>().windows(2).find(|w| w[0] == unit).and_then(|w| w[1].parse().ok()).unwrap_or_else(|| panic!("no count in {l:?}"))
    };
    let dumps = a.sends.sends.len();
    assert!(dumps > 0, "the seed holds dumps");
    assert!(line("Last dump").contains(&format!("{dumps} dump")), "{}", line("Last dump"));
    let dirs: usize = a.forest.clusters.iter().map(|c| c.directives.len()).sum();
    let cons: usize = a.forest.clusters.iter().map(|c| c.constraints.len()).sum();
    assert!(dirs > 0 && cons > 0);
    let _ = n;
    assert!(line("Directives").contains(&format!(" {dirs} directives · you")), "{}", line("Directives"));
    assert!(line("Constraints").contains(&format!(" {cons} constraints · you")), "{}", line("Constraints"));
}

// ── H4 ─────────────────────────────────────────────────────────────────────

#[test]
fn h4_the_help_names_human_and_the_orders_msg_23_set() {
    let scratch = tempfile::tempdir().unwrap();
    let out = crate::face::Face::new(scratch.path()).args(["--cauldron", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    // 8e4fca10 msg 385 flattened Human: no --human-tab, and Questions and
    // Actions are cluster tabs.
    assert!(help.contains("questions | actions | tasks | nodes | status | details"), "{help}");
    assert!(!help.contains("--human-tab") && !help.contains("--tasks-tab"), "{help}");
    assert!(help.contains("tokens | mesh | soup | bus"), "{help}");
    assert!(help.contains("cauldron (this cauldron) | tui | devices | providers | permissions"), "{help}");
    // The palette names the replace and the whole cauldron.
    let pal: String = tui::cauldron::app::KEYMAP.iter().map(|(k, v)| format!("{k} {v}\n")).collect();
    assert!(pal.contains("in place of the one there"), "{pal}");
    assert!(pal.contains("the whole cauldron too"), "{pal}");
}

/// Adversarial: the other face types right where this face's caret sits,
/// after its reference, and this face clicks before its tick: the click
/// judges the bucket as merged, replaces only the reference this face's
/// caret sits on, and nothing of the other face's goes.
#[test]
fn h1_a_click_judges_the_reference_at_the_caret_against_the_merged_bucket() {
    let root = scratch_root();
    let mut f = open_at(&root);
    let mut g = open_at(&root);
    let (q1, q2) = two_questions(&mut f);
    let (t1, t2) = (tag_of(&f, q1), tag_of(&f, q2));
    press_hit(&mut f, Hit::Item(q1));
    g.sync();
    g.focus_input();
    g.handle(Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)));
    typed(&mut g, "his words");
    // f has not ticked: its caret is one space after its reference.
    press_hit(&mut f, Hit::Item(q2));
    let t = f.composer.text();
    // This face's caret still sits on its own reference, so that one is
    // replaced; his words, typed after it, are untouched.
    assert_eq!(t, format!("{t2} his words"));
    let _ = t1;
}
