//! Headless exercise of the whole soup face: real seed files, real key and
//! mouse events, real frames drawn to ratatui's TestBackend, and real files
//! under a scratch cauldron root (never `~/.cauldron`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use tui::cauldron::app::{App, Band, CTab, Focus, Footer, HaBand, Hit, Item, Modal, QBand, RBand, Row, KTab, KEYMAP};
use tui::cauldron::model::{DataFile, Forest, Headline, Soup};
use tui::cauldron::render_to_text;
use tui::cauldron::store::{seed_dir, soup_dir};

use crate::own_seed_fixture;

// ── fixtures ────────────────────────────────────────────────────────────────

/// A fresh, empty cauldron root for one test.
fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "clusters-test-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Environment `name` in `root`, validated and guarded by the env crate.
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

fn open(root: &Path, name: &str) -> App {
    // Hermetic: no test here may make a billed type-one call, whatever is on
    // PATH (a cluster made here seeds; tests/it/seed_live.rs is the live proof).
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(root, name), seed_dir(), true).expect("seed loads")
}

fn app() -> App {
    open(&scratch_root(), "prod")
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn with(code: KeyCode, m: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, m))
}

/// ⌘<c>, as the kitty keyboard protocol delivers it (SUPER).
fn cmd(c: char) -> Event {
    with(KeyCode::Char(c), KeyModifiers::SUPER)
}

/// ⌥<c>, as kitty or Option-as-Meta delivers it (ALT).
fn opt(c: char) -> Event {
    with(KeyCode::Char(c), KeyModifiers::ALT)
}

/// Keystrokes, one per character, wherever the focus is.
fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(key(KeyCode::Char(c)));
    }
}

/// Write in the dump: `a` gives it the focus, then the keystrokes.
fn write(a: &mut App, s: &str) {
    if a.focus != Focus::Input {
        a.handle(key(KeyCode::Char('a')));
        assert_eq!(a.focus, Focus::Input);
    }
    typed(a, s);
}

fn click(a: &mut App, x: u16, y: u16) {
    // A click is a press and its release: a page item acts at the release
    // over it (0211 R11).
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
    }
}

/// Draw a frame (recording hit regions) and click the first region that
/// does `hit`.
fn click_on(a: &mut App, hit: Hit) {
    render_to_text(a, 120, 60).unwrap();
    let (r, _) = *a.hits.iter().find(|(_, h)| *h == hit).unwrap_or_else(|| panic!("{hit:?} not on screen"));
    click(a, r.x, r.y);
}

fn screen(app: &mut App) -> String {
    render_to_text(app, 120, 400).unwrap().join("\n")
}

/// The row the terminal caret is drawn on, at 120x40, or None when the frame
/// leaves it hidden (a fresh backend's caret stays at the origin).
fn caret_row(a: &mut App) -> Option<u16> {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    term.draw(|f| tui::cauldron::view::draw(f, a)).unwrap();
    let p = term.get_cursor_position().unwrap();
    (p != ratatui::layout::Position::ORIGIN).then_some(p.y)
}

fn id(a: &App, key: &str) -> usize {
    a.forest.find(key).unwrap()
}

/// The index of `it` on the page on screen.
fn at(a: &App, it: Item) -> usize {
    a.items().iter().position(|x| *x == it).unwrap_or_else(|| panic!("{it:?} not on the page: {:?}", a.items()))
}

/// Select `it` on the page, the page holding the keys.
fn select_item(a: &mut App, it: Item) {
    a.focus = Focus::Content;
    a.item = at(a, it);
}

/// Text that explains the interface instead of showing it. None of it may
/// appear on any screen.
const LEGENDS: &[&str] = &[
    // "fake" as a label (the seed's titles no longer say "on fake data"
    // either: audit 0118 F29).
    "FAKE",
    "(fake",
    "fake data —",
    "(headless",
    "headless ·",
    "not saved",
    "draft saved",
    "q questions",
    "a active ·",
    "p pending",
    "tab ⇥",
    "^G to include",
    "← back",
    "the clusters —",
    "the soup —",
    "every cluster, its questions",
    "enter/click",
    "click tag",
    "F1-F6",
    "(^E)",
    "esc ",
    "each one is its own",
    "created on first draft",
    "→ opens",
    "type to write",
    "(needs nothing)",
    "a task's step is one past",
    "Type below",
    "One node today",
    "this face's addition",
    "dump anything →",
    "── Soup Explorer",
    "Soup Explorer ·",
    // Iteration seven (design 0069 §9): the label words and the empty-state
    // sentences every cluster tab lost. Structure says what these said.
    "← needs",
    "unblocks",
    "filed",
    "”  BLOCKING",
    "value 1", "value 2", "value 3", "value 4", "value 5", "value 6", "value 7", "value 8", "value 9",
    "schema v5 ·",
    "schema v3 (older",
    "Nothing is waiting",
    "Nothing matches",
    "Nothing sent",
    "No open",
    "No tasks",
    "No regions",
    "No running",
    "No parked",
    "No node",
    "(no body)",
    "OPEN QUESTIONS",
    "HUMAN ACTIONS",
    "▾ Regions —",
    "▾ Repos —",
    "DAG —",
    "│DAG",
    // The old DAG label, alone on its row (an artifact's handle that opens
    // with the word, `DAG sidecar for …`, is its content).
    "│ DAG  ",
    "│ DAG │",
    "step 1",
    "“question",
    "in regions",
    "LOG —",
    "parked —",
    "running —",
    "waiting for an account",
    // The feedback audit (0118, F37 F33 F10 F14; msg 55 "I don't want any
    // explanations", msg 33 "you don't have to say globs below"): every
    // caption, key hint and developer leak it found, cut. None may return,
    // on a cluster tab, a Config band, a confirm or a picker.
    "A NEW POOL INHERITS",
    "0 or over 100 disables",
    "nothing is killed",
    "new work parks",
    "the soup, settings, env.toml",
    "applies on daemon restart",
    "from claude_code, with its settings",
    "max spendable",
    "R · n · S",
    "fell below",
    "n·S·cap",
    "made by",
    "rename, move and delete are for",
    "over which list",
    "on which value",
    "LEDGER_MAX_BYTES",
    "shapewire",
    "by the seed",
    "/data/settings/",
    "set by the divider",
    "Enter deletes it",
    "Enter removes it",
    "Enter unregisters it",
    "Esc keeps it",
    "Enter does it",
    "any other key keeps it",
    "below ·",
    // The retired trace root (task 0119); an artifact's path is content.
    "• ~/code/harness/.mind/workspaces",
    // The Bus modal's empty-state sentences (msg 235 adversarial pass, task
    // 0190): an empty band's metadata is all it says.
    "no trigger wakes",
    "is not recorded here",
    "it carries no payload",
    "this event did not fire it",
    // msg 165: the old Settings tab's breadcrumb.
    "Settings ›",
    // msg 186 (task 0166): no selection bar anywhere (S3), no square in the
    // sidebar (V8), and the old knob toggle (A5) never comes back.
    "▌",
    "▪",
    "──●",
    "●──",
    // msg 235 (task 0191): no override marker — "just say what the default
    // is" — no dots on a toggle, no `↑pool` provenance, and no lifted
    // threshold (an opportunistic account is simply spent freely).
    "● override",
    "override (",
    "inherited ·",
    "↑pool",
    "lifted",
    "○ on",
    "○ off",
    "● brewing",
    "○ banked",
    // msg 235 C9 (task 0192): the inference endpoints' design mockups wrote
    // hints beside some rows — none of them is drawn, and no OpenRouter
    // source or planned one comes back.
    "x removes",
    "listed from",
    "ignored by",
    "· read-only",
    "paste the",
    "Esc closes",
    "+ add API key",
    "+ add token source",
    "planned",
];

/// What is STATUS, never config (msg 235: "I can see you've got share total
/// and pressure and stuff ... that would be stuff that's in status, right?
/// It does not belong in config"): none of it on any Config tab or modal.
const STATUS_ON_CONFIG: &[&str] = &["share total", "pressure", "last flip", "flipped", "proven good", "spent", "unranked", "R 1", "calibrating", "main @", "claimed by", "worktree", "subscription", "this period", "resets in", " left"];

/// A workspace name, or the word workspace used as a label.
const WORKSPACE_LABELS: &[&str] =
    &["WORKSPACE", "workspace cauldron", "workspace broomstick", "workspace harness", "workspace is v", "workspace scratch"];

/// Label words that began a row of the old Questions tab (`why`,
/// `recommend`, `options`): a row of the page that STARTS with one is a label.
/// (A question's box has its own parts — Context, Options, Recommendation —
/// inside its border, which is structure, msg 165.)
const LABEL_ROWS: &[&str] = &["why  ", "recommend "];

pub(crate) fn assert_clean(s: &str, what: &str) {
    assert_clean_but(s, what, &[]);
}

/// [`assert_clean`], but for `prose`: words that are a page's CONTENT where
/// it quotes what a ripple does (its job, its prompt), not a label.
fn assert_clean_but(s: &str, what: &str, prose: &[&str]) {
    for l in LEGENDS.iter().chain(WORKSPACE_LABELS).filter(|l| !prose.contains(l)) {
        assert!(!s.contains(l), "{what}: `{l}` is on screen:\n{s}");
    }
    for row in s.lines() {
        let page = row.split('│').nth(2).unwrap_or("").trim_start_matches(['▌', ' ']);
        for l in LABEL_ROWS {
            assert!(!page.starts_with(l), "{what}: a row starts with the label `{l}`:\n{s}");
        }
        assert!(page.trim_end() != "options", "{what}: the label `options` is a row:\n{s}");
    }
}

/// A frame tall enough for every page in the seed, short enough to draw
/// thousands of.
fn sweep(a: &mut App) -> String {
    render_to_text(a, 120, 110).unwrap().join("\n")
}

/// Every band of a cluster's page that opens.
const BANDS: [Band; 12] = [
    Band::Questions(QBand::Active),
    Band::Questions(QBand::Complete),
    Band::Questions(QBand::Superseded),
    Band::Questions(QBand::Archived),
    Band::Actions(HaBand::Active),
    Band::Actions(HaBand::Done),
    Band::Actions(HaBand::Archived),
    Band::Ripples,
    Band::RippleState(RBand::Active),
    Band::RippleState(RBand::Parked),
    Band::RippleState(RBand::Pending),
    Band::RippleState(RBand::Background),
];

/// Every band of the page open (or, `false`, every band as it opens).
pub(crate) fn all_bands_open(a: &mut App, open: bool) {
    a.bands = BANDS.iter().chain([Band::Engine].iter()).copied().filter(|b| open && !b.open_by_default()).collect();
    a.art_toggled.clear();
    if open && a.ktab == KTab::Nodes {
        open_every_art_band(a);
    }
}

