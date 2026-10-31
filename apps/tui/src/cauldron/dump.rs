//! **The bucket's grammar** (design 0138, option C — paragraph notes): ONE
//! text for the cauldron (8e4fca10 msg 235: "We're gonna have one bucket and
//! because it's all going into one soup"), the bottom box of the Surface
//! tab, and a BLANK LINE is the cut. Every paragraph is a NOTE; a note whose
//! first line is `> [tag]` (the quote macro, `i`) is ANCHORED to that item.
//! Every PASTE is a CHIP in the text (`tags::Tag::Chip`), its body kept
//! beside the bucket — and a speech-to-text tool's words arrive as a paste
//! (msg 235: "I actually don't want to support transcription directly into
//! the Cauldron ... it just comes in as a paste").
//!
//! This module is the grammar and nothing else: where the notes are, what a
//! chip shows, where chip bodies live, and the record a send produces. The
//! keys are `app`'s; the drawing is `view`'s.
//!
//! The record (one line of `dumps.jsonl`, `store::Send`) carries the text
//! AND its notes with their anchors and offsets, and the artifact bodies by
//! id — the contract between the face and the `dump` ripple, so the ripple
//! never re-derives the cut (0138 §4.5). Nothing processes a dump yet: the
//! ripple is a definition in the policy (`<seeds>/ripples.json`), read on Status › Bus.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cauldron::tags::{self, Tag};

// ── notes ────────────────────────────────────────────────────────────────

/// One note of a dump's text: its 1-based number and its BYTE range (no
/// leading or trailing blank lines).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteSpan {
    pub n: usize,
    pub start: usize,
    pub end: usize,
}

/// Every note in `text`: each run of lines with no blank line in it. A blank
/// line is one holding nothing but whitespace.
pub fn notes(text: &str) -> Vec<NoteSpan> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut cur: Option<(usize, usize)> = None;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        if body.trim().is_empty() {
            if let Some((s, e)) = cur.take() {
                out.push(NoteSpan { n: out.len() + 1, start: s, end: e });
            }
        } else {
            let end = at + body.len();
            cur = Some(match cur {
                Some((s, _)) => (s, end),
                None => (at, end),
            });
        }
        at += line.len();
    }
    if let Some((s, e)) = cur {
        out.push(NoteSpan { n: out.len() + 1, start: s, end: e });
    }
    out
}

/// The note `byte` falls in (a caret on a blank line between two notes is
/// in neither).
pub fn note_at(text: &str, byte: usize) -> Option<NoteSpan> {
    notes(text).into_iter().find(|n| byte >= n.start && byte <= n.end)
}

/// The 1-based note each LOGICAL line of `text` belongs to (none on a blank
/// line), and whether it is that note's first line — what the gutter draws.
pub fn line_notes(text: &str) -> Vec<Option<(usize, bool)>> {
    let mut out = Vec::new();
    let mut n = 0;
    let mut inside = false;
    for line in text.split('\n') {
        if line.trim().is_empty() {
            inside = false;
            out.push(None);
        } else {
            let first = !inside;
            if first {
                n += 1;
            }
            inside = true;
            out.push(Some((n, first)));
        }
    }
    out
}

/// One note as a dump RECORD carries it: offsets in CHARACTERS of the
/// stored text, the items it is anchored to (the tags of its `> ` line), and
/// the chips in it (`paste:b21c`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Note {
    pub n: usize,
    pub start: usize,
    pub end: usize,
    #[serde(default)]
    pub anchors: Vec<String>,
    #[serde(default)]
    pub chips: Vec<String>,
}

/// The chip ids in `s`, as a record names them: `paste:b21c`.
pub fn chip_ids(s: &str) -> Vec<String> {
    tags::scan(s).into_iter().filter_map(|(_, t)| t.chip_id().map(|(k, id)| format!("{k}:{id}"))).collect()
}

/// The notes of `text`, as the record carries them.
pub fn note_records(text: &str) -> Vec<Note> {
    let chars = |b: usize| text[..b].chars().count();
    notes(text)
        .into_iter()
        .map(|sp| {
            let body = &text[sp.start..sp.end];
            let first = body.lines().next().unwrap_or("");
            let anchors = if first.trim_start().starts_with('>') {
                tags::scan(first).into_iter().filter(|(_, t)| t.chip_id().is_none()).map(|(_, t)| t.render().trim_matches(['[', ']']).to_string()).collect()
            } else {
                Vec::new()
            };
            Note { n: sp.n, start: chars(sp.start), end: chars(sp.end), anchors, chips: chip_ids(body) }
        })
        .collect()
}

