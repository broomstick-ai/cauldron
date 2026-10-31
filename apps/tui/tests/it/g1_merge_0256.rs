//! Task 0256 (8e4fca10 msg 235, the 0252 re-audit). G1 first: an older
//! face's in-word edits must arrive as edits, never as new words. Proved by
//! a property test over random interleavings of a REAL older writer's
//! algorithm (b7c1cb42's, copied below byte for byte in what it does) and a
//! face of this build, and of two faces of this build, at 30 ms and 150 ms.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use tui::cauldron::store::{merge3, merge3_unmarked, DraftStore};

fn scratch() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0256-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

// ── b7c1cb42, the build his live face runs ─────────────────────────────────

/// b7c1cb42's `one_edit`, unchanged.
fn b7_one_edit<'a>(base: &str, new: &'a str) -> (usize, usize, &'a str) {
    let mut p = base.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    while !base.is_char_boundary(p) || !new.is_char_boundary(p) {
        p -= 1;
    }
    let room = base.len().min(new.len()) - p;
    let mut q = base.bytes().rev().zip(new.bytes().rev()).take(room).take_while(|(a, b)| a == b).count();
    while !base.is_char_boundary(base.len() - q) || !new.is_char_boundary(new.len() - q) {
        q -= 1;
    }
    (p, base.len() - q, &new[p..new.len() - q])
}

/// b7c1cb42's `merge3`, unchanged: one edit per side, overlap keeps both.
fn b7_merge3(base: &str, mine: &str, theirs: &str, caret: usize) -> (String, usize) {
    if mine == base {
        let (bs, be, tb) = b7_one_edit(base, theirs);
        let at = if caret <= bs { caret } else if caret >= be { caret - be + bs + tb.len() } else { bs + tb.len() };
        return (theirs.to_string(), at.min(theirs.len()));
    }
    if theirs == base {
        return (mine.to_string(), caret);
    }
    let (as_, ae, ta) = b7_one_edit(base, mine);
    let (bs, be, tb) = b7_one_edit(base, theirs);
    let in_mine = |p: usize| if p <= as_ { p } else { p - ae + as_ + ta.len() };
    let mut parts: Vec<(String, Option<usize>)> = Vec::new();
    if ae <= bs {
        parts.push((base[..as_].to_string(), Some(0)));
        parts.push((ta.to_string(), Some(as_)));
        parts.push((base[ae..bs].to_string(), Some(in_mine(ae))));
        parts.push((tb.to_string(), None));
        parts.push((base[be..].to_string(), Some(in_mine(be))));
    } else if be <= as_ {
        parts.push((base[..bs].to_string(), Some(0)));
        parts.push((tb.to_string(), None));
        parts.push((base[be..as_].to_string(), Some(be)));
        parts.push((ta.to_string(), Some(as_)));
        parts.push((base[ae..].to_string(), Some(in_mine(ae))));
    } else {
        let (us, ue) = (as_.min(bs), ae.max(be));
        let m = format!("{}{ta}{}", &base[us..as_], &base[ae..ue]);
        let t = format!("{}{tb}{}", &base[us..bs], &base[be..ue]);
        parts.push((base[..us].to_string(), Some(0)));
        let keep_t = !t.is_empty() && t != m;
        parts.push((m, Some(us)));
        if keep_t {
            parts.push((t, None));
        }
        parts.push((base[ue..].to_string(), Some(in_mine(ue))));
    }
    let mut out = String::new();
    let mut at = None;
    for (piece, from) in &parts {
        if let Some(m) = from {
            if at.is_none() && *m <= caret && caret <= m + piece.len() {
                at = Some(out.len() + (caret - m));
            }
        }
        out.push_str(piece);
    }
    let at = at.unwrap_or(out.len());
    let at = (0..=at.min(out.len())).rev().find(|i| out.is_char_boundary(*i)).unwrap_or(0);
    (out, at)
}

fn read_file(path: &Path) -> Option<(String, String)> {
    let s = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    Some((v["nonce"].as_str()?.to_string(), v["text"].as_str()?.to_string()))
}

