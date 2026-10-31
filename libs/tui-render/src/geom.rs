//! Geometry: the crop/pad rule, wrapping, and the one duration grammar.
//!
//! **The rule this module exists to hold** (`0005`, carried forward by `0023`
//! `defect_classes` CLASS 3): every line is pre-sized to EXACTLY the width it
//! is handed, and the resulting block is cropped, never wrapped by whatever
//! draws it. `0018` proved the stakes empirically in THIS stack: ratatui 0.28
//! does not wrap an over-reported line, it PANICS
//! (`index outside of buffer`). So over-report is fatal and under-report
//! leaves a stale column — **narrower always wins** on any disagreement,
//! because under-width fails bounded and over-width fails unbounded.
//!
//! The corollary: **never crop or pad using a width captured from a different
//! render pass than the one currently painting.** This crate makes that
//! structurally impossible — `width` arrives as a parameter of
//! [`crate::frame`] and nothing in here can reach any other source for it.

use chrono::TimeDelta;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Below this the face draws one honest line and nothing else.
pub const MIN_WIDTH: u16 = 40;

/// Below this the face draws one honest line and nothing else: four bands
/// cannot be laid out in fewer rows without one of them lying about its size.
pub const MIN_HEIGHT: u16 = 8;

/// Columns the gutter+rail costs on each side of a chat box.
pub const BOX_CHROME: usize = 2;

/// At or below this width the margins collapse (see [`msg_margin`] and
/// [`blob_margin`]) and timestamps drop out of run headers (`0023` `layout`,
/// "AT 80 COLUMNS").
pub const NARROW_WIDTH: u16 = 80;

/// **THE TWO-COLUMN GUTTER, as geometry sees it.** The same two columns as
/// `render::GUTTER`; that is the name the rest of the crate and the shell use,
/// and a `const` assertion in `render.rs` fails the build if the two ever
/// disagree. It is restated here because the margin arithmetic below is the
/// one place a message block's width is decided, and that arithmetic cannot
/// reach up into `render`.
pub const MSG_GUTTER: usize = 2;

/// **HIS MARGIN, AND IT IS NOW THE SPECIFICATION.**
///
/// > *the margins are too big. The agent margin should be like 10 on the
/// > right. The user margin should be like 10 on the left. The tool calls can
/// > be like 8 on the left and 8 on the right.*
///
/// Ten columns: the AGENT's on the right of its words, the USER's on the left
/// of its box. One number for both because he gave one number for both, and
/// because it is what makes the two blocks the same width — the agent's block
/// is [`box_width`] columns of text and his is [`box_width`] columns of box,
/// of which two are the box's own padding.
///
/// **THIS SUPERSEDES THE CEILING AS THE THING THAT DECIDES A MARGIN.** It used
/// to be the other way round: `BOX_MAX` was 76, it bound at any width over 82,
/// and the margin was then whatever was left over — 21 columns on the agent's
/// right at his 97-column pane, measured. The ceiling still exists, but only as
/// a ceiling; see [`WIDE_WIDTH`].
pub const MSG_MARGIN: usize = 10;

/// The same margin at or below [`NARROW_WIDTH`] — halved, because ten columns
/// each side of a 40-column terminal would leave twenty for his words.
/// *Decoration first, metadata second, content never* (`0023` `layout`).
pub const NARROW_MSG_MARGIN: usize = 5;

/// **THE FIRM MARGIN**, in columns, on EACH side of an info blob — *"the tool
/// calls can be like 8 on the left and 8 on the right"*.
///
/// The word "firm" is his and it is the whole specification: an expander that
/// widened on expand would have failed the scope, so the margin is computed
/// from the width alone and nothing about a blob's content or its open-ness can
/// reach it. [`blob_margin`] is a function of ONE argument for that reason.
pub const BLOB_MARGIN: usize = 8;

/// A blob's margin at or below [`NARROW_WIDTH`], halved for
/// [`NARROW_MSG_MARGIN`]'s reason.
pub const NARROW_BLOB_MARGIN: usize = 4;

