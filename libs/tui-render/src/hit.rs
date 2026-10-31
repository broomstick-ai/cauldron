//! **WHAT IS UNDER A COORDINATE, AND WHAT ACTIVATING IT MEANS.**
//!
//! Two things live here and they are deliberately separate:
//!
//! * [`HitMap`] — a per-frame answer to *what is at (col, row)*. Built by the
//!   SAME pass that draws the frame ([`crate::frame_with_hits`]), so it can
//!   never describe a layout other than the one on screen.
//! * [`activation`] — a pure function from a [`Target`] and a [`Mode`] to the
//!   [`Intent`] the keyboard already produces.
//!
//! # Click never gets a HANDLER of its own — but it does get its own COLUMN
//!
//! The dispatch is `target -> intent -> the existing handler`, and it is the
//! same three steps for a mouse click and for `→`:
//!
//! ```text
//!   click (col,row) ──► HitMap::hit ──► Target ──┐
//!                                                ├─► activation(mode, via) ─► Intent
//!   `→`            ──► cursor            Target ─┘
//! ```
//!
//! **THE RETRACTION IS NOW TOTAL — A CLICK NEVER ENTERS ANYTHING.** Round six,
//! his words: *"As I requested once before, clicking on a thread shouldn't take
//! me into it. Only right can do it. Clicking should only move pointers and
//! click buttons (which we don't even have yet - nothing in this app counts as
//! that)."*
//!
//! That last clause is the load-bearing one: it FORECLOSES THE AFFORDANCE
//! EXCEPTION this module used to lean on. Round four read *"clicking on a
//! thread opens its options, not the thread. It should open the thread"* as an
//! instruction to make a click DRILL, and defended it by calling a thread's
//! name an affordance. He meant click should stop doing the wrong thing, not
//! start doing a different one — so `Via::Click` no longer resolves to
//! anything that changes the MODE or the SUBJECT:
//!
//! * a thread's NAME selects its row (it drilled),
//! * an ARCHIVED row, a SHEET act row and the QUEUE BADGE are `Via::Key` only
//!   (each keeps a verified key door — `→`/`enter` for the first two, `u` /
//!   `^O` for the badge),
//! * `→` and `enter` remain the only way IN, everywhere.
//!
//! What a click still does is REPOSITION and REVEAL IN PLACE: it moves the
//! cursor to the row, it moves the caret in the textarea (`DEC`-backed), it
//! opens the row its expander is drawn on, it re-arms follow from the `↓ N new`
//! chip, it dismisses an overlay, and the mic queue's per-row buttons still
//! answer it because they are the only way to reach rows 2..n at all and a
//! deletion that drops reachability is not a deletion we are allowed to make.
//!
//! **ONE ROUTE SURVIVES AGAINST THE RULE AND IT IS REPORTED, NOT HIDDEN**:
//! a click on a PROJECT'S NAME still opens the project sheet, because that
//! sheet has NO general keyboard door (`o` aims at a thread title; `c` opens it
//! only to press `remove`). Closing the route would make the room unreachable
//! and minting the key belongs to the round that owns the key namespace.
//!
//! **`via` IS THE OPERATOR'S RETRACTION, not a loosening.**
//! `DIR-13` said a click must do *"the SAME operation as right"*, and
//! `DEC-21`/`DEC-26` made that true by construction with one arm per
//! `(Target, Mode)` pair. He has since retracted the equivalence itself:
//!
//! > a click on a row moves the cursor to that row. It does not drill, does
//! > not expand, does not activate the row's default act. Buttons and explicit
//! > affordances (the queue count, sheet action rows, play buttons, expanders)
//! > still activate on click.
//!
//! So the two gestures now MEAN different things on a plain row, and a design
//! that cannot express the difference would have to express it somewhere else
//! — which is precisely the second implementation this module exists to
//! prevent. [`Via`] is therefore an INPUT to the one resolution rather than a
//! second resolution: there is still exactly one function, one table, and one
//! place to read what a gesture does. What changed is that the table has two
//! columns instead of one, and every arm that does NOT depend on the gesture
//! is still written once, for both.
//!
//! **What is unchanged, and it is the part that was load-bearing:** neither
//! gesture has an arm the other cannot see, nothing dispatches an intent
//! without coming through here, and no click handler anywhere has a `match` of
//! its own.
//!
//! # What a click does to each kind of target
//!
//! | target | click | `→` / `enter` |
//! |---|---|---|
//! | a project / thread ROW | move the cursor there ([`Intent::Select`]) | OPEN it |
//! | a transcript BLOCK | nothing — a transcript has no cursor (`CON-4`) | expand the newest folded row |
//! | the EXPANDER on a block | expand THAT row | — (it is not a cursor position) |
//! | the queue badge, the `↓ N new` chip | activate — they are buttons | — |
//! | a composer text row | put the caret there | — |
//!
//! The second row of that table is the one worth reading twice. `CON-4` allows
//! a per-message cursor in the transcript for exactly ONE reason (an audio
//! mode that does not exist yet), so "a click moves the cursor to that row"
//! has nothing to move there; the honest implementation of his ruling is that
//! a click on the body of a message does NOTHING, and the expander is how the
//! row is still reachable by mouse. That is why the expander had to become a
//! drawn, hit-tested affordance in this round rather than staying a model flag.
//!
//! # Misses are free
//!
//! A [`Target`] with no arm returns `None` and **nothing happens, silently** —
//! no toast, no bell, no flash. The fastest way to make a click-driven
//! interface feel bad is to punish an imprecise click; a miss must be
//! indistinguishable from not having clicked. The one exception is a gain: a
//! click anywhere inside an overlay dismisses it, which is what the overlay's
//! existing `KeySet::Any` binding already does for keys — the mouse inherits
//! the grammar rather than adding one.