// ── chips ────────────────────────────────────────────────────────────────

/// How a chip's head is cut (0138 §4.3: the whole chip fits in 72 columns).
const HEAD_CHARS: usize = 34;

/// `1,204`.
pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The first `HEAD_CHARS` characters of `text`'s first line, cut with `…`.
fn head(text: &str, whole_line: bool) -> String {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let n = first.chars().count();
    if n <= HEAD_CHARS && (whole_line || n == text.trim().chars().count()) {
        return first.replace('"', "'");
    }
    let cut: String = first.chars().take(HEAD_CHARS).collect();
    format!("{}…", cut.trim_end().replace('"', "'"))
}

/// A paste chip: `[paste b21c: "fn main() {" 14 lines · 412 chars]`.
pub fn paste_chip(id: &str, body: &str) -> Tag {
    let lines = body.lines().count();
    let size = if lines > 1 { format!("{lines} lines · {} chars", thousands(body.chars().count())) } else { format!("{} chars", thousands(body.chars().count())) };
    Tag::chip("paste", id, &format!(": \"{}\" {size}", head(body, true)))
}

// ── where chip bodies live ───────────────────────────────────────────────

/// The bucket's chip bodies: `bucket/artifacts/<id>.txt`, beside it (0138
/// §7).
pub struct Artifacts {
    pub dir: PathBuf,
    writable: bool,
    /// Bodies held in memory when nothing may be written (the `--dump` path).
    mem: std::collections::BTreeMap<String, String>,
}

impl Artifacts {
    pub fn new(soup: &Path, writable: bool) -> Self {
        Artifacts { dir: soup.join("bucket").join("artifacts"), writable, mem: Default::default() }
    }

    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.txt"))
    }

    pub fn read(&self, id: &str) -> Option<String> {
        self.mem.get(id).cloned().or_else(|| std::fs::read_to_string(self.path(id)).ok())
    }

    pub fn write(&mut self, id: &str, body: &str) -> Result<()> {
        self.mem.insert(id.to_string(), body.to_string());
        if !self.writable {
            return Ok(());
        }
        crate::cauldron::store::write_text_atomic(&self.path(id), body).with_context(|| format!("keep chip {id}"))
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        self.mem.remove(id);
        if !self.writable {
            return Ok(());
        }
        match std::fs::remove_file(self.path(id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).with_context(|| format!("drop chip {id}")),
            _ => Ok(()),
        }
    }

    /// A fresh id: four hex characters no chip here has.
    pub fn fresh_id(&self, text: &str) -> String {
        loop {
            let id: String = uuid::Uuid::new_v4().simple().to_string().chars().take(4).collect();
            if !text.contains(&format!(" {id}")) && self.read(&id).is_none() {
                return id;
            }
        }
    }
}

/// Copy every chip body the buckets of before kept (`drafts/<cluster>/
/// artifacts/<id>.txt`, design 0138) beside the one bucket, where it is not
/// there yet: the bucket adopting their texts ([`crate::cauldron::store::
/// readopt`]) keeps what their chips hold. Nothing old is
/// deleted.
pub fn adopt_chips(soup: &Path) -> Result<()> {
    let to = soup.join("bucket").join("artifacts");
    let Ok(rd) = std::fs::read_dir(soup.join("drafts")) else { return Ok(()) };
    for e in rd.flatten() {
        let Ok(files) = std::fs::read_dir(e.path().join("artifacts")) else { continue };
        for f in files.flatten() {
            let dest = to.join(f.file_name());
            if dest.exists() {
                continue;
            }
            let body = std::fs::read_to_string(f.path()).with_context(|| format!("read {}", f.path().display()))?;
            crate::cauldron::store::write_text_atomic(&dest, &body)?;
        }
    }
    Ok(())
}

/// One artifact as a dump record carries it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub kind: String,
    pub chars: usize,
    /// Where its body is.
    pub path: String,
}

// ── the dump in the face ─────────────────────────────────────────────────

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_render::compose::Composer;

use crate::cauldron::app::{App, Modal};

/// What the chip modal is focused on: the text, or one of its buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipAt {
    Text,
    /// `insert as text`
    Insert,
    /// `keep`
    Keep,
    /// `discard` (a second Enter confirms)
    Discard,
}

