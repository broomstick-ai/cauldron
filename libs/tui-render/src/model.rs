//! The render layer's INPUT. Everything [`crate::frame`] is allowed to know.
//!
//! This type is the purity boundary made concrete: the shell projects daemon
//! truth into a `ViewState` and hands it over with a `now`, a width and a
//! height. There is no second channel. If a fact is not in here it cannot
//! reach the screen, which is what makes the golden tests able to assert
//! anything at all.
//!
//! **What is NOT in here, on purpose** (`0023` `what_the_tui_stores`): no
//! socket, no cursor into the ledger, no queue the client counted itself, no
//! unread bookkeeping. Those are the daemon's and they arrive as values on a
//! frame, already decided.

use chrono::{DateTime, TimeDelta, Utc};

pub use crate::keys::Mode;
pub use crate::palette::Severity;

/// The FIVE BANDS (`0023` `layout`), named once so the layout arithmetic and
/// the prose cannot drift apart.
///
/// Four of them are rectangles. The fifth — [`Band::EventLane`] — is a row
/// CLASS inside [`Band::Transcript`] rather than a pane of its own, and that
/// is a decision with an argument: seq order IS the story of a turn, so a tool
/// call between two assistant texts has to render between them. A side pane
/// would re-create the master-detail split `0003` rejected and force a second
/// scroll state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// 1 row: thread · connection · account/model · queue badge · unread hint.
    Header,
    /// Min: the chat, newest at the BOTTOM.
    Transcript,
    /// Interleaved inside [`Band::Transcript`], in ledger order.
    EventLane,
    /// 3 rows, growing to 8. **Thread view only** — there is nothing to compose
    /// to from the thread list, and a band that cannot mean anything is a band
    /// that should not take rows from the one that can.
    Composer,
    /// 1 row: the top-ranked key hints, the mouse owner, and the toast slot.
    Footer,
}

pub const HEADER_ROWS: u16 = 1;
pub const FOOTER_ROWS: u16 = 1;
pub const COMPOSER_MIN_ROWS: u16 = 3;
pub const COMPOSER_MAX_ROWS: u16 = 8;

// **`MouseOwner` IS GONE, with the `M` key that needed it.**
//
// It existed to render which of the face and the terminal was being sent
// mouse reports, because `M` handed the mouse back so the TERMINAL could
// drag-select. `0036` measured that trade twice over and it was dead both
// ways: under his tmux the terminal's own selection had already been
// suppressed by `mouse on` and there was nothing to hand back, and the face
// now selects for itself on every cell it draws. A toggle with nothing on the
// other side of it is not a door, and an indicator for a toggle that does not
// exist is one more thing on the footer that cannot be acted on.

/// The state of the link to the threads daemon.
///
/// **`Absent` is GONE, and its going was the seam behaving.** Build step 1 had
/// no transport at all, so it carried a variant that said exactly that rather
/// than rendering a `Disconnected` it had never earned. Step 2 deleted it, the
/// `match` in `render.rs` went non-exhaustive, and the compiler walked to every
/// place the link reaches the screen. There is now always an attempt: the
/// transport connects the moment the face starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    /// An attempt is in flight.
    Connecting { attempt: u32 },
    /// Subscribed, receiving frames.
    Connected,
    /// It was up and is not.
    ///
    /// **`next_retry_in_ms` IS GONE AND ITS GOING IS THE POINT.** It was a
    /// DURATION captured at the instant of the drop — a snapshot — and the
    /// banner printed it as a live promise, so thirty seconds after the drop
    /// the face was still saying "retry in 4s". That is the same class of
    /// defect as a stale "2m ago", which `DEC-13` exists to prevent. A
    /// DEADLINE cannot go stale the way a duration can: the renderer is handed
    /// a `now` on every frame, so `retry_at - now` is recomputed from scratch
    /// each paint and is true at the moment it is painted.
    ///
    /// `None` means we are not retrying, or were not told when — and saying
    /// nothing is the honest version of not knowing.
    Disconnected {
        reason: String,
        retry_at: Option<DateTime<Utc>>,
    },
}

impl LinkState {
    /// The severity grammar for the link line. Note what is NOT here: nothing
    /// maps to [`Severity::Ok`], because connected is a STEADY state and green
    /// is never steady (`palette`).
    pub fn severity(&self) -> Severity {
        match self {
            LinkState::Connecting { .. } => Severity::Warn,
            LinkState::Connected => Severity::Calm,
            LinkState::Disconnected { .. } => Severity::Critical,
        }
    }
}

/// A value the daemon owns that we may not have yet.
///
/// Honesty rule 1 made into a type: there is no way to spell "we have not been
/// told the queue depth" as `0`, because [`Unknown`](Known::Unknown) is a
/// different variant and the renderer paints it `?` in [`Severity::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Known<T> {
    #[default]
    Unknown,
    Known(T),
}

impl<T: std::fmt::Display> Known<T> {
    pub fn label(&self) -> String {
        match self {
            Known::Unknown => "?".into(),
            Known::Known(v) => v.to_string(),
        }
    }
}

/// The header band's content.
#[derive(Debug, Clone, Default)]
pub struct Header {
    /// **THE BREADCRUMB TRAIL — the back stack, rendered, outermost first.**
    ///
    /// Each crumb NAMES A SCREEN `←` WILL LAND ON, in the order it will land on
    /// them; the leaf is [`Header::title`] and is not repeated here. The ROOT
    /// IS THE FLOOR AND IS NOT WRITTEN — every trail ends there, and seven
    /// columns of `menu ›` on every screen say nothing.
    ///
    /// **IT IS DERIVED AND NEVER STORED.** The shell computes it at projection
    /// time from `App::origin` plus the view it is projecting; nothing on a
    /// thread, a row or the store carries a crumb. That is not tidiness: 46 of
    /// 207 thread ids live in two to four project indexes (`0109`), so "where
    /// does this thread live" has no answer, while "where does `←` land" has
    /// exactly one — per VISIT. The same thread reached through the recent page
    /// and through its project therefore shows two different trails, and both
    /// are right.
    pub trail: Vec<String>,
    /// What this view is looking at, already worded by the shell — the open
    /// thread, or the fleet WINDOW. It is a sentence and not a count; see
    /// [`ThreadList::window`] for why the fleet no longer has a size here.
    pub title: String,
    /// The open thread's cwd, shortened by the SHELL — this crate has no
    /// filesystem and no `$HOME` to shorten against. **Rendered verbatim**: two
    /// of his directories differ by one syllable (`mythcellaneous`), and a face
    /// that tidied a path would be the thing that made them look alike.
    pub cwd: String,
    pub link: Link,
    /// Account and model as the daemon reports them.
    pub account: Known<String>,
    /// Queued messages. Daemon-derived, never counted here.
    pub queued: Known<u32>,
    /// What [`Header::queued`] is the queue OF — "this thread", "500 shown".
    /// A badge whose scope is a guess is a badge that reads as a fleet total
    /// when it is a window's sum.
    pub queue_scope: String,
    /// Unread on OTHER threads.
    pub unread_elsewhere: Known<u32>,
    /// **THE DAEMON'S OWN WARNING COUNT SINCE ITS BOOT** — `warn + error`.
    ///
    /// Daemon-derived, never counted here, and deliberately a snapshot rather
    /// than a live number: this face is on the request/response door and learns
    /// it from `threads/status`, which it reads on connect and on every fleet
    /// repaint. `Known<u32>` is therefore load-bearing twice over — `Unknown`
    /// covers both *"this daemon is too old to report its warnings"* and *"we
    /// have not read the fleet yet"*, and neither may be painted as zero.
    ///
    /// The operator's ask was *"display the number of warnings coming from
    /// threads so it annoys me? 27k is incredible"*, and the 27,705 were
    /// invisible because their only output was a log file.
    pub warnings: Known<u32>,
    /// How many of [`Header::warnings`] were `ERROR` rather than `WARN`.
    ///
    /// Kept apart because the producer keeps them apart (`warnings::Tally`): one
    /// number cannot tell 47 warnings from 47 errors, and the second is strictly
    /// worse. It only changes the colour and a parenthesis, which is as much as
    /// a one-row header can afford.
    pub warning_errors: Known<u32>,
    /// **WHAT THE OPEN THREAD IS DOING, AS ONE PHRASE** — `waiting on 2
    /// subagents · 1 parked`, `resuming · 2 reports in`, `↑ subagent of …`.
    /// Derived by the shell from cells it already holds; `None` draws nothing.
    /// It exists so a parent whose turn ended while its children run never
    /// reads as idle.
    pub activity: Option<(String, Severity)>,
    /// **THIS HEADER IS A THREAD PAGE'S** (TUI-P1a, inspect F-2) — set by the
    /// shell from the surface UNDER any overlay (`App::view`), because the
    /// rename box, the queue, help and the sheet all stand over a thread with
    /// `ViewState::mode` naming the overlay. A thread page's header is its name
    /// and its status and nothing else in every one of those states.
    pub thread_page: bool,
}

#[derive(Debug, Clone)]
pub struct Link {
    /// The endpoint the shell resolved. Shown so "which daemon" is never a
    /// guess.
    pub url: String,
    pub state: LinkState,
    /// **WHEN THE LINK WENT DOWN, and `None` while it is up.**
    ///
    /// It lives on `Link` and NOT inside [`LinkState::Disconnected`] because
    /// the reconnect ladder alternates between the two down states: the
    /// transport emits `Disconnected` and then `Connecting { attempt: 2 }` and
    /// then `Disconnected` again, once per attempt. Held inside the
    /// `Disconnected` variant, the instant would be destroyed and re-made on
    /// every hop — so the face would say "down 4s" forever, however long the
    /// daemon had really been gone, which is the exact lie this field exists
    /// to kill. Held here it spans the whole outage, and **how long it has
    /// been down is the fact that separates a blip from a dead daemon.**
    pub down_since: Option<DateTime<Utc>>,
}

impl Default for Link {
    fn default() -> Self {
        Self {
            url: String::new(),
            state: LinkState::Connecting { attempt: 1 },
            down_since: None,
        }
    }
}

impl Link {
    /// How long the link has been down, as of the frame's `now`.
    ///
    /// Monotonic by construction, and that is why it is the number the face
    /// leads with: it is a statement about the PAST, so a frame that is a
    /// moment behind the clock under-reports it rather than promising
    /// something that has already failed to happen.
    pub fn down_for(&self, now: DateTime<Utc>) -> Option<TimeDelta> {
        self.down_since.map(|t| (now - t).max(TimeDelta::zero()))
    }

    /// When the next attempt is due, if the transport told us.
    pub fn retry_at(&self) -> Option<DateTime<Utc>> {
        match &self.state {
            LinkState::Disconnected { retry_at, .. } => *retry_at,
            _ => None,
        }
    }

    /// **HOW LONG UNTIL ANYTHING THE LINK LINE SAYS WOULD CHANGE** — `None`
    /// when nothing it says is time-varying.
    ///
    /// The renderer owns what it draws, so it owns the question of when that
    /// drawing goes stale; the shell reads this and arms a timer with it, the
    /// same shape the expiring notice already uses. Without it the two live
    /// numbers would only be recomputed when some UNRELATED event happened to
    /// repaint, and a face sitting still through an outage would show the
    /// second it was disconnected on until the daemon came back.
    ///
    /// It is deliberately COARSE — it fires when the rendered STRING would
    /// change, not on a fixed cadence — so a link that has been down for two
    /// hours costs one wakeup a minute rather than sixty.
    pub fn refresh_in(&self, now: DateTime<Utc>) -> Option<TimeDelta> {
        let mut next: Option<TimeDelta> = None;
        let mut take = |d: TimeDelta| {
            let d = d.max(TimeDelta::zero());
            next = Some(match next {
                Some(n) if n <= d => n,
                _ => d,
            });
        };
        if let Some(t) = self.down_since {
            let age = (now - t).max(TimeDelta::zero());
            // Mirrors `geom::fmt_age`'s own ladder: seconds under a minute,
            // then minutes (through the `1h 02m` form), then hours.
            let step = if age < TimeDelta::minutes(1) {
                TimeDelta::seconds(1)
            } else if age < TimeDelta::days(1) {
                TimeDelta::minutes(1)
            } else {
                TimeDelta::hours(1)
            };
            let steps = age.num_milliseconds() / step.num_milliseconds();
            take(t + step * ((steps + 1) as i32) - now);
        }
        if let Some(r) = self.retry_at() {
            if r > now {
                // The countdown is drawn in whole seconds, so the next thing
                // it could say is one second-boundary away — and never later
                // than the deadline itself.
                let left = r - now;
                let rem = TimeDelta::milliseconds(left.num_milliseconds() % 1000);
                take(if rem.is_zero() {
                    TimeDelta::seconds(1)
                } else {
                    rem
                });
            }
        }
        next
    }
}