use crate::keys::{Intent, Mode, Region};
use crate::select::{RowText, TextPos};

/// What sits at one coordinate of a drawn frame.
///
/// **Row-addressed, with column spans as the exception.** Everything clickable
/// today is a whole row; the queue badge is not, so [`HitMap`] carries column
/// ranges for the row it sits on. [`HitMap::hit`] takes a column either way, so
/// a later column-sensitive target (a rail, a lane digit) needs no signature
/// change anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
    /// Nothing is there. A pad row, or a coordinate off the frame.
    #[default]
    Empty,
    Header,
    Footer,
    Banner,
    Composer,
    /// Any row of a raised overlay's body.
    Overlay,
    /// The `▲N` badge on the header row. `DIR-6` says the queue must be
    /// visible at all times; this makes it REACHABLE at all times too.
    QueueBadge,
    /// **SELECTABLE ROW `n` OF THE ROOT SURFACE** — which is a pinned thread, a
    /// recent thread or a project, resolved by
    /// [`ProjectList::item`](crate::model::ProjectList::item).
    ///
    /// It was `ProjectRow` while the root was a list of projects. The root
    /// became three bands and the name stopped being true: `RootRow(3)` is the
    /// first PROJECT in the fixture the goldens use, and code reading the old
    /// name would have indexed `rows` with it and opened the wrong thing. A
    /// band header and a spacer are NOT rows here — they are drawn, they are
    /// not stops, and they carry [`Target::Empty`].
    RootRow(usize),
    /// Row `n` of a project's thread list.
    ThreadRow(usize),
    /// Block `n` of the transcript or the event lane.
    Block(usize),
    /// **THE EXPANDER DRAWN IN BLOCK `n`'s GUTTER** — the explicit affordance
    /// his ruling keeps activating on click while the row's body stops.
    ///
    /// It exists as a TARGET because it exists as two painted columns: an
    /// affordance that is not drawn cannot be clicked, and one that is drawn
    /// but not claimed here would be a button the mouse falls through.
    Expander(usize),
    /// **ONE TEXT ROW OF THE COMPOSER, AND THE COLUMN THE POINTER WAS AT.**
    ///
    /// `row` indexes [`crate::model::Composer::buf`]'s FULL layout, not the
    /// window the band happens to be showing — the renderer knows its own
    /// scroll offset and resolves it here, so the shell never re-derives it.
    ///
    /// **It is the one column-SENSITIVE target**, and [`HitMap::hit`] fills
    /// `col` in from the coordinate that was asked about. A caret lands where
    /// the pointer is; every other target in this enum is answered by the row
    /// alone.
    ComposerRow {
        row: usize,
        col: u16,
    },
    /// **THE THREAD'S TITLE, INSIDE ROW `n`** — the columns the summary
    /// actually occupies, and the door onto its sheet (§5: *"Click the title of
    /// a thread, anywhere it appears"*).
    ///
    /// It is a COLUMN SPAN and not the row, and that is the whole of its
    /// correctness. The operator's standing retraction is that a click on a
    /// plain row moves the cursor and does nothing else; the same ruling keeps
    /// *"buttons and explicit affordances (the queue count, sheet action rows,
    /// play buttons, expanders)"* activating on click. A title is that kind of
    /// thing — a named affordance inside a row, like the expander two columns
    /// to its left — so it claims its own columns and the rest of the row keeps
    /// the plain behaviour it was given.
    ///
    /// `n` is the SELECTABLE index of the surface it was drawn on: a root
    /// selectable on the home bands, a list index in a project's threads. The
    /// mode says which, exactly as it does for [`Target::RootRow`] and
    /// [`Target::ThreadRow`].
    ThreadTitle(usize),
    /// **THE PROJECT'S NAME, INSIDE ROOT ROW `n`** — the door onto the PROJECT
    /// sheet (§2: *"clicking the project's title opens its sheet"*, §5: *"a
    /// project's title opens the same shape"*).
    ///
    /// The thread title's argument, one surface over: it is a COLUMN SPAN and
    /// not the row, so the rest of the row keeps the plain click behaviour the
    /// operator's retraction gave it, and `->` still drills into the project.
    ProjectName(usize),
    /// **ONE ROW OF THE ARCHIVED SET.** An explicit affordance — the row SAYS
    /// "click to bring it back" — so it activates under both gestures, exactly
    /// as a sheet act row does.
    ArchivedRow(usize),
    /// **ONE ACT ROW OF THE SHEET.** Pressable under both gestures, because it
    /// is precisely the "sheet action row" the retraction names.
    SheetRow(usize),
    /// The `↓ N new` chip pinned to the bottom edge while follow is off.
    NewChip,
    /// **ONE ROW OF THE CLIENT QUEUE** — the thing mic mode and the queue view
    /// both draw. `n` indexes `Composer::pending`.
    MicRow(usize),
    /// **THE `▶` PLAY BUTTON DRAWN IN QUEUE ROW `n`.**
    ///
    /// It is a TARGET because it is two painted columns: his ruling is that a
    /// transcribed row looks exactly like a typed one *except for a play
    /// button*, and an affordance that is drawn but not claimed here is a
    /// button the mouse falls straight through.
    MicPlay(usize),
    /// The `retry` affordance on a row whose transcription failed.
    MicRetry(usize),
    /// The `drop` affordance on a row whose transcription failed.
    MicDrop(usize),
    /// **ROW `n` OF THE SUBAGENT TREE**, indexing [`crate::AgentTree::rows`] —
    /// so row 0 is the PARENT THREAD, which is a row of that list by his own
    /// structural clause: *"the parent needs to be an item in that list."*
    ///
    /// A CLICK moves the cursor and sets the region (a click on a plain row
    /// selects and does not drill — the standing retraction); `→`/`enter` on
    /// it DRILLS, which is the gesture he named: *"if we click right or enter
    /// on them we show their chat history instead of the parent."*
    AgentRow(usize),
}

