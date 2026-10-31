//! **THE MARKDOWN SUBSET** — exactly the things he named, and nothing else.
//!
//! > *Maybe you can detect markdown and handle it a little bit. e.g.:*
//! > *- auto structure tables to fill the whole width of the page*
//! > *- bold, italics*
//! > *- numbered, bulleted lists*
//! >
//! > *Should we handle headers? I don't think so.*
//!
//! …and then, in a later round, the fifth thing:
//!
//! > *`code` inline could be rendered as markdown. Just a different style.
//! > claude-code uses text color but I think a background color change would be
//! > nice too.*
//!
//! **INLINE CODE IS THE ONE SHAPE HERE THAT CARRIES A COLOUR, AND THIS MODULE
//! STILL CHOOSES NONE.** Bold and italic are modifiers on whatever style the
//! caller handed in, which is what keeps bold on paper black. A code span is a
//! foreground *and* a background, because he asked for both — so [`lay`] takes
//! that pair as a PARAMETER beside `base` and the palette decision stays where
//! the `base` decision already was, in `render::lay_out`, one arm per sender.
//! This module remains pure and remains ignorant of the palette.
//!
//! **HEADERS ARE RULED OUT, and that is a DECISION rather than an unbuilt
//! feature.** He asked and answered in the same breath, so there is no
//! `headers` flag here to turn on later: a `# ` line is prose and renders as
//! prose, hash and all. A feature flag would be an invitation to re-open a
//! question he closed.
//!
//! # The invariant that makes drag-select survive this module
//!
//! A message's LOGICAL text — what a copy takes, and what
//! [`crate::select::RowText::start`] is an offset into — is [`plain`], not the
//! source. [`plain`] is width-INDEPENDENT, and [`lay`] paints exactly the
//! characters [`plain`] produces, in the same order, for every shape except a
//! TABLE.
//!
//! **The table is the stated exception.** A grid's painted characters are its
//! cell text plus padding that depends on the terminal width, so a painted
//! table row is not a slice of any width-independent string. Those rows are
//! therefore emitted with `text: None` — the same thing a run header does —
//! which means a drag cannot ANCHOR on a table row and no highlight is painted
//! over one. A copy that SPANS the table still takes the whole table, because
//! [`crate::select::selected_text`] reads the unit's logical text from
//! [`plain`] and never from the screen. That is the honest trade: the grid is
//! for reading, the normalised `a | b | c` form is for the clipboard.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::geom::{self, text_width, Seg};

/// The bullet glyph a `-`, `*` or `+` list item is drawn with. One column, so
/// a nested list's indent arithmetic is the source's own.
pub const BULLET: char = '•';

/// The column separator between two table cells, with its single space each
/// side. Three columns of chrome per gap.
const CELL_SEP: &str = " │ ";

/// One painted row of a message.
#[derive(Clone)]
pub struct MdRow {
    pub spans: Vec<Span<'static>>,
    /// The characters of [`plain`] this row carries, and the offset of the
    /// first of them. `None` on a row that is not a slice of the logical text
    /// — a table row, or a table's rule.
    pub text: Option<String>,
    pub start: usize,
}

/// One logical line of a message, after the shape has been read off it.
enum Kind {
    /// Prose, or a list item: a run of inline pieces, already stripped of
    /// emphasis markers, plus the fixed prefix it is drawn under.
    Line { prefix: String, body: Vec<(String, Style)> },
    /// A table: rows of cell text, and whether the first row is a header (a
    /// `|---|---|` delimiter row followed it).
    Table { rows: Vec<Vec<String>>, header: bool },
}

impl Kind {
    /// The width-independent visible text of this piece, one entry per logical
    /// line. This is the ONE definition of what a copy takes.
    fn plain_lines(&self) -> Vec<String> {
        match self {
            Kind::Line { prefix, body } => {
                let mut s = prefix.clone();
                for (t, _) in body {
                    s.push_str(t);
                }
                vec![s]
            }
            Kind::Table { rows, .. } => rows.iter().map(|r| r.join(" | ")).collect(),
        }
    }
}

/// **THE LOGICAL TEXT OF A MESSAGE** — what a copy takes and what every
/// selection offset indexes into.
///
/// Width-independent by construction: nothing in here consults a terminal
/// width, so the same message has the same logical text at 60 columns and at
/// 200. That is what lets a selection survive a resize.
pub fn plain(src: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    // The code style is irrelevant to the logical text — the backticks are
    // gone from the piece TEXT either way, which is what keeps `plain` and
    // `lay` two readings of one string.
    for k in parse(src, Style::new()) {
        out.extend(k.plain_lines());
    }
    out.join("\n")
}

