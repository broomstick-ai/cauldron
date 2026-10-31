//! The palette, carried over from the `$ threads` board by way of report
//! `0005`, and re-derived here rather than imported from `libs/tui-view` —
//! that crate dies with the v2 wire, and `0023` carries its palette
//! translation forward as a PATTERN, not as a dependency.
//!
//! **What carries over and what deliberately does not.**
//!
//! `SURFACE` / `INK` / `INK2` / `MUTED` are plain chrome and transfer verbatim.
//! `GREEN` transfers verbatim *with its rule attached*: it is for MOMENTARY
//! CONFIRMATIONS ONLY and never for a steady state. `connected` is a steady
//! state and is therefore **not** green; it is [`Colors::ink2`], calm and legible.
//!
//! `BLUE` and `ATLIMIT` are **absent on purpose.** On the threads board they
//! are two bands of a magnitude meter defined against a percentage of a quota.
//! Nothing on the threads wire this face speaks is a bounded percentage: a
//! queue has a depth but no ceiling, a ledger has a seq but no maximum, a
//! reconnect has an attempt number that only grows. Importing those two
//! colours would mean inventing the meter that justifies them. The named tell
//! for that mistake is catching yourself asking "what is 100% of a queue" —
//! there is no answer, so there is no meter, so there are no bands.
//!
//! Two standing rules the rest of this crate is written against (0005
//! `honesty_rules`, restated in `0023` `philosophy` §4):
//!
//! 1. **Staleness is never red.** Data age tops out at [`Colors::amber`]. [`Colors::red`] is
//!    reserved for an actual failure or a refusal. A stale transcript and a
//!    dead daemon must not read as the same thing.
//! 2. **Green is never steady.** See above.

use std::cell::Cell;

use ratatui::style::{Color, Modifier, Style};

/// Which palette the face draws in: Config › TUI › `theme` (8e4fca10 msg 235:
/// "the display stuff, like light mode versus dark mode").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

impl Theme {
    /// The setting's word for it, and back.
    pub fn word(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }

    pub fn from_word(w: &str) -> Option<Theme> {
        match w {
            "dark" => Some(Theme::Dark),
            "light" => Some(Theme::Light),
            _ => None,
        }
    }

    /// Every colour role in this theme.
    pub fn colors(self) -> &'static Colors {
        match self {
            Theme::Dark => &DARK,
            Theme::Light => &LIGHT,
        }
    }
}

thread_local! {
    /// The theme of the frame being drawn on this thread. A frame is drawn on
    /// one thread start to finish, and the face sets this at the top of every
    /// frame from its setting, so a switch is live on the next frame; being
    /// per thread, two faces (or two tests) drawing at once never see each
    /// other's theme.
    static THEME: Cell<Theme> = const { Cell::new(Theme::Dark) };
}

/// Draw from now on (on this thread) in `t`.
pub fn set_theme(t: Theme) {
    THEME.with(|c| c.set(t));
}

/// The theme frames on this thread are drawn in.
pub fn theme() -> Theme {
    THEME.with(Cell::get)
}

/// Every colour role of the theme in force — the ONLY way a colour is named
/// anywhere in the face, so nothing can stay dark when the theme is light.
pub fn c() -> &'static Colors {
    theme().colors()
}