/// **WHICH GESTURE IS ASKING.** See the module header: this is the operator's
/// retraction of "a click is the same op as right", made explicit so that the
/// difference lives in the one resolution table instead of in a second handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The mouse, on release.
    Click,
    /// `→`, `enter`, `^E` — anything that resolved [`Intent::Activate`].
    Key,
}

impl Target {
    /// The list index this target names, if it names one.
    ///
    /// **THE SHELL NO LONGER READS THIS.** It used to: `App::activate` moved
    /// the cursor here before every activation, silently, which is what made a
    /// click on row 7 and `→` after seven `↓`s the same act. Since the operator
    /// retracted that equivalence the cursor move IS the act — it went into the
    /// table as [`Intent::Select`], which carries the index itself, so the
    /// number reaches the shell through the intent rather than beside it.
    ///
    /// Kept because it is the honest way for a TEST to ask a target which row
    /// it names without re-matching the enum, and because a column-sensitive
    /// target added later will want the same question asked of it.
    pub fn row_index(self) -> Option<usize> {
        match self {
            // `ThreadTitle` names the same index its row does — it is a
            // span INSIDE that row, not a row of its own — so a caller asking
            // "which row is this" gets the same answer wherever on the row the
            // pointer landed.
            // `ProjectName` is the same kind of span on a ROOT row, so it
            // answers with that row's index for the same reason.
            Target::RootRow(i)
            | Target::ThreadRow(i)
            | Target::ThreadTitle(i)
            | Target::ProjectName(i) => Some(i),
            _ => None,
        }
    }

    /// **WHICH REGION OF A THREAD SCREEN THIS TARGET BELONGS TO**, if any.
    ///
    /// A pointer, unlike a key, says where it is — so a click is the one
    /// gesture that can MOVE the selection without an arrow, and this is the
    /// mapping it moves it by. The shell calls it once, in the click handler,
    /// BEFORE [`activation`] is consulted, so a click in the box is resolved
    /// under the box's grammar.
    ///
    /// `None` is "leave the selection where it is": the header, the footer, a
    /// pad row and every surface outside a thread have no region, and a click
    /// on them must not quietly re-point the arrows.
    pub fn region(self) -> Option<Region> {
        match self {
            Target::Block(_) | Target::Expander(_) | Target::NewChip => Some(Region::Chat),
            Target::Composer | Target::ComposerRow { .. } => Some(Region::Composer),
            // The client queue's rows, drawn inside the composer's band.
            Target::MicRow(_) | Target::MicPlay(_) | Target::MicRetry(_) | Target::MicDrop(_) => {
                Some(Region::Queue)
            }
            // The subagent band's own rows.
            Target::AgentRow(_) => Some(Region::Agents),
            _ => None,
        }
    }
}

