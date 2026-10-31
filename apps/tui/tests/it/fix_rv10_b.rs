//! Task fix-rv10-b (re-audit 10, 0316): the bucket's navigation defects. D1
//! first — a hand-typed `[msg:…]` goes to the Background run that cites it,
//! on its cluster's Status tab — then K5, K6, D2 and D6.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Hit, Item, KTab};
use tui::cauldron::render_to_text;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "rv10b-{}-{}-{}",
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
    App::open(env, tui::cauldron::store::seed_dir(), true).expect("opens")
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

/// X-1's first question on its Questions tab: its index and its reference.
fn first_question(a: &mut App) -> (usize, String) {
    let x1 = a.forest.find("X-1").unwrap();
    a.set_ctab(CTab::Cauldron);
    a.select(x1);
    a.set_ktab(KTab::Questions);
    let i = a.items().iter().position(|i| matches!(i, Item::Question { .. })).unwrap();
    (i, a.tag_for(a.items()[i]).unwrap().render())
}

fn caret(a: &App) -> usize {
    tui::cauldron::dump::caret_offset(&a.composer)
}

/// Put `text` in a fresh bucket, the caret at byte `at`, as a merge or a
/// click leaves it, and click X-1's first question in.
fn insert_at(text: &str, at: usize) -> (App, String) {
    let mut a = open_at(&scratch_root());
    a.focus_input();
    a.type_text(text);
    tui::cauldron::dump::set_caret(&mut a.composer, at);
    let (q, t) = first_question(&mut a);
    click(&mut a, q);
    (a, t)
}

/// A click in the bucket on the first character of `needle` as drawn.
fn click_in_bucket(a: &mut App, needle: &str) {
    a.set_ctab(CTab::Cauldron);
    a.last_click = None;
    let s = render_to_text(a, 140, 160).unwrap();
    let (r, _) = a.input_at;
    let y = (r.y as usize..(r.y + r.height) as usize).rev().find(|&y| s[y].contains(needle)).unwrap_or_else(|| panic!("`{needle}` not in the bucket:\n{}", s.join("\n")));
    let x = s[y][..s[y].find(needle).unwrap()].chars().count() as u16 + 1;
    mouse(a, x, y as u16);
}

// ── D1 ─────────────────────────────────────────────────────────────────────

/// 0316 D1 (K4 for messages): a hand-typed `[msg:8e4fca10#96]` — a dump run
/// X-1 lists under Background on its Status tab — goes there, the band open
/// and the run selected. It is never said to be nowhere.
#[test]
fn d1_a_typed_msg_reference_goes_to_its_background_run() {
    let mut a = open_at(&scratch_root());
    // The band starts shut, so the search has to open it.
    let x1 = a.forest.find("X-1").unwrap();
    assert!(a.forest.get(x1).background.iter().any(|b| b.cite.as_deref() == Some("8e4fca10#96")), "the seed lists #96 under X-1");
    a.focus_input();
    a.type_text("see [msg:8e4fca10#96] now ");
    let text = a.composer.text();
    click_in_bucket(&mut a, "[msg:8e4fca10#96]");
    let n = a.notice.clone().unwrap_or_default();
    assert!(!n.contains("nowhere"), "{n}");
    assert_eq!((a.ctab, a.ktab), (CTab::Cauldron, KTab::Status), "{n}");
    assert_eq!(a.forest.get(a.selected).key, "X-1", "{n}");
    let it = a.items().get(a.item).copied().unwrap_or_else(|| panic!("nothing selected: {n}"));
    assert!(matches!(it, Item::Background { .. }), "{it:?}: {n}");
    assert_eq!(a.tag_for(it).unwrap().render(), "[msg:8e4fca10#96]");
    // Drawn: the run's line is on screen.
    let s = render_to_text(&mut a, 140, 60).unwrap().join("\n");
    assert!(s.contains("8e4fca10#96 → 6 atoms"), "{s}");
    assert_eq!(a.composer.text(), text, "navigation never changes the bucket");
    // Opposed: a message nothing cites is still nowhere, and moves nothing.
    let mut b = open_at(&scratch_root());
    b.focus_input();
    b.type_text("see [msg:8e4fca10#99999] ");
    let before = (b.ctab, b.ktab, b.selected);
    click_in_bucket(&mut b, "[msg:8e4fca10#99999]");
    assert!(b.notice.as_deref().is_some_and(|n| n.contains("nowhere")), "{:?}", b.notice);
    assert_eq!((b.ctab, b.ktab, b.selected), before);
}