/// The caret's position as one byte offset into the composer's whole text.
pub fn caret_offset(c: &Composer) -> usize {
    let cur = c.cursor();
    c.lines()[..cur.line].iter().map(|l| l.len() + 1).sum::<usize>() + cur.byte
}

/// Put the caret at byte `at` of the whole text (or the nearest boundary
/// after it).
pub fn set_caret(c: &mut Composer, at: usize) {
    c.buffer_start();
    let total = c.text().len();
    while caret_offset(c) < at.min(total) {
        let before = caret_offset(c);
        c.right();
        if caret_offset(c) == before {
            break;
        }
    }
}

/// Replace bytes `range` of the whole text with `with`, as EDITS (so undo
/// takes it back): the caret goes to the range's end, deletes back to its
/// start, and `with` is pasted.
pub fn replace_range(c: &mut Composer, range: std::ops::Range<usize>, with: &str) {
    set_caret(c, range.end);
    while caret_offset(c) > range.start {
        let before = caret_offset(c);
        c.backspace();
        if caret_offset(c) == before {
            break;
        }
    }
    if !with.is_empty() {
        c.insert_paste(with);
    }
}

/// The tag the caret is on or against, with its byte range in the WHOLE
/// text: `before` asks for one ending at (or holding) the caret, as
/// Backspace does; otherwise one starting at (or holding) it, as Delete
/// does. A chip and a reference are ONE unit (0138 §4.3; msg 165).
pub fn tag_at(c: &Composer, before: bool) -> Option<(std::ops::Range<usize>, Tag)> {
    let text = c.text();
    let at = caret_offset(c);
    tags::scan(&text).into_iter().find(|(r, _)| if before { r.start < at && at <= r.end } else { r.start <= at && at < r.end })
}

impl App {
    /// A setting's number, as the cauldron's settings say.
    pub fn setting_int(&self, key: &str, default: i64) -> i64 {
        match self.settings.get(crate::cauldron::settings::def(key), &Default::default()).val {
            crate::cauldron::settings::Val::Int(n) => n,
            _ => default,
        }
    }

    /// A paste into the bucket: always a CHIP (msg 235: "fuck it just all
    /// pastes"), whatever its length — a click on it opens its text.
    pub fn paste_into_dump(&mut self, s: &str) {
        if s.trim().is_empty() {
            return;
        }
        // His own edit: the reference he went by is his to replace again.
        self.went_by = None;
        let id = self.chips.fresh_id(&self.composer.text());
        let body = s.replace('\r', "");
        if let Err(e) = self.chips.write(&id, &body) {
            self.error = Some(format!("paste NOT kept: {e:#}"));
            return;
        }
        let chip = paste_chip(&id, &body).render();
        self.composer.insert_paste(&chip);
        self.save_draft_now();
    }

    pub(crate) fn save_draft_now(&mut self) {
        match self.draft.save(&self.composer.text(), caret_offset(&self.composer)) {
            Ok(merged) => {
                self.error = None;
                self.take_merged(merged);
            }
            Err(e) => self.error = Some(format!("bucket NOT saved: {e:#}")),
        }
        self.save_refs();
    }

    /// Carry the references' origins over to the bucket as it is, and write
    /// them if they moved (8e4fca10 msg 385; `refs`).
    pub(crate) fn save_refs(&mut self) {
        if let Err(e) = self.refs.save(&self.composer.text()) {
            self.error = Some(format!("the references' index NOT saved: {e:#}"));
        }
    }

    /// Take the bucket merged with what another face wrote (0220 E2) as
    /// this face's text, the caret where the merge put it.
    pub(crate) fn take_merged(&mut self, merged: Option<(String, usize)>) {
        if let Some((text, at)) = merged {
            if text != self.composer.text() {
                // The anchors follow this face's own edits first, then the
                // merge: each step its own diff (`refs::follow`).
                self.refs.follow_to(&self.composer.text());
                self.composer = Composer::from_text(&text);
                set_caret(&mut self.composer, at);
                self.refs.follow_to(&text);
            }
        }
    }

    /// `^S` in the dump: send the note the caret is in, alone, as its own
    /// dump, and take it out of the text (`designs::SEND_ONE_NOTE`).
    pub fn send_note(&mut self) {
        let text = self.composer.text();
        let at = caret_offset(&self.composer);
        let Some(n) = note_at(&text, at) else {
            self.notice = Some("the caret is between notes: nothing to send".into());
            return;
        };
        let note = text[n.start..n.end].to_string();
        if !self.send_text(&note) {
            return;
        }
        // The note goes, with the blank line that parted it from the next.
        let mut end = n.end;
        while end < text.len() && text[end..].starts_with('\n') {
            end += 1;
        }
        let mut start = n.start;
        if end == text.len() {
            while start > 0 && text[..start].ends_with('\n') {
                start -= 1;
            }
        }
        let rest = format!("{}{}", &text[..start], &text[end..]);
        self.composer = Composer::from_text(&rest);
        set_caret(&mut self.composer, start.min(rest.len()));
        self.save_draft_now();
    }