/// **WHAT ACTIVATING A TARGET MEANS IN A MODE, TO THIS GESTURE.** The one
/// resolution; see the module header for why `via` is a parameter rather than a
/// second function.
///
/// `None` is a MISS and it is free — see the module header.
pub fn activation(target: Target, mode: Mode, via: Via) -> Option<Intent> {
    match (target, mode, via) {
        // ── gesture-INDEPENDENT arms: written once, for both ────────────────
        //
        // An overlay is dismissed by a click anywhere inside it, exactly as
        // `KeySet::Any` already dismisses it from the keyboard.
        (Target::Overlay, _, _) => Some(Intent::Back),
        // **THE QUEUE BADGE.** His earlier ruling named it in the list of
        // affordances that still activated on click; that list is retracted —
        // *"nothing in this app counts as that."*
        // **AND A CLICK NO LONGER PRESSES IT.** Opening the queue view is
        // entering a surface, which is the thing a click stopped doing. The
        // door it loses is one `u` / `^O` already carries in every body
        // mode, verified against the table — so this closes a route without
        // closing a room.
        (Target::QueueBadge, m, Via::Key) if !m.is_overlay() => Some(Intent::QueueOverlay),
        // **THE EXPANDER IS AN AFFORDANCE, NOT A ROW**, so it keeps the old
        // behaviour under both gestures — "click and expand, or click right and
        // it expands", narrowed to the two columns that say so.
        //
        // The INDEX is carried (`IS-31`, whose own note predicted this exact
        // line). It was `Target::Block(_)` once; the underscore threw away the
        // one fact the gesture was about.
        (Target::Expander(i), Mode::Thread(_) | Mode::Activity, _) => Some(Intent::ExpandBlock(i)),
        // **THE SUBAGENT TREE OPENS ON `→`/`enter`, AND NOT ON A CLICK** —
        // *"if we click right or enter on them we show their chat history
        // instead of the parent."* A plain click on the row moves the cursor
        // and sets the region, which is the standing retraction's rule for
        // every row that is not a button; the click handler does that before
        // this function is consulted at all.
        (Target::AgentRow(i), Mode::Thread(_), Via::Key) => Some(Intent::Drill(i)),
        // ── THE SHEET (§5) ──────────────────────────────────────────────────
        //
        // **THE TITLE IS A DOOR, AND THE TWO GESTURES GO THROUGH IT TO
        // DIFFERENT PLACES.** A CLICK on a thread's name OPENS THE THREAD; `o`
        // — which is the only thing that aims a KEY at a title — opens its
        // options. `0076`, and both halves are his:
        //
        // > *"currently, clicking on a thread opens its options, not the
        // > thread. It should open the thread."*
        //
        // > *"I want to be able to jump straight into the options of a thread
        // > by clicking o while my cursor is on the thread handle."*
        //
        // Those are ONE design and they are built together on purpose. Round
        // two put the sheet behind a clicked title, deliberately and well, and
        // this is him hitting it; removing the click door without adding the
        // key door would have made the sheet unreachable by mouse or keyboard
        // — the same silent feature removal round two measured when the
        // ellipsis fell outside its own click target.
        //
        // **AND THE OLDER CLICK LAW SURVIVES, NARROWED.** *"A click on a row
        // moves the cursor to that row. It does not drill"* — except on
        // buttons and affordances, which still activate. A thread's NAME is an
        // affordance by that ruling's own list (it is why the title had a
        // claim of its own at all), so what changed is where this affordance
        // leads, not whether an affordance activates. A click anywhere else on
        // the row, on a project row, or on empty space still SELECTS: see the
        // `RootRow`/`ThreadRow` arms below, which are untouched.
        // **A CLICK ON A THREAD'S NAME SELECTS ITS ROW AND STOPS.** It used
        // to OPEN the thread; see the module header for why that reading was
        // wrong and what retracted it. The name is a span INSIDE the row, so
        // "select the row it is a span of" is the same answer the rest of the
        // row gives — which is the point: after this there is no column of a
        // thread row where a click means something other than "put the cursor
        // here."
        // **ON A `see all` PAGE A CLICK ON A TITLE SELECTS AND STOPS.**
        //
        // This is the operator's standing ruling, written for a surface that
        // did not exist when the title became an affordance:
        //
        // > clicking on a thread shouldn't take me into it. Only right can do
        // > it. Clicking should only move pointers and click buttons (which we
        // > don't even have yet - nothing in this app counts as that).
        //
        // "nothing in this app counts as that" is the load-bearing clause and
        // it is why this arm is `Select` and not `OpenAt`: the page adds no
        // button, so NOTHING on it activates on a click. `→` on the row opens
        // the thread (the `RootRow` key arm below) and `o` opens its options
        // (the key arm on the very next line), so both doors the title carried
        // are still reachable — by the gestures he said should carry them.
        (Target::ThreadTitle(i), Mode::SeeAll(_), Via::Click) => Some(Intent::Select(i)),
        (Target::ThreadTitle(i), Mode::Projects | Mode::NewProject | Mode::List, Via::Click) => {
            Some(Intent::Select(i))
        }
        // `o` is the only thing that aims a KEY at a title, and it opens the
        (
            Target::ThreadTitle(i),
            Mode::Projects | Mode::NewProject | Mode::List | Mode::SeeAll(_),
            Via::Key,
        ) => Some(Intent::OpenSheet(i)),
        // **A PROJECT'S NAME IS THE SAME DOOR ONTO THE SAME SHAPE, AND SINCE
        // ROUND SIX IT IS `Via::Key` LIKE EVERY OTHER DOOR — the last click
        // route the law had left open is now closed.**
        //
        // `0127` kept this one arm deliberately and said why: the project sheet
        // had NO general keyboard door, so taking the click would have deleted
        // the ROOM rather than the gesture. It refused to mint a key, because
        // minting belonged to the round that owns the key namespace.
        //
        // **THE DOOR EXISTS NOW, AND IT IS NOT A NEW KEY.** `o` —
        // `Intent::Options`, which already opened a THREAD's sheet — resolves
        // the row under the cursor and aims at `Target::ProjectName` when that
        // row is a PROJECT. One existing act learning one more row type, so it
        // costs nothing in the ctrl namespace and needed no assignment from
        // `0128`'s table. `App::on_intent`'s `Options` arm is the whole of it,
        // and it goes through THIS table, so the key and the pointer still
        // cannot drift.
        //
        // **AND A PROJECT'S NAME ON THE PAGE IS THE SAME TWO GESTURES SPLIT
        // THE SAME WAY.** The projects band has a page too, and on it the name
        // is not a button either.
        //
        // **THE CLICK ARM IS ONE ARM FOR ALL THREE MODES, and it is written
        // this way ON PURPOSE.** `chat_conformance_test.sh`'s `click-title`
        // probe mutates the THREAD TITLE's click arm, and it finds that arm by
        // asserting its BRACED SHAPE occurs exactly once in this file. A second
        // arm written in that same shape makes the probe abort with
        // `ANCHOR-MISSING click-title`, which is the anchor doing its job: it
        // cannot tell which of two identical-looking arms it is about to
        // mutate. Collapsing the three modes into one parenthesised pattern
        // keeps the probe's match unique AND says the truer thing — a project's
        // name is not a button on any surface that draws one.
        //
        // The anchor text is deliberately NOT quoted here: a comment that
        // repeats it is one more thing for the probe's own count to find.
        (
            Target::ProjectName(i),
            Mode::SeeAll(_) | Mode::Projects | Mode::NewProject,
            Via::Click,
        ) => Some(Intent::Select(i)),
        (
            Target::ProjectName(i),
            Mode::SeeAll(_) | Mode::Projects | Mode::NewProject,
            Via::Key,
        ) => Some(Intent::OpenProjectSheet(i)),
        // **BRINGING A PROJECT BACK IS ONE PRESS**, and the row says so in
        // words. There is deliberately no arm for "the AUI is unreachable" —
        // that row returns this same intent and the shell answers with the
        // row's own sentence, never with a silent no-op.
        // **`Via::Key` ONLY.** `→`/`enter` on the row is the door, verified
        // against `Mode::Archived`'s own rows; a click on it now moves nothing
        // and acts on nothing, because un-archiving is not a pointer move.
        (Target::ArchivedRow(i), Mode::Archived, Via::Key) => Some(Intent::Unarchive(i)),
        // **AN ACT ROW ACTS, AND IT ACTS THE SAME WAY FOR BOTH GESTURES.**
        //
        // There is deliberately NO arm here for "the owner is unreachable":
        // that row returns the same intent as every other, and the shell
        // answers it by putting the row's own sentence in the notice line and
        // doing nothing else. Branching here would make this table know which
        // process owns a row, which is exactly what the pure crate must not
        // know — and it would turn an unavailable row into a SILENT no-op,
        // which the stub doctrine names by name.
        // **AND `Via::Key` ONLY, since round six.** Some of these rows open a
        // FIELD, which is a mode change and therefore an entry; the rest
        // mutate. Neither is a pointer move. `→`/`enter` over the sheet's own
        // cursor is the door and `↑`/`↓` walk to any row, so every act stays
        // reachable.
        (Target::SheetRow(i), Mode::Sheet | Mode::SheetField, Via::Key) => Some(Intent::SheetAct(i)),

        // Re-arm follow. This is `G`'s capability kept while `G`'s letter goes
        // — the chip is already drawn on the last row, it says "click here" in
        // words, and it is a button by every reading of his list.
        (Target::NewChip, _, _) => Some(Intent::Bottom),

        // ── the two arms the RETRACTION split ───────────────────────────────
        //
        // **A CLICK ON A PLAIN ROW SELECTS IT AND STOPS.** It does not drill.
        // The cursor move IS the whole act, which is why it is an intent of its
        // own rather than something the click handler does on the side: `→`
        // then opens what he picked, and the two-step grammar is two arms of
        // one table rather than an agreement between a handler and a binding.
        // `Mode::NewProject` is the ROOT with a live field in one of its rows,
        // so the root's own click grammar still holds under him: a click on a
        // plain row selects it and stops.
        // **AND THE PAGE INHERITS THIS LAW WHOLE, with no exception written
        // for the band header or the `see all` foot** — both of which are
        // `RootRow`s (`DEC-78`), so a click on either one already selects and
        // `→` already opens. That is exactly the ruling above, which is why
        // this feature needed no exception to it.
        (Target::RootRow(i), Mode::Projects | Mode::NewProject, Via::Click)
        | (Target::ThreadRow(i), Mode::List, Via::Click) => Some(Intent::Select(i)),
        // **THE PAGE INHERITS THE CLICK LAW WHOLE**, additively — because
        // `pointer_frames.sh`'s "click-selects" mutation quotes the two lines
        // above VERBATIM to prove a drilling click is caught. Folding the page
        // into them moves the anchor and the suite refuses with
        // ANCHOR-MISSING. Those two lines are byte-identical to `0127`'s.
        (Target::RootRow(i), Mode::SeeAll(_), Via::Click) => Some(Intent::Select(i)),
        // `→` / `enter` on the row the cursor is already on: OPEN. `DIR-12`'s
        // drill-in, untouched by the retraction — it was only ever about the
        // mouse.
        (Target::RootRow(_), Mode::Projects, Via::Key)
        | (Target::ThreadRow(_), Mode::List, Via::Key) => Some(Intent::Open),
        // `→` on a page row, additive for the reason above. THREE suite
        // mutations quote arms of this function line for line, which makes this
        // match's formatting load-bearing text and not a style question.
        (Target::RootRow(_), Mode::SeeAll(_), Via::Key) => Some(Intent::Open),

        // **THE BODY OF A TRANSCRIPT ROW IS NOW SILENT TO THE MOUSE.** His
        // ruling says a click moves the cursor to the row; `CON-4` says the
        // transcript has no cursor to move until an audio mode exists. Both are
        // obeyed by doing nothing — and the row stays reachable by mouse
        // through its expander, which is drawn for exactly this reason.
        (Target::Block(_), _, Via::Click) => None,
        // `→` and `^E` still reach the newest folded row, which is the
        // target `App::keyboard_target` hands in.
        (Target::Block(i), Mode::Thread(_) | Mode::Activity, Via::Key) => Some(Intent::ExpandBlock(i)),

        // **THE CARET GOES WHERE HE POINTED.** Only a click can produce this
        // target at all — the keyboard has no coordinate — so there is no `Key`
        // arm to write, and the caret's keyboard grammar is the motion keys.
        (Target::ComposerRow { row, col }, Mode::Thread(Region::Composer), Via::Click) => {
            Some(Intent::Caret { row, col })
        }

        // ── MIC MODE'S AFFORDANCES ──────────────────────────────────────────
        //
        // **THREE BUTTONS, AND THEY ARE BUTTONS**, so they answer to both
        // gestures for the same reason the expander does: each is a painted
        // affordance that says what it does. They are live in mic mode, in the
        // THREAD'S OWN QUEUE REGION — the client rows are drawn in the
        // composer's band now, and *"make the queue accessible by click"* is
        // the ask that put them there — AND in the queue view, because the queue view is where every item is shown
        // and a play button he can see but not press would be the worse half of
        // an affordance.
        (Target::MicPlay(i), Mode::Mic | Mode::Queue | Mode::Thread(Region::Queue), _) => {
            Some(Intent::MicPlay(i))
        }
        (Target::MicRetry(i), Mode::Mic | Mode::Queue | Mode::Thread(Region::Queue), _) => {
            Some(Intent::MicRetry(i))
        }
        (Target::MicDrop(i), Mode::Mic | Mode::Queue | Mode::Thread(Region::Queue), _) => {
            Some(Intent::MicDrop(i))
        }
        // **THE BODY OF A QUEUE ROW IS SILENT**, exactly as a transcript row's
        // is. The row is not a cursor stop and clicking one must not be a way
        // to fire an act he did not aim at; the three buttons above are how a
        // row is acted on.
        (Target::MicRow(_), _, _) => None,

        // Header, Footer, Banner, Composer chrome, Empty, and every row/mode
        // pair with no meaning: nothing, silently.
        _ => None,
    }
}

