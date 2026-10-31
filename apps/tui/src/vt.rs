//! **THE PRE-PARSE LAYER — this program owns its bytes before anything else
//! sees them.**
//!
//! # Why this file exists at all
//!
//! Two defects (`IS-33`, `IS-38`) were left open by the composer build for the
//! same structural reason, and it was not a reason about our code: **crossterm
//! read the file descriptor, crossterm held the buffer, and there was no API
//! that reached into it.**
//!
//! * `IS-33` — tmux with `extended-keys-format xterm` (the DEFAULT) answers
//!   `modifyOtherKeys` with `CSI 27 ; <mod> ; <code> ~`. crossterm 0.28.1 has
//!   **no match arm for a leading 27**, so `parse_csi_special_key_code` errors,
//!   `Parser::advance` calls `self.buffer.clear()`, and the keystroke is gone.
//!   `shift-enter` therefore arrived byte-identical to `enter` — and `enter`
//!   SENDS.
//! * `IS-38` — a bracketed paste whose `CSI 201~` terminator never arrives
//!   leaves crossterm holding every subsequent byte in that same private
//!   buffer, waiting. The face kept drawing, but **the keyboard was deaf** until
//!   a terminator happened to arrive.
//!
//! Both are the same sentence: *somebody else is holding bytes addressed to
//! us.* So this module takes the read.
//!
//! # Why a WHOLE parser, and not a shim in front of crossterm's
//!
//! The obvious cheaper design is to recognise only the sequences crossterm
//! cannot handle and hand the rest to its parser. **It is not available.**
//! `crossterm::event::sys::unix::parse::parse_event` is `pub(crate)` on 0.28.1
//! and on master; there is no public entry point that takes a byte slice and
//! returns an `Event`. The only ways to reach it are a fork (`Q-16`, which
//! needs a `[patch.crates-io]` in the workspace root this crate does not own)
//! or driving crossterm through a proxy pty, which means a second termios, a
//! second SIGWINCH path, and a `dup2` in a crate whose manifest says
//! `unsafe_code = "forbid"`.
//!
//! So the grammar is implemented here. **The output type is deliberately
//! crossterm's own `Event`**, which is what keeps the blast radius at this
//! file: [`crate::input::to_key`], [`crate::input::defuse`] and
//! [`crate::input::Gesture`] are unchanged, every test they carry still runs
//! against the same types, and nothing above `input` learned a new word.
//!
//! # What it is NOT
//!
//! It is not a terminal emulator and it is not a general VT parser. It parses
//! exactly what a keyboard, a mouse claiming [`crate::term::CLAIMS`], and a
//! bracketed paste can produce, and it SKIPS everything else (device replies,
//! OSC answers, focus reports) rather than guessing. An unknown sequence is
//! consumed and dropped, never printed into his composer as text.
//!
//! # The three timing rules, which are the whole of `IS-38`'s fix
//!
//! A parser over a byte stream has to decide when an incomplete thing is never
//! going to be completed. crossterm's answer is "wait forever", and that is the
//! deafness. Ours is three deadlines, all measured from the last byte that
//! arrived, and all of them owned by data this file can see:
//!
//! | rule | window | what it resolves |
//! |---|---|---|
//! | [`ESC_WINDOW`] | 50 ms | a lone `ESC` is the Escape KEY, not the start of a sequence |
//! | [`SEQ_WINDOW`] | 1 s | a truncated sequence is flushed rather than held forever |
//! | [`PASTE_IDLE`] | 1 s | a paste with content that has gone quiet is COMPLETE |
//! | [`PASTE_SILENT`] | 3 s | a paste that opened and never said anything has FAILED |
//!
//! **`PASTE_IDLE` and `PASTE_SILENT` do not overlap, and that is the reason
//! there is no false-timeout on a big slow paste.** Both are measured in
//! SILENCE, never in elapsed time: an 800 KB paste dribbling in over a minute
//! of a bad link never goes quiet for a second, so neither rule fires while it
//! is still arriving. The only thing that can reach `PASTE_SILENT` is a paste
//! that opened and produced no content at all, which is precisely the case
//! `PASTE_IDLE` cannot judge — an empty run of silence after `CSI 200~` is
//! indistinguishable from "his clipboard was empty" and must not be folded in
//! as a completed paste.
//!
//! **A timed-out paste still keeps what arrived.** `0042` set the precedent and
//! it is not weakened here: the tail of a corrupted paste is his clipboard, and
//! dropping it loses his text silently. Timing out says so in the notice area;
//! it does not throw the text away.
//!
//! # No clock inside
//!
//! Every entry point takes `now: Instant` from its caller. That is not purity
//! theatre — it is what makes the four rules above testable at all. A timeout
//! whose proof needs a real second elapsing is a proof nobody runs.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

/// How long a lone `ESC` waits before it is the Escape KEY.
///
/// crossterm decides this from `input_available` — whether the same `read()`
/// returned more bytes — rather than from a clock, which `crossterm#993`
/// records as misfiring on every byte-at-a-time transport. A window is the
/// honest form: a real `CSI` sequence follows its `ESC` within microseconds on
/// the same read, and 50 ms is three orders of magnitude above that while still
/// being under the threshold at which a person feels a key was slow.
pub const ESC_WINDOW: Duration = Duration::from_millis(50);

/// How long an incomplete-but-plausible sequence is held before it is flushed
/// as an `ESC` followed by its literal bytes.
///
/// **This is the general form of `IS-38`, and it is the reason this file cannot
/// reproduce that defect in any other sequence either.** crossterm holds a
/// truncated `CSI` forever, exactly as it holds an unterminated paste forever;
/// the deafness was never specific to paste. A second is far longer than any
/// real sequence takes to finish and far shorter than a keyboard being dead.
pub const SEQ_WINDOW: Duration = Duration::from_secs(1);

/// **His rule, verbatim:** *"if you detect raw characters after the paste
/// starts, and then all of a sudden there's a gap of 1s since the last
/// character, it's probably done."*
///
/// **THE SAME RULE HAS A SECOND READER NOW, AND IT IS NOT IN THIS FILE.**
/// Hold-to-speak ends its gesture on an idle gap for exactly this reason —
/// there is no key-release event in his terminal (`0095`: tmux 3.5a never
/// forwards the kitty push to iTerm2, and only `modifyOtherKeys`, which has no
/// release concept, survives). Its thresholds are [`crate::mic::HOLD_MIN_GAP`]
/// and its neighbours. They live beside the machine that reads them rather
/// than beside this constant, because that machine is fed an ACT in one mode
/// and this file knows only bytes; the cross-reference is the join.
pub const PASTE_IDLE: Duration = Duration::from_secs(1);

/// How long a paste that has produced NO CONTENT AT ALL waits before it is
/// declared failed. See the module header for why this is a separate window
/// from [`PASTE_IDLE`] rather than a larger value of it.
pub const PASTE_SILENT: Duration = Duration::from_secs(3);

