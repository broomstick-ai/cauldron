//! Task 0223 (8e4fca10 msg 235, the 0220 re-audit). Each test names the 0220
//! finding it proves closed, on scratch roots.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tui::cauldron::app::{App, Focus};
use tui::cauldron::store::{merge3, seed_dir, DraftStore};

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0223-{}-{}-{}",
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

fn missing(text: &str, tokens: &[String]) -> Vec<String> {
    tokens.iter().filter(|t| !text.contains(t.as_str())).cloned().collect()
}

// ── E2: two faces typing into one bucket lose nothing ──────────────────────

#[test]
fn e2_merge3_keeps_both_sides() {
    // Both insert at the end: mine first, the caret still after it, so the
    // next letter typed here goes on with mine and not into theirs.
    assert_eq!(merge3("x", "xa", "xb", 2), ("xab".to_string(), 2));
    // The same letter typed in both faces at once: both letters stay.
    assert_eq!(merge3("", " ", " ", 1), ("  ".to_string(), 1));
    // Different places: both apply; the caret stays after mine.
    assert_eq!(merge3("one two", "one! two", "one two?", 4), ("one! two?".to_string(), 4));
    assert_eq!(merge3("one two", "one two?", "one! two", 8), ("one! two?".to_string(), 9));
    // Only theirs changed: theirs, caret moved past its insert.
    assert_eq!(merge3("ab", "ab", "Xab", 2), ("Xab".to_string(), 3));
    // Only mine changed.
    assert_eq!(merge3("ab", "abc", "ab", 3), ("abc".to_string(), 3));
    // Overlapping, different edits: every letter either side typed, once
    // (0256: a base letter either side deleted is deleted).
    let (m, _) = merge3("the cat sat", "the dog sat", "the cow sat", 7);
    assert!(m.contains("dog") && m.contains("ow"), "{m}");
    // One deletes what the other leaves be: the deletion applies.
    assert_eq!(merge3("a b c", "a c", "a b c d", 2).0, "a c d");
    // Multibyte text never splits a char.
    let (m, at) = merge3("é", "éà", "éü", 4);
    assert_eq!(m, "éàü");
    assert!(m.is_char_boundary(at));
}

/// The auditor's own measure (0220 E2): two faces, each typing 20 tokens,
/// alternating 30 ms and then 150 ms apart. Real concurrency: two threads,
/// each its own store on one bucket, each polling as the live loop does and
/// saving on every keystroke. Nothing either typed is lost.
#[test]
fn e2_two_faces_bursting_concurrently_lose_nothing() {
    for gap in [30u64, 150, 0] {
        let root = scratch_root();
        let dir = root.join("soup");
        std::fs::create_dir_all(&dir).unwrap();
        let run = |who: &'static str, dir: PathBuf| {
            std::thread::spawn(move || {
                let mut s = DraftStore::new(&dir, true);
                let mut text = s.load().unwrap();
                // The face's caret, as the face keeps it: typing goes there.
                let mut at = text.len();
                for i in 0..20 {
                    if let Some((t, c)) = s.poll(&text, at).unwrap() {
                        (text, at) = (t, c);
                    }
                    let tok = format!(" {who}{i:02}");
                    for c in tok.chars() {
                        text.insert(at, c);
                        at += c.len_utf8();
                        if let Some((t, c)) = s.save(&text, at).unwrap() {
                            (text, at) = (t, c);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(gap));
                }
                let _ = s.poll(&text, at).unwrap();
            })
        };
        let a = run("alpha", dir.clone());
        let b = run("bravo", dir.clone());
        a.join().unwrap();
        b.join().unwrap();
        let mut s = DraftStore::new(&dir, true);
        let text = s.load().unwrap();
        let toks: Vec<String> = (0..20).flat_map(|i| [format!("alpha{i:02}"), format!("bravo{i:02}")]).collect();
        assert!(missing(&text, &toks).is_empty(), "gap {gap} ms lost {:?} from {text:?}", missing(&text, &toks));
        // Every letter typed is there (40 spaces too), and each face's run
        // is whole, in its own order. (Which face's space a space is cannot
        // be told when both typed one at one spot, so the run is read
        // without them.)
        assert_eq!(text.chars().filter(|c| *c == ' ').count(), 40, "{text:?}");
        let bare = text.replace(' ', "");
        for who in ["alpha", "bravo"] {
            let run: String = (0..20).map(|i| format!("{who}{i:02}")).collect();
            assert!(bare.contains(&run), "gap {gap} ms: {who}'s words not in one piece in {text:?}");
        }
    }
}

