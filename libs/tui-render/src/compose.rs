//! **THE COMPOSER'S MODEL — the buffer, the cursor, the layout and the undo.**
//!
//! It lives in the PURE crate on purpose. `0023`'s architecture forbids a
//! self-rendering widget in the band, and a textarea widget would have made the
//! surface he touches most the one band `assert_shape` cannot see and `0018`'s
//! repaint law cannot reason about. So the model is ours, it is data, and every
//! law below is a unit test rather than a promise about somebody else's crate.
//!
//! # The three decisions this file IS
//!
//! **1. The cursor is `{line, byte}`, and the byte is ALWAYS on an extended
//! grapheme cluster boundary.** Bytes are the only unit
//! `unicode_segmentation` speaks and the only one that is O(1) to slice.
//! CHARACTERS are the unit `tui-textarea` chose and they are wrong twice over:
//! a ZWJ family emoji is three-plus `char`s, so `Left`/`Right` land INSIDE the
//! glyph — and crossterm really does deliver such an emoji as several separate
//! `Char` events, so the buffer genuinely holds the pieces. The invariant is
//! enforced by [`snap`], which is IDEMPOTENT, and every mutation and every
//! motion in this file ends with it — so a cursor that lands mid-cluster
//! self-heals instead of panicking on the next slice.
//!
//! **2. The DISPLAY COLUMN IS NEVER STORED.** [`Composer::layout`] both
//! produces the rows a renderer draws AND answers "where is the cursor", so
//! wrap and cursor cannot drift: they are the same function. A stored column is
//! a lie the moment the terminal is resized, and that is the largest single
//! class of cursor bug in terminal editors. The one display-column value kept
//! is the vertical-motion GOAL COLUMN, and it is explicitly a hint — recomputed
//! on every horizontal edit and never trusted as truth.
//!
//! **3. NOTHING BUT TEXT IS EVER STORED.** No tab, no C0, no C1, no `U+FEFF`,
//! no lone CR. [`sanitize`] is the only door into the buffer and it is total.
//! That is not tidiness: writing raw escape bytes back at a terminal is a
//! CVE-grade class of its own, and a stray CR inside a logical line is a
//! cursor-drift bug that surfaces an hour later. Pulling tabs out deletes the
//! whole tab-stop glitch family at once — ratatui's `Paragraph` does not render
//! `\t` at all, terminals disagree about tab stops, and no chat message needs a
//! hard tab.
//!
//! # The rows TILE the line, which is what makes the cursor total
//!
//! [`Composer::layout`] returns rows whose byte ranges are contiguous and
//! exhaustive: `rows[i].end == rows[i+1].start` for every row of a line. A
//! wrapper that DROPS the space it broke on (which `geom::wrap` does, correctly,
//! for the transcript) leaves characters belonging to no row — fine for
//! painting, fatal for a caret, because there is then a cursor position with no
//! screen cell. Here the break space stays at the end of the row it ended, and
//! every byte of the buffer has exactly one home.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// What a Tab keypress and a tab inside a paste both become.
///
/// A tab is never stored; it is expanded on the way in. See the module header
/// for why that deletes a whole family of glitches rather than one.
pub const TAB_WIDTH: usize = 4;

/// The undo stack's ceiling, in ENTRIES.
pub const UNDO_DEPTH: usize = 200;

/// The undo stack's ceiling, in BYTES of retained buffer text.
///
/// Both ceilings exist because only one of them binds in each of the two cases
/// that matter: two hundred one-line edits are bounded by the depth, and three
/// snapshots of a ten-thousand-line paste are bounded by the bytes. A single
/// ceiling would have to be set for the worse case and would then be wrong for
/// the common one.
pub const UNDO_BYTES: usize = 4 * 1024 * 1024;

/// A position in the buffer: which logical line, and a BYTE offset into it.
///
/// The invariant, enforced everywhere by [`snap`]: `byte` is always on an
/// extended-grapheme-cluster boundary of `lines[line]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: usize,
    pub byte: usize,
}

impl Pos {
    pub fn new(line: usize, byte: usize) -> Self {
        Self { line, byte }
    }
}

/// One laid-out visual row: which logical line it came from, and the byte range
/// of that line it paints.
///
/// **The rows of a line TILE it** — see the module header. That is what makes
/// "which row is the cursor on" a total function rather than a special case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualRow {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// **THE WIDEST A SINGLE GRAPHEME CLUSTER MAY BE MEASURED AS.**
///
/// Not a fudge factor — a property of the medium. A terminal is a grid of cells
/// and a character occupies one of them or two; there is no third case. So a
/// CLUSTER, which the terminal draws as one glyph, can be measured at one or
/// two and at nothing else, whatever arithmetic a table suggests.
pub const CELL_MAX: usize = 2;

/// **THE ONE WIDTH FUNCTION.** An ASCII fast path, then `unicode-width` over
/// the whole cluster, then clamped into `1..=`[`CELL_MAX`].
///
/// **`.max(1)` is not defensive padding**, it is the fix for a confirmed bug
/// class: zellij DROPS width-0 codepoints and that is the documented cause of
/// its own #1538, where pasted combining diacritics silently vanish. A width-0
/// cluster still needs one editable cell or the cursor can never land on it.
/// helix ships the identical `.max(1)` and the identical fast path.
///
/// **`.min(CELL_MAX)` is a GUARD, and it is INERT TODAY — measured, not
/// assumed.** `unicode-width 0.1.14` already folds every cluster in this
/// module's nasty corpus to 1 or 2 (the ZWJ family to 2, a three-mark combining
/// pile to 1, a conjoining jamo syllable to 2), so the clamp changes no answer
/// on the table we are pinned to. It is here because the failure it prevents is
/// unbounded and the cost of preventing it is nothing: a cluster measured at
/// eight columns puts a laid-out row wider than the band it was laid out for,
/// and `0018` measured what ratatui 0.28 does with an over-reported line — it
/// PANICS rather than wrapping. A table that regresses, a version bump, or a
/// cluster nobody in the corpus thought of would each reach that panic through
/// this one function, and the clamp is where it stops.
/// `no_single_cluster_is_ever_measured_wider_than_a_cell_pair` is the CONTRACT
/// with the table, so a bump that changes the answer is looked at rather than
/// absorbed.
///
/// **What it cannot fix, said out loud.** The terminal, tmux and this table can
/// each believe a different number for the same cluster, and there is no
/// protocol on this desk to reconcile them — Mode 2027 is the real answer and
/// tmux does not implement it. The clamp bounds our error to at most one cell
/// per cluster in the direction that cannot panic, and the composer band takes
/// a full repaint whenever an edit touched a non-ASCII cluster so a drift
/// cannot accumulate on screen.
pub fn cluster_width(g: &str) -> usize {
    let b = g.as_bytes();
    if b.len() == 1 && b[0] >= 0x20 && b[0] < 0x7f {
        return 1;
    }
    UnicodeWidthStr::width(g).clamp(1, CELL_MAX)
}

/// The display width of a whole string, measured the way this file measures —
/// by CLUSTER, never by `char`.
///
/// It differs from `geom::text_width`, which sums per-`char`, and the
/// difference is the point: per-`char` summing counts a ZWJ family emoji as the
/// sum of its parts, which is how a composer row over-reports its width and how
/// ratatui 0.28 then PANICS instead of wrapping (`0018`).
pub fn str_width(s: &str) -> usize {
    s.graphemes(true).map(cluster_width).sum()
}

/// **SNAP TO A GRAPHEME BOUNDARY — idempotent, total, and the invariant's
/// enforcer.**
///
/// Clamps into `[0, s.len()]` and then walks BACK to the nearest cluster
/// boundary at or before `byte`. Idempotent means a cursor that has already
/// been snapped is unchanged, which is what lets every mutation and every
/// motion end with a call to this without any of them having to know whether
/// one of the others already did.
pub fn snap(s: &str, byte: usize) -> usize {
    if byte >= s.len() {
        return s.len();
    }
    let mut last = 0usize;
    for (i, _) in s.grapheme_indices(true) {
        if i > byte {
            return last;
        }
        last = i;
    }
    last
}

/// **THE ONLY DOOR INTO THE BUFFER.**
///
/// * `\r\n` and a lone `\r` both become `\n` — a probe of this stack shows a
///   paste arrives with its CRs intact, and a stray CR inside a logical line is
///   a cursor-drift bug that surfaces later and looks like something else.
/// * A tab becomes [`TAB_WIDTH`] spaces.
/// * Every other C0 control, `DEL`, every C1 control and `U+FEFF` is DROPPED.
///   A probe of this stack shows `NUL` and `BEL` survive a paste verbatim, and
///   writing raw escape bytes back at a terminal is its own CVE class
///   (CVE-2017-10784, CVE-2022-28391).
///
/// **What is deliberately KEPT**: `U+200D` (ZWJ) and `U+200B` (ZWSP), because
/// the first is load-bearing inside ordinary emoji and dropping the second
/// would be the zellij defect in a different coat — a character he pasted
/// vanishing with nothing said.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            '\t' => out.push_str(&" ".repeat(TAB_WIDTH)),
            '\u{feff}' => {}
            c if (c as u32) < 0x20 => {}
            c if (c as u32) == 0x7f => {}
            c if (0x80..=0x9f).contains(&(c as u32)) => {}
            c => out.push(c),
        }
    }
    out
}

/// Which kind of edit is in flight, for undo-run granularity.
///
/// tui-textarea commits per CHARACTER, which makes `^Z` useless on a paragraph;
/// helix commits per insert-mode SESSION, which is right for a modal editor and
/// too coarse for a box with no modes. A RUN is the middle, and it is what a
/// chat composer's user means by "undo that".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Type,
    Delete,
    /// Its own kind AND it closes the run on both sides, which is the only way
    /// a ten-thousand-line paste undoes as ONE operation.
    Paste,
    Kill,
}

