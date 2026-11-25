//! **SELECT AND COPY ON EVERY SURFACE** (#34): "Text selection on every
//! surface: modals, error states, lists and pages, not only the thread
//! transcript."
//!
//! The face claims mouse reporting (button and drag events, SGR), so the
//! terminal hands every drag over to the face instead of selecting text itself. So the
//! face selects: over the CELLS OF THE FRAME IT LAST DREW — whatever surface
//! drew them, a modal, an error, a list, a page, the first-run screen — never
//! over any one surface's model. A press remembers its cell; a drag away from
//! it selects from there, row by row, as a terminal does, and paints the
//! selected cells reversed; the release copies their text (each line's
//! trailing spaces trimmed, a box's border trimmed off each end, the margin
//! the rows share taken off, the lines joined with `\n`) through the face's
//! ONE copy path — [`Outbox::Copy`], which the live loop performs with
//! `term_shell::copy_to_clipboard` (OSC 52) and which a test or a `--dump`
//! never performs.
//!
//! **A press acts only as a click** (round 2). A left press is HELD
//! here, not handed on: released on the cell it was pressed on with no drag
//! in between, it is a click, and the press and its release then go on to
//! do exactly what they did (a button, a toggle, a row, an option, a tab, a
//! page item). Dragged to another cell, it is a selection and nothing else:
//! the control it started on is never activated or changed. (The first
//! version handed the press on at once, so a drag that started on a
//! setting's `off` switched the setting off while selecting its text.) A
//! press on a divider's grabber (the sidebar's, the bucket's) is handed on
//! at once, so that divider still drags.
//!
//! **Where it starts, it stays.** A press inside a modal (or the dropdown)
//! selects inside that box only; a press on the cauldron surface selects on
//! its side of the sidebar's divider only — so a page's lines never come
//! with the sidebar's beside them.
//!
//! **The terminal's own selection** is still there: every common terminal
//! bypasses mouse reporting while a modifier is held — Shift in most (xterm,
//! kitty, WezTerm, Alacritty, GNOME), Option in iTerm2, Fn in Terminal.app.

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{Event, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use crate::cauldron::app::{App, Hit, Outbox};

/// A cell of the frame: (column, row).
pub type Pos = (u16, u16);

/// The selection, and the frame it is made over.
#[derive(Debug, Clone, Default)]
pub struct Select {
    /// Where the left press was; `None` when no press is being selected from.
    anchor: Option<Pos>,
    /// Where the drag is (or the release was), clamped into [`Self::clip`].
    head: Option<Pos>,
    /// The box the selection may not leave: the modal, or the side of the
    /// divider, the press was in; else the whole frame.
    clip: Rect,
    /// The release copied: the selection is shown until the next press,
    /// key, paste or scroll, but no longer follows the mouse.
    done: bool,
    /// The last drawn frame, as drawn (before the selection was painted).
    screen: Buffer,
    /// The left press being held until it is known to be a click (released
    /// where it was pressed, no drag) or a selection (dragged).
    pending: Option<MouseEvent>,
}

/// What the face does with an event after the selection has seen it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Then {
    /// The selection took it: nothing else sees it.
    Take,
    /// It goes on as it always has.
    Pass,
    /// A held press turned out to be a click: the press goes on first, then
    /// this event (its release).
    Replay(MouseEvent),
}

impl Select {
    /// Whether cells are selected (a drag moved off its press).
    pub fn active(&self) -> bool {
        self.anchor.is_some() && self.head.is_some()
    }

    fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
        self.done = false;
        self.pending = None;
    }

    fn clamp(&self, (x, y): Pos) -> Pos {
        let c = self.clip;
        (x.clamp(c.x, c.right().saturating_sub(1).max(c.x)), y.clamp(c.y, c.bottom().saturating_sub(1).max(c.y)))
    }

    /// The selected cells, row by row: `(row, first column, last column)`,
    /// both columns inclusive.
    pub fn spans(&self) -> Vec<(u16, u16, u16)> {
        let (Some(a), Some(h)) = (self.anchor, self.head) else { return Vec::new() };
        let ((sx, sy), (ex, ey)) = if (a.1, a.0) <= (h.1, h.0) { (a, h) } else { (h, a) };
        let (left, right) = (self.clip.x, self.clip.right().saturating_sub(1));
        (sy..=ey)
            .map(|y| {
                let x0 = if y == sy { sx } else { left };
                let x1 = if y == ey { ex } else { right };
                (y, x0, x1)
            })
            .filter(|(_, x0, x1)| x0 <= x1)
            .collect()
    }

    /// The selected text, as it is copied: each line's trailing spaces
    /// trimmed, a box border trimmed off its ends, the margin every row after
    /// the first shares taken off, lines joined with `\n` (blank lines at
    /// either end dropped).
    pub fn text(&self) -> String {
        let lines: Vec<String> = self
            .spans()
            .into_iter()
            .map(|(y, x0, x1)| {
                let s: String = (x0..=x1).filter_map(|x| self.screen.cell((x, y)).map(|c| c.symbol().to_string())).collect();
                unborder(&s)
            })
            .collect();
        // The rows after the first start at the box's left edge: the margin
        // they all share is the box's, not the text's.
        let margin = lines.iter().skip(1).filter(|l| !l.is_empty()).map(|l| l.len() - l.trim_start_matches(' ').len()).min().unwrap_or(0);
        let lines: Vec<String> = lines.into_iter().enumerate().map(|(i, l)| if i == 0 || l.is_empty() { l } else { l[margin..].to_string() }).collect();
        let first = lines.iter().position(|l| !l.is_empty()).unwrap_or(lines.len());
        let last = lines.iter().rposition(|l| !l.is_empty()).map_or(first, |i| i + 1);
        lines[first..last.max(first)].join("\n")
    }
}

/// A box-drawing glyph (a modal's or a panel's border, a rule).
fn is_border(c: char) -> bool {
    ('\u{2500}'..='\u{257F}').contains(&c)
}

/// One selected line as copied: trailing spaces and border glyphs off its
/// end; a border at its start (with the padding around it) off its start —
/// indentation that is not next to a border stays.
fn unborder(s: &str) -> String {
    let s = s.trim_end_matches(|c: char| c.is_whitespace() || is_border(c));
    let lead = s.len() - s.trim_start_matches(|c: char| c.is_whitespace() || is_border(c)).len();
    if s[..lead].chars().any(is_border) {
        s[lead..].to_string()
    } else {
        s.to_string()
    }
}

/// The box a press at `p` may select inside: the topmost modal (or
/// dropdown) under it; else its side of the sidebar's divider, on the
/// cauldron surface; else the whole frame.
fn clip_at(app: &App, p: Pos, frame: Rect) -> Rect {
    let inside = |r: &Rect| p.0 >= r.x && p.0 < r.right() && p.1 >= r.y && p.1 < r.bottom();
    if let Some((r, _)) = app.hits.iter().rev().find(|(r, h)| matches!(h, Hit::Modal) && inside(r)) {
        return *r;
    }
    let area = app.clusters_area;
    if let Some((d, _)) = app.hits.iter().find(|(_, h)| matches!(h, Hit::Divider)) {
        if inside(&area) && p.0 != d.x {
            return if p.0 < d.x {
                Rect { x: area.x, width: d.x - area.x, ..area }
            } else {
                Rect { x: d.x + 1, width: area.right().saturating_sub(d.x + 1), ..area }
            };
        }
    }
    frame
}

