//! **THE INTERNAL EVENT VOCABULARY — everything that can move this program.**
//!
//! `0023` `mined_from_apps_tui` item 7 is *one internal event enum, one loop,
//! one redraw per event*, and this is the enum. It sits in its own module
//! rather than inside [`crate::app`] because it is the one type EVERY other
//! module in this crate speaks: the input pump ([`crate::input`]) produces it,
//! the read-only RPCs ([`crate::reads`]) produce it, the transport forwarder
//! produces it, and the loop consumes it.
//!
//! With the enum living in `app.rs` those producers all had to name the
//! COORDINATOR to say a word to it, which is the shape that turns a coordinator
//! into the place everything ends up. Here, the dependency graph is a fan-in
//! with no cycle: `input`, `reads` and `transport` know `event`; `app` knows
//! all four; nothing knows `app`.
//!
//! **A variant here is a promise that the loop redraws after handling it.**
//! That is [`crate::app::run`]'s invariant and it is why nothing in this crate
//! draws anything on its own.

use ratatui::crossterm::event::KeyEvent;
use serde_json::Value;

use crate::store::LedgerRow;
use crate::transport::TransportEvent;

/// **ONE THING THE TERMINAL SAID.**
///
/// It exists because of [`Ev::Input`]: the terminal's word is now delivered a
/// READ AT A TIME rather than an event at a time, and the batch needs an
/// element type. Everything in here used to be an `Ev` variant of its own and
/// is otherwise unchanged.
#[derive(Debug, Clone)]
pub enum Input {
    Key(KeyEvent),
    /// **A BRACKETED PASTE, AS ONE EVENT.** Requires `?2004h`, which
    /// `term::CLAIMS` now takes. Without it a paste arrives as a burst of
    /// keystrokes and there is nothing to tell it from typing — which is
    /// precisely the hole the batch rule's second half covers.
    Paste(String),
    /// **`CSI 200~` ARRIVED AND THE PASTE IS OPEN.** Not an edit and not a
    /// keystroke — a STATE, so the face can say a paste is in progress rather
    /// than looking frozen while a slow clipboard arrives.
    ///
    /// A paste that completes at once delivers this and its [`Input::Paste`] in
    /// the SAME batch, which is what stops a fast paste from flashing a notice
    /// for one frame: `app::on_input` decides the notice over the whole batch,
    /// not event by event.
    PasteBegan,
    /// **A PASTE OPENED, SAID NOTHING, AND WAS ABANDONED** — `IS-38`'s timeout
    /// half. The keyboard is live again and the face owes him a notice saying
    /// what happened, because a paste that silently produced nothing is
    /// indistinguishable from a face that ignored him.
    PasteFailed,
    /// **A PASTE WAS CLOSED BY THE IDLE-GAP RULE RATHER THAN BY A TERMINATOR**
    /// — `IS-38`'s other half. Carried as its own fact because it IS one: his
    /// clipboard landed, and the terminal never said the paste was finished.
    PasteHealed,
    /// **THE TERMINAL ANSWERED THE KITTY-KEYBOARD QUERY.** Outside tmux this is
    /// the only evidence that `shift-enter` can be reported apart from `enter`,
    /// and it arrives as an event rather than as a probe's return value because
    /// a probe would be a second thread reading the one input descriptor — see
    /// `term::negotiate` for the keystroke that cost.
    EnhancedKeys,
    Press {
        col: u16,
        row: u16,
    },
    Click {
        col: u16,
        row: u16,
    },
    Drag {
        col: u16,
        row: u16,
    },
    DragEnd {
        col: u16,
        row: u16,
    },
    Wheel {
        up: bool,
    },
    Resize,
}