/// Open every band, worktree and artifact of the Artifacts tab, and extend
/// every `n more ▸` — but not the directories of the disk (the FILES and
/// FILE SYSTEM roots open, one level).
fn open_every_art_band(a: &mut App) {
    for _ in 0..8 {
        let page = tui::cauldron::pages::page(a, 100);
        let mut changed = false;
        for (i, r) in page.arts.iter().enumerate() {
            let _ = i;
            if !r.opens || r.key.starts_with("dir:") || r.key.starts_with("vwt:") || a.art_toggled.contains(&r.key) {
                continue;
            }
            let open = r.open;
            if !open || r.key.starts_with("more:") {
                a.art_toggled.insert(r.key.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

/// Open every band of the cauldron's Status tab (the devices, each device,
/// each service, the token sources, each account): its keys are kept in
/// `stnav` as turned from their default.
fn open_every_status_band(a: &mut App) {
    for _ in 0..8 {
        let closed: Vec<String> = tui::cauldron::status::view(a, 120)
            .items
            .into_iter()
            .filter(|it| it.act == tui::cauldron::status::SAct::Band && !tui::cauldron::status::is_open(a, &it.key))
            .map(|it| it.key)
            .collect();
        if closed.is_empty() {
            return;
        }
        for k in closed {
            if !a.stnav.remove(&k) {
                a.stnav.insert(k);
            }
        }
    }
}

/// Open every band of a Config tab, however deep: each opened band shows
/// the bands it holds, which open on the next round.
fn open_every_config_band(a: &mut App) -> usize {
    let mut opened = 0;
    for _ in 0..12 {
        let closed: Vec<_> = a.entries().into_iter().filter(|f| f.open == Some(false)).filter_map(|f| f.entry.band()).collect();
        if closed.is_empty() {
            break;
        }
        for b in closed {
            a.snav.set_open(b, true);
            opened += 1;
        }
    }
    opened
}

/// Laid-out lines as the text they draw (a page taller than any test
/// frame can hold: a TestBackend buffer stops at 65 535 cells).
fn text_of(lines: Vec<ratatui::text::Line<'static>>) -> String {
    lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join("\n")
}

/// A Config tab's every line as it is open, at the width a 120-column
/// frame gives it.
fn config_text(a: &App) -> String {
    text_of(tui::cauldron::view::settings_entry_lines(a, 114).0)
}

/// No cluster tab explains itself (msg 55, msg 82, msg 165): every tab of
/// every cluster — Overview's boxes, both Tasks sub-tabs with every band open
/// and shut, Soup, Artifacts, Status's ripples and engine — own and
/// aggregate, and every page that opens from one: a trace, the log, a region,
/// a node, a repo and its entries. (Config, which is bands, is swept below.)
#[test]
fn no_cluster_tab_carries_a_legend_a_label_or_an_empty_state_sentence() {
    let mut a = app();
    let tabs_swept = sweep_every_cluster_tab(&mut a);
    assert!(tabs_swept > 100, "{tabs_swept}");
}

/// The same sweep on a cauldron that carries its OWN seed (task 0168, the
/// design cauldron's shape): the fixture always, and the real design
/// cauldron on this machine when it exists (copied into a scratch root, so
/// nothing is written there). Its hand-curated cards are held to the same
/// laws as the repo's fixtures, every cauldron tab with them.
#[test]
fn no_screen_of_a_cauldron_with_its_own_seed_carries_a_legend() {
    let root = scratch_root();
    own_seed_fixture::write(&root, "design");
    let mut cauldrons = vec![("fixture", open(&root, "design"))];
    if let Some(real) = own_seed_fixture::real_design_seed() {
        let r2 = scratch_root();
        own_seed_fixture::copy_real_design_seed(&real, &r2, "design");
        cauldrons.push(("the design cauldron", open(&r2, "design")));
    }
    for (what, mut a) in cauldrons {
        let n = sweep_every_cluster_tab(&mut a);
        assert!(n > 10, "{what}: {n}");
        for t in CTab::ALL {
            a.set_ctab(t);
            assert_clean(&sweep(&mut a), &format!("{what}: {}", t.label()));
        }
        a.set_ctab(CTab::Status);
        for t in tui::cauldron::status::StatusTab::ALL {
            a.set_sttab(t);
            open_every_status_band(&mut a);
            assert_clean(&sweep(&mut a), &format!("{what}: Status › {}", t.label()));
        }
    }
}

/// Every tab of every cluster of `a`, every band open and shut, and the
/// first pages that open from each, held to [`assert_clean`]. Returns how
/// many tabs it swept.
fn sweep_every_cluster_tab(a: &mut App) -> usize {
    let mut tabs_swept = 0;
    let clusters: Vec<usize> = (0..a.forest.clusters.len()).collect();
    for g in clusters {
        let roots_only = if a.forest.get(g).parent.is_none() { vec![false, true] } else { vec![false] };
        for aggregate in roots_only {
            for t in KTab::ALL {
                for open in [false, true] {
                    a.select(g);
                    a.aggregate = aggregate;
                    a.set_ktab(t);
                    all_bands_open(a, open);
                    let what = format!("{} {t:?} aggregate={aggregate} bands open={open}", a.forest.get(g).key);
                    let s = sweep(a);
                    assert_clean(&s, &what);
                    tabs_swept += 1;
                    // The description is on Overview (its Scope box) and nowhere else.
                    let desc = a.forest.get(g).description.clone();
                    if t != KTab::Details && !desc.is_empty() {
                        // Wide enough that the paragraph would sit on one row.
                        let wide = render_to_text(a, 400, 100).unwrap().join("\n");
                        assert!(!wide.contains(&desc), "{what} repeats the description");
                    }
                    // Open every item one level, and its page.
                    let n = a.items().len();
                    for i in 0..n.min(14) {
                        a.set_ktab(t);
                        a.focus = Focus::Content;
                        a.item = i;
                        if a.open_selected() {
                            let s = sweep(a);
                            if matches!(a.modal, Some(Modal::Artifact { .. })) {
                                // The artifact modal QUOTES a file — a design
                                // mockup's bars, knobs and key hints are its
                                // content, not the face's. The face under it
                                // is held to the law.
                                assert!(s.contains('┏'), "{what} item {i}: the artifact modal");
                                a.modal = None;
                                assert_clean(&sweep(a), &format!("{what} item {i}, under its artifact modal"));
                                while a.close_level() {}
                                continue;
                            }
                            assert_clean(&s, &format!("{what} item {i} opened"));
                            // One level deeper: the first openable rows on what opened.
                            let m = a.items().len();
                            for j in 0..m.min(2) {
                                a.item = j;
                                if a.open_selected() {
                                    assert_clean(&sweep(a), &format!("{what} item {i}/{j}"));
                                    a.close_level();
                                }
                            }
                        }
                        while a.close_level() {}
                        a.aggregate = aggregate;
                        all_bands_open(a, open);
                    }
                    a.aggregate = false;
                }
            }
        }
    }
    tabs_swept
}

/// No cauldron tab explains itself: Work, Status with every band open (each
/// device, service, token source and account), System on every ripple's
/// pages, and Config's top.
/// The bucket in every state (msg 235: one bucket, every paste a chip, no
/// dictation): empty, holding notes with a reference and a paste chip, and
/// the chip's modal — no legend, no label, no hint. Its box's one legend is
/// `bucket`, the soup's `soup` (S1), and the DEV rename modal says nothing
/// but its title either.
#[test]
fn no_bucket_state_carries_a_legend() {
    let mut a = app();
    assert_clean(&sweep(&mut a), "the bucket, empty");
    a.focus = Focus::Content;
    a.set_ktab(KTab::Questions);
    a.handle(key(KeyCode::Char('i')));
    typed(&mut a, " four");
    a.handle(Event::Paste("fn main() {\n    one();\n    two();\n}".into()));
    a.focus = Focus::Tree;
    assert_clean(&sweep(&mut a), "the bucket, notes and chips");
    a.focus_input();
    {
        let kind = "paste";
        let t = tui::cauldron::tags::scan(&a.composer.text()).into_iter().map(|(_, t)| t).find(|t| t.chip_id().is_some_and(|(k, _)| k == kind)).unwrap();
        a.open_chip(&t);
        assert!(a.modal.is_some(), "{kind}");
        assert_clean(&sweep(&mut a), &format!("the {kind} chip's modal"));
        a.close_modal();
    }
}

/// The Threads tab (msg 235; it was a mode, msg 186) carries no legend either: the list with
/// every band open, every thread, every subagent's transcript, every tool
/// call and batch opened, the message box in use, and a message sent.
#[test]
fn no_threads_mode_screen_carries_a_legend() {
    use tui::cauldron::threads::TFocus;
    let mut a = app();
    a.set_ctab(CTab::Threads);
    crate::live_fixture::feed(&mut a);
    assert_clean(&sweep(&mut a), "Threads, the list");
    a.threads.bands.insert("archived".into());
    assert_clean(&sweep(&mut a), "Threads, the list, every band open");
    for t in crate::live_fixture::LISTED {
        crate::live_fixture::open(&mut a, t);
        let subjects: Vec<Option<String>> = std::iter::once(None).chain(a.threads.agent_rows.iter().map(|x| Some(x.id.clone()))).collect();
        for subject in subjects {
            a.threads.set_subject(subject.clone());
            tui::cauldron::threads::settle(&mut a);
            crate::live_fixture::transcript(&mut a);
            for i in 0..a.threads.blocks().len() {
                a.threads.chat_item = i;
                a.handle(key(KeyCode::Right));
                if a.threads.subject != subject {
                    a.threads.set_subject(subject.clone());
                }
            }
            assert_clean(&sweep(&mut a), &format!("Threads, thread {t}, {subject:?}, everything opened"));
        }
        a.threads.set_subject(None);
        a.threads.focus = Some(TFocus::Input);
        typed(&mut a, "a message");
        assert_clean(&sweep(&mut a), &format!("Threads, thread {t}, the message box in use"));
        a.handle(key(KeyCode::Enter));
        assert_clean(&sweep(&mut a), &format!("Threads, thread {t}, a message sent"));
        a.threads.close_thread();
    }
}

#[test]
fn no_cauldron_tab_carries_a_legend_on_any_band_or_page() {
    use tui::cauldron::bus::BusPage;
    let mut a = app();
    for t in CTab::ALL {
        a.set_ctab(t);
        assert_clean(&sweep(&mut a), t.label());
    }
    // Status: every sub-tab (msg 186), every band open.
    a.set_ctab(CTab::Status);
    let mut all = String::new();
    for t in tui::cauldron::status::StatusTab::ALL {
        a.set_sttab(t);
        open_every_status_band(&mut a);
        let s = text_of(tui::cauldron::status::view(&a, 114).lines);
        assert_clean(&s, &format!("Status › {}, every band open", t.label()));
        assert_clean(&sweep(&mut a), &format!("Status › {}, on screen", t.label()));
        all.push_str(&s);
    }
    assert!(all.contains("copy log link") && all.contains("config ▸") && all.contains("pressure") && all.contains("Running") && all.contains("Unclaimed regions"), "every band open:\n{all}");
    // The Bus modal (msg 235): every page reachable from the Bus tab —
    // events, triggers, ripples, executions and their steps.
    a.set_sttab(tui::cauldron::status::StatusTab::Bus);
    open_every_status_band(&mut a);
    let mut todo: std::collections::VecDeque<BusPage> = tui::cauldron::status::view(&a, 114).items.into_iter().filter_map(|i| match i.act { tui::cauldron::status::SAct::Bus(p) => Some(p), _ => None }).collect();
    let mut seen = std::collections::BTreeSet::new();
    while let Some(p) = todo.pop_front() {
        if !seen.insert(format!("{p:?}")) || seen.len() > 300 {
            continue;
        }
        a.open_bus(p.clone());
        let laid = a.bus_laid(tui::cauldron::bus::BUS_WIDTH).unwrap();
        // A ripple's job line quotes what it does ("unblocks tasks the
        // commit touched"): that is the ripple's content.
        assert_clean_but(&sweep(&mut a), &format!("Bus {p:?}"), &["unblocks"]);
        todo.extend(laid.links.into_iter().map(|(_, q)| q));
        a.modal = None;
    }
    assert!(seen.len() > 20, "only {} bus pages", seen.len());
    // The DEV rename modal and the New cluster modal.
    a.set_ctab(CTab::Cauldron);
    a.modal = Some(Modal::DevRename { key: "surface".into(), name: "surface".into() });
    assert_clean(&sweep(&mut a), "the DEV rename modal");
    a.modal = None;
    a.open_new_cluster(None);
    assert_clean(&sweep(&mut a), "the New cluster modal");
    a.modal = None;
}

/// No Config band explains itself (msg 55, msg 165; audit 0118 F37): the
/// cauldron's Config with EVERY band open, and every confirm a management
/// act asks. A setting's explanation is
/// in its Help modal and nowhere on the page.
#[test]
fn no_config_band_confirm_or_picker_carries_a_cut_explanation() {
    use tui::cauldron::menu::{Entry, Nav};
    let mut a = app();
    a.set_ctab(CTab::Config);
    // Both tabs (msg 186), every band open.
    let mut opened = 0;
    let mut s = String::new();
    let mut modals = 0;
    for t in tui::cauldron::menu::CfgTab::ALL {
        a.set_cfg_tab(t);
        opened += open_every_config_band(&mut a);
        let one = config_text(&a);
        assert_clean(&sweep(&mut a), &format!("Config › {}, every band open, on screen", t.label()));
        assert_clean(&one, &format!("Config › {}, every band open", t.label()));
        // (`on disk pressure` is a setting: collect when the disk is full.)
        let bare = one.replace("on disk pressure", "");
        for w in STATUS_ON_CONFIG {
            assert!(!bare.contains(w), "Config › {}: `{w}` is status, not config:\n{one}", t.label());
        }
        s.push_str(&one);
        // Every table row's modal (msg 235), swept the same way.
        let rows: Vec<tui::cauldron::menu::SPage> = a.selectable().into_iter().filter_map(|e| if let Entry::Row(p) = e { Some(p) } else { None }).collect();
        for p in rows {
            a.open_row(p.clone());
            let m = sweep(&mut a);
            assert_clean(&m, &format!("Config › {}, the modal of {p:?}", t.label()));
            let inside: String = m.lines().filter(|l| l.contains('┃')).collect::<Vec<_>>().join("\n");
            for w in STATUS_ON_CONFIG {
                assert!(!inside.contains(w), "{p:?}'s modal: `{w}` is status, not config:\n{m}");
            }
            a.modal = None;
            modals += 1;
        }
        // Every `+ add endpoint`'s modal (design 0199 §3, §4), as each
        // template and each local server draws it.
        let adds: Vec<tui::cauldron::manage::Act> = a.selectable().into_iter().filter_map(|e| match e {
            Entry::Do { act: act @ tui::cauldron::manage::Act::NewEndpoint { .. }, .. } => Some(act),
            _ => None,
        }).collect();
        for act in adds {
            let here = matches!(act, tui::cauldron::manage::Act::NewEndpoint { here: true });
            for (ti, t) in tui::cauldron::settings::TEMPLATE_IDS.iter().enumerate() {
                for server in if *t == "local" { 0..3 } else { 0..1 } {
                    a.start_act(act.clone());
                    a.set_option(&tui::cauldron::menu::ep_bound(here, "ep-0", "template"), ti);
                    a.set_option(&tui::cauldron::menu::ep_bound(here, "ep-0", "server"), server);
                    let m = sweep(&mut a);
                    assert_clean(&m, &format!("Config › {}, the add modal, {t} {server}", t));
                    let inside: String = m.lines().filter(|l| l.contains('┃')).collect::<Vec<_>>().join("\n");
                    for w in STATUS_ON_CONFIG {
                        assert!(!inside.contains(w), "the add modal ({t}): `{w}` is status, not config:\n{m}");
                    }
                    a.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
                    assert!(a.modal.is_none() && a.settings.endpoint_ids(here).iter().all(|id| id != "ep-0"));
                    modals += 1;
                }
            }
        }
    }
    assert!(modals > 30, "{modals} row modals swept");
    // An account, a role and a service are a table's ROW now, opened in a
    // modal, not a band (msg 235).
    assert!(opened > 30, "{opened}");
    // A table row's `›` ends its row; a breadcrumb's sits between words.
    assert!(!s.match_indices(" › ").any(|(i, _)| s[i + " › ".len()..].chars().next().is_some_and(char::is_alphanumeric)), "no breadcrumb:\n{s}");
    // A value from the cauldron's own level is the default (msg 165).
    assert!(!s.contains("inherited · cauldron") && s.contains("default"), "{s}");
    // No planned token source (design 0199: they are endpoint templates).
    assert!(!s.contains("planned"), "{s}");
    // Every setting's explanation is in its Help modal, never on the page.
    let mut helped = 0;
    let mut every: Vec<Entry> = Vec::new();
    for t in tui::cauldron::menu::CfgTab::ALL {
        a.set_cfg_tab(t);
        every.extend(a.selectable());
    }
    for e in every {
        let (Entry::Set(bd) | Entry::Run(bd)) = e else { continue };
        let text = tui::cauldron::settings::help(bd.def());
        let head: String = text.chars().take(36).collect();
        if head.chars().count() < 36 {
            continue;
        }
        assert!(!s.contains(&head), "{}: its explanation is on the page: {head}", bd.key);
        a.open_help(&bd);
        let Some(Modal::Help { text: t, .. }) = &a.modal else { panic!("{:?}", a.modal) };
        assert_eq!(t, &text);
        a.modal = None;
        helped += 1;
    }
    assert!(helped > 40, "{helped}");
    a.snav = Nav::cauldron();
    // Every confirm: a body, and no key hints.
    use tui::cauldron::manage::Act;
    for act in [Act::DeleteCluster("X-1".into()), Act::RemoveAccount { pool: "claude_code".into(), id: "acct2".into() }, Act::UnregisterRepo("harness".into())] {
        a.start_act(act.clone());
        let s = sweep(&mut a);
        assert!(matches!(a.modal, Some(Modal::Confirm { .. })), "{act:?}");
        assert_clean(&s, &format!("{act:?}"));
        a.modal = None;
    }
}

// ── the tree ────────────────────────────────────────────────────────────────

#[test]
fn sidebar_rows_carry_no_counts_and_each_has_its_plus() {
    let mut a = app();
    let s = screen(&mut a);
    let tree_cell = |name: &str| -> String {
        let l = s.lines().find(|l| l.contains(name)).unwrap_or_else(|| panic!("{name}:\n{s}"));
        l.split('│').nth(1).unwrap().trim().to_string()
    };
    // The disclosure, the name and the `+` beside it: no `3q:2a:2p` on any row.
    for (row, name) in [("▾ Broomstick", "▾ Broomstick"), ("· Landing", "· Landing"), ("▾ Cauldron", "▾ Cauldron")] {
        let cell = tree_cell(row);
        let (left, plus) = cell.split_at(cell.len() - 1);
        // No marker but the chevron (msg 186, task 0166 V8): no `▪`.
        assert_eq!((left.trim_end(), plus), (name, "+"), "{cell}");
    }
    for l in s.lines() {
        let cell = l.split('│').nth(1).unwrap_or("");
        assert!(!cell.chars().any(|c| c.is_ascii_digit()), "a count on `{cell}`");
    }
    // The header says Clusters with its `+`; there is no `+ new cluster` row.
    let head = tree_cell("Clusters");
    assert!(head.starts_with("Clusters") && head.ends_with('+'), "{head}");
    assert!(!s.contains("new cluster"), "{s}");
    assert_clean(&s, "Work");
}

#[test]
fn right_expands_then_enters_left_collapses_then_climbs() {
    let mut a = app();
    let cauldron = id(&a, "X-1");
    assert_eq!(a.selected, cauldron);
    assert!(a.is_expanded(cauldron));

    // Left on an open cluster closes it; its children leave the tree.
    a.handle(key(KeyCode::Left));
    assert!(!a.is_expanded(cauldron));
    let s = screen(&mut a);
    assert!(s.contains("▸ Cauldron"), "{s}");
    assert!(!s.contains("· Interface"), "{s}");

    // Right on a closed cluster opens it; focus stays in the tree.
    a.handle(key(KeyCode::Right));
    assert!(a.is_expanded(cauldron));
    assert_eq!(a.focus, Focus::Tree);
    assert!(screen(&mut a).contains("· Interface"));

    // Right on an open cluster steps INTO its content; Left comes back out
    // (Overview's first row is its open Scope band: ← closes it first).
    a.handle(key(KeyCode::Right));
    assert_eq!(a.focus, Focus::Content);
    a.handle(key(KeyCode::Left));
    assert_eq!(a.focus, Focus::Content, "← closed the Scope band");
    a.handle(key(KeyCode::Left));
    assert_eq!(a.focus, Focus::Tree);
    a.bands.clear();

    // Down twice: Interface, then Ripples. Left closes Ripples; Left again climbs.
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::Down));
    let ripples = id(&a, "X-3");
    assert_eq!(a.selected, ripples);
    a.handle(key(KeyCode::Left));
    assert!(!a.is_expanded(ripples));
    // Gone from the sidebar.
    let s = screen(&mut a);
    assert!(!s.lines().any(|l| l.split('│').nth(1).unwrap_or("").contains("Dreaming")), "{s}");
    a.handle(key(KeyCode::Left));
    assert_eq!(a.selected, cauldron);

    // Down walks only VISIBLE rows: Ripples' child is skipped while it is closed.
    for _ in 0..3 {
        a.handle(key(KeyCode::Down));
    }
    assert_eq!(a.selected, id(&a, "X-5"));

    // Left on a leaf goes to its parent; at a closed root it stays.
    a.handle(key(KeyCode::Left));
    assert_eq!(a.selected, cauldron);
    a.handle(key(KeyCode::Left));
    a.handle(key(KeyCode::Left));
    assert_eq!((a.selected, a.focus), (cauldron, Focus::Tree));
    assert!(!a.quit);
}

#[test]
fn mouse_selects_rows_and_toggles_the_disclosure() {
    let mut a = app();
    let broom = id(&a, "X-6");
    click_on(&mut a, Hit::TreeRow(broom));
    assert_eq!(a.selected, broom);
    assert!(a.is_expanded(broom));
    click_on(&mut a, Hit::Disclosure(broom));
    assert!(!a.is_expanded(broom));
    assert!(!screen(&mut a).contains("· Landing"));
    click_on(&mut a, Hit::Disclosure(broom));
    assert!(a.is_expanded(broom));
}

// ── the cluster page ───────────────────────────────────────────────────────────

#[test]
fn there_is_no_cluster_header_and_only_questions_and_actions_carry_a_count() {
    let mut a = app();
    let s = screen(&mut a);
    // msg 235: no header over the cluster tabs — the sidebar's selected row
    // says which cluster this is. 8e4fca10 msg 385 flattened Human: Questions
    // and Actions each carry their own count.
    assert!(!s.lines().any(|l| l.split('│').nth(2).is_some_and(|h| h.trim() == "Cauldron")), "no header: {s}");
    assert!(s.contains("Questions 3 | Actions 1 | Tasks | Nodes | Status | Details"), "{s}");
    assert_eq!(a.ktab_title(KTab::Questions), format!("Questions {}", a.active_questions()));
    assert_eq!(a.ktab_title(KTab::Actions), format!("Actions {}", a.active_actions()));
    assert_eq!((a.active_questions(), a.active_actions()), (3, 1));
    for t in KTab::ALL.into_iter().filter(|t| !matches!(t, KTab::Questions | KTab::Actions)) {
        assert_eq!(a.ktab_title(t), t.label(), "{t:?} carries no number");
    }
    // With aggregate on, the Tasks count covers the subtree; the strip says
    // nothing more (the sidebar paints the subtree).
    a.select(id(&a, "X-1"));
    a.handle(ctrl('g'));
    let s = screen(&mut a);
    let strip = s.lines().find(|l| l.contains(" | Actions ")).expect(&s);
    assert!(strip.contains(&format!("Questions {} | Actions {}", a.active_questions(), a.active_actions())) && !strip.contains("aggregate"), "{strip}");
    assert!(a.active_questions() > 3, "the subtree's questions count");
    assert_eq!(a.cluster_regions().len(), 6);
}

#[test]
fn the_cluster_strip_is_the_pages_first_row_and_the_content_starts_under_it_on_every_tab() {
    // msg 235: no header over the strip, no rule under it — the strip is
    // the first row of the soup box's page, the content the next.
    let mut a = app();
    for t in KTab::ALL {
        a.set_ktab(t);
        let rows = render_to_text(&mut a, 120, 40).unwrap();
        let page = |y: usize| rows[y].split('│').nth(2).unwrap_or("").trim().to_string();
        assert!(rows[1].starts_with('╭') && rows[1].contains(" surface "), "{t:?} the surface box: {:?}", rows[1]);
        assert!(page(2).starts_with("Questions "), "{t:?} strip: {:?}", rows[2]);
        assert!(!page(3).is_empty() && !page(3).chars().all(|c| c == '─'), "{t:?}: no rule, no blank under the tabs: {:?}", rows[3]);
    }
}

#[test]
fn own_questions_only_by_default_and_what_is_below_is_one_footer() {
    let mut a = app();
    a.set_ktab(KTab::Questions);
    let s = screen(&mut a);
    // A question is a box: its id the legend at the top right.
    assert!(s.lines().any(|l| l.contains("◆ Q-1 ─╮")), "{s}");
    assert!(s.contains("When the system is nearly certain"), "{s}");
    // Interface's focus nag counts with its questions (iteration nine).
    let below = s.lines().find(|l| l.contains(" below")).expect(&s);
    let below = below.split('│').nth(2).unwrap().trim();
    assert!(below.ends_with(" below") && below.split(' ').next().unwrap().parse::<usize>().is_ok(), "a bare count, no cluster names (msg 33, F10): {below}");
    assert!(!s.contains("Q-1 · Interface") && !s.contains("▾ Interface"), "descendant question leaked: {s}");
    assert!(!s.contains("CLUSTERS BELOW"), "the tree already shows the clusters below: {s}");
    // → on the footer turns aggregate on.
    select_item(&mut a, Item::Footer(Footer::Below));
    a.handle(key(KeyCode::Right));
    assert!(a.aggregate);
}

/// Re-audit 0126 F10: on Soup and Artifacts too (and on the Human Actions
/// sub-tab and Status), → on `n below` turns aggregate on (their own →
/// navigation used to swallow the footer).
#[test]
fn below_footer_opens_aggregate_on_soup_artifacts_actions_and_status_too() {
    for (tab, cluster) in [
        (KTab::Nodes, None),
        (KTab::Nodes, Some("X-9")),
        (KTab::Actions, None),
        (KTab::Status, None),
    ] {
        let mut a = app();
        if let Some(k) = cluster {
            a.select(id(&a, k));
        }
        a.set_ktab(tab);
        // The page's `n below`, not the sidebar's `Aggregated clusters`
        // (b964d571 msg 23).
        let below = |s: &str| s.lines().any(|l| l.split('│').nth(2).is_some_and(|p| p.contains(" below")));
        let s = screen(&mut a);
        let footer = a.items().iter().position(|i| *i == Item::Footer(Footer::Below)).unwrap_or_else(|| panic!("{tab:?}: no `n below` footer: {s}"));
        assert!(below(&s), "{tab:?}: {s}");
        a.focus = Focus::Content;
        a.item = footer;
        a.handle(key(KeyCode::Right));
        assert!(a.aggregate, "{tab:?}: → on the footer turns aggregate on");
        assert!(!below(&screen(&mut a)), "{tab:?}: aggregate has nothing below");
    }
}

#[test]
fn aggregate_names_each_descendant_question_by_its_cluster_in_tree_order() {
    let mut a = app();
    a.set_ktab(KTab::Questions);
    a.handle(ctrl('g'));
    assert!(a.aggregate);
    let s = screen(&mut a);
    let own = s.find("When the system is nearly certain").unwrap();
    let interface = s.find("· Interface ─╮").expect(&s);
    let ripples_q = s.find("Q-15 · Ripples ─╮").expect(&s);
    let dreaming = s.find("· Dreaming ─╮").expect(&s);
    assert!(own < interface && interface < ripples_q && ripples_q < dreaming, "{s}");
    assert!(s.contains("May Cauldron discover"), "{s}");
    assert!(!a.items().contains(&Item::Footer(Footer::Below)), "aggregate has nothing below: {s}");
}

// ── select, then insert ─────────────────────────────────────────────────────

#[test]
fn a_click_selects_and_i_inserts_and_printable_keys_type_only_into_a_focused_dump() {
    let root = scratch_root();
    let mut a = open(&root, "prod");
    a.set_ktab(KTab::Questions);
    // Q-1's options are items inside its box: always there, nothing opens.
    let opt3 = at(&a, Item::Option { cluster: 0, serial: 1, opt: 3 });
    assert_eq!(at(&a, Item::Option { cluster: 0, serial: 1, opt: 1 }), at(&a, Item::Question { cluster: 0, serial: 1 }) + 1);
    // msg 235: a click on an item selects it AND inserts its reference at
    // the bucket's caret; the page keeps the keys.
    click_on(&mut a, Hit::Item(opt3));
    assert_eq!(a.item, opt3);
    assert_eq!(a.focus, Focus::Content);
    assert_eq!(a.composer.text().trim_end(), "[Q-1.opt3]", "a click inserts its reference");
    assert_eq!(caret_row(&mut a), None, "no caret while a pane has the keys");
    // Right after that click, what he types is text, in the bucket (0255
    // H2, b964d571 msg 23: type a space to keep several).
    typed(&mut a, " xyz");
    assert_eq!(a.composer.text(), "[Q-1.opt3]  xyz");
    assert_eq!(a.focus, Focus::Input);
    a.composer = tui_render::compose::Composer::new();
    a.focus = Focus::Content;
    a.click_inserted = None;
    // Otherwise printable keys outside the dump are commands, never text.
    typed(&mut a, "xyz");
    assert!(a.composer.is_blank(), "keys outside the dump typed: {}", a.composer.text());
    assert_eq!(a.focus, Focus::Content);
    // `i` QUOTES the selected item into the empty draft — no new lines above
    // it — and hands the dump the focus.
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [Q-1.opt3]");
    assert_eq!(a.focus, Focus::Input);
    // The caret shows only while the dump has the focus.
    assert_eq!(caret_row(&mut a), Some(36), "the caret sits on the bucket's first line");
    // Now keys type — including `i`.
    typed(&mut a, " is it");
    assert_eq!(a.composer.text(), "> [Q-1.opt3] is it");
    // ↓ on the dump's last line goes nowhere; ↑ on its top line hands the
    // selection back to the pane it came from WITHOUT moving it; then ↓
    // moves it, and `i` again quotes, below what is there.
    a.handle(key(KeyCode::Down));
    assert_eq!(a.focus, Focus::Input);
    a.handle(key(KeyCode::Up));
    assert_eq!(a.focus, Focus::Content);
    assert_eq!(a.item, opt3);
    a.handle(key(KeyCode::Down));
    assert_eq!(a.items()[a.item], Item::Option { cluster: 0, serial: 1, opt: 4 });
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [Q-1.opt3] is it\n\n> [Q-1.opt4]");

    // Over to Broomstick's questions: click its question, `i`.
    let broom = id(&a, "X-6");
    click_on(&mut a, Hit::TreeRow(broom));
    assert_eq!(a.ktab, KTab::Questions, "the tab stays when the cluster changes");
    let q7 = at(&a, Item::Question { cluster: broom, serial: 7 });
    a.focus = Focus::Content;
    a.item = q7;
    a.handle(key(KeyCode::Char('i')));
    write(&mut a, " yes");
    // msg 235: ONE bucket per cauldron — Broomstick's quote goes below
    // Cauldron's, and the bucket is the same wherever he is.
    assert_eq!(a.composer.text(), "> [Q-1.opt3] is it\n\n> [Q-1.opt4]\n\n> [Q-7] yes");
    a.select(id(&a, "X-1"));
    assert_eq!(a.composer.text(), "> [Q-1.opt3] is it\n\n> [Q-1.opt4]\n\n> [Q-7] yes");
    let s = screen(&mut a);
    assert!(s.contains("> [Q-7] yes"), "{s}");

    // ONE send. It lands on both tagged clusters, and only there.
    a.handle(key(KeyCode::Enter));
    assert!(a.composer.is_blank());
    let send = a.sends.sends.last().unwrap().clone();
    assert_eq!(send.targets, vec!["X-1", "X-6"]);
    assert_eq!(send.tags, vec!["[Q-1.opt3]", "[Q-1.opt4]", "[Q-7]"]);
    assert_eq!(send.notes.len(), 3);
    assert_eq!(send.notes[2].anchors, vec!["Q-7"]);
    // Broomstick's log has it; Harness's has nothing from him.
    let focus = a.focus;
    a.select(id(&a, "X-6"));
    a.set_ktab(KTab::Status);
    open_log(&mut a);
    let s = screen(&mut a);
    assert!(s.contains("> [Q-1.opt3] is it > [Q-1"), "{s}");
    assert!(s.contains("[Q-1.opt3] [Q-1.opt4] [Q-7]"), "its tags: {s}");
    a.select(id(&a, "X-9"));
    a.set_ktab(KTab::Status);
    open_log(&mut a);
    assert!(!screen(&mut a).contains(&format!("#{} ", send.seq)), "harness was not tagged");
    a.trace.clear();
    a.select(id(&a, "X-1"));
    a.focus = focus;
    let log = std::fs::read_to_string(soup_dir(&a.env).unwrap().join("dumps.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 1);

    // Esc in the dump hands the selection back (0138 Q2); ← at the very
    // start of the draft leaves it too. `a` focuses it with no tag; Esc,
    // from a pane, hands it back.
    a.handle(key(KeyCode::Esc));
    assert_ne!(a.focus, Focus::Input);
    a.handle(key(KeyCode::Char('a')));
    a.handle(key(KeyCode::Left));
    assert_ne!(a.focus, Focus::Input);
    a.handle(key(KeyCode::Char('a')));
    assert_eq!(a.focus, Focus::Input);
    assert!(a.composer.is_blank());
    a.handle(key(KeyCode::Up));
    assert_ne!(a.focus, Focus::Input);
    a.handle(key(KeyCode::Esc));
    assert_eq!(a.focus, Focus::Input);
    a.handle(key(KeyCode::Up));
    assert_ne!(a.focus, Focus::Input);
    // A click on the dump focuses it too.
    click_on(&mut a, Hit::Input);
    assert_eq!(a.focus, Focus::Input);
}

#[test]
fn engine_rows_on_status_and_human_action_boxes_quote_with_i() {
    let mut a = app();
    // The Engine is on the Tasks tab (b964d571 msg 23), the third since
    // 8e4fca10 msg 385 (Questions, Actions, Tasks, …, Details).
    a.handle(opt('3'));
    assert_eq!(a.ktab, KTab::Tasks);
    let s = screen(&mut a);
    assert!(s.contains("DIR-1  One input, many surfaces, nobody in the middle."), "{s}");
    assert!(s.contains("T-2  First TUI"), "{s}");
    let engine = s.find("▾ Engine").expect(&s);
    assert!(engine < s[engine..].find("T-1 ").unwrap() + engine && s[engine..].find("T-1 ").unwrap() < s[engine..].find("T-2 ").unwrap(), "{s}");
    // Inside the Engine band: DIR-1's rule, OBJ-1, M-1, T-1, M-2, T-2.
    let items = a.items();
    let e = at(&a, Item::Band(Band::Engine));
    assert!(matches!(items[e + 1], Item::Directive { ix: 0, .. }), "{:?}", &items[e..]);
    assert_eq!(&items[e + 2..e + 7], &[
        Item::Objective { cluster: 0, serial: 1 },
        Item::Milestone { cluster: 0, serial: 1 },
        Item::Task { cluster: 0, serial: 1 },
        Item::Milestone { cluster: 0, serial: 2 },
        Item::Task { cluster: 0, serial: 2 },
    ]);
    a.handle(key(KeyCode::Right));
    a.item = e + 6;
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [T-2]");

    a.handle(key(KeyCode::Up));
    a.set_ktab(KTab::Actions);
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("● open · ") && l.contains(" steps ") && l.contains("HA-1 ─╮")), "{s}");
    assert!(s.contains("Approve the Cauldron TUI's environment layout"), "{s}");
    // The resolved one is under Done, shut until opened.
    assert!(has_band(&s, "▸ Done", 1) && !s.contains("Read the interface sketch"), "{s}");
    select_item(&mut a, Item::Band(Band::Actions(HaBand::Done)));
    a.handle(key(KeyCode::Right));
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("✓ resolved") && l.contains("HA-2 ─╮")), "{s}");
    assert!(s.contains("Read the interface sketch"), "{s}");
    select_item(&mut a, Item::Ha { cluster: 0, serial: 1 });
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [T-2]\n\n> [HA-1]");
    click_on(&mut a, Hit::KTab(KTab::Details));
    assert_eq!(a.ktab, KTab::Details);
}

// ── the draft: persisted, and shared live ──────────────────────────────────

#[test]
fn draft_survives_a_restart() {
    let root = scratch_root();
    {
        let mut a = open(&root, "prod");
        a.set_ktab(KTab::Questions);
        a.handle(key(KeyCode::Right));
        select_item(&mut a, Item::Question { cluster: 0, serial: 1 });
        a.handle(key(KeyCode::Char('i')));
        typed(&mut a, " half a thought");
        assert_eq!(a.composer.text(), "> [Q-1] half a thought");
    } // the process "exits"
    let mut b = open(&root, "prod");
    assert_eq!(b.composer.text(), "> [Q-1] half a thought");
    assert!(screen(&mut b).contains("> [Q-1] half a thought"));
}

#[test]
fn two_faces_on_one_environment_share_the_draft_and_the_sends() {
    let root = scratch_root();
    let mut a = open(&root, "prod");
    let mut b = open(&root, "prod");
    write(&mut a, "from a");
    b.sync();
    assert_eq!(b.composer.text(), "from a");
    b.handle(key(KeyCode::Char('a')));
    b.handle(key(KeyCode::End));
    typed(&mut b, ", then b");
    a.sync();
    assert_eq!(a.composer.text(), "from a, then b");
    a.sync();
    assert_eq!(a.composer.text(), "from a, then b");

    // a sends; b sees the send and the cleared draft.
    a.handle(key(KeyCode::Enter));
    b.sync();
    assert!(b.composer.is_blank());
    assert_eq!(b.sends.sends.len(), 2, "the seed's dump 11, and a's");
    // msg 235: untagged, it lands on no cluster's log — it went into the soup.
    assert_eq!(b.sends.sends.last().unwrap().text, "from a, then b", "b has the send");
    assert!(b.sends.sends.last().unwrap().targets.is_empty());

    // Both send without syncing: the sequence comes from the file, not the face.
    write(&mut a, "two");
    a.handle(key(KeyCode::Enter));
    write(&mut b, "three");
    b.handle(key(KeyCode::Enter));
    let seqs: Vec<u64> = b.sends.sends.iter().map(|s| s.seq).collect();
    assert_eq!(seqs, vec![11, 12, 13, 14]);
    a.sync();
    assert_eq!(a.sends.sends.len(), 4);
}

#[test]
fn a_partial_line_is_never_read() {
    let root = scratch_root();
    let mut a = open(&root, "prod");
    a.dump("whole");
    let path = soup_dir(&a.env).unwrap().join("dumps.jsonl");
    use std::io::Write;
    let mut fh = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    fh.write_all(b"{\"seq\":13,\"at\":\"x\"").unwrap();
    let mut b = open(&root, "prod");
    assert_eq!(b.sends.sends.len(), 2, "the seed's, and the whole line");
    fh.write_all(b",\"env\":\"prod\",\"from_cluster\":\"cauldron\",\"text\":\"t\",\"tags\":[],\"targets\":[\"cauldron\"]}\n").unwrap();
    b.sync();
    assert_eq!(b.sends.sends.len(), 3);
}

#[test]
fn headless_writes_nothing() {
    let root = scratch_root();
    let mut a = App::open(env_in(&root, "prod"), seed_dir(), false).unwrap();
    write(&mut a, "unsent words");
    a.handle(key(KeyCode::Enter));
    // Not even a new cauldron.
    a.open_cauldrons();
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::Enter));
    assert!(matches!(a.modal, Some(Modal::NewCauldron { .. })), "{:?}", a.modal);
    typed(&mut a, "trial");
    a.handle(key(KeyCode::Enter));
    assert!(!root.join("data").exists(), "a dump must not create anything");
    assert_clean(&screen(&mut a), "headless");
}

