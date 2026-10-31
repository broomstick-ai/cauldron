//! **One key table with two readers.**
//!
//! [`BINDINGS`] is the single source of truth. [`intent`] does not contain a
//! `match` over keys — it SCANS this table — and the `?` overlay and the
//! footer hints RENDER this same table. There is therefore no expression of a
//! binding anywhere else in the system, and the commonest terminal-app lie (a
//! help screen documenting a key that was renamed) is not merely discouraged,
//! it has nowhere to live.
//!
//! This is `0023` `philosophy` §3 stated as a mechanism: *"no binding exists
//! outside the key table"* is checkable here, because the shell matches on
//! [`Intent`] and never on a [`Key`]. An unhandled intent is a non-exhaustive
//! `match` — a compile error, not a dead key.
//!
//! **Why `Key` is our own type and not `crossterm::KeyEvent`.** `crossterm` is
//! a terminal driver and this crate may not depend on one — the reason it may
//! not is the reason the whole crate exists. So the table speaks a
//! face-neutral `Key`, the shell writes a small total
//! `crossterm::KeyEvent -> Key` conversion, and the table stays pure and
//! testable with no terminal anywhere near it.
//!
//! Note for that conversion: `Key::Char` is CASE-SIGNIFICANT — `g` and `G` are
//! different bindings, and `0005` §3's lowercase=cheap / uppercase=heavy rule
//! depends on it — and `Key::Ctrl(c)` always carries the LOWERCASE letter, so
//! `Ctrl-L` is `Key::Ctrl('l')`.
//!
//! **THE WHEEL, THE CLICK AND THE DRAG ARE NOT IN THIS TABLE, deliberately**
//! (`0018`). The table speaks keystrokes; a wheel notch is not one, a
//! coordinate is not one, and a gesture with two ends is not one. A
//! `Key::WheelUp` or a `Key::Click` would be a fiction the `?` overlay would
//! then have to render as a "key". The shell resolves a notch straight to the
//! same [`Intent`] the arrow keys carry; a click resolves to a
//! [`crate::hit::Target`] and then through [`crate::hit::activation`] to the
//! Intent `→` already produces; a DRAG resolves to a
//! [`crate::select::Selection`] and reaches no intent at all, because it is
//! not an action on the model. All three keep one meaning without inventing a
//! keystroke.
//!
//! **`←` IS BACK AND `→` IS ACTIVATE, at every depth** — the AUI's
//! `NavAction::Back` / `NavAction::Select` (`libs/aui-nav/src/input.rs:48`),
//! mirrored so the muscle memory carries across the two faces. The one place
//! the mirror is deliberate rather than mechanical: **`←` AT THE ROOT DOES NOT
//! EXIT.** It jumps to the first row, exactly as the AUI's Root does
//! (`apps/aui/src/dispatch/root.rs:64-72`) — an accidental `←` must never end
//! the session.
//!
//! **What was CUT this round, and by whose ruling.** `k` and `j` (operator:
//! "cut" — the arrows and the wheel remain); `x` (operator: *"if it's a
//! truncated message we should be able to click and expand, or click right and
//! it expands"*, so expansion became an ACTIVATION rather than a key); and
//! Help's `↑`/`↓`/`j`/`k`, which were a live defect — they scrolled the view
//! UNDERNEATH the overlay and never scrolled the overlay. **`u` is KEPT**: the
//! queue gained a click affordance, and removing the key before he has felt the
//! replacement would strand the queue.
//!
//! **What `0041` CUT, and why it is a deletion rather than a replacement.**
//! **Select mode is gone in its entirety** — `v`, `y`, `Enter`, `Esc`, `q`,
//! and the [`Mode`] itself. It existed for exactly one reason: to be a
//! KEYBOARD SUBSTITUTE for the drag-copy the face could not do, and it was a
//! poor one — it could only ever select a WHOLE message, and `0036` measured
//! that its copy never reached his clipboard at all, because his tmux runs
//! `set-clipboard external` and drops a raw `OSC 52`. Now that a drag selects
//! any characters on any cell and copies through a wrapper that does reach
//! him, keeping six keys as a worse way to do the same thing would be six keys
//! in a letter-space he has asked twice to keep small. **`M` is gone with
//! it**: it handed the mouse back so the TERMINAL could select, and the face
//! now selects for itself, so there is nothing on the other side of the
//! toggle. The escape hatch for a wedged process is the terminal's own
//! modifier-drag (OPTION in iTerm2, not Shift — `IS-26`), which needs no
//! binding here because it never reaches this program.
//!
//! **WHAT THE COMPOSER CHANGED, and it is the largest edit this table has
//! taken.** A thread now holds a real always-focused textarea, and three things
//! follow from that and could not be avoided:
//!
//! 1. **Every printable key in a thread is TEXT — IN THE BOX, AND ONLY
//!    THERE.** `f` types an `f` while the composer holds the selection. So a
//!    thread's global commands are `ctrl-<letter>`, and `KeySet::Printable` is
//!    the last row of the COMPOSER'S grammar. The other three body modes KEEP
//!    their bare letters as unlisted synonyms — there is nothing to type into
//!    them, so the letters cost nothing there — and `ctrl-<letter>` works in all
//!    four, which is what keeps the PRINTED grammar uniform. See `common_rows`.
//!
//!    **ROUND FIVE MOVED THE SECOND HALF OF THIS.** The chat, the queue and the
//!    subagent band carried a printable row of their own until then, on the
//!    reading that typing always types and drags the selection into the box
//!    with it. His answer: *"The textarea should be inactive when the user
//!    cursor is outside of the textarea."* Those three rows are gone;
//!    [`Mode::textarea_is_live`] is the fact they were deleted to state, and
//!    the caret and the paste path read it too.
//! 2. **`←` and `→` are the composer's cursor in a thread**, not Back and
//!    Activate. `esc` carries back (it always did, as `←`'s synonym) and
//!    `^E` expands a folded row through the same `hit::activation` path a
//!    click takes. This is the one place the AUI mirror is deliberately broken,
//!    and it is broken in the one mode the AUI has no equivalent of.
//! 3. **A binding may now require a CAPABILITY** — see [`Caps`] and [`Need`].
//!    `shift-enter` and `^enter` exist in the table and are rendered by neither
//!    reader unless the terminal negotiated the ability to report them. That
//!    rule is what stops this face joining the long list of terminal agents
//!    that advertise `shift-enter` on stacks which cannot deliver it.
//!
//! **What is STILL absent.** Mic mode, and every queue MUTATION — cancel and
//! re-select. `^Q` fills the CLIENT's queue, which never reaches the daemon
//! until a send stitches it into one payload, so the daemon's queue is still
//! something this face only reads.

/// **WHICH REGION OF THE THREAD SCREEN HOLDS THE SELECTION.**
///
/// A thread screen is not one surface with an implicit focus: it is a stack of
/// REGIONS — the chat, the client queue (when it holds anything), the composer,
/// and the subagent band (when the thread has any) — exactly one of which holds
/// the selection, with every arrow resolving against it.
///
/// **It is carried INSIDE [`Mode::Thread`] rather than beside it**, for the
/// reason [`Mode::SheetField`] and [`Mode::Mic`] are modes and not flags: the
/// arrows mean three different things in three of these regions, and the `?`
/// overlay and the footer render FROM the key table — so a region the table
/// cannot see is a grammar the overlay would document over a surface that does
/// not answer to it, which is the exact drift `0042` caught. Every consumer
/// that must change behaviour by region already keys on `Mode` (the table,
/// `footer_hints`, `overlay_rows`, `withheld`, `help_for`, `hit::activation`,
/// the bands, the origin back-stack), so a payload on the existing key reaches
/// all of them with no second axis to keep in step.
///
/// **THE ORDER IS HIS, and it is an order over a VARIABLE SET** — *"conversation
/// / queue (if it exists) / input box / subagents (if they exist)"* — so the walk
/// skips absent members rather than stopping on an empty band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Region {
    /// The transcript. Scroll-only: it has no cursor row (`CON-4`).
    Chat,
    /// The CLIENT queue — what this composer has set aside and not sent.
    /// A stop only while it holds at least one item.
    Queue,
    /// The textarea. **Where a thread opens**, and where typing always lands.
    #[default]
    Composer,
    /// **THE SUBAGENT TREE.** A stop only while the thread has subagents to
    /// show — [`crate::AgentTree::rows`] is what the walk consults, and the
    /// parent alone is not a tree.
    ///
    /// > *I just want to see the subagents, and their subagents. It looks like
    /// > a file tree… I don't want to see them in the chat area, it takes up
    /// > 90% of the chat area.*
    Agents,
}

impl Region {
    /// The word the `?` overlay's heading uses for this region.
    pub fn label(self) -> &'static str {
        match self {
            Region::Chat => "chat",
            Region::Queue => "queue",
            Region::Composer => "composer",
            Region::Agents => "agents",
        }
    }
}

/// Which key grammar is live. The table is scanned per mode, so a key can mean
/// two things in two modes without either one being a special case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// **THE ROOT.** One row per project — every non-dot directory under
    /// `~/minds/` plus `~/.aui/miscellaneous`. Where the face opens, following
    /// the AUI, whose root menu is projects and not threads.
    #[default]
    Projects,
    /// One project's threads, newest at the bottom.
    List,
    /// **ONE THREAD'S SCREEN, WITH THE REGION THAT HOLDS THE SELECTION.**
    ///
    /// See [`Region`]. `Mode::Thread(r)` IS the selection: there is no second
    /// field to keep in step with it, and the compiler enumerates every site
    /// that has to decide what a region means there.
    Thread(Region),
    /// The fleet's event lane — every event from every thread, live.
    Activity,
    /// The `?` overlay is up.
    Help,
    /// The queue view is up.
    Queue,
    /// **THE SHEET IS UP** (§5) — one thread's facts and its acts, over
    /// whatever it was opened from. The letters are NOT text here: the act
    /// rows are walked and activated, exactly as a menu.
    Sheet,
    /// **THE SHEET'S FIELD IS LIVE** — he is typing compaction instructions or
    /// a new summary.
    ///
    /// A SECOND mode and not a flag, for the reason [`Mode::Mic`] is a mode:
    /// **in the field the letters ARE text**, and `c` types a `c` rather than
    /// compacting. Only the key table can express that, and the `?` overlay and
    /// the footer both render FROM the table — so a flag would leave the
    /// overlay documenting the sheet's menu grammar over a surface that does
    /// not answer to it, which is the exact drift `0042` caught.
    SheetField,
    /// **MIC MODE — a SEPARATE input mode, entered and left explicitly.**
    ///
    /// `DIR-5`: *"need a text area in the TUI, not a mic toggle. We can have a
    /// mic mode too but it's separate…"* It is a `Mode` and not a flag on the
    /// thread for one reason that outranks the cost: **in mic mode the letters
    /// are not text.** `r` records rather than typing an `r`. Only the key
    /// table can express that, and the `?` overlay and the footer both render
    /// FROM the table — so a flag would have left the overlay documenting the
    /// composer's grammar over a mode that does not answer to it, which is the
    /// exact drift `0042` caught and the one-table rule exists to prevent.
    ///
    /// The thread's transcript is still on screen underneath; what the
    /// composer band held is replaced by the mic band.
    Mic,
    /// **THE ARCHIVED SET IS OPEN** (§1) — one door back, one act.
    ///
    /// A body mode and not an overlay: §1 draws it as its own screen with its
    /// own header and a `<- back` footer, which is what "there is no read-only
    /// second entrance to a room you said you didn't want to see" looks like on
    /// glass. An overlay would have left the root visible underneath it.
    Archived,
    /// **HE IS TYPING A NEW PROJECT'S NAME** into the root's `+ new project`
    /// row (§1, §10).
    ///
    /// A SECOND mode and not a flag, for the reason [`Mode::SheetField`] is
    /// one: **in the field the letters ARE text.** `?` types a `?` rather than
    /// raising the help overlay, and `Tab` types nothing rather than swapping
    /// the view. Only the key table can express that, and the `?` overlay and
    /// the footer both render FROM the table — so a flag would leave the
    /// overlay documenting the root's menu grammar over a live field, which is
    /// the exact drift `0042` caught.
    NewProject,
    /// **THE LITTLE RENAME MODAL — a box over wherever he is standing.**
    ///
    /// His words: *"also I want rename to do a modal. Don't take me to the
    /// settings. Just a little modal."*
    ///
    /// **WHAT HE OVERRULED WAS THE DESTINATION, NOT THE MECHANISM.** Bare `r`
    /// used to open [`Mode::SheetField`] by way of the sheet, deliberately, so
    /// that one place knew what rename means and two surfaces could not
    /// disagree about it. That reasoning stands and is KEPT: this mode drives
    /// the identical mutation through the identical state — the same
    /// `SheetState`, the same `rename` buffer, the same `act_for(SheetAct::
    /// Rename)` → `ThreadAct::Rename` → `Mutation::SetThreadSummary`. **Only
    /// the FRAME is different.** A second `Mutation::` path was the failure
    /// mode here and there is not one.
    ///
    /// A SECOND MODE and not a flag, for exactly [`Mode::SheetField`]'s reason:
    /// **in the box the letters ARE text.** `r` types an `r` rather than
    /// re-opening the rename he is already inside, and `o` types an `o` rather
    /// than reaching for a sheet. Only the key table can express that, and the
    /// footer renders FROM the table.
    ///
    /// It is NOT in [`Mode::is_overlay`]'s list, which is the one thing that
    /// separates it from the sheet: an overlay REPLACES the body, and this
    /// stands over a body that is still drawn. `App::view` names it directly
    /// instead.
    RenameModal,
    /// **ONE BAND'S OWN PAGE — `see all`, and it is a BODY MODE.**
    ///
    /// > let's replace 'show more' with 'see all' and it takes us to a
    /// > dedicated page with just that band and all results. Going left from
    /// > there goes back to the main menu. … Going right on a band title does
    /// > the same thing.
    ///
    /// A body mode on [`Mode::Archived`]'s precedent — its own screen, its own
    /// header, `← back` in the footer, nothing visible underneath — drawing the
    /// ROOT'S OWN PROJECTION with one band in focus and no cap
    /// (`ProjectList::focus`).
    ///
    /// **IT CARRIES ITS BAND, the way [`Mode::Thread`] carries its region**,
    /// for two reasons that are the same reason twice: the compiler then
    /// enumerates every site that has to know WHICH band, and an entry on the
    /// shell's back-stack (`App::origin`) says which page it was, so `←` out of
    /// a thread opened from the recent page lands on the recent page and not on
    /// a guess.
    ///
    /// **WHY IT IS A MODE AND NOT A FLAG ON [`Mode::Projects`].** The page's
    /// grammar differs from the root's in exactly one row — `←` is
    /// [`Intent::Top`] at the root, which never exits, and [`Intent::Back`]
    /// here, which leaves the screen — and the `?` overlay and the footer both
    /// render FROM this table, which is per-Mode. A flag would leave the legend
    /// documenting "first row" over a key that goes back to the main menu:
    /// exactly the drift `0042` caught, and the same argument by which
    /// `SheetField`, `NewProject` and `Mic` are modes.
    ///
    /// Everything else it does it does through [`root_acts`], which the root
    /// and the page both read — so a page where `r` silently did nothing could
    /// not happen without also breaking the root.
    SeeAll(crate::model::BandId),
}

impl Mode {
    /// **IS THE TEXTAREA LIVE? — the ONE predicate, read by everything that
    /// can write into the draft or draw its caret.**
    ///
    /// His words, and they are the whole contract: *"The textarea should be
    /// inactive when the user cursor is outside of the textarea, including on
    /// click anywhere else."*
    ///
    /// **WHY IT IS A PREDICATE AND NOT A RULE REPEATED AT EACH SITE.** The
    /// region model shipped in round four gated the KEY TABLE per region
    /// correctly — every band has its own rows — but "the box is live" was
    /// never stated anywhere as a fact, so each writer into the draft made its
    /// own call and two of them (the printable rows, and the caret) answered
    /// differently from the arrows. One definition with three readers (the key
    /// table's printable rows, `App`'s paste path, and the composer band's
    /// caret) cannot drift, and the next writer into the draft has something to
    /// consult rather than a convention to notice.
    ///
    /// It is deliberately about the THREAD'S composer only. `Mode::NewProject`
    /// and `Mode::SheetField` have buffers of their own, which are the whole
    /// surface while they are up and have no region to be outside of.
    pub fn textarea_is_live(self) -> bool {
        matches!(self, Mode::Thread(Region::Composer))
    }

    /// **WHICH ROW-SET OF THE TABLE THIS MODE READS.** The identity for every
    /// mode but one.
    ///
    /// [`Mode::SeeAll`] carries its band so that an entry on the shell's
    /// back-stack can say WHICH page it was. Its KEY GRAMMAR does not vary with
    /// the band — the three pages answer to the same keys, through
    /// [`root_acts`] — so the rows are written ONCE, under a representative
    /// band, and the four readers of this table ask this question first.
    ///
    /// Writing the block three times would say the same thing and would be
    /// three places for the pinned page to fall out of step with the recent
    /// one. This is deliberately the ONLY collapse in the table: a payload that
    /// genuinely CHANGED the grammar has to be its own rows, which is exactly
    /// what `Mode::Thread(Region)` is and why it is four entries.
    pub fn table_key(self) -> Mode {
        match self {
            Mode::SeeAll(_) => Mode::SeeAll(crate::model::BandId::Recent),
            m => m,
        }
    }

    /// Whether this mode REPLACES the body rather than being a way of reading
    /// it. An overlay's own grammar is the only one live while it is up, which
    /// is why it is a `Mode` and not a flag.
    pub fn is_overlay(self) -> bool {
        matches!(
            self,
            Mode::Help | Mode::Queue | Mode::Sheet | Mode::SheetField
        )
    }

    /// The word the header uses for this view.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Projects => "projects",
            // **THE PAGE IS NAMED BY ITS BAND.** Three static strings and not
            // a `format!`, because this function returns a `&'static str` and
            // because the page's real header line is built by the shell from
            // `ProjectList::totals` (`recent · 182 threads`) — this is the
            // fallback word, the same role `Mode::Sheet`'s "sheet" plays.
            Mode::SeeAll(crate::model::BandId::Pinned) => "all pinned",
            Mode::SeeAll(crate::model::BandId::Recent) => "all recent",
            Mode::SeeAll(crate::model::BandId::Projects) => "all projects",
            Mode::List => "threads",
            // All four regions are ONE view with one name: the header says
            // where he is, and the region says which band answers the arrows.
            Mode::Thread(_) => "thread",
            Mode::Activity => "activity",
            Mode::Help => "keys",
            Mode::Queue => "queue",
            // The sheet's header shows the THREAD'S NAME, not a view word —
            // it is the one surface whose subject IS its title (§5's frame).
            // This label is the fallback for a sheet with no subject, which
            // the shell never builds.
            Mode::Sheet | Mode::SheetField => "sheet",
            Mode::Archived => "archived",
            // The root is still what he is looking at; the field is a row on it.
            Mode::NewProject => "projects",
            Mode::Mic => "mic",
            // **THE MODAL DOES NOT RENAME THE VIEW IT STANDS OVER**, which is
            // the whole of *"don't take me to the settings"*: the header is
            // still telling him where he is, because he has not gone anywhere.
            // The shell keeps the under-mode's word (`App::view`), so this
            // label is a fallback nothing reaches today.
            Mode::RenameModal => "rename",
        }
    }
}

/// A key as this crate understands it.
///
/// **The modified ENTER, the modified ARROWS and the modified HOME/END are
/// their own variants rather than a `(modifier, base)` pair**, which matches
/// how `Ctrl(c)` and `Alt(c)` were already spelled and keeps [`Key`] a flat
/// thing a `match` can be exhaustive over. The composer is what made them
/// necessary: with a real textarea in the band, `alt-enter` and `enter` are two
/// different acts and one of them is a SEND.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    /// In the two lists, **BACK** — the AUI's `NavAction::Back`. In a thread,
    /// the composer's cursor: a textarea owns the arrows, and `esc` carries
    /// back on its own.
    Left,
    /// In the two lists, **ACTIVATE** — the AUI's `NavAction::Select`, and the
    /// keyboard twin of a click. In a thread, the composer's cursor.
    Right,
    Enter,
    /// **NEWLINE, always reachable, with no protocol at all.** `\x1b\r`, which
    /// every terminal produces for alt-enter and which needs nothing
    /// negotiated.
    AltEnter,
    /// **NEWLINE, but only when the stack can report it** — see [`Caps`]. Never
    /// the only way to reach anything; a binding that cannot work is never
    /// advertised.
    ShiftEnter,
    /// **QUEUE, when the stack can report it.** An alias for `^Q`, never the
    /// only route.
    CtrlEnter,
    Esc,
    Tab,
    Home,
    End,
    AltHome,
    AltEnd,
    AltLeft,
    AltRight,
    /// **BY PARAGRAPH IN THE TEXTAREA, BY HIS OWN MESSAGES IN THE CHAT, AND
    /// BETWEEN BAND HEADERS AT THE ROOT** — three meanings, one key pair,
    /// resolved by the region that holds the selection.
    ///
    /// **They did not exist until `0100`, and their absence was not a missing
    /// binding — it was a FLATTENING.** `input::to_key` had no `alt` arm for
    /// Up/Down (it has one for Left/Right), so `opt-↑` arrived as a plain `↑`
    /// and the table never saw it, exactly as `0052` found for the ctrl-arrows
    /// one line earlier in the same function. The wire was MEASURED before
    /// either was bound — `CSI 1;3A` / `CSI 1;3B` on a tmux configured as his
    /// is; see `motion_keys_probe.sh` and
    /// `vt::tests::the_measured_motion_wire_table_decodes_to_distinguishable_keys`.
    AltUp,
    AltDown,
    /// **THE LIMIT IN THAT DIRECTION** — line start / line end / buffer top /
    /// buffer bottom. `0052`, and they are bound only because they were
    /// MEASURED to arrive distinguishably: tmux 3.5a with his own
    /// `extended-keys on` / `extended-keys-format xterm` emits `CSI 1;5D`,
    /// `CSI 1;5C`, `CSI 1;5A`, `CSI 1;5B`, which `vt` already decodes as an
    /// arrow with CONTROL. **What was missing was not the bytes** — it was that
    /// `input::to_key` had no `ctrl` arm for an arrow and silently collapsed all
    /// four onto the unmodified key, so `ctrl-←` moved the caret one character
    /// and nothing anywhere said why.
    CtrlLeft,
    CtrlRight,
    CtrlUp,
    CtrlDown,
    Backspace,
    /// **`opt-delete` ON HIS KEYBOARD** — the Mac `delete` key with option
    /// held, which is a BACKSPACE, and which tmux emits two ways depending on
    /// whether `modifyOtherKeys` has been requested: `ESC \x7f` without it and
    /// `CSI 27;3;127~` with it. Both were measured on a private tmux server
    /// configured as his is, and `vt` decodes both to Backspace+ALT — so the
    /// binding does not depend on which of the two a given session negotiated.
    AltBackspace,
    Delete,
    PageUp,
    PageDown,
    /// Case-significant.
    Char(char),
    /// Always the lowercase letter.
    Ctrl(char),
    /// Always the lowercase letter.
    ///
    /// **ON A MAC THIS KEY MAY NEVER ARRIVE, AND THAT IS NOT A DEFECT IN THIS
    /// TABLE.** Measured in round five, from his own iTerm2 preferences
    /// (`com.googlecode.iterm2`, the Default profile, which is also the default
    /// bookmark): `Option Key Sends = 0` and `Right Option Key Sends = 0` —
    /// NORMAL. With that setting macOS's own keyboard layer resolves
    /// `opt-<letter>` to a COMPOSED GLYPH before any escape sequence exists:
    /// `opt-m` is `µ`, `opt-f` is `ƒ`, `opt-s` is `ß`, `opt-r` is `®`, `opt-/`
    /// is `÷`, and `opt-{e,i,n,u,\`}` are DEAD KEYS that emit nothing at all
    /// until the next keystroke. The program receives text, not a modifier,
    /// and no parser downstream can undo that.
    ///
    /// **WHICH IS WHY THE `opt-` ARROWS WORK AND THE `opt-` LETTERS DO NOT.**
    /// An arrow has no composed character, so it still arrives as
    /// `CSI 1;3<A-D>` and [`Key::AltUp`] and friends are delivered normally.
    /// That asymmetry is the whole diagnosis.
    ///
    /// **THE FIX WAS ONE SETTING AND HE REFUSED IT — WHICH IS WHY NO `opt-`
    /// LETTER IS LEFT IN THIS TABLE.**
    ///
    /// Round five's answer was to ask him to change his terminal: iTerm2 →
    /// Settings → Profiles → Keys → General → Left Option key → `Esc+`, after
    /// which every binding would arrive as `ESC <letter>` and `vt` would decode
    /// it to `Key::Alt(letter)` with nothing in this crate to change. **That
    /// ASK IS RETIRED and must not be restored here or anywhere.** His answer
    /// was better than ours: *"ctrl-* might be better because it doesn't have
    /// keybindings and it's universally understood to work. Let's just switch
    /// to all ctrl-* across the app instead of opt-*."* — so round six moved
    /// every `opt-<letter>` and `opt-<punct>` binding onto a ctrl key and the
    /// face depends on no terminal setting at all.
    ///
    /// **WHAT THIS VARIANT STILL CARRIES, and why it is not dead code**: the
    /// ARROW and NAMED-KEY family — [`Key::AltUp`], [`Key::AltDown`],
    /// [`Key::AltLeft`], [`Key::AltRight`], [`Key::AltHome`], [`Key::AltEnd`],
    /// [`Key::AltBackspace`], [`Key::AltEnter`] — plus `Key::Alt(char)` itself,
    /// which is now bound NOWHERE and is kept because `vt` still produces it
    /// and a decoder that could produce an unrepresentable key would be worse
    /// than an unbound one. `ModKeys` is still live for the same reason: those
    /// eight spellings still have to print `opt-` on his Mac.
    ///
    /// Mapping the composed glyph BACK to a modifier was considered and
    /// refused: it is a corrector standing downstream of a producer, and it
    /// would cost him the ability to type the glyph at all.
    Alt(char),
}

/// **HOW THIS HOST SPELLS THE MODIFIER KEYS — a fact about the keyboard in
/// front of him, not about the program.**
///
/// His words, twice: *"alt- doesn't exist on my mac. cmd-q does something
/// different."* and, on the fix, *"make sure it's operating system agnostic."*
/// So the descriptor is CHOSEN BY HOST rather than hard-coded, and it is chosen
/// through a table over the host name — never a `#[cfg(target_os = "macos")]`
/// special case, which is the shape he ruled out by asking for OS-agnostic by
/// name.
///
/// **IT IS THE SAME MODIFIER, CORRECTLY NAMED.** There is no third key class
/// and no new grammar: `Key::AltUp` is one binding that prints `opt-↑` on a
/// Mac and `alt-↑` everywhere else. A second modifier here would be a second
/// grammar to learn, which is the opposite of what he asked for.
///
/// It is data, so the pure crate can hold it; the SHELL detects the host and
/// hands the answer in on [`Caps`], exactly as it hands the negotiated
/// capabilities in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModKeys {
    /// What the ALT/OPTION modifier is called here, WITH its separator:
    /// `"alt-"` or `"opt-"`.
    pub alt: &'static str,
}

impl ModKeys {
    /// What every terminal that is not a Mac calls it.
    pub const ALT: Self = Self { alt: "alt-" };
    /// What HIS keyboard calls it, and how he spells it himself.
    pub const OPT: Self = Self { alt: "opt-" };

    /// **THE HOST TABLE.** Takes the host name (`std::env::consts::OS` in the
    /// shell) rather than reading it here, so the pure crate stays pure and the
    /// mapping is a value a test can drive with any host string.
    pub fn for_host(os: &str) -> Self {
        match os {
            // The two hosts whose keyboards carry an OPTION key.
            "macos" | "ios" => Self::OPT,
            _ => Self::ALT,
        }
    }
}

impl Default for ModKeys {
    /// `alt-`, because that is what the great majority of hosts call it and
    /// because a default that names HIS keyboard would be the hard-coding he
    /// ruled out.
    fn default() -> Self {
        Self::ALT
    }
}

impl Key {
    /// How this key is written in the help overlay and the footer, **spelled
    /// for the host he is sitting at** — see [`ModKeys`].
    pub fn label(self, m: ModKeys) -> String {
        match self {
            Key::Up => "↑".into(),
            Key::Down => "↓".into(),
            Key::Left => "←".into(),
            Key::Right => "→".into(),
            Key::Enter => "enter".into(),
            Key::AltEnter => format!("{}enter", m.alt),
            Key::ShiftEnter => "shift-enter".into(),
            Key::CtrlEnter => "^enter".into(),
            Key::Esc => "esc".into(),
            Key::Tab => "tab".into(),
            Key::Home => "home".into(),
            Key::End => "end".into(),
            Key::AltHome => format!("{}home", m.alt),
            Key::AltEnd => format!("{}end", m.alt),
            Key::AltLeft => format!("{}←", m.alt),
            Key::AltRight => format!("{}→", m.alt),
            Key::AltUp => format!("{}↑", m.alt),
            Key::AltDown => format!("{}↓", m.alt),
            Key::CtrlLeft => "^←".into(),
            Key::CtrlRight => "^→".into(),
            Key::CtrlUp => "^↑".into(),
            Key::CtrlDown => "^↓".into(),
            Key::Backspace => "bksp".into(),
            Key::AltBackspace => format!("{}bksp", m.alt),
            Key::Delete => "del".into(),
            Key::PageUp => "pgup".into(),
            Key::PageDown => "pgdn".into(),
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("^{}", c.to_ascii_uppercase()),
            Key::Alt(c) => format!("{}{c}", m.alt),
        }
    }
}

/// **WHAT THE TERMINAL ACTUALLY NEGOTIATED.**
///
/// The rule that kills an entire class of bug: **the `?` overlay and the footer
/// render FROM THIS**, so a binding that cannot work in his terminal is never
/// shown to him. The single most repeated defect across this whole generation
/// of terminal agents is advertising `shift-enter` on stacks that cannot report
/// it (codex#21699/#16121, claude-code#26629, pi#9456, aider#2197 — whose
/// maintainers state flatly that "there is no portable way to detect it"). A
/// promised key that does nothing is a glitch, and this type is why one cannot
/// be promised here.
///
/// It is data, so the pure crate can hold it; the SHELL does the negotiating
/// and hands the answer in on the [`crate::ViewState`], exactly as it hands
/// `now` and the width in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Caps {
    /// Whether modified keys that are otherwise indistinguishable —
    /// `shift-enter` against `enter`, `^enter` against `^J` — arrive as
    /// distinct events IN A FORM THIS PROGRAM CAN READ.
    ///
    /// Two different mechanisms can set it and the shell says which: the kitty
    /// keyboard protocol outside tmux, and tmux's own `modifyOtherKeys` when
    /// its `extended-keys-format` is `csi-u`. It is FALSE on the default tmux
    /// format, and that is not a failure to try — see `term::negotiate`.
    pub enhanced_keys: bool,
    /// Whether a paste arrives as ONE event rather than as a burst of
    /// keystrokes. Claimed by `?2004h`; the composer needs it and the face
    /// claims it now.
    pub bracketed_paste: bool,
    /// **WHAT THIS HOST CALLS THE MODIFIER** — see [`ModKeys`].
    ///
    /// It rides here rather than on a field of its own because this type is
    /// already exactly "the environment facts the shell measures and hands to
    /// the two readers of the table", and both readers already take it: every
    /// one of these fields exists for one purpose, which is deciding what the
    /// `?` overlay and the footer PRINT.
    pub mods: ModKeys,
}

/// What a binding NEEDS from the terminal before it may be advertised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Works with zero protocol, on any terminal, inside or outside tmux.
    Always,
    /// Works only when [`Caps::enhanced_keys`] is true.
    Enhanced,
}

impl Need {
    pub fn met(self, caps: Caps) -> bool {
        match self {
            Need::Always => true,
            Need::Enhanced => caps.enhanced_keys,
        }
    }
}

