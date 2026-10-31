//! **THE INPUT PATH: where terminal input enters this program, and the one
//! place it is translated.**
//!
//! Three parts now, and they belong together because they are the same
//! boundary seen from each end:
//!
//! * [`spawn_pump`] — the SOURCE. **Two plain OS threads: one doing blocking
//!   reads of the raw file descriptor, one turning those bytes into [`Ev`]
//!   through [`crate::vt::Parser`].** It used to be one thread calling
//!   `crossterm::event::read`; `0050` took the read because crossterm holding
//!   bytes in a buffer we could not reach was the root of BOTH `IS-33` and
//!   `IS-38`. The parser's own header carries that argument in full.
//! * [`to_key`] and [`Gesture`] — the TRANSLATION. `crossterm::KeyEvent`
//!   becomes `tui_render::Key`; a stream of mouse reports becomes a wheel
//!   notch, a click, or a drag.
//! * [`defuse`] — the BATCH RULE, unchanged by `0050` and deliberately so.
//!
//! **The only place crossterm is named in an input context.** The key TABLE
//! lives in the pure crate, which may not depend on a terminal driver, so the
//! boundary has to be crossed exactly once and here is it — which is also why
//! the pump moved here out of `app.rs`: a coordinator that owns the loop and
//! ALSO owns the thread that reads the device has two jobs, and the second one
//! is this module's.
//!
//! **`crossterm::event::Event` IS STILL THE VOCABULARY, and that is the point
//! of the whole shape.** `vt` parses the bytes itself but emits crossterm's own
//! types, so `to_key`, `defuse` and `Gesture` did not change a line and every
//! test they carry still runs against the same values. The layer that was
//! inserted is invisible from above it.
//!
//! **Nothing in here decides what a key MEANS.** That is `tui_render::intent`,
//! scanning the one table. A binding written in this file would be a binding
//! the `?` overlay could not render, which is the drift the table exists to
//! make impossible.
//!
//! **THE SEAM FOR THE INPUT GRAMMAR.** A click, a drag, a left/right gesture —
//! whatever the grammar turns out to be — arrives as a `crossterm` event at
//! [`pump_once`] and leaves as an `Ev`. Widening it is: a variant on
//! [`crate::event::Ev`], an arm in [`pump_once`], and an arm in
//! [`Gesture::feed`]. Nothing above this module has to learn a new device
//! word, and nothing below it has to learn a terminal one.
//!
//! **A GESTURE HAS STATE, AND IT LIVES HERE.** A click and a drag are the same
//! bytes until the release, so telling them apart is a state machine over
//! three event kinds rather than a function of any one of them — see
//! [`Gesture`]. It sits beside the device rather than in the coordinator,
//! because the thing it is reasoning about is the device's report stream.

use std::io::{self, IsTerminal, Read};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::terminal;
use tokio::sync::mpsc;
use tui_render::Key;

use crate::event::{Ev, Input};

/// Total, and lossy on purpose: a key this face has no spelling for is `None`,
/// not a panic and not a guess.
///
/// **RELEASE AND REPEAT ARE BOTH FILTERED HERE, and `Repeat` is new.** A
/// release that acted would double every keystroke; that was already true. A
/// REPEAT that acted was harmless while every binding was a navigation, and is
/// not harmless now: `enter` SENDS, and a key held a fraction too long under
/// the kitty protocol delivers `Repeat` events that would each fire a separate
/// `threads/{id}/send`. The one thing lost is auto-repeat while scrolling, which the
/// wheel and `pgdn` already cover.
pub fn to_key(ev: KeyEvent) -> Option<Key> {
    if ev.kind != KeyEventKind::Press {
        return None;
    }
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
    Some(match ev.code {
        // **THE CTRL ARROWS ARRIVE AND WERE BEING THROWN AWAY.** Measured on a
        // private tmux server configured exactly as his live one
        // (`extended-keys on`, `extended-keys-format xterm`, tmux 3.5a):
        // `ctrl-←` emits `CSI 1;5D` and `vt` decodes it as `Left+CONTROL`,
        // correctly, today. It then reached THIS function, which had no `ctrl`
        // arm for an arrow, and fell through to `Key::Left` — so the key was not
        // swallowed by tmux and not missed by the parser, it was FLATTENED here,
        // one line above where it would have been used. That is the whole of
        // `0052`'s "if it can be made to work in his terminal": it can, and the
        // measurement is in `motion_keys_probe.sh`.
        KeyCode::Up if ctrl => Key::CtrlUp,
        KeyCode::Down if ctrl => Key::CtrlDown,
        // **AND THE ALT ARROWS WERE BEING THROWN AWAY THE SAME WAY, ONE AXIS
        // OVER.** `alt-←`/`alt-→` had arms two lines below and the vertical
        // pair did not, so `opt-↑` arrived here as a plain `↑` and the key
        // table never saw it — the identical FLATTENING `0052` found for the
        // ctrl-arrows, in the identical place, and the reason `opt-↑` "did
        // nothing" rather than doing something wrong.
        //
        // **MEASURED BEFORE BOUND**, as that lesson requires: tmux configured
        // as his is emits `CSI 1;3A` / `CSI 1;3B` and `vt` decodes them as an
        // arrow carrying ALT. See `motion_keys_probe.sh`, whose key list now
        // includes them, and the wire table in `vt`'s tests.
        KeyCode::Up if alt => Key::AltUp,
        KeyCode::Down if alt => Key::AltDown,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left if ctrl => Key::CtrlLeft,
        KeyCode::Right if ctrl => Key::CtrlRight,
        // In the two lists, **BACK AND ACTIVATE** — the AUI's
        // `NavAction::Back`/`Select`. In a thread the composer owns them; the
        // TABLE decides which, and this function only says which key it is. A
        // key this function has no arm for is `None` and therefore silent,
        // which is how the first live run of the new grammar had a dead `→`.
        KeyCode::Left if alt => Key::AltLeft,
        KeyCode::Right if alt => Key::AltRight,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        // **THE THREE ENTERS, AND THEY ARE THREE DIFFERENT ACTS.** `enter`
        // SENDS; `alt-enter` is a newline and arrives as `\x1b\r` on every
        // terminal with no protocol at all; `shift-enter` and `^enter` arrive
        // ONLY where the stack can report them, which is what `Caps` is for.
        KeyCode::Enter if alt => Key::AltEnter,
        KeyCode::Enter if ctrl => Key::CtrlEnter,
        KeyCode::Enter if shift => Key::ShiftEnter,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab | KeyCode::BackTab => Key::Tab,
        // **`opt-delete`.** The Mac `delete` key is a BACKSPACE, and with
        // option held tmux emits `ESC \x7f` plainly or `CSI 27;3;127~` once
        // `modifyOtherKeys` is on — both measured, and `vt` decodes both to
        // Backspace+ALT, so one arm covers the session whichever way it
        // negotiated.
        KeyCode::Backspace if alt => Key::AltBackspace,
        KeyCode::Backspace => Key::Backspace,
        // **`opt-forward-delete` IS DELIBERATELY NOT SPLIT OUT.** It arrives as
        // `CSI 3;3~` → Delete+ALT and is flattened here to a plain `del`, which
        // deletes one character forward. That is the status quo and it is a
        // conservative one; `DIR-14` says stop adding bindings on reflex, and he
        // asked for the BACKWARD word delete by name. Recorded so the next
        // reader knows it is a decision rather than an oversight.
        KeyCode::Delete => Key::Delete,
        KeyCode::Home if alt => Key::AltHome,
        KeyCode::End if alt => Key::AltEnd,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        // Ctrl always carries the LOWERCASE letter; the table is written that
        // way, and `Ctrl-C` arriving as `Ctrl('C')` would silently miss quit.
        KeyCode::Char(c) if ctrl => Key::Ctrl(c.to_ascii_lowercase()),
        KeyCode::Char(c) if alt => Key::Alt(c.to_ascii_lowercase()),
        // CASE-SIGNIFICANT, and that is `0005` §3's lowercase=cheap /
        // uppercase=heavy rule depending on it: `g` and `G` are two bindings.
        KeyCode::Char(c) => Key::Char(c),
        _ => return None,
    })
}