/// An unmarked write, as every face before this build writes.
fn write_old(path: &Path, text: &str) -> String {
    let nonce = uuid::Uuid::new_v4().to_string();
    let body = format!("{{\"nonce\":\"{nonce}\",\"writer\":\"tui --cauldron pid 8269\",\"updated_at\":\"x\",\"text\":{}}}", serde_json::to_string(text).unwrap());
    let tmp = path.with_extension(format!("{nonce}.tmp"));
    std::fs::write(&tmp, body).unwrap();
    std::fs::rename(&tmp, path).unwrap();
    nonce
}

/// One face on the bucket: its text, its caret (bytes), and how it saves.
trait Face {
    fn text(&self) -> &str;
    fn caret(&self) -> usize;
    fn set(&mut self, text: String, caret: usize);
    /// Save after a keystroke (the face saves on every edit).
    fn save(&mut self);
    /// The quiet tick: take in another face's write.
    fn poll(&mut self);
}

/// b7c1cb42's `DraftStore` (save = read-merge-write under the lock, poll =
/// save when the file holds a nonce it did not write), writing unmarked.
struct B7 {
    path: PathBuf,
    last: Option<String>,
    base: String,
    text: String,
    caret: usize,
}

impl B7 {
    fn open(dir: &Path) -> B7 {
        let d = read_file(&dir.join("bucket.json"));
        let text = d.as_ref().map(|d| d.1.clone()).unwrap_or_default();
        B7 { path: dir.join("bucket.json"), last: d.map(|d| d.0), base: text.clone(), caret: text.len(), text }
    }
}

impl Face for B7 {
    fn text(&self) -> &str {
        &self.text
    }
    fn caret(&self) -> usize {
        self.caret
    }
    fn set(&mut self, text: String, caret: usize) {
        (self.text, self.caret) = (text, caret);
    }
    fn save(&mut self) {
        let disk = read_file(&self.path);
        let foreign = disk.as_ref().filter(|d| self.last.as_deref() != Some(d.0.as_str()));
        let out = match foreign {
            Some((nonce, dt)) => {
                let (merged, at) = b7_merge3(&self.base, &self.text, dt, self.caret);
                if merged == *dt {
                    self.last = Some(nonce.clone());
                    self.base = merged.clone();
                    (self.text, self.caret) = (merged, at);
                    return;
                }
                (self.text, self.caret) = (merged.clone(), at);
                merged
            }
            None if disk.as_ref().is_some_and(|d| d.1 == self.text) => return,
            None => self.text.clone(),
        };
        self.last = Some(write_old(&self.path, &out));
        self.base = out;
    }
    fn poll(&mut self) {
        let Some(d) = read_file(&self.path) else { return };
        if self.last.as_deref() != Some(d.0.as_str()) {
            self.save();
        }
    }
}

/// A face of this build.
struct New {
    store: DraftStore,
    text: String,
    caret: usize,
}

impl New {
    fn open(dir: &Path) -> New {
        let mut store = DraftStore::new(dir, true);
        let text = store.load().unwrap();
        New { store, caret: text.len(), text }
    }
}

impl Face for New {
    fn text(&self) -> &str {
        &self.text
    }
    fn caret(&self) -> usize {
        self.caret
    }
    fn set(&mut self, text: String, caret: usize) {
        (self.text, self.caret) = (text, caret);
    }
    fn save(&mut self) {
        if let Some((t, c)) = self.store.save(&self.text, self.caret).unwrap() {
            (self.text, self.caret) = (t, c);
        }
    }
    fn poll(&mut self) {
        if let Some((t, c)) = self.store.poll(&self.text, self.caret).unwrap() {
            (self.text, self.caret) = (t, c);
        }
    }
}

/// A face from before b7c1cb42: it adopts the file whole on its tick and
/// writes its own text blindly, so a write between two ticks is STALE.
struct Blind {
    path: PathBuf,
    last: Option<String>,
    text: String,
    caret: usize,
}

