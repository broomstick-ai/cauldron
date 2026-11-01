//! Task 0253 (b964d571 msg 23, his UI asks spoken in the sidecar's thread):
//! the `|` between menu items, the cauldron's Config tab first, inherit on
//! `allowed here`, Tokens first on Status, the sidebar's toggle and the
//! whole-cauldron root, the bucket's copy at its bottom left, Human and the
//! new Tasks tab, and a click that replaces the reference at the caret. Real
//! seed files, real events, real frames, scratch roots.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, KTab};
use tui::cauldron::render_to_text;
use tui::cauldron::store::seed_dir;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0253-{}-{}-{}",
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

fn app() -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    // V1.6: Claude Code's accounts are the daemon's; the fixture stands in.
    crate::live_pool::seeded(App::open(env_in(&scratch_root(), "prod"), seed_dir(), true).expect("opens"))
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn press(a: &mut App, code: KeyCode) {
    a.handle(key(code));
}

fn click(a: &mut App, x: u16, y: u16) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
    }
}

const W: u16 = 140;
const H: u16 = 50;

fn rows(a: &mut App) -> Vec<String> {
    render_to_text(a, W, H).unwrap()
}

/// The cell column of `needle` on `row` (every glyph here is one cell wide).
fn col(row: &str, needle: &str) -> u16 {
    let b = row.find(needle).unwrap_or_else(|| panic!("`{needle}` not on `{row}`"));
    row[..b].chars().count() as u16
}

/// The row holding `needle`, and its index.
fn row_with(rows: &[String], needle: &str) -> (usize, String) {
    rows.iter().enumerate().find(|(_, r)| r.contains(needle)).map(|(i, r)| (i, r.clone())).unwrap_or_else(|| panic!("`{needle}` on no row:\n{}", rows.join("\n")))
}

/// Click the label `label` on the row holding `on`, after a fresh frame.
fn click_label(a: &mut App, on: &str, label: &str) {
    let r = rows(a);
    let (y, row) = row_with(&r, on);
    let x = col(&row, label) + 1;
    click(a, x, y as u16);
}

// ── 1: a `|` between menu items ────────────────────────────────────────────

#[test]
fn item1_every_strip_parts_its_items_with_a_bar_and_every_label_still_clicks() {
    let mut a = app();
    let r = rows(&mut a);
    // The top tabs: the environment bar, and the global bar over it (V1.1).
    let (y, top) = row_with(&r, " | Threads ");
    assert!(top.contains("Cauldron | Threads | Status | Permissions | Settings"), "{top}");
    assert_eq!(y, 1);
    assert!(r[0].contains("TUI | Devices | Providers | Accounts"), "{}", r[0]);
    // The cluster tabs.
    let (_, kt) = row_with(&r, " | Actions ");
    assert!(kt.contains(" | Details"), "{kt}");
    // Config is the global bar now (V1.1): every label on it is still its
    // own hotspot, past the bars, and so is each settings tab of the
    // environment bar.
    a.set_ctab(CTab::Settings);
    for (on, label, want) in [("Devices", "Providers", tui::cauldron::menu::CfgTab::Providers), ("Devices", "TUI", tui::cauldron::menu::CfgTab::Tui), (" | Threads ", "Permissions", tui::cauldron::menu::CfgTab::Permissions), (" | Threads ", "Settings", tui::cauldron::menu::CfgTab::Cauldron)] {
        click_label(&mut a, on, label);
        assert_eq!(a.cfg_tab, want, "a click on {label}");
    }
    // Status.
    a.set_ctab(CTab::Status);
    let r = rows(&mut a);
    let (_, st) = row_with(&r, " | Soup | ");
    assert!(st.contains(" | Soup | Bus"), "{st}");
    for (label, want) in [("Bus", tui::cauldron::status::StatusTab::Bus), ("Mesh", tui::cauldron::status::StatusTab::Mesh), ("Tokens", tui::cauldron::status::StatusTab::Tokens)] {
        click_label(&mut a, " | Soup | ", label);
        assert_eq!(a.sttab, want, "a click on {label}");
    }
}

#[test]
fn item1_cluster_tabs_still_fit_a_narrow_page_with_the_bars() {
    let mut a = app();
    for w in [60u16, 72, 80, 100] {
        let r = render_to_text(&mut a, w, 40).unwrap();
        let (_, kt) = row_with(&r, " | Act");
        // The last tab, Details since 8e4fca10 msg 385, is never cut off.
        assert!(kt.contains("Det"), "width {w}: {kt}");
    }
}

// ── 2: the cauldron's own Config tab is the first ──────────────────────────