/// What a keystroke MEANS. The shell matches on this exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// **THE UP GESTURE, RESOLVED BY THE SURFACE IT LANDS ON**: a viewport in
    /// a transcript (clearing follow-mode), a CURSOR in the two list modes and
    /// in a thread's client queue. One intent, because it is one gesture — see
    /// `App::scroll_key`, which also holds the edge rule that lets it leave
    /// the region it was pressed in.
    ScrollUp,
    /// Down one row. Re-arms follow-mode on reaching the bottom.
    ScrollDown,
    PageUp,
    PageDown,
    /// Oldest retained row.
    Top,
    /// Newest row, and RE-ARM follow-mode. This is the "↓ N new" chip's key.
    Bottom,
    /// Open the selected project / thread, or take the selected message.
    ///
    /// **AT THE ROOT IT ALSO EXPANDS A BAND**, and that is not a second
    /// meaning. `→` DRILLS INTO WHAT THE CURSOR NAMES; on a thread row that is
    /// its sheet, on a project row that is its threads, and on a BAND HEADER
    /// the thing named is the band, so drilling into it is seeing all of it.
    /// One law, three rows.
    Open,
    /// **MOVE THE PINNED ROW UNDER THE CURSOR UP ONE**, and take the cursor
    /// with it. `ctrl-↑` at the root.
    ///
    /// The face computes NO order of its own: this becomes
    /// `ThreadAct::Move(Dir::Up)`, which `acts::route` turns into a `MovePin`
    /// naming a NEIGHBOUR, and the AUI renumbers. See `App::move_pin`.
    MovePinUp,
    /// **MOVE THE PINNED ROW UNDER THE CURSOR DOWN ONE**, cursor travelling.
    /// `ctrl-↓` at the root.
    MovePinDown,
    /// **MOVE THE CURSOR TO ROW `n` AND STOP** — what a CLICK on a plain row
    /// means since the operator retracted click-is-the-same-op-as-right:
    ///
    /// > a click on a row moves the cursor to that row. It does not drill, does
    /// > not expand, does not activate the row's default act.
    ///
    /// It is an INTENT rather than something the click handler does on the
    /// side, because that is what keeps the retraction inside the one
    /// resolution table ([`crate::hit::activation`]) instead of splitting the
    /// mouse's grammar across two files. It has no key: `↑`/`↓` are how the
    /// keyboard moves a cursor, and a second spelling would be a binding the
    /// `?` overlay would have to explain twice.
    Select(usize),
    /// **ACTIVATE WHATEVER IS UNDER THE CURSOR.** Not an action: a
    /// RE-ENTRY. The shell resolves the cursor to a [`crate::hit::Target`],
    /// asks [`crate::hit::activation`] what that target means in this mode, and
    /// dispatches the intent it gets back — the SAME call a click makes with
    /// the target it hit.
    ///
    /// This variant exists so that `→` and a click cannot drift: there is one
    /// resolution function, and neither gesture has an arm the other does not.
    /// It never reaches the shell's intent handler as itself.
    Activate,
    /// Leave the thread / close the overlay / leave selection mode.
    ///
    /// **`←` IS THE TRIGGER IN A THREAD NOW, AND `esc` IS NOT.** His words:
    /// *"And left should go back. I don't even want ESC to do that behavior."*
    /// The BACK-STACK is untouched — `origin`, `descend`, `ascend` and the
    /// raw-list removal all stand; what changed is which key reaches them.
    Back,
    /// **MOVE THE SELECTION TO THIS REGION AND DO NOTHING ELSE.**
    ///
    /// `esc`'s meaning outside the composer — every region needs a way home
    /// that is not three arrows — and the first half of what a CLICK does.
    ///
    /// It is deliberately NOT a descent: a region is not a door, so it must
    /// never be pushed onto the back-stack, or `←` would "go back" to a band.
    Region(Region),
    /// **AGENTS: THE ROW UNDER THE CURSOR BECOMES THE SUBJECT** — its chat
    /// history above, its composer (or the stand-in that says why there is
    /// none) below.
    ///
    /// > *we should be able to navigate to those subagents in the tree view
    /// > below, and if we click right or enter on them we show their chat
    /// > history instead of the parent.*
    ///
    /// It carries the ROW INDEX rather than reading the cursor, so the one
    /// resolution in [`crate::hit::activation`] answers for the key exactly as
    /// it would for any other gesture aimed at a row.
    ///
    /// **It is not a descent.** Drilling changes which chat the transcript is
    /// projecting, not which screen he is on; `←` un-drills one level and only
    /// then leaves the thread, or drilling in and pressing `←` would eject him
    /// from the thread he was reading.
    Drill(usize),
    /// **CHAT: SCROLL TO HIS PREVIOUS MESSAGE.** Navigation by AUTHORSHIP —
    /// *"let's make opt-{up,down} jump to the next/previous user message"* —
    /// so the message's first row heads the band and follow-mode goes off.
    /// Nothing happens when there is no earlier message of his.
    JumpUserPrev,
    /// **CHAT: HIS NEXT MESSAGE — and past the last one, THE LIVE STATE.**
    ///
    /// *"opt-down to the bottom should go to the live state of the convo."*
    /// That second act is [`Intent::Bottom`]'s exact act (newest + follow
    /// re-armed + thawed) and NOT a scroll to the last row: a screenshot
    /// cannot tell those apart and the `new_since` chip can.
    JumpUserNext,
    /// **THE ROOT: TO AND BETWEEN BAND HEADERS.** *"opt-{up,down} in the main
    /// menu should jump to and between band headers."* The third meaning of
    /// the same key pair, and the third region resolution rather than a third
    /// grammar: the band headers are already cursor stops.
    BandPrev,
    /// The root: the NEXT band header below the cursor.
    BandNext,
    /// Swap between the thread list and the fleet's event lane.
    SwapView,
    /// Raise the `?` overlay.
    Help,
    /// **SHOW ONLY WHAT WAS SAID — AND, PRESSED AGAIN, EVERYTHING AGAIN.**
    ///
    /// His ask: *"give me a toggle for the info blocks. Until we iron out the
    /// massive wave of agent messages I just need a way to hide everything
    /// except stdout."*
    ///
    /// **ONE INTENT AND NOT TWO**, for the reason [`Intent::MicRecord`] is
    /// one: the thing it flips is a single state, and two keys for it would be
    /// two ways to get out of step with it.
    ///
    /// **AND IT IS ONE BOOLEAN, NOT A FILTER.** The nine-group lane filter
    /// this used to be a shortcut into is DELETED — *"filter is too much. Just
    /// drop that feature… I had that request because I wanted a simple
    /// toggle."* What it flips is `ViewState::stdout_only`; there is nothing to
    /// stash and nothing to restore, because there is no tuned state it could
    /// be collapsing away from.
    ///
    /// **IT IS THREAD-SCOPED** — *"I'm seeing speech on the menu and don't
    /// know what it's for in that context. It should be in the thread
    /// context."* See [`Intent::is_thread_scoped`], which is the machine that
    /// keeps it off the root's table rather than this comment.
    StdoutOnly,
    /// Raise the queue view for the thread in focus.
    ///
    /// **Renamed from `Queue` when the composer landed**, because there are now
    /// two queues and one of them is the CLIENT's. This one opens the view of
    /// the DAEMON's; [`Intent::QueueMessage`] sets a composed message aside in
    /// ours. Two acts that were one word are two words.
    QueueOverlay,
    /// **SET THE COMPOSED MESSAGE ASIDE.** His `left`, mirrored from the AUI:
    /// the buffer empties, the entry joins the client's queue, and nothing goes
    /// to the daemon.
    QueueMessage,
    /// **SEND THE WHOLE PAYLOAD.** His `wildcard`: every queued entry plus
    /// whatever is in the buffer, as ONE message, in ONE `threads/{id}/send`.
    Send,
    /// **ENTER OR LEAVE MIC MODE.** Refuses while the textarea holds content —
    /// `DIR-5`'s constraint, which he himself labelled NAIVE: *"don't toggle it
    /// if there is content in the textarea, to avoid out of order concerns.
    /// That's a naive constraint we'll apply for now."* It is implemented as
    /// the provisional guard he described and is expected to be replaced by a
    /// mechanism that removes the ordering hazard, not defended on its own
    /// terms.
    MicMode,
    /// **START OR STOP RECORDING.** One key, both acts, because the thing it
    /// toggles is one state and two keys for it would be two ways to get out of
    /// step with it.
    MicRecord,
    /// **PLAY THE CAPTURE BEHIND QUEUE ROW `n`** — the play button, reached by
    /// a click on it. A second press stops it.
    MicPlay(usize),
    /// **PLAY THE MOST RECENT CAPTURE** — the play button's keyboard spelling.
    ///
    /// It is a SEPARATE intent from [`Intent::MicPlay`] rather than a sentinel
    /// index, because a sentinel is a value the shell has to remember to
    /// special-case and a variant is one the compiler makes it handle.
    MicPlayLast,
    /// Retry the FIRST failed entry — the keyboard's spelling of the retry
    /// button.
    MicRetryFirst,
    /// Take the FIRST failed entry out — the keyboard's spelling of drop. It
    /// exists so a failure can be cleared without a mouse.
    MicDropFirst,
    /// **TRANSCRIBE QUEUE ROW `n` AGAIN** after a failure. The audio was kept;
    /// this is what makes keeping it worth something.
    MicRetry(usize),
    /// **TAKE QUEUE ROW `n` OUT**, capture and all. The way a failed entry
    /// leaves without being sent.
    MicDrop(usize),
    /// One act on the composer's buffer. The composer's grammar is in the table
    /// like everything else — a binding written in the shell would be a binding
    /// the `?` overlay could not render.
    Compose(Edit),
    /// A printable character, typed into the composer. The `char` travels with
    /// the intent because one row of the table stands for the whole printable
    /// alphabet, and the alternative was a row per character.
    Type(char),
    /// **TAKE the recovered draft the composer is offering.** Reachable only
    /// while an offer is standing; a no-op otherwise, exactly as `^E` is a
    /// no-op with nothing folded.
    AcceptDraft,
    /// **LET THE RECOVERED DRAFT GO**, and delete its journal file.
    DiscardDraft,
    /// **THE WHOLE-TRANSCRIPT RAW ESCAPE.** Every fold off, every clipped
    /// payload whole, every row that renders nothing by default rendered as its
    /// raw self. It is `DEC-19`'s "nothing is dropped" promise made reachable,
    /// and it is the ONE thing that still reaches a stream chunk now that
    /// deltas render as the message they are building (`DEC-31`).
    Expand,
    /// **OPEN ONE ROW, AND ONLY THAT ROW** (`IS-31`).
    ///
    /// Carries the block index it was activated on, which [`Intent::Expand`]
    /// never could — and which is the whole difference between "show me what is
    /// behind this" and "un-fold the transcript". The shell resolves the index
    /// to the row's LEDGER identity before remembering it, because the index is
    /// only true of the projection that produced it.
    ExpandBlock(usize),
    /// **PUT THE CARET WHERE HE CLICKED** — *"to that position, or the nearest
    /// legal one."*
    ///
    /// `row` indexes the composer's FULL visual layout (not the window the band
    /// is showing) and `col` is the SCREEN column the pointer was at; the shell
    /// subtracts the band's gutter and asks the composer itself to snap the
    /// result to a grapheme boundary. Both facts come from the frame that was
    /// on screen, which is the only frame either of them is true of.
    ///
    /// It has no key, and cannot: the keyboard has no coordinate. Its keyboard
    /// counterpart is the whole motion grammar — the arrows, the word keys and
    /// the limit keys.
    Caret {
        row: usize,
        col: u16,
    },
    // ── THE SHEET (§5) ───────────────────────────────────────────────────
    /// **OPEN THE SHEET FOR THE THREAD AT SELECTABLE ROW `n`** of whatever
    /// surface is up — the home bands, or a project's thread list.
    ///
    /// §5's door, and it is a CLICK ON THE TITLE rather than on the row. The
    /// operator's standing retraction (`crate::hit`'s header) is that a click
    /// on a plain row only moves the cursor; the same ruling names *"sheet
    /// action rows"* among the explicit affordances that still activate, and a
    /// title is the same kind of thing — a named affordance inside a row, like
    /// the expander two columns to its left.
    OpenSheet(usize),
    /// **OPEN THE THREAD AT SELECTABLE ROW `n`** — the cursor moves to it and
    /// it opens, in that order and as one act.
    ///
    /// It exists because a CLICK carries a row index and [`Intent::Open`] does
    /// not: `Open` acts on wherever the cursor already is, which is right for
    /// `→` and useless to a pointer that has just landed somewhere else. The
    /// two are not merged, because a merge would mean either the key growing a
    /// coordinate it does not have or the click pre-moving the cursor behind
    /// the table's back — which is the second-derivation defect this whole
    /// module is built to prevent.
    ///

    /// **OPEN THE OPTIONS FOR THE THREAD UNDER THE CURSOR.** `o`, and it is the
    /// keyboard door onto the sheet.
    ///
    /// **His words**: *"I want to be able to jump straight into the options of
    /// a thread by clicking o while my cursor is on the thread handle."*
    ///
    /// It is its own intent rather than an [`Intent::OpenSheet`] carrying a
    /// cursor, because the key table must not read the cursor — the shell
    /// aims it at a [`crate::hit::Target::ThreadTitle`] and puts it through
    /// the same `activation` table a click goes through, so the two doors onto
    /// the sheet cannot drift apart. `hit.rs` already had the `Via::Key` arm
    /// waiting for exactly this, with a comment saying its absence was *"a
    /// fact about the key table, not a hole in this one"*; this is the key
    /// table catching up.
    Options,
    /// **RENAME THE THREAD UNDER THE CURSOR.** Bare `r`, and it is the key he
    /// was promised twice.
    ///
    /// **His words**: *"On any thread throughout the application, I need to be
    /// able to rename with a tap of the letter r."* And, this turn, after two
    /// rounds in which the act existed and the key did not: *"I stil can't do
    /// a rename of a thread."*
    ///
    /// # It DISPLACED `Reload`, and that is a decision he may overturn
    ///
    /// Bare `r` meant [`Intent::Reload`] in every body mode, from
    /// `common_rows`. The manager of round three ruled the collision by
    /// CONTEXT, exactly as `p` was ruled: **on a surface where a thread row
    /// can be under the cursor, bare `r` is RENAME; `^R` is RELOAD
    /// everywhere, unchanged.** Reload loses nothing it was promised —
    /// `common_rows`' own doc comment argues that `ctrl-<letter>` is the
    /// documented grammar and the bare letters are *"an extra that works where
    /// it can work, never the documented route"*, and `^R` is the form both
    /// readers of this table print. The bare letter goes to the act that can
    /// only mean one thing on a thread row.
    ///
    /// **`Mode::Activity` keeps bare `r` as RELOAD**, because the event lane
    /// has no thread row to stand on — which is why that row is written out
    /// per mode below rather than left inside `common_rows`.
    ///
    /// Like [`Intent::Options`] it carries no cursor: the shell aims it at the
    /// thread the cursor is already on and goes through the SAME sheet door a
    /// click and `o` go through, so there is one way for a rename to begin.
    Rename,
    /// **`p` AT THE ROOT — PIN THE THREAD UNDER THE CURSOR, OR ADD A
    /// PROJECT.** One letter, two acts, **resolved by what the cursor is
    /// standing on** and not by a second key.
    ///
    /// That is a coordinator's ruling carried verbatim: *"Same letter as
    /// `pin`, resolved by CONTEXT — `p` on a thread row pins; `p` not on a
    /// thread adds a project."* The reason it is ONE intent rather than two
    /// bound to one key is that this table must not read the cursor: a
    /// `Binding` knows the MODE and nothing else, so a table that branched on
    /// what is under the cursor would be a second cursor resolution living
    /// beside `ProjectList::item`. The shell asks `item()` — the ONE
    /// resolution — and does one of the two things.
    ///
    /// **The split is total on the root and the root is the only surface it is
    /// bound on.** `Mode::Projects` lays out exactly five kinds of selectable
    /// row (`RootItem`): a pinned thread, a recent thread, a project, `+ new
    /// project` and `archived`. The first two are threads and the last three
    /// are not, with no row that is both and no row that is neither. On
    /// `Mode::List` every selectable row IS a thread, so `p` there can only
    /// ever mean pin and is not bound here — that is the thread-act worker's
    /// letter and this one does not take it.
    AddProject,
    /// **`c` AT THE ROOT — DE-REGISTER THE PROJECT UNDER THE CURSOR.**
    ///
    /// *"`c` plus confirmation, `Enter` or a second `c`. Reuse the
    /// confirmation machinery that already exists; two confirmation grammars
    /// on one face is how they drift apart."*
    ///
    /// **It writes no confirmation of its own.** It opens the project's own
    /// sheet and presses the sheet's `remove` row, which is §5's `asks first`:
    /// the first press ARMS the row and says so in words, and only a second
    /// press sends anything. So `c` `c` de-registers, and so does `c` then
    /// `enter` — because after the first `c` the armed row is the selected
    /// row and `enter` activates the selected row, which is the sheet's
    /// ordinary grammar and not a second rule. Exactly the shape
    /// [`Intent::Rename`] uses to reach the sheet's rename field.
    DeregisterProject,
    /// **`tab` IN THE ADD-A-PROJECT FIELD — COMPLETE THE PATH.**
    ///
    /// The field takes a NAME or a PATH (`crate::Intent::CreateProject`), and
    /// a path is long and he should not have to type it. It completes to the
    /// longest unambiguous prefix, appends `/` when exactly one directory
    /// matched, and says what still matches in the notice line when several
    /// do — the shell's `index::complete_path` does the reading.
    ///
    /// **It is a completion INTO THE FIELD and not a picker overlay.** A
    /// picker is `render/src/{model,render,select}.rs`; this is the same
    /// capability with no new surface, which is what lets it land beside a
    /// sibling who is editing those files.
    CompletePath,
    /// **ACTIVATE ACT ROW `n` OF THE SHEET.** One variant for the click and the
    /// key alike, so the two cannot drift.
    ///
    /// A row whose owner is unreachable does NOT get a different intent: it
    /// gets this one, and the shell answers by putting the row's own sentence
    /// in the notice line and doing nothing else. Branching here would have
    /// made "unavailable" a thing the key table knows about, and the key table
    /// must not know which process owns a row.
    SheetAct(usize),
    /// **OPEN THE PROJECT SHEET FOR ROOT ROW `n`** — §5's second frame.
    ///
    /// Its own intent rather than a flag on [`Intent::OpenSheet`], because the
    /// two resolve different things: `OpenSheet` resolves a THREAD out of the
    /// band the cursor is in, and this resolves a PROJECT out of the root's
    /// project rows. One intent with a subject flag would have made the shell
    /// ask "which kind of row was that" twice.
    OpenProjectSheet(usize),
    /// **BRING ARCHIVED PROJECT `n` BACK.** One variant for the click and the
    /// key alike, so the two cannot drift — the sheet's rule.
    Unarchive(usize),
    /// **OPEN THE `+ new project` FIELD**, or, when it is already open, do
    /// nothing (he is already typing into it).
    NewProjectField,
    /// **CREATE THE PROJECT WITH WHAT IS TYPED.** `enter`, because a name is
    /// one short answer and not a document — §1: *"no confirm scope; read-back
    /// exists for voice, not for typing."*
    CreateProject,
    /// **COMMIT WHAT IS TYPED IN THE SHEET'S FIELD** — the rename, or the
    /// compaction with its instructions. `enter`, because the field is one
    /// short answer and not a document; `alt-enter` is still a newline.
    SheetCommit,
    /// Re-ask the daemon for everything this view shows.
    Reload,
    /// Force a full repaint. A user-reachable set-point of the repaint law:
    /// every OTHER set-point is something the program can detect, and this one
    /// is the door for the moments it cannot.
    Redraw,
    /// Clean exit with the terminal restored.
    Quit,
    /// This key means nothing in this mode. Not an error — most keys do not.
    None,
    /// **RECORD NOW, FROM WHEREVER HE IS — his *"regardless"*.**
    ///
    /// His words: *"after doing r to record then it went into the queue and
    /// then I no longer could do r to record another one. I need opt-r to let
    /// me do that regardless."* Read literally, that is not a change to a
    /// guard: it is a binding that RECORDS in one keystroke from any surface
    /// inside a thread, entering mic mode on its way if it has to.
    ///
    /// It is a SECOND ACT and not a second spelling of [`Intent::MicRecord`],
    /// because the two differ in what they do when nothing is recording in a
    /// mode that is not `Mode::Mic`: `MicRecord` is a TOGGLE that only exists
    /// inside mic mode, and this one is a DOOR plus a start. Collapsing them
    /// would put a mode change on the key the hold detector reads as a
    /// gesture, and `Hold` is fed this act's absence — see `App::on_intent`.
    ///
    /// **AND IT IS DELIBERATELY LAST IN THIS ENUM.** Round six's fragment:
    /// a variant added in the middle is a merge conflict with the sibling
    /// deleting variants from the same enum.
    MicRecordNow,
    /// **`n` ON THE MAIN PAGE — START A NEW THREAD IN `miscellaneous`, AND
    /// OPEN IT.**
    ///
    /// **His words**: *"ok and where is the n for new thread. It should work on
    /// the main page and create a misc thread. I need that asap."* `n` was
    /// bound to nothing at all, which is the whole of the defect: he had a
    /// thread client that could not start a thread.
    ///
    /// It is the daemon's act and the daemon's alone — whether a thread EXISTS
    /// is the daemon's state, and there is no `CreateThread` in the audio
    /// interface's mutation vocabulary to relay it through. See
    /// `crate`'s consumer, `tui::acts::route_new_thread`, for the bytes and
    /// for the one thing this act does NOT do (put the thread on his list).
    ///
    /// **BARE `n`, and it is free**: the root's grammar is `→`/`enter`, `←`,
    /// `tab`, the eight `root_acts` (`o`, `r`, `p`, `c`, `ctrl-↑`, `ctrl-↓`),
    /// the six `scroll_rows` and the five `common_rows` (`?`, `u`, `r`, `^L`,
    /// `q`/`^C`). `n` is in none of them, and
    /// `one_key_one_meaning_in_one_mode` is the machine that says so rather
    /// than this sentence.
    ///
    /// **IT IS BOUND ON `Mode::Projects` ONLY, not through `root_acts`.** That
    /// macro is stamped on the root AND on a band's `see all` page, and this is
    /// *"the MAIN PAGE"* in his words — a `see all` page is one band's own
    /// surface, not the menu. It is also not a ROW act: every other member of
    /// that macro acts on whatever the cursor is standing on, and this one acts
    /// on nothing.
    ///
    /// **A variant added after [`Intent::MicRecordNow`]**, whose note above
    /// asks for exactly that: the end of the enum is where a new row costs no
    /// sibling a merge conflict.
    NewThread,
}

impl Intent {
    /// **DOES THIS ACT ONLY MEAN SOMETHING ON A THREAD'S SCREEN?**
    ///
    /// It exists because of a measured SCOPING DEFECT, and it is the mechanical
    /// half of the fix. His words: *"I'm seeing speech on the menu and don't
    /// know what it's for in that context. It should be in the thread
    /// context."* What he was reading was `stdout` — then spelled `speech` —
    /// in the ROOT'S FOOTER, at rank 2, because the toggle had been added to
    /// `common_rows`: a macro stamped onto `Mode::Projects`, `Mode::List` and
    /// `Mode::Activity` in one go, with no notion of scope at all. Anything
    /// dropped into that bucket is published to the root BY CONSTRUCTION.
    ///
    /// A comment saying "don't do that" is not a mechanism, so this is the
    /// mechanism: `no_non_thread_mode_names_a_thread_scoped_act` walks
    /// [`BINDINGS`] and fails if any non-thread mode's row carries an act this
    /// returns `true` for. Putting the toggle back in `common_rows` reddens
    /// that test, which is what "found by a check rather than by an eyeball"
    /// means here.
    ///
    /// **THE LIST IS DELIBERATELY SMALL AND IT IS A LIST.** An act is
    /// thread-scoped when its subject is THE OPEN THREAD'S transcript, its
    /// composer, its client queue or its subagent band — things that do not
    /// exist on a surface that is not a thread. It is NOT "acts a thread
    /// happens to bind": `Intent::Help`, `Intent::Quit` and the scroll family
    /// are thread-bound and belong everywhere. Nor is it "acts about a thread":
    /// `Intent::Rename` and `Intent::Options` act on the thread UNDER THE
    /// CURSOR, which is exactly a root/list act.
    ///
    /// `Intent::Send`, `Intent::QueueMessage`, `Intent::Compose` and
    /// `Intent::Type` are in the list and are also bound in `Mode::SheetField`
    /// and `Mode::NewProject`, which have buffers of their own. Those are not
    /// non-thread BODY modes — the check walks the four body modes that are not
    /// a thread — so the two facts do not collide; see the test.
    pub fn is_thread_scoped(self) -> bool {
        matches!(
            self,
            // the transcript
            Intent::StdoutOnly
                | Intent::Expand
                | Intent::ExpandBlock(_)
                | Intent::JumpUserPrev
                | Intent::JumpUserNext
                // the composer and the client queue
                | Intent::Send
                | Intent::QueueMessage
                | Intent::Compose(_)
                | Intent::Type(_)
                | Intent::Caret { .. }
                | Intent::AcceptDraft
                | Intent::DiscardDraft
                // mic mode, which is a thread's composer wearing another hat
                | Intent::MicMode
                | Intent::MicRecord
                | Intent::MicRecordNow
                | Intent::MicPlay(_)
                | Intent::MicPlayLast
                | Intent::MicRetry(_)
                | Intent::MicRetryFirst
                | Intent::MicDrop(_)
                | Intent::MicDropFirst
                // the subagent band
                | Intent::Drill(_)
        )
    }
}

/// One act on the composer's buffer.
///
/// **Two of these DEFER to the transcript when the composer cannot serve them**
/// — [`Edit::Up`]/[`Edit::Down`] at the composer's first/last row, and
/// [`Edit::RowStart`]/[`Edit::RowEnd`] on an empty buffer. That is the whole of
/// the "escape" rule and it is deliberately STATELESS: it depends only on
/// things he can see (is there text, is the caret on the first row), never on a
/// focus mode he would have to track. The shell owns the deferral because only
/// the shell knows what the transcript would do instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Newline,
    Backspace,
    Delete,
    Left,
    Right,
    WordLeft,
    WordRight,
    /// Up one VISUAL row, or the transcript when the caret is already at the
    /// top of the composer.
    Up,
    /// Down one visual row, or the transcript at the bottom.
    Down,
    /// **THE TOP OF THIS PARAGRAPH, OR THE TOP OF THE PREVIOUS ONE.** `opt-↑`.
    ///
    /// His words: *"opt-{up,down} could move paragraphs with similar logic,
    /// opt-up to the top of the current or top of the previous, opt-down to
    /// the bottom of the current or bottom of the next"* — and, in the same
    /// breath, *"they should stop at the beginning and end, they should not
    /// escape the textarea."* So unlike [`Edit::Up`] this one NEVER defers.
    ///
    /// **A PARAGRAPH IS ONE HARD-BREAK LOGICAL LINE** — the `lines[i]` the
    /// composer already stores, regardless of soft wrap — so `↑` climbs one
    /// VISUAL row and `opt-↑` goes to the logical line's start in one press.
    /// That difference is the whole reason the key exists. He did not define
    /// the word; the blank-line-delimited alternative is one word away and
    /// would collapse most of his drafts to a single paragraph.
    ParaUp,
    /// The end of this logical line, or the end of the next one. Never
    /// escapes. See [`Edit::ParaUp`].
    ParaDown,
    /// Start of the visual row, or the transcript's oldest row when the
    /// composer is empty.
    RowStart,
    /// End of the visual row, or the newest row when the composer is empty.
    RowEnd,
    BufStart,
    BufEnd,
    KillToEnd,
    KillToStart,
    KillWord,
    Yank,
    Undo,
    Redo,
    /// Spaces, never a tab — `compose::TAB_WIDTH` of them.
    Tab,
    /// Put the last queued entry back in the buffer. The undo of a queue.
    Unqueue,
}

/// The keys a binding answers to.
#[derive(Debug, Clone, Copy)]
pub enum KeySet {
    /// Every key. Used by the overlay-dismiss binding, so that "any key" is a
    /// row in the table like everything else rather than a special case in
    /// [`intent`].
    Any,
    /// **EVERY PRINTABLE CHARACTER.** One row standing for the whole alphabet,
    /// which is what makes the composer's "any letter is text" a ROW IN THE
    /// TABLE rather than a fall-through in the shell. It sits LAST in a mode's
    /// rows, so every specific binding above it still wins.
    Printable,
    These(&'static [Key]),
}

impl KeySet {
    pub fn contains(&self, key: Key) -> bool {
        match self {
            KeySet::Any => true,
            KeySet::Printable => matches!(key, Key::Char(c) if is_printable(c)),
            KeySet::These(keys) => keys.contains(&key),
        }
    }

    /// How the key column reads in the help overlay, spelled for this host.
    pub fn label(&self, m: ModKeys) -> String {
        match self {
            KeySet::Any => "any key".into(),
            KeySet::Printable => "any character".into(),
            KeySet::These(keys) => keys
                .iter()
                .map(|k| k.label(m))
                .collect::<Vec<_>>()
                .join(" / "),
        }
    }
}

/// Whether a character is one the composer should accept as TEXT.
///
/// Control characters are excluded here as well as at the buffer's door, and
/// the redundancy is deliberate: this one decides whether a keystroke means
/// "type this", and `compose::sanitize` decides what may be stored. A control
/// character that reached this predicate would become a no-op keystroke rather
/// than a stripped one, which is a different and worse answer — it would look
/// like a dead key.
fn is_printable(c: char) -> bool {
    !c.is_control() && c != '\u{feff}' && !(0x80..=0x9f).contains(&(c as u32))
}

/// One row of the table.
///
/// `Copy` so the shared-row macros below can name a row out of a `const` array
/// — the alternative was forty-five hand-written rows in which the scroll
/// grammar of three modes could quietly diverge.
#[derive(Clone, Copy)]
pub struct Binding {
    pub keys: KeySet,
    pub mode: Mode,
    pub intent: Intent,
    /// What the `?` overlay says this key does — room for the nuance.
    pub act: &'static str,
    /// The same act in footer-length. ONE row still owns the binding, so the
    /// two readers cannot disagree about WHICH key does WHAT; they differ only
    /// in how many columns they have to say it in.
    pub short: &'static str,
    /// Position in the footer hint line, lower first. `None` keeps it out of
    /// the footer (it is still in the overlay).
    ///
    /// The footer takes as many as fit and drops the rest, which is why the
    /// rank exists: at 40 columns the survivors should be the ones he cannot
    /// guess, and the door to all the others (`?`) is rank 0.
    pub footer_rank: Option<u8>,
    /// **WHAT THE TERMINAL MUST PROVIDE BEFORE THIS ROW MAY BE SHOWN.** See
    /// [`Caps`]. Both readers of the table filter on it, so an unavailable
    /// binding is not merely de-prioritised, it is INVISIBLE.
    pub needs: Need,
}

/// Scrolling, which means the same thing in all three body modes and is written
/// once per mode by [`scroll_rows`] rather than five times by hand.
macro_rules! scroll_rows {
    ($mode:expr) => {
        [
            // `k` and `j` are CUT — operator ruling, this round, one word:
            // "cut". The arrows and the wheel remain, and `^P`/`^N` remain as
            // aliases in the same row. Nothing was added to replace them:
            // scrolling already had a grammar, and a second one cost two
            // letters out of a letter-space he wants small.
            Binding {
                keys: KeySet::These(&[Key::Up, Key::Ctrl('p')]),
                mode: $mode,
                intent: Intent::ScrollUp,
                act: "up one row (leaves follow-mode)",
                short: "up",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Down, Key::Ctrl('n')]),
                mode: $mode,
                intent: Intent::ScrollDown,
                act: "down one row",
                short: "down",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::PageUp, Key::Ctrl('u')]),
                mode: $mode,
                intent: Intent::PageUp,
                act: "up one screen",
                short: "page up",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::PageDown, Key::Ctrl('d')]),
                mode: $mode,
                intent: Intent::PageDown,
                act: "down one screen",
                short: "page down",
                footer_rank: None,
                needs: Need::Always,
            },
            // **`g` AND `G` ARE RETIRED — the LABEL was wrong AND the
            // function was redundant, which is two findings in one sentence of
            // his.** *"G newest isn't accurate. Idk what it's supposed to do
            // but g and G are jumping to the top and bottom. We don't need
            // that because we have opt-{up,down}"*
            //
            // They were NOT relabelled. A correctly-labelled redundant key is
            // still a key he does not want, and the letters go back to the
            // small letter-space he keeps asking for. `home` and `end` stay:
            // they are the keys a terminal user reaches for, they are not
            // letters, and nothing he said was about them.
            Binding {
                keys: KeySet::These(&[Key::Home]),
                mode: $mode,
                intent: Intent::Top,
                act: "jump to the oldest row held",
                short: "top",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::End]),
                mode: $mode,
                intent: Intent::Bottom,
                act: "jump to newest and follow again",
                short: "newest",
                footer_rank: Some(6),
                needs: Need::Always,
            },
        ]
    };
}

