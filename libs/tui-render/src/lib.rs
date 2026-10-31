//! **The pure render layer of the terminal face.**
//!
//! `frame(&ViewState, now, width, height) -> Vec<Line>` and nothing else. No
//! clock, no socket, no terminal, no filesystem, no environment.
//!
//! **Three mechanisms enforce that, and each is forced to fail before it is
//! believed** (`./conformance_test.sh`):
//!
//! 1. **The dependency set** (`Cargo.toml`). Rust only lets a crate `use` its
//!    direct dependencies, so `tokio`, `crossterm` and `std::fs` cannot be
//!    named in this crate at all. This is why the render layer is a CRATE and
//!    not a module inside `apps/tui`: a module would have inherited the
//!    shell's whole dependency graph and enforced nothing.
//! 2. **`clippy.toml`'s `disallowed-methods`.** Closes the one door Cargo
//!    cannot: this crate legitimately depends on `chrono` for the TYPE of
//!    `now`, and that same crate hands out `Utc::now()`.
//! 3. **The golden tests** (`tests/golden.rs`). A fixed `ViewState`, a fixed
//!    `now`, exact expected lines. With a real clock anywhere inside the
//!    crate no two runs agree — so this catches a clock that arrived by a
//!    route the lint list never named, and it is the reason the other two are
//!    load-bearing rather than decorative.
//!
//! **The postcondition** every golden case asserts with `assert_shape`:
//! exactly `height` lines of exactly `width` columns. `0018` measured the
//! stakes in this exact stack — ratatui 0.28 PANICS on an over-reported line
//! rather than wrapping it — so narrower always wins on any disagreement.
//!
//! Carried forward from `libs/tui-view` as PATTERNS, never as a dependency:
//! the golden harness with `assert_shape`, the exact-width discipline, the
//! `0005` palette translation, and the one-call-site geometry rule. That crate
//! dies with the retired v2 wire; these four ideas do not.

pub mod compose;
pub mod geom;
pub mod hit;
pub mod keys;
pub mod md;
pub mod model;
pub mod palette;
mod render;
pub mod select;

pub use hit::{activation, HitMap, RowInfo, Target, Toward, Via};
pub use keys::{
    draft_offer_intent, footer_hints, footer_split, intent, overlay_rows, spelling, withheld,
    Binding, Caps, Edit, Intent, Key, FooterHints, KeySet, ModKeys, Mode, Need, Region, BINDINGS,
    BODY_MODES, DOCUMENTED_MODES, DRAFT_OFFER_ACCEPT, DRAFT_OFFER_DISCARD,
};
pub use model::{
    root_lines, AgentTree, Avail, Band, BandId, Banner, Block, BlockId, Composer, DownPage,
    DraftOffer,
    Fetch, Header, Known, Link, LinkState, MicState, Notice, ProjectList, ProjectRow,
    QueueItem, QueueView, RootItem, RootLine, Sender, SheetAct, SheetFacts, SheetField, SheetRow,
    SheetView, StandIn, SubagentClass, SubagentState, ThreadList, ThreadRow, TreeId, TreeRow,
    ViewState, Viewport, BAND_ROWS, COMPOSER_MAX_ROWS, COMPOSER_MIN_ROWS, FOOTER_ROWS,
    HEADER_ROWS, PINNED_EMPTY, PROJECTS_EMPTY, RECENT_EMPTY,
};
pub use palette::Severity;
/// `frame` draws the rows; `bands` says how they divide. **The shell reads
/// `bands` rather than re-deriving a page height from the constants** — see
/// its doc comment for the drift that cost.
///
/// `frame_with_hits` is the real body: it returns the rows AND the
/// [`HitMap`] of what is under every coordinate of them, from ONE pass, and
/// `frame` is its first half. A hit map built by a second pass would be a
/// second derivation of the geometry, and a wrong one does not look wrong.
pub use render::{
    bands, composer_inner, fmt_tokens, frame, frame_with_hits, jump_user, tree_height, Bands,
    GUTTER, TREE_MAX_ROWS,
};
/// **THE DRAG SELECTION.** A range over the face's LOGICAL document rather
/// than over the screen — see [`select`] for the two independent reasons a
/// screen-cell selection is the wrong model here, and for the list of places
/// the logical-text rule can be silently wrong.
pub use select::{selected_text, RowText, Selection, TextPos, Unit, UnitKind};