/// Identity of one visual block.
///
/// **This is the cursor SEAM** (`0023` `cursor_verdict`). v1 has no
/// per-message cursor: scrolling moves the viewport, not a selection. Keeping
/// identity addressable per block is what lets copy-a-message and — if audio
/// mode ever lands — a playhead ride the same concept, with no change to the
/// viewport model.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlockId {
    pub thread_id: String,
    pub message_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    /// Left-aligned boxes.
    Assistant,
    /// Right-aligned boxes.
    User,
}

/// One thing in the transcript, in ledger order.
#[derive(Debug, Clone)]
pub enum Block {
    /// A speech act. Boxed, sided by [`Sender`].
    Message {
        id: BlockId,
        sender: Sender,
        /// `None` when the daemon gave us no timestamp we could read. It
        /// renders `?`, never the moment the frame ARRIVED — passing an
        /// arrival time off as an event time is honesty rule 1 wearing a
        /// plausible number.
        ts: Option<DateTime<Utc>>,
        text: String,
        /// **A WORD ON THE RUN HEADER ABOUT THE STATE OF THIS BOX**, and there
        /// are exactly two: `writing`, for a message still assembling from
        /// stream deltas, and `interrupted mid-reply`, for one whose turn ended
        /// before it finished.
        ///
        /// A WORD and never a glyph, and never a spinner: the operator's glyph
        /// ruling, and a spinner would be a clock this crate does not have.
        /// `None` is the ordinary case — a finished message says nothing about
        /// itself, because being finished is what a message normally is.
        note: Option<String>,
        /// **THE METADATA THAT ATTACHES TO A USER INPUT**, as ordered
        /// key/value pairs, flattened from the two sources he named:
        ///
        /// > *There's also some metadata we attach to user inputs.*
        /// > *- message id*
        /// > *- threads system metadata*
        /// > *- app specific metadata.*
        /// > *we need a way to toggle viewing this.*
        ///
        /// **The message id is NOT in here** — it is [`BlockId::message_id`],
        /// it is drawn at the top right of the box by default, and it is the
        /// one piece of this that is never behind the toggle. What is in here
        /// is everything else, and it is drawn only when the block is `open`.
        ///
        /// ORDERED PAIRS rather than a map, so the daemon's `intent` leads and
        /// the app's own stamp follows, the same way every time. A map would
        /// have made the row order an artefact of a hash seed.
        ///
        /// Empty on an assistant box: the wire attaches none, and inventing a
        /// row to fill the space would be honesty rule 1.
        meta: Vec<(String, String)>,
        /// See [`Block::Lane::key`].
        key: Option<RowKey>,
        /// See [`Block::Lane::expandable`].
        expandable: bool,
        /// See [`Block::Lane::open`].
        open: bool,
    },
    /// An annotation BETWEEN speech acts: the third lane. Left-aligned,
    /// distinct muted style, one line, glyph-prefixed — not centred and not
    /// boxed (`0023` `layout`, THIRD LANE VERDICT; `DEC-11`/`DEC-12`).
    Lane {
        /// As [`Block::Message::ts`]. Kept on the row for a future day-boundary
        /// rule; the lane itself is ordered by `seq`, which is the ledger's own
        /// order and the only one that is always right.
        ts: Option<DateTime<Utc>>,
        glyph: char,
        severity: Severity,
        text: String,
        /// `Some(n)` renders a `×n` count stamp: display coalescing of a live
        /// row, never a drop. Nothing it stands for is discarded — `x` expands
        /// the row to every event it stands for.
        count: Option<u64>,
        /// **WHICH LEDGER ROW THIS IS**, so expansion can be remembered about
        /// a ROW rather than about the whole transcript. See [`RowKey`].
        /// `None` on a row no single event produced.
        key: Option<RowKey>,
        /// **WHETHER OPENING THIS ROW WOULD SHOW HIM ANYTHING**, computed by
        /// the projection as "the expanded rendering differs from this one".
        ///
        /// Derived rather than declared, for the reason [`Block::is_folded`]
        /// gives about reading the value instead of carrying a flag — but
        /// computed instead of guessed from a trailing `…`, which is what
        /// makes it safe to hang an affordance on. `0047` D6 measured the cost
        /// of guessing: 92% of `unknown` payloads were under the clip cap, so
        /// clicking them did nothing, and the face gave no sign in advance.
        expandable: bool,
        /// **WHETHER THIS ROW IS CURRENTLY OPEN**, so the expander can draw `▾`
        /// rather than `▸` and the affordance can report its own state.
        ///
        /// This is the ONE fact the shell knows and the render layer did not.
        /// Open-ness lives in `App::opened` (a set of [`RowKey`]s) plus the
        /// whole-transcript `x`; the projection CONSUMED both and reported only
        /// the resulting text, so `0052` drew one glyph for both states and
        /// recorded the gap rather than reaching outside its bound to close it.
        ///
        /// **Why a face that cannot say this is a face that lies quietly.** He
        /// clicks an expander, the row opens, and the marker does not move — so
        /// the only way to learn whether the gesture landed is to read the body
        /// and reason about whether it got longer. On a row whose expansion is
        /// SMALL, that is indistinguishable from a click that missed, which is
        /// exactly the failure `0052` had just finished fixing one level down
        /// (an affordance nothing drew). An affordance that cannot report its
        /// own state is worth less than the glyph it costs.
        ///
        /// It is a plain field and not a computed one, deliberately: it is not
        /// derivable from anything else on the block. `expandable` is about
        /// whether there is MORE; this is about whether the more is SHOWING.
        open: bool,
        /// **THE TRAILING CLAUSE THE CROP MAY NEVER EAT.**
        ///
        /// `Some(s)` declares that the last `s` of `text` is an AFFORDANCE —
        /// what this row tells him he can DO with it — and a closed blob
        /// reserves its columns BEFORE it measures anything else, so the
        /// gesture survives at every width and the elastic middle ellipsizes
        /// instead.
        ///
        /// **Why this is a field and not a care rule.** The third lane runs at
        /// [`crate::geom::blob_width`] columns since the firm margins landed,
        /// and a row that is NOT `expandable` has no second state to recover a
        /// cropped tail from — so on such a row a crop is not a fold, it is a
        /// deletion. The subagent tile is exactly that row: one line, never
        /// expandable, and the only thing in the chat that says the tree can be
        /// drilled. Losing its `→ opens its chat` cost nothing the screen could
        /// show and everything the gesture depended on.
        ///
        /// `None` on every other lane row, which crops exactly as before.
        affordance: Option<String>,
    },
    /// A horizontal rule — a compaction, a gap repair, a day boundary.
    Rule { text: String },
}

/// **THE STABLE IDENTITY OF ONE PROJECTED ROW**, and the whole reason per-row
/// expansion can exist at all.
///
/// It is `(thread id, ledger seq)`, which is the ledger's own identity for the
/// event the row came from — and it is deliberately NOT the row's index. An
/// index is a fact about one projection: the transcript re-projects on every
/// arrival, and a set of expanded indices would silently come to mean different
/// rows the moment anything landed above them. `IS-31`'s note calls per-block
/// expansion "a larger change than it looks" for exactly this reason; the
/// change is this type. The thread id is in the key because the fleet lane
/// interleaves threads whose `seq` counters are independent.
pub type RowKey = (String, u64);

impl Block {
    /// Whether this block is a SPEECH ACT — a box rather than a lane row.
    /// Used by the projection's own tests to say which shape a kind takes.
    pub fn is_message(&self) -> bool {
        matches!(self, Block::Message { .. })
    }

    /// **Whether this row SHOWS LESS THAN IT STANDS FOR** — a `×n` stamp, or
    /// prose the projection ellipsized. It is what `→` and a click activate:
    /// the operator cut `x` and ruled that *"if it's a truncated message we
    /// should be able to click and expand, or click right and it expands"*.
    ///
    /// Read off the VALUE rather than carried as a flag, deliberately: the
    /// fold and the clip are both visible in what was produced (`×n`, and the
    /// `…` `store::clip` appends), so there is no second field for the
    /// projection to forget to set. **The honest limitation**: a message whose
    /// own text genuinely ends in an ellipsis reads as folded. The cost of that
    /// is that `→` toggles expansion on a row that was already whole, which
    /// changes nothing on screen — cheap enough to prefer over a flag that can
    /// silently disagree with the text beside it.
    pub fn is_folded(&self) -> bool {
        match self {
            Block::Lane { count: Some(n), .. } if *n > 1 => true,
            Block::Message { text, .. } | Block::Lane { text, .. } => text.ends_with('…'),
            Block::Rule { .. } => false,
        }
    }

    /// **WHETHER THIS ROW HAS MORE TO SHOW** — the derived flag the projection
    /// computed, which supersedes [`Block::is_folded`]'s ellipsis heuristic as
    /// the thing an affordance is hung on. `is_folded` stays: it is about what
    /// the row LOOKS like, which is still what the `×n` stamp reports.
    pub fn is_expandable(&self) -> bool {
        match self {
            Block::Message { expandable, .. } | Block::Lane { expandable, .. } => *expandable,
            Block::Rule { .. } => false,
        }
    }

    /// **WHETHER THIS ROW IS SHOWING EVERYTHING IT HAS** — what the expander
    /// draws itself from. See [`Block::Lane::open`] for why the row model
    /// carries it at all.
    ///
    /// A [`Block::Rule`] is never open because it is never expandable; there is
    /// nothing behind a horizontal rule.
    pub fn is_open(&self) -> bool {
        match self {
            Block::Message { open, .. } | Block::Lane { open, .. } => *open,
            Block::Rule { .. } => false,
        }
    }

    /// The ledger row behind this block, when one event produced it.
    pub fn key(&self) -> Option<&RowKey> {
        match self {
            Block::Message { key, .. } | Block::Lane { key, .. } => key.as_ref(),
            Block::Rule { .. } => None,
        }
    }

    /// The text a copy would take.
    pub fn text(&self) -> &str {
        match self {
            Block::Message { text, .. } | Block::Lane { text, .. } | Block::Rule { text } => text,
        }
    }
}

/// Where the viewport is and whether it is following.
#[derive(Debug, Clone, Copy, Default)]
pub struct Viewport {
    /// Rows above the bottom of the laid-out content. `0` is the bottom.
    pub from_bottom: usize,
    /// On at the bottom, off the moment he scrolls up. Arrivals NEVER yank the
    /// viewport (`0023` `layout`).
    pub follow: bool,
    /// Blocks that arrived while follow was off. Renders as the `↓ N new` chip.
    pub new_since: usize,
}