/// The universal rows: present in every body mode.
///
/// **EVERY GLOBAL COMMAND ANSWERS TO `ctrl-<letter>` AS WELL AS ITS LETTER, and
/// the CTRL form is the one the overlay prints.** That is the composer's doing
/// and it is the narrowing of `0026`'s decision #4, argued in `0042`: with a
/// real always-focused textarea in a thread, a bare `f` cannot both type the
/// letter `f` and open the filter — so a thread's globals MUST be modified. The
/// research ruled that every mode should therefore move; this table keeps the
/// bare letters in the three modes that have NO composer, as unlisted synonyms,
/// because there they cost nothing and removing them would take `q` away from
/// the root menu to buy a uniformity the printed grammar already has.
///
/// The invariant that makes it safe: **`ctrl-<letter>` means the same thing in
/// every mode, including the thread**, so there is exactly one grammar to
/// learn and it never fails. The bare letters are an extra that works where it
/// can work, never the documented route.
///
/// # THE MODIFIER WAS `opt-` UNTIL ROUND SIX, AND WHY IT MOVED
///
/// His ruling, verbatim: *"opt-m give me µ, opt-r ® opt-f ƒ, etc. ctrl-* might
/// be better because it doesn't have keybindings and it's universally
/// understood to work. Let's just switch to all ctrl-* across the app instead
/// of opt-*."*
///
/// He is right about the world and the reason is measured: macOS resolves
/// `opt-<character>` to a COMPOSED GLYPH in the keyboard layer, so no escape
/// sequence was ever generated and this grammar's whole point — *"it never
/// fails"* — was false on the only keyboard that matters. Nothing composes under
/// `ctrl`, and every one of the 26 ctrl letters was measured reaching
/// `Key::Ctrl` intact through the shipped decoder (`0128` §F).
///
/// **THE MIGRATION IS LETTERS AND PUNCTUATION ONLY.** An ARROW has no composed
/// glyph — `CSI 1;3A` was measured arriving as `Key::AltUp` — so `opt-↑/↓/←/→`,
/// `opt-home`, `opt-end`, `opt-bksp` and `opt-enter` were never broken, and
/// moving them is the one place an irreducible collision exists (`^↑`/`^↓`
/// already carry `0119`'s reorder at the root and the buffer's limits in the
/// box). They stay exactly as they were.
///
/// **AND ctrl IS NOT THE EMPTY NAMESPACE the ruling assumes** — it is this
/// table's FULLEST. Eighteen of twenty-six letters were already bound, `^I` is
/// Tab, `^M` is Enter, `^J` is the newline and `^H` is Backspace, so the
/// assignments came from `0128`'s collision table rather than letter-for-letter:
/// the queue overlay is `^O` (`^U` is PageUp / kill-to-start), and four
/// composer-only acts have no mnemonic home there at all. See the notes on the
/// rows that lost one.
macro_rules! common_rows {
    ($mode:expr) => {
        [
            Binding {
                keys: KeySet::These(&[Key::Char('?'), Key::Ctrl('/'), Key::Ctrl('7')]),
                mode: $mode,
                intent: Intent::Help,
                act: "every key this face answers to, in THIS terminal",
                short: "keys",
                footer_rank: Some(0),
                needs: Need::Always,
            },
            // **THE STDOUT-ONLY TOGGLE IS NOT HERE, AND ITS ABSENCE IS
            // SCOPE 1b.** It used to be, on the argument that it was "a GLOBAL
            // of exactly the kind `f` is" — and that argument is how a
            // THREAD-scoped act came to be printed in the root's footer at
            // rank 2. His words: *"I'm seeing speech on the menu and don't
            // know what it's for in that context. It should be in the thread
            // context."*
            //
            // **THE MECHANISM, because the mechanism is the finding.** This
            // macro is a bucket with no notion of SCOPE: it is stamped onto
            // `Mode::Projects`, `Mode::List` and `Mode::Activity` in one go,
            // so anything dropped into it is published to the root BY
            // CONSTRUCTION, footer rank and all, whether or not the act means
            // anything there. `Intent::Filter` legitimately did mean something
            // on all three; the toggle was added beside it because it was "the
            // same state, reached the short way", and inherited a reach it had
            // no claim to.
            //
            // The toggle now lives in `alt_common_rows` only, and
            // `no_non_thread_mode_names_a_thread_scoped_act` is the machine
            // that keeps it there rather than this comment.
            Binding {
                keys: KeySet::These(&[Key::Char('u'), Key::Ctrl('o')]),
                mode: $mode,
                intent: Intent::QueueOverlay,
                act: "view the queue for the thread in focus",
                short: "queue",
                footer_rank: Some(3),
                needs: Need::Always,
            },
            // **THE BARE `r` IS NOT HERE ANY MORE, and its absence is the
            // whole of round three's ruling.** `^R` is reload in every body
            // mode, which is the DOCUMENTED form and the one both readers of
            // this table print. The bare letter is now spent per mode, below:
            // RENAME where a thread row can be under the cursor, and RELOAD in
            // the event lane, where one cannot. Writing it per mode is what
            // lets those two meanings exist without `one_key_one_meaning_in_
            // one_mode` being weakened to allow a shadow.
            Binding {
                keys: KeySet::These(&[Key::Ctrl('r')]),
                mode: $mode,
                intent: Intent::Reload,
                act: "re-ask the daemon for everything this view shows",
                short: "reload",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('l')]),
                mode: $mode,
                intent: Intent::Redraw,
                act: "repaint the whole screen",
                short: "redraw",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Char('q'), Key::Ctrl('c')]),
                mode: $mode,
                intent: Intent::Quit,
                act: "quit, restoring the terminal",
                short: "quit",
                footer_rank: Some(8),
                needs: Need::Always,
            },
        ]
    };
}

/// The universal rows AS A THREAD GETS THEM: `ctrl-<letter>` only.
///
/// The bare letters are absent here and their absence is the whole point — in a
/// thread every printable key is TEXT, so `u` types a `u` and the queue is
/// `^O`. These are the same commands as [`common_rows`], reachable by the same
/// CTRL keys, which is what makes the printed grammar uniform across every mode
/// even though the bare-letter synonyms only exist where there is nothing to
/// type into — PLUS `^S`, which is the one act in this macro that is NOT in
/// `common_rows`, deliberately (see its row), MINUS reload, which `^R` cannot
/// carry inside a textarea (see the note where its row used to be).
///
/// `^C` still quits, everywhere, unmodified. It is the one key a terminal user
/// never has to be told, and raw mode has already taken `ISIG` away from it, so
/// it is ours to bind rather than the shell's to intercept.
macro_rules! alt_common_rows {
    ($mode:expr) => {
        [
            Binding {
                keys: KeySet::These(&[Key::Ctrl('/'), Key::Ctrl('7')]),
                mode: $mode,
                intent: Intent::Help,
                act: "every key this face answers to, in THIS terminal",
                short: "keys",
                footer_rank: Some(0),
                needs: Need::Always,
            },
            // **THE STDOUT-ONLY TOGGLE, AND THIS IS THE ONLY TABLE IT IS
            // IN.** A thread is the context he said it belonged to, and these
            // rows are stamped onto the thread's four regions and nowhere
            // else — so the act is reachable everywhere he could be standing
            // inside a thread and is absent from the root, the list and the
            // event lane.
            //
            // The bare `s` is absent for the reason the bare `f` was: in a
            // thread every printable key is TEXT, so `s` types an `s`.
            //
            // **THE SPELLING IS `^S` SINCE ROUND SIX**, letter-for-letter from
            // `opt-s`, which macOS composes into `ß` and which therefore never
            // reached this face. `^S` is not XOFF here: raw mode clears `IXON`
            // (measured, `0128`).
            //
            // `short` is ONE WORD because the footer is full, and it is six
            // columns exactly as `speech` was — the word changed, the width did
            // not, so `conformance_test.sh`'s `q quit` paint anchor keeps the
            // columns it was measured with.
            Binding {
                keys: KeySet::These(&[Key::Ctrl('s')]),
                mode: $mode,
                intent: Intent::StdoutOnly,
                act: "show only what was said — press again to bring everything back",
                short: "stdout",
                footer_rank: Some(2),
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('o')]),
                mode: $mode,
                intent: Intent::QueueOverlay,
                act: "view the queue — the daemon's, and what this composer is holding",
                short: "queue",
                footer_rank: Some(5),
                needs: Need::Always,
            },
            // **RELOAD IS NOT IN THIS MACRO ANY MORE, AND ITS ABSENCE IS THE
            // ONE COST OF THE CTRL MIGRATION.** `^R` is `Compose(Redo)` in
            // `Thread(Composer)` — the partner of `^Z` undo, and its only
            // spelling — so reload cannot have `^R` there. It is therefore
            // stamped per region by [`thread_reload_row`] onto the three
            // regions where `^R` IS free, and the composer has NO reload key.
            //
            // **Stated plainly because it is a real loss he has to accept:**
            // reload is unreachable from inside the textarea and is one `esc`
            // away in the chat region. The `?` overlay renders per mode, so it
            // documents this for free rather than promising a key that is not
            // there.
            Binding {
                keys: KeySet::These(&[Key::Ctrl('l')]),
                mode: $mode,
                intent: Intent::Redraw,
                act: "repaint the whole screen",
                short: "redraw",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('c')]),
                mode: $mode,
                intent: Intent::Quit,
                act: "quit, restoring the terminal (`q` is a letter here, not a command)",
                short: "quit",
                footer_rank: Some(8),
                needs: Need::Always,
            },
        ]
    };
}

/// **RELOAD, FOR THE THREE THREAD REGIONS THAT CAN HAVE IT.**
///
/// One row, written once and stamped three times, exactly as
/// [`alt_common_rows`] is — it is simply not stampable onto the fourth. See the
/// note in `alt_common_rows` for why the composer has no reload key.
macro_rules! thread_reload_row {
    ($mode:expr) => {
        Binding {
            keys: KeySet::These(&[Key::Ctrl('r')]),
            mode: $mode,
            intent: Intent::Reload,
            act: "re-ask the daemon for everything this view shows",
            short: "reload",
            footer_rank: None,
            needs: Need::Always,
        }
    };
}

/// **THE COMPOSER'S OWN GRAMMAR — the thread's rows, and the only place in this
/// table where a printable character means something.**
///
/// Read the ORDER as part of the meaning: every specific binding comes first
/// and `KeySet::Printable` is last, so `^S` collapses to stdout and `s`
/// types an `s`, with first-match-wins doing the arbitration rather than a rule
/// somewhere else.
///
/// **TIER 1 is everything that works with ZERO protocol.** `enter` sends,
/// `alt-enter` and `^J` are newlines, `^Q` queues. All three were measured
/// through a pty against this stack: `\r` arrives as Enter, `\x1b\r` as
/// Enter+ALT, `\n` as `^J`. None of them needs anything negotiated, inside tmux
/// or out, which is what makes [`Caps`] an UPGRADE and never a dependency.
///
/// **TIER 2 is `shift-enter` and `^enter`, and nothing is reachable only
/// through it.** Both are aliases for an act tier 1 already carries.
macro_rules! composer_rows {
    ($mode:expr) => {
        [
            // ── the two acts, and they are the point ─────────────────────
            Binding {
                keys: KeySet::These(&[Key::Enter]),
                mode: $mode,
                intent: Intent::Send,
                act: "SEND the whole payload — everything queued, plus what is typed, as one message",
                short: "send",
                // **NOT IN THE FOOTER, and that is not an oversight.** It was
                // absent because the composer's border carried `enter SENDS all
                // · <queue> · <newline>` one row above the footer, so repeating
                // the three of them here would have spent the footer's width
                // saying what he was already looking at. **THAT LEGEND IS GONE**
                // — *"You don't need to give me instructions on how to use the
                // textarea"* — and the rank stays `None` anyway: `enter` sends
                // in every mode a payload leaves from and is the one key he has
                // never had to be told.
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('q')]),
                mode: $mode,
                intent: Intent::QueueMessage,
                act: "QUEUE what is typed and keep composing — nothing is sent",
                short: "queue it",
                footer_rank: None,
                needs: Need::Always,
            },
            // **THE GATE INTO MIC MODE, AND IT REFUSES WHILE THE BOX HAS
            // TEXT.** `DIR-5`'s constraint, in his own words and with his own
            // label on it: *"don't toggle it if there is content in the
            // textarea, to avoid out of order concerns. That's a naive
            // constraint we'll apply for now."* The refusal is SPOKEN, not
            // silent — see `App::toggle_mic`.
            //
            // **AND `^T` IS THE WHOLE ROW NOW.** It was minted in round five
            // against a measurement rather than a preference: his terminal is
            // iTerm2 with `Option Key Sends = Normal` (read from his own
            // preferences, both halves of the key), which means macOS RESOLVES
            // `opt-m` to `µ` in the keyboard layer and no escape sequence is
            // ever generated — so `Key::Alt('m')` could never arrive on that
            // stack at all, and mic mode had no door. `^T` is a C0 byte
            // (`0x14`) that no layout composes and no tmux prefix claims (his
            // is `^B`, measured), so it arrives today.
            //
            // **`opt-m` IS GONE FROM THIS ROW, AND THE ASK THAT WAS KEEPING IT
            // ALIVE IS RETIRED.** This note used to say `opt-m` *"is not being
            // replaced and it is not broken here; it works the moment the
            // option key is set to `Esc+`"* — a STANDING HUMAN ACTION, living
            // in a code comment where a successor would find it and act on it.
            // **He answered it by refusing to depend on the setting at all:**
            // *"ctrl-* might be better because it doesn't have keybindings and
            // it's universally understood to work. Let's just switch to all
            // ctrl-* across the app instead of opt-*."* That is a better answer
            // than ours and it removes an item from him. **Do not ask him to
            // change his terminal, here or anywhere.**
            //
            // The consequence is not cosmetic: macOS only offers the
            // microphone grant when an app ATTEMPTS capture, so a face with no
            // reachable mic mode is a face whose capture leg can never be
            // granted and therefore never verified.
            Binding {
                keys: KeySet::These(&[Key::Ctrl('t')]),
                mode: $mode,
                intent: Intent::MicMode,
                act: "mic mode — speak instead of typing (refuses while the box has text)",
                short: "mic",
                footer_rank: Some(7),
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::AltEnter, Key::Ctrl('j')]),
                mode: $mode,
                intent: Intent::Compose(Edit::Newline),
                act: "newline (works on every terminal, with no protocol at all)",
                short: "newline",
                footer_rank: None,
                needs: Need::Always,
            },
            // ── TIER 2: shown only when the terminal can report it ───────
            Binding {
                keys: KeySet::These(&[Key::ShiftEnter]),
                mode: $mode,
                intent: Intent::Compose(Edit::Newline),
                act: "newline (an alias — the newline row above always works)",
                short: "newline",
                footer_rank: None,
                needs: Need::Enhanced,
            },
            Binding {
                keys: KeySet::These(&[Key::CtrlEnter]),
                mode: $mode,
                intent: Intent::QueueMessage,
                act: "queue what is typed (an alias — the queue row above always works)",
                short: "queue it",
                footer_rank: None,
                needs: Need::Enhanced,
            },
            // **RECORD REGARDLESS OF WHERE HE IS STANDING** — his own
            // *"regardless"*, and see [`Intent::MicRecordNow`] for the reading.
            //
            // **WHY `^V` AND NOT THE `^R` HIS SENTENCE NAMES.** He asked for
            // `opt-r`, and `R` is not available for it: `^R` is
            // `Compose(Redo)` in this very mode — the partner of `^Z`, and its
            // ONLY spelling — and `^R` is RELOAD in every other mode that has
            // one. Splitting one act across two letters in different modes is
            // the thing `0128` forbids, so this act takes a letter it can keep
            // everywhere. `V` is for VOICE, `^V` is free in all fifteen modes,
            // and no shell or tmux claims it. **This is the one place his
            // literal words are not executed, and it is reported as such.**
            Binding {
                keys: KeySet::These(&[Key::Ctrl('v')]),
                mode: $mode,
                intent: Intent::MicRecordNow,
                act: "start recording — from here, whatever is in the box",
                short: "record",
                footer_rank: None,
                needs: Need::Always,
            },
            // **UNQUEUE HAS NO CTRL HOME IN THE COMPOSER, AND IT IS NOT A
            // REGRESSION.** `^K` is `Compose(KillToEnd)` here (readline, in a
            // textarea), and `opt-k` does not work on his keyboard TODAY —
            // macOS composes it into a dead key. So nothing he has is lost.
            // The SAME ACT lives in `Thread(Queue)`, where `^K` is free, and
            // that is where it is bound; `↓` out of the box reaches it.
            //
            // **AND THE RECOVERED-DRAFT PAIR IS NOT HERE EITHER — it moved
            // somewhere better than a key.** `^Y` is `Compose(Yank)` and `^D`
            // is `Compose(Delete)`, so `AcceptDraft`/`DiscardDraft` had no
            // mnemonic left. But they are a MODAL QUESTION: they mean nothing
            // unless the offer is on screen, so they cost no namespace at all
            // when they answer the offer itself. `y` and `n` while the offer
            // stands — see [`DRAFT_OFFER_ACCEPT`] and [`draft_offer_intent`],
            // which is one source both the shell and the band read.
            // ── motion, all of it by GRAPHEME CLUSTER ────────────────────
            // **`←` IS TEXT MOVEMENT UNTIL THERE IS NO TEXT LEFT TO THE LEFT OF
            // IT.** The edge for this key is the BUFFER START — line 0, byte 0
            // — and not the start of the visual row: `←` at the start of line 2
            // moves to the END OF LINE 1, which is text movement. Getting that
            // backwards is the inverted-boundary failure, and it is why the
            // deferral lives in `App::compose` beside the caret rather than
            // here.
            Binding {
                keys: KeySet::These(&[Key::Left]),
                mode: $mode,
                intent: Intent::Compose(Edit::Left),
                act: "left one character — and at the very start of the draft, BACK out of the thread",
                short: "back at the edge",
                footer_rank: Some(9),
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Right]),
                mode: $mode,
                intent: Intent::Compose(Edit::Right),
                act: "right one character",
                short: "right",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::AltLeft]),
                mode: $mode,
                intent: Intent::Compose(Edit::WordLeft),
                act: "left one word",
                short: "word left",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::AltRight]),
                mode: $mode,
                intent: Intent::Compose(Edit::WordRight),
                act: "right one word",
                short: "word right",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Up]),
                mode: $mode,
                intent: Intent::Compose(Edit::Up),
                act: "up one line — and at the box's TOP edge, up to the queue or the chat",
                short: "up",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Down]),
                mode: $mode,
                intent: Intent::Compose(Edit::Down),
                act: "down one line — and at the box's BOTTOM edge, down to the subagents",
                short: "down",
                footer_rank: None,
                needs: Need::Always,
            },
            // **BY PARAGRAPH, AND THEY NEVER LEAVE THE BOX.** His callout, in
            // the same breath as the escape rule, and the exception to it:
            // *"opt-up to the top of the current or top of the previous,
            // opt-down to the bottom of the current or bottom of the next"*,
            // *"they should stop at the beginning and end, they should not
            // escape the textarea."* Same keys, different region, different
            // act — in the chat they walk HIS OWN MESSAGES. Two rows of one
            // table, which is the whole principle.
            Binding {
                keys: KeySet::These(&[Key::AltUp]),
                mode: $mode,
                intent: Intent::Compose(Edit::ParaUp),
                act: "top of this paragraph, then the previous one — never leaves the box",
                short: "para up",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::AltDown]),
                mode: $mode,
                intent: Intent::Compose(Edit::ParaDown),
                act: "end of this paragraph, then the next one — never leaves the box",
                short: "para down",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                // **`^←` IS THE LIMIT IN THAT DIRECTION**, joining the two
                // spellings this act already had. It is one row and not a
                // second binding: `0052`'s ask is *"jump to the LIMIT in that
                // direction — line start/end, buffer top/bottom"*, and the four
                // limits were already bound to `home`/`end`/`alt-home`/
                // `alt-end`. What the ctrl-arrows add is a SHAPE — the same
                // direction his hand is already pointing — which is why they go
                // in the rows that exist rather than into rows of their own.
                keys: KeySet::These(&[Key::Home, Key::Ctrl('a'), Key::CtrlLeft]),
                mode: $mode,
                intent: Intent::Compose(Edit::RowStart),
                act: "start of the line — or the oldest row held, with the composer empty",
                short: "line start",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::End, Key::Ctrl('e'), Key::CtrlRight]),
                mode: $mode,
                intent: Intent::Compose(Edit::RowEnd),
                act: "end of the line — or newest and follow again, with the composer empty",
                short: "line end",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::AltHome, Key::CtrlUp]),
                mode: $mode,
                intent: Intent::Compose(Edit::BufStart),
                act: "start of everything typed",
                short: "top of draft",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::AltEnd, Key::CtrlDown]),
                mode: $mode,
                intent: Intent::Compose(Edit::BufEnd),
                act: "end of everything typed",
                short: "end of draft",
                footer_rank: None,
                needs: Need::Always,
            },
            // ── deletion ─────────────────────────────────────────────────
            Binding {
                // **`^H` IS BOUND HERE AND IT IS NOT DECORATION.** Found by
                // hand, on a pty: the ERASE key sends `\x7f` on his terminal
                // and crossterm turns that into `Backspace`, but a terminal
                // whose erase is `^H` — `stty erase ^H`, which is a real and
                // common configuration — sends `\x08`, which crossterm turns
                // into `Ctrl('h')`. Without this row, backspace is a DEAD KEY
                // on such a terminal, in the box he types into. The research's
                // own collision list names `^M`/`^I`/`^H`/`^[` as the aliases
                // of Enter/Tab/Backspace/Esc; the other three are handled by
                // crossterm before the control range and this one is not.
                keys: KeySet::These(&[Key::Backspace, Key::Ctrl('h')]),
                mode: $mode,
                intent: Intent::Compose(Edit::Backspace),
                act: "delete the character before the cursor, whole",
                short: "backspace",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Delete, Key::Ctrl('d')]),
                mode: $mode,
                intent: Intent::Compose(Edit::Delete),
                act: "delete the character after the cursor, whole",
                short: "delete",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('k')]),
                mode: $mode,
                intent: Intent::Compose(Edit::KillToEnd),
                act: "cut to the end of the line",
                short: "kill to end",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('u')]),
                mode: $mode,
                intent: Intent::Compose(Edit::KillToStart),
                act: "cut to the start of the line",
                short: "kill to start",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                // **`alt-bksp` IS HIS `opt-delete`**, added beside the `^W` that
                // was already here rather than instead of it: `^W` is the
                // terminal spelling every shell has, and `opt-delete` is the
                // one his hands already know from every other Mac text box.
                // Two spellings, ONE row, so the `?` overlay cannot come to
                // describe them as two different acts.
                keys: KeySet::These(&[Key::Ctrl('w'), Key::AltBackspace]),
                mode: $mode,
                intent: Intent::Compose(Edit::KillWord),
                act: "cut the word before the cursor",
                short: "kill word",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('y')]),
                mode: $mode,
                intent: Intent::Compose(Edit::Yank),
                act: "put back whatever the last cut took",
                short: "yank",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('z')]),
                mode: $mode,
                intent: Intent::Compose(Edit::Undo),
                act: "undo the last edit RUN — a whole paste undoes in one",
                short: "undo",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('r')]),
                mode: $mode,
                intent: Intent::Compose(Edit::Redo),
                act: "redo",
                short: "redo",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Tab]),
                mode: $mode,
                intent: Intent::Compose(Edit::Tab),
                act: "insert spaces — a hard tab is never stored",
                short: "indent",
                footer_rank: None,
                needs: Need::Always,
            },
            // ── the transcript, from inside a composer that owns the arrows
            Binding {
                keys: KeySet::These(&[Key::PageUp, Key::Ctrl('p')]),
                mode: $mode,
                intent: Intent::PageUp,
                act: "transcript up one screen (^P moves it one row)",
                short: "page up",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::PageDown, Key::Ctrl('n')]),
                mode: $mode,
                intent: Intent::PageDown,
                act: "transcript down one screen (^N moves it one row)",
                short: "page down",
                footer_rank: None,
                needs: Need::Always,
            },
            // **`opt-g` AND `opt-n` ARE RETIRED HERE TOO, and that is the
            // collision audit's ruling rather than a reading of my own.** His
            // sentence is about the FUNCTION: *"g and G are jumping to the top
            // and bottom. We don't need that because we have opt-{up,down}."*
            // A redundant function retires in EVERY spelling it has, or we
            // have retired a key and left the act reachable — which is the
            // relabel-instead-of-retire failure the plain `g`/`G` were
            // retired to avoid.
            //
            // **AND THE CONSEQUENCE IS REPORTED, NOT SOFTENED.** `opt-↑`/
            // `opt-↓` is BandPrev/BandNext at the root, ParaUp/ParaDown in the
            // box and JumpUserPrev/JumpUserNext in the chat — none of which is
            // an absolute top/bottom jump. After this, in a thread, nothing is.
            // `home`/`end` survive in the three list-shaped modes only.
            // **`Intent::Activate` HAS NO CTRL HOME IN THE COMPOSER EITHER,
            // AND FOR THE SAME STRUCTURAL REASON.** `^E` is
            // `Compose(RowEnd)` here. The act lives in `Thread(Chat)`, where
            // `^E` is free and where the folded rows actually are, and `opt-e`
            // was a dead key on his keyboard today — macOS composes it into an
            // accent — so nothing he has is lost. `↑` out of the box reaches
            // it, and a click on the `▸` still expands any row.
            // **THE `esc` ROW IS DELETED, AND ONLY THIS ROW IS.** His words:
            // *"And left should go back. I don't even want ESC to do that
            // behavior."* So `esc` is UNBOUND in the composer — not rebound,
            // and certainly not a reason to remove the back-stack, which is
            // the machinery `←` now reaches and which was itself the fix for
            // escape teleporting him into a raw thread list. In the other
            // regions `esc` means COME HOME TO THE COMPOSER, which is not
            // "that behavior" at all.
            // ── LAST, and last on purpose: everything else is TEXT ───────
            Binding {
                keys: KeySet::Printable,
                mode: $mode,
                intent: Intent::Type('\0'),
                act: "typed into the composer",
                short: "type",
                footer_rank: None,
                needs: Need::Always,
            },
        ]
    };
}

/// **THE CHAT'S OWN GRAMMAR — the region where the arrows scroll, and the only
/// one where they do.**
///
/// His ruling, verbatim: *"When the chat is selected then that's the only time
/// that up and down should scroll. And left should go back… And if the chat is
/// selected then right should jump to the end of the chat."*
///
/// The ESCAPE RULE is the same one the textarea obeys, which is the whole point
/// of having one: an arrow leaves its region only when its region has nothing
/// left to move through in that direction. For the chat that is the BOTTOM of
/// the transcript, and the shell defers there to `step_region` — so `↓` past
/// the newest row lands in the queue, or in the composer.
macro_rules! chat_rows {
    ($mode:expr) => {
        [
            Binding {
                keys: KeySet::These(&[Key::Up, Key::Ctrl('p')]),
                mode: $mode,
                intent: Intent::ScrollUp,
                act: "up one row (leaves follow-mode)",
                short: "up",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Down, Key::Ctrl('n')]),
                mode: $mode,
                intent: Intent::ScrollDown,
                act: "down one row — and at the newest row, down into the queue or the box",
                short: "down",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::PageUp]),
                mode: $mode,
                intent: Intent::PageUp,
                act: "up one screen",
                short: "page up",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::PageDown]),
                mode: $mode,
                intent: Intent::PageDown,
                act: "down one screen",
                short: "page down",
                footer_rank: None,
                needs: Need::Always,
            },
            // `opt-g` is RETIRED here for the reason it is retired in
            // `composer_rows` — see that macro's note.
            // **`→` AND `opt-↓`-past-the-last ARE ONE ACT.** On this face
            // there is no state that is "at the last row but not following" —
            // `follow` re-arms the moment the bottom is reached — so "the end
            // of the chat" and "the live state of the convo" are the same
            // destination, and they are one intent with two spellings rather
            // than two acts that could drift.
            //
            // **`alt-n` WAS THE THIRD SPELLING AND IT IS RETIRED**, with
            // `opt-g` and the plain `g`/`G`: the audit found it was a second
            // spelling of the redundant top/bottom pair, and `^N` has no free
            // ctrl home, so leaving it would have handed `0134` a binding with
            // nowhere to migrate to. `→` is untouched and is what reaches the
            // end of the chat.
            Binding {
                keys: KeySet::These(&[Key::Right]),
                mode: $mode,
                intent: Intent::Bottom,
                act: "the end of the chat — newest, and following again",
                short: "end",
                footer_rank: Some(11),
                needs: Need::Always,
            },
            // **NAVIGATION BY AUTHORSHIP.** *"let's make opt-{up,down} jump to
            // the next/previous user message."* The same key pair means
            // PARAGRAPH in the composer and BAND HEADER at the root; three
            // rows of one table, resolved by region.
            Binding {
                keys: KeySet::These(&[Key::AltUp]),
                mode: $mode,
                intent: Intent::JumpUserPrev,
                act: "your previous message, at the top of the band",
                short: "your last",
                footer_rank: Some(12),
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::AltDown]),
                mode: $mode,
                intent: Intent::JumpUserNext,
                act: "your next message — and past the last one, the live state of the convo",
                short: "your next",
                footer_rank: Some(13),
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Left]),
                mode: $mode,
                intent: Intent::Back,
                act: "back to wherever you opened the thread from — what is typed is kept",
                short: "back",
                footer_rank: Some(10),
                needs: Need::Always,
            },
            // **`esc` IS NOT BACK ANY MORE; IT IS HOME.** Every region needs a
            // way to the box that is not three arrows, and typing already goes
            // there — this is the non-typing way. In the composer it is
            // unbound.
            Binding {
                keys: KeySet::These(&[Key::Esc]),
                mode: $mode,
                intent: Intent::Region(Region::Composer),
                act: "put the selection back in the message box",
                short: "compose",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Enter]),
                mode: $mode,
                intent: Intent::Send,
                act: "SEND the whole payload — everything queued, plus what is typed",
                short: "send",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Tab]),
                mode: $mode,
                intent: Intent::SwapView,
                act: "the fleet's event lane, and back",
                short: "activity",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('e')]),
                mode: $mode,
                intent: Intent::Activate,
                act: "expand the newest folded row (or click the ▸ on any row)",
                short: "expand",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('v')]),
                mode: $mode,
                intent: Intent::MicRecordNow,
                act: "start recording — from here, whatever is in the box",
                short: "record",
                footer_rank: None,
                needs: Need::Always,
            },
            // **AND NOTHING TYPES HERE.** *"The textarea should be inactive
            // when the user cursor is outside of the textarea."* This region
            // had a `KeySet::Printable -> Intent::Type` row until round five,
            // on the earlier reading that typing always types and drags the
            // selection into the box with it. He read that as the box changing
            // under him while he was reading the chat, and it is gone: a
            // printable key in the chat is UNBOUND, and `esc` is the one key
            // that comes home to the box. See [`Mode::textarea_is_live`].
        ]
    };
}

/// **THE CLIENT QUEUE'S GRAMMAR** — the region his *"make the queue accessible
/// by click or by navigating above or below the textarea"* asks for.
///
/// It is a LIST, so `↑`/`↓` move a cursor and the edge rule takes over at the
/// ends. `→` is deliberately UNBOUND: taking a queued row back into the box is
/// a per-row unqueue, which is a follow-up and not a region-model question
/// (`^K` already takes the LAST one back).
macro_rules! queue_rows {
    ($mode:expr) => {
        [
            Binding {
                keys: KeySet::These(&[Key::Up, Key::Ctrl('p')]),
                mode: $mode,
                intent: Intent::ScrollUp,
                act: "the queued message above — and at the first, up into the chat",
                short: "up",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Down, Key::Ctrl('n')]),
                mode: $mode,
                intent: Intent::ScrollDown,
                act: "the queued message below — and at the last, down into the box",
                short: "down",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Left]),
                mode: $mode,
                intent: Intent::Back,
                act: "back to wherever you opened the thread from — what is queued is kept",
                short: "back",
                footer_rank: Some(10),
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Esc]),
                mode: $mode,
                intent: Intent::Region(Region::Composer),
                act: "put the selection back in the message box",
                short: "compose",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Enter]),
                mode: $mode,
                intent: Intent::Send,
                act: "SEND the whole payload — everything queued, plus what is typed",
                short: "send",
                footer_rank: Some(11),
                needs: Need::Always,
            },
            // **`^K` IS FREE HERE, WHICH IS WHY THIS ACT LIVES HERE AND NOT
            // IN THE BOX.** In the composer `^K` is `Compose(KillToEnd)`.
            Binding {
                keys: KeySet::These(&[Key::Ctrl('k')]),
                mode: $mode,
                intent: Intent::Compose(Edit::Unqueue),
                act: "take the last queued message back into the box",
                short: "unqueue",
                footer_rank: Some(12),
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('v')]),
                mode: $mode,
                intent: Intent::MicRecordNow,
                act: "start recording — from here, whatever is in the box",
                short: "record",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Tab]),
                mode: $mode,
                intent: Intent::SwapView,
                act: "the fleet's event lane, and back",
                short: "activity",
                footer_rank: None,
                needs: Need::Always,
            },
            // Nothing types here either — see the note at the end of
            // `chat_rows!` and [`Mode::textarea_is_live`].
        ]
    };
}