/// Paint a message to `width` columns.
///
/// `base` is the style the prose is drawn in — the assistant's ink, or the
/// paper box's black — and every emphasis this module applies is a MODIFIER on
/// top of it, never a different colour. Bold on paper must still be black.
///
/// `code` is the one exception and it is handed in for exactly that reason: a
/// backtick span is drawn in `base.patch(code)`, so a `code` carrying a
/// foreground and a background overrides both while a code span inside a bold
/// run still keeps its bold. Two callers, two pairs — see
/// [`crate::palette::code`] and [`crate::palette::code_on_paper`].
pub fn lay(src: &str, width: usize, base: Style, code: Style) -> Vec<MdRow> {
    let width = width.max(1);
    let mut out: Vec<MdRow> = Vec::new();
    let mut off = 0usize;
    let pieces = parse(src, code);
    for (i, k) in pieces.iter().enumerate() {
        if i > 0 {
            // The newline between two logical lines is one character of the
            // logical text that no row paints.
            off += 1;
        }
        match k {
            Kind::Line { prefix, body } => {
                let mut segs: Vec<Seg> = Vec::new();
                if !prefix.is_empty() {
                    segs.push(Seg::new(prefix.clone(), base));
                }
                for (t, st) in body {
                    segs.push(Seg::new(t.clone(), base.patch(*st)));
                }
                if segs.is_empty() {
                    segs.push(Seg::new(String::new(), base));
                }
                let rows = geom::wrap(&segs, width);
                let consumed = k.plain_lines()[0].chars().count();
                for r in rows {
                    let text = r.text();
                    out.push(MdRow {
                        start: off + r.start,
                        text: Some(text),
                        spans: r.spans,
                    });
                }
                off += consumed;
            }
            Kind::Table { rows, header } => {
                let lines = k.plain_lines();
                let consumed: usize = lines.iter().map(|l| l.chars().count()).sum::<usize>()
                    + lines.len().saturating_sub(1);
                out.extend(table_rows(rows, *header, width, base, off, &lines));
                off += consumed;
            }
        }
    }
    if out.is_empty() {
        out.push(MdRow {
            spans: vec![Span::styled(String::new(), base)],
            text: Some(String::new()),
            start: 0,
        });
    }
    out
}

// ── the parse ───────────────────────────────────────────────────────────────

fn parse(src: &str, code: Style) -> Vec<Kind> {
    let lines: Vec<&str> = src.split('\n').collect();
    let mut out: Vec<Kind> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if is_table_row(lines[i]) {
            let start = i;
            let mut rows: Vec<Vec<String>> = Vec::new();
            let mut header = false;
            while i < lines.len() && is_table_row(lines[i]) {
                if i == start + 1 && is_delimiter_row(lines[i]) && !rows.is_empty() {
                    // The `|---|---|` row is NOT content. It says the row
                    // above it is a header and then it is gone — it is never
                    // drawn and it is never copied.
                    header = true;
                    i += 1;
                    continue;
                }
                rows.push(cells(lines[i]));
                i += 1;
            }
            // A one-row "table" with no delimiter is a line that happens to
            // contain pipes. Grid-ing it would be a guess; leave it as prose.
            if rows.len() > 1 || header {
                let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
                for r in rows.iter_mut() {
                    r.resize(cols, String::new());
                }
                out.push(Kind::Table { rows, header });
                continue;
            }
            i = start;
        }
        out.push(line_kind(lines[i], code));
        i += 1;
    }
    out
}

/// The shape of one non-table line: a bullet, a numbered item, or prose.
fn line_kind(line: &str, code: Style) -> Kind {
    let indent = line.len() - line.trim_start().len();
    let pad = " ".repeat(indent.min(16));
    let rest = line.trim_start();
    // A BULLET: `- `, `* ` or `+ ` with a space after it. Without the space it
    // is arithmetic or a horizontal rule, not a list.
    if let Some(body) = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))
        .or_else(|| rest.strip_prefix("+ "))
    {
        return Kind::Line {
            prefix: format!("{pad}{BULLET} "),
            body: inline(body, code),
        };
    }
    // A NUMBERED ITEM: `1. ` / `12) `. The number is kept verbatim — his
    // ordering, not a renumbering of ours.
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() && digits.len() <= 3 {
        let after = &rest[digits.len()..];
        if let Some(body) = after.strip_prefix(". ").or_else(|| after.strip_prefix(") ")) {
            return Kind::Line {
                prefix: format!("{pad}{digits}. "),
                body: inline(body, code),
            };
        }
    }
    Kind::Line {
        prefix: pad,
        body: inline(rest, code),
    }
}

fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.len() > 1 && t.contains('|')
}

fn is_delimiter_row(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty()
        && t.chars()
            .all(|c| matches!(c, '|' | '-' | ':' | ' '))
        && t.contains('-')
}

fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|')
        .map(|c| {
            // Emphasis inside a cell is stripped rather than styled: a cell is
            // already being padded and re-flowed, and a bold run that survived
            // the padding but not the wrap would be the one inconsistency in
            // the grid. **A CODE SPAN IS STRIPPED THE SAME WAY, AND THAT IS A
            // DECISION** — a chip whose background stopped in the middle of a
            // padded cell would read as a broken grid, and a chip padded to
            // the column would read as a cell of a different colour. The
            // backticks still go; only the style does not arrive.
            inline(c.trim(), Style::new())
                .into_iter()
                .map(|(s, _)| s)
                .collect()
        })
        .collect()
}

// ── inline emphasis ─────────────────────────────────────────────────────────

/// Split a line into `(text, style-delta)` pieces, with `**bold**`, `__bold__`,
/// `*italic*`, `_italic_` and `` `code` `` consumed and their markers removed.
///
/// **An UNMATCHED marker is literal.** `a * b` is arithmetic, a lone `*` that
/// opened a run nothing closed must not swallow the rest of the message, and
/// the same is true of a lone backtick: `` a ` b `` keeps its backtick on
/// screen rather than turning the rest of the line into code.
///
/// **THE EDGE CASES, DECIDED** (the whole list, because a delimiter grammar
/// with undecided edges is a grammar that guesses):
///
/// - **An unmatched backtick is literal**, as above — the same rule the
///   emphasis markers already had, and the reason `2 * 3` survives.
/// - **A DOUBLE-BACKTICK span is a span**, `` ``like this`` ``, and its
///   delimiter is the pair. That is the only way to put a backtick INSIDE a
///   code span, which is why it exists and not because CommonMark has it.
/// - **A backtick inside a single-backtick span closes it.** There is no
///   escape grammar in this subset and inventing one would be a fifth thing he
///   did not ask for.
/// - **An EMPTY span is literal.** ``` `` ``` and ``` ```` ``` are two and four
///   backticks on screen, matching what `**` already does.
/// - **A code span never crosses a newline**, because [`find_close`] stops
///   there and because [`parse`] works one source line at a time anyway. A
///   backtick at the end of a line cannot swallow the line below it.
/// - **A span may WRAP.** The style travels with the characters through
///   [`geom::wrap`], so a span broken across two rows paints a chip on each —
///   and the chip covers only the characters, never the row's trailing pad.
/// - **EMPHASIS AND CODE NEST ONE WAY, STRUCTURALLY.** A code span inside a
///   bold run is honoured and keeps the bold ([`code_pieces`]); an asterisk
///   inside a code span is verbatim, because the content of a code span is the
///   one thing in markdown that is not markup — and because
///   [`code_span_at`] hands its inner text back untouched, there is nowhere
///   for a second reading of it to happen.
/// - **Inside a table cell both are stripped.** See [`cells`].
fn inline(src: &str, code: Style) -> Vec<(String, Style)> {
    let ch: Vec<char> = src.chars().collect();
    let mut out: Vec<(String, Style)> = Vec::new();
    let mut buf = String::new();
    let mut i = 0usize;
    while i < ch.len() {
        let c = ch[i];
        if let Some((span, next)) = code_span_at(&ch, i) {
            if !buf.is_empty() {
                out.push((std::mem::take(&mut buf), Style::new()));
            }
            out.push((span, code));
            i = next;
            continue;
        }
        if c == '*' || c == '_' {
            let double = i + 1 < ch.len() && ch[i + 1] == c;
            let marker: String = if double {
                [c, c].iter().collect()
            } else {
                c.to_string()
            };
            let from = i + marker.len();
            // An underscore INSIDE a word is a character, not a marker
            // (CommonMark's intraword rule): `expand_on_init` stays one word.
            // It neither opens after a letter or digit nor closes before one.
            let word = |j: usize| ch.get(j).is_some_and(|x| x.is_alphanumeric());
            let mut close = if c == '_' && i > 0 && word(i - 1) {
                None
            } else {
                find_close(&ch, from, &marker, true)
            };
            if c == '_' {
                while let Some(e) = close.filter(|&e| word(e + marker.len())) {
                    close = find_close(&ch, e + 1, &marker, true);
                }
            }
            if let Some(end) = close {
                let inner: String = ch[from..end].iter().collect();
                if !inner.is_empty() {
                    if !buf.is_empty() {
                        out.push((std::mem::take(&mut buf), Style::new()));
                    }
                    let style = if double {
                        Style::new().add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().add_modifier(Modifier::ITALIC)
                    };
                    // A code span inside the run keeps the run's modifier AND
                    // gains the chip; everything else is one piece as before.
                    out.extend(code_pieces(&inner, style, code));
                    i = end + marker.len();
                    continue;
                }
            }
        }
        buf.push(c);
        i += 1;
    }
    if !buf.is_empty() {
        out.push((buf, Style::new()));
    }
    out
}