// ── K5 and D9 ──────────────────────────────────────────────────────────────

/// 0316 K5: after an H6 word-boundary insert, or after punctuation, the
/// reference has exactly one space on each side; the caret sits after it
/// and its one space, and the index knows where it is. D9's shape — `It`
/// becoming `I [T-13] t` — never happens: a caret inside a word moves the
/// reference to the word's edge.
#[test]
fn k5_one_space_each_side_and_never_inside_a_word() {
    // (draft, caret byte, expected with `R` for the reference)
    let cases: &[(&str, usize, &str)] = &[
        ("It indicates", 2, "It R indicates"),    // end of a word, a space after
        ("It indicates", 1, "It R indicates"),    // D9: between I and t
        ("It indicates", 3, "It R indicates"),    // start of a word
        ("hello, world", 6, "hello, R world"),    // after punctuation
        ("hello,world", 6, "hello,world R "),    // one word: to its nearer edge
        ("a café b", 3, "a R café b"),            // inside a word, multibyte
        ("a café b", 5, "a café R b"),            // nearer its end
        ("see https://x.io/a now", 10, "see R https://x.io/a now"),
        ("hi 🎉 there", 3, "hi R 🎉 there"),
        ("go", 2, "go R "),                       // at the end: one space after
        ("", 0, "R "),
    ];
    for &(draft, at, want) in cases {
        let (mut a, t) = insert_at(draft, at);
        let want = want.replace('R', &t);
        let got = a.composer.text();
        assert_eq!(got, want, "{draft:?} at {at}");
        assert!(!got.contains("  "), "{draft:?} at {at}: two spaces in {got:?}");
        // The caret is after the reference and its one space.
        let end = got.find(&t).unwrap() + t.len() + 1;
        assert_eq!(caret(&a), end, "{draft:?} at {at}: caret");
        // The index recorded it where it is: a click on it goes to X-1's
        // Questions, where it came from.
        a.set_ktab(KTab::Details);
        click_in_bucket(&mut a, &t);
        assert_eq!((a.ctab, a.ktab), (CTab::Cauldron, KTab::Questions), "{draft:?} at {at}: {:?}", a.notice);
    }
}

/// D9 with a second face typing the word the reference lands next to: g is
/// typing `It`, f clicks a reference in between the two keys. Every letter
/// g types stays in its word.
#[test]
fn d9_a_second_face_typing_never_gets_a_reference_inside_its_word() {
    let root = scratch_root();
    let mut f = open_at(&root);
    let mut g = open_at(&root);
    g.focus_input();
    typed(&mut g, "so I");
    f.sync();
    // f's caret at the end of g's half-typed word.
    let end = f.composer.text().len();
    tui::cauldron::dump::set_caret(&mut f.composer, end);
    let (q, t) = first_question(&mut f);
    click(&mut f, q);
    g.sync();
    typed(&mut g, "t indicates");
    for _ in 0..2 {
        f.sync();
        g.sync();
    }
    let got = g.composer.text();
    assert!(got.contains("It indicates"), "g's word split: {got:?}");
    assert_eq!(got.matches(&t).count(), 1, "{got:?}");
    assert!(!got.contains(&format!("I {t} t")), "{got:?}");
    assert_eq!(f.composer.text(), got, "the faces converge");
    // And the other way: f's caret inside g's word as the merge left it.
    let root = scratch_root();
    let mut f = open_at(&root);
    let mut g = open_at(&root);
    f.focus_input();
    f.type_text("x It indicates");
    g.sync();
    tui::cauldron::dump::set_caret(&mut f.composer, 3);
    g.focus_input();
    tui::cauldron::dump::set_caret(&mut g.composer, 0);
    typed(&mut g, "hey ");
    f.sync();
    let (q, t) = first_question(&mut f);
    click(&mut f, q);
    g.sync();
    let got = g.composer.text();
    assert!(got.starts_with("hey x It ") && got.contains("indicates"), "{got:?}");
    assert!(!got.contains(&format!("I {t} t")) && !got.contains(&format!("I{t}")), "{got:?}");
}