/// How many text-bearing keystrokes in ONE drained read are a paste rather than
/// typing.
///
/// It is the backstop for the case where bracketed paste is not honoured at all
/// and a paste arrives as a burst of ordinary keys with nothing marking it. At
/// 120 words a minute a human produces about ten characters a SECOND, and a
/// drained read covers a millisecond — so eight is three orders of magnitude
/// above anything a hand can do and needs no tuning. tmux encodes the same
/// judgement in `assume-paste-time 1`.
pub const PASTE_BURST: usize = 8;

/// **THE TEXT A KEYSTROKE WOULD HAVE BEEN, if it were paste residue.**
///
/// `None` means "this keystroke is not text", and that is the important half:
/// every `None` is a key that gets DROPPED inside a paste batch rather than
/// acting. `^Q` in the tail of a corrupted paste must not queue his message;
/// `^C` in it must not quit.
fn key_as_text(k: &KeyEvent) -> Option<String> {
    if k.kind != KeyEventKind::Press {
        return None;
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    match k.code {
        // A newline inside a paste reaches crossterm as a CR and comes back out
        // as Enter. It is the exact byte that would have SENT.
        KeyCode::Enter => Some("\n".into()),
        // `^J` IS a newline — the byte `\n` — and is the other spelling of the
        // same thing.
        KeyCode::Char('j') if ctrl && !alt => Some("\n".into()),
        KeyCode::Tab => Some("\t".into()),
        KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => Some(c.to_string()),
        _ => None,
    }
}

/// **THE BATCH RULE — the defence for the paste defect, as a pure function.**
///
/// A paste whose content contains the bytes `ESC[201~` is truncated by
/// crossterm's parser and its TAIL is delivered as ordinary key events,
/// including a trailing Enter. Under "enter sends" that dispatches a message he
/// did not write. The rule: **from the first paste in a batch onwards, nothing
/// may act.** Every text-bearing keystroke in that tail is folded back into the
/// paste it came from and everything else is dropped.
///
/// **This is deliberately BETTER than discarding the tail**, which is what a
/// patch to crossterm would do: the tail really is his clipboard's content, so
/// dropping it loses his text silently. Folding it back reconstructs the
/// clipboard as faithfully as the stack allows, atomically, as one undo step,
/// and with nothing in it able to reach a command.
///
/// **Its second half covers the case bracketed paste does not.** With `?2004h`
/// unhonoured there is no paste EVENT at all, only a burst of keys — so a batch
/// that is entirely text-bearing keystrokes and at least [`PASTE_BURST`] long is
/// treated as a paste too. Mouse reports and resizes are never folded: they
/// cannot be paste residue, and dropping them would break the drag.
pub fn defuse(batch: Vec<Input>) -> Vec<Input> {
    let key_count = batch.iter().filter(|i| matches!(i, Input::Key(_))).count();
    let all_keys_are_text = batch
        .iter()
        .all(|i| !matches!(i, Input::Key(k) if key_as_text(k).is_none()));
    let start = match batch.iter().position(|i| matches!(i, Input::Paste(_))) {
        Some(i) => i,
        None if key_count >= PASTE_BURST && all_keys_are_text => 0,
        None => return batch,
    };

    let mut out: Vec<Input> = Vec::new();
    let mut tail: Vec<Input> = Vec::new();
    let mut text = String::new();
    for (i, item) in batch.into_iter().enumerate() {
        if i < start {
            out.push(item);
            continue;
        }
        match item {
            Input::Paste(s) => text.push_str(&s),
            Input::Key(k) => {
                if let Some(s) = key_as_text(&k) {
                    text.push_str(&s);
                } else {
                    tracing::debug!(?k, "dropped a keystroke inside a paste batch");
                }
            }
            // A mouse report or a resize in the same read as a paste is not
            // paste residue and keeps its meaning.
            other => tail.push(other),
        }
    }
    out.push(Input::Paste(text));
    out.extend(tail);
    out
}

/// **THE GESTURE CLASSIFIER — a state machine over three event kinds, not a
/// heuristic over paths.**
///
/// This is the thing `?1000h` made impossible. With press, motion-while-held
/// and release all arriving (`?1002h`, see `term::CLAIMS`), a gesture
/// classifies ITSELF:
///
/// ```text
///   press ───────────────────────────► release, no motion  ⇒ CLICK
///     │
///     └── motion (a different cell) ──► release            ⇒ DRAG
/// ```
///
/// **The two are mutually exclusive by construction, and the decision is made
/// at RELEASE.** A click never flashes an activation on the way into a drag,
/// because nothing acts at press time; a drag never activates, because the
/// release that would have activated has already been claimed by the motion
/// that preceded it. There is no timer and no distance threshold, so there is
/// no tuning and nothing to get wrong on a slow trackpad.
///
/// **`moved` is sticky, and it is set only by motion onto a DIFFERENT CELL
/// than the anchor.** Terminals report cell motion, so a report normally means
/// a new cell already — but a same-cell report would otherwise turn every
/// slightly-shaky click into a drag that selects one character and silently
/// swallows the activation. Sticky, so a drag out and back to the anchor cell
/// is still a drag: his hand moved, he did not mean to click.
#[derive(Debug, Default)]
pub struct Gesture {
    /// The cell the button went down on. `None` between gestures.
    anchor: Option<(u16, u16)>,
    /// Whether any motion has reached a cell other than the anchor.
    moved: bool,
}

impl Gesture {
    /// One mouse report in, at most one [`Input`] out.
    pub fn feed(&mut self, m: MouseEvent) -> Option<Input> {
        let (col, row) = (m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollUp => Some(Input::Wheel { up: true }),
            MouseEventKind::ScrollDown => Some(Input::Wheel { up: false }),
            MouseEventKind::Down(MouseButton::Left) => {
                self.anchor = Some((col, row));
                self.moved = false;
                // NOT an activation. It anchors, and it clears the last
                // selection off the screen; both of those are things a press
                // may do, and acting on the model is not.
                Some(Input::Press { col, row })
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let anchor = self.anchor?;
                if (col, row) == anchor && !self.moved {
                    return None;
                }
                self.moved = true;
                Some(Input::Drag { col, row })
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // A release we never saw the press for — the gesture began
                // before this program claimed the mouse, or tmux ate the
                // press. It is not a click: a click we did not see the start
                // of is a click we cannot vouch for.
                self.anchor.take()?;
                if std::mem::take(&mut self.moved) {
                    Some(Input::DragEnd { col, row })
                } else {
                    Some(Input::Click { col, row })
                }
            }
            // Idle motion (`?1003h`, which is NOT claimed), and every other
            // button: dropped. The other buttons are RESERVED and bound to
            // nothing — `DIR-14` says stop adding bindings on reflex.
            _ => None,
        }
    }
}

/// **THE DEVICE READ, on a thread that does nothing else.**
///
/// It is its own thread because a `read()` on a terminal blocks with no
/// timeout, and every one of [`crate::vt`]'s four timing rules needs a timeout.
/// Splitting them gives the pump a `recv_timeout` — a deadline it can wait on
/// without the file descriptor having to support one — and it does it with no
/// `poll`, no `select` and no `libc`, which matters in a crate whose manifest
/// says `unsafe_code = "forbid"`.
///
/// **Which fd.** Stdin when stdin is a terminal, which is the normal case and
/// the one `enable_raw_mode` has already configured. Otherwise `/dev/tty`,
/// because a face whose stdin was redirected still has a keyboard — and this is
/// the same rule crossterm applies, kept so that redirecting stdin does not
/// silently change which device the keys come from.
fn spawn_reader() -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (btx, brx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut src: Box<dyn Read + Send> = if io::stdin().is_terminal() {
            Box::new(io::stdin())
        } else {
            match std::fs::File::open("/dev/tty") {
                Ok(f) => Box::new(f),
                Err(e) => {
                    tracing::error!(error = %e, "no terminal to read: the keyboard is gone");
                    return;
                }
            }
        };
        // 8 KiB: comfortably more than any one keystroke or mouse report, and
        // small enough that a huge paste arrives as a stream the parser can
        // start folding rather than as one allocation.
        let mut buf = [0u8; 8192];
        loop {
            match src.read(&mut buf) {
                Ok(0) => return, // EOF — something took the terminal away.
                Ok(n) => {
                    if btx.send(buf[..n].to_vec()).is_err() {
                        return; // the face is leaving
                    }
                }
                // A signal landed mid-read. SIGWINCH does this on every
                // resize, so treating it as an error would kill the keyboard
                // the first time he dragged the window.
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    tracing::error!(error = %e, "terminal read failed");
                    return;
                }
            }
        }
    });
    brx
}