impl Face for Blind {
    fn text(&self) -> &str {
        &self.text
    }
    fn caret(&self) -> usize {
        self.caret
    }
    fn set(&mut self, text: String, caret: usize) {
        (self.text, self.caret) = (text, caret);
    }
    fn save(&mut self) {
        self.last = Some(write_old(&self.path, &self.text));
    }
    fn poll(&mut self) {
        let Some(d) = read_file(&self.path) else { return };
        if self.last.as_deref() != Some(d.0.as_str()) {
            self.last = Some(d.0);
            self.text = d.1;
            self.caret = self.caret.min(self.text.len());
            while !self.text.is_char_boundary(self.caret) {
                self.caret -= 1;
            }
        }
    }
}

// ── the property ───────────────────────────────────────────────────────────

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    New,
    B7,
    Blind,
}

fn open(kind: Kind, dir: &Path) -> Box<dyn Face> {
    match kind {
        Kind::New => Box::new(New::open(dir)),
        Kind::B7 => Box::new(B7::open(dir)),
        Kind::Blind => {
            let d = read_file(&dir.join("bucket.json"));
            let text = d.as_ref().map(|d| d.1.clone()).unwrap_or_default();
            Box::new(Blind { path: dir.join("bucket.json"), last: d.map(|d| d.0), caret: text.len(), text })
        }
    }
}

/// What one run did and ended with.
struct Run {
    finals: Vec<String>,
    file: String,
    typed: Vec<char>,
    deleted: HashSet<char>,
    /// Every face's text after every step, and which kind of face it is.
    snapshots: Vec<(Kind, String)>,
}

/// Every character typed is unique (a fresh code point each), so a lost or
/// doubled character is seen exactly. Each face types `keys` keystrokes,
/// `gap` ms apart (with jitter), and ticks every 20-120 ms; it saves on
/// every keystroke, as both builds do. A keystroke is a letter typed at the
/// caret (most often), a Backspace, a Delete, the caret moved somewhere at
/// random (the next letters are an in-word edit there), or to the end.
fn run(kinds: [Kind; 2], seed: u64, gap: u64, keys: usize) -> Run {
    let dir = scratch();
    let path = dir.join("bucket.json");
    // A bucket already holding text, so there is something to edit.
    let seed_text: String = "the quick brown fox ".to_string();
    write_old(&path, &seed_text);
    let mut faces: Vec<Box<dyn Face>> = kinds.iter().map(|k| open(*k, &dir)).collect();
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut next_char = 0x4E00u32;
    let mut typed = Vec::new();
    let mut deleted = HashSet::new();
    let mut snapshots = Vec::new();
    // (time, face, is_key)
    let mut due_key = [rng.below(gap + 1), rng.below(gap + 1)];
    let mut due_tick = [20 + rng.below(100), 20 + rng.below(100)];
    let mut left = [keys, keys];
    while left.iter().any(|n| *n > 0) {
        let (f, is_key) = (0..2)
            .flat_map(|f| [(due_key[f], f, true), (due_tick[f], f, false)])
            .filter(|&(_, f, k)| !k || left[f] > 0)
            .min()
            .map(|(_, f, k)| (f, k))
            .unwrap();
        let face = &mut faces[f];
        if is_key {
            let (mut t, mut c) = (face.text().to_string(), face.caret());
            match rng.below(20) {
                0..=11 => {
                    let ch = char::from_u32(next_char).unwrap();
                    next_char += 1;
                    typed.push(ch);
                    t.insert(c, ch);
                    c += ch.len_utf8();
                }
                12..=14 => {
                    if let Some(ch) = t[..c].chars().next_back() {
                        c -= ch.len_utf8();
                        t.remove(c);
                        deleted.insert(ch);
                    }
                }
                15 => {
                    if c < t.len() {
                        deleted.insert(t.remove(c));
                    }
                }
                16..=18 => {
                    let n = t.chars().count() as u64;
                    let k = rng.below(n + 1) as usize;
                    c = t.char_indices().nth(k).map_or(t.len(), |(i, _)| i);
                }
                _ => c = t.len(),
            }
            face.set(t, c);
            face.save();
            left[f] -= 1;
            due_key[f] += gap * 3 / 4 + rng.below(gap / 2 + 1);
        } else {
            face.poll();
            due_tick[f] += 20 + rng.below(100);
        }
        snapshots.push((kinds[f], faces[f].text().to_string()));
        if std::env::var("G1_TRACE").is_ok() {
            let now = read_file(&path).unwrap().1;
            let mut seen_c = HashSet::new();
            let dup = now.chars().filter(|c| *c as u32 >= 0x4E00).any(|c| !seen_c.insert(c));
            let back = now.chars().any(|c| c as u32 >= 0x4E00 && deleted.contains(&c));
            let gone: Vec<char> = typed.iter().filter(|c| !deleted.contains(c) && !now.contains(**c) && faces.iter().all(|f| !f.text().contains(**c)) ).copied().collect();
            eprintln!("step f{f}({:?}) key={is_key} file={now:?} face={:?} dup={dup} back={back} gone={gone:?}", kinds[f], faces[f].text());
            if dup || !gone.is_empty() {
                panic!("first fault");
            }
        }
    }
    // Then quiet: both tick until neither changes.
    for _ in 0..8 {
        for face in faces.iter_mut() {
            face.poll();
        }
    }
    let file = read_file(&path).unwrap().1;
    let finals = faces.iter().map(|f| f.text().to_string()).collect();
    let _ = std::fs::remove_dir_all(&dir);
    Run { finals, file, typed, deleted, snapshots }
}

