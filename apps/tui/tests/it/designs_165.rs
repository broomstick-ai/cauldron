//! The three msg-165 designs, built (task 0153): THE DUMP (design 0138,
//! option C), ARTIFACTS (0146, four bands) and RUNNING / PARKED (0142,
//! option A) — driven headlessly through `App::handle`, keys and mouse,
//! against a scratch cauldron root (never `~/.cauldron`). One test per
//! thing the designs promise.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Focus, Hit, Item, Modal, Outbox, KTab};
use tui::cauldron::dump::ChipAt;
use tui::cauldron::model::RippleState;
use tui::cauldron::render_to_text;
use tui::cauldron::status::StatusTab;
use tui::cauldron::status::{self, census, counts, cluster_counts};
use tui::cauldron::store::{seed_dir, soup_dir};
use tui::cauldron::tags::{self, Tag};

// ── fixtures ────────────────────────────────────────────────────────────────

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "designs165-{}-{}-{}",
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

fn open_in(root: &Path) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    crate::live_pool::seeded(App::open(env_in(root, "prod"), seed_dir(), true).expect("seed loads"))
}

fn open() -> (App, PathBuf) {
    let root = scratch_root();
    (open_in(&root), root)
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn press(a: &mut App, keys: &[KeyCode]) {
    for &k in keys {
        a.handle(key(k));
    }
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(key(KeyCode::Char(c)));
    }
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 120, 60).unwrap().join("\n")
}

fn click_at(a: &mut App, x: u16, y: u16) {
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE }));
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE }));
}

fn click_hit(a: &mut App, hit: Hit) {
    screen(a);
    let r = a.hits.iter().find(|(_, h)| *h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {hit:?} on screen"));
    click_at(a, r.x, r.y);
}

fn cluster(a: &mut App, key: &str) {
    let g = a.forest.find(key).unwrap();
    a.select(g);
}

fn select_item(a: &mut App, it: Item) {
    a.item = a.items().iter().position(|x| *x == it).unwrap_or_else(|| panic!("{it:?} is not on the page: {:?}", a.items()));
    a.focus = Focus::Content;
}

/// The Artifacts row whose key is `key`.
fn art(a: &App, key: &str) -> Item {
    tui::cauldron::pages::page(a, 100).arts.into_iter().find(|r| r.key == key).unwrap_or_else(|| panic!("no Artifacts row `{key}`")).item
}

fn chip_of(a: &App, kind: &str) -> Tag {
    tags::scan(&a.composer.text()).into_iter().map(|(_, t)| t).find(|t| t.chip_id().is_some_and(|(k, _)| k == kind)).unwrap_or_else(|| panic!("no {kind} chip in {:?}", a.composer.text()))
}

/// The bucket's text rows, inside its box, as drawn: from the row under its
/// top border to the row above its bottom one, `│ ` and ` │` taken off.
fn dump_rows(a: &mut App) -> Vec<String> {
    let rows = render_to_text(a, 120, 60).unwrap();
    let top = rows.iter().rposition(|l| l.starts_with("╭") && l.trim_end().ends_with("bucket ╮")).expect("the bucket's top border");
    rows[top + 1..rows.len() - 1].iter().map(|l| l.trim_start_matches("│ ").trim_end().trim_end_matches('│').trim_end().to_string()).collect()
}

// ── THE BUCKET (design 0138, option C; msg 235) ─────────────────────────────

/// K1 K4 (msg 235: "We're gonna have one bucket ... because it's all going
/// into one soup"; "we're actually gonna take it out"): ONE text for the
/// cauldron, kept in `bucket.json`, whichever cluster is selected; a blank
/// line still cuts its notes, but there is no ordinal in a gutter.
#[test]
fn k1_k4_the_bucket_is_one_text_for_the_cauldron_and_carries_no_ordinals() {
    let (mut a, root) = open();
    a.focus_input();
    typed(&mut a, "first thought");
    a.handle(ctrl('j'));
    a.handle(ctrl('j'));
    typed(&mut a, "second thought");
    let rows = dump_rows(&mut a);
    assert_eq!(rows[0], "first thought", "no ordinal: {rows:?}");
    assert_eq!(rows[1].trim(), "");
    assert_eq!(rows[2], "second thought", "{rows:?}");
    assert_eq!(tui::cauldron::dump::notes(&a.composer.text()).len(), 2, "the blank line still cuts the notes");
    // One file for the cauldron.
    let dir = soup_dir(&env_in(&root, "prod")).unwrap();
    let disk = std::fs::read_to_string(dir.join("bucket.json")).unwrap();
    assert!(disk.contains("second thought"), "{disk}");
    assert!(!dir.join("drafts").exists(), "no bucket per cluster");
    // Another cluster: the same bucket.
    cluster(&mut a, "X-9");
    assert_eq!(a.composer.text(), "first thought\n\nsecond thought");
    a.focus = Focus::Tree;
    let s = screen(&mut a);
    assert!(!s.contains('▪'), "{s}");
}

/// K2 K3 (msg 235: "It needs to have a minimum number of lines that it takes
/// up. It needs to look like a text area ... make use of the same border ...
/// a draggable bar that you can like resize it ... a minimum like height ...
/// of three lines and a maximum of six lines. But if you drag past the
/// maximum, that's okay"; "if the bucket's on the right, put the copy on the
/// left"): the bucket is a box under the soup's, both from the line under
/// the top bar to the last; its legend `bucket` top right, `⧉ copy` at its
/// bottom left (b964d571 msg 23); three text rows empty, growing to six as it fills; its grabber
/// drags it — never under three, past six if he likes — and the height is
/// kept beside the sidebar's share.
#[test]
fn k2_k3_the_bucket_is_a_box_three_to_six_rows_and_its_divider_drags() {
    let (mut a, root) = open();
    a.focus = Focus::Tree;
    let rows = render_to_text(&mut a, 120, 40).unwrap();
    assert!(rows[2].starts_with('╭') && rows[2].trim_end().ends_with("surface ╮"), "the surface's box, its legend top right: {:?}", rows[2]);
    let top = rows.iter().rposition(|l| l.starts_with("╭") && l.trim_end().ends_with("bucket ╮")).expect("the bucket's box");
    assert!(rows[top].trim_end().ends_with("bucket ╮"), "its legend top right: {:?}", rows[top]);
    assert_eq!(rows.len() - 1 - top, 4, "three text rows and its bottom border");
    assert!(rows[39].starts_with("╰") && rows[39].ends_with("┤ ⧉ copy ├╯"), "the boxes run to the last line, the copy at its bottom right (msg 457)");
    // It grows to six as it fills, then scrolls inside itself.
    a.focus_input();
    for i in 0..30 {
        typed(&mut a, &format!("line {i}"));
        a.handle(ctrl('j'));
    }
    let rows = render_to_text(&mut a, 120, 40).unwrap();
    let top = rows.iter().rposition(|l| l.starts_with("╭") && l.trim_end().ends_with("bucket ╮")).unwrap();
    assert_eq!(rows.len() - 2 - top, 6, "capped at six");
    // The grabber, on its top edge: drag it up past six, then down under three.
    let g = a.hits.iter().find(|(_, h)| *h == Hit::BucketDivider).map(|(r, _)| *r).expect("the grabber");
    assert_eq!(g.y as usize, top);
    let drag = |a: &mut App, to: u16| {
        a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: g.x, row: g.y, modifiers: KeyModifiers::NONE }));
        a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Drag(MouseButton::Left), column: g.x, row: to, modifiers: KeyModifiers::NONE }));
        a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: g.x, row: to, modifiers: KeyModifiers::NONE }));
    };
    drag(&mut a, 20);
    let rows = render_to_text(&mut a, 120, 40).unwrap();
    let top = rows.iter().rposition(|l| l.starts_with("╭") && l.trim_end().ends_with("bucket ╮")).unwrap();
    assert_eq!(top, 20, "dragged past six: it is as tall as he dragged it");
    assert_eq!(a.layout.bucket_rows, Some(18));
    let kept = std::fs::read_to_string(soup_dir(&env_in(&root, "prod")).unwrap().join("layout.json")).unwrap();
    assert!(kept.contains("\"bucket_rows\": 18"), "{kept}");
    let g = a.hits.iter().find(|(_, h)| *h == Hit::BucketDivider).map(|(r, _)| *r).unwrap();
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: g.x, row: g.y, modifiers: KeyModifiers::NONE }));
    a.handle(Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: g.x, row: 39, modifiers: KeyModifiers::NONE }));
    assert_eq!(a.layout.bucket_rows, Some(3), "never under three");
    // `⧉ copy` copies at one click, from its place at the bottom right.
    a.type_text("x");
    click_hit(&mut a, Hit::BucketCopy);
    assert!(matches!(a.outbox.last(), Some(Outbox::Copy(_))));
}