/// Everything that can move this program.
#[derive(Debug)]
pub enum Ev {
    /// **ONE TERMINAL READ'S WORTH OF INPUT, AND THE BATCH IS THE UNIT ON
    /// PURPOSE.**
    ///
    /// This is the shape of the defence against a reproduced security-shaped
    /// defect. A paste whose CONTENT contains the bytes `ESC[201~` terminates
    /// the paste early inside crossterm, and everything after it is delivered
    /// as ORDINARY KEY EVENTS — including a trailing carriage return. Under
    /// "enter sends", a clipboard carrying those bytes does not merely corrupt
    /// the buffer: it DISPATCHES A MESSAGE HE DID NOT WRITE.
    ///
    /// crossterm cannot be asked about this from outside — its parser clears
    /// its buffer on the failed re-parse and the bytes are gone before any code
    /// of ours exists. But every one of those events comes out of ONE `read()`,
    /// so a batch is exactly the right unit: drain the whole read, and if it
    /// contained a paste, nothing after that paste may act. `app::on_input`
    /// holds the rule; this type is what makes it expressible.
    ///
    /// It is also why there is one redraw per BATCH rather than per event,
    /// which is strictly fewer redraws than before and never more.
    Input(Vec<Input>),
    /// A tick while the pointer is parked at an edge of the content band with
    /// the button still down. Scrolls one row and re-reads where the pointer
    /// now is, which is how a selection reaches past the top of the screen.
    ///
    /// The ONE event this program generates for itself, and it exists because
    /// a stationary pointer produces no reports at all: without it, autoscroll
    /// would advance only while he kept jiggling his hand.
    AutoScroll,
    /// The input pump died. Not a normal exit; something took stdin away.
    InputClosed,
    /// One fact from the threads daemon, or about the link to it.
    Transport(TransportEvent),
    /// An open-thread load came back. `from_seq` is where the load started, so
    /// the transcript can say what it is NOT showing.
    Transcript {
        thread_id: String,
        result: Result<(Vec<LedgerRow>, u64), String>,
    },
    /// **ELIDED BODIES OF AN OPENED THREAD, FILLED IN AFTER IT WAS DRAWN**
    /// (THREAD-LOAD-LATENCY F7). The transcript is handed over the moment its
    /// window lands, with each elided row saying its words are loading; these
    /// rows replace them, newest first, in chunks. They are the same rows the
    /// transcript carried, completed — never a new fact, and never a reason to
    /// move the viewport.
    TranscriptBodies {
        thread_id: String,
        rows: Vec<LedgerRow>,
    },
    /// A queue view came back — both halves, because the failed half is the
    /// one that most needs a human.
    Queue {
        thread_id: String,
        result: Result<(Vec<Value>, Vec<Value>), String>,
    },
    /// A fleet re-paint he asked for with `^R`.
    Fleet {
        result: Result<(Vec<Value>, Vec<Value>), String>,
    },
    /// **A `threads/{id}/send` CAME BACK.** The ONE mutation this face issues, and
    /// the composer is not emptied until this arrives saying the daemon took
    /// it — a box cleared on the optimistic assumption is a box that eats his
    /// words when the socket was down.
    Sent {
        thread_id: String,
        /// The daemon's `message_id` for the accepted message, or its refusal
        /// in its own words.
        result: Result<Option<u64>, String>,
    },
    /// **ONE FACT ABOUT THE SECOND SOCKET** — the AUI's 9010 listener.
    ///
    /// It carries the link STATE and the `Invalidate` hints and nothing else.
    /// It deliberately cannot carry anything the face renders as a fact about
    /// his work: `0060`'s binding condition is that nothing drawn is sourced
    /// from 9010, and a channel that cannot express such a thing is a stronger
    /// guarantee than a rule saying not to.
    Aui(crate::aui::AuiEvent),
    /// **AN ACT FROM THE SHEET CAME BACK**, from whichever of the two owners it
    /// was bound to.
    ///
    /// One variant for both sockets, because from the face's side they are the
    /// same event: the act happened, or it did not and here is the sentence. The
    /// sentence is the OWNER'S OWN — the AUI's refusals are already written for
    /// a human, and paraphrasing them here would put the face between him and
    /// the only party that knows what went wrong.
    ActDone {
        /// What he asked for, in his words, for the notice line.
        what: String,
        result: Result<(), String>,
        /// **WHAT AN ACCEPTED ACT EMPTIES.** `None` for every act that owns no
        /// buffer.
        ///
        /// It rides on the EVENT rather than being inferred from `what`,
        /// because `what` is a sentence written for him and matching on a
        /// sentence is how a face starts behaving differently when somebody
        /// improves the wording.
        clears: Option<Clears>,
    },
    /// **THE INFO AREA'S TIMER.** The standing notice has reached its
    /// deadline: clear it and redraw.
    ///
    /// It exists because a face sitting still produces no events at all, so a
    /// notice with a timeout and nothing to wake the loop would stay on screen
    /// until something unrelated repainted — which is to say, a timeout nobody
    /// can observe. The RENDERER also drops an expired notice on its own
    /// (`Notice::live_at`), so the two together mean the field and the screen
    /// cannot disagree in either direction.
    NoticeExpired,
    /// **A CAPTURE CAME BACK FROM THE TRANSCRIBER.**
    ///
    /// Keyed by the capture PATH rather than by a queue index, because the
    /// queue can move underneath a transcription in flight — `^K` takes an
    /// entry back, a failed row is dropped — and an index resolved on arrival
    /// would write his words onto somebody else's row. The same lesson
    /// `RowKey` carries for the transcript.
    ///
    /// The `Err` half is already in HIS language, not an exit code: the gate
    /// and the exit-code table are read in `mic`, at the point where the
    /// meaning is known.
    Transcribed {
        path: String,
        result: Result<String, String>,
    },
    /// **A RECORDING STOPPED AND THE FILE IS READY TO TRANSCRIBE.** Separate
    /// from the stop keystroke because stopping is not instant: the child is
    /// signalled, waited for, and its header repaired, and doing that on the
    /// event loop's thread would freeze the face for the length of it.
    Recorded {
        /// `Err` carries the reason in his language — there was no capture
        /// command, no home directory, or the device could not be opened.
        result: Result<String, String>,
    },
    /// **PLAYBACK FINISHED OR FAILED.** `afplay` blocks for the length of the
    /// clip, so it is spawned detached and reaped here; this is what lets the
    /// `▶` stop being highlighted and what carries a missing file's refusal to
    /// the info area.
    Played {
        path: String,
        result: Result<(), String>,
    },
    /// **THE COMPOSER'S IDLE TIMER.** It closes an edit RUN (so the next
    /// keystroke starts a fresh undo step) and writes the debounced draft
    /// journal. Armed only while there is unsaved editing to do, so a face
    /// sitting still is a face with no timers at all.
    ComposeIdle,
    /// **THE LINK LINE'S TIMER — the one that makes "down 4m" true on the
    /// SCREEN and not merely true at the moment it was painted.**
    ///
    /// The header and the banner both state how long the daemon has been gone
    /// and when the next attempt is due, and both are recomputed from the
    /// frame's `now` — so they are exact on every frame that happens. The
    /// problem is the frames that do not: through an outage the face has
    /// nothing to react to but the reconnect ladder, whose backoff grows to
    /// tens of seconds, so a screen that had just been painted would hold a
    /// second that had already passed. That is the stale-`2m ago` defect
    /// `DEC-13` exists to prevent, arriving by a different door.
    ///
    /// It carries NO state change. The loop redraws after every event, which
    /// is the whole point of it: it is a request for a frame, nothing more.
    ///
    /// Armed only while the link is down, and only until the rendered STRING
    /// would next change — `tui_render::Link::refresh_in` decides that, since
    /// the renderer is what knows how coarsely it spells a duration. A link
    /// that has been down for two hours costs one wakeup a minute.
    LinkTick,
    /// **THE DOWN PAGE'S RESTART BUTTON CAME BACK** (R7.S1c): the first line
    /// of what `cauldron --env <name> start threads` printed, or why it could
    /// not be run. It changes only the note on the down page; reconnecting is
    /// the seam's ladder and nothing here drives it.
    DownRestart(String),
    /// **HOLD-TO-SPEAK'S IDLE GAP — the clock that stands in for the key
    /// release his terminal cannot report.**
    ///
    /// `0095` measured it: tmux 3.5a never forwards the kitty keyboard push to
    /// iTerm2, so no key-up byte exists anywhere in his stack, and the
    /// reference implementation he named does not use one either — it counts
    /// auto-repeat and ends the gesture on a gap. **The end of a hold is
    /// therefore SILENCE, and silence is the one thing no event can carry.**
    /// This is the wake-up that notices it.
    ///
    /// Armed only while a HOLD is in flight that opened a capture — a tap, an
    /// idle mic band and a face outside mic mode all arm nothing. The interval
    /// is learned from the repeats themselves; see [`crate::mic::Hold`].
    HoldIdle,
    /// **THE INVALIDATE DEBOUNCE'S TRAILING EDGE — the re-read the burst owed
    /// and never paid.**
    ///
    /// `App::on_invalidate` coalesces a burst of `Invalidate` frames into one
    /// re-read, which is right: a `MovePin` ripples six nodes and six re-reads
    /// of thirteen files is five re-reads of no new information. What was
    /// wrong is that it coalesced by DROPPING — leading edge only, no trailing
    /// edge — so a frame that arrived inside the window was gone with nothing
    /// to retry it.
    ///
    /// **That is a data-loss bug and not a latency one**, because the frames
    /// in a burst are not interchangeable. `apps/aui`'s 9010 handler answers a
    /// `SetThreadSummary` the instant the command is QUEUED for the desk loop
    /// (`v2/mutate.rs`: `send(&tx, Command::SetThreadSummary …)`) and fires its
    /// ripple immediately — BEFORE `ThreadIndex::save` has written the new
    /// name. So the frame the face spent its one re-read on carried the OLD
    /// file, and the frame that carried the rename was the one that got
    /// dropped. His words: *"after rename of the thread, the main menu wasn't
    /// updated."*
    ///
    /// This is the wake-up that pays the debt. Armed only while a frame has
    /// actually been coalesced away (`App::invalidate_deadline`), so a face
    /// sitting still with nothing outstanding arms no timer at all — the same
    /// rule every other clock in this loop obeys.
    IndexSettled,
}