/// The same at the face: two `App`s on one cauldron, each typing a token,
/// neither syncing before the other types (the window E2 lost whole bursts
/// in), then both syncing on their ticks. Both end with all 40 tokens.
#[test]
fn e2_two_faces_typing_without_syncing_lose_nothing() {
    for gap in [30u64, 150] {
        let root = scratch_root();
        let mut a = open_env(&root, "prod");
        let mut b = open_env(&root, "prod");
        into_bucket(&mut a);
        into_bucket(&mut b);
        let mut toks = Vec::new();
        for i in 0..20 {
            let (ta, tb) = (format!("aa{i:02}"), format!("bb{i:02}"));
            typed(&mut a, &format!(" {ta}"));
            typed(&mut b, &format!(" {tb}"));
            toks.push(ta);
            toks.push(tb);
            std::thread::sleep(Duration::from_millis(gap / 4));
            if i % 3 == 0 {
                a.sync();
            }
            if i % 4 == 1 {
                b.sync();
            }
        }
        for _ in 0..2 {
            a.sync();
            b.sync();
        }
        let (x, y) = (a.composer.text(), b.composer.text());
        assert!(missing(&x, &toks).is_empty(), "face a lost {:?}", missing(&x, &toks));
        assert_eq!(x, y, "the two faces converge");
        let on_disk = std::fs::read_to_string(root.join("data/prod/tui/soup/bucket.json")).unwrap();
        assert!(missing(&on_disk, &toks).is_empty(), "the file lost {:?}", missing(&on_disk, &toks));
    }
}

/// One face sends the bucket while the other has typed into it unseen: the
/// sent text leaves, the unseen words stay.
#[test]
fn e2_a_send_keeps_what_the_other_face_typed_since() {
    let root = scratch_root();
    let mut a = open_env(&root, "prod");
    let mut b = open_env(&root, "prod");
    into_bucket(&mut a);
    typed(&mut a, "shared words");
    b.sync();
    into_bucket(&mut b);
    typed(&mut b, " and b's late ones");
    a.handle(key(KeyCode::Enter));
    a.sync();
    b.sync();
    assert!(a.composer.text().contains("b's late ones"), "{:?}", a.composer.text());
    assert_eq!(a.composer.text(), b.composer.text());
}

// ── E3: a save keeps his comments; an unknown key never locks the face ─────

#[test]
fn e3_a_theme_toggle_keeps_every_comment_and_an_unknown_key_opens_the_face() {
    use tui::cauldron::settings::{def, Ctx, Val};
    let root = scratch_root();
    let shared = root.join("data/shared/tui");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::create_dir_all(root.join("data/prod")).unwrap();
    let text = "# his own notes\n\n# above the table\n[pools.claude_code.accounts.acct2]\nshare = 30 # half of acct1\n\n[zzz_unknown]\nmine = 1\n";
    std::fs::write(shared.join("settings.toml"), text).unwrap();
    let mut a = open_env(&root, "prod");
    assert!(a.notice.as_deref().is_some_and(|n| n.contains("zzz_unknown.mine") && n.contains("kept")), "{:?}", a.notice);
    a.settings.set(def("ui.theme"), &Ctx::default(), Val::Str("light".into())).unwrap();
    let out = std::fs::read_to_string(shared.join("settings.toml")).unwrap();
    for line in ["# his own notes", "# above the table", "share = 30 # half of acct1", "[zzz_unknown]", "mine = 1"] {
        assert!(out.contains(line), "lost `{line}`:\n{out}");
    }
    assert!(out.contains("theme = \"light\""), "{out}");
    // And a second face reads it.
    let b = open_env(&root, "prod");
    assert_eq!(b.settings.get(def("ui.theme"), &Ctx::default()).val, Val::Str("light".into()));
}


// ── E5: no human-action step asks for a paste the face does not take ───────

#[test]
fn e5_no_seed_step_says_paste_it_here() {
    let mut files = vec![seed_dir().join("clusters.json")];
    files.extend(std::fs::read_dir(seed_dir().join("soup")).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")));
    for p in files {
        let t = std::fs::read_to_string(&p).unwrap().to_lowercase();
        for bad in ["paste it here", "paste its url", "paste them here", "paste the stripe", "anon key here"] {
            assert!(!t.contains(bad), "{} still says `{bad}` over a field that is not taken here", p.display());
        }
    }
}

// ── E6: a click in the bucket's text places the caret ──────────────────────

#[test]
fn e6_a_click_in_the_bucket_puts_the_caret_there() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let root = scratch_root();
    let mut a = open_env(&root, "prod");
    into_bucket(&mut a);
    a.composer = tui_render::compose::Composer::from_text("first line\nsecond line\nthird line");
    tui::cauldron::render_to_text(&mut a, 140, 40).unwrap();
    let (r, top) = a.input_at;
    assert!(r.width > 10 && r.height >= 3, "{r:?}");
    // Line 2, before `line`: `second ` is 7 columns.
    let (x, y) = (r.x + 7, r.y + (1 - top as u16));
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
    }
    typed(&mut a, "X ");
    assert_eq!(a.composer.text(), "first line\nsecond X line\nthird line");
    // A click past a line's end lands at its end.
    tui::cauldron::render_to_text(&mut a, 140, 40).unwrap();
    let (r, top) = a.input_at;
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: r.x + 60, row: r.y + (0 - top as u16), modifiers: KeyModifiers::NONE }));
    }
    typed(&mut a, "!");
    assert_eq!(a.composer.text(), "first line!\nsecond X line\nthird line");
}