    /// The chips in `text`, as a record carries their bodies.
    pub fn artifact_refs(&self, text: &str) -> std::collections::BTreeMap<String, ArtifactRef> {
        let mut out = std::collections::BTreeMap::new();
        for (_, t) in tags::scan(text) {
            if let Some((kind, id)) = t.chip_id() {
                let chars = self.chips.read(id).map_or(0, |b| b.chars().count());
                out.insert(format!("{kind}:{id}"), ArtifactRef { kind: kind.to_string(), chars, path: self.chips.path(id).display().to_string() });
            }
        }
        out
    }

    /// `^O` in the bucket, or a click on a chip: open the chip under (or
    /// against) the caret. A paste opens in its modal; a reference reveals
    /// its item on the page, the bucket keeping the selection.
    pub fn open_chip_at_caret(&mut self) {
        let Some((_, tag)) = tag_at(&self.composer, false).or_else(|| tag_at(&self.composer, true)) else {
            self.notice = Some("no chip at the caret".into());
            return;
        };
        self.open_chip(&tag);
    }

    pub fn open_chip(&mut self, tag: &Tag) {
        if let Some((_, id)) = tag.chip_id() {
            let body = self.chips.read(id).unwrap_or_default();
            let mut editor = Composer::from_text(&body);
            editor.buffer_end();
            self.modal = Some(Modal::Chip { id: id.to_string(), editor, at: ChipAt::Text, confirm: false });
            return;
        }
        // A reference: its item on the page, when the page shows it.
        match self.items().iter().position(|&it| self.tag_for(it).as_ref() == Some(tag)) {
            Some(i) => {
                self.item = i;
                self.reveal = true;
                self.notice = Some(format!("{} is on the page", tag.render()));
            }
            None => self.notice = Some(format!("{} is not on this page", tag.render())),
        }
    }

    /// Close the chip modal KEEPING the edit (the new-cluster modal's rule):
    /// its body saved and its chip relabelled.
    pub fn keep_chip(&mut self) {
        let Some(Modal::Chip { id, editor, .. }) = self.modal.take() else { return };
        let body = editor.text();
        let old = self.chips.read(&id).unwrap_or_default();
        if body == old {
            return;
        }
        if let Err(e) = self.chips.write(&id, &body) {
            self.error = Some(format!("the paste NOT kept: {e:#}"));
            return;
        }
        self.relabel_chip(&id, &paste_chip(&id, &body).render());
    }

    /// Put `with` where chip `id` is in the dump (none: nothing).
    fn relabel_chip(&mut self, id: &str, with: &str) {
        let text = self.composer.text();
        let Some((r, _)) = tags::scan(&text).into_iter().find(|(_, t)| t.chip_id().is_some_and(|(_, x)| x == id)) else { return };
        let caret = caret_offset(&self.composer);
        replace_range(&mut self.composer, r.clone(), with);
        let back = if caret >= r.end { caret + with.len() - (r.end - r.start) } else { caret.min(r.start) };
        set_caret(&mut self.composer, back);
        self.save_draft_now();
    }

    /// A chip modal's button.
    fn chip_button(&mut self, at: ChipAt) {
        let Some(Modal::Chip { id, editor, confirm, .. }) = self.modal.clone() else { return };
        match at {
            ChipAt::Text => {}
            ChipAt::Keep => self.keep_chip(),
            // Insert as text: the chip is replaced by its (edited) body, as
            // an edit, so it undoes as one (0138 §4.3).
            ChipAt::Insert => {
                self.modal = None;
                let body = editor.text();
                let _ = self.chips.write(&id, &body);
                self.relabel_chip(&id, &body);
                self.notice = Some("the paste is text now".into());
            }
            ChipAt::Discard if !confirm => {
                if let Some(Modal::Chip { confirm, .. }) = &mut self.modal {
                    *confirm = true;
                }
            }
            ChipAt::Discard => {
                self.modal = None;
                self.relabel_chip(&id, "");
                if let Err(e) = self.chips.remove(&id) {
                    self.error = Some(format!("{e:#}"));
                }
                self.notice = Some("the paste is discarded".into());
            }
        }
    }