/// One drawn row's clickable structure, and the logical text on it.
#[derive(Debug, Clone, Default)]
struct Row {
    /// What the row is, wherever no span claims the column.
    target: Target,
    /// `(first_col, last_col_exclusive, target)`, searched before `target`.
    spans: Vec<(u16, u16, Target)>,
    /// What this row shows, of which logical unit, starting where. `None` is a
    /// row with no text a copy could take — padding, a border, blank space.
    text: Option<RowText>,
}

/// One drawn row, as the body builders hand it back: what is under it, and
/// what is written on it.
///
/// The two travel together because they are the same fact seen twice and they
/// are both produced by the pass that drew the row. Separating them into two
/// parallel vectors is how they would come apart.
#[derive(Debug, Clone, Default)]
pub struct RowInfo {
    pub target: Target,
    pub text: Option<RowText>,
    /// `(first_col, last_col_exclusive, target)`, searched before `target`.
    ///
    /// **Carried on the ROW INFO and not attached afterwards**, because the
    /// body builders are the only code that knows a span is there: the
    /// expander's two columns are decided by the same pass that paints the
    /// glyph into them, so the claim travels with the row that has it.
    pub spans: Vec<(u16, u16, Target)>,
}

impl RowInfo {
    pub fn new(target: Target, text: Option<RowText>) -> Self {
        Self {
            target,
            text,
            spans: Vec::new(),
        }
    }