// ── E7: a bucket an older face wrote with slugs reads as ids ───────────────

#[test]
fn e7_the_dev_buckets_slugs_are_read_as_ids_on_open_and_live() {
    let root = scratch_root();
    let soup = root.join("data/dev/tui/soup");
    std::fs::create_dir_all(&soup).unwrap();
    let old = |text: &str| format!("{{\"nonce\":\"{}\",\"writer\":\"tui --cauldron pid 8214\",\"updated_at\":\"x\",\"text\":\"{text}\"}}", uuid::Uuid::new_v4());
    std::fs::write(soup.join("bucket.json"), old("[scratch/Q-1] [scratch/Q-1] ")).unwrap();
    let mut a = open_env(&root, "dev");
    assert_eq!(a.composer.text(), "[Q-1] [Q-1] ");
    // Opening writes nothing.
    assert!(std::fs::read_to_string(soup.join("bucket.json")).unwrap().contains("scratch/Q-1"));
    // The older face writes more, live: it comes in as ids.
    std::fs::write(soup.join("bucket.json"), old("[Q-1] [Q-1] [scratch-ripples/Q-1] [surface:sandbox]")).unwrap();
    a.sync();
    // (Its own trailing space stays: an older face's change of whitespace
    // alone is not taken in, 0250.)
    assert_eq!(a.composer.text().trim_end(), "[Q-1] [Q-1] [Q-2] [X-1]");
}

// ── E8: a service's version and address are not settings ──────────────────

#[test]
fn e8_a_file_holding_a_service_reading_is_kept_and_not_used() {
    let s = tui::cauldron::settings::Store::parse("[services.threads]\nversion = \"pinned\"\naddr = \"127.0.0.1:1\"\n", "t").unwrap();
    assert!(s.vals.is_empty() && s.warnings.is_empty(), "retired, not unknown: {:?}", s.warnings);
    assert_eq!(s.retired.len(), 2);
    assert!(s.to_text().unwrap().contains("version = \"pinned\""), "written back as he wrote it");
}

// ── E10: a letter typed in the bucket is text, whatever the layout ─────────

#[test]
fn e10_option_characters_typed_in_the_bucket_are_text() {
    let root = scratch_root();
    let mut a = open_env(&root, "prod");
    into_bucket(&mut a);
    a.composer = tui_render::compose::Composer::new();
    typed(&mut a, "£5 · å · § ≤ ≥ ∞ ¡ ™ ¢ ª");
    assert_eq!(a.composer.text(), "£5 · å · § ≤ ≥ ∞ ¡ ™ ¢ ª", "every character landed");
    // ⌥ the terminal reports still acts: ⌥A copies (nothing typed).
    a.handle(Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT)));
    assert_eq!(a.composer.text(), "£5 · å · § ≤ ≥ ∞ ¡ ™ ¢ ª");
}

// ── E11, E12: the dumps cap is named for the dumps; help says one click ────

#[test]
fn e11_the_sends_cap_is_the_dumps_cap_and_e12_help_says_what_one_click_does() {
    let s = tui::cauldron::settings::Store::parse("[storage.tui]\nsends_cap_mb = 70\n", "t").unwrap();
    assert_eq!(s.vals.get("storage.tui.dumps_cap_mb"), Some(&tui::cauldron::settings::Val::Int(70)), "the old name is read as the new");
    assert_eq!(tui::cauldron::settings::def("storage.tui.dumps_cap_mb").label, "dumps cap");
    let click = tui::cauldron::app::KEYMAP.iter().find(|(k, _)| *k == "click").unwrap().1;
    assert!(click.contains("acts at once") && !click.starts_with("select · click the selected thing again"), "{click}");
}

// ── E14: a dump an older face appends to sends.jsonl reaches the new face ──