/// **THE SUBAGENT BAND'S GRAMMAR.**
///
/// > *we should be able to navigate to those subagents in the tree view below,
/// > and if we click right or enter on them we show their chat history instead
/// > of the parent. Basically the text area above should be whichever one
/// > we've selected, so the parent needs to be an item in that list.*
///
/// `↑`/`↓` walk the rows and step out only at the ends — the same sentence the
/// queue's rows answer to and the same sentence the textarea answers to, which
/// is what keeps the textarea from being a special case. `→`/`enter` DRILL:
/// the row under the cursor becomes the SUBJECT and its chat replaces the
/// parent's above. `←` un-drills one level first and only then leaves the
/// thread.
macro_rules! agents_rows {
    ($mode:expr) => {
        [
            Binding {
                keys: KeySet::These(&[Key::Up, Key::Ctrl('p')]),
                mode: $mode,
                intent: Intent::ScrollUp,
                act: "up one row — and at the first, back up into the box",
                short: "up",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Down, Key::Ctrl('n')]),
                mode: $mode,
                intent: Intent::ScrollDown,
                act: "down one row",
                short: "down",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Right, Key::Enter]),
                mode: $mode,
                intent: Intent::Activate,
                // TUI-P1b: ONE KEY expands a thread row AND enters it, and
                // the tree around it is kept to the root.
                act: "expand this one and open its chat above — the tree stays",
                short: "expand + open",
                footer_rank: Some(1),
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Left]),
                mode: $mode,
                intent: Intent::Back,
                act: "back to the parent thread, then out of the thread",
                short: "back",
                footer_rank: Some(10),
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Esc]),
                mode: $mode,
                intent: Intent::Region(Region::Composer),
                act: "put the selection back in the message box",
                short: "compose",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Tab]),
                mode: $mode,
                intent: Intent::SwapView,
                act: "the fleet's event lane, and back",
                short: "activity",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Ctrl('v')]),
                mode: $mode,
                intent: Intent::MicRecordNow,
                act: "start recording — from here, whatever is in the box",
                short: "record",
                footer_rank: None,
                needs: Need::Always,
            },
            // Nothing types here either — see the note at the end of
            // `chat_rows!` and [`Mode::textarea_is_live`].
        ]
    };
}

/// **THE LITTLE RENAME MODAL'S WHOLE GRAMMAR** ([`Mode::RenameModal`]).
///
/// Written as ONE const in ONE place, deliberately, rather than interleaved
/// into the table below: three workers are inside this file at once this round
/// and a contiguous block is a block the merge can move whole.
///
/// **HERE THE LETTERS ARE TEXT.** `r` types an `r`; it does not re-open the
/// rename he is already inside. `c` types a `c`; it does not compact. That is
/// [`Mode::SheetField`]'s precedent followed exactly, and it is the reason this
/// is a mode.
///
/// **IT IS A SMALLER GRAMMAR THAN THE SHEET'S FIELD, for the reason
/// [`Mode::NewProject`]'s is smaller**: a thread's name is ONE SHORT ANSWER, so
/// there is no newline, no vertical motion and no `alt-enter` — and the rows
/// that would produce them are simply absent, so `App::sheet_compose` can
/// never be asked for them here.
///
/// **NO NEW MODIFIED-LETTER SPELLING IS ADDED**, per the round's merge contract.
/// `alt-←`, `alt-→` and `alt-backspace` are carried over verbatim from
/// `Mode::SheetField`'s rows — they are existing MOTION spellings, not a claim
/// on the global `ctrl-<letter>` grammar, whose safety property is that a
/// modified letter means one thing everywhere. **AND THE ARROW FAMILY IS WHERE
/// `alt-` SURVIVES**: an arrow has no composed glyph, so those three spellings
/// were never the defect the ctrl migration fixed.
///
/// **THE TWO DOORS OUT ARE THE TWO HE WAS PROMISED.** `enter` commits through
/// `Intent::SheetCommit` — the SAME intent the sheet's rename row commits
/// through, so there is one mutation and not two. `esc` is `Intent::Back` and
/// changes nothing at all.
const RENAME_MODAL_ROWS: [Binding; 16] = [
    Binding {
        keys: KeySet::These(&[Key::Enter]),
        mode: Mode::RenameModal,
        intent: Intent::SheetCommit,
        act: "rename the thread to what is typed",
        short: "rename",
        footer_rank: Some(1),
        needs: Need::Always,
    },
    // **`esc` CANCELS AND CHANGES NOTHING**, which is the one place this box
    // differs from the sheet's field rather than copying it. In the sheet, `esc`
    // KEEPS what is typed and lands on the menu, because a half-written
    // compaction instruction is worth more than the keystroke that closed the
    // door. A modal has no menu to land on and no door to leave ajar: it is
    // over the list he was reading, so leaving it means the list, unchanged.
    Binding {
        keys: KeySet::These(&[Key::Esc]),
        mode: Mode::RenameModal,
        intent: Intent::Back,
        act: "close the box — the thread keeps the name it has",
        short: "cancel",
        footer_rank: Some(0),
        needs: Need::Always,
    },
    Binding {
        keys: KeySet::These(&[Key::Left]),
        mode: Mode::RenameModal,
        intent: Intent::Compose(Edit::Left),
        act: "left one character (a whole emoji is one character)",
        short: "left",
        footer_rank: None,
        needs: Need::Always,
    },
    Binding {
        keys: KeySet::These(&[Key::Right]),
        mode: Mode::RenameModal,
        intent: Intent::Compose(Edit::Right),
        act: "right one character",
        short: "right",
        footer_rank: None,
        needs: Need::Always,
    },
    Binding {
        keys: KeySet::These(&[Key::AltLeft]),
        mode: Mode::RenameModal,
        intent: Intent::Compose(Edit::WordLeft),
        act: "left one word",
        short: "word left",
        footer_rank: None,
        needs: Need::Always,
    },
    Binding {
        keys: KeySet::These(&[Key::AltRight]),
        mode: Mode::RenameModal,
        intent: Intent::Compose(Edit::WordRight),
        act: "right one word",
        short: "word right",
        footer_rank: None,
        needs: Need::Always,
    },
    Binding {
        keys: KeySet::These(&[Key::Home, Key::Ctrl('a'), Key::CtrlLeft]),
        mode: Mode::RenameModal,
        intent: Intent::Compose(Edit::RowStart),
        act: "start of the name",
        short: "start",
        footer_rank: None,
        needs: Need::Always,
    },
    Binding {
        keys: KeySet::These(&[Key::End, Key::Ctrl('e'), Key::CtrlRight]),
        mode: Mode::RenameModal,
        intent: Intent::Compose(Edit::RowEnd),
        act: "end of the name",
        short: "end",
        footer_rank: None,
        needs: Need::Always,
    },
    // `^H` is bound beside backspace for the reason the composer binds it: a
    // terminal whose erase is `^H` sends `\x08`, and without this row backspace
    // is a DEAD KEY in the box he types into.
    Binding {
        keys: KeySet::These(&[Key::Backspace, Key::Ctrl('h')]),
        mode: Mode::RenameModal,
        intent: Intent::Compose(Edit::Backspace),
        act: "delete the character before the cursor, whole",
        short: "backspace",
        footer_rank: None,
        needs: Need::Always,
    },
    Binding {
        keys: KeySet::These(&[Key::Delete, Key::Ctrl('d')]),
        mode: Mode::RenameModal,
        intent: Intent::Compose(Edit::Delete),
        act: "delete the character after the cursor, whole",
        short: "delete",
        footer_rank: None,
        needs: Need::Always,
    },
    Binding {
        keys: KeySet::These(&[Key::Ctrl('u')]),
        mode: Mode::RenameModal,
        intent: Intent::Compose(Edit::KillToStart),
        act: "cut back to the start of the name",
        short: "kill to start",
        footer_rank: None,
        needs: Need::Always,
    },
    Binding {
        keys: KeySet::These(&[Key::Ctrl('k')]),
        mode: Mode::RenameModal,
        intent: Intent::Compose(Edit::KillToEnd),
        act: "cut to the end of the name",
        short: "kill to end",
        footer_rank: None,
        needs: Need::Always,
    },
    Binding {
        keys: KeySet::These(&[Key::Ctrl('w'), Key::AltBackspace]),
        mode: Mode::RenameModal,
        intent: Intent::Compose(Edit::KillWord),
        act: "cut the word before the cursor",
        short: "kill word",
        footer_rank: None,
        needs: Need::Always,
    },
    Binding {
        keys: KeySet::These(&[Key::Ctrl('y')]),
        mode: Mode::RenameModal,
        intent: Intent::Compose(Edit::Yank),
        act: "put back whatever the last cut took",
        short: "yank",
        footer_rank: None,
        needs: Need::Always,
    },
    Binding {
        keys: KeySet::These(&[Key::Ctrl('c')]),
        mode: Mode::RenameModal,
        intent: Intent::Quit,
        act: "quit, restoring the terminal",
        short: "quit",
        footer_rank: None,
        needs: Need::Always,
    },
    // LAST in this mode, exactly as it is last in the sheet's field and in the
    // thread: a printable key is a character, and every binding above it had
    // first refusal.
    Binding {
        keys: KeySet::Printable,
        mode: Mode::RenameModal,
        intent: Intent::Type('\0'),
        act: "type",
        short: "type",
        footer_rank: None,
        needs: Need::Always,
    },
];
/// **THE ROOT'S ROW ACTS — the grammar of "the thing under the cursor", written
/// once and read by the root AND by a band's own `see all` page.**
///
/// Every row this table names is a row BOTH surfaces can put the cursor on: a
/// thread in a cross-project band, a project, and (on the root only) the band
/// headers, the `see all` feet and the two chrome rows. `ProjectList::item` is
/// the one resolution of a cursor index to the thing it names, and it is the
/// same function on both, so these bindings reach the same handlers with no
/// second act path.
///
/// **WHY IT IS A MACRO AND NOT A SECOND BLOCK.** `0133`'s stated fear was *"a
/// page where `r` silently did nothing"*, and a copied block is exactly how
/// that happens — the root gains a key, nobody remembers the page. Stamped
/// from here it cannot: a key that works on the root works on the page, and
/// `the_page_answers_to_every_root_row_act` is the machine that says so rather
/// than this comment.
///
/// **WHAT IS NOT IN HERE, and both absences are the design's.**
///
///   * `←`. It is [`Intent::Top`] at the root — first row, NEVER exits — and
///     [`Intent::Back`] on the page, which is the one row where the two
///     surfaces genuinely differ and the whole reason the page is a `Mode`.
///   * The band jump (`opt-↑`/`opt-↓`). The page IS one band, so the key has
///     nowhere to go; a no-op with a legend row printed over it is a lie, and
///     the legend renders FROM this table.
macro_rules! root_acts {
    ($mode:expr) => {
        [
            Binding {
                // `→` and `enter` and a CLICK are one code path: all three
                // become `Intent::Activate`, which resolves a Target and asks
                // `hit::activation` what it means — and that function is where
                // the gesture's MEANING splits, so a click selects and `→`
                // opens without either one having a handler of its own.
                keys: KeySet::These(&[Key::Right, Key::Enter]),
                mode: $mode,
                intent: Intent::Activate,
                // **ONE SENTENCE FOR BOTH SURFACES.** It said "open this
                // project's threads" while this row lived in the root's block
                // alone, and the root's rows have not been only projects since
                // the bands landed — a band row opens a THREAD and a band
                // HEADER now opens that band's own page. Kept to the old row's
                // width on purpose: the `?` overlay crops at the pane width and
                // a longer sentence loses its own tail (measured — the overlay
                // golden is what said so).
                act: "open what the cursor is on (a click SELECTS the row; this opens it)",
                short: "open",
                footer_rank: Some(1),
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::Tab]),
                mode: $mode,
                intent: Intent::SwapView,
                act: "switch to the live event lane across every thread he owns",
                short: "activity",
                footer_rank: Some(2),
                needs: Need::Always,
            },
            // ── `o` — THE OPTIONS OF THE THREAD UNDER THE CURSOR ──────────
            //
            // **His words**: *"I want to be able to jump straight into the
            // options of a thread by clicking o while my cursor is on the
            // thread handle."*
            //
            // **WHY `o` IS FREE IN THESE MODES, checked against the table
            // rather than assumed.** The root's whole grammar is `→`/`enter`,
            // `←`, `tab`, these row acts, the six `scroll_rows` (`^P`, `^N`,
            // `^U`/`pgup`, `^D`/`pgdn`, `g`/`home`, `G`/`end`) and the seven
            // `common_rows` (`?`, `f`, `s`, `u`, `r`, `^L`, `q`/`^C`, each with
            // its `alt-` twin). `o` is in none of them, and
            // `one_key_one_meaning_in_one_mode` is the machine that says so.
            //
            // It is a BARE LETTER rather than an `alt-` one because the `alt-`
            // space is the documented GLOBAL grammar — an `alt-` letter means
            // the same thing in every mode — and this act only exists where a
            // thread row can be under the cursor. It is also the letter he
            // said.
            Binding {
                keys: KeySet::These(&[Key::Char('o')]),
                mode: $mode,
                intent: Intent::Options,
                act: "options for the thread OR the project under the cursor",
                short: "options",
                footer_rank: Some(3),
                needs: Need::Always,
            },
            // ── `r` — RENAME THE THREAD UNDER THE CURSOR ──────────────────
            //
            // **His words**: *"On any thread throughout the application, I need
            // to be able to rename with a tap of the letter r."* **ANYWHERE it
            // appears** is why this row is in the macro and not in the root's
            // block: the page is a new place a thread row appears.
            //
            // `^R` is still reload here, from `common_rows`, so nothing that
            // was documented moved. It aims at the row's TITLE through
            // `hit::activation`, exactly as `o` does, so the key door and the
            // pointer door onto a rename are one path. A cursor that is not on
            // a thread resolves to no thread and nothing happens, silently: a
            // miss is free.
            Binding {
                keys: KeySet::These(&[Key::Char('r')]),
                mode: $mode,
                intent: Intent::Rename,
                act: "rename the thread under the cursor",
                short: "rename",
                // **RANK 4, AND IT IS SCOPE 4 OF ROUND SIX** — *"rename and pin
                // keybindings aren't shown and they're the most important."*
                // `0127` set this on the root's and the list's INLINE rows; the
                // page (`0135`) factored those rows into this macro from a base
                // where the rank was still `None`, so the merge that kept the
                // macro would have silently reverted his fix. Restored here, in
                // the ONE place both surfaces read, which is what the factoring
                // was for: the page now names rename in its footer too, and it
                // cannot drift from the root's.
                footer_rank: Some(4),
                needs: Need::Always,
            },
            // ── `p` — PIN THE THREAD UNDER THE CURSOR, OR ADD A PROJECT ───
            //
            // The ruled collision, resolved by CONTEXT in the shell and not by
            // a second key here. See [`Intent::AddProject`] for why the split
            // is total on this surface and why it is not bound on `Mode::List`.
            //
            // `footer_rank: None` for the reason `rename` carries it: the root
            // already spends seven ranks and five more columns pushed `q quit`
            // — the last hint, and `conformance_test.sh`'s paint anchor — off
            // the row at the 100 columns that suite probes. It IS in the `?`
            // overlay.
            Binding {
                keys: KeySet::These(&[Key::Char('p')]),
                mode: $mode,
                intent: Intent::AddProject,
                act: "pin the thread under the cursor — elsewhere, add a project by path",
                short: "pin / add project",
                footer_rank: None,
                needs: Need::Always,
            },
            // ── `c` — DE-REGISTER THE PROJECT UNDER THE CURSOR ────────────
            //
            // It presses the project sheet's own `remove` row, so the `asks
            // first` confirmation is the ONE that already exists. See
            // [`Intent::DeregisterProject`].
            Binding {
                keys: KeySet::These(&[Key::Char('c')]),
                mode: $mode,
                intent: Intent::DeregisterProject,
                act: "de-register the project under the cursor (asks first)",
                short: "de-register",
                footer_rank: None,
                needs: Need::Always,
            },
            // ── REORDERING A PIN: `ctrl-↑` / `ctrl-↓` ─────────────────────
            //
            // **WHY THESE TWO KEYS, out of everything the root has left.** The
            // choice was made against the table rather than by taste, and it is
            // a DECISION the operator may overturn in one beat:
            //
            //  1. **They collide with nothing in this mode.** `ctrl-↑` and
            //     `ctrl-↓` are in none of the rows named in `o`'s note above.
            //     They ARE bound in `Mode::Thread`, where the composer reads
            //     them as buffer-top / buffer-bottom — a different mode, and
            //     see 3.
            //  2. **They are MEASURED to arrive on his stack.** `0052` bound
            //     the four ctrl-arrows only after `motion_keys_probe.sh`
            //     watched tmux 3.5a with his own `extended-keys` settings emit
            //     `CSI 1;5A` / `CSI 1;5B`. Nothing here is a key we hope
            //     arrives — which is the whole reason `alt-↑`/`alt-↓`, the
            //     editor-world convention, was NOT taken.
            //  3. **`ctrl` + arrow already MEANS "the strong form of that
            //     arrow"** in this face: `↑` moves the CURSOR up and `ctrl-↑`
            //     moves the ROW up. One family, learned once.
            //  4. **A MODIFIED LETTER was not taken** because that space is
            //     the documented GLOBAL grammar (`ctrl-<letter>` since round
            //     six).
            //
            // **ON A ROW THAT IS NOT PINNED THEY SAY SO.** `App::move_pin` puts
            // a sentence in the notice line naming what it aimed at; it is
            // never a silent no-op, and it never reorders from the `recent`
            // band, where the row would move somewhere he is not looking.
            //
            // **THE SAME KEY PAIR REORDERS A PROJECT**, by the same
            // neighbour-naming grammar (`acts::neighbour`, shared with the
            // pins) and with the cursor following by NAME as the pins' follows
            // by id. One binding and not two: it is one gesture — *move the row
            // I am standing on* — and the shell asks `ProjectList::item` what
            // that row is, exactly as `p` does.
            Binding {
                keys: KeySet::These(&[Key::CtrlUp]),
                mode: $mode,
                intent: Intent::MovePinUp,
                act: "move this pinned thread, or this project, up one — the cursor follows",
                short: "move up",
                footer_rank: None,
                needs: Need::Always,
            },
            Binding {
                keys: KeySet::These(&[Key::CtrlDown]),
                mode: $mode,
                intent: Intent::MovePinDown,
                act: "move this pinned thread, or this project, down one — the cursor follows",
                short: "move down",
                footer_rank: None,
                needs: Need::Always,
            },
        ]
    };
}