    /// A row that can be clicked but carries no copyable text.
    pub fn bare(target: Target) -> Self {
        Self::new(target, None)
    }

    /// Claim a column range on this row for a finer target.
    pub fn claim(mut self, from: u16, to: u16, target: Target) -> Self {
        self.spans.push((from, to, target));
        self
    }
}

/// **What is under every coordinate of ONE drawn frame.**
///
/// Built row by row by the draw itself, so there is no second layout pass to
/// disagree with the first. `frame_with_hits` debug-asserts the invariant:
/// exactly one entry per screen row.
#[derive(Debug, Clone, Default)]
pub struct HitMap {
    rows: Vec<Row>,
    width: u16,
}

impl HitMap {
    pub(crate) fn new(width: u16) -> Self {
        Self {
            rows: Vec::new(),
            width,
        }
    }

    pub(crate) fn push(&mut self, target: Target) {
        self.rows.push(Row {
            target,
            spans: Vec::new(),
            text: None,
        });
    }

    pub(crate) fn push_row(&mut self, info: RowInfo) {
        self.rows.push(Row {
            target: info.target,
            spans: info.spans,
            text: info.text,
        });
    }

    pub(crate) fn push_many(&mut self, rows: impl IntoIterator<Item = RowInfo>) {
        for r in rows {
            self.push_row(r);
        }
    }

    /// Attach the logical text to the row most recently pushed.
    pub(crate) fn text_on_last(&mut self, text: RowText) {
        if let Some(r) = self.rows.last_mut() {
            r.text = Some(text);
        }
    }

    /// Claim a column range on the row most recently pushed.
    pub(crate) fn span_on_last(&mut self, from: u16, to: u16, target: Target) {
        if let Some(r) = self.rows.last_mut() {
            r.spans.push((from, to, target));
        }
    }