/// No loss, no duplication, no reordering, and the faces converge. Returns
/// how many steps showed b7c1cb42's own doubling on ITS screen (its overlap
/// rule, before its next tick takes in the write that undoes it): never on a
/// face of this build, and never in what the bucket ends with.
fn check(r: &Run, what: &str) -> usize {
    let fin = &r.file;
    for (i, f) in r.finals.iter().enumerate() {
        assert_eq!(f, fin, "{what}: face {i} differs from the file");
    }
    let mut count: HashMap<char, usize> = HashMap::new();
    for ch in fin.chars().filter(|c| *c as u32 >= 0x4E00) {
        *count.entry(ch).or_default() += 1;
    }
    let doubled: Vec<char> = count.iter().filter(|(_, n)| **n > 1).map(|(c, _)| *c).collect();
    assert!(doubled.is_empty(), "{what}: doubled {doubled:?} in {fin:?}");
    let lost: Vec<char> = r.typed.iter().filter(|c| !r.deleted.contains(c) && !count.contains_key(c)).copied().collect();
    assert!(lost.is_empty(), "{what}: lost {lost:?} from {fin:?}");
    let back: Vec<char> = r.deleted.iter().filter(|c| count.contains_key(c)).copied().collect();
    assert!(back.is_empty(), "{what}: deleted yet back {back:?} in {fin:?}");
    // Order: every two typed characters that sat together in some face keep
    // the order they had there.
    let here: HashSet<char> = count.keys().copied().collect();
    let fin_order: Vec<char> = fin.chars().filter(|c| here.contains(c)).collect();
    let mut b7_doubled = 0;
    for (kind, s) in &r.snapshots {
        let mut once = HashSet::new();
        if s.chars().filter(|c| *c as u32 >= 0x4E00).any(|c| !once.insert(c)) {
            assert_eq!(*kind, Kind::B7, "{what}: a face of this build showed doubled text {s:?}");
            b7_doubled += 1;
            continue;
        }
        let in_s: HashSet<char> = s.chars().collect();
        let a: Vec<char> = s.chars().filter(|c| here.contains(c)).collect();
        let b: Vec<char> = fin_order.iter().filter(|c| in_s.contains(c)).copied().collect();
        assert_eq!(a, b, "{what}: reordered against a face's text {s:?}");
    }
    b7_doubled
}

fn property(kinds: [Kind; 2], seeds: u64) {
    let (mut steps, mut shown) = (0, 0);
    for gap in [30u64, 150] {
        for seed in 1..=seeds {
            let what = format!("{kinds:?} gap {gap} ms seed {seed}");
            let r = run(kinds, seed, gap, 60);
            shown += check(&r, &what);
            steps += r.snapshots.len();
            // Deterministic: the same interleaving ends the same.
            let again = run(kinds, seed, gap, 60);
            assert_eq!(r.file, again.file, "{what}: not deterministic");
        }
    }
    eprintln!("{kinds:?}: {} runs, {steps} steps, {shown} with b7c1cb42's own doubling on its screen", seeds * 2);
}