impl Viewport {
    /// How a view opens: at the bottom, following. Not [`Default`], which is
    /// neither — a chat that opens scrolled away from the newest thing is a
    /// chat you have to fix before you can read it (`DIR-3`).
    pub fn at_bottom() -> Self {
        Self {
            from_bottom: 0,
            follow: true,
            new_since: 0,
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  the thread list
// ────────────────────────────────────────────────────────────────────────────

/// One row of a project's thread list.
///
/// **Every field on here except the last four comes out of a PROJECT INDEX**
/// (`<root>/.mind/threads.json`), not out of the daemon. `0035` established the
/// inversion and it is the whole of `IS-20`'s fix: the index drives, the daemon
/// decorates. A row exists because a human face wrote a header for it; the
/// daemon is asked, afterwards and by id, only whether it is running.
#[derive(Debug, Clone, Default)]
pub struct ThreadRow {
    /// The full id. Never rendered whole — it is 36 characters — but it is what
    /// a selection resolves to. `id == session_id == the daemon's thread id`,
    /// so the decoration join is exact and needs no translation.
    pub id: String,
    /// **WHICH ROOT'S INDEX THIS HEADER CAME FROM** — the `~/minds/<name>`
    /// basename, or `miscellaneous`.
    ///
    /// Inside a project it is constant and never rendered; on the ROOT's
    /// cross-project bands it is the only thing that says where a row lives,
    /// and the design doc draws it in that column (`recent`: *"Answer the tax
    /// letter — miscellaneous — 4h"*). It is also the key the AUI's pin
    /// mutations are addressed by (`{project, thread_id}`, `0035`
    /// `pinning_shared`), and it is NOT a field on any header — so a row that
    /// forgot it has nothing to address a future pin write with.
    pub project: String,
    // **THERE IS NO `short` FIELD, and its absence is the fix.** It held the
    // first eight characters of the id and existed so a row could print them.
    // `DIR-16` says not to — *"don't show me thread or task hash names. not
    // useful to me."* — and `0073` found it on 38 of 38 rows of his real face
    // anyway, because a field that exists to be drawn gets drawn. Leaving it
    // here with nothing reading it would be an invitation with a comment on
    // it; `store::short` is still there for the places that legitimately need
    // an abbreviated id, and none of them is a row.
    /// The index's own drifting summary. **This is the line he recognises a
    /// thread by**, and inside a project the cwd cannot be: every thread in a
    /// project shares it.
    pub summary: String,
    /// Verbatim from `threads/list`, shortened against `$HOME` by the shell.
    /// Rendered only when there is no summary to render instead.
    pub cwd: String,
    pub model: String,
    /// `active | parked | retrying | queued | idle`, verbatim from the daemon's
    /// own derivation. Never re-derived here. `absent` when the daemon has no
    /// row for this id at all — see [`ThreadRow::known`].
    pub state: String,
    pub executing: bool,
    pub queued: Known<u32>,
    pub unread: Known<u32>,
    /// `updated_at`, which is what the daemon's own list is ordered by.
    pub updated: Option<DateTime<Utc>>,
    /// `pinned` from the index. The AUI's pin and this one are the same bit in
    /// the same file, which is what the operator asked for ("you can use the
    /// aui registry for the pinned threads").
    pub pinned: bool,
    /// `closed` from the index. Kept in its own project and marked, never
    /// dropped — mirroring `libs/aui-nav/src/threads.rs:77-82`.
    pub closed: bool,
    /// **Whether the DAEMON knows this id.** `false` is a legal, permanent
    /// state: one index thread today has no daemon row, and it still belongs on
    /// the list. Daemon absence means unadopted, not un-owned.
    pub known: bool,
    /// **THE STATE COLUMN'S OVERRIDE** — `⋯ waiting 2`, `⋯ waiting 2 · ◆1`,
    /// `! 1 unreported` — for a thread whose state word would lie (`idle`
    /// while its subagents run). `None` draws [`ThreadRow::state`].
    pub activity: Option<(String, Severity)>,
    /// **`↳ «parent»`** — this thread is a managed subagent of the named
    /// thread. Drawn in place of the project column.
    pub parent: Option<String>,
}

/// One project's thread list.
#[derive(Debug, Clone, Default)]
pub struct ThreadList {
    /// Oldest first, so the NEWEST sits at the bottom — one direction for the
    /// whole face (`DIR-3`). Pinned rows are NOT hoisted: they are marked where
    /// they stand, because moving them would break the one direction.
    pub rows: Vec<ThreadRow>,
    /// Which row is selected. A list needs a cursor to be openable, and that is
    /// not what `CON-4` forbids: its ruling is about a per-message cursor in the
    /// TRANSCRIPT, reached by analogy to the audio face's playhead. This one is
    /// how `enter` knows what to open.
    pub cursor: usize,
    /// Rows above the bottom of the list.
    pub from_bottom: usize,
    /// **RETIRED as a meaning, kept as a field.** It was the daemon's recency
    /// WINDOW, back when this list was a page of `threads/list`. The list is no
    /// longer a page of anything: it is a project's whole index. `None` now
    /// means exactly "this project has no index yet".
    pub window: Option<u32>,
    /// Always true now — an index is not a page. Kept so the empty-state
    /// wording can tell "asked and got nothing" from "never asked".
    pub complete: bool,
}

// ────────────────────────────────────────────────────────────────────────────
//  the projects — THE ROOT MENU
// ────────────────────────────────────────────────────────────────────────────

/// One root the human faces own.
///
/// **The root of this face is PROJECTS, following the AUI** — every non-dot
/// directory under `~/minds/` (symlink-following), plus `~/.aui/miscellaneous`,
/// the un-projected bucket.
#[derive(Debug, Clone, Default)]
pub struct ProjectRow {
    /// The `~/minds/<name>` basename, which is also the key the AUI's mutation
    /// socket is addressed by.
    pub name: String,
    /// The root path, shortened against `$HOME` by the shell.
    pub path: String,
    /// **HOW MANY THREADS ITS INDEX CARRIES**, after dedupe — and `Unknown`
    /// when the index is PRESENT AND UNREADABLE.
    ///
    /// It was a bare `usize` beside a `no_index: bool`, and that pairing had
    /// nowhere to put "I could not read it" except the words `no index` on the
    /// row. The operator asked for those words to go; the DISTINCTION cannot go
    /// with them, because an unreadable index that renders as `0 thr` is a
    /// project that looks empty and is not — so it moved into the type.
    ///
    /// `Known(0)` is a true zero (no `.mind/`, no file, or an index that really
    /// is empty — `~/minds/plan`'s is literally `[]`). `Unknown` is the corrupt
    /// case and renders `?`, never a zero. Honesty rule 1, the same rule
    /// [`ProjectRow::active`] below is written under.
    pub threads: Known<usize>,
    /// How many of them are pinned.
    pub pinned: usize,
    /// **HOW MANY UNREAD THE DAEMON REPORTS ACROSS THIS PROJECT'S THREADS**,
    /// summed. `Unknown` until the daemon has answered once — never `0`, which
    /// is honesty rule 1's exact violation.
    ///
    /// One of the three facts he named for a project row: *"{N} thr, the time
    /// since last event … and the num unread."*
    pub unread: Known<u32>,
    /// How many the daemon says are executing right now. `Unknown` until the
    /// daemon has answered once — never `0`, which is honesty rule 1's exact
    /// violation.
    pub active: Known<u32>,
    /// The newest `last_turn_at` in the index. `None` when the project has no
    /// threads, which is a different thing from an unreadable index.
    pub last: Option<DateTime<Utc>>,
    /// **THE INDEX IS PRESENT AND WE COULD NOT READ IT.**
    ///
    /// Narrowed from "no index at all, OR one that did not parse". The absent
    /// case is an ordinary empty project now and says `0 thr`, which is true;
    /// only the CORRUPT case is still distinguished, and it is distinguished
    /// by `threads` being [`Known::Unknown`] rather than by a sentence.
    ///
    /// Kept as a field because the corrupt case earns more than a `?` if this
    /// face ever gets room to say so — and because deleting it would leave
    /// `Unknown` ambiguous between "corrupt" and "the daemon has not answered",
    /// which are different problems with different owners.
    pub unreadable_index: bool,
}

/// **HOW MANY ROWS A COLLAPSED BAND HOLDS** — ALL THREE OF THEM — in one
/// obvious place.
///
/// **TEN. `Q-14` IS ANSWERED**, and then generalised. His first answer was
/// about `recent` alone:
///
/// > that's not what recent is supposed to be it's supposed to be like the
/// > last 20 or 30 threads maybe if we just show like the last 10 we can do
/// > that
///
/// and his second, later the same round, took the other two bands with it:
///
/// > Let's make the default for pinned and projects just 10 like the recents.
///
/// So the constant stopped being `RECENT_ROWS` and became this. `Q-14`'s old
/// recommendation ("three to five, capped so the projects band stays above the
/// fold") is superseded twice over: the fold argument was only ever an argument
/// against an UNBOUNDED band, and with every band capped at ten the fold is
/// closer than it has ever been.
///
/// **THERE WAS NEVER A SELECTION MECHANISM.** His diagnosis of `recent` was
/// that it showed only what was "truly active right now"; it did not.
/// `all_headers` (`apps/tui/src/store.rs`) walks every project and every thread
/// with no activity filter at all, `Store::load_projects` reads every index
/// eagerly, and the band is a plain recency sort with closed rows excluded. The
/// cap was the entire cause, and `0072` measured that against his real `~/minds`
/// (182 recent rows behind a cap of three) before changing it.
pub const BAND_ROWS: usize = 10;

/// The words the empty RECENT band says ON ITS OWN PAGE.
///
/// The ROOT draws nothing at all for an empty recent band — no header over
/// nothing — and that is still right there, where two other bands are on
/// screen. It is wrong on a page that is ONLY that band: a screen he asked to
/// be taken to must never be blank (`0040` §10), so the page says what fills
/// the band instead.
pub const RECENT_EMPTY: &str = "nothing recent yet — threads you write to land here";

/// The words the empty PROJECTS band says, on the root and on its own page.
pub const PROJECTS_EMPTY: &str = "no projects under ~/minds — the AUI would show none either";


/// The words the empty PINNED band says, drawn verbatim in `0040` §10.
///
/// An empty pinned band is the one empty band that gets a sentence, because
/// the sentence is the only place the pin affordance is taught. The empty
/// RECENT band gets nothing at all — see [`root_lines`](crate::root_lines).
pub const PINNED_EMPTY: &str =
    "pinned — nothing pinned yet · pin a thread from its sheet and it lands here";

/// **WHICH BAND** — a NAME, not a string, because three readers now ask.
///
/// It was `&'static str` while a band header was only drawn. It stopped being
/// one the moment the header became a cursor target: `→` has to know which
/// band it is expanding, and comparing header captions to decide that is how a
/// caption change silently unbinds a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandId {
    Pinned,
    Recent,
    Projects,
}

impl BandId {
    /// The caption. The one place the word is spelled.
    pub fn label(self) -> &'static str {
        match self {
            BandId::Pinned => "pinned",
            BandId::Recent => "recent",
            BandId::Projects => "projects",
        }
    }

    /// Its index into [`ProjectList::totals`].
    pub fn slot(self) -> usize {
        match self {
            BandId::Pinned => 0,
            BandId::Recent => 1,
            BandId::Projects => 2,
        }
    }

    /// Every band, in surface order. So that "for each band" is written once.
    pub const ALL: [BandId; 3] = [BandId::Pinned, BandId::Recent, BandId::Projects];

    /// **WHAT THIS BAND'S OWN PAGE SAYS WHEN IT HOLDS NOTHING.**
    ///
    /// A page is only that band, so a blank one is a screen with nothing on it
    /// — the one thing `0040` §10 forbids everywhere. It is reachable only by a
    /// state change under him (an empty band draws no header and no foot at the
    /// root, so there is no door in), which is exactly why it must be drawn
    /// rather than assumed unreachable.
    pub fn empty_sentence(self) -> &'static str {
        match self {
            BandId::Pinned => PINNED_EMPTY,
            BandId::Recent => RECENT_EMPTY,
            BandId::Projects => PROJECTS_EMPTY,
        }
    }
}

/// **WHAT ONE SELECTABLE ROW OF THE ROOT SURFACE IS.**
///
/// The root stopped being a list of projects and became one scrolling surface
/// of three bands, so a cursor index no longer indexes `rows`. It indexes THIS
/// — and the mapping lives in [`ProjectList::item`] so that the renderer, the
/// hit map and the shell's `Open` all resolve a cursor the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootItem {
    /// **A BAND'S OWN HEADER — a cursor stop since `DEC-46`.**
    ///
    /// `DEC-37` drew band headers and ruled they were NOT stops. The operator
    /// retracted that in one sentence — *"if we are selecting one of the band
    /// headers and then click right we should just expand that band"* — and a
    /// header he cannot select is a header he cannot press `→` on. Everything
    /// else `DEC-37` decided stands: AUI-order layout, one layout function, the
    /// drill-in mechanics, and recent's newest-first exception to `DIR-3`.
    Band(BandId),
    /// **THE `see all` ROW AT THE FOOT OF A CAPPED BAND** — his own
    /// affordance, in his own words, twice. First:
    ///
    /// > Maybe we have the collapsed bands have an item at the bottom saying
    /// > 'show more' and we can also accomplish this by clicking right on the
    /// > band header.
    ///
    /// and then, this round, renaming it and changing what it opens:
    ///
    /// > let's replace 'show more' with 'see all' and it takes us to a
    /// > dedicated page with just that band and all results… Going right on a
    /// > band title does the same thing.
    ///
    /// "ALSO" and "the same thing" are the load-bearing words in the two
    /// quotes: the header is the SECOND door, not the only one. This row is
    /// where the affordance is taught, which is why the header still does not
    /// advertise a key.
    ///
    /// **IT IS NOT A BUTTON.** `→` on it opens the page; a click on it moves
    /// the cursor and stops, exactly as on every other row (`DEC-78`, and the
    /// operator's standing click ruling). There is no `Via::Click` arm that
    /// activates it and there must never be one.
    ///
    /// `RootItem::ShowLess` was its inverse and is GONE: with no in-place
    /// expansion there is nothing to collapse, and the page's way back is `←`.
    SeeAll(BandId),
    /// Row `n` of the cross-project PINNED band.
    Pinned(usize),
    /// Row `n` of the cross-project RECENT band.
    Recent(usize),
    /// Row `n` of [`ProjectList::rows`].
    Project(usize),
    /// **`+ new project — type its name`** (§1, §10). A stop, because a row he
    /// cannot put the cursor on is a row he cannot reach from the keyboard.
    NewProject,
    /// **`archived (N)` — THE DOOR**, now that there is a room behind it.
    ///
    /// It was not a stop while the room did not exist (`0053` shipped the
    /// counted row honestly with the door shut). It is one now. The row is
    /// still drawn at `archived (0)`, unchanged — *a row that comes and goes
    /// reshapes the menu under his muscle memory*.
    Archived,
}