#[test]
fn i_starts_an_anchored_note_and_a_send_is_a_numbered_record_of_notes() {
    let (mut a, root) = open();
    a.set_ktab(KTab::Questions);
    a.focus = Focus::Content;
    let q1 = a.items().iter().position(|i| matches!(i, Item::Question { serial: 1, .. })).unwrap();
    a.item = q1;
    a.handle(key(KeyCode::Char('i')));
    typed(&mut a, " four");
    a.handle(ctrl('j'));
    a.handle(ctrl('j'));
    typed(&mut a, "bands everywhere");
    assert_eq!(a.composer.text(), "> [Q-1] four\n\nbands everywhere");
    a.handle(key(KeyCode::Enter));
    assert!(a.composer.is_blank());
    let d = a.sends.sends.last().unwrap().clone();
    assert_eq!(d.seq, 12, "after the seed's dump 11");
    assert_eq!(d.notes.len(), 2);
    assert_eq!(d.notes[0].anchors, vec!["Q-1"]);
    assert!(d.notes[1].anchors.is_empty());
    assert_eq!((d.notes[1].start, d.notes[1].end), (14, 30), "character offsets of the stored text");
    // On disk, one line of dumps.jsonl.
    let log = std::fs::read_to_string(soup_dir(&env_in(&root, "prod")).unwrap().join("dumps.jsonl")).unwrap();
    assert!(log.contains("\"notes\"") && log.contains("\"anchors\":[\"Q-1\"]"), "{log}");
    // "dump twelve, note two" is a tag, and it lands where the dump did: on
    // the cluster that holds Q-1.
    let t = tags::scan("[dump:12.2]")[0].1.clone();
    assert_eq!(t, Tag::thing("dump", "12.2"));
    assert_eq!(a.clusters_of(&t), vec!["X-1".to_string()]);
    // An untagged dump lands on no cluster: into the soup (msg 235).
    a.dump("just a thought");
    assert!(a.sends.sends.last().unwrap().targets.is_empty());
    assert!(a.notice.as_deref().is_some_and(|n| n.ends_with("→ the soup")), "{:?}", a.notice);
}

