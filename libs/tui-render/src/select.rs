//! **THE SELECTION: what a drag is over, and what a copy takes.**
//!
//! # Why the selection is not a rectangle of screen cells
//!
//! The obvious model for drag-to-copy in a terminal app is *"remember the two
//! screen cells and copy what is painted between them"*, and it is the wrong
//! one here for two independent reasons, both of which bite in his session:
//!
//! 1. **Autoscroll moves the content under the anchor.** Drag past the top
//!    edge and the transcript scrolls; a selection anchored to a screen cell
//!    would then be anchored to whatever slid into that cell, and the anchor
//!    would walk down the screen as he dragged up. The failure is silent — the
//!    highlight looks perfectly reasonable the whole time.
//! 2. **What is painted is not what he wants on his clipboard.** A message on
//!    screen carries a two-column gutter, a `▌`/`▐` rail and hard line breaks
//!    the wrapper put there. Copying the screen copies all three. `0036`
//!    called this out as the one place an app-owned selection is strictly
//!    BETTER than the terminal's own, and it is the whole reason this module
//!    exists rather than a pair of `(col, row)` fields on the shell.
//!
//! So a selection is a range over the face's **logical document**: a
//! [`TextPos`] is a [`Unit`] plus a character offset into that unit's logical
//! text, and both endpoints survive any amount of scrolling because neither
//! names a screen cell.
//!
//! # The document
//!
//! Every addressable thing the frame draws is a UNIT, and the units have a
//! total order — the order they appear down the screen:
//!
//! ```text
//!   Header · Banner · Content(0..n) · Overlay(0..n) · Chip · Composer · Footer
//! ```
//!
//! `Content` is whichever body the mode is showing: a transcript block, a
//! thread row, a project row. **The content units are addressed by MODEL
//! INDEX, not by screen position**, which is what lets a copy reach a block
//! that autoscroll has already carried off the top of the screen — the shell
//! still holds it, and [`selected_text`] reads it from the [`ViewState`]
//! rather than from the frame.
//!
//! # Where this can be silently wrong, stated out loud
//!
//! * **A folded block's logical text is the FOLDED text.** `store`'s
//!   projection is what produced `Block::text()`, and while `expanded` is off
//!   that text is the clipped one, ellipsis and all. Copying a folded message
//!   copies what the projection made, not what the daemon holds. Expanding
//!   first (`→`, or a click on the row) is the answer, and it is the same
//!   answer the screen gives.
//! * **Chrome units are their painted row.** The header assembles itself from
//!   ranked segments and may drop or crop some of them; its logical text is
//!   what survived, because there is no other string that is "the header".
//!   Copying a cropped header copies the crop, `…` included.
//! * **A column inside a double-width glyph resolves to that glyph.** There is
//!   no half-character offset to resolve to.
//! * **A TABLE's painted rows are not addressable.** A grid's characters are
//!   its cells plus padding computed from the terminal width, so a painted
//!   table row is not a slice of any width-independent string and
//!   [`crate::md`] emits it with no [`RowText`] at all — exactly as a run
//!   header does. A drag therefore cannot ANCHOR inside a table and no
//!   highlight is painted over one; a drag that SPANS the table still copies
//!   the whole table, in the normalised `a | b | c` form, because the unit's
//!   text is read from the model and never from the screen.

use unicode_segmentation::UnicodeSegmentation;

use crate::keys::Mode;
use crate::model::ViewState;

/// Which band of the document a unit belongs to.
///
/// The variant ORDER is the document order, and [`UnitKind::rank`] is what the
/// comparison actually uses — written out rather than derived from the
/// discriminant so that inserting a variant cannot silently re-order the
/// document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitKind {
    Header,
    Banner,
    /// One row of whatever body the mode is showing — a transcript block, a
    /// thread row, a project row. Addressed by MODEL index.
    Content,
    /// One row of a raised overlay's body. Addressed by row.
    Overlay,
    /// The `↓ N new` chip.
    Chip,
    Composer,
    /// One row of the SUBAGENT BAND, below the composer. Its own kind, not the
    /// composer's: a drag that addressed a tree row as `Composer(3)` would name
    /// the same unit as the fourth row of the box, and a selection spanning the
    /// two would take the wrong text out of one of them.
    Agents,
    Footer,
}