/// The root menu — **ONE SCROLLING SURFACE, three bands** (`DEC-28`).
///
/// The AUI's root order survives intact — pinned first, recent next, the
/// projects, chrome last — but as bands on one surface rather than three
/// screens, because the screens-per-list shape was an audio necessity and on
/// glass they are one glance. **The ORDER is the design's argument** and the
/// muscle memory rides on it.
#[derive(Debug, Clone, Default)]
pub struct ProjectList {
    /// Alphabetical, as `discover_projects()` sorts them, so his hand knows
    /// where a project is without reading. Archived projects are NOT here —
    /// see [`ProjectList::archived`].
    pub rows: Vec<ProjectRow>,
    /// **THE PINNED BAND — every pin, cross-project, in pin order.**
    ///
    /// `pin_order` ascending, tie-broken by `last_turn_at` DESCENDING, which is
    /// `build_pinned_items` (`apps/aui/src/dispatch/unread.rs:110-133`) exactly.
    /// Deduped by thread id across roots: `.mind/threads.json` is tracked in
    /// git, so 46 ids live in three or four indexes and a naive union shows
    /// "Broomstick platform braindump" four times (`0035` `ambiguous_cases`).
    pub pinned: Vec<ThreadRow>,
    /// **THE RECENT BAND — the jump-back-in shortcut**, newest FIRST, capped at
    /// [`RECENT_ROWS`].
    ///
    /// Newest first is a deliberate exception to `DIR-3`'s one direction, and
    /// the doc draws it that way: `DIR-3` is about a stream of things that
    /// ARRIVE and scroll, and this is a fixed-height shortcut read from its
    /// top. Closed threads are excluded — they stay visible inside their own
    /// project, mirroring `build_recent_items` and `0035`'s ruling.
    pub recent: Vec<ThreadRow>,
    /// **WHICH BAND THIS PROJECTION IS FOCUSED ON — the `see all` PAGE.**
    ///
    /// `None` is the ROOT: three bands, each capped at [`BAND_ROWS`], with the
    /// chrome tail. `Some(b)` is `Mode::SeeAll(b)` — THAT BAND AND NOTHING
    /// ELSE, uncapped: no band-header row (the header LINE names the band), no
    /// `see all` foot, no spacers, no `+ new project`, no `archived`. See
    /// [`root_lines`].
    ///
    /// **IT REPLACED `expanded: [bool; 3]`, and the replacement is the shape of
    /// the feature.** Band expansion put more rows on the root in place; he
    /// asked instead for *"a dedicated page with just that band and all
    /// results"*, so a per-band open/closed FLAG became a per-projection FOCUS
    /// — one band or the root, never a root with one band grown. There is
    /// nothing to collapse any more, which is why `show less` is gone.
    ///
    /// It is DERIVED FROM THE MODE rather than stored beside it: the shell
    /// passes `Some(b)` exactly when it is projecting `Mode::SeeAll(b)`, so the
    /// page cannot be focused on a band the mode does not name.
    pub focus: Option<BandId>,
    /// **HOW MANY ROWS EACH BAND WOULD HOLD IF NOTHING CAPPED IT**, indexed by
    /// [`BandId::slot`] — every pin, every non-closed thread, every project.
    ///
    /// Carried so the `show more` row can say `172 not shown` rather than
    /// inviting him into a band without saying how deep it is. A band that
    /// shows a cap without showing what it is capping looks complete, which is
    /// the exact misreading `0072` was sent to fix.
    pub totals: [usize; 3],
    /// How many projects the AUI's archive set is hiding
    /// (`~/.aui/archived-projects.json`).
    ///
    /// **The counted row is drawn even at zero**, because a row that comes and
    /// goes reshapes the menu under his muscle memory (`DEC-28`). Fail-open: an
    /// unreadable archive file hides NOTHING and counts zero, which is the one
    /// direction this must never fail in (`apps/aui/src/v2/archived.rs`).
    pub archived: usize,
    /// Which SELECTABLE row is under the cursor — an index into
    /// [`ProjectList::item`]'s sequence, NOT into `rows`.
    pub cursor: usize,
    pub from_bottom: usize,
    /// False until the filesystem has been read once.
    pub loaded: bool,
    /// **THE NAME HE IS TYPING INTO `+ new project`**, when he is.
    ///
    /// `None` is the resting row. It lives on the projection but is MOVED IN by
    /// the shell, which owns the buffer for the same reason the sheet's field
    /// is owned there: the projection is rebuilt on every intent, so a buffer
    /// born here would be re-created empty on the next keystroke.
    pub new_project: Option<crate::compose::Composer>,
    /// **THE SENTENCE THE AUI-OWNED CHROME ROW WEARS** when the audio interface
    /// is unreachable. `None` when it is up.
    ///
    /// It reaches only `+ new project`, which is an ACT. The `archived` row is
    /// a DOOR onto a set this face reads off the filesystem itself, so it opens
    /// in every state — nothing rendered is sourced from 9010 (`0060`'s second
    /// binding condition), and the unavailability lands on the unarchive rows
    /// inside, which are the acts.
    pub aui_down: Option<String>,
}

impl ProjectList {
    /// How many rows the cursor can stand on.
    ///
    /// **DERIVED FROM [`root_lines`], not counted a second time.** It used to
    /// be `pinned + recent + rows`, which was true only while every stop was a
    /// content row. `0063` made two CHROME rows stops, and a second count would
    /// then have had to know about the `!loaded` case (which draws one hint and
    /// no chrome at all) — two answers to "how many stops are there", the
    /// second of which decides where the cursor may go.
    pub fn selectable(&self) -> usize {
        self.lines().iter().filter(|l| l.selectable()).count()
    }

    /// **WHAT A BAND HEADER SAYS AFTER ITS NAME: THE TOTAL, AND NOTHING
    /// ELSE.**
    ///
    /// `pinned · 26`, `recent · 182`, `projects · 33`. It is the count of what
    /// EXISTS, not of what is drawn, which is the number he asked for:
    ///
    /// > You can say the number of projects available in the band header.
    ///
    /// **THE AFFORDANCE HINT WAS HERE AND HE TOOK IT OUT.** This read
    /// `recent · 10 of 182 · → for all` for exactly one build, until:
    ///
    /// > Don't show `· → for all` in the header. It can be discovered.
    ///
    /// Discovered WHERE is not left to chance — the `show more` row at the foot
    /// of the band is the affordance, in a place he is already looking when he
    /// reaches the end of what is shown. `→` on the header is the second door
    /// onto the same act and is deliberately unadvertised.
    pub fn band_note(&self, b: BandId) -> String {
        self.totals[b.slot()].to_string()
    }

    /// **HOW MANY ROWS THIS BAND IS HIDING RIGHT NOW.** Zero when it is showing
    /// everything it has, which is also when the `show more` row is not drawn.
    pub fn band_hidden(&self, b: BandId) -> usize {
        self.totals[b.slot()].saturating_sub(self.band_len(b))
    }

    /// How many rows of this band are DRAWN.
    pub fn band_len(&self, b: BandId) -> usize {
        match b {
            BandId::Pinned => self.pinned.len(),
            BandId::Recent => self.recent.len(),
            BandId::Projects => self.rows.len(),
        }
    }

    /// **IS THERE ANYTHING THE ROOT IS NOT SHOWING OF THIS BAND?** `false`
    /// means the band is complete on the root, which is also when the
    /// `see all` foot is not drawn.
    ///
    /// Derived from what is on screen against what exists — never from which
    /// band it is — so a `recent` band that happens to hold every thread he
    /// owns correctly reports itself complete, and a `pinned` band with eleven
    /// pins correctly reports that it is not.
    ///
    /// **IT NO LONGER GATES `→` ON A HEADER.** `App::expand_band` used to
    /// answer a press on a complete band's header with *"pinned is already
    /// showing all 4"*; now `→` on a header always opens that band's page, in
    /// a room of its own, and there is ONE law for the key. The foot is still
    /// drawn only when this is true, because a row offering to show more over
    /// nothing is the invented-zero defect wearing an affordance.
    pub fn band_has_more(&self, b: BandId) -> bool {
        self.band_hidden(b) > 0
    }

    /// **WHAT THE `see all` ROW SAYS.** HIS WORD FIRST, then the honest depth —
    /// an invitation into a band that does not say how deep it is is an
    /// invitation to be surprised.
    ///
    /// > let's replace 'show more' with 'see all'
    ///
    /// The wording is the design's (`0133`) and is HIS to overturn: the bare
    /// `see all`, with no count, is one format string away. The count is kept
    /// because it is the whole job the row it replaces was doing, and the band
    /// header's note carries the TOTAL rather than the remainder.
    pub fn see_all_words(&self, b: BandId) -> String {
        format!("see all — {} more", self.band_hidden(b))
    }

    /// **THE ONE RESOLUTION** of a cursor index to the thing it names. Band
    /// order is surface order: pinned, recent, projects, `+ new project`,
    /// `archived`. It walks the SAME layout the renderer draws and the hit map
    /// claims, so a cursor index cannot mean one thing here and another there.
    pub fn item(&self, i: usize) -> Option<RootItem> {
        self.lines()
            .iter()
            .filter(|l| l.selectable())
            .nth(i)
            .and_then(RootLine::item)
    }

    /// The row the cursor names, whichever band it is in.
    pub fn thread_at(&self, i: usize) -> Option<&ThreadRow> {
        match self.item(i)? {
            RootItem::Pinned(n) => self.pinned.get(n),
            RootItem::Recent(n) => self.recent.get(n),
            RootItem::Band(_)
            | RootItem::SeeAll(_)
            | RootItem::Project(_)
            | RootItem::NewProject
            | RootItem::Archived => None,
        }
    }

    /// The surface, line by line. See [`root_lines`].
    pub fn lines(&self) -> Vec<RootLine> {
        root_lines(self)
    }

    /// How many lines the surface lays out to — which is NOT
    /// [`ProjectList::selectable`], because band headers and spacers are drawn
    /// and cannot be stood on.
    pub fn drawn_len(&self) -> usize {
        self.lines().len()
    }

    /// **WHICH DRAWN LINE A CURSOR INDEX SITS ON.** The shell's viewport
    /// arithmetic is in drawn rows and the cursor is in selectable rows, so this
    /// is the one place the two coordinate systems meet — and it is derived from
    /// [`root_lines`] rather than counted independently, which is how the two
    /// would drift.
    pub fn drawn_row_of(&self, cursor: usize) -> usize {
        let mut seen = 0usize;
        for (row, l) in self.lines().iter().enumerate() {
            if l.selectable() {
                if seen == cursor {
                    return row;
                }
                seen += 1;
            }
        }
        self.drawn_len().saturating_sub(1)
    }
}

/// **ONE LAID-OUT LINE OF THE ROOT SURFACE.**
///
/// The root is no longer index-identical to `rows`: it interleaves band
/// headers, spacers and a chrome tail between three lists. This enum is what it
/// interleaves them as, and [`root_lines`] is the ONE function that decides the
/// order — read by the renderer, by the hit map, by the drag-selection's unit
/// mapping and by the shell's scroll arithmetic. Four readers, one layout; a
/// second count of the surface anywhere is how a click lands one row off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootLine {
    /// A band's name, at column 0 — and **A CURSOR STOP** (`DEC-46`).
    Band(BandId),
    /// `see all — N more`, the last row of a capped band, and the door onto
    /// that band's own page. Drawn only when the band is hiding rows.
    SeeAll(BandId),
    /// A sentence where a list would be. Never a blank screen and never an
    /// invented zero (`0040` §10).
    Hint(&'static str),
    /// Blank air between bands.
    Spacer,
    Pinned(usize),
    Recent(usize),
    Project(usize),
    /// **`+ new project — type its name`**, the last row of the projects band.
    /// §1 and §10 both draw it there — inside the band, above the spacer — and
    /// not in the chrome tail.
    NewProject,
    /// The counted chrome row at the tail, drawn EVEN AT ZERO.
    Archived,
}

impl RootLine {
    /// Whether the cursor can stand here. Spacers and hints cannot be opened,
    /// so they are not stops.
    ///
    /// **The two chrome rows ARE stops**, and that is the whole of `0063`:
    /// `+ new project` takes a typed name and `archived (N)` opens the archived
    /// set, so both are things he does rather than things he reads.
    ///
    /// **AND THE BAND HEADERS ARE STOPS NOW** (`DEC-46`, retracting `DEC-37`'s
    /// one clause). A header is a thing he does too: `→` expands the band. It
    /// is one more row in this list and NOT a special case the cursor skips
    /// into and out of with different rules in each direction — `↑`/`↓` walk
    /// through it exactly as they walk through any other row, because
    /// [`ProjectList::item`] and the renderer both read THIS function and
    /// neither has a second opinion about where the cursor may stand.
    pub fn selectable(&self) -> bool {
        matches!(
            self,
            RootLine::Band(_)
                | RootLine::SeeAll(_)
                | RootLine::Pinned(_)
                | RootLine::Recent(_)
                | RootLine::Project(_)
                | RootLine::NewProject
                | RootLine::Archived
        )
    }