#[test]
fn g1_trace() {
    if let (Ok(k), Ok(seed), Ok(gap)) = (std::env::var("G1_TRACE"), std::env::var("G1_SEED"), std::env::var("G1_GAP")) {
        let kinds = match k.as_str() {
            "nb" => [Kind::New, Kind::B7],
            "bn" => [Kind::B7, Kind::New],
            "nB" => [Kind::New, Kind::Blind],
            _ => [Kind::Blind, Kind::New],
        };
        let _ = run(kinds, seed.parse().unwrap(), gap.parse().unwrap(), 60);
    }
}

#[test]
fn g1_property_new_beside_his_older_face() {
    property([Kind::New, Kind::B7], 60);
}

#[test]
fn g1_property_his_older_face_beside_new() {
    property([Kind::B7, Kind::New], 60);
}

#[test]
fn g1_property_two_new_faces() {
    property([Kind::New, Kind::New], 60);
}

// ── 0252 G1's repros, exactly ──────────────────────────────────────────────

/// The old face types or deletes one keystroke at a time; the new face ticks
/// between keystrokes. Both end on what the old face shows.
fn old_face_edits(start: &str, keys: &[&dyn Fn(&mut String, &mut usize)]) -> (String, String, String) {
    let dir = scratch();
    let mut n = New::open(&dir);
    n.set(start.to_string(), start.len());
    n.save();
    let mut o = B7::open(&dir);
    for k in keys {
        let (mut t, mut c) = (o.text.clone(), o.caret);
        k(&mut t, &mut c);
        o.set(t, c);
        o.save();
        n.poll();
    }
    o.poll();
    let file = read_file(&dir.join("bucket.json")).unwrap().1;
    (n.text, o.text, file)
}

fn ty(s: &'static str) -> Vec<Box<dyn Fn(&mut String, &mut usize)>> {
    s.chars()
        .map(|ch| {
            Box::new(move |t: &mut String, c: &mut usize| {
                t.insert(*c, ch);
                *c += ch.len_utf8();
            }) as Box<dyn Fn(&mut String, &mut usize)>
        })
        .collect()
}

fn bs(n: usize) -> Vec<Box<dyn Fn(&mut String, &mut usize)>> {
    (0..n)
        .map(|_| {
            Box::new(|t: &mut String, c: &mut usize| {
                if let Some(ch) = t[..*c].chars().next_back() {
                    *c -= ch.len_utf8();
                    t.remove(*c);
                }
            }) as Box<dyn Fn(&mut String, &mut usize)>
        })
        .collect()
}

fn to(at: usize) -> Vec<Box<dyn Fn(&mut String, &mut usize)>> {
    vec![Box::new(move |_: &mut String, c: &mut usize| *c = at)]
}

fn go(start: &str, steps: Vec<Vec<Box<dyn Fn(&mut String, &mut usize)>>>) -> (String, String, String) {
    let all: Vec<Box<dyn Fn(&mut String, &mut usize)>> = steps.into_iter().flatten().collect();
    let refs: Vec<&dyn Fn(&mut String, &mut usize)> = all.iter().map(|b| b.as_ref()).collect();
    old_face_edits(start, &refs)
}

#[test]
fn g1_slow_typing_in_the_older_face_is_one_word() {
    let (n, o, f) = go("", vec![ty(" hello there")]);
    assert_eq!(o, " hello there");
    assert_eq!(n, o);
    assert_eq!(f, o);
}

#[test]
fn g1_backspaces_and_retyping_in_the_older_face() {
    let (n, o, f) = go("", vec![ty(" hello world"), bs(3), ty("ld")]);
    assert_eq!(o, " hello wold");
    assert_eq!((n.as_str(), f.as_str()), (o.as_str(), o.as_str()));
}

#[test]
fn g1_a_mid_word_fix_in_the_older_face() {
    // `wrd` corrected to `word`: the caret put after the `w`, an `o` typed.
    let (n, o, f) = go("a wrd here", vec![to(3), ty("o")]);
    assert_eq!(o, "a word here");
    assert_eq!((n.as_str(), f.as_str()), (o.as_str(), o.as_str()));
}