/// One thing the parser decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    /// A terminal event, in crossterm's own vocabulary so that nothing above
    /// this module has to learn a second one.
    Event(Event),
    /// `CSI 200~` was seen and the paste is now open. Emitted at once, so the
    /// face can say a paste is in progress — and, because the pump batches a
    /// whole read, a paste that completes immediately delivers this and its
    /// `Event::Paste` in the SAME batch, which is what keeps a fast paste from
    /// flashing a notice for one frame.
    PasteBegan,
    /// A paste opened, said nothing for [`PASTE_SILENT`], and was abandoned.
    /// The keyboard is live again; the face owes him a notice saying so.
    PasteFailed,
    /// A paste was completed by the idle-gap rule rather than by a terminator.
    /// Carried separately from the `Event::Paste` that accompanies it because
    /// it is a different FACT — his clipboard landed, and the terminal never
    /// said it was finished.
    PasteHealed,
    /// **THE TERMINAL ANSWERED THE KITTY-KEYBOARD QUERY** (`CSI ? <flags> u`),
    /// which is the only evidence there is that it can report `shift-enter`
    /// apart from `enter` outside tmux.
    ///
    /// It is an EVENT rather than the return value of a probe, and that is the
    /// point. crossterm's `supports_keyboard_enhancement` writes the query and
    /// then BLOCKS READING STDIN for up to two seconds waiting for the answer.
    /// With this module owning the descriptor there would be two readers on one
    /// fd, and the loser eats the winner's bytes — measured: it swallowed the
    /// first keystroke of the session. A reply that arrives as an ordinary
    /// event has no second reader, no timeout, and no startup cost.
    EnhancedKeys,
}

/// Where an open bracketed paste has got to.
#[derive(Debug)]
struct Paste {
    /// What has arrived since `CSI 200~`. Bytes rather than a `String`: a
    /// paste can be split mid-codepoint across reads, and decoding is deferred
    /// to the one place that has the whole of it.
    text: Vec<u8>,
    /// When the most recent CONTENT byte arrived. The two paste windows are
    /// both measured from here.
    last: Instant,
}

/// **The byte-stream parser. One per input pump, for the life of the session.**
#[derive(Debug)]
pub struct Parser {
    buf: Vec<u8>,
    /// When the oldest unconsumed byte in `buf` arrived — the anchor for
    /// [`ESC_WINDOW`] and [`SEQ_WINDOW`].
    buf_since: Option<Instant>,
    paste: Option<Paste>,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

/// What one parse step did with the front of the buffer.
enum Step {
    /// Consumed `n` bytes; `out` is what they meant, if anything.
    Took(usize, Option<Out>),
    /// The front of the buffer is a valid PREFIX and needs more bytes.
    Want,
}

impl Parser {
    pub fn new() -> Self {
        Parser {
            buf: Vec::new(),
            buf_since: None,
            paste: None,
        }
    }

    /// Whether a bracketed paste is open right now.
    pub fn pasting(&self) -> bool {
        self.paste.is_some()
    }

    /// Bytes in, events out.
    pub fn feed(&mut self, bytes: &[u8], now: Instant, out: &mut Vec<Out>) {
        if bytes.is_empty() {
            return;
        }
        self.buf.extend_from_slice(bytes);
        if self.buf_since.is_none() {
            self.buf_since = Some(now);
        }
        self.drain(now, out);
    }

    /// **Nothing has arrived for a while — resolve what the silence means.**
    ///
    /// This is the half crossterm does not have, and every one of the four
    /// timing rules lands here.
    pub fn idle(&mut self, now: Instant, out: &mut Vec<Out>) {
        // 1. The paste rules, first: an open paste is why the keyboard would
        //    otherwise be deaf, so it is the state that most needs resolving.
        if let Some(p) = &self.paste {
            let quiet = now.saturating_duration_since(p.last);
            if !p.text.is_empty() && quiet >= PASTE_IDLE {
                let p = self.paste.take().expect("checked just above");
                tracing::info!(
                    bytes = p.text.len(),
                    "a paste went quiet without a terminator; folding it in (IS-38 idle-gap)"
                );
                out.push(Out::PasteHealed);
                out.push(Out::Event(Event::Paste(decode(&p.text))));
            } else if p.text.is_empty() && quiet >= PASTE_SILENT {
                self.paste = None;
                tracing::warn!("a paste opened and said nothing; abandoning it (IS-38 timeout)");
                out.push(Out::PasteFailed);
            }
            // Bytes buffered behind an open paste cannot be parsed as keys —
            // they are paste content — so there is nothing else to resolve.
            if self.paste.is_some() {
                return;
            }
            // The paste just closed. Whatever is left in `buf` is ordinary
            // input again and falls through to the drain below.
            self.drain(now, out);
            return;
        }

        // 2. A held escape sequence.
        let Some(since) = self.buf_since else { return };
        if self.buf.is_empty() || self.buf[0] != 0x1b {
            return;
        }
        let held = now.saturating_duration_since(since);
        if self.buf.len() == 1 {
            if held >= ESC_WINDOW {
                self.buf.clear();
                self.buf_since = None;
                out.push(key(KeyCode::Esc, KeyModifiers::NONE));
            }
            return;
        }
        if held >= SEQ_WINDOW {
            // A sequence that was started and never finished. Emit the `ESC`
            // as a key and let the remainder be re-parsed as ordinary bytes —
            // which is what a terminal does, and which is strictly better than
            // holding the keyboard shut waiting for a byte that is not coming.
            tracing::warn!(
                bytes = self.buf.len(),
                "a truncated escape sequence was flushed rather than held"
            );
            self.buf.remove(0);
            self.buf_since = Some(now);
            out.push(key(KeyCode::Esc, KeyModifiers::NONE));
            self.drain(now, out);
        }
    }

    /// How long until this parser's next timing decision, if it has one.
    ///
    /// The pump waits on THIS rather than on a fixed tick, so a face with
    /// nothing pending sleeps exactly as long as it always did and a face with
    /// a half-typed escape wakes when it has something to decide.
    pub fn deadline(&self, now: Instant) -> Option<Duration> {
        if let Some(p) = &self.paste {
            let window = if p.text.is_empty() {
                PASTE_SILENT
            } else {
                PASTE_IDLE
            };
            return Some(window.saturating_sub(now.saturating_duration_since(p.last)));
        }
        let since = self.buf_since?;
        if self.buf.first() != Some(&0x1b) {
            return None;
        }
        let window = if self.buf.len() == 1 {
            ESC_WINDOW
        } else {
            SEQ_WINDOW
        };
        Some(window.saturating_sub(now.saturating_duration_since(since)))
    }

    /// Consume as much of the buffer as can be decided right now.
    fn drain(&mut self, now: Instant, out: &mut Vec<Out>) {
        loop {
            if self.paste.is_some() {
                if !self.eat_paste(now, out) {
                    return;
                }
                continue;
            }
            if self.buf.is_empty() {
                self.buf_since = None;
                return;
            }
            match self.step() {
                Step::Took(n, ev) => {
                    debug_assert!(n > 0, "a zero-width step would spin");
                    self.buf.drain(..n);
                    self.buf_since = if self.buf.is_empty() { None } else { Some(now) };
                    if let Some(ev) = ev {
                        out.push(ev);
                    }
                    if matches!(out.last(), Some(Out::PasteBegan)) {
                        // The rest of the buffer is paste content.
                        self.paste = Some(Paste {
                            text: Vec::new(),
                            last: now,
                        });
                    }
                }
                Step::Want => return,
            }
        }
    }

