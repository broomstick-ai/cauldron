//! Task 0250 (8e4fca10 msg 235, the 0225 re-audit). Each test names the 0225
//! finding it proves closed, on scratch roots.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, Focus};
use tui::cauldron::store::{merge3_unmarked, seed_dir, DraftStore};

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0250-{}-{}-{}",
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

fn open_env(root: &Path, name: &str) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(root, name), seed_dir(), true).expect("opens")
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn into_bucket(a: &mut App) {
    if a.focus != Focus::Input {
        a.handle(key(KeyCode::Char('a')));
        assert_eq!(a.focus, Focus::Input);
    }
    a.handle(key(KeyCode::End));
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(key(KeyCode::Char(c)));
    }
}

fn click(a: &mut App, x: u16, y: u16) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
    }
}

/// An older face's write: no `merges` mark.
fn old(text: &str) -> String {
    format!("{{\"nonce\":\"{}\",\"writer\":\"tui --cauldron pid 2\",\"updated_at\":\"x\",\"text\":{}}}", uuid::Uuid::new_v4(), serde_json::to_string(text).unwrap())
}

fn bucket_text(p: &Path) -> String {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    v["text"].as_str().unwrap().to_string()
}

fn count(text: &str, word: &str) -> usize {
    text.split_whitespace().filter(|w| *w == word).count()
}

// ── F2: a stale write from an older face never doubles text ───────────────

#[test]
fn f2_a_stale_write_is_merged_from_the_bucket_it_was_typed_on() {
    // The auditor's mechanism: the stale write's text is an older bucket plus
    // its word, so against the base it "replaces" `T2 B1` with `B1 B2`. From
    // the bucket it was typed on, only B2 is its own (0256: since G1, the
    // base is found, not the words guessed).
    let seen = vec!["base T1 B1".to_string(), "base T1 T2 B1".to_string()];
    assert_eq!(merge3_unmarked(&seen, None, "base T1 T2 B1", &all_own("base T1 T2 B1"), "base T1 T2 B1", "base T1 B1 B2", 0).0, "base T1 T2 B1 B2");
    // Words it never saw stay; what it typed comes in whole, after them.
    let seen = vec!["shared start".to_string(), "shared start new words".to_string()];
    assert_eq!(merge3_unmarked(&seen, None, "shared start new words", &all_own("shared start new words"), "shared start new words", "shared start old words", 0).0, "shared start new words old words");
    // Mine typed on unsaved: kept, and theirs comes in beside it.
    let seen = vec!["a b".to_string()];
    assert_eq!(merge3_unmarked(&seen, None, "a b", &all_own("a b"), "a b c", "a x b", 5), ("a x b c".to_string(), 7));
    // An insert at the caret goes after it (mine first).
    assert_eq!(merge3_unmarked(&[], None, "a", &all_own("a"), "a", "a z", 1), ("a z".to_string(), 1));
    // A fresh deletion applies (0252 G2): the older face took the words out.
    assert_eq!(merge3_unmarked(&[], None, "keep me please", &all_own("keep me please"), "keep me please", "keep", 0).0, "keep");
    // A clear after a send applies too, to what it sent.
    assert_eq!(merge3_unmarked(&[], None, "one two", &all_own("one two"), "one two", "", 0).0, "");
    // Multibyte words; a newline it typed comes with its word.
    assert_eq!(merge3_unmarked(&[], None, "é à", &all_own("é à"), "é à", "é à\n\nü", 0).0, "é à\n\nü");
    // Into an empty bucket: its words whole.
    assert_eq!(merge3_unmarked(&[], None, "", &all_own(""), "", "hello there", 0).0, "hello there");
    assert_eq!(merge3_unmarked(&[], None, "x", &all_own("x"), "y x", "z x", 0).0, "y z x");
    // It took out text this face typed while typing its own, and no recent
    // bucket explains it: only what it typed comes in, nothing goes.
    let (m, _) = merge3_unmarked(&[], None, "shared start new words", &all_own("shared start new words"), "shared start new words", "shared start old words", 0);
    assert_eq!(m, "shared start newold words");
}