#[test]
fn ctrl_s_sends_the_carets_note_alone_and_esc_hands_back_the_page() {
    let (mut a, _r) = open();
    a.focus = Focus::Content;
    a.focus_input();
    typed(&mut a, "keep this");
    a.handle(ctrl('j'));
    a.handle(ctrl('j'));
    typed(&mut a, "send this now");
    a.handle(ctrl('s'));
    let d = a.sends.sends.last().unwrap().clone();
    assert_eq!(d.text, "send this now");
    assert_eq!(d.notes.len(), 1);
    assert_eq!(a.composer.text(), "keep this", "the note left the text; the rest stays");
    assert_eq!(a.focus, Focus::Input);
    // Esc leaves the bucket for the page it was entered from (Q2).
    a.handle(key(KeyCode::Esc));
    assert_eq!(a.focus, Focus::Content);
    a.handle(key(KeyCode::Esc));
    assert_eq!(a.focus, Focus::Input, "Esc is a two-way switch");
}

/// K5 (msg 235: "a paste should be inserted as like a paste like artifact
/// ... fuck it just all pastes"): EVERY paste is a chip, short or long, one
/// unit to the caret and to Backspace.
#[test]
fn k5_every_paste_is_a_chip_one_unit_to_the_caret_and_to_backspace() {
    let (mut a, _r) = open();
    a.focus_input();
    typed(&mut a, "see ");
    a.handle(Event::Paste("fn main() {\n    let a = 1;\n    let b = 2;\n}".into()));
    let t = chip_of(&a, "paste");
    let r = t.render();
    assert!(r.starts_with("[paste ") && r.ends_with(": \"fn main() {\" 4 lines · 43 chars]"), "{r}");
    // A short paste is a chip too.
    a.handle(Event::Paste(" short".into()));
    let chips = tags::scan(&a.composer.text()).into_iter().filter(|(_, t)| t.chip_id().is_some()).count();
    assert_eq!(chips, 2, "{}", a.composer.text());
    assert!(a.composer.text().ends_with(": \"short\" 6 chars]"), "{}", a.composer.text());
    a.handle(key(KeyCode::Backspace));
    // ← jumps the whole chip; → jumps it back.
    a.handle(key(KeyCode::Left));
    assert_eq!(tui::cauldron::dump::caret_offset(&a.composer), "see ".len());
    a.handle(key(KeyCode::Right));
    assert_eq!(tui::cauldron::dump::caret_offset(&a.composer), a.composer.text().len());
    // Backspace takes all of it.
    a.handle(key(KeyCode::Backspace));
    assert_eq!(a.composer.text(), "see ");
    // ^Z puts it back.
    a.handle(ctrl('z'));
    assert!(a.composer.text().contains("[paste "), "{}", a.composer.text());
    // Delete before it takes it too.
    a.handle(key(KeyCode::Left));
    a.handle(key(KeyCode::Delete));
    assert_eq!(a.composer.text(), "see ");
}

#[test]
fn a_chip_opens_to_its_full_text_editable_kept_inserted_as_text_or_discarded() {
    let (mut a, root) = open();
    a.focus_input();
    a.handle(Event::Paste("alpha\nbeta\ngamma".into()));
    let (_, id) = chip_of(&a, "paste").chip_id().map(|(k, i)| (k.to_string(), i.to_string())).unwrap();
    let body = soup_dir(&env_in(&root, "prod")).unwrap().join(format!("bucket/artifacts/{id}.txt"));
    assert_eq!(std::fs::read_to_string(&body).unwrap(), "alpha\nbeta\ngamma");
    // ^O opens it in its modal; letters are text there; Esc KEEPS the edit.
    a.handle(ctrl('o'));
    assert!(matches!(&a.modal, Some(Modal::Chip { .. })));
    let s = screen(&mut a);
    assert!(s.contains("paste · 3 lines · 16 chars") && s.contains("insert as text") && s.contains("discard"), "{s}");
    assert!(!s.contains("note 1"), "no ordinal (msg 235): {s}");
    typed(&mut a, "!");
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none());
    assert_eq!(std::fs::read_to_string(&body).unwrap(), "alpha\nbeta\ngamma!");
    assert!(chip_of(&a, "paste").render().contains("17 chars"), "relabelled: {}", a.composer.text());
    // A click on the chip opens it; ↓ past the text reaches the buttons;
    // Enter on `insert as text` puts the text in its place.
    let s = screen(&mut a);
    let _ = s;
    let hit = a.hits.iter().find(|(_, h)| matches!(h, Hit::Chip(_))).map(|(_, h)| *h).expect("the chip is a click target");
    click_hit(&mut a, hit);
    assert!(a.modal.is_some());
    press(&mut a, &[KeyCode::Down, KeyCode::Down, KeyCode::Down]);
    assert!(matches!(&a.modal, Some(Modal::Chip { at: ChipAt::Insert, .. })));
    a.handle(key(KeyCode::Enter));
    assert_eq!(a.composer.text(), "alpha\nbeta\ngamma!");
    // Discard asks once more: a second Enter does it, anything else keeps.
    // (Undo takes the insert back in two steps: the text, then the chip.)
    a.handle(ctrl('z'));
    a.handle(ctrl('z'));
    assert!(a.composer.text().contains("[paste "), "undo restores the chip: {}", a.composer.text());
    a.handle(key(KeyCode::Left));
    a.handle(ctrl('o'));
    press(&mut a, &[KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Right, KeyCode::Right]);
    a.handle(key(KeyCode::Enter));
    assert!(screen(&mut a).contains("discard? Enter again"));
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none() && !a.composer.text().contains("[paste "), "{}", a.composer.text());
    assert!(!body.exists(), "its body went with it");
}

