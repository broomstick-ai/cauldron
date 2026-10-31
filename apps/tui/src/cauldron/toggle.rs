//! **The toggle** — every boolean setting, and every switch in a table row,
//! is drawn by this one widget (0057: "he asked to SEE a toggle switch").
//!
//! It is a SEGMENTED control (8e4fca10 msg 235: "The whole toggle, all of
//! the options in a toggle should have a different background color to
//! indicate that they're all one contiguous toggle ... each of the items has
//! one space of margin on the left and the right and whichever one is
//! selected has a different background color that has a more like bright or
//! bold ... text color ... just remove those dots"): every option on screen,
//! side by side, on ONE background, the one in force lit.
//!
//! ```text
//!  [ on  off ]               two states: `on` / `off`
//!  [ inherit  on  off ]      a setting that can inherit (msg 235: "we're
//!                            going to have inherit, on, and off")
//!  [ enabled  disabled ]     the cauldron's own words
//! ```
//!
//! Each option is its word with one column of the toggle's background on
//! each side, so two words stand two columns apart. A click on an option
//! picks it ([`Toggle::hotspots`]); Space steps to the next.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use tui_render::palette;

/// Cells the `on` / `off` toggle takes: ` on ` and ` off `.
pub const WIDTH: usize = 9;

/// The words of a tri-state toggle, in order.
pub const TRI: [&str; 3] = ["inherit", "on", "off"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Toggle {
    /// Its options, in order.
    pub options: &'static [&'static str],
    /// The one in force.
    pub selected: usize,
}

const ON_OFF: [&str; 2] = ["on", "off"];

impl Toggle {
    /// `on` / `off`, `on` in force when `on`.
    pub fn new(on: bool) -> Self {
        Toggle { options: &ON_OFF, selected: usize::from(!on) }
    }

    /// Inherit · on · off: `None` inherits.
    pub fn tri(state: Option<bool>) -> Self {
        Toggle { options: &TRI, selected: match state { None => 0, Some(true) => 1, Some(false) => 2 } }
    }

    /// A two-way toggle in other words: the first in force when `first`.
    pub fn pair(words: &'static [&'static str; 2], first: bool) -> Self {
        Toggle { options: words, selected: usize::from(!first) }
    }

    /// The cells it takes.
    pub fn width(self) -> usize {
        self.options.iter().map(|o| o.chars().count() + 2).sum()
    }

    /// Each option's (column, width) from the toggle's left edge: its click
    /// hotspot.
    pub fn hotspots(self) -> Vec<(usize, usize)> {
        let mut x = 0;
        self.options
            .iter()
            .map(|o| {
                let w = o.chars().count() + 2;
                let z = (x, w);
                x += w;
                z
            })
            .collect()
    }

    /// The switch as spans, for a row that carries other spans too.
    pub fn spans(self) -> Vec<Span<'static>> {
        self.options
            .iter()
            .enumerate()
            .map(|(i, o)| {
                if i == self.selected {
                    Span::styled(format!(" {o} "), Style::new().fg(palette::c().ink).bg(palette::c().code_bg).add_modifier(Modifier::BOLD))
                } else {
                    Span::styled(format!(" {o} "), Style::new().fg(palette::c().muted).bg(palette::c().toggle_bg))
                }
            })
            .collect()
    }
}

impl Widget for Toggle {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Line::from(self.spans()).render(area, buf);
    }
}