// ── K6 ─────────────────────────────────────────────────────────────────────

/// 0316 K6: with the caret inside a paste chip's label (a merge can leave it
/// there), a click never inserts inside the chip: the reference goes to the
/// chip's end, one space off it, and the chip stays whole.
#[test]
fn k6_an_insert_never_lands_inside_a_chip() {
    let chip = "[paste b21c: \"fn main\" 14 lines · 412 chars]";
    let draft = format!("look {chip} here");
    let start = draft.find(chip).unwrap();
    // Inside the label's word, between words in the label, just after `[`,
    // and on its last character.
    for off in [chip.find("main").unwrap() + 2, chip.find(" main").unwrap(), 1, chip.len() - 1] {
        let (a, t) = insert_at(&draft, start + off);
        let got = a.composer.text();
        assert_eq!(got, format!("look {chip} {t} here"), "caret {off} into the chip");
    }
    // Opposed: on either edge of the chip the insert goes there, unmoved.
    let (a, t) = insert_at(&draft, start);
    assert_eq!(a.composer.text(), format!("look {t} {chip} here"));
    let (a, t) = insert_at(&draft, start + chip.len());
    assert_eq!(a.composer.text(), format!("look {chip} {t} here"));
}

// ── D2 ─────────────────────────────────────────────────────────────────────

/// 0316 D2: a dump reference moves him to Status › Ledger only when its row
/// is there. One that is not leaves him where he is, and the `nowhere`
/// notice is on the page he is on. One that is lands him on its row, and
/// the notice — on the Status box he lands on — names Status, not the
/// cluster (`Cauldron ›` is a root tab Status is not under).
#[test]
fn d2_a_dump_reference_moves_only_to_a_row_that_is_there() {
    // Missing: he stays on the cluster page, and sees the notice there.
    let mut a = open_at(&scratch_root());
    a.focus_input();
    a.type_text("see [dump:99] and [dump:11] ");
    let x1 = a.forest.find("X-1").unwrap();
    a.select(x1);
    a.set_ktab(KTab::Questions);
    let before = (a.selected, a.ktab, a.sttab, a.rows[0]);
    click_in_bucket(&mut a, "[dump:99]");
    let n = a.notice.clone().unwrap_or_default();
    assert!(n.contains("nowhere"), "{n}");
    assert_eq!(a.ctab, CTab::Cauldron, "{n}: no jump");
    assert_eq!((a.selected, a.ktab, a.sttab, a.rows[0]), before, "{n}: nothing he is on changes");
    let s = render_to_text(&mut a, 140, 50).unwrap().join("\n");
    assert!(s.contains("[dump:99] is nowhere"), "the notice is on screen:\n{s}");
    // There: Status › Ledger, on its row; the notice on the Status box.
    click_in_bucket(&mut a, "[dump:11]");
    let n = a.notice.clone().unwrap_or_default();
    assert_eq!(n, "[dump:11] — Status › Ledger");
    assert_eq!(a.ctab, CTab::Status);
    let v = tui::cauldron::status::view(&a, 120);
    assert_eq!(v.items[a.rows[0]].key, "ld:11");
    let s = render_to_text(&mut a, 140, 50).unwrap().join("\n");
    assert!(s.contains("[dump:11] — Status › Ledger"), "the notice is on screen:\n{s}");
    // A cluster-page reference keeps its cluster in the path (opposed).
    let mut b = open_at(&scratch_root());
    b.focus_input();
    b.type_text("see [cluster:X-2] ");
    click_in_bucket(&mut b, "[cluster:X-2]");
    let n = b.notice.clone().unwrap_or_default();
    assert_eq!(n, "[X-2] — Interface › Details");
}