    /// The thing this line names, for the lines that name one.
    pub fn item(&self) -> Option<RootItem> {
        match *self {
            RootLine::Band(b) => Some(RootItem::Band(b)),
            RootLine::SeeAll(b) => Some(RootItem::SeeAll(b)),
            RootLine::Pinned(i) => Some(RootItem::Pinned(i)),
            RootLine::Recent(i) => Some(RootItem::Recent(i)),
            RootLine::Project(i) => Some(RootItem::Project(i)),
            RootLine::NewProject => Some(RootItem::NewProject),
            RootLine::Archived => Some(RootItem::Archived),
            _ => None,
        }
    }
}

/// **THE ROOT SURFACE, IN ORDER — and the ORDER IS THE DESIGN'S ARGUMENT.**
///
/// Pinned, recent, projects, chrome last. That is the AUI's root order carried
/// over intact (`DEC-28`), and the muscle memory rides on it: the operator's
/// hand knows how far down the projects are, so nothing here may be
/// rearranged.
///
/// **The empty cases are the doc's, not a convention invented here.** An empty
/// PINNED band draws its teaching sentence, because `0040` §10 draws exactly
/// that line and it is the only place the pin affordance is taught. An empty
/// RECENT band draws NOTHING AT ALL — no header over nothing — which is what
/// §10's own frame does when there is nothing recent, and it is also the
/// general rule this build adopts wherever the doc is silent.
///
/// **AND IT IS ALSO THE `see all` PAGE.** With [`ProjectList::focus`] set it
/// emits ONE band's rows, uncapped, and no chrome — one layout function with
/// one extra question, rather than a second row model for a second surface.
/// That is why every consumer of `ProjectList::item` (the renderer, the hit
/// map, the drag-selection's unit mapping, `o`, `r`, `p`, `c`, move-pin and the
/// shell's scroll arithmetic) works on the page with no second implementation:
/// there is still exactly one resolution of a cursor index to the thing it
/// names.
pub fn root_lines(list: &ProjectList) -> Vec<RootLine> {
    // Before the filesystem has been read once there is no honest band to draw:
    // "nothing pinned yet" and "no projects" are both claims we have not earned.
    if !list.loaded {
        return vec![RootLine::Hint("reading ~/minds…")];
    }
    let mut v = Vec::new();

    // **THE `see all` PAGE IS THIS FUNCTION WITH ONE BAND IN FOCUS**, and it
    // returns before the root's order is laid at all.
    //
    // *"a dedicated page with just that band and all results"* — so: that
    // band's rows, ALL of them, and NOTHING else. No band-header row, because
    // the header LINE names the band and a caption above the only band on
    // screen is a second name for what he is looking at. No `see all` foot,
    // because he is already there. No spacers, no `+ new project`, no
    // `archived` tally: those are the ROOT'S chrome and the page is not the
    // root.
    //
    // **THE EMPTY PAGE DRAWS A SENTENCE AND NOT A BLANK SCREEN** (`0040` §10).
    // It is reachable only by a state change under him — an empty band has no
    // header and no foot on the root, so there is no door into its page — and
    // that is exactly why it is drawn rather than assumed away. See
    // [`BandId::empty_sentence`].
    if let Some(b) = list.focus {
        if list.band_len(b) == 0 {
            return vec![RootLine::Hint(b.empty_sentence())];
        }
        let row: fn(usize) -> RootLine = match b {
            BandId::Pinned => RootLine::Pinned,
            BandId::Recent => RootLine::Recent,
            BandId::Projects => RootLine::Project,
        };
        v.extend((0..list.band_len(b)).map(row));
        return v;
    }

    // **ONE BAND, ONE SHAPE** — header, rows, foot. Written once and called
    // three times, because three copies of "and then the `see all` row, unless
    // there is nothing hidden" is three places for the fourth band to be
    // forgotten.
    //
    // The FOOT is `see all` on a band that is hiding rows and nothing at all on
    // a band that is simply complete — a row that offers to show more over
    // nothing is the invented-zero defect wearing an affordance. There is no
    // `show less` foot any more: nothing expands in place, so there is nothing
    // to collapse.
    fn band(v: &mut Vec<RootLine>, list: &ProjectList, b: BandId, row: fn(usize) -> RootLine) {
        v.push(RootLine::Band(b));
        v.extend((0..list.band_len(b)).map(row));
        if list.band_has_more(b) {
            v.push(RootLine::SeeAll(b));
        }
    }

    // **THE ORDER IS UNCHANGED**: pinned, recent, projects, chrome last.
    if list.pinned.is_empty() {
        v.push(RootLine::Hint(PINNED_EMPTY));
    } else {
        band(&mut v, list, BandId::Pinned, RootLine::Pinned);
    }
    v.push(RootLine::Spacer);

    if !list.recent.is_empty() {
        band(&mut v, list, BandId::Recent, RootLine::Recent);
        v.push(RootLine::Spacer);
    }

    if list.rows.is_empty() {
        v.push(RootLine::Band(BandId::Projects));
        v.push(RootLine::Hint(PROJECTS_EMPTY));
    } else {
        band(&mut v, list, BandId::Projects, RootLine::Project);
    }
    // **`+ new project` IS THE LAST ROW OF THE BAND**, where §1 and §10 both
    // draw it — inside the projects band, above the spacer, so it reads as one
    // more project-shaped thing rather than as chrome. It sits BELOW the
    // band's `show more` foot: the foot is about the list, and this is one more
    // thing he can do with it.
    //
    // **It is drawn when there are NO projects too**, and that is the point of
    // it: §10's empty root is a state whose way out is on screen, exactly as an
    // empty project's way out is its composer.
    //
    // `0053` declined this row on stub doctrine — *"creating a project is a
    // filesystem mutation this face does not make"* — and that reasoning was
    // right then. It is obsolete now: `Mutation::CreateProject` is a built,
    // production v2 handler (`apps/aui/src/v2/mutate.rs:442-476`), the face
    // still performs no write, and the doctrine is satisfied by ROUTING TO THE
    // OWNER rather than by abstention.
    v.push(RootLine::NewProject);

    // **The chrome tail.** `archived` keeps its counted row even at zero — a
    // row that comes and goes reshapes the menu under his muscle memory.
    v.push(RootLine::Spacer);
    v.push(RootLine::Archived);
    v
}

// ────────────────────────────────────────────────────────────────────────────
//  THE ARCHIVED SET — the room behind the counted row (§1)
// ────────────────────────────────────────────────────────────────────────────

/// One project in the archive set.
///
/// §1 draws it as `splash-legacy — click to bring it back`, with its age on the
/// right. Unarchiving is the ONE act — *"there is no read-only second entrance
/// to a room you said you didn't want to see"* — so there is nothing else on
/// the row and nothing else to press.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArchivedRow {
    pub name: String,
    /// When he archived it, from `~/.aui/archived-projects.json`. `None` is `?`
    /// and never an invented date.
    pub since: Option<DateTime<Utc>>,
    /// The AUI owns unarchive, so an unreachable AUI is a sentence ON THE ROW —
    /// the same rule as every act row on the sheet, and the same reason.
    pub avail: Avail,
}

impl ArchivedRow {
    /// The row exactly as it is drawn, reason winning over the standing hint.
    pub fn words(&self) -> String {
        match self.avail.reason() {
            Some(why) => format!("{} — {why}", self.name),
            None => format!("{} — click to bring it back", self.name),
        }
    }
}

/// **THE ARCHIVED SET, OPENED.** One door back, one act (§1).
///
/// It is READ FROM THE FILESYSTEM by this face, never from 9010: `0060`'s
/// second binding condition is that nothing rendered is sourced from the AUI's
/// socket, so the door opens and the room draws whether or not the audio
/// interface is running. What needs the AUI is the act, and that is on the row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArchivedView {
    pub rows: Vec<ArchivedRow>,
    pub cursor: usize,
}

impl ArchivedView {
    pub fn row(&self, i: usize) -> Option<&ArchivedRow> {
        self.rows.get(i)
    }
    pub fn selected(&self) -> Option<&ArchivedRow> {
        self.rows.get(self.cursor)
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  THE STDOUT-ONLY TOGGLE — one boolean, and the last of `DIR-4`'s machinery
// ────────────────────────────────────────────────────────────────────────────
//
// **THE LANE FILTER IS GONE AND THE TOGGLE IS WHAT SURVIVED IT.** His words,
// complete: *"filter is too much. Just drop that feature. We'll just tune the
// interface. I had that request because I wanted a simple toggle. This is too
// granular and not useful to me."*
//
// What used to live here was a nine-group `Lane` enum, a `LANES` table naming
// each group and the digit that toggled it, and a `Filter` bitmask with a
// stash for the one-key collapse. Every one of those was the FILTER'S; the
// thing he asked for was the collapse. It is now `ViewState::stdout_only` — a
// single `bool` — and there is nothing left to configure: no digit reaches it,
// no overlay tunes it, and the header says `stdout only` rather than a count
// of hidden groups he had to decode.
//
// **`DIR-4` IS DISCHARGED, NOT CONTRADICTED.** *"Give me ALL the events and
// I'll filter down"* is what shipped: the breadth arrived, he filtered down
// twice — first to a ten-kind short list, then to this — and the conclusion is
// that he wants no filter CONTROL at all. Nothing is dropped from the ledger,
// `x` still reaches every event (the default face applies no predicate at
// all), and what changed is that the knob is gone.
//
// **THE CLASSIFIER THAT SURVIVED IS INVISIBLE.** Deciding whether a row is
// speech is still a question the projection has to answer while the toggle is
// on, and `store::is_stdout` answers it — one predicate, no key, no overlay,
// no indicator, no label, no user-facing word. The nine-group vocabulary is
// not part of it.

// ────────────────────────────────────────────────────────────────────────────
//  the queue overlay — DIR-6's second half
// ────────────────────────────────────────────────────────────────────────────

/// One queued or failed message, verbatim from `threads/{id}/queue` /
/// `threads/{id}/queue_failed`.
#[derive(Debug, Clone)]
pub struct QueueItem {
    pub message_id: u64,
    pub text: String,
    pub intent: String,
    pub created_at: String,
}

/// How a fetched-on-demand surface is doing. `Failed` carries the daemon's own
/// words: a surface that cannot say why it is empty reads as empty.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Fetch {
    #[default]
    Idle,
    Loading,
    Loaded,
    Failed(String),
}

/// `u` opens this. **VIEW ONLY in this build**, and it says so on its own last
/// line rather than offering a cancel or a re-select that would be a mutation
/// this step is forbidden to send.
#[derive(Debug, Clone, Default)]
pub struct QueueView {
    /// Which thread's queue. Empty when nothing is in focus.
    pub thread: String,
    pub waiting: Vec<QueueItem>,
    /// The re-select surface. Load-bearing: it is where an exhausted retry
    /// ladder ends up, and a queue view that omitted it would hide exactly the
    /// rows that most need a human.
    pub failed: Vec<QueueItem>,
    pub state: Fetch,
}

/// The composer band.
///
/// **A REAL TEXTAREA since build step 4.** The buffer, the cursor, the wrap and
/// the undo are [`crate::compose::Composer`]'s — in this crate, as data, so the
/// golden harness can see every row of it and `0018`'s repaint law can reason
/// about it. A self-rendering widget in this band would have been the one part
/// of the screen neither of those can reach, which is why there is not one.
#[derive(Debug, Clone, Default)]
pub struct Composer {
    /// The model. See [`crate::compose`] for the three decisions it is.
    pub buf: crate::compose::Composer,
    /// Whether the band accepts keystrokes. False only where there is nothing
    /// to compose TO — which today means it is true whenever the band is drawn
    /// at all, since the band is drawn only in a thread.
    pub editable: bool,
    /// **A DRAFT RECOVERED FROM DISK, OFFERED AND NOT YET ANSWERED.**
    ///
    /// It is an OFFER and never an insertion: a stale draft silently re-typed
    /// into the box would be indistinguishable from something he wrote this
    /// minute, which is the same honesty rule the disconnected transcript obeys.
    /// See `tui::draft` for the journal it came from and the argued exception
    /// it is.
    pub offer: Option<DraftOffer>,
    /// **A SEND IS ON THE WIRE.** While it is set, the band says so and another
    /// `enter` cannot fire a second one — a double send is his words arriving
    /// twice, and the round trip is long enough for two presses to fit inside
    /// it.
    pub sending: bool,
    /// **THE BAND IS REPLACED BY TWO SENTENCES**, because the thing the
    /// transcript is showing cannot receive a message. See [`StandIn`].
    /// `Some` implies `editable == false`; the parent's draft stays in `buf`
    /// untouched and comes back with the parent.
    pub stand_in: Option<StandIn>,
}

/// A draft the journal had, waiting on a yes or a no.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftOffer {
    pub text: String,
    /// How long ago it was written, already worded by the shell — this crate
    /// has no clock to subtract with.
    pub age: String,
}