#[test]
fn g2_the_older_faces_deletion_applies() {
    let (n, o, f) = go("alpha beta gamma", vec![to(10), bs(5)]);
    assert_eq!(o, "alpha gamma");
    assert_eq!((n.as_str(), f.as_str()), (o.as_str(), o.as_str()));
}

// ── the merge itself ───────────────────────────────────────────────────────

#[test]
fn g1_merge3_is_character_exact() {
    // An edit inside a word, beside one elsewhere: both, once.
    assert_eq!(merge3("one wrd three", "one wrd three!", "one word three", 14).0, "one word three!");
    // Both delete the same letter: it goes once, nothing else does.
    assert_eq!(merge3("abcd", "abd", "abd", 2).0, "abd");
    // One deletes a letter the other typed beside: the typed one stays.
    assert_eq!(merge3("ab", "a", "aXb", 1).0, "aX");
    // Overlapping replacements: every letter either typed, once.
    let (m, _) = merge3("the cat sat", "the dog sat", "the cow sat", 7);
    for c in ["d", "o", "g", "w"] {
        assert!(m.contains(c), "{m}");
    }
    assert_eq!(m.matches('o').count(), 2, "dog's o and cow's o: {m}");
    // Several edits on each side at once.
    assert_eq!(merge3("a b c d", "A b c D", "a B c d!", 0).0, "A B c D!");
}

#[test]
fn g1_unmarked_fresh_writes_are_exact() {
    // A Backspace: the base minus one letter, even when that text was an
    // earlier bucket here (the letter had been typed and adopted).
    let seen = vec!["x hel".to_string(), "x hell".to_string()];
    assert_eq!(merge3_unmarked(&seen, None, "x hell", &all_own("x hell"), "x hell", "x hel", 0).0, "x hel");
    // A clear after a send, with this face's unseen words since: those stay.
    assert_eq!(merge3_unmarked(&[], None, "sent text", &all_own("sent text"), "sent text and more", "", 18).0, " and more");
}

/// Every character of `base` typed by this face.
fn all_own(base: &str) -> Vec<bool> {
    vec![true; base.chars().count()]
}

// ── G3, G4, G5 at the face ─────────────────────────────────────────────────

mod at_the_face {
    use std::path::Path;

    use env::paths::Source;
    use env::Environment;
    use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use tui::cauldron::app::{App, Focus};
    use tui::cauldron::store::{seed_dir, DraftStore};