// ── the top bar and the cauldrons ───────────────────────────────────────────

#[test]
fn the_top_bar_is_cauldron_its_name_then_the_tabs_and_names_no_workspace() {
    let mut a = app();
    let rows = render_to_text(&mut a, 120, 40).unwrap();
    let top = &rows[0];
    let words: Vec<&str> = top.split_whitespace().collect();
    // No switch (msg 186: it is Config's first row now).
    // msg 235: the plain product name, the cauldrons dropdown, the tabs;
    // 8e4fca10 msg 457: the product name is gone, the environment leads.
    assert_eq!(&words[..3], &["prod", "▾", "Cauldron"], "{top}");
    assert_eq!(top.matches("Cauldron").count(), 1, "only the tab says Cauldron: {top}");
    assert!(top.ends_with("Cauldron | Threads | Status | Config"), "{top}");
    for gone in ["Cluster ·", "Energy", "Ripples", "Mesh", "Settings", "Soup", "System", "Services", "Contents"] {
        assert!(!top.contains(gone), "{gone}: {top}");
    }
    for tab in CTab::ALL {
        a.set_ctab(tab);
        assert_clean(&screen(&mut a), tab.label());
    }
    // Inside the soup, too.
    let mut a = app();
    a.open_ids(Some("R-4"), Some("DEC-23")).unwrap();
    assert_clean(&screen(&mut a), "an opened node");
    a.select(id(&a, "X-11"));
    a.open_ids(Some("SUB-4"), None).unwrap();
    assert_clean(&screen(&mut a), "an opened older region");
}