impl EditKind {
    /// Whether this kind always starts and ends its own run.
    fn always_breaks(self) -> bool {
        matches!(self, EditKind::Paste)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snap {
    lines: Vec<String>,
    cursor: Pos,
}

/// **THE COMPOSER.** A `Vec<String>` of logical lines, a grapheme-boundary
/// cursor, an undo history measured in EDIT RUNS, and the queue of payloads he
/// has already composed and not yet sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composer {
    lines: Vec<String>,
    cursor: Pos,
    /// The vertical-motion goal, in DISPLAY COLUMNS. Explicitly a hint: reset
    /// by every horizontal edit and motion, never read as truth.
    goal: Option<usize>,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    run_open: bool,
    run_kind: Option<EditKind>,
    /// What the last kill took, for `^Y`.
    yanked: String,
    /// **THE QUEUE — his grammar, mirrored from the AUI.** *"in the aui we use
    /// left to queue, and wildcard to send the whole payload."* Each entry is
    /// one composed message he has set aside; a SEND stitches every one of them
    /// plus whatever is in the buffer into ONE payload.
    ///
    /// It is the CLIENT's queue and it is not the daemon's. The header badge
    /// counts the daemon's; this one is counted here and named apart on the
    /// composer's own border, because a badge whose scope is a guess reads as
    /// the wrong number.
    ///
    /// **It became a `Vec<PendingItem>` when mic mode landed**, from a
    /// `Vec<String>`. A spoken entry has a LIFE a typed one does not — it is
    /// recorded, then transcribing, then either text or a failure that can be
    /// retried — and it keeps a path to the audio it came from so the row can
    /// offer to play it back. See [`PendingItem`].
    pending: Vec<PendingItem>,
}

/// **WHAT BECOMES OF A SPOKEN ENTRY WHILE IT IS STILL BECOMING TEXT.**
///
/// A typed entry is born [`ItemState::Typed`] and never moves. An audio entry
/// walks `Transcribing → Ready`, or `Transcribing → Failed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemState {
    /// He typed it. There is nothing to wait for and nothing to play.
    Typed,
    /// The transcriber is running. **The row says so** — a queue that showed a
    /// blank row here would be a face that looked like it had lost his words.
    Transcribing,
    /// It is text now, and it goes out with everything else.
    Ready,
    /// It did not become text, and the reason is in his language rather than in
    /// an exit code. **The audio is KEPT** and the row offers a retry: a
    /// failure that deleted the recording would be the one unrecoverable
    /// outcome in this whole path.
    Failed(String),
}

/// One entry in the client's queue.
///
/// **A transcribed audio entry is deliberately INDISTINGUISHABLE from a typed
/// one in the payload.** His ruling: once transcribed they look exactly like
/// typed messages except for a play button. So [`Composer::payload`] reads
/// `text` and never asks where it came from, and the only thing `audio` changes
/// is that the row can be played back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingItem {
    /// What goes into the payload. Empty until an audio entry transcribes.
    pub text: String,
    /// The capture this entry came from, if it came from the microphone.
    /// `None` for anything he typed.
    pub audio: Option<String>,
    pub state: ItemState,
}

impl PendingItem {
    pub fn typed(text: impl Into<String>) -> Self {
        PendingItem {
            text: text.into(),
            audio: None,
            state: ItemState::Typed,
        }
    }

    /// A capture that has been taken and is on its way to the transcriber.
    pub fn recorded(path: impl Into<String>) -> Self {
        PendingItem {
            text: String::new(),
            audio: Some(path.into()),
            state: ItemState::Transcribing,
        }
    }

    /// Whether this entry can go out. **A queue holding one entry that is still
    /// transcribing cannot be sent**, because the payload would silently omit
    /// something he put in it.
    pub fn is_ready(&self) -> bool {
        matches!(self.state, ItemState::Typed | ItemState::Ready)
    }

    /// Whether there is a capture behind this row to play.
    pub fn is_playable(&self) -> bool {
        self.audio.is_some()
    }

    /// What the queue row shows for this entry.
    pub fn row_text(&self) -> String {
        match &self.state {
            ItemState::Typed | ItemState::Ready => self.text.clone(),
            ItemState::Transcribing => "transcribing…".to_string(),
            ItemState::Failed(why) => why.clone(),
        }
    }
}

/// **WHETHER THE WHOLE QUEUE CAN GO OUT, AND WHY NOT IF IT CANNOT.**
///
/// `payload()` used to answer "nothing to send" and "here it is" and there was
/// no third answer to give. Mic mode adds one: an entry that is still becoming
/// text. Collapsing that into `None` would have made `enter` say *"nothing to
/// send"* over a queue with three things in it, which is a face lying about its
/// own contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// There is nothing queued and nothing in the buffer.
    Nothing,
    /// `n` entries are still transcribing. Sending now would drop them.
    Transcribing(usize),
    /// `n` entries failed and are waiting to be retried or taken out. Sending
    /// now would silently omit words he spoke.
    Failed(usize),
    Ready(String),
}