/// Terminal events, on a plain OS thread.
///
/// Not `spawn_blocking`: the tokio runtime waits for blocking tasks at
/// shutdown, and these sit in a read. OS threads the process simply leaves
/// behind cannot deadlock the exit path, and the exit path is the one that has
/// to restore his terminal.
pub fn spawn_pump(tx: mpsc::Sender<Ev>) {
    std::thread::spawn(move || {
        let bytes = spawn_reader();
        // ONE classifier and ONE parser for the life of the pump. A gesture
        // spans several reports and a paste spans several reads, so the state
        // that resolves either has to outlive any one of them — and both
        // belong here, beside the device, rather than in the coordinator.
        let mut gesture = Gesture::default();
        let mut parser = crate::vt::Parser::new();
        // The last size we told the loop about. **This is how a resize is seen
        // now**: crossterm delivered `Event::Resize` off its own SIGWINCH
        // pipe, and taking the read meant taking that too. Asking the terminal
        // its size on each turn is one `ioctl`, needs no signal handler, and
        // cannot be lost the way a coalesced signal can.
        let mut size = terminal::size().unwrap_or((0, 0));
        while pump_once(&tx, &bytes, &mut parser, &mut gesture, &mut size) == Pump::Continue {}
        let _ = tx.blocking_send(Ev::InputClosed);
    });
}