/// Idempotent against a stale writer: the same text written again (a
/// keystroke that changed nothing in it) changes nothing, and no word comes
/// out more often than the two sides typed it.
#[test]
fn f2_the_same_stale_write_merged_twice_changes_nothing() {
    // The last two: theirs is no recent bucket plus an insertion, so only
    // its insertions come in; idempotent all the same.
    let cases = [
        ("base T1 B1", "base T1 T2 B1 extra", "base T1 B1 B2", true),
        ("", "mine only", "theirs only", true),
        ("p q r", "q r s", "p q r t p q", true),
        ("a b c d e", "a b X c d e", "a c e f", false),
        ("one\ntwo\nthree", "one\ntwo\nthree four", "zero one\nthree", false),
    ];
    for (snap, mine, theirs, stale) in cases {
        let seen = vec![snap.to_string(), mine.to_string()];
        let (m1, _) = merge3_unmarked(&seen, None, mine, &all_own(mine), mine, theirs, 0);
        let seen = vec![snap.to_string(), mine.to_string(), m1.clone()];
        let (m2, _) = merge3_unmarked(&seen, Some(theirs), &m1, &all_own(&m1), &m1, theirs, 0);
        assert_eq!(m1, m2, "snap {snap:?} mine {mine:?} theirs {theirs:?}");
        for w in m2.split_whitespace().filter(|w| count(mine, w) + count(theirs, w) > 0) {
            let most = count(mine, w).max(count(snap, w)) + count(theirs, w).saturating_sub(count(snap, w));
            assert!(count(&m2, w) <= most, "{w} {} times in {m2:?}", count(&m2, w));
        }
        // Character-exact: what each typed is there, whole, if not as a
        // word of its own (two faces typing at one spot at once abut).
        if !stale {
            // No bucket explains it: insertions only, every one of mine kept.
            for w in mine.split_whitespace() {
                assert!(m1.contains(w), "lost mine's {w} in {m1:?}");
            }
            continue;
        }
        for w in mine.split_whitespace().filter(|w| count(snap, w) == 0 || count(theirs, w) > 0) {
            assert!(m1.contains(w), "lost mine's {w} in {m1:?}");
        }
        for w in theirs.split_whitespace().filter(|w| count(snap, w) == 0) {
            assert!(m1.contains(w), "lost theirs' {w} in {m1:?}");
        }
    }
}

/// The auditor's dupe.sh, at the face, four rounds: snapshot the bucket,
/// type ` Tn`, then write the snapshot plus ` Bn` as an older face would
/// (unmarked, stale). Every word once — 0225 measured B1 at 1, 2, 4.
#[test]
fn f2_dupe_rounds_at_the_face_keep_every_word_once() {
    let root = scratch_root();
    let path = root.join("data/prod/tui/soup/bucket.json");
    let mut a = open_env(&root, "prod");
    into_bucket(&mut a);
    a.composer = tui_render::compose::Composer::new();
    typed(&mut a, " base");
    a.sync();
    for r in 1..=4 {
        let snap = bucket_text(&path);
        typed(&mut a, &format!(" T{r}"));
        a.sync();
        std::fs::write(&path, old(&format!("{snap} B{r}"))).unwrap();
        a.sync();
        a.sync();
        let t = a.composer.text();
        for w in ["base"].into_iter().map(String::from).chain((1..=r).flat_map(|i| [format!("T{i}"), format!("B{i}")])) {
            // Once each (0256: character-exact, so the two faces' tokens typed
            // at one spot may abut, `T1B1`).
            assert_eq!(t.matches(w.as_str()).count(), 1, "round {r}: {w} in {t:?}");
        }
        assert_eq!(bucket_text(&path), t, "the file holds what the face shows");
    }
}

/// Compound: two new faces typing, an older face writing stale snapshots
/// between them, many rounds. No word is ever written twice, none is lost.
#[test]
fn f2_two_faces_and_a_stale_writer_never_double_or_lose() {
    let root = scratch_root();
    let path = root.join("data/prod/tui/soup/bucket.json");
    let mut a = open_env(&root, "prod");
    let mut b = open_env(&root, "prod");
    into_bucket(&mut a);
    into_bucket(&mut b);
    a.composer = tui_render::compose::Composer::new();
    typed(&mut a, "start");
    a.sync();
    b.sync();
    let mut words = vec!["start".to_string()];
    let mut snaps: Vec<String> = Vec::new();
    for r in 0..12 {
        snaps.push(bucket_text(&path));
        let (wa, wb, wo) = (format!("a{r}"), format!("b{r}"), format!("o{r}"));
        a.handle(key(KeyCode::End));
        typed(&mut a, &format!(" {wa}"));
        b.handle(key(KeyCode::End));
        typed(&mut b, &format!(" {wb}"));
        if r % 2 == 0 {
            a.sync();
        }
        // The older face writes from a snapshot one, two or three rounds old.
        let snap = &snaps[snaps.len().saturating_sub(1 + r % 3)];
        std::fs::write(&path, old(&format!("{snap} {wo}"))).unwrap();
        b.sync();
        a.sync();
        words.extend([wa, wb, wo]);
    }
    for _ in 0..3 {
        a.sync();
        b.sync();
    }
    let (x, y) = (a.composer.text(), b.composer.text());
    assert_eq!(x, y, "the faces converge");
    for w in &words {
        assert_eq!(count(&x, w), 1, "{w} in {x:?}");
    }
    assert_eq!(x.split_whitespace().count(), words.len(), "nothing extra in {x:?}");
}