impl Default for Composer {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: Pos::default(),
            goal: None,
            undo: Vec::new(),
            redo: Vec::new(),
            run_open: false,
            run_kind: None,
            yanked: String::new(),
            pending: Vec::new(),
        }
    }
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A composer holding exactly this text, cursor at the end. For restoring a
    /// draft and for tests; it carries NO undo history, because a history over
    /// edits that happened in a previous process is a history that can undo
    /// into a state he never saw.
    pub fn from_text(text: &str) -> Self {
        let mut c = Self::new();
        c.lines = split_lines(&sanitize(text));
        c.cursor = Pos::new(c.lines.len() - 1, c.lines[c.lines.len() - 1].len());
        c
    }

    // ── reading ──────────────────────────────────────────────────────────

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Whether there is anything to send. Whitespace alone is NOT something to
    /// send: a stray space that fired a turn would be a message he did not
    /// write, which is the same family as the paste defect.
    pub fn is_blank(&self) -> bool {
        self.lines.iter().all(|l| l.trim().is_empty())
    }

    pub fn pending(&self) -> &[PendingItem] {
        &self.pending
    }

    /// How many entries are still becoming text.
    pub fn transcribing(&self) -> usize {
        self.pending
            .iter()
            .filter(|p| p.state == ItemState::Transcribing)
            .count()
    }

    /// How many entries failed to become text and are waiting for him.
    pub fn failed(&self) -> usize {
        self.pending
            .iter()
            .filter(|p| matches!(p.state, ItemState::Failed(_)))
            .count()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    // ── layout: the ONE function that both draws and locates ──────────────

    /// **Every visual row of the whole buffer, in order.**
    ///
    /// Greedy word wrap that breaks at the last space in the row when there is
    /// one, and hard-breaks a word longer than the line — a URL that does not
    /// fit still has to be readable. Rows TILE their logical line: consecutive
    /// ranges, no gap, so every byte has exactly one screen home and the caret
    /// is a total function. A line with no text still produces one (empty) row,
    /// so a caller may always index row 0.
    pub fn layout(&self, width: usize) -> Vec<VisualRow> {
        let mut out = Vec::new();
        for (i, line) in self.lines.iter().enumerate() {
            wrap_line(i, line, width, &mut out);
        }
        out
    }

    /// Visual rows for ONE logical line.
    pub fn layout_line(&self, line: usize, width: usize) -> Vec<VisualRow> {
        let mut out = Vec::new();
        let s = self.lines.get(line).map(String::as_str).unwrap_or("");
        wrap_line(line, s, width, &mut out);
        out
    }

    /// The text one visual row paints.
    pub fn row_text(&self, row: VisualRow) -> &str {
        self.lines
            .get(row.line)
            .and_then(|l| l.get(row.start..row.end))
            .unwrap_or("")
    }

    /// **WHERE THE CARET IS, answered by the SAME function that lays out the
    /// rows.** `(row index into [`Composer::layout`], display column)`.
    ///
    /// There is no cached table for this to disagree with — helix's rule, and
    /// it is structural rather than careful: wrap logic and cursor logic cannot
    /// drift because they are literally the same code.
    pub fn caret(&self, width: usize) -> (usize, usize) {
        let rows = self.layout(width);
        let (i, row) = self.caret_row(&rows);
        let text = self.row_text(row);
        let upto = self.cursor.byte.saturating_sub(row.start).min(text.len());
        (i, str_width(&text[..snap(text, upto)]))
    }

    /// The row the caret sits on, and its index. The caret at a soft-wrap
    /// boundary belongs to the START of the FOLLOWING row — which is where the
    /// next character he types will actually appear — except at the end of a
    /// logical line, where there is no following row to belong to.
    fn caret_row(&self, rows: &[VisualRow]) -> (usize, VisualRow) {
        let mut last: Option<(usize, VisualRow)> = None;
        for (i, r) in rows.iter().enumerate() {
            if r.line != self.cursor.line {
                continue;
            }
            if self.cursor.byte < r.end {
                return (i, *r);
            }
            last = Some((i, *r));
        }
        last.unwrap_or((
            0,
            VisualRow {
                line: self.cursor.line,
                start: 0,
                end: 0,
            },
        ))
    }

    /// How many rows the buffer occupies at this width.
    pub fn visual_rows(&self, width: usize) -> usize {
        self.layout(width).len()
    }

    // ── the undo machinery ───────────────────────────────────────────────

    fn push_undo(&mut self) {
        self.undo.push(Snap {
            lines: self.lines.clone(),
            cursor: self.cursor,
        });
        while self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
        // Bytes as well as entries — see UNDO_BYTES for why one ceiling cannot
        // do both jobs.
        let mut bytes: usize = self
            .undo
            .iter()
            .map(|s| s.lines.iter().map(|l| l.len() + 1).sum::<usize>())
            .sum();
        while bytes > UNDO_BYTES && self.undo.len() > 1 {
            let dropped = &self.undo[0];
            bytes -= dropped.lines.iter().map(|l| l.len() + 1).sum::<usize>();
            self.undo.remove(0);
        }
    }

    fn open_run(&mut self, kind: EditKind) {
        let fresh = !self.run_open || self.run_kind != Some(kind) || kind.always_breaks();
        if fresh {
            self.push_undo();
            self.run_open = true;
            self.run_kind = Some(kind);
        }
        // A new edit invalidates the redo branch. This is the ordinary
        // linear-history rule, and it is what keeps undo∘redo an involution.
        self.redo.clear();
    }

    /// **CLOSE THE CURRENT EDIT RUN.** Called on paste, on a newline, on any
    /// cursor move that is not the natural advance of typing, on send, on queue,
    /// and by the shell after 400ms of idle.
    pub fn close_run(&mut self) {
        self.run_open = false;
        self.run_kind = None;
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        self.redo.push(Snap {
            lines: self.lines.clone(),
            cursor: self.cursor,
        });
        self.lines = prev.lines;
        self.cursor = prev.cursor;
        self.close_run();
        self.heal();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(Snap {
            lines: self.lines.clone(),
            cursor: self.cursor,
        });
        self.lines = next.lines;
        self.cursor = next.cursor;
        self.close_run();
        self.heal();
        true
    }

    /// Re-establish every invariant: at least one line, a cursor inside the
    /// buffer, and a byte on a cluster boundary. Every mutation and every
    /// motion in this file ends here.
    fn heal(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor.line = self.cursor.line.min(self.lines.len() - 1);
        let line = &self.lines[self.cursor.line];
        self.cursor.byte = snap(line, self.cursor.byte.min(line.len()));
    }

    // ── edits ────────────────────────────────────────────────────────────

    /// Type one character. Control characters cannot arrive here — [`sanitize`]
    /// is the only door and it drops them — but the call is routed through it
    /// anyway so there is exactly one door and not one door plus a promise.
    pub fn insert_char(&mut self, c: char) {
        self.open_run(EditKind::Type);
        self.insert_sanitized(&sanitize(&c.to_string()));
        self.goal = None;
    }

    /// **A PASTE: ONE INSERT, ONE UNDO STEP, NORMALIZED ON THE WAY IN.**
    ///
    /// The run is closed on both sides, which is what makes a ten-thousand-line
    /// paste undo as a single operation rather than as ten thousand.
    pub fn insert_paste(&mut self, s: &str) {
        self.close_run();
        self.open_run(EditKind::Paste);
        self.insert_sanitized(&sanitize(s));
        self.close_run();
        self.goal = None;
    }

    /// **REPLACE A SPAN OF ONE LINE, AS ONE UNDO STEP.** Bytes `range` of
    /// logical line `line` become `with` (normalized as a paste is), and the
    /// cursor lands after what went in. A range off the buffer, or not on
    /// cluster boundaries, is snapped inside it rather than trusted. For a
    /// caller that swaps one span for another (a reference chip for the one
    /// clicked next) without walking the cursor there by keys.
    pub fn replace_span(&mut self, line: usize, range: std::ops::Range<usize>, with: &str) {
        if line >= self.lines.len() {
            return;
        }
        self.close_run();
        self.open_run(EditKind::Paste);
        let l = &self.lines[line];
        let end = snap(l, range.end.min(l.len()));
        let start = snap(l, range.start.min(end));
        self.lines[line].replace_range(start..end, "");
        self.cursor = Pos::new(line, start);
        self.heal();
        self.insert_sanitized(&sanitize(with));
        self.close_run();
        self.goal = None;
    }

    /// Put the cursor at `pos`, snapped onto the buffer and a cluster
    /// boundary. A motion: it closes the run.
    pub fn set_cursor(&mut self, pos: Pos) {
        self.close_run();
        self.goal = None;
        self.cursor = pos;
        self.heal();
    }

    /// A newline the user asked for. Closes the run: a paragraph break is where
    /// "undo that" naturally stops.
    pub fn newline(&mut self) {
        self.close_run();
        self.open_run(EditKind::Type);
        self.insert_sanitized("\n");
        self.close_run();
        self.goal = None;
    }

    fn insert_sanitized(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let Pos { line, byte } = self.cursor;
        let tail = self.lines[line].split_off(byte);
        let mut parts = s.split('\n');
        let first = parts.next().unwrap_or("");
        self.lines[line].push_str(first);
        let rest: Vec<&str> = parts.collect();
        if rest.is_empty() {
            self.cursor.byte = self.lines[line].len();
            self.lines[line].push_str(&tail);
        } else {
            let mut new_lines: Vec<String> = rest.iter().map(|s| (*s).to_string()).collect();
            let last = new_lines.len() - 1;
            self.cursor.line = line + 1 + last;
            self.cursor.byte = new_lines[last].len();
            new_lines[last].push_str(&tail);
            for (k, l) in new_lines.into_iter().enumerate() {
                self.lines.insert(line + 1 + k, l);
            }
        }
        self.heal();
    }

    /// Delete the cluster BEFORE the cursor, or join with the previous line.
    pub fn backspace(&mut self) {
        if self.cursor.byte == 0 && self.cursor.line == 0 {
            return;
        }
        self.open_run(EditKind::Delete);
        if self.cursor.byte == 0 {
            let cur = self.lines.remove(self.cursor.line);
            self.cursor.line -= 1;
            self.cursor.byte = self.lines[self.cursor.line].len();
            self.lines[self.cursor.line].push_str(&cur);
        } else {
            let line = &self.lines[self.cursor.line];
            let prev = prev_boundary(line, self.cursor.byte);
            self.lines[self.cursor.line].replace_range(prev..self.cursor.byte, "");
            self.cursor.byte = prev;
        }
        self.goal = None;
        self.heal();
    }

    /// Delete the cluster AFTER the cursor, or pull the next line up.
    pub fn delete(&mut self) {
        let line_len = self.lines[self.cursor.line].len();
        if self.cursor.byte >= line_len && self.cursor.line + 1 >= self.lines.len() {
            return;
        }
        self.open_run(EditKind::Delete);
        if self.cursor.byte >= line_len {
            let next = self.lines.remove(self.cursor.line + 1);
            self.lines[self.cursor.line].push_str(&next);
        } else {
            let line = &self.lines[self.cursor.line];
            let next = next_boundary(line, self.cursor.byte);
            self.lines[self.cursor.line].replace_range(self.cursor.byte..next, "");
        }
        self.goal = None;
        self.heal();
    }

    /// `^K` — take the rest of the LOGICAL line, or join the next one when
    /// already at the end.
    pub fn kill_to_line_end(&mut self) {
        self.close_run();
        self.open_run(EditKind::Kill);
        let line_len = self.lines[self.cursor.line].len();
        if self.cursor.byte >= line_len {
            if self.cursor.line + 1 < self.lines.len() {
                let next = self.lines.remove(self.cursor.line + 1);
                self.lines[self.cursor.line].push_str(&next);
                self.yanked = "\n".into();
            }
        } else {
            self.yanked = self.lines[self.cursor.line].split_off(self.cursor.byte);
        }
        self.close_run();
        self.goal = None;
        self.heal();
    }

    /// `^U` — take everything before the cursor on the logical line.
    pub fn kill_to_line_start(&mut self) {
        if self.cursor.byte == 0 {
            return;
        }
        self.close_run();
        self.open_run(EditKind::Kill);
        let head: String = self.lines[self.cursor.line][..self.cursor.byte].into();
        self.lines[self.cursor.line].replace_range(..self.cursor.byte, "");
        self.yanked = head;
        self.cursor.byte = 0;
        self.close_run();
        self.goal = None;
        self.heal();
    }

    /// `^W` — take the word before the cursor.
    pub fn kill_word_back(&mut self) {
        let to = self.cursor.byte;
        let from = word_start(&self.lines[self.cursor.line], to);
        if from == to {
            // At the start of a line, `^W` joins upwards exactly as backspace
            // does. Anything else is a key that sometimes does nothing.
            self.backspace();
            return;
        }
        self.close_run();
        self.open_run(EditKind::Kill);
        self.yanked = self.lines[self.cursor.line][from..to].into();
        self.lines[self.cursor.line].replace_range(from..to, "");
        self.cursor.byte = from;
        self.close_run();
        self.goal = None;
        self.heal();
    }

    /// `^Y` — put back whatever the last kill took.
    pub fn yank(&mut self) {
        if self.yanked.is_empty() {
            return;
        }
        let y = self.yanked.clone();
        self.close_run();
        self.open_run(EditKind::Paste);
        self.insert_sanitized(&y);
        self.close_run();
        self.goal = None;
    }

    /// Empty the buffer and hand back what was in it. The undo history goes
    /// with it: a history that could undo INTO a message already sent is a
    /// history that can put his words back in a box they have left.
    pub fn take(&mut self) -> String {
        let text = self.text();
        *self = Composer {
            pending: std::mem::take(&mut self.pending),
            ..Composer::new()
        };
        text
    }

    // ── the queue, and the payload ───────────────────────────────────────

    /// **QUEUE what is composed.** His `left`. The buffer empties, the entry
    /// joins the queue, and NOTHING goes to the daemon.
    ///
    /// Returns false when there was nothing to queue, so the caller can say so
    /// rather than silently doing nothing.
    pub fn queue(&mut self) -> bool {
        if self.is_blank() {
            return false;
        }
        let text = self.text();
        // Undo cannot reach across a queue: the text has left the buffer and is
        // visible somewhere else, so `^Z` putting it back would DUPLICATE it.
        self.lines = vec![String::new()];
        self.cursor = Pos::default();
        self.undo.clear();
        self.redo.clear();
        self.close_run();
        self.pending.push(PendingItem::typed(text.trim_end()));
        true
    }

    /// **QUEUE A CAPTURE**, before it has any text at all.
    ///
    /// Returns the index of the new entry, which is the handle the shell holds
    /// while the transcriber runs. The entry is in the queue IMMEDIATELY —
    /// visible, counted, and saying it is transcribing — rather than appearing
    /// once the text arrives: a recording that vanished for a second and came
    /// back as words is a face he cannot tell from one that dropped it.
    pub fn queue_audio(&mut self, path: impl Into<String>) -> usize {
        self.pending.push(PendingItem::recorded(path));
        self.pending.len() - 1
    }

    /// **THE TRANSCRIBER CAME BACK.** `Ok` makes the row text, `Err` makes it a
    /// failure in his language — and either way the audio stays.
    ///
    /// Addressed by the entry's own `audio` path rather than by index, because
    /// an index is not stable: `^K` can take an entry back while a
    /// transcription is in flight, and an index resolved afterwards would land
    /// on somebody else's row. This is the same lesson `RowKey` carries for the
    /// transcript.
    pub fn resolve_audio(&mut self, path: &str, result: Result<String, String>) -> bool {
        let Some(item) = self
            .pending
            .iter_mut()
            .find(|p| p.audio.as_deref() == Some(path) && p.state == ItemState::Transcribing)
        else {
            return false;
        };
        match result {
            Ok(text) if !text.trim().is_empty() => {
                item.text = text.trim().to_string();
                item.state = ItemState::Ready;
            }
            Ok(_) => item.state = ItemState::Failed("the transcript came back empty".into()),
            Err(why) => item.state = ItemState::Failed(why),
        }
        true
    }

    /// Put a failed entry back into `Transcribing` so a retry can run.
    pub fn retry_audio(&mut self, path: &str) -> bool {
        let Some(item) = self
            .pending
            .iter_mut()
            .find(|p| p.audio.as_deref() == Some(path) && matches!(p.state, ItemState::Failed(_)))
        else {
            return false;
        };
        item.state = ItemState::Transcribing;
        true
    }

    /// Take one entry out of the queue by its capture path. The audio file is
    /// the shell's to delete; this only forgets the row.
    pub fn drop_audio(&mut self, path: &str) -> bool {
        let before = self.pending.len();
        self.pending.retain(|p| p.audio.as_deref() != Some(path));
        before != self.pending.len()
    }

    /// The capture behind queue row `i`, if it has one.
    pub fn audio_at(&self, i: usize) -> Option<&str> {
        self.pending.get(i)?.audio.as_deref()
    }

    /// **EVERY CAPTURE THE QUEUE IS CURRENTLY HOLDING.**
    ///
    /// The shell reads this before clearing the queue on an accepted send, so
    /// the files can be deleted. Once the rows are gone nothing in the face can
    /// reach those recordings — there is no play button left — and an
    /// unreachable file that is never cleaned up is his voice accumulating on
    /// disk forever.
    pub fn audio_paths(&self) -> Vec<String> {
        self.pending
            .iter()
            .filter_map(|p| p.audio.clone())
            .collect()
    }

    /// Put a queued entry back at the front of the buffer. The undo of a queue.
    ///
    /// **An AUDIO entry is never unqueued into the text buffer.** Its words are
    /// not the row's identity — the recording is — and dropping a transcript
    /// into the box would leave the capture orphaned with no row to play it
    /// from. `^K` therefore skips over audio entries and takes the last
    /// TYPED one back, which is the act it has always been.
    pub fn unqueue(&mut self) -> bool {
        let Some(at) = self.pending.iter().rposition(|p| p.audio.is_none()) else {
            return false;
        };
        let last = self.pending.remove(at).text;
        if self.is_blank() {
            *self = Composer {
                pending: std::mem::take(&mut self.pending),
                ..Composer::from_text(&last)
            };
        } else {
            let rest = self.text();
            let pending = std::mem::take(&mut self.pending);
            *self = Composer {
                pending,
                ..Composer::from_text(&format!("{last}\n\n{rest}"))
            };
        }
        true
    }

    /// Discard a queued entry outright.
    pub fn clear_queue(&mut self) -> usize {
        let n = self.pending.len();
        self.pending.clear();
        n
    }

    /// **THE WHOLE PAYLOAD.** His `wildcard`: every queued entry plus whatever
    /// is in the buffer, as ONE message.
    ///
    /// With nothing queued it is the buffer, verbatim — a lone message must not
    /// arrive wearing a header that says it was one of several. With entries
    /// queued it is the AUI's own `Queued messages:` block, which is the shape
    /// the agent on the other end already knows how to read
    /// (`apps/aui/src/dispatch/thread.rs::build_queued_block`), so a stitched
    /// turn reads as several things said and not as one run-on.
    ///
    /// `None` when there is nothing to send — **or when something queued is not
    /// ready yet**, which is [`Composer::payload_state`]'s third answer. Kept
    /// as the thin wrapper so every existing caller and every existing test
    /// reads exactly as it did.
    pub fn payload(&self) -> Option<String> {
        match self.payload_state() {
            Payload::Ready(s) => Some(s),
            _ => None,
        }
    }

    /// **THE WHOLE PAYLOAD, OR THE REASON THERE ISN'T ONE.**
    ///
    /// **A queue with an entry still transcribing DOES NOT SEND.** That is the
    /// load-bearing half: stitching the ready entries and going would silently
    /// drop something he spoke, and a dropped message that nothing reports is
    /// the worst outcome this whole path has. Same for a failed entry — it is
    /// still his, it is still in the queue, and it goes out or comes out by his
    /// decision and not by the send path's silence.
    pub fn payload_state(&self) -> Payload {
        let here = self.text();
        let here = here.trim_end();
        let waiting = self.transcribing();
        let failed = self.failed();
        if waiting > 0 {
            return Payload::Transcribing(waiting);
        }
        if failed > 0 {
            return Payload::Failed(failed);
        }
        let mut parts: Vec<&str> = self.pending.iter().map(|p| p.text.as_str()).collect();
        if !here.trim().is_empty() {
            parts.push(here);
        }
        parts.retain(|t| !t.trim().is_empty());
        match parts.len() {
            0 => Payload::Nothing,
            1 => Payload::Ready(parts[0].to_string()),
            _ => {
                let body: Vec<String> = parts
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("Message {}:\n{}", i + 1, t.trim()))
                    .collect();
                Payload::Ready(format!("Queued messages:\n{}", body.join("\n\n")))
            }
        }
    }

    /// Everything goes: the buffer, its history and the queue. Called once a
    /// send has been ACCEPTED by the daemon and not one moment before.
    pub fn accepted(&mut self) {
        *self = Composer::new();
    }

    // ── motions ──────────────────────────────────────────────────────────

    /// One grapheme cluster left, crossing a line boundary at the start.
    pub fn left(&mut self) {
        self.close_run();
        self.goal = None;
        if self.cursor.byte == 0 {
            if self.cursor.line == 0 {
                return;
            }
            self.cursor.line -= 1;
            self.cursor.byte = self.lines[self.cursor.line].len();
        } else {
            self.cursor.byte = prev_boundary(&self.lines[self.cursor.line], self.cursor.byte);
        }
        self.heal();
    }

    /// One grapheme cluster right, crossing a line boundary at the end.
    pub fn right(&mut self) {
        self.close_run();
        self.goal = None;
        let len = self.lines[self.cursor.line].len();
        if self.cursor.byte >= len {
            if self.cursor.line + 1 >= self.lines.len() {
                return;
            }
            self.cursor.line += 1;
            self.cursor.byte = 0;
        } else {
            self.cursor.byte = next_boundary(&self.lines[self.cursor.line], self.cursor.byte);
        }
        self.heal();
    }

    pub fn word_left(&mut self) {
        self.close_run();
        self.goal = None;
        if self.cursor.byte == 0 {
            self.left();
            return;
        }
        self.cursor.byte = word_start(&self.lines[self.cursor.line], self.cursor.byte);
        self.heal();
    }

    pub fn word_right(&mut self) {
        self.close_run();
        self.goal = None;
        let len = self.lines[self.cursor.line].len();
        if self.cursor.byte >= len {
            self.right();
            return;
        }
        self.cursor.byte = word_end(&self.lines[self.cursor.line], self.cursor.byte);
        self.heal();
    }

    /// The start of the VISUAL row — what `home` means on a wrapped line.
    pub fn row_start(&mut self, width: usize) {
        self.close_run();
        self.goal = None;
        let rows = self.layout(width);
        let (_, row) = self.caret_row(&rows);
        self.cursor.byte = row.start;
        self.heal();
    }

    /// The end of the VISUAL row.
    pub fn row_end(&mut self, width: usize) {
        self.close_run();
        self.goal = None;
        let rows = self.layout(width);
        let (i, row) = self.caret_row(&rows);
        // The last row of a logical line ends at the line's end; an earlier row
        // ends one cluster short of the break, so `end` then `→` does not skip
        // over the character the break stands on.
        let last_of_line = rows.get(i + 1).map(|r| r.line != row.line).unwrap_or(true);
        self.cursor.byte = if last_of_line {
            row.end
        } else {
            prev_boundary(&self.lines[row.line], row.end)
        };
        self.heal();
    }

    /// **PUT THE CARET AT A VISUAL ROW AND A DISPLAY COLUMN** — his rule for a
    /// click inside the composer, verbatim: *"to that position, or the nearest
    /// legal one."*
    ///
    /// `row` indexes [`Composer::layout`] at this width; anything past the last
    /// row lands on the last row, because a coordinate can name a band the
    /// buffer has since shrunk out of. The column is resolved by
    /// [`Composer::byte_at`], which is the SAME walk `↑`/`↓` use to honour a
    /// goal column — so a click and a vertical motion cannot come to disagree
    /// about which character a column names.
    ///
    /// **"Nearest legal" has three parts and all three are here**: snapped to a
    /// grapheme boundary (a click in the middle of an emoji lands on one side
    /// of it, never inside), clamped to the row's end (a click past the last
    /// character lands after it, which is where he meant), and clamped to the
    /// row the pointer was on (a click below the text lands on the last row).
    ///
    /// The goal column is CLEARED, exactly as every other horizontal motion
    /// clears it: he has just said where he wants to be.
    pub fn place(&mut self, row: usize, col: usize, width: usize) {
        let rows = self.layout(width);
        let Some(dest) = rows.get(row.min(rows.len().saturating_sub(1))).copied() else {
            return;
        };
        self.close_run();
        self.goal = None;
        self.cursor.line = dest.line;
        self.cursor.byte = self.byte_at(dest, col);
        self.heal();
    }

    /// **THE ONE COLUMN → BYTE WALK**, used by [`Composer::place`] and by the
    /// goal-column arithmetic in [`Composer::vertical`].
    ///
    /// Returns a byte offset into the LINE (not into the row), already clamped
    /// to the row's end. It is one function because it was very nearly two: a
    /// click that resolved a column one way and `↓` that resolved it another
    /// would put the caret in two different places for the same column, and the
    /// screen would look right in both cases.
    fn byte_at(&self, row: VisualRow, col: usize) -> usize {
        let text = self.row_text(row);
        let mut at = 0usize;
        let mut width = 0usize;
        for (i, g) in text.grapheme_indices(true) {
            if width >= col {
                break;
            }
            width += cluster_width(g);
            at = i + g.len();
        }
        row.start + at.min(text.len())
    }

    pub fn buffer_start(&mut self) {
        self.close_run();
        self.goal = None;
        self.cursor = Pos::default();
        self.heal();
    }

    pub fn buffer_end(&mut self) {
        self.close_run();
        self.goal = None;
        self.cursor.line = self.lines.len() - 1;
        self.cursor.byte = self.lines[self.cursor.line].len();
        self.heal();
    }

    /// **TO THE TOP OF THIS PARAGRAPH, OR THE TOP OF THE PREVIOUS ONE.**
    ///
    /// **A PARAGRAPH IS ONE HARD-BREAK LOGICAL LINE** — one `lines[i]`,
    /// regardless of how many visual rows it wraps over. That is what makes
    /// `opt-↑` different from `↑` and is the whole reason the key exists: on a
    /// long line wrapped over three rows, `↑` climbs one row and this goes to
    /// the line's start in one press. The alternative reading (blank-line
    /// delimited, the Emacs one) would collapse most chat drafts — where every
    /// newline is a deliberate `alt-enter` — to a single paragraph.
    ///
    /// **A RUN OF BLANK LINES IS ONE BOUNDARY, NOT SEVERAL** — the round-five
    /// refinement, in his words: *"opt-{up,down} should skip over chunks of
    /// newlines. I usually do \n\n and I want to skip the empty line."* The
    /// rule above is unchanged (a paragraph is still a hard-break LINE, never a
    /// blank-line block, so a wrapped line is still one press); what changed is
    /// that the motion does not COME TO REST on an empty line it stepped onto.
    /// It is one amendment to the one rule, not a second rule beside it.
    ///
    /// **IT NEVER ESCAPES THE BUFFER**, which is the difference between it and
    /// [`Composer::up`]: his callout says these *"stop at the beginning and
    /// end, they should not escape the textarea."* `false` means it had
    /// nowhere to go, and the shell does nothing with that — it is returned so
    /// a test can tell "stopped" from "moved".
    pub fn para_up(&mut self) -> bool {
        self.close_run();
        self.goal = None;
        if self.cursor.byte > 0 {
            self.cursor.byte = 0;
        } else if self.cursor.line > 0 {
            self.cursor.line -= 1;
            self.cursor.byte = 0;
            // Walk the whole run of blanks in one press. The buffer's first
            // line is the floor: a draft that begins with blank lines stops
            // there rather than reporting that it could not move.
            while self.cursor.line > 0 && self.lines[self.cursor.line].is_empty() {
                self.cursor.line -= 1;
            }
        } else {
            return false;
        }
        self.heal();
        true
    }

    /// **TO THE END OF THIS PARAGRAPH, OR THE END OF THE NEXT ONE.** The mirror
    /// of [`Composer::para_up`], blank-line run and all, and it never escapes
    /// either.
    pub fn para_down(&mut self) -> bool {
        self.close_run();
        self.goal = None;
        let end = self.lines[self.cursor.line].len();
        if self.cursor.byte < end {
            self.cursor.byte = end;
        } else if self.cursor.line + 1 < self.lines.len() {
            self.cursor.line += 1;
            while self.cursor.line + 1 < self.lines.len()
                && self.lines[self.cursor.line].is_empty()
            {
                self.cursor.line += 1;
            }
            self.cursor.byte = self.lines[self.cursor.line].len();
        } else {
            return false;
        }
        self.heal();
        true
    }

    /// **UP ONE VISUAL ROW, keeping the goal column.** `false` when the caret
    /// is already on the first row, which is how the shell knows to give the
    /// gesture to the transcript instead.
    pub fn up(&mut self, width: usize) -> bool {
        self.vertical(width, true)
    }

    /// Down one visual row. `false` on the last row.
    pub fn down(&mut self, width: usize) -> bool {
        self.vertical(width, false)
    }

    fn vertical(&mut self, width: usize, up: bool) -> bool {
        let rows = self.layout(width);
        let (i, row) = self.caret_row(&rows);
        let target = if up {
            if i == 0 {
                return false;
            }
            i - 1
        } else {
            if i + 1 >= rows.len() {
                return false;
            }
            i + 1
        };
        // The GOAL COLUMN is set on the first vertical move of a run and kept
        // for the rest of it, so walking up through short lines and back down
        // returns to the column he started in. It is a HINT: any horizontal
        // edit or motion clears it.
        let goal = match self.goal {
            Some(g) => g,
            None => {
                let text = self.row_text(row);
                let upto = self.cursor.byte.saturating_sub(row.start).min(text.len());
                let g = str_width(&text[..snap(text, upto)]);
                self.goal = Some(g);
                g
            }
        };
        let dest = rows[target];
        // The SAME walk a click uses — see `byte_at`. It was written out here
        // once and copied nowhere, and now it is written out nowhere and called
        // twice.
        let byte = self.byte_at(dest, goal);
        self.close_run();
        self.cursor.line = dest.line;
        self.cursor.byte = byte;
        // `heal` snaps; the goal SURVIVES, which is the whole point of it being
        // stored at all.
        let goal = self.goal;
        self.heal();
        self.goal = goal;
        true
    }
}