    /// How many rows this map describes. Equal to the frame's height.
    pub fn height(&self) -> usize {
        self.rows.len()
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    /// **What is at `(col, row)`.** Off the frame is [`Target::Empty`], never a
    /// panic and never a guess: a mouse report can name a coordinate a resize
    /// has already invalidated.
    pub fn hit(&self, col: u16, row: u16) -> Target {
        let Some(r) = self.rows.get(row as usize) else {
            return Target::Empty;
        };
        if col >= self.width {
            return Target::Empty;
        }
        for (from, to, t) in &r.spans {
            if col >= *from && col < *to {
                return self.with_col(*t, col);
            }
        }
        self.with_col(r.target, col)
    }

    /// **THE ONE COLUMN-SENSITIVE TARGET, ANSWERED HERE AND NOWHERE ELSE.**
    ///
    /// A composer text row is stored with a placeholder column because the row
    /// is the same row whatever column is asked about; the CARET, though, lands
    /// where the pointer was. Filling it in at the point of the question keeps
    /// `activation`'s signature free of a coordinate that means nothing to any
    /// other arm, and keeps the shell from re-deriving which band a column is
    /// in — which is the second-derivation class this module exists to close.
    fn with_col(&self, t: Target, col: u16) -> Target {
        match t {
            Target::ComposerRow { row, .. } => Target::ComposerRow { row, col },
            other => other,
        }
    }

    /// Every row that carries logical text, in screen order. This is the whole
    /// input to [`crate::selected_text`] and to the selection paint.
    pub fn texts(&self) -> Vec<RowText> {
        self.rows.iter().filter_map(|r| r.text.clone()).collect()
    }

    /// The logical text on one screen row, if there is any.
    pub fn text_row(&self, row: u16) -> Option<&RowText> {
        self.rows.get(row as usize)?.text.as_ref()
    }

    /// **WHERE IN THE DOCUMENT A SCREEN CELL IS.**
    ///
    /// A cell on a row with text resolves exactly. A cell on a row WITHOUT
    /// text — padding above a short transcript, a rule, a border, the blank
    /// half of the composer — resolves to the nearest row that has some,
    /// searching in `toward`'s direction first and then the other way.
    ///
    /// **A miss must not cancel the gesture.** He drags from the blank space
    /// above the oldest message down through the transcript; if the press
    /// resolved to nothing, that whole drag would select nothing and look
    /// broken, so the press snaps DOWN into the content and the release snaps
    /// UP into it. The snap is the reason a drag can start anywhere at all,
    /// which is the literal ask.
    pub fn pos_at(&self, col: u16, row: u16, toward: Toward) -> Option<TextPos> {
        let n = self.rows.len();
        if n == 0 {
            return None;
        }
        let start = (row as usize).min(n.saturating_sub(1));
        let resolve = |i: usize, exact: bool| -> Option<TextPos> {
            let t = self.rows.get(i)?.text.as_ref()?;
            let off = if exact {
                t.offset_at(col)
            } else {
                match toward {
                    // Snapping downwards lands at the START of the row we
                    // found; upwards, at its end. That is the direction the
                    // selection is growing in either case.
                    Toward::Down => t.start,
                    Toward::Up => t.start + t.len().saturating_sub(1),
                }
            };
            Some(TextPos::new(t.unit, off))
        };
        if let Some(p) = resolve(start, true) {
            return Some(p);
        }
        let (first, second): (Vec<usize>, Vec<usize>) = match toward {
            Toward::Down => ((start + 1..n).collect(), (0..start).rev().collect()),
            Toward::Up => ((0..start).rev().collect(), (start + 1..n).collect()),
        };
        first
            .into_iter()
            .chain(second)
            .find_map(|i| resolve(i, false))
    }
}

/// Which way a coordinate with no text on it should look for some.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toward {
    Down,
    Up,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE RETRACTION, ASSERTED AS A DIFFERENCE RATHER THAN AS AN
    /// EQUIVALENCE.** A click on a plain row SELECTS; `→` on the same row
    /// OPENS. This test is the inverse of the one it replaces, and it is the
    /// operator's ruling stated in code.
    #[test]
    fn a_click_on_a_plain_row_selects_it_and_right_is_what_opens_it() {
        for (target, mode) in [
            (Target::RootRow(3), Mode::Projects),
            (Target::ThreadRow(7), Mode::List),
        ] {
            let i = target.row_index().expect("a list row names its index");
            assert_eq!(
                activation(target, mode, Via::Click),
                Some(Intent::Select(i)),
                "{target:?} must SELECT on a click, never drill"
            );
            assert_eq!(
                activation(target, mode, Via::Key),
                Some(Intent::Open),
                "{target:?} must still OPEN on →"
            );
        }
        // **THE BODY OF A TRANSCRIPT ROW IS SILENT TO THE MOUSE.** It does not
        // expand, which is the specific act he retracted, and it cannot move a
        // cursor the transcript does not have (`CON-4`).
        assert_eq!(
            activation(Target::Block(11), Mode::Thread(Region::Chat), Via::Click),
            None
        );
        assert_eq!(
            activation(Target::Block(2), Mode::Activity, Via::Click),
            None
        );
        assert_eq!(
            activation(Target::Block(11), Mode::Thread(Region::Chat), Via::Key),
            Some(Intent::ExpandBlock(11)),
            "→ still reaches the newest folded row"
        );
    }

