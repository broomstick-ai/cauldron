//! **#34: text can be selected and copied on every surface** — "Text
//! selection on every surface: modals, error states, lists and pages, not
//! only the thread transcript."
//!
//! A drag over the drawn frame selects; its release copies through the face's
//! one copy path, `Outbox::Copy`, which only the live loop performs — so no
//! test here writes a clipboard (each asserts the outbox instead). A click
//! still acts as it did, and the divider still drags.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::accounts::{LoginCode, LoginStage};
use tui::cauldron::app::{App, CTab, Focus, Hit, Modal, Outbox};
use tui::cauldron::render_to_text;
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::seed_dir;

const W: u16 = 140;
const H: u16 = 60;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "select34-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") }
}

/// A face over the seed, writing nothing (`persist` off, as a `--dump`).
fn face() -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(&scratch_root()), seed_dir(), false).expect("seed loads")
}

fn mouse(a: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
}

/// What the live face does: a frame is drawn before every event, so each is
/// resolved against what he sees.
fn frame(a: &mut App) -> Vec<String> {
    render_to_text(a, W, H).unwrap()
}

fn click(a: &mut App, x: u16, y: u16) {
    frame(a);
    mouse(a, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(a, MouseEventKind::Up(MouseButton::Left), x, y);
}

/// Press at `from`, drag through to `to`, release there.
fn drag(a: &mut App, from: (u16, u16), to: (u16, u16)) {
    frame(a);
    mouse(a, MouseEventKind::Down(MouseButton::Left), from.0, from.1);
    frame(a);
    mouse(a, MouseEventKind::Drag(MouseButton::Left), (from.0 + to.0) / 2, (from.1 + to.1) / 2);
    frame(a);
    mouse(a, MouseEventKind::Drag(MouseButton::Left), to.0, to.1);
    mouse(a, MouseEventKind::Up(MouseButton::Left), to.0, to.1);
}

/// The one text the release asked to be copied.
fn copied(a: &App) -> String {
    let c: Vec<&String> = a.outbox.iter().filter_map(|o| if let Outbox::Copy(t) = o { Some(t) } else { None }).collect();
    assert_eq!(c.len(), 1, "one copy asked: {:?}", a.outbox);
    c[0].clone()
}

/// The cell (column, row) where `needle` is first drawn in `rows`.
fn find(rows: &[String], needle: &str) -> (u16, u16) {
    for (y, l) in rows.iter().enumerate() {
        if let Some(b) = l.find(needle) {
            return (l[..b].chars().count() as u16, y as u16);
        }
    }
    panic!("`{needle}` is not drawn:\n{}", rows.join("\n"));
}

fn words(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// The login modal's failure — his first report — is selected and copied
/// exactly, URL and all, and the modal stays as it was.
#[test]
fn the_login_modals_error_is_copied() {
    let mut a = face();
    let said = "not started: the threads daemon refused the login, see https://example.invalid/help/login";
    a.modal = Some(Modal::Login { title: "Log in acct1".into(), stage: LoginStage::Done { ok: false, said: said.into() }, code: LoginCode::default(), tag: 1 });
    let rows = frame(&mut a);
    let start = find(&rows, "not started");
    // Its last word, wherever the modal wrapped it.
    let end = find(&rows, "help/login");
    drag(&mut a, start, (end.0 + "help/login".len() as u16 - 1, end.1));
    assert_eq!(words(&copied(&a)), words(said));
    assert!(!copied(&a).contains(['│', '┃']), "no border is copied: {:?}", copied(&a));
    assert!(matches!(a.modal, Some(Modal::Login { .. })), "the modal stays open");
    assert_eq!(a.notice.as_deref(), Some(format!("sending {} characters to the terminal's clipboard (OSC 52)", copied(&a).chars().count()).as_str()));
    // The selection is drawn: its cells are reversed.
    assert!(a.select.active());
}

/// The URL the login modal asks him to open is copied on its own.
#[test]
fn the_login_url_is_copied() {
    let mut a = face();
    let url = "https://example.invalid/oauth/authorize?code=true";
    a.modal = Some(Modal::Login { title: "Log in acct1".into(), stage: LoginStage::AwaitCode { session_id: "s1".into(), url: url.into() }, code: LoginCode::default(), tag: 1 });
    let rows = frame(&mut a);
    let (x, y) = find(&rows, "https://example.invalid");
    let line = &rows[y as usize];
    let shown = line[line.find("https://").unwrap()..].trim_end_matches(|c: char| c == ' ' || c == '│' || c == '┃').to_string();
    drag(&mut a, (x, y), (x + shown.chars().count() as u16 - 1, y));
    assert_eq!(copied(&a), shown);
    assert!(url.starts_with(&shown), "{shown}");
}

/// A confirm's error state (a `--dump` refuses the act) is selectable, and
/// the press on its text presses no button.
#[test]
fn a_confirms_error_is_copied_and_nothing_is_pressed() {
    let mut a = face();
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Mesh);
    a.focus = Focus::Content;
    a.stnav.insert("svc:example-mac/threads".into());
    frame(&mut a);
    let row = tui::cauldron::status::view(&a, 120).items.iter().position(|i| i.key == "ctl:example-mac/threads/restart").expect("the restart row");
    a.rows[0] = row;
    a.handle(key(KeyCode::Enter));
    a.handle(key(KeyCode::Enter));
    let Some(Modal::Confirm { error: Some(e), .. }) = a.modal.clone() else { panic!("the confirm carries its error") };
    let rows = frame(&mut a);
    let first = e.split_whitespace().next().unwrap();
    let last = e.split_whitespace().last().unwrap();
    let start = find(&rows, first);
    let (ex, ey) = rows.iter().enumerate().skip(start.1 as usize).find_map(|(y, l)| l.find(last).map(|b| (l[..b].chars().count() + last.chars().count() - 1, y))).unwrap();
    drag(&mut a, start, (ex as u16, ey as u16));
    assert_eq!(words(&copied(&a)), words(&e));
    assert!(matches!(a.modal, Some(Modal::Confirm { error: Some(_), .. })), "still open, its error standing");
}

/// A page's lines on the Status tab: two rows of a list, copied as two lines.
#[test]
fn a_list_on_a_page_is_copied_line_by_line() {
    let mut a = face();
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Mesh);
    a.focus = Focus::Content;
    a.stnav.insert("svc:example-mac/tts".into());
    let rows = frame(&mut a);
    let (x, y) = find(&rows, "no version of tts before");
    // What is drawn there, to the box's edge (whose border is not copied).
    let line = rows[y as usize][rows[y as usize].find("no version").unwrap()..].trim_end_matches([' ', '│']).to_string();
    drag(&mut a, (x, y), (x + line.chars().count() as u16 - 1, y));
    assert_eq!(copied(&a), line);
    // A key ends the selection's highlight; nothing else.
    a.handle(key(KeyCode::Down));
    assert!(!a.select.active());
}