/// D2's other Status references (a service, a run): the same — to a row
/// that is there, or nowhere, unmoved.
#[test]
fn d2_a_status_reference_to_nothing_stays_put() {
    let mut a = open_at(&scratch_root());
    a.focus_input();
    a.type_text("see [service:no-such-service] ");
    let before = (a.selected, a.ktab, a.sttab);
    click_in_bucket(&mut a, "[service:no-such-service]");
    let n = a.notice.clone().unwrap_or_default();
    assert!(n.contains("nowhere"), "{n}");
    assert_eq!((a.ctab, a.selected, a.ktab, a.sttab), (CTab::Cauldron, before.0, before.1, before.2), "{n}");
    // A service that is on Status › Mesh is gone to, and named without a
    // cluster.
    a.set_ctab(CTab::Status);
    let found = tui::cauldron::status::StatusTab::ALL.into_iter().find_map(|t| {
        a.set_sttab(t);
        tui::cauldron::status::view(&a, 120).items.into_iter().find_map(|it| it.tag.filter(|t| t.render().starts_with("[service:")).map(|t| t.render()))
    });
    let Some(svc) = found else { panic!("the seed shows no service on Status") };
    a.set_ctab(CTab::Cauldron);
    a.focus_input();
    let end = a.composer.text().len();
    tui::cauldron::dump::set_caret(&mut a.composer, end);
    a.type_text(&format!("and {svc} "));
    click_in_bucket(&mut a, &svc);
    let n = a.notice.clone().unwrap_or_default();
    assert_eq!(a.ctab, CTab::Status, "{n}");
    assert!(n.starts_with(&format!("{svc} — Status › ")), "{n}");
}

// ── D6 ─────────────────────────────────────────────────────────────────────

/// The first ripple row of Status › Ledger: the row, as `Hit::Row` counts.
fn ledger_ripple(a: &mut App) -> usize {
    a.set_ctab(CTab::Status);
    a.set_sttab(tui::cauldron::status::StatusTab::Ledger);
    let v = tui::cauldron::status::view(a, 120);
    v.items.iter().position(|it| matches!(it.act, tui::cauldron::status::SAct::Exec { .. })).expect("a ripple on the Ledger")
}

/// 0316 D6: a double click on a Ledger ripple row explores it, and the
/// explore page stays open. Opposed: a click outside it, not the second of
/// a double, still shuts it; and so does a second press that came too late.
#[test]
fn d6_a_double_click_on_a_ledger_ripple_explores_and_stays_open() {
    let mut a = open_at(&scratch_root());
    let i = ledger_ripple(&mut a);
    a.last_click = None;
    press_hit(&mut a, Hit::Row(i));
    assert!(a.modal.is_some(), "one click explores it");
    // The second press, at once, on the same row (outside the modal).
    let _ = render_to_text(&mut a, 140, 160).unwrap();
    let r = a.hits.iter().find(|(_, h)| *h == Hit::Row(i)).map(|(r, _)| *r).expect("the row is still drawn under the modal");
    assert!(a.hits.iter().rev().find(|(q, _)| r.x >= q.x && r.x < q.x + q.width && r.y >= q.y && r.y < q.y + q.height).is_some_and(|(_, h)| *h == Hit::Row(i)), "the press lands on the row, outside the modal");
    mouse(&mut a, r.x, r.y);
    assert!(a.modal.is_some(), "the double click's second press never shuts it");
    let _ = render_to_text(&mut a, 140, 160).unwrap();
    assert!(a.modal.is_some(), "still open after it is drawn");
    // Opposed: a third press there, now a single click, shuts it.
    mouse(&mut a, r.x, r.y);
    assert!(a.modal.is_none(), "a plain click outside shuts it");
    // Opposed: a second press too late to be a double shuts it too.
    a.last_click = None;
    press_hit(&mut a, Hit::Row(i));
    assert!(a.modal.is_some());
    let _ = render_to_text(&mut a, 140, 160).unwrap();
    a.last_click = a.last_click.map(|(t, h)| (t - std::time::Duration::from_secs(5), h));
    mouse(&mut a, r.x, r.y);
    assert!(a.modal.is_none(), "a slow second press is a click, and shuts it");
}