/// A store-level stale writer that keeps writing the same old text: the
/// bucket settles and stops changing.
#[test]
fn f2_a_stale_writer_repeating_itself_settles() {
    let root = scratch_root();
    let dir = root.join("soup");
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = DraftStore::new(&dir, true);
    let mut text = s.load().unwrap();
    for w in ["one", " two", " three"] {
        text.push_str(w);
        let at = text.len();
        if let Some((t, _)) = s.save(&text, at).unwrap() {
            text = t;
        }
    }
    let mut seen = Vec::new();
    for _ in 0..5 {
        std::fs::write(dir.join("bucket.json"), old("one stale")).unwrap();
        let at = text.len();
        if let Some((t, _)) = s.poll(&text, at).unwrap() {
            text = t;
        }
        seen.push(text.clone());
    }
    assert_eq!(text, "one two three stale");
    assert!(seen.iter().all(|t| *t == text), "{seen:?}");
}

// ── F3 (V9): the bucket's view follows the caret, never pins it ────────────

#[test]
fn f3_a_click_in_the_bucket_leaves_the_text_still() {
    let root = scratch_root();
    let mut a = open_env(&root, "prod");
    into_bucket(&mut a);
    let text: String = (1..=40).map(|i| format!("line number {i:02}")).collect::<Vec<_>>().join("\n");
    a.composer = tui_render::compose::Composer::from_text(&text);
    let screen = tui::cauldron::render_to_text(&mut a, 140, 40).unwrap();
    let (r, top) = a.input_at;
    let rows = r.height as usize;
    assert!(rows >= 3 && rows < 40, "the bucket scrolls: {r:?}");
    assert_eq!(top, 40 - rows, "the caret at the end shows the last rows");
    // Click every visible line in turn: the view never moves, and the
    // clicked line is still on the row that was clicked.
    for i in 0..rows {
        click(&mut a, r.x + 2, r.y + i as u16);
        let now = tui::cauldron::render_to_text(&mut a, 140, 40).unwrap();
        assert_eq!(a.input_at.1, top, "a click on visible row {i} scrolled the bucket");
        let want = format!("line number {:02}", top + i + 1);
        assert!(now[(r.y as usize) + i].contains(&want), "row {i}: {:?}", now[(r.y as usize) + i]);
        for k in 0..rows {
            assert_eq!(now[r.y as usize + k], screen[r.y as usize + k], "after a click on row {i}, row {k} changed");
        }
    }
    // Typing on a clicked middle line edits it in place.
    click(&mut a, r.x, r.y + 1);
    typed(&mut a, "X");
    tui::cauldron::render_to_text(&mut a, 140, 40).unwrap();
    assert_eq!(a.input_at.1, top);
    assert!(a.composer.text().contains(&format!("Xline number {:02}", top + 2)));
}

#[test]
fn f3_motion_scrolls_only_when_the_caret_leaves_the_box() {
    let root = scratch_root();
    let mut a = open_env(&root, "prod");
    into_bucket(&mut a);
    let text: String = (1..=40).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
    a.composer = tui_render::compose::Composer::from_text(&text);
    tui::cauldron::render_to_text(&mut a, 140, 40).unwrap();
    let (r, top) = a.input_at;
    let rows = r.height as usize;
    // Up to the top visible row: no scroll.
    for _ in 0..rows - 1 {
        a.handle(key(KeyCode::Up));
        tui::cauldron::render_to_text(&mut a, 140, 40).unwrap();
        assert_eq!(a.input_at.1, top);
    }
    // One more: one row, no more.
    a.handle(key(KeyCode::Up));
    tui::cauldron::render_to_text(&mut a, 140, 40).unwrap();
    assert_eq!(a.input_at.1, top - 1);
    // Back down inside the box: still.
    a.handle(key(KeyCode::Down));
    tui::cauldron::render_to_text(&mut a, 140, 40).unwrap();
    assert_eq!(a.input_at.1, top - 1);
    // Text taken away below: the box stays full, the caret in it.
    a.composer = tui_render::compose::Composer::from_text("short\ntext");
    tui::cauldron::render_to_text(&mut a, 140, 40).unwrap();
    assert_eq!(a.input_at.1, 0);
}

// ── F1 (code half): a choice equal to the default is kept as a choice ──────