#[test]
fn clicking_the_name_opens_the_cauldrons_dropdown() {
    let mut a = app();
    click_on(&mut a, Hit::Env);
    assert!(a.modal.is_none(), "a dropdown, not a modal");
    let d = a.dropdown.clone().expect("the dropdown");
    assert_eq!(d.entries, vec![("prod".to_string(), true)], "only cauldrons that exist, and whether each is on");
    let rows = render_to_text(&mut a, 120, 60).unwrap();
    let s = rows.join("\n");
    // Untitled; each cauldron says whether it is on; the last row is `+`.
    assert!(!s.contains("Cauldrons"), "{s}");
    let prod = rows.iter().position(|r| r.contains("│ prod ") && r.contains(" enabled ")).expect(&s);
    assert!(rows[prod + 1].contains("│ +"), "{s}");
    assert_clean(&s, "the cauldrons dropdown");
    // It hangs under the name, left-aligned with it.
    let (r, _) = *a.hits.iter().find(|(_, h)| *h == Hit::Env).unwrap();
    let name: String = rows[0].chars().skip(r.x as usize).take(r.width as usize).collect();
    assert_eq!(name.split_whitespace().collect::<Vec<_>>(), ["prod", "▾"]);
    assert_eq!(a.dropdown_anchor, (r.x, 1));
}

#[test]
fn a_click_outside_the_dropdown_or_a_modal_closes_it_and_touches_nothing_under_it() {
    for open_it in [App::open_cauldrons as fn(&mut App), App::open_palette] {
        let mut a = app();
        open_it(&mut a);
        let is_open = |a: &App| a.modal.is_some() || a.dropdown.is_some();
        assert!(is_open(&a));
        // A tab, a tree row and the dump are all outside it.
        for target in [Hit::CTab(CTab::Status), Hit::TreeRow(id(&a, "X-9")), Hit::Input] {
            if !is_open(&a) {
                open_it(&mut a);
            }
            render_to_text(&mut a, 120, 60).unwrap();
            let (r, _) = *a.hits.iter().find(|(_, h)| *h == target).unwrap_or_else(|| panic!("{target:?}"));
            click(&mut a, r.x, r.y);
            assert!(!is_open(&a), "{target:?} closed nothing");
            assert_eq!(a.ctab, CTab::Cauldron, "the click on {target:?} went through");
            assert_eq!(a.selected, id(&a, "X-1"), "the click on {target:?} went through");
            assert_ne!(a.focus, Focus::Input, "the click on {target:?} went through");
        }
        // A click INSIDE it keeps it open.
        open_it(&mut a);
        render_to_text(&mut a, 120, 60).unwrap();
        let (r, _) = *a.hits.iter().rev().find(|(_, h)| *h == Hit::Modal).unwrap();
        click(&mut a, r.x + 1, r.y);
        assert!(is_open(&a));
    }
}

#[test]
fn plus_in_the_dropdown_names_a_cauldron_creates_it_and_lands_on_its_config() {
    let root = scratch_root();
    let mut a = open(&root, "prod");
    write(&mut a, "prod's draft");
    a.handle(key(KeyCode::Up));
    a.handle(ctrl('e'));
    // `+` is the entry after the last.
    a.handle(key(KeyCode::Down));
    let d = a.dropdown.clone().expect("the dropdown");
    assert_eq!(d.selected, d.entries.len());
    a.handle(key(KeyCode::Enter));
    assert!(a.dropdown.is_none());
    assert!(matches!(a.modal, Some(Modal::NewCauldron { .. })), "{:?}", a.modal);
    let s = screen(&mut a);
    assert!(s.contains("┏ Name the new cauldron ━"), "{s}");
    // Typing goes to the name, not the draft.
    typed(&mut a, "Bad Name");
    a.handle(key(KeyCode::Enter));
    let Some(Modal::NewCauldron { error, .. }) = &a.modal else { panic!("{:?}", a.modal) };
    assert!(error.as_deref().unwrap_or("").contains("REFUSED"), "{error:?}");
    assert_eq!(a.env.name, "prod");
    assert!(!root.join("data/Bad Name").exists());
    for _ in 0.."Bad Name".len() {
        a.handle(key(KeyCode::Backspace));
    }
    typed(&mut a, "trial");
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert_eq!(a.env.name, "trial");
    assert_eq!(a.ctab, CTab::Config, "a new cauldron opens on its Config");
    assert!(root.join("data/trial").is_dir(), "created on disk by the explicit new");
    assert!(a.composer.is_blank(), "its own draft");
    let s = screen(&mut a);
    let top = s.lines().next().unwrap();
    assert!(top.contains(" trial "), "{top}");
    assert!(s.contains("created trial"), "{s}");

    // It is now in the list; an existing name is refused.
    a.handle(ctrl('e'));
    let d = a.dropdown.clone().expect("the dropdown");
    let names: Vec<&str> = d.entries.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names, ["prod", "trial"]);
    let n = d.entries.len();
    // A click on an entry acts at once: a menu, not a list (task 0209 D18).
    click_on(&mut a, Hit::Drop(n));
    assert!(a.dropdown.is_none(), "{:?}", a.dropdown);
    assert!(matches!(a.modal, Some(Modal::NewCauldron { .. })));
    typed(&mut a, "prod");
    a.handle(key(KeyCode::Enter));
    let Some(Modal::NewCauldron { error, .. }) = &a.modal else { panic!() };
    assert!(error.as_deref().unwrap_or("").contains("already exists"), "{error:?}");
    // Esc goes back to the list; a click on prod switches to it.
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none() && a.dropdown.is_some(), "{:?} {:?}", a.modal, a.dropdown);
    click_on(&mut a, Hit::Drop(0));
    assert!(a.dropdown.is_none());
    assert_eq!(a.env.name, "prod");
    assert_eq!(a.composer.text(), "prod's draft");
}

#[test]
fn switching_to_an_existing_cauldron_keeps_each_ones_own_soup_and_draft() {
    let root = scratch_root();
    std::fs::create_dir_all(root.join("data/dev")).unwrap();
    let mut a = open(&root, "prod");
    write(&mut a, "prod's draft");
    a.handle(key(KeyCode::Up));
    a.handle(ctrl('e'));
    a.handle(key(KeyCode::Up));
    a.handle(key(KeyCode::Enter));
    assert_eq!(a.env.name, "dev");
    assert!(a.forest.find("X-1").is_some(), "dev has its own soup");
    assert!(a.composer.is_blank(), "dev has its own draft");
    assert_clean(&screen(&mut a), "dev");
}

// ── keys ────────────────────────────────────────────────────────────────────

#[test]
fn shift_tab_steps_cauldron_tabs_f_keys_jump_and_cmd_is_unbound() {
    let mut a = app();
    // ⌘ is no longer bound: nothing moves, nothing is typed.
    for c in ['2', '4', '.', ','] {
        a.handle(cmd(c));
        assert_eq!(a.ctab, CTab::Cauldron, "⌘{c} moved a tab");
    }
    a.handle(key(KeyCode::F(4)));
    assert_eq!(a.ctab, CTab::Config);
    a.handle(key(KeyCode::F(5))); // no fifth tab: nothing
    assert_eq!(a.ctab, CTab::Config);
    a.handle(key(KeyCode::F(9)));
    assert_eq!(a.ctab, CTab::Config);
    a.handle(key(KeyCode::F(1)));
    assert_eq!(a.ctab, CTab::Cauldron);
    // Shift-Tab: legacy CSI Z, the kitty form (BackTab + SHIFT), and Tab +
    // SHIFT as some stacks report it — each steps to the next, wrapping.
    // msg 235: Surface · Threads · Status · Config.
    a.handle(key(KeyCode::BackTab));
    assert_eq!(a.ctab, CTab::Threads);
    a.handle(with(KeyCode::BackTab, KeyModifiers::SHIFT));
    assert_eq!(a.ctab, CTab::Status);
    a.handle(with(KeyCode::Tab, KeyModifiers::SHIFT));
    assert_eq!(a.ctab, CTab::Config);
    a.handle(key(KeyCode::BackTab));
    assert_eq!(a.ctab, CTab::Cauldron, "wraps through four");
    // From inside the bucket too, and nothing is typed.
    a.handle(key(KeyCode::Char('a')));
    a.handle(key(KeyCode::BackTab));
    assert_eq!(a.ctab, CTab::Threads);
    a.handle(cmd('3'));
    assert_eq!(a.ctab, CTab::Threads);
    assert!(a.composer.is_blank(), "{}", a.composer.text());
    // Clicking a tab works as before.
    click_on(&mut a, Hit::CTab(CTab::Status));
    assert_eq!(a.ctab, CTab::Status);
}

#[test]
fn opt_keys_pick_cluster_tabs_in_every_encoding_and_tab_still_cycles() {
    let mut a = app();
    // The six tabs (msg 235 took Config; 8e4fca10 msg 385 flattened Human
    // into Questions and Actions and moved Overview to the end as Details),
    // in order: Questions first, and the face opens on it.
    assert_eq!(a.ktab, KTab::Questions, "the face opens on the first tab");
    // ALT (kitty, or Option-as-Meta).
    a.handle(opt('5'));
    assert_eq!(a.ktab, KTab::Status);
    a.handle(opt(','));
    assert_eq!(a.ktab, KTab::Nodes);
    a.handle(opt('.'));
    assert_eq!(a.ktab, KTab::Status);
    a.handle(opt('.'));
    assert_eq!(a.ktab, KTab::Details, "Details is the sixth, the last");
    a.handle(opt('.'));
    assert_eq!(a.ktab, KTab::Questions, "wraps after Details");
    a.handle(opt('3'));
    assert_eq!(a.ktab, KTab::Tasks);
    a.handle(opt('4'));
    a.handle(opt('7'));
    a.handle(opt('9'));
    assert_eq!(a.ktab, KTab::Nodes, "no seventh or ninth tab: nothing");
    a.handle(key(KeyCode::Char('ª'))); // Option-9
    a.handle(key(KeyCode::Char('¶'))); // Option-7
    assert_eq!(a.ktab, KTab::Nodes);
    // The characters macOS types for Option-digit when Option is not Meta.
    a.handle(key(KeyCode::Char('£')));
    assert_eq!(a.ktab, KTab::Tasks);
    a.handle(key(KeyCode::Char('¢')));
    assert_eq!(a.ktab, KTab::Nodes);
    a.handle(key(KeyCode::Char('∞'))); // Option-5
    assert_eq!(a.ktab, KTab::Status);
    a.handle(key(KeyCode::Char('§'))); // Option-6
    assert_eq!(a.ktab, KTab::Details);
    a.handle(key(KeyCode::Char('≤')));
    assert_eq!(a.ktab, KTab::Status);
    a.handle(key(KeyCode::Char('≥')));
    assert_eq!(a.ktab, KTab::Details);
    a.handle(key(KeyCode::Char('™')));
    assert_eq!(a.ktab, KTab::Actions);
    a.handle(key(KeyCode::Char('¡')));
    assert_eq!(a.ktab, KTab::Questions);
    // Tab cycles the cluster tabs.
    a.handle(key(KeyCode::Tab));
    assert_eq!(a.ktab, KTab::Actions);
    // From another cauldron tab, a cluster-tab key goes to Nodes.
    a.handle(key(KeyCode::F(4)));
    a.handle(opt('4'));
    assert_eq!((a.ctab, a.ktab), (CTab::Cauldron, KTab::Nodes));
    assert!(a.composer.is_blank());
}