#[test]
fn item2_the_cauldrons_config_tab_is_first_and_config_opens_on_it() {
    use tui::cauldron::menu::CfgTab;
    assert_eq!(CfgTab::ALL[0], CfgTab::Cauldron);
    let mut a = app();
    // V1.1: the cauldron's own page is the environment bar's Settings, and
    // ⇧Tab from it is the global bar's first, TUI.
    a.set_ctab(CTab::Settings);
    assert_eq!(a.cfg_tab, CfgTab::Cauldron, "Settings is the cauldron's page");
    let r = rows(&mut a);
    // The cauldron's own rows are what is shown under it: its switch first.
    assert!(r[3].contains("cauldron enabled"), "{}", r.join("\n"));
    assert!(!r.iter().any(|l| l.contains("prod cauldron |")), "no sub-tab strip: {}", r.join("\n"));
    a.focus = tui::cauldron::app::Focus::Content;
    press(&mut a, KeyCode::BackTab);
    assert_eq!((a.ctab, a.cfg_tab), (CTab::Tui, CfgTab::Tui));
    // A --config-tab path still names each by its slug.
    assert_eq!(CfgTab::parse("cauldron"), Some(CfgTab::Cauldron));
    assert_eq!(CfgTab::parse("tui"), Some(CfgTab::Tui));
}

// ── 3: `allowed here` inherits ─────────────────────────────────────────────

#[test]
fn item3_allowed_here_is_a_tri_state_that_inherits_the_accounts_own_switch() {
    use tui::cauldron::menu::{is_tri, Bound};
    use tui::cauldron::settings::{Ctx, Val};
    let mut a = app();
    let acct = a.accounts("claude_code").into_iter().next().expect("an account").id;
    let allowed = Bound::new("cauldron.accounts.{pool}.{account}.allowed", Ctx::account("claude_code", &acct));
    let enabled = Bound::new("pools.{pool}.accounts.{account}.enabled", Ctx::account("claude_code", &acct));
    assert!(is_tri(&allowed), "inherit · on · off");
    let t = a.toggle_of(&allowed);
    assert_eq!(t.options, &["inherit", "on", "off"]);
    assert_eq!(t.selected, 0, "unset, it inherits");
    // Inheriting, it follows the account's own switch, both ways.
    assert!(a.account_allowed("claude_code", &acct));
    a.sset(&enabled, Val::Bool(false)).unwrap();
    assert!(!a.account_allowed("claude_code", &acct), "inherits the account switched off");
    a.sset(&enabled, Val::Bool(true)).unwrap();
    assert!(a.account_allowed("claude_code", &acct));
    // `off` here holds against the account on; `on` here holds against it off.
    a.sset(&allowed, Val::Bool(false)).unwrap();
    assert_eq!(a.toggle_of(&allowed).selected, 2);
    assert!(!a.account_allowed("claude_code", &acct));
    a.sset(&allowed, Val::Bool(true)).unwrap();
    a.sset(&enabled, Val::Bool(false)).unwrap();
    assert_eq!(a.toggle_of(&allowed).selected, 1, "`on` set here is kept, though it is what it would inherit");
    assert!(a.account_allowed("claude_code", &acct));
    // And inherit again.
    a.sclear(&allowed).unwrap();
    assert_eq!(a.toggle_of(&allowed).selected, 0);
    assert!(!a.account_allowed("claude_code", &acct));
    // The cauldron's tab draws it: the accounts table's `allowed here` column.
    a.sset(&enabled, Val::Bool(true)).unwrap();
    a.set_ctab(CTab::Settings);
    a.snav.set_open(tui::cauldron::menu::SPage::Use, true);
    let r = rows(&mut a);
    let (_, head) = row_with(&r, "allowed here");
    assert!(head.contains("allowed here"), "{head}");
    // An account row's cell says it inherits, not a bare on.
    let label = a.accounts("claude_code").into_iter().next().unwrap().label;
    let (_, row) = row_with(&r, &label);
    assert!(row.contains("inherit"), "{row}\n{}", r.join("\n"));
}

// ── 4: Tokens was the first Status sub-tab; the Ledger is now ──────────────

#[test]
fn item4_tokens_is_the_first_status_sub_tab_and_status_opens_on_it() {
    // 8e4fca10 msg 457: the Ledger goes before Tokens; Tokens keeps its
    // place ahead of the rest.
    use tui::cauldron::status::StatusTab;
    assert_eq!(StatusTab::ALL[..2], [StatusTab::Ledger, StatusTab::Tokens]);
    let mut a = app();
    a.set_ctab(CTab::Status);
    assert_eq!(a.sttab, StatusTab::Ledger);
    let r = rows(&mut a);
    let (_, st) = row_with(&r, " | Soup | ");
    assert!(st.contains(" Ledger | Tokens | Mesh | Soup | Bus"), "{st}");
    assert!(st.trim_start_matches(['│', ' ']).starts_with("Ledger"), "{st}");
    a.focus = tui::cauldron::app::Focus::Content;
    press(&mut a, KeyCode::Right);
    assert_eq!(a.sttab, StatusTab::Tokens);
    press(&mut a, KeyCode::Right);
    assert_eq!(a.sttab, StatusTab::Mesh);
    press(&mut a, KeyCode::Left);
    assert_eq!(a.sttab, StatusTab::Tokens);
}