/// K5 (msg 235: "I actually don't want to support transcription directly
/// into the Cauldron ... it's the same thing for the speech-to-text it just
/// comes in as a paste"): there is no dictation — ⌥M (or `µ`), `d` and ^V do
/// nothing to the bucket — and a speech-to-text tool's words, a paste,
/// arrive as a paste chip.
#[test]
fn k5_there_is_no_dictation_and_speech_to_text_arrives_as_a_paste() {
    let (mut a, _r) = open();
    a.set_ktab(KTab::Questions);
    a.focus = Focus::Content;
    for e in [Event::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT)), key(KeyCode::Char('µ')), key(KeyCode::Char('d'))] {
        a.handle(e);
    }
    assert!(a.composer.is_blank(), "{}", a.composer.text());
    a.focus_input();
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT)));
    a.handle(ctrl('v'));
    assert!(!a.composer.text().contains("[audio") && !a.composer.text().contains('m'), "{}", a.composer.text());
    a.handle(Event::Paste("so the key is in the shared vault".into()));
    assert!(chip_of(&a, "paste").render().contains("so the key is in the shared vault"));
    assert!(!tui::cauldron::app::KEYMAP.iter().any(|(k, w)| k.contains("⌥M") || w.contains("dictation")));
    let s = screen(&mut a);
    assert!(!s.contains("● rec"), "{s}");
}

/// K1: what the buckets held before there was one — a bucket per cluster
/// (`drafts/<cluster>.json`, design 0138) and the one before that
/// (`draft.json`) — comes into the ONE bucket, once, a blank line between
/// them; a dictation's chip comes in as a paste with its body; nothing old
/// is deleted.
#[test]
fn k1_the_buckets_of_before_are_adopted_into_the_one_bucket_once() {
    let root = scratch_root();
    let dir = soup_dir(&env_in(&root, "prod")).unwrap();
    std::fs::create_dir_all(dir.join("drafts/cauldron-ripples/artifacts")).unwrap();
    let draft = |t: &str| format!(r#"{{"nonce":"n","writer":"w","updated_at":"t","text":{}}}"#, serde_json::to_string(t).unwrap());
    std::fs::write(dir.join("drafts/cauldron.json"), draft("on the cauldron")).unwrap();
    std::fs::write(dir.join("drafts/cauldron-ripples.json"), draft("said aloud [audio a7f3: \"so the key\" 10 chars]")).unwrap();
    std::fs::write(dir.join("drafts/cauldron-ripples/artifacts/a7f3.txt"), "so the key").unwrap();
    std::fs::write(dir.join("draft.json"), draft("what he was writing")).unwrap();
    let a = open_in(&root);
    assert_eq!(a.composer.text(), "said aloud [paste a7f3: \"so the key\" 10 chars]\n\non the cauldron\n\nwhat he was writing");
    assert_eq!(a.chips.read("a7f3").as_deref(), Some("so the key"), "the chip's body came with it");
    assert!(std::fs::read_to_string(dir.join("bucket.json")).unwrap().contains("what he was writing"));
    assert!(dir.join("draft.json").exists() && dir.join("drafts/cauldron.json").exists(), "the old files are never deleted");
    // Once: the next face finds it in its new place, not adopted twice.
    let b = open_in(&root);
    assert_eq!(b.composer.text(), a.composer.text());
    // His prod data had the same text in `draft.json` and in the first
    // per-cluster bucket (it was copied there): it is adopted ONCE.
    let root = scratch_root();
    let dir = soup_dir(&env_in(&root, "prod")).unwrap();
    std::fs::create_dir_all(dir.join("drafts")).unwrap();
    std::fs::write(dir.join("drafts/cauldron.json"), draft("the same words")).unwrap();
    std::fs::write(dir.join("draft.json"), draft("the same words\n")).unwrap();
    assert_eq!(open_in(&root).composer.text(), "the same words");
}

#[test]
fn the_dump_ripple_is_in_the_policy_as_definitions_only() {
    let (a, _r) = open();
    let p = &a.rstore.live;
    let dump = p.get("dump").expect("the aggregate ripple");
    assert_eq!(dump.triggers[0].on, "dump.received");
    let calls: Vec<String> = dump.steps().iter().flat_map(|s| std::iter::once(*s).chain(s.children())).filter_map(|s| s.call.clone()).collect();
    assert!(calls.contains(&"fragment".to_string()) && calls.contains(&"apply".to_string()), "{calls:?}");
    let frag = p.get("fragment").unwrap();
    let set = &frag.action_sets[0];
    assert!(set.terminal && set.exclusive && set.name == "FRAGMENT");
    let names: Vec<&str> = set.actions.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, ["span", "group"]);
    // Strict: offsets and ids, no free text but the ids.
    for x in &set.actions {
        for prm in &x.params {
            assert!(!prm.ty.starts_with("text") || prm.name == "id", "{}.{} is free text", x.name, prm.name);
        }
    }
    assert!(p.get("intake").is_none() && p.get("itemize").is_none(), "renamed (0138 §7)");
    for c in ["dump.received", "dump.fragmented", "dump.processed", "dump.group_failed"] {
        assert!(tui::cauldron::ripples::EVENT_CLASSES.contains(&c), "{c}");
    }
    p.check().expect("the policy is still valid");
}