#[test]
fn the_palette_lists_every_binding_and_opens_on_question_mark_or_ctrl_slash() {
    let mut a = app();
    a.handle(key(KeyCode::Char('?')));
    assert_eq!(a.modal, Some(Modal::Palette { scroll: 0 }));
    let s = render_to_text(&mut a, 160, 80).unwrap().join("\n");
    assert!(s.contains("┏ Keys ━"), "{s}");
    for (k, what) in KEYMAP {
        assert!(s.contains(k) && s.contains(what), "palette lacks {k} {what}:\n{s}");
    }
    // Keys go to the palette while it is open: Esc closes it, nothing else moves.
    a.handle(key(KeyCode::F(2)));
    assert_eq!(a.ctab, CTab::Cauldron);
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none());
    assert!(!a.quit);
    // ^/ arrives as Ctrl+'/' or as Ctrl+'7' (0x1f), depending on the stack.
    for c in ['/', '7'] {
        a.handle(ctrl(c));
        assert!(matches!(a.modal, Some(Modal::Palette { .. })), "^{c}");
        a.handle(ctrl(c));
        assert!(a.modal.is_none(), "^{c} closes it");
    }
    // ^P is no longer the palette: it is ↑, as in the harness.
    a.handle(ctrl('p'));
    assert!(a.modal.is_none());
    // In the dump, `?` is text and ^/ still opens it.
    a.handle(key(KeyCode::Char('a')));
    typed(&mut a, "why?");
    assert_eq!(a.composer.text(), "why?");
    assert!(a.modal.is_none());
    a.handle(ctrl('/'));
    assert!(a.modal.is_some());
}

// ── the cauldron tabs ───────────────────────────────────────────────────────

/// The Status row whose band key is `key`.
fn status_row(a: &App, key: &str) -> usize {
    tui::cauldron::status::view(a, 120).items.iter().position(|it| it.key == key).unwrap_or_else(|| panic!("no Status row `{key}`"))
}

#[test]
fn status_and_config_render_and_their_rows_quote_into_the_dump_without_leaving() {
    let mut a = app();
    a.handle(key(KeyCode::F(3)));
    assert_eq!(a.ctab, CTab::Status);
    // msg 235: the Mesh — each device a band, its services nested in it
    // (the second sub-tab since b964d571 msg 23).
    a.set_sttab(tui::cauldron::status::StatusTab::Mesh);
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("▾ example-mac")), "{s}");
    let tally = s.lines().find(|l| l.contains("▸ tally")).expect(&s);
    assert!(tally.contains("failed") && tally.trim_end_matches('│').trim_end().ends_with("⚠ 3 warnings · ✕ 2 errors"), "{tally}");
    let v = tui::cauldron::status::view(&a, 120);
    let svc = v.items.iter().find(|i| i.key == "svc:example-mac/tally").unwrap();
    assert_eq!(svc.parent.as_deref(), Some("dev:example-mac"), "a service sits in its device");
    // ▶ running and ◆ parked (one aggregate) and ◇ pending: the Bus's executions.
    let (run, park, pend) = tui::cauldron::status::counts(&a);
    a.set_sttab(tui::cauldron::status::StatusTab::Bus);
    let s = screen(&mut a);
    let totals = s.lines().find(|l| l.contains("▾ Executions")).expect(&s);
    assert!(totals.contains(&format!("▶ {run} running · ◆ {park} parked · ◇ {pend} pending")), "{totals}");
    // The token sources, each a band; an account's state word says why
    // nothing runs on it.
    a.set_sttab(tui::cauldron::status::StatusTab::Tokens);
    let s = screen(&mut a);
    let acct2 = s.lines().find(|l| l.contains("• acct2")).expect(&s);
    assert!(acct2.contains("AT LIMIT · back 1h 05m"), "{acct2}");
    assert!(s.contains("▸ Codex") && s.contains("▾ Inference endpoints") && s.contains("▾ Claude Code"), "{s}");
    assert!(!s.contains("THREADS") && !s.contains("8e4fca10  Cauldron interface"), "no threads table: {s}");
    // No bucket on this tab.
    assert!(!s.contains(" bucket "), "{s}");
    // A row is selected by a click (a band's toggles, I1); `i` quotes it
    // into the bucket and he stays.
    a.set_sttab(tui::cauldron::status::StatusTab::Mesh);
    let dev_row = status_row(&a, "dev:example-mac");
    click_on(&mut a, Hit::Row(dev_row));
    assert!(a.composer.is_blank() && a.ctab == CTab::Status);
    assert!(!tui::cauldron::status::is_open(&a, "dev:example-mac"), "one click closes the band");
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [device:example-mac]");
    assert_eq!((a.ctab, a.focus), (CTab::Status, Focus::Content), "the quote does not leave the tab");
    assert_eq!(a.notice.as_deref(), Some("quoted [device:example-mac] into the bucket"));
    a.handle(key(KeyCode::Right));
    assert!(tui::cauldron::status::is_open(&a, "dev:example-mac"));
    a.rows[0] = status_row(&a, "svc:example-mac/tts");
    a.handle(key(KeyCode::Char('i')));
    assert!(a.composer.text().ends_with("\n\n> [service:tts]"), "{}", a.composer.text());
    // A service's band holds the way to copy its log's link, not its log.
    a.handle(key(KeyCode::Right));
    a.handle(key(KeyCode::Right));
    let (rows, sel) = tui::cauldron::render_with_selection(&mut a, 120, 70).unwrap();
    assert!(sel.iter().any(|&y| rows[y].contains("copy log link")), "{rows:#?}");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(a.outbox.last(), Some(tui::cauldron::app::Outbox::Copy(_))), "{:?}", a.outbox);
    // A token source quotes as its setting; an account too.
    a.set_sttab(tui::cauldron::status::StatusTab::Tokens);
    a.rows[0] = status_row(&a, "tok:claude_code");
    a.handle(key(KeyCode::Char('i')));
    assert!(a.composer.text().ends_with("\n\n> [setting:pools.claude_code]"), "{}", a.composer.text());
    a.handle(key(KeyCode::Right));
    a.rows[0] = status_row(&a, "acct:claude_code/acct2");
    a.handle(key(KeyCode::Char('i')));
    assert!(a.composer.text().ends_with("\n\n> [setting:pools.claude_code.accounts.acct2]"), "{}", a.composer.text());
    // `config ▸` inside an account (its card is always open, msg 235) opens
    // that account's row's modal on Config › Providers.
    a.rows[0] = status_row(&a, "cfg:claude_code/acct2");
    a.handle(key(KeyCode::Enter));
    use tui::cauldron::menu::{Entry, SPage};
    assert_eq!(a.ctab, CTab::Config);
    let acct2 = SPage::Account("claude_code".into(), "acct2".into());
    assert_eq!(a.settings_entry(), Some(Entry::Row(acct2.clone())));
    assert!(matches!(&a.modal, Some(Modal::Settings { page, .. }) if *page == acct2), "{:?}", a.modal);
    // ← closes the modal; ← again climbs to Claude Code.
    a.handle(key(KeyCode::Left));
    assert!(a.modal.is_none());
    a.handle(key(KeyCode::Left));
    assert!(matches!(a.settings_entry(), Some(Entry::Sub { page: SPage::Pool(p), .. }) if p == "claude_code"), "{:?}", a.settings_entry());

    // Config: five tabs (msg 235), THIS cauldron's last and named for it;
    // what no cluster claims is on Status › Soup, not here.
    a.set_ctab(CTab::Cauldron);
    a.set_ctab(CTab::Config);
    a.set_cfg_tab(tui::cauldron::menu::CfgTab::Cauldron);
    let s = screen(&mut a);
    assert!(s.contains(" prod cauldron | TUI | Devices | Providers | Permissions ") && s.contains("▸ Directories") && s.contains("▸ Accounts") && !s.contains("▸ TUI"), "{s}");
    assert!(!s.contains("Unclaimed") && !s.contains("UNCLAIMED") && !s.contains("Mesh devices"), "{s}");
    a.set_ctab(CTab::Status);
    a.set_sttab(tui::cauldron::status::StatusTab::Soup);
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("▾ Unclaimed regions") && l.contains("1 region")), "{s}");
    assert!(s.lines().any(|l| l.contains("▸ R-9") && l.contains("Mesh devices")), "{s}");
    assert!(s.lines().any(|l| l.contains("▾ Unclaimed repos")), "{s}");
    assert!(s.lines().any(|l| l.contains("▸ orchard") && l.contains("/Users/demo/code/orchard")), "{s}");
    let row = status_row(&a, "repo:orchard");
    click_on(&mut a, Hit::Row(row));
    a.handle(key(KeyCode::Char('i')));
    assert!(a.composer.text().ends_with("\n\n> [repo:orchard]"), "{}", a.composer.text());
    let row = status_row(&a, "reg:R-9");
    click_on(&mut a, Hit::Row(row));
    a.handle(key(KeyCode::Char('i')));
    assert!(a.composer.text().ends_with("\n\n> [region:R-9]"), "{}", a.composer.text());
    assert_eq!(a.ctab, CTab::Status);
}

/// msg 165: the dump is on Work alone. ↓ at the last row of Status, System
/// and Config stays there, and `a` goes to Work's dump.
#[test]
fn down_at_the_last_row_of_a_cauldron_tab_stays_and_a_goes_to_works_dump() {
    for tab in [CTab::Status, CTab::Config] {
        let mut a = app();
        a.set_ctab(tab);
        a.handle(key(KeyCode::End));
        let last = a.row();
        assert_eq!(last, a.dash_rows().len() - 1, "{tab:?}");
        a.handle(key(KeyCode::Down));
        assert_eq!((a.row(), a.focus, a.ctab), (last, Focus::Content, tab), "{tab:?}: ↓ left the rows");
        a.handle(key(KeyCode::Char('a')));
        assert_eq!((a.ctab, a.focus), (CTab::Cauldron, Focus::Input), "{tab:?}");
    }
}

#[test]
fn every_row_fits_the_width_at_several_sizes_on_every_tab() {
    let pages = CTab::ALL.into_iter().map(|t| (t, KTab::Details)).chain(KTab::ALL.into_iter().map(|s| (CTab::Cauldron, s)));
    for (tab, ktab) in pages {
        for (w, h) in [(60, 24), (80, 24), (110, 48), (200, 60)] {
            for modal in [None, Some(0), Some(1), Some(2), Some(3), Some(4)] {
                let mut a = app();
                a.type_text("a fairly long dump [Q-1] that has to wrap inside the input box when narrow");
                a.handle(ctrl('g'));
                a.set_ctab(tab);
                a.set_ktab(ktab);
                match modal {
                    Some(0) => a.open_cauldrons(),
                    Some(1) => a.open_palette(),
                    Some(2) => a.modal = Some(Modal::NewCauldron { name: "x".into(), error: Some("a long error that has to wrap inside the modal box".into()) }),
                    Some(3) => a.modal = Some(Modal::NewCluster { parent: None, name: "a cluster".into(), boundaries: "what belongs in it, over several words so it wraps\nand a second line".into(), at: 1, caret: usize::MAX, error: Some("a long error that has to wrap inside the modal box".into()) }),
                    Some(4) => a.modal = Some(Modal::Help { title: "weekly threshold".into(), text: "an explanation long enough that it has to wrap inside the help modal, at every width this test draws".into() }),
                    _ => {}
                }
                let rows = render_to_text(&mut a, w, h).unwrap();
                assert_eq!(rows.len(), h as usize);
                for r in rows {
                    assert!(r.chars().count() <= w as usize, "{tab:?} {ktab:?} row wider than {w}: {r}");
                }
            }
        }
    }
}

// ── the model ───────────────────────────────────────────────────────────────

#[test]
fn status_is_computed_from_children() {
    let f = Forest::load(&tui::cauldron::store::seed_path(&seed_dir(), "prod").unwrap()).unwrap();
    let broom = f.find("X-6").unwrap();
    let r = f.rollup(broom);
    assert_eq!(r.open_questions, 3);
    assert_eq!(r.blocking_questions, 1);
    assert_eq!(r.headline(), Headline::Blocked);
    let tui = f.find("X-10").unwrap();
    assert_eq!(f.rollup(tui).headline(), Headline::Active, "T-3 runs");
    let landing = f.find("X-7").unwrap();
    assert_eq!(f.rollup(landing).headline(), Headline::Active);
}

#[test]
fn bad_data_is_refused_loudly() {
    let refuse = |json: &str, why: &str| {
        let f: DataFile = serde_json::from_str(json).unwrap();
        let e = Forest::from_specs(f.clusters).unwrap_err().to_string();
        assert!(e.contains(why), "{e}");
    };
    refuse(
        r#"{"clusters":[{"id":"X-1","name":"A","description":"d","questions":[
        {"serial":1,"value":1,"question":"q","why":"w","options":[],"recommendation":"r"},
        {"serial":1,"value":2,"question":"q","why":"w","options":[],"recommendation":"r"}]}]}"#,
        "reuses question serial 1",
    );
    refuse(
        r#"{"clusters":[{"id":"X-1","name":"A","description":"d"},{"id":"X-1","name":"B","description":"d"}]}"#,
        "duplicate cluster id",
    );
    refuse(
        r#"{"clusters":[{"id":"X-1","name":"A","description":"d","tasks":[
        {"serial":1,"title":"t","state":"ready","depends_on":[2]},
        {"serial":2,"title":"t","state":"ready","depends_on":[1]}]}]}"#,
        "form a cycle",
    );
    // REL-3.S2d: a dependency on an absent task is skipped and counted.
    let f: DataFile = serde_json::from_str(
        r#"{"clusters":[{"id":"X-1","name":"A","description":"d","tasks":[
        {"serial":1,"title":"t","state":"ready","depends_on":[9]}]}]}"#,
    )
    .unwrap();
    let f = Forest::from_specs(f.clusters).expect("REL-3.S2d: skipped, not refused");
    assert!(f.stale.len() == 1 && f.stale[0].contains("depends on T-9"), "{:?}", f.stale);
    assert!(f.clusters[0].tasks[0].depends_on.is_empty(), "skipped in the view");
    refuse(r#"{"clusters":[{"id":"A b","name":"A","description":"d"}]}"#, "is not a minted `X-<n>`");
    refuse(r#"{"clusters":[{"id":"cauldron","name":"A","description":"d"}]}"#, "is not a minted `X-<n>`");
}

// ── Soup: a cluster's memory, and the explorer into it ─────────────────────

fn region(a: &App, id: &str) -> usize {
    a.corpus.find_region(id).unwrap()
}

/// Select the cluster `key` and step into its Soup tab, as he would.
fn soup_of(a: &mut App, key: &str) {
    let g = id(a, key);
    a.focus = Focus::Tree;
    a.select(g);
    while a.ktab != KTab::Nodes {
        a.handle(self::key(KeyCode::Tab));
    }
    if a.is_expanded(g) || a.forest.get(g).children.is_empty() {
        a.handle(self::key(KeyCode::Right));
    } else {
        a.handle(self::key(KeyCode::Right));
        a.handle(self::key(KeyCode::Right));
    }
    assert_eq!(a.focus, Focus::Content);
}

/// The row of `s` that holds `needle`.
fn row_with<'a>(s: &'a str, needle: &str) -> &'a str {
    s.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("no row holds `{needle}`:\n{s}"))
}