// ── 6: the bucket's copy at its bottom left ────────────────────────────────

#[test]
fn item6_the_buckets_copy_is_at_its_bottom_left_and_copies_at_one_click() {
    use tui::cauldron::app::Outbox;
    let mut a = app();
    a.focus_input();
    a.type_text("what he wrote");
    let r = rows(&mut a);
    let top = r.iter().rposition(|l| l.trim_end().ends_with("bucket ╮")).expect("the bucket's top border");
    assert!(!r[top].contains("⧉ copy"), "no longer at the top: {}", r[top]);
    let bottom = r.len() - 1;
    // 8e4fca10 msg 457 moved it to the bottom RIGHT, set into the border.
    assert!(r[bottom].ends_with("┤ ⧉ copy ├╯"), "at its bottom right: {:?}", r[bottom]);
    // What was said sits at the bottom right and never covers it.
    a.notice = Some("a very long notice ".repeat(20));
    let r = rows(&mut a);
    assert!(r[bottom].ends_with("┤ ⧉ copy ├╯"), "{:?}", r[bottom]);
    assert!(r[bottom].contains("a very long notice"), "{:?}", r[bottom]);
    // One click on it, where it is drawn, copies the bucket.
    a.notice = None;
    let _ = rows(&mut a);
    let copy_x = (W as usize - 6) as u16;
    click(&mut a, copy_x, bottom as u16);
    match a.outbox.last() {
        Some(Outbox::Copy(t)) => assert!(t.contains("what he wrote"), "{t}"),
        other => panic!("not copied: {other:?}"),
    }
    // Its old place at the bottom left copies nothing (msg 457).
    let n = a.outbox.len();
    click(&mut a, 4, bottom as u16);
    assert_eq!(a.outbox.len(), n, "the bottom left copies nothing");
    // The top-left corner is no longer a button: a click there copies nothing.
    let n = a.outbox.len();
    click(&mut a, 4, top as u16);
    assert_eq!(a.outbox.len(), n, "the old place copies nothing");
    assert_eq!(a.composer.text(), "what he wrote", "nothing he typed is lost");
}

// ── 7: Human keeps the count; Tasks is the DAG ─────────────────────────────

#[test]
fn item7_human_is_the_old_tasks_with_its_count_and_tasks_is_the_execution_graph() {
    use tui::cauldron::app::{Band, Item};
    // 8e4fca10 msg 385 flattened Human into Questions and Actions.
    assert_eq!(KTab::ALL, [KTab::Questions, KTab::Actions, KTab::Tasks, KTab::Nodes, KTab::Status, KTab::Details]);
    assert_eq!(KTab::parse("human"), None);
    assert_eq!(KTab::parse("questions"), Some(KTab::Questions));
    assert_eq!(KTab::parse("actions"), Some(KTab::Actions));
    assert_eq!(KTab::parse("details"), Some(KTab::Details));
    assert_eq!(KTab::parse("overview"), None, "8e4fca10 msg 385: Overview is Details, no alias");
    assert_eq!(KTab::parse("tasks"), Some(KTab::Tasks));
    let mut a = app();
    let x1 = a.forest.find("X-1").unwrap();
    a.select(x1);
    let (nq, na) = (a.active_questions(), a.active_actions());
    assert!(nq + na > 0, "the seed waits on him");
    assert_eq!(a.ktab_title(KTab::Questions), format!("Questions {nq}"), "Questions carries its count");
    assert_eq!(a.ktab_title(KTab::Actions), format!("Actions {na}"), "Actions carries its count");
    assert_eq!(a.ktab_title(KTab::Tasks), "Tasks", "Tasks carries none");
    let r = rows(&mut a);
    let (y, kt) = row_with(&r, " | Actions ");
    assert!(kt.contains(&format!("Questions {nq} | Actions {na} | Tasks | Nodes | Status | Details ")), "{kt}");
    // Each label clicks to its own tab.
    for (label, want) in [("Tasks", KTab::Tasks), ("Questions", KTab::Questions), ("Actions", KTab::Actions), ("Status", KTab::Status)] {
        let r = rows(&mut a);
        let x = col(&r[y], &format!(" {label}")) + 2;
        click(&mut a, x, y as u16);
        assert_eq!(a.ktab, want, "a click on {label}");
    }
    // Human holds what waits on him; Tasks holds the Engine; Status no longer does.
    a.set_ktab(KTab::Questions);
    assert!(a.items().iter().any(|i| matches!(i, Item::Question { .. })), "{:?}", a.items());
    a.set_ktab(KTab::Tasks);
    let items = a.items();
    assert!(items.contains(&Item::Band(Band::Engine)), "{items:?}");
    assert!(items.iter().any(|i| matches!(i, Item::Task { .. })), "the DAG's tasks: {items:?}");
    let page = rows(&mut a).join("\n");
    assert!(page.contains("Engine") && page.contains("T-1"), "{page}");
    a.set_ktab(KTab::Status);
    assert!(!a.items().contains(&Item::Band(Band::Engine)), "the Engine moved off Status");
    assert!(a.items().contains(&Item::Band(Band::Ripples)), "Status keeps its ripples");
    // Tab walks all six, in order, and comes round.
    a.set_ktab(KTab::Questions);
    a.focus = tui::cauldron::app::Focus::Content;
    let mut seen = vec![a.ktab];
    for _ in 0..6 {
        press(&mut a, KeyCode::Tab);
        seen.push(a.ktab);
    }
    assert_eq!(seen, [KTab::Questions, KTab::Actions, KTab::Tasks, KTab::Nodes, KTab::Status, KTab::Details, KTab::Questions]);
}

