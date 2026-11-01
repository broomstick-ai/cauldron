//! **The visual system** (8e4fca10 msg 186; task 0166): the few shapes every
//! tab is drawn with, in ONE place, so no tab can look different from the
//! others by accident.
//!
//! - **The band line** ([`band_line`]): `▾ Name` on the left and its
//!   METADATA flush right, in the pane's rightmost columns — no rule or
//!   leader running between them ("We shouldn't have those lines pointing to
//!   the status all the way on the right"). The metadata is written the way
//!   the cluster Status tab writes its ripples ("that looks the best ... make
//!   them all look like that everywhere"): `glyph n word` segments ([`seg`])
//!   joined by a muted ` · ` ([`meta`]), muted unless the segment signals —
//!   good states in the cool colour, waiting in amber, failure in red.
//! - **The card** ([`card_top`], [`card_close`]): the one box, and ONLY for a
//!   node or an artifact ("We should just use boxes to represent nodes or
//!   artifacts"). It is the question card ("The question card looks the
//!   best. So let's just get everything to match the question card"): a
//!   rounded border, its id as a legend at the top right, its body inside;
//!   the border's colour is the node's status. An ARTIFACT card is the same
//!   card with square corners and its file's name at the top left.
//! - **The table** ([`columns`], [`table`]): enumerated rows at one level line
//!   their columns up, and an opened item's details are a two-column table
//!   of its fields ("I'm looking for a table. So just give me a table").
//! - **Selection** ([`paint`] with [`palette::Colors::sel_bg`]): a subtle background over the
//!   selected rows — no bar, no reserved gutter, anywhere.
//! - **The modal** ([`palette::Colors::modal_bg`], [`palette::Colors::code`]): a thick outer border, a box
//!   inside it, and its own background colour, so it can never be taken for
//!   another box on the page.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tui_render::palette;

use crate::cauldron::view::{crop, display_width};

/// The selection background for a pane: [`palette::Colors::sel_bg`] when it has the focus.
pub fn sel_bg(focused: bool) -> Color {
    if focused {
        palette::c().sel_bg
    } else {
        palette::c().sel_bg_dim
    }
}

/// Repaint every cell of `area` whose background is the chrome's
/// ([`palette::Colors::surface`]) with `bg`. A cell already painted something else
/// — a tag chip, a toggle's segment — keeps it.
pub fn paint(buf: &mut Buffer, area: Rect, bg: Color) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                if c.bg == palette::c().surface || c.bg == Color::Reset {
                    c.set_bg(bg);
                }
            }
        }
    }
}

// ── metadata ──────────────────────────────────────────────────────────────

/// What a metadata segment says about its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Nothing to signal: muted.
    Plain,
    /// A good state alive now — running, online (`palette::code`).
    Good,
    /// Waiting on something, or near a limit (amber).
    Warn,
    /// A failure or a refusal (red).
    Bad,
}

pub fn tone(t: Tone) -> Style {
    match t {
        Tone::Plain => palette::muted(),
        Tone::Good => palette::code(),
        Tone::Warn => palette::amber(),
        Tone::Bad => palette::red(),
    }
}

/// One metadata segment, `glyph n word` (the glyph left out when empty):
/// `▶ 3 running`, `◆ 1 parked`, `12 nodes`.
pub fn seg(glyph: &str, n: impl std::fmt::Display, word: &str, t: Tone) -> Span<'static> {
    let mut s = String::new();
    if !glyph.is_empty() {
        s.push_str(glyph);
        s.push(' ');
    }
    s.push_str(&n.to_string());
    if !word.is_empty() {
        s.push(' ');
        s.push_str(word);
    }
    Span::styled(s, tone(t))
}

/// A metadata segment that is a word, not a count: `v5`, `main @ 06cada2b`.
pub fn word(s: impl Into<String>, t: Tone) -> Span<'static> {
    Span::styled(s.into(), tone(t))
}