#[test]
fn a_clusters_soup_tab_lists_what_it_claims_as_bands_with_count_and_schema_and_no_row_grows() {
    let mut a = app();
    soup_of(&mut a, "X-11");
    let s = screen(&mut a);
    assert!(s.contains("| Nodes |"), "the tab carries no number: {s}");
    // Soup and Artifacts are one tab (msg 186): REGIONS · ARTIFACTS · REPOS
    // · DIRECTORIES, and no FILE SYSTEM (it is config).
    for band in ["Regions", "Artifacts", "Repos", "Directories"] {
        assert!(s.lines().any(|l| l.contains(&format!("▾ {band}")) || l.contains(&format!("▸ {band}"))), "{band}: {s}");
    }
    assert!(!s.contains("FILE SYSTEM") && !s.contains("CREATED"), "{s}");
    // A region is a band: `R-31  <handle>`, its rollup and its schema on the right.
    let r31 = row_with(&s, "R-31   The v5 regions protocol");
    assert!(r31.trim_end_matches('│').trim_end().ends_with("v5"), "{r31}");
    assert!(r31.contains("2 nodes"), "{r31}");
    // The region made under an older schema says so with its version alone.
    let sub4 = row_with(&s, "SUB-4  Subjects");
    assert!(sub4.trim_end_matches('│').trim_end().ends_with("v3"), "{sub4}");
    // No row grows when selected (msg 165): the boundary is not on the list,
    // selected or not; the page is the same frame wherever the bar is.
    assert!(!s.contains("Inside: the protocol as written in core:workspaces-v5"), "{s}");
    let rows = s.lines().count();
    a.handle(key(KeyCode::Down));
    let t = screen(&mut a);
    assert_eq!(t.lines().count(), rows);
    assert_eq!(s.replace('▌', " "), t.replace('▌', " "), "only the bar moved");
    assert_eq!(a.items()[..3], [Item::Art(tui::cauldron::artpage::hash("regions")), Item::Region(region(&a, "R-31")), Item::Region(region(&a, "SUB-4"))]);
    assert_eq!(a.corpus.schema_version, 5);
    // Opened, the region shows its whole boundary, then its nodes.
    a.handle(key(KeyCode::Right));
    let s = screen(&mut a);
    assert!(s.contains("Inside: the protocol as written in core:workspaces-v5"), "{s}");
    assert!(row_with(&s, "R-31   The v5").contains('▾'), "{s}");
}

#[test]
fn a_region_opens_to_its_nodes_and_a_node_its_body_in_place_and_left_walks_back() {
    let mut a = app();
    soup_of(&mut a, "X-4");
    let s = screen(&mut a);
    assert!(s.contains("R-4    Dreaming"), "{s}");
    assert!(!s.contains("↑ R-3"), "no row grows when selected: {s}");
    // → on REGIONS (open) steps to R-4; → opens it in place: its boundary,
    // what it narrows, then its nodes.
    a.handle(key(KeyCode::Right));
    assert_eq!(a.items()[a.item], Item::Region(region(&a, "R-4")));
    a.handle(key(KeyCode::Right));
    let s = screen(&mut a);
    assert!(s.contains("↑ R-3"), "{s}");
    // A soup node is a card (task 0166 V1): its id the legend, its kind and
    // state at the top left, its handle inside.
    assert!(card_body(&s, "DEC-23").contains("The dream ripple may re-cut"), "{s}");
    assert!(row_with(&s, "C-24").contains("exploring"), "{s}");
    assert!(!s.contains("Deletion is his. A re-cut re-quotes every member"), "a node is one row: {s}");
    // ↓ to DEC-23, Enter opens its body in place: its trace, then its text.
    let dec = Item::Node(a.corpus.find_node("DEC-23").unwrap());
    while a.items()[a.item] != dec {
        a.handle(key(KeyCode::Down));
    }
    a.handle(key(KeyCode::Enter));
    let s = screen(&mut a);
    assert!(s.contains("↑ R-4 R-5"), "{s}");
    assert!(s.contains("Deletion is his. A re-cut re-quotes every member"), "{s}");
    // ← closes the body, ← again climbs to the region, ← closes it, ← climbs
    // to REGIONS, ← closes that, and ← at the top goes to the sidebar.
    a.handle(key(KeyCode::Left));
    assert!(!screen(&mut a).contains("Deletion is his."));
    assert_eq!(a.items()[a.item], dec);
    a.handle(key(KeyCode::Left));
    assert_eq!(a.items()[a.item], Item::Region(region(&a, "R-4")));
    a.handle(key(KeyCode::Left));
    assert!(!screen(&mut a).contains("DEC-23"));
    a.handle(key(KeyCode::Left));
    assert_eq!(a.item, 0);
    a.handle(key(KeyCode::Left));
    assert!(!screen(&mut a).contains("R-4    Dreaming"), "REGIONS closed");
    assert_eq!(a.focus, Focus::Content);
    a.handle(key(KeyCode::Left));
    assert_eq!(a.focus, Focus::Tree);
}

#[test]
fn an_artifact_is_a_soup_node_wrapping_a_file_and_sits_in_its_regions() {
    let mut a = app();
    soup_of(&mut a, "X-7");
    // R-20 holds the landing page's artifacts beside its nodes (his Q11).
    a.handle(key(KeyCode::Right));
    a.handle(key(KeyCode::Right));
    let s = screen(&mut a);
    assert!(s.contains("LANDING-PAGE-VISION"), "{s}");
    let art = Item::Art(tui::cauldron::artpage::hash("art:ART-201"));
    while a.items()[a.item] != art {
        a.handle(key(KeyCode::Down));
    }
    a.handle(key(KeyCode::Right));
    let page: String = tui::cauldron::pages::page(&a, 110).lines.iter().map(|l| l.to_string() + "\n").collect();
    assert!(page.contains("node     │ soup node ART-201 · wraps a file") && page.contains("regions  │ R-20"), "{page}");
    assert_eq!(a.selected_tag().map(|t| t.render()), Some("[ART-201]".into()));
    assert_eq!(tui::cauldron::designs::ARTIFACT_HOME, "soup");
}

#[test]
fn one_click_opens_a_region_and_a_second_click_opens_a_node() {
    let mut a = app();
    soup_of(&mut a, "X-4");
    a.focus = Focus::Tree;
    // msg 235 (I1): a region is a band, and a band toggles on ONE click.
    let r4 = at(&a, Item::Region(region(&a, "R-4")));
    click_on(&mut a, Hit::Item(r4));
    assert!(screen(&mut a).contains("DEC-23"), "one click opens the band");
    assert!(a.composer.is_blank(), "a band's click inserts nothing");
    // A node in it is a band too: one click opens it, the next closes it.
    let dec = at(&a, Item::Node(a.corpus.find_node("DEC-23").unwrap()));
    a.last_click = None;
    click_on(&mut a, Hit::Item(dec));
    assert!(screen(&mut a).contains("Deletion is his."));
    a.last_click = None;
    click_on(&mut a, Hit::Item(dec));
    assert!(!screen(&mut a).contains("Deletion is his."));
    assert!(a.composer.is_blank(), "a band's click inserts nothing");
}

#[test]
fn a_region_claimed_by_two_sibling_clusters_shows_on_both_and_names_the_other() {
    let mut a = app();
    let r3 = region(&a, "R-3");
    let ripples = id(&a, "X-3");
    let curation = id(&a, "X-5");
    assert_eq!(a.forest.get(ripples).parent, a.forest.get(curation).parent, "siblings");
    assert_eq!(a.corpus.claimants[r3], vec![ripples, curation]);
    soup_of(&mut a, "X-3");
    let s = screen(&mut a);
    assert!(s.contains("R-3    Ripples"), "{s}");
    assert!(s.contains("also Curation"), "{s}");
    soup_of(&mut a, "X-5");
    let s = screen(&mut a);
    assert!(s.contains("also Ripples"), "{s}");
    soup_of(&mut a, "X-9");
    assert!(screen(&mut a).contains("also Platform"));
}

#[test]
fn the_aggregate_soup_page_rolls_up_every_sub_clusters_regions_once_with_origin() {
    let mut a = app();
    soup_of(&mut a, "X-1");
    let s = screen(&mut a);
    // Own only, and what is below as one footer, which is itself an item.
    assert!(s.contains("below") && !s.contains("below ·") && !s.contains("Interface R-1"), "a bare count (F10): {s}");
    assert_eq!(a.items().iter().filter(|i| matches!(i, Item::Region(_))).count(), 1, "its own region only");
    a.handle(ctrl('g'));
    let s = screen(&mut a);
    assert!(!s.contains("below ·"), "{s}");
    // Each region once, however many sub-clusters claim it.
    assert_eq!(s.matches("R-3    Ripples").count(), 1, "{s}");
    let ids: Vec<String> = a.cluster_regions().iter().map(|g| a.corpus.regions[g.region].id.clone()).collect();
    assert_eq!(ids, ["R-6", "R-1", "R-2", "R-3", "R-4", "R-5"]);
    let via: Vec<Vec<usize>> = a.cluster_regions().iter().map(|g| g.via.clone()).collect();
    assert_eq!(via[3], vec![id(&a, "X-3"), id(&a, "X-5")]);
    assert!(!s.contains("R-20") && !s.contains("R-31"), "{s}");
}

#[test]
fn an_unclaimed_region_is_reached_from_status_soup_and_from_no_cluster() {
    let mut a = app();
    assert_eq!(a.corpus.orphans(), vec![region(&a, "R-9")]);
    for root in ["X-1", "X-6", "X-9"] {
        let scope = a.forest.subtree(id(&a, root));
        assert!(a.corpus.cluster_regions(&scope).iter().all(|x| x.region != region(&a, "R-9")));
    }
    // Moved off Config (msg 186): Status › Soup.
    a.set_ctab(CTab::Config);
    assert!(!screen(&mut a).contains("Mesh devices"));
    a.set_ctab(CTab::Status);
    a.set_sttab(tui::cauldron::status::StatusTab::Soup);
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("R-9") && l.contains("Mesh devices")), "{s}");
}

#[test]
fn search_is_a_mode_on_the_clusters_soup_tab_and_stays_inside_the_cluster() {
    let mut a = app();
    a.select(id(&a, "X-3"));
    // ^F from anywhere: the cluster's Soup tab, with the field taking keys.
    a.handle(key(KeyCode::F(4)));
    a.handle(ctrl('f'));
    assert_eq!((a.ctab, a.ktab), (CTab::Cauldron, KTab::Nodes));
    assert!(a.searching);
    typed(&mut a, "budget");
    assert_eq!(a.search, "budget");
    assert!(a.composer.is_blank(), "search keys must not reach the draft");
    let s = screen(&mut a);
    assert!(s.contains("search budget▏"), "{s}");
    // Under the search a region the query reaches is OPEN on what it matched.
    assert!(card_body(&s, "IS-22").contains("A ripple that overruns its budget"), "{s}");
    // Enter keeps the query; → opens the hit's body in place.
    a.handle(key(KeyCode::Enter));
    assert!(!a.searching);
    let is22 = Item::Node(a.corpus.find_node("IS-22").unwrap());
    select_item(&mut a, is22);
    a.handle(key(KeyCode::Right));
    let s = screen(&mut a);
    assert!(s.contains("↑ "), "its trace: {s}");
    a.handle(key(KeyCode::Left));
    assert_eq!(a.items()[a.item], is22);
    // Another cluster's regions are outside the search: Harness has no budget.
    a.select(id(&a, "X-9"));
    let s = screen(&mut a);
    assert!(!s.contains("R-21   "), "a search with no hits lists no region: {s}");
    assert!(!a.items().iter().any(|i| matches!(i, Item::Region(_))));
    // Over its subtree with aggregate on.
    a.select(id(&a, "X-6"));
    a.set_search("SUCCESS PATH");
    assert!(!a.items().contains(&Item::Region(region(&a, "R-21"))), "own regions only: Broomstick claims none");
    a.handle(ctrl('g'));
    let s = screen(&mut a);
    // Its id and the start of its handle (the noun counts, msg 403, take
    // the rest of a narrow line).
    assert!(s.contains("R-21   Platform"), "{s}");
    assert!(s.contains("IS-42"), "{s}");
    // Esc, outside the field, hands the dump the selection; the query stands.
    a.focus = Focus::Content;
    a.handle(key(KeyCode::Esc));
    assert_eq!(a.focus, Focus::Input);
    assert_eq!(a.search, "SUCCESS PATH");
    // In the field: ^U clears, Backspace edits, Esc leaves KEEPING the query.
    a.handle(ctrl('f'));
    assert!(a.searching);
    a.handle(ctrl('u'));
    assert_eq!(a.search, "");
    typed(&mut a, "zz");
    a.handle(key(KeyCode::Backspace));
    assert_eq!(a.search, "z");
    a.handle(key(KeyCode::Esc));
    assert!(!a.searching);
    assert_eq!(a.search, "z");
    assert!(a.composer.is_blank(), "the field's keys never reach the draft");
}

#[test]
fn regions_and_nodes_quote_and_a_send_lands_on_their_claimants() {
    let root = scratch_root();
    let mut a = open(&root, "prod");
    soup_of(&mut a, "X-5");
    a.handle(key(KeyCode::Down)); // off the REGIONS band, onto R-3
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [region:R-3]");
    // Open R-5 and quote one of its nodes.
    a.handle(key(KeyCode::Up)); // out of the dump, back on R-3
    a.handle(key(KeyCode::Down)); // onto R-5
    a.handle(key(KeyCode::Right));
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [region:R-3]\n\n> [DEC-23]");
    // The orphan, from Status › Soup (msg 186: moved off Config).
    a.handle(key(KeyCode::F(3)));
    a.set_sttab(tui::cauldron::status::StatusTab::Soup);
    let orphan = status_row(&a, "reg:R-9");
    assert_eq!(a.dash_rows()[orphan], Row::Status(orphan));
    click_on(&mut a, Hit::Row(orphan));
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [region:R-3]\n\n> [DEC-23]\n\n> [region:R-9]");
    // `a` goes to Work's dump, the caret after the last quote.
    a.handle(key(KeyCode::Char('a')));
    assert_eq!((a.ctab, a.focus), (CTab::Cauldron, Focus::Input));
    typed(&mut a, " ok");
    a.handle(key(KeyCode::Enter));
    let send = a.sends.sends.last().expect("sent");
    assert_eq!(send.text, "> [region:R-3]\n\n> [DEC-23]\n\n> [region:R-9] ok");
    assert_eq!(send.tags, ["[region:R-3]", "[DEC-23]", "[region:R-9]"]);
    assert_eq!(send.targets, ["X-3", "X-5", "X-4"]);
}

#[test]
fn a_send_tagged_only_with_an_orphan_region_lands_on_no_cluster() {
    // msg 235: what no cluster claims goes into the soup, not to the cluster
    // he happens to be looking at.
    let mut a = open(&scratch_root(), "prod");
    a.dump("about this [region:R-9]");
    let send = a.sends.sends.last().unwrap();
    assert!(send.targets.is_empty(), "{:?}", send.targets);
}