// ── ARTIFACTS (design 0146) ─────────────────────────────────────────────────

#[test]
fn soup_is_four_bands_open_along_the_active_path_only() {
    let (mut a, _r) = open();
    a.set_ktab(KTab::Nodes);
    let s = screen(&mut a);
    let at = |t: &str| s.find(t).unwrap_or_else(|| panic!("{t}:\n{s}"));
    // msg 186: Soup and Artifacts are one tab — REGIONS · ARTIFACTS (it was
    // CREATED) · REPOS · DIRECTORIES; FILE SYSTEM is config. Open where a
    // run holds a worktree or an artifact is being written.
    assert!(at("▾ Regions") < at("▾ Artifacts") && at("▾ Artifacts") < at("▾ Repos") && at("▾ Repos") < at("▸ Directories"));
    assert!(!s.contains("FILE SYSTEM") && !s.contains("CREATED"), "{s}");
    for open in ["▾ cauldron  /Users/demo/code/cauldron ↗", "▾ Worktrees", "▾ Active", "▾ Reports"] {
        assert!(s.contains(open), "{open}:\n{s}");
    }
    for closed in ["▸ Idle", "▸ Merged", "▸ Missing", "▸ Blueprint", "▸ Files  /Users/demo/code/cauldron ↗"] {
        assert!(s.contains(closed), "{closed}:\n{s}");
    }
    // A header is ONE line, its metadata on the right.
    let wts = s.lines().find(|l| l.contains("▾ Worktrees")).unwrap();
    let n = tui::cauldron::artpage::worktrees(&a, "cauldron").len();
    assert!(wts.contains(&format!("{n} worktrees · 3 active · ")) && wts.contains(" merged · ") && wts.contains("1 missing"), "{wts}");
    // The status bands partition the worktrees.
    use tui::cauldron::artifacts::WtState;
    let total: usize = WtState::ALL.iter().map(|&st| tui::cauldron::artpage::worktrees(&a, "cauldron").into_iter().filter(|w| tui::cauldron::artpage::wt_state(&a, w) == st).count()).sum();
    assert_eq!(total, n);
}

#[test]
fn every_worktree_row_has_the_same_columns_and_opens_its_detail_in_place() {
    let (mut a, _r) = open();
    a.set_ktab(KTab::Nodes);
    a.art_toggled.insert("wts:cauldron:IDLE".into());
    let s = screen(&mut a);
    let rows: Vec<&str> = s.lines().filter(|l| l.contains(" ↗ ") && (l.contains("▶ cauldron-wt") || l.contains("○ cauldron-wt"))).collect();
    assert!(rows.len() > 10, "{s}");
    let col = |l: &str| l.find(" ↗ ").map(|b| l[..b].chars().count());
    assert!(rows.iter().all(|l| col(l) == col(rows[0])), "the ↗ column lines up:\n{}", rows.join("\n"));
    assert!(s.contains("16 more ▸") || s.contains(" more ▸"), "a long band ends in `n more ▸`: {s}");
    // → on a worktree: its detail rows, in place; ← closes them.
    let wt = art(&a, "wt:/Users/demo/code/cauldron-wt-globs2");
    select_item(&mut a, wt);
    a.handle(key(KeyCode::Right));
    let s = screen(&mut a);
    assert!(s.contains("~/code/cauldron-wt-globs2 ↗") || s.contains("/cauldron-wt-globs2 ↗"), "{s}");
    assert!(s.contains("behind main") && s.contains("T-3 Iteration two"), "{s}");
    assert!(s.contains("last commit"), "{s}");
    assert!(a.tag_for(wt).unwrap().render().starts_with("[WT-"));
    a.handle(key(KeyCode::Left));
    assert!(!screen(&mut a).contains("behind main"));
    // ← again climbs to its status band.
    a.handle(key(KeyCode::Left));
    assert_eq!(a.items()[a.item], art(&a, "wts:cauldron:ACTIVE"));
}

#[test]
fn an_artifact_is_a_file_node_with_intent_and_fact_and_its_place_is_computed() {
    let (mut a, _r) = open();
    a.set_ktab(KTab::Nodes);
    let s = screen(&mut a);
    // The report this task writes: being written, by its maker, running.
    let row = s.lines().find(|l| l.contains("0154-report-build-designs-165")).expect(&s);
    assert!(row.contains("▶ 0154") && row.contains("T-3"), "{row}");
    let created = s.lines().find(|l| l.contains("▾ Artifacts")).unwrap();
    assert!(created.contains("1 being written"), "{created}");
    // → opens its detail rows: kind, state, maker, what it is for, where.
    let it = art(&a, "art:ART-154");
    select_item(&mut a, it);
    assert_eq!(a.tag_for(it).unwrap().render(), "[ART-154]");
    a.handle(key(KeyCode::Right));
    // The whole page (the card is taller than the frame's share of it).
    let s: String = tui::cauldron::pages::page(&a, 110).lines.iter().map(|l| l.to_string() + "\n").collect();
    // Its details are a TABLE now (msg 186, task 0166 V6).
    let field = |k: &str, v: &str| s.lines().any(|l| l.contains(&format!("{k:<8} │ {v}")));
    assert!(field("kind", "report") && field("size", "—") && field("state", "being written"), "{s}");
    assert!(s.lines().any(|l| l.contains("written  │ ") && l.contains("ago by T-3")) && field("for", ""), "{s}");
    assert!(field("in", "harness · main") && field("verified", "not verified"), "{s}");
    // A file that is gone is under MISSING (the page and the deck).
    let missing = s.lines().find(|l| l.contains("▸ Missing") && l.trim_end_matches('│').trim_end().ends_with("2 artifacts")).is_some();
    assert!(missing, "{s}");
    // With everything below, Interface's reports join.
    a.toggle_aggregate();
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("▾ Artifacts") && !l.contains(" 3 ·")), "{s}");
}