// ── 8: a click replaces the reference at the caret ─────────────────────────

fn press_hit(a: &mut App, hit: tui::cauldron::app::Hit) {
    let _ = rows(a);
    let r = a.hits.iter().rev().find(|(_, h)| *h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {hit:?} on screen"));
    click(a, r.x, r.y);
}

/// The second press of a double click: its frame drawn first, so a loaded
/// machine's slow frame never turns it into two single clicks.
fn second_press(a: &mut App, hit: tui::cauldron::app::Hit) {
    let _ = rows(a);
    let r = a.hits.iter().rev().find(|(_, h)| *h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {hit:?} on screen"));
    a.last_click = Some((std::time::Instant::now(), hit));
    click(a, r.x, r.y);
}

/// Two questions' item indexes on X-1's Human tab, the page holding them.
fn two_questions(a: &mut App) -> (usize, usize) {
    use tui::cauldron::app::Item;
    let x1 = a.forest.find("X-1").unwrap();
    a.select(x1);
    a.set_ktab(KTab::Questions);
    let qs: Vec<usize> = a.items().iter().enumerate().filter(|(_, i)| matches!(i, Item::Question { .. })).map(|(i, _)| i).collect();
    assert!(qs.len() >= 2, "{:?}", a.items());
    (qs[0], qs[1])
}

fn tag_of(a: &App, i: usize) -> String {
    a.tag_for(a.items()[i]).unwrap().render()
}

#[test]
fn item8_a_click_then_another_replaces_the_reference_at_the_caret() {
    use tui::cauldron::app::Hit;
    let mut a = app();
    let (q1, q2) = two_questions(&mut a);
    let (t1, t2) = (tag_of(&a, q1), tag_of(&a, q2));
    assert_ne!(t1, t2);
    a.type_text("see ");
    press_hit(&mut a, Hit::Item(q1));
    assert_eq!(a.composer.text(), format!("see {t1} "));
    // Another item, at once: it REPLACES the one at the caret.
    press_hit(&mut a, Hit::Item(q2));
    assert_eq!(a.composer.text(), format!("see {t2} "), "replaced, not added");
    // And back again: still one.
    press_hit(&mut a, Hit::Item(q1));
    assert_eq!(a.composer.text(), format!("see {t1} "));
    // He types a space to keep it: the next one goes beside it.
    a.composer.insert_char(' ');
    press_hit(&mut a, Hit::Item(q2));
    assert_eq!(a.composer.text(), format!("see {t1}  {t2} "), "a space he typed keeps several");
    // Typed text after a reference: the reference is no longer at the caret.
    a.type_text("and then");
    press_hit(&mut a, Hit::Item(q1));
    assert_eq!(a.composer.text(), format!("see {t1}  {t2} and then {t1} "), "nothing he typed is lost");
    // The caret moved back INTO a reference: that one is replaced, the rest stays.
    let mut b = app();
    let (q1, q2) = two_questions(&mut b);
    b.type_text(&format!("x {t1} y"));
    for _ in 0.." y".len() + 2 {
        b.composer.left();
    }
    press_hit(&mut b, Hit::Item(q2));
    assert_eq!(b.composer.text(), format!("x {t2} y"));
    // The replace is one undo step.
    b.composer.undo();
    assert_eq!(b.composer.text(), format!("x {t1} y"));
    let _ = q1;
}

#[test]
fn item8_a_paste_chip_at_the_caret_is_never_replaced() {
    use tui::cauldron::app::Hit;
    let mut a = app();
    let (q1, _) = two_questions(&mut a);
    a.focus_input();
    a.handle(Event::Paste("one\ntwo\nthree\nfour".into()));
    let pasted = a.composer.text();
    assert!(pasted.contains("[paste "), "a paste chip: {pasted}");
    a.focus = tui::cauldron::app::Focus::Content;
    press_hit(&mut a, Hit::Item(q1));
    let t1 = tag_of(&a, q1);
    assert!(a.composer.text().starts_with(pasted.trim_end()), "the paste stays: {}", a.composer.text());
    assert!(a.composer.text().ends_with(&format!("{t1} ")), "{}", a.composer.text());
}

#[test]
fn item8_a_double_click_clears_the_reference_at_the_caret_then_quotes() {
    use tui::cauldron::app::Hit;
    let mut a = app();
    let (q1, q2) = two_questions(&mut a);
    let (t1, t2) = (tag_of(&a, q1), tag_of(&a, q2));
    a.type_text("note ");
    press_hit(&mut a, Hit::Item(q1));
    assert_eq!(a.composer.text(), format!("note {t1} "));
    // A double click on another: its first press replaced Q-1 with it; the
    // second takes that back, takes out the reference the click just put at
    // the caret (Q-1), and quotes the item on a new line.
    a.last_click = None;
    press_hit(&mut a, Hit::Item(q2));
    second_press(&mut a, Hit::Item(q2));
    assert_eq!(a.composer.text(), format!("note\n\n> {t2}"), "{:?}", a.composer.text());
    // With nothing at the caret, a double click only quotes (as before).
    let mut b = app();
    let (q1, _) = two_questions(&mut b);
    b.type_text("words he wrote");
    press_hit(&mut b, Hit::Item(q1));
    second_press(&mut b, Hit::Item(q1));
    assert_eq!(b.composer.text(), format!("words he wrote\n\n> {t1}"));
}

// ── 5: the sidebar's switch, and the whole cauldron ────────────────────────

/// The sidebar column of every screen row (left of its divider), trimmed.
fn sidebar(r: &[String]) -> Vec<String> {
    r.iter().map(|l| l.split('│').nth(1).unwrap_or("").trim_end().to_string()).collect()
}

fn key_of(a: &App) -> String {
    a.forest.get(a.selected).key.clone()
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        press(a, KeyCode::Char(c));
    }
}

#[test]
fn item5_the_switch_at_the_sidebars_foot_brings_the_whole_cauldron_as_every_clusters_root() {
    use tui::cauldron::app::{Hit, Item};
    let mut a = app();
    assert!(!a.aggregate && a.forest.whole().is_none(), "off by default, no root");
    let r = rows(&mut a);
    let side = sidebar(&r);
    let (y, foot) = side.iter().enumerate().rev().find(|(_, l)| l.contains("Aggregated clusters")).map(|(i, l)| (i, l.clone())).expect("the switch");
    assert!(foot.ends_with(" on  off"), "{foot}");
    assert!(side[y - 1].trim().chars().all(|c| c == '─'), "a line above it: {}", side[y - 1]);
    // The bucket's box is below the soup's: the switch is on the soup box's last inner row.
    assert!(r[y + 1].starts_with('╰') || r[y + 1].starts_with('├'), "{}", r[y + 1]);
    let tops: Vec<String> = a.forest.clusters.iter().filter(|g| g.depth == 0).map(|g| g.key.clone()).collect();
    let n = a.forest.clusters.len();
    let x9 = a.forest.find("X-9").unwrap();
    a.select(x9);
    // Its `on`, clicked where it is drawn.
    press_hit(&mut a, Hit::BelowOpt(true));
    assert!(a.aggregate, "on");
    let w = a.forest.whole().expect("the whole cauldron");
    assert_eq!(w, 0);
    assert_eq!(a.forest.get(w).name, "prod cauldron");
    assert_eq!(a.forest.clusters.len(), n + 1);
    assert!(a.forest.clusters[1..].iter().all(|g| g.depth >= 1), "every cluster under it");
    let under: Vec<String> = a.forest.get(w).children.iter().map(|&g| a.forest.get(g).key.clone()).collect();
    assert_eq!(under, tops, "its children are the roots, in order");
    assert_eq!(key_of(&a), "X-9", "the selection stays, by key");
    let side = sidebar(&rows(&mut a));
    let first = side.iter().position(|l| l.starts_with('─')).map(|i| &side[i + 1]).unwrap();
    assert!(first.contains("▾ prod cauldron"), "the first row of the tree: {side:?}");
    assert!(side.iter().any(|l| l.starts_with("   ▾ Cauldron")), "the old roots one level in: {side:?}");
    // The whole cauldron's page holds every question in the soup.
    a.select(w);
    a.set_ktab(KTab::Questions);
    let all: usize = (0..a.forest.clusters.len()).map(|g| a.forest.get(g).questions.len()).sum();
    let shown = a.items().iter().filter(|i| matches!(i, Item::Question { .. })).count();
    assert!(all > 3 && shown == all, "every question in the soup: {shown} of {all}");
    assert!(a.ktab_title(KTab::Questions).ends_with(&format!(" {}", a.active_questions())));
    assert!(a.ktab_title(KTab::Actions).ends_with(&format!(" {}", a.active_actions())));
    // Its Tasks tab is the whole soup's graph, each cluster its own band.
    a.set_ktab(KTab::Tasks);
    let s = rows(&mut a).join("\n");
    assert!(s.contains("Engine"), "{s}");
    // Off, by its `off`: the root goes; the selection on it falls to the first cluster.
    press_hit(&mut a, Hit::BelowOpt(false));
    assert!(!a.aggregate && a.forest.whole().is_none());
    assert_eq!(a.forest.clusters.len(), n);
    assert_eq!(a.selected, 0);
    // `on` when on and `off` when off do nothing.
    press_hit(&mut a, Hit::BelowOpt(false));
    assert!(!a.aggregate && a.forest.whole().is_none());
    // Its name flips it.
    press_hit(&mut a, Hit::Below);
    assert!(a.aggregate && a.forest.whole().is_some());
    press_hit(&mut a, Hit::Below);
    assert!(!a.aggregate && a.forest.whole().is_none());
    // ^G is the same switch.
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)));
    assert!(a.aggregate && a.forest.whole().is_some());
    let side = sidebar(&rows(&mut a));
    assert!(side.iter().any(|l| l.contains("Aggregated clusters") && l.ends_with(" on  off")), "{side:?}");
}