#[test]
fn e14_an_older_faces_dump_in_sends_jsonl_comes_into_the_log_once() {
    let root = scratch_root();
    let mut a = open_env(&root, "prod");
    a.dump("mine first");
    let soup = root.join("data/prod/tui/soup");
    let n0 = a.sends.sends.len();
    let line = |seq: u64, text: &str| format!("{{\"seq\":{seq},\"at\":\"2026-10-27T00:00:00Z\",\"env\":\"prod\",\"from_cluster\":\"X-1\",\"text\":\"{text}\",\"tags\":[],\"targets\":[]}}\n");
    // The older face recreates sends.jsonl and appends (its own numbering).
    std::fs::write(soup.join("sends.jsonl"), line(1, "from the old face")).unwrap();
    a.sync();
    assert_eq!(a.sends.sends.len(), n0 + 1, "it came in");
    let last = a.sends.sends.last().unwrap();
    assert_eq!(last.text, "from the old face");
    assert!(last.seq > a.sends.sends[n0 - 1].seq, "numbered after the log's own");
    assert!(std::fs::read_to_string(soup.join("dumps.jsonl")).unwrap().contains("from the old face"), "in the log every reader reads");
    a.sync();
    assert_eq!(a.sends.sends.len(), n0 + 1, "once");
    // More, and a half-written line: the whole one comes, the half waits.
    use std::io::Write;
    let mut fh = std::fs::OpenOptions::new().append(true).open(soup.join("sends.jsonl")).unwrap();
    fh.write_all(line(2, "and again").as_bytes()).unwrap();
    fh.write_all(b"{\"seq\":3,\"at\"").unwrap();
    a.sync();
    assert_eq!(a.sends.sends.last().unwrap().text, "and again");
    assert_eq!(a.sends.sends.len(), n0 + 2);
    // sends.jsonl itself is never changed; a second face sees them too.
    assert!(std::fs::read_to_string(soup.join("sends.jsonl")).unwrap().starts_with(&line(1, "from the old face")));
    let b = open_env(&root, "prod");
    assert_eq!(b.sends.sends.len(), n0 + 2);
}

// ── adversarial pass: older faces and settings written by two faces ────────

/// An older face writes without the `merges` mark: words it never saw are
/// not taken out. A clear of what it sent applies, and so does a deletion.
#[test]
fn adv_an_older_faces_blind_write_never_drops_new_words_and_its_send_clears() {
    let root = scratch_root();
    let soup = root.join("data/prod/tui/soup");
    let mut a = open_env(&root, "prod");
    into_bucket(&mut a);
    a.composer = tui_render::compose::Composer::new();
    typed(&mut a, "shared start");
    let old = |text: &str| format!("{{\"nonce\":\"{}\",\"writer\":\"tui --cauldron pid 8214\",\"updated_at\":\"x\",\"text\":{}}}", uuid::Uuid::new_v4(), serde_json::to_string(text).unwrap());
    // a types on; the old face, which saw only `shared start`, writes its own.
    typed(&mut a, " new words");
    std::fs::write(soup.join("bucket.json"), old("shared start old words")).unwrap();
    a.sync();
    let t = a.composer.text();
    // 0256: merged from the bucket it typed on, character-exact; that bucket
    // may have been `shared start ` (this face saved on every keystroke), so
    // its words may abut this face's.
    assert!(t == "shared start new words old words" || t == "shared start new wordsold words", "both faces' words, each whole: {t:?}");
    // The old face sends everything it holds and clears: the clear applies
    // to what it sent (its dump is in the log), and nothing else goes.
    a.sync();
    let sent = a.composer.text();
    a.sends.append("prod", "X-1", &sent, Default::default(), |_| Vec::new()).unwrap();
    std::fs::write(soup.join("bucket.json"), old("")).unwrap();
    a.sync();
    assert_eq!(a.composer.text(), "", "a send's clear applies");
    // A hand deletion by the old face, fresh against this bucket, applies
    // (0252 G2: b7c1cb42 merges before it writes, so its deletion is his).
    typed(&mut a, "keep me please");
    a.sync();
    std::fs::write(soup.join("bucket.json"), old("keep")).unwrap();
    a.sync();
    assert_eq!(a.composer.text(), "keep");
}

/// Two faces each change a different setting before either re-reads: both
/// changes stand.
#[test]
fn adv_two_faces_setting_different_things_keep_both() {
    use tui::cauldron::settings::{def, Ctx, Val};
    let root = scratch_root();
    let mut a = open_env(&root, "prod");
    let mut b = open_env(&root, "prod");
    a.settings.set(def("ui.theme"), &Ctx::default(), Val::Str("light".into())).unwrap();
    b.settings.set(def("supervisor.auto_restart"), &Ctx::default(), Val::Bool(true)).unwrap();
    let c = open_env(&root, "prod");
    assert_eq!(c.settings.get(def("ui.theme"), &Ctx::default()).val, Val::Str("light".into()), "a's change survived b's save");
    assert_eq!(c.settings.get(def("supervisor.auto_restart"), &Ctx::default()).val, Val::Bool(true));
}