#[test]
fn a_path_copies_on_one_click_its_open_icon_opens_and_c_and_o_do_the_same() {
    let (mut a, _r) = open();
    a.set_ktab(KTab::Nodes);
    screen(&mut a);
    let path = "/Users/demo/code/cauldron-wt-globs2".to_string();
    let ix = a.hotspot_paths.iter().position(|p| *p == path).unwrap() as u16;
    click_hit(&mut a, Hit::CopyAt(ix));
    assert_eq!(a.outbox, vec![Outbox::Copy(path.clone())]);
    assert_eq!(a.flash.as_deref(), Some(path.as_str()), "the copied cell flashes");
    assert_eq!(a.items()[a.item], art(&a, &format!("wt:{path}")), "and its row is selected");
    let ix = a.hotspot_paths.iter().position(|p| *p == path).unwrap() as u16;
    click_hit(&mut a, Hit::OpenAt(ix));
    assert_eq!(a.outbox.last(), Some(&Outbox::Open(path.clone())));
    a.outbox.clear();
    a.focus = Focus::Content;
    press(&mut a, &[KeyCode::Char('c'), KeyCode::Char('o')]);
    assert_eq!(a.outbox, vec![Outbox::Copy(path.clone()), Outbox::Open(path)]);
    // Nothing on the tab writes (Q2: show only).
    assert!(!screen(&mut a).contains("remove ▸"));
}

#[test]
fn the_file_system_is_config_its_roots_on_config_directories_the_same_on_every_cluster() {
    let (mut a, _r) = open();
    // msg 186: "I don't know why we have file system in here, but that's
    // like config information": no FILE SYSTEM band on any cluster's Soup.
    for key in ["X-1", "X-8"] {
        cluster(&mut a, key);
        a.set_ktab(KTab::Nodes);
        assert!(!screen(&mut a).contains("FILE SYSTEM"));
    }
    // Config › Cauldron › Directories lists every root.
    a.set_ctab(CTab::Settings);
    a.open_settings_path("directories").unwrap();
    let s = screen(&mut a);
    assert!(s.contains("▾ Roots") && s.contains("cauldron home"), "{s}");
    for r in &a.repos.repos {
        assert!(s.lines().any(|l| l.contains(&super_tilde(&r.path)) && l.contains(" repo ")), "{}: {s}", r.name);
    }
    assert_eq!(a.file_roots().len(), 1 + a.repos.repos.len() + a.arts.directories.iter().filter(|d| !a.repos.repos.iter().any(|r| r.path == d.path)).count());
}

#[test]
fn the_worktrees_are_read_from_git_off_the_ui_thread_in_a_live_face() {
    // A scratch repo with one linked worktree one commit ahead of main.
    let root = scratch_root();
    let repo = root.join("repo");
    let git = |dir: &Path, args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success(), "{args:?}");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "root"]);
    let wt = root.join("repo-wt");
    git(&repo, &["worktree", "add", "-q", "-b", "feature", wt.to_str().unwrap()]);
    git(&wt, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "ahead by one"]);
    std::fs::write(wt.join("x.txt"), "dirty").unwrap();
    let read = tui::cauldron::artifacts::read_worktrees("repo", repo.to_str().unwrap(), "main").unwrap();
    assert_eq!(read.len(), 1, "the primary checkout is the repo, never a worktree");
    let w = &read[0];
    assert_eq!((w.branch.as_deref(), w.ahead, w.behind, w.dirty, w.present), (Some("feature"), 1, 0, 1, true));
    assert_eq!(w.last_commit.as_ref().unwrap().subject, "ahead by one");
    assert!(tui::cauldron::artifacts::read_worktrees("x", root.join("nope").to_str().unwrap(), "main").is_none());
    // --dump never reads git; a live face does, and lands it on a tick.
    let (mut a, _r) = open();
    a.sync();
    assert!(a.git_rx.is_none(), "not live: the seed's worktrees");
}

#[test]
fn scrolling_never_changes_a_rows_shape_on_the_new_lists() {
    let (mut a, _r) = open();
    // Artifacts with its bands open and Status with every band open.
    a.set_ktab(KTab::Nodes);
    for k in ["wts:cauldron:IDLE", "wts:cauldron:MERGED", "wts:cauldron:MISSING", "bp:cauldron", "directories", "created:missing"] {
        a.art_toggled.insert(k.into());
    }
    let lines = |a: &App| tui::cauldron::pages::page(a, 100).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
    let first = lines(&a);
    for i in 0..a.items().len() {
        a.item = i;
        assert_eq!(lines(&a), first, "Artifacts: selecting item {i} changed the page");
    }
    a.set_ctab(CTab::Status);
    a.stnav.insert("pending".into());
    let lines = |a: &App| status::view(a, 100).lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
    let first = lines(&a);
    for i in 0..status::view(&a, 100).items.len() {
        a.rows[0] = i;
        assert_eq!(lines(&a), first, "Status: selecting row {i} changed the page");
    }
    // The dump: its rows are the text's, wherever the caret is.
    a.set_ctab(CTab::Cauldron);
    a.focus_input();
    typed(&mut a, "one");
    a.handle(ctrl('j'));
    a.handle(ctrl('j'));
    typed(&mut a, "two");
    let before = dump_rows(&mut a).iter().map(|l| l.chars().skip(4).collect::<String>()).collect::<Vec<_>>();
    a.handle(ctrl('a'));
    a.handle(key(KeyCode::Up));
    let after = dump_rows(&mut a).iter().map(|l| l.chars().skip(4).collect::<String>()).collect::<Vec<_>>();
    assert_eq!(before, after);
}