    fn open_env(root: &Path) -> App {
        std::env::set_var("CAULDRON_TYPE_ONE", "off");
        let env = Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") };
        App::open(env, seed_dir(), true).expect("opens")
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn into_bucket(a: &mut App) {
        if a.focus != Focus::Input {
            a.handle(key(KeyCode::Char('a')));
        }
        a.handle(key(KeyCode::End));
        a.composer = tui_render::compose::Composer::new();
    }

    fn typed(a: &mut App, s: &str) {
        for c in s.chars() {
            a.handle(key(KeyCode::Char(c)));
        }
    }

    fn draft(t: &str) -> String {
        format!("{{\"nonce\":\"{}\",\"writer\":\"w\",\"updated_at\":\"2026-10-25T05:41:44+00:00\",\"text\":{}}}", uuid::Uuid::new_v4(), serde_json::to_string(t).unwrap())
    }

    /// A drafts-era face typing on, one write per keystroke.
    fn types_on(a: &mut App, dir: &Path, sofar: &mut String, keys: &str) {
        for c in keys.chars() {
            sofar.push(c);
            std::fs::write(dir.join("drafts/cauldron.json"), draft(sofar)).unwrap();
            a.sync();
        }
    }

    /// 0252 G4: a gain is judged by position. Letters and short words the
    /// bucket already holds elsewhere all come in.
    #[test]
    fn g4_a_drafts_era_faces_letters_all_come_in() {
        let root = super::scratch();
        let dir = root.join("data/prod/tui/soup");
        let mut a = open_env(&root);
        into_bucket(&mut a);
        typed(&mut a, "x y hello the ok");
        a.sync();
        std::fs::create_dir_all(dir.join("drafts")).unwrap();
        let mut sofar = String::from("L3");
        std::fs::write(dir.join("drafts/cauldron.json"), draft(&sofar)).unwrap();
        a.sync();
        assert_eq!(a.composer.text(), "x y hello the ok\n\nL3");
        types_on(&mut a, &dir, &mut sofar, "x more wordsy hello the ok");
        assert_eq!(a.composer.text(), "x y hello the ok\n\nL3x more wordsy hello the ok");
        // A letter typed twice running, both.
        types_on(&mut a, &dir, &mut sofar, "kk");
        assert!(a.composer.text().ends_with("the okkk"), "{:?}", a.composer.text());
    }

    /// 0252 G5: once its first continuation came in as a paragraph of its
    /// own, what it types on glues on to that, not a paragraph per append.
    #[test]
    fn g5_appends_glue_on_after_a_first_paragraph() {
        let root = super::scratch();
        let dir = root.join("data/prod/tui/soup");
        let mut a = open_env(&root);
        into_bucket(&mut a);
        typed(&mut a, "mine");
        a.sync();
        std::fs::create_dir_all(dir.join("drafts")).unwrap();
        let mut sofar = String::from("a long last line the bucket does not end with");
        std::fs::write(dir.join("drafts/cauldron.json"), draft(&sofar)).unwrap();
        a.sync();
        // He types on at the bucket's end: it no longer ends with that line.
        for _ in 0..3 {
            a.handle(key(KeyCode::Down));
        }
        a.handle(key(KeyCode::End));
        typed(&mut a, " more");
        a.sync();
        for w in [" L1", " L2", " L3"] {
            sofar.push_str(w);
            std::fs::write(dir.join("drafts/cauldron.json"), draft(&sofar)).unwrap();
            a.sync();
        }
        assert_eq!(a.composer.text(), "mine\n\na long last line the bucket does not end with more\n\nL1 L2 L3");
    }

    /// 0252 G3: a stale write never drops a word this face once held there.
    /// The face typed ` the dog` and took it out; a stale write of ` the cat`
    /// (from before) comes in whole.
    #[test]
    fn g3_a_stale_write_keeps_its_every_word() {
        let dir = super::scratch();
        let path = dir.join("bucket.json");
        let mut s = DraftStore::new(&dir, true);
        let mut text = s.load().unwrap();
        let mut put = |s: &mut DraftStore, text: &mut String| {
            let at = text.len();
            if let Some((t, _)) = s.save(text, at).unwrap() {
                *text = t;
            }
        };
        text.push('x');
        put(&mut s, &mut text);
        let snap = text.clone();
        for c in " the dog".chars() {
            text.push(c);
            put(&mut s, &mut text);
        }
        for _ in 0..8 {
            text.pop();
            put(&mut s, &mut text);
        }
        super::write_old(&path, &format!("{snap} the cat"));
        let at = text.len();
        if let Some((t, _)) = s.poll(&text, at).unwrap() {
            text = t;
        }
        assert_eq!(text, "x the cat");
    }
}

// ── adversarial: a face from before b7c1cb42 (blind, stale) ────────────────

/// Measured, not asserted: a blind writer's stale write cannot always be
/// told from b7c1cb42's fresh deletion (0256 report, `unknowns`). Prints how
/// many runs of each pairing broke which part of the property.
#[test]
#[ignore = "measurement for the 0256 report; run with --ignored --nocapture"]
fn adv_blind_writer_measured() {
    for kinds in [[Kind::New, Kind::Blind], [Kind::Blind, Kind::New]] {
        let mut tally: HashMap<String, usize> = HashMap::new();
        let mut runs = 0;
        for gap in [30u64, 150] {
            for seed in 1..=60 {
                runs += 1;
                let r = run(kinds, seed, gap, 60);
                let what = String::new();
                let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&r, &what)));
                let kind = match out {
                    Ok(_) => "clean".to_string(),
                    Err(e) => {
                        let m = e.downcast_ref::<String>().cloned().unwrap_or_default();
                        ["lost", "doubled", "deleted yet back", "reordered", "differs", "showed doubled"].iter().find(|k| m.contains(*k)).map_or("other".to_string(), |k| k.to_string())
                    }
                };
                *tally.entry(kind).or_default() += 1;
            }
        }
        eprintln!("{kinds:?}: {runs} runs: {tally:?}");
    }
}