// ── free functions the model is built out of ────────────────────────────────

fn split_lines(s: &str) -> Vec<String> {
    let v: Vec<String> = s.split('\n').map(str::to_string).collect();
    if v.is_empty() {
        vec![String::new()]
    } else {
        v
    }
}

fn prev_boundary(s: &str, byte: usize) -> usize {
    let b = snap(s, byte);
    if b == 0 {
        return 0;
    }
    let mut last = 0usize;
    for (i, _) in s.grapheme_indices(true) {
        if i >= b {
            return last;
        }
        last = i;
    }
    last
}

fn next_boundary(s: &str, byte: usize) -> usize {
    let b = snap(s, byte);
    for (i, g) in s.grapheme_indices(true) {
        if i == b {
            return i + g.len();
        }
        if i > b {
            return i;
        }
    }
    s.len()
}

/// Whether a cluster counts as part of a WORD. Anything that is not whitespace
/// is, which is the readline rule and the one that surprises least in prose.
fn wordish(g: &str) -> bool {
    !g.chars().all(char::is_whitespace)
}

/// The start of the word ending at `byte`: back over whitespace, then back over
/// the word.
fn word_start(s: &str, byte: usize) -> usize {
    let b = snap(s, byte);
    let gs: Vec<(usize, &str)> = s.grapheme_indices(true).filter(|(i, _)| *i < b).collect();
    let mut k = gs.len();
    while k > 0 && !wordish(gs[k - 1].1) {
        k -= 1;
    }
    while k > 0 && wordish(gs[k - 1].1) {
        k -= 1;
    }
    gs.get(k).map(|(i, _)| *i).unwrap_or(0)
}

