//! **THE COORDINATOR.** The event loop, the repaint law, follow mode, and the
//! one call that turns daemon truth into a [`ViewState`].
//!
//! **One internal event enum, one loop, one redraw per event** — mined from
//! the old `apps/tui` (`0023` `mined_from_apps_tui` item 7), and kept because
//! it is exactly the shape the transport actor plugs into: a socket task
//! pushes [`Ev`] variants down the same channel the keyboard already uses, and
//! nothing about this loop changes.
//!
//! # What this file owns, and what it must not
//!
//! It owns the SEQUENCE: an [`Ev`] arrives, state moves, one frame is drawn.
//! It owns the things that only make sense against that sequence — the freeze
//! point that follow mode is built on, the per-view viewports, the transient
//! selection, and the [`App::force_repaint`] flag.
//!
//! It does NOT own, and no longer holds:
//!
//! * **the input device** — [`crate::input`] has the pump and the one
//!   crossterm translation;
//! * **the wire** — [`crate::reads`] has every RPC, and the module header is
//!   the read-only claim rather than a grep over this file;
//! * **the internal event vocabulary** — [`crate::event`], because four
//!   modules speak it and only one of them is this coordinator;
//! * **terminal modes** — [`crate::term`]; [`App::on_intent`] is a pure state
//!   transition that RECORDS a mouse claim and never writes an escape
//!   sequence;
//! * **geometry** — `tui_render::bands` says how the frame divides, and this
//!   file asks it rather than re-deriving a page height from the constants;
//! * **presentation** — `store`'s projection decides what a block says, and
//!   `tui-render` decides how it looks.
//!
//! That list is the answer to "has the coordinator become the place everything
//! that did not fit went". It had started to; `0037` moved four of those six
//! out and named the other two.

use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use ratatui::widgets::Paragraph;
use serde_json::Value;
use tokio::sync::mpsc;
use tui_render::compose::Payload;
use tui_render::{
    activation, Block, HitMap, Intent, LinkState, MicState, Mode, Notice, Region, RootItem,
    Selection, Severity, SheetAct, StandIn, SubagentClass, Target, TextPos, Toward, TreeId, Via,
    ViewState, Viewport,
};

use crate::acts::{self, Dir, ProjectAct, ThreadAct, ThreadRef};
use crate::aui::{self, DownCause, Reach};
use crate::event::{Clears, Ev};
use crate::index;
use crate::input::to_key;
use crate::mirror::{self, Handle};
use crate::queue_reads;
use crate::store::{self, LedgerRow, Store, FLEET_WINDOW};
use crate::term::{self, Tui};
use crate::transport::{NoteLevel, TransportEvent};

/// **ONE ROW, AND IT IS WHAT `↑` AND `^P` MEAN.** Not the wheel — see
/// [`WHEEL_ROWS`], which is a different number for a different gesture.
const ROW_STEP: usize = 1;

/// **THE WHEEL HAS NO STEP SIZE ANY MORE. IT HAS A FLOOR AND A CURVE.**
///
/// Read this before touching any of the four constants below, because the
/// obvious change to them is the one he ruled out before it was made.
///
/// # What was here, and why it is gone
///
/// A constant. First `1`, then `5`. Five was MEASURED, not chosen — his own
/// tmux server binds the wheel in copy-mode to `send-keys -X -N 5 scroll-up`,
/// so five rows a notch is what every other pane on his desk does (read it out
/// of the running server with `tmux list-keys -T copy-mode | grep -i wheel`).
/// It was a good number and it did not work, and he said so in the only terms
/// that matter:
///
/// > the scrolling is pretty bad. It's sooo slow. I don't want you to solve it
/// > with a bigger step size. We need step size 1 and also need to be able to
/// > scroll fast, proportional to how hard I scroll.
///
/// **Both halves are requirements and no constant satisfies both.** One that
/// is small enough to place a cursor on the row he means is too small to cross
/// four thousand rows; one that crosses four thousand rows cannot land on a
/// row. `n * WHEEL_ROWS` was a constant multiplier over a COUNT. What he
/// described is a function of a RATE, and the thing that was never modelled
/// anywhere in this file is TIME: [`App::flush_wheel`] knew how many notches
/// had arrived and had no idea how fast.
///
/// # The curve
///
/// ```text
///     v    = charge / WHEEL_TAU                       notches per second
///     t    = clamp01((v - WHEEL_V_FLOOR) / (WHEEL_V_FULL - WHEEL_V_FLOOR))
///     gain = 1 + (WHEEL_GAIN_MAX - 1) * t²            rows per notch
///     rows = max(1, round(n * gain))
/// ```
///
/// * **`t²` rather than `t`.** The low end is where he is placing a cursor and
///   it must stay boring: squaring keeps the gain within a few percent of 1
///   for the whole first third of the range and spends the climb at the top,
///   where he is already committed to travelling.
/// * **The floor is EXACT, not approximate.** At `v <= WHEEL_V_FLOOR` the
///   clamp makes `t` exactly zero, so `gain` is exactly `1.0` and one notch is
///   one row — the thing he asked for by name, twice, for two surfaces.
/// * **Monotone.** `t` is non-decreasing in `v`, `t²` is non-decreasing in
///   `t`, `round` is non-decreasing. Scrolling harder can never move less.
/// * **Bounded.** `gain <= WHEEL_GAIN_MAX`, always. An unbounded curve turns a
///   momentum tail into a teleport, and a face that overshoots is worse than
///   one that is slow, because he then has to hunt his way back.
///
/// # Where it is mounted
///
/// ONE mechanism, TWO mount points, and they are one because the gain is
/// applied in [`App::flush_wheel`] — which is UPSTREAM of the place
/// [`App::scroll_run`] splits a transcript's viewport from a menu's cursor.
/// The main menu, the thread list, the fleet lane, the transcript and the `?`
/// overlay all receive the same row count from the same curve. There is no
/// second implementation to drift, and no second constant to raise.
///
/// The arrow keys are NOT on it: they call `scroll_run(_, ROW_STEP)` directly
/// and still mean one row, which is what a key means.
const WHEEL_TAU: Duration = Duration::from_millis(150);

/// **THE RATE AT OR BELOW WHICH A NOTCH IS EXACTLY ONE ROW**, in notches per
/// second. See [`WHEEL_TAU`] for the curve this is the foot of.
///
/// A single notch out of stillness charges the meter to `1`, which reads as
/// `1 / 0.150 = 6.7` notches a second — under this floor with room to spare,
/// so the deliberate notch is one row and is not near a boundary. A steady
/// hand-walked scroll of ten notches a second settles at about `13` and buys
/// a gain of 1.05, which still rounds to one row a notch.
///
/// **Raise this and deliberate scrolling gets coarser; lower it and a slow
/// walk starts accelerating under him.** It is the one number to move if the
/// floor ever feels like it is not holding.
const WHEEL_V_FLOOR: f32 = 8.0;

/// **THE RATE AT WHICH THE GAIN IS FULLY OPEN**, in notches per second.
///
/// A hard flick of the trackpad arrives as tens of notches inside one or two
/// `read()`s — several hundred notches a second by this meter — so the flick
/// itself is well past this and is at the ceiling. macOS's momentum tail then
/// walks the rate back DOWN through this range as it thins, which is where the
/// deceleration comes from; see [`WheelMeter`].
///
/// **Lower it and the face reaches full speed on a gentler gesture.**
const WHEEL_V_FULL: f32 = 110.0;

/// **THE MOST ROWS ONE NOTCH MAY EVER MOVE.** The bound, and the only reason
/// a momentum tail cannot teleport him.
///
/// Twenty is sized off the transcript it has to cross: his deepest is around
/// four thousand rows, a hard flick plus its tail is a couple of hundred
/// notches, and 200 × 20 is the same order of magnitude — so ONE hard flick
/// crosses the thing, which is the complaint. A single hard batch of forty
/// notches moves 800 rows, well short of a teleport on that transcript.
///
/// **This is the knob for "too fast" or "still too slow", and it is the only
/// one that changes the top end without touching the floor.**
const WHEEL_GAIN_MAX: f32 = 20.0;

/// **HOW HARD HE IS SCROLLING, AS ONE FLOAT.**
///
/// A LEAKY INTEGRATOR, and the shape was chosen for what it does at the END of
/// a gesture rather than at the start.
///
/// Every flushed run adds its notch count to `charge`; `charge` decays
/// continuously as `exp(-dt / WHEEL_TAU)`. So `charge` is "how many notches
/// worth of gesture is still in flight" and `charge / WHEEL_TAU` is a rate.
///
/// # Why not `n / dt`
///
/// The obvious estimator divides a batch by the time since the last batch, and
/// it is unusable here: the gap between two `read()`s is a scheduling artefact,
/// so a batch that lands one millisecond after the last one reports a
/// preposterous rate, and the FIRST batch after a pause divides by the pause.
/// The integrator never divides by an observed interval at all — it only ever
/// divides by its own fixed `WHEEL_TAU` — so no scheduling hiccup can spike it
/// and no pause can make a single notch look fast.
///
/// # macOS momentum, which is the case that decides whether this feels right
///
/// The trap: macOS keeps delivering wheel reports for a second or two AFTER
/// his fingers leave the glass, at a DECAYING rate. An estimator that held a
/// peak — a max over a window, a rise-fast/fall-slow filter, anything with a
/// hold — would read that tail as continued hard scrolling and keep the face
/// at full gain long after he had stopped touching the device.
///
/// **This one cannot, and the reason is structural rather than tuned: the
/// charge has exactly one input, which is observed notches, and one behaviour
/// with no input, which is to decay.** There is no peak anywhere in it to
/// hold. As the tail thins, the charge falls, the gain falls with it, and the
/// two compound — fewer notches, each worth fewer rows. And the face moves on
/// a report or not at all, so when the reports stop it stops in the same
/// frame; nothing here runs a physics simulation he would have to catch.
///
/// # The clock is an argument
///
/// `now` is passed in rather than read inside, so the curve is testable
/// without a sleep. `App::on_input` reads `Instant::now()` ONCE per terminal
/// read and hands the same instant to every flush that batch produces — which
/// is also correct rather than merely convenient: those notches really did all
/// arrive in the same read, and a reversal mid-batch must not be charged as
/// though time had passed.
#[derive(Debug, Clone, Copy, Default)]
struct WheelMeter {
    /// Decayed notch count. `0.0` at rest.
    charge: f32,
    /// When the charge was last brought up to date. `None` before the first
    /// notch of the session, which reads as "infinitely long ago".
    last: Option<std::time::Instant>,
}

impl WheelMeter {
    /// Charge the meter with a run of `n` notches that arrived at `now`, and
    /// say how many ROWS that run is worth.
    ///
    /// Never returns `0` for a non-empty run: a gesture that produced a notch
    /// must move the face, or the floor is not a floor.
    fn rows(&mut self, n: usize, now: std::time::Instant) -> usize {
        if n == 0 {
            return 0;
        }
        let tau = WHEEL_TAU.as_secs_f32();
        // `saturating_duration_since`, because `Instant` arithmetic panics on
        // a reversal and a test that hands two instants out of order should
        // get a stale reading, not an abort.
        let decayed = match self.last {
            Some(t) => {
                let dt = now.saturating_duration_since(t).as_secs_f32();
                self.charge * (-dt / tau).exp()
            }
            None => 0.0,
        };
        self.last = Some(now);
        self.charge = decayed + n as f32;
        let rows = n as f32 * self.gain();
        // `max(1)` is the floor restated where it cannot be rounded away.
        (rows.round() as usize).max(1)
    }

    /// Rows per notch at the charge the meter is holding right now. Exactly
    /// `1.0` at or below [`WHEEL_V_FLOOR`], exactly [`WHEEL_GAIN_MAX`] at or
    /// above [`WHEEL_V_FULL`], and `t²` between them.
    fn gain(&self) -> f32 {
        let v = self.charge / WHEEL_TAU.as_secs_f32();
        let t = ((v - WHEEL_V_FLOOR) / (WHEEL_V_FULL - WHEEL_V_FLOOR)).clamp(0.0, 1.0);
        1.0 + (WHEEL_GAIN_MAX - 1.0) * t * t
    }
}

/// How often the band scrolls while he holds the pointer at its edge with the
/// button down.
///
/// It is a TICK and not a motion event because a stationary pointer produces
/// no reports at all — the terminal reports cell MOTION, so a hand held still
/// past the top of the transcript is silent, and without a clock the selection
/// would stop at the first row and look stuck. 50ms is twenty rows a second,
/// which is fast enough to sweep a long transcript and slow enough to stop
/// where he means to.
///
/// **A tick that changes nothing costs nothing on screen**: ratatui writes
/// only cells that differ, so once the band is against its end the redraw
/// emits no bytes at all.
const AUTOSCROLL_TICK: Duration = Duration::from_millis(50);

/// **HOW LONG A NOTICE STANDS, and it is a judgment recorded as one constant
/// pair rather than as a number sprinkled over thirty call sites.**
///
/// *"Have a timeout on those info messages."* — his words, and the only thing
/// he specified about it. So the value is ours to justify:
///
/// * A CONFIRMATION (`sent`, `draft restored`, `pasted 4,000 characters`) has
///   one job — tell him the thing he just asked for happened — and he is
///   looking at the screen when it lands, because he pressed the key. Five
///   seconds is several times longer than it takes to read six words.
/// * A FAILURE or a WARNING (`not sent: …`, `paste failed — timed out`) is the
///   opposite case: it arrives for something he did NOT ask for, possibly while
///   he is reading the transcript or away from the pane. It gets twelve.
///
/// **Neither is a substitute for a [`tui_render::Banner`]**, which is the band
/// for a fact that has not stopped being true and which does not expire at all.
/// If something needs to stay on screen, it is not a notice.
const NOTICE_TTL: Duration = Duration::from_secs(5);
const NOTICE_TTL_LOUD: Duration = Duration::from_secs(12);

/// Which of the two a severity gets. See [`NOTICE_TTL`] for the argument.
fn ttl(severity: Severity) -> Duration {
    match severity {
        Severity::Warn | Severity::Critical => NOTICE_TTL_LOUD,
        Severity::Calm | Severity::Ok | Severity::Unknown => NOTICE_TTL,
    }
}

/// **THE FREEZE POINT — and it is CONTENT, not a count.**
///
/// It was `Option<usize>`: the number of blocks the view held when he scrolled
/// away. `new_since` is `blocks.len() - count`, and the renderer lays out only
/// `blocks[..len - new_since]` — so an ARRIVAL, which appends, cannot move what
/// he is reading. That much was built and works.
///
/// **What was never built is the case that actually happens to him**, and
/// `0097` measured it on a painted frame: he scrolls up WHILE a reply is
/// streaming. That reply is the LAST block of the held prefix, it is BELOW the
/// freeze point, so it is laid out — and it keeps growing. `blocks.len()` never
/// changes, `new_since` stays `0`, `from_bottom` stays exactly where he put it,
/// and thirty-five of forty screen rows scroll away under him anyway.
///
/// **`from_bottom` is a distance from the bottom of the laid-out content, so it
/// is only a place while that bottom holds still.** Holding the block COUNT
/// still is not the same thing as holding the content still, and the difference
/// is invisible to every assertion phrased in terms of `from_bottom` — which is
/// exactly why `an_arrival_while_scrolled_up_never_yanks_the_viewport` was
/// green over this the whole time. A frame is the only honest witness here.
///
/// So the freeze point carries the tail block's CONTENT as well as the count,
/// and [`App::project`] paints that copy rather than the live one for as long
/// as he is away from the bottom. The two live in one `Option` deliberately:
/// every `= None` in this file is a thaw, and a pin that could outlive its
/// count would pin a block that no longer exists.
#[derive(Debug, Clone)]
struct Frozen {
    /// How many blocks the view held at the freeze. `new_since` is derived
    /// from this rather than counted per event — one transport event does not
    /// make one block, so a counter would promise rows that were never coming.
    count: usize,
    /// **THE LAST HELD BLOCK, AS IT LOOKED WHEN HE SCROLLED AWAY.** `None`
    /// until the first projection after the freeze captures it; the transcript
    /// can be empty, and a pin on nothing is not a special case worth a branch.
    ///
    /// Only the LAST held block needs pinning, and that is a property of the
    /// assembly rather than a guess: `store::place_all` updates a block in
    /// place ONLY on its `i + 1 == out.len()` arm. A block that grows anywhere
    /// else is blanked at its old slot and re-pushed at the end — which is a
    /// different disturbance, is NOT fixed by this, and is named in `0097`'s
    /// report as its own defect.
    tail: Option<Block>,
    /// **THE LEDGER SEQ THE VIEW WAS FROZEN AT**, and the half of the freeze
    /// that a block COUNT could never be.
    ///
    /// A live row does not APPEND, it RELOCATES — the projection holes its old
    /// slot and pushes a new one at the end, so `blocks.len()` is unchanged.
    /// A count-derived `new_since` therefore reads ZERO while every row below
    /// the relocation slides up one, and his text jumps under `↓ 0 new`. The
    /// seq is immune to that because it addresses the LEDGER rather than the
    /// projection: a row at or below it is his, it stays where it was, and
    /// arrivals are the rows past it.
    ///
    /// `None` on the FLEET lane, deliberately: its seqs are per-thread, so
    /// there is no single ordering for them to be compared against, and
    /// nothing in it relocates.
    seq: Option<u64>,
}

impl Frozen {
    fn at(count: usize, seq: Option<u64>) -> Self {
        Self {
            count,
            tail: None,
            seq,
        }
    }
}

/// **HOW THE SERVICE COMES BACK — OR THAT IT WILL NOT** — for the down page,
/// from the environment's `auto_restart` as read at startup (R7.S1c). A page
/// that promised a relaunch in an environment that never relaunches would be
/// the exact stale-promise class `DEC-13` exists to prevent.
pub fn comes_back(env: &str, auto_restart: Option<bool>) -> String {
    match auto_restart {
        Some(true) => format!(
            "auto_restart is on in `{env}`: a cauldron holding it relaunches threads, and this \
             page leaves by itself when threads answers again."
        ),
        Some(false) => format!(
            "auto_restart is OFF in `{env}`: nothing will relaunch threads by itself. Press the \
             button."
        ),
        None => "whether this environment relaunches threads is not known here. This page \
                 leaves by itself when threads answers again."
            .to_string(),
    }
}

/// The down page's restart button (R7.S1c).
pub const DOWN_RESTART_KEY: char = 'r';

pub struct App {
    vs: ViewState,
    /// The session model. See `store.rs` for what it refuses to hold.
    store: Store,
    handle: Handle,
    tx: mpsc::Sender<Ev>,
    /// **THE DOWN PAGE'S RESTART BUTTON** (R7.S1c): what the last press
    /// produced, and whether one is still running (a second press while one is
    /// in flight is ignored rather than queued, so a held key cannot fork a
    /// row of `cauldron start`s).
    restart_note: Option<String>,
    restarting: bool,
    /// **ONE VIEWPORT PER VIEW.** Sharing one would mean his place in a long
    /// transcript is destroyed by a glance at the fleet, which is the specific
    /// way a reader loses a thread.
    projects_vp: Viewport,
    /// **THE `see all` PAGE'S OWN VIEWPORT**, for the reason it has its own
    /// cursor: `←` gives the root its page back, not row zero of it.
    seeall_vp: Viewport,
    list_vp: Viewport,
    thread_vp: Viewport,
    activity_vp: Viewport,
    /// **THE FOLLOW-MODE FREEZE POINT.** How many blocks the view held at the
    /// moment he scrolled away from the bottom.
    ///
    /// `new_since` is DERIVED from this — `blocks.len() - frozen` — rather than
    /// counted per arriving event, and that is a correction: one transport
    /// event does not make one block (the lane folds runs and coalesces tasks),
    /// so a counter would promise a number of new rows that was never going to
    /// be there. This way the chip's number and the rows the renderer holds
    /// back are the same fact read twice.
    thread_frozen: Option<Frozen>,
    activity_frozen: Option<Frozen>,
    /// **WHICH ROWS HE HAS OPENED**, keyed by the ledger identity of the event
    /// each row came from and NOT by its index — see
    /// [`tui_render::model::RowKey`] for why an index could not work.
    ///
    /// This is `IS-31`'s fix and the answer to his *"clicking on a message
    /// seems to change the number of events being displayed beneath it"*: the
    /// click used to flip ONE GLOBAL BOOL, which turned folding off for the
    /// entire transcript, so every folded run everywhere un-coalesced at once
    /// (`0047` D1). A click now opens the row he clicked and nothing else.
    ///
    /// **`vs.expanded` survives beside it**, unchanged, as the whole-transcript
    /// raw escape `x` reaches — which is what keeps `DEC-19`'s "nothing is
    /// dropped" promise true under a retraction that removed rows from the
    /// default face.
    ///
    /// Session-scoped, and DELIBERATELY not cleared when a thread closes: he
    /// opens a thread, goes to the fleet, comes back, and the row he was
    /// reading is still open. The set is bounded by what he has clicked.
    opened: store::Opened,
    /// **HOW MANY TIMES THE WHOLE MODEL HAS BEEN RE-PROJECTED**, and it is not
    /// a statistic — it is the only observable the scroll law has.
    ///
    /// *"Scrolling fast should actually scroll fast. It just scrolls slowly for
    /// a long time."* The cause was that every wheel notch re-projected the
    /// tail, so a flick of forty notches did forty full projections of a
    /// four-thousand-row transcript, one frame apart, LONG after his hand had
    /// stopped. The fix is to do the arithmetic once per batch — and "once"
    /// is a claim a test can only check by counting.
    ///
    /// A `u64` bump on a path that already walks thousands of rows, so it
    /// cannot be the thing that makes anything slow.
    projections: u64,
    /// **A TRANSPORT BATCH IS OPEN** (`begin_batch` .. `end_batch`): a transport
    /// fact marks the projection owed instead of re-projecting the whole tail
    /// once per fact. See [`App::begin_batch`].
    batching: bool,
    /// A transport fact arrived inside a batch and the view has not been
    /// re-projected since. Paid by the next non-transport event or `end_batch`.
    projection_owed: bool,
    /// **THE LAST TRANSCRIPT THE FLIGHT RECORDER SAW**, as `(thread, block
    /// key → one-line summary)`, so every projection can log which blocks
    /// appeared, changed and DISAPPEARED. Only filled while `diag` is on.
    diag_view: (String, std::collections::BTreeMap<String, String>),
    /// **HOW HARD HE IS SCROLLING.** One meter for the whole face — see
    /// [`WheelMeter`] and [`WHEEL_TAU`].
    ///
    /// **Deliberately NOT per view**, unlike the viewports beside it. A
    /// viewport is a PLACE and losing it costs him his page; velocity is a
    /// property of the hand, it is the same hand on every surface, and it is
    /// stale after 150ms anyway. A per-view meter would mean a flick that
    /// crossed from the transcript to the fleet lane started again from rest,
    /// which is the one thing his gesture does not do.
    wheel: WheelMeter,
    /// Which row of the ROOT MENU `→` opens.
    project_cursor: usize,
    /// **WHICH ROW OF A BAND'S OWN `see all` PAGE `→` OPENS** — and it is a
    /// SECOND cursor, deliberately, never `project_cursor`.
    ///
    /// > Going left from there goes back to the main menu.
    ///
    /// `←` off the page must land the ROOT cursor exactly where it was — on the
    /// band header or the `see all` foot he pressed `→` on — and a shared
    /// cursor would land it on row 0 or row `BAND_ROWS` of a surface he never
    /// moved through. This is the cursor-per-depth half of `DEC-24`, which the
    /// AUI spells `ScopeStack { stack, cursors }` and which the thread tests
    /// already assert one level down.
    ///
    /// **IT REPLACED `band_expanded: [bool; 3]`.** There is no expansion state
    /// on the shell any more: a band is never grown in place, so the only thing
    /// to remember about a band's page is where he is standing on it.
    seeall_cursor: usize,
    /// **THE THREAD THE ROOT CURSOR IS STUCK TO**, while a pin is being walked
    /// up or down the band.
    ///
    /// This is the whole of *"the cursor should go with it, so we can click
    /// that multiple times to move it to a new location"*, and it is an ID
    /// rather than an index because an index is exactly what a reorder
    /// invalidates. A naive implementation keeps the cursor on the same ROW
    /// NUMBER and therefore lands on whichever thread the move displaced —
    /// so the second tap moves the wrong thread.
    ///
    /// It is STICKY on purpose. The order does not change when the key is
    /// pressed; it changes when the AUI has accepted the mutation, rewritten
    /// `.mind/threads.json` and this face has re-read it, which is several
    /// events later. An anchor that cleared itself on the next projection
    /// would be gone before the row ever moved. It is cleared by the next
    /// deliberate cursor movement — see [`App::release_pin_follow`].
    pin_follow: Option<String>,
    /// **THE PROJECT A REORDER IS WAITING TO FOLLOW**, by NAME — the registry
    /// has no other identity for a project and the name is what `move_before`
    /// names a neighbour by. [`App::pin_follow`]'s counterpart, and separate
    /// from it because the two anchors resolve to two different `RootItem`s
    /// and one scalar could not say which.
    project_follow: Option<String>,
    /// `$HOME`, resolved once by the shell. The `Store` holds a copy for
    /// shortening a cwd; this one is for the ONE file this face writes
    /// (`index::apply_registry`). A `dirs::home_dir()` call down here would be
    /// a second answer to "which machine is this", and the one that a test
    /// could not point at a tempdir.
    home: String,
    /// **THE ID `n` MINTED, WAITING FOR THE DAEMON TO SAY THE THREAD EXISTS.**
    ///
    /// `threads/create` takes the id as a PARAMETER and adopts it verbatim, so
    /// the face knows the address before the reply comes back and does not have
    /// to read one. What it is still waiting for is the ACK: the thread is
    /// opened on `Clears::ThreadCreated` and never on the send, because a view
    /// opened over a refused create is a screen that pretends to be a thread.
    pending_new_thread: Option<String>,
    /// **THE LITTLE NAME BOX, WHEN IT IS NOT THE SHEET'S RENAME** (TUI-P1a) —
    /// a NEW thread's name (`n`), or a rename of a thread the sheet cannot
    /// open (one this face created, which has no index header). Drawn by
    /// [`Mode::RenameModal`], the same box; `None` whenever it is not up.
    naming: Option<Naming>,
    /// **THE LAST WIRE THIS FACE SENT** — the bytes the newest act left
    /// with, kept because the send itself is spawned and a test (or a
    /// diagnostic) has no other window onto them. One value, so it costs
    /// nothing to keep in production.
    last_wire: Option<acts::Wire>,
    /// Which row of the open project's thread list `→` opens.
    list_cursor: usize,
    /// **WHAT WAS UNDER EVERY COORDINATE OF THE LAST FRAME.** Written only by
    /// [`App::redraw`], from the same pass that drew it. A click is resolved
    /// against THIS rather than against a geometry the shell re-derives, which
    /// is `0023` `defect_classes` CLASS 3 applied to the mouse: a hit map built
    /// a second time would be a second derivation, and a wrong one does not
    /// look wrong.
    hits: HitMap,
    /// **WHAT AN OVERLAY WAS RAISED OVER, AND NOTHING ELSE.** An overlay is a
    /// way of reading the view under it, so it owes that view back.
    ///
    /// It used to do a SECOND job — *"where a body mode came from"* — and the
    /// two jobs silently fought: every body-mode door wrote it and every
    /// overlay door overwrote it on the way in, so `open_band_thread`'s own
    /// write was dead code under a doc comment that argued for it (`0073`).
    /// The second job is now [`App::origin`]'s, which is a STACK and can
    /// express what one scalar could not. **Written by the four overlay doors
    /// only** — `Intent::Help`, `Intent::QueueOverlay` and the two
    /// `open_*_sheet` (there were four when the filter overlay existed) — and
    /// read by [`App::view`] and by `Intent::Back`.
    under: Mode,
    /// **THE BODY BACK-STACK — where each body mode was entered FROM**,
    /// outermost first. `esc` POPS it; it does not compute a depth.
    ///
    /// This is the AUI's `ScopeStack` (`apps/aui/src/scope.rs`), which is a
    /// real push/pop and is why the AUI has never had the bug this replaces.
    /// The TUI collapsed the AUI's three root scopes onto one scrolling
    /// surface (`DEC-28`) and kept a back-transition that still assumed three
    /// scopes' worth of depth, so `esc` out of a thread opened from a root
    /// band delivered him into a project he never entered (`0073`, frames
    /// A3/C3). A one-deep scalar cannot say which of two doors he came
    /// through; a stack says it by construction, and every future door — a
    /// search result, the archived room, a click from the event lane — costs
    /// nothing to add.
    ///
    /// **The root is the FLOOR.** An empty stack pops to `Mode::Projects`,
    /// which is the honest answer for a thread opened by something that left
    /// no origin (`App::open`, a reload, a test).
    origin: Vec<Mode>,
    /// **THE PARENTS HE CAME DOWN THROUGH** — one entry per managed child he
    /// opened from its parent's tree: the parent's thread id, and the tree row
    /// to stand on when `←` brings him back. `origin` holds MODES, and a thread
    /// opened from a thread is the same mode with a different id, so the way
    /// back to a parent lives here. Emptied whenever a thread is entered by any
    /// other door.
    parents: Vec<(String, TreeId)>,
    /// **THE SECOND SOCKET.** `None` when no 9010 client was started at all —
    /// which is a legal, drawn state and not an error: the sheet's AUI-owned
    /// rows then carry their reason in words, exactly as they do when the
    /// listener is unbound. Tests construct an `App` without one for precisely
    /// that reason.
    aui: Option<aui::Handle>,
    /// **THE OPEN SHEET.** `None` when none is up.
    ///
    /// It holds the typed buffers rather than letting them live in the view,
    /// which is what makes `0060`'s rule true: *"compaction instructions
    /// already typed are KEPT in the field."* The projection is rebuilt on
    /// every intent; a buffer living inside it would be re-created empty each
    /// time, and the words he typed would vanish on whatever happened next.
    sheet: Option<SheetState>,
    /// **THE NAME HE IS TYPING INTO `+ new project`**, when he is. `None` is
    /// the resting row. It lives here rather than on the projection for the
    /// sheet field's reason: the projection is rebuilt on every intent.
    new_project: Option<tui_render::compose::Composer>,
    /// Where the cursor stands in the archived room.
    archived_cursor: usize,
    /// **WHICH ROW OF THE SUBAGENT TREE THE KEYBOARD IS ON**, keyed by
    /// identity and never by index: arrivals reorder the tree, and an index
    /// would come to mean a different subagent under his cursor.
    tree_cursor: TreeId,
    /// **WHOSE CHAT IS ABOVE** — the SUBJECT, which is a different thing from
    /// the cursor and is deliberately kept apart from it.
    ///
    /// `↑`/`↓` move the cursor; only `→`/`enter` move this. Re-projecting the
    /// transcript on every keystroke would make the tree impossible to browse,
    /// and it would mean he could not pass the parent row without the chat
    /// flickering back to it.
    tree_subject: TreeId,
    /// The aggregation this projection built, held so `Drill` and the composer
    /// can ask about the subject without re-deriving it.
    agents: Vec<crate::agents::SubagentRow>,
    /// **WHAT THE TREE KEEPS ACROSS ENTERING A CHILD** (TUI-P1b): the folds
    /// he opened, and each thread's rows as last seen, so the tree is kept to
    /// the root and never rebuilt from scratch. See [`store::TreeCtx`].
    tree_ctx: store::TreeCtx,
    /// **WHEN THE LAST `Invalidate` RE-READ HAPPENED.** One re-read per
    /// `INVALIDATE_DEBOUNCE`, because a single `MovePin` ripples six nodes and
    /// six re-reads of thirteen files is five re-reads of no new information.
    last_invalidate: Option<std::time::Instant>,
    /// **A FRAME THAT WAS COALESCED AWAY AND IS STILL OWED A RE-READ** — the
    /// node kind of the last `Invalidate` the debounce swallowed, or `None`
    /// when the burst is settled and nothing is outstanding.
    ///
    /// **THIS FIELD IS THE TRAILING EDGE**, and it is what turns
    /// [`App::on_invalidate`] from a debounce that DROPS into one that
    /// COALESCES. The frames in a burst are not interchangeable — see
    /// [`Ev::IndexSettled`] for the exact producer ordering that makes the
    /// LAST frame the one carrying the new name — so a window that keeps only
    /// the first and forgets the rest is losing data, not saving work.
    ///
    /// It holds the node kind rather than a bool purely so the log line the
    /// trailing read writes can name what it was paying for.
    invalidate_owed: Option<String>,
    /// **THE RECORDER, WHILE ONE IS RUNNING.** A child process holding the
    /// microphone; `None` whenever it is not. See `crate::mic`.
    recording: Option<crate::mic::Recording>,
    /// **THE HOLD-TO-SPEAK DETECTOR** — repeat-counting with a learned idle
    /// gap, because there is no key-release event in his terminal. It is fed
    /// the `MicRecord` ACT rather than a key, so both spellings of that act
    /// carry the gesture. See `crate::mic::Hold`.
    hold: crate::mic::Hold,
    /// **THE DETACHED PLAYER.** Retained rather than waited on, because
    /// `afplay` blocks for the whole clip; holding the handle is also what
    /// lets a second press stop it.
    ///
    /// Shared with the waiter thread that reports the clip finishing, which is
    /// the only reason it is behind a lock: both ends must be able to reach the
    /// child, one to reap it and one to kill it.
    playing: Option<std::sync::Arc<std::sync::Mutex<std::process::Child>>>,
    /// **THE REPAINT LAW** (`0023` `defect_classes` CLASS 2). ratatui writes
    /// only the cells that CHANGED — measured on a raw pty in `0018`, a cursor
    /// move touches three rows out of twenty — so once something else has
    /// moved what is on screen there is no later frame at which the diff
    /// catches up: the model agrees with the stale cells, so they are never
    /// rewritten. This flag is the only route back.
    ///
    /// **Every set-point is a call to [`App::force_repaint`], so
    /// `grep -n 'force_repaint(' apps/tui/src` enumerates the whole set in one
    /// command.** Today that is five: startup, resize, the mouse changing hands
    /// in EITHER direction, the link changing hands, and the user-reachable
    /// `^L`. Not a set-point, with its reason: suspend/resume, because raw mode
    /// clears `ISIG` and Ctrl-Z never becomes a SIGTSTP (see `term.rs`); focus
    /// regain, because `?1004h` is not in `term::CLAIMS` and an unclaimed mode
    /// reports nothing.
    needs_full_repaint: bool,
    /// The body height from the LAST draw, for paging. Written only by
    /// [`App::redraw`], which is the one call site that reads the frame area.
    last_rows: usize,
    /// The frame size from the LAST draw. Written only by [`App::redraw`],
    /// which is still the one call site that reads the frame area — this is
    /// that one reading, kept, not a second one.
    last_size: (u16, u16),
    /// **THE DRAG IN PROGRESS.** `None` between gestures.
    ///
    /// The anchor is a [`TextPos`] and NOT a screen cell, which is what lets
    /// autoscroll carry the content it points at right off the top of the
    /// screen without the selection moving. It is resolved ONCE, at
    /// [`Ev::Press`], against the frame that was on screen then.
    drag: Option<Drag>,
    /// **A CLIPBOARD WRITE THE LOOP STILL OWES THE TERMINAL.**
    ///
    /// The same shape `mouse_claim_owed` had and for the same reason: a copy
    /// writes REAL `OSC 52` bytes at stdout, and with the write inline
    /// `cargo test` would spray escape sequences into whatever terminal was
    /// running it — and, worse, would put whatever a test fixture said onto
    /// the developer's actual clipboard. So the gesture handler decides WHAT
    /// to copy, [`run`] performs it, and the decision is a pure state
    /// transition a test can read.
    copy_owed: Option<String>,
    /// **ONE COMPOSER PER THREAD, KEPT WHILE THE SESSION LIVES.**
    ///
    /// Stepping out to the thread list and back must not lose what he was
    /// halfway through typing — and `esc` is the ONLY way back out of a thread
    /// now that the composer owns `←`, so it is a gesture he will make with a
    /// draft in the box. This is memory, not disk: the disk journal
    /// ([`crate::draft`]) exists for the case this map cannot survive, which is
    /// a crash.
    parked: std::collections::HashMap<String, tui_render::compose::Composer>,
    /// When the composer was last edited, for the two idle deadlines: closing
    /// an edit RUN at 400ms and writing the draft journal at 750ms.
    last_edit: Option<std::time::Instant>,
    /// Whether the journal owes a write. Separate from [`App::last_edit`] so a
    /// write that has already happened does not happen again every tick.
    draft_dirty: bool,
    /// The thread a `threads/{id}/send` is in flight for. **The double-send guard**:
    /// the round trip is long enough for a second `enter` to fit inside it, and
    /// a second send is his words arriving twice.
    sending: Option<String>,
    quit: bool,
}

/// How long the composer must sit still before an edit RUN closes, so the next
/// keystroke starts a fresh undo step.
///
/// A judgment call, recorded as one, and it is one constant. Below it, typing a
/// paragraph is one `^Z`; above it, a pause for thought is a new one.
const RUN_IDLE: Duration = Duration::from_millis(400);

/// How long the composer must sit still before the draft journal is written.
///
/// Longer than [`RUN_IDLE`] on purpose: a run boundary is free and a disk write
/// is not, and the thing the journal protects against — a crash — does not care
/// about a third of a second.
const DRAFT_IDLE: Duration = Duration::from_millis(750);

/// One gesture, from press to release.
#[derive(Debug, Clone, Copy)]
struct Drag {
    /// Where it started, in the document. Immovable.
    anchor: TextPos,
    /// The screen row the press landed on — the only thing the anchor CELL is
    /// still needed for, and only to decide which way a head that landed on
    /// blank space should look for text.
    from_row: u16,
    /// Where the pointer is now, in screen cells. Re-read on every autoscroll
    /// tick against the frame that is up at that moment, which is precisely
    /// how holding still at the edge sweeps through content.
    cell: (u16, u16),
    /// Which way the band is scrolling under him, if it is.
    autoscroll: Option<bool>,
}

/// **THE SHEET'S SHELL-SIDE STATE** — what survives a re-projection.
///
/// The `SheetView` handed to the renderer is rebuilt on every intent, so
/// anything that must outlive one keystroke lives here instead: which thread
/// the sheet is about, where the cursor is, and — the load-bearing one — the
/// two typed buffers.
#[derive(Debug)]
struct SheetState {
    subject: Subject,
    cursor: usize,
    /// Which field is OPEN, if one is. `None` is the menu.
    editing: Option<SheetAct>,
    /// **KEPT ACROSS EVERYTHING.** Closing the field keeps it, the link going
    /// down keeps it, and re-projecting keeps it. It is cleared only by an act
    /// the far end ACCEPTED — the composer's own rule, for the composer's own
    /// reason: a box cleared optimistically is a box that eats his words when
    /// the socket was down.
    compact: tui_render::compose::Composer,
    rename: tui_render::compose::Composer,
    /// **WHAT THIS FACE LAST SET THE PROJECT'S MODEL TO**, in this sheet.
    ///
    /// `None` is "we have not set one", which draws `model ?`. A project's
    /// override lives in the AUI's settings file and this face has no honest
    /// read of it — and unlike a thread, a project has no daemon value to fall
    /// back on. So the row states what it knows, which is what it did.
    chosen: Option<Option<aui_wire::Model>>,
    /// **THE ROW THAT HAS ASKED AND IS WAITING FOR THE SECOND PRESS.** §5's
    /// `asks first`, and the only user of it is `remove` — the one act on this
    /// face that takes something of his off his root. Since round five it is
    /// also what `c` reaches from the keyboard — see
    /// [`App::deregister_project`], which presses THIS row rather than
    /// growing a second confirmation grammar beside it.
    armed: Option<SheetAct>,
}

/// **THE NAME BOX'S STATE** (TUI-P1a): what it names and what is typed.
#[derive(Debug)]
struct Naming {
    target: NameTarget,
    buf: tui_render::compose::Composer,
}

/// **WHAT THE NAME BOX NAMES.**
#[derive(Debug, Clone, PartialEq, Eq)]
enum NameTarget {
    /// A thread that does not exist yet — `n`. `place` is the project's name
    /// (or the bucket's) for the box's own words; `cwd` is where it runs.
    New { cwd: std::path::PathBuf, place: String },
    /// A thread that exists — `r` on a thread the sheet cannot hold.
    Thread { id: String },
}

/// **WHAT A SHEET IS ABOUT.** §5 draws one shape over two subjects.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Subject {
    Thread {
        /// The `~/minds/<name>` basename. Half of the key every AUI act is
        /// addressed by, and on no header — the row it was opened from is what
        /// carried it (`ThreadRow::project`, 0053).
        project: String,
        id: String,
    },
    Project {
        name: String,
    },
}

impl SheetState {
    fn thread(project: &str, id: &str, summary: &str) -> Self {
        Self {
            subject: Subject::Thread {
                project: project.to_string(),
                id: id.to_string(),
            },
            cursor: 0,
            editing: None,
            compact: tui_render::compose::Composer::new(),
            // **THE RENAME STARTS FROM WHAT IT IS CALLED NOW**, because §5 says
            // rename edits the summary IN PLACE. An empty box would be a
            // different act — "name this thread" — and would make an accidental
            // enter erase the name.
            rename: tui_render::compose::Composer::from_text(summary),
            chosen: None,
            armed: None,
        }
    }

    fn project(name: &str) -> Self {
        Self {
            subject: Subject::Project {
                name: name.to_string(),
            },
            cursor: 0,
            editing: None,
            compact: tui_render::compose::Composer::new(),
            rename: tui_render::compose::Composer::new(),
            chosen: None,
            armed: None,
        }
    }

    fn buf(&mut self, act: SheetAct) -> &mut tui_render::compose::Composer {
        match act {
            SheetAct::Rename => &mut self.rename,
            _ => &mut self.compact,
        }
    }
}

/// One re-read per burst of `Invalidate` frames. A `MovePin` ripples six nodes
/// (`apps/aui/src/v2/mod.rs:487-510`) and every one of them would otherwise
/// cost a full re-read of thirteen index files.
const INVALIDATE_DEBOUNCE: Duration = Duration::from_millis(250);

/// **WHAT HE ASKED FOR, IN HIS WORDS**, for the notice that reports the answer.
///
/// It names the ACT and never the socket, the mutation kind or the port. He
/// does not need to know there are two owners — `0060`'s first sentence — and a
/// notice reading `set_thread_pinned: ok` would tell him about the wire when he
/// asked about his thread.
fn describe(act: &ThreadAct) -> String {
    match act {
        ThreadAct::SetModel(Some(m)) => format!("model {}", m.label().to_ascii_lowercase()),
        ThreadAct::SetModel(None) => "model back to the default".into(),
        ThreadAct::Interrupt => "interrupt".into(),
        ThreadAct::Compact { .. } => "compact".into(),
        ThreadAct::Rename { .. } => "rename".into(),
        ThreadAct::SetPinned(true) => "pin".into(),
        ThreadAct::SetPinned(false) => "unpin".into(),
        ThreadAct::Move(Dir::Up) => "move up".into(),
        ThreadAct::Move(Dir::Down) => "move down".into(),
        ThreadAct::SetClosed(true) => "mark closed".into(),
        ThreadAct::SetClosed(false) => "reopen".into(),
    }
}

/// What he asked for, in his words, for the notice line.
fn describe_registry(act: &acts::RegistryAct) -> String {
    match act {
        acts::RegistryAct::Register { path, .. } => format!("add {}", path.display()),
        acts::RegistryAct::Reorder { name, dir } => match dir {
            Dir::Up => format!("move {name} up"),
            Dir::Down => format!("move {name} down"),
        },
    }
}

fn describe_project(act: &ProjectAct, name: &str) -> String {
    match act {
        ProjectAct::SetModel(Some(m)) => {
            format!("{name}'s model {}", m.label().to_ascii_lowercase())
        }
        ProjectAct::SetModel(None) => format!("{name}'s model back to the default"),
        ProjectAct::Archive => format!("archive {name}"),
        ProjectAct::Remove => format!("remove {name}"),
        ProjectAct::Unarchive => format!("bring {name} back"),
        ProjectAct::Create { .. } => format!("create {name}"),
    }
}

impl App {
    pub fn new(
        handle: Handle,
        tx: mpsc::Sender<Ev>,
        endpoint: String,
        home: String,
        tz: i32,
    ) -> Self {
        let home_for_app = home.clone();
        let store = Store::new(endpoint, home);
        let mut app = Self {
            vs: ViewState {
                // VIEWPORT ENTRY IS THE BOTTOM, following (`DIR-3`).
                viewport: Viewport::at_bottom(),
                // **THE FACE OPENS ON PROJECTS**, following the AUI, whose
                // root menu is projects and not threads.
                mode: Mode::Projects,
                tz_offset_minutes: tz,
                ..Default::default()
            },
            store,
            handle,
            tx,
            // **THE ROOT MENU OPENS AT ITS TOP**, not at its bottom.
            // `from_bottom` stays the one scroll coordinate in the crate — the
            // render side pads a short project list at the BOTTOM instead of
            // the top and clamps this — so "the top" is spelled as the largest
            // possible distance from the bottom, exactly as `Intent::Top`
            // spells it. The first live run opened the 32-project root scrolled
            // to `zach` with the cursor off screen, which is how this was
            // found.
            projects_vp: Viewport {
                from_bottom: usize::MAX,
                follow: false,
                new_since: 0,
            },
            // The page is a top-anchored list exactly as the root is, and it is
            // re-aimed on every entry (see `Intent::Open`'s band arms) — this is
            // only the resting value.
            seeall_vp: Viewport {
                from_bottom: usize::MAX,
                follow: false,
                new_since: 0,
            },
            list_vp: Viewport::at_bottom(),
            thread_vp: Viewport::at_bottom(),
            activity_vp: Viewport::at_bottom(),
            opened: store::Opened::default(),
            projections: 0,
            batching: false,
            projection_owed: false,
            diag_view: Default::default(),
            thread_frozen: None,
            activity_frozen: None,
            tree_cursor: TreeId::Parent,
            tree_subject: TreeId::Parent,
            agents: Vec::new(),
            tree_ctx: store::TreeCtx::default(),
            project_cursor: 0,
            seeall_cursor: 0,
            pin_follow: None,
            project_follow: None,
            home: home_for_app,
            pending_new_thread: None,
            naming: None,
            last_wire: None,
            wheel: WheelMeter::default(),
            list_cursor: usize::MAX,
            hits: HitMap::default(),
            under: Mode::Projects,
            origin: Vec::new(),
            parents: Vec::new(),
            aui: None,
            sheet: None,
            new_project: None,
            archived_cursor: 0,
            last_invalidate: None,
            invalidate_owed: None,
            recording: None,
            hold: crate::mic::Hold::default(),
            playing: None,
            needs_full_repaint: false,
            last_rows: 0,
            last_size: (0, 0),
            drag: None,
            copy_owed: None,
            parked: std::collections::HashMap::new(),
            last_edit: None,
            draft_dirty: false,
            sending: None,
            quit: false,
            restart_note: None,
            restarting: false,
        };
        app.project();
        app
    }

    /// **WHAT THE TERMINAL NEGOTIATED**, recorded once at entry and handed to
    /// the renderer on every frame. See `term::negotiate`.
    pub fn set_caps(&mut self, caps: tui_render::Caps) {
        self.vs.caps = caps;
    }

    /// The columns the composer's text may occupy in the LAST drawn frame.
    ///
    /// Every edit that depends on the wrap — vertical motion, `home`, `end` —
    /// is answered against this, and it comes from the render crate's own
    /// function rather than from arithmetic here. A second derivation of the
    /// composer's inner width is `0023` `defect_classes` CLASS 3 in the one
    /// place it would show up as the cursor being on a different row from the
    /// one it is painted on.
    fn compose_width(&self) -> usize {
        tui_render::composer_inner(self.last_size.0 as usize)
    }

    /// **GO INTO `next`, REMEMBERING WHERE HE WAS.** The one push.
    ///
    /// Every door that descends goes through here, so "what does back mean"
    /// is answered once, by the stack, rather than by a clause per door.
    fn descend(&mut self, next: Mode) {
        let from = self.view();
        if from != next {
            // A depth this face cannot reach honestly is a leak, not a
            // feature: the deepest legal path is root -> project -> thread ->
            // activity, so anything past a handful is a push that lost its
            // pop and the OLDEST frame is the one to forget.
            if self.origin.len() >= 16 {
                self.origin.remove(0);
            }
            self.origin.push(from);
        }
        self.vs.mode = next;
    }

    // ── THE REGION MODEL ─────────────────────────────────────────────────
    //
    // A thread screen is a STACK OF REGIONS in his order — *"conversation /
    // queue (if it exists) / input box / subagents (if they exist)"* — exactly
    // one of which holds the selection, with every arrow resolving against it.
    // The selection IS `Mode::Thread(region)`; there is no second field to keep
    // in step with it. See `tui_render::Region`.

    /// Which region holds the selection, or `None` outside a thread.
    fn region(&self) -> Option<Region> {
        match self.view() {
            Mode::Thread(r) => Some(r),
            _ => None,
        }
    }

    /// **THE STOPS THAT EXIST ON THIS FRAME**, in his order, with the absent
    /// members skipped — an order over a VARIABLE SET, not four fixed stops.
    ///
    /// The queue is a stop only while it is holding something; the subagent
    /// band only while the thread has subagents to show. Recomputed per walk
    /// rather than stored, because both conditions change under him (a send
    /// empties the queue) and a remembered stop list would be a second truth.
    fn stops(&self) -> Vec<Region> {
        let mut v = vec![Region::Chat];
        if !self.vs.composer.buf.pending().is_empty() {
            v.push(Region::Queue);
        }
        v.push(Region::Composer);
        // **THE SUBAGENT BAND IS A STOP ONLY WHEN THERE IS A TREE** — which
        // is exactly "down to subagents IF APPLICABLE". The parent alone is
        // not a tree: with no subagents the band is not drawn, `Agents` is not
        // a stop, and `↓` off the box's last row is a no-op.
        if self.vs.tree.rows.len() > 1 {
            v.push(Region::Agents);
        }
        v
    }

    /// **THE ONLY WRITE TO A THREAD'S REGION.**
    ///
    /// Never [`App::descend`]: a region is not a DOOR. Pushing one onto the
    /// back-stack would make `←` "go back" to a band instead of leaving the
    /// thread, which is the first thing this model can get wrong.
    fn select_region(&mut self, r: Region) {
        let Mode::Thread(from) = self.vs.mode else {
            return;
        };
        if from == r {
            return;
        }
        self.vs.mode = Mode::Thread(r);
        if r == Region::Agents {
            // **ENTERING THE TREE LANDS ON THE SUBJECT'S ROW** — whose chat is
            // already above — so the band opens where he is rather than at an
            // end he has to walk back from.
            if self.vs.tree.subject_at().is_some() {
                self.tree_cursor = self.tree_subject.clone();
                self.vs.tree.cursor = self.tree_cursor.clone();
            }
        }
        if r == Region::Queue {
            // Entering from BELOW lands on the last row and from ABOVE on the
            // first, so the walk reads as one continuous column of stops
            // rather than teleporting to an end of the list.
            let last = self.vs.composer.buf.pending().len().saturating_sub(1);
            self.vs.queue_cursor = if from == Region::Chat { 0 } else { last };
        }
    }

    /// **THE WALK.** Called ONLY by an arrow whose own region had nothing left
    /// to move through in that direction. Past either end it does nothing:
    /// the chat has nothing above it and the last band nothing below.
    fn step_region(&mut self, down: bool) {
        let Some(here) = self.region() else { return };
        let stops = self.stops();
        let Some(i) = stops.iter().position(|r| *r == here) else {
            return;
        };
        let next = if down { i.checked_add(1) } else { i.checked_sub(1) };
        if let Some(r) = next.and_then(|j| stops.get(j)).copied() {
            self.select_region(r);
        }
    }

    /// How many rows the client queue is showing inside the composer band.
    fn queue_len(&self) -> usize {
        self.vs.composer.buf.pending().len()
    }

    /// **THE NEXT BAND HEADER ABOVE OR BELOW `from`, in the root's SELECTABLE
    /// coordinates** — what `opt-↑`/`opt-↓` land on at the main menu.
    ///
    /// It walks `ProjectList::item`, which is the one mapping from a selectable
    /// index to what is on that row, so this cannot disagree with what `→`
    /// would do on the row it lands on. `None` means there is no header in that
    /// direction and the key does nothing, silently.
    fn band_header_from(&self, from: usize, back: bool) -> Option<usize> {
        let n = self.vs.projects.selectable();
        let is_header = |i: usize| matches!(self.vs.projects.item(i), Some(RootItem::Band(_)));
        if back {
            (0..from.min(n)).rev().find(|i| is_header(*i))
        } else {
            (from.saturating_add(1)..n).find(|i| is_header(*i))
        }
    }

    /// **COME BACK OUT.** The one pop; returns where he landed.
    ///
    /// The root is the floor — see [`App::origin`].
    fn ascend(&mut self) -> Mode {
        let mut to = self.origin.pop().unwrap_or(Mode::Projects);
        // **NEVER LAND ON A PROJECT THAT IS NO LONGER OPEN** (`I-11`).
        //
        // `Mode::List` is the ONE stack entry whose target lives OUTSIDE the
        // stack: the mode says "a project's threads" and which project is
        // `store.open_project()`. De-register that project from inside one of
        // its threads — `c` on the root reaches the same registry — and `←`
        // used to deliver him onto `threads · no project open`, a screen with
        // no rows and a header naming nothing.
        //
        // So it pops again. The frame is not "repaired" into something else
        // and no clause is added to the caller: a door whose room has been
        // demolished is not a door, and popping past it is what a stack does.
        while to == Mode::List && self.store.open_project().is_none() {
            to = self.origin.pop().unwrap_or(Mode::Projects);
        }
        self.vs.mode = to;
        to
    }

    /// Which body mode is live — the one under an overlay, if one is up.
    /// **THE MODAL IS NAMED HERE AND NOT IN `Mode::is_overlay`**, deliberately.
    ///
    /// `is_overlay` means *"this mode REPLACES the body"* — it is what tells
    /// `render::bands` to hand the whole body to one surface and what tells
    /// `hit::activation` that the queue badge is not pressable. The rename
    /// modal does neither: the list he was reading is still drawn and still
    /// laid out by its own function. What it shares with an overlay is only
    /// this one thing — **there is a mode UNDERNEATH it** — so it is spelled
    /// out here rather than by widening a predicate whose other two readers
    /// would then be wrong.
    fn view(&self) -> Mode {
        if self.vs.mode.is_overlay() || self.vs.mode == Mode::RenameModal {
            self.under
        } else {
            self.vs.mode
        }
    }

    fn vp(&self) -> Viewport {
        match self.view() {
            // The field is a ROW OF THE ROOT, so the root's viewport is the
            // one that is on screen while he types into it.
            Mode::Projects | Mode::NewProject => self.projects_vp,
            Mode::SeeAll(_) => self.seeall_vp,
            Mode::List => self.list_vp,
            Mode::Activity => self.activity_vp,
            _ => self.thread_vp,
        }
    }

    fn vp_mut(&mut self) -> &mut Viewport {
        match self.view() {
            Mode::Projects | Mode::NewProject => &mut self.projects_vp,
            Mode::SeeAll(_) => &mut self.seeall_vp,
            Mode::List => &mut self.list_vp,
            Mode::Activity => &mut self.activity_vp,
            _ => &mut self.thread_vp,
        }
    }

    /// Whether this view's cursor is a ROW CURSOR rather than a viewport — the
    /// two lists move a cursor and the two transcripts move a window, and `↑`
    /// means both because it is the same gesture.
    fn is_list(&self) -> bool {
        matches!(self.view(), Mode::Projects | Mode::SeeAll(_) | Mode::List)
    }

    fn cursor(&self) -> usize {
        match self.view() {
            Mode::Projects => self.project_cursor,
            Mode::SeeAll(_) => self.seeall_cursor,
            _ => self.list_cursor,
        }
    }

    fn set_cursor(&mut self, n: usize) {
        match self.view() {
            Mode::Projects => self.project_cursor = n,
            Mode::SeeAll(_) => self.seeall_cursor = n,
            _ => self.list_cursor = n,
        }
    }

    /// **WHICH ROW OF THE ROOT'S PROJECTION THE CURSOR IS ON**, on whichever of
    /// the two surfaces that projection is being drawn as.
    ///
    /// `ProjectList` is ONE model with two surfaces — the root, and one band's
    /// `see all` page (`ProjectList::focus`) — so every act that resolves a row
    /// through `ProjectList::item` (`o`, `r`, `p`, `c`, move-pin, the two sheet
    /// doors, `→`) asks THIS rather than reading `project_cursor` directly. Nine
    /// call sites read that field before this existed, and a tenth that forgot
    /// would silently act on the root's row while he stood on the page's.
    fn root_cursor(&self) -> usize {
        match self.view() {
            Mode::SeeAll(_) => self.seeall_cursor,
            _ => self.project_cursor,
        }
    }

    /// **THE THREAD THE CURSOR IS ON**, on the root, a band's page or a
    /// project's list — the id `r` renames when no sheet can hold it.
    fn thread_under_cursor(&self) -> Option<String> {
        match self.view() {
            Mode::Projects | Mode::SeeAll(_) => self.vs.projects.thread_at(self.root_cursor()).map(|r| r.id.clone()),
            Mode::List => self.vs.list.rows.get(self.list_cursor).map(|r| r.id.clone()),
            _ => None,
        }
    }

    /// [`App::root_cursor`]'s setter. Used by the two pin/project FOLLOWS,
    /// which run inside the projection and must move the cursor of the surface
    /// being projected.
    fn set_root_cursor(&mut self, n: usize) {
        match self.view() {
            Mode::SeeAll(_) => self.seeall_cursor = n,
            _ => self.project_cursor = n,
        }
    }

    /// **THE BREADCRUMB FOR ONE SCREEN — the word `←` will land on.**
    ///
    /// It is the SHELL'S and not the pure crate's for one reason: two of the
    /// five answers need the store (a project's name, a thread's label), and
    /// `Mode::label` can only return a `&'static str`.
    ///
    /// `None` is a screen that contributes nothing to a trail. `Mode::Projects`
    /// is the FLOOR and is deliberately one of them: every trail ends at the
    /// menu, and seven columns of `menu ›` on every screen say nothing. **That
    /// is `0133`'s choice and it is HIS to overturn** — writing the root as a
    /// crumb is one static string here.
    ///
    /// Overlays never appear because they are never on the stack: an overlay
    /// owes back `App::under`, not `App::origin`, so they cannot reach this.
    fn crumb(&self, m: Mode) -> Option<String> {
        match m {
            Mode::Projects | Mode::NewProject => None,
            Mode::SeeAll(b) => Some(b.label().to_string()),
            // **`threads` WHEN NO PROJECT IS OPEN**, which is a state `I-11`
            // then makes unreachable by `←`: an `ascend()` onto `Mode::List`
            // with nothing open pops again. The word is here anyway, because a
            // crumb that could be an empty string is a trail with a hole in it.
            Mode::List => Some(
                self.store
                    .open_project()
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "threads".to_string()),
            ),
            Mode::Activity => Some("activity".to_string()),
            // Only ever a crumb UNDER the event lane — a thread is the leaf
            // everywhere else, and a leaf is the title, not a crumb.
            Mode::Thread(_) | Mode::Mic => self
                .store
                .open_id()
                .map(|id| self.store.label_of(id))
                .filter(|l| !l.is_empty()),
            Mode::Archived => Some("archived".to_string()),
            // **NO CRUMB, AND THE RENAME MODAL IS THE INTERESTING ONE.**
            //
            // A crumb names a screen `←` will LAND ON. The help overlay, the
            // queue, the sheet and its field are never on `App::origin`, so
            // they can never be landed on and can never be a crumb.
            //
            // `Mode::RenameModal` is not on the stack either, and it is worth
            // saying why, because it is the one mode here that is NOT an
            // overlay (`0130` deliberately kept it out of `Mode::is_overlay`:
            // it stands over a body that is still drawn). It is still not a
            // PLACE. Entering it does not `descend()` and leaving it is `esc`,
            // not `←` — so nothing ever pops to it and it owes no crumb.
            //
            // Added AT MERGE TIME: `crumb()` came from `0135` and the modal
            // from `0130`, in parallel worktrees, so neither branch could see
            // that the match had to answer for the other's mode. The compiler
            // caught it, which is the argument for `Mode::SeeAll` carrying its
            // band as a payload rather than being a flag.
            Mode::Help | Mode::Queue | Mode::Sheet | Mode::SheetField | Mode::RenameModal => None,
        }
    }

    /// **THE WHOLE TRAIL: THE BACK STACK, RENDERED** — the crumb of every
    /// screen `←` will visit before it reaches the root, outermost first.
    ///
    /// **DERIVED, NEVER STORED, and that is the invariant** (`I-6`). The
    /// tempting implementation is a `trail: Vec<String>` written on `descend`,
    /// and it is wrong for a reason that is not an implementation detail: 46 of
    /// 207 thread ids live in two to four project indexes (`0109`), so the same
    /// thread reached through the recent page and through its project MUST show
    /// two different trails. A crumb answers *"where does `←` land"*, which has
    /// one true answer per VISIT; it does not answer *"where does this thread
    /// live"*, which for a third of his fleet has none. A snapshot taken at
    /// `descend` time would also go stale under a rename, which is the hole
    /// `0130` is fixing one surface over.
    fn trail(&self) -> Vec<String> {
        let mut t: Vec<String> = self
            .origin
            .iter()
            .copied()
            .filter_map(|m| self.crumb(m))
            .collect();
        // The parents he came down through, outermost first: `mishmash ›
        // curator-v5` reads as the way back.
        for (p, _) in &self.parents {
            t.push(self.store.parent_label(p));
        }
        t
    }

    /// How many rows the cursor can stand on.
    ///
    /// **At the root this is the whole SURFACE, not `rows`.** The cursor space
    /// spans the pinned band, the recent band and the projects — a band row
    /// drills into its thread exactly as a project row drills into its project
    /// — and band headers, spacers and the archived tally are drawn but are not
    /// stops. `ProjectList::selectable` is the one count; see `drawn_row_of`
    /// for where that count meets the viewport's.
    fn list_len(&self) -> usize {
        match self.view() {
            Mode::Projects | Mode::SeeAll(_) => self.vs.projects.selectable(),
            _ => self.vs.list.rows.len(),
        }
    }

    /// **THE PROJECTION SEAM — the one CALL SITE, not the one author.**
    ///
    /// The projection itself lives in `store` (`thread_blocks`,
    /// `activity_blocks`, `thread_list`, `header`, `banner`); this method is
    /// the only place it is invoked, and after it returns `self.vs` is the
    /// whole truth the renderer will see. The distinction matters because the
    /// comment that stood here said truth became a `ViewState` "here and
    /// nowhere else", which sent a reader to this file looking for a thousand
    /// lines of presentation that are in `store.rs`.
    ///
    /// **Nothing else in this file may write to `self.vs`'s derived fields.**
    /// An intent moves the state the projection reads — the viewport, the
    /// filter, the mode — and then this runs. A handler that set `vs.blocks`
    /// directly would be a second projection with no name.
    fn project(&mut self) {
        self.projections = self.projections.wrapping_add(1);
        self.vs.header = store::header(&self.store, self.view());
        // **THE TRAIL IS DERIVED HERE, ON EVERY PROJECTION** — from `origin`
        // plus the view, and from nothing that was stored when a door was
        // opened. That is `I-6`, and it is also what makes a rename show up in
        // a crumb: the name is re-read, not remembered. See `App::trail`.
        self.vs.header.trail = self.trail();
        self.vs.banner = store::banner(&self.store, self.view());
        self.vs.down = self.down_page();
        self.vs.queue = self.store.queue().clone();
        let (stdout_only, expanded) = (self.vs.stdout_only, self.vs.expanded);
        let opened = std::mem::take(&mut self.opened);
        match self.view() {
            // **THE ROOT AND THE PAGE ARE ONE PROJECTION** — the only
            // difference is `focus`, which is `Some(b)` exactly when the view is
            // `Mode::SeeAll(b)`, so the page cannot be focused on a band its
            // mode does not name.
            Mode::Projects | Mode::NewProject | Mode::SeeAll(_) => {
                let focus = match self.view() {
                    Mode::SeeAll(b) => Some(b),
                    _ => None,
                };
                self.vs.projects = store::project_list(
                    &self.store,
                    self.root_cursor(),
                    self.vp().from_bottom,
                    focus,
                );
                // **THE CLAMP IS READ BACK — and `I-10` is why.** The band can
                // lose the row under him while he is standing on it (a thread
                // closes and leaves `recent`, a pin is dropped from a phone),
                // and `project_list` clamps the cursor against the surface's own
                // stop count. A cursor past the end is how `→` opens the wrong
                // thread.
                let clamped = self.vs.projects.cursor;
                self.set_root_cursor(clamped);
                // **THE CURSOR FOLLOWS THE THREAD, NOT THE ROW NUMBER.**
                //
                // Applied HERE, after the projection has been built and its
                // clamp has been read back, because this is the one place that
                // sees the NEW pinned order — and the new order is the thing
                // the old cursor index is wrong about. Doing it at the keypress
                // would anchor to an order that has not changed yet.
                self.follow_pin();
                self.follow_project();
                self.vs.list = Default::default();
                self.vs.blocks = Vec::new();
            }
            Mode::List => {
                self.vs.list =
                    store::thread_list(&self.store, self.list_cursor, self.list_vp.from_bottom);
                // The cursor is CLAMPED by the projection, then read back: the
                // index can change under him, and a cursor past the end is how
                // `→` opens the wrong thread.
                self.list_cursor = self.vs.list.cursor;
                self.vs.blocks = Vec::new();
            }
            Mode::Activity => {
                self.vs.blocks = store::activity_blocks(&self.store, stdout_only, expanded, &opened);
                self.vs.list = Default::default();
            }
            _ => {
                // **THE AGGREGATION, THEN THE TWO PLACES IT IS DRAWN.** One
                // record per subagent, read twice: as every node of the tree,
                // and as one collapsed tile in the chat at its launch seq. The
                // aggregate is KEPT — *"to be clear, I still want this"* — and
                // what was superseded is where it lived.
                let rows = store::open_rows(&self.store);
                self.agents = crate::agents::direct_attribute(&rows, Utc::now());
                drop(rows);
                // The path he came down through, root first: the tree is drawn
                // from ITS root (TUI-P1b), not from the open thread.
                self.tree_ctx.path = self.parents.iter().map(|(p, _)| p.clone()).collect();
                self.vs.tree = store::agent_tree(
                    &self.store,
                    &self.agents,
                    self.tree_cursor.clone(),
                    self.tree_subject.clone(),
                    &mut self.tree_ctx,
                );
                // **A ROW THAT LEFT THE RETAINED TAIL TAKES ITS CURSOR AND ITS
                // SUBJECT WITH IT.** The cursor falls to the parent, which
                // never vanishes while the tree exists; the subject falls back
                // to the thread and the transcript says so rather than going
                // blank.
                if self.vs.tree.subject_at().is_none() {
                    self.tree_subject = TreeId::Parent;
                    self.vs.tree.subject = TreeId::Parent;
                }
                if !self
                    .vs
                    .tree
                    .rows
                    .iter()
                    .any(|r| r.id == self.vs.tree.cursor)
                {
                    self.tree_cursor = TreeId::Parent;
                    self.vs.tree.cursor = TreeId::Parent;
                }
                self.vs.blocks = store::thread_blocks(
                    &self.store,
                    stdout_only,
                    expanded,
                    &opened,
                    &self.agents,
                    &self.tree_subject,
                    self.thread_frozen.as_ref().and_then(|f| f.seq),
                );
                self.vs.list = Default::default();
                self.follow_subject();
                self.diag_transcript();
            }
        }
        self.opened = opened;
        // **THE SHEET, REBUILT EVERY PROJECTION.** Its availability words track
        // both links live, so a row that could not act a second ago can act now
        // without him doing anything — and the reverse. The typed buffers are
        // MOVED in from `self.sheet` rather than rebuilt, which is what makes
        // "what is typed is kept" true across every intent.
        self.vs.sheet = match self.sheet.as_ref() {
            None => None,
            Some(st) => match &st.subject {
                Subject::Thread { project, id } => {
                    let field = st.editing.map(|act| tui_render::SheetField {
                        act,
                        label: match act {
                            SheetAct::Rename => "summary",
                            _ => "instructions",
                        },
                        buf: match act {
                            SheetAct::Rename => st.rename.clone(),
                            _ => st.compact.clone(),
                        },
                    });
                    store::sheet(&self.store, project, id, st.cursor, field, self.owners())
                }
                Subject::Project { name } => store::project_sheet(
                    &self.store,
                    name,
                    st.cursor,
                    st.chosen,
                    st.armed,
                    self.owners(),
                ),
            },
        };
        // **THE NAME BOX** (TUI-P1a) — projected as a sheet with no rows and
        // one field, because that is all the little modal draws: its rule and
        // the name. Its own words name its own door.
        self.vs.modal_label = None;
        if let Some(n) = self.naming.as_ref() {
            let (title, label) = match &n.target {
                NameTarget::New { place, .. } => (
                    format!("new thread in {place}"),
                    format!("new thread in {place} — name it · enter creates · esc cancels"),
                ),
                NameTarget::Thread { id } => (
                    self.store.label_of(id),
                    "rename — enter renames · esc cancels".to_string(),
                ),
            };
            self.vs.sheet = Some(tui_render::SheetView {
                title,
                facts: Default::default(),
                rows: Vec::new(),
                cursor: 0,
                armed: None,
                field: Some(tui_render::SheetField {
                    act: SheetAct::Rename,
                    label: "name",
                    buf: n.buf.clone(),
                }),
            });
            self.vs.modal_label = Some(label);
        }
        // **THE ROOT'S TWO AUI-OWNED CHROME FACTS**, moved in rather than
        // rebuilt: the typed name must outlive a projection, and the link state
        // is a socket fact `store` cannot see.
        self.vs.projects.new_project = self.new_project.clone();
        self.vs.projects.aui_down = self.owners().aui_down.map(str::to_string);
        // **THE ARCHIVED ROOM, ALWAYS PROJECTED.** It is read off the
        // filesystem, so it costs nothing to keep true and the door can never
        // open onto a stale room.
        self.vs.archived = store::archived_view(&self.store, self.archived_cursor, self.owners());
        self.archived_cursor = self.vs.archived.cursor;
        // The title the header shows while a sheet is up is the THREAD'S NAME
        // — §5's frame, where the sheet's subject IS its title.
        // …except over a THREAD PAGE, whose header is its name and status in
        // every overlay state (TUI-P1a, inspect F-2) — the sheet's title is the
        // name alone and would drop the status.
        if let Some(v) = self.vs.sheet.as_ref().filter(|_| !self.vs.header.thread_page) {
            self.vs.header.title = v.title.clone();
        }
        // A sheet whose thread has left the index (he closed it elsewhere, the
        // file was rewritten) closes rather than standing over nothing.
        if self.sheet.is_some() && self.vs.sheet.is_none() {
            self.sheet = None;
            if matches!(
                self.vs.mode,
                Mode::Sheet | Mode::SheetField | Mode::RenameModal
            ) {
                self.vs.mode = self.under;
            }
        }
        // **WHICH LIST THE MODAL IS STANDING OVER**, published every projection
        // so the frame can draw his place unchanged. `None` whenever no modal
        // is up, which is what keeps `Mode::RenameModal` the only reader of it.
        //
        // It is `self.under` and not `self.view()` for a reason the compiler
        // cannot catch: `view()` already resolves THROUGH the modal, so the two
        // are the same value today — but `under` is the field that means "the
        // surface this box is over", and naming the meaning rather than the
        // coincidence is what stops this drifting the day a modal opens over
        // something else.
        self.vs.rename_under = (self.vs.mode == Mode::RenameModal).then_some(self.under);
        // Read back the clamp the projection applied.
        if let (Some(st), Some(v)) = (self.sheet.as_mut(), self.vs.sheet.as_ref()) {
            st.cursor = v.cursor;
        }
        // **HOLD THE HELD PREFIX STILL.** See [`Frozen`] — the block count
        // alone does not do it, and the gap is the one he complained about.
        self.pin_the_frozen_tail();
        self.vs.viewport = self.vp();
        self.vs.viewport.new_since = self.arrivals();
        // **THE SELECTION NEEDS NO CLAMPING, AND THAT IS A PROPERTY OF
        // ADDRESSING IT LOGICALLY.** Select mode's index into the drawn blocks
        // had to be clamped here on every projection, because a block index
        // that outlived the blocks it indexed would highlight whatever moved
        // into its place. A `TextPos` names a unit that either exists or does
        // not: a selection over content that has gone paints nothing and
        // copies nothing, rather than quietly pointing somewhere else.
    }

    /// How many blocks have arrived since the view was frozen. `0` while
    /// following, always — following means the bottom IS where he is.
    fn arrivals(&self) -> usize {
        let frozen = match self.view() {
            // **NEITHER LIST HAS A TRANSCRIPT, AND NEITHER DOES THE PAGE.**
            // Leaving `Mode::SeeAll(_)` to the `_` arm would have made a scroll
            // on the page freeze and thaw the OPEN THREAD's transcript, which is
            // a band it is not even drawing.
            Mode::Projects | Mode::SeeAll(_) | Mode::List => return 0,
            Mode::Activity => self.activity_frozen.as_ref(),
            _ => self.thread_frozen.as_ref(),
        };
        match frozen {
            // **COUNTED BY LEDGER SEQ, FROM THE END.** The rows past the
            // freeze point are the ones at the tail whose seq is past it —
            // which is true whether a live row appended or relocated, and the
            // count is not.
            Some(Frozen { seq: Some(s), .. }) => {
                let s = *s;
                self.vs
                    .blocks
                    .iter()
                    .rev()
                    .take_while(|b| b.key().is_some_and(|(_, q)| *q > s))
                    .count()
            }
            // The fleet lane, whose seqs are per-thread. See [`Frozen::seq`].
            Some(f) => self.vs.blocks.len().saturating_sub(f.count),
            None => 0,
        }
    }

    /// **PAINT THE HELD PREFIX AS IT LOOKED WHEN HE SCROLLED AWAY.**
    ///
    /// > when my scroll isn't at the bottom of the convo, then new messages
    /// > should not scroll me down. Only if I'm at the bottom.
    ///
    /// The renderer already refuses to lay out blocks that arrived after the
    /// freeze. This is the other half, and it is the half that was missing:
    /// the last block BEFORE the freeze is laid out, and while a reply is
    /// streaming it grows several rows a second. See [`Frozen`] for the
    /// measurement.
    ///
    /// **The pin is taken on the first projection after the freeze and held
    /// until [`App::thaw`].** It is deliberately NOT refreshed by his
    /// keystrokes: re-anchoring on input would mean pressing `k` for one more
    /// row and getting thirty, which is the same yank wearing a different
    /// trigger. The cost is that the last held block cannot visibly change
    /// while he is away from the bottom — and that block is below the window
    /// by construction whenever `from_bottom` exceeds its height, so there is
    /// nothing on screen for it to fail to update.
    ///
    /// **It does not lie about what exists.** Only the PAINT is pinned; the
    /// store holds the live block throughout, `↓ N new` still counts arrivals,
    /// and the next thaw shows everything at once — which is the one
    /// deliberate jump he asked for.
    fn pin_the_frozen_tail(&mut self) {
        if matches!(self.view(), Mode::Projects | Mode::SeeAll(_) | Mode::List) {
            return;
        }
        // **THE LAST HELD BLOCK, LOCATED THE SAME WAY THE CHIP COUNTS.** With
        // a seq freeze the held prefix is `len - arrivals` and not the LENGTH
        // the view was frozen at: a row that relocated is still his, and the
        // old length is a fact about a projection that no longer exists.
        // Computed before the borrow, because it reads the same blocks.
        let held = self.vs.blocks.len().saturating_sub(self.arrivals());
        let frozen = match self.view() {
            Mode::Activity => self.activity_frozen.as_mut(),
            _ => self.thread_frozen.as_mut(),
        };
        let Some(frozen) = frozen else { return };
        // A projection SHORTER than the prefix has lost the block the pin
        // names; there is nothing to hold and reaching into it would panic.
        let Some(i) = held.checked_sub(1) else {
            return;
        };
        let Some(live) = self.vs.blocks.get_mut(i) else {
            return;
        };
        match frozen.tail {
            Some(ref pinned) => *live = pinned.clone(),
            None => frozen.tail = Some(live.clone()),
        }
    }

    /// Freeze the live view's content where it stands. Idempotent: a second
    /// scroll up must not move the anchor, or the rows under his eye slide by
    /// one every time he presses `k`.
    fn freeze(&mut self) {
        let n = self.vs.blocks.len();
        match self.view() {
            Mode::Projects | Mode::SeeAll(_) | Mode::List => {}
            Mode::Activity => {
                self.activity_frozen
                    .get_or_insert_with(|| Frozen::at(n, None));
            }
            _ => {
                // **THE LEDGER'S HIGH-WATER MARK AT THE MOMENT HE SCROLLED
                // AWAY.** See [`Frozen::seq`] for why the block count alone
                // cannot hold a relocating row still.
                let seq = self.store.open_head_seq();
                self.thread_frozen
                    .get_or_insert_with(|| Frozen::at(n, seq));
            }
        }
    }

    /// Release the anchor: everything held back becomes visible on the next
    /// frame. The one deliberate jump, and he asked for it.
    fn thaw(&mut self) {
        match self.view() {
            Mode::Projects | Mode::SeeAll(_) | Mode::List => {}
            Mode::Activity => self.activity_frozen = None,
            _ => self.thread_frozen = None,
        }
    }

    /// One fact from the daemon.
    fn on_transport(&mut self, ev: TransportEvent) {
        // A reconnect repaints over a screen the last session left behind, and
        // a differential renderer has no way back from cells it agrees with.
        let relink = matches!(
            ev,
            TransportEvent::Connected { .. } | TransportEvent::Disconnected { .. }
        );
        // A toast for the loud ones, so a gap repair is visible even to someone
        // who is scrolled away from the bottom of the lane.
        if let TransportEvent::Note {
            ref text,
            level: NoteLevel::Critical | NoteLevel::Warn,
        } = ev
        {
            self.notice(text.clone(), Severity::Warn);
        }
        // ARRIVALS NEVER YANK THE VIEWPORT. The freeze point set when he
        // scrolled away is what holds the window still; `project` re-derives
        // the chip's count from it. `Applied` is still read, because the two
        // views freeze independently — an event on another thread is not new to
        // the thread he has open.
        let applied = self.store.apply(ev, Utc::now());
        tracing::trace!(?applied, "applied");
        if relink {
            self.force_repaint("the link changed hands");
        }
        if self.batching {
            self.projection_owed = true;
        } else {
            self.project();
        }
    }

    /// **ONE PROJECTION PER BATCH, NOT PER FACT** (THREAD-LOAD-LATENCY F1).
    ///
    /// Opening a 4,000-row thread replays every row as a transport fact, and a
    /// streaming reply is ~125 deltas a message. Re-projecting the whole tail
    /// once per fact was O(rows) per fact — measured 1.6 s of UI-loop time for a
    /// 257-event batch on a 4,000-row thread, during which nothing was drawn and
    /// no key was read. Inside a batch a transport fact only marks the
    /// projection owed; the debt is paid ONCE, before any non-transport event is
    /// applied (so every input handler still sees a current view) and at
    /// [`App::end_batch`], before the paint. Order and content are unchanged:
    /// the store still applies every fact, in arrival order.
    pub fn begin_batch(&mut self) {
        self.batching = true;
    }

    /// Close the batch and pay any owed projection. See [`App::begin_batch`].
    pub fn end_batch(&mut self) {
        self.batching = false;
        self.settle_projection();
    }

    fn settle_projection(&mut self) {
        if self.projection_owed {
            self.projection_owed = false;
            self.project();
        }
    }

    /// **WHAT THE TRANSCRIPT VIEW GAINED, CHANGED AND LOST IN THIS
    /// PROJECTION**, to the flight recorder. A block is named by its row key
    /// (thread, seq) or, for a message, its id; its summary is the sender,
    /// the text length and a preview — so a box that drew words and then drew
    /// none is one `CHANGE` line, and a box that left is one `GONE` line.
    fn diag_transcript(&mut self) {
        if !crate::diag::on() {
            return;
        }
        let thread = self.store.open_id().unwrap_or("").to_string();
        let mut now = std::collections::BTreeMap::new();
        for b in &self.vs.blocks {
            let (k, v) = match b {
                Block::Message {
                    id, sender, text, key, ..
                } => (
                    match key {
                        Some((_, seq)) => format!("seq{seq:010}"),
                        None => format!("msg:{}", id.message_id),
                    },
                    format!(
                        "message id={} sender={sender:?} text_len={} text={}",
                        id.message_id,
                        text.len(),
                        crate::diag::preview(text, 40)
                    ),
                ),
                Block::Lane { key, text, glyph, .. } => (
                    match key {
                        Some((_, seq)) => format!("seq{seq:010}"),
                        None => format!("lane:{}", crate::diag::preview(text, 24)),
                    },
                    format!("lane {glyph} text_len={} text={}", text.len(), crate::diag::preview(text, 40)),
                ),
                Block::Rule { .. } => continue,
            };
            now.insert(k, v);
        }
        let (was_thread, was) = std::mem::take(&mut self.diag_view);
        if was_thread != thread {
            crate::diag!(
                "view",
                "RESET thread={thread} (was {was_thread}) blocks={}",
                now.len()
            );
        } else {
            for (k, v) in &was {
                match now.get(k) {
                    None => crate::diag!(
                        "view",
                        "GONE thread={thread} key={k} was [{v}] reason=not in this projection{}",
                        if self.store.open_has_seq(k) { " (row still in store — folded away by the projection)" } else { " (row not in store)" }
                    ),
                    Some(n) if n != v => crate::diag!(
                        "view",
                        "CHANGE thread={thread} key={k} was [{v}] now [{n}]"
                    ),
                    _ => {}
                }
            }
            for (k, v) in &now {
                if !was.contains_key(k) {
                    crate::diag!("view", "NEW thread={thread} key={k} [{v}]");
                }
            }
        }
        self.diag_view = (thread, now);
    }

    /// The ONE way this flag is ever set. See its doc comment.
    fn force_repaint(&mut self, why: &'static str) {
        tracing::debug!(why, "full repaint");
        self.needs_full_repaint = true;
    }

    /// **POST A NOTICE — the one door into the info area, for every transient
    /// message this face has.**
    ///
    /// Last write wins, deliberately: there is ONE slot and no queue. A queue
    /// would mean a notice he has already stopped caring about delaying the one
    /// that matters, and the newest fact is always the one worth the columns.
    fn notice(&mut self, text: impl Into<String>, severity: Severity) {
        self.notice_for(text, severity, ttl(severity));
    }

    /// A notice with a stated lifetime, for the callers that know better than
    /// [`ttl`] — a paste in progress knows exactly how long its subject can
    /// last.
    fn notice_for(&mut self, text: impl Into<String>, severity: Severity, ttl: Duration) {
        self.vs.notice = Some(Notice {
            text: text.into(),
            severity,
            expires_at: Some(Utc::now() + ttl),
        });
    }

    // ── THE SHEET (§5) ──────────────────────────────────────────────────

    /// **HAND THE FACE ITS 9010 CLIENT.**
    ///
    /// A setter and not a constructor argument, deliberately: an `App` with no
    /// 9010 client is a state the face must draw correctly anyway — it is what
    /// a stopped ui-server looks like from here — so every test gets that state
    /// for free and the live loop opts in.
    /// **THE LAST WIRE AN ACT LEFT WITH** (TUI-P1a) — what a test outside this
    /// crate asserts a daemon act's bytes on, since the send itself is spawned.
    pub fn last_wire(&self) -> Option<&acts::Wire> {
        self.last_wire.as_ref()
    }

    pub fn set_aui(&mut self, h: aui::Handle) {
        self.aui = Some(h);
    }

    /// The 9010 link, or [`Reach::Absent`] when no client was started.
    ///
    /// **NOTHING THE FACE RENDERS MAY READ THIS EXCEPT AN ACT ROW'S WORDS.**
    /// That is `0060`'s second binding condition and it is the one that keeps
    /// `DIR-7` intact: no fact, no count, no state word anywhere on this face
    /// is sourced from 9010, and the face draws identically with it absent. A
    /// row saying why it cannot act is the opposite of a face that needs a
    /// socket to draw.
    fn aui_reach(&self) -> Reach {
        match self.aui.as_ref() {
            Some(h) => h.reach(),
            // No client at all: there is no door, which is the same thing a
            // down tailnet produces and is said the same way.
            None => Reach::Absent {
                cause: DownCause::DoorShut,
            },
        }
    }

    /// Which owners can act right now, in the words their rows will carry.
    fn owners(&self) -> store::Owners<'_> {
        store::Owners {
            aui_down: self.aui_reach().words(),
            daemon_down: (!matches!(self.store.link, LinkState::Connected))
                .then_some(store::DAEMON_DOWN),
        }
    }

    /// **OPEN THE SHEET FOR SELECTABLE ROW `i`** of whatever surface is up.
    ///
    /// A miss is silent and the sheet does not open — a door that opens onto
    /// nothing is worse than one that does not open.
    fn open_sheet(&mut self, i: usize) {
        let row = match self.view() {
            Mode::Projects | Mode::SeeAll(_) => self.vs.projects.thread_at(i).cloned(),
            Mode::List => self.vs.list.rows.get(i).cloned(),
            _ => None,
        };
        let Some(row) = row else { return };
        self.sheet = Some(SheetState::thread(&row.project, &row.id, &row.summary));
        self.under = self.view();
        self.vs.mode = Mode::Sheet;
    }

    /// **OPEN THE PROJECT SHEET FOR ROOT ROW `i`** (§5's second frame).
    ///
    /// A miss is silent, exactly as the thread door's is — and a row that is
    /// not a PROJECT row is a miss here even though it is a legal root row: the
    /// name span is only ever claimed on a project, so this can only be reached
    /// by one.
    fn open_project_sheet(&mut self, i: usize) {
        let Some(RootItem::Project(n)) = self.vs.projects.item(i) else {
            return;
        };
        let Some(name) = self.vs.projects.rows.get(n).map(|r| r.name.clone()) else {
            return;
        };
        self.sheet = Some(SheetState::project(&name));
        self.under = self.view();
        self.vs.mode = Mode::Sheet;
    }

    /// The thread the open sheet is about, as [`acts`] needs it.
    fn sheet_thread(&self) -> Option<ThreadRef> {
        let Subject::Thread { project, id } = &self.sheet.as_ref()?.subject else {
            return None;
        };
        let t = self
            .store
            .projects()
            .iter()
            .find(|p| &p.name == project)?
            .threads
            .iter()
            .find(|t| &t.id == id)?;
        Some(ThreadRef {
            project: project.clone(),
            id: id.clone(),
            turn_count: t.turn_count,
            pinned: t.pinned,
            closed: t.closed,
        })
    }

    /// The project the open sheet is about, if it is about one.
    fn sheet_project(&self) -> Option<String> {
        match &self.sheet.as_ref()?.subject {
            Subject::Project { name } => Some(name.clone()),
            Subject::Thread { .. } => None,
        }
    }

    /// **ACTIVATE ONE ACT ROW.**
    ///
    /// The unavailable case is FIRST and it is the whole of `0060`'s temporal
    /// rule: the row's own sentence goes into the notice line, once, nothing
    /// is sent, and the row stays selected. It is emphatically not a silent
    /// no-op, which the stub doctrine forbids by name.
    fn sheet_act(&mut self, i: usize) {
        let Some(view) = self.vs.sheet.as_ref() else {
            return;
        };
        let Some(row) = view.row(i).cloned() else {
            return;
        };
        if let Some(sheet) = self.sheet.as_mut() {
            sheet.cursor = i;
        }
        if let Some(why) = row.avail.reason() {
            // The same sentence the row is already wearing, so the two halves
            // of the refusal cannot say different things.
            self.notice(why.to_string(), Severity::Warn);
            // An unreachable row also DISARMS whatever was armed: he pressed
            // something else, and §5's `asks first` must not survive a press
            // that was aimed at a different row.
            self.disarm();
            return;
        }
        // **THE ARMED ROW FIRES; EVERY OTHER PRESS DISARMS.** This is §5's
        // `asks first`, and the order matters: read the arm BEFORE clearing it,
        // so the second press on the same row is the one that acts.
        let was_armed = self.sheet.as_ref().and_then(|s| s.armed) == Some(row.act);
        self.disarm();
        match row.act {
            // The two acts that TAKE TEXT open the field instead of firing.
            SheetAct::Compact | SheetAct::Rename => {
                if let Some(sheet) = self.sheet.as_mut() {
                    sheet.editing = Some(row.act);
                }
                self.vs.mode = Mode::SheetField;
            }
            // **THE ONE ROW THAT ASKS FIRST.** `remove` takes a project off
            // his root — the act that changes his list most — so the first
            // press ARMS it and says so on the row, and only the second sends
            // anything. Nothing leaves the process on the first press.
            //
            // It de-registers and destroys nothing on disk; it stopped
            // unlinking `~/minds/<name>` at the registry cutover
            // (`crate::acts::ProjectAct`). The arm STAYS: a project leaving
            // his root is still his root changing, `0099` asks for it by
            // name, and `c` now reaches this row from the keyboard.
            SheetAct::Remove if !was_armed => {
                if let Some(sheet) = self.sheet.as_mut() {
                    sheet.armed = Some(SheetAct::Remove);
                }
            }
            SheetAct::ProjectModel | SheetAct::Archive | SheetAct::Remove => {
                if let (Some(act), Some(name)) =
                    (self.project_act_for(row.act), self.sheet_project())
                {
                    self.fire_project(act, name);
                }
            }
            // **THE ONE ACT THAT NEVER LEAVES THIS PROCESS.** It copies the
            // thread's FULL id, and it is the deliberate home the id moved to
            // when `0076` took it off the list row (`DEC-27`'s invariant kept,
            // its eight characters retired). It says so out loud, because a
            // clipboard write is invisible — but it says so through
            // `pay_the_clipboard`, which is the ONE place that knows whether
            // `OSC 52` was actually accepted. A notice written here would be a
            // second, optimistic answer to the same question, and it would be
            // the one he read.
            SheetAct::CopyId => {
                if let Some(t) = self.sheet_thread() {
                    self.copy_owed = Some(t.id.clone());
                }
            }
            other => {
                if let Some(act) = self.act_for(other, None) {
                    self.fire(act);
                }
            }
        }
    }

    /// **OPEN THE ADD-A-PROJECT FIELD.**
    ///
    /// # It no longer refuses when the audio interface is down, and that is a
    /// change round five made because it added a SECOND OWNER to this row
    ///
    /// It used to repeat the AUI's down-sentence and open nothing, which was
    /// right while the field had exactly one act behind it: creating a project
    /// by NAME is `Mutation::CreateProject` and needs 9010. The field now also
    /// takes a PATH, and registering a path is a locked write to
    /// `~/.cauldron/projects.json` that needs no socket at all — it is the one
    /// act on this face that is MOST wanted with `ui-server` stopped, because
    /// the file is what `ui-server` reads when it comes back.
    ///
    /// So the gate moves from the DOOR to the ACT, which is where `0060` puts
    /// it anyway (*"what needs the AUI is the act"*): a NAME committed with
    /// the socket down is refused by `send_wire` in the AUI's own down-words
    /// and his typing survives in the buffer, exactly as a refused name does.
    /// A PATH just works.
    fn open_new_project(&mut self) {
        if self.new_project.is_none() {
            self.new_project = Some(tui_render::compose::Composer::new());
        }
        self.vs.mode = Mode::NewProject;
    }

    /// **CREATE THE PROJECT WITH WHAT IS TYPED.**
    ///
    /// The name goes RAW to the AUI, which owns the one normalizer and refuses
    /// a collision rather than repairing it. **A name that is in the archive is
    /// refused too, and never auto-unarchived** (`server/mod.rs:163` — bringing
    /// a project back is its own deliberate act), and that refusal is the AUI's
    /// own sentence, relayed verbatim: `<name> is in the archive.` Saying what
    /// happened in words is the whole of the asymmetry this face must respect.
    fn create_project(&mut self) {
        let name = self
            .new_project
            .as_ref()
            .map(|c| c.text())
            .unwrap_or_default();
        let name = name.trim().to_string();
        // Back to the root either way — the field is one short answer. The
        // BUFFER is kept until the far end accepts (see `Clears`).
        self.vs.mode = Mode::Projects;
        // **ONE FIELD, ONE ACT, TWO SHAPES OF ARGUMENT** — `p`'s add-by-path
        // and `n`'s create-by-name are the same act (*add a project*) and the
        // argument says which. They are not two fields because a NAME can
        // never contain `/`: `projects::name_objection` refuses one that does,
        // so the split is total rather than a heuristic. See
        // `acts::looks_like_a_path`.
        //
        // A PATH registers a directory that is already there and the AUI has
        // no mutation for it; a NAME makes a new directory and the AUI does
        // (`Mutation::CreateProject`). Two owners, and the argument chooses.
        if acts::looks_like_a_path(&name) {
            let path = index::resolve_typed_path(&self.home, &name);
            self.fire_registry(acts::RegistryAct::Register { path, name: None }, &[]);
            return;
        }
        self.fire_project(ProjectAct::Create { name: name.clone() }, name);
    }

    /// **`n` ON THE MAIN PAGE — A NEW THREAD IN `miscellaneous`, AND IT
    /// OPENS.**
    ///
    /// **His words**: *"ok and where is the n for new thread. It should work on
    /// the main page and create a misc thread. I need that asap."*
    ///
    /// Three facts, and the third is the uncomfortable one:
    ///
    ///  1. **THE DAEMON OWNS IT.** Whether a thread exists is the daemon's
    ///     state, so the act is `threads/create` on 3651 and there is nothing
    ///     on 9010 to relay it through — `libs/aui-state`'s mutation
    ///     vocabulary has no `CreateThread`. The bytes are
    ///     [`acts::route_new_thread`]'s, pure and asserted without a socket.
    ///  2. **THE ID IS MINTED HERE**, which is the AUI's own convention and is
    ///     what lets the thread be opened off the ACK rather than off a reply
    ///     this face would have to parse.
    ///  3. **IT WILL NOT BE ON HIS LIST, AND HE IS TOLD SO IN THE SAME
    ///     BEAT.** The list is the union of every root's `.mind/threads.json`
    ///     ([`index`]'s predicate); a daemon-born thread has a header in none
    ///     of them, `apps/aui`'s ledger fold never inserts one, and this face
    ///     does not write that file. A face that let the empty list deliver
    ///     that news would be a face that pretended the act was complete.
    /// **`n` — NAME A NEW THREAD, THEN MAKE IT** (TUI-P1a, checklist TC-01).
    ///
    /// *"When I create a new thread, whether it's miscellaneous or in a
    /// project, I want to be able to name it immediately. I want the modal to
    /// pop up to name the thread."* So `n` opens the little name box and sends
    /// nothing; `enter` creates the thread WITH what is typed (blank is an
    /// unnamed thread, called by its first message), and `esc` makes nothing.
    ///
    /// **WHERE IT RUNS:** inside a project, that project's root — *"if I go
    /// into a project and I press the letter N, it should create a new thread
    /// in that project"*; anywhere else, the bucket.
    fn new_thread(&mut self) {
        let (cwd, place) = match (self.view(), self.store.open_project()) {
            (Mode::List, Some(p)) => (p.root.clone(), p.name.clone()),
            // **THE BUCKET'S ROOT FROM THE CRATE THAT OWNS IT**, never a
            // literal: `~/.aui/miscellaneous` is where the bucket's threads
            // have always been spawned, and it is the AUI's own fallback cwd
            // (`build_thread_settings`: `cfg.state_dir.join("miscellaneous")`).
            _ => (
                projects::bucket_root(std::path::Path::new(&self.home)),
                acts::BUCKET.to_string(),
            ),
        };
        self.open_naming(NameTarget::New { cwd, place }, "");
    }

    /// Raise the name box over the surface he is on.
    fn open_naming(&mut self, target: NameTarget, prefill: &str) {
        self.naming = Some(Naming {
            target,
            buf: tui_render::compose::Composer::from_text(prefill),
        });
        self.under = self.view();
        self.vs.mode = Mode::RenameModal;
    }

    /// **CREATE THE THREAD** — the send `n` used to make on the press, now
    /// made on the name box's `enter`, with the name.
    fn create_thread(&mut self, cwd: &std::path::Path, place: &str, name: Option<&str>) {
        let id = uuid::Uuid::new_v4();
        let wire = acts::route_new_thread(&id, cwd, aui_wire::Model::default(), name);
        // Minted BEFORE the send, so the ack has somewhere to land however fast
        // it comes back.
        self.pending_new_thread = Some(id.to_string());
        self.send_wire(
            wire,
            format!("a new thread in {place}"),
            Some(Clears::ThreadCreated),
        );
    }

    /// **`enter` IN THE NAME BOX.** A new thread is created (named, or not);
    /// an existing one is renamed through the daemon, and an empty rename
    /// keeps the box open, exactly as the sheet's modal does.
    fn commit_naming(&mut self) {
        let Some(n) = self.naming.take() else { return };
        let text = n.buf.text().trim().to_string();
        match &n.target {
            NameTarget::New { cwd, place } => {
                self.vs.mode = self.under;
                let name = (!text.is_empty()).then_some(text.as_str());
                self.create_thread(cwd, place, name);
            }
            NameTarget::Thread { id } => {
                if text.is_empty() {
                    self.naming = Some(n);
                    self.notice(
                        "the name is empty — type one, or esc to keep the name it has".to_string(),
                        Severity::Warn,
                    );
                    return;
                }
                self.vs.mode = self.under;
                let wire = acts::Wire::Daemon(acts::Verb::Name {
                    thread: id.clone(),
                    name: text,
                });
                self.send_wire(wire, "rename".to_string(), None);
            }
        }
    }

    /// **OPEN THE ARCHIVED ROOM.**
    ///
    /// It opens in EVERY state, including with the audio interface down: the
    /// set is read off the filesystem by this face, and `0060`'s second binding
    /// condition is that nothing rendered is sourced from 9010. What needs the
    /// AUI is the unarchive, and that is a sentence on each row.
    fn open_archived(&mut self) {
        self.archived_cursor = 0;
        self.vs.mode = Mode::Archived;
    }

    /// **BRING ARCHIVED PROJECT `i` BACK.** One press — the row says so in
    /// words — and an unreachable owner is that row's own sentence, once.
    fn unarchive(&mut self, i: usize) {
        let Some(row) = self.vs.archived.row(i).cloned() else {
            return;
        };
        self.archived_cursor = i;
        if let Some(why) = row.avail.reason() {
            self.notice(why.to_string(), Severity::Warn);
            return;
        }
        self.fire_project(ProjectAct::Unarchive, row.name);
    }

    /// Clear any armed row. Called by every press that is not the second press
    /// on the armed row itself, and by leaving the sheet.
    fn disarm(&mut self) {
        if let Some(s) = self.sheet.as_mut() {
            s.armed = None;
        }
    }

    /// Turn a drawn PROJECT row into the act it stands for.
    fn project_act_for(&mut self, act: SheetAct) -> Option<ProjectAct> {
        let _ = self.sheet_project()?;
        Some(match act {
            SheetAct::ProjectModel => {
                // Cycle from what THIS FACE last set, which is the only project
                // model value it can honestly name: `None` -> Sonnet -> Opus ->
                // Fable -> back to the default. See `store::project_sheet`.
                let next = aui_wire::Model::cycle_opt(self.sheet.as_ref()?.chosen.flatten());
                if let Some(s) = self.sheet.as_mut() {
                    s.chosen = Some(next);
                }
                ProjectAct::SetModel(next)
            }
            SheetAct::Archive => ProjectAct::Archive,
            SheetAct::Remove => ProjectAct::Remove,
            _ => return None,
        })
    }

    /// Turn a drawn row into the act it stands for.
    ///
    /// This is the pure crate's vocabulary meeting the shell's, and it is the
    /// only place the two meet. The render crate may not know a `Mutation`
    /// exists — that is its manifest's gate — so the translation happens here
    /// and the OWNER binding happens one layer further in, in [`acts::route`].
    fn act_for(&self, act: SheetAct, text: Option<String>) -> Option<ThreadAct> {
        let t = self.sheet_thread()?;
        Some(match act {
            // **THE MODEL CYCLES FROM WHAT THE LAST TURN RAN ON**, because that
            // is the only model value this face can honestly read. The AUI's
            // override lives in its settings file, which this face never opens;
            // `0060` records that the two can disagree and that the daemon's is
            // "what the last turn ran" while the AUI's is "what the next will".
            // Cycling from the observable one is the honest available input.
            SheetAct::Model => {
                ThreadAct::SetModel(aui_wire::Model::cycle_opt(self.daemon_model(&t.id)))
            }
            SheetAct::Interrupt => ThreadAct::Interrupt,
            SheetAct::Compact => ThreadAct::Compact { instructions: text },
            SheetAct::Rename => ThreadAct::Rename {
                summary: text.unwrap_or_default(),
            },
            SheetAct::Pin => ThreadAct::SetPinned(!t.pinned),
            SheetAct::MoveUp => ThreadAct::Move(Dir::Up),
            SheetAct::MoveDown => ThreadAct::Move(Dir::Down),
            SheetAct::Closed => ThreadAct::SetClosed(!t.closed),
            // The project sheet's three rows are not thread acts and never
            // reach here — `sheet_act` routes them to `project_act_for` — but
            // the arm is written rather than wildcarded so that adding a row
            // to either sheet is a compile error until it has an owner.
            SheetAct::ProjectModel | SheetAct::Archive | SheetAct::Remove => return None,
            // **`copy the thread id` LEAVES NO PROCESS.** It is a clipboard
            // write, so it has no owner to route to and no mutation to send;
            // `fire_sheet_act` handles it before it ever reaches here. Written
            // out rather than wildcarded, for the reason above.
            SheetAct::CopyId => return None,
        })
    }

    /// The model the DAEMON reports for this thread, mapped onto the AUI's
    /// three-value vocabulary. Tolerant by design: the daemon's string is
    /// whatever was configured, and an unrecognised one is `None` — which
    /// cycles to Sonnet, the AUI's own first step.
    fn daemon_model(&self, id: &str) -> Option<aui_wire::Model> {
        let m = self.store.model_of(id)?.to_ascii_lowercase();
        if m.contains("opus") {
            Some(aui_wire::Model::Opus)
        } else if m.contains("sonnet") {
            Some(aui_wire::Model::Sonnet)
        } else if m.contains("fable") {
            Some(aui_wire::Model::Fable)
        } else {
            None
        }
    }

    // ── THE ROOT'S BANDS: EXPAND, COLLAPSE, AND REORDER ──────────────────

    /// **OPEN ONE BAND'S OWN PAGE — `see all`, from either of its two doors.**
    ///
    /// > let's replace 'show more' with 'see all' and it takes us to a dedicated
    /// > page with just that band and all results. … Going right on a band title
    /// > does the same thing.
    ///
    /// `at` is where the page's cursor lands, and it is the ONLY thing the two
    /// doors disagree about: `0` from the band HEADER, `BAND_ROWS` from the
    /// `see all` FOOT — the first row the root could not show him, which is the
    /// row he was reaching for. Both doors land here so they cannot drift, which
    /// is the same reason `expand_band` existed before them.
    ///
    /// **THE VIEWPORT IS RE-AIMED AND NOT INHERITED.** A page he last left
    /// scrolled a hundred rows down would otherwise open scrolled there with the
    /// cursor at row 0; `follow_cursor` then drags it back, which is a visible
    /// jump. Aiming the window at the top and letting `follow_cursor` scroll
    /// down to `at` is the same arithmetic every other door uses.
    ///
    /// This is a DESCENT and it goes through [`App::descend`], so `←` is
    /// answered by the stack and not by a clause of the page's own.
    fn open_band_page(&mut self, b: tui_render::BandId, at: usize) {
        self.release_pin_follow();
        self.seeall_cursor = at;
        self.seeall_vp = Viewport {
            from_bottom: usize::MAX,
            follow: false,
            new_since: 0,
        };
        self.descend(Mode::SeeAll(b));
    }

    /// **PUT THE ROOT CURSOR BACK ON THE THREAD IT WAS FOLLOWING.**
    ///
    /// Called from [`App::project`] with the NEW pinned order in hand. It walks
    /// the selectable rows through `ProjectList::item` — the one resolution the
    /// renderer and the hit map also read — so the index it computes is the
    /// index `→` would open, with no second reading of the surface.
    ///
    /// A followed thread that is no longer in the pinned band (he unpinned it
    /// from the phone, the AUI refused the move and something else changed the
    /// file) simply leaves the cursor where it is. The anchor is a preference,
    /// never a jump to somewhere he did not ask to be.
    fn follow_pin(&mut self) {
        let Some(id) = self.pin_follow.clone() else {
            return;
        };
        let list = &self.vs.projects;
        let found = (0..list.selectable()).find(|&i| {
            matches!(list.item(i), Some(tui_render::RootItem::Pinned(n))
                if list.pinned.get(n).is_some_and(|r| r.id == id))
        });
        if let Some(i) = found {
            self.set_root_cursor(i);
            self.vs.projects.cursor = i;
        }
    }

    /// **DROP THE ANCHOR.** Any deliberate cursor movement ends the follow —
    /// otherwise the cursor would refuse to leave the row he just moved.
    /// [`App::follow_pin`] for a PROJECT row. Same shape, different
    /// `RootItem`, and the identity is the NAME.
    fn follow_project(&mut self) {
        let Some(name) = self.project_follow.clone() else {
            return;
        };
        let list = &self.vs.projects;
        let found = (0..list.selectable()).find(|&i| {
            matches!(list.item(i), Some(tui_render::RootItem::Project(n))
                if list.rows.get(n).is_some_and(|r| r.name == name))
        });
        if let Some(i) = found {
            self.set_root_cursor(i);
            self.vs.projects.cursor = i;
        }
    }

    fn release_pin_follow(&mut self) {
        self.pin_follow = None;
    }

    /// **`ctrl-↑` / `ctrl-↓` AT THE ROOT — MOVE A PIN, CURSOR TRAVELLING.**
    ///
    /// The operator's words, and the two readings `0072` made of them, both
    /// recorded here because they are readings and not his literal text:
    ///
    /// > in the pinned menu, if we go into the band with pinned projects, I
    /// > want to be able to make them reorder them. Unpin them to move their
    /// > position up one, and the cursor should go with it, so we can click
    /// > that multiple times to move it to a new location.
    ///
    /// 1. **"Unpin them to move their position up one" is read as MOVE-UP.**
    ///    The rest of the sentence describes reordering end to end; unpinning a
    ///    thread would remove it from the band rather than move it within one,
    ///    and there is nowhere for a repeated tap to walk it to. `unpin` is
    ///    taken as a capture artifact.
    /// 2. **"pinned projects" is read as the pinned THREAD band.** The pinned
    ///    band holds threads; there is no band of pinned projects. The ambiguity
    ///    is harmless because project reorder is being built this round too, so
    ///    whichever he meant, he gets it.
    ///
    /// **THE FACE KEEPS NO PRIVATE ORDER, and this is the trap the design was
    /// written around.** The order belongs to the AUI: `acts::route` turns this
    /// into a `MovePin` naming a NEIGHBOUR's id and the AUI renumbers
    /// (`0053`'s recorded divergence — this face never writes
    /// `.mind/threads.json`). So nothing is reordered locally and nothing is
    /// optimistic: the band on screen keeps showing the order the FILE says
    /// until the AUI has accepted, rewritten it, and this face has re-read it
    /// (`Clears::PinMove`). **A failed act therefore leaves a screen that is
    /// still true** — the row does not move and a sentence says why, which is
    /// the one behaviour an optimistic reorder could not have.
    ///
    /// **WHAT A BURST OF TAPS DOES.** Taps faster than that round trip compute
    /// their neighbour from an order that has not changed yet, so they re-send
    /// the SAME mutation — `move_before` is idempotent in both directions, and
    /// a repeat of a move already applied is a move to where the row already
    /// is. The band therefore advances one position per settled tap and a burst
    /// COLLAPSES rather than corrupting. That is a real limit and it is named
    /// rather than hidden; what it is not is a way to move the wrong row.
    fn move_pin(&mut self, dir: acts::Dir) {
        // **THE PINNED BAND'S OWN PAGE IS WHERE HE WILL MOST WANT THIS** — 26
        // pins against a ten-row band — and it reorders through the same
        // `RootItem::Pinned(n)` resolution, so the page needed no second
        // implementation of it. That is the measured reason the page draws the
        // ROOT'S projection rather than `Mode::List`'s: a List-shaped page
        // would have needed move-pin written a second time over `ThreadList`.
        if !matches!(self.view(), Mode::Projects | Mode::SeeAll(_)) {
            return;
        }
        // **ONLY THE PINNED BAND REORDERS.** A `recent` row can name a pinned
        // thread, and moving it from there would reorder a band he is not
        // looking at — so the aim is the BAND, not the thread's pinned flag,
        // and every other row says what it is instead of doing nothing.
        let n = match self.vs.projects.item(self.root_cursor()) {
            Some(tui_render::RootItem::Pinned(n)) => n,
            Some(tui_render::RootItem::Recent(_)) => {
                self.notice(
                    "reorder works in the pinned band — this is recent",
                    Severity::Unknown,
                );
                return;
            }
            // **THE SAME GESTURE ON A PROJECT ROW REORDERS THE REGISTRY.**
            // `0099`: *"REORDER PROJECTS, by the same grammar as reordering
            // pinned threads — reuse it."* The arithmetic IS reused —
            // `acts::neighbour` is one function with two wordings — and the
            // key is the same key. What differs is the owner: the pinned order
            // is the AUI's and goes out on 9010, and the project order is
            // `~/.cauldron/projects.json`'s array position (`0071` §5), which
            // has no mutation and is written through the crate's own locked
            // update. See `acts::RegistryAct`.
            Some(tui_render::RootItem::Project(n)) => {
                let Some(name) = self.vs.projects.rows.get(n).map(|r| r.name.clone()) else {
                    return;
                };
                self.move_project(name, dir);
                return;
            }
            _ => {
                self.notice(
                    "only a pinned thread or a project can be reordered",
                    Severity::Unknown,
                );
                return;
            }
        };
        let Some(row) = self.vs.projects.pinned.get(n) else {
            return;
        };
        let t = acts::ThreadRef {
            project: row.project.clone(),
            id: row.id.clone(),
            turn_count: 0,
            pinned: true,
            closed: row.closed,
        };
        // Anchor BEFORE firing: the re-read can land on any later event and the
        // anchor has to already be there when it does.
        self.pin_follow = Some(row.id.clone());
        self.fire_for(acts::ThreadAct::Move(dir), t);
    }

    /// **SEND ONE ACT TO ITS OWNER**, and nowhere else.
    ///
    /// The routing decision is not made here — it is made by [`acts::route`],
    /// which is pure and whose table is asserted byte-for-byte without a socket
    /// anywhere near it. This function is only the two `spawn`s the two answers
    /// name.
    fn fire(&mut self, act: ThreadAct) {
        let Some(t) = self.sheet_thread() else { return };
        self.fire_for(act, t);
    }

    /// **THE SAME SEND, FOR A THREAD THAT IS NOT THE SHEET'S.**
    ///
    /// Split out when the root gained a reorder key: `App::fire` resolved its
    /// subject from the open sheet, and a `ctrl-↑` on a band row has no sheet
    /// open at all. Everything below the subject is unchanged and is shared, so
    /// there is still exactly ONE place an act becomes bytes.
    fn fire_for(&mut self, act: ThreadAct, t: acts::ThreadRef) {
        let order = store::pinned_order(&self.store);
        let what = describe(&act);
        // **A MOVE'S ACK IS AN ORDER OF THE FACE'S OWN**: re-read the index,
        // because the AUI has just rewritten it and the band is now stale. See
        // `Clears::PinMove`.
        let clears = matches!(act, ThreadAct::Move(_)).then_some(Clears::PinMove);
        let wire = match acts::route(&act, &t, &order) {
            Ok(w) => w,
            // A refusal about the ACT — it cannot be expressed for this thread.
            // Never a refusal about a socket; that was decided on the row.
            //
            // **AND THE ANCHOR GOES WITH IT.** A refused move never happens, so
            // nothing is coming to follow; leaving the anchor set would pin the
            // cursor to a row for the rest of the session.
            Err(why) => {
                self.pin_follow = None;
                self.notice(why, Severity::Warn);
                return;
            }
        };
        self.send_wire(wire, what, clears);
    }

    /// **SEND ONE PROJECT ACT TO ITS OWNER, WHICH IS ALWAYS THE AUI.**
    ///
    /// The routing is [`acts::route_project`], pure, asserted as bytes. This
    /// function is the spawn and nothing else — which is why the most
    /// destructive acts on this face have no branch of their own here.
    fn fire_project(&mut self, act: ProjectAct, name: String) {
        let what = describe_project(&act, &name);
        let clears = matches!(act, ProjectAct::Create { .. }).then_some(Clears::NewProjectName);
        let wire = match acts::route_project(&act, &name) {
            Ok(w) => w,
            Err(why) => {
                self.notice(why, Severity::Warn);
                return;
            }
        };
        self.send_wire(wire, what, clears);
    }

    // ────────────────────────────────────────────────────────────────────
    //  THE REGISTRY — the acts with no mutation behind them
    // ────────────────────────────────────────────────────────────────────

    /// **REORDER A PROJECT.** The neighbour is computed from the DRAWN order
    /// by the same function the pinned band uses, and the cursor is anchored
    /// to the project by NAME before anything is written — the re-read can
    /// land on any later event and the anchor has to already be there when it
    /// does, exactly as `move_pin`'s is.
    fn move_project(&mut self, name: String, dir: acts::Dir) {
        // The DRAWN order, bucket included — see `acts::route_registry` for
        // why the bucket has to be in it.
        let order: Vec<String> = self
            .vs
            .projects
            .rows
            .iter()
            .map(|r| r.name.clone())
            .collect();
        self.project_follow = Some(name.clone());
        self.fire_registry(acts::RegistryAct::Reorder { name, dir }, &order);
    }

    /// **`p` — PIN THE THREAD UNDER THE CURSOR, OR OPEN THE ADD-A-PROJECT
    /// FIELD.** The ruled collision, resolved by asking `ProjectList::item`
    /// what the cursor is standing on — the ONE cursor resolution on this
    /// surface, shared with `move_pin`, `open_project_sheet` and the hit map.
    ///
    /// The pin half goes through the sheet's own `pin` row, exactly as
    /// `Intent::Rename` goes through the sheet's rename row: one door onto a
    /// thread's acts, so a key and a click cannot mean two different pins.
    fn add_project_or_pin(&mut self) {
        match self.vs.projects.item(self.root_cursor()) {
            Some(tui_render::RootItem::Pinned(_) | tui_render::RootItem::Recent(_)) => {
                self.press_sheet_row(Intent::Options, SheetAct::Pin);
            }
            _ => self.open_new_project(),
        }
    }

    /// **`c` — DE-REGISTER THE PROJECT UNDER THE CURSOR.**
    ///
    /// It presses the project sheet's `remove` row, which ASKS FIRST: the
    /// first press arms the row and says so in words, the second sends. So
    /// `c` `c` de-registers and `c` `enter` does too — `enter` activates the
    /// selected row and the armed row is the selected row. **No second
    /// confirmation grammar is written here**, which is `0099`'s instruction
    /// and the reason this is four lines.
    ///
    /// **THE SECOND PRESS MUST NOT RE-OPEN THE SHEET.** `open_project_sheet`
    /// builds a fresh `SheetState`, and a fresh one is not armed — so a
    /// `deregister` that opened unconditionally would ask first, forever. It
    /// therefore opens only when no project sheet is up, and otherwise goes
    /// straight to the row, which is what makes the second `c` the second
    /// PRESS rather than a second first press.
    ///
    /// A cursor that is not on a project opens no sheet, so this is a silent
    /// miss — the same free miss `o` and `r` take. So is `c` over an open
    /// THREAD sheet: that surface has no project to de-register and the key
    /// is not about it.
    fn deregister_project(&mut self) {
        if self.sheet_project().is_none() {
            if self.sheet.is_some() {
                return;
            }
            self.on_intent(Intent::OpenProjectSheet(self.root_cursor()));
        }
        if let Some(i) = self
            .vs
            .sheet
            .as_ref()
            .and_then(|v| store::sheet_row_for(v, SheetAct::Remove))
        {
            self.on_intent(Intent::SheetAct(i));
        }
    }

    /// Open a sheet through `door`, then press whichever row carries `act`.
    ///
    /// Asking the PROJECTION which row an act is on — `store::sheet_row_for`
    /// — rather than hard-coding an index is what keeps this from drifting
    /// when a sheet's rows change with its subject's state. Factored out of
    /// `Intent::Rename`, which was the first of these.
    fn press_sheet_row(&mut self, door: Intent, act: SheetAct) {
        self.on_intent(door);
        if let Some(i) = self
            .vs
            .sheet
            .as_ref()
            .and_then(|v| store::sheet_row_for(v, act))
        {
            self.on_intent(Intent::SheetAct(i));
        }
    }

    /// **`tab` — COMPLETE THE PATH IN THE ADD-A-PROJECT FIELD.**
    ///
    /// Nothing happens for a bare word: that is a NAME, and there is nothing
    /// to complete it against. Several matches are named in the notice line
    /// rather than drawn as a menu — the honest thing a face with no picker
    /// surface can do, and it is not a silent no-op.
    fn complete_path(&mut self) {
        let Some(c) = self.new_project.as_ref() else {
            return;
        };
        let typed = c.text();
        if !acts::looks_like_a_path(&typed) {
            return;
        }
        let done = index::complete_path(&self.home, &typed);
        if done.matches.is_empty() {
            self.notice("nothing there to add", Severity::Warn);
            return;
        }
        if done.text != typed {
            self.new_project = Some(tui_render::compose::Composer::from_text(&done.text));
        }
        if done.matches.len() > 1 {
            // A bounded sentence: six names is a notice, thirty is a wall.
            let shown: Vec<&str> = done.matches.iter().take(6).map(String::as_str).collect();
            let more = done.matches.len().saturating_sub(shown.len());
            let tail = if more > 0 {
                format!(" … and {more} more")
            } else {
                String::new()
            };
            self.notice(format!("{}{tail}", shown.join("  ")), Severity::Unknown);
        }
    }

    /// **SEND ONE REGISTRY ACT TO ITS OWNER, WHICH IS THE FILE.**
    ///
    /// The routing is `acts::route_registry`, pure and asserted as a value;
    /// the write is `index::apply_registry`, which is one call to
    /// `projects::Registry::update`. This function is the `spawn_blocking` and
    /// nothing else, and it ends in the SAME `Ev::ActDone` the two sockets end
    /// in — so the notice line, the verbatim refusal and the re-read are one
    /// mechanism rather than three.
    fn fire_registry(&mut self, act: acts::RegistryAct, order: &[String]) {
        let what = describe_registry(&act);
        let write = match acts::route_registry(&act, order) {
            Ok(w) => w,
            Err(why) => {
                // A refused move never happens, so nothing is coming to
                // follow; leaving the anchor set would pin the cursor to a row
                // for the rest of the session.
                self.project_follow = None;
                self.notice(why, Severity::Warn);
                return;
            }
        };
        let home = self.home.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            // `Registry::update` takes an advisory lock and does a
            // temp/fsync/rename — blocking work, off the reactor.
            let result = tokio::task::spawn_blocking(move || index::apply_registry(&home, &write))
                .await
                .unwrap_or_else(|e| Err(format!("the write did not finish: {e}")));
            let _ = tx
                .send(Ev::ActDone {
                    what,
                    result,
                    clears: Some(Clears::RegistryWrite),
                })
                .await;
        });
    }

    /// The two spawns the two owners name, and nothing else.
    fn send_wire(&mut self, wire: acts::Wire, what: String, clears: Option<Clears>) {
        self.last_wire = Some(wire.clone());
        let tx = self.tx.clone();
        match wire {
            acts::Wire::Aui(m) => {
                let Some(h) = self.aui.clone() else {
                    self.notice(DownCause::DoorShut.words(), Severity::Warn);
                    return;
                };
                tokio::spawn(async move {
                    let result = h.mutate(m).await;
                    let _ = tx
                        .send(Ev::ActDone {
                            what,
                            result,
                            clears,
                        })
                        .await;
                });
            }
            // **ONE VERB, NOT A METHOD NAME AND A BAG OF PARAMS.** The arm
            // this replaces took a `String` method and a `Value`, which is a
            // door wide enough for a read; `acts::Verb` is three variants and
            // the compiler is what keeps it three.
            acts::Wire::Daemon(verb) => {
                let h = self.handle.clone();
                // **WHICH KEY ASKED** (TC-46): every daemon act reaches here
                // from a sheet row (`→`/`enter`, `hit.rs` `Via::Key` only) or a
                // reorder key; an interrupt only ever from the options sheet's
                // `interrupt the running turn` row. The cancel's diag line
                // carries this.
                let via = format!("options sheet · {what} row (→/enter)");
                tokio::spawn(async move {
                    let result = h.verb_via(verb, &via).await.map(|_| ());
                    let _ = tx
                        .send(Ev::ActDone {
                            what,
                            result,
                            clears,
                        })
                        .await;
                });
            }
        }
    }

    /// **COMMIT THE FIELD.** The text goes with the act; the buffer is kept
    /// until the far end accepts, exactly as the composer's is.
    fn sheet_commit(&mut self) {
        if self.naming.is_some() {
            return self.commit_naming();
        }
        let Some(act) = self.sheet.as_ref().and_then(|s| s.editing) else {
            return;
        };
        let text = self
            .sheet
            .as_mut()
            .map(|s| s.buf(act).text())
            .unwrap_or_default();
        let text = text.trim().to_string();
        // **AN EMPTY BOX IN THE MODAL COMMITS NOTHING AND THE BOX STAYS OPEN.**
        //
        // The hazard is real and the sheet's own comment flags it: a rename
        // carries the summary as its whole payload, so an empty one is
        // `Mutation::SetThreadSummary { summary: "" }` — the act that erases
        // the name. `apps/aui` refuses exactly that, in its own words (*"an
        // empty summary would erase the thread's name"*), and that refusal is
        // the OWNER'S and is not second-guessed here.
        //
        // **WHAT IS DECIDED HERE IS WHEN THE BOX CLOSES, WHICH IS THE FACE'S
        // OWN QUESTION.** On the sheet, committing lands on the sheet's menu
        // and the words stay in the buffer, so a refusal arrives with his
        // typing still in front of him. A MODAL has no menu to land on — if it
        // closed on an empty enter he would be back at his list with a refusal
        // in the notice line and nowhere to correct it. So an empty modal
        // commits nothing, stays open, and says what it wants in one line that
        // names both doors.
        //
        // It is deliberately NOT a local re-statement of the AUI's rule: the
        // sentence is about the BOX (it has nothing in it), not about what a
        // rename means. A non-empty name still goes RAW to the owner, which is
        // `create_project`'s posture and the reason there is one normalizer.
        if self.vs.mode == Mode::RenameModal && text.is_empty() {
            self.notice(
                "the name is empty — type one, or esc to keep the name it has".to_string(),
                Severity::Warn,
            );
            return;
        }
        let payload = (!text.is_empty()).then_some(text);
        if let Some(a) = self.act_for(act, payload) {
            self.fire(a);
        }
        if let Some(s) = self.sheet.as_mut() {
            s.editing = None;
        }
        // **THE MODAL CLOSES ONTO HIS LIST; THE SHEET'S FIELD LANDS ON THE
        // SHEET'S MENU.** One commit, two surfaces, and each owes back the
        // place it was standing over — which for the modal is the whole of
        // *"don't take me to the settings"*: he never went there, so he is not
        // returned there either.
        if self.vs.mode == Mode::RenameModal {
            self.sheet = None;
            self.vs.sheet = None;
            self.vs.mode = self.under;
        } else {
            // Back to the menu. The words stay in the buffer.
            self.vs.mode = Mode::Sheet;
        }
    }

    /// **ONE EDIT INSIDE THE ROOT'S NAME FIELD.**
    ///
    /// A deliberately SMALLER grammar again: a project name is one short
    /// answer, so there is no newline, no vertical motion and no undo — and the
    /// bindings that would produce them are simply not in the table, so this
    /// handler cannot be asked for them.
    fn name_compose(&mut self, edit: tui_render::Edit) {
        use tui_render::Edit as E;
        let Some(c) = self.new_project.as_mut() else {
            return;
        };
        match edit {
            E::Backspace => c.backspace(),
            E::Delete => c.delete(),
            E::Left => c.left(),
            E::Right => c.right(),
            E::WordLeft => c.word_left(),
            E::WordRight => c.word_right(),
            E::RowStart => c.buffer_start(),
            E::RowEnd => c.buffer_end(),
            E::KillToEnd => c.kill_to_line_end(),
            E::KillToStart => c.kill_to_line_start(),
            E::KillWord => c.kill_word_back(),
            // Nothing else is bound in `Mode::NewProject`.
            _ => {}
        }
    }

    /// One edit inside the sheet's field.
    ///
    /// A deliberately SMALLER grammar than `App::compose`: this box has no
    /// queue, no recovered-draft offer, no audio and no send-on-enter, so
    /// reaching into the composer's machinery for it would have coupled the
    /// sheet to four things it does not have.
    fn sheet_compose(&mut self, edit: tui_render::Edit) {
        use tui_render::Edit as E;
        let w = self.compose_width();
        let naming = self.naming.is_some();
        // **THE NAME BOX FIRST** (TUI-P1a): while it is up it is the box he is
        // typing into, whatever sheet is behind it.
        let c = if let Some(n) = self.naming.as_mut() {
            &mut n.buf
        } else {
            let Some(sheet) = self.sheet.as_mut() else {
                return;
            };
            let Some(act) = sheet.editing else { return };
            sheet.buf(act)
        };
        match edit {
            // A NAME is one line: enter commits it (it is bound to the commit,
            // not to this), and no newline can reach the daemon inside one.
            E::Newline if naming => {}
            E::Newline => c.newline(),
            E::Backspace => c.backspace(),
            E::Delete => c.delete(),
            E::Left => c.left(),
            E::Right => c.right(),
            E::WordLeft => c.word_left(),
            E::WordRight => c.word_right(),
            E::Up => {
                c.up(w);
            }
            E::Down => {
                c.down(w);
            }
            E::RowStart => c.row_start(w),
            E::RowEnd => c.row_end(w),
            E::BufStart => c.buffer_start(),
            E::BufEnd => c.buffer_end(),
            E::KillToEnd => c.kill_to_line_end(),
            E::KillToStart => c.kill_to_line_start(),
            E::KillWord => c.kill_word_back(),
            E::Yank => c.yank(),
            E::Undo => {
                c.undo();
            }
            E::Redo => {
                c.redo();
            }
            // Bound in the composer and deliberately NOT in this mode's
            // grammar; reached only if the table gains a row without this arm.
            _ => {}
        }
    }

    /// **AN INVALIDATE MEANS RE-READ THE INDEXES**, debounced.
    ///
    /// `0060`'s "write-side plus a HINT": a successful mutation — this face's,
    /// the phone's, or the AUI's own — broadcasts `Invalidate` to every
    /// connected client, and this face answers by re-reading the files, which
    /// is the SAME door `^R` uses (`App::reload`). That is what makes a pin
    /// made from the phone land here without him pressing anything.
    ///
    /// # THE DESK IS ANNOUNCED NOW, AND THE OLD NOTE HERE WAS OBSOLETE
    ///
    /// This doc used to say: *"nothing outside `apps/aui`'s `v2/` ever calls
    /// `invalidate()`, so a pin made at the DESK is not announced and still
    /// needs the manual reload. Filed by `0060`, not fixed."* **Its literal
    /// half is still true and its conclusion is now false**, which is the
    /// worst shape a stale comment can take — a successor reads it and
    /// re-diagnoses a solved bug.
    ///
    /// Measured at `c0a34cc`, not inferred: `install_index_invalidation`
    /// (`apps/aui/src/v2/mutate.rs`) hangs an observer off `ThreadIndex::save`
    /// itself (`libs/aui-nav/src/threads.rs`), so EVERY durable index write
    /// announces itself — a spoken turn, an agent's `<<<thread_summary>>>`, a
    /// ledger fold, a desk rename — whatever drove it. The emitter is still
    /// lexically inside `v2/`, so the sentence is not a lie; what it concluded
    /// from that fact stopped being true when `0114` shipped.
    ///
    /// # THE DEBOUNCE COALESCES; IT DOES NOT DROP
    ///
    /// One re-read per burst is right. **Achieving it by discarding every
    /// frame after the first was not**, and it is the defect behind *"after
    /// rename of the thread, the main menu wasn't updated"*: the frames in a
    /// burst are not interchangeable, and for a rename the LAST one is the one
    /// that carries the new name. [`Ev::IndexSettled`] holds the producer
    /// ordering that makes that so.
    ///
    /// So a swallowed frame now leaves a DEBT (`invalidate_owed`), the loop
    /// arms a clock for it ([`App::invalidate_deadline`]), and the burst is
    /// paid for exactly once when it settles. Two re-reads per burst at most,
    /// instead of one re-read and a silent loss.
    fn on_invalidate(&mut self, node: &str) {
        if !aui::invalidate_touches_index(node) {
            return;
        }
        let now = std::time::Instant::now();
        if self
            .last_invalidate
            .is_some_and(|t| now.duration_since(t) < INVALIDATE_DEBOUNCE)
        {
            // **COALESCED, NOT DROPPED.** The window is still open, so this
            // frame costs no read now — but it is remembered, and the clock
            // the loop arms from it pays the burst off once it ends.
            self.invalidate_owed = Some(node.to_string());
            return;
        }
        self.reread_indexes(node);
    }

    /// **PAY OFF A COALESCED BURST — the trailing edge, once the window has
    /// closed.**
    ///
    /// Reached only from [`Ev::IndexSettled`], which the loop arms only while
    /// [`App::invalidate_deadline`] is `Some`, which is only true while a
    /// frame has actually been swallowed. A face with nothing outstanding does
    /// no work here and arms no timer to get here.
    fn on_index_settled(&mut self) {
        let Some(node) = self.invalidate_owed.take() else {
            return;
        };
        self.reread_indexes(&node);
    }

    /// **HOW LONG UNTIL A COALESCED BURST IS OWED ITS RE-READ**, or `None`
    /// when nothing was swallowed.
    ///
    /// `pub` for one reason worth stating: the defect this closes was invisible
    /// from inside the piece that works, so the proof has to be able to see
    /// that the clock is ARMED and not merely that a re-read happens when it is
    /// driven by hand. An unarmed clock is exactly how this hid.
    ///
    /// **IT NEVER GOES `None` BECAUSE THE WINDOW HAS PASSED**, which is the
    /// opposite of what [`App::notice_deadline`] does and is deliberate: an
    /// expired notice is also dropped by the RENDERER, so a `None` there is
    /// covered by a second mechanism, and a debt has no second mechanism. An
    /// overdue re-read is owed NOW, so an elapsed window arms the shortest
    /// timer there is rather than disarming.
    ///
    /// Floored at one millisecond rather than zero so the loop never holds a
    /// zero-length timer — `tokio::time::timeout` polls the channel before the
    /// deadline, so a zero there is a timer a busy channel can keep winning.
    pub fn invalidate_deadline(&self) -> Option<Duration> {
        self.invalidate_owed.as_ref()?;
        let since = self.last_invalidate?.elapsed();
        Some(
            INVALIDATE_DEBOUNCE
                .saturating_sub(since)
                .max(Duration::from_millis(1)),
        )
    }

    /// **RE-READ THE INDEX FILES AND PUT THEM ON THE SCREEN.** The one body
    /// both edges of the debounce run, so the leading read and the trailing
    /// read can never come to do different amounts of work.
    fn reread_indexes(&mut self, node: &str) {
        self.last_invalidate = Some(std::time::Instant::now());
        tracing::info!(
            node,
            "9010 invalidated a node this face's index holds — re-reading"
        );
        self.store.load_projects();
        // **AND THE RE-READ HAS TO REACH THE SCREEN.** `load_projects` fills
        // the STORE; `vs.projects` — the thing he looks at — is built only by
        // `project()`, and `apply()` has no projecting tail the way
        // `on_intent` does. Without this line the indexes were re-read into
        // memory and the root kept drawing the previous list until some
        // unrelated keystroke happened to rebuild it.
        //
        // **That was the whole of "the recent band isn't reordering", and it
        // is why a manual reload looked necessary**: `App::reload` reaches the
        // same `load_projects` through `on_intent`, whose tail DOES project,
        // so pressing the key made data that was already in memory appear.
        // Proved by `tests/it/rows_registry_and_rename.rs`, which drives the loop
        // and reads `app.state()`; the unit test beside this one asserts on
        // `app.store` and stayed green throughout.
        self.project();
    }

    /// **AN ACT SUPERSEDES A CONFIRMATION, BUT IT DOES NOT SUPERSEDE A
    /// FAILURE.** The one place the notice area is cleared by something he did
    /// rather than by its own clock.
    ///
    /// Before the info area had a timeout, the next keystroke was the ONLY way
    /// a message ever left the screen, so every message had to go that way.
    /// With a real timeout that rule is not merely redundant, it is harmful:
    /// `paste failed — timed out` and `not sent: …` would both vanish on the
    /// first key he pressed afterwards, which for a paste failure is the SAME
    /// keystroke he presses to try again. Measured on a pty: the failure notice
    /// was on screen for one frame and gone.
    ///
    /// So a QUIET notice — a confirmation about the act he has just replaced —
    /// still goes, and a LOUD one stays for the twelve seconds it was given.
    /// That is the same loud/quiet split [`ttl`] already makes, applied to the
    /// other end of a notice's life.
    fn dismiss_notice(&mut self) {
        let loud = self
            .vs
            .notice
            .as_ref()
            .is_some_and(|n| matches!(n.severity, Severity::Warn | Severity::Critical));
        if !loud {
            self.vs.notice = None;
        }
    }

    /// How long until the standing notice expires, if one is standing.
    ///
    /// The loop waits on this so the frame is redrawn AT the deadline. Without
    /// it a notice would linger on screen until some unrelated event happened
    /// to cause a repaint, which for a face sitting still is "forever" — and a
    /// timeout nobody can observe is not a timeout.
    ///
    /// **`None` once it has already expired**, which is what stops the loop
    /// spinning on a zero-length timer: the `NoticeExpired` arm clears the
    /// field, and a cleared field arms nothing.
    /// **WHEN THE LINK LINE GOES STALE** — `None` when nothing it says moves.
    ///
    /// The renderer owns the answer (`Link::refresh_in`), because the renderer
    /// is what decides how coarsely a duration is spelled; a second opinion
    /// here would be `0023` `defect_classes` CLASS 3 with a clock attached.
    fn link_deadline(&self) -> Option<Duration> {
        self.vs
            .header
            .link
            .refresh_in(Utc::now())?
            .to_std()
            .ok()
            .filter(|d| !d.is_zero())
    }

    fn notice_deadline(&self) -> Option<Duration> {
        let at = self.vs.notice.as_ref()?.expires_at?;
        (at - Utc::now()).to_std().ok().filter(|d| !d.is_zero())
    }

    // ── the on-demand reads ──────────────────────────────────────────────

    /// The thread the queue overlay and `enter` are about.
    fn focused_thread(&self) -> Option<String> {
        match self.view() {
            // **THE ROOT AND THE PAGE ANSWER `None`, both of them.** A band row
            // names a thread, but "the thread in focus" is what the queue
            // overlay is ABOUT, and at a menu there is none — the same answer
            // the root has always given, for the same reason.
            Mode::Projects | Mode::SeeAll(_) => None,
            Mode::List => self
                .vs
                .list
                .rows
                .get(self.list_cursor)
                .map(|r| r.id.clone()),
            _ => self.store.open_id().map(str::to_string),
        }
    }

    // ── ACTIVATION: the one path a click and `→` share ───────────────────

    /// **WHAT THE KEYBOARD IS POINTING AT.** `→` has no coordinate, so this is
    /// where its [`Target`] comes from — and it is the SAME kind of value a
    /// click produces, which is the whole reason the two cannot drift.
    ///
    /// In the two lists it is the cursor's row. In a transcript there is no
    /// cursor — `CON-4` forbids one until an audio mode exists — so it is the
    /// NEWEST FOLDED ROW, which is the one he just read and the only one `→`
    /// could plausibly be about. With nothing folded it is [`Target::Empty`],
    /// and `→` does nothing, silently.
    fn keyboard_target(&self) -> Target {
        // **THE SHEET ANSWERS FOR ITSELF FIRST.** `view()` reports the mode
        // UNDER an overlay, which is right for a viewport and wrong for a
        // cursor: `->` inside the sheet must act on the sheet's row, not on the
        // list it was raised over.
        if let (Mode::Sheet, Some(s)) = (self.vs.mode, self.sheet.as_ref()) {
            return Target::SheetRow(s.cursor);
        }
        // **THE ARCHIVED ROOM ANSWERS FOR ITSELF TOO**, and an empty room has
        // nothing under the cursor, so `->` finds `Empty` and does nothing.
        if self.vs.mode == Mode::Archived {
            return if self.vs.archived.rows.is_empty() {
                Target::Empty
            } else {
                Target::ArchivedRow(self.archived_cursor)
            };
        }
        match self.view() {
            // **AN EMPTY LIST HAS NOTHING UNDER THE CURSOR**, so `→` finds
            // `Empty` and does nothing, silently. The live run caught this:
            // `→` inside a project with no index toasted "no thread to open
            // yet", which is punishing a gesture that was not wrong. A click
            // on the same rows already resolved to `Empty` — this is the
            // keyboard catching up to it, not a second rule.
            // **AN EMPTY PAGE HAS NOTHING UNDER THE CURSOR EITHER.** A band
            // can go empty under him — the row he was standing on closed — and
            // then the page is one sentence, which is not a row `→` is about.
            Mode::Projects | Mode::SeeAll(_) if self.vs.projects.selectable() == 0 => {
                Target::Empty
            }
            Mode::List if self.vs.list.rows.is_empty() => Target::Empty,
            Mode::Projects | Mode::SeeAll(_) => Target::RootRow(self.root_cursor()),
            Mode::List => Target::ThreadRow(self.list_cursor),
            // **THE TREE'S CURSOR ROW IS WHAT `→`/`enter` AIM AT** while the
            // band holds the selection — the same resolution a click on that
            // row would get, through the same `hit::activation`.
            Mode::Thread(Region::Agents) if self.vs.tree.rows.len() > 1 => {
                Target::AgentRow(self.vs.tree.cursor_at())
            }
            Mode::Thread(_) | Mode::Activity => {
                // While follow is off the renderer holds back the arrivals, so
                // the newest row HE CAN SEE is the newest laid-out one.
                let visible = self
                    .vs
                    .blocks
                    .len()
                    .saturating_sub(if self.vs.viewport.follow {
                        0
                    } else {
                        self.vs.viewport.new_since
                    });
                self.vs.blocks[..visible]
                    .iter()
                    .enumerate()
                    .rev()
                    // **THE NEWEST ROW WITH SOMETHING BEHIND IT.**
                    //
                    // `is_expandable` replaces the old `is_folded` reading
                    // here: the projection now COMPUTES whether opening a row
                    // would show him anything, rather than guessing it from a
                    // trailing ellipsis, so `→` can no longer land on a row
                    // where it does nothing. `0047` D6 measured that failure —
                    // 92% of `unknown` payloads were under the clip cap, so the
                    // gesture fired and the row could not change.
                    //
                    // Expansion is PER ROW now, so the "any row is the way
                    // back" clause is gone with the strandedness it guarded
                    // against: a row he opened is expandable (its affordance is
                    // "close"), and `x` still has its own key.
                    .find(|(_, b)| b.is_expandable())
                    .map(|(i, _)| Target::Block(i))
                    .unwrap_or(Target::Empty)
            }
            _ => Target::Overlay,
        }
    }

    /// **THE ONE RESOLUTION.** Ask [`activation`] what this target means in
    /// this mode TO THIS GESTURE, and dispatch the intent it gives back.
    ///
    /// A click calls this with the target it HIT and [`Via::Click`]; `→` calls
    /// it with the target the cursor names and [`Via::Key`]. There is still no
    /// third caller and no arm either gesture cannot see — what changed is that
    /// the two no longer have to MEAN the same thing, which is the operator's
    /// retraction of `DIR-13` and is argued in `hit.rs`'s module header.
    ///
    /// **The cursor move left this method and became `Intent::Select`.** It
    /// used to happen here, silently, before every activation; it is now the
    /// whole of what a click on a plain row does, and a thing that IS the act
    /// belongs in the table rather than beside it.
    fn activate(&mut self, target: Target, via: Via) {
        if let Some(intent) = activation(target, self.vs.mode, via) {
            self.on_intent(intent);
        }
        // A MISS IS FREE. No toast, no bell, no flash — a miss must be
        // indistinguishable from not having clicked.
    }

    /// One click, already resolved to a coordinate of the LAST drawn frame.
    ///
    /// **THE REGION IS SET FROM THE TARGET BEFORE THE ACTIVATION IS RESOLVED**,
    /// in this one place. A pointer says where it is, so a click in the box
    /// must resolve under `Thread(Composer)` and a click on a queued row under
    /// `Thread(Queue)` — otherwise the FIRST click after an arrow moved the
    /// selection would be answered by the wrong grammar, and his next arrow
    /// would act on the band he had just clicked away from.
    ///
    /// A target with no region of its own (the header, the footer, a pad row)
    /// leaves the selection where it is: a miss must be indistinguishable from
    /// not having clicked.
    fn on_click(&mut self, col: u16, row: u16) {
        let target = self.hits.hit(col, row);
        tracing::debug!(col, row, ?target, "click");
        if let (Mode::Thread(_), Some(r)) = (self.vs.mode, target.region()) {
            self.select_region(r);
            if let (Region::Queue, Target::MicRow(i) | Target::MicPlay(i)) = (r, target) {
                self.vs.queue_cursor = i;
            }
            // **A CLICK ON A TREE ROW MOVES THE CURSOR AND DOES NOT DRILL** —
            // the standing retraction's rule for every row that is not a
            // button. `→`/`enter` on it opens its chat, which is the gesture
            // he named.
            if let (Region::Agents, Target::AgentRow(i)) = (r, target) {
                if let Some(row) = self.vs.tree.rows.get(i) {
                    self.tree_cursor = row.id.clone();
                    self.vs.tree.cursor = self.tree_cursor.clone();
                }
            }
        }
        self.activate(target, Via::Click);
    }

    // ── THE DRAG: select, autoscroll, copy ───────────────────────────────

    /// The rows of the CONTENT band in the last drawn frame, as
    /// `(first, last_exclusive)`.
    ///
    /// Read from `tui_render::bands`, which is the one derivation of how a
    /// frame divides — the same rule `redraw` reads its page height from. A
    /// shell that worked out "the transcript starts under the header and the
    /// banner" for itself would be `0023` `defect_classes` CLASS 3 all over
    /// again, and the symptom would be autoscroll triggering one row early
    /// whenever the link was down.
    fn content_band(&self) -> (u16, u16) {
        let (w, h) = self.last_size;
        let b = tui_render::bands(&self.vs, w, h);
        let top = (b.header + b.banner) as u16;
        (top, top + b.content as u16)
    }

    /// **THE BUTTON WENT DOWN.** Anchor the gesture and clear the last
    /// selection; act on nothing.
    ///
    /// The anchor is resolved HERE, against the frame that is on screen at
    /// this instant, and never again. That is the whole reason `Ev::Press`
    /// exists as an event: resolving it later, at the first motion, would
    /// resolve it against a frame a transport arrival may already have
    /// scrolled.
    fn on_press(&mut self, col: u16, row: u16) {
        self.vs.selection = None;
        // `Toward::Down` — a press on the blank space above a short transcript
        // means "from the top of the content", which is what a terminal does
        // and what makes a sweep of the whole screen work.
        let anchor = self.hits.pos_at(col, row, Toward::Down);
        tracing::debug!(col, row, ?anchor, "press");
        self.drag = anchor.map(|anchor| Drag {
            anchor,
            from_row: row,
            cell: (col, row),
            autoscroll: None,
        });
    }

    /// **MOTION WITH THE BUTTON HELD, OR AN AUTOSCROLL TICK.** Move the head.
    ///
    /// The head is re-resolved from the CURRENT frame every time, which is
    /// what makes autoscroll correct rather than merely animated: the pointer
    /// stays at one cell, the content moves under it, and the head walks
    /// through the content exactly as far as it scrolled.
    fn extend_to(&mut self, col: u16, row: u16) {
        let Some(mut drag) = self.drag else { return };
        drag.cell = (col, row);
        // A head that lands on blank space looks for text in the direction the
        // selection is GROWING: upwards past the top of a short transcript
        // means "down into the first row", and the other way at the bottom.
        let toward = if row < drag.from_row {
            Toward::Down
        } else {
            Toward::Up
        };
        if let Some(head) = self.hits.pos_at(col, row, toward) {
            self.vs.selection = Some(Selection::new(drag.anchor, head));
        }
        let (top, bottom) = self.content_band();
        drag.autoscroll = if bottom <= top {
            None
        } else if row <= top {
            Some(true)
        } else if row + 1 >= bottom {
            Some(false)
        } else {
            None
        };
        self.drag = Some(drag);
    }

    /// One autoscroll tick: read where the pointer is against the frame that
    /// is up NOW, then move the band one row under it.
    fn on_autoscroll(&mut self) {
        let Some(drag) = self.drag else { return };
        let Some(up) = drag.autoscroll else { return };
        let (col, row) = drag.cell;
        self.extend_to(col, row);
        self.scroll_band(up);
        self.project();
    }

    /// Move the content band one row, WITHOUT moving a list's cursor.
    ///
    /// Deliberately not [`App::up`]/[`App::down`]: in the two list views those
    /// move the CURSOR, and a drag that moved the cursor would change what `→`
    /// opens as a side effect of copying text. Scrolling to see more of what
    /// you are selecting is not navigation.
    fn scroll_band(&mut self, up: bool) {
        self.scroll_band_by(up, 1)
    }

    /// **N ROWS IN ONE STEP**, and it is not an optimisation of the one-row
    /// case — it is the one-row case's general form, with the same clamps.
    ///
    /// *"Scrolling fast should actually scroll fast. It just scrolls slowly for
    /// a long time."* A flick of the trackpad is forty wheel reports out of one
    /// or two `read()`s; walking them one at a time meant forty freezes, forty
    /// clamps and — the part that actually costs — forty full re-projections of
    /// a four-thousand-row tail. The viewport always ENDED in the right place,
    /// which is why nothing caught it; what he could see was the face still
    /// working long after his hand had stopped.
    ///
    /// **Saturating in both directions is what makes a run equal to its
    /// steps.** Forty steps that each saturate at `0` land exactly where one
    /// step of forty does; `on_input` keeps runs monotone so the `min(max)`
    /// clamp cannot behave differently either.
    fn scroll_band_by(&mut self, up: bool, n: usize) {
        if n == 0 {
            return;
        }
        let rows = self.last_rows.max(1);
        // A list knows its own length, so its scroll can be clamped here; a
        // transcript's row count is the renderer's (a message is many rows),
        // and the renderer clamps it there.
        let max = if self.is_list() {
            self.list_len().saturating_sub(rows)
        } else {
            usize::MAX
        };
        if up {
            self.freeze();
            let vp = self.vp_mut();
            vp.from_bottom = vp.from_bottom.min(max).saturating_add(n).min(max);
            vp.follow = false;
        } else {
            let vp = self.vp_mut();
            vp.from_bottom = vp.from_bottom.min(max).saturating_sub(n);
            if vp.from_bottom == 0 {
                *vp = Viewport::at_bottom();
                self.thaw();
            }
        }
    }

    /// **AN ARROW, RESOLVED AGAINST THE REGION THAT HOLDS THE SELECTION.**
    ///
    /// The one rule, stated once and applied to every region: **an arrow leaves
    /// its region only when its region has nothing left to move through in
    /// that direction.** In the chat that edge is the newest row; in the queue
    /// it is the first and last queued message; in the composer it is the
    /// first and last visual row (and that edge is in [`App::compose`], where
    /// the caret is). Nothing here is a special case for the textarea — the
    /// textarea is one region obeying the same sentence.
    ///
    /// The WHEEL does not come through here, deliberately: it has a position,
    /// so it scrolls the band it is over whichever region holds the keyboard.
    fn scroll_key(&mut self, up: bool, n: usize) {
        match self.region() {
            Some(Region::Queue) => {
                let last = self.queue_len().saturating_sub(1);
                let at = self.vs.queue_cursor.min(last);
                if up {
                    match at.checked_sub(n) {
                        Some(next) => self.vs.queue_cursor = next,
                        // Row 0 with nowhere above: up into the chat.
                        None if at == 0 => self.step_region(false),
                        None => self.vs.queue_cursor = 0,
                    }
                } else if at >= last {
                    // Past the last queued message: down into the box.
                    self.step_region(true);
                } else {
                    self.vs.queue_cursor = at.saturating_add(n).min(last);
                }
            }
            // **THE TREE'S CURSOR, WITH THE SAME SENTENCE THE QUEUE ANSWERS
            // TO**: move through the rows, and leave only at an end. `↑` off
            // row 0 — the parent — goes back up into the box; `↓` past the
            // last row does nothing, because the tree is the last stop.
            Some(Region::Agents) => {
                let last = self.vs.tree.rows.len().saturating_sub(1);
                let at = self.vs.tree.cursor_at();
                let next = if up {
                    match at.checked_sub(n) {
                        None if at == 0 => {
                            self.step_region(false);
                            return;
                        }
                        None => 0,
                        Some(next) => next,
                    }
                } else {
                    at.saturating_add(n).min(last)
                };
                if let Some(row) = self.vs.tree.rows.get(next) {
                    self.tree_cursor = row.id.clone();
                    self.vs.tree.cursor = self.tree_cursor.clone();
                }
            }
            // **AT THE NEWEST ROW, `↓` LEAVES THE CHAT.** The symmetric half of
            // the composer's top-edge rule, and without it the chat has no
            // keyboard exit downward at all. At the TOP, `↑` does nothing: the
            // chat is the first stop and there is nothing above it.
            Some(Region::Chat) if !up && self.vp().from_bottom == 0 => self.step_region(true),
            _ => self.scroll_run(up, n),
        }
    }

    /// **ONE SCROLL OF N ROWS — the single implementation behind the arrow
    /// keys, the page keys and the wheel.**
    ///
    /// It was three: the `?` overlay's own offset, the wheel's direct call into
    /// the band, and `up`/`down` for everything else. Each was reached from a
    /// different place in `on_intent` and the wheel reached none of them, which
    /// is how a fast flick came to cost one projection per notch while `pgdn`
    /// cost one per screen. Here they are one function that takes a COUNT, so
    /// "coalesce the wheel" is a smaller number of calls rather than a fourth
    /// code path.
    ///
    /// **It does NOT project.** `on_intent` projects once at its end and the
    /// wheel's flush projects once per run, which is the whole point.
    fn scroll_run(&mut self, up: bool, n: usize) {
        if n == 0 {
            return;
        }
        // **MOVING THE CURSOR ENDS A PIN FOLLOW.** This is the one funnel every
        // cursor motion at the root goes through — `↑`, `↓`, page, wheel — so
        // releasing the anchor here is releasing it everywhere, rather than in
        // five arms of the intent match where the fifth would be the one
        // forgotten. Without it the cursor would refuse to leave the row he
        // just moved.
        self.release_pin_follow();
        // **THE `?` OVERLAY SCROLLS ITSELF, AND THIS IS WHY IT CANNOT SCROLL
        // THE VIEW UNDERNEATH AGAIN.** That was the live defect `0036` deleted
        // the keys over; the keys are back because the overlay no longer fits
        // (a thread's grammar is forty bindings), and this is the one place
        // that could reintroduce the bug.
        if self.vs.mode == Mode::Help {
            self.vs.help_from = if up {
                self.vs.help_from.saturating_sub(n)
            } else {
                self.vs.help_from.saturating_add(n)
            };
            return;
        }
        // A transcript moves its VIEWPORT and a list moves its CURSOR — the
        // distinction `up`/`down` already draw, kept here so that a wheel over
        // a chat transcript means what a wheel means everywhere, and a wheel
        // over a list moves the thing `→` would open.
        if matches!(self.view(), Mode::Thread(_)) {
            self.scroll_band_by(up, n);
        } else if up {
            self.up(n);
        } else {
            self.down(n);
        }
    }

    /// **THE BUTTON CAME UP AFTER A DRAG: copy what is selected.**
    ///
    /// The text comes from `tui_render::selected_text`, which reads CONTENT
    /// units out of the `ViewState` and only chrome out of the frame — so a
    /// selection that autoscroll carried off the top of the screen copies in
    /// full, and what lands on the clipboard is the logical text rather than
    /// the painted screen: no gutter, no `▌`/`▐` rails, no hard wraps.
    ///
    /// The highlight is KEPT after the release. He should be able to see what
    /// he took; the next press or the next keystroke clears it.
    fn finish_drag(&mut self, col: u16, row: u16) {
        self.extend_to(col, row);
        self.drag = None;
        let Some(sel) = self.vs.selection else { return };
        let text = tui_render::selected_text(&self.vs, &self.hits.texts(), sel);
        if text.is_empty() {
            // A drag across padding. Nothing was selected, so nothing is said
            // about it — the same rule a missed click obeys.
            self.vs.selection = None;
            return;
        }
        tracing::debug!(chars = text.chars().count(), "drag copy");
        self.copy_owed = Some(text);
    }

    /// Perform the copy [`App::finish_drag`] decided on, and say what happened.
    fn pay_the_clipboard(&mut self) {
        let Some(text) = self.copy_owed.take() else {
            return;
        };
        match term::copy_to_clipboard(&text) {
            Ok(done) => {
                let sev = if done.is_warning() {
                    Severity::Warn
                } else {
                    Severity::Ok
                };
                self.notice(done.notice(), sev);
            }
            // A refusal that names the ceiling, rather than a copy tmux
            // discards without a word — which is the failure mode being fixed.
            Err(why) => self.notice(why, Severity::Critical),
        }
        // Set-point: `copy_to_clipboard` writes an escape sequence STRAIGHT at
        // the terminal, around ratatui, from inside raw mode. If any part of
        // it is not consumed — an old tmux, a terminal that does not know
        // `OSC 52`, a passthrough that was not forwarded — the residue lands
        // on the screen as text, and a differential renderer agrees with every
        // cell of it forever.
        self.force_repaint("a clipboard sequence went straight at the terminal");
    }

    /// Drop the highlight. Called by the next gesture and by any keystroke,
    /// because a selection nothing can act on is decoration.
    fn clear_selection(&mut self) {
        self.vs.selection = None;
    }

    fn open_project(&mut self, i: usize) {
        self.store.enter_project(i);
        self.descend(Mode::List);
        // A project opens at its NEWEST thread, which is the bottom.
        self.list_cursor = usize::MAX;
        self.list_vp = Viewport::at_bottom();
    }

    /// **DRILL INTO A THREAD FROM WHEREVER HE IS**, remembering the surface he
    /// drilled from so `esc` can give it back.
    ///
    /// # What this function used to be, and why the old reasoning was wrong
    ///
    /// It was `open_band_thread`, it called `store.enter_project` on the way
    /// through, and its doc comment argued that a `self.under = Mode::List`
    /// write was what made the depth come back. **Both halves were wrong**,
    /// measured in `0073`:
    ///
    ///   * the `under` write was DEAD — `open_thread` overwrote it with
    ///     `Mode::Thread` seven lines later, so a builder who "fixed" that line
    ///     would have watched the bug survive;
    ///   * and `enter_project` was the actual teleport. It left a project open
    ///     in the store that he never entered, so `esc` delivered him onto that
    ///     project's whole thread list (frames A3/C3) — *"it brings me back to
    ///     the list of all raw threads"* — and `Tab` out of the event lane
    ///     delivered him there too, with no escape pressed at all (frames
    ///     E8-E10).
    ///
    /// A band row names its own project on the row itself, and the thread view
    /// reads nothing from `open_project`, so entering one was never needed for
    /// the DRILL. It was only ever there to arrange the way back — and the way
    /// back is now [`App::origin`], which remembers the truth instead of
    /// reconstructing a plausible one.
    fn drill_to_thread(&mut self, id: String) {
        self.origin.push(self.view());
        self.parents.clear();
        self.open_thread(id);
    }

    /// **`→` ON A THREAD ROW EXPANDS IT AND ENTERS IT — ONE KEY** (TUI-P1b).
    ///
    /// The row may be anywhere in the tree kept to the root: a child, a
    /// grandchild under a fold he opened, a sibling, an ancestor. The way back
    /// ([`App::parents`]) is set to the tree's own path to it, so `←` climbs
    /// the same tree he is looking at. The folds, the siblings and the
    /// ancestors are untouched — the tree is never rebuilt from scratch — and
    /// the selection stays IN the tree so the next `→` goes one deeper.
    fn enter_tree_row(&mut self, i: usize) {
        let Some(open) = self.store.open_id().map(str::to_string) else {
            return;
        };
        let chain: Vec<String> = tree_chain(&self.vs.tree.rows, i, &open)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let Some(target) = chain.last().cloned() else {
            return;
        };
        // EXPAND: the fold is his, and it outlives this visit.
        self.tree_ctx.expanded.insert(target.clone());
        if target == open {
            return;
        }
        // ENTER: the way back is the tree's own path to the row.
        self.parents = chain
            .windows(2)
            .map(|w| (w[0].clone(), TreeId::Thread(w[1].clone())))
            .collect();
        self.open_thread(target);
        self.tree_subject = TreeId::Parent;
        self.tree_cursor = TreeId::Parent;
        self.vs.mode = Mode::Thread(Region::Agents);
    }

    /// **`←` FROM A CHILD RETURNS TO ITS PARENT**, with the tree cursor on the
    /// child he left. Returns false when he did not come down through one.
    fn back_to_parent(&mut self) -> bool {
        let Some((parent, row)) = self.parents.pop() else {
            return false;
        };
        self.open_thread(parent);
        self.tree_subject = TreeId::Parent;
        self.tree_cursor = row;
        self.vs.mode = Mode::Thread(Region::Agents);
        true
    }

    /// Open a thread IN PLACE. Deliberately **not** a door: it records no
    /// origin, because `reload` and `App::open` both come through here and
    /// neither is a descent.
    fn open_thread(&mut self, id: String) {
        // Whatever composer belongs to the thread we are LEAVING goes back in
        // the drawer before the store forgets which thread that was.
        self.park_composer();
        self.store.open(&id);
        self.restore_composer(&id, Utc::now());
        // **A THREAD OPENS WITH THE SELECTION IN THE BOX** — today's
        // implicit focus, made explicit. Everything he can do on this screen
        // is one arrow away from here.
        self.vs.mode = Mode::Thread(Region::Composer);
        self.vs.queue_cursor = 0;
        self.thread_vp = Viewport::at_bottom();
        self.thread_frozen = None;
        // **NO `last_seq` TO LOOK UP.** It was asked for so the `history`
        // paging could start at `last_seq - OPEN_CAP` instead of at the HEAD of
        // the thread; `threads/log` IS the tail, 4,000 rows of it, so there is
        // no cursor to compute and nothing to compute it from.
        mirror::spawn_transcript(self.handle.clone(), self.tx.clone(), id);
    }

    fn load_queue(&mut self) {
        match self.focused_thread() {
            Some(id) => {
                self.store.queue_loading(&id);
                queue_reads::spawn_queue(self.handle.clone(), self.tx.clone(), id);
            }
            None => self.store.queue_idle(),
        }
    }

    fn reload(&mut self) {
        match self.view() {
            Mode::Thread(_) => {
                if let Some(id) = self.store.open_id().map(str::to_string) {
                    self.open_thread(id);
                }
                self.notice("re-asking the daemon", Severity::Calm);
            }
            _ => {
                // **ONE SOURCE NOW, AND THE OTHER ONE CANNOT BE STALE.**
                //
                // This used to re-read the disk indexes AND re-ask the daemon
                // (`threads/status` + `threads/list`), because the daemon half
                // went stale between repaints. It cannot any more: the fleet is
                // a mirror the producer pushes, so there is nothing to re-ask
                // and `^R` would have nothing to send. What it still does is
                // re-read the two FILES — `~/.cauldron/projects.json` and
                // `.mind/threads.json` — which nobody pushes and which pick up
                // a project he created or a header the AUI just wrote.
                //
                // The notice says exactly that, rather than claiming a
                // round trip that no longer happens.
                self.store.load_projects();
                self.notice("re-read the indexes · the fleet is a mirror", Severity::Calm);
            }
        }
    }

    // ── THE COMPOSER ─────────────────────────────────────────────────────

    /// **ONE TERMINAL READ'S WORTH OF INPUT, AND THE PASTE RULE IS APPLIED
    /// BEFORE ANYTHING IS LOOKED AT.**
    ///
    /// `input::defuse` rewrites the batch so that nothing a paste dragged in
    /// with it can act — see its doc comment for the defect, which was
    /// reproduced on a pty and is the reason this method takes a batch at all
    /// rather than an event. By the time the loop below runs, an `enter` that
    /// was really the tail of a corrupted paste is no longer an `enter`: it is
    /// a newline inside the one `Input::Paste` the whole batch collapsed into.
    /// **THE DOWN PAGE, OR NOTHING** (R7.S1c). `Some` exactly while the link
    /// is down — an outage has a `down_since`, which only getting all the way
    /// up clears (`Store::apply`) — so the page arrives on the first failed
    /// attempt and leaves on the first frame the link is up, with no key.
    fn down_page(&self) -> Option<tui_render::DownPage> {
        let link = &self.vs.header.link;
        if matches!(link.state, LinkState::Connected) || link.down_since.is_none() {
            return None;
        }
        let r = crate::cauldron_env::resolved();
        let env = r.map(|r| r.env.clone()).unwrap_or_else(|| "?".into());
        let mut cmd = vec!["cauldron".to_string()];
        cmd.extend(crate::cauldron_env::restart_args(&env));
        Some(tui_render::DownPage {
            service: crate::cauldron_env::SERVICE.into(),
            env: env.clone(),
            addr: r
                .map(|r| r.addr.clone())
                .unwrap_or_else(|| crate::seam::link::ws_url()),
            source: r.map(|r| r.source.describe()).unwrap_or_else(|| "not resolved".into()),
            comes_back: comes_back(&env, r.and_then(|r| r.auto_restart)),
            restart_cmd: cmd.join(" "),
            restart_key: DOWN_RESTART_KEY,
            restart_note: if self.restarting {
                Some("running…".into())
            } else {
                self.restart_note.clone()
            },
        })
    }

    /// **THE RESTART BUTTON: `cauldron --env <name> start threads`.**
    ///
    /// Run off the loop, and its result comes back as [`Ev::DownRestart`]. In
    /// an environment a cauldron daemon holds, `start` is a no-op that says the
    /// holder is the one that launches (and the holder is already restarting
    /// it); in one nobody holds, it launches threads on the environment's
    /// address. Either way the face does not reconnect by itself here — the
    /// seam's ladder does, the moment the port answers.
    fn restart_daemon(&mut self) {
        if self.restarting {
            return;
        }
        let Some(r) = crate::cauldron_env::resolved() else {
            self.restart_note = Some("no cauldron environment was resolved at startup".into());
            self.project();
            return;
        };
        self.restarting = true;
        self.project();
        let bin = crate::cauldron_env::cauldron_bin();
        let args = crate::cauldron_env::restart_args(&r.env);
        let root = r.root.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let note = tokio::task::spawn_blocking(move || {
                match std::process::Command::new(&bin)
                    .args(&args)
                    .env("CAULDRON_HOME", &root)
                    .stdin(std::process::Stdio::null())
                    .output()
                {
                    Err(e) => format!("could not run {}: {e}", bin.display()),
                    Ok(o) => {
                        let text = format!(
                            "{}{}",
                            String::from_utf8_lossy(&o.stdout),
                            String::from_utf8_lossy(&o.stderr)
                        );
                        // The banner line is first; the line that SAYS what
                        // happened is the first that is not the banner.
                        let said = text
                            .lines()
                            .map(str::trim)
                            .find(|l| !l.is_empty() && !l.starts_with("env "))
                            .unwrap_or("")
                            .to_string();
                        let code = o.status.code().map(|c| c.to_string()).unwrap_or("signal".into());
                        format!("exit {code}: {said}")
                    }
                }
            })
            .await
            .unwrap_or_else(|e| format!("the restart did not finish: {e}"));
            let _ = tx.send(Ev::DownRestart(note)).await;
        });
    }

    fn on_input(&mut self, batch: Vec<crate::event::Input>) {
        self.on_input_at(batch, std::time::Instant::now())
    }

    /// [`App::on_input`] with the clock handed in, so the wheel's velocity
    /// curve can be driven from a test without a sleep.
    ///
    /// **ONE instant for the whole batch, and that is the correct reading
    /// rather than a convenient one.** Every notch in a `read()` really did
    /// arrive in that read; charging a reversal mid-batch as though time had
    /// passed between the two runs would make a flick that wobbled read as two
    /// slower gestures.
    fn on_input_at(&mut self, batch: Vec<crate::event::Input>, now: std::time::Instant) {
        use crate::event::Input;
        let batch = crate::input::defuse(batch);
        self.paste_notice(&batch);
        // **THE WHEEL COALESCES INTO RUNS, AND NOTHING ELSE MOVES.**
        //
        // *"Scrolling fast should actually scroll fast. It just scrolls slowly
        // for a long time."* A flick is forty wheel reports out of one or two
        // `read()`s, and each one was a `scroll_band` plus a FULL re-projection
        // of the tail. `pending` accumulates consecutive notches going the same
        // way and `flush_wheel` spends them in one step, with one projection.
        //
        // **A RUN, not a net sum, and the difference is the clamps.** Thirty up
        // then five down is not "twenty-five up": in a list the thirty saturate
        // against the top first, so the five down start from the clamp. Runs
        // are monotone by construction, so a run of `n` lands exactly where `n`
        // single steps would — and a reversal simply ends one run and starts
        // the next, which costs two projections instead of forty.
        //
        // **And it never reorders.** Anything that is not a wheel notch flushes
        // the run BEFORE it is handled, so a keystroke that arrived in the
        // middle of a flick still happens in the middle of it.
        let mut pending: Option<(bool, usize)> = None;
        for item in batch {
            if !matches!(item, Input::Wheel { .. }) {
                self.flush_wheel(&mut pending, now);
            }
            match item {
                Input::Key(k) => {
                    self.dismiss_notice();
                    // A keystroke ends a selection. It is transient by design
                    // (`CON-4`), and a highlight left standing under a keyboard
                    // that is doing something else is decoration.
                    self.clear_selection();
                    if let Some(key) = to_key(k) {
                        // **THE DOWN PAGE OWNS THE KEYBOARD WHILE IT IS UP**
                        // (R7.S1c), except for leaving: its one key is the
                        // restart button, a quit still quits, and anything
                        // else is swallowed rather than typed into a composer
                        // the page is covering.
                        if self.vs.down.is_some() {
                            if key == tui_render::Key::Char(DOWN_RESTART_KEY) {
                                self.restart_daemon();
                            } else {
                                let intent = self.resolve(key);
                                if matches!(intent, Intent::Quit) {
                                    self.on_intent(intent);
                                }
                            }
                            continue;
                        }
                        // **A STANDING DRAFT OFFER IS A MODAL QUESTION, and
                        // `y`/`n` answer it before the table is asked.**
                        //
                        // `AcceptDraft`/`DiscardDraft` used to be `opt-y` and
                        // `opt-d`, which macOS composes into `¥` and `∂` — they
                        // have never worked on his keyboard. `0128` found them
                        // no mnemonic ctrl home either (`^Y` is yank, `^D` is
                        // delete, both readline inside a textarea) and its
                        // answer is better than any letter: the offer is only
                        // live while it is ON SCREEN, so answering it costs no
                        // namespace at all.
                        //
                        // **IT IS NOT A ROW OF `BINDINGS` and must not be.** A
                        // row would claim `y` and `n` unconditionally, in the
                        // one mode where every letter is text. The keys live in
                        // `keys::DRAFT_OFFER_*`, which the band that DRAWS the
                        // offer reads too, so the words on screen and the keys
                        // that work come from one place.
                        //
                        // Anything else still types, and an edit still answers
                        // the offer by implication — see `App::compose`.
                        let intent = self.resolve(key);
                        self.on_intent(intent);
                    }
                }
                Input::Paste(s) => {
                    self.dismiss_notice();
                    self.clear_selection();
                    self.on_paste(&s);
                }
                Input::Press { col, row } => {
                    self.dismiss_notice();
                    self.on_press(col, row);
                }
                Input::Click { col, row } => {
                    self.dismiss_notice();
                    self.on_click(col, row);
                }
                Input::Drag { col, row } => self.extend_to(col, row),
                Input::DragEnd { col, row } => self.finish_drag(col, row),
                // A wheel notch resolves straight to the intent the arrows
                // carry in a LIST. It does NOT go through the key table: the
                // table speaks keystrokes, and a `Key::WheelUp` would be a
                // fiction the `?` overlay would then have to render. In a
                // THREAD the arrows belong to the composer, so the wheel is
                // routed to the transcript directly — which is what a wheel
                // over a chat transcript means everywhere.
                Input::Wheel { up } => match pending {
                    // Same direction: one more notch on the run in hand.
                    Some((dir, ref mut n)) if dir == up => *n += 1,
                    // A REVERSAL ends the run, because the clamps at each end
                    // are what make a run equal to its steps.
                    Some(_) => {
                        self.flush_wheel(&mut pending, now);
                        pending = Some((up, 1));
                    }
                    None => pending = Some((up, 1)),
                },
                // Set-point 2.
                Input::Resize => self.force_repaint("resize"),
                // The three paste STATE markers. They are not edits and they
                // are not keystrokes; everything they do to the face is the
                // notice `paste_notice` already decided, above, over the whole
                // batch.
                Input::PasteBegan | Input::PasteFailed | Input::PasteHealed => {}
                // **THE ENHANCED-KEY CAPABILITY, TURNED ON BY THE TERMINAL'S
                // OWN ANSWER.** Everything the surface says about keys is
                // filtered through `Caps`, so this is the moment `shift-enter`
                // stops being withheld — and the repaint is not optional, since
                // the footer and the `?` overlay are both rendered FROM it.
                Input::EnhancedKeys => {
                    if !self.vs.caps.enhanced_keys {
                        term::push_kitty();
                        self.vs.caps.enhanced_keys = true;
                        self.force_repaint("the terminal answered the keyboard query");
                    }
                }
            }
        }
        self.flush_wheel(&mut pending, now);
    }

    /// Spend an accumulated run of wheel notches: ONE scroll, ONE projection.
    ///
    /// Takes the run by `&mut` and clears it, so there is no way to flush
    /// without also emptying the accumulator — spending a run twice would
    /// double every flick, and it would look like an over-sensitive trackpad
    /// rather than like a bug.
    fn flush_wheel(&mut self, pending: &mut Option<(bool, usize)>, now: std::time::Instant) {
        let Some((up, n)) = pending.take() else {
            return;
        };
        // **THE RUN'S DISTANCE IS ITS NOTCHES TIMES A FUNCTION OF HOW FAST
        // THEY CAME**, and the multiplier is the only part of this line that
        // changed when the constant went — see [`WheelMeter`] for the curve and
        // [`WHEEL_TAU`] for why a constant could not do this job.
        //
        // **THIS IS THE ONE MOUNT POINT.** `scroll_run` is where a transcript's
        // viewport parts company with a menu's cursor and with the `?`
        // overlay's own offset; charging the meter HERE, above that fork, is
        // what makes "the menu scrolls by velocity too" a property of the
        // existing funnel rather than a second implementation of it.
        //
        // The meter is charged exactly once per RUN, which is also once per
        // projection — so the coalescing `0052` bought is untouched: a
        // forty-notch flick is still one `scroll_run` and one `project`.
        let rows = self.wheel.rows(n, now);
        // **THE FACE SAYS WHAT IT MOVED, and this line is the only way the rate
        // table is measurable at all.** Every unit test of the curve hands
        // `on_input_at` a SYNTHETIC instant — it has to, or it would have to
        // sleep — so those tests prove the arithmetic and cannot prove that
        // real wheel reports, batched by whatever one terminal `read()` hands
        // us, charge the meter the way the arithmetic assumes. The batching IS
        // the input. `wheel_rate_probe.sh` drives the real binary over a real
        // pty at graded rates and reads these lines back, which makes the table
        // a MEASUREMENT rather than a restatement of the formula.
        //
        // It is also the knob's instrument: the three constants above are meant
        // to be re-tunable, and `TUI_LOG=tui::app=debug` on his own face prints
        // what each of his own flicks was worth. `debug`, so the default `info`
        // filter never pays for it, and one line per RUN rather than per notch
        // — the same coalescing `flush_wheel` exists for.
        tracing::debug!(notches = n, rows, gain = self.wheel.gain(), "wheel_run");
        self.scroll_run(up, rows);
        self.project();
    }

    /// **WHAT A PASTE SAYS WHILE IT IS HAPPENING — `IS-38`'s visible half.**
    ///
    /// His instruction: *"Show one when paste is in progress, and if it fails
    /// via timeout then say so."*
    ///
    /// **Decided over the WHOLE batch rather than event by event, and that is
    /// what keeps a normal paste silent.** A paste that completes inside one
    /// read delivers `PasteBegan` and its `Paste` in the same batch; walking
    /// the batch in order would set the in-progress notice and then have to
    /// unset it, which is a state the screen can catch between two frames.
    /// Asking "what does this batch, as a whole, say about a paste" cannot.
    ///
    /// So the ONLY way "paste in progress…" reaches the screen is a batch that
    /// opened a paste and did not close it — which is precisely the case he
    /// asked to be told about, and never the fast path.
    fn paste_notice(&mut self, batch: &[crate::event::Input]) {
        use crate::event::Input;
        let has = |f: fn(&Input) -> bool| batch.iter().any(f);
        if has(|i| matches!(i, Input::PasteFailed)) {
            // Said in HIS terms — what was lost and what to do — rather than in
            // the parser's. Nothing arrived, so nothing was dropped.
            self.notice(
                "paste failed — timed out with nothing received; try again",
                Severity::Warn,
            );
        } else if has(|i| matches!(i, Input::PasteHealed)) {
            // The paste LANDED; the terminal simply never said it was over.
            // Honesty rule: say what actually happened rather than letting a
            // clipboard appear out of a second of silence with no explanation.
            self.notice(
                "paste completed — the terminal never sent an end marker, so it \
                 was closed after a 1s gap",
                Severity::Unknown,
            );
        } else if has(|i| matches!(i, Input::Paste(_))) {
            // A paste resolved normally. Whatever the in-progress notice said
            // is now stale; `on_paste` posts its own if the paste was big.
        } else if has(|i| matches!(i, Input::PasteBegan)) {
            // Capped just past the longest the parser can hold a paste open, so
            // this notice can never outlive the thing it describes even if the
            // batch that resolves it is somehow lost.
            self.notice_for(
                "paste in progress…",
                Severity::Calm,
                crate::vt::PASTE_SILENT + Duration::from_secs(1),
            );
        }
    }

    /// A paste, already normalized by the batch rule and about to be normalized
    /// again on the way into the buffer.
    ///
    /// Outside a thread there is nothing to paste INTO, and a paste that
    /// silently did nothing would be indistinguishable from one that was
    /// swallowed — so it says so.
    fn on_paste(&mut self, s: &str) {
        // The printable rule, for the gesture that delivers thousands at once.
        if self.composer_is_stood_in() {
            self.refuse_to_send();
            return;
        }
        // **A PASTE IS TYPING WITH MORE CHARACTERS IN IT, AND IT OBEYS THE
        // SAME RULE.** It used to land in the box from anywhere and drag the
        // selection in with it; under *"the textarea should be inactive when
        // the user cursor is outside of the textarea"* it does not.
        //
        // **AND THIS IS THE PRODUCER, not a second copy of the key table's
        // gate.** A paste arrives as its own `Input` and never passes through
        // `intent()`, so there is no table row to delete for it — this
        // function IS the boundary, and [`Mode::textarea_is_live`] is the same
        // fact the table's rows were deleted to state.
        //
        // It is never SILENT: a clipboard that vanished with no word said is
        // indistinguishable from one the terminal swallowed, which is the
        // failure `paste_notice` exists for.
        if !self.vs.mode.textarea_is_live() {
            if !s.trim().is_empty() {
                self.notice(
                    if matches!(self.vs.mode, Mode::Thread(_)) {
                        "the message box does not have the selection — esc puts it back, then paste"
                    } else {
                        "nothing to paste into here — open a thread"
                    },
                    Severity::Unknown,
                );
            }
            return;
        }
        if self.vs.composer.offer.is_some() {
            self.answer_offer(false);
        }
        let chars = s.chars().count();
        self.vs.composer.buf.insert_paste(s);
        self.touch_draft();
        // A paste big enough to be worth remarking on says what it took. It is
        // also the visible half of the paste rule: if a clipboard carrying an
        // embedded terminator arrives, this number is bigger than the paste
        // looked, and that is the tell.
        if chars > 200 {
            self.notice(format!("pasted {chars} characters"), Severity::Calm);
        }
    }

    /// One act on the composer's buffer, **with the two DEFERRALS that make the
    /// composer share the keyboard without a focus mode.**
    ///
    /// `↑`/`↓` at the composer's own edge, and `home`/`end` on an empty
    /// composer, belong to the transcript instead. Both conditions are things
    /// he can SEE — is there text, is the caret on the first row — so there is
    /// no mode to track and no state that can be wrong.
    fn compose(&mut self, edit: tui_render::Edit) {
        use tui_render::Edit as E;
        // **THERE IS NO BOX TO COMPOSE IN.** The stand-in is standing where the
        // textarea was, and the PARENT'S draft is still in `buf` behind it —
        // so an edit dispatched here would silently rewrite a draft he is not
        // looking at, which is the same failure as a composer that looks live.
        //
        // The band with nothing in it is at all three of its edges at once,
        // which is exactly the empty-textarea case: `↑` and `↓` step the
        // selection, `←` goes back (one level: out of the subagent), `→` never
        // escapes, and anything that would EDIT answers with the notice.
        if self.composer_is_stood_in() {
            match edit {
                E::Up => self.step_region(false),
                E::Down => self.step_region(true),
                E::Left => self.on_intent(Intent::Back),
                E::Right => {}
                _ => self.refuse_to_send(),
            }
            return;
        }
        // An edit answers the offer by implication: he started typing, so the
        // recovered draft is not what he wants. It is DISCARDED from the screen
        // and left on disk, where the debounce will overwrite it — never
        // merged into what he is typing.
        if self.vs.composer.offer.is_some() && !matches!(edit, E::Undo | E::Redo) {
            self.answer_offer(false);
        }
        let w = self.compose_width();
        let c = &mut self.vs.composer.buf;
        let mut edited = true;
        match edit {
            E::Newline => c.newline(),
            E::Backspace => c.backspace(),
            E::Delete => c.delete(),
            // **`←` EXITS ONLY FROM THE VERY START OF THE DRAFT.** *"left to
            // exit."* The edge here is `Pos { line: 0, byte: 0 }` and NOT the
            // start of a visual row: with two lines and the caret at the head
            // of the second, `←` moves to the end of the first, because there
            // is still text to its left. Reading this edge as the visual row
            // is the inverted-boundary failure this whole ordering exists to
            // prevent.
            E::Left => {
                if c.cursor() == tui_render::compose::Pos::default() {
                    self.on_intent(Intent::Back);
                    return;
                }
                c.left();
                edited = false;
            }
            E::Right => {
                c.right();
                edited = false;
            }
            E::WordLeft => {
                c.word_left();
                edited = false;
            }
            E::WordRight => {
                c.word_right();
                edited = false;
            }
            // **TEXT WINS, AND THE ESCAPE IS THE EXCEPTION.** Every arrow is
            // dispatched to the buffer FIRST; only a motion that could not
            // move reaches the walk. His rule, in his own words: *"a cursor in
            // the textarea should not interpret any key bindings as anything
            // other than text. So if the textarea is empty then up, down and
            // left each have a purpose… Or if it's not empty and they're in
            // the first/last position then same thing."*
            //
            // **THE EDGE IS PER AXIS.** For `↑`/`↓` it is the first/last
            // VISUAL row, which is what `Composer::up`/`down` return `false`
            // on. For `←` it is the BUFFER START, below — `←` at the start of
            // line 2 goes to the end of line 1, which is text movement.
            //
            // This is the SAME LINE the old deferral lived on, re-pointed: it
            // used to scroll the transcript from inside the box (an arrow
            // acting at a distance, which is the thing `DIR-22` removes) and
            // it now steps the selection to the neighbouring region.
            E::Up => {
                if !c.up(w) {
                    self.step_region(false);
                    self.project();
                    return;
                }
                edited = false;
            }
            E::Down => {
                if !c.down(w) {
                    self.step_region(true);
                    self.project();
                    return;
                }
                edited = false;
            }
            // **NEVER ESCAPE.** His callout, and the one exception to the edge
            // rule: *"I want opt-{left,right} to move the cursor within the
            // textarea and they should stop at the beginning and end, they
            // should not escape the textarea. And opt-{up,down} could move
            // paragraphs with similar logic."*
            E::ParaUp => {
                c.para_up();
                edited = false;
            }
            E::ParaDown => {
                c.para_down();
                edited = false;
            }
            E::RowStart => {
                if c.is_blank() {
                    self.on_intent(Intent::Top);
                    return;
                }
                c.row_start(w);
                edited = false;
            }
            E::RowEnd => {
                if c.is_blank() {
                    self.on_intent(Intent::Bottom);
                    return;
                }
                c.row_end(w);
                edited = false;
            }
            E::BufStart => {
                c.buffer_start();
                edited = false;
            }
            E::BufEnd => {
                c.buffer_end();
                edited = false;
            }
            E::KillToEnd => c.kill_to_line_end(),
            E::KillToStart => c.kill_to_line_start(),
            E::KillWord => c.kill_word_back(),
            E::Yank => c.yank(),
            E::Undo => {
                if !c.undo() {
                    self.notice("nothing to undo", Severity::Unknown);
                    return;
                }
            }
            E::Redo => {
                if !c.redo() {
                    self.notice("nothing to redo", Severity::Unknown);
                    return;
                }
            }
            E::Tab => c.insert_paste(&" ".repeat(tui_render::compose::TAB_WIDTH)),
            E::Unqueue => {
                if !c.unqueue() {
                    self.notice("nothing queued here", Severity::Unknown);
                    return;
                }
            }
        }
        if edited {
            self.touch_draft();
        }
        self.project();
    }

    /// **QUEUE IT.** His `left`, mirrored from the AUI: the composed text is
    /// set aside and the buffer empties. Nothing reaches the daemon.
    fn do_queue(&mut self) {
        if self.vs.composer.buf.queue() {
            let n = self.vs.composer.buf.pending().len();
            self.notice(
                format!("queued — {n} held here, enter sends them all"),
                Severity::Ok,
            );
            self.touch_draft();
        } else {
            self.notice("nothing to queue", Severity::Unknown);
        }
        self.project();
    }

    // ── MIC MODE ─────────────────────────────────────────────────────────

    /// The capture behind the first failed entry, if there is one.
    fn first_failed(&self) -> Option<String> {
        self.vs
            .composer
            .buf
            .pending()
            .iter()
            .find(|p| matches!(p.state, tui_render::compose::ItemState::Failed(_)))
            .and_then(|p| p.audio.clone())
    }

    /// **ENTER OR LEAVE MIC MODE — and the gate is the point.**
    ///
    /// `DIR-5`, in his own words: *"don't toggle it if there is content in the
    /// textarea, to avoid out of order concerns. That's a naive constraint
    /// we'll apply for now."* **The refusal is SPOKEN.** A toggle that
    /// silently declines is a key he presses again, which is the failure the
    /// notice area exists to prevent.
    ///
    /// Recorded as HIS NAIVE CONSTRAINT and not as law: a later round that
    /// removes the ordering hazard directly is expected to delete this guard,
    /// not to re-prove it.
    fn toggle_mic(&mut self) {
        if self.vs.mode == Mode::Mic {
            // Leaving with a recording running stops it rather than orphaning
            // a child process holding the microphone open.
            if self.recording.is_some() {
                self.toggle_record();
            }
            self.vs.mode = Mode::Thread(Region::Composer);
            self.vs.mic = MicState::Idle;
            self.project();
            return;
        }
        if self.store.open_id().is_none() {
            self.notice("no thread open to speak into", Severity::Unknown);
            return;
        }
        if !self.vs.composer.buf.is_blank() {
            self.notice(
                "the box has text in it — send or queue it first, then mic mode",
                Severity::Warn,
            );
            return;
        }
        self.vs.mode = Mode::Mic;
        self.vs.mic = MicState::Idle;
        self.project();
    }

    /// **RECORD NOW, FROM WHEREVER HE IS — and it never answers with a
    /// refusal, because "regardless" was the word he used.**
    ///
    /// His words: *"after doing r to record then it went into the queue and
    /// then I no longer could do r to record another one. I need opt-r to let
    /// me do that regardless."*
    ///
    /// # What was actually wrong, measured rather than assumed
    ///
    /// **NOT the `is_blank()` guard on [`App::toggle_mic`].** That was the
    /// standing diagnosis and it is FALSE: `Composer::is_blank` reads
    /// `self.lines` only, and a finished capture lands in `pending` — so a
    /// queued recording, transcribed or not, leaves the buffer blank and mic
    /// mode re-opens. `record_twice_end_to_end` drives exactly that and passes.
    ///
    /// **The real gap is STRUCTURAL and it is what his sentence describes.**
    /// `Intent::MicRecord` exists in `Mode::Mic` and nowhere else, so from the
    /// composer he is returned to, from the chat, and from the queue VIEW —
    /// where `r` is RELOAD — there is no key that records. Getting back cost a
    /// mode change plus a second keystroke, and before the ctrl migration the
    /// mode-change key he had been given (`opt-m`) did not arrive on his
    /// keyboard at all.
    ///
    /// # The typed-text decision, and it is a decision
    ///
    /// `DIR-5`'s guard refuses mic mode over a non-blank box — *"to avoid out
    /// of order concerns. That's a naive constraint we'll apply for now."* His
    /// "regardless" cannot be answered with that refusal, and of the two
    /// defensible answers this takes the first: **QUEUE what is typed, then
    /// record.** Reasons, in order:
    ///
    ///  1. It is exactly what the guard's own notice tells him to do by hand
    ///     (*"send or queue it first, then mic mode"*), so the key does the
    ///     chore instead of naming it.
    ///  2. It PRESERVES the order the guard exists to protect: the typed words
    ///     go into the queue BEFORE the recording, which is the order he made
    ///     them in. Recording alongside typed text would leave two things with
    ///     no stated order between them, which is the hazard itself.
    ///  3. It is non-destructive and undoable — `^K` in the queue region takes
    ///     the entry straight back into the box.
    ///
    /// The guard on `toggle_mic` is LEFT STANDING: it is his naive constraint,
    /// he has not retracted it for that key, and it was never this defect.
    ///
    /// **AND IT IS IDEMPOTENT, OUT LOUD.** Pressing it while a capture is
    /// already open says so rather than stopping the capture — a key named
    /// "record" must not be a stop button — and a silent decline is a key he
    /// presses again.
    fn record_now(&mut self) {
        if self.store.open_id().is_none() {
            self.notice("no thread open to speak into", Severity::Unknown);
            return;
        }
        if self.recording.is_some() {
            self.notice("already recording — r or space stops it", Severity::Ok);
            return;
        }
        // **THE BURST ENDS HERE, EXPLICITLY.** `on_intent` only spares
        // `Intent::MicRecord` from `interrupt_hold`, so a hold in flight has
        // already been settled by the time this runs — which is what keeps a
        // deferred press from being applied after the capture this opens.
        if !self.vs.composer.buf.is_blank() {
            // Queue it rather than refuse. See the doc comment.
            self.do_queue();
        }
        if self.vs.mode != Mode::Mic {
            self.vs.mode = Mode::Mic;
            self.vs.mic = MicState::Idle;
        }
        self.toggle_record();
    }

    /// **START OR STOP RECORDING.**
    ///
    /// The stop half runs OFF the event loop: signalling the child, waiting for
    /// it and repairing its header takes up to a second and a half, and doing
    /// that inline would freeze the face — the same rule that makes playback
    /// detached.
    fn toggle_record(&mut self) {
        if let Some(rec) = self.recording.take() {
            self.vs.mic = MicState::Stopping;
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let path = rec.stop();
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                // **A CAPTURE WITH NO BYTES IN IT IS A FAILURE, SAID OUT
                // LOUD.** It is the shape an ungranted microphone most often
                // takes after the device refuses outright, and a queue row that
                // appeared and then sat empty would be the face losing his
                // words in silence.
                let result = if bytes == 0 {
                    let _ = std::fs::remove_file(&path);
                    Err("the recording came back empty — nothing was captured".to_string())
                } else {
                    Ok(path.to_string_lossy().to_string())
                };
                let _ = tx.blocking_send(Ev::Recorded { result });
            });
            self.project();
            return;
        }
        match crate::mic::Recording::start() {
            Ok(rec) => {
                self.recording = Some(rec);
                self.vs.mic = MicState::Recording;
                self.notice("recording — r or space stops it", Severity::Ok);
            }
            Err(e) => {
                // **THE REFUSAL IS A STATE, NOT A FLASH.** It stays on the band
                // until something changes it, because "the audio interface has
                // the microphone" is still true a minute later.
                let why = e.to_string();
                self.vs.mic = MicState::Refused(why.clone());
                self.notice(why, Severity::Critical);
            }
        }
        self.project();
    }

    /// **ONE KEY, TWO GESTURES — TAP TOGGLES, HOLD STREAMS.**
    ///
    /// His ask, verbatim: *"it would be useful to have the option to STT
    /// stream… the way claude code does it is if you hold down space then it
    /// does a STT stream… **in addition to the toggle mode**."* The toggle is
    /// untouched; this is a second reading of the same act.
    ///
    /// **IT READS AN ACT AND NOT A KEY.** `r` and `space` both mean
    /// `Intent::MicRecord` in `Mode::Mic` (`keys.rs`), and holding either one
    /// is the same gesture — so no key was minted, no table row changed, and
    /// the `?` overlay still enumerates the whole grammar truthfully. The
    /// mode gate is free: this intent cannot be produced anywhere else.
    ///
    /// **THE FIRST EVENT OF A HOLD IS THE TAP.** The capture opens on the
    /// press, which is why this face owes him no "keep holding…" affordance
    /// to cover a dead half-second the way Claude Code's does — Claude Code
    /// arms at five repeats because its space is TEXT and it cannot act until
    /// it is sure. In mic mode the letters are not text, so the microphone is
    /// open from the first byte and the repeat count decides only whether the
    /// END of the burst should close it.
    ///
    /// **WHY THE REPEATS ARE SWALLOWED AND NOT ACTED ON.** They are the same
    /// act arriving ten times a second. Acting on them would stop the
    /// recording 90 ms after starting it, start another 90 ms later, and leave
    /// a trail of sub-tenth-second captures in the queue — which is what this
    /// face does TODAY when the key is held, and is the defect this gesture
    /// closes on its way past.
    fn on_mic_record(&mut self) {
        self.on_mic_record_at(std::time::Instant::now())
    }

    /// The same act with the clock handed in, so a test can state a cadence
    /// exactly instead of sleeping for one. **A timing machine proven with
    /// real sleeps is a machine whose test is a load measurement**, and this
    /// round runs five agents compiling at once.
    fn on_mic_record_at(&mut self, now: std::time::Instant) {
        match self.hold.press(now) {
            crate::mic::Press::Act => {
                let was_idle = self.recording.is_none();
                self.toggle_record();
                // Only a press that OPENED the microphone gives the end of the
                // hold something to close. A hold that began by stopping a
                // running capture must not start a new one when he lets go.
                let opened = was_idle && self.recording.is_some();
                self.hold.began_capture(opened);
            }
            crate::mic::Press::Armed => {
                // **THE ONE THING THE FACE OWES A HOLD**: saying that letting
                // go is what ends it. Until this line the band reads "r or
                // space stops it", which is the TAP's contract and is now the
                // wrong sentence for the gesture actually in progress.
                if self.recording.is_some() {
                    self.notice("● listening — let go to transcribe", Severity::Critical);
                    self.project();
                }
            }
            // Held rather than acted on, and held rather than thrown away —
            // see `mic::Press::Defer`. The idle gap decides which it was.
            crate::mic::Press::Defer | crate::mic::Press::Repeat => {}
        }
    }

    /// **THE FIFTH CLOCK, AND IT EXISTS BECAUSE SILENCE CANNOT BE AN EVENT.**
    ///
    /// A hold ends when the auto-repeat stops, and nothing arrives to say so —
    /// there is no key-release byte in his terminal (`0095`). So the end of the
    /// gesture is a deadline, armed only while a hold that opened a capture is
    /// in flight. A tap, an idle mic band, and every mode that is not
    /// `Mode::Mic` arm nothing, exactly as the link clock arms nothing on a
    /// healthy link.
    fn hold_deadline(&self) -> Option<Duration> {
        self.hold.deadline(std::time::Instant::now())
    }

    /// The idle gap expired. If a HOLD was in flight, he has let go: stop the
    /// capture, which puts it on exactly the same path a second tap would
    /// have.
    fn on_hold_idle(&mut self) {
        let what = self.hold.expire(std::time::Instant::now());
        self.settle_hold(what);
    }

    /// **ANY OTHER ACT ENDS THE BURST.** A hold is a run of ONE act with
    /// nothing between its events; another key or a click is proof the key
    /// came up, and it is proof that arrives sooner than the idle gap. A press
    /// the burst was holding is applied here rather than dropped, and it is
    /// applied BEFORE the act that interrupted it, which is the order he made
    /// them in.
    fn interrupt_hold(&mut self) {
        let what = self.hold.interrupt();
        self.settle_hold(what);
    }

    fn settle_hold(&mut self, what: crate::mic::Expire) {
        match what {
            // He let go. Stop the capture the hold opened, which is the same
            // path a second tap takes — one transcription, at the end, of the
            // whole utterance.
            crate::mic::Expire::EndHold => {
                if self.recording.is_some() {
                    self.toggle_record();
                }
            }
            // **THE PRESS THAT WAS HELD WAS HIS.** No third event came, so it
            // was a hand and not a keyboard, and it acts now rather than being
            // eaten — a fifth of a second late, which is the whole price of
            // telling a hold from a double-tap in a terminal that cannot
            // report a key coming up.
            crate::mic::Expire::Deferred => self.toggle_record(),
            crate::mic::Expire::None => {}
        }
    }

    /// A capture came back from the recorder. Queue it and start transcribing.
    ///
    /// **IT MAY NOT SPEAK FOR THE BAND IF ANOTHER CAPTURE IS ALREADY OPEN.**
    /// Stopping runs off the event loop and takes up to a second and a half,
    /// so this arrives LATE — and a press in the meantime has already set
    /// `MicState::Recording`. Writing `Idle` over it put the face in the one
    /// state `mic.rs`'s header says mic mode must never be in: *"a face that
    /// is recording and does not look like it is recording"*. Measured on a
    /// real pty, three taps 700 ms apart, with `rec` still running and the
    /// band reading "press r to record".
    ///
    /// The QUEUE half is unconditional and must stay that way: the capture
    /// really did arrive, and dropping it because a newer one is running would
    /// lose his words. Only the BAND's claim about the device is withheld.
    fn on_recorded(&mut self, result: Result<String, String>) {
        let free = self.recording.is_none();
        if free {
            self.vs.mic = MicState::Idle;
        }
        match result {
            Ok(path) => {
                self.vs.composer.buf.queue_audio(&path);
                self.spawn_transcribe(path);
            }
            Err(why) => {
                // The NOTICE is unconditional — that capture really did fail
                // and he is owed the sentence. The standing STATE is not: it
                // would be a refusal painted over a live microphone.
                if free {
                    self.vs.mic = MicState::Refused(why.clone());
                }
                self.notice(why, Severity::Critical);
            }
        }
        self.project();
    }

    /// Run the transcriber off-thread. **The gate is inside `mic::transcribe`**
    /// and runs before the transcriber is spawned — see that module for why the
    /// order is the whole point.
    fn spawn_transcribe(&mut self, path: String) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result =
                crate::mic::transcribe(std::path::Path::new(&path)).map_err(|e| e.plainly());
            let _ = tx.blocking_send(Ev::Transcribed { path, result });
        });
    }

    /// The transcriber answered.
    fn on_transcribed(&mut self, path: String, result: Result<String, String>) {
        let failed = result.is_err();
        let why = result.as_ref().err().cloned();
        if !self.vs.composer.buf.resolve_audio(&path, result) {
            // The row went away while the transcription was in flight. Dropping
            // the text is correct — there is nothing to attach it to — and the
            // capture is the shell's to clean up.
            tracing::debug!(%path, "a transcript arrived for a row that is gone");
            let _ = std::fs::remove_file(&path);
            return;
        }
        if let Some(why) = why {
            // The row already says this; the info area carries the MOMENT it
            // happened, which is what he is looking away from the queue for.
            self.notice(why, Severity::Critical);
        } else if failed {
            self.notice("that recording did not transcribe", Severity::Critical);
        }
        self.touch_draft();
        self.project();
    }

    /// Transcribe a failed capture again. The audio was kept; this is what
    /// makes keeping it worth something.
    fn retry_one(&mut self, path: Option<String>) {
        let Some(path) = path else {
            self.notice("nothing failed to retry", Severity::Unknown);
            return;
        };
        if !self.vs.composer.buf.retry_audio(&path) {
            self.notice("that item is not a failed recording", Severity::Unknown);
            return;
        }
        self.notice("transcribing again…", Severity::Ok);
        self.spawn_transcribe(path);
        self.project();
    }

    /// Take an entry out of the queue, capture and all.
    fn drop_one(&mut self, path: Option<String>) {
        let Some(path) = path else {
            self.notice("nothing failed to drop", Severity::Unknown);
            return;
        };
        if self.vs.playing.as_deref() == Some(path.as_str()) {
            self.stop_playback();
        }
        if self.vs.composer.buf.drop_audio(&path) {
            let _ = std::fs::remove_file(&path);
            self.notice("recording dropped", Severity::Ok);
        }
        self.project();
    }

    /// **PLAY A CAPTURE, OR STOP THE ONE PLAYING.**
    ///
    /// `afplay` blocks for the length of the file, so it is spawned DETACHED
    /// and reaped by the loop. A second press on a row that is already playing
    /// stops it; a press on a different row replaces it.
    fn toggle_play(&mut self, path: &str) {
        if self.vs.playing.as_deref() == Some(path) {
            self.stop_playback();
            self.project();
            return;
        }
        self.stop_playback();
        match crate::mic::play(std::path::Path::new(path)) {
            Ok(child) => {
                self.playing = Some(std::sync::Arc::new(std::sync::Mutex::new(child)));
                self.vs.playing = Some(path.to_string());
                // The WAITER. It owns a clone of the handle so a second press
                // can still kill the child through the other one.
                let held = self.playing.clone().expect("just set");
                let tx = self.tx.clone();
                let p = path.to_string();
                std::thread::spawn(move || {
                    // Poll rather than block on `wait()`: `wait()` takes the
                    // lock for the length of the clip, and a second press would
                    // then block the event loop waiting for it — which is the
                    // freeze this whole detached design exists to avoid.
                    let result = loop {
                        {
                            let Ok(mut c) = held.lock() else { return };
                            match crate::mic::reap(&mut c) {
                                Some(Ok(())) => break Ok(()),
                                Some(Err(_)) => {
                                    break Err("that recording could not be played — the file is \
                                               missing or unreadable"
                                        .to_string())
                                }
                                None => {}
                            }
                        }
                        std::thread::sleep(Duration::from_millis(80));
                    };
                    let _ = tx.blocking_send(Ev::Played { path: p, result });
                });
            }
            Err(e) => self.notice(format!("could not play it: {e}"), Severity::Critical),
        }
        self.project();
    }

    fn stop_playback(&mut self) {
        if let Some(c) = self.playing.take() {
            if let Ok(mut c) = c.lock() {
                crate::mic::stop_playing(&mut c);
            }
        }
        self.vs.playing = None;
    }

    /// **SEND THE WHOLE PAYLOAD.** His `wildcard`: everything queued plus
    /// whatever is in the buffer, as ONE message, in ONE `threads/{id}/send`.
    ///
    /// **The composer is not emptied here.** It is emptied by [`App::on_sent`]
    /// when the daemon says it took it — see `sends::spawn_send` for why.
    fn do_send(&mut self) {
        // **THE COMPOSER IS POINTING AT SOMETHING WITH NO INBOX.** Answer with
        // the notice; issue nothing. See [`App::follow_subject`] — there is no
        // send path to a claude-code subagent and inventing one here would
        // send his words to the PARENT thread while the screen said otherwise.
        if self.composer_is_stood_in() {
            self.refuse_to_send();
            return;
        }
        if self.sending.is_some() {
            // The double-send guard, and it says so rather than doing nothing:
            // a key that silently declines is a key he presses again.
            self.notice("still sending the last one", Severity::Warn);
            return;
        }
        let Some(thread_id) = self.store.open_id().map(str::to_string) else {
            self.notice("no thread open to send to", Severity::Unknown);
            return;
        };
        // **A QUEUE THAT IS NOT READY DOES NOT GO, AND IT SAYS WHY.**
        //
        // Sending the ready entries and leaving the rest would drop something
        // he put in the queue with nothing reporting the loss — the worst
        // failure this path has. `payload()` used to have only two answers and
        // collapsing the third into "nothing to send" would have been the face
        // lying about its own contents.
        let text = match self.vs.composer.buf.payload_state() {
            Payload::Ready(t) => t,
            Payload::Nothing => {
                self.notice("nothing to send", Severity::Unknown);
                return;
            }
            Payload::Transcribing(n) => {
                self.notice(
                    format!("{n} still transcribing — nothing was sent, and nothing was lost"),
                    Severity::Warn,
                );
                return;
            }
            Payload::Failed(n) => {
                self.notice(
                    format!("{n} recording(s) did not transcribe — retry or drop before sending"),
                    Severity::Warn,
                );
                return;
            }
        };
        if !self.handle.is_live() {
            // The transport refuses a call on a dead link loudly rather than
            // queueing it into whatever socket exists minutes later. Saying so
            // HERE keeps his words in the box, which is where he can still see
            // them.
            self.notice(
                "the daemon is not connected — nothing was sent, and nothing was lost",
                Severity::Critical,
            );
            return;
        }
        let parts = self.vs.composer.buf.pending().len();
        // **THE LOG LINE FOR THIS LIVES IN `sends`, WITH THE CALL.** It was
        // here, and the chat suite's mutation-surface check could not tell a
        // log message from a call site — which is the ambiguity that check
        // exists to remove, so the log moved rather than the check widening.
        self.sending = Some(thread_id.clone());
        self.vs.composer.sending = true;
        crate::sends::spawn_send(self.handle.clone(), self.tx.clone(), thread_id, text, parts);
        self.project();
    }

    /// The daemon's answer to the one mutation.
    fn on_sent(&mut self, thread_id: String, result: Result<Option<u64>, String>) {
        self.sending = None;
        self.vs.composer.sending = false;
        match result {
            Ok(id) => {
                tracing::info!(%thread_id, message_id = ?id, "the send was accepted");
                crate::diag!("send", "ACK thread={thread_id} message_id={id:?}");
                // **NOW** the buffer, its history and the queue go, and the
                // journal with them. This is the one place a draft is deleted
                // on a success.
                self.vs.composer.buf.accepted();
                self.vs.composer.offer = None;
                self.last_edit = None;
                self.draft_dirty = false;
                crate::draft::clear(&thread_id);
                // Sending is the one moment the view should jump to the bottom:
                // he just spoke, and what he said is at the end.
                self.thread_vp = Viewport::at_bottom();
                self.thread_frozen = None;
                self.notice("sent", Severity::Ok);
            }
            Err(why) => {
                tracing::error!(%thread_id, %why, "the send was refused");
                crate::diag!("send", "REFUSED thread={thread_id} why={why}");
                // The words are still in the box. Say what happened and leave
                // them there — a face that cleared the composer and then failed
                // is a face that ate a message.
                self.notice(format!("not sent: {why}"), Severity::Critical);
            }
        }
        self.project();
    }

    /// Record that the buffer changed, for the two idle deadlines.
    fn touch_draft(&mut self) {
        self.last_edit = Some(std::time::Instant::now());
        self.draft_dirty = true;
    }

    /// When the loop should next wake for the composer, if at all.
    ///
    /// `None` means there is nothing pending and the loop blocks on `recv()`
    /// exactly as it always did — a face sitting still has no timers.
    fn compose_deadline(&self) -> Option<Duration> {
        let since = self.last_edit?.elapsed();
        let want = if self.draft_dirty {
            DRAFT_IDLE
        } else {
            RUN_IDLE
        };
        if since >= want {
            Some(Duration::ZERO)
        } else {
            Some(want - since)
        }
    }

    /// The idle timer fired: close the edit run, and write the journal.
    fn on_compose_idle(&mut self) {
        let Some(since) = self.last_edit.map(|t| t.elapsed()) else {
            return;
        };
        if since >= RUN_IDLE {
            // A pause for thought is an undo boundary. Cheap, and it is what
            // makes `^Z` mean "the thing I just typed" rather than "everything
            // since I opened this thread".
            self.vs.composer.buf.close_run();
        }
        if self.draft_dirty && since >= DRAFT_IDLE {
            self.draft_dirty = false;
            self.last_edit = None;
            if let Some(id) = self.store.open_id().map(str::to_string) {
                crate::draft::save(&id, &self.journal_text());
            }
        }
    }

    /// What the journal holds: everything unsent, the QUEUE included.
    ///
    /// A crash with four messages queued and a fifth half-typed must not come
    /// back with only the fifth. They are joined the way the payload joins
    /// them, so a restored draft reads as the thing it was.
    fn journal_text(&self) -> String {
        self.vs.composer.buf.payload().unwrap_or_default()
    }

    /// **PARK THE COMPOSER** against the thread being left, so stepping out and
    /// back does not lose a draft. Memory only; the journal is the crash case.
    fn park_composer(&mut self) {
        if let Some(id) = self.store.open_id().map(str::to_string) {
            // Write the journal NOW rather than waiting for a debounce that
            // will not fire: leaving a thread is exactly when the timer would
            // have been disarmed with the write still owed.
            if self.draft_dirty {
                self.draft_dirty = false;
                crate::draft::save(&id, &self.journal_text());
            }
            let c = std::mem::take(&mut self.vs.composer.buf);
            if !c.is_blank() || !c.pending().is_empty() {
                self.parked.insert(id, c);
            }
        }
        self.vs.composer.offer = None;
        self.last_edit = None;
    }

    /// **UNPARK, or OFFER what the journal has.**
    ///
    /// The precedence matters: an in-session parked composer WINS over the
    /// disk journal, because it is the same session and he watched himself type
    /// it. The journal is only consulted when this process has never held a
    /// composer for this thread — which is exactly the crash case it exists
    /// for — and even then it is an OFFER and never an insertion.
    fn restore_composer(&mut self, id: &str, now: chrono::DateTime<Utc>) {
        self.vs.composer.editable = true;
        self.vs.composer.sending = false;
        self.vs.composer.offer = None;
        if let Some(c) = self.parked.remove(id) {
            self.vs.composer.buf = c;
            return;
        }
        self.vs.composer.buf = Default::default();
        if let Some((text, when)) = crate::draft::load(id) {
            let age = std::time::SystemTime::now()
                .duration_since(when)
                .ok()
                .map(|d| {
                    tui_render::geom::fmt_age(
                        chrono::TimeDelta::from_std(d).unwrap_or(chrono::TimeDelta::zero()),
                    )
                })
                .unwrap_or_else(|| "?".into());
            let _ = now;
            self.vs.composer.offer = Some(tui_render::DraftOffer {
                text,
                age: format!("{age} ago"),
            });
        }
    }

    /// **WHAT THIS KEY MEANS RIGHT NOW — the key table, unless a standing
    /// MODAL QUESTION has first refusal.**
    ///
    /// One function, and that is the point: the event loop resolves a keystroke
    /// through here and so does `deliver`, the test helper that drives real
    /// BYTES through the real decoder. A second copy of this decision in either
    /// place is how a test comes to prove something the face does not do — and
    /// `deliver`'s whole reason for existing is that `tap` starts at a `Key` a
    /// test made up.
    fn resolve(&self, key: tui_render::Key) -> Intent {
        self.offer_answer(key)
            .unwrap_or_else(|| tui_render::intent(key, self.vs.mode))
    }

    /// **WHAT THIS KEY MEANS WHILE A RECOVERED-DRAFT OFFER IS STANDING**, and
    /// `None` whenever there is no offer or the key is not one of its two
    /// answers — in which case the key table decides as it always did.
    ///
    /// Gated on the COMPOSER region as well as on the offer, because that is
    /// the only surface the offer is drawn on and the only one whose letters
    /// would otherwise be text.
    fn offer_answer(&self, key: tui_render::Key) -> Option<Intent> {
        if self.vs.composer.offer.is_none()
            || self.vs.mode != Mode::Thread(Region::Composer)
        {
            return None;
        }
        tui_render::draft_offer_intent(key)
    }

    /// Answer the standing draft offer: `true` takes it, `false` lets it go.
    fn answer_offer(&mut self, take: bool) {
        let Some(offer) = self.vs.composer.offer.take() else {
            return;
        };
        if take {
            self.vs.composer.buf = tui_render::compose::Composer::from_text(&offer.text);
            self.touch_draft();
            self.notice("draft restored", Severity::Ok);
        } else if let Some(id) = self.store.open_id().map(str::to_string) {
            crate::draft::clear(&id);
            self.notice("draft discarded", Severity::Calm);
        }
    }

    // ── intents ──────────────────────────────────────────────────────────

    fn on_intent(&mut self, intent: Intent) {
        // **A HOLD IS A RUN OF ONE ACT AND NOTHING ELSE.** Any other intent is
        // proof the key came up — auto-repeat stops the moment another key is
        // pressed — and it is proof that arrives sooner than the idle gap. It
        // is one line here because the burst must not span the work between
        // two unrelated keystrokes; see `App::interrupt_hold`.
        if !matches!(intent, Intent::MicRecord) {
            self.interrupt_hold();
        }
        // **THE FOUR SCROLLS ARE ONE CALL WITH A COUNT** — see `scroll_run`,
        // which also holds the `?` overlay's own offset. That used to be an
        // early-returning block right here, and the wheel reached neither it
        // nor these arms: it called into the band directly, which is how a
        // flick came to cost one full projection per notch.
        //
        // The page size is `last_rows - 1` under the overlay (a page that
        // overlapped by nothing would drop a line between screens) and the
        // whole band everywhere else, which is what those two expressions were
        // before they were brought together.
        let page = if self.vs.mode == Mode::Help {
            self.last_rows.max(2) - 1
        } else {
            self.last_rows.max(1)
        };
        match intent {
            // **A KEY MOVES ONE ROW; THE WHEEL MOVES FIVE PER NOTCH.** They
            // shared a constant until `0052`, which is why raising the wheel's
            // step would have made `↑` jump five rows in a list — the same
            // number serving two gestures that were never the same gesture.
            // **THE ARROWS GO THROUGH THE REGION; THE WHEEL DOES NOT.** A
            // notch has a POSITION — it scrolls the band under the pointer
            // whichever region holds the keyboard — so the edge rule lives on
            // the key path (`scroll_key`) and not in `scroll_run`, which the
            // wheel shares.
            Intent::ScrollUp => self.scroll_key(true, ROW_STEP),
            Intent::ScrollDown => self.scroll_key(false, ROW_STEP),
            Intent::PageUp => self.scroll_run(true, page),
            Intent::PageDown => self.scroll_run(false, page),
            Intent::MovePinUp => self.move_pin(acts::Dir::Up),
            Intent::MovePinDown => self.move_pin(acts::Dir::Down),
            // **`←` AT THE ROOT MEANS ONE THING AGAIN: THE FIRST ROW.**
            //
            // It used to collapse an expanded band first — a guard clause on
            // this arm — because `→` on a band header grew the band in place.
            // It does not any more: `→` on a header opens that band's own PAGE,
            // and the way back off a page is `←` there, so there is nothing at
            // the root left to collapse. The guard is DELETED rather than left
            // returning false, which is what keeps `Intent::Top`'s printed line
            // in the `?` overlay honest.
            Intent::Top => {
                self.release_pin_follow();
                if self.is_list() {
                    self.set_cursor(0);
                }
                self.freeze();
                // The renderer clamps to the real content height, which is the
                // only place that knows it. Saturation here is not a guess.
                let vp = self.vp_mut();
                vp.from_bottom = usize::MAX;
                vp.follow = false;
            }
            Intent::Bottom => {
                self.release_pin_follow();
                if self.is_list() {
                    self.set_cursor(usize::MAX);
                }
                *self.vp_mut() = Viewport::at_bottom();
                self.thaw();
            }
            // **MOVE THE SELECTION, AND DO NOTHING ELSE.** `esc`'s meaning
            // outside the composer, and the first half of what a click does.
            // It assigns the mode DIRECTLY and never descends: a region is not
            // a door, and one on the back-stack would make `←` "go back" to a
            // band instead of leaving the thread.
            Intent::Region(r) => self.select_region(r),
            // **BY AUTHORSHIP, NOT BY ROWS.** The row offset comes from the
            // renderer's own layout through one pure function — the shell does
            // not re-derive where a message sits, which is `0023`'s CLASS 3.
            Intent::JumpUserPrev | Intent::JumpUserNext => {
                let forward = matches!(intent, Intent::JumpUserNext);
                let (w, _) = self.last_size;
                let rows = self.last_rows.max(1);
                match tui_render::jump_user(&self.vs, Utc::now(), w as usize, rows, forward) {
                    Some(from_bottom) => {
                        self.freeze();
                        let vp = self.vp_mut();
                        vp.from_bottom = from_bottom;
                        vp.follow = false;
                    }
                    // **PAST THE LAST ONE IS THE LIVE STATE, AND IT IS
                    // `Bottom`'S EXACT ACT** — *"opt-down to the bottom should
                    // go to the live state of the convo"* — newest, following
                    // again, thawed. NOT a scroll to the last row: a
                    // screenshot cannot tell those two apart and the `↓ N new`
                    // chip can. Backwards past the first, there is nothing of
                    // his above, so nothing happens.
                    None if forward => self.on_intent(Intent::Bottom),
                    None => {}
                }
            }
            // **THE THIRD MEANING OF THE SAME KEY PAIR** — *"opt-{up,down} in
            // the main menu should jump to and between band headers."* The
            // headers are already cursor stops; this walks between them.
            Intent::BandPrev | Intent::BandNext => {
                let back = matches!(intent, Intent::BandPrev);
                if let Some(i) = self.band_header_from(self.project_cursor, back) {
                    self.on_intent(Intent::Select(i));
                }
            }
            // **`Intent::Activate` NEVER REACHES HERE.** It is resolved in
            // `activate` into whatever `hit::activation` says the cursor's
            // target means, and that intent is what arrives. Arriving as
            // itself would mean a resolution was skipped.
            Intent::Activate => self.activate(self.keyboard_target(), Via::Key),
            // **A CLICK ON A PLAIN ROW, AND IT DOES ONE THING.** The cursor
            // moves and the act ends there: no drill, no expand, no default
            // act. `→` is what opens what he picked, and `follow_cursor` keeps
            // the row he picked on screen if the list had to scroll to it.
            Intent::Select(i) => {
                self.release_pin_follow();
                self.set_cursor(i);
                self.follow_cursor();
            }
            // **THE CARET GOES WHERE HE POINTED** — `row` is an index into the
            // composer's whole layout (the renderer already undid the band's
            // own scroll) and `col` is a SCREEN column, so the band's gutter
            // comes off here and the composer snaps the rest to a legal
            // position.
            Intent::Caret { row, col } => {
                let inner = tui_render::composer_inner(self.last_size.0 as usize);
                let col = (col as usize).saturating_sub(tui_render::GUTTER);
                // No `touch_draft`: moving the caret is not an edit, and arming
                // the journal timer for it would write a draft file for a
                // gesture that changed no text. `place` closes the edit RUN on
                // its own, which is the part that does matter — the next
                // keystroke starts a fresh undo step, exactly as it would after
                // any other motion.
                self.vs.composer.buf.place(row, col, inner);
            }
            // **THE ROOT'S BANDS JOIN THE ONE DRILL.** A band row opens its
            // THREAD and a project row opens its project, resolved through the
            // one `ProjectList::item` mapping rather than by a second reading
            // of the cursor here.
            Intent::Open => match self.vs.mode {
                // **THE ROOT AND A BAND'S PAGE SHARE ONE ARM**, because they
                // share one projection and one cursor resolution. The rows that
                // exist only at the root — the band headers, the `see all`
                // feet, `+ new project`, `archived` — simply never come up on
                // the page, so the arms that answer them are dead there rather
                // than wrong there.
                Mode::Projects | Mode::SeeAll(_) => match self.vs.projects.item(self.root_cursor()) {
                    Some(RootItem::Project(i)) => self.open_project(i),
                    // **THE TWO CHROME ROWS ARE DOORS NOW.** `->` on them is
                    // the same act a click is, resolved in the same place.
                    Some(RootItem::NewProject) => self.open_new_project(),
                    Some(RootItem::Archived) => self.open_archived(),
                    // **`→` ON A BAND HEADER OPENS THAT BAND'S OWN PAGE**,
                    // which is the same drill-in law every other row obeys: `→`
                    // goes INTO what the cursor names, and what a header names
                    // is its band.
                    //
                    // > let's replace 'show more' with 'see all' and it takes
                    // > us to a dedicated page with just that band and all
                    // > results. … Going right on a band title does the same
                    // > thing.
                    //
                    // **TWO DOORS, AND THEY LAND IN DIFFERENT PLACES.** From
                    // the HEADER the page opens at row 0 — he asked to see the
                    // band, and the band starts at the top. From the `see all`
                    // FOOT it opens at row `BAND_ROWS`, THE FIRST ROW HE COULD
                    // NOT SEE, which is the row he was reaching for when he
                    // pressed the foot. That difference is the whole reason two
                    // doors are not one door twice.
                    //
                    // **AND IT IS ONE LAW WITH NO BRANCH.** A band that is
                    // already showing everything it has used to answer `→` with
                    // *"pinned is already showing all 4"*; now it opens its page
                    // too, with the same rows in a room of their own. `→` on a
                    // header always goes into the band.
                    Some(RootItem::Band(b)) => self.open_band_page(b, 0),
                    Some(RootItem::SeeAll(b)) => {
                        self.open_band_page(b, tui_render::BAND_ROWS)
                    }
                    Some(RootItem::Pinned(_) | RootItem::Recent(_)) => {
                        match self.vs.projects.thread_at(self.root_cursor()) {
                            Some(r) => {
                                let id = r.id.clone();
                                self.drill_to_thread(id);
                            }
                            None => self.notice("no thread to open", Severity::Unknown),
                        }
                    }
                    None => self.notice("no project to open", Severity::Unknown),
                },
                Mode::List => {
                    if let Some(id) = self.focused_thread() {
                        self.drill_to_thread(id);
                    } else {
                        self.notice("no thread to open yet", Severity::Unknown);
                    }
                }
                // Reached from nowhere: `activation` returns `Open` only for
                // a list row, and a transcript has none.
                _ => {}
            },
            Intent::MicMode => self.toggle_mic(),
            Intent::MicRecord => self.on_mic_record(),
            Intent::MicRecordNow => self.record_now(),
            Intent::MicPlay(i) => {
                if let Some(p) = self.vs.composer.buf.audio_at(i).map(str::to_string) {
                    self.toggle_play(&p);
                }
            }
            Intent::MicPlayLast => {
                let last = self
                    .vs
                    .composer
                    .buf
                    .pending()
                    .iter()
                    .rev()
                    .find_map(|p| p.audio.clone());
                match last {
                    Some(p) => self.toggle_play(&p),
                    None => self.notice("nothing recorded to play", Severity::Unknown),
                }
            }
            Intent::MicRetry(i) => {
                let p = self.vs.composer.buf.audio_at(i).map(str::to_string);
                self.retry_one(p);
            }
            Intent::MicRetryFirst => {
                let p = self.first_failed();
                self.retry_one(p);
            }
            Intent::MicDrop(i) => {
                let p = self.vs.composer.buf.audio_at(i).map(str::to_string);
                self.drop_one(p);
            }
            Intent::MicDropFirst => {
                let p = self.first_failed();
                self.drop_one(p);
            }
            Intent::Back => match self.vs.mode {
                // An overlay owes back the view it was raised over.
                Mode::Help | Mode::Queue => self.vs.mode = self.under,
                // **LEAVING THE FIELD KEEPS WHAT IS TYPED** and lands on the
                // menu, not outside the sheet. `esc` from a half-written
                // compaction instruction must not also close the door.
                Mode::SheetField => {
                    if let Some(s) = self.sheet.as_mut() {
                        s.editing = None;
                    }
                    self.vs.mode = Mode::Sheet;
                }
                // The sheet owes back the surface it was opened from. The typed
                // buffers go with it — a sheet re-opened on the same thread
                // starts empty, because `open_sheet` mints a fresh state, and
                // that is deliberate: a draft kept across a door he closed is a
                // draft he cannot see.
                Mode::Sheet => {
                    self.sheet = None;
                    self.vs.sheet = None;
                    self.vs.mode = self.under;
                }
                // **THE ARCHIVED ROOM OWES BACK THE ROOT**, which is the one
                // place it can be opened from (§1: *"one door back"*).
                Mode::Archived => self.vs.mode = Mode::Projects,
                // **LEAVING THE FIELD KEEPS THE NAME.** The same rule the
                // sheet's field has, for the same reason: `esc` is a gesture he
                // will make mid-word.
                Mode::NewProject => self.vs.mode = Mode::Projects,
                // **`esc` IN MIC MODE LEAVES MIC MODE**, and it goes through
                // the same gate the toggle does — a recording in flight is
                // stopped rather than orphaned.
                Mode::Mic => self.toggle_mic(),
                // **`esc` RETURNS HIM TO THE SURFACE HE OPENED THE THREAD
                // FROM** — the root when he opened it from a band, the
                // project's list when he drilled through the project. His
                // words: *"when I go into a thread and then hit escape, it
                // brings me back to the list of all raw threads. And I never
                // want to see that."*
                // **ONE LEVEL AT A TIME: subagent → parent thread → wherever
                // the thread was opened from.** Drilling into a subagent is a
                // push in the AUI's `ScopeStack` sense, so the first `←` has to
                // undo it or one press would take him two levels out of what he
                // was reading. It is NOT a back-stack entry — a subject is not
                // a door — so `origin` is untouched and this is the only place
                // that knows about it.
                Mode::Thread(_) if self.tree_subject != TreeId::Parent => {
                    self.undrill();
                }
                Mode::Thread(_) if !self.parents.is_empty() => {
                    self.park_composer();
                    self.back_to_parent();
                }
                Mode::Thread(_) => {
                    // **PARK THE DRAFT FIRST.** `esc` is the only way out of a
                    // thread now that the composer owns `←`, so this is a
                    // gesture he will make mid-sentence — and the store is
                    // closed on the next line, which is what takes the thread
                    // id the park needs.
                    self.park_composer();
                    self.store.close();
                    // **LANDING AT THE ROOT LEAVES THE PROJECT.** Nothing
                    // enters one behind his back any more, but the root is the
                    // one surface where "a project is open" can only ever be
                    // stale, and `Intent::SwapView` reads exactly that flag.
                    if self.ascend() == Mode::Projects {
                        self.store.leave_project();
                    }
                }
                // **`←` AT THE ROOT DOES NOT EXIT.** The table binds it to
                // `Intent::Top` there, so this arm is only reachable from a
                // click on nothing — and doing nothing is the right answer to
                // that too. An accidental `←` must never end the session.
                Mode::Projects => {}
                // **`←` OFF A BAND'S PAGE GOES BACK TO THE MAIN MENU.** His
                // words, exactly:
                //
                // > Going left from there goes back to the main menu.
                //
                // It is `ascend()` and not `vs.mode = Mode::Projects`, and the
                // difference matters even though the page's only door IS the
                // root: the stack is the ONE answer to "where did he come
                // from" (`0073` spent a round removing the second one), and an
                // arm that assigned the mode directly would leave `origin`
                // holding a frame nothing will ever pop — which `I-8`'s
                // empty-stack-at-the-root half catches.
                //
                // **The root's cursor and viewport are not touched here**, and
                // that is the whole of `I-3`: he comes back standing on the
                // band header or the `see all` foot he pressed `→` on. The page
                // keeps its own (`seeall_cursor`), so it is also still where he
                // left it if he goes back in.
                Mode::SeeAll(_) => {
                    self.ascend();
                }
                // Leaving a project's threads goes back to the projects. The
                // cursor is REMEMBERED per depth (`project_cursor` is not
                // touched), which is the AUI's `ScopeStack { stack, cursors }`
                // behaviour and the detail most likely to be dropped.
                Mode::List => {
                    self.store.leave_project();
                    self.ascend();
                }
                Mode::Activity => {}
                // **`esc` CANCELS THE RENAME AND CHANGES NOTHING**, which is
                // the modal's contract and the one place it deliberately does
                // NOT copy `Mode::SheetField`. There, `esc` keeps what is typed
                // and lands on the sheet's menu, because a half-written
                // compaction instruction is worth more than the keystroke that
                // closed the door and the sheet is still open behind it to hold
                // it. A modal has nothing behind it but his list, so keeping a
                // draft would mean keeping it somewhere he cannot see — and the
                // sheet's own rule already says why that is wrong: *"a draft
                // kept across a door he closed is a draft he cannot see."*
                //
                // Nothing has been sent at this point on any path, so "changes
                // nothing" is a property of the code and not a promise: the one
                // mutation is in `sheet_commit`, which `enter` reaches and this
                // does not.
                Mode::RenameModal => {
                    self.sheet = None;
                    self.vs.sheet = None;
                    self.naming = None;
                    self.vs.mode = self.under;
                }
            },
            Intent::SwapView => {
                // The event lane is a SIBLING of whichever surface he was on,
                // and it owes that surface back — which is why `tab` cannot be
                // expressed as left/right and is the one honest survivor of the
                // letter cut.
                //
                // **IT OWES BACK WHAT IT WAS RAISED OVER, not what the store
                // happens to have open.** It used to ask
                // `store.open_project().is_some()`, which was the SECOND
                // teleport `0073` measured (frames E8-E10): after opening any
                // root-band thread the store held a project open forever, so
                // `Tab Tab` from the root landed on that project's list with no
                // escape pressed at all. The stack answers the question the
                // flag was standing in for, and it answers it for the thread
                // and the mic too — surfaces the old clause could not name.
                if self.view() == Mode::Activity {
                    self.ascend();
                } else {
                    self.descend(Mode::Activity);
                }
            }
            Intent::Help => {
                self.under = self.view();
                // The overlay prints the grammar of the view it was raised
                // OVER, and opens at the top of it.
                self.vs.help_for = self.view();
                self.vs.help_from = 0;
                self.vs.mode = Mode::Help;
            }
            // **THE ONE-KEY COLLAPSE TO WHAT WAS SAID, AND ITS WAY BACK.**
            // `^S`, in a thread and nowhere else.
            //
            // It RE-CUTS the block list wholesale, so it thaws: a freeze point
            // measured in blocks stops meaning anything once the blocks are
            // different blocks. The three arms that used to share that reason
            // — `Intent::Filter`, `FilterAll` and `FilterLane` — are deleted
            // with the filter, and this is the only one left.
            //
            // **AND IT IS ONE LINE NOW.** It used to call
            // `Filter::toggle_speech_only`, which stashed the bitmask he had
            // tuned so a second press could put it back. With no filter there
            // is no tuned state to stash, nothing to restore, and nothing this
            // arm has to remember: the state IS the boolean.
            Intent::StdoutOnly => {
                self.vs.stdout_only = !self.vs.stdout_only;
                self.thaw();
            }
            Intent::QueueOverlay => {
                self.under = self.view();
                self.vs.mode = Mode::Queue;
                self.load_queue();
            }
            // ── THE TWO ACTS, AND THEY ARE TWO ───────────────────────────
            Intent::Send => self.do_send(),
            Intent::QueueMessage => self.do_queue(),
            Intent::Compose(e) if self.vs.mode == Mode::NewProject => self.name_compose(e),
            // **THE MODAL TYPES INTO THE SHEET'S OWN BUFFER**, because it IS
            // the sheet's own buffer — one box, one set of edit semantics, and
            // no second textarea to get grapheme motion wrong in.
            Intent::Compose(e)
                if matches!(self.vs.mode, Mode::SheetField | Mode::RenameModal) =>
            {
                self.sheet_compose(e)
            }
            Intent::Compose(e) => self.compose(e),
            Intent::Type(c) if self.vs.mode == Mode::NewProject => {
                if let Some(c0) = self.new_project.as_mut() {
                    c0.insert_char(c);
                }
            }
            Intent::Type(c) if self.vs.mode == Mode::RenameModal && self.naming.is_some() => {
                if let Some(n) = self.naming.as_mut() {
                    n.buf.insert_char(c);
                }
            }
            Intent::Type(c) if matches!(self.vs.mode, Mode::SheetField | Mode::RenameModal) => {
                if let Some(s) = self.sheet.as_mut() {
                    if let Some(act) = s.editing {
                        s.buf(act).insert_char(c);
                    }
                }
            }
            // **TYPING TYPES ONLY WHERE THE BOX HOLDS THE SELECTION.**
            //
            // Round four's rule was the opposite — typing always typed and
            // dragged the selection into the box with it — and his answer to
            // it is the defect this round exists for: *"if I do keypresses
            // anywhere, including when I'm in the chat scroller or in the
            // agent tree, the keys still make changes into the textarea."*
            //
            // **THE GATE IS THE TABLE, NOT THIS ARM.** `chat_rows!`,
            // `queue_rows!` and `agents_rows!` no longer carry a printable
            // row, so a `Type` cannot be produced outside the box at all —
            // which is why there is no `select_region` here any more and no
            // guard in its place. A guard here would be a corrector standing
            // downstream of a producer that has been fixed, and it would hide
            // the day somebody puts the row back.
            //
            // **A PRINTABLE AIMED AT A SUBAGENT TYPES NOTHING AND SAYS SO.**
            // It must not fall through into the PARENT'S draft: he would be
            // writing into a box he is not looking at.
            Intent::Type(_) if self.composer_is_stood_in() => {
                self.refuse_to_send();
            }
            Intent::Type(c) => {
                if self.vs.composer.offer.is_some() {
                    self.answer_offer(false);
                }
                self.vs.composer.buf.insert_char(c);
                self.touch_draft();
            }
            Intent::Drill(i) => self.drill(i),
            Intent::AcceptDraft => self.answer_offer(true),
            Intent::DiscardDraft => self.answer_offer(false),
            Intent::Expand => {
                self.vs.expanded = !self.vs.expanded;
                self.thaw();
            }
            // **ONE ROW.** Resolve the index against the projection that is on
            // screen RIGHT NOW — which is the only moment the index is true —
            // and remember the ledger identity it names. A miss (a rule, a row
            // with nothing behind it, an index the view has since outgrown) is
            // silent, exactly as a click on nothing is.
            //
            // **NO `thaw()` HERE, and that is the point.** The old handler
            // released the frozen viewport on every expansion, so a click while
            // scrolled up dumped every held-back arrival in and jumped the view
            // (`0047` D1's side note). Opening a row he is looking at must not
            // move the thing he is looking at.
            Intent::ExpandBlock(i) => {
                if let Some(key) = self.vs.blocks.get(i).and_then(|b| b.key()).cloned() {
                    if !self.opened.remove(&key) {
                        self.opened.insert(key);
                    }
                }
            }
            // ── THE SHEET (§5) ───────────────────────────────────────────
            Intent::OpenSheet(i) => self.open_sheet(i),
            // **THERE IS NO `Intent::OpenAt` ARM ANY MORE, AND ITS ABSENCE IS
            // THE WHOLE OF SCOPE 2.** Round four built it so that a click on a
            // thread's NAME selected the row and then opened it, in one act.
            // He retracted the reading it rested on: *"clicking on a thread
            // shouldn't take me into it. Only right can do it."* The intent is
            // deleted from the table, `hit::activation` resolves that click to
            // `Intent::Select` instead, and `→` is the only way in.
            // **`o` OPENS THE OPTIONS FOR THE ROW UNDER THE CURSOR**, and it
            // asks the SAME table a click asks. His words: *"I want to be able
            // to jump straight into the options of a thread by clicking o
            // while my cursor is on the thread handle."*
            //
            // It aims at the TITLE rather than at the row, because the title
            // is what the sheet's door is claimed on — so `o` and a pointer
            // land on one arm of `hit::activation` and cannot drift. A cursor
            // that is not on a thread (a band header, a project row, the
            // `show more` foot) resolves to no thread and the door stays shut,
            // silently: a miss is free.
            // # AND SINCE ROUND SIX IT ANSWERS ON A PROJECT ROW TOO
            //
            // **THE PROJECT SHEET HAD NO KEYBOARD DOOR AT ALL** — `0127`
            // measured it and kept the project-title CLICK rather than delete
            // the room. This is the ruling that closed it, and the cheap thing
            // about it is what makes it right: **no new key.** `o` already
            // means *the options for the row I am standing on*; it simply had
            // no answer when that row was a project. So it learns one more row
            // TYPE, costs nothing in the ctrl namespace, and needed no
            // assignment from `0128`'s collision table.
            //
            // It aims at `Target::ProjectName`, which is the span a pointer
            // lands on, so both doors are still ONE arm of `hit::activation`
            // and cannot drift — the same property the thread title has. The
            // click arm is `Via::Key` now that this exists, which is the other
            // half of the same edit.
            //
            // A cursor on a band header, the `see all` foot or `+ new project`
            // resolves to neither kind of row and the door stays shut,
            // silently: a miss is free.
            Intent::Options => {
                let i = match self.view() {
                    Mode::Projects | Mode::SeeAll(_) => self.root_cursor(),
                    _ => self.list_cursor,
                };
                let on_project = matches!(self.view(), Mode::Projects | Mode::SeeAll(_))
                    && matches!(self.vs.projects.item(i), Some(RootItem::Project(_)));
                if on_project {
                    self.activate(Target::ProjectName(i), Via::Key);
                } else {
                    self.activate(Target::ThreadTitle(i), Via::Key);
                }
            }
            // **`r` RENAMES THE THREAD UNDER THE CURSOR**, and it is the key
            // he was promised in round three and told about again this turn:
            // *"I stil can't do a rename of a thread."* The verb has existed
            // since round two (`SheetAct::Rename` -> `Mutation::
            // SetThreadSummary`); what was missing was the binding.
            //
            // **It opens the sheet's OWN rename field rather than a second
            // rename path.** `Intent::Options` is the one door onto a thread's
            // acts — the same door a click on the title and the `o` key go
            // through — and `store::sheet_row_for` asks the PROJECTION which
            // row carries the act, so this cannot drift from the sheet the way
            // a hard-coded row index would. Two dispatches, one mechanism, and
            // nothing new that could disagree with the sheet about what rename
            // is.
            //
            // A cursor that is not on a thread opens no sheet, so
            // `self.vs.sheet` is `None` and this is a silent no-op — the same
            // free miss `o` takes. The PROJECT sheet has no rename row at all,
            // so `sheet_row_for` returns `None` there rather than firing
            // whatever row is third.
            //
            // # AND SINCE ROUND SIX IT ENDS IN A MODAL, NOT ON THE SHEET
            //
            // His words: *"also I want rename to do a modal. Don't take me to
            // the settings. Just a little modal."*
            //
            // **WHAT HE OVERRULED IS THE DESTINATION, NOT THE REASONING
            // ABOVE.** *"Don't take me to the settings"* is about being MOVED
            // to another screen; what the paragraphs above were protecting is
            // that ONE place knows what rename means, so two surfaces cannot
            // disagree about it. Both survive, and the last two lines of this
            // arm are the whole of the change: every step that decides what
            // rename IS still runs — `Intent::Options` resolves the thread
            // through `hit::activation`, `sheet_row_for` finds the act on the
            // PROJECTION, `SheetAct(i)` opens the sheet's own `rename` buffer
            // prefilled from the summary — and then the MODE is swapped, so
            // the frame draws a box over his list instead of the sheet's
            // screen.
            //
            // **A second `Mutation::` path was the failure mode here and there
            // is not one.** `Intent::SheetCommit` commits both surfaces, so
            // `act_for(SheetAct::Rename)` → `ThreadAct::Rename` →
            // `Mutation::SetThreadSummary` is reached by exactly one route
            // however he got into the box.
            Intent::Rename => {
                self.on_intent(Intent::Options);
                if let Some(i) = self
                    .vs
                    .sheet
                    .as_ref()
                    .and_then(|v| store::sheet_row_for(v, SheetAct::Rename))
                {
                    self.on_intent(Intent::SheetAct(i));
                    // **GUARDED ON THE FIELD ACTUALLY HAVING OPENED.** An
                    // unavailable rename row (the audio interface is down) puts
                    // its own sentence in the notice line and opens nothing —
                    // `sheet_act`'s first branch — and swapping the mode there
                    // would draw an empty modal over a refusal he is trying to
                    // read.
                    if self.vs.mode == Mode::SheetField {
                        self.vs.mode = Mode::RenameModal;
                    }
                } else if self.sheet.is_none() {
                    // **NO SHEET COULD OPEN, SO THE THREAD HAS NO INDEX
                    // HEADER** — one this face created (TUI-P1a, TC-03: *"I
                    // need to be able to access the options for threads even
                    // if they're created as miscellaneous threads"*). The name
                    // is the daemon's, so the box needs no header: it renames
                    // the thread under the cursor directly.
                    if let Some(id) = self.thread_under_cursor() {
                        let now = self.store.label_of(&id);
                        let prefill = if self.store.name_of(&id).is_some() { now } else { String::new() };
                        self.open_naming(NameTarget::Thread { id }, &prefill);
                    }
                }
            }
            Intent::NewThread => self.new_thread(),
            Intent::AddProject => self.add_project_or_pin(),
            Intent::DeregisterProject => self.deregister_project(),
            Intent::CompletePath => self.complete_path(),
            Intent::OpenProjectSheet(i) => self.open_project_sheet(i),
            Intent::NewProjectField => self.open_new_project(),
            Intent::CreateProject => self.create_project(),
            Intent::Unarchive(i) => self.unarchive(i),
            Intent::SheetAct(i) => self.sheet_act(i),
            Intent::SheetCommit => self.sheet_commit(),
            Intent::Reload => self.reload(),
            Intent::Redraw => self.force_repaint("^L"),
            Intent::Quit => self.quit = true,
            Intent::None => {}
        }
        self.clamp_region();
        self.project();
    }

    // ── THE COMPOSER FOLLOWS THE SELECTION ───────────────────────────────
    //
    // > *Basically the text area above should be whichever one we've selected,
    // > so the parent needs to be an item in that list.*

    /// **WHAT THE COMPOSER IS POINTING AT, AND WHETHER IT CAN RECEIVE.**
    ///
    /// Run at the end of every projection, so the band can never be live
    /// against a subject that changed under it.
    ///
    /// **THERE IS NO SEND PATH TO A `direct` SUBAGENT AND NONE IS INVENTED.**
    /// The face's one mutation is `threads/{id}/send {thread_id, text}`; a direct
    /// subagent runs inside Claude Code, has no thread id and exposes no
    /// inbox, and the AUI cannot reach one either — his own words this round:
    /// *"We currently can't submit a message into claude that way, we would
    /// have to interrupt."* So the band is REPLACED by two sentences that say
    /// so, `editable` goes false for the first time, and his draft stays in
    /// `buf` untouched and comes back with the parent. A box that looks ready
    /// and silently drops what he typed is the worst thing this could ship.
    fn follow_subject(&mut self) {
        let stand_in = match &self.tree_subject {
            TreeId::Parent | TreeId::Thread(_) => None,
            TreeId::Agent(id) => match self.agents.iter().find(|a| &a.id == id) {
                // A subject that left the tail is not a subject; the
                // projection has already handed it back to the parent.
                None => None,
                // **`managed` KEEPS A LIVE COMPOSER.** A managed subagent IS a
                // thread and `threads/{id}/send` addresses it by id. This arm is
                // real code that is unreachable today, and no `Managed` row is
                // faked to demonstrate it.
                Some(a) if a.class == SubagentClass::Managed => None,
                Some(a) => Some(StandIn {
                    what: a.label.clone(),
                    back_to: self
                        .vs
                        .tree
                        .rows
                        .first()
                        .map(|r| r.label.clone())
                        .unwrap_or_else(|| "the thread".to_string()),
                }),
            },
        };
        self.vs.composer.editable = stand_in.is_none();
        self.vs.composer.stand_in = stand_in;
    }

    /// Whether the composer is pointing at something that cannot receive.
    fn composer_is_stood_in(&self) -> bool {
        self.vs.composer.stand_in.is_some()
    }

    /// The notice a key gets when it is aimed at a subagent with no inbox.
    fn refuse_to_send(&mut self) {
        let what = self
            .vs
            .composer
            .stand_in
            .as_ref()
            .map(|s| s.what.clone())
            .unwrap_or_default();
        self.notice(
            format!(
                "nothing can be sent to {what} — it runs inside claude code. ← returns to the thread"
            ),
            Severity::Warn,
        );
    }

    /// **DRILL: THE ROW UNDER THE CURSOR BECOMES THE SUBJECT.**
    ///
    /// > *if we click right or enter on them we show their chat history
    /// > instead of the parent.*
    ///
    /// Row 0 is the parent, so the same gesture that opens a subagent's chat
    /// is the one that gives him the thread back.
    fn drill(&mut self, i: usize) {
        let Some(row) = self.vs.tree.rows.get(i) else {
            return;
        };
        let to = row.id.clone();
        if to == self.tree_subject {
            return;
        }
        // **A MANAGED ROW IS EXPANDED AND ENTERED** (TUI-P1b) — it opens its
        // thread rather than projecting into the parent's transcript, which
        // never carried it, and the tree around it is kept.
        if let TreeId::Thread(_) = to {
            self.enter_tree_row(i);
            return;
        }
        // **AN IN-PROCESS SUBAGENT OF ANOTHER THREAD** (an ancestor's, kept on
        // the tree): enter the thread it belongs to first.
        if let Some(open) = self.store.open_id().map(str::to_string) {
            if let Some((owner, at)) = tree_chain(&self.vs.tree.rows, i, &open).pop() {
                if owner != open {
                    self.enter_tree_row(at);
                    self.tree_cursor = to;
                    return;
                }
            }
        }
        self.tree_subject = to.clone();
        self.tree_cursor = to;
        self.vs.tree.subject = self.tree_subject.clone();
        self.vs.tree.cursor = self.tree_cursor.clone();
        // A different chat is a different document. Show it from its newest
        // row, following, exactly as opening a thread does — and thaw, because
        // the freeze anchors a seq in the chat he just left.
        *self.vp_mut() = Viewport::at_bottom();
        self.thaw();
    }

    /// **`←` UN-DRILLS ONE LEVEL BEFORE IT LEAVES THE THREAD.**
    ///
    /// Returns true when it consumed the gesture. Without it, drilling into a
    /// subagent and pressing `←` would eject him from the thread he was
    /// reading — one press, two levels.
    fn undrill(&mut self) -> bool {
        if self.tree_subject == TreeId::Parent {
            return false;
        }
        self.tree_subject = TreeId::Parent;
        self.vs.tree.subject = TreeId::Parent;
        *self.vp_mut() = Viewport::at_bottom();
        self.thaw();
        true
    }

    /// **A REGION THAT VANISHED UNDER THE SELECTION HANDS IT TO THE COMPOSER.**
    ///
    /// The queue empties when he sends; the subagent band will empty when the
    /// last of them leaves the retained tail. Either way the selection has to
    /// go somewhere, and it goes to the BOX and never to the chat: the
    /// composer is the one unconditional neighbour of both conditional stops,
    /// and landing in the chat would turn his next `↓` into a scroll he did
    /// not ask for.
    ///
    /// It runs at the end of every intent rather than in the four handlers
    /// that can empty a queue, because the fifth one is the one that would be
    /// forgotten.
    fn clamp_region(&mut self) {
        let gone = match self.region() {
            Some(Region::Queue) => self.queue_len() == 0,
            Some(Region::Agents) => self.vs.tree.rows.len() < 2,
            _ => false,
        };
        if gone {
            self.select_region(Region::Composer);
        }
        self.vs.queue_cursor = self
            .vs
            .queue_cursor
            .min(self.queue_len().saturating_sub(1));
    }

    /// Up: the list moves its CURSOR, a transcript moves its VIEWPORT, and
    /// selection mode moves its transient pick. Three different things, one
    /// key, because they are the same gesture.
    fn up(&mut self, n: usize) {
        if self.vs.mode == Mode::Sheet {
            if let Some(s) = self.sheet.as_mut() {
                s.cursor = s.cursor.saturating_sub(n);
            }
            // **MOVING OFF A ROW DISARMS IT.** §5's `asks first` is a question
            // about THAT row; walking away from it is an answer.
            self.disarm();
            return;
        }
        if self.vs.mode == Mode::Archived {
            self.archived_cursor = self.archived_cursor.saturating_sub(n);
            return;
        }
        match self.vs.mode {
            Mode::Projects | Mode::SeeAll(_) | Mode::List => {
                let c = self.cursor().saturating_sub(n);
                self.set_cursor(c);
                self.follow_cursor();
            }
            _ => {
                // FREEZE BEFORE MOVING. The anchor has to be where the content
                // was when he left the bottom, not where it is after the move.
                self.freeze();
                let vp = self.vp_mut();
                vp.from_bottom = vp.from_bottom.saturating_add(n);
                vp.follow = false;
            }
        }
    }

    fn down(&mut self, n: usize) {
        if self.vs.mode == Mode::Sheet {
            // CLAMPED BY THE PROJECTION, which is the one place that knows how
            // many acts there are.
            let last = self
                .vs
                .sheet
                .as_ref()
                .map_or(0, |v| v.rows.len().saturating_sub(1));
            if let Some(s) = self.sheet.as_mut() {
                s.cursor = s.cursor.saturating_add(n).min(last);
            }
            self.disarm();
            return;
        }
        if self.vs.mode == Mode::Archived {
            let last = self.vs.archived.rows.len().saturating_sub(1);
            self.archived_cursor = self.archived_cursor.saturating_add(n).min(last);
            return;
        }
        match self.vs.mode {
            // **THE PAGE MOVES A CURSOR, LIKE THE TWO LISTS IT IS ONE OF.**
            // `is_list()` already says so; this is the arm that acts on it, and
            // leaving it out would have made `↓` on the page scroll the window
            // under a cursor that never moved.
            Mode::Projects | Mode::SeeAll(_) | Mode::List => {
                let c = self.cursor().saturating_add(n);
                self.set_cursor(c);
                self.follow_cursor();
            }
            _ => {
                let vp = self.vp_mut();
                vp.from_bottom = vp.from_bottom.saturating_sub(n);
                if vp.from_bottom == 0 {
                    *vp = Viewport::at_bottom();
                    self.thaw();
                }
            }
        }
    }

    /// The list's viewport follows its cursor. `from_bottom` is measured from
    /// the END, and the cursor is an index from the START, so this is the one
    /// place the two coordinate systems meet.
    fn follow_cursor(&mut self) {
        if !self.is_list() {
            return;
        }
        // **THREE COORDINATE SYSTEMS MEET HERE AT THE ROOT, NOT TWO.** The
        // viewport counts DRAWN rows; the cursor counts SELECTABLE ones; and
        // the root surface interleaves band headers and spacers between the
        // two, so at the root they are different numbers. Both are resolved
        // through `ProjectList`, off the one `root_lines` layout, rather than
        // re-counted here — a second count of that surface is exactly how a
        // scroll ends up one row out.
        let (n, cursor) = match self.view() {
            Mode::Projects | Mode::SeeAll(_) => (
                self.vs.projects.drawn_len(),
                self.vs.projects.drawn_row_of(self.root_cursor()),
            ),
            _ => {
                let n = self.list_len();
                (n, self.cursor().min(n.saturating_sub(1)))
            }
        };
        let rows = self.last_rows.max(1);
        // CLAMPED FIRST. `usize::MAX` is a legal `from_bottom` — it is how
        // "the top" is spelled, by `Intent::Top` and by the root menu's
        // opening viewport — and `rows + from_bottom` overflows on it.
        let from_bottom = self.vp().from_bottom.min(n.saturating_sub(rows));
        let top = n.saturating_sub(rows + from_bottom);
        let new = if cursor < top {
            n.saturating_sub(rows).saturating_sub(cursor)
        } else if cursor >= top.saturating_add(rows) {
            n.saturating_sub(cursor + 1)
        } else {
            from_bottom
        };
        let vp = self.vp_mut();
        vp.from_bottom = new;
        vp.follow = new == 0;
        if new == 0 {
            vp.new_since = 0;
        }
    }

    // ── responses ────────────────────────────────────────────────────────

    fn on_transcript(&mut self, thread_id: String, result: Result<(Vec<LedgerRow>, u64), String>) {
        match result {
            Ok((rows, from_seq)) => {
                tracing::info!(%thread_id, rows = rows.len(), from_seq, "transcript loaded");
                self.store.load_transcript(&thread_id, rows, from_seq);
                if self.store.open_id() == Some(thread_id.as_str()) {
                    self.thread_vp = Viewport::at_bottom();
                    self.thread_frozen = None;
                }
            }
            Err(why) => {
                tracing::error!(%thread_id, %why, "transcript load failed");
                self.store.transcript_failed(&thread_id, why.clone());
                self.notice(
                    format!("could not load the thread: {why}"),
                    Severity::Critical,
                );
            }
        }
        self.project();
    }

    fn on_queue(&mut self, thread_id: String, result: Result<(Vec<Value>, Vec<Value>), String>) {
        match result {
            Ok((waiting, failed)) => self.store.queue_loaded(&thread_id, &waiting, &failed),
            Err(why) => self.store.queue_failed(&thread_id, why),
        }
        self.project();
    }

    fn on_fleet(&mut self, result: Result<(Vec<Value>, Vec<Value>), String>) {
        match result {
            Ok((statuses, threads)) => {
                let capped = statuses.len() as u64 >= store::STATUS_WINDOW;
                self.store
                    .apply(TransportEvent::Fleet { statuses, capped }, Utc::now());
                let capped = threads.len() as u64 >= FLEET_WINDOW;
                self.store.apply(
                    TransportEvent::Threads {
                        rows: threads,
                        capped,
                    },
                    Utc::now(),
                );
            }
            Err(why) => self.notice(format!("re-paint failed: {why}"), Severity::Critical),
        }
        self.project();
    }

    /// **APPLY ONE EVENT.** The loop body, as a method.
    ///
    /// [`run`] is now "wait for an event, apply it, pay the clipboard, redraw",
    /// and everything that decides what an event MEANS is in here. That is a
    /// library boundary rather than test scaffolding: `0037` argued that the
    /// test of a real lib is whether a second face could be built on it, and a
    /// second face needs exactly this — somewhere to hand an event and
    /// something to draw afterwards ([`App::state`]).
    ///
    /// It is also what lets the SEND PATH be proven against a scripted fake
    /// daemon rather than only by running the face at his real one.
    ///
    /// Returns whether the loop should keep going.
    pub fn apply(&mut self, ev: Ev) -> bool {
        if self.projection_owed && !matches!(ev, Ev::Transport(_)) {
            self.settle_projection();
        }
        match ev {
            // ONE TERMINAL READ, AS ONE BATCH. The paste rule lives inside.
            Ev::Input(batch) => self.on_input(batch),
            Ev::AutoScroll => self.on_autoscroll(),
            Ev::NoticeExpired => self.vs.notice = None,
            Ev::ComposeIdle => self.on_compose_idle(),
            Ev::HoldIdle => self.on_hold_idle(),
            Ev::IndexSettled => self.on_index_settled(),
            // Nothing to apply. The redraw at the bottom of the loop IS the
            // handling — see `Ev::LinkTick`.
            Ev::LinkTick => {}
            Ev::DownRestart(note) => {
                self.restarting = false;
                self.restart_note = Some(note);
                self.project();
            }
            Ev::InputClosed => {
                tracing::error!("input pump closed; leaving");
                return false;
            }
            Ev::Transport(t) => self.on_transport(t),
            // **THE SECOND SOCKET SAYS SOMETHING.** Nothing here touches a
            // rendered fact: a link change only re-projects (so the sheet's row
            // words follow it), and an `Invalidate` only re-reads the files the
            // face already reads.
            Ev::Aui(a) => match a {
                crate::aui::AuiEvent::Reach(_) => {}
                crate::aui::AuiEvent::Invalidate { node } => self.on_invalidate(&node),
            },
            Ev::ActDone {
                what,
                result,
                clears,
            } => match result {
                // A momentary confirmation, which is the ONE thing green is
                // for on this face (palette rule 2).
                Ok(()) => {
                    // **ONLY AN ACCEPTED ACT EMPTIES A BOX.** The composer's
                    // rule: a box cleared optimistically eats his words when
                    // the socket was down.
                    if clears == Some(Clears::NewProjectName) {
                        self.new_project = None;
                    }
                    // **RE-READ THE ORDER FROM ITS OWNER.** The AUI has just
                    // rewritten `.mind/threads.json`; this is the face asking
                    // the file what the order now is rather than assuming what
                    // its own act did to it. The next `project()` then puts the
                    // cursor back on the thread through `App::follow_pin`, so
                    // the row and the cursor arrive together.
                    if clears == Some(Clears::PinMove) {
                        self.store.load_projects();
                    }
                    // **AND SO DOES A REGISTRY WRITE.** Same argument, one
                    // file over: this face has just changed
                    // `~/.cauldron/projects.json` and everything it believes
                    // about the project band is one revision old. The buffer
                    // is emptied here and NOT on the send, because a refused
                    // registration must leave his path on screen to be fixed
                    // — `+ new project`'s own rule.
                    if clears == Some(Clears::RegistryWrite) {
                        self.new_project = None;
                        self.store.load_projects();
                    }
                    self.notice(format!("{what} — done"), Severity::Ok);
                    // **THE THREAD EXISTS; NOW OPEN IT.** On the ACK and never
                    // on the send — a view opened over a refused create is a
                    // screen that pretends to be a thread, and he would type
                    // into it.
                    //
                    // The second notice REPLACES the "— done" above on purpose:
                    // done is true and incomplete, and the incompleteness is
                    // the thing he has to hear. A thread the daemon made has a
                    // header in no project index, so it is not on the list and
                    // nothing this face is allowed to write would put it there.
                    if clears == Some(Clears::ThreadCreated) {
                        if let Some(id) = self.pending_new_thread.take() {
                            self.open_thread(id);
                            // TUI-P1a: it IS on his recent list now — this face
                            // stamped it as his (`app_id`), so the band shows it
                            // as soon as he has said something in it.
                            self.notice(
                                "new thread — it joins your recent list with its first message",
                                Severity::Ok,
                            );
                            self.project();
                        }
                    }
                }
                // **THE OWNER'S OWN SENTENCE, RELAYED VERBATIM.** `0060`: the
                // AUI's refusals are already written for a human, and the
                // sharpest case is `remove` on a real directory — the v2
                // handler's refusal explains a rule this face does not know,
                // so paraphrasing it would put the face between him and the
                // only party that knows what went wrong.
                Err(why) => {
                    // A refused create minted an id and nothing else; holding
                    // it would leave the next ack opening a thread that was
                    // never made.
                    if clears == Some(Clears::ThreadCreated) {
                        self.pending_new_thread = None;
                    }
                    self.notice(format!("{what}: {why}"), Severity::Critical)
                }
            },
            Ev::Transcript { thread_id, result } => self.on_transcript(thread_id, result),
            Ev::TranscriptBodies { thread_id, rows } => {
                // Same rows, completed: `load_transcript` replaces by seq and
                // leaves the viewport where he put it.
                let from = rows.first().map(|r| r.seq).unwrap_or(0);
                let from = self.store.open_from_seq().unwrap_or(from);
                self.store.load_transcript(&thread_id, rows, from);
                self.project();
            }
            Ev::Queue { thread_id, result } => self.on_queue(thread_id, result),
            Ev::Fleet { result } => self.on_fleet(result),
            Ev::Sent { thread_id, result } => self.on_sent(thread_id, result),
            Ev::Recorded { result } => self.on_recorded(result),
            Ev::Transcribed { path, result } => self.on_transcribed(path, result),
            // **ONLY THE CURRENT PLAYBACK MAY SPEAK.** A clip he stopped with a
            // second press, or replaced by pressing another row, is killed —
            // and a killed child exits non-zero. Reporting that as a failure
            // would put "could not be played" on screen every time he stopped
            // something deliberately.
            Ev::Played { path, result } => {
                if self.vs.playing.as_deref() == Some(path.as_str()) {
                    self.vs.playing = None;
                    self.playing = None;
                    if let Err(why) = result {
                        self.notice(why, Severity::Critical);
                    }
                    self.project();
                }
            }
        }
        !self.quit
    }

    /// What a face would draw. The whole of it; there is no second channel.
    pub fn state(&self) -> &ViewState {
        &self.vs
    }

    /// Open a thread by id, as `→` on its row does.
    pub fn open(&mut self, id: impl Into<String>) {
        self.open_thread(id.into());
        self.project();
    }

    /// Lay a frame out WITHOUT a terminal, so the hit map, the band heights and
    /// the composer's own width exist for a caller that is not drawing.
    ///
    /// The three lines it keeps are the three [`App::redraw`] keeps after the
    /// draw; everything a terminal is needed for is what it leaves out. A test
    /// that had to own a pty could not assert on a drag at all, and the drag
    /// is the thing most worth asserting on.
    pub fn lay(&mut self, w: u16, h: u16) {
        let (_, hits) = tui_render::frame_with_hits(&self.vs, Utc::now(), w, h);
        self.hits = hits;
        self.last_size = (w, h);
        self.last_rows = tui_render::bands(&self.vs, w, h).content.max(1);
    }

    /// **The ONE call site that reads the frame area** (`0023`
    /// `defect_classes` CLASS 3). `grep -n '\.area()' apps/tui/src` returns
    /// this line and no other, so there is no second width or height for the
    /// renderer's to disagree with.
    fn redraw(&mut self, terminal: &mut Tui) -> Result<()> {
        // **SYNCHRONIZED OUTPUT AROUND THE WHOLE FRAME.** His client negotiates
        // `sync`, so this costs one escape pair and stops the tear on a band
        // that GROWS and SHRINKS under the cursor as he types. It does not
        // replace the repaint law below it; `0018`'s finding is unchanged.
        let mut out = std::io::stdout();
        let _ = out.write_all(term::FRAME_BEGIN.as_bytes());
        let r = self.draw_inner(terminal);
        let _ = out.write_all(term::FRAME_END.as_bytes());
        let _ = out.flush();
        r
    }

    fn draw_inner(&mut self, terminal: &mut Tui) -> Result<()> {
        if std::mem::take(&mut self.needs_full_repaint) {
            terminal.clear()?;
        }
        let now = Utc::now();
        let vs = &self.vs;
        let mut size = (0u16, 0u16);
        let mut hits = None;
        terminal.draw(|f| {
            let area = f.area();
            size = (area.width, area.height);
            // **THE HIT MAP COMES OUT OF THE DRAW.** Not out of a second call
            // that would re-derive the layout: a click resolved against a map
            // built by a different pass is the mouse's version of `0023`
            // `defect_classes` CLASS 3, and nothing on screen would look wrong
            // while it was happening.
            let (lines, map) = tui_render::frame_with_hits(vs, now, area.width, area.height);
            hits = Some(map);
            f.render_widget(Paragraph::new(lines), area);
        })?;
        if let Some(map) = hits {
            self.hits = map;
        }
        self.last_size = size;
        // **THE PAGE HEIGHT IS ASKED FOR, NOT RE-DERIVED.** This used to be
        // `height - 2 - COMPOSER_MIN_ROWS` computed right here, which is a
        // SECOND derivation of a number the render crate had already worked
        // out — and it had already drifted from the first: it never subtracted
        // the banner, so with the link down `PageDown` moved by one row more
        // than the transcript held, and it keyed off the view UNDER an overlay
        // so it subtracted a composer that was not on the screen. Both are
        // `0023` `defect_classes` CLASS 3, and both stop being possible when
        // there is one function and two callers.
        self.last_rows = tui_render::bands(&self.vs, size.0, size.1).content.max(1);
        Ok(())
    }
}

/// Run the face. Returns when he quits; the caller restores the terminal.
pub async fn run(
    terminal: &mut Tui,
    home: String,
    tz_offset_minutes: i32,
    // Whether `~/.tui` was absent when `main` looked, BEFORE the log created
    // it. A value and not a probe, for the same reason `home` and the timezone
    // are values: it is ambient state, and a layer that reads its own ambient
    // state is a layer no test can put anywhere.
    first_run: bool,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<Ev>(256);

    // ── THE TRANSPORT ──
    //
    // `DIR-7`: subscribed, not polled. The actor owns its own reconnect ladder
    // and never needs asking; this loop only renders what it is told.
    //
    // The forwarder is deliberately a plain `while let` and not a `select!`
    // arm: both sends are AWAITED on bounded channels, so the path from the
    // socket to the store applies back-pressure rather than dropping — which is
    // what makes "the cursor advances only on an event actually delivered" true
    // by construction rather than by hope.
    let (handle, mut trx) = mirror::spawn();
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = trx.recv().await {
                if tx.send(Ev::Transport(ev)).await.is_err() {
                    return; // the face is leaving
                }
            }
        });
    }

    // **THE ENDPOINT IS THE SEAM'S TO SAY**, derived from
    // `TUI_THREADS_CHASSIS_PORT` and nothing else. It is read here only to put
    // on the screen, which is why it arrives as a string and not as a
    // parameter: there is no second place a URL could be chosen from.
    let mut app = App::new(
        handle,
        tx.clone(),
        crate::seam::link::ws_url(),
        home,
        tz_offset_minutes,
    );
    app.store.first_run = first_run;

    // ── THE SECOND SOCKET (T-27) ──
    //
    // The AUI's proto-v2 mutation listener on 9010. **The face does not wait
    // for it and does not care whether it ever comes up**: it is write-side
    // plus an `Invalidate` hint, so an absent 9010 costs exactly the acts it
    // owns and nothing on screen. That is the condition `0060` binds this whole
    // design to, and starting it here — after the first index read is already
    // scheduled, with no preflight and no error path — is what keeps it true.
    {
        let (aui_handle, mut arx) = aui::spawn(aui::Config::live());
        app.set_aui(aui_handle);
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = arx.recv().await {
                if tx.send(Ev::Aui(ev)).await.is_err() {
                    return; // the face is leaving
                }
            }
        });
    }
    // **WHAT THE TERMINAL CAN ACTUALLY REPORT, ASKED ONCE, BEFORE THE FIRST
    // FRAME.** Every key printed on the surface is filtered through this, so a
    // binding that cannot work here is never shown — and the probe runs here
    // rather than inside `term::enter` so the answer is a value this loop owns
    // rather than a global somebody could read stale.
    //
    // **IT RUNS BEFORE THE PUMP IS SPAWNED, AND THE ORDER IS LOAD-BEARING NOW.**
    // Outside tmux this probe READS STDIN, waiting for the terminal's reply —
    // and since `0050` the pump reads stdin too, on its own thread. Two readers
    // on one descriptor race for that reply, and the one that loses waits out
    // its timeout for an answer the other already swallowed. Under the old pump
    // crossterm owned both sides and serialised them internally; it does not
    // any more, so the ordering is the mechanism.
    app.set_caps(term::negotiate());
    crate::input::spawn_pump(tx.clone());
    // **THE INDEXES ARE READ BEFORE THE FIRST FRAME.** They are the list's
    // source, not a decoration of it, so the root menu is a real answer on the
    // first paint rather than a placeholder that fills in.
    app.store.load_projects();
    app.project();
    // Set-point 1: startup. The terminal was somebody else's a moment ago.
    app.force_repaint("startup");
    app.redraw(terminal)?;
    let mut last_paint = std::time::Instant::now();
    let mut closed_after_paint = false;

    loop {
        // **THE THREE PLACES THIS LOOP HAS A CLOCK, AND EVERY ONE IS ARMED
        // ONLY BY SOMETHING ON SCREEN.** A pointer held still at the edge of
        // the band produces no terminal reports at all, so the autoscroll tick
        // is what carries a selection past the top of the screen; a composer
        // that has just been typed into owes an undo-run boundary and a journal
        // write; and a standing NOTICE owes a frame at the moment it expires,
        // because a face sitting still would otherwise keep showing it until
        // some unrelated event happened to repaint. With none pending the loop
        // blocks on `recv()` exactly as it always has — no timer, no polling,
        // no wakeups.
        //
        // **Written as a LIST rather than as nested matches**, which is the
        // shape that made adding the third one safe: the old form paired the
        // deadline with the event by position, so a `min` over two durations
        // could fire the wrong one the moment there were three.
        let mut timers: Vec<(Duration, Ev)> = Vec::new();
        if app.drag.and_then(|d| d.autoscroll).is_some() {
            timers.push((AUTOSCROLL_TICK, Ev::AutoScroll));
        }
        if let Some(d) = app.compose_deadline() {
            timers.push((d, Ev::ComposeIdle));
        }
        if let Some(d) = app.notice_deadline() {
            timers.push((d, Ev::NoticeExpired));
        }
        // **THE FOURTH CLOCK, and it is armed by something on screen exactly
        // as the other three are**: a link line that states a duration owes a
        // frame at the moment that duration would read differently. Without
        // it the face would keep showing the second the socket died until the
        // reconnect ladder happened to produce an event.
        if let Some(d) = app.link_deadline() {
            timers.push((d, Ev::LinkTick));
        }
        // **THE FIFTH CLOCK — the one that stands in for a key release his
        // terminal cannot report.** A hold ends in SILENCE, and silence is the
        // one thing no event can carry, so the face has to wake itself up to
        // notice it. Armed only while a hold that opened a capture is running;
        // see `App::hold_deadline`.
        if let Some(d) = app.hold_deadline() {
            timers.push((d, Ev::HoldIdle));
        }
        // **THE SIXTH CLOCK — the one that pays a coalesced `Invalidate`
        // burst.** It is armed by something OFF screen rather than on it, and
        // that is the point: a durable index write has happened that the face
        // has not drawn yet, and the only thing that can tell him is the face
        // waking itself up. Armed only while a frame was actually swallowed;
        // see `App::invalidate_deadline` and `Ev::IndexSettled`.
        if let Some(d) = app.invalidate_deadline() {
            timers.push((d, Ev::IndexSettled));
        }
        if closed_after_paint {
            break;
        }
        // **THERE IS NO FOURTH CLOCK FOR PLAYBACK, DELIBERATELY.** `afplay`
        // blocks for the whole clip, so it is spawned detached and a WAITER
        // THREAD sends `Ev::Played` when it finishes — the same shape
        // `spawn_send` and the transcriber already use. A timer would have been
        // a second mechanism for a fact one thread already knows.
        let ev = match timers.into_iter().min_by_key(|(d, _)| *d) {
            Some((d, fires)) => match tokio::time::timeout(d, rx.recv()).await {
                Ok(Some(ev)) => ev,
                Ok(None) => break,
                Err(_) => fires,
            },
            None => match rx.recv().await {
                Some(ev) => ev,
                None => break,
            },
        };
        let t_apply = std::time::Instant::now();
        let mut batch = 1usize;
        app.begin_batch();
        let mut go = app.apply(ev);
        // ── ONE REDRAW PER EVENT *BATCH* ──────────────────────────────────
        //
        // **The repaint law did not change; the unit it is measured in did.**
        // Live assembly turned a stream of deltas from rows-nobody-wanted into
        // the message he is reading, and deltas arrive in BURSTS: `0047`
        // measured 125 stream chunks per assistant message. Projecting a
        // four-thousand-row tail once per chunk is work whose only visible
        // product is lag.
        //
        // Draining first changes no ordering — every event is still applied,
        // in arrival order, through the same `apply` — and it changes nothing
        // he can see except the latency. `try_recv` and not `recv`: this loop
        // must never WAIT for a batch to fill, or a quiet moment would sit on
        // the last event instead of drawing it.
        //
        // The bound exists because the channel is bounded and the producer is
        // a socket: without it, a replay of a long thread could hold the paint
        // off for as long as the backlog lasted, which is the failure this is
        // supposed to prevent seen from the other end.
        //
        // **AND THE PAINT IS ON A FIXED CADENCE** (THREAD-LOAD-LATENCY F2): no
        // two paints closer than [`FRAME_INTERVAL`]. Events that arrive before
        // the next paint is due join this batch, so a stream of deltas costs one
        // projection and one paint per frame, not one per delta. A face that
        // has been quiet for a frame paints at once — the wait only exists
        // while something is already arriving faster than the eye can see.
        let paint_at = last_paint + FRAME_INTERVAL;
        let mut closed = false;
        while go && batch < PAINT_BATCH {
            match rx.try_recv() {
                Ok(next) => {
                    batch += 1;
                    go = app.apply(next);
                    continue;
                }
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    closed = true;
                    break;
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
            let now = std::time::Instant::now();
            if now >= paint_at {
                break;
            }
            match tokio::time::timeout(paint_at - now, rx.recv()).await {
                Ok(Some(next)) => {
                    batch += 1;
                    go = app.apply(next);
                }
                Ok(None) => {
                    closed = true;
                    break;
                }
                Err(_) => break,
            }
        }
        app.end_batch();
        let apply_us = t_apply.elapsed().as_micros();
        app.pay_the_clipboard();
        if !go {
            break;
        }
        let t_draw = std::time::Instant::now();
        app.redraw(terminal)?;
        last_paint = std::time::Instant::now();
        let draw_us = t_draw.elapsed().as_micros();
        if apply_us + draw_us >= 16_000 {
            crate::diag!("loop", "SLOW events={batch} apply_us={apply_us} redraw_us={draw_us}");
        }
        closed_after_paint = closed;
    }
    Ok(())
}

/// **WHAT THE HEADLESS RUNNER IS TOLD.** See [`run_headless`].
#[derive(Debug, Clone)]
pub struct Headless {
    /// The thread to open, exactly as `→` on its row would.
    pub thread: Option<String>,
    /// How long to watch before leaving.
    pub seconds: u64,
    pub width: u16,
    pub height: u16,
    /// Where the frames go; `None` is stdout.
    pub out: Option<std::path::PathBuf>,
    /// Write EVERY frame, not only the ones that differ from the last.
    pub every: bool,
}

/// **THE FACE WITH NO TERMINAL — every frame it would have drawn, as text.**
///
/// The same `App`, the same seam against the same daemon, the same `apply`
/// and the same `tui_render::frame_with_hits` the live loop draws with; what
/// it leaves out is exactly what needs a terminal — the input pump, the
/// alternate screen, the clipboard and the 9010 socket. It therefore cannot
/// SEND anything: there is no input, and nothing in here calls a verb. It
/// opens one thread (a `threads/log` subscribe and a `threads` hold, the
/// same reads the live face makes) and dumps each distinct frame, stamped
/// with milliseconds since start, until `seconds` have passed.
///
/// So an agent can see exactly which characters land on screen while the
/// operator's own face is doing whatever it is doing, without touching it.
pub async fn run_headless(opts: Headless, home: String, tz_offset_minutes: i32) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<Ev>(256);
    let (handle, mut trx) = mirror::spawn();
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = trx.recv().await {
                if tx.send(Ev::Transport(ev)).await.is_err() {
                    return;
                }
            }
        });
    }
    let mut app = App::new(
        handle,
        tx.clone(),
        crate::seam::link::ws_url(),
        home,
        tz_offset_minutes,
    );
    app.store.load_projects();
    if let Some(id) = &opts.thread {
        app.open(id.clone());
    } else {
        app.project();
    }
    let mut sink: Box<dyn Write> = match &opts.out {
        Some(p) => Box::new(std::io::BufWriter::new(std::fs::File::create(p)?)),
        None => Box::new(std::io::stdout()),
    };
    let start = std::time::Instant::now();
    let deadline = start + Duration::from_secs(opts.seconds);
    let mut last: Option<String> = None;
    let mut n = 0u64;
    let mut dump = |app: &mut App, sink: &mut Box<dyn Write>, why: &str| -> Result<()> {
        let (lines, _) =
            tui_render::frame_with_hits(&app.vs, Utc::now(), opts.width, opts.height);
        let text: String = lines
            .iter()
            .map(|l| {
                let s: String = l.spans.iter().map(|sp| sp.content.as_ref()).collect();
                format!("{}\n", s.trim_end())
            })
            .collect();
        if opts.every || last.as_deref() != Some(text.as_str()) {
            n += 1;
            writeln!(
                sink,
                "=== frame {n} +{}ms ({why}) {}x{} ===",
                start.elapsed().as_millis(),
                opts.width,
                opts.height
            )?;
            sink.write_all(text.as_bytes())?;
            sink.flush()?;
            crate::diag!("view", "FRAME n={n} why={why}");
            last = Some(text);
        }
        Ok(())
    };
    dump(&mut app, &mut sink, "startup")?;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let ev = match tokio::time::timeout(left, rx.recv()).await {
            Ok(Some(ev)) => ev,
            Ok(None) | Err(_) => break,
        };
        let why = match &ev {
            Ev::Transport(t) => format!("transport {}", transport_label(t)),
            Ev::TranscriptBodies { rows, .. } => format!("bodies rows={}", rows.len()),
            Ev::Transcript { result, .. } => match result {
                Ok((rows, _)) => format!("transcript rows={}", rows.len()),
                Err(e) => format!("transcript error: {e}"),
            },
            _ => "event".to_string(),
        };
        let t_apply = std::time::Instant::now();
        let mut batch = 1usize;
        app.begin_batch();
        app.apply(ev);
        for _ in 0..PAINT_BATCH {
            match rx.try_recv() {
                Ok(next) => {
                    batch += 1;
                    app.apply(next);
                }
                Err(_) => break,
            }
        }
        app.end_batch();
        let apply_us = t_apply.elapsed().as_micros();
        let t_dump = std::time::Instant::now();
        dump(&mut app, &mut sink, &why)?;
        crate::diag!(
            "loop",
            "BATCH events={batch} apply_us={apply_us} render_us={} why={why}",
            t_dump.elapsed().as_micros()
        );
    }
    writeln!(sink, "=== end: {n} distinct frames in {}ms ===", start.elapsed().as_millis())?;
    sink.flush()?;
    Ok(())
}

fn transport_label(t: &TransportEvent) -> String {
    match t {
        TransportEvent::Frame {
            msg: crate::proto::ServerMsg::Event { seq, event, .. },
            ..
        } => format!("row seq={seq} kind={}", event.kind()),
        other => {
            let d = format!("{other:?}");
            d.split([' ', '{', '(']).next().unwrap_or("").to_string()
        }
    }
}

/// How many further events one wake-up may absorb before the screen is owed a
/// frame. Large enough that a generation-speed delta burst is one paint, small
/// enough that a replay backlog cannot starve the display.
const PAINT_BATCH: usize = 256;

/// **THE PAINT CADENCE** (THREAD-LOAD-LATENCY F2): the shortest gap between two
/// paints, ~60 per second. Under it, a burst coalesces into one paint.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// **THE TREE'S OWN PATH TO ROW `i`** (TUI-P1b) — `(thread id, row index)`
/// for every THREAD row from the root down to row `i`'s thread: row `i`
/// itself when it is a thread row, else the thread that owns the in-process
/// subagent on it. [`TreeId::Parent`] is the open thread, `open`.
fn tree_chain(rows: &[tui_render::TreeRow], i: usize, open: &str) -> Vec<(String, usize)> {
    let Some(row) = rows.get(i) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut depth = row.depth + 1;
    for j in (0..=i).rev() {
        let r = &rows[j];
        if r.depth >= depth {
            continue;
        }
        depth = r.depth;
        match &r.id {
            TreeId::Parent => out.push((open.to_string(), j)),
            TreeId::Thread(t) => out.push((t.clone(), j)),
            TreeId::Agent(_) => {}
        }
        if depth == 0 {
            break;
        }
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app() -> App {
        // The transport is spawned but never connected: these tests are about
        // the loop's state transitions, and a real socket would make them
        // depend on a daemon being up.
        // **A SEAM POINTED AT PORT 1, WHICH NOTHING LISTENS ON.**
        // `mirror::spawn_at` and not `mirror::spawn`, and the difference is the
        // operator's money: the bare `spawn` takes its port from the
        // environment and falls back to 3653, which is his LIVE daemon. See
        // `mirror::TestPorts`. `spawn_dead` needs no guard held: it only ever
        // writes the same two values, so two callers agree.
        let (handle, _rx) = mirror::spawn_dead();
        let (tx, _erx) = mpsc::channel::<Ev>(16);
        App::new(
            handle,
            tx,
            "ws://127.0.0.1:1/ws".into(),
            "/Users/x".into(),
            0,
        )
    }

    /// The same shape `store`'s tests use: a root the face would have read.
    fn proj(name: &str, ids: &[&str]) -> crate::index::Project {
        crate::index::Project {
            name: name.to_string(),
            root: std::path::PathBuf::from(format!("/Users/x/minds/{name}")),
            threads: ids
                .iter()
                .enumerate()
                .map(|(i, id)| crate::index::IndexThread {
                    id: (*id).to_string(),
                    session_id: (*id).to_string(),
                    legacy_ids: Vec::new(),
                    summary_locked: false,
                    summary_set_at: None,
                    created_at: None,
                    summary: format!("thread {id}"),
                    // **`{:02}` AND A MINUTE FIELD, not `2026-09-0{i+1}`.**
                    // The old spelling produced `2026-09-010` on the tenth
                    // thread and panicked in the fixture — harmless while no
                    // test had ten, and a trap the moment one did (the ten-row
                    // band default). Minutes give 60 distinct stamps.
                    //
                    // Both clocks, same instant: he spoke in it then. The
                    // recents band orders on `last_user_input_at`, so a fixture
                    // that left it `None` would stop exercising the order.
                    last_user_input_at: Some(
                        chrono::DateTime::parse_from_rfc3339(&format!(
                            "2026-09-01T00:{:02}:00Z",
                            i.min(59)
                        ))
                        .expect("fixture stamp")
                        .with_timezone(&Utc),
                    ),
                    last_turn_at: Some(
                        chrono::DateTime::parse_from_rfc3339(&format!(
                            "2026-09-01T00:{:02}:00Z",
                            i.min(59)
                        ))
                        .expect("fixture stamp")
                        .with_timezone(&Utc),
                    ),
                    turn_count: 1,
                    closed: false,
                    pinned: false,
                    pin_order: 0,
                    queued_count: 0,
                })
                .collect(),
            index: crate::index::IndexState::Read,
        }
    }

    /// **A UUID PER FIXTURE LABEL, STABLE ACROSS CALLS.**
    ///
    /// `Mutation::MovePin` keys a thread by `Uuid` and `acts::route` refuses a
    /// legacy non-uuid id LOUDLY — "this thread's id (p2) is not one the audio
    /// interface can address" — which is correct behaviour and was measured
    /// here the first time this fixture used bare labels. A reorder test on
    /// non-uuid ids would never reach the socket at all and would prove the
    /// wrong refusal.
    fn uid(label: &str) -> String {
        let n = label.trim_start_matches('p');
        format!("0000000{n}-0000-4000-8000-000000000000")
    }

    /// A root whose threads are PINNED, in the `pin_order` the slice gives.
    /// Labels are `p1`, `p2`, … and become real uuids through [`uid`].
    fn pinned_proj(name: &str, pins: &[(&str, i64)]) -> crate::index::Project {
        let ids: Vec<String> = pins.iter().map(|(id, _)| uid(id)).collect();
        let mut p = proj(name, &ids.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        for (t, (_, order)) in p.threads.iter_mut().zip(pins) {
            t.pinned = true;
            t.pin_order = *order;
        }
        p
    }

    /// A root of `n` dated threads, so the recent band has something to cap.
    fn many_proj(name: &str, n: usize) -> crate::index::Project {
        let mut p = proj(name, &["seed"]);
        p.threads = (0..n)
            .map(|i| crate::index::IndexThread {
                id: format!("{name}-{i:02}"),
                session_id: format!("{name}-{i:02}"),
                legacy_ids: Vec::new(),
                summary_locked: false,
                summary_set_at: None,
                created_at: None,
                summary: format!("thread {i}"),
                last_user_input_at: Some(
                    chrono::DateTime::parse_from_rfc3339(&format!("2026-01-01T00:{i:02}:00Z"))
                        .expect("fixture stamp")
                        .with_timezone(&Utc),
                ),
                last_turn_at: Some(
                    chrono::DateTime::parse_from_rfc3339(&format!("2026-01-01T00:{i:02}:00Z"))
                        .expect("fixture stamp")
                        .with_timezone(&Utc),
                ),
                turn_count: 1,
                closed: false,
                pinned: false,
                pin_order: 0,
                queued_count: 0,
            })
            .collect();
        p
    }

    /// Put the root cursor on the band header for `b`, through the ONE
    /// resolution the renderer and the hit map read.
    fn cursor_to_band(a: &mut App, b: tui_render::BandId) {
        let i = (0..a.vs.projects.selectable())
            .find(|&i| a.vs.projects.item(i) == Some(RootItem::Band(b)))
            .unwrap_or_else(|| panic!("no {b:?} band header on the root"));
        a.on_intent(Intent::Select(i));
    }

    fn cursor_to(a: &mut App, want: RootItem) {
        let i = (0..a.vs.projects.selectable())
            .find(|&i| a.vs.projects.item(i) == Some(want))
            .unwrap_or_else(|| panic!("no {want:?} on the root"));
        a.on_intent(Intent::Select(i));
    }

    fn notice(a: &App) -> String {
        a.vs.notice
            .as_ref()
            .map(|n| n.text.clone())
            .unwrap_or_default()
    }

    // ── `see all` IS A PAGE, and the trail is the back stack rendered ─────

    /// **`→` ON A BAND HEADER AND `→` ON THE `see all` FOOT ARE TWO DOORS ONTO
    /// ONE PAGE, AND THEY LAND IN DIFFERENT PLACES.** `I-1`.
    ///
    /// His ask, verbatim: *"let's replace 'show more' with 'see all' and it
    /// takes us to a dedicated page with just that band and all results. …
    /// Going right on a band title does the same thing."* — "the same thing" is
    /// why there are two doors, and the landing row is why they are not the same
    /// door twice: the foot lands on the FIRST ROW THE ROOT COULD NOT SHOW HIM,
    /// which is the row he was reaching for when he pressed it.
    ///
    /// The key comes from the TABLE (`right(&a)`), never hand-written, so this
    /// asserts the binding and not a test's idea of it.
    ///
    /// FORCED RED by pointing the `RootItem::Band(b)` arm of `Intent::Open` at
    /// `open_band_page(b, tui_render::BAND_ROWS)` — the header door then lands
    /// ten rows down — and by pointing the `SeeAll` arm at `0`, which reddens
    /// the foot half. Both were watched.
    #[tokio::test]
    async fn right_on_a_band_header_and_on_the_see_all_foot_open_the_same_page() {
        let mut a = app();
        a.store.adopt_projects(vec![many_proj("harness", 40)]);
        a.project();
        assert_eq!(
            a.vs.projects.recent.len(),
            tui_render::BAND_ROWS,
            "the recent band is not capped at ten"
        );

        // DOOR ONE: the band header. Row 0 — he asked to see the band, and a
        // band starts at its top.
        cursor_to_band(&mut a, tui_render::BandId::Recent);
        let header_at = a.project_cursor;
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(a.vs.mode, Mode::SeeAll(tui_render::BandId::Recent));
        assert_eq!(
            a.origin.last().copied(),
            Some(Mode::Projects),
            "the page did not remember the menu it came from"
        );
        assert_eq!(a.seeall_cursor, 0, "the header door did not land on row 0");
        assert_eq!(
            a.vs.projects.recent.len(),
            40,
            "the page capped the band he asked to see whole"
        );

        // Back out, and DOOR TWO: the foot.
        let i = left(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(a.vs.mode, Mode::Projects);
        assert_eq!(a.project_cursor, header_at, "← moved the root cursor");

        cursor_to(&mut a, RootItem::SeeAll(tui_render::BandId::Recent));
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(a.vs.mode, Mode::SeeAll(tui_render::BandId::Recent));
        assert_eq!(
            a.seeall_cursor,
            tui_render::BAND_ROWS,
            "the foot door did not land on the first row he could not see"
        );
        assert_eq!(a.origin.last().copied(), Some(Mode::Projects));
    }

    /// **`→` ON A COMPLETE BAND'S HEADER OPENS ITS PAGE ANYWAY.** One law for
    /// the key, replacing the *"pinned is already showing all 4"* notice.
    ///
    /// A band that hides nothing has no `see all` foot — a row offering to show
    /// more over nothing is the invented-zero defect wearing an affordance — so
    /// the header is the only door, and it has to work or the band with four
    /// rows is the one band he cannot get a page of.
    ///
    /// FORCED RED by restoring the `band_has_more` guard in front of
    /// `open_band_page` (the mode stays `Projects` and the first assertion
    /// fails), and by leaving the old notice in place (the second fails).
    #[tokio::test]
    async fn right_on_a_complete_bands_header_opens_its_page_too() {
        let mut a = app();
        a.store.adopt_projects(vec![many_proj("harness", 3)]);
        a.project();
        for b in tui_render::BandId::ALL {
            if !(0..a.vs.projects.selectable())
                .any(|i| a.vs.projects.item(i) == Some(RootItem::Band(b)))
            {
                continue;
            }
            assert!(
                !a.vs.projects.band_has_more(b),
                "{b:?} is hiding rows in this fixture"
            );
            cursor_to_band(&mut a, b);
            let i = right(&a);
            a.on_intent(i);
            a.project();
            assert_eq!(a.vs.mode, Mode::SeeAll(b), "{b:?}'s header opened nothing");
            assert_eq!(notice(&a), "", "{b:?} answered with a sentence, not a page");
            let i = left(&a);
            a.on_intent(i);
            a.project();
        }
    }

    /// **`←` OFF THE PAGE GOES BACK TO THE MAIN MENU, WITH THE ROOT'S CURSOR
    /// AND VIEWPORT BIT-IDENTICAL — FROM BOTH DOORS.** `I-3`, and his sentence:
    /// *"Going left from there goes back to the main menu."*
    ///
    /// This is the cursor-per-depth half of `DEC-24`. A shared cursor passes
    /// the mode assertion and fails these two: the header door would come back
    /// on row 0 and the foot door on row `BAND_ROWS`, which is why the page has
    /// `seeall_cursor`/`seeall_vp` of its own.
    ///
    /// FORCED RED by making `App::root_cursor`/`set_root_cursor` return
    /// `project_cursor` in every mode — both doors then come back on the wrong
    /// row. Watched.
    #[tokio::test]
    async fn left_off_the_page_lands_on_the_menu_with_the_root_cursor_untouched() {
        for door in [
            RootItem::Band(tui_render::BandId::Recent),
            RootItem::SeeAll(tui_render::BandId::Recent),
        ] {
            let mut a = app();
            a.store.adopt_projects(vec![many_proj("harness", 40)]);
            a.project();
            cursor_to(&mut a, door);
            let was_cursor = a.project_cursor;
            let was_vp = a.projects_vp.from_bottom;

            let i = right(&a);
            a.on_intent(i);
            a.project();
            assert_eq!(a.vs.mode, Mode::SeeAll(tui_render::BandId::Recent));
            // Walk down the page, so a shared cursor would be visibly wrong.
            for _ in 0..7 {
                a.on_intent(Intent::ScrollDown);
            }
            a.project();

            let i = left(&a);
            a.on_intent(i);
            a.project();
            assert_eq!(a.vs.mode, Mode::Projects, "{door:?}: ← did not reach the menu");
            assert_eq!(
                a.project_cursor, was_cursor,
                "{door:?}: ← moved the root cursor"
            );
            assert_eq!(
                a.projects_vp.from_bottom, was_vp,
                "{door:?}: ← moved the root viewport"
            );
            assert!(
                a.origin.is_empty(),
                "{door:?}: the stack still holds a frame at the root"
            );
        }
    }

    /// **THE JOURNEY, END TO END: root → the page → a thread → back to the page
    /// → back to the root.** `I-4`.
    ///
    /// The mirror of `a_thread_opened_from_a_root_band_escapes_back_to_the_root`
    /// one door deeper, and the reason the page needed no `Intent::Back` clause
    /// of its own: `ascend()` lands on `Mode::SeeAll(b)` because that is what
    /// `drill_to_thread` pushed.
    ///
    /// FORCED RED by replacing `self.ascend()` in `Intent::Back`'s
    /// `Mode::Thread` arm with `self.vs.mode = Mode::Projects` — the first
    /// landing assertion goes red. Watched.
    #[tokio::test]
    async fn a_thread_opened_from_the_page_escapes_back_to_the_page_then_the_root() {
        let mut a = app();
        a.store.adopt_projects(vec![many_proj("harness", 40)]);
        a.project();
        cursor_to_band(&mut a, tui_render::BandId::Recent);
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(a.vs.mode, Mode::SeeAll(tui_render::BandId::Recent));

        // Down onto a row the ROOT could never have shown him, which is the
        // point of the page.
        for _ in 0..15 {
            a.on_intent(Intent::ScrollDown);
        }
        a.project();
        let on = a.seeall_cursor;
        assert!(on >= tui_render::BAND_ROWS, "the walk stayed inside the cap");
        let depth = a.origin.len();

        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert!(
            matches!(a.vs.mode, Mode::Thread(_)),
            "→ on a page row did not open the thread: {:?}",
            a.vs.mode
        );
        assert_eq!(a.origin.len(), depth + 1, "the drill did not push one frame");

        let i = left(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(
            a.vs.mode,
            Mode::SeeAll(tui_render::BandId::Recent),
            "← out of the thread did not give the page back"
        );
        assert_eq!(a.seeall_cursor, on, "the page forgot the row he opened");
        assert_eq!(a.origin.len(), depth);

        let i = left(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Projects);
        assert!(a.origin.is_empty());
    }

    /// **THE TRAIL IS EXACTLY THE SEQUENCE OF SCREENS `←` WILL VISIT.** `I-5`.
    ///
    /// *"The threads should be aware of their breadcrumbs."* Asserted the only
    /// way that sentence can be checked: the trail is captured, and then `←` is
    /// pressed until the root, and the labels of where he LANDED are compared
    /// to it. A trail derived from anything other than the stack — the open
    /// project, say — passes a spot check and fails this.
    ///
    /// The ROOT CONTRIBUTES NOTHING, which is `0133`'s choice and his to
    /// overturn: a thread opened straight from a band has an EMPTY trail.
    ///
    /// FORCED RED by giving `Mode::Projects` the crumb `"menu"` (every case
    /// gains a leading crumb the walk never lands on) and by deriving the trail
    /// from `store.open_project()` instead of `origin` (the page-thread case
    /// reads `["harness"]`). Both were watched.
    #[tokio::test]
    async fn the_trail_is_the_sequence_of_screens_left_will_visit() {
        // CASE A — a thread straight off a root band: no crumb at all.
        let mut a = app();
        a.store.adopt_projects(vec![many_proj("harness", 40)]);
        a.project();
        cursor_to(&mut a, RootItem::Recent(0));
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert!(
            matches!(a.vs.mode, Mode::Thread(_)),
            "the band row did not open"
        );
        assert_eq!(
            a.vs.header.trail,
            Vec::<String>::new(),
            "a thread opened from a band is already at the floor"
        );

        // CASE B — a thread off the recent PAGE: one crumb, and it is the band.
        let mut a = app();
        a.store.adopt_projects(vec![many_proj("harness", 40)]);
        a.project();
        cursor_to_band(&mut a, tui_render::BandId::Recent);
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(a.vs.header.trail, Vec::<String>::new(), "the page is not its own crumb");
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(a.vs.header.trail, vec!["recent".to_string()]);

        // CASE C — Tab into the event lane over that thread: two crumbs, and
        // the second is the thread he raised it over.
        a.on_intent(Intent::SwapView);
        a.project();
        assert_eq!(a.vs.mode, Mode::Activity);
        let trail = a.vs.header.trail.clone();
        assert_eq!(trail.len(), 2, "the activity trail is not two crumbs: {trail:?}");
        assert_eq!(trail[0], "recent");
        assert!(!trail[1].is_empty(), "the thread crumb is blank");

        // **AND NOW THE WALK.** `←` (or its spelling per mode) until the root,
        // recording where each press LANDED, and the landings must be the trail
        // read backwards.
        let mut landed: Vec<String> = Vec::new();
        for _ in 0..8 {
            if a.vs.mode == Mode::Projects {
                break;
            }
            let i = press(&a, tui_render::Key::Esc);
            let i = if i == Intent::None { left(&a) } else { i };
            a.on_intent(i);
            a.project();
            if a.vs.mode == Mode::Projects {
                break;
            }
            landed.push(match a.vs.mode {
                Mode::SeeAll(b) => b.label().to_string(),
                m => a.crumb(m).unwrap_or_default(),
            });
        }
        let mut want = trail.clone();
        want.reverse();
        assert_eq!(
            landed, want,
            "the trail promised screens ← does not visit (landed {landed:?}, trail {trail:?})"
        );

        // CASE D — a thread reached THROUGH its project: the crumb is the
        // project's name. The same thread reached two ways shows two trails,
        // which is the whole resolution of the tension in his sentence.
        let mut a = app();
        a.store.adopt_projects(vec![many_proj("harness", 40)]);
        a.project();
        cursor_to(&mut a, RootItem::Project(0));
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(a.vs.mode, Mode::List);
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert!(matches!(a.vs.mode, Mode::Thread(_)));
        assert_eq!(a.vs.header.trail, vec!["harness".to_string()]);
    }

    /// **THE TRAIL IS DERIVED, AND NOTHING STORES A CRUMB.** `I-6`, and the
    /// invariant most likely to be violated by a well-meaning builder: the
    /// tempting implementation is a `trail: Vec<String>` written on `descend`.
    ///
    /// Asserted by building TWO apps that arrive at the same `(origin, mode)`
    /// through DIFFERENT HISTORIES — one goes straight in, the other wanders
    /// through the event lane and a second band first — and comparing their
    /// headers. A stored trail accumulates the wandering; a derived one cannot.
    ///
    /// FORCED RED by adding a `trail: Vec<String>` field pushed in `descend`
    /// and read by `project()` instead of `App::trail` — the wandering app's
    /// header then carries the lane it passed through. Watched.
    #[tokio::test]
    async fn the_trail_is_derived_from_the_stack_and_not_stored() {
        let straight = {
            let mut a = app();
            a.store.adopt_projects(vec![many_proj("harness", 40)]);
            a.project();
            cursor_to_band(&mut a, tui_render::BandId::Recent);
            let i = right(&a);
            a.on_intent(i);
            let i = right(&a);
            a.on_intent(i);
            a.project();
            a
        };
        let wandering = {
            let mut a = app();
            a.store.adopt_projects(vec![many_proj("harness", 40)]);
            a.project();
            // Into the lane and back, into the projects band's page and back,
            // into a project's list and back — then the same two doors.
            a.on_intent(Intent::SwapView);
            a.on_intent(Intent::SwapView);
            cursor_to_band(&mut a, tui_render::BandId::Projects);
            let i = right(&a);
            a.on_intent(i);
            let i = left(&a);
            a.on_intent(i);
            a.project();
            cursor_to(&mut a, RootItem::Project(0));
            let i = right(&a);
            a.on_intent(i);
            let i = left(&a);
            a.on_intent(i);
            a.project();
            cursor_to_band(&mut a, tui_render::BandId::Recent);
            let i = right(&a);
            a.on_intent(i);
            let i = right(&a);
            a.on_intent(i);
            a.project();
            a
        };
        assert_eq!(
            straight.origin, wandering.origin,
            "the fixture did not reach one state by two routes"
        );
        assert_eq!(straight.vs.mode, wandering.vs.mode);
        assert_eq!(
            straight.vs.header.trail, wandering.vs.header.trail,
            "the trail remembers a history the stack does not hold"
        );
        assert_eq!(straight.vs.header.trail, vec!["recent".to_string()]);
    }

    /// **`←` IN THE TEXTAREA MOVES THE CARET AND DOES NOT POP.** `I-7`'s depth
    /// half, which is the half a "promote `←` to `Back` unconditionally" change
    /// would break silently: the caret assertion alone still passes if the pop
    /// ALSO happens, because the buffer is untouched by a mode change.
    ///
    /// FORCED RED by making the `E::Left` arm of the composer call
    /// `Intent::Back` unconditionally. Watched.
    #[tokio::test]
    async fn left_with_text_to_its_left_moves_the_caret_and_never_pops() {
        let mut a = app();
        a.store.adopt_projects(vec![many_proj("harness", 40)]);
        a.project();
        cursor_to_band(&mut a, tui_render::BandId::Recent);
        let i = right(&a);
        a.on_intent(i);
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
        for c in "hi".chars() {
            a.on_intent(Intent::Type(c));
        }
        let depth = a.origin.len();
        let i = left(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer), "← left the thread");
        assert_eq!(a.origin.len(), depth, "← popped the stack from inside a draft");
        // At the buffer start it IS the way out, which is the other half.
        a.on_intent(Intent::Compose(tui_render::Edit::BufStart));
        let i = left(&a);
        a.on_intent(i);
        assert_eq!(
            a.vs.mode,
            Mode::SeeAll(tui_render::BandId::Recent),
            "← at the buffer start did not leave the thread"
        );
        assert_eq!(a.origin.len(), depth - 1);
    }

    /// **THE BACK STACK IS BOUNDED, AND IT IS EMPTY AT THE ROOT.** `I-8`.
    ///
    /// Unbounded state is a leak, and a stack is the one kind of state where a
    /// missing pop is invisible: every screen looks right and the depth grows.
    /// Two thousand keys drawn from the six that move between screens, against
    /// a fixture with every band populated, and two properties: the depth never
    /// passes FOUR (the deepest legal path is root → page → thread → activity,
    /// three pushes), and the stack is EMPTY whenever the root is showing.
    ///
    /// The second half is the one that catches a leak in a new arm: a `Back`
    /// that assigned `vs.mode = Mode::Projects` instead of popping would leave
    /// a frame nobody will ever pop, and the depth bound alone would not notice
    /// for a long time.
    ///
    /// FORCED RED by replacing `self.ascend()` in the page's `Intent::Back` arm
    /// with `self.vs.mode = Mode::Projects` — the empty-at-the-root assertion
    /// fires within a few dozen keys. Watched.
    #[tokio::test]
    async fn the_back_stack_stays_shallow_and_empties_at_the_root() {
        use tui_render::Key;
        let mut a = app();
        a.store.adopt_projects(vec![
            many_proj("harness", 40),
            pinned_proj("misc", &[("p1", 0), ("p2", 1)]),
        ]);
        a.project();
        // A cheap deterministic walk — a seeded LCG, so a failure is
        // reproducible rather than a Heisenbug in CI.
        let keys = [Key::Right, Key::Left, Key::Esc, Key::Tab, Key::Up, Key::Down];
        let mut seed: u64 = 0x5eed_1357_9bdf_0246;
        for step in 0..2_000u32 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let k = keys[(seed >> 33) as usize % keys.len()];
            let i = press(&a, k);
            if i != Intent::None {
                a.on_intent(i);
            }
            a.project();
            assert!(
                a.origin.len() <= 4,
                "the stack leaked at step {step} (depth {}, mode {:?}, stack {:?})",
                a.origin.len(),
                a.vs.mode,
                a.origin
            );
            if a.vs.mode == Mode::Projects {
                assert!(
                    a.origin.is_empty(),
                    "the root is showing over a stack of {:?} at step {step}",
                    a.origin
                );
            }
        }
    }

    /// **AN `Invalidate` RE-PROJECTION CHANGES NOTHING ABOUT WHERE HE IS**, and
    /// a RENAME in the store shows up in the crumb. `I-9`.
    ///
    /// The two halves are one invariant read in both directions: the trail is a
    /// function of `(origin, view, store)` and of nothing that was captured when
    /// a door opened. So re-running the function with the store unchanged must
    /// return the same words, and re-running it after a rename must return the
    /// NEW name — which a snapshot taken at `descend` time could not.
    ///
    /// FORCED RED by clearing `origin` in the invalidate path (the first block
    /// fails) and by snapshotting the crumb string when a door opens (the
    /// rename block fails). Watched.
    #[tokio::test]
    async fn an_invalidate_leaves_the_page_alone_and_a_rename_reaches_the_crumb() {
        let mut a = app();
        a.store.adopt_projects(vec![many_proj("harness", 40)]);
        a.project();
        cursor_to_band(&mut a, tui_render::BandId::Recent);
        let i = right(&a);
        a.on_intent(i);
        for _ in 0..4 {
            a.on_intent(Intent::ScrollDown);
        }
        a.project();
        let (mode, stack, cur, trail) = (
            a.vs.mode,
            a.origin.clone(),
            a.seeall_cursor,
            a.vs.header.trail.clone(),
        );
        a.project();
        a.project();
        assert_eq!(a.vs.mode, mode, "a re-projection moved the view");
        assert_eq!(a.origin, stack, "a re-projection touched the stack");
        assert_eq!(a.seeall_cursor, cur, "a re-projection moved the page's cursor");
        assert_eq!(a.vs.header.trail, trail, "a re-projection changed the trail");

        // Now the rename, one level over: into a project, into a thread, and
        // the project's name changes underneath.
        let mut a = app();
        a.store.adopt_projects(vec![many_proj("harness", 40)]);
        a.project();
        cursor_to(&mut a, RootItem::Project(0));
        let i = right(&a);
        a.on_intent(i);
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(a.vs.header.trail, vec!["harness".to_string()]);
        a.store.adopt_projects(vec![many_proj("harness-renamed", 40)]);
        a.store.enter_project(0);
        a.project();
        assert_eq!(
            a.vs.header.trail,
            vec!["harness-renamed".to_string()],
            "the crumb was snapshotted when the door opened"
        );
    }

    /// **A ROW THAT VANISHES UNDER THE CURSOR ON THE PAGE CLAMPS, AND `←` STILL
    /// WORKS.** `I-10`. The band can go EMPTY under him, and then the page draws
    /// one sentence rather than a blank screen.
    ///
    /// A page is reached and then the world changes: a thread closes and leaves
    /// `recent`, the whole project is de-registered. Neither may crash, neither
    /// may leave a cursor past the end (that is how `→` opens the wrong thread),
    /// and neither may leave him on a screen with no way off it.
    ///
    /// FORCED RED by dropping the clamp read-back in `project()` (`seeall_cursor
    /// < selectable()` fails), and by returning `Vec::new()` from `root_lines`
    /// for an empty focused band (the empty-sentence assertion fails). Watched.
    #[tokio::test]
    async fn a_page_whose_rows_vanish_clamps_and_says_so_and_left_still_works() {
        let mut a = app();
        a.store.adopt_projects(vec![many_proj("harness", 40)]);
        a.project();
        cursor_to_band(&mut a, tui_render::BandId::Recent);
        let i = right(&a);
        a.on_intent(i);
        for _ in 0..30 {
            a.on_intent(Intent::ScrollDown);
        }
        a.project();
        assert!(a.seeall_cursor > tui_render::BAND_ROWS);

        // The band loses almost everything under him.
        a.store.adopt_projects(vec![many_proj("harness", 2)]);
        a.project();
        assert!(
            a.seeall_cursor < a.vs.projects.selectable(),
            "the page's cursor is past the end (cursor {}, stops {})",
            a.seeall_cursor,
            a.vs.projects.selectable()
        );

        // And now the band goes EMPTY.
        a.store.adopt_projects(vec![]);
        a.project();
        assert_eq!(a.vs.projects.selectable(), 0);
        assert_eq!(
            a.vs.projects.lines(),
            vec![tui_render::RootLine::Hint(tui_render::RECENT_EMPTY)],
            "an empty page drew a blank screen"
        );
        // `→` on nothing is a free miss, not a panic.
        let i = right(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::SeeAll(tui_render::BandId::Recent));
        // …and `←` still goes home.
        let i = left(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Projects, "← off an empty page went nowhere");
    }

    /// **`←` NEVER LANDS ON A PROJECT THAT IS NO LONGER OPEN.** `I-11`.
    ///
    /// `Mode::List` is the ONE stack entry whose target lives outside the stack:
    /// the mode says "a project's threads" and which project is
    /// `store.open_project()`. De-register it from inside one of its threads and
    /// `←` used to deliver him onto `threads · no project open` — a screen with
    /// no rows and a header naming nothing.
    ///
    /// FORCED RED by removing the re-pop loop from `App::ascend` — he lands on
    /// `Mode::List` and the assertion names it. Watched.
    #[tokio::test]
    async fn left_never_lands_on_a_project_that_is_no_longer_open() {
        let mut a = app();
        a.store.adopt_projects(vec![many_proj("harness", 5)]);
        a.project();
        cursor_to(&mut a, RootItem::Project(0));
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(a.vs.mode, Mode::List);
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert!(matches!(a.vs.mode, Mode::Thread(_)));
        assert_eq!(a.origin, vec![Mode::Projects, Mode::List]);

        // The project goes away underneath him.
        a.store.adopt_projects(vec![]);
        assert!(a.store.open_project().is_none(), "the fixture kept it open");

        let i = left(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(
            a.vs.mode,
            Mode::Projects,
            "← landed on a project's list with no project open"
        );
    }

    /// **THE PAGE ANSWERS TO THE ROOT'S ROW ACTS, AND `ctrl-↑` IS THE ONE THAT
    /// PROVES IT.**
    ///
    /// The pinned band's page is where he will most want move-pin — 26 pins
    /// against a ten-row band — and it is the measured reason the page draws the
    /// ROOT'S projection rather than `Mode::List`'s: a List-shaped page would
    /// have needed `Intent::MovePinUp` written a second time over `ThreadList`.
    /// This asserts it reaches the wire from the page, through the same
    /// `RootItem::Pinned(n)` resolution.
    ///
    /// FORCED RED by leaving `move_pin`'s guard as `self.view() != Mode::Projects`
    /// — nothing reaches the socket from the page. Watched.
    #[tokio::test]
    async fn ctrl_up_reorders_a_pin_from_the_pinned_bands_own_page() {
        let mut a = app();
        let (h, mut out) = crate::aui::test_handle_up();
        a.set_aui(h);
        let pins: Vec<(&str, i64)> = PIN_LABELS
            .iter()
            .enumerate()
            .map(|(i, l)| (*l, i as i64))
            .collect();
        a.store.adopt_projects(vec![pinned_proj("harness", &pins)]);
        a.project();
        cursor_to_band(&mut a, tui_render::BandId::Pinned);
        let i = right(&a);
        a.on_intent(i);
        a.project();
        assert_eq!(a.vs.mode, Mode::SeeAll(tui_render::BandId::Pinned));
        assert_eq!(
            a.vs.projects.pinned.len(),
            12,
            "the pinned page is still capped at ten"
        );
        // Onto the third pin, then move it up.
        a.seeall_cursor = 2;
        a.project();
        assert_eq!(a.vs.projects.item(a.seeall_cursor), Some(RootItem::Pinned(2)));
        let i = tui_render::intent(tui_render::Key::CtrlUp, a.vs.mode);
        assert_eq!(i, Intent::MovePinUp, "ctrl-↑ is not bound on the page");
        a.on_intent(i);
        // The send is a spawned task, exactly as it is on the root's own test.
        tokio::task::yield_now().await;
        let job = out
            .try_recv()
            .expect("ctrl-up on the page put nothing on the wire");
        assert_eq!(job.kind(), "move_pin");
        // **AND THE PAGE HAS NOT REORDERED ITSELF.** The order belongs to the
        // AUI; until it acks and the index is re-read, the page still draws what
        // the file says — the same promise the root's band makes.
        assert_eq!(
            a.vs.projects.pinned.first().map(|r| r.id.clone()),
            Some(uid("p1")),
            "the page reordered the band behind the owner's back"
        );
        assert_eq!(a.pin_follow.as_deref(), Some(uid("p3").as_str()));
    }

    /// Twelve pin labels, which `uid` turns into real uuids.
    const PIN_LABELS: [&str; 12] = [
        "p1", "p2", "p3", "p4", "p5", "p6", "p7", "p8", "p9", "p10", "p11", "p12",
    ];

    // ── the pinned band reorders, and the cursor travels ─────────────────

    /// **`ctrl-↑` PUTS A `MovePin` ON THE WIRE AND ANCHORS THE CURSOR TO THE
    /// THREAD.**
    ///
    /// The binding is taken from the key TABLE, so this also asserts that
    /// `ctrl-↑` is bound in `Mode::Projects` at all.
    ///
    /// **THE FACE REORDERS NOTHING LOCALLY**, which is the trap this design was
    /// written around: the order belongs to the AUI, so until it acks and the
    /// index is re-read the band on screen still shows what the FILE says.
    /// Asserted positively below — a screen that moved the row before the owner
    /// agreed would be a screen that lies when the act fails.
    ///
    /// FORCED RED by removing the `Key::CtrlUp` binding (the intent comes back
    /// as something else and nothing reaches the wire), and by anchoring
    /// `pin_follow` to the cursor INDEX instead of the id (the assertion below
    /// still passes but `the_cursor_follows_the_thread…` goes red, which is
    /// why that half is a test of its own).
    #[tokio::test]
    async fn ctrl_up_on_a_pinned_row_sends_a_move_and_does_not_reorder_locally() {
        let mut a = app();
        let (h, mut out) = crate::aui::test_handle_up();
        a.set_aui(h);
        a.store.adopt_projects(vec![pinned_proj(
            "harness",
            &[("p1", 0), ("p2", 1), ("p3", 2)],
        )]);
        a.project();
        let before: Vec<String> = a.vs.projects.pinned.iter().map(|r| r.id.clone()).collect();
        assert_eq!(
            before,
            vec![uid("p1"), uid("p2"), uid("p3")],
            "the fixture is not in pin order"
        );

        cursor_to(&mut a, RootItem::Pinned(2));
        let i = press(&a, tui_render::Key::CtrlUp);
        assert_eq!(i, Intent::MovePinUp, "ctrl-↑ is not bound at the root");
        a.on_intent(i);
        tokio::task::yield_now().await;

        // THE BYTES LEFT: a `MovePin`, and nothing else.
        let job = out.try_recv().expect("ctrl-↑ put nothing on the wire");
        assert_eq!(job.kind(), "move_pin");
        assert!(out.try_recv().is_err(), "one press sent two mutations");

        // **AND THE BAND HAS NOT MOVED.** The AUI has not answered, so the face
        // still draws the order the file says — which is the whole reason a
        // failed act cannot leave the screen lying.
        let after: Vec<String> = a.vs.projects.pinned.iter().map(|r| r.id.clone()).collect();
        assert_eq!(
            after, before,
            "the face reordered the band behind the owner's back"
        );
        // The cursor is anchored to the THREAD, by id.
        assert_eq!(a.pin_follow.as_deref(), Some(uid("p3").as_str()));
    }

    /// **THE CURSOR FOLLOWS THE THREAD, NOT THE ROW NUMBER** — and this is the
    /// half a naive implementation gets wrong.
    ///
    /// *"the cursor should go with it, so we can click that multiple times to
    /// move it to a new location."* After the AUI accepts, rewrites the index
    /// and this face re-reads it, the row is at a NEW index and the old index
    /// now names the thread it displaced. Keeping the cursor on the index is
    /// therefore keeping it on the wrong thread, and the second tap moves that
    /// one.
    ///
    /// The re-read is simulated the way it actually happens — the index comes
    /// back with a different `pin_order` — because that is what
    /// `Clears::PinMove`'s `load_projects()` produces.
    ///
    /// FORCED RED by deleting the `self.follow_pin()` call in `App::project`:
    /// the cursor stays at index 2 and lands on `p2`, the thread that was
    /// displaced. That is the defect this test exists for, and it was watched
    /// go red before the fix was trusted.
    #[tokio::test]
    async fn the_cursor_follows_the_thread_across_a_reorder_and_not_the_index() {
        let mut a = app();
        let (h, _out) = crate::aui::test_handle_up();
        a.set_aui(h);
        a.store.adopt_projects(vec![pinned_proj(
            "harness",
            &[("p1", 0), ("p2", 1), ("p3", 2)],
        )]);
        a.project();
        cursor_to(&mut a, RootItem::Pinned(2));
        let was = a.project_cursor;
        a.on_intent(Intent::MovePinUp);

        // THE OWNER APPLIED IT: `p3` now sits between `p1` and `p2`. This is
        // the index the re-read hands back.
        a.store.adopt_projects(vec![pinned_proj(
            "harness",
            &[("p1", 0), ("p2", 2), ("p3", 1)],
        )]);
        a.project();
        assert_eq!(
            a.vs.projects
                .pinned
                .iter()
                .map(|r| r.id.clone())
                .collect::<Vec<_>>(),
            vec![uid("p1"), uid("p3"), uid("p2")],
            "the simulated re-read did not reorder the band"
        );

        // **THE CURSOR IS ON `p3`, AT ITS NEW INDEX**, one row UP from where it
        // was — which is the row moving under a cursor that stayed with it.
        assert_eq!(
            a.project_cursor,
            was - 1,
            "the cursor did not travel with the row"
        );
        assert_eq!(
            a.vs.projects.item(a.project_cursor),
            Some(RootItem::Pinned(1))
        );
        assert_eq!(
            a.vs.projects
                .thread_at(a.project_cursor)
                .map(|r| r.id.clone()),
            Some(uid("p3")),
            "the cursor followed the INDEX and landed on the displaced thread"
        );

        // **AND THE ANCHOR LETS GO THE MOMENT HE MOVES.** Otherwise the cursor
        // would refuse to leave the row he just walked.
        a.on_intent(Intent::ScrollDown);
        assert!(
            a.pin_follow.is_none(),
            "the anchor outlived his next keypress"
        );
    }

    /// **A ROW THAT IS NOT PINNED SAYS SO AND SENDS NOTHING.**
    ///
    /// The aim is the BAND, not the thread's pinned flag: a `recent` row can
    /// name a pinned thread, and reordering from there would move a row in a
    /// band he is not looking at.
    ///
    /// **ROUND FIVE NARROWED THIS TEST BY ONE ROW AND THAT IS A REAL
    /// BEHAVIOUR CHANGE**: `ctrl-↑`/`ctrl-↓` on a PROJECT row now reorders the
    /// registry (`0099`: *"REORDER PROJECTS, by the same grammar as reordering
    /// pinned threads — reuse it"*). It used to say *"only a pinned thread can
    /// be reordered"* and do nothing. Every other row here is unchanged, and
    /// the project row's own refusals are asserted in
    /// [`tests::a_project_reorder_names_a_neighbour_and_never_an_index`].
    ///
    /// FORCED RED by widening the `move_pin` match to accept `Recent(_)` — the
    /// recent arm then reaches the wire and `out.try_recv()` succeeds.
    #[tokio::test]
    async fn the_reorder_key_refuses_every_row_that_is_not_in_the_pinned_band() {
        let mut a = app();
        let (h, mut out) = crate::aui::test_handle_up();
        a.set_aui(h);
        // `p1` is PINNED and therefore also appears in `recent` — the exact row
        // a pinned-flag test would wave through.
        a.store
            .adopt_projects(vec![pinned_proj("harness", &[("p1", 0)])]);
        a.project();

        for (target, words) in [
            (
                RootItem::Recent(0),
                "reorder works in the pinned band — this is recent",
            ),
            (
                RootItem::Band(tui_render::BandId::Pinned),
                "only a pinned thread or a project can be reordered",
            ),
            (
                RootItem::Archived,
                "only a pinned thread or a project can be reordered",
            ),
        ] {
            a.vs.notice = None;
            cursor_to(&mut a, target);
            a.on_intent(Intent::MovePinUp);
            tokio::task::yield_now().await;
            assert_eq!(notice(&a), words, "{target:?} said the wrong thing");
            assert!(
                out.try_recv().is_err(),
                "{target:?} put a mutation on the wire"
            );
            assert!(a.pin_follow.is_none(), "{target:?} anchored the cursor");
        }

        // THE POSITIVE HALF, in the same test: the pinned row DOES reach the
        // wire, so the four refusals above mean "refused" and not "this App
        // cannot send anything at all".
        cursor_to(&mut a, RootItem::Pinned(0));
        a.on_intent(Intent::MovePinDown);
        tokio::task::yield_now().await;
        // A single pin cannot move, and the REFUSAL is about the act rather
        // than about the socket — `acts::move_before` says so in words.
        assert!(
            notice(&a).contains("bottom of the pinned band"),
            "the one-pin case said: {}",
            notice(&a)
        );
    }

    /// **WHAT HE SEES WHEN THE SOCKET IS DOWN AND HE TAPS THE KEY.**
    ///
    /// The row does not move and a sentence says why. That is the property the
    /// no-local-reorder decision buys: an optimistic implementation would have
    /// walked the row up the band and then had to walk it back, and between
    /// those two frames the screen was lying.
    ///
    /// FORCED RED by reordering `self.vs.projects.pinned` in `move_pin` before
    /// firing — the band moves with no owner anywhere, which is the defect.
    #[tokio::test]
    async fn a_move_with_the_owner_unreachable_says_why_and_leaves_the_band_alone() {
        let mut a = app();
        // NO `set_aui`: the 9010 door is shut, which is the live failure.
        a.store
            .adopt_projects(vec![pinned_proj("harness", &[("p1", 0), ("p2", 1)])]);
        a.project();
        let before: Vec<String> = a.vs.projects.pinned.iter().map(|r| r.id.clone()).collect();

        cursor_to(&mut a, RootItem::Pinned(1));
        let at = a.project_cursor;
        a.on_intent(Intent::MovePinUp);
        tokio::task::yield_now().await;

        assert_eq!(
            notice(&a),
            crate::aui::DownCause::DoorShut.words(),
            "a move with no socket said nothing, or said the wrong thing"
        );
        assert_eq!(
            a.vs.projects
                .pinned
                .iter()
                .map(|r| r.id.clone())
                .collect::<Vec<_>>(),
            before,
            "the band moved with no owner to move it"
        );
        assert_eq!(a.project_cursor, at, "the cursor moved on a failed act");
        // The row under him is still the one he aimed at.
        assert_eq!(
            a.vs.projects
                .thread_at(a.project_cursor)
                .map(|r| r.id.clone()),
            Some(uid("p2"))
        );
    }

    /// **`ctrl-↑` AND `ctrl-↓` COLLIDE WITH NOTHING THE ROOT ALREADY BINDS.**
    ///
    /// The justification for the choice is in the key table's own comment; this
    /// is the machine-checked half of it. It reads `BINDINGS` rather than a
    /// list written here, so a key added to the root tomorrow is caught.
    ///
    /// FORCED RED by binding `Key::CtrlUp` a second time in `Mode::Projects`.
    #[test]
    fn the_reorder_keys_are_unique_in_the_mode_that_owns_them() {
        use tui_render::{Key, KeySet, Mode as M};
        for k in [Key::CtrlUp, Key::CtrlDown] {
            let owners: Vec<&str> = tui_render::BINDINGS
                .iter()
                .filter(|b| b.mode == M::Projects)
                .filter(|b| matches!(b.keys, KeySet::These(ks) if ks.contains(&k)))
                .map(|b| b.short)
                .collect();
            assert_eq!(owners.len(), 1, "{k:?} has {owners:?} at the root");
        }
        assert_eq!(
            tui_render::intent(Key::CtrlUp, M::Projects),
            Intent::MovePinUp
        );
        assert_eq!(
            tui_render::intent(Key::CtrlDown, M::Projects),
            Intent::MovePinDown
        );
        // **AND THEY ARE NOT STOLEN FROM THE COMPOSER**, where `0052` bound the
        // same two keys to the buffer limits. A mode-local binding must not
        // change what another mode's grammar means.
        assert_eq!(
            tui_render::intent(Key::CtrlUp, M::Thread(tui_render::Region::Composer)),
            Intent::Compose(tui_render::Edit::BufStart)
        );
    }

    /// **`→` AND `←` AS THE TABLE PRODUCES THEM, IN THE MODE HE IS IN.**
    /// Written this way on purpose: a test that hand-wrote `Intent::Activate`
    /// or `Intent::Back` would be asserting against its own idea of the
    /// binding rather than against the one key table — and `←` at the ROOT is
    /// deliberately not `Back`.
    fn press(a: &App, k: tui_render::Key) -> Intent {
        tui_render::intent(k, a.vs.mode)
    }

    fn right(a: &App) -> Intent {
        press(a, tui_render::Key::Right)
    }

    fn left(a: &App) -> Intent {
        press(a, tui_render::Key::Left)
    }

    fn event(app: &mut App, thread: &str, seq: u64, e: Value) {
        app.on_transport(TransportEvent::Frame {
            msg: crate::proto::ServerMsg::Event {
                thread_id: thread.into(),
                seq,
                message_id: None,
                ts: "2026-09-14T11:59:00Z".into(),
                event: crate::proto::ThreadEvent::parse(e),
            },
            replayed: false,
        });
    }

    /// **THREE ROWS THAT ALL HAVE SOMETHING BEHIND THEM.** The fixture for
    /// every per-row-expansion test, and it is deliberately three DIFFERENT
    /// kinds: a test whose rows were six of one kind could pass while opening
    /// them all, which is the defect these tests exist to catch.
    fn expandable_rows(app: &mut App) {
        let long = "x".repeat(400);
        event(
            app,
            "t1",
            1,
            json!({"kind":"task_notification","task_id":"agent-a","status":"completed",
                   "summary": long}),
        );
        event(
            app,
            "t1",
            2,
            json!({"kind":"context_injected","text": "y".repeat(400)}),
        );
        event(
            app,
            "t1",
            3,
            json!({"kind":"assistant_text","text":"z".repeat(400),
                   "parent_tool_use_id":"toolu_1"}),
        );
    }

    /// **A FAST FLICK MUST SCROLL FAST** — his words: *"scrolling fast should
    /// actually scroll fast. It just scrolls slowly for a long time."*
    ///
    /// Two assertions, and the SECOND one is the complaint. The first — the
    /// viewport lands where the burst says — was already true; a face that
    /// processes forty notches one at a time still ends up forty rows away. It
    /// is the *for a long time* that was the defect: forty notches meant forty
    /// full re-projections of the tail, and on his transcript each of those
    /// walks thousands of ledger rows. The work outlived the gesture.
    ///
    /// FORCED: put the per-notch `self.scroll_band(up); self.project();` back
    /// in `on_input`'s `Wheel` arm and the projection count goes to 40 while
    /// the viewport assertion stays green — which is exactly how this shipped
    /// and exactly why nothing caught it.
    /// **A BATCH OF TRANSPORT FACTS IS ONE PROJECTION** (THREAD-LOAD-LATENCY
    /// F1), and it draws exactly what projecting after every fact drew. Opening
    /// a 4,000-row thread replays every row as a fact; one projection per fact
    /// was 1.6 s of UI-loop time per 257-fact batch, measured.
    ///
    /// Also proves the debt is paid BEFORE a non-transport event inside the
    /// batch, so an input handler never acts on a stale view.
    #[tokio::test]
    async fn a_batch_of_transport_facts_is_one_projection_and_draws_the_same_view() {
        let rows = |a: &mut App, from: u64, to: u64| {
            for seq in from..=to {
                event(a, "t1", seq, json!({"kind":"assistant_text","text": format!("line {seq}")}));
            }
        };
        let mut eager = app();
        eager.open_thread("t1".into());
        eager.lay(100, 24);
        rows(&mut eager, 1, 300);

        let mut batched = app();
        batched.open_thread("t1".into());
        batched.lay(100, 24);
        let before = batched.projections;
        batched.begin_batch();
        rows(&mut batched, 1, 300);
        assert_eq!(batched.projections, before, "a fact inside a batch projected");
        batched.end_batch();
        assert_eq!(batched.projections, before + 1, "end_batch must pay exactly one projection");
        assert_eq!(
            format!("{:?}", batched.vs.blocks),
            format!("{:?}", eager.vs.blocks),
            "the batched view differs from the per-fact view"
        );

        // A non-transport event inside a batch sees a current view first.
        batched.begin_batch();
        rows(&mut batched, 301, 310);
        let owed = batched.projections;
        batched.apply(Ev::LinkTick);
        assert_eq!(batched.projections, owed + 1, "the owed projection was not paid before a non-transport event");
        batched.end_batch();
        assert_eq!(batched.projections, owed + 1, "end_batch projected with nothing owed");
    }

    #[tokio::test]
    async fn a_burst_of_wheel_notches_is_one_projection_and_lands_where_the_burst_says() {
        use crate::event::Input;
        const NOTCHES: usize = 40;

        let mut a = app();
        a.open_thread("t1".into());
        // A transcript with real depth, so a projection is real work rather
        // than a no-op on an empty tail.
        let rows: Vec<crate::store::LedgerRow> = Vec::new();
        a.store.load_transcript("t1", rows, 1);
        for seq in 1..=400u64 {
            event(
                &mut a,
                "t1",
                seq,
                json!({"kind":"assistant_text","text": format!("line {seq}")}),
            );
        }
        a.lay(100, 24);
        a.project();

        // **FORTY NOTCHES IN ONE READ IS A HARD FLICK**, and a hard flick is
        // at the ceiling of the curve by construction: forty notches charge
        // the meter to 40, which reads as 267 notches a second against a
        // `WHEEL_V_FULL` of 110. So the distance is `n * WHEEL_GAIN_MAX`
        // exactly — written out of the constants rather than as `800`, so
        // retuning the ceiling retunes this expectation with it.
        let flick = (NOTCHES as f32 * WHEEL_GAIN_MAX).round() as usize;
        let t0 = std::time::Instant::now();
        let before = a.projections;
        a.on_input_at(
            (0..NOTCHES).map(|_| Input::Wheel { up: true }).collect(),
            t0,
        );

        assert_eq!(
            a.vp().from_bottom,
            flick,
            "the burst did not take the viewport as far as it said"
        );
        assert!(
            flick >= 400,
            "a hard flick that moves {flick} rows does not cross a transcript"
        );
        assert!(!a.vp().follow, "a scroll up must leave follow mode");
        assert_eq!(
            a.projections - before,
            1,
            "{NOTCHES} notches cost {} projections — the gesture is over and the \
             face is still working",
            a.projections - before
        );

        // …and the way BACK is the same shape, in the other direction. Twenty
        // milliseconds later, which is one frame: the meter is still hot, so
        // the return trip is at the ceiling too and lands exactly at the
        // bottom rather than short of it.
        let before = a.projections;
        a.on_input_at(
            (0..NOTCHES).map(|_| Input::Wheel { up: false }).collect(),
            t0 + Duration::from_millis(20),
        );
        assert_eq!(a.vp().from_bottom, 0);
        assert!(
            a.vp().follow,
            "coming back to the bottom must re-arm follow"
        );
        assert_eq!(a.projections - before, 1);
    }

    /// A burst that changes DIRECTION mid-flight lands exactly where its
    /// notches say, and costs ONE PROJECTION PER RUN — two here, not one and
    /// not forty-two.
    ///
    /// **Two is the deliberate answer and a net sum would have been wrong.**
    /// "Thirty up then twelve down" is not "eighteen up" in general: in a list
    /// the thirty saturate against the top first, so the twelve down start from
    /// the clamp rather than from thirty. Ending the run at the reversal keeps
    /// every run monotone, which is the property that makes a run of `n`
    /// identical to `n` single steps. The cost is bounded by the number of
    /// times his hand changed direction, which is a small number by
    /// construction.
    #[tokio::test]
    async fn a_burst_that_reverses_ends_where_the_arithmetic_says_not_where_it_started() {
        use crate::event::Input;
        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        for seq in 1..=200u64 {
            event(
                &mut a,
                "t1",
                seq,
                json!({"kind":"assistant_text","text": format!("line {seq}")}),
            );
        }
        a.lay(100, 24);
        a.project();

        let mut batch: Vec<Input> = (0..30).map(|_| Input::Wheel { up: true }).collect();
        batch.extend((0..12).map(|_| Input::Wheel { up: false }));
        let before = a.projections;
        a.on_input_at(batch, std::time::Instant::now());
        // Both runs are inside one `read()` and both are at the ceiling — a
        // reversal does not discharge the meter, and it must not, or a wobble
        // mid-flick would read as two slower gestures. At a constant gain the
        // arithmetic stays linear, which is what keeps "18 notches up" true of
        // a velocity curve as it was of a constant.
        assert_eq!(
            a.vp().from_bottom,
            ((30 - 12) as f32 * WHEEL_GAIN_MAX).round() as usize,
            "30 up then 12 down is 18 notches up"
        );
        assert_eq!(
            a.projections - before,
            2,
            "one projection per RUN — a reversal is two runs, never 42 notches"
        );
    }

    /// **A KEYSTROKE IN THE MIDDLE OF A BURST STILL HAPPENS IN ORDER.**
    /// Coalescing is allowed to make the wheel cheaper; it is NOT allowed to
    /// move a keystroke past it, because the transcript he is looking at when
    /// he presses a key is the one the wheel left him on.
    #[tokio::test]
    async fn coalescing_the_wheel_does_not_reorder_anything_around_it() {
        use crate::event::Input;
        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        for seq in 1..=100u64 {
            event(
                &mut a,
                "t1",
                seq,
                json!({"kind":"assistant_text","text": format!("line {seq}")}),
            );
        }
        a.lay(100, 24);
        a.project();

        // **THE KEYSTROKE IS `→` IN THE CHAT REGION, AND IT USED TO BE
        // `alt-n`.** `alt-n` was retired with `g`/`G`/`opt-g` this round — the
        // audit found it was a second spelling of the same redundant
        // top/bottom pair. `→` in the chat is `Intent::Bottom`'s surviving
        // spelling and was always the same row `alt-n` sat in, so the act
        // under test did not change; only the key did. The region moves with
        // it, because `→` in the COMPOSER is a caret move.
        a.vs.mode = Mode::Thread(Region::Chat);

        // Five up, then `→` (newest, re-arm follow), then three up. The
        // keystroke must land BETWEEN the two runs, so the end state is three
        // rows up from the bottom and not eight.
        let mut batch: Vec<Input> = (0..5).map(|_| Input::Wheel { up: true }).collect();
        batch.push(Input::Key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Right,
            ratatui::crossterm::event::KeyModifiers::NONE,
        )));
        batch.extend((0..3).map(|_| Input::Wheel { up: true }));
        let t0 = std::time::Instant::now();
        a.on_input_at(batch, t0);
        // **THE EXPECTATION IS A SECOND METER DRIVEN THE SAME WAY**, not a
        // literal. The distance a run covers is now a function of everything
        // that came before it in the same read, so "three notches" has no
        // fixed answer — but "the second run, charged after the first" does,
        // and that is exactly the quantity this test is about. A five-notch
        // run left standing would land him at `five + three`, not at `three`.
        let mut mirror = WheelMeter::default();
        let five = mirror.rows(5, t0);
        let three_after_five = mirror.rows(3, t0);
        assert_eq!(
            a.vp().from_bottom,
            three_after_five,
            "the keystroke was reordered around the wheel"
        );
        assert!(
            a.vp().from_bottom < five + three_after_five,
            "the keystroke did not land between the two runs"
        );
    }

    /// **WHAT A PROJECTION OF A DEEP TAIL ACTUALLY COSTS.** A measuring
    /// instrument, `#[ignore]`d because a timing assertion is a flake on a
    /// loaded machine — run it with `--ignored --nocapture` when the number
    /// matters again.
    ///
    /// **Recorded 2026-09-15, debug build, this machine:**
    ///
    /// | blocks | per projection | forty of them |
    /// |---|---|---|
    /// | 400 | 84 µs | 3.4 ms |
    /// | 2,000 | 441 µs | 17.6 ms |
    /// | 4,000 | 952 µs | 38.1 ms |
    ///
    /// **It is here because it CORRECTED the diagnosis.** The obvious reading
    /// of *"it just scrolls slowly for a long time"* is a deep queue drained
    /// one expensive step at a time — and at 38 ms for forty notches on his
    /// deepest transcript, that is real work but it is not something a person
    /// can see. Coalescing was worth doing and is not what he was looking at.
    /// The step SIZE was: one row a notch against his own tmux's five.
    /// Measuring first is what kept the round from shipping the coalescing
    /// alone and calling the complaint closed.
    #[tokio::test]
    #[ignore]
    async fn measure_projection_cost() {
        for rows in [400u64, 2000, 4000] {
            let mut a = app();
            a.open_thread("t1".into());
            a.store.load_transcript("t1", Vec::new(), 1);
            for seq in 1..=rows {
                event(
                    &mut a,
                    "t1",
                    seq,
                    json!({"kind":"assistant_text","text": format!("line {seq} of a transcript that is long enough to matter")}),
                );
            }
            a.lay(100, 24);
            let t = std::time::Instant::now();
            for _ in 0..40 {
                a.project();
            }
            let each = t.elapsed() / 40;
            println!(
                "MEASURED rows={rows} blocks={} per-projection={:?} forty={:?}",
                a.vs.blocks.len(),
                each,
                each * 40
            );
        }
    }

    #[tokio::test]
    async fn scrolling_up_leaves_follow_mode_and_coming_back_re_arms_it() {
        let mut a = app();
        a.on_intent(Intent::SwapView); // the fleet lane, which has a viewport
        assert!(a.vp().follow);
        a.on_intent(Intent::ScrollUp);
        assert!(!a.vp().follow, "a manual scroll must clear follow-mode");
        a.on_intent(Intent::ScrollDown);
        assert!(a.vp().follow, "reaching the bottom must re-arm it");
    }

    /// **FOLLOW MODE, THE WHOLE PROMISE.** An arrival while he is scrolled up
    /// must not move his view, and the chip must appear.
    #[tokio::test]
    async fn an_arrival_while_scrolled_up_never_yanks_the_viewport() {
        let mut a = app();
        a.on_intent(Intent::SwapView);
        a.on_intent(Intent::ScrollUp);
        a.on_intent(Intent::ScrollUp);
        let was = a.vp().from_bottom;
        assert!(was > 0);

        event(&mut a, "t1", 1, json!({"kind":"turn_ended","exit_code":0}));

        assert_eq!(a.vp().from_bottom, was, "the viewport was YANKED");
        assert_eq!(a.vs.viewport.new_since, 1, "the chip must count it instead");
        assert!(!a.vs.viewport.follow);

        // …and `G` takes him back and clears the chip.
        a.on_intent(Intent::Bottom);
        assert_eq!(a.vs.viewport.from_bottom, 0);
        assert_eq!(a.vs.viewport.new_since, 0);
        assert!(a.vs.viewport.follow);
    }

    /// **ONE DELIBERATE NOTCH IS EXACTLY ONE ROW, ON EVERY SURFACE THE WHEEL
    /// REACHES.** His words, twice, for two different surfaces:
    ///
    /// > We need step size 1 and also need to be able to scroll fast
    ///
    /// > in the main menu, the scrolling should be proportional to the scroll
    /// > speed, with the finest granularity being 1 row.
    ///
    /// **EXACTLY, not approximately.** The floor is reachable because the
    /// curve's `clamp` makes `t` exactly zero below [`WHEEL_V_FLOOR`] — a
    /// curve that merely APPROACHED one would put him one row off the row he
    /// was aiming at, forever, and that is the complaint that opened this
    /// arc.
    ///
    /// All three surfaces in one test on purpose: they are one mechanism, and
    /// a test per surface would let them drift apart without anything going
    /// red.
    ///
    /// FORCED: make [`WheelMeter::gain`] return [`WHEEL_GAIN_MAX`]
    /// unconditionally — which is precisely "solve it with a bigger step
    /// size" — and all three assertions go red at 20 instead of 1.
    #[tokio::test]
    async fn one_deliberate_notch_is_exactly_one_row_on_every_surface() {
        use crate::event::Input;
        let t0 = std::time::Instant::now();

        // ── the TRANSCRIPT: a viewport over thousands of rows ──
        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        for seq in 1..=200u64 {
            event(
                &mut a,
                "t1",
                seq,
                json!({"kind":"assistant_text","text": format!("line {seq}")}),
            );
        }
        a.lay(100, 24);
        a.project();
        a.on_input_at(vec![Input::Wheel { up: true }], t0);
        assert_eq!(
            a.vp().from_bottom,
            1,
            "one notch on a transcript must move exactly one row"
        );

        // ── the MAIN MENU: a cursor over a small set ──
        let mut m = app();
        m.store.adopt_projects(vec![many_proj("harness", 40)]);
        m.lay(100, 24);
        m.project();
        let was = m.project_cursor;
        m.on_input_at(vec![Input::Wheel { up: false }], t0);
        assert_eq!(
            m.project_cursor,
            was + 1,
            "one notch on the main menu must move the cursor exactly one row"
        );

        // ── the `?` OVERLAY: its own offset, its own scroll path, and it has
        //    been wrong before (`0036`) ──
        let mut h = app();
        h.vs.mode = Mode::Help;
        h.lay(100, 24);
        h.on_input_at(vec![Input::Wheel { up: false }], t0);
        assert_eq!(
            h.vs.help_from, 1,
            "one notch on the ? overlay must move exactly one row"
        );
    }

    /// **THROUGHPUT SCALES WITH HOW FAST THE NOTCHES COME.** The other half of
    /// his sentence, and the half no constant can satisfy at the same time as
    /// the half above.
    ///
    /// The table this builds is the evidence: the SAME number of notches,
    /// delivered at different rates, must move monotonically further. Written
    /// against [`WheelMeter`] directly because the quantity under test is the
    /// curve — the pty suites prove the curve is actually wired to the face.
    ///
    /// FORCED: return a constant from [`WheelMeter::gain`] (either `1.0` or
    /// [`WHEEL_GAIN_MAX`]) and the strict inequality between the slow row and
    /// the fast row goes red, because every rate then moves the same distance.
    #[test]
    fn the_wheel_scales_with_how_fast_the_notches_come() {
        // Twenty notches, one per batch, `gap` apart. One per batch is the
        // pessimistic shape: a real flick delivers them in handfuls, which
        // charges the meter harder still.
        fn run(gap_ms: u64) -> usize {
            let mut m = WheelMeter::default();
            let mut t = std::time::Instant::now();
            let mut rows = 0;
            for _ in 0..20 {
                rows += m.rows(1, t);
                t += Duration::from_millis(gap_ms);
            }
            rows
        }
        let table: Vec<(u64, usize)> = [400, 200, 100, 50, 25, 16, 8, 4]
            .into_iter()
            .map(|g| (g, run(g)))
            .collect();
        for (gap, rows) in &table {
            eprintln!("  {:>4} ms between notches ({:>5.0} notches/s) -> {rows:>4} rows for 20 notches",
                      gap, 1000.0 / *gap as f64);
        }

        // **MONOTONE.** Scrolling harder never moves less.
        for w in table.windows(2) {
            assert!(
                w[1].1 >= w[0].1,
                "a FASTER burst ({} ms) moved LESS than a slower one ({} ms): {} < {}",
                w[1].0,
                w[0].0,
                w[1].1,
                w[0].1
            );
        }
        // **THE FLOOR IS EXACT** at a deliberate rate.
        assert_eq!(
            table[0].1, 20,
            "twenty deliberate notches must be twenty rows, not {}",
            table[0].1
        );
        // **AND IT ACTUALLY TRAVELS** at a fast one — this is the complaint.
        //
        // Five times the floor is the bar, and the shape above is deliberately
        // the PESSIMISTIC one: one notch per batch means the meter has to
        // charge from rest over the whole burst, so the first notches are near
        // the floor and only the last ones are at the ceiling. That shape is
        // what the MOMENTUM TAIL looks like, and a smooth ramp is the right
        // answer there.
        assert!(
            table.last().unwrap().1 >= 100,
            "twenty notches at 250/s moved only {} rows — that is still 'sooo slow'",
            table.last().unwrap().1
        );
        // **WHAT A REAL FLICK LOOKS LIKE**, and it is the other shape: the OS
        // hands the face twenty notches inside ONE `read()`, so the meter is at
        // the ceiling on the first and only charge.
        let mut one_read = WheelMeter::default();
        let in_one_read = one_read.rows(20, std::time::Instant::now());
        eprintln!("  20 notches in ONE read                        -> {in_one_read:>4} rows");
        assert!(
            in_one_read > table.last().unwrap().1,
            "a flick delivered in one read ({in_one_read}) must beat the same notches \
             dribbled in one at a time ({})",
            table.last().unwrap().1
        );
        assert_eq!(
            in_one_read,
            (20.0 * WHEEL_GAIN_MAX) as usize,
            "a twenty-notch read is a hard flick and must be at the ceiling"
        );
        // **AND IT IS BOUNDED.** No rate may exceed the ceiling.
        assert!(
            table.last().unwrap().1 <= (20.0 * WHEEL_GAIN_MAX) as usize,
            "the curve is not bounded by WHEEL_GAIN_MAX"
        );
    }

    /// **THE END OF A HARD FLICK, SECOND BY SECOND — and it is the case that
    /// decides whether any of this feels right.**
    ///
    /// macOS delivers wheel reports for a second or two AFTER his fingers
    /// leave the glass, at a DECAYING rate. An estimator that held a peak
    /// would read that tail as continued hard scrolling and keep accelerating
    /// after he stopped touching the device.
    ///
    /// This drives exactly that shape — a hard batch, then a tail that thins
    /// from one notch every frame to one notch every fifth of a second — and
    /// asserts that the ROWS PER NOTCH only ever falls, and is back on the
    /// floor before the tail ends.
    ///
    /// FORCED: replace the decay in [`WheelMeter::rows`] with
    /// `self.charge.max(decayed + n)` — a peak hold, the naive fix — and the
    /// monotone-decrease assertion goes red on the first thinning batch.
    #[test]
    fn a_momentum_tail_decelerates_rather_than_running_away() {
        let mut m = WheelMeter::default();
        let mut t = std::time::Instant::now();

        // The flick itself: forty notches out of one read.
        let flick = m.rows(40, t);
        assert!(flick >= 400, "the flick itself only moved {flick} rows");

        // The tail. Gaps widen; notches per batch fall. This is what the OS
        // does with its fingers off the glass.
        let tail: [(u64, usize); 8] = [
            (16, 3),
            (16, 2),
            (33, 2),
            (33, 1),
            (66, 1),
            (100, 1),
            (150, 1),
            (200, 1),
        ];
        let mut last_per_notch = flick as f32 / 40.0;
        let mut travelled = 0usize;
        for (gap, n) in tail {
            t += Duration::from_millis(gap);
            let rows = m.rows(n, t);
            travelled += rows;
            let per_notch = rows as f32 / n as f32;
            eprintln!("  +{gap:>3}ms  {n} notch(es) -> {rows:>3} rows  ({per_notch:>5.1} rows/notch)");
            assert!(
                per_notch <= last_per_notch + f32::EPSILON,
                "the tail ACCELERATED: {per_notch} rows/notch after {last_per_notch}"
            );
            last_per_notch = per_notch;
        }
        assert!(
            last_per_notch <= 1.0,
            "the tail was still at {last_per_notch} rows/notch when it ended — \
             the face is still moving after he let go"
        );
        eprintln!("  flick {flick} rows + tail {travelled} rows = {} total", flick + travelled);

        // And after the reports stop, the meter is back at rest: the next
        // deliberate notch is one row, not a continuation of the flick.
        t += Duration::from_millis(600);
        assert_eq!(
            m.rows(1, t),
            1,
            "a deliberate notch after the tail was charged as part of the flick"
        );
    }

    /// **THE MENU AND THE TRANSCRIPT ARE ONE MECHANISM WITH TWO MOUNT POINTS.**
    ///
    /// He asked for the same property twice, for two surfaces, and pre-rejected
    /// a bigger fixed step both times. The way that promise is kept is that the
    /// curve is applied in [`App::flush_wheel`], ABOVE the fork in
    /// [`App::scroll_run`] where a transcript's viewport parts company with a
    /// menu's cursor — so there is no second implementation to drift.
    ///
    /// This asserts the consequence: the identical batch of notches moves the
    /// identical number of rows on both surfaces.
    ///
    /// FORCED: move the `wheel.rows` call down into `scroll_run`'s
    /// `Mode::Thread` arm and give the list arm back a constant. The transcript
    /// assertions all stay green and this one goes red — which is the exact
    /// shape of the drift it exists to forbid.
    #[tokio::test]
    async fn the_menu_and_the_transcript_move_by_the_same_curve() {
        use crate::event::Input;
        let t0 = std::time::Instant::now();
        // FOUR notches, not forty: the root's bands are capped at ten rows
        // each, so a hard flick would saturate the cursor against the end of
        // the menu and the two surfaces would be compared at a clamp rather
        // than at the curve.
        const NOTCHES: usize = 4;
        let flick: Vec<Input> = (0..NOTCHES).map(|_| Input::Wheel { up: false }).collect();

        let mut m = app();
        m.store.adopt_projects(vec![many_proj("harness", 40)]);
        m.lay(100, 24);
        m.project();
        m.on_input_at(flick.clone(), t0);
        let menu_rows = m.project_cursor;
        assert!(
            m.vs.projects.selectable() > menu_rows + 1,
            "the menu fixture is too short to tell a curve from a clamp"
        );

        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        for seq in 1..=400u64 {
            event(
                &mut a,
                "t1",
                seq,
                json!({"kind":"assistant_text","text": format!("line {seq}")}),
            );
        }
        a.lay(100, 24);
        a.project();
        // Up, because a transcript at the bottom has nowhere down to go.
        a.on_input_at(
            (0..NOTCHES).map(|_| Input::Wheel { up: true }).collect(),
            t0,
        );
        let chat_rows = a.vp().from_bottom;

        assert_eq!(
            menu_rows, chat_rows,
            "the same flick moved {menu_rows} menu rows and {chat_rows} transcript rows — \
             they are not sharing the mechanism"
        );
        assert!(
            menu_rows > NOTCHES,
            "a {NOTCHES}-notch flick moved {menu_rows} rows — the curve is not mounted \
             on the menu, only the floor is"
        );
    }

    /// **AN ARRIVAL NEVER MOVES A ROW HE IS READING — ASSERTED ON THE PAINTED
    /// FRAME, WHICH IS THE POINT.**
    ///
    /// > when my scroll isn't at the bottom of the convo, then new messages
    /// > should not scroll me down. Only if I'm at the bottom.
    ///
    /// **`an_arrival_while_scrolled_up_never_yanks_the_viewport` already
    /// asserts `from_bottom`, and it was green over a face that scrolled
    /// thirty-five of forty rows away under him.** `from_bottom` is a distance
    /// from the bottom of the laid-out content, so it is only a PLACE while
    /// that bottom holds still — and a reply streaming into the last held
    /// block moves the bottom without touching any number this face records.
    /// The frame is the only witness that can tell, which is why this test is
    /// phrased in rows of text rather than in fields.
    ///
    /// Both arrival shapes, because they fail differently:
    ///
    ///   * a WHOLE NEW BLOCK — held back by `new_since`, and only the `↓ N new`
    ///     chip may change;
    ///   * a block that GROWS IN PLACE while he is scrolled up — the case
    ///     `0097` measured failing, now pinned by [`Frozen::tail`].
    ///
    /// FORCED: delete the `Some(ref pinned) => *live = pinned.clone()` arm of
    /// [`App::pin_the_frozen_tail`] (leave the capture) and the second half
    /// goes red with 35 of 40 rows changed, while every `from_bottom`
    /// assertion in this file stays green.
    #[tokio::test]
    async fn an_arrival_never_moves_a_row_he_is_reading() {
        /// **EVERY ROW MUST STILL BE THE ROW IT WAS, AT THE INDEX IT WAS.**
        ///
        /// Not a set comparison: a yank moves text to a different screen row,
        /// so the index is the whole assertion. The `↓ N new` chip is the one
        /// exemption — it is PINNED to the bottom edge of the content band, it
        /// overwrites the row that would otherwise be there, and it is
        /// SUPPOSED to change: it is how he learns the bottom grew.
        fn same_rows(before: &str, after: &str, what: &str) {
            let b: Vec<&str> = before.lines().collect();
            let a: Vec<&str> = after.lines().collect();
            assert_eq!(b.len(), a.len(), "{what}: the frame changed height");
            let mut chip = 0;
            for (i, (was, now)) in b.iter().zip(a.iter()).enumerate() {
                if now.contains("new — click here") {
                    chip += 1;
                    continue;
                }
                assert_eq!(was, now, "{what}: screen row {i} moved");
            }
            assert!(chip <= 1, "{what}: more than one row claimed to be the chip");
        }

        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        for seq in 1..=120u64 {
            event(
                &mut a,
                "t1",
                seq,
                json!({"kind":"assistant_text","text": format!("line {seq}")}),
            );
        }
        // A reply is ALREADY STREAMING when he scrolls up. This is the shape
        // that was never built for, and it is the ordinary case: the agent is
        // talking and he scrolls back to re-read something.
        event(
            &mut a,
            "t1",
            500,
            json!({"kind":"stream_event","raw":{"event":{"type":"content_block_start",
                   "index":0,"content_block":{"type":"text"}}}}),
        );
        event(
            &mut a,
            "t1",
            501,
            json!({"kind":"stream_event","raw":{"event":{"type":"content_block_delta",
                   "index":0,"delta":{"type":"text_delta","text":"first words. "}}}}),
        );
        a.lay(160, 40);
        a.project();
        a.on_intent(Intent::ScrollUp);
        a.on_intent(Intent::ScrollUp);
        a.on_intent(Intent::ScrollUp);

        let was = a.vp().from_bottom;
        assert!(was > 0, "the fixture never left the bottom");
        let before = painted(&a);

        // ── SHAPE ONE: the reply keeps being typed ──
        for seq in 502..=560u64 {
            event(
                &mut a,
                "t1",
                seq,
                json!({"kind":"stream_event","raw":{"event":{"type":"content_block_delta",
                       "index":0,"delta":{"type":"text_delta",
                       "text":"a long streamed sentence that wraps and adds rows. "}}}}),
            );
        }
        same_rows(
            &before,
            &painted(&a),
            "a reply growing in place scrolled him down",
        );
        assert_eq!(a.vp().from_bottom, was, "the viewport was yanked");
        assert!(!a.vp().follow, "an arrival re-armed follow mode");

        // ── SHAPE TWO: a whole new block lands ──
        event(&mut a, "t1", 600, json!({"kind":"turn_ended","exit_code":0}));
        same_rows(&before, &painted(&a), "a new block scrolled him down");
        assert_eq!(a.vp().from_bottom, was);
        assert!(!a.vp().follow);
        assert!(
            a.vs.viewport.new_since > 0,
            "nothing was held back, so nothing was being protected"
        );

        // ── AND THE THAW IS THE ONE DELIBERATE JUMP HE ASKED FOR ──
        a.on_intent(Intent::Bottom);
        assert!(a.vp().follow);
        assert_eq!(a.vs.viewport.new_since, 0);
        let after = painted(&a);
        assert_ne!(
            after, before,
            "`end` did not bring the held content into view"
        );
        assert!(
            after.contains("a long streamed sentence"),
            "the pinned paint outlived the thaw — he cannot see what arrived"
        );
    }

    /// The chip is PER VIEW. An event on another thread is not new to the
    /// thread he has open, and counting it there would be a promise about
    /// content that is not going to be down there.
    #[tokio::test]
    async fn an_event_on_another_thread_does_not_chip_the_open_one() {
        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        a.on_intent(Intent::ScrollUp);

        event(&mut a, "t2", 1, json!({"kind":"turn_ended","exit_code":0}));
        assert_eq!(
            a.vs.viewport.new_since, 0,
            "another thread's event chipped the open one"
        );

        event(&mut a, "t1", 1, json!({"kind":"turn_ended","exit_code":0}));
        assert_eq!(a.vs.viewport.new_since, 1, "its own event did not");

        // The fleet lane saw both, and its own chip is its own: it was at the
        // bottom the whole time, so it followed rather than counting.
        a.on_intent(Intent::Back);
        a.on_intent(Intent::SwapView);
        assert_eq!(a.vs.viewport.new_since, 0, "a following view never chips");
        assert!(
            a.vs.blocks.len() >= 2,
            "the fleet lane holds both threads' events"
        );
    }

    /// The chip counts BLOCKS, not events — and the two differ, because the
    /// lane folds a run into one row. A counter would promise rows that were
    /// never going to be there.
    #[tokio::test]
    async fn the_chip_counts_rows_he_will_actually_see_not_events_that_arrived() {
        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        a.on_intent(Intent::ScrollUp);
        // `task_progress` is the fold this test used to reach through
        // `stream_event`, which is not a row at all any more. The promise is
        // unchanged and so is the arithmetic: six events, one live row.
        for i in 1..=6 {
            event(
                &mut a,
                "t1",
                i,
                json!({"kind":"task_progress","task_id":"agent-a","description":format!("step {i}")}),
            );
        }
        assert_eq!(
            a.vs.viewport.new_since, 1,
            "six folded events are ONE new row, and the chip must say one"
        );
    }

    /// **A MESSAGE THAT BUILT ITSELF OUT OF FOUR HUNDRED DELTAS IS ONE THING.**
    ///
    /// `new_since` counts what he has not seen, and a reply he has been WATCHED
    /// growing is one of them — not one per keystroke. The chip is derived from
    /// the block count rather than counted per event, so this is really a
    /// statement about the assembly holding ONE slot as it grows; a version
    /// that pushed a row per delta would make the chip count typing.
    ///
    /// FORCED: remove the `i + 1 == out.len()` in-place arm of `place_all` and
    /// the count climbs with the deltas.
    #[tokio::test]
    async fn a_growing_message_is_one_new_row_however_many_deltas_built_it() {
        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        a.on_intent(Intent::ScrollUp);
        event(
            &mut a,
            "t1",
            1,
            json!({"kind":"stream_event","raw":{"event":{"type":"content_block_start",
                   "index":0,"content_block":{"type":"text"}}}}),
        );
        for i in 2..=40 {
            event(
                &mut a,
                "t1",
                i,
                json!({"kind":"stream_event","raw":{"event":{"type":"content_block_delta",
                       "index":0,"delta":{"type":"text_delta","text":"word "}}}}),
            );
        }
        assert_eq!(
            a.vs.viewport.new_since, 1,
            "the chip counted keystrokes instead of messages"
        );
        // …and the box he is being held back from is the growing one.
        let last = a.vs.blocks.last().expect("no block was produced");
        assert!(last.text().starts_with("word word"), "{:?}", last.text());
    }

    /// **A CLIPBOARD WRITE IS A REPAINT SET-POINT.** `copy_to_clipboard`
    /// writes an escape sequence STRAIGHT at the terminal from inside raw
    /// mode, around ratatui. Anything of it the terminal does not consume —
    /// an old tmux, a passthrough that was not forwarded — lands on the screen
    /// as text that the differential renderer then agrees with forever, which
    /// is `0023` `defect_classes` CLASS 2 exactly.
    ///
    /// It REPLACES the mouse-handoff set-point, which went with `M`.
    #[tokio::test]
    async fn a_clipboard_write_forces_a_repaint() {
        let mut a = app();
        a.needs_full_repaint = false;
        a.copy_owed = Some("something that went at the terminal".into());
        a.pay_the_clipboard();
        assert!(
            a.needs_full_repaint,
            "bytes went past ratatui and nothing forced a repaint"
        );
        assert!(a.vs.notice.is_some(), "a copy must say what it did");
        // Nothing owed, nothing done — and in particular no second write.
        a.needs_full_repaint = false;
        a.pay_the_clipboard();
        assert!(!a.needs_full_repaint);
    }

    // ── THE DRAG ─────────────────────────────────────────────────────────

    /// **WHERE THE EXPANDER ACTUALLY IS**, asked of the frame that was drawn.
    /// Panics if no row claims one, which is the right failure: a face with
    /// expandable rows and no reachable expander is a face whose transcript
    /// cannot be opened by mouse at all.
    fn expander_cell(a: &App, w: u16, h: u16) -> (u16, u16) {
        let hits = tui_render::frame_with_hits(&a.vs, Utc::now(), w, h).1;
        for row in 0..hits.height() as u16 {
            for col in 0..w {
                if matches!(hits.hit(col, row), tui_render::Target::Expander(_)) {
                    return (col, row);
                }
            }
        }
        panic!("no expander is reachable anywhere on the frame");
    }

    /// The screen row of the first line of the first block, at `W x H`.
    fn first_content_row(a: &App, w: u16, h: u16) -> u16 {
        let b = tui_render::bands(&a.vs, w, h);
        let rows = tui_render::frame_with_hits(&a.vs, Utc::now(), w, h).1;
        ((b.header + b.banner)..(b.header + b.banner + b.content))
            .map(|r| r as u16)
            .find(|r| rows.text_row(*r).is_some())
            .expect("no content row carries text")
    }

    /// **CLICK AND DRAG ARE MUTUALLY EXCLUSIVE, AT THE COORDINATOR.** The
    /// classifier proves it over events; this proves the two paths do not
    /// converge afterwards — a drag must not activate, and a click must not
    /// select. Both directions, on the SAME cell, so neither can pass by
    /// aiming somewhere the other could not reach.
    #[tokio::test]
    async fn a_drag_selects_without_activating_and_a_click_activates_without_selecting() {
        const W: u16 = 100;
        const H: u16 = 24;

        // A CLICK ON AN EXPANDER OPENS THAT ROW and selects nothing.
        //
        // **The cell is found by ASKING THE HIT MAP where the expander is**,
        // not by predicting it. That is the whole difference between proving
        // the affordance is reachable and proving a constant: if the glyph and
        // the claim ever drift apart, this goes red rather than testing the
        // number it was written with.
        let mut clicked = app();
        clicked.open_thread("t1".into());
        clicked.store.load_transcript("t1", Vec::new(), 1);
        expandable_rows(&mut clicked);
        clicked.lay(W, H);
        let (ecol, erow) = expander_cell(&clicked, W, H);
        assert!(clicked.opened.is_empty());
        clicked.on_press(ecol, erow);
        clicked.on_click(ecol, erow);
        assert_eq!(
            clicked.opened.len(),
            1,
            "a click on the expander did not activate"
        );
        assert!(
            clicked.vs.selection.is_none(),
            "a click left a selection behind"
        );
        assert!(clicked.copy_owed.is_none(), "a click copied something");

        // **AND THE BODY OF THE SAME ROW IS A FREE MISS.** His retraction, at
        // the coordinate that used to act: two columns to the right of a
        // button that still works, nothing happens at all.
        let mut body = app();
        body.open_thread("t1".into());
        body.store.load_transcript("t1", Vec::new(), 1);
        expandable_rows(&mut body);
        body.lay(W, H);
        body.on_press(10, erow);
        body.on_click(10, erow);
        assert!(
            body.opened.is_empty(),
            "a click on the row's BODY expanded it — the retraction is not in force"
        );
        assert!(body.vs.notice.is_none(), "a missed click said something");

        // A DRAG across the same cell selects and does NOT activate.
        let mut dragged = app();
        dragged.open_thread("t1".into());
        dragged.store.load_transcript("t1", Vec::new(), 1);
        expandable_rows(&mut dragged);
        dragged.lay(W, H);
        let row = first_content_row(&dragged, W, H);
        assert!(dragged.opened.is_empty());
        dragged.on_press(10, row);
        dragged.extend_to(30, row);
        dragged.finish_drag(30, row);
        assert!(
            dragged.opened.is_empty(),
            "a DRAG activated — the click fired at the end of it, which is IS-21's other half"
        );
        assert!(dragged.vs.selection.is_some(), "a drag selected nothing");
        let copied = dragged.copy_owed.clone().expect("a drag copied nothing");
        assert!(!copied.is_empty());
        // What it took is the LOGICAL text — no gutter, no glyph, no rails.
        assert!(
            !copied.contains('▌'),
            "a rail reached the clipboard: {copied:?}"
        );
    }

    /// **THE COPY IS THE LOGICAL TEXT OF THE BLOCKS IT CROSSED**, unwrapped
    /// and without the chrome the rows were drawn with — and a drag across
    /// several messages joins them one per line.
    #[tokio::test]
    async fn a_drag_across_two_messages_copies_both_in_full_and_nothing_else() {
        const W: u16 = 60;
        const H: u16 = 24;
        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        let first = "the first message, long enough that it certainly wraps at sixty columns";
        let second = "and the second one";
        event(
            &mut a,
            "t1",
            1,
            json!({"kind":"assistant_text","text":first}),
        );
        event(
            &mut a,
            "t1",
            2,
            json!({"kind":"user_message","text":second}),
        );
        a.lay(W, H);

        let top = first_content_row(&a, W, H);
        a.on_press(0, top);
        // …all the way to the last row of the band, which is the second
        // message's last row.
        let b = tui_render::bands(&a.vs, W, H);
        let last = (b.header + b.banner + b.content - 1) as u16;
        a.extend_to(W - 1, last);
        a.finish_drag(W - 1, last);

        let copied = a.copy_owed.clone().expect("nothing was copied");
        assert!(
            copied.contains(first),
            "the first message came back clipped or wrapped: {copied:?}"
        );
        assert!(copied.contains(second), "{copied:?}");
        assert!(
            !copied.contains('▌'),
            "a rail reached the clipboard: {copied:?}"
        );
        assert!(
            !copied.contains('▐'),
            "a rail reached the clipboard: {copied:?}"
        );
        // One line per message, not one line per ROW — the first message is
        // wider than the box and was definitely drawn on several.
        assert_eq!(
            copied.lines().count(),
            2,
            "the hard wraps came with it: {copied:?}"
        );
    }

    /// **AUTOSCROLL: a drag past the top edge scrolls the band and keeps
    /// going.** Without the tick the selection stops at the first visible row,
    /// because a pointer held still at the edge produces no terminal reports
    /// at all.
    #[tokio::test]
    async fn a_drag_held_at_the_top_edge_scrolls_and_the_anchor_does_not_move() {
        const W: u16 = 100;
        const H: u16 = 24;
        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        for i in 1..=60 {
            event(
                &mut a,
                "t1",
                i,
                json!({"kind":"assistant_text","text":format!("message number {i}")}),
            );
        }
        a.lay(W, H);
        let b = tui_render::bands(&a.vs, W, H);
        let bottom = (b.header + b.banner + b.content - 1) as u16;
        let top = (b.header + b.banner) as u16;

        // Press near the BOTTOM, drag to the top edge, then hold.
        a.on_press(10, bottom);
        let anchor = a.drag.expect("the press anchored nothing").anchor;
        a.extend_to(10, top);
        assert_eq!(
            a.drag.and_then(|d| d.autoscroll),
            Some(true),
            "the pointer is at the top edge and the band is not scrolling"
        );
        let before = a.vs.viewport.from_bottom;
        for _ in 0..5 {
            a.on_autoscroll();
            a.lay(W, H);
        }
        assert!(
            a.vs.viewport.from_bottom > before,
            "five ticks at the edge scrolled nothing"
        );
        assert_eq!(
            a.drag.expect("the drag ended by itself").anchor,
            anchor,
            "the anchor MOVED while the content scrolled under it — that is the \
             failure a screen-cell selection has and a logical one does not"
        );
        // The selection now reaches back past what is on screen, and the copy
        // gets it anyway, because content text is read from the model.
        a.finish_drag(10, top);
        let copied = a.copy_owed.clone().expect("nothing was copied");
        assert!(
            copied.lines().count() > b.content,
            "the copy stopped at the screen's edge ({} lines, band is {})",
            copied.lines().count(),
            b.content
        );
    }

    /// **A DRAG WORKS ON A HEADER CELL AND A FOOTER CELL.** He said ANY part
    /// of the interface, and the two bands most likely to have been forgotten
    /// are the two that are not the transcript.
    #[tokio::test]
    async fn the_header_and_the_footer_are_draggable_too() {
        const W: u16 = 100;
        const H: u16 = 24;
        let mut a = app();
        a.store.adopt_projects(vec![proj("harness", &["a", "b"])]);
        a.project();
        a.lay(W, H);

        a.on_press(0, 0);
        a.extend_to(20, 0);
        a.finish_drag(20, 0);
        let header = a.copy_owed.take().expect("the header copied nothing");
        assert!(
            a.vs.header
                .title
                .starts_with(&header[..5.min(header.len())]),
            "the header copy is not the header: {header:?}"
        );

        a.on_press(2, H - 1);
        a.extend_to(20, H - 1);
        a.finish_drag(20, H - 1);
        let footer = a.copy_owed.take().expect("the footer copied nothing");
        assert!(
            footer.contains("keys") || footer.contains('?'),
            "the footer copy is not the footer: {footer:?}"
        );
    }

    /// A drag on a frame with nothing copyable on it says nothing at all — the
    /// same rule a missed click obeys. Punishing an imprecise gesture is how a
    /// pointer interface starts to feel bad, and the too-small frame is the
    /// one state where every row really is unselectable: it draws one sentence
    /// and no bands, deliberately.
    #[tokio::test]
    async fn a_drag_on_a_frame_with_no_text_on_it_is_silent() {
        let mut a = app();
        a.lay(30, 6); // below MIN_WIDTH x MIN_HEIGHT
        a.on_press(0, 2);
        assert!(a.drag.is_none(), "a press on nothing anchored a gesture");
        a.finish_drag(10, 3);
        assert!(a.copy_owed.is_none(), "an empty drag copied something");
        assert!(
            a.vs.notice.is_none(),
            "an empty drag punished him with a toast"
        );
    }

    /// A gesture that leaves the frame is CLAMPED to it rather than dropped:
    /// the pointer routinely runs past the bottom row on the way to selecting
    /// the last message, and a drag that gave up there would feel broken.
    #[tokio::test]
    async fn a_pointer_that_leaves_the_frame_is_clamped_rather_than_lost() {
        let mut a = app();
        a.store.adopt_projects(vec![proj("harness", &["a", "b"])]);
        a.project();
        a.lay(100, 24);
        a.on_press(0, 0);
        a.extend_to(0, 500);
        let sel =
            a.vs.selection
                .expect("a drag off the bottom selected nothing");
        assert!(sel.head.unit >= sel.anchor.unit);
    }

    #[tokio::test]
    async fn a_face_that_has_not_been_told_anything_never_renders_a_zero() {
        let a = app();
        assert!(matches!(a.vs.header.queued, tui_render::Known::Unknown));
        assert!(matches!(
            a.vs.header.unread_elsewhere,
            tui_render::Known::Unknown
        ));
        assert!(matches!(
            a.vs.header.link.state,
            tui_render::LinkState::Connecting { .. }
        ));
    }

    /// **THE FOURTH CLOCK IS ARMED BY THE LINK GOING DOWN, AND BY NOTHING
    /// ELSE.**
    ///
    /// The duration the banner states is recomputed from the frame's `now`, so
    /// it is exact on every frame that HAPPENS — and through an outage the
    /// face has nothing to react to but a reconnect ladder whose backoff grows
    /// to tens of seconds. Without a timer the screen would hold the second
    /// the socket died until the daemon came back, which is the defect wearing
    /// a correct implementation as a disguise. This is the wiring assertion;
    /// `tui_render::Link::refresh_in`'s own tests own how COARSE it is.
    ///
    /// The other half matters just as much: a healthy face arms NOTHING, so
    /// the loop still blocks on `recv()` with no timers at all.
    #[tokio::test]
    async fn a_down_link_owes_itself_a_frame_and_a_healthy_one_owes_nothing() {
        let mut a = app();
        assert!(
            a.link_deadline().is_none(),
            "a face with a healthy link armed a timer nothing on screen needs"
        );

        a.on_transport(TransportEvent::Disconnected {
            reason: "the daemon closed the socket".into(),
            next_retry_in_ms: 2000,
        });
        let d = a
            .link_deadline()
            .expect("a face sitting still through an outage would never repaint");
        assert!(
            d <= Duration::from_secs(1),
            "the age is spelled in seconds at this point, so the frame is owed within one: {d:?}"
        );

        // …and the link coming back disarms it, rather than leaving a timer
        // waking a face that has nothing left to say about time.
        a.on_transport(TransportEvent::Connected {
            boot_id: "b1".into(),
            resync: crate::transport::ResyncKind::First,
        });
        assert!(a.link_deadline().is_none(), "the clock outlived the outage");
    }

    fn down(a: &mut App) {
        a.on_transport(TransportEvent::Disconnected {
            reason: "the daemon closed the socket".into(),
            next_retry_in_ms: 2000,
        });
    }

    fn up(a: &mut App) {
        a.on_transport(TransportEvent::Connected {
            boot_id: "b2".into(),
            resync: crate::transport::ResyncKind::Restart,
        });
    }

    /// Bytes from the wire, through the vt parser and into `on_input_at` —
    /// THE LOOP'S OWN DOOR, not `deliver`'s shortcut past it, because the down
    /// page's key arm lives in `on_input_at` and a helper that skips that hop
    /// would test a face nobody runs.
    fn type_in(a: &mut App, bytes: &[u8]) {
        let mut p = crate::vt::Parser::new();
        let mut out = Vec::new();
        p.feed(bytes, std::time::Instant::now(), &mut out);
        let batch: Vec<crate::event::Input> = out
            .into_iter()
            .filter_map(|o| match o {
                crate::vt::Out::Event(ratatui::crossterm::event::Event::Key(k)) => {
                    Some(crate::event::Input::Key(k))
                }
                _ => None,
            })
            .collect();
        assert!(!batch.is_empty(), "those bytes produced no key: {bytes:?}");
        a.on_input_at(batch, std::time::Instant::now());
    }

    fn screen(a: &App) -> String {
        tui_render::frame(&a.vs, Utc::now(), 120, 30)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>() + "\n")
            .collect()
    }

    /// **THE DOWN PAGE ARRIVES WITH THE OUTAGE AND LEAVES WITH IT, WITH NO KEY**
    /// (R7.S1c val 2's face half). Both directions, on the same app: a healthy
    /// link draws no page, a down one draws exactly one, and the link coming
    /// back takes it away by itself — which is "reconnects without a key
    /// press" as far as the face is concerned.
    ///
    /// FORCED RED by making `down_page` return `None` (never shows) and by
    /// dropping its `Connected` arm (never leaves).
    #[tokio::test]
    async fn the_down_page_comes_with_the_outage_and_leaves_by_itself() {
        let mut a = app();
        assert!(a.vs.down.is_none(), "a face that never went down drew a down page");
        assert!(!screen(&a).contains("threads is down"));

        down(&mut a);
        let d = a.vs.down.clone().expect("a down link drew no down page");
        assert_eq!(d.service, "threads");
        assert!(d.restart_cmd.ends_with("start threads"), "{d:?}");
        let s = screen(&a);
        assert!(s.contains("threads is down"), "{s}");
        assert!(s.contains("[r] restart now"), "the button is not on the page: {s}");

        up(&mut a);
        assert!(a.vs.down.is_none(), "the page outlived the outage");
        assert!(!screen(&a).contains("threads is down"));
    }

    /// **THE PAGE SAYS WHETHER ANYTHING WILL RELAUNCH THREADS**, both ways: a
    /// constant sentence fails one arm. FORCED RED by returning the `Some(true)`
    /// sentence for every input.
    #[test]
    fn the_down_page_says_whether_the_environment_relaunches() {
        let on = comes_back("t1", Some(true));
        let off = comes_back("t1", Some(false));
        assert!(on.contains("auto_restart is on in `t1`") && on.contains("relaunches"), "{on}");
        assert!(off.contains("auto_restart is OFF in `t1`") && off.contains("nothing will"), "{off}");
        assert!(comes_back("t1", None).contains("not known"));
    }

    /// **ON THE DOWN PAGE A LETTER IS NOT TEXT**, and the opposing case: the
    /// same letter types once the page is gone. The page covers the composer,
    /// so a keystroke that reached it would be typing blind.
    ///
    /// FORCED RED by deleting the `self.vs.down.is_some()` arm in `on_input_at`.
    #[tokio::test]
    async fn on_the_down_page_a_letter_is_swallowed_and_types_again_after() {
        let mut a = app();
        in_a_thread(&mut a);
        down(&mut a);
        type_in(&mut a, b"x");
        assert_eq!(a.vs.composer.buf.text(), "", "a key typed into the covered composer");
        up(&mut a);
        type_in(&mut a, b"x");
        assert_eq!(a.vs.composer.buf.text(), "x", "the composer stopped taking text");
    }

    /// **`r` IS THE RESTART BUTTON: IT RUNS `cauldron --env <name> start
    /// threads` AND THE PAGE SAYS WHAT CAME BACK.** A fake `cauldron` records
    /// its argv, so what is asserted is the command that actually ran.
    ///
    /// The opposing case is in the same test: `r` with the link UP does not
    /// run anything (the fake's record stays empty).
    ///
    /// FORCED RED by changing `restart_args` to name `stop`, and by making the
    /// `r` arm not call `restart_daemon`.
    #[tokio::test]
    async fn r_on_the_down_page_runs_cauldron_start_threads_in_its_environment() {
        let dir = std::env::temp_dir().join(format!("tui-restart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("argv");
        let _ = std::fs::remove_file(&record);
        let fake = dir.join("cauldron");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$@\" >> {}\necho 'env unittest (from flag)'\necho 'cauldron: started threads'\n",
                record.display()
            ),
        )
        .unwrap();
        assert!(std::process::Command::new("/bin/chmod").arg("+x").arg(&fake).status().unwrap().success());
        std::env::set_var("CAULDRON_BIN", &fake);
        crate::cauldron_env::set_resolved(crate::cauldron_env::Resolved {
            env: "unittest".into(),
            root: dir.clone(),
            addr: "127.0.0.1:1".into(),
            port: 1,
            source: crate::cauldron_env::Source::Override,
            auto_restart: Some(true),
        });
        let env = crate::cauldron_env::resolved().unwrap().env.clone();

        let (handle, _rx) = mirror::spawn_dead();
        let (tx, mut erx) = mpsc::channel::<Ev>(16);
        let mut a = App::new(handle, tx, "ws://127.0.0.1:1/ws".into(), "/Users/x".into(), 0);

        // UP: `r` is not the button.
        a.on_transport(TransportEvent::Connected {
            boot_id: "b1".into(),
            resync: crate::transport::ResyncKind::First,
        });
        type_in(&mut a, b"r");
        // `restarting` is set SYNCHRONOUSLY by the button, before anything is
        // spawned — so this is a negative with no period to outlast (doctrine
        // 4): it cannot be green merely because a spawned process was slow.
        assert!(!a.restarting, "`r` with the link up pressed the restart button");
        assert!(!record.exists(), "`r` with the link up ran cauldron");

        // DOWN: it is.
        down(&mut a);
        type_in(&mut a, b"r");
        assert_eq!(a.vs.down.as_ref().unwrap().restart_note.as_deref(), Some("running…"));
        let note = loop {
            match tokio::time::timeout(Duration::from_secs(10), erx.recv()).await {
                Ok(Some(Ev::DownRestart(n))) => break n,
                Ok(Some(_)) => continue,
                other => panic!("the restart never reported back: {other:?}"),
            }
        };
        a.apply(Ev::DownRestart(note));
        std::env::remove_var("CAULDRON_BIN");
        let argv = std::fs::read_to_string(&record).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(argv.trim(), format!("--env {env} start threads"));
        assert_eq!(
            a.vs.down.as_ref().unwrap().restart_note.as_deref(),
            Some("exit 0: cauldron: started threads"),
            "the page must say what came back, past the banner line"
        );
    }

    #[tokio::test]
    async fn a_disconnect_forces_a_full_repaint_and_keeps_the_rows() {
        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        event(
            &mut a,
            "t1",
            1,
            json!({"kind":"assistant_text","text":"a fact that stays true"}),
        );
        a.needs_full_repaint = false;
        a.on_transport(TransportEvent::Disconnected {
            reason: "the daemon closed the socket".into(),
            next_retry_in_ms: 2000,
        });
        assert!(
            a.needs_full_repaint,
            "a reconnect repaints over a screen the last session left behind"
        );
        assert!(
            a.vs.blocks
                .iter()
                .any(|b| b.text().contains("a fact that stays true")),
            "the transcript is history and does not go down with the link"
        );
        assert!(a.vs.banner.is_some(), "a down link must be announced");
    }

    /// An overlay is a way of READING the view under it, so it owes that view
    /// back — including the one it was not raised from.
    #[tokio::test]
    async fn every_overlay_returns_to_the_view_it_was_raised_over() {
        let mut a = app();
        a.on_intent(Intent::SwapView);
        assert_eq!(a.vs.mode, Mode::Activity);
        for raise in [Intent::Help, Intent::QueueOverlay] {
            a.on_intent(raise);
            assert!(a.vs.mode.is_overlay(), "{raise:?} did not raise an overlay");
            a.on_intent(Intent::Back);
            assert_eq!(
                a.vs.mode,
                Mode::Activity,
                "{raise:?} lost the view under it"
            );
        }
    }

    /// `CON-4`: the selection is still TRANSIENT. It is kept after the release
    /// so he can SEE what he took, and destroyed by the next gesture or the
    /// next keystroke — nothing persists to become a cursor, and nothing
    /// navigates by it.
    #[tokio::test]
    async fn a_selection_survives_the_release_and_nothing_else() {
        const W: u16 = 100;
        const H: u16 = 24;
        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        event(
            &mut a,
            "t1",
            1,
            json!({"kind":"assistant_text","text":"one two three"}),
        );
        a.lay(W, H);
        let row = first_content_row(&a, W, H);

        a.on_press(0, row);
        a.extend_to(40, row);
        a.finish_drag(40, row);
        assert!(
            a.vs.selection.is_some(),
            "the highlight vanished before he could see what he copied"
        );
        assert!(a.drag.is_none(), "the gesture outlived the release");

        // A keystroke ends it.
        a.clear_selection();
        assert!(
            a.vs.selection.is_none(),
            "a selection that survives is a cursor"
        );

        // …and so does the next press.
        a.on_press(0, row);
        a.extend_to(40, row);
        a.finish_drag(40, row);
        assert!(a.vs.selection.is_some());
        a.on_press(0, row);
        assert!(
            a.vs.selection.is_none(),
            "the last selection was still painted under the new gesture"
        );
    }

    /// **THE STDOUT-ONLY TOGGLE IS SESSION STATE, AND THE HEADER SAYS SO.**
    ///
    /// This replaces `the_filter_survives_a_view_change_and_the_header_says_
    /// it_is_on`, which pressed `f`, then `3`, then read two counters off the
    /// bitmask. There is one boolean now.
    ///
    /// **THE TOGGLE MUST BE FORCED TO FAIL AND THIS IS THE TEST THAT DOES IT.**
    /// FORCED: make the `Intent::StdoutOnly` arm in `on_intent` a no-op and
    /// assertion 1 goes red; make it `= true` instead of `= !` and assertion 3
    /// goes red (it never comes back); move the state into `App` instead of
    /// `ViewState` and assertion 2 goes red (it does not survive the view
    /// change, because `project` rebuilds `vs` from the store).
    #[tokio::test]
    async fn the_stdout_toggle_flips_survives_a_view_change_and_comes_back() {
        let mut a = app();
        assert!(!a.vs.stdout_only, "the face started collapsed");

        // 1 — ONE KEY, AND THE FACE IS COLLAPSED.
        a.on_intent(Intent::StdoutOnly);
        assert!(a.vs.stdout_only, "the toggle did not arm");

        // 2 — IT IS SESSION STATE, NOT VIEW STATE.
        a.on_intent(Intent::SwapView);
        assert!(
            a.vs.stdout_only,
            "the toggle is view state and it must be session state"
        );

        // 3 — AND PRESSING IT AGAIN BRINGS EVERYTHING BACK. There is nothing
        // to stash: `DIR-4`'s breadth IS the off state now, so the way back is
        // the way back to everything and there is no tuned filter to lose.
        a.on_intent(Intent::StdoutOnly);
        assert!(!a.vs.stdout_only, "the toggle did not disarm");

        // 4 — AND A FRESH FACE IS NEVER COLLAPSED. `DIR-4` reserved what the
        // face SHIPS with to him and that is untouched by the deletion.
        assert!(
            !app().vs.stdout_only && !tui_render::ViewState::default().stdout_only,
            "the face now ships collapsed — DIR-4 is broken"
        );
    }

    /// **THE FILTER IS GONE FROM THE SHELL, AND THE OVERLAY DOORS THAT
    /// SURVIVED STILL WORK.**
    ///
    /// A WITNESS, not an absence (`0065`): the two remaining overlay doors are
    /// exercised positively before the deleted one is asserted absent, because
    /// an `on_intent` that had stopped dispatching anything at all would score
    /// a triumphant "no filter mode is reachable".
    ///
    /// FORCED: re-add a `Mode::Filter` variant and an `Intent::Filter` arm and
    /// this stops COMPILING, which is the strongest form the check can take —
    /// both enums are exhaustively matched in `app.rs`.
    #[tokio::test]
    async fn no_intent_raises_a_filter_and_the_other_overlays_still_open() {
        let mut a = app();
        for raise in [Intent::Help, Intent::QueueOverlay] {
            a.on_intent(raise);
            assert!(a.vs.mode.is_overlay(), "{raise:?} did not raise an overlay");
            a.on_intent(Intent::Back);
            assert!(!a.vs.mode.is_overlay(), "{raise:?} would not close");
        }
        // And the collapse is NOT an overlay — it is a state on the view he is
        // already looking at, which is the whole difference between the toggle
        // and the thing it replaced.
        a.on_intent(Intent::StdoutOnly);
        assert!(
            !a.vs.mode.is_overlay(),
            "the stdout toggle raised a surface; it must only flip a state"
        );
    }

    /// The list cursor is clamped by the PROJECTION, because the index can
    /// change under him — the AUI rewrites it on every turn he speaks.
    #[tokio::test]
    async fn the_list_cursor_never_points_past_the_end() {
        let mut a = app();
        a.store.adopt_projects(vec![proj("harness", &["a", "b"])]);
        a.open_project(0);
        a.project();
        a.on_intent(Intent::Bottom);
        assert_eq!(
            a.list_cursor, 1,
            "`G` selects the NEWEST, which is the bottom"
        );
        assert_eq!(a.focused_thread().as_deref(), Some("b"));
        // The index shrank under him.
        a.store.adopt_projects(vec![proj("harness", &["a"])]);
        a.store.enter_project(0);
        a.project();
        assert_eq!(a.list_cursor, 0, "the cursor followed the list down");
    }

    // ── the root, the descent, and the one activation path ───────────────

    /// **PROJECT -> THREAD -> TRANSCRIPT, with `←`/`→` only.** The AUI's
    /// `NavAction::Back`/`Select` mirrored all the way down, including the
    /// detail most likely to be dropped: the cursor is REMEMBERED PER DEPTH,
    /// so coming back out of a project lands on the project you were in.
    #[tokio::test]
    async fn left_and_right_walk_the_whole_depth_and_remember_the_cursor() {
        let mut a = app();
        a.store.adopt_projects(vec![
            proj("airbnb", &["x"]),
            proj("harness", &["a", "b"]),
            proj("mc", &["y"]),
        ]);
        a.project();
        assert_eq!(a.vs.mode, Mode::Projects, "the face opens on PROJECTS");

        // **THE ROOT IS BANDS NOW, AND THE PROJECTS ARE UNDER THEM.** Nothing
        // in this fixture is pinned, so the surface is the recent band under
        // its own HEADER — a cursor stop since `DEC-46` — and then the projects
        // band under ITS header. The first PROJECT therefore sits at
        // `1 + recent.len() + 1`, and it is derived here rather than written as
        // a literal so that the arithmetic is the surface's and not a second
        // reading of it.
        let first_project = 1 + a.vs.projects.recent.len() + 1;
        assert!(
            !a.vs.projects.recent.is_empty(),
            "the recent band should have rows to skip"
        );
        assert_eq!(
            a.vs.projects.item(0),
            Some(RootItem::Band(tui_render::BandId::Recent)),
            "the recent band header is not the first cursor stop"
        );
        assert_eq!(
            a.vs.projects.item(first_project),
            Some(RootItem::Project(0)),
            "the projects do not start where the bands end"
        );

        // Down past the bands to the SECOND project, then RIGHT.
        a.on_intent(Intent::Select(first_project + 1));
        assert_eq!(a.project_cursor, first_project + 1);
        let i = right(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::List, "→ at the root did not descend");
        assert_eq!(
            a.store.open_project().map(|p| p.name.as_str()),
            Some("harness")
        );
        // A project opens at its NEWEST thread.
        assert_eq!(a.focused_thread().as_deref(), Some("b"));

        // RIGHT again opens the thread.
        let i = right(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
        assert_eq!(a.store.open_id(), Some("b"));

        // **AND HERE THE MIRROR COMES BACK, WHICH IS WHAT `DIR-22` CHANGED.**
        // In a thread the composer still owns the arrows as TEXT — the table
        // says `Compose(Edit::Left)` and it must keep saying that, or a
        // textarea has stopped answering to its own cursor. What the region
        // model added is the EDGE: with nothing to the left of the caret there
        // is no text to move through, so the same key is the way out. The
        // table cannot express that condition (it cannot see a caret) which is
        // exactly why the deferral lives in `App::compose`.
        assert_eq!(
            press(&a, tui_render::Key::Left),
            Intent::Compose(tui_render::Edit::Left),
            "← stopped being text in the composer — the textarea lost its cursor"
        );
        // `←` at the buffer start is the way out of a thread since `DIR-22`;
        // `esc` is unbound in the composer and means "come home to the box"
        // in the other regions.
        let i = press(&a, tui_render::Key::Left);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::List);
        let i = left(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Projects);
        assert_eq!(
            a.project_cursor,
            first_project + 1,
            "the cursor-per-depth was lost — he came back to the top"
        );

        // **AND THE ROOT NEVER EXITS ON `←`.** It lands on row 0.
        let i = left(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Projects);
        assert!(!a.quit, "an accidental ← ended the session");
        assert_eq!(a.project_cursor, 0);
    }

    /// **A BAND ROW DRILLS INTO ITS THREAD, exactly as a project row drills
    /// into its project** — one cursor space, one `→`, two destinations. **And
    /// `esc` COMES BACK TO THE ROOT**, because the root is where he opened it.
    ///
    /// # This test asserted the opposite until `0076`, and that was the defect
    ///
    /// It used to require `esc` to land on `Mode::List` inside the thread's
    /// project, and its doc comment argued for it: *"it enters the PROJECT on
    /// the way through, which is not decoration."* That is the behaviour he
    /// complained about in his own words — *"when I go into a thread and then
    /// hit escape, it brings me back to the list of all raw threads. And I
    /// never want to see that."* A green test asserting a live complaint is
    /// worse than no test, so the assertions are INVERTED rather than deleted:
    /// the path is the same, the destination is the one he asked for.
    ///
    /// FORCED: point the `Pinned | Recent` arm of `Intent::Open` at
    /// `open_project` and the mode lands on `List` instead of `Thread`; put
    /// `store.enter_project` back into `drill_to_thread` and the
    /// `open_project()` assertion below goes red; replace `self.ascend()` in
    /// the `Mode::Thread` arm of `Intent::Back` with `Mode::List` and the last
    /// three assertions go red together.
    #[tokio::test]
    async fn a_thread_opened_from_a_root_band_escapes_back_to_the_root() {
        let mut a = app();
        a.store
            .adopt_projects(vec![proj("airbnb", &["x"]), proj("harness", &["a", "b"])]);
        a.project();
        // Nothing is pinned in this fixture, so selectable 0 is the RECENT
        // band's own header (`DEC-46`) and selectable 1 is the newest thread in
        // it — `harness`'s "b".
        assert_eq!(
            a.vs.projects.item(0),
            Some(RootItem::Band(tui_render::BandId::Recent))
        );
        assert_eq!(a.vs.projects.item(1), Some(RootItem::Recent(0)));
        a.on_intent(Intent::Select(1));
        let row = a.vs.projects.thread_at(1).expect("a recent row").clone();
        assert_eq!(row.id, "b");
        assert_eq!(row.project, "harness", "the band row lost its project");

        let i = right(&a);
        a.on_intent(i);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Composer),
            "→ on a band row did not open the thread"
        );
        assert_eq!(a.store.open_id(), Some("b"));
        // **NO PROJECT WAS ENTERED BEHIND HIS BACK.** This is the teleport at
        // its source: a project open in the store that he never chose is what
        // `esc` and `Tab` both used to read.
        assert_eq!(
            a.store.open_project().map(|p| p.name.as_str()),
            None,
            "the drill entered a project he never opened"
        );
        // **`←` IS THE WAY BACK NOW, NOT `esc`** — *"And left should go back. I
        // don't even want ESC to do that behavior."* With an empty box the
        // caret is already at the buffer start, so the first `←` has no text
        // to move through and becomes the Back this stack answers.
        //
        // **THE STACK ITSELF IS UNTOUCHED**, which is the half most likely to
        // be got wrong: what died is one row of the key table, not `origin`,
        // `descend`, `ascend` or the raw-list removal they were built for.
        let i = press(&a, tui_render::Key::Left);
        a.on_intent(i);
        assert_eq!(
            a.vs.mode,
            Mode::Projects,
            "← out of a band-opened thread did not come back to the root"
        );
        // And the ROOT's cursor is still on the row he drilled from — the
        // cursor-per-depth half of `DEC-24`, which was already right.
        assert_eq!(a.project_cursor, 1);
        assert_eq!(
            a.vs.projects.thread_at(a.project_cursor).map(|r| &*r.id),
            Some("b"),
            "the root cursor left the row he opened"
        );
    }

    /// **THE DRILLED PATH STILL GIVES BACK THE PROJECT'S LIST.** The other half
    /// of the same rule, and the half a one-line *"always back to the root"*
    /// fix would have destroyed: a man working through a project's threads is
    /// not thrown to the root after every one.
    ///
    /// FORCED: make `Intent::Back`'s `Mode::Thread` arm set `Mode::Projects`
    /// unconditionally (the rejected candidate) and the first assertion goes
    /// red; drop the `origin.push` from `drill_to_thread` and it goes red the
    /// same way, because an empty stack floors at the root.
    #[tokio::test]
    async fn a_thread_drilled_through_a_project_escapes_back_to_that_project() {
        let mut a = app();
        a.store
            .adopt_projects(vec![proj("airbnb", &["x"]), proj("harness", &["a", "b"])]);
        a.project();
        // Walk to the `harness` PROJECT row and drill: root -> list -> thread.
        let first_project = (0..a.vs.projects.selectable())
            .find(|i| matches!(a.vs.projects.item(*i), Some(RootItem::Project(_))))
            .expect("a project row");
        a.on_intent(Intent::Select(first_project + 1));
        let i = right(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::List);
        assert_eq!(
            a.store.open_project().map(|p| p.name.as_str()),
            Some("harness")
        );
        let i = right(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
        assert_eq!(a.store.open_id(), Some("b"));

        // `←` at the buffer start is the way out of a thread since `DIR-22`;
        // `esc` is unbound in the composer and means "come home to the box"
        // in the other regions.
        let i = press(&a, tui_render::Key::Left);
        a.on_intent(i);
        assert_eq!(
            a.vs.mode,
            Mode::List,
            "escaping a thread he drilled into threw him past the project"
        );
        assert_eq!(
            a.store.open_project().map(|p| p.name.as_str()),
            Some("harness"),
            "the project he chose was closed under him"
        );
        assert_eq!(
            a.focused_thread().as_deref(),
            Some("b"),
            "backing out did not land on the row he opened"
        );
        // And ONE MORE `esc` is the root, with the project left behind.
        // `←` at the buffer start is the way out of a thread since `DIR-22`;
        // `esc` is unbound in the composer and means "come home to the box"
        // in the other regions.
        let i = press(&a, tui_render::Key::Left);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Projects);
        assert_eq!(a.store.open_project().map(|p| p.name.as_str()), None);
    }

    /// **`Tab Tab` FROM THE ROOT COMES BACK TO THE ROOT** — the second teleport,
    /// the one he never described. `0073` measured it at frames E8-E10: after
    /// opening any root-band thread the store held that project open forever,
    /// and `Intent::SwapView` read exactly that flag, so `Tab` out of the event
    /// lane delivered him onto a project's list with NO escape pressed at all.
    ///
    /// It falls out of the origin stack rather than needing its own clause,
    /// which is the whole argument for a structure over a rule about doors.
    ///
    /// FORCED: restore `if self.store.open_project().is_some() { Mode::List }`
    /// in `Intent::SwapView` AND the `enter_project` in `drill_to_thread`, and
    /// the last assertion lands on `Mode::List`. Restoring either one alone
    /// leaves it green, which is why both halves are asserted here.
    #[tokio::test]
    async fn tab_out_of_the_event_lane_gives_back_the_surface_it_was_raised_over() {
        let mut a = app();
        a.store
            .adopt_projects(vec![proj("airbnb", &["x"]), proj("harness", &["a", "b"])]);
        a.project();
        // Open a band thread and come straight back out, exactly as he would.
        a.on_intent(Intent::Select(1));
        let i = right(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
        // `←` at the buffer start is the way out of a thread since `DIR-22`;
        // `esc` is unbound in the composer and means "come home to the box"
        // in the other regions.
        let i = press(&a, tui_render::Key::Left);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Projects);

        // Now the gesture that used to teleport him.
        let i = press(&a, tui_render::Key::Tab);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Activity, "tab did not reach the event lane");
        let i = press(&a, tui_render::Key::Tab);
        a.on_intent(i);
        assert_eq!(
            a.vs.mode,
            Mode::Projects,
            "tab tab from the root landed on a project he never entered"
        );

        // **AND FROM A PROJECT'S LIST IT GIVES THAT LIST BACK** — the case the
        // old clause got right by accident, kept green on purpose.
        //
        // (There is no `Tab` from a THREAD to assert: the composer owns that
        // key, so `Mode::Thread` has no `SwapView` binding at all. The stack
        // would give the thread back if one were ever added, which is the
        // point of writing it as a stack rather than as two named modes.)
        let first_project = (0..a.vs.projects.selectable())
            .find(|i| matches!(a.vs.projects.item(*i), Some(RootItem::Project(_))))
            .expect("a project row");
        a.on_intent(Intent::Select(first_project));
        let i = right(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::List);
        let open = a.store.open_project().map(|p| p.name.clone());
        let i = press(&a, tui_render::Key::Tab);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Activity);
        let i = press(&a, tui_render::Key::Tab);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::List, "the event lane did not owe the list back");
        assert_eq!(a.store.open_project().map(|p| p.name.clone()), open);

        // **AND THE HANDLER GIVES BACK A THREAD, which the old clause could
        // not express at all** — it knew only `List` and `Projects`.
        //
        // MEASURED, and it is why this half is written: restoring the old
        // `if self.store.open_project().is_some()` clause leaves every
        // assertion ABOVE green. Once nothing enters a project behind his back
        // and `esc` leaves one on the way to the root, the flag the old clause
        // read is never wrong — so the rewrite is a structural simplification
        // whose BEHAVIOUR only shows on a surface the flag cannot name. That
        // surface is asserted here rather than left unprotected, which is the
        // inherited-green trap turned on my own work.
        //
        // `Intent::SwapView` is driven directly: `Mode::Thread` has no `Tab`
        // binding (the composer owns that key), so there is no keystroke to
        // press. The HANDLER is what is under test, and saying so is better
        // than pretending a key exists.
        let i = right(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
        a.on_intent(Intent::SwapView);
        assert_eq!(a.vs.mode, Mode::Activity);
        a.on_intent(Intent::SwapView);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Composer),
            "the event lane gave back a list it was not raised over"
        );
    }

    /// **THE RETRACTION, END TO END.** A click on a thread row MOVES THE
    /// CURSOR and stays on the list; `→` on the row the click picked is what
    /// descends. Two acts, in the order he asked for them.
    ///
    /// This test is the INVERSE of the one it replaces — which asserted that a
    /// click and `→` landed in the same state — and the inversion is the
    /// operator's ruling, not a regression: *"a click on a row moves the
    /// cursor to that row. It does not drill."*
    ///
    /// FORCED: point the `Via::Click` arm in `hit.rs` at `Intent::Open` (what
    /// it did before this round) and the first assertion goes red — the click
    /// descends into the thread again.
    #[tokio::test]
    async fn a_click_on_a_row_selects_it_and_right_is_what_opens_it() {
        let target = tui_render::Target::ThreadRow(1);

        let mut a = app();
        a.store.adopt_projects(vec![proj("harness", &["a", "b"])]);
        a.open_project(0);
        a.project();
        a.set_cursor(0);

        a.activate(target, Via::Click);
        assert_eq!(
            a.vs.mode,
            Mode::List,
            "a click DRILLED IN — it must only move the cursor"
        );
        assert_eq!(a.store.open_id(), None, "a click opened a thread");
        assert_eq!(
            a.list_cursor, 1,
            "a click did not move the cursor to the row"
        );

        // …and NOW `→` opens the row the click picked. The two-step grammar.
        let i = right(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
        assert_eq!(a.store.open_id(), Some("b"), "the wrong row was opened");

        // The resolution itself: ONE function, two gestures, and the
        // keyboard's target for that cursor IS the target a click on that row
        // produces. Asked of a SECOND face still standing on the list.
        let mut standing = app();
        standing
            .store
            .adopt_projects(vec![proj("harness", &["a", "b"])]);
        standing.open_project(0);
        standing.project();
        standing.set_cursor(1);
        assert_eq!(standing.keyboard_target(), target);
        assert_eq!(
            tui_render::activation(target, Mode::List, Via::Click),
            Some(Intent::Select(1))
        );
        assert_eq!(
            tui_render::activation(target, Mode::List, Via::Key),
            Some(Intent::Open)
        );
    }

    /// **EXPANSION IS AN ACTIVATION ON A ROW, AND SINCE `0051` IT REACHES ONLY
    /// THAT ROW** (`IS-31`, and his *"clicking on a message seems to change the
    /// number of events being displayed beneath it"*).
    ///
    /// Both gestures still go through the one arm; what changed is that the arm
    /// carries the index it was given instead of discarding it.
    ///
    /// FORCED: put the `_` back in `hit.rs`'s `Target::Block` arm so it yields
    /// `Intent::Expand` again, and the SIBLING assertion below goes red —
    /// which is the assertion that separates "open this" from "un-fold the
    /// transcript", and the one no earlier test made.
    #[tokio::test]
    async fn expanding_a_row_opens_that_row_and_leaves_its_siblings_alone() {
        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        expandable_rows(&mut a);
        let n = a.vs.blocks.len();
        assert!(n >= 3, "the fixture is wrong, not the code: {n}");
        let before: Vec<String> = a.vs.blocks.iter().map(|b| b.text().to_string()).collect();
        let target =
            a.vs.blocks
                .iter()
                .rposition(|b| b.is_expandable())
                .expect("nothing expandable — the fixture is wrong, not the code");
        assert_eq!(
            a.keyboard_target(),
            tui_render::Target::Block(target),
            "→ in a transcript must point at the newest expandable row"
        );
        assert!(a.opened.is_empty());
        // **THE KEY IS `^E` IN THE CHAT REGION NOW, and BOTH halves of that
        // sentence are the round's doing.** `opt-e` was a dead key on his
        // keyboard — macOS composes it into an accent — and its ctrl twin `^E`
        // is `Compose(RowEnd)` INSIDE the box, which is readline and is not
        // moving. So the act keeps its letter in the region where the folded
        // rows actually are and has no composer spelling at all (`0128`). The
        // ACT is unchanged and so is its path: it resolves to
        // `Intent::Activate`, which asks `hit::activation` what the target
        // means, exactly as a click does.
        //
        // **THE COST, STATED: `↑` out of the box first.** That is the region
        // move the chat's own grammar already requires for every other key it
        // owns, and the assertion below proves the key is dead in the box so a
        // successor does not re-mint it there.
        assert_eq!(
            press(&a, tui_render::Key::Ctrl('e')),
            Intent::Compose(tui_render::Edit::RowEnd),
            "^E in the BOX must stay readline's end-of-row"
        );
        a.on_intent(Intent::Region(Region::Chat));
        assert_eq!(
            a.keyboard_target(),
            tui_render::Target::Block(target),
            "the region move lost the transcript's target"
        );
        let i = press(&a, tui_render::Key::Ctrl('e'));
        assert_eq!(i, Intent::Activate);
        a.on_intent(i);
        assert_eq!(
            a.opened.len(),
            1,
            "^E on an expandable row did not open it"
        );
        assert!(!a.vs.expanded, "one row opened the WHOLE transcript");

        // **THE SIBLINGS DID NOT MOVE.** This is the assertion the old
        // whole-transcript toggle could never have passed, and the operator's
        // reported mystery seen from the inside: the row count is the same and
        // every other row's text is byte-identical.
        assert_eq!(
            a.vs.blocks.len(),
            n,
            "opening one row changed the row COUNT"
        );
        let after: Vec<String> = a.vs.blocks.iter().map(|b| b.text().to_string()).collect();
        let moved: Vec<usize> = (0..n).filter(|i| before[*i] != after[*i]).collect();
        assert_eq!(
            moved,
            vec![target],
            "rows other than the one he opened changed"
        );

        // **A CLICK ON THE ROW'S BODY NO LONGER TOUCHES IT** — the operator's
        // retraction, asserted where it bites: the row stays open, because the
        // gesture reached nothing at all.
        a.activate(tui_render::Target::Block(target), Via::Click);
        assert_eq!(
            a.opened.len(),
            1,
            "a click on the row's BODY acted — it must be a free miss now"
        );
        // A click on its EXPANDER closes it again, through the same arm the
        // keyboard used. That is the affordance his ruling kept.
        a.activate(tui_render::Target::Expander(target), Via::Click);
        assert!(
            a.opened.is_empty(),
            "the expander did not close the row it opened"
        );
        // …and a click on a DIFFERENT row's expander opens that one instead.
        a.activate(tui_render::Target::Expander(target), Via::Click);
        let other =
            a.vs.blocks
                .iter()
                .position(|b| b.is_expandable())
                .expect("no expandable row");
        if other != target {
            a.activate(tui_render::Target::Expander(other), Via::Click);
            assert_eq!(a.opened.len(), 2, "two rows can be open at once");
        }
        a.vs.expanded = false;
        // In the ACTIVITY lane, which has no composer, `→` is unchanged.
        assert_eq!(
            tui_render::intent(tui_render::Key::Right, Mode::Activity),
            Intent::Activate
        );
        // And the key he cut is still a letter rather than a command: in a
        // thread `x` types an `x`, and in the lane it does nothing.
        assert_eq!(
            tui_render::intent(tui_render::Key::Char('x'), Mode::Thread(Region::Composer)),
            Intent::Type('x')
        );
        assert_eq!(
            tui_render::intent(tui_render::Key::Char('x'), Mode::Activity),
            Intent::None
        );
    }

    /// Everything the face has painted, as one string. The frame is the only
    /// honest place to assert a GLYPH: a flag on a block says what the
    /// projection decided, not what he can see.
    fn painted(a: &App) -> String {
        tui_render::frame(&a.vs, Utc::now(), 160, 40)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// **THE EXPANDER SAYS WHETHER ITS ROW IS OPEN** — `▸` closed, `▾` open.
    ///
    /// Written END-TO-END on purpose, because the end is where the defect was.
    /// A render-layer test can only say that a `Block` carrying `open: true`
    /// draws `▾`; it cannot say that OPENING A ROW produces such a block, and
    /// the gap `0052` recorded was exactly that wiring — the open set lives on
    /// the shell (`App::opened`), the projection consumed it and reported only
    /// the resulting text, so no value anywhere on the `ViewState` knew. Both
    /// halves are asserted here: the shell's set changed, and the screen
    /// changed with it.
    ///
    /// **Why an unchanging mark is a face that lies quietly.** He clicks, the
    /// row opens, the gutter looks identical — so the only way to learn whether
    /// the gesture landed is to read the body and judge whether it grew, which
    /// on a small expansion is indistinguishable from a click that missed.
    ///
    /// FORCED: draw `EXPANDER` unconditionally in `render.rs`'s gutter arm
    /// (drop the `is_open()` match) and the `▾` assertions go red while EVERY
    /// OTHER assertion here stays green — which is the precise shape of the
    /// defect, since the row genuinely did open and only the mark was silent.
    #[tokio::test]
    async fn opening_a_row_turns_its_expander_from_closed_to_open() {
        let mut a = app();
        a.open_thread("t1".into());
        a.store.load_transcript("t1", Vec::new(), 1);
        expandable_rows(&mut a);
        let target =
            a.vs.blocks
                .iter()
                .rposition(|b| b.is_expandable())
                .expect("nothing expandable — the fixture is wrong, not the code");

        // CLOSED — the row says so, and so does the gutter.
        assert!(
            !a.vs.blocks[target].is_open(),
            "a row nobody opened reads open"
        );
        let closed = painted(&a);
        assert!(
            closed.contains('▸'),
            "an expandable row painted no closed mark:\n{closed}"
        );
        assert!(
            !closed.contains('▾'),
            "nothing is open, yet an open mark was painted:\n{closed}"
        );

        // THE REAL GESTURE — a click on the expander, through the same one arm
        // `^E` uses. Not a hand-set field.
        a.activate(tui_render::Target::Expander(target), Via::Click);
        assert_eq!(a.opened.len(), 1, "the click did not open the row");

        // OPEN — and this is the assertion `0052` could not have written.
        assert!(
            a.vs.blocks[target].is_open(),
            "the shell's open set never reached the row model"
        );
        let open = painted(&a);
        assert!(
            open.contains('▾'),
            "an OPEN row still draws the closed mark:\n{open}"
        );
        assert_ne!(closed, open, "opening the row changed nothing on screen");

        // …and it goes back, so the mark reports a STATE rather than a history.
        a.activate(tui_render::Target::Expander(target), Via::Click);
        assert!(a.opened.is_empty(), "the expander did not close the row");
        assert!(
            !a.vs.blocks[target].is_open(),
            "a closed row still reads open"
        );
        assert!(
            !painted(&a).contains('▾'),
            "the row closed and kept its open mark"
        );
    }

    /// **A MISS IS FREE.** Header, footer, composer, blank space: nothing
    /// happens, and nothing is said about it.
    #[tokio::test]
    async fn a_click_on_nothing_changes_nothing_and_says_nothing() {
        let mut a = app();
        a.store.adopt_projects(vec![proj("harness", &["a"])]);
        a.project();
        for t in [
            tui_render::Target::Header,
            tui_render::Target::Footer,
            tui_render::Target::Banner,
            tui_render::Target::Composer,
            tui_render::Target::Empty,
        ] {
            a.vs.notice = None;
            a.activate(t, Via::Click);
            assert_eq!(a.vs.mode, Mode::Projects, "{t:?} moved the face");
            assert!(a.vs.notice.is_none(), "{t:?} punished an imprecise click");
        }
        // **AND THE QUEUE BADGE IS A FREE MISS NOW TOO.** It used to be the
        // exception — `DIR-6` says the badge is visible at all times, so it was
        // reachable at all times — and round six closed it: raising the queue
        // view is ENTERING a surface. The door it lost is `u` / `^O`, which
        // is in every body mode and is asserted against `BINDINGS` in
        // `golden.rs`; here the KEY path is exercised so that this test cannot
        // pass on the badge being dead for the wrong reason.
        a.activate(tui_render::Target::QueueBadge, Via::Click);
        assert_eq!(a.vs.mode, Mode::Projects, "a click still raised the queue");
        a.activate(tui_render::Target::QueueBadge, Via::Key);
        assert_eq!(a.vs.mode, Mode::Queue, "the badge lost its keyboard door");
        // A click anywhere in a raised overlay dismisses it, exactly as the
        // overlay's existing any-key binding already does.
        a.activate(tui_render::Target::Overlay, Via::Click);
        assert_eq!(a.vs.mode, Mode::Projects);
    }

    /// The `↓ N new` chip is a click target, which is how `G` gives up its
    /// letter without giving up its job.
    #[tokio::test]
    async fn clicking_the_chip_re_arms_follow_mode() {
        let mut a = app();
        a.on_intent(Intent::SwapView);
        a.on_intent(Intent::ScrollUp);
        event(&mut a, "t1", 1, json!({"kind":"turn_ended","exit_code":0}));
        assert!(!a.vs.viewport.follow);
        assert_eq!(a.vs.viewport.new_since, 1);
        a.activate(tui_render::Target::NewChip, Via::Click);
        assert!(a.vs.viewport.follow, "the chip did not re-arm follow");
        assert_eq!(a.vs.viewport.new_since, 0);
    }

    // ── THE SHEET (§5) ──────────────────────────────────────────────────

    /// A root with one thread the sheet can be opened on.
    fn with_thread(app: &mut App) {
        app.store.adopt_projects(vec![proj("harness", &["t1"])]);
        app.project();
    }

    /// **THE SELECTABLE INDEX OF THE FIRST THREAD ROW ON THE ROOT.**
    ///
    /// Not `0` since `DEC-46`: a band's HEADER is a cursor stop now and sits
    /// above its rows. Derived through `ProjectList::item` — the same one
    /// resolution the renderer and the hit map read — so a test cannot hold a
    /// second opinion about where a row is.
    fn first_thread_row(app: &App) -> usize {
        (0..app.vs.projects.selectable())
            .find(|&i| {
                matches!(
                    app.vs.projects.item(i),
                    Some(RootItem::Pinned(_) | RootItem::Recent(_))
                )
            })
            .expect("the root has no thread row at all")
    }

    /// **OPEN THE OPTIONS FOR SELECTABLE ROW `sel`, THROUGH THE KEY TABLE.**
    ///
    /// The sheet's door moved from a clicked title to `o` in `0076` (a click
    /// on a name now opens the THREAD — his ask), so every test that needed a
    /// sheet up came through here rather than each one re-deciding how the
    /// door is spelled. The key is pressed through `tui_render::intent`, so a
    /// test that reached a sheet proves the BINDING exists and not just the
    /// intent handler.
    fn open_options(app: &mut App, sel: usize) {
        app.on_intent(Intent::Select(sel));
        let i = press(app, tui_render::Key::Char('o'));
        assert_eq!(i, Intent::Options, "`o` is not bound in {:?}", app.vs.mode);
        app.on_intent(i);
    }

    /// **CLICKING A THREAD'S NAME SELECTS ITS ROW; `→` OPENS THE THREAD; `o`
    /// OPENS ITS OPTIONS.**
    ///
    /// **THIS IS THE TEST THE ROUND-SIX REVERSAL HAD TO TURN AROUND**, and it
    /// is the one the task predicted would exist: a green test asserting the
    /// wrong thing. It was
    /// `a_click_on_a_threads_name_opens_it_and_o_opens_its_options`, and until
    /// now it asserted that `on_click` on column 4 of a thread row landed in
    /// `Mode::Thread(Region::Composer)`.
    ///
    /// **THE PAIR OF QUOTES, READ IN ORDER, IS THE WHOLE STORY.** Round four
    /// built the drill from:
    ///
    /// > *"currently, clicking on a thread opens its options, not the thread.
    /// > It should open the thread."*
    ///
    /// That sentence was a complaint that click reached the SHEET. It was
    /// resolved by making click DRILL, which is a different act rather than the
    /// absence of a wrong one — and he has now said so:
    ///
    /// > *"As I requested once before, clicking on a thread shouldn't take me
    /// > into it. Only right can do it. Clicking should only move pointers and
    /// > click buttons (which we don't even have yet - nothing in this app
    /// > counts as that)."*
    ///
    /// The parenthetical is what forecloses the affordance argument this test's
    /// old body rested on: a thread's name was called a button, and nothing in
    /// this app counts as one.
    ///
    /// **ALL THREE DOORS ARE ASSERTED HERE** because closing a route with no
    /// replacement deletes the room: the click SELECTS, `→` OPENS, and `o`
    /// still reaches the sheet.
    ///
    /// FORCED RED four ways: point the `Via::Click` arm back at
    /// `Intent::OpenAt`/`Intent::OpenSheet` (the click enters again — the
    /// defect); claim the whole row for the title (the marker column stops
    /// selecting); drop the `.claim(...)` (the name is no longer its own
    /// target, so `o`'s door and the click's door stop being the same span);
    /// and unbind `o` (the options door goes silent).
    #[tokio::test]
    async fn a_click_on_a_threads_name_selects_it_and_right_is_what_opens_it() {
        let mut app = app();
        with_thread(&mut app);
        app.lay(100, 24);
        // Find the drawn row of the first selectable thread.
        let sel = first_thread_row(&app);
        let b = tui_render::bands(app.state(), 100, 24);
        let row = (b.header + b.banner + app.vs.projects.drawn_row_of(sel)) as u16;

        // Column 0 is the marker: the plain row behaviour survives.
        assert_eq!(app.hits.hit(0, row), Target::RootRow(sel));
        assert_eq!(
            activation(Target::RootRow(sel), Mode::Projects, Via::Click),
            Some(Intent::Select(sel)),
            "a click off the name must still SELECT — his older ruling, narrowed"
        );
        // Column 4 is inside the name: the door.
        assert_eq!(app.hits.hit(4, row), Target::ThreadTitle(sel));

        // **A CLICK ON IT MOVES THE CURSOR AND STOPS.**
        app.on_click(4, row);
        assert_eq!(
            app.vs.mode,
            Mode::Projects,
            "clicking a thread's name took him into it"
        );
        assert_eq!(
            app.store.open_id(),
            None,
            "the click opened a thread on the store"
        );
        assert_eq!(
            app.project_cursor, sel,
            "the click did not move the cursor to the row it landed on"
        );

        // **AND `→` IS WHAT OPENS IT — the only way in.** Pressed through the
        // key table, so the binding is what is under test.
        let i = press(&app, tui_render::Key::Right);
        assert_eq!(i, Intent::Activate, "`→` is not bound at the root");
        app.on_intent(i);
        assert_eq!(
            app.vs.mode,
            Mode::Thread(Region::Composer),
            "`→` on the selected thread row did not open it"
        );
        assert_eq!(app.store.open_id(), Some("t1"));
        // And it came from the ROOT, so escape gives the root back.
        app.on_intent(Intent::Back);
        assert_eq!(app.vs.mode, Mode::Projects);

        // **THE OTHER HALF: `o`, on the row the cursor is on.** Pressed
        // through the key table, so the binding is what is under test and not
        // this test's idea of one.
        app.on_intent(Intent::Select(sel));
        let i = press(&app, tui_render::Key::Char('o'));
        assert_eq!(i, Intent::Options, "`o` is not bound at the root");
        app.on_intent(i);
        assert_eq!(app.vs.mode, Mode::Sheet, "`o` did not open the options");
        let sheet = app.vs.sheet.as_ref().expect("a sheet");
        assert_eq!(sheet.title, "thread t1");
        assert_eq!(sheet.rows.len(), 9, "the sheet's act set changed");

        // `←` closes it and hands back the surface it was opened from.
        app.on_intent(Intent::Back);
        assert_eq!(app.vs.mode, Mode::Projects);
        assert!(app.vs.sheet.is_none());
    }

    /// **AN UNAVAILABLE ROW PUTS ITS OWN SENTENCE IN THE NOTICE LINE AND SENDS
    /// NOTHING.**
    ///
    /// This `App` has no 9010 client at all, which is exactly what a stopped
    /// ui-server looks like from here. Every AUI-owned row is therefore muted,
    /// and activating one must be loud rather than silent — a row that is drawn
    /// and does nothing is the stub the doctrine names.
    ///
    /// FORCED RED by returning early from `sheet_act` without the notice (the
    /// notice assertion fails), and by dropping the `avail.reason()` guard so
    /// the act fires anyway (the mode/notice assertions fail and a mutation is
    /// attempted with no link).
    #[tokio::test]
    async fn activating_an_unreachable_row_says_why_once_and_sends_nothing() {
        let mut app = app();
        with_thread(&mut app);
        app.lay(100, 24);
        let sel = first_thread_row(&app);
        open_options(&mut app, sel);
        assert_eq!(app.vs.mode, Mode::Sheet);

        // Row 0 is `model`, which the AUI owns and this App has no client for.
        let row = app.vs.sheet.as_ref().expect("a sheet").rows[0].clone();
        assert_eq!(row.act, SheetAct::Model);
        let why = row
            .avail
            .reason()
            .expect("the model row is not muted")
            .to_string();
        assert_eq!(why, DownCause::DoorShut.words());

        app.on_intent(Intent::SheetAct(0));
        let notice = app.vs.notice.as_ref().expect("nothing was said");
        assert_eq!(
            notice.text, why,
            "the notice and the row say different things about the same refusal"
        );
        // The row STAYS SELECTED and the sheet stays up. The menu does not move
        // under him because an owner is down.
        assert_eq!(app.vs.mode, Mode::Sheet);
        assert_eq!(app.vs.sheet.as_ref().expect("a sheet").cursor, 0);
    }

    /// **THE FIELD-BEARING ROWS OPEN A FIELD INSTEAD OF FIRING**, and what is
    /// typed SURVIVES the field being closed.
    ///
    /// `0060`: *"compaction instructions already typed are KEPT in the field."*
    ///
    /// FORCED RED by clearing the buffer in the `Back` arm for `SheetField`.
    #[tokio::test]
    async fn the_compaction_field_keeps_what_is_typed_when_he_leaves_it() {
        let mut app = app();
        with_thread(&mut app);
        app.lay(100, 24);
        let sel = first_thread_row(&app);
        open_options(&mut app, sel);

        // `compact` is row 2, and it is DAEMON-owned — this App's transport is
        // pointed at a dead port, so it is muted for the daemon's reason, not
        // the AUI's. Force the daemon up so the field can open.
        app.store.link = LinkState::Connected;
        app.project();
        assert!(
            app.vs.sheet.as_ref().expect("a sheet").rows[2]
                .avail
                .is_ready(),
            "compact is not available with the daemon connected"
        );

        app.on_intent(Intent::SheetAct(2));
        assert_eq!(
            app.vs.mode,
            Mode::SheetField,
            "the compact row did not open a field"
        );
        for c in "drop the dead ends".chars() {
            app.on_intent(Intent::Type(c));
        }
        assert_eq!(
            app.sheet.as_ref().expect("state").compact.text(),
            "drop the dead ends"
        );

        // `esc` leaves the field. THE WORDS STAY.
        app.on_intent(Intent::Back);
        assert_eq!(app.vs.mode, Mode::Sheet, "esc closed the whole sheet");
        assert_eq!(
            app.sheet.as_ref().expect("state").compact.text(),
            "drop the dead ends",
            "leaving the field ate what he had typed"
        );
    }

    /// **THE RENAME FIELD STARTS FROM WHAT THE THREAD IS CALLED NOW**, because
    /// §5 says rename edits the summary IN PLACE. An empty box would be a
    /// different act, and an accidental enter would erase the name.
    #[tokio::test]
    async fn the_rename_field_is_prefilled_with_the_summary_it_edits() {
        let mut app = app();
        with_thread(&mut app);
        app.lay(100, 24);
        let sel = first_thread_row(&app);
        open_options(&mut app, sel);
        assert_eq!(
            app.sheet.as_ref().expect("state").rename.text(),
            "thread t1",
            "rename would have started from an empty box and erased the name"
        );
    }

    /// **AN `Invalidate` FOR AN INDEX NODE RE-READS THE INDEXES.**
    ///
    /// This is what makes a pin made on the phone land here without `^R`.
    ///
    /// FORCED RED by deleting the `self.store.load_projects()` call in
    /// `on_invalidate` — the projection count then never moves and the second
    /// assertion fails. Also forced by removing the node filter, which makes
    /// the `thread_options` half fail instead.
    ///
    /// # AN INHERITED GREEN THAT WAS NOT COVERAGE — AND WHY IT IS KEPT
    ///
    /// **Round six found that this test's last section was PROTECTING the
    /// defect it looks like it is protecting against.** It asserted that a
    /// second `Invalidate` inside the window changed nothing and stopped
    /// there, so "the debounce swallowed it and nothing will ever retry it"
    /// scored a pass — which is exactly the state that left his main menu
    /// showing the old name after a rename.
    ///
    /// It is KEPT rather than replaced, because the leading-edge coalescing it
    /// asserts is still correct and still worth defending: a `MovePin` really
    /// does ripple six nodes and six re-reads really are five re-reads of no
    /// new information. What it was missing is the other half of the contract,
    /// which is now the section below it: **the burst is still owed ONE read,
    /// and the clock that pays it is armed.** Half a contract asserted
    /// confidently is how this shipped.
    ///
    /// The end-to-end half — that the trailing read reaches the SCREEN and
    /// carries the renamed thread — is
    /// `tests/it/rows_registry_and_rename.rs::a_rename_that_lands_after_the_
    /// first_frame_still_reaches_the_main_menu`, because this test reads
    /// `app.store` and the store is not what he looks at.
    #[tokio::test]
    async fn an_invalidate_re_reads_the_indexes_once_per_burst_and_only_for_index_nodes() {
        let mut app = app();
        with_thread(&mut app);
        // The store is pointed at a `$HOME` with no `~/minds`, so a re-read is
        // observable as the adopted fixture being REPLACED by what is on disk.
        assert_eq!(app.store.projects().len(), 1);

        // A node no index holds: nothing happens, and nothing is re-read.
        app.apply(Ev::Aui(crate::aui::AuiEvent::Invalidate {
            node: "thread_options".into(),
        }));
        assert_eq!(
            app.store.projects().len(),
            1,
            "an invalidate for a node no index file holds cost a re-read"
        );

        // A node the index DOES hold: the files are re-read, and the fixture
        // is gone because `/Users/x` has no `~/minds`.
        app.apply(Ev::Aui(crate::aui::AuiEvent::Invalidate {
            node: "pinned".into(),
        }));
        assert_eq!(
            app.store.projects().len(),
            1,
            "a re-read of a home with no ~/minds still yields the miscellaneous bucket"
        );
        assert_eq!(
            app.store.projects()[0].name,
            "miscellaneous",
            "the indexes were not re-read — the adopted fixture is still there"
        );

        // **DEBOUNCED.** A `MovePin` ripples six nodes; six re-reads of thirteen
        // files is five re-reads of no new information.
        app.store.adopt_projects(vec![proj("harness", &["t1"])]);
        app.apply(Ev::Aui(crate::aui::AuiEvent::Invalidate {
            node: "root".into(),
        }));
        assert_eq!(
            app.store.projects()[0].name,
            "harness",
            "the debounce did not hold a second invalidate inside its window"
        );

        // **…AND THE HELD FRAME IS STILL OWED A READ.** This is the half that
        // was missing, and its absence is what made the section above read as
        // a proof of correctness instead of a proof of data loss.
        assert!(
            app.invalidate_deadline().is_some(),
            "a swallowed Invalidate left no debt — nothing will ever pay it, \
             which is his `after rename of the thread, the main menu wasn't \
             updated` exactly"
        );
        // The burst settles and the trailing read happens ONCE, off the clock
        // and with no keystroke. The fixture is replaced again, which is what
        // says a real re-read occurred rather than a no-op.
        app.apply(Ev::IndexSettled);
        assert_eq!(
            app.store.projects()[0].name,
            "miscellaneous",
            "the trailing read never happened — the burst was coalesced by \
             DROPPING, not by deferring"
        );
        // And it settles: the debt is paid, so the clock disarms rather than
        // re-arming itself into a loop that re-reads thirteen files forever.
        assert!(
            app.invalidate_deadline().is_none(),
            "the trailing read left its own debt behind"
        );
    }

    /// **THE SHEET OPENS FROM A PROJECT'S THREAD LIST TOO** — §5's "anywhere it
    /// appears" is one behaviour on two surfaces — **and the CLICK does not
    /// drill here either.** The reversal is the same one
    /// `a_click_on_a_threads_name_selects_it_and_right_is_what_opens_it`
    /// carries at the root; it is asserted on both surfaces because the door
    /// was built on both.
    #[tokio::test]
    async fn the_title_is_a_door_on_the_project_thread_list_as_well() {
        let mut app = app();
        with_thread(&mut app);
        // Into the PROJECT, not into a band row. At the root the cursor starts
        // on the recent band, so a bare `Open` would drill into the thread.
        app.open_project(0);
        app.project();
        assert_eq!(app.vs.mode, Mode::List);
        app.lay(100, 24);
        // FOUND, not assumed: a short thread list is padded at the top, so the
        // row is not the content band's first line.
        let row = (0..24u16)
            .find(|r| app.hits.hit(0, *r) == Target::ThreadRow(0))
            .expect("the thread row is not on the frame at all");
        assert_eq!(app.hits.hit(0, row), Target::ThreadRow(0));
        assert!(
            matches!(app.hits.hit(6, row), Target::ThreadTitle(0)),
            "the name is not a door in a project's thread list: {:?}",
            app.hits.hit(6, row)
        );
        // **A CLICK ON THE NAME SELECTS THE ROW, HERE TOO.**
        app.on_click(6, row);
        assert_eq!(
            app.vs.mode,
            Mode::List,
            "clicking a thread's name in a project list took him into it"
        );
        assert_eq!(app.store.open_id(), None);
        assert_eq!(app.list_cursor, 0, "the click did not move the cursor");
        // …and `→` opens it, owing the LIST back and not the root — he drilled
        // through the project to get here.
        app.on_intent(Intent::Activate);
        assert_eq!(app.vs.mode, Mode::Thread(Region::Composer));
        assert_eq!(app.store.open_id(), Some("t1"));
        app.on_intent(Intent::Back);
        assert_eq!(app.vs.mode, Mode::List);

        // **AND `o` OPENS THE OPTIONS FROM THE LIST**, owing the list back.
        open_options(&mut app, 0);
        assert_eq!(app.vs.mode, Mode::Sheet, "`o` did not open the options");
        app.on_intent(Intent::Back);
        assert_eq!(app.vs.mode, Mode::List);
    }

    // ────────────────────────────────────────────────────────────────────
    //  THE PROJECT SHEET, THE ARCHIVED ROOM AND `+ new project`
    // ────────────────────────────────────────────────────────────────────

    /// A root with an ordinary project and the un-projected bucket beside it.
    fn with_two_projects(app: &mut App) {
        app.store.adopt_projects(vec![
            proj("harness", &["t1", "t2"]),
            proj("miscellaneous", &["t3"]),
        ]);
        app.project();
    }

    /// The root row index whose NAME span is the door onto `name`'s sheet.
    ///
    /// FOUND on the frame rather than assumed: the root's bands push the
    /// project rows down by an amount that depends on what is pinned, so a
    /// hard-coded index would be asserting against this test's own arithmetic.
    fn name_door(app: &mut App, name: &str) -> (u16, u16) {
        app.lay(100, 24);
        for r in 0..24u16 {
            for c in 0..100u16 {
                if let Target::ProjectName(i) = app.hits.hit(c, r) {
                    if app.vs.projects.item(i)
                        == Some(RootItem::Project(
                            app.vs
                                .projects
                                .rows
                                .iter()
                                .position(|x| x.name == name)
                                .expect("a row"),
                        ))
                    {
                        return (c, r);
                    }
                }
            }
        }
        panic!("no name door for {name} is on the frame at all");
    }

    /// **THE PROJECT SHEET'S KEYBOARD DOOR — the cursor onto the project row,
    /// then `o`.**
    ///
    /// It is the door round six minted, and it is not a new key: `o` is
    /// `Intent::Options`, which already opened a THREAD's sheet and now
    /// resolves a PROJECT row as well. Every test that used to reach this sheet
    /// with a CLICK goes through here, which is what makes the click's removal
    /// a route change rather than a lost room.
    fn open_project_sheet_by_key(app: &mut App, name: &str) {
        let n = app
            .vs
            .projects
            .rows
            .iter()
            .position(|x| x.name == name)
            .expect("a row");
        cursor_to(app, RootItem::Project(n));
        let i = press(app, tui_render::Key::Char('o'));
        assert_eq!(i, Intent::Options, "`o` is not bound at the root");
        app.on_intent(i);
    }

    /// **THE PROJECT SHEET OPENS FROM THE NAME ON THE ROOT, AND SAYS WHAT THE
    /// PROJECT IS.**
    ///
    /// §5's second frame: the facts line is computed from what the root already
    /// loaded — no new read, and nothing sourced from 9010 — and the three acts
    /// below it are the ones `0060` bound to live `Mutation` variants.
    ///
    /// # AND SINCE ROUND SIX THE DOOR IS A KEY, WHICH IS THE OTHER HALF
    ///
    /// `0127` closed every click route but this one, because the project sheet
    /// had no keyboard door at all and taking the click would have deleted the
    /// ROOM. `o` is that door now — `Intent::Options`, the same act that opens a
    /// THREAD's sheet, resolving one more row type — and so this test asserts
    /// BOTH halves: a click on the name now only MOVES THE CURSOR, and `o`
    /// opens the sheet.
    ///
    /// FORCED RED by dropping the `.claim(name.0, name.1, ProjectName(sel))` on
    /// the project row (no door is on the frame at all), by pointing
    /// `Intent::OpenProjectSheet` at the thread sheet (the act set fails), and
    /// — for the key half — by taking the `on_project` branch out of
    /// `Intent::Options`, which leaves `app.vs.sheet` `None`.
    #[tokio::test]
    async fn the_project_sheet_opens_from_the_name_on_the_root_and_states_its_facts() {
        let mut app = app();
        with_two_projects(&mut app);
        let (c, r) = name_door(&mut app, "harness");

        // The row's other columns keep the plain behaviour the operator's
        // retraction gave them — this is the same two-behaviours-one-row shape
        // the thread title has, and it is asserted here so a later widening of
        // the span is caught on the PROJECT row too.
        assert!(matches!(app.hits.hit(0, r), Target::RootRow(_)));

        // ── THE CLICK NO LONGER ENTERS. It selects the row it landed on, which
        //    is the whole of the law `0127` built and this round finished.
        app.on_click(c, r);
        assert_eq!(
            app.vs.mode,
            Mode::Projects,
            "a click on a project's name still opens its sheet"
        );
        assert!(
            app.vs.sheet.is_none(),
            "a click on a project's name still built a sheet"
        );

        // ── AND THE KEY DOES. One act, one more row type, no new key.
        open_project_sheet_by_key(&mut app, "harness");
        assert_eq!(app.vs.mode, Mode::Sheet);
        let v = app.vs.sheet.as_ref().expect("a project sheet");
        assert_eq!(v.title, "harness");
        let tui_render::model::Facts::Project(f) = &v.facts else {
            panic!("the project sheet drew a THREAD's facts: {:?}", v.facts)
        };
        assert_eq!(f.name, "harness");
        assert_eq!(f.threads, 2, "the facts line counts what the root loaded");
        assert!(!f.bucket);
        assert_eq!(
            v.rows.iter().map(|x| x.act).collect::<Vec<_>>(),
            vec![SheetAct::ProjectModel, SheetAct::Archive, SheetAct::Remove],
            "the project sheet is not §5's three acts"
        );
    }

    // ────────────────────────────────────────────────────────────────────
    //  ROUND FIVE — THE ACTS THAT CHANGE WHAT A PROJECT IS
    // ────────────────────────────────────────────────────────────────────

    /// An `App` over a SCRATCH `$HOME` with a real registry in it, and the
    /// channel its acts report back on.
    ///
    /// **His real `~/.cauldron` is never what a test opens.** `home` is a
    /// parameter through `App`, `Store` and every `cauldron` call, and this
    /// helper is the only place a test names one.
    fn app_at(home: &std::path::Path, names: &[&str]) -> (App, mpsc::Receiver<Ev>) {
        std::fs::create_dir_all(projects::cauldron_dir(home)).expect("mkdir");
        let mut reg = projects::Registry::empty();
        for n in names {
            let d = home.join("code").join(n);
            std::fs::create_dir_all(&d).expect("mkdir");
            reg.register(&d, Some(n)).expect("registered");
        }
        reg.save(home).expect("saved");
        // **A SEAM POINTED AT PORT 1, WHICH NOTHING LISTENS ON.**
        // `mirror::spawn_at` and not `mirror::spawn`, and the difference is the
        // operator's money: the bare `spawn` takes its port from the
        // environment and falls back to 3653, which is his LIVE daemon. See
        // `mirror::TestPorts`. `spawn_dead` needs no guard held: it only ever
        // writes the same two values, so two callers agree.
        let (handle, _rx) = mirror::spawn_dead();
        let (tx, erx) = mpsc::channel::<Ev>(16);
        let mut a = App::new(
            handle,
            tx,
            "ws://127.0.0.1:1/ws".into(),
            home.to_string_lossy().to_string(),
            0,
        );
        a.store.load_projects();
        a.project();
        (a, erx)
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("tui-acts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("mkdir");
        p
    }

    /// Drain one `ActDone` and apply it, as the loop would.
    async fn settle(a: &mut App, erx: &mut mpsc::Receiver<Ev>) {
        let ev = tokio::time::timeout(Duration::from_secs(5), erx.recv())
            .await
            .expect("the act never reported back")
            .expect("the channel closed");
        a.apply(ev);
    }

    /// **`p` IS TWO ACTS AND THE CURSOR CHOOSES**, which is the whole of the
    /// ruled collision: *"`p` on a thread row pins; `p` not on a thread adds a
    /// project."*
    ///
    /// N >= 2 on the thing that arbitrates — the resolution — with BOTH arms
    /// exercised on ONE surface, because a test that only pressed `p` on a
    /// project row would pass just as well against a `p` that never pins.
    ///
    /// FORCED RED two ways, each watched: make `add_project_or_pin` always
    /// open the field (the pinned row stops reaching the socket), and make it
    /// always pin (the project row opens no field and `Mode` stays
    /// `Projects`).
    #[tokio::test]
    async fn p_pins_on_a_thread_row_and_opens_the_add_field_on_every_other_row() {
        let mut a = app();
        let (h, mut out) = crate::aui::test_handle_up();
        a.set_aui(h);
        a.store
            .adopt_projects(vec![pinned_proj("harness", &[("p1", 0)])]);
        a.project();

        // THE KEY COMES FROM THE TABLE, never hand-written.
        let i = tui_render::intent(tui_render::Key::Char('p'), Mode::Projects);
        assert_eq!(i, Intent::AddProject, "`p` is not bound at the root");

        // ── ARM ONE: a thread row PINS, through the sheet's own pin act ──
        cursor_to(&mut a, RootItem::Pinned(0));
        a.on_intent(i);
        tokio::task::yield_now().await;
        let job = out.try_recv().expect("`p` on a pinned row sent nothing");
        assert_eq!(
            job.kind(),
            "set_thread_pinned",
            "`p` on a thread row sent something other than a pin"
        );
        assert!(out.try_recv().is_err(), "one press sent two mutations");
        a.on_intent(Intent::Back);

        // ── ARM TWO: a project row OPENS THE FIELD and sends nothing ─────
        cursor_to(&mut a, RootItem::Project(0));
        a.on_intent(i);
        tokio::task::yield_now().await;
        assert_eq!(
            a.vs.mode,
            Mode::NewProject,
            "`p` off a thread row did not open the add-a-project field"
        );
        assert!(
            out.try_recv().is_err(),
            "`p` on a project row put a mutation on the wire"
        );

        // ── AND A BAND HEADER IS ALSO "not a thread" ────────────────────
        a.on_intent(Intent::Back);
        a.vs.mode = Mode::Projects;
        a.new_project = None;
        cursor_to(&mut a, RootItem::Band(tui_render::BandId::Projects));
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::NewProject);
    }

    /// **`c` ASKS FIRST, AND IT IS THE SHEET'S OWN ARM AND NOT A SECOND
    /// CONFIRMATION GRAMMAR.**
    ///
    /// `0099`: *"Reuse the confirmation machinery that already exists; two
    /// confirmation grammars on one face is how they drift apart."* This
    /// asserts the reuse rather than the behaviour: the first `c` leaves the
    /// sheet's `remove` row ARMED and wearing the sheet's own *"press again"*
    /// words, which only `SheetState::armed` can produce.
    ///
    /// **Both spellings of the second press are proven** — a second `c` and an
    /// `enter` — because the design's claim is that `enter` works for free by
    /// being the sheet's ordinary activation of the selected row, and a claim
    /// that gets something for free is exactly the one that is never checked.
    ///
    /// FORCED RED three ways, each watched: remove the `if !was_armed` guard
    /// (the FIRST `c` reaches the wire); make `deregister_project` fire
    /// `ProjectAct::Remove` directly (the row never says *"press again"*); and
    /// point `press_sheet_row` at `SheetAct::Archive` (the kind comes back
    /// `archive_project`).
    #[tokio::test]
    async fn c_asks_first_on_a_project_row_and_either_second_press_de_registers() {
        for second in ["c", "enter"] {
            let mut a = app();
            let (h, mut out) = crate::aui::test_handle_up();
            a.set_aui(h);
            with_two_projects(&mut a);

            let c_key = tui_render::intent(tui_render::Key::Char('c'), Mode::Projects);
            assert_eq!(c_key, Intent::DeregisterProject, "`c` is not bound at the root");

            cursor_to(&mut a, RootItem::Project(0));
            a.on_intent(c_key);
            tokio::task::yield_now().await;

            // NOTHING LEFT THE PROCESS, and the row SAYS it is waiting.
            assert!(
                out.try_recv().is_err(),
                "the FIRST `c` put a de-registration on the wire"
            );
            let v = a.vs.sheet.as_ref().expect("`c` opened no project sheet");
            let i = store::sheet_row_for(v, SheetAct::Remove).expect("no remove row");
            assert!(
                v.rows[i].words().contains("press again"),
                "the row does not say it is armed — this is not the sheet's own arm: {}",
                v.rows[i].words()
            );

            // THE SECOND PRESS, in both of its spellings.
            match second {
                "c" => a.on_intent(c_key),
                _ => a.on_intent(tui_render::intent(tui_render::Key::Enter, Mode::Sheet)),
            }
            let job = tokio::time::timeout(Duration::from_secs(5), out.recv())
                .await
                .unwrap_or_else(|_| panic!("the second press ({second}) never reached the wire"))
                .expect("the channel closed");
            assert_eq!(
                job.kind(),
                "remove_project",
                "the second press ({second}) sent something else"
            );
        }
    }

    /// **`ctrl-↑` ON A PROJECT ROW REWRITES `~/.cauldron/projects.json` AND
    /// THE CURSOR TRAVELS WITH THE ROW.**
    ///
    /// The end-to-end proof of the third owner: the key, the pure route, the
    /// locked write, the re-read, and the anchor — against a scratch `$HOME`
    /// with a real registry in it and nothing live anywhere.
    ///
    /// **It also proves the face does NOT reorder optimistically**: between
    /// the press and the ack the drawn order is still the file's old one,
    /// which is the property that keeps a failed act from leaving the screen
    /// lying. Same rule as `MovePin`.
    ///
    /// FORCED RED three ways, each watched: drop the
    /// `Clears::RegistryWrite` re-read (the file moves and the face keeps
    /// drawing the old order); drop `self.project_follow` (the cursor stays on
    /// the row number and lands on the displaced project); and reorder
    /// `vs.projects.rows` in `move_project` before firing (the mid-flight
    /// assertion fails, which is the optimistic-reorder defect).
    #[tokio::test]
    async fn ctrl_up_on_a_project_row_rewrites_the_registry_and_the_cursor_follows_by_name() {
        let home = scratch("reorder");
        let (mut a, mut erx) = app_at(&home, &["alpha", "bravo", "charlie"]);

        // WITNESS, keyed on something no write can move: the face is drawing
        // this scratch registry's order at all.
        let drawn: Vec<String> = a.vs.projects.rows.iter().map(|r| r.name.clone()).collect();
        assert_eq!(
            drawn,
            vec!["alpha", "bravo", "charlie", projects::BUCKET],
            "THE WITNESS FAILED: the face is not reading the scratch registry"
        );

        let up = tui_render::intent(tui_render::Key::CtrlUp, Mode::Projects);
        assert_eq!(up, Intent::MovePinUp, "ctrl-↑ is not bound at the root");
        cursor_to(&mut a, RootItem::Project(2)); // charlie
        a.on_intent(up);

        // MID-FLIGHT: nothing has moved on screen yet.
        let mid: Vec<String> = a.vs.projects.rows.iter().map(|r| r.name.clone()).collect();
        assert_eq!(
            mid, drawn,
            "the face reordered the band behind the file's back"
        );
        assert_eq!(a.project_follow.as_deref(), Some("charlie"));

        settle(&mut a, &mut erx).await;
        a.project();

        // THE FILE MOVED …
        let names: Vec<String> = projects::Registry::load(&home)
            .expect("loads")
            .expect("present")
            .names()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(names, vec!["alpha", "charlie", "bravo"]);
        // … THE FACE FOLLOWED …
        let after: Vec<String> = a.vs.projects.rows.iter().map(|r| r.name.clone()).collect();
        assert_eq!(after, vec!["alpha", "charlie", "bravo", projects::BUCKET]);
        // … AND THE CURSOR IS STILL ON `charlie`, one row up.
        assert_eq!(
            a.vs.projects.item(a.project_cursor),
            Some(RootItem::Project(1)),
            "the cursor did not travel with the row"
        );
        assert_eq!(
            a.vs.projects.rows[1].name, "charlie",
            "the cursor followed the INDEX and landed on the displaced project"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **ONE FIELD, TWO OWNERS, AND THE ARGUMENT CHOOSES.**
    ///
    /// A bare NAME is `Mutation::CreateProject` on 9010 (the AUI makes the
    /// directory); a PATH is a locked registry write and never touches the
    /// socket. N >= 2 on the arbitration, both arms in one test, because
    /// either alone would pass against a face that only ever did the other.
    ///
    /// FORCED RED by deleting the `looks_like_a_path` branch in
    /// `create_project` (the path is sent to 9010 as a project NAME, which the
    /// AUI would normalize into `users-x-code-newthing` — a directory he never
    /// asked for), and by making `looks_like_a_path` always true (the bare
    /// name never reaches the socket).
    #[tokio::test]
    async fn a_path_in_the_add_field_registers_and_a_bare_name_goes_to_the_aui() {
        let home = scratch("addfield");
        let (mut a, mut erx) = app_at(&home, &["alpha"]);
        let (h, mut out) = crate::aui::test_handle_up();
        a.set_aui(h);
        let target = home.join("code").join("brought-in");
        std::fs::create_dir_all(&target).expect("mkdir");

        // ── ARM ONE: A PATH. The registry gains a row; 9010 hears nothing.
        a.on_intent(Intent::AddProject);
        assert_eq!(a.vs.mode, Mode::NewProject);
        a.new_project = Some(tui_render::compose::Composer::from_text(
            &target.to_string_lossy(),
        ));
        a.on_intent(Intent::CreateProject);
        settle(&mut a, &mut erx).await;
        assert!(
            out.try_recv().is_err(),
            "a PATH was relayed to the audio interface as a project name"
        );
        let names: Vec<String> = projects::Registry::load(&home)
            .expect("loads")
            .expect("present")
            .names()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(names, vec!["alpha", "brought-in"]);
        // …and only an ACCEPTED add empties the field.
        assert!(a.new_project.is_none(), "the accepted add left the field full");

        // ── ARM TWO: A BARE NAME. 9010 owns it; the file is untouched.
        a.on_intent(Intent::AddProject);
        a.new_project = Some(tui_render::compose::Composer::from_text("freshone"));
        a.on_intent(Intent::CreateProject);
        let job = tokio::time::timeout(Duration::from_secs(5), out.recv())
            .await
            .expect("a bare name never reached the socket")
            .expect("the channel closed");
        assert_eq!(job.kind(), "create_project");
        let after: Vec<String> = projects::Registry::load(&home)
            .expect("loads")
            .expect("present")
            .names()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            after, names,
            "a bare name was written into the registry by this face"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **A PATH REGISTERS WITH THE AUDIO INTERFACE STOPPED, AND A NAME SAYS
    /// WHY IT CANNOT.**
    ///
    /// The gate this round moved from the door to the act. `ui-server` down is
    /// exactly when registering a project matters most — the registry file is
    /// what it reads when it comes back — and it was the state in which the
    /// field refused to open at all.
    ///
    /// **WITNESS, keyed on something no gate can move**: the owner really is
    /// down in this `App` (it has no 9010 client at all), asserted before
    /// anything else. Without it, a face whose AUI happened to be up would
    /// score a triumphant pass on both halves.
    ///
    /// FORCED RED by putting the `owners().aui_down` refusal back at the top
    /// of `open_new_project` (the field never opens and the mode assertion
    /// fails).
    #[tokio::test]
    async fn a_path_registers_with_the_owner_down_and_a_name_says_why() {
        let home = scratch("down");
        let (mut a, mut erx) = app_at(&home, &["alpha"]);
        // No `set_aui` — this is what a stopped `ui-server` looks like here.
        assert!(a.owners().aui_down.is_some(), "THE WITNESS FAILED: the owner is up");

        let target = home.join("code").join("offline");
        std::fs::create_dir_all(&target).expect("mkdir");
        a.on_intent(Intent::AddProject);
        assert_eq!(
            a.vs.mode,
            Mode::NewProject,
            "the field refused to open with the owner down — the path act needs no socket"
        );
        a.new_project = Some(tui_render::compose::Composer::from_text(
            &target.to_string_lossy(),
        ));
        a.on_intent(Intent::CreateProject);
        settle(&mut a, &mut erx).await;
        let names: Vec<String> = projects::Registry::load(&home)
            .expect("loads")
            .expect("present")
            .names()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(names, vec!["alpha", "offline"]);

        // …and a NAME is refused in the AUI's own down-words, with his typing
        // still in the buffer to try again with.
        a.on_intent(Intent::AddProject);
        a.new_project = Some(tui_render::compose::Composer::from_text("needsaui"));
        a.on_intent(Intent::CreateProject);
        assert!(!notice(&a).is_empty(), "a name committed with the owner down said nothing");
        assert_eq!(
            a.new_project.as_ref().expect("the field was emptied").text(),
            "needsaui",
            "a refused name ate his typing"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **`tab` COMPLETES A PATH AND LEAVES A NAME ALONE.**
    ///
    /// The second half is the one worth asserting: `tab` must be inert on a
    /// bare word, or a half-typed project name would be silently rewritten
    /// into whatever directory happened to match.
    ///
    /// **THE FIRST DRAFT OF THIS TEST DID NOT BITE, AND THAT IS WHY IT SAYS
    /// WHAT IT SAYS NOW.** Dropping the `looks_like_a_path` guard in
    /// `App::complete_path` left it GREEN: `index::complete_path` already
    /// refuses a string with no separator in it, so the FIELD is not rewritten
    /// either way and an assertion about the field could not see the guard at
    /// all. What the guard actually buys is the NOTICE — without it a
    /// half-typed project name answers *"nothing there to add"*, which is a
    /// sentence about a path, about a thing he is not typing. So the test
    /// asserts the notice, with a WITNESS that the notice line works.
    ///
    /// FORCED RED three ways, each watched: drop the guard (the bare name now
    /// carries the path sentence); drop the `Composer::from_text` write-back
    /// (the field never changes); and have `index::complete_path` return its
    /// first match instead of the common prefix (the unique case still passes,
    /// which is why `index`'s own test covers that half and this one does
    /// not).
    #[tokio::test]
    async fn tab_completes_a_path_in_the_field_and_is_inert_on_a_name() {
        let home = scratch("tab");
        let (mut a, _erx) = app_at(&home, &["alpha"]);
        std::fs::create_dir_all(home.join("code").join("alphabet")).expect("mkdir");

        let tab = tui_render::intent(tui_render::Key::Tab, Mode::NewProject);
        assert_eq!(tab, Intent::CompletePath, "`tab` is not bound in the field");

        a.on_intent(Intent::AddProject);
        a.new_project = Some(tui_render::compose::Composer::from_text(&format!(
            "{}/code/alphab",
            home.to_string_lossy()
        )));
        a.on_intent(tab);
        assert_eq!(
            a.new_project.as_ref().expect("a field").text(),
            format!("{}/code/alphabet/", home.to_string_lossy()),
            "tab did not complete a unique directory"
        );

        // WITNESS, keyed on something the guard cannot move: a PATH that
        // matches nothing does say so. Without this, a face whose notice line
        // never renders would score a triumphant pass below.
        a.vs.notice = None;
        a.new_project = Some(tui_render::compose::Composer::from_text(&format!(
            "{}/code/zzz",
            home.to_string_lossy()
        )));
        a.on_intent(tab);
        assert_eq!(
            notice(&a),
            "nothing there to add",
            "THE WITNESS FAILED: an unmatchable PATH said nothing"
        );

        // A BARE NAME IS NOT A PATH: `tab` does not touch the field AND does
        // not answer with a sentence about paths.
        a.vs.notice = None;
        a.new_project = Some(tui_render::compose::Composer::from_text("alp"));
        a.on_intent(tab);
        assert_eq!(
            a.new_project.as_ref().expect("a field").text(),
            "alp",
            "tab rewrote a project NAME into a directory"
        );
        assert_eq!(
            notice(&a),
            "",
            "a half-typed project NAME was answered with a sentence about paths"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **`miscellaneous` GETS A TWO-ROW SHEET, AND `archive`/`remove` ARE
    /// ABSENT RATHER THAN GREYED.**
    ///
    /// The distinction this whole round turns on. Everywhere else an act that
    /// cannot be performed KEEPS ITS ROW and wears its reason, because the
    /// reason will lift. The bucket's will not: it is the un-projected home,
    /// it is not under `~/minds`, and a GREYED `archive` here would promise a
    /// temporary outage that can never end. It is a different KIND of thing,
    /// not the same menu at a different time.
    ///
    /// **WITNESS, keyed on something the absence cannot move**: the ORDINARY
    /// project's sheet is opened in the same test, under the identical owner
    /// state, and it HAS all three rows. Without that, a `project_sheet` that
    /// returned two rows for everything — or a view that failed to build at all
    /// — would score a triumphant pass on an assertion about a missing row.
    ///
    /// FORCED RED by drawing the two rows greyed for the bucket instead of
    /// omitting them (`if !bucket` -> always push, with `Avail::Unavailable`),
    /// which is the exact failure the design names.
    #[tokio::test]
    async fn the_buckets_sheet_is_two_rows_and_archive_and_remove_are_absent_not_greyed() {
        let mut app = app();
        with_two_projects(&mut app);

        // THE WITNESS FIRST.
        open_project_sheet_by_key(&mut app, "harness");
        let ordinary: Vec<SheetAct> = app
            .vs
            .sheet
            .as_ref()
            .expect("the witness sheet")
            .rows
            .iter()
            .map(|x| x.act)
            .collect();
        assert!(
            ordinary.contains(&SheetAct::Archive) && ordinary.contains(&SheetAct::Remove),
            "THE WITNESS FAILED: an ordinary project's sheet is missing the rows too, \
             so their absence on the bucket proves nothing: {ordinary:?}"
        );
        app.on_intent(Intent::Back);

        open_project_sheet_by_key(&mut app, "miscellaneous");
        let v = app.vs.sheet.as_ref().expect("the bucket's sheet");
        let acts: Vec<SheetAct> = v.rows.iter().map(|x| x.act).collect();
        assert_eq!(
            acts,
            vec![SheetAct::ProjectModel],
            "the bucket's sheet is not the model row alone"
        );
        // ABSENT, not unavailable — asserted over EVERY row rather than over a
        // count, so a greyed row added anywhere in the sheet is caught.
        assert!(
            !v.rows
                .iter()
                .any(|x| matches!(x.act, SheetAct::Archive | SheetAct::Remove)),
            "the bucket drew a row for an act that can never lift: {:#?}",
            v.rows
        );
        // …and it says why IN WORDS on the facts line, rather than leaving him
        // to interpret a silence.
        let tui_render::model::Facts::Project(f) = &v.facts else {
            panic!("not a project's facts")
        };
        assert!(
            f.bucket,
            "the bucket's facts line does not admit what it is"
        );
    }

    /// **`remove` ASKS FIRST, AND THE FIRST PRESS PUTS NOTHING ON THE WIRE.**
    ///
    /// §5's `asks first`, on the most destructive act this face can perform:
    /// `RemoveProject` takes a project off his root (it de-registers; it
    /// stopped unlinking `~/minds/<name>` at the registry cutover). The arm is
    /// DRAWN rather than modal, so the key grammar does not change under him
    /// and the `?` overlay stays true.
    ///
    /// The AUI is REACHABLE here — `aui::test_handle_up` — because a muted row
    /// never reaches the arming branch at all, so proving arming against a down
    /// owner would prove nothing. The test holds the outbound channel, so the
    /// first press's silence is measured one step before the bytes.
    ///
    /// FORCED RED by removing the `if !was_armed` guard (the first press
    /// enqueues), and by clearing `armed` before it is read (the second press
    /// only re-arms and the channel stays empty).
    #[tokio::test]
    async fn remove_asks_first_and_the_first_press_enqueues_nothing() {
        let mut app = app();
        let (h, mut out) = crate::aui::test_handle_up();
        app.set_aui(h);
        with_two_projects(&mut app);
        open_project_sheet_by_key(&mut app, "harness");

        let i = app
            .vs
            .sheet
            .as_ref()
            .expect("a sheet")
            .rows
            .iter()
            .position(|x| x.act == SheetAct::Remove)
            .expect("no remove row");
        // The row must be READY, or this test is about the muted branch.
        assert!(
            app.vs.sheet.as_ref().expect("a sheet").rows[i]
                .avail
                .is_ready(),
            "the remove row is muted; the arming branch is never reached"
        );

        app.on_intent(Intent::SheetAct(i));
        assert!(
            out.try_recv().is_err(),
            "the FIRST press on `remove` put a mutation on the wire"
        );
        // The row SAYS it is armed — the second press is something he reads
        // before he makes it.
        let words = app.vs.sheet.as_ref().expect("a sheet").rows[i].words();
        assert!(
            words.contains("press again"),
            "the armed row does not say it is armed: {words}"
        );

        // THE POSITIVE HALF: the second press does reach the wire, which is
        // what makes the first press's silence evidence rather than an absence.
        app.on_intent(Intent::SheetAct(i));
        let job = tokio::time::timeout(Duration::from_secs(5), out.recv())
            .await
            .expect("the second press never reached the outbound channel")
            .expect("the channel closed");
        assert_eq!(
            job.kind(),
            "remove_project",
            "the second press sent something other than the remove"
        );

        // …and any OTHER press disarms, so a confirm cannot survive him
        // aiming somewhere else.
        app.on_intent(Intent::SheetAct(i));
        app.on_intent(Intent::SheetAct(0));
        let words = app.vs.sheet.as_ref().expect("a sheet").rows[i].words();
        assert!(
            !words.contains("press again"),
            "an armed `remove` survived a press on another row: {words}"
        );
    }

    /// **THE AUI'S REFUSAL REACHES THE NOTICE LINE WITH ITS SENTENCE INTACT.**
    ///
    /// The sharpest case in the arc: `remove` on a project that is a REAL
    /// DIRECTORY rather than a symlink. The v2 handler's refusal
    /// (`apps/aui/src/v2/mutate.rs:405-424`) explains a rule this face does not
    /// know — that `~/minds` holds links, and that a row he can press must not
    /// be able to delete a repository. Paraphrasing it would put this face
    /// between him and the only party that knows what went wrong.
    ///
    /// The face names the ACT in front of the sentence (`remove harness: …`)
    /// so the notice says what it is about; the AUI's words are carried
    /// contiguously and unaltered, which is what "verbatim" has to mean on a
    /// line that also has to identify itself.
    ///
    /// FORCED RED by paraphrasing the refusal in the `Err` arm, and by
    /// truncating it to a width.
    #[tokio::test]
    async fn the_v2_refusal_reaches_the_notice_line_with_its_sentence_intact() {
        let mut app = app();
        let sentence = "harness is a real directory here, not a link — \
                        removing it is not something a menu row may do";

        // WITNESS, keyed on something no refusal can move: the notice line
        // carries an ORDINARY sentence first. An assertion about a substring is
        // otherwise satisfied by a face whose notice line never renders.
        app.apply(Ev::ActDone {
            what: "archive harness".into(),
            result: Ok(()),
            clears: None,
        });
        assert_eq!(
            app.vs.notice.as_ref().expect("the witness notice").text,
            "archive harness — done",
            "THE WITNESS FAILED: the notice line does not carry an ordinary sentence"
        );

        app.apply(Ev::ActDone {
            what: "remove harness".into(),
            result: Err(sentence.to_string()),
            clears: None,
        });
        let n = app.vs.notice.as_ref().expect("the refusal was swallowed");
        assert!(
            n.text.contains(sentence),
            "the AUI's refusal was re-worded on its way to the notice line:\n{}",
            n.text
        );
        assert_eq!(
            n.severity,
            Severity::Critical,
            "a refusal that destroyed nothing is not reported as a refusal"
        );
    }

    /// **ONLY AN ACCEPTED `create` EMPTIES THE NAME FIELD.**
    ///
    /// The composer's rule, applied to the one other box on this face: a field
    /// cleared optimistically eats what he typed when the far end refused. And
    /// the refusal that matters most here is the one `server/mod.rs:163`
    /// records — a name that is IN THE ARCHIVE is refused and **never
    /// auto-unarchived**, so the words have to survive for him to change.
    ///
    /// FORCED RED by clearing `new_project` in `create_project` rather than on
    /// the ack, and by clearing it on the `Err` arm of `ActDone`.
    #[tokio::test]
    async fn the_typed_project_name_survives_a_refusal_and_only_an_ack_clears_it() {
        let mut app = app();
        let (h, mut out) = crate::aui::test_handle_up();
        app.set_aui(h);
        with_two_projects(&mut app);

        app.on_intent(Intent::NewProjectField);
        assert_eq!(app.vs.mode, Mode::NewProject);
        for ch in "dead".chars() {
            app.on_intent(Intent::Type(ch));
        }
        app.project();
        assert!(
            app.vs.projects.new_project.is_some(),
            "the field is not live in the row"
        );

        // The commit is the KEY TABLE'S, not a hand-written intent: `enter` in
        // `Mode::NewProject` is what he actually presses, and a test that named
        // `Intent::CreateProject` directly would be asserting against its own
        // idea of the binding.
        let commit = press(&app, tui_render::Key::Enter);
        assert_eq!(
            commit,
            Intent::CreateProject,
            "`enter` does not commit the name"
        );
        app.on_intent(commit);
        let job = tokio::time::timeout(Duration::from_secs(5), out.recv())
            .await
            .expect("the create never reached the outbound channel")
            .expect("the channel closed");
        assert_eq!(job.kind(), "create_project");

        // The AUI refuses it — the name is in the archive, and bringing a
        // project back is its own deliberate act.
        app.apply(Ev::ActDone {
            what: "create dead".into(),
            result: Err("dead is in the archive.".into()),
            clears: Some(Clears::NewProjectName),
        });
        app.project();
        assert_eq!(
            app.vs.projects.new_project.as_ref().map(|c| c.text()),
            Some("dead".to_string()),
            "a refused create ate what he typed"
        );
        assert!(
            app.vs
                .notice
                .as_ref()
                .expect("nothing was said")
                .text
                .contains("is in the archive"),
            "the archive collision was not said in words"
        );

        // …and an ACCEPTED one empties it.
        app.apply(Ev::ActDone {
            what: "create dead".into(),
            result: Ok(()),
            clears: Some(Clears::NewProjectName),
        });
        app.project();
        assert!(
            app.vs.projects.new_project.is_none(),
            "an accepted create left his words in the field"
        );
    }
    /// **`n` OPENS THE NAME BOX; `enter` CREATES THE THREAD WITH THAT NAME;
    /// THE DOOR OPENS ON THE ACK AND NOT ON THE PRESS** (TUI-P1a, TC-01).
    ///
    /// *"When I create a new thread … I want the modal to pop up to name the
    /// thread. And then as soon as I hit enter …"* The bytes are
    /// [`acts::route_new_thread`]'s and are asserted there with no socket; what
    /// is asserted HERE is the shell's half: the key is the table's, the press
    /// sends NOTHING and raises the box, `enter` sends a create carrying what
    /// he typed (N = 2: a named one and a blank one, opposed so neither
    /// "always a name" nor "never a name" passes), a REFUSED create opens
    /// nothing and keeps nothing, and an accepted one lands him in the thread.
    ///
    /// FORCED RED (report): create on the press instead of on `enter` (the
    /// press assertion goes red); drop `name` from `create_thread`'s route call
    /// (the named create's bytes go red); open the thread in `create_thread`
    /// instead of on the ack; drop the `pending_new_thread = None` from the
    /// `Err` arm (the refused-then-accepted case opens the dead id).
    #[tokio::test]
    async fn n_names_a_thread_then_creates_it_and_the_door_opens_on_the_ack() {
        let mut app = app();
        with_two_projects(&mut app);

        // THE KEY IS THE TABLE'S.
        let key = press(&app, tui_render::Key::Char('n'));
        assert_eq!(key, Intent::NewThread, "`n` does not start a thread");

        // **THE PRESS RAISES THE BOX AND SENDS NOTHING.**
        app.on_intent(key);
        assert_eq!(app.vs.mode, Mode::RenameModal, "`n` did not raise the name box");
        assert!(app.pending_new_thread.is_none(), "`n` created a thread before it was named");
        assert!(app.last_wire.is_none(), "the press sent something: {:?}", app.last_wire);
        app.project();
        assert!(
            app.vs.modal_label.as_deref().is_some_and(|l| l.contains("new thread")),
            "the box does not say it names a new thread: {:?}",
            app.vs.modal_label
        );
        for c in "Tax letter".chars() {
            let k = press(&app, tui_render::Key::Char(c));
            app.on_intent(k);
        }
        app.on_intent(press(&app, tui_render::Key::Enter));
        assert_eq!(app.vs.mode, Mode::Projects, "enter did not close the box");
        let minted = app
            .pending_new_thread
            .clone()
            .expect("enter minted no id, so there is nothing to open");
        match app.last_wire.as_ref() {
            Some(acts::Wire::Daemon(acts::Verb::Create { thread, name, .. })) => {
                assert_eq!(thread, &minted);
                assert_eq!(name.as_deref(), Some("Tax letter"), "the create did not carry his name");
            }
            other => panic!("enter sent no create: {other:?}"),
        }

        // **A REFUSAL OPENS NOTHING AND KEEPS NOTHING.**
        app.apply(Ev::ActDone {
            what: "a new thread in miscellaneous".into(),
            result: Err("the daemon is not listening".into()),
            clears: Some(Clears::ThreadCreated),
        });
        assert_eq!(app.vs.mode, Mode::Projects, "a refused create opened a view");
        assert!(
            app.pending_new_thread.is_none(),
            "a refused create left its id behind for the next ack to open"
        );

        // …and a BLANK name is an unnamed thread, not a thread named "".
        app.on_intent(press(&app, tui_render::Key::Char('n')));
        app.on_intent(press(&app, tui_render::Key::Enter));
        let minted2 = app.pending_new_thread.clone().expect("a second mint");
        assert_ne!(minted, minted2, "the second `n` reused the first id");
        match app.last_wire.as_ref() {
            Some(acts::Wire::Daemon(acts::Verb::Create { name, .. })) => {
                assert_eq!(name, &None, "a blank name was sent as a name")
            }
            other => panic!("the blank enter sent no create: {other:?}"),
        }
        app.apply(Ev::ActDone {
            what: "a new thread in miscellaneous".into(),
            result: Ok(()),
            clears: Some(Clears::ThreadCreated),
        });
        assert_eq!(
            app.vs.mode,
            Mode::Thread(tui_render::Region::Composer),
            "the thread did not open on the ack"
        );
        assert_eq!(
            app.store.open_id(),
            Some(minted2.as_str()),
            "the face opened a thread that is not the one it minted"
        );
        let said = &app.vs.notice.as_ref().expect("nothing was said").text;
        assert!(said.contains("recent"), "the face did not say where the new thread lives: {said}");
    }

    /// **`n` INSIDE A PROJECT NAMES AND CREATES THE THREAD IN THAT PROJECT**
    /// (TUI-P1a, TC-01: *"if I go into a project and I press the letter N, it
    /// should create a new thread in that project. But it's not."*). Opposed
    /// by the root's `n`, which creates in the bucket.
    ///
    /// FORCED RED (report): resolve the cwd to the bucket unconditionally →
    /// the project's assertion goes red; unbind `n` on `Mode::List` → the key
    /// assertion goes red.
    #[tokio::test]
    async fn n_inside_a_project_creates_the_thread_in_that_project() {
        let mut app = app();
        with_two_projects(&mut app);
        app.store.enter_project(0);
        app.descend(Mode::List);
        app.project();
        assert_eq!(app.view(), Mode::List);
        let root = app.store.open_project().expect("a project is open").root.display().to_string();
        let key = press(&app, tui_render::Key::Char('n'));
        assert_eq!(key, Intent::NewThread, "`n` inside a project does nothing");
        app.on_intent(key);
        for c in "Plan".chars() {
            app.on_intent(press(&app, tui_render::Key::Char(c)));
        }
        app.on_intent(press(&app, tui_render::Key::Enter));
        assert_eq!(app.vs.mode, Mode::List, "enter did not return to the project");
        match app.last_wire.as_ref() {
            Some(acts::Wire::Daemon(acts::Verb::Create { settings, name, .. })) => {
                assert_eq!(settings["cwd"], serde_json::json!(root), "the thread was not made in the project");
                assert_eq!(name.as_deref(), Some("Plan"));
            }
            other => panic!("no create was sent: {other:?}"),
        }
        // THE OPPOSITE ARM: at the root, `n` creates in the bucket.
        let mut root_app = app_at_root();
        root_app.on_intent(press(&root_app, tui_render::Key::Char('n')));
        root_app.on_intent(press(&root_app, tui_render::Key::Enter));
        match root_app.last_wire.as_ref() {
            Some(acts::Wire::Daemon(acts::Verb::Create { settings, .. })) => {
                assert!(
                    settings["cwd"].as_str().is_some_and(|c| c.ends_with("miscellaneous")),
                    "the root's thread was not made in the bucket: {settings}"
                );
            }
            other => panic!("no create was sent at the root: {other:?}"),
        }
    }

    /// **`r` RENAMES A THREAD THIS FACE CREATED — ONE WITH NO INDEX HEADER —
    /// THROUGH THE DAEMON** (TUI-P1a, TC-03: *"I need to be able to access the
    /// options for threads even if they're created as miscellaneous threads"*).
    /// The sheet cannot open over such a thread, so `r` raises the name box
    /// itself; `enter` sends `Verb::Name` for that thread with what he typed,
    /// and an empty box sends nothing and stays up (opposes "always send").
    ///
    /// FORCED RED (report): delete the `else if self.sheet.is_none()` arm in
    /// `Intent::Rename` → no box opens and nothing is sent.
    #[tokio::test]
    async fn r_renames_a_thread_with_no_index_header_through_the_daemon() {
        let mut app = app();
        with_two_projects(&mut app);
        app.store.apply(
            TransportEvent::Threads {
                rows: vec![serde_json::json!({"thread_id": "fresh", "cwd": "/Users/x/.aui/miscellaneous",
                    "identity": true, "app_id": "harness-tui",
                    "last_user_input_at": "2026-10-25T10:00:00+00:00"})],
                capped: false,
            },
            Utc::now(),
        );
        app.project();
        let i = (0..200)
            .find(|i| app.vs.projects.thread_at(*i).is_some_and(|r| r.id == "fresh"))
            .expect("the thread this face created is on the recent band");
        app.project_cursor = i;
        app.on_intent(press(&app, tui_render::Key::Char('r')));
        assert_eq!(app.vs.mode, Mode::RenameModal, "`r` raised no box over a header-less thread");
        // An EMPTY name sends nothing and the box stays.
        app.on_intent(press(&app, tui_render::Key::Enter));
        assert_eq!(app.vs.mode, Mode::RenameModal, "an empty rename closed the box");
        assert!(app.last_wire.is_none(), "an empty rename was sent: {:?}", app.last_wire);
        for c in "Tax letter".chars() {
            app.on_intent(press(&app, tui_render::Key::Char(c)));
        }
        app.on_intent(press(&app, tui_render::Key::Enter));
        assert_eq!(
            app.last_wire.as_ref(),
            Some(&acts::Wire::Daemon(acts::Verb::Name {
                thread: "fresh".into(),
                name: "Tax letter".into(),
            }))
        );
        assert_eq!(app.vs.mode, Mode::Projects);
    }

    /// **THE THREAD'S HEADER IS NAME AND STATUS UNDER EVERY OVERLAY, ON THE
    /// DRAWN FRAME** (TUI-P1a, inspect F-2) — the shell half: a thread with a
    /// cwd, an account and unread elsewhere, then the queue overlay, help and
    /// the name box raised over it in turn. N = 3 overlays, each asserted on
    /// row 0 of the frame `tui_render::frame` draws. The bare thread page is
    /// the positive control.
    ///
    /// FORCED (report): set `thread_page: false` in `store::header` → every
    /// overlay red.
    #[tokio::test]
    async fn every_overlay_over_a_thread_keeps_its_header_to_name_and_status() {
        let mut a = app();
        in_a_thread(&mut a);
        a.on_transport(TransportEvent::Fleet {
            statuses: vec![
                serde_json::json!({"thread_id": "t1", "state": "idle", "executing": false, "queued": 2, "unread": 0}),
                serde_json::json!({"thread_id": "other", "state": "idle", "executing": false, "queued": 0, "unread": 5}),
            ],
            capped: false,
        });
        a.on_transport(TransportEvent::Threads {
            rows: vec![serde_json::json!({"thread_id": "t1", "cwd": "/Users/x/minds/harness", "model": "sonnet",
                                          "account": "alice-example-com"})],
            capped: false,
        });
        a.project();
        let top = |a: &App| screen(a).lines().next().unwrap_or_default().trim_end().to_string();
        let bare = top(&a);
        assert_eq!(bare, "thread t1 · idle", "the control: the bare thread page");
        for (name, raise) in [
            ("queue", Intent::QueueOverlay),
            ("help", Intent::Help),
        ] {
            a.on_intent(raise);
            assert_ne!(a.vs.mode, Mode::Thread(tui_render::Region::Composer), "{name} raised nothing");
            assert_eq!(top(&a), bare, "{name} over the thread brought parts back");
            a.on_intent(Intent::Back);
        }
        a.open_naming(NameTarget::Thread { id: "t1".into() }, "thread t1");
        a.project();
        assert_eq!(a.vs.mode, Mode::RenameModal);
        assert_eq!(top(&a), bare, "the rename box over the thread brought parts back");
    }

    fn app_at_root() -> App {
        let mut a = app();
        with_two_projects(&mut a);
        a
    }

    /// **`esc` IN THE NAME BOX MAKES NOTHING.** Opposes the test above: the
    /// box is the only door, and leaving it sends no create.
    #[tokio::test]
    async fn esc_in_the_new_thread_box_creates_nothing() {
        let mut app = app();
        with_two_projects(&mut app);
        app.on_intent(press(&app, tui_render::Key::Char('n')));
        app.on_intent(press(&app, tui_render::Key::Char('x')));
        app.on_intent(press(&app, tui_render::Key::Esc));
        assert_eq!(app.vs.mode, Mode::Projects);
        assert!(app.naming.is_none() && app.pending_new_thread.is_none() && app.last_wire.is_none());
    }
    /// **THE ARCHIVED DOOR OPENS WITH THE AUDIO INTERFACE DOWN**, and the
    /// unarchive says why on the row.
    ///
    /// `0060`'s second binding condition: nothing this face RENDERS is sourced
    /// from 9010. The archive set is read off the filesystem, so the room draws
    /// whether or not the AUI is running. What needs the AUI is the act — and
    /// that is a sentence on the row, repeated once on activation, never a
    /// silent no-op and never a vanishing row.
    ///
    /// FORCED RED by sourcing `archived_view`'s rows from the link state (the
    /// room draws empty with the owner down), and by returning early from
    /// `unarchive` without the notice.
    #[tokio::test]
    async fn the_archived_room_opens_with_the_owner_down_and_says_why_once() {
        let mut app = app();
        with_two_projects(&mut app);
        app.store.adopt_archived(vec![
            crate::index::Archived {
                name: "dead".into(),
                at: Some(
                    chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
                        .expect("fixture stamp")
                        .with_timezone(&Utc),
                ),
            },
            crate::index::Archived {
                name: "older".into(),
                at: None,
            },
        ]);
        app.project();
        // This App has NO 9010 client, which is what a stopped ui-server looks
        // like from here.
        assert!(app.owners().aui_down.is_some(), "the owner is not down");

        // FOUND, not assumed: the archived row's position depends on what is
        // pinned above it, so the cursor is walked to the item the model names.
        app.project_cursor = (0..app.vs.projects.drawn_len())
            .find(|i| app.vs.projects.item(*i) == Some(RootItem::Archived))
            .expect("the archived row is not on the root at all");
        app.on_intent(Intent::Open);
        assert_eq!(app.vs.mode, Mode::Archived);
        assert_eq!(
            app.vs
                .archived
                .rows
                .iter()
                .map(|r| r.name.clone())
                .collect::<Vec<_>>(),
            vec!["dead".to_string(), "older".to_string()],
            "the room is empty or mis-ordered with the owner down — \
             it is read off the filesystem and must draw regardless"
        );
        let why = app.vs.archived.rows[0]
            .avail
            .reason()
            .expect("the row is not muted with the owner down")
            .to_string();

        app.on_intent(Intent::Unarchive(0));
        let n = app
            .vs
            .notice
            .as_ref()
            .expect("activating said nothing at all");
        assert_eq!(
            n.text, why,
            "the notice and the row say different things about the same refusal"
        );
        // The row STAYS, at its position, and the room stays up.
        assert_eq!(app.vs.mode, Mode::Archived);
        assert_eq!(app.vs.archived.rows.len(), 2);
    }

    /// **THE SHEET IS WHERE THE ID STILL LIVES**, and that is the exchange
    /// `0076` owed for taking it off the row.
    ///
    /// `DEC-27` made a thread row copy its eight-character id so that a thread
    /// had a reachable ADDRESS. `DIR-16` says the face may not SHOW him a
    /// hash, and once the id left the screen the copy had to follow it — a run
    /// richer than its row resolves every dragged column to the wrong
    /// character. Removing the last reachable copy of a thread's address would
    /// have been a regression dressed as a fix, so it moved to an ACT: a verb
    /// he presses on purpose, which keeps §5's *"absent, deliberately … the
    /// id"* true of the sheet's FACTS line and still hands him the address.
    ///
    /// **AND IT IS THE FULL THIRTY-SIX CHARACTERS**, not the eight the row
    /// used to copy. The abbreviation existed because a copied run had to map
    /// column-for-column onto a drawn row; an act has no columns to map.
    ///
    /// FORCED RED three ways: delete the `CopyId` row from `store::sheet`;
    /// point its arm in `fire_sheet_act` at `act_for` (which returns `None`
    /// for it, so nothing is copied); and copy `short`-style eight characters
    /// instead of the whole id.
    #[tokio::test]
    async fn the_sheet_is_where_a_threads_id_is_still_reachable() {
        let mut app = app();
        app.store
            .adopt_projects(vec![proj("harness", &["9fe1bc02-4b77-4d0e-9a55-1e6b0c2f77aa"])]);
        app.project();
        // Selectable 0 is the RECENT band's own header (`DEC-46`), not a row.
        let sel = first_thread_row(&app);
        open_options(&mut app, sel);
        assert_eq!(app.vs.mode, Mode::Sheet);

        let i = app
            .vs
            .sheet
            .as_ref()
            .expect("a sheet")
            .rows
            .iter()
            .position(|r| r.act == SheetAct::CopyId)
            .expect("the id has no home on the sheet at all");
        // It says what it does, in words, like every other act row.
        assert_eq!(
            app.vs.sheet.as_ref().expect("a sheet").rows[i].stem,
            "copy the thread id"
        );
        // **AND IT IS NEVER MUTED.** It leaves no process, so no owner can be
        // down for it — the one row on this sheet that is always ready.
        assert!(app.vs.sheet.as_ref().expect("a sheet").rows[i].avail.is_ready());

        app.on_intent(Intent::SheetAct(i));
        assert_eq!(
            app.copy_owed.as_deref(),
            Some("9fe1bc02-4b77-4d0e-9a55-1e6b0c2f77aa"),
            "the id did not reach the clipboard, or reached it abbreviated"
        );
        // The sheet stays up: copying is not leaving.
        assert_eq!(app.vs.mode, Mode::Sheet);
    }

    // ────────────────────────────────────────────────────────────────────
    //  THE REGION MODEL — which band holds the selection, and every arrow
    //  resolving against it.
    // ────────────────────────────────────────────────────────────────────

    /// Open a thread with one of HIS messages, one reply, and a laid-out frame.
    fn in_a_thread(a: &mut App) {
        a.store.adopt_projects(vec![proj("harness", &["t1"])]);
        a.project();
        a.lay(100, 24);
        a.open("t1");
    }

    /// Press `k` and dispatch whatever the table says it means, exactly as the
    /// loop does. Tests that drive intents directly cannot catch a binding that
    /// was never written.
    fn tap(a: &mut App, k: tui_render::Key) {
        let i = press(a, k);
        a.on_intent(i);
    }

    /// **FROM THE BYTES, through every hop the real keyboard takes.**
    ///
    /// `tap` starts at a `Key` a test made up, which is the layer that cannot
    /// fail: `intent(Key::Alt('f'), …)` was green for a whole arc while `opt-f`
    /// did nothing on his machine. This starts at the wire — `vt::Parser`, then
    /// `input::to_key`, then the table, then the dispatch — so a key that is
    /// bound but undeliverable is a RED test rather than a green one.
    fn deliver(a: &mut App, bytes: &[u8]) {
        let mut p = crate::vt::Parser::new();
        let mut out = Vec::new();
        p.feed(bytes, std::time::Instant::now(), &mut out);
        let mut acted = false;
        for o in out {
            if let crate::vt::Out::Event(ratatui::crossterm::event::Event::Key(k)) = o {
                if let Some(key) = crate::input::to_key(k) {
                    // **THROUGH `App::resolve`, not through the table**, so a
                    // standing modal question is resolved here exactly as the
                    // loop resolves it. This line read `tui_render::intent(…)`
                    // until the draft offer became a modal, and a helper that
                    // skips a hop the real keyboard takes is a helper whose
                    // green means less than it looks like.
                    let i = a.resolve(key);
                    a.on_intent(i);
                    acted = true;
                }
            }
        }
        assert!(acted, "those bytes produced no key this face has a name for: {bytes:?}");
    }

    /// **THE RECOVERED-DRAFT OFFER IS A MODAL QUESTION, ANSWERED BY `y`/`n`**,
    /// and every other letter still types.
    ///
    /// `AcceptDraft`/`DiscardDraft` were `opt-y` and `opt-d` until round six.
    /// macOS composes those into `¥` and `∂`, so **they have never worked on his
    /// keyboard**, and `0128` found them no mnemonic ctrl home either — `^Y` is
    /// yank and `^D` is delete, both readline conventions inside a textarea.
    /// The audit's answer is better than any letter: the offer is live only
    /// while it is ON SCREEN, so answering the offer itself costs no namespace.
    ///
    /// **THIS DRIVES BYTES**, through `vt::Parser` -> `input::to_key` ->
    /// `App::resolve`, because the thing at risk is a resolution hop and not a
    /// branch: a test that called `answer_offer` directly would pass on a face
    /// where `y` had never been wired to anything.
    ///
    /// FORCED: make `App::offer_answer` return `None` unconditionally and the
    /// `n` half goes red with the offer still standing; take the `mode ==
    /// Thread(Composer)` guard out of it and nothing here notices — which is
    /// why the LAST assertion exists, where `y` must type with no offer up.
    #[tokio::test]
    async fn the_draft_offer_is_answered_by_y_and_n_and_other_letters_still_type() {
        let mut a = app();
        in_a_thread(&mut a);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));

        let offer = || tui_render::DraftOffer {
            text: "half a sentence he never sent".into(),
            age: "2m ago".into(),
        };

        // ── `n` DISCARDS. The offer goes and the box stays empty.
        a.vs.composer.offer = Some(offer());
        deliver(&mut a, b"n");
        assert!(
            a.vs.composer.offer.is_none(),
            "`n` did not answer the offer"
        );
        assert_eq!(
            a.vs.composer.buf.text(),
            "",
            "`n` typed an `n` instead of discarding"
        );

        // ── `y` RESTORES. The draft comes back as the buffer.
        a.vs.composer.offer = Some(offer());
        deliver(&mut a, b"y");
        assert!(a.vs.composer.offer.is_none(), "`y` did not answer the offer");
        assert_eq!(
            a.vs.composer.buf.text(),
            "half a sentence he never sent",
            "`y` did not restore the draft"
        );

        // ── EVERY OTHER LETTER STILL TYPES WHILE THE OFFER STANDS. This is the
        //    property that pays for the modal, and the one a binding would have
        //    broken: `y` and `n` are not reserved, they are ANSWERED.
        a.vs.composer.buf = Default::default();
        a.vs.composer.offer = Some(offer());
        deliver(&mut a, b"z");
        assert_eq!(a.vs.composer.buf.text(), "z", "`z` stopped being text");

        // ── AND WITH NO OFFER UP, `y` IS A LETTER AGAIN. Without this the
        //    modal could have become a permanent binding and every assertion
        //    above would still pass.
        a.vs.composer.buf = Default::default();
        a.vs.composer.offer = None;
        deliver(&mut a, b"y");
        assert_eq!(
            a.vs.composer.buf.text(),
            "y",
            "`y` is bound even with no offer standing — the modal became a key"
        );
    }

    /// **THE HELP DOOR REACHES THE TEXTAREA ON BOTH DIALECTS, FROM THE WIRE.**
    ///
    /// This is the single most consequential row of the migration and the one
    /// nobody had named before `0128`: in the four thread regions and in mic
    /// mode the modified spelling was the ONLY one — `?` types a `?` there —
    /// so **the help overlay was unreachable from the textarea on his keyboard**
    /// (`opt-/` is `÷`).
    ///
    /// `ctrl-/` arrives two ways depending on what the session negotiated, and
    /// BOTH are in one row so the door is `Need::Always`:
    ///   * `CSI 27;5;47~` under modifyOtherKeys, which his tmux turns on ->
    ///     `Key::Ctrl('/')`
    ///   * the bare byte `0x1f` on any other stack, which `vt.rs:406` maps to
    ///     `Key::Ctrl('7')` (`0x1c..=0x1f` -> `Ctrl('4')..Ctrl('7')`)
    ///
    /// FORCED: drop either key from the `common_rows`/`alt_common_rows` help
    /// row and the matching half goes red with the mode unchanged.
    #[tokio::test]
    async fn both_dialects_of_the_help_chord_open_the_overlay_from_inside_the_box() {
        for bytes in [&b"\x1b[27;5;47~"[..], &b"\x1f"[..]] {
            let mut a = app();
            in_a_thread(&mut a);
            assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
            deliver(&mut a, bytes);
            assert_eq!(
                a.vs.mode,
                Mode::Help,
                "these bytes did not open the help overlay from the textarea: {bytes:?}"
            );
            // THE WITNESS: it really is the overlay and not a mode set by
            // accident — it renders the COMPOSER's grammar, which is the whole
            // point of a contextual help screen.
            assert_eq!(a.vs.help_for, Mode::Thread(Region::Composer));
        }
    }

    // ────────────────────────────────────────────────────────────────────
    //  HOLD TO SPEAK — the second mic gesture, beside the toggle
    // ────────────────────────────────────────────────────────────────────

    /// A face sitting in mic mode with a capture command that opens no device.
    ///
    /// `/usr/bin/true` is a boundary substitution and not a fake microphone:
    /// it is spawned by the same `Command` the real `rec` is, and it records
    /// nothing. **The real device is never opened** — `0046` forbids it, and
    /// the grant it would need has never been given.
    fn in_mic_mode(a: &mut App) {
        std::env::set_var(crate::mic::CAPTURE_VAR, "/usr/bin/true");
        in_a_thread(a);
        deliver(a, b"\x14");
        assert_eq!(a.vs.mode, Mode::Mic, "^T did not reach mic mode");
        assert_eq!(a.vs.mic, MicState::Idle);
    }

    /// **HOLDING `record` STREAMS ONE CAPTURE. IT DOES NOT MACHINE-GUN THE
    /// TOGGLE, AND NOTHING SAYS HE LET GO EXCEPT SILENCE.**
    ///
    /// His ask: *"if you hold down space then it does a STT stream… in
    /// addition to the toggle mode."* There is no key-release event in his
    /// terminal (`0095`) so the end of the hold is an IDLE GAP, and the clock
    /// is handed in here rather than slept for, so the cadence is exact.
    ///
    /// **WHAT THIS ALSO PROVES IS A DEFECT THAT WAS ALREADY THERE.** Before
    /// this gesture, every auto-repeat of `r` or `space` in mic mode reached
    /// `toggle_record` and acted: hold the key for a second at the macOS
    /// default rate and the face started, stopped, started and stopped a
    /// capture seven times, spawning a transcription of each sub-tenth-second
    /// fragment. Mic mode has never had a user, so nobody has seen it.
    ///
    /// FORCED: make `Press::Repeat`/`Press::Defer` fall through to
    /// `toggle_record()` and the mid-burst assertion goes red with the
    /// microphone shut.
    #[tokio::test]
    async fn holding_record_streams_one_capture_instead_of_machine_gunning_the_toggle() {
        let mut a = app();
        in_mic_mode(&mut a);

        // **THE WITNESS, AND IT IS NOT AN ABSENCE.** The first press must
        // POSITIVELY open a capture; every later assertion is that the same
        // capture is still open. A face that had stopped answering keys fails
        // here, before anything about repeats is claimed.
        let t0 = std::time::Instant::now();
        a.on_mic_record_at(t0);
        assert_eq!(
            a.vs.mic,
            MicState::Recording,
            "the first press of the hold did not open a capture"
        );

        // macOS's own cadence: the initial repeat delay, then the rate.
        let mut t = t0 + Duration::from_millis(375);
        a.on_mic_record_at(t);
        for _ in 0..8 {
            t += Duration::from_millis(90);
            a.on_mic_record_at(t);
            assert_eq!(
                a.vs.mic,
                MicState::Recording,
                "an auto-repeat toggled the microphone off mid-utterance"
            );
        }
        assert!(a.hold.armed(), "eight repeats were not read as a hold");

        // **THE FIFTH CLOCK IS ARMED, AND IT IS THE GAP IT IS WAITING FOR.**
        let d = a
            .hold.deadline(t)
            .expect("a hold in flight armed no wake-up — the mic would stay open forever");
        assert_eq!(d, a.hold.gap());
        assert!(
            d < Duration::from_millis(300),
            "the face would sit on an open microphone for {d:?} after he let go"
        );

        // …and the silence that follows is what ends it.
        let what = a.hold.expire(t + d);
        a.settle_hold(what);
        assert_eq!(
            a.vs.mic,
            MicState::Stopping,
            "he let go and the capture was never closed"
        );
    }

    /// **A TAP IS STILL A TOGGLE.** He asked for the hold *"in addition to the
    /// toggle mode"*, so the gesture that was already here has to survive the
    /// one built beside it — and a tap must never be auto-stopped by a gap it
    /// was not part of.
    ///
    /// FORCED: drop the `count == 2` guard from `Hold::owed` so every burst
    /// ends as a hold, and the "still recording" assertion goes red.
    #[tokio::test]
    async fn a_tap_still_toggles_and_no_idle_gap_closes_it_under_him() {
        let mut a = app();
        in_mic_mode(&mut a);

        let t0 = std::time::Instant::now();
        a.on_mic_record_at(t0);
        assert_eq!(a.vs.mic, MicState::Recording, "a tap did not start a capture");
        assert!(!a.hold.armed(), "one press was read as a hold");
        // **THE CLOCK CLAIM IS TAKEN NOW AND ASSERTED LAST, DELIBERATELY.**
        // A mutation that makes every burst end as a hold trips both this and
        // the behaviour below; asserting the clock first would hide the
        // behaviour behind it and the strongest claim in the test would never
        // be the one that failed. (`0115`'s mutation 9, learned rather than
        // repeated.)
        let clock = a.hold.deadline(t0);
        // Ten seconds of him talking with nothing held down.
        let late = t0 + Duration::from_secs(10);
        let what = a.hold.expire(late);
        a.settle_hold(what);
        assert_eq!(
            a.vs.mic,
            MicState::Recording,
            "the idle gap stopped a capture that was never a hold"
        );
        assert!(
            clock.is_none(),
            "a tap armed a clock that would have stopped his recording under him"
        );
        // And the second tap stops it, exactly as it always did.
        a.on_mic_record_at(late);
        assert_eq!(a.vs.mic, MicState::Stopping, "the second tap did not stop it");
    }

    /// **A SECOND PRESS TOO FAST TO TELL FROM A REPEAT IS LATE, NOT LOST.**
    ///
    /// One sample cannot separate "the first auto-repeat of a hold" from "he
    /// pressed it again" — the discriminator is whether a third event follows
    /// at a machine's cadence. So the press is HELD and the idle gap decides.
    /// **No keystroke this face received is silently dropped**, which is the
    /// only reason the ambiguity is affordable.
    ///
    /// FORCED: make `Hold::owed` answer `Expire::None` for `count == 2` and
    /// the press vanishes — the capture is still running at the end.
    #[tokio::test]
    async fn a_second_record_press_too_fast_to_tell_from_a_repeat_is_late_not_lost() {
        let mut a = app();
        in_mic_mode(&mut a);

        let t0 = std::time::Instant::now();
        a.on_mic_record_at(t0);
        assert_eq!(a.vs.mic, MicState::Recording);

        // 250 ms: inside the window that bridges macOS's initial repeat delay,
        // so at this instant it is genuinely unknowable which gesture it is.
        let t1 = t0 + Duration::from_millis(250);
        a.on_mic_record_at(t1);
        assert_eq!(
            a.vs.mic,
            MicState::Recording,
            "the ambiguous press acted immediately and would have cut a hold in two"
        );
        // Taken now, asserted last — see `a_tap_still_toggles…` for why.
        let clock = a.hold.deadline(t1);
        let what = a.hold.expire(t1 + a.hold.gap());
        a.settle_hold(what);
        assert_eq!(
            a.vs.mic,
            MicState::Stopping,
            "his second press was eaten by the hold detector"
        );
        assert!(
            clock.is_some(),
            "the held press armed no wake-up — it would never have landed"
        );
    }

    /// **ANY OTHER KEY ENDS THE BURST** — auto-repeat stops the moment another
    /// key goes down, so a `record` press after some other act is a fresh tap
    /// and not that burst's first repeat. Without this the burst would span
    /// unrelated work and the gesture would misread a tap seconds later.
    ///
    /// FORCED: delete the `interrupt_hold()` line from `on_intent` and the
    /// second `record` press is swallowed as a repeat — the capture never
    /// stops and the last assertion goes red.
    #[tokio::test]
    async fn another_act_ends_the_burst_and_lands_the_press_it_was_holding() {
        let mut a = app();
        in_mic_mode(&mut a);

        let t0 = std::time::Instant::now();
        a.on_mic_record_at(t0);
        assert_eq!(a.vs.mic, MicState::Recording);
        // The ambiguous second press, then something else entirely.
        a.on_mic_record_at(t0 + Duration::from_millis(200));
        assert_eq!(a.vs.mic, MicState::Recording, "the held press acted early");
        a.on_intent(Intent::MicPlayLast);
        assert_eq!(
            a.vs.mic,
            MicState::Stopping,
            "the press the burst was holding was thrown away when another act arrived"
        );
        assert!(
            a.hold.deadline(t0 + Duration::from_millis(200)).is_none(),
            "the burst outlived the act that proved the key was up"
        );
    }

    /// **THE GESTURE IS REACHED BY THE BYTE THE TERMINAL ACTUALLY SENDS.**
    ///
    /// `0115`'s lesson: a test that starts at a `Key` value cannot see a key
    /// that is bound and undeliverable — `intent(Key::Alt('m'), …)` was green
    /// for a whole arc while `opt-m` did nothing on his machine. Space through
    /// his stack is one byte, `0x20`, and there is no other spelling available
    /// (`0095`, measured on a pty through his tmux).
    #[tokio::test]
    async fn the_space_byte_reaches_the_record_act_in_mic_mode() {
        let mut a = app();
        in_mic_mode(&mut a);
        deliver(&mut a, b" ");
        assert_eq!(
            a.vs.mic,
            MicState::Recording,
            "the byte 0x20 — the only spelling of space his terminal has — did not record"
        );
        // The witness: the face is still answering keys afterwards.
        deliver(&mut a, b"\x14");
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
    }

    /// **THE BAND MUST NEVER SAY THE MICROPHONE IS SHUT WHILE IT IS OPEN.**
    ///
    /// A DEFECT IN THE EXISTING TOGGLE PATH, found by building the hold
    /// gesture beside it and measured on a real pty before it was fixed: three
    /// taps 700 ms apart left `rec` running with the band reading *"press r to
    /// record"*. Mic mode has never had a user, so nobody had seen it.
    ///
    /// The cause is that STOPPING is asynchronous — signalling the child,
    /// waiting for it and repairing its header takes up to a second and a
    /// half, and doing that inline would freeze the face — so `Ev::Recorded`
    /// arrives LATE. It used to set `MicState::Idle` unconditionally, which
    /// overwrites the `Recording` a press made in the meantime. `mic.rs`'s own
    /// header names this state as the one mic mode must never be in: *"a face
    /// that is recording and does not look like it is recording"*.
    ///
    /// **THE HOLD GESTURE MAKES IT EASY TO REACH** — let go, and press again
    /// while the stop is still in flight — which is why it is fixed here
    /// rather than only reported.
    ///
    /// FORCED: drop the `self.recording.is_none()` guards in `on_recorded`
    /// and both halves of this go red.
    #[tokio::test]
    async fn a_late_recorded_event_never_says_idle_over_a_live_capture() {
        let mut a = app();
        in_mic_mode(&mut a);
        std::env::set_var(crate::mic::STT_VAR, "/usr/bin/false");

        let t = std::time::Instant::now();
        a.on_mic_record_at(t);
        assert_eq!(a.vs.mic, MicState::Recording, "the first tap did not record");
        a.on_mic_record_at(t + Duration::from_secs(2));
        assert_eq!(a.vs.mic, MicState::Stopping);
        a.on_mic_record_at(t + Duration::from_secs(4));
        assert_eq!(a.vs.mic, MicState::Recording, "the third tap did not record");
        assert!(a.recording.is_some(), "the fixture stopped holding a child");

        // The FIRST capture's stop thread reports, now, late.
        a.apply(Ev::Recorded {
            result: Ok("/tmp/does-not-matter.wav".into()),
        });
        assert_eq!(
            a.vs.mic,
            MicState::Recording,
            "the band said the microphone was shut while a capture was open"
        );
        // …and the row it carried still joined the queue, which is the half a
        // guard in the wrong place would have eaten.
        assert_eq!(
            a.vs.composer.buf.pending().len(),
            1,
            "the late capture never reached the queue"
        );

        // The failure shape says the same thing: a refusal from a capture that
        // has already been superseded must not paint over a live one.
        a.apply(Ev::Recorded {
            result: Err("the recording came back empty — nothing was captured".into()),
        });
        assert_eq!(
            a.vs.mic,
            MicState::Recording,
            "a late refusal painted over a live capture"
        );
        // The notice still tells him what happened — the fact is true, it is
        // just not the BAND's fact.
        assert!(
            a.vs.notice
                .as_ref()
                .is_some_and(|n| n.text.contains("came back empty")),
            "the late failure was swallowed entirely"
        );
    }

    /// **TEXT WINS, AND THE ESCAPE IS THE EXCEPTION.** His rule, in the order
    /// he stated it: *"a cursor in the textarea should not interpret any key
    /// bindings as anything other than text. So if the textarea is empty then
    /// up, down and left each have a purpose."*
    ///
    /// FORCED: swap the two halves of the `E::Up` arm in `App::compose` — step
    /// the region first and give the buffer what is left — and the "moved
    /// inside the text" assertion goes red while the empty-box one stays
    /// green. That is the inverted-boundary failure, and only the second half
    /// of this test can see it.
    #[tokio::test]
    async fn the_arrows_are_text_until_the_box_has_none_left_in_that_direction() {
        let mut a = app();
        in_a_thread(&mut a);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));

        // **AN EMPTY BOX IS AT EVERY EDGE AT ONCE**, which is exactly why he
        // said up, down and left each have a purpose there.
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Chat),
            "↑ out of an empty box did not reach the chat"
        );
        tap(&mut a, tui_render::Key::Down);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer), "↓ did not come back");
        // With no subagent band there is nothing below the box, so `↓` is a
        // no-op — "down to subagents IF APPLICABLE".
        tap(&mut a, tui_render::Key::Down);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Composer),
            "↓ left the box for a band that is not drawn"
        );

        // **NOW GIVE IT TEXT.** Two lines, caret at the end: `↑` is a text
        // motion and must NOT leave, because there is text above the caret.
        a.vs.composer.buf = tui_render::compose::Composer::from_text("alpha\nbravo");
        a.vs.composer.buf.buffer_end();
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Composer),
            "↑ escaped a box that still had a line above the caret"
        );
        assert_eq!(a.vs.composer.buf.cursor().line, 0, "↑ did not move the caret");
        // From the FIRST row it escapes, because now there is nothing above.
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Chat));
    }

    /// **`←` AT THE START OF LINE 2 IS TEXT MOVEMENT, NOT AN EXIT.**
    ///
    /// The edge for `←` is the BUFFER START — line 0, byte 0 — and not the
    /// start of a visual row, which is the edge for `↑`/`↓`. A builder who
    /// reads "first position" as the row is the failure this catches, and it
    /// is silent: the thread simply closes under him mid-draft.
    ///
    /// FORCED: change the guard in `App::compose` to `c.cursor().byte == 0`
    /// and the first assertion goes red — the draft's second line becomes a
    /// door out of the thread.
    #[tokio::test]
    async fn left_at_the_head_of_a_second_line_moves_the_caret_and_does_not_exit() {
        let mut a = app();
        in_a_thread(&mut a);
        a.vs.composer.buf = tui_render::compose::Composer::from_text("alpha\nbravo");
        a.vs.composer.buf.buffer_end();
        for _ in 0..5 {
            tap(&mut a, tui_render::Key::Left);
        }
        assert_eq!(
            a.vs.composer.buf.cursor(),
            tui_render::compose::Pos::new(1, 0),
            "the fixture did not reach the head of line 2"
        );
        tap(&mut a, tui_render::Key::Left);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Composer),
            "← at the head of line 2 left the thread — the edge was read as the ROW"
        );
        assert_eq!(
            a.vs.composer.buf.cursor(),
            tui_render::compose::Pos::new(0, 5),
            "← did not move to the end of the line above"
        );
        // **AND AT THE BUFFER START IT IS THE WAY OUT**, which is the other
        // half and the one his words name: *"left to exit"*.
        a.vs.composer.buf.buffer_start();
        tap(&mut a, tui_render::Key::Left);
        assert!(
            !matches!(a.vs.mode, Mode::Thread(_)),
            "← at the very start of the draft did not leave the thread"
        );
    }

    /// **A REGION IS NOT A DOOR.** Stepping between bands must never push onto
    /// the back-stack, or `←` would "go back" to a band instead of leaving.
    ///
    /// FORCED: route `select_region` through `descend` and the last assertion
    /// lands in `Thread(Chat)` instead of leaving the thread.
    #[tokio::test]
    async fn stepping_between_regions_does_not_push_the_back_stack() {
        let mut a = app();
        a.store
            .adopt_projects(vec![proj("airbnb", &["x"]), proj("harness", &["a", "b"])]);
        a.project();
        a.lay(100, 24);
        a.on_intent(Intent::Select(1));
        let i = right(&a);
        a.on_intent(i);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
        let depth = a.origin.len();

        tap(&mut a, tui_render::Key::Up);
        tap(&mut a, tui_render::Key::Down);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
        assert_eq!(
            a.origin.len(),
            depth,
            "a region change was pushed onto the back-stack"
        );
        tap(&mut a, tui_render::Key::Left);
        assert_eq!(
            a.vs.mode,
            Mode::Projects,
            "← went back to a REGION rather than out of the thread"
        );
    }

    /// **`esc` COMES HOME TO THE BOX AND DOES NOT LEAVE THE THREAD.**
    ///
    /// *"I don't even want ESC to do that behavior."* What died is one row of
    /// the table; the back-stack `←` now reaches is untouched.
    ///
    /// FORCED: put the `esc` row back in `composer_rows` and the first
    /// assertion goes red; bind `esc` to `Intent::Back` in `chat_rows` and the
    /// second does.
    #[tokio::test]
    async fn esc_is_no_longer_the_way_out_of_a_thread() {
        let mut a = app();
        in_a_thread(&mut a);
        assert_eq!(
            press(&a, tui_render::Key::Esc),
            Intent::None,
            "esc still means something in the box"
        );
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Chat));
        tap(&mut a, tui_render::Key::Esc);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Composer),
            "esc out of the chat did not come home to the box"
        );
    }

    /// **THE QUEUE IS A STOP BETWEEN THE CHAT AND THE BOX** — *"just make the
    /// queue accessible by click or by navigating above or below the
    /// textarea"* — and it is a stop ONLY while it is holding something, which
    /// is the "if it exists" half of his order.
    ///
    /// FORCED: drop the `pending().is_empty()` guard from `App::stops` and the
    /// first assertion goes red, because `↑` then lands on an empty band.
    #[tokio::test]
    async fn the_queue_is_a_stop_between_the_chat_and_the_box() {
        let mut a = app();
        in_a_thread(&mut a);
        // Nothing queued: `↑` goes straight past it to the chat.
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Chat),
            "an empty queue was a stop"
        );
        tap(&mut a, tui_render::Key::Esc);

        // Queue two messages and the band exists.
        for text in ["first", "second"] {
            // Typed through the intent, not assigned: replacing `buf` would
            // throw away the pending list the queue lives in.
            for c in text.chars() {
                a.on_intent(Intent::Type(c));
            }
            a.on_intent(Intent::QueueMessage);
        }
        assert_eq!(a.queue_len(), 2);
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Queue));
        assert_eq!(
            a.vs.queue_cursor, 1,
            "entering the queue from BELOW must land on the last row"
        );
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(a.vs.queue_cursor, 0, "↑ did not walk the queue's rows");
        assert_eq!(a.vs.mode, Mode::Thread(Region::Queue));
        // Off the top of the list, into the chat.
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Chat));
        // …and back down through it, landing on the FIRST row this time.
        tap(&mut a, tui_render::Key::Down);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Queue));
        assert_eq!(a.vs.queue_cursor, 0);
        tap(&mut a, tui_render::Key::Down);
        tap(&mut a, tui_render::Key::Down);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Composer),
            "↓ off the last queued row did not reach the box"
        );

        // **AND A REGION THAT VANISHES HANDS THE SELECTION TO THE BOX.**
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Queue));
        a.on_intent(Intent::Compose(tui_render::Edit::Unqueue));
        a.on_intent(Intent::Compose(tui_render::Edit::Unqueue));
        assert_eq!(a.queue_len(), 0);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Composer),
            "the selection was left on a band that no longer exists"
        );
    }

    /// **THE TEXTAREA IS INACTIVE WHEN THE SELECTION IS OUTSIDE IT.**
    ///
    /// > *if I do keypresses anywhere, including when I'm in the chat scroller
    /// > or in the agent tree, the keys still make changes into the textarea.*
    ///
    /// This REPLACES `typing_from_the_chat_types_and_brings_the_selection_with_it`,
    /// which asserted the opposite and passed for a whole round. The rule it
    /// defended — round four's "typing always types, and takes the selection
    /// with it" — is the defect, in his own reading of the screen.
    ///
    /// **THE WITNESS, because "the character did not appear" is an ABSENCE and
    /// a face that never came up scores it triumphantly.** The draft holds the
    /// word `seed`, typed through the same key path before the walk starts, and
    /// every assertion below is an EQUALITY against it — so a dead app fails on
    /// `seed` rather than passing on emptiness. The witness is keyed on text
    /// that no part of this change can move: `seed` is not named by any
    /// binding, any region or any row.
    ///
    /// FORCED, four ways, each watched red: (1) put the `KeySet::Printable` row
    /// back in `chat_rows!` — the chat assertion reads `seedj`; (2) the same in
    /// `agents_rows!` — the tree assertion reads `seedj`; (3) the same in
    /// `queue_rows!` — the queue assertion reads `seedj`; (4) delete the final
    /// `esc`-then-type block and the test still passes with a face that cannot
    /// type at all, which is why that block is not optional.
    #[tokio::test]
    async fn the_textarea_is_inactive_when_the_selection_is_outside_it() {
        let mut a = app();
        a_thread_with_subagents(&mut a);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
        for c in "seed".chars() {
            tap(&mut a, tui_render::Key::Char(c));
        }
        assert_eq!(
            a.vs.composer.buf.text(),
            "seed",
            "the box never took a character, so nothing below would mean anything"
        );

        // ── THE CHAT SCROLLER, by name ───────────────────────────────────
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Chat));
        for c in "jx?".chars() {
            tap(&mut a, tui_render::Key::Char(c));
        }
        assert_eq!(
            a.vs.composer.buf.text(),
            "seed",
            "keys typed in the chat scroller reached the textarea"
        );
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Chat),
            "a printable key dragged the selection out of the chat"
        );

        // ── THE AGENT TREE, by name ──────────────────────────────────────
        tap(&mut a, tui_render::Key::Esc);
        tap(&mut a, tui_render::Key::Down);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Agents));
        for c in "jx".chars() {
            tap(&mut a, tui_render::Key::Char(c));
        }
        assert_eq!(
            a.vs.composer.buf.text(),
            "seed",
            "keys typed in the agent tree reached the textarea"
        );
        assert_eq!(a.vs.mode, Mode::Thread(Region::Agents));

        // ── AND THE CLIENT QUEUE, the third band ─────────────────────────
        tap(&mut a, tui_render::Key::Esc);
        a.on_intent(Intent::QueueMessage);
        assert_eq!(a.queue_len(), 1, "nothing was queued, so there is no band");
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Queue));
        for c in "jx".chars() {
            tap(&mut a, tui_render::Key::Char(c));
        }
        assert_eq!(
            a.vs.composer.buf.text(),
            "",
            "keys typed in the queue band reached the textarea"
        );
        assert_eq!(a.vs.mode, Mode::Thread(Region::Queue));

        // ── AND A PASTE IS TYPING WITH MORE CHARACTERS IN IT ─────────────
        // It never passes through the key table, so `on_paste` is its own
        // boundary and needs its own assertion.
        a.on_paste("a clipboard");
        assert_eq!(
            a.vs.composer.buf.text(),
            "",
            "a paste reached the textarea from the queue band"
        );
        assert!(
            a.vs.notice.as_ref().is_some_and(|n| n.text.contains("esc")),
            "the paste was swallowed with nothing said: {:?}",
            a.vs.notice
        );

        // ── THE OTHER HALF OF THE WITNESS: the box is not broken, it is
        //    INACTIVE, and `esc` is what makes it live again.
        tap(&mut a, tui_render::Key::Esc);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
        for c in "back".chars() {
            tap(&mut a, tui_render::Key::Char(c));
        }
        assert_eq!(
            a.vs.composer.buf.text(),
            "back",
            "the box stopped taking keys even with the selection in it"
        );
    }

    /// **`shift-enter` IS A NEWLINE, FROM THE BYTES TO THE BUFFER — and it is
    /// EMPHATICALLY NOT A SEND.**
    ///
    /// This key had no test at all. `vt` asserts the xterm form decodes to a
    /// shifted Enter and `keys.rs` holds the binding, but nothing joined them:
    /// nothing said the binding means `Newline`, and nothing drove the key
    /// through dispatch. On the most-pressed key in any composer, that is
    /// `0046`'s "an inherited green is not coverage" in its purest form — and
    /// `IS-33` is the recorded case of this exact key SENDING a message he was
    /// still writing, so the failure mode is not hypothetical.
    ///
    /// It is written as a DELIVERY test because that is the hop that was
    /// unguarded, and because the `opt-*` work in this round reads the same
    /// decoder: `CSI 27;2;13~` is what tmux emits under `extended-keys-format
    /// xterm`, which is his live setting.
    ///
    /// **IT WAS GREEN THE FIRST TIME IT RAN.** No defect fell out of it; what
    /// it closes is the hole, not a bug. The forced failures below are what
    /// make that statement worth anything.
    ///
    /// FORCED, three ways, each watched red: (1) point the `ShiftEnter` row at
    /// `Intent::Send` and the draft comes back as one line with the payload
    /// gone; (2) delete the `KeyCode::Enter if shift` arm from `input::to_key`
    /// and the key flattens onto a plain Enter, which SENDS; (3) delete the
    /// xterm `27;<mods>;<code>~` arm from `vt` and the bytes produce no key at
    /// all, which `deliver` itself catches.
    #[tokio::test]
    async fn shift_enter_is_a_newline_and_not_a_send() {
        let mut a = app();
        in_a_thread(&mut a);
        for c in "alpha".chars() {
            tap(&mut a, tui_render::Key::Char(c));
        }
        // The WITNESS: the very same decoder, on the very same shape of
        // sequence, delivering a key that is NOT under test — so a `vt` that
        // had stopped parsing `CSI 27;…;…~` at all cannot score this test by
        // making the newline "not a send" through silence.
        deliver(&mut a, b"\x1b[27;3;127~"); // opt-delete: kill the word back
        assert_eq!(
            a.vs.composer.buf.text(),
            "",
            "the witness key did not arrive, so the decoder proves nothing here"
        );
        for c in "alpha".chars() {
            tap(&mut a, tui_render::Key::Char(c));
        }

        // ── the key itself, in the dialect his tmux emits ────────────────
        deliver(&mut a, b"\x1b[27;2;13~");
        for c in "bravo".chars() {
            tap(&mut a, tui_render::Key::Char(c));
        }
        assert_eq!(
            a.vs.composer.buf.text(),
            "alpha\nbravo",
            "shift-enter did not put a newline in the draft"
        );
        assert_eq!(
            a.vs.composer.buf.lines().len(),
            2,
            "the newline did not divide the draft into two lines"
        );
        // …and nothing left the process. `IS-33` was this key SENDING.
        assert_eq!(a.queue_len(), 0, "shift-enter queued or sent the draft");
        assert!(
            !a.vs.composer.buf.text().is_empty(),
            "the draft was consumed, which is what a send looks like from here"
        );

        // ── AND THE OTHER DIALECT AGREES ─────────────────────────────────
        // `csi-u`, which is what `extended-keys-format csi-u` would emit. The
        // face must not behave differently depending on a tmux server option,
        // which is the state `IS-33` actually was.
        deliver(&mut a, b"\x1b[13;2u");
        assert_eq!(
            a.vs.composer.buf.lines().len(),
            3,
            "the csi-u spelling of shift-enter did not reach the same act"
        );
    }

    /// **A CLICK OUTSIDE THE BOX DEACTIVATES IT** — *"including on click
    /// anywhere else."*
    ///
    /// The click half was BUILT in round four and had no test of its own; this
    /// one exists because an inherited green is not coverage. It presses the
    /// pointer, not an arrow, and then asserts the typing rule from the region
    /// a click alone put him in.
    ///
    /// FORCED: delete the `Target::region()` call from `App::on_click` and the
    /// mode assertion goes red; put the chat's printable row back and the draft
    /// assertion goes red with the click still working.
    #[tokio::test]
    async fn a_click_in_the_chat_deactivates_the_textarea() {
        let mut a = app();
        in_a_thread(&mut a);
        event(&mut a, "t1", 1, json!({"kind": "user_message", "text": "a row to click"}));
        for c in "seed".chars() {
            tap(&mut a, tui_render::Key::Char(c));
        }
        assert_eq!(a.vs.composer.buf.text(), "seed");

        // A row of the transcript, found through the same hit map the loop
        // uses rather than by guessing a coordinate.
        a.lay(100, 24);
        let row = (0..24u16)
            .find(|r| matches!(a.hits.hit(5, *r), tui_render::Target::Block(_)))
            .expect("no transcript row on the frame to click");
        a.on_click(5, row);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Chat),
            "a click in the transcript left the selection in the box"
        );
        for c in "jx".chars() {
            tap(&mut a, tui_render::Key::Char(c));
        }
        assert_eq!(
            a.vs.composer.buf.text(),
            "seed",
            "the box was still taking keys after a click moved the selection out of it"
        );
    }

    /// **AN INACTIVE BOX DRAWS NO CARET** — *"I can see the cursor is still
    /// there."*
    ///
    /// The caret is a REVERSED CELL painted into the composer's row, so the
    /// only thing that can see it is the frame. This compares the composer's
    /// own rows between the two regions with everything else held still, which
    /// is the trap `0100`'s mutation 15 fell into and fixed: comparing WHOLE
    /// frames would have passed on the footer changing.
    ///
    /// FORCED: drop the `live &&` from `render::composer` and the second
    /// assertion goes red; invert it to `!live &&` and the first does.
    #[tokio::test]
    async fn the_caret_is_drawn_only_while_the_box_holds_the_selection() {
        let mut a = app();
        in_a_thread(&mut a);
        for c in "seed".chars() {
            tap(&mut a, tui_render::Key::Char(c));
        }
        // The composer's OWN rows, located through `bands` — the one
        // derivation of how a frame divides — so this cannot drift with the
        // band heights. `(reversed cells, text)`.
        let box_rows = |a: &App| -> (usize, String) {
            let b = tui_render::bands(&a.vs, 100, 24);
            let rows = tui_render::frame(&a.vs, Utc::now(), 100, 24);
            let first = b.header + b.banner + b.content;
            let mut reversed = 0usize;
            let mut text = String::new();
            for line in rows.iter().skip(first).take(b.composer) {
                for s in &line.spans {
                    if s.style
                        .add_modifier
                        .contains(ratatui::style::Modifier::REVERSED)
                    {
                        reversed += s.content.chars().count();
                    }
                    text.push_str(s.content.as_ref());
                }
            }
            (reversed, text)
        };
        // The witness: the caret is genuinely THERE first, and it is the
        // REVERSED cell the `caret` helper paints, not a label that a rename
        // could move.
        let (live, live_text) = box_rows(&a);
        assert_eq!(
            live, 1,
            "the box drew no caret while it held the selection: {live_text:?}"
        );

        tap(&mut a, tui_render::Key::Up);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Chat));
        let (dark, dark_text) = box_rows(&a);
        assert_eq!(
            dark, 0,
            "the box drew a live caret with the selection in the chat: {dark_text:?}"
        );
        // And the draft is still on screen — the caret went, the text did not.
        assert!(
            dark_text.contains("seed"),
            "the draft vanished with the caret: {dark_text:?}"
        );
    }

    /// **`opt-↑`/`opt-↓` WALK HIS OWN MESSAGES, AND PAST THE LAST ONE IT IS
    /// THE LIVE STATE.**
    ///
    /// *"let's make opt-{up,down} jump to the next/previous user message, and
    /// opt-down to the bottom should go to the live state of the convo."*
    ///
    /// The second act is the one a screenshot cannot check: re-entering live
    /// FOLLOW is not the same as scrolling to the last row, and only `follow`
    /// and `new_since` can tell them apart.
    ///
    /// FORCED: make the `None if forward` arm set `from_bottom = 0` instead of
    /// dispatching `Intent::Bottom` and the last two assertions go red while
    /// the screen looks identical.
    #[tokio::test]
    async fn opt_down_past_his_last_message_re_enters_live_follow() {
        let mut a = app();
        in_a_thread(&mut a);
        let rows: Vec<crate::store::LedgerRow> = Vec::new();
        a.store.load_transcript("t1", rows, 1);
        for seq in 1..=40u64 {
            let who = if seq % 10 == 0 {
                "user_message"
            } else {
                "assistant_text"
            };
            event(&mut a, "t1", seq, json!({"kind": who, "text": format!("line {seq}")}));
        }
        a.lay(100, 24);
        a.project();
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Chat));

        // Walk BACK through his messages. Each jump freezes the view and puts
        // one of his messages at the top of the band.
        tap(&mut a, tui_render::Key::AltUp);
        let first = a.vs.viewport.from_bottom;
        assert!(first > 0, "opt-↑ did not move the viewport at all");
        assert!(!a.vs.viewport.follow, "a jump by authorship left follow armed");
        tap(&mut a, tui_render::Key::AltUp);
        assert!(
            a.vs.viewport.from_bottom > first,
            "the second opt-↑ did not reach an EARLIER message of his"
        );

        // …and forward again, past the last one, which is the live state.
        for _ in 0..8 {
            tap(&mut a, tui_render::Key::AltDown);
        }
        assert_eq!(a.vs.viewport.from_bottom, 0, "opt-↓ never reached the bottom");
        assert!(
            a.vs.viewport.follow,
            "opt-↓ past the last message scrolled to the bottom instead of \
             re-entering live follow"
        );
        assert_eq!(a.vs.viewport.new_since, 0, "the view is at the bottom and still counting arrivals");
    }

    /// **A LIVE ROW THAT RELOCATES MUST NOT YANK HIS TEXT** — and the
    /// block-COUNT freeze could not hold one still.
    ///
    /// > *when my scroll isn't at the bottom of the convo, then new messages
    /// > should not scroll me down. Only if I'm at the bottom.*
    ///
    /// A `task_progress` beat does not APPEND: the projection holes the task's
    /// old slot and pushes a new one at the end, so `blocks.len()` never
    /// changes. The old freeze derived `new_since` from that length, so it
    /// reported ZERO arrivals while every row below the relocation slid up
    /// one — his text moving under `↓ 0 new`. The freeze is a LEDGER SEQ now:
    /// a row at or below it updates in place and nothing above it moves.
    ///
    /// FORCED: make `store::held_by` return `false` and the frozen assertion
    /// goes red. The POSITIVE WITNESS is the second half — while he is at the
    /// bottom the row still relocates, so this cannot pass by the projection
    /// having quietly stopped moving anything at all.
    #[tokio::test]
    async fn a_relocating_live_row_does_not_move_his_text_while_he_is_reading_above_it() {
        // The CONTENT band's painted rows, at the size the fixture laid out —
        // the header carries a live link age and the footer a notice, and
        // neither is what this test is about.
        let content = |a: &App| -> Vec<String> {
            let b = tui_render::bands(&a.vs, 100, 20);
            tui_render::frame(&a.vs, Utc::now(), 100, 20)
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .skip(b.header + b.banner)
                .take(b.content)
                .collect()
        };
        let beat = |a: &mut App, seq: u64| {
            event(
                a,
                "t1",
                seq,
                json!({"kind": "task_progress", "task_id": "T1",
                       "description": "building step 3", "total_tokens": seq * 100}),
            );
        };

        let mut a = app();
        in_a_thread(&mut a);
        event(&mut a, "t1", 1, json!({"kind": "user_message", "text": "the first thing I said"}));
        beat(&mut a, 2);
        for i in 3..14u64 {
            event(&mut a, "t1", i, json!({"kind": "user_message", "text": format!("line {i}")}));
        }
        a.lay(100, 20);

        // ── HE SCROLLS UP. The view freezes where it stands. ──────────────
        // **SCROLL UNTIL HIS OWN WORDS ARE IN VIEW, rather than a fixed count
        // of taps.** How many rows a message occupies is a RENDERING decision —
        // `0101` spent whitespace on it and gave every box a blank row and a
        // metadata row — and a fixture that hard-codes five taps does not FAIL
        // when that changes, it quietly scrolls to somewhere else and tests
        // nothing. The assertion below is what says the scroll landed.
        for _ in 0..60 {
            tap(&mut a, tui_render::Key::Up);
            let rows = content(&a);
            if rows.iter().any(|l| l.contains("line 5"))
                && rows.iter().any(|l| l.contains("building step 3"))
            {
                break;
            }
        }
        assert!(a.vs.viewport.from_bottom > 0, "the fixture never left the bottom");
        assert!(!a.vs.viewport.follow);
        // **WHERE HIS OWN WORDS ARE ON THE SCREEN** — which row of the band
        // each one is painted on. That is the fact "arrivals never yank" is
        // about; the live row's own text legitimately changes in place, and a
        // whole-band compare would have called that a yank.
        let his = |rows: &[String]| -> Vec<(usize, String)> {
            rows.iter()
                .enumerate()
                .filter(|(_, l)| l.contains("line ") || l.contains("the first thing I said"))
                .map(|(i, l)| (i, l.clone()))
                .collect()
        };
        let before = content(&a);
        assert!(
            before.iter().any(|l| l.contains("line 5")),
            "the fixture is wrong: his text is not on the screen:\n{}",
            before.join("\n")
        );
        // The live row is ABOVE him, which is the case a count cannot hold.
        let task_at = before
            .iter()
            .position(|l| l.contains("building step 3"))
            .expect("the fixture is wrong: no live row on the screen");

        // ── A BEAT LANDS ON THE TASK ABOVE HIM. ──────────────────────────
        beat(&mut a, 40);
        let after = content(&a);
        assert_eq!(
            his(&before),
            his(&after),
            "his text moved while he was reading above a relocating row"
        );
        assert_eq!(
            after.iter().position(|l| l.contains("building step 3")),
            Some(task_at),
            "the live row relocated out from under him"
        );

        // ── THE WITNESS: THE SAME BEAT, ON THE SAME FIXTURE, AT THE BOTTOM,
        //    DOES MOVE HIS TEXT. ───────────────────────────────────────────
        //
        // Without this the test would pass on a projection that had simply
        // stopped folding tasks at all: if nothing ever moves, "nothing moved"
        // proves nothing. It is a SECOND app rather than the first one thawed,
        // because thawing re-projects and the row has already relocated by the
        // time the "before" could be read.
        let mut b = app();
        in_a_thread(&mut b);
        event(&mut b, "t1", 1, json!({"kind": "user_message", "text": "the first thing I said"}));
        beat(&mut b, 2);
        for i in 3..14u64 {
            event(&mut b, "t1", i, json!({"kind": "user_message", "text": format!("line {i}")}));
        }
        b.lay(100, 20);
        assert!(b.vs.viewport.follow, "the witness is not at the bottom");
        let live = his(&content(&b));
        beat(&mut b, 40);
        assert_ne!(
            live,
            his(&content(&b)),
            "the live row does not move his text even at the bottom — this test proves nothing"
        );
    }

    /// **A THREAD WITH TWO SUBAGENTS AND ONE OF THEIR OWN**, delivered as the
    /// daemon delivers them: a parent stream with `parent_tool_use_id` on
    /// everything a subagent did.
    ///
    /// Seq 1-2 are the operator and the assistant, so the chat has rows of its
    /// own to be measured against. Seq 3 launches `scout`, seq 10 launches
    /// `builder`; 4-8 are the scout's own work (including its own nested
    /// agent at 7) and are exactly what must NOT appear in his chat.
    fn a_thread_with_subagents(a: &mut App) {
        in_a_thread(a);
        event(a, "t1", 1, json!({"kind": "user_message", "text": "scout the raw list"}));
        event(a, "t1", 2, json!({"kind": "assistant_text", "text": "on it"}));
        event(
            a,
            "t1",
            3,
            json!({"kind": "tool_use", "id": "ag1", "name": "Agent",
                   "input": {"subagent_type": "Explore", "description": "scout the raw list",
                             "prompt": "read the raw list projection and report"}}),
        );
        event(a, "t1", 4, json!({"kind": "task_started", "task_id": "T1",
                                 "task_type": "local_agent", "tool_use_id": "ag1"}));
        event(
            a,
            "t1",
            5,
            json!({"kind": "tool_use", "id": "r1", "name": "Read",
                   "input": {"file_path": "/x/y.rs"}, "parent_tool_use_id": "ag1"}),
        );
        event(a, "t1", 6, json!({"kind": "assistant_text", "text": "the projection is here",
                                 "parent_tool_use_id": "ag1"}));
        event(
            a,
            "t1",
            7,
            json!({"kind": "tool_use", "id": "ag2", "name": "Agent",
                   "input": {"subagent_type": "Explore", "description": "sub-scout the daemon",
                             "prompt": "read event.rs"}, "parent_tool_use_id": "ag1"}),
        );
        event(a, "t1", 8, json!({"kind": "task_progress", "task_id": "T1",
                                 "total_tokens": 31000, "tool_uses": 12}));
        event(
            a,
            "t1",
            9,
            json!({"kind": "background_tasks_changed",
                   "tasks": [{"task_id": "T1", "task_type": "local_agent"}]}),
        );
        a.lay(100, 30);
    }

    /// **THE SUBAGENTS ARE OUT OF THE CHAT AND INTO A TREE, AND THE PARENT IS
    /// A ROW OF IT.**
    ///
    /// > *I just want to see the subagents, and their subagents. It looks like
    /// > a file tree… I don't want to see them in the chat area, it takes up
    /// > 90% of the chat area. I only want things having to do with the viewed
    /// > thread in view. Although you can have a single info tile in that view
    /// > per subagent… Basically the text area above should be whichever one
    /// > we've selected, so the parent needs to be an item in that list.*
    ///
    /// Every clause of that paragraph is one assertion here.
    ///
    /// FORCED: delete the `lens.owner … != lens.subject` guard from
    /// `store::project` and the "his chat holds none of the subagent's rows"
    /// assertion goes red — five rows he did not ask for come back. Delete the
    /// parent row from `store::agent_tree` and the row-0 assertion goes red.
    #[tokio::test]
    async fn the_subagents_leave_the_chat_for_a_tree_whose_first_row_is_the_thread() {
        let mut a = app();
        a_thread_with_subagents(&mut a);

        // ── ROW 0 IS THE PARENT, then the subagents, nested. ──────────────
        assert_eq!(
            a.vs.tree.rows.iter().map(|r| (r.depth, r.label.clone())).collect::<Vec<_>>(),
            vec![
                (0, a.vs.tree.rows[0].label.clone()),
                (1, "Explore · scout the raw list".to_string()),
                (2, "Explore · sub-scout the daemon".to_string()),
            ],
            "the tree is not the parent and its subagents, nested"
        );
        assert_eq!(a.vs.tree.rows[0].id, TreeId::Parent);

        // ── HIS CHAT HOLDS NONE OF THEIR ROWS, AND ONE TILE FOR EACH. ─────
        let texts: Vec<String> = a.vs.blocks.iter().map(|b| b.text().to_string()).collect();
        assert!(
            !texts.iter().any(|t| t.contains("y.rs") || t.contains("the projection is here")),
            "a subagent's own rows are still eating his chat area: {texts:?}"
        );
        let tiles: Vec<&String> = texts
            .iter()
            .filter(|t| t.contains("— → opens its chat"))
            .collect();
        assert_eq!(
            tiles.len(),
            1,
            "expected exactly one tile for the one top-level subagent: {texts:?}"
        );
        assert!(
            tiles[0].contains("scout the raw list"),
            "the tile does not name the subagent: {}",
            tiles[0]
        );
        // **THE TILE DOES NOT EXPAND.** Inline expansion into the trail is
        // what ate the chat area; the trail is reached by drilling now.
        let tile = a
            .vs
            .blocks
            .iter()
            .find(|b| b.text().contains("— → opens its chat"))
            .expect("the tile");
        assert!(!tile.is_expandable(), "the tile still expands inline");
        // …and his own two rows are untouched.
        assert!(texts.iter().any(|t| t.contains("scout the raw list")));
        assert!(texts.iter().any(|t| t == "on it"), "{texts:?}");
    }

    /// **THE CHAT AREA HE GETS BACK, MEASURED.**
    ///
    /// > *I don't want to see them in the chat area, it takes up 90% of the
    /// > chat area.*
    ///
    /// `store::chat_area_reclaimed` is the instrument, and this is its test:
    /// on the fixture it must report a real gain and land on the same number
    /// the projection actually produced. Over THIRTY of his real threads it
    /// reports 21,948 chat rows down to 8,526 — 61.2% reclaimed, and 90.8% on
    /// the worst one, which is where his figure came from.
    ///
    /// FORCED: delete the lens guard from `store::project` and `before` and
    /// `after` become equal.
    #[tokio::test]
    async fn the_chat_area_reclaimed_is_measurable_and_agrees_with_the_projection() {
        let mut a = app();
        a_thread_with_subagents(&mut a);
        let (before, after) =
            store::chat_area_reclaimed(&a.store, a.vs.stdout_only, &a.opened, &a.agents);
        assert!(before > after, "nothing left his chat: {before} -> {after}");
        // The measurement counts the PROJECTION's rows; `thread_blocks` puts
        // its own head rules ("showing from seq …") above them.
        let head = a
            .vs
            .blocks
            .iter()
            .take_while(|b| matches!(b, Block::Rule { .. }))
            .count();
        assert_eq!(
            after,
            a.vs.blocks.len() - head,
            "the measurement and the projection disagree about his chat"
        );
    }

    /// **THE FIGURES ARE THE MEASURED ONES, AND AN UNSOURCEABLE ONE IS ABSENT
    /// RATHER THAN ZERO.**
    ///
    /// Running-ness comes from the ROSTER (`background_tasks_changed`), which
    /// is a LEVEL signal — never from unmatched edges, which reported 13
    /// running where the OS had 0. Tokens and calls come from `task_progress`.
    /// The nested subagent has neither a roster entry nor a terminal event, so
    /// it is `no end seen` and its tokens print NOTHING.
    ///
    /// FORCED: make `direct_attribute` derive running-ness from "a
    /// `task_started` with no later `task_notification`" and the second
    /// assertion goes red — the nested agent, which nothing ever started,
    /// stays honest but the ROSTER stops being what decides.
    #[tokio::test]
    async fn the_tree_figures_come_from_the_roster_and_an_absent_one_prints_nothing() {
        let mut a = app();
        a_thread_with_subagents(&mut a);
        let scout = &a.vs.tree.rows[1];
        assert_eq!(scout.state, tui_render::SubagentState::Running);
        assert_eq!(scout.tokens, Some(31_000));
        assert_eq!(scout.calls, Some(12));
        let nested = &a.vs.tree.rows[2];
        assert_eq!(
            nested.state,
            tui_render::SubagentState::Unknown,
            "a subagent with no roster entry and no terminal event was given a state"
        );
        assert_eq!(nested.tokens, None, "an unsourceable figure was invented");
        assert_eq!(nested.bash_in_flight, None);
    }

    /// **THE SUBAGENT BAND IS A STOP ONLY WHEN THERE IS A TREE**, and it is
    /// not a trap when it is.
    ///
    /// FORCED: drop the `rows.len() > 1` guard from `App::stops` and the first
    /// assertion goes red — `↓` lands on a band that is not on the screen.
    #[tokio::test]
    async fn the_subagent_region_is_a_stop_only_when_there_is_a_band() {
        let mut a = app();
        in_a_thread(&mut a);
        assert!(a.vs.tree.rows.is_empty(), "a thread with no subagents drew a band");
        assert_eq!(a.stops(), vec![Region::Chat, Region::Composer]);
        tap(&mut a, tui_render::Key::Down);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Composer),
            "↓ left the box for a subagent band that is not drawn"
        );

        let mut a = app();
        a_thread_with_subagents(&mut a);
        assert_eq!(
            a.stops(),
            vec![Region::Chat, Region::Composer, Region::Agents],
            "the walk cannot reach the subagent band even when it exists"
        );
        tap(&mut a, tui_render::Key::Down);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Agents));
        // **AND IT IS NOT A TRAP**: `esc` comes home, `↑` off row 0 steps back.
        tap(&mut a, tui_render::Key::Esc);
        assert_eq!(a.vs.mode, Mode::Thread(Region::Composer));
        tap(&mut a, tui_render::Key::Down);
        tap(&mut a, tui_render::Key::Up);
        assert_eq!(
            a.vs.mode,
            Mode::Thread(Region::Composer),
            "↑ off the tree's first row did not come back to the box"
        );
        assert_eq!(
            press(&a, tui_render::Key::Right),
            Intent::Compose(tui_render::Edit::Right),
            "something bound → in the box while the band exists"
        );
    }

    /// **THE CURSOR WALKS THE ROWS AND ONLY `→`/`enter` CHANGES WHICH CHAT IS
    /// SHOWN.** Selection and SUBJECT are two different things.
    ///
    /// FORCED: make `App::scroll_key`'s Agents arm call `drill` as it moves —
    /// which is what "the cursor changes the chat" would look like — and the
    /// "browsing did not re-project" assertion goes red.
    #[tokio::test]
    async fn moving_the_tree_cursor_does_not_change_the_chat_but_enter_does() {
        let mut a = app();
        a_thread_with_subagents(&mut a);
        tap(&mut a, tui_render::Key::Down); // into the band, on the parent row
        assert_eq!(a.vs.mode, Mode::Thread(Region::Agents));
        let before: Vec<String> = a.vs.blocks.iter().map(|b| b.text().to_string()).collect();

        tap(&mut a, tui_render::Key::Down); // cursor onto the scout
        assert_eq!(a.vs.tree.cursor, TreeId::Agent("ag1".into()));
        assert_eq!(a.vs.tree.subject, TreeId::Parent, "browsing changed the subject");
        let after: Vec<String> = a.vs.blocks.iter().map(|b| b.text().to_string()).collect();
        assert_eq!(before, after, "browsing the tree re-projected his chat");

        // …and `→` opens that subagent's own history in place of the parent's.
        tap(&mut a, tui_render::Key::Right);
        assert_eq!(a.vs.tree.subject, TreeId::Agent("ag1".into()));
        let drilled: Vec<String> = a.vs.blocks.iter().map(|b| b.text().to_string()).collect();
        assert!(
            drilled.iter().any(|t| t.contains("the projection is here")),
            "the subagent's own chat is not showing: {drilled:?}"
        );
        assert!(
            drilled.iter().any(|t| t.contains("read the raw list projection")),
            "the DIRECT INVOCATION is not the first turn of its chat: {drilled:?}"
        );
        assert!(
            !drilled.iter().any(|t| t == "on it"),
            "the PARENT's rows are still in the subagent's chat: {drilled:?}"
        );
        // Its own nested agent is a tile in ITS chat — the same mechanism, one
        // level down.
        assert!(
            drilled.iter().any(|t| t.contains("— → opens its chat") && t.contains("sub-scout")),
            "the nested agent is not a tile in its parent's chat: {drilled:?}"
        );
    }

    /// **THE COMPOSER FOLLOWS THE SELECTION, AND IT SAYS SO WHEN IT CANNOT
    /// SEND.**
    ///
    /// A `direct` subagent runs inside Claude Code and has no inbox. His own
    /// words this round: *"We currently can't submit a message into claude
    /// that way, we would have to interrupt."* So the band is replaced by two
    /// sentences, `editable` goes false, `enter` and every printable answer
    /// with the notice, **and his draft survives untouched**.
    ///
    /// FORCED: delete the `composer_is_stood_in` guard from `App::do_send` and
    /// the "no send was issued" assertion goes red — his words go to the
    /// PARENT thread while the screen says nothing can be sent.
    #[tokio::test]
    async fn the_composer_against_a_subagent_refuses_rather_than_looking_live() {
        let mut a = app();
        a_thread_with_subagents(&mut a);
        // A half-written draft, which is the thing most at risk.
        for c in "half a message".chars() {
            a.on_intent(Intent::Type(c));
        }
        assert!(a.vs.composer.editable, "the parent's own box is not editable");

        tap(&mut a, tui_render::Key::Down);
        tap(&mut a, tui_render::Key::Down);
        tap(&mut a, tui_render::Key::Right);
        assert_eq!(a.vs.tree.subject, TreeId::Agent("ag1".into()));

        // ── THE BAND SAYS SO RATHER THAN LOOKING LIVE ────────────────────
        assert!(!a.vs.composer.editable, "the box looks live against a subagent");
        let stood = a.vs.composer.stand_in.as_ref().expect("the stand-in");
        assert!(stood.what.contains("scout the raw list"));
        let frame = painted(&a);
        assert!(
            frame.contains("nothing can be sent to it"),
            "the band does not say why there is no box:\n{frame}"
        );

        // ── NOTHING IS SENT AND NOTHING IS TYPED ─────────────────────────
        //
        // **THE OBSERVABLE IS WHICH REFUSAL CAME BACK**, and that is not a
        // weaker check than "no call went out": `do_send` is a funnel with
        // four refusals in a fixed order, and the stand-in's is FIRST. Remove
        // the guard and the notice becomes the transport's — a different
        // string — because the send got past this gate and was stopped by a
        // dead link instead. A test that only asserted "nothing was sent"
        // would stay green with the guard deleted, since nothing is sent in a
        // test either way. This one cannot.
        a.on_intent(Intent::Send);
        assert!(
            a.vs.notice.as_ref().is_some_and(|n| n.text.contains("nothing can be sent")),
            "the send was refused by something other than the stand-in: {:?}",
            a.vs.notice.as_ref().map(|n| n.text.clone())
        );
        assert!(a.sending.is_none(), "a send is in flight against a subagent");
        a.on_intent(Intent::Type('z'));
        assert_eq!(
            a.vs.composer.buf.text(),
            "half a message",
            "a key aimed at a subagent was typed into the parent's draft"
        );
        assert!(
            a.vs.notice.as_ref().is_some_and(|n| n.text.contains("nothing can be sent")),
            "the refusal was silent"
        );

        // ── `←` UN-DRILLS ONE LEVEL AND THE DRAFT COMES BACK ─────────────
        tap(&mut a, tui_render::Key::Esc);
        tap(&mut a, tui_render::Key::Left);
        assert_eq!(a.vs.tree.subject, TreeId::Parent);
        assert!(matches!(a.vs.mode, Mode::Thread(_)), "one ← left the thread as well");
        assert!(a.vs.composer.editable);
        assert_eq!(a.vs.composer.buf.text(), "half a message", "his draft did not come back");
    }

    /// **`opt-{up,down}` IN THE MAIN MENU JUMPS TO AND BETWEEN BAND HEADERS** —
    /// the THIRD meaning of the pair, and the third region resolution.
    ///
    /// FORCED: make `band_header_from` match `RootItem::Project` instead of
    /// `Band` and the "lands on a header" assertion goes red.
    #[tokio::test]
    async fn opt_arrows_at_the_root_jump_between_band_headers() {
        let mut a = app();
        a.store
            .adopt_projects(vec![proj("airbnb", &["x"]), proj("harness", &["a", "b"])]);
        a.project();
        a.lay(100, 24);
        let headers: Vec<usize> = (0..a.vs.projects.selectable())
            .filter(|i| matches!(a.vs.projects.item(*i), Some(RootItem::Band(_))))
            .collect();
        assert!(
            headers.len() >= 2,
            "the fixture has fewer than two band headers: {headers:?}"
        );
        a.on_intent(Intent::Select(headers[0]));
        tap(&mut a, tui_render::Key::AltDown);
        assert_eq!(
            a.project_cursor, headers[1],
            "opt-↓ did not land on the NEXT band header"
        );
        tap(&mut a, tui_render::Key::AltUp);
        assert_eq!(a.project_cursor, headers[0]);
        // Past the first there is no header above, so it stays put rather than
        // wrapping — a cursor that wrapped would be a jump he did not ask for.
        tap(&mut a, tui_render::Key::AltUp);
        assert_eq!(a.project_cursor, headers[0]);
    }

    // ────────────────────────────────────────────────────────────────────
    //  THE `DIR-16` SWEEP MACHINE
    // ────────────────────────────────────────────────────────────────────

    /// **EVERY MODE THIS FACE CAN BE IN, AND HOW TO GET THERE.**
    ///
    /// The `match` is EXHAUSTIVE and that is the whole point: adding a
    /// `Mode` will not compile until this function says how the sweep
    /// reaches it. A list of surfaces goes stale in silence; a match does
    /// not.
    ///
    /// `None` means the mode cannot be reached from a fixture — say so here,
    /// out loud, rather than leaving it off a list.
    fn drive_to(app: &mut App, mode: Mode) -> Option<()> {
        // Always start from the root with the fixture loaded.
        match mode {
            Mode::Projects => {}
            Mode::List => app.open_project(0),
            Mode::Thread(_) => {
                app.open_project(0);
                app.on_intent(Intent::Select(0));
                app.on_intent(Intent::Open);
            }
            Mode::Activity => app.on_intent(Intent::SwapView),
            Mode::Help => app.on_intent(Intent::Help),
            Mode::Queue => {
                app.open_project(0);
                app.on_intent(Intent::Select(0));
                app.on_intent(Intent::Open);
                app.on_intent(Intent::QueueOverlay);
            }
            Mode::Sheet => {
                app.open_project(0);
                open_options(app, 0);
            }
            // **`compact`, NOT `rename`.** Both open a field; `rename` is
            // AUI-owned and this `App` has no 9010 client, so its row is
            // muted and activating it says so instead of opening anything.
            // `compact` is DAEMON-owned, so bringing the link up is enough.
            // FOUND by position rather than hard-coded: the sheet's act set
            // grew a row in `0076` and an index would have gone stale.
            Mode::SheetField => {
                app.store.link = LinkState::Connected;
                app.open_project(0);
                app.project();
                open_options(app, 0);
                let i = app
                    .vs
                    .sheet
                    .as_ref()?
                    .rows
                    .iter()
                    .position(|r| r.act == SheetAct::Compact)?;
                app.on_intent(Intent::SheetAct(i));
            }
            // Mic mode needs a thread open and a blank composer, both of
            // which the fixture has.
            Mode::Mic => {
                app.open_project(0);
                app.on_intent(Intent::Select(0));
                app.on_intent(Intent::Open);
                app.on_intent(Intent::MicMode);
            }
            Mode::Archived => app.open_archived(),
            // **A BAND'S OWN PAGE, THROUGH ITS REAL DOOR** — `→` on the band
            // header, resolved through `ProjectList::item`, so the sweep drives
            // it the way he does rather than by assigning the mode. The fixture
            // has a recent band whichever band the sweep asked for, and the
            // page is reached for whichever `b` the `Mode` carries.
            Mode::SeeAll(b) => {
                app.project();
                let i = (0..app.vs.projects.selectable())
                    .find(|&i| app.vs.projects.item(i) == Some(RootItem::Band(b)))?;
                app.on_intent(Intent::Select(i));
                app.on_intent(Intent::Open);
            }
            // **THE FIELD IS OPENED DIRECTLY, and that is said rather than
            // hidden.** Its door — the root's `+ new project` row — is
            // AUI-owned, and this `App` has no 9010 client, so the door
            // correctly refuses and says why. The sweep is about what a
            // SURFACE DRAWS, not about whether its door opens; the door has
            // its own test. Reaching it any other way would have meant
            // leaving a whole mode off the sweep to keep the fixture tidy,
            // which is the class of hole this machine exists to close.
            Mode::NewProject => {
                app.new_project = Some(tui_render::compose::Composer::new());
                app.vs.mode = Mode::NewProject;
            }
            // **THE LITTLE RENAME MODAL, OPENED DIRECTLY — `Mode::NewProject`'s
            // reason, said out loud rather than hidden.** Its door is bare `r`,
            // which routes through the sheet's rename row, and rename is
            // AUI-OWNED: with no 9010 client that row is correctly muted and
            // opens nothing, exactly as `Mode::SheetField`'s note above says.
            // The sweep is about what a SURFACE DRAWS; the door has its own
            // test, against a fake 9010
            // (`tests/it/rows_registry_and_rename.rs`).
            //
            // Everything else is the real state: `open_options` mints the
            // `SheetState` with the summary already in its `rename` buffer, so
            // the box the sweep renders is the box he gets.
            Mode::RenameModal => {
                app.open_project(0);
                app.project();
                open_options(app, 0);
                let i = app
                    .vs
                    .sheet
                    .as_ref()?
                    .rows
                    .iter()
                    .position(|r| r.act == SheetAct::Rename)?;
                if let Some(s) = app.sheet.as_mut() {
                    s.cursor = i;
                    s.editing = Some(SheetAct::Rename);
                }
                app.vs.mode = Mode::RenameModal;
            }
        }
        app.project();
        (app.vs.mode == mode).then_some(())
    }

    /// **NO HASH REACHES THE GLASS OR THE CLIPBOARD, ON ANY SURFACE.**
    ///
    /// `DIR-16`, his own words: *"don't show me thread or task hash names. not
    /// useful to me."*
    ///
    /// # Why this exists, and why it is not another list
    ///
    /// `store::tests::no_row_prints_a_hash_it_was_handed` calls itself *"A
    /// SWEEP rather than a list of cases"* and greps exactly TWO projections —
    /// the transcript and the event lane. `0073` measured what that cost: a
    /// hex id was on 38 of 38 rows of his real project list, in the shipped
    /// tree, and the whole suite returned green over it. The list header
    /// carried a working directory, the thread header carried `5f4413b1`, and
    /// the queue overlay carried one too — **four live violations of one
    /// directive, under a guard that names it.**
    ///
    /// The lesson `0073` drew is the specification for this test: *a directive
    /// scoped "wherever a row prints X" needs a MACHINE that enumerates row
    /// builders, because a human sweep stops at the surface it was dispatched
    /// about.* So this does not enumerate BUILDERS — a builder can be added
    /// without anyone noticing — it enumerates MODES, exhaustively and under
    /// the compiler's eye ([`drive_to`]), renders each one as a FRAME, and
    /// greps what was actually drawn. Whatever builder put a hash there, it
    /// lands here.
    ///
    /// **AND THE CLIPBOARD IS SWEPT TOO**, because a copied run is a surface
    /// he reads and it is not on the glass: `select::thread_run` carried the
    /// eight-character id by design until `0076` (`DEC-27`), so a frame-only
    /// sweep would have been green over the one place the id survived.
    ///
    /// # The witness, and why the absence alone would prove nothing
    ///
    /// A face that died on startup draws a blank pane, and a blank pane
    /// contains no hex id — it would score a triumphant pass. So every mode
    /// must be POSITIVELY RIGHT before its absence means anything: the frame
    /// has to be the mode it was driven to, it has to carry ink, and it has to
    /// name the thread the way `Store::label_of` names it. The witness is
    /// keyed on the FIXTURE'S SUMMARY, which no mutation that puts an id back
    /// can move — a witness that looked for the id's absence would be the
    /// check under test wearing a second hat.
    #[tokio::test]
    async fn no_surface_of_this_face_prints_a_hash_on_the_glass_or_the_clipboard() {
        // **EIGHT CHARACTERS EACH, DELIBERATELY** — the same trap
        // `store`'s sweep names: a longer fixture id would let a leaked hash
        // pass by being cut one character short of what this greps for.
        const HASHES: [&str; 6] = [
            "thrdhash", "scndhash", "taskhash", "agnthash", "toolhash", "othrhash",
        ];
        // **THE NAME MUST NOT CONTAIN THE HASH.** `proj`'s default summary is
        // `thread <id>`, which would have made this test grep for a string its
        // own witness contains — the check passing on its own fixture rather
        // than on the face. Caught on the first run of this sweep.
        const NAME: &str = "the arc, round four";
        // Wide and tall: a crop is not an answer, and a hash that only shows
        // at 200 columns is still a hash he can see.
        const W: u16 = 200;
        const H: u16 = 60;

        let modes = [
            Mode::Projects,
            Mode::List,
            Mode::Thread(Region::Composer),
            Mode::Activity,
            Mode::Help,
            Mode::Queue,
            Mode::Sheet,
            Mode::SheetField,
            Mode::Mic,
            Mode::Archived,
            Mode::NewProject,
        ];
        let mut reached = 0usize;
        for mode in modes {
            let mut app = app();
            let mut harness = proj("harness", &HASHES[..2]);
            for (i, t) in harness.threads.iter_mut().enumerate() {
                t.summary = format!("{NAME} — {i}");
            }
            let mut airbnb = proj("airbnb", &["othrhash"]);
            airbnb.threads[0].summary = "the other project's thread".into();
            app.store.adopt_projects(vec![harness, airbnb]);
            app.project();
            // **EVERY HASH-CARRYING EVENT SHAPE**, so the transcript and the
            // event lane have rows to draw — an empty lane proves nothing, and
            // these are the two projections the OLD sweep covered, kept inside
            // the new one rather than left beside it.
            for (seq, e) in [
                json!({"kind":"task_started","task_id":"taskhash",
                       "description":"diagnose the lane","subagent_type":"claude"}),
                json!({"kind":"task_progress","task_id":"taskhash","last_tool_name":"Grep"}),
                json!({"kind":"task_updated","task_id":"taskhash","status":"running"}),
                json!({"kind":"task_notification","task_id":"taskhash","status":"completed",
                       "summary":"found it"}),
                json!({"kind":"agent_launched","agent_id":"agnthash","status":"ok",
                       "is_async":true}),
                json!({"kind":"agent_completed","agent_id":"agnthash"}),
                json!({"kind":"tool_use","id":"toolhash","name":"Grep",
                       "input":{"pattern":"fold_of"}}),
                json!({"kind":"assistant_text","text":"the lane says something"}),
            ]
            .into_iter()
            .enumerate()
            {
                event(&mut app, HASHES[0], seq as u64 + 1, e);
            }
            app.project();
            let Some(()) = drive_to(&mut app, mode) else {
                // Not reachable from a fixture — and it is SAID, not skipped
                // silently. Today nothing lands here.
                panic!("the sweep could not reach {mode:?}; it must say how, or say why not");
            };
            reached += 1;
            app.lay(W, H);
            let lines: Vec<String> = tui_render::frame(app.state(), Utc::now(), W, H)
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect();
            let glass = lines.join("\n");

            // ── THE WITNESS, BEFORE THE ABSENCE MEANS ANYTHING ───────────
            assert_eq!(app.vs.mode, mode, "drove to the wrong surface");
            assert!(
                glass.chars().any(|c| !c.is_whitespace()),
                "{mode:?} drew an empty frame — an absence proved on a dead \
                 face is not evidence"
            );
            // Every one of these surfaces can name the thread, and the two
            // that cannot are named here rather than excused by a wildcard.
            match mode {
                // The root, the list and the sheet all draw the thread's row
                // or its title; the thread, the queue and mic all draw its
                // header. The archived room and `+ new project` are about
                // PROJECTS and have no thread on them, and the `?` overlay
                // draws a key table over whatever is beneath.
                Mode::Archived | Mode::NewProject | Mode::Help => {}
                _ => assert!(
                    glass.contains(NAME),
                    "{mode:?} does not name the thread at all, so its lack of \
                     a hash says nothing:\n{glass}"
                ),
            }

            // ── AND NOW THE ABSENCE ──────────────────────────────────────
            for hash in HASHES {
                assert!(
                    !glass.contains(hash),
                    "`{hash}` reached the glass in {mode:?}:\n{glass}"
                );
            }

            // ── THE CLIPBOARD, WHICH IS NOT ON THE GLASS ─────────────────
            //
            // **ONE DRAG ACROSS THE WHOLE FRAME**, resolved by the same
            // `selected_text` a real gesture goes through — so what is
            // asserted here is what would actually land on his clipboard, not
            // a second derivation of it. A row's LOGICAL text can be richer
            // than its painted characters (that is the whole of `DEC-27`), so
            // grepping the glass would not have covered this.
            let texts = app.hits.texts();
            let copied = match (texts.first(), texts.last()) {
                (Some(a), Some(b)) => {
                    let end = tui_render::select::TextPos::new(b.unit, usize::MAX / 2);
                    tui_render::select::selected_text(
                        app.state(),
                        &texts,
                        tui_render::select::Selection::new(
                            tui_render::select::TextPos::new(a.unit, 0),
                            end,
                        ),
                    )
                }
                _ => panic!("{mode:?} laid out no selectable text at all"),
            };
            for hash in HASHES {
                assert!(
                    !copied.contains(hash),
                    "`{hash}` reached the clipboard in {mode:?}:\n{copied}"
                );
            }
        }
        assert_eq!(
            reached,
            modes.len(),
            "the sweep skipped a surface without saying so"
        );
    }


    // ── R9.UI1: the operator's two subagent rulings, at this face ─────────

    /// The root menu after one `Fleet` frame (state + lineage + rollup) and one
    /// `Threads` frame (when he last spoke), over an index holding `p1` and
    /// `kid-idx`.
    fn root_after(statuses: Vec<Value>, spoke: &[&str]) -> App {
        let mut a = app();
        a.store.adopt_projects(vec![proj("harness", &["p1", "kid-idx"])]);
        a.on_transport(TransportEvent::Fleet { statuses, capped: false });
        a.on_transport(TransportEvent::Threads {
            rows: spoke
                .iter()
                .map(|id| json!({"thread_id": id, "cwd": "/Users/x/minds/harness", "model": "sonnet",
                                 "account": "acct", "last_user_input_at": "2026-10-18T00:00:00Z",
                                 // TUI-P1a: threads his face started, so the
                                 // only thing that can hide one is the rule
                                 // under test.
                                 "identity": true, "app_id": "harness-tui",
                                 "title": format!("title {id}")}))
                .collect(),
            capped: false,
        });
        a.project();
        a
    }

    fn recent_ids(a: &App) -> Vec<String> {
        a.vs.projects.recent.iter().map(|r| r.id.clone()).collect()
    }

    /// **UI-a: THE RECENT BAND LEAVES OUT EVERY SUBAGENT** — one this face
    /// merges in from a project index (`kid-idx`) and one it adds from the
    /// daemon's rows (`kid-d`), both of which the daemon says have a parent.
    /// The roots beside them (`p1` from the index, `root-d` from the daemon)
    /// stay, so a band that dropped everything goes red too.
    ///
    /// FORCED RED twice: `in_recents` without its `hidden_from_recents` check
    /// (kid-idx returns), and `daemon_only_recent` without it (kid-d returns).
    #[tokio::test]
    async fn ui_a_the_recent_band_leaves_out_subagents_from_the_index_and_the_daemon() {
        let a = root_after(
            vec![
                json!({"thread_id": "p1", "state": "idle", "executing": false}),
                json!({"thread_id": "kid-idx", "state": "idle", "executing": false, "parent_id": "p1"}),
                json!({"thread_id": "kid-d", "state": "idle", "executing": false, "parent_id": "p1"}),
                json!({"thread_id": "root-d", "state": "idle", "executing": false}),
            ],
            &["kid-d", "root-d"],
        );
        let ids = recent_ids(&a);
        assert!(ids.contains(&"p1".to_string()) && ids.contains(&"root-d".to_string()), "{ids:?}");
        assert!(!ids.contains(&"kid-idx".to_string()), "an indexed subagent: {ids:?}");
        assert!(!ids.contains(&"kid-d".to_string()), "a daemon-only subagent: {ids:?}");
    }

    /// **UI-b ON THE ROOT MENU'S ROWS** — the recent band's row and the
    /// project page's row draw the ROLLED-UP word. `p1` is idle with a running
    /// subtree (the daemon's `rolled.status: active`), `root-d` is itself
    /// parked (`rolled.status: error`), `kid-idx` has nothing below it and
    /// keeps its own `idle`. (TUI-P1c: the fixture was the mirror's retired
    /// `rollup` word, which no frame carries any more; it is the `rolled` cell.)
    ///
    /// FORCED RED by `row_of`/`daemon_only_recent` reading `f.state` instead
    /// of `Store::shown_word`.
    #[tokio::test]
    async fn ui_b_root_menu_rows_draw_the_subtree_rollup() {
        let mut a = root_after(
            vec![
                with_rolled(json!({"thread_id": "p1", "state": "idle", "executing": false}), "active"),
                with_rolled(json!({"thread_id": "kid-idx", "state": "idle", "executing": false}), "idle"),
                with_rolled(json!({"thread_id": "root-d", "state": "parked", "executing": false}), "error"),
            ],
            &["root-d"],
        );
        let st = |a: &App, id: &str| {
            a.vs.projects.recent.iter().find(|r| r.id == id).map(|r| r.state.clone())
        };
        assert_eq!(st(&a, "p1").as_deref(), Some("active"), "rule 2 on a recent row");
        assert_eq!(st(&a, "root-d").as_deref(), Some("parked"), "its own error on a daemon-only row");
        assert_eq!(st(&a, "kid-idx").as_deref(), Some("idle"), "rule 4: own word");
        let s = screen(&a);
        assert!(s.lines().any(|l| l.contains("thread p1") && l.contains("active")), "{s}");
        // The project page (one project's thread list) draws the same word.
        a.store.enter_project(0);
        let list = crate::store::thread_list(&a.store, 0, 0);
        let row = list.rows.iter().find(|r| r.id == "p1").map(|r| r.state.clone());
        assert_eq!(row.as_deref(), Some("active"), "the project page's row");
    }

    /// **UI-b IN THE THREAD HEADER** — an open thread whose own state is
    /// `idle` and whose subtree the daemon rolls up `active` says `active`;
    /// the daemon moving it to `idle` makes it say `idle`; a thread that is
    /// itself parked says `error: parked`. (TUI-P1c: the `rolled` cell, not
    /// the retired `rollup` word; an error no longer rolls up — R9.TS1.)
    ///
    /// FORCED RED by `state_word` reading `f.state`.
    #[tokio::test]
    async fn ui_b_the_thread_header_says_the_subtree_rollup() {
        let mut a = app();
        in_a_thread(&mut a);
        let frame = |state: &str, rolled: &str| TransportEvent::Fleet {
            statuses: vec![with_rolled(json!({"thread_id": "t1", "state": state, "executing": false}), rolled)],
            capped: false,
        };
        a.on_transport(frame("idle", "active"));
        a.project();
        assert!(a.vs.header.title.ends_with("· active"), "{}", a.vs.header.title);
        a.on_transport(frame("idle", "idle"));
        a.project();
        assert!(a.vs.header.title.ends_with("· idle"), "{}", a.vs.header.title);
        // TUI-P1a: in his words — `parked` is the error state.
        a.on_transport(frame("parked", "error"));
        a.project();
        assert!(a.vs.header.title.ends_with("· error: parked"), "{}", a.vs.header.title);
    }

    // ── R9.UI1c: UI-a / UI-b read off the RENDERED root menu ──────────────
    //
    // R9.UI1b's TUI tests read `Store` values; only shapewire's table caught a
    // precedence flip (inspect N1). These read what `tui_render::frame` draws
    // for a recent-band row: the cell's text AND its foreground colour.

    /// `v` with the daemon's `rolled` cell (R9.TS1) whose `status` is `status`:
    /// `active`, `idle`, or `error` (a parked thread's own error).
    fn with_rolled(mut v: Value, status: &str) -> Value {
        let display = if status == "error" { "error: parked" } else { status };
        v["rolled"] = json!({"status": status, "display": display, "active_descendants": 0,
                             "error": if status == "error" { json!("parked") } else { Value::Null }});
        v
    }

    /// A status row for the root menu: `kids` are the thread's managed runs,
    /// `owed` the subset still outstanding; `parent` is its `parent_id`.
    fn ui1c(id: &str, state: &str, exec: bool, parent: Option<&str>, kids: &[&str], owed: &[&str]) -> Value {
        let mut v = json!({"thread_id": id, "state": state, "executing": exec,
                           "queued": 0, "unread": 0});
        if let Some(p) = parent {
            v["parent_id"] = json!(p);
        }
        if !kids.is_empty() {
            v["children"] = json!({
                "outstanding": owed,
                "runs": kids.iter().map(|k| json!({"id": k, "wake": "end",
                    "started_at": "2026-10-19T08:00:00Z", "finished": null}))
                    .collect::<Vec<_>>(),
            });
        }
        v
    }

    /// Every span on the first rendered line containing `needle`, as
    /// `(trimmed text, fg)`. Panics when no line contains it.
    fn drawn_row(a: &App, needle: &str) -> Vec<(String, Option<ratatui::style::Color>)> {
        let lines = tui_render::frame(&a.vs, Utc::now(), 120, 30);
        let line = lines
            .iter()
            .find(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().contains(needle))
            .unwrap_or_else(|| panic!("no rendered line holds {needle:?}:\n{}", screen(a)));
        line.spans
            .iter()
            .map(|s| (s.content.trim().to_string(), s.style.fg))
            .collect()
    }

    /// Asserts the `thread p1` recent row draws a cell whose text satisfies
    /// `text` in exactly `sev`'s colour.
    fn p1_cell(a: &App, text: impl Fn(&str) -> bool, sev: Severity, why: &str) {
        let row = drawn_row(a, "thread p1");
        assert!(
            row.iter().any(|(t, fg)| text(t) && *fg == sev.style().fg),
            "{why}: want a {sev:?} ({:?}) state cell, drew {row:?}",
            sev.style().fg
        );
    }

    /// **OWN PARKED + A DESCENDANT ACTIVE DRAWS `active`, CALM** (UI-b rule 2
    /// over rule 3; inspect N1's missing TUI case). `p1`'s own turn is parked;
    /// its child runs. Opposed: the child idle, and the same row draws
    /// `parked` in amber — so a constant word or colour fails one arm.
    ///
    /// FORCED RED by `shown_word` short-circuiting an own-`parked` thread to
    /// its own word, and by `shapewire::subtree::rollup` returning `Own` for an
    /// own-parked thread before rule 2.
    #[tokio::test]
    async fn ui_c_rendered_recent_row_own_parked_with_an_active_child_is_active() {
        let a = root_after(
            vec![
                with_rolled(ui1c("p1", "parked", false, None, &["c-run"], &[]), "active"),
                ui1c("c-run", "active", true, Some("p1"), &[], &[]),
            ],
            &[],
        );
        p1_cell(&a, |t| t == "active", Severity::Calm, "own parked, child active");
        let a = root_after(
            vec![
                with_rolled(ui1c("p1", "parked", false, None, &["c-run"], &[]), "error"),
                ui1c("c-run", "idle", false, Some("p1"), &[], &[]),
            ],
            &[],
        );
        p1_cell(&a, |t| t == "parked", Severity::Warn, "own parked, nothing active below");
    }

    /// **OWN IDLE + ONE CHILD PARKED + ANOTHER ACTIVE DRAWS ACTIVE**, on both
    /// render paths of the state cell: the WORD (no child owed) and the
    /// activity PHRASE (both owed: `⋯ waiting 2 · ◆1`, whose colour is the
    /// subtree's). Opposed on each path: the running child replaced by a
    /// queued one, and the row turns amber.
    ///
    /// FORCED RED by the rollup putting rule 3 (parked) before rule 2.
    ///
    /// TUI-P1c: the subtree's word is the daemon's `rolled` (fixtures carry
    /// it), and under R9.TS1 an error does NOT roll up — the second arm's
    /// parent (nothing active below, a parked child) is `idle`, muted on the
    /// word path; on the phrase path it stays calm (the phrase still carries
    /// `◆1`). The amber arm this test had is now the daemon's to decide.
    #[tokio::test]
    async fn ui_c_rendered_recent_row_idle_with_a_parked_and_an_active_child_is_active() {
        for (owed, word) in [(&[][..], true), (&["c-run", "c-park"][..], false)] {
            let calm = |t: &str| if word { t == "active" } else { t.starts_with("⋯ waiting 2") };
            let quiet = |t: &str| if word { t == "idle" } else { t.starts_with("⋯ waiting 2") };
            let a = root_after(
                vec![
                    with_rolled(ui1c("p1", "idle", false, None, &["c-run", "c-park"], owed), "active"),
                    ui1c("c-run", "active", true, Some("p1"), &[], &[]),
                    ui1c("c-park", "parked", false, Some("p1"), &[], &[]),
                ],
                &[],
            );
            p1_cell(&a, calm, Severity::Calm, &format!("parked beside active, owed {owed:?}"));
            let a = root_after(
                vec![
                    with_rolled(ui1c("p1", "idle", false, None, &["c-run", "c-park"], owed), "idle"),
                    ui1c("c-run", "queued", false, Some("p1"), &[], &[]),
                    ui1c("c-park", "parked", false, Some("p1"), &[], &[]),
                ],
                &[],
            );
            let sev = if word { Severity::Unknown } else { Severity::Calm };
            p1_cell(&a, quiet, sev, &format!("parked, nothing active, owed {owed:?}"));
        }
    }

    /// **A GRANDCHILD-ONLY ACTIVE SUBTREE DRAWS `active`** — `p1 → m → g`,
    /// `p1` and `m` idle, `g` running. Opposed: `g` idle and `p1` draws its
    /// own `idle`, muted.
    ///
    /// FORCED RED by `Store::subtree_rollup` walking direct children only.
    /// TUI-P1c: the daemon rolls `g` up into `p1`'s `rolled` cell; the face
    /// no longer walks the held children at all.
    #[tokio::test]
    async fn ui_c_rendered_recent_row_grandchild_only_active_is_active() {
        let tree = |g: &str, exec: bool| {
            vec![
                with_rolled(ui1c("p1", "idle", false, None, &["m"], &[]), if exec { "active" } else { "idle" }),
                ui1c("m", "idle", false, Some("p1"), &["g"], &[]),
                ui1c("g", g, exec, Some("m"), &[], &[]),
            ]
        };
        let a = root_after(tree("active", true), &[]);
        p1_cell(&a, |t| t == "active", Severity::Calm, "grandchild-only active");
        let a = root_after(tree("idle", false), &[]);
        p1_cell(&a, |t| t == "idle", Severity::Unknown, "nothing below runs");
    }

    /// **A CHILD WITH `parent_id` IS NOT DRAWN IN THE RECENT BAND** (UI-a), on
    /// the rendered frame: neither an indexed one (`thread kid-idx`) nor a
    /// daemon-only one he spoke in (`title kid-d`). Opposed: the roots beside
    /// them (`thread p1`, `title root-d`) are drawn, so a band that drew
    /// nothing fails too.
    ///
    /// FORCED RED by `in_recents` and by `daemon_only_recent` each dropping
    /// their `hidden_from_recents` check.
    #[tokio::test]
    async fn ui_c_rendered_recent_band_draws_no_child_with_a_parent() {
        let a = root_after(
            vec![
                ui1c("p1", "idle", false, None, &[], &[]),
                ui1c("kid-idx", "idle", false, Some("p1"), &[], &[]),
                ui1c("kid-d", "idle", false, Some("p1"), &[], &[]),
                ui1c("root-d", "idle", false, None, &[], &[]),
            ],
            &["kid-d", "root-d"],
        );
        let s = screen(&a);
        assert!(s.contains("thread p1") && s.contains("title root-d"), "the roots: {s}");
        assert!(!s.contains("kid-idx"), "an indexed child was drawn: {s}");
        assert!(!s.contains("kid-d"), "a daemon-only child was drawn: {s}");
    }

    // ── MANAGED SUBAGENTS (TUI-SUBAGENTS, H179) ──────────────────────────

    /// **A PARENT WHOSE TURN ENDED, WAITING ON TWO MANAGED CHILDREN** — one
    /// running, one parked by the daemon for want of a budget. Delivered as the
    /// mirror delivers it: one `Fleet` frame carrying the parent's `children`
    /// cell and each child's own `status`.
    fn a_parent_waiting_on_two(a: &mut App) {
        in_a_thread(a);
        a.on_transport(TransportEvent::Fleet {
            statuses: vec![
                json!({
                    "thread_id": "t1", "state": "idle", "executing": false,
                    "queued": 0, "unread": 0, "last_seq": 2,
                    "children": {
                        "outstanding": ["c-run", "c-park"],
                        "runs": [
                            {"id": "c-run", "agent": "core:curator-v5", "name": "carve region R7",
                             "wake": "end", "started_at": "2026-10-16T20:28:11Z",
                             "request_id": "r1", "finished": null},
                            {"id": "c-park", "agent": "scout", "name": "stale nodes",
                             "wake": "end", "started_at": "2026-10-16T20:28:14Z",
                             "request_id": "r2", "finished": null}
                        ]
                    },
                    // TUI-P1c: the daemon's answer is the only rollup.
                    "rolled": {"status": "active", "display": "active (+1)", "active_descendants": 1}
                }),
                with_rolled(json!({"thread_id": "c-run", "state": "active", "executing": true,
                       "queued": 0, "unread": 0, "parent_id": "t1"}), "active"),
                with_rolled(json!({"thread_id": "c-park", "state": "parked", "executing": false,
                       "detail": "no budget config for key scout-b",
                       "queued": 1, "unread": 0, "parent_id": "t1"}), "error"),
            ],
            capped: false,
        });
        event(a, "t1", 1, json!({"kind": "user_message", "text": "carve R7 and scout"}));
        event(a, "t1", 2, json!({"kind": "turn_ended", "exit_code": 0}));
        a.project();
    }

    /// **THE HEADLESS RENDER: A PARENT WITH TWO CHILDREN, ONE RUNNING AND ONE
    /// PARKED, READS AS WAITING — NEVER AS IDLE OR FINISHED.**
    ///
    /// The frame is the one `tui --headless` dumps (`tui_render::frame`). It
    /// must say `waiting on 2 subagents` with the parked count, draw both
    /// children in the tree with their states, and give the parked one its
    /// reason in the daemon's words.
    ///
    /// RED without the change: before it, the header carried no activity, the
    /// tree was empty (no managed rows were ever constructed), and the screen
    /// said nothing about either child.
    #[tokio::test]
    async fn a_parent_with_a_running_and_a_parked_child_renders_as_waiting() {
        let mut a = app();
        a_parent_waiting_on_two(&mut a);
        let s = screen(&a);
        assert!(s.contains("waiting on 2 subagents · 1 held"), "{s}");
        let head = s.lines().next().unwrap_or("");
        assert!(!head.contains("idle"), "the header still says idle beside the phrase: {head}");
        // TUI-P1b: no `rolled` cell in this fixture, so the rule line carries
        // no count at all — never the retired `N running · M done`.
        assert!(!s.contains("1 running · 1 parked"), "the band's old tally: {s}");
        assert!(s.contains("error: parked"), "the parked child's status: {s}");
        assert!(s.contains("curator-v5 · carve region R7"), "{s}");
        assert!(s.contains("scout · stale nodes"), "{s}");
        assert!(s.contains("parked"), "{s}");
        assert!(s.contains("no budget config for key scout-b"), "the reason is not drawn: {s}");
        let rows = &a.vs.tree.rows;
        assert_eq!(rows.len(), 3, "{rows:?}");
        // Most recently used first (TUI-P1c): the running child is in use now.
        assert_eq!(rows[1].state, tui_render::SubagentState::Running);
        assert_eq!(rows[2].state, tui_render::SubagentState::Parked);
        assert_eq!(rows[1].class, SubagentClass::Managed);
        assert!(
            !a.vs.header.activity.as_ref().is_some_and(|(t, _)| t.contains("idle")),
            "a waiting parent read as idle"
        );
    }

    /// **THE MENU ROW SAYS `⋯ waiting`, NOT `idle`**, and a child that
    /// reaches recents hangs under its parent.
    #[tokio::test]
    async fn a_waiting_parent_menu_row_says_waiting_with_the_parked_mark() {
        let mut a = app();
        a_parent_waiting_on_two(&mut a);
        a.store.enter_project(0);
        let t1 = super::store::thread_list(&a.store, 0, 0)
            .rows
            .into_iter()
            .find(|r| r.id == "t1")
            .expect("t1 is listed");
        let (text, sev) = t1.activity.expect("the row carried no activity");
        assert_eq!(text, "⋯ waiting 2 · ◆1");
        // UI-b (R9.UI1b): one child RUNS beside the parked one, so the subtree
        // is active and the colour is calm; the parked count stays in the
        // text. This fixture carries no `rollup`, and this assertion said
        // `Warn` (parked before active) until the fallback was routed through
        // `shapewire::subtree::rollup`.
        assert_eq!(sev, Severity::Calm);
    }

    /// **A CHILD THAT WENT IDLE WITH NO REPORT IS NAMED, NOT DRAWN AS
    /// RUNNING.** The 400-tail bug made visible.
    #[tokio::test]
    async fn an_idle_child_the_parent_still_owes_reads_ended_unreported() {
        let mut a = app();
        a_parent_waiting_on_two(&mut a);
        a.on_transport(TransportEvent::Fleet {
            statuses: vec![with_rolled(json!({"thread_id": "c-run", "state": "idle", "executing": false,
                                  "queued": 0, "unread": 0}), "idle")],
            capped: false,
        });
        a.project();
        let c_run = a.vs.tree.rows.iter().find(|r| r.id == TreeId::Thread("c-run".into())).expect("c-run");
        assert_eq!(c_run.state, tui_render::SubagentState::Unreported);
        let (text, sev) = a.vs.header.activity.clone().expect("no activity");
        assert!(text.contains("1 ended, unreported"), "{text}");
        assert_eq!(sev, Severity::Critical);
    }

    /// **`→` ON A MANAGED ROW OPENS THE CHILD AS A THREAD, AND `←` COMES
    /// BACK TO THE PARENT ON THAT ROW.**
    #[tokio::test]
    async fn entering_a_managed_child_opens_it_and_back_returns_to_the_parent() {
        let mut a = app();
        a_parent_waiting_on_two(&mut a);
        tap(&mut a, tui_render::Key::Down); // into the band, on the parent
        assert_eq!(a.vs.mode, Mode::Thread(Region::Agents));
        tap(&mut a, tui_render::Key::Down); // onto the running child
        tap(&mut a, tui_render::Key::Down); // onto the parked child
        assert_eq!(a.vs.tree.cursor, TreeId::Thread("c-park".into()));
        tap(&mut a, tui_render::Key::Right);
        assert_eq!(a.store.open_id(), Some("c-park"), "the child did not open");
        assert!(
            a.vs.header.trail.iter().any(|c| !c.is_empty()),
            "the way back is not in the trail: {:?}",
            a.vs.header.trail
        );
        let (sub, _) = a.vs.header.activity.clone().expect("the child names its parent");
        assert!(sub.starts_with("↑ subagent of"), "{sub}");
        assert!(a.vs.composer.editable, "a managed child's composer is not live");
        tap(&mut a, tui_render::Key::Left);
        assert_eq!(a.store.open_id(), Some("t1"), "back did not return to the parent");
        assert_eq!(a.vs.tree.cursor, TreeId::Thread("c-park".into()));
    }

    /// **WHEN THE LAST CHILD REPORTS** the parent reads `resuming · 2 reports
    /// in` while the wake is queued, and the wake itself is a lane block of
    /// agent reports, never a "you said" box.
    #[tokio::test]
    async fn the_wake_reads_as_resuming_and_renders_as_agent_reports() {
        let mut a = app();
        a_parent_waiting_on_two(&mut a);
        a.on_transport(TransportEvent::Fleet {
            statuses: vec![json!({
                "thread_id": "t1", "state": "queued", "executing": false,
                "queued": 1, "unread": 1, "last_seq": 3,
                "children": {"outstanding": [], "runs": [
                    {"id": "c-run", "agent": "core:curator-v5", "name": "carve region R7", "wake": "end",
                     "started_at": "2026-10-16T20:28:11Z", "request_id": "r1",
                     "finished": {"outcome": "ok", "at": "2026-10-16T20:31:00Z"}},
                    {"id": "c-park", "agent": "scout", "name": "stale nodes", "wake": "end",
                     "started_at": "2026-10-16T20:28:14Z", "request_id": "r2",
                     "finished": {"outcome": "failed", "at": "2026-10-16T20:31:01Z"}}
                ]}
            }),
            // TUI-P1c: the daemon moves each child's `rolled` with its end.
            with_rolled(json!({"thread_id": "c-run", "state": "idle", "executing": false}), "idle"),
            with_rolled(json!({"thread_id": "c-park", "state": "idle", "executing": false}), "idle")],
            capped: false,
        });
        event(&mut a, "t1", 3, json!({
            "kind": "user_message",
            "text": "<agent_reports>\n<agent_report thread_id=\"c-run\" outcome=\"ok\">\nCarved R7 into 3 regions.\n</agent_report>\n<agent_report thread_id=\"c-park\" outcome=\"failed\">\nno budget\n</agent_report>\n</agent_reports>",
            "metadata": {"origin": "agents", "kind": "child_reported", "wake_id": "w1",
                         "children": ["c-run", "c-park"]}
        }));
        a.project();
        let (text, _) = a.vs.header.activity.clone().expect("no activity while resuming");
        assert_eq!(text, "resuming · 2 reports in");
        let s = screen(&a);
        assert!(s.contains("agent reports · 2 in · done · failed"), "{s}");
        assert!(!s.contains("<agent_reports>"), "the wake was drawn as his words: {s}");
        // Most recently used first (TUI-P1c, TC-10): c-park finished at
        // 20:31:01, a second after c-run, so it is row 1.
        assert_eq!(a.vs.tree.rows[1].state, tui_render::SubagentState::Failed);
        assert_eq!(a.vs.tree.rows[2].state, tui_render::SubagentState::Done);
    }

    // ── TUI-P1b: THE TREE — `→` EXPANDS AND ENTERS, KEPT TO THE ROOT ──────

    fn run(id: &str, name: &str) -> Value {
        json!({"id": id, "agent": "scout", "name": name, "wake": "end",
               "started_at": "2026-10-25T20:00:00Z", "request_id": id, "finished": null})
    }

    fn kids(ids: &[(&str, &str)]) -> Value {
        json!({"outstanding": ids.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
               "runs": ids.iter().map(|(i, n)| run(i, n)).collect::<Vec<_>>()})
    }

    fn rolled(status: &str, display: &str, x: u64, summary: Option<&str>) -> Value {
        json!({"status": status, "display": display, "active_descendants": x,
               "children_summary": summary})
    }

    /// **A TREE FOUR LEVELS DEEP, AS THE DAEMON WILL SERVE IT AFTER R9.TS1**
    /// — every thread's `rolled` cell carries the display string and the
    /// children summary, and the face draws them verbatim:
    ///
    /// ```text
    /// t1                active (+3)   4 children
    /// ├ c-a  carve      active (+2)   ├ g-1 grind  active (+1)  └ gg-1 deep  active
    /// │                               └ g-2 grout  idle
    /// ├ c-b  bake       active        └ h-1 hull   idle
    /// ├ c-c  cool       idle
    /// └ c-d  dread      error: parked
    /// ```
    fn a_deep_tree(a: &mut App) {
        in_a_thread(a);
        let st = |id: &str, state: &str, exec: bool, parent: &str, r: Value| {
            json!({"thread_id": id, "state": state, "executing": exec, "queued": 0,
                   "unread": 0, "parent_id": parent, "rolled": r})
        };
        a.on_transport(TransportEvent::Fleet {
            statuses: vec![
                json!({"thread_id": "t1", "state": "idle", "executing": false, "queued": 0,
                       "unread": 0, "last_seq": 1,
                       "children": kids(&[("c-a", "carve"), ("c-b", "bake"), ("c-c", "cool"), ("c-d", "dread")]),
                       "rolled": rolled("active", "active (+3)", 3, Some("2 active, 1 idle, 1 error"))}),
                {
                    let mut v = st("c-a", "idle", false, "t1",
                                   rolled("active", "active (+2)", 2, Some("1 active, 1 idle")));
                    v["children"] = kids(&[("g-1", "grind"), ("g-2", "grout")]);
                    v
                },
                {
                    let mut v = st("c-b", "active", true, "t1",
                                   rolled("active", "active", 0, Some("0 active, 1 idle")));
                    v["children"] = kids(&[("h-1", "hull")]);
                    v
                },
                st("c-c", "idle", false, "t1", rolled("idle", "idle", 0, None)),
                st("c-d", "parked", false, "t1", json!({"status": "error", "error": "parked",
                    "display": "error: parked", "active_descendants": 0})),
                {
                    let mut v = st("g-1", "idle", false, "c-a",
                                   rolled("active", "active (+1)", 1, Some("1 active, 0 idle")));
                    v["children"] = kids(&[("gg-1", "deep")]);
                    v
                },
                st("g-2", "idle", false, "c-a", rolled("idle", "idle", 0, None)),
                st("gg-1", "active", true, "g-1", rolled("active", "active", 0, None)),
                st("h-1", "idle", false, "c-b", rolled("idle", "idle", 0, None)),
            ],
            capped: false,
        });
        event(a, "t1", 1, json!({"kind": "user_message", "text": "fan out"}));
        a.project();
    }

    /// The tree's rows as thread ids — the open thread's `Parent` named by its
    /// id — so two trees seen from two different open threads compare.
    fn tree_ids(a: &App) -> Vec<(String, usize, Option<bool>)> {
        let open = a.store.open_id().unwrap_or_default().to_string();
        a.vs.tree
            .rows
            .iter()
            .map(|r| {
                let id = match &r.id {
                    TreeId::Parent => open.clone(),
                    TreeId::Thread(t) | TreeId::Agent(t) => t.clone(),
                };
                (id, r.depth, r.expanded)
            })
            .collect()
    }

    fn walk_to(a: &mut App, want: TreeId) {
        if a.vs.mode != Mode::Thread(Region::Agents) {
            tap(a, tui_render::Key::Down);
        }
        assert_eq!(a.vs.mode, Mode::Thread(Region::Agents), "not in the tree");
        for _ in 0..a.vs.tree.rows.len() {
            if a.vs.tree.cursor == want {
                return;
            }
            let at = a.vs.tree.cursor_at();
            let to = a.vs.tree.rows.iter().position(|r| r.id == want).expect("row not in tree");
            tap(a, if to > at { tui_render::Key::Down } else { tui_render::Key::Up });
        }
        assert_eq!(a.vs.tree.cursor, want, "could not walk to the row");
    }

    /// **`→` ON A COLLAPSED NODE EXPANDS IT AND ENTERS IT — ONE KEY.**
    ///
    /// FORCED (TUI-P1b-F1, expand-only): `enter_tree_row` returns after the
    /// `expanded.insert` — the open thread stays `t1` and this goes red on the
    /// first assertion. Opposed (F2, enter-only): drop the `expanded.insert`
    /// — it still enters, and goes red on the last assertion, because back at
    /// `t1` the fold he opened is gone.
    #[tokio::test]
    async fn tui_p1b_right_on_a_collapsed_node_expands_and_enters_it() {
        let mut a = app();
        a_deep_tree(&mut a);
        let ca = a.vs.tree.rows.iter().find(|r| r.id == TreeId::Thread("c-a".into())).cloned();
        assert_eq!(ca.expect("c-a is a row").expanded, Some(false), "c-a does not start collapsed");
        assert!(!tree_ids(&a).iter().any(|(id, ..)| id == "g-1"), "a collapsed fold drew its child");
        walk_to(&mut a, TreeId::Thread("c-a".into()));
        tap(&mut a, tui_render::Key::Right);
        assert_eq!(a.store.open_id(), Some("c-a"), "→ did not ENTER the node");
        assert_eq!(a.vs.mode, Mode::Thread(Region::Agents), "the selection left the tree");
        let ids = tree_ids(&a);
        assert!(ids.iter().any(|(id, d, _)| id == "g-1" && *d == 2), "→ did not EXPAND it: {ids:?}");
        assert!(ids.iter().any(|(id, d, _)| id == "g-2" && *d == 2), "{ids:?}");
        tap(&mut a, tui_render::Key::Left);
        assert_eq!(a.store.open_id(), Some("t1"));
        let back = tree_ids(&a);
        assert!(
            back.iter().any(|(id, _, e)| id == "c-a" && *e == Some(true))
                && back.iter().any(|(id, ..)| id == "g-1"),
            "the fold → opened did not outlive the visit: {back:?}"
        );
    }

    /// **ENTERING A CHILD KEEPS THE TREE TO THE ROOT** — ancestors, siblings
    /// and every fold — and is never a rebuild from the child.
    ///
    /// FORCED (TUI-P1b-F3, rebuild): `agent_tree` roots at the OPEN thread
    /// (`let root = open.clone()`) — row 0 becomes `c-a`, `t1` and the
    /// sibling `c-b` with its open fold vanish, and this goes red. Opposed by
    /// the first assertion, which holds under either implementation: at the
    /// root the two agree, so a constant "never show ancestors" cannot pass
    /// the later ones and a constant "always show t1" cannot pass the
    /// depth/fold ones.
    #[tokio::test]
    async fn tui_p1b_entering_a_child_keeps_ancestors_siblings_and_their_folds() {
        let mut a = app();
        a_deep_tree(&mut a);
        assert_eq!(a.vs.tree.rows[0].id, TreeId::Parent, "at the root, row 0 is the open thread");
        // Open c-b's fold by entering it, and come back.
        walk_to(&mut a, TreeId::Thread("c-b".into()));
        tap(&mut a, tui_render::Key::Right);
        assert_eq!(a.store.open_id(), Some("c-b"));
        tap(&mut a, tui_render::Key::Left);
        assert_eq!(a.store.open_id(), Some("t1"));
        let before = tree_ids(&a);
        // Now into c-a, then into its child g-1: depth 2, three levels down.
        walk_to(&mut a, TreeId::Thread("c-a".into()));
        tap(&mut a, tui_render::Key::Right);
        walk_to(&mut a, TreeId::Thread("g-1".into()));
        tap(&mut a, tui_render::Key::Right);
        assert_eq!(a.store.open_id(), Some("g-1"), "the grandchild did not open");
        let ids = tree_ids(&a);
        assert_eq!(ids[0].0, "t1", "the ROOT is not row 0 any more: {ids:?}");
        assert_eq!(a.vs.tree.rows[0].id, TreeId::Thread("t1".into()));
        assert!(ids.iter().any(|(id, d, _)| id == "c-a" && *d == 1), "the parent left: {ids:?}");
        assert!(ids.iter().any(|(id, d, _)| id == "gg-1" && *d == 3), "depth 3 is not drawn: {ids:?}");
        assert!(
            ids.iter().any(|(id, _, e)| id == "c-b" && *e == Some(true))
                && ids.iter().any(|(id, d, _)| id == "h-1" && *d == 2),
            "the SIBLING's fold was lost on entering: {ids:?}"
        );
        // Every row he could see before is still there, in the same order.
        let mut it = ids.iter();
        for b in &before {
            assert!(
                it.any(|x| x.0 == b.0 && x.1 == b.1),
                "{b:?} vanished or moved — the tree was rebuilt:\nbefore {before:?}\nafter  {ids:?}"
            );
        }
        // The way back is the tree's own path.
        let path: Vec<&str> = a.parents.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(path, ["t1", "c-a"]);
    }

    /// **BACK RESTORES THE SAME TREE STATE** — at every level on the way up,
    /// the tree is the one he saw going down, with the cursor on the row he
    /// left.
    ///
    /// FORCED (TUI-P1b-F4): `back_to_parent` clears `tree_ctx.expanded` — the
    /// tree at `c-a` loses `c-b`'s fold and this goes red; and with
    /// `self.tree_cursor = row` dropped, the cursor assertion goes red.
    #[tokio::test]
    async fn tui_p1b_back_restores_the_same_tree_state() {
        let mut a = app();
        a_deep_tree(&mut a);
        walk_to(&mut a, TreeId::Thread("c-b".into()));
        tap(&mut a, tui_render::Key::Right);
        tap(&mut a, tui_render::Key::Left);
        walk_to(&mut a, TreeId::Thread("c-a".into()));
        tap(&mut a, tui_render::Key::Right);
        let at_ca = tree_ids(&a);
        walk_to(&mut a, TreeId::Thread("g-1".into()));
        tap(&mut a, tui_render::Key::Right);
        let at_g1 = tree_ids(&a);
        assert_eq!(a.store.open_id(), Some("g-1"));
        tap(&mut a, tui_render::Key::Left);
        assert_eq!(a.store.open_id(), Some("c-a"));
        assert_eq!(a.vs.tree.cursor, TreeId::Thread("g-1".into()), "the cursor is not on the row he left");
        // g-1 is now unfolded for good — he opened it — and nothing else moved.
        assert_eq!(tree_ids(&a), at_g1, "c-a's tree is not the one he left");
        assert_ne!(at_ca, at_g1, "sentinel: entering g-1 changed nothing, so the comparison is x == x");
        tap(&mut a, tui_render::Key::Left);
        assert_eq!(a.store.open_id(), Some("t1"));
        assert_eq!(a.vs.tree.cursor, TreeId::Thread("c-a".into()));
        assert_eq!(tree_ids(&a), at_g1, "the root's tree is not the one he left");
    }

    /// **THE SUBAGENT LIST: STATUS, THEN TITLE, FROM THE DAEMON'S STRING** —
    /// `active (+2) …`, `active …`, `idle …`, `error: parked …` — never
    /// `(+0)`, never `running`/`done`, at depth ≥ 3 and N ≥ 2 children; the
    /// parent's line and the rule line carry `N active, M idle`.
    ///
    /// FORCED (TUI-P1b-F5): `managed_rows` ignores `rolled` (status from the
    /// state word only) — `active (+2)` is gone and `running` appears, red.
    /// Opposed (F6): the render draws `row.state.word()` and ignores
    /// `row.status` — red on every status. (F7) the rule line back to
    /// `N running · M done` — red on the vocabulary ban.
    #[tokio::test]
    async fn tui_p1b_subagent_rows_render_daemon_status_then_title() {
        let mut a = app();
        a_deep_tree(&mut a);
        let s = screen(&a);
        let line = |needle: &str| -> String {
            s.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("no row {needle:?}:\n{s}")).to_string()
        };
        for (title, status) in [
            ("scout · carve", "active (+2)"),
            ("scout · bake", "active"),
            ("scout · cool", "idle"),
            ("scout · dread", "error: parked"),
        ] {
            let l = line(title);
            let (si, ti) = (l.find(status), l.find(title));
            assert!(
                si.is_some() && si < ti,
                "{title:?} does not read `{status} {title}`: {l:?}"
            );
        }
        assert!(line("scout · bake").contains("0 active, 1 idle"), "a parent row lacks its summary");
        let rule = line("─ agents ·");
        assert!(rule.contains("2 active, 1 idle, 1 error"), "the rule line: {rule:?}");
        let band: String = s.lines().skip_while(|l| !l.contains("─ agents")).collect::<Vec<_>>().join("\n");
        for banned in ["(+0)", "running", " done", "parked ·"] {
            assert!(!band.contains(banned), "the band says {banned:?}:\n{band}");
        }
        // Depth ≥ 3: walk down to g-1 and read gg-1 and the ancestors' lines.
        walk_to(&mut a, TreeId::Thread("c-a".into()));
        tap(&mut a, tui_render::Key::Right);
        walk_to(&mut a, TreeId::Thread("g-1".into()));
        tap(&mut a, tui_render::Key::Right);
        let s = screen(&a);
        let deep = s.lines().find(|l| l.contains("scout · deep")).unwrap_or_else(|| panic!("no gg-1:\n{s}"));
        assert!(deep.contains("│ │ └"), "gg-1 is not at depth 3: {deep:?}");
        // **A REAL ORDER CHECK** (TUI-P1b2): `None < Some(_)` is true, so the
        // bare `find < find` this replaced passed with gg-1's status blanked
        // (TUI-P1b inspect, forcing P). The status must be THERE, then first.
        let (si, ti) = (deep.find("active"), deep.find("scout · deep"));
        assert!(si.is_some() && si < ti, "gg-1 (depth 3) does not read `active scout · deep`: {deep:?}");
        assert!(!deep.contains("(+"), "a leaf with no active descendants reads (+N): {deep:?}");
        let root = s.lines().find(|l| l.contains("active (+3)")).unwrap_or_else(|| panic!("root:\n{s}"));
        assert!(root.contains("2 active, 1 idle, 1 error"), "the root's line: {root:?}");
    }

    /// **THE SUBAGENT TALLY IS THE DAEMON'S `children_summary`, NOT A COUNT OF
    /// THE ROWS THIS FACE HOLDS** (TUI-P1b2, TC-09). The daemon counts every
    /// direct child; the mirror's window holds at most `CHILDREN_HELD` of
    /// them, so a recount of the rows reads a different number — here the two
    /// rows on screen are 1 active and 1 idle, and the daemon says `3 active,
    /// 17 idle`. The rule line and t1's own row say the daemon's string. With
    /// no `rolled` cell on t1 the rule line has NO count at all.
    ///
    /// FORCED RED: `agent_tree`'s `summary` recounted from the rows (the GAP
    /// N-8 shape) — the rule line reads `1 active, 1 idle`. Opposed: a summary
    /// invented with no cell — the second half's rule line grows a count.
    #[tokio::test]
    async fn tui_p1b2_the_tally_is_the_daemons_children_summary_not_a_recount() {
        let fleet = |t1_rolled: Option<Value>| {
            let mut t1 = json!({"thread_id": "t1", "state": "idle", "executing": false, "queued": 0,
                                "unread": 0, "last_seq": 1,
                                "children": kids(&[("c-a", "carve"), ("c-b", "bake")])});
            if let Some(r) = t1_rolled {
                t1["rolled"] = r;
            }
            TransportEvent::Fleet {
                statuses: vec![
                    t1,
                    json!({"thread_id": "c-a", "state": "active", "executing": true, "queued": 0,
                           "unread": 0, "parent_id": "t1", "rolled": rolled("active", "active", 0, None)}),
                    json!({"thread_id": "c-b", "state": "idle", "executing": false, "queued": 0,
                           "unread": 0, "parent_id": "t1", "rolled": rolled("idle", "idle", 0, None)}),
                ],
                capped: false,
            }
        };
        let mut a = app();
        in_a_thread(&mut a);
        a.on_transport(fleet(Some(rolled("active", "active (+3)", 3, Some("3 active, 17 idle")))));
        event(&mut a, "t1", 1, json!({"kind": "user_message", "text": "fan out"}));
        a.project();
        let s = screen(&a);
        let rule = s.lines().find(|l| l.contains("─ agents")).unwrap_or_else(|| panic!("no rule line:\n{s}"));
        assert!(rule.contains("─ agents · 3 active, 17 idle"), "the rule line is not the daemon's tally: {rule:?}");
        assert!(!s.contains("1 active, 1 idle"), "a recount of the held rows is on screen:\n{s}");
        assert!(s.lines().any(|l| l.contains("active (+3)") && l.contains("3 active, 17 idle")), "t1's row:\n{s}");

        let mut b = app();
        in_a_thread(&mut b);
        b.on_transport(fleet(None));
        event(&mut b, "t1", 1, json!({"kind": "user_message", "text": "fan out"}));
        b.project();
        let s = screen(&b);
        let rule = s.lines().find(|l| l.contains("─ agents")).unwrap_or_else(|| panic!("no rule line:\n{s}"));
        assert!(!rule.contains("─ agents ·"), "no `rolled` cell, yet the rule line counts: {rule:?}");
        assert!(!rule.contains("active") && !rule.contains("idle"), "{rule:?}");
    }

    // ── TUI-P1c: MOST RECENTLY USED FIRST; DEPTH 2 VISIBLE AND LIVE ────────

    /// **THE SUBAGENT LIST IS MOST RECENTLY USED FIRST — DIRECT (Task) AND
    /// MANAGED CHILDREN IN ONE LIST, EACH ROW STATUS THEN TITLE** (TC-10:
    /// "the most recently used agents should be at the top of the list of
    /// subagents, not the bottom"; "display both of them the same way").
    ///
    /// Under t1: managed `c-old` (spawned first, finished 20:10), `c-new`
    /// (finished 20:30), `c-run` (spawned LAST-but-one, still running); and one
    /// direct Task subagent `ag1`, still running. Active rows are in use NOW:
    /// `c-run` then `ag1` (c-run's start is the later), then `c-new`, then
    /// `c-old`. Read off the RENDERED band, and off the rails: only the last
    /// row is `└`.
    ///
    /// FORCED RED: `recency_order` reversed (oldest first) — `c-old` leads.
    /// Opposed: no sort at all (the P1b launch order: direct first, then
    /// managed in spawn order) — `ag1` leads, red too; so neither "oldest
    /// first" nor "as launched" can pass.
    #[tokio::test]
    async fn tui_p1c_subagents_are_most_recently_used_first_direct_and_managed_alike() {
        let mut a = app();
        in_a_thread(&mut a);
        let run = |id: &str, name: &str, start: &str, end: Option<&str>| {
            json!({"id": id, "agent": "scout", "name": name, "wake": "end",
                   "started_at": start, "request_id": id,
                   "finished": end.map(|e| json!({"outcome": "ok", "at": e}))})
        };
        let st = |id: &str, state: &str, exec: bool, r: Value| {
            json!({"thread_id": id, "state": state, "executing": exec, "queued": 0,
                   "unread": 0, "parent_id": "t1", "rolled": r})
        };
        a.on_transport(TransportEvent::Fleet {
            statuses: vec![
                json!({"thread_id": "t1", "state": "active", "executing": true, "queued": 0,
                       "unread": 0, "last_seq": 4,
                       "children": {"outstanding": ["c-run"], "runs": [
                           run("c-old", "old", "2026-10-25T20:00:00Z", Some("2026-10-25T20:10:00Z")),
                           run("c-new", "new", "2026-10-25T20:05:00Z", Some("2026-10-25T20:30:00Z")),
                           run("c-run", "runner", "2026-10-25T19:00:00Z", None),
                       ]},
                       "rolled": rolled("active", "active (+1)", 1, Some("1 active, 2 idle"))}),
                st("c-old", "idle", false, rolled("idle", "idle", 0, None)),
                st("c-new", "idle", false, rolled("idle", "idle", 0, None)),
                st("c-run", "active", true, rolled("active", "active", 0, None)),
            ],
            capped: false,
        });
        event(&mut a, "t1", 1, json!({"kind": "user_message", "text": "fan out"}));
        event(&mut a, "t1", 2, json!({"kind": "tool_use", "id": "ag1", "name": "Agent",
            "input": {"subagent_type": "Explore", "description": "direct scout", "prompt": "look"}}));
        event(&mut a, "t1", 3, json!({"kind": "task_started", "task_id": "T1",
                                      "task_type": "local_agent", "tool_use_id": "ag1"}));
        event(&mut a, "t1", 4, json!({"kind": "background_tasks_changed",
                                      "tasks": [{"task_id": "T1", "task_type": "local_agent"}]}));
        a.project();
        let ids: Vec<(String, bool)> = a.vs.tree.rows[1..]
            .iter()
            .map(|r| match &r.id {
                TreeId::Thread(t) | TreeId::Agent(t) => (t.clone(), r.last_sibling),
                TreeId::Parent => ("?".into(), r.last_sibling),
            })
            .collect();
        assert_eq!(
            ids,
            vec![("c-run".into(), false), ("ag1".into(), false), ("c-new".into(), false), ("c-old".into(), true)],
            "not most recently used first"
        );
        let full = screen(&a);
        // The band only: the chat above carries the direct subagent's tile.
        let s = &full[full.find("─ agents").unwrap_or_else(|| panic!("no band:\n{full}"))..];
        let at = |needle: &str| s.find(needle).unwrap_or_else(|| panic!("no {needle:?}:\n{s}"));
        assert!(
            at("scout · runner") < at("direct scout") && at("direct scout") < at("scout · new")
                && at("scout · new") < at("scout · old"),
            "the drawn band is not most recently used first:\n{s}"
        );
        // Status, then title, for both kinds.
        for (title, status) in [("scout · runner", "active"), ("direct scout", "active"), ("scout · old", "idle")] {
            let l = s.lines().find(|l| l.contains(title)).expect(title);
            let (si, ti) = (l.find(status), l.find(title));
            assert!(si.is_some() && si < ti, "{title:?} does not read `{status} {title}`: {l:?}");
        }
    }

    /// **A DEPTH-2 CHILD IS DRAWN UNDER A FOLD HE OPENED WITHOUT ENTERING IT,
    /// AND ITS ROW MOVES WITH THE DAEMON** (TC-12: "I no longer see the
    /// nested children"). At t1, c-a's fold is open (he went in and came
    /// back); g-1 sits at depth 2 reading `active (+1)`. A new frame says g-1
    /// is `idle` and the same row now reads `idle` — no re-entry, no rebuild,
    /// the root still row 0.
    ///
    /// FORCED RED (TUI-P1c-F3b): `tree_node` draws nothing below depth 1 —
    /// "g-1 (depth 2) is not drawn". Opposed by the `before` read: g-1 must
    /// first say `active (+1)`, so a row that always reads `idle` fails, and
    /// the `after` read fails a row that never moves. The live HOLD of g-1's
    /// key is `mirror::tests::the_open_threads_whole_tree_is_held_and_only_its_tree`.
    #[tokio::test]
    async fn tui_p1c_a_depth_two_child_is_visible_and_moves_live() {
        let mut a = app();
        a_deep_tree(&mut a);
        walk_to(&mut a, TreeId::Thread("c-a".into()));
        tap(&mut a, tui_render::Key::Right);
        tap(&mut a, tui_render::Key::Left);
        assert_eq!(a.store.open_id(), Some("t1"));
        let g1 = |a: &App| {
            screen(a).lines().find(|l| l.contains("scout · grind")).map(str::to_string)
        };
        let before = g1(&a).unwrap_or_else(|| panic!("g-1 (depth 2) is not drawn:\n{}", screen(&a)));
        assert!(before.contains("│ ├") || before.contains("│ └"), "g-1 is not at depth 2: {before:?}");
        assert!(before.contains("active (+1)"), "{before:?}");
        a.on_transport(TransportEvent::Fleet {
            statuses: vec![json!({"thread_id": "g-1", "state": "idle", "executing": false,
                                  "queued": 0, "unread": 0, "parent_id": "c-a",
                                  "rolled": rolled("idle", "idle", 0, None)})],
            capped: false,
        });
        a.project();
        let after = g1(&a).unwrap_or_else(|| panic!("g-1 vanished:\n{}", screen(&a)));
        assert!(after.contains("idle") && !after.contains("active"), "g-1 did not move live: {after:?}");
        assert_eq!(a.vs.tree.rows[0].id, TreeId::Parent, "the root moved");
    }

    /// **`enter` IS `→` ON A TREE ROW: IT EXPANDS AND ENTERS** (TC-11 — "make
    /// the right arrow do both of them"; `enter` is the same binding). The
    /// `→` half is `tui_p1b_right_on_a_collapsed_node_expands_and_enters_it`.
    ///
    /// FORCED RED: `Key::Enter` dropped from the agents binding — the open
    /// thread stays t1. Opposed by the first assertion: c-a starts collapsed,
    /// so a fold that was always open cannot pass as "expanded".
    #[tokio::test]
    async fn tui_p1c_enter_on_a_tree_row_expands_and_enters_like_right() {
        let mut a = app();
        a_deep_tree(&mut a);
        let ca = |a: &App| a.vs.tree.rows.iter().find(|r| r.id == TreeId::Thread("c-a".into())).map(|r| r.expanded);
        assert_eq!(ca(&a), Some(Some(false)), "c-a does not start collapsed");
        walk_to(&mut a, TreeId::Thread("c-a".into()));
        tap(&mut a, tui_render::Key::Enter);
        assert_eq!(a.store.open_id(), Some("c-a"), "enter did not ENTER the node");
        assert!(tree_ids(&a).iter().any(|(id, d, _)| id == "g-1" && *d == 2), "enter did not EXPAND it");
        assert_eq!(a.vs.tree.rows[0].id, TreeId::Thread("t1".into()), "entering re-rooted the tree");
    }
}
