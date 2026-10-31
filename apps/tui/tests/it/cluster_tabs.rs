//! Iteration seven (8e4fca10 msg 82; designs 0069 and 0071), as redesigned
//! at msg 165: the cluster tabs' content — Overview's boxes, Tasks (its
//! Questions and Human Actions), the Status tab's Engine and its traces,
//! Artifacts (repos and their blueprints) — and the `i` macro. Real seed files, real key events, real frames on ratatui's
//! TestBackend, real files under a scratch cauldron root.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tui::cauldron::app::{App, Band, Focus, Footer, HaBand, Item, QBand, KTab};
use tui::cauldron::engine;
use tui::cauldron::model::{DataFile, Soup};
use tui::cauldron::render_to_text;
use tui::cauldron::store::seed_dir;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "cluster-tabs-test-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path, name: &str) -> Environment {
    Environment {
        root: root.to_path_buf(),
        root_source: Source::CauldronHome,
        name: "prod".into(),
        name_source: Source::Default,
        data_root: root.join("data").join("prod"),
    }
    .for_name(name)
    .unwrap()
}

fn app() -> App {
    // Hermetic: no test here may make a billed type-one call, whatever is on
    // PATH (a cluster made here seeds; tests/it/seed_live.rs is the live proof).
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(&scratch_root(), "prod"), seed_dir(), true).expect("seed loads")
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn press(a: &mut App, code: KeyCode) {
    a.handle(key(code));
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 200).unwrap().join("\n")
}

/// The page column of every row (what is right of the sidebar), trimmed.
fn page_rows(a: &mut App) -> Vec<String> {
    render_to_text(a, 120, 200)
        .unwrap()
        .into_iter()
        .map(|r| r.splitn(3, '│').nth(2).map(|x| x.trim_end_matches(['│', ' ']).to_string()).unwrap_or_default())
        .collect()
}

fn cluster(a: &App, key: &str) -> usize {
    a.forest.find(key).unwrap()
}

/// Put the selection on `it` in the page, with the page holding the keys.
fn select(a: &mut App, it: Item) {
    a.focus = Focus::Content;
    a.item = a.items().iter().position(|x| *x == it).unwrap_or_else(|| panic!("{it:?} not on the page: {:?}", a.items()));
}

fn task(a: &App, g: &str, serial: u32) -> Item {
    Item::Task { cluster: cluster(a, g), serial }
}

// ── Overview ───────────────────────────────────────────────────────────────