#[test]
fn item5_the_switch_is_a_row_of_the_sidebar_by_keys() {
    let mut a = app();
    a.focus = tui::cauldron::app::Focus::Tree;
    press(&mut a, KeyCode::End);
    let last = a.selected;
    press(&mut a, KeyCode::Down);
    assert!(a.on_below, "the row after the last cluster");
    assert_eq!(a.focus, tui::cauldron::app::Focus::Tree);
    // Its row carries the selection, and no cluster does.
    let (r, sel) = tui::cauldron::render_with_selection(&mut a, W, H).unwrap();
    let side = sidebar(&r);
    let lit: Vec<&String> = sel.iter().map(|&y| &side[y]).filter(|l| !l.trim().is_empty()).collect();
    assert!(lit.iter().any(|l| l.contains("Aggregated clusters")), "{lit:?}");
    let last_name = a.forest.get(last).name.clone();
    assert!(!lit.iter().any(|l| l.contains(&last_name)), "no cluster carries it: {lit:?}");
    press(&mut a, KeyCode::Char(' '));
    assert!(a.aggregate && a.forest.whole().is_some(), "Space flips it");
    assert!(a.on_below, "and the cursor stays on it");
    press(&mut a, KeyCode::Enter);
    assert!(!a.aggregate && a.forest.whole().is_none(), "Enter flips it");
    press(&mut a, KeyCode::Right);
    assert!(a.aggregate, "→ flips it");
    press(&mut a, KeyCode::Left);
    assert!(a.aggregate && a.on_below, "← does nothing on it");
    // ↑ goes back to the last cluster; ↓ past the switch, into the bucket.
    press(&mut a, KeyCode::Up);
    assert!(!a.on_below);
    let k = a.forest.get(a.selected).key.clone();
    a.set_aggregate(false);
    assert_eq!(a.forest.find(&k), Some(last), "the last cluster again");
    a.select(last);
    press(&mut a, KeyCode::Down);
    press(&mut a, KeyCode::Down);
    assert_eq!(a.focus, tui::cauldron::app::Focus::Input, "past the switch is the bucket");
    // `+` on it makes nothing under a cluster it is not.
    a.focus = tui::cauldron::app::Focus::Tree;
    press(&mut a, KeyCode::Up);
    assert!(a.on_below || a.focus == tui::cauldron::app::Focus::Tree);
}