/// `n` and its NOUN (8e4fca10 msg 403: every band header's metadata is a
/// noun count — "35 artifacts", "16 ripples", "2 directives" — never a bare
/// number): `1 artifact`, `2 entries`.
pub fn noun(n: impl std::fmt::Display, one: &str) -> String {
    let s = n.to_string();
    if s == "1" { format!("{s} {one}") } else { format!("{s} {}", plural_of(one)) }
}

/// The plural of noun `one`, as the face's nouns take it.
pub fn plural_of(one: &str) -> String {
    let (head, last) = one.rsplit_once(' ').map_or(("", one), |(h, l)| (h, l));
    let pl = if last.eq_ignore_ascii_case("data") {
        last.to_string()
    } else if let Some(stem) = last.strip_suffix('y').filter(|s| !s.ends_with(['a', 'e', 'o', 'u'])) {
        format!("{stem}ies")
    } else if last.ends_with('s') || last.ends_with("sh") || last.ends_with("ch") || last.ends_with('x') {
        format!("{last}es")
    } else {
        format!("{last}s")
    };
    if head.is_empty() { pl } else { format!("{head} {pl}") }
}

/// Segments joined by a muted ` · ` — the band line's metadata.
pub fn meta(parts: impl IntoIterator<Item = Span<'static>>) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for p in parts {
        if p.content.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Span::styled(" · ", palette::muted()));
        }
        out.push(p);
    }
    out
}

/// `▶ 9 running · ◆ 3 parked · ◇ 4 pending` — the ripples' counts, the
/// metadata every band is written like; running and parked `—` when the
/// threads daemon does not answer.
pub fn three_words(running: usize, parked: usize, pending: usize, live: bool) -> Vec<Span<'static>> {
    let n = |x: usize| if live { x.to_string() } else { "—".into() };
    meta([
        seg("▶", n(running), "running", if live && running > 0 { Tone::Good } else { Tone::Plain }),
        seg("◆", n(parked), "parked", if live && parked > 0 { Tone::Warn } else { Tone::Plain }),
        seg("◇", pending, "pending", Tone::Plain),
    ])
}

fn w(spans: &[Span]) -> usize {
    spans.iter().map(|s| display_width(&s.content)).sum()
}

/// `left`, then `right` flush against the right edge of `width`. The left
/// crops (with …) so the right always stands; a right wider than all but
/// eight columns crops too.
pub fn lr(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let right = fit_spans(right, width.saturating_sub(8).max(1));
    let rw = w(&right);
    let avail = width.saturating_sub(rw + if rw > 0 { 2 } else { 0 });
    let mut out = fit_spans(left, avail);
    let used = w(&out);
    if rw > 0 {
        out.push(Span::raw(" ".repeat(width.saturating_sub(used + rw))));
        out.extend(right);
    }
    Line::from(out)
}

/// Spans cropped (with …) to `width` columns.
pub fn fit_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut used = 0;
    let mut out = Vec::new();
    for s in spans {
        let sw = display_width(&s.content);
        if used + sw > width {
            let room = width.saturating_sub(used);
            if room > 0 {
                out.push(Span::styled(crop(&s.content, room), s.style));
            }
            break;
        }
        used += sw;
        out.push(s);
    }
    out
}

// ── the band line ─────────────────────────────────────────────────────────

/// The disclosure glyph of a band.
pub fn chevron(open: bool) -> Span<'static> {
    Span::styled(if open { "▾ " } else { "▸ " }, palette::muted())
}

/// A band that does not close (8e4fca10 msg 235: "we can have bands that are
/// not collapsible. So instead of it being a chevron, it could just be a
/// dot ... We'll call that an always expanded band"): THE band line with a
/// dot where the chevron would be, so nothing on it looks like it toggles.
pub fn fixed_band_line(depth: usize, name: Vec<Span<'static>>, meta: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut left = vec![Span::raw("  ".repeat(depth)), Span::styled("• ", palette::muted())];
    left.extend(name);
    let meta = fit_spans(meta, width.saturating_sub(w(&left) + 2));
    lr(left, meta, width)
}