#[test]
fn overview_is_bands_scope_last_dump_directives_constraints_objectives_milestones() {
    let mut a = app();
    a.set_ktab(KTab::Details);
    let rows = page_rows(&mut a);
    let s = rows.join("\n");
    // The bands, in order, Scope first (msg 186: bands, not boxes).
    let at = |needle: &str| s.find(needle).unwrap_or_else(|| panic!("{needle}:\n{s}"));
    // 8e4fca10 msg 385: always-expanded bands — a dot for the chevron — and
    // one blank row after each section.
    let order = ["Scope", "Last dump", "Directives", "Constraints", "Objectives", "Milestones", "Cluster"].map(|b| at(&format!("• {b}")));
    assert!(order.windows(2).all(|w| w[0] < w[1]), "the bands in order: {s}");
    assert!(!s.contains("▾ ") && !s.contains("▸ "), "no chevron on Details: {s}");
    for b in ["Last dump", "Directives", "Constraints", "Objectives", "Milestones", "Cluster"] {
        let y = rows.iter().position(|r| r.contains(&format!("• {b}"))).unwrap();
        assert!(rows[y - 1].trim().is_empty(), "a blank row before `{b}`, after the section above: {:?}", rows[y - 1]);
        assert!(!rows[y - 2].trim().is_empty(), "one blank row, not two, before `{b}`");
    }
    assert!(s.contains("One input, many clusters, nobody in the middle. You dump anything"), "{s}");
    // His directives, verbatim, each with the message it came from.
    // Each is a CARD (msg 186, task 0166 V1): the message it came from at
    // its top left, its id the legend, its words inside.
    let card = |id: &str, cite: &str, text: &str| {
        let top = rows.iter().position(|r| r.contains(&format!(" {id} ─╮"))).unwrap_or_else(|| panic!("{id}:
{s}"));
        assert!(rows[top].contains(&format!("╭─ {cite} ")), "{}", rows[top]);
        assert!(rows[top + 1].contains(text), "{}", rows[top + 1]);
    };
    card("DIR-1", "8e4fca10#1", "One input, many surfaces, nobody in the middle.");
    card("DIR-2", "8e4fca10#55", "Assume the user knows how to use this thing.");
    // His constraints, with theirs.
    card("CON-2", "8e4fca10#165", "Scrolling never changes any row's shape.");
    // No row per child cluster, no log, no repos line, no linked section.
    assert!(!s.contains("log ▸") && !s.contains("Repos"), "{s}");
    // A sub-cluster's Overview carries the directives that bind it from above.
    a.select(cluster(&a, "X-2"));
    assert!(screen(&mut a).contains(" DIR-1 ─╮"));
    // The items and their tags.
    a.select(cluster(&a, "X-1"));
    let c = cluster(&a, "X-1");
    let items = a.items();
    use tui::cauldron::app::{Band, DBand};
    assert_eq!(items[0], Item::Band(Band::Details(DBand::Scope)));
    assert_eq!(a.tag_for(items[0]), None, "a band header quotes nothing");
    assert_eq!(items[1], Item::Description(c));
    assert_eq!(a.tag_for(items[1]).unwrap().render(), "[X-1]");
    assert_eq!(items[3], Item::LastDump(c));
    assert_eq!(a.tag_for(items[3]).unwrap().render(), "[dump:11]", "the seed's last dump");
    assert_eq!(items[5], Item::Directive { cluster: c, ix: 0 });
    assert_eq!(a.tag_for(items[5]).unwrap().render(), "[DIR-1]");
    assert!(items.contains(&Item::Constraint { cluster: c, ix: 0 }));
    assert_eq!(a.tag_for(Item::Constraint { cluster: c, ix: 0 }).unwrap().render(), "[CON-1]");
    let s = screen(&mut a);
    for child in ["Interface", "Ripples"] {
        assert!(!s.lines().any(|l| l.split('│').nth(2).is_some_and(|page| page.trim_start_matches(['▌', ' ']).starts_with(child))), "{child} has a row on Overview:\n{s}");
    }
    // → on a directive opens its trace: his words quoted, what serves it.
    select(&mut a, Item::Directive { cluster: c, ix: 0 });
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    assert!(s.contains("Details › DIR-1"), "{s}");
    assert!(s.contains("“One input, many surfaces, nobody in the middle. I dump anything"), "{s}");
    assert!(s.contains("↓ OBJ-1  A cluster's page is one living state he reads top-down"), "{s}");
    assert!(s.contains("↓ OBJ-2"), "{s}");
    press(&mut a, KeyCode::Left);
    assert!(a.trace.is_empty());
    assert_eq!(a.items()[a.item], Item::Directive { cluster: c, ix: 0 });
}

#[test]
fn what_he_sent_is_on_the_status_tabs_log_and_the_last_dump_box() {
    let mut a = app();
    let c = cluster(&a, "X-1");
    // msg 235: an untagged dump lands on no cluster, so this one names it.
    a.dump("move the description [cluster:X-1]");
    a.dump("> [Q-1] option four, with the floor");
    let s = screen(&mut a);
    assert!(!s.contains("SENT") && !s.contains("option four"), "Overview carries no send list: {s}");
    // The Last dump box says when, and quotes as the send.
    assert!(a.tag_for(Item::LastDump(c)).unwrap().render().starts_with("[dump:"), "{s}");
    a.set_ktab(KTab::Status);
    assert!(screen(&mut a).contains("log ▸"));
    select(&mut a, Item::Footer(Footer::Log));
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    let newest = s.find("> [Q-1] option four, with the floor").expect(&s);
    let older = s.find("move the description").expect(&s);
    assert!(newest < older, "newest first: {s}");
    press(&mut a, KeyCode::Left);
    a.set_ktab(KTab::Questions);
    assert!(!screen(&mut a).contains("option four"), "nowhere else");
}

// ── Tasks: Questions ───────────────────────────────────────────────────────

#[test]
fn a_question_is_a_box_with_context_options_and_recommendation_never_collapsed() {
    let mut a = app();
    a.set_ktab(KTab::Questions);
    let c = cluster(&a, "X-1");
    let s = screen(&mut a);
    // 8e4fca10 msg 385: Questions and Actions are cluster tabs, each with
    // its own count, and no sub-tab strip under them.
    assert!(!s.contains("Human Actions") && !s.contains("Questions │"), "{s}");
    assert!(s.contains("Questions 3 | Actions 1 | Tasks | Nodes | Status | Details"), "{s}");
    // Every active question is a box, its id the legend; blocking is a glyph.
    let q1 = s.lines().position(|l| l.contains("◆ Q-1 ─╮")).expect(&s);
    let lines: Vec<&str> = s.lines().collect();
    assert!(lines[q1 + 1].contains("When the system is nearly certain"), "{}", lines[q1 + 1]);
    assert!(s.lines().any(|l| l.contains(" Q-2 ─╮") && !l.contains('◆')), "{s}");
    // Context, Options and Recommendation, all on screen with nothing opened.
    assert!(s.contains("This gates everything downstream"), "{s}");
    assert!(s.contains("1. ASK EVERYTHING. Every open question reaches you"), "{s}");
    assert!(s.contains("4. THREE WAYS WITH A FLOOR. As above"), "{s}");
    assert!(s.contains("Most questions have one obvious answer;"), "the recommendation's argument: {s}");
    assert!(s.contains("TONE PLUS ROUTING ECHO, suppressed"), "Q-2 is open too: {s}");
    // ↓ from the question walks its options; `i` on one quotes it.
    select(&mut a, Item::Question { cluster: c, serial: 1 });
    let before = screen(&mut a).lines().count();
    for _ in 0..4 {
        press(&mut a, KeyCode::Down);
    }
    assert_eq!(a.items()[a.item], Item::Option { cluster: c, serial: 1, opt: 4 });
    assert_eq!(screen(&mut a).lines().count(), before);
    press(&mut a, KeyCode::Char('i'));
    assert_eq!(a.composer.text(), "> [Q-1.opt4]");
    // ← on an option climbs to its question; nothing closes.
    a.focus = Focus::Content;
    press(&mut a, KeyCode::Left);
    assert_eq!(a.items()[a.item], Item::Question { cluster: c, serial: 1 });
    assert!(screen(&mut a).contains("ASK EVERYTHING"));
}

#[test]
fn closed_questions_are_bands_by_status_that_open_to_boxes() {
    let mut a = app();
    a.set_ktab(KTab::Questions);
    let c = cluster(&a, "X-1");
    let s = screen(&mut a);
    assert!(has_band(&s, "▸ Complete", 2) && has_band(&s, "▸ Superseded", 1) && has_band(&s, "▸ Archived", 1), "{s}");
    assert!(!s.contains("Should each row of the cluster tree"), "collapsed by default: {s}");
    select(&mut a, Item::Band(Band::Questions(QBand::Complete)));
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    assert!(has_band(&s, "▾ Complete", 2), "{s}");
    assert!(s.contains("Should each row of the cluster tree carry its coun"), "{s}");
    assert!(s.contains("No. The tabs carry the counts; the tree carries names only."), "{s}");
    assert!(s.contains("→ DEC-3 · 2026-10-09"), "{s}");
    select(&mut a, Item::Closed { cluster: c, serial: 4 });
    press(&mut a, KeyCode::Char('i'));
    assert_eq!(a.composer.text(), "> [Q-4]");
    // ← climbs to the band, ← again closes it.
    a.focus = Focus::Content;
    press(&mut a, KeyCode::Left);
    assert_eq!(a.items()[a.item], Item::Band(Band::Questions(QBand::Complete)));
    press(&mut a, KeyCode::Left);
    assert!(!screen(&mut a).contains("Should each row of the cluster tree"));
    // The archived and superseded bands hold theirs.
    select(&mut a, Item::Band(Band::Questions(QBand::Archived)));
    press(&mut a, KeyCode::Right);
    assert!(a.items().contains(&Item::Closed { cluster: c, serial: 8 }), "{:?}", a.items());
    select(&mut a, Item::Band(Band::Questions(QBand::Superseded)));
    press(&mut a, KeyCode::Right);
    assert!(a.items().contains(&Item::Closed { cluster: c, serial: 9 }), "{:?}", a.items());
}

// ── Status: the Engine ─────────────────────────────────────────────────────

/// The rail margin of every row, `\0`-free: the design's mockup, column for
/// column (0069 §3, §9).
#[test]
fn the_engine_is_a_rail_graph_in_the_git_log_grammar() {
    let a = app();
    let c = cluster(&a, "X-1");
    let binding: Vec<String> = a.forest.binding_directives(c).into_iter().map(|(g, ix)| a.forest.get(g).directives[ix].id.clone()).collect();
    let g = engine::layout(a.forest.get(c), &binding);
    assert_eq!(g.lanes, 3, "the fake data stays at three lanes");
    let want = [
        ("DIR-1", "      "),
        ("OBJ-1", "      "),
        ("M-1", "●     "),
        ("T-1", "│ ●   "),
        ("M-2", "● │   "),
        ("T-2", "│ ●   "),
        ("T-3", "│ ●   "),
        ("fork", "│ ├─╮ "),
        ("OBJ-2", "│ │ │ "),
        ("M-3", "● │ │ "),
        ("T-4", "│ ● │ "),
        ("T-6", "│ │ ● "),
        ("M-4", "● │   "),
        ("T-5", "  ●   "),
        ("DIR-2", "      "),
        ("OBJ-3", "      "),
        ("M-5", "●     "),
        ("T-8", "●     "),
    ];
    assert_eq!(g.rows.len(), want.len(), "{:#?}", g.rows);
    for (i, (what, margin)) in want.iter().enumerate() {
        assert_eq!(g.margin(i, "●"), *margin, "row {i} ({what})");
    }
    // The same rails on screen, in their states' glyphs, in the Tasks tab's
    // Engine band.
    let mut a = a;
    a.set_ktab(KTab::Tasks);
    let s = screen(&mut a);
    for line in [
        "✓      M-1  An interface sketch exists and he has answered its",
        "│ ✓      T-1  Interface sketch (report 0017)",
        "◆ │    M-2  The cluster tree and a page render",
        "│ ├─╮",
        "│ │ │  OBJ-2  A dump lands on every cluster it concerns",
        "◇ │ │  M-3  A tagged send reaches every tagged cluster through a ripple",
        "│ ○ │    T-4  Ripple-backed input path",
        "│ │ ○    T-6  Answer questions by tag from the input",
        "■ │    M-4  He hears a tone and a one-line routing echo after a dump",
        "  ■      T-5  Voice echo after a dump",
        "DIR-2  Assume the user knows how to use this thing.",
    ] {
        assert!(s.contains(line), "missing `{line}`:\n{s}");
    }
    // The right-hand tokens: n/m done, the run, the report, the blocker.
    let row = |needle: &str| s.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle}:\n{s}")).trim_end_matches('│').trim_end().to_string();
    assert!(row("OBJ-1  A cluster's page").ends_with("1/2"));
    assert!(row("M-2  The cluster tree").ends_with("0/2"));
    assert!(row("T-2  First TUI").ends_with("acct3 · 41m"));
    assert!(row("T-1  Interface sketch").ends_with("0017"));
    assert!(row("T-5  Voice echo").ends_with("■ Q-2"));
    assert!(row("▾ Engine").ends_with("7 tasks · 5 milestones · ✓ 1 done"));
}

#[test]
fn a_join_is_drawn_where_a_node_needs_two_rails() {
    let json = r#"{"clusters":[{"id":"X-1","name":"G","description":"",
        "directives":[{"id":"DIR-9","text":"t","cite":"x#1"}],
        "objectives":[{"serial":1,"title":"o","serves":"DIR-9"}],
        "milestones":[{"serial":1,"handle":"m","objective":1,"tasks":[1,2,3]}],
        "tasks":[{"serial":1,"title":"a","state":"done"},{"serial":2,"title":"b","state":"ready"},
                 {"serial":3,"title":"c","state":"ready","depends_on":[1,2]}]}]}"#;
    let f: DataFile = serde_json::from_str(json).unwrap();
    let soup = Soup::from_file(f).unwrap();
    let g = soup.forest.get(0);
    let lay = engine::layout(g, &["DIR-9".to_string()]);
    let margins: Vec<String> = (0..lay.rows.len()).map(|i| lay.margin(i, "●")).collect();
    // DIR, OBJ, M-1, T-1 (lane 0: M-1 needs nothing after it), T-2 (lane 1,
    // T-1's rail running beside it), the JOIN, T-3 on the lane that stays.
    assert_eq!(margins, ["    ", "    ", "●   ", "●   ", "│ ● ", "├─╯ ", "●   "], "{margins:#?}");
    assert_eq!(lay.rows[5].kind, engine::Kind::Connector);
}