/// On the Cauldron surface a page selection stays on the page's side of the
/// sidebar's divider: no sidebar text comes with it.
#[test]
fn a_page_selection_never_takes_the_sidebar() {
    let mut a = face();
    a.set_ctab(CTab::Cauldron);
    let rows = frame(&mut a);
    let (d, _) = *a.hits.iter().find(|(_, h)| matches!(h, Hit::Divider)).expect("the divider");
    let area = a.clusters_area;
    // From the page's first column, three rows down, to its far edge.
    drag(&mut a, (d.x + 1, area.y + 1), (area.right() - 1, area.y + 3));
    let text = copied(&a);
    for (y, l) in rows.iter().enumerate().skip(area.y as usize + 1).take(3) {
        let side: String = l.chars().take(d.x as usize).collect();
        let side = side.trim();
        if side.len() > 3 {
            assert!(!text.contains(side), "row {y}'s sidebar `{side}` was copied: {text:?}");
        }
    }
}

/// A click with no drag acts exactly as before and copies nothing: a
/// control's row asks, the confirm's button does it.
#[test]
fn a_click_still_acts_and_copies_nothing() {
    let mut a = face();
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Mesh);
    a.focus = Focus::Content;
    a.stnav.insert("svc:example-mac/tts".into());
    let rows = frame(&mut a);
    let (y, l) = rows.iter().enumerate().skip_while(|(_, l)| !l.contains("tts ")).find(|(_, l)| l.contains("      restart ")).expect("the restart row");
    let x = l[..l.find("restart").unwrap()].chars().count() as u16;
    click(&mut a, x, y as u16);
    assert!(matches!(a.modal, Some(Modal::Confirm { ref title, .. }) if title == "Restart tts?"));
    assert!(a.outbox.is_empty() && !a.select.active());
    // Its `no` button closes it at one click, as it always did.
    frame(&mut a);
    let (b, _) = *a.hits.iter().find(|(_, h)| matches!(h, Hit::ConfirmButton(1))).expect("the confirm's second button");
    click(&mut a, b.x, b.y);
    assert!(a.modal.is_none());
    assert!(a.outbox.is_empty());
}