// ── the tab strip ─────────────────────────────────────────────────────────

/// THE GLOBAL BAR's background (V1.1, product spec §12: the two bars in
/// "different colors, like the global above and then the environment
/// below"): the warm [`palette::Colors::toggle_bg`], so it never reads as the
/// cool environment bar under it. Both are palette roles every ink is
/// already checked on (`palette::BG_ROLES`), in both themes.
pub fn global_bar_bg() -> Color {
    palette::c().toggle_bg
}

/// THE ENVIRONMENT BAR's background (V1.1): a tab strip's own,
/// [`palette::Colors::strip_bg`] — it is the bar the strips under it belong to.
pub fn env_bar_bg() -> Color {
    palette::c().strip_bg
}

/// What stands between two menu items on a strip (b964d571 msg 23).
pub const STRIP_SEP: &str = "|";

/// How wide a strip of `labels` is drawn: each label and its two pads, and
/// a [`STRIP_SEP`] between each two.
pub fn strip_width(labels: &[String]) -> usize {
    labels.iter().map(|l| display_width(l) + 2).sum::<usize>() + labels.len().saturating_sub(1) * display_width(STRIP_SEP)
}

/// A tab strip as one line of `width` on [`palette::Colors::strip_bg`]: each label padded one
/// column on each side, a muted [`STRIP_SEP`] between two labels (msg 23
/// of b964d571: "the prod cauldron config tab looks like two words. maybe we
/// put a | between menu items in general"), the shown one bold and lit
/// (`lit`: the strip's pane holds the keys), the others muted; and each
/// label's (column, width) for its click hotspot — the separator is in no
/// label's. When the strip is itself the selected row (`selected`,
/// Status's and Config's), its shown label carries the selection's
/// background — the selection is a background everywhere (rule 10).
pub fn strip(labels: &[String], shown: usize, lit: bool, selected: bool, width: usize) -> (Line<'static>, Vec<(usize, usize)>) {
    let mut spans = Vec::new();
    let mut hotspots = Vec::new();
    let mut x = 0;
    for (i, l) in labels.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(STRIP_SEP, palette::muted().bg(palette::c().strip_bg)));
            x += display_width(STRIP_SEP);
        }
        let text = format!(" {l} ");
        let tw = display_width(&text);
        let style = if i == shown {
            if lit { palette::paper().add_modifier(Modifier::BOLD) } else { palette::ink().add_modifier(Modifier::BOLD | Modifier::UNDERLINED) }
        } else {
            palette::muted()
        };
        let bg = if i == shown && selected { palette::c().sel_bg } else { palette::c().strip_bg };
        spans.push(Span::styled(text, style.bg(bg)));
        hotspots.push((x, tw));
        x += tw;
    }
    let spans = fit_spans(spans, width);
    let used = w(&spans);
    let mut spans = spans;
    spans.push(Span::styled(" ".repeat(width.saturating_sub(used)), Style::new().bg(palette::c().strip_bg)));
    (Line::from(spans), hotspots)
}

/// THE band line, used by every band on every tab: `depth` levels in, the
/// chevron, `name`, and `meta` flush right — nothing between them.
pub fn band_line(depth: usize, open: bool, name: Vec<Span<'static>>, meta: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut left = vec![Span::raw("  ".repeat(depth)), chevron(open)];
    left.extend(name);
    // The name stands; the metadata crops to what is left of the line.
    let meta = fit_spans(meta, width.saturating_sub(w(&left) + 2));
    lr(left, meta, width)
}

/// A band's name in THE band case (audit 0177 V3: one case on every tab):
/// its first letter capital, the rest as written lower — `Unclaimed
/// regions`, `Type one`. A name that is a proper name (`Claude Code`) is
/// passed as it is, not through this.
pub fn sentence(s: &str) -> String {
    let lower = s.replace('_', " ").to_lowercase();
    let mut c = lower.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// A band's name: bold ink.
pub fn name(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), palette::ink().add_modifier(Modifier::BOLD))
}