    /// **WHAT A CLICK STILL DOES, AND IT IS NO LONGER "his list".**
    ///
    /// This test used to be `an_explicit_affordance_activates_on_a_click_
    /// exactly_as_it_did` and it walked his own enumeration — the queue count,
    /// the sheet's act rows, the play buttons and the expanders — asserting
    /// each answered to BOTH gestures identically. Round six retracted the
    /// enumeration: *"Clicking should only move pointers and click buttons
    /// (which we don't even have yet - nothing in this app counts as that)."*
    ///
    /// **THE QUEUE BADGE IS THE ONE THAT MOVED**, and it moved because raising
    /// the queue view is ENTERING a surface. The other three survive on
    /// properties rather than on the word "button": the expander reveals its
    /// own row in place, the chip is a scroll, and an overlay dismissal is
    /// LEAVING. See the module header.
    ///
    /// FORCED: give the `QueueBadge` arm `_` for its `via` again and the first
    /// assertion goes red.
    #[test]
    fn a_click_reveals_and_leaves_but_never_enters() {
        // Still both gestures: nothing here changes the mode or the subject.
        for (target, mode, want) in [
            (Target::Expander(11), Mode::Thread(Region::Chat), Intent::ExpandBlock(11)),
            (Target::Expander(2), Mode::Activity, Intent::ExpandBlock(2)),
            (Target::NewChip, Mode::Thread(Region::Chat), Intent::Bottom),
            (Target::Overlay, Mode::Help, Intent::Back),
        ] {
            assert_eq!(
                activation(target, mode, Via::Click),
                Some(want),
                "{target:?} stopped activating on a click"
            );
            assert_eq!(
                activation(target, mode, Via::Key),
                Some(want),
                "{target:?} must mean the same to both gestures"
            );
        }
        // **AND THE BADGE IS KEY-ONLY NOW.** `u` / `^O` is its door and
        // `golden.rs` asserts that door against `BINDINGS`.
        assert_eq!(activation(Target::QueueBadge, Mode::List, Via::Click), None);
        assert_eq!(
            activation(Target::QueueBadge, Mode::List, Via::Key),
            Some(Intent::QueueOverlay)
        );
    }

    /// The caret lands where the pointer was, and the column is carried all the
    /// way through the resolution rather than being re-derived by the shell.
    #[test]
    fn a_click_in_the_composer_carries_its_column_into_the_caret_intent() {
        assert_eq!(
            activation(
                Target::ComposerRow { row: 4, col: 17 },
                Mode::Thread(Region::Composer),
                Via::Click
            ),
            Some(Intent::Caret { row: 4, col: 17 })
        );
        // The keyboard has no coordinate, so it can never produce one.
        assert_eq!(
            activation(
                Target::ComposerRow { row: 4, col: 17 },
                Mode::Thread(Region::Composer),
                Via::Key
            ),
            None
        );
    }

    /// A miss is silent. Every one of these is a coordinate he can plausibly
    /// click, and none of them may do anything.
    #[test]
    fn a_miss_does_nothing_and_says_nothing() {
        for t in [
            Target::Empty,
            Target::Header,
            Target::Footer,
            Target::Banner,
            Target::Composer,
        ] {
            for via in [Via::Click, Via::Key] {
                assert_eq!(activation(t, Mode::List, via), None, "{t:?} is not free");
            }
        }
        // A row target in the WRONG mode is also a miss, not a wrong action.
        assert_eq!(
            activation(Target::ThreadRow(0), Mode::Projects, Via::Click),
            None
        );
        assert_eq!(activation(Target::RootRow(0), Mode::List, Via::Click), None);
        assert_eq!(
            activation(Target::ThreadRow(0), Mode::Projects, Via::Key),
            None
        );
    }

    #[test]
    fn a_coordinate_off_the_frame_is_empty_rather_than_a_panic() {
        let mut m = HitMap::new(20);
        m.push(Target::Header);
        m.push(Target::ThreadRow(0));
        assert_eq!(m.hit(0, 0), Target::Header);
        assert_eq!(m.hit(19, 1), Target::ThreadRow(0));
        assert_eq!(m.hit(0, 9), Target::Empty, "past the last row");
        assert_eq!(m.hit(20, 0), Target::Empty, "past the last column");
        assert_eq!(m.hit(u16::MAX, u16::MAX), Target::Empty);
    }

    /// A column span beats the row it sits on, and only inside its range.
    #[test]
    fn a_span_claims_its_columns_and_no_others() {
        let mut m = HitMap::new(40);
        m.push(Target::Header);
        m.span_on_last(10, 14, Target::QueueBadge);
        assert_eq!(m.hit(9, 0), Target::Header);
        assert_eq!(m.hit(10, 0), Target::QueueBadge);
        assert_eq!(m.hit(13, 0), Target::QueueBadge);
        assert_eq!(m.hit(14, 0), Target::Header);
    }

    /// **THE EXPANDER'S TWO COLUMNS BEAT THE ROW THEY SIT ON, AND ONLY THEM.**
    /// One column to the right of the affordance is the message body, which is
    /// now silent — so this span is the whole difference between a row that can
    /// be opened by mouse and one that cannot.
    #[test]
    fn the_expander_claims_the_gutter_and_the_body_beside_it_stays_a_plain_block() {
        let mut m = HitMap::new(40);
        m.push_row(RowInfo::bare(Target::Block(6)).claim(0, 2, Target::Expander(6)));
        assert_eq!(m.hit(0, 0), Target::Expander(6));
        assert_eq!(m.hit(1, 0), Target::Expander(6));
        assert_eq!(m.hit(2, 0), Target::Block(6));
        assert_eq!(m.hit(39, 0), Target::Block(6));
    }

    /// The composer row answers with the column that was ASKED about, whatever
    /// column it was stored with — the caret is the one thing on screen whose
    /// target is finer than a row.
    #[test]
    fn a_composer_row_reports_the_column_the_pointer_was_at() {
        let mut m = HitMap::new(40);
        m.push(Target::ComposerRow { row: 3, col: 0 });
        assert_eq!(m.hit(0, 0), Target::ComposerRow { row: 3, col: 0 });
        assert_eq!(m.hit(17, 0), Target::ComposerRow { row: 3, col: 17 });
        assert_eq!(m.hit(39, 0), Target::ComposerRow { row: 3, col: 39 });
        // Off the frame is still Empty, not a caret at a column that is not
        // there.
        assert_eq!(m.hit(40, 0), Target::Empty);
    }
}