#[test]
fn item5_the_whole_cauldron_is_never_renamed_moved_deleted_or_a_parent_on_disk() {
    use tui::cauldron::app::{Item, Modal};
    use tui::cauldron::manage::Act;
    let mut a = app();
    a.set_aggregate(true);
    let w = a.forest.whole().unwrap();
    a.select(w);
    a.set_ktab(KTab::Details);
    assert!(!a.items().iter().any(|i| matches!(i, Item::Manage(_))), "no rename, move or delete: {:?}", a.items());
    assert!(!a.made_here(&a.forest.get(w).key.clone()));
    // `+` beside it makes a cluster at the ROOT.
    a.focus = tui::cauldron::app::Focus::Tree;
    press(&mut a, KeyCode::Char('+'));
    assert!(matches!(&a.modal, Some(Modal::NewCluster { parent: None, .. })), "{:?}", a.modal);
    typed(&mut a, "Fresh");
    press(&mut a, KeyCode::Down);
    typed(&mut a, "what it holds");
    press(&mut a, KeyCode::Enter);
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert!(a.wait_seeding(std::time::Duration::from_secs(10)));
    let fresh = (0..a.forest.clusters.len()).find(|&g| a.forest.get(g).name == "Fresh").expect("made");
    assert_eq!(a.forest.get(fresh).parent, Some(w), "shown under the whole cauldron");
    assert_eq!(a.forest.parent_key(fresh), None, "stored at the root");
    // A name a root sibling has is refused while the whole cauldron is shown.
    a.select(w);
    press(&mut a, KeyCode::Char('+'));
    typed(&mut a, "Fresh");
    press(&mut a, KeyCode::Down);
    typed(&mut a, "again");
    press(&mut a, KeyCode::Enter);
    assert!(matches!(&a.modal, Some(Modal::NewCluster { error: Some(_), .. })), "a sibling's name: {:?}", a.modal);
    a.modal = None;
    // Moving it: the whole cauldron is not offered; `the root` is, and is
    // where it is now.
    let k = a.forest.get(fresh).key.clone();
    a.start_act(Act::MoveCluster(k.clone()));
    match &a.modal {
        Some(Modal::Picker { options, selected, .. }) => {
            assert_eq!(options[0], "the root");
            assert!(!options.iter().any(|o| o.starts_with("prod cauldron")), "{options:?}");
            assert_eq!(*selected, 0, "now at the root");
        }
        other => panic!("{other:?}"),
    }
    a.modal = None;
    // Off: it is a root cluster, as stored.
    a.set_aggregate(false);
    let fresh = a.forest.find(&k).unwrap();
    assert_eq!(a.forest.get(fresh).parent, None);
    assert_eq!(a.forest.get(fresh).depth, 0);
    // A path never names the whole cauldron.
    a.set_aggregate(true);
    let fresh = a.forest.find(&k).unwrap();
    assert_eq!(a.forest.path(fresh), "Fresh");
}