    /// Move buffered bytes into the open paste, and close it if the terminator
    /// is in hand. Returns whether the paste ENDED (so the drain loop should
    /// go back to parsing keys).
    ///
    /// **It ends at the FIRST terminator, exactly as crossterm does, and that
    /// is deliberate.** A paste whose CONTENT contains `ESC[201~` is
    /// indistinguishable from one that ended there — no layer can tell them
    /// apart, ours included. The tail is then ordinary input in the same
    /// batch, where [`crate::input::defuse`] folds it back into the paste it
    /// came from. Changing the split here would break that rule's only proof
    /// without making anything more correct.
    fn eat_paste(&mut self, now: Instant, out: &mut Vec<Out>) -> bool {
        const END: &[u8] = b"\x1b[201~";
        let p = self.paste.as_mut().expect("called with a paste open");
        if let Some(i) = find(&self.buf, END) {
            p.text.extend_from_slice(&self.buf[..i]);
            self.buf.drain(..i + END.len());
            self.buf_since = if self.buf.is_empty() { None } else { Some(now) };
            let done = self.paste.take().expect("held above");
            out.push(Out::Event(Event::Paste(decode(&done.text))));
            return true;
        }
        // No terminator yet. Keep back the longest suffix that could still be
        // the START of one — otherwise a terminator split across two reads
        // would be pasted into his composer as literal text.
        let keep = partial_suffix(&self.buf, END);
        let take = self.buf.len() - keep;
        if take > 0 {
            p.text.extend_from_slice(&self.buf[..take]);
            p.last = now;
            self.buf.drain(..take);
            self.buf_since = if self.buf.is_empty() { None } else { Some(now) };
        }
        false
    }

    /// Decide the front of the buffer. Never called with an empty buffer and
    /// never called with a paste open.
    fn step(&self) -> Step {
        let b = &self.buf[..];
        match b[0] {
            0x1b => self.escape(),
            b'\r' => Step::Took(1, Some(key(KeyCode::Enter, KeyModifiers::NONE))),
            b'\t' => Step::Took(1, Some(key(KeyCode::Tab, KeyModifiers::NONE))),
            0x7f => Step::Took(1, Some(key(KeyCode::Backspace, KeyModifiers::NONE))),
            0x00 => Step::Took(1, Some(key(KeyCode::Char(' '), KeyModifiers::CONTROL))),
            // `\n` is 0x0A and lands here: in RAW MODE the terminal no longer
            // translates CR, so a `\n` on the wire is `^J` and nothing else.
            // crossterm reaches the same answer by the same route.
            c @ 0x01..=0x1a => Step::Took(
                1,
                Some(key(
                    KeyCode::Char((c - 1 + b'a') as char),
                    KeyModifiers::CONTROL,
                )),
            ),
            c @ 0x1c..=0x1f => Step::Took(
                1,
                Some(key(
                    KeyCode::Char((c - 0x1c + b'4') as char),
                    KeyModifiers::CONTROL,
                )),
            ),
            _ => match utf8(b) {
                Utf8::Char(c, n) => Step::Took(n, Some(key(KeyCode::Char(c), KeyModifiers::NONE))),
                // Invalid UTF-8 is a byte we cannot name. Dropping it is the
                // only honest act: guessing a codepoint puts a character he
                // never typed into his message.
                Utf8::Bad => Step::Took(1, None),
                Utf8::Want => Step::Want,
            },
        }
    }

    /// Everything that begins `ESC`.
    fn escape(&self) -> Step {
        let b = &self.buf[..];
        if b.len() == 1 {
            return Step::Want;
        }
        match b[1] {
            b'[' => self.csi(),
            b'O' => {
                if b.len() < 3 {
                    return Step::Want;
                }
                let code = match b[2] {
                    b'A' => KeyCode::Up,
                    b'B' => KeyCode::Down,
                    b'C' => KeyCode::Right,
                    b'D' => KeyCode::Left,
                    b'H' => KeyCode::Home,
                    b'F' => KeyCode::End,
                    v @ b'P'..=b'S' => KeyCode::F(1 + v - b'P'),
                    _ => return Step::Took(3, None),
                };
                Step::Took(3, Some(key(code, KeyModifiers::NONE)))
            }
            // A string sequence the terminal sent US — an OSC reply, a DCS, an
            // APC. Consumed whole and dropped: it is an answer to a question,
            // not a keystroke, and letting its body fall through would type its
            // payload into his composer.
            b']' | b'P' | b'X' | b'^' | b'_' => match string_seq_len(b) {
                Some(n) => Step::Took(n, None),
                None => Step::Want,
            },
            // `ESC ESC`. One Escape, both bytes — crossterm's answer, kept so
            // that a face driven by a script that sends it behaves the same.
            0x1b => Step::Took(2, Some(key(KeyCode::Esc, KeyModifiers::NONE))),
            // `ESC <anything>` is that thing with ALT held. Parse the tail and
            // add the modifier.
            _ => {
                let mut tail = Parser::new();
                tail.buf = b[1..].to_vec();
                match tail.step() {
                    Step::Want => Step::Want,
                    Step::Took(n, ev) => Step::Took(n + 1, ev.map(alt)),
                }
            }
        }
    }