// ────────────────────────────────────────────────────────────────────────────
//  THE SHEET — details and options, one door (§5)
// ────────────────────────────────────────────────────────────────────────────

/// **One act the sheet offers, in THIS crate's vocabulary.**
///
/// Deliberately NOT the shell's `acts::ThreadAct`, and deliberately carrying no
/// socket, no mutation name and no port number. This crate may not know the AUI
/// wire exists — `render/Cargo.toml` is the gate that enforces it — and that
/// boundary is doing real work here rather than being a formality: **the sheet
/// is drawn identically whichever process is behind a row**, which is the
/// design's own first sentence. *"He does not need to know there are two
/// owners; he needs the row to work, and when it cannot, to say why in words he
/// can act on."*
///
/// The shell maps these onto owners in ONE place (`acts::route`), so the
/// binding is testable without a frame and the frame is testable without a
/// binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetAct {
    /// Cycle the model override. `None` -> Sonnet -> Opus -> Fable -> `None`.
    Model,
    Interrupt,
    /// Opens the instructions field.
    Compact,
    /// Opens the summary field, pre-filled with the current summary.
    Rename,
    /// Pin, or unpin if it is already pinned.
    Pin,
    MoveUp,
    MoveDown,
    /// Mark closed, or reopen if it is already closed.
    Closed,
    /// **PUT THE THREAD'S FULL ID ON THE CLIPBOARD.** The one deliberate home
    /// of an id on this face, and it is the EXCHANGE `0076` owed for taking
    /// the eight-character id off the list row and out of the copied run.
    ///
    /// `DEC-27` made a thread row copy its id so that a thread had a reachable
    /// address; `DIR-16` says the face may not SHOW him a hash. An act row
    /// satisfies both: it is a VERB, so §5's *"absent, deliberately … the id"*
    /// still holds of the sheet's FACTS line, and the address is still one
    /// gesture away when he actually wants it. Removing the last reachable
    /// copy of a thread's address would have been a regression dressed as a
    /// fix.
    ///
    /// It copies the FULL thirty-six characters, not the eight the row used
    /// to: the row's abbreviation was forced by column mapping (`select`), and
    /// an act has no columns to map.
    CopyId,

    // ── the PROJECT sheet's three acts (§5's second frame) ───────────────
    //
    // They are members of the SAME enum as the thread's, because §5 draws
    // **one shape** — *"a project's title opens the same shape"* — and one
    // shape with two act vocabularies would be two surfaces wearing one name.
    // The subject is carried by [`Facts`]; the act set is carried here.
    /// Cycle the project's DEFAULT model. `None` clears it back to the global.
    ProjectModel,
    /// `archive <name>` — leaves the root, threads kept, one click brings it
    /// back. **Never drawn for `miscellaneous`** — see [`ProjectFacts::bucket`].
    Archive,
    /// `remove <name> — asks first`. The most destructive row on this face: it
    /// unlinks `~/minds/<name>`. **Never drawn for `miscellaneous`.**
    Remove,
}

/// **WHETHER A ROW CAN ACT, AND IF NOT, WHY — IN WORDS.**
///
/// `0060`'s rule, and it is his own ruling applied: *"A row is NEVER removed
/// and NEVER greyed without a reason."* A row that comes and goes reshapes the
/// menu under his muscle memory — the same finding that made `archived (0)`
/// keep its counted row at zero. And a row that is drawn but silently does
/// nothing is the stub the doctrine names.
///
/// So there are exactly two states and the second one carries a SENTENCE. There
/// is no third state for "greyed", because greyed without words is the thing
/// being forbidden.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Avail {
    /// The act will be attempted. (Whether the far end then refuses it is the
    /// far end's business, and its refusal is relayed verbatim.)
    #[default]
    Ready,
    /// Drawn muted, at its usual position, with this reason appended in words.
    /// Activating it repeats the same sentence once in the notice line and does
    /// nothing else; the row stays selected.
    Unavailable(String),
}

impl Avail {
    pub fn is_ready(&self) -> bool {
        matches!(self, Avail::Ready)
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Avail::Ready => None,
            Avail::Unavailable(r) => Some(r),
        }
    }
}

/// One act row of the sheet.
///
/// A row is a STEM plus a trailing clause, and the clause is the one thing that
/// changes with availability. `0060`'s degraded frame draws
/// `model opus — needs the audio interface, which is not running` where the
/// ready frame draws `model opus — click to change`: the reason does not stack
/// on top of the hint, it REPLACES it. Modelling stem and hint separately is
/// what makes that substitution a property of the type rather than of whoever
/// writes the next renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetRow {
    pub act: SheetAct,
    /// `model opus`, `rename`, `pin to the root`, `mark closed`.
    pub stem: String,
    /// `click to change`, `edit the summary in place`. `None` for a row whose
    /// stem says the whole thing (`interrupt the running turn`).
    pub hint: Option<String>,
    pub avail: Avail,
}

impl SheetRow {
    /// The row exactly as it is drawn, reason winning over hint.
    pub fn words(&self) -> String {
        match (self.avail.reason(), self.hint.as_deref()) {
            (Some(why), _) => format!("{} — {why}", self.stem),
            (None, Some(h)) => format!("{} — {h}", self.stem),
            (None, None) => self.stem.clone(),
        }
    }
}

/// **THE TYPED FIELD** — compaction instructions, and the summary a rename
/// edits in place.
///
/// §5: *"Rename edits the summary in place — the capture prompt scope existed
/// because the closed phone cannot type. Compaction instructions are typed the
/// same way."* Both are the same affordance, so both are this one type.
///
/// It reuses [`crate::compose::Composer`] rather than holding a `String`, for
/// the reason that crate exists: a cursor must move in EXTENDED GRAPHEME
/// CLUSTERS, and a second, simpler text field would be a second place to get
/// that wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetField {
    /// Which row opened it. Only [`SheetAct::Compact`] and [`SheetAct::Rename`]
    /// ever do.
    pub act: SheetAct,
    /// `instructions` / `summary` — drawn in the field's own border, the way
    /// the composer band names itself.
    pub label: &'static str,
    pub buf: crate::compose::Composer,
}

/// The four facts the sheet's top line states. `None` is `?` and NEVER a zero
/// or a blank — honesty rule 1, which this face applies to a date exactly as it
/// applies it to a count.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SheetFacts {
    /// The `~/minds/<name>` basename this thread was read from.
    pub project: String,
    pub started: Option<DateTime<Utc>>,
    pub turns: u64,
    pub last_turn: Option<DateTime<Utc>>,
}

/// **THE FACTS OF A PROJECT** — §5's second frame, `48 threads · 1 running ·
/// oldest Sep 2025`.
///
/// A project is not a thread and its facts are not the thread's, so it gets its
/// own struct rather than a `SheetFacts` with four fields left blank. The
/// `Option`/[`Known`] discipline is identical: a fact we were not given is `?`,
/// never a zero.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectFacts {
    /// The `~/minds/<name>` basename — the key every AUI project mutation is
    /// addressed by, and the word the archive and remove rows say out loud.
    pub name: String,
    pub threads: usize,
    /// How many of its threads the daemon says are executing. `Unknown` until
    /// the daemon has answered once — never `0`, which is honesty rule 1's
    /// exact violation. Drawn only when it is a nonzero known: *"the running
    /// count appears only when nonzero — a column of zeros is noise"* (§1).
    pub running: Known<u32>,
    /// The OLDEST `created_at` in its index. `None` is `?`.
    pub oldest: Option<DateTime<Utc>>,
    /// **`miscellaneous` — THE UN-PROJECTED BUCKET, and it is a different KIND
    /// of thing.**
    ///
    /// It is not under `~/minds` and it is never archivable
    /// (`crate` consumers: `apps/tui/src/index.rs:277-279`), so `archive` and
    /// `remove` are **ABSENT from its sheet, not unavailable**. That is a
    /// deliberate departure from the unreachable-owner rule and the distinction
    /// is the whole point: an unreachable owner is the SAME menu at a different
    /// time and its row must stay drawn with its reason, whereas this is not
    /// that menu at all. A greyed `archive` here would promise a temporary
    /// outage that will never lift.
    ///
    /// The facts line says so in words, so the absence is explained where he is
    /// looking rather than left as a silence.
    pub bucket: bool,
}

/// **WHAT THE SHEET IS ABOUT** — a thread, or a project.
///
/// §5 draws one shape for both (*"a project's title opens the same shape"*), so
/// there is one [`SheetView`], one cursor, one key table and one renderer. What
/// differs is the facts line and the act set, and this enum is the seam between
/// them. Making it an enum rather than two optional structs is what stops a
/// sheet from being half of each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Facts {
    Thread(SheetFacts),
    Project(ProjectFacts),
}

impl Default for Facts {
    fn default() -> Self {
        Facts::Thread(SheetFacts::default())
    }
}

impl Facts {
    /// The project this sheet is about, either way — the thread's home, or the
    /// project itself.
    pub fn project(&self) -> &str {
        match self {
            Facts::Thread(f) => &f.project,
            Facts::Project(f) => &f.name,
        }
    }
}

/// **THE SHEET — facts on top, acts below, `<-` closes.**
///
/// The AUI's `thread_details` (read-only lines) and `thread_options` (model,
/// interrupt, compact, change summary, pin, reorder, mark closed) collapsed
/// into one surface.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SheetView {
    /// The thread's summary — its name everywhere, always. It is the sheet's
    /// header title, so the header's usual title is replaced while it is up.
    pub title: String,
    /// **THE FACTS, AS DATA RATHER THAN AS A SENTENCE.**
    ///
    /// The line reads `harness · started Sep 12 · 41 turns · last turn 2m ago`,
    /// and it is composed in the RENDERER because two of its four parts are
    /// clock-relative. Handing the shell a finished string would have put an
    /// age in the model, where it goes stale the moment the frame after it is
    /// drawn — the defect `0061` records as still open for the link banner
    /// ("`next_retry_in_ms` is a static snapshot taken at disconnect"). Every
    /// other age on this face is derived at frame time from `now`, and so is
    /// this one.
    ///
    /// Absent DELIBERATELY, per §5: the account, the working directory, and the
    /// id — *"the sheet describes your thread, not the daemon's row."*
    pub facts: Facts,
    pub rows: Vec<SheetRow>,
    pub cursor: usize,
    /// **THE ONE ROW THAT ASKS FIRST**, armed and waiting for a second press.
    ///
    /// §5 draws `remove <name> — asks first`, and this is the ask. It is a
    /// STATE ON THE SHEET rather than a mode of its own, because the grammar
    /// does not change while it is armed — the arrows still walk, `->` still
    /// activates, `<-` still closes — and a mode whose key table is identical
    /// to its parent's would put a lie in the `?` overlay.
    ///
    /// What changes is the ROW'S WORDS: an armed row says it is armed, so the
    /// second press is something he reads before he makes it. Moving the
    /// cursor, leaving, or activating any other row disarms it; nothing is
    /// sent on the first press.
    pub armed: Option<SheetAct>,
    /// Open while he is typing into a row. **What is typed survives the field
    /// being closed and the link going down** — `0060`: *"compaction
    /// instructions already typed are KEPT in the field."*
    pub field: Option<SheetField>,
}

impl SheetView {
    pub fn row(&self, i: usize) -> Option<&SheetRow> {
        self.rows.get(i)
    }
    /// The row under the cursor, which is what `->` and Enter act on.
    pub fn selected(&self) -> Option<&SheetRow> {
        self.rows.get(self.cursor)
    }
}

/// **THE INFO-NOTICE AREA — one transient message, bottom-right, over the
/// keybind bar.**
///
/// His ask, verbatim: *"Let's add a space for warning / info messages at the
/// bottom of the screen, floating on the right side of the keybind bar. If it's
/// longer than the space available, have it sit over the top of the keybind
/// messages. Have a timeout on those info messages."*
///
/// **It is a GENERAL SURFACE and not a feature of whatever needed it first.**
/// Every transient notice in the face lands here — a copy that went out, a send
/// that was refused, a paste that timed out, an undo with nothing to undo — and
/// a second per-feature message slot would be the drift this type exists to
/// make impossible. It is the `Toast` this crate used to carry, renamed to what
/// it actually is and given the one thing it was missing.
///
/// **What it is NOT: a [`Banner`].** A banner is a STANDING statement about the
/// world — the link is down, the transcript is still loading — and it must not
/// expire, because the thing it describes has not stopped being true. A notice
/// is a momentary statement about an ACT, and it must expire, because the act
/// is over. The two are different bands for that reason and neither substitutes
/// for the other.
///
/// **The timeout is data, not a timer.** [`expires_at`](Notice::expires_at) is
/// compared against the `now` the shell already hands to `frame`, so the pure
/// crate can drop an expired notice without owning a clock — which is the only
/// shape available to it, and which also makes expiry testable at an exact
/// instant rather than by sleeping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub severity: Severity,
    /// When it stops being drawn. `None` means it stands until something
    /// replaces it — reserved for a notice whose subject is still happening,
    /// like a paste that has not finished arriving.
    pub expires_at: Option<DateTime<Utc>>,
}