#[test]
fn every_engine_row_traces_back_to_his_words_and_the_trace_page_drills_in_place() {
    let mut a = app();
    a.set_ktab(KTab::Tasks);
    let c = cluster(&a, "X-1");
    let t5 = task(&a, "X-1", 5);
    select(&mut a, t5);
    // No row grows when selected: no trace line under the selected row.
    let s = screen(&mut a);
    assert!(!s.contains("↑ M-4 · OBJ-2"), "{s}");
    // → opens the trace page.
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    assert!(s.contains("Tasks › T-5"), "{s}");
    assert!(s.contains("■ T-5   Voice echo after a dump"), "{s}");
    assert!(s.contains("← T-4   ■ Q-2"), "{s}");
    assert!(s.contains("↑ M-4    He hears a tone and a one-line routing echo after a dump"), "{s}");
    assert!(s.contains("↑ OBJ-2  A dump lands on every cluster it concerns"), "{s}");
    assert!(s.contains("↑ DIR-1  One input, many surfaces, nobody in the middle."), "{s}");
    let msg = s.lines().find(|l| l.contains("8e4fca10#21")).expect(&s);
    assert!(msg.trim_end_matches('│').trim_end().ends_with("2026-10-07"), "{msg}");
    assert!(s.contains("“And right after I dump something in I want to hear a tone"), "his words, verbatim: {s}");
    assert!(s.contains("Q-2    Right after you dump something in, what do you want to hear?"), "{s}");
    assert!(s.contains("R-2   THE DUMP-ANYTHING LINE"), "{s}");
    // Every row is an item; `i` on the message quotes it.
    let msg_item = a.items().iter().copied().find(|i| matches!(i, Item::Message(_))).unwrap();
    assert_eq!(a.tag_for(msg_item).unwrap().render(), "[msg:8e4fca10#21]");
    // → on the blocking question opens its options in place, the crumb grows.
    select(&mut a, Item::Question { cluster: c, serial: 2 });
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    assert!(s.contains("Tasks › T-5 › Q-2"), "{s}");
    assert!(s.contains("1  TONE ONLY"), "{s}");
    assert!(s.contains("▸ 2  TONE PLUS ROUTING ECHO"), "{s}");
    // ← climbs one level, then back to the graph with T-5 selected.
    press(&mut a, KeyCode::Left);
    let s = screen(&mut a);
    assert!(s.contains("Tasks › T-5") && !s.contains("Tasks › T-5 › Q-2"), "{s}");
    assert!(!s.contains("1  TONE ONLY"), "{s}");
    press(&mut a, KeyCode::Left);
    assert!(a.trace.is_empty());
    assert_eq!(a.items()[a.item], t5);
    assert!(screen(&mut a).contains("DIR-1  One input"));
    // An ancestor opens ITS trace page; the region lists its members.
    press(&mut a, KeyCode::Right);
    select(&mut a, Item::Milestone { cluster: c, serial: 4 });
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    assert!(s.contains("Tasks › T-5 › M-4"), "{s}");
    assert!(s.contains("■ T-5    Voice echo after a dump"), "a milestone lists what it holds: {s}");
    assert!(s.contains("← M-3"), "and what it needs: {s}");
    let r2 = Item::Region(a.corpus.find_region("R-2").unwrap());
    select(&mut a, r2);
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    assert!(s.contains("Tasks › T-5 › M-4 › R-2"), "{s}");
    assert!(s.contains("IS-14"), "the region's members, in place: {s}");
    assert_eq!(a.tag_for(r2).unwrap().render(), "[region:R-2]");
    press(&mut a, KeyCode::Left);
    press(&mut a, KeyCode::Left);
    press(&mut a, KeyCode::Left);
    assert!(a.trace.is_empty());
    assert_eq!(a.items()[a.item], t5);
    // Off the trace, ← climbs to the Engine band's header, then closes it.
    press(&mut a, KeyCode::Left);
    assert_eq!(a.items()[a.item], Item::Band(Band::Engine));
    press(&mut a, KeyCode::Left);
    assert!(!a.band_open(Band::Engine));
    assert!(!screen(&mut a).contains("DIR-1  One input"));
}