// ── tables ────────────────────────────────────────────────────────────────

/// Rows of cells laid out as a table: every column as wide as its widest
/// cell (the last column free), two spaces between columns. Rows at one
/// level that are the same kind of thing are rows of a table, so their
/// columns line up whatever their content.
pub fn columns(rows: Vec<Vec<Span<'static>>>) -> Vec<Vec<Span<'static>>> {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; n];
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(display_width(&c.content));
        }
    }
    rows.into_iter()
        .map(|r| {
            let last = r.len().saturating_sub(1);
            r.into_iter()
                .enumerate()
                .map(|(i, c)| {
                    if i == last {
                        c
                    } else {
                        let pad = widths[i].saturating_sub(display_width(&c.content)) + 2;
                        Span::styled(format!("{}{}", c.content, " ".repeat(pad)), c.style)
                    }
                })
                .collect()
        })
        .collect()
}

/// The widths [`columns`] would give, for laying out rows built one at a
/// time (a band header per row): the widest cell of each column.
pub fn widths(cells: &[Vec<String>]) -> Vec<usize> {
    let n = cells.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = vec![0usize; n];
    for r in cells {
        for (i, c) in r.iter().enumerate() {
            out[i] = out[i].max(display_width(c));
        }
    }
    out
}

/// `s` padded (or cropped) to exactly `w` columns.
pub fn cell(s: &str, w: usize) -> String {
    let c = crop(s, w);
    let pad = w.saturating_sub(display_width(&c));
    format!("{c}{}", " ".repeat(pad))
}

/// An opened item's details as a TABLE: a field column (muted) and a value
/// column, split by a muted `│`, `indent` columns in — never a run-on line
/// of facts. A value wider than its column crops.
pub fn table(rows: Vec<(String, Vec<Span<'static>>)>, indent: usize, width: usize) -> Vec<Line<'static>> {
    let kw = rows.iter().map(|(k, _)| display_width(k)).max().unwrap_or(0);
    rows.into_iter()
        .map(|(k, v)| {
            let mut spans = vec![Span::raw(" ".repeat(indent)), Span::styled(cell(&k, kw), palette::muted()), Span::styled(" │ ", palette::muted())];
            spans.extend(v);
            Line::from(fit_spans(spans, width))
        })
        .collect()
}

// ── cards ─────────────────────────────────────────────────────────────────

/// Which card: a node's (rounded) or an artifact's (square, its file named
/// at the top left).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Card {
    Node,
    Artifact,
}

impl Card {
    fn corners(self) -> [&'static str; 4] {
        match self {
            Card::Node => ["╭", "╮", "╰", "╯"],
            Card::Artifact => ["┌", "┐", "└", "┘"],
        }
    }
}

/// The glyph an artifact card's name carries.
pub const ARTIFACT_GLYPH: &str = "▤";

/// What a card's body is laid out to: its width less `│ ` and ` │`.
pub fn inside(width: usize) -> usize {
    width.saturating_sub(4).max(1)
}

/// The colour a node card's border takes from its state: amber while it
/// waits on him (open, blocking, active), the cool colour while it is
/// alive (running, being written), red when it failed or is refused, muted
/// when it is settled.
pub fn status_border(state: &str) -> Style {
    match state {
        "open" | "blocking" | "active" | "draft" | "stale" | "parked" => palette::amber(),
        "running" | "being written" | "ready" => palette::code(),
        "failed" | "refused" | "missing" | "contradicted" => palette::red(),
        _ => palette::muted(),
    }
}

/// A card's top border: `╭─ <left> ──────── <right> ─╮`. The left crops so
/// the right (its id, as a legend) always stands.
pub fn card_top(kind: Card, left: Vec<Span<'static>>, right: Vec<Span<'static>>, style: Style, width: usize) -> Line<'static> {
    let [tl, tr, _, _] = kind.corners();
    let right = fit_spans(right, width.saturating_sub(12).max(1));
    let rw = if right.is_empty() { 0 } else { w(&right) + 2 };
    let room = width.saturating_sub(4 + rw + 2);
    let mut spans = vec![Span::styled(format!("{tl}─"), style)];
    let mut used = 2;
    if !left.is_empty() {
        spans.push(Span::styled(" ", style));
        let left = fit_spans(left, room);
        let lu = w(&left);
        spans.extend(left);
        spans.push(Span::styled(" ", style));
        used += lu + 2;
    }
    let fill = width.saturating_sub(used + rw + 2);
    spans.push(Span::styled("─".repeat(fill), style));
    if !right.is_empty() {
        spans.push(Span::styled(" ", style));
        spans.extend(right);
        spans.push(Span::styled(" ", style));
    }
    spans.push(Span::styled(format!("─{tr}"), style));
    Line::from(spans)
}