    /// `ESC [ …` — the one that carries the whole grammar.
    fn csi(&self) -> Step {
        let b = &self.buf[..];
        // The legacy X10 mouse report is the one CSI whose body is BINARY and
        // therefore cannot be scanned for a final byte: `CSI M` then three
        // bytes that may be anything at all. It is checked before the scan for
        // exactly that reason. `?1006h` is claimed so SGR is what actually
        // arrives, but a terminal that ignored the claim still reports.
        if b.len() >= 3 && b[2] == b'M' {
            if b.len() < 6 {
                return Step::Want;
            }
            return Step::Took(6, legacy_mouse(b[3], b[4], b[5]));
        }
        // Parameters, intermediates, final.
        let mut i = 2;
        while i < b.len() && matches!(b[i], b'0'..=b'9' | b';' | b':' | b'<' | b'>' | b'?' | b'!') {
            i += 1;
        }
        while i < b.len() && (0x20..=0x2f).contains(&b[i]) {
            i += 1;
        }
        if i >= b.len() {
            return Step::Want;
        }
        let final_byte = b[i];
        if !(0x40..=0x7e).contains(&final_byte) {
            // Not a legal final byte: this is not a CSI after all. Drop the
            // `ESC` and let the rest be re-read as ordinary bytes.
            return Step::Took(1, None);
        }
        let params = &b[2..i];
        let n = i + 1;

        // SGR mouse: `CSI < b ; x ; y M|m`.
        if params.first() == Some(&b'<') && (final_byte == b'M' || final_byte == b'm') {
            return Step::Took(n, sgr_mouse(&params[1..], final_byte == b'M'));
        }
        // A private-mode REPLY is the terminal answering a probe, never a key.
        // `CSI ? <flags> u` is the ONE reply this face asked a question to get
        // — see [`Out::EnhancedKeys`]. Every other one is consumed and dropped.
        if params.first() == Some(&b'?') {
            if final_byte == b'u' {
                return Step::Took(n, Some(Out::EnhancedKeys));
            }
            return Step::Took(n, None);
        }

        let nums = numbers(params);
        match final_byte {
            b'~' => Step::Took(n, tilde(&nums)),
            b'A' | b'B' | b'C' | b'D' | b'H' | b'F' | b'E' => {
                let code = match final_byte {
                    b'A' => KeyCode::Up,
                    b'B' => KeyCode::Down,
                    b'C' => KeyCode::Right,
                    b'D' => KeyCode::Left,
                    b'H' => KeyCode::Home,
                    b'F' => KeyCode::End,
                    _ => KeyCode::Char('5'),
                };
                // `CSI 1 ; <mod> A` is the modified form; a bare `CSI A` has
                // no modifier.
                let m = mods(nums.get(1).copied().flatten());
                Step::Took(n, Some(key(code, m)))
            }
            b'Z' => Step::Took(n, Some(key(KeyCode::BackTab, KeyModifiers::SHIFT))),
            // **THE KITTY / CSI-u SPELLING.** `CSI <code> ; <mod> [: <event>] u`.
            b'u' => {
                let code = nums.first().copied().flatten().unwrap_or(0);
                let m = mods(nums.get(1).copied().flatten());
                let kind = event_kind(params);
                Step::Took(n, unicode_key(code, m, kind))
            }
            // Focus in/out (`?1004h`, not claimed), cursor position reports,
            // device attributes: consumed, dropped, never typed.
            _ => Step::Took(n, None),
        }
    }
}

// ── the pieces ───────────────────────────────────────────────────────────────

/// `CSI <n> ~` and its two modified forms.
fn tilde(nums: &[Option<u32>]) -> Option<Out> {
    let first = nums.first().copied().flatten()?;
    match first {
        200 => return Some(Out::PasteBegan),
        // A terminator with no paste open. It is residue — of a paste this
        // layer already closed by the idle-gap rule, most often — and it is
        // dropped rather than typed.
        201 => return None,
        _ => {}
    }
    // **`CSI 27 ; <mod> ; <code> ~` — xterm's `modifyOtherKeys`, and the whole
    // of `IS-33`.** This is the arm crossterm does not have. tmux emits this
    // form whenever `extended-keys-format` is `xterm`, which is its DEFAULT,
    // which is why `shift-enter` sent on his desk instead of inserting a
    // newline.
    if first == 27 {
        let m = mods(nums.get(1).copied().flatten());
        let code = nums.get(2).copied().flatten()?;
        return unicode_key(code, m, KeyEventKind::Press);
    }
    let m = mods(nums.get(1).copied().flatten());
    let code = match first {
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        v @ 11..=15 => KeyCode::F(v as u8 - 10),
        v @ 17..=21 => KeyCode::F(v as u8 - 11),
        v @ 23..=26 => KeyCode::F(v as u8 - 12),
        _ => return None,
    };
    Some(key(code, m))
}

/// A key named by its UNICODE CODE POINT, which is how both enhanced-key
/// dialects spell everything that is not a named key.
///
/// The named codes are listed rather than derived because they are the ones
/// that MEAN something to this face: 13 is the one `IS-33` is about.
fn unicode_key(code: u32, m: KeyModifiers, kind: KeyEventKind) -> Option<Out> {
    let c = match code {
        13 => KeyCode::Enter,
        27 => KeyCode::Esc,
        9 => KeyCode::Tab,
        8 | 127 => KeyCode::Backspace,
        // Kitty's private-use block for named keys we do not bind. Dropped
        // rather than turned into a private-use CHARACTER, which would put an
        // unrenderable glyph in his message.
        57344..=63743 => return None,
        v => KeyCode::Char(char::from_u32(v)?),
    };
    Some(Out::Event(Event::Key(KeyEvent::new_with_kind(c, m, kind))))
}

/// `CSI < b ; x ; y M|m` — the SGR mouse report `?1006h` buys.
fn sgr_mouse(params: &[u8], press: bool) -> Option<Out> {
    let nums = numbers(params);
    let b = nums.first().copied().flatten()?;
    let col = nums.get(1).copied().flatten()?;
    let row = nums.get(2).copied().flatten()?;
    let kind = mouse_kind(b, press)?;
    Some(Out::Event(Event::Mouse(MouseEvent {
        kind,
        // The wire is 1-based and every consumer of this crate is 0-based.
        column: (col.max(1) - 1) as u16,
        row: (row.max(1) - 1) as u16,
        modifiers: mouse_mods(b),
    })))
}

/// `CSI M <b+32> <x+32> <y+32>` — the pre-SGR encoding, which cannot express a
/// column past 223. Supported so a terminal that ignored `?1006h` still has a
/// working mouse; `term::CLAIMS` explains why we do not rely on it.
fn legacy_mouse(b: u8, x: u8, y: u8) -> Option<Out> {
    let btn = u32::from(b.wrapping_sub(32));
    // In this encoding a release does not say WHICH button was released.
    let kind = if btn & 3 == 3 && btn & 64 == 0 {
        MouseEventKind::Up(MouseButton::Left)
    } else {
        mouse_kind(btn, true)?
    };
    Some(Out::Event(Event::Mouse(MouseEvent {
        kind,
        column: u16::from(x.wrapping_sub(32)).saturating_sub(1),
        row: u16::from(y.wrapping_sub(32)).saturating_sub(1),
        modifiers: mouse_mods(btn),
    })))
}

fn mouse_kind(b: u32, press: bool) -> Option<MouseEventKind> {
    // Bit 6 (64) is the wheel; bit 5 (32) is motion-while-held.
    if b & 64 != 0 {
        return match b & 3 {
            0 => Some(MouseEventKind::ScrollUp),
            1 => Some(MouseEventKind::ScrollDown),
            2 => Some(MouseEventKind::ScrollLeft),
            3 => Some(MouseEventKind::ScrollRight),
            _ => None,
        };
    }
    let button = match b & 3 {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        // Motion with no button held. `?1003h` is deliberately NOT claimed
        // (`term::CLAIMS`), so this only arrives from a terminal doing its own
        // thing; `Gesture::feed` drops it either way.
        _ => return Some(MouseEventKind::Moved),
    };
    Some(if b & 32 != 0 {
        MouseEventKind::Drag(button)
    } else if press {
        MouseEventKind::Down(button)
    } else {
        MouseEventKind::Up(button)
    })
}

fn mouse_mods(b: u32) -> KeyModifiers {
    let mut m = KeyModifiers::NONE;
    if b & 4 != 0 {
        m |= KeyModifiers::SHIFT;
    }
    if b & 8 != 0 {
        m |= KeyModifiers::ALT;
    }
    if b & 16 != 0 {
        m |= KeyModifiers::CONTROL;
    }
    m
}

/// The xterm modifier parameter: a bitfield, BIASED BY ONE, so that a `1` means
/// "no modifiers" and the parameter is never zero.
fn mods(param: Option<u32>) -> KeyModifiers {
    let Some(v) = param else {
        return KeyModifiers::NONE;
    };
    let bits = v.saturating_sub(1);
    let mut m = KeyModifiers::NONE;
    if bits & 1 != 0 {
        m |= KeyModifiers::SHIFT;
    }
    if bits & 2 != 0 {
        m |= KeyModifiers::ALT;
    }
    if bits & 4 != 0 {
        m |= KeyModifiers::CONTROL;
    }
    if bits & 8 != 0 {
        m |= KeyModifiers::SUPER;
    }
    m
}

/// Kitty's event type, which rides on the modifier parameter as `<mod>:<event>`.
/// 1 press, 2 repeat, 3 release. A release or a repeat that ACTED would double
/// every keystroke, and `enter` SENDS — so getting this right is not cosmetic.
/// [`crate::input::to_key`] is the filter; this only has to report it honestly.
fn event_kind(params: &[u8]) -> KeyEventKind {
    let Some(second) = params.split(|c| *c == b';').nth(1) else {
        return KeyEventKind::Press;
    };
    match second.split(|c| *c == b':').nth(1) {
        Some(b"2") => KeyEventKind::Repeat,
        Some(b"3") => KeyEventKind::Release,
        _ => KeyEventKind::Press,
    }
}

/// Semicolon-separated numbers, with an EMPTY parameter preserved as `None`
/// rather than collapsed to zero — `CSI ; 5 H` means "default row, column 5"
/// and reading that empty slot as a `0` would be a silent off-by-one.
///
/// A sub-parameter (`5:3`) is reported by its first component; the rest is the
/// event type, which [`event_kind`] reads from the raw bytes.
fn numbers(params: &[u8]) -> Vec<Option<u32>> {
    params
        .split(|c| *c == b';')
        .map(|p| {
            let head = p.split(|c| *c == b':').next().unwrap_or(p);
            if head.is_empty() {
                return None;
            }
            std::str::from_utf8(head).ok()?.parse().ok()
        })
        .collect()
}

fn key(code: KeyCode, m: KeyModifiers) -> Out {
    Out::Event(Event::Key(KeyEvent::new(code, m)))
}

fn alt(o: Out) -> Out {
    match o {
        Out::Event(Event::Key(mut k)) => {
            k.modifiers |= KeyModifiers::ALT;
            Out::Event(Event::Key(k))
        }
        other => other,
    }
}

/// How long the `ESC ] … ST` / `ESC P … ST` string sequence at the front of
/// `b` is, or `None` if its terminator has not arrived.
fn string_seq_len(b: &[u8]) -> Option<usize> {
    let mut i = 2;
    while i < b.len() {
        match b[i] {
            0x07 => return Some(i + 1),
            0x1b if i + 1 < b.len() && b[i + 1] == b'\\' => return Some(i + 2),
            // A bare ESC that is not the start of an ST: the sequence was
            // abandoned. End it here rather than swallowing the keystroke that
            // follows.
            0x1b if i + 1 < b.len() => return Some(i),
            _ => i += 1,
        }
    }
    None
}

enum Utf8 {
    Char(char, usize),
    Want,
    Bad,
}

fn utf8(b: &[u8]) -> Utf8 {
    let n = match b[0] {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return Utf8::Bad,
    };
    if b.len() < n {
        return Utf8::Want;
    }
    match std::str::from_utf8(&b[..n]) {
        Ok(s) => match s.chars().next() {
            Some(c) => Utf8::Char(c, n),
            None => Utf8::Bad,
        },
        Err(_) => Utf8::Bad,
    }
}

/// A paste's bytes as text. **Lossy on purpose**: a clipboard can hold anything,
/// and refusing a paste because one byte was not UTF-8 would lose the other
/// 800 KB. `compose::sanitize` is the door that decides what may enter the
/// buffer; this only has to get it there.
fn decode(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

/// The length of the longest suffix of `hay` that is a proper prefix of
/// `needle` — the bytes that must be held back because they might be the front
/// half of a terminator split across two reads.
fn partial_suffix(hay: &[u8], needle: &[u8]) -> usize {
    let max = needle.len().min(hay.len());
    for n in (1..max).rev() {
        if hay[hay.len() - n..] == needle[..n] {
            return n;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A parser and a clock that only moves when a test says so.
    struct Fix {
        p: Parser,
        t: Instant,
    }

    impl Fix {
        fn new() -> Self {
            Fix {
                p: Parser::new(),
                t: Instant::now(),
            }
        }
        fn feed(&mut self, bytes: &[u8]) -> Vec<Out> {
            let mut out = Vec::new();
            self.p.feed(bytes, self.t, &mut out);
            out
        }
        /// Let `d` pass with nothing arriving, then resolve the silence.
        fn wait(&mut self, d: Duration) -> Vec<Out> {
            self.t += d;
            let mut out = Vec::new();
            self.p.idle(self.t, &mut out);
            out
        }
    }

    fn keys(out: &[Out]) -> Vec<KeyEvent> {
        out.iter()
            .filter_map(|o| match o {
                Out::Event(Event::Key(k)) => Some(*k),
                _ => None,
            })
            .collect()
    }

    fn pastes(out: &[Out]) -> Vec<String> {
        out.iter()
            .filter_map(|o| match o {
                Out::Event(Event::Paste(s)) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    // ── IS-33 ────────────────────────────────────────────────────────────────

    /// **THE `IS-33` PROOF, AND IT IS ONE LINE OF BYTES.**
    ///
    /// `CSI 27 ; 2 ; 13 ~` is what tmux sends for `shift-enter` under its
    /// DEFAULT `extended-keys-format xterm`. crossterm drops it, which made
    /// `shift-enter` byte-identical to `enter` — and `enter` SENDS. Here it is
    /// an Enter carrying SHIFT, which `to_key` spells `Key::ShiftEnter` and the
    /// table binds to a NEWLINE.
    #[test]
    fn the_xterm_spelling_of_shift_enter_is_an_enter_with_shift() {
        let mut f = Fix::new();
        let out = f.feed(b"\x1b[27;2;13~");
        assert_eq!(
            keys(&out),
            vec![KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)],
            "IS-33: the xterm modifyOtherKeys form did not become a shifted Enter"
        );
    }

    /// The same key in the OTHER dialect, which crossterm could already read.
    /// Both must land on the same event or the face behaves differently
    /// depending on a tmux server option — which is the state `IS-33` was.
    #[test]
    fn both_enhanced_dialects_agree_on_every_modifier_of_enter() {
        for (bits, want) in [
            (2u32, KeyModifiers::SHIFT),
            (5, KeyModifiers::CONTROL),
            (3, KeyModifiers::ALT),
            (6, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        ] {
            let mut a = Fix::new();
            let xterm = keys(&a.feed(format!("\x1b[27;{bits};13~").as_bytes()));
            let mut b = Fix::new();
            let csi_u = keys(&b.feed(format!("\x1b[13;{bits}u").as_bytes()));
            assert_eq!(
                xterm,
                vec![KeyEvent::new(KeyCode::Enter, want)],
                "the xterm form of modifier {bits} is wrong"
            );
            assert_eq!(xterm, csi_u, "the two dialects disagree at modifier {bits}");
        }
    }

    /// A RELEASE must stay a release all the way through this layer. `to_key`
    /// is what drops it, and it can only drop what it is told about.
    #[test]
    fn a_kitty_release_is_reported_as_a_release_and_not_as_a_press() {
        let mut f = Fix::new();
        let out = f.feed(b"\x1b[13;2:3u");
        assert_eq!(keys(&out).len(), 1);
        assert_eq!(keys(&out)[0].kind, KeyEventKind::Release);
        let mut f = Fix::new();
        assert_eq!(keys(&f.feed(b"\x1b[13;2:2u"))[0].kind, KeyEventKind::Repeat);
    }

    // ── IS-38 ────────────────────────────────────────────────────────────────

    /// **`IS-38`, THE IDLE-GAP HALF — his own rule, and the deafness is gone.**
    ///
    /// A paste that opens, delivers content, and never gets a terminator used
    /// to hold every subsequent keystroke inside crossterm's buffer forever.
    /// Here: the content lands after a second of quiet, and the keyboard is
    /// live immediately after.
    #[test]
    fn an_unterminated_paste_that_goes_quiet_lands_and_the_keyboard_survives() {
        let mut f = Fix::new();
        assert_eq!(f.feed(b"\x1b[200~his clipboard"), vec![Out::PasteBegan]);
        // Still inside the gap: nothing has been decided.
        assert!(f.wait(Duration::from_millis(400)).is_empty());
        let out = f.wait(PASTE_IDLE);
        assert_eq!(pastes(&out), vec!["his clipboard".to_string()]);
        assert!(out.contains(&Out::PasteHealed), "the heal was not reported");
        assert!(
            !f.p.pasting(),
            "the paste is still open after the gap closed it"
        );
        // THE POINT OF THE WHOLE FIX: the next keystroke is seen.
        let out = f.feed(b"x");
        assert_eq!(
            keys(&out),
            vec![KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)],
            "IS-38: the keyboard is still deaf after an unterminated paste"
        );
    }

    /// **`IS-38`, THE TIMEOUT HALF.** A paste that opens and says NOTHING is
    /// the case the idle gap cannot judge — an empty run of silence after
    /// `CSI 200~` is not a completed empty paste. It fails, it says so, and the
    /// keyboard comes back.
    #[test]
    fn a_paste_that_never_says_anything_fails_by_name_and_frees_the_keyboard() {
        let mut f = Fix::new();
        assert_eq!(f.feed(b"\x1b[200~"), vec![Out::PasteBegan]);
        assert!(
            f.wait(PASTE_IDLE + Duration::from_millis(1)).is_empty(),
            "an EMPTY paste was folded in by the idle gap — that is a paste \
             invented out of silence"
        );
        let out = f.wait(PASTE_SILENT);
        assert_eq!(out, vec![Out::PasteFailed]);
        assert!(!f.p.pasting());
        assert_eq!(
            keys(&f.feed(b"y")),
            vec![KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)]
        );
    }

    /// **THE FALSE-TIMEOUT CASE, which is the one a naive elapsed-time rule
    /// gets wrong.** A big paste dribbling in over far longer than either
    /// window must not be cut in half: both windows measure SILENCE, so a
    /// paste that keeps talking is never abandoned.
    #[test]
    fn a_slow_paste_that_keeps_arriving_is_never_timed_out_mid_flight() {
        let mut f = Fix::new();
        f.feed(b"\x1b[200~");
        for i in 0..20 {
            f.t += PASTE_IDLE - Duration::from_millis(100);
            let mut out = Vec::new();
            f.p.idle(f.t, &mut out);
            assert!(out.is_empty(), "the paste was cut off at chunk {i}");
            let out = f.feed(b"chunk ");
            assert!(out.is_empty(), "a chunk was emitted before the paste ended");
        }
        let out = f.feed(b"end\x1b[201~");
        assert_eq!(pastes(&out), vec!["chunk ".repeat(20) + "end"]);
    }

    /// A terminator split across two reads is still a terminator, and its
    /// first half must never reach his composer as text.
    #[test]
    fn a_terminator_split_across_reads_is_not_pasted_as_literal_text() {
        let mut f = Fix::new();
        f.feed(b"\x1b[200~body\x1b[20");
        assert!(f.feed(b"1~").iter().any(|o| matches!(
            o,
            Out::Event(Event::Paste(s)) if s == "body"
        )));
        // …and the held-back bytes did not become a paste of their own.
        let mut f = Fix::new();
        f.feed(b"\x1b[200~body\x1b[20");
        let out = f.wait(PASTE_IDLE + Duration::from_millis(1));
        assert_eq!(
            pastes(&out),
            vec!["body".to_string()],
            "the partial terminator leaked into the pasted text"
        );
    }

    /// The `0042` defect, at THIS layer: a paste carrying its own terminator
    /// splits, exactly as it did before, and the tail arrives as ordinary input
    /// IN THE SAME BATCH — which is the precondition `input::defuse` needs to
    /// fold it back. A layer that "fixed" the split here would break that rule
    /// without making anything more correct.
    #[test]
    fn a_paste_carrying_its_own_terminator_still_splits_in_the_same_batch() {
        let mut f = Fix::new();
        let out = f.feed(b"\x1b[200~hello\x1b[201~\x1b[201~ rm -rf\r");
        assert_eq!(pastes(&out), vec!["hello".to_string()]);
        let tail: String = keys(&out)
            .iter()
            .filter_map(|k| match k.code {
                KeyCode::Char(c) => Some(c),
                KeyCode::Enter => Some('\n'),
                _ => None,
            })
            .collect();
        assert_eq!(
            tail, " rm -rf\n",
            "the tail is not in the same batch to fold"
        );
    }

    // ── the rest of the grammar, which must not have regressed ──────────────

    /// **THE BYTES `0026` MEASURED ON A PTY, AS A TABLE.** Every row is a
    /// recorded observation of what this stack actually sends; this layer
    /// replaces the parser that produced the right-hand column, so the table is
    /// the regression test for the replacement.
    #[test]
    fn the_measured_wire_table_still_decodes_the_same_way() {
        let cases: &[(&[u8], KeyCode, KeyModifiers)] = &[
            (b"\r", KeyCode::Enter, KeyModifiers::NONE),
            (b"\x1b\r", KeyCode::Enter, KeyModifiers::ALT),
            (b"\n", KeyCode::Char('j'), KeyModifiers::CONTROL),
            (b"\x1ba", KeyCode::Char('a'), KeyModifiers::ALT),
            (b"\t", KeyCode::Tab, KeyModifiers::NONE),
            (b"\x7f", KeyCode::Backspace, KeyModifiers::NONE),
            (b"\x08", KeyCode::Char('h'), KeyModifiers::CONTROL),
            (b"\x03", KeyCode::Char('c'), KeyModifiers::CONTROL),
            (b"\x00", KeyCode::Char(' '), KeyModifiers::CONTROL),
            (b"\x1b[A", KeyCode::Up, KeyModifiers::NONE),
            (b"\x1b[B", KeyCode::Down, KeyModifiers::NONE),
            (b"\x1b[C", KeyCode::Right, KeyModifiers::NONE),
            (b"\x1b[D", KeyCode::Left, KeyModifiers::NONE),
            (b"\x1b[1;3C", KeyCode::Right, KeyModifiers::ALT),
            (b"\x1b[1;3D", KeyCode::Left, KeyModifiers::ALT),
            (b"\x1b[H", KeyCode::Home, KeyModifiers::NONE),
            (b"\x1b[F", KeyCode::End, KeyModifiers::NONE),
            (b"\x1b[1;3H", KeyCode::Home, KeyModifiers::ALT),
            (b"\x1b[3~", KeyCode::Delete, KeyModifiers::NONE),
            (b"\x1b[2~", KeyCode::Insert, KeyModifiers::NONE),
            (b"\x1b[5~", KeyCode::PageUp, KeyModifiers::NONE),
            (b"\x1b[6~", KeyCode::PageDown, KeyModifiers::NONE),
            (b"\x1bOA", KeyCode::Up, KeyModifiers::NONE),
            (b"\x1bOH", KeyCode::Home, KeyModifiers::NONE),
            (b"\x1b[Z", KeyCode::BackTab, KeyModifiers::SHIFT),
            (b"\x1b[15~", KeyCode::F(5), KeyModifiers::NONE),
            (b"a", KeyCode::Char('a'), KeyModifiers::NONE),
            (b"G", KeyCode::Char('G'), KeyModifiers::NONE),
        ];
        for (bytes, code, m) in cases {
            let mut f = Fix::new();
            let got = keys(&f.feed(bytes));
            assert_eq!(
                got,
                vec![KeyEvent::new(*code, *m)],
                "{:?} decoded wrong",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    /// **THE BYTES `0052` MEASURED FOR THE MOTION KEYS, AS A TABLE.**
    ///
    /// Every row was observed on a private tmux 3.5a server configured exactly
    /// as his live one (`extended-keys on`, `extended-keys-format xterm`,
    /// `escape-time 10`, `default-terminal tmux-256color`) by
    /// `motion_keys_probe.sh`, which prints the raw bytes beside what this
    /// parser makes of them. It is the evidence behind `0052` item 3's "if it
    /// can be made to work in his terminal": it can, and these are the bytes.
    ///
    /// **`opt-delete` HAS TWO SPELLINGS AND BOTH ARE HERE.** tmux emits
    /// `ESC \x7f` with `modifyOtherKeys` off and `CSI 27;3;127~` with it on, so
    /// the binding must not depend on which one a session negotiated — and this
    /// asserts they decode identically, which is what makes that true.
    #[test]
    fn the_measured_motion_wire_table_decodes_to_distinguishable_keys() {
        let cases: &[(&[u8], KeyCode, KeyModifiers)] = &[
            // the word motions `0042` built — VERIFIED, not trusted
            (b"\x1b[1;3D", KeyCode::Left, KeyModifiers::ALT),
            (b"\x1b[1;3C", KeyCode::Right, KeyModifiers::ALT),
            // **THE VERTICAL PAIR `0100` MEASURED**, for the paragraph motions
            // in the box, the jump-by-authorship in the chat and the band
            // headers at the root. Same probe, same server settings, same
            // shape of answer as the four limit keys: the bytes arrive and
            // `to_key` had no arm for them.
            (b"\x1b[1;3A", KeyCode::Up, KeyModifiers::ALT),
            (b"\x1b[1;3B", KeyCode::Down, KeyModifiers::ALT),
            // the four limit keys
            (b"\x1b[1;5D", KeyCode::Left, KeyModifiers::CONTROL),
            (b"\x1b[1;5C", KeyCode::Right, KeyModifiers::CONTROL),
            (b"\x1b[1;5A", KeyCode::Up, KeyModifiers::CONTROL),
            (b"\x1b[1;5B", KeyCode::Down, KeyModifiers::CONTROL),
            // opt-delete, both dialects
            (b"\x1b\x7f", KeyCode::Backspace, KeyModifiers::ALT),
            (b"\x1b[27;3;127~", KeyCode::Backspace, KeyModifiers::ALT),
            // and the unmodified keys they must stay distinguishable FROM
            (b"\x1b[D", KeyCode::Left, KeyModifiers::NONE),
            (b"\x1b[C", KeyCode::Right, KeyModifiers::NONE),
            (b"\x1b[A", KeyCode::Up, KeyModifiers::NONE),
            (b"\x1b[B", KeyCode::Down, KeyModifiers::NONE),
            (b"\x7f", KeyCode::Backspace, KeyModifiers::NONE),
        ];
        for (bytes, code, m) in cases {
            let mut f = Fix::new();
            assert_eq!(
                keys(&f.feed(bytes)),
                vec![KeyEvent::new(*code, *m)],
                "{:?} decoded wrong",
                String::from_utf8_lossy(bytes)
            );
        }
        // **THE MODIFIED FORM IS NOT THE PLAIN ONE.** The defect this round
        // fixed was one level ABOVE the parser — `to_key` flattened these — but
        // the property the fix depends on is this one, and it belongs where the
        // bytes are read.
        let mut f = Fix::new();
        let ctrl_left = keys(&f.feed(b"\x1b[1;5D"));
        let mut f = Fix::new();
        let plain_left = keys(&f.feed(b"\x1b[D"));
        assert_ne!(
            ctrl_left, plain_left,
            "ctrl-left and left arrive indistinguishably — the binding cannot work"
        );
    }

    /// Multi-byte text, including the case `0026` measured: a codepoint split
    /// across two reads must be reassembled, not dropped and not doubled.
    #[test]
    fn utf8_survives_being_cut_in_half_by_a_read_boundary() {
        let mut f = Fix::new();
        assert!(
            f.feed(&"日".as_bytes()[..2]).is_empty(),
            "half a char acted"
        );
        let out = f.feed(&"日".as_bytes()[2..]);
        assert_eq!(
            keys(&out),
            vec![KeyEvent::new(KeyCode::Char('日'), KeyModifiers::NONE)]
        );

        // The ZWJ family emoji, which arrives as three separate codepoints.
        let mut f = Fix::new();
        let out = f.feed("👩‍💻".as_bytes());
        assert_eq!(keys(&out).len(), 3, "the ZWJ sequence lost a codepoint");
    }

    /// **A LONE `ESC` IS THE ESCAPE KEY, AND ONLY AFTER THE WINDOW.** Resolving
    /// it immediately would turn every arrow key into `Esc` `[` `C` — three
    /// characters typed into his message.
    #[test]
    fn a_lone_escape_waits_its_window_and_a_real_sequence_never_waits() {
        let mut f = Fix::new();
        assert!(f.feed(b"\x1b").is_empty(), "ESC acted before its window");
        assert!(f.wait(Duration::from_millis(20)).is_empty());
        assert_eq!(
            keys(&f.wait(ESC_WINDOW)),
            vec![KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)]
        );

        // The sequence case: the `[` arrives late but WITHIN the window, and
        // the key is an arrow rather than an Esc and a bracket.
        let mut f = Fix::new();
        f.feed(b"\x1b");
        f.t += Duration::from_millis(20);
        let out = f.feed(b"[C");
        assert_eq!(
            keys(&out),
            vec![KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)]
        );
    }

    /// **THE GENERAL FORM OF `IS-38`: NO SEQUENCE MAY HOLD THE KEYBOARD.**
    /// crossterm waits forever on a truncated `CSI`, exactly as it waits
    /// forever on an unterminated paste — the deafness was never specific to
    /// paste. After [`SEQ_WINDOW`] the fragment is flushed and input resumes.
    #[test]
    fn a_truncated_sequence_is_flushed_rather_than_holding_the_keyboard_shut() {
        let mut f = Fix::new();
        assert!(f.feed(b"\x1b[27;2").is_empty());
        assert!(
            f.wait(Duration::from_millis(200)).is_empty(),
            "flushed too early"
        );
        let out = f.wait(SEQ_WINDOW);
        assert!(
            keys(&out).iter().any(|k| k.code == KeyCode::Esc),
            "the truncated sequence was not resolved: {out:?}"
        );
        assert_eq!(
            keys(&f.feed(b"z")),
            vec![KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)],
            "the keyboard is still deaf behind a truncated sequence"
        );
    }

    /// A reply the terminal sent US is never a keystroke. Before this layer
    /// crossterm consumed these; if they fell through now, a device-attributes
    /// answer would type `?62;c` into his composer.
    #[test]
    fn terminal_replies_are_consumed_and_never_typed() {
        for reply in [
            &b"\x1b[?62;1;6c"[..],
            b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07",
            b"\x1bP>|tmux 3.5a\x1b\\",
            b"\x1b[8;24;80t",
            b"\x1b[I",
            b"\x1b[O",
            b"\x1b[12;40R",
        ] {
            let mut f = Fix::new();
            let out = f.feed(reply);
            assert!(
                out.is_empty(),
                "{:?} produced {out:?}",
                String::from_utf8_lossy(reply)
            );
        }
        // …and a keystroke immediately after one still arrives.
        let mut f = Fix::new();
        let out = f.feed(b"\x1b[?62;1;6cq");
        assert_eq!(
            keys(&out),
            vec![KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)]
        );
    }

    /// **THE ONE REPLY THIS FACE ASKED A QUESTION TO GET.** `CSI ? <flags> u`
    /// is the terminal saying it speaks the kitty keyboard protocol, which
    /// outside tmux is the only evidence that `shift-enter` can be reported
    /// apart from `enter`.
    ///
    /// It has to be an EVENT rather than a probe's return value: a probe reads
    /// stdin, this module reads stdin, and two readers on one descriptor lose
    /// each other's bytes. That was not hypothetical — the blocking probe this
    /// replaced ate the first keystroke of every session, measured on a pty.
    #[test]
    fn the_kitty_reply_is_reported_and_is_never_typed_as_text() {
        for reply in [&b"\x1b[?1u"[..], b"\x1b[?0u", b"\x1b[?1;2u"] {
            let mut f = Fix::new();
            let out = f.feed(reply);
            assert_eq!(
                out,
                vec![Out::EnhancedKeys],
                "{:?} did not report the capability",
                String::from_utf8_lossy(reply)
            );
        }
        // A DEVICE-ATTRIBUTES answer is not a keyboard answer. Reading it as
        // one would turn on a capability on every terminal ever made.
        let mut f = Fix::new();
        assert!(f.feed(b"\x1b[?62;1;6c").is_empty());
        // …and the reply never becomes text, whatever follows it.
        let mut f = Fix::new();
        let out = f.feed(b"\x1b[?1uhi");
        assert_eq!(out.len(), 3, "the reply leaked into the stream: {out:?}");
        assert_eq!(keys(&out).len(), 2);
    }

    /// The mouse, in the encoding `?1006h` buys — press, motion-while-held,
    /// release and a wheel notch, since `Gesture` needs all four to tell a
    /// click from a drag.
    #[test]
    fn the_sgr_mouse_report_decodes_to_the_four_kinds_a_gesture_needs() {
        let cases: &[(&[u8], MouseEventKind, u16, u16)] = &[
            (
                b"\x1b[<0;11;6M",
                MouseEventKind::Down(MouseButton::Left),
                10,
                5,
            ),
            (
                b"\x1b[<32;21;6M",
                MouseEventKind::Drag(MouseButton::Left),
                20,
                5,
            ),
            (
                b"\x1b[<0;21;6m",
                MouseEventKind::Up(MouseButton::Left),
                20,
                5,
            ),
            (b"\x1b[<64;5;5M", MouseEventKind::ScrollUp, 4, 4),
            (b"\x1b[<65;5;5M", MouseEventKind::ScrollDown, 4, 4),
            (
                b"\x1b[<2;5;5M",
                MouseEventKind::Down(MouseButton::Right),
                4,
                4,
            ),
        ];
        for (bytes, kind, col, row) in cases {
            let mut f = Fix::new();
            let out = f.feed(bytes);
            let m = out
                .iter()
                .find_map(|o| match o {
                    Out::Event(Event::Mouse(m)) => Some(*m),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    panic!("no mouse event from {:?}", String::from_utf8_lossy(bytes))
                });
            assert_eq!((m.kind, m.column, m.row), (*kind, *col, *row));
        }
        // A column past 223 is the reason `?1006h` is claimed at all, and the
        // SGR encoding must carry it.
        let mut f = Fix::new();
        let out = f.feed(b"\x1b[<0;400;9M");
        assert!(matches!(
            out.first(),
            Some(Out::Event(Event::Mouse(MouseEvent { column: 399, .. })))
        ));
    }

    /// The legacy encoding, for a terminal that ignored `?1006h`. It cannot be
    /// scanned for a final byte — its three body bytes may be ANY byte — so it
    /// is the one CSI that is length-delimited, and getting that wrong would
    /// desynchronise the whole stream.
    #[test]
    fn the_legacy_mouse_report_is_length_delimited_and_does_not_desync() {
        let mut f = Fix::new();
        // `CSI M` + button 0 + column 11 + row 6, then an ordinary keystroke.
        let out = f.feed(b"\x1b[M\x20\x2b\x26k");
        assert!(matches!(
            out.first(),
            Some(Out::Event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 10,
                row: 5,
                ..
            })))
        ));
        assert_eq!(
            keys(&out),
            vec![KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)],
            "the stream desynchronised after a legacy mouse report"
        );
    }