#[test]
fn engine_items_quote_as_their_ids_and_a_milestones_trace_shows_what_it_builds() {
    let mut a = app();
    a.set_ktab(KTab::Tasks);
    let c = cluster(&a, "X-1");
    for (it, tag) in [
        (Item::Directive { cluster: c, ix: 0 }, "[DIR-1]"),
        (Item::Objective { cluster: c, serial: 2 }, "[OBJ-2]"),
        (Item::Milestone { cluster: c, serial: 3 }, "[M-3]"),
        (task(&a, "X-1", 6), "[T-6]"),
    ] {
        assert!(a.items().contains(&it), "{it:?}");
        assert_eq!(a.tag_for(it).unwrap().render(), tag);
    }
    // 0071 S5: a milestone's links into a blueprint, on its trace page (no
    // row grows when selected, so never on the graph).
    let tui = cluster(&a, "X-10");
    a.select(tui);
    select(&mut a, Item::Milestone { cluster: tui, serial: 17 });
    assert!(!screen(&mut a).contains("builds MOD-tui"), "the selected row does not grow");
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    assert!(s.contains("Tasks › M-17"), "{s}");
    assert!(s.contains("builds   MOD-tui"), "{s}");
    assert!(s.contains("IF-tui") && s.contains("R-key-table"), "{s}");
    assert!(s.contains("waits on MIG-cluster-face-keys"), "{s}");
    press(&mut a, KeyCode::Left);
    select(&mut a, Item::Milestone { cluster: tui, serial: 18 });
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    assert!(s.contains("Tasks › M-18"), "{s}");
    assert!(s.contains("clears   DRAG-keys-duplicated"), "{s}");
}