#[test]
fn bad_corpus_data_is_refused_loudly_and_absent_regions_are_skipped() {
    let refuse = |corpus: &str, claims: &str, why: &str| {
        let json = format!(
            r#"{{"clusters":[{{"id":"X-1","name":"A","description":"d","regions":{claims}}}],"corpus":{corpus}}}"#
        );
        let f: DataFile = serde_json::from_str(&json).unwrap();
        let e = format!("{:#}", Soup::from_file(f).unwrap_err());
        assert!(e.contains(why), "{e}");
    };
    let ws = |regions: &str, nodes: &str| format!(r#"{{"schema_version":5,"regions":{regions},"nodes":{nodes}}}"#);
    let r = |id: &str, narrows: &str| format!(r#"{{"id":"{id}","handle":"h","boundary":"b","narrows":{narrows}}}"#);
    // REL-3.S2d: a region that is not in the soup is skipped where it is
    // named and counted, never refused.
    let skip = |corpus: &str, claims: &str, why: &str| {
        let json = format!(
            r#"{{"clusters":[{{"id":"X-1","name":"A","description":"d","regions":{claims}}}],"corpus":{corpus}}}"#
        );
        let f: DataFile = serde_json::from_str(&json).unwrap();
        let s = Soup::from_file(f).unwrap_or_else(|e| panic!("REL-3.S2d: skipped, not refused: {e:#}"));
        assert!(s.corpus.stale.len() == 1 && s.corpus.stale[0].contains(why), "{:?}", s.corpus.stale);
    };
    skip(&ws(&format!("[{}]", r("R-1", "[]")), "[]"), r#"["R-2"]"#, "cluster `X-1` names region `R-2`");
    refuse(&ws(&format!("[{},{}]", r("R-1", "[]"), r("R-1", "[]")), "[]"), "[]", "region id `R-1` is used twice");
    refuse(
        &ws(&format!("[{},{}]", r("R-1", r#"["R-2"]"#), r("R-2", r#"["R-1"]"#)), "[]"),
        "[]",
        "narrows DAG has a cycle",
    );
    refuse(
        &ws(&format!("[{}]", r("R-1", "[]")), r#"[{"id":"IS-1","type":"issue","handle":"h","in_region":[]}]"#),
        "[]",
        "attests no region",
    );
    skip(
        &ws(&format!("[{}]", r("R-1", "[]")), r#"[{"id":"IS-1","type":"issue","handle":"h","in_region":["R-7"]}]"#),
        "[]",
        "node `IS-1` names region `R-7`",
    );
    refuse(&ws(&format!("[{}]", r("R 1", "[]")), "[]"), "[]", "is not [A-Za-z0-9-_.]+");
    refuse(
        r#"{"schema_version":5,"regions":[{"id":"R-1","handle":"h","boundary":"b","schema_version":6}]}"#,
        "[]",
        "newer than its workspace's v5",
    );
    // The retired multi-workspace shape is refused, not half-read.
    let json = r#"{"clusters":[{"id":"X-1","name":"A","description":"d"}],"corpus":{"workspaces":[]}}"#;
    assert!(serde_json::from_str::<DataFile>(json).is_err());
    for env in ["prod", "dev"] {
        tui::cauldron::store::load_soup(&seed_dir(), env, &scratch_root()).unwrap();
    }
}

// ── iteration five: one navigation philosophy (docs/navigation.md) ─────────

fn opt_key(code: KeyCode) -> Event {
    with(code, KeyModifiers::ALT)
}

fn mouse(a: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
}

#[test]
fn in_the_dropdown_and_every_modal_the_arrows_navigate_only_it_left_escapes_and_right_is_enter() {
    let root = scratch_root();
    std::fs::create_dir_all(root.join("data/dev")).unwrap();
    let mut a = open(&root, "prod");
    let start = (a.selected, a.focus, a.ktab, a.ctab);

    // The cauldrons: ↑ ↓ move inside the dropdown and touch nothing under it.
    a.open_cauldrons();
    let d = a.dropdown.clone().expect("the dropdown");
    let names: Vec<&str> = d.entries.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names, ["dev", "prod"]);
    assert_eq!(d.selected, 1);
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::Down));
    assert_eq!(a.dropdown.as_ref().map(|d| d.selected), Some(2));
    a.handle(key(KeyCode::Up));
    a.handle(key(KeyCode::Up));
    a.handle(key(KeyCode::Up));
    assert_eq!(a.dropdown.as_ref().map(|d| d.selected), Some(0));
    assert_eq!((a.selected, a.focus, a.ktab, a.ctab), start, "an arrow reached the face under the dropdown");
    // ← escapes it, as Esc would.
    a.handle(key(KeyCode::Left));
    assert!(a.dropdown.is_none());
    assert_eq!((a.selected, a.focus, a.ktab, a.ctab), start, "← went through to the sidebar");

    // → acts as Enter: on `+` it opens the name modal…
    a.open_cauldrons();
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::Right));
    assert!(matches!(a.modal, Some(Modal::NewCauldron { .. })), "{:?}", a.modal);
    // …where ← goes back one level, to the list, as Esc does.
    typed(&mut a, "x");
    a.handle(key(KeyCode::Left));
    assert!(a.modal.is_none() && a.dropdown.is_some(), "{:?}", a.modal);
    // → on an entry switches to it.
    a.handle(key(KeyCode::Up));
    a.handle(key(KeyCode::Up));
    a.handle(key(KeyCode::Right));
    assert!(a.dropdown.is_none());
    assert_eq!(a.env.name, "dev");

    // The new-cluster modal: ← closes it, → goes on from the name to the
    // boundaries, and → there creates.
    a.open_new_cluster(None);
    a.handle(key(KeyCode::Left));
    assert!(a.modal.is_none());
    a.open_new_cluster(None);
    typed(&mut a, "Arrow made");
    a.handle(key(KeyCode::Right));
    assert!(matches!(a.modal, Some(Modal::NewCluster { at: 1, .. })), "{:?}", a.modal);
    typed(&mut a, "arrows");
    a.handle(key(KeyCode::Right));
    assert!(a.modal.is_none(), "{:?}", a.modal);
    assert!(a.forest.clusters.iter().any(|g| g.name == "Arrow made"));

    // The palette: ↑ ↓ scroll it, ← and → both close it.
    for close in [KeyCode::Left, KeyCode::Right] {
        a.open_palette();
        a.handle(key(KeyCode::Down));
        assert_eq!(a.modal, Some(Modal::Palette { scroll: 1 }));
        a.handle(key(close));
        assert!(a.modal.is_none(), "{close:?}");
    }

    // Help: ← and → both close it.
    for close in [KeyCode::Left, KeyCode::Right] {
        a.modal = Some(Modal::Help { title: "t".into(), text: "x".into() });
        a.handle(key(KeyCode::Down));
        assert!(matches!(a.modal, Some(Modal::Help { .. })));
        a.handle(key(close));
        assert!(a.modal.is_none(), "{close:?}");
    }
}

#[test]
fn esc_hands_the_dump_the_selection_on_work_and_never_quits() {
    let mut a = app();
    // From the sidebar.
    a.handle(key(KeyCode::Esc));
    assert_eq!(a.focus, Focus::Input);
    assert!(!a.quit);
    // In the dump it hands the selection back to where it came from (design
    // 0138 Q2): Esc is a two-way switch. So does ↑ on the top line.
    a.handle(key(KeyCode::Esc));
    assert_eq!(a.focus, Focus::Tree);
    a.handle(key(KeyCode::Esc));
    a.handle(key(KeyCode::Up));
    assert_eq!(a.focus, Focus::Tree);
    // From the page, even with a region opened (← closes that, not Esc).
    a.set_ktab(KTab::Nodes);
    a.handle(key(KeyCode::Right));
    a.handle(key(KeyCode::Right));
    let closed = a.items().len();
    a.handle(key(KeyCode::Right));
    let opened = a.items().len();
    assert!(opened > closed, "the region opened");
    a.handle(key(KeyCode::Esc));
    assert_eq!(a.focus, Focus::Input);
    assert_eq!(a.items().len(), opened, "Esc closed nothing");
    a.handle(key(KeyCode::Up));
    assert_eq!(a.focus, Focus::Content);
    a.handle(key(KeyCode::Left));
    assert_eq!(a.items().len(), closed);
    for _ in 0..4 {
        a.handle(key(KeyCode::Left));
    }
    assert_eq!(a.focus, Focus::Tree);
    // On Status there is no bucket: Esc does nothing; `a` goes to it.
    a.handle(key(KeyCode::F(3)));
    a.handle(key(KeyCode::Esc));
    assert_eq!((a.ctab, a.focus), (CTab::Status, Focus::Content));
    a.handle(key(KeyCode::Char('a')));
    assert_eq!((a.ctab, a.focus), (CTab::Cauldron, Focus::Input));
    assert!(!a.quit);
    // `q` quits outside the dump; inside it, it is a letter.
    typed(&mut a, "q");
    assert!(!a.quit);
    assert_eq!(a.composer.text(), "q");
    a.handle(key(KeyCode::Up));
    a.handle(key(KeyCode::Char('q')));
    assert!(a.quit);
    // ^Q is not a quit any more (in the harness it queues).
    let mut b = app();
    b.handle(ctrl('q'));
    assert!(!b.quit);
    b.handle(ctrl('c'));
    assert!(b.quit);
}

#[test]
fn the_arrows_cross_regions_only_at_their_edges() {
    let mut a = app();
    // The sidebar: ↑ above the first cluster is the header's `+`; ↓ walks
    // every visible cluster, and past the last, the dump.
    a.handle(key(KeyCode::Up));
    assert!(a.on_add_root);
    assert_eq!(a.selected, id(&a, "X-1"), "the page still shows the first cluster");
    a.handle(key(KeyCode::Up));
    assert!(a.on_add_root, "nothing above the header");
    a.handle(key(KeyCode::Down));
    assert!(!a.on_add_root);
    let n = a.visible().len();
    for _ in 0..n - 1 {
        a.handle(key(KeyCode::Down));
    }
    assert_eq!(a.selected, id(&a, "X-11"));
    assert_eq!(a.focus, Focus::Tree);
    // Past the last cluster, the switch at the sidebar's foot (b964d571
    // msg 23); past that, the dump; ↑ comes back to the switch, then the
    // last cluster.
    a.handle(key(KeyCode::Down));
    assert!(a.on_below && a.focus == Focus::Tree);
    a.handle(key(KeyCode::Down));
    assert_eq!(a.focus, Focus::Input);
    a.handle(key(KeyCode::Up));
    assert_eq!(a.focus, Focus::Tree);
    assert!(a.on_below);
    a.handle(key(KeyCode::Up));
    assert!(!a.on_below);
    assert_eq!(a.selected, id(&a, "X-11"));
    // Home / End: first cluster / last cluster.
    a.handle(key(KeyCode::Home));
    assert_eq!(a.selected, id(&a, "X-1"));
    a.handle(key(KeyCode::End));
    assert_eq!(a.selected, id(&a, "X-11"));
    a.handle(key(KeyCode::Home));

    // The page: ↓ past the last item goes to the dump; ↑ comes back to it.
    a.handle(key(KeyCode::Right));
    assert_eq!(a.focus, Focus::Content);
    let last = a.items().len() - 1;
    a.handle(key(KeyCode::End));
    assert_eq!(a.item, last);
    a.handle(key(KeyCode::Down));
    assert_eq!(a.focus, Focus::Input);
    a.handle(key(KeyCode::Up));
    assert_eq!((a.focus, a.item), (Focus::Content, last));
    a.handle(key(KeyCode::Home));
    assert_eq!(a.item, 0);
    // ^P / ^N are ↑ / ↓ outside the dump.
    a.handle(ctrl('n'));
    assert_eq!(a.item, 1);
    a.handle(ctrl('p'));
    assert_eq!(a.item, 0);

    // The dump: ↑ ↓ move by line, and only the top line's ↑ leaves.
    a.handle(key(KeyCode::Char('a')));
    typed(&mut a, "one");
    a.handle(opt_key(KeyCode::Enter));
    typed(&mut a, "two");
    a.handle(with(KeyCode::Enter, KeyModifiers::SHIFT));
    typed(&mut a, "three");
    a.handle(ctrl('j'));
    typed(&mut a, "four");
    assert_eq!(a.composer.text(), "one\ntwo\nthree\nfour");
    a.handle(key(KeyCode::Up));
    a.handle(key(KeyCode::Up));
    assert_eq!((a.focus, a.composer.cursor().line), (Focus::Input, 1));
    a.handle(key(KeyCode::Down));
    assert_eq!(a.composer.cursor().line, 2);
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::Down));
    assert_eq!((a.focus, a.composer.cursor().line), (Focus::Input, 3), "↓ on the last line stays");
    // ⌥↑ / ⌥↓ by paragraph, never leaving the box.
    for _ in 0..6 {
        a.handle(opt_key(KeyCode::Up));
    }
    assert_eq!((a.focus, a.composer.cursor().line, a.composer.cursor().byte), (Focus::Input, 0, 0));
    a.handle(opt_key(KeyCode::Down));
    assert_eq!((a.composer.cursor().line, a.composer.cursor().byte), (0, 3));
    // Home / End / ^A / ^E: the LINE; ⌥Home / ⌥End: the whole draft.
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::End));
    assert_eq!((a.composer.cursor().line, a.composer.cursor().byte), (1, 3));
    a.handle(ctrl('a'));
    assert_eq!((a.composer.cursor().line, a.composer.cursor().byte), (1, 0));
    a.handle(ctrl('e'));
    assert_eq!((a.composer.cursor().line, a.composer.cursor().byte), (1, 3));
    assert!(a.modal.is_none() && a.dropdown.is_none(), "^E in the dump is end-of-line, not the cauldrons");
    a.handle(opt_key(KeyCode::End));
    assert_eq!((a.composer.cursor().line, a.composer.cursor().byte), (3, 4));
    a.handle(opt_key(KeyCode::Home));
    assert_eq!((a.composer.cursor().line, a.composer.cursor().byte), (0, 0));
    // ⌥→ by word; ← at the very start of the draft leaves the dump.
    a.handle(opt_key(KeyCode::Right));
    assert_eq!(a.composer.cursor().byte, 3);
    a.handle(key(KeyCode::Left));
    a.handle(key(KeyCode::Left));
    a.handle(key(KeyCode::Left));
    assert_eq!(a.focus, Focus::Input, "← moves the caret first");
    a.handle(key(KeyCode::Left));
    assert_eq!(a.focus, Focus::Content);
    // Tab in the dump is spaces, not a cluster tab.
    a.handle(key(KeyCode::Char('a')));
    let ktab = a.ktab;
    a.handle(key(KeyCode::Tab));
    assert_eq!(a.ktab, ktab);
    assert!(a.composer.text().starts_with("    one"), "{:?}", a.composer.text());
    // Enter on the sidebar is →: it does not fold an open cluster.
    let mut b = app();
    b.handle(key(KeyCode::Enter));
    assert!(b.is_expanded(id(&b, "X-1")));
    assert_eq!(b.focus, Focus::Content);
}

#[test]
fn opt_up_and_down_jump_between_sections() {
    let mut a = app();
    // The sidebar's sections are its root clusters.
    a.handle(opt_key(KeyCode::Down));
    assert_eq!(a.selected, id(&a, "X-6"));
    a.handle(opt_key(KeyCode::Down));
    assert_eq!(a.selected, id(&a, "X-9"));
    a.handle(opt_key(KeyCode::Down));
    assert_eq!(a.selected, id(&a, "X-9"), "no root below the last");
    a.handle(opt_key(KeyCode::Up));
    assert_eq!(a.selected, id(&a, "X-6"));
    // The page's: each question's box, skipping the options inside it.
    a.select(id(&a, "X-1"));
    a.set_ktab(KTab::Questions);
    select_item(&mut a, Item::Question { cluster: 0, serial: 1 });
    let items = a.items();
    assert!(matches!(items[a.item + 1], Item::Option { opt: 1, .. }), "{items:?}");
    a.handle(opt_key(KeyCode::Down));
    assert!(matches!(items[a.item], Item::Question { serial: 2, .. }), "{:?}", items[a.item]);
    let second = a.item;
    a.handle(opt_key(KeyCode::Up));
    assert_eq!(a.items()[a.item], Item::Question { cluster: 0, serial: 1 });
    a.handle(opt_key(KeyCode::Down));
    assert_eq!(a.item, second);
    // On the Tasks tab's Engine (b964d571 msg 23): each directive's rule.
    a.set_ktab(KTab::Tasks);
    select_item(&mut a, Item::Band(Band::Engine));
    a.handle(opt_key(KeyCode::Down));
    let items = a.items();
    assert!(matches!(items[a.item], Item::Directive { ix: 0, .. }), "{:?}", items[a.item]);
    a.handle(opt_key(KeyCode::Down));
    assert!(matches!(a.items()[a.item], Item::Directive { ix: 1, .. }), "{:?}", a.items()[a.item]);
    a.handle(opt_key(KeyCode::Up));
    assert!(matches!(a.items()[a.item], Item::Directive { ix: 0, .. }));
    // A cauldron tab's: the top-level bands of the sub-tab shown (msg 235:
    // the Bus holds EVENTS, TRIGGERS, RIPPLES and EXECUTIONS).
    a.handle(key(KeyCode::F(3)));
    a.set_sttab(tui::cauldron::status::StatusTab::Bus);
    assert_eq!(a.row(), 0, "on the strip");
    a.handle(opt_key(KeyCode::Down));
    assert_eq!(a.row(), status_row(&a, "events"));
    a.handle(opt_key(KeyCode::Down));
    assert_eq!(a.row(), status_row(&a, "triggers"), "from EVENTS to TRIGGERS");
    a.handle(opt_key(KeyCode::Up));
    assert_eq!(a.row(), status_row(&a, "events"));
}