/// **THE WIDTH ABOVE WHICH THE LANES STOP WIDENING**, and the one number both
/// ceilings below are derived from.
///
/// A margin is a specification, not a maximum: at 300 columns a 10-column
/// margin would hand the reader a 288-column line and the return sweep would
/// cost more than the margin saved. So the margins bind up to here and the
/// ceilings bind above it. 108 is chosen so that the ceiling CANNOT bind at his
/// 97-column pane — which is the exact failure being fixed — while still being
/// inside the reading measure the ceiling exists to protect: 96 columns of
/// agent text, 94 inside his box, against a classic comfortable measure that
/// tops out around 90.
pub const WIDE_WIDTH: usize = 108;

/// The chat box ceiling — a CEILING and no longer the thing that decides a
/// margin. It binds only above [`WIDE_WIDTH`]; at or below it the block is
/// `width - MSG_GUTTER - `[`msg_margin`] and the margin is exact.
pub const BOX_MAX: usize = WIDE_WIDTH - MSG_GUTTER - MSG_MARGIN;

/// The margin a message block keeps at `width` — [`MSG_MARGIN`], or
/// [`NARROW_MSG_MARGIN`] at or below [`NARROW_WIDTH`].
pub fn msg_margin(width: usize) -> usize {
    if width <= NARROW_WIDTH as usize {
        NARROW_MSG_MARGIN
    } else {
        MSG_MARGIN
    }
}

/// The margin an info blob keeps at `width`. **One argument, and that is the
/// point** — see [`BLOB_MARGIN`].
pub fn blob_margin(width: usize) -> usize {
    if width <= NARROW_WIDTH as usize {
        NARROW_BLOB_MARGIN
    } else {
        BLOB_MARGIN
    }
}

/// **THE INFO BLOB's CEILING**, and — like [`BOX_MAX`] — a ceiling only. The
/// third lane does not run the width of the page; below [`WIDE_WIDTH`] it runs
/// `width - 2 * `[`blob_margin`] and the margins are exact, and above it this
/// number binds instead.
///
/// > *info blobs should be centered with a max character size that enforces a
/// > margin on each side so it doesn't take up the whole page. Clicking it
/// > should expand it but should not break the margins, they're firm. It can
/// > expand or collapse vertically.*
///
/// It was 64, which bound at any width over 76 and left his 97-column pane
/// with 16 columns on the left and 17 on the right where he asked for 8 and 8.
pub const BLOB_MAX: usize = WIDE_WIDTH - 2 * BLOB_MARGIN;

/// How many columns an info blob's row may occupy. See [`BLOB_MAX`].
///
/// **It does not take `open`, and that is the point.** There is one width for a
/// given terminal width and both states are drawn in it; opening a blob can
/// only add ROWS.
pub fn blob_width(width: usize) -> usize {
    BLOB_MAX
        .min(width.saturating_sub(2 * blob_margin(width)))
        .max(1)
        .min(width)
}

/// The column an info blob starts at — centred, with the remainder of an odd
/// gap given to the RIGHT margin so the left edge of every blob at a given
/// width is the same column.
pub fn blob_left(width: usize) -> usize {
    width.saturating_sub(blob_width(width)) / 2
}

/// The one duration grammar, everywhere a duration appears (`0005`).
///
/// `<60s -> "7s"`, `<60m -> "42m"`, `<24h -> "15h 42m"`, else `"3d 07h"`,
/// remainders zero-padded.
///
/// A NEGATIVE delta renders `?`. A `now` that precedes the timestamp means
/// clock skew somewhere, and a timestamp we cannot read is not a timestamp we
/// may guess at — honesty rule 1, which is also why there is no `0m` bucket
/// swallowing a four-second age.
pub fn fmt_age(d: TimeDelta) -> String {
    let secs = d.num_seconds();
    if secs < 0 {
        return "?".to_string();
    }
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h {:02}m", mins % 60);
    }
    format!("{}d {:02}h", hours / 24, hours % 24)
}