// ── Tasks: Human Actions ───────────────────────────────────────────────────

#[test]
fn human_actions_are_boxes_in_status_bands_with_their_steps() {
    let mut a = app();
    a.set_ktab(KTab::Actions);
    let c = cluster(&a, "X-1");
    let rows = page_rows(&mut a);
    let s = rows.join("\n");
    assert!(has_band(&s, "▾ Active", 1), "{s}");
    let head = rows.iter().find(|r| r.contains("● open · ") && r.contains("HA-1")).expect(&s);
    assert!(head.contains(" · 0/2 steps"), "{head}");
    assert!(s.contains("Approve the Cauldron TUI's environment layout"), "{s}");
    // Its steps, always shown.
    assert!(s.contains("Step 1 — read the four files"), "{s}");
    assert!(s.contains("↗ Open the soup folder"), "{s}");
    assert!(s.contains("[ done ]"), "{s}");
    // Resolved tickets are a band of their own, collapsed.
    assert!(has_band(&s, "▸ Done", 1), "{s}");
    assert!(!s.contains("Read the interface sketch"), "{s}");
    assert!(s.contains("1 below") && !s.contains("below ·"), "a bare count (F10): {s}");
    select(&mut a, Item::Band(Band::Actions(HaBand::Done)));
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    let ha2 = s.lines().find(|l| l.contains("✓ resolved") && l.contains("HA-2")).expect(&s);
    assert!(ha2.contains(" · "), "{ha2}");
    assert!(s.contains("Read the interface sketch"), "{s}");
    // `i` on a ticket quotes it; → steps into its first part.
    select(&mut a, Item::Ha { cluster: c, serial: 1 });
    press(&mut a, KeyCode::Char('i'));
    assert_eq!(a.composer.text(), "> [HA-1]");
    a.focus = Focus::Content;
    press(&mut a, KeyCode::Right);
    assert_eq!(a.items()[a.item], Item::HaPart { cluster: c, serial: 1, part: 0 });
    assert_eq!(a.tag_for(a.items()[a.item]).unwrap().render(), "[HA-1.step1]");
}

/// 0214 A1: a step's `[ done ]` is a button — ONE click marks the step done
/// and it persists (tickets.json); it never inserts a reference. A second
/// click marks it not done. Inserting is the CARD's job: a click on the card
/// puts `[HA-1]` at the bucket's caret.
#[test]
fn a_steps_done_button_marks_it_at_one_click_and_inserts_nothing() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let root = scratch_root();
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env_in(&root, "prod"), seed_dir(), true).expect("seed loads");
    a.set_ktab(KTab::Actions);
    let c = cluster(&a, "X-1");
    let click = |a: &mut App, needle: &str| {
        let rows = render_to_text(a, 120, 200).unwrap();
        let (y, row) = rows.iter().enumerate().find(|(_, r)| r.contains(needle)).unwrap_or_else(|| panic!("no `{needle}`:\n{}", rows.join("\n")));
        let x = row[..row.find(needle).unwrap()].chars().count() + 2;
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
            a.handle(Event::Mouse(MouseEvent { kind, column: x as u16, row: y as u16, modifiers: KeyModifiers::NONE }));
        }
    };
    let (_, t) = a.ticket(c, 1).unwrap();
    assert!(!a.step_done(c, 1, &t, 0));
    click(&mut a, "[ done ]");
    assert_eq!(a.composer.text(), "", "nothing inserted");
    let (_, t) = a.ticket(c, 1).unwrap();
    assert!(a.step_done(c, 1, &t, 0), "the first click marked it: {:?}", a.notice);
    let on_disk = std::fs::read_to_string(root.join("data/prod/tui/soup/tickets.json")).unwrap();
    assert!(on_disk.contains("step-1"), "{on_disk}");
    // It persists: a face opened afresh shows it done.
    let mut b = App::open(env_in(&root, "prod"), seed_dir(), true).unwrap();
    b.set_ktab(KTab::Actions);
    assert!(screen(&mut b).contains("[✓ done]"), "{}", screen(&mut b));
    // Again, past the double-click window: not done.
    std::thread::sleep(std::time::Duration::from_millis(450));
    click(&mut a, "[✓ done]");
    let (_, t) = a.ticket(c, 1).unwrap();
    assert!(!a.step_done(c, 1, &t, 0));
    assert_eq!(a.composer.text(), "");
    // The card inserts its reference.
    std::thread::sleep(std::time::Duration::from_millis(450));
    click(&mut a, "Approve the Cauldron TUI's environment layout");
    assert_eq!(a.composer.text().trim(), "[HA-1]");
}

// ── Repos and their blueprints ─────────────────────────────────────────────