/// The table.
pub const BINDINGS: &[Binding] = &{
    // A const block so the four body modes' shared rows are written once and
    // the array is still a `const` the overlay can render without allocating.
    const PROJECTS: [Binding; 6] = scroll_rows!(Mode::Projects);
    const LIST: [Binding; 6] = scroll_rows!(Mode::List);
    const ACTIVITY: [Binding; 6] = scroll_rows!(Mode::Activity);
    const CP: [Binding; 5] = common_rows!(Mode::Projects);
    const CL: [Binding; 5] = common_rows!(Mode::List);
    const CA: [Binding; 5] = common_rows!(Mode::Activity);
    // **THE ROOT'S ROW ACTS, STAMPED TWICE.** See [`root_acts`]: the page
    // answers to the same keys as the root for every row it can hold, and it
    // does so by construction rather than by a second block somebody has to
    // remember to update.
    const RA_ROOT: [Binding; 8] = root_acts!(Mode::Projects);
    const RA_PAGE: [Binding; 8] = root_acts!(SEE_ALL);
    // **ONE BAND'S PAGE IS WRITTEN UNDER ONE BAND**, and every band's page
    // reads it — see [`Mode::table_key`] for why that is one collapse and not
    // a special case.
    const SEE_ALL: Mode = Mode::SeeAll(crate::model::BandId::Recent);
    const SA_SCROLL: [Binding; 6] = scroll_rows!(SEE_ALL);
    const SA_COMMON: [Binding; 5] = common_rows!(SEE_ALL);
    // **THE THREAD HAS NEITHER OF THE OTHER TWO SETS**, and that is the
    // composer's whole footprint on this table: `scroll_rows` binds `^U` and
    // `^D`, which a textarea needs for something else, and `common_rows` binds
    // bare letters that must now be text.
    //
    // **AND IT IS STAMPED ONCE PER REGION.** The globals mean the same
    // thing wherever the selection is standing — that is what makes them the
    // STABLE LEFT of the per-section legend — so they are written once and
    // stamped three times, exactly as the scroll rows are across the body
    // modes.
    const CT: [Binding; 5] = alt_common_rows!(Mode::Thread(Region::Composer));
    const CT_CHAT: [Binding; 5] = alt_common_rows!(Mode::Thread(Region::Chat));
    const CT_QUEUE: [Binding; 5] = alt_common_rows!(Mode::Thread(Region::Queue));
    const THREAD: [Binding; 31] = composer_rows!(Mode::Thread(Region::Composer));
    const CHAT: [Binding; 13] = chat_rows!(Mode::Thread(Region::Chat));
    const QUEUE_REGION: [Binding; 8] = queue_rows!(Mode::Thread(Region::Queue));
    const CT_AGENTS: [Binding; 5] = alt_common_rows!(Mode::Thread(Region::Agents));
    const AGENTS: [Binding; 7] = agents_rows!(Mode::Thread(Region::Agents));
    [
        // ── THE ROOT: projects ───────────────────────────────────────────
        RA_ROOT[0],
        RA_ROOT[1],
        RA_ROOT[2],
        RA_ROOT[3],
        RA_ROOT[4],
        RA_ROOT[5],
        RA_ROOT[6],
        RA_ROOT[7],
        Binding {
            // **THE ROOT DOES NOT EXIT ON `←`.** It jumps to the first row,
            // which is exactly what the AUI does at Root
            // (`apps/aui/src/dispatch/root.rs:64-72`). An accidental `←`
            // must never end the session.
            //
            // **AND IT MEANS ONE THING AGAIN.** It used to collapse an
            // expanded band first, when a band could be expanded in place;
            // bands do not expand any more — `→` on a header opens that band's
            // PAGE — so the branch is gone from `Intent::Top` and this row's
            // act text no longer has to describe two acts.
            keys: KeySet::These(&[Key::Left]),
            mode: Mode::Projects,
            intent: Intent::Top,
            act: "the first row of the menu; ← never exits the root",
            short: "first",
            footer_rank: None,
            needs: Need::Always,
        },
        // ── REORDERING A PIN: `ctrl-↑` / `ctrl-↓` ────────────────────────
        //
        // **WHY THESE TWO KEYS, out of everything the root has left.** The
        // choice was made against the table above rather than by taste, and it
        // is a DECISION the operator may overturn in one beat:
        //
        //  1. **They collide with nothing in this mode.** The root's whole
        //     grammar is `→`/`enter`, `←`, `tab`, the six `scroll_rows`
        //     (`↑`/`^P`, `↓`/`^N`, `pgup`/`^U`, `pgdn`/`^D`, `home`,
        //     `end`) and the five `common_rows` (`?`, `u`, `r`, `^L`,
        //     `q`/`^C`, each with its `alt-` twin). `ctrl-↑` and `ctrl-↓` are
        //     in none of them. They ARE bound in `Mode::Thread`, where the
        //     composer reads them as buffer-top / buffer-bottom — a different
        //     mode, and see 3.
        //  2. **They are MEASURED to arrive on his stack.** `0052` bound the
        //     four ctrl-arrows only after `motion_keys_probe.sh` watched tmux
        //     3.5a with his own `extended-keys` settings emit `CSI 1;5A` /
        //     `CSI 1;5B`, and `input::to_key` already has the arm that keeps
        //     them distinguishable. Nothing here is a key we hope arrives —
        //     which is the whole reason `alt-↑`/`alt-↓`, the editor-world
        //     convention, was NOT taken: `Key` has no `AltUp`, `to_key` has no
        //     arm for it, and binding an unmeasured key is how `0052`'s
        //     original defect was born.
        //  3. **`ctrl` + arrow already MEANS "the strong form of that arrow"**
        //     in this face. In a thread `ctrl-↑` is `↑` taken to its limit; at
        //     the root `↑` moves the CURSOR up and `ctrl-↑` moves the ROW up.
        //     One family, learned once.
        //  4. **`ctrl-<letter>` was not taken** because that space is the
        //     documented GLOBAL grammar (see `common_rows`' doc comment): its
        //     safety property is that a modified letter means the same thing in
        //     every mode, and spending one on a root-only act would make the
        //     printed grammar a mode-dependent thing to learn. Round six made
        //     that space MORE crowded rather than less — the whole `opt-`
        //     grammar moved into it — so the argument is stronger now, not
        //     weaker.
        //
        // **ON A ROW THAT IS NOT PINNED THEY SAY SO.** `App::move_pin` puts a
        // sentence in the notice line naming what it aimed at; it is never a
        // silent no-op, and it never reorders from the `recent` band, where the
        // row would move somewhere he is not looking.
        // ── `o` — THE OPTIONS OF THE THREAD UNDER THE CURSOR ─────────────
        //
        // **His words**: *"I want to be able to jump straight into the options
        // of a thread by clicking o while my cursor is on the thread handle."*
        //
        // **WHY `o` IS FREE IN THIS MODE, checked against the table rather than
        // assumed.** `The root`'s whole grammar is `->`/`enter`, `<-` (which jumps to the first row), `tab`, the
        // six `scroll_rows` (`^P`, `^N`, `^U`/`pgup`, `^D`/`pgdn`, `home`,
        // `end`) and the five `common_rows` (`?`, `u`, `r`, `^L`,
        // `q`/`^C`, each with its `alt-` twin). `o` is in none of them, and
        // `one_key_one_meaning_in_one_mode` is the machine that says so.
        //
        // It is a BARE LETTER rather than an `alt-` one because the `alt-`
        // space is the documented GLOBAL grammar — an `alt-` letter means the
        // same thing in every mode — and this act only exists where a thread
        // row can be under the cursor. It is also the letter he said.
        // ── `r` — RENAME THE THREAD UNDER THE CURSOR ─────────────────────
        //
        // **His words**: *"On any thread throughout the application, I need to
        // be able to rename with a tap of the letter r."* Round three ruled
        // the collision with `Reload` by CONTEXT and it was never built; this
        // is the table catching up. `^R` is still reload here, from
        // `common_rows`, so nothing that was documented moved.
        //
        // It aims at the row's TITLE through `hit::activation`, exactly as `o`
        // does, so the key door and the pointer door onto a rename are one
        // path. A cursor that is not on a thread resolves to no thread and
        // nothing happens, silently: a miss is free.
        // ── `p` — PIN THE THREAD UNDER THE CURSOR, OR ADD A PROJECT ──────
        //
        // The ruled collision, resolved by CONTEXT in the shell and not by a
        // second key here. See [`Intent::AddProject`] for why the split is
        // total on this surface and why it is not bound on `Mode::List`.
        //
        // `footer_rank: None` for the reason `rename` carries it: the root
        // already spends seven ranks and five more columns pushed `q quit` —
        // the last hint, and `conformance_test.sh`'s paint anchor — off the
        // row at the 100 columns that suite probes. It IS in the `?` overlay.
        // ── `c` — DE-REGISTER THE PROJECT UNDER THE CURSOR ───────────────
        //
        // It presses the project sheet's own `remove` row, so the `asks first`
        // confirmation is the ONE that already exists. See
        // [`Intent::DeregisterProject`].
        // **AND THE SAME LETTER IS THE SECOND PRESS**, on the sheet `c` just
        // opened. `enter` is the other spelling and needs no row here: it is
        // the sheet's ordinary activation of the selected row, and the armed
        // row IS the selected row. Written out because the alternative —
        // leaving only `enter` — would have made *"press again"* a lie about
        // the key he had just pressed.
        //
        // On a THREAD sheet it resolves to no project and is a free miss; the
        // shell says so, not this table, which must not know what a sheet is
        // about.
        Binding {
            keys: KeySet::These(&[Key::Char('c')]),
            mode: Mode::Sheet,
            intent: Intent::DeregisterProject,
            act: "confirm the de-registration this sheet is asking about",
            short: "confirm",
            footer_rank: None,
            needs: Need::Always,
        },
        PROJECTS[0],
        PROJECTS[1],
        PROJECTS[2],
        PROJECTS[3],
        PROJECTS[4],
        PROJECTS[5],
        CP[0],
        CP[1],
        CP[2],
        CP[3],
        CP[4],
        // ── `opt-↑` / `opt-↓` — TO AND BETWEEN THE BAND HEADERS ──────────
        //
        // His words: *"opt-{up,down} in the main menu should jump to and
        // between band headers."*
        //
        // **THIS IS THE THIRD MEANING OF THAT KEY PAIR AND IT IS NOT A
        // COLLISION** — it is the same principle a third time. `opt-↑`/`opt-↓`
        // mean PARAGRAPH in the textarea, HIS OWN MESSAGES in the chat, and
        // BAND HEADERS here; all three are one table resolved by the region
        // that holds the selection, which is why they are three rows and not
        // three special cases. The band headers are already cursor stops, so
        // this pairs with machinery that exists rather than adding any.
        //
        // **IT IS THE ROOT'S AND NOT THE PAGE'S.** A `see all` page is ONE
        // band, so there is no header above and none below; a key bound to a
        // guaranteed no-op, with a legend row printed over it from this very
        // table, is a lie about what the surface answers to.
        // ── `n` — A NEW THREAD IN `miscellaneous`, AND IT OPENS ──────────
        //
        // **His words**: *"ok and where is the n for new thread. It should work
        // on the main page and create a misc thread. I need that asap."*
        //
        // **`footer_rank: Some(9)`, WHICH IS AFTER `q quit`, AND THAT IS
        // DELIBERATE.** The root already spends ranks 0–8 (`? keys`, `→ open`,
        // `tab activity`, `o options`, `u queue`, `r rename`, `end newest`,
        // `q quit`) and `render::footer_line` BREAKS on the first hint that
        // does not fit — so a rank ahead of `quit` would push `quit` off the
        // row at the 100 columns `conformance_test.sh` probes, which is the
        // measured reason `p` and `c` carry no rank at all. Rank 9 means: this
        // hint prints on a terminal with room and is the first to go on one
        // without, and it displaces nothing. It is in the `?` overlay
        // unconditionally, which is where `overlay_rows` puts every row of the
        // table.
        //
        // It is a BARE LETTER, which is the letter he said; the `ctrl-`/`alt-`
        // space is the documented GLOBAL grammar and this act exists on one
        // surface.
        Binding {
            keys: KeySet::These(&[Key::Char('n')]),
            mode: Mode::Projects,
            intent: Intent::NewThread,
            act: "start a new thread in miscellaneous — name it, enter, and it opens",
            short: "new thread",
            footer_rank: Some(9),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::AltUp]),
            mode: Mode::Projects,
            intent: Intent::BandPrev,
            act: "to the band header above — pinned, recent, projects",
            short: "band up",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::AltDown]),
            mode: Mode::Projects,
            intent: Intent::BandNext,
            act: "to the band header below — pinned, recent, projects",
            short: "band down",
            footer_rank: None,
            needs: Need::Always,
        },
        // ── ONE BAND'S OWN PAGE: `see all` ───────────────────────────────
        //
        // **THE WHOLE ROOT ROW GRAMMAR, AND ONE ROW THAT DIFFERS.** The eight
        // acts above it are the same eight the root gets, from the same macro.
        // This is the difference, and it is the reason the page is a `Mode`:
        //
        // > Going left from there goes back to the main menu.
        //
        // `esc` is bound alongside `←`, as `Mode::List` and `Mode::Archived`
        // both bind it, because on a body surface with a door out those two
        // keys are one gesture. (His *"I don't want ESC to do that"* was about
        // a THREAD, where `esc` means come-home-to-the-box.)
        RA_PAGE[0],
        RA_PAGE[1],
        RA_PAGE[2],
        RA_PAGE[3],
        RA_PAGE[4],
        RA_PAGE[5],
        RA_PAGE[6],
        RA_PAGE[7],
        Binding {
            keys: KeySet::These(&[Key::Left, Key::Esc]),
            mode: SEE_ALL,
            intent: Intent::Back,
            act: "back to the main menu",
            short: "menu",
            footer_rank: Some(2),
            needs: Need::Always,
        },
        SA_SCROLL[0],
        SA_SCROLL[1],
        SA_SCROLL[2],
        SA_SCROLL[3],
        SA_SCROLL[4],
        SA_SCROLL[5],
        SA_COMMON[0],
        SA_COMMON[1],
        SA_COMMON[2],
        SA_COMMON[3],
        SA_COMMON[4],
        // ── one project's threads ────────────────────────────────────────
        Binding {
            keys: KeySet::These(&[Key::Right, Key::Enter]),
            mode: Mode::List,
            intent: Intent::Activate,
            act: "open this thread (a click SELECTS the row; this opens it)",
            short: "open",
            footer_rank: Some(1),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Left, Key::Esc]),
            mode: Mode::List,
            intent: Intent::Back,
            act: "back to the projects",
            short: "projects",
            footer_rank: Some(2),
            needs: Need::Always,
        },
        // ── `o` — THE OPTIONS OF THE THREAD UNDER THE CURSOR ─────────────
        //
        // **His words**: *"I want to be able to jump straight into the options
        // of a thread by clicking o while my cursor is on the thread handle."*
        //
        // **WHY `o` IS FREE IN THIS MODE, checked against the table rather than
        // assumed.** `A project's list`'s whole grammar is `->`/`enter`, `<-`/`esc`, `tab`, the
        // six `scroll_rows` (`^P`, `^N`, `^U`/`pgup`, `^D`/`pgdn`, `home`,
        // `end`) and the five `common_rows` (`?`, `u`, `r`, `^L`,
        // `q`/`^C`, each with its `alt-` twin). `o` is in none of them, and
        // `one_key_one_meaning_in_one_mode` is the machine that says so.
        //
        // It is a BARE LETTER rather than an `alt-` one because the `alt-`
        // space is the documented GLOBAL grammar — an `alt-` letter means the
        // same thing in every mode — and this act only exists where a thread
        // row can be under the cursor. It is also the letter he said.
        Binding {
            keys: KeySet::These(&[Key::Char('o')]),
            mode: Mode::List,
            intent: Intent::Options,
            act: "options for the thread OR the project under the cursor",
            short: "options",
            footer_rank: Some(3),
            needs: Need::Always,
        },
        // ── `r` — RENAME THE THREAD UNDER THE CURSOR ─────────────────────
        //
        // **His words**: *"On any thread throughout the application, I need to
        // be able to rename with a tap of the letter r."* Round three ruled
        // the collision with `Reload` by CONTEXT and it was never built; this
        // is the table catching up. `^R` is still reload here, from
        // `common_rows`, so nothing that was documented moved.
        //
        // It aims at the row's TITLE through `hit::activation`, exactly as `o`
        // does, so the key door and the pointer door onto a rename are one
        // path. A cursor that is not on a thread resolves to no thread and
        // nothing happens, silently: a miss is free.
        // **`n` INSIDE A PROJECT STARTS A THREAD IN THAT PROJECT** (TUI-P1a,
        // checklist TC-01). His words: *"if I go into a project and I press
        // the letter N, it should create a new thread in that project. But
        // it's not."* No footer rank: the list's footer is full, and the `?`
        // overlay carries every row.
        Binding {
            keys: KeySet::These(&[Key::Char('n')]),
            mode: Mode::List,
            intent: Intent::NewThread,
            act: "start a new thread in this project — name it, enter, and it opens",
            short: "new thread",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Char('r')]),
            mode: Mode::List,
            intent: Intent::Rename,
            act: "rename the thread under the cursor",
            short: "rename",
            // **IN THE FOOTER NOW, AND THAT IS SCOPE 4.** *"rename and pin
            // keybindings aren't shown and they're the most important."* It
            // was `None` when it landed, for a reason that has since expired:
            // the root spent seven ranks and the eighth pushed `q quit` — the
            // last hint, and `conformance_test.sh`'s paint anchor — off the row
            // at 100 columns. This round deleted `f filter` (rank 4) and moved
            // the stdout toggle (rank 2) out of this mode's table, so there are
            // two ranks and eleven columns free; rename takes the one `filter`
            // vacated and the footer is measurably narrower than it was.
            footer_rank: Some(4),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Tab]),
            mode: Mode::List,
            intent: Intent::SwapView,
            act: "switch to the live event lane",
            short: "activity",
            footer_rank: Some(5),
            needs: Need::Always,
        },
        LIST[0],
        LIST[1],
        LIST[2],
        LIST[3],
        LIST[4],
        LIST[5],
        CL[0],
        CL[1],
        CL[2],
        CL[3],
        CL[4],
        // ── one thread's chat: THE COMPOSER OWNS THE KEYBOARD ────────────
        //
        // **`←` IS NO LONGER BACK HERE AND `→` IS NO LONGER ACTIVATE**, and
        // that is the composer's cost, paid deliberately. A textarea that does
        // not answer to the arrow keys is not a textarea. `esc` carries back on
        // its own — it always did, as `←`'s unlisted synonym — and expansion,
        // which `→` used to reach, is `^E` and is still one click on the row.
        // The `x` ruling is untouched: expansion is an ACTIVATION, resolved
        // through `hit::activation`, and the mouse and the keyboard still share
        // the one path.
        //
        // The GLOBALS come first and the composer's own rows follow, which
        // matters only for `KeySet::Printable` at the very end: `^S` is
        // found before any character row can claim it, and `s` falls through to
        // being an `s`.
        CT[0],
        CT[1],
        CT[2],
        CT[3],
        CT[4],
        THREAD[0],
        THREAD[1],
        THREAD[2],
        THREAD[3],
        THREAD[4],
        THREAD[5],
        THREAD[6],
        THREAD[7],
        THREAD[8],
        THREAD[9],
        THREAD[10],
        THREAD[11],
        THREAD[12],
        THREAD[13],
        THREAD[14],
        THREAD[15],
        THREAD[16],
        THREAD[17],
        THREAD[18],
        THREAD[19],
        THREAD[20],
        THREAD[21],
        THREAD[22],
        THREAD[23],
        THREAD[24],
        THREAD[25],
        THREAD[26],
        THREAD[27],
        THREAD[28],
        THREAD[29],
        THREAD[30],
        // ── THE CHAT REGION: the one place the arrows scroll ─────────────
        CT_CHAT[0],
        CT_CHAT[1],
        CT_CHAT[2],
        CT_CHAT[3],
        CT_CHAT[4],
        thread_reload_row!(Mode::Thread(Region::Chat)),
        CHAT[0],
        CHAT[1],
        CHAT[2],
        CHAT[3],
        CHAT[4],
        CHAT[5],
        CHAT[6],
        CHAT[7],
        CHAT[8],
        CHAT[9],
        CHAT[10],
        CHAT[11],
        CHAT[12],
        // ── THE CLIENT QUEUE REGION ──────────────────────────────────────
        CT_QUEUE[0],
        CT_QUEUE[1],
        CT_QUEUE[2],
        CT_QUEUE[3],
        CT_QUEUE[4],
        thread_reload_row!(Mode::Thread(Region::Queue)),
        QUEUE_REGION[0],
        QUEUE_REGION[1],
        QUEUE_REGION[2],
        QUEUE_REGION[3],
        QUEUE_REGION[4],
        QUEUE_REGION[5],
        QUEUE_REGION[6],
        QUEUE_REGION[7],
        // ── THE SUBAGENT BAND: the walk's fourth stop, and the tree he
        //    navigates in it — see `agents_rows`.
        CT_AGENTS[0],
        CT_AGENTS[1],
        CT_AGENTS[2],
        CT_AGENTS[3],
        CT_AGENTS[4],
        thread_reload_row!(Mode::Thread(Region::Agents)),
        AGENTS[0],
        AGENTS[1],
        AGENTS[2],
        AGENTS[3],
        AGENTS[4],
        AGENTS[5],
        AGENTS[6],
        // ── the live event lane ──────────────────────────────────────────
        Binding {
            keys: KeySet::These(&[Key::Tab, Key::Left, Key::Esc]),
            mode: Mode::Activity,
            intent: Intent::SwapView,
            act: "back to the thread list",
            short: "threads",
            footer_rank: Some(1),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Right]),
            mode: Mode::Activity,
            intent: Intent::Activate,
            act: "expand a folded row (or click the ▸ beside it)",
            short: "expand",
            footer_rank: Some(5),
            needs: Need::Always,
        },
        // **THE EVENT LANE KEEPS BARE `r` AS RELOAD**, and it is written out
        // here rather than left in `common_rows` because the other two body
        // modes have spent that letter on RENAME. There is no thread row to
        // stand on in the lane, so the act `r` would rename is not expressible
        // here — and taking the letter away from a mode that can still use it
        // would be a uniformity nobody asked for. `^R` means reload in all
        // four modes, which is the invariant that survives.
        Binding {
            keys: KeySet::These(&[Key::Char('r')]),
            mode: Mode::Activity,
            intent: Intent::Reload,
            act: "re-ask the daemon for everything this view shows",
            short: "reload",
            footer_rank: None,
            needs: Need::Always,
        },
        ACTIVITY[0],
        ACTIVITY[1],
        ACTIVITY[2],
        ACTIVITY[3],
        ACTIVITY[4],
        ACTIVITY[5],
        CA[0],
        CA[1],
        CA[2],
        CA[3],
        CA[4],
        // ── the `?` overlay ──────────────────────────────────────────────
        Binding {
            keys: KeySet::These(&[Key::Ctrl('c')]),
            mode: Mode::Help,
            intent: Intent::Quit,
            act: "quit, restoring the terminal",
            short: "quit",
            footer_rank: None,
            needs: Need::Always,
        },
        // **`↑`/`↓` ARE BACK, AND NOW THEY SCROLL THE OVERLAY.**
        //
        // `0036` deleted them and was right to: they resolved through
        // `App::up`, whose `_` arm moves the viewport of the view UNDERNEATH,
        // while `help_overlay` took no scroll offset at all — so they silently
        // scrolled the transcript behind the overlay and never the overlay. A
        // binding that does the wrong thing quietly is worse than no binding.
        //
        // The fix then was to make the overlay FIT, and that worked until the
        // composer arrived with forty bindings of its own. A thread's grammar
        // does not fit his window at any factoring, so the overlay is now
        // contextual AND scrollable, `ViewState::help_from` is the offset these
        // move, and `app.rs` has an explicit Help arm so they can never reach
        // the view underneath again. `j`/`k` stay cut — the arrows are the
        // grammar and a second one costs two letters.
        Binding {
            keys: KeySet::These(&[Key::Up]),
            mode: Mode::Help,
            intent: Intent::ScrollUp,
            act: "up one row OF THIS LIST",
            short: "up",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Down]),
            mode: Mode::Help,
            intent: Intent::ScrollDown,
            act: "down one row of this list",
            short: "down",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::PageUp]),
            mode: Mode::Help,
            intent: Intent::PageUp,
            act: "up one screen of this list",
            short: "page up",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::PageDown]),
            mode: Mode::Help,
            intent: Intent::PageDown,
            act: "down one screen of this list",
            short: "page down",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::Any,
            mode: Mode::Help,
            intent: Intent::Back,
            act: "close this overlay",
            short: "close",
            footer_rank: Some(0),
            needs: Need::Always,
        },
        // ── the queue view ───────────────────────────────────────────────
        Binding {
            keys: KeySet::These(&[Key::Char('r')]),
            mode: Mode::Queue,
            intent: Intent::Reload,
            act: "re-ask the daemon for this thread's queue",
            short: "reload",
            footer_rank: Some(1),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('c')]),
            mode: Mode::Queue,
            intent: Intent::Quit,
            act: "quit, restoring the terminal",
            short: "quit",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::Any,
            mode: Mode::Queue,
            intent: Intent::Back,
            act: "close the queue view",
            short: "close",
            footer_rank: Some(0),
            needs: Need::Always,
        },
        // ── THE SHEET (§5) ───────────────────────────────────────────────
        //
        // **A MENU, NOT A TEXTAREA.** The act rows are walked and activated,
        // so the arrows move a cursor and `->`/enter act. `<-` CLOSES — §5's
        // own footer says `<- closes` — which is why this mode does NOT use
        // `KeySet::Any` the way Help and Queue do: the field-opening rows need
        // `enter` to mean ACTIVATE, and an `Any` row at the end would swallow
        // every key the sheet has not bound into a dismissal.
        Binding {
            keys: KeySet::These(&[Key::Up, Key::Ctrl('p')]),
            mode: Mode::Sheet,
            intent: Intent::ScrollUp,
            act: "up one act",
            short: "up",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Down, Key::Ctrl('n')]),
            mode: Mode::Sheet,
            intent: Intent::ScrollDown,
            act: "down one act",
            short: "down",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Right, Key::Enter]),
            mode: Mode::Sheet,
            intent: Intent::Activate,
            act: "do the act under the cursor",
            short: "do it",
            footer_rank: Some(1),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Left, Key::Esc]),
            mode: Mode::Sheet,
            intent: Intent::Back,
            act: "close the sheet and go back to where it was opened from",
            short: "closes",
            footer_rank: Some(0),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('c')]),
            mode: Mode::Sheet,
            intent: Intent::Quit,
            act: "quit, restoring the terminal",
            short: "quit",
            footer_rank: None,
            needs: Need::Always,
        },
        // ── THE SHEET'S FIELD ────────────────────────────────────────────
        //
        // **HERE THE LETTERS ARE TEXT.** `c` types a `c`; it does not compact.
        // That single fact is why this is a second MODE rather than a flag on
        // the sheet — see `Mode::SheetField`.
        Binding {
            keys: KeySet::These(&[Key::Enter]),
            mode: Mode::SheetField,
            intent: Intent::SheetCommit,
            act: "do the act with what is typed",
            short: "do it",
            footer_rank: Some(1),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::AltEnter, Key::Ctrl('j')]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::Newline),
            act: "newline (works on every terminal, with no protocol at all)",
            short: "newline",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::ShiftEnter]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::Newline),
            act: "newline (an alias — the newline row above always works)",
            short: "newline",
            footer_rank: None,
            needs: Need::Enhanced,
        },
        // **`<-` LEAVES THE FIELD AND KEEPS WHAT IS TYPED**, which is
        // `DEC-13`'s posture and `0060`'s rule in one key: *"compaction
        // instructions already typed are KEPT in the field."* A field that
        // emptied itself on the way out would eat his words every time the
        // link blinked.
        Binding {
            keys: KeySet::These(&[Key::Esc]),
            mode: Mode::SheetField,
            intent: Intent::Back,
            act: "leave the field — WHAT IS TYPED IS KEPT",
            short: "keep & close",
            footer_rank: Some(0),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Left]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::Left),
            act: "left one character (a whole emoji is one character)",
            short: "left",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Right]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::Right),
            act: "right one character",
            short: "right",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::AltLeft]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::WordLeft),
            act: "left one word",
            short: "word left",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::AltRight]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::WordRight),
            act: "right one word",
            short: "word right",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Up]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::Up),
            act: "up one line",
            short: "up",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Down]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::Down),
            act: "down one line",
            short: "down",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Home, Key::Ctrl('a'), Key::CtrlLeft]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::RowStart),
            act: "start of the line",
            short: "line start",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::End, Key::Ctrl('e'), Key::CtrlRight]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::RowEnd),
            act: "end of the line",
            short: "line end",
            footer_rank: None,
            needs: Need::Always,
        },
        // `^H` is bound beside backspace for the reason the composer binds it:
        // a terminal whose erase is `^H` sends `\x08`, and without this row
        // backspace is a DEAD KEY in the box he types into.
        Binding {
            keys: KeySet::These(&[Key::Backspace, Key::Ctrl('h')]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::Backspace),
            act: "delete the character before the cursor, whole",
            short: "backspace",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Delete, Key::Ctrl('d')]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::Delete),
            act: "delete the character after the cursor, whole",
            short: "delete",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('u')]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::KillToStart),
            act: "cut to the start of the line",
            short: "kill to start",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('k')]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::KillToEnd),
            act: "cut to the end of the line",
            short: "kill to end",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('w'), Key::AltBackspace]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::KillWord),
            act: "cut the word before the cursor",
            short: "kill word",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('y')]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::Yank),
            act: "put back whatever the last cut took",
            short: "yank",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('z')]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::Undo),
            act: "undo the last edit RUN — a whole paste undoes in one",
            short: "undo",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('r')]),
            mode: Mode::SheetField,
            intent: Intent::Compose(Edit::Redo),
            act: "redo",
            short: "redo",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('c')]),
            mode: Mode::SheetField,
            intent: Intent::Quit,
            act: "quit, restoring the terminal",
            short: "quit",
            footer_rank: None,
            needs: Need::Always,
        },
        // LAST in this mode, exactly as it is last in the thread: a printable
        // key is a character, and every binding above it had first refusal.
        Binding {
            keys: KeySet::Printable,
            mode: Mode::SheetField,
            intent: Intent::Type('\0'),
            act: "type",
            short: "type",
            footer_rank: None,
            needs: Need::Always,
        },
        // ── THE ARCHIVED SET (§1) ────────────────────────────────────────
        //
        // **ONE DOOR BACK, ONE ACT.** *"Unarchiving is the only act … and
        // there is no read-only second entrance to a room you said you didn't
        // want to see."* So the grammar is four keys and there is nothing else
        // to press: walk, bring one back, leave.
        Binding {
            keys: KeySet::These(&[Key::Up, Key::Ctrl('p')]),
            mode: Mode::Archived,
            intent: Intent::ScrollUp,
            act: "up one project",
            short: "up",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Down, Key::Ctrl('n')]),
            mode: Mode::Archived,
            intent: Intent::ScrollDown,
            act: "down one project",
            short: "down",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Right, Key::Enter]),
            mode: Mode::Archived,
            intent: Intent::Activate,
            act: "bring this project back to the root",
            short: "bring back",
            footer_rank: Some(1),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Left, Key::Esc]),
            mode: Mode::Archived,
            intent: Intent::Back,
            act: "back to the root",
            short: "back",
            footer_rank: Some(0),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('c')]),
            mode: Mode::Archived,
            intent: Intent::Quit,
            act: "quit, restoring the terminal",
            short: "quit",
            footer_rank: None,
            needs: Need::Always,
        },
        // ── `+ new project`, TYPED IN THE ROW (§1, §10) ──────────────────
        //
        // **HERE THE LETTERS ARE TEXT.** `?` types a `?`; it does not raise the
        // help overlay. That single fact is why this is a mode — see
        // `Mode::NewProject`.
        //
        // A name is ONE SHORT ANSWER, so the grammar is the short-answer half
        // of the composer's and no more: no newline, no queue, no send-on-empty
        // and no audio. `enter` creates it; `esc` leaves the field with the
        // name still in it.
        Binding {
            keys: KeySet::These(&[Key::Enter]),
            mode: Mode::NewProject,
            intent: Intent::CreateProject,
            act: "add it — a bare name makes a project, a path registers one",
            short: "add",
            footer_rank: Some(1),
            needs: Need::Always,
        },
        // **`tab` COMPLETES A PATH.** It is the one key in this mode that is
        // not text and not a motion, and it is here rather than in the
        // composer's grammar because the composer has no paths in it.
        Binding {
            keys: KeySet::These(&[Key::Tab]),
            mode: Mode::NewProject,
            intent: Intent::CompletePath,
            act: "complete the path — press again to go into a directory",
            short: "complete",
            footer_rank: Some(2),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Left, Key::Ctrl('b')]),
            mode: Mode::NewProject,
            intent: Intent::Compose(Edit::Left),
            act: "left one character",
            short: "left",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Right, Key::Ctrl('f')]),
            mode: Mode::NewProject,
            intent: Intent::Compose(Edit::Right),
            act: "right one character",
            short: "right",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::AltLeft]),
            mode: Mode::NewProject,
            intent: Intent::Compose(Edit::WordLeft),
            act: "back one word",
            short: "word left",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::AltRight]),
            mode: Mode::NewProject,
            intent: Intent::Compose(Edit::WordRight),
            act: "forward one word",
            short: "word right",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('a'), Key::Home]),
            mode: Mode::NewProject,
            intent: Intent::Compose(Edit::RowStart),
            act: "to the start of the name",
            short: "start",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('e'), Key::End]),
            mode: Mode::NewProject,
            intent: Intent::Compose(Edit::RowEnd),
            act: "to the end of the name",
            short: "end",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Backspace]),
            mode: Mode::NewProject,
            intent: Intent::Compose(Edit::Backspace),
            act: "delete the character before the caret",
            short: "delete",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Delete, Key::Ctrl('d')]),
            mode: Mode::NewProject,
            intent: Intent::Compose(Edit::Delete),
            act: "delete the character under the caret",
            short: "delete",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('k')]),
            mode: Mode::NewProject,
            intent: Intent::Compose(Edit::KillToEnd),
            act: "delete to the end of the name",
            short: "kill",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('u')]),
            mode: Mode::NewProject,
            intent: Intent::Compose(Edit::KillToStart),
            act: "delete to the start of the name",
            short: "kill",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('w'), Key::AltBackspace]),
            mode: Mode::NewProject,
            intent: Intent::Compose(Edit::KillWord),
            act: "delete the word before the caret",
            short: "kill word",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Esc]),
            mode: Mode::NewProject,
            intent: Intent::Back,
            act: "leave the field — what is typed is kept",
            short: "keeps it",
            footer_rank: Some(0),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('c')]),
            mode: Mode::NewProject,
            intent: Intent::Quit,
            act: "quit, restoring the terminal",
            short: "quit",
            footer_rank: None,
            needs: Need::Always,
        },
        // LAST in this mode: a printable key is a character, and every binding
        // above it had first refusal.
        Binding {
            keys: KeySet::Printable,
            mode: Mode::NewProject,
            intent: Intent::Type('\0'),
            act: "type",
            short: "type",
            footer_rank: None,
            needs: Need::Always,
        },
        // ── MIC MODE ─────────────────────────────────────────────────────
        //
        // **THE LETTERS ARE NOT TEXT HERE**, which is the whole reason this is
        // a mode and not a flag. There is no textarea in this band, so `r` is
        // free to be RECORD and the `?` overlay can say so truthfully.
        //
        // `enter` keeps its meaning from the thread — SEND THE WHOLE QUEUE —
        // because his grammar has two acts and changing the send key between
        // two halves of one queue would be a third thing to remember.
        Binding {
            keys: KeySet::These(&[Key::Char('r'), Key::Char(' ')]),
            mode: Mode::Mic,
            intent: Intent::MicRecord,
            act: "start recording, or stop the recording that is running",
            short: "record",
            footer_rank: Some(1),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Enter]),
            mode: Mode::Mic,
            intent: Intent::Send,
            act: "SEND the whole queue — everything held here, as one message",
            short: "send all",
            footer_rank: Some(2),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Char('p')]),
            mode: Mode::Mic,
            intent: Intent::MicPlayLast,
            act: "play the most recent recording back (press again to stop it)",
            short: "play",
            footer_rank: Some(4),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Char('t')]),
            mode: Mode::Mic,
            intent: Intent::MicRetryFirst,
            act: "transcribe the first failed recording again — the audio was kept",
            short: "retry",
            footer_rank: Some(6),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Char('d')]),
            mode: Mode::Mic,
            intent: Intent::MicDropFirst,
            act: "take the first failed recording out of the queue, capture and all",
            short: "drop",
            footer_rank: Some(7),
            needs: Need::Always,
        },
        Binding {
            // `^T` leaves by the same key it entered by — see the note on the
            // composer's mic row for why that key exists.
            keys: KeySet::These(&[Key::Esc, Key::Ctrl('t')]),
            mode: Mode::Mic,
            intent: Intent::MicMode,
            act: "leave mic mode and go back to the textarea",
            short: "leave mic",
            footer_rank: Some(3),
            needs: Need::Always,
        },
        // **THE SAME `^V` THAT REACHES HERE FROM THE THREAD MEANS THE SAME
        // THING ONCE HE IS HERE**, so the grammar he learns is one key and not
        // "one key to get in and a different one once you are in". `r` and
        // `space` stay the TOGGLE (tap toggles, hold streams — `0120`); this is
        // the idempotent START, and it says so out loud rather than declining
        // silently when a capture is already open.
        Binding {
            keys: KeySet::These(&[Key::Ctrl('v')]),
            mode: Mode::Mic,
            intent: Intent::MicRecordNow,
            act: "start recording, if one is not already running",
            short: "record",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('o')]),
            mode: Mode::Mic,
            intent: Intent::QueueOverlay,
            act: "open the queue view — every item, spoken and typed",
            short: "queue",
            footer_rank: Some(5),
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('/'), Key::Ctrl('7')]),
            mode: Mode::Mic,
            intent: Intent::Help,
            act: "the key table for this mode",
            short: "keys",
            footer_rank: Some(0),
            needs: Need::Always,
        },
        // The transcript is still on screen behind the mic band, so it still
        // has to be readable. Same keys it answers to in a thread.
        Binding {
            keys: KeySet::These(&[Key::Up, Key::Ctrl('p')]),
            mode: Mode::Mic,
            intent: Intent::ScrollUp,
            act: "up one row of the transcript (leaves follow-mode)",
            short: "up",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Down, Key::Ctrl('n')]),
            mode: Mode::Mic,
            intent: Intent::ScrollDown,
            act: "down one row of the transcript",
            short: "down",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::PageUp]),
            mode: Mode::Mic,
            intent: Intent::PageUp,
            act: "up one screen of the transcript",
            short: "page up",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::PageDown]),
            mode: Mode::Mic,
            intent: Intent::PageDown,
            act: "down one screen of the transcript",
            short: "page down",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('l')]),
            mode: Mode::Mic,
            intent: Intent::Redraw,
            act: "repaint the screen",
            short: "redraw",
            footer_rank: None,
            needs: Need::Always,
        },
        Binding {
            keys: KeySet::These(&[Key::Ctrl('c')]),
            mode: Mode::Mic,
            intent: Intent::Quit,
            act: "quit, restoring the terminal",
            short: "quit",
            footer_rank: None,
            needs: Need::Always,
        },
        // ── THE LITTLE RENAME MODAL ──────────────────────────────────────
        //
        // Spliced in WHOLE from `RENAME_MODAL_ROWS`, at the END of the table,
        // and written nowhere else in this file. See that const for the
        // grammar and for why it is one block.
        RENAME_MODAL_ROWS[0],
        RENAME_MODAL_ROWS[1],
        RENAME_MODAL_ROWS[2],
        RENAME_MODAL_ROWS[3],
        RENAME_MODAL_ROWS[4],
        RENAME_MODAL_ROWS[5],
        RENAME_MODAL_ROWS[6],
        RENAME_MODAL_ROWS[7],
        RENAME_MODAL_ROWS[8],
        RENAME_MODAL_ROWS[9],
        RENAME_MODAL_ROWS[10],
        RENAME_MODAL_ROWS[11],
        RENAME_MODAL_ROWS[12],
        RENAME_MODAL_ROWS[13],
        RENAME_MODAL_ROWS[14],
        RENAME_MODAL_ROWS[15],
    ]
};

/// What `key` means in `mode`. A SCAN of [`BINDINGS`], never a `match`.
///
/// First match wins, which is what lets [`KeySet::Any`] sit at the end of a
/// mode's rows as a catch-all without swallowing the specific ones above it.
pub fn intent(key: Key, mode: Mode) -> Intent {
    // One table, asked under the mode's own row-set — see [`Mode::table_key`].
    let mode = mode.table_key();
    for b in BINDINGS {
        if b.mode == mode && b.keys.contains(key) {
            // The printable row is one binding standing for the whole
            // alphabet, so this is the one place the KEY travels with the
            // intent rather than being consumed by it. The alternative was a
            // row per character. (The lane-digit row was the other such place
            // and it is gone with the filter.)
            if let (Intent::Type(_), Key::Char(c)) = (b.intent, key) {
                return Intent::Type(c);
            }
            return b.intent;
        }
    }
    Intent::None
}

/// The footer hints for a mode, best-ranked first, **filtered by what the
/// terminal actually negotiated**, and **INVARIANT ONES FIRST**. The renderer
/// takes as many as fit and drops the rest.
///
/// The order is [`footer_split`]'s two halves concatenated; see there for what
/// makes the left half load-bearing.
pub fn footer_hints(mode: Mode, caps: Caps) -> Vec<(String, &'static str)> {
    let (mut stable, variable) = footer_split(mode, caps);
    stable.extend(variable);
    stable
}