/// **Display width of a string in terminal columns — measured by GRAPHEME
/// CLUSTER, through the one width function the crate has.**
///
/// It used to sum `char` widths, and that was a second derivation of a fact
/// [`crate::compose::cluster_width`] already owned. The two disagreed on
/// exactly the inputs that matter: `unicode-width` folds a ZWJ family emoji to
/// TWO columns when asked about the cluster and reports EIGHT when asked about
/// its characters one at a time. The composer laid its rows out by the first
/// number and this function sized them by the second, so a row of emoji was
/// cropped to a quarter of itself and the frame came back a column short of its
/// own width — `0018`'s panic class, found by `tests/nasty.rs`.
///
/// One function, one answer. `geom::wrap` still decides WHERE the transcript
/// breaks by character, which is a different question and can only ever produce
/// a row that is SHORTER than it needs to be — safe, because short is padded
/// and long is a panic.
pub fn text_width(s: &str) -> usize {
    crate::compose::str_width(s)
}

/// Crop or pad a span sequence to EXACTLY `width` columns and seal it into an
/// owning [`Line`].
///
/// Cropping is by column, and by CLUSTER rather than by char: half of a wide
/// glyph is a corrupted cell, and a corrupted cell shifts everything after it.
/// A cluster that straddles the boundary is dropped whole.
///
/// **The padding is computed from what was actually TAKEN.** The previous body
/// set `used = width` when a glyph would not fit and then skipped the pad,
/// which produced a line one column short of the width it was asked for
/// whenever a double-width cluster straddled the boundary — a silent violation
/// of the one postcondition this whole crate is built around, and reachable
/// with a single emoji at the wrong column.
pub fn size_line(
    spans: Vec<Span<'_>>,
    width: usize,
    pad_style: ratatui::style::Style,
) -> Line<'static> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    'outer: for span in spans {
        if used >= width {
            break;
        }
        let style = span.style;
        let mut taken = String::new();
        for g in span.content.graphemes(true) {
            let w = crate::compose::cluster_width(g);
            if used + w > width {
                if !taken.is_empty() {
                    out.push(Span::styled(taken, style));
                }
                break 'outer;
            }
            taken.push_str(g);
            used += w;
        }
        if !taken.is_empty() {
            out.push(Span::styled(taken, style));
        }
    }
    if used < width {
        out.push(Span::styled(" ".repeat(width - used), pad_style));
    }
    Line::from(out)
}

/// One styled fragment on its way into the wrapper.
#[derive(Debug, Clone)]
pub struct Seg {
    pub text: String,
    pub style: ratatui::style::Style,
}