#[test]
fn a_clusters_soup_tab_is_four_bands_and_a_repo_band_carries_its_metadata() {
    let mut a = app();
    let tui = cluster(&a, "X-10");
    a.select(tui);
    a.set_ktab(KTab::Nodes);
    let s = screen(&mut a);
    // Only Tasks carries a number on the strip (msg 165).
    assert!(s.lines().any(|l| l.contains(" Details") && l.contains(" Nodes ")), "{s}");
    // Four bands (msg 186: Soup and Artifacts merged), open only along the
    // ACTIVE path (design 0146): runs hold worktrees of cauldron, so REPOS
    // and cauldron are open and harness, where nothing runs, is closed.
    for band in ["▾ Regions", "▾ Repos", "▸ Directories", "▸ Artifacts", "▾ cauldron  /Users/demo/code/cauldron ↗"] {
        assert!(s.contains(band), "{band}:\n{s}");
    }
    assert_eq!(a.tag_for(a.items()[0]), None, "a band header quotes nothing");
    assert!(s.contains("▸ harness  /Users/demo/code/harness ↗"), "{s}");
    // Wider than 120: the fixture's path is absolute (REL-3.S2: no operator
    // HOME for `~` to shorten), so the metadata is read at a width it fits.
    let wide = render_to_text(&mut a, 160, 200).unwrap().join("\n");
    let head = wide.lines().find(|l| l.contains("▸ harness  /Users/demo/code/harness ↗")).expect(&wide);
    assert!(head.contains("main @ 8299dcc1") && head.contains("bp v1") && head.contains("●"), "{head}");
    a.focus = Focus::Content;
    let repo = a.repos.find_repo("harness").unwrap();
    select(&mut a, Item::Repo(repo));
    assert_eq!(a.tag_for(a.items()[a.item]).unwrap().render(), "[repo:harness]");
    // `c` copies the selected repo's path, `o` opens it.
    press(&mut a, KeyCode::Char('c'));
    press(&mut a, KeyCode::Char('o'));
    assert_eq!(a.outbox.len(), 2, "{:?}", a.outbox);
    // Aggregate on a root: each repo once.
    a.select(cluster(&a, "X-9"));
    a.toggle_aggregate();
    assert_eq!(a.cluster_repos().len(), 2);
    let via = &a.cluster_repos()[0].via;
    assert_eq!(via, &vec![cluster(&a, "X-9"), cluster(&a, "X-10"), cluster(&a, "X-11")]);
}

#[test]
fn a_repo_opens_to_worktrees_blueprint_and_files_and_an_entry_to_its_page() {
    let mut a = app();
    let tui = cluster(&a, "X-10");
    a.select(tui);
    a.set_ktab(KTab::Nodes);
    a.focus = Focus::Content;
    // → on harness opens it in place.
    let repo = a.repos.find_repo("harness").unwrap();
    select(&mut a, Item::Repo(repo));
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    for band in ["▸ Worktrees", "▸ Blueprint", "▸ Files  /Users/demo/code/harness ↗"] {
        assert!(s.contains(band), "{band}:\n{s}");
    }
    let bp = s.lines().find(|l| l.contains("▸ Blueprint")).unwrap();
    assert!(bp.contains("v1 · adopted 2026-08-22 · 12 modules · 61% linked"), "{bp}");
    // The 0071 page nests under BLUEPRINT, its sections as bands.
    let bpi = a.items().iter().position(|i| a.tag_for(*i).is_none() && matches!(i, Item::Art(_)) && a.art_row(*i).is_some_and(|r| r.key == "bp:harness")).unwrap();
    a.item = bpi;
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    for section in ["▸ Modules", "▸ Violations", "▸ Drag", "▸ Contradictions", "▸ Migrations", "▸ Capabilities"] {
        assert!(s.contains(section), "{section}:\n{s}");
    }
    press(&mut a, KeyCode::Right); // to MODULES
    press(&mut a, KeyCode::Right); // open it
    assert!(screen(&mut a).contains("▸ MOD-threads"), "{}", screen(&mut a));
    // → on a collapsed module expands it in place; ← collapses it.
    let threads = a.repos.repos[repo].find("MOD-threads-b61c2e9d").unwrap();
    select(&mut a, Item::Bp { repo, node: threads });
    press(&mut a, KeyCode::Right);
    assert!(screen(&mut a).contains("▾ MOD-threads"));
    press(&mut a, KeyCode::Left);
    assert!(screen(&mut a).contains("▸ MOD-threads"), "← collapses it first");
    // An expanded module, → again: its page.
    let modtui = a.repos.repos[repo].find("MOD-tui-4f1c9a02").unwrap();
    select(&mut a, Item::Bp { repo, node: modtui });
    assert_eq!(a.tag_for(a.items()[a.item]).unwrap().render(), "[MOD-tui-4f1c9a02]");
    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Right);
    let s = screen(&mut a);
    assert!(s.contains("Nodes › harness › MOD-tui"), "{s}");
    assert!(s.lines().any(|l| l.contains("MOD-tui-4f1c9a02") && l.trim_end_matches('│').trim_end().ends_with("module · locked · built for MIG-cluster-face-keys")), "{s}");
    assert!(s.contains("owns       R-draft-store"), "{s}");
    // ← walks back to the row it left.
    press(&mut a, KeyCode::Left);
    assert!(a.rnav.is_empty());
    assert_eq!(a.items()[a.item], Item::Bp { repo, node: modtui });
    // ← collapses it, then climbs to MODULES, which ← closes.
    press(&mut a, KeyCode::Left);
    assert!(screen(&mut a).contains("▸ MOD-tui "));
    press(&mut a, KeyCode::Left);
    assert_eq!(a.art_row(a.items()[a.item]).unwrap().key, "bp:harness:modules");
    press(&mut a, KeyCode::Left);
    assert!(!screen(&mut a).contains("MOD-threads"));
}