/// A card that is also a BAND (an account, a soup node): its top border
/// carries the chevron and the name at the left and the band's metadata as
/// its legend; open, its body is framed by [`card_close`], shut, the card
/// closes at once.
pub fn card_band_top(kind: Card, open: bool, name: Vec<Span<'static>>, meta: Vec<Span<'static>>, style: Style, width: usize) -> Line<'static> {
    let mut left = vec![chevron(open)];
    left.extend(name);
    card_top(kind, left, meta, style, width)
}

/// Frame every line of `lines` from `from` on (laid out to [`inside`]) as
/// the card's body, and close it.
pub fn card_close(kind: Card, lines: &mut Vec<Line<'static>>, from: usize, style: Style, width: usize) {
    let [_, _, bl, br] = kind.corners();
    let iw = inside(width);
    for l in &mut lines[from..] {
        let body = fit_spans(std::mem::take(&mut l.spans), iw);
        let used = w(&body);
        let mut spans = vec![Span::styled("│ ", style)];
        spans.extend(body);
        spans.push(Span::raw(" ".repeat(iw.saturating_sub(used))));
        spans.push(Span::styled(" │", style));
        *l = Line::from(spans);
    }
    lines.push(Line::from(Span::styled(format!("{bl}{}{br}", "─".repeat(width.saturating_sub(2))), style)));
}

/// Shift every line of `lines` from `from` on `n` columns right — a card
/// inside a band at depth sits under the band's name.
pub fn indent(lines: &mut [Line<'static>], from: usize, n: usize) {
    if n == 0 {
        return;
    }
    for l in &mut lines[from..] {
        l.spans.insert(0, Span::raw(" ".repeat(n)));
    }
}

// ── table rows and buttons ────────────────────────────────────────────────

/// The chevron at a configurable row's right end: it opens.
pub const ROW_CHEVRON: &str = "›";

/// A button: its words with a column of its background on each side.
pub fn button(words: &str, selected: bool) -> Span<'static> {
    let style = if selected { Style::new().fg(palette::c().ink).bg(palette::c().button_sel_bg).add_modifier(Modifier::BOLD) } else { Style::new().fg(palette::c().code).bg(palette::c().button_bg) };
    Span::styled(format!(" {words} "), style)
}

/// A button that cannot press yet: its words muted on the button's ground.
pub fn button_dim(words: &str, selected: bool) -> Span<'static> {
    let bg = if selected { palette::c().button_sel_bg } else { palette::c().button_bg };
    Span::styled(format!(" {words} "), Style::new().fg(palette::c().muted).bg(bg))
}

/// The line under a table row: the row's background in its top half and
/// the app's in the bottom, so each row stands apart from the next by a
/// stripe of the app's background — the border in the app's colour.
pub fn row_edge(lead: usize, width: usize, bg: Color) -> Line<'static> {
    Line::from(vec![Span::raw(" ".repeat(lead)), Span::styled("▀".repeat(width.saturating_sub(lead)), Style::new().fg(bg).bg(palette::c().surface))])
}