impl Notice {
    /// Whether this notice should still be on screen.
    ///
    /// EXPIRED AT THE INSTANT, not after it: a notice whose deadline is exactly
    /// `now` is gone. The half-open convention matters because the shell wakes
    /// the loop AT the deadline to redraw, and a closed interval would need a
    /// second wake-up to actually clear it.
    pub fn live_at(&self, now: DateTime<Utc>) -> bool {
        match self.expires_at {
            None => true,
            Some(t) => now < t,
        }
    }
}

/// A standing statement above the content: the link is down, the transcript is
/// still loading, the transcript is collapsed to stdout only.
///
/// It is a BAND-TOP row and not a full-screen page, which is the deliberate
/// deviation from `$ threads`' down page: that board is all current-state
/// meters, where nothing on screen survives the daemon. A chat is mostly
/// HISTORY, and history remains true. The rule is "never render stale as
/// current", not "render nothing" (`DEC-13`).
#[derive(Debug, Clone)]
pub struct Banner {
    pub text: String,
    pub severity: Severity,
}

/// **WHAT MIC MODE IS DOING**, as data the renderer can draw and the golden
/// harness can see.
///
/// The `Recording` variant carries no handle and no child process — those are
/// the shell's, in `tui::mic`. This is the projection of them, exactly as
/// `Composer` is the projection of a buffer rather than a widget.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MicState {
    /// In mic mode, nothing running. The band says how to start.
    #[default]
    Idle,
    /// **THE MICROPHONE IS OPEN.** The band says so unmistakably: a face that
    /// is recording and does not look like it is recording is the one state
    /// this mode must never be in.
    Recording,
    /// The capture is being stopped and read.
    Stopping,
    /// **MIC MODE COULD NOT OPEN THE DEVICE**, with the reason in his language.
    /// Standing rather than transient, because it is a state and not an event
    /// — the notice area carries the moment it happened, this carries the fact
    /// that it is still true.
    Refused(String),
}

// ────────────────────────────────────────────────────────────────────────────
//  THE SUBAGENT TREE
// ────────────────────────────────────────────────────────────────────────────

/// **WHO RUNS A SUBAGENT — the whole of the `direct`/`managed` seam, on this
/// side of the boundary.**
///
/// *"I just want to see the subagents, and their subagents."* Today every one
/// of them is a `direct` subagent: Claude Code runs it IN ITS OWN PROCESS, its
/// events reach us on the parent thread's stream with a `parent_tool_use_id`,
/// and it has no address of its own. The face is going multi-backend, and the
/// second class is named here so the day it arrives costs one function rather
/// than a rewrite of every row that carries a class.
///
/// **It is one enum, one field, one naming convention** (`direct_attribute`
/// in the shell) and nothing else. There is no adapter, no registry and no
/// trait with one implementor: the operator ruled that today's claude-code
/// handling is fine, and an abstraction over one thing would be the cost
/// without the benefit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubagentClass {
    /// **A CLAUDE CODE SUBAGENT.** In-process, narrated on the parent's
    /// stream, and — the fact the composer has to obey — with NO INBOX. See
    /// [`Composer::stand_in`].
    #[default]
    Direct,
    /// **A SUBAGENT THAT IS A THREAD OF ITS OWN**, run by the daemon, with an
    /// id `threads/{id}/send` can address.
    ///
    /// **Nothing constructs one today.** It is declared, the composer has a
    /// live arm for it, and neither is faked: a `Managed` row demonstrated by
    /// hand would be a stub pretending to work.
    Managed,
}

/// What a subagent is doing, as a WORD — never a spinner, never an asterisk.
///
/// **`Running` comes from the LEVEL ROSTER and never from unmatched edges.**
/// Measured (`0080`): at one instant the edge derivation said 13 running, the
/// CLI's roster said 1 and the operating system said 0, because `task_id` is
/// not unique and a resumed agent reuses it. `Unknown` is the honest answer
/// when a subagent has neither a terminal event nor a place in the roster, and
/// it is drawn as such rather than rounded to `done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubagentState {
    Running,
    Done,
    Failed,
    #[default]
    Unknown,
    /// **A MANAGED CHILD THE DAEMON PARKED** — no budget, no slot. The reason
    /// rides on [`TreeRow::detail`], verbatim.
    Parked,
    /// A managed child between attempts.
    Retrying,
    /// A managed child whose run the daemon recorded as `interrupted`.
    Interrupted,
    /// **A MANAGED CHILD THAT STOPPED AND ITS PARENT NEVER HEARD** — its own
    /// row reads idle, it has no `finished`, and the parent still lists it as
    /// outstanding. The bug case, made visible rather than drawn as running.
    Unreported,
}

impl SubagentState {
    /// The word the row prints.
    pub fn word(self) -> &'static str {
        match self {
            SubagentState::Running => "running",
            SubagentState::Done => "done",
            SubagentState::Failed => "failed",
            SubagentState::Unknown => "no end seen",
            SubagentState::Parked => "parked",
            SubagentState::Retrying => "retrying",
            SubagentState::Interrupted => "interrupted",
            SubagentState::Unreported => "ended, unreported",
        }
    }

    /// How the word is painted. One table, so the tree row and anything else
    /// that prints a child's state cannot disagree about its colour.
    pub fn severity(self) -> Severity {
        match self {
            SubagentState::Running => Severity::Calm,
            SubagentState::Done => Severity::Unknown,
            SubagentState::Failed | SubagentState::Unreported => Severity::Critical,
            SubagentState::Unknown
            | SubagentState::Parked
            | SubagentState::Retrying
            | SubagentState::Interrupted => Severity::Warn,
        }
    }
}

/// **WHICH ROW OF THE TREE**, keyed by identity and never by index.
///
/// *"the parent needs to be an item in that list"* — so the thread itself is
/// [`TreeId::Parent`], row 0, always. A subagent is addressed by the opaque id
/// of the `Agent` tool call that launched it: arrivals reorder the tree, and an
/// index would come to mean a different subagent under his cursor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TreeId {
    /// The thread itself.
    #[default]
    Parent,
    /// One subagent, by its launching tool_use id.
    Agent(String),
    /// **A MANAGED SUBAGENT — A THREAD OF ITS OWN**, by its thread id. Making
    /// it the subject OPENS that thread; it is never drawn in the parent's
    /// transcript, because the parent's stream does not carry it.
    Thread(String),
}

/// One row of the subagent tree — the parent thread, or one subagent.
///
/// **Every figure is an `Option` and an absent one prints NOTHING.** A number
/// on his screen that is wrong is worse than a column that is absent, because
/// he will act on it; `0` would be a claim this face cannot source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub id: TreeId,
    /// 0 for the parent, 1 for its subagents, and so on to full depth.
    pub depth: usize,
    /// Draws `└` rather than `├`.
    pub last_sibling: bool,
    /// `"Explore · scout the raw list"`, or the thread's own summary on the
    /// parent row.
    pub label: String,
    pub state: SubagentState,
    /// When the launching `Agent` call was made. The renderer subtracts it
    /// from the `now` it is handed — a duration formatted in the projection
    /// would be frozen at whatever the clock said when the event arrived.
    pub started: Option<DateTime<Utc>>,
    /// When the terminal `task_notification` landed. `None` while running.
    pub ended: Option<DateTime<Utc>>,
    /// `task_progress.total_tokens` while running; `task_notification`'s on
    /// finish. Present for 99.7% of subagents, measured.
    pub tokens: Option<u64>,
    /// `task_progress.usage.tool_uses`, else the tool calls in its trail.
    pub calls: Option<u64>,
    /// **BASH CALLS IN FLIGHT — never OS processes.** His own ruling. `ps` is
    /// measurably wrong in both directions here (`0080`): the live turn's
    /// `claude` had zero children while a subagent ran, and another had five
    /// persistent shells while running nothing.
    pub bash_in_flight: Option<u32>,
    /// Children of this row that the roster says are running.
    pub agents_running: Option<u32>,
    pub class: SubagentClass,
    /// **WHY**, for a parked or retrying managed child — the daemon's
    /// `status.detail`, verbatim. `None` prints nothing.
    pub detail: Option<String>,
    /// `↯` — a managed child spawned `wake: never`, which its parent is not
    /// waiting on and which is not counted in "waiting on".
    pub no_wake: bool,
    /// **THE STATUS HE READS, IN HIS WORDS** (TUI-P1b) — `active`,
    /// `active (+2)`, `idle`, or an error label. For a thread row this is the
    /// daemon's own `rolled.display` (R9.TS1), drawn verbatim and never
    /// computed here. `None` draws the row's [`SubagentState::word`] instead.
    pub status: Option<String>,
    /// **`N active, M idle`** over this row's DIRECT children — the daemon's
    /// `rolled.children_summary`, verbatim. `None` prints nothing.
    pub children_summary: Option<String>,
    /// **THE FOLD** (TUI-P1b): `None` when the row has no children to show,
    /// `Some(false)` collapsed (`▹`), `Some(true)` expanded (`▿`).
    pub expanded: Option<bool>,
}

impl Default for TreeRow {
    fn default() -> Self {
        Self {
            id: TreeId::Parent,
            depth: 0,
            last_sibling: true,
            label: String::new(),
            state: SubagentState::Unknown,
            started: None,
            ended: None,
            tokens: None,
            calls: None,
            bash_in_flight: None,
            agents_running: None,
            class: SubagentClass::Direct,
            detail: None,
            no_wake: false,
            status: None,
            children_summary: None,
            expanded: None,
        }
    }
}

/// **THE SUBAGENT TREE — the band below the composer, and the reason the chat
/// area is his again.**
///
/// > *I don't want to see them in the chat area, it takes up 90% of the chat
/// > area. I only want things having to do with the viewed thread in view.*
///
/// SELECTION and SUBJECT are two different things and this type keeps them
/// apart. [`AgentTree::cursor`] is the row the keyboard is on and `↑`/`↓` move
/// it; [`AgentTree::subject`] is whose chat is ABOVE, and only `→`/`enter`
/// (or a click) changes it. Browsing siblings must not re-project the
/// transcript on every keystroke.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentTree {
    /// Row 0 is the ROOT of the tree he came down from (TUI-P1b: the tree is
    /// kept all the way to the root). The OPEN thread's row is always
    /// [`TreeId::Parent`] — row 0 when he is at the root — and every other
    /// thread row is a [`TreeId::Thread`]. Subagents in LAUNCH order,
    /// depth-first under their own parent.
    ///
    /// **The parent alone is not a tree.** With no subagents this is empty,
    /// the band is not drawn, and `Region::Agents` is not a stop.
    pub rows: Vec<TreeRow>,
    /// The row the keyboard is on.
    pub cursor: TreeId,
    /// Whose chat the transcript and the composer are showing.
    pub subject: TreeId,
    /// **THE RULE LINE'S COUNT** (TUI-P1b): the open thread's DIRECT children
    /// as the daemon summarises them — `N active, M idle` — verbatim. It
    /// replaces the old `N running · M done`; `None` prints no count at all.
    pub summary: Option<String>,
}

impl AgentTree {
    /// Where `cursor` sits in `rows`, or 0 if the row it named has gone.
    pub fn cursor_at(&self) -> usize {
        self.rows
            .iter()
            .position(|r| r.id == self.cursor)
            .unwrap_or(0)
    }

    /// Where `subject` sits in `rows`, if it is still there.
    pub fn subject_at(&self) -> Option<usize> {
        self.rows.iter().position(|r| r.id == self.subject)
    }

    /// The row the subject names, if it is still there.
    pub fn subject_row(&self) -> Option<&TreeRow> {
        self.subject_at().map(|i| &self.rows[i])
    }
}

/// **NOTHING CAN BE SENT TO WHAT THE TRANSCRIPT IS SHOWING**, and the band
/// says so instead of looking live.
///
/// A `direct` subagent runs inside Claude Code and has no inbox: the face's one
/// mutation is `threads/{id}/send {thread_id, text}` and a direct subagent has no
/// thread id. His own words this round: *"We currently can't submit a message
/// into claude that way, we would have to interrupt."* So there is no send path
/// and none is invented — a green box that eats his words is the worst defect
/// this could ship, and [`Composer::editable`] is false for the first time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandIn {
    /// The subagent's label, for the first row's sentence.
    pub what: String,
    /// The parent's label, for the second row's *"← returns to …"*.
    pub back_to: String,
}