    /// Keys in the chip modal (rule 8: it takes every key). In the text the
    /// composer's grammar: letters are text, Enter a new line, ↑ ↓ by line
    /// and ↓ past the last line to the buttons. On the buttons ← → move
    /// between them, Enter acts, ↑ goes back to the text. Esc — or ← at the
    /// start of a text he has not changed — closes it KEEPING the edit.
    pub fn chip_modal_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Modal::Chip { id, editor, at, confirm, .. }) = self.modal.as_mut() else { return };
        // Any key but Enter takes back a discard that asked to be confirmed.
        if *confirm && key.code != KeyCode::Enter {
            *confirm = false;
            return;
        }
        let w = 76;
        if *at != ChipAt::Text {
            let order = [ChipAt::Insert, ChipAt::Keep, ChipAt::Discard];
            let i = order.iter().position(|x| x == at).unwrap_or(0);
            match key.code {
                KeyCode::Esc => self.keep_chip(),
                KeyCode::Left if i == 0 => *at = ChipAt::Text,
                KeyCode::Left => *at = order[i - 1],
                KeyCode::Right => *at = order[(i + 1).min(2)],
                KeyCode::Up => *at = ChipAt::Text,
                KeyCode::Home => *at = ChipAt::Insert,
                KeyCode::End => *at = ChipAt::Discard,
                KeyCode::Enter => {
                    let a = *at;
                    self.chip_button(a);
                }
                _ => {}
            }
            return;
        }
        let unchanged = self.chips.read(id).unwrap_or_default() == editor.text();
        match key.code {
            KeyCode::Esc => self.keep_chip(),
            KeyCode::Left if editor.cursor().line == 0 && editor.cursor().byte == 0 && unchanged => self.keep_chip(),
            KeyCode::Left => editor.left(),
            KeyCode::Right => editor.right(),
            KeyCode::Up => {
                editor.up(w);
            }
            KeyCode::Down => {
                if !editor.down(w) {
                    *at = ChipAt::Insert;
                }
            }
            KeyCode::Home => editor.row_start(w),
            KeyCode::End => editor.row_end(w),
            KeyCode::Enter => editor.newline(),
            KeyCode::Backspace => editor.backspace(),
            KeyCode::Delete => editor.delete(),
            KeyCode::Tab => *at = ChipAt::Insert,
            KeyCode::Char('u') if ctrl => editor.kill_to_line_start(),
            KeyCode::Char('k') if ctrl => editor.kill_to_line_end(),
            KeyCode::Char('z') if ctrl => {
                editor.undo();
            }
            KeyCode::Char(ch) if !ctrl => editor.insert_char(ch),
            _ => {}
        }
    }

    /// A click on a chip modal's button `i` (0 insert, 1 keep, 2 discard).
    pub fn chip_click(&mut self, i: u8) {
        let at = match i {
            0 => ChipAt::Insert,
            1 => ChipAt::Keep,
            _ => ChipAt::Discard,
        };
        if let Some(Modal::Chip { at: a, confirm, .. }) = &mut self.modal {
            if at != ChipAt::Discard || *a != ChipAt::Discard {
                *confirm = false;
            }
            *a = at;
        }
        self.chip_button(at);
    }

    /// Everything in the bucket as text to paste elsewhere (msg 186: "I
    /// might use the dump area in order to build up my payload, and then I
    /// copy it and paste it over here"): each chip's whole text in its place,
    /// since a chip means nothing outside this face.
    pub fn bucket_text(&self) -> String {
        let mut text = self.composer.text();
        let mut chips: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        for (r, t) in tags::scan(&text) {
            if let Some((_, id)) = t.chip_id() {
                if let Some(body) = self.chips.read(id) {
                    chips.push((r, body));
                }
            }
        }
        for (r, body) in chips.into_iter().rev() {
            text.replace_range(r, &body);
        }
        text
    }

    /// `copy` on the bucket's border, or ⌥A: the whole bucket to the
    /// clipboard (OSC 52, via term-shell), chips as their text.
    pub fn copy_bucket(&mut self) {
        let text = self.bucket_text();
        if text.trim().is_empty() {
            self.notice = Some("the bucket is empty: nothing to copy".into());
            return;
        }
        self.notice = Some(format!("copying the bucket · {} chars", thousands(text.chars().count())));
        self.outbox.push(crate::cauldron::app::Outbox::Copy(text));
    }
}