// ── RUNNING · PARKED · PENDING (design 0142, option A) ──────────────────────

/// S1 S2 (msg 235): Status's sub-tabs are Tokens · Mesh · Soup · Bus (b964d571
/// msg 23 put Tokens first), and
/// the strip carries no metadata ("Don't have any metadata in the tab
/// header"); Mesh is each device with the services running on it (the
/// threads daemon first); running, parked and pending are the Bus's
/// EXECUTIONS, in that order.
#[test]
fn s1_s2_status_is_mesh_tokens_soup_bus_and_its_strip_says_nothing_else() {
    let (mut a, _r) = open();
    a.set_ctab(CTab::Status);
    let s = screen(&mut a);
    let (_, p, _) = counts(&a);
    let strip = s.lines().nth(3).unwrap();
    assert_eq!(strip.trim_start_matches('│').trim().trim_end_matches('│').trim(), "Ledger | Tokens | Mesh | Soup | Bus", "{strip}");
    // The Ledger is first and shown (8e4fca10 msg 457), Tokens after it.
    assert_eq!(a.sttab, StatusTab::Ledger);
    a.set_sttab(StatusTab::Mesh);
    let s = screen(&mut a);
    // Mesh: the device, then the daemon and every service, nested in it.
    let at = |s: &str, t: &str| s.find(t).unwrap_or_else(|| panic!("{t}:\n{s}"));
    assert!(at(&s, "▾ example-mac") < at(&s, "▸ threads daemon") && at(&s, "▸ threads daemon") < at(&s, "▸ tts "), "{s}");
    assert!(s.lines().any(|l| l.contains("▸ threads daemon") && l.contains("● live · answered 42s ago")), "{s}");
    a.set_sttab(StatusTab::Tokens);
    assert!(screen(&mut a).contains("Claude Code"));
    // Executions: running, parked and pending, in that order, on the Bus.
    a.set_sttab(StatusTab::Bus);
    let s = screen(&mut a);
    assert!(at(&s, "▾ Executions") < at(&s, "▾ Running") && at(&s, "▾ Running") < at(&s, "▾ Parked") && at(&s, "▾ Parked") < at(&s, "▸ Pending"), "{s}");
    // PARKED: ONE number, broken down by cause.
    let parked = s.lines().find(|l| l.contains("▾ Parked")).unwrap();
    assert!(parked.contains(&format!("◆ {p}")) && parked.contains("1 threshold") && parked.contains("1 continuity") && parked.contains("1 share"), "{parked}");
    // Each parked row: what, why, and the sources it could run on — a label.
    let t8 = s.lines().find(|l| l.contains("Cauldron T-8")).unwrap();
    assert!(t8.contains("weekly 80% on every account") && t8.contains("Claude Code · Codex") && t8.contains("41m"), "{t8}");
    // Never a parked count under a token source or an account.
    a.set_sttab(StatusTab::Tokens);
    let t = screen(&mut a);
    assert!(!t.contains('◆') && !t.contains("parked"), "{t}");
    a.set_sttab(StatusTab::Bus);
    // Pending opens to its queue; its head takes the next slot.
    a.rows[0] = status::view(&a, 120).items.iter().position(|i| i.key == "pending").unwrap();
    a.handle(key(KeyCode::Right));
    let s = screen(&mut a);
    assert!(s.lines().any(|l| l.contains("◇ ") && l.contains("next slot")), "{s}");
    assert!(s.contains("↻ retry 2/3"), "{s}");
}