/// Everything the renderer may know.
#[derive(Debug, Clone, Default)]
pub struct ViewState {
    pub header: Header,
    pub mode: Mode,
    /// The ROOT MENU, for [`Mode::Projects`].
    pub projects: ProjectList,
    /// One project's threads, for [`Mode::List`].
    pub list: ThreadList,
    /// The transcript, for [`Mode::Thread`] and [`Mode::Activity`].
    pub blocks: Vec<Block>,
    /// **WHICH QUEUED ROW THE CURSOR IS ON**, while `Region::Queue` holds the
    /// selection.
    ///
    /// An INDEX, deliberately: the client queue changes only by his own acts
    /// (queue, unqueue, send, drop, retry) and never by an arrival, so an
    /// index cannot drift under him the way one over a live list would.
    /// Clamped by the renderer against the rows it actually draws.
    pub queue_cursor: usize,
    /// **THE SUBAGENT TREE** — the band below the composer. See [`AgentTree`].
    ///
    /// Empty when the thread has no subagents, and then the band is not drawn,
    /// `Region::Agents` is not a stop, and the thread screen is exactly what it
    /// was. `rows.len()` is what `App::stops` consults.
    pub tree: AgentTree,
    pub viewport: Viewport,
    pub composer: Composer,
    /// **MIC MODE'S STATE.** Meaningful only in [`Mode::Mic`]; `Idle`
    /// everywhere else. See [`MicState`].
    pub mic: MicState,
    /// **WHICH QUEUE ROW IS PLAYING BACK**, by its capture path.
    ///
    /// It is the PATH and not the index for the same reason
    /// `Composer::resolve_audio` addresses by path: a row can be taken out
    /// while its audio is playing, and an index would then point the `▶`
    /// highlight at whichever row slid into the slot.
    pub playing: Option<String>,
    /// **THE INFO-NOTICE AREA.** One slot, last write wins, expired by the
    /// `now` handed to `frame` rather than by anything holding a timer. See
    /// [`Notice`] for why it is one slot and not a queue.
    pub notice: Option<Notice>,
    pub banner: Option<Banner>,
    /// **HIDE EVERYTHING EXCEPT WHAT WAS SAID.** One boolean, session-scoped,
    /// and the whole of what the lane filter left behind.
    ///
    /// His ask, verbatim: *"give me a toggle for the info blocks. Until we
    /// iron out the massive wave of agent messages I just need a way to hide
    /// everything except stdout."* "Stdout" is the boxes — what he said and
    /// what came back — and it is one keystroke away in a thread and nowhere
    /// else (it is a THREAD-scoped act; see `keys::Intent::is_thread_scoped`).
    ///
    /// `0023` `what_the_tui_stores` still refuses it a byte on disk.
    pub stdout_only: bool,
    pub queue: QueueView,
    /// **THE SHEET'S STATE**, for [`Mode::Sheet`] and [`Mode::SheetField`].
    ///
    /// `None` when no sheet is up. It is a field beside `queue` rather than a
    /// variant carried inside `Mode` for the same reason `queue` is: the mode
    /// says which GRAMMAR is live, and the data the grammar acts on is the
    /// shell's to project.
    pub sheet: Option<SheetView>,
    /// **THE ARCHIVED SET**, for [`Mode::Archived`]. Empty when the door has
    /// never been opened, which is indistinguishable from an empty archive and
    /// deliberately so: the door itself says how many are behind it.
    pub archived: ArchivedView,
    /// Coalesced rows show every event they stand for, and collapsed prose
    /// shows whole. One toggle for both, because both are the same promise:
    /// nothing was dropped, it was folded.
    pub expanded: bool,
    /// **THE DRAG SELECTION.** A range over the face's LOGICAL document (see
    /// [`crate::select`]), live while he is dragging and kept painted after he
    /// lets go so he can see what went to the clipboard.
    ///
    /// **Still TRANSIENT, and `CON-4` still holds.** `CON-4` forbids a
    /// PERSISTENT per-message cursor in the transcript, reached by analogy to
    /// the audio face's playhead. This is not one: it is destroyed by the next
    /// keystroke or the next press, it addresses characters rather than
    /// messages, and nothing navigates by it. It is also the seam that
    /// playhead would ride, unchanged.
    pub selection: Option<crate::select::Selection>,
    /// **WHICH VIEW THE `?` OVERLAY WAS RAISED OVER**, and therefore whose
    /// grammar it prints.
    ///
    /// The overlay became CONTEXTUAL when the composer landed, and the reason
    /// is arithmetic rather than taste: a thread's grammar is forty bindings on
    /// its own, so printing all six modes made the overlay 90-odd rows against
    /// the 33 his pane has. `0036` measured that exact regression once already
    /// and fixed it by factoring the shared block out; the composer blew
    /// through that fix, and the remaining lever is to print the grammar he is
    /// actually standing in.
    pub help_for: Mode,
    /// How far the `?` overlay is scrolled. **`0036` deleted Help's scroll keys
    /// because they scrolled the view UNDERNEATH the overlay and never the
    /// overlay** — a binding that did the wrong thing quietly. This field is
    /// what makes them do the right thing instead, which is the honest way to
    /// bring them back.
    pub help_from: usize,
    /// **WHAT THE TERMINAL NEGOTIATED**, handed in by the shell exactly as
    /// `now` and the width are.
    ///
    /// The `?` overlay and the footer are rendered FROM this, so a key that
    /// cannot work in his terminal is never advertised — see
    /// [`crate::keys::Caps`] for the class of bug that kills.
    pub caps: crate::keys::Caps,
    /// Minutes east of UTC for DISPLAYING timestamps.
    ///
    /// The shell owns the display timezone and hands the offset in, exactly as
    /// it hands `now` in — `chrono::Local` is a banned type in this crate
    /// (`clippy.toml`) precisely because reading the machine's zone is ambient
    /// state the golden test cannot fix. An offset is data; a zone lookup is
    /// an I/O act.
    pub tz_offset_minutes: i32,
    /// **WHICH SURFACE THE LITTLE RENAME MODAL IS STANDING OVER**, for
    /// [`Mode::RenameModal`]. `None` whenever no modal is up.
    ///
    /// This is the modal's ENTIRE state, and that is the point. *"Don't take me
    /// to the settings. Just a little modal."* — so the box edits the sheet's
    /// own `rename` buffer, which is already projected into [`Self::sheet`]'s
    /// `field`, and the one thing the renderer cannot otherwise know is which
    /// body to draw BEHIND it. Holding a second copy of the buffer here would
    /// be a second definition of what he is typing, and a second place for the
    /// prefill to be got wrong.
    ///
    /// It is a `Mode` rather than a bool because the answer is genuinely the
    /// root OR a project's thread list — round five landed bare `r` on both —
    /// and the frame draws a different band for each.
    pub rename_under: Option<Mode>,
    /// **WHAT THE LITTLE MODAL CALLS ITSELF**, when it is not a rename
    /// (TUI-P1a). The same box names a NEW thread — `n` opens it, *"I want the
    /// modal to pop up to name the thread"* — and its rule has to say which
    /// door enter is, so the label is the shell's. `None` is the rename box's
    /// own words.
    pub modal_label: Option<String>,
    /// **THE ONE "DAEMON IS DOWN" PAGE** (R7.S1c). `Some` exactly while the
    /// link to threads is down; the shell decides that, this crate only draws
    /// it. When it is `Some` it REPLACES the body of whatever mode is under it
    /// — header and footer stay — so there is one page for an outage and not
    /// a down posture per mode. It leaves by itself when the link comes back:
    /// there is no key that dismisses it and no refresh key, only the restart
    /// button, which asks cauldron to start the service.
    pub down: Option<DownPage>,
}

/// **WHAT THE DOWN PAGE SAYS** — every word already resolved by the shell,
/// which is the side that knows which cauldron environment it was launched in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownPage {
    /// The service the face reads, `threads`.
    pub service: String,
    /// The cauldron environment the face resolved its port from, e.g. `prod`.
    pub env: String,
    /// The address the face is trying, `127.0.0.1:<port>`.
    pub addr: String,
    /// Where that address came from, in words (`env.toml`, the declaration…).
    pub source: String,
    /// How the service comes back by itself, or that it will not — already a
    /// sentence, because only the shell knows the environment's policy.
    pub comes_back: String,
    /// The command the restart button runs, spelled as a human would type it.
    pub restart_cmd: String,
    /// The key that is the restart button.
    pub restart_key: char,
    /// What the last press of the button produced, if it was pressed: a
    /// running note, then the command's own first line. `None` before any press.
    pub restart_note: Option<String>,
}

#[cfg(test)]
mod link_tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 14, 12, 0, 0).unwrap()
    }

    fn down(since: TimeDelta, retry: Option<TimeDelta>) -> Link {
        Link {
            url: "ws://127.0.0.1:3651/ws".into(),
            state: LinkState::Disconnected {
                reason: "the daemon closed the socket".into(),
                retry_at: retry.map(|d| t0() + d),
            },
            down_since: Some(t0() - since),
        }
    }

    /// **THE CLOCK IS ARMED BY WHAT IS ON SCREEN, AND ONLY BY THAT.**
    ///
    /// A healthy link states no duration, so it owes no frame and the loop
    /// blocks on `recv()` with no timer at all — which is the property the
    /// event loop's three existing clocks were each written to preserve.
    #[test]
    fn a_healthy_link_arms_no_clock() {
        let up = Link {
            url: String::new(),
            state: LinkState::Connected,
            down_since: None,
        };
        assert_eq!(up.refresh_in(t0()), None);
        assert_eq!(up.down_for(t0()), None);
    }

    /// **THE WAKEUPS ARE AS COARSE AS THE WORDS.** `fmt_age` spells seconds
    /// under a minute and whole minutes above it, so asking for a frame more
    /// often than that would be asking for a repaint that draws the same
    /// characters. An outage of hours costs one wakeup a minute, not sixty.
    #[test]
    fn the_refresh_is_asked_for_exactly_when_the_words_would_change() {
        // 4.25s down: the next second-boundary is 0.75s away.
        let l = down(TimeDelta::milliseconds(4_250), None);
        assert_eq!(l.refresh_in(t0()).unwrap().num_milliseconds(), 750);
        // 4m15s down: the reading is whole minutes now, so the next change is
        // 45s away and not a quarter-second.
        let l = down(TimeDelta::seconds(255), None);
        assert_eq!(l.refresh_in(t0()).unwrap().num_seconds(), 45);
        // Past a day the reading is whole hours.
        let l = down(TimeDelta::hours(25), None);
        assert_eq!(l.refresh_in(t0()).unwrap().num_minutes(), 60);
    }

    /// A live countdown moves every second, so it takes the clock over from
    /// the age whenever it is the finer of the two.
    #[test]
    fn a_live_countdown_is_the_finer_deadline_and_wins() {
        // Down 4m (a minute-grained reading) with a retry 2.4s out.
        let l = down(TimeDelta::minutes(4), Some(TimeDelta::milliseconds(2_400)));
        assert_eq!(l.refresh_in(t0()).unwrap().num_milliseconds(), 400);
    }

    /// **A DEADLINE THAT HAS PASSED IS NOT A COUNTDOWN ANY MORE**, so it stops
    /// driving the clock — otherwise a retry the transport never got round to
    /// would spin the loop on a zero-length timer.
    #[test]
    fn a_deadline_already_gone_stops_asking_for_frames() {
        let l = down(TimeDelta::minutes(4), Some(TimeDelta::seconds(-30)));
        // Only the age is left, and the age is minute-grained.
        assert_eq!(l.refresh_in(t0()).unwrap().num_seconds(), 60);
        assert!(l.refresh_in(t0()).unwrap() > TimeDelta::zero());
    }

    /// The age is measured, never remembered: one `Link`, two clocks, two
    /// answers.
    #[test]
    fn the_age_comes_off_the_clock_it_is_asked_with() {
        let l = down(TimeDelta::seconds(41), None);
        assert_eq!(l.down_for(t0()).unwrap().num_seconds(), 41);
        assert_eq!(
            l.down_for(t0() + TimeDelta::minutes(5))
                .unwrap()
                .num_seconds(),
            341
        );
        // A clock that has run backwards (NTP, a suspended laptop) reads zero
        // rather than negative: "down -3s" is not a thing to put on a screen.
        assert_eq!(
            l.down_for(t0() - TimeDelta::minutes(5))
                .unwrap()
                .num_seconds(),
            0
        );
    }
}