/// **THE ONE DEFINITION OF WHAT A CODE SPAN IS.** A span opening at `i`, as
/// `(its text, the index after its closing marker)`, or `None` if `i` does not
/// open one.
///
/// One or two backticks; the inner text is taken VERBATIM, which is what makes
/// "an asterisk inside a code span is a character" structural rather than a
/// rule somebody has to remember. `strict: false` because the space guard in
/// [`find_close`] exists to stop arithmetic reading as emphasis and there is no
/// arithmetic spelled with backticks — `` `ls -l ` `` is a code span with a
/// trailing space in it, which is a thing people write.
///
/// It exists as its own function because [`inline`] and [`code_pieces`] both
/// need it, and two scanners that disagreed about where a span ends would
/// disagree about what a copy takes.
fn code_span_at(ch: &[char], i: usize) -> Option<(String, usize)> {
    if ch[i] != '`' {
        return None;
    }
    let double = i + 1 < ch.len() && ch[i + 1] == '`';
    let marker = if double { "``" } else { "`" };
    let from = i + marker.len();
    let end = find_close(ch, from, marker, false)?;
    let inner: String = ch[from..end].iter().collect();
    // An EMPTY span is literal, exactly as `**` is.
    (!inner.is_empty()).then_some((inner, end + marker.len()))
}

/// The inside of an emphasis run, split on its code spans.
///
/// The emphasis path takes its inner text verbatim — that is what stops `*` in
/// the middle of a bold run re-opening anything — so code spans inside it need
/// their own pass. `emph` is the run's modifier and survives on every piece;
/// `code` is patched on top of it for the spans themselves.
///
/// **IT SCANS CODE SPANS AND NOTHING ELSE, AND THAT IS NOT A SIMPLIFICATION —
/// IT IS WHY THIS TERMINATES IN BOUNDED STACK.** The first cut called [`inline`]
/// here, which reads emphasis too, so an emphasis run nested inside an emphasis
/// run recursed: `_*_*_*…x…*_*_*_` is one frame per level and two source
/// characters per frame, which makes a long enough message a stack overflow in
/// a crate whose whole contract is that it cannot panic (`0018`). Scanning only
/// code spans makes the call depth exactly one, for any input.
fn code_pieces(inner: &str, emph: Style, code: Style) -> Vec<(String, Style)> {
    let ch: Vec<char> = inner.chars().collect();
    let mut out: Vec<(String, Style)> = Vec::new();
    let mut buf = String::new();
    let mut i = 0usize;
    while i < ch.len() {
        if let Some((span, next)) = code_span_at(&ch, i) {
            if !buf.is_empty() {
                out.push((std::mem::take(&mut buf), emph));
            }
            out.push((span, emph.patch(code)));
            i = next;
            continue;
        }
        buf.push(ch[i]);
        i += 1;
    }
    if !buf.is_empty() {
        out.push((buf, emph));
    }
    // `inline` only calls this on a non-empty run, so this is unreachable — but
    // an empty piece list would silently delete the run's text, which is worse
    // than a redundant line.
    if out.is_empty() {
        out.push((inner.to_string(), emph));
    }
    out
}

/// Where the run opened at `from` closes, or `None` if it never does.
///
/// With `strict`, a closing marker may not be preceded by a space (`a *b * c`
/// does not close on the second star), which is what stops arithmetic reading
/// as emphasis. Backticks pass `strict: false` — see [`inline`].
fn find_close(ch: &[char], from: usize, marker: &str, strict: bool) -> Option<usize> {
    let m: Vec<char> = marker.chars().collect();
    let mut i = from;
    while i + m.len() <= ch.len() {
        if ch[i..i + m.len()] == m[..] && i > from && (!strict || ch[i - 1] != ' ') {
            return Some(i);
        }
        if ch[i] == '\n' {
            return None;
        }
        i += 1;
    }
    None
}

// ── the table ───────────────────────────────────────────────────────────────