impl UnitKind {
    /// Position in the document. `Content` and `Overlay` never coexist — an
    /// overlay replaces the body — so their relative order is arbitrary and
    /// unobservable.
    pub fn rank(self) -> u8 {
        match self {
            UnitKind::Header => 0,
            UnitKind::Banner => 1,
            UnitKind::Content => 2,
            UnitKind::Overlay => 3,
            UnitKind::Chip => 4,
            UnitKind::Composer => 5,
            UnitKind::Agents => 6,
            UnitKind::Footer => 7,
        }
    }
}

/// One addressable piece of logical text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Unit {
    pub kind: UnitKind,
    /// The model index for [`UnitKind::Content`], the row for
    /// [`UnitKind::Overlay`] and [`UnitKind::Composer`], `0` otherwise.
    pub index: usize,
}

impl Unit {
    pub fn new(kind: UnitKind, index: usize) -> Self {
        Self { kind, index }
    }

    fn key(self) -> (u8, usize) {
        (self.kind.rank(), self.index)
    }
}

impl PartialOrd for Unit {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Unit {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key().cmp(&other.key())
    }
}

/// A position in the document: a unit, and a character offset into it.
///
/// The offset is a CHARACTER index, never a byte index and never a column —
/// the first because a byte index into UTF-8 can land mid-codepoint, the
/// second because a column means nothing once the row it was on has scrolled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPos {
    pub unit: Unit,
    pub off: usize,
}

impl TextPos {
    pub fn new(unit: Unit, off: usize) -> Self {
        Self { unit, off }
    }
}

impl PartialOrd for TextPos {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TextPos {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.unit, self.off).cmp(&(other.unit, other.off))
    }
}

/// **A live or finished drag.** Both ends are INCLUSIVE cell positions: the
/// character under the pointer is part of the selection, which is what every
/// terminal does and what makes a one-cell drag select one character rather
/// than nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Where the button went down. Never moves, however far the view scrolls.
    pub anchor: TextPos,
    /// Where the pointer is now.
    pub head: TextPos,
}

impl Selection {
    pub fn new(anchor: TextPos, head: TextPos) -> Self {
        Self { anchor, head }
    }