#[test]
fn item5_sections_and_a_narrow_sidebar_under_the_whole_cauldron() {
    let mut a = app();
    a.set_aggregate(true);
    a.focus = tui::cauldron::app::Focus::Tree;
    let x1 = a.forest.find("X-1").unwrap();
    a.select(x1);
    // ⌥↓ walks the sections — the whole cauldron's children.
    let alt = |a: &mut App, c: KeyCode| a.handle(Event::Key(KeyEvent::new(c, KeyModifiers::ALT)));
    alt(&mut a, KeyCode::Down);
    assert_eq!(key_of(&a), "X-6");
    alt(&mut a, KeyCode::Down);
    assert_eq!(key_of(&a), "X-9");
    alt(&mut a, KeyCode::Up);
    assert_eq!(key_of(&a), "X-6");
    // Every width and height draws it, never past the sidebar.
    for (w, h) in [(40u16, 12u16), (60, 8), (80, 5), (100, 30), (140, 50)] {
        let r = render_to_text(&mut a, w, h).unwrap();
        assert_eq!(r.len(), h as usize);
    }
}

// ── the sweeps: legends, every band, scroll — over what msg 23 added ───────

/// Every cluster tab of every cluster — the whole cauldron first — with
/// `Aggregated clusters` on, every band shut and open: no legend, no label, no
/// empty-state sentence (`cauldron_render::assert_clean`), and no row grows
/// when it is selected.
#[test]
fn sweep_every_tab_of_the_whole_cauldron_and_the_new_tasks_tab() {
    let mut a = app();
    a.set_aggregate(true);
    assert!(a.forest.whole().is_some());
    let mut swept = 0;
    for g in 0..a.forest.clusters.len() {
        for t in KTab::ALL {
            for open in [false, true] {
                a.select(g);
                a.set_ktab(t);
                crate::cauldron_render::all_bands_open(&mut a, open);
                let what = format!("{} {t:?} open={open}", a.forest.get(g).key);
                let s = render_to_text(&mut a, 120, 110).unwrap().join("\n");
                crate::cauldron_render::assert_clean(&s, &what);
                // Scroll: selecting any item never grows a row — the page
                // keeps its lines, each its width (an artifact's age may
                // tick while the sweep runs: `55m` → `56m`).
                let shape = |a: &App| tui::cauldron::pages::page(a, 100).lines.iter().map(|l| l.width()).collect::<Vec<_>>();
                let first = shape(&a);
                a.focus = tui::cauldron::app::Focus::Content;
                for i in 0..a.items().len().min(60) {
                    a.item = i;
                    assert_eq!(shape(&a), first, "{what}: item {i} changed the page's shape");
                }
                swept += 1;
            }
        }
    }
    assert!(swept > 100, "{swept}");
    // The narrow faces draw too.
    for (w, h) in [(50u16, 20u16), (70, 12), (90, 6)] {
        for t in KTab::ALL {
            a.set_ktab(t);
            assert_eq!(render_to_text(&mut a, w, h).unwrap().len(), h as usize);
        }
    }
}