/// The end of the word starting at `byte`: forward over whitespace, then
/// forward over the word.
fn word_end(s: &str, byte: usize) -> usize {
    let b = snap(s, byte);
    let gs: Vec<(usize, &str)> = s.grapheme_indices(true).filter(|(i, _)| *i >= b).collect();
    let mut k = 0usize;
    while k < gs.len() && !wordish(gs[k].1) {
        k += 1;
    }
    while k < gs.len() && wordish(gs[k].1) {
        k += 1;
    }
    gs.get(k).map(|(i, _)| *i).unwrap_or(s.len())
}

/// Greedy word wrap for ONE logical line, appending TILING rows.
///
/// The contract every caller depends on: consecutive ranges with no gap, at
/// least one row, and every row no wider than `width` unless a single cluster
/// is wider than the whole line — in which case it gets a row to itself, which
/// is the only honest thing to do with it.
fn wrap_line(index: usize, s: &str, width: usize, out: &mut Vec<VisualRow>) {
    let width = width.max(1);
    if s.is_empty() {
        out.push(VisualRow {
            line: index,
            start: 0,
            end: 0,
        });
        return;
    }
    let gs: Vec<(usize, &str)> = s.grapheme_indices(true).collect();
    let mut k = 0usize;
    loop {
        let row_start = gs[k].0;
        let mut col = 0usize;
        // Cluster index just AFTER the last space that fitted in this row —
        // the word-wrap break point. `None` until one is seen.
        let mut brk: Option<usize> = None;
        let mut j = k;
        while j < gs.len() {
            let (bi, g) = gs[j];
            let w = cluster_width(g);
            // `bi > row_start` is what guarantees progress: the first cluster of
            // a row is always taken, even when it is wider than the whole band.
            // A two-cell glyph in a one-cell window has to go somewhere, and
            // the renderer's crop is the only honest place.
            if col + w > width && bi > row_start {
                break;
            }
            col += w;
            if g == " " {
                brk = Some(j + 1);
            }
            j += 1;
        }
        if j >= gs.len() {
            out.push(VisualRow {
                line: index,
                start: row_start,
                end: s.len(),
            });
            return;
        }
        // Break at the last space, unless doing so would leave a row holding
        // nothing but whitespace — which happens when the break that produced
        // THIS row landed on a space and the next word is wider than the band.
        let cut = match brk {
            Some(b) if b > k && b < j && !s[row_start..gs[b].0].trim().is_empty() => b,
            _ => j,
        };
        out.push(VisualRow {
            line: index,
            start: row_start,
            end: gs[cut].0,
        });
        // `cut` is strictly greater than `k` in both arms, so this terminates.
        k = cut;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A PARAGRAPH IS ONE LOGICAL LINE, AND `opt-↑` NEVER LEAVES THE BOX.**
    ///
    /// His callout: *"opt-up to the top of the current or top of the previous,
    /// opt-down to the bottom of the current or bottom of the next"*, and
    /// *"they should stop at the beginning and end, they should not escape the
    /// textarea."*
    ///
    /// The case that decides whether the definition was understood is the
    /// WRAPPED one: a line long enough to take three visual rows is still ONE
    /// paragraph, so `opt-↑` goes to its start in one press where `↑` would
    /// climb one row. If a builder implemented this against visual rows, the
    /// third assertion is what catches it.
    ///
    /// FORCED: make `para_up` call `self.up(width)` and the wrapped case lands
    /// on the second row instead of byte 0; delete the `else { return false }`
    /// and the buffer-start case walks off the top.
    #[test]
    fn a_paragraph_is_a_logical_line_and_the_motion_never_escapes() {
        let mut c = Composer::from_text("alpha
bravo
charlie");
        c.buffer_end();
        assert_eq!(c.cursor(), Pos::new(2, 7));
        // The top of the CURRENT paragraph first…
        assert!(c.para_up());
        assert_eq!(c.cursor(), Pos::new(2, 0));
        // …then the top of the PREVIOUS one, and one line at a time.
        assert!(c.para_up());
        assert_eq!(c.cursor(), Pos::new(1, 0));
        assert!(c.para_up());
        assert_eq!(c.cursor(), Pos::new(0, 0));
        // **AND IT STOPS THERE.** It does not escape and it does not wrap.
        assert!(!c.para_up(), "opt-↑ moved past the start of the buffer");
        assert_eq!(c.cursor(), Pos::new(0, 0));
        // Down is the mirror: end of this line, then end of the next.
        assert!(c.para_down());
        assert_eq!(c.cursor(), Pos::new(0, 5));
        assert!(c.para_down());
        assert_eq!(c.cursor(), Pos::new(1, 5));
        assert!(c.para_down());
        assert_eq!(c.cursor(), Pos::new(2, 7));
        assert!(!c.para_down(), "opt-↓ moved past the end of the buffer");

        // **THE WRAPPED LINE IS THE WHOLE POINT.** Sixty characters at a width
        // of twenty is one paragraph over three visual rows: `↑` climbs a row,
        // `opt-↑` takes the paragraph.
        let mut w = Composer::from_text(&"ab ".repeat(20));
        w.buffer_end();
        let (row, _) = w.caret(20);
        assert!(row >= 2, "the fixture did not wrap: {row}");
        assert!(w.up(20), "↑ should climb one visual row inside the line");
        assert_ne!(w.cursor().byte, 0, "↑ went to the start of the paragraph");
        assert!(w.para_up());
        assert_eq!(
            w.cursor(),
            Pos::new(0, 0),
            "opt-↑ did not take the whole wrapped paragraph in one press"
        );
        assert!(!w.para_up(), "a single wrapped line has nothing above it");
    }

    /// **A RUN OF NEWLINES IS ONE BOUNDARY** — *"opt-{up,down} should skip over
    /// chunks of newlines. I usually do \n\n and I want to skip the empty
    /// line."*
    ///
    /// This is an AMENDMENT to the rule the test above states, not a second
    /// rule: a paragraph is still a hard-break LINE (the wrapped case there is
    /// untouched), and what changed is that the motion does not come to REST on
    /// a blank line it stepped onto.
    ///
    /// FORCED, both directions, each watched red on its own: delete `para_up`'s
    /// `while` loop and the third `opt-↑` rests on line 2, the LOWER of the two
    /// blanks, instead of reaching `alpha`; delete `para_down`'s and the second
    /// `opt-↓` rests on line 1, the upper one. Two mutations and not one,
    /// because a single loop could otherwise have carried both directions.
    /// Delete the `cursor.line > 0` / `line + 1 < len` guard instead and the
    /// all-blank fixture at the end walks off its own end.
    #[test]
    fn a_run_of_blank_lines_is_one_stop_for_the_paragraph_keys() {
        // "alpha\n\n\nbravo\ncharlie" — a THREE-newline chunk, because his
        // `\n\n` is the smallest case and a rule that only handles one blank
        // line is the rule he already has.
        let mut c = Composer::from_text("alpha\n\n\nbravo\ncharlie");
        assert_eq!(c.lines().len(), 5, "the fixture is not five lines");
        c.buffer_end();
        assert_eq!(c.cursor(), Pos::new(4, 7));
        assert!(c.para_up());
        assert_eq!(c.cursor(), Pos::new(4, 0), "the head of the current line");
        assert!(c.para_up());
        assert_eq!(c.cursor(), Pos::new(3, 0), "the head of `bravo`");
        // **THE ONE THIS TEST EXISTS FOR.** Lines 1 and 2 are both empty; one
        // press clears the whole run and lands on `alpha`.
        assert!(c.para_up());
        assert_eq!(
            c.cursor(),
            Pos::new(0, 0),
            "opt-↑ stopped inside the run of blank lines"
        );
        assert!(!c.para_up(), "opt-↑ moved past the start of the buffer");

        // And down is the mirror.
        assert!(c.para_down());
        assert_eq!(c.cursor(), Pos::new(0, 5), "the end of `alpha`");
        assert!(c.para_down());
        assert_eq!(
            c.cursor(),
            Pos::new(3, 5),
            "opt-↓ stopped inside the run of blank lines"
        );

        // **A DRAFT THAT IS NOTHING BUT BLANK LINES STILL STOPS AT ITS ENDS**
        // rather than walking off either one.
        let mut b = Composer::from_text("\n\n\n");
        b.buffer_end();
        assert_eq!(b.cursor(), Pos::new(3, 0));
        assert!(b.para_up());
        assert_eq!(b.cursor(), Pos::new(0, 0), "the run's floor is line 0");
        assert!(!b.para_up());
        assert!(b.para_down());
        assert_eq!(b.cursor(), Pos::new(3, 0), "the run's ceiling is the last line");
        assert!(!b.para_down());
    }

    /// **A CLICK PUTS THE CARET WHERE HE POINTED** — *"to that position, or the
    /// nearest legal one."*
    ///
    /// FORCED: make `byte_at` return `row.start` unconditionally and every
    /// column assertion below goes red at once; drop the `heal` snap and the
    /// emoji case lands inside a cluster.
    #[test]
    fn a_click_lands_on_the_column_it_was_given_or_the_nearest_legal_one() {
        let mut c = Composer::from_text("alpha beta\ngamma");
        // Column 0 is before the first character; column 6 is the `b` of beta.
        c.place(0, 0, 40);
        assert_eq!(c.cursor(), Pos::new(0, 0));
        c.place(0, 6, 40);
        assert_eq!(c.cursor(), Pos::new(0, 6));
        assert_eq!(&c.lines()[0][c.cursor().byte..], "beta");
        // The SECOND logical line is the second laid row at this width.
        c.place(1, 3, 40);
        assert_eq!(c.cursor(), Pos::new(1, 3));

        // **PAST THE END OF THE ROW IS THE END OF THE ROW**, not an error and
        // not the next line — a click in the blank space to the right of a
        // short line means "after the last character", which is what every
        // other text box does.
        c.place(1, 500, 40);
        assert_eq!(c.cursor(), Pos::new(1, "gamma".len()));

        // **PAST THE LAST ROW IS THE LAST ROW.** A coordinate can name a band
        // the buffer has since shrunk out of.
        c.place(99, 0, 40);
        assert_eq!(c.cursor().line, 1);

        // Every column of every row is a legal position, which is the general
        // form of the three cases above.
        for row in 0..2 {
            for col in 0..60 {
                c.place(row, col, 40);
                let p = c.cursor();
                assert!(
                    c.lines()[p.line].is_char_boundary(p.byte),
                    "({row},{col}) landed mid-character"
                );
            }
        }
    }

    /// **A CLICK NEVER LANDS INSIDE A GRAPHEME CLUSTER**, which is the whole of
    /// "the nearest LEGAL position" on a line with an emoji in it: the caret
    /// goes to one side of the flag or the other, never between its two
    /// codepoints, where the next keystroke would corrupt it.
    #[test]
    fn a_click_in_the_middle_of_a_wide_cluster_lands_beside_it_and_never_inside() {
        let text = "a\u{1F1EC}\u{1F1E7}b";
        let mut c = Composer::from_text(text);
        let boundaries: Vec<usize> = {
            let mut v: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
            v.push(text.len());
            v
        };
        for col in 0..8 {
            c.place(0, col, 40);
            assert!(
                boundaries.contains(&c.cursor().byte),
                "column {col} landed at byte {} — inside a cluster",
                c.cursor().byte
            );
        }
    }

    /// **A CLICK AND `↓` RESOLVE A COLUMN THE SAME WAY**, because they are the
    /// same walk. This is what makes `byte_at` being ONE function load-bearing
    /// rather than tidy: two walks would put the caret in two different places
    /// for the same column, and the screen would look right both times.
    #[test]
    fn a_click_and_a_vertical_motion_agree_about_what_a_column_names() {
        for col in [0usize, 1, 3, 7, 40] {
            let mut clicked = Composer::from_text("alpha beta gamma\nzeta");
            clicked.place(1, col, 40);

            let mut moved = Composer::from_text("alpha beta gamma\nzeta");
            moved.place(0, col, 40);
            assert!(moved.down(40), "the fixture has a second row");

            assert_eq!(
                clicked.cursor(),
                moved.cursor(),
                "a click and down disagree about column {col}"
            );
        }
    }

    /// A click on a SOFT-WRAPPED row addresses the VISUAL row, not the logical
    /// line — the case a naive "row index is the line index" gets wrong the
    /// moment a draft is wider than the band.
    #[test]
    fn a_click_on_a_wrapped_row_addresses_the_visual_row() {
        let mut c = Composer::from_text("aaaa bbbb cccc dddd");
        let rows = c.layout(10);
        assert!(rows.len() >= 2, "the fixture must wrap: {rows:?}");
        c.place(1, 0, 10);
        assert_eq!(
            c.cursor().byte,
            rows[1].start,
            "a click on the second visual row landed on the first"
        );
    }

    /// The nasty corpus, named once so the unit tests and the golden corpus in
    /// `tests/nasty.rs` are the same list and cannot drift.
    pub const NASTY: &[(&str, &str)] = &[
        ("zwj family", "👩‍👩‍👧‍👦"),
        ("zwj profession", "👩‍💻"),
        ("skin tone", "👍🏽"),
        ("regional flag", "🇯🇵"),
        ("keycap", "1️⃣"),
        ("vs16", "❤️"),
        ("vs15", "❤︎"),
        ("combining", "e\u{301}\u{302}\u{303}"),
        ("conjoining jamo", "\u{1100}\u{1161}\u{11a8}"),
        ("cjk", "日本語"),
        ("rtl", "شسيب"),
        ("zwsp", "a\u{200b}b"),
        ("nerd font pua", "\u{f09b}"),
    ];

    #[test]
    fn snap_is_idempotent_and_never_lands_inside_a_cluster() {
        for (name, g) in NASTY {
            let s = format!("a{g}b");
            for i in 0..=s.len() {
                let once = snap(&s, i);
                assert_eq!(
                    once,
                    snap(&s, once),
                    "{name}: snap is not idempotent at {i}"
                );
                assert!(s.is_char_boundary(once), "{name}: snapped into a codepoint");
                assert!(
                    s.grapheme_indices(true).any(|(k, _)| k == once) || once == s.len(),
                    "{name}: {once} is not a cluster boundary of {s:?}"
                );
            }
        }
    }

    /// **A ZWJ FAMILY IS ONE CURSOR STOP.** reedline's own test, applied to our
    /// model: the emoji is eighteen bytes and `→` crosses it in one press.
    #[test]
    fn a_zwj_family_emoji_is_exactly_one_cursor_stop() {
        let mut c = Composer::from_text("👩‍👩‍👧‍👦");
        assert_eq!(c.lines()[0].len(), 25, "the buffer holds the whole cluster");
        c.buffer_start();
        c.right();
        assert_eq!(
            c.cursor(),
            Pos::new(0, 25),
            "→ did not cross the cluster whole"
        );
        c.left();
        assert_eq!(
            c.cursor(),
            Pos::new(0, 0),
            "← did not cross the cluster whole"
        );
        // …and one backspace takes all of it.
        c.buffer_end();
        c.backspace();
        assert_eq!(c.text(), "", "backspace left half an emoji behind");
    }

    #[test]
    fn a_tab_never_reaches_the_buffer_and_a_control_never_does_either() {
        let c = Composer::from_text("a\tb\u{0}c\u{7}d\u{feff}e\r\nf\rg");
        assert_eq!(c.text(), "a    bcde\nf\ng");
        assert!(!c.text().contains('\t'));
        assert!(!c.text().contains('\r'));
    }

    /// The rows of a line TILE it. Without this the caret has positions with no
    /// screen cell, which is a cursor that vanishes at a wrap point.
    #[test]
    fn the_wrap_rows_tile_their_line_at_every_width() {
        let texts = [
            "the quick brown fox jumps over the lazy dog",
            "supercalifragilisticexpialidocious",
            "日本語 の テキスト です",
            "👩‍👩‍👧‍👦 a 👍🏽 b ❤️ c",
            "",
            " leading and trailing  ",
        ];
        for t in texts {
            let c = Composer::from_text(t);
            for w in 1..=40usize {
                let rows = c.layout(w);
                assert!(!rows.is_empty(), "{t:?} at {w}: no rows");
                assert_eq!(rows[0].start, 0);
                for pair in rows.windows(2) {
                    assert_eq!(pair[0].end, pair[1].start, "{t:?} at {w}: rows do not tile");
                }
                assert_eq!(rows.last().unwrap().end, c.lines()[0].len());
            }
        }
    }

    /// **RENDERED WIDTH EQUALS COMPUTED WIDTH.** No row may be wider than the
    /// band it was laid out for — over-report is what makes ratatui panic.
    #[test]
    fn no_laid_out_row_is_wider_than_the_width_it_was_given() {
        let c = Composer::from_text(
            "the quick 👩‍👩‍👧‍👦 brown 日本語 fox ❤️ jumps supercalifragilisticexpialidocious",
        );
        for w in 1..=60usize {
            for row in c.layout(w) {
                let got = str_width(c.row_text(row));
                // The one legal exception, and it is the medium's rather than
                // ours: a two-cell cluster cannot be drawn in a one-cell band,
                // so it gets the row and the renderer crops it. `CELL_MAX` is
                // what makes that exception BOUNDED — without the clamp in
                // `cluster_width` a family emoji would put eight columns in a
                // five-column band, which is the ratatui panic.
                assert!(
                    got <= w.max(CELL_MAX),
                    "a row measured {got} in a band of {w}: {:?}",
                    c.row_text(row)
                );
            }
        }
    }

    /// **THE CLAMP, ON THE EXACT INPUT THAT NEEDS IT.** A later edit that
    /// removes `.min(CELL_MAX)` has to delete this.
    #[test]
    fn no_single_cluster_is_ever_measured_wider_than_a_cell_pair() {
        for (name, g) in NASTY {
            let w = cluster_width(g);
            assert!(
                (1..=CELL_MAX).contains(&w),
                "{name}: measured {w} columns — one glyph cannot occupy {w} cells"
            );
        }
        // **THE CONTRACT WITH `unicode-width`, pinned by measurement.** These
        // are the numbers the table we depend on actually returns today, read
        // off it rather than assumed. The clamp in `cluster_width` currently
        // changes none of them — which is worth knowing and is why it is
        // documented as a guard rather than as a fix — and a version bump that
        // moves any of them fails HERE, where it can be read, instead of
        // somewhere downstream where it is a panic.
        for (name, g, want) in [
            ("zwj family", "👩‍👩‍👧‍👦", 2usize),
            ("zwj profession", "👩‍💻", 2),
            ("skin tone", "👍🏽", 2),
            ("regional flag", "🇯🇵", 2),
            ("keycap", "1️⃣", 2),
            ("vs16", "❤️", 2),
            ("vs15", "❤︎", 1),
            ("three combining marks", "e\u{301}\u{302}\u{303}", 1),
            ("conjoining jamo", "\u{1100}\u{1161}\u{11a8}", 2),
            ("nerd font pua", "\u{f09b}", 1),
        ] {
            assert_eq!(
                UnicodeWidthStr::width(g),
                want,
                "{name}: the width table moved. This is a CONTRACT, not a \
                 preference — re-measure against the terminal before re-greening."
            );
            assert_eq!(
                cluster_width(g),
                want,
                "{name}: the clamp altered a good answer"
            );
        }
    }

    #[test]
    fn the_caret_is_always_inside_the_row_it_is_on() {
        let mut c = Composer::from_text("hello world this is a longer line that wraps");
        for w in 5..=30usize {
            c.buffer_start();
            loop {
                let (row, col) = c.caret(w);
                let rows = c.layout(w);
                assert!(row < rows.len());
                assert!(col <= w, "caret column {col} past the band {w}");
                let before = c.cursor();
                c.right();
                if c.cursor() == before {
                    break;
                }
            }
        }
    }

    /// **UNDO∘REDO IS IDENTITY, BOTH WAYS.**
    #[test]
    fn undo_and_redo_are_inverses() {
        let mut c = Composer::new();
        for ch in "hello".chars() {
            c.insert_char(ch);
        }
        c.newline();
        c.insert_paste("pasted 👩‍👩‍👧‍👦 text");
        for ch in " tail".chars() {
            c.insert_char(ch);
        }
        let before = c.clone();
        while c.undo() {}
        assert_eq!(c.text(), "", "undo did not reach the empty buffer");
        while c.redo() {}
        assert_eq!(c.text(), before.text(), "redo did not restore the text");
        // …and one step of each, from the middle.
        let mid = c.clone();
        assert!(c.undo());
        assert!(c.redo());
        assert_eq!(c.text(), mid.text());
    }

    /// **A PASTE IS ONE UNDO STEP.** The property the task names by hand: ten
    /// thousand lines in, one `^Z` out.
    #[test]
    fn a_ten_thousand_line_paste_undoes_as_one_operation() {
        let big: String = (0..10_000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut c = Composer::new();
        c.insert_char('x');
        c.insert_paste(&big);
        assert_eq!(c.lines().len(), 10_000);
        assert!(c.undo(), "nothing to undo after a paste");
        assert_eq!(c.text(), "x", "the paste did not undo in one step");
    }

    #[test]
    fn typing_is_one_run_and_a_motion_closes_it() {
        let mut c = Composer::new();
        for ch in "abc".chars() {
            c.insert_char(ch);
        }
        assert!(c.undo());
        assert_eq!(c.text(), "", "typing did not undo as one run");

        let mut c = Composer::new();
        for ch in "ab".chars() {
            c.insert_char(ch);
        }
        c.left();
        c.insert_char('X');
        assert!(c.undo());
        assert_eq!(c.text(), "ab", "a motion did not close the run");
    }

    /// The goal column survives vertical motion through a short line.
    #[test]
    fn vertical_motion_keeps_its_goal_column_through_a_short_line() {
        let mut c = Composer::from_text("aaaaaaaaaa\nbb\ncccccccccc");
        c.cursor = Pos::new(0, 7);
        assert!(c.down(40));
        assert_eq!(
            c.cursor(),
            Pos::new(1, 2),
            "did not clamp to the short line"
        );
        assert!(c.down(40));
        assert_eq!(c.cursor(), Pos::new(2, 7), "the goal column was forgotten");
        // A horizontal edit clears it, which is what makes it a hint.
        c.left();
        assert!(c.up(40));
        assert_eq!(c.cursor().line, 1);
    }

    #[test]
    fn up_at_the_top_and_down_at_the_bottom_report_that_they_could_not_move() {
        let mut c = Composer::from_text("one\ntwo");
        c.buffer_start();
        assert!(!c.up(40), "up at the top claimed to move");
        c.buffer_end();
        assert!(!c.down(40), "down at the bottom claimed to move");
    }

    /// **QUEUE AND SEND ARE TWO DIFFERENT ACTS**, and the payload says which
    /// happened.
    #[test]
    fn queue_sets_aside_and_the_payload_stitches_everything() {
        let mut c = Composer::new();
        assert!(!c.queue(), "an empty composer queued something");
        for ch in "first".chars() {
            c.insert_char(ch);
        }
        assert!(c.queue());
        assert_eq!(c.pending(), [PendingItem::typed("first")]);
        assert_eq!(c.text(), "", "queue did not empty the buffer");
        for ch in "second".chars() {
            c.insert_char(ch);
        }
        let p = c.payload().unwrap();
        assert!(p.starts_with("Queued messages:"), "{p}");
        assert!(p.contains("Message 1:\nfirst"), "{p}");
        assert!(p.contains("Message 2:\nsecond"), "{p}");
        // A LONE message is never dressed up as one of several.
        let mut c = Composer::from_text("alone");
        assert_eq!(c.payload().as_deref(), Some("alone"));
        assert!(c.take().contains("alone"));
    }

    /// **A TRANSCRIBED SPOKEN ENTRY IS INDISTINGUISHABLE FROM A TYPED ONE IN
    /// THE PAYLOAD.** His ruling: once transcribed they look exactly like typed
    /// messages except for a play button. The payload is where that has to be
    /// literally true, because the agent on the other end must not be able to
    /// tell which of his messages he spoke.
    #[test]
    fn a_transcribed_entry_goes_out_exactly_as_a_typed_one_does() {
        let mut c = Composer::from_text("typed one");
        c.queue();
        c.queue_audio("/tmp/cap-a.wav");
        assert!(c.resolve_audio("/tmp/cap-a.wav", Ok("spoken one".into())));
        let p = c.payload().expect("a ready queue must have a payload");
        assert!(p.contains("Message 1:\ntyped one"), "{p}");
        assert!(p.contains("Message 2:\nspoken one"), "{p}");
        // Nothing in the payload says where message 2 came from.
        assert!(
            !p.contains("cap-a"),
            "the payload leaked the capture path: {p}"
        );
        assert!(!p.to_lowercase().contains("transcri"), "{p}");
    }

    /// **A QUEUE WITH AN ENTRY STILL TRANSCRIBING DOES NOT SEND.**
    ///
    /// This is the one that keeps a spoken message from being silently dropped:
    /// stitching only the ready entries would produce a payload missing
    /// something he put in the queue, and nothing would report the loss.
    #[test]
    fn a_queue_that_is_still_transcribing_refuses_to_produce_a_payload() {
        let mut c = Composer::from_text("typed");
        c.queue();
        c.queue_audio("/tmp/cap-b.wav");
        assert_eq!(c.payload_state(), Payload::Transcribing(1));
        assert_eq!(c.payload(), None, "a half-ready queue produced a payload");

        // It becomes sendable the moment the transcript lands, and the typed
        // entry that was there all along is still in it.
        c.resolve_audio("/tmp/cap-b.wav", Ok("spoken".into()));
        let Payload::Ready(p) = c.payload_state() else {
            panic!("still not ready")
        };
        assert!(p.contains("typed") && p.contains("spoken"), "{p}");
    }

    /// **A FAILED ENTRY BLOCKS THE SEND AND KEEPS ITS AUDIO.** The words are
    /// still his; they go out or come out by his decision, never by the send
    /// path quietly skipping them.
    #[test]
    fn a_failed_entry_blocks_the_send_keeps_its_audio_and_can_be_retried() {
        let mut c = Composer::new();
        c.queue_audio("/tmp/cap-c.wav");
        c.resolve_audio("/tmp/cap-c.wav", Err("heard nothing".into()));
        assert_eq!(c.failed(), 1);
        assert_eq!(c.payload_state(), Payload::Failed(1));
        assert_eq!(c.pending()[0].row_text(), "heard nothing");
        // THE AUDIO SURVIVED THE FAILURE — this is what makes a retry possible.
        assert_eq!(c.pending()[0].audio.as_deref(), Some("/tmp/cap-c.wav"));

        assert!(c.retry_audio("/tmp/cap-c.wav"));
        assert_eq!(c.payload_state(), Payload::Transcribing(1));
        c.resolve_audio("/tmp/cap-c.wav", Ok("second time".into()));
        assert_eq!(c.payload().as_deref(), Some("second time"));
    }

    /// An empty transcript at exit 0 is a FAILURE, not an empty message. A row
    /// that went blank would look like a message he never spoke.
    #[test]
    fn an_empty_transcript_becomes_a_failure_and_not_an_empty_message() {
        let mut c = Composer::new();
        c.queue_audio("/tmp/cap-d.wav");
        c.resolve_audio("/tmp/cap-d.wav", Ok("   ".into()));
        assert_eq!(c.failed(), 1);
        assert!(matches!(c.pending()[0].state, ItemState::Failed(_)));
    }

    /// **A TRANSCRIPT LANDING ON A ROW THAT IS GONE IS DROPPED, NOT
    /// MISFILED.** `^K` can take an entry back while a transcription is in
    /// flight; resolving by index would have written the text onto whichever
    /// row slid into that slot.
    #[test]
    fn a_transcript_for_a_row_that_no_longer_exists_lands_nowhere() {
        let mut c = Composer::new();
        c.queue_audio("/tmp/cap-e.wav");
        assert!(c.drop_audio("/tmp/cap-e.wav"));
        assert!(c.pending().is_empty());
        assert!(
            !c.resolve_audio("/tmp/cap-e.wav", Ok("words".into())),
            "a transcript was filed against a row that had been taken out"
        );

        // And it cannot overwrite a DIFFERENT row that happens to sit at the
        // index the dropped one used to occupy.
        let mut c = Composer::new();
        c.queue_audio("/tmp/cap-f.wav");
        c.queue_audio("/tmp/cap-g.wav");
        c.drop_audio("/tmp/cap-f.wav");
        c.resolve_audio("/tmp/cap-f.wav", Ok("wrong row".into()));
        assert_eq!(c.pending()[0].state, ItemState::Transcribing);
        assert_eq!(c.pending()[0].audio.as_deref(), Some("/tmp/cap-g.wav"));
    }

    /// `^K` takes back the last TYPED entry and steps over spoken ones,
    /// which have a recording that a text buffer cannot hold.
    #[test]
    fn unqueue_skips_audio_entries_and_takes_the_last_typed_one() {
        let mut c = Composer::from_text("typed");
        c.queue();
        c.queue_audio("/tmp/cap-h.wav");
        assert!(c.unqueue());
        assert_eq!(c.text(), "typed");
        assert_eq!(c.pending().len(), 1, "the audio entry was taken instead");
        assert_eq!(c.pending()[0].audio.as_deref(), Some("/tmp/cap-h.wav"));
        // With only audio left there is nothing for it to take.
        assert!(!c.unqueue());
    }

    #[test]
    fn a_blank_composer_has_no_payload_and_whitespace_is_blank() {
        assert_eq!(Composer::new().payload(), None);
        assert_eq!(Composer::from_text("   \n  \n ").payload(), None);
        assert!(Composer::from_text("   \n  ").is_blank());
    }

    #[test]
    fn unqueue_puts_the_last_entry_back() {
        let mut c = Composer::from_text("one");
        c.queue();
        assert!(c.unqueue());
        assert_eq!(c.text(), "one");
        assert!(c.pending().is_empty());
        assert!(!c.unqueue());
    }

    #[test]
    fn the_kills_take_what_they_say_and_yank_puts_it_back() {
        let mut c = Composer::from_text("hello world");
        c.buffer_end();
        c.kill_word_back();
        assert_eq!(c.text(), "hello ");
        c.yank();
        assert_eq!(c.text(), "hello world");
        c.buffer_start();
        c.kill_to_line_end();
        assert_eq!(c.text(), "");
        c.yank();
        assert_eq!(c.text(), "hello world");
    }

    /// A width of zero and a width of one are both legal frames to be asked
    /// about, and neither may hang or panic.
    #[test]
    fn a_degenerate_width_neither_hangs_nor_panics() {
        let c = Composer::from_text("日本語 👩‍👩‍👧‍👦 ok");
        for w in [0usize, 1, 2] {
            let rows = c.layout(w);
            assert!(!rows.is_empty());
            let (_, col) = c.caret(w);
            assert!(col <= w.max(1) + 2);
        }
    }
}