/// **A BUFFER AN ACCEPTED ACT EMPTIES** — and only an accepted one.
///
/// The composer's own rule, applied to the root's field: a box cleared
/// optimistically is a box that eats his words when the socket was down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clears {
    /// The name typed into `+ new project`.
    NewProjectName,
    /// **AN ACCEPTED `MovePin` — WHAT IT CLEARS IS THE FACE'S STALE READING OF
    /// THE PINNED ORDER.**
    ///
    /// The order belongs to the AUI. The moment it acks, `.mind/threads.json`
    /// has been rewritten and everything this face believes about the band is
    /// one revision old — so the ack is the signal to re-read, and the re-read
    /// is what makes the row appear to move.
    ///
    /// **IT DELIBERATELY BYPASSES `INVALIDATE_DEBOUNCE`**, and that is not a
    /// second door: the debounce exists because one `MovePin` ripples SIX
    /// `Invalidate` frames and six re-reads of thirteen files is five re-reads
    /// of no new information. An ack is not a ripple — it is ONE act
    /// completing, known to have changed the file — so it costs exactly one
    /// re-read and is never debounced.
    ///
    /// **THE SECOND HALF OF THIS NOTE WAS A WORKAROUND AND IS NOW OBSOLETE.**
    /// It used to say the debounce was leading-edge only, so a second move
    /// inside its 250 ms window had its `Invalidate` swallowed and the face
    /// would keep drawing the old order until something unrelated woke it —
    /// which was TRUE, and was this crate documenting the round-six defect a
    /// round before it was reported. The debounce now has a trailing edge
    /// ([`Ev::IndexSettled`]), so a swallowed frame is deferred rather than
    /// lost and no act needs a bypass to be live. This one keeps its bypass
    /// because an ack is genuinely cheaper and genuinely sooner, not because
    /// the debounce would otherwise eat it.
    PinMove,
    /// **AN ACCEPTED REGISTRY WRITE.** `PinMove`'s argument, one file over:
    /// this face has just rewritten `~/.cauldron/projects.json` through
    /// `projects::Registry::update`, so the project band it is drawing is one
    /// revision old and the ack is the signal to re-read. It also empties the
    /// add-a-project field — on the ACK and never on the send, so a refused
    /// path stays on screen to be corrected.
    RegistryWrite,
    /// **AN ACCEPTED `threads/create` — WHAT IT CLEARS IS THE FACE WAITING TO
    /// KNOW THE THREAD EXISTS.**
    ///
    /// `n` mints the id and asks the daemon for the thread; this ack is the
    /// daemon saying it is there. **Only then is the thread opened**, which is
    /// the composer's rule applied to a door rather than to a buffer: a view
    /// opened optimistically on a refused create is a screen that pretends to
    /// be a thread, and he would type into it.
    ///
    /// It re-reads NOTHING, unlike [`Clears::PinMove`] and
    /// [`Clears::RegistryWrite`], and that absence is a FINDING rather than an
    /// oversight: a thread the daemon made has a header in no project index, so
    /// there is no file whose re-read would put it on his list. See
    /// `crate::acts::route_new_thread`.
    ThreadCreated,
}