    /// **N ≥ 2, per `core:writing-tools`.** The paste state ARBITRATES between
    /// two readings of the same byte — "is this `\r` his ENTER, or is it inside
    /// his clipboard?" — so it is proven at two or more interleavings or it is
    /// not proven at all.
    #[test]
    fn the_paste_state_is_proven_at_both_interleavings() {
        // (1) A `\r` INSIDE the paste is content and reaches no key.
        let mut f = Fix::new();
        let out = f.feed(b"\x1b[200~a\rb\x1b[201~");
        assert_eq!(pastes(&out), vec!["a\rb".to_string()]);
        assert!(keys(&out).is_empty(), "a CR inside a paste became an ENTER");

        // (2) A `\r` AFTER the paste closed is his, and it must act.
        let out = f.feed(b"\r");
        assert_eq!(
            keys(&out),
            vec![KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)]
        );

        // (3) …and both in one read, which is the case either single-direction
        //     test would pass while still being wrong.
        let mut f = Fix::new();
        let out = f.feed(b"\r\x1b[200~x\ry\x1b[201~\r");
        assert_eq!(keys(&out).len(), 2, "wrong number of real ENTERs: {out:?}");
        assert_eq!(pastes(&out), vec!["x\ry".to_string()]);
    }

    /// Two pastes in one read stay two pastes. A parser that let the state
    /// leak would glue his last clipboard onto his next one.
    #[test]
    fn two_pastes_in_one_read_stay_two() {
        let mut f = Fix::new();
        let out = f.feed(b"\x1b[200~one\x1b[201~\x1b[200~two\x1b[201~");
        assert_eq!(pastes(&out), vec!["one".to_string(), "two".to_string()]);
        assert_eq!(
            out.iter().filter(|o| **o == Out::PasteBegan).count(),
            2,
            "the second paste was never announced"
        );
    }

    /// An 800 KB paste is ONE event, whatever read boundaries it crossed. This
    /// is the case that makes the held-back-suffix rule load-bearing rather
    /// than theoretical.
    #[test]
    fn a_huge_paste_split_into_many_reads_is_still_one_event() {
        let body = "x".repeat(79) + "\n";
        let whole = body.repeat(10_000);
        let mut f = Fix::new();
        f.feed(b"\x1b[200~");
        let bytes = whole.as_bytes();
        let mut out = Vec::new();
        for chunk in bytes.chunks(4096) {
            f.p.feed(chunk, f.t, &mut out);
        }
        assert!(out.is_empty(), "the paste emitted before it ended");
        let out = f.feed(b"\x1b[201~");
        assert_eq!(pastes(&out).len(), 1);
        assert_eq!(pastes(&out)[0].len(), whole.len());
    }

    /// The deadline is what the pump sleeps on. With nothing pending there is
    /// no deadline at all, which is what keeps a still face from waking up.
    #[test]
    fn a_parser_with_nothing_pending_asks_for_no_timer() {
        let mut f = Fix::new();
        assert!(f.p.deadline(f.t).is_none());
        f.feed(b"\x1b");
        assert!(f.p.deadline(f.t).is_some_and(|d| d <= ESC_WINDOW));
        let mut f = Fix::new();
        f.feed(b"\x1b[200~a");
        assert!(f.p.deadline(f.t).is_some_and(|d| d <= PASTE_IDLE));
        let mut f = Fix::new();
        f.feed(b"abc");
        assert!(
            f.p.deadline(f.t).is_none(),
            "consumed bytes left a timer armed"
        );
    }

    #[test]
    fn the_suffix_helper_finds_only_proper_prefixes() {
        assert_eq!(partial_suffix(b"body\x1b[20", b"\x1b[201~"), 4);
        assert_eq!(partial_suffix(b"body\x1b", b"\x1b[201~"), 1);
        assert_eq!(partial_suffix(b"body", b"\x1b[201~"), 0);
        assert_eq!(partial_suffix(b"", b"\x1b[201~"), 0);
    }
}