    /// The two ends in document order. A drag upwards is the same selection as
    /// the identical drag downwards, so nothing downstream has to know which
    /// way his hand went.
    pub fn bounds(&self) -> (TextPos, TextPos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

/// **What one drawn row shows, of which unit, starting where.**
///
/// Built by the draw itself (`render::frame_with_hits`), exactly as the
/// [`crate::HitMap`] it travels inside is — a second pass that re-derived
/// which characters landed on which row would be a second derivation of the
/// wrap, and a wrong one puts the wrong text on his clipboard without
/// anything on screen looking wrong.
#[derive(Debug, Clone)]
pub struct RowText {
    pub unit: Unit,
    /// Char offset, within the unit's logical text, of this row's first
    /// painted character.
    pub start: usize,
    /// The screen column that character is painted at.
    pub col0: u16,
    /// Exactly the characters of the unit painted on this row — no gutter, no
    /// rail, no padding.
    pub text: String,
}

impl RowText {
    /// How many characters of the unit this row carries.
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// **Which character of the unit is under `col`.**
    ///
    /// Left of the text is its first character, right of it is its last: a
    /// drag that starts in the gutter selects from the start of the line and a
    /// drag that ends past the text selects to the end of it, which is what a
    /// terminal does and what he will expect.
    pub fn offset_at(&self, col: u16) -> usize {
        let last = self.start + self.len().saturating_sub(1);
        if col < self.col0 {
            return self.start;
        }
        let mut c = self.col0 as usize;
        let mut off = self.start;
        // **BY CLUSTER**, so the column a pointer is on is computed with the
        // same arithmetic `geom::size_line` used to paint it and
        // `geom::highlight` uses to reverse it. Per-`char` measurement here
        // would put the highlight and the clipboard on different characters of
        // the same emoji, silently.
        for g in self.text.graphemes(true) {
            let w = crate::compose::cluster_width(g);
            if (col as usize) < c + w {
                return off;
            }
            c += w;
            off += g.chars().count();
        }
        last
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  the logical text of a unit
// ────────────────────────────────────────────────────────────────────────────

/// How many content units this mode's body has.
///
/// **The ROOT counts LAID-OUT LINES, not project rows**, and that is the band
/// surface's one consequence for a drag: a band header and a spacer are drawn,
/// so they are units, so a selection that runs across them copies what he can
/// see. Counting `rows` here instead would silently shift every unit above the
/// projects band and the clipboard would disagree with the highlight.
pub fn content_len(vs: &ViewState) -> usize {
    match vs.mode {
        // **THE PAGE'S UNITS ARE THE ROOT'S**, off the same `root_lines`
        // layout — one band's worth of them. A second answer here is how a
        // drag on the page would copy the row above the one he dragged.
        Mode::Projects | Mode::NewProject | Mode::SeeAll(_) => vs.projects.drawn_len(),
        Mode::List => vs.list.rows.len(),
        Mode::Archived => vs.archived.rows.len(),
        Mode::Thread(_) | Mode::Activity => vs.blocks.len(),
        _ => 0,
    }
}

/// **The label a thread row shows, and whether it is a blank stand-in.**
///
/// The ONE derivation of the fallback — the name, else `untitled`.
/// `render::thread_row` paints it and [`thread_run`] maps columns onto it, and
/// they have to be the same string or a drag selects text the row is not
/// showing.
///
/// **THE CWD IS NO LONGER A NAME** (TUI-P1a, checklist TC-02). It was the
/// second rung, and his verdict on it: *"if a thread wasn't named, then it just
/// shows the project path, which is the worst possible thing it could show."*
/// The shell now hands every row a name — the thread's own, else the first
/// line of its first message — so an empty summary reaching here means there
/// is truly nothing, and that is `untitled`.
pub fn thread_label(r: &crate::model::ThreadRow) -> (String, bool) {
    if !r.summary.is_empty() {
        return (r.summary.clone(), false);
    }
    ("untitled".to_string(), true)
}

/// **The CONTIGUOUS RUN a thread row paints**, after its two marker columns.
///
/// The label alone. **The ID LEFT THE CLIPBOARD WITH THE ROW** (`0076`), and
/// the rule that made it a copy target in the first place is what forces it
/// out now: *"a copy may be richer than the screen only where the mapping
/// survives it"* — true of a wrapped message (one string, laid out), false of
/// a row assembled out of two. The row no longer draws the id (`DIR-16`), so a
/// run that still carried it would put eight characters in front of every
/// column he drags and resolve each one to the wrong character of the summary.
///
/// # The `DEC-27` collision, and the ruling
///
/// `DEC-27` made a thread row copy its EIGHT-character id rather than its
/// thirty-six, deliberately, so that the copy and the row stayed
/// column-for-column the same thing; `DIR-16` was then scoped to DISPLAY
/// explicitly so as not to contradict it. That scoping worked only while the
/// id was on the screen. **The manager's ruling: the row displays the summary
/// and COPIES the summary — the id leaves the clipboard.** The id in the
/// clipboard was the face's design choice; `DIR-16` is his ask, in his own
/// words. `DEC-27`'s INVARIANT is preserved exactly — it is the id that goes,
/// not the rule — and the id stays reachable on purpose through the sheet's
/// `copy the thread id` row, which is where an id you copy deliberately
/// belongs.
///
/// It is now the same derivation as [`band_run`], which is the honest end of
/// the story `0073` told: the root's bands and the project's list draw one
/// `ThreadRow` and disagreed about it for two rounds.
pub fn thread_run(r: &crate::model::ThreadRow) -> String {
    thread_label(r).0
}

/// The contiguous run a project row paints, after its two marker columns.
pub fn project_run(r: &crate::model::ProjectRow) -> String {
    // **THE PADDING IS PART OF THE RUN**, because it is part of what was
    // DRAWN. The project rows align their paths in a column now (his ask:
    // "I want the directories to be aligned on each other"), and the gap
    // between a short name and its path is real screen the drag crosses. A run
    // that collapsed it to two spaces would hand back a copy that does not
    // match the rows it was dragged over — the exact two-derivations defect
    // this module exists to prevent.
    format!(
        "{}{}{}",
        r.name,
        crate::render::name_col(&r.name, usize::MAX),
        r.path
    )
}

/// The contiguous run a ROOT BAND row paints, after its two marker columns.
///
/// The label alone — no id, because a band row draws none, and [`thread_run`]'s
/// rule binds here too: a run richer than the row is a run whose columns
/// resolve to the wrong characters.
pub fn band_run(r: &crate::model::ThreadRow) -> String {
    thread_label(r).0
}

/// The words the chrome tally paints. One derivation, so the row and the copy
/// cannot say different numbers.
/// **THE COPYABLE TEXT OF A BAND HEADER.**
///
/// The caption plus its note, spelled the ONE way the renderer spells it —
/// because a drag over the root copies this string and a second derivation is
/// how a copy comes back reading something the screen never said.
pub fn band_header_run(list: &crate::model::ProjectList, b: crate::model::BandId) -> String {
    let note = list.band_note(b);
    if note.is_empty() {
        b.label().to_string()
    } else {
        format!("{} · {}", b.label(), note)
    }
}

pub fn archived_run(n: usize) -> String {
    format!("archived ({n})")
}

/// **THE WORDS `+ new project` PAINTS**, in each of its three states — resting,
/// typing, and unreachable.
///
/// One derivation, for the same reason every other `*_run` is one: the row, the
/// copy and the hit map's text must not be able to say different things.
///
/// **The unreachable sentence REPLACES the hint rather than stacking on it** —
/// [`SheetRow::words`](crate::model::SheetRow::words)' rule, applied to the one
/// AUI-owned row that lives outside the sheet.
pub fn new_project_run(list: &crate::model::ProjectList) -> String {
    match (list.new_project.as_ref(), list.aui_down.as_deref()) {
        // Typing wins over everything: what he has typed is the row.
        (Some(c), _) => format!("+ {}", c.text()),
        (None, Some(why)) => format!("+ new project — {why}"),
        (None, None) => "+ new project — type its name".to_string(),
    }
}

/// **The logical text of one content row, read from the MODEL.**
///
/// Not from the frame, and that is the point: this answers for a block the
/// viewport is not currently showing, which is what a drag with autoscroll
/// produces on every single use.
///
/// `None` means the mode has no model row at that index — an overlay is up, or
/// the body is an empty-state sentence. The caller then falls back to what was
/// drawn, which is the only thing there is.
pub fn content_text(vs: &ViewState, i: usize) -> Option<String> {
    match vs.mode {
        // The root's unit `i` is its `i`th LAID-OUT LINE — see `content_len`.
        Mode::Projects | Mode::NewProject | Mode::SeeAll(_) => {
            vs.projects.lines().get(i).and_then(|l| match *l {
            crate::model::RootLine::Band(b) => Some(band_header_run(&vs.projects, b)),
            crate::model::RootLine::SeeAll(b) => Some(vs.projects.see_all_words(b)),
            crate::model::RootLine::Hint(s) => Some(s.to_string()),
            crate::model::RootLine::Spacer => None,
            crate::model::RootLine::Pinned(n) => vs.projects.pinned.get(n).map(band_run),
            crate::model::RootLine::Recent(n) => vs.projects.recent.get(n).map(band_run),
            crate::model::RootLine::Project(n) => vs.projects.rows.get(n).map(project_run),
            crate::model::RootLine::NewProject => Some(new_project_run(&vs.projects)),
            crate::model::RootLine::Archived => Some(archived_run(vs.projects.archived)),
            })
        }
        Mode::List => vs.list.rows.get(i).map(thread_run),
        Mode::Archived => vs.archived.rows.get(i).map(|r| r.words()),
        // **A MESSAGE's LOGICAL TEXT IS ITS RENDERED TEXT, NOT ITS SOURCE.**
        // `crate::md` strips the emphasis markers and normalises the lists and
        // tables, so `**bold**` is painted as four characters and not eight.
        // Every `RowText::start` in a message is an offset into THIS string,
        // which is why it has to be the same function that painted them — a
        // copy taken from the source would be one offset per marker out, and
        // the screen would look perfect the whole time.
        Mode::Thread(_) | Mode::Activity => vs.blocks.get(i).map(|b| match b {
            crate::model::Block::Message { text, .. } => crate::md::plain(text),
            _ => b.text().to_string(),
        }),
        _ => None,
    }
}

/// Every unit of the document, in order, restricted to `a..=b`.
///
/// `map` supplies the chrome and overlay units, which exist only because
/// something was drawn; the content units come from the model, so a unit that
/// has scrolled off screen is still in this list.
fn units_between(vs: &ViewState, map: &[Unit], a: Unit, b: Unit) -> Vec<Unit> {
    let mut all: Vec<Unit> = Vec::new();
    let has = |k: UnitKind| map.iter().any(|u| u.kind == k);
    if has(UnitKind::Header) {
        all.push(Unit::new(UnitKind::Header, 0));
    }
    if has(UnitKind::Banner) {
        all.push(Unit::new(UnitKind::Banner, 0));
    }
    // The model's rows, plus any content row that was DRAWN without one — the
    // empty-state sentence is content row 0 of a body with no model rows at
    // all, and "any part of the interface" includes it.
    let mut content: Vec<usize> = (0..content_len(vs)).collect();
    content.extend(
        map.iter()
            .filter(|u| u.kind == UnitKind::Content)
            .map(|u| u.index),
    );
    content.sort_unstable();
    content.dedup();
    all.extend(content.into_iter().map(|i| Unit::new(UnitKind::Content, i)));
    let mut overlay: Vec<usize> = map
        .iter()
        .filter(|u| u.kind == UnitKind::Overlay)
        .map(|u| u.index)
        .collect();
    overlay.sort_unstable();
    overlay.dedup();
    all.extend(overlay.into_iter().map(|i| Unit::new(UnitKind::Overlay, i)));
    if has(UnitKind::Chip) {
        all.push(Unit::new(UnitKind::Chip, 0));
    }
    let mut composer: Vec<usize> = map
        .iter()
        .filter(|u| u.kind == UnitKind::Composer)
        .map(|u| u.index)
        .collect();
    composer.sort_unstable();
    composer.dedup();
    all.extend(
        composer
            .into_iter()
            .map(|i| Unit::new(UnitKind::Composer, i)),
    );
    if has(UnitKind::Footer) {
        all.push(Unit::new(UnitKind::Footer, 0));
    }
    all.retain(|u| *u >= a && *u <= b);
    all
}

/// The full logical text of one unit: from the model where there is one, from
/// what was drawn where there is not.
///
/// The model wins for content because it can answer about a row that has
/// scrolled off the screen, which is the case every autoscrolling drag ends
/// in. The fallback catches the content rows that have no model behind them at
/// all — an empty-state sentence is content row 0 and is still copyable.
fn unit_text(vs: &ViewState, rows: &[RowText], unit: Unit) -> Option<String> {
    if unit.kind == UnitKind::Content {
        if let Some(t) = content_text(vs, unit.index) {
            return Some(t);
        }
    }
    if !rows.iter().any(|r| r.unit == unit) {
        return None;
    }
    let mut out = String::new();
    for r in rows.iter().filter(|r| r.unit == unit) {
        out.push_str(&r.text);
    }
    Some(out)
}

fn slice_chars(s: &str, from: usize, to: usize) -> String {
    s.chars().skip(from).take(to.saturating_sub(from)).collect()
}

/// **THE TEXT A COPY TAKES.**
///
/// The anchor unit from the anchor offset to its end, every unit between in
/// full, the head unit from its start through the head offset INCLUSIVE, and
/// the units joined by a newline. One unit, one line — so a transcript
/// selection comes out as one paragraph per message, unwrapped, with no
/// gutter and no rails.
pub fn selected_text(vs: &ViewState, rows: &[RowText], sel: Selection) -> String {
    let (a, b) = sel.bounds();
    let present: Vec<Unit> = rows.iter().map(|r| r.unit).collect();
    let mut parts: Vec<String> = Vec::new();
    for unit in units_between(vs, &present, a.unit, b.unit) {
        let Some(full) = unit_text(vs, rows, unit) else {
            continue;
        };
        let n = full.chars().count();
        let from = if unit == a.unit { a.off.min(n) } else { 0 };
        // INCLUSIVE at the far end: the character under the pointer is his.
        let to = if unit == b.unit {
            (b.off + 1).min(n)
        } else {
            n
        };
        parts.push(slice_chars(&full, from, to));
    }
    parts.join("\n")
}

/// The COLUMN RANGE of `row` that the selection covers, if any.
///
/// Returned as `(first, last_exclusive)` display columns, for
/// [`crate::geom::highlight`]. `None` means this row is outside the selection
/// entirely and must be drawn untouched.
pub fn row_span(row: &RowText, sel: Selection) -> Option<(usize, usize)> {
    let (a, b) = sel.bounds();
    if row.unit < a.unit || row.unit > b.unit {
        return None;
    }
    let lo = if row.unit == a.unit { a.off } else { 0 };
    let hi = if row.unit == b.unit {
        b.off.saturating_add(1)
    } else {
        usize::MAX
    };
    let row_end = row.start + row.len();
    let from = lo.max(row.start);
    let to = hi.min(row_end);
    if to <= from {
        return None;
    }
    // Walk the row's own CLUSTERS to turn the char range into columns — the
    // same unit `geom::highlight` reverses in, so the two cannot disagree about
    // where a double-width glyph starts.
    let mut col = row.col0 as usize;
    let mut first = None;
    let mut last = col;
    let mut k = 0usize;
    for g in row.text.graphemes(true) {
        let w = crate::compose::cluster_width(g);
        let off = row.start + k;
        if off >= from && off < to {
            if first.is_none() {
                first = Some(col);
            }
            last = col + w;
        }
        col += w;
        k += g.chars().count();
    }
    first.map(|f| (f, last))
}