#[test]
fn f1_a_value_set_to_its_default_stays_in_his_file() {
    use tui::cauldron::menu::Bound;
    use tui::cauldron::settings::{Ctx, Val};
    let root = scratch_root();
    let mut a = open_env(&root, "prod");
    let own = root.join("data/prod/tui/soup/settings.toml");
    let shared = root.join("data/shared/tui/settings.toml");
    let enabled = Bound::new("cauldron.enabled", Ctx::default());
    let auto = Bound::new("supervisor.auto_restart", Ctx::default());
    // Both equal to their defaults, chosen explicitly.
    a.sset(&enabled, Val::Bool(true)).unwrap();
    a.sset(&auto, Val::Bool(false)).unwrap();
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    assert!(read(&own).contains("enabled = true"), "{}", read(&own));
    assert!(read(&shared).contains("auto_restart = false"), "{}", read(&shared));
    // Another face's saves of other settings keep both.
    let mut b = open_env(&root, "prod");
    b.sset(&Bound::new("cauldron.opportunistic", Ctx::default()), Val::Bool(false)).unwrap();
    b.sset(&Bound::new("ui.theme", Ctx::default()), Val::Str("light".into())).unwrap();
    a.sset(&Bound::new("ui.theme", Ctx::default()), Val::Str("dark".into())).unwrap();
    assert!(read(&own).contains("enabled = true") && read(&own).contains("opportunistic = false"), "{}", read(&own));
    assert!(read(&shared).contains("auto_restart = false") && read(&shared).contains("theme = \"dark\""), "{}", read(&shared));
    let c = open_env(&root, "prod");
    assert!(c.sis_set(&enabled) && c.sis_set(&auto), "read back as his own choices");
    // Only ↺ / x takes a value back out.
    let mut c = c;
    c.sclear(&enabled).unwrap();
    assert!(!read(&own).contains("enabled = true"), "{}", read(&own));
}

// ── F5: an older face typing on in an old bucket file stays on its line ────

#[test]
fn f5_an_older_faces_appends_come_in_on_one_line() {
    let root = scratch_root();
    let dir = root.join("data/prod/tui/soup");
    let mut a = open_env(&root, "prod");
    into_bucket(&mut a);
    a.composer = tui_render::compose::Composer::new();
    typed(&mut a, "mine");
    a.sync();
    std::fs::create_dir_all(dir.join("drafts")).unwrap();
    let draft = |t: &str| format!("{{\"nonce\":\"{}\",\"writer\":\"w\",\"updated_at\":\"2026-10-25T05:41:44+00:00\",\"text\":{}}}", uuid::Uuid::new_v4(), serde_json::to_string(t).unwrap());
    let mut sofar = String::from("L1");
    std::fs::write(dir.join("drafts/cauldron.json"), draft(&sofar)).unwrap();
    a.sync();
    assert_eq!(a.composer.text(), "mine\n\nL1");
    for w in [" L2", " L3", "x"] {
        sofar.push_str(w);
        std::fs::write(dir.join("drafts/cauldron.json"), draft(&sofar)).unwrap();
        a.sync();
    }
    assert_eq!(a.composer.text(), "mine\n\nL1 L2 L3x");
    // Once the bucket has moved on past it, what it types is a paragraph.
    for _ in 0..3 {
        a.handle(key(KeyCode::Down));
    }
    a.handle(key(KeyCode::End));
    typed(&mut a, " later");
    sofar.push_str(" L4");
    std::fs::write(dir.join("drafts/cauldron.json"), draft(&sofar)).unwrap();
    a.sync();
    assert_eq!(a.composer.text(), "mine\n\nL1 L2 L3x later\n\nL4");
}

/// A word this face has typed onto since (`start` → `b0start`), still whole
/// in an older face's stale text: it is not that face's, and never comes in
/// again, however often it writes.
#[test]
fn f2_a_word_typed_onto_here_is_not_brought_back_by_a_stale_write() {
    let root = scratch_root();
    let dir = root.join("soup");
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = DraftStore::new(&dir, true);
    let _ = s.load().unwrap();
    let mut text = String::from("start");
    if let Some((t, _)) = s.save(&text, 5).unwrap() {
        text = t;
    }
    let snap = text.clone();
    for (i, tok) in ["b0", " b1", " b2"].iter().enumerate() {
        // The first typed onto `start`; the rest at the end.
        let at = if i == 0 { 0 } else { text.len() };
        text.insert_str(at, tok);
        if let Some((t, _)) = s.save(&text, at + tok.len()).unwrap() {
            text = t;
        }
        std::fs::write(dir.join("bucket.json"), old(&format!("{snap} o{i}"))).unwrap();
        if let Some((t, _)) = s.poll(&text, 0).unwrap() {
            text = t;
        }
    }
    assert_eq!(count(&text, "start"), 0, "{text:?}");
    assert_eq!(text.matches("start").count(), 1, "{text:?}");
    for o in ["o0", "o1", "o2"] {
        assert_eq!(count(&text, o), 1, "{text:?}");
    }
}

/// Every character of `base` typed by this face.
fn all_own(base: &str) -> Vec<bool> {
    vec![true; base.chars().count()]
}