/// Every event passes here first. [`Then::Take`]: the selection took it (a
/// held press, a drag that selects, the release that copies); nothing else
/// sees it. [`Then::Replay`]: a held press was a click, and goes on now,
/// before its release. Everything else goes on as it always has.
pub fn event(app: &mut App, ev: &Event) -> Then {
    let m: MouseEvent = match ev {
        Event::Mouse(m) => *m,
        Event::Key(_) | Event::Paste(_) => {
            app.select.clear();
            return Then::Pass;
        }
        _ => return Then::Pass,
    };
    let at = (m.column, m.row);
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.select.clear();
            app.select.clip = clip_at(app, at, app.select.screen.area);
            app.select.anchor = Some(at);
            // A divider's grabber drags at once: the press is its.
            if matches!(app.hit_at_pub(m.column, m.row), Some(Hit::Divider | Hit::BucketDivider)) {
                return Then::Pass;
            }
            app.select.pending = Some(m);
            Then::Take
        }
        MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            app.select.clear();
            Then::Pass
        }
        // A divider's (or the bucket's) drag is the divider's.
        MouseEventKind::Drag(MouseButton::Left) if app.dragging || app.dragging_bucket => {
            app.select.clear();
            Then::Pass
        }
        MouseEventKind::Drag(MouseButton::Left) if app.select.anchor.is_some() && !app.select.done => {
            if Some(at) != app.select.anchor || app.select.head.is_some() {
                // Moved off its cell: a selection, never a click.
                app.select.pending = None;
                app.select.head = Some(app.select.clamp(at));
            }
            Then::Take
        }
        MouseEventKind::Up(MouseButton::Left) if app.select.pending.is_some() && Some(at) == app.select.anchor && app.select.head.is_none() => {
            // Released where it was pressed, never dragged: a click.
            let down = app.select.pending.take().expect("checked");
            app.select.clear();
            Then::Replay(down)
        }
        MouseEventKind::Up(MouseButton::Left) if (app.select.active() || app.select.pending.is_some()) && !app.select.done => {
            // Released elsewhere (a terminal may send no motion between):
            // still a selection, never a click.
            app.select.pending = None;
            app.select.head = Some(app.select.clamp(at));
            app.select.done = true;
            app.item_press = None;
            let text = app.select.text();
            if text.is_empty() {
                app.notice = Some("nothing to copy: only blank cells are selected".into());
            } else {
                app.notice = Some(crate::cauldron::select::copy_notice(text.chars().count()));
                app.outbox.push(Outbox::Copy(text));
            }
            Then::Take
        }
        MouseEventKind::Up(_) => {
            if !app.select.done {
                app.select.clear();
            }
            Then::Pass
        }
        _ => Then::Pass,
    }
}

/// The notice a selection's release raises before the copy is performed:
/// what is being sent, not that it landed (the run loop replaces it with
/// term-shell's, which says how to copy otherwise in this terminal).
pub fn copy_notice(chars: usize) -> String {
    format!("sending {chars} characters to the terminal's clipboard (OSC 52)")
}

/// After a frame is drawn: keep it (the text a selection copies) and paint
/// the selection on it.
pub fn draw(app: &mut App, buf: &mut Buffer) {
    app.select.screen = buf.clone();
    for (y, x0, x1) in app.select.spans() {
        for x in x0..=x1 {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.modifier.insert(Modifier::REVERSED);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rows: &[&str]) -> Buffer {
        Buffer::with_lines(rows.iter().copied())
    }

    fn sel(rows: &[&str], a: Pos, h: Pos, clip: Option<Rect>) -> Select {
        let screen = screen(rows);
        Select { anchor: Some(a), head: Some(h), clip: clip.unwrap_or(screen.area), done: true, screen, pending: None }
    }

    #[test]
    fn one_row_is_the_cells_between_press_and_release() {
        let s = sel(&["hello world   "], (6, 0), (10, 0), None);
        assert_eq!(s.text(), "world");
        // Dragged right to left: the same cells.
        let s = sel(&["hello world   "], (10, 0), (6, 0), None);
        assert_eq!(s.text(), "world");
    }

    #[test]
    fn rows_run_like_a_terminal_and_trailing_spaces_go() {
        let s = sel(&["one two   ", "three     ", "four five "], (4, 0), (3, 2), None);
        assert_eq!(s.text(), "two\nthree\nfour");
    }

    #[test]
    fn a_modal_border_is_not_copied() {
        let rows = ["┏━━━━━━━━━━━━━┓", "┃ │ error: x │┃", "┃ │ at y     │┃", "┗━━━━━━━━━━━━━┛"];
        let s = sel(&rows, (0, 0), (14, 3), None);
        assert_eq!(s.text(), "error: x\nat y");
    }

    #[test]
    fn indentation_not_next_to_a_border_stays() {
        assert_eq!(unborder("   indented  "), "   indented");
        assert_eq!(unborder("│  boxed  │"), "boxed");
    }

    #[test]
    fn the_clip_keeps_a_selection_on_its_side() {
        let rows = ["tree │ page one ", "tree │ page two "];
        let clip = Rect { x: 6, y: 0, width: 10, height: 2 };
        let mut s = sel(&rows, (7, 0), (10, 1), Some(clip));
        assert_eq!(s.text(), "page one\npage");
        // A head dragged into the sidebar is held at the clip's edge.
        s.head = Some(s.clamp((0, 1)));
        assert_eq!(s.head, Some((6, 1)));
    }

    #[test]
    fn blank_rows_at_the_ends_are_dropped() {
        let s = sel(&["          ", "  text    ", "    more  ", "          "], (0, 0), (9, 3), None);
        assert_eq!(s.text(), "text\n  more");
    }
}