// ── the adversarial pass ───────────────────────────────────────────────────

#[test]
fn adversarial_the_whole_cauldron_follows_a_switch_of_cauldron_and_a_dump_tagged_with_it_lands_on_the_soup() {
    let root = scratch_root();
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env_in(&root, "prod"), seed_dir(), true).unwrap();
    std::fs::create_dir_all(root.join("data/dev")).unwrap();
    a.set_aggregate(true);
    a.switch_env("dev").unwrap();
    assert!(a.aggregate);
    let w = a.forest.whole().expect("the whole cauldron came along");
    assert_eq!(a.forest.get(w).name, "dev cauldron");
    a.switch_env("prod").unwrap();
    assert_eq!(a.forest.get(a.forest.whole().unwrap()).name, "prod cauldron");
    // `[X-0]` names the soup: it lands on no one cluster.
    let t = tui::cauldron::tags::Tag::thing("cluster", "X-0");
    assert!(a.clusters_of(&t).is_empty());
    assert_eq!(a.clusters_of(&tui::cauldron::tags::Tag::thing("cluster", "X-1")), vec!["X-1".to_string()]);
    // Nothing of the root is ever written: the made-clusters file never names it.
    a.select(w);
    a.focus_input();
    a.type_text("a thought sent from the whole cauldron");
    press(&mut a, KeyCode::Enter);
    assert!(a.composer.is_blank(), "{:?} {:?}", a.error, a.notice);
    let dir = tui::cauldron::store::soup_dir(&env_in(&root, "prod")).unwrap();
    for f in ["clusters.json", "created.json"] {
        if let Ok(t) = std::fs::read_to_string(dir.join(f)) {
            assert!(!t.contains("\"X-0\""), "{f}: {t}");
        }
    }
}

#[test]
fn adversarial_nothing_he_types_is_lost_across_every_new_control() {
    use tui::cauldron::app::Hit;
    let mut a = app();
    a.focus_input();
    a.type_text("keep every word");
    let kept = a.composer.text();
    // The cauldron's strip, the status strip, the cluster tabs, the switch,
    // the copy: none of them touches the bucket.
    // V1.1: the global bar, where Config's strip went.
    a.set_ctab(CTab::Settings);
    click_label(&mut a, "TUI | Devices", "TUI");
    assert_eq!(a.ctab, CTab::Tui);
    a.set_ctab(CTab::Status);
    click_label(&mut a, " | Soup | ", "Bus");
    a.set_ctab(CTab::Cauldron);
    for t in KTab::ALL {
        press_hit(&mut a, Hit::KTab(t));
    }
    press_hit(&mut a, Hit::BelowOpt(true));
    press_hit(&mut a, Hit::Below);
    press_hit(&mut a, Hit::Below);
    press_hit(&mut a, Hit::BucketCopy);
    assert_eq!(a.composer.text(), kept);
    // A reference click with the caret right after a typed word, no space:
    // it goes in beside it, never over it.
    let (q1, q2) = two_questions(&mut a);
    a.focus_input();
    press(&mut a, KeyCode::End);
    a.focus = tui::cauldron::app::Focus::Content;
    press_hit(&mut a, Hit::Item(q1));
    let t1 = tag_of(&a, q1);
    assert_eq!(a.composer.text(), format!("{kept} {t1} "));
    // Undo takes the replace back whole, and the reference before it.
    press_hit(&mut a, Hit::Item(q2));
    a.composer.undo();
    assert_eq!(a.composer.text(), format!("{kept} {t1} "));
    a.composer.undo();
    assert_eq!(a.composer.text(), kept);
}