/// One side of the footer: the key label and the act, in the order they print.
pub type FooterHints = Vec<(String, &'static str)>;

/// **THE LEGEND IS PER-SECTION, AND THE STABILITY IS THE FEATURE.**
///
/// His words: *"each section, when in that band, should display its own
/// keybindings, not all of them at once. Keep the invariant ones at the left so
/// its easy to see that they stay the same."*
///
/// A legend that merely FILTERED per band would have missed the second
/// sentence, which is the load-bearing one: he is using POSITION to tell him
/// what does not change. So the footer has a STABLE LEFT half and a VARIABLE
/// RIGHT half, and because the left half is the same sequence in every region
/// of a mode it occupies the same COLUMNS in every region — the hints that
/// moved are exactly the ones that are new.
///
/// **WHICH ROWS ARE INVARIANT IS DERIVED, NEVER DECLARED.** A hint is stable
/// when the same key spells the same act in EVERY region of this mode's family.
/// A `stable: bool` field on [`Binding`] would be a second statement of a fact
/// the table already makes, and the day a row stopped being shared in one
/// region the field would still say it was.
pub fn footer_split(mode: Mode, caps: Caps) -> (FooterHints, FooterHints) {
    let here = ranked(mode, caps);
    // The REGIONS OF A THREAD are one family; every other mode is a family of
    // one, so nothing there is variable and its footer is exactly what it was.
    let family: Vec<Mode> = match mode {
        Mode::Thread(_) => DOCUMENTED_MODES
            .iter()
            .copied()
            .filter(|m| matches!(m, Mode::Thread(_)))
            .collect(),
        m => vec![m],
    };
    let elsewhere: Vec<Vec<(u8, String, &'static str)>> =
        family.iter().map(|m| ranked(*m, caps)).collect();
    let (mut stable, mut variable) = (Vec::new(), Vec::new());
    for (_, key, short) in here {
        let everywhere = elsewhere
            .iter()
            .all(|rows| rows.iter().any(|(_, k, s)| *k == key && *s == short));
        if everywhere {
            stable.push((key, short));
        } else {
            variable.push((key, short));
        }
    }
    (stable, variable)
}

/// The footer rows of one mode, best-ranked first.
fn ranked(mode: Mode, caps: Caps) -> Vec<(u8, String, &'static str)> {
    let mode = mode.table_key();
    let mut hits: Vec<(u8, String, &'static str)> = BINDINGS
        .iter()
        .filter(|b| b.mode == mode && b.needs.met(caps))
        .filter_map(|b| {
            b.footer_rank
                .map(|r| (r, first_key_label(&b.keys, caps.mods), b.short))
        })
        .collect();
    hits.sort_by_key(|(r, _, _)| *r);
    hits
}

/// The footer names the FIRST key of a set — order inside a binding is
/// presentation; the set is what the table means.
fn first_key_label(keys: &KeySet, m: ModKeys) -> String {
    match keys {
        KeySet::Any => "any".into(),
        KeySet::Printable => keys.label(m),
        KeySet::These(ks) => ks.first().map(|k| k.label(m)).unwrap_or_default(),
    }
}

/// Every row of a mode's grammar, for the `?` overlay — **rendered FROM THE
/// NEGOTIATED CAPABILITY SET**, so a binding this terminal cannot deliver is
/// never shown. The overlay renders THIS; it never carries a list of its own.
pub fn overlay_rows(mode: Mode, caps: Caps) -> Vec<(String, &'static str)> {
    let mode = mode.table_key();
    BINDINGS
        .iter()
        .filter(|b| b.mode == mode && b.needs.met(caps))
        .map(|b| (b.keys.label(caps.mods), b.act))
        .collect()
}

/// **HOW THIS MODE SPELLS ONE ACT — for the rendered rows that have to NAME a
/// key in a sentence.**
///
/// It exists because of a measured defect class, and the class is worth naming:
/// **six strings in this crate printed a literal `alt-` to the screen** while
/// every table-derived label printed `opt-` on his Mac, because they were
/// hand-written prose rather than derived from [`Key::label`]. They were wrong
/// about the HOST before this round and would have been wrong about the KEY
/// after it. A hand-corrected literal is the same defect with today's spelling.
///
/// So any row that has to say "press X to do Y" asks THIS, and the answer comes
/// from the same table the `?` overlay and the footer render from. There is then
/// no second place a key spelling can live, which is the only fix that holds.
///
/// `None` when the act has no key in that mode — reload in the composer, for
/// instance — so a caller must decide what to say instead rather than printing
/// a key that is not there.
pub fn spelling(intent: Intent, mode: Mode, caps: Caps) -> Option<String> {
    let mode = mode.table_key();
    BINDINGS
        .iter()
        .find(|b| b.mode == mode && b.intent == intent && b.needs.met(caps))
        .map(|b| b.keys.label(caps.mods))
}

/// **THE TWO KEYS THAT ANSWER A RECOVERED-DRAFT OFFER, and the one place they
/// are written down.**
///
/// `AcceptDraft` and `DiscardDraft` have no mnemonic ctrl home in the composer
/// (`^Y` is yank, `^D` is delete) and `0128`'s answer is better than any letter:
/// the offer is a MODAL QUESTION, live only while it is on screen, so `y`/`n`
/// answer it at no cost to the namespace at all. They are therefore NOT rows of
/// [`BINDINGS`] — a row would claim `y` and `n` unconditionally, in the one mode
/// where every letter is text — and the shell consults
/// [`draft_offer_intent`] only while `Composer::offer` is standing.
///
/// **The band that draws the offer reads these same two constants**, so the
/// words on screen and the keys that work cannot drift.
pub const DRAFT_OFFER_ACCEPT: Key = Key::Char('y');
/// See [`DRAFT_OFFER_ACCEPT`].
pub const DRAFT_OFFER_DISCARD: Key = Key::Char('n');

/// What a key means WHILE a recovered-draft offer is standing, and `None` for
/// every other key — which is the whole modal: anything else types, exactly as
/// it did before, and an edit answers the offer by implication.
///
/// It is here rather than in the shell so the key→act mapping lives with every
/// other key→act mapping, and so a test in this crate can read it.
pub fn draft_offer_intent(key: Key) -> Option<Intent> {
    match key {
        k if k == DRAFT_OFFER_ACCEPT => Some(Intent::AcceptDraft),
        k if k == DRAFT_OFFER_DISCARD => Some(Intent::DiscardDraft),
        _ => None,
    }
}

/// **THE BINDINGS THIS TERMINAL CANNOT DELIVER, so the overlay can say so out
/// loud instead of merely omitting them.**
///
/// Omission alone is honest but not helpful: he presses `shift-enter`, it
/// sends, and nothing on screen ever explains why. Naming the withheld keys —
/// with the reason — turns an invisible glitch into a stated limitation, which
/// is the only move available when the stack genuinely cannot tell two
/// keystrokes apart.
pub fn withheld(mode: Mode, caps: Caps) -> Vec<String> {
    let mode = mode.table_key();
    let mut out: Vec<String> = BINDINGS
        .iter()
        .filter(|b| b.mode == mode && !b.needs.met(caps))
        .map(|b| b.keys.label(caps.mods))
        .collect();
    out.dedup();
    out
}

/// Every mode that has a grammar worth showing, in the order the `?` overlay
/// shows them. `Help` is excluded: its whole grammar is "any key closes this",
/// and printing that inside itself is a joke the reader pays for in rows.
/// **THE THREAD IS THREE ENTRIES, NOT ONE** — one per region that has a
/// grammar — because `one_key_one_meaning_in_one_mode`, the overlay goldens and
/// the withheld-keys test all loop over this list, and a region that is not in
/// it is a grammar nothing ever checks.
///
/// `Region::Agents` is here with its whole grammar — the walk over the rows,
/// the drill, the way back and the way home. The region can only ever hold the
/// selection while the band exists, so what the overlay prints over it is true
/// whenever it can be raised.
pub const DOCUMENTED_MODES: &[Mode] = &[
    Mode::Projects,
    Mode::List,
    // Composer first: it is where a thread opens and where he lands.
    Mode::Thread(Region::Composer),
    Mode::Thread(Region::Chat),
    Mode::Thread(Region::Queue),
    Mode::Thread(Region::Agents),
    Mode::Mic,
    Mode::Activity,
    Mode::Queue,
    Mode::Sheet,
    Mode::SheetField,
    Mode::Archived,
    Mode::NewProject,
    // **ONE ENTRY FOR THREE PAGES**, and that is not a gap. The three bands'
    // pages answer to one block of rows — see [`Mode::table_key`] — so a
    // second and third entry here would loop the same assertions over the same
    // bindings and would silently stop meaning anything the day the collapse
    // changed. `the_page_reads_one_table_whichever_band_it_is` is what holds
    // the collapse itself.
    Mode::SeeAll(crate::model::BandId::Recent),
    // **THE LITTLE RENAME MODAL** (`0130`), added AT MERGE TIME. The modal and
    // the page were built in parallel worktrees, so neither worker could add
    // the other's entry — and `0130` declared this as an explicit post-merge
    // ask rather than guessing a count a sibling was also moving. That was the
    // right call: the count it would have guessed was wrong.
    Mode::RenameModal,
];

/// The four modes that have a BODY rather than being a way of reading one.
///
/// **What this const NO LONGER does: define a shared grammar.** Until the
/// composer landed, the `?` overlay printed all six modes and factored out the
/// block they had in common, because printing it six times cost more rows than
/// his pane has (`0036` measured 74 against 33). The overlay is now CONTEXTUAL
/// — it prints the ONE mode it was raised over — so there is nothing to factor
/// out, and `shared_rows`/`unshared_rows` are GONE rather than left unused.
///
/// They were also, by then, WRONG. A thread's grammar is disjoint from the
/// other three: raised over a thread, the shared block printed `q quit`,
/// `f filter` and `G newest`, none of which are bindings there — `q` types a
/// `q`. A help screen that documents keys the mode does not answer to is the
/// exact drift the one-table rule exists to make impossible, and it was this
/// mechanism doing it.
///
/// **THE THREAD IS NAMED BY ITS COMPOSER REGION HERE**, which is where a thread
/// opens and the region every global must work from. The `alt-` grammar is
/// asserted uniform across this list; the other regions carry the same rows
/// through the same macro, and `the_alt_grammar_is_uniform_across_every_region`
/// is what says so.
pub const BODY_MODES: &[Mode] = &[
    Mode::Projects,
    Mode::List,
    Mode::Thread(Region::Composer),
    Mode::Activity,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE THREE REGIONS, NAMED ONCE.** Every one of these assertions used
    /// to say `Mode::Thread`; each is now a statement about ONE region, and
    /// which one is the interesting half.
    const COMPOSER: Mode = Mode::Thread(Region::Composer);
    const CHAT: Mode = Mode::Thread(Region::Chat);
    const QUEUE: Mode = Mode::Thread(Region::Queue);

    /// **THE MOTION GRAMMAR `0052` ADDED, ASSERTED AS INTENTS.**
    ///
    /// Four limit keys and one word-delete. Each is bound BECAUSE it was
    /// measured arriving distinguishably on his stack (`motion_keys_probe.sh`,
    /// and `vt::tests::the_measured_motion_wire_table_decodes_to_distinguishable_keys`);
    /// this is the other half of that proof — that the key the wire produces
    /// reaches the act he asked for.
    ///
    /// FORCED: take `Key::CtrlLeft` out of the `RowStart` row and this goes red
    /// on the first assertion; take `Key::AltBackspace` out of the `KillWord`
    /// row and it goes red on the last.
    #[test]
    fn the_limit_keys_and_the_word_delete_reach_the_acts_he_asked_for() {
        // *"Ctrl+←/→/↑/↓ jump to the LIMIT in that direction — line start/end,
        // buffer top/bottom."*
        assert_eq!(
            intent(Key::CtrlLeft, COMPOSER),
            Intent::Compose(Edit::RowStart)
        );
        assert_eq!(
            intent(Key::CtrlRight, COMPOSER),
            Intent::Compose(Edit::RowEnd)
        );
        assert_eq!(
            intent(Key::CtrlUp, COMPOSER),
            Intent::Compose(Edit::BufStart)
        );
        assert_eq!(
            intent(Key::CtrlDown, COMPOSER),
            Intent::Compose(Edit::BufEnd)
        );
        // *"Opt+Delete deletes a word backward — alongside the existing ^W."*
        assert_eq!(
            intent(Key::AltBackspace, COMPOSER),
            Intent::Compose(Edit::KillWord)
        );
        assert_eq!(
            intent(Key::Ctrl('w'), COMPOSER),
            Intent::Compose(Edit::KillWord),
            "^W must not have been displaced by the key that joined it"
        );
        // The word motions `0042` built are untouched, and are what `alt-←`
        // still means after the ctrl forms landed beside them.
        assert_eq!(
            intent(Key::AltLeft, COMPOSER),
            Intent::Compose(Edit::WordLeft)
        );
        assert_eq!(
            intent(Key::AltRight, COMPOSER),
            Intent::Compose(Edit::WordRight)
        );
        // **AND THE UNMODIFIED ARROWS STILL MOVE ONE CHARACTER.** A limit key
        // that had displaced the plain arrow would be a far worse defect than
        // the missing binding it replaced.
        assert_eq!(intent(Key::Left, COMPOSER), Intent::Compose(Edit::Left));
        assert_eq!(
            intent(Key::Right, COMPOSER),
            Intent::Compose(Edit::Right)
        );
    }

    /// **THE `?` OVERLAY IS THE TABLE, so the new keys are IN it — and this
    /// asserts that rather than assuming it** (`0042`'s one-table rule: the
    /// readers of the table, never a hand-written list).
    #[test]
    fn the_new_motion_keys_are_printed_by_the_overlay_because_it_reads_the_table() {
        let printed: String = BINDINGS
            .iter()
            .filter(|b| b.mode == COMPOSER)
            .map(|b| b.keys.label(ModKeys::ALT))
            .collect::<Vec<_>>()
            .join(" | ");
        for want in ["^←", "^→", "^↑", "^↓", "alt-bksp"] {
            assert!(
                printed.contains(want),
                "{want} is bound but the overlay would never say so: {printed}"
            );
        }
    }

    /// The philosophy check, as a test: the `?` overlay's content IS the
    /// table, so a binding that exists without a row cannot be rendered and a
    /// row that exists without a binding cannot be pressed.
    #[test]
    fn every_binding_is_reachable_by_its_own_keys() {
        for b in BINDINGS {
            if let KeySet::These(keys) = b.keys {
                for &k in keys {
                    let got = intent(k, b.mode);
                    // First-match-wins means an earlier row may legitimately
                    // shadow this one; what must never happen is a key in the
                    // table resolving to nothing at all.
                    assert_ne!(
                        got,
                        Intent::None,
                        "{:?} in {:?} resolves to None",
                        k,
                        b.mode
                    );
                }
            }
        }
    }

    /// **THE CUTS, AS A TEST.** Each of these keys was ruled out; a later edit
    /// that restores one has to delete a line that says why.
    #[test]
    fn the_cut_keys_are_cut_in_every_mode_that_had_them() {
        for m in [Mode::Projects, Mode::List, Mode::Activity] {
            assert_eq!(
                intent(Key::Char('k'), m),
                Intent::None,
                "k survived in {m:?}"
            );
            assert_eq!(
                intent(Key::Char('j'), m),
                Intent::None,
                "j survived in {m:?}"
            );
            assert_eq!(
                intent(Key::Char('x'), m),
                Intent::None,
                "x survived in {m:?}"
            );
        }
        // **IN A THREAD THEY ARE TEXT, WHICH IS A DIFFERENT ANSWER FROM
        // `None`** and a strictly better one: the cut letters, and every other
        // letter, now type themselves.
        for c in [
            'k', 'j', 'x', 'v', 'y', 'M', 'q', 'f', 'u', 'r', 'g', 'G', '?',
        ] {
            assert_eq!(
                intent(Key::Char(c), COMPOSER),
                Intent::Type(c),
                "{c:?} is not text in a thread — the composer does not own the keyboard"
            );
        }
        // **HELP'S `↑`/`↓` ARE BACK, AND THE DIFFERENCE IS WHAT THEY MOVE.**
        // `0036` deleted them because they scrolled the view UNDERNEATH the
        // overlay; they return only now that `ViewState::help_from` exists for
        // them to move and `app.rs` has an explicit Help arm that intercepts
        // them before anything can reach the view below. `j`/`k` stay cut: the
        // arrows are the grammar, and a second spelling costs two letters.
        assert_eq!(intent(Key::Up, Mode::Help), Intent::ScrollUp);
        assert_eq!(intent(Key::PageDown, Mode::Help), Intent::PageDown);
        assert_eq!(intent(Key::Char('k'), Mode::Help), Intent::Back);
        // The arrows and the wheel are what remain, and `^P`/`^N` stay as
        // unlisted-in-the-footer aliases in the same row.
        assert_eq!(intent(Key::Up, Mode::List), Intent::ScrollUp);
        assert_eq!(intent(Key::Ctrl('p'), Mode::List), Intent::ScrollUp);
        assert_eq!(intent(Key::Down, Mode::Projects), Intent::ScrollDown);
        // `u` is KEPT in the list modes and is `^O` in a thread.
        assert_eq!(intent(Key::Char('u'), Mode::List), Intent::QueueOverlay);
        assert_eq!(intent(Key::Ctrl('o'), COMPOSER), Intent::QueueOverlay);
    }

    /// **THE MODIFIED GRAMMAR IS UNIFORM ACROSS EVERY BODY MODE, AND SINCE
    /// ROUND SIX THE MODIFIER IS `ctrl` — WITH ONE NAMED EXCEPTION.**
    ///
    /// This is the property that makes the bare-letter synonyms safe to keep
    /// where they cost nothing: there is ONE grammar to learn and it works
    /// everywhere, including the one mode where every letter is text.
    ///
    /// **His ruling, verbatim:** *"ctrl-* might be better because it doesn't
    /// have keybindings and it's universally understood to work. Let's just
    /// switch to all ctrl-* across the app instead of opt-*."* The reason is
    /// measured: macOS composes `opt-<letter>` into a glyph in the keyboard
    /// layer (`opt-u` is a dead diaeresis, `opt-r` is `®`, `opt-/` is `÷`), so
    /// those bytes never existed. Nothing composes under `ctrl`.
    ///
    /// **THE EXCEPTION IS RELOAD, AND IT IS ASSERTED AS AN ABSENCE RATHER THAN
    /// SKIPPED.** `^R` is `Compose(Redo)` in `Thread(Composer)`, the partner of
    /// `^Z` and its only spelling, so reload has NO key inside the textarea. A
    /// loop that quietly omitted the composer would pass on a table that had
    /// lost reload everywhere.
    #[test]
    fn every_global_answers_to_the_same_ctrl_key_in_every_body_mode() {
        for m in BODY_MODES {
            assert_eq!(
                intent(Key::Ctrl('o'), *m),
                Intent::QueueOverlay,
                "^O in {m:?}"
            );
            assert_eq!(intent(Key::Ctrl('/'), *m), Intent::Help, "^/ in {m:?}");
            // **BOTH DIALECTS OF ONE CHORD, IN ONE ROW.** `ctrl-/` arrives as
            // `CSI 27;5;47~` under modifyOtherKeys and as the bare byte `0x1f`
            // otherwise, which this decoder turns into `Ctrl('7')`. Binding
            // both is what makes the help door `Need::Always` (`0128` §F/§G).
            assert_eq!(intent(Key::Ctrl('7'), *m), Intent::Help, "^7 in {m:?}");
            assert_eq!(intent(Key::Ctrl('l'), *m), Intent::Redraw, "^L in {m:?}");
            assert_eq!(intent(Key::Ctrl('c'), *m), Intent::Quit, "^C in {m:?}");
            let want = if *m == COMPOSER {
                Intent::Compose(Edit::Redo)
            } else {
                Intent::Reload
            };
            assert_eq!(intent(Key::Ctrl('r'), *m), want, "^R in {m:?}");
        }
        // AND THE OLD SPELLINGS ARE DEAD, which is the half a remap can leave
        // behind and which no assertion above would notice.
        for m in BODY_MODES {
            for c in ['u', 'r', 's', 'q', 'm', 'k', 'y', 'd', 'e', '/', '?'] {
                assert_eq!(
                    intent(Key::Alt(c), *m),
                    Intent::None,
                    "opt-{c} still means something in {m:?}"
                );
            }
        }
    }

    /// **THE TWO ACTS, AND THE TEST THAT SAYS THEY ARE TWO.**
    ///
    /// His grammar: *"in the aui we use left to queue, and wildcard to send the
    /// whole payload."* `enter` is the wildcard and `^Q` is the left, and
    /// nothing may collapse them into one. (It was `opt-q`, which macOS
    /// composes into `œ`; `^Q` is letter-for-letter and `^Q` is not XOFF here,
    /// because raw mode clears `IXON` — measured, `0128`.)
    #[test]
    fn queue_and_send_are_two_distinct_reachable_acts() {
        assert_eq!(intent(Key::Enter, COMPOSER), Intent::Send);
        assert_eq!(intent(Key::Ctrl('q'), COMPOSER), Intent::QueueMessage);
        assert_ne!(
            intent(Key::Enter, COMPOSER),
            intent(Key::Ctrl('q'), COMPOSER),
            "the two acts collapsed into one"
        );
        // A NEWLINE is neither of them and is reachable two ways with no
        // protocol at all — measured on a pty: `\x1b\r` is alt-enter and
        // `\n` is ^J. **THE ARROW/NAMED-KEY FAMILY DID NOT MIGRATE**: an arrow
        // and `enter` have no composed glyph, so `opt-enter` always arrived.
        assert_eq!(
            intent(Key::AltEnter, COMPOSER),
            Intent::Compose(Edit::Newline)
        );
        assert_eq!(
            intent(Key::Ctrl('j'), COMPOSER),
            Intent::Compose(Edit::Newline)
        );
    }

    /// **TIER 2 IS AN UPGRADE AND NEVER A DEPENDENCY.** Every act a
    /// negotiated-only key reaches is reachable without it, and the readers
    /// hide what the terminal cannot deliver.
    #[test]
    fn nothing_is_reachable_only_through_a_negotiated_key() {
        let bare = Caps::default();
        let rich = Caps {
            enhanced_keys: true,
            bracketed_paste: true,
            ..Caps::default()
        };
        let reachable = |caps: Caps| -> Vec<Intent> {
            BINDINGS
                .iter()
                .filter(|b| b.mode == COMPOSER && b.needs.met(caps))
                .map(|b| b.intent)
                .collect()
        };
        for i in reachable(rich) {
            assert!(
                reachable(bare).contains(&i),
                "{i:?} is reachable only with enhanced keys — tier 2 became a dependency"
            );
        }
        // …and the OVERLAY hides what is not negotiated, which is the rule that
        // kills the advertised-dead-key class.
        let shown = |caps: Caps| overlay_rows(COMPOSER, caps);
        assert!(
            shown(rich).iter().any(|(k, _)| k.contains("shift-enter")),
            "shift-enter is not offered even when the terminal CAN report it"
        );
        assert!(
            !shown(bare).iter().any(|(k, _)| k.contains("shift-enter")),
            "shift-enter is ADVERTISED on a terminal that cannot report it — \
             that is the single most repeated defect in this whole class of tool"
        );
        assert!(
            !footer_hints(COMPOSER, bare)
                .iter()
                .any(|(k, _)| k.contains("shift")),
            "the footer advertises a key the terminal cannot report"
        );
        // …and says so out loud rather than merely omitting it.
        assert!(withheld(COMPOSER, bare)
            .iter()
            .any(|k| k.contains("shift-enter")));
        assert!(withheld(COMPOSER, rich).is_empty());
    }

    /// **LEFT IS BACK AND RIGHT IS ACTIVATE, all the way down** — the AUI's
    /// `NavAction::Back`/`Select`, mirrored. And Root+Left does NOT exit.
    #[test]
    fn left_is_back_and_right_activates_at_every_depth() {
        assert_eq!(intent(Key::Right, Mode::Projects), Intent::Activate);
        assert_eq!(intent(Key::Right, Mode::List), Intent::Activate);
        // **IN THE COMPOSER THE ARROWS ARE TEXT**, and that is the region
        // model's whole first rule: a cursor in the textarea does not
        // interpret a key binding as anything but text. `←` becomes Back at
        // the BUFFER START, `↑`/`↓` step to the neighbouring band at the
        // first/last visual row — and all three of those edges are decided
        // where the caret is (`App::compose`), not here, because the table
        // cannot see a caret.
        assert_eq!(intent(Key::Right, COMPOSER), Intent::Compose(Edit::Right));
        // **`Intent::Activate` LEFT THE BOX THIS ROUND**, and it is asserted as
        // an absence beside its new home: `^E` is `Compose(RowEnd)` here
        // (readline, in a textarea) and the act lives in the CHAT region, where
        // `^E` is free and where the folded rows are (`0128`).
        assert_eq!(
            intent(Key::Ctrl('e'), COMPOSER),
            Intent::Compose(Edit::RowEnd)
        );
        assert_eq!(intent(Key::Ctrl('e'), CHAT), Intent::Activate);
        assert_eq!(intent(Key::Left, Mode::List), Intent::Back);
        assert_eq!(intent(Key::Left, COMPOSER), Intent::Compose(Edit::Left));
        // **IN THE CHAT, `←` IS BACK OUTRIGHT** — *"And left should go back"* —
        // and `→` is the end of the chat, which is the same act as `opt-↓`
        // past his last message.
        assert_eq!(intent(Key::Left, CHAT), Intent::Back);
        assert_eq!(intent(Key::Right, CHAT), Intent::Bottom);
        assert_eq!(intent(Key::Left, QUEUE), Intent::Back);
        assert_eq!(intent(Key::Left, Mode::Activity), Intent::SwapView);
        // The root never exits on `←`; it lands on the first row, as the AUI's
        // Root does. Quit would be a mistake — an accidental `←` must never
        // end the session.
        assert_eq!(intent(Key::Left, Mode::Projects), Intent::Top);
        assert_ne!(intent(Key::Left, Mode::Projects), Intent::Quit);
        // `enter` and `esc` still work and are NOT in the footer: the
        // unlisted-synonym category `DIR-14` makes possible.
        assert_eq!(intent(Key::Enter, Mode::List), Intent::Activate);
        // **`esc` NO LONGER CARRIES BACK IN A THREAD, AND THIS IS THE LINE
        // THAT SAYS SO.** *"And left should go back. I don't even want ESC to
        // do that behavior."* In the composer it is unbound; in the other
        // regions it means COME HOME TO THE BOX, which is not that behaviour.
        // The back-stack it used to reach is untouched — `←` reaches it now.
        //
        // FORCED: put the `esc` row back in `composer_rows` and the first of
        // these goes red.
        assert_eq!(intent(Key::Esc, COMPOSER), Intent::None);
        assert_eq!(intent(Key::Esc, CHAT), Intent::Region(Region::Composer));
        assert_eq!(intent(Key::Esc, QUEUE), Intent::Region(Region::Composer));
        assert_eq!(intent(Key::Left, Mode::Sheet), Intent::Back);
    }

    /// **EVERY ROW THE OVERLAY PRINTS IS A BINDING OF THE MODE IT WAS RAISED
    /// OVER.** This replaced the shared/delta factoring, and it is the property
    /// that factoring was quietly violating: raised over a thread, the shared
    /// block printed `q quit`, `f filter` and `G newest`, none of which are
    /// bindings in a thread — `q` types a `q` there.
    #[test]
    fn a_modes_overlay_documents_that_mode_and_nothing_else() {
        let caps = Caps::default();
        for m in DOCUMENTED_MODES {
            let rows = overlay_rows(*m, caps);
            assert!(!rows.is_empty(), "{m:?} has no bindings");
            for (label, act) in &rows {
                assert!(
                    BINDINGS.iter().any(|b| b.mode == *m
                        && b.needs.met(caps)
                        && b.keys.label(caps.mods) == *label
                        && b.act == *act),
                    "{m:?}'s overlay shows a row that is not one of its bindings: {label} {act}"
                );
            }
        }
        // …and the thread's grammar really is disjoint from the list modes',
        // which is why printing a common block over it was wrong.
        //
        // **ONE ROW IS COMMON, AND IT IS BACK TO ONE FOR A REASON THIS ROUND
        // HAS TO STATE.** `^L redraw` was always universal. Reload was common
        // too for one round — the list's row and the thread's printed
        // identically once the bare `r` left `common_rows` — and it is NOT any
        // more, because **the composer has no reload key at all**: `^R` is
        // `Compose(Redo)` in the box, its only spelling, so reload is stamped
        // onto the thread's other three regions and not onto this one
        // (`0128`'s mode-scoped resolution). The `?` overlay renders per mode,
        // so it documents the absence for free rather than promising a key.
        //
        // The assertion NAMES the row rather than counting, because a bound of
        // one would also pass if `^L` vanished and something unrelated
        // collided instead.
        let thread: Vec<_> = overlay_rows(COMPOSER, caps);
        let list: Vec<_> = overlay_rows(Mode::List, caps);
        let common: Vec<&str> = thread
            .iter()
            .filter(|r| list.contains(r))
            .map(|(_, act)| *act)
            .collect();
        assert_eq!(
            common,
            vec!["repaint the whole screen"],
            "the thread and the list share a row they were not supposed to"
        );
        // …and reload IS common with the three regions that kept it, which is
        // the half a shrinking list could otherwise hide.
        for r in [Region::Chat, Region::Queue, Region::Agents] {
            let rows = overlay_rows(Mode::Thread(r), caps);
            assert!(
                rows.iter().any(|(k, a)| k == "^R"
                    && *a == "re-ask the daemon for everything this view shows"),
                "reload lost its key in {r:?} as well as in the box"
            );
        }
    }

    /// **SELECT MODE AND `M` ARE CUT, AND THIS IS THE TEST THAT SAYS SO.**
    /// Six keys plus a toggle, all of which existed only because the face
    /// could not drag-select. A later edit that brings one back has to delete
    /// a line here that says why it went.
    #[test]
    fn select_mode_and_the_mouse_toggle_are_gone_from_every_mode() {
        for m in [Mode::Projects, Mode::List, Mode::Activity] {
            assert_eq!(
                intent(Key::Char('M'), m),
                Intent::None,
                "the mouse toggle survived in {m:?} — there is nothing to toggle"
            );
            assert_eq!(
                intent(Key::Char('v'), m),
                Intent::None,
                "select mode's door survived in {m:?}"
            );
            assert!(
                !overlay_rows(m, Caps::default())
                    .iter()
                    .any(|(k, _)| k.contains('M')),
                "the `?` overlay still documents a mouse key that does nothing"
            );
        }
        // In a THREAD they are letters, which is the composer's answer and not
        // a resurrection: `Intent::Type('v')` types a `v`.
        assert_eq!(intent(Key::Char('v'), COMPOSER), Intent::Type('v'));
        assert_eq!(intent(Key::Char('y'), COMPOSER), Intent::Type('y'));
        // …and the mode itself cannot be named: `Mode` has no `Select`, so
        // this file would not compile if one came back by accident.
        // NINE since the sheet landed: Projects, List, Thread, Mic, Activity,
        // Filter, Queue, Sheet, SheetField. Help is still excluded — its whole
        // grammar is "any key closes this".
        //
        // **THE SHEET IS TWO MODES AND THAT IS THE POINT.** `Sheet` walks act
        // rows and `SheetField` is a textarea; in the field a letter is TEXT,
        // so `c` types a `c` rather than compacting. Documenting them
        // separately is the one-table rule doing its job — an overlay that
        // printed the menu grammar over a live textarea is exactly the drift
        // `0042` caught.
        //
        // **ELEVEN SINCE `0063`.** `Archived` is the room behind the root's
        // counted row — a body mode with four keys and nothing else to press —
        // and `NewProject` is the root with a LIVE FIELD in one of its rows,
        // where `?` types a `?` instead of raising this very overlay. That
        // second one is the same argument as `SheetField`'s, one surface over,
        // and it is why the field could not be a flag on `Mode::Projects`.
        // **FOURTEEN SINCE `0100`.** The thread became FOUR entries — one per
        // region of its screen that has a grammar — because the arrows mean
        // different things in each and the `?` overlay renders from the table.
        // — one per region of its screen — and the fourth, `Agents`, carries
        // only the half of its grammar that is built, because its band is the
        // next slice.
        // **FIFTEEN AFTER ROUND SIX, and the arithmetic is the round in one
        // line:** `Mode::Filter` went (`0127` — *"filter is too much. Just drop
        // that feature."*), `Mode::SeeAll(b)` arrived (`0135`, a band's own
        // page) and `Mode::RenameModal` arrived (`0130`, the little modal).
        // 14 - 1 + 1 + 1 = 15.
        //
        // **THREE BRANCHES MOVED THIS NUMBER IN THREE DIRECTIONS** and no
        // worker could see the others: HEAD asserted 13, `r6-seeall` asserted
        // 15, and NEITHER was right — each counted its own change against a
        // base the other had already moved. The manager resolved it from the
        // LIST rather than by taking a side. That is what a hardcoded count is
        // for, and it earned its keep this round.
        assert_eq!(
            DOCUMENTED_MODES.len(),
            15,
            "the documented mode set changed"
        );
    }


    /// **`←` AND `esc` LEAVE A BAND'S PAGE, WHICHEVER BAND IT IS, AND `←` AT THE
    /// ROOT STILL DOES NOT.** `I-13`'s first half, and it is the one row that
    /// makes the page a `Mode` rather than a flag on `Mode::Projects`.
    ///
    /// > Going left from there goes back to the main menu.
    ///
    /// The root's `←` is [`Intent::Top`] — first row, never exits — and the
    /// page's is [`Intent::Back`]. A flag could not express that, and the `?`
    /// overlay and the footer both render FROM this table, so a flag would have
    /// left the legend printing "the first row of the menu" over a key that
    /// leaves the screen: the drift `0042` caught.
    ///
    /// FORCED RED by binding the page's `←` to `Intent::Top` (the first two
    /// assertions), and by deleting the page's `Key::Esc` (the third).
    #[test]
    fn left_leaves_a_bands_page_and_still_does_not_leave_the_root() {
        for b in crate::model::BandId::ALL {
            assert_eq!(
                intent(Key::Left, Mode::SeeAll(b)),
                Intent::Back,
                "← does not leave {b:?}'s page"
            );
            assert_eq!(
                intent(Key::Esc, Mode::SeeAll(b)),
                Intent::Back,
                "esc does not leave {b:?}'s page"
            );
        }
        assert_eq!(
            intent(Key::Left, Mode::Projects),
            Intent::Top,
            "← started exiting the root"
        );
        assert_eq!(
            intent(Key::Esc, Mode::Projects),
            Intent::None,
            "esc grew a meaning at the root"
        );
    }

    /// **EVERY ROW ACT MEANS THE SAME THING ON THE ROOT AND ON THE PAGE.**
    /// `I-13`'s second half, and the answer to `0133`'s stated fear: *"a page
    /// where `r` silently did nothing."*
    ///
    /// It is asserted over [`root_acts`]'s own key set read off the TABLE rather
    /// than off a list written here — so the day a tenth row act is added to the
    /// macro, this covers it without anybody remembering to. A list would be the
    /// thing that went stale.
    ///
    /// **AND THE TWO ABSENCES ARE ASSERTED TOO.** The band jump has nowhere to
    /// go on a one-band page, and a key bound to a guaranteed no-op with a
    /// legend row printed over it from this very table is a lie about what the
    /// surface answers to.
    ///
    /// FORCED RED by writing the page's block out by hand instead of stamping
    /// `root_acts!` and dropping one row from it (`r`, say) — the loop names the
    /// key that went missing. Watched.
    #[test]
    fn the_page_answers_to_every_one_of_the_roots_row_acts() {
        const PAGE: Mode = Mode::SeeAll(crate::model::BandId::Recent);
        let acts: Vec<(Key, Intent)> = BINDINGS
            .iter()
            .filter(|b| b.mode == Mode::Projects)
            .filter_map(|b| match b.keys {
                KeySet::These(ks) => ks.first().map(|k| (*k, b.intent)),
                _ => None,
            })
            // `←` is the one row that differs; the band jump and the
            // new-thread key are root-only.
            //
            // **`n` IS NOT A ROW ACT AND THAT IS WHY IT IS EXCLUDED HERE.**
            // Every member of `root_acts!` acts on whatever the cursor is
            // standing on, which is exactly why the page must answer to all of
            // them — a thread row is a thread row wherever it is drawn.
            // `Intent::NewThread` acts on NOTHING, and his ask names the
            // surface: *"it should work on the main page"*. A `see all` page is
            // one band's own surface, not the menu, so the key is bound on
            // `Mode::Projects` alone rather than through the macro.
            .filter(|(k, i)| {
                !matches!(k, Key::Left)
                    && !matches!(
                        i,
                        Intent::BandPrev | Intent::BandNext | Intent::NewThread
                    )
            })
            .collect();
        // **NINETEEN AFTER ROUND SIX, and the number moved for a reason rather
        // than by drift.** This is a VACUITY guard: without it, a `filter` that
        // returned nothing would make every assertion below pass over an empty
        // list and the test would defend nothing. It was `>= 20` when `0135`
        // wrote it against a base where the root still had `f` (the lane
        // filter), `g` and `G` (jump to top/bottom) and the stdout toggle. All
        // four left the root in the same round — `0127` deleted the filter
        // whole, retired `g`/`G` in every spelling, and moved the toggle into
        // the thread context where he asked for it — so the root's grammar is
        // legitimately three rows shorter and the old floor could not be met.
        //
        // Kept EXACT rather than loosened: the next removal from the root
        // should trip this and be read, which is the same bargain the
        // `DOCUMENTED_MODES` count makes.
        assert!(
            acts.len() >= 19,
            "the root's grammar came back too small to be the root's: {acts:?}"
        );
        let mut seen_row_acts = 0;
        for (k, want) in acts {
            assert_eq!(
                intent(k, PAGE), want,
                "{k:?} means {:?} on the root and {:?} on the page",
                want,
                intent(k, PAGE)
            );
            if matches!(
                want,
                Intent::Activate
                    | Intent::Options
                    | Intent::Rename
                    | Intent::AddProject
                    | Intent::DeregisterProject
                    | Intent::MovePinUp
                    | Intent::MovePinDown
                    | Intent::SwapView
            ) {
                seen_row_acts += 1;
            }
        }
        assert_eq!(
            seen_row_acts, 8,
            "`root_acts!` no longer stamps the eight row acts this asserts"
        );

        // THE ABSENCES: the band jump is not bound, and the overlay does not
        // print a row for it either — which is the half that matters, because
        // the legend renders FROM this table.
        for k in [Key::AltUp, Key::AltDown] {
            assert_eq!(
                intent(k, PAGE),
                Intent::None,
                "{k:?} is bound on a page with one band"
            );
        }
        let legend = overlay_rows(PAGE, Caps::default());
        assert!(
            !legend.iter().any(|(_, act)| act.contains("band header")),
            "the page's overlay advertises a band jump it does not answer to"
        );
        // …and it DOES advertise the way home, in words.
        assert!(
            legend.iter().any(|(_, act)| *act == "back to the main menu"),
            "the page's overlay does not name the way back: {legend:?}"
        );
    }

    /// **ONE TABLE FOR ALL THREE PAGES** — [`Mode::table_key`]'s whole contract.
    ///
    /// The rows are written under a representative band, so the collapse itself
    /// is the thing that has to be checked: every reader of the table must answer
    /// identically for `SeeAll(Pinned)`, `SeeAll(Recent)` and
    /// `SeeAll(Projects)`, and must NOT collapse anything else — a payload that
    /// genuinely changed the grammar has to be its own rows, which is what
    /// `Mode::Thread(Region)` is and why it is four entries in
    /// `DOCUMENTED_MODES`.
    ///
    /// FORCED RED by returning `self` from `table_key` (the pinned and projects
    /// pages answer `Intent::None` to everything), and by collapsing
    /// `Mode::Thread(_)` there too (the second block's four regions stop
    /// differing and the `↑` assertion fires).
    #[test]
    fn the_page_reads_one_table_whichever_band_it_is() {
        let caps = Caps::default();
        let keys = [
            Key::Left,
            Key::Esc,
            Key::Right,
            Key::Enter,
            Key::Tab,
            Key::Char('o'),
            Key::Char('r'),
            Key::Char('p'),
            Key::Char('c'),
            Key::CtrlUp,
            Key::CtrlDown,
            Key::Up,
            Key::Down,
            Key::Char('?'),
            Key::Char('q'),
        ];
        for b in crate::model::BandId::ALL {
            for k in keys {
                assert_eq!(
                    intent(k, Mode::SeeAll(b)),
                    intent(k, Mode::SeeAll(crate::model::BandId::Recent)),
                    "{k:?} means different things on {b:?}'s page"
                );
            }
            assert_eq!(
                overlay_rows(Mode::SeeAll(b), caps),
                overlay_rows(Mode::SeeAll(crate::model::BandId::Recent), caps),
                "{b:?}'s page prints a different legend"
            );
            assert_eq!(
                footer_hints(Mode::SeeAll(b), caps),
                footer_hints(Mode::SeeAll(crate::model::BandId::Recent), caps),
                "{b:?}'s page prints a different footer"
            );
            assert!(
                !overlay_rows(Mode::SeeAll(b), caps).is_empty(),
                "{b:?}'s page has no grammar at all"
            );
        }
        // **AND NOTHING ELSE IS COLLAPSED.** The thread's regions are four
        // entries because the arrows mean four different things; if `table_key`
        // ever grew to them, `↑` in the chat and `↑` in the composer would
        // silently become one row.
        assert_ne!(
            intent(Key::Up, Mode::Thread(Region::Chat)),
            intent(Key::Up, Mode::Thread(Region::Composer)),
            "the thread's regions were collapsed into one row-set"
        );
        for m in DOCUMENTED_MODES {
            if matches!(m, Mode::SeeAll(_)) {
                continue;
            }
            assert_eq!(m.table_key(), *m, "{m:?} is no longer its own row-set");
        }
    }

    #[test]
    fn an_unbound_key_is_none_not_a_panic() {
        assert_eq!(intent(Key::Char('%'), Mode::List), Intent::None);
    }

    #[test]
    fn any_key_dismisses_an_overlay_but_ctrl_c_still_quits() {
        assert_eq!(intent(Key::Char('%'), Mode::Help), Intent::Back);
        assert_eq!(intent(Key::Ctrl('c'), Mode::Help), Intent::Quit);
        assert_eq!(intent(Key::Ctrl('c'), Mode::Queue), Intent::Quit);
    }

    /// **NO DIGIT MEANS ANYTHING ANY MORE, AND NO ROW NAMES THE FILTER.**
    ///
    /// This is the same test slot that used to hold
    /// `a_lane_key_carries_its_own_lane_through_the_table` — the lane row was
    /// ONE binding standing for nine, and it is gone with everything else the
    /// filter owned. *"filter is too much. Just drop that feature."*
    ///
    /// **IT ASSERTS THE TABLE IS POSITIVELY RIGHT BEFORE IT ASSERTS AN
    /// ABSENCE.** A `BINDINGS` that failed to build, or a `Mode` whose rows all
    /// vanished, would satisfy "no row says filter" triumphantly — that is the
    /// pass-on-absence failure `0065` measured. So the witness comes first and
    /// it is keyed on rows this round did not touch: `?` raises the help
    /// overlay and `^C` quits, in every body mode.
    ///
    /// FORCED: put a `Binding` with `act: "filter which event lanes are
    /// shown"` back into `common_rows` and the second half goes red; break
    /// `BINDINGS` and the witness goes red first.
    #[test]
    fn no_digit_is_bound_and_no_row_names_the_filter() {
        // ── THE WITNESS ────────────────────────────────────────────────────
        //
        // `^/` and `^C`, not `?`: in a thread every printable key is TEXT, so
        // `?` types a `?` there. The point of a witness is that no mutation of
        // THIS round can move it, and the uniform modified grammar is that —
        // the key it names is `^/` since the ctrl migration, for the reason
        // `every_global_answers_to_the_same_ctrl_key_in_every_body_mode`
        // states: `opt-/` is `÷` on his keyboard and never arrived.
        for m in BODY_MODES {
            assert_eq!(intent(Key::Ctrl('/'), *m), Intent::Help, "witness: {m:?}");
            assert_eq!(intent(Key::Ctrl('c'), *m), Intent::Quit, "witness: {m:?}");
        }
        assert!(BINDINGS.len() > 100, "the table did not build: {}", BINDINGS.len());

        // ── AND ONLY THEN THE ABSENCE ──────────────────────────────────────
        for d in '0'..='9' {
            for m in [Mode::Projects, Mode::List, Mode::Activity] {
                assert_eq!(
                    intent(Key::Char(d), m),
                    Intent::None,
                    "`{d}` still means something in {m:?}"
                );
            }
        }
        // **`lane` IS NOT IN THIS LIST AND THAT IS NOT AN OVERSIGHT.** The
        // word is overloaded in this codebase and the overload is exactly what
        // made the filter's own doc comment wrong: the FLEET'S EVENT LANE
        // (`Band::EventLane`, `Block::Lane`, `Mode::Activity`) is a live
        // surface with nothing to do with filtering, and `tab` legitimately
        // says "switch to the live event lane". The filter's vocabulary was
        // `filter` and the nine GROUP names, of which only `speech` ever
        // reached a key-table row.
        for b in BINDINGS {
            for word in ["filter", "speech"] {
                assert!(
                    !b.act.contains(word) && !b.short.contains(word),
                    "a key-table row still says `{word}`: {:?} / {:?}",
                    b.act,
                    b.short
                );
            }
        }
    }

    /// **NO NON-THREAD MODE NAMES A THREAD-SCOPED ACT** — the mechanical half
    /// of Scope 1b, and the answer to *"is anything ELSE leaking?"* given as a
    /// check rather than an eyeball.
    ///
    /// The defect it exists for: *"I'm seeing speech on the menu and don't know
    /// what it's for in that context. It should be in the thread context."*
    /// The toggle was in `common_rows`, a macro with no notion of scope that is
    /// stamped onto the root, the list and the event lane in one go — so the
    /// act was published to the root BY CONSTRUCTION, at footer rank 2.
    ///
    /// **THE SWEEP'S RESULT IS THIS TEST PASSING.** It walks every row of every
    /// non-thread mode, not just the one he noticed, so a second leak of the
    /// same shape cannot be hiding behind the one that was fixed.
    ///
    /// `Mode::SheetField`, `Mode::NewProject` and `Mode::RenameModal` are
    /// EXCLUDED and the reason is not a loophole: they are typing fields with
    /// buffers of their own, so `Compose`/`Type` there act on THAT field and
    /// not on a thread's composer.
    ///
    /// **`Mode::RenameModal` was added AT MERGE TIME by the manager**, not by
    /// the worker that wrote this test. The modal (`0130`) and this sweep
    /// (`0127`) were built in parallel worktrees and neither could see the
    /// other: each branch was green alone and the pair was RED together, which
    /// is the only composition regression round six produced. The exclusion
    /// applies this test's OWN stated rule to a third typing field rather than
    /// widening it — if a future mode carries a buffer of its own it belongs
    /// here too, and if it does not, it does not.
    /// The check is over the BODY modes that are not a thread, which is exactly
    /// the set `common_rows` is stamped onto, plus the overlays that are not
    /// about a thread.
    ///
    /// FORCED: put the `^S` row back into `common_rows` and this goes red
    /// naming `Mode::Projects`, `Mode::List` and `Mode::Activity`.
    #[test]
    fn no_non_thread_mode_names_a_thread_scoped_act() {
        let offenders: Vec<_> = BINDINGS
            .iter()
            .filter(|b| !matches!(b.mode, Mode::Thread(_) | Mode::Mic))
            .filter(|b| !matches!(b.mode, Mode::SheetField | Mode::NewProject | Mode::RenameModal))
            .filter(|b| b.intent.is_thread_scoped())
            .map(|b| (b.mode, b.intent, b.short))
            .collect();
        assert!(
            offenders.is_empty(),
            "a thread-scoped act is printed on a surface that is not a thread: {offenders:?}"
        );
        // **AND THE PREDICATE IS NOT VACUOUS.** A `is_thread_scoped` that
        // returned `false` for everything would make the sweep above a
        // triumphant no-op, which is the same absence-as-evidence failure the
        // witness above guards against.
        assert!(Intent::StdoutOnly.is_thread_scoped());
        assert!(Intent::Send.is_thread_scoped());
        assert!(!Intent::Help.is_thread_scoped());
        assert!(!Intent::Rename.is_thread_scoped());
        assert!(
            BINDINGS
                .iter()
                .any(|b| matches!(b.mode, Mode::Thread(_)) && b.intent.is_thread_scoped()),
            "no thread row is thread-scoped, so the sweep proves nothing"
        );
    }

    /// **`g` AND `G` ARE RETIRED, AND THE SURVIVORS STILL WORK.**
    ///
    /// *"G newest isn't accurate. Idk what it's supposed to do but g and G are
    /// jumping to the top and bottom. We don't need that because we have
    /// opt-{up,down}"* — two findings in one sentence: the LABEL was wrong and
    /// the function was redundant. Neither key was relabelled, because a
    /// correctly-labelled redundant key is still a key he does not want.
    ///
    /// FORCED: put `Key::Char('g')` back in `scroll_rows`' first row and the
    /// first assertion goes red; delete `Key::Home` from the same row and the
    /// third does.
    #[test]
    fn g_and_capital_g_are_retired_and_the_jump_keys_survive() {
        for m in [Mode::Projects, Mode::List, Mode::Activity] {
            assert_eq!(intent(Key::Char('g'), m), Intent::None, "`g` in {m:?}");
            assert_eq!(intent(Key::Char('G'), m), Intent::None, "`G` in {m:?}");
            // The survivors: not letters, and nothing he said was about them.
            assert_eq!(intent(Key::Home, m), Intent::Top, "home in {m:?}");
            assert_eq!(intent(Key::End, m), Intent::Bottom, "end in {m:?}");
        }
        // **AND THE TWO OTHER SPELLINGS OF THE SAME FUNCTION.** The audit
        // (`0128`) found `opt-g` (Top) and `opt-n` (Bottom) live in the
        // thread's composer and chat rows — different rows from the plain
        // letters, and named by nothing in the original ask. A redundant
        // FUNCTION retires in every spelling it has; leaving one would be the
        // relabel-instead-of-retire failure wearing a modifier.
        for m in [COMPOSER, CHAT] {
            assert_ne!(intent(Key::Alt('g'), m), Intent::Top, "opt-g in {m:?}");
            assert_ne!(intent(Key::Alt('n'), m), Intent::Bottom, "opt-n in {m:?}");
        }
        // `→` in the chat is UNTOUCHED — it was the same row `alt-n` sat in,
        // and it is how the end of the chat is reached.
        assert_eq!(intent(Key::Right, CHAT), Intent::Bottom);

        // **AND `opt-↑`/`opt-↓` — the pair he named as the reason — still
        // produce their three region-resolved intents.** `0134` is migrating
        // the SPELLING to `ctrl-*`; the bindings are what this asserts.
        assert_eq!(intent(Key::AltUp, Mode::Projects), Intent::BandPrev);
        assert_eq!(intent(Key::AltDown, Mode::Projects), Intent::BandNext);
        assert_eq!(intent(Key::AltUp, CHAT), Intent::JumpUserPrev);
        assert_eq!(intent(Key::AltDown, CHAT), Intent::JumpUserNext);
        assert_eq!(intent(Key::AltUp, COMPOSER), Intent::Compose(Edit::ParaUp));
        assert_eq!(
            intent(Key::AltDown, COMPOSER),
            Intent::Compose(Edit::ParaDown)
        );
        // **NO LEGEND ROW SAYS `g` OR `G` EITHER**, because the table IS the
        // legend: the footer and the `?` overlay both render from it.
        // `KeySet::Printable` is EXCLUDED and must be: it is the composer's
        // "any letter is text" row, so of course it contains `g`. Typing a `g`
        // is not a binding on `g`. `These` is what the letters live in.
        for b in BINDINGS {
            if !matches!(b.keys, KeySet::These(_)) {
                continue;
            }
            assert!(
                !b.keys.contains(Key::Char('g')) && !b.keys.contains(Key::Char('G')),
                "a row still answers to g/G: {:?} in {:?}",
                b.short,
                b.mode
            );
        }
    }

    /// **NO KEY MEANS TWO THINGS IN ONE MODE** — the machine two comments in
    /// this file have been citing by name since `o` was bound, and which did
    /// not exist until the speech toggle needed it.
    ///
    /// It walks the `These` rows only. `Any` and `Printable` are
    /// deliberate catch-alls that sit LAST in their mode and are arbitrated by
    /// first-match-wins; folding them in here would report the design as a
    /// collision. Everything else is a hand-written key and a second row
    /// claiming it is a binding somebody silently lost.
    ///
    /// FORCED: bind the speech toggle to `f` instead of `s` and this goes red
    /// naming `Char('f')` in three modes.
    #[test]
    fn one_key_one_meaning_in_one_mode() {
        for m in DOCUMENTED_MODES {
            let mut seen: Vec<(Key, Intent)> = Vec::new();
            for b in BINDINGS.iter().filter(|b| b.mode == *m) {
                if let KeySet::These(keys) = b.keys {
                    for &k in keys {
                        if let Some((_, other)) = seen.iter().find(|(s, _)| *s == k) {
                            panic!(
                                "{k:?} means two things in {m:?}: {other:?} and {:?}",
                                b.intent
                            );
                        }
                        seen.push((k, b.intent));
                    }
                }
            }
        }
    }

    /// **ROUND THREE'S `r` RULING, AS A MACHINE.**
    ///
    /// > On any thread throughout the application, I need to be able to rename
    /// > with a tap of the letter r.
    ///
    /// The ruling resolved the collision with `Reload` BY CONTEXT: bare `r` is
    /// RENAME where a thread row can be under the cursor, and `^R` is
    /// RELOAD everywhere, unchanged. Both halves are asserted, and the second
    /// one matters as much as the first — reload keeps the form that both
    /// readers of this table PRINT, so nothing documented moved.
    ///
    /// `Mode::Activity` is the third case and it is not an oversight: the
    /// event lane has no thread row, so the act `r` would reach is not
    /// expressible there and the letter stays with reload.
    ///
    /// **AND THE OVERLAY TELLS THE TRUTH**, which is the half `0042` caught
    /// drifting once already. A context-sensitive binding the table could not
    /// express would leave the `?` overlay documenting a grammar the surface
    /// does not answer to; this one is expressed as three ordinary rows, so
    /// the overlay renders it for free and this test reads it back.
    ///
    /// FORCED: give the two rename rows `Key::Alt('r')` as well and
    /// `one_key_one_meaning_in_one_mode` goes red first; put the bare `r` back
    /// in `common_rows` and the `Mode::Projects` assertion goes red; delete
    /// the `Mode::Activity` row and reload becomes unreachable there.
    #[test]
    fn bare_r_renames_on_a_thread_surface_and_ctrl_r_is_still_reload() {
        let caps = Caps::default();
        // The two modes a thread row can be under the cursor in.
        for m in [Mode::Projects, Mode::List] {
            assert_eq!(
                intent(Key::Char('r'), m),
                Intent::Rename,
                "bare r does not rename in {m:?} — he still cannot rename a thread"
            );
            assert!(
                overlay_rows(m, caps)
                    .iter()
                    .any(|(k, a)| k == "r" && a.contains("rename")),
                "the `?` overlay does not document rename in {m:?}"
            );
        }
        // The event lane has no thread row; the letter stays with reload.
        assert_eq!(intent(Key::Char('r'), Mode::Activity), Intent::Reload);
        // **THE OTHER HALF OF THE RULING, AND THE ROUND-SIX CORRECTION TO
        // IT.** The ruling said the modified spelling is reload EVERYWHERE, so
        // taking the bare letter cost reload nothing. That is still true of
        // every mode with a list in it — the spelling is `^R` now — and it is
        // NOT true of the textarea, where `^R` is `Compose(Redo)`. The loop
        // therefore asserts reload where it lives and its ABSENCE where it
        // does not, rather than skipping the awkward mode.
        for m in [
            Mode::Projects,
            Mode::List,
            Mode::Activity,
            CHAT,
            QUEUE,
            Mode::Thread(Region::Agents),
        ] {
            assert_eq!(
                intent(Key::Ctrl('r'), m),
                Intent::Reload,
                "^R stopped reloading in {m:?}"
            );
            assert!(
                overlay_rows(m, caps)
                    .iter()
                    .any(|(k, a)| k.contains("^R") && a.contains("re-ask the daemon")),
                "the `?` overlay stopped documenting reload in {m:?}"
            );
        }
        // **AND THE COST, AS A TEST RATHER THAN A COMMENT:** inside the box
        // `^R` is redo and NOTHING is reload, so the overlay must not claim one.
        assert_eq!(
            intent(Key::Ctrl('r'), COMPOSER),
            Intent::Compose(Edit::Redo)
        );
        assert!(
            !overlay_rows(COMPOSER, caps)
                .iter()
                .any(|(_, a)| a.contains("re-ask the daemon")),
            "the composer's overlay promises a reload key it does not have"
        );
        // In a thread every printable key is TEXT, so `r` types an `r` — the
        // rename door there is the sheet, reached from a list.
        assert_eq!(intent(Key::Char('r'), COMPOSER), Intent::Type('r'));
    }

    /// **THE ONE-KEY COLLAPSE IS ONE KEY — IN A THREAD, AND NOWHERE ELSE.**
    ///
    /// Two halves, and this round moved the second one. His ask was one toggle
    /// that hides everything except what was said, and *"a toggle he cannot
    /// find in the legend is not delivered"* — so `^S` must reach it from
    /// wherever he is standing INSIDE A THREAD, including the region where
    /// every letter is text, and both readers of the table must PRINT it.
    ///
    /// And it must reach it from nowhere else: *"I'm seeing speech on the menu
    /// and don't know what it's for in that context. It should be in the thread
    /// context."*
    ///
    /// **NONE OF THE FILTER'S VOCABULARY SURVIVES IN WHAT IS PRINTED.** The
    /// word is `stdout`, which is his word, and the assertions check for it
    /// rather than for the absence of `speech` — an absence would pass on an
    /// empty legend.
    ///
    /// FORCED: case-shift the `^S` in `alt_common_rows` and the first
    /// assertion goes red — the key no longer reaches the act; set that row's
    /// `footer_rank` to `None` and the footer assertion goes red instead, with
    /// the binding still perfectly reachable. Two halves of "delivered", each
    /// with its own mutation. Copy the row into `common_rows` and the
    /// nowhere-else half goes red.
    #[test]
    fn the_stdout_toggle_is_one_key_in_a_thread_and_absent_everywhere_else() {
        let caps = Caps::default();
        for m in [COMPOSER, CHAT, QUEUE, Mode::Thread(Region::Agents)] {
            // `^S` — letter-for-letter from `opt-s`, and `^S` is not XOFF in
            // this face because raw mode clears `IXON` (measured, `0128`).
            assert_eq!(
                intent(Key::Ctrl('s'), m),
                Intent::StdoutOnly,
                "^S does not collapse to stdout in {m:?}"
            );
            assert_eq!(
                intent(Key::Alt('s'), m),
                Intent::None,
                "opt-s still reaches the toggle in {m:?} — the old key outlived the remap"
            );
            assert!(
                overlay_rows(m, caps)
                    .iter()
                    .any(|(k, a)| k.contains("^S") && a.contains("what was said")),
                "the `?` overlay does not document the stdout toggle in {m:?}"
            );
            assert!(
                footer_hints(m, caps)
                    .iter()
                    .any(|(_, short)| *short == "stdout"),
                "the footer never offers the stdout toggle in {m:?}"
            );
        }
        // **AND IT IS A LETTER AGAIN ON THE SURFACES IT LEFT.** The bare `s`
        // was a synonym in the three modes with nothing to type into; both
        // spellings are gone from all three, and `s` in the composer is still
        // an `s`.
        for m in [Mode::Projects, Mode::List, Mode::Activity] {
            assert_eq!(intent(Key::Char('s'), m), Intent::None, "bare s in {m:?}");
            assert_eq!(intent(Key::Alt('s'), m), Intent::None, "^S in {m:?}");
            assert!(
                !footer_hints(m, caps)
                    .iter()
                    .any(|(_, short)| *short == "stdout" || *short == "speech"),
                "{m:?} still prints the toggle in its footer"
            );
        }
        assert_eq!(intent(Key::Char('s'), COMPOSER), Intent::Type('s'));
        // **AND IT DISPLACED NOTHING.** The letters this arc promised him, and
        // the globals that were here first, all still mean what they meant.
        assert_eq!(intent(Key::Char('u'), Mode::List), Intent::QueueOverlay);
        assert_eq!(intent(Key::Char('r'), Mode::List), Intent::Rename);
        assert_eq!(intent(Key::Char('o'), Mode::List), Intent::Options);
        assert_eq!(intent(Key::Char('q'), Mode::List), Intent::Quit);
        assert_eq!(intent(Key::Char('?'), Mode::List), Intent::Help);
        assert_eq!(intent(Key::Ctrl('q'), COMPOSER), Intent::QueueMessage);
        // **`f` IS A FREE LETTER AGAIN**, on every surface — and it stayed free
        // through the ctrl migration, which took `^S ^O ^R ^Q ^E ^K ^V ^/ ^7`
        // and left `^F` alone.
        for m in BODY_MODES {
            assert_eq!(intent(Key::Alt('f'), *m), Intent::None, "opt-f in {m:?}");
            assert_eq!(intent(Key::Ctrl('f'), *m), Intent::None, "^F in {m:?}");
        }
        assert_eq!(intent(Key::Char('f'), Mode::List), Intent::None);
        assert_eq!(intent(Key::Char('f'), COMPOSER), Intent::Type('f'));
    }

    /// **RENAME AND PIN: WHAT THE LEGEND OWES HIM, AND WHAT IT HONESTLY
    /// CANNOT.**
    ///
    /// *"rename and pin keybindings aren't shown and they're the most
    /// important."* `r` is bound on both surfaces where a thread row can be
    /// under the cursor and was missing from the FOOTER (it was already in the
    /// `?` overlay); this asserts both readers now carry it.
    ///
    /// **AND `p` IS NOT A PIN KEY IN A THREAD-ROW SENSE**, which this test
    /// STATES rather than papers over: `p` on the root is
    /// `Intent::AddProject`, whose shell arm pins when the cursor is on a
    /// thread row and opens the add-project field otherwise. There is no `p` on
    /// `Mode::List` at all. Nothing is fabricated here — a pin row in the
    /// legend that resolved to no act would be a stub that pretends to work.
    ///
    /// FORCED: set either rename row's `footer_rank` back to `None` and the
    /// footer assertion goes red on that mode.
    #[test]
    fn rename_is_in_both_legends_and_pin_is_only_where_it_really_is() {
        let caps = Caps::default();
        for m in [Mode::Projects, Mode::List] {
            assert_eq!(intent(Key::Char('r'), m), Intent::Rename, "r in {m:?}");
            assert!(
                footer_hints(m, caps)
                    .iter()
                    .any(|(_, short)| *short == "rename"),
                "the footer does not name rename in {m:?}"
            );
            assert!(
                overlay_rows(m, caps)
                    .iter()
                    .any(|(k, a)| k.contains('r') && a.contains("rename")),
                "the `?` overlay does not name rename in {m:?}"
            );
        }
        // `p` is exactly where it is, and it is not a thread-row pin key on
        // the list at all.
        assert_eq!(intent(Key::Char('p'), Mode::Projects), Intent::AddProject);
        assert_eq!(intent(Key::Char('p'), Mode::List), Intent::None);
    }

    /// **`n` STARTS A THREAD ON THE MAIN PAGE, AND NOWHERE ELSE.**
    ///
    /// *"ok and where is the n for new thread. It should work on the main page
    /// and create a misc thread. I need that asap."* `n` reached
    /// [`Intent::None`] in every mode before this; the act existed in no table
    /// at all, which is why three rounds of tuning shipped over it.
    ///
    /// Four claims, and each is a separate mutation to force red:
    ///
    ///  1. the key reaches the act on the ROOT — change the binding's letter;
    ///  2. it is in the `?` overlay — delete the row;
    ///  3. it is in the FOOTER — set `footer_rank` to `None`;
    ///  4. it is bound NOWHERE else, and in the composer `n` is still a
    ///     letter — stamp the row into `root_acts!` or `common_rows!` and the
    ///     loop goes red on `Mode::List`, `Mode::Activity` and the page.
    #[test]
    fn n_starts_a_thread_on_the_main_page_and_is_a_letter_everywhere_else() {
        let caps = Caps::default();
        assert_eq!(
            intent(Key::Char('n'), Mode::Projects),
            Intent::NewThread,
            "`n` does not start a thread at the root — the key he asked for"
        );
        assert!(
            overlay_rows(Mode::Projects, caps)
                .iter()
                .any(|(k, a)| k.contains('n') && a.contains("new thread")),
            "the `?` overlay does not document the new-thread key"
        );
        assert!(
            footer_hints(Mode::Projects, caps)
                .iter()
                .any(|(_, short)| *short == "new thread"),
            "the footer never offers the new-thread key"
        );
        // **THE MAIN PAGE AND NOT THE `see all` PAGE**, which is what keeps
        // this out of `root_acts!`: that macro is stamped on both.
        // …and inside a project, where he asked for it too (TUI-P1a, TC-01).
        assert_eq!(
            intent(Key::Char('n'), Mode::List),
            Intent::NewThread,
            "`n` inside a project does nothing — *\"I am inside of a project and clicking n and nothing happens\"*"
        );
        assert_eq!(intent(Key::Char('n'), Mode::RenameModal), Intent::Type('n'), "`n` in the name box is a letter");
        for m in [
            Mode::Activity,
            Mode::SeeAll(crate::model::BandId::Recent),
            Mode::SeeAll(crate::model::BandId::Pinned),
        ] {
            assert_eq!(
                intent(Key::Char('n'), m),
                Intent::None,
                "`n` reaches the new-thread act in {m:?} — it is the main page's"
            );
        }
        // And in a box, `n` is an `n`. The one thing a bare letter must never
        // cost him.
        assert_eq!(intent(Key::Char('n'), COMPOSER), Intent::Type('n'));
        assert_eq!(intent(Key::Char('n'), Mode::NewProject), Intent::Type('n'));
        // It displaced nothing: `^N` is still a cursor move at the root.
        assert_eq!(intent(Key::Ctrl('n'), Mode::Projects), Intent::ScrollDown);
    }

    /// **ONE KEY PAIR, THREE MEANINGS, AND NOT ONE OF THEM IS A SPECIAL
    /// CASE.**
    ///
    /// `opt-↑`/`opt-↓` mean PARAGRAPH in the textarea, HIS OWN MESSAGES in the
    /// chat, and BAND HEADERS at the root. He specified all three separately
    /// and they look like a collision; they are three rows of one table,
    /// resolved by the region that holds the selection. That is the whole
    /// principle of this build, and this is the test of it.
    ///
    /// FORCED: point the Chat row at `Compose(Edit::ParaUp)` — what a builder
    /// who "unified" the key would write — and the second assertion goes red.
    #[test]
    fn the_opt_arrows_mean_three_things_in_three_regions() {
        assert_eq!(intent(Key::AltUp, COMPOSER), Intent::Compose(Edit::ParaUp));
        assert_eq!(
            intent(Key::AltDown, COMPOSER),
            Intent::Compose(Edit::ParaDown)
        );
        assert_eq!(intent(Key::AltUp, CHAT), Intent::JumpUserPrev);
        assert_eq!(intent(Key::AltDown, CHAT), Intent::JumpUserNext);
        assert_eq!(intent(Key::AltUp, Mode::Projects), Intent::BandPrev);
        assert_eq!(intent(Key::AltDown, Mode::Projects), Intent::BandNext);
        // **AND `one_key_one_meaning_in_one_mode` STILL HOLDS UNWEAKENED**,
        // because the three rows are three different modes. The region model
        // did not need that test taught anything: carrying the region inside
        // `Mode` is what keeps "one key, one meaning" true while a key means
        // three things on one screen.
        for m in [COMPOSER, CHAT, Mode::Projects] {
            assert_ne!(intent(Key::AltUp, m), Intent::None, "opt-↑ is dead in {m:?}");
        }
        // **THE PLAIN ARROWS ARE NOT DISPLACED.** A modified key that ate its
        // unmodified twin would be a far worse defect than the missing one.
        assert_eq!(intent(Key::Up, COMPOSER), Intent::Compose(Edit::Up));
        assert_eq!(intent(Key::Up, CHAT), Intent::ScrollUp);
        assert_eq!(intent(Key::Up, Mode::Projects), Intent::ScrollUp);
    }

    /// **THE ARROWS ARE MODAL, AND THE TABLE IS WHERE THAT IS TRUE.**
    ///
    /// *"When the chat is selected then that's the only time that up and down
    /// should scroll."* So `ScrollUp`/`ScrollDown` must be the CHAT's answer to
    /// `↑`/`↓` and must NOT be the composer's — there the same keys are text.
    #[test]
    fn up_and_down_scroll_only_where_the_chat_holds_the_selection() {
        assert_eq!(intent(Key::Up, CHAT), Intent::ScrollUp);
        assert_eq!(intent(Key::Down, CHAT), Intent::ScrollDown);
        // **IN THE BOX THE SAME KEY IS TEXT**, which is the half of DIR-22
        // that matters: the arrows stopped acting at a distance.
        assert_eq!(intent(Key::Up, COMPOSER), Intent::Compose(Edit::Up));
        assert_eq!(intent(Key::Down, COMPOSER), Intent::Compose(Edit::Down));
        // The QUEUE answers to `ScrollUp` too — and it is not a second
        // meaning: `Intent::ScrollUp` has always been "the up gesture,
        // resolved by the surface", moving a CURSOR in the two list modes and
        // a viewport in a transcript. The queue is a list, so it moves its
        // cursor; `App::scroll_key` is where that resolution lives, and
        // `the_queue_is_a_stop_between_the_chat_and_the_box` proves it.
        assert_eq!(intent(Key::Up, QUEUE), Intent::ScrollUp);
        // `→` in the chat is the END of it, which is the same intent `alt-n`
        // and `opt-↓`-past-the-last reach: one destination, three spellings.
        assert_eq!(intent(Key::Right, CHAT), Intent::Bottom);
    }

    /// **THE MODIFIER IS NAMED FOR THE HOST, AND IT IS THE SAME BINDING.**
    ///
    /// *"alt- doesn't exist on my mac. cmd-q does something different"*, and
    /// *"make sure it's operating system agnostic"*. So the KEY does not
    /// change and the SPELLING does.
    ///
    /// FORCED: return `Self::ALT` from `for_host("macos")` and the first two
    /// assertions go red; make the overlay call `label(ModKeys::default())`
    /// instead of `label(caps.mods)` and the third does.
    #[test]
    fn the_modifier_is_spelled_for_the_host_and_binds_the_same_key() {
        assert_eq!(ModKeys::for_host("macos").alt, "opt-");
        assert_eq!(ModKeys::for_host("ios").alt, "opt-");
        for os in ["linux", "windows", "freebsd", "", "unknown"] {
            assert_eq!(ModKeys::for_host(os).alt, "alt-", "{os} is not a Mac");
        }
        let mac = Caps {
            mods: ModKeys::OPT,
            ..Caps::default()
        };
        // **THE ROW THIS ASKS ABOUT IS AN ARROW NOW, AND THAT IS THE ROUND'S
        // OTHER FINDING.** Every `opt-<LETTER>` spelling migrated to `ctrl`,
        // because macOS composes those into glyphs. The ARROW and named-key
        // family did NOT: an arrow has no composed glyph, `CSI 1;3A` was
        // measured arriving as `Key::AltUp`, and migrating them is the one
        // place an irreducible collision exists (`^↑`/`^↓` carry `0119`'s
        // project reorder and the buffer's start/end). So the host-spelling
        // question is still LIVE and is now asked of `opt-↑`.
        let rows = overlay_rows(COMPOSER, mac);
        assert!(
            rows.iter().any(|(k, _)| k.contains("opt-↑")),
            "the overlay does not spell the paragraph key for his keyboard"
        );
        assert!(
            !rows.iter().any(|(k, _)| k.contains("alt-")),
            "a row still says `alt-` on a host that has no alt key: {rows:?}"
        );
        // …and the same list on any other host says `alt-`, with no `opt-`.
        let other = overlay_rows(COMPOSER, Caps::default());
        assert!(other.iter().any(|(k, _)| k.contains("alt-↑")));
        assert!(!other.iter().any(|(k, _)| k.contains("opt-")));
        // **THE BINDING IS UNTOUCHED BY EITHER SPELLING.** This is not a new
        // modifier and there is no third key class. (`alt-f` was the example
        // here until round six deleted the filter, and `alt-u` until the same
        // round moved the queue to `^O`; `Key::AltUp` is what is left and it is
        // the honest one, because the arrow family is where `ModKeys` still
        // does any work at all.)
        assert_eq!(
            intent(Key::AltUp, COMPOSER),
            Intent::Compose(Edit::ParaUp)
        );
        assert_eq!(Key::AltUp.label(ModKeys::OPT), "opt-↑");
        assert_eq!(Key::AltUp.label(ModKeys::ALT), "alt-↑");
    }

    /// **THE LEGEND IS PER-SECTION, AND THE INVARIANT HALF DOES NOT MOVE.**
    ///
    /// *"each section, when in that band, should display its own keybindings,
    /// not all of them at once. Keep the invariant ones at the left so its easy
    /// to see that they stay the same."* The second sentence is the one with
    /// work in it: the stable half must be the SAME SEQUENCE in every region,
    /// because that is what makes it the same COLUMNS on screen.
    ///
    /// FORCED: give `^S` a different `short` in `chat_rows` than in
    /// `alt_common_rows` and it leaves the stable half in every region — the
    /// first assertion goes red naming the shorter prefix.
    #[test]
    fn the_footer_keeps_the_invariant_keys_in_the_same_columns_in_every_region() {
        let caps = Caps::default();
        let regions = [COMPOSER, CHAT, QUEUE];
        let first = footer_split(regions[0], caps).0;
        assert!(!first.is_empty(), "no invariant hints at all");
        for m in regions {
            let (stable, variable) = footer_split(m, caps);
            assert_eq!(stable, first, "the invariant half moved in {m:?}");
            assert!(
                variable.iter().all(|v| !stable.contains(v)),
                "{m:?} prints a hint in both halves"
            );
        }
        // **AND EACH SECTION SHOWS ITS OWN.** A footer that was merely the
        // same everywhere would satisfy the sentence above and none of his.
        let chat: Vec<_> = footer_split(CHAT, caps).1;
        let composer: Vec<_> = footer_split(COMPOSER, caps).1;
        assert_ne!(chat, composer, "every region prints the same legend");
        assert!(
            chat.iter().any(|(_, s)| *s == "end"),
            "the chat's own keys are not in its legend: {chat:?}"
        );
        assert!(
            composer.iter().any(|(_, s)| *s == "mic"),
            "the box's own keys are not in its legend: {composer:?}"
        );
        // Outside a thread there is one band, so nothing varies and the root's
        // footer is exactly what it always was.
        assert!(footer_split(Mode::Projects, caps).1.is_empty());
    }

    /// Every mode the `?` overlay documents must actually have rows, or the
    /// overlay prints a heading over nothing.
    #[test]
    fn every_documented_mode_has_a_grammar() {
        for m in DOCUMENTED_MODES {
            assert!(
                !overlay_rows(*m, Caps::default()).is_empty(),
                "{m:?} has no bindings"
            );
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    //  THE CTRL MIGRATION, AS MACHINES
    //
    //  Every one of these is a GATE rather than a demonstration: the thing
    //  each defends is a thing a future round could take back by accident,
    //  and `0128`'s whole reason for existing is that the ctrl namespace is
    //  the app's FULLEST rather than its emptiest.
    // ─────────────────────────────────────────────────────────────────────

    /// **THE WHOLE ASSIGNMENT TABLE, BOTH HALVES OF EVERY ROW.**
    ///
    /// His ruling: *"Let's just switch to all ctrl-* across the app instead of
    /// opt-*."* Every migrated act is asserted twice — the CTRL key reaches it,
    /// and the OPT key it replaced reaches nothing. Both halves, because a remap
    /// that leaves the old key live is not a remap, and one that only tests the
    /// new key cannot tell you it did not double-bind.
    ///
    /// **THE ARROW FAMILY IS DELIBERATELY ABSENT FROM THIS TABLE** and has its
    /// own test below: an arrow has no composed glyph, so `opt-↑` always
    /// arrived, and migrating it is the one place an irreducible collision
    /// exists.
    ///
    /// FORCED: change any `is_now` cell to the key it used to be and that row
    /// goes red on its first half; leave any migrated `Key::Alt` row in the
    /// table and the same row goes red on its second.
    #[test]
    fn every_migrated_act_answers_to_its_ctrl_key_and_no_longer_to_its_opt_key() {
        const AGENTS: Mode = Mode::Thread(Region::Agents);
        const PAGE: Mode = Mode::SeeAll(crate::model::BandId::Recent);
        // act | the modes it lives in | was | is now
        let table: &[(Intent, &[Mode], char, Key)] = &[
            (
                Intent::Help,
                &[Mode::Projects, Mode::List, Mode::Activity, PAGE,
                  COMPOSER, CHAT, QUEUE, AGENTS, Mode::Mic],
                '/',
                Key::Ctrl('/'),
            ),
            (
                Intent::QueueOverlay,
                &[Mode::Projects, Mode::List, Mode::Activity, PAGE,
                  COMPOSER, CHAT, QUEUE, AGENTS, Mode::Mic],
                'u',
                Key::Ctrl('o'),
            ),
            (
                Intent::Reload,
                &[Mode::Projects, Mode::List, Mode::Activity, PAGE, CHAT, QUEUE, AGENTS],
                'r',
                Key::Ctrl('r'),
            ),
            (Intent::StdoutOnly, &[COMPOSER, CHAT, QUEUE, AGENTS], 's', Key::Ctrl('s')),
            (Intent::QueueMessage, &[COMPOSER], 'q', Key::Ctrl('q')),
            (Intent::MicMode, &[COMPOSER, Mode::Mic], 'm', Key::Ctrl('t')),
            (Intent::Activate, &[CHAT], 'e', Key::Ctrl('e')),
            (Intent::Compose(Edit::Unqueue), &[QUEUE], 'k', Key::Ctrl('k')),
        ];
        for (act, modes, was, is_now) in table {
            for m in *modes {
                assert_eq!(
                    intent(*is_now, *m),
                    *act,
                    "{is_now:?} does not reach {act:?} in {m:?}"
                );
                assert_eq!(
                    intent(Key::Alt(*was), *m),
                    Intent::None,
                    "opt-{was} still means something in {m:?} — the old key \
                     outlived the remap"
                );
            }
        }
        // **AND THE TWO DIALECTS OF THE HELP CHORD ARE ONE ROW**, so the door is
        // `Need::Always` on a stack with modifyOtherKeys and on one without.
        for m in [Mode::Projects, COMPOSER, Mode::Mic] {
            assert_eq!(intent(Key::Ctrl('7'), m), Intent::Help, "^7 in {m:?}");
        }
        // **AND `opt-?`, the help door's second opt spelling, is dead too** — it
        // was in a different position of the same `KeySet` and a sweep that
        // worked from the act list would have missed it.
        for m in [Mode::Projects, COMPOSER, CHAT, QUEUE, Mode::Mic] {
            assert_eq!(intent(Key::Alt('?'), m), Intent::None, "opt-? in {m:?}");
        }
        // **NO `Key::Alt(char)` SURVIVES ANYWHERE IN THE TABLE.** The sweep as a
        // property rather than as a list, so a row nobody thought of is caught.
        for b in BINDINGS {
            if let KeySet::These(keys) = b.keys {
                for k in keys {
                    assert!(
                        !matches!(k, Key::Alt(_)),
                        "an `opt-<letter>` spelling survives: {:?} in {:?} ({:?})",
                        k,
                        b.mode,
                        b.short
                    );
                }
            }
        }
    }

    /// **HIS SUB-ASK, AND IT IS THE HALF A REMAP CAN SILENTLY FAIL TO DELIVER.**
    ///
    /// > Note that I want the opt-* items to work even when I'm in the textarea.
    /// > They still do not work.
    ///
    /// A ctrl byte ARRIVES in the textarea — nothing composes it away — but the
    /// composer has forty-odd rows of its own and a readline row can CLAIM a
    /// byte the table would otherwise have given to a global. So each global is
    /// asserted from inside `Thread(Composer)` specifically, and the two acts
    /// that genuinely have no home there are asserted as the readline keys they
    /// actually are rather than skipped.
    ///
    /// FORCED: swap the composer's `^S` global for `Compose(Redo)`'s letter and
    /// the first row goes red; take `^O` out of `alt_common_rows` and the second
    /// does.
    #[test]
    fn every_global_works_from_inside_the_textarea() {
        assert!(
            Mode::textarea_is_live(COMPOSER),
            "this test is about the mode where letters are TEXT"
        );
        for (k, want) in [
            (Key::Ctrl('/'), Intent::Help),
            (Key::Ctrl('7'), Intent::Help),
            (Key::Ctrl('s'), Intent::StdoutOnly),
            (Key::Ctrl('o'), Intent::QueueOverlay),
            (Key::Ctrl('q'), Intent::QueueMessage),
            (Key::Ctrl('t'), Intent::MicMode),
            (Key::Ctrl('v'), Intent::MicRecordNow),
            (Key::Ctrl('l'), Intent::Redraw),
            (Key::Ctrl('c'), Intent::Quit),
        ] {
            assert_eq!(intent(k, COMPOSER), want, "{k:?} is dead in the textarea");
        }
        // **AND THE PLAIN LETTERS OF EVERY ONE OF THEM STILL TYPE**, which is
        // the property the modifier exists to protect and the one a careless
        // row would break.
        for c in ['s', 'o', 'q', 't', 'v', 'l', 'c', 'r', 'u', 'e', 'k', 'y', 'd'] {
            assert_eq!(
                intent(Key::Char(c), COMPOSER),
                Intent::Type(c),
                "`{c}` stopped being text in the textarea"
            );
        }
        // **THE TWO REAL LOSSES, ASSERTED AS LOSSES.** `^R` and `^E` are
        // readline here and the acts that wanted them live one region away.
        assert_eq!(intent(Key::Ctrl('r'), COMPOSER), Intent::Compose(Edit::Redo));
        assert_eq!(intent(Key::Ctrl('e'), COMPOSER), Intent::Compose(Edit::RowEnd));
        assert_eq!(intent(Key::Ctrl('k'), COMPOSER), Intent::Compose(Edit::KillToEnd));
    }

    /// **THE FIVE BYTES THAT MAY NEVER BE TAKEN, AS A MACHINE.**
    ///
    /// `0128` measured all 32 C0 bytes plus `0x7f` through the SHIPPED
    /// `vt::Parser` -> `input::to_key` path, and four of them are not free
    /// letters at all:
    ///
    /// * **`^I` IS Tab** (`0x09` decodes to `Key::Tab`; `Ctrl('i')` is reachable
    ///   only as `CSI 27;5;105~`), so a `Need::Always` row here is dead.
    /// * **`^M` IS Enter** (`0x0d` -> `Key::Enter`), same mechanism.
    /// * **`^J` IS the newline**, and it is a DELIBERATE alias of `opt-enter`
    ///   rather than a leftover — proven identical in intent.
    /// * **`^H` IS Backspace**, because it is what a terminal whose ERASE is
    ///   `^H` actually sends.
    /// * **`^C` must always quit.** It is the one key a terminal user never has
    ///   to be told.
    ///
    /// Plus `^[` (Esc), `^?` (no raw byte exists) and `^ ` (`Key::label` renders
    /// it with a trailing space).
    ///
    /// The check is not "unbound" — three of them ARE bound, to the key they
    /// alias — it is **bound to that and nothing else, in no mode.** A future
    /// round cannot take one by accident.
    ///
    /// FORCED: bind `Key::Ctrl('m')` to `Intent::Reload` anywhere and this names
    /// the mode; give `Key::Ctrl('j')` to `Intent::QueueMessage` and it names
    /// the wrong-act branch instead.
    #[test]
    fn no_structurally_unavailable_byte_is_bound_to_anything_it_does_not_alias() {
        // byte | the ONLY act it may ever carry (`None` = it may carry nothing)
        let reserved: &[(Key, Option<Intent>)] = &[
            (Key::Ctrl('i'), None),
            (Key::Ctrl('m'), None),
            (Key::Ctrl('['), None),
            (Key::Ctrl('?'), None),
            (Key::Ctrl(' '), None),
            (Key::Ctrl('j'), Some(Intent::Compose(Edit::Newline))),
            (Key::Ctrl('h'), Some(Intent::Compose(Edit::Backspace))),
            (Key::Ctrl('c'), Some(Intent::Quit)),
        ];
        for b in BINDINGS {
            let KeySet::These(keys) = b.keys else { continue };
            for k in keys {
                if let Some((_, allowed)) = reserved.iter().find(|(r, _)| r == k) {
                    match allowed {
                        None => panic!(
                            "{k:?} is structurally unavailable and {:?} bound it in \
                             {:?} — see 0128's wire measurement",
                            b.short, b.mode
                        ),
                        Some(only) => assert_eq!(
                            &b.intent, only,
                            "{k:?} may only ever mean {only:?}; {:?} in {:?} took it",
                            b.short, b.mode
                        ),
                    }
                }
            }
        }
        // **AND THE WITNESS**, because every assertion above is an ABSENCE and
        // an absence passes on an empty table. Three cells no mutation of this
        // round can move.
        assert!(BINDINGS.len() > 100, "the table did not build");
        assert_eq!(intent(Key::Ctrl('c'), Mode::Projects), Intent::Quit);
        assert_eq!(intent(Key::Ctrl('i'), Mode::Projects), Intent::None);
    }

    /// **TAB, ENTER, NEWLINE AND BACKSPACE STILL WORK.** The four keys the whole
    /// audit exists to protect, each asserted where it means the most, because
    /// binding `^I`, `^M`, `^J` or `^H` to a migrated act would have broken the
    /// key it aliases and the face would still have compiled.
    ///
    /// FORCED: one mutation per key. `^I` -> `Intent::Reload` in `Mode::Sheet`
    /// and tab stops completing; `^M` -> anything in the composer and `enter`
    /// stops sending; take `Key::Ctrl('j')` out of the newline row and the
    /// no-protocol newline goes; give `Key::Ctrl('h')` to a global and backspace
    /// dies on a `stty erase ^H` terminal.
    #[test]
    fn tab_enter_newline_and_backspace_all_survive_the_remap() {
        // TAB — the root's view swap and the field's completion.
        assert_eq!(intent(Key::Tab, Mode::Projects), Intent::SwapView);
        assert_eq!(intent(Key::Tab, Mode::NewProject), Intent::CompletePath);
        // ENTER — how he sends, in all four places a payload leaves from.
        for m in [COMPOSER, CHAT, QUEUE, Mode::Mic] {
            assert_eq!(intent(Key::Enter, m), Intent::Send, "enter in {m:?}");
        }
        assert_eq!(intent(Key::Enter, Mode::Projects), Intent::Activate);
        // NEWLINE — both spellings, and `^J` is the one that needs no protocol.
        for m in [COMPOSER, Mode::SheetField] {
            assert_eq!(
                intent(Key::AltEnter, m),
                Intent::Compose(Edit::Newline),
                "opt-enter in {m:?}"
            );
            assert_eq!(
                intent(Key::Ctrl('j'), m),
                Intent::Compose(Edit::Newline),
                "^J in {m:?}"
            );
        }
        // BACKSPACE — both spellings, and `^H` is what a `stty erase ^H`
        // terminal sends.
        for m in [COMPOSER, Mode::SheetField] {
            assert_eq!(
                intent(Key::Backspace, m),
                Intent::Compose(Edit::Backspace),
                "bksp in {m:?}"
            );
            assert_eq!(
                intent(Key::Ctrl('h'), m),
                Intent::Compose(Edit::Backspace),
                "^H in {m:?}"
            );
        }
    }

    /// **THE ARROW FAMILY DID NOT MIGRATE, AND THIS IS WHY IT MUST NOT.**
    ///
    /// The defect he reported is macOS composing `opt-<CHARACTER>` into a glyph
    /// — µ, ®, ƒ. **An arrow has no composed glyph**: `CSI 1;3A` was measured
    /// arriving as `Key::AltUp` through the shipped parser, so these keys were
    /// never broken. And migrating them is the ONLY place an irreducible
    /// collision exists — `^↑`/`^↓` already carry `0119`'s project reorder at
    /// the root and the buffer's start/end in the box, so five acts would want
    /// two keys.
    ///
    /// FORCED: give `Intent::BandPrev` `Key::CtrlUp` as well and
    /// `one_key_one_meaning_in_one_mode` goes red naming `MovePinUp`; take
    /// `Key::AltUp` out of `chat_rows` and the chat assertion here goes red.
    #[test]
    fn the_arrow_family_is_left_exactly_as_it_was() {
        // The three regions where `opt-↑`/`opt-↓` mean three different things.
        assert_eq!(intent(Key::AltUp, Mode::Projects), Intent::BandPrev);
        assert_eq!(intent(Key::AltDown, Mode::Projects), Intent::BandNext);
        assert_eq!(intent(Key::AltUp, CHAT), Intent::JumpUserPrev);
        assert_eq!(intent(Key::AltDown, CHAT), Intent::JumpUserNext);
        assert_eq!(intent(Key::AltUp, COMPOSER), Intent::Compose(Edit::ParaUp));
        assert_eq!(intent(Key::AltDown, COMPOSER), Intent::Compose(Edit::ParaDown));
        // The word keys, the limit keys and the kill-word alias.
        assert_eq!(intent(Key::AltLeft, COMPOSER), Intent::Compose(Edit::WordLeft));
        assert_eq!(intent(Key::AltRight, COMPOSER), Intent::Compose(Edit::WordRight));
        assert_eq!(intent(Key::AltHome, COMPOSER), Intent::Compose(Edit::BufStart));
        assert_eq!(intent(Key::AltEnd, COMPOSER), Intent::Compose(Edit::BufEnd));
        assert_eq!(intent(Key::AltBackspace, COMPOSER), Intent::Compose(Edit::KillWord));
        assert_eq!(intent(Key::AltEnter, COMPOSER), Intent::Compose(Edit::Newline));
        // **AND THE COLLISION THE MIGRATION WOULD HAVE COST**, asserted as the
        // thing that still works: `^↑`/`^↓` are `0119`'s reorder at the root and
        // the buffer's limits in the box, in the same breath as the opt-arrows
        // above meaning something else in the same two modes.
        assert_eq!(intent(Key::CtrlUp, Mode::Projects), Intent::MovePinUp);
        assert_eq!(intent(Key::CtrlDown, Mode::Projects), Intent::MovePinDown);
        assert_eq!(intent(Key::CtrlUp, COMPOSER), Intent::Compose(Edit::BufStart));
        assert_eq!(intent(Key::CtrlDown, COMPOSER), Intent::Compose(Edit::BufEnd));
    }

    /// **THE COLLISION GATE'S BLIND SPOT, CLOSED.**
    ///
    /// `one_key_one_meaning_in_one_mode` walks `KeySet::These` only, and it is
    /// right to: `Any` and `Printable` are deliberate catch-alls arbitrated by
    /// first-match-wins, so folding them in would report the design as a
    /// collision. **But the arbitration is POSITIONAL** — `intent()` takes the
    /// first matching row — so a catch-all is only safe while every specific row
    /// it overlaps sits ABOVE it. A specific row placed after a mode's `Any` row
    /// would be silently dead, and neither the gate nor the compiler would say
    /// so.
    ///
    /// This is that rule as a machine, in table ORDER:
    ///
    /// * after a `KeySet::Any` row, a mode may have NO further rows at all;
    /// * after a `KeySet::Printable` row, no further row may claim a printable
    ///   `Key::Char`.
    ///
    /// **A ctrl byte can never be shadowed by `Printable`** — that set matches
    /// `Key::Char(c)` for printable `c`, and `Key::Ctrl(c)` is a different
    /// variant — which is why this round's assignments were safe. The gate is
    /// written for the round that does not check.
    ///
    /// FORCED: move `Mode::Help`'s `Any` row above its `pgup` row and this names
    /// `Help`; add a `Key::Char('z')` row after `composer_rows`' `Printable` row
    /// and it names `Thread(Composer)`.
    #[test]
    fn a_catch_all_row_never_shadows_a_specific_one() {
        for m in DOCUMENTED_MODES {
            let rows: Vec<&Binding> = BINDINGS.iter().filter(|b| b.mode == *m).collect();
            let mut after_any: Option<&str> = None;
            let mut after_printable: Option<&str> = None;
            for b in rows {
                if let Some(who) = after_any {
                    panic!(
                        "{m:?}: {:?} sits after the `any key` row ({who}) and is \
                         therefore DEAD — first match wins",
                        b.short
                    );
                }
                if let (Some(who), KeySet::These(keys)) = (after_printable, b.keys) {
                    for k in keys {
                        assert!(
                            !matches!(k, Key::Char(c) if is_printable(*c)),
                            "{m:?}: {:?} claims {k:?} after the `any character` row \
                             ({who}) and is therefore DEAD",
                            b.short
                        );
                    }
                }
                match b.keys {
                    KeySet::Any => after_any = Some(b.short),
                    KeySet::Printable => after_printable = Some(b.short),
                    KeySet::These(_) => {}
                }
            }
        }
        // THE WITNESS: the two modes that DO have an `Any` row, so the loop is
        // known to have reached the branch it exists for.
        for m in [Mode::Help, Mode::Queue] {
            assert!(
                BINDINGS
                    .iter()
                    .any(|b| b.mode == m && matches!(b.keys, KeySet::Any)),
                "{m:?} has no `any key` row — this test proved nothing"
            );
        }
        assert!(
            BINDINGS
                .iter()
                .any(|b| b.mode == COMPOSER && matches!(b.keys, KeySet::Printable)),
            "the composer has no `any character` row — this test proved nothing"
        );
    }

    /// **NO ROW'S PROSE MAY NAME THE MODIFIER, because the modifier is spelled
    /// BY HOST and prose is not.**
    ///
    /// `0128` found six strings in this crate that printed a literal `alt-` to
    /// the screen while every table-derived label printed `opt-` on his Mac.
    /// They were a live presentation defect before this round and would have
    /// been outright false after it — `^Q queues` while the key is `^Q`.
    ///
    /// **The fix is the CLASS, not the six.** A hand-corrected literal is the
    /// same defect with today's spelling, and the next remap finds it again. So
    /// an `act` may not name a host-spelled modifier at all: the KEY COLUMN
    /// beside it already carries the spelling, from [`Key::label`], and a second
    /// source of truth is the whole bug. Anything that genuinely has to say
    /// "press X" in a sentence asks [`spelling`].
    ///
    /// `^P`/`^N` survive in two acts and are deliberately allowed: a caret
    /// spelling is the same on every host, so it cannot disagree with a label.
    ///
    /// FORCED: put `(^R still reloads)` back on either rename row and this
    /// names it.
    #[test]
    fn no_act_or_short_names_the_host_spelled_modifier() {
        for b in BINDINGS {
            for (what, text) in [("act", b.act), ("short", b.short)] {
                for bad in [ModKeys::ALT.alt, ModKeys::OPT.alt] {
                    assert!(
                        !text.contains(bad),
                        "{:?}'s {what} in {:?} spells the modifier itself: {text:?} — \
                         the key column is derived from `Key::label`, so prose that \
                         repeats it is a second source of truth",
                        b.short,
                        b.mode
                    );
                }
            }
        }
        // THE WITNESS: the strings really are being read, and the predicate
        // really can fire.
        assert!(BINDINGS.iter().any(|b| b.act.len() > 20));
        assert!("newline (an alias — alt-enter always works)".contains(ModKeys::ALT.alt));
    }

    /// **NO ROW PROMISES A CLICK ANY MORE, except where a click really does
    /// still act.**
    ///
    /// Round six reversed the click law — *"a click never enters"* — and the
    /// BEHAVIOUR went out in `0127` while the WORDS describing it survived: two
    /// `Intent::Options` rows still ended *"(or click its name to open it)"*,
    /// which advertised the exact gesture he had had us remove. **A legend that
    /// lies is worse than a missing one, because he will press the thing it
    /// names.**
    ///
    /// **AN ACT MAY MENTION A CLICK IN EXACTLY TWO WAYS.**
    ///
    ///  1. To say a click *SELECTS* — which is the new law stated out loud, and
    ///     is the opposite of a promise. Two `Intent::Open` rows do this and
    ///     they are the reason `→` needs explaining at all.
    ///  2. To name the `▸` EXPANDER, whose click route was kept deliberately (it
    ///     reveals in place and is the transcript's only mouse door). Those rows
    ///     are checked against `hit::activation` rather than exempted by name,
    ///     so the day that route closes they go red instead of lying.
    ///
    /// Anything else is a promise, and a promise is what came out this round.
    ///
    /// FORCED: put the parenthetical back on either `Intent::Options` row and
    /// this names it; narrow `Target::Expander`'s arm to `Via::Key` and the
    /// surviving `▸` promises go red instead.
    #[test]
    fn no_act_promises_a_click_that_no_longer_acts() {
        let (mut selects, mut expanders) = (0, 0);
        for b in BINDINGS {
            if !b.act.contains("click") {
                continue;
            }
            if b.act.contains("a click SELECTS the row") {
                selects += 1;
                continue;
            }
            assert!(
                b.act.contains("▸"),
                "{:?} in {:?} promises a click: {:?}",
                b.short,
                b.mode,
                b.act
            );
            expanders += 1;
            assert_eq!(
                crate::hit::activation(
                    crate::hit::Target::Expander(0),
                    b.mode,
                    crate::hit::Via::Click
                ),
                Some(Intent::ExpandBlock(0)),
                "{:?} in {:?} promises the ▸ click and it no longer acts",
                b.short,
                b.mode
            );
        }
        // THE WITNESS, both branches, so a table that had lost every mention of
        // a click could not score this.
        assert!(selects > 0, "no act states the click law — this test proved nothing");
        assert!(expanders > 0, "no act names the ▸ — this test proved nothing");
        // **AND THE PROMISE THAT WAS ACTUALLY THERE, NAMED**, so this cannot be
        // read as a rule nobody ever broke.
        assert!(
            !BINDINGS
                .iter()
                .any(|b| b.act.contains("click its name to open it")),
            "the retracted click promise is back on a row"
        );
    }

    /// **`spelling` IS THE ONE PLACE A RENDERED SENTENCE LEARNS A KEY**, and it
    /// answers `None` rather than inventing one.
    ///
    /// FORCED: make it ignore `needs.met(caps)` and the `Need::Enhanced`
    /// assertion goes red; make it return the first row of any mode and the
    /// composer-reload `None` goes red.
    #[test]
    fn spelling_reads_the_table_and_admits_when_there_is_no_key() {
        let caps = Caps::default();
        assert_eq!(
            spelling(Intent::QueueOverlay, Mode::Mic, caps).as_deref(),
            Some("^O")
        );
        assert_eq!(
            spelling(Intent::StdoutOnly, COMPOSER, caps).as_deref(),
            Some("^S")
        );
        // **THE HOST STILL CHOOSES THE SPELLING** where a spelling is
        // host-dependent, which is the whole reason this is derived.
        let mac = Caps {
            mods: ModKeys::OPT,
            ..Caps::default()
        };
        assert_eq!(
            spelling(Intent::Compose(Edit::Newline), COMPOSER, mac).as_deref(),
            Some("opt-enter / ^J")
        );
        assert_eq!(
            spelling(Intent::Compose(Edit::Newline), COMPOSER, caps).as_deref(),
            Some("alt-enter / ^J")
        );
        // **AND `None` WHERE THERE IS NO KEY**, which is what stops a rendered
        // row promising reload inside the textarea.
        assert_eq!(spelling(Intent::Reload, COMPOSER, caps), None);
        assert_eq!(spelling(Intent::Reload, CHAT, caps).as_deref(), Some("^R"));
        // A withheld row is not a spelling.
        assert_eq!(spelling(Intent::MicRecordNow, Mode::Projects, caps), None);
    }

    /// **THE DRAFT OFFER IS A MODAL, NOT TWO BINDINGS**, and that is the answer
    /// `0128` recommended over any letter: `^Y` is yank and `^D` is delete, so
    /// `AcceptDraft`/`DiscardDraft` had no mnemonic ctrl home — but they mean
    /// nothing unless the offer is on screen, so answering the offer itself
    /// costs no namespace at all.
    ///
    /// FORCED: add a `Key::Char('y') -> AcceptDraft` row to `composer_rows` and
    /// the "not in the table" half goes red — and `y` would stop typing.
    #[test]
    fn the_draft_offer_is_answered_by_two_keys_that_are_not_in_the_table() {
        assert_eq!(
            draft_offer_intent(DRAFT_OFFER_ACCEPT),
            Some(Intent::AcceptDraft)
        );
        assert_eq!(
            draft_offer_intent(DRAFT_OFFER_DISCARD),
            Some(Intent::DiscardDraft)
        );
        assert_eq!(draft_offer_intent(Key::Char('z')), None);
        assert_eq!(draft_offer_intent(Key::Ctrl('y')), None);
        // **NEITHER ACT IS A ROW.** A row would claim `y` and `n`
        // unconditionally, in the one mode where every letter is text.
        for b in BINDINGS {
            assert!(
                !matches!(b.intent, Intent::AcceptDraft | Intent::DiscardDraft),
                "the draft offer became a binding again, in {:?}",
                b.mode
            );
        }
        // …and the two letters still type, which is the property that pays for
        // the modal.
        assert_eq!(
            intent(DRAFT_OFFER_ACCEPT, COMPOSER),
            Intent::Type('y'),
            "`y` stopped being text in the composer"
        );
        assert_eq!(intent(DRAFT_OFFER_DISCARD, COMPOSER), Intent::Type('n'));
    }

    /// **`^V` RECORDS FROM EVERY SURFACE INSIDE A THREAD, AND IT IS THE ONE
    /// PLACE HIS LITERAL WORDS WERE NOT EXECUTED.**
    ///
    /// > I need opt-r to let me do that regardless.
    ///
    /// `R` was not available: `^R` is `Compose(Redo)` in the composer — its only
    /// spelling — and RELOAD in every mode that has one, and `0128` forbids
    /// splitting one act across two letters in different modes. So the act took
    /// a letter it can keep everywhere. `V` is for voice.
    ///
    /// FORCED: take the `^V` row out of any one region's macro and this names
    /// that region.
    #[test]
    fn record_now_is_one_letter_in_every_region_and_nothing_else_took_it() {
        for m in [COMPOSER, CHAT, QUEUE, Mode::Thread(Region::Agents), Mode::Mic] {
            assert_eq!(
                intent(Key::Ctrl('v'), m),
                Intent::MicRecordNow,
                "^V does not record in {m:?}"
            );
        }
        // **AND `r`/`space` ARE STILL THE TOGGLE IN MIC MODE**, untouched: tap
        // toggles, hold streams (`0120`), and this round did not go near it.
        assert_eq!(intent(Key::Char('r'), Mode::Mic), Intent::MicRecord);
        assert_eq!(intent(Key::Char(' '), Mode::Mic), Intent::MicRecord);
        // It is a THREAD act and must not appear on a non-thread surface, which
        // is `no_non_thread_mode_names_a_thread_scoped_act`'s rule applied to
        // the one act this round minted.
        assert!(Intent::MicRecordNow.is_thread_scoped());
        for m in [Mode::Projects, Mode::List, Mode::Activity] {
            assert_eq!(intent(Key::Ctrl('v'), m), Intent::None, "^V in {m:?}");
        }
    }
}