#[test]
fn plus_in_the_sidebar_header_names_a_root_cluster_that_persists_and_is_selected() {
    let root = scratch_root();
    let mut a = open(&root, "prod");
    let before = a.forest.clusters.len();
    // Keyboard: ↑ above the first cluster lands on the header's `+`; Enter
    // (or →) asks for a name.
    a.handle(key(KeyCode::Up));
    assert!(a.on_add_root);
    a.handle(key(KeyCode::Enter));
    assert!(matches!(a.modal, Some(Modal::NewCluster { parent: None, .. })), "{:?}", a.modal);
    // N8 (msg 235): "New cluster", and the second field "(boundary conditions)".
    let s = screen(&mut a);
    assert!(s.contains("┏ New cluster ━") && s.contains("(boundary conditions)"), "{s}");
    // Refused: no letter or digit; no boundaries; an id that exists.
    typed(&mut a, "!!");
    a.handle(key(KeyCode::Enter)); // on to the boundaries
    typed(&mut a, "tax paperwork");
    a.handle(key(KeyCode::Enter));
    let Some(Modal::NewCluster { error, .. }) = &a.modal else { panic!() };
    assert!(error.as_deref().unwrap_or("").contains("letter or a digit"), "{error:?}");
    a.handle(key(KeyCode::Up)); // back to the name
    a.handle(key(KeyCode::Backspace));
    a.handle(key(KeyCode::Backspace));
    typed(&mut a, "Harness");
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::Enter));
    let Some(Modal::NewCluster { error, .. }) = &a.modal else { panic!() };
    assert!(error.as_deref().unwrap_or("").contains("a cluster named `Harness` already exists here"), "{error:?}");
    assert_eq!(a.forest.clusters.len(), before);
    // Made: a ROOT cluster, after every other, selected, the draft untouched.
    a.handle(key(KeyCode::Up));
    for _ in 0.."Harness".len() {
        a.handle(key(KeyCode::Backspace));
    }
    typed(&mut a, "Tax Season 2027");
    a.handle(key(KeyCode::Down));
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none(), "{:?}", a.modal);
    // N7: a minted id, the next free X-<n>.
    let g = id(&a, &format!("X-{}", before + 1));
    assert_eq!(g, before, "appended after every existing cluster");
    assert_eq!(a.forest.get(g).parent, None);
    assert_eq!(a.forest.get(g).name, "Tax Season 2027");
    assert_eq!((a.selected, a.on_add_root, a.focus), (g, false, Focus::Tree));
    assert!(a.composer.is_blank());
    let s = screen(&mut a);
    assert!(s.find("· Workspaces").unwrap() < s.find("· Tax Season 2027").unwrap(), "{s}");
    // On disk, in this environment, and there after a restart.
    let file = soup_dir(&a.env).unwrap().join("clusters.json");
    assert!(std::fs::read_to_string(&file).unwrap().contains(&format!("\"X-{}\"", before + 1)));
    let b = open(&root, "prod");
    assert_eq!(b.forest.find(&format!("X-{}", before + 1)), Some(before));
    assert!(!open(&root, "dev").forest.clusters.iter().any(|g| g.name == "Tax Season 2027"), "per environment");
    // The mouse: one click on the header's `+` asks for a name.
    let mut c = open(&root, "prod");
    click_on(&mut c, Hit::AddRoot);
    assert!(matches!(c.modal, Some(Modal::NewCluster { parent: None, .. })));
    // A tag on the new cluster is valid: its minted id is the tag grammar's.
    assert!(tui::cauldron::tags::scan(&format!("[cluster:X-{}]", before + 1)).len() == 1);
    // The dump writes nothing.
    let mut d = App::open(env_in(&root, "prod"), seed_dir(), false).unwrap();
    d.open_new_cluster(None);
    typed(&mut d, "nope");
    d.handle(key(KeyCode::Enter));
    typed(&mut d, "nothing");
    d.handle(key(KeyCode::Enter));
    assert!(matches!(&d.modal, Some(Modal::NewCluster { error: Some(e), .. }) if e.contains("--dump")));
}

#[test]
fn the_status_tab_bands_the_ripples_running_parked_pending_then_background() {
    use tui::cauldron::status::cluster_counts;
    let mut a = app();
    a.handle(opt('5'));
    assert_eq!(a.ktab, KTab::Status);
    // Own only by default (F5): Cauldron's own ripples; what runs below is
    // one bare count, which → turns aggregate on.
    let (r, p, pe) = cluster_counts(&a, a.selected, false);
    let own = screen(&mut a);
    let ripples = own.lines().find(|l| l.contains("▾ Ripples")).expect(&own);
    assert!(ripples.contains(&format!("▶ {r} running · ◆ {p} parked · ◇ {pe} pending")), "{ripples}");
    for (band, n) in [("▾ Running", r), ("▸ Parked", p), ("▸ Pending", pe)] {
        assert!(has_band(&own, band, n), "{band} {n}:\n{own}");
    }
    assert!(own.contains("▸ Background ") && !own.contains("Interface/T-1"), "{own}");
    assert!(own.contains(" below"), "{own}");
    a.handle(ctrl('g'));
    all_bands_open(&mut a, true);
    let (r, p, pe) = cluster_counts(&a, a.selected, true);
    let s = screen(&mut a);
    assert!(has_band(&s, "▾ Running", r) && has_band(&s, "▾ Parked", p) && has_band(&s, "▾ Pending", pe), "{s}");
    let running = s.find("▾ Running").expect(&s);
    let parked = s.find("▾ Parked").expect(&s);
    let pending = s.find("▾ Pending").expect(&s);
    let background = s.find("▾ Background ").expect(&s);
    assert!(running < parked && parked < pending && pending < background, "{s}");
    // A running ripple is ONE row: what it is doing on it, its account and
    // time on the right.
    let t2 = s.lines().find(|l| l.contains("▶ T-2  First TUI — rendering the Ripples tab")).expect(&s);
    assert!(t2.contains("acct3 · 41m"), "{t2}");
    // A sub-cluster's ripple carries its cluster.
    assert!(s.contains("▶ Interface/T-9"), "{s}");
    // A parked ripple says why the gate refused it and what it could run on.
    let t8 = s.lines().find(|l| l.contains("◆ T-8")).expect(&s);
    assert!(t8.contains("weekly 80% on every account") && t8.contains("Claude Code · Codex"), "{t8}");
    // Work the graph holds is not parked (design 0142 Q1): T-5 is not here.
    assert!(!s.contains("■ T-5  Voice echo") || s.find("■ T-5  Voice echo").unwrap() > s.find("▾ Engine").unwrap(), "{s}");
    // The runs that are not tasks.
    assert!(s.contains("⟳ dump      8e4fca10#96 → 6 atoms"), "{s}");
    assert!(s.contains("✕ voice     tts refused: service stopped"), "{s}");
    let rows = a.items().iter().filter(|i| matches!(i, Item::Ripple { .. } | Item::Run(_))).count();
    assert_eq!(rows, r + p + pe, "the band header and its rows are one count");
    // A ripple is selectable and tagged as its task, and selecting it grows
    // nothing (msg 165).
    // (Under ^G the whole cauldron is the root, b964d571 msg 23: X-1 is no
    // longer cluster 0.)
    let x1 = id(&a, "X-1");
    select_item(&mut a, Item::Ripple { cluster: x1, serial: 2 });
    let sel = screen(&mut a);
    assert_eq!(sel.replace('▌', " ").lines().filter(|l| !l.trim().is_empty()).count(), s.replace('▌', " ").lines().filter(|l| !l.trim().is_empty()).count(), "a row grew:\n{sel}");
    assert!(!sel.contains("↑ M-2 · OBJ-1 · DIR-1"), "{sel}");
    a.handle(key(KeyCode::Char('i')));
    assert_eq!(a.composer.text(), "> [T-2]");
    // A cluster with nothing parked or running shows no empty-state sentence.
    a.aggregate = false;
    a.select(id(&a, "X-11"));
    let s = screen(&mut a);
    assert!(!s.contains("No running") && !s.contains("PARKED"), "{s}");
}

#[test]
fn the_divider_has_one_grabber_and_a_drag_resizes_the_sidebar_and_persists() {
    let root = scratch_root();
    let mut a = open(&root, "prod");
    let rows = render_to_text(&mut a, 120, 40).unwrap();
    let grabbers: Vec<(usize, usize)> = rows
        .iter()
        .enumerate()
        .flat_map(|(y, r)| r.chars().enumerate().filter(|(_, c)| *c == '⣿').map(move |(x, _)| (x, y)))
        .collect();
    // Two on the screen: the sidebar's, and the bucket's on its box's top
    // border (msg 235); the sidebar has exactly one.
    let area = a.clusters_area;
    let side: Vec<(usize, usize)> = grabbers.iter().copied().filter(|&(_, y)| y < (area.y + area.height) as usize).collect();
    assert_eq!(side.len(), 1, "exactly one sidebar grabber:\n{}", rows.join("\n"));
    assert_eq!(grabbers.len(), 2, "and the bucket's:\n{}", rows.join("\n"));
    let (gx, gy) = side[0];
    assert_eq!(gy, (area.y + area.height / 2) as usize, "the vertical middle");
    let divider = a.hits.iter().find(|(_, h)| *h == Hit::Divider).unwrap().0;
    assert_eq!(divider.x as usize, gx);
    assert_eq!((divider.y as usize, divider.height, divider.width), (gy, 1, 1), "only the grabber takes the drag (F52)");
    // A press on the divider line away from the grabber does not drag.
    mouse(&mut a, MouseEventKind::Down(MouseButton::Left), gx as u16, gy as u16 + 2);
    assert!(!a.dragging, "the line off the grabber is not a handle");
    mouse(&mut a, MouseEventKind::Up(MouseButton::Left), gx as u16, gy as u16 + 2);
    let before = a.sidebar_width(area.width);

    // Press on the grabber, drag right 20 columns, release.
    mouse(&mut a, MouseEventKind::Down(MouseButton::Left), gx as u16, gy as u16);
    assert!(a.dragging);
    mouse(&mut a, MouseEventKind::Drag(MouseButton::Left), gx as u16 + 10, gy as u16 + 3);
    mouse(&mut a, MouseEventKind::Drag(MouseButton::Left), gx as u16 + 20, gy as u16 + 3);
    mouse(&mut a, MouseEventKind::Up(MouseButton::Left), gx as u16 + 20, gy as u16 + 3);
    assert!(!a.dragging);
    let after = a.sidebar_width(area.width);
    assert_eq!(after, before + 20);
    // Redrawn there: the grabber follows.
    let rows = render_to_text(&mut a, 120, 40).unwrap();
    assert_eq!(rows[gy].chars().position(|c| c == '⣿'), Some(gx + 20));
    // Nothing under the drag was clicked.
    assert_eq!(a.selected, id(&a, "X-1"));
    // Persisted per environment, beside the draft.
    let file = soup_dir(&a.env).unwrap().join("layout.json");
    assert!(file.is_file());
    let mut b = open(&root, "prod");
    render_to_text(&mut b, 120, 40).unwrap();
    assert_eq!(b.sidebar_width(b.clusters_area.width), after);
    assert_eq!(open(&root, "dev").layout.sidebar_permille, None, "another environment keeps its own");
    // A drag never squeezes the page away or the sidebar to nothing.
    mouse(&mut b, MouseEventKind::Down(MouseButton::Left), divider.x + 20, gy as u16);
    mouse(&mut b, MouseEventKind::Up(MouseButton::Left), 119, gy as u16);
    assert!(b.sidebar_width(b.clusters_area.width) <= b.clusters_area.width - 24);
    let rows = render_to_text(&mut b, 120, 40).unwrap();
    assert!(rows.iter().all(|r| r.chars().count() <= 120));
    let d = b.hits.iter().find(|(_, h)| *h == Hit::Divider).unwrap().0;
    mouse(&mut b, MouseEventKind::Down(MouseButton::Left), d.x, gy as u16);
    mouse(&mut b, MouseEventKind::Up(MouseButton::Left), 0, gy as u16);
    assert!(b.sidebar_width(b.clusters_area.width) >= 12);
    // A drag with no press on the divider does nothing.
    let w = b.layout;
    mouse(&mut b, MouseEventKind::Drag(MouseButton::Left), 50, 10);
    mouse(&mut b, MouseEventKind::Up(MouseButton::Left), 50, 10);
    assert_eq!(b.layout, w);
}

// ── the selected row is visible on every list (8e4fca10 msg 96) ─────────────

/// One frame of `a` at 110x48, as cells, so a test can read a row's style.
fn cells(a: &mut App) -> ratatui::buffer::Buffer {
    use ratatui::backend::TestBackend;
    let mut term = ratatui::Terminal::new(TestBackend::new(110, 48)).unwrap();
    term.draw(|f| tui::cauldron::view::draw(f, a)).unwrap();
    term.backend().buffer().clone()
}

#[test]
fn the_selected_row_on_status_and_config_is_one_line_carrying_the_selection() {
    for tab in [CTab::Status, CTab::Config] {
        let mut a = app();
        a.set_ctab(tab);
        // Mesh's rows (Status opens on Tokens, whose accounts are cards,
        // since b964d571 msg 23).
        a.set_sttab(tui::cauldron::status::StatusTab::Mesh);
        a.handle(key(KeyCode::Down));
        // The marked lines: the one selected row's — no row grows (msg 165).
        // The selection is a BACKGROUND (msg 186), and no glyph marks it.
        let marked = |a: &mut App| -> (String, Vec<usize>) {
            let (rows, sel) = tui::cauldron::render_with_selection(a, 120, 70).unwrap();
            (rows.join("\n"), sel)
        };
        let (s, m) = marked(&mut a);
        assert!(!s.contains('▌'), "{tab:?}: no bar\n{s}");
        assert_eq!(m.len(), 1, "{tab:?}: exactly the selected row carries the selection\n{s}");
        a.handle(key(KeyCode::Down));
        let (t, n) = marked(&mut a);
        assert_eq!(n.len(), 1, "{tab:?}\n{t}");
        assert_ne!(m, n, "{tab:?}: the bar follows the selection\n{t}");
        assert_eq!(s.lines().count(), t.lines().count(), "{tab:?}: a row grew");
    }
}

#[test]
fn the_selected_cauldron_in_the_dropdown_is_the_selection_background() {
    let mut a = app();
    click_on(&mut a, Hit::Env);
    let buf = cells(&mut a);
    // Not paper any more (msg 186: selection is a subtle background, anywhere).
    let sel = tui_render::palette::c().sel_bg;
    let rows: Vec<String> = (0..buf.area.height)
        .filter(|&y| (0..buf.area.width).any(|x| buf[(x, y)].bg == sel))
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect();
    assert_eq!(rows.iter().filter(|r| r.contains("│ prod ")).count(), 1, "the selected cauldron is on the selection background: {rows:?}");
    assert!(!(0..buf.area.height).any(|y| (0..buf.area.width).any(|x| buf[(x, y)].bg == tui_render::palette::c().paper)), "no paper bar anywhere");
}

/// Open the focused cluster's log from its Status tab (`log ▸`, →).
fn open_log(a: &mut App) {
    a.trace.clear();
    a.focus = Focus::Content;
    a.item = a.items().iter().position(|x| *x == Item::Footer(Footer::Log)).expect("log ▸ on Status");
    a.handle(key(KeyCode::Right));
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

/// The first line inside the card whose top border carries `id`.
#[allow(dead_code)]
fn card_body(s: &str, id: &str) -> String {
    let rows: Vec<&str> = s.lines().collect();
    let top = rows.iter().position(|l| l.contains(&format!(" {id} ─"))).unwrap_or_else(|| panic!("no card {id}:\n{s}"));
    rows.get(top + 1).copied().unwrap_or("").to_string()
}