#[test]
fn every_count_is_on_an_axis_that_partitions_so_the_numbers_add_up() {
    let (a, _r) = open();
    let (r, p, pe) = counts(&a);
    // Σ clusters (own) + the background no cluster owns = the strip.
    let mut sum = (0, 0, 0);
    for g in 0..a.forest.clusters.len() {
        let (x, y, z) = cluster_counts(&a, g, false);
        sum = (sum.0 + x, sum.1 + y, sum.2 + z);
    }
    let c = census(&a);
    let bg = |st: RippleState| c.iter().filter(|e| e.cluster.is_none() && e.state == st).count();
    assert_eq!((sum.0 + bg(RippleState::Running), sum.1 + bg(RippleState::Parked), sum.2 + bg(RippleState::Pending)), (r, p, pe));
    // A root under ^G is its subtree's sum.
    let root = a.forest.find("X-1").unwrap();
    let sub = a.forest.subtree(root).iter().map(|&g| cluster_counts(&a, g, false)).fold((0, 0, 0), |s, x| (s.0 + x.0, s.1 + x.1, s.2 + x.2));
    assert_eq!(cluster_counts(&a, root, true), sub);
    // Work the graph holds is none of the three (Q1): cauldron T-5 is
    // blocked by Q-2, and is counted nowhere.
    let t5 = c.iter().find(|e| e.cluster == Some(root) && e.what == status::What::Task(5));
    assert!(t5.is_none(), "{t5:?}");
    // Running per account sums to the source, the sources to the strip.
    let mut a = a;
    a.set_ctab(CTab::Status);
    // V1.7: the endpoints' band is greyed and closed at first; opened, its
    // runs still count.
    a.stnav.insert("tok:endpoints".into());
    a.set_sttab(StatusTab::Tokens);
    let s = render_to_text(&mut a, 120, 80).unwrap().join("\n");
    let cc = s.lines().find(|l| l.contains("Claude Code")).unwrap();
    let per: usize = ["sam-broomstick", "acct2", "acct3"].iter().map(|n| {
        let l = s.lines().find(|l| l.contains(&format!("• {n} "))).unwrap();
        l.split("▶ ").nth(1).unwrap().split(' ').next().unwrap().parse::<usize>().unwrap()
    }).sum();
    assert!(cc.contains(&format!("▶ {per} runs")), "{cc}");
    // V1.5: Claude Code's runs are now the DAEMON's roster (`threads/non-idle`,
    // spec step 5), not the cauldron's own ripple runs, so they no longer
    // partition the cauldron's strip; the endpoints' still count the cauldron's.
    let or = s.lines().find(|l| l.contains("▾ Inference endpoints")).unwrap();
    let orn: usize = or.split("▶ ").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
    // V1-CLEAN1 (V1.5-I F4): the partition, restated on the axis it still
    // holds on. Every running run of the cauldron is on an endpoint or on a
    // Claude Code account; the endpoints' band counts the first, the
    // census's Claude Code runs are the rest, and the two add up to the
    // strip exactly. (Claude Code's band itself counts the daemon's roster
    // since V1.5 — R2, held — so it is not a term here.)
    let cc_accounts = ["sam-broomstick", "acct2", "acct3"];
    let on_cc = c.iter().filter(|e| e.state == RippleState::Running && e.account.as_deref().is_some_and(|x| cc_accounts.contains(&x))).count();
    assert!(on_cc > 0 && orn > 0, "sentinel: the seed runs work on both sides ({on_cc} on Claude Code, {orn} on endpoints)");
    assert_eq!(orn + on_cc, r, "the endpoints' runs and the Claude Code runs add up to the strip");
    let jev = s.lines().find(|l| l.contains("• jev ")).unwrap();
    assert!(jev.contains("$12.40 of $100 / month"), "{jev}");
}

#[test]
fn a_down_threads_link_is_a_dash_never_a_zero() {
    let (mut a, _r) = open();
    a.dash.threads.link = "offline".into();
    // V1.5: running and parked are known only while the face's ONE connection
    // is up (`status::threads_live`), not by the seed's `"link"`.
    a.live.store.link = tui_render::LinkState::Disconnected { reason: "the daemon went away".into(), retry_at: None };
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Bus);
    let s = screen(&mut a);
    let ex = s.lines().find(|l| l.contains("▾ Executions")).unwrap();
    let (_, _, pe) = counts(&a);
    assert!(ex.contains("▶ — running · ◆ — parked") && ex.contains(&format!("◇ {pe} pending")), "{ex}");
    a.set_sttab(StatusTab::Mesh);
    assert!(screen(&mut a).lines().any(|l| l.contains("▸ threads daemon") && l.contains("● offline")));
}

#[test]
fn a_cluster_counts_gate_refused_only_and_its_header_is_its_name() {
    let (mut a, _r) = open();
    a.set_ktab(KTab::Status);
    let s = screen(&mut a);
    let (r, p, pe) = cluster_counts(&a, a.selected, false);
    let rip = s.lines().find(|l| l.contains("▾ Ripples")).unwrap();
    assert!(rip.contains(&format!("▶ {r} running · ◆ {p} parked · ◇ {pe} pending")), "{rip}");
    assert_eq!(p, 1, "T-8, which the gate refused; not T-5, which Q-2 blocks");
    // N7 (msg 235: "there's no useful information there anymore"): no
    // header above the cluster's tabs — the strip is the page's first line.
    let strip = s.lines().nth(3).unwrap();
    assert!(strip.split('│').nth(2).unwrap().trim_start().starts_with("Questions "), "{strip}");
}

#[test]
fn from_status_a_parked_row_opens_its_run_and_space_flips_an_account() {
    let (mut a, root) = open();
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Bus);
    let v = status::view(&a, 120);
    a.rows[0] = v.items.iter().position(|i| i.key == "parked:0").unwrap();
    assert_eq!(a.selected_tag().unwrap().render(), "[T-8]");
    a.handle(key(KeyCode::Enter));
    assert_eq!((a.ctab, a.ktab), (CTab::Cauldron, KTab::Status));
    assert_eq!(a.items()[a.item], Item::Ripple { cluster: a.forest.find("X-1").unwrap(), serial: 8 });
    // Space on an account's band flips it, as on Config.
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Tokens);
    a.rows[0] = status::view(&a, 120).items.iter().position(|i| i.key == "acct:claude_code/acct2").unwrap();
    assert!(a.account_on("claude_code", "acct2"));
    a.handle(key(KeyCode::Char(' ')));
    assert!(!a.account_on("claude_code", "acct2"));
    let s = render_to_text(&mut a, 120, 80).unwrap().join("\n");
    assert!(s.lines().find(|l| l.contains("acct2")).unwrap().contains("DISABLED"), "{s}");
    // An account is global (msg 186): its switch is in the global file.
    let settings = std::fs::read_to_string(root.join("data/shared/tui/settings.toml")).unwrap();
    assert!(settings.contains("[pools.claude_code.accounts.acct2]"), "{settings}");
}

/// A path under the home directory, written from `~`, as Config writes it.
fn super_tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}