impl Seg {
    pub fn new(text: impl Into<String>, style: ratatui::style::Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

/// Crop a styled segment sequence to `width` columns, marking the cut with `…`.
///
/// The ellipsis is not decoration. [`size_line`] alone crops SILENTLY, which
/// makes "that is all of it" and "there is more, and you cannot see it" look
/// identical. The event lane is one-liners by construction (`0023` `layout`),
/// so nearly every lane row is a candidate for this cut, and a tool name cut
/// off with no mark reads as a shorter tool name.
pub fn crop_with_ellipsis(segs: Vec<Seg>, width: usize) -> Vec<Seg> {
    let total: usize = segs.iter().map(|s| text_width(&s.text)).sum();
    if total <= width || width == 0 {
        return segs;
    }
    let budget = width - 1;
    let mut out: Vec<Seg> = Vec::new();
    let mut used = 0usize;
    let mut last_style = segs.last().map(|s| s.style);
    for seg in segs {
        if used >= budget {
            break;
        }
        let mut taken = String::new();
        // By CLUSTER, like everything else that decides a painted width.
        for g in seg.text.graphemes(true) {
            let w = crate::compose::cluster_width(g);
            if used + w > budget {
                break;
            }
            taken.push_str(g);
            used += w;
        }
        if !taken.is_empty() {
            last_style = Some(seg.style);
            out.push(Seg::new(taken, seg.style));
        }
    }
    // **THE ELLIPSIS BELONGS TO THE SEGMENT IT TRUNCATED, NOT BESIDE IT.**
    //
    // It used to be pushed as a segment of its own, and that was a defect the
    // frame could not show. `render::span_of` reads a segment's columns off
    // this output to claim a CLICK TARGET — the thread title is the door onto
    // its sheet — and a standalone `…` is a segment the title's index does not
    // name. So at 60 columns, where every title ellipsizes, the one glyph on
    // the row that says "there is more of this title here" was the one column
    // that did not open it. Measured on a real pty before this change: columns
    // 2–24 of a band row opened the sheet and column 25, the `…`, did not.
    //
    // Merging it into the last TAKEN segment fixes that at the source, for
    // every caller, and keeps the rule the span was written to protect — the
    // claim still covers only columns that were DRAWN, never blank space past
    // the crop.
    if let Some(style) = last_style {
        match out.last_mut() {
            Some(last) => last.text.push('…'),
            None => out.push(Seg::new("…", style)),
        }
    }
    out
}

/// One wrapped row, and **where in the SOURCE it came from**.
///
/// `start` and `len` are char indices into the concatenation of the segment
/// texts handed to [`wrap`] — not byte offsets and not columns. They exist
/// because a drag-selection has to answer *"what logical text is under this
/// screen cell"*, and the only place that knows how a paragraph became rows is
/// the function that wrapped it. Deriving it a second time from the painted
/// rows would be `0023` `defect_classes` CLASS 3 in the one place a wrong
/// answer never looks wrong: the clipboard.
///
/// **`start + len` is not always the next row's `start`.** [`wrap`] drops the
/// space it broke on, so a character of the source can belong to no row at all.
/// A consumer therefore maps a COLUMN to an offset inside one row, and never
/// assumes the rows tile the source.
#[derive(Debug, Clone)]
pub struct WrapRow {
    pub spans: Vec<Span<'static>>,
    /// Char index of this row's first character in the source.
    pub start: usize,
    /// How many source characters this row carries.
    pub len: usize,
}

impl WrapRow {
    /// The characters this row actually paints, as one string.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.content.as_ref()).collect()
    }
}

/// Word-wrap a styled segment sequence to `width` columns.
///
/// Styles travel with the characters, so a fragment that changes colour
/// mid-row wraps as one piece of text rather than as two independently
/// laid-out blocks. Words longer than the line are broken hard: a URL that
/// does not fit still has to be readable, and a line that silently vanished
/// would be worse than one that is split.
///
/// Returns at least one (possibly empty) row, so a caller can always index 0.
/// Each row also carries its source range — see [`WrapRow`].
pub fn wrap(segs: &[Seg], width: usize) -> Vec<WrapRow> {
    if width == 0 {
        return vec![WrapRow {
            spans: Vec::new(),
            start: 0,
            len: 0,
        }];
    }
    // Flatten to (source index, char, style) so a word may legally straddle a
    // style change AND every character can still say where it came from.
    let mut chars: Vec<(usize, char, ratatui::style::Style)> = Vec::new();
    for seg in segs {
        for ch in seg.text.chars() {
            chars.push((chars.len(), ch, seg.style));
        }
    }

    type Cell = (usize, char, ratatui::style::Style);
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut cur: Vec<Cell> = Vec::new();
    let mut cur_w = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let (_, ch, _) = chars[i];
        if ch == '\n' {
            rows.push(std::mem::take(&mut cur));
            cur_w = 0;
            i += 1;
            continue;
        }
        if ch == ' ' {
            if cur_w == 0 {
                // Leading spaces at a wrap point are dropped, not drawn.
                i += 1;
                continue;
            }
            if cur_w + 1 > width {
                rows.push(std::mem::take(&mut cur));
                cur_w = 0;
                i += 1;
                continue;
            }
            cur.push(chars[i]);
            cur_w += 1;
            i += 1;
            continue;
        }
        // Measure the whole word.
        let start = i;
        let mut word_w = 0usize;
        while i < chars.len() && chars[i].1 != ' ' && chars[i].1 != '\n' {
            word_w += chars[i].1.width().unwrap_or(0);
            i += 1;
        }
        let word = &chars[start..i];
        if cur_w + word_w <= width {
            cur.extend_from_slice(word);
            cur_w += word_w;
        } else if word_w <= width {
            // Fits on a line of its own: break before it.
            rows.push(std::mem::take(&mut cur));
            cur.extend_from_slice(word);
            cur_w = word_w;
        } else {
            // Longer than a whole line: break it hard, filling as we go.
            for &cell in word {
                let w = cell.1.width().unwrap_or(0);
                if cur_w + w > width {
                    rows.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                cur.push(cell);
                cur_w += w;
            }
        }
    }
    rows.push(cur);

    rows.into_iter()
        .map(|row| {
            let start = row.first().map(|(i, ..)| *i).unwrap_or(0);
            let len = row.len();
            // Coalesce runs of equal style back into spans.
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut buf = String::new();
            let mut cur_style: Option<ratatui::style::Style> = None;
            for (_, ch, style) in row {
                match cur_style {
                    Some(s) if s == style => buf.push(ch),
                    Some(s) => {
                        spans.push(Span::styled(std::mem::take(&mut buf), s));
                        buf.push(ch);
                        cur_style = Some(style);
                    }
                    None => {
                        buf.push(ch);
                        cur_style = Some(style);
                    }
                }
            }
            if let Some(s) = cur_style {
                spans.push(Span::styled(buf, s));
            }
            WrapRow { spans, start, len }
        })
        .collect()
}

/// **Reverse-video the columns `[from, to)` of an already-sized line.**
///
/// The selection is painted by RESTYLING a drawn row rather than by drawing it
/// differently, which is the only way "any part of the interface is
/// selectable" can be true without every band learning what a selection is.
/// The row's characters, its widths and its length are untouched — so the
/// shape law ([`size_line`]'s postcondition) survives a highlight by
/// construction, and there is no second layout for the highlight to disagree
/// with.
///
/// Columns are DISPLAY columns. A double-width character straddling a boundary
/// is highlighted whole: half a reversed glyph is a cell nobody can read, and
/// the alternative — splitting it — is not expressible in a terminal at all.
pub fn highlight(line: Line<'static>, from: usize, to: usize) -> Line<'static> {
    if to <= from {
        return line;
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;
    for span in line.spans {
        let style = span.style;
        // Split this span at the two boundaries, by column.
        let mut buf = String::new();
        let mut buf_on = false;
        // By CLUSTER: a highlight that split a ZWJ emoji would reverse half a
        // glyph, which no terminal can draw, and it would disagree with
        // `size_line` about where the columns are.
        for g in span.content.graphemes(true) {
            let w = crate::compose::cluster_width(g);
            let on = col < to && col + w > from;
            if on != buf_on && !buf.is_empty() {
                out.push(Span::styled(
                    std::mem::take(&mut buf),
                    if buf_on {
                        style.add_modifier(Modifier::REVERSED)
                    } else {
                        style
                    },
                ));
            }
            buf_on = on;
            buf.push_str(g);
            col += w;
        }
        if !buf.is_empty() {
            out.push(Span::styled(
                buf,
                if buf_on {
                    style.add_modifier(Modifier::REVERSED)
                } else {
                    style
                },
            ));
        }
    }
    Line::from(out)
}

/// **THE MESSAGE BLOCK WIDTH** — the columns one speech act occupies, for
/// EITHER sender.
///
/// `width - MSG_GUTTER - `[`msg_margin`], capped at [`BOX_MAX`]. The agent
/// spends all of it on text, starting at the gutter, so its right margin is
/// exactly [`msg_margin`]; his box spends two of it on the box's own padding
/// and is pushed right, so its left margin is exactly [`msg_margin`] too.
///
/// **THE TWO LANES ARE THE SAME WIDTH AND NO LONGER THE SAME MEASURE.** They
/// used to wrap at one number — `box_width - 2` for both — which was possible
/// only while the margin was a leftover. Now that both margins are ten and only
/// one of the two lanes has a box to pay for, the wrap measures differ by
/// exactly the box's two columns of padding. What survives is the fact the eye
/// actually reads: the two blocks occupy the same span of the screen.
///
/// It returned `min(76, width - 6)` above 80 columns and `width - 4` at or
/// below — and the `76` is the defect: it bound at anything over 82, so the
/// margin was whatever the ceiling happened to leave.
pub fn box_width(width: usize) -> usize {
    BOX_MAX
        .min(width.saturating_sub(MSG_GUTTER + msg_margin(width)))
        .max(1)
}