/// Whether the pump should keep reading. Its own type, because `bool` at a
/// call site does not say which way round it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pump {
    Continue,
    Stop,
}

/// The most inputs one batch may carry.
///
/// A ceiling rather than a policy: without it, a terminal handing over bytes
/// faster than this loop can drain them would build one unbounded batch and the
/// face would stop redrawing. An 800 KB paste is ONE `Input`, so this number is
/// about event STORMS — a drag across a wide window, or a key held down — and
/// not about paste size.
const BATCH_MAX: usize = 2048;

/// The longest the pump will sleep with nothing pending.
///
/// It is not an input latency — a byte arriving wakes the `recv_timeout`
/// immediately. It is the interval at which the terminal is asked its SIZE,
/// which is the one fact that no longer arrives as an event, and it is the same
/// 100 ms the old `event::poll` used so the wake-up rate did not change.
const IDLE_TICK: Duration = Duration::from_millis(100);

/// **One turn of the pump — wait, then DRAIN EVERYTHING ALREADY IN HAND,
/// translate, and send it as ONE batch.**
///
/// The drain is the whole point and it is the paste defence's foundation: the
/// loop collects exactly the events that came out of the bytes the terminal has
/// already produced. A paste and the keystrokes that were mis-parsed out of
/// that same paste therefore land in one batch, where `app::on_input` can
/// refuse to let any of them act.
///
/// **No human produces two keystrokes inside one drained read**, which is what
/// makes the rule safe as well as sufficient — tmux's own `assume-paste-time 1`
/// encodes the same assumption with the same millisecond.
///
/// **The timeout arm is not idling; it is the other half of the parser.** It is
/// where a lone `ESC` becomes the Escape key, where an unterminated paste is
/// folded in on its idle gap, and where one that never said anything at all is
/// abandoned. Under the old pump there was nothing on this path at all, which
/// is exactly why `IS-38` could not be closed from inside this crate.
///
/// Returns [`Pump::Stop`] when the channel is gone (the face is leaving) or
/// when the terminal stops answering — and the caller says so on the channel,
/// because a face that simply stopped responding to the keyboard is
/// indistinguishable from a hang.
fn pump_once(
    tx: &mpsc::Sender<Ev>,
    bytes: &std::sync::mpsc::Receiver<Vec<u8>>,
    parser: &mut crate::vt::Parser,
    gesture: &mut Gesture,
    size: &mut (u16, u16),
) -> Pump {
    use std::sync::mpsc::RecvTimeoutError;

    let now = Instant::now();
    let wait = parser.deadline(now).unwrap_or(IDLE_TICK).min(IDLE_TICK);
    let mut outs: Vec<crate::vt::Out> = Vec::new();

    match bytes.recv_timeout(wait) {
        Ok(chunk) => {
            parser.feed(&chunk, Instant::now(), &mut outs);
            // Everything else the reader has ALREADY handed over, in the same
            // batch. `try_recv`, never a waiting receive: this asks "is there
            // anything already in hand", and a waiting one here would glue a
            // paste to the keystroke that came after it, which would be the
            // batch rule doing harm.
            while outs.len() < BATCH_MAX {
                match bytes.try_recv() {
                    Ok(more) => parser.feed(&more, Instant::now(), &mut outs),
                    Err(_) => break,
                }
            }
        }
        Err(RecvTimeoutError::Timeout) => {
            parser.idle(Instant::now(), &mut outs);
            if tx.is_closed() {
                return Pump::Stop;
            }
        }
        // The reader thread is gone: EOF, or the device errored. Either way
        // there is no keyboard any more and the loop must be told.
        Err(RecvTimeoutError::Disconnected) => return Pump::Stop,
    }

    let mut batch: Vec<Input> = Vec::with_capacity(outs.len() + 1);
    // The size, asked once per turn. It leads the batch so a resize that
    // arrived with a keystroke re-lays the frame before the keystroke is drawn
    // into it.
    if let Ok(now_size) = terminal::size() {
        if now_size != *size {
            *size = now_size;
            batch.push(Input::Resize);
        }
    }
    for out in outs {
        match out {
            crate::vt::Out::Event(Event::Key(k)) => batch.push(Input::Key(k)),
            crate::vt::Out::Event(Event::Paste(s)) => batch.push(Input::Paste(s)),
            crate::vt::Out::Event(Event::Mouse(m)) => {
                if let Some(i) = gesture.feed(m) {
                    batch.push(i);
                }
            }
            crate::vt::Out::Event(_) => {}
            crate::vt::Out::PasteBegan => batch.push(Input::PasteBegan),
            crate::vt::Out::PasteFailed => batch.push(Input::PasteFailed),
            crate::vt::Out::PasteHealed => batch.push(Input::PasteHealed),
            crate::vt::Out::EnhancedKeys => batch.push(Input::EnhancedKeys),
        }
    }
    if batch.is_empty() {
        return Pump::Continue;
    }
    if tx.blocking_send(Ev::Input(batch)).is_err() {
        Pump::Stop
    } else {
        Pump::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn ctrl_lowercases() {
        assert_eq!(
            to_key(ev(KeyCode::Char('C'), KeyModifiers::CONTROL)),
            Some(Key::Ctrl('c'))
        );
    }

    #[test]
    fn plain_chars_keep_their_case() {
        assert_eq!(
            to_key(ev(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Some(Key::Char('G'))
        );
        assert_eq!(
            to_key(ev(KeyCode::Char('g'), KeyModifiers::NONE)),
            Some(Key::Char('g'))
        );
    }

    /// **EVERY KEY THE TABLE BINDS MUST HAVE A TRANSLATION**, or the binding
    /// is a dead letter that nothing reports. This is the test that would have
    /// caught `→` arriving as `None` for a whole build.
    #[test]
    fn every_key_the_table_names_can_be_produced_by_this_translation() {
        use ratatui::crossterm::event::KeyCode as C;
        let spellings = [
            (C::Up, Key::Up),
            (C::Down, Key::Down),
            (C::Left, Key::Left),
            (C::Right, Key::Right),
            (C::Enter, Key::Enter),
            (C::Esc, Key::Esc),
            (C::Tab, Key::Tab),
            (C::Home, Key::Home),
            (C::End, Key::End),
            (C::PageUp, Key::PageUp),
            (C::PageDown, Key::PageDown),
            (C::Backspace, Key::Backspace),
            (C::Delete, Key::Delete),
        ];
        // The MODIFIED keys, which are their own `Key` variants and which the
        // composer made necessary. Each is `(code, modifier, Key)`.
        let modified = [
            (C::Enter, KeyModifiers::ALT, Key::AltEnter),
            (C::Enter, KeyModifiers::SHIFT, Key::ShiftEnter),
            (C::Enter, KeyModifiers::CONTROL, Key::CtrlEnter),
            (C::Left, KeyModifiers::ALT, Key::AltLeft),
            (C::Right, KeyModifiers::ALT, Key::AltRight),
            (C::Home, KeyModifiers::ALT, Key::AltHome),
            (C::End, KeyModifiers::ALT, Key::AltEnd),
            // `0052`'s four limit keys and the word delete. Every one of these
            // was MEASURED arriving on his stack before it was bound — see
            // `motion_keys_probe.sh` and `the_measured_motion_wire_table`.
            (C::Left, KeyModifiers::CONTROL, Key::CtrlLeft),
            (C::Right, KeyModifiers::CONTROL, Key::CtrlRight),
            (C::Up, KeyModifiers::CONTROL, Key::CtrlUp),
            (C::Down, KeyModifiers::CONTROL, Key::CtrlDown),
            (C::Backspace, KeyModifiers::ALT, Key::AltBackspace),
            // **`0100`'s VERTICAL PAIR.** Same shape as the four above and
            // measured the same way: they were arriving and being flattened
            // onto the plain arrows one line up from where they were needed.
            (C::Up, KeyModifiers::ALT, Key::AltUp),
            (C::Down, KeyModifiers::ALT, Key::AltDown),
        ];
        for b in tui_render::BINDINGS {
            if let tui_render::KeySet::These(keys) = b.keys {
                for &want in keys {
                    let ok = match want {
                        Key::Char(c) => to_key(ev(C::Char(c), KeyModifiers::NONE)) == Some(want),
                        Key::Ctrl(c) => to_key(ev(C::Char(c), KeyModifiers::CONTROL)) == Some(want),
                        Key::Alt(c) => to_key(ev(C::Char(c), KeyModifiers::ALT)) == Some(want),
                        other => {
                            spellings.iter().any(|(code, k)| {
                                *k == other && to_key(ev(*code, KeyModifiers::NONE)) == Some(other)
                            }) || modified.iter().any(|(code, m, k)| {
                                *k == other && to_key(ev(*code, *m)) == Some(other)
                            })
                        }
                    };
                    assert!(ok, "{want:?} is bound in {:?} and cannot be typed", b.mode);
                }
            }
        }
    }

    #[test]
    fn a_key_with_no_spelling_is_none() {
        assert_eq!(to_key(ev(KeyCode::F(7), KeyModifiers::NONE)), None);
    }

    #[test]
    fn a_release_never_acts() {
        let mut e = ev(KeyCode::Char('q'), KeyModifiers::NONE);
        e.kind = KeyEventKind::Release;
        assert_eq!(to_key(e), None);
    }

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        at(kind, 10, 5)
    }

    fn at(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn down(c: u16, r: u16) -> MouseEvent {
        at(MouseEventKind::Down(MouseButton::Left), c, r)
    }
    fn moved(c: u16, r: u16) -> MouseEvent {
        at(MouseEventKind::Drag(MouseButton::Left), c, r)
    }
    fn up(c: u16, r: u16) -> MouseEvent {
        at(MouseEventKind::Up(MouseButton::Left), c, r)
    }

    /// A wheel notch is not a key and never becomes one — a `Key::WheelUp`
    /// would be a fiction the `?` overlay would then have to render.
    #[test]
    fn a_wheel_notch_is_a_scroll_and_a_click_carries_its_coordinate() {
        let mut g = Gesture::default();
        assert!(matches!(
            g.feed(mouse(MouseEventKind::ScrollUp)),
            Some(Input::Wheel { up: true })
        ));
        assert!(matches!(
            g.feed(mouse(MouseEventKind::ScrollDown)),
            Some(Input::Wheel { up: false })
        ));
        g.feed(down(10, 5));
        assert!(matches!(
            g.feed(up(10, 5)),
            Some(Input::Click { col: 10, row: 5 })
        ));
        assert!(g.feed(mouse(MouseEventKind::Moved)).is_none());
    }

    /// **CLICK AND DRAG ARE MUTUALLY EXCLUSIVE, AND THIS IS THE TEST THAT SAYS
    /// SO — IN BOTH DIRECTIONS.** It is the operator's literal requirement
    /// (*"It needs to be treated separately from a click"*), and it is the one
    /// property of this file that a later edit could break while every other
    /// test stayed green.
    #[test]
    fn a_drag_never_clicks_and_a_click_never_drags() {
        // A DRAG: press, motion, release. No `Click`, ever.
        let mut g = Gesture::default();
        assert!(matches!(g.feed(down(4, 4)), Some(Input::Press { .. })));
        assert!(matches!(
            g.feed(moved(9, 4)),
            Some(Input::Drag { col: 9, row: 4 })
        ));
        assert!(matches!(
            g.feed(moved(9, 7)),
            Some(Input::Drag { col: 9, row: 7 })
        ));
        let end = g.feed(up(9, 7));
        assert!(
            matches!(end, Some(Input::DragEnd { col: 9, row: 7 })),
            "a drag produced {end:?} — a drag that activates is the defect"
        );

        // A CLICK: press, release, nothing between. No `Drag`, ever.
        let mut g = Gesture::default();
        assert!(matches!(g.feed(down(4, 4)), Some(Input::Press { .. })));
        let end = g.feed(up(4, 4));
        assert!(
            matches!(end, Some(Input::Click { col: 4, row: 4 })),
            "a click produced {end:?}"
        );

        // A SHAKY click — a motion report that never left the anchor cell —
        // is still a click. Otherwise every imprecise press would swallow its
        // own activation and select one character instead.
        let mut g = Gesture::default();
        g.feed(down(4, 4));
        assert!(
            g.feed(moved(4, 4)).is_none(),
            "a same-cell report is not motion"
        );
        assert!(matches!(g.feed(up(4, 4)), Some(Input::Click { .. })));

        // …but a drag that came BACK to the anchor cell is still a drag. His
        // hand moved; he did not mean to click.
        let mut g = Gesture::default();
        g.feed(down(4, 4));
        g.feed(moved(20, 9));
        g.feed(moved(4, 4));
        assert!(matches!(g.feed(up(4, 4)), Some(Input::DragEnd { .. })));
    }

    /// **THE PRESS STILL DOES NOT ACT.** It anchors, and that is all. A later
    /// edit that makes `Down` produce a `Click` has to delete this.
    #[test]
    fn a_press_anchors_and_never_activates() {
        let mut g = Gesture::default();
        assert!(
            matches!(g.feed(down(10, 5)), Some(Input::Press { col: 10, row: 5 })),
            "a press activated — a drag would now fire an activation too"
        );
        // The other buttons are RESERVED and bound to nothing: `DIR-14` says
        // stop adding bindings on reflex, and double-click-selects-word is the
        // first thing that will want one of them.
        assert!(g
            .feed(mouse(MouseEventKind::Up(MouseButton::Right)))
            .is_none());
        assert!(g
            .feed(mouse(MouseEventKind::Up(MouseButton::Middle)))
            .is_none());
        assert!(g
            .feed(at(MouseEventKind::Drag(MouseButton::Right), 3, 3))
            .is_none());
    }

    /// Motion with no press behind it is nothing, and a release with no press
    /// behind it is nothing. Both arrive in real life: the gesture can begin
    /// before this program claims the mouse.
    #[test]
    fn a_gesture_we_did_not_see_the_start_of_produces_nothing() {
        let mut g = Gesture::default();
        assert!(g.feed(moved(5, 5)).is_none());
        assert!(g.feed(up(5, 5)).is_none());
    }

    fn key(c: KeyCode) -> Input {
        Input::Key(KeyEvent::new(c, KeyModifiers::NONE))
    }
    fn modkey(c: KeyCode, m: KeyModifiers) -> Input {
        Input::Key(KeyEvent::new(c, m))
    }
    fn pasted(batch: Vec<Input>) -> Option<String> {
        defuse(batch).into_iter().find_map(|i| match i {
            Input::Paste(s) => Some(s),
            _ => None,
        })
    }
    fn acts(batch: Vec<Input>) -> usize {
        defuse(batch)
            .iter()
            .filter(|i| matches!(i, Input::Key(_)))
            .count()
    }

    /// **THE REPRODUCED DEFECT, AND THE PROOF IT CANNOT SEND.**
    ///
    /// A paste whose content contains `ESC[201~` is truncated by crossterm and
    /// its tail delivered as ordinary key events — the exact byte sequence
    /// `\x1b[200~hello\x1b[201~\x1b[201~ rm -rf\r` produced `Paste("hello")`
    /// followed by ` `, `r`, `m`, ` `, `-`, `r`, `f` as key presses and then
    /// ENTER. Under "enter sends" that dispatches a message he did not write.
    ///
    /// After `defuse` there is no key event left in the batch at all: the tail
    /// is folded back into the paste, the Enter is a newline inside it, and the
    /// one thing that reaches the composer is text.
    #[test]
    fn a_paste_carrying_its_own_terminator_cannot_send_and_loses_nothing() {
        let batch = vec![
            Input::Paste("hello".into()),
            key(KeyCode::Char(' ')),
            key(KeyCode::Char('r')),
            key(KeyCode::Char('m')),
            key(KeyCode::Char(' ')),
            key(KeyCode::Char('-')),
            key(KeyCode::Char('r')),
            key(KeyCode::Char('f')),
            key(KeyCode::Enter),
        ];
        assert_eq!(
            acts(batch.clone()),
            0,
            "a keystroke survived a paste batch — one of them was an ENTER, and \
             enter SENDS"
        );
        assert_eq!(
            pasted(batch).as_deref(),
            Some("hello rm -rf\n"),
            "the tail was dropped rather than folded back — that is his clipboard, lost"
        );
    }

    /// **N ≥ 2, per `core:writing-tools`.** The batch rule ARBITRATES between
    /// two sources — "is this Enter his, or is it paste residue?" — and a thing
    /// that arbitrates is proven at two or more interleavings or it is not
    /// proven: its single-event case cannot exercise the contention it exists
    /// to manage.
    #[test]
    fn the_arbiter_is_proven_at_both_interleavings() {
        // (1) PASTE THEN ENTER — the defect's own order. Nothing acts.
        let paste_first = vec![Input::Paste("one".into()), key(KeyCode::Enter)];
        assert_eq!(acts(paste_first.clone()), 0);
        assert_eq!(pasted(paste_first).as_deref(), Some("one\n"));

        // (2) ENTER THEN PASTE — the other order, and it must be the OTHER
        // answer: an enter that arrived BEFORE any paste is his, and it sends.
        // A rule that refused both would be a face that cannot send while the
        // clipboard is busy.
        let enter_first = vec![key(KeyCode::Enter), Input::Paste("two".into())];
        assert_eq!(
            acts(enter_first.clone()),
            1,
            "an enter that preceded the paste was swallowed — the rule is too wide"
        );
        assert_eq!(pasted(enter_first).as_deref(), Some("two"));

        // (3) …and interleaved both ways in one batch, which is the case a
        // single-direction test would pass while still being wrong.
        let both = vec![
            key(KeyCode::Enter),
            Input::Paste("a".into()),
            key(KeyCode::Enter),
            Input::Paste("b".into()),
            key(KeyCode::Char('!')),
        ];
        assert_eq!(acts(both.clone()), 1, "only the leading enter may act");
        assert_eq!(pasted(both).as_deref(), Some("a\nb!"));
    }

    /// **NOTHING IN A PASTE TAIL MAY REACH A COMMAND, not only `enter`.**
    ///
    /// This is the half a patch to crossterm would not have covered. A stray
    /// `\x1b` in the residue parses as an ALT key: `^Q` would QUEUE his
    /// message and `^C` would QUIT, both out of a clipboard he only meant to
    /// paste. They are dropped rather than folded, because they are not text.
    #[test]
    fn a_control_sequence_in_a_paste_tail_reaches_no_command() {
        let batch = vec![
            Input::Paste("payload".into()),
            modkey(KeyCode::Char('q'), KeyModifiers::ALT),
            modkey(KeyCode::Char('c'), KeyModifiers::CONTROL),
            modkey(KeyCode::Char('f'), KeyModifiers::ALT),
            key(KeyCode::Esc),
            key(KeyCode::Char('x')),
        ];
        assert_eq!(acts(batch.clone()), 0, "a command survived a paste batch");
        assert_eq!(
            pasted(batch).as_deref(),
            Some("payloadx"),
            "the modified keys must be DROPPED, not folded in as letters"
        );
    }

    /// **THE BACKSTOP FOR A TERMINAL THAT IGNORES `?2004h`.** With no paste
    /// event at all, a burst of text keys in ONE drained read is still a paste
    /// — and the trailing carriage return still must not send.
    #[test]
    fn a_keystroke_burst_with_no_paste_event_is_still_treated_as_a_paste() {
        let mut batch: Vec<Input> = "pasted line"
            .chars()
            .map(|c| key(KeyCode::Char(c)))
            .collect();
        batch.push(key(KeyCode::Enter));
        assert!(batch.len() >= PASTE_BURST);
        assert_eq!(acts(batch.clone()), 0);
        assert_eq!(pasted(batch).as_deref(), Some("pasted line\n"));
    }

    /// **WHERE HOLD-TO-SPEAK MEETS THE PASTE BACKSTOP, MEASURED RATHER THAN
    /// ASSUMED — AND THE ANSWER IS NOT THE COMFORTABLE ONE.**
    ///
    /// `0095` checked `PASTE_BURST` against this gesture and cleared it: at a
    /// measured 51–112 ms inter-arrival, eight auto-repeats span about 700 ms
    /// and `pump_once` drains with `try_recv`, whose own comment is *"no human
    /// produces two keystrokes inside one drained read"*. That reasoning is
    /// right about the KEYBOARD and it says nothing about the READER: if this
    /// loop is behind by three quarters of a second — a long paint, a big
    /// projection — eight repeats ARE sitting in the channel and they do land
    /// in one batch. This test pins what happens then, so the next reader
    /// finds a measurement instead of an assumption.
    ///
    /// **A run of held spaces IS folded into a paste**, because a run of
    /// spaces is exactly what a pasted indent looks like and nothing in a byte
    /// stream distinguishes them. In mic mode that paste is then DROPPED —
    /// `Mode::Mic` is not `textarea_is_live` — so the cost is repeats lost
    /// from the middle of a hold, which can end the gesture early and split
    /// one utterance into two. It is not a swallowed command and it cannot
    /// type spaces into his draft.
    ///
    /// **THE BACKSTOP IS NOT WEAKENED TO SUIT THE GESTURE.** It is the defence
    /// against a clipboard that dispatches a message he did not write; trading
    /// that for a smoother hold under load would be the wrong side of the
    /// trade, and `0046` forbids adding a downstream corrector for it.
    #[test]
    fn a_stalled_loop_folds_a_held_key_into_a_paste_and_that_is_the_known_cost() {
        let held: Vec<Input> = (0..PASTE_BURST).map(|_| key(KeyCode::Char(' '))).collect();
        assert_eq!(
            acts(held.clone()),
            0,
            "a stalled read of held spaces reached the act path"
        );
        assert_eq!(
            pasted(held).as_deref(),
            Some("        "),
            "the fold changed shape — re-read this test's argument before relying on it"
        );
        // One repeat short of the threshold is still a run of keystrokes, so
        // a hold whose repeats arrive at anything like a human read rate is
        // untouched by the rule.
        let fewer: Vec<Input> = (0..PASTE_BURST - 1)
            .map(|_| key(KeyCode::Char(' ')))
            .collect();
        assert_eq!(
            acts(fewer),
            PASTE_BURST - 1,
            "the backstop fired below its own threshold"
        );
    }

    /// **AND THE BACKSTOP MUST NOT FIRE ON TYPING OR ON NAVIGATION.** This is
    /// the half that makes it safe rather than merely strong: a held arrow key
    /// produces a long burst too, and swallowing it would break scrolling.
    #[test]
    fn a_burst_of_navigation_keys_is_never_mistaken_for_a_paste() {
        let arrows: Vec<Input> = (0..20).map(|_| key(KeyCode::Down)).collect();
        assert_eq!(
            acts(arrows.clone()),
            20,
            "a held arrow key was eaten as a paste"
        );
        assert!(pasted(arrows).is_none());

        // A short burst of letters — a fast typist inside one read — is typing.
        let typing: Vec<Input> = "abc".chars().map(|c| key(KeyCode::Char(c))).collect();
        assert_eq!(acts(typing.clone()), 3);
        assert!(pasted(typing).is_none());

        // A mouse gesture is never folded, whatever else is in the batch: the
        // drag that just landed depends on it.
        let mixed = vec![
            Input::Paste("x".into()),
            Input::Drag { col: 4, row: 4 },
            Input::DragEnd { col: 9, row: 4 },
        ];
        let out = defuse(mixed);
        assert!(matches!(out[1], Input::Drag { .. }));
        assert!(matches!(out[2], Input::DragEnd { .. }));
    }

    /// **THE PASTE STATE MARKERS SURVIVE THE BATCH RULE AND NEVER BECOME
    /// TEXT.** `0050` added four non-key, non-text `Input` variants, and
    /// `defuse` folds everything it does not recognise as text into a paste or
    /// drops it — so "the catch-all arm happens to do the right thing" is a
    /// property worth a test rather than a property worth a comment. If
    /// `PasteBegan` were ever folded, `app::paste_notice` would stop seeing it
    /// and the in-progress notice would silently never appear again.
    #[test]
    fn the_paste_markers_pass_through_the_batch_rule_untouched() {
        let batch = vec![
            Input::PasteBegan,
            Input::Paste("clipboard".into()),
            key(KeyCode::Char('!')),
            Input::PasteHealed,
        ];
        let out = defuse(batch);
        assert_eq!(
            pasted(vec![
                Input::PasteBegan,
                Input::Paste("clipboard".into()),
                key(KeyCode::Char('!')),
                Input::PasteHealed,
            ])
            .as_deref(),
            Some("clipboard!"),
            "the markers were folded into the pasted text"
        );
        assert!(
            out.iter().any(|i| matches!(i, Input::PasteBegan)),
            "PasteBegan was eaten: the in-progress notice can never fire"
        );
        assert!(
            out.iter().any(|i| matches!(i, Input::PasteHealed)),
            "PasteHealed was eaten: a gap-closed paste would land with no word said"
        );

        // A marker ALONE is a legal batch and must come out as it went in —
        // this is the shape `CSI 200~` with no content produces, and it is the
        // one that carries the whole of IS-38's visible half.
        for marker in [Input::PasteBegan, Input::PasteFailed, Input::EnhancedKeys] {
            let out = defuse(vec![marker.clone()]);
            assert_eq!(out.len(), 1, "{marker:?} did not survive alone: {out:?}");
        }
    }

    /// A REPEAT never acts, and that matters now that `enter` SENDS: a key held
    /// a fraction too long under the kitty protocol would otherwise fire a
    /// `threads/{id}/send` per repeat.
    #[test]
    fn a_repeat_never_acts_either() {
        let mut e = ev(KeyCode::Enter, KeyModifiers::NONE);
        e.kind = KeyEventKind::Repeat;
        assert_eq!(to_key(e), None, "a key repeat would fire a second send");
    }

    // There is deliberately NO unit test of `pump_once` here. It calls
    // `event::poll` on the real terminal, and a test that reads the device
    // could swallow a keystroke out of the terminal running `cargo test` —
    // the same class of side effect that kept the DECSET writes out of
    // `App::on_intent`. The pump's behaviour is proven by running the face.
}