/// Every colour role the face uses. Each role is one meaning; a theme gives
/// each a value. The notes below were measured on [`DARK`]; [`LIGHT`]'s
/// numbers are checked by `tests/golden.rs` (`light_palette_*`): every text
/// role clears WCAG AA (4.5:1) on every background role, and every background
/// stays distinguishable from the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colors {
    /// App/screen background chrome. `#1a1a19`.
    pub surface: Color,
    /// Primary text and selected-state emphasis. `#ffffff`.
    pub ink: Color,
    /// Secondary, de-emphasised-but-legible text. `#c3c2b7`.
    pub ink2: Color,
    /// Labels, dividers, rails, "nothing to report" text. `#898781`.
    pub muted: Color,
    /// Warning, staleness, reconnecting/hazard frames. `#fab219`.
    pub amber: Color,
    /// Critical: dead, expired, stranded, refused. `#d03b3b`.
    pub red: Color,
    /// Momentary confirmations ONLY — never a steady-state colour. `#0ca30c`.
    pub green: Color,
    /// **THE PAPER BOX — AND IT IS NO LONGER WHITE.** `#d0d0d0`, a BACKGROUND
    /// rather than a text colour, and still the only background in this file
    /// besides the two inline-code chips below.
    ///
    /// His words, and the reference implementation is named in the first of them:
    /// *"You don't need to have the quote line on the left of every message. It's
    /// distracting. Claude code solves this by making our messages right-aligned
    /// with a white background and black text. Let's emulate that."* Then, once he
    /// had lived with it: *"The white background for the user input is too bright.
    /// Maybe a grayscale would be better."*
    ///
    /// **WHY THIS EXACT GREY, IN NUMBERS.** It is neutral (`r == g == b`), which is
    /// the literal reading of "a grayscale" against a palette that is otherwise
    /// warm. Its relative luminance is 0.631 against white's 1.000 — a 37% drop,
    /// which is the glare he is complaining about, gone. The floor is set by
    /// [`Colors::on_paper_muted`], not by taste: `#d0d0d0` is the DARKEST neutral grey at
    /// which that colour still clears WCAG AA (4.55:1; `#cecece` is 4.46 and
    /// fails), and it is chosen there so the change is as large as it can be
    /// without moving any ink. Against [`Colors::surface`] it still reads as paper —
    /// 11.29:1, down from white's 17.4:1.
    ///
    /// It is not only HIS box. The same treatment renders **an agent's input to a
    /// subagent** — *"The white box that shows user input, it would also show the
    /// inputs from an agent to a subagent"* — which is why the thing it keys on is
    /// [`crate::model::Sender::User`] and not "the operator", and why the grey was
    /// checked against a wall of metadata rows as well as against one sentence.
    pub paper: Color,
    /// The ink ON [`Colors::paper`]. Black, because he said black — and because [`Colors::surface`]
    /// on paper is a dark grey that reads as a rendering bug beside the real thing.
    ///
    /// **RE-DERIVED FOR THE NEW GREY RATHER THAN LEFT TO FEND FOR ITSELF**: black
    /// on `#d0d0d0` is **13.62:1**, down from 21.00:1 on white and still nearly
    /// double the 7:1 AAA bar. The one alternative the palette offers — [`Colors::surface`]
    /// as ink, 11.29:1 — is barred by the sentence above, so black stays and the
    /// ratio is the reason rather than the inertia.
    pub on_paper: Color,
    /// The metadata printed on [`Colors::paper`]: legible, clearly subordinate to the
    /// words, and NOT [`Colors::muted`] — `#898781` is the one combination in this palette
    /// that fails to be readable at all on a light background.
    ///
    /// **UNCHANGED BY THE GREY, AND MEASURED SO.** `#5a5955` on `#d0d0d0` is
    /// **4.55:1** (7.01:1 on white) — past WCAG AA for body text, and clearly
    /// subordinate to the ink's 13.62:1, which is the whole job of this colour.
    /// Its reason for existing survives too: [`Colors::muted`] on the new grey is 2.33:1,
    /// still unreadable, so this is still the colour the metadata has to use.
    pub on_paper_muted: Color,
    /// **INLINE `code`, ON THE CHROME.** The foreground of a backtick span in the
    /// assistant's lane.
    ///
    /// > *`code` inline could be rendered as markdown. Just a different style.
    /// > claude-code uses text color but I think a background color change would be
    /// > nice too.*
    ///
    /// **BOTH, because he asked for both** — this is the text colour and
    /// [`Colors::code_bg`] is the background, and "would be nice too" is an addition to the
    /// reference behaviour rather than an alternative to it.
    ///
    /// **WHY A COOL HUE IS THE ONE HUE THAT IS FREE.** Every other hue in this
    /// palette is load-bearing: [`Colors::amber`] means staleness, [`Colors::red`] means failure or
    /// refusal, [`Colors::green`] means a momentary confirmation. A code span is none of
    /// those and must never read as one, so it cannot borrow from the severity
    /// grammar — and the grammar is entirely warm plus green, which leaves cool
    /// unclaimed. It is 6.75:1 on [`Colors::code_bg`], and 1.75:1 against [`Colors::ink`], so the
    /// ink really has changed rather than merely dimmed.
    pub code: Color,
    /// The chip BEHIND an inline `code` span on the chrome: [`Colors::surface`] lifted,
    /// cool to match [`Colors::code`]. 1.47:1 against [`Colors::surface`] — a panel you can see the
    /// edge of without it becoming a block of colour in the middle of a sentence.
    pub code_bg: Color,
    /// **INLINE `code`, INSIDE THE PAPER BOX.** The same span, the other lane.
    ///
    /// One pair cannot serve both lanes well: the chip has to be distinguishable
    /// from [`Colors::surface`] *and* from [`Colors::paper`], and the only backgrounds that manage
    /// both are mid-greys that read as a redaction bar inside a light box. So the
    /// code style is chosen per lane, exactly as the prose style already is — see
    /// `render::lay_out`, where `base` is picked by sender and this is picked
    /// beside it. 6.12:1 on [`Colors::code_on_paper_bg`], and 8.72:1 on bare [`Colors::paper`], so
    /// a row that somehow lost its chip is still legible.
    pub code_on_paper: Color,
    /// The chip behind an inline `code` span inside the paper box: [`Colors::paper`]
    /// darkened and cooled. 1.42:1 against [`Colors::paper`] — the same visible-edge
    /// judgment as [`Colors::code_bg`]'s 1.47:1, from the other side.
    pub code_on_paper_bg: Color,
    /// The selected rows' background while their pane holds the keys: the
    /// cluster, lifted a little and kept warm, so it reads as "here" and never
    /// as a tag chip ([`Colors::code_bg`], which is cool).
    pub sel_bg: Color,
    /// The selected rows' background while another pane holds the keys.
    pub sel_bg_dim: Color,
    /// A modal's background: cool, unlike any cluster, so a modal can never be
    /// taken for another box on the page.
    pub modal_bg: Color,
    /// The box inside a modal.
    pub modal_inner: Color,
    /// The background of a TAB STRIP — a cluster's tabs, Status's, Config's,
    /// Tasks' (8e4fca10 msg 235).
    pub strip_bg: Color,
    /// A table ROW's background: the row stands on this, and what parts two
    /// rows is the app's own background (8e4fca10 msg 235).
    pub row_bg: Color,
    /// A BUTTON's background: the button alone, never its row.
    pub button_bg: Color,
    /// A button's background while its row is selected.
    pub button_sel_bg: Color,
    /// A toggle's own background: one contiguous control. The option in force
    /// sits on [`Colors::code_bg`].
    pub toggle_bg: Color,
    /// The chevron beside the cauldron's name, a dropdown's further background.
    pub chevron_bg: Color,
}