/// **AUTO-STRUCTURE A TABLE TO FILL THE PAGE WIDTH** — his one hard ask in this
/// scope, and the answer to "what does it do when it cannot fit".
///
/// Three regimes, and the narrow one is the interesting one:
///
/// 1. **It fits with room to spare.** Every column gets its natural width and
///    the SLACK is handed out in proportion to those widths, so the grid ends
///    flush with the right margin instead of leaving a ragged gap.
/// 2. **It does not fit.** Columns are shrunk in proportion to their natural
///    widths, never below one column each, and every cell WRAPS inside its own
///    column. A logical row then occupies as many screen rows as its tallest
///    cell needs. **Nothing is ellipsized and nothing is dropped** — the table
///    grows DOWN, which is the same answer the info blobs give and the same
///    answer the firm margins force.
/// 3. **It cannot fit at all** — fewer columns available than `3(n-1) + n`, so
///    not even one character per column survives. The grid is ABANDONED and the
///    rows are drawn as ordinary wrapped prose, `a | b | c`. A grid whose
///    columns are narrower than one character is not a degraded table, it is an
///    unreadable one, and the prose form still carries every word.
fn table_rows(
    rows: &[Vec<String>],
    header: bool,
    width: usize,
    base: Style,
    off: usize,
    lines: &[String],
) -> Vec<MdRow> {
    let n = rows.first().map(Vec::len).unwrap_or(0);
    let chrome = CELL_SEP.chars().count() * n.saturating_sub(1);
    if n == 0 || width < chrome + n {
        // REGIME 3 — no grid. The rows are prose, and because their painted
        // characters are then EXACTLY their logical characters, these rows are
        // addressable again.
        let mut out = Vec::new();
        let mut at = off;
        for (i, l) in lines.iter().enumerate() {
            if i > 0 {
                at += 1;
            }
            for r in geom::wrap(&[Seg::new(l.clone(), base)], width) {
                out.push(MdRow {
                    start: at + r.start,
                    text: Some(r.text()),
                    spans: r.spans,
                });
            }
            at += l.chars().count();
        }
        return out;
    }
    let budget = width - chrome;
    let natural: Vec<usize> = (0..n)
        .map(|c| {
            rows.iter()
                .map(|r| text_width(&r[c]))
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect();
    let total: usize = natural.iter().sum();
    let mut cols: Vec<usize> = if total == 0 {
        vec![budget / n; n]
    } else {
        natural
            .iter()
            .map(|w| (w * budget / total).max(1))
            .collect()
    };
    // Hand the rounding remainder to the widest columns, so the grid ends
    // EXACTLY on the right margin. A table one column short of the margin is
    // the tell that a renderer is guessing.
    let mut used: usize = cols.iter().sum();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(natural[i]));
    let mut k = 0usize;
    while used < budget {
        cols[order[k % n]] += 1;
        used += 1;
        k += 1;
    }
    while used > budget {
        let i = order[k % n];
        if cols[i] > 1 {
            cols[i] -= 1;
            used -= 1;
        }
        k += 1;
        if k > n * budget + n {
            break;
        }
    }

    let mut out: Vec<MdRow> = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        let style = if header && ri == 0 {
            base.add_modifier(Modifier::BOLD)
        } else {
            base
        };
        // Wrap every cell to its own column, then read the grid off row by row.
        let wrapped: Vec<Vec<String>> = row
            .iter()
            .enumerate()
            .map(|(c, cell)| {
                geom::wrap(&[Seg::new(cell.clone(), style)], cols[c])
                    .iter()
                    .map(geom::WrapRow::text)
                    .collect()
            })
            .collect();
        let tall = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
        for line in 0..tall {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for c in 0..n {
                if c > 0 {
                    spans.push(Span::styled(CELL_SEP.to_string(), base));
                }
                let t = wrapped[c].get(line).cloned().unwrap_or_default();
                let pad = cols[c].saturating_sub(text_width(&t));
                spans.push(Span::styled(t, style));
                if pad > 0 {
                    spans.push(Span::styled(" ".repeat(pad), base));
                }
            }
            // **NOT ADDRESSABLE** — see the module header. The grid's
            // characters are not a slice of the logical text.
            out.push(MdRow {
                spans,
                text: None,
                start: off,
            });
        }
        if header && ri == 0 {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (c, w) in cols.iter().enumerate() {
                if c > 0 {
                    spans.push(Span::styled(CELL_SEP.to_string(), base));
                }
                spans.push(Span::styled("─".repeat(*w), base));
            }
            out.push(MdRow {
                spans,
                text: None,
                start: off,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A style no prose can be drawn in by accident, so an assertion that a
    /// span carries it is an assertion about the CODE path and not about the
    /// default. `REVERSED` is deliberately not a colour: this module must be
    /// provable without naming one.
    fn code_style() -> Style {
        Style::new().add_modifier(Modifier::REVERSED)
    }

    fn painted(src: &str, w: usize) -> Vec<String> {
        lay(src, w, Style::new(), code_style())
            .iter()
            .map(|r| r.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// The spans of row 0 that were drawn in the code style, in screen order.
    fn coded(src: &str, w: usize) -> Vec<String> {
        lay(src, w, Style::new(), code_style())
            .first()
            .map(|r| {
                r.spans
                    .iter()
                    .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
                    .map(|s| s.content.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// **HEADERS ARE NOT A FEATURE AND THERE IS NO FLAG.** A `# ` line is
    /// prose, hash and all — which is what "I don't think so" means when it is
    /// an answer rather than a deferral.
    #[test]
    fn a_header_is_prose_and_keeps_its_hash() {
        assert_eq!(painted("# Title", 40), vec!["# Title"]);
        assert_eq!(plain("## Two"), "## Two");
    }

    #[test]
    fn bold_and_italics_lose_their_markers_and_gain_a_modifier() {
        let rows = lay("a **b** c *d* e", 40, Style::new(), code_style());
        let bold: Vec<&str> = rows[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(bold, vec!["b"]);
        let ital: Vec<&str> = rows[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::ITALIC))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(ital, vec!["d"]);
        assert_eq!(plain("a **b** c *d* e"), "a b c d e");
    }

    /// Arithmetic is not emphasis. An unmatched marker stays on screen.
    #[test]
    fn an_unmatched_marker_is_literal() {
        assert_eq!(plain("2 * 3 = 6"), "2 * 3 = 6");
        assert_eq!(plain("a *b and nothing closes it"), "a *b and nothing closes it");
    }

    #[test]
    fn lists_get_a_bullet_and_keep_their_numbers() {
        assert_eq!(plain("- one\n- two"), "• one\n• two");
        assert_eq!(plain("1. one\n2. two"), "1. one\n2. two");
        // Indentation survives, so a nested list still reads as nested.
        assert_eq!(plain("  - deep"), "  • deep");
        // No space after the dash is not a list.
        assert_eq!(plain("-not a list"), "-not a list");
    }

    /// **THE GRID FILLS THE WIDTH.** Wherever a grid is drawn at all, every one
    /// of its rows is EXACTLY `w` columns — that is what "auto structure tables
    /// to fill the whole width of the page" means, and it is also what keeps
    /// the firm margins firm: a short grid row would leave the box background
    /// ragged.
    ///
    /// Below the grid floor there is no grid, so the rule there is only that no
    /// row may EXCEED the width it was given (`0018`: over-report panics
    /// ratatui, under-report is merely a stale column).
    #[test]
    fn a_table_fills_the_width_exactly_wherever_a_grid_is_drawn() {
        let src = "| a | bb |\n|---|---|\n| 1 | 2 |";
        for w in [100usize, 80, 60, 40, 24, 12, 8, 5, 4, 2, 1] {
            let rows = painted(src, w);
            let grid = rows.iter().any(|r| r.contains('\u{2502}'));
            for row in &rows {
                if grid {
                    assert_eq!(text_width(row), w, "grid at {w}: {row:?}");
                } else {
                    assert!(text_width(row) <= w, "prose at {w}: {row:?}");
                }
            }
        }
    }

    /// The narrow regime: nothing is ellipsized, the row grows DOWN.
    #[test]
    fn a_table_too_narrow_to_fit_wraps_its_cells_rather_than_cutting_them() {
        let src = "| alpha beta | gamma delta |\n|---|---|";
        let rows = painted(src, 20);
        let all: String = rows.join("");
        for word in ["alpha", "beta", "gamma", "delta"] {
            assert!(all.contains(word), "{word} was dropped: {rows:?}");
        }
    }

    /// Regime 3: the grid is abandoned and the prose still carries every word.
    #[test]
    fn a_table_that_cannot_fit_at_all_falls_back_to_prose() {
        let src = "| a | b | c | d |\n|---|---|---|---|\n| 1 | 2 | 3 | 4 |";
        // 3*(4-1) + 4 = 13 columns is the floor for a grid of four.
        let rows = painted(src, 6);
        let all: String = rows.join("");
        assert!(!all.contains('│'), "a grid was drawn below its floor: {rows:?}");
        for word in ["a", "b", "c", "d", "1", "2", "3", "4"] {
            assert!(all.contains(word), "{word} was dropped: {rows:?}");
        }
    }

    /// The delimiter row is never content — not on screen, not on the
    /// clipboard.
    #[test]
    fn the_delimiter_row_is_not_copied() {
        let p = plain("| a | b |\n|---|---|\n| 1 | 2 |");
        assert_eq!(p, "a | b\n1 | 2");
    }

    /// A line with pipes that is not a table stays prose — grid-ing it would
    /// be a guess.
    #[test]
    fn one_piped_line_is_not_a_table() {
        assert_eq!(plain("| just a pipe"), "| just a pipe");
    }

    // ── inline `code` ──────────────────────────────────────────────────────

    /// **A CODE SPAN LOSES ITS BACKTICKS AND GAINS THE STYLE — AND A LINE WITH
    /// NO BACKTICKS GAINS NOTHING.**
    ///
    /// Both directions, in one test, because only the pair is evidence: an
    /// assertion that the span is styled would also pass on a module that
    /// styled every span it drew.
    #[test]
    fn a_code_span_is_styled_and_a_line_without_one_is_not() {
        assert_eq!(coded("run `cargo test` now", 40), vec!["cargo test"]);
        assert_eq!(painted("run `cargo test` now", 40), vec!["run cargo test now"]);
        assert_eq!(plain("run `cargo test` now"), "run cargo test now");
        // …and the backtick-free line: no styled span at all.
        assert!(
            coded("run cargo test now", 40).is_empty(),
            "prose with no backticks was drawn in the code style"
        );
    }

    /// **THE BACKTICKS ARE NOT PART OF THE SPAN**, on screen or on the
    /// clipboard.
    #[test]
    fn the_backticks_are_never_drawn() {
        // **DELIMITERS, not content**: `` ``a`b`` `` is excluded on purpose —
        // the backtick it keeps is INSIDE the span, which is the only reason
        // the double delimiter exists, and it has its own test below.
        for src in ["`a`", "x `a` y", "`a` and `b`", "``plain``"] {
            assert!(
                !painted(src, 80).concat().contains('`'),
                "a backtick survived: {src:?} -> {:?}",
                painted(src, 80)
            );
            assert!(!plain(src).contains('`'), "a backtick reached the clipboard: {src:?}");
        }
        assert_eq!(coded("`a` and `b`", 80), vec!["a", "b"]);
    }

    /// **AN UNMATCHED BACKTICK IS LITERAL** — the same rule `2 * 3` gets, and
    /// the reason one stray backtick cannot turn the rest of a message into a
    /// chip.
    #[test]
    fn an_unmatched_backtick_is_literal() {
        assert_eq!(plain("a ` b"), "a ` b");
        assert!(coded("a ` b", 40).is_empty());
        assert_eq!(plain("cost is 40`"), "cost is 40`");
        // An EMPTY span is literal too, exactly as `**` is.
        assert_eq!(plain("`` here"), "`` here");
        assert!(coded("`` here", 40).is_empty());
    }

    /// **AN UNDERSCORE INSIDE A WORD IS A CHARACTER.** `expand_on_init` is one
    /// identifier, not `expand` + italic `on` + `init`; `_this_` is still
    /// emphasis, and so is `*a*b`.
    #[test]
    fn an_underscore_inside_a_word_is_not_emphasis() {
        assert_eq!(plain("set expand_on_init now"), "set expand_on_init now");
        assert_eq!(plain("a_b and c_d"), "a_b and c_d");
        assert_eq!(plain("say _this_ loudly"), "say this loudly");
        assert_eq!(plain("_open_ed"), "_open_ed");
        assert_eq!(plain("__bold__ and snake__case"), "bold and snake__case");
        assert_eq!(plain("*a*b"), "ab");
    }

    /// **A DOUBLE-BACKTICK SPAN IS THE ONLY WAY TO PUT A BACKTICK INSIDE
    /// CODE**, and inside a single-backtick span a backtick closes it.
    #[test]
    fn a_double_backtick_span_carries_a_backtick_and_a_single_one_does_not() {
        assert_eq!(coded("``a`b``", 40), vec!["a`b"]);
        assert_eq!(plain("``a`b``"), "a`b");
        // Single delimiter: the second backtick CLOSES, so `b` is prose.
        assert_eq!(coded("`a`b`", 40), vec!["a"]);
        assert_eq!(plain("`a`b`"), "ab`");
    }

    /// **A CODE SPAN NEVER CROSSES A NEWLINE.** A backtick at the end of a
    /// line cannot swallow the line below it.
    #[test]
    fn a_code_span_does_not_cross_a_line() {
        assert_eq!(plain("open `a\nb` close"), "open `a\nb` close");
        let rows = lay("open `a\nb` close", 40, Style::new(), code_style());
        assert!(
            rows.iter().all(|r| r
                .spans
                .iter()
                .all(|s| !s.style.add_modifier.contains(Modifier::REVERSED))),
            "a span closed across a newline"
        );
    }

    /// **A SPAN THAT WRAPS PAINTS A CHIP ON EACH ROW**, and the chip covers
    /// only the characters — the row's pad is the caller's style, not the
    /// code's.
    #[test]
    fn a_code_span_that_wraps_keeps_its_style_on_both_rows() {
        let src = "`aaaaaaaaaa bbbbbbbbbb cccccccccc`";
        let rows = lay(src, 12, Style::new(), code_style());
        assert!(rows.len() > 1, "the fixture did not wrap into more than one row");
        for (i, r) in rows.iter().enumerate() {
            assert!(
                r.spans
                    .iter()
                    .any(|s| s.style.add_modifier.contains(Modifier::REVERSED)),
                "row {i} lost the chip: {:?}",
                r.spans
            );
        }
        assert_eq!(plain(src), "aaaaaaaaaa bbbbbbbbbb cccccccccc");
    }

    /// **EMPHASIS AND CODE NEST ONE WAY.** A code span inside a bold run keeps
    /// the bold and gains the chip; an asterisk inside a code span is verbatim,
    /// because the content of a code span is the one thing here that is not
    /// markup.
    #[test]
    fn code_nests_inside_emphasis_and_emphasis_does_not_nest_inside_code() {
        let rows = lay("**a `b` c**", 40, Style::new(), code_style());
        let both: Vec<&str> = rows[0]
            .spans
            .iter()
            .filter(|s| {
                s.style.add_modifier.contains(Modifier::BOLD)
                    && s.style.add_modifier.contains(Modifier::REVERSED)
            })
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(both, vec!["b"], "a code span inside bold lost one of the two");
        assert_eq!(plain("**a `b` c**"), "a b c");
        // A bold run with NO backticks is bit-for-bit what it was: ONE piece.
        let plainbold = lay("**a b c**", 40, Style::new(), code_style());
        let bold: Vec<&str> = plainbold[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(bold, vec!["a b c"]);
        // The other way: `*` inside code is a character.
        assert_eq!(coded("`a *b* c`", 40), vec!["a *b* c"]);
        assert_eq!(plain("`a *b* c`"), "a *b* c");
    }

    /// **A PATHOLOGICAL MARKER SOUP TERMINATES IN BOUNDED STACK.**
    ///
    /// The regression this guards is one I introduced and then removed: a
    /// `code_pieces` that recursed into `inline` was one stack frame per nested
    /// emphasis level and two source characters per frame, so a long enough
    /// message overflowed the stack — in a crate whose contract is that it
    /// cannot panic. 40,000 alternating markers is far past the depth that
    /// would have killed it.
    #[test]
    fn a_wall_of_nested_markers_neither_recurses_nor_loses_its_text() {
        let deep = format!("{}x{}", "_*".repeat(10_000), "*_".repeat(10_000));
        let p = plain(&deep);
        assert!(p.contains('x'), "the payload was lost");
        // And it still paints, at a real width, without panicking.
        let rows = lay(&deep, 40, Style::new(), code_style());
        assert!(!rows.is_empty());
        // The same wall, spelled with backticks.
        let ticks = format!("{}x{}", "`".repeat(5_000), "`".repeat(5_000));
        assert!(plain(&ticks).contains('x'), "the payload was lost in backticks");
        assert!(!lay(&ticks, 40, Style::new(), code_style()).is_empty());
    }

    /// A code span inside a table cell is STRIPPED, not styled — the decision
    /// [`cells`] documents.
    #[test]
    fn a_code_span_in_a_table_cell_is_stripped_of_its_backticks_and_its_style() {
        let src = "| `a` | b |\n|---|---|\n| 1 | 2 |";
        assert_eq!(plain(src), "a | b\n1 | 2");
        let all: String = painted(src, 40).concat();
        assert!(!all.contains('`'), "a backtick survived the grid: {all:?}");
    }

    /// **EVERY ADDRESSABLE ROW IS A SLICE OF `plain`, AT THE OFFSET IT
    /// CLAIMS.** This is the property drag-select rests on; break it and a copy
    /// silently takes the wrong characters.
    #[test]
    fn every_addressable_row_is_a_true_slice_of_the_logical_text() {
        for src in [
            "a **b** c",
            "- one\n- two\n\n1. three",
            "the quick brown fox jumps over the lazy dog and keeps going",
            "| a | b |\n|---|---|\n| 1 | 2 |",
            "before\n| a | b |\n|---|---|\n| 1 | 2 |\nafter",
            // **THE CODE PATH IS IN HERE TOO**, because a delimiter this
            // module removes is exactly how `plain` and `lay` come apart: they
            // must remove the same characters at the same offsets.
            "run `cargo test -p tui-render` and then `cargo clippy` to finish",
            "- a `code` item\n- **bold with `code` in it**\n1. ``a`b``",
        ] {
            let p = plain(src);
            let chars: Vec<char> = p.chars().collect();
            for w in [80usize, 20, 7] {
                for r in lay(src, w, Style::new(), code_style()) {
                    let Some(t) = r.text.as_ref() else { continue };
                    let n = t.chars().count();
                    let got: String = chars
                        .iter()
                        .skip(r.start)
                        .take(n)
                        .collect();
                    assert_eq!(&got, t, "src={src:?} w={w} start={}", r.start);
                }
            }
        }
    }
}