/// A page item acts at the release of a click on it; a press DRAGGED into a
/// selection is the selection's: the item does not act.
#[test]
fn an_item_clicks_but_a_drag_from_it_only_selects() {
    let mut a = face();
    a.set_ctab(CTab::Cauldron);
    frame(&mut a);
    let items: Vec<(ratatui::layout::Rect, usize)> = a.hits.iter().filter_map(|(r, h)| if let Hit::Item(i) = h { Some((*r, *i)) } else { None }).collect();
    let (r, i) = *items.iter().find(|(_, i)| *i != a.item).expect("an item other than the selected one");
    let before = a.item;
    drag(&mut a, (r.x, r.y), (r.right() - 1, r.y));
    assert_eq!(a.item, before, "the drag did not act on the item: {:?}", a.notice);
    assert!(!copied(&a).is_empty());
    a.outbox.clear();
    click(&mut a, r.x, r.y);
    assert_eq!(a.item, i, "a click on it still selects it");
    assert!(a.outbox.is_empty());
}

/// The sidebar's divider still drags, and its drag selects nothing.
#[test]
fn the_divider_still_drags() {
    let mut a = face();
    a.set_ctab(CTab::Cauldron);
    frame(&mut a);
    let (d, _) = *a.hits.iter().find(|(_, h)| matches!(h, Hit::Divider)).expect("the divider");
    drag(&mut a, (d.x, d.y), (d.x + 12, d.y));
    frame(&mut a);
    let (d2, _) = *a.hits.iter().find(|(_, h)| matches!(h, Hit::Divider)).expect("the divider");
    assert!(d2.x > d.x, "the divider moved right: {} -> {}", d.x, d2.x);
    assert!(a.outbox.is_empty() && !a.select.active(), "and nothing was selected or copied");
    assert!(!a.dragging);
}