impl Colors {
    /// Every role, named: what a test sweeps, so a role added to the struct
    /// is checked the moment it is listed here (`tests/golden.rs` holds its length to the
    /// struct's size).
    pub fn roles(&self) -> [(&'static str, Color); 24] {
        [
            ("surface", self.surface),
            ("ink", self.ink),
            ("ink2", self.ink2),
            ("muted", self.muted),
            ("amber", self.amber),
            ("red", self.red),
            ("green", self.green),
            ("paper", self.paper),
            ("on_paper", self.on_paper),
            ("on_paper_muted", self.on_paper_muted),
            ("code", self.code),
            ("code_bg", self.code_bg),
            ("code_on_paper", self.code_on_paper),
            ("code_on_paper_bg", self.code_on_paper_bg),
            ("sel_bg", self.sel_bg),
            ("sel_bg_dim", self.sel_bg_dim),
            ("modal_bg", self.modal_bg),
            ("modal_inner", self.modal_inner),
            ("strip_bg", self.strip_bg),
            ("row_bg", self.row_bg),
            ("button_bg", self.button_bg),
            ("button_sel_bg", self.button_sel_bg),
            ("toggle_bg", self.toggle_bg),
            ("chevron_bg", self.chevron_bg),
        ]
    }
}

/// The face's own text roles — every colour drawn AS INK on the chrome — and
/// its own background roles, which any of them can land on. The paper box's
/// inks are checked on the paper alone.
pub const TEXT_ROLES: &[&str] = &["ink", "ink2", "muted", "amber", "red", "green", "code"];
pub const BG_ROLES: &[&str] = &["surface", "sel_bg", "sel_bg_dim", "modal_bg", "strip_bg", "row_bg", "button_bg", "button_sel_bg", "toggle_bg", "code_bg", "chevron_bg"];

/// The palette the face was built in.
pub const DARK: Colors = Colors {
    surface: Color::Rgb(0x1a, 0x1a, 0x19),
    ink: Color::Rgb(0xff, 0xff, 0xff),
    ink2: Color::Rgb(0xc3, 0xc2, 0xb7),
    muted: Color::Rgb(0x89, 0x87, 0x81),
    amber: Color::Rgb(0xfa, 0xb2, 0x19),
    red: Color::Rgb(0xd0, 0x3b, 0x3b),
    green: Color::Rgb(0x0c, 0xa3, 0x0c),
    paper: Color::Rgb(0xd0, 0xd0, 0xd0),
    on_paper: Color::Rgb(0x00, 0x00, 0x00),
    on_paper_muted: Color::Rgb(0x5a, 0x59, 0x55),
    code: Color::Rgb(0x9e, 0xc9, 0xe8),
    code_bg: Color::Rgb(0x34, 0x38, 0x3b),
    code_on_paper: Color::Rgb(0x16, 0x32, 0x3f),
    code_on_paper_bg: Color::Rgb(0xa8, 0xb0, 0xb8),
    sel_bg: Color::Rgb(0x34, 0x33, 0x2e),
    sel_bg_dim: Color::Rgb(0x26, 0x26, 0x24),
    modal_bg: Color::Rgb(0x1d, 0x26, 0x2e),
    modal_inner: Color::Rgb(0x4f, 0x63, 0x73),
    strip_bg: Color::Rgb(0x23, 0x2a, 0x30),
    row_bg: Color::Rgb(0x27, 0x29, 0x2b),
    button_bg: Color::Rgb(0x2b, 0x35, 0x3d),
    button_sel_bg: Color::Rgb(0x3f, 0x50, 0x5c),
    toggle_bg: Color::Rgb(0x2e, 0x2d, 0x29),
    chevron_bg: Color::Rgb(0x4a, 0x50, 0x55),
};

/// THE LIGHT PALETTE: the same roles on a warm off-white, each ink darkened
/// until it clears 4.5:1 on every background it can land on (the darkest of
/// them, the selected button and the chevron, set the floor), the paper box
/// a grey a step DARKER than the surface rather than lighter, and the
/// severity hues kept recognisably the same amber, red and green.
pub const LIGHT: Colors = Colors {
    surface: Color::Rgb(0xf7, 0xf6, 0xf2),
    ink: Color::Rgb(0x11, 0x11, 0x10),
    ink2: Color::Rgb(0x3b, 0x3a, 0x36),
    muted: Color::Rgb(0x5c, 0x5a, 0x55),
    amber: Color::Rgb(0x7a, 0x4f, 0x00),
    red: Color::Rgb(0xa8, 0x22, 0x1b),
    green: Color::Rgb(0x14, 0x66, 0x14),
    paper: Color::Rgb(0xd6, 0xd6, 0xd6),
    on_paper: Color::Rgb(0x00, 0x00, 0x00),
    on_paper_muted: Color::Rgb(0x4f, 0x4e, 0x4a),
    code: Color::Rgb(0x1d, 0x53, 0x73),
    code_bg: Color::Rgb(0xdd, 0xe6, 0xed),
    code_on_paper: Color::Rgb(0x13, 0x30, 0x40),
    code_on_paper_bg: Color::Rgb(0xbc, 0xc6, 0xcf),
    sel_bg: Color::Rgb(0xe5, 0xe1, 0xd6),
    sel_bg_dim: Color::Rgb(0xeb, 0xe8, 0xdf),
    modal_bg: Color::Rgb(0xe3, 0xeb, 0xf1),
    modal_inner: Color::Rgb(0x7f, 0x95, 0xa6),
    strip_bg: Color::Rgb(0xe2, 0xe8, 0xed),
    row_bg: Color::Rgb(0xe8, 0xe9, 0xeb),
    button_bg: Color::Rgb(0xd6, 0xe1, 0xea),
    button_sel_bg: Color::Rgb(0xc8, 0xd6, 0xe1),
    toggle_bg: Color::Rgb(0xe7, 0xe4, 0xdc),
    chevron_bg: Color::Rgb(0xce, 0xd3, 0xd7),
};

/// Base style for every line this crate emits: the chrome background, so a
/// line that is padded to width paints the surface rather than whatever the
/// terminal happened to have there.
pub fn base() -> Style {
    Style::new().bg(c().surface).fg(c().ink2)
}

pub fn ink() -> Style {
    base().fg(c().ink)
}

pub fn ink2() -> Style {
    base().fg(c().ink2)
}

pub fn muted() -> Style {
    base().fg(c().muted)
}

pub fn amber() -> Style {
    base().fg(c().amber)
}

pub fn red() -> Style {
    base().fg(c().red)
}

pub fn green() -> Style {
    base().fg(c().green)
}

/// **THE PAPER BOX's body style** — black on grey. Every row of a
/// [`crate::model::Sender::User`] box is padded to the box width in THIS
/// style, which is what makes the box a rectangle rather than a ragged stripe
/// of highlight behind the words.
pub fn paper() -> Style {
    Style::new().bg(c().paper).fg(c().on_paper)
}

/// **THE INLINE-CODE STYLE ON THE CHROME** — a real foreground AND a real
/// background, which is what he asked for. It is applied as a DELTA on the
/// prose style (`base.patch(code)`), so it overrides both and a code span in a
/// bold run keeps its bold.
pub fn code() -> Style {
    Style::new().bg(c().code_bg).fg(c().code)
}

/// The same span inside the paper box. See [`Colors::code_on_paper`] for why there are
/// two pairs and not one.
pub fn code_on_paper() -> Style {
    Style::new().bg(c().code_on_paper_bg).fg(c().code_on_paper)
}

/// Metadata inside the paper box — the bare message id, and the key/value rows
/// the expander opens.
pub fn paper_muted() -> Style {
    Style::new().bg(c().paper).fg(c().on_paper_muted)
}

/// The header band: inked, bold, so the one always-present row of standing
/// facts reads as chrome rather than as content.
pub fn header() -> Style {
    base().fg(c().ink).add_modifier(Modifier::BOLD)
}

/// The severity grammar that DOES carry over from the board's colour scheme.
///
/// Four steps, not three, because "unknown" is its own thing and rendering it
/// as calm is honesty rule 1's exact violation (unknown never renders as zero,
/// and it never renders as fine either).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Severity {
    /// Steady, nothing to say.
    #[default]
    Calm,
    /// We do not know. Never calm, never red.
    Unknown,
    /// Stale, degraded, retrying. Amber tops out here.
    Warn,
    /// An actual failure or a refusal.
    Critical,
    /// A MOMENTARY confirmation. Never a steady state.
    Ok,
}

impl Severity {
    pub fn style(self) -> Style {
        match self {
            Severity::Calm => ink2(),
            Severity::Unknown => muted(),
            Severity::Warn => amber(),
            Severity::Critical => red(),
            Severity::Ok => green(),
        }
    }
}