#[test]
fn a_repo_tag_lands_on_every_cluster_that_claims_it() {
    let mut a = app();
    a.dump("about [repo:harness]");
    assert_eq!(a.sends.sends.last().unwrap().targets, ["X-1", "X-5", "X-9", "X-10", "X-11"]);
    a.dump("this one [node:MOD-billing-8d1c5e92]");
    assert_eq!(a.sends.sends.last().unwrap().targets, ["X-6", "X-7", "X-8"]);
    a.dump("and [node:DIR-3]");
    assert_eq!(a.sends.sends.last().unwrap().targets, ["X-6"]);
    a.dump("quoting [msg:8e4fca10#21] only");
    // msg 235: an untagged dump lands on no cluster; it goes into the soup.
    assert!(a.sends.sends.last().unwrap().targets.is_empty(), "a message lands nowhere, and nor does the dump");
    assert!(a.notice.as_deref().unwrap_or("").ends_with("→ the soup"), "{:?}", a.notice);
}

#[test]
fn bad_blueprint_and_plan_data_is_refused_loudly_and_vanished_repo_links_are_skipped() {
    use tui::cauldron::blueprint::{BlueprintFile, Repos};
    let forest = |json: &str| Soup::from_file(serde_json::from_str::<DataFile>(json).unwrap());
    let one = |extra: &str| format!(r#"{{"clusters":[{{"id":"X-1","name":"A","description":"",{extra}}}]}}"#);
    let refuse = |extra: &str, why: &str| {
        let e = format!("{:#}", forest(&one(extra)).unwrap_err());
        assert!(e.contains(why), "{e}");
    };
    // REL-3.S2d: an absent referent is skipped and counted, not refused
    // (every kind, each opposed by its resolving arm: `stale_refs_rel3s2d`).
    let skip = |extra: &str, why: &str| {
        let s = forest(&one(extra)).unwrap_or_else(|e| panic!("REL-3.S2d: skipped, not refused: {e:#}"));
        assert!(s.forest.stale.len() == 1 && s.forest.stale[0].contains(why), "{:?}", s.forest.stale);
    };
    skip(r#""objectives":[{"serial":1,"title":"o","serves":"DIR-9"}]"#, "a directive neither it nor an ancestor holds");
    refuse(r#""tasks":[{"serial":1,"title":"t","state":"blocked"}]"#, "blocked exactly when something blocks it");
    refuse(r#""tasks":[{"serial":1,"title":"t","state":"ready","blocked_by":["Q-1"]}]"#, "blocked exactly when something blocks it");
    refuse(
        r#""directives":[{"id":"DIR-1","text":"t","cite":"x#1"}],"objectives":[{"serial":1,"title":"o","serves":"DIR-1"}],
           "milestones":[{"serial":1,"handle":"a","objective":1,"depends_on":["M-2"]},{"serial":2,"handle":"b","objective":1,"depends_on":["M-1"]}]"#,
        "form a cycle",
    );
    refuse(
        r#""directives":[{"id":"DIR-1","text":"t","cite":"x#1"}],"objectives":[{"serial":1,"title":"o","serves":"DIR-1"}],
           "tasks":[{"serial":1,"title":"t","state":"ready"}],
           "milestones":[{"serial":1,"handle":"a","objective":1,"tasks":[1]},{"serial":2,"handle":"b","objective":1,"tasks":[1]}]"#,
        "sits under two milestones",
    );
    skip(r#""directives":[{"id":"DIR-1","text":"t","cite":"x#1","region":"R-404"}]"#, "which is not in the soup");
    // REL-3.S2c: a cluster that claims a repo nobody registered, focuses a
    // node its repo does not hold, or has an edge into nothing is NOT
    // refused any more (a seed this environment no longer reads registered
    // them): the link is skipped and counted for the face's one notice.
    // Opposed in `stale_links_rel3s2c`, where the same links resolve.
    let soup = forest(&one(r#""repos":[{"name":"ghost"}]"#)).unwrap();
    let r = Repos::build(BlueprintFile::default(), &soup.forest).expect("REL-3.S2c: a claim of a vanished repo is skipped, not refused");
    assert_eq!(r.stale_links, vec!["cluster `X-1` claims repo `ghost`, which is not registered".to_string()]);
    assert!(r.claims[0].is_empty(), "skipped in the view");
    let soup = forest(&one(r#""tasks":[{"serial":1,"title":"t","state":"ready","edges":[{"kind":"builds","to":"MOD-x-00000000"}]}]"#)).unwrap();
    let r = Repos::build(BlueprintFile::default(), &soup.forest).expect("REL-3.S2c: an edge into nothing is skipped, not refused");
    assert_eq!(r.stale_links.len(), 1, "{:?}", r.stale_links);
    assert!(r.stale_links[0].contains("which is in no registered blueprint"), "{:?}", r.stale_links);
    // REL-3.S2d: a repo's OWN blueprint pointing outside itself is skipped
    // and counted too (REL-3.S2c had kept it refused).
    let bp: BlueprintFile = serde_json::from_str(
        r#"{"repos":[{"name":"r","path":"/r","branch":"main","head":"0","blueprint":{"schema_version":1,
            "nodes":[{"id":"MOD-a-00000000","type":"module","state":"draft","handle":"h","edges":[{"kind":"owns","to":"R-b-11111111"}]}]}}]}"#,
    )
    .unwrap();
    let soup = forest(&one("\"tasks\":[]")).unwrap();
    let r = Repos::build(bp, &soup.forest).expect("REL-3.S2d: an in-repo edge into nothing is skipped, not refused");
    assert_eq!(r.stale_refs, vec!["repo `r`: `MOD-a-00000000` points at `R-b-11111111`, which is not in its blueprint".to_string()]);
    assert!(r.repos[0].nodes[0].edges.is_empty(), "skipped in the view");
}

// ── the `i` macro ──────────────────────────────────────────────────────────

/// His macro, msg 82: `[⌥↓, newline, newline, "> [tag]"]`.
#[test]
fn i_quotes_the_selected_item_below_the_draft() {
    let mut a = app();
    a.set_ktab(KTab::Tasks);
    let t4 = task(&a, "X-1", 4);
    // EMPTY draft: no ⌥↓, no new lines — the draft is the quote alone.
    select(&mut a, t4);
    press(&mut a, KeyCode::Char('i'));
    assert_eq!(a.composer.text(), "> [T-4]");
    assert_eq!(a.focus, Focus::Input);
    let end = a.composer.cursor();
    assert_eq!((end.line, end.byte), (0, "> [T-4]".len()), "the caret sits after the tag");

    // A draft with the caret at its very START: ⌥↓ until it stops is the end
    // of the draft, so the quote never lands in the middle of his words.
    let mut b = app();
    b.set_ktab(KTab::Tasks);
    b.type_text("first thought\nsecond thought");
    b.composer.buffer_start();
    assert_eq!(b.composer.cursor().line, 0);
    select(&mut b, t4);
    press(&mut b, KeyCode::Char('i'));
    assert_eq!(b.composer.text(), "first thought\nsecond thought\n\n> [T-4]");
    // Typing goes on right after the tag.
    for c in " is next".chars() {
        press(&mut b, KeyCode::Char(c));
    }
    assert_eq!(b.composer.text(), "first thought\nsecond thought\n\n> [T-4] is next");
    // A second quote goes below the first.
    while b.focus == Focus::Input {
        press(&mut b, KeyCode::Up);
    }
    let t6 = task(&b, "X-1", 6);
    select(&mut b, t6);
    press(&mut b, KeyCode::Char('i'));
    assert_eq!(b.composer.text(), "first thought\nsecond thought\n\n> [T-4] is next\n\n> [T-6]");
    // A draft that already ends in blank lines loses them first (F53): the
    // quote sits exactly two new lines below the last text.
    let mut c = app();
    c.set_ktab(KTab::Tasks);
    c.type_text("x\n");
    select(&mut c, t4);
    press(&mut c, KeyCode::Char('i'));
    assert_eq!(c.composer.text(), "x\n\n> [T-4]");
    let mut c = app();
    c.set_ktab(KTab::Tasks);
    c.type_text("x  \n\n\n  \n");
    select(&mut c, t4);
    press(&mut c, KeyCode::Char('i'));
    assert_eq!(c.composer.text(), "x\n\n> [T-4]");
    // A draft of nothing but blanks takes the quote alone.
    let mut c = app();
    c.set_ktab(KTab::Tasks);
    c.type_text("\n\n");
    select(&mut c, t4);
    press(&mut c, KeyCode::Char('i'));
    assert_eq!(c.composer.text(), "> [T-4]");
}


/// 0071 Q2, its recommendation built while it is open: a claimed repo's
/// open violations and contradictions that touch the claim's FOCUS enter the
/// cluster's ranking at the floor; everything else stays on Repos.
#[test]
fn focus_nags_rank_among_the_questions_at_the_floor() {
    let mut a = app();
    let platform = cluster(&a, "X-8");
    a.select(platform);
    a.set_ktab(KTab::Questions);
    let s = screen(&mut a);
    // A box among his questions, its id the legend, ranked at the floor: no
    // score is shown, so the floor is the order.
    assert!(s.lines().any(|l| l.contains("⚡  CONTRA-billing-owner ─╮")), "{s}");
    assert!(s.contains("the builder found platform-api"), "{s}");
    assert!(s.contains("broomstick · contradiction"), "{s}");
    let q1 = s.find("◆ Q-20").expect(&s);
    assert!(q1 < s.find("CONTRA-billing-owner").unwrap(), "blocking first, then by value: {s}");
    // The Questions tab counts it (8e4fca10 msg 385: each tab its own count).
    assert_eq!(a.active_questions(), 2, "the contradiction is counted");
    assert!(s.contains(&format!("Questions {}", a.active_questions())), "{s}");
    let _ = tui::cauldron::pages::NAG_FLOOR;
    let repo = a.repos.find_repo("broomstick").unwrap();
    let node = a.repos.repos[repo].find("CONTRA-billing-owner-8e6d2a14").unwrap();
    let it = Item::Bp { repo, node };
    assert_eq!(a.tag_for(it).unwrap().render(), "[CONTRA-billing-owner-8e6d2a14]");
    // Its box says what it involves, with nothing opened.
    assert!(a.items().contains(&it));
    assert!(s.contains("involves MOD-billing · MOD-platform-api · R-stripe-events"), "{s}");
    // Landing's focus is touched by no violation or contradiction: nothing.
    a.select(cluster(&a, "X-7"));
    assert!(!a.items().iter().any(|i| matches!(i, Item::Bp { .. })));
    // Harness/TUI's focus has only drag on it, which is tolerated: nothing.
    a.select(cluster(&a, "X-10"));
    assert!(!a.items().iter().any(|i| matches!(i, Item::Bp { .. })));
}

/// Whether `s` has the band line `head` whose metadata counts `n` (msg 186:
/// the count is the band's metadata, flush right — `▾ Active … 3 questions`).
#[allow(dead_code)]
fn has_band(s: &str, head: &str, n: usize) -> bool {
    s.lines().any(|l| {
        let Some(at) = l.find(head) else { return false };
        let rest = l[at + head.len()..].trim_end_matches(['│', ' ']);
        rest.starts_with(' ') && (rest.contains(&format!(" {n} ")) || rest.ends_with(&format!(" {n}")))
    })
}