/// The `--keys` driver scripts the mouse: a `--dump` drag selects and says
/// on stderr what it would copy — and copies nothing.
#[test]
fn the_keys_driver_drags_and_a_dump_copies_nothing() {
    let root = scratch_root();
    std::fs::create_dir_all(root.join("data").join("dev")).unwrap();
    let run = |keys: Option<&str>| {
        let mut c = crate::face::Face::new(&root.with_extension("face"));
        c.arg("--cauldron").args(["--dump", "--env", "dev", "--tab", "status", "--size", "120x40"]).env("CAULDRON_HOME", &root).stdin(std::process::Stdio::null());
        if let Some(k) = keys {
            c.args(["--keys", k]);
        }
        let o = c.output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        (String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
    };
    let (out, _) = run(None);
    let rows: Vec<String> = out.lines().map(str::to_string).collect();
    // A word of the second tab bar, which every Status frame draws.
    let (x, y) = find(&rows, "Status");
    let (out, err) = run(Some(&format!("press:{x},{y} drag:{},{y} release:{},{y}", x + 3, x + 5)));
    assert!(err.contains(r#"--dump: not copied (a dump writes no clipboard): "Status""#), "{err}\n{out}");
    assert!(out.contains("sending 6 characters to the terminal's clipboard (OSC 52)"), "the notice says sent, not landed:\n{out}");
}

// ── round 2: a drag that starts on a control never acts on it ──────────
//
// Every case runs the built face through its `--keys` driver, as the
// reported repro did: the same gesture as a drag (press, drag away,
// release) must leave the control exactly as it was and copy its text; the
// same place as a click (press and release on one cell) must still act.

/// `--dump` of an empty scratch root on `tab`, after `keys` (if any): the
/// frame's rows and stderr.
fn keys_dump(root: &Path, tab: &str, keys: Option<&str>) -> (Vec<String>, String) {
    let mut c = crate::face::Face::new(&root.with_extension("face"));
    c.arg("--cauldron").args(["--dump", "--env", "dev", "--tab", tab, "--size", "120x40"]).env("CAULDRON_HOME", root).stdin(std::process::Stdio::null());
    if let Some(k) = keys {
        c.args(["--keys", k]);
    }
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    (String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn dev_root() -> PathBuf {
    let root = scratch_root();
    std::fs::create_dir_all(root.join("data").join("dev")).unwrap();
    root
}

/// A press at `at`, dragged `dx` cells right through the row, released there.
fn drag_keys(at: (u16, u16), dx: u16) -> String {
    let (x, y) = at;
    format!("press:{x},{y} drag:{},{y} drag:{},{y} release:{},{y}", x + dx / 2, x + dx, x + dx)
}

fn row_with<'a>(rows: &'a [String], needle: &str) -> &'a str {
    rows.iter().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("`{needle}` is not drawn:\n{}", rows.join("\n")))
}

/// THE REPORTED REPRO, a toggle and its options: a drag from `off` on
/// "opportunistic accounts" copied its text and switched it off. Now the drag
/// copies and the setting stays `on`; a click on `off` still switches it.
#[test]
fn n1_a_drag_from_a_toggles_option_copies_and_never_switches_it() {
    let root = dev_root();
    let (rows, _) = keys_dump(&root, "settings", None);
    let before = row_with(&rows, "opportunistic accounts").to_string();
    assert!(!before.contains('↺'), "it starts at its default: {before}");
    let (x, y) = find(&rows, " off ");
    let off = (x + 1, y);
    let (after, err) = keys_dump(&root, "settings", Some(&drag_keys(off, 26)));
    assert_eq!(row_with(&after, "opportunistic accounts"), before, "the drag changed the setting:\n{err}");
    assert!(err.contains(r#"--dump: not copied (a dump writes no clipboard): "off"#), "and it copied the text: {err}");
    let (clicked, _) = keys_dump(&root, "settings", Some(&format!("click:{},{}", off.0, off.1)));
    assert!(row_with(&clicked, "opportunistic accounts").contains('↺'), "a click on `off` still switches it");
}

/// A setting's value opens its field on a click; a drag from it only selects.
#[test]
fn n1_a_drag_from_a_settings_value_never_opens_its_field() {
    let root = dev_root();
    let (rows, _) = keys_dump(&root, "settings", None);
    let line = row_with(&rows, "sidebar").to_string();
    let y = rows.iter().position(|l| *l == line).unwrap() as u16;
    let x = line[..line.find("auto").unwrap()].chars().count() as u16;
    let (after, err) = keys_dump(&root, "settings", Some(&drag_keys((x, y), 20)));
    assert!(!row_with(&after, "sidebar").contains('▏'), "the drag opened the field:\n{}", after.join("\n"));
    assert!(err.contains("not copied"), "{err}");
    let (clicked, _) = keys_dump(&root, "settings", Some(&format!("click:{x},{y}")));
    assert!(row_with(&clicked, "sidebar").contains("› auto▏"), "a click still opens it");
}

/// A row that expands on a click (a pool on Accounts) does not on a drag.
#[test]
fn n1_a_drag_from_a_row_never_opens_it() {
    let root = dev_root();
    let (rows, _) = keys_dump(&root, "accounts", None);
    let (x, y) = find(&rows, "Claude Code");
    let (after, err) = keys_dump(&root, "accounts", Some(&drag_keys((x, y), 10)));
    assert!(row_with(&after, "Claude Code").contains('▸') && !after.iter().any(|l| l.contains("+ add account")), "the drag opened the row:\n{}", after.join("\n"));
    assert!(err.contains(r#""Claude Code""#), "{err}");
    let (clicked, _) = keys_dump(&root, "accounts", Some(&format!("click:{x},{y}")));
    assert!(clicked.iter().any(|l| l.contains("+ add account")), "a click still opens it");
}

/// A tab in the tab bar switches on a click, never on a drag.
#[test]
fn n1_a_drag_from_a_tab_never_switches_to_it() {
    let root = dev_root();
    let (rows, _) = keys_dump(&root, "settings", None);
    let (x, y) = find(&rows, "Status");
    let (after, err) = keys_dump(&root, "settings", Some(&drag_keys((x, y), 4)));
    assert!(after.iter().any(|l| l.contains("opportunistic accounts")), "the drag switched tabs:\n{}", after.join("\n"));
    assert!(err.contains(r#""Statu"#), "{err}");
    let (clicked, _) = keys_dump(&root, "settings", Some(&format!("click:{x},{y}")));
    assert!(!clicked.iter().any(|l| l.contains("opportunistic accounts")), "a click still switches");
}

/// A modal's button acts on a click, never on a drag that starts on it.
#[test]
fn n1_a_drag_from_a_modals_button_never_presses_it() {
    let root = dev_root();
    let (rows, _) = keys_dump(&root, "accounts", None);
    let (px, py) = find(&rows, "Claude Code");
    let (opened, _) = keys_dump(&root, "accounts", Some(&format!("click:{px},{py}")));
    let (ax, ay) = find(&opened, "+ add account");
    let open = format!("click:{px},{py} click:{},{ay}", ax + 2);
    let (modal, _) = keys_dump(&root, "accounts", Some(&open));
    assert!(modal.iter().any(|l| l.contains("Add an account: its email address")), "{}", modal.join("\n"));
    let (cx, cy) = find(&modal, "cancel");
    let (after, err) = keys_dump(&root, "accounts", Some(&format!("{open} {}", drag_keys((cx, cy), 6))));
    assert!(after.iter().any(|l| l.contains("Add an account: its email address")), "the drag pressed `cancel`:\n{}", after.join("\n"));
    assert!(err.contains(r#""cancel"#), "{err}");
    let (clicked, _) = keys_dump(&root, "accounts", Some(&format!("{open} click:{cx},{cy}")));
    assert!(!clicked.iter().any(|l| l.contains("Add an account: its email address")), "a click on `cancel` still closes it");
}

/// The sidebar's divider keeps its drag: a press on its grabber drags it
/// at once, and selects nothing.
#[test]
fn n1_the_divider_still_drags_through_the_keys_driver() {
    let root = dev_root();
    let (rows, _) = keys_dump(&root, "cauldron", None);
    let (gx, gy) = find(&rows, "⣿");
    let (after, err) = keys_dump(&root, "cauldron", Some(&drag_keys((gx, gy), 10)));
    let (gx2, _) = find(&after, "⣿");
    assert!(gx2 > gx, "the divider moved right: {gx} -> {gx2}");
    assert!(!err.contains("not copied"), "and nothing was selected: {err}");
}
